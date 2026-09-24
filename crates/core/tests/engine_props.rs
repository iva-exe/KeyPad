//! Property testy enginu: libovolná sekvence událostí klávesnice,
//! přepínání, přiřazování, vynucení, výpadků padu a skoků času.
//!
//! Hlavní vlastnost z ROADMAP.md (Fáze 1): po uvolnění všech kláves je
//! PadState neutrální a `held` prázdné.
//!
//! Engine se po každém kroku porovnává s **nezávislým referenčním
//! modelem** (`Model`) — druhou, co nejpřímočařejší implementací
//! specifikace s vlastním mapováním a vlastním výpočtem padu. Dřívější
//! verze testu brala očekávání z enginu samotného (jeho mapování, jeho
//! výpočet padu) a revize ukázala čtyři mutanty enginu, které jí prošly.
//! Model je naopak zabije: nesedí-li režim, mapování, vlastník, pořadí
//! stisku, spočítaný pad, odeslaný pad nebo oznámení, test spadne.
//!
//! Navíc se hlídají invarianty „fyzického světa", ze kterých plyne, že
//! se nic nezasekne:
//! - všechny události jednoho stisku (key-down, autorepeaty, key-up)
//!   mají stejné rozhodnutí o potlačení → OS dostane key-up právě ke
//!   key-downům, které viděl (princip 2);
//! - autorepeat nikdy nic nespouští;
//! - mimo režim Gamepad je pad neutrální, osa nikdy `i16::MIN`.

use std::collections::HashMap;

use keypad_core::{
    Action, BindingCancel, Decision, DisabledReason, Engine, ForceReason, KeyId, Mapping, Mode,
    Owner, PadButton, PadState, StickDir, UiEvent, BINDING_TIMEOUT_MS, STALE_KEY_MS,
};
use proptest::prelude::*;

/// Klávesy, se kterými se hraje: namapované (i dvě na jeden směr),
/// zkratky obou mapování, Esc, nenamapované, dvojice se stejným scan
/// kódem (šipka × numpad), falešný Ctrl z AltGr a klávesa bez scan kódu.
const KEYS: [KeyId; 17] = [
    KeyId::W,
    KeyId::A,
    KeyId::S,
    KeyId::D,
    KeyId::ARROW_UP,
    KeyId::ARROW_LEFT,
    KeyId::SPACE,
    KeyId::DIGIT_1,
    KeyId::LEFT_SHIFT,
    KeyId::SCROLL_LOCK,
    KeyId::ESC,
    KeyId::X,
    KeyId::NUMPAD_8,
    KeyId::I,
    KeyId::ALTGR_FAKE_CTRL,
    KeyId::LEFT_CTRL,
    KeyId::new(0),
];

#[derive(Debug, Clone)]
enum Op {
    /// Key-down: nový stisk, nebo autorepeat, pokud už je klávesa dole.
    Press(usize),
    /// Key-up: skutečné uvolnění, nebo key-up bez key-down.
    Release(usize),
    Toggle,
    StartBinding(usize),
    CancelBinding,
    /// Posun času + tik časovače.
    Advance(u64),
    /// Posun času BEZ tiku — propadlé přiřazování pak musí zachytit
    /// samotná klávesa nebo příkaz.
    AdvanceNoTick(u64),
    /// Skok času kamkoli (i dozadu, i na krajní hodnoty).
    Jump(u64),
    ForceKeyboard,
    ResetHeld,
    Disable(u8),
    Enable,
    /// 0 = výchozí, 1 = jiná zkratka (X) + dvě klávesy na jednom směru,
    /// 2 = Esc namapovaný, zkratka na levém Shiftu.
    SetMapping(u8),
}

fn op() -> impl Strategy<Value = Op> {
    let n = KEYS.len();
    prop_oneof![
        10 => (0..n).prop_map(Op::Press),
        8 => (0..n).prop_map(Op::Release),
        2 => Just(Op::Toggle),
        2 => (0..24usize).prop_map(Op::StartBinding),
        1 => Just(Op::CancelBinding),
        2 => prop_oneof![0..60u64, 1_400..1_600u64, 9_000..12_000u64].prop_map(Op::Advance),
        2 => prop_oneof![0..60u64, 1_400..1_600u64, 9_000..12_000u64].prop_map(Op::AdvanceNoTick),
        1 => prop_oneof![Just(0u64), Just(u64::MAX), Just(u64::MAX - 5_000), any::<u64>()]
            .prop_map(Op::Jump),
        1 => Just(Op::ForceKeyboard),
        1 => Just(Op::ResetHeld),
        1 => (0..3u8).prop_map(Op::Disable),
        2 => Just(Op::Enable),
        1 => (0..3u8).prop_map(Op::SetMapping),
    ]
}

fn action_nr(i: usize) -> Action {
    Action::all().nth(i).expect("24 akcí")
}

// ── Nezávislý referenční model ─────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
struct MMapping {
    toggle: KeyId,
    keys: HashMap<KeyId, Action>,
}

fn mapping_variant(v: u8) -> (Mapping, MMapping) {
    let d = Mapping::default();
    let mut keys: HashMap<KeyId, Action> = d.bindings().collect();
    let mut m = d.clone();
    let toggle = match v {
        1 => {
            m.bind(KeyId::NUMPAD_8, Action::LeftStick(StickDir::Up))
                .unwrap();
            keys.insert(KeyId::NUMPAD_8, Action::LeftStick(StickDir::Up));
            m.set_toggle_key(KeyId::X).unwrap();
            KeyId::X
        }
        2 => {
            m.bind(KeyId::ESC, Action::Button(PadButton::Back)).unwrap();
            keys.insert(KeyId::ESC, Action::Button(PadButton::Back));
            m.unbind(KeyId::LEFT_SHIFT).unwrap();
            keys.remove(&KeyId::LEFT_SHIFT);
            m.set_toggle_key(KeyId::LEFT_SHIFT).unwrap();
            KeyId::LEFT_SHIFT
        }
        _ => KeyId::SCROLL_LOCK,
    };
    (m, MMapping { toggle, keys })
}

fn mappable(k: KeyId) -> bool {
    (1..=0x7F).contains(&k.scan)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MHeld {
    owner: Owner,
    seq: u64,
    last: u64,
}

struct Model {
    mode: Mode,
    mapping: MMapping,
    held: HashMap<KeyId, MHeld>,
    seq: u64,
}

impl Model {
    fn new() -> Model {
        Model {
            mode: Mode::Disabled {
                reason: DisabledReason::PadNotConnected,
            },
            mapping: mapping_variant(0).1,
            held: HashMap::new(),
            seq: 0,
        }
    }

    fn set_mode(&mut self, new: Mode) {
        if self.mode == Mode::Gamepad && new != Mode::Gamepad {
            for h in self.held.values_mut() {
                if matches!(h.owner, Owner::Pad(_)) {
                    h.owner = Owner::Swallow;
                }
            }
        }
        self.mode = new;
    }

    fn expire(&mut self, now: u64) {
        if let Mode::Binding { started_at_ms, .. } = self.mode {
            if now.saturating_sub(started_at_ms) >= BINDING_TIMEOUT_MS {
                self.set_mode(Mode::Keyboard);
            }
        }
    }

    fn toggle(&mut self) {
        match self.mode {
            Mode::Keyboard => self.set_mode(Mode::Gamepad),
            Mode::Gamepad => self.set_mode(Mode::Keyboard),
            _ => {}
        }
    }

    /// Vrací očekávané potlačení.
    fn key_down(&mut self, key: KeyId, now: u64) -> bool {
        self.expire(now);
        if !mappable(key) {
            return false;
        }
        if let Some(h) = self.held.get_mut(&key) {
            let stale = h.owner != Owner::Os && now.saturating_sub(h.last) >= STALE_KEY_MS;
            if !stale {
                h.last = now;
                return h.owner != Owner::Os;
            }
            self.held.remove(&key);
        }
        self.seq += 1;
        let owner = if key == self.mapping.toggle {
            Owner::Swallow
        } else {
            match self.mode {
                Mode::Binding { .. } => Owner::Swallow,
                Mode::Gamepad => match self.mapping.keys.get(&key) {
                    Some(&a) => Owner::Pad(a),
                    None => Owner::Os,
                },
                _ => Owner::Os,
            }
        };
        self.held.insert(
            key,
            MHeld {
                owner,
                seq: self.seq,
                last: now,
            },
        );
        if key == self.mapping.toggle {
            self.toggle();
        } else if let Mode::Binding { action, .. } = self.mode {
            if key != KeyId::ESC {
                // Přesun: klávesa má vždy nejvýš jednu akci.
                self.mapping.keys.insert(key, action);
            }
            self.set_mode(Mode::Keyboard);
        }
        owner != Owner::Os
    }

    fn key_up(&mut self, key: KeyId, now: u64) -> bool {
        self.expire(now);
        match self.held.remove(&key) {
            Some(h) => h.owner != Owner::Os,
            None => false,
        }
    }

    /// Stav padu spočítaný nezávisle na enginu: tlačítka OR, triggery,
    /// SOCD podle pořadí stisku, diagonála 23170.
    fn pad(&self) -> PadState {
        let mut p = PadState::NEUTRAL;
        // [páčka][směr] → nejnovější seq
        let mut newest: HashMap<(bool, StickDir), u64> = HashMap::new();
        for h in self.held.values() {
            let Owner::Pad(a) = h.owner else { continue };
            match a {
                Action::Button(b) => p.buttons |= b.mask(),
                Action::LeftTrigger => p.left_trigger = 255,
                Action::RightTrigger => p.right_trigger = 255,
                Action::LeftStick(d) | Action::RightStick(d) => {
                    let right = matches!(a, Action::RightStick(_));
                    let e = newest.entry((right, d)).or_insert(0);
                    *e = (*e).max(h.seq);
                }
            }
        }
        let axis = |stick: bool, neg: StickDir, pos: StickDir| -> i32 {
            match (newest.get(&(stick, neg)), newest.get(&(stick, pos))) {
                (Some(n), Some(p)) if p > n => 1,
                (Some(n), Some(p)) if n > p => -1,
                (Some(_), None) => -1,
                (None, Some(_)) => 1,
                _ => 0,
            }
        };
        for stick in [false, true] {
            let x = axis(stick, StickDir::Left, StickDir::Right);
            let y = axis(stick, StickDir::Down, StickDir::Up);
            let mag = if x != 0 && y != 0 { 23_170 } else { 32_767 };
            let (vx, vy) = ((x * mag) as i16, (y * mag) as i16);
            if stick {
                (p.thumb_rx, p.thumb_ry) = (vx, vy);
            } else {
                (p.thumb_lx, p.thumb_ly) = (vx, vy);
            }
        }
        p
    }
}

// ── Svět: engine + model + fyzické klávesy ─────────────────────────

/// Jeden fyzický stisk z pohledu testu.
struct Press {
    /// Rozhodnutí o potlačení při key-down.
    suppressed: bool,
    /// Engine záznam o stisku zapomněl (reset_held) — key-up pak jde do
    /// OS, i když key-down nešel. Takový stisk se na principu 2
    /// nekontroluje (viz ROADMAP, Otevřené otázky).
    forgotten: bool,
    /// Čas poslední události (kvůli pravidlu ztraceného key-upu).
    last: u64,
}

struct World {
    e: Engine,
    m: Model,
    now: u64,
    physical: HashMap<KeyId, Press>,
    /// Co naposledy odešlo do ViGEm.
    sent: PadState,
}

impl World {
    fn new() -> World {
        World {
            e: Engine::new(Mapping::default()),
            m: Model::new(),
            now: 1_000,
            physical: HashMap::new(),
            sent: PadState::NEUTRAL,
        }
    }

    fn step(&mut self, op: &Op) -> Result<(), TestCaseError> {
        let mode_before = self.e.mode();
        let mut is_key = false;
        let mut forced_pad = false;
        let d = match *op {
            Op::Press(i) => {
                is_key = true;
                self.press(KEYS[i])?
            }
            Op::Release(i) => {
                is_key = true;
                self.release(KEYS[i])?
            }
            Op::Toggle => {
                self.m.expire(self.now);
                self.m.toggle();
                self.e.toggle(self.now)
            }
            Op::StartBinding(i) => {
                let action = action_nr(i);
                self.m.expire(self.now);
                let ok = self.m.mode == Mode::Keyboard;
                if ok {
                    self.m.set_mode(Mode::Binding {
                        action,
                        started_at_ms: self.now,
                    });
                }
                match self.e.start_binding(action, self.now) {
                    Ok(d) => {
                        prop_assert!(ok, "binding mimo Klávesnici");
                        d
                    }
                    Err(_) => {
                        prop_assert!(!ok, "binding z Klávesnice musí jít");
                        // I odmítnutý příkaz mohl zrušit propadlé
                        // přiřazování — to ale model neumí odmítnout
                        // (v Klávesnici by přiřazování začalo).
                        Decision::NONE
                    }
                }
            }
            Op::CancelBinding => {
                self.m.expire(self.now);
                if matches!(self.m.mode, Mode::Binding { .. }) {
                    self.m.set_mode(Mode::Keyboard);
                }
                self.e.cancel_binding(self.now)
            }
            Op::Advance(ms) => {
                self.now = self.now.saturating_add(ms);
                self.m.expire(self.now);
                self.e.tick(self.now)
            }
            Op::AdvanceNoTick(ms) => {
                self.now = self.now.saturating_add(ms);
                return Ok(());
            }
            Op::Jump(t) => {
                self.now = t;
                return Ok(());
            }
            Op::ForceKeyboard => {
                forced_pad = true;
                if matches!(self.m.mode, Mode::Gamepad | Mode::Binding { .. }) {
                    self.m.set_mode(Mode::Keyboard);
                }
                self.e.force_keyboard(ForceReason::Watchdog)
            }
            Op::ResetHeld => {
                forced_pad = true;
                for p in self.physical.values_mut() {
                    p.forgotten = true;
                }
                if matches!(self.m.mode, Mode::Gamepad | Mode::Binding { .. }) {
                    self.m.set_mode(Mode::Keyboard);
                }
                self.m.held.clear();
                self.e.reset_held(ForceReason::SessionLock)
            }
            Op::Disable(r) => {
                let reason = match r {
                    0 => DisabledReason::ViGEmMissing,
                    1 => DisabledReason::PadNotConnected,
                    _ => DisabledReason::PadError,
                };
                self.m.set_mode(Mode::Disabled { reason });
                self.e.disable(reason)
            }
            Op::Enable => {
                if matches!(self.m.mode, Mode::Disabled { .. }) {
                    self.m.set_mode(Mode::Keyboard);
                }
                self.e.enable()
            }
            Op::SetMapping(v) => {
                let (em, mm) = mapping_variant(v);
                if matches!(self.m.mode, Mode::Gamepad | Mode::Binding { .. }) {
                    self.m.set_mode(Mode::Keyboard);
                }
                self.m.mapping = mm;
                self.e.set_mapping(em)
            }
        };
        self.check_after(&d, mode_before, is_key, forced_pad)
    }

    fn press(&mut self, key: KeyId) -> Result<Decision, TestCaseError> {
        let now = self.now;
        // Klávesa, o které engine zapomněl (reset_held), je pro něj po
        // návratu nový stisk — LL hook autorepeat nerozliší.
        if self.physical.get(&key).is_some_and(|p| p.forgotten) {
            self.physical.remove(&key);
        }
        let expected = self.m.key_down(key, now);
        let d = self.e.on_key(key, true, now);
        prop_assert_eq!(
            d.suppress,
            expected,
            "key-down {} v {:?}",
            key,
            self.e.mode()
        );

        match self.physical.get_mut(&key) {
            Some(p) => {
                // Autorepeat, nebo nový stisk po „ztraceném key-upu"
                // (dlouhá pauza u spolknuté klávesy).
                let stale = p.suppressed && now.saturating_sub(p.last) >= STALE_KEY_MS;
                if stale {
                    p.suppressed = d.suppress;
                } else {
                    prop_assert_eq!(
                        d.suppress,
                        p.suppressed,
                        "autorepeat {} jinak než stisk",
                        key
                    );
                    // Nemapovatelné klávesy engine nesleduje, takže jejich
                    // autorepeat při přiřazování znovu oznámí „nejde
                    // přiřadit" — je to jen hláška, nic se nespouští.
                    let quiet = matches!(
                        d.ui,
                        None | Some(UiEvent::BindingCancelled {
                            reason: BindingCancel::Timeout
                        })
                    ) || (!mappable(key)
                        && matches!(d.ui, Some(UiEvent::BindingRejected { .. })));
                    prop_assert!(quiet, "autorepeat {} něco spustil: {:?}", key, d.ui);
                }
                p.last = now;
            }
            None => {
                self.physical.insert(
                    key,
                    Press {
                        suppressed: d.suppress,
                        forgotten: false,
                        last: now,
                    },
                );
            }
        }
        Ok(d)
    }

    fn release(&mut self, key: KeyId) -> Result<Decision, TestCaseError> {
        let expected = self.m.key_up(key, self.now);
        let d = self.e.on_key(key, false, self.now);
        prop_assert_eq!(d.suppress, expected, "key-up {}", key);
        match self.physical.remove(&key) {
            Some(p) if !p.forgotten => {
                prop_assert_eq!(
                    d.suppress,
                    p.suppressed,
                    "key-up {} jinak než key-down",
                    key
                )
            }
            // Zapomenutý stisk: engine o něm neví, key-up jde do OS.
            Some(_) => prop_assert!(!d.suppress),
            // Key-up bez key-down: propustit.
            None => prop_assert!(!d.suppress, "key-up bez key-down {} spolknut", key),
        }
        let quiet = matches!(
            d.ui,
            None | Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Timeout
            })
        );
        prop_assert!(quiet, "key-up {} něco spustil: {:?}", key, d.ui);
        prop_assert_eq!(self.e.held(key), None);
        Ok(d)
    }

    fn check_after(
        &mut self,
        d: &Decision,
        mode_before: Mode,
        is_key: bool,
        forced_pad: bool,
    ) -> Result<(), TestCaseError> {
        let mode = self.e.mode();
        prop_assert_eq!(mode, self.m.mode, "režim enginu a modelu");
        prop_assert_eq!(self.e.mapping().toggle_key(), self.m.mapping.toggle);
        let bindings: HashMap<KeyId, Action> = self.e.mapping().bindings().collect();
        prop_assert_eq!(&bindings, &self.m.mapping.keys, "mapování enginu a modelu");

        // Držené klávesy: vlastník, pořadí stisku i čas.
        prop_assert_eq!(self.e.held_len(), self.m.held.len());
        for k in KEYS {
            let e = self.e.held(k).map(|h| (h.owner, h.seq, h.last_ms));
            let m = self.m.held.get(&k).map(|h| (h.owner, h.seq, h.last));
            prop_assert_eq!(e, m, "záznam {}", k);
            if e.is_some() {
                prop_assert!(
                    self.physical.contains_key(&k),
                    "engine drží {} co není dole",
                    k
                );
            }
        }

        // Stav padu: spočítaný nezávisle.
        let expected_pad = self.m.pad();
        prop_assert_eq!(self.e.pad_state(), expected_pad, "stav padu");

        // Co odešlo do ViGEm: při změně režimu a u vynucení vždy, jinak
        // právě tehdy, když se stav změnil.
        let changed = mode != mode_before;
        if changed || forced_pad {
            prop_assert_eq!(d.pad, Some(expected_pad), "pad při změně režimu / vynucení");
        } else if is_key || d.pad.is_some() {
            let want = (expected_pad != self.sent).then_some(expected_pad);
            prop_assert_eq!(d.pad, want, "pad jen při změně");
        }
        if let Some(p) = d.pad {
            self.sent = p;
        }
        prop_assert_eq!(
            self.sent,
            self.e.pad_state(),
            "ViGEm má jiný stav, než se drží"
        );

        // Oznámení: změna režimu se oznámí, beze změny se nic neoznamuje.
        match d.ui {
            Some(UiEvent::ModeChanged { mode: m, .. }) => {
                prop_assert!(changed, "ModeChanged beze změny");
                prop_assert_eq!(m, mode);
            }
            Some(UiEvent::BindingSaved { .. } | UiEvent::BindingCancelled { .. }) => {
                let was_binding = matches!(mode_before, Mode::Binding { .. });
                prop_assert!(was_binding, "konec přiřazování mimo přiřazování");
                let still_binding = matches!(mode, Mode::Binding { .. });
                prop_assert!(!still_binding, "po konci přiřazování se pořád přiřazuje");
            }
            _ => prop_assert!(
                !changed,
                "změna režimu {:?} → {:?} bez oznámení",
                mode_before,
                mode
            ),
        }

        if mode != Mode::Gamepad {
            prop_assert!(
                self.sent.is_neutral(),
                "mimo Gamepad nesmí být pad vychýlený"
            );
        }
        for axis in [
            self.sent.thumb_lx,
            self.sent.thumb_ly,
            self.sent.thumb_rx,
            self.sent.thumb_ry,
        ] {
            prop_assert!(axis != i16::MIN);
        }
        Ok(())
    }

    /// Pustí všechny fyzicky držené klávesy.
    fn release_all(&mut self) -> Result<(), TestCaseError> {
        let mut down: Vec<KeyId> = self.physical.keys().copied().collect();
        down.sort();
        for k in down {
            let before = self.e.mode();
            let d = self.release(k)?;
            self.check_after(&d, before, true, false)?;
        }
        Ok(())
    }
}

proptest! {
    // Nalezené protipříklady se ukládají vedle testu (tests/engine_props.
    // proptest-regressions) a přehrávají se při každém běhu jako první.
    // Výchozí umístění hledá lib.rs, které integrační test nemá.
    //
    // 1500 sekvencí trvá v ladicím buildu ~20 s; mutanty enginu model
    // zabíjí do 0,5 s. Důkladnější běh: `PROPTEST_CASES=50000`.
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1_500),
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::WithSource("proptest-regressions"),
        )),
        ..ProptestConfig::default()
    })]

    #[test]
    fn po_uvolneni_vseho_je_pad_neutralni_a_held_prazdne(
        ops in prop::collection::vec(op(), 0..300)
    ) {
        let mut w = World::new();
        for o in &ops {
            w.step(o)?;
        }
        w.release_all()?;
        prop_assert_eq!(w.e.held_len(), 0, "held po uvolnění všeho");
        prop_assert!(w.e.pad_state().is_neutral(), "pad po uvolnění všeho");
        prop_assert!(w.sent.is_neutral(), "odeslaný stav po uvolnění všeho");
    }

    #[test]
    fn compute_pad_state_nezavisi_na_poradi(
        pressed in prop::collection::vec((0..24usize, 0..1_000u64), 0..12)
    ) {
        let items: Vec<(Action, u64)> =
            pressed.iter().map(|&(a, s)| (action_nr(a), s)).collect();
        let forward = keypad_core::compute_pad_state(items.iter().copied());
        let backward = keypad_core::compute_pad_state(items.iter().rev().copied());
        prop_assert_eq!(forward, backward);
        for axis in [forward.thumb_lx, forward.thumb_ly, forward.thumb_rx, forward.thumb_ry] {
            prop_assert!([0, 23_170, -23_170, 32_767, -32_767].contains(&axis), "osa {}", axis);
        }
    }
}

//! Property testy enginu: libovolná sekvence událostí klávesnice,
//! přepínání, zachytávání, přiřazování, vynucení, výpadků a připojení
//! jednotlivých ovladačů a skoků času.
//!
//! Hlavní vlastnost z ROADMAP.md (Fáze 1): po uvolnění všech kláves je
//! každý ovladač neutrální a `held` prázdné.
//!
//! Engine se po každém kroku porovnává s **nezávislým referenčním
//! modelem** (`Model`) — druhou, co nejpřímočařejší implementací
//! specifikace s vlastním mapováním, vlastní evidencí připravených
//! ovladačů a vlastním výpočtem padu. Dřívější verze testu brala
//! očekávání z enginu samotného (jeho mapování, jeho výpočet padu)
//! a revize ukázala čtyři mutanty enginu, které jí prošly. Model je
//! naopak zabije: nesedí-li režim, připravenost ovladačů, mapování,
//! vlastník, pořadí stisku, spočítaný nebo odeslaný stav kteréhokoli
//! ovladače nebo oznámení (i jeho obsah), test spadne.
//!
//! Navíc se hlídají invarianty „fyzického světa", ze kterých plyne, že
//! se nic nezasekne:
//! - všechny události jednoho stisku (key-down, autorepeaty, key-up)
//!   mají stejné rozhodnutí o potlačení → OS dostane key-up právě ke
//!   key-downům, které viděl (princip 2);
//! - autorepeat nikdy nic nespouští;
//! - nepřipravený ovladač je neutrální a nedrží žádnou klávesu;
//! - mimo režim Gamepad jsou všechny ovladače neutrální, osa nikdy
//!   `i16::MIN`.

use std::collections::{HashMap, HashSet};

use keypad_core::{
    Action, BindingCancel, BindingReject, Decision, DisabledReason, Engine, ForceReason, KeyId,
    Mapping, Mode, ModeCause, Owner, PadAction, PadButton, PadId, PadState, StickDir, ToggleReject,
    UiEvent, BINDING_TIMEOUT_MS, MAX_PADS, STALE_KEY_MS,
};
use proptest::prelude::*;

/// Klávesy, se kterými se hraje: namapované (i dvě na jeden směr, i na
/// různých ovladačích), zkratky všech mapování, Esc, nenamapované,
/// dvojice se stejným scan kódem (šipka × numpad), falešný Ctrl z AltGr
/// a klávesa bez scan kódu.
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

const P1: PadId = PadId::ALL[1];
const P2: PadId = PadId::ALL[2];

#[derive(Debug, Clone)]
enum Op {
    /// Key-down: nový stisk, nebo autorepeat, pokud už je klávesa dole.
    Press(usize),
    /// Key-up: skutečné uvolnění, nebo key-up bez key-down.
    Release(usize),
    Toggle,
    /// Zapnout zachytávání (přepínač ovladače v okně).
    Capture,
    /// Přiřazování: ovladač 0–2 × 24 akcí.
    StartBinding(usize, usize),
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
    /// Ovladač, důvod.
    Disable(usize, u8),
    Enable(usize),
    /// 0 = výchozí (vše na prvním ovladači); 1 = jiná zkratka (X), dvě
    /// klávesy na jednom směru, část kláves na druhém ovladači (SOCD
    /// pár); 2 = Esc namapovaný, zkratka na levém Shiftu, klávesy na
    /// druhém a třetím ovladači; 3 = skoro vše na druhém ovladači.
    SetMapping(u8),
}

fn op() -> impl Strategy<Value = Op> {
    let n = KEYS.len();
    prop_oneof![
        10 => (0..n).prop_map(Op::Press),
        8 => (0..n).prop_map(Op::Release),
        2 => Just(Op::Toggle),
        2 => Just(Op::Capture),
        2 => (0..3usize, 0..24usize).prop_map(|(p, a)| Op::StartBinding(p, a)),
        1 => Just(Op::CancelBinding),
        2 => prop_oneof![0..60u64, 1_400..1_600u64, 9_000..12_000u64].prop_map(Op::Advance),
        2 => prop_oneof![0..60u64, 1_400..1_600u64, 9_000..12_000u64].prop_map(Op::AdvanceNoTick),
        1 => prop_oneof![Just(0u64), Just(u64::MAX), Just(u64::MAX - 5_000), any::<u64>()]
            .prop_map(Op::Jump),
        1 => Just(Op::ForceKeyboard),
        1 => Just(Op::ResetHeld),
        2 => (0..MAX_PADS, 0..3u8).prop_map(|(p, r)| Op::Disable(p, r)),
        4 => (0..MAX_PADS).prop_map(Op::Enable),
        1 => (0..4u8).prop_map(Op::SetMapping),
    ]
}

fn action_nr(i: usize) -> Action {
    Action::all().nth(i).expect("24 akcí")
}

fn pad_nr(i: usize) -> PadId {
    PadId::new(i).expect("ovladač v rozsahu")
}

// ── Nezávislý referenční model ─────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
struct MMapping {
    toggle: KeyId,
    keys: HashMap<KeyId, PadAction>,
}

/// Vazba do mapování enginu i do modelu — každé zvlášť.
fn bind_both(m: &mut Mapping, keys: &mut HashMap<KeyId, PadAction>, k: KeyId, t: PadAction) {
    m.bind(k, t).unwrap();
    keys.insert(k, t);
}

fn mapping_variant(v: u8) -> (Mapping, MMapping) {
    use Action::{Button, LeftStick, RightStick};
    use StickDir::{Left, Right, Up};

    let d = Mapping::default();
    let mut keys: HashMap<KeyId, PadAction> = d.bindings().collect();
    let mut m = d.clone();
    let vazby: Vec<(KeyId, PadAction)> = match v {
        1 => vec![
            (KeyId::NUMPAD_8, PadAction::first(LeftStick(Up))),
            // I a levý Ctrl: protilehlé směry druhého ovladače (SOCD jen
            // v rámci ovladače), mezerník se přesune z prvního.
            (KeyId::I, PadAction::new(P1, LeftStick(Left))),
            (KeyId::LEFT_CTRL, PadAction::new(P1, LeftStick(Right))),
            (KeyId::SPACE, PadAction::new(P1, Button(PadButton::A))),
        ],
        2 => {
            m.unbind(KeyId::LEFT_SHIFT).unwrap();
            keys.remove(&KeyId::LEFT_SHIFT);
            vec![
                (KeyId::ESC, PadAction::first(Button(PadButton::Back))),
                // A vlevo zůstává prvnímu ovladači, D vpravo patří
                // druhému: stisk obou nesmí dělat SOCD napříč ovladači.
                (KeyId::D, PadAction::new(P1, LeftStick(Right))),
                (KeyId::ARROW_UP, PadAction::new(P2, RightStick(Up))),
            ]
        }
        3 => {
            // Všechno na druhý ovladač (první zůstane prázdný), pak
            // prvnímu jen X.
            let mut v: Vec<_> = d
                .bindings()
                .map(|(k, t)| (k, PadAction::new(P1, t.action)))
                .collect();
            v.push((KeyId::X, PadAction::first(Button(PadButton::A))));
            v.push((KeyId::NUMPAD_8, PadAction::new(P2, LeftStick(Up))));
            v
        }
        _ => Vec::new(),
    };
    for (k, t) in vazby {
        bind_both(&mut m, &mut keys, k, t);
    }
    let toggle = match v {
        1 => KeyId::X,
        2 => KeyId::LEFT_SHIFT,
        _ => KeyId::SCROLL_LOCK,
    };
    m.set_toggle_key(toggle).unwrap();
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
    /// Připravené (připojené) ovladače.
    ready: HashSet<PadId>,
    /// Důvod posledního vypnutí ovladače.
    reason: DisabledReason,
    /// Po konci přiřazování zachytávat.
    resume: bool,
    /// Tenhle krok musí vydat stav všech ovladačů (změna režimu,
    /// vynucení).
    emit_all: bool,
    /// Tenhle krok musí vydat stav právě vypnutého ovladače.
    emit_one: Option<PadId>,
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
            ready: HashSet::new(),
            reason: DisabledReason::PadNotConnected,
            resume: false,
            emit_all: false,
            emit_one: None,
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
        self.emit_all = true;
    }

    /// Konec přiřazování: bez ovladače Disabled, jinak zpět do hry,
    /// pokud se hrálo (a konec nebyl vynucený), jinak Klávesnice.
    fn end_binding(&mut self, may_capture: bool) {
        let resume = self.resume;
        self.resume = false;
        let to = if self.ready.is_empty() {
            Mode::Disabled {
                reason: self.reason,
            }
        } else if resume && may_capture {
            Mode::Gamepad
        } else {
            Mode::Keyboard
        };
        self.set_mode(to);
    }

    fn expire(&mut self, now: u64) -> Option<UiEvent> {
        if let Mode::Binding { started_at_ms, .. } = self.mode {
            if now.saturating_sub(started_at_ms) >= BINDING_TIMEOUT_MS {
                self.end_binding(true);
                return Some(UiEvent::BindingCancelled {
                    reason: BindingCancel::Timeout,
                });
            }
        }
        None
    }

    fn toggle(&mut self, cause: ModeCause) -> Option<UiEvent> {
        match self.mode {
            Mode::Keyboard | Mode::Gamepad => {
                let new = if self.mode == Mode::Keyboard {
                    Mode::Gamepad
                } else {
                    Mode::Keyboard
                };
                self.set_mode(new);
                Some(UiEvent::ModeChanged { mode: new, cause })
            }
            Mode::Binding { .. } if cause == ModeCause::Hotkey => Some(UiEvent::BindingRejected {
                key: self.mapping.toggle,
                reason: BindingReject::ToggleKey,
            }),
            Mode::Binding { .. } => Some(UiEvent::ToggleRejected {
                reason: ToggleReject::Binding,
            }),
            Mode::Disabled { reason } => Some(UiEvent::ToggleRejected {
                reason: ToggleReject::Disabled(reason),
            }),
        }
    }

    /// Klávesa, kterou drží OS a model o ní neví, se zapíše jako OS
    /// (co dělá hook podle asynchronního stavu klávesnice).
    fn adopt_os(&mut self, key: KeyId, now: u64) -> bool {
        if !mappable(key) || self.held.contains_key(&key) {
            return false;
        }
        self.seq += 1;
        self.held.insert(
            key,
            MHeld {
                owner: Owner::Os,
                seq: self.seq,
                last: now,
            },
        );
        true
    }

    /// Vrací očekávané potlačení a oznámení.
    fn key_down(&mut self, key: KeyId, now: u64) -> (bool, Option<UiEvent>) {
        let expired = self.expire(now);
        if !mappable(key) {
            let ui =
                matches!(self.mode, Mode::Binding { .. }).then_some(UiEvent::BindingRejected {
                    key,
                    reason: BindingReject::Unmappable,
                });
            return (false, ui.or(expired));
        }
        if let Some(h) = self.held.get_mut(&key) {
            let stale = h.owner != Owner::Os && now.saturating_sub(h.last) >= STALE_KEY_MS;
            if !stale {
                h.last = now;
                return (h.owner != Owner::Os, expired);
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
                    Some(&t) if self.ready.contains(&t.pad) => Owner::Pad(t),
                    _ => Owner::Os,
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
        let ui = if key == self.mapping.toggle {
            self.toggle(ModeCause::Hotkey)
        } else if let Mode::Binding { target, .. } = self.mode {
            if key == KeyId::ESC {
                self.end_binding(true);
                Some(UiEvent::BindingCancelled {
                    reason: BindingCancel::Escape,
                })
            } else {
                // Přesun: klávesa má vždy nejvýš jeden cíl.
                let old = self.mapping.keys.insert(key, target);
                self.end_binding(true);
                Some(UiEvent::BindingSaved {
                    key,
                    target,
                    moved_from: old.filter(|&o| o != target),
                })
            }
        } else {
            None
        };
        (owner != Owner::Os, ui.or(expired))
    }

    fn key_up(&mut self, key: KeyId, now: u64) -> (bool, Option<UiEvent>) {
        let expired = self.expire(now);
        let suppress = self.held.remove(&key).is_some_and(|h| h.owner != Owner::Os);
        (suppress, expired)
    }

    fn capture(&mut self, now: u64) -> Option<UiEvent> {
        let expired = self.expire(now);
        let ui = match self.mode {
            Mode::Keyboard => {
                self.set_mode(Mode::Gamepad);
                Some(UiEvent::ModeChanged {
                    mode: Mode::Gamepad,
                    cause: ModeCause::Gui,
                })
            }
            Mode::Gamepad => None,
            Mode::Disabled { reason } => Some(UiEvent::ToggleRejected {
                reason: ToggleReject::Disabled(reason),
            }),
            Mode::Binding { .. } => {
                self.resume = true;
                None
            }
        };
        ui.or(expired)
    }

    fn start_binding(&mut self, target: PadAction, now: u64) -> Option<UiEvent> {
        let _ = self.expire(now);
        self.resume = match self.mode {
            Mode::Gamepad => true,
            Mode::Binding { .. } => self.resume,
            Mode::Keyboard | Mode::Disabled { .. } => false,
        };
        let new = Mode::Binding {
            target,
            started_at_ms: now,
        };
        self.set_mode(new);
        Some(UiEvent::ModeChanged {
            mode: new,
            cause: ModeCause::Gui,
        })
    }

    fn cancel_binding(&mut self, now: u64) -> Option<UiEvent> {
        let expired = self.expire(now);
        if matches!(self.mode, Mode::Binding { .. }) {
            self.end_binding(true);
            return Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Gui,
            });
        }
        expired
    }

    fn force(&mut self, reason: ForceReason) -> Option<UiEvent> {
        match self.mode {
            Mode::Gamepad => {
                self.set_mode(Mode::Keyboard);
                Some(UiEvent::ModeChanged {
                    mode: Mode::Keyboard,
                    cause: ModeCause::Forced(reason),
                })
            }
            Mode::Binding { .. } => {
                self.end_binding(false);
                Some(UiEvent::BindingCancelled {
                    reason: BindingCancel::Forced(reason),
                })
            }
            // Vynucení vždy potvrdí neutrál.
            Mode::Keyboard | Mode::Disabled { .. } => {
                self.emit_all = true;
                None
            }
        }
    }

    fn disable(&mut self, pad: PadId, reason: DisabledReason) -> Option<UiEvent> {
        self.reason = reason;
        if self.ready.remove(&pad) {
            for h in self.held.values_mut() {
                if matches!(h.owner, Owner::Pad(t) if t.pad == pad) {
                    h.owner = Owner::Swallow;
                }
            }
            if matches!(self.mode, Mode::Keyboard | Mode::Gamepad) && self.ready.is_empty() {
                let new = Mode::Disabled { reason };
                self.set_mode(new);
                return Some(UiEvent::ModeChanged {
                    mode: new,
                    cause: ModeCause::PadStatus,
                });
            }
            self.emit_one = Some(pad);
            return None;
        }
        match self.mode {
            Mode::Disabled { reason: r } if r != reason => {
                let new = Mode::Disabled { reason };
                self.set_mode(new);
                Some(UiEvent::ModeChanged {
                    mode: new,
                    cause: ModeCause::PadStatus,
                })
            }
            _ => None,
        }
    }

    fn enable(&mut self, pad: PadId) -> Option<UiEvent> {
        if !self.ready.insert(pad) {
            return None;
        }
        if matches!(self.mode, Mode::Disabled { .. }) {
            self.set_mode(Mode::Keyboard);
            return Some(UiEvent::ModeChanged {
                mode: Mode::Keyboard,
                cause: ModeCause::PadStatus,
            });
        }
        None
    }

    /// Stav jednoho ovladače spočítaný nezávisle na enginu: tlačítka
    /// OR, triggery, SOCD podle pořadí stisku, diagonála 23170 — jen
    /// z kláves, které patří tomuhle ovladači.
    fn pad(&self, pad: PadId) -> PadState {
        let mut p = PadState::NEUTRAL;
        // [páčka][směr] → nejnovější seq
        let mut newest: HashMap<(bool, StickDir), u64> = HashMap::new();
        for h in self.held.values() {
            let Owner::Pad(t) = h.owner else { continue };
            if t.pad != pad {
                continue;
            }
            match t.action {
                Action::Button(b) => p.buttons |= b.mask(),
                Action::LeftTrigger => p.left_trigger = 255,
                Action::RightTrigger => p.right_trigger = 255,
                Action::LeftStick(d) | Action::RightStick(d) => {
                    let right = matches!(t.action, Action::RightStick(_));
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
    /// Engine záznam o stisku zapomněl (reset_held). Stisk, který OS
    /// viděl, hook při dalším autorepeatu ohlásí jako klávesu OS (dál se
    /// kontroluje). Spolknutý stisk je pro engine nový — jeho key-up jde
    /// do OS, i když key-down nešel; ten se na principu 2 nekontroluje
    /// (neškodný key-up navíc).
    forgotten: bool,
    /// Čas poslední události (kvůli pravidlu ztraceného key-upu).
    last: u64,
}

struct World {
    e: Engine,
    m: Model,
    now: u64,
    physical: HashMap<KeyId, Press>,
    /// Co naposledy odešlo do ViGEm — pro každý ovladač.
    sent: [PadState; MAX_PADS],
}

impl World {
    fn new() -> World {
        World {
            e: Engine::new(Mapping::default()),
            m: Model::new(),
            now: 1_000,
            physical: HashMap::new(),
            sent: [PadState::NEUTRAL; MAX_PADS],
        }
    }

    fn step(&mut self, op: &Op) -> Result<(), TestCaseError> {
        let mode_before = self.e.mode();
        self.m.emit_all = false;
        self.m.emit_one = None;
        let now = self.now;
        let (d, want_ui) = match *op {
            Op::Press(i) => self.press(KEYS[i])?,
            Op::Release(i) => self.release(KEYS[i])?,
            Op::Toggle => {
                let expired = self.m.expire(now);
                let ui = self.m.toggle(ModeCause::Gui).or(expired);
                (self.e.toggle(now), ui)
            }
            Op::Capture => (self.e.capture(now), self.m.capture(now)),
            Op::StartBinding(p, a) => {
                let target = PadAction::new(pad_nr(p), action_nr(a));
                (
                    self.e.start_binding(target, now),
                    self.m.start_binding(target, now),
                )
            }
            Op::CancelBinding => (self.e.cancel_binding(now), self.m.cancel_binding(now)),
            Op::Advance(ms) => {
                self.now = now.saturating_add(ms);
                (self.e.tick(self.now), self.m.expire(self.now))
            }
            Op::AdvanceNoTick(ms) => {
                self.now = now.saturating_add(ms);
                return Ok(());
            }
            Op::Jump(t) => {
                self.now = t;
                return Ok(());
            }
            Op::ForceKeyboard => {
                let ui = self.m.force(ForceReason::Watchdog);
                (self.e.force_keyboard(ForceReason::Watchdog), ui)
            }
            Op::ResetHeld => {
                for p in self.physical.values_mut() {
                    p.forgotten = true;
                }
                let ui = self.m.force(ForceReason::SessionLock);
                self.m.held.clear();
                self.m.emit_all = true;
                (self.e.reset_held(ForceReason::SessionLock), ui)
            }
            Op::Disable(p, r) => {
                let reason = match r {
                    0 => DisabledReason::ViGEmMissing,
                    1 => DisabledReason::PadNotConnected,
                    _ => DisabledReason::PadError,
                };
                let pad = pad_nr(p);
                (self.e.disable(pad, reason), self.m.disable(pad, reason))
            }
            Op::Enable(p) => {
                let pad = pad_nr(p);
                (self.e.enable(pad), self.m.enable(pad))
            }
            Op::SetMapping(v) => {
                let (em, mm) = mapping_variant(v);
                let ui = if matches!(self.m.mode, Mode::Gamepad | Mode::Binding { .. }) {
                    self.m.force(ForceReason::MappingChanged)
                } else {
                    None
                };
                self.m.mapping = mm;
                (self.e.set_mapping(em), ui)
            }
        };
        let is_key = matches!(op, Op::Press(_) | Op::Release(_));
        if !is_key {
            prop_assert!(!d.suppress, "příkaz nemá co potlačovat");
        }
        self.check_after(&d, want_ui, mode_before)
    }

    fn press(&mut self, key: KeyId) -> Result<(Decision, Option<UiEvent>), TestCaseError> {
        let now = self.now;
        // Klávesa, o které engine zapomněl (reset_held): hook se podívá
        // do asynchronního stavu klávesnice. Drží-li ji OS (viděl její
        // key-down), ohlásí ji enginu jako klávesu OS — autorepeat
        // i key-up jdou pak dál do OS a nic nevisí (princip 2 platí
        // i přes zapomenutí). Stisk, který OS neviděl (spolknutý), je
        // pro engine nový stisk — LL hook autorepeat nerozliší.
        match self.physical.get(&key).map(|p| (p.forgotten, p.suppressed)) {
            Some((true, false)) => {
                let a = self.e.adopt_os_key(key, now);
                let b = self.m.adopt_os(key, now);
                prop_assert_eq!(a, b, "převzetí klávesy OS {}", key);
                if let Some(p) = self.physical.get_mut(&key) {
                    p.forgotten = false;
                }
            }
            Some((true, true)) => {
                self.physical.remove(&key);
            }
            _ => {}
        }
        let (expected, want_ui) = self.m.key_down(key, now);
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
                    if d.ui.is_none() {
                        prop_assert!(d.pads.is_empty(), "autorepeat {} hnul ovladačem", key);
                    }
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
        Ok((d, want_ui))
    }

    fn release(&mut self, key: KeyId) -> Result<(Decision, Option<UiEvent>), TestCaseError> {
        let (expected, want_ui) = self.m.key_up(key, self.now);
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
        Ok((d, want_ui))
    }

    fn check_after(
        &mut self,
        d: &Decision,
        want_ui: Option<UiEvent>,
        mode_before: Mode,
    ) -> Result<(), TestCaseError> {
        let mode = self.e.mode();
        prop_assert_eq!(mode, self.m.mode, "režim enginu a modelu");
        prop_assert_eq!(self.e.mapping().toggle_key(), self.m.mapping.toggle);
        let bindings: HashMap<KeyId, PadAction> = self.e.mapping().bindings().collect();
        prop_assert_eq!(&bindings, &self.m.mapping.keys, "mapování enginu a modelu");
        for p in PadId::ALL {
            prop_assert_eq!(
                self.e.is_ready(p),
                self.m.ready.contains(&p),
                "připravenost {:?}",
                p
            );
        }

        // Držené klávesy: vlastník, pořadí stisku i čas.
        prop_assert_eq!(self.e.held_len(), self.m.held.len());
        for k in KEYS {
            let e = self.e.held(k).map(|h| (h.owner, h.seq, h.last_ms));
            let m = self.m.held.get(&k).map(|h| (h.owner, h.seq, h.last));
            prop_assert_eq!(e, m, "záznam {}", k);
            if let Some((owner, ..)) = e {
                prop_assert!(
                    self.physical.contains_key(&k),
                    "engine drží {} co není dole",
                    k
                );
                if let Owner::Pad(t) = owner {
                    prop_assert!(mode == Mode::Gamepad, "klávesa ovladače mimo Gamepad");
                    prop_assert!(
                        self.e.is_ready(t.pad),
                        "{} patří nepřipravenému {:?}",
                        k,
                        t.pad
                    );
                }
            }
        }

        // Oznámení — i obsah.
        prop_assert_eq!(d.ui, want_ui, "oznámení");
        if mode != mode_before {
            prop_assert!(
                d.ui.is_some(),
                "změna režimu {:?} → {:?} bez oznámení",
                mode_before,
                mode
            );
        }

        // Stavy ovladačů: spočítané nezávisle. Do ViGEm se posílají
        // všechny při změně režimu a u vynucení, vypnutý ovladač vždy
        // (neutrál), jinak právě ty, které se změnily.
        for p in PadId::ALL {
            let i = p.index();
            let expected = self.m.pad(p);
            prop_assert_eq!(self.e.pad_state(p), expected, "stav {:?}", p);
            let want = if self.m.emit_all || self.m.emit_one == Some(p) {
                Some(expected)
            } else {
                (expected != self.sent[i]).then_some(expected)
            };
            prop_assert_eq!(d.pads.get(p), want, "odeslaný stav {:?}", p);
            if let Some(s) = d.pads.get(p) {
                self.sent[i] = s;
            }
            prop_assert_eq!(
                self.sent[i],
                self.e.pad_state(p),
                "ViGEm má u {:?} jiný stav, než se drží",
                p
            );
            if mode != Mode::Gamepad || !self.e.is_ready(p) {
                prop_assert!(
                    self.sent[i].is_neutral(),
                    "{:?} je vychýlený mimo Gamepad nebo nepřipravený",
                    p
                );
            }
            for axis in [
                self.sent[i].thumb_lx,
                self.sent[i].thumb_ly,
                self.sent[i].thumb_rx,
                self.sent[i].thumb_ry,
            ] {
                prop_assert!(axis != i16::MIN);
            }
        }

        // Disabled právě tehdy, když není připravený žádný ovladač
        // (přiřazování smí běžet s ovladači i bez nich).
        if !matches!(mode, Mode::Binding { .. }) {
            let any_ready = PadId::ALL.into_iter().any(|p| self.e.is_ready(p));
            prop_assert_eq!(
                matches!(mode, Mode::Disabled { .. }),
                !any_ready,
                "Disabled × připravené ovladače v {:?}",
                mode
            );
            prop_assert!(!self.m.resume, "příznak návratu mimo přiřazování");
        }
        Ok(())
    }

    /// Pustí všechny fyzicky držené klávesy.
    fn release_all(&mut self) -> Result<(), TestCaseError> {
        let mut down: Vec<KeyId> = self.physical.keys().copied().collect();
        down.sort();
        for k in down {
            let before = self.e.mode();
            self.m.emit_all = false;
            self.m.emit_one = None;
            let (d, want_ui) = self.release(k)?;
            self.check_after(&d, want_ui, before)?;
        }
        Ok(())
    }
}

proptest! {
    // Nalezené protipříklady se ukládají vedle testu (tests/engine_props.
    // proptest-regressions) a přehrávají se při každém běhu jako první.
    // Výchozí umístění hledá lib.rs, které integrační test nemá.
    //
    // 1500 sekvencí trvá v ladicím buildu desítky sekund; mutanty enginu
    // model zabíjí do vteřiny. Důkladnější běh: `PROPTEST_CASES=50000`.
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
        for p in PadId::ALL {
            prop_assert!(w.e.pad_state(p).is_neutral(), "{:?} po uvolnění všeho", p);
            prop_assert!(w.sent[p.index()].is_neutral(), "odeslaný {:?} po uvolnění všeho", p);
        }
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

//! Property testy enginu: libovolná sekvence událostí klávesnice,
//! přepínání, zachytávání, přiřazování (nahradit i přidat), úprav
//! mapování z editoru za běhu, vynucení, výpadků a připojení
//! jednotlivých ovladačů a skoků času.
//!
//! Hlavní vlastnost z ROADMAP.md (Fáze 1): po uvolnění všech kláves je
//! každý ovladač neutrální a `held` prázdné.
//!
//! Engine se po každém kroku porovnává s **nezávislým referenčním
//! modelem** (`Model`) — druhou, co nejpřímočařejší implementací
//! specifikace s vlastním mapováním, vlastní evidencí připravených
//! ovladačů, vlastním výpočtem padu i živého stavu (vlastní SOCD)
//! a vlastní revizí mapování. Dřívější verze testu brala očekávání
//! z enginu samotného (jeho mapování, jeho výpočet padu) a revize ukázala
//! čtyři mutanty enginu, které jí prošly. Model je naopak zabije:
//! nesedí-li režim, připravenost ovladačů, mapování, revize, druh
//! přiřazování, vlastník, pořadí stisku, spočítaný nebo odeslaný stav
//! kteréhokoli ovladače, živý stav (s klávesami OS i bez nich) nebo
//! oznámení (i jeho obsah a jeho zabalení pro okno), test spadne.
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
    Action, BindKind, BindingCancel, BindingReject, Decision, DisabledReason, Engine, ForceReason,
    KeyId, LiveInputs, Mapping, MappingError, Mode, ModeCause, Owner, PadAction, PadButton, PadId,
    PadState, StickDir, ToggleReject, UiEvent, BINDING_TIMEOUT_MS, MAX_PADS, STALE_KEY_MS,
};
use proptest::prelude::*;

/// Klávesy, se kterými se hraje: namapované (i dvě na jeden směr, i na
/// různých ovladačích), zkratky všech mapování, Esc, nenamapované,
/// dvojice se stejným scan kódem (šipka × numpad), falešný Ctrl z AltGr
/// a pravý Alt (AltGr na českém rozložení = obojí za sebou), klávesa bez
/// scan kódu, levá Win (patří Windows) a modifikátory (Shift, Ctrl, Alt —
/// při přiřazování ťuknutí a zkratky Windows).
const KEYS: [KeyId; 20] = [
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
    KeyId::LEFT_WIN,
    KeyId::LEFT_ALT,
    KeyId::RIGHT_ALT,
];

const P1: PadId = PadId::ALL[1];
const P2: PadId = PadId::ALL[2];

#[derive(Debug, Clone)]
enum Op {
    /// Key-down: nový stisk, nebo autorepeat, pokud už je klávesa dole.
    Press(usize),
    /// Key-up: skutečné uvolnění, nebo key-up bez key-down.
    Release(usize),
    /// AltGr na českém rozložení, jak ho posílají Windows: falešný levý
    /// Ctrl a hned pravý Alt (`true`), puštění v tomtéž pořadí (`false`).
    /// Obě klávesy jsou v `KEYS` i samostatně — tahle dvojice těsně za
    /// sebou by z nich ale vznikla jen zřídka.
    AltGr(bool),
    Toggle,
    /// Zapnout zachytávání (přepínač ovladače v okně).
    Capture,
    /// Přiřazování: ovladač 0–2 × 24 akcí × druh (`true` = přidat).
    StartBinding(usize, usize, bool),
    CancelBinding,
    /// Živá výměna celého mapování z editoru (varianta jako `SetMapping`,
    /// ale bez vynucení — „Zpět").
    ReplaceMapping(u8),
    /// Editor: vyprázdnit vstup (ovladač × akce) a vyměnit živě.
    UnbindTarget(usize, usize),
    /// Editor: odebrat ovladač (vymazat jeho klávesy) a vyměnit živě.
    ClearPad(usize),
    /// Editor: výchozí klávesy prvního ovladače a vyměnit živě.
    DefaultsFirstPad,
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

/// Akce (pořadí `Action::all()`), které mají ve výchozím mapování klávesu
/// z `KEYS`: ls_up (W), ls_left (A), ls_right (D), A (mezerník), L3
/// (levý Shift), LT (1). Přiřazování a vyprázdnění míří na ně častěji —
/// jen tak náhoda poskládá vstup s víc klávesami a pak ho nahradí jednou
/// z nich (revize při čistém odebrání), přesun mezi ovladači a podobně.
const HOT_ACTIONS: [usize; 6] = [0, 2, 3, 8, 14, 22];

fn action_idx() -> impl Strategy<Value = usize> {
    prop_oneof![0..24usize, prop::sample::select(HOT_ACTIONS.to_vec())]
}

fn op() -> impl Strategy<Value = Op> {
    let n = KEYS.len();
    prop_oneof![
        10 => (0..n).prop_map(Op::Press),
        8 => (0..n).prop_map(Op::Release),
        1 => any::<bool>().prop_map(Op::AltGr),
        2 => Just(Op::Toggle),
        2 => Just(Op::Capture),
        3 => (0..3usize, action_idx(), any::<bool>())
            .prop_map(|(p, a, k)| Op::StartBinding(p, a, k)),
        1 => Just(Op::CancelBinding),
        1 => (0..4u8).prop_map(Op::ReplaceMapping),
        1 => (0..MAX_PADS, action_idx()).prop_map(|(p, a)| Op::UnbindTarget(p, a)),
        1 => (0..MAX_PADS).prop_map(Op::ClearPad),
        1 => Just(Op::DefaultsFirstPad),
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

#[test]
fn caste_akce_maji_klavesu_v_keys() {
    let d = Mapping::default();
    for i in HOT_ACTIONS {
        let a = PadAction::first(action_nr(i));
        assert!(
            d.keys_for(a).any(|k| KEYS.contains(&k)),
            "{a:?} nemá klávesu v KEYS"
        );
    }
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

/// Win (E0 0x5B / E0 0x5C) — zapsaná syrovými kódy, ne přes
/// `KeyId::is_reserved`, ať model nezdědí chybu enginu.
fn reserved(k: KeyId) -> bool {
    k.extended && (k.scan == 0x5B || k.scan == 0x5C)
}

fn mappable(k: KeyId) -> bool {
    (1..=0x7F).contains(&k.scan) && !reserved(k)
}

/// Ctrl, Shift a Alt (levé i pravé) — syrovými kódy, ne přes
/// `KeyId::is_modifier`.
fn modifier(k: KeyId) -> bool {
    [
        (0x1D, false),
        (0x1D, true),
        (0x2A, false),
        (0x36, false),
        (0x38, false),
        (0x38, true),
    ]
    .contains(&(k.scan, k.extended))
}

/// Bit akce v živém stavu: pozice v `Action::all()` — ne
/// `Action::index`, ať model nezdědí chybu číslování enginu.
fn model_bit(a: Action) -> u32 {
    let i = Action::all().position(|x| x == a).expect("akce je v all()");
    1 << i
}

/// Znaménko osy z nejnovějších stisků protilehlých směrů (vyhrává
/// novější, shodné = 0).
fn model_axis(neg: Option<u64>, pos: Option<u64>) -> i8 {
    match (neg, pos) {
        (Some(n), Some(p)) if p > n => 1,
        (Some(n), Some(p)) if n > p => -1,
        (Some(_), None) => -1,
        (None, Some(_)) => 1,
        _ => 0,
    }
}

/// „Výchozí klávesy" podle specifikace: první ovladač dostane výchozí
/// rozvržení, klávesy jiných ovladačů a zkratka se přeskočí.
fn model_defaults_first(m: &MMapping) -> MMapping {
    let mut keys: HashMap<KeyId, PadAction> = m
        .keys
        .iter()
        .filter(|(_, t)| t.pad != PadId::FIRST)
        .map(|(&k, &t)| (k, t))
        .collect();
    for (k, t) in mapping_variant(0).1.keys {
        if k != m.toggle && !keys.contains_key(&k) {
            keys.insert(k, t);
        }
    }
    MMapping {
        toggle: m.toggle,
        keys,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MHeld {
    owner: Owner,
    seq: u64,
    last: u64,
}

/// Živý stav jednoho ovladače v modelu: (bity držených akcí, levá
/// páčka, pravá páčka).
type MLive = (u32, (i8, i8), (i8, i8));

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
    /// Druh přiřazování (platí jen v režimu Binding).
    kind: BindKind,
    /// Revize mapování: +1 při každé skutečné změně.
    rev: u64,
    /// Modifikátor stisknutý při přiřazování, který se přiřadí při
    /// key-upu, pokud mezitím nepřišel jiný stisk.
    tap: Option<KeyId>,
    /// Při přiřazování právě přišel falešný Ctrl z AltGr: pravý Alt hned
    /// po něm se ťuknutím nepřiřadí.
    altgr: bool,
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
            kind: BindKind::Replace,
            rev: 0,
            tap: None,
            altgr: false,
            emit_all: false,
            emit_one: None,
        }
    }

    /// Nové mapování; revize roste jen s jiným obsahem.
    fn store_mapping(&mut self, m: MMapping) {
        if m != self.mapping {
            self.rev += 1;
        }
        self.mapping = m;
    }

    /// Živá výměna z editoru: běžící přiřazování se zruší (vrací se, kam
    /// patří), režim ani vlastníci se jinak nemění.
    fn replace_mapping(&mut self, m: MMapping, now: u64) -> Option<UiEvent> {
        let expired = self.expire(now);
        let ui = if matches!(self.mode, Mode::Binding { .. }) {
            self.end_binding(true);
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Gui,
            })
        } else {
            None
        };
        self.store_mapping(m);
        ui.or(expired)
    }

    /// Živý stav: (bity držených akcí, levá páčka, pravá páčka) každého
    /// ovladače. Pad → jeho cíl, Swallow → cíl v mapování, Os → cíl
    /// v mapování jen s `include_os`.
    fn live(&self, include_os: bool) -> [MLive; MAX_PADS] {
        let mut bits = [0u32; MAX_PADS];
        // (ovladač, pravá páčka?, směr) → nejnovější seq
        let mut newest: HashMap<(usize, bool, StickDir), u64> = HashMap::new();
        for (k, h) in &self.held {
            let t = match h.owner {
                Owner::Pad(t) => Some(t),
                Owner::Swallow => self.mapping.keys.get(k).copied(),
                Owner::Os if include_os => self.mapping.keys.get(k).copied(),
                Owner::Os => None,
            };
            let Some(t) = t else { continue };
            let p = t.pad.index();
            bits[p] |= model_bit(t.action);
            let stick = match t.action {
                Action::LeftStick(d) => Some((false, d)),
                Action::RightStick(d) => Some((true, d)),
                _ => None,
            };
            if let Some((right, d)) = stick {
                let e = newest.entry((p, right, d)).or_insert(0);
                *e = (*e).max(h.seq);
            }
        }
        let mut out = [(0, (0, 0), (0, 0)); MAX_PADS];
        for (p, o) in out.iter_mut().enumerate() {
            let dir = |right: bool, d: StickDir| newest.get(&(p, right, d)).copied();
            let stick = |right: bool| {
                (
                    model_axis(dir(right, StickDir::Left), dir(right, StickDir::Right)),
                    model_axis(dir(right, StickDir::Down), dir(right, StickDir::Up)),
                )
            };
            *o = (bits[p], stick(false), stick(true));
        }
        out
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
        self.tap = None;
        self.altgr = false;
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
            self.tap = None;
            // Falešný Ctrl z AltGr — syrovým kódem, ne přes konstantu jádra.
            self.altgr = matches!(self.mode, Mode::Binding { .. })
                && (key.scan, key.extended) == (0x21D, false);
            let ui =
                matches!(self.mode, Mode::Binding { .. }).then_some(UiEvent::BindingRejected {
                    key,
                    reason: if reserved(key) {
                        BindingReject::Reserved
                    } else {
                        BindingReject::Unmappable
                    },
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
        let binding = matches!(self.mode, Mode::Binding { .. });
        // Windows drží modifikátor → nový stisk je jejich zkratka.
        let chord = self
            .held
            .iter()
            .any(|(&k, h)| modifier(k) && h.owner == Owner::Os);
        let owner = if key == self.mapping.toggle {
            Owner::Swallow
        } else {
            match self.mode {
                Mode::Binding { .. } if modifier(key) || chord => Owner::Os,
                Mode::Binding { .. } => Owner::Swallow,
                Mode::Gamepad => match self.mapping.keys.get(&key) {
                    Some(&t) if self.ready.contains(&t.pad) => Owner::Pad(t),
                    _ => Owner::Os,
                },
                _ => Owner::Os,
            }
        };
        if binding {
            // Samotný modifikátor čeká na key-up; cokoli jiného ťuknutí
            // ruší. Pravý Alt hned po falešném Ctrl je AltGr — ten ne.
            let altgr = std::mem::take(&mut self.altgr) && (key.scan, key.extended) == (0x38, true);
            self.tap =
                (key != self.mapping.toggle && modifier(key) && !chord && !altgr).then_some(key);
        }
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
            if owner == Owner::Os {
                None
            } else if key == KeyId::ESC {
                self.end_binding(true);
                Some(UiEvent::BindingCancelled {
                    reason: BindingCancel::Escape,
                })
            } else {
                Some(self.bind(key, target))
            }
        } else {
            None
        };
        (owner != Owner::Os, ui.or(expired))
    }

    /// Uloží vazbu a ukončí přiřazování.
    fn bind(&mut self, key: KeyId, target: PadAction) -> UiEvent {
        // Nahradit: ostatní klávesy cíle pryč. Přidat: zůstanou.
        let mut removed = 0;
        if self.kind == BindKind::Replace {
            let before = self.mapping.keys.len();
            self.mapping
                .keys
                .retain(|&k, &mut t| k == key || t != target);
            removed = before - self.mapping.keys.len();
        }
        // Přesun: klávesa má vždy nejvýš jeden cíl.
        let old = self.mapping.keys.insert(key, target);
        if old != Some(target) || removed > 0 {
            self.rev += 1;
        }
        self.end_binding(true);
        UiEvent::BindingSaved {
            key,
            target,
            moved_from: old.filter(|&o| o != target),
        }
    }

    fn key_up(&mut self, key: KeyId, now: u64) -> (bool, Option<UiEvent>) {
        let expired = self.expire(now);
        let suppress = self.held.remove(&key).is_some_and(|h| h.owner != Owner::Os);
        // Ťuknutí modifikátorem: přiřadí se teď (konec přiřazování ho
        // smazal už v `expire`).
        let ui = match (self.tap, self.mode) {
            (Some(t), Mode::Binding { target, .. }) if t == key => Some(self.bind(key, target)),
            _ => None,
        };
        (suppress, ui.or(expired))
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

    fn start_binding(&mut self, target: PadAction, kind: BindKind, now: u64) -> Option<UiEvent> {
        let _ = self.expire(now);
        self.kind = kind;
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
            i32::from(model_axis(
                newest.get(&(stick, neg)).copied(),
                newest.get(&(stick, pos)).copied(),
            ))
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
        if let Op::AltGr(dolu) = *op {
            let i = |k: KeyId| {
                KEYS.iter()
                    .position(|&x| x == k)
                    .expect("klávesa AltGr je v KEYS")
            };
            let (ctrl, alt) = (i(KeyId::ALTGR_FAKE_CTRL), i(KeyId::RIGHT_ALT));
            return if dolu {
                self.step(&Op::Press(ctrl))?;
                self.step(&Op::Press(alt))
            } else {
                self.step(&Op::Release(ctrl))?;
                self.step(&Op::Release(alt))
            };
        }
        let mode_before = self.e.mode();
        self.m.emit_all = false;
        self.m.emit_one = None;
        let now = self.now;
        let (d, want_ui) = match *op {
            Op::Press(i) => self.press(KEYS[i])?,
            Op::Release(i) => self.release(KEYS[i])?,
            Op::AltGr(_) => unreachable!("AltGr se rozkládá na dva stisky výš"),
            Op::Toggle => {
                let expired = self.m.expire(now);
                let ui = self.m.toggle(ModeCause::Gui).or(expired);
                (self.e.toggle(now), ui)
            }
            Op::Capture => (self.e.capture(now), self.m.capture(now)),
            Op::StartBinding(p, a, add) => {
                let target = PadAction::new(pad_nr(p), action_nr(a));
                let kind = if add {
                    BindKind::Add
                } else {
                    BindKind::Replace
                };
                (
                    self.e.start_binding(target, kind, now),
                    self.m.start_binding(target, kind, now),
                )
            }
            Op::CancelBinding => (self.e.cancel_binding(now), self.m.cancel_binding(now)),
            Op::ReplaceMapping(v) => {
                let (em, mm) = mapping_variant(v);
                let ui = self.m.replace_mapping(mm, now);
                (self.e.replace_mapping(em, now), ui)
            }
            Op::UnbindTarget(p, a) => {
                let t = PadAction::new(pad_nr(p), action_nr(a));
                let mut mm = self.m.mapping.clone();
                mm.keys.retain(|_, &mut x| x != t);
                let removed = self.m.mapping.keys.len() - mm.keys.len();
                let want = if mm.keys.is_empty() && removed > 0 {
                    Err(MappingError::WouldBeEmpty)
                } else {
                    Ok(removed)
                };
                let mut em = self.e.mapping().clone();
                prop_assert_eq!(em.unbind_target(t), want, "vyprázdnění {:?}", t);
                if want.is_err() {
                    prop_assert_eq!(&em, self.e.mapping(), "neúspěch nic nezměnil");
                    return Ok(());
                }
                let ui = self.m.replace_mapping(mm, now);
                (self.e.replace_mapping(em, now), ui)
            }
            Op::ClearPad(p) => {
                let pad = pad_nr(p);
                let mut mm = self.m.mapping.clone();
                mm.keys.retain(|_, x| x.pad != pad);
                let removed = self.m.mapping.keys.len() - mm.keys.len();
                let want = if mm.keys.is_empty() && removed > 0 {
                    Err(MappingError::WouldBeEmpty)
                } else {
                    Ok(removed)
                };
                let mut em = self.e.mapping().clone();
                prop_assert_eq!(em.clear_pad(pad), want, "vymazání {:?}", pad);
                if want.is_err() {
                    prop_assert_eq!(&em, self.e.mapping(), "neúspěch nic nezměnil");
                    return Ok(());
                }
                let ui = self.m.replace_mapping(mm, now);
                (self.e.replace_mapping(em, now), ui)
            }
            Op::DefaultsFirstPad => {
                let em = self.e.mapping().defaults_for_first_pad();
                let mm = model_defaults_first(&self.m.mapping);
                prop_assert!(!em.is_empty(), "výchozí klávesy daly prázdné mapování");
                prop_assert_eq!(
                    Mapping::new(em.toggle_key(), em.bindings()),
                    Ok(em.clone()),
                    "výchozí klávesy daly neplatné mapování"
                );
                let ui = self.m.replace_mapping(mm, now);
                (self.e.replace_mapping(em, now), ui)
            }
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
                self.m.store_mapping(mm);
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
        //
        // Hook se Windows ptá jen tehdy, když engine o novém stisku
        // rozhoduje (`claims_new_press` — dotaz je drahý). Jinak musí
        // převzetí i nový stisk dopadnout úplně stejně: ověří se na
        // kopiích enginu, celým stavem.
        if self.e.held(key).is_none() && !self.e.claims_new_press(key) {
            let (mut prevzeti, mut novy) = (self.e.clone(), self.e.clone());
            let _ = prevzeti.adopt_os_key(key, now);
            let a = prevzeti.on_key(key, true, now);
            let b = novy.on_key(key, true, now);
            prop_assert_eq!(a, b, "převzetí × nový stisk {}", key);
            prop_assert_eq!(
                format!("{prevzeti:?}"),
                format!("{novy:?}"),
                "stav po převzetí × po novém stisku {}",
                key
            );
        }
        match self.physical.get(&key).map(|p| (p.forgotten, p.suppressed)) {
            Some((true, false)) => {
                if self.e.claims_new_press(key) {
                    let a = self.e.adopt_os_key(key, now);
                    let b = self.m.adopt_os(key, now);
                    prop_assert_eq!(a, b, "převzetí klávesy OS {}", key);
                }
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
        // Key-up spouští jen uložení ťuknutého modifikátoru (OQ 55).
        let quiet = matches!(
            d.ui,
            None | Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Timeout
            })
        ) || (modifier(key)
            && matches!(d.ui, Some(UiEvent::BindingSaved { key: k, .. }) if k == key));
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
        prop_assert_eq!(self.e.mapping_rev(), self.m.rev, "revize mapování");
        prop_assert_eq!(
            self.e.binding_kind(),
            matches!(self.m.mode, Mode::Binding { .. }).then_some(self.m.kind),
            "druh přiřazování"
        );
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

        // Oznámení — i obsah a jeho cesta do okna (48 bitů v atomiku).
        prop_assert_eq!(d.ui, want_ui, "oznámení");
        if let Some(ev) = d.ui {
            match ev {
                UiEvent::ModeChanged { .. } => prop_assert_eq!(ev.pack(), None),
                _ => {
                    let bits = ev.pack();
                    prop_assert!(bits.is_some_and(|b| b < 1 << 48), "{:?} → {:?}", ev, bits);
                    prop_assert_eq!(bits.and_then(UiEvent::unpack), Some(ev), "zabalení");
                }
            }
        }

        // Živý stav pro okno — bez kláves OS i s nimi (okno v popředí).
        let live_game = self.e.live_inputs(false);
        for include_os in [false, true] {
            let el = self.e.live_inputs(include_os);
            let ml = self.m.live(include_os);
            for p in PadId::ALL {
                let (e, (bits, left, right)) = (el[p.index()], ml[p.index()]);
                prop_assert_eq!(
                    (e.held().bits(), e.left_stick(), e.right_stick()),
                    (bits, left, right),
                    "živý stav {:?} (include_os {})",
                    p,
                    include_os
                );
                prop_assert_eq!(LiveInputs::from_bits(e.bits()), e);
            }
        }
        // Co dostává hra, je vždy i fyzicky držené (svítí plně ⊆ svítí).
        for p in PadId::ALL {
            let hra = self.e.pad_state(p).active_inputs().bits();
            let drzi = live_game[p.index()].held().bits();
            prop_assert_eq!(hra & !drzi, 0, "{:?} hraje vstup, který nedrží", p);
        }
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
            prop_assert!(self.m.tap.is_none(), "ťuknutí mimo přiřazování");
            prop_assert!(!self.m.altgr, "AltGr mimo přiřazování");
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

    /// Oznámení pro okno tam a zpět — i klávesy, které engine nesleduje
    /// (scan až 0xFFFF, s E0 i bez). Rozbalení libovolných bitů dá buď
    /// nic, nebo přesně to, co by se zabalilo zpět na tytéž bity.
    #[test]
    fn oznameni_jdou_zabalit_a_rozbalit(
        scan in any::<u16>(),
        e0 in any::<bool>(),
        druh in 0..4u8,
        cil in (0..MAX_PADS, 0..24usize),
        odkud in prop::option::of((0..MAX_PADS, 0..24usize)),
        duvod in 0..13usize,
        bit in 0..64u32,
        syrove in any::<u64>(),
    ) {
        let key = KeyId { scan, extended: e0 };
        let pa = |(p, a): (usize, usize)| PadAction::new(pad_nr(p), action_nr(a));
        let zruseni = [
            BindingCancel::Escape,
            BindingCancel::Timeout,
            BindingCancel::Gui,
            BindingCancel::PadStatus,
        ]
        .into_iter()
        .chain(ForceReason::ALL.map(BindingCancel::Forced))
        .collect::<Vec<_>>();
        let e = match druh {
            0 => UiEvent::BindingSaved { key, target: pa(cil), moved_from: odkud.map(pa) },
            1 => UiEvent::BindingRejected {
                key,
                reason: [
                    BindingReject::ToggleKey,
                    BindingReject::Unmappable,
                    BindingReject::Reserved,
                ][duvod % 3],
            },
            2 => UiEvent::BindingCancelled { reason: zruseni[duvod] },
            _ => UiEvent::ToggleRejected {
                reason: [
                    ToggleReject::Disabled(DisabledReason::ViGEmMissing),
                    ToggleReject::Disabled(DisabledReason::PadNotConnected),
                    ToggleReject::Disabled(DisabledReason::PadError),
                    ToggleReject::Binding,
                ][duvod % 4],
            },
        };
        let b = e.pack();
        prop_assert!(b.is_some_and(|b| b < 1 << 48), "{:?} → {:?}", e, b);
        let b = b.unwrap_or_default();
        prop_assert_eq!(UiEvent::unpack(b), Some(e));
        for bits in [b ^ (1 << bit), syrove, syrove & ((1 << 40) - 1)] {
            if let Some(jine) = UiEvent::unpack(bits) {
                prop_assert_eq!(jine.pack(), Some(bits), "{:#x}", bits);
                prop_assert!(bits == b || jine != e, "dvoje bity, jedna událost");
            }
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

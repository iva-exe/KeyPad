//! Co se na gamepadu stane, když se stiskne namapovaná klávesa — a na
//! kterém z ovladačů.

use serde::{Deserialize, Serialize};

/// Tlačítko Xbox 360 ovladače.
///
/// D-pad je v XInput sada čtyř tlačítek, ne osa — proto je tady, a ne
/// u páček. (Zda má mít i D-pad SOCD jako páčky, je otevřená otázka
/// v ROADMAP.md; teď platí doslovně „tlačítko drží kterákoli klávesa".)
///
/// Guide (Xbox logo) záměrně chybí: výchozí mapování ho nemá a Steam
/// ho zachytává pro vlastní overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PadButton {
    A,
    B,
    X,
    Y,
    #[serde(rename = "LB")]
    Lb,
    #[serde(rename = "RB")]
    Rb,
    /// Stisk levé páčky.
    L3,
    /// Stisk pravé páčky.
    R3,
    Start,
    Back,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
}

impl PadButton {
    /// Všechna tlačítka v pořadí, v jakém je ukazuje GUI.
    pub const ALL: [PadButton; 14] = [
        PadButton::A,
        PadButton::B,
        PadButton::X,
        PadButton::Y,
        PadButton::Lb,
        PadButton::Rb,
        PadButton::L3,
        PadButton::R3,
        PadButton::Start,
        PadButton::Back,
        PadButton::DpadUp,
        PadButton::DpadDown,
        PadButton::DpadLeft,
        PadButton::DpadRight,
    ];

    /// Bit ve `wButtons` struktury `XINPUT_GAMEPAD`. Hodnoty jsou
    /// z XInput.h a ViGEm je bere beze změny, takže [`crate::PadState`]
    /// jde do ovladače bez překládání.
    pub const fn mask(self) -> u16 {
        match self {
            PadButton::DpadUp => 0x0001,
            PadButton::DpadDown => 0x0002,
            PadButton::DpadLeft => 0x0004,
            PadButton::DpadRight => 0x0008,
            PadButton::Start => 0x0010,
            PadButton::Back => 0x0020,
            PadButton::L3 => 0x0040,
            PadButton::R3 => 0x0080,
            PadButton::Lb => 0x0100,
            PadButton::Rb => 0x0200,
            PadButton::A => 0x1000,
            PadButton::B => 0x2000,
            PadButton::X => 0x4000,
            PadButton::Y => 0x8000,
        }
    }
}

/// Směr vychýlení páčky.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum StickDir {
    Up,
    Down,
    Left,
    Right,
}

impl StickDir {
    pub const ALL: [StickDir; 4] = [
        StickDir::Up,
        StickDir::Down,
        StickDir::Left,
        StickDir::Right,
    ];
}

/// Akce, na kterou se klávesa mapuje.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Action {
    Button(PadButton),
    LeftStick(StickDir),
    RightStick(StickDir),
    LeftTrigger,
    RightTrigger,
}

impl Action {
    /// Počet akcí ([`Action::ALL`]).
    pub const COUNT: usize = 24;

    /// Všechny akce v pořadí [`Action::all`] — pořadí bitů [`ActionSet`]
    /// a čísel [`Action::index`]. Okno dostane seznam kódů v tomhle
    /// pořadí a bity podle něj dekóduje, takže pořadí nikde jinde napsané
    /// není a změna se do okna dostane sama.
    pub const ALL: [Action; Action::COUNT] = {
        use PadButton as B;
        use StickDir::{Down, Left, Right, Up};
        [
            Action::LeftStick(Up),
            Action::LeftStick(Down),
            Action::LeftStick(Left),
            Action::LeftStick(Right),
            Action::RightStick(Up),
            Action::RightStick(Down),
            Action::RightStick(Left),
            Action::RightStick(Right),
            Action::Button(B::A),
            Action::Button(B::B),
            Action::Button(B::X),
            Action::Button(B::Y),
            Action::Button(B::Lb),
            Action::Button(B::Rb),
            Action::Button(B::L3),
            Action::Button(B::R3),
            Action::Button(B::Start),
            Action::Button(B::Back),
            Action::Button(B::DpadUp),
            Action::Button(B::DpadDown),
            Action::Button(B::DpadLeft),
            Action::Button(B::DpadRight),
            Action::LeftTrigger,
            Action::RightTrigger,
        ]
    };

    /// Všechny akce v pořadí pro editor mapování: páčky, tlačítka
    /// (včetně D-padu), triggery.
    pub fn all() -> impl Iterator<Item = Action> {
        StickDir::ALL
            .into_iter()
            .map(Action::LeftStick)
            .chain(StickDir::ALL.into_iter().map(Action::RightStick))
            .chain(PadButton::ALL.into_iter().map(Action::Button))
            .chain([Action::LeftTrigger, Action::RightTrigger])
    }

    /// Pozice v [`Action::ALL`] (0 až [`Action::COUNT`] − 1).
    ///
    /// Výčtem, ne `as usize` z pořadí deklarace: přeskládání variant by
    /// jinak potichu přečíslovalo bity, které už okno dekóduje. Shodu
    /// s `ALL` hlídá test.
    pub const fn index(self) -> usize {
        match self {
            Action::LeftStick(d) => stick_index(d),
            Action::RightStick(d) => 4 + stick_index(d),
            Action::Button(b) => 8 + button_index(b),
            Action::LeftTrigger => 22,
            Action::RightTrigger => 23,
        }
    }

    /// Opak [`Action::index`]; mimo rozsah `None`.
    pub fn from_index(i: usize) -> Option<Action> {
        Action::ALL.get(i).copied()
    }

    /// Stabilní kód akce pro konfiguraci a okno (`"ls_up"`, `"a"`, `"lt"`…).
    ///
    /// Kód, ne název varianty přes serde: `config.json` přežije
    /// přejmenování v Rustu a okno (TypeScript) má jednoduchý řetězec
    /// místo vnořeného enumu.
    pub const fn code(self) -> &'static str {
        use PadButton as B;
        use StickDir::{Down, Left, Right, Up};
        match self {
            Action::LeftStick(Up) => "ls_up",
            Action::LeftStick(Down) => "ls_down",
            Action::LeftStick(Left) => "ls_left",
            Action::LeftStick(Right) => "ls_right",
            Action::RightStick(Up) => "rs_up",
            Action::RightStick(Down) => "rs_down",
            Action::RightStick(Left) => "rs_left",
            Action::RightStick(Right) => "rs_right",
            Action::Button(B::A) => "a",
            Action::Button(B::B) => "b",
            Action::Button(B::X) => "x",
            Action::Button(B::Y) => "y",
            Action::Button(B::Lb) => "lb",
            Action::Button(B::Rb) => "rb",
            Action::Button(B::L3) => "l3",
            Action::Button(B::R3) => "r3",
            Action::Button(B::Start) => "start",
            Action::Button(B::Back) => "back",
            Action::Button(B::DpadUp) => "dpad_up",
            Action::Button(B::DpadDown) => "dpad_down",
            Action::Button(B::DpadLeft) => "dpad_left",
            Action::Button(B::DpadRight) => "dpad_right",
            Action::LeftTrigger => "lt",
            Action::RightTrigger => "rt",
        }
    }

    /// Opak [`Action::code`]; neznámý kód (překlep v konfiguraci, jiná
    /// verze okna) je `None`. Velikost písmen se rozlišuje — kód je
    /// identifikátor, ne text pro člověka.
    pub fn from_code(s: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.code() == s)
    }
}

const fn stick_index(d: StickDir) -> usize {
    match d {
        StickDir::Up => 0,
        StickDir::Down => 1,
        StickDir::Left => 2,
        StickDir::Right => 3,
    }
}

const fn button_index(b: PadButton) -> usize {
    match b {
        PadButton::A => 0,
        PadButton::B => 1,
        PadButton::X => 2,
        PadButton::Y => 3,
        PadButton::Lb => 4,
        PadButton::Rb => 5,
        PadButton::L3 => 6,
        PadButton::R3 => 7,
        PadButton::Start => 8,
        PadButton::Back => 9,
        PadButton::DpadUp => 10,
        PadButton::DpadDown => 11,
        PadButton::DpadLeft => 12,
        PadButton::DpadRight => 13,
    }
}

/// Množina akcí jednoho ovladače jako bity podle [`Action::index`].
///
/// Jedno `u32`, ne `HashSet`: vzniká v hook callbacku (živý stav pro
/// okno), který nesmí alokovat (princip 3), a do okna jde přes atomik.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ActionSet(u32);

impl ActionSet {
    /// Bity všech [`Action::COUNT`] akcí.
    const MASK: u32 = (1 << Action::COUNT) - 1;

    pub const EMPTY: ActionSet = ActionSet(0);

    pub fn insert(&mut self, a: Action) {
        self.0 |= 1 << a.index();
    }

    pub const fn contains(self, a: Action) -> bool {
        self.0 & (1 << a.index()) != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Z bitů (např. z atomiku). Bity nad [`Action::COUNT`] se zahodí —
    /// jinak by dvě hodnoty se stejnými akcemi nebyly shodné.
    pub const fn from_bits(bits: u32) -> ActionSet {
        ActionSet(bits & ActionSet::MASK)
    }

    /// Akce v množině v pořadí [`Action::ALL`].
    pub fn iter(self) -> impl Iterator<Item = Action> {
        Action::ALL.into_iter().filter(move |&a| self.contains(a))
    }
}

/// Nejvýš tolik virtuálních ovladačů z jedné klávesnice.
///
/// Strop XInputu: hry přes XInput vidí jen čtyři ovladače (hráč 1–4),
/// pátý by pro ně neexistoval.
pub const MAX_PADS: usize = 4;

/// Který virtuální ovladač (0 až [`MAX_PADS`] − 1).
///
/// Hodnota mimo rozsah nejde vyrobit — ani deserializací (konfigurace,
/// příkaz z okna): pevné tabulky enginu se jím indexují a index mimo
/// rozsah by v hook callbacku znamenal paniku.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct PadId(u8);

impl PadId {
    /// První ovladač — na něm je výchozí mapování.
    pub const FIRST: PadId = PadId(0);

    /// Všechny ovladače vzestupně.
    pub const ALL: [PadId; MAX_PADS] = [PadId(0), PadId(1), PadId(2), PadId(3)];

    pub const fn new(i: usize) -> Option<PadId> {
        if i < MAX_PADS {
            // `i < MAX_PADS ≤ u8::MAX`, přetypování nic neusekne.
            Some(PadId(i as u8))
        } else {
            None
        }
    }

    /// Index do tabulek o [`MAX_PADS`] položkách — vždy menší než
    /// [`MAX_PADS`].
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Číslo ovladače mimo rozsah 0 až [`MAX_PADS`] − 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidPadId(pub u8);

impl std::fmt::Display for InvalidPadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ovladač č. {} neexistuje (nejvýš {} ovladače)",
            self.0, MAX_PADS
        )
    }
}

impl std::error::Error for InvalidPadId {}

impl TryFrom<u8> for PadId {
    type Error = InvalidPadId;

    fn try_from(v: u8) -> Result<PadId, InvalidPadId> {
        PadId::new(usize::from(v)).ok_or(InvalidPadId(v))
    }
}

impl From<PadId> for u8 {
    fn from(p: PadId) -> u8 {
        p.0
    }
}

/// Akce na konkrétním ovladači — to, na co se klávesa mapuje.
///
/// Klávesa patří vždy jen jednomu ovladači: dva hráči na jedné
/// klávesnici si nesmí sdílet klávesu, jinak by jeden stisk hýbal oběma.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PadAction {
    pub pad: PadId,
    pub action: Action,
}

impl PadAction {
    pub const fn new(pad: PadId, action: Action) -> PadAction {
        PadAction { pad, action }
    }

    /// Akce na prvním ovladači.
    pub const fn first(action: Action) -> PadAction {
        PadAction::new(PadId::FIRST, action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::value::{Error as DeError, U8Deserializer};
    use serde::de::IntoDeserializer;

    #[test]
    fn pad_id_jen_v_rozsahu() {
        for (i, p) in PadId::ALL.into_iter().enumerate() {
            assert_eq!(p.index(), i);
            assert_eq!(PadId::new(i), Some(p));
            assert_eq!(PadId::try_from(i as u8), Ok(p));
            assert_eq!(u8::from(p), i as u8);
        }
        assert_eq!(PadId::FIRST, PadId::ALL[0]);
        assert_eq!(PadId::new(MAX_PADS), None);
        assert_eq!(PadId::new(usize::MAX), None);
        assert_eq!(PadId::try_from(4), Err(InvalidPadId(4)));
        assert_eq!(PadId::try_from(u8::MAX), Err(InvalidPadId(u8::MAX)));
    }

    #[test]
    fn deserializace_odmitne_neexistujici_ovladac() {
        // serde_json v core není; deserializátor z holého u8 stačí —
        // jde přes tentýž `try_from` jako JSON z konfigurace.
        let z = |v: u8| {
            let d: U8Deserializer<DeError> = v.into_deserializer();
            PadId::deserialize(d)
        };
        assert_eq!(z(0).unwrap(), PadId::FIRST);
        assert_eq!(z(3).unwrap(), PadId::ALL[3]);
        for v in [4, 5, 200, u8::MAX] {
            assert!(z(v).is_err(), "{v} prošlo");
        }
    }

    #[test]
    fn masky_tlacitek_jsou_ruzne_bity() {
        let mut seen = 0u16;
        for b in PadButton::ALL {
            let m = b.mask();
            assert_eq!(m.count_ones(), 1, "{b:?}");
            assert_eq!(seen & m, 0, "{b:?} sdílí bit");
            seen |= m;
        }
    }

    #[test]
    fn vsech_akci_je_24_a_kazda_jednou() {
        let v: Vec<Action> = Action::all().collect();
        assert_eq!(v.len(), 24);
        assert_eq!(v.len(), Action::COUNT);
        let mut s = v.clone();
        s.sort();
        s.dedup();
        assert_eq!(s.len(), v.len());
        assert_eq!(v, Action::ALL.to_vec(), "ALL v pořadí all()");
    }

    #[test]
    fn index_tam_a_zpet() {
        for (i, a) in Action::ALL.into_iter().enumerate() {
            assert_eq!(a.index(), i, "{a:?}");
            assert_eq!(Action::from_index(i), Some(a));
        }
        assert_eq!(Action::from_index(Action::COUNT), None);
        assert_eq!(Action::from_index(usize::MAX), None);
    }

    #[test]
    fn kody_jsou_stabilni_a_jedinecne() {
        // Kódy jsou v config.json a v okně — změna kteréhokoli rozbije
        // uložené klávesy, proto jsou tu vypsané celé a v pořadí ALL.
        let kody: Vec<&str> = Action::ALL.iter().map(|a| a.code()).collect();
        assert_eq!(
            kody,
            [
                "ls_up",
                "ls_down",
                "ls_left",
                "ls_right",
                "rs_up",
                "rs_down",
                "rs_left",
                "rs_right",
                "a",
                "b",
                "x",
                "y",
                "lb",
                "rb",
                "l3",
                "r3",
                "start",
                "back",
                "dpad_up",
                "dpad_down",
                "dpad_left",
                "dpad_right",
                "lt",
                "rt",
            ]
        );
        let mut s = kody.clone();
        s.sort();
        s.dedup();
        assert_eq!(s.len(), Action::COUNT, "kódy jsou jedinečné");
        for a in Action::ALL {
            assert_eq!(Action::from_code(a.code()), Some(a));
        }
        for neznamy in ["", "LS_UP", "ls-up", "a ", "guide", "dpad", "lt2"] {
            assert_eq!(Action::from_code(neznamy), None, "{neznamy:?}");
        }
    }

    #[test]
    fn mnozina_akci() {
        let mut s = ActionSet::EMPTY;
        assert!(s.is_empty());
        assert_eq!(s, ActionSet::default());
        let a = Action::Button(PadButton::A);
        let rt = Action::RightTrigger;
        s.insert(a);
        s.insert(rt);
        s.insert(a);
        assert!(s.contains(a) && s.contains(rt));
        assert!(!s.contains(Action::LeftTrigger));
        assert_eq!(s.bits(), 1 << 8 | 1 << 23);
        assert_eq!(s.iter().collect::<Vec<_>>(), vec![a, rt]);
        assert_eq!(ActionSet::from_bits(s.bits()), s);
        // Bity nad 24 akcemi se zahodí.
        assert_eq!(ActionSet::from_bits(u32::MAX).bits(), (1 << 24) - 1);
        assert_eq!(ActionSet::from_bits(1 << 24), ActionSet::EMPTY);
        assert_eq!(ActionSet::from_bits(u32::MAX).iter().count(), 24);
    }
}

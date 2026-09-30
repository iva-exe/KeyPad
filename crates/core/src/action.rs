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
    /// Všechny akce v pořadí pro editor mapování: páčky, D-pad,
    /// tlačítka, triggery.
    pub fn all() -> impl Iterator<Item = Action> {
        StickDir::ALL
            .into_iter()
            .map(Action::LeftStick)
            .chain(StickDir::ALL.into_iter().map(Action::RightStick))
            .chain(PadButton::ALL.into_iter().map(Action::Button))
            .chain([Action::LeftTrigger, Action::RightTrigger])
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
        let mut s = v.clone();
        s.sort();
        s.dedup();
        assert_eq!(s.len(), v.len());
    }
}

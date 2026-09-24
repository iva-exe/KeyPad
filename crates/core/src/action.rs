//! Co se na gamepadu stane, když se stiskne namapovaná klávesa.

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

#[cfg(test)]
mod tests {
    use super::*;

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

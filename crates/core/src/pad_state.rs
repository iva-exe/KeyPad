//! Stav virtuálního gamepadu a jeho výpočet z držených kláves.
//!
//! Princip 7: stav se VŽDY přepočítává celý z množiny držených kláves
//! (čistá funkce), nikdy se inkrementálně nepřičítá ani neodečítá.
//! Inkrementální stav se při první ztracené nebo zdvojené události
//! rozjede a páčka zůstane vychýlená; přepočet z množiny se sám srovná
//! s každou další událostí.

use serde::{Deserialize, Serialize};

use crate::action::{Action, PadButton, StickDir};

/// Plná výchylka na ose.
pub const AXIS_MAX: i16 = 32_767;

/// Výchylka každé osy na diagonále: 32767 / √2 = 23169,8 → 23170.
///
/// Bez toho by W+D dalo (32767, 32767) — vektor délky √2, tedy o 41 %
/// rychlejší pohyb šikmo než rovně. S 23170 je délka 32767,3, prakticky
/// přesně 1.
pub const AXIS_DIAGONAL: i16 = 23_170;

/// Plně stisknutý trigger.
pub const TRIGGER_MAX: u8 = 255;

/// Stav Xbox 360 ovladače — zrcadlí `XINPUT_GAMEPAD`, takže jde do ViGEm
/// bez překládání.
///
/// Záporné výchylky jsou vždy `-32767`, nikdy `i16::MIN` (-32768):
/// hodnoty se nikde nenegují, znaménko se skládá z kladné konstanty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct PadState {
    /// Bitová maska tlačítek ([`PadButton::mask`]).
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub thumb_lx: i16,
    /// Kladné = nahoru.
    pub thumb_ly: i16,
    pub thumb_rx: i16,
    /// Kladné = nahoru.
    pub thumb_ry: i16,
}

impl PadState {
    /// Nic nestisknuto, páčky uprostřed.
    pub const NEUTRAL: PadState = PadState {
        buttons: 0,
        left_trigger: 0,
        right_trigger: 0,
        thumb_lx: 0,
        thumb_ly: 0,
        thumb_rx: 0,
        thumb_ry: 0,
    };

    pub fn is_neutral(&self) -> bool {
        *self == PadState::NEUTRAL
    }

    pub fn is_pressed(&self, button: PadButton) -> bool {
        self.buttons & button.mask() != 0
    }
}

/// Nejnovější stisk každého ze čtyř směrů jedné páčky.
#[derive(Default)]
struct StickPresses {
    up: Option<u64>,
    down: Option<u64>,
    left: Option<u64>,
    right: Option<u64>,
}

impl StickPresses {
    fn press(&mut self, dir: StickDir, seq: u64) {
        let slot = match dir {
            StickDir::Up => &mut self.up,
            StickDir::Down => &mut self.down,
            StickDir::Left => &mut self.left,
            StickDir::Right => &mut self.right,
        };
        // Drží-li směr víc kláves, platí nejnovější z nich.
        *slot = Some(slot.map_or(seq, |s| s.max(seq)));
    }

    /// Výchylka páčky jako (x, y).
    fn value(&self) -> (i16, i16) {
        let x = socd(self.left, self.right);
        // **V XInput je kladné Y NAHORU** (obráceně než souřadnice
        // obrazovky): W = Up = +Y.
        let y = socd(self.down, self.up);
        let mag = if x != 0 && y != 0 {
            AXIS_DIAGONAL
        } else {
            AXIS_MAX
        };
        (scale(x, mag), scale(y, mag))
    }
}

/// SOCD na jedné ose: vyhrává NAPOSLEDY stisknutý směr (vyšší `seq`).
///
/// A+D současně = D, pokud přišlo později; po uvolnění D se osa vrátí
/// k A, protože A je pořád v množině držených. Shodné `seq` (engine je
/// nevydá, ale funkce je veřejná) = neutrál, ať výsledek nezávisí na
/// pořadí vstupu.
fn socd(negative: Option<u64>, positive: Option<u64>) -> i8 {
    match (negative, positive) {
        (None, None) => 0,
        (Some(_), None) => -1,
        (None, Some(_)) => 1,
        (Some(n), Some(p)) if p > n => 1,
        (Some(n), Some(p)) if n > p => -1,
        _ => 0,
    }
}

/// -1/0/1 × kladná velikost. Záporná hodnota vzniká `-mag` z kladného
/// `mag ≤ 32767`, takže `i16::MIN` nevznikne a nic nepřeteče.
fn scale(dir: i8, mag: i16) -> i16 {
    debug_assert!(mag > 0);
    match dir {
        1 => mag,
        -1 => -mag,
        _ => 0,
    }
}

/// Spočítá stav gamepadu z kláves, které právě drží pad.
///
/// Vstup: akce každé držené klávesy s vlastníkem Pad a pořadí jejího
/// stisku (`seq`). Víc kláves na jednu akci je v pořádku:
/// - tlačítko je stisknuté, drží-li ho **kterákoli** jeho klávesa;
/// - trigger je 255, drží-li ho kterákoli klávesa, jinak 0;
/// - páčka: na každé ose vyhrává naposledy stisknutý směr (SOCD),
///   výsledek je (x, y) ∈ {-1, 0, 1}², osa = 32767, diagonála = 23170.
///
/// Pořadí vstupu na výsledek nemá vliv.
pub fn compute_pad_state<I>(pressed: I) -> PadState
where
    I: IntoIterator<Item = (Action, u64)>,
{
    let mut state = PadState::NEUTRAL;
    let mut left = StickPresses::default();
    let mut right = StickPresses::default();

    for (action, seq) in pressed {
        match action {
            Action::Button(b) => state.buttons |= b.mask(),
            Action::LeftTrigger => state.left_trigger = TRIGGER_MAX,
            Action::RightTrigger => state.right_trigger = TRIGGER_MAX,
            Action::LeftStick(dir) => left.press(dir, seq),
            Action::RightStick(dir) => right.press(dir, seq),
        }
    }

    (state.thumb_lx, state.thumb_ly) = left.value();
    (state.thumb_rx, state.thumb_ry) = right.value();
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::StickDir::*;

    fn ls(d: StickDir) -> Action {
        Action::LeftStick(d)
    }

    #[test]
    fn nic_drzeno_je_neutral() {
        assert!(compute_pad_state([]).is_neutral());
    }

    #[test]
    fn w_samotne_je_plne_nahoru() {
        let s = compute_pad_state([(ls(Up), 1)]);
        assert_eq!((s.thumb_lx, s.thumb_ly), (0, 32_767));
    }

    #[test]
    fn diagonala_w_d() {
        let s = compute_pad_state([(ls(Up), 1), (ls(Right), 2)]);
        assert_eq!((s.thumb_lx, s.thumb_ly), (23_170, 23_170));
    }

    #[test]
    fn diagonala_ma_delku_jedna() {
        let d = f64::from(AXIS_DIAGONAL);
        let len = (d * d * 2.0).sqrt();
        assert!((len - f64::from(AXIS_MAX)).abs() < 1.0, "délka {len}");
    }

    #[test]
    fn zaporne_smery_nikdy_i16_min() {
        let s = compute_pad_state([(ls(Down), 1), (ls(Left), 2)]);
        assert_eq!((s.thumb_lx, s.thumb_ly), (-23_170, -23_170));
        let s = compute_pad_state([(ls(Down), 1)]);
        assert_eq!(s.thumb_ly, -32_767);
        let s = compute_pad_state([(Action::RightStick(Left), 1)]);
        assert_eq!(s.thumb_rx, -32_767);
    }

    #[test]
    fn socd_vyhrava_posledni() {
        // A (seq 1) → D (seq 2): vpravo.
        let s = compute_pad_state([(ls(Left), 1), (ls(Right), 2)]);
        assert_eq!(s.thumb_lx, 32_767);
        // Opačné pořadí stisků: vlevo.
        let s = compute_pad_state([(ls(Right), 1), (ls(Left), 2)]);
        assert_eq!(s.thumb_lx, -32_767);
        // Pořadí ve vstupu nehraje roli, jen seq.
        let s = compute_pad_state([(ls(Right), 2), (ls(Left), 1)]);
        assert_eq!(s.thumb_lx, 32_767);
    }

    #[test]
    fn socd_na_ose_y_a_diagonala_zustava() {
        // W, pak S, k tomu D: dolů + vpravo = diagonála dolů.
        let s = compute_pad_state([(ls(Up), 1), (ls(Down), 2), (ls(Right), 3)]);
        assert_eq!((s.thumb_lx, s.thumb_ly), (23_170, -23_170));
    }

    #[test]
    fn smer_drzeny_dvema_klavesami_bere_novejsi_stisk() {
        // Vlevo drží klávesy se seq 1 a 3, vpravo seq 2 → vlevo (3 > 2).
        let s = compute_pad_state([(ls(Left), 1), (ls(Right), 2), (ls(Left), 3)]);
        assert_eq!(s.thumb_lx, -32_767);
    }

    #[test]
    fn shodne_seq_je_neutral() {
        let s = compute_pad_state([(ls(Left), 5), (ls(Right), 5)]);
        assert_eq!(s.thumb_lx, 0);
    }

    #[test]
    fn tlacitka_a_triggery() {
        let s = compute_pad_state([
            (Action::Button(PadButton::A), 1),
            (Action::Button(PadButton::DpadLeft), 2),
            (Action::LeftTrigger, 3),
        ]);
        assert!(s.is_pressed(PadButton::A));
        assert!(s.is_pressed(PadButton::DpadLeft));
        assert!(!s.is_pressed(PadButton::B));
        assert_eq!(s.buttons, 0x1000 | 0x0004);
        assert_eq!((s.left_trigger, s.right_trigger), (255, 0));
    }

    #[test]
    fn prava_packa_nezavisi_na_leve() {
        let s = compute_pad_state([(ls(Up), 1), (Action::RightStick(Left), 2)]);
        assert_eq!((s.thumb_lx, s.thumb_ly), (0, 32_767));
        assert_eq!((s.thumb_rx, s.thumb_ry), (-32_767, 0));
    }
}

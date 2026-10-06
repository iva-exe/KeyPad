//! Stav virtuálního gamepadu a jeho výpočet z držených kláves.
//!
//! Při víc ovladačích se počítá každý zvlášť, jen z kláves, které patří
//! jemu ([`PadUpdates`] pak nese ty, které se mají poslat).
//!
//! Princip 7: stav se VŽDY přepočítává celý z množiny držených kláves
//! (čistá funkce), nikdy se inkrementálně nepřičítá ani neodečítá.
//! Inkrementální stav se při první ztracené nebo zdvojené události
//! rozjede a páčka zůstane vychýlená; přepočet z množiny se sám srovná
//! s každou další událostí.

use serde::{Deserialize, Serialize};

use crate::action::{Action, ActionSet, PadAction, PadButton, PadId, StickDir, MAX_PADS};

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

    /// Vstupy, které hra v tomhle stavu opravdu dostává: stisknutá
    /// tlačítka, trigger > 0 a směr každé vychýlené osy podle znaménka.
    ///
    /// Okno podle toho svítí čepičky „naplno" — ze stavu, který jde do
    /// ViGEm, ne z držených kláves: u A+D hra dostává jen vítěze SOCD
    /// a poražený směr má svítit jen obrysem.
    pub fn active_inputs(&self) -> ActionSet {
        let mut s = ActionSet::EMPTY;
        for b in PadButton::ALL {
            if self.is_pressed(b) {
                s.insert(Action::Button(b));
            }
        }
        if self.left_trigger > 0 {
            s.insert(Action::LeftTrigger);
        }
        if self.right_trigger > 0 {
            s.insert(Action::RightTrigger);
        }
        let osy = [
            (
                self.thumb_lx,
                Action::LeftStick(StickDir::Left),
                Action::LeftStick(StickDir::Right),
            ),
            (
                self.thumb_ly,
                Action::LeftStick(StickDir::Down),
                Action::LeftStick(StickDir::Up),
            ),
            (
                self.thumb_rx,
                Action::RightStick(StickDir::Left),
                Action::RightStick(StickDir::Right),
            ),
            (
                self.thumb_ry,
                Action::RightStick(StickDir::Down),
                Action::RightStick(StickDir::Up),
            ),
        ];
        for (v, zaporny, kladny) in osy {
            if v > 0 {
                s.insert(kladny);
            } else if v < 0 {
                s.insert(zaporny);
            }
        }
        s
    }
}

/// Fyzicky držené vstupy jednoho ovladače pro okno (živá detekce).
///
/// Bity 0–23 = [`ActionSet`] držených vstupů (i poražený směr SOCD);
/// 24–25 LX, 26–27 LY, 28–29 RX, 30–31 RY jako znaménko výchylky po SOCD
/// (00 = 0, 01 = +1, 10 = −1; kladné Y = nahoru jako v XInputu).
///
/// Jedno `u32`: počítá se v hook callbacku a do okna jde přes atomik
/// (princip 3) — žádná alokace, žádný zámek.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LiveInputs(u32);

impl LiveInputs {
    pub const EMPTY: LiveInputs = LiveInputs(0);

    /// Držené vstupy a výchylky páček (každá složka −1, 0 nebo 1; jiná
    /// hodnota se bere podle znaménka).
    pub fn new(held: ActionSet, left: (i8, i8), right: (i8, i8)) -> LiveInputs {
        LiveInputs(
            held.bits()
                | axis_bits(left.0) << 24
                | axis_bits(left.1) << 26
                | axis_bits(right.0) << 28
                | axis_bits(right.1) << 30,
        )
    }

    /// Fyzicky držené vstupy (včetně poraženého směru SOCD).
    pub fn held(self) -> ActionSet {
        ActionSet::from_bits(self.0)
    }

    /// Levá páčka po SOCD jako (x, y) ∈ {−1, 0, 1}², kladné y = nahoru.
    pub fn left_stick(self) -> (i8, i8) {
        (self.axis(24), self.axis(26))
    }

    /// Pravá páčka, stejně jako [`LiveInputs::left_stick`].
    pub fn right_stick(self) -> (i8, i8) {
        (self.axis(28), self.axis(30))
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Z bitů (např. z atomiku). Neplatná dvojice bitů osy (11) se bere
    /// jako 0 a zahodí — stejný význam má pak i stejnou hodnotu.
    pub fn from_bits(bits: u32) -> LiveInputs {
        let mut b = bits;
        for shift in [24, 26, 28, 30] {
            if (b >> shift) & 0b11 == 0b11 {
                b &= !(0b11 << shift);
            }
        }
        LiveInputs(b)
    }

    fn axis(self, shift: u32) -> i8 {
        match (self.0 >> shift) & 0b11 {
            0b01 => 1,
            0b10 => -1,
            _ => 0,
        }
    }
}

fn axis_bits(v: i8) -> u32 {
    match v.signum() {
        1 => 0b01,
        -1 => 0b10,
        _ => 0,
    }
}

/// Nové stavy ovladačů k odeslání do ViGEm — jen ty, které se mají
/// poslat.
///
/// Pevné pole + bitová maska, ne `Vec` ani mapa: vzniká v hook
/// callbacku, který nesmí alokovat (princip 3). Ovladač bez nastaveného
/// bitu se neposílá (beze změny); jeho slot v poli je vždy neutrální,
/// ať porovnání dvou hodnot nezávisí na tom, co v nepoužitém slotu zbylo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PadUpdates {
    /// Bit `1 << pad.index()` = stav toho ovladače je platný.
    changed: u8,
    states: [PadState; MAX_PADS],
}

impl PadUpdates {
    /// Nic se neposílá.
    pub const NONE: PadUpdates = PadUpdates {
        changed: 0,
        states: [PadState::NEUTRAL; MAX_PADS],
    };

    /// Stav ovladače k odeslání; `None` = beze změny.
    pub fn get(&self, pad: PadId) -> Option<PadState> {
        let i = pad.index();
        (self.changed & (1 << i) != 0).then(|| self.states[i])
    }

    pub fn set(&mut self, pad: PadId, s: PadState) {
        let i = pad.index();
        self.changed |= 1 << i;
        self.states[i] = s;
    }

    pub fn is_empty(&self) -> bool {
        self.changed == 0
    }

    /// Nastavené stavy vzestupně podle ovladače.
    pub fn iter(&self) -> impl Iterator<Item = (PadId, PadState)> + '_ {
        PadId::ALL
            .into_iter()
            .filter_map(|p| self.get(p).map(|s| (p, s)))
    }

    /// Sloučí s dřívějším krokem téhož volání: u každého ovladače platí
    /// novější stav, když ho tenhle krok nastavil, jinak ten dřívější.
    pub(crate) fn or(self, earlier: PadUpdates) -> PadUpdates {
        let mut out = earlier;
        for (p, s) in self.iter() {
            out.set(p, s);
        }
        out
    }
}

/// Nejnovější stisk každého ze čtyř směrů jedné páčky.
#[derive(Clone, Copy, Default)]
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

    /// Směr páčky po SOCD jako (x, y) ∈ {−1, 0, 1}².
    fn signs(&self) -> (i8, i8) {
        // **V XInput je kladné Y NAHORU** (obráceně než souřadnice
        // obrazovky): W = Up = +Y.
        (socd(self.left, self.right), socd(self.down, self.up))
    }

    /// Výchylka páčky jako (x, y).
    fn value(&self) -> (i16, i16) {
        let (x, y) = self.signs();
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

/// Živý stav všech ovladačů z držených kláves: cíl každé klávesy a pořadí
/// jejího stisku. Jeden průchod a SOCD stejně jako [`compute_pad_state`]
/// (hlavička páčky v okně ukazuje totéž, co by dostala hra).
///
/// Pevná pole, nic nealokuje — volá se z hook callbacku.
pub(crate) fn live_from(it: impl Iterator<Item = (PadAction, u64)>) -> [LiveInputs; MAX_PADS] {
    let mut held = [ActionSet::EMPTY; MAX_PADS];
    let mut left = [StickPresses::default(); MAX_PADS];
    let mut right = [StickPresses::default(); MAX_PADS];
    for (t, seq) in it {
        let p = t.pad.index();
        held[p].insert(t.action);
        match t.action {
            Action::LeftStick(dir) => left[p].press(dir, seq),
            Action::RightStick(dir) => right[p].press(dir, seq),
            Action::Button(_) | Action::LeftTrigger | Action::RightTrigger => {}
        }
    }
    let mut out = [LiveInputs::EMPTY; MAX_PADS];
    for (p, o) in out.iter_mut().enumerate() {
        *o = LiveInputs::new(held[p], left[p].signs(), right[p].signs());
    }
    out
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

    fn stav_a() -> PadState {
        compute_pad_state([(Action::Button(PadButton::A), 1)])
    }

    #[test]
    fn pad_updates_nese_jen_nastavene_ovladace() {
        let mut u = PadUpdates::NONE;
        assert!(u.is_empty());
        assert_eq!(u, PadUpdates::default());
        assert_eq!(u.iter().count(), 0);

        // Neutrál je platný stav k odeslání, ne „nic".
        u.set(PadId::ALL[2], PadState::NEUTRAL);
        u.set(PadId::FIRST, stav_a());
        assert!(!u.is_empty());
        assert_eq!(u.get(PadId::FIRST), Some(stav_a()));
        assert_eq!(u.get(PadId::ALL[1]), None);
        assert_eq!(u.get(PadId::ALL[2]), Some(PadState::NEUTRAL));
        assert_eq!(u.get(PadId::ALL[3]), None);
        assert_eq!(
            u.iter().collect::<Vec<_>>(),
            vec![(PadId::FIRST, stav_a()), (PadId::ALL[2], PadState::NEUTRAL)],
            "vzestupně podle ovladače"
        );
    }

    #[test]
    fn aktivni_vstupy_neutralu_jsou_prazdne() {
        assert_eq!(PadState::NEUTRAL.active_inputs(), ActionSet::EMPTY);
    }

    #[test]
    fn aktivni_vstupy_tlacitka_triggery_a_osy() {
        // Každé tlačítko samo.
        for b in PadButton::ALL {
            let s = compute_pad_state([(Action::Button(b), 1)]);
            assert_eq!(
                s.active_inputs().iter().collect::<Vec<_>>(),
                vec![Action::Button(b)],
                "{b:?}"
            );
        }
        // Všechna tlačítka a oba triggery najednou.
        let vse = PadState {
            buttons: PadButton::ALL.iter().fold(0, |m, b| m | b.mask()),
            left_trigger: 1,
            right_trigger: TRIGGER_MAX,
            ..PadState::NEUTRAL
        };
        let a = vse.active_inputs();
        for b in PadButton::ALL {
            assert!(a.contains(Action::Button(b)), "{b:?}");
        }
        assert!(a.contains(Action::LeftTrigger) && a.contains(Action::RightTrigger));
        assert_eq!(a.iter().count(), 16);
        // Každý směr obou páček podle znaménka osy (i diagonála a i malá
        // výchylka), protilehlý směr ne.
        for (dir, opak) in [(Up, Down), (Down, Up), (Left, Right), (Right, Left)] {
            for (packa, f) in [
                (true, Action::LeftStick as fn(StickDir) -> Action),
                (false, Action::RightStick as fn(StickDir) -> Action),
            ] {
                let s = compute_pad_state([(f(dir), 1)]);
                let a = s.active_inputs();
                assert_eq!(
                    a.iter().collect::<Vec<_>>(),
                    vec![f(dir)],
                    "{dir:?} {packa}"
                );
                assert!(!a.contains(f(opak)));
            }
        }
        let diag = compute_pad_state([(ls(Down), 1), (ls(Left), 2)]).active_inputs();
        assert_eq!(diag.iter().collect::<Vec<_>>(), vec![ls(Down), ls(Left)]);
        let maly = PadState {
            thumb_rx: -1,
            thumb_ly: 1,
            ..PadState::NEUTRAL
        };
        assert_eq!(
            maly.active_inputs().iter().collect::<Vec<_>>(),
            vec![ls(Up), Action::RightStick(Left)]
        );
    }

    #[test]
    fn aktivni_vstupy_jen_vitez_socd() {
        // A pak D: hra dostává jen vpravo.
        let s = compute_pad_state([(ls(Left), 1), (ls(Right), 2)]);
        assert_eq!(
            s.active_inputs().iter().collect::<Vec<_>>(),
            vec![ls(Right)]
        );
    }

    #[test]
    fn zivy_stav_bity() {
        let mut held = ActionSet::EMPTY;
        held.insert(ls(Left));
        held.insert(ls(Right));
        held.insert(Action::Button(PadButton::A));
        let z = LiveInputs::new(held, (1, -1), (-1, 0));
        assert_eq!(z.held(), held);
        assert_eq!(z.left_stick(), (1, -1));
        assert_eq!(z.right_stick(), (-1, 0));
        assert_eq!(
            z.bits(),
            held.bits() | 0b01 << 24 | 0b10 << 26 | 0b10 << 28,
            "rozložení bitů je smlouva s oknem"
        );
        assert_eq!(LiveInputs::from_bits(z.bits()), z);
        assert_eq!(
            LiveInputs::new(ActionSet::EMPTY, (0, 0), (0, 0)),
            LiveInputs::EMPTY
        );
        assert_eq!(LiveInputs::default(), LiveInputs::EMPTY);
        // Velikost složky nehraje roli, jen znaménko.
        assert_eq!(
            LiveInputs::new(ActionSet::EMPTY, (5, -128), (127, 0)),
            LiveInputs::new(ActionSet::EMPTY, (1, -1), (1, 0))
        );
        // Neplatná dvojice 11 je 0 a hodnota se srovná.
        let spatne = LiveInputs::from_bits(0b11 << 24 | 0b01 << 26);
        assert_eq!(spatne.left_stick(), (0, 1));
        assert_eq!(spatne, LiveInputs::new(ActionSet::EMPTY, (0, 1), (0, 0)));
        for b in [0, u32::MAX, 0x5555_5555, 0xAAAA_AAAA] {
            let z = LiveInputs::from_bits(b);
            assert_eq!(LiveInputs::from_bits(z.bits()), z, "{b:#x}");
            assert_eq!(
                LiveInputs::new(z.held(), z.left_stick(), z.right_stick()),
                z
            );
        }
    }

    #[test]
    fn zivy_stav_z_drzenych_klaves() {
        let p0 = |a| PadAction::new(PadId::FIRST, a);
        let p1 = |a| PadAction::new(PadId::ALL[1], a);
        // A (seq 1) a D (seq 2) na prvním: drží oba, páčka vpravo. Na
        // druhém W a pravá páčka dolů a tlačítko — každý ovladač zvlášť.
        let z = live_from(
            [
                (p0(ls(Left)), 1),
                (p1(ls(Up)), 3),
                (p0(ls(Right)), 2),
                (p1(Action::RightStick(Down)), 4),
                (p1(Action::Button(PadButton::Y)), 5),
                (p0(Action::LeftTrigger), 6),
            ]
            .into_iter(),
        );
        assert_eq!(
            z[0].held().iter().collect::<Vec<_>>(),
            vec![ls(Left), ls(Right), Action::LeftTrigger]
        );
        assert_eq!(z[0].left_stick(), (1, 0), "vyhrává D");
        assert_eq!(z[0].right_stick(), (0, 0));
        assert_eq!(
            z[1].held().iter().collect::<Vec<_>>(),
            vec![
                ls(Up),
                Action::RightStick(Down),
                Action::Button(PadButton::Y)
            ]
        );
        assert_eq!(z[1].left_stick(), (0, 1));
        assert_eq!(z[1].right_stick(), (0, -1));
        assert_eq!(z[2], LiveInputs::EMPTY);
        assert_eq!(z[3], LiveInputs::EMPTY);
        // Opačné pořadí stisků: vlevo. Pořadí ve vstupu nehraje roli.
        let z = live_from([(p0(ls(Right)), 2), (p0(ls(Left)), 7)].into_iter());
        assert_eq!(z[0].left_stick(), (-1, 0));
        assert_eq!(live_from(std::iter::empty()), [LiveInputs::EMPTY; MAX_PADS]);
    }

    #[test]
    fn zivy_stav_souhlasi_se_stavem_padu() {
        // Hlavička páčky v okně ukazuje totéž co hra: znaménka os živého
        // stavu = znaménka compute_pad_state ze stejných kláves.
        let drzene = [
            (ls(Up), 4),
            (ls(Down), 9),
            (ls(Right), 2),
            (Action::RightStick(Left), 5),
            (Action::RightStick(Right), 5),
        ];
        let s = compute_pad_state(drzene);
        let z = live_from(drzene.into_iter().map(|(a, q)| (PadAction::first(a), q)));
        let sg = |v: i16| v.signum() as i8;
        assert_eq!(z[0].left_stick(), (sg(s.thumb_lx), sg(s.thumb_ly)));
        assert_eq!(z[0].right_stick(), (sg(s.thumb_rx), sg(s.thumb_ry)));
    }

    #[test]
    fn pad_updates_slouceni_bere_novejsi() {
        let mut drivejsi = PadUpdates::NONE;
        drivejsi.set(PadId::FIRST, stav_a());
        drivejsi.set(PadId::ALL[1], stav_a());
        let mut novejsi = PadUpdates::NONE;
        novejsi.set(PadId::FIRST, PadState::NEUTRAL);
        novejsi.set(PadId::ALL[3], stav_a());

        let s = novejsi.or(drivejsi);
        assert_eq!(
            s.get(PadId::FIRST),
            Some(PadState::NEUTRAL),
            "novější vyhrává"
        );
        assert_eq!(s.get(PadId::ALL[1]), Some(stav_a()), "z dřívějšího");
        assert_eq!(s.get(PadId::ALL[2]), None);
        assert_eq!(s.get(PadId::ALL[3]), Some(stav_a()));
        assert_eq!(PadUpdates::NONE.or(PadUpdates::NONE), PadUpdates::NONE);
        assert_eq!(PadUpdates::NONE.or(drivejsi), drivejsi);
        assert_eq!(drivejsi.or(PadUpdates::NONE), drivejsi);
    }
}

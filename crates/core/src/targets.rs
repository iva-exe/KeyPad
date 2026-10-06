//! Množina cílů jedné klávesy (Fáze 7, sdílené klávesy).
//!
//! Klávesa smí ovládat nejvýš [`MAX_TARGETS_PER_KEY`] vstupů — téhož
//! i různých ovladačů. Stisk takové klávesy spustí všechny její vstupy
//! na připravených ovladačích najednou.

use serde::{Deserialize, Serialize};

use crate::action::{Action, ActionSet, PadAction, PadId, MAX_PADS};

/// Nejvýš tolik vstupů smí ovládat jedna klávesa.
///
/// Pevný strop: pátý vstup by bublina čepičky už nevypsala čitelně,
/// vlastník držené klávesy zůstává malá hodnota bez alokace a počty
/// v oznámení o uložení mají po 2 bitech (schránka hooku má jen 48 bitů,
/// OQ 64). Strop hlídá [`Targets::insert`]; bez volby „Jedna klávesa pro
/// víc vstupů" se klávesa přesouvá a strop se neuplatní.
pub const MAX_TARGETS_PER_KEY: usize = 4;

/// Cíle jedné klávesy: nejvýš [`MAX_TARGETS_PER_KEY`] vstupů ovladačů.
///
/// Bitová množina (bit `Action::index` v ovladači `PadId::index`), ne
/// seznam: je **vždy kanonická** — tatáž množina má jedinou hodnotu, ať
/// se vkládala v jakémkoli pořadí. Rovnost hodnot je tak rovnost množin,
/// a tatáž množina dá stejnou revizi mapování („Zpět" se trefí), stejný
/// text v `config.json` i stejného vlastníka držené klávesy. Procházení
/// jde v pořadí ovladač, pak [`Action::index`] („první" cíl je nejmenší
/// v tomhle pořadí).
///
/// `Copy` a pevná velikost: je to vlastník držené klávesy
/// (`Owner::Pad`) i řádek tabulky mapování — v hook callbacku se s ní
/// nic nealokuje (princip 3).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Targets([ActionSet; MAX_PADS]);

/// Klávesa by ovládala víc než [`MAX_TARGETS_PER_KEY`] vstupů.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooManyTargets;

impl std::fmt::Display for TooManyTargets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "klávesa smí ovládat nejvýš {MAX_TARGETS_PER_KEY} vstupy")
    }
}

impl std::error::Error for TooManyTargets {}

impl Targets {
    /// Žádný cíl.
    pub const EMPTY: Targets = Targets([ActionSet::EMPTY; MAX_PADS]);

    /// Jediný cíl.
    pub fn one(t: PadAction) -> Targets {
        let mut s = Targets::EMPTY;
        s.0[t.pad.index()].insert(t.action);
        s
    }

    /// Počet cílů (0 až [`MAX_TARGETS_PER_KEY`]).
    pub fn len(self) -> usize {
        self.0.iter().map(|a| a.bits().count_ones() as usize).sum()
    }

    pub fn is_empty(self) -> bool {
        self == Targets::EMPTY
    }

    pub fn contains(self, t: PadAction) -> bool {
        self.0[t.pad.index()].contains(t.action)
    }

    /// Přidá cíl. `true` = cíl v množině je (byl, nebo přibyl); `false` =
    /// množina je plná a cíl v ní není — nic se nezmění.
    pub fn insert(&mut self, t: PadAction) -> bool {
        if self.contains(t) {
            return true;
        }
        if self.len() >= MAX_TARGETS_PER_KEY {
            return false;
        }
        self.0[t.pad.index()].insert(t.action);
        true
    }

    /// Odebere cíl. `true` = byl v množině.
    pub fn remove(&mut self, t: PadAction) -> bool {
        let p = &mut self.0[t.pad.index()];
        if !p.contains(t.action) {
            return false;
        }
        *p = ActionSet::from_bits(p.bits() & !(1 << t.action.index()));
        true
    }

    /// Odebere všechny cíle ovladače; vrací, kolik jich bylo.
    pub fn remove_pad(&mut self, pad: PadId) -> usize {
        let n = self.0[pad.index()].bits().count_ones() as usize;
        self.0[pad.index()] = ActionSet::EMPTY;
        n
    }

    /// Jen cíle ovladačů, pro které `keep[ovladač]` platí (připravené
    /// ovladače při stisku).
    pub fn only_pads(self, keep: &[bool; MAX_PADS]) -> Targets {
        let mut s = self;
        for (p, k) in s.0.iter_mut().zip(keep) {
            if !k {
                *p = ActionSet::EMPTY;
            }
        }
        s
    }

    /// Má cíl na ovladači?
    pub fn has_pad(self, pad: PadId) -> bool {
        !self.0[pad.index()].is_empty()
    }

    /// Vstupy jednoho ovladače.
    pub fn actions(self, pad: PadId) -> ActionSet {
        self.0[pad.index()]
    }

    /// Cíle v kanonickém pořadí: ovladač, pak [`Action::index`].
    pub fn iter(self) -> TargetsIter {
        TargetsIter {
            bits: self.0.map(ActionSet::bits),
            pad: 0,
        }
    }

    /// Nejmenší cíl v kanonickém pořadí.
    pub fn first(self) -> Option<PadAction> {
        self.iter().next()
    }

    /// Cíle, které jsou v `self` a nejsou v `other`.
    pub fn minus(self, other: Targets) -> Targets {
        let mut s = self;
        for (p, o) in s.0.iter_mut().zip(other.0) {
            *p = ActionSet::from_bits(p.bits() & !o.bits());
        }
        s
    }

    /// Množina z cílů (kanonická, duplicity se sloučí); víc než
    /// [`MAX_TARGETS_PER_KEY`] různých cílů je chyba.
    pub fn from_targets(
        it: impl IntoIterator<Item = PadAction>,
    ) -> Result<Targets, TooManyTargets> {
        let mut s = Targets::EMPTY;
        for t in it {
            if !s.insert(t) {
                return Err(TooManyTargets);
            }
        }
        Ok(s)
    }
}

/// Procházení [`Targets`] po nastavených bitech — vlastní iterátor, ne
/// řetěz `flat_map`: stav ovladačů i živý stav ho volají v hook callbacku
/// pro každou drženou klávesu a prázdná množina má skončit hned.
#[derive(Clone, Debug)]
pub struct TargetsIter {
    bits: [u32; MAX_PADS],
    pad: usize,
}

impl Iterator for TargetsIter {
    type Item = PadAction;

    fn next(&mut self) -> Option<PadAction> {
        while let Some(b) = self.bits.get_mut(self.pad) {
            if *b != 0 {
                let i = b.trailing_zeros() as usize;
                *b &= *b - 1;
                let pad = PadId::new(self.pad)?;
                return Some(PadAction::new(pad, Action::from_index(i)?));
            }
            self.pad += 1;
        }
        None
    }
}

impl From<PadAction> for Targets {
    fn from(t: PadAction) -> Targets {
        Targets::one(t)
    }
}

impl std::fmt::Debug for Targets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

// Serde jako seznam cílů (kanonické pořadí): bitová podoba je vnitřní
// věc jádra, která se může změnit, seznam ne.
impl Serialize for Targets {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.iter())
    }
}

impl<'de> Deserialize<'de> for Targets {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Targets, D::Error> {
        let v = Vec::<PadAction>::deserialize(d)?;
        Targets::from_targets(v).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{PadButton, StickDir};

    fn pa(p: usize, a: Action) -> PadAction {
        PadAction::new(PadId::ALL[p], a)
    }

    const A: Action = Action::Button(PadButton::A);
    const UP: Action = Action::LeftStick(StickDir::Up);
    const RT: Action = Action::RightTrigger;

    #[test]
    fn prazdna_a_jeden_cil() {
        assert!(Targets::EMPTY.is_empty());
        assert_eq!(Targets::EMPTY.len(), 0);
        assert_eq!(Targets::default(), Targets::EMPTY);
        let s = Targets::one(pa(2, A));
        assert_eq!(s.len(), 1);
        assert!(s.contains(pa(2, A)));
        assert!(!s.contains(pa(1, A)));
        assert_eq!(s.first(), Some(pa(2, A)));
        assert_eq!(Targets::from(pa(2, A)), s);
    }

    /// Tatáž množina vložená v jiném pořadí je TÁŽ hodnota — jinak by
    /// tatáž sdílená klávesa dala jinou revizi mapování a jiný text
    /// v config.json.
    #[test]
    fn kanonicka_bez_ohledu_na_poradi() {
        let cile = [pa(3, RT), pa(0, UP), pa(1, A), pa(0, A)];
        let mut a = Targets::EMPTY;
        for t in cile {
            assert!(a.insert(t));
        }
        let mut b = Targets::EMPTY;
        for t in cile.iter().rev() {
            assert!(b.insert(*t));
        }
        assert_eq!(a, b);
        // Pořadí: ovladač, pak Action::index (páčky před tlačítky).
        assert_eq!(
            a.iter().collect::<Vec<_>>(),
            vec![pa(0, UP), pa(0, A), pa(1, A), pa(3, RT)]
        );
        assert_eq!(a.first(), Some(pa(0, UP)));
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let h = |t: &Targets| {
            let mut s = DefaultHasher::new();
            t.hash(&mut s);
            s.finish()
        };
        assert_eq!(h(&a), h(&b));
    }

    #[test]
    fn strop_ctyri_cile() {
        let mut s = Targets::EMPTY;
        for i in 0..MAX_TARGETS_PER_KEY {
            assert!(s.insert(pa(i, A)));
        }
        assert_eq!(s.len(), 4);
        let pred = s;
        assert!(!s.insert(pa(0, UP)), "pátý cíl se nevejde");
        assert_eq!(s, pred, "neúspěch nic nezměnil");
        assert!(s.insert(pa(1, A)), "už tam je");
        assert_eq!(s, pred);
        assert_eq!(
            Targets::from_targets([pa(0, A), pa(0, A), pa(1, A)]).map(Targets::len),
            Ok(2),
            "duplicita se sloučí"
        );
        assert_eq!(
            Targets::from_targets([pa(0, A), pa(1, A), pa(2, A), pa(3, A), pa(0, UP)]),
            Err(TooManyTargets)
        );
    }

    #[test]
    fn odebirani_a_ovladace() {
        let mut s = Targets::from_targets([pa(0, A), pa(0, UP), pa(2, RT)]).unwrap();
        assert!(s.has_pad(PadId::ALL[0]) && s.has_pad(PadId::ALL[2]));
        assert!(!s.has_pad(PadId::ALL[1]));
        assert_eq!(
            s.actions(PadId::ALL[0]).iter().collect::<Vec<_>>(),
            vec![UP, A]
        );
        let jen2 = s.only_pads(&[false, false, true, true]);
        assert_eq!(jen2, Targets::one(pa(2, RT)));
        assert_eq!(
            s.minus(Targets::one(pa(0, A))),
            Targets::from_targets([pa(0, UP), pa(2, RT)]).unwrap()
        );
        assert!(s.remove(pa(0, A)));
        assert!(!s.remove(pa(0, A)));
        assert_eq!(s.remove_pad(PadId::ALL[0]), 1);
        assert_eq!(s.remove_pad(PadId::ALL[0]), 0);
        assert_eq!(s, Targets::one(pa(2, RT)));
        assert_eq!(format!("{s:?}"), format!("[{:?}]", pa(2, RT)));
    }
}

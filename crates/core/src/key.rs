//! Identita fyzické klávesy.

use serde::{Deserialize, Serialize};

/// Velikost tabulek indexovaných klávesou ([`KeyId::index`]).
pub(crate) const KEY_TABLE_SIZE: usize = 256;

/// Fyzická klávesa = scan kód + příznak „rozšířená" (prefix E0).
///
/// Proč scan kód, a ne virtuální klávesa (VK): VK závisí na rozložení.
/// Na české QWERTZ má VK_Z klávesa, která je na americké klávesnici Y,
/// a horní řada čísel dává bez Shiftu znaky s diakritikou. Scan kód
/// drží POZICI klávesy — W A S D jsou W A S D na každém rozložení
/// (princip 5 v ROADMAP.md).
///
/// `extended` rozlišuje dvojice se stejným scan kódem: šipka nahoru
/// (0x48 + E0) vs. 8 na numerické klávesnici (0x48), pravý Ctrl vs. levý,
/// Enter na numerické klávesnici vs. hlavní Enter. V hooku (Fáze 3) je
/// to `KBDLLHOOKSTRUCT::scanCode` + příznak `LLKHF_EXTENDED`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct KeyId {
    pub scan: u16,
    pub extended: bool,
}

impl KeyId {
    /// Běžná klávesa (bez prefixu E0).
    pub const fn new(scan: u16) -> Self {
        KeyId {
            scan,
            extended: false,
        }
    }

    /// Rozšířená klávesa (prefix E0) — šipky, pravý Ctrl/Alt, Enter na
    /// numerické klávesnici…
    pub const fn ext(scan: u16) -> Self {
        KeyId {
            scan,
            extended: true,
        }
    }

    /// Dá se klávesa namapovat na akci (nebo použít jako zkratka)?
    ///
    /// Make kódy sady 1 leží v 0x01–0x7F; prefix E0 nese `extended`,
    /// ne scan kód. Mimo tenhle rozsah jsou dvě skupiny, které se mapovat
    /// NESMÍ — engine je vůbec nesleduje a vždy je propouští do OS (i při
    /// přiřazování):
    /// - `scan == 0` — mediální a podobné klávesy, které scan kód nemají
    ///   (a které by si navíc všechny sdílely jedno `KeyId`);
    /// - AltGr na českém rozložení — Windows k němu posílají falešný levý
    ///   Ctrl se scan kódem typicky 0x21D. Kdyby prošel jako 0x1D, každé
    ///   napsané „@" nebo „€" by spustilo akci namapovanou na LCtrl.
    ///
    /// Do rozsahu patří i F13–F24, Pause/NumLock, PrintScreen a japonské
    /// klávesy — vše pod 0x80.
    pub const fn is_mappable(self) -> bool {
        self.scan >= 0x01 && self.scan <= 0x7F
    }

    /// Index do tabulek o [`KEY_TABLE_SIZE`] položkách: nízkých 7 bitů je
    /// scan kód, horní bit prefix E0. `None` pro nemapovatelné klávesy.
    ///
    /// Tabulka místo hashovací mapy: vyhledání je jeden přístup do pole,
    /// nic se nehashuje a hlavně nic nealokuje — engine běží v hook
    /// callbacku (princip 3).
    pub(crate) const fn index(self) -> Option<usize> {
        if self.is_mappable() {
            Some(self.scan as usize | if self.extended { 0x80 } else { 0 })
        } else {
            None
        }
    }

    /// Opak [`KeyId::index`].
    pub(crate) const fn from_index(i: usize) -> KeyId {
        KeyId {
            scan: (i & 0x7F) as u16,
            extended: i & 0x80 != 0,
        }
    }

    // ── Pojmenované klávesy (scan kódy sady 1, pozice US/CZ) ────────
    // Jen ty, které potřebuje výchozí mapování, engine a testy. Názvy
    // jsou podle POZICE (americké popisky); na české QWERTZ jsou na
    // těchto pozicích tatáž písmena, jen horní řada čísel píše +ěšč…

    pub const ESC: KeyId = KeyId::new(0x01);
    pub const DIGIT_1: KeyId = KeyId::new(0x02);
    pub const DIGIT_3: KeyId = KeyId::new(0x04);
    pub const BACKSPACE: KeyId = KeyId::new(0x0E);
    pub const Q: KeyId = KeyId::new(0x10);
    pub const W: KeyId = KeyId::new(0x11);
    pub const E: KeyId = KeyId::new(0x12);
    pub const R: KeyId = KeyId::new(0x13);
    pub const I: KeyId = KeyId::new(0x17);
    pub const ENTER: KeyId = KeyId::new(0x1C);
    pub const LEFT_CTRL: KeyId = KeyId::new(0x1D);
    pub const A: KeyId = KeyId::new(0x1E);
    pub const S: KeyId = KeyId::new(0x1F);
    pub const D: KeyId = KeyId::new(0x20);
    pub const F: KeyId = KeyId::new(0x21);
    pub const J: KeyId = KeyId::new(0x24);
    pub const K: KeyId = KeyId::new(0x25);
    pub const L: KeyId = KeyId::new(0x26);
    pub const LEFT_SHIFT: KeyId = KeyId::new(0x2A);
    pub const X: KeyId = KeyId::new(0x2D);
    pub const C: KeyId = KeyId::new(0x2E);
    pub const V: KeyId = KeyId::new(0x2F);
    pub const SPACE: KeyId = KeyId::new(0x39);
    pub const SCROLL_LOCK: KeyId = KeyId::new(0x46);
    /// 8 na numerické klávesnici — stejný scan kód jako šipka nahoru,
    /// jen bez E0. Testy s ním hlídají, že se `extended` nezanedbává.
    pub const NUMPAD_8: KeyId = KeyId::new(0x48);
    pub const ARROW_UP: KeyId = KeyId::ext(0x48);
    pub const ARROW_LEFT: KeyId = KeyId::ext(0x4B);
    pub const ARROW_RIGHT: KeyId = KeyId::ext(0x4D);
    pub const ARROW_DOWN: KeyId = KeyId::ext(0x50);
    /// Falešný levý Ctrl, který Windows posílají s AltGr (CZ rozložení).
    /// Hodnota podle ROADMAP.md — ve Fázi 3 ověřit v logu.
    pub const ALTGR_FAKE_CTRL: KeyId = KeyId::new(0x21D);
}

impl std::fmt::Display for KeyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.extended {
            write!(f, "E0 {:#04X}", self.scan)
        } else {
            write!(f, "{:#04X}", self.scan)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapovatelne_jsou_jen_make_kody() {
        assert!(KeyId::W.is_mappable());
        assert!(KeyId::ARROW_UP.is_mappable());
        assert!(KeyId::new(0x7F).is_mappable());
        assert!(
            !KeyId::new(0).is_mappable(),
            "mediální klávesy bez scan kódu"
        );
        assert!(!KeyId::new(0x80).is_mappable());
        assert!(
            !KeyId::ALTGR_FAKE_CTRL.is_mappable(),
            "falešný Ctrl z AltGr"
        );
    }

    #[test]
    fn index_je_bijekce_na_mapovatelnych() {
        let mut seen = [false; KEY_TABLE_SIZE];
        for scan in 0..=0x300u16 {
            for extended in [false, true] {
                let k = KeyId { scan, extended };
                match k.index() {
                    Some(i) => {
                        assert!(k.is_mappable());
                        assert!(i < KEY_TABLE_SIZE);
                        assert!(!seen[i], "{k} koliduje");
                        seen[i] = true;
                        assert_eq!(KeyId::from_index(i), k);
                    }
                    None => assert!(!k.is_mappable()),
                }
            }
        }
        // 0x01–0x7F × {bez E0, s E0}
        assert_eq!(seen.iter().filter(|&&s| s).count(), 254);
    }

    #[test]
    fn sipka_a_numpad_jsou_ruzne_klavesy() {
        assert_eq!(KeyId::ARROW_UP.scan, KeyId::NUMPAD_8.scan);
        assert_ne!(KeyId::ARROW_UP, KeyId::NUMPAD_8);
    }

    #[test]
    fn altgr_neni_levy_ctrl() {
        assert_ne!(KeyId::ALTGR_FAKE_CTRL, KeyId::LEFT_CTRL);
    }

    #[test]
    fn zobrazeni() {
        assert_eq!(KeyId::W.to_string(), "0x11");
        assert_eq!(KeyId::ARROW_UP.to_string(), "E0 0x48");
    }
}

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

    /// Patří klávesa Windows (levá a pravá Win, E0 0x5B / E0 0x5C)?
    ///
    /// Win nejde mapovat, přiřadit ani použít jako zkratku (zpětná vazba
    /// vlastníka k Fázi 4): kdyby ji engine sledoval, v Gamepadu by ji
    /// spolkl jako klávesu ovladače a Win+D, Win+E ani Start by za hry
    /// nefungovaly. Jako zkratka by se navíc bila se zkratkami Windows —
    /// modifikátor zkratky bude jen Alt (Fáze 7). Proto ji engine vůbec
    /// nesleduje a jde vždy do OS, i při přiřazování.
    ///
    /// Jen s E0: Win přichází vždy s prefixem (v hooku `LLKHF_EXTENDED`),
    /// stejný scan kód bez něj je jiná klávesa a ta Windows nepatří.
    pub const fn is_reserved(self) -> bool {
        self.extended && (self.scan == 0x5B || self.scan == 0x5C)
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
    /// Uvnitř rozsahu se stejně zachází s klávesou Win
    /// ([`KeyId::is_reserved`]) — ta scan kód má, ale patří Windows.
    ///
    /// Do rozsahu patří i F13–F24, Pause/NumLock, PrintScreen a japonské
    /// klávesy — vše pod 0x80.
    pub const fn is_mappable(self) -> bool {
        self.scan >= 0x01 && self.scan <= 0x7F && !self.is_reserved()
    }

    /// Modifikátor — levý i pravý Ctrl, Shift a Alt (pravý Alt je na
    /// českém rozložení AltGr)?
    ///
    /// Při přiřazování jde jeho stisk do Windows a přiřadí se až ťuknutím
    /// (key-up bez jiné klávesy mezitím): kdyby se přiřadil hned při
    /// key-downu, Alt+Tab, Alt+F4 ani Ctrl+Shift+Esc by z okna KeyPadu
    /// nevedly nikam a vstup by potichu dostal Alt (otevřená otázka 55).
    ///
    /// Shift jen bez E0 — E0 0x2A a E0 0x36 jsou „falešné" Shifty, které
    /// klávesnice posílá s jinými klávesami (Print Screen…), ne modifikátor.
    pub const fn is_modifier(self) -> bool {
        matches!(
            (self.scan, self.extended),
            (0x1D | 0x38, _) | (0x2A | 0x36, false)
        )
    }

    /// Smí být klávesa zkratkou pozastavení, kterou uživatel přiřadí
    /// v okně (Fáze 7, Z6)? Jen **F1–F24 kromě F4, Scroll Lock a Pause**,
    /// vše bez E0.
    ///
    /// Seznam povolených, ne zakázaných: zkratku engine spolkne v každém
    /// režimu po celou dobu, kdy je hook v systému (zapnutý ovladač nebo
    /// okno v popředí), a i s drženým Altem — výjimku má jen Win. Se
    /// zkratkou Tab by v celém systému přestal fungovat Alt+Tab, se
    /// zkratkou Enter, Backspace, šipkou, Caps Lock nebo Print Screen
    /// totéž pro ně (princip 1 a 8). Ze stejného důvodu chybí F4: Alt+F4
    /// by hru nezavřel, jen pozastavil, a za pauzy by v jiném okně
    /// zachytávání obnovil — psaní by pak mířilo do padu (princip 1,
    /// nalezeno revizí). Ostatní F-klávesy nic celosystémového nemají.
    ///
    /// Platí jen pro přiřazování z okna: zkratka mimo seznam ručně
    /// zapsaná v `config.json` platí dál ([`crate::Mapping::new`] ji kvůli
    /// starším souborům brát nepřestane; okno ji jen označí — OQ 69).
    /// Pause v LL hooku: scan 0x45 bez E0 (Num Lock je 0x45 s E0, Ctrl+Pause
    /// = Break 0x46 s E0 — v seznamu nejsou); ověří vlastník (OQ 69).
    pub const fn is_toggle_candidate(self) -> bool {
        !self.extended
            && matches!(
                self.scan,
                // F1–F3, F5–F10 (bez F4 0x3E), Pause, Scroll Lock, F11,
                // F12, F13–F23, F24
                0x3B..=0x3D | 0x3F..=0x44 | 0x45 | 0x46 | 0x57 | 0x58 | 0x64..=0x6E | 0x76
            )
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
    pub const RIGHT_CTRL: KeyId = KeyId::ext(0x1D);
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
    pub const RIGHT_SHIFT: KeyId = KeyId::new(0x36);
    /// Levý Alt. Mapovat jde; okno u něj jen varuje, že při hraní
    /// nepůjde Alt+Tab (Fáze 6).
    pub const LEFT_ALT: KeyId = KeyId::new(0x38);
    pub const SPACE: KeyId = KeyId::new(0x39);
    pub const TAB: KeyId = KeyId::new(0x0F);
    pub const F1: KeyId = KeyId::new(0x3B);
    /// F4 — zkratkou pozastavení být nesmí: s ní by Alt+F4 nezavřel okno
    /// (OQ 69).
    pub const F4: KeyId = KeyId::new(0x3E);
    pub const F5: KeyId = KeyId::new(0x3F);
    /// F12 — jde za zkratku pozastavení, okno ale varuje: Steam jím fotí
    /// snímek obrazovky a se zapnutým ovladačem by ho nedostal.
    pub const F12: KeyId = KeyId::new(0x58);
    pub const F24: KeyId = KeyId::new(0x76);
    /// Pause/Break bez Ctrl (Num Lock má týž scan kód s E0).
    pub const PAUSE: KeyId = KeyId::new(0x45);
    pub const SCROLL_LOCK: KeyId = KeyId::new(0x46);
    /// 8 na numerické klávesnici — stejný scan kód jako šipka nahoru,
    /// jen bez E0. Testy s ním hlídají, že se `extended` nezanedbává.
    pub const NUMPAD_8: KeyId = KeyId::new(0x48);
    pub const ARROW_UP: KeyId = KeyId::ext(0x48);
    pub const ARROW_LEFT: KeyId = KeyId::ext(0x4B);
    pub const ARROW_RIGHT: KeyId = KeyId::ext(0x4D);
    pub const ARROW_DOWN: KeyId = KeyId::ext(0x50);
    /// Pravý Alt (na českém rozložení AltGr) — stejný scan kód jako levý,
    /// jen s E0.
    pub const RIGHT_ALT: KeyId = KeyId::ext(0x38);
    /// Levá a pravá Win — patří Windows ([`KeyId::is_reserved`]).
    pub const LEFT_WIN: KeyId = KeyId::ext(0x5B);
    pub const RIGHT_WIN: KeyId = KeyId::ext(0x5C);
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
        // 0x01–0x7F × {bez E0, s E0} bez levé a pravé Win
        assert_eq!(seen.iter().filter(|&&s| s).count(), 252);
    }

    #[test]
    fn win_patri_windows() {
        for k in [KeyId::LEFT_WIN, KeyId::RIGHT_WIN] {
            assert!(k.is_reserved(), "{k}");
            assert!(!k.is_mappable(), "{k}");
            assert_eq!(k.index(), None, "{k} engine nesleduje");
        }
        // Vyhrazené jsou právě tyhle dvě klávesy — Alt ani stejné scan
        // kódy bez E0 ne.
        let mut vyhrazene = Vec::new();
        for scan in 0..=0x300u16 {
            for extended in [false, true] {
                let k = KeyId { scan, extended };
                if k.is_reserved() {
                    vyhrazene.push(k);
                }
            }
        }
        assert_eq!(vyhrazene, vec![KeyId::LEFT_WIN, KeyId::RIGHT_WIN]);
        for k in [KeyId::LEFT_ALT, KeyId::RIGHT_ALT] {
            assert!(k.is_mappable() && !k.is_reserved(), "{k}");
        }
        assert_ne!(KeyId::LEFT_ALT, KeyId::RIGHT_ALT);
    }

    #[test]
    fn modifikatory_jsou_prave_ctrl_shift_alt() {
        let mut modifikatory = Vec::new();
        for scan in 0..=0x300u16 {
            for extended in [false, true] {
                let k = KeyId { scan, extended };
                if k.is_modifier() {
                    modifikatory.push(k);
                }
            }
        }
        modifikatory.sort();
        let mut cekane = vec![
            KeyId::LEFT_CTRL,
            KeyId::RIGHT_CTRL,
            KeyId::LEFT_SHIFT,
            KeyId::RIGHT_SHIFT,
            KeyId::LEFT_ALT,
            KeyId::RIGHT_ALT,
        ];
        cekane.sort();
        assert_eq!(modifikatory, cekane);
        // Všechny jdou mapovat (přiřazují se ťuknutím); falešný Ctrl
        // z AltGr modifikátor není — engine ho vůbec nesleduje.
        assert!(cekane.iter().all(|k| k.is_mappable()));
        assert!(!KeyId::ALTGR_FAKE_CTRL.is_modifier());
        assert!(!KeyId::LEFT_WIN.is_modifier());
    }

    /// Celá tabulka (i nemapovatelné kódy a E0) proti ručnímu výčtu
    /// z ROADMAP (Z6, OQ 69): F1–F10 0x3B–0x44 bez F4 0x3E, F11 0x57,
    /// F12 0x58, F13–F23 0x64–0x6E, F24 0x76, Scroll Lock 0x46, Pause
    /// 0x45 — vše bez E0.
    #[test]
    fn zkratka_jen_f_klavesy_scroll_lock_a_pause() {
        let mut cekane: Vec<KeyId> = Vec::new();
        for s in 0x3Bu16..=0x44 {
            if s != 0x3E {
                cekane.push(KeyId::new(s));
            }
        }
        cekane.extend([KeyId::new(0x57), KeyId::new(0x58)]);
        for s in 0x64u16..=0x6E {
            cekane.push(KeyId::new(s));
        }
        cekane.extend([KeyId::new(0x76), KeyId::SCROLL_LOCK, KeyId::PAUSE]);
        cekane.sort();
        assert_eq!(
            cekane.len(),
            23 + 2,
            "23 F-kláves (bez F4), Scroll Lock a Pause"
        );
        let mut nalezene = Vec::new();
        for scan in 0..=0x300u16 {
            for extended in [false, true] {
                let k = KeyId { scan, extended };
                if k.is_toggle_candidate() {
                    assert!(k.is_mappable(), "{k} musí jít sledovat");
                    nalezene.push(k);
                }
            }
        }
        assert_eq!(nalezene, cekane);
        // Klávesy, které by zkratka vzala celému systému, v seznamu nejsou.
        for k in [
            KeyId::TAB,
            KeyId::ENTER,
            KeyId::BACKSPACE,
            KeyId::ESC,
            KeyId::W,
            KeyId::LEFT_SHIFT,
            KeyId::LEFT_ALT,
            KeyId::LEFT_CTRL,
            KeyId::ARROW_UP,
            KeyId::new(0x3A), // Caps Lock
            KeyId::ext(0x45), // Num Lock
            KeyId::ext(0x46), // Break (Ctrl+Pause)
            KeyId::ext(0x37), // Print Screen
            KeyId::LEFT_WIN,
            KeyId::F4, // Alt+F4 musí dál zavírat okna (OQ 69)
        ] {
            assert!(!k.is_toggle_candidate(), "{k}");
        }
        for k in [KeyId::F1, KeyId::F5, KeyId::F12, KeyId::F24] {
            assert!(k.is_toggle_candidate(), "{k}");
        }
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

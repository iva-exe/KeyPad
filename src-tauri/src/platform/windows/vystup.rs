//! Kam hook předává rozhodnutí enginu (Fáze 4): stavy ovladačů do
//! slotů jejich pad vláken, režim pro okno.
//!
//! Volá se z callbacku hooku — proto jen atomiky a události Windows,
//! žádný zámek, alokace ani logování (princip 3). Okno se o změně
//! režimu dozví přes [`Budik`]: probudí vlákno, které teprve vydá
//! Tauri událost (z callbacku `emit` nikdy).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use keypad_core::{Decision, Mode, MAX_PADS};
use serde::Serialize;

use super::hook::{Udalost, Vystup};
use super::slot::{Budik, StavSlot};

/// Režim, jak ho ukazuje okno.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rezim {
    /// Žádný ovladač není zapnutý — klávesnice patří Windows.
    Disabled,
    /// Zachytávání pozastavené (zkratka) — klávesy jdou do Windows,
    /// zapnuté ovladače stojí v neutrálu.
    Paused,
    /// Zachytávání běží — klávesy zapnutých ovladačů ovládají je.
    Capturing,
    /// Přiřazuje se klávesa.
    Binding,
    /// Ovladač je zapnutý, ale hook nejde dostat do systému — klávesy
    /// jdou do Windows. Okno nesmí tvrdit, že ovladač ovládají.
    NoHook,
}

/// Režim pro okno s pořadovým číslem změny: odpověď příkazu `rezim`
/// a událost se můžou předběhnout — okno si nechá novější.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RezimInfo {
    pub rezim: Rezim,
    pub seq: u64,
}

impl Rezim {
    pub fn z(m: Mode) -> Rezim {
        match m {
            Mode::Disabled { .. } => Rezim::Disabled,
            Mode::Keyboard => Rezim::Paused,
            Mode::Gamepad => Rezim::Capturing,
            Mode::Binding { .. } => Rezim::Binding,
        }
    }

    fn kod(self) -> u64 {
        match self {
            Rezim::Disabled => 0,
            Rezim::Paused => 1,
            Rezim::Capturing => 2,
            Rezim::Binding => 3,
            Rezim::NoHook => 4,
        }
    }

    fn z_kodu(k: u64) -> Rezim {
        match k {
            1 => Rezim::Paused,
            2 => Rezim::Capturing,
            3 => Rezim::Binding,
            4 => Rezim::NoHook,
            _ => Rezim::Disabled,
        }
    }
}

/// Bity atomiku `stav`: 0–7 režim enginu, 8 = hook nejde, zbytek číslo
/// změny.
const REZIM: u64 = 0xFF;
const CHYBA: u64 = 1 << 8;
const SEQ_POSUN: u32 = 9;

pub struct HookVystup {
    sloty: [Arc<StavSlot>; MAX_PADS],
    /// Režim, chyba hooku a číslo změny v JEDNOM atomiku — okno tak
    /// nikdy nedostane nový režim se starým číslem. Zapisuje jen hook
    /// vlákno (callback i smyčka), proto stačí načíst a uložit.
    stav: AtomicU64,
    budik: Arc<Budik>,
}

impl HookVystup {
    /// `budik` probudí vlákno, které změnu režimu ohlásí oknu.
    pub fn new(sloty: [Arc<StavSlot>; MAX_PADS], budik: Arc<Budik>) -> HookVystup {
        HookVystup {
            sloty,
            stav: AtomicU64::new(Rezim::Disabled.kod()),
            budik,
        }
    }

    /// Režim s číslem změny pro okno. Chyba hooku přebíjí zapnuté stavy
    /// (bez hooku se nic nezachytává); bez ovladače je to prostě
    /// `Disabled`.
    pub fn info(&self) -> RezimInfo {
        let s = self.stav.load(Ordering::Acquire);
        let rezim = match Rezim::z_kodu(s & REZIM) {
            Rezim::Disabled => Rezim::Disabled,
            _ if s & CHYBA != 0 => Rezim::NoHook,
            r => r,
        };
        RezimInfo {
            rezim,
            seq: s >> SEQ_POSUN,
        }
    }

    /// Nový obsah atomiku; při změně zvýší číslo a probudí okno.
    fn zmen(&self, f: impl FnOnce(u64) -> u64) {
        let stary = self.stav.load(Ordering::Acquire);
        let obsah = f(stary & (REZIM | CHYBA));
        if obsah != stary & (REZIM | CHYBA) {
            let seq = (stary >> SEQ_POSUN).wrapping_add(1);
            self.stav.store(seq << SEQ_POSUN | obsah, Ordering::Release);
            self.budik.probud();
        }
    }
}

impl Vystup for HookVystup {
    fn rozhodnuti(&self, _u: Option<&Udalost>, d: &Decision, rezim: Mode) {
        for (pad, stav) in d.pads.iter() {
            if let Some(slot) = self.sloty.get(pad.index()) {
                slot.zapis(stav);
            }
        }
        let kod = Rezim::z(rezim).kod();
        self.zmen(|s| s & CHYBA | kod);
    }

    fn hook_chyba(&self, chyba: bool) {
        self.zmen(|s| if chyba { s | CHYBA } else { s & !CHYBA });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keypad_core::{DisabledReason, PadId, PadState, PadUpdates, AXIS_MAX};

    fn vystup() -> (HookVystup, [Arc<StavSlot>; MAX_PADS], Arc<Budik>) {
        let sloty: [Arc<StavSlot>; MAX_PADS] =
            std::array::from_fn(|_| Arc::new(StavSlot::new().unwrap()));
        let budik = Arc::new(Budik::new().unwrap());
        (
            HookVystup::new(sloty.clone(), Arc::clone(&budik)),
            sloty,
            budik,
        )
    }

    fn stav() -> PadState {
        PadState {
            thumb_ly: AXIS_MAX,
            ..PadState::NEUTRAL
        }
    }

    /// Stav jde jen do slotu svého ovladače; okno se budí jen změnou
    /// režimu, ne každou klávesou.
    #[test]
    fn stav_do_sveho_slotu_rezim_jen_pri_zmene() {
        let (v, sloty, budik) = vystup();
        let mut pads = PadUpdates::NONE;
        pads.set(PadId::new(2).unwrap(), stav());
        let d = Decision {
            pads,
            ..Decision::NONE
        };
        v.rozhodnuti(None, &d, Mode::Gamepad);
        assert_eq!(sloty[2].cti().unwrap().stav, stav());
        assert_eq!(sloty[2].cti().unwrap().cislo, 1);
        for i in [0, 1, 3] {
            assert_eq!(sloty[i].cti().unwrap().cislo, 0, "slot {i}");
        }
        assert_eq!(v.info().rezim, Rezim::Capturing);
        assert_eq!(v.info().seq, 1);
        assert!(budik.cekej(Some(0)));
        v.rozhodnuti(None, &d, Mode::Gamepad);
        assert!(!budik.cekej(Some(0)), "stejný režim okno nebudí");
        assert_eq!(v.info().seq, 1, "ani nemění číslo");
        v.rozhodnuti(
            None,
            &Decision::NONE,
            Mode::Disabled {
                reason: DisabledReason::PadError,
            },
        );
        assert_eq!(v.info().rezim, Rezim::Disabled);
        assert!(budik.cekej(Some(0)));
        assert_eq!(v.info().seq, 2);
    }

    /// Hook nejde: zapnutý ovladač se v okně nesmí tvářit, že klávesy
    /// ovládají ho. Bez ovladače je to jen „vypnuto". Každá změna zvýší
    /// číslo (okno zahodí starší odpověď).
    #[test]
    fn chyba_hooku_je_videt_a_cisluje_se() {
        let (v, _sloty, budik) = vystup();
        v.hook_chyba(true);
        assert_eq!(v.info().rezim, Rezim::Disabled, "bez ovladače nic nechybí");
        assert!(budik.cekej(Some(0)));
        v.rozhodnuti(None, &Decision::NONE, Mode::Keyboard);
        assert_eq!(v.info().rezim, Rezim::NoHook);
        v.rozhodnuti(None, &Decision::NONE, Mode::Gamepad);
        assert_eq!(v.info().rezim, Rezim::NoHook);
        let pred = v.info().seq;
        v.hook_chyba(true);
        assert_eq!(v.info().seq, pred, "stejná chyba nic nemění");
        v.hook_chyba(false);
        assert_eq!(
            v.info(),
            RezimInfo {
                rezim: Rezim::Capturing,
                seq: pred + 1
            }
        );
    }

    /// Výstup běží v callbacku hooku — nesmí alokovat.
    #[test]
    fn vystup_nealokuje() {
        let (v, _sloty, _budik) = vystup();
        let mut pads = PadUpdates::NONE;
        for p in PadId::ALL {
            pads.set(p, stav());
        }
        let d = Decision {
            suppress: true,
            pads,
            ui: None,
        };
        let pred = crate::testy_alokace::pocet();
        for m in [Mode::Gamepad, Mode::Keyboard, Mode::Gamepad] {
            v.rozhodnuti(None, &d, m);
        }
        assert_eq!(crate::testy_alokace::pocet(), pred);
    }
}

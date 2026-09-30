//! Kam hook předává rozhodnutí enginu (Fáze 4): stavy ovladačů do
//! slotů jejich pad vláken, režim pro okno.
//!
//! Volá se z callbacku hooku — proto jen atomiky a události Windows,
//! žádný zámek, alokace ani logování (princip 3). Okno se o změně
//! režimu dozví přes [`Budik`]: probudí vlákno, které teprve vydá
//! Tauri událost (z callbacku `emit` nikdy).

use std::sync::atomic::{AtomicU8, Ordering};
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

    fn kod(self) -> u8 {
        match self {
            Rezim::Disabled => 0,
            Rezim::Paused => 1,
            Rezim::Capturing => 2,
            Rezim::Binding => 3,
        }
    }

    fn z_kodu(k: u8) -> Rezim {
        match k {
            1 => Rezim::Paused,
            2 => Rezim::Capturing,
            3 => Rezim::Binding,
            _ => Rezim::Disabled,
        }
    }
}

pub struct HookVystup {
    sloty: [Arc<StavSlot>; MAX_PADS],
    rezim: AtomicU8,
    budik: Arc<Budik>,
}

impl HookVystup {
    /// `budik` probudí vlákno, které změnu režimu ohlásí oknu.
    pub fn new(sloty: [Arc<StavSlot>; MAX_PADS], budik: Arc<Budik>) -> HookVystup {
        HookVystup {
            sloty,
            rezim: AtomicU8::new(Rezim::Disabled.kod()),
            budik,
        }
    }

    /// Poslední režim od enginu — čte se bez čekání odkudkoli.
    pub fn rezim(&self) -> Rezim {
        Rezim::z_kodu(self.rezim.load(Ordering::Acquire))
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
        if self.rezim.swap(kod, Ordering::AcqRel) != kod {
            self.budik.probud();
        }
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
        assert_eq!(v.rezim(), Rezim::Capturing);
        assert!(budik.cekej(Some(0)));
        v.rozhodnuti(None, &d, Mode::Gamepad);
        assert!(!budik.cekej(Some(0)), "stejný režim okno nebudí");
        v.rozhodnuti(
            None,
            &Decision::NONE,
            Mode::Disabled {
                reason: DisabledReason::PadError,
            },
        );
        assert_eq!(v.rezim(), Rezim::Disabled);
        assert!(budik.cekej(Some(0)));
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

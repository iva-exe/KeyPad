//! Cesta hook → pad vlákno bez zámku (Fáze 4).
//!
//! Hook callback nesmí posílat stav přes frontu crossbeamu: `send` do
//! čekajícího vlákna bere `std::sync::Mutex` sdílený s GUI a fronta
//! občas alokuje (princip 3). Tady hook jen přepíše „nejnovější stav"
//! ve dvou atomikách a nastaví událost Windows (`SetEvent` nic nesdílí
//! s GUI). Pad vlákno čeká na tutéž událost i kvůli příkazům z okna —
//! jejich odesílatel ji nastaví po `send` ([`Probouzec`]).
//!
//! Ze stavů platí jen nejnovější; mezistav, který pad vlákno nestihlo
//! přečíst, se zahodí. Je to v pořádku: stav padu je vždy celý
//! přepočet z držených kláves (princip 7), ne přírůstek.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use keypad_core::PadState;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject, INFINITE};

/// Auto-reset událost Windows: probudí jedno čekající vlákno, nic
/// nezamyká a nealokuje — smí ji nastavit i callback hooku.
pub struct Budik(HANDLE);

// SAFETY: handle události jde používat z kteréhokoli vlákna.
unsafe impl Send for Budik {}
// SAFETY: SetEvent/WaitForSingleObject jsou vláknově bezpečné.
unsafe impl Sync for Budik {}

impl Drop for Budik {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: handle z CreateEventW, zavírá se právě jednou.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

impl Budik {
    /// `Err`, když Windows nedají událost (vyčerpané prostředky).
    pub fn new() -> Result<Budik, String> {
        // SAFETY: nepojmenovaná auto-reset událost, výchozí zabezpečení.
        unsafe { CreateEventW(None, false, false, None) }
            .map(Budik)
            .map_err(|e| format!("CreateEventW: {e}"))
    }

    pub fn probud(&self) {
        // SAFETY: platný handle události po celý život budíku.
        let _ = unsafe { SetEvent(self.0) };
    }

    /// Čeká nejvýš `ms` (`None` = bez limitu). `true` = probuzen.
    pub fn cekej(&self, ms: Option<u64>) -> bool {
        let limit = ms.map_or(INFINITE, |ms| u32::try_from(ms).unwrap_or(INFINITE - 1));
        // SAFETY: platný handle události po celý život budíku.
        unsafe { WaitForSingleObject(self.0, limit) == WAIT_OBJECT_0 }
    }
}

/// Nejnovější stav jednoho padu + událost, na kterou čeká jeho vlákno.
///
/// Stav (12 bajtů) se do jednoho atomiku nevejde, proto dva a v obou
/// stejné 16bitové číslo zápisu: čtenář, který chytí půlky ze dvou
/// různých zápisů, to pozná a přečte znovu. Zapisovatel nikdy nečeká.
/// Aby se čísla zápisu shodla omylem, muselo by mezi dvěma čteními
/// proběhnout 65 536 zápisů — stisky kláves chodí po desítkách.
pub struct StavSlot {
    /// číslo zápisu (16) | tlačítka (16) | LT (8) | RT (8) | LX (16)
    a: AtomicU64,
    /// číslo zápisu (16) | LY (16) | RX (16) | RY (16)
    b: AtomicU64,
    zapisu: AtomicU32,
    budik: Budik,
}

/// Přečtený stav a číslo jeho zápisu (nový stav = jiné číslo).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Precteno {
    pub cislo: u16,
    pub stav: PadState,
}

impl StavSlot {
    /// Nový slot s neutrálem (číslo zápisu 0). `Err`, když Windows
    /// nedají událost — pad vlákno pak nejde spustit.
    pub fn new() -> Result<StavSlot, String> {
        Ok(StavSlot {
            a: AtomicU64::new(0),
            b: AtomicU64::new(0),
            zapisu: AtomicU32::new(0),
            budik: Budik::new()?,
        })
    }

    /// Zapíše nový stav a probudí pad vlákno.
    ///
    /// Smí jen JEDEN zapisovatel (hook vlákno) — dva by si čísla zápisu
    /// přepisovaly. Nealokuje, nečeká, nic nezamyká (volá se z callbacku
    /// hooku).
    pub fn zapis(&self, s: PadState) {
        let cislo = u64::from((self.zapisu.fetch_add(1, Ordering::Relaxed) as u16).wrapping_add(1));
        let a = cislo << 48
            | u64::from(s.buttons) << 32
            | u64::from(s.left_trigger) << 24
            | u64::from(s.right_trigger) << 16
            | u64::from(s.thumb_lx as u16);
        let b = cislo << 48
            | u64::from(s.thumb_ly as u16) << 32
            | u64::from(s.thumb_rx as u16) << 16
            | u64::from(s.thumb_ry as u16);
        self.a.store(a, Ordering::Release);
        self.b.store(b, Ordering::Release);
        self.probud();
    }

    /// Nejnovější celý stav. `None` jen tehdy, když zapisovatel právě
    /// zapisuje — jeho zápis vzápětí nastaví událost znovu.
    pub fn cti(&self) -> Option<Precteno> {
        for _ in 0..4 {
            let a = self.a.load(Ordering::Acquire);
            let b = self.b.load(Ordering::Acquire);
            if a >> 48 == b >> 48 {
                return Some(Precteno {
                    cislo: (a >> 48) as u16,
                    stav: PadState {
                        buttons: (a >> 32) as u16,
                        left_trigger: (a >> 24) as u8,
                        right_trigger: (a >> 16) as u8,
                        thumb_lx: a as u16 as i16,
                        thumb_ly: (b >> 32) as u16 as i16,
                        thumb_rx: (b >> 16) as u16 as i16,
                        thumb_ry: b as u16 as i16,
                    },
                });
            }
        }
        None
    }

    /// Nastaví událost (nový příkaz ve frontě pad vlákna).
    pub fn probud(&self) {
        self.budik.probud();
    }

    /// Čeká na událost nejvýš `ms` (`None` = bez limitu). `true` =
    /// událost přišla.
    pub fn cekej(&self, ms: Option<u64>) -> bool {
        self.budik.cekej(ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keypad_core::{AXIS_DIAGONAL, AXIS_MAX, TRIGGER_MAX};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// Stav, který se s `i` mění v OBOU půlkách slotu (LX v první,
    /// RX a RY ve druhé) — jinak by test smíchání půlek nic neověřil.
    fn stav(i: i16) -> PadState {
        PadState {
            buttons: 0x1234,
            left_trigger: TRIGGER_MAX,
            right_trigger: 0,
            thumb_lx: i,
            thumb_ly: AXIS_DIAGONAL,
            thumb_rx: -AXIS_MAX,
            // Nikdy nenegovat i16::MIN (princip u stavu padu platí i v testu).
            thumb_ry: i.saturating_neg(),
        }
    }

    #[test]
    fn novy_slot_je_neutral_s_cislem_nula() {
        let s = StavSlot::new().unwrap();
        assert_eq!(
            s.cti(),
            Some(Precteno {
                cislo: 0,
                stav: PadState::NEUTRAL
            })
        );
        assert!(!s.cekej(Some(0)), "událost na začátku není");
    }

    #[test]
    fn zapis_se_precte_cely_a_probudi() {
        let s = StavSlot::new().unwrap();
        s.zapis(stav(-AXIS_MAX));
        assert!(s.cekej(Some(0)));
        assert!(!s.cekej(Some(0)), "auto-reset");
        let p = s.cti().unwrap();
        assert_eq!((p.cislo, p.stav), (1, stav(-AXIS_MAX)));
        // Krajní hodnoty os projdou beze změny (nikdy i16::MIN z enginu,
        // ale slot by přenesl i ten).
        s.zapis(stav(i16::MIN));
        assert_eq!(s.cti().unwrap().stav, stav(i16::MIN));
    }

    /// Čtenář nikdy nedostane směs dvou zápisů: zapisovatel střídá dva
    /// stavy, které se liší v obou půlkách, čtenář to hlídá.
    #[test]
    fn soubezne_cteni_nikdy_nesmicha_dva_zapisy() {
        let s = Arc::new(StavSlot::new().unwrap());
        let konec = Instant::now() + Duration::from_millis(300);
        let pisar = {
            let s = Arc::clone(&s);
            std::thread::spawn(move || {
                let mut n = 0u32;
                while Instant::now() < konec {
                    s.zapis(if n % 2 == 0 { stav(1) } else { stav(-1) });
                    n += 1;
                }
                n
            })
        };
        let mut precteno = 0u32;
        while Instant::now() < konec {
            if let Some(p) = s.cti() {
                if p.cislo != 0 {
                    assert!(p.stav == stav(1) || p.stav == stav(-1), "{p:?}");
                    precteno += 1;
                }
            }
        }
        assert!(pisar.join().unwrap() > 1000);
        assert!(precteno > 0);
    }

    #[test]
    fn cekani_s_limitem_skonci() {
        let s = StavSlot::new().unwrap();
        let t = Instant::now();
        assert!(!s.cekej(Some(20)));
        assert!(t.elapsed() >= Duration::from_millis(15));
        s.probud();
        assert!(s.cekej(None));
    }
}

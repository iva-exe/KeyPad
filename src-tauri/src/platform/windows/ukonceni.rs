//! Ukončení na žádost instalátoru.
//!
//! Zavření okna KeyPad jen schová do oznamovací oblasti (jako WinSent),
//! takže WM_CLOSE aplikaci neukončí. Instalátor (aktualizace,
//! odinstalace) proto otevře pojmenovanou událost
//! [`updater::QUIT_EVENT_NAME`] a nastaví ji; KeyPad pak skončí stejnou
//! cestou jako „Ukončit" v nabídce — neutrální pad, odpojení, log.

use std::ffi::c_void;

use windows::core::HSTRING;
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};

/// Vlákno jen čeká v jádře — nic nepočítá a skoro nic nepotřebuje.
const ZASOBNIK: usize = 64 * 1024;

/// Založí událost a vlákno, které na ni čeká. Po nastavení události
/// zavolá `pri_signalu` (jednou).
pub fn hlidej(pri_signalu: impl FnOnce() + Send + 'static) -> Result<(), String> {
    // Auto-reset, bez výchozího signálu. Když už událost existuje
    // (nemělo by — single-instance pustí jen jednu instanci), dostaneme
    // handle na tutéž, což je v pořádku.
    // SAFETY: jméno žije po celou dobu volání; výchozí zabezpečení
    // = jen tenhle uživatel (a `Local\` = jen tahle relace).
    let udalost =
        unsafe { CreateEventW(None, false, false, &HSTRING::from(updater::QUIT_EVENT_NAME)) }
            .map_err(|e| format!("událost pro ukončení nejde založit: {e}"))?;
    // HANDLE není Send; jako číslo ho do vlákna přenést jde.
    let h = udalost.0 as usize;
    std::thread::Builder::new()
        .name("keypad-ukonceni".into())
        .stack_size(ZASOBNIK)
        .spawn(move || {
            let udalost = HANDLE(h as *mut c_void);
            // SAFETY: handle patří tomuhle procesu a nikdo ho nezavírá —
            // žije až do konce procesu (se jménem musí zůstat i událost).
            let r = unsafe { WaitForSingleObject(udalost, INFINITE) };
            if r == WAIT_OBJECT_0 {
                log::info!("instalátor žádá o ukončení ({})", updater::QUIT_EVENT_NAME);
                pri_signalu();
            } else {
                log::warn!("čekání na událost ukončení selhalo ({})", r.0);
            }
        })
        .map(|_| ())
        .map_err(|e| format!("vlákno pro ukončení nejde spustit: {e}"))
}

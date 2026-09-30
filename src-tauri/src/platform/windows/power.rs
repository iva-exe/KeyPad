//! Uspání a probuzení počítače — bez okna a bez pollování.
//!
//! ViGEmBus 1.21.442 má otevřenou chybu: modrá obrazovka WDF_VIOLATION
//! po probuzení, když byl během spánku připojený virtuální pad
//! (nefarius/ViGEmBus#160, projekt je archivovaný, oprava nepřijde).
//! Doporučené obejití: před spánkem pad odpojit. Proto pad vlákno před
//! uspáním pošle neutrál a odpojí se — a po probuzení zůstane vypnuté:
//! ovladač se připojuje JEN na povel uživatele (Fáze 2b).
//!
//! `PowerRegisterSuspendResumeNotification` s callbackem (Windows 8+,
//! powrprof.dll ze System32): zprávy chodí do vlákna systémového poolu,
//! žádné skryté okno ani smyčka zpráv navíc. `WM_POWERBROADCAST` pro
//! engine (`reset_held`) si ve Fázi 5 vezme hook vlákno samo.
//!
//! powrprof.dll se schválně NEimportuje staticky, ale načte se až tady
//! a jen ze System32 ([`registracni_funkce`]). Statický import ji
//! natáhne při startu procesu a ona si hned (ještě před `main`) dotáhne
//! UMPDC.dll — odložený import powrprof, na který `/DEPENDENTLOADFLAG`
//! KeyPad.exe nedosáhne a `SetDefaultDllDirectories` v `main` ještě
//! neplatí. UMPDC.dll podstrčená vedle přenosného KeyPad.exe ve
//! Stažených souborech se pak načetla (ověřeno skutečnou kopií DLL).

use std::ffi::c_void;
use std::sync::OnceLock;
use std::time::Duration;

use std::sync::Arc;

use windows::core::{s, w};
use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::System::Power::DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS;
use windows::Win32::UI::WindowsAndMessaging::{
    DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND,
};

use super::pad::{PadPrikaz, Pady};

/// Co potřebuje spánek od virtuálních ovladačů. Rozhraní kvůli testu
/// obsluhy bez skutečných pad vláken.
pub trait Napajeni: Send + Sync {
    /// Všechny ovladače neutrál → odpojit; `true` = potvrzeno do `limit`.
    fn uspat(&self, limit: Duration) -> bool;
    /// Počítač se probudil — nic se nezapíná, jen končí pojistka proti
    /// zapnutí těsně před spánkem.
    fn probuzeni(&self);
}

impl Napajeni for Pady {
    fn uspat(&self, limit: Duration) -> bool {
        Pady::uspat(self, limit)
    }

    fn probuzeni(&self) {
        self.vsem(|| PadPrikaz::Probuzeni);
    }
}

/// `PowerRegisterSuspendResumeNotification` (powerbase.h): příznaky,
/// příjemce, výstup registrace → kód Win32.
type Registrace = unsafe extern "system" fn(u32, HANDLE, *mut *mut c_void) -> u32;

/// `PowerRegisterSuspendResumeNotification` z powrprof.dll ze System32.
/// Knihovna se neuvolňuje — registrace v ní žije do konce procesu.
fn registracni_funkce() -> Result<Registrace, String> {
    // SAFETY: jméno systémové DLL, hledá se jen v System32.
    let dll = unsafe { LoadLibraryExW(w!("powrprof.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
        .map_err(|e| format!("powrprof.dll nejde načíst: {e}"))?;
    // SAFETY: platný handle knihovny a jméno exportu zakončené nulou.
    let f = unsafe { GetProcAddress(dll, s!("PowerRegisterSuspendResumeNotification")) }
        .ok_or("powrprof.dll nemá PowerRegisterSuspendResumeNotification")?;
    // SAFETY: podpis podle powerbase.h (a windows-rs): DWORD flags,
    // HANDLE recipient, PHPOWERNOTIFY → DWORD; volací konvence system.
    Ok(unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, Registrace>(f) })
}

/// Jak dlouho zdržet uspání, než pad potvrdí odpojení. Normálně je to
/// pár milisekund (neutrál + odpojení); déle jen s visícím ovladačem —
/// a spánek kvůli němu stát nesmí.
const LIMIT_ODPOJENI: Duration = Duration::from_secs(2);

/// Kam posílat. Callback nemá jiný kontext než ukazatel, statická
/// proměnná je jednodušší a nic nevisí na životnosti.
static PAD: OnceLock<Arc<dyn Napajeni>> = OnceLock::new();

/// Zaregistruje hlídání spánku. Chyba se jen zaloguje — aplikace běží
/// dál, jen bez vypnutí padu před spánkem.
pub fn registruj(pad: Arc<dyn Napajeni>) {
    if PAD.set(pad).is_err() {
        return;
    }
    let registruj_se = match registracni_funkce() {
        Ok(f) => f,
        Err(e) => {
            log::warn!("hlídání spánku nejde zaregistrovat ({e}) — pad se před spánkem nevypne");
            return;
        }
    };
    // Registrace (a s ní i parametry) žije do konce procesu, odregistrovat
    // není potřeba — proto `leak`.
    let parametry: &'static mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS =
        Box::leak(Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(zmena_napajeni),
            Context: std::ptr::null_mut(),
        }));
    let mut registrace: *mut c_void = std::ptr::null_mut();
    // SAFETY: `parametry` žijí navždy; s DEVICE_NOTIFY_CALLBACK je
    // „recipient" ukazatel na DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS.
    let r = unsafe {
        registruj_se(
            DEVICE_NOTIFY_CALLBACK.0,
            HANDLE((parametry as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS).cast()),
            &mut registrace,
        )
    };
    if r == ERROR_SUCCESS.0 {
        log::info!("hlídání spánku zaregistrované (před uspáním se pad vypne)");
    } else {
        log::warn!("hlídání spánku nejde zaregistrovat (chyba {r}) — pad se před spánkem nevypne");
    }
}

/// Callback Windows (vlákno systémového poolu). Panika přes hranici FFI
/// by shodila proces — proto `catch_unwind`.
unsafe extern "system" fn zmena_napajeni(
    _kontext: *const c_void,
    typ: u32,
    _nastaveni: *const c_void,
) -> u32 {
    let _ = std::panic::catch_unwind(|| {
        if let Some(pad) = PAD.get() {
            obsluz(pad.as_ref(), typ);
        }
    });
    ERROR_SUCCESS.0
}

/// Obsluha oznámení — mimo callback, ať ji test prožene bez skutečného
/// spánku.
pub(super) fn obsluz(pad: &dyn Napajeni, typ: u32) {
    match typ {
        PBT_APMSUSPEND => {
            log::info!("počítač se uspává — vypínám virtuální ovladače (ViGEmBus #160)");
            if !pad.uspat(LIMIT_ODPOJENI) {
                log::warn!("ovladače se před spánkem do 2 s neodpojily — spánek nečeká");
            }
            // Ať řádky o spánku přežijí i výpadek napájení ve spánku.
            crate::logger::flush(Duration::from_millis(500));
        }
        // AUTOMATIC chodí po každém probuzení, RESUMESUSPEND navíc, když
        // se probudil uživatel. Pad zůstává vypnutý — zapne ho uživatel;
        // probuzení jen ruší pojistku proti zapnutí těsně před spánkem.
        PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => {
            log::info!("počítač se probudil — virtuální ovladače zůstávají vypnuté");
            pad.probuzeni();
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// powrprof.dll jde načíst ze System32 a funkce v ní je — bez
    /// registrace (ta by do testu tahala skutečné uspávání).
    #[test]
    fn registracni_funkce_se_najde_v_system32() {
        assert!(registracni_funkce().is_ok());
    }

    /// Obsluha oznámení o spánku: uspání počká na potvrzení (ne na
    /// limit) a probuzení jen ohlásí probuzení — nic, co by pad zapnulo.
    /// Co s tím dělají skutečná pad vlákna, testuje `pad.rs`.
    #[test]
    fn uspani_ceka_na_potvrzeni_a_probuzeni_nic_nezapne() {
        use std::sync::Mutex;
        #[derive(Default)]
        struct Zaznam(Mutex<Vec<&'static str>>);
        impl Napajeni for Zaznam {
            fn uspat(&self, _limit: Duration) -> bool {
                self.0.lock().unwrap().push("uspat");
                true
            }
            fn probuzeni(&self) {
                self.0.lock().unwrap().push("probuzeni");
            }
        }
        let z = Zaznam::default();
        let start = std::time::Instant::now();
        obsluz(&z, PBT_APMSUSPEND);
        assert!(start.elapsed() < LIMIT_ODPOJENI, "potvrzení, ne limit");
        obsluz(&z, PBT_APMRESUMEAUTOMATIC);
        obsluz(&z, PBT_APMRESUMESUSPEND);
        obsluz(&z, 0xFFFF);
        assert_eq!(*z.0.lock().unwrap(), ["uspat", "probuzeni", "probuzeni"]);
    }
}

//! Spuštění instalátoru ViGEmBus (`KeyPadSetup.exe /vigembus`).
//!
//! KeyPad sám práva správce nikdy nemá ani o ně nežádá (princip 6):
//! spouští obyčejně („open", žádné „runas") vlastní instalátor
//! z instalační složky. Ten vysvětlí, co udělá, stáhne a ověří oficiální
//! instalátor ViGEmBus a teprve ten požádá Windows o oprávnění.
//!
//! Po skončení instalátoru pad vlákno zkusí připojit znovu — to je
//! ověření, že ovladač opravdu běží (plán → provedení → ověření).

use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
    SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// Běží instalátor spuštěný odsud? Dvojklik na tlačítko nesmí pustit
/// dva instalátory ovladače najednou.
static BEZI: AtomicBool = AtomicBool::new(false);

/// Spustí `exe` s `parametry` a po jeho skončení zavolá `po_skonceni`
/// s návratovým kódem (když ho jde zjistit).
///
/// Vrací se, jakmile proces běží. Spuštění i čekání obstará vlastní
/// vlákno: ShellExecuteEx potřebuje COM (MSDN) a může chvíli trvat
/// (antivirus) — hlavní vlákno s oknem nesmí stát.
pub fn spust_a_hlidej(
    exe: &Path,
    parametry: &str,
    po_skonceni: impl FnOnce(Option<u32>) + Send + 'static,
) -> Result<(), String> {
    if BEZI.swap(true, Ordering::AcqRel) {
        return Err("Instalátor ViGEmBus už běží — dokonči ho v jeho okně.".into());
    }
    let exe = exe.to_path_buf();
    let parametry = parametry.to_string();
    let (tx, rx) = crossbeam_channel::bounded::<Result<(), String>>(1);
    // Výchozí zásobník: ShellExecuteEx může v procesu spustit rozšíření
    // Průzkumníka (COM), která s malým zásobníkem nepočítají.
    let vlakno = std::thread::Builder::new()
        .name("keypad-instalator".into())
        .spawn(move || {
            // SAFETY: COM pro tohle vlákno; STA + bez OLE1DDE doporučuje
            // MSDN pro ShellExecuteEx. Uvolní se na konci vlákna.
            let com =
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
            match spust(&exe, &parametry) {
                Ok(proces) => {
                    let _ = tx.send(Ok(()));
                    po_skonceni(pockej(proces));
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            }
            if com.is_ok() {
                // SAFETY: párové k úspěšnému CoInitializeEx výše.
                unsafe { CoUninitialize() };
            }
            BEZI.store(false, Ordering::Release);
        });
    if let Err(e) = vlakno {
        BEZI.store(false, Ordering::Release);
        return Err(format!("instalátor nejde spustit: {e}"));
    }
    rx.recv_timeout(Duration::from_secs(60))
        .unwrap_or_else(|_| Err("instalátor se do minuty nespustil".into()))
}

/// ShellExecuteExW „open" plnou cestou. Vrací handle procesu (může být
/// prázdný, pokud ho Windows nedají).
fn spust(exe: &Path, parametry: &str) -> Result<Option<usize>, String> {
    let soubor = HSTRING::from(exe.as_os_str());
    let param = HSTRING::from(parametry);
    let slozka = exe.parent().map(|d| HSTRING::from(d.as_os_str()));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        // NOASYNC: vlákno po návratu čeká na proces, ale ShellExecuteEx
        // má dojet celý tady. FLAG_NO_UI: chybu ukáže okno KeyPadu česky.
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: windows::core::w!("open"),
        lpFile: PCWSTR(soubor.as_ptr()),
        lpParameters: PCWSTR(param.as_ptr()),
        lpDirectory: slozka
            .as_ref()
            .map_or(PCWSTR::null(), |s| PCWSTR(s.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: všechny řetězce žijí po celou dobu volání, struktura má
    // správnou velikost.
    unsafe { ShellExecuteExW(&mut info) }
        .map_err(|e| format!("instalátor nejde spustit: {}", e.message()))?;
    Ok((!info.hProcess.is_invalid()).then_some(info.hProcess.0 as usize))
}

/// Počká na konec procesu a vrátí jeho návratový kód.
fn pockej(proces: Option<usize>) -> Option<u32> {
    let h = HANDLE(proces? as *mut c_void);
    let mut kod = 0u32;
    // SAFETY: handle procesu z ShellExecuteExW (NOCLOSEPROCESS) patří
    // nám a zavírá se tady, jednou.
    unsafe {
        WaitForSingleObject(h, INFINITE);
        let ok = GetExitCodeProcess(h, &mut kod).is_ok();
        let _ = CloseHandle(h);
        ok.then_some(kod)
    }
}

//! Spuštění instalátoru ViGEmBus (`KeyPadSetup.exe /vigembus`)
//! a otevření odkazu v prohlížeči.
//!
//! KeyPad sám práva správce nikdy nemá ani o ně nežádá (princip 6):
//! spouští obyčejně („open", žádné „runas") vlastní instalátor
//! z instalační složky. Ten stáhne a ověří oficiální instalátor
//! ViGEmBus a teprve ten požádá Windows o oprávnění.
//!
//! Před spuštěním pad vlákno ovladač vypne a přepínač zablokuje; po
//! skončení instalátoru blokaci zruší a sběrnici znovu ověří — tak se
//! pozná, že ovladač opravdu je (plán → provedení → ověření). Virtuální
//! ovladač se tím nepřipojí; zapne ho až uživatel.

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

/// Běží instalátor ViGEmBus spuštěný z okna? Během něj se virtuální
/// ovladač nezapíná — instalace nebo aktualizace ovladače by ho pod
/// rukama odebrala.
pub fn instalator_bezi() -> bool {
    BEZI.load(Ordering::Acquire)
}

/// Zámek instalátoru ViGEmBus: dokud žije, druhý instalátor z okna
/// nejde spustit. Při uvolnění zavolá `po_uvolneni` — PRÁVĚ JEDNOU
/// a v každém případě: instalátor doběhl, nespustil se, vlákno pro něj
/// nevzniklo, nebo se ke spuštění vůbec nedošlo. Na tom stojí zrušení
/// blokace přepínače; kdyby se na některé cestě zapomnělo, přepínač by
/// zůstal zablokovaný až do restartu KeyPadu.
pub struct Zamek {
    po_uvolneni: Option<Box<dyn FnOnce() + Send>>,
}

impl Zamek {
    /// `None`, když už instalátor z okna běží.
    pub fn zaber(po_uvolneni: impl FnOnce() + Send + 'static) -> Option<Zamek> {
        if BEZI.swap(true, Ordering::AcqRel) {
            return None;
        }
        Some(Zamek {
            po_uvolneni: Some(Box::new(po_uvolneni)),
        })
    }
}

impl Drop for Zamek {
    fn drop(&mut self) {
        BEZI.store(false, Ordering::Release);
        if let Some(po) = self.po_uvolneni.take() {
            po();
        }
    }
}

/// Otevře adresu ve výchozím prohlížeči (ShellExecuteExW „open").
///
/// Jen `https://` a jen adresy, které předá backend (pevný seznam
/// v `gamepad::open_link`) — nikdy text z okna. Vlastní vlákno se STA:
/// ShellExecuteEx potřebuje COM a může chvíli trvat.
pub fn otevri_url(url: &'static str) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("tuhle adresu KeyPad neotevírá".into());
    }
    let (tx, rx) = crossbeam_channel::bounded::<Result<(), String>>(1);
    std::thread::Builder::new()
        .name("keypad-odkaz".into())
        .spawn(move || {
            // SAFETY: COM pro tohle vlákno (viz `spust_a_hlidej`).
            let com =
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
            let adresa = HSTRING::from(url);
            let mut info = SHELLEXECUTEINFOW {
                cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
                fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
                lpVerb: windows::core::w!("open"),
                lpFile: PCWSTR(adresa.as_ptr()),
                nShow: SW_SHOWNORMAL.0,
                ..Default::default()
            };
            // SAFETY: řetězce žijí po celou dobu volání, struktura má
            // správnou velikost.
            let r = unsafe { ShellExecuteExW(&mut info) }
                .map_err(|e| format!("prohlížeč nejde otevřít: {}", e.message()));
            let _ = tx.send(r);
            if com.is_ok() {
                // SAFETY: párové k úspěšnému CoInitializeEx výše.
                unsafe { CoUninitialize() };
            }
        })
        .map_err(|e| format!("prohlížeč nejde otevřít: {e}"))?;
    rx.recv_timeout(Duration::from_secs(30))
        .unwrap_or_else(|_| Err("prohlížeč se neotevřel včas".into()))
}

/// Spustí `exe` s `parametry` a hlídá ho; `zamek` se uvolní (a zavolá
/// svoje `po_uvolneni`), až proces skončí — nebo hned, když se
/// nespustí.
///
/// Vrací se, jakmile proces běží. Spuštění i čekání obstará vlastní
/// vlákno: ShellExecuteEx potřebuje COM (MSDN) a může chvíli trvat
/// (antivirus) — hlavní vlákno s oknem nesmí stát. Když se do minuty
/// nerozhodne, vrátí se chyba, ale zámek dál drží vlákno: kdyby se
/// proces přece jen spustil, blokace přepínače skončí až s ním.
pub fn spust_a_hlidej(zamek: Zamek, exe: &Path, parametry: &str) -> Result<(), String> {
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
                    match pockej(proces) {
                        Some(k) => log::info!("instalátor ViGEmBus skončil (kód {k})"),
                        None => log::info!("instalátor ViGEmBus skončil (kód neznámý)"),
                    }
                }
                Err(e) => {
                    log::warn!("{e}");
                    let _ = tx.send(Err(e));
                }
            }
            if com.is_ok() {
                // SAFETY: párové k úspěšnému CoInitializeEx výše.
                unsafe { CoUninitialize() };
            }
            drop(zamek);
        });
    // Vlákno nevzniklo: uzávěr i se zámkem už je zahozený — blokace
    // přepínače je tím zrušená.
    vlakno.map_err(|e| format!("instalátor nejde spustit: {e}"))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;

    /// Zámek: druhý instalátor nejde, uvolnění zavolá `po_uvolneni`
    /// právě jednou (i při zahození bez spuštění) a pak jde znovu.
    /// Jediný test nad globálním `BEZI` — souběžné testy by se praly.
    #[test]
    fn zamek_uvolni_prave_jednou() {
        let pocet = Arc::new(AtomicU32::new(0));
        let p = Arc::clone(&pocet);
        let zamek = Zamek::zaber(move || {
            p.fetch_add(1, Ordering::SeqCst);
        })
        .expect("první zámek");
        assert!(instalator_bezi());
        assert!(Zamek::zaber(|| panic!("druhý zámek nesmí vzniknout")).is_none());
        assert_eq!(pocet.load(Ordering::SeqCst), 0);
        drop(zamek);
        assert!(!instalator_bezi());
        assert_eq!(pocet.load(Ordering::SeqCst), 1);

        // Zámek, který skončí ve vlákně (tak ho drží `spust_a_hlidej`).
        let p = Arc::clone(&pocet);
        let zamek = Zamek::zaber(move || {
            p.fetch_add(1, Ordering::SeqCst);
        })
        .expect("po uvolnění jde znovu");
        std::thread::spawn(move || drop(zamek)).join().unwrap();
        assert_eq!(pocet.load(Ordering::SeqCst), 2);
        assert!(!instalator_bezi());
    }
}

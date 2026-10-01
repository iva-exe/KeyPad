//! DLL, které si za běhu dotahují samy systémové knihovny, jen ze
//! System32 — pro celý proces.
//!
//! `/DEPENDENTLOADFLAG` (build.rs) chrání jen statické importy
//! KeyPad.exe. Systémové DLL si ale další knihovny načítají až za běhu
//! a jen JMÉNEM (WinHttp → IPHLPAPI, šifrování → CRYPTSP/CRYPTBASE,
//! SHGetKnownFolderPath → profapi, MMDevAPI → AudioSes při otevření
//! zvuku) a ty by Windows hledaly NEJDŘÍV ve složce programu —
//! přenosný KeyPad.exe ve Stažených souborech by načetl DLL, kterou tam
//! podstrčila kdejaká stránka. Neplatí to jen pro naše volání, ale pro
//! celý proces, i pro LoadLibrary v cizím kódu (Tauri, WebView2).
//! KeyPad žádné vlastní DLL nemá, složka programu se tedy nehledá vůbec.
//!
//! Ani objekt COM, jehož DLL je v registru zapsaná absolutní cestou,
//! sám nestačí: absolutní cestou se načte jen ta jedna DLL, její
//! závislosti už podle pořadí hledání procesu (sonda revize: bez tohoto
//! volání si MMDevAPI.dll ze System32 vzala podvrženou AudioSes.dll ze
//! složky programu). Proto se kód, který na tom závisí, ptá
//! [`omezeno`] — zvuk bez něj nehraje.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::System::LibraryLoader::{
    SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

/// Hledání DLL procesu už je omezené na System32.
static OMEZENO: AtomicBool = AtomicBool::new(false);

/// Omezí hledání DLL za běhu na System32. Volá se jako ÚPLNĚ PRVNÍ
/// příkaz `main` — každá DLL načtená dřív by se hledala postaru.
pub fn jen_ze_system32() -> windows::core::Result<()> {
    // SAFETY: jen nastaví pořadí hledání DLL pro tenhle proces.
    unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) }?;
    OMEZENO.store(true, Ordering::Release);
    Ok(())
}

/// Proběhlo [`jen_ze_system32`] úspěšně? Kód, který načítá systémové
/// knihovny s dalšími závislostmi (zvuk), bez toho nesmí běžet.
pub fn omezeno() -> bool {
    OMEZENO.load(Ordering::Acquire)
}

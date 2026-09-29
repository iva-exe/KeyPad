//! Microsoft Edge WebView2 Runtime — co KeyPad potřebuje od systému,
//! ale instalátor to sám dodat nemůže.
//!
//! Jen se **čte** z registru. WebView2 Runtime je komponenta Microsoftu
//! s vlastním instalátorem; úkolem KeyPadSetupu je jasně říct, že chybí
//! a kde ho vzít — ne mlčky dokončit instalaci aplikace, která se pak
//! nespustí. (Ovladač ViGEmBus instalátor umí doinstalovat sám, po
//! kliknutí — stav i ověřené vydání má `updater::vigembus`, postup
//! `driver.rs`.)

use windows::core::HSTRING;
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
};

/// Stránka Microsoftu s „Evergreen Bootstrapperem" WebView2 Runtime
/// (sdílená s aplikací přes updater).
pub use updater::WEBVIEW2_URL;

/// GUID klienta WebView2 Runtime v EdgeUpdate (stálý, z dokumentace
/// Microsoftu k distribuci WebView2).
const WEBVIEW2_CLIENT: &str =
    r"Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";

/// Je nainstalovaný Microsoft Edge WebView2 Runtime?
///
/// Okno KeyPadu je Tauri, tedy WebView2 — bez runtime se aplikace
/// spustí a hned skončí, uživatel neuvidí vůbec nic. Na Windows 11 je
/// runtime součástí systému, na Windows 10 ho většinou přinesla
/// aktualizace Edge, ale „většinou" nestačí.
///
/// Kontroluje se tak, jak radí Microsoft: hodnota `pv` na třech
/// místech — 32bitový pohled HKLM (systémová instalace na 64bit
/// Windows), nativní HKLM a HKCU (instalace jen pro uživatele).
pub fn webview2_present() -> bool {
    let places = [
        (
            HKEY_LOCAL_MACHINE,
            format!(r"SOFTWARE\WOW6432Node\{WEBVIEW2_CLIENT}"),
        ),
        (HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\{WEBVIEW2_CLIENT}")),
        (HKEY_CURRENT_USER, format!(r"Software\{WEBVIEW2_CLIENT}")),
    ];
    places
        .iter()
        .any(|(root, path)| webview2_version_ok(read_sz(*root, path, "pv").as_deref()))
}

/// Je hodnota `pv` platná verze runtime?
///
/// Odinstalovaný runtime po sobě klíč někdy nechá a verzi přepíše na
/// „0.0.0.0" — takový záznam tedy neznamená „nainstalováno".
pub fn webview2_version_ok(pv: Option<&str>) -> bool {
    match pv.map(str::trim) {
        Some(v) => !v.is_empty() && v != "0.0.0.0",
        None => false,
    }
}

/// Přečte řetězcovou hodnotu z registru; `None`, když klíč nebo hodnota
/// chybí (nebo má jiný typ).
fn read_sz(root: HKEY, path: &str, name: &str) -> Option<String> {
    let sub = HSTRING::from(path);
    let val = HSTRING::from(name);
    // SAFETY: velikost bufferu se nejdřív zjistí a pak se do něj čte
    // s touž hodnotou velikosti; RegGetValueW zapisuje nanejvýš tolik.
    unsafe {
        let mut size: u32 = 0;
        let rc = RegGetValueW(root, &sub, &val, RRF_RT_REG_SZ, None, None, Some(&mut size));
        if rc.is_err() || size == 0 {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        let rc = RegGetValueW(
            root,
            &sub,
            &val,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        );
        if rc.is_err() {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platna_verze_webview2() {
        assert!(webview2_version_ok(Some("128.0.2739.42")));
        assert!(!webview2_version_ok(Some("0.0.0.0")));
        assert!(!webview2_version_ok(Some("")));
        assert!(!webview2_version_ok(Some("  ")));
        assert!(!webview2_version_ok(None));
    }

    /// Na vývojovém stroji (Windows 10/11 s Edge) runtime je. Test tu
    /// hlídá hlavně to, že čtení z registru nepadá a vrací bool.
    #[test]
    fn cteni_registru_nepada() {
        let _ = webview2_present();
    }
}

//! Zápisy, které z instalace dělají „normální aplikaci": zástupce
//! v nabídce Start a záznam v Nastavení → Aplikace.
//!
//! Všechno jde do profilu uživatele (HKCU, jeho vlastní nabídka Start)
//! — zápis do HKLM nebo do společné nabídky Start by chtěl práva
//! správce, a ta KeyPad z principu nikdy nežádá. Windows per-user
//! záznamy v HKCU\…\Uninstall ukazují v seznamu aplikací stejně jako
//! ty systémové (tak to dělá třeba VS Code v uživatelské variantě).

use std::path::{Path, PathBuf};

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
};

const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\KeyPad";

/// Zástupce v nabídce Start **tohoto uživatele**
/// (`%APPDATA%\Microsoft\Windows\Start Menu\Programs\KeyPad.lnk`).
pub fn start_menu_lnk() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(r"AppData\Roaming"))
        })
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\Public\AppData\Roaming"));
    base.join(r"Microsoft\Windows\Start Menu\Programs")
        .join(format!("{}.lnk", updater::APP_NAME))
}

/// Zapíše záznam do Nastavení → Aplikace, aby šlo KeyPad odinstalovat
/// běžnou cestou, bez hledání složky v profilu.
pub fn register_uninstall(
    setup_exe: &Path,
    install_dir: &Path,
    version: &str,
    size_kb: u32,
) -> Result<(), String> {
    let app = install_dir.join(updater::APP_EXE);
    // SAFETY: klíč se vždy zavírá; hodnoty jsou platné UTF-16 buffery,
    // které žijí po celou dobu volání.
    unsafe {
        let mut key = HKEY::default();
        let rc = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(UNINSTALL_KEY),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        );
        if rc.is_err() {
            return Err(format!("nelze zapsat do registru: {rc:?}"));
        }

        let sz = |name: &str, value: &str| {
            let w: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = std::slice::from_raw_parts(w.as_ptr() as *const u8, w.len() * 2);
            let _ = RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(bytes));
        };
        let dw = |name: &str, value: u32| {
            let _ = RegSetValueExW(
                key,
                &HSTRING::from(name),
                None,
                REG_DWORD,
                Some(&value.to_le_bytes()),
            );
        };

        sz("DisplayName", updater::APP_NAME);
        sz("DisplayVersion", version);
        sz("Publisher", "KeyPad");
        sz("InstallLocation", &install_dir.to_string_lossy());
        sz("DisplayIcon", &app.to_string_lossy());
        sz(
            "UninstallString",
            &format!("\"{}\" /uninstall", setup_exe.to_string_lossy()),
        );
        // Odinstalace i „změna" vedou na tentýž instalátor — ten se
        // podle parametru rozhodne, co udělat. Bez parametru = znovu
        // nainstalovat aktuální verzi, což je zároveň oprava.
        sz(
            "ModifyPath",
            &format!("\"{}\"", setup_exe.to_string_lossy()),
        );
        sz(
            "URLInfoAbout",
            &format!("https://github.com/{}", updater::REPO),
        );
        dw("NoModify", 0);
        dw("NoRepair", 1);
        dw("EstimatedSize", size_kb);

        let _ = RegCloseKey(key);
    }
    Ok(())
}

/// Existuje záznam v Nastavení → Aplikace? (Jen pro hlášku odinstalace:
/// „nebylo co odebírat" je poctivější než „odebráno".)
pub fn uninstall_entry_exists() -> bool {
    // SAFETY: jen otevření pro čtení; otevřený klíč se hned zavírá.
    unsafe {
        let mut key = HKEY::default();
        let rc = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(UNINSTALL_KEY),
            None,
            KEY_READ,
            &mut key,
        );
        if rc.is_ok() {
            let _ = RegCloseKey(key);
            true
        } else {
            false
        }
    }
}

/// Odstraní záznam z Nastavení → Aplikace.
pub fn unregister_uninstall() {
    // SAFETY: mazání podstromu; neexistence není chyba.
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(UNINSTALL_KEY));
    }
}

/// Vytvoří zástupce v nabídce Start.
///
/// Zkratka se skládá přes IShellLink — stejné API, jaké používá
/// Průzkumník; žádné generování .lnk bajtů ručně.
pub fn create_shortcut(target: &Path, lnk: &Path) -> Result<(), String> {
    use windows::core::Interface;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    if let Some(dir) = lnk.parent() {
        // Složka Programs v profilu existuje vždy — až na čerstvě
        // založené nebo ručně pročištěné profily. Pojistka nic nestojí.
        let _ = std::fs::create_dir_all(dir);
    }
    // SAFETY: COM se inicializuje pro tohle vlákno; rozhraní se uvolní
    // s koncem platnosti proměnných.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("nelze vytvořit zástupce: {e}"))?;
        link.SetPath(&HSTRING::from(target.as_os_str()))
            .map_err(|e| format!("zástupce bez cíle: {e}"))?;
        if let Some(dir) = target.parent() {
            let _ = link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()));
        }
        let _ = link.SetDescription(&HSTRING::from("KeyPad — klávesnice jako Xbox ovladač"));

        let persist: IPersistFile = link
            .cast()
            .map_err(|e| format!("zástupce nelze uložit: {e}"))?;
        persist
            .Save(PCWSTR(HSTRING::from(lnk.as_os_str()).as_ptr()), true)
            .map_err(|e| format!("zástupce nelze zapsat: {e}"))?;
    }
    Ok(())
}

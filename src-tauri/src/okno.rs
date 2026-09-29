//! Vzhled a vyzdvižení okna — co se nedá nastavit v tauri.conf.json.

/// Dá hlavní okno do popředí — bez syntetického vstupu.
///
/// Schválně NE `set_focus()` z Tauri: tao v něm, když Windows
/// SetForegroundWindow odmítnou (hra drží popředí), pošle přes
/// SendInput stisk a uvolnění Altu, aby zámek popředí obešel. Ten Alt
/// by dostala hra v popředí — přesně ten zásah do cizího programu, který
/// princip 8 vylučuje. Tady se jen slušně požádá; když Windows odmítnou,
/// tlačítko na hlavním panelu zabliká, dokud si ho uživatel nevšimne.
///
/// Ukázat a obnovit okno musí volající přes Tauri (`show`, `unminimize`)
/// — tao si viditelnost pamatuje a přímé ShowWindow by ho rozhodilo
/// (příští `hide` by pak nic neudělal).
pub fn do_popredi(w: &tauri::WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::{
        FlashWindowEx, SetForegroundWindow, FLASHWINFO, FLASHW_TIMERNOFG, FLASHW_TRAY,
    };

    let Ok(hwnd) = w.hwnd() else {
        return;
    };
    // SAFETY: platný handle okna tohoto procesu; FLASHWINFO má správnou
    // velikost a žije po celou dobu volání.
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() {
            return;
        }
        let blikani = FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd,
            dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
            uCount: 0,
            dwTimeout: 0,
        };
        let _ = FlashWindowEx(&blikani);
    }
    log::debug!("Windows nepustily okno do popředí — bliká tlačítko na hlavním panelu");
}

/// Zaoblené rohy a tmavý systémový rámeček přes DWM (jako WinSent).
///
/// Okno je bez dekorací a průhledné, takže rohy by jinak byly ostré.
/// Obojí umí až Windows 11 — na desítkách atributy neexistují, volání
/// tiše selže a okno zůstane s ostrými rohy. Region jako náhrada (co
/// dělá WinSent u vyhledávací lišty) se tu schválně nepoužívá: okno se
/// dá zvětšovat a region by se musel přepočítávat při každé změně
/// velikosti i DPI, za cenu tvrdé nevyhlazené hrany.
pub fn zaobli_a_ztmav(w: &tauri::WebviewWindow) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
        DWMWCP_ROUND,
    };

    let Ok(hwnd) = w.hwnd() else {
        return;
    };
    let rohy = DWMWCP_ROUND;
    // Na Windows 11 si materiál pozadí řídí systém a ve světlém motivu
    // ho dodá SVĚTLÝ — okno by vyšlo bledé. Tímhle si řekneme o tmavou
    // variantu. BOOL je pro DWM čtyřbajtová nenulová hodnota; i32 je
    // přesně to.
    let tmave: i32 = 1;
    // SAFETY: platný handle okna; ukazatele míří na hodnoty, které žijí
    // po celou dobu volání, a velikosti odpovídají jejich typům.
    unsafe {
        let r = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &rohy as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&rohy) as u32,
        );
        if r.is_err() {
            log::debug!("DWM zaoblení rohů nejde (Windows 10?) — okno zůstane hranaté");
        }
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &tmave as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&tmave) as u32,
        );
    }
}

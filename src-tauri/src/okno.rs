//! Vzhled okna, který se nedá nastavit v tauri.conf.json.

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

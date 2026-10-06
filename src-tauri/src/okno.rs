//! Vzhled a vyzdvižení okna — co se nedá nastavit v tauri.conf.json.

use tauri::utils::config::WindowEffectsConfig;
use tauri::window::{Color, Effect};

/// První build Windows 11. Od něj má okno systémový Mica místo blur.
const WINDOWS_11: u32 = 22_000;

/// Číslo buildu Windows (19045, 22631…) z `RtlGetVersion`; 0, když ho
/// nejde zjistit.
///
/// `RtlGetVersion`, ne `GetVersionEx`: ta bez manifestu „podporuje
/// Windows 10" lže a vrací 6.2 (build 9200). ntdll ji má na každém
/// Windows a KeyPad ji importuje už teď (Tauri, window-vibrancy) —
/// žádný nový import pro `check-imports`.
pub fn build_windows() -> u32 {
    use windows::Wdk::System::SystemServices::RtlGetVersion;
    use windows::Win32::System::SystemInformation::OSVERSIONINFOW;

    let mut v = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    // SAFETY: struktura má vyplněnou velikost a žije po celou dobu volání.
    let r = unsafe { RtlGetVersion(&mut v) };
    if r.is_ok() {
        v.dwBuildNumber
    } else {
        0
    }
}

/// Pozadí okna podle buildu — rozhoduje schopnost systému, ne
/// předpoklad (princip 9).
///
/// - **Windows 10:** blur se stejným tónem jako hlavní okno WinSentu.
///   WinSent s ním přestal zadrhávat při tažení (acrylic na desítkách
///   při tažení a změně velikosti trhá — nedokumentované API).
/// - **Windows 11:** systémový Mica. Blur přes totéž nedokumentované
///   API na 22H2+ (build 22621) podle autorů window-vibrancy při tažení
///   zadrhává a Microsoft to neopraví. Mica je dokumentovaný materiál
///   DWM a tón si řídí systém — tmavou variantu drží `"theme": "Dark"`
///   v tauri.conf.json (proč ne DWM odsud: viz [`zaobli`]).
///
/// Na Windows 10 před 1809 blur chybí (window-vibrancy vrátí chybu,
/// Tauri ji zahodí) — okno zůstane s poloprůhledným tmavým pozadím
/// z CSS, jen bez rozmazání.
///
/// **Tažení na Windows 10 (revize Fáze 2b):** efekt je PŘESNĚ ten, který
/// měla Fáze 2 i hlavní okno WinSentu (test níž) a Tauri ho aplikuje
/// stejnou cestou, takže tohle místo na desítkách nic nemění. Prošlo se,
/// v čem se KeyPad od WinSentu liší, a nic z toho tažení nezatěžuje:
/// - `preventOverflow` jen jednou zkrátí okno při vytvoření
///   (tauri-runtime-wry), za tažení se nevolá;
/// - `data-tauri-drag-region="deep"` rozhoduje jen o tom, který mousedown
///   tažení začne; samotné tažení je v obou aplikacích táž systémová
///   smyčka (`start_dragging` → tao → WM_NCLBUTTONDOWN/HTCAPTION);
/// - CSS: žádný `backdrop-filter` ani `will-change`, stín jen u 8px tečky
///   stavu a u panelu „O aplikaci" (klik do titulku ho zavře); jediná
///   nekonečná animace (pulz tečky) běží jen ve stavu „zapínám…";
/// - události `tauri://move` Tauri do stránky neposílá (nikdo je
///   neposlouchá), cíl paměti WebView2 se mění jen při schování/ukázání;
/// - okno je menší než WinSent (440×620 proti 1180×740), rozmazává se
///   tedy menší plocha;
/// - verze Tauri/tao/wry/window-vibrancy jsou tytéž (2.11.x, 0.35.3,
///   0.55.1, 0.6.0).
///
/// Kdyby tažení na Windows 10 přesto zadrhávalo, je to blur samotný
/// (DWM ho při každém posunu počítá znovu) — změřit to jde jen na
/// skutečné ploše (skrytou plochu DWM neskládá). Pak zbývá okno bez
/// efektu s neprůhledným pozadím z CSS (otevřená otázka 19).
pub fn efekt_pozadi(build: u32) -> WindowEffectsConfig {
    if build >= WINDOWS_11 {
        WindowEffectsConfig {
            effects: vec![Effect::Mica],
            ..Default::default()
        }
    } else {
        WindowEffectsConfig {
            effects: vec![Effect::Blur],
            // #0e0f1299 — tón WinSentu (jeho tauri.conf.json).
            color: Some(Color(0x0e, 0x0f, 0x12, 0x99)),
            ..Default::default()
        }
    }
}

/// Nastaví pozadí okna podle systému (viz [`efekt_pozadi`]).
///
/// Tauri efekt aplikuje ve smyčce událostí — stejně jako dřív efekt
/// z tauri.conf.json, jen o pár řádků později ve frontě, pořád před
/// prvním snímkem. Chybu efektu Tauri zahodí, proto se Mica hned za
/// ním ve frontě zpětně přečte z DWM a výsledek jde do logu. Blur na
/// Windows 10 zpětně přečíst nejde (GetWindowCompositionAttribute
/// accent policy nevrací — ověřeno, chyba 87 i u vlastního okna);
/// ten hlídá test shody s dřívější konfigurací.
pub fn nastav_pozadi(w: &tauri::WebviewWindow) {
    let build = build_windows();
    let mica = build >= WINDOWS_11;
    let jmeno = if mica { "Mica" } else { "blur" };
    if let Err(e) = w.set_effects(efekt_pozadi(build)) {
        log::warn!("pozadí okna ({jmeno}) nejde nastavit: {e}");
        return;
    }
    if !mica {
        log::info!("pozadí okna: {jmeno} (Windows build {build})");
        return;
    }
    let okno = w.clone();
    let _ = w.run_on_main_thread(move || {
        let Ok(hwnd) = okno.hwnd() else {
            return;
        };
        match mica_je(hwnd) {
            Some((true, co)) => log::info!("pozadí okna: Mica (Windows build {build}, {co})"),
            Some((false, co)) => log::warn!(
                "pozadí okna: Mica se nenastavil (Windows build {build}, {co}) — okno bez materiálu"
            ),
            None => log::info!("pozadí okna: Mica (Windows build {build}, neověřeno)"),
        }
    });
}

/// Má okno Mica? Dokumentovaný atribut DWM; na buildech 22000–22522
/// ho window-vibrancy nastavuje nedokumentovaným 1029.
fn mica_je(hwnd: windows::Win32::Foundation::HWND) -> Option<(bool, String)> {
    use windows::Win32::Graphics::Dwm::{
        DwmGetWindowAttribute, DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWINDOWATTRIBUTE,
    };

    let cti = |atribut: DWMWINDOWATTRIBUTE| -> Option<i32> {
        let mut v = 0i32;
        // SAFETY: okno tohoto procesu, výstup je i32 správné velikosti.
        unsafe {
            DwmGetWindowAttribute(
                hwnd,
                atribut,
                (&raw mut v).cast(),
                std::mem::size_of::<i32>() as u32,
            )
        }
        .ok()
        .map(|()| v)
    };
    if let Some(v) = cti(DWMWA_SYSTEMBACKDROP_TYPE) {
        return Some((v == DWMSBT_MAINWINDOW.0, format!("backdrop {v}")));
    }
    // DWMWA_MICA_EFFECT (nedokumentovaný, 22000–22522).
    cti(DWMWINDOWATTRIBUTE(1029)).map(|v| (v != 0, format!("mica {v}")))
}

/// Vypne zkratky prohlížeče ve WebView2 (spec 1.8): F5 a Ctrl+R by
/// obnovily stránku, Ctrl+P otevřel tisk, Ctrl+F hledání… KeyPad je
/// aplikace, ne prohlížeč — a bez zapnutého ovladače jdou do okna
/// i herní klávesy. wry je nechává zapnuté (`browser_accelerator_keys`)
/// a Tauri 2.11 to nastavit neumí, proto přímo přes
/// `ICoreWebView2Settings3`.
///
/// Starší WebView2 Runtime bez `ICoreWebView2Settings3` → tiše nic
/// (zkratky zůstanou, nic se nerozbije).
pub fn vypni_zkratky_prohlizece(w: &tauri::WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3;
    use windows::core::Interface;

    // `with_webview` běží na hlavním vlákně, kde COM objekty WebView2
    // žijí.
    let r = w.with_webview(|pw| {
        // SAFETY: volání COM na vlákně, které webview vlastní; controller
        // drží Tauri po celou dobu života okna.
        let r = unsafe {
            pw.controller()
                .CoreWebView2()
                .and_then(|wv| wv.Settings())
                .and_then(|s| s.cast::<ICoreWebView2Settings3>())
                .and_then(|s| s.SetAreBrowserAcceleratorKeysEnabled(false))
        };
        match r {
            Ok(()) => log::debug!("WebView2: zkratky prohlížeče vypnuté"),
            Err(e) => log::debug!("WebView2: zkratky prohlížeče nejde vypnout ({e}) — nevadí"),
        }
    });
    if let Err(e) = r {
        log::debug!("WebView2: zkratky prohlížeče nejde vypnout ({e}) — nevadí");
    }
}

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

/// Zaoblené rohy přes DWM (jako WinSent).
///
/// Okno je bez dekorací a průhledné, takže rohy by jinak byly ostré.
/// Umí to až Windows 11 — na desítkách atribut neexistuje, volání tiše
/// selže a okno zůstane s ostrými rohy. Region jako náhrada (co dělá
/// WinSent u vyhledávací lišty) se tu schválně nepoužívá: okno se dá
/// zvětšovat a region by se musel přepočítávat při každé změně velikosti
/// i DPI, za cenu tvrdé nevyhlazené hrany.
///
/// Tmavý motiv okna (DWMWA_USE_IMMERSIVE_DARK_MODE) se tu schválně
/// NEnastavuje, drží ho `"theme": "Dark"` v tauri.conf.json. Materiál
/// Mica na Windows 11 si ve světlém motivu systému jinak vezme SVĚTLOU
/// variantu a okno pod poloprůhledným CSS vyjde bledé (WinSent to
/// opravoval dvakrát). Jednorázové volání DWM odsud nestačí: tao při
/// každém WM_SETTINGCHANGE (tapeta, prezentace pozadí, nastavení,
/// proměnné prostředí — chodí všem oknům nejvyšší úrovně) motiv okna
/// přepočítá podle systému a atribut přepíše — pokud okno nemá motiv
/// předepsaný. S předepsaným tmavým ho tao nastaví při vytvoření (na
/// každém buildu od 1809 správným atributem, 19 nebo 20) a pak ho nechá
/// být. Hlídá to test `motiv_okna_je_tmavy_z_konfigurace`.
pub fn zaobli(w: &tauri::WebviewWindow) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };

    let Ok(hwnd) = w.hwnd() else {
        return;
    };
    let rohy = DWMWCP_ROUND;
    // SAFETY: platný handle okna; ukazatel míří na hodnotu, která žije
    // po celou dobu volání, a velikost odpovídá jejímu typu.
    let r = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &rohy as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&rohy) as u32,
        )
    };
    if r.is_err() {
        log::debug!("DWM zaoblení rohů nejde (Windows 10?) — okno zůstane hranaté");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows 10: PŘESNĚ tentýž efekt, jaký dřív stál v tauri.conf.json
    /// (a jaký má hlavní okno WinSentu) — Tauri ho aplikuje stejnou
    /// cestou, takže se vzhled nemění. Blur zpětně přečíst z okna nejde,
    /// proto tahle shoda.
    #[test]
    fn windows_10_blur_jako_drive_a_jako_winsent() {
        let drive: WindowEffectsConfig =
            serde_json::from_str(r##"{ "effects": ["blur"], "color": "#0e0f1299" }"##).unwrap();
        for build in [10_240, 17_763, 19_045, 21_999] {
            let e = efekt_pozadi(build);
            assert_eq!(e, drive, "{build}");
            assert_eq!(e.effects, vec![Effect::Blur]);
            assert_eq!(e.color, Some(Color(0x0e, 0x0f, 0x12, 0x99)));
        }
    }

    #[test]
    fn windows_11_mica() {
        for build in [22_000, 22_621, 26_100] {
            let e = efekt_pozadi(build);
            assert_eq!(e.effects, vec![Effect::Mica], "{build}");
            assert_eq!(e.color, None);
        }
    }

    /// Hlavní okno má v tauri.conf.json předepsaný tmavý motiv (jinak by
    /// Mica na Windows 11 po prvním WM_SETTINGCHANGE zesvětlal — viz
    /// [`zaobli`]) a efekt pozadí tam NENÍ (vybírá ho [`efekt_pozadi`]
    /// podle buildu). Čte se týmž typem, jakým konfiguraci čte Tauri.
    #[test]
    fn motiv_okna_je_tmavy_z_konfigurace() {
        use tauri::utils::config::WindowConfig;
        use tauri::utils::Theme;

        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let okna: Vec<WindowConfig> =
            serde_json::from_value(conf["app"]["windows"].clone()).unwrap();
        let hlavni = okna.iter().find(|o| o.label == "main").expect("okno main");
        assert_eq!(hlavni.theme, Some(Theme::Dark));
        assert!(hlavni.window_effects.is_none());
        assert!(hlavni.transparent && !hlavni.decorations);
    }

    /// Na vývojovém PC (Windows 10/11) musí jít build přečíst.
    #[test]
    fn build_se_precte() {
        let b = build_windows();
        assert!(b >= 10_240, "{b}");
    }
}

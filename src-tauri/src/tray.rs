//! Oznamovací oblast a schovávání okna (stejně jako WinSent).
//!
//! Zavření okna (křížek, Alt+F4, WM_CLOSE) KeyPad neukončí, jen schová:
//! zapnutý virtuální ovladač má zůstat zapnutý (hraje se se schovaným
//! oknem, aby nepřekáželo streamu) a ve Fázi 3+ i hook. Ikona
//! v oznamovací oblasti je vidět pořád — nic neběží skrytě (princip 8).
//! Ukončit jde z její nabídky, na žádost instalátoru pojmenovanou
//! událostí (`platform::windows::ukonceni`) a při konci relace Windows
//! (`platform::windows::relace`).

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

const ID: &str = "keypad";

/// Povedlo se ikonu vytvořit? Bez ní by schované okno nešlo vrátit
/// (kromě druhého spuštění) a proces by běžel neviditelně — zavření
/// okna pak aplikaci radši opravdu ukončí.
static IKONA: AtomicBool = AtomicBool::new(false);

/// Ikona v oznamovací oblasti: levý klik otevře okno, pravý nabídku.
pub fn nastav(app: &tauri::App) -> tauri::Result<()> {
    let otevrit = MenuItem::with_id(app, "otevrit", "Otevřít KeyPad", true, None::<&str>)?;
    let ukoncit = MenuItem::with_id(app, "ukoncit", "Ukončit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&otevrit, &ukoncit])?;
    let mut stavba = TrayIconBuilder::with_id(ID)
        .tooltip("KeyPad")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, udalost| match udalost.id.as_ref() {
            "otevrit" => {
                ukaz_okno(app);
            }
            "ukoncit" => ukonci(app, "nabídka v oznamovací oblasti"),
            _ => {}
        })
        .on_tray_icon_event(|ikona, udalost| {
            // Levý klik = otevřít okno (pravý nechává nabídku).
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = udalost
            {
                ukaz_okno(ikona.app_handle());
            }
        });
    if let Some(obrazek) = app.default_window_icon() {
        stavba = stavba.icon(obrazek.clone());
    }
    stavba.build(app)?;
    IKONA.store(true, Ordering::Release);
    Ok(())
}

/// Má zavření okna jen schovávat? Jen když je kam okno schovat.
pub fn schovavat() -> bool {
    IKONA.load(Ordering::Acquire)
}

/// Popisek ikony (stav padu). Z libovolného vlákna — nečeká: práci
/// jen předá hlavnímu vláknu. Pad vlákno tu nesmí stát, hlavní vlákno
/// může zrovna čekat na něj (konec aplikace).
pub fn popisek(app: &AppHandle, text: String) {
    let a = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(ikona) = a.tray_by_id(ID) {
            let _ = ikona.set_tooltip(Some(text));
        }
    });
}

/// Ukáže, obnoví z minimalizace a vyzdvihne hlavní okno. Vrací `false`,
/// když hlavní okno není. Volat z hlavního vlákna.
pub fn ukaz_okno(app: &AppHandle) -> bool {
    let Some(w) = crate::hlavni_okno(app) else {
        return false;
    };
    // Nejdřív probudit webview, teprve pak ukázat okno — jinak by se
    // první snímek kreslil do něčeho, co má vypnutou kompozici.
    uspi_webview(&w, false);
    let _ = w.show();
    let _ = w.unminimize();
    crate::okno::do_popredi(&w);
    true
}

/// Schová okno do oznamovací oblasti a uspí webview.
pub fn schovej(w: &tauri::Window) {
    let _ = w.hide();
    if let Some(ww) = w.app_handle().get_webview_window(w.label()) {
        uspi_webview(&ww, true);
    }
}

/// Uspí nebo probudí webview okna.
///
/// Schování okna samo webview NEZASTAVÍ: Tauri sáhne jen na okno
/// Windows, WebView2 dál kreslí a tiká časovači (WinSent naměřil
/// schovanou aplikaci stejně drahou jako otevřenou). Skrytý webview si
/// Chromium utlumí sám a ve stránce začne platit `document.hidden`
/// — na to navazuje kontrola aktualizací (`visibilitychange`).
fn uspi_webview(w: &tauri::WebviewWindow, uspat: bool) {
    let v: &tauri::Webview = w.as_ref();
    let _ = if uspat { v.hide() } else { v.show() };
    cil_pameti(w, uspat);
}

/// Cíl spotřeby paměti WebView2: „málo" ve schovaném okně, „normální"
/// po návratu (princip 10).
///
/// Naměřeno (WebView2 154, KeyPad + 6 procesů msedgewebview2, skrytá
/// plocha): soukromá paměť se NEZMĚNÍ (~160 MB před i po), ale
/// pracovní sada — fyzická RAM, kterou ukazuje Správce úloh — spadne
/// z ~345 MB na ~50 MB za 30 s schování; za 2 min si Chromium část
/// stránek vezme zpátky (~120 MB), pořád o ~220 MB míň. CPU ve
/// schování v šumu měření (687 vs. 812 ms za 120 s celého stromu).
/// Stránka přitom běží dál (události `pad-stav`, kontrola aktualizací),
/// na rozdíl od `TrySuspend`, který by ji zmrazil. Zpátky na NORMAL to
/// samo nepřepne, proto i při ukázání. Starší WebView2 Runtime bez
/// ICoreWebView2_19 → tiše nic.
fn cil_pameti(w: &tauri::WebviewWindow, malo: bool) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW,
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
    };
    use windows::core::Interface;

    let uroven = if malo {
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
    } else {
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
    };
    // `with_webview` běží na hlavním vlákně (odtud rovnou), kde COM
    // objekty WebView2 žijí.
    let r = w.with_webview(move |pw| {
        // SAFETY: volání COM na vlákně, které webview vlastní; controller
        // drží Tauri po celou dobu života okna.
        let r = unsafe {
            pw.controller()
                .CoreWebView2()
                .and_then(|wv| wv.cast::<ICoreWebView2_19>())
                .and_then(|wv| wv.SetMemoryUsageTargetLevel(uroven))
        };
        if let Err(e) = r {
            log::debug!("WebView2: cíl paměti nejde nastavit ({e}) — nevadí");
        }
    });
    if let Err(e) = r {
        log::debug!("WebView2: cíl paměti nejde nastavit ({e}) — nevadí");
    }
}

/// Uklizený konec aplikace — jediná cesta pro nabídku i instalátor.
/// Pad a log dořeší `RunEvent::Exit` v `main`.
pub fn ukonci(app: &AppHandle, proc: &str) {
    log::info!("ukončuji ({proc})");
    app.exit(0);
}

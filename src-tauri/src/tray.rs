//! Oznamovací oblast a schovávání okna (stejně jako WinSent).
//!
//! Zavření okna (křížek, Alt+F4, WM_CLOSE) KeyPad neukončí, jen schová:
//! zapnutý virtuální ovladač má zůstat zapnutý (hraje se se schovaným
//! oknem, aby nepřekáželo streamu) a ve Fázi 3+ i hook. Ikona
//! v oznamovací oblasti je vidět pořád — nic neběží skrytě (princip 8).
//! Ukončit jde z její nabídky, na žádost instalátoru pojmenovanou
//! událostí (`platform::windows::ukonceni`) a při konci relace Windows
//! (`platform::windows::relace`).
//!
//! Fáze 4b: ikona ukazuje režim (hraje / pozastaveno / vypnuto /
//! porucha) a nabídka umí pozastavit a vypnout zvuk — okno je během
//! hry schované a přes hru se nic neukazuje (žádné balónky ani toasty,
//! princip 8).

mod ikona;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

pub use ikona::DruhIkony;

const ID: &str = "keypad";

/// Povedlo se ikonu vytvořit? Bez ní by schované okno nešlo vrátit
/// (kromě druhého spuštění) a proces by běžel neviditelně — zavření
/// okna pak aplikaci radši opravdu ukončí.
static IKONA: AtomicBool = AtomicBool::new(false);

/// Co ikona ukazuje (počítá `gamepad::znameni::stav_oblasti`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayStav {
    pub ikona: DruhIkony,
    /// Bublina (Windows ji utne na 127 znaků).
    pub popisek: String,
    /// Položka Pozastavit/Pokračovat: `Some(true)` = pozastaveno
    /// („Pokračovat“), `Some(false)` = hraje („Pozastavit“), `None` =
    /// zakázaná (bez hry, přiřazování, porucha).
    pub pauza: Option<bool>,
}

/// Položky nabídky, které se mění, a naposledy nastavený stav.
struct Oblast {
    pauza: MenuItem<Wry>,
    zvuk: CheckMenuItem<Wry>,
    /// Bere jen hlavní vlákno ([`stav`]); ikona se nastaví jen při změně
    /// (každé `set_icon` vyrábí nový HICON).
    posledni: Mutex<Option<TrayStav>>,
    /// Čtyři varianty ikony, poprvé až při potřebě; `None` = aplikace
    /// ikonu nemá a mění se jen bublina.
    varianty: OnceLock<Option<[Vec<u8>; 4]>>,
}

impl Oblast {
    fn obrazek(&self, app: &AppHandle, druh: DruhIkony) -> Option<Image<'_>> {
        let varianty = self.varianty.get_or_init(|| {
            let zaklad = app.default_window_icon()?;
            let (rgba, w, h) = (zaklad.rgba(), zaklad.width(), zaklad.height());
            Some(
                [
                    DruhIkony::Vypnuto,
                    DruhIkony::Hraje,
                    DruhIkony::Pauza,
                    DruhIkony::Pozor,
                ]
                .map(|d| ikona::varianta(rgba, w, h, d)),
            )
        });
        let i = match druh {
            DruhIkony::Vypnuto => 0,
            DruhIkony::Hraje => 1,
            DruhIkony::Pauza => 2,
            DruhIkony::Pozor => 3,
        };
        varianty
            .as_ref()
            .map(|v| Image::new(&v[i], ikona::STRANA, ikona::STRANA))
    }
}

/// Ikona v oznamovací oblasti: levý klik otevře okno, pravý nabídku.
pub fn nastav(app: &tauri::App) -> tauri::Result<()> {
    let otevrit = MenuItem::with_id(app, "otevrit", "Otevřít KeyPad", true, None::<&str>)?;
    // Bez zapnutého ovladače není co pozastavit — povolí ji až hra.
    let pauza = MenuItem::with_id(app, "pauza", TEXT_POZASTAVIT, false, None::<&str>)?;
    let zvuk = CheckMenuItem::with_id(
        app,
        "zvuk",
        "Zvuk",
        true,
        crate::gamepad::ZVUK_VYCHOZI,
        None::<&str>,
    )?;
    let oddelovac = PredefinedMenuItem::separator(app)?;
    let ukoncit = MenuItem::with_id(app, "ukoncit", "Ukončit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&otevrit, &pauza, &zvuk, &oddelovac, &ukoncit])?;
    app.manage(Oblast {
        pauza,
        zvuk,
        posledni: Mutex::new(None),
        varianty: OnceLock::new(),
    });
    let mut stavba = TrayIconBuilder::with_id(ID)
        .tooltip("KeyPad")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, udalost| match udalost.id.as_ref() {
            "otevrit" => {
                ukaz_okno(app);
            }
            "pauza" => crate::gamepad::prepni_z_nabidky(app),
            "zvuk" => {
                // Zaškrtnutí přepnula nabídka sama už před touhle událostí.
                if let Some(zap) = app
                    .try_state::<Oblast>()
                    .and_then(|o| o.zvuk.is_checked().ok())
                {
                    crate::gamepad::zvuk_z_nabidky(app, zap);
                }
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

const TEXT_POZASTAVIT: &str = "Pozastavit";
const TEXT_POKRACOVAT: &str = "Pokračovat";

/// Ikona, bublina a položka Pozastavit/Pokračovat podle režimu.
/// Z libovolného vlákna — nečeká: práci jen předá hlavnímu vláknu
/// (vlákno okna, které to volá, hlídá tep ovladačů a nesmí stát; hlavní
/// vlákno může zrovna čekat na pady při konci aplikace).
pub fn stav(app: &AppHandle, s: TrayStav) {
    let a = app.clone();
    let _ = app.run_on_main_thread(move || proved(&a, s));
}

/// [`stav`] na hlavním vlákně: mění jen to, co se opravdu změnilo.
fn proved(app: &AppHandle, s: TrayStav) {
    let (Some(ikona), Some(o)) = (app.tray_by_id(ID), app.try_state::<Oblast>()) else {
        return;
    };
    let mut posledni = o.posledni.lock().unwrap_or_else(|e| e.into_inner());
    let stary = posledni.as_ref();
    if stary.map(|p| p.ikona) != Some(s.ikona) {
        if let Some(obrazek) = o.obrazek(app, s.ikona) {
            if let Err(e) = ikona.set_icon(Some(obrazek)) {
                log::warn!("ikona v oznamovací oblasti: nejde změnit ({e})");
            }
        }
    }
    if stary.map(|p| p.popisek.as_str()) != Some(s.popisek.as_str()) {
        let _ = ikona.set_tooltip(Some(&s.popisek));
    }
    if stary.map(|p| p.pauza) != Some(s.pauza) {
        let text = if s.pauza == Some(true) {
            TEXT_POKRACOVAT
        } else {
            TEXT_POZASTAVIT
        };
        let _ = o.pauza.set_text(text);
        let _ = o.pauza.set_enabled(s.pauza.is_some());
    }
    *posledni = Some(s);
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

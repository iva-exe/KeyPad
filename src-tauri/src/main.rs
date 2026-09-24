//! KeyPad — vybrané klávesy jako virtuální Xbox 360 ovladač (Tauri 2).
//!
//! Fáze 0: okno ve stylu WinSentu, logger, panic hook a aktualizace.
//! Hook klávesnice a ViGEm přijdou ve Fázi 2+ (viz ROADMAP.md).

// Release bez konzolového okna. Ve vývoji konzole zůstává — je v ní
// vidět výchozí výpis paniky a výstup Tauri.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod logger;
mod okno;
mod update;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::Manager;

/// Odpověď příkazu `app_info` (patička okna).
#[derive(Debug, Clone, Serialize)]
struct AppInfo {
    /// Verze z `version.txt` vedle programu, u vývojového buildu verze
    /// z Cargo.toml s poznámkou „(vývoj)".
    version: String,
    /// Kam se loguje; prázdné = log nejde založit.
    log_path: String,
    /// Vývojový build (bez `version.txt`) — aktualizace se nenabízí.
    dev: bool,
}

/// Stav sdílený s příkazy. Počítá se jednou při startu: nic z toho se
/// za běhu nemění a patička se ptá při každém otevření okna.
struct Stav {
    info: AppInfo,
    log: Option<PathBuf>,
}

#[tauri::command]
fn app_info(stav: tauri::State<'_, Stav>) -> AppInfo {
    stav.info.clone()
}

/// Otevře Průzkumníka s označeným souborem logu.
///
/// `raw_arg`, ne `arg`: Rust by celý argument obalil uvozovkami
/// (`"/select,C:\…"`) a Průzkumník takový zápis nepozná — otevře
/// Dokumenty místo složky s logem. Uvozovky patří jen kolem cesty.
/// Návratový kód Průzkumníka se nekontroluje, vrací 1 i při úspěchu.
#[tauri::command(async)]
fn open_log_dir(stav: tauri::State<'_, Stav>) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    let Some(log) = &stav.log else {
        return Err("log se nepodařilo založit — ani vedle programu, ani v %APPDATA%".into());
    };
    std::process::Command::new(pruzkumnik())
        .raw_arg(format!("/select,\"{}\"", log.display()))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Průzkumník nejde otevřít: {e}"))
}

/// `explorer.exe` plnou cestou ze složky Windows.
///
/// Jen jménem by ho `Command` hledal NEJDŘÍV ve složce programu — a
/// KeyPad.exe jde spustit i ze Stažených souborů, kam může cizí
/// `explorer.exe` podstrčit kdejaká stránka; klik na „log" by ho pak
/// spustil. Stejná ochrana jako `proc::system32` v instalátoru, jen
/// Průzkumník bydlí ve `%SystemRoot%`, ne v System32.
fn pruzkumnik() -> PathBuf {
    std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("windir"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("explorer.exe")
}

/// Hlavní okno — ale jen když opravdu existuje i ve Windows.
///
/// Tauri si okno zaeviduje, i když se ho vytvořit nepovedlo (rozbitý
/// WebView2, složka dat, kterou nejde založit) — chybu jen zaloguje.
/// Jestli okno doopravdy vzniklo, prozradí až jeho handle.
fn hlavni_okno(app: &tauri::AppHandle) -> Option<tauri::WebviewWindow> {
    app.get_webview_window("main").filter(|w| w.hwnd().is_ok())
}

/// Jak dlouho po startu se chybějící hlavní okno bere jako „teprve se
/// vytváří". Dvojklik na zástupce = dvě spuštění těsně po sobě a první
/// instance v tu chvíli čeká, než se rozjede WebView2 (za studena i
/// sekundy). Ukončit ji by znamenalo, že se neukáže ani jedno okno.
const LHUTA_OKNA: Duration = Duration::from_secs(20);

/// Druhé spuštění, ale hlavní okno pořád není: skončit.
///
/// Proces bez okna nikdo nevidí, a přitom drží single-instance — každé
/// další spuštění by skončilo u něj a nic by se neukázalo. Druhé
/// spuštění už odešlo, ale to další naběhne načisto.
///
/// Nejdřív slušně (`exit` → `RunEvent::Exit`, dopsání logu). Když ale
/// hlavní vlákno visí ve vytváření okna (naměřeno: WebView2 ukáže
/// vlastní dialog „Nemohli jsme vytvořit datový adresář" a pak čeká
/// na svůj časový limit), k požadavku na konec se smyčka událostí
/// nedostane — proto pojistka z vedlejšího vlákna.
fn ukonci_bez_okna(app: &tauri::AppHandle) {
    log::error!("druhé spuštění, ale hlavní okno neexistuje — končím");
    app.exit(1);
    let _ = std::thread::Builder::new()
        .name("keypad-konec".into())
        .spawn(|| {
            std::thread::sleep(Duration::from_secs(3));
            log::error!("smyčka událostí na konec nereaguje — končím natvrdo");
            logger::flush(Duration::from_secs(1));
            std::process::exit(1);
        });
}

/// Ukáže, obnoví z minimalizace a vyzdvihne hlavní okno. Vrací `false`,
/// když hlavní okno není.
fn ukaz_okno(app: &tauri::AppHandle) -> bool {
    let Some(w) = hlavni_okno(app) else {
        return false;
    };
    let _ = w.show();
    let _ = w.unminimize();
    let _ = w.set_focus();
    true
}

/// Panika → řádek do logu, a to dřív, než proces skončí.
///
/// Release nemá konzoli, takže výchozí výpis paniky by šel do prázdna
/// a pád u kamaráda by nezanechal žádnou stopu. Proto se panika zapíše
/// do logu a (mimo realtime vlákno) se počká, až je na disku.
fn nainstaluj_panic_hook() {
    // Ve vývoji se výchozí výpis (s RUST_BACKTRACE i backtrace) hodí
    // do konzole dál.
    #[cfg(debug_assertions)]
    let vychozi = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |info| {
        let vlakno = std::thread::current();
        let jmeno = vlakno.name().unwrap_or("bez jména");
        let misto = info.location().map_or_else(
            || "neznámé místo".to_string(),
            |l| format!("{}:{}:{}", l.file(), l.line(), l.column()),
        );
        let obsah = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("(obsah paniky není text)");
        log::error!("PANIKA ve vlákně '{jmeno}' ({misto}): {obsah}");

        // Realtime vlákno (hook) nesmí čekat ani tady: panika v hooku
        // se chytá přes catch_unwind a klávesa se musí propustit hned.
        // Řádek je ve frontě a zapisovač ho uloží sám. Totéž platí pro
        // výchozí výpis ve vývoji: zápis do konzole stojí, dokud je v ní
        // označený text (QuickEdit), a s RUST_BACKTRACE se backtrace
        // symbolizuje stovky milisekund — hook by Windows mezitím odebraly.
        if !logger::is_realtime_thread() {
            logger::flush(Duration::from_millis(500));
            #[cfg(debug_assertions)]
            vychozi(info);
        }
    }));
}

/// Poslední možnost, jak říct, že se aplikace nespustila — release
/// nemá konzoli a bez okna by se jinak neukázalo vůbec nic.
fn hlaska_o_chybe(text: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    // SAFETY: řetězce žijí po celou dobu volání; bez vlastníka okna.
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from("KeyPad"),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// Aplikace se nespustí: řádek do logu, hláška a konec s kódem 1.
///
/// Proces MUSÍ skončit. Kdyby běžel dál bez okna, nikdo by ho neviděl,
/// a přitom by držel single-instance — každé další spuštění by jen
/// „ukázalo" jeho neexistující okno a skončilo (od Fáze 3 by navíc
/// držel hook klávesnice).
fn nespusteno(do_logu: &str, hlaska: &str) -> ! {
    log::error!("{do_logu}");
    logger::flush(Duration::from_secs(1));
    hlaska_o_chybe(hlaska);
    std::process::exit(1);
}

// Instalátor WebView2 Runtime od Microsoftu — týž odkaz jako
// v instalátoru a README (drží ho updater).
use updater::WEBVIEW2_URL;

/// Je WebView2 Runtime k dispozici? Když ne, skončí s českou hláškou.
///
/// Ptá se PŘED Tauri a týmž voláním jako Tauri samo
/// (`wry::webview_version`). Tauri by jinak ukázalo anglickou hlášku,
/// chybu vytvoření okna jen zalogovalo a proces nechalo běžet bez okna
/// — viz [`nespusteno`].
fn over_webview2() {
    match tauri::webview_version() {
        Ok(verze) => log::info!("WebView2 {verze}"),
        Err(e) => nespusteno(
            &format!("WebView2 Runtime není k dispozici: {e}"),
            &format!(
                "KeyPad potřebuje Microsoft Edge WebView2 Runtime — na tomhle \
                 počítači chybí, nebo je poškozený. Bez něj se okno KeyPadu \
                 neotevře.\n\nStáhni ho od Microsoftu (malý instalátor):\n\
                 {WEBVIEW2_URL}\n\nPo instalaci spusť KeyPad znovu.\n\n\
                 Ctrl+C zkopíruje celou tuhle zprávu i s adresou."
            ),
        ),
    }
}

fn main() {
    let spusteno = Instant::now();
    // Logger a panic hook PRVNÍ, ještě před Tauri: i pád při startu
    // (chybějící WebView2, rozbitá konfigurace) musí nechat stopu v logu.
    let cesta_logu = logger::init();
    nainstaluj_panic_hook();

    let verze = update::bezici_verze();
    let info = AppInfo {
        version: verze
            .clone()
            .unwrap_or_else(|| format!("{} (vývoj)", env!("CARGO_PKG_VERSION"))),
        log_path: cesta_logu
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        dev: verze.is_none(),
    };

    log::info!("start KeyPad {} (pid {})", info.version, std::process::id());
    if let Ok(exe) = std::env::current_exe() {
        log::info!("program: {}", exe.display());
    }
    log::info!(
        "log: {}",
        cesta_logu
            .as_ref()
            .map_or("—".into(), |p| p.display().to_string())
    );

    over_webview2();

    let aplikace = tauri::Builder::default()
        // MUSÍ být první plugin (vyžaduje dokumentace pluginu). Druhé
        // spuštění nevyrobí další proces — jen ukáže okno toho běžícího
        // a samo hned skončí. Dvě instance by se ve Fázi 3 praly
        // o klávesnici a o virtuální pad.
        .plugin(tauri_plugin_single_instance::init(
            move |app, _argv, _cwd| {
                if ukaz_okno(app) {
                    log::info!("druhé spuštění — ukazuji běžící okno");
                } else if spusteno.elapsed() < LHUTA_OKNA {
                    log::info!("druhé spuštění — hlavní okno se teprve vytváří");
                } else {
                    ukonci_bez_okna(app);
                }
            },
        ))
        .manage(Stav {
            info,
            log: cesta_logu,
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            open_log_dir,
            update::check_update,
            update::run_update
        ])
        .setup(|app| {
            // Okna z konfigurace Tauri vytváří těsně před tímhle voláním,
            // ve stejném vlákně a synchronně — tady už je jasné, jestli
            // vzniklo. Selhání (WebView2 je, ale nejde spustit) Tauri jen
            // zaloguje a běží dál. Naměřeno se složkou dat, kterou nejde
            // založit: WebView2 ukáže vlastní dialog, pak visí až do
            // svého časového limitu (minuty) a teprve potom se sem dojde.
            //
            // (V tauri.conf.json má okno `preventOverflow`: na malém
            // displeji s velkým měřítkem, třeba 1280×720 při 125 %, je
            // výška 620 víc než pracovní plocha a vycentrované okno by
            // mělo titulek nad horním okrajem obrazovky — a titulek je
            // jediné místo, za které jde okno bez rámečku chytit.)
            let Some(w) = hlavni_okno(app.handle()) else {
                nespusteno(
                    "hlavní okno se nepodařilo vytvořit (příčinu Tauri zapsalo o řádek výš)",
                    &format!(
                        "KeyPad se nepodařilo spustit — okno aplikace nejde vytvořit.\n\n\
                         Zkus to znovu. Když to nepomůže, přeinstaluj Microsoft Edge \
                         WebView2 Runtime:\n{WEBVIEW2_URL}\n\n\
                         Podrobnosti jsou v logu (keypad.log)."
                    ),
                );
            };
            okno::zaobli_a_ztmav(&w);
            // Sem přibude start hook a pad vláken (Fáze 2+). Jejich stav
            // (režim, pad, chyby) půjde do GUI Tauri událostmi přes
            // `app.emit(...)` — náhrada za egui `request_repaint` z ROADMAP.
            Ok(())
        })
        .build(tauri::generate_context!());

    let aplikace = match aplikace {
        Ok(a) => a,
        Err(e) => nespusteno(
            &format!("Tauri se nepodařilo spustit: {e}"),
            &format!("KeyPad se nepodařilo spustit.\n\n{e}\n\nPodrobnosti jsou v logu."),
        ),
    };

    aplikace.run(|_app, udalost| {
        // `run` se nevrací — smyčka událostí končí přímo ukončením
        // procesu. Exit je tedy poslední chvíle, kdy jde log dopsat.
        if let tauri::RunEvent::Exit = udalost {
            log::info!("konec");
            logger::flush(Duration::from_secs(1));
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn pruzkumnik_plnou_cestou() {
        // Jen jméno by `Command` hledal nejdřív ve složce programu.
        let p = super::pruzkumnik();
        assert!(p.is_absolute(), "{}", p.display());
        assert!(p.ends_with("explorer.exe"));
        assert!(p.is_file(), "{} neexistuje", p.display());
    }
}

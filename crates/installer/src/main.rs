//! KeyPadSetup — jediný soubor, který dostane kamarád.
//!
//! Spuštění bez parametrů = okno s tlačítkem Nainstalovat:
//!   1. zjistí z repozitáře aktuální verzi
//!   2. porovná ji s nainstalovanou (stejná + soubory na místě → nic
//!      se nestahuje, jen se KeyPad spustí)
//!   3. stáhne KeyPad.exe do paměti, zavře běžící KeyPad, přepíše ho
//!   4. udělá zástupce v nabídce Start a záznam v Nastavení → Aplikace
//!   5. když ovladač ViGEmBus úplně chybí a uživatel nechal zapnutý
//!      přepínač, nainstaluje i ten (viz `driver.rs`)
//!
//! `KeyPadSetup.exe /uninstall` = odeber všechno (ViGEmBus ne).
//! `KeyPadSetup.exe /quiet`     = okno, které se spustí i zavře samo
//!                                (tudy jde aktualizace z aplikace;
//!                                ovladač NIKDY neinstaluje); platí
//!                                i pro `/uninstall /quiet`.
//! `KeyPadSetup.exe /vigembus`  = jen ovladač ViGEmBus, po kliknutí
//!                                (spouští ho aplikace, když ovladač chybí;
//!                                `/quiet` se s ním ignoruje).
//! `KeyPadSetup.exe /headless`  = bez okna, výpis do konzole (skripty);
//!                                s `/vigembus` instaluje jen ovladač.
//!
//! Kód návratu `/vigembus` (okno i konzole) se skládá z ověřeného stavu:
//! 0 = ovladač běží, 3010 = je v systému a poběží po restartu, 1 = jinak.
//!
//! KeyPad se instaluje do profilu uživatele (`%LOCALAPPDATA%\Programs\
//! KeyPad`, HKCU, jeho nabídka Start) — instalátor sám nikdy neběží
//! s právy správce a manifest to říká výslovně (`asInvoker`). Jediná
//! výzva UAC, kterou kdy uvidí uživatel, patří oficiálnímu instalátoru
//! ViGEmBus: jen když ovladač chybí, jen po výslovném kliknutí a jen
//! s ověřeným otiskem a podpisem (`driver.rs`).
//!
//! Podsystém je „windows", ne „console": jinak by u grafického
//! instalátoru bliklo černé okno. V headless režimu se konzole rodiče
//! připojí ručně, aby výpis měl kam jít. (Testy běží jako konzolová
//! binárka, ať cargo vidí jejich výstup.)
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod driver;
mod gui;
mod prereq;
mod proc;
mod shell;

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use updater::vigembus::{self, BusState};
use updater::{APP_EXE, QUIT_EVENT_NAME, SETUP_ARG_VIGEMBUS, SETUP_EXE, VERSION_FILE};

/// Kroky instalace tak, jak je vidí uživatel v okně. Poslední krok
/// (ovladač) se ukáže, jen když se ovladač nabízí.
const INSTALL_STEPS: &[&str] = &[
    "Zjišťuji aktuální verzi",
    "Stahuji KeyPad",
    "Zavírám běžící KeyPad",
    "Zapisuji soubory",
    "Zástupce a záznam v systému",
];
const STEP_REGISTER: usize = 4;
/// Krok ovladače — za všemi kroky KeyPadu. Ovladač přichází až po
/// zápisu KeyPadu (jeho selhání nesmí instalaci KeyPadu shodit) a před
/// spuštěním KeyPadu (ať aplikace sběrnici najde hned při startu).
const DRIVER_STEP_LABEL: &str = "Ovladač ViGEmBus";
const STEP_DRIVER: usize = INSTALL_STEPS.len();

const UNINSTALL_STEPS: &[&str] = &[
    "Zavírám běžící KeyPad",
    "Mažu soubory aplikace",
    "Odebírám zástupce a záznam v systému",
];

/// Soubory, které vedle KeyPad.exe zakládá instalace nebo aplikace
/// a které odinstalace maže. `config.toml` mezi nimi schválně NENÍ —
/// je to uživatelovo mapování kláves a to se bez ptaní nemaže.
const SIDE_FILES: &[&str] = &[VERSION_FILE, "keypad.log", "keypad.old.log"];
const CONFIG_FILE: &str = "config.toml";

/// Hlášení průběhu. Okno i konzole dostávají totéž — jen to jinak
/// ukazují, takže se logika instalace nemusí ptát, kde zrovna běží.
trait Report {
    /// Začal krok `idx`, popisek pod pruhem je `status`.
    fn step(&mut self, idx: usize, status: &str);
    /// Jen změna řádku pod pruhem.
    fn status(&mut self, status: &str);
    /// Průběh stahování (počet dosud stažených bajtů).
    fn download(&mut self, name: &str, bytes: usize);
    /// 0.0–1.0, nebo `None` pro neurčitý pruh.
    fn progress(&mut self, p: Option<f32>);
    /// Okno, nad kterým se má ukázat výzva UAC (0 = žádné).
    fn owner(&self) -> isize;
}

/// Hlášení do okna.
struct GuiReport {
    state: gui::Shared,
    note: gui::Notifier,
}

impl GuiReport {
    fn with(&mut self, f: impl FnOnce(&mut gui::State)) {
        if let Ok(mut s) = self.state.lock() {
            f(&mut s);
        }
        self.note.tick();
    }
}

impl Report for GuiReport {
    fn step(&mut self, idx: usize, status: &str) {
        self.with(|s| s.step(idx, status));
    }
    fn status(&mut self, status: &str) {
        self.with(|s| s.status = status.into());
    }
    fn download(&mut self, name: &str, bytes: usize) {
        self.with(|s| s.status = format!("{name} — staženo {}", mb(bytes)));
    }
    fn progress(&mut self, p: Option<f32>) {
        self.with(|s| s.progress = p);
    }
    fn owner(&self) -> isize {
        self.note.hwnd()
    }
}

/// Hlášení do konzole (headless).
struct ConsoleReport;

impl Report for ConsoleReport {
    fn step(&mut self, idx: usize, status: &str) {
        println!("  [{}] {}", idx + 1, status);
    }
    fn status(&mut self, status: &str) {
        println!("      {status}");
    }
    /// Průběžné bajty by v konzoli udělaly desítky řádků; stačí
    /// výsledná velikost, kterou hlásí `status` po stažení.
    fn download(&mut self, _name: &str, _bytes: usize) {}
    fn progress(&mut self, _p: Option<f32>) {}
    /// Konzole, ze které nás spustili — výzva UAC se ukáže nad ní.
    fn owner(&self) -> isize {
        use windows::Win32::System::Console::GetConsoleWindow;
        // SAFETY: jen dotaz na okno připojené konzole (může být NULL).
        unsafe { GetConsoleWindow().0 as isize }
    }
}

/// Úspěšný konec — co ukázat a jestli po uživateli ještě něco chceme.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Done {
    message: String,
    /// Uživatel musí něco udělat nebo vědět (chybí WebView2, KeyPad se
    /// nepodařilo spustit, ovladač se nenainstaloval, poznámka „Pozor:")
    /// — okno se samo nezavře ani v tichém režimu.
    attention: bool,
    /// Co nabídne hlavní tlačítko (stažení, znovu ovladač).
    next: Option<gui::Next>,
}

impl Done {
    fn plain(message: impl Into<String>) -> Self {
        Done {
            message: message.into(),
            attention: false,
            next: None,
        }
    }
}

// ── Parametry ──────────────────────────────────────────────────────

/// Co má instalátor dělat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Install,
    Uninstall,
    /// Jen ovladač ViGEmBus (`/vigembus`).
    Driver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Args {
    mode: Mode,
    headless: bool,
    /// Tichý režim = okno se spustí i zavře samo (aktualizace z aplikace,
    /// skripty). Platí pro instalaci i odinstalaci jako od první verze —
    /// ovladače se netýká ani jedna (instalace ho v tichém režimu nenabízí,
    /// odinstalace na něj nesahá nikdy). `/vigembus` ho ignoruje schválně:
    /// ovladač se instaluje vždy až po kliknutí v okně.
    quiet: bool,
}

/// Parametry příkazové řádky (bez jména programu). Odinstalace má
/// přednost před ovladačem, ovladač před instalací — nejopatrnější
/// výklad nejasné kombinace je ten, který nic neinstaluje navíc.
fn parse_args(args: &[String]) -> Args {
    let args: Vec<String> = args.iter().map(|a| a.to_ascii_lowercase()).collect();
    let has = |names: &[&str]| args.iter().any(|a| names.contains(&a.as_str()));
    let mode = if has(&["/uninstall", "--uninstall", "/u"]) {
        Mode::Uninstall
    } else if has(&[SETUP_ARG_VIGEMBUS, "--vigembus"]) {
        Mode::Driver
    } else {
        Mode::Install
    };
    Args {
        mode,
        headless: has(&["/headless", "--headless"]),
        quiet: mode != Mode::Driver && has(&["/quiet", "--quiet", "/q", "/s", "/silent"]),
    }
}

/// Knihovny načítané ZA BĚHU jen ze System32, pro celý proces.
///
/// `/DEPENDENTLOADFLAG:0x800` (build.rs) chrání jen statické importy
/// samotného .exe. Systémové DLL si ale další knihovny načítají samy
/// až za běhu — a ty se bez tohohle hledají nejdřív ve složce s .exe,
/// tedy ve Stažených souborech: WinVerifyTrust tahá CRYPTSP.dll
/// a CRYPTBASE.dll, SHGetKnownFolderPath profapi.dll, WinHttp přes HTTPS
/// IPHLPAPI.DLL. Ověřeno při review podstrčenou DLL vedle instalátoru:
/// načetla se uvnitř ověřování podpisu, tedy v procesu, který vzápětí
/// žádá Windows o spuštění instalátoru ovladače s právy správce — mohla
/// mu změnit parametry a výzva UAC by pořád ukazovala „Nefarius".
/// Funkce je v kernel32 od Windows 8, takže statický import na 1507 nevadí.
fn harden_dll_search() -> windows::core::Result<()> {
    use windows::Win32::System::LibraryLoader::{
        SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_SYSTEM32,
    };
    // SAFETY: jen změní pořadí hledání DLL pro tento proces.
    unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) }
}

/// Hledání DLL nejde omezit — instalátor dál nepokračuje (na Windows 10
/// a 11 se to nestane; kdyby ano, je bezpečnější skončit než pokračovat
/// ke stahování a výzvě UAC). Zpráva je poslední, co proces udělá.
fn refuse_to_start(headless: bool, e: &windows::core::Error) -> ! {
    let msg = format!(
        "KeyPadSetup se nespustí: Windows nedovolily načítat knihovny jen ze systémové \
         složky ({}). Bez toho by si instalátor mohl načíst podvrženou knihovnu ze složky, \
         ve které leží.",
        e.message()
    );
    if headless {
        attach_console();
        println!("\n  CHYBA: {msg}");
    } else {
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        // SAFETY: řetězce žijí po celé volání; okno bez vlastníka.
        unsafe {
            MessageBoxW(
                None,
                &windows::core::HSTRING::from(msg),
                windows::core::w!("KeyPad — instalace"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    std::process::exit(1)
}

// ── Stav ovladače pro okno ─────────────────────────────────────────

/// Stav ViGEmBus, podle kterého se okno rozhodne, co nabídne.
///
/// Ladicí build umí stav předstírat přes proměnnou
/// `KEYPAD_SETUP_TEST_VIGEMBUS` (`chybi`, `bezi`, `vypnuty`, `blokovany`,
/// `restart`, `bez-zarizeni`, `bez-zaznamu`) — kvůli snímkům obrazovky
/// na PC, kde ViGEmBus je. Release build proměnnou vůbec nečte. A ani
/// v ladicím buildu se podle ní nic neinstaluje: pojistka
/// v `driver::install` čte vždy skutečný stav, takže na PC s ovladačem
/// skončí „už běží".
fn shown_bus_state() -> BusState {
    #[cfg(debug_assertions)]
    if let Some(s) = std::env::var("KEYPAD_SETUP_TEST_VIGEMBUS")
        .ok()
        .and_then(|v| fake_bus_state(&v))
    {
        return s;
    }
    vigembus::state()
}

#[cfg(debug_assertions)]
fn fake_bus_state(v: &str) -> Option<BusState> {
    use vigembus::DeviceStatus;
    let dev = |problem, need_restart| {
        Some(DeviceStatus {
            problem,
            need_restart,
            started: false,
        })
    };
    let there = |device, in_apps| BusState::InstalledNotRunning { device, in_apps };
    Some(match v {
        "chybi" => BusState::NotInstalled,
        "bezi" => BusState::Ready,
        "vypnuty" => there(dev(Some(22), false), true),
        "blokovany" => there(dev(Some(48), false), true),
        "restart" => there(dev(None, true), true),
        "bez-zarizeni" => there(None, true),
        // Zbytek bez záznamu v Aplikacích (devcon, nefcon, jiný program).
        "bez-zaznamu" => there(None, false),
        _ => return None,
    })
}

/// Text přepínače ovladače na úvodní obrazovce.
///
/// Přepínač je ZAPNUTÝ, když ovladač chybí — rozhodnutí vlastníka:
/// kamarád nemá nic stahovat a hledat zvlášť. Souhlas je pak v tom, že
/// přepínač i poznámka pod ním jsou vidět před kliknutím na
/// Nainstalovat, a ve výzvě Windows, kterou může odmítnout (instalace
/// KeyPadu se tím nezkazí). Rozhodnutí je zapsané v ROADMAP.
const DRIVER_TOGGLE: &str =
    "Nainstalovat i ovladač ViGEmBus — potřebný pro gamepad; Windows se zeptá na povolení správce";

/// Co se stane, když přepínač necháš zapnutý — předem a bez zamlčování
/// (princip 8): čí ovladač, pod jakou licencí, odkud, jak ověřený a čí
/// je výzva Windows.
fn driver_toggle_note() -> String {
    format!(
        "Nainstaluje se oficiální ViGEmBus {} od {} (licence {}) z GitHubu — před spuštěním \
         ověřím otisk SHA-256 i podpis. Výzva Windows k povolení správce patří jeho instalátoru; \
         KeyPad sám práva správce nikdy nemá.",
        vigembus::VERSION,
        vigembus::SIGNER,
        vigembus::LICENSE
    )
}

/// Vysvětlení na úvodní obrazovce režimu `/vigembus`.
fn driver_intro() -> String {
    format!(
        "KeyPad potřebuje ovladač ViGEmBus — virtuální gamepad od {} (licence {}). Stáhnu \
         jeho oficiální instalátor {} z GitHubu, ověřím, že je to bajt po bajtu ten správný \
         soubor (otisk SHA-256 a podpis), a spustím ho.\nWindows se zeptá na povolení \
         správce — patří instalátoru ovladače, KeyPad sám práva správce nikdy nemá.",
        vigembus::SIGNER,
        vigembus::LICENSE,
        vigembus::VERSION
    )
}

fn main() {
    // Úplně první příkaz — dřív, než cokoli (konzole, cesty, síť, okno)
    // stihne za běhu načíst systémovou DLL, která si tahá další.
    let hardened = harden_dll_search();
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = parse_args(&raw);
    if let Err(e) = hardened {
        refuse_to_start(args.headless, &e);
    }

    // Ladicí sonda k testu podstrčených DLL (jen debug build): projde
    // volání, při kterých se za běhu načítají další knihovny, a skončí.
    // Nic neinstaluje ani nespouští.
    #[cfg(debug_assertions)]
    if let Some(setup) = std::env::var_os("KEYPAD_SETUP_TEST_KNIHOVNY") {
        attach_console();
        let r = driver::load_probe(Path::new(&setup)).and_then(|m| {
            updater::latest_commit().map(|sha| format!("{m}; HTTPS v pořádku (commit {sha})"))
        });
        match r {
            Ok(m) => {
                println!("  sonda: {m}");
                std::process::exit(0);
            }
            Err(e) => {
                println!("  sonda: CHYBA {e}");
                std::process::exit(1);
            }
        }
    }

    // Běží instalátor z instalační složky (odinstalace z Nastavení →
    // Aplikace)? Pak se sám smazat nemůže — viz `schedule_self_delete`.
    let dir = updater::install_dir();
    let own_copy = dir.join(SETUP_EXE);
    let running_from_install = std::env::current_exe()
        .map(|me| proc::same_file(&me, &own_copy))
        .unwrap_or(false);

    if args.headless {
        // Podsystém je „windows", takže vlastní konzoli nemáme —
        // připojíme se k té, ze které nás spustili.
        attach_console();
        println!(
            "  KeyPad — {}",
            match args.mode {
                Mode::Install => "instalace",
                Mode::Uninstall => "odinstalace",
                Mode::Driver => "ovladač ViGEmBus",
            }
        );
        let mut rep = ConsoleReport;
        if args.mode == Mode::Driver {
            // Headless /vigembus je výslovný příkaz — to je to kliknutí.
            let o = driver::install(&mut rep, 0);
            println!("\n  {}", o.message().replace('\n', "\n  "));
            std::process::exit(o.exit_code());
        }
        let r = match args.mode {
            Mode::Uninstall => do_uninstall(&mut rep),
            // Skript nikdy neinstaluje ovladač sám od sebe — jen poradí
            // příkaz, kterým to uživatel udělá výslovně.
            _ => do_install(&mut rep, false, Missing::Hint),
        };
        match r {
            Ok(done) => {
                println!("\n  {}", done.message.replace('\n', "\n  "));
                if args.mode == Mode::Uninstall && running_from_install {
                    schedule_self_delete(&own_copy, &dir);
                }
                std::process::exit(0);
            }
            Err(e) => {
                println!("\n  CHYBA: {}", e.replace('\n', "\n  "));
                std::process::exit(1);
            }
        }
    }

    // Ladicí náhled hotových obrazovek (jen debug build) — kvůli
    // kontrole rozvržení nejdelších zpráv bez skutečné instalace.
    #[cfg(debug_assertions)]
    if let Ok(which) = std::env::var("KEYPAD_SETUP_TEST_NAHLED") {
        preview(&which);
        return;
    }

    let bus = shown_bus_state();
    // Ovladač se nabízí jen tam, kde úplně chybí, a nikdy v tichém
    // režimu (aktualizace z aplikace).
    let offer = args.mode == Mode::Install && bus == BusState::NotInstalled && !args.quiet;
    let state = match args.mode {
        Mode::Uninstall => {
            // Poctivý výčet toho, co zmizí — i logy, o které README žádá
            // při hlášení chyby, a data okna. Patička má místo na dva
            // řádky, proto je výčet v podtitulku a patička říká, co zůstane.
            gui::State::new(
                "Odebrat KeyPad",
                "smaže aplikaci, zástupce, záznam v Aplikacích, logy a data okna (WebView2)",
                if bus == BusState::NotInstalled {
                    "Tvoje nastavení (config.toml) zůstane."
                } else {
                    "Tvoje nastavení (config.toml) i ovladač ViGEmBus zůstanou."
                },
                UNINSTALL_STEPS,
                "Odebrat",
            )
        }
        Mode::Driver => driver_screen(bus),
        Mode::Install => {
            let mut steps = INSTALL_STEPS.to_vec();
            if offer {
                steps.push(DRIVER_STEP_LABEL);
            }
            let st = gui::State::new(
                "KeyPad",
                "klávesnice jako Xbox ovladač",
                "KeyPad se nainstaluje do tvého profilu — bez práv správce.",
                &steps,
                "Nainstalovat",
            );
            if offer {
                st.with_option(gui::Toggle {
                    label: DRIVER_TOGGLE.into(),
                    note: driver_toggle_note(),
                    on: true,
                    step: STEP_DRIVER,
                })
            } else {
                st
            }
        }
    };
    let state: gui::Shared = Arc::new(Mutex::new(state));

    // Povedla se odinstalace? Až pak se smí po zavření okna smazat
    // i kopie instalátoru — po nepovedené by nebylo čím to zkusit znovu.
    let succeeded = Arc::new(AtomicBool::new(false));
    // Kód návratu posledního pokusu o ovladač (režim /vigembus);
    // NO_ATTEMPT = žádný pokus neproběhl.
    let driver_code = Arc::new(AtomicI32::new(NO_ATTEMPT));
    let (flag, code) = (Arc::clone(&succeeded), Arc::clone(&driver_code));
    let mode = args.mode;
    // Tlačítko „Nainstalovat ovladač" jen tam, kde okno krok ovladače
    // ukazuje; jinak (tichý režim, ovladač při startu nechyběl) odkaz.
    let missing = if offer {
        Missing::Button
    } else {
        Missing::Link
    };
    let action: gui::Action = Arc::new(move |job, st: gui::Shared, note: gui::Notifier| {
        let mut rep = GuiReport {
            state: Arc::clone(&st),
            note,
        };
        let r = match (mode, job) {
            (Mode::Uninstall, _) => do_uninstall(&mut rep),
            (Mode::Install, gui::Job::Main) => {
                let with_driver = st.lock().map(|s| s.option_on()).unwrap_or(false);
                do_install(&mut rep, with_driver, missing)
            }
            // Ovladač sám: v režimu /vigembus, nebo „Zkusit znovu
            // ovladač" po instalaci KeyPadu.
            (Mode::Driver, _) | (Mode::Install, gui::Job::Driver) => {
                let step = if mode == Mode::Driver { 0 } else { STEP_DRIVER };
                let o = driver::install(&mut rep, step);
                code.store(o.exit_code(), Ordering::SeqCst);
                Ok(driver_done(&o))
            }
        };
        if r.is_ok() {
            flag.store(true, Ordering::SeqCst);
        }
        if let Ok(mut s) = st.lock() {
            match r {
                Ok(done) => s.finish(&done.message, done.attention, done.next),
                Err(e) => s.fail(&e),
            }
        }
        note.tick();
    });

    let title = match args.mode {
        Mode::Install => "KeyPad — instalace",
        Mode::Uninstall => "KeyPad — odinstalace",
        Mode::Driver => "KeyPad — ovladač ViGEmBus",
    };
    gui::run(title, state, action, args.quiet, args.quiet);

    match args.mode {
        Mode::Uninstall if running_from_install && succeeded.load(Ordering::SeqCst) => {
            schedule_self_delete(&own_copy, &dir);
        }
        // Aplikace, která /vigembus spustila, podle kódu pozná, jestli má
        // gamepad zkusit připojit znovu.
        Mode::Driver => {
            let last = driver_code.load(Ordering::SeqCst);
            std::process::exit(driver_exit_code(
                vigembus::state(),
                (last != NO_ATTEMPT).then_some(last),
            ));
        }
        _ => {}
    }
}

/// „Žádný pokus o ovladač neproběhl" v `driver_code`.
const NO_ATTEMPT: i32 = -1;

/// Kód návratu okna `/vigembus` — stejná pravidla jako headless
/// (`Outcome::exit_code`), jen se na konci čte skutečný stav znovu:
/// běžící ovladač je 0 i bez kliknutí (nebo když ho mezitím nainstaloval
/// někdo jiný). Jinak výsledek posledního pokusu, a bez pokusu to, co
/// by řekla pojistka (je a čeká na restart → 3010, jinak 1).
fn driver_exit_code(now: BusState, last_attempt: Option<i32>) -> i32 {
    match driver::precheck(now) {
        Some(o) if o.is_ready() => 0,
        pre => last_attempt.unwrap_or_else(|| pre.map_or(1, |o| o.exit_code())),
    }
}

/// Úvodní obrazovka režimu `/vigembus` podle stavu ovladače.
fn driver_screen(bus: BusState) -> gui::State {
    let footer = "Stahuje se jen z github.com/nefarius; jiný soubor se nespustí.";
    match bus {
        BusState::NotInstalled => {
            let mut st = gui::State::new(
                "Ovladač ViGEmBus",
                "virtuální Xbox ovladač pro KeyPad",
                footer,
                &[DRIVER_STEP_LABEL],
                "Nainstalovat ViGEmBus",
            );
            st.message = driver_intro();
            st
        }
        // Ovladač běží, nebo je a neběží — instalovat se nic nebude,
        // okno jen řekne, jak to je a co s tím (stejná pojistka jako
        // v `driver::install`). Kroky žádné.
        other => {
            let mut st = gui::State::new(
                "Ovladač ViGEmBus",
                "virtuální Xbox ovladač pro KeyPad",
                "",
                &[],
                "",
            );
            match driver::precheck(other) {
                Some(o) if !o.is_ready() => st.finish(&o.message(), true, driver_next(&o)),
                _ => st.finish(
                    "Ovladač ViGEmBus je nainstalovaný a běží — není co dělat.",
                    false,
                    None,
                ),
            }
            // Nic neproběhlo — plný pruh průběhu by lhal.
            st.progress = None;
            st
        }
    }
}

/// Závěr samotné instalace ovladače (`/vigembus`, „Zkusit znovu ovladač").
fn driver_done(o: &driver::Outcome) -> Done {
    let mut message = o.message();
    if o == &driver::Outcome::Installed {
        message.push_str(" Jestli máš KeyPad otevřený, klikni v něm na „Zkusit znovu“.");
    }
    Done {
        message,
        attention: !o.is_ready(),
        next: driver_next(o),
    }
}

/// Tlačítko po nepovedené instalaci ovladače: znovu, kde to má smysl
/// (zrušená výzva, síť, zaneprázdněný instalátor Windows, smazaná
/// složka rozbalování), jinak odkaz na ruční instalaci (soubor není ten
/// oficiální, neznámý kód, ovladač bez zařízení a bez záznamu
/// v Aplikacích).
fn driver_next(o: &driver::Outcome) -> Option<gui::Next> {
    match o {
        o if o.retry_makes_sense() => Some(gui::Next::Driver("Zkusit znovu ovladač".into())),
        driver::Outcome::Failed { .. } => Some(gui::Next::Link(vigembus_link())),
        driver::Outcome::NotRunning { advice, .. } => advice_next(advice),
        _ => None,
    }
}

/// Odkaz k radě, která vede na ruční instalaci; jinak nic.
fn advice_next(advice: &vigembus::Advice) -> Option<gui::Next> {
    advice
        .suggests_manual_install()
        .then(|| gui::Next::Link(vigembus_link()))
}

fn vigembus_link() -> gui::Link {
    gui::Link {
        label: "Stáhnout ViGEmBus".into(),
        url: vigembus::RELEASES_URL.into(),
    }
}

/// Ladicí náhled hotových obrazovek: `KEYPAD_SETUP_TEST_NAHLED` =
/// `nejdelsi` | `odmitnuta-slozka` | `preteceni` | `ovladac-zrusen` |
/// `ovladac-vynechan` | `ovladac-hotovo` | `prepinac-vypnuty` |
/// `odinstalovano`. Nic se
/// neinstaluje ani nemaže — jen se ukáže okno s výsledkem, jaký by
/// složila skutečná instalace. `preteceni` = nejdelší zpráva a k ní
/// všechny tři poznámky „Pozor:" (profil bez práva zápisu) — kontrola,
/// že okno přejde na menší písmo místo useknutí (`gui::Fit`).
#[cfg(debug_assertions)]
fn preview(which: &str) {
    let action: gui::Action = Arc::new(|_, _, _| {});
    if which == "prepinac-vypnuty" {
        // Úvodní obrazovka s vypnutým přepínačem (bez klikání do okna).
        let st = gui::State::new(
            "KeyPad",
            "klávesnice jako Xbox ovladač",
            "KeyPad se nainstaluje do tvého profilu — bez práv správce.",
            &INSTALL_STEPS
                .iter()
                .copied()
                .chain([DRIVER_STEP_LABEL])
                .collect::<Vec<_>>(),
            "Nainstalovat",
        )
        .with_option(gui::Toggle {
            label: DRIVER_TOGGLE.into(),
            note: driver_toggle_note(),
            on: false,
            step: STEP_DRIVER,
        });
        gui::run(
            "KeyPad — náhled",
            Arc::new(Mutex::new(st)),
            action,
            false,
            false,
        );
        return;
    }
    let h = PREVIEW_HEADLINE;
    let (title, with_driver_step, done) = match which {
        "nejdelsi" => ("KeyPad", true, longest_done(h)),
        // Kombinace z review: chybí WebView2 + odmítnutá složka
        // rozbalování + „Pozor:" (dřív se useknul konec).
        "odmitnuta-slozka" => {
            let refused = driver::sample_outcomes()
                .into_iter()
                .find(|o| o.message().contains("už existuje"))
                .expect("odmítnutá složka mezi ukázkami");
            let notes = [setup_copy_note("Přístup byl odepřen. (os error 5)")];
            (
                "KeyPad",
                true,
                outcome(h, false, None, &Driver::Step(refused), &notes),
            )
        }
        "preteceni" => {
            let mut d = longest_done(h);
            let denied = "Přístup byl odepřen. (0x80070005)";
            for n in [
                shortcut_note(&format!("zástupce nelze uložit: {denied}")),
                arp_note("nelze zapsat do registru: WIN32_ERROR(5)"),
            ] {
                d.message.push_str("\nPozor: ");
                d.message.push_str(&n);
            }
            ("KeyPad", true, d)
        }
        "ovladac-zrusen" => (
            "KeyPad",
            true,
            outcome(
                h,
                true,
                Some(Ok(())),
                &Driver::Step(driver::Outcome::Cancelled),
                &[],
            ),
        ),
        "ovladac-vynechan" => (
            "KeyPad",
            true,
            outcome(
                h,
                true,
                Some(Ok(())),
                &Driver::Missing(Missing::Button),
                &[],
            ),
        ),
        "ovladac-hotovo" => (
            "Ovladač ViGEmBus",
            false,
            driver_done(&driver::Outcome::Installed),
        ),
        _ => (
            "Odebrat KeyPad",
            false,
            Done::plain(uninstall_message(true, None, None, BusLeft::InApps)),
        ),
    };
    let labels: Vec<&str> = match (title, with_driver_step) {
        ("Odebrat KeyPad", _) => UNINSTALL_STEPS.to_vec(),
        ("Ovladač ViGEmBus", _) => vec![DRIVER_STEP_LABEL],
        (_, true) => INSTALL_STEPS
            .iter()
            .copied()
            .chain([DRIVER_STEP_LABEL])
            .collect(),
        _ => INSTALL_STEPS.to_vec(),
    };
    let mut st = gui::State::new(title, "náhled (ladicí build)", "", &labels, "");
    if which == "ovladac-vynechan" {
        // Jako ve skutečnosti: vypnutý přepínač nechá krok vynechaný.
        st = st.with_option(gui::Toggle {
            label: DRIVER_TOGGLE.into(),
            note: String::new(),
            on: false,
            step: STEP_DRIVER,
        });
    }
    st.finish(&done.message, done.attention, done.next);
    gui::run(
        "KeyPad — náhled",
        Arc::new(Mutex::new(st)),
        action,
        false,
        false,
    );
}

/// Nadpis ladicích náhledů a testů délky zprávy (skutečně dlouhá verze).
#[cfg(any(test, debug_assertions))]
const PREVIEW_HEADLINE: &str = "KeyPad 0.1.0+20260929.1200 je nainstalovaný";

/// Nejdelší skutečná závěrečná zpráva instalace: chybí WebView2,
/// nejdelší možný výsledek kroku ovladače (všechny skutečné varianty
/// z `driver::sample_outcomes`, změřené písmem okna) a poznámka „Pozor:"
/// se skutečnou chybou Windows. Pro ladicí náhled `nejdelsi` i test, že
/// se vejde do okna.
#[cfg(any(test, debug_assertions))]
fn longest_done(headline: &str) -> Done {
    let notes = [setup_copy_note("Přístup byl odepřen. (os error 5)")];
    driver::sample_outcomes()
        .into_iter()
        .map(|o| outcome(headline, false, None, &Driver::Step(o), &notes))
        .max_by_key(|d| {
            (
                gui::message_height_96(&d.message, INSTALL_STEPS.len() + 1),
                d.message.len(),
            )
        })
        .expect("aspoň jeden výsledek")
}

/// Připojí konzoli rodiče, aby měl headless výpis kam jít.
fn attach_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: selhání (spuštěno bez konzole) se ignoruje — výpis pak
    // prostě nikam nejde, což je u headless z plochy v pořádku.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

// ── Rozhodnutí: co instalace udělá ─────────────────────────────────

/// Co je potřeba udělat vzhledem k tomu, co je nainstalované.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Plan {
    /// Stejná verze a KeyPad.exe na místě — nic se nestahuje.
    UpToDate,
    /// Stejná verze, ale KeyPad.exe chybí (smazaný, antivir…).
    Repair,
    /// Jiná verze, než je v repozitáři.
    Update { from: String, to: String },
    /// Nic nainstalováno (nebo chybí záznam verze).
    Fresh,
}

/// Čisté rozhodnutí podle nainstalované verze, verze v repozitáři
/// a toho, jestli je binárka na disku.
///
/// Shodná verze sama o sobě neznamená, že aplikace funguje: binárka
/// mohla zmizet. Za „nainstalováno" se proto považuje až verze + soubor.
/// A naopak: chybějící `version.txt` u existující binárky znamená
/// přerušený zápis (verze se píše poslední) — to je čistá instalace.
///
/// Porovnává se na rovnost, ne na „vyšší": verze je
/// `0.1.0+RRRRMMDD.HHMM` a vydavatel může vydat i starší build zpátky —
/// i to je změna, kterou má instalátor provést.
fn plan(installed: Option<&str>, remote: &str, files_ok: bool) -> Plan {
    let remote = remote.trim();
    match installed.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) if v == remote && files_ok => Plan::UpToDate,
        Some(v) if v == remote => Plan::Repair,
        Some(v) => Plan::Update {
            from: v.to_string(),
            to: remote.to_string(),
        },
        None => Plan::Fresh,
    }
}

/// Jak zní úspěch pro daný plán („KeyPad … je …").
fn headline(plan: &Plan, version: &str) -> String {
    match plan {
        Plan::UpToDate => format!("KeyPad {version} je aktuální"),
        Plan::Repair => format!("KeyPad {version} je opravený"),
        Plan::Update { to, .. } => format!("KeyPad je aktualizovaný na {to}"),
        Plan::Fresh => format!("KeyPad {version} je nainstalovaný"),
    }
}

/// Jak nabídnout ovladač ViGEmBus, který chybí a tentokrát se
/// neinstaloval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Missing {
    /// Okno: uživatel přepínač vypnul — tlačítko „Nainstalovat ovladač"
    /// (zase jen po kliknutí).
    Button,
    /// Tichý režim (aktualizace z aplikace): ovladač se tu nenabízí,
    /// jen odkaz; stav hlásí aplikace sama.
    Link,
    /// Konzole: příkaz, kterým ho uživatel doinstaluje.
    Hint,
}

/// Co je s ovladačem ViGEmBus na konci instalace KeyPadu.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Driver {
    /// Běží, krok se nespouštěl.
    Ready,
    /// Chybí a krok se nespouštěl.
    Missing(Missing),
    /// Je, ale neběží — rada, co s tím.
    NotRunning(vigembus::Advice),
    /// Krok instalace ovladače proběhl s tímto výsledkem.
    Step(driver::Outcome),
}

impl Driver {
    /// Věta do závěrečné zprávy; `None` = není co říkat.
    fn note(&self) -> Option<String> {
        let gamepad = "bez něj KeyPad nevytvoří gamepad";
        match self {
            Driver::Ready => None,
            Driver::Missing(Missing::Button) => Some(format!(
                "Ovladač ViGEmBus jsi vynechal — {gamepad}. Doinstaluješ ho tlačítkem níž nebo \
                 později z KeyPadu."
            )),
            Driver::Missing(Missing::Link) => Some(format!(
                "Chybí ještě ovladač ViGEmBus — {gamepad}. Stáhneš ho z {} (ovladač chce práva \
                 správce, KeyPad sám ne).",
                vigembus::RELEASES_URL
            )),
            Driver::Missing(Missing::Hint) => Some(format!(
                "Chybí ještě ovladač ViGEmBus — {gamepad}. Nainstaluješ ho příkazem \
                 KeyPadSetup.exe {SETUP_ARG_VIGEMBUS} (Windows se zeptá na povolení správce)."
            )),
            Driver::NotRunning(advice) => Some(advice.text()),
            // Nenainstalovaný ovladač: připomenout, co to znamená — hned
            // za první větu, ne až za adresu ruční instalace.
            Driver::Step(
                o @ (driver::Outcome::Cancelled
                | driver::Outcome::Busy
                | driver::Outcome::Failed { .. }),
            ) => {
                let m = o.message();
                let extra = "Bez ovladače KeyPad nevytvoří gamepad.";
                Some(match m.split_once('\n') {
                    Some((first, rest)) => format!("{first} {extra}\n{rest}"),
                    None => format!("{m} {extra}"),
                })
            }
            Driver::Step(o) => Some(o.message()),
        }
    }

    /// Tlačítko, které k ovladači nabídnout.
    fn next(&self) -> Option<gui::Next> {
        match self {
            Driver::Missing(Missing::Button) => {
                Some(gui::Next::Driver("Nainstalovat ovladač".into()))
            }
            Driver::Missing(Missing::Link) => Some(gui::Next::Link(vigembus_link())),
            Driver::NotRunning(advice) => advice_next(advice),
            Driver::Step(o) => driver_next(o),
            _ => None,
        }
    }

    /// Musí to uživatel vědět, než okno zavře? Ovladač, který se měl
    /// nainstalovat a nenainstaloval (nebo čeká na restart), ano.
    /// Vynechaný nebo dávno chybějící ne — to hlásí i aplikace.
    fn attention(&self) -> bool {
        matches!(self, Driver::Step(o) if !o.is_ready())
    }
}

/// Závěrečná zpráva podle stavu systému.
///
/// `launched` je `None`, když se aplikace vůbec nespouštěla (chybí
/// WebView2), jinak výsledek spuštění.
fn outcome(
    headline: &str,
    webview2: bool,
    launched: Option<Result<(), String>>,
    driver: &Driver,
    notes: &[String],
) -> Done {
    let mut done = if !webview2 {
        // Aplikace by se spustila a hned skončila — uživatel by neviděl
        // nic a nevěděl proč. Proto se nespouští a zpráva říká, co dál.
        // Odkaz na WebView2 má přednost: bez něj se KeyPad neotevře
        // vůbec, bez ovladače jen nevytvoří gamepad.
        // (Bez „víc není potřeba": pod tím může být i nenainstalovaný
        // ovladač, a pak by to nebyla pravda.)
        Done {
            message: format!(
                "{headline}, ale zatím ho nespouštím — chybí Microsoft Edge WebView2 Runtime, \
                 bez kterého se okno KeyPadu neotevře.\nNainstaluj ho z {} a spusť KeyPadSetup \
                 znovu.",
                prereq::WEBVIEW2_URL
            ),
            attention: true,
            next: Some(gui::Next::Link(gui::Link {
                label: "Stáhnout WebView2".into(),
                url: prereq::WEBVIEW2_URL.into(),
            })),
        }
    } else {
        // Nepovedené spuštění musí zůstat na očích i v tichém režimu:
        // při aktualizaci z aplikace uživatel viděl, jak se KeyPad zavřel,
        // a bez zprávy by nevěděl, proč se nevrátil (typicky ho zablokoval
        // antivirus).
        let failed_launch = matches!(launched, Some(Err(_)));
        Done {
            message: match launched {
                Some(Err(e)) => format!(
                    "Hotovo — {headline}, jen se ho nepodařilo spustit ({e}). Spusť ho z nabídky \
                     Start."
                ),
                _ => format!("Hotovo — {headline} a běží. Najdeš ho i v nabídce Start."),
            },
            attention: failed_launch || driver.attention(),
            next: driver.next(),
        }
    };
    if let Some(n) = driver.note() {
        done.message.push('\n');
        done.message.push_str(&n);
    }
    // Poznámka „Pozor:" (chybí zástupce, kopie pro odinstalaci…) by se
    // v tichém režimu zavřela dřív, než by ji kdo přečetl.
    if !notes.is_empty() {
        done.attention = true;
    }
    for n in notes {
        done.message.push_str("\nPozor: ");
        done.message.push_str(n);
    }
    done
}

// ── Instalace ──────────────────────────────────────────────────────

fn mb(bytes: usize) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

/// Chyba z doby, kdy se ještě na nic nesáhlo. Uživatel má vědět, že
/// stará instalace (pokud byla) zůstala celá a funkční.
fn untouched(what: &str, e: impl std::fmt::Display) -> String {
    format!("{what}: {e}.\nNa disku se nic nezměnilo.")
}

fn do_install(rep: &mut dyn Report, with_driver: bool, missing: Missing) -> Result<Done, String> {
    let dir = updater::install_dir();
    let app = dir.join(APP_EXE);

    // ── 1. Jaká verze je v repu ────────────────────────────────────
    rep.step(0, "ptám se GitHubu na nejnovější verzi…");
    rep.progress(None);
    // Commit se zjišťuje zvlášť a všechno se pak stahuje z něj: adresa
    // s konkrétním commitem obchází pětiminutovou CDN cache větve, takže
    // se nesmíchá stará version.txt s novou binárkou (viz updater).
    let sha = updater::latest_commit()
        .map_err(|e| untouched("Nepodařilo se zjistit aktuální verzi", e))?;
    let version = updater::fetch_release_file(&sha, VERSION_FILE, |_| {})
        .map_err(|e| untouched("Nepodařilo se zjistit aktuální verzi", e))?;
    let version = String::from_utf8_lossy(&version).trim().to_string();
    if version.is_empty() {
        return Err(untouched(
            "Nepodařilo se zjistit aktuální verzi",
            "server vrátil prázdnou verzi",
        ));
    }

    let decision = plan(
        updater::installed_version().as_deref(),
        &version,
        app.is_file(),
    );
    let mut notes = Vec::new();
    // Co se chystá — jde rovnou do popisku kroku stahování, ať je to
    // vidět v okně (samostatný stavový řádek by hned přepsal další krok).
    let what = match &decision {
        Plan::UpToDate => {
            // Nic se nestahuje ani nepřepisuje, ale zástupce a záznam
            // v systému se obnoví (levné, lokální). Tím je „spusť
            // instalátor znovu" jediná rada, kterou kamarád kdy
            // potřebuje — opraví i smazaného zástupce.
            rep.step(STEP_REGISTER, "verze sedí — kontroluji zástupce a záznam…");
            ensure_setup_copy(&dir, &mut notes);
            register(&dir, &version, &mut notes);
            let drv = driver_step(rep, with_driver, missing);
            return Ok(launch_and_report(
                &dir,
                &headline(&decision, &version),
                notes,
                &drv,
            ));
        }
        Plan::Repair => format!("verze {version} sedí, ale chybí {APP_EXE} — opravuji"),
        Plan::Update { from, to } => format!("aktualizuji {from} → {to}"),
        Plan::Fresh => format!("instaluji {version}"),
    };

    // ── 2. Stažení do paměti ───────────────────────────────────────
    // Nejdřív se stáhne všechno, teprve pak se sahá na disk: když
    // spadne síť uprostřed, zůstane funkční stará instalace.
    rep.step(1, &format!("{what}, stahuji…"));
    // Průběžné hlášení po 256 KiB — každý blok zvlášť by okno budil
    // stokrát za sekundu bez jediné nové informace.
    let mut last = 0usize;
    let data = updater::fetch_release_file(&sha, APP_EXE, |n| {
        if n >= last + 256 * 1024 {
            last = n;
            rep.download(APP_EXE, n);
        }
    })
    .map_err(|e| untouched("Stažení se nepovedlo", e))?;
    if !updater::looks_like_exe(&data) {
        return Err(untouched(
            &format!("{APP_EXE} se stáhl poškozený ({} B)", data.len()),
            "zkus to za chvíli znovu",
        ));
    }
    rep.status(&format!("{APP_EXE} — {}", mb(data.len())));
    rep.progress(Some(0.5));

    // ── 3. Zavřít běžící KeyPad ────────────────────────────────────
    // Běžící proces drží vlastní .exe zamčený — přepsat ho nejde.
    rep.step(2, "hledám běžící KeyPad…");
    rep.progress(None);
    let closed = proc::close_app(&app, QUIT_EVENT_NAME, &mut |s| rep.status(s))
        .map_err(|e| format!("{e}\nNa disku se nic nezměnilo."))?;

    // ── 4. Zápis na disk ───────────────────────────────────────────
    rep.step(3, "zapisuji do profilu…");
    rep.progress(Some(0.7));
    if let Err(e) = write_files(&dir, &data, &version, &mut notes) {
        // KeyPad.exe je díky zápisu přes .new pořád celý — buď starý
        // (nepovedlo se ho přepsat), nebo už nový (selhal až zápis verze,
        // která se píše poslední; zůstala stará, takže příští spuštění
        // instalátoru aktualizaci dokončí). Spustit jde tak jako tak.
        // Když jsme ho zavřeli, vrátíme ho — kamarád nemá zůstat bez
        // aplikace jen proto, že se nepovedla aktualizace. Zpráva proto
        // neříká „původní": může to být i nová binárka.
        if closed != proc::Closed::NotRunning && app.is_file() && launch(&dir).is_ok() {
            return Err(format!(
                "{e}\nKeyPad jsem zase spustil, ať nezůstaneš bez aplikace."
            ));
        }
        return Err(e);
    }

    // ── 5. Zápisy do systému ───────────────────────────────────────
    rep.step(
        STEP_REGISTER,
        "zástupce v nabídce Start a záznam v Aplikacích…",
    );
    rep.progress(Some(0.9));
    register(&dir, &version, &mut notes);

    // ── 6. Ovladač ViGEmBus (jen když ho uživatel nechal zapnutý) ──
    let drv = driver_step(rep, with_driver, missing);

    Ok(launch_and_report(
        &dir,
        &headline(&decision, &version),
        notes,
        &drv,
    ))
}

/// Krok ovladače: nainstalovat (když ho uživatel nechal zapnutý), nebo
/// jen zjistit, jak na tom je.
///
/// Běží PŘED spuštěním KeyPadu — aplikace pak sběrnici najde hned při
/// startu. Jeho výsledek instalaci KeyPadu nikdy neshodí: KeyPad je
/// v tu chvíli zapsaný a zaregistrovaný, ovladač se dá zkusit znovu.
fn driver_step(rep: &mut dyn Report, with_driver: bool, missing: Missing) -> Driver {
    if with_driver {
        return Driver::Step(driver::install(rep, STEP_DRIVER));
    }
    match vigembus::state() {
        BusState::Ready => Driver::Ready,
        BusState::NotInstalled => Driver::Missing(missing),
        s => Driver::NotRunning(s.advice().unwrap_or(vigembus::Advice::Restart)),
    }
}

/// Zkontroluje prostředí, případně spustí aplikaci, a složí zprávu.
fn launch_and_report(dir: &Path, headline: &str, notes: Vec<String>, drv: &Driver) -> Done {
    let webview2 = prereq::webview2_present();
    let launched = webview2.then(|| launch(dir));
    outcome(headline, webview2, launched, drv, &notes)
}

/// Zapíše KeyPad.exe, kopii instalátoru a nakonec verzi.
fn write_files(
    dir: &Path,
    exe: &[u8],
    version: &str,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("Nelze vytvořit složku {}: {e}", dir.display()))?;
    replace_file(&dir.join(APP_EXE), exe)?;
    ensure_setup_copy(dir, notes);
    // Verze se zapisuje až nakonec: kdyby zápis binárky selhal, zůstane
    // u ní sedět stará verze a příští spuštění instalátoru ji opraví.
    // Nová verze u starého souboru by opravu zablokovala.
    write_retry(&dir.join(VERSION_FILE), version.as_bytes())
}

/// Přepíše soubor přes dočasný `.new` a přejmenování.
///
/// Rovnou `fs::write` by nejdřív soubor zkrátil na nulu a teprve pak
/// psal — selhání uprostřed (plný disk, antivir) by nechalo rozbitou
/// binárku. Přejmenování je naopak atomické: buď je na místě celý
/// starý soubor, nebo celý nový.
///
/// Přejmenování se opakuje: zavřený proces ještě chvíli dobíhá a drží
/// svůj .exe, a čerstvě zapsaný soubor si umí na okamžik zamknout
/// antivir. Pár sekund trpělivosti je levnější než přerušená instalace.
fn replace_file(path: &Path, data: &[u8]) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".new");
    let tmp = PathBuf::from(tmp);
    write_retry(&tmp, data)?;
    let mut last = String::new();
    for _ in 0..20 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = e.to_string();
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(format!(
        "{} nejde přepsat ({last}) — nejspíš ho pořád něco drží. Zavři KeyPad a zkus to znovu.",
        path.display()
    ))
}

/// Zápis s několika pokusy (důvod viz `replace_file`).
fn write_retry(path: &Path, data: &[u8]) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..20 {
        match std::fs::write(path, data) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = e.to_string();
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }
    Err(format!("Nelze zapsat {}: {last}", path.display()))
}

/// Uloží kopii běžícího instalátoru vedle aplikace — odinstalace přes
/// Nastavení → Aplikace pak funguje i po smazání staženého souboru.
///
/// Když instalátor běží právě z té kopie (Změnit v Nastavení), kopírovat
/// není co — a ani by to nešlo, běžící .exe je zamčený.
fn ensure_setup_copy(dir: &Path, notes: &mut Vec<String>) {
    let dst = dir.join(SETUP_EXE);
    let me = match std::env::current_exe() {
        Ok(me) => me,
        Err(e) => {
            notes.push(format!("kopie instalátoru pro odinstalaci chybí ({e})"));
            return;
        }
    };
    if proc::same_file(&me, &dst) {
        return;
    }
    let _ = std::fs::create_dir_all(dir);
    let mut last = String::new();
    for _ in 0..8 {
        match std::fs::copy(&me, &dst) {
            Ok(_) => return,
            Err(e) => {
                last = e.to_string();
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }
    notes.push(setup_copy_note(&last));
}

/// Poznámka „Pozor:", když se kopie instalátoru nezapsala (sdílí ji
/// instalace a náhled nejdelší zprávy).
fn setup_copy_note(err: &str) -> String {
    format!(
        "kopie instalátoru pro odinstalaci se nezapsala ({err}) — odinstaluješ příkazem \
         KeyPadSetup.exe /uninstall"
    )
}

/// Zástupce v nabídce Start a záznam v Nastavení → Aplikace. Obojí je
/// pohodlí navíc — selhání instalaci nezastaví, jen se ohlásí.
fn register(dir: &Path, version: &str, notes: &mut Vec<String>) {
    if let Err(e) = shell::create_shortcut(&dir.join(APP_EXE), &shell::start_menu_lnk()) {
        notes.push(shortcut_note(&e));
    }
    let size_kb = [APP_EXE, SETUP_EXE]
        .iter()
        .filter_map(|n| std::fs::metadata(dir.join(n)).ok())
        .map(|m| (m.len() / 1024) as u32)
        .sum();
    if let Err(e) = shell::register_uninstall(&dir.join(SETUP_EXE), dir, version, size_kb) {
        notes.push(arp_note(&e));
    }
}

/// Poznámky „Pozor:" k zápisům do systému (sdílí je instalace a ladicí
/// náhled přetečení).
fn shortcut_note(err: &str) -> String {
    format!("zástupce v nabídce Start se nepodařilo vytvořit: {err}")
}

fn arp_note(err: &str) -> String {
    format!("záznam v Nastavení → Aplikace: {err}")
}

/// Spustí nainstalovaný KeyPad.
///
/// Obyčejné spuštění stačí: instalátor běží pod uživatelem bez zvýšených
/// práv, takže je aplikace zdědí stejná. (WinSent tu musel jít oklikou
/// přes explorer.exe, protože jeho instalátor běžel jako správce.)
/// Standardní vstupy/výstupy se odpojí — v headless režimu by aplikace
/// jinak dostala konzoli, ke které se instalátor připojil.
fn launch(dir: &Path) -> Result<(), String> {
    Command::new(dir.join(APP_EXE))
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ── Odinstalace ────────────────────────────────────────────────────

fn do_uninstall(rep: &mut dyn Report) -> Result<Done, String> {
    let dir = updater::install_dir();
    let app = dir.join(APP_EXE);
    let lnk = shell::start_menu_lnk();
    // Bylo vůbec co odebírat? Jen kvůli poctivé závěrečné hlášce.
    let found = app.exists()
        || dir.join(VERSION_FILE).exists()
        || dir.join(SETUP_EXE).exists()
        || lnk.exists()
        || shell::uninstall_entry_exists();

    rep.step(0, "hledám běžící KeyPad…");
    rep.progress(None);
    proc::close_app(&app, QUIT_EVENT_NAME, &mut |s| rep.status(s))?;

    // Nejdřív soubory, až pak zástupce a záznam: když smazání selže,
    // záznam v Aplikacích zůstane a odinstalace se dá zopakovat.
    rep.step(1, "mažu soubory…");
    rep.progress(Some(0.4));
    remove_retry(&app)?;
    let mut new = app.as_os_str().to_owned();
    new.push(".new");
    let _ = std::fs::remove_file(PathBuf::from(new));
    for name in SIDE_FILES {
        let _ = std::fs::remove_file(dir.join(name));
    }

    // Cache a data okna (WebView2) — patří jen KeyPadu, složka nese
    // jeho identifikátor. Bez tohohle by po odinstalaci zůstávaly
    // v profilu desítky MB.
    let mut webview_left = None;
    if let Some(wv) = updater::webview_data_dir() {
        if wv.is_dir() {
            rep.status("mažu data okna (WebView2)…");
            if !remove_dir_retry(&wv) {
                webview_left = Some(wv);
            }
        }
    }
    // Náhradní umístění logu (když instalační složka nebyla zapisovatelná).
    if let Some(roaming) = updater::roaming_dir() {
        for name in SIDE_FILES {
            let _ = std::fs::remove_file(roaming.join(name));
        }
        let _ = std::fs::remove_dir(&roaming);
    }
    remove_update_downloads(&updater::update_temp_dir());
    // Zbytky přerušené instalace ovladače v %TEMP% (jen naše soubory).
    driver::remove_leftovers();

    rep.step(2, "odebírám zástupce a záznam v systému…");
    rep.progress(Some(0.8));
    let _ = std::fs::remove_file(&lnk);
    shell::unregister_uninstall();

    // Kopii instalátoru smazat jde, jen když neběžíme z ní; jinak ji
    // po skončení smaže `schedule_self_delete`.
    let setup = dir.join(SETUP_EXE);
    let running_from_it = std::env::current_exe()
        .map(|me| proc::same_file(&me, &setup))
        .unwrap_or(false);
    if !running_from_it {
        let _ = std::fs::remove_file(&setup);
    }
    let config_left = dir.join(CONFIG_FILE).is_file();
    // Jen prázdnou složku — cokoliv, co v ní zůstalo, není naše.
    let _ = std::fs::remove_dir(&dir);

    let bus = if vigembus::state() == BusState::NotInstalled {
        BusLeft::No
    } else if vigembus::arp_entry_exists() {
        BusLeft::InApps
    } else {
        BusLeft::Elsewhere
    };
    Ok(Done::plain(uninstall_message(
        found,
        config_left.then_some(dir.as_path()),
        webview_left.as_deref(),
        bus,
    )))
}

/// Zůstal po odinstalaci KeyPadu v systému ViGEmBus, a jde odebrat
/// v Aplikacích?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BusLeft {
    No,
    /// Se záznamem „ViGEm Bus Driver" v Aplikacích.
    InApps,
    /// Bez záznamu — dal ho tam jiný program (devcon, přibalený
    /// instalátor); poslat uživatele do Aplikací by byla slepá ulička.
    Elsewhere,
}

/// Závěrečná zpráva odinstalace.
///
/// ViGEmBus se s KeyPadem NEodinstalovává: je to ovladač celého systému,
/// může ho používat i jiný program (DS4Windows, Steam Input…) a jeho
/// odebrání chce práva správce. Uživatel ale má vědět, že zůstal a kde
/// ho odebere, když ho nechce.
fn uninstall_message(
    found: bool,
    config_dir: Option<&Path>,
    webview_left: Option<&Path>,
    bus: BusLeft,
) -> String {
    let mut msg = match (found, config_dir) {
        (false, _) => String::from("KeyPad tu nainstalovaný nebyl — nebylo co odebírat."),
        (true, Some(dir)) => format!(
            "KeyPad je odebraný.\nTvoje nastavení ({CONFIG_FILE}) zůstalo v {} — smaž ho ručně, \
             pokud ho už nechceš.",
            dir.display()
        ),
        (true, None) => String::from("KeyPad je odebraný."),
    };
    // Není to chyba (aplikace je pryč), ale uživatel má vědět, kde
    // zbyly megabajty, které čekal smazané.
    if let Some(wv) = webview_left {
        msg.push_str(&format!(
            "\nData okna (WebView2) v {} se nepodařilo smazat celá — nejspíš je ještě drží \
             dobíhající proces WebView2. Smaž tu složku ručně.",
            wv.display()
        ));
    }
    match bus {
        BusLeft::No => {}
        BusLeft::InApps => msg.push_str(
            "\nViGEmBus zůstává — může ho používat i jiný program; odebereš ho v Aplikacích \
             („ViGEm Bus Driver“).",
        ),
        BusLeft::Elsewhere => msg.push_str(
            "\nViGEmBus v systému zůstává — v Aplikacích záznam nemá, nejspíš ho přinesl jiný \
             program, který ho může dál používat.",
        ),
    }
    msg
}

/// Smaže složku i s obsahem, s pokusy; `true` = složka je pryč.
///
/// Procesy WebView2 (`msedgewebview2.exe`) přežijí KeyPad o pár vteřin
/// a do té doby drží soubory své cache otevřené — první pokus by smazal
/// jen část. Každý další pokus pokračuje tam, kde předchozí skončil.
/// `remove_dir_all` nenásleduje spojení (junction) ani symbolické
/// odkazy, takže nemůže sáhnout mimo složku.
fn remove_dir_retry(path: &Path) -> bool {
    for _ in 0..24 {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return true,
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
    !path.exists()
}

/// Uklidí instalátory stažené aktualizací z aplikace
/// (`%TEMP%\keypad-update`). Chyby se ignorují — je to dočasná složka.
///
/// Maže se jen po souborech, ne `remove_dir_all`: složka leží ve
/// sdíleném %TEMP% a podsložky do ní aplikace nezakládá — co v ní je
/// navíc, není naše. Běžící instalátor se přeskočí: kdyby tahle
/// odinstalace běžela právě z jednoho z nich, sám sebe smazat nejde,
/// a zbytek se tak přesto uklidí.
fn remove_update_downloads(dir: &Path) {
    let me = std::env::current_exe().ok();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let path = e.path();
            let is_file = e.file_type().map(|t| t.is_file()).unwrap_or(false);
            let is_me = me.as_deref().is_some_and(|me| proc::same_file(me, &path));
            if is_file && !is_me {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// Smaže soubor, s pokusy (proces po zavření ještě chvíli dobíhá).
/// Neexistující soubor je v pořádku.
fn remove_retry(path: &Path) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..20 {
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                last = e.to_string();
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }
    Err(format!(
        "{} nejde smazat ({last}) — zavři KeyPad a zkus to znovu.",
        path.display()
    ))
}

/// Příkazová řádka pro `cmd.exe`, která po chvíli smaže instalátor
/// a prázdnou instalační složku.
///
/// Cesty v ní NEJSOU — jdou přes proměnné prostředí ([`cleanup_env`]).
/// cmd rozbaluje `%…%` v celé řádce, i uvnitř uvozovek; cesta vepsaná
/// přímo by se rozbalila znovu a profil `x%OS%y` by z ní udělal
/// `xWindows_NTy` — smazal by se cizí soubor (ověřeno). Hodnota
/// proměnné se rozbalí jen jednou a znovu se už nečte.
///
/// `/S /C "…"` = cmd odstraní jen první a poslední uvozovku a zbytek
/// nechá, jak je — proměnné tak můžou být ve vlastních uvozovkách
/// (mezery, `&` a spol. ve jméně uživatele). `/D` vypne AutoRun
/// z registru, `/V:OFF` zpožděné rozbalování (jinak by `!` v cestě
/// něco znamenal, když ho má uživatel v registru zapnuté).
/// `ping -n 3` je obvyklé „počkej ~2 s" bez `timeout.exe`, který bez
/// konzole odmítá běžet; plnou cestou, aby ho cmd nehledal v pracovní
/// složce (`%LOCALAPPDATA%\Programs` — zapsat do ní může cokoli, co
/// běží pod uživatelem).
const CLEANUP_COMMAND: &str = "/D /V:OFF /S /C \"\"%KP_PING%\" -n 3 127.0.0.1 >nul \
     & del /F /Q \"%KP_SETUP%\" 2>nul & rmdir \"%KP_DIR%\" 2>nul\"";

/// Proměnné prostředí pro [`CLEANUP_COMMAND`].
fn cleanup_env(setup: &Path, dir: &Path) -> [(&'static str, PathBuf); 3] {
    [
        ("KP_PING", proc::system32("PING.EXE")),
        ("KP_SETUP", setup.to_path_buf()),
        ("KP_DIR", dir.to_path_buf()),
    ]
}

/// Běžící .exe sám sebe smazat nemůže (Windows ho drží zamčený).
/// Úklid proto dokončí skrytý `cmd.exe`, který chvíli počká, až tenhle
/// proces skončí, a pak smaže kopii instalátoru a prázdnou složku.
fn schedule_self_delete(setup: &Path, dir: &Path) {
    // Pracovní složka mimo instalační — jinak by ji cmd držel a rmdir
    // by selhal na „používá ji jiný proces".
    let cwd = dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let _ = Command::new(proc::system32("cmd.exe"))
        .raw_arg(CLEANUP_COMMAND)
        .envs(cleanup_env(setup, dir))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(proc::CREATE_NO_WINDOW)
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stejna_verze_se_soubory_je_aktualni() {
        assert_eq!(plan(Some("0.1.0+1"), "0.1.0+1", true), Plan::UpToDate);
        // Bílé znaky kolem (konec řádku ve version.txt) nerozhodují.
        assert_eq!(plan(Some("0.1.0+1\r\n"), " 0.1.0+1", true), Plan::UpToDate);
    }

    #[test]
    fn stejna_verze_bez_binarky_je_oprava() {
        assert_eq!(plan(Some("0.1.0+1"), "0.1.0+1", false), Plan::Repair);
    }

    #[test]
    fn jina_verze_je_aktualizace_i_smerem_dolu() {
        assert_eq!(
            plan(Some("0.1.0+1"), "0.1.0+2", true),
            Plan::Update {
                from: "0.1.0+1".into(),
                to: "0.1.0+2".into()
            }
        );
        // Vydavatel vrátil starší build — i to se nainstaluje.
        assert_eq!(
            plan(Some("0.1.0+2"), "0.1.0+1", false),
            Plan::Update {
                from: "0.1.0+2".into(),
                to: "0.1.0+1".into()
            }
        );
    }

    #[test]
    fn bez_verze_je_cista_instalace() {
        assert_eq!(plan(None, "0.1.0+1", false), Plan::Fresh);
        // Binárka bez version.txt = přerušený zápis → instalovat znovu.
        assert_eq!(plan(None, "0.1.0+1", true), Plan::Fresh);
        assert_eq!(plan(Some("  "), "0.1.0+1", true), Plan::Fresh);
    }

    fn url(next: Option<gui::Next>) -> Option<String> {
        match next {
            Some(gui::Next::Link(l)) => Some(l.url),
            _ => None,
        }
    }

    const READY: Driver = Driver::Ready;
    const MISSING_LINK: Driver = Driver::Missing(Missing::Link);

    #[test]
    fn bez_webview2_se_nespousti_a_okno_zustane() {
        let d = outcome("KeyPad 1 je nainstalovaný", false, None, &READY, &[]);
        assert!(d.attention);
        assert!(d.message.contains(prereq::WEBVIEW2_URL));
        assert!(d.message.contains("KeyPadSetup znovu"));
        assert_eq!(url(d.next), Some(prereq::WEBVIEW2_URL.to_string()));
    }

    #[test]
    fn chybejici_vigembus_v_tichem_rezimu_neni_chyba() {
        let d = outcome(
            "KeyPad 1 je nainstalovaný",
            true,
            Some(Ok(())),
            &MISSING_LINK,
            &[],
        );
        assert!(!d.attention);
        assert!(d.message.starts_with("Hotovo"));
        assert!(d.message.contains(vigembus::RELEASES_URL));
        assert_eq!(url(d.next), Some(vigembus::RELEASES_URL.to_string()));
    }

    #[test]
    fn chybi_oboji_prednost_ma_webview2() {
        let d = outcome("KeyPad 1 je nainstalovaný", false, None, &MISSING_LINK, &[]);
        assert!(d.attention);
        assert!(d.message.contains(vigembus::RELEASES_URL));
        assert_eq!(url(d.next), Some(prereq::WEBVIEW2_URL.to_string()));
    }

    #[test]
    fn vse_v_poradku_bez_odkazu() {
        let d = outcome("KeyPad 1 je aktuální", true, Some(Ok(())), &READY, &[]);
        assert_eq!(
            d,
            Done::plain("Hotovo — KeyPad 1 je aktuální a běží. Najdeš ho i v nabídce Start.")
        );
    }

    #[test]
    fn nepovedene_spusteni_a_poznamky_jsou_ve_zprave() {
        let d = outcome(
            "KeyPad 1 je nainstalovaný",
            true,
            Some(Err("přístup odepřen".into())),
            &READY,
            &["zástupce nevznikl".into()],
        );
        assert!(d.message.contains("přístup odepřen"));
        assert!(d.message.ends_with("Pozor: zástupce nevznikl"));
        assert!(d.attention);
    }

    /// Tichý režim (aktualizace z aplikace) zavře okno jen u čistého
    /// úspěchu. Nespuštěný KeyPad nebo „Pozor:" musí zůstat na očích.
    #[test]
    fn nespusteni_nebo_poznamka_nechaji_okno_otevrene() {
        let h = "KeyPad je aktualizovaný na 2";
        let d = outcome(h, true, Some(Err("blokováno".into())), &READY, &[]);
        assert!(d.attention);
        assert!(d.message.contains("blokováno"));

        let d = outcome(h, true, Some(Ok(())), &READY, &["zástupce nevznikl".into()]);
        assert!(d.attention);

        // S chybějícím ViGEmBus a nespuštěním: pořád pozornost i odkaz.
        let d = outcome(h, true, Some(Err("x".into())), &MISSING_LINK, &[]);
        assert!(d.attention);
        assert_eq!(url(d.next), Some(vigembus::RELEASES_URL.to_string()));

        assert!(!outcome(h, true, Some(Ok(())), &READY, &[]).attention);
    }

    /// Instalace KeyPadu se povede i se zrušenou výzvou UAC: zpráva to
    /// řekne a hlavní tlačítko nabídne ovladač znovu (jen ovladač).
    #[test]
    fn zruseny_ovladac_nezrusi_keypad_a_nabidne_opakovani() {
        let d = outcome(
            "KeyPad 1 je nainstalovaný",
            true,
            Some(Ok(())),
            &Driver::Step(driver::Outcome::Cancelled),
            &[],
        );
        assert!(d
            .message
            .starts_with("Hotovo — KeyPad 1 je nainstalovaný a běží."));
        assert!(d.message.contains("výzva Windows nebyla potvrzena"));
        assert!(d.attention);
        assert_eq!(
            d.next,
            Some(gui::Next::Driver("Zkusit znovu ovladač".into()))
        );
    }

    #[test]
    fn neoficialni_soubor_vede_na_rucni_instalaci() {
        let d = outcome(
            "KeyPad 1 je nainstalovaný",
            true,
            Some(Ok(())),
            &Driver::Step(driver::Outcome::Failed {
                reason: "otisk SHA-256 nesedí".into(),
                retry: false,
            }),
            &[],
        );
        assert!(d.attention);
        assert!(d.message.contains("otisk SHA-256 nesedí"));
        assert_eq!(url(d.next), Some(vigembus::RELEASES_URL.to_string()));
    }

    #[test]
    fn nainstalovany_ovladac_se_potvrdi() {
        let d = outcome(
            "KeyPad 1 je nainstalovaný",
            true,
            Some(Ok(())),
            &Driver::Step(driver::Outcome::Installed),
            &[],
        );
        assert!(!d.attention);
        assert!(d.next.is_none());
        assert!(d
            .message
            .ends_with("Ovladač ViGEmBus je nainstalovaný a běží."));
    }

    #[test]
    fn vynechany_ovladac_jde_doinstalovat_tlacitkem() {
        let d = outcome(
            "KeyPad 1 je nainstalovaný",
            true,
            Some(Ok(())),
            &Driver::Missing(Missing::Button),
            &[],
        );
        assert!(!d.attention);
        assert!(d.message.contains("jsi vynechal"));
        assert_eq!(
            d.next,
            Some(gui::Next::Driver("Nainstalovat ovladač".into()))
        );
        // Konzole: jen příkaz, žádné tlačítko ani instalace.
        let d = outcome(
            "h",
            true,
            Some(Ok(())),
            &Driver::Missing(Missing::Hint),
            &[],
        );
        assert!(d.message.contains("KeyPadSetup.exe /vigembus"));
        assert!(d.next.is_none());
    }

    #[test]
    fn nebezici_ovladac_jen_poradi() {
        let advice = vigembus::Advice::EnableDevice;
        let d = outcome(
            "KeyPad 1 je nainstalovaný",
            true,
            Some(Ok(())),
            &Driver::NotRunning(advice),
            &[],
        );
        assert!(d.message.ends_with(&advice.text()));
        assert!(d.next.is_none(), "nic se neinstaluje, žádné tlačítko");
        // Rada vedoucí na ruční instalaci (bez záznamu v Aplikacích)
        // dostane tlačítko s odkazem — adresu z okna zkopírovat nejde.
        let d = outcome(
            "KeyPad 1 je nainstalovaný",
            true,
            Some(Ok(())),
            &Driver::NotRunning(vigembus::Advice::Reinstall { in_apps: false }),
            &[],
        );
        assert_eq!(url(d.next), Some(vigembus::RELEASES_URL.to_string()));
    }

    fn args(a: &[&str]) -> Args {
        parse_args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parametry_prikazove_radky() {
        assert_eq!(args(&[]).mode, Mode::Install);
        assert_eq!(args(&["/VIGEMBUS"]).mode, Mode::Driver);
        assert_eq!(args(&[SETUP_ARG_VIGEMBUS]).mode, Mode::Driver);
        let a = args(&["/headless", "/vigembus"]);
        assert_eq!((a.mode, a.headless), (Mode::Driver, true));
        // Tichý režim ovladač nikdy nespustí sám — /vigembus čeká na klik.
        let a = args(&["/quiet", "/vigembus"]);
        assert_eq!((a.mode, a.quiet), (Mode::Driver, false));
        assert!(args(&["/quiet"]).quiet);
        // Nejasná kombinace: odinstalace vyhrává, nic se neinstaluje.
        assert_eq!(args(&["/vigembus", "/uninstall"]).mode, Mode::Uninstall);
        // /uninstall /quiet = spustí se i zavře samo, jako od první verze
        // (ovladače se odinstalace netýká).
        let a = args(&["/uninstall", "/quiet"]);
        assert_eq!((a.mode, a.quiet), (Mode::Uninstall, true));
        assert!(args(&["/U", "/S"]).quiet);
        assert!(!args(&["/uninstall"]).quiet);
    }

    #[test]
    fn odinstalace_rekne_ze_vigembus_zustava() {
        let m = uninstall_message(true, None, None, BusLeft::InApps);
        assert!(m.starts_with("KeyPad je odebraný."));
        assert!(m.contains(
            "ViGEmBus zůstává — může ho používat i jiný program; odebereš ho v Aplikacích"
        ));
        // Bez záznamu v Aplikacích tam uživatele neposílat.
        let m = uninstall_message(true, None, None, BusLeft::Elsewhere);
        assert!(m.contains("ViGEmBus v systému zůstává"));
        assert!(!m.contains("odebereš ho v Aplikacích"));
        assert!(!uninstall_message(true, None, None, BusLeft::No).contains("ViGEmBus"));
        let m = uninstall_message(false, None, None, BusLeft::No);
        assert!(m.contains("nebylo co odebírat"));
        let d = Path::new(r"C:\x\KeyPad");
        assert!(uninstall_message(true, Some(d), None, BusLeft::No).contains(r"C:\x\KeyPad"));
    }

    /// Přepínač je vidět předem a říká, co se stane: čí ovladač, pod
    /// jakou licencí, že je oficiální a že se Windows zeptají na správce.
    #[test]
    fn prepinac_ovladace_vysvetli_co_se_stane() {
        assert!(DRIVER_TOGGLE.contains("Windows se zeptá na povolení správce"));
        assert!(DRIVER_TOGGLE.contains("ViGEmBus"));
        let note = driver_toggle_note();
        for part in [
            "oficiální ViGEmBus",
            vigembus::VERSION,
            vigembus::SIGNER,
            vigembus::LICENSE,
            "SHA-256",
            "povolení správce",
        ] {
            assert!(note.contains(part), "{part}: {note}");
        }
        assert!(driver_intro().contains("SHA-256"));
        assert_eq!(STEP_DRIVER, INSTALL_STEPS.len());
    }

    /// Kód okna /vigembus: běžící ovladač 0 vždy; jinak poslední pokus;
    /// bez pokusu pojistka (čeká na restart → 3010, jinak 1).
    #[test]
    fn kod_okna_ovladace() {
        let restart = BusState::InstalledNotRunning {
            device: Some(vigembus::DeviceStatus {
                problem: None,
                need_restart: true,
                started: false,
            }),
            in_apps: true,
        };
        let broken = BusState::InstalledNotRunning {
            device: None,
            in_apps: false,
        };
        assert_eq!(driver_exit_code(BusState::Ready, None), 0);
        assert_eq!(driver_exit_code(BusState::Ready, Some(1)), 0);
        assert_eq!(driver_exit_code(BusState::NotInstalled, None), 1);
        assert_eq!(driver_exit_code(BusState::NotInstalled, Some(1)), 1);
        assert_eq!(driver_exit_code(restart, None), 3010);
        assert_eq!(driver_exit_code(restart, Some(3010)), 3010);
        assert_eq!(driver_exit_code(broken, None), 1);
    }

    /// Nejdelší skutečná závěrečná zpráva (chybí WebView2 + nejdelší
    /// výsledek ovladače + „Pozor:") se při 100 % DPI vejde do okna
    /// s šesti kroky běžným písmem — změřeno GDI stejně jako při
    /// kreslení. Delší texty v driver.rs / main.rs tenhle test shodí;
    /// okno by pak přešlo na menší písmo (viz `gui::Fit`).
    #[test]
    fn nejdelsi_zprava_se_vejde_do_okna() {
        let d = longest_done(PREVIEW_HEADLINE);
        assert!(d.message.contains(prereq::WEBVIEW2_URL));
        assert!(d.message.contains("\nPozor: "));
        let steps = INSTALL_STEPS.len() + 1;
        assert_eq!(
            gui::message_fit_96(&d.message, steps),
            gui::Fit::Body,
            "{}",
            d.message
        );
    }

    /// `/vigembus` na PC, kde ovladač je: žádné tlačítko instalace, jen
    /// stav nebo rada.
    #[test]
    fn obrazovka_ovladace_podle_stavu() {
        let st = driver_screen(BusState::NotInstalled);
        assert_eq!(st.primary, "Nainstalovat ViGEmBus");
        assert_eq!(st.steps.len(), 1);
        assert!(st.message.contains("povolení"));

        let st = driver_screen(BusState::Ready);
        assert_eq!(st.phase, gui::Phase::Done);
        assert!(st.primary.is_empty() && st.steps.is_empty());

        let st = driver_screen(BusState::InstalledNotRunning {
            device: None,
            in_apps: true,
        });
        assert_eq!(st.phase, gui::Phase::Done);
        assert!(st.attention && st.primary.is_empty());
        assert!(st.message.contains("Aplikace"));

        // Bez záznamu v Aplikacích: žádné „odeber v Aplikacích", ale
        // tlačítko na ruční instalaci.
        let st = driver_screen(BusState::InstalledNotRunning {
            device: None,
            in_apps: false,
        });
        assert!(st.attention);
        assert!(!st.message.contains("Aplikace →"));
        assert_eq!(st.primary, "Stáhnout ViGEmBus");
    }

    #[test]
    fn uklid_bere_cesty_z_promennych() {
        let cmd = CLEANUP_COMMAND;
        assert!(cmd.starts_with("/D /V:OFF /S /C \"\"%KP_PING%\" -n 3 127.0.0.1 >nul & "));
        assert!(cmd.contains(" & del /F /Q \"%KP_SETUP%\" 2>nul & "));
        assert!(cmd.contains(" & rmdir \"%KP_DIR%\" 2>nul"));
        // /S odstraní právě první a poslední uvozovku celé řádky.
        assert!(cmd.ends_with("2>nul\""));
        assert_eq!(cmd.matches('"').count(), 8);
        // Žádná jiná procenta než tři naše proměnné.
        assert_eq!(cmd.matches('%').count(), 6);

        let dir = Path::new(r"C:\Users\x%OS%y\AppData\Local\Programs\KeyPad");
        let setup = dir.join(SETUP_EXE);
        let env = cleanup_env(&setup, dir);
        for (name, _) in &env {
            assert!(cmd.contains(&format!("\"%{name}%\"")), "{name}");
        }
        assert!(env[0].1.is_absolute());
        assert!(env[0].1.ends_with(r"System32\PING.EXE"));
        assert_eq!(env[1].1, setup);
        assert_eq!(env[2].1, dir);
    }

    /// Skutečný `cmd.exe` na cestě s `%OS%` a znaky, které cmd jinak
    /// něco znamenají: smaže se náš soubor a složka — ne soubor v cestě,
    /// která by vznikla rozbalením (`xWindows_NTy`). Pracuje se v `target\`.
    #[test]
    fn uklid_nerozbaluje_procenta_v_ceste() {
        let base = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("keypad-cleanup-test-{}", std::process::id()));
        let os = std::env::var("OS").unwrap_or_else(|_| "Windows_NT".into());
        let ours = base.join("x%OS%y & (a) ^b! c").join("KeyPad");
        let decoy = base.join(format!("x{os}y & (a) ^b! c")).join("KeyPad");
        for d in [&ours, &decoy] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join(SETUP_EXE), b"x").unwrap();
        }

        let status = Command::new(proc::system32("cmd.exe"))
            .raw_arg(CLEANUP_COMMAND)
            .envs(cleanup_env(&ours.join(SETUP_EXE), &ours))
            .current_dir(&base)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(proc::CREATE_NO_WINDOW)
            .status()
            .unwrap();

        let ours_gone = !ours.exists();
        let decoy_kept = decoy.join(SETUP_EXE).is_file();
        let _ = std::fs::remove_dir_all(&base);
        assert!(status.success(), "{status:?}");
        assert!(ours_gone, "náš instalátor i složka měly zmizet");
        assert!(decoy_kept, "soubor v rozbalené cestě se smazat nesměl");
    }

    #[test]
    fn mazani_slozky_pocka_na_uvolneni_souboru() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("keypad-rmdir-test-{}", std::process::id()));
        let nested = dir.join("EBWebView").join("Default");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.join("lockfile"), b"x").unwrap();
        std::fs::write(nested.join("data"), b"x").unwrap();
        // Soubor držený bez sdílení mazání — tak ho drží dobíhající
        // proces WebView2. Po půl vteřině ho „proces" pustí.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(dir.join("lockfile"))
            .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            drop(held);
        });
        assert!(remove_dir_retry(&dir));
        assert!(!dir.exists());
        release.join().unwrap();
        // Neexistující složka je v pořádku.
        assert!(remove_dir_retry(&dir));
    }

    #[test]
    fn uklid_stazenych_instalatoru() {
        let dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("keypad-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("KeyPadSetup-abc1234.exe"), b"x").unwrap();
        std::fs::write(dir.join("KeyPadSetup-def5678.exe"), b"x").unwrap();
        remove_update_downloads(&dir);
        assert!(!dir.exists());
        // Neexistující složka nevadí.
        remove_update_downloads(&dir);
    }

    #[test]
    fn nadpis_podle_planu() {
        assert_eq!(headline(&Plan::Fresh, "1"), "KeyPad 1 je nainstalovaný");
        assert_eq!(
            headline(
                &Plan::Update {
                    from: "1".into(),
                    to: "2".into()
                },
                "2"
            ),
            "KeyPad je aktualizovaný na 2"
        );
    }

    #[test]
    fn prepsani_pres_new_nenecha_docasny_soubor() {
        // Pracovní složka v `target\` vedle testovací binárky — test
        // nemá co psát mimo repozitář.
        let dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("keypad-setup-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("KeyPad.exe");
        std::fs::write(&target, b"stary").unwrap();
        replace_file(&target, b"novy").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"novy");
        assert!(!dir.join("KeyPad.exe.new").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

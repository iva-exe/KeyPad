//! KeyPadSetup — jediný soubor, který dostane kamarád.
//!
//! Spuštění bez parametrů = okno s tlačítkem Nainstalovat:
//!   1. zjistí z repozitáře aktuální verzi
//!   2. porovná ji s nainstalovanou (stejná + soubory na místě → nic
//!      se nestahuje, jen se KeyPad spustí)
//!   3. stáhne KeyPad.exe do paměti, zavře běžící KeyPad, přepíše ho
//!   4. udělá zástupce v nabídce Start a záznam v Nastavení → Aplikace
//!   5. když ovladač ViGEmBus chybí, nainstaluje ho, a když je starší
//!      než poslední vydání, aktualizuje ho (viz `driver.rs`)
//!
//! `KeyPadSetup.exe /uninstall` = odeber všechno (ViGEmBus ne).
//! `KeyPadSetup.exe /quiet`     = okno, které se spustí i zavře samo
//!                                (tudy jde aktualizace z aplikace, i s
//!                                ovladačem); platí i pro `/uninstall /quiet`.
//! `KeyPadSetup.exe /vigembus`  = jen ovladač ViGEmBus: rovnou nainstaluje
//!                                chybějící, aktualizuje starší (spouští ho
//!                                aplikace; `/quiet` se s ním ignoruje —
//!                                výsledek zůstane v okně).
//! `KeyPadSetup.exe /headless`  = bez okna, výpis do konzole (skripty);
//!                                s `/vigembus` jen ovladač. Bez něj
//!                                ovladač jen poradí příkaz — skript
//!                                nemá sám od sebe vyvolat výzvu UAC.
//!
//! Kód návratu `/vigembus` (okno i konzole) se skládá z ověřeného stavu:
//! 0 = ovladač běží, 3010 = poběží po restartu, 1 = jinak.
//!
//! Aktualizace ovladače (v každém režimu) nejdřív zavře běžící KeyPad —
//! drží sběrnici — a potom ho zase spustí (`driver.rs`, krok 5).
//!
//! KeyPad se instaluje do profilu uživatele (`%LOCALAPPDATA%\Programs\
//! KeyPad`, HKCU, jeho nabídka Start) — instalátor sám nikdy neběží
//! s právy správce a manifest to říká výslovně (`asInvoker`). Jediná
//! výzva UAC, kterou kdy uvidí uživatel, patří oficiálnímu instalátoru
//! ViGEmBus: jen když ovladač chybí nebo je starší, a jen s ověřeným
//! otiskem a podpisem (`driver.rs`). Výzva je souhlas; odmítnutá
//! instalaci KeyPadu nezkazí.
//!
//! Podsystém je „windows", ne „console": jinak by u grafického
//! instalátoru bliklo černé okno. V headless režimu se konzole rodiče
//! připojí ručně, aby výpis měl kam jít. (Testy běží jako konzolová
//! binárka, ať cargo vidí jejich výstup.)
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod driver;
mod gui;
mod log;
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
/// (ovladač) se ukáže, jen když se s ovladačem něco udělá.
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
const STEP_DRIVER: usize = INSTALL_STEPS.len();

/// Popisek kroku ovladače.
fn driver_step_label(work: driver::Work) -> &'static str {
    match work {
        driver::Work::Install => "Ovladač ViGEmBus",
        driver::Work::Update => "Aktualizace ovladače ViGEmBus",
    }
}

/// Patička úvodní obrazovky — předem a krátce, co se stane (princip 8):
/// KeyPad bez práv správce, ovladač přes výzvu Windows.
const FOOTER: &str = "Do tvého profilu, bez práv správce.";
const FOOTER_DRIVER: &str = "KeyPad bez práv správce. Ovladač ViGEmBus potvrdíš ve výzvě Windows.";
/// Patička odinstalace — co zůstane (ViGEmBus se s KeyPadem neodebírá).
const UNINSTALL_FOOTER: &str = "Tvoje nastavení zůstane.";
const UNINSTALL_FOOTER_BUS: &str = "Tvoje nastavení i ovladač ViGEmBus zůstanou.";

/// Credit ViGEmBus s odkazem na repo autora (texty sdílí s aplikací
/// `updater::vigembus`).
fn credit() -> gui::Credit {
    gui::Credit {
        text: vigembus::CREDIT.into(),
        url: vigembus::REPO_URL.into(),
    }
}

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
    /// Jen zpráva (ladicí náhledy a testy; skutečné výsledky skládá
    /// `outcome` / `driver_done` / odinstalace).
    #[cfg(any(test, debug_assertions))]
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
    /// skripty). Platí pro instalaci i odinstalaci jako od první verze;
    /// instalace v něm ovladač nainstaluje / aktualizuje stejně jako
    /// v okně (souhlas je výzva UAC). Okno se samo zavře jen po čistém
    /// úspěchu. `/vigembus` ho ignoruje: výsledek ovladače (a credit)
    /// v okně zůstane, dokud ho uživatel nezavře.
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

/// Stav ViGEmBus (a „je starší"), podle kterého okno ukáže krok
/// ovladače.
///
/// Ladicí build umí stav předstírat přes proměnnou
/// `KEYPAD_SETUP_TEST_VIGEMBUS` (`chybi`, `bezi`, `stary`,
/// `stary-vypnuty`, `vypnuty`, `blokovany`, `restart`, `bez-zarizeni`,
/// `bez-zaznamu`) — kvůli snímkům obrazovky na PC, kde ViGEmBus je.
/// Release build proměnnou vůbec nečte. A ani v ladicím buildu se podle
/// ní nic neinstaluje: pojistka v `driver::install` čte vždy skutečný
/// stav, takže na PC s aktuálním ovladačem skončí „v pořádku".
fn shown_bus() -> (BusState, bool) {
    #[cfg(debug_assertions)]
    if let Some(s) = std::env::var("KEYPAD_SETUP_TEST_VIGEMBUS")
        .ok()
        .and_then(|v| fake_bus(&v))
    {
        return s;
    }
    driver::current()
}

#[cfg(debug_assertions)]
fn fake_bus(v: &str) -> Option<(BusState, bool)> {
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
        "chybi" => (BusState::NotInstalled, false),
        "bezi" => (BusState::Ready, false),
        "stary" => (BusState::Ready, true),
        "stary-vypnuty" => (there(dev(Some(22), false), true), true),
        "vypnuty" => (there(dev(Some(22), false), true), false),
        "blokovany" => (there(dev(Some(48), false), true), false),
        "restart" => (there(dev(None, true), true), false),
        "bez-zarizeni" => (there(None, true), false),
        // Zbytek bez záznamu v Aplikacích (devcon, nefcon, jiný program).
        "bez-zaznamu" => (there(None, false), false),
        _ => return None,
    })
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
            updater::latest_commit()
                .map(|sha| format!("{m}; HTTPS v pořádku (commit {sha})"))
                .map_err(|e| e.to_string())
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
            let (o, relaunched) = driver_only(&mut rep, 0);
            print_driver(&o);
            if let Some(n) = relaunch_note(&relaunched) {
                println!("  {n}");
            }
            std::process::exit(driver_exit_code(vigembus::state(), Some(o.exit_code())));
        }
        let r = match args.mode {
            Mode::Uninstall => do_uninstall(&mut rep),
            // Skript nikdy nespustí instalátor ovladače sám od sebe (výzva
            // UAC by visela bez člověka) — jen poradí příkaz.
            _ => do_install(&mut rep, false, Missing::Hint),
        };
        match r {
            Ok(done) => {
                println!("\n  {}", done.message.replace('\n', "\n  "));
                if let Some(gui::Next::Link(l)) = &done.next {
                    println!("  {}: {}", l.label, l.url);
                }
                if args.mode == Mode::Uninstall && running_from_install {
                    schedule_self_delete(&own_copy, &dir);
                }
                std::process::exit(0);
            }
            Err(e) => {
                println!("\n  CHYBA: {}", e.replace('\n', "\n  "));
                // Věta je krátká a bez kódů; podrobnosti jsou v logu.
                println!("  Podrobnosti: {}", log::path().display());
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

    let (bus, outdated) = shown_bus();
    // Krok ovladače jen tam, kde se s ním něco udělá (chybí → instalace,
    // starší → aktualizace), v okně i v tichém režimu.
    let work = match args.mode {
        Mode::Install => driver::decide(bus, outdated).ok(),
        _ => None,
    };
    let state = match args.mode {
        Mode::Uninstall => {
            // Poctivý výčet toho, co zmizí (i logy, o které README žádá
            // při hlášení chyby); patička říká, co zůstane.
            gui::State::new(
                "Odebrat KeyPad",
                "smaže aplikaci, zástupce, záznam v Aplikacích, logy a data okna",
                if bus == BusState::NotInstalled {
                    UNINSTALL_FOOTER
                } else {
                    UNINSTALL_FOOTER_BUS
                },
                UNINSTALL_STEPS,
                "Odebrat",
            )
        }
        Mode::Driver => driver_screen(bus, outdated),
        Mode::Install => install_screen(work),
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
    let with_driver = work.is_some();
    let action: gui::Action = Arc::new(move |job, st: gui::Shared, note: gui::Notifier| {
        let mut rep = GuiReport {
            state: Arc::clone(&st),
            note,
        };
        let r = match (mode, job) {
            (Mode::Uninstall, _) => do_uninstall(&mut rep),
            (Mode::Install, gui::Job::Main) => do_install(&mut rep, with_driver, Missing::Link),
            // Ovladač sám: v režimu /vigembus, nebo „Zkusit znovu
            // ovladač" po instalaci KeyPadu.
            (Mode::Driver, _) | (Mode::Install, gui::Job::Driver) => {
                let step = if mode == Mode::Driver { 0 } else { STEP_DRIVER };
                let (o, relaunched) = driver_only(&mut rep, step);
                code.store(o.exit_code(), Ordering::SeqCst);
                Ok(driver_done(&o, &relaunched))
            }
        };
        if r.is_ok() {
            flag.store(true, Ordering::SeqCst);
        }
        // Odinstalace log maže (je mezi logy, které slibuje smazat).
        if mode != Mode::Uninstall {
            match &r {
                Ok(done) => log::line(&format!("hotovo: {}", done.message)),
                Err(e) => log::line(&format!("chyba: {e}")),
            }
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
    // /vigembus spouští aplikace po kliknutí uživatele — začne hned
    // (souhlas je výzva UAC) a výsledek nechá v okně.
    let (autostart, autoclose) = match args.mode {
        Mode::Driver => (true, false),
        _ => (args.quiet, args.quiet),
    };
    gui::run(title, state, action, autostart, autoclose);

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

/// Úvodní obrazovka instalace: kroky KeyPadu, k nim krok ovladače
/// a credit, když se s ovladačem něco udělá.
fn install_screen(work: Option<driver::Work>) -> gui::State {
    let mut steps = INSTALL_STEPS.to_vec();
    steps.extend(work.map(driver_step_label));
    let st = gui::State::new(
        "KeyPad",
        "klávesnice jako Xbox ovladač",
        if work.is_some() {
            FOOTER_DRIVER
        } else {
            FOOTER
        },
        &steps,
        "Nainstalovat",
    );
    if work.is_some() {
        st.with_credit(credit())
    } else {
        st
    }
}

/// Výsledek ovladače do konzole: věta, podrobnosti (kódy), adresa ruční
/// instalace a credit.
fn print_driver(o: &driver::Outcome) {
    println!("\n  {}", o.message().replace('\n', "\n  "));
    if !o.detail().is_empty() {
        println!("  ({})", o.detail());
    }
    if o.manual_install() {
        println!("  Ruční instalace: {}", vigembus::RELEASES_URL);
    }
    println!(
        "  {} · {} · licence {}",
        vigembus::CREDIT,
        vigembus::REPO_URL,
        vigembus::LICENSE
    );
}

/// „Žádný pokus o ovladač neproběhl" v `driver_code`.
const NO_ATTEMPT: i32 = -1;

/// Kód návratu režimu `/vigembus` (okno i konzole) — podle něj aplikace
/// pozná, jestli má gamepad zkusit připojit znovu: 0 = ovladač běží,
/// 3010 = poběží po restartu, 1 = neběží. Na konci se čte skutečný stav
/// znovu: běžící ovladač je 0 i bez pokusu (nebo když aktualizace
/// neprošla a starší běží dál) — jen ne, když poslední pokus řekl
/// „restartuj" (aktualizace nedoběhla, běží ještě starý). Jinak výsledek
/// posledního pokusu, a bez pokusu to, co by řekla pojistka.
fn driver_exit_code(now: BusState, last_attempt: Option<i32>) -> i32 {
    match (now, last_attempt) {
        (BusState::Ready, Some(3010)) => 3010,
        (BusState::Ready, _) => 0,
        (_, Some(c)) => c,
        (s, None) => driver::decide(s, false).err().map_or(1, |o| o.exit_code()),
    }
}

/// Obrazovka režimu `/vigembus` podle stavu ovladače: je co dělat →
/// krok a credit (okno začne samo); jinak rovnou výsledek (v pořádku,
/// nebo rada — stejná pojistka jako v `driver::install`).
fn driver_screen(bus: BusState, outdated: bool) -> gui::State {
    let (title, subtitle) = ("Ovladač ViGEmBus", "virtuální Xbox ovladač pro KeyPad");
    match driver::decide(bus, outdated) {
        Ok(work) => gui::State::new(
            title,
            subtitle,
            "",
            &[driver_step_label(work)],
            match work {
                driver::Work::Install => "Nainstalovat",
                driver::Work::Update => "Aktualizovat",
            },
        )
        .with_credit(credit()),
        Err(o) => {
            let mut st = gui::State::new(title, subtitle, "", &[], "");
            st.finish(&o.message(), o.needs_attention(), driver_next(&o));
            // Nic neproběhlo — plný pruh průběhu by lhal.
            st.progress = None;
            st
        }
    }
}

/// Ovladač sám (`/vigembus`, „Zkusit znovu ovladač"). Když aktualizace
/// zavřela běžící KeyPad (drží sběrnici — `driver.rs`, krok 5), spustí
/// ho zase: kdo KeyPad zavřel, ten ho vrací. (Instalace KeyPadu ho na
/// konci spouští sama, viz `launch_and_report`.) Vrací výsledek ovladače
/// a výsledek nového spuštění (`None` = KeyPad se nezavíral).
fn driver_only(rep: &mut dyn Report, step: usize) -> (driver::Outcome, Option<Result<(), String>>) {
    let ran = driver::install(rep, step);
    let relaunched = ran.closed_app.then(|| {
        let r = launch(&updater::install_dir());
        match &r {
            Ok(()) => log::line("KeyPad po aktualizaci ovladače znovu spuštěn"),
            Err(e) => log::line(&format!("KeyPad se po aktualizaci ovladače nespustil: {e}")),
        }
        r
    });
    (ran.outcome, relaunched)
}

/// Věta, když se KeyPad zavřený kvůli aktualizaci ovladače nespustil
/// znovu (důvod je v logu); jinak nic.
fn relaunch_note(relaunched: &Option<Result<(), String>>) -> Option<&'static str> {
    matches!(relaunched, Some(Err(_)))
        .then_some("KeyPad se znovu nespustil — spusť ho z nabídky Start.")
}

/// Závěr samotného ovladače (`/vigembus`, „Zkusit znovu ovladač").
/// KeyPad, který se po aktualizaci nevrátil, musí zůstat na očích.
fn driver_done(o: &driver::Outcome, relaunched: &Option<Result<(), String>>) -> Done {
    let mut done = Done {
        message: o.message(),
        attention: o.needs_attention(),
        next: driver_next(o),
    };
    if let Some(n) = relaunch_note(relaunched) {
        done.message.push('\n');
        done.message.push_str(n);
        done.attention = true;
    }
    done
}

/// Tlačítko po ovladači: znovu, kde to má smysl (zrušená výzva, síť,
/// zaneprázdněný instalátor Windows, smazaná složka rozbalování, KeyPad,
/// který nešel zavřít, ovladač, po kterém aktualizace nic nenechala), odkaz
/// na ruční instalaci, kde vede cesta tudy (soubor není ten oficiální,
/// instalátor nic nenainstaloval, ovladač bez zařízení a bez záznamu
/// v Aplikacích), jinak nic.
fn driver_next(o: &driver::Outcome) -> Option<gui::Next> {
    if o.retry_makes_sense() {
        Some(gui::Next::Driver("Zkusit znovu ovladač".into()))
    } else if o.manual_install() {
        Some(gui::Next::Link(vigembus_link()))
    } else {
        None
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
/// `ovladac-hotovo` | `ovladac-aktualizovan` | `aktualizace-hotova` |
/// `restart` | `nestazeno` | `aktualizace-bez-zarizeni` |
/// `aktualizace-vypnuty` | `aktualizace-blokovany` | `aktualizace-odebrala` |
/// `keypad-nejde-zavrit` | `keypad-nespusten` | `chyba-site` |
/// `chyba-souboru` | `odinstalovano`. Nic se neinstaluje ani nemaže —
/// jen se ukáže okno s výsledkem, jaký by složila skutečná instalace
/// (výsledky po instalátoru ovladače jdou přes `driver::preview_settle`,
/// tedy přes skutečné rozhodnutí). `preteceni` = nejdelší zpráva a k ní
/// všechny poznámky „Pozor:" (profil bez práva zápisu) — kontrola, že
/// okno přejde na menší písmo místo useknutí (`gui::Fit`).
#[cfg(debug_assertions)]
fn preview(which: &str) {
    use driver::Work;
    let action: gui::Action = Arc::new(|_, _, _| {});
    let install = Some(Work::Install);
    let update = Some(Work::Update);
    let preview_update = Plan::Update {
        from: "0.1.0+20260924.2345".into(),
        to: "0.1.0+20260929.2241".into(),
    };
    // Závěr instalace KeyPadu (aktualizace) s daným výsledkem ovladače.
    let after_update = |o: driver::Outcome| {
        Ok(outcome(
            &headline(&preview_update),
            true,
            Some(Ok(())),
            &Driver::Step(o),
            &[],
        ))
    };
    // Závěr režimu /vigembus.
    let alone = |o: driver::Outcome| Ok(driver_done(&o, &None));
    let (title, work, done): (&str, Option<Work>, Result<Done, String>) = match which {
        "nejdelsi" => ("KeyPad", install, Ok(longest_done(Plan::Fresh))),
        // Kombinace z review: chybí WebView2 + odmítnutá složka
        // rozbalování + „Pozor:" (dřív se useknul konec).
        "odmitnuta-slozka" => {
            let refused = driver::sample_outcomes()
                .into_iter()
                .find(|o| o.message().contains("brání složka"))
                .expect("odmítnutá složka mezi ukázkami");
            let notes = [setup_copy_note()];
            (
                "KeyPad",
                install,
                Ok(outcome(
                    &headline(&Plan::Fresh),
                    false,
                    None,
                    &Driver::Step(refused),
                    &notes,
                )),
            )
        }
        "preteceni" => {
            let mut d = longest_done(Plan::Fresh);
            for n in [shortcut_note(), arp_note()] {
                d.message.push_str("\nPozor: ");
                d.message.push_str(&n);
            }
            ("KeyPad", install, Ok(d))
        }
        "ovladac-zrusen" => (
            "KeyPad",
            update,
            after_update(driver::Outcome::Cancelled { update: true }),
        ),
        "aktualizace-hotova" => (
            "KeyPad",
            update,
            after_update(driver::Outcome::Updated { restart: true }),
        ),
        "restart" => (
            "KeyPad",
            update,
            after_update(driver::Outcome::RebootRequired),
        ),
        // Skutečný stav po upgradu, který starou verzi odebral a novou
        // nepřidal: záznam v Aplikacích, zařízení žádné, MSI 3010.
        "aktualizace-bez-zarizeni" => (
            "KeyPad",
            update,
            after_update(driver::preview_settle(Work::Update, 3010, None, false)),
        ),
        // Neprošlá aktualizace (1603) vypnutého / zablokovaného ovladače.
        "aktualizace-vypnuty" => (
            "Ovladač ViGEmBus",
            update,
            alone(driver::preview_settle(Work::Update, 1603, Some(22), true)),
        ),
        "aktualizace-blokovany" => (
            "Ovladač ViGEmBus",
            update,
            alone(driver::preview_settle(Work::Update, 1603, Some(48), true)),
        ),
        // Po aktualizaci po ovladači nic (ani záznam v Aplikacích).
        "aktualizace-odebrala" => ("Ovladač ViGEmBus", update, alone(driver::preview_removed())),
        "keypad-nejde-zavrit" => (
            "Ovladač ViGEmBus",
            update,
            alone(
                driver::sample_outcomes()
                    .into_iter()
                    .find(|o| o.message().contains("KeyPad nejde zavřít"))
                    .expect("KeyPad nejde zavřít mezi ukázkami"),
            ),
        ),
        // Aktualizace ovladače KeyPad zavřela a ten se znovu nespustil.
        "keypad-nespusten" => (
            "Ovladač ViGEmBus",
            update,
            Ok(driver_done(
                &driver::Outcome::Updated { restart: true },
                &Some(Err("přístup odepřen".into())),
            )),
        ),
        "chyba-site" => (
            "KeyPad",
            None,
            Err(untouched(
                updater::ReleaseError::Http {
                    file: None,
                    error: updater::http::Error::Connect("odeslání požadavku".into()),
                }
                .sentence(),
            )),
        ),
        "chyba-souboru" => ("KeyPad", None, Err(untouched(updater::BAD_DOWNLOAD))),
        "nestazeno" => (
            "Ovladač ViGEmBus",
            install,
            alone(
                driver::sample_outcomes()
                    .into_iter()
                    .find(|o| o.manual_install() && matches!(o, driver::Outcome::Failed { .. }))
                    .expect("neoficiální soubor mezi ukázkami"),
            ),
        ),
        "ovladac-hotovo" => (
            "Ovladač ViGEmBus",
            install,
            alone(driver::Outcome::Installed),
        ),
        "ovladac-aktualizovan" => (
            "Ovladač ViGEmBus",
            update,
            Ok(driver_done(
                &driver::Outcome::Updated { restart: false },
                &Some(Ok(())),
            )),
        ),
        _ => (
            "Odebrat KeyPad",
            None,
            Ok(Done::plain(uninstall_message(
                true,
                None,
                None,
                BusLeft::InApps,
            ))),
        ),
    };
    let labels: Vec<&str> = match title {
        "Odebrat KeyPad" => UNINSTALL_STEPS.to_vec(),
        "Ovladač ViGEmBus" => work.map(driver_step_label).into_iter().collect(),
        _ => INSTALL_STEPS
            .iter()
            .copied()
            .chain(work.map(driver_step_label))
            .collect(),
    };
    let mut st = gui::State::new(title, "náhled (ladicí build)", "", &labels, "");
    if work.is_some() {
        st = st.with_credit(credit());
    }
    match done {
        Ok(done) => st.finish(&done.message, done.attention, done.next),
        // Chyba stažení přichází v prvním kroku (zjištění verze).
        Err(e) => {
            st.step(0, "");
            st.fail(&e);
        }
    }
    gui::run(
        "KeyPad — náhled",
        Arc::new(Mutex::new(st)),
        action,
        false,
        false,
    );
}

/// Nejdelší skutečná závěrečná zpráva instalace: chybí WebView2,
/// nejdelší možný výsledek kroku ovladače (všechny skutečné varianty
/// z `driver::sample_outcomes`, změřené písmem okna) a poznámka „Pozor:".
/// Pro ladicí náhled `nejdelsi` i test, že se vejde do okna.
#[cfg(any(test, debug_assertions))]
fn longest_done(plan: Plan) -> Done {
    let notes = [setup_copy_note()];
    driver::sample_outcomes()
        .into_iter()
        .map(|o| outcome(&headline(&plan), false, None, &Driver::Step(o), &notes))
        .max_by_key(|d| {
            (
                gui::message_height_96(&d.message, INSTALL_STEPS.len() + 1, true),
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

/// Jak zní úspěch pro daný plán („KeyPad je …"). Bez čísla verze —
/// razítko buildu uživateli nic neřekne; verzi ukazuje popisek kroku
/// během instalace, konzole a log.
fn headline(plan: &Plan) -> String {
    match plan {
        Plan::UpToDate => "KeyPad je aktuální",
        Plan::Repair => "KeyPad je opravený",
        Plan::Update { .. } => "KeyPad je aktualizovaný",
        Plan::Fresh => "KeyPad je nainstalovaný",
    }
    .into()
}

/// Jak připomenout ovladač ViGEmBus, se kterým se tentokrát nic nedělalo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Missing {
    /// Okno, kde se krok ovladače neukázal (při startu okna ovladač
    /// nechyběl a mezitím zmizel): odkaz na ruční instalaci; stav hlásí
    /// i aplikace.
    Link,
    /// Konzole: příkaz, kterým ho uživatel nainstaluje / aktualizuje.
    Hint,
}

/// Co je s ovladačem ViGEmBus na konci instalace KeyPadu.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Driver {
    /// Běží, krok se nespouštěl.
    Ready,
    /// Chybí a krok se nespouštěl.
    Missing(Missing),
    /// Je starší a krok se nespouštěl (jen konzole — okno ho aktualizuje).
    Outdated,
    /// Je, ale neběží — rada, co s tím.
    NotRunning(vigembus::Advice),
    /// Krok ovladače proběhl s tímto výsledkem.
    Step(driver::Outcome),
}

impl Driver {
    /// Věta do závěrečné zprávy; `None` = není co říkat.
    fn note(&self) -> Option<String> {
        match self {
            Driver::Ready => None,
            Driver::Missing(Missing::Link) => {
                Some("Chybí ovladač ViGEmBus — bez něj KeyPad nevytvoří gamepad.".into())
            }
            Driver::Missing(Missing::Hint) => Some(format!(
                "Chybí ovladač ViGEmBus — nainstaluješ ho příkazem KeyPadSetup.exe \
                 {SETUP_ARG_VIGEMBUS}."
            )),
            Driver::Outdated => Some(format!(
                "Ovladač ViGEmBus je starší — aktualizuješ ho příkazem KeyPadSetup.exe \
                 {SETUP_ARG_VIGEMBUS}."
            )),
            Driver::NotRunning(advice) => Some(advice.text()),
            Driver::Step(o) => Some(o.message()),
        }
    }

    /// Tlačítko, které k ovladači nabídnout.
    fn next(&self) -> Option<gui::Next> {
        match self {
            Driver::Missing(Missing::Link) => Some(gui::Next::Link(vigembus_link())),
            Driver::NotRunning(advice) => advice_next(advice),
            Driver::Step(o) => driver_next(o),
            _ => None,
        }
    }

    /// Musí to uživatel vědět, než okno zavře? Ovladač, který se měl
    /// nainstalovat / aktualizovat a nepovedlo se to (nebo čeká na
    /// restart), ano — i v tichém režimu. Dávno chybějící ne — to hlásí
    /// i aplikace.
    fn attention(&self) -> bool {
        matches!(self, Driver::Step(o) if o.needs_attention())
    }
}

/// Závěrečná zpráva podle stavu systému — krátce; adresy jsou
/// v tlačítku (okno) nebo pod zprávou (konzole), podrobnosti v logu.
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
        Done {
            message: format!(
                "{headline}, ale chybí Microsoft Edge WebView2 Runtime — bez něj se KeyPad \
                 neotevře. Nainstaluj ho a spusť KeyPadSetup znovu."
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
        // antivirus). Důvod jde do logu (`launch_and_report`).
        let failed_launch = matches!(launched, Some(Err(_)));
        Done {
            message: if failed_launch {
                format!("Hotovo — {headline}, jen se nespustil. Spusť ho z nabídky Start.")
            } else {
                format!("Hotovo — {headline} a běží. Najdeš ho v nabídce Start.")
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

/// Chyba z doby, kdy se ještě na nic nesáhlo: krátká věta bez kódů
/// a k ní, že stará instalace (pokud byla) zůstala celá a funkční.
fn untouched(sentence: &str) -> String {
    format!("{sentence}\nNa disku se nic nezměnilo.")
}

/// Stažení se nepovedlo (síť, server, nesmyslná odpověď): do okna jen
/// věta, podrobnosti (`what` a chyba WinHttp / HTTP) do logu.
fn download_failed(what: &str, e: &updater::ReleaseError) -> String {
    log::line(&format!("{what}: {e} ({e:?})"));
    download_message(e)
}

/// Co z chyby stažení uvidí okno.
fn download_message(e: &updater::ReleaseError) -> String {
    untouched(e.sentence())
}

/// Stažený obsah nedává smysl (prázdná verze, místo binárky chybová
/// stránka); `detail` jde do logu.
fn bad_download(detail: &str) -> String {
    log::line(detail);
    untouched(updater::BAD_DOWNLOAD)
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
    let sha =
        updater::latest_commit().map_err(|e| download_failed("zjištění posledního commitu", &e))?;
    let version = updater::fetch_release_file(&sha, VERSION_FILE, |_| {})
        .map_err(|e| download_failed("zjištění verze", &e))?;
    let version = String::from_utf8_lossy(&version).trim().to_string();
    if version.is_empty() {
        return Err(bad_download(&format!(
            "{VERSION_FILE} v commitu {sha} je prázdný"
        )));
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
            return Ok(launch_and_report(&dir, &headline(&decision), notes, &drv));
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
    .map_err(|e| download_failed("stažení", &e))?;
    if !updater::looks_like_exe(&data) {
        return Err(bad_download(&format!(
            "{APP_EXE} se stáhl poškozený ({} B)",
            data.len()
        )));
    }
    rep.status(&format!("{APP_EXE} — {}", mb(data.len())));
    rep.progress(Some(0.5));

    // ── 3. Zavřít běžící KeyPad ────────────────────────────────────
    // Běžící proces drží vlastní .exe zamčený — přepsat ho nejde.
    rep.step(2, "hledám běžící KeyPad…");
    rep.progress(None);
    let closed = proc::close_app(&app, QUIT_EVENT_NAME, &mut |s| rep.status(s))
        .map_err(|e| untouched(&e))?;

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
            return Err(relaunched_after_failure(&e));
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

    // ── 6. Ovladač ViGEmBus (chybí → instalace, starší → aktualizace) ──
    let drv = driver_step(rep, with_driver, missing);

    Ok(launch_and_report(&dir, &headline(&decision), notes, &drv))
}

/// Krok ovladače: nainstalovat / aktualizovat (když ho okno ukázalo),
/// nebo jen zjistit, jak na tom je.
///
/// Běží PŘED spuštěním KeyPadu — aplikace pak sběrnici najde hned při
/// startu. Při aktualizaci KeyPadu je v tu chvíli zavřený (krok 3);
/// když se KeyPad nestahoval (stejná verze), aktualizace ovladače ho
/// zavře sama (drží sběrnici, `driver.rs` krok 5). Znovu ho v obou
/// případech spustí `launch_and_report`. Výsledek ovladače instalaci
/// KeyPadu nikdy neshodí: KeyPad je v tu chvíli zapsaný
/// a zaregistrovaný, ovladač se dá zkusit znovu.
fn driver_step(rep: &mut dyn Report, with_driver: bool, missing: Missing) -> Driver {
    if with_driver {
        return Driver::Step(driver::install(rep, STEP_DRIVER).outcome);
    }
    let (state, outdated) = driver::current();
    match driver::decide(state, outdated) {
        Ok(driver::Work::Install) => Driver::Missing(missing),
        Ok(driver::Work::Update) if missing == Missing::Hint => Driver::Outdated,
        // Okno bez kroku ovladače (stav se změnil až za běhu): starší,
        // ale běžící ovladač počká na příští spuštění instalátoru.
        Ok(driver::Work::Update) => match state.advice() {
            Some(a) => Driver::NotRunning(a),
            None => Driver::Ready,
        },
        Err(o) if o.is_ready() => Driver::Ready,
        Err(driver::Outcome::NotRunning(a)) => Driver::NotRunning(a),
        Err(_) => Driver::NotRunning(vigembus::Advice::Restart),
    }
}

/// Zkontroluje prostředí, případně spustí aplikaci, a složí zprávu.
fn launch_and_report(dir: &Path, headline: &str, notes: Vec<String>, drv: &Driver) -> Done {
    let webview2 = prereq::webview2_present();
    let launched = webview2.then(|| launch(dir));
    if !webview2 {
        log::line("WebView2 Runtime chybí — KeyPad se nespouští");
    }
    if let Some(Err(e)) = &launched {
        log::line(&format!("KeyPad se nespustil: {e}"));
    }
    outcome(headline, webview2, launched, drv, &notes)
}

/// Zapíše KeyPad.exe, kopii instalátoru a nakonec verzi.
fn write_files(
    dir: &Path,
    exe: &[u8],
    version: &str,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| {
        log::line(&format!("{}: {e}", dir.display()));
        cant_create_dir(dir)
    })?;
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
    log::line(&format!("{}: {last}", path.display()));
    Err(cant_replace(path))
}

// Chybové věty zápisu — cesta v nich je, protože s ní uživatel jedná;
// chyba Windows (kód) jde do logu. Zvlášť kvůli testu bez kódů.

fn cant_create_dir(dir: &Path) -> String {
    format!("Nelze vytvořit složku {}.", dir.display())
}

fn cant_replace(path: &Path) -> String {
    format!(
        "{} nejde přepsat — nejspíš ho pořád něco drží. Zavři KeyPad a zkus to znovu.",
        path.display()
    )
}

fn cant_write(path: &Path) -> String {
    format!("Nelze zapsat {}.", path.display())
}

/// Dovětek, když se po nepovedeném zápisu KeyPad zase spustil.
fn relaunched_after_failure(e: &str) -> String {
    format!("{e}\nKeyPad jsem zase spustil, ať nezůstaneš bez aplikace.")
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
    log::line(&format!("{}: {last}", path.display()));
    Err(cant_write(path))
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
            log::line(&format!("kopie instalátoru: {e}"));
            notes.push(setup_copy_note());
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
    log::line(&format!("kopie instalátoru {}: {last}", dst.display()));
    notes.push(setup_copy_note());
}

/// Poznámka „Pozor:", když se kopie instalátoru nezapsala (sdílí ji
/// instalace a náhled nejdelší zprávy). Příkaz v ní je místo, kde
/// uživatel jednat musí; chyba Windows jde do logu.
fn setup_copy_note() -> String {
    "kopie instalátoru se nezapsala — odinstaluješ příkazem KeyPadSetup.exe /uninstall".into()
}

/// Zástupce v nabídce Start a záznam v Nastavení → Aplikace. Obojí je
/// pohodlí navíc — selhání instalaci nezastaví, jen se ohlásí (chyba
/// do logu).
fn register(dir: &Path, version: &str, notes: &mut Vec<String>) {
    if let Err(e) = shell::create_shortcut(&dir.join(APP_EXE), &shell::start_menu_lnk()) {
        log::line(&format!("zástupce: {e}"));
        notes.push(shortcut_note());
    }
    let size_kb = [APP_EXE, SETUP_EXE]
        .iter()
        .filter_map(|n| std::fs::metadata(dir.join(n)).ok())
        .map(|m| (m.len() / 1024) as u32)
        .sum();
    if let Err(e) = shell::register_uninstall(&dir.join(SETUP_EXE), dir, version, size_kb) {
        log::line(&format!("záznam v Aplikacích: {e}"));
        notes.push(arp_note());
    }
}

/// Poznámky „Pozor:" k zápisům do systému (sdílí je instalace a ladicí
/// náhled přetečení).
fn shortcut_note() -> String {
    "zástupce v nabídce Start se nevytvořil.".into()
}

fn arp_note() -> String {
    "záznam v Nastavení → Aplikace se nezapsal.".into()
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
    // Zbytky přerušené instalace ovladače v %TEMP% (jen naše soubory)
    // a log instalátoru.
    driver::remove_leftovers();
    let _ = std::fs::remove_file(log::path());

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
    Ok(Done {
        message: uninstall_message(
            found,
            config_left.then_some(dir.as_path()),
            webview_left.as_deref(),
            bus,
        ),
        // Nesmazaná data okna chtějí ruční zásah — v tichém režimu
        // (`/uninstall /quiet`) by jinak okno zmizelo i s pokynem.
        attention: webview_left.is_some(),
        next: None,
    })
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
        // Cesta zůstává: kdo nastavení nechce, smaže ho ručně.
        (true, Some(dir)) => format!(
            "KeyPad je odebraný. Tvoje nastavení zůstalo v {}.",
            dir.display()
        ),
        (true, None) => String::from("KeyPad je odebraný."),
    };
    // Není to chyba (aplikace je pryč), ale uživatel má vědět, kde
    // zbyly megabajty, které čekal smazané.
    if let Some(wv) = webview_left {
        msg.push_str(&format!(
            "\nData okna v {} se nesmazala celá — smaž tu složku ručně.",
            wv.display()
        ));
    }
    match bus {
        BusLeft::No => {}
        BusLeft::InApps => msg.push_str(
            "\nOvladač ViGEmBus zůstává (může ho používat i jiný program) — odebereš ho \
             v Aplikacích jako „ViGEm Bus Driver“.",
        ),
        BusLeft::Elsewhere => {
            msg.push_str("\nOvladač ViGEmBus zůstává (může ho používat i jiný program).")
        }
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
    // Selhání přijde dřív, než odinstalace log smaže — důvod v něm zůstane.
    log::line(&format!("{}: {last}", path.display()));
    Err(format!(
        "{} nejde smazat — zavři KeyPad a zkus to znovu.",
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
    const H: &str = "KeyPad je nainstalovaný";

    #[test]
    fn bez_webview2_se_nespousti_a_okno_zustane() {
        let d = outcome(H, false, None, &READY, &[]);
        assert!(d.attention);
        assert!(d.message.contains("WebView2"));
        assert!(d.message.contains("KeyPadSetup znovu"));
        // Adresa je v tlačítku (z GDI okna se text zkopírovat nedá).
        assert!(!d.message.contains("http"));
        assert_eq!(url(d.next), Some(prereq::WEBVIEW2_URL.to_string()));
    }

    #[test]
    fn chybejici_vigembus_bez_kroku_neni_chyba() {
        let d = outcome(H, true, Some(Ok(())), &MISSING_LINK, &[]);
        assert!(!d.attention);
        assert!(d.message.starts_with("Hotovo"));
        assert!(d.message.contains("Chybí ovladač ViGEmBus"));
        assert_eq!(url(d.next), Some(vigembus::RELEASES_URL.to_string()));
    }

    #[test]
    fn chybi_oboji_prednost_ma_webview2() {
        let d = outcome(H, false, None, &MISSING_LINK, &[]);
        assert!(d.attention);
        assert!(d.message.contains("Chybí ovladač ViGEmBus"));
        assert_eq!(url(d.next), Some(prereq::WEBVIEW2_URL.to_string()));
    }

    #[test]
    fn vse_v_poradku_bez_odkazu() {
        let d = outcome("KeyPad je aktuální", true, Some(Ok(())), &READY, &[]);
        assert_eq!(
            d,
            Done::plain("Hotovo — KeyPad je aktuální a běží. Najdeš ho v nabídce Start.")
        );
    }

    #[test]
    fn nepovedene_spusteni_a_poznamky_jsou_ve_zprave() {
        let d = outcome(
            H,
            true,
            Some(Err("přístup odepřen (os error 5)".into())),
            &READY,
            &["zástupce nevznikl".into()],
        );
        assert!(d.message.contains("jen se nespustil"));
        // Chyba Windows jde do logu, ne do okna.
        assert!(!d.message.contains("os error"));
        assert!(d.message.ends_with("Pozor: zástupce nevznikl"));
        assert!(d.attention);
    }

    /// Tichý režim (aktualizace z aplikace) zavře okno jen u čistého
    /// úspěchu. Nespuštěný KeyPad, „Pozor:" nebo ovladač, který se
    /// nenainstaloval, musí zůstat na očích.
    #[test]
    fn nespusteni_nebo_poznamka_nechaji_okno_otevrene() {
        let h = "KeyPad je aktualizovaný";
        assert!(outcome(h, true, Some(Err("blokováno".into())), &READY, &[]).attention);
        assert!(outcome(h, true, Some(Ok(())), &READY, &["zástupce nevznikl".into()]).attention);
        let d = outcome(h, true, Some(Err("x".into())), &MISSING_LINK, &[]);
        assert!(d.attention);
        assert_eq!(url(d.next), Some(vigembus::RELEASES_URL.to_string()));
        assert!(!outcome(h, true, Some(Ok(())), &READY, &[]).attention);
        // Aktualizace ovladače odmítnutá / nutný restart (i když nová
        // verze už je zapsaná): zůstane otevřené.
        for o in [
            driver::Outcome::Cancelled { update: true },
            driver::Outcome::RebootRequired,
            driver::Outcome::Updated { restart: true },
        ] {
            assert!(outcome(h, true, Some(Ok(())), &Driver::Step(o), &[]).attention);
        }
        // Aktualizovaný ovladač bez restartu: čistý úspěch, okno se samo zavře.
        let d = outcome(
            h,
            true,
            Some(Ok(())),
            &Driver::Step(driver::Outcome::Updated { restart: false }),
            &[],
        );
        assert!(!d.attention && d.next.is_none());
    }

    /// Instalace KeyPadu se povede i se zrušenou výzvou UAC: zpráva to
    /// řekne krátce a hlavní tlačítko nabídne ovladač znovu (jen ovladač).
    #[test]
    fn zruseny_ovladac_nezrusi_keypad_a_nabidne_opakovani() {
        let d = outcome(
            H,
            true,
            Some(Ok(())),
            &Driver::Step(driver::Outcome::Cancelled { update: false }),
            &[],
        );
        assert!(d
            .message
            .starts_with("Hotovo — KeyPad je nainstalovaný a běží."));
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
            H,
            true,
            Some(Ok(())),
            &Driver::Step(
                driver::sample_outcomes()
                    .into_iter()
                    .find(|o| o.manual_install() && !o.detail().is_empty())
                    .unwrap(),
            ),
            &[],
        );
        assert!(d.attention);
        assert!(!d.message.contains("podrobnosti"), "{}", d.message);
        assert_eq!(url(d.next), Some(vigembus::RELEASES_URL.to_string()));
    }

    #[test]
    fn nainstalovany_ovladac_se_potvrdi() {
        let d = outcome(
            H,
            true,
            Some(Ok(())),
            &Driver::Step(driver::Outcome::Installed),
            &[],
        );
        assert!(!d.attention);
        assert!(d.next.is_none());
        assert!(d.message.ends_with("Ovladač ViGEmBus je nainstalovaný."));
    }

    /// Konzole ovladač sama nespouští (výzva UAC bez člověka) — jen
    /// poradí příkaz, u chybějícího i u staršího.
    #[test]
    fn konzole_jen_poradi_prikaz() {
        for drv in [Driver::Missing(Missing::Hint), Driver::Outdated] {
            let d = outcome("h", true, Some(Ok(())), &drv, &[]);
            assert!(
                d.message.contains("KeyPadSetup.exe /vigembus"),
                "{}",
                d.message
            );
            assert!(d.next.is_none());
            assert!(!d.attention);
        }
    }

    #[test]
    fn nebezici_ovladac_jen_poradi() {
        let advice = vigembus::Advice::EnableDevice;
        let d = outcome(H, true, Some(Ok(())), &Driver::NotRunning(advice), &[]);
        assert!(d.message.ends_with(&advice.text()));
        assert!(d.next.is_none(), "nic se neinstaluje, žádné tlačítko");
        // Rada vedoucí na ruční instalaci (bez záznamu v Aplikacích)
        // dostane tlačítko s odkazem — adresu z okna zkopírovat nejde.
        let d = outcome(
            H,
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
        // /vigembus výsledek v okně nechá — /quiet se s ním ignoruje.
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
        assert!(m.contains("Ovladač ViGEmBus zůstává"));
        assert!(m.contains("v Aplikacích jako „ViGEm Bus Driver“"));
        // Bez záznamu v Aplikacích tam uživatele neposílat.
        let m = uninstall_message(true, None, None, BusLeft::Elsewhere);
        assert!(m.contains("Ovladač ViGEmBus zůstává"));
        assert!(!m.contains("Aplikacích"));
        assert!(!uninstall_message(true, None, None, BusLeft::No).contains("ViGEmBus"));
        let m = uninstall_message(false, None, None, BusLeft::No);
        assert!(m.contains("nebylo co odebírat"));
        let d = Path::new(r"C:\x\KeyPad");
        assert!(uninstall_message(true, Some(d), None, BusLeft::No).contains(r"C:\x\KeyPad"));
        let wv = Path::new(r"C:\x\cz.hexel.keypad");
        assert!(uninstall_message(true, None, Some(wv), BusLeft::No).contains("smaž tu složku"));
    }

    /// Úvodní obrazovka: krok ovladače (instalace / aktualizace) a credit
    /// s odkazem jen tam, kde se s ovladačem něco udělá; patička předem
    /// řekne, že ovladač chce výzvu Windows.
    #[test]
    fn uvodni_obrazovka_podle_ovladace() {
        let st = install_screen(Some(driver::Work::Install));
        assert_eq!(st.steps.len(), INSTALL_STEPS.len() + 1);
        assert_eq!(st.steps[STEP_DRIVER].0, "Ovladač ViGEmBus");
        let c = st.credit.as_ref().expect("credit");
        assert_eq!(c.text, "ViGEmBus — Nefarius Software Solutions e.U.");
        assert_eq!(c.url, "https://github.com/nefarius/ViGEmBus");
        assert!(st.footer.contains("výzvě Windows"));

        let st = install_screen(Some(driver::Work::Update));
        assert_eq!(st.steps[STEP_DRIVER].0, "Aktualizace ovladače ViGEmBus");
        assert!(st.credit.is_some());

        let st = install_screen(None);
        assert_eq!(st.steps.len(), INSTALL_STEPS.len());
        assert!(st.credit.is_none());
        assert!(!st.footer.contains("ViGEmBus"));
        assert_eq!(STEP_DRIVER, INSTALL_STEPS.len());
    }

    /// Patičky se vejdou vedle tlačítek do jejich výšky (změřeno GDI
    /// jako v okně) — delší text by vyjel pod okraj okna.
    #[test]
    fn paticky_se_vejdou_vedle_tlacitek() {
        for f in [
            FOOTER,
            FOOTER_DRIVER,
            UNINSTALL_FOOTER,
            UNINSTALL_FOOTER_BUS,
        ] {
            assert!(gui::footer_fits_96(f), "{f}");
        }
        // Kontrola, že měření něco měří: tři řádky se nevejdou.
        assert!(!gui::footer_fits_96(&[FOOTER_DRIVER; 3].join(" ")));
    }

    /// Kód okna /vigembus: běžící ovladač 0 (i starší, když aktualizace
    /// neprošla) — kromě „restartuj" z posledního pokusu; jinak poslední
    /// pokus; bez pokusu pojistka (čeká na restart → 3010, jinak 1).
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
        assert_eq!(driver_exit_code(BusState::Ready, Some(3010)), 3010);
        assert_eq!(driver_exit_code(BusState::NotInstalled, None), 1);
        assert_eq!(driver_exit_code(BusState::NotInstalled, Some(1)), 1);
        assert_eq!(driver_exit_code(restart, None), 3010);
        assert_eq!(driver_exit_code(restart, Some(3010)), 3010);
        assert_eq!(driver_exit_code(broken, None), 1);
    }

    /// Nejdelší skutečná závěrečná zpráva (chybí WebView2 + nejdelší
    /// výsledek ovladače + „Pozor:") se při 100 % DPI vejde do okna
    /// s šesti kroky a creditem běžným písmem — změřeno GDI stejně jako
    /// při kreslení. Delší texty v driver.rs / main.rs tenhle test shodí;
    /// okno by pak přešlo na menší písmo (viz `gui::Fit`).
    #[test]
    fn nejdelsi_zprava_se_vejde_do_okna() {
        let d = longest_done(Plan::Fresh);
        assert!(d.message.contains("WebView2"));
        assert!(d.message.contains("\nPozor: "));
        assert_eq!(
            gui::message_fit_96(&d.message, INSTALL_STEPS.len() + 1, true),
            gui::Fit::Body,
            "{}",
            d.message
        );
    }

    /// `/vigembus`: je co dělat → krok, credit a hned start; jinak rovnou
    /// výsledek (žádné tlačítko instalace, jen stav nebo rada).
    #[test]
    fn obrazovka_ovladace_podle_stavu() {
        let st = driver_screen(BusState::NotInstalled, false);
        assert_eq!(st.phase, gui::Phase::Ready);
        assert_eq!(st.steps.len(), 1);
        assert!(st.credit.is_some());
        assert_eq!(st.primary, "Nainstalovat");

        let st = driver_screen(BusState::Ready, true);
        assert_eq!(st.steps[0].0, "Aktualizace ovladače ViGEmBus");
        assert_eq!(st.primary, "Aktualizovat");

        let st = driver_screen(BusState::Ready, false);
        assert_eq!(st.phase, gui::Phase::Done);
        assert!(st.primary.is_empty() && st.steps.is_empty() && !st.attention);

        let st = driver_screen(
            BusState::InstalledNotRunning {
                device: None,
                in_apps: true,
            },
            false,
        );
        assert_eq!(st.phase, gui::Phase::Done);
        assert!(st.attention && st.primary.is_empty());
        assert!(st.message.contains("Aplikace"));

        // Bez záznamu v Aplikacích: žádné „odeber v Aplikacích", ale
        // tlačítko na ruční instalaci.
        let st = driver_screen(
            BusState::InstalledNotRunning {
                device: None,
                in_apps: false,
            },
            false,
        );
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

    /// Všechny chybové zprávy, které `do_install` umí ukázat v okně —
    /// z týchž funkcí jako za běhu, s cestami jako na běžném PC.
    fn sample_install_errors() -> Vec<String> {
        use updater::http::Error as H;
        use updater::ReleaseError as R;
        let dir = Path::new(r"C:\Users\Kamarad\AppData\Local\Programs\KeyPad");
        let https = [
            H::Connect("WinHttpOpen".into()),
            H::Connect("spojení na api.github.com".into()),
            H::Connect("WinHttpOpenRequest".into()),
            H::Connect("odeslání požadavku".into()),
            H::Connect("čekání na odpověď".into()),
            H::Http { status: 0 },
            H::Http { status: 403 },
            H::Http { status: 404 },
            H::Http { status: 409 },
            H::Http { status: 500 },
            H::Http { status: 502 },
            H::Read("dotaz na data".into()),
            H::Read("čtení dat".into()),
            H::Read("dorazilo 123 z 456 B".into()),
        ];
        let mut out = Vec::new();
        for error in https {
            for file in [None, Some(APP_EXE.to_string()), Some(VERSION_FILE.into())] {
                out.push(download_message(&R::Http {
                    file,
                    error: error.clone(),
                }));
            }
        }
        out.push(download_message(&R::Invalid(
            "odpověď GitHubu neobsahuje platný commit".into(),
        )));
        out.push(untouched(updater::BAD_DOWNLOAD));
        out.push(untouched(proc::CLOSE_FAILED));
        for e in [
            cant_create_dir(dir),
            cant_replace(&dir.join(APP_EXE)),
            cant_write(&dir.join(VERSION_FILE)),
        ] {
            out.push(relaunched_after_failure(&e));
            out.push(e);
        }
        out
    }

    /// Chyba instalace v okně: krátké věty bez kódů, velikostí a vnitřností
    /// WinHttp — ty jdou do logu (`download_failed`, `bad_download`). Cesta
    /// je jen tam, kde s ní uživatel jedná.
    #[test]
    fn chyby_instalace_bez_kodu() {
        let all = sample_install_errors();
        for m in &all {
            assert!(!m.chars().any(|c| c.is_ascii_digit()), "číslo v „{m}“");
            for bad in [
                "WinHttp",
                "http",
                "kód",
                "0x",
                "os error",
                " B",
                "požadavku",
                "odpověď",
                "dorazilo",
                "commit",
                "server",
            ] {
                assert!(!m.contains(bad), "{bad} v „{m}“");
            }
            // Délku hlídá změřené písmo okna (cesty v profilu bývají dlouhé).
            assert_eq!(
                gui::message_fit_96(m, INSTALL_STEPS.len(), false),
                gui::Fit::Body,
                "{m}"
            );
        }
        // Síť a limit dotazů mají každý svou radu; stará instalace zůstala.
        let net = download_message(&updater::ReleaseError::Http {
            file: None,
            error: updater::http::Error::Connect("WinHttpOpen".into()),
        });
        assert_eq!(
            net,
            "Nepodařilo se spojit s GitHubem — zkontroluj připojení a zkus to znovu.\n\
             Na disku se nic nezměnilo."
        );
        let limit = download_message(&updater::ReleaseError::Http {
            file: Some(APP_EXE.into()),
            error: updater::http::Error::Http { status: 403 },
        });
        assert!(limit.starts_with(updater::http::RATE_LIMITED), "{limit}");
        assert!(all
            .iter()
            .any(|m| m.starts_with("Stažený soubor nebyl v pořádku — zkus to znovu.")));
    }

    /// KeyPad, který aktualizace ovladače zavřela (drží sběrnici), se
    /// vrací; když se nespustí, okno to řekne a samo se nezavře.
    #[test]
    fn keypad_zavreny_kvuli_ovladaci_se_vraci() {
        let o = driver::Outcome::Updated { restart: false };
        for relaunched in [None, Some(Ok(()))] {
            let d = driver_done(&o, &relaunched);
            assert_eq!(d.message, o.message());
            assert!(!d.attention);
        }
        let d = driver_done(&o, &Some(Err("přístup odepřen (os error 5)".into())));
        assert!(d.attention);
        assert!(d
            .message
            .ends_with("\nKeyPad se znovu nespustil — spusť ho z nabídky Start."));
        assert!(!d.message.contains("os error"));
        // Nejdelší výsledek ovladače i s touhle větou se vejde do okna
        // /vigembus (jeden krok a credit).
        for o in driver::sample_outcomes() {
            let d = driver_done(&o, &Some(Err("x".into())));
            assert_eq!(
                gui::message_fit_96(&d.message, 1, true),
                gui::Fit::Body,
                "{}",
                d.message
            );
        }
    }

    #[test]
    fn nadpis_podle_planu() {
        // Bez razítka buildu — uživateli nic neřekne.
        assert_eq!(headline(&Plan::Fresh), "KeyPad je nainstalovaný");
        assert_eq!(
            headline(&Plan::Update {
                from: "0.1.0+20260924.2345".into(),
                to: "0.1.0+20260929.2241".into()
            }),
            "KeyPad je aktualizovaný"
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

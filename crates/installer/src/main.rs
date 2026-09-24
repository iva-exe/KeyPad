//! KeyPadSetup — jediný soubor, který dostane kamarád.
//!
//! Spuštění bez parametrů = okno s tlačítkem Nainstalovat:
//!   1. zjistí z repozitáře aktuální verzi
//!   2. porovná ji s nainstalovanou (stejná + soubory na místě → nic
//!      se nestahuje, jen se KeyPad spustí)
//!   3. stáhne KeyPad.exe do paměti, zavře běžící KeyPad, přepíše ho
//!   4. udělá zástupce v nabídce Start a záznam v Nastavení → Aplikace
//!
//! `KeyPadSetup.exe /uninstall` = odeber všechno.
//! `KeyPadSetup.exe /quiet`     = okno, které se spustí i zavře samo
//!                                (tudy jde aktualizace z aplikace).
//! `KeyPadSetup.exe /headless`  = bez okna, výpis do konzole (skripty).
//!
//! Všechno se děje v profilu uživatele (`%LOCALAPPDATA%\Programs\KeyPad`,
//! HKCU, jeho nabídka Start) — instalátor nikdy nežádá práva správce
//! a nikdy nevyvolá UAC. Manifest to říká výslovně (`asInvoker`).
//!
//! Podsystém je „windows", ne „console": jinak by u grafického
//! instalátoru bliklo černé okno. V headless režimu se konzole rodiče
//! připojí ručně, aby výpis měl kam jít. (Testy běží jako konzolová
//! binárka, ať cargo vidí jejich výstup.)
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod gui;
mod prereq;
mod proc;
mod shell;

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use updater::{APP_EXE, SETUP_EXE, VERSION_FILE};

/// Kroky instalace tak, jak je vidí uživatel v okně.
const INSTALL_STEPS: &[&str] = &[
    "Zjišťuji aktuální verzi",
    "Stahuji KeyPad",
    "Zavírám běžící KeyPad",
    "Zapisuji soubory",
    "Zástupce a záznam v systému",
];
const STEP_REGISTER: usize = 4;

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
}

/// Úspěšný konec — co ukázat a jestli po uživateli ještě něco chceme.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Done {
    message: String,
    /// Uživatel musí něco udělat nebo vědět (chybí WebView2, KeyPad se
    /// nepodařilo spustit, poznámka „Pozor:") — okno se samo nezavře
    /// ani v tichém režimu.
    attention: bool,
    /// Stránka ke stažení toho, co chybí.
    link: Option<gui::Link>,
}

impl Done {
    fn plain(message: impl Into<String>) -> Self {
        Done {
            message: message.into(),
            attention: false,
            link: None,
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .map(|a| a.to_ascii_lowercase())
        .collect();
    let has = |names: &[&str]| args.iter().any(|a| names.contains(&a.as_str()));

    let uninstall = has(&["/uninstall", "--uninstall", "/u"]);
    let headless = has(&["/headless", "--headless"]);
    // Tichý režim = okno, které se spustí i zavře samo. Tudy chodí
    // aktualizace z aplikace: uživatel klikl v aplikaci, takže se ho
    // nemá cenu ptát znovu — ale vidět, co se děje, chce.
    let quiet = has(&["/quiet", "--quiet", "/q", "/s", "/silent"]);

    // Běží instalátor z instalační složky (odinstalace z Nastavení →
    // Aplikace)? Pak se sám smazat nemůže — viz `schedule_self_delete`.
    let dir = updater::install_dir();
    let own_copy = dir.join(SETUP_EXE);
    let running_from_install = std::env::current_exe()
        .map(|me| proc::same_file(&me, &own_copy))
        .unwrap_or(false);

    if headless {
        // Podsystém je „windows", takže vlastní konzoli nemáme —
        // připojíme se k té, ze které nás spustili.
        attach_console();
        println!(
            "  KeyPad — {}",
            if uninstall {
                "odinstalace"
            } else {
                "instalace"
            }
        );
        let mut rep = ConsoleReport;
        let r = if uninstall {
            do_uninstall(&mut rep)
        } else {
            do_install(&mut rep)
        };
        match r {
            Ok(done) => {
                println!("\n  {}", done.message.replace('\n', "\n  "));
                if uninstall && running_from_install {
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

    let state: gui::Shared = Arc::new(Mutex::new(if uninstall {
        // Poctivý výčet toho, co zmizí — i logy, o které README žádá
        // při hlášení chyby, a data okna. Patička má místo na dva řádky,
        // proto je výčet v podtitulku a patička říká, co zůstane.
        gui::State::new(
            "Odebrat KeyPad",
            "smaže aplikaci, zástupce, záznam v Aplikacích, logy a data okna (WebView2)",
            "Tvoje nastavení (config.toml) zůstane.",
            UNINSTALL_STEPS,
            "Odebrat",
        )
    } else {
        gui::State::new(
            "KeyPad",
            "klávesnice jako Xbox ovladač",
            "Nainstaluje se do tvého profilu — bez práv správce.",
            INSTALL_STEPS,
            "Nainstalovat",
        )
    }));

    // Povedla se odinstalace? Až pak se smí po zavření okna smazat
    // i kopie instalátoru — po nepovedené by nebylo čím to zkusit znovu.
    let succeeded = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&succeeded);
    let action: gui::Action = Arc::new(move |st: gui::Shared, note: gui::Notifier| {
        let mut rep = GuiReport {
            state: Arc::clone(&st),
            note,
        };
        let r = if uninstall {
            do_uninstall(&mut rep)
        } else {
            do_install(&mut rep)
        };
        if r.is_ok() {
            flag.store(true, Ordering::SeqCst);
        }
        if let Ok(mut s) = st.lock() {
            match r {
                Ok(done) => s.finish(&done.message, done.attention, done.link),
                Err(e) => s.fail(&e),
            }
        }
        note.tick();
    });

    let title = if uninstall {
        "KeyPad — odinstalace"
    } else {
        "KeyPad — instalace"
    };
    gui::run(title, state, action, quiet, quiet);

    if uninstall && running_from_install && succeeded.load(Ordering::SeqCst) {
        schedule_self_delete(&own_copy, &dir);
    }
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

/// Závěrečná zpráva podle stavu systému.
///
/// `launched` je `None`, když se aplikace vůbec nespouštěla (chybí
/// WebView2), jinak výsledek spuštění.
fn outcome(
    headline: &str,
    webview2: bool,
    launched: Option<Result<(), String>>,
    vigembus: bool,
    notes: &[String],
) -> Done {
    let vigem_note = format!(
        "Chybí ještě ovladač ViGEmBus — bez něj KeyPad nevytvoří gamepad. Stáhneš ho z {} \
         (ovladač chce práva správce, KeyPad sám ne).",
        prereq::VIGEMBUS_URL
    );
    let mut done = if !webview2 {
        // Aplikace by se spustila a hned skončila — uživatel by neviděl
        // nic a nevěděl proč. Proto se nespouští a zpráva říká, co dál.
        let mut message = format!(
            "{headline}, ale zatím ho nespouštím — chybí Microsoft Edge WebView2 Runtime, \
             bez kterého se okno KeyPadu neotevře.\nNainstaluj ho z {} a spusť KeyPadSetup \
             znovu, víc není potřeba.",
            prereq::WEBVIEW2_URL
        );
        if !vigembus {
            message.push('\n');
            message.push_str(&vigem_note);
        }
        Done {
            message,
            attention: true,
            link: Some(gui::Link {
                label: "Stáhnout WebView2".into(),
                url: prereq::WEBVIEW2_URL.into(),
            }),
        }
    } else {
        // Nepovedené spuštění musí zůstat na očích i v tichém režimu:
        // při aktualizaci z aplikace uživatel viděl, jak se KeyPad zavřel,
        // a bez zprávy by nevěděl, proč se nevrátil (typicky ho zablokoval
        // antivirus).
        let failed_launch = matches!(launched, Some(Err(_)));
        let mut message = match launched {
            Some(Err(e)) => format!(
                "Hotovo — {headline}, jen se ho nepodařilo spustit ({e}). Spusť ho z nabídky Start."
            ),
            _ => format!("Hotovo — {headline} a běží. Najdeš ho i v nabídce Start."),
        };
        let mut link = None;
        if !vigembus {
            message.push('\n');
            message.push_str(&vigem_note);
            link = Some(gui::Link {
                label: "Stáhnout ViGEmBus".into(),
                url: prereq::VIGEMBUS_URL.into(),
            });
        }
        Done {
            message,
            attention: failed_launch,
            link,
        }
    };
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

fn do_install(rep: &mut dyn Report) -> Result<Done, String> {
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
            return Ok(launch_and_report(
                &dir,
                &headline(&decision, &version),
                notes,
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
    let closed = proc::close_app(&app, &mut |s| rep.status(s))
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

    Ok(launch_and_report(
        &dir,
        &headline(&decision, &version),
        notes,
    ))
}

/// Zkontroluje prostředí, případně spustí aplikaci, a složí zprávu.
fn launch_and_report(dir: &Path, headline: &str, notes: Vec<String>) -> Done {
    let webview2 = prereq::webview2_present();
    let launched = webview2.then(|| launch(dir));
    outcome(
        headline,
        webview2,
        launched,
        prereq::vigembus_present(),
        &notes,
    )
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
    notes.push(format!(
        "kopie instalátoru pro odinstalaci se nezapsala ({last}); odinstaluješ spuštěním \
         KeyPadSetup.exe /uninstall"
    ));
}

/// Zástupce v nabídce Start a záznam v Nastavení → Aplikace. Obojí je
/// pohodlí navíc — selhání instalaci nezastaví, jen se ohlásí.
fn register(dir: &Path, version: &str, notes: &mut Vec<String>) {
    if let Err(e) = shell::create_shortcut(&dir.join(APP_EXE), &shell::start_menu_lnk()) {
        notes.push(format!(
            "zástupce v nabídce Start se nepodařilo vytvořit: {e}"
        ));
    }
    let size_kb = [APP_EXE, SETUP_EXE]
        .iter()
        .filter_map(|n| std::fs::metadata(dir.join(n)).ok())
        .map(|m| (m.len() / 1024) as u32)
        .sum();
    if let Err(e) = shell::register_uninstall(&dir.join(SETUP_EXE), dir, version, size_kb) {
        notes.push(format!("záznam v Nastavení → Aplikace: {e}"));
    }
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
    proc::close_app(&app, &mut |s| rep.status(s))?;

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

    let mut msg = if !found {
        String::from("KeyPad tu nainstalovaný nebyl — nebylo co odebírat.")
    } else if config_left {
        format!(
            "KeyPad je odebraný.\nTvoje nastavení ({CONFIG_FILE}) zůstalo v {} — smaž ho ručně, \
             pokud ho už nechceš.",
            dir.display()
        )
    } else {
        String::from("KeyPad je odebraný.")
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
    Ok(Done::plain(msg))
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

    #[test]
    fn bez_webview2_se_nespousti_a_okno_zustane() {
        let d = outcome("KeyPad 1 je nainstalovaný", false, None, true, &[]);
        assert!(d.attention);
        assert!(d.message.contains(prereq::WEBVIEW2_URL));
        assert!(d.message.contains("KeyPadSetup znovu"));
        assert_eq!(
            d.link.map(|l| l.url),
            Some(prereq::WEBVIEW2_URL.to_string())
        );
    }

    #[test]
    fn chybejici_vigembus_neni_chyba() {
        let d = outcome("KeyPad 1 je nainstalovaný", true, Some(Ok(())), false, &[]);
        assert!(!d.attention);
        assert!(d.message.starts_with("Hotovo"));
        assert!(d.message.contains(prereq::VIGEMBUS_URL));
        assert_eq!(
            d.link.map(|l| l.url),
            Some(prereq::VIGEMBUS_URL.to_string())
        );
    }

    #[test]
    fn chybi_oboji_prednost_ma_webview2() {
        let d = outcome("KeyPad 1 je nainstalovaný", false, None, false, &[]);
        assert!(d.attention);
        assert!(d.message.contains(prereq::VIGEMBUS_URL));
        assert_eq!(
            d.link.map(|l| l.url),
            Some(prereq::WEBVIEW2_URL.to_string())
        );
    }

    #[test]
    fn vse_v_poradku_bez_odkazu() {
        let d = outcome("KeyPad 1 je aktuální", true, Some(Ok(())), true, &[]);
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
            true,
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
        let d = outcome(h, true, Some(Err("blokováno".into())), true, &[]);
        assert!(d.attention);
        assert!(d.message.contains("blokováno"));

        let d = outcome(h, true, Some(Ok(())), true, &["zástupce nevznikl".into()]);
        assert!(d.attention);

        // S chybějícím ViGEmBus a nespuštěním: pořád pozornost i odkaz.
        let d = outcome(h, true, Some(Err("x".into())), false, &[]);
        assert!(d.attention);
        assert_eq!(
            d.link.map(|l| l.url),
            Some(prereq::VIGEMBUS_URL.to_string())
        );

        assert!(!outcome(h, true, Some(Ok(())), true, &[]).attention);
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

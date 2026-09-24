//! Souborový logger: `keypad.log` vedle `.exe`, jinak v `%APPDATA%\KeyPad`.
//!
//! Zapisuje VÝHRADNĚ vlastní vlákno, které dostává hotové řádky přes
//! omezenou frontu. [`log::Log::log`] jen zkusí řádek do fronty vložit
//! (`try_send`) a když je plná, zprávu zahodí a započítá — na disk ani
//! na volné místo ve frontě nikdy nečeká.
//!
//! Proč: GUI, síť ani start nesmí zdržovat pomalý disk, antivirus nebo
//! síťová složka s logem. Ztracený řádek logu je proti tomu zanedbatelná
//! cena — a počet ztracených se do logu zapíše, jakmile se fronta
//! uvolní, takže mezera nezůstane nevysvětlená.
//!
//! „Nečeká na disk" ale NENÍ „nikdy nečeká": řádek se formátuje
//! do `String` (alokace = zámek haldy) a `try_send` krátce bere vnitřní
//! zámek kanálu, když zapisovač spí v `recv` — tentýž zámek, který berou
//! všechna ostatní logující vlákna včetně GUI. Úseky jsou krátké, ale
//! pod zátěží hry si vlákno může počkat (inverze priorit).
//!
//! Proto callback klávesnicového hooku (Fáze 3) `log::…` makra volat
//! NESMÍ (ROADMAP, princip 3 — Windows by po `LowLevelHooksTimeout`
//! hook potichu odebraly). Hook předá pevně velký záznam (atomiky nebo
//! předem alokovaný kruhový buffer) jinému vláknu a logovat bude to.

use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, TrySendError};

/// Název souboru logu.
const SOUBOR: &str = "keypad.log";
/// Předchozí log po rotaci.
const SOUBOR_STARY: &str = "keypad.old.log";
/// Nad tuhle velikost se log při startu odsune do `keypad.old.log`.
/// Na diagnostiku stačí poslední běhy; log nesmí na disku růst donekonečna.
const MAX_VELIKOST: u64 = 1024 * 1024;
/// Kapacita fronty. Běžný provoz logne pár řádků za minutu; tisícovka
/// pojme i dávku z ladění hooku, a přitom je to jen pár set kB paměti.
const KAPACITA: usize = 1024;

/// Zpráva pro zapisovací vlákno.
enum Zprava {
    Radek {
        cas: SystemTime,
        text: String,
    },
    /// Vyprázdni buffer na disk a potvrď to.
    Flush(Sender<()>),
}

struct Logger {
    tx: Sender<Zprava>,
    /// Zahozené zprávy od posledního hlášení (plná fronta).
    zahozeno: AtomicU64,
    /// Úroveň pro vlastní kód (`keypad`, `updater`, `keypad_core`).
    uroven: log::LevelFilter,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

thread_local! {
    /// Vlákno, které nesmí nikdy čekat (budoucí hook). Nastavuje ho
    /// [`mark_realtime_thread`].
    static REALTIME: Cell<bool> = const { Cell::new(false) };
    /// Zapisovací vlákno samo. `flush` z něj (panika při zápisu) by
    /// čekal sám na sebe — až do vypršení limitu.
    static ZAPISOVAC: Cell<bool> = const { Cell::new(false) };
}

/// Označí aktuální vlákno jako „nikdy neblokovat".
///
/// [`flush`] se v něm vrátí okamžitě a panic hook na disk nečeká.
/// Volat na začátku vlákna hooku (Fáze 3). Samotné logování tím
/// bezpečné nezačne — callback hooku `log::…` nevolá vůbec (viz
/// dokumentace modulu); řádek z panic hooku je výjimka, panika už
/// alokuje sama.
#[expect(dead_code, reason = "použije hook vlákno ve Fázi 3")]
pub fn mark_realtime_thread() {
    REALTIME.with(|r| r.set(true));
}

/// Je aktuální vlákno označené jako realtime?
pub fn is_realtime_thread() -> bool {
    REALTIME.with(|r| r.get())
}

/// Nainstaluje logger. Volat jednou, co nejdřív v `main`.
///
/// Vrací cestu k logu, nebo `None`, když se soubor nepodařilo otevřít
/// ani vedle programu, ani v `%APPDATA%` — aplikace pak běží bez logu
/// (nepadá kvůli tomu).
pub fn init() -> Option<PathBuf> {
    let (soubor, cesta) = match otevri_log() {
        Some(v) => v,
        None => {
            log::set_max_level(log::LevelFilter::Off);
            return None;
        }
    };

    let (tx, rx) = crossbeam_channel::bounded(KAPACITA);
    let logger = LOGGER.get_or_init(|| Logger {
        tx,
        zahozeno: AtomicU64::new(0),
        uroven: vlastni_uroven(),
    });

    let spusteno = std::thread::Builder::new()
        .name("keypad-log".into())
        .spawn(move || zapisovac(rx, soubor));
    if spusteno.is_err() {
        // Bez vlákna by se fronta jen plnila a pak zahazovala; lepší
        // logování rovnou vypnout.
        log::set_max_level(log::LevelFilter::Off);
        return None;
    }

    if log::set_logger(logger).is_err() {
        // Jiný logger už je nainstalovaný — dvakrát `init` je chyba
        // programátora, ne uživatele; aplikace pokračuje s tím prvním.
        return Some(cesta);
    }
    log::set_max_level(logger.uroven.max(CIZI_UROVEN));
    Some(cesta)
}

/// Úroveň pro cizí crates (Tauri, wry, tao…). Jejich info/debug řádky
/// by log zaplavily šumem, ve kterém by se vlastní zprávy ztratily.
const CIZI_UROVEN: log::LevelFilter = log::LevelFilter::Warn;

/// Úroveň pro vlastní kód: `KEYPAD_LOG=debug|trace|…` ji přepíše
/// (ladění u kamaráda bez nového buildu), jinak debug ve vývoji
/// a info ve vydání.
fn vlastni_uroven() -> log::LevelFilter {
    std::env::var("KEYPAD_LOG")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(if cfg!(debug_assertions) {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Info
        })
}

/// Je zpráva z našeho kódu? Podle cíle (`target`), který je u `log`
/// ve výchozím stavu cesta modulu. Bez ohledu na velikost písmen:
/// crate aplikace se jmenuje `KeyPad` (podle `KeyPad.exe`), cíle jsou
/// tedy `KeyPad::update` a podobně.
fn je_vlastni(target: &str) -> bool {
    let koren = target.split("::").next().unwrap_or(target);
    ["keypad", "updater", "keypad_core"]
        .iter()
        .any(|k| koren.eq_ignore_ascii_case(k))
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        let limit = if je_vlastni(metadata.target()) {
            self.uroven
        } else {
            CIZI_UROVEN
        };
        metadata.level() <= limit
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Formátuje se tady (argumenty zprávy nejdou poslat do jiného
        // vlákna), ale jen do paměti. Čas se bere hned, ať řádek nese
        // okamžik události, ne okamžik zápisu.
        let text = format!(
            "{:<5} {}: {}",
            record.level(),
            record.target(),
            record.args()
        );
        let zprava = Zprava::Radek {
            cas: SystemTime::now(),
            text,
        };
        match self.tx.try_send(zprava) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.zahozeno.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn flush(&self) {
        let _ = flush(Duration::from_millis(500));
    }
}

/// Počká, až je všechno z fronty na disku. Vrací `true`, když se to
/// stihlo do `limit`.
///
/// Z realtime vlákna (a ze zapisovače samotného) se vrací hned
/// s `false` — čekat tam nesmíme.
pub fn flush(limit: Duration) -> bool {
    if is_realtime_thread() || ZAPISOVAC.with(|z| z.get()) {
        return false;
    }
    let Some(logger) = LOGGER.get() else {
        return false;
    };
    let konec = Instant::now() + limit;
    let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
    if logger
        .tx
        .send_timeout(Zprava::Flush(ack_tx), limit)
        .is_err()
    {
        return false;
    }
    let zbyva = konec.saturating_duration_since(Instant::now());
    ack_rx.recv_timeout(zbyva).is_ok()
}

/// Najde, kam logovat, a soubor otevře.
///
/// Zapisovatelnost složky se ZKOUŠÍ otevřením souboru, ne čtením
/// atributů: „jen pro čtení" u složky Windows nevynucují a skutečná
/// práva (ACL, Program Files, síťový disk, antivirus) prozradí až pokus.
fn otevri_log() -> Option<(File, PathBuf)> {
    let vedle_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    // Stejné místo, kde ho hledá odinstalace (jeden zdroj pravdy v updateru).
    let appdata = updater::roaming_dir();

    for slozka in [vedle_exe, appdata].into_iter().flatten() {
        if std::fs::create_dir_all(&slozka).is_err() {
            continue;
        }
        let cesta = slozka.join(SOUBOR);
        rotuj(&cesta, &slozka.join(SOUBOR_STARY));
        if let Ok(f) = otevri_pro_pripis(&cesta) {
            return Some((f, cesta));
        }
    }
    None
}

/// Otevře log pro připisování.
///
/// Sdílení BEZ `FILE_SHARE_DELETE`: druhé spuštění (single-instance ho
/// hned ukončí, ale logger startuje dřív) by jinak při rotaci přejmenovalo
/// soubor, do kterého běžící instance zrovna píše — a ta by dál psala
/// do `keypad.old.log`. Takhle rotace u otevřeného logu prostě selže.
fn otevri_pro_pripis(cesta: &Path) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.create(true).append(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ | FILE_SHARE_WRITE — číst log za běhu jde.
        o.share_mode(0x1 | 0x2);
    }
    o.open(cesta)
}

/// Odsune příliš velký log stranou. Chyby se ignorují: rotace je
/// úklid, ne podmínka startu.
///
/// Jen `rename`, žádné mazání `keypad.old.log` předem: `rename` ve
/// Windows cíl nahradí najednou, nebo selže a nechá ho být. Selhává
/// běžně — druhé spuštění (single-instance ho ukončí až po startu
/// loggeru) narazí na log, do kterého běžící instance píše. Mazání
/// předem by v tu chvíli zahodilo předchozí log bez náhrady.
fn rotuj(cesta: &Path, stary: &Path) {
    let velky = std::fs::metadata(cesta).is_ok_and(|m| m.len() > MAX_VELIKOST);
    if velky {
        let _ = std::fs::rename(cesta, stary);
    }
}

/// Smyčka zapisovacího vlákna.
fn zapisovac(rx: Receiver<Zprava>, soubor: File) {
    ZAPISOVAC.with(|z| z.set(true));
    let mut out = BufWriter::new(soubor);
    // Oddělovač běhů — v logu s několika starty za sebou je hned vidět,
    // kde jeden skončil a další začal.
    let _ = writeln!(out);
    while let Ok(zprava) = rx.recv() {
        // Hlášení o zahozených jde PŘED další řádek, ať je mezera
        // v logu vysvětlená na místě, kde vznikla.
        if let Some(logger) = LOGGER.get() {
            let n = logger.zahozeno.swap(0, Ordering::Relaxed);
            if n > 0 {
                let _ = writeln!(
                    out,
                    "{} WARN  keypad::logger: zahozeno {n} zpráv (plná fronta logu)",
                    formatuj_cas(SystemTime::now())
                );
            }
        }
        match zprava {
            Zprava::Radek { cas, text } => {
                let _ = writeln!(out, "{} {text}", formatuj_cas(cas));
            }
            Zprava::Flush(ack) => {
                let _ = out.flush();
                let _ = ack.send(());
            }
        }
        // Buffer na disk, jakmile je fronta prázdná: při dávce se píše
        // najednou, v klidu je každý řádek v souboru hned — pád procesu
        // pak nesebere poslední zprávy před ním.
        if rx.is_empty() {
            let _ = out.flush();
        }
    }
    let _ = out.flush();
}

/// Čas jako `RRRR-MM-DD HH:MM:SS.mmm` v místním čase. Když převod na
/// místní čas selže, vrátí UTC s příponou `Z`, ať je to v logu poznat.
fn formatuj_cas(cas: SystemTime) -> String {
    let ms = cas
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let utc = Rozklad::z_unix_ms(ms);
    match mistni(&utc) {
        Some(m) => m.text(""),
        None => utc.text("Z"),
    }
}

/// Datum a čas rozložené na složky.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rozklad {
    rok: i64,
    mesic: u32,
    den: u32,
    hodina: u32,
    minuta: u32,
    sekunda: u32,
    milis: u32,
}

impl Rozklad {
    /// Rozloží milisekundy od UNIX epochy na UTC datum a čas.
    ///
    /// Vlastní převod místo knihovny (chrono, time): kvůli jednomu
    /// řádku v logu se nevyplatí tahat závislost. Algoritmus je
    /// „civil_from_days" Howarda Hinnanta — platí pro celý rozsah i64.
    fn z_unix_ms(ms: i64) -> Self {
        let sekundy = ms.div_euclid(1000);
        let milis = ms.rem_euclid(1000) as u32;
        let dny = sekundy.div_euclid(86_400);
        let v_dni = sekundy.rem_euclid(86_400);

        let z = dny + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let den = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let mesic = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let rok = yoe + era * 400 + i64::from(mesic <= 2);

        Rozklad {
            rok,
            mesic,
            den,
            hodina: (v_dni / 3600) as u32,
            minuta: (v_dni % 3600 / 60) as u32,
            sekunda: (v_dni % 60) as u32,
            milis,
        }
    }

    fn text(&self, pripona: &str) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}{pripona}",
            self.rok, self.mesic, self.den, self.hodina, self.minuta, self.sekunda, self.milis
        )
    }
}

/// Převede UTC na místní čas podle časového pásma Windows — včetně
/// letního času platného pro TO datum (ne pro dnešek).
#[cfg(windows)]
fn mistni(utc: &Rozklad) -> Option<Rozklad> {
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::System::Time::SystemTimeToTzSpecificLocalTime;

    let vstup = SYSTEMTIME {
        wYear: u16::try_from(utc.rok).ok()?,
        wMonth: utc.mesic as u16,
        wDayOfWeek: 0,
        wDay: utc.den as u16,
        wHour: utc.hodina as u16,
        wMinute: utc.minuta as u16,
        wSecond: utc.sekunda as u16,
        wMilliseconds: utc.milis as u16,
    };
    let mut vystup = SYSTEMTIME::default();
    // SAFETY: obě struktury žijí po celou dobu volání; `None` = aktuální
    // časové pásmo systému.
    unsafe { SystemTimeToTzSpecificLocalTime(None, &vstup, &mut vystup) }.ok()?;
    Some(Rozklad {
        rok: i64::from(vystup.wYear),
        mesic: u32::from(vystup.wMonth),
        den: u32::from(vystup.wDay),
        hodina: u32::from(vystup.wHour),
        minuta: u32::from(vystup.wMinute),
        sekunda: u32::from(vystup.wSecond),
        milis: u32::from(vystup.wMilliseconds),
    })
}

#[cfg(not(windows))]
fn mistni(_utc: &Rozklad) -> Option<Rozklad> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epocha_je_1970() {
        assert_eq!(Rozklad::z_unix_ms(0).text("Z"), "1970-01-01 00:00:00.000Z");
    }

    #[test]
    fn znamy_okamzik() {
        // 2024-02-29 13:45:07.089 UTC — přestupný den.
        assert_eq!(
            Rozklad::z_unix_ms(1_709_214_307_089).text("Z"),
            "2024-02-29 13:45:07.089Z"
        );
        // Silvestr a Nový rok (přechod roku).
        assert_eq!(
            Rozklad::z_unix_ms(1_735_689_599_999).text("Z"),
            "2024-12-31 23:59:59.999Z"
        );
        assert_eq!(
            Rozklad::z_unix_ms(1_735_689_600_000).text("Z"),
            "2025-01-01 00:00:00.000Z"
        );
    }

    #[test]
    fn pred_epochou() {
        assert_eq!(Rozklad::z_unix_ms(-1).text("Z"), "1969-12-31 23:59:59.999Z");
    }

    #[test]
    fn vlastni_cile() {
        assert!(je_vlastni("keypad"));
        assert!(je_vlastni("KeyPad"));
        assert!(je_vlastni("KeyPad::update"));
        assert!(je_vlastni("updater::http"));
        assert!(je_vlastni("keypad_core::engine"));
        assert!(!je_vlastni("keypadx"));
        assert!(!je_vlastni("tauri::manager"));
        assert!(!je_vlastni("wry"));
    }

    #[test]
    fn rotace_odsune_velky_log() {
        let dir = std::env::temp_dir().join(format!("keypad-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join(SOUBOR);
        let stary = dir.join(SOUBOR_STARY);

        std::fs::write(&log, vec![b'x'; 10]).unwrap();
        rotuj(&log, &stary);
        assert!(log.exists() && !stary.exists(), "malý log zůstává");

        std::fs::write(&log, vec![b'x'; MAX_VELIKOST as usize + 1]).unwrap();
        std::fs::write(&stary, b"predchozi").unwrap();
        rotuj(&log, &stary);
        assert!(!log.exists(), "velký log se odsunul");
        assert_eq!(
            std::fs::metadata(&stary).unwrap().len(),
            MAX_VELIKOST + 1,
            "starý log se přepsal novějším"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Druhé spuštění, zatímco běžící instance píše do velkého logu:
    /// rotace selže a předchozí `keypad.old.log` musí zůstat celý.
    #[test]
    fn rotace_otevreneho_logu_nesmaze_stary() {
        let dir = std::env::temp_dir().join(format!("keypad-log-test2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join(SOUBOR);
        let stary = dir.join(SOUBOR_STARY);

        std::fs::write(&log, vec![b'x'; MAX_VELIKOST as usize + 1]).unwrap();
        std::fs::write(&stary, b"predchozi").unwrap();
        // Tak, jak ho drží běžící instance (bez FILE_SHARE_DELETE).
        let bezici = otevri_pro_pripis(&log).unwrap();
        rotuj(&log, &stary);
        assert_eq!(std::fs::read(&stary).unwrap(), b"predchozi");
        assert_eq!(
            std::fs::metadata(&log).unwrap().len(),
            MAX_VELIKOST + 1,
            "log běžící instance zůstal na místě"
        );
        drop(bezici);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

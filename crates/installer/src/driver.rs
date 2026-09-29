//! Instalace ovladače ViGEmBus z KeyPadSetupu.
//!
//! Jediné místo, kde se kvůli KeyPadu něco spouští s právy správce —
//! a ani tady to není KeyPad: instalátor požádá Windows, aby s právy
//! správce spustily oficiální podepsaný instalátor Nefarius, ověřený
//! bajt po bajtu (pin v `updater::vigembus`). Výzvu UAC ukazují Windows
//! i se jménem ověřeného vydavatele. Vlastník si to výslovně přál, aby
//! kamarád nemusel nic stahovat zvlášť; principy 6 a 8 to připouštějí
//! jen takhle: po kliknutí, vysvětlené předem, ověřené potom.
//!
//! Postup:
//! 1. **Pojistka.** ViGEmBus v systému úplně chybí (jinak by MSI při
//!    „první instalaci" odebralo cizí zařízení sběrnice nebo při upgradu
//!    vynutilo restart) a složka, kam zavaděč Advanced Installer rozbaluje
//!    MSI, neexistuje ani nepatří běžnému uživateli (klasický vektor
//!    zvýšení práv — složku v ProgramData smí založit kdokoli předem).
//!    Stav se tu VŽDY čte skutečný — ladicí předstírání stavu v `main.rs`
//!    sem nedosáhne.
//! 2. **Stažení** do paměti, přesná velikost + SHA-256.
//! 3. **Zápis a zámek.** Čerstvá náhodná složka v `%TEMP%`, soubor přes
//!    CREATE_NEW, pak znovu otevřený jen pro čtení se zákazem zápisu
//!    a mazání a druhý SHA-256 z téhož handlu. Soubor se až do konce
//!    instalace nedá přepsat, přejmenovat ani smazat (ani složka, dokud
//!    je v ní otevřený soubor).
//! 4. **Podpis** Authenticode přes týž handle (`WinVerifyTrust`, bez UI,
//!    bez kontroly odvolání — hlavní kontrolou je otisk a síťové dotazy
//!    by uměly viset) a jméno vydavatele.
//! 5. **Pojistka znovu**, těsně před spuštěním: stažení a zápis trvají
//!    sekundy až minuty a složku rozbalování mezitím mohl kdokoli
//!    založit (stejně jako mohl ovladač nainstalovat jiný program).
//! 6. **Spuštění** `ShellExecuteExW("runas")` plnou cestou, s oknem
//!    instalátoru jako vlastníkem — jediná výzva UAC.
//! 7. **Čekání** na pracovním vlákně (okno se dál překresluje).
//! 8. **Ověření.** Kód návratu nic nedokazuje — 0 z MSI ani 3010 (vlastní
//!    akce, které ovladač instalují, mají chyby ignorovat). Po KAŽDÉM
//!    kódu se stav ovladače čte znovu (u 0 a 3010 se na rozhraní sběrnice
//!    čeká až 15 s) a výsledek říká jen to, co se ověřilo.
//! 9. **Úklid.** Nikdy se nic nezkouší znovu samo, nikdy se ViGEmBus
//!    neaktualizuje, neopravuje ani neodinstaluje.
//!
//! **Zbytkové riziko, vědomě přijaté** (zámek chrání jen soubor, ne jeho
//! složku): čerstvá složka `%TEMP%\keypad-vigembus-…` má zděděná
//! oprávnění uživatele, takže do ní proces běžící pod týmž uživatelem
//! může přidávat soubory až do konce instalace. Zavaděč Advanced
//! Installer z ní běží s právy správce (je to jeho složka aplikace
//! i pracovní složka) a jeho PE nemá DependentLoadFlags, staticky
//! importuje jen KERNEL32 a 19 DLL načítá zpožděně — i takové, které
//! nejsou v KnownDLLs (VERSION, UxTheme, dwmapi, MSIMG32, dbghelp,
//! Cabinet, msi, WININET, MPR, NETAPI32). Jestli si hledání DLL omezí
//! sám dřív, než je poprvé načte, jsme neověřili — bezpečnost tu stojí
//! na zabezpečení zavaděče od výrobce. Okno trvá od výzvy UAC (čeká se
//! na uživatele) do konce instalace; totéž okno má i pojistka složky
//! rozbalování v ProgramData (kontroluje se naposled těsně před výzvou).
//! Útočník ale musí už běžet pod týmž uživatelem — stažený soubor do
//! náhodné podsložky %TEMP% nedoputuje (na rozdíl od Stažených souborů,
//! proti kterým chrání `SetDefaultDllDirectories` v `main`). A takový
//! proces umí zapisovat i do paměti samotného KeyPadSetupu (stejný
//! uživatel, stejná úroveň integrity) a změnit parametry spuštění před
//! výzvou — vlastní ACL na složce by tuhle třídu útoků neuzavřelo, jen
//! by přidalo kód, který musí po sobě bezchybně uklízet. Proto se neřeší.

use std::io::{Seek, SeekFrom, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use updater::vigembus::{self, BusState, DownloadError};
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_CANCELLED, HANDLE, HLOCAL, HWND, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows::Win32::Security::Cryptography::{CertGetNameStringW, CERT_NAME_SIMPLE_DISPLAY_TYPE};
use windows::Win32::Security::WinTrust::{
    WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData, WinVerifyTrust,
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
    WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
    WTD_UICONTEXT_EXECUTE, WTD_UI_NONE,
};
use windows::Win32::Security::{OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows::Win32::UI::Shell::{
    FOLDERID_ProgramData, SHGetKnownFolderPath, ShellExecuteExW, KF_FLAG_DEFAULT, SEE_MASK_NOASYNC,
    SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::Report;

/// Konstanty z `Win32_Storage_FileSystem` — kvůli dvěma číslům se ta
/// obří feature netahá (stejně jako WM_MOUSELEAVE v gui.rs).
const FILE_SHARE_READ: u32 = 0x1;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Předpona dočasných složek v `%TEMP%` — podle ní se uklízí i to, co
/// zbylo po přerušeném běhu.
const TEMP_PREFIX: &str = "keypad-vigembus-";
/// Jak dlouho se čeká na instalátor ViGEmBus. Běžně trvá kolem 15 s;
/// déle visí jen, když ho něco drží (dotaz jiné instalace, antivir).
/// Pak se přestane čekat, nechá se doběhnout a uživatel se to dozví.
const SETUP_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Jak dlouho po úspěšném konci instalátoru čekat, než se objeví
/// rozhraní sběrnice (ovladač se rozbíhá chvíli po skončení MSI).
const READY_TIMEOUT: Duration = Duration::from_secs(15);
const READY_POLL: Duration = Duration::from_millis(500);

/// Vlastníci složky výrobce v ProgramData, kterým věříme: Administrators,
/// SYSTEM, TrustedInstaller. Složku, kterou založil běžný uživatel, ten
/// uživatel ovládá — i to, co do ní později rozbalí proces s právy
/// správce (dědí její oprávnění).
const TRUSTED_OWNERS: [&str; 3] = [
    "S-1-5-32-544",
    "S-1-5-18",
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464",
];

/// Jméno mutexu „instalace ovladače běží" (v relaci uživatele).
const SINGLE_RUN_MUTEX: &str = "Local\\KeyPad.InstalaceViGEmBus";

/// Jak instalace ovladače dopadla.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nainstalováno a rozhraní sběrnice je vidět.
    Installed,
    /// Ovladač už běžel — nebylo co instalovat.
    AlreadyReady,
    /// Ovladač v systému je a poběží po restartu: instalátor skončil
    /// kódem 3010/1641, nebo to říká stav zařízení.
    RebootRequired,
    /// Uživatel nepotvrdil výzvu UAC nebo instalaci zrušil.
    Cancelled,
    /// Windows zrovna instalují něco jiného (1618).
    Busy,
    /// Ovladač v systému je, ale neběží. `after_setup` = zjištěno až po
    /// doběhnutí instalátoru (jinak se instalátor vůbec nespouštěl).
    NotRunning {
        advice: vigembus::Advice,
        after_setup: bool,
    },
    /// Instalátor běží déle než [`SETUP_TIMEOUT`] — nechává se doběhnout.
    /// Ruční instalace ani „zkusit znovu" tu nepomůžou (běží pořád
    /// tentýž); cesta je restart a nové spuštění KeyPadSetupu.
    TimedOut,
    /// Nepovedlo se. `retry` = má smysl zkusit znovu (síť, disk);
    /// jinak vede cesta přes ruční instalaci.
    Failed { reason: String, retry: bool },
}

impl Outcome {
    /// Ovladač běží.
    pub fn is_ready(&self) -> bool {
        matches!(self, Outcome::Installed | Outcome::AlreadyReady)
    }

    /// Má smysl nabídnout „Zkusit znovu ovladač"?
    pub fn retry_makes_sense(&self) -> bool {
        matches!(
            self,
            Outcome::Cancelled | Outcome::Busy | Outcome::Failed { retry: true, .. }
        )
    }

    /// Česká věta pro okno i konzoli.
    pub fn message(&self) -> String {
        match self {
            Outcome::Installed => "Ovladač ViGEmBus je nainstalovaný a běží.".into(),
            Outcome::AlreadyReady => {
                "Ovladač ViGEmBus už v systému běží — nebylo co instalovat.".into()
            }
            Outcome::RebootRequired => {
                "Ovladač ViGEmBus je nainstalovaný; Windows ho spustí po restartu počítače.".into()
            }
            Outcome::Cancelled => {
                "Ovladač ViGEmBus se nenainstaloval — výzva Windows nebyla potvrzena.".into()
            }
            Outcome::Busy => "Ovladač ViGEmBus se nenainstaloval — Windows zrovna instalují \
                              něco jiného. Zkus to za chvíli znovu."
                .into(),
            Outcome::NotRunning {
                advice,
                after_setup: true,
            } => format!("Instalátor skončil, ale ovladač neběží. {}", advice.text()),
            Outcome::NotRunning { advice, .. } => advice.text(),
            Outcome::TimedOut => format!(
                "Instalátor ViGEmBus běží přes {} minut — nechám ho doběhnout. Pak restartuj \
                 počítač a spusť KeyPadSetup znovu.",
                SETUP_TIMEOUT.as_secs() / 60
            ),
            Outcome::Failed {
                reason,
                retry: true,
            } => format!("Ovladač ViGEmBus se nepodařilo nainstalovat: {reason}."),
            Outcome::Failed { reason, .. } => format!(
                "Ovladač ViGEmBus se nepodařilo nainstalovat: {reason}.\nRuční instalace: {}",
                vigembus::RELEASES_URL
            ),
        }
    }

    /// Kód návratu režimu `/vigembus` — podle něj aplikace pozná, jestli
    /// má zkusit gamepad připojit znovu: 0 = ovladač běží, 3010 = je
    /// v systému a poběží po restartu (stejný kód jako u MSI), 1 = neběží.
    /// Protože se výsledek skládá z ověřeného stavu (ne z kódu
    /// instalátoru), je 0 jen tam, kde rozhraní sběrnice opravdu je.
    pub fn exit_code(&self) -> i32 {
        match self {
            o if o.is_ready() => 0,
            Outcome::RebootRequired => 3010,
            _ => 1,
        }
    }
}

/// Stav ovladače → výsledek, když se instalovat nebude; `None` =
/// ovladač úplně chybí a instalovat se smí (pojistka z kroku 1 a 5).
pub fn precheck(state: BusState) -> Option<Outcome> {
    match state {
        BusState::NotInstalled => None,
        BusState::Ready => Some(Outcome::AlreadyReady),
        s @ BusState::InstalledNotRunning { .. } => Some(not_running(
            s.advice().unwrap_or(vigembus::Advice::Restart),
            false,
            false,
        )),
    }
}

/// Ovladač je, ale neběží. Když pomůže restart (říká to instalátor
/// kódem 3010/1641, nebo stav zařízení), je to [`Outcome::RebootRequired`]
/// — aplikace podle kódu 3010 pozná, že nemá co zkoušet hned.
fn not_running(advice: vigembus::Advice, after_setup: bool, setup_said_reboot: bool) -> Outcome {
    if setup_said_reboot || advice == vigembus::Advice::Restart {
        Outcome::RebootRequired
    } else {
        Outcome::NotRunning {
            advice,
            after_setup,
        }
    }
}

/// Čisté rozhodnutí po doběhnutí instalátoru: kód návratu `code`
/// a stav ovladače `now`, přečtený ZNOVU až po něm.
///
/// Rozhoduje stav, ne kód (princip 8 — hlásit jen ověřené): kód 0 bez
/// ovladače (vlastní akce MSI chyby ignorují) je selhání a 3010 u už
/// běžícího ovladače je úspěch. Kód rozhoduje jen tam, kde ovladač
/// v systému není: zrušená výzva a zaneprázdněný instalátor Windows
/// mají vlastní radu (zkusit znovu), ostatní je selhání s ruční cestou.
fn settle(code: u32, now: BusState) -> Outcome {
    let exit = map_exit(code);
    match now {
        BusState::Ready => Outcome::Installed,
        s @ BusState::InstalledNotRunning { .. } => not_running(
            s.advice().unwrap_or(vigembus::Advice::Restart),
            true,
            exit == Exit::Reboot,
        ),
        BusState::NotInstalled => match exit {
            Exit::Cancelled => Outcome::Cancelled,
            Exit::Busy => Outcome::Busy,
            Exit::Ok | Exit::Reboot | Exit::Other(_) => Outcome::Failed {
                reason: format!(
                    "instalátor ViGEmBus skončil kódem {code}, ale ovladač v systému není"
                ),
                retry: false,
            },
        },
    }
}

/// Nainstaluje ViGEmBus (celý postup z hlavičky modulu). `step` = index
/// kroku „Ovladač ViGEmBus" v okně.
pub fn install(rep: &mut dyn Report, step: usize) -> Outcome {
    rep.step(step, "kontroluji, že ovladač v systému opravdu chybí…");
    rep.progress(None);
    // Jen jedna instalace ovladače naráz (dvakrát kliknuté tlačítko
    // v aplikaci, dvě okna). Druhá by jinak prošla pojistkou dřív, než
    // první něco zapsala, a skončila by až u Windows (1618) nebo
    // u složky rozbalování, kterou mezitím založila ta první.
    let Some(_single) = SingleRun::acquire(SINGLE_RUN_MUTEX) else {
        return Outcome::Failed {
            reason: "ovladač už instaluje jiné okno KeyPadSetupu — počkej, až doběhne".into(),
            retry: true,
        };
    };
    remove_leftovers();

    // ── 1. Pojistka ── (už tady, ať se zbytečně nestahuje)
    if let Some(o) = precheck(vigembus::state()) {
        return o;
    }
    if let Err(reason) = check_extraction_dir() {
        return extraction_refused(reason);
    }

    // ── 2. Stažení ──
    rep.status(&format!(
        "stahuji oficiální instalátor ViGEmBus {} z GitHubu…",
        vigembus::VERSION
    ));
    let mut last = 0usize;
    let data = match vigembus::download_verified(|n| {
        if n >= last + 256 * 1024 {
            last = n;
            rep.download("ViGEmBus", n);
        }
    }) {
        Ok(d) => d,
        Err(e @ DownloadError::Network(_)) => {
            return Outcome::Failed {
                reason: e.to_string(),
                retry: true,
            }
        }
        Err(e) => {
            return Outcome::Failed {
                reason: e.to_string(),
                retry: false,
            }
        }
    };

    // ── 3. Zápis a zámek ──
    rep.status("ukládám a zamykám instalátor, ověřuji jeho otisk…");
    let staged = match Staged::create(&data) {
        Ok(s) => s,
        Err(e) => return e,
    };

    // ── 4. Podpis ──
    rep.status("ověřuji podpis vydavatele…");
    if let Err(reason) = staged.verify_signature() {
        return Outcome::Failed {
            reason,
            retry: false,
        };
    }

    // ── 5. Pojistka znovu ──
    // Mezi první kontrolou a tímhle místem je stažení (sekundy až
    // minuty) — dost času na to, aby složku rozbalování založil běžný
    // uživatel nebo ovladač nainstaloval jiný program. Zbývající okno
    // (výzva UAC čeká na uživatele) viz „Zbytkové riziko" v hlavičce.
    rep.status("naposledy kontroluji systém před spuštěním…");
    if let Some(o) = precheck(vigembus::state()) {
        return o;
    }
    if let Err(reason) = check_extraction_dir() {
        return extraction_refused(reason);
    }

    // ── 6. Spuštění ──
    rep.status("čekám na povolení správce — potvrď výzvu Windows…");
    let child = match run_elevated(rep.owner(), &staged.path, &staged.dir) {
        Ok(c) => c,
        Err(Run::Cancelled) => return Outcome::Cancelled,
        Err(Run::Failed(reason)) => {
            return Outcome::Failed {
                reason,
                retry: true,
            }
        }
    };

    // ── 7. Čekání ──
    rep.status("instaluji ovladač (trvá kolem 15 s)…");
    let Some(code) = child.wait(SETUP_TIMEOUT) else {
        // Instalátor pořád běží — nechat ho doběhnout (zabít proces
        // s právy správce ani nejde a přerušené MSI je horší než pomalé).
        // Zámek se pustí, soubor zůstane, dokud instalátor nedoběhne.
        return Outcome::TimedOut;
    };
    drop(child);
    drop(staged);

    // ── 8. Ověření ──
    // U „úspěšných" kódů se ovladači dá chvíle na rozběhnutí; u ostatních
    // stačí jedno čtení (MSI skončilo chybou, ovladač se nerozbíhá).
    if matches!(map_exit(code), Exit::Ok | Exit::Reboot) {
        rep.status("ověřuji, že ovladač běží…");
        wait_ready(READY_TIMEOUT);
    }
    settle(code, vigembus::state())
}

/// Složka rozbalování nevyhověla pojistce. „Zkusit znovu" má smysl (po
/// smazání složky projde) a ruční instalace NE — oficiální instalátor
/// spuštěný ručně by rozbaloval do téže složky.
fn extraction_refused(reason: String) -> Outcome {
    Outcome::Failed {
        reason,
        retry: true,
    }
}

/// Pojmenovaný mutex „instalace ovladače běží" (v relaci uživatele).
struct SingleRun(HANDLE);

impl SingleRun {
    /// `None` = mutex `name` už drží jiný proces (nebo jiný `SingleRun`
    /// v tomtéž). Jméno je parametr kvůli testům — ty nesmí sahat na
    /// skutečné [`SINGLE_RUN_MUTEX`], jinak by zablokovaly (nebo samy
    /// selhaly na) instalaci, která zrovna běží.
    fn acquire(name: &str) -> Option<SingleRun> {
        use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
        use windows::Win32::System::Threading::CreateMutexW;
        // SAFETY: jméno žije po celé volání; handle zavře Drop.
        unsafe {
            let h = CreateMutexW(None, false, &HSTRING::from(name)).ok()?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(h);
                return None;
            }
            Some(SingleRun(h))
        }
    }
}

impl Drop for SingleRun {
    fn drop(&mut self) {
        // SAFETY: handle z CreateMutexW, zavírá se právě jednou.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Kód návratu instalátoru (zavaděč vrací kód msiexec).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exit {
    Ok,
    Reboot,
    Busy,
    Cancelled,
    Other(u32),
}

fn map_exit(code: u32) -> Exit {
    match code {
        0 => Exit::Ok,
        // ERROR_SUCCESS_REBOOT_REQUIRED, ERROR_SUCCESS_REBOOT_INITIATED
        3010 | 1641 => Exit::Reboot,
        // ERROR_INSTALL_ALREADY_RUNNING
        1618 => Exit::Busy,
        // ERROR_INSTALL_USEREXIT; ERROR_CANCELLED (zavaděč sám nedostal práva)
        1602 | 1223 => Exit::Cancelled,
        c => Exit::Other(c),
    }
}

/// Čeká, až se objeví rozhraní sběrnice.
fn wait_ready(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if vigembus::interface_present() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(READY_POLL);
    }
}

// ── Pojistka: složka rozbalování ───────────────────────────────────

/// `%ProgramData%` tak, jak ho vidí systém (ne proměnná prostředí,
/// kterou si uživatel umí přepsat).
fn program_data() -> Option<PathBuf> {
    // SAFETY: vrácený řetězec alokuje shell a uvolní se CoTaskMemFree.
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_ProgramData, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

/// Vlastník souboru nebo složky jako textové SID; `None`, když ho nejde
/// přečíst.
fn owner_sid(path: &Path) -> Option<String> {
    // SAFETY: popisovač zabezpečení i textové SID alokuje systém a obojí
    // se uvolní LocalFree; SID vlastníka ukazuje dovnitř popisovače.
    unsafe {
        let mut owner = PSID::default();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let rc = GetNamedSecurityInfoW(
            &HSTRING::from(path),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            &mut sd,
        );
        if rc.is_err() {
            return None;
        }
        let mut text = PWSTR::null();
        let out = ConvertSidToStringSidW(owner, &mut text)
            .ok()
            .and_then(|_| text.to_string().ok());
        if !text.is_null() {
            let _ = LocalFree(Some(HLOCAL(text.0 as _)));
        }
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        out
    }
}

/// Smí se instalátor ViGEmBus spustit vzhledem ke složce, kam rozbaluje?
fn check_extraction_dir() -> Result<(), String> {
    let pd = program_data().ok_or("nejde zjistit složku ProgramData")?;
    let vendor = pd.join(vigembus::VENDOR_DIR);
    let version = pd.join(vigembus::EXTRACTION_DIR);
    let meta = std::fs::symlink_metadata(&vendor).ok();
    let reparse = meta
        .as_ref()
        .is_some_and(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0);
    let owner = meta.as_ref().and_then(|_| owner_sid(&vendor));
    extraction_verdict(
        &vendor,
        meta.is_some(),
        reparse,
        owner.as_deref(),
        &version,
        std::fs::symlink_metadata(&version).is_ok(),
    )
}

/// Čisté rozhodnutí k [`check_extraction_dir`].
///
/// Proč vůbec (do okna se to nevejde, tak je to tady a v README):
/// zavaděč Advanced Installer rozbaluje MSI do složky verze s právy
/// správce a z ní ho i spouští. Složku v ProgramData smí založit
/// kdokoli a kdo ji založil, ten ji vlastní — rozbalené soubory v ní
/// může vyměnit nebo k nim podstrčit DLL, a ty pak běží s právy
/// správce (klasické zvýšení práv). Složka výrobce patřící správci je
/// v pořádku (jiný produkt Nefarius); patřící uživateli nebo vedoucí
/// jinam (spojení, symbolický odkaz) ne. Zpráva proto říká jen co
/// a co s tím — a vejde se do okna i s ostatními.
fn extraction_verdict(
    vendor: &Path,
    vendor_exists: bool,
    vendor_reparse: bool,
    vendor_owner: Option<&str>,
    version: &Path,
    version_exists: bool,
) -> Result<(), String> {
    let refuse = "z bezpečnostních důvodů instalátor nespustím";
    if version_exists {
        return Err(format!(
            "složka {} už existuje — {refuse}; smaž ji a zkus to znovu",
            version.display()
        ));
    }
    if !vendor_exists {
        return Ok(());
    }
    if vendor_reparse {
        return Err(format!(
            "složka {} je odkaz jinam — {refuse}; smaž ji a zkus to znovu",
            vendor.display()
        ));
    }
    match vendor_owner {
        Some(o) if TRUSTED_OWNERS.contains(&o) => Ok(()),
        _ => Err(format!(
            "složku {} nezaložil správce — {refuse}; pokud ji neznáš, smaž ji a zkus to znovu",
            vendor.display()
        )),
    }
}

/// Všechny výsledky, které instalace ovladače umí skutečně vrátit,
/// s nejdelšími skutečnými důvody (cesty jako na běžném PC) — pro
/// ladicí náhled nejdelší zprávy a test, že se vejde do okna. Texty
/// jdou z týchž funkcí jako za běhu, takže delší hláška test shodí.
#[cfg(any(test, debug_assertions))]
pub fn sample_outcomes() -> Vec<Outcome> {
    use vigembus::DeviceStatus;
    let pd = Path::new(r"C:\ProgramData");
    let (vendor, version) = (
        pd.join(vigembus::VENDOR_DIR),
        pd.join(vigembus::EXTRACTION_DIR),
    );
    let user = Some("S-1-5-21-1111111111-2222222222-3333333333-1001");
    let mut out = vec![
        Outcome::Installed,
        Outcome::AlreadyReady,
        Outcome::RebootRequired,
        Outcome::Cancelled,
        Outcome::Busy,
        Outcome::TimedOut,
        Outcome::Failed {
            reason: vigembus::verify(b"MZ").unwrap_err(),
            retry: false,
        },
    ];
    for refused in [
        extraction_verdict(&vendor, true, false, Some("S-1-5-18"), &version, true),
        extraction_verdict(&vendor, true, true, None, &version, false),
        extraction_verdict(&vendor, true, false, user, &version, false),
    ] {
        out.extend(refused.err().map(extraction_refused));
    }
    let dev = |problem| {
        Some(DeviceStatus {
            problem,
            need_restart: false,
            started: false,
        })
    };
    for in_apps in [true, false] {
        for device in [None, dev(Some(22)), dev(Some(48)), dev(Some(10)), dev(None)] {
            let s = BusState::InstalledNotRunning { device, in_apps };
            out.extend(precheck(s));
            for code in [0, 3010, 1602, 1603] {
                out.push(settle(code, s));
            }
        }
    }
    for code in [0, 3010, 1602, 1618, 1603, u32::MAX] {
        out.push(settle(code, BusState::NotInstalled));
    }
    out
}

/// Ladicí sonda k testu podstrčených DLL (jen debug build, spouští ji
/// `KEYPAD_SETUP_TEST_KNIHOVNY` v `main` po omezení hledání DLL): projde
/// volání, při kterých systémové DLL za běhu načítají další knihovny —
/// ProgramData a jeho vlastník (profapi), zápis, zámek a podpis lokální
/// kopie oficiálního instalátoru (CRYPTSP, CRYPTBASE). Instalátor se
/// NIKDY nespouští; dočasná složka se po sobě uklidí.
#[cfg(debug_assertions)]
pub fn load_probe(setup: &Path) -> Result<String, String> {
    let pd = program_data().ok_or("ProgramData nejde zjistit")?;
    check_extraction_dir()?;
    let data = std::fs::read(setup).map_err(|e| format!("{}: {e}", setup.display()))?;
    vigembus::verify(&data)?;
    let staged = Staged::create(&data).map_err(|o| o.message())?;
    staged.verify_signature()?;
    Ok(format!(
        "ProgramData {} v pořádku, podpis „{}“ ověřen",
        pd.display(),
        vigembus::SIGNER
    ))
}

// ── Zápis, zámek, podpis ───────────────────────────────────────────

/// Instalátor na disku, zamčený proti zápisu a smazání. `Drop` zámek
/// pustí a soubor i složku smaže (co jde — běžící soubor smazat nejde).
struct Staged {
    dir: PathBuf,
    path: PathBuf,
    lock: Option<std::fs::File>,
}

impl Staged {
    fn create(data: &[u8]) -> Result<Staged, Outcome> {
        let fail = |reason: String, retry: bool| Outcome::Failed { reason, retry };
        let dir = fresh_dir(&std::env::temp_dir())
            .map_err(|e| fail(format!("nejde založit dočasnou složku ({e})"), true))?;
        let mut st = Staged {
            path: dir.join(vigembus::SETUP_FILE),
            dir,
            lock: None,
        };
        {
            // CREATE_NEW: soubor, který tu už někdo nachystal, se
            // nepřepíše — zápis selže. Během zápisu nikdo jiný nic.
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .share_mode(0)
                .open(&st.path)
                .map_err(|e| fail(format!("nejde uložit instalátor ({e})"), true))?;
            f.write_all(data)
                .and_then(|_| f.flush())
                .map_err(|e| fail(format!("nejde uložit instalátor ({e})"), true))?;
        }
        // Znovu jen pro čtení; ostatní smí jen číst (spuštění je čtení),
        // zapsat, přejmenovat ani smazat nemůže nikdo, dokud handle žije.
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&st.path)
            .map_err(|e| fail(format!("nejde zamknout instalátor ({e})"), true))?;
        let digest = updater::sha256::sha256_reader(&mut f).map_err(|e| fail(e, true))?;
        // Mezi zavřením zápisu a zamčením je okamžik, kdy soubor mohl
        // někdo vyměnit — druhý otisk z uzamčeného handlu to pozná.
        vigembus::verify_digest(&digest)
            .map_err(|e| fail(format!("soubor se na disku po uložení změnil — {e}"), false))?;
        f.seek(SeekFrom::Start(0))
            .map_err(|e| fail(format!("čtení instalátoru ({e})"), true))?;
        st.lock = Some(f);
        Ok(st)
    }

    fn verify_signature(&self) -> Result<(), String> {
        let f = self.lock.as_ref().ok_or("instalátor není zamčený")?;
        let signer = signer_of(&self.path, HANDLE(f.as_raw_handle()))?;
        signer_matches(&signer)
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        self.lock.take();
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

fn signer_matches(signer: &str) -> Result<(), String> {
    if signer == vigembus::SIGNER {
        Ok(())
    } else {
        Err(format!(
            "instalátor podepsal „{signer}“, ne „{}“",
            vigembus::SIGNER
        ))
    }
}

/// Založí novou prázdnou složku s náhodným jménem. `create_dir` selže,
/// když už existuje — nachystaná složka se tak nikdy nepoužije.
fn fresh_dir(base: &Path) -> std::io::Result<PathBuf> {
    use std::hash::{BuildHasher, Hasher};
    let mut last = std::io::Error::other("žádný pokus");
    for i in 0..8u32 {
        // RandomState má klíč z generátoru náhodných čísel systému.
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        h.write_u32(i);
        let dir = base.join(format!("{TEMP_PREFIX}{:016x}", h.finish()));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Uklidí dočasné složky po předchozích bězích (přerušená instalace,
/// instalátor, který běžel déle než limit). Maže jen náš soubor a pak
/// prázdnou složku — cokoli jiného v ní není naše a zůstane.
pub fn remove_leftovers() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir && name.starts_with(TEMP_PREFIX) {
            let _ = std::fs::remove_file(e.path().join(vigembus::SETUP_FILE));
            let _ = std::fs::remove_dir(e.path());
        }
    }
}

/// Ověří podpis Authenticode souboru (přes otevřený handle) a vrátí
/// jednoduché jméno podepisujícího certifikátu.
fn signer_of(path: &Path, file: HANDLE) -> Result<String, String> {
    let wpath = HSTRING::from(path);
    let mut info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(wpath.as_ptr()),
        hFile: file,
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        // Odvolání se nekontroluje: hlavní kontrola je otisk souboru
        // a síťové dotazy na seznamy odvolání umí viset desítky vteřin.
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut info },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwUIContext: WTD_UICONTEXT_EXECUTE,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // INVALID_HANDLE_VALUE jako okno = žádná interakce s uživatelem.
    let no_ui = HWND(-1isize as *mut _);
    // SAFETY: struktury žijí po celou dobu volání; stav z VERIFY se vždy
    // uvolní voláním s CLOSE (i po neúspěchu, jak chce dokumentace).
    unsafe {
        let rc = WinVerifyTrust(no_ui, &mut action, &mut data as *mut _ as *mut _);
        let signer = if rc == 0 {
            signer_name(data.hWVTStateData)
        } else {
            Err(format!(
                "podpis instalátoru neprošel ověřením Windows (0x{:08X})",
                rc as u32
            ))
        };
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        let _ = WinVerifyTrust(no_ui, &mut action, &mut data as *mut _ as *mut _);
        signer
    }
}

/// Jméno podepisujícího z ověřeného stavu WinVerifyTrust.
///
/// # Safety
/// `state` musí být platný stav z úspěšného `WTD_STATEACTION_VERIFY`,
/// ještě neuzavřený.
unsafe fn signer_name(state: HANDLE) -> Result<String, String> {
    let missing = || "podpis nemá čitelný certifikát vydavatele".to_string();
    // SAFETY: ukazatele pochází z WinTrust a platí, dokud se stav
    // neuzavře; každý se před použitím kontroluje na NULL.
    unsafe {
        let prov = WTHelperProvDataFromStateData(state);
        if prov.is_null() {
            return Err(missing());
        }
        let sgnr = WTHelperGetProvSignerFromChain(prov, 0, false, 0);
        if sgnr.is_null() || (*sgnr).csCertChain == 0 || (*sgnr).pasCertChain.is_null() {
            return Err(missing());
        }
        // První certifikát řetězu = ten, kterým je soubor podepsaný.
        let cert = (*(*sgnr).pasCertChain).pCert;
        if cert.is_null() {
            return Err(missing());
        }
        let mut buf = [0u16; 256];
        let n = CertGetNameStringW(cert, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, Some(&mut buf));
        // n počítá i ukončovací NUL; 1 = prázdné jméno.
        if n <= 1 {
            return Err(missing());
        }
        Ok(String::from_utf16_lossy(
            &buf[..(n as usize - 1).min(buf.len())],
        ))
    }
}

// ── Spuštění s právy správce ───────────────────────────────────────

enum Run {
    Cancelled,
    Failed(String),
}

/// Proces instalátoru; handle se zavře v `Drop`.
struct Child(HANDLE);

impl Child {
    /// Kód návratu, nebo `None`, když do `timeout` neskončil.
    fn wait(&self, timeout: Duration) -> Option<u32> {
        let ms = timeout.as_millis().min(u32::MAX as u128 - 1) as u32;
        // SAFETY: handle procesu z ShellExecuteEx je platný až do Drop.
        unsafe {
            if WaitForSingleObject(self.0, ms) != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 0u32;
            GetExitCodeProcess(self.0, &mut code).ok()?;
            Some(code)
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // SAFETY: handle zavíráme právě jednou.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Požádá Windows o spuštění `file` s právy správce — výzvu UAC ukážou
/// Windows (se jménem ověřeného vydavatele), KeyPad sám práva nezíská.
/// `owner` = okno instalátoru (nebo konzole), aby výzva nezapadla.
fn run_elevated(owner: isize, file: &Path, dir: &Path) -> Result<Child, Run> {
    let wfile = HSTRING::from(file);
    let wargs = HSTRING::from(vigembus::SETUP_ARGS);
    let wdir = HSTRING::from(dir);
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        // NOCLOSEPROCESS = chceme handle procesu (čekání, kód návratu);
        // NOASYNC = z pracovního vlákna se nesmí vrátit dřív, než je
        // spuštění hotové.
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        hwnd: HWND(owner as *mut _),
        lpVerb: w!("runas"),
        // Plná cesta k souboru v zamčené složce — nic se nehledá.
        lpFile: PCWSTR(wfile.as_ptr()),
        lpParameters: PCWSTR(wargs.as_ptr()),
        lpDirectory: PCWSTR(wdir.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: COM pro ShellExecuteEx (dokumentace to chce kvůli
    // rozšířením shellu); řetězce žijí po celé volání.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        match ShellExecuteExW(&mut sei) {
            Ok(()) if !sei.hProcess.is_invalid() => Ok(Child(sei.hProcess)),
            Ok(()) => Err(Run::Failed(
                "Windows instalátor spustily, ale nevrátily jeho proces".into(),
            )),
            Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => Err(Run::Cancelled),
            Err(e) => Err(Run::Failed(format!(
                "instalátor nejde spustit ({})",
                e.message()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kody_navratu_instalatoru() {
        assert_eq!(map_exit(0), Exit::Ok);
        assert_eq!(map_exit(3010), Exit::Reboot);
        assert_eq!(map_exit(1641), Exit::Reboot);
        assert_eq!(map_exit(1618), Exit::Busy);
        assert_eq!(map_exit(1602), Exit::Cancelled);
        assert_eq!(map_exit(1223), Exit::Cancelled);
        assert_eq!(map_exit(1603), Exit::Other(1603));
        assert_eq!(map_exit(u32::MAX), Exit::Other(u32::MAX));
    }

    #[test]
    fn zpravy_a_kody_vysledku() {
        assert_eq!(Outcome::Installed.exit_code(), 0);
        assert_eq!(Outcome::AlreadyReady.exit_code(), 0);
        assert_eq!(Outcome::RebootRequired.exit_code(), 3010);
        assert_eq!(Outcome::Cancelled.exit_code(), 1);
        assert!(Outcome::Cancelled.retry_makes_sense());
        assert!(Outcome::Busy.retry_makes_sense());
        let net = Outcome::Failed {
            reason: "síť".into(),
            retry: true,
        };
        assert!(net.retry_makes_sense());
        assert!(!net.message().contains(vigembus::RELEASES_URL));
        let bad = Outcome::Failed {
            reason: "otisk nesedí".into(),
            retry: false,
        };
        assert!(!bad.retry_makes_sense());
        assert!(bad.message().contains(vigembus::RELEASES_URL));
        let nr = Outcome::NotRunning {
            advice: vigembus::Advice::Blocked(48),
            after_setup: true,
        };
        assert!(nr
            .message()
            .starts_with("Instalátor skončil, ale ovladač neběží."));
        assert!(nr.message().contains("kód 48"));
        assert_eq!(nr.exit_code(), 1);
        // Instalátor pořád běží: restart a znovu, ne ruční instalace.
        let t = Outcome::TimedOut;
        assert_eq!(t.exit_code(), 1);
        assert!(!t.retry_makes_sense());
        assert!(t.message().contains("restartuj počítač"));
        assert!(!t.message().contains(vigembus::RELEASES_URL));
    }

    fn dev(problem: Option<u32>, need_restart: bool) -> Option<vigembus::DeviceStatus> {
        Some(vigembus::DeviceStatus {
            problem,
            need_restart,
            started: false,
        })
    }

    /// Po instalátoru rozhoduje znovu přečtený stav, ne jeho kód: 0 jen
    /// s běžícím ovladačem, 3010 jen s ovladačem, který v systému je,
    /// jinak selhání s ruční cestou (princip 8).
    #[test]
    fn po_instalatoru_rozhoduje_stav_ne_kod() {
        let none = BusState::NotInstalled;
        let there = |device, in_apps| BusState::InstalledNotRunning { device, in_apps };
        // Běží → nainstalováno, ať instalátor vrátil cokoli (i 3010).
        for code in [0, 3010, 1641, 1603, 1602] {
            assert_eq!(settle(code, BusState::Ready), Outcome::Installed);
            assert_eq!(settle(code, BusState::Ready).exit_code(), 0);
        }
        // Kód 0 nebo 3010, ale ovladač v systému není → selhání
        // s kódem a ruční instalací, žádné „nainstalováno".
        for code in [0, 3010, 1641, 1603] {
            let o = settle(code, none);
            assert_eq!(o.exit_code(), 1, "{code}");
            assert!(!o.retry_makes_sense());
            let m = o.message();
            assert!(
                m.contains(&format!("skončil kódem {code}, ale ovladač v systému není")),
                "{m}"
            );
            assert!(m.contains(vigembus::RELEASES_URL));
        }
        // Zrušená výzva / zaneprázdněné Windows bez ovladače → zkusit znovu.
        assert_eq!(settle(1602, none), Outcome::Cancelled);
        assert_eq!(settle(1223, none), Outcome::Cancelled);
        assert_eq!(settle(1618, none), Outcome::Busy);
        // Je, neběží: 3010 = restart (kód 3010), jinak rada podle zařízení.
        let blocked = there(dev(Some(48), false), true);
        assert_eq!(settle(3010, blocked), Outcome::RebootRequired);
        assert_eq!(settle(3010, blocked).exit_code(), 3010);
        assert_eq!(
            settle(0, blocked),
            Outcome::NotRunning {
                advice: vigembus::Advice::Blocked(48),
                after_setup: true
            }
        );
        // Zařízení samo chce restart → restart i po kódu 0.
        assert_eq!(
            settle(0, there(dev(None, true), true)),
            Outcome::RebootRequired
        );
        // Bez zařízení a bez záznamu v Aplikacích: rada bez Aplikací.
        let o = settle(0, there(None, false));
        assert_eq!(o.exit_code(), 1);
        assert!(!o.message().contains("Nastavení → Aplikace"));
    }

    #[test]
    fn pojistka_pred_instalaci() {
        assert_eq!(precheck(BusState::NotInstalled), None);
        assert_eq!(precheck(BusState::Ready), Some(Outcome::AlreadyReady));
        let o = precheck(BusState::InstalledNotRunning {
            device: dev(Some(22), false),
            in_apps: true,
        })
        .unwrap();
        assert_eq!(
            o,
            Outcome::NotRunning {
                advice: vigembus::Advice::EnableDevice,
                after_setup: false
            }
        );
        assert_eq!(o.message(), vigembus::Advice::EnableDevice.text());
        // Nainstalovaný dřív, čeká na restart → 3010 i bez instalátoru.
        let o = precheck(BusState::InstalledNotRunning {
            device: dev(None, true),
            in_apps: true,
        })
        .unwrap();
        assert_eq!(o.exit_code(), 3010);
    }

    /// Odmítnutá složka rozbalování: po jejím smazání má smysl zkusit
    /// znovu, a ruční instalaci nenabízet — rozbalovala by do téže složky.
    #[test]
    fn odmitnuta_slozka_nabidne_opakovani_ne_rucni_instalaci() {
        let o = extraction_refused("složka x už existuje".into());
        assert!(o.retry_makes_sense());
        assert!(!o.message().contains(vigembus::RELEASES_URL));
        assert_eq!(o.exit_code(), 1);
    }

    #[test]
    fn ukazkove_vysledky_pokryvaji_vsechny_druhy() {
        let all = sample_outcomes();
        let has = |f: fn(&Outcome) -> bool| all.iter().any(f);
        assert!(has(|o| matches!(o, Outcome::Installed)));
        assert!(has(|o| matches!(o, Outcome::RebootRequired)));
        assert!(has(|o| matches!(
            o,
            Outcome::NotRunning {
                after_setup: true,
                ..
            }
        )));
        assert!(has(|o| matches!(
            o,
            Outcome::NotRunning {
                after_setup: false,
                ..
            }
        )));
        assert!(has(|o| matches!(o, Outcome::Failed { retry: true, .. })));
        assert!(has(|o| matches!(o, Outcome::Failed { retry: false, .. })));
        assert!(has(|o| o.message().contains("nezaložil správce")));
    }

    #[test]
    fn slozka_rozbalovani() {
        let v = Path::new(r"C:\ProgramData\Nefarius Software Solutions");
        let x = Path::new(r"C:\ProgramData\Nefarius Software Solutions\ViGEm Bus Driver 1.22.0");
        // Nic tam není — v pořádku.
        assert!(extraction_verdict(v, false, false, None, x, false).is_ok());
        // Složka verze už je — nikdy.
        let e = extraction_verdict(v, true, false, Some("S-1-5-18"), x, true).unwrap_err();
        assert!(e.contains("už existuje") && e.contains("smaž ji"), "{e}");
        assert!(e.contains(&x.display().to_string()));
        // Složka výrobce od správce (jiný produkt Nefarius) — v pořádku.
        for o in TRUSTED_OWNERS {
            assert!(extraction_verdict(v, true, false, Some(o), x, false).is_ok());
        }
        // Od běžného uživatele, nečitelná, nebo odkaz jinam — ne.
        assert!(extraction_verdict(v, true, false, Some("S-1-5-21-1-2-3-1001"), x, false).is_err());
        assert!(extraction_verdict(v, true, false, None, x, false).is_err());
        assert!(
            extraction_verdict(v, true, true, Some("S-1-5-18"), x, false)
                .unwrap_err()
                .contains("odkaz")
        );
    }

    /// Vlastní jméno mutexu pro každý testovací proces — skutečné
    /// [`SINGLE_RUN_MUTEX`] by test shodilo, kdyby zrovna běžel
    /// KeyPadSetup /vigembus, a naopak by mu zablokovalo instalaci.
    #[test]
    fn instalace_ovladace_jen_jedna_naraz() {
        let name = format!(
            "Local\\KeyPad.Test.InstalaceViGEmBus.{}",
            std::process::id()
        );
        assert_ne!(name, SINGLE_RUN_MUTEX);
        let first = SingleRun::acquire(&name).expect("první smí");
        assert!(SingleRun::acquire(&name).is_none(), "druhá musí počkat");
        drop(first);
        assert!(
            SingleRun::acquire(&name).is_some(),
            "po skončení první zase smí"
        );
    }

    #[test]
    fn vydavatel_se_porovnava_presne() {
        assert!(signer_matches("Nefarius Software Solutions e.U.").is_ok());
        assert!(signer_matches("Nefarius Software Solutions").is_err());
        assert!(signer_matches("nefarius software solutions e.u.").is_err());
    }

    /// Nepodepsaný soubor (testovací binárka) podpisem neprojde — a nic
    /// se kvůli tomu neptá sítě ani neukazuje.
    #[test]
    fn nepodepsany_soubor_neprojde() {
        let me = std::env::current_exe().unwrap();
        let f = std::fs::File::open(&me).unwrap();
        let r = signer_of(&me, HANDLE(f.as_raw_handle()));
        assert!(r.unwrap_err().contains("neprošel"));
    }

    #[test]
    fn cerstva_slozka_je_pokazde_jina_a_prazdna() {
        let base = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("keypad-fresh-test-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let a = fresh_dir(&base).unwrap();
        let b = fresh_dir(&base).unwrap();
        assert_ne!(a, b);
        assert!(a
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(TEMP_PREFIX));
        assert_eq!(std::fs::read_dir(&a).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Soubor ze staženého podpisu: zápis, zámek, druhý otisk a podpis
    /// Nefarius — celý postup až po spuštění (to se v testu NIKDY
    /// nedělá). Bere lokální kopii z `KEYPAD_TEST_VIGEMBUS_SETUP`, jinak
    /// stahuje z GitHubu:
    /// `cargo test -p installer -- --ignored podpis_oficialniho`.
    ///
    /// Hledání DLL se nejdřív omezí stejně jako v `main` — test tak zároveň
    /// ověřuje, že WinVerifyTrust s knihovnami jen ze System32 funguje
    /// (a s podstrčenými DLL vedle testovací binárky je nenačte).
    #[test]
    #[ignore = "potřebuje oficiální instalátor ViGEmBus (soubor nebo síť)"]
    fn podpis_oficialniho_instalatoru_a_zamek() {
        crate::harden_dll_search().expect("SetDefaultDllDirectories");
        let data = match std::env::var_os("KEYPAD_TEST_VIGEMBUS_SETUP") {
            Some(p) => std::fs::read(p).unwrap(),
            None => vigembus::download_verified(|_| {}).unwrap(),
        };
        vigembus::verify(&data).unwrap();
        let st = Staged::create(&data).unwrap();
        assert!(st.path.starts_with(std::env::temp_dir()));
        // Zámek: zapsat ani smazat nejde, číst ano (spuštění = čtení).
        assert!(std::fs::OpenOptions::new()
            .write(true)
            .open(&st.path)
            .is_err());
        assert!(std::fs::remove_file(&st.path).is_err());
        assert!(std::fs::rename(&st.dir, st.dir.with_extension("jinam")).is_err());
        assert!(std::fs::File::open(&st.path).is_ok());
        st.verify_signature().unwrap();
        let (dir, path) = (st.dir.clone(), st.path.clone());
        drop(st);
        assert!(!path.exists() && !dir.exists(), "úklid po sobě");
    }
}

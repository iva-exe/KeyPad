//! Instalace a aktualizace ovladače ViGEmBus z KeyPadSetupu.
//!
//! Jediné místo, kde se kvůli KeyPadu něco spouští s právy správce —
//! a ani tady to není KeyPad: instalátor požádá Windows, aby s právy
//! správce spustily oficiální podepsaný instalátor Nefarius, ověřený
//! bajt po bajtu (pin v `updater::vigembus`). Výzvu UAC ukazují Windows
//! i se jménem ověřeného vydavatele — ta je souhlasem (Fáze 2b: ovladač
//! se instaluje i aktualizuje sám, bez zaškrtávátka, aby kamarád nemusel
//! nic hledat). Odmítnutá výzva instalaci KeyPadu nikdy nezkazí.
//!
//! Postup:
//! 1. **Pojistka.** Co se smí ([`decide`]): ovladač úplně chybí →
//!    instalace; je, ale prokazatelně starší než poslední vydání →
//!    aktualizace. Aktuální, neznámý nebo na restart čekající ovladač se
//!    nikdy nepřeinstalovává (MSI by při „první instalaci" odebralo jeho
//!    zařízení a při upgradu chtělo restart). Složka, kam zavaděč Advanced
//!    Installer rozbaluje MSI, nesmí existovat ani patřit běžnému
//!    uživateli (klasický vektor zvýšení práv — složku v ProgramData smí
//!    založit kdokoli předem). Stav se tu VŽDY čte skutečný — ladicí
//!    předstírání stavu v `main.rs` sem nedosáhne.
//! 2. **Stažení** do paměti (oficiální adresa, pak zrcadlo), přesná
//!    velikost + SHA-256.
//! 3. **Zápis a zámek.** Čerstvá náhodná složka v `%TEMP%`, soubor přes
//!    CREATE_NEW, pak znovu otevřený jen pro čtení se zákazem zápisu
//!    a mazání a druhý SHA-256 z téhož handlu. Soubor se až do konce
//!    instalace nedá přepsat, přejmenovat ani smazat (ani složka, dokud
//!    je v ní otevřený soubor).
//! 4. **Podpis** Authenticode přes týž handle (`WinVerifyTrust`, bez UI,
//!    bez kontroly odvolání — hlavní kontrolou je otisk a síťové dotazy
//!    by uměly viset) a jméno vydavatele.
//! 5. **Běžící KeyPad pryč — jen u aktualizace.** KeyPad drží sběrnici
//!    otevřenou (a zapnutý pad na ní); MSI by pak odebrání starého
//!    zařízení odložilo na restart, nebo by pad zmizel uprostřed hry.
//!    Proto se nainstalovaný KeyPad před aktualizací zavře stejně jako
//!    před aktualizací aplikace (událost → WM_CLOSE → natvrdo,
//!    `proc::close_app`) — v každém režimu, i v `/vigembus`, který
//!    spouští sama aplikace. Kdo ho zavřel, spustí ho potom znovu
//!    ([`Ran::closed_app`]). Když zavřít nejde, aktualizace se nespustí.
//!    U čisté instalace sběrnice není, takže ji nikdo držet nemůže.
//! 6. **Pojistka znovu**, těsně před spuštěním: stažení a zápis trvají
//!    sekundy až minuty a složku rozbalování mezitím mohl kdokoli založit
//!    (stejně jako mohl ovladač změnit jiný program).
//! 7. **Spuštění** `ShellExecuteExW("runas")` plnou cestou, s oknem
//!    instalátoru jako vlastníkem — výzva UAC.
//! 8. **Čekání** na pracovním vlákně (okno se dál překresluje).
//! 9. **Ověření.** Kód návratu nic nedokazuje — 0 z MSI ani 3010 (vlastní
//!    akce, které ovladač instalují, mají chyby ignorovat). Po KAŽDÉM
//!    kódu se stav i verze ovladače čtou znovu a výsledek říká jen to, co
//!    se ověřilo ([`settle`]). Instalátor běží vždy jen JEDNOU. Vydání
//!    1.22.0 sice píše, že aktualizace na místě nemusí fungovat (upgrade
//!    starou verzi odebere a novou nepřidá), jenže po takovém upgradu
//!    zůstává záznam nové verze v Aplikacích a co MSI udělá, když ho
//!    spustíme nad už zapsaným ProductCode (údržba / oprava), jsme
//!    neověřili. Takový stav (ovladač bez zařízení) proto dostane radu
//!    odebrat „ViGEm Bus Driver" v Aplikacích a spustit KeyPadSetup
//!    znovu — nikdy „restartuj, pak naběhne": kořenové zařízení sběrnice
//!    zakládá instalátor a restart ho nevytvoří.
//! 10. **Úklid.** Ovladač se nikdy neopravuje ani neodinstalovává.
//!
//! Uživatel vidí krátkou větu; kódy (MSI, WinVerifyTrust, HTTP) a přesné
//! chyby jdou do `KeyPadSetup.log` (a v headless režimu do konzole) —
//! [`Outcome::detail`].
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

use updater::vigembus::{self, Advice, BusState, DownloadError, Source};
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

use crate::{log, proc, Report};

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
/// Jak dlouho po úspěšném konci instalátoru čekat, než ovladač naběhne
/// (rozhraní sběrnice, u aktualizace i nová verze) — rozbíhá se chvíli
/// po skončení MSI.
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

/// Co se s ovladačem udělá.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    /// Ovladač v systému úplně chybí.
    Install,
    /// Ovladač je, ale starší než poslední vydání
    /// (`vigembus::needs_update`).
    Update,
}

/// Proč se to nepovedlo — podle toho zpráva a co nabídnout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fail {
    /// Instalátor nejde stáhnout z žádného zdroje (síť).
    Download,
    /// Stažený soubor (nebo soubor na disku) není ten oficiální, případně
    /// neprošel podpis. Opakování by dopadlo stejně → ruční instalace.
    NotOfficial,
    /// Instalátor nejde uložit do `%TEMP%`.
    Disk,
    /// Windows instalátor nespustily (jinak než odmítnutou výzvou).
    Launch,
    /// Instalátor doběhl, ale ovladač v systému není → ruční instalace.
    Setup,
    /// Aktualizace neprošla; starší ovladač zůstal a dál běží.
    Update,
    /// Po aktualizaci po ovladači nezbyla ani stopa (ani záznam
    /// v Aplikacích) — „Zkusit znovu" ho nainstaluje načisto (vlastní
    /// výzvou UAC, na povel uživatele).
    Removed,
    /// Běžící KeyPad nejde před aktualizací zavřít (drží sběrnici);
    /// instalátor ovladače se proto nespustil.
    AppRunning,
    /// Složka rozbalování nevyhověla pojistce. Po jejím smazání má smysl
    /// zkusit znovu; ruční instalace NE — oficiální instalátor spuštěný
    /// ručně by rozbaloval do téže složky.
    Folder(PathBuf),
    /// Ovladač už instaluje jiné okno KeyPadSetupu.
    OtherWindow,
}

/// Jak instalace / aktualizace ovladače dopadla.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nainstalováno a rozhraní sběrnice je vidět.
    Installed,
    /// Aktualizováno: běží a verze je aktuální. `restart` = instalátor
    /// chtěl restart (3010/1641) — u upgradu z 1.18–1.21 ho MSI chce
    /// vždy (ForceReboot, potlačený `/norestart`): nová verze je zapsaná,
    /// dokončí se po restartu. Uživatel to má vědět.
    Updated { restart: bool },
    /// Ovladač běží v aktuální verzi — nebylo co dělat.
    AlreadyReady,
    /// Ovladač poběží po restartu: instalátor skončil kódem 3010/1641,
    /// říká to stav zařízení, nebo aktualizace nedoběhla celá.
    RebootRequired,
    /// Uživatel nepotvrdil výzvu UAC nebo instalaci zrušil. `update` =
    /// starší ovladač zůstal (jinak ovladač v systému není).
    Cancelled { update: bool },
    /// Windows zrovna instalují něco jiného (1618).
    Busy,
    /// Ovladač v systému je, ale neběží — rada, co s tím.
    NotRunning(Advice),
    /// Instalátor běží déle než [`SETUP_TIMEOUT`] — nechává se doběhnout.
    /// Ruční instalace ani „zkusit znovu" tu nepomůžou (běží pořád
    /// tentýž); cesta je restart.
    TimedOut,
    /// Nepovedlo se; `detail` jde do logu, ne do okna.
    Failed { fail: Fail, detail: String },
}

fn failed(fail: Fail, detail: impl Into<String>) -> Outcome {
    Outcome::Failed {
        fail,
        detail: detail.into(),
    }
}

impl Outcome {
    /// Ovladač běží (v aktuální verzi).
    pub fn is_ready(&self) -> bool {
        matches!(
            self,
            Outcome::Installed | Outcome::Updated { .. } | Outcome::AlreadyReady
        )
    }

    /// Musí to uživatel vědět, než se okno samo zavře (tichý režim)?
    /// Vše, co neběží — a aktualizace, která chce restart.
    pub fn needs_attention(&self) -> bool {
        !self.is_ready() || matches!(self, Outcome::Updated { restart: true })
    }

    /// Má smysl nabídnout „Zkusit znovu ovladač"?
    pub fn retry_makes_sense(&self) -> bool {
        matches!(
            self,
            Outcome::Cancelled { .. }
                | Outcome::Busy
                | Outcome::Failed {
                    fail: Fail::Download
                        | Fail::Disk
                        | Fail::Launch
                        | Fail::Removed
                        | Fail::AppRunning
                        | Fail::Folder(_)
                        | Fail::OtherWindow,
                    ..
                }
        )
    }

    /// Vede cesta přes ruční instalaci ze stránky vydání? (Okno k ní
    /// nabídne tlačítko, konzole adresu.)
    pub fn manual_install(&self) -> bool {
        match self {
            Outcome::Failed {
                fail: Fail::NotOfficial | Fail::Setup,
                ..
            } => true,
            Outcome::NotRunning(a) => a.suggests_manual_install(),
            _ => false,
        }
    }

    /// Krátká česká věta pro okno i konzoli — bez kódů; ty jsou
    /// v [`Outcome::detail`].
    pub fn message(&self) -> String {
        match self {
            Outcome::Installed => "Ovladač ViGEmBus je nainstalovaný.".into(),
            Outcome::Updated { restart: false } => "Ovladač ViGEmBus je aktualizovaný.".into(),
            Outcome::Updated { restart: true } => {
                "Ovladač ViGEmBus je aktualizovaný — dokončí se po restartu počítače.".into()
            }
            Outcome::AlreadyReady => "Ovladač ViGEmBus je v pořádku.".into(),
            Outcome::RebootRequired => Advice::Restart.text(),
            Outcome::Cancelled { update: false } => {
                "Ovladač ViGEmBus se nenainstaloval — výzva Windows nebyla potvrzena.".into()
            }
            Outcome::Cancelled { update: true } => {
                "Ovladač ViGEmBus se neaktualizoval — výzva Windows nebyla potvrzena.".into()
            }
            Outcome::Busy => {
                "Windows zrovna instalují něco jiného — ovladač ViGEmBus zkus za chvíli.".into()
            }
            Outcome::NotRunning(advice) => advice.text(),
            Outcome::TimedOut => {
                "Instalátor ViGEmBus pořád běží — nech ho doběhnout a restartuj počítač.".into()
            }
            Outcome::Failed { fail, .. } => match fail {
                Fail::Download => {
                    "Instalátor ViGEmBus se nepodařilo stáhnout — zkontroluj připojení a zkus \
                     to znovu."
                        .into()
                }
                Fail::NotOfficial => {
                    "Stažený instalátor ViGEmBus neprošel kontrolou, proto se nespustil.".into()
                }
                Fail::Disk => "Instalátor ViGEmBus nejde uložit — zkus to znovu.".into(),
                Fail::Launch => "Instalátor ViGEmBus nejde spustit — zkus to znovu.".into(),
                Fail::Setup => "Ovladač ViGEmBus se nepodařilo nainstalovat.".into(),
                Fail::Update => {
                    "Ovladač ViGEmBus se nepodařilo aktualizovat — zůstává starší verze.".into()
                }
                Fail::Removed => {
                    "Aktualizace starý ovladač ViGEmBus odebrala a nový nepřidala — zkus to \
                     znovu."
                        .into()
                }
                Fail::AppRunning => {
                    "Ovladač ViGEmBus se neaktualizoval — běžící KeyPad nejde zavřít. Zavři ho \
                     a zkus to znovu."
                        .into()
                }
                // Tady uživatel jednat musí — cesta ke složce do zprávy patří.
                Fail::Folder(p) => format!(
                    "Instalaci ovladače ViGEmBus brání složka {} — smaž ji a zkus to znovu.",
                    p.display()
                ),
                Fail::OtherWindow => {
                    "Ovladač ViGEmBus už instaluje jiné okno — počkej, až doběhne.".into()
                }
            },
        }
    }

    /// Technické podrobnosti pro log a konzoli (prázdné, když nejsou).
    pub fn detail(&self) -> &str {
        match self {
            Outcome::Failed { detail, .. } => detail,
            _ => "",
        }
    }

    /// Kód návratu — 0 = ovladač běží, 3010 = je v systému a poběží po
    /// restartu (stejný kód jako u MSI), 1 = jinak. Protože se výsledek
    /// skládá z ověřeného stavu (ne z kódu instalátoru), je 0 jen tam,
    /// kde rozhraní sběrnice opravdu je. (Konečný kód procesu skládá
    /// `main::driver_exit_code` ještě se skutečným stavem na konci.)
    pub fn exit_code(&self) -> i32 {
        match self {
            o if o.is_ready() => 0,
            Outcome::RebootRequired => 3010,
            _ => 1,
        }
    }
}

/// Skutečný stav ovladače a jestli je starší než poslední vydání.
pub fn current() -> (BusState, bool) {
    (vigembus::state(), vigembus::needs_update())
}

/// Pojistka (krok 1 a 5): co se s ovladačem smí udělat, nebo výsledek,
/// když se instalátor spouštět nebude.
///
/// Ovladač, který čeká na restart, dostane radu „restartuj", i když je
/// starší — instalátor nad nedokončenou změnou (typicky předchozí
/// aktualizace) nic nespraví a jen by přidal další restart. Neznámou
/// verzi (`outdated` = false) KeyPad nikdy nepřeinstalovává.
pub fn decide(state: BusState, outdated: bool) -> Result<Work, Outcome> {
    match state {
        BusState::NotInstalled => Ok(Work::Install),
        BusState::Ready if outdated => Ok(Work::Update),
        BusState::Ready => Err(Outcome::AlreadyReady),
        s @ BusState::InstalledNotRunning { .. } => {
            let advice = s.advice().unwrap_or(Advice::Restart);
            if outdated && advice != Advice::Restart {
                Ok(Work::Update)
            } else {
                Err(not_running(advice, false))
            }
        }
    }
}

/// Ovladač je, ale neběží. Když pomůže restart (říká to instalátor
/// kódem 3010/1641, nebo stav zařízení), je to [`Outcome::RebootRequired`]
/// — aplikace podle kódu 3010 pozná, že nemá co zkoušet hned.
fn not_running(advice: Advice, setup_said_reboot: bool) -> Outcome {
    if setup_said_reboot || advice == Advice::Restart {
        Outcome::RebootRequired
    } else {
        Outcome::NotRunning(advice)
    }
}

/// Čisté rozhodnutí po doběhnutí instalátoru: co se dělalo (`work`),
/// kód návratu `code` a stav ovladače `now` + `outdated`, přečtené ZNOVU
/// až po něm.
///
/// Rozhoduje stav, ne kód (princip 8 — hlásit jen ověřené): kód 0 bez
/// ovladače (vlastní akce MSI chyby ignorují) je selhání a 3010 u už
/// běžícího aktuálního ovladače úspěch. Kód rozhoduje jen tam, kde se
/// nic neověřilo: zrušená výzva a zaneprázdněný instalátor Windows mají
/// vlastní radu (zkusit znovu), a „restartuj" smí říct jen instalátor,
/// který doběhl (0 / 3010), nebo zařízení samo.
fn settle(work: Work, code: u32, now: BusState, outdated: bool) -> Outcome {
    let exit = map_exit(code);
    let update = work == Work::Update;
    // Běží a je aktuální (u čisté instalace stačí, že běží — starší
    // verzi instalátor 1.22.0 dodat neumí).
    if now == BusState::Ready && (!outdated || !update) {
        return if update {
            Outcome::Updated {
                restart: exit == Exit::Reboot,
            }
        } else {
            Outcome::Installed
        };
    }
    match exit {
        Exit::Cancelled => {
            return Outcome::Cancelled {
                update: update && now != BusState::NotInstalled,
            }
        }
        Exit::Busy => return Outcome::Busy,
        _ => {}
    }
    let finished = matches!(exit, Exit::Ok | Exit::Reboot);
    match (work, now) {
        // Ovladač je (záznam v Aplikacích / služba), ale jeho zařízení
        // ne — typicky upgrade, který starou verzi odebral a novou
        // nepřidal. Kořenové zařízení sběrnice zakládá instalátor, restart
        // ho nevytvoří: „pak naběhne" by lhalo, i když MSI hlásilo 3010.
        // Druhý běh sám nespouštíme (viz hlavička, krok 9).
        (
            _,
            BusState::InstalledNotRunning {
                device: None,
                in_apps,
            },
        ) => Outcome::NotRunning(Advice::Reinstall { in_apps }),
        // Zařízení je a neběží. Doběhnutá aktualizace (0 / 3010) má novou
        // verzi zapsanou a zařízení se rozběhne po restartu. Neprošlá
        // (1603…) MSI vrátilo zpátky — zařízení je ve stejném stavu jako
        // předtím a restart by nepomohl (vypnuté zůstane vypnuté): rada
        // podle zařízení.
        (Work::Update, s @ BusState::InstalledNotRunning { .. }) => {
            not_running(s.advice().unwrap_or(Advice::Restart), finished)
        }
        (Work::Install, s @ BusState::InstalledNotRunning { .. }) => {
            not_running(s.advice().unwrap_or(Advice::Restart), exit == Exit::Reboot)
        }
        // Pořád běží starší ovladač.
        (Work::Update, BusState::Ready) if finished => Outcome::RebootRequired,
        (Work::Update, BusState::Ready) => failed(
            Fail::Update,
            format!("instalátor ViGEmBus skončil kódem {code}, ovladač zůstal ve starší verzi"),
        ),
        // Po ovladači nezbylo nic ani po aktualizaci. Po doběhnutém MSI
        // zůstává záznam nové verze a po neprošlém MSI (návrat) záznam
        // staré, takže sem se nejspíš nedojde — kdyby přece, nový běh
        // spustí až uživatel („Zkusit znovu" = čistá instalace).
        (Work::Update, BusState::NotInstalled) => failed(
            Fail::Removed,
            format!(
                "instalátor ViGEmBus (aktualizace) skončil kódem {code} a ovladač v systému není"
            ),
        ),
        (Work::Install, _) => failed(
            Fail::Setup,
            format!("instalátor ViGEmBus skončil kódem {code}, ale ovladač v systému není"),
        ),
    }
}

/// Výsledek kroku ovladače.
#[derive(Debug)]
pub struct Ran {
    pub outcome: Outcome,
    /// Kvůli aktualizaci se zavíral běžící nainstalovaný KeyPad (krok 5).
    /// Volající, který KeyPad na konci sám nespouští (`/vigembus`,
    /// „Zkusit znovu ovladač"), ho podle toho spustí znovu.
    pub closed_app: bool,
}

/// Musí se před spuštěním instalátoru ViGEmBus zavřít běžící KeyPad?
/// Jen u aktualizace — u čisté instalace sběrnice není a držet ji nejde.
fn closes_app(work: Work) -> bool {
    work == Work::Update
}

/// Nainstaluje nebo aktualizuje ViGEmBus (celý postup z hlavičky
/// modulu). `step` = index kroku ovladače v okně. Průběh i výsledek
/// s podrobnostmi jde do logu.
pub fn install(rep: &mut dyn Report, step: usize) -> Ran {
    let mut closed_app = false;
    let o = run(rep, step, &mut closed_app);
    log::line(&format!(
        "ovladač: {:?} → {} {}",
        o,
        o.message(),
        o.detail()
    ));
    Ran {
        outcome: o,
        closed_app,
    }
}

/// Zavře nainstalovaný KeyPad před aktualizací ovladače (krok 5).
/// `closed` = opravdu běžel a skončil.
fn close_app_for_update(rep: &mut dyn Report, closed: &mut bool) -> Result<(), Outcome> {
    let app = updater::install_dir().join(updater::APP_EXE);
    match proc::close_app(&app, updater::QUIT_EVENT_NAME, &mut |s| rep.status(s)) {
        Ok(proc::Closed::NotRunning) => Ok(()),
        Ok(how) => {
            *closed = true;
            log::line(&format!(
                "ovladač: KeyPad zavřen před aktualizací ({how:?})"
            ));
            Ok(())
        }
        Err(e) => Err(failed(Fail::AppRunning, e)),
    }
}

fn run(rep: &mut dyn Report, step: usize, closed_app: &mut bool) -> Outcome {
    rep.step(step, "kontroluji ovladač…");
    rep.progress(None);
    // Jen jedna instalace ovladače naráz (dvakrát kliknuté tlačítko
    // v aplikaci, dvě okna). Druhá by jinak prošla pojistkou dřív, než
    // první něco zapsala, a skončila by až u Windows (1618) nebo
    // u složky rozbalování, kterou mezitím založila ta první.
    let Some(_single) = SingleRun::acquire(SINGLE_RUN_MUTEX) else {
        return failed(
            Fail::OtherWindow,
            format!("mutex {SINGLE_RUN_MUTEX} drží jiný proces"),
        );
    };
    remove_leftovers();

    // ── 1. Pojistka ── (už tady, ať se zbytečně nestahuje)
    let (state, outdated) = current();
    log::line(&format!(
        "ovladač: stav {state:?}, verze {:?}, poslední {:?}",
        vigembus::driver_version(),
        vigembus::DRIVER_VERSION
    ));
    let work = match decide(state, outdated) {
        Ok(w) => w,
        Err(o) => return o,
    };
    if let Err(o) = check_extraction_dir() {
        return o;
    }

    // ── 2. Stažení ──
    rep.status(match work {
        Work::Install => "stahuji instalátor ViGEmBus…",
        Work::Update => "stahuji aktualizaci ViGEmBus…",
    });
    let mut last = 0usize;
    let mut from = Source::Official;
    let fetched = vigembus::fetch_setup(|src, n| {
        if src != from {
            from = src;
            last = 0;
            rep.status("oficiální adresa nejde — stahuji ze zálohy…");
        }
        if n >= last + 256 * 1024 {
            last = n;
            rep.download("ViGEmBus", n);
        }
    });
    let data = match fetched {
        Ok((d, src)) => {
            log::line(&format!("ovladač: instalátor stažen z {}", src.url()));
            d
        }
        Err(e @ DownloadError::Network(_)) => return failed(Fail::Download, e.to_string()),
        Err(e) => return failed(Fail::NotOfficial, e.to_string()),
    };

    // ── 3. Zápis a zámek, 4. podpis ──
    rep.status("ověřuji instalátor…");
    let staged = match Staged::create(&data) {
        Ok(s) => s,
        Err(o) => return o,
    };
    drop(data);
    if let Err(e) = staged.verify_signature() {
        return failed(Fail::NotOfficial, e);
    }

    // ── 5.–9. Běžící KeyPad pryč, pojistka znovu, spuštění, čekání,
    // ověření ──
    let ran = match run_setup(rep, &staged, closed_app) {
        Ok(r) => r,
        Err(o) => return o,
    };
    let (now, outdated) = verify_after(rep, ran);
    settle(ran.0, ran.1, now, outdated)
}

/// Pojistka znovu, zavření KeyPadu (jen aktualizace), výzva UAC
/// a čekání na instalátor. Vrací, co se dělalo, a kód návratu.
fn run_setup(
    rep: &mut dyn Report,
    staged: &Staged,
    closed_app: &mut bool,
) -> Result<(Work, u32), Outcome> {
    // Mezi první kontrolou a tímhle místem je stažení (sekundy až
    // minuty) — dost času na to, aby složku rozbalování založil běžný
    // uživatel nebo ovladač změnil jiný program. Zbývající okno (výzva
    // UAC čeká na uživatele) viz „Zbytkové riziko" v hlavičce.
    rep.status("kontroluji systém…");
    let (state, outdated) = current();
    let work = decide(state, outdated)?;
    // Až tady — po stažení a ověření (když selžou, KeyPad běží dál)
    // a podle stavu přečteného těsně předtím, co se opravdu spustí.
    if closes_app(work) {
        rep.status("zavírám KeyPad (drží ovladač)…");
        close_app_for_update(rep, closed_app)?;
    }
    check_extraction_dir()?;

    rep.status("potvrď výzvu Windows…");
    let child = match run_elevated(rep.owner(), &staged.path, &staged.dir) {
        Ok(c) => c,
        Err(Run::Cancelled) => {
            log::line("ovladač: výzva UAC nebyla potvrzena");
            return Err(Outcome::Cancelled {
                update: work == Work::Update,
            });
        }
        Err(Run::Failed(reason)) => return Err(failed(Fail::Launch, reason)),
    };

    rep.status(match work {
        Work::Install => "instaluji ovladač…",
        Work::Update => "aktualizuji ovladač…",
    });
    let Some(code) = child.wait(SETUP_TIMEOUT) else {
        // Instalátor pořád běží — nechat ho doběhnout (zabít proces
        // s právy správce ani nejde a přerušené MSI je horší než pomalé).
        // Zámek se pustí, soubor zůstane, dokud instalátor nedoběhne.
        return Err(Outcome::TimedOut);
    };
    log::line(&format!(
        "ovladač: instalátor ViGEmBus ({work:?}) skončil kódem {code}"
    ));
    Ok((work, code))
}

/// Po instalátoru: u „úspěšných" kódů se ovladači dá chvíle na
/// rozběhnutí (u aktualizace i na novou verzi); u ostatních stačí jedno
/// čtení (MSI skončilo chybou, ovladač se nerozbíhá). Vrací znovu
/// přečtený stav.
fn verify_after(rep: &mut dyn Report, (work, code): (Work, u32)) -> (BusState, bool) {
    if matches!(map_exit(code), Exit::Ok | Exit::Reboot) {
        rep.status("ověřuji ovladač…");
        let deadline = Instant::now() + READY_TIMEOUT;
        while !(vigembus::interface_present()
            && (work == Work::Install || !vigembus::needs_update()))
            && Instant::now() < deadline
        {
            std::thread::sleep(READY_POLL);
        }
    }
    let now = current();
    log::line(&format!(
        "ovladač: po instalátoru stav {:?}, verze {:?}",
        now.0,
        vigembus::driver_version()
    ));
    now
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
fn check_extraction_dir() -> Result<(), Outcome> {
    let Some(pd) = program_data() else {
        return Err(failed(Fail::Launch, "nejde zjistit složku ProgramData"));
    };
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
    .map_err(|(path, why)| failed(Fail::Folder(path), why))
}

/// Čisté rozhodnutí k [`check_extraction_dir`]: `Err((složka, proč))`.
///
/// Proč vůbec (do okna se to nevejde, tak je to tady a v README):
/// zavaděč Advanced Installer rozbaluje MSI do složky verze s právy
/// správce a z ní ho i spouští. Složku v ProgramData smí založit
/// kdokoli a kdo ji založil, ten ji vlastní — rozbalené soubory v ní
/// může vyměnit nebo k nim podstrčit DLL, a ty pak běží s právy
/// správce (klasické zvýšení práv). Složka výrobce patřící správci je
/// v pořádku (jiný produkt Nefarius); patřící uživateli nebo vedoucí
/// jinam (spojení, symbolický odkaz) ne. Okno řekne jen kterou složku
/// smazat; důvod jde do logu.
fn extraction_verdict(
    vendor: &Path,
    vendor_exists: bool,
    vendor_reparse: bool,
    vendor_owner: Option<&str>,
    version: &Path,
    version_exists: bool,
) -> Result<(), (PathBuf, String)> {
    if version_exists {
        return Err((
            version.to_path_buf(),
            "složka rozbalování už existuje (mohl ji předem založit kdokoli)".into(),
        ));
    }
    if !vendor_exists {
        return Ok(());
    }
    if vendor_reparse {
        return Err((
            vendor.to_path_buf(),
            "složka výrobce je odkaz jinam (spojení / symbolický odkaz)".into(),
        ));
    }
    match vendor_owner {
        Some(o) if TRUSTED_OWNERS.contains(&o) => Ok(()),
        o => Err((
            vendor.to_path_buf(),
            format!("složku výrobce nezaložil správce (vlastník {o:?})"),
        )),
    }
}

/// Všechny výsledky, které instalace ovladače umí skutečně vrátit,
/// s nejdelšími skutečnými texty (cesty jako na běžném PC) — pro
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
        Outcome::Updated { restart: false },
        Outcome::Updated { restart: true },
        Outcome::AlreadyReady,
        Outcome::RebootRequired,
        Outcome::Cancelled { update: false },
        Outcome::Cancelled { update: true },
        Outcome::Busy,
        Outcome::TimedOut,
        failed(Fail::OtherWindow, ""),
    ];
    for f in [
        Fail::Download,
        Fail::NotOfficial,
        Fail::Disk,
        Fail::Launch,
        Fail::Setup,
        Fail::Update,
        Fail::Removed,
        Fail::AppRunning,
    ] {
        out.push(failed(f, "podrobnosti jen do logu"));
    }
    for refused in [
        extraction_verdict(&vendor, true, false, Some("S-1-5-18"), &version, true),
        extraction_verdict(&vendor, true, true, None, &version, false),
        extraction_verdict(&vendor, true, false, user, &version, false),
    ] {
        out.extend(refused.err().map(|(p, why)| failed(Fail::Folder(p), why)));
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
            for outdated in [false, true] {
                out.extend(decide(s, outdated).err());
                for work in [Work::Install, Work::Update] {
                    for code in [0, 3010, 1602, 1603] {
                        out.push(settle(work, code, s, outdated));
                    }
                }
            }
        }
    }
    for code in [0, 3010, 1602, 1618, 1603, u32::MAX] {
        for work in [Work::Install, Work::Update] {
            for (now, outdated) in [(BusState::NotInstalled, false), (BusState::Ready, true)] {
                out.push(settle(work, code, now, outdated));
            }
        }
    }
    out
}

/// Výsledek po instalátoru pro ladicí náhled obrazovek — přes skutečné
/// [`settle`]. Ovladač má záznam v Aplikacích; `problem` = kód problému
/// jeho zařízení (`None` = zařízení není).
#[cfg(debug_assertions)]
pub fn preview_settle(work: Work, code: u32, problem: Option<u32>, outdated: bool) -> Outcome {
    let device = problem.map(|p| vigembus::DeviceStatus {
        problem: Some(p),
        need_restart: false,
        started: false,
    });
    let now = BusState::InstalledNotRunning {
        device,
        in_apps: true,
    };
    settle(work, code, now, outdated)
}

/// Náhled: po aktualizaci po ovladači nezbylo nic.
#[cfg(debug_assertions)]
pub fn preview_removed() -> Outcome {
    settle(Work::Update, 0, BusState::NotInstalled, false)
}

/// Ladicí sonda k testu podstrčených DLL (jen debug build, spouští ji
/// `KEYPAD_SETUP_TEST_KNIHOVNY` v `main` po omezení hledání DLL): projde
/// volání, při kterých systémové DLL za běhu načítají další knihovny —
/// ProgramData a jeho vlastník (profapi), zápis, zámek a podpis lokální
/// kopie oficiálního instalátoru (CRYPTSP, CRYPTBASE). Instalátor se
/// NIKDY nespouští; dočasná složka se po sobě uklidí.
#[cfg(debug_assertions)]
pub fn load_probe(setup: &Path) -> Result<String, String> {
    let why = |o: Outcome| format!("{} ({})", o.message(), o.detail());
    let pd = program_data().ok_or("ProgramData nejde zjistit")?;
    check_extraction_dir().map_err(why)?;
    let data = std::fs::read(setup).map_err(|e| format!("{}: {e}", setup.display()))?;
    vigembus::verify(&data)?;
    let staged = Staged::create(&data).map_err(why)?;
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
        // Disk a zámek = má smysl zkusit znovu; změněný soubor = není
        // oficiální (opakování by nepomohlo).
        let disk = |what: String| failed(Fail::Disk, what);
        let dir = fresh_dir(&std::env::temp_dir())
            .map_err(|e| disk(format!("nejde založit dočasnou složku ({e})")))?;
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
                .map_err(|e| disk(format!("nejde uložit instalátor ({e})")))?;
            f.write_all(data)
                .and_then(|_| f.flush())
                .map_err(|e| disk(format!("nejde uložit instalátor ({e})")))?;
        }
        // Znovu jen pro čtení; ostatní smí jen číst (spuštění je čtení),
        // zapsat, přejmenovat ani smazat nemůže nikdo, dokud handle žije.
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&st.path)
            .map_err(|e| disk(format!("nejde zamknout instalátor ({e})")))?;
        let digest = updater::sha256::sha256_reader(&mut f).map_err(disk)?;
        // Mezi zavřením zápisu a zamčením je okamžik, kdy soubor mohl
        // někdo vyměnit — druhý otisk z uzamčeného handlu to pozná.
        vigembus::verify_digest(&digest).map_err(|e| {
            failed(
                Fail::NotOfficial,
                format!("soubor se na disku po uložení změnil — {e}"),
            )
        })?;
        f.seek(SeekFrom::Start(0))
            .map_err(|e| disk(format!("čtení instalátoru ({e})")))?;
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
        for restart in [false, true] {
            assert_eq!(Outcome::Updated { restart }.exit_code(), 0);
        }
        // Aktualizace, která chce restart: ovladač běží, ale uživatel to
        // má vědět (okno se samo nezavře).
        let r = Outcome::Updated { restart: true };
        assert!(r.is_ready() && r.needs_attention());
        assert!(r.message().contains("po restartu"));
        assert!(!Outcome::Updated { restart: false }.needs_attention());
        assert!(!Outcome::Installed.needs_attention());
        assert!(Outcome::RebootRequired.needs_attention());
        assert_eq!(Outcome::AlreadyReady.exit_code(), 0);
        assert_eq!(Outcome::RebootRequired.exit_code(), 3010);
        assert!(Outcome::RebootRequired
            .message()
            .contains("Restartuj počítač"));
        let c = Outcome::Cancelled { update: false };
        assert_eq!(c.exit_code(), 1);
        assert!(c.retry_makes_sense());
        assert!(Outcome::Cancelled { update: true }
            .message()
            .contains("neaktualizoval"));
        assert!(Outcome::Busy.retry_makes_sense());
        let net = failed(Fail::Download, "github.com: timeout");
        assert!(net.retry_makes_sense() && !net.manual_install());
        let bad = failed(Fail::NotOfficial, "otisk nesedí (89220A78…)");
        assert!(!bad.retry_makes_sense() && bad.manual_install());
        // Podrobnosti do logu, ne do okna.
        assert!(!bad.message().contains("89220A78"));
        assert_eq!(bad.detail(), "otisk nesedí (89220A78…)");
        let nr = Outcome::NotRunning(Advice::Blocked(48));
        assert_eq!(nr.message(), Advice::Blocked(48).text());
        assert_eq!(nr.exit_code(), 1);
        // Neprošlá aktualizace: starší ovladač zůstal — nic ručně, nic znovu.
        let u = failed(Fail::Update, "kód 1603");
        assert!(!u.retry_makes_sense() && !u.manual_install());
        assert!(u.message().contains("starší verze"));
        // Instalátor pořád běží: restart, ne ruční instalace ani znovu.
        let t = Outcome::TimedOut;
        assert_eq!(t.exit_code(), 1);
        assert!(!t.retry_makes_sense() && !t.manual_install());
        assert!(t.message().contains("restartuj počítač"));
    }

    /// Žádná zpráva pro uživatele nenese kód ani otisk — ty jsou v logu.
    /// Výjimky jsou jen tam, kde uživatel musí jednat: cesta ke složce,
    /// kterou má smazat, a adresa ruční instalace v radě (tu sdílí
    /// s aplikací; okno k ní dá i tlačítko).
    #[test]
    fn zpravy_bez_kodu() {
        for o in sample_outcomes() {
            let m = o.message();
            for bad in ["kód", "0x", "3010", "1603", "SHA", "S-1-5", "podrobnosti"] {
                assert!(!m.contains(bad), "{bad} v „{m}“");
            }
            if !matches!(o, Outcome::NotRunning(_)) {
                assert!(!m.contains("http"), "{m}");
            }
            assert!(m.chars().count() < 170, "dlouhé: {m}");
        }
    }

    fn dev(problem: Option<u32>, need_restart: bool) -> Option<vigembus::DeviceStatus> {
        Some(vigembus::DeviceStatus {
            problem,
            need_restart,
            started: false,
        })
    }

    fn there(device: Option<vigembus::DeviceStatus>) -> BusState {
        BusState::InstalledNotRunning {
            device,
            in_apps: true,
        }
    }

    /// Kdy se instalátor ViGEmBus vůbec smí spustit.
    #[test]
    fn pojistka_co_se_smi() {
        assert_eq!(decide(BusState::NotInstalled, false), Ok(Work::Install));
        assert_eq!(decide(BusState::Ready, true), Ok(Work::Update));
        // Běžící aktuální nebo neznámý ovladač se nikdy nepřeinstalovává.
        assert_eq!(decide(BusState::Ready, false), Err(Outcome::AlreadyReady));
        // Je, neběží, verze neznámá / aktuální → jen rada.
        assert_eq!(
            decide(there(dev(Some(22), false)), false),
            Err(Outcome::NotRunning(Advice::EnableDevice))
        );
        assert_eq!(
            decide(there(dev(None, true)), false),
            Err(Outcome::RebootRequired)
        );
        // Je, neběží a je prokazatelně starší → aktualizace…
        assert_eq!(decide(there(dev(Some(22), false)), true), Ok(Work::Update));
        assert_eq!(decide(there(dev(Some(48), false)), true), Ok(Work::Update));
        // …ale čeká-li na restart, nejdřív restart (žádný další běh nad
        // nedokončenou změnou).
        assert_eq!(
            decide(there(dev(None, true)), true),
            Err(Outcome::RebootRequired)
        );
        assert_eq!(
            decide(there(dev(Some(14), false)), true),
            Err(Outcome::RebootRequired)
        );
        assert_eq!(
            decide(there(dev(None, true)), true)
                .unwrap_err()
                .exit_code(),
            3010
        );
    }

    /// Po čisté instalaci rozhoduje znovu přečtený stav, ne kód: 0 jen
    /// s běžícím ovladačem, 3010 jen s ovladačem, který v systému je,
    /// jinak selhání s ruční cestou (princip 8).
    #[test]
    fn po_instalaci_rozhoduje_stav_ne_kod() {
        let fin = |code, now| settle(Work::Install, code, now, false);
        for code in [0, 3010, 1641, 1603, 1602] {
            assert_eq!(fin(code, BusState::Ready), Outcome::Installed);
        }
        for code in [0, 3010, 1641, 1603] {
            let o = fin(code, BusState::NotInstalled);
            assert!(
                matches!(
                    &o,
                    Outcome::Failed {
                        fail: Fail::Setup,
                        ..
                    }
                ),
                "{o:?}"
            );
            assert!(o.detail().contains(&format!("kódem {code}")), "{o:?}");
            assert!(!o.retry_makes_sense() && o.manual_install());
        }
        assert_eq!(
            fin(1602, BusState::NotInstalled),
            Outcome::Cancelled { update: false }
        );
        assert_eq!(
            fin(1223, BusState::NotInstalled),
            Outcome::Cancelled { update: false }
        );
        assert_eq!(fin(1618, BusState::NotInstalled), Outcome::Busy);
        // Je, neběží: 3010 = restart, jinak rada podle zařízení.
        let blocked = there(dev(Some(48), false));
        assert_eq!(fin(3010, blocked), Outcome::RebootRequired);
        assert_eq!(fin(0, blocked), Outcome::NotRunning(Advice::Blocked(48)));
        assert_eq!(fin(0, there(dev(None, true))), Outcome::RebootRequired);
        let o = fin(
            0,
            BusState::InstalledNotRunning {
                device: None,
                in_apps: false,
            },
        );
        assert!(o.manual_install() && o.exit_code() == 1, "{o:?}");
        // Záznam v Aplikacích bez zařízení: ani s 3010 ne „pak naběhne" —
        // kořenové zařízení sběrnice restart nevytvoří.
        for code in [0, 3010] {
            let o = fin(
                code,
                BusState::InstalledNotRunning {
                    device: None,
                    in_apps: true,
                },
            );
            assert_eq!(o, Outcome::NotRunning(Advice::Reinstall { in_apps: true }));
            assert_eq!(o.exit_code(), 1);
        }
    }

    /// Aktualizace: aktuální a běžící = hotovo; pořád starší a instalátor
    /// doběhl = restart; MSI chyba = starší zůstal; odmítnutá výzva =
    /// starší zůstal. Instalátor se nikdy nespouští podruhé sám.
    #[test]
    fn po_aktualizaci() {
        let up = |code, now, outdated| settle(Work::Update, code, now, outdated);
        // Běží aktuální: hotovo; s 3010 (ForceReboot u upgradu) „dokončí
        // se po restartu".
        for (code, restart) in [(0, false), (3010, true), (1641, true), (1603, false)] {
            assert_eq!(
                up(code, BusState::Ready, false),
                Outcome::Updated { restart }
            );
        }
        // Pořád běží starší a instalátor doběhl → restartuj počítač.
        assert_eq!(up(0, BusState::Ready, true), Outcome::RebootRequired);
        assert_eq!(up(3010, BusState::Ready, true), Outcome::RebootRequired);
        // Zařízení je a neběží, instalátor doběhl → nová verze po restartu.
        for outdated in [false, true] {
            for code in [0, 3010] {
                assert_eq!(
                    up(code, there(dev(Some(48), false)), outdated),
                    Outcome::RebootRequired
                );
            }
        }
        // MSI chyba a starší ovladač běží dál.
        match up(1603, BusState::Ready, true) {
            o @ Outcome::Failed {
                fail: Fail::Update, ..
            } => assert!(o.detail().contains("1603")),
            o => panic!("{o:?}"),
        }
        // Odmítnutá výzva: starší zůstal. Kdyby ovladač mezitím zmizel,
        // zpráva nesmí tvrdit, že zůstal.
        assert_eq!(
            up(1602, BusState::Ready, true),
            Outcome::Cancelled { update: true }
        );
        assert_eq!(
            up(1602, BusState::NotInstalled, false),
            Outcome::Cancelled { update: false }
        );
        assert_eq!(up(1618, BusState::Ready, true), Outcome::Busy);
        // Po ovladači nezbylo nic (sem se po MSI nejspíš nedojde — viz
        // `settle`): žádný druhý běh sám od sebe; „Zkusit znovu" na povel
        // uživatele pak projde pojistkou jako čistá instalace.
        for code in [0, 3010, 1603] {
            let o = up(code, BusState::NotInstalled, false);
            assert!(
                matches!(
                    o,
                    Outcome::Failed {
                        fail: Fail::Removed,
                        ..
                    }
                ),
                "{o:?}"
            );
            assert!(o.retry_makes_sense() && !o.manual_install());
            assert_eq!(o.exit_code(), 1);
            assert!(o.detail().contains(&format!("kódem {code}")));
        }
        assert_eq!(decide(BusState::NotInstalled, false), Ok(Work::Install));
    }

    /// Skutečný stav po upgradu, který starou verzi odebral a novou
    /// nepřidal (nález review): záznam nové verze (ProductCode 1.22.0)
    /// v Aplikacích zůstal, zařízení sběrnice ne, verze se bez zařízení
    /// nedá přečíst. Rada = odebrat v Aplikacích a spustit KeyPadSetup
    /// znovu, kód 1 — nikdy „restartuj, pak naběhne" (3010) a nikdy druhý
    /// běh instalátoru nad už zapsaným ProductCode (neověřené chování MSI).
    #[test]
    fn aktualizace_bez_zarizeni_radi_odebrat_a_spustit_znovu() {
        let after = BusState::InstalledNotRunning {
            device: None,
            in_apps: true,
        };
        for code in [0, 3010, 1641, 1603, u32::MAX] {
            for outdated in [false, true] {
                let o = settle(Work::Update, code, after, outdated);
                assert_eq!(
                    o,
                    Outcome::NotRunning(Advice::Reinstall { in_apps: true }),
                    "kód {code}"
                );
                assert_eq!(o.exit_code(), 1);
                assert!(o.needs_attention() && !o.retry_makes_sense() && !o.manual_install());
                let m = o.message();
                assert!(
                    !m.contains("pak naběhne") && !m.contains("Restartuj"),
                    "{m}"
                );
                assert!(m.contains("„ViGEm Bus Driver“"), "{m}");
                assert!(m.contains("spusť KeyPadSetup znovu"), "{m}");
            }
        }
        // Po restartu pojistka nic nespustí a řekne totéž.
        assert_eq!(
            decide(after, false),
            Err(Outcome::NotRunning(Advice::Reinstall { in_apps: true }))
        );
        // Bez záznamu v Aplikacích tam neposílat — ruční instalace.
        let o = settle(
            Work::Update,
            0,
            BusState::InstalledNotRunning {
                device: None,
                in_apps: false,
            },
            false,
        );
        assert!(o.manual_install() && o.exit_code() == 1, "{o:?}");
    }

    /// Neprošlá aktualizace (1603…) vypnutého nebo zablokovaného staršího
    /// ovladače: MSI vrátilo zpátky a zařízení je, jak bylo. Rada podle
    /// zařízení a kód 1 — ne „restartuj" s 3010 (restart by nepomohl
    /// a aplikace by na něj čekala). „Restartuj" jen tam, kde to chce
    /// zařízení samo, nebo kde instalátor doběhl.
    #[test]
    fn neprosla_aktualizace_radi_podle_zarizeni() {
        let up = |code, d| settle(Work::Update, code, there(d), true);
        for code in [1603, u32::MAX] {
            let off = up(code, dev(Some(22), false));
            assert_eq!(off, Outcome::NotRunning(Advice::EnableDevice));
            let blocked = up(code, dev(Some(48), false));
            assert_eq!(blocked, Outcome::NotRunning(Advice::Blocked(48)));
            for o in [off, blocked] {
                assert_eq!(o.exit_code(), 1);
                assert!(!o.message().contains("Restartuj"), "{}", o.message());
            }
            assert_eq!(up(code, dev(None, true)), Outcome::RebootRequired);
            assert_eq!(up(code, dev(Some(14), false)), Outcome::RebootRequired);
        }
        for code in [0, 3010] {
            for problem in [22, 48] {
                assert_eq!(up(code, dev(Some(problem), false)), Outcome::RebootRequired);
            }
        }
    }

    /// Běžící KeyPad drží sběrnici — zavírá se jen před aktualizací;
    /// u čisté instalace sběrnice není.
    #[test]
    fn keypad_se_zavira_jen_pred_aktualizaci() {
        assert!(closes_app(Work::Update));
        assert!(!closes_app(Work::Install));
        let o = failed(Fail::AppRunning, "KeyPad běží jako správce");
        assert!(o.retry_makes_sense() && !o.manual_install());
        assert!(o.message().contains("Zavři ho"));
        assert_eq!(o.exit_code(), 1);
    }

    /// Odmítnutá složka rozbalování: po jejím smazání má smysl zkusit
    /// znovu, a ruční instalaci nenabízet — rozbalovala by do téže složky.
    #[test]
    fn odmitnuta_slozka_nabidne_opakovani_ne_rucni_instalaci() {
        let o = failed(Fail::Folder(PathBuf::from(r"C:\x")), "už existuje");
        assert!(o.retry_makes_sense() && !o.manual_install());
        assert!(o.message().contains(r"C:\x") && o.message().contains("smaž ji"));
        assert!(!o.message().contains("už existuje"));
        assert_eq!(o.exit_code(), 1);
    }

    #[test]
    fn ukazkove_vysledky_pokryvaji_vsechny_druhy() {
        let all = sample_outcomes();
        let has = |f: fn(&Outcome) -> bool| all.iter().any(f);
        assert!(has(|o| matches!(o, Outcome::Installed)));
        assert!(has(|o| matches!(o, Outcome::Updated { restart: true })));
        assert!(has(|o| matches!(o, Outcome::RebootRequired)));
        assert!(has(|o| matches!(o, Outcome::NotRunning(_))));
        assert!(has(|o| o.retry_makes_sense()));
        assert!(has(|o| o.manual_install()));
        assert!(has(|o| matches!(
            o,
            Outcome::Failed {
                fail: Fail::Folder(_),
                ..
            }
        )));
    }

    #[test]
    fn slozka_rozbalovani() {
        let v = Path::new(r"C:\ProgramData\Nefarius Software Solutions");
        let x = Path::new(r"C:\ProgramData\Nefarius Software Solutions\ViGEm Bus Driver 1.22.0");
        // Nic tam není — v pořádku.
        assert!(extraction_verdict(v, false, false, None, x, false).is_ok());
        // Složka verze už je — nikdy; smazat se má ona.
        let (p, why) = extraction_verdict(v, true, false, Some("S-1-5-18"), x, true).unwrap_err();
        assert_eq!(p, x);
        assert!(why.contains("už existuje"), "{why}");
        // Složka výrobce od správce (jiný produkt Nefarius) — v pořádku.
        for o in TRUSTED_OWNERS {
            assert!(extraction_verdict(v, true, false, Some(o), x, false).is_ok());
        }
        // Od běžného uživatele, nečitelná, nebo odkaz jinam — ne, a smazat
        // se má složka výrobce.
        for (reparse, owner) in [
            (false, Some("S-1-5-21-1-2-3-1001")),
            (false, None),
            (true, Some("S-1-5-18")),
        ] {
            let (p, _) = extraction_verdict(v, true, reparse, owner, x, false).unwrap_err();
            assert_eq!(p, v);
        }
        assert!(
            extraction_verdict(v, true, true, Some("S-1-5-18"), x, false)
                .unwrap_err()
                .1
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

    /// Špatný soubor se na disk nedostane dál než do zámku: druhý otisk
    /// z uzamčeného handlu ho odmítne jako neoficiální a složka se uklidí.
    #[test]
    fn neoficialni_soubor_neprojde_zamkem() {
        let o = match Staged::create(b"MZ tohle neni instalator") {
            Err(o) => o,
            Ok(_) => panic!("neoficiální soubor prošel"),
        };
        assert!(
            matches!(
                o,
                Outcome::Failed {
                    fail: Fail::NotOfficial,
                    ..
                }
            ),
            "{o:?}"
        );
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

//! Aktualizace aplikace.
//!
//! Zdroj pravdy je `release/version.txt` v repozitáři — týž soubor,
//! ze kterého bere verzi instalátor (kanál vydání drží crate `updater`,
//! sdílený s `KeyPadSetup.exe`, ať se aplikace a instalátor nemůžou
//! rozejít v tom, co je „nejnovější").
//!
//! Samotnou aktualizaci NEDĚLÁME sami: stáhne se `KeyPadSetup.exe`
//! a spustí se s `/quiet`. Ten umí zavřít tohle okno, přepsat binárku
//! a aplikaci spustit znovu — druhá implementace téhož by se s ním
//! nutně rozešla. Instalace je per-user (`%LOCALAPPDATA%\Programs`),
//! takže se spouští obyčejně: žádné „runas", žádná výzva UAC.
//!
//! Nabízí se jen tam, kde to instalátor opravdu udělá: kopii
//! v instalační složce, a jen NOVĚJŠÍ verze (viz [`check_update`]).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// Odpověď pro frontend (`lib/updater.svelte.ts`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UpdateInfo {
    /// Verze, která běží teď. Prázdná u vývojového buildu.
    current: String,
    /// Verze v repozitáři; prázdná, když se nepodařilo zjistit.
    latest: String,
    /// Je co aktualizovat? Jen když obě verze známe a ta v repozitáři
    /// je novější.
    available: bool,
    /// Proč se nepodařilo zjistit (síť, GitHub, vývojový build,
    /// přenosná kopie). UI to v banneru nekřičí — kontrola jede každou
    /// minutu a výpadek sítě není porucha aplikace.
    error: Option<String>,
}

/// Složka, ze které běží tahle binárka.
pub fn slozka_exe() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

/// Verze, která běží: `version.txt` VEDLE BĚŽÍCÍ BINÁRKY.
///
/// Schválně ne `updater::installed_version()` (instalační složka): kdyby
/// na stroji byla nainstalovaná kopie a vedle ní se spustil vývojový
/// build z `target\`, ten by převzal její verzi. Vývojový build
/// `version.txt` nemá, takže se u něj ukáže „(vývoj)". Jestli jde
/// běžící kopii aktualizovat, rozhoduje až [`aktualizovatelna_verze`].
pub fn bezici_verze() -> Option<String> {
    updater::version_in(&slozka_exe()?)
}

/// Verze běžící kopie — pokud ji jde aktualizovat. Jinak důvod proč ne.
///
/// Instalátor aktualizuje VŽDY instalační složku. Přenosná kopie jinde
/// (třeba `release\KeyPad.exe` s `version.txt` vedle, jak ho nechává
/// publish.ps1) by aktualizaci nabízela, instalátor by ale přepsal
/// instalaci, běžící kopie by zůstala stará a banner by nešel odbýt.
fn aktualizovatelna_verze() -> Result<String, String> {
    let Some(current) = bezici_verze() else {
        return Err(
            "vývojová verze (vedle programu není version.txt) — aktualizace se nenabízí".into(),
        );
    };
    let z_instalace = std::env::current_exe().is_ok_and(|exe| updater::runs_from_install_dir(&exe));
    if !z_instalace {
        return Err(format!(
            "přenosná kopie (neběží z {}) — aktualizuj přes KeyPadSetup.exe",
            updater::install_dir().display()
        ));
    }
    Ok(current)
}

/// Poslední výsledek kontroly — do logu jde jen ZMĚNA. Kontrola běží
/// každou minutu; zapisovat každou by z logu udělalo seznam kontrol.
static POSLEDNI: Mutex<Option<UpdateInfo>> = Mutex::new(None);

/// Je v repozitáři NOVĚJŠÍ verze, než která běží?
///
/// Novější, ne jen jiná ([`updater::is_newer`]): verze se čte přes
/// `raw`, které drží `version.txt` v CDN cache až 5 minut, kdežto
/// instalátor bere verzi z čerstvého commitu. Hned po aktualizaci by
/// tak čerstvě nainstalovaná verze mohla z cache dostat tu PŘEDCHOZÍ
/// a nabízet ji jako „novou" — a klik by nic neudělal. Návrat ke
/// staršímu vydání se v aplikaci proto nenabízí; udělá ho ruční
/// spuštění KeyPadSetup.exe.
///
/// `async` = běží mimo hlavní vlákno: dotaz jde po síti a hlavní vlákno
/// obsluhuje okno — čekání na GitHub by ho zamrazilo.
#[tauri::command(async)]
pub fn check_update() -> UpdateInfo {
    let info = zjisti();
    if let Ok(mut posledni) = POSLEDNI.lock() {
        if posledni.as_ref() != Some(&info) {
            zaloguj(&info);
            *posledni = Some(info.clone());
        }
    }
    info
}

fn zjisti() -> UpdateInfo {
    let current = bezici_verze().unwrap_or_default();
    let nejde = |error: String| UpdateInfo {
        current: current.clone(),
        latest: String::new(),
        available: false,
        error: Some(error),
    };
    if let Err(e) = aktualizovatelna_verze() {
        return nejde(e);
    }
    // Verze se čte přímo z větve přes raw (`remote_version`), ne přes
    // commit z API: API má pro nepřihlášené 60 dotazů za hodinu a
    // kontrola každou minutu by ho vyčerpala (WinSent na to naletěl).
    match updater::remote_version() {
        Ok(latest) => UpdateInfo {
            available: updater::is_newer(&latest, &current),
            current,
            latest,
            error: None,
        },
        Err(e) => nejde(e),
    }
}

fn zaloguj(info: &UpdateInfo) {
    match (&info.error, info.available) {
        (Some(e), _) => log::info!("kontrola aktualizace: {e}"),
        (None, true) => log::info!(
            "je dostupná nová verze {} (běží {})",
            info.latest,
            info.current
        ),
        (None, false) => log::info!(
            "aktualizace: verze {} je aktuální (v repozitáři {})",
            info.current,
            info.latest
        ),
    }
}

/// Stáhne instalátor a spustí ho.
///
/// Vrací se hned, jakmile instalátor běží — čekat nemá smysl: jeho
/// první práce je zavřít tohle okno. Zbytek (binárka, nové spuštění)
/// dělá on. Kdyby okno nezavřel (selhal dřív), UI tlačítko po chvíli
/// samo uvolní (`lib/updater.svelte.ts`).
#[tauri::command(async)]
pub fn run_update() -> Result<String, String> {
    let vysledek = stahni_a_spust();
    match &vysledek {
        Ok(cesta) => log::info!("aktualizace: spuštěn instalátor {cesta}"),
        Err(e) => log::warn!("aktualizace selhala: {e}"),
    }
    vysledek
}

fn stahni_a_spust() -> Result<String, String> {
    let current = aktualizovatelna_verze()?;
    log::info!("aktualizace: zjišťuji poslední vydání");
    // Commit místo větve: soubory z jednoho konkrétního commitu, ne mix
    // nové a staré verze z CDN cache (viz `updater::latest_commit`).
    let sha = updater::latest_commit().map_err(|e| format!("nové vydání nejde najít: {e}"))?;

    // Verze z TÉHOŽ commitu, ze kterého se bude instalovat — ne z raw
    // cache, podle které se banner ukázal. Když novější není (cache se
    // rozcházela s commitem), instalátor by nic nenainstaloval, okno
    // by nezavřel a tlačítko by viselo. Chyba místo toho UI uvolní.
    let nova = updater::fetch_release_file(&sha, updater::VERSION_FILE, |_| {})
        .map(|data| String::from_utf8_lossy(&data).trim().to_string())
        .map_err(|e| format!("verzi nového vydání nejde zjistit: {e}"))?;
    if nova.is_empty() {
        return Err("vydání na GitHubu nemá verzi — zkus to za pár minut znovu".into());
    }
    if !updater::is_newer(&nova, &current) {
        return Err(format!(
            "už máš nejnovější verzi {current} (GitHub chvíli ukazoval jiný údaj) — \
             kdyby nabídka nezmizela, zkus to za pár minut znovu"
        ));
    }
    log::info!("aktualizace: {current} → {nova} (commit {})", zkratka(&sha));

    let data = updater::fetch_release_file(&sha, updater::SETUP_EXE, |_| {})
        .map_err(|e| format!("instalátor nejde stáhnout: {e}"))?;
    // Když místo instalátoru dorazí chybová stránka nebo pár bajtů,
    // pozná se to tady — ne až ve chvíli, kdy by Windows odmítly
    // „aplikaci" spustit s nesrozumitelnou hláškou. Useknutý přenos
    // hlídá už `updater::http::get` (ohlášená délka).
    if !updater::looks_like_exe(&data) {
        return Err(format!(
            "instalátor se stáhl poškozený ({} B) — zkus to za chvíli znovu",
            data.len()
        ));
    }

    let slozka = updater::update_temp_dir();
    std::fs::create_dir_all(&slozka)
        .map_err(|e| format!("složku pro instalátor nejde založit: {e}"))?;
    uklid_stare_instalatory(&slozka);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let exe = slozka.join(jmeno_instalatoru(&sha, ms));
    std::fs::write(&exe, &data).map_err(|e| format!("instalátor nejde uložit: {e}"))?;

    // Obyčejné spuštění: instalátor je per-user a v manifestu má
    // asInvoker, takže práva správce nepotřebuje a UAC se neukáže.
    std::process::Command::new(&exe)
        .arg("/quiet")
        .spawn()
        .map_err(|e| format!("instalátor nejde spustit: {e}"))?;
    Ok(exe.display().to_string())
}

/// Prvních 7 znaků commitu (jako na GitHubu).
fn zkratka(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// Jméno staženého instalátoru — jiné pro každý pokus.
///
/// Instalátor z minulého pokusu může pořád běžet (skončil chybou a jeho
/// okno s hláškou zůstalo otevřené) a Windows drží jeho soubor zamčený.
/// Stejné jméno by nový pokus skončilo chybou sdílení (os error 32),
/// jméno podle commitu nestačí — opakuje se týž commit. Proto čas
/// v milisekundách; commit v jménu je jen pro čtení logu.
fn jmeno_instalatoru(sha: &str, ms: u128) -> String {
    format!("KeyPadSetup-{}-{ms}.exe", zkratka(sha))
}

/// Je soubor instalátor stažený aktualizací (viz [`jmeno_instalatoru`])?
fn je_stazeny_instalator(jmeno: &str) -> bool {
    let j = jmeno.to_ascii_lowercase();
    j.starts_with("keypadsetup-") && j.ends_with(".exe")
}

/// Smaže instalátory z minulých aktualizací — každý má kolem 0,6 MB
/// a nic jiného je neuklidí. Jen úklid: chyby se ignorují a soubor,
/// který zrovna běží (instalátor s otevřeným oknem), zůstane do příště.
fn uklid_stare_instalatory(slozka: &Path) {
    let Ok(polozky) = std::fs::read_dir(slozka) else {
        return;
    };
    for polozka in polozky.flatten() {
        let je_soubor = polozka.file_type().is_ok_and(|t| t.is_file());
        if je_soubor && je_stazeny_instalator(&polozka.file_name().to_string_lossy()) {
            let _ = std::fs::remove_file(polozka.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jmeno_instalatoru_je_pro_kazdy_pokus_jine() {
        let sha = "6f39a52c0ffee1234";
        assert_eq!(
            jmeno_instalatoru(sha, 1_727_200_000_123),
            "KeyPadSetup-6f39a52-1727200000123.exe"
        );
        assert_ne!(jmeno_instalatoru(sha, 1), jmeno_instalatoru(sha, 2));
        // Kratší „commit" (nemělo by nastat) nesmí spadnout na řezu.
        assert_eq!(jmeno_instalatoru("abc", 5), "KeyPadSetup-abc-5.exe");
    }

    #[test]
    fn uklid_pozna_jen_stazene_instalatory() {
        assert!(je_stazeny_instalator(
            "KeyPadSetup-6f39a52-1727200000123.exe"
        ));
        // Staré jméno (jen commit) z předchozích verzí aplikace.
        assert!(je_stazeny_instalator("KeyPadSetup-6f39a52.exe"));
        assert!(je_stazeny_instalator("keypadsetup-x.EXE"));
        assert!(!je_stazeny_instalator("KeyPadSetup.exe"));
        assert!(!je_stazeny_instalator("KeyPad.exe"));
        assert!(!je_stazeny_instalator("KeyPadSetup-6f39a52.exe.txt"));
    }

    #[test]
    fn uklid_smaze_stare_a_nic_jineho() {
        let dir = std::env::temp_dir().join(format!("keypad-upd-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("KeyPadSetup-slozka.exe")).unwrap();
        std::fs::write(dir.join("KeyPadSetup-aaaaaaa.exe"), b"x").unwrap();
        std::fs::write(dir.join("KeyPadSetup-bbbbbbb-1.exe"), b"x").unwrap();
        std::fs::write(dir.join("jiny.txt"), b"x").unwrap();

        uklid_stare_instalatory(&dir);

        assert!(!dir.join("KeyPadSetup-aaaaaaa.exe").exists());
        assert!(!dir.join("KeyPadSetup-bbbbbbb-1.exe").exists());
        assert!(dir.join("jiny.txt").exists(), "cizí soubor zůstává");
        assert!(
            dir.join("KeyPadSetup-slozka.exe").is_dir(),
            "složka zůstává"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

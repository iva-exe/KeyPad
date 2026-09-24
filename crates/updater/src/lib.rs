//! Kanál vydání KeyPadu a cesty instalace.
//!
//! Jeden zdroj pravdy pro instalátor (`KeyPadSetup.exe`) i aplikaci
//! (kontrola nové verze). Dvě kopie téhož by se dřív nebo později
//! rozešly — a pak by aplikace hlásila jinou verzi, než jakou instalátor
//! stáhne.
//!
//! Vydání = složka `release/` v repozitáři na GitHubu:
//!
//! ```text
//! release/version.txt      0.1.0+RRRRMMDD.HHMM (bez konce řádku)
//! release/KeyPad.exe       aplikace
//! release/KeyPadSetup.exe  instalátor (odsud si ho stahuje i aktualizace)
//! ```
//!
//! Plní ji `tools\publish.ps1`.

pub mod http;

use std::path::PathBuf;

/// Veřejný repozitář — kamarád nepotřebuje účet ani přístup.
pub const REPO: &str = "iva-exe/KeyPad";
pub const RAW_HOST: &str = "raw.githubusercontent.com";
pub const API_HOST: &str = "api.github.com";

/// Název aplikace (složky, zástupce, záznamu v Aplikacích).
pub const APP_NAME: &str = "KeyPad";
/// Binárka aplikace.
pub const APP_EXE: &str = "KeyPad.exe";
/// Binárka instalátoru.
pub const SETUP_EXE: &str = "KeyPadSetup.exe";
/// Soubor s verzí — v `release/` i vedle nainstalované aplikace.
pub const VERSION_FILE: &str = "version.txt";

/// Instalátor WebView2 Runtime od Microsoftu (Evergreen Bootstrapper).
/// Odkazuje na něj instalátor, aplikace i README — jedno místo pravdy.
pub const WEBVIEW2_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

/// Identifikátor aplikace — MUSÍ se shodovat s `identifier`
/// v `src-tauri/tauri.conf.json`. Tauri podle něj pojmenuje složku,
/// kam WebView2 ukládá cache a data okna; odinstalace ji podle něj najde.
pub const APP_IDENTIFIER: &str = "cz.hexel.keypad";

/// Data WebView2 (cache, localStorage okna): `%LOCALAPPDATA%\<identifier>`.
pub fn webview_data_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join(APP_IDENTIFIER))
}

/// Náhradní místo logu, když složka s `.exe` není zapisovatelná:
/// `%APPDATA%\KeyPad`.
pub fn roaming_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join(APP_NAME))
}

/// Zjistí commit, na kterém větev `main` právě stojí.
///
/// Proč tahle oklika: `raw.githubusercontent.com` drží soubory v CDN
/// cache 5 minut a **ignoruje query parametry**, takže se stará verze
/// nedá „obejít" přidáním `?t=`. Horší než zpoždění je ale míchání —
/// cache může vydat starou `version.txt` k novým binárkám. Adresa
/// s konkrétním commitem je naproti tomu neměnná: nový commit = jiná
/// cesta = žádná stará cache. Všechny soubory se pak stahují z jednoho
/// a téhož commitu.
///
/// POZOR: GitHub API má pro nepřihlášené limit 60 dotazů za hodinu na
/// IP. Volat jen tehdy, když se opravdu stahuje — na pravidelnou
/// kontrolu je [`remote_version`].
pub fn latest_commit() -> Result<String, String> {
    let body = http::get(API_HOST, &format!("/repos/{REPO}/commits/main"), |_| {})
        .map_err(|e| e.to_string())?;
    parse_commit_sha(&String::from_utf8_lossy(&body))
        .ok_or_else(|| "odpověď GitHubu neobsahuje platný commit".to_string())
}

/// Vytáhne SHA z odpovědi `GET /repos/{repo}/commits/main`.
///
/// Odpověď začíná `{"sha":"…"` — první výskyt je commit, který hledáme.
/// Kvůli jednomu poli se JSON knihovna tahat nemusí.
fn parse_commit_sha(text: &str) -> Option<String> {
    let pos = text.find("\"sha\":\"")?;
    let sha: String = text[pos + 7..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    (sha.len() >= 7).then_some(sha)
}

/// Verze, kterou má větev `main` v `release/version.txt`.
///
/// Čte se PŘÍMO z větve přes `raw`, ne přes commit z API: API má limit
/// 60 dotazů za hodinu a pravidelná kontrola by ho vyčerpala (WinSent
/// na to naletěl — kontrola začala vracet 403). `raw` jede přes CDN,
/// hodinový limit nemá a pro otázku „jaká verze je venku" stačí;
/// pětiminutová cache tu vadí jen tím, že se nová verze ohlásí o chvíli
/// později.
pub fn remote_version() -> Result<String, String> {
    let body = http::get(
        RAW_HOST,
        &format!("/{REPO}/main/release/{VERSION_FILE}"),
        |_| {},
    )
    .map_err(|e| e.to_string())?;
    let v = String::from_utf8_lossy(&body).trim().to_string();
    if v.is_empty() {
        return Err("server vrátil prázdnou verzi".into());
    }
    Ok(v)
}

/// Stáhne soubor z `release/` v daném commitu.
pub fn fetch_release_file(
    sha: &str,
    name: &str,
    progress: impl FnMut(usize),
) -> Result<Vec<u8>, String> {
    http::get(RAW_HOST, &format!("/{REPO}/{sha}/release/{name}"), progress)
        .map_err(|e| format!("{name}: {e}"))
}

/// Vypadá stažený obsah jako spustitelný soubor?
///
/// Každý PE soubor začíná „MZ". Když místo binárky dorazí chybová HTML
/// stránka nebo pár bajtů, pozná se to tady — ne až při spuštění.
/// Useknutý soubor tahle kontrola NEpozná (začátek má v pořádku); to
/// hlídá [`http::get`] porovnáním s ohlášenou délkou.
pub fn looks_like_exe(data: &[u8]) -> bool {
    data.len() >= 100_000 && data.starts_with(b"MZ")
}

/// Je vydání `remote` novější než běžící `current`?
///
/// Verze má tvar `X.Y.Z+RRRRMMDD.HHMM` (publish.ps1): základ se porovná
/// číselně, razítko buildu jako text — má pevnou šířku, takže textové
/// pořadí je časové.
///
/// Proč „novější", a ne jen „jiná": aplikace se ptá přes `raw`, které
/// drží `version.txt` v CDN cache až 5 minut, kdežto instalátor bere
/// verzi z čerstvého commitu. Hned po aktualizaci by tak čerstvě
/// nainstalovaná verze mohla z cache dostat tu PŘEDCHOZÍ a nabízet ji
/// jako „novou" — klik by nic neudělal (instalátor by viděl, že je
/// aktuální). Návrat ke staršímu vydání se proto v aplikaci nenabízí;
/// udělá ho ruční spuštění KeyPadSetup.exe (ten porovnává na rovnost).
///
/// Verze v jiném tvaru se porovnají jen na rozdíl — lepší nabídnout
/// zbytečnou aktualizaci, než mlčet o skutečné.
pub fn is_newer(remote: &str, current: &str) -> bool {
    match (parse_version(remote), parse_version(current)) {
        (Some(r), Some(c)) => r > c,
        _ => remote != current,
    }
}

/// `X.Y.Z+RAZITKO` → (čísla základu, razítko). Razítko musí mít pevný
/// tvar `RRRRMMDD.HHMM`, jinak by textové porovnání lhalo.
fn parse_version(v: &str) -> Option<(Vec<u64>, String)> {
    let (base, stamp) = v.trim().split_once('+')?;
    let nums = base
        .split('.')
        .map(|p| p.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    let ok_stamp = stamp.len() == 13
        && stamp
            .char_indices()
            .all(|(i, c)| if i == 8 { c == '.' } else { c.is_ascii_digit() });
    ok_stamp.then(|| (nums, stamp.to_string()))
}

/// Kam aplikace ukládá stažený instalátor: `%TEMP%\keypad-update`.
/// Odinstalace ji podle toho uklidí.
pub fn update_temp_dir() -> PathBuf {
    std::env::temp_dir().join("keypad-update")
}

/// Běží `exe` z instalační složky (`install_dir()\KeyPad.exe`)?
///
/// Aktualizace přepisuje právě tu kopii. Přenosná kopie jinde (třeba
/// `release\KeyPad.exe` s `version.txt` vedle) by aktualizaci nabízela,
/// instalátor by ale aktualizoval instalaci a běžící kopie by zůstala
/// stará — s bannerem, který nejde odbýt.
pub fn runs_from_install_dir(exe: &std::path::Path) -> bool {
    let norm = |p: &std::path::Path| {
        std::fs::canonicalize(p)
            .ok()
            .map(|p| p.to_string_lossy().to_lowercase())
    };
    match (norm(exe), norm(&install_dir().join(APP_EXE))) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Kam se KeyPad instaluje: `%LOCALAPPDATA%\Programs\KeyPad`.
///
/// Instalace je per-user (jako VS Code nebo Discord): zápis do profilu
/// nepotřebuje práva správce, takže instalátor ani aktualizace nikdy
/// nevyvolají výzvu UAC (princip 6 v ROADMAP.md).
pub fn install_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(r"AppData\Local"))
        })
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\Public\AppData\Local"));
    base.join("Programs").join(APP_NAME)
}

/// Verze uložená ve `version.txt` v dané složce. `None` = soubor chybí
/// nebo je prázdný.
pub fn version_in(dir: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(dir.join(VERSION_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Verze nainstalované kopie.
pub fn installed_version() -> Option<String> {
    version_in(&install_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha_z_odpovedi_api() {
        let body =
            r#"{"sha":"6f39a52c0ffee1234","node_id":"x","commit":{"tree":{"sha":"aaaaaaa"}}}"#;
        assert_eq!(parse_commit_sha(body).as_deref(), Some("6f39a52c0ffee1234"));
    }

    #[test]
    fn neplatna_odpoved_neni_commit() {
        assert_eq!(parse_commit_sha(r#"{"message":"Not Found"}"#), None);
        assert_eq!(parse_commit_sha(r#"{"sha":"12"}"#), None);
    }

    #[test]
    fn exe_se_pozna_podle_mz() {
        let mut ok = vec![0u8; 200_000];
        ok[0] = b'M';
        ok[1] = b'Z';
        assert!(looks_like_exe(&ok));
        assert!(!looks_like_exe(b"<!DOCTYPE html>"));
        assert!(!looks_like_exe(&ok[..1000]));
    }

    #[test]
    fn novejsi_verze() {
        let a = "0.1.0+20260924.2235";
        let b = "0.1.0+20260925.0010";
        assert!(is_newer(b, a));
        assert!(!is_newer(a, b), "starší z CDN cache se nenabízí");
        assert!(!is_newer(a, a));
        // Základ vyhrává nad razítkem.
        assert!(is_newer("0.2.0+20260101.0000", "0.1.9+20261231.2359"));
        assert!(is_newer("0.10.0+20260101.0000", "0.9.0+20260101.0000"));
        // Nečitelné tvary: jen rozdíl.
        assert!(is_newer("1.0", "0.1.0+20260924.2235"));
        assert!(!is_newer("vlastni", "vlastni"));
        assert!(is_newer("0.1.0+2026", "0.1.0+20260924.2235"));
    }

    #[test]
    fn razitko_musi_mit_pevny_tvar() {
        assert!(parse_version("0.1.0+20260924.2235").is_some());
        assert!(parse_version("0.1.0+2026092.42235").is_none());
        assert!(parse_version("0.1.0+20260924-2235").is_none());
        assert!(parse_version("0.1.x+20260924.2235").is_none());
    }

    #[test]
    fn cizi_cesta_neni_instalace() {
        assert!(!runs_from_install_dir(std::path::Path::new(
            r"C:\neexistuje\KeyPad.exe"
        )));
    }

    #[test]
    fn identifikator_sedi_s_tauri_conf() {
        // Odinstalace podle něj maže data WebView2 — rozejít se nesmí.
        let conf = include_str!("../../../src-tauri/tauri.conf.json");
        assert!(
            conf.contains(&format!("\"identifier\": \"{APP_IDENTIFIER}\"")),
            "APP_IDENTIFIER nesedí s tauri.conf.json"
        );
    }

    #[test]
    fn instalace_je_v_profilu() {
        let d = install_dir();
        assert!(d.ends_with(r"Programs\KeyPad"), "{}", d.display());
    }
}

//! Virtuální gamepad z pohledu okna: příkazy, událost se stavem padu
//! a popisek ikony v oznamovací oblasti.
//!
//! Samotný pad (ViGEmBus, pad vlákno) je v `platform::windows::pad`;
//! tady je jen lepidlo k Tauri, ať platformní kód na Tauri nezávisí
//! a jde testovat bez okna.
//!
//! Fáze 2b: ovladač se připojuje JEN přepínačem v okně (`pad_on`) —
//! nikdy sám po startu, po probuzení ani po aktualizaci.

use std::time::Duration;

use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::platform::windows::pad::{Oznam, Pad, PadInfo, PadPrikaz, PadStav, UDALOST};
use crate::platform::windows::{power, shell};

/// Jak dlouho při konci aplikace čekat na neutrál + odpojení padu.
/// Normálně milisekundy; když ovladač visí, proces skončí i tak a pad
/// odpojí ovladač sám se zavřením spojení.
const LIMIT_KONCE: Duration = Duration::from_millis(1_500);

/// Jak dlouho před spuštěním instalátoru ViGEmBus čekat, než pad vlákno
/// ovladač vypne. Normálně milisekundy; zdrží ho jen rozjeté zapínání
/// (`wait_ready`). Bez potvrzení se instalátor nespustí.
const LIMIT_VYPNUTI: Duration = Duration::from_secs(3);

/// Spustí pad vlákno a hlídání spánku; stav je pak v `app.state::<Pad>()`.
/// Pad vlákno sběrnici jen ověří — nic nepřipojí.
pub fn spust(app: &tauri::App) {
    let handle = app.handle().clone();
    let oznam: Oznam = std::sync::Arc::new(move |info: &PadInfo| {
        // `emit` i `popisek` jen předají práci hlavnímu vláknu, nečekají
        // (pad vlákno nesmí stát na hlavním — to může čekat na pad).
        let _ = handle.emit(UDALOST, info);
        crate::tray::popisek(&handle, popisek(info));
    });
    let pad = Pad::spust(oznam);
    power::registruj(pad.odesilatel());
    // Fáze 4: stavy z enginu do pad vlákna NE přes `pad.odesilatel()`.
    // Ta fronta bere zámek sdílený s GUI a alokuje — hook callback nesmí
    // ani jedno (princip 3). Hook dostane atomický slot „nejnovější stav"
    // + auto-reset událost, na kterou pad vlákno čeká spolu s frontou
    // (podrobně u `PadPrikaz::Stav`). Opačný směr: pad vlákno při
    // zapnutí pošle hooku `enable()`, při chybě a vypnutí `disable(…)`.
    app.manage(pad);
}

/// Konec aplikace: neutrál → odpojit → zavřít (s časovým limitem).
/// Smí se volat víckrát a z libovolného vlákna (konec relace Windows,
/// `RunEvent::Exit`).
pub fn ukonci(app: &AppHandle) {
    if let Some(pad) = app.try_state::<Pad>() {
        if !pad.ukonci(LIMIT_KONCE) {
            log::warn!(
                "pad se do {} ms neodpojil — odpojí ho ovladač se zavřením procesu",
                LIMIT_KONCE.as_millis()
            );
        }
    }
}

/// Popisek ikony v oznamovací oblasti (Windows ho utne na 127 znaků).
fn popisek(info: &PadInfo) -> String {
    // Bez čísla hráče — ViGEmBus ho s víc pady hlásí špatně (PadInfo::player).
    let stav = match info.state {
        PadStav::On => "ovladač zapnutý",
        PadStav::Connecting => "zapínám ovladač…",
        PadStav::Off => "ovladač vypnutý",
        PadStav::BusMissing => "chybí ViGEmBus",
        PadStav::BusNotRunning => "ViGEmBus neběží",
        PadStav::Error => "chyba ovladače",
    };
    format!("KeyPad — {stav}")
}

fn posli(pad: &Pad, p: PadPrikaz) -> Result<(), String> {
    if pad.posli(p) {
        Ok(())
    } else {
        Err("KeyPad je potřeba spustit znovu.".into())
    }
}

/// Stav padu pro okno (při startu a po návratu okna z oznamovací
/// oblasti). Změny pak chodí událostí `pad-stav`.
#[tauri::command]
pub fn pad_status(pad: tauri::State<'_, Pad>) -> PadInfo {
    pad.status().snapshot()
}

/// Přepínač „zapnout" — jediná cesta, kudy se virtuální ovladač
/// připojuje. Výsledek přijde událostí `pad-stav`.
#[tauri::command]
pub fn pad_on(pad: tauri::State<'_, Pad>) -> Result<(), String> {
    // Okno má přepínač během instalátoru zablokovaný a pad vlákno by
    // zapnutí odmítlo samo — tohle jen dá klikajícímu srozumitelnou větu.
    if pad.status().snapshot().installer || shell::instalator_bezi() {
        return Err("Počkej, až doběhne instalátor ovladače.".into());
    }
    posli(&pad, PadPrikaz::Zapnout)
}

/// Přepínač „vypnout": neutrál → odpojit.
#[tauri::command]
pub fn pad_off(pad: tauri::State<'_, Pad>) -> Result<(), String> {
    posli(&pad, PadPrikaz::Vypnout)
}

/// Zkouška: levá páčka opíše kruh a vrátí se na neutrál.
#[tauri::command]
pub fn pad_test(pad: tauri::State<'_, Pad>) -> Result<(), String> {
    if pad.status().stav() != PadStav::On {
        return Err("Ovladač je vypnutý.".into());
    }
    // Fáze 4: jen v režimu Klávesnice — v režimu Gamepad by se kruh
    // pral s klávesami (pad vlákno ho při stavu z enginu stejně přeruší).
    posli(&pad, PadPrikaz::Test)
}

/// „Zkusit znovu" — sběrnici znovu ověřit (po chybě, po ruční instalaci
/// nebo zapnutí ViGEmBus). Nic nepřipojí.
#[tauri::command]
pub fn pad_retry(pad: tauri::State<'_, Pad>) -> Result<(), String> {
    posli(&pad, PadPrikaz::Znovu)
}

/// Smí se teď spustit `KeyPadSetup /vigembus`? `Err` = proč ne.
///
/// Instalace jen tam, kde ViGEmBus opravdu chybí (`BusMissing` pad
/// vlákno hlásí jen pro `BusState::NotInstalled`), aktualizace jen
/// u staršího ovladače (`needs_update`) — v jakémkoli stavu padu:
/// zapnutý pad se před spuštěním vypne ([`install_vigembus`]). Instalátor
/// to hlídá taky; tohle je druhá pojistka (instalátor ViGEmBus nad cizí
/// instalací škodí).
fn smi_instalovat(info: &PadInfo) -> Result<(), &'static str> {
    if info.installer {
        return Err("Instalátor už běží.");
    }
    match info.state {
        PadStav::BusMissing => Ok(()),
        _ if info.needs_update => Ok(()),
        PadStav::BusNotRunning => Err("ViGEmBus už je nainstalovaný — instalace by nepomohla."),
        _ => Err("ViGEmBus je aktuální."),
    }
}

/// Spustí `KeyPadSetup.exe /vigembus` z instalační složky: instalace
/// chybějícího ViGEmBus, nebo aktualizace staršího.
///
/// Plán → provedení → ověření: nejdřív pad vypnout (neutrál → odpojit
/// → zavřít spojení) a přepínač zablokovat — nový ovladač by zapnutý
/// pad odebral uprostřed hry a otevřené spojení by držel starý ovladač
/// v paměti. Pak instalátor. Po jeho konci (i když se nespustil) pad
/// vlákno blokaci zruší a sběrnici jen ověří; zapne zase uživatel.
#[tauri::command(async)]
pub fn install_vigembus(pad: tauri::State<'_, Pad>) -> Result<(), String> {
    smi_instalovat(&pad.status().snapshot()).map_err(String::from)?;
    let setup = updater::install_dir().join(updater::SETUP_EXE);
    if !pad.simulace() && !setup.is_file() {
        log::warn!(
            "instalátor KeyPadu tu není ({}) — ViGEmBus jen ručně z {}",
            setup.display(),
            updater::vigembus::RELEASES_URL
        );
        // Adresu do okna nedáváme: tlačítko „Stáhnout" otevře pevnou
        // stránku vydání (`open_link`).
        return Err("KeyPad neběží z instalace — ovladač stáhni ručně.".into());
    }
    // Uvolnění zámku (v každém případě, i při chybě níž) = konec
    // blokace přepínače a ověření sběrnice.
    let tx = pad.odesilatel();
    let Some(zamek) = shell::Zamek::zaber(move || {
        let _ = tx.send(PadPrikaz::PoInstalaci);
    }) else {
        return Err("Instalátor už běží.".into());
    };
    if !pad.pred_instalaci(LIMIT_VYPNUTI) {
        log::error!(
            "pad se do {} ms nevypnul — instalátor ViGEmBus se nespouští",
            LIMIT_VYPNUTI.as_millis()
        );
        return Err("Ovladač se nepodařilo vypnout — zkus to znovu.".into());
    }
    if pad.simulace() {
        // Test okna (KEYPAD_BEZ_VIGEM / KEYPAD_VIGEM_STARY): pad se
        // vypne jako naostro, ale skutečný instalátor ovladače se nikdy
        // nespouští.
        log::warn!("simulace ViGEmBus — instalátor se nespouští");
        return Err("Simulace — instalátor se nespouští.".into());
    }
    log::info!(
        "spouštím {} {}",
        setup.display(),
        updater::SETUP_ARG_VIGEMBUS
    );
    shell::spust_a_hlidej(zamek, &setup, updater::SETUP_ARG_VIGEMBUS)
}

/// Adresy, které okno smí otevřít. Okno posílá jen jméno, nikdy adresu
/// — příkaz tak nejde zneužít k otevření čehokoli jiného.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Odkaz {
    /// Repozitář ViGEmBus (credit v „O aplikaci").
    Vigembus,
    /// Vydání ViGEmBus — ruční instalace, když KeyPad neběží z instalace.
    VigembusReleases,
}

impl Odkaz {
    /// Adresy drží `updater::vigembus` — tytéž jako v instalátoru.
    fn adresa(self) -> &'static str {
        match self {
            Odkaz::Vigembus => updater::vigembus::REPO_URL,
            Odkaz::VigembusReleases => updater::vigembus::RELEASES_URL,
        }
    }
}

/// Otevře pevně danou stránku ve výchozím prohlížeči.
#[tauri::command(async)]
pub fn open_link(link: Odkaz) -> Result<(), String> {
    let url = link.adresa();
    log::info!("otevírám {url}");
    shell::otevri_url(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(state: PadStav, player: Option<u8>, needs_update: bool) -> PadInfo {
        PadInfo {
            state,
            player,
            detail: String::new(),
            needs_update,
            installer: false,
            seq: 1,
        }
    }

    #[test]
    fn popisek_ikony() {
        assert_eq!(
            popisek(&info(PadStav::On, Some(2), false)),
            "KeyPad — ovladač zapnutý"
        );
        assert_eq!(
            popisek(&info(PadStav::Off, None, false)),
            "KeyPad — ovladač vypnutý"
        );
        for s in [
            PadStav::Off,
            PadStav::Connecting,
            PadStav::On,
            PadStav::BusMissing,
            PadStav::BusNotRunning,
            PadStav::Error,
        ] {
            // NOTIFYICONDATAW::szTip má 128 znaků včetně nuly.
            assert!(popisek(&info(s, Some(4), false)).encode_utf16().count() < 128);
        }
    }

    /// Instalace jen při chybějícím ViGEmBus, aktualizace jen staršího —
    /// i se zapnutým padem (ten se před spuštěním vypne), ale nikdy
    /// podruhé, dokud instalátor běží.
    #[test]
    fn instalator_jen_kdyz_ma_smysl() {
        for povoleno in [
            info(PadStav::BusMissing, None, false),
            info(PadStav::Off, None, true),
            info(PadStav::Error, None, true),
            info(PadStav::BusNotRunning, None, true),
            info(PadStav::On, Some(1), true),
            info(PadStav::Connecting, None, true),
        ] {
            assert!(smi_instalovat(&povoleno).is_ok(), "{povoleno:?}");
            let bezi = PadInfo {
                installer: true,
                ..povoleno
            };
            assert!(smi_instalovat(&bezi).is_err(), "{bezi:?}");
        }
        for zakazano in [
            info(PadStav::Off, None, false),
            info(PadStav::Error, None, false),
            info(PadStav::BusNotRunning, None, false),
            info(PadStav::Connecting, None, false),
            info(PadStav::On, Some(1), false),
        ] {
            assert!(smi_instalovat(&zakazano).is_err(), "{zakazano:?}");
        }
    }

    /// Okno otevírá jen pevné adresy z `updater::vigembus` (tytéž jako
    /// instalátor) a repozitář sedí se stránkou vydání.
    #[test]
    fn odkazy_jsou_pevne() {
        use updater::vigembus::{RELEASES_URL, REPO_URL};
        assert!(RELEASES_URL.starts_with(REPO_URL));
        let o: Odkaz = serde_json::from_str("\"vigembus\"").unwrap();
        assert_eq!(o.adresa(), REPO_URL);
        assert_eq!(o.adresa(), "https://github.com/nefarius/ViGEmBus");
        let o: Odkaz = serde_json::from_str("\"vigembus_releases\"").unwrap();
        assert_eq!(o.adresa(), updater::vigembus::RELEASES_URL);
        assert!(serde_json::from_str::<Odkaz>("\"https://example.com\"").is_err());
    }
}

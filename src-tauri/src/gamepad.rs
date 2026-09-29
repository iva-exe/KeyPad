//! Virtuální gamepad z pohledu okna: příkazy, událost se stavem padu
//! a popisek ikony v oznamovací oblasti.
//!
//! Samotný pad (ViGEmBus, pad vlákno) je v `platform::windows::pad`;
//! tady je jen lepidlo k Tauri, ať platformní kód na Tauri nezávisí
//! a jde testovat bez okna.

use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::platform::windows::pad::{Oznam, Pad, PadInfo, PadPrikaz, PadStav, UDALOST};
use crate::platform::windows::{power, shell};

/// Jak dlouho při konci aplikace čekat na neutrál + odpojení padu.
/// Normálně milisekundy; když ovladač visí, proces skončí i tak a pad
/// odpojí ovladač sám se zavřením spojení.
const LIMIT_KONCE: Duration = Duration::from_millis(1_500);

/// Spustí pad vlákno a hlídání spánku; stav je pak v `app.state::<Pad>()`.
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
    // Connected pošle hooku `enable()`, při chybě `disable(PadError)`.
    app.manage(pad);
}

/// Konec aplikace: neutrál → odpojit → zavřít (s časovým limitem).
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
    let stav = match (info.state, info.player) {
        (PadStav::Connected, Some(p)) => format!("gamepad připojen (hráč {p})"),
        (PadStav::Connected, None) => "gamepad připojen".into(),
        (PadStav::Connecting, _) => "připojuji gamepad…".into(),
        (PadStav::BusMissing, _) => "chybí ovladač ViGEmBus".into(),
        (PadStav::BusNotRunning, _) => "ovladač ViGEmBus neběží".into(),
        (PadStav::Error, _) => "chyba virtuálního gamepadu".into(),
        (PadStav::Suspended, _) => "gamepad odpojený (spánek)".into(),
    };
    format!("KeyPad — {stav}")
}

/// Stav padu pro okno (při startu a po návratu okna z oznamovací
/// oblasti). Změny pak chodí událostí `pad-stav`.
#[tauri::command]
pub fn pad_status(pad: tauri::State<'_, Pad>) -> PadInfo {
    pad.status().snapshot()
}

/// Zkouška: levá páčka opíše kruh a vrátí se na neutrál.
#[tauri::command]
pub fn pad_test(pad: tauri::State<'_, Pad>) -> Result<(), String> {
    if pad.status().stav() != PadStav::Connected {
        return Err("Virtuální ovladač není připojený.".into());
    }
    // Fáze 4: jen v režimu Klávesnice — v režimu Gamepad by se kruh
    // pral s klávesami (pad vlákno ho při stavu z enginu stejně přeruší).
    if pad.posli(PadPrikaz::Test) {
        Ok(())
    } else {
        Err("pad vlákno neběží — restartuj KeyPad".into())
    }
}

/// „Zkusit znovu" — po chybě, po ruční instalaci nebo zapnutí ViGEmBus
/// a „Připojit znovu" po spánku, když oznámení o probuzení nepřišlo.
#[tauri::command]
pub fn pad_retry(pad: tauri::State<'_, Pad>) -> Result<(), String> {
    if pad.posli(PadPrikaz::Znovu) {
        Ok(())
    } else {
        Err("pad vlákno neběží — restartuj KeyPad".into())
    }
}

/// Spustí `KeyPadSetup.exe /vigembus` z instalační složky.
///
/// Jen když ViGEmBus opravdu chybí (`BusMissing` pad vlákno hlásí jen
/// pro `BusState::NotInstalled`): instalátor ViGEmBus nad existující
/// instalací škodí (odebere cizí uzel zařízení, vynutí restart) —
/// instalátor KeyPadu to hlídá taky, tohle je druhá pojistka.
///
/// Adresa ruční instalace jde do okna jen v textu chyby — okno si ji
/// odtud vezme (tlačítko „kopírovat odkaz"), žádnou vlastní kopii nemá.
#[tauri::command(async)]
pub fn install_vigembus(pad: tauri::State<'_, Pad>) -> Result<(), String> {
    match pad.status().stav() {
        PadStav::BusMissing => {}
        PadStav::BusNotRunning => {
            return Err(
                "ViGEmBus už je nainstalovaný — instalace by nepomohla, postupuj podle rady výš."
                    .into(),
            )
        }
        _ => return Err("ViGEmBus už v systému je — instalace není potřeba.".into()),
    }
    let setup = updater::install_dir().join(updater::SETUP_EXE);
    if !setup.is_file() {
        return Err(format!(
            "Instalátor KeyPadu tu není ({}) — tahle kopie KeyPadu neběží z instalace. \
             ViGEmBus nainstaluj ručně ze stránky {} (soubor {}) a pak klikni na Zkusit znovu.",
            setup.display(),
            updater::vigembus::RELEASES_URL,
            updater::vigembus::SETUP_FILE,
        ));
    }
    log::info!(
        "spouštím {} {}",
        setup.display(),
        updater::SETUP_ARG_VIGEMBUS
    );
    let tx = pad.odesilatel();
    shell::spust_a_hlidej(&setup, updater::SETUP_ARG_VIGEMBUS, move |kod| {
        match kod {
            Some(k) => log::info!("instalátor ViGEmBus skončil (kód {k}) — ověřuji ovladač"),
            None => log::info!("instalátor ViGEmBus skončil — ověřuji ovladač"),
        }
        // Ověření: pad se zkusí připojit. Povede se jen s běžícím
        // ovladačem, takže okno ukáže skutečný stav, ne slib instalátoru.
        let _ = tx.send(PadPrikaz::Znovu);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popisek_ikony() {
        let info = |state, player| PadInfo {
            state,
            player,
            detail: String::new(),
            seq: 1,
        };
        assert_eq!(
            popisek(&info(PadStav::Connected, Some(2))),
            "KeyPad — gamepad připojen (hráč 2)"
        );
        for s in [
            PadStav::Connecting,
            PadStav::BusMissing,
            PadStav::BusNotRunning,
            PadStav::Error,
            PadStav::Suspended,
        ] {
            // NOTIFYICONDATAW::szTip má 128 znaků včetně nuly.
            assert!(popisek(&info(s, None)).encode_utf16().count() < 128);
        }
    }
}

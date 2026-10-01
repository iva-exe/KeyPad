//! Virtuální ovladače z pohledu okna: příkazy, události se stavem padů
//! a režimem, popisek ikony v oznamovací oblasti — a lepidlo mezi pad
//! vlákny a hookem klávesnice (Fáze 4).
//!
//! Samotné pady (ViGEmBus, pad vlákna) jsou v `platform::windows::pad`,
//! hook v `platform::windows::hook`; tady je jen spojení s Tauri, ať
//! platformní kód na Tauri nezávisí a jde testovat bez okna.
//!
//! Fáze 2b: ovladač se připojuje JEN přepínačem v okně (`pad_on`) —
//! nikdy sám po startu, po probuzení ani po aktualizaci. Fáze 4: jakmile
//! se zapnutý ovladač ohlásí jako připojený, engine ho povolí a začne
//! zachytávat jeho klávesy (přepnutí přepínače JE povel „hrát");
//! zkratka (Scroll Lock) zachytávání jen pozastaví. Hook klávesnice je
//! v systému, jen když je zapnutý aspoň jeden ovladač.
//!
//! Fáze 4b: vlákno okna při změně režimu navíc pípne a přepne ikonu
//! v oznamovací oblasti (pravidla v [`znameni`]) — okno je během hry
//! schované a pozastavení by jinak nešlo poznat.

mod znameni;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use keypad_core::{DisabledReason, ForceReason, Mapping, PadId, MAX_PADS};
use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::platform::windows::hook::{Hook, HookOdesilatel, HookPrikaz, Vystup, WATCHDOG_MS};
use crate::platform::windows::pad::{Oznam, PadInfo, PadPrikaz, PadStav, Pady, UDALOST};
use crate::platform::windows::slot::Budik;
use crate::platform::windows::vystup::{HookVystup, Rezim, RezimInfo};
use crate::platform::windows::{klavesy, power, shell, zvuk};
use crate::tray::TrayStav;

/// Tauri událost se změnou režimu (payload [`RezimInfo`]).
pub const UDALOST_REZIM: &str = "rezim";

/// ✓ Zvuk v nabídce ikony po startu. Uložení volby do konfigurace
/// přinese Fáze 6/7; do té doby platí jen do konce běhu.
pub const ZVUK_VYCHOZI: bool = true;

/// Jak často vlákno okna během hraní kontroluje tep ovladačů (Fáze 5).
/// Callback hooku to kontroluje u každé klávesy; tohle pokryje chvíli,
/// kdy hráč nic nemačká. Mimo hraní se netiká (princip 10).
const PULS_MS: u64 = 500;

/// Jak dlouho při konci aplikace čekat na neutrál + odpojení padů.
/// Normálně milisekundy; když ovladač visí, proces skončí i tak a pady
/// odpojí ovladač sám se zavřením spojení.
const LIMIT_KONCE: Duration = Duration::from_millis(1_500);

/// Jak dlouho před spuštěním instalátoru ViGEmBus čekat, než pad
/// vlákna ovladače vypnou. Normálně milisekundy; zdrží je jen rozjeté
/// zapínání (`wait_ready`). Bez potvrzení se instalátor nespustí.
const LIMIT_VYPNUTI: Duration = Duration::from_secs(3);

/// Všechno kolem virtuálních ovladačů, co potřebují příkazy okna.
pub struct Ovladani {
    pady: Arc<Pady>,
    /// Hook vlákno; `None` po konci aplikace.
    hook: Mutex<Option<Hook>>,
    vystup: Arc<HookVystup>,
    /// Konec běží celý najednou: konec relace Windows a `RunEvent::Exit`
    /// ho můžou spustit souběžně a druhý by jinak odpojil pady dřív,
    /// než první odebere hook (pořadí neutrál → odhooknout → odpojit).
    konec: Mutex<()>,
    /// Příkazy hooku z nabídky ikony — bez zámku `hook`, který drží konec.
    prikazy: HookOdesilatel,
    /// ✓ Zvuk v nabídce ikony.
    zvuk: Arc<AtomicBool>,
    /// Aplikace končí: vlákno okna už nepípá (konec vynutí Klávesnici
    /// a to by jinak znělo jako pozastavení).
    konci: Arc<AtomicBool>,
}

/// Spustí pad vlákno prvního ovladače (jen ověří sběrnici — nic
/// nepřipojí), hlídání spánku a hook vlákno (hook sám se nainstaluje až
/// se zapnutým ovladačem). Stav je pak v `app.state::<Ovladani>()`.
pub fn spust(app: &tauri::App) -> Result<(), String> {
    // Budík vlákna okna dřív než pady: budí ho i jejich oznámení (ikona
    // a bublina se počítají ze stavu všech ovladačů).
    let budik = Arc::new(Budik::new()?);
    // Pad vlákna vznikají dřív než hook a hlásí se hned — odesílatel
    // hooku se do jejich oznámení doplní, jakmile hook běží. Oznámení
    // před tím (první ověření sběrnice) engine nepotřebuje: startuje
    // bez povolených ovladačů.
    let hook_tx: Arc<OnceLock<HookOdesilatel>> = Arc::default();
    let predchozi: Arc<[AtomicU8; MAX_PADS]> = Arc::new(std::array::from_fn(|_| AtomicU8::new(0)));
    let handle = app.handle().clone();
    let tx = Arc::clone(&hook_tx);
    let budik_padu = Arc::clone(&budik);
    let oznam: Oznam = Arc::new(move |info: &PadInfo| {
        // `emit` jen předá práci hlavnímu vláknu, nečeká (pad vlákno
        // nesmí stát na hlavním — to může čekat na pad).
        let _ = handle.emit(UDALOST, info);
        // Ikonu přepočítá vlákno okna; tady jen `SetEvent`.
        budik_padu.probud();
        let Some(pad) = PadId::new(usize::from(info.pad)) else {
            return;
        };
        let pred = predchozi[pad.index()].swap(kod_stavu(info.state), Ordering::AcqRel);
        if let Some(h) = tx.get() {
            for p in prikazy_hooku(pad, stav_z_kodu(pred), info.state) {
                h.posli(p);
            }
        }
    });
    let pady = Arc::new(Pady::spust(oznam)?);
    power::registruj(Arc::clone(&pady) as Arc<dyn power::Napajeni>);

    let vystup = Arc::new(HookVystup::new(pady.sloty(), Arc::clone(&budik)));
    // Rozložení zatím výchozí; vlastní klávesy přinese Fáze 6/7.
    let mapovani = Mapping::default();
    // Název zkratky pro bublinu ikony — tady, na hlavním vlákně, podle
    // rozložení, se kterým aplikace startovala.
    let zkratka = klavesy::nazev(mapovani.toggle_key());
    let hook = Hook::spust(mapovani, Arc::clone(&vystup) as Arc<dyn Vystup>)?;
    let _ = hook_tx.set(hook.odesilatel());

    // Zamčení relace (Fáze 5): key-upy kláves držených přes zámek hook
    // neuvidí — zapomenout je a vynutit Klávesnici. Po odemčení
    // zachytávání vrátí zkratka.
    let tx_relace = hook.odesilatel();
    // Kdy se relace naposledy zamkla — vlákno okna podle toho ztiší tón
    // k přepnutí plochy, které zamčení předchází (znameni::Znameni).
    let zamek = Arc::new(AtomicU64::new(0));
    let zamek_relace = Arc::clone(&zamek);
    crate::platform::windows::relace::pri_zamceni(move |duvod| {
        zamek_relace.store(ted_ms().max(1), Ordering::Release);
        log::info!("relace: {duvod} — Klávesnice, držené klávesy zapomenuty");
        tx_relace.posli(HookPrikaz::Zapomen(ForceReason::SessionLock));
    });

    // Změna režimu → okno, ikona a zvuk. Vlastní vlákno, protože hook
    // callback smí jen nastavit událost (princip 3); tohle vlákno spí,
    // dokud nepřijde — jen během hraní se budí i samo a hlídá tep
    // ovladačů (watchdog).
    let zvuk = Arc::new(AtomicBool::new(ZVUK_VYCHOZI));
    let konci = Arc::new(AtomicBool::new(false));
    let pocatek = vystup.info();
    let okno = VlaknoOkna {
        app: app.handle().clone(),
        seq: pocatek.seq,
        znameni: znameni::Znameni::new(pocatek.rezim),
        vystup: Arc::clone(&vystup),
        pady: Arc::clone(&pady),
        hook: hook.odesilatel(),
        budik,
        zkratka,
        zvuk: Arc::clone(&zvuk),
        konci: Arc::clone(&konci),
        zamek,
        oblast: None,
    };
    std::thread::Builder::new()
        .name("keypad-okno".into())
        .spawn(move || okno.smycka())
        .map_err(|e| format!("vlákno okna nejde spustit: {e}"))?;

    app.manage(Ovladani {
        pady,
        prikazy: hook.odesilatel(),
        hook: Mutex::new(Some(hook)),
        vystup,
        konec: Mutex::new(()),
        zvuk,
        konci,
    });
    Ok(())
}

/// Vlákno okna (`keypad-okno`): režim do okna, ikona, zvuk a watchdog
/// během hraní. Nikdy nečeká na nic jiného než na budík — ikonu
/// předává hlavnímu vláknu a zvuk vláknu zvuku.
struct VlaknoOkna {
    app: AppHandle,
    vystup: Arc<HookVystup>,
    pady: Arc<Pady>,
    hook: HookOdesilatel,
    budik: Arc<Budik>,
    /// Název zkratky pozastavení pro bublinu.
    zkratka: String,
    zvuk: Arc<AtomicBool>,
    konci: Arc<AtomicBool>,
    /// Kdy se relace naposledy zamkla (`GetTickCount64`, 0 = zatím ne).
    zamek: Arc<AtomicU64>,
    /// Číslo naposledy ohlášené změny režimu.
    seq: u64,
    /// Co a kdy zapípat — čisté rozhodování, testuje se bez vlákna.
    znameni: znameni::Znameni,
    /// Naposledy předaný stav ikony — ikona se mění jen při změně.
    oblast: Option<TrayStav>,
}

impl VlaknoOkna {
    fn smycka(mut self) {
        // První obrátka hned: ikona podle výchozího stavu (šedá „vypnuto“).
        // Panika (zapíše ji panic hook) nesmí vlákno zastavit — hlídá tep
        // ovladačů během hry (princip 1).
        let _ = catch_unwind(AssertUnwindSafe(|| self.obratka()));
        loop {
            // Mimo hru a bez tónu čekajícího na odklad spí bez limitu
            // (princip 10).
            let hraje = self.vystup.info().rezim == Rezim::Capturing;
            let limit = [hraje.then_some(PULS_MS), self.znameni.probudit_za(ted_ms())]
                .into_iter()
                .flatten()
                .min();
            self.budik.cekej(limit);
            let _ = catch_unwind(AssertUnwindSafe(|| self.obratka()));
        }
    }

    fn obratka(&mut self) {
        let (r, pricina) = self.vystup.info_s_pricinou();
        let stavy: [Option<(PadStav, u64)>; MAX_PADS] =
            std::array::from_fn(|i| self.pady.stav_a_tep(i));
        let pady = stavy.map(|s| s.map(|(stav, _)| stav));
        let mut zmena = None;
        if r.seq != self.seq {
            log::info!("režim: {:?} ({pricina:?})", r.rezim);
            self.seq = r.seq;
            let _ = self.app.emit(UDALOST_REZIM, r);
            zmena = Some((r.rezim, pricina));
        }
        let smi = znameni::smi_znit(
            self.zvuk.load(Ordering::Acquire),
            self.konci.load(Ordering::Acquire),
            self.pady.simulace(),
        );
        let zamek = self.zamek.load(Ordering::Acquire);
        if let Some(z) = self.znameni.obratka(zmena, ted_ms(), zamek, smi) {
            zvuk::prehraj(z);
        }
        let oblast = znameni::stav_oblasti(r.rezim, &pady, &self.zkratka);
        if self.oblast.as_ref() != Some(&oblast) {
            crate::tray::stav(&self.app, oblast.clone());
            self.oblast = Some(oblast);
        }
        if r.rezim == Rezim::Capturing {
            if let Some(i) = zaseknuty_pad(|i| stavy[i], ted_ms()) {
                log::warn!(
                    "ovladač {} přes {WATCHDOG_MS} ms nemluví s ViGEmBus — Klávesnice",
                    i + 1
                );
                self.hook.posli(HookPrikaz::Vynut(ForceReason::Watchdog));
            }
        }
    }
}

/// „Pozastavit / Pokračovat“ z nabídky ikony.
///
/// Pozastavit = přepnout, jako Scroll Lock. Pokračovat = jako zapnutí
/// přepínačem (`Zachytavej`): z pozastavení se hook nejdřív
/// přeinstaluje — Windows ho mohli potichu odebrat a hra by jinak jen
/// předstírala, že klávesy hrají (princip 8). Scroll Lock tohle
/// nepotřebuje: jeho stisk sám dokazuje, že hook žije. A když se režim
/// mezitím změnil (Scroll Lock s otevřenou nabídkou), `Zachytavej`
/// hru nepozastaví, jen nic neudělá.
pub fn prepni_z_nabidky(app: &AppHandle) {
    let Some(o) = app.try_state::<Ovladani>() else {
        return;
    };
    let (prikaz, co) = match o.vystup.info().rezim {
        Rezim::Capturing => (HookPrikaz::Prepni, "pozastavit"),
        Rezim::Paused => (HookPrikaz::Zachytavej, "pokračovat"),
        // Položka je mimo hru a pauzu zakázaná; tohle je jen souběh.
        _ => return,
    };
    log::info!("nabídka ikony: {co}");
    o.prikazy.posli(prikaz);
}

/// „✓ Zvuk“ z nabídky ikony.
pub fn zvuk_z_nabidky(app: &AppHandle, zapnuto: bool) {
    if let Some(o) = app.try_state::<Ovladani>() {
        o.zvuk.store(zapnuto, Ordering::Release);
        log::info!("zvuk {}", if zapnuto { "zapnutý" } else { "vypnutý" });
    }
}

/// Zapnutý ovladač, jehož pad vlákno přes [`WATCHDOG_MS`] nemluvilo
/// s ViGEmBus (první takový). Čistá funkce kvůli testu.
fn zaseknuty_pad(stav_a_tep: impl Fn(usize) -> Option<(PadStav, u64)>, ted: u64) -> Option<usize> {
    (0..MAX_PADS).find(|&i| {
        stav_a_tep(i)
            .is_some_and(|(s, tep)| s == PadStav::On && ted.saturating_sub(tep) > WATCHDOG_MS)
    })
}

fn ted_ms() -> u64 {
    // SAFETY: bez parametrů, jen čte čítač.
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

/// Co má hook udělat, když se stav ovladače změnil.
///
/// Připojený ovladač (přechod na `On`) engine povolí a začne zachytávat
/// — ovladač se připojuje jen přepínačem, takže tohle je vždy povel
/// uživatele. Cokoli jiného ovladač v enginu zakáže: jeho klávesy jdou
/// zase do Windows a držené se spolknou (OS jejich stisk neviděl).
/// Stejný stav znovu (nová podrobnost, ohlášení po instalátoru) nic
/// nemění — hlavně nesmí znovu spustit zachytávání pozastavené zkratkou.
fn prikazy_hooku(pad: PadId, pred: Option<PadStav>, ted: PadStav) -> Vec<HookPrikaz> {
    if pred == Some(ted) {
        return Vec::new();
    }
    match ted {
        PadStav::On => vec![HookPrikaz::Povol(pad), HookPrikaz::Zachytavej],
        PadStav::BusMissing => vec![HookPrikaz::Zakaz(pad, DisabledReason::ViGEmMissing)],
        PadStav::Error => vec![HookPrikaz::Zakaz(pad, DisabledReason::PadError)],
        PadStav::Off | PadStav::Connecting | PadStav::BusNotRunning => {
            vec![HookPrikaz::Zakaz(pad, DisabledReason::PadNotConnected)]
        }
    }
}

/// Poslední ohlášený stav ovladače v atomiku (0 = zatím nic).
fn kod_stavu(s: PadStav) -> u8 {
    match s {
        PadStav::Off => 1,
        PadStav::Connecting => 2,
        PadStav::On => 3,
        PadStav::BusMissing => 4,
        PadStav::BusNotRunning => 5,
        PadStav::Error => 6,
    }
}

fn stav_z_kodu(k: u8) -> Option<PadStav> {
    Some(match k {
        1 => PadStav::Off,
        2 => PadStav::Connecting,
        3 => PadStav::On,
        4 => PadStav::BusMissing,
        5 => PadStav::BusNotRunning,
        6 => PadStav::Error,
        _ => return None,
    })
}

/// Konec aplikace: hook pryč (engine předtím pošle neutrál), pak pady
/// neutrál → odpojit → zavřít (s časovým limitem). Smí se volat víckrát
/// a z libovolného vlákna (konec relace Windows, `RunEvent::Exit`).
pub fn ukonci(app: &AppHandle) {
    let Some(o) = app.try_state::<Ovladani>() else {
        return;
    };
    // První, ještě před vynucenou Klávesnicí: konec nepípá.
    o.konci.store(true, Ordering::Release);
    let _konec = o.konec.lock().unwrap_or_else(|e| e.into_inner());
    let hook = o.hook.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(mut h) = hook {
        if !h.zastav() {
            log::warn!("hook se nezastavil včas — zmizí s procesem");
        }
    }
    if !o.pady.ukonci(LIMIT_KONCE) {
        log::warn!(
            "ovladače se do {} ms neodpojily — odpojí je ViGEmBus se zavřením procesu",
            LIMIT_KONCE.as_millis()
        );
    }
}

/// Číslo ovladače z okna (bez čísla = první).
fn cislo(pad: Option<u8>) -> Result<usize, String> {
    let i = usize::from(pad.unwrap_or(0));
    if i < MAX_PADS {
        Ok(i)
    } else {
        Err("Takový ovladač není.".into())
    }
}

fn posli(o: &Ovladani, pad: Option<u8>, p: PadPrikaz) -> Result<(), String> {
    if o.pady.posli(cislo(pad)?, p) {
        Ok(())
    } else {
        Err("KeyPad je potřeba spustit znovu.".into())
    }
}

/// Stav ovladače pro okno (při startu a po návratu okna z oznamovací
/// oblasti). Změny pak chodí událostí `pad-stav`.
#[tauri::command]
pub fn pad_status(o: tauri::State<'_, Ovladani>, pad: Option<u8>) -> Result<PadInfo, String> {
    o.pady
        .stav(cislo(pad)?)
        .ok_or_else(|| "Takový ovladač není.".into())
}

/// Režim zachytávání pro okno (změny chodí událostí `rezim`; starší
/// z obojího okno pozná podle `seq`).
#[tauri::command]
pub fn rezim(o: tauri::State<'_, Ovladani>) -> RezimInfo {
    o.vystup.info()
}

/// Přepínač „zapnout" — jediná cesta, kudy se virtuální ovladač
/// připojuje. Výsledek přijde událostí `pad-stav`; po připojení engine
/// začne zachytávat klávesy ovladače.
#[tauri::command]
pub fn pad_on(o: tauri::State<'_, Ovladani>, pad: Option<u8>) -> Result<(), String> {
    // Okno má přepínač během instalátoru zablokovaný a pad vlákno by
    // zapnutí odmítlo samo — tohle jen dá klikajícímu srozumitelnou větu.
    let i = cislo(pad)?;
    if o.pady.stav(i).is_some_and(|s| s.installer) || shell::instalator_bezi() {
        return Err("Počkej, až doběhne instalátor ovladače.".into());
    }
    posli(&o, pad, PadPrikaz::Zapnout)
}

/// Přepínač „vypnout": neutrál → odpojit. Engine se to dozví ze stavu
/// padu (klávesy ovladače jdou zase do Windows).
#[tauri::command]
pub fn pad_off(o: tauri::State<'_, Ovladani>, pad: Option<u8>) -> Result<(), String> {
    posli(&o, pad, PadPrikaz::Vypnout)
}

/// Zkouška: levá páčka opíše kruh a vrátí se na neutrál. Stisk klávesy
/// ovladače ji přeruší (vstup z klávesnice má přednost).
#[tauri::command]
pub fn pad_test(o: tauri::State<'_, Ovladani>, pad: Option<u8>) -> Result<(), String> {
    let i = cislo(pad)?;
    if o.pady.stav(i).map(|s| s.state) != Some(PadStav::On) {
        return Err("Ovladač je vypnutý.".into());
    }
    posli(&o, pad, PadPrikaz::Test)
}

/// „Zkusit znovu" — sběrnici znovu ověřit (po chybě, po ruční instalaci
/// nebo zapnutí ViGEmBus) ve všech ovladačích. Nic nepřipojí.
#[tauri::command]
pub fn pad_retry(o: tauri::State<'_, Ovladani>) {
    o.pady.vsem(|| PadPrikaz::Znovu);
}

/// Smí se teď spustit `KeyPadSetup /vigembus`? `Err` = proč ne.
///
/// Instalace jen tam, kde ViGEmBus opravdu chybí (`BusMissing` pad
/// vlákno hlásí jen pro `BusState::NotInstalled`), aktualizace jen
/// u staršího ovladače (`needs_update`) — v jakémkoli stavu padu:
/// zapnuté pady se před spuštěním vypnou ([`install_vigembus`]).
/// Instalátor to hlídá taky; tohle je druhá pojistka (instalátor
/// ViGEmBus nad cizí instalací škodí). Stav sběrnice hlásí první
/// ovladač — jeho vlákno běží vždy.
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
/// Plán → provedení → ověření: nejdřív všechny pady vypnout (neutrál →
/// odpojit → zavřít spojení) a přepínače zablokovat — nový ovladač by
/// zapnutý pad odebral uprostřed hry a otevřené spojení by drželo starý
/// ovladač v paměti. Pak instalátor. Po jeho konci (i když se nespustil)
/// pad vlákna blokaci zruší a sběrnici jen ověří; zapne zase uživatel.
#[tauri::command(async)]
pub fn install_vigembus(o: tauri::State<'_, Ovladani>) -> Result<(), String> {
    let info = o.pady.stav(0).ok_or("KeyPad je potřeba spustit znovu.")?;
    smi_instalovat(&info).map_err(String::from)?;
    let setup = updater::install_dir().join(updater::SETUP_EXE);
    if !o.pady.simulace() && !setup.is_file() {
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
    // blokace přepínačů a ověření sběrnice.
    let pady = Arc::clone(&o.pady);
    let Some(zamek) = shell::Zamek::zaber(move || pady.po_instalaci()) else {
        return Err("Instalátor už běží.".into());
    };
    if !o.pady.pred_instalaci(LIMIT_VYPNUTI) {
        log::error!(
            "ovladače se do {} ms nevypnuly — instalátor ViGEmBus se nespouští",
            LIMIT_VYPNUTI.as_millis()
        );
        return Err("Ovladač se nepodařilo vypnout — zkus to znovu.".into());
    }
    if o.pady.simulace() {
        // Test okna (KEYPAD_BEZ_VIGEM / KEYPAD_VIGEM_STARY): pady se
        // vypnou jako naostro, ale skutečný instalátor ovladače se nikdy
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
            pad: 0,
            state,
            player,
            detail: String::new(),
            needs_update,
            installer: false,
            seq: 1,
        }
    }

    const VSECHNY: [PadStav; 6] = [
        PadStav::Off,
        PadStav::Connecting,
        PadStav::On,
        PadStav::BusMissing,
        PadStav::BusNotRunning,
        PadStav::Error,
    ];

    /// Zachytávání spouští jen PŘECHOD na „zapnuto" (povel přepínače);
    /// opakované ohlášení téhož stavu nesmí obnovit zachytávání, které
    /// uživatel pozastavil zkratkou. Každý jiný stav ovladač zakáže.
    #[test]
    fn hook_jen_na_prechod_stavu() {
        let p = PadId::new(1).unwrap();
        for s in VSECHNY {
            assert!(prikazy_hooku(p, Some(s), s).is_empty(), "{s:?}");
        }
        for pred in [None, Some(PadStav::Off), Some(PadStav::Connecting)] {
            let v = prikazy_hooku(p, pred, PadStav::On);
            assert!(
                matches!(v[..], [HookPrikaz::Povol(x), HookPrikaz::Zachytavej] if x == p),
                "{v:?}"
            );
        }
        let v = prikazy_hooku(p, Some(PadStav::On), PadStav::Error);
        assert!(matches!(
            v[..],
            [HookPrikaz::Zakaz(x, DisabledReason::PadError)] if x == p
        ));
        let v = prikazy_hooku(p, Some(PadStav::Off), PadStav::BusMissing);
        assert!(matches!(
            v[..],
            [HookPrikaz::Zakaz(_, DisabledReason::ViGEmMissing)]
        ));
        for s in [PadStav::Off, PadStav::Connecting, PadStav::BusNotRunning] {
            let v = prikazy_hooku(p, Some(PadStav::On), s);
            assert!(matches!(
                v[..],
                [HookPrikaz::Zakaz(_, DisabledReason::PadNotConnected)]
            ));
        }
        for s in VSECHNY {
            assert_eq!(stav_z_kodu(kod_stavu(s)), Some(s));
        }
        assert_eq!(stav_z_kodu(0), None);
    }

    /// Watchdog okna: jen zapnutý ovladač se starým tepem; vypnutý ani
    /// čerstvý ne.
    #[test]
    fn watchdog_okna() {
        let stavy = [
            Some(PadStav::Off),
            Some(PadStav::On),
            Some(PadStav::On),
            None,
        ];
        let tepy = [0, 9_000, 10_000, 0];
        let st = |i: usize| stavy[i].map(|s| (s, tepy[i]));
        assert_eq!(zaseknuty_pad(st, 10_000), None);
        assert_eq!(zaseknuty_pad(st, 10_000 + WATCHDOG_MS), Some(1));
        assert_eq!(zaseknuty_pad(st, 12_000), Some(1));
        assert_eq!(zaseknuty_pad(|_| Some((PadStav::Off, 0)), u64::MAX), None);
    }

    #[test]
    fn cislo_ovladace_z_okna() {
        assert_eq!(cislo(None), Ok(0));
        assert_eq!(cislo(Some(3)), Ok(3));
        assert!(cislo(Some(4)).is_err());
        assert!(cislo(Some(255)).is_err());
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

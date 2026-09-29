//! Pad vlákno — jediné místo, které mluví s ViGEmBus.
//!
//! Pravidla (revize Fáze 2, měřeno na ovladači 1.21.442):
//!
//! - **Start:** spojení → připojit target → PRÁVĚ JEDNOU `wait_ready`.
//!   Chyba 483 (první připojení na novém PC, Windows teprve instalují
//!   zařízení) není porucha: slot XInput se pak zkouší každých 50 ms až
//!   10 s a okno ukazuje „připojuji…". 650 i po 10 s = už jsou připojené
//!   4 ovladače. Pak neutrál a teprve po jeho potvrzení „připojeno".
//! - **Nikdy** dvakrát `wait_ready` na jeden target a nikdy odpojení
//!   dřív, než se `wait_ready` vrátil (chyba v ovladači). Obojí tu drží
//!   to, že všechno běží v jednom vlákně a `wait_ready` je synchronní.
//! - **Smyčka:** z fronty vždy jen NEJNOVĚJŠÍ stav; každých 200 ms se
//!   stav pošle znovu (ověří, že pad žije) a zapíše se heartbeat.
//! - **170 (ovladač report zahodil):** posílat znovu (~1 ms) — vždy ten
//!   nejnovější stav. Po 250 ms v kuse → chyba. 55 a cokoli jiného →
//!   chyba hned. Při chybě se pad odpojí: zmizelý ovladač nemůže držet
//!   vychýlenou páčku (princip 1).
//! - **Spánek:** před uspáním neutrál → odpojit → zavřít spojení,
//!   po probuzení znovu jako při startu (ViGEmBus BSOD #160). Když
//!   oznámení o probuzení nepřijde, připojí pad ruční „Připojit znovu".
//! - **ViGEmBus není:** instalace se nabízí, jen když v systému opravdu
//!   chybí; nainstalovaný, ale neběžící ovladač dostane radu
//!   z `updater::vigembus` (`BusNotRunning`).
//! - **Konec:** neutrál (i s opakováním) → odpojit → zavřít.
//!
//! Logika smyčky ([`Smycka`]) stojí za malým rozhraním [`Backend`]
//! (ovladač + hodiny), takže jde otestovat s falešným ovladačem bez
//! vláken a bez čekání.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use keypad_core::{PadState, AXIS_MAX};
use serde::Serialize;
use updater::vigembus::{BusState, DeviceStatus};

use super::vigem::{Bus, VigemError, XusbReport, X360};

/// Tauri událost se stavem padu (payload [`PadInfo`]).
pub const UDALOST: &str = "pad-stav";

/// Jak často se nezměněný stav posílá znovu. Stojí 6–8 µs a ověří, že
/// pad pořád existuje; zároveň je to takt heartbeatu (watchdog, Fáze 5).
const KEEPALIVE_MS: u64 = 200;
/// Po jak dlouhé souvislé řadě 170 (report zahozen) to vzdát.
const BUSY_LIMIT_MS: u64 = 250;
/// Rozestup opakování po 170. Windows čekání zaokrouhlí na takt
/// systémového časovače (bez hry až 15,6 ms) — pořád ~16 pokusů do
/// limitu. Zjemňovat takt (timeBeginPeriod) kvůli tomu nebudeme:
/// zdražil by chod celého systému (princip 10).
const BUSY_KROK_MS: u64 = 1;
/// Rozestup dotazů na slot XInput při připojování.
const SLOT_KROK_MS: u64 = 50;
/// Jak dlouho čekat na slot XInput (první instalace zařízení trvá déle).
const SLOT_LIMIT_MS: u64 = 10_000;
/// Délka zkoušky páčky (jedno otočení).
const TEST_MS: u64 = 1_200;
/// Takt zkoušky páčky.
const TEST_KROK_MS: u64 = 10;

/// Stav virtuálního padu, jak ho vidí okno.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PadStav {
    /// Připojuje se (start, „Zkusit znovu", probuzení).
    Connecting,
    /// Target existuje, má slot XInput a přijal neutrál.
    Connected,
    /// Ovladač ViGEmBus v systému vůbec NENÍ — jediný stav, kde okno
    /// nabídne instalaci. Aplikace běží dál.
    BusMissing,
    /// ViGEmBus je nainstalovaný, ale neběží (vypnutý ve Správci
    /// zařízení, čeká na restart, zablokovaný). Instalace by nepomohla;
    /// `detail` je rada z `updater::vigembus` (tatáž jako v instalátoru).
    BusNotRunning,
    /// Porucha — čeká se na „Zkusit znovu".
    Error,
    /// Počítač se uspává, pad je dočasně odpojený.
    Suspended,
}

impl PadStav {
    fn z_u8(v: u8) -> PadStav {
        match v {
            1 => PadStav::Connected,
            2 => PadStav::BusMissing,
            3 => PadStav::Error,
            4 => PadStav::Suspended,
            5 => PadStav::BusNotRunning,
            _ => PadStav::Connecting,
        }
    }

    fn jako_u8(self) -> u8 {
        match self {
            PadStav::Connecting => 0,
            PadStav::Connected => 1,
            PadStav::BusMissing => 2,
            PadStav::Error => 3,
            PadStav::Suspended => 4,
            PadStav::BusNotRunning => 5,
        }
    }
}

/// Stav padu pro okno (příkaz `pad_status` i událost [`UDALOST`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PadInfo {
    pub state: PadStav,
    /// Číslo hráče (slot XInput + 1), jen když je pad připojený.
    pub player: Option<u8>,
    /// Podrobnost česky (proč chyba, co se právě děje); může být prázdná.
    pub detail: String,
    /// Pořadové číslo změny — okno podle něj pozná starou odpověď
    /// `pad_status`, která dorazila až po novější události.
    pub seq: u64,
}

/// Stav padu sdílený s ostatními vlákny.
///
/// Atomiky čte kdokoli bez čekání — i hook (Fáze 3+) a jeho watchdog
/// (Fáze 5). Celý stav pro okno (`info`) je za zámkem, který bere jen
/// pad vlákno (zápis) a GUI (`pad_status`); hook ho NIKDY nezamyká
/// (princip 3) a čte jen atomik `stav`.
pub struct PadStatus {
    stav: AtomicU8,
    heartbeat_ms: AtomicU64,
    /// Stav, hráč, podrobnost i `seq` se zapisují a čtou NAJEDNOU.
    /// Po jednotlivých atomikách vracel `pad_status` roztržené snímky
    /// (revize: nový `seq` se starou podrobností, 117 tisíc ze 17 milionů
    /// pod zátěží) — a okno podle `seq` věří, že snímek je celý.
    info: Mutex<PadInfo>,
}

impl PadStatus {
    fn new() -> PadStatus {
        PadStatus {
            stav: AtomicU8::new(PadStav::Connecting.jako_u8()),
            heartbeat_ms: AtomicU64::new(0),
            info: Mutex::new(PadInfo {
                state: PadStav::Connecting,
                player: None,
                detail: String::new(),
                seq: 0,
            }),
        }
    }

    pub fn stav(&self) -> PadStav {
        PadStav::z_u8(self.stav.load(Ordering::Acquire))
    }

    /// Kdy pad vlákno naposledy úspěšně mluvilo s ovladačem
    /// (`GetTickCount64`, ms). Watchdog Fáze 5: v režimu Gamepad starší
    /// než 1 s → vynutit Klávesnici.
    #[cfg_attr(not(test), expect(dead_code, reason = "watchdog padu ve Fázi 5"))]
    pub fn heartbeat_ms(&self) -> u64 {
        self.heartbeat_ms.load(Ordering::Acquire)
    }

    /// Celý stav pro okno — vždy jedna ucelená změna. Stejný `seq`
    /// tedy znamená stejný obsah a okno může odpověď se stejným `seq`
    /// jako už převzatá událost klidně vzít znovu.
    pub fn snapshot(&self) -> PadInfo {
        self.info.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn zapis(&self, info: &PadInfo) {
        let nove = info.clone();
        let mut g = self.info.lock().unwrap_or_else(|e| e.into_inner());
        *g = nove;
        // Atomik ještě pod zámkem: kdo má snímek, nenajde pak v `stav()`
        // starší stav, než jaký snímek ukazuje.
        self.stav.store(info.state.jako_u8(), Ordering::Release);
    }

    fn tep(&self, ted_ms: u64) {
        self.heartbeat_ms.store(ted_ms, Ordering::Release);
    }
}

/// Příkazy pro pad vlákno (GUI, uspání, instalátor ViGEmBus).
#[derive(Debug)]
pub enum PadPrikaz {
    /// Nový stav od enginu. Platí jen nejnovější.
    ///
    /// POZOR, Fáze 4: hook callback touhle frontou posílat NESMÍ
    /// (princip 3). Je to neomezený kanál crossbeamu — `send` do
    /// čekajícího pad vlákna bere `std::sync::Mutex` budíku, který sdílí
    /// s GUI (`pad_test`, `pad_retry`) i s vláknem napájení, a každých
    /// 31 zpráv alokuje nový blok. Hook dostane vlastní cestu bez zámku:
    /// atomický slot „nejnovější stav" (hook jen přepíše) + auto-reset
    /// událost, na kterou pad vlákno čeká spolu s touhle frontou
    /// (`SetEvent` nic nesdílí s GUI). Tahle varianta zůstane pro testy
    /// a pro cestu mimo hook.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "stavy padu z enginu posílá hook ve Fázi 4")
    )]
    Stav(PadState),
    /// Zkouška: levá páčka opíše kruh (~1,2 s) a vrátí se na neutrál.
    Test,
    /// Znovu připojit po chybě / po instalaci ViGEmBus.
    Znovu,
    /// Počítač se uspává: neutrál, odpojit, potvrdit.
    Uspat(Sender<()>),
    /// Počítač se probudil: připojit znovu.
    Probudit,
    /// Konec aplikace: neutrál, odpojit, potvrdit a skončit.
    Konec(Sender<()>),
}

/// Všechno, co pad vlákno potřebuje zvenku: ovladač a hodiny.
pub trait Backend {
    /// Monotónní čas v ms.
    fn ted_ms(&self) -> u64;
    /// Krátké čekání (jen opakování neutrálu před odpojením).
    fn spi_ms(&mut self, ms: u64);
    /// Spojení s ViGEmBus + připojení targetu (bez `wait_ready`).
    fn pripoj(&mut self) -> Result<(), VigemError>;
    /// Proč rozhraní ViGEmBus není: ovladač chybí úplně, nebo je a
    /// neběží. Jen čte registr a správce zařízení — volá se jen po
    /// `pripoj` s [`VigemError::BusMissing`].
    fn stav_sbernice(&mut self) -> BusState;
    /// `wait_ready` — volá se PRÁVĚ JEDNOU po každém `pripoj`.
    fn cekej_na_pripravenost(&mut self) -> Result<(), VigemError>;
    /// Slot XInput (0–3).
    fn slot(&mut self) -> Result<u32, VigemError>;
    /// Poslat stav.
    fn posli(&mut self, report: XusbReport) -> Result<(), VigemError>;
    /// Odpojit target a zavřít spojení; bez targetu nic nedělá.
    fn odpoj(&mut self);
}

/// Kam se hlásí každá změna stavu (Tauri událost + popisek v oznamovací
/// oblasti). Volá se z pad vlákna a nesmí čekat na hlavní vlákno — to
/// může zrovna čekat na pad (konec aplikace).
pub type Oznam = Arc<dyn Fn(&PadInfo) + Send + Sync>;

/// Fáze připojení.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Faze {
    /// Bez targetu (start, chyba, chybějící ViGEmBus, spánek, konec).
    Odpojeno,
    /// Target připojený, `wait_ready` se vrátil, čeká se na slot XInput.
    CekaNaSlot { od: u64, dalsi: u64 },
    /// Target má slot a přijímá stavy.
    Pripojeno,
}

/// Logika pad vlákna — bez vláken a bez čekání (to dělá [`vlakno`]).
pub struct Smycka<B: Backend> {
    b: B,
    status: Arc<PadStatus>,
    oznam: Oznam,
    faze: Faze,
    /// Naposledy ohlášený stav.
    info: PadInfo,
    slot: Option<u32>,
    /// Nejnovější stav od enginu.
    chci: PadState,
    /// Stav, který ovladač naposledy PŘIJAL.
    poslano: Option<XusbReport>,
    /// Kdy ovladač naposledy něco přijal (keep-alive).
    odeslano_ms: u64,
    /// Začátek souvislé řady 170.
    busy_od: Option<u64>,
    /// Začátek zkoušky páčky.
    test_od: Option<u64>,
    uspano: bool,
    konec: bool,
}

impl<B: Backend> Smycka<B> {
    pub fn new(b: B, status: Arc<PadStatus>, oznam: Oznam) -> Smycka<B> {
        let info = status.snapshot();
        Smycka {
            b,
            status,
            oznam,
            faze: Faze::Odpojeno,
            info,
            slot: None,
            chci: PadState::NEUTRAL,
            poslano: None,
            odeslano_ms: 0,
            busy_od: None,
            test_od: None,
            uspano: false,
            konec: false,
        }
    }

    /// Připojit hned po startu — stabilní pořadí hráčů (ROADMAP Fáze 2).
    pub fn start(&mut self) {
        self.pripoj();
    }

    pub fn skoncila(&self) -> bool {
        self.konec
    }

    /// Za kolik ms má smyčka zase něco dělat; `None` = jen na příkaz.
    ///
    /// Bez připojeného padu se nečeká na nic (žádné pollování,
    /// princip 10); s padem jen keep-alive každých 200 ms.
    pub fn cekani_ms(&self) -> Option<u64> {
        if self.konec {
            return Some(0);
        }
        let ted = self.b.ted_ms();
        match self.faze {
            Faze::Odpojeno => None,
            Faze::CekaNaSlot { dalsi, .. } => Some(dalsi.saturating_sub(ted)),
            Faze::Pripojeno => {
                if self.busy_od.is_some() {
                    return Some(BUSY_KROK_MS);
                }
                if let Some(t0) = self.test_od {
                    return Some(TEST_KROK_MS.min((t0 + TEST_MS).saturating_sub(ted)));
                }
                if self.poslano != Some(self.cil(ted)) {
                    return Some(0);
                }
                Some((self.odeslano_ms + KEEPALIVE_MS).saturating_sub(ted))
            }
        }
    }

    /// Zpracuje jeden příkaz. Stavy se jen zapamatují (vyhrává
    /// nejnovější), odešle je až [`Smycka::krok`].
    pub fn prikaz(&mut self, p: PadPrikaz) {
        match p {
            PadPrikaz::Stav(s) => {
                self.chci = s;
                // Skutečný vstup z klávesnice má přednost před ukázkou.
                if self.test_od.take().is_some() {
                    log::info!("pad: zkouška páčky přerušena — přišel stav z klávesnice");
                }
            }
            PadPrikaz::Test => {
                if self.faze == Faze::Pripojeno && self.info.state == PadStav::Connected {
                    log::info!("pad: zkouška páčky (kruh levou páčkou)");
                    self.test_od = Some(self.b.ted_ms());
                } else {
                    log::debug!("pad: zkouška páčky bez připojeného padu — nic");
                }
            }
            PadPrikaz::Znovu => {
                if self.faze != Faze::Odpojeno || self.konec {
                    return;
                }
                if self.uspano {
                    // Oznámení o probuzení nemusí přijít vůbec (Modern
                    // Standby, ovladač napájení…) a pad by pak zůstal
                    // odpojený navždy. Kdo v okně klikne, je vzhůru —
                    // ruční „Připojit znovu" platí jako probuzení.
                    self.uspano = false;
                    log::info!("pad: připojuji ručně — oznámení o probuzení nepřišlo");
                } else {
                    log::info!("pad: znovu připojuji");
                }
                self.pripoj();
            }
            PadPrikaz::Uspat(ack) => {
                if !self.konec {
                    self.neutral_a_odpoj();
                    self.uspano = true;
                    self.publikuj(PadStav::Suspended, String::new());
                }
                let _ = ack.send(());
            }
            PadPrikaz::Probudit => {
                if self.uspano && !self.konec {
                    self.uspano = false;
                    log::info!("pad: po probuzení znovu připojuji");
                    self.pripoj();
                }
            }
            PadPrikaz::Konec(ack) => {
                self.ukonci();
                let _ = ack.send(());
            }
        }
    }

    /// Udělá, co je právě na řadě (dotaz na slot, odeslání stavu).
    pub fn krok(&mut self) {
        if self.konec {
            return;
        }
        let ted = self.b.ted_ms();
        match self.faze {
            Faze::Odpojeno => {}
            Faze::CekaNaSlot { od, dalsi } => {
                if ted >= dalsi {
                    self.zkus_slot(od, ted);
                }
            }
            Faze::Pripojeno => self.odesli(ted),
        }
    }

    /// Neutrál → odpojit → zavřít; smyčka pak skončí.
    pub fn ukonci(&mut self) {
        if self.konec {
            return;
        }
        self.neutral_a_odpoj();
        self.konec = true;
        log::info!("pad: odpojený, vlákno končí");
    }

    fn pripoj(&mut self) {
        // Po (znovu)připojení vždy z neutrálu. Engine je teď v Disabled
        // a nic jiného neposílá — kdyby ale ve frontě zbyl starý stav,
        // nový pad s ním nesmí začít (princip 1).
        self.chci = PadState::NEUTRAL;
        self.test_od = None;
        self.publikuj(PadStav::Connecting, String::new());
        if let Err(e) = self.b.pripoj() {
            if e == VigemError::BusMissing {
                self.bez_sbernice();
            } else {
                log::error!("pad: virtuální ovladač nejde připojit: {e}");
                self.publikuj(
                    PadStav::Error,
                    format!("virtuální ovladač nejde připojit: {e}"),
                );
            }
            return;
        }
        match self.b.cekej_na_pripravenost() {
            Ok(()) => {}
            Err(VigemError::NotReadyYet) => {
                log::info!("pad: Windows zařízení teprve instalují (483) — čekám na slot XInput");
                self.publikuj(
                    PadStav::Connecting,
                    "Windows ovladač instalují poprvé — může to pár sekund trvat".into(),
                );
            }
            Err(e) => {
                self.selhani(format!("virtuální ovladač se nepřipravil: {e}"));
                return;
            }
        }
        let ted = self.b.ted_ms();
        self.faze = Faze::CekaNaSlot {
            od: ted,
            dalsi: ted,
        };
        self.zkus_slot(ted, ted);
    }

    /// Rozhraní ViGEmBus není — proč?
    ///
    /// Instalaci okno nabídne JEN tam, kde ovladač opravdu chybí: nad
    /// existující instalací by nic nespravila (instalátor ViGEmBus pustí
    /// jen na čisté PC) a okno by slibovalo, co se nestane (princip 8).
    /// Vypnutý, zablokovaný nebo na restart čekající ovladač dostane
    /// radu z `updater::vigembus` — tutéž, jakou ukáže instalátor.
    fn bez_sbernice(&mut self) {
        match self.b.stav_sbernice() {
            BusState::NotInstalled => {
                log::warn!("pad: ovladač ViGEmBus v systému není");
                self.publikuj(
                    PadStav::BusMissing,
                    "ovladač ViGEmBus v systému není".into(),
                );
            }
            stav @ BusState::InstalledNotRunning { .. } => {
                let rada = stav.advice().map_or_else(String::new, |a| a.text());
                log::warn!("pad: ViGEmBus je nainstalovaný, ale neběží ({stav:?}) — {rada}");
                self.publikuj(PadStav::BusNotRunning, rada);
            }
            BusState::Ready => {
                // Rozhraní naběhlo mezi pokusem o spojení a zjišťováním
                // (ovladač se zrovna spouští). Samo se znovu nepřipojuje
                // — žádná smyčka; stačí jedno kliknutí.
                log::warn!("pad: rozhraní ViGEmBus se objevilo až po pokusu o spojení");
                self.publikuj(
                    PadStav::Error,
                    "ovladač ViGEmBus se právě spustil — klikni na Zkusit znovu".into(),
                );
            }
        }
    }

    fn zkus_slot(&mut self, od: u64, ted: u64) {
        match self.b.slot() {
            Ok(i) => {
                log::info!("pad: slot XInput {i} (hráč {})", i + 1);
                self.slot = Some(i);
                self.faze = Faze::Pripojeno;
                self.poslano = None;
                self.busy_od = None;
                // „Připojeno" až po přijatém neutrálu (viz `odesli`).
                self.odesli(ted);
            }
            Err(VigemError::NoUserIndex | VigemError::NotReadyYet)
                if ted.saturating_sub(od) < SLOT_LIMIT_MS =>
            {
                self.faze = Faze::CekaNaSlot {
                    od,
                    dalsi: ted + SLOT_KROK_MS,
                };
            }
            Err(VigemError::NoUserIndex | VigemError::NotReadyYet) => self.selhani(
                "Windows nepřidělily virtuálnímu ovladači číslo hráče — nejspíš už jsou \
                 připojené 4 ovladače (víc XInput neumí). Odpoj jeden a zkus to znovu."
                    .into(),
            ),
            Err(e) => self.selhani(format!("virtuální ovladač nedostal číslo hráče: {e}")),
        }
    }

    /// Cílový stav: zkouška páčky, jinak nejnovější stav od enginu.
    fn cil(&self, ted: u64) -> XusbReport {
        match self.test_od {
            Some(t0) if ted.saturating_sub(t0) < TEST_MS => {
                let (lx, ly) = kruh(ted - t0);
                XusbReport {
                    lx,
                    ly,
                    ..XusbReport::NEUTRAL
                }
            }
            _ => XusbReport::from(&self.chci),
        }
    }

    fn odesli(&mut self, ted: u64) {
        if let Some(t0) = self.test_od {
            if ted.saturating_sub(t0) >= TEST_MS {
                self.test_od = None;
                log::info!("pad: zkouška páčky hotová");
            }
        }
        let cil = self.cil(ted);
        let na_rade =
            self.poslano != Some(cil) || ted.saturating_sub(self.odeslano_ms) >= KEEPALIVE_MS;
        if !na_rade {
            return;
        }
        match self.b.posli(cil) {
            Ok(()) => {
                self.poslano = Some(cil);
                self.odeslano_ms = ted;
                self.busy_od = None;
                self.status.tep(ted);
                if self.info.state != PadStav::Connected {
                    log::info!("pad: připojený (ovladač přijal neutrál)");
                    self.publikuj(PadStav::Connected, String::new());
                    // Fáze 4: tady pad vlákno pustí engine ven z Disabled
                    // (`Engine::enable()` přes příkaz hook vláknu). Při
                    // `selhani` naopak `Engine::disable(PadError)`.
                }
            }
            Err(VigemError::Busy) => {
                // Report se zahodil: `poslano` zůstává, takže příští krok
                // pošle znovu — a to vždy nejnovější stav.
                let od = *self.busy_od.get_or_insert(ted);
                self.status.tep(ted);
                if ted.saturating_sub(od) >= BUSY_LIMIT_MS {
                    self.selhani(format!(
                        "virtuální ovladač {} ms nepřijímá data",
                        ted.saturating_sub(od)
                    ));
                }
            }
            Err(e) => self.selhani(format!("odeslání stavu padu selhalo: {e}")),
        }
    }

    /// Porucha: odpojit a ohlásit. Zmizelý ovladač je bezpečnější než
    /// ovladač, kterému mohla zůstat vychýlená páčka (princip 1).
    fn selhani(&mut self, detail: String) {
        log::error!("pad: {detail}");
        self.b.odpoj();
        self.vycisti();
        self.publikuj(PadStav::Error, detail);
    }

    fn neutral_a_odpoj(&mut self) {
        match self.faze {
            Faze::Odpojeno => return,
            // Bez slotu hry pad ještě nevidí — neutrál nemá komu jít.
            Faze::CekaNaSlot { .. } => {}
            Faze::Pripojeno => {
                let od = self.b.ted_ms();
                loop {
                    match self.b.posli(XusbReport::NEUTRAL) {
                        Ok(()) => break,
                        Err(VigemError::Busy)
                            if self.b.ted_ms().saturating_sub(od) < BUSY_LIMIT_MS =>
                        {
                            self.b.spi_ms(BUSY_KROK_MS);
                        }
                        Err(e) => {
                            log::warn!(
                                "pad: neutrál před odpojením nevyšel ({e}) — odpojuji i tak"
                            );
                            break;
                        }
                    }
                }
            }
        }
        self.b.odpoj();
        self.vycisti();
    }

    fn vycisti(&mut self) {
        self.faze = Faze::Odpojeno;
        self.slot = None;
        self.poslano = None;
        self.busy_od = None;
        self.test_od = None;
    }

    fn publikuj(&mut self, state: PadStav, detail: String) {
        let player = match (state, self.slot) {
            (PadStav::Connected, Some(s)) => u8::try_from(s + 1).ok(),
            _ => None,
        };
        if self.info.state == state && self.info.player == player && self.info.detail == detail {
            return;
        }
        self.info = PadInfo {
            state,
            player,
            detail,
            seq: self.info.seq + 1,
        };
        self.status.zapis(&self.info);
        (self.oznam)(&self.info);
    }
}

/// Levá páčka na kružnici: začátek nahoře, po směru hodinových ručiček,
/// jedno otočení za [`TEST_MS`]. |hodnota| ≤ 32767, takže `i16::MIN`
/// nevznikne (`as` navíc saturuje).
fn kruh(dt_ms: u64) -> (i16, i16) {
    let uhel = dt_ms as f32 / TEST_MS as f32 * std::f32::consts::TAU;
    let r = f32::from(AXIS_MAX);
    (
        (uhel.sin() * r).round() as i16,
        (uhel.cos() * r).round() as i16,
    )
}

/// Skutečný ovladač.
pub struct VigemBackend {
    pad: Option<X360>,
    /// `KEYPAD_BEZ_VIGEM` — ViGEmBus se nepoužije a pad se tváří, jako
    /// by ovladač byl v tomhle stavu. Na testy okna bez skutečného padu
    /// a na PC, kde se virtuální pad právě připojit nemá (třeba během
    /// hry); zapíše se do logu. Viz [`simulace_z_promenne`].
    simulace: Option<BusState>,
}

/// `KEYPAD_BEZ_VIGEM`: nenastavená = skutečný ViGEmBus; `vypnuty` =
/// nainstalovaný, ale vypnutý ve Správci zařízení (rada „zapni ho");
/// `zbytek` = zbyla jen služba, bez zařízení i záznamu v Aplikacích
/// (rada s adresou ruční instalace); cokoli jiného = ViGEmBus v systému
/// není (nabídka instalace).
fn simulace_z_promenne(hodnota: Option<&std::ffi::OsStr>) -> Option<BusState> {
    let h = hodnota?;
    Some(if h.eq_ignore_ascii_case("vypnuty") {
        BusState::InstalledNotRunning {
            device: Some(DeviceStatus {
                problem: Some(22),
                need_restart: false,
                started: false,
            }),
            in_apps: true,
        }
    } else if h.eq_ignore_ascii_case("zbytek") {
        BusState::InstalledNotRunning {
            device: None,
            in_apps: false,
        }
    } else {
        BusState::NotInstalled
    })
}

impl Backend for VigemBackend {
    fn ted_ms(&self) -> u64 {
        // Stejné hodiny jako `now_ms` enginu v hooku (Fáze 3) — heartbeat
        // se s nimi bude porovnávat.
        // SAFETY: bez parametrů, jen čte čítač.
        unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
    }

    fn spi_ms(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn pripoj(&mut self) -> Result<(), VigemError> {
        // Starý target (nemělo by nastat) nejdřív pryč — dva by si braly
        // dva sloty XInput.
        self.odpoj();
        if self.simulace.is_some() {
            return Err(VigemError::BusMissing);
        }
        let bus = Bus::connect()?;
        // Při chybě se `bus` zahodí = zavře.
        let pad = X360::plug(bus).map_err(|(_bus, e)| e)?;
        self.pad = Some(pad);
        Ok(())
    }

    fn stav_sbernice(&mut self) -> BusState {
        self.simulace.unwrap_or_else(updater::vigembus::state)
    }

    fn cekej_na_pripravenost(&mut self) -> Result<(), VigemError> {
        self.pad.as_mut().ok_or(VigemError::Gone)?.wait_ready()
    }

    fn slot(&mut self) -> Result<u32, VigemError> {
        self.pad.as_mut().ok_or(VigemError::Gone)?.user_index()
    }

    fn posli(&mut self, report: XusbReport) -> Result<(), VigemError> {
        self.pad.as_mut().ok_or(VigemError::Gone)?.submit(report)
    }

    fn odpoj(&mut self) {
        if let Some(pad) = self.pad.take() {
            drop(pad.unplug());
        }
    }
}

/// Řízení pad vlákna pro zbytek aplikace.
pub struct Pad {
    tx: Sender<PadPrikaz>,
    status: Arc<PadStatus>,
}

impl Pad {
    /// Spustí pad vlákno. Nikdy neselže: když vlákno nejde vytvořit,
    /// stav padu je chyba a aplikace běží dál bez padu.
    pub fn spust(oznam: Oznam) -> Pad {
        let status = Arc::new(PadStatus::new());
        let (tx, rx) = crossbeam_channel::unbounded();
        let simulace = simulace_z_promenne(std::env::var_os("KEYPAD_BEZ_VIGEM").as_deref());
        if let Some(s) = simulace {
            log::warn!("KEYPAD_BEZ_VIGEM: ViGEmBus se nepoužije (simulace: {s:?})");
        }
        let backend = VigemBackend {
            pad: None,
            simulace,
        };
        let smycka = Smycka::new(backend, Arc::clone(&status), Arc::clone(&oznam));
        let status_vlakna = Arc::clone(&status);
        // Výchozí zásobník: rezervace nic nestojí (paměť se bere až
        // použitím) a přetečení by shodilo celý proces i s hookem.
        let spusteno = std::thread::Builder::new()
            .name("keypad-pad".into())
            .spawn(move || {
                let vysledek =
                    std::panic::catch_unwind(AssertUnwindSafe(move || vlakno(smycka, rx)));
                if vysledek.is_err() {
                    // Target zmizel s rozvinutím zásobníku (Drop zavře
                    // spojení → ovladač ho odpojí). Okno se to musí dozvědět.
                    let mut info = status_vlakna.snapshot();
                    info.state = PadStav::Error;
                    info.player = None;
                    info.detail = "pad vlákno spadlo — podrobnosti jsou v logu".into();
                    info.seq += 1;
                    status_vlakna.zapis(&info);
                    oznam(&info);
                }
            });
        if let Err(e) = spusteno {
            log::error!("pad vlákno nejde spustit: {e}");
            let info = PadInfo {
                state: PadStav::Error,
                player: None,
                detail: format!("pad vlákno nejde spustit: {e}"),
                seq: 1,
            };
            status.zapis(&info);
        }
        Pad { tx, status }
    }

    pub fn status(&self) -> &Arc<PadStatus> {
        &self.status
    }

    /// Odesílatel příkazů (uspání, instalátor ViGEmBus). Hook ho dostat
    /// nesmí — viz [`PadPrikaz::Stav`].
    pub fn odesilatel(&self) -> Sender<PadPrikaz> {
        self.tx.clone()
    }

    /// Pošle příkaz; `false`, když vlákno neběží.
    pub fn posli(&self, p: PadPrikaz) -> bool {
        self.tx.send(p).is_ok()
    }

    /// Uklizené ukončení: neutrál → odpojit → zavřít. Čeká nejvýš
    /// `limit`; `true`, když pad vlákno potvrdilo.
    pub fn ukonci(&self, limit: Duration) -> bool {
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        if self.tx.send(PadPrikaz::Konec(ack_tx)).is_err() {
            return false;
        }
        ack_rx.recv_timeout(limit).is_ok()
    }
}

/// Tělo pad vlákna: čeká na příkazy nebo na čas dalšího kroku.
fn vlakno<B: Backend>(mut s: Smycka<B>, rx: Receiver<PadPrikaz>) {
    s.start();
    while !s.skoncila() {
        let prvni = match s.cekani_ms() {
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(ms) => rx.recv_timeout(Duration::from_millis(ms)),
        };
        match prvni {
            Ok(p) => {
                s.prikaz(p);
                // Vyprázdnit frontu: ze stavů platí jen poslední, příkazy
                // se provedou po pořadě.
                while !s.skoncila() {
                    match rx.try_recv() {
                        Ok(p) => s.prikaz(p),
                        Err(_) => break,
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            // Všichni odesílatelé jsou pryč = aplikace končí.
            Err(RecvTimeoutError::Disconnected) => s.ukonci(),
        }
        s.krok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Volani {
        Pripoj,
        Cekej,
        Slot,
        Posli(XusbReport),
        Odpoj,
    }

    /// Falešný ovladač: zapisuje volání, výsledky bere z front
    /// (prázdná fronta = úspěch) a čas posouvá test ručně.
    #[derive(Default)]
    struct Stav {
        cas: u64,
        volani: Vec<Volani>,
        pripoj: VecDeque<Result<(), VigemError>>,
        cekej: VecDeque<Result<(), VigemError>>,
        slot: VecDeque<Result<u32, VigemError>>,
        posli: VecDeque<Result<(), VigemError>>,
        /// O kolik ms se posune čas při každém `posli` (souvislá řada 170).
        posli_trva: u64,
        pripojeno: bool,
        /// Co vrátí `stav_sbernice`; `None` = ViGEmBus v systému není.
        sbernice: Option<BusState>,
    }

    #[derive(Clone, Default)]
    struct Falesny(Rc<RefCell<Stav>>);

    impl Backend for Falesny {
        fn ted_ms(&self) -> u64 {
            self.0.borrow().cas
        }
        fn spi_ms(&mut self, ms: u64) {
            self.0.borrow_mut().cas += ms;
        }
        fn pripoj(&mut self) -> Result<(), VigemError> {
            let mut s = self.0.borrow_mut();
            s.volani.push(Volani::Pripoj);
            let r = s.pripoj.pop_front().unwrap_or(Ok(()));
            s.pripojeno = r.is_ok();
            r
        }
        fn stav_sbernice(&mut self) -> BusState {
            self.0.borrow().sbernice.unwrap_or(BusState::NotInstalled)
        }
        fn cekej_na_pripravenost(&mut self) -> Result<(), VigemError> {
            let mut s = self.0.borrow_mut();
            s.volani.push(Volani::Cekej);
            s.cekej.pop_front().unwrap_or(Ok(()))
        }
        fn slot(&mut self) -> Result<u32, VigemError> {
            let mut s = self.0.borrow_mut();
            s.volani.push(Volani::Slot);
            s.slot.pop_front().unwrap_or(Ok(1))
        }
        fn posli(&mut self, r: XusbReport) -> Result<(), VigemError> {
            let mut s = self.0.borrow_mut();
            assert!(s.pripojeno, "posílá se do nepřipojeného padu");
            s.volani.push(Volani::Posli(r));
            s.cas += s.posli_trva;
            s.posli.pop_front().unwrap_or(Ok(()))
        }
        fn odpoj(&mut self) {
            let mut s = self.0.borrow_mut();
            if s.pripojeno {
                s.volani.push(Volani::Odpoj);
                s.pripojeno = false;
            }
        }
    }

    struct Test {
        b: Falesny,
        s: Smycka<Falesny>,
        ohlaseno: Arc<Mutex<Vec<PadInfo>>>,
    }

    impl Test {
        fn new() -> Test {
            Test::s(Stav::default())
        }

        fn s(stav: Stav) -> Test {
            let b = Falesny(Rc::new(RefCell::new(stav)));
            let ohlaseno = Arc::new(Mutex::new(Vec::new()));
            let o = Arc::clone(&ohlaseno);
            let oznam: Oznam = Arc::new(move |i: &PadInfo| o.lock().unwrap().push(i.clone()));
            let s = Smycka::new(b.clone(), Arc::new(PadStatus::new()), oznam);
            Test { b, s, ohlaseno }
        }

        fn cas(&self, ms: u64) {
            self.b.0.borrow_mut().cas = ms;
        }

        fn posun(&self, ms: u64) {
            self.b.0.borrow_mut().cas += ms;
        }

        fn volani(&self) -> Vec<Volani> {
            std::mem::take(&mut self.b.0.borrow_mut().volani)
        }

        fn stav(&self) -> PadStav {
            self.s.status.stav()
        }

        /// Chod smyčky jako ve vlákně, jen s posunem času místo čekání.
        fn bez_do(&mut self, konec_ms: u64) {
            while self.b.ted_ms() < konec_ms && !self.s.skoncila() {
                match self.s.cekani_ms() {
                    None => break,
                    Some(ms) => {
                        let ted = self.b.ted_ms();
                        self.cas((ted + ms.max(1)).min(konec_ms));
                    }
                }
                self.s.krok();
            }
        }
    }

    const NEUTRAL: XusbReport = XusbReport::NEUTRAL;

    fn stav_lx(lx: i16) -> PadState {
        PadState {
            thumb_lx: lx,
            ..PadState::NEUTRAL
        }
    }

    #[test]
    fn start_pripoji_pocka_jednou_a_posle_neutral() {
        let mut t = Test::new();
        t.s.start();
        assert_eq!(
            t.volani(),
            [
                Volani::Pripoj,
                Volani::Cekej,
                Volani::Slot,
                Volani::Posli(NEUTRAL)
            ]
        );
        assert_eq!(t.stav(), PadStav::Connected);
        let info = t.s.status.snapshot();
        assert_eq!(info.player, Some(2), "slot 1 = hráč 2");
        // „Připojuji" je výchozí stav (okno ho zná z `pad_status`),
        // takže se hlásí až změna.
        let ohlaseno: Vec<PadStav> = t.ohlaseno.lock().unwrap().iter().map(|i| i.state).collect();
        assert_eq!(ohlaseno, [PadStav::Connected]);
    }

    #[test]
    fn busy_posila_znovu_az_projde() {
        let mut t = Test::s(Stav {
            posli: VecDeque::from([Err(VigemError::Busy), Err(VigemError::Busy)]),
            ..Stav::default()
        });
        t.s.start();
        assert_eq!(t.stav(), PadStav::Connecting, "neutrál ještě neprošel");
        assert_eq!(t.s.cekani_ms(), Some(BUSY_KROK_MS));
        t.bez_do(10);
        let posilani = t
            .volani()
            .into_iter()
            .filter(|v| matches!(v, Volani::Posli(_)))
            .count();
        assert_eq!(posilani, 3, "dvakrát zahozeno, potřetí přijato");
        assert_eq!(t.stav(), PadStav::Connected);
    }

    #[test]
    fn busy_250_ms_je_chyba_a_pad_se_odpoji() {
        let mut t = Test::s(Stav {
            posli: VecDeque::from(vec![Err(VigemError::Busy); 1000]),
            ..Stav::default()
        });
        t.s.start();
        t.bez_do(BUSY_LIMIT_MS - 1);
        assert_eq!(t.stav(), PadStav::Connecting, "249 ms je ještě v limitu");
        t.bez_do(BUSY_LIMIT_MS + 5);
        assert_eq!(t.stav(), PadStav::Error);
        assert_eq!(t.volani().last(), Some(&Volani::Odpoj));
        assert_eq!(t.s.cekani_ms(), None, "po chybě se jen čeká na příkaz");
    }

    #[test]
    fn busy_po_pripojeni_posle_vzdy_nejnovejsi_stav() {
        let mut t = Test::new();
        t.s.start();
        t.volani();
        t.b.0.borrow_mut().posli = VecDeque::from([Err(VigemError::Busy)]);
        t.s.prikaz(PadPrikaz::Stav(stav_lx(100)));
        t.s.krok();
        t.s.prikaz(PadPrikaz::Stav(stav_lx(200)));
        t.posun(1);
        t.s.krok();
        let r = |lx| XusbReport {
            lx,
            ..XusbReport::NEUTRAL
        };
        assert_eq!(t.volani(), [Volani::Posli(r(100)), Volani::Posli(r(200))]);
        assert_eq!(t.s.poslano, Some(r(200)));
    }

    #[test]
    fn vyhrava_nejnovejsi_stav() {
        let mut t = Test::new();
        t.s.start();
        t.volani();
        for lx in [1, 2, 3] {
            t.s.prikaz(PadPrikaz::Stav(stav_lx(lx)));
        }
        assert_eq!(t.s.cekani_ms(), Some(0));
        t.s.krok();
        assert_eq!(
            t.volani(),
            [Volani::Posli(XusbReport {
                lx: 3,
                ..XusbReport::NEUTRAL
            })]
        );
    }

    #[test]
    fn keepalive_kazdych_200_ms_s_heartbeatem() {
        let mut t = Test::new();
        t.cas(1_000);
        t.s.start();
        t.volani();
        assert_eq!(t.s.status.heartbeat_ms(), 1_000);
        assert_eq!(t.s.cekani_ms(), Some(KEEPALIVE_MS));
        t.cas(1_199);
        t.s.krok();
        assert!(t.volani().is_empty(), "199 ms: ještě ne");
        t.cas(1_200);
        t.s.krok();
        assert_eq!(t.volani(), [Volani::Posli(NEUTRAL)]);
        assert_eq!(t.s.status.heartbeat_ms(), 1_200);
        t.bez_do(2_200);
        let pocet = t.volani().len();
        assert_eq!(pocet, 5, "1 s = 5 keep-alive");
    }

    #[test]
    fn zkouska_opise_kruh_a_skonci_neutralem() {
        let mut t = Test::new();
        t.s.start();
        t.volani();
        t.s.prikaz(PadPrikaz::Test);
        t.bez_do(TEST_MS + 50);
        let poslane: Vec<XusbReport> = t
            .volani()
            .into_iter()
            .filter_map(|v| match v {
                Volani::Posli(r) => Some(r),
                _ => None,
            })
            .collect();
        assert!(poslane.len() > 50, "takt ~10 ms: {}", poslane.len());
        assert_eq!(*poslane.last().unwrap(), NEUTRAL, "končí neutrálem");
        assert!(t.s.test_od.is_none());
        // Kruh: všechny body mimo konec na obvodu, žádné tlačítko.
        let r2 = |r: &XusbReport| (f64::from(r.lx).powi(2) + f64::from(r.ly).powi(2)).sqrt();
        for r in &poslane[..poslane.len() - 1] {
            assert_eq!((r.buttons, r.lt, r.rt, r.rx, r.ry), (0, 0, 0, 0, 0));
            assert!((r2(r) - 32_767.0).abs() < 2.0, "{r:?}");
        }
        assert!(poslane.iter().any(|r| r.lx > 30_000) && poslane.iter().any(|r| r.lx < -30_000));
        assert!(poslane.iter().any(|r| r.ly > 30_000) && poslane.iter().any(|r| r.ly < -30_000));
    }

    #[test]
    fn kruh_nikdy_neda_i16_min() {
        for dt in 0..=TEST_MS {
            let (x, y) = kruh(dt);
            assert!(x > i16::MIN && y > i16::MIN, "{dt}");
        }
        assert_eq!(kruh(0), (0, 32_767), "začíná nahoře (kladné Y = nahoru)");
    }

    #[test]
    fn stav_z_klavesnice_prerusi_zkousku() {
        let mut t = Test::new();
        t.s.start();
        t.s.prikaz(PadPrikaz::Test);
        t.bez_do(100);
        t.s.prikaz(PadPrikaz::Stav(stav_lx(7)));
        assert!(t.s.test_od.is_none());
    }

    #[test]
    fn zkouska_bez_padu_nic_nedela() {
        let mut t = Test::s(Stav {
            pripoj: VecDeque::from([Err(VigemError::BusMissing)]),
            ..Stav::default()
        });
        t.s.start();
        t.s.prikaz(PadPrikaz::Test);
        assert!(t.s.test_od.is_none());
    }

    #[test]
    fn konec_posle_neutral_a_pak_odpoji() {
        let mut t = Test::new();
        t.s.start();
        t.s.prikaz(PadPrikaz::Stav(stav_lx(-32_767)));
        t.s.krok();
        t.volani();
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        t.s.prikaz(PadPrikaz::Konec(ack_tx));
        assert_eq!(t.volani(), [Volani::Posli(NEUTRAL), Volani::Odpoj]);
        assert!(ack_rx.try_recv().is_ok(), "potvrzeno");
        assert!(t.s.skoncila());
        // Po konci už nic.
        t.s.prikaz(PadPrikaz::Znovu);
        t.s.krok();
        assert!(t.volani().is_empty());
    }

    #[test]
    fn konec_zkousi_neutral_znovu_pri_busy() {
        let mut t = Test::new();
        t.s.start();
        t.volani();
        t.b.0.borrow_mut().posli = VecDeque::from([Err(VigemError::Busy), Err(VigemError::Busy)]);
        t.s.ukonci();
        assert_eq!(
            t.volani(),
            [
                Volani::Posli(NEUTRAL),
                Volani::Posli(NEUTRAL),
                Volani::Posli(NEUTRAL),
                Volani::Odpoj
            ]
        );
    }

    #[test]
    fn konec_pri_vecnem_busy_odpoji_po_limitu() {
        let mut t = Test::s(Stav {
            posli_trva: 0,
            ..Stav::default()
        });
        t.s.start();
        t.volani();
        t.b.0.borrow_mut().posli = VecDeque::from(vec![Err(VigemError::Busy); 10_000]);
        t.s.ukonci();
        let v = t.volani();
        assert_eq!(v.last(), Some(&Volani::Odpoj));
        assert!(
            v.len() <= BUSY_LIMIT_MS as usize + 2,
            "omezeno časem: {}",
            v.len()
        );
    }

    #[test]
    fn uspani_odpoji_probuzeni_pripoji_znovu() {
        let mut t = Test::new();
        t.s.start();
        t.volani();
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        t.s.prikaz(PadPrikaz::Uspat(ack_tx));
        assert_eq!(t.volani(), [Volani::Posli(NEUTRAL), Volani::Odpoj]);
        assert!(ack_rx.try_recv().is_ok());
        assert_eq!(t.stav(), PadStav::Suspended);
        assert_eq!(t.s.cekani_ms(), None, "ve spánku žádné buzení");
        t.s.prikaz(PadPrikaz::Probudit);
        assert_eq!(
            t.volani(),
            [
                Volani::Pripoj,
                Volani::Cekej,
                Volani::Slot,
                Volani::Posli(NEUTRAL)
            ]
        );
        assert_eq!(t.stav(), PadStav::Connected);
        // Druhé probuzení (PBT_APMRESUMESUSPEND po AUTOMATIC) nic nedělá.
        t.s.prikaz(PadPrikaz::Probudit);
        assert!(t.volani().is_empty());
    }

    /// Oznámení o probuzení nepřišlo (Modern Standby…): „Připojit
    /// znovu" v okně platí jako probuzení, jinak by pad zůstal odpojený
    /// navždy. Pozdě doručené probuzení pak už nic nedělá.
    #[test]
    fn znovu_ve_spanku_pripoji_jako_probuzeni() {
        let mut t = Test::new();
        t.s.start();
        t.volani();
        let (ack_tx, _ack_rx) = crossbeam_channel::bounded(1);
        t.s.prikaz(PadPrikaz::Uspat(ack_tx));
        t.volani();
        assert_eq!(t.stav(), PadStav::Suspended);
        t.s.prikaz(PadPrikaz::Znovu);
        assert_eq!(
            t.volani(),
            [
                Volani::Pripoj,
                Volani::Cekej,
                Volani::Slot,
                Volani::Posli(NEUTRAL)
            ]
        );
        assert_eq!(t.stav(), PadStav::Connected);
        assert!(!t.s.uspano);
        t.s.prikaz(PadPrikaz::Probudit);
        assert!(
            t.volani().is_empty(),
            "pozdní probuzení nepřipojuje podruhé"
        );
        assert_eq!(t.stav(), PadStav::Connected);
    }

    #[test]
    fn uspani_behem_cekani_na_slot_odpoji_bez_neutralu() {
        let mut t = Test::s(Stav {
            slot: VecDeque::from([Err(VigemError::NoUserIndex)]),
            ..Stav::default()
        });
        t.s.start();
        t.volani();
        let (ack_tx, _ack_rx) = crossbeam_channel::bounded(1);
        t.s.prikaz(PadPrikaz::Uspat(ack_tx));
        assert_eq!(t.volani(), [Volani::Odpoj]);
    }

    #[test]
    fn znovu_po_chybe_pripoji() {
        let mut t = Test::new();
        t.s.start();
        t.volani();
        t.b.0.borrow_mut().posli = VecDeque::from([Err(VigemError::Gone)]);
        t.s.prikaz(PadPrikaz::Stav(stav_lx(5)));
        t.s.krok();
        assert_eq!(t.stav(), PadStav::Error);
        assert_eq!(t.volani().last(), Some(&Volani::Odpoj), "55 = chyba hned");
        t.s.prikaz(PadPrikaz::Znovu);
        assert_eq!(
            t.volani(),
            [
                Volani::Pripoj,
                Volani::Cekej,
                Volani::Slot,
                Volani::Posli(NEUTRAL)
            ],
            "znovu od neutrálu, ne od starého stavu"
        );
        assert_eq!(t.stav(), PadStav::Connected);
        // Znovu v připojeném stavu nic nedělá.
        t.s.prikaz(PadPrikaz::Znovu);
        assert!(t.volani().is_empty());
    }

    #[test]
    fn chybejici_vigembus_neni_chyba_a_znovu_to_zkusi() {
        let mut t = Test::s(Stav {
            pripoj: VecDeque::from([Err(VigemError::BusMissing)]),
            ..Stav::default()
        });
        t.s.start();
        assert_eq!(t.stav(), PadStav::BusMissing);
        assert_eq!(t.volani(), [Volani::Pripoj], "bez wait_ready i odpojení");
        assert_eq!(t.s.cekani_ms(), None);
        t.s.prikaz(PadPrikaz::Znovu);
        assert_eq!(t.stav(), PadStav::Connected);
    }

    /// Nainstalovaný, ale neběžící ViGEmBus NENÍ „chybí": okno nesmí
    /// nabízet instalaci (nic by nespravila) — dostane radu z updateru.
    #[test]
    fn nebezici_vigembus_dostane_radu_ne_instalaci() {
        let vypnuty = BusState::InstalledNotRunning {
            device: Some(DeviceStatus {
                problem: Some(22),
                need_restart: false,
                started: false,
            }),
            in_apps: true,
        };
        let mut t = Test::s(Stav {
            pripoj: VecDeque::from([Err(VigemError::BusMissing)]),
            sbernice: Some(vypnuty),
            ..Stav::default()
        });
        t.s.start();
        assert_eq!(t.stav(), PadStav::BusNotRunning);
        let info = t.s.status.snapshot();
        assert_eq!(info.detail, vypnuty.advice().unwrap().text());
        assert!(info.detail.contains("Správci zařízení"), "{}", info.detail);
        assert_eq!(t.volani(), [Volani::Pripoj]);
        // Po zapnutí ovladače stačí „Zkusit znovu".
        t.s.prikaz(PadPrikaz::Znovu);
        assert_eq!(t.stav(), PadStav::Connected);
    }

    #[test]
    fn rozhrani_naskocilo_po_pokusu_je_chyba_se_znovu() {
        let mut t = Test::s(Stav {
            pripoj: VecDeque::from([Err(VigemError::BusMissing)]),
            sbernice: Some(BusState::Ready),
            ..Stav::default()
        });
        t.s.start();
        assert_eq!(t.stav(), PadStav::Error);
        assert_eq!(t.volani(), [Volani::Pripoj], "žádné samovolné opakování");
    }

    /// Porucha při kontrole verze (vypršený limit, přístup) je chyba
    /// padu, ne „ViGEmBus chybí" — instalace by ji nespravila.
    #[test]
    fn porucha_spojeni_je_chyba_ne_chybejici_vigembus() {
        for e in [
            VigemError::TimedOut,
            VigemError::BusAccess(5),
            VigemError::Other(31),
            VigemError::VersionMismatch,
        ] {
            let mut t = Test::s(Stav {
                pripoj: VecDeque::from([Err(e)]),
                ..Stav::default()
            });
            t.s.start();
            assert_eq!(t.stav(), PadStav::Error, "{e:?}");
            assert!(!t.s.status.snapshot().detail.is_empty());
        }
    }

    /// Simulace přes `KEYPAD_BEZ_VIGEM`.
    #[test]
    fn simulace_chybejiciho_a_vypnuteho_vigembus() {
        use std::ffi::OsStr;
        assert_eq!(simulace_z_promenne(None), None);
        assert_eq!(
            simulace_z_promenne(Some(OsStr::new("1"))),
            Some(BusState::NotInstalled)
        );
        assert!(matches!(
            simulace_z_promenne(Some(OsStr::new("vypnuty"))),
            Some(BusState::InstalledNotRunning { device: Some(d), .. }) if d.problem == Some(22)
        ));
        assert!(matches!(
            simulace_z_promenne(Some(OsStr::new("zbytek"))),
            Some(BusState::InstalledNotRunning { device: None, .. })
        ));
    }

    /// `pad_status` čte stav z jiného vlákna než pad vlákno zapisuje —
    /// snímek musí být vždy jedna celá změna (revize: roztržené snímky
    /// s novým `seq` a starou podrobností).
    #[test]
    fn snimek_neni_roztrzeny() {
        let status = Arc::new(PadStatus::new());
        let konec = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pisar = {
            let status = Arc::clone(&status);
            let konec = Arc::clone(&konec);
            std::thread::spawn(move || {
                let mut seq = 0u64;
                while !konec.load(Ordering::Relaxed) {
                    seq += 1;
                    let (state, player, detail) = match seq % 3 {
                        0 => (PadStav::Connecting, None, String::new()),
                        1 => (PadStav::Connected, Some(1), String::new()),
                        _ => (PadStav::Error, None, format!("chyba {seq}")),
                    };
                    status.zapis(&PadInfo {
                        state,
                        player,
                        detail,
                        seq,
                    });
                }
            })
        };
        // Číst, dokud písař neudělá dost změn (souběh musí opravdu
        // nastat), nejdéle ale pár sekund.
        let start = std::time::Instant::now();
        let mut ruznych = 0u64;
        let mut minule = u64::MAX;
        loop {
            let i = status.snapshot();
            let ok = match i.seq % 3 {
                0 => i.state == PadStav::Connecting && i.player.is_none() && i.detail.is_empty(),
                1 => i.state == PadStav::Connected && i.player == Some(1) && i.detail.is_empty(),
                _ => i.state == PadStav::Error && i.detail == format!("chyba {}", i.seq),
            };
            assert!(ok, "roztržený snímek: {i:?}");
            if i.seq != minule {
                ruznych += 1;
                minule = i.seq;
            }
            if (ruznych >= 20_000 && i.seq >= 200_000) || start.elapsed().as_secs() >= 5 {
                break;
            }
        }
        konec.store(true, Ordering::Relaxed);
        pisar.join().unwrap();
        assert!(
            ruznych >= 1_000,
            "souběh nenastal: {ruznych} různých snímků"
        );
    }

    #[test]
    fn prvni_instalace_483_ceka_na_slot() {
        let mut t = Test::s(Stav {
            cekej: VecDeque::from([Err(VigemError::NotReadyYet)]),
            slot: VecDeque::from(vec![Err(VigemError::NoUserIndex); 20]),
            ..Stav::default()
        });
        t.s.start();
        assert_eq!(t.stav(), PadStav::Connecting);
        assert!(!t.s.status.snapshot().detail.is_empty(), "okno ví proč");
        t.bez_do(20 * SLOT_KROK_MS + 1);
        assert_eq!(t.stav(), PadStav::Connected);
        let v = t.volani();
        assert_eq!(
            v.iter().filter(|x| **x == Volani::Cekej).count(),
            1,
            "wait_ready jednou"
        );
        assert_eq!(v.iter().filter(|x| **x == Volani::Slot).count(), 21);
        assert!(!v.contains(&Volani::Odpoj));
    }

    #[test]
    fn bez_slotu_10_s_je_chyba_ctyri_ovladace() {
        let mut t = Test::s(Stav {
            slot: VecDeque::from(vec![Err(VigemError::NoUserIndex); 1_000]),
            ..Stav::default()
        });
        t.s.start();
        t.bez_do(SLOT_LIMIT_MS - 10);
        assert_eq!(t.stav(), PadStav::Connecting);
        t.bez_do(SLOT_LIMIT_MS + 100);
        assert_eq!(t.stav(), PadStav::Error);
        assert!(t.s.status.snapshot().detail.contains("4 ovladače"));
        let v = t.volani();
        assert_eq!(v.last(), Some(&Volani::Odpoj));
        assert_eq!(v.iter().filter(|x| **x == Volani::Cekej).count(), 1);
    }

    #[test]
    fn jina_chyba_wait_ready_odpoji_az_po_nem() {
        let mut t = Test::s(Stav {
            cekej: VecDeque::from([Err(VigemError::TimedOut)]),
            ..Stav::default()
        });
        t.s.start();
        assert_eq!(t.volani(), [Volani::Pripoj, Volani::Cekej, Volani::Odpoj]);
        assert_eq!(t.stav(), PadStav::Error);
    }

    #[test]
    fn stejny_stav_se_neohlasuje_dvakrat() {
        let mut t = Test::new();
        t.s.start();
        let pred = t.ohlaseno.lock().unwrap().len();
        t.bez_do(1_000);
        assert_eq!(
            t.ohlaseno.lock().unwrap().len(),
            pred,
            "keep-alive nic neohlašuje"
        );
        let seq: Vec<u64> = t.ohlaseno.lock().unwrap().iter().map(|i| i.seq).collect();
        assert!(seq.windows(2).all(|w| w[0] < w[1]), "{seq:?}");
    }

    /// Celé vlákno s falešným ovladačem: konec přes frontu se potvrdí
    /// a zavření fronty vlákno ukončí.
    #[test]
    fn vlakno_konci_prikazem_i_zavrenim_fronty() {
        struct Poslany(Arc<Mutex<Vec<Volani>>>);
        impl Backend for Poslany {
            fn ted_ms(&self) -> u64 {
                0
            }
            fn spi_ms(&mut self, _ms: u64) {}
            fn pripoj(&mut self) -> Result<(), VigemError> {
                Ok(())
            }
            fn stav_sbernice(&mut self) -> BusState {
                BusState::Ready
            }
            fn cekej_na_pripravenost(&mut self) -> Result<(), VigemError> {
                Ok(())
            }
            fn slot(&mut self) -> Result<u32, VigemError> {
                Ok(0)
            }
            fn posli(&mut self, r: XusbReport) -> Result<(), VigemError> {
                self.0.lock().unwrap().push(Volani::Posli(r));
                Ok(())
            }
            fn odpoj(&mut self) {
                self.0.lock().unwrap().push(Volani::Odpoj);
            }
        }
        for zavrit in [false, true] {
            let log = Arc::new(Mutex::new(Vec::new()));
            let oznam: Oznam = Arc::new(|_: &PadInfo| {});
            let s = Smycka::new(Poslany(Arc::clone(&log)), Arc::new(PadStatus::new()), oznam);
            let (tx, rx) = crossbeam_channel::unbounded();
            let h = std::thread::spawn(move || vlakno(s, rx));
            if zavrit {
                drop(tx);
            } else {
                let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
                tx.send(PadPrikaz::Konec(ack_tx)).unwrap();
                ack_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            h.join().unwrap();
            assert_eq!(
                log.lock().unwrap().last(),
                Some(&Volani::Odpoj),
                "zavřít={zavrit}"
            );
        }
    }
}

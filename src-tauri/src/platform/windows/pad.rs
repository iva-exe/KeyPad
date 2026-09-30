//! Pad vlákno — jediné místo, které mluví s ViGEmBus.
//!
//! Pravidla (revize Fáze 2, měřeno na ovladači 1.21.442; Fáze 2b):
//!
//! - **Jen na povel.** Po startu vlákno sběrnici jen OVĚŘÍ (otevře,
//!   zkontroluje verzi protokolu, zavře) — ať okno ví, jestli ViGEmBus
//!   je a jestli nechce aktualizovat. Virtuální ovladač nepřipojí nikdy
//!   samo: ani po startu, ani po probuzení, ani po aktualizaci. Jen
//!   příkaz [`PadPrikaz::Zapnout`] z přepínače v okně. Hry a Steam tak
//!   ovladač uvidí jen tehdy, když ho uživatel opravdu chce.
//! - **Zapnutí:** spojení → připojit target → PRÁVĚ JEDNOU `wait_ready`.
//!   Chyba 483 (první připojení na novém PC, Windows teprve instalují
//!   zařízení) není porucha: slot XInput se pak zkouší každých 50 ms až
//!   10 s a okno ukazuje „připojuji…". 650 i po 10 s = už jsou připojené
//!   4 ovladače. Pak neutrál a teprve po jeho potvrzení „zapnuto".
//! - **Nikdy** dvakrát `wait_ready` na jeden target a nikdy odpojení
//!   dřív, než se `wait_ready` vrátil (chyba v ovladači). Obojí tu drží
//!   to, že všechno běží v jednom vlákně a `wait_ready` je synchronní —
//!   i „vypnout" uprostřed připojování se zpracuje až po něm.
//! - **Smyčka:** z fronty vždy jen NEJNOVĚJŠÍ stav; každých 200 ms se
//!   stav pošle znovu (ověří, že pad žije) a zapíše se heartbeat.
//! - **170 (ovladač report zahodil):** posílat znovu (~1 ms) — vždy ten
//!   nejnovější stav. Po 250 ms v kuse → chyba. 55 a cokoli jiného →
//!   chyba hned. Při chybě se pad odpojí: zmizelý ovladač nemůže držet
//!   vychýlenou páčku (princip 1).
//! - **Vypnutí, spánek, konec:** neutrál (i s opakováním) → odpojit →
//!   zavřít. Po spánku zůstává VYPNUTÝ — zapne ho zase uživatel
//!   (ViGEmBus BSOD #160 při probuzení s připojeným padem).
//! - **ViGEmBus není:** instalace se nabízí, jen když v systému opravdu
//!   chybí; nainstalovaný, ale neběžící ovladač dostane radu
//!   z `updater::vigembus` (`BusNotRunning`). Starší ovladač než
//!   z posledního vydání → příznak `needs_update` (okno nabídne
//!   aktualizaci přes KeyPadSetup).
//! - **Instalátor ViGEmBus:** před spuštěním pad vypnout (neutrál →
//!   odpojit → zavřít spojení) a až do konce instalátoru nezapínat ani
//!   neotevírat sběrnici — nový ovladač by pad odebral pod rukama
//!   a otevřené spojení by držel starý ovladač v paměti (aktualizace by
//!   chtěla restart). Po konci instalátoru jen ověřit, nic nepřipojit.
//!
//! Logika smyčky ([`Smycka`]) stojí za malým rozhraním [`Backend`]
//! (ovladač + hodiny), takže jde otestovat s falešným ovladačem bez
//! vláken a bez čekání.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
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
/// Jak dlouho po oznámení „počítač se uspává" nejde ovladač zapnout
/// (když nepřijde probuzení dřív). Klik, který by dorazil mezi
/// oznámením a skutečným spánkem, by pad připojil těsně před spánkem
/// — přesně do BSOD ViGEmBus #160. Hodiny (`GetTickCount64`) běží
/// i ve spánku, takže po skutečném spánku pojistka dávno vypršela
/// i bez oznámení o probuzení (Modern Standby ho posílat nemusí).
const SPANEK_POJISTKA_MS: u64 = 5_000;
/// Proč se ovladač nezapnul těsně před spánkem. Po probuzení už neplatí
/// — `PadPrikaz::Probuzeni` ji z okna smaže.
const USPAVA_SE: &str = "Počítač se uspává — zkus to za chvíli.";

/// Stav virtuálního padu, jak ho vidí okno.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PadStav {
    /// ViGEmBus je, ovladač je vypnutý — zapne ho přepínač v okně.
    Off,
    /// Zapíná se (po kliknutí na přepínač).
    Connecting,
    /// Target existuje, má slot XInput a přijal neutrál.
    On,
    /// Ovladač ViGEmBus v systému vůbec NENÍ — jediný stav, kde okno
    /// nabídne instalaci. Aplikace běží dál.
    BusMissing,
    /// ViGEmBus je nainstalovaný, ale neběží (vypnutý ve Správci
    /// zařízení, čeká na restart, zablokovaný). Instalace by nepomohla;
    /// `detail` je rada z `updater::vigembus` (tatáž jako v instalátoru).
    BusNotRunning,
    /// Porucha — čeká se na „Zkusit znovu" nebo nové zapnutí.
    Error,
}

impl PadStav {
    fn z_u8(v: u8) -> PadStav {
        match v {
            1 => PadStav::Connecting,
            2 => PadStav::On,
            3 => PadStav::BusMissing,
            4 => PadStav::Error,
            5 => PadStav::BusNotRunning,
            _ => PadStav::Off,
        }
    }

    fn jako_u8(self) -> u8 {
        match self {
            PadStav::Off => 0,
            PadStav::Connecting => 1,
            PadStav::On => 2,
            PadStav::BusMissing => 3,
            PadStav::Error => 4,
            PadStav::BusNotRunning => 5,
        }
    }
}

/// Stav padu pro okno (příkaz `pad_status` i událost [`UDALOST`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PadInfo {
    pub state: PadStav,
    /// Číslo hráče podle ViGEmBus (index LED + 1), jen když je pad
    /// zapnutý. Okno ho NEukazuje: s druhým virtuálním padem v systému
    /// ViGEmBus hlásí 0 i padu, kterému XInput dal slot 1 (naměřeno
    /// 29. 9. 2026) — číslo by lhalo. Zůstává pro log a Fázi 4.
    pub player: Option<u8>,
    /// Podrobnost česky (proč chyba, co se právě děje); může být
    /// prázdná. Okno ji ukazuje v bublině, ne jako text v kartě. Ve stavu
    /// `Off` je neprázdná jen tehdy, když se poslední zapnutí odmítlo
    /// (počítač se uspává, ViGEmBus se teprve spustil).
    pub detail: String,
    /// ViGEmBus v systému je starší než z posledního vydání
    /// (`updater::vigembus::needs_update`) — okno nabídne aktualizaci.
    pub needs_update: bool,
    /// Běží instalátor ViGEmBus spuštěný z okna: ovladač je vypnutý
    /// a přepínač zablokovaný, dokud instalátor neskončí.
    pub installer: bool,
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
    /// Pad vlákno pad uklidilo a skončilo. Konec aplikace ho může chtít
    /// víckrát (konec relace Windows, smyčka událostí, nabídka) — každé
    /// další „ukonči" se tím dozví, že je hotovo, a nečeká na odpověď
    /// vlákna, které už neběží.
    ukonceno: AtomicBool,
    /// Stav, hráč, podrobnost i `seq` se zapisují a čtou NAJEDNOU.
    /// Po jednotlivých atomikách vracel `pad_status` roztržené snímky
    /// (revize: nový `seq` se starou podrobností, 117 tisíc ze 17 milionů
    /// pod zátěží) — a okno podle `seq` věří, že snímek je celý.
    info: Mutex<PadInfo>,
}

impl PadStatus {
    fn new() -> PadStatus {
        PadStatus {
            stav: AtomicU8::new(PadStav::Off.jako_u8()),
            heartbeat_ms: AtomicU64::new(0),
            ukonceno: AtomicBool::new(false),
            // „Vypnuto" hned od začátku: ověření sběrnice trvá
            // milisekundy a okno se ptá až po stovkách. Kdyby se zeptalo
            // dřív, klik na přepínač jen počká ve frontě za ověřením.
            info: Mutex::new(PadInfo {
                state: PadStav::Off,
                player: None,
                detail: String::new(),
                needs_update: false,
                installer: false,
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

/// Příkazy pro pad vlákno (GUI, uspání, konec relace, instalátor).
#[derive(Debug)]
pub enum PadPrikaz {
    /// Nový stav od enginu. Platí jen nejnovější.
    ///
    /// POZOR, Fáze 4: hook callback touhle frontou posílat NESMÍ
    /// (princip 3). Je to neomezený kanál crossbeamu — `send` do
    /// čekajícího pad vlákna bere `std::sync::Mutex` budíku, který sdílí
    /// s GUI (`pad_test`, `pad_on`…) i s vláknem napájení, a každých
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
    /// Přepínač v okně: připojit virtuální ovladač. JEDINÁ cesta, kudy
    /// se ovladač připojuje.
    Zapnout,
    /// Přepínač v okně: neutrál → odpojit.
    Vypnout,
    /// Zkouška: levá páčka opíše kruh (~1,2 s) a vrátí se na neutrál.
    Test,
    /// „Zkusit znovu" a konec instalátoru ViGEmBus: sběrnici znovu jen
    /// OVĚŘIT (nic se nepřipojí) a stav ohlásit, i když se nezměnil —
    /// okno podle toho pozná, že instalátor doběhl.
    Znovu,
    /// Počítač se uspává: neutrál, odpojit, potvrdit. Zůstane vypnutý.
    Uspat(Sender<()>),
    /// Počítač se probudil. Ovladač se NEzapíná (zapne ho uživatel),
    /// jen skončí pojistka proti zapnutí těsně před spánkem.
    Probuzeni,
    /// Spouští se instalátor ViGEmBus: neutrál → odpojit → zavřít
    /// spojení, přepínač zablokovat, potvrdit. Teprve po potvrzení se
    /// instalátor spustí.
    PredInstalaci(Sender<()>),
    /// Instalátor ViGEmBus skončil (nebo se vůbec nespustil): přepínač
    /// uvolnit a sběrnici jen OVĚŘIT — nic se nepřipojí.
    PoInstalaci,
    /// Konec aplikace: neutrál, odpojit, potvrdit a skončit.
    Konec(Sender<()>),
}

/// Všechno, co pad vlákno potřebuje zvenku: ovladač a hodiny.
pub trait Backend {
    /// Monotónní čas v ms.
    fn ted_ms(&self) -> u64;
    /// Krátké čekání (jen opakování neutrálu před odpojením).
    fn spi_ms(&mut self, ms: u64);
    /// Jen ověřit sběrnici: otevřít, zkontrolovat verzi protokolu,
    /// zavřít. NIC nepřipojuje.
    fn over_sbernici(&mut self) -> Result<(), VigemError>;
    /// Je ViGEmBus v systému starší než z posledního vydání? Jen čte
    /// registr a správce zařízení.
    fn stary_ovladac(&mut self) -> bool;
    /// Spojení s ViGEmBus + připojení targetu (bez `wait_ready`).
    fn pripoj(&mut self) -> Result<(), VigemError>;
    /// Proč rozhraní ViGEmBus není: ovladač chybí úplně, nebo je a
    /// neběží. Jen čte registr a správce zařízení — volá se jen po
    /// [`VigemError::BusMissing`].
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
    /// Bez targetu (vypnuto, chyba, chybějící ViGEmBus, spánek, konec).
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
    /// Kdy přišlo „počítač se uspává" (a ještě nepřišlo probuzení).
    uspava_se_od: Option<u64>,
    /// ViGEmBus je starší než z posledního vydání.
    stary: bool,
    /// Běží instalátor ViGEmBus — nezapínat, sběrnici neotevírat.
    instalace: bool,
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
            uspava_se_od: None,
            stary: false,
            instalace: false,
            konec: false,
        }
    }

    /// Start vlákna: sběrnici jen ověřit, NIC nepřipojit (Fáze 2b).
    pub fn start(&mut self) {
        self.over_a_ohlas();
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
            PadPrikaz::Zapnout => self.zapni(),
            PadPrikaz::Vypnout => {
                if self.konec || self.faze == Faze::Odpojeno {
                    return;
                }
                log::info!("pad: vypínám (přepínač)");
                self.neutral_a_odpoj();
                self.publikuj(PadStav::Off, String::new());
            }
            PadPrikaz::Test => {
                if self.faze == Faze::Pripojeno && self.info.state == PadStav::On {
                    log::info!("pad: zkouška páčky (kruh levou páčkou)");
                    self.test_od = Some(self.b.ted_ms());
                } else {
                    log::debug!("pad: zkouška páčky bez zapnutého padu — nic");
                }
            }
            PadPrikaz::Znovu => {
                if self.faze != Faze::Odpojeno || self.konec {
                    return;
                }
                if self.instalace {
                    // Ani jen ověřit: otevřené spojení by instalátoru
                    // drželo starý ovladač v paměti.
                    log::info!("pad: ověření počká na konec instalátoru ViGEmBus");
                    return;
                }
                log::info!("pad: znovu ověřuji ViGEmBus");
                self.over_a_ohlas();
            }
            PadPrikaz::Uspat(ack) => {
                if !self.konec {
                    self.uspava_se_od = Some(self.b.ted_ms());
                    if self.faze != Faze::Odpojeno {
                        self.neutral_a_odpoj();
                        log::info!("pad: vypnutý kvůli spánku — po probuzení zůstane vypnutý");
                        self.publikuj(PadStav::Off, String::new());
                    }
                }
                let _ = ack.send(());
            }
            PadPrikaz::Probuzeni => {
                // Nic nepřipojovat — zapne ho uživatel (Fáze 2b).
                self.uspava_se_od = None;
                // Odmítnuté zapnutí před spánkem: „počítač se uspává" by
                // po probuzení v okně lhalo, dokud se stav zase nezmění.
                if self.info.state == PadStav::Off && self.info.detail == USPAVA_SE {
                    self.publikuj(PadStav::Off, String::new());
                }
            }
            PadPrikaz::PredInstalaci(ack) => {
                if !self.konec {
                    if self.faze != Faze::Odpojeno {
                        log::info!("pad: vypínám — spouští se instalátor ViGEmBus");
                        // Odpojit = zahodit target i spojení (handle
                        // sběrnice se zavře), ne jen poslat neutrál.
                        self.neutral_a_odpoj();
                    }
                    self.instalace = true;
                    // Zapnutý nebo zapínaný je teď vypnutý; chybějící,
                    // neběžící ViGEmBus a chyba zůstávají, jak byly.
                    let (stav, detail) = match self.info.state {
                        PadStav::On | PadStav::Connecting => (PadStav::Off, String::new()),
                        s => (s, self.info.detail.clone()),
                    };
                    self.publikuj(stav, detail);
                }
                let _ = ack.send(());
            }
            PadPrikaz::PoInstalaci => {
                self.instalace = false;
                if self.konec || self.faze != Faze::Odpojeno {
                    return;
                }
                log::info!("pad: instalátor ViGEmBus skončil — ověřuji ovladač (nic nepřipojuji)");
                // Ohlásit i nezměněný stav: okno čeká na `installer: false`.
                self.over_a_ohlas();
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
        self.status.ukonceno.store(true, Ordering::Release);
        log::info!("pad: odpojený, vlákno končí");
    }

    /// [`Smycka::over`] a ohlásit výsledek, i když se stav nezměnil:
    /// po startu to potřebuje popisek ikony (jinak by zůstal jen
    /// „KeyPad") a po instalátoru okno (tlačítko „instalátor běží…"
    /// čeká na odpověď).
    fn over_a_ohlas(&mut self) {
        let seq = self.info.seq;
        self.over();
        if self.info.seq == seq {
            self.ohlas();
        }
    }

    /// Ověří sběrnici (bez připojení) a ohlásí, co zjistilo.
    fn over(&mut self) {
        self.stary = false;
        match self.b.over_sbernici() {
            Ok(()) => {
                self.stary = self.b.stary_ovladac();
                if self.stary {
                    log::info!(
                        "pad: ViGEmBus je starší než z vydání {} — okno nabídne aktualizaci",
                        updater::vigembus::VERSION
                    );
                } else {
                    log::info!("pad: ViGEmBus je připravený, ovladač vypnutý (zapne ho přepínač)");
                }
                self.publikuj(PadStav::Off, String::new());
            }
            Err(VigemError::BusMissing) => self.bez_sbernice(),
            Err(e) => {
                log::error!("pad: ViGEmBus nejde použít: {e}");
                self.publikuj(PadStav::Error, e.to_string());
            }
        }
    }

    /// Přepínač „zapnout". Jediné místo, odkud se volá [`Smycka::pripoj`].
    fn zapni(&mut self) {
        if self.konec || self.faze != Faze::Odpojeno {
            return;
        }
        if self.instalace {
            // Okno má přepínač zablokovaný; tohle je druhá pojistka
            // (klik těsně před zablokováním). Ohlásit i nezměněný stav,
            // ať přepínač hned ukáže skutečnost.
            log::warn!("pad: zapnutí odmítnuto — běží instalátor ViGEmBus");
            self.ohlas();
            return;
        }
        if let Some(od) = self.uspava_se_od {
            if self.b.ted_ms().saturating_sub(od) < SPANEK_POJISTKA_MS {
                log::warn!("pad: zapnutí odmítnuto — počítač se právě uspává (ViGEmBus #160)");
                self.publikuj(PadStav::Off, USPAVA_SE.into());
                return;
            }
            self.uspava_se_od = None;
        }
        log::info!("pad: zapínám (přepínač)");
        self.pripoj();
    }

    fn pripoj(&mut self) {
        // Po zapnutí vždy z neutrálu. Engine je teď v Disabled a nic
        // jiného neposílá — kdyby ale ve frontě zbyl starý stav, nový
        // pad s ním nesmí začít (princip 1).
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
                    format!("Virtuální ovladač nejde připojit: {e}."),
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
                    "Windows ovladač instalují poprvé — chvíli to potrvá.".into(),
                );
            }
            Err(e) => {
                self.selhani(format!("Virtuální ovladač se nepřipravil: {e}."));
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
    /// jen na čisté PC nebo na starší verzi) a okno by slibovalo, co se
    /// nestane (princip 8). Vypnutý, zablokovaný nebo na restart čekající
    /// ovladač dostane radu z `updater::vigembus` — tutéž, jakou ukáže
    /// instalátor.
    fn bez_sbernice(&mut self) {
        match self.b.stav_sbernice() {
            BusState::NotInstalled => {
                self.stary = false;
                log::warn!("pad: ovladač ViGEmBus v systému není");
                self.publikuj(
                    PadStav::BusMissing,
                    "Ovladač ViGEmBus v systému není.".into(),
                );
            }
            stav @ BusState::InstalledNotRunning { .. } => {
                // Neběžící STARÝ ovladač spraví aktualizace — nabídnout ji.
                self.stary = self.b.stary_ovladac();
                let rada = stav.advice().map_or_else(String::new, |a| a.text());
                log::warn!("pad: ViGEmBus je nainstalovaný, ale neběží ({stav:?}) — {rada}");
                self.publikuj(PadStav::BusNotRunning, rada);
            }
            BusState::Ready => {
                // Rozhraní naběhlo mezi pokusem o spojení a zjišťováním
                // (ovladač se zrovna spouští). Samo se nepřipojuje — žádná
                // smyčka; stačí přepínač znovu.
                log::warn!("pad: rozhraní ViGEmBus se objevilo až po pokusu o spojení");
                self.publikuj(
                    PadStav::Off,
                    "Ovladač ViGEmBus se právě spustil — zkus to znovu.".into(),
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
                // „Zapnuto" až po přijatém neutrálu (viz `odesli`).
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
                "Windows nepřidělily ovladači číslo hráče — nejspíš už jsou připojené \
                 4 ovladače (víc XInput neumí)."
                    .into(),
            ),
            Err(e) => self.selhani(format!("Virtuální ovladač nedostal číslo hráče: {e}.")),
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
                if self.info.state != PadStav::On {
                    log::info!("pad: zapnutý (ovladač přijal neutrál)");
                    self.publikuj(PadStav::On, String::new());
                    // Fáze 4: tady pad vlákno pustí engine ven z Disabled
                    // (`Engine::enable()` přes příkaz hook vláknu). Při
                    // `selhani` a vypnutí naopak `Engine::disable(…)`.
                }
            }
            Err(VigemError::Busy) => {
                // Report se zahodil: `poslano` zůstává, takže příští krok
                // pošle znovu — a to vždy nejnovější stav.
                let od = *self.busy_od.get_or_insert(ted);
                self.status.tep(ted);
                if ted.saturating_sub(od) >= BUSY_LIMIT_MS {
                    self.selhani(format!(
                        "Virtuální ovladač {} ms nepřijímá data.",
                        ted.saturating_sub(od)
                    ));
                }
            }
            Err(e) => self.selhani(format!("Odeslání stavu padu selhalo: {e}.")),
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
            (PadStav::On, Some(s)) => u8::try_from(s + 1).ok(),
            _ => None,
        };
        if self.info.state == state
            && self.info.player == player
            && self.info.detail == detail
            && self.info.needs_update == self.stary
            && self.info.installer == self.instalace
        {
            return;
        }
        self.info = PadInfo {
            state,
            player,
            detail,
            needs_update: self.stary,
            installer: self.instalace,
            seq: self.info.seq,
        };
        self.ohlas();
    }

    /// Ohlásí aktuální stav s novým `seq` (i nezměněný).
    fn ohlas(&mut self) {
        self.info.seq += 1;
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
    /// `KEYPAD_VIGEM_STARY` — tvářit se, že ViGEmBus potřebuje
    /// aktualizaci (test okna; instalátor se pak nespouští).
    simulace_stary: bool,
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
        // se s nimi bude porovnávat. Běží i ve spánku (pojistka spánku).
        // SAFETY: bez parametrů, jen čte čítač.
        unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
    }

    fn spi_ms(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn over_sbernici(&mut self) -> Result<(), VigemError> {
        if self.simulace.is_some() {
            return Err(VigemError::BusMissing);
        }
        // Spojení se hned zavře (Drop): otevřený handle by držel ovladač
        // v paměti a jeho aktualizace by pak chtěla restart.
        Bus::connect().map(drop)
    }

    fn stary_ovladac(&mut self) -> bool {
        self.simulace_stary || updater::vigembus::needs_update()
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
    /// Běží simulace (`KEYPAD_BEZ_VIGEM`, `KEYPAD_VIGEM_STARY`) — test
    /// okna. Instalátor ViGEmBus se pak nikdy nespustí.
    simulace: bool,
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
        let simulace_stary = std::env::var_os("KEYPAD_VIGEM_STARY").is_some();
        if simulace_stary {
            log::warn!("KEYPAD_VIGEM_STARY: ViGEmBus se tváří jako starý (simulace)");
        }
        let backend = VigemBackend {
            pad: None,
            simulace,
            simulace_stary,
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
                    info.detail = "Pad vlákno spadlo — podrobnosti jsou v logu.".into();
                    // Konec instalátoru by už nikdo neohlásil — okno by
                    // navždy ukazovalo „instalátor běží" místo poruchy.
                    info.installer = false;
                    info.seq += 1;
                    status_vlakna.zapis(&info);
                    status_vlakna.ukonceno.store(true, Ordering::Release);
                    oznam(&info);
                }
            });
        if let Err(e) = spusteno {
            log::error!("pad vlákno nejde spustit: {e}");
            let info = PadInfo {
                state: PadStav::Error,
                player: None,
                detail: format!("Pad vlákno nejde spustit: {e}."),
                needs_update: false,
                installer: false,
                seq: 1,
            };
            status.zapis(&info);
        }
        Pad {
            tx,
            status,
            simulace: simulace.is_some() || simulace_stary,
        }
    }

    pub fn status(&self) -> &Arc<PadStatus> {
        &self.status
    }

    /// Běží simulace ViGEmBus (test okna)?
    pub fn simulace(&self) -> bool {
        self.simulace
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

    /// Před spuštěním instalátoru ViGEmBus: pad vypnout (neutrál →
    /// odpojit → zavřít spojení) a přepínač zablokovat. `true` až po
    /// potvrzení pad vlákna — dřív se instalátor spouštět nesmí.
    ///
    /// Zablokování platí až do [`PadPrikaz::PoInstalaci`], a to i když
    /// potvrzení nepřišlo včas (příkaz zůstal ve frontě) — `PoInstalaci`
    /// proto musí přijít v každém případě (`gamepad::install_vigembus`
    /// ho posílá z uvolnění `shell::Zamek`).
    pub fn pred_instalaci(&self, limit: Duration) -> bool {
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        self.tx.send(PadPrikaz::PredInstalaci(ack_tx)).is_ok() && ack_rx.recv_timeout(limit).is_ok()
    }

    /// Uklizené ukončení: neutrál → odpojit → zavřít. Čeká nejvýš
    /// `limit`; `true`, když je pad uklizený (i dřívějším voláním).
    ///
    /// Volá se víckrát (konec relace Windows z vlastního vlákna a pak
    /// `RunEvent::Exit`) — druhé volání odpoví hned.
    pub fn ukonci(&self, limit: Duration) -> bool {
        if self.status.ukonceno.load(Ordering::Acquire) {
            return true;
        }
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        if self.tx.send(PadPrikaz::Konec(ack_tx)).is_err() {
            return self.status.ukonceno.load(Ordering::Acquire);
        }
        // Souběžné volání: vlákno skončilo po PRVNÍM Konec a náš se
        // zahodil s frontou — odpověď „odpojeno" pak dá příznak.
        ack_rx.recv_timeout(limit).is_ok() || self.status.ukonceno.load(Ordering::Acquire)
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
    // Kdo ještě čeká na potvrzení (uspání, druhý konec), ať nečeká do
    // limitu — pad je odpojený.
    while let Ok(p) = rx.try_recv() {
        if let PadPrikaz::Uspat(ack) | PadPrikaz::Konec(ack) | PadPrikaz::PredInstalaci(ack) = p {
            let _ = ack.send(());
        }
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
        Over,
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
        over: VecDeque<Result<(), VigemError>>,
        pripoj: VecDeque<Result<(), VigemError>>,
        cekej: VecDeque<Result<(), VigemError>>,
        slot: VecDeque<Result<u32, VigemError>>,
        posli: VecDeque<Result<(), VigemError>>,
        /// O kolik ms se posune čas při každém `posli` (souvislá řada 170).
        posli_trva: u64,
        pripojeno: bool,
        /// Co vrátí `stav_sbernice`; `None` = ViGEmBus v systému není.
        sbernice: Option<BusState>,
        /// Co vrátí `stary_ovladac`.
        stary: bool,
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
        fn over_sbernici(&mut self) -> Result<(), VigemError> {
            let mut s = self.0.borrow_mut();
            assert!(!s.pripojeno, "ověřuje se sběrnice s připojeným padem");
            s.volani.push(Volani::Over);
            s.over.pop_front().unwrap_or(Ok(()))
        }
        fn stary_ovladac(&mut self) -> bool {
            self.0.borrow().stary
        }
        fn pripoj(&mut self) -> Result<(), VigemError> {
            let mut s = self.0.borrow_mut();
            assert!(!s.pripojeno, "druhý target");
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

        /// Start a zapnutí přepínačem; volání vyčištěná.
        fn zapnuty() -> Test {
            let mut t = Test::new();
            t.s.start();
            t.s.prikaz(PadPrikaz::Zapnout);
            assert_eq!(t.stav(), PadStav::On);
            t.volani();
            t
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

        fn pripojeno(&self) -> bool {
            self.b.0.borrow().pripojeno
        }

        fn stav(&self) -> PadStav {
            self.s.status.stav()
        }

        fn info(&self) -> PadInfo {
            self.s.status.snapshot()
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
    const ZAPNUTI: [Volani; 4] = [
        Volani::Pripoj,
        Volani::Cekej,
        Volani::Slot,
        Volani::Posli(NEUTRAL),
    ];

    fn stav_lx(lx: i16) -> PadState {
        PadState {
            thumb_lx: lx,
            ..PadState::NEUTRAL
        }
    }

    // ── Fáze 2b: jen na povel ──────────────────────────────────────

    /// Po startu se sběrnice jen ověří — žádný target, žádný slot
    /// XInput, dokud uživatel nezapne přepínač.
    #[test]
    fn start_jen_overi_sbernici_a_nic_nepripoji() {
        let mut t = Test::new();
        t.s.start();
        assert_eq!(t.volani(), [Volani::Over]);
        assert_eq!(t.stav(), PadStav::Off);
        assert!(!t.pripojeno());
        assert_eq!(t.s.cekani_ms(), None, "vypnutý pad nic nepolluje");
        // Popisek ikony a okno se o „vypnuto" dozvědí hned po startu.
        let ohlaseno: Vec<PadStav> = t.ohlaseno.lock().unwrap().iter().map(|i| i.state).collect();
        assert_eq!(ohlaseno, [PadStav::Off]);
    }

    #[test]
    fn bez_prikazu_se_nepripoji_ani_za_minutu() {
        let mut t = Test::new();
        t.s.start();
        t.volani();
        for _ in 0..600 {
            t.posun(100);
            t.s.krok();
        }
        assert!(t.volani().is_empty());
        assert!(!t.pripojeno());
        assert_eq!(t.stav(), PadStav::Off);
    }

    #[test]
    fn zapnuti_pripoji_pocka_jednou_a_posle_neutral() {
        let mut t = Test::new();
        t.s.start();
        t.volani();
        t.s.prikaz(PadPrikaz::Zapnout);
        assert_eq!(t.volani(), ZAPNUTI);
        assert_eq!(t.stav(), PadStav::On);
        assert_eq!(t.info().player, Some(2), "slot 1 = hráč 2");
        let ohlaseno: Vec<PadStav> = t.ohlaseno.lock().unwrap().iter().map(|i| i.state).collect();
        assert_eq!(ohlaseno, [PadStav::Off, PadStav::Connecting, PadStav::On]);
        // Druhé „zapnout" nic nedělá — žádný druhý target.
        t.s.prikaz(PadPrikaz::Zapnout);
        assert!(t.volani().is_empty());
    }

    #[test]
    fn vypnuti_posle_neutral_a_odpoji() {
        let mut t = Test::zapnuty();
        t.s.prikaz(PadPrikaz::Stav(stav_lx(-32_767)));
        t.s.krok();
        t.volani();
        t.s.prikaz(PadPrikaz::Vypnout);
        assert_eq!(t.volani(), [Volani::Posli(NEUTRAL), Volani::Odpoj]);
        assert_eq!(t.stav(), PadStav::Off);
        assert_eq!(t.info().player, None);
        assert_eq!(t.s.cekani_ms(), None);
        // Vypnutý se nevypíná podruhé.
        t.s.prikaz(PadPrikaz::Vypnout);
        assert!(t.volani().is_empty());
    }

    /// Opakované zapnutí/vypnutí s vychýlenou páčkou: pokaždé neutrál
    /// před odpojením, na konci nic připojené a stejně odpojení jako
    /// připojení.
    #[test]
    fn cyklus_zapni_vypni_konci_neutralem_a_odpojeny() {
        let mut t = Test::new();
        t.s.start();
        let mut vse = Vec::new();
        for i in 0..5 {
            t.s.prikaz(PadPrikaz::Zapnout);
            t.s.prikaz(PadPrikaz::Stav(stav_lx(1_000 + i)));
            let konec = t.b.ted_ms() + 50;
            t.bez_do(konec);
            t.s.prikaz(PadPrikaz::Vypnout);
            let v = t.volani();
            let n = v.len();
            assert_eq!(
                &v[n - 2..],
                [Volani::Posli(NEUTRAL), Volani::Odpoj],
                "{v:?}"
            );
            vse.extend(v);
        }
        assert!(!t.pripojeno());
        assert_eq!(t.stav(), PadStav::Off);
        let pocet = |x: &Volani| vse.iter().filter(|v| *v == x).count();
        assert_eq!(pocet(&Volani::Pripoj), 5);
        assert_eq!(pocet(&Volani::Odpoj), 5);
        assert_eq!(
            pocet(&Volani::Cekej),
            5,
            "wait_ready právě jednou na target"
        );
    }

    /// „Vypnout" během připojování (483, čeká se na slot) se zpracuje
    /// až po návratu `wait_ready` — a bez neutrálu (hry pad ještě nevidí).
    #[test]
    fn vypnuti_behem_pripojovani_je_bezpecne() {
        let mut t = Test::s(Stav {
            cekej: VecDeque::from([Err(VigemError::NotReadyYet)]),
            slot: VecDeque::from(vec![Err(VigemError::NoUserIndex); 5]),
            ..Stav::default()
        });
        t.s.start();
        t.volani();
        t.s.prikaz(PadPrikaz::Zapnout);
        assert_eq!(t.volani(), [Volani::Pripoj, Volani::Cekej, Volani::Slot]);
        assert_eq!(t.stav(), PadStav::Connecting);
        t.bez_do(2 * SLOT_KROK_MS);
        t.volani();
        t.s.prikaz(PadPrikaz::Vypnout);
        assert_eq!(t.volani(), [Volani::Odpoj]);
        assert_eq!(t.stav(), PadStav::Off);
        assert_eq!(t.s.cekani_ms(), None, "už se nečeká na slot");
        t.bez_do(SLOT_LIMIT_MS * 2);
        assert!(t.volani().is_empty());
    }

    // ── Odesílání ──────────────────────────────────────────────────

    #[test]
    fn busy_posila_znovu_az_projde() {
        let mut t = Test::s(Stav {
            posli: VecDeque::from([Err(VigemError::Busy), Err(VigemError::Busy)]),
            ..Stav::default()
        });
        t.s.start();
        t.s.prikaz(PadPrikaz::Zapnout);
        assert_eq!(t.stav(), PadStav::Connecting, "neutrál ještě neprošel");
        assert_eq!(t.s.cekani_ms(), Some(BUSY_KROK_MS));
        t.bez_do(10);
        let posilani = t
            .volani()
            .into_iter()
            .filter(|v| matches!(v, Volani::Posli(_)))
            .count();
        assert_eq!(posilani, 3, "dvakrát zahozeno, potřetí přijato");
        assert_eq!(t.stav(), PadStav::On);
    }

    #[test]
    fn busy_250_ms_je_chyba_a_pad_se_odpoji() {
        let mut t = Test::s(Stav {
            posli: VecDeque::from(vec![Err(VigemError::Busy); 1000]),
            ..Stav::default()
        });
        t.s.start();
        t.s.prikaz(PadPrikaz::Zapnout);
        t.bez_do(BUSY_LIMIT_MS - 1);
        assert_eq!(t.stav(), PadStav::Connecting, "249 ms je ještě v limitu");
        t.bez_do(BUSY_LIMIT_MS + 5);
        assert_eq!(t.stav(), PadStav::Error);
        assert_eq!(t.volani().last(), Some(&Volani::Odpoj));
        assert_eq!(t.s.cekani_ms(), None, "po chybě se jen čeká na příkaz");
    }

    #[test]
    fn busy_po_pripojeni_posle_vzdy_nejnovejsi_stav() {
        let mut t = Test::zapnuty();
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
        let mut t = Test::zapnuty();
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
        t.s.prikaz(PadPrikaz::Zapnout);
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
        let mut t = Test::zapnuty();
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
    fn vypnuti_behem_zkousky_posle_neutral() {
        let mut t = Test::zapnuty();
        t.s.prikaz(PadPrikaz::Test);
        t.bez_do(300);
        t.volani();
        t.s.prikaz(PadPrikaz::Vypnout);
        assert_eq!(t.volani(), [Volani::Posli(NEUTRAL), Volani::Odpoj]);
        assert!(t.s.test_od.is_none());
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
        let mut t = Test::zapnuty();
        t.s.prikaz(PadPrikaz::Test);
        t.bez_do(100);
        t.s.prikaz(PadPrikaz::Stav(stav_lx(7)));
        assert!(t.s.test_od.is_none());
    }

    #[test]
    fn zkouska_vypnuteho_padu_nic_nedela() {
        let mut t = Test::new();
        t.s.start();
        t.s.prikaz(PadPrikaz::Test);
        assert!(t.s.test_od.is_none());
        assert_eq!(t.volani(), [Volani::Over]);
    }

    // ── Konec ──────────────────────────────────────────────────────

    #[test]
    fn konec_posle_neutral_a_pak_odpoji() {
        let mut t = Test::zapnuty();
        t.s.prikaz(PadPrikaz::Stav(stav_lx(-32_767)));
        t.s.krok();
        t.volani();
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        t.s.prikaz(PadPrikaz::Konec(ack_tx));
        assert_eq!(t.volani(), [Volani::Posli(NEUTRAL), Volani::Odpoj]);
        assert!(ack_rx.try_recv().is_ok(), "potvrzeno");
        assert!(t.s.skoncila());
        assert!(t.s.status.ukonceno.load(Ordering::Acquire));
        // Po konci už nic — ani zapnutí.
        t.s.prikaz(PadPrikaz::Zapnout);
        t.s.prikaz(PadPrikaz::Znovu);
        t.s.krok();
        assert!(t.volani().is_empty());
    }

    #[test]
    fn konec_vypnuteho_nic_neposila() {
        let mut t = Test::new();
        t.s.start();
        t.volani();
        t.s.ukonci();
        assert!(t.volani().is_empty());
        assert!(t.s.skoncila());
    }

    #[test]
    fn konec_zkousi_neutral_znovu_pri_busy() {
        let mut t = Test::zapnuty();
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
        t.s.prikaz(PadPrikaz::Zapnout);
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

    // ── Spánek ─────────────────────────────────────────────────────

    /// Před spánkem neutrál → odpojit → vypnuto; po probuzení se NIC
    /// nepřipojuje (Fáze 2b) — zapne ho zase uživatel.
    #[test]
    fn uspani_vypne_a_po_probuzeni_zustane_vypnuty() {
        let mut t = Test::zapnuty();
        t.s.prikaz(PadPrikaz::Stav(stav_lx(20_000)));
        t.s.krok();
        t.volani();
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        t.s.prikaz(PadPrikaz::Uspat(ack_tx));
        assert_eq!(t.volani(), [Volani::Posli(NEUTRAL), Volani::Odpoj]);
        assert!(ack_rx.try_recv().is_ok());
        assert_eq!(t.stav(), PadStav::Off);
        assert_eq!(t.s.cekani_ms(), None, "ve spánku žádné buzení");
        t.posun(60_000);
        t.s.prikaz(PadPrikaz::Probuzeni);
        t.s.prikaz(PadPrikaz::Probuzeni);
        let konec = t.b.ted_ms() + 10_000;
        t.bez_do(konec);
        assert!(t.volani().is_empty(), "po probuzení se nic nepřipojuje");
        assert_eq!(t.stav(), PadStav::Off);
        // Uživatel ho zapne sám.
        t.s.prikaz(PadPrikaz::Zapnout);
        assert_eq!(t.volani(), ZAPNUTI);
    }

    /// Klik na přepínač mezi oznámením o uspání a spánkem by pad
    /// připojil těsně před spánkem (BSOD #160) — odmítne se, dokud
    /// nepřijde probuzení nebo nevyprší pojistka.
    #[test]
    fn zapnuti_tesne_pred_spankem_se_odmitne() {
        let mut t = Test::new();
        t.s.start();
        t.volani();
        let (ack_tx, _ack_rx) = crossbeam_channel::bounded(1);
        t.s.prikaz(PadPrikaz::Uspat(ack_tx));
        assert!(t.volani().is_empty(), "vypnutý pad nemá co odpojit");
        let seq = t.info().seq;
        t.posun(100);
        t.s.prikaz(PadPrikaz::Zapnout);
        assert!(t.volani().is_empty());
        assert_eq!(t.stav(), PadStav::Off);
        assert!(t.info().seq > seq, "okno se dozví, proč se nezapnul");
        assert!(!t.info().detail.is_empty());
        // Probuzení pojistku zruší.
        t.s.prikaz(PadPrikaz::Probuzeni);
        t.s.prikaz(PadPrikaz::Zapnout);
        assert_eq!(t.volani(), ZAPNUTI);
        assert!(t.info().detail.is_empty());
    }

    #[test]
    fn pojistka_spanku_vyprsi_i_bez_probuzeni() {
        let mut t = Test::new();
        t.s.start();
        t.volani();
        let (ack_tx, _ack_rx) = crossbeam_channel::bounded(1);
        t.s.prikaz(PadPrikaz::Uspat(ack_tx));
        t.posun(SPANEK_POJISTKA_MS);
        t.s.prikaz(PadPrikaz::Zapnout);
        assert_eq!(t.volani(), ZAPNUTI);
    }

    /// Zapnutí odmítnuté těsně před spánkem nechá v okně „počítač se
    /// uspává". Po probuzení už to neplatí — věta zmizí (nový `seq`),
    /// ovladač zůstane vypnutý a na ViGEmBus se nesahá.
    #[test]
    fn probuzeni_smaze_vetu_o_uspavani() {
        let mut t = Test::new();
        t.s.start();
        t.volani();
        let (ack_tx, _ack_rx) = crossbeam_channel::bounded(1);
        t.s.prikaz(PadPrikaz::Uspat(ack_tx));
        t.posun(100);
        t.s.prikaz(PadPrikaz::Zapnout);
        assert_eq!(t.info().detail, USPAVA_SE);
        let seq = t.info().seq;
        t.posun(60_000);
        t.s.prikaz(PadPrikaz::Probuzeni);
        assert_eq!(t.stav(), PadStav::Off);
        assert!(t.info().detail.is_empty(), "{:?}", t.info());
        assert!(t.info().seq > seq, "okno se to dozví");
        // Druhé oznámení (AUTOMATIC + RESUMESUSPEND) už nic neohlásí.
        let seq = t.info().seq;
        t.s.prikaz(PadPrikaz::Probuzeni);
        assert_eq!(t.info().seq, seq);
        assert!(t.volani().is_empty(), "probuzení na ovladač nesahá");
    }

    /// Probuzení maže jen větu o uspávání — jiná podrobnost (ViGEmBus
    /// se právě spustil) platí dál a probuzení bez ní nic neohlašuje.
    #[test]
    fn probuzeni_nemaze_jinou_vetu_ani_neohlasuje_zbytecne() {
        let mut t = Test::s(Stav {
            pripoj: VecDeque::from([Err(VigemError::BusMissing)]),
            sbernice: Some(BusState::Ready),
            ..Stav::default()
        });
        t.s.start();
        t.s.prikaz(PadPrikaz::Zapnout);
        let pred = t.info();
        assert!(!pred.detail.is_empty());
        t.s.prikaz(PadPrikaz::Probuzeni);
        assert_eq!(t.info(), pred);

        let mut t = Test::new();
        t.s.start();
        let pred = t.info();
        t.s.prikaz(PadPrikaz::Probuzeni);
        assert_eq!(t.info(), pred, "bez věty o uspávání žádná změna");
    }

    #[test]
    fn uspani_behem_cekani_na_slot_odpoji_bez_neutralu() {
        let mut t = Test::s(Stav {
            slot: VecDeque::from([Err(VigemError::NoUserIndex)]),
            ..Stav::default()
        });
        t.s.start();
        t.s.prikaz(PadPrikaz::Zapnout);
        t.volani();
        let (ack_tx, _ack_rx) = crossbeam_channel::bounded(1);
        t.s.prikaz(PadPrikaz::Uspat(ack_tx));
        assert_eq!(t.volani(), [Volani::Odpoj]);
        assert_eq!(t.stav(), PadStav::Off);
    }

    /// Spánek nesmí přepsat „ViGEmBus chybí" na „vypnuto" — okno by
    /// přestalo nabízet instalaci.
    #[test]
    fn uspani_nemeni_stav_bez_sbernice() {
        let mut t = Test::s(Stav {
            over: VecDeque::from([Err(VigemError::BusMissing)]),
            ..Stav::default()
        });
        t.s.start();
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        t.s.prikaz(PadPrikaz::Uspat(ack_tx));
        assert!(ack_rx.try_recv().is_ok());
        assert_eq!(t.stav(), PadStav::BusMissing);
    }

    // ── Chyby, ViGEmBus, „Zkusit znovu" ────────────────────────────

    /// Po chybě „Zkusit znovu" jen ověří sběrnici a vrátí vypnuto —
    /// připojí zase až přepínač.
    #[test]
    fn znovu_po_chybe_jen_overi() {
        let mut t = Test::zapnuty();
        t.b.0.borrow_mut().posli = VecDeque::from([Err(VigemError::Gone)]);
        t.s.prikaz(PadPrikaz::Stav(stav_lx(5)));
        t.s.krok();
        assert_eq!(t.stav(), PadStav::Error);
        assert_eq!(t.volani().last(), Some(&Volani::Odpoj), "55 = chyba hned");
        t.s.prikaz(PadPrikaz::Znovu);
        assert_eq!(t.volani(), [Volani::Over]);
        assert_eq!(t.stav(), PadStav::Off);
        // Zapnutí po chybě začíná od neutrálu, ne od starého stavu.
        t.s.prikaz(PadPrikaz::Zapnout);
        assert_eq!(t.volani(), ZAPNUTI);
        // Znovu v zapnutém stavu nic nedělá.
        t.s.prikaz(PadPrikaz::Znovu);
        assert!(t.volani().is_empty());
    }

    /// Okno čeká po instalátoru na odpověď — i beze změny stavu.
    #[test]
    fn znovu_ohlasi_i_nezmeneny_stav() {
        let mut t = Test::new();
        t.s.start();
        let pred = t.ohlaseno.lock().unwrap().len();
        let seq = t.info().seq;
        t.s.prikaz(PadPrikaz::Znovu);
        assert_eq!(t.ohlaseno.lock().unwrap().len(), pred + 1);
        assert_eq!(t.info().seq, seq + 1);
        assert_eq!(t.stav(), PadStav::Off);
    }

    #[test]
    fn chybejici_vigembus_neni_chyba_a_znovu_to_overi() {
        let mut t = Test::s(Stav {
            over: VecDeque::from([Err(VigemError::BusMissing)]),
            ..Stav::default()
        });
        t.s.start();
        assert_eq!(t.stav(), PadStav::BusMissing);
        assert_eq!(t.volani(), [Volani::Over], "nic se nepřipojuje");
        assert_eq!(t.s.cekani_ms(), None);
        // Po instalaci: ověřit, pořád nic nepřipojit.
        t.s.prikaz(PadPrikaz::Znovu);
        assert_eq!(t.volani(), [Volani::Over]);
        assert_eq!(t.stav(), PadStav::Off);
    }

    #[test]
    fn zapnuti_bez_vigembus_hlasi_chybejici() {
        let mut t = Test::s(Stav {
            pripoj: VecDeque::from([Err(VigemError::BusMissing)]),
            ..Stav::default()
        });
        t.s.start();
        t.s.prikaz(PadPrikaz::Zapnout);
        assert_eq!(t.stav(), PadStav::BusMissing);
        assert!(!t.pripojeno());
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
            over: VecDeque::from([Err(VigemError::BusMissing)]),
            sbernice: Some(vypnuty),
            ..Stav::default()
        });
        t.s.start();
        assert_eq!(t.stav(), PadStav::BusNotRunning);
        let info = t.info();
        assert_eq!(info.detail, vypnuty.advice().unwrap().text());
        assert!(info.detail.contains("Správci zařízení"), "{}", info.detail);
        assert!(!info.needs_update);
        assert_eq!(t.volani(), [Volani::Over]);
        // Po zapnutí ovladače stačí „Zkusit znovu".
        t.s.prikaz(PadPrikaz::Znovu);
        assert_eq!(t.stav(), PadStav::Off);
    }

    /// Starší ViGEmBus: okno nabídne aktualizaci (příznak), jinak se
    /// chová jako vypnutý pad. Chybějící ovladač příznak nemá.
    #[test]
    fn stary_vigembus_ma_priznak_aktualizace() {
        let mut t = Test::s(Stav {
            stary: true,
            ..Stav::default()
        });
        t.s.start();
        assert_eq!(t.stav(), PadStav::Off);
        assert!(t.info().needs_update);
        t.s.prikaz(PadPrikaz::Zapnout);
        assert_eq!(t.stav(), PadStav::On);
        assert!(t.info().needs_update, "příznak platí dál");
        // Po aktualizaci (instalátor → Znovu) příznak zmizí.
        t.s.prikaz(PadPrikaz::Vypnout);
        t.b.0.borrow_mut().stary = false;
        t.s.prikaz(PadPrikaz::Znovu);
        assert!(!t.info().needs_update);

        let mut t = Test::s(Stav {
            stary: true,
            over: VecDeque::from([Err(VigemError::BusMissing)]),
            ..Stav::default()
        });
        t.s.start();
        assert_eq!(t.stav(), PadStav::BusMissing);
        assert!(
            !t.info().needs_update,
            "chybějící ovladač se instaluje, ne aktualizuje"
        );
    }

    // ── Instalátor ViGEmBus ────────────────────────────────────────

    /// Aktualizace se zapnutým padem: nejdřív neutrál → odpojit (spojení
    /// se zavře), potvrdit; během instalátoru nic nezapne ani neotevře
    /// sběrnici; po něm se sběrnice jen ověří a zapne zase uživatel.
    #[test]
    fn instalator_vypne_pad_a_do_konce_nic_nezapne() {
        let mut t = Test::s(Stav {
            stary: true,
            ..Stav::default()
        });
        t.s.start();
        t.s.prikaz(PadPrikaz::Zapnout);
        assert_eq!(t.stav(), PadStav::On);
        assert!(t.info().needs_update);
        t.s.prikaz(PadPrikaz::Stav(stav_lx(-32_767)));
        t.s.krok();
        t.volani();
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        t.s.prikaz(PadPrikaz::PredInstalaci(ack_tx));
        assert_eq!(t.volani(), [Volani::Posli(NEUTRAL), Volani::Odpoj]);
        assert!(ack_rx.try_recv().is_ok(), "potvrzeno až po odpojení");
        assert!(!t.pripojeno());
        assert_eq!(t.stav(), PadStav::Off);
        assert!(t.info().installer);
        assert_eq!(t.s.cekani_ms(), None, "vypnutý pad nic nepolluje");

        let seq = t.info().seq;
        t.s.prikaz(PadPrikaz::Zapnout);
        assert!(t.info().seq > seq, "odmítnuté zapnutí se ohlásí (přepínač)");
        t.s.prikaz(PadPrikaz::Znovu);
        t.s.prikaz(PadPrikaz::Probuzeni);
        let konec = t.b.ted_ms() + 60_000;
        t.bez_do(konec);
        assert!(t.volani().is_empty(), "během instalátoru ani ověření");
        assert!(t.info().installer);

        // Instalátor skončil (ovladač už je nový).
        t.b.0.borrow_mut().stary = false;
        t.s.prikaz(PadPrikaz::PoInstalaci);
        assert_eq!(t.volani(), [Volani::Over], "jen ověřit, nic nepřipojit");
        let i = t.info();
        assert_eq!(i.state, PadStav::Off);
        assert!(!i.installer && !i.needs_update, "{i:?}");
        assert_eq!(t.s.cekani_ms(), None);
        t.s.prikaz(PadPrikaz::Zapnout);
        assert_eq!(t.volani(), ZAPNUTI);
    }

    /// Instalátor během připojování (483, čeká se na slot): odpojit až
    /// po návratu `wait_ready` (jedno vlákno) a bez neutrálu.
    #[test]
    fn instalator_behem_pripojovani_odpoji_bez_neutralu() {
        let mut t = Test::s(Stav {
            cekej: VecDeque::from([Err(VigemError::NotReadyYet)]),
            slot: VecDeque::from(vec![Err(VigemError::NoUserIndex); 5]),
            ..Stav::default()
        });
        t.s.start();
        t.s.prikaz(PadPrikaz::Zapnout);
        assert_eq!(t.stav(), PadStav::Connecting);
        t.volani();
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        t.s.prikaz(PadPrikaz::PredInstalaci(ack_tx));
        assert_eq!(t.volani(), [Volani::Odpoj]);
        assert!(ack_rx.try_recv().is_ok());
        assert_eq!(t.stav(), PadStav::Off);
        assert!(t.info().detail.is_empty(), "věta o první instalaci zmizí");
        let konec = t.b.ted_ms() + SLOT_LIMIT_MS * 2;
        t.bez_do(konec);
        assert!(t.volani().is_empty(), "už se nečeká na slot");
    }

    /// Instalace chybějícího ViGEmBus: stav „chybí" zůstane (s blokací
    /// přepínače), po instalátoru se ověří skutečnost.
    #[test]
    fn instalator_bez_vigembus_nechava_stav_a_pak_overi() {
        let mut t = Test::s(Stav {
            over: VecDeque::from([Err(VigemError::BusMissing)]),
            ..Stav::default()
        });
        t.s.start();
        t.volani();
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        t.s.prikaz(PadPrikaz::PredInstalaci(ack_tx));
        assert!(ack_rx.try_recv().is_ok());
        assert!(t.volani().is_empty());
        let i = t.info();
        assert_eq!(i.state, PadStav::BusMissing);
        assert!(i.installer && !i.detail.is_empty(), "{i:?}");
        t.s.prikaz(PadPrikaz::PoInstalaci);
        assert_eq!(t.volani(), [Volani::Over]);
        assert_eq!(t.stav(), PadStav::Off);
        assert!(!t.info().installer);
        assert!(!t.pripojeno());
    }

    /// Po konci aplikace se instalátor jen potvrdí (nikdo nečeká do
    /// limitu) a nic se neděje.
    #[test]
    fn instalator_po_konci_jen_potvrdi() {
        let mut t = Test::new();
        t.s.start();
        t.s.ukonci();
        t.volani();
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        t.s.prikaz(PadPrikaz::PredInstalaci(ack_tx));
        t.s.prikaz(PadPrikaz::PoInstalaci);
        assert!(ack_rx.try_recv().is_ok());
        assert!(t.volani().is_empty());
    }

    #[test]
    fn rozhrani_naskocilo_po_pokusu_je_vypnuto_s_radou() {
        let mut t = Test::s(Stav {
            pripoj: VecDeque::from([Err(VigemError::BusMissing)]),
            sbernice: Some(BusState::Ready),
            ..Stav::default()
        });
        t.s.start();
        t.volani();
        t.s.prikaz(PadPrikaz::Zapnout);
        assert_eq!(t.stav(), PadStav::Off);
        assert!(!t.info().detail.is_empty());
        assert_eq!(t.volani(), [Volani::Pripoj], "žádné samovolné opakování");
    }

    /// Porucha při kontrole verze (vypršený limit, přístup) je chyba
    /// padu, ne „ViGEmBus chybí" — instalace by ji nespravila. Platí
    /// pro ověření po startu i pro zapnutí.
    #[test]
    fn porucha_spojeni_je_chyba_ne_chybejici_vigembus() {
        for e in [
            VigemError::TimedOut,
            VigemError::BusAccess(5),
            VigemError::Other(31),
            VigemError::VersionMismatch,
        ] {
            let mut t = Test::s(Stav {
                over: VecDeque::from([Err(e)]),
                ..Stav::default()
            });
            t.s.start();
            assert_eq!(t.stav(), PadStav::Error, "ověření {e:?}");
            assert!(!t.info().detail.is_empty());

            let mut t = Test::s(Stav {
                pripoj: VecDeque::from([Err(e)]),
                ..Stav::default()
            });
            t.s.start();
            t.s.prikaz(PadPrikaz::Zapnout);
            assert_eq!(t.stav(), PadStav::Error, "zapnutí {e:?}");
            assert!(!t.pripojeno());
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
        let konec = Arc::new(AtomicBool::new(false));
        let pisar = {
            let status = Arc::clone(&status);
            let konec = Arc::clone(&konec);
            std::thread::spawn(move || {
                let mut seq = 0u64;
                while !konec.load(Ordering::Relaxed) {
                    seq += 1;
                    let (state, player, detail) = match seq % 3 {
                        0 => (PadStav::Connecting, None, String::new()),
                        1 => (PadStav::On, Some(1), String::new()),
                        _ => (PadStav::Error, None, format!("chyba {seq}")),
                    };
                    status.zapis(&PadInfo {
                        state,
                        player,
                        detail,
                        needs_update: seq % 2 == 0,
                        installer: seq % 5 == 0,
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
            let ok = i.seq == 0
                || (match i.seq % 3 {
                    0 => {
                        i.state == PadStav::Connecting && i.player.is_none() && i.detail.is_empty()
                    }
                    1 => i.state == PadStav::On && i.player == Some(1) && i.detail.is_empty(),
                    _ => i.state == PadStav::Error && i.detail == format!("chyba {}", i.seq),
                } && i.needs_update == (i.seq % 2 == 0)
                    && i.installer == (i.seq % 5 == 0));
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
        t.s.prikaz(PadPrikaz::Zapnout);
        assert_eq!(t.stav(), PadStav::Connecting);
        assert!(!t.info().detail.is_empty(), "okno ví proč");
        t.bez_do(20 * SLOT_KROK_MS + 1);
        assert_eq!(t.stav(), PadStav::On);
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
        t.s.prikaz(PadPrikaz::Zapnout);
        t.bez_do(SLOT_LIMIT_MS - 10);
        assert_eq!(t.stav(), PadStav::Connecting);
        t.bez_do(SLOT_LIMIT_MS + 100);
        assert_eq!(t.stav(), PadStav::Error);
        assert!(t.info().detail.contains("4 ovladače"));
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
        t.volani();
        t.s.prikaz(PadPrikaz::Zapnout);
        assert_eq!(t.volani(), [Volani::Pripoj, Volani::Cekej, Volani::Odpoj]);
        assert_eq!(t.stav(), PadStav::Error);
    }

    #[test]
    fn stejny_stav_se_neohlasuje_dvakrat() {
        let mut t = Test::zapnuty();
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

    // ── Celé vlákno ────────────────────────────────────────────────

    /// Falešný ovladač sdílený s testem přes vlákna.
    struct Sdileny(Arc<Mutex<Vec<Volani>>>);
    impl Backend for Sdileny {
        fn ted_ms(&self) -> u64 {
            0
        }
        fn spi_ms(&mut self, _ms: u64) {}
        fn over_sbernici(&mut self) -> Result<(), VigemError> {
            self.0.lock().unwrap().push(Volani::Over);
            Ok(())
        }
        fn stary_ovladac(&mut self) -> bool {
            false
        }
        fn pripoj(&mut self) -> Result<(), VigemError> {
            self.0.lock().unwrap().push(Volani::Pripoj);
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

    /// Skutečné vlákno: bez příkazu nepřipojí nic, konec přes frontu se
    /// potvrdí a zavření fronty vlákno ukončí (vždy neutrál → odpojit).
    #[test]
    fn vlakno_bez_prikazu_nepripoji_a_konci_prikazem_i_zavrenim_fronty() {
        for zavrit in [false, true] {
            let log = Arc::new(Mutex::new(Vec::new()));
            let oznam: Oznam = Arc::new(|_: &PadInfo| {});
            let s = Smycka::new(Sdileny(Arc::clone(&log)), Arc::new(PadStatus::new()), oznam);
            let (tx, rx) = crossbeam_channel::unbounded();
            let h = std::thread::spawn(move || vlakno(s, rx));
            std::thread::sleep(Duration::from_millis(150));
            assert_eq!(*log.lock().unwrap(), [Volani::Over], "zavřít={zavrit}");
            tx.send(PadPrikaz::Zapnout).unwrap();
            if zavrit {
                drop(tx);
            } else {
                let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
                tx.send(PadPrikaz::Konec(ack_tx)).unwrap();
                ack_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            h.join().unwrap();
            let v = log.lock().unwrap().clone();
            assert!(v.contains(&Volani::Pripoj), "zavřít={zavrit}: {v:?}");
            let n = v.len();
            assert_eq!(
                &v[n - 2..],
                [Volani::Posli(NEUTRAL), Volani::Odpoj],
                "zavřít={zavrit}"
            );
        }
    }

    /// Konec relace Windows a pak `RunEvent::Exit`: další „ukonči" se
    /// nesmí tvářit, že se pad neodpojil (vlákno už neběží), a nesmí
    /// čekat do limitu.
    #[test]
    fn ukonceni_dvakrat_i_soubezne_hlasi_odpojeno() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let oznam: Oznam = Arc::new(|_: &PadInfo| {});
        let status = Arc::new(PadStatus::new());
        let s = Smycka::new(Sdileny(Arc::clone(&log)), Arc::clone(&status), oznam);
        let (tx, rx) = crossbeam_channel::unbounded();
        let h = std::thread::spawn(move || vlakno(s, rx));
        let pad = Arc::new(Pad {
            tx,
            status,
            simulace: false,
        });
        assert!(pad.posli(PadPrikaz::Zapnout));
        let soubezne: Vec<_> = (0..4)
            .map(|_| {
                let pad = Arc::clone(&pad);
                std::thread::spawn(move || pad.ukonci(Duration::from_secs(5)))
            })
            .collect();
        for v in soubezne {
            assert!(v.join().unwrap());
        }
        h.join().unwrap();
        let start = std::time::Instant::now();
        assert!(pad.ukonci(Duration::from_secs(5)), "po konci vlákna");
        assert!(start.elapsed() < Duration::from_secs(1), "nečeká na limit");
        let odpojeni = log
            .lock()
            .unwrap()
            .iter()
            .filter(|v| **v == Volani::Odpoj)
            .count();
        assert_eq!(odpojeni, 1);
    }

    /// `Pad::pred_instalaci` se skutečným vláknem: vrátí se až ve chvíli,
    /// kdy je pad odpojený (neutrál → odpojit) a okno o blokaci ví;
    /// `PoInstalaci` blokaci zruší a nic nepřipojí.
    #[test]
    fn pred_instalaci_ceka_na_odpojeni_padu() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let oznam: Oznam = Arc::new(|_: &PadInfo| {});
        let status = Arc::new(PadStatus::new());
        let s = Smycka::new(Sdileny(Arc::clone(&log)), Arc::clone(&status), oznam);
        let (tx, rx) = crossbeam_channel::unbounded();
        let h = std::thread::spawn(move || vlakno(s, rx));
        let pad = Pad {
            tx,
            status: Arc::clone(&status),
            simulace: false,
        };
        assert!(pad.posli(PadPrikaz::Zapnout));
        assert!(pad.pred_instalaci(Duration::from_secs(5)));
        // Hned po návratu: odpojeno a zablokováno — žádné čekání na nic.
        let v = log.lock().unwrap().clone();
        assert_eq!(&v[v.len() - 2..], [Volani::Posli(NEUTRAL), Volani::Odpoj]);
        let i = status.snapshot();
        assert_eq!(i.state, PadStav::Off);
        assert!(i.installer);

        assert!(pad.posli(PadPrikaz::PoInstalaci));
        let start = std::time::Instant::now();
        while status.snapshot().installer && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!status.snapshot().installer);
        assert_eq!(status.stav(), PadStav::Off);
        let pripojeni = log
            .lock()
            .unwrap()
            .iter()
            .filter(|x| **x == Volani::Pripoj)
            .count();
        assert_eq!(pripojeni, 1, "po instalátoru se nic nepřipojilo");
        assert!(pad.ukonci(Duration::from_secs(5)));
        h.join().unwrap();
    }

    /// Cesta oznámení o spánku až do skutečného pad vlákna (bez
    /// skutečného spánku): zapnutý pad se před spánkem vypne (neutrál →
    /// odpojit), obsluha se dočká potvrzení a po probuzení se NIC
    /// nepřipojí — pad zůstane vypnutý, dokud ho nezapne uživatel.
    #[test]
    fn spanek_pres_obsluhu_napajeni_vypne_a_necha_vypnuty() {
        use windows::Win32::UI::WindowsAndMessaging::{
            PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND,
        };
        let log = Arc::new(Mutex::new(Vec::new()));
        let oznam: Oznam = Arc::new(|_: &PadInfo| {});
        let status = Arc::new(PadStatus::new());
        let s = Smycka::new(Sdileny(Arc::clone(&log)), Arc::clone(&status), oznam);
        let (tx, rx) = crossbeam_channel::unbounded();
        let h = std::thread::spawn(move || vlakno(s, rx));
        tx.send(PadPrikaz::Zapnout).unwrap();
        let cekej_na = |stav: PadStav| {
            let start = std::time::Instant::now();
            while status.stav() != stav && start.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(5));
            }
            status.stav()
        };
        assert_eq!(cekej_na(PadStav::On), PadStav::On);

        super::super::power::obsluz(&tx, PBT_APMSUSPEND);
        // Obsluha se vrátila až po potvrzení: pad už je vypnutý.
        assert_eq!(status.stav(), PadStav::Off);
        let v = log.lock().unwrap().clone();
        assert_eq!(&v[v.len() - 2..], [Volani::Posli(NEUTRAL), Volani::Odpoj]);

        super::super::power::obsluz(&tx, PBT_APMRESUMEAUTOMATIC);
        super::super::power::obsluz(&tx, PBT_APMRESUMESUSPEND);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(status.stav(), PadStav::Off, "po probuzení zůstává vypnutý");
        let pripojeni = log
            .lock()
            .unwrap()
            .iter()
            .filter(|x| **x == Volani::Pripoj)
            .count();
        assert_eq!(pripojeni, 1, "jen to jedno zapnutí uživatelem");

        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        tx.send(PadPrikaz::Konec(ack_tx)).unwrap();
        ack_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        h.join().unwrap();
    }
}

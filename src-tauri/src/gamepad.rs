//! Virtuální ovladače z pohledu okna: příkazy, události se stavem padů,
//! režimem, klávesami a živým stavem, ikona v oznamovací oblasti — a
//! lepidlo mezi pad vlákny, hookem klávesnice a uložením kláves.
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
//! v systému, jen když je zapnutý aspoň jeden ovladač — nebo je okno
//! KeyPadu v popředí (Fáze 6: živé klávesy v okně).
//!
//! Fáze 4b: vlákno okna při změně režimu navíc pípne a přepne ikonu
//! v oznamovací oblasti (pravidla v [`znameni`]) — okno je během hry
//! schované a pozastavení by jinak nešlo poznat.
//!
//! Fáze 6: editor kláves. Hook callback publikuje jen atomiky (režim
//! s cílem přiřazování, poslední oznámení, revizi mapování, živý stav)
//! a nastaví budík; vlákno okna z nich vydá události. Snímek mapování si
//! vlákno okna řekne povelem `Zverejni` (klon jen ve smyčce hooku, nikdy
//! v callbacku), drží ho v zrcadle pro příkazy okna a uložení
//! (`config::Ukladac`). Tvar dat pro okno je v [`smlouva`].

mod smlouva;
mod znameni;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use keypad_core::{
    Action, BindKind, DisabledReason, ForceReason, Mapping, MappingError, PadAction, PadId,
    UiEvent, MAX_PADS,
};
use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::config::{self, Konfigurace, StavKonfigurace, Ukladac};
use crate::platform::windows::hook::{
    ChybaUpravy, Hook, HookOdesilatel, HookPrikaz, Vystup, Zmena, WATCHDOG_MS,
};
use crate::platform::windows::pad::{Oznam, PadInfo, PadPrikaz, PadStav, Pady, UDALOST};
use crate::platform::windows::slot::Budik;
use crate::platform::windows::vystup::{HookVystup, Rezim, RezimInfo, OZNAMENI_OBSAH};
use crate::platform::windows::{klavesy, power, shell, zvuk};
use crate::tray::TrayStav;
use smlouva::{KlavesyInfo, KonfiguraceInfo, Oznameni, RevizeInfo, ZivaInfo, ZivyPad, ZmenaOkna};

/// Tauri událost se změnou režimu (payload [`RezimInfo`]).
pub const UDALOST_REZIM: &str = "rezim";
/// Co se stalo při přiřazování (payload [`Oznameni`]).
const UDALOST_OZNAMENI: &str = "oznameni";
/// Živý stav vstupů (payload [`ZivaInfo`]), jen s viditelným oknem.
const UDALOST_ZIVE: &str = "zive";
/// Mapování se změnilo (payload [`RevizeInfo`]) — okno si klávesy
/// načte příkazem `klavesy` (názvy podle rozložení).
const UDALOST_KLAVESY: &str = "klavesy-zmena";
/// Stav uložení kláves (payload [`KonfiguraceInfo`]).
const UDALOST_KONFIGURACE: &str = "konfigurace";

/// Jak často vlákno okna během hraní kontroluje tep ovladačů (Fáze 5).
/// Callback hooku to kontroluje u každé klávesy; tohle pokryje chvíli,
/// kdy hráč nic nemačká. Mimo hraní se netiká (princip 10).
const PULS_MS: u64 = 500;

/// Živý stav jde do okna nejvýš jednou za snímek obrazovky (60 Hz):
/// autorepeat ani rychlé psaní nesmí okno zahltit událostmi.
const ROZESTUP_ZIVE: Duration = Duration::from_millis(16);

/// Jak dlouho při konci aplikace čekat na neutrál + odpojení padů.
/// Normálně milisekundy; když ovladač visí, proces skončí i tak a pady
/// odpojí ovladač sám se zavřením spojení.
const LIMIT_KONCE: Duration = Duration::from_millis(1_500);

/// Jak dlouho při konci čekat na zápis kláves na disk. Zápis je
/// atomický, takže nestihnutý zápis nechá na disku celý starší soubor.
const LIMIT_ULOZENI: Duration = Duration::from_millis(500);

/// Jak dlouho před spuštěním instalátoru ViGEmBus čekat, než pad
/// vlákna ovladače vypnou. Normálně milisekundy; zdrží je jen rozjeté
/// zapínání (`wait_ready`). Bez potvrzení se instalátor nespustí.
const LIMIT_VYPNUTI: Duration = Duration::from_secs(3);

/// Odpověď hook vlákna na úpravu kláves. Smyčka hooku odpovídá
/// v mikrosekundách; limit jen hlídá, aby příkaz okna nečekal navždy.
const LIMIT_UPRAVY: Duration = Duration::from_secs(1);

const RESTART: &str = "KeyPad je potřeba spustit znovu.";
const NENI_OVLADAC: &str = "Takový ovladač není.";
const NENI_VSTUP: &str = "Takový vstup není.";
const POSLEDNI_KLAVESA: &str = "Poslední klávesu nejde odebrat.";
const VRATIT_NEJDE: &str = "Vrátit už nejde.";

/// Mapování, jak ho zná vlákno okna — pro příkazy okna (`klavesy`,
/// „Zpět") a uložení. Bere ho jen vlákno okna a příkazy, NIKDY hook.
pub struct Zrcadlo {
    /// Revize mapování v enginu, ze které je snímek.
    rev: u64,
    mapovani: Arc<Mapping>,
    /// Předchozí snímek — „Zpět" ho vrátí, jen když je to přesný
    /// předchůdce (revize o jedna menší): jinak by vracel víc změn,
    /// než uživatel viděl.
    predchozi: Option<(u64, Arc<Mapping>)>,
    konfigurace: StavKonfigurace,
    zaloha: Option<PathBuf>,
    /// Chyby nevalidního config.json ze startu — pro bublinu pruhu.
    chyby: Vec<String>,
}

impl Zrcadlo {
    fn novy(
        m: Mapping,
        konfigurace: StavKonfigurace,
        zaloha: Option<PathBuf>,
        chyby: Vec<String>,
    ) -> Zrcadlo {
        Zrcadlo {
            rev: 0,
            mapovani: Arc::new(m),
            predchozi: None,
            konfigurace,
            zaloha,
            chyby,
        }
    }

    /// Nový snímek z hook vlákna; `false` = není novější (zahodit).
    fn prevezmi(&mut self, rev: u64, m: Arc<Mapping>) -> bool {
        if rev <= self.rev {
            return false;
        }
        let stary = std::mem::replace(&mut self.mapovani, m);
        self.predchozi = Some((self.rev, stary));
        self.rev = rev;
        true
    }

    /// Mapování, které „Zpět" vrátí (přesný předchůdce aktuální revize).
    fn lze_vratit(&self) -> Option<&Arc<Mapping>> {
        self.predchozi
            .as_ref()
            .filter(|(r, _)| r.checked_add(1) == Some(self.rev))
            .map(|(_, m)| m)
    }
}

fn zamkni(z: &Mutex<Zrcadlo>) -> MutexGuard<'_, Zrcadlo> {
    // Panika pod zámkem nechá zrcadlo v platném stavu (jen přiřazení).
    z.lock().unwrap_or_else(PoisonError::into_inner)
}

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
    /// Příkazy hooku z okna a nabídky ikony — bez zámku `hook`, který
    /// drží konec.
    prikazy: HookOdesilatel,
    /// ✓ Zvuk v nabídce ikony.
    zvuk: Arc<AtomicBool>,
    /// Aplikace končí: vlákno okna už nepípá (konec vynutí Klávesnici
    /// a to by jinak znělo jako pozastavení).
    konci: Arc<AtomicBool>,
    /// Zrcadlo mapování (vlákno okna, příkazy; nikdy hook).
    klavesy: Arc<Mutex<Zrcadlo>>,
    /// Hlavní okno je vidět (ne schované, ne minimalizované).
    okno_vidi: Arc<AtomicBool>,
    ukladac: Arc<Ukladac>,
    /// Budík vlákna okna.
    budik: Arc<Budik>,
    /// Pořadí snímků živého stavu (události i příkaz `zive`).
    zive_seq: Arc<AtomicU64>,
}

/// Spustí pad vlákno prvního ovladače (jen ověří sběrnici — nic
/// nepřipojí), hlídání spánku, hook vlákno s klávesami z konfigurace
/// (hook sám se nainstaluje až se zapnutým ovladačem nebo oknem
/// v popředí) a ukládání kláves. Stav je pak v `app.state::<Ovladani>()`.
pub fn spust(app: &tauri::App, nacteno: config::Nacteno) -> Result<(), String> {
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
    let mapovani = nacteno.konfigurace.mapovani.clone();
    // Název zkratky pro bublinu ikony — tady, na hlavním vlákně, podle
    // rozložení, se kterým aplikace startovala.
    let zkratka = klavesy::nazev(mapovani.toggle_key());
    let hook = Hook::spust(mapovani.clone(), Arc::clone(&vystup) as Arc<dyn Vystup>)?;
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

    // Klávesy: zrcadlo začíná revizí 0 s mapováním z konfigurace (engine
    // taky), ukládá se až skutečná změna.
    let klavesy_zrcadlo = Arc::new(Mutex::new(Zrcadlo::novy(
        mapovani,
        nacteno.stav,
        nacteno.zaloha.clone(),
        nacteno.chyby.clone(),
    )));
    let ukladac = {
        let handle = app.handle().clone();
        let zrcadlo = Arc::clone(&klavesy_zrcadlo);
        Arc::new(Ukladac::spust(
            updater::config_path(),
            &nacteno,
            move |stav| {
                zamkni(&zrcadlo).konfigurace = stav;
                let _ = handle.emit(UDALOST_KONFIGURACE, KonfiguraceInfo { stav });
            },
        ))
    };

    // Změna režimu → okno, ikona a zvuk; oznámení, klávesy a živý stav
    // → okno. Vlastní vlákno, protože hook callback smí jen nastavit
    // událost (princip 3); tohle vlákno spí, dokud nepřijde — jen během
    // hraní se budí i samo a hlídá tep ovladačů (watchdog).
    let zvuk = Arc::new(AtomicBool::new(nacteno.konfigurace.zvuk));
    let konci = Arc::new(AtomicBool::new(false));
    let okno_vidi = Arc::new(AtomicBool::new(false));
    let zive_seq = Arc::new(AtomicU64::new(0));
    let pocatek = vystup.info();
    let okno = VlaknoOkna {
        app: app.handle().clone(),
        seq: pocatek.seq,
        znameni: znameni::Znameni::new(pocatek.rezim),
        vystup: Arc::clone(&vystup),
        pady: Arc::clone(&pady),
        hook: hook.odesilatel(),
        budik: Arc::clone(&budik),
        zkratka,
        zvuk: Arc::clone(&zvuk),
        konci: Arc::clone(&konci),
        zamek,
        oblast: None,
        oznameni: PrijemOznameni::od(vystup.oznameni()),
        zrcadlo: Arc::clone(&klavesy_zrcadlo),
        pozadano: 0,
        ukladac: Arc::clone(&ukladac),
        okno_vidi: Arc::clone(&okno_vidi),
        zive_seq: Arc::clone(&zive_seq),
        zive: OdesilaniZive::default(),
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
        klavesy: klavesy_zrcadlo,
        okno_vidi,
        ukladac,
        budik,
        zive_seq,
    });
    Ok(())
}

/// Příjem oznámení ze schránky hooku: pozná nové oznámení a mezeru
/// (přepsané oznámení). Čisté — testuje se bez vlákna.
#[derive(Debug)]
struct PrijemOznameni {
    /// Pořadí naposledy přijatého oznámení.
    seq: u16,
    /// Mezera, kterou okno ještě nedostalo (oznámení, které okno
    /// nedostává, ji nese dál).
    mezera: bool,
}

impl PrijemOznameni {
    fn od(schranka: u64) -> PrijemOznameni {
        PrijemOznameni {
            seq: (schranka >> 48) as u16,
            mezera: false,
        }
    }

    /// Nové oznámení pro okno, nebo `None` (nic nového, nebo oznámení,
    /// které okno nedostává).
    fn prijmi(&mut self, schranka: u64) -> Option<(Oznameni, UiEvent)> {
        let seq = (schranka >> 48) as u16;
        if seq == self.seq {
            return None;
        }
        let mezera = self.mezera || seq != self.seq.wrapping_add(1);
        self.seq = seq;
        let Some(u) = UiEvent::unpack(schranka & OZNAMENI_OBSAH) else {
            // Nemělo by nastat (hook píše jen `pack`); okno si radši
            // načte stav znovu, než aby ukázalo smyšlené oznámení.
            self.mezera = true;
            return None;
        };
        match Oznameni::nove(seq, mezera, u) {
            Some(o) => {
                self.mezera = false;
                Some((o, u))
            }
            None => {
                self.mezera = mezera;
                None
            }
        }
    }
}

/// Odesílání živého stavu: jen změna a nejvýš jednou za
/// [`ROZESTUP_ZIVE`]. Čisté — testuje se bez vlákna.
#[derive(Debug, Default)]
struct OdesilaniZive {
    /// Co okno naposledy dostalo.
    posledni: [ZivyPad; MAX_PADS],
    /// Kdy to dostalo.
    kdy: Option<Instant>,
    /// Změna čeká na konec rozestupu (vlákno se probudí samo).
    ceka: Option<Instant>,
}

impl OdesilaniZive {
    /// `Some` = poslat teď.
    fn rozhodni(&mut self, pady: [ZivyPad; MAX_PADS], ted: Instant) -> Option<[ZivyPad; MAX_PADS]> {
        if pady == self.posledni {
            self.ceka = None;
            return None;
        }
        if let Some(dalsi) = self.kdy.map(|k| k + ROZESTUP_ZIVE).filter(|&d| ted < d) {
            self.ceka = Some(dalsi);
            return None;
        }
        self.posledni = pady;
        self.kdy = Some(ted);
        self.ceka = None;
        Some(pady)
    }

    /// Za kolik ms se vlákno okna musí probudit kvůli čekající změně.
    fn probudit_za(&self, ted: Instant) -> Option<u64> {
        self.ceka.map(|t| {
            let ms = t.saturating_duration_since(ted).as_millis();
            u64::try_from(ms).unwrap_or(u64::MAX).saturating_add(1)
        })
    }

    /// Okno je schované — nic nečeká (po ukázání pošle hook stav znovu).
    fn uspi(&mut self) {
        self.ceka = None;
    }
}

/// Vlákno okna (`keypad-okno`): režim do okna, ikona, zvuk, oznámení,
/// zrcadlo kláves, živý stav a watchdog během hraní. Nikdy nečeká na nic
/// jiného než na budík — ikonu předává hlavnímu vláknu, zvuk vláknu
/// zvuku a zápis kláves vláknu `keypad-konfig`.
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
    oznameni: PrijemOznameni,
    zrcadlo: Arc<Mutex<Zrcadlo>>,
    /// Revize, o jejíž snímek už vlákno požádalo (`Zverejni`).
    pozadano: u64,
    ukladac: Arc<Ukladac>,
    okno_vidi: Arc<AtomicBool>,
    zive_seq: Arc<AtomicU64>,
    zive: OdesilaniZive,
}

impl VlaknoOkna {
    fn smycka(mut self) {
        // První obrátka hned: ikona podle výchozího stavu (šedá „vypnuto“).
        // Panika (zapíše ji panic hook) nesmí vlákno zastavit — hlídá tep
        // ovladačů během hry (princip 1).
        let _ = catch_unwind(AssertUnwindSafe(|| self.obratka()));
        loop {
            // Mimo hru, bez tónu čekajícího na odklad a bez živého stavu
            // čekajícího na rozestup spí bez limitu (princip 10).
            let hraje = self.vystup.info().rezim == Rezim::Capturing;
            let limit = [
                hraje.then_some(PULS_MS),
                self.znameni.probudit_za(ted_ms()),
                self.zive.probudit_za(Instant::now()),
            ]
            .into_iter()
            .flatten()
            .min();
            self.budik.cekej(limit);
            let _ = catch_unwind(AssertUnwindSafe(|| self.obratka()));
        }
    }

    fn obratka(&mut self) {
        // Oznámení PŘED režimem: hook zapisuje režim dřív než oznámení,
        // takže kdo vidí nové oznámení, vidí i režim, ke kterému patří —
        // okno tak dostane `rezim` vždy před `oznameni` (spec B4).
        let schranka = self.vystup.oznameni();
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
        self.oznam(schranka);
        self.zrcadli();
        self.zive();
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

    fn oznam(&mut self, schranka: u64) {
        let Some((o, u)) = self.oznameni.prijmi(schranka) else {
            return;
        };
        // Bez klávesy (soukromí, spec 1.5): jen kam se ukládalo.
        if let UiEvent::BindingSaved { target, .. } = u {
            log::info!(
                "klávesy: vazba uložena (ovladač {}, {})",
                target.pad.index() + 1,
                target.action.code()
            );
        }
        let _ = self.app.emit(UDALOST_OZNAMENI, o);
    }

    /// Revize mapování v hooku je novější než zrcadlo → povel `Zverejni`
    /// (jednou na revizi); snímek ze schránky → zrcadlo, uložení
    /// a událost `klavesy-zmena`.
    fn zrcadli(&mut self) {
        let rev = self.vystup.revize_mapovani();
        if rev > zamkni(&self.zrcadlo).rev && rev != self.pozadano {
            self.pozadano = rev;
            self.hook.posli(HookPrikaz::Zverejni);
        }
        let Some((rev, m)) = self.vystup.vezmi_mapovani() else {
            return;
        };
        let mut z = zamkni(&self.zrcadlo);
        if !z.prevezmi(rev, m) {
            return;
        }
        // Pod zámkem zrcadla: „✓ Zvuk" ukládá taky pod ním, takže
        // poslední uložení má vždy nejnovější mapování i zvuk.
        self.ukladac.uloz(Konfigurace {
            mapovani: (*z.mapovani).clone(),
            zvuk: self.zvuk.load(Ordering::Acquire),
        });
        drop(z);
        let _ = self.app.emit(UDALOST_KLAVESY, RevizeInfo { rev });
    }

    /// Živý stav do okna — jen s viditelným oknem a nejvýš jednou za
    /// [`ROZESTUP_ZIVE`]. Hra = stav ve slotu padu (to, co dostává ViGEm).
    fn zive(&mut self) {
        if !self.okno_vidi.load(Ordering::Acquire) {
            self.zive.uspi();
            return;
        }
        let pady = smlouva::zive_pady(self.vystup.zive_vstupy(), self.vystup.hra());
        if let Some(pady) = self.zive.rozhodni(pady, Instant::now()) {
            let seq = self.zive_seq.fetch_add(1, Ordering::AcqRel) + 1;
            let _ = self.app.emit(UDALOST_ZIVE, ZivaInfo { seq, pady });
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

/// „✓ Zvuk“ z nabídky ikony — platí hned a uloží se s klávesami.
pub fn zvuk_z_nabidky(app: &AppHandle, zapnuto: bool) {
    if let Some(o) = app.try_state::<Ovladani>() {
        o.zvuk.store(zapnuto, Ordering::Release);
        log::info!("zvuk {}", if zapnuto { "zapnutý" } else { "vypnutý" });
        let z = zamkni(&o.klavesy);
        o.ukladac.uloz(Konfigurace {
            mapovani: (*z.mapovani).clone(),
            zvuk: zapnuto,
        });
    }
}

/// Hlavní okno je vidět (ukázané, ne minimalizované) — nebo už ne.
///
/// Jen při změně: hook dostane HWND okna (popředí, živý stav) nebo
/// `None` (konec živého stavu, zrušené přiřazování) a vlákno okna se
/// probudí, ať po návratu pošle aktuální živý stav.
pub fn okno_videt(app: &AppHandle, videt: bool) {
    let Some(o) = app.try_state::<Ovladani>() else {
        return;
    };
    if o.okno_vidi.swap(videt, Ordering::AcqRel) == videt {
        return;
    }
    let hwnd = if videt {
        crate::hlavni_okno(app)
            .and_then(|w| w.hwnd().ok())
            .map(|h| h.0 as isize)
    } else {
        None
    };
    log::debug!("okno {}", if videt { "je vidět" } else { "není vidět" });
    o.prikazy.posli(HookPrikaz::Okno(hwnd));
    o.budik.probud();
}

/// Běží simulace ViGEmBus (test okna)? Pak se nesmí hlídat pojmenovaná
/// událost ukončení — patří nainstalovanému KeyPadu vlastníka.
pub fn simulace(app: &AppHandle) -> bool {
    app.try_state::<Ovladani>()
        .is_some_and(|o| o.pady.simulace())
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
/// neutrál → odpojit → zavřít (s časovým limitem) a nakonec klávesy na
/// disk. Smí se volat víckrát a z libovolného vlákna (konec relace
/// Windows, `RunEvent::Exit`).
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
    // Až po padech: ovladač je důležitější než soubor, a zápis je
    // atomický — nestihnutý nechá na disku celý starší.
    if !o.ukladac.uloz_hned(LIMIT_ULOZENI) {
        log::warn!(
            "klávesy se do {} ms neuložily — platí poslední uložené",
            LIMIT_ULOZENI.as_millis()
        );
    }
}

/// Číslo ovladače z okna (bez čísla = první).
fn cislo(pad: Option<u8>) -> Result<usize, String> {
    let i = usize::from(pad.unwrap_or(0));
    if i < MAX_PADS {
        Ok(i)
    } else {
        Err(NENI_OVLADAC.into())
    }
}

fn posli(o: &Ovladani, pad: Option<u8>, p: PadPrikaz) -> Result<(), String> {
    if o.pady.posli(cislo(pad)?, p) {
        Ok(())
    } else {
        Err(RESTART.into())
    }
}

fn posli_hooku(o: &Ovladani, p: HookPrikaz) -> Result<(), String> {
    if o.prikazy.posli(p) {
        Ok(())
    } else {
        Err(RESTART.into())
    }
}

/// Vstup ovladače z okna (ovladač 0–3, kód vstupu z `Action::code`).
fn cil_z_okna(pad: u8, vstup: &str) -> Result<PadAction, String> {
    let pad = PadId::new(usize::from(pad)).ok_or(NENI_OVLADAC)?;
    let action = Action::from_code(vstup).ok_or(NENI_VSTUP)?;
    Ok(PadAction::new(pad, action))
}

/// Pošle úpravu kláves hook vláknu a počká na odpověď. Vnější `Err` =
/// hook vlákno neodpovědělo (text pro okno), vnitřní = proč úprava
/// neprošla.
fn proved_upravu(o: &Ovladani, zmena: Zmena) -> Result<Result<(), ChybaUpravy>, String> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    posli_hooku(o, HookPrikaz::Uprav { zmena, odpoved: tx })?;
    rx.recv_timeout(LIMIT_UPRAVY).map_err(|e| match e {
        crossbeam_channel::RecvTimeoutError::Timeout => {
            log::warn!(
                "klávesy: hook vlákno do {} ms neodpovědělo",
                LIMIT_UPRAVY.as_millis()
            );
            "KeyPad neodpověděl — zkus to znovu.".to_string()
        }
        crossbeam_channel::RecvTimeoutError::Disconnected => RESTART.to_string(),
    })
}

/// Výsledek úpravy z editoru jako odpověď okna (prostá česká věta).
fn text_upravy(r: Result<(), ChybaUpravy>) -> Result<(), String> {
    match r {
        // Vyprázdnit vstup, který už prázdný je: není co hlásit.
        Ok(()) | Err(ChybaUpravy::Mapovani(MappingError::NotMapped { .. })) => Ok(()),
        Err(ChybaUpravy::Mapovani(MappingError::WouldBeEmpty)) => Err(POSLEDNI_KLAVESA.into()),
        Err(ChybaUpravy::Zastarale) => Err(VRATIT_NEJDE.into()),
        Err(ChybaUpravy::Mapovani(e)) => {
            log::warn!("klávesy: úprava odmítnuta ({e})");
            Err("Klávesy takhle změnit nejde.".into())
        }
    }
}

/// Stav ovladače pro okno. Změny pak chodí událostí `pad-stav`.
#[tauri::command]
pub fn pad_status(o: tauri::State<'_, Ovladani>, pad: Option<u8>) -> Result<PadInfo, String> {
    o.pady.stav(cislo(pad)?).ok_or_else(|| NENI_OVLADAC.into())
}

/// Stav všech čtyř ovladačů (při startu a po návratu okna; změny chodí
/// událostí `pad-stav`). Neběžící vlákno = vypnutý ovladač.
#[tauri::command]
pub fn pady(o: tauri::State<'_, Ovladani>) -> Vec<PadInfo> {
    (0..MAX_PADS).filter_map(|i| o.pady.stav(i)).collect()
}

/// Režim zachytávání pro okno (změny chodí událostí `rezim`; starší
/// z obojího okno pozná podle `seq`).
#[tauri::command]
pub fn rezim(o: tauri::State<'_, Ovladani>) -> RezimInfo {
    o.vystup.info()
}

/// Klávesy všech ovladačů s názvy. Synchronní příkaz = hlavní vlákno:
/// `GetKeyNameTextW` bere rozložení volajícího vlákna a hlavní vlákno
/// má rozložení okna (přepnutí jazyka v okně se tak projeví).
#[tauri::command]
pub fn klavesy(o: tauri::State<'_, Ovladani>) -> KlavesyInfo {
    let (rev, m, zpet, konfigurace, zaloha, chyby) = {
        let z = zamkni(&o.klavesy);
        (
            z.rev,
            Arc::clone(&z.mapovani),
            z.lze_vratit().is_some(),
            z.konfigurace,
            z.zaloha.clone(),
            z.chyby.clone(),
        )
    };
    smlouva::klavesy_info(
        rev,
        &m,
        zpet,
        konfigurace,
        zaloha.as_deref(),
        &chyby,
        klavesy::nazev,
    )
}

/// Živý stav vstupů (snímek po návratu okna; změny chodí událostí
/// `zive`). Nové pořadí, ať snímek nepřebije novější událost.
#[tauri::command]
pub fn zive(o: tauri::State<'_, Ovladani>) -> ZivaInfo {
    let seq = o.zive_seq.fetch_add(1, Ordering::AcqRel) + 1;
    ZivaInfo {
        seq,
        pady: smlouva::zive_pady(o.vystup.zive_vstupy(), o.vystup.hra()),
    }
}

/// Klik na čepičku (`pridat = false`: klávesa vstup převezme) nebo na
/// `+` (přidá se). Hook kliknutí přijme, jen když je okno v popředí;
/// výsledek přijde událostmi `rezim` a `oznameni`.
#[tauri::command]
pub fn prirad(
    o: tauri::State<'_, Ovladani>,
    pad: u8,
    vstup: String,
    pridat: bool,
) -> Result<(), String> {
    let cil = cil_z_okna(pad, &vstup)?;
    let druh = if pridat {
        BindKind::Add
    } else {
        BindKind::Replace
    };
    posli_hooku(&o, HookPrikaz::Prirad { cil, druh })
}

/// Esc nebo klik jinam: přiřazování skončí beze změny.
#[tauri::command]
pub fn zrus_prirazeni(o: tauri::State<'_, Ovladani>) -> Result<(), String> {
    posli_hooku(&o, HookPrikaz::ZrusPrirazeni)
}

/// Úprava kláves z editoru: vyprázdnit vstup, výchozí klávesy ovladače
/// 1, „Zpět". I za hry, bez pozastavení (OQ 43).
#[tauri::command(async)]
pub fn uprav_klavesy(o: tauri::State<'_, Ovladani>, zmena: ZmenaOkna) -> Result<(), String> {
    let (z, popis) = match zmena {
        ZmenaOkna::Vyprazdnit { pad, vstup } => {
            let cil = cil_z_okna(pad, &vstup)?;
            (
                Zmena::VyprazdniVstup(cil),
                format!(
                    "vstup vyprázdněn (ovladač {}, {})",
                    pad + 1,
                    cil.action.code()
                ),
            )
        }
        ZmenaOkna::Vychozi => (Zmena::VychoziPrvni, "výchozí klávesy ovladače 1".into()),
        ZmenaOkna::Zpet { rev } => {
            let z = zamkni(&o.klavesy);
            match z.lze_vratit() {
                // Jen z revize, kterou okno vidí; hook to ověří ještě
                // jednou proti enginu (mezitím mohla přijít změna).
                Some(m) if rev == z.rev => (
                    Zmena::Obnov {
                        mapovani: Box::new((**m).clone()),
                        kdyz_revize: rev,
                    },
                    "poslední změna vrácena".into(),
                ),
                _ => return Err(VRATIT_NEJDE.into()),
            }
        }
    };
    text_upravy(proved_upravu(&o, z)?)?;
    log::info!("klávesy: {popis}");
    Ok(())
}

/// 🗑 Odebrat ovladač 2–4 (v okně až po potvrzení): jen vypnutý, a jen
/// když mapování nezůstane prázdné. Plán → provedení → ověření: stav
/// i pravidla kontroluje backend, okno jen zašedí tlačítko.
#[tauri::command(async)]
pub fn odeber_ovladac(o: tauri::State<'_, Ovladani>, pad: u8) -> Result<(), String> {
    if pad == 0 {
        return Err("Ovladač 1 nejde odebrat.".into());
    }
    let id = PadId::new(usize::from(pad)).ok_or(NENI_OVLADAC)?;
    if o.pady.stav(id.index()).map(|s| s.state) != Some(PadStav::Off) {
        return Err("Nejdřív ho vypni.".into());
    }
    match proved_upravu(&o, Zmena::VymazOvladac(id))? {
        Err(ChybaUpravy::Mapovani(MappingError::WouldBeEmpty)) => {
            Err("Nejdřív dej klávesy jinému ovladači.".into())
        }
        r => {
            text_upravy(r)?;
            log::info!("klávesy: ovladač {} odebrán", pad + 1);
            Ok(())
        }
    }
}

/// Syntetická klávesa pro test okna na skryté ploše (B6): jde do hooku
/// stejnou cestou jako skutečná, ne do OS (žádný `SendInput`). Jen debug
/// build a jen s `KEYPAD_TEST_KLAVESY=1` — release ten příkaz vůbec nemá.
#[cfg(debug_assertions)]
#[tauri::command]
pub fn test_klavesa(
    o: tauri::State<'_, Ovladani>,
    scan: u16,
    e0: bool,
    dolu: bool,
) -> Result<(), String> {
    if std::env::var_os("KEYPAD_TEST_KLAVESY").is_none_or(|v| v != "1") {
        return Err("Testovací klávesy jsou vypnuté.".into());
    }
    let klavesa = keypad_core::KeyId { scan, extended: e0 };
    // VK se v testu nečte: hook má místo stavu klávesnice podvrh.
    posli_hooku(
        &o,
        HookPrikaz::TestKlavesa {
            klavesa,
            vk: 0,
            dolu,
        },
    )
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
    let info = o.pady.stav(0).ok_or(RESTART)?;
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
    use crate::platform::windows::slot::StavSlot;
    use keypad_core::{
        ActionSet, BindingCancel, KeyId, LiveInputs, PadButton, PadState, StickDir, AXIS_MAX,
    };

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

    /// Vstup z okna: ovladače 0–3 a jen kódy z jádra; chyba je věta,
    /// kterou okno ukáže tak, jak je.
    #[test]
    fn cil_z_okna_validuje_pad_a_vstup() {
        for pad in 0..4u8 {
            for a in Action::ALL {
                let c = cil_z_okna(pad, a.code()).unwrap();
                assert_eq!((c.pad.index(), c.action), (usize::from(pad), a));
            }
        }
        assert_eq!(cil_z_okna(4, "a"), Err(NENI_OVLADAC.into()));
        assert_eq!(cil_z_okna(255, "a"), Err(NENI_OVLADAC.into()));
        for spatne in ["", "A", "ls-up", "start ", "win", "x1"] {
            assert_eq!(cil_z_okna(0, spatne), Err(NENI_VSTUP.into()), "{spatne:?}");
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

    fn bez(m: &Mapping, k: KeyId) -> Mapping {
        let mut m = m.clone();
        m.unbind(k).unwrap();
        m
    }

    /// Zrcadlo: starší snímek zahodí, „Zpět" jen na přesného předchůdce
    /// (revize o jedna menší) — po vynechané revizi by vrátilo víc, než
    /// uživatel viděl.
    #[test]
    fn zrcadlo_a_zpet_jen_na_predchudce() {
        let m0 = Mapping::default();
        let m1 = bez(&m0, KeyId::W);
        let m3 = bez(&m1, KeyId::A);
        let mut z = Zrcadlo::novy(m0.clone(), StavKonfigurace::Ok, None, Vec::new());
        assert!(z.lze_vratit().is_none(), "po startu není co vrátit");
        assert!(z.prevezmi(1, Arc::new(m1.clone())));
        assert_eq!(z.lze_vratit().map(|m| (**m).clone()), Some(m0.clone()));
        assert!(!z.prevezmi(1, Arc::new(m3.clone())), "stejná revize");
        assert!(!z.prevezmi(0, Arc::new(m3.clone())), "starší revize");
        assert_eq!(*z.mapovani, m1);
        // Revize 2 se nestihla zrcadlit: „Zpět" z 3 by vrátil dvě změny.
        assert!(z.prevezmi(3, Arc::new(m3.clone())));
        assert!(z.lze_vratit().is_none());
        assert!(z.prevezmi(4, Arc::new(m1.clone())));
        assert_eq!(z.lze_vratit().map(|m| (**m).clone()), Some(m3));
        assert_eq!(z.rev, 4);
    }

    /// Chyby úprav jsou věty pro okno; prázdný vstup není chyba.
    #[test]
    fn texty_uprav() {
        assert_eq!(text_upravy(Ok(())), Ok(()));
        assert_eq!(
            text_upravy(Err(ChybaUpravy::Mapovani(MappingError::NotMapped {
                key: KeyId::W
            }))),
            Ok(())
        );
        assert_eq!(
            text_upravy(Err(ChybaUpravy::Mapovani(MappingError::WouldBeEmpty))),
            Err(POSLEDNI_KLAVESA.into())
        );
        assert_eq!(
            text_upravy(Err(ChybaUpravy::Zastarale)),
            Err(VRATIT_NEJDE.into())
        );
        assert!(text_upravy(Err(ChybaUpravy::Mapovani(MappingError::Empty))).is_err());
    }

    /// Schránka oznámení: nové oznámení jen se změnou pořadí, mezera při
    /// vynechaném pořadí — a oznámení, které okno nedostává, mezeru
    /// nespotřebuje (dostane ji další).
    #[test]
    fn prijem_oznameni_a_mezera() {
        let schranka = |seq: u16, u: UiEvent| u64::from(seq) << 48 | u.pack().unwrap();
        let zruseno = UiEvent::BindingCancelled {
            reason: BindingCancel::Escape,
        };
        let binding = UiEvent::ToggleRejected {
            reason: keypad_core::ToggleReject::Binding,
        };
        let mut p = PrijemOznameni::od(0);
        assert!(p.prijmi(0).is_none(), "nic nového");
        let (o, u) = p.prijmi(schranka(1, zruseno)).unwrap();
        assert_eq!((o.seq, o.mezera, u), (1, false, zruseno));
        assert!(p.prijmi(schranka(1, zruseno)).is_none(), "totéž znovu ne");
        let (o, _) = p.prijmi(schranka(3, zruseno)).unwrap();
        assert!(o.mezera, "2 se ztratilo");
        // Odmítnuté přepnutí při přiřazování okno nedostává…
        assert!(p.prijmi(schranka(5, binding)).is_none());
        // …a mezeru (4) nese další oznámení.
        let (o, _) = p.prijmi(schranka(6, zruseno)).unwrap();
        assert_eq!((o.seq, o.mezera), (6, true));
        let (o, _) = p.prijmi(schranka(7, zruseno)).unwrap();
        assert!(!o.mezera);
        // Pořadí přeteče.
        let mut p = PrijemOznameni::od(0xFFFF << 48);
        let (o, _) = p.prijmi(schranka(0, zruseno)).unwrap();
        assert_eq!((o.seq, o.mezera), (0, false));
        // Nesmyslná schránka: nic neukázat, příště mezera.
        assert!(p.prijmi(1 << 48 | 0b111).is_none());
        let (o, _) = p.prijmi(schranka(2, zruseno)).unwrap();
        assert!(o.mezera);
    }

    fn pad(drzi: u32) -> [ZivyPad; MAX_PADS] {
        let mut p = [ZivyPad::default(); MAX_PADS];
        p[0].drzi = drzi;
        p
    }

    /// Živý stav jde jen se změnou a nejvýš jednou za 16 ms; změna
    /// v rozestupu počká a vlákno se kvůli ní probudí.
    #[test]
    fn zive_jen_zmena_a_rozestup() {
        let t0 = Instant::now();
        let mut z = OdesilaniZive::default();
        assert_eq!(
            z.rozhodni(pad(0), t0),
            None,
            "prázdný na začátku = beze změny"
        );
        assert_eq!(z.probudit_za(t0), None);
        assert_eq!(z.rozhodni(pad(1), t0), Some(pad(1)));
        let t1 = t0 + Duration::from_millis(5);
        assert_eq!(z.rozhodni(pad(3), t1), None, "v rozestupu");
        let za = z.probudit_za(t1).unwrap();
        assert!((11..=12).contains(&za), "{za}");
        // Vrátí se to, co okno už má: nic nečeká.
        assert_eq!(z.rozhodni(pad(1), t1), None);
        assert_eq!(z.probudit_za(t1), None);
        assert_eq!(z.rozhodni(pad(3), t1), None);
        let t2 = t0 + ROZESTUP_ZIVE;
        assert_eq!(z.rozhodni(pad(3), t2), Some(pad(3)));
        assert_eq!(z.probudit_za(t2), None);
        // Schované okno: nic nečeká.
        assert_eq!(z.rozhodni(pad(7), t2), None);
        z.uspi();
        assert_eq!(z.probudit_za(t2), None);
    }

    /// Živý stav pro okno z výstupu hooku: držené vstupy z atomiků,
    /// „hra" ze slotu padu.
    #[test]
    fn zivy_stav_z_vystupu_hooku() {
        let sloty: [Arc<StavSlot>; MAX_PADS] =
            std::array::from_fn(|_| Arc::new(StavSlot::new().unwrap()));
        let v = HookVystup::new(sloty.clone(), Arc::new(Budik::new().unwrap()));
        let mut drzi = ActionSet::EMPTY;
        drzi.insert(Action::LeftStick(StickDir::Left));
        drzi.insert(Action::LeftStick(StickDir::Right));
        drzi.insert(Action::Button(PadButton::A));
        let mut z = [LiveInputs::EMPTY; MAX_PADS];
        z[1] = LiveInputs::new(drzi, (1, 0), (0, 0));
        v.zive(&z);
        sloty[1].zapis(PadState {
            thumb_lx: AXIS_MAX,
            buttons: PadButton::A.mask(),
            ..PadState::NEUTRAL
        });
        let pady = smlouva::zive_pady(v.zive_vstupy(), v.hra());
        assert_eq!(pady[0], ZivyPad::default());
        let mut hra = ActionSet::EMPTY;
        hra.insert(Action::LeftStick(StickDir::Right));
        hra.insert(Action::Button(PadButton::A));
        assert_eq!(
            pady[1],
            ZivyPad {
                drzi: drzi.bits(),
                hra: hra.bits(),
                l: [1, 0],
                p: [0, 0],
            }
        );
    }
}

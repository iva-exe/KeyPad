//! Zvuk při pozastavení a návratu do hry (Fáze 4b, přepracováno ve
//! Fázi 7).
//!
//! Během hry je okno schované a ikona v oznamovací oblasti bývá
//! v přetečení nebo pod hrou přes celou obrazovku — pozastavení Scroll
//! Lockem by jinak nešlo poznat. Zvuk nic nekreslí přes hru (princip 8)
//! a hráč ho slyší, aniž by hledal lištu.
//!
//! WASAPI, ne `PlaySoundW`: winmm hraje přes starou cestu zvukových
//! ovladačů (Drivers32) a `wdmaud.drv` si načte i ze složky programu —
//! sonda revize to ukázala i po `SetDefaultDllDirectories(SYSTEM32)`
//! a s winmm.dll načtenou jen ze System32. KeyPad.exe spuštěný ze
//! Stažených souborů by tak natáhl DLL, kterou mu tam kdokoli podstrčí.
//! winmm.dll proto KeyPad.exe nesmí importovat vůbec — hlídá
//! `tools\check-imports.ps1`.
//!
//! Ani WASAPI ale není bezpečná sama od sebe. COM načte podle absolutní
//! cesty z registru jen MMDevApi.dll (enumerátor zařízení); AudioSes.dll
//! si MMDevAPI při `IMMDevice::Activate` dotahuje JMÉNEM (zpožděný
//! import), tedy podle pořadí hledání DLL procesu. Před kopií vedle
//! KeyPad.exe chrání jen `SetDefaultDllDirectories(SYSTEM32)` jako první
//! příkaz `main` (`dll.rs`) — sonda revize: bez něj si MMDevAPI vzala
//! podvrženou AudioSes.dll ze složky programu, s ním nenačetla nic ani
//! ze 70 podvržených jmen. Na rozdíl od winmm → wdmaud.drv tahle cesta
//! omezené hledání respektuje. Proto se zvuk bez `dll::omezeno()` vůbec
//! neotevře (radši ticho). Statický import WASAPI nepřidá žádný (jen
//! funkce COM z ole32, KnownDLL, kterou KeyPad.exe importuje i bez
//! zvuku); čekání, priorita vlákna a čas jsou z kernel32.
//!
//! Sdílený režim s výchozí relací: zvuk je ve Směšovači hlasitosti vidět
//! jako KeyPad (jde ztlumit zvlášť) a neumlčí ho vypnuté systémové
//! zvuky. Vypnout ho jde zaškrtnutím „Zvuk“.
//!
//! **Fáze 7** (vlastník: „zvuk je krátký, náhodně se mění vyšší a nižší
//! tón … občas se spustí dohromady, zvuky nejsou smooth“):
//! - Dva zvuky jasně odlišné rejstříkem i počtem nástupů ([`Zvuk`]) —
//!   dřív oba dvojtóny z týchž dvou výšek v opačném pořadí, směr se za
//!   0,16 s špatně slyšel. Měkký náběh, exponenciální doznění a kosinový
//!   doběh na přesnou nulu; počítají se přímo ve vzorkování směšovače.
//! - Nikdy dva přes sebe ani těsně za sebou: [`Planovac`] (čistý
//!   automat) rozehraný zvuk při novém požadavku plynule dohasí (15 ms),
//!   nechá 50 ms ticha a zahraje nový. Dřív se rozehraný dohrál celý
//!   a čekající zazněl hned za ním — „pauza“ a „hra“ se slily v jednu
//!   melodii. Kvůli doběhu se buffer neplní celý předem, ale po periodách
//!   zařízení (událost WASAPI) s předstihem 50 ms: data, která už jsou ve
//!   frontě zařízení, nejdou vzít zpět.
//! - Předehřátí ([`priprav`]) při prvním zapnutí ovladače: knihovny
//!   zvuku a vzorky jsou hotové dřív, než hráč poprvé zmáčkne Scroll Lock
//!   (log vlastníka: první otevření 408 ms, první zvuk pak přišel pozdě
//!   a slil se s dalším).
//! - Hrají vlastní zvuky vlastníka (WAV vestavěné do binárky, [`VLASTNI`]);
//!   syntetizované jsou záloha, kdyby je dekodér nepřijal.
//!
//! Klient zařízení se otevírá jen na dobu hraní a pak se zavře: nečinný
//! KeyPad nedrží otevřený zvukový výstup a audio engine kvůli němu
//! nemíchá prázdný proud (princip 10). Přehrává vlastní malé vlákno —
//! vlákno okna, které o zvuku rozhoduje, hlídá tep ovladačů a nesmí na
//! nic čekat.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    CloseHandle, HANDLE, RPC_E_CHANGED_MODE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, WAVEFORMATEX,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentThread, ResetEvent, SetEvent, SetThreadPriority,
    WaitForMultipleObjects, WaitForSingleObject, INFINITE, THREAD_PRIORITY_HIGHEST,
    THREAD_PRIORITY_NORMAL,
};

use super::dll;

/// Který zvuk ohlašuje změnu režimu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zvuk {
    /// Klávesy teď jdou do Windows: jeden měkký nižší tón (G4).
    Pauza,
    /// Klávesy zase ovládají ovladač: jasná stoupající trojice C5–E5–G5.
    Hra,
}

// ── Parametry (ROADMAP, Fáze 7, Z1) ──────────────────────────────────

/// Špička. Tichý signál: systémové zvuky Windows mívají kolem −6 dBFS
/// a KeyPad nemá přehlušit hru ani hlasový chat.
const SPICKA_DBFS: f64 = -14.0;
/// Délka syntetizovaného zvuku. Dřív 170 ms — vlastník: „zvuk je krátký“;
/// víc než 0,4 s by už zdržovalo, když hráč pozastavuje často.
const DELKA_MS: u32 = 360;
/// Kosinový doběh konce syntetizovaného zvuku na přesnou nulu.
const DOBEH_KONCE_MS: u32 = 40;
/// Ticho za posledním zvukem, než se klient zastaví. Poslední
/// milisekundy ještě míří do zařízení, takže na konci musí být digitální
/// ticho, ne doznívající tón. Aspoň [`MEZERA_MS`]: zavřený výstup na
/// mezeru zapomene (nový proud hraje první zvuk hned) — s kratším
/// dozvukem by zvuk těsně po zavření přišel dřív než za mezeru
/// (dřív 30 ms, nalezeno revizí).
const DOZVUK_MS: u32 = 50;
/// Doběh rozehraného zvuku při novém požadavku. Useknutá sinusovka by
/// lupla; delší doběh by zdržel zvuk, který ohlašuje skutečný stav.
const PRERUSENI_MS: u32 = 15;
/// Ticho mezi doběhem (nebo koncem) zvuku a dalším zvukem — bez něj by
/// se dva zvuky slily v jednu melodii („spustí se dohromady“).
const MEZERA_MS: u32 = 50;

// Mezera platí i přes zavření výstupu (viz DOZVUK_MS) — hlídá překladač.
const _: () = assert!(DOZVUK_MS >= MEZERA_MS);
/// Kolik držet ve frontě zařízení. Doběh začne nejpozději o tolik po
/// požadavku (co je ve frontě, nejde vzít zpět); méně by pod zátěží hry
/// hrozilo podtečení (vlákno zvuku nestihne dopsat).
const PREDSTIH_MS: u32 = 50;
/// Velikost bufferu, o kterou se klient žádá — s rezervou nad předstih.
const BUFFER_MS: u32 = 100;

// Předstih 40–60 ms (specifikace Z1) a buffer s rezervou nad ním —
// hlídá překladač, ne až test.
const _: () = assert!(PREDSTIH_MS >= 40 && PREDSTIH_MS <= 60 && BUFFER_MS > PREDSTIH_MS);
/// Ticho mezi Pauzou a Hrou v ukázce (▷ v nastavení).
const UKAZKA_TICHO_MS: u32 = 600;
/// Nejdelší vlastní zvuk (WAV); plánovač s ním počítá. Zvuky vlastníka
/// (6. 10. 2026) doznívají 0,57 a 0,72 s — dozvuk je jejich součást
/// („zvuk je krátký“), useknout ho by znělo hůř než syntetizované.
const MAX_ZVUK_MS: u32 = 800;
/// Z kolika prvních ms se porovnává hlasitost (RMS) dvojice zvuků.
const RMS_OKNO_MS: u32 = 250;
/// Náběh a doběh přidaný vlastnímu WAV: nic nelupne ani u souboru
/// s useknutým koncem.
const WAV_NABEH_MS: u32 = 2;
const WAV_DOBEH_MS: u32 = 10;

/// Hlas syntetizovaného zvuku: kdy začne a jakou výškou.
struct Hlas {
    nastup_ms: f64,
    vyska: Vyska,
}

#[derive(Clone, Copy)]
enum Vyska {
    Ton(f64),
    /// Plynulý přechod `od` → `na` za `ms` (kosinová křivka, spojitá fáze).
    Glissando {
        od: f64,
        na: f64,
        ms: f64,
    },
}

impl Vyska {
    fn hz(self, t_ms: f64) -> f64 {
        match self {
            Vyska::Ton(f) => f,
            Vyska::Glissando { od, na, ms } if t_ms < ms => {
                na + (od - na) * (0.5 + 0.5 * (std::f64::consts::PI * t_ms / ms).cos())
            }
            Vyska::Glissando { na, .. } => na,
        }
    }

    /// Nejvyšší frekvence (pro mez kroku mezi vzorky v testech).
    #[cfg(test)]
    fn max_hz(self) -> f64 {
        match self {
            Vyska::Ton(f) => f,
            Vyska::Glissando { od, na, .. } => od.max(na),
        }
    }
}

/// Recept syntetizovaného zvuku.
struct Recept {
    hlasy: &'static [Hlas],
    /// Druhá a třetí harmonická v dB vůči základu. Malé reproduktory notebooku
    /// základ kolem 400 Hz skoro nehrají — výšku ucho „domyslí“ z vyšších
    /// harmonických; čistý sinus navíc zní tvrdě a „elektronicky“.
    harmonicke_db: [f64; 2],
    /// Náběh (zvýšený kosinus): kratší lupne, delší zní líně.
    nabeh_ms: f64,
    /// Časová konstanta exponenciálního doznění.
    tau_ms: f64,
}

/// Hra: jasná stoupající trojice (rychlé arpeggio), každý tón doznívá
/// do dalšího. Jiná než dvojtón připojení zařízení ve Windows.
const HRA: Recept = Recept {
    hlasy: &[
        Hlas {
            nastup_ms: 0.0,
            vyska: Vyska::Ton(523.25),
        },
        Hlas {
            nastup_ms: 70.0,
            vyska: Vyska::Ton(659.26),
        },
        Hlas {
            nastup_ms: 140.0,
            vyska: Vyska::Ton(783.99),
        },
    ],
    harmonicke_db: [-12.0, -22.0],
    nabeh_ms: 12.0,
    tau_ms: 110.0,
};

/// Pauza: jeden měkký nižší tón (zvon/marimba), bez glissanda. Od Hry se
/// liší rejstříkem a jedním nástupem místo tří.
const PAUZA: Recept = Recept {
    hlasy: &[Hlas {
        nastup_ms: 0.0,
        vyska: Vyska::Ton(392.0),
    }],
    harmonicke_db: [-8.0, -24.0],
    nabeh_ms: 15.0,
    tau_ms: 170.0,
};

/// Varianta Pauzy s glissandem A4 → G4 jen k poslechu
/// (`zvuk_selftest -- pauza-glis`); do aplikace jen, když ji vlastník
/// vybere — mohla by znít rozladěně.
const PAUZA_GLIS: Recept = Recept {
    hlasy: &[Hlas {
        nastup_ms: 0.0,
        vyska: Vyska::Glissando {
            od: 440.0,
            na: 392.0,
            ms: 180.0,
        },
    }],
    harmonicke_db: PAUZA.harmonicke_db,
    nabeh_ms: PAUZA.nabeh_ms,
    tau_ms: PAUZA.tau_ms,
};

/// Vlastní zvuky vestavěné do KeyPad.exe: `(hra.wav, pauza.wav)` —
/// od vlastníka (`continue_pad.wav`, `pause_pad.wav`, 6. 10. 2026),
/// v repu oříznuté za posledním vzorkem nad −60 dBFS s 30ms doběhem
/// a bez metadat. Za běhu se z disku nic nečte — podvržený soubor vedle
/// exe nic nezmění. Formát pro nové zvuky je v ROADMAP („Vlastní zvuky“).
///
/// Soubor, který dekodér nepřijme, znamená jeden řádek v logu
/// a syntetizované zvuky — nikdy zkreslený zvuk.
const VLASTNI: Option<(&[u8], &[u8])> = Some((
    include_bytes!("../../../zvuky/hra.wav"),
    include_bytes!("../../../zvuky/pauza.wav"),
));

/// Strop velikosti vestavěného zvuku (princip 10: binárka nemá kynout;
/// 0,8 s mono 16 bit 48 kHz je 77 KB).
const MAX_WAV_BAJTU: usize = 100 * 1024;

const fn vlastni_se_vejdou(v: Option<(&[u8], &[u8])>) -> bool {
    match v {
        None => true,
        Some((h, p)) => h.len() <= MAX_WAV_BAJTU && p.len() <= MAX_WAV_BAJTU,
    }
}

const _: () = assert!(vlastni_se_vejdou(VLASTNI), "vlastní zvuk má nejvýš 100 KB");

// ── Syntéza (čisté funkce, testují se bez přehrávání) ────────────────

fn pocet_vzorku(ms: u32, vzorkovani: u32) -> usize {
    (u64::from(vzorkovani) * u64::from(ms) / 1000) as usize
}

fn z_db(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Lineární špička, na kterou se zvuky srovnávají.
fn spicka_cil() -> f64 {
    z_db(SPICKA_DBFS)
}

/// Obálka hlasu `t_ms` po jeho nástupu: zvýšený kosinus 0 → 1 za
/// `nabeh_ms`, pak exponenciální doznění. Spojitá i v místě přechodu.
fn obalka(t_ms: f64, nabeh_ms: f64, tau_ms: f64) -> f64 {
    if t_ms < nabeh_ms {
        0.5 - 0.5 * (std::f64::consts::PI * t_ms / nabeh_ms).cos()
    } else {
        (-(t_ms - nabeh_ms) / tau_ms).exp()
    }
}

/// Zisk `k`-tého z `n` vzorků kosinového doběhu: začíná těsně pod 1,
/// poslední vzorek je přesně nula (žádný skok = žádné lupnutí).
fn kosinovy_dobeh(k: usize, n: usize) -> f64 {
    if k + 1 >= n {
        return 0.0;
    }
    0.5 * (1.0 + (std::f64::consts::PI * (k + 1) as f64 / n as f64).cos())
}

/// Nenormalizované vzorky receptu ve vzorkování `vz`: součet hlasů, každý
/// základ, druhá a třetí harmonická se svou obálkou; posledních
/// [`DOBEH_KONCE_MS`] kosinový doběh. Délka [`DELKA_MS`], první
/// i poslední vzorek přesně nula.
fn syntetizuj(r: &Recept, vz: u32) -> Vec<f64> {
    let n = pocet_vzorku(DELKA_MS, vz);
    let vz_f = f64::from(vz);
    let (a2, a3) = (z_db(r.harmonicke_db[0]), z_db(r.harmonicke_db[1]));
    let mut v = vec![0.0f64; n];
    for hlas in r.hlasy {
        let start = (hlas.nastup_ms * vz_f / 1000.0).round() as usize;
        // Fáze se sčítá (ne t·f): glissando pak nemá skok a v f64 se
        // nerozchází ani při 96 kHz.
        let mut faze = 0.0f64;
        for (i, x) in v.iter_mut().enumerate().skip(start) {
            let t = (i - start) as f64 * 1000.0 / vz_f;
            let e = obalka(t, r.nabeh_ms, r.tau_ms);
            *x += e * (faze.sin() + a2 * (2.0 * faze).sin() + a3 * (3.0 * faze).sin());
            faze += std::f64::consts::TAU * hlas.vyska.hz(t) / vz_f;
            if faze >= std::f64::consts::TAU {
                faze -= std::f64::consts::TAU;
            }
        }
    }
    let d = pocet_vzorku(DOBEH_KONCE_MS, vz).min(n);
    for (k, x) in v[n - d..].iter_mut().enumerate() {
        *x *= kosinovy_dobeh(k, d);
    }
    v
}

/// Jeden zvuk ve vzorkování směšovače, plná škála ±1: levý kanál
/// a pravý (prázdný = mono).
#[derive(Clone, Debug, Default, PartialEq)]
struct Zaznam {
    l: Vec<f32>,
    p: Vec<f32>,
}

impl Zaznam {
    fn mono(l: Vec<f32>) -> Zaznam {
        Zaznam { l, p: Vec::new() }
    }

    fn z_f64(v: &[f64]) -> Zaznam {
        Zaznam::mono(v.iter().map(|&x| x as f32).collect())
    }

    fn delka(&self) -> usize {
        self.l.len()
    }

    fn ramec(&self, i: usize) -> [f32; 2] {
        let l = self.l[i];
        [l, if self.p.is_empty() { l } else { self.p[i] }]
    }

    fn kanaly(&self) -> impl Iterator<Item = &f32> {
        self.l.iter().chain(self.p.iter())
    }

    fn spicka(&self) -> f64 {
        self.kanaly().fold(0f64, |s, &x| s.max(f64::from(x).abs()))
    }

    /// Žádný vzorek není nekonečno ani NaN.
    fn konecny(&self) -> bool {
        self.kanaly().all(|x| x.is_finite())
    }

    /// RMS prvních `n` rámců přes oba kanály.
    fn rms(&self, n: usize) -> f64 {
        let n = n.min(self.delka());
        if n == 0 {
            return 0.0;
        }
        let mut soucet = 0f64;
        let mut pocet = 0usize;
        for kanal in [&self.l, &self.p] {
            if kanal.is_empty() {
                continue;
            }
            soucet += kanal[..n]
                .iter()
                .map(|&x| f64::from(x).powi(2))
                .sum::<f64>();
            pocet += n;
        }
        (soucet / pocet as f64).sqrt()
    }

    fn zesil(&mut self, g: f64) {
        for x in self.l.iter_mut().chain(self.p.iter_mut()) {
            *x = (f64::from(*x) * g) as f32;
        }
    }

    /// Náběh (zvýšený kosinus) přes `nabeh` vzorků a kosinový doběh na
    /// přesnou nulu přes posledních `dobeh`.
    fn okraje(&mut self, nabeh: usize, dobeh: usize) {
        let n = self.delka();
        let (nabeh, dobeh) = (nabeh.min(n), dobeh.min(n));
        for kanal in [&mut self.l, &mut self.p] {
            if kanal.is_empty() {
                continue;
            }
            for (k, x) in kanal.iter_mut().take(nabeh).enumerate() {
                let g = 0.5 - 0.5 * (std::f64::consts::PI * k as f64 / nabeh as f64).cos();
                *x = (f64::from(*x) * g) as f32;
            }
            for (k, x) in kanal[n - dobeh..].iter_mut().enumerate() {
                *x = (f64::from(*x) * kosinovy_dobeh(k, dobeh)) as f32;
            }
        }
    }
}

/// Srovná hlasitost dvojice: špička obou nejvýš [`SPICKA_DBFS`] a stejné
/// RMS prvních [`RMS_OKNO_MS`] — ztlumí se hlasitější. Pauza nese
/// důležitější zprávu (klávesy jdou zase Windows), tišší být nesmí;
/// hra víc nástupy při stejné špičce zní jinak hlasitě než jeden tón.
/// `zesilit` = tichý zvuk se dorovná na špičku (syntetizované); vlastní
/// WAV se jen tlumí.
fn vyrovnej(a: &mut Zaznam, b: &mut Zaznam, vz: u32, zesilit: bool) {
    let cil = spicka_cil();
    let okno = pocet_vzorku(RMS_OKNO_MS, vz);
    let zisk = |z: &Zaznam| {
        let s = z.spicka();
        if s == 0.0 {
            1.0
        } else if zesilit {
            cil / s
        } else {
            (cil / s).min(1.0)
        }
    };
    let (mut ga, mut gb) = (zisk(a), zisk(b));
    let (ra, rb) = (a.rms(okno) * ga, b.rms(okno) * gb);
    if ra > 0.0 && rb > 0.0 {
        if ra > rb {
            ga *= rb / ra;
        } else {
            gb *= ra / rb;
        }
    }
    a.zesil(ga);
    b.zesil(gb);
}

/// Který záznam sady.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Id {
    Pauza,
    Hra,
    Glis,
}

/// Úsek položky plánovače.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Usek {
    Zaznam(Id),
    Ticho(u32),
}

/// Zvuky připravené ve vzorkování směšovače.
struct Sada {
    vzorkovani: u32,
    hra: Zaznam,
    pauza: Zaznam,
    /// Varianta s glissandem — počítá se, až když o ni někdo požádá.
    glis: Option<Zaznam>,
    /// Nulové vzorky stejné délky (`zvuk_selftest -- ticho`).
    ticha: bool,
    vlastni: bool,
}

/// Vlastní zvuky se nepovedly — hlásí se jen jednou za běh.
static VLASTNI_HLASENO: AtomicBool = AtomicBool::new(false);

impl Sada {
    fn synteticka(vz: u32) -> Sada {
        let mut hra = Zaznam::z_f64(&syntetizuj(&HRA, vz));
        let mut pauza = Zaznam::z_f64(&syntetizuj(&PAUZA, vz));
        vyrovnej(&mut hra, &mut pauza, vz, true);
        Sada {
            vzorkovani: vz,
            hra,
            pauza,
            glis: None,
            ticha: false,
            vlastni: false,
        }
    }

    /// Ticho přesně stejně dlouhé jako skutečné zvuky — sonda podvržených
    /// DLL projde celou cestou se stejným časováním a nic není slyšet.
    fn ticha(vz: u32) -> Sada {
        let nuly = || Zaznam::mono(vec![0.0; pocet_vzorku(DELKA_MS, vz)]);
        Sada {
            vzorkovani: vz,
            hra: nuly(),
            pauza: nuly(),
            glis: Some(nuly()),
            ticha: true,
            vlastni: false,
        }
    }

    /// Vlastní zvuky z bajtů WAV (vestavěných), převedené do vzorkování
    /// `vz` a srovnané jako syntetizované.
    fn z_wav(hra: &[u8], pauza: &[u8], vz: u32) -> Result<Sada, String> {
        let mut h = zaznam_z_wav(hra, vz).map_err(|e| format!("hra.wav: {e}"))?;
        let mut p = zaznam_z_wav(pauza, vz).map_err(|e| format!("pauza.wav: {e}"))?;
        vyrovnej(&mut h, &mut p, vz, false);
        // Pojistka za dekodérem: do zařízení nikdy nic nekonečného ani
        // NaN (`do_formatu` floaty jen kopíruje) — pak hrají syntetizované.
        for (nazev, z) in [("hra.wav", &h), ("pauza.wav", &p)] {
            if !z.konecny() {
                return Err(format!("{nazev}: vzorky po převodu nejsou konečná čísla"));
            }
        }
        Ok(Sada {
            vzorkovani: vz,
            hra: h,
            pauza: p,
            glis: None,
            ticha: false,
            vlastni: true,
        })
    }

    /// Zvuky aplikace: vlastní, jsou-li vestavěné a platné, jinak
    /// syntetizované.
    fn pro(vz: u32) -> Sada {
        if let Some((hra, pauza)) = VLASTNI {
            match Sada::z_wav(hra, pauza, vz) {
                Ok(s) => return s,
                Err(e) => {
                    if !VLASTNI_HLASENO.swap(true, Ordering::AcqRel) {
                        log::warn!("zvuk: vlastní zvuky nejdou použít ({e}) — hrají syntetizované");
                    }
                }
            }
        }
        Sada::synteticka(vz)
    }

    fn zaznam(&self, id: Id) -> &Zaznam {
        match id {
            Id::Pauza => &self.pauza,
            Id::Hra => &self.hra,
            Id::Glis => self.glis.as_ref().unwrap_or(&self.pauza),
        }
    }

    fn delka(&self, u: Usek) -> usize {
        match u {
            Usek::Zaznam(id) => self.zaznam(id).delka(),
            Usek::Ticho(ms) => pocet_vzorku(ms, self.vzorkovani),
        }
    }

    fn ramec(&self, u: Usek, i: usize) -> [f32; 2] {
        match u {
            Usek::Zaznam(id) => self.zaznam(id).ramec(i),
            Usek::Ticho(_) => [0.0; 2],
        }
    }

    /// Dopočítá záznam, který položka potřebuje a sada ještě nemá.
    fn zajisti(&mut self, p: Polozka) {
        if p != Polozka::Glis || self.glis.is_some() {
            return;
        }
        let vz = self.vzorkovani;
        let mut g = Zaznam::z_f64(&syntetizuj(&PAUZA_GLIS, vz));
        let s = g.spicka();
        if s > 0.0 {
            g.zesil(spicka_cil() / s);
        }
        let okno = pocet_vzorku(RMS_OKNO_MS, vz);
        let (r, cil) = (g.rms(okno), self.hra.rms(okno));
        if r > cil && cil > 0.0 {
            g.zesil(cil / r);
        }
        self.glis = Some(g);
    }

    fn popis(&self) -> &'static str {
        if self.ticha {
            "nulové vzorky"
        } else if self.vlastni {
            "vlastní zvuky"
        } else {
            "syntetizované zvuky"
        }
    }
}

// ── Vlastní zvuky: WAV (čisté funkce) ────────────────────────────────

/// Dekódovaný WAV: vzorky ±1 po kanálech, `p` prázdné u mona.
#[derive(Debug, PartialEq)]
struct Wav {
    vzorkovani: u32,
    l: Vec<f32>,
    p: Vec<f32>,
}

/// Hlavička `WAVEFORMATEX` / `WAVEFORMATEXTENSIBLE` (směšovač i blok
/// `fmt ` souboru WAV).
struct Hlavicka {
    /// Tag formátu; u EXTENSIBLE tag z podformátu.
    druh: u16,
    kanaly: u16,
    vzorkovani: u32,
    blok: u16,
    bity: u16,
    /// Platné bity (jen EXTENSIBLE).
    platne: Option<u16>,
}

fn cti_hlavicku(b: &[u8]) -> Option<Hlavicka> {
    let u16_na = |i: usize| Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]));
    let u32_na = |i: usize| {
        Some(u32::from_le_bytes([
            *b.get(i)?,
            *b.get(i + 1)?,
            *b.get(i + 2)?,
            *b.get(i + 3)?,
        ]))
    };
    let tag = u16_na(0)?;
    let (druh, platne) = if tag == TAG_EXTENSIBLE {
        // cbSize ≥ 22: platné bity, maska kanálů, podformát (GUID).
        if u16_na(16)? < 22 || b.get(24 + 4..40)? != PODFORMAT_KONEC {
            return None;
        }
        (u16::try_from(u32_na(24)?).ok()?, Some(u16_na(18)?))
    } else {
        (tag, None)
    };
    Some(Hlavicka {
        druh,
        kanaly: u16_na(2)?,
        vzorkovani: u32_na(4)?,
        blok: u16_na(12)?,
        bity: u16_na(14)?,
        platne,
    })
}

/// Největší absolutní hodnota vzorku float WAV. Plná škála je ±1; malá
/// rezerva pro editory, které po normalizaci zaokrouhlí těsně nad.
const MAX_WAV_VZOREK: f32 = 1.0 + 1e-3;

/// WAV (RIFF/WAVE): blok `fmt ` PCM 16 bit nebo IEEE float 32 bit (i jako
/// `WAVE_FORMAT_EXTENSIBLE`; vzorky v ±1), 1–2 kanály, 8–192 kHz; neznámé
/// bloky (`LIST`, `bext`…) přeskočí. Cokoli jiného — i useknutý soubor,
/// nesmyslná délka bloku nebo float mimo plnou škálu — `None`: radši
/// syntetizovaný zvuk než šum.
fn dekoduj_wav(b: &[u8]) -> Option<Wav> {
    if b.get(0..4)? != b"RIFF" || b.get(8..12)? != b"WAVE" {
        return None;
    }
    let (mut fmt, mut data) = (None, None);
    let mut i = 12usize;
    while let Some(hlava) = b.get(i..i + 8) {
        let delka = u32::from_le_bytes([hlava[4], hlava[5], hlava[6], hlava[7]]) as usize;
        let telo = b.get(i + 8..(i + 8).checked_add(delka)?)?;
        match &hlava[..4] {
            b"fmt " if fmt.is_none() => fmt = Some(telo),
            b"data" if data.is_none() => data = Some(telo),
            _ => {}
        }
        // Bloky liché délky mají za sebou výplňový bajt.
        i += 8 + delka + (delka & 1);
    }
    let h = cti_hlavicku(fmt?)?;
    let typ = match (h.druh, h.bity) {
        (TAG_PCM, 16) => TypVzorku::I16,
        (TAG_FLOAT, 32) => TypVzorku::F32,
        _ => return None,
    };
    // 24 platných bitů ve 32bitovém kontejneru a podobné — neumíme.
    if h.platne.is_some_and(|p| p != h.bity)
        || !(1..=2).contains(&h.kanaly)
        || !(8_000..=192_000).contains(&h.vzorkovani)
    {
        return None;
    }
    let blok = usize::from(h.kanaly) * typ.bajtu();
    if usize::from(h.blok) != blok {
        return None;
    }
    let data = data?;
    let ramcu = data.len() / blok;
    if ramcu == 0 {
        return None;
    }
    let stereo = h.kanaly == 2;
    let mut l = Vec::with_capacity(ramcu);
    let mut p = Vec::with_capacity(if stereo { ramcu } else { 0 });
    for ramec in data.chunks_exact(blok) {
        for (k, s) in ramec.chunks_exact(typ.bajtu()).enumerate() {
            let v = match typ {
                TypVzorku::I16 => f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0,
                _ => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            };
            // Float mimo plnou škálu odmítnout, ne jen nekonečno: obří
            // konečný vzorek (3e38) by převzorkování (překmit až 1,25×)
            // posunulo do ±inf a srovnání hlasitosti udělalo NaN —
            // do zařízení by šel šum (nalezeno revizí).
            if !v.is_finite() || v.abs() > MAX_WAV_VZOREK {
                return None;
            }
            if k == 0 {
                l.push(v);
            } else {
                p.push(v);
            }
        }
    }
    Some(Wav {
        vzorkovani: h.vzorkovani,
        l,
        p,
    })
}

/// Převzorkování `z` → `na` Hz kubickou interpolací (Catmull–Rom). Délka
/// se zachová ±1 vzorek. Bez dolní propusti — při zmenšení vzorkování by
/// obsah nad novou Nyquistovou frekvencí zrcadlil; vlastník dodá 48 kHz
/// (ROADMAP), takže se to týká jen souborů mimo doporučení.
fn prevzorkuj(x: &[f32], z: u32, na: u32) -> Vec<f32> {
    if z == na || z == 0 || na == 0 {
        return x.to_vec();
    }
    let n = ((x.len() as u64 * u64::from(na) + u64::from(z) / 2) / u64::from(z)) as usize;
    let krok = f64::from(z) / f64::from(na);
    let v = |i: isize| -> f64 {
        usize::try_from(i)
            .ok()
            .and_then(|i| x.get(i))
            .map_or(0.0, |&s| f64::from(s))
    };
    (0..n)
        .map(|j| {
            let poloha = j as f64 * krok;
            let i = poloha.floor() as isize;
            let t = poloha - i as f64;
            let (p0, p1, p2, p3) = (v(i - 1), v(i), v(i + 1), v(i + 2));
            let y = 0.5
                * (2.0 * p1
                    + (p2 - p0) * t
                    + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t
                    + (3.0 * (p1 - p2) + p3 - p0) * t * t * t);
            y as f32
        })
        .collect()
}

/// Vlastní WAV jako záznam ve vzorkování `vz`: digitální ticho na konci
/// pryč (ticho za zvukem přidá plánovač), nejvýš [`MAX_ZVUK_MS`],
/// náběh a doběh proti lupnutí.
fn zaznam_z_wav(b: &[u8], vz: u32) -> Result<Zaznam, String> {
    let w = dekoduj_wav(b)
        .ok_or("formát neumím (jen WAV PCM 16 bit nebo float 32 bit, 1–2 kanály, 8–192 kHz)")?;
    let ticho = |i: usize| w.l[i] == 0.0 && w.p.get(i).is_none_or(|&x| x == 0.0);
    let mut delka = w.l.len();
    while delka > 0 && ticho(delka - 1) {
        delka -= 1;
    }
    if delka == 0 {
        return Err("samé ticho".into());
    }
    if delka as u64 * 1000 > u64::from(MAX_ZVUK_MS) * u64::from(w.vzorkovani) {
        return Err(format!("zvuk je delší než {MAX_ZVUK_MS} ms"));
    }
    let l = prevzorkuj(&w.l[..delka], w.vzorkovani, vz);
    let p = if w.p.is_empty() {
        Vec::new()
    } else {
        prevzorkuj(&w.p[..delka], w.vzorkovani, vz)
    };
    let mut z = Zaznam { l, p };
    z.okraje(
        pocet_vzorku(WAV_NABEH_MS, vz),
        pocet_vzorku(WAV_DOBEH_MS, vz),
    );
    Ok(z)
}

// ── Plánovač (čistý automat) ─────────────────────────────────────────

/// Co plánovač přehraje jako jeden celek.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Polozka {
    Zvuk(Zvuk),
    /// Ukázka z nastavení (▷): Pauza, ticho a Hra. Pro plánovač JEDNA
    /// položka — skutečný požadavek ji zruší celou, i s čekající Hrou;
    /// jinak by zazněly dva zvuky po sobě, nebo zvuk, který neodpovídá
    /// stavu.
    Ukazka,
    /// Varianta Pauzy s glissandem, jen k poslechu.
    Glis,
    /// Nic nehrát: rozehrané plynule dohasne, čekající se zahodí
    /// ([`zastav`] — vypnutý ✓ Zvuk, konec aplikace). Nikdy nehraje
    /// a sama výstup neotevře.
    Ztis,
}

impl Polozka {
    fn kod(self) -> u64 {
        match self {
            Polozka::Zvuk(Zvuk::Pauza) => 1,
            Polozka::Zvuk(Zvuk::Hra) => 2,
            Polozka::Ukazka => 3,
            Polozka::Glis => 4,
            Polozka::Ztis => 5,
        }
    }

    fn z_kodu(k: u64) -> Option<Polozka> {
        match k {
            1 => Some(Polozka::Zvuk(Zvuk::Pauza)),
            2 => Some(Polozka::Zvuk(Zvuk::Hra)),
            3 => Some(Polozka::Ukazka),
            4 => Some(Polozka::Glis),
            5 => Some(Polozka::Ztis),
            _ => None,
        }
    }

    fn useky(self) -> &'static [Usek] {
        match self {
            Polozka::Zvuk(Zvuk::Pauza) => &[Usek::Zaznam(Id::Pauza)],
            Polozka::Zvuk(Zvuk::Hra) => &[Usek::Zaznam(Id::Hra)],
            Polozka::Glis => &[Usek::Zaznam(Id::Glis)],
            Polozka::Ukazka => &[
                Usek::Zaznam(Id::Pauza),
                Usek::Ticho(UKAZKA_TICHO_MS),
                Usek::Zaznam(Id::Hra),
            ],
            // Nehraje nikdy — plánovač ji vyřídí v `pozadavek`.
            Polozka::Ztis => &[],
        }
    }

    fn nazev(self) -> &'static str {
        match self {
            Polozka::Zvuk(Zvuk::Pauza) => "Pauza",
            Polozka::Zvuk(Zvuk::Hra) => "Hra",
            Polozka::Ukazka => "ukázka",
            Polozka::Glis => "Pauza s glissandem",
            Polozka::Ztis => "ztišení",
        }
    }
}

/// Rozehraná položka.
struct Hraje {
    polozka: Polozka,
    /// Zbývající úseky; „stejný požadavek“ za běhu ukázky je zkrátí na ten
    /// rozehraný.
    useky: &'static [Usek],
    usek: usize,
    /// Rámců úseku už venku.
    pozice: usize,
    /// Doběh: kolik jeho rámců už je venku (`None` = hraje naplno).
    dobeh: Option<usize>,
}

impl Hraje {
    fn new(p: Polozka) -> Hraje {
        Hraje {
            polozka: p,
            useky: p.useky(),
            usek: 0,
            pozice: 0,
            dobeh: None,
        }
    }

    /// Rozehraný (ne dohasínající) úsek už ohlašuje stav, který
    /// požadavek `p` chce ohlásit.
    fn uz_hlasi(&self, p: Polozka) -> bool {
        if self.dobeh.is_some() {
            return false;
        }
        match (self.polozka, p) {
            (Polozka::Ukazka, Polozka::Ukazka) => true,
            (_, Polozka::Ukazka) => false,
            (_, p) => p.useky().first() == self.useky.get(self.usek),
        }
    }
}

/// Děj plánovače pro testy (pozice = rámec výstupu, kde nastal).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dej {
    Start(Polozka),
    Usek(Usek),
    Dobeh,
    Konec,
}

/// Plánovač zvuků: čistý stavový automat bez zařízení a bez hodin —
/// požadavky dovnitř ([`Planovac::pozadavek`]), rámce ven
/// ([`Planovac::vyrob`]). Přehrávání je jen dopisuje do fronty zařízení.
///
/// - nikdy dva zvuky přes sebe: hraje nejvýš jedna položka, čeká nejvýš
///   jedna (novější požadavek přepíše čekající);
/// - jiný požadavek během hraní → rozehraný plynule dohasne
///   ([`PRERUSENI_MS`]), [`MEZERA_MS`] ticha, pak nový celý;
/// - stejný požadavek během hraní (ne během doběhu) se zahodí;
/// - [`Polozka::Ztis`]: rozehraný plynule dohasne, čekající se zahodí;
/// - po posledním zvuku [`DOZVUK_MS`] digitálního ticha, pak hotovo.
struct Planovac {
    hraje: Option<Hraje>,
    /// Čekající položka a čas požadavku (`GetTickCount64`, jen pro log).
    ceka: Option<(Polozka, u64)>,
    /// Rámců ticha od konce posledního zvuku; `None` = v tomhle proudu
    /// ještě nic nehrálo (první zvuk bez čekání).
    ticho: Option<usize>,
    /// Položka, která právě začala hrát, a čas jejího požadavku.
    zacatek: Option<(Polozka, u64)>,
    dobehu: u32,
    mezera: usize,
    dozvuk: usize,
    dobeh: usize,
    vyrobeno: usize,
    #[cfg(test)]
    historie: Vec<(usize, Dej)>,
}

impl Planovac {
    fn new(vz: u32) -> Planovac {
        Planovac {
            hraje: None,
            ceka: None,
            ticho: None,
            zacatek: None,
            dobehu: 0,
            mezera: pocet_vzorku(MEZERA_MS, vz),
            dozvuk: pocet_vzorku(DOZVUK_MS, vz),
            dobeh: pocet_vzorku(PRERUSENI_MS, vz).max(1),
            vyrobeno: 0,
            #[cfg(test)]
            historie: Vec::new(),
        }
    }

    #[cfg(test)]
    fn dej(&mut self, d: Dej) {
        self.historie.push((self.vyrobeno, d));
    }

    /// Nový požadavek (`cas` jen pro log latence).
    fn pozadavek(&mut self, p: Polozka, cas: u64) {
        if p == Polozka::Ztis {
            // Vypnutý zvuk, konec aplikace: nic dalšího nezazní a rozehrané
            // nedohraje — useknutí by ale lupnulo, proto doběh.
            self.ceka = None;
            if let Some(h) = self.hraje.as_mut().filter(|h| h.dobeh.is_none()) {
                h.dobeh = Some(0);
                self.dobehu += 1;
                #[cfg(test)]
                self.dej(Dej::Dobeh);
            }
            return;
        }
        let Some(h) = self.hraje.as_mut() else {
            // Nic nehraje (ještě nezačalo, nebo ticho mezi zvuky): zazní
            // jen nejnovější.
            self.ceka = Some((p, cas));
            return;
        };
        if h.dobeh.is_some() {
            // Rozehraný už dohasíná: jen přepsat čekající.
            self.ceka = Some((p, cas));
            return;
        }
        if h.uz_hlasi(p) {
            // Rozehraný už ohlašuje správný stav. Zbytek ukázky (čekající
            // Hra) zahodit — skutečný stav je ten rozehraný.
            h.useky = &h.useky[..=h.usek];
            self.ceka = None;
            return;
        }
        h.dobeh = Some(0);
        self.dobehu += 1;
        self.ceka = Some((p, cas));
        #[cfg(test)]
        self.dej(Dej::Dobeh);
    }

    /// Hraje, čeká, nebo ještě nedoznělo ticho za posledním zvukem.
    fn hotovo(&self) -> bool {
        self.hraje.is_none() && self.ceka.is_none() && self.ticho.is_none_or(|t| t >= self.dozvuk)
    }

    /// Položka, která od minulého dotazu začala hrát.
    fn vezmi_zacatek(&mut self) -> Option<(Polozka, u64)> {
        self.zacatek.take()
    }

    /// Další rámce proudu do `out`; vrátí, kolik jich vyrobil (méně než
    /// `out.len()` jen tehdy, když je hotovo).
    fn vyrob(&mut self, s: &Sada, out: &mut [[f32; 2]]) -> usize {
        for (i, ramec) in out.iter_mut().enumerate() {
            match self.dalsi(s) {
                Some(v) => *ramec = v,
                None => return i,
            }
        }
        out.len()
    }

    fn dohral(&mut self) {
        self.hraje = None;
        self.ticho = Some(0);
        #[cfg(test)]
        self.dej(Dej::Konec);
    }

    fn dalsi(&mut self, s: &Sada) -> Option<[f32; 2]> {
        loop {
            if let Some(h) = self.hraje.as_mut() {
                let usek = h.useky[h.usek];
                if h.pozice < s.delka(usek) {
                    let mut v = s.ramec(usek, h.pozice);
                    h.pozice += 1;
                    let mut dohral = false;
                    if let Some(k) = h.dobeh {
                        let g = kosinovy_dobeh(k, self.dobeh) as f32;
                        v = [v[0] * g, v[1] * g];
                        h.dobeh = Some(k + 1);
                        dohral = k + 1 >= self.dobeh;
                    }
                    self.vyrobeno += 1;
                    if dohral {
                        self.dohral();
                    }
                    return Some(v);
                }
                // Konec úseku. Při doběhu končí celá položka (zbytek ukázky
                // se zahazuje), jinak jde na další úsek.
                if h.dobeh.is_none() && h.usek + 1 < h.useky.len() {
                    h.usek += 1;
                    h.pozice = 0;
                    #[cfg(test)]
                    {
                        let u = h.useky[h.usek];
                        self.dej(Dej::Usek(u));
                    }
                    continue;
                }
                self.dohral();
                continue;
            }
            let ticho = self.ticho.unwrap_or(usize::MAX);
            if let Some((p, cas)) = self.ceka {
                if ticho >= self.mezera {
                    self.hraje = Some(Hraje::new(p));
                    self.ceka = None;
                    self.zacatek = Some((p, cas));
                    #[cfg(test)]
                    {
                        self.dej(Dej::Start(p));
                        self.dej(Dej::Usek(p.useky()[0]));
                    }
                    continue;
                }
            } else if ticho >= self.dozvuk {
                return None;
            }
            self.ticho = Some(ticho.saturating_add(1));
            self.vyrobeno += 1;
            return Some([0.0; 2]);
        }
    }
}

/// Kolik rámců dopsat, aby ve frontě zařízení bylo nejvýš `predstih`
/// (a nikdy víc, než se vejde do bufferu). Strop je podstata doběhu:
/// celý zvuk zapsaný předem by už nešel utnout.
fn kolik_dopsat(ve_fronte: usize, predstih: usize, kapacita: usize) -> usize {
    predstih.min(kapacita).saturating_sub(ve_fronte)
}

// ── Formát směšovače ─────────────────────────────────────────────────

/// Typ vzorku ve formátu směšovače.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypVzorku {
    /// 32bitový float (sdílený režim Windows skoro vždy).
    F32,
    /// 16bitové celé číslo.
    I16,
    /// 32bitový kontejner celého čísla (i 24 platných bitů zarovnaných
    /// nahoru — spodní bity se ignorují).
    I32,
}

impl TypVzorku {
    fn bajtu(self) -> usize {
        match self {
            TypVzorku::F32 | TypVzorku::I32 => 4,
            TypVzorku::I16 => 2,
        }
    }
}

/// Formát směšovače, do kterého umíme psát.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub vzorkovani: u32,
    pub kanaly: u16,
    pub typ: TypVzorku,
}

/// `WAVE_FORMAT_PCM`, `WAVE_FORMAT_IEEE_FLOAT`, `WAVE_FORMAT_EXTENSIBLE`
/// (mmreg.h).
const TAG_PCM: u16 = 1;
const TAG_FLOAT: u16 = 3;
const TAG_EXTENSIBLE: u16 = 0xFFFE;
/// `KSDATAFORMAT_SUBTYPE_PCM` i `_IEEE_FLOAT` jsou GUID
/// `{0000000X-0000-0010-8000-00aa00389b71}`, kde X je tag formátu;
/// tohle je jejich společný konec v paměti (za `Data1`).
const PODFORMAT_KONEC: [u8; 12] = [
    0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];

impl Format {
    /// Formát z bajtů `WAVEFORMATEX` (18 B + `cbSize` B rozšíření, tedy
    /// i `WAVEFORMATEXTENSIBLE`). `None` = formát, do kterého psát
    /// neumíme (nebo nesmyslný) — pak se nehraje nic.
    pub fn z_waveformatex(b: &[u8]) -> Option<Format> {
        let h = cti_hlavicku(b)?;
        if h.platne.is_some_and(|p| p > h.bity) {
            return None;
        }
        let typ = match (h.druh, h.bity) {
            (TAG_FLOAT, 32) => TypVzorku::F32,
            (TAG_PCM, 16) => TypVzorku::I16,
            (TAG_PCM, 32) => TypVzorku::I32,
            _ => return None,
        };
        let f = Format {
            vzorkovani: h.vzorkovani,
            kanaly: h.kanaly,
            typ,
        };
        let rozumny = h.kanaly >= 1
            && (8_000..=768_000).contains(&h.vzorkovani)
            && usize::from(h.blok) == f.bajtu_na_ramec();
        rozumny.then_some(f)
    }

    /// Bajtů na rámec (všechny kanály jednoho okamžiku).
    pub fn bajtu_na_ramec(self) -> usize {
        usize::from(self.kanaly) * self.typ.bajtu()
    }
}

/// Rámce (levý, pravý) jako prokládané vzorky formátu `f` do `cil`
/// (aspoň `ramce.len() × f.bajtu_na_ramec()` bajtů). Zvuk jde do prvních
/// dvou kanálů (levý a pravý přední; mono zařízení dostane průměr),
/// ostatní mlčí — u 5.1 by jinak hrál i střed a subwoofer a zvuk by byl
/// hlasitější, než má být.
fn do_formatu(ramce: &[[f32; 2]], f: Format, cil: &mut [u8]) {
    let kanalu = f.kanaly;
    let hodnoty = ramce.iter().flat_map(|&[l, p]| {
        (0..kanalu).map(move |k| match k {
            0 if kanalu == 1 => 0.5 * (l + p),
            0 => l,
            1 => p,
            _ => 0.0,
        })
    });
    for (c, v) in cil.chunks_exact_mut(f.typ.bajtu()).zip(hodnoty) {
        match f.typ {
            TypVzorku::F32 => c.copy_from_slice(&v.to_le_bytes()),
            // Useknout k nule, ne zaokrouhlit: špička pak nikdy
            // nepřeleze SPICKA_DBFS ani o zlomek decibelu.
            TypVzorku::I16 => c.copy_from_slice(&((v * f32::from(i16::MAX)) as i16).to_le_bytes()),
            TypVzorku::I32 => {
                c.copy_from_slice(&((f64::from(v) * f64::from(i32::MAX)) as i32).to_le_bytes())
            }
        }
    }
}

/// Krátký popis formátu, který neumíme — do logu.
fn popis_formatu(b: &[u8]) -> String {
    let u16_na = |i: usize| b.get(i..i + 2).map(|x| u16::from_le_bytes([x[0], x[1]]));
    let u32_na = |i: usize| {
        b.get(i..i + 4)
            .map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
    };
    format!(
        "tag {:#06x}, podformát {:?}, {:?} bitů, {:?} kanálů, {:?} Hz",
        u16_na(0).unwrap_or(0),
        u32_na(24),
        u16_na(14),
        u16_na(2),
        u32_na(4)
    )
}

// ── Přehrávání (WASAPI, sdílený režim, po periodách) ─────────────────

/// Jak dlouho smí zařízení mlčet (žádná událost), než se přehrávání
/// vzdá. Zařízení, které data nebere (odpojená sluchátka uprostřed
/// zvuku), nesmí vlákno zvuku zaseknout.
const LIMIT_TICHA_ZARIZENI: Duration = Duration::from_millis(1000);
/// Čekání na událost mezi kontrolami limitu.
const CEKANI_MS: u32 = 200;

/// Zásobník vlákna zvuku. Jen rezervace adresního prostoru, paměť se
/// bere až použitím; COM a zavaděč DLL (první otevření zvuku) potřebují
/// víc než pár desítek KiB.
#[doc(hidden)]
pub const ZASOBNIK: usize = 256 * 1024;

fn ted_ms() -> u64 {
    // SAFETY: bez parametrů, nic nesdílí.
    unsafe { GetTickCount64() }
}

/// Auto-reset událost Windows (budík požadavků i událost zařízení).
struct Udalost(HANDLE);

// SAFETY: handle události jde používat z kteréhokoli vlákna.
unsafe impl Send for Udalost {}
// SAFETY: SetEvent/WaitFor… jsou vláknově bezpečné.
unsafe impl Sync for Udalost {}

impl Udalost {
    fn new() -> Result<Udalost, String> {
        // SAFETY: nepojmenovaná auto-reset událost, výchozí zabezpečení.
        unsafe { CreateEventW(None, false, false, None) }
            .map(Udalost)
            .map_err(|e| format!("CreateEventW: {e}"))
    }

    /// Manual-reset událost (zůstane nastavená, dokud ji někdo nezruší) —
    /// stav, na který může čekat kdokoli a kolikrát chce.
    fn rucni(nastavena: bool) -> Result<Udalost, String> {
        // SAFETY: nepojmenovaná manual-reset událost, výchozí zabezpečení.
        unsafe { CreateEventW(None, true, nastavena, None) }
            .map(Udalost)
            .map_err(|e| format!("CreateEventW: {e}"))
    }

    fn nastav(&self) {
        // SAFETY: platný handle po celý život objektu.
        let _ = unsafe { SetEvent(self.0) };
    }

    fn zrus(&self) {
        // SAFETY: platný handle po celý život objektu.
        let _ = unsafe { ResetEvent(self.0) };
    }

    /// Čeká bez limitu.
    fn cekej(&self) {
        // SAFETY: platný handle po celý život objektu.
        unsafe { WaitForSingleObject(self.0, INFINITE) };
    }

    /// Čeká nejvýš `limit`; `true` = událost je nastavená.
    fn cekej_nejvys(&self, limit: Duration) -> bool {
        let ms = u32::try_from(limit.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: platný handle po celý život objektu.
        let r = unsafe { WaitForSingleObject(self.0, ms) };
        r == WAIT_OBJECT_0
    }
}

impl Drop for Udalost {
    fn drop(&mut self) {
        // SAFETY: handle z CreateEventW, zavírá se právě jednou.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Schránka požadavků vlákna zvuku. Píše vlákno okna — nikdy nečeká:
/// atomik a `SetEvent`.
struct Fronta {
    /// Kód položky ≪ 56 | `GetTickCount64` požadavku (0 = nic). Platí jen
    /// nejnovější — starší čekající požadavek ohlašoval stav, který už
    /// neplatí.
    ceka: AtomicU64,
    /// Předehřát (knihovny zvuku a vzorky), nic nehrát.
    pripravit: AtomicBool,
    budik: Udalost,
    /// Kolik zvuků začalo hrát (sonda `ticho` podle toho pošle druhý
    /// požadavek až do rozehraného prvního).
    zacato: AtomicU32,
    /// Nastavená = výstup je zavřený (nic nehraje). Konec aplikace na ni
    /// krátce počká ([`pockej_na_ticho`]), ať rozehraný zvuk dohasne
    /// doběhem a neusekne se s procesem.
    zavreno: Udalost,
}

const KOD_POSUN: u32 = 56;
const CAS_MASKA: u64 = (1 << KOD_POSUN) - 1;

impl Fronta {
    fn new() -> Result<Fronta, String> {
        Ok(Fronta {
            ceka: AtomicU64::new(0),
            pripravit: AtomicBool::new(false),
            budik: Udalost::new()?,
            zacato: AtomicU32::new(0),
            zavreno: Udalost::rucni(true)?,
        })
    }

    fn posli(&self, p: Polozka, cas: u64) {
        self.ceka
            .store(p.kod() << KOD_POSUN | cas & CAS_MASKA, Ordering::Release);
        self.budik.nastav();
    }

    fn vezmi(&self) -> Option<(Polozka, u64)> {
        let v = self.ceka.swap(0, Ordering::AcqRel);
        Polozka::z_kodu(v >> KOD_POSUN).map(|p| (p, v & CAS_MASKA))
    }
}

/// COM na aktuálním vlákně. Drop ho uvolní, pokud ho tenhle objekt
/// zapnul — na tomtéž vlákně, proto není `Send`.
struct Com {
    uvolnit: bool,
    _vlakno: PhantomData<*const ()>,
}

impl Com {
    fn zapni() -> Result<Com, String> {
        // SAFETY: bez rezervovaného parametru; MTA — vlákno zvuku nemá
        // okna ani smyčku zpráv, kterou by STA potřebovalo.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // Vlákno už je STA (volající mimo vlákno zvuku): WASAPI funguje
        // i tam a cizí inicializaci uvolňovat nebudeme.
        let uvolnit = hr != RPC_E_CHANGED_MODE;
        if uvolnit {
            hr.ok().map_err(|e| format!("CoInitializeEx: {e}"))?;
        }
        Ok(Com {
            uvolnit,
            _vlakno: PhantomData,
        })
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.uvolnit {
            // SAFETY: párové k úspěšnému CoInitializeEx na tomtéž
            // vlákně; objekty COM z něj už jsou uvolněné.
            unsafe { CoUninitialize() };
        }
    }
}

/// Zvýšená priorita vlákna zvuku po dobu hraní — pod zátěží hry by
/// vlákno s normální prioritou nemuselo stihnout dopsat a zvuk by
/// zadrhl. Jen vlastní vlákno, nic v systému (princip 8); Drop vrátí
/// normální.
struct Priorita {
    _vlakno: PhantomData<*const ()>,
}

impl Priorita {
    fn zvys() -> Priorita {
        // SAFETY: pseudo-handle aktuálního vlákna (nezavírá se).
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };
        Priorita {
            _vlakno: PhantomData,
        }
    }
}

impl Drop for Priorita {
    fn drop(&mut self) {
        // SAFETY: pseudo-handle aktuálního vlákna; objekt není Send, je
        // to tedy totéž vlákno, kterému se priorita zvýšila.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_NORMAL) };
    }
}

/// Formát směšovače z `GetMixFormat`. Paměť patří COM, uvolní ji Drop.
struct MixFormat(*mut WAVEFORMATEX);

impl MixFormat {
    /// Celá struktura jako bajty: `WAVEFORMATEX` (18 B) a za ní
    /// `cbSize` bajtů rozšíření (`WAVEFORMATEXTENSIBLE`).
    fn bajty(&self) -> &[u8] {
        // SAFETY: GetMixFormat vrátil (nenulový, ověřeno v `otevri`)
        // blok s celou WAVEFORMATEX a za ní cbSize bajtů rozšíření;
        // blok žije do Drop. Čte se po bajtech — struktura je packed.
        unsafe {
            let p = self.0.cast::<u8>().cast_const();
            let cb = u16::from_le_bytes([*p.add(16), *p.add(17)]);
            std::slice::from_raw_parts(p, 18 + usize::from(cb))
        }
    }

    fn format(&self) -> Result<Format, String> {
        Format::z_waveformatex(self.bajty())
            .ok_or_else(|| format!("formát směšovače neumím ({})", popis_formatu(self.bajty())))
    }
}

impl Drop for MixFormat {
    fn drop(&mut self) {
        // SAFETY: paměť z GetMixFormat (CoTaskMemAlloc), uvolňuje se
        // právě jednou.
        unsafe { CoTaskMemFree(Some(self.0.cast_const().cast())) };
    }
}

/// Výchozí výstup (role „konzole“ = hry a systémové zvuky) a jeho
/// formát směšovače. Jen otevření, nic nehraje. COM musí být na vlákně
/// zapnutý.
fn otevri() -> Result<(IAudioClient, MixFormat), String> {
    // Absolutní cesta z registru chrání jen MMDevApi.dll, AudioSes.dll
    // a další si bere jménem (viz hlavička) — bez omezeného hledání DLL
    // se nic nenačítá.
    if !dll::omezeno() {
        return Err(
            "hledání DLL není omezené na System32 (SetDefaultDllDirectories) — \
             AudioSes.dll by se mohla načíst i ze složky programu"
                .into(),
        );
    }
    // SAFETY: COM na vlákně zapnutý volajícím; in-proc třída. Její DLL
    // (MMDevApi.dll) načte COM absolutní cestou z registru, závislosti
    // načítané jménem (AudioSes.dll) jen ze System32 — ověřeno výš.
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| format!("MMDeviceEnumerator: {e}"))?;
    // SAFETY: platný enumerátor.
    let zarizeni = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
        .map_err(|e| format!("bez výchozího výstupu zvuku: {e}"))?;
    // SAFETY: platné zařízení, bez aktivačních parametrů.
    let klient: IAudioClient =
        unsafe { zarizeni.Activate(CLSCTX_ALL, None) }.map_err(|e| format!("IAudioClient: {e}"))?;
    // SAFETY: platný, ještě neinicializovaný klient.
    let mix = unsafe { klient.GetMixFormat() }.map_err(|e| format!("GetMixFormat: {e}"))?;
    if mix.is_null() {
        return Err("GetMixFormat bez formátu".into());
    }
    Ok((klient, MixFormat(mix)))
}

/// Paměť procesu v bajtech: soukromá (commit) a pracovní sada. Zvuk ji
/// měří před prvním otevřením a po něm — knihovny zvuku pak zůstávají
/// načtené po celý běh (COM na vlákně zvuku), a to se má změřit, ne
/// odhadnout (princip 10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub struct Pamet {
    pub soukroma: u64,
    pub pracovni: u64,
}

impl Pamet {
    /// Paměť tohohle procesu teď (`None`, když ji Windows neřeknou).
    #[doc(hidden)]
    pub fn ted() -> Option<Pamet> {
        let mut c = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        // SAFETY: pseudo-handle vlastního procesu (zavírat se nesmí);
        // struktura i s vyplněným `cb` žije po celé volání. K32… je
        // z kernel32 (Windows 7+), žádná další DLL.
        let ok = unsafe {
            K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
                c.cb,
            )
        };
        ok.as_bool().then_some(Pamet {
            soukroma: c.PrivateUsage as u64,
            pracovni: c.WorkingSetSize as u64,
        })
    }

    /// Kolik přibylo od `pred` (KiB, se znaménkem) — text do logu.
    #[doc(hidden)]
    pub fn prirustek(self, pred: Pamet) -> String {
        let kib = |po: u64, pred: u64| (i128::from(po) - i128::from(pred)) / 1024;
        format!(
            "soukromá {:+} KiB, pracovní sada {:+} KiB",
            kib(self.soukroma, pred.soukroma),
            kib(self.pracovni, pred.pracovni)
        )
    }
}

/// Co se změřilo při jednom otevření výstupu (do logu a pro sondu).
#[derive(Clone, Copy, Debug)]
#[doc(hidden)]
pub struct Souhrn {
    /// Od začátku po připravený klient (Activate, Initialize).
    pub otevreno: Duration,
    pub format: Format,
    /// Událost zařízení s prázdnou frontou před koncem zvuku.
    pub podteceni: u32,
    /// Kolik položek začalo hrát.
    pub zvuku: u32,
    /// Kolikrát rozehraný zvuk dohasínal kvůli novému požadavku.
    pub dobehu: u32,
    /// Prvních pár položek a ms od požadavku po zápis prvního rámce
    /// (u nového proudu = po `Start`).
    latence: [Option<(Polozka, u64)>; 4],
}

impl Souhrn {
    fn new(otevreno: Duration, format: Format) -> Souhrn {
        Souhrn {
            otevreno,
            format,
            podteceni: 0,
            zvuku: 0,
            dobehu: 0,
            latence: [None; 4],
        }
    }

    fn zacal(&mut self, p: Polozka, ms: u64) {
        if let Some(volne) = self.latence.iter_mut().find(|x| x.is_none()) {
            *volne = Some((p, ms));
        }
        self.zvuku += 1;
    }

    /// „Pauza za 12 ms, Hra za 40 ms od požadavku“.
    #[doc(hidden)]
    pub fn zvuky(&self) -> String {
        let casti: Vec<String> = self
            .latence
            .iter()
            .flatten()
            .map(|(p, ms)| format!("{} za {ms} ms", p.nazev()))
            .collect();
        let dalsi = self.zvuku as usize - casti.len().min(self.zvuku as usize);
        let mut s = casti.join(", ");
        if dalsi > 0 {
            s += &format!(" a {dalsi} dalších");
        }
        s + " od požadavku"
    }
}

/// Výsledek předehřátí.
#[doc(hidden)]
pub struct Pripraveno {
    pub za: Duration,
    pub format: Format,
    pub popis: &'static str,
}

/// Podtečení po prvních zalogovaných zvucích (souhrnně při konci).
static PODTECENI_POZDEJI: AtomicU32 = AtomicU32::new(0);

/// Stav vlákna zvuku.
struct Prehravac {
    /// COM na vlákně zvuku: `None` = ještě nezapnutý; `Some(None)` =
    /// nejde, zvuk nebude. Zůstává zapnutý po celý život vlákna —
    /// knihovny zvuku se tak načítají jen jednou.
    com: Option<Option<Com>>,
    sada: Option<Sada>,
    /// Nulové vzorky místo zvuků (sonda `zvuk_selftest -- ticho`).
    ticho: bool,
    /// Knihovny zvuku načtené a vzorky spočítané.
    predehrato: bool,
    /// Kolik zvuků se zalogovalo (log jen u prvních [`LOG_ZVUKU`]).
    zalogovano: u32,
    /// Poslední pokus selhal — chyba se loguje jen jednou za výpadek,
    /// ne u každého Scroll Locku.
    selhalo: bool,
    /// Paměť procesu před prvním otevřením (před COM a knihovnami).
    pamet_pred: Option<Pamet>,
}

/// U kolika prvních zvuků běhu zapíše vlákno řádek s latencí.
const LOG_ZVUKU: u32 = 3;

impl Prehravac {
    fn new(ticho: bool) -> Prehravac {
        Prehravac {
            com: None,
            sada: None,
            ticho,
            predehrato: false,
            zalogovano: 0,
            selhalo: false,
            pamet_pred: None,
        }
    }

    fn com_zapnuty(&mut self) -> bool {
        if self.com.is_none() {
            self.pamet_pred = Pamet::ted();
        }
        let com = self.com.get_or_insert_with(|| match Com::zapni() {
            Ok(c) => Some(c),
            Err(e) => {
                log::warn!("zvuk: {e} — bez zvuku");
                None
            }
        });
        com.is_some()
    }

    /// Sada pro vzorkování `vz`; při jiném vzorkování (jiné výchozí
    /// zařízení, sluchátka) se přepočítá — hraje se vždy v aktuálním
    /// formátu.
    fn sada_pro(&mut self, vz: u32) -> &mut Sada {
        if let Some(s) = self.sada.as_ref().filter(|s| s.vzorkovani != vz) {
            log::info!(
                "zvuk: vzorkování směšovače se změnilo ({} → {vz} Hz) — vzorky se přepočítají",
                s.vzorkovani
            );
            self.sada = None;
        }
        let ticho = self.ticho;
        self.sada.get_or_insert_with(|| {
            if ticho {
                Sada::ticha(vz)
            } else {
                Sada::pro(vz)
            }
        })
    }

    /// Předehřátí: COM, enumerátor, `Activate`, `GetMixFormat`
    /// a spočítané vzorky. Klient se zavře bez `Initialize` — nic nehraje
    /// a zařízení se nedrží.
    fn priprav(&mut self) -> Result<Pripraveno, String> {
        if !self.com_zapnuty() {
            return Err("COM nejde zapnout".into());
        }
        let t = Instant::now();
        let (klient, mix) = otevri()?;
        let format = mix.format()?;
        drop(mix);
        drop(klient);
        let popis = self.sada_pro(format.vzorkovani).popis();
        self.predehrato = true;
        Ok(Pripraveno {
            za: t.elapsed(),
            format,
            popis,
        })
    }

    /// Předehřátí z požadavku aplikace — jednou za běh, do logu.
    fn priprav_a_zaloguj(&mut self) {
        if self.predehrato {
            return;
        }
        match self.priprav() {
            Ok(p) => {
                log::info!(
                    "zvuk: připraveno za {} ms ({} Hz, {} kanálů, {:?}; {}); paměť procesu: {}",
                    p.za.as_millis(),
                    p.format.vzorkovani,
                    p.format.kanaly,
                    p.format.typ,
                    p.popis,
                    self.pamet_od_pocatku()
                );
                self.selhalo = false;
            }
            Err(e) => {
                if !self.selhalo {
                    log::warn!("zvuk nejde připravit ({e}) — zkusí se při prvním zvuku");
                }
                self.selhalo = true;
            }
        }
    }

    fn pamet_od_pocatku(&self) -> String {
        match (self.pamet_pred, Pamet::ted()) {
            (Some(pred), Some(po)) => po.prirustek(pred),
            _ => "nezměřená".into(),
        }
    }

    /// Přehraje `prvni` a všechno, co mezitím přijde, jedním otevřením
    /// výstupu; do logu jen prvních pár zvuků běhu.
    fn prehraj(&mut self, f: &Fronta, prvni: (Polozka, u64)) {
        if !self.com_zapnuty() {
            return;
        }
        let predehrato = self.predehrato;
        match self.hraj(f, prvni) {
            Ok(s) => {
                self.selhalo = false;
                if self.zalogovano < LOG_ZVUKU || cfg!(debug_assertions) {
                    let prvni_otevreni = if predehrato {
                        String::new()
                    } else {
                        format!(
                            "; první otevření za {} ms ({} Hz, {} kanálů, {:?}), paměť procesu: {}",
                            s.otevreno.as_millis(),
                            s.format.vzorkovani,
                            s.format.kanaly,
                            s.format.typ,
                            self.pamet_od_pocatku()
                        )
                    };
                    let dobeh = if s.dobehu > 0 {
                        format!(", doběh {}×", s.dobehu)
                    } else {
                        String::new()
                    };
                    log::info!(
                        "zvuk: {} (předehřáto {}){dobeh}, podtečení {}{prvni_otevreni}",
                        s.zvuky(),
                        if predehrato { "ano" } else { "ne" },
                        s.podteceni
                    );
                    self.zalogovano = self.zalogovano.saturating_add(s.zvuku);
                } else if s.podteceni > 0 {
                    PODTECENI_POZDEJI.fetch_add(s.podteceni, Ordering::AcqRel);
                }
            }
            Err(e) => {
                if !self.selhalo {
                    log::warn!("zvuk nejde přehrát ({e}) — KeyPad běží dál bez něj");
                }
                self.selhalo = true;
            }
        }
    }

    /// Otevře výchozí výstup v režimu s událostí, přehrává podle
    /// plánovače (požadavky během hraní bere z `f`), dohraje ticho na
    /// konci a klient zavře. Synchronní — jen na vlákně zvuku (nebo
    /// v sondě). COM musí být na vlákně zapnutý.
    fn hraj(&mut self, f: &Fronta, prvni: (Polozka, u64)) -> Result<Souhrn, String> {
        // Výstup je otevřený — konec aplikace počká na doběh. Strážce
        // vzniká první, zaniká tedy poslední: „zavřeno“ až po uvolněném
        // klientovi, i po chybě.
        f.zavreno.zrus();
        let _zavreno = ZavrenoPriKonci(&f.zavreno);
        let _priorita = Priorita::zvys();
        let t = Instant::now();
        // Událost dřív než klient: proměnné se uvolňují v opačném pořadí,
        // takže handle, který klient drží, se zavře až po klientovi.
        let zarizeni = Udalost::new()?;
        let (klient, mix) = otevri()?;
        let format = mix.format()?;
        // SAFETY: přesně formát směšovače z GetMixFormat (sdílený režim
        // ho vždy přijme), výchozí relace aplikace; v režimu s událostí
        // je perioda 0 (určí ji engine).
        unsafe {
            klient.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                i64::from(BUFFER_MS) * 10_000,
                0,
                mix.0.cast_const(),
                None,
            )
        }
        .map_err(|e| format!("Initialize: {e}"))?;
        drop(mix);
        // SAFETY: inicializovaný klient v režimu s událostí; událost žije
        // déle než klient (viz pořadí proměnných výš).
        unsafe { klient.SetEventHandle(zarizeni.0) }.map_err(|e| format!("SetEventHandle: {e}"))?;
        // SAFETY: inicializovaný klient.
        let kapacita =
            unsafe { klient.GetBufferSize() }.map_err(|e| format!("GetBufferSize: {e}"))? as usize;
        // SAFETY: inicializovaný klient.
        let render: IAudioRenderClient =
            unsafe { klient.GetService() }.map_err(|e| format!("IAudioRenderClient: {e}"))?;
        let mut s = Souhrn::new(t.elapsed(), format);
        // Knihovny jsou načtené a sada se spočítá hned teď — další
        // předehřátí by nic nepřidalo.
        self.predehrato = true;
        let vz = format.vzorkovani;
        let sada = self.sada_pro(vz);
        sada.zajisti(prvni.0);
        let predstih = pocet_vzorku(PREDSTIH_MS, vz).min(kapacita).max(1);
        let mut planovac = Planovac::new(vz);
        planovac.pozadavek(prvni.0, prvni.1);
        let mut ramce = vec![[0f32; 2]; predstih];
        let vysledek = pumpuj(
            &Proud {
                klient: &klient,
                render: &render,
                zarizeni: &zarizeni,
                fronta: f,
                format,
                kapacita,
                predstih,
            },
            sada,
            &mut planovac,
            &mut ramce,
            &mut s,
        );
        // I po chybě: zastavený klient se uvolní hned (Drop), nic nevisí.
        // SAFETY: inicializovaný klient; Stop nespuštěného nic nedělá.
        let _ = unsafe { klient.Stop() };
        s.dobehu = planovac.dobehu;
        vysledek.map(|()| s)
    }
}

/// Nastaví „výstup zavřený“ při opuštění [`Prehravac::hraj`].
struct ZavrenoPriKonci<'a>(&'a Udalost);

impl Drop for ZavrenoPriKonci<'_> {
    fn drop(&mut self) {
        self.0.nastav();
    }
}

/// Jedna obrátka plnění fronty zařízení ([`pumpuj`] a v testech
/// simulace): nejdřív požadavek ze schránky bez čekání, pak rámce do
/// předstihu. Schránka první: požadavek, který přišel během otevírání
/// výstupu nebo během psaní, dostane plánovač dřív, než se zapíšou další
/// data — jinak by první zápis poslal do zařízení 50 ms zvuku, který už
/// neplatí („kousek Pauzy + Hra“, nalezeno revizí; co je ve frontě
/// zařízení, nejde vzít zpět).
struct Obratka {
    /// Požadavek, který plánovač právě dostal (simulace v testech si
    /// podle něj zapíše, kolik bylo zapsáno, když přišel).
    #[cfg_attr(not(test), allow(dead_code, reason = "čte jen simulace v testech"))]
    prijato: Option<(Polozka, u64)>,
    /// Rámců připravených v `ramce` k zápisu (0 = plánovač dohrál, nebo
    /// je fronta plná).
    ramcu: usize,
}

fn obratka(
    f: &Fronta,
    sada: &mut Sada,
    planovac: &mut Planovac,
    ve_fronte: usize,
    predstih: usize,
    kapacita: usize,
    ramce: &mut [[f32; 2]],
) -> Obratka {
    let prijato = f.vezmi();
    if let Some((polozka, cas)) = prijato {
        sada.zajisti(polozka);
        planovac.pozadavek(polozka, cas);
    }
    let ramcu = if planovac.hotovo() {
        0
    } else {
        let n = kolik_dopsat(ve_fronte, predstih, kapacita).min(ramce.len());
        planovac.vyrob(sada, &mut ramce[..n])
    };
    Obratka { prijato, ramcu }
}

/// Otevřený výstup, do kterého se píše.
struct Proud<'a> {
    klient: &'a IAudioClient,
    render: &'a IAudioRenderClient,
    zarizeni: &'a Udalost,
    fronta: &'a Fronta,
    format: Format,
    kapacita: usize,
    predstih: usize,
}

/// Plní frontu zařízení po periodách, dokud plánovač nedohraje a fronta
/// se nevyprázdní. Čeká na událost zařízení a budík požadavků zároveň —
/// žádné pollování.
fn pumpuj(
    p: &Proud,
    sada: &mut Sada,
    planovac: &mut Planovac,
    ramce: &mut [[f32; 2]],
    s: &mut Souhrn,
) -> Result<(), String> {
    let mut bezi = false;
    // Ve frontě mají být data — prázdná fronta je pak podtečení.
    let mut ceka_data = false;
    let mut posledni_udalost = Instant::now();
    loop {
        // SAFETY: inicializovaný klient.
        let ve_fronte = unsafe { p.klient.GetCurrentPadding() }
            .map_err(|e| format!("GetCurrentPadding: {e}"))? as usize;
        // Podtečení: fronta prázdná, ačkoli plánovač ještě měl co hrát.
        if ceka_data && ve_fronte == 0 && !planovac.hotovo() {
            s.podteceni += 1;
        }
        let o = obratka(
            p.fronta, sada, planovac, ve_fronte, p.predstih, p.kapacita, ramce,
        );
        if o.ramcu > 0 {
            zapis(p.render, &ramce[..o.ramcu], p.format)?;
        }
        if let Some((polozka, cas)) = planovac.vezmi_zacatek() {
            s.zacal(polozka, ted_ms().saturating_sub(cas));
            p.fronta.zacato.fetch_add(1, Ordering::AcqRel);
        }
        if planovac.hotovo() {
            ceka_data = false;
            if ve_fronte == 0 && o.ramcu == 0 {
                return Ok(());
            }
        } else {
            ceka_data = true;
        }
        if !bezi {
            // SAFETY: inicializovaný klient s předplněnou frontou.
            unsafe { p.klient.Start() }.map_err(|e| format!("Start: {e}"))?;
            bezi = true;
            posledni_udalost = Instant::now();
        }
        // SAFETY: obě události žijí po celou dobu čekání.
        let r =
            unsafe { WaitForMultipleObjects(&[p.zarizeni.0, p.fronta.budik.0], false, CEKANI_MS) };
        if r == WAIT_OBJECT_0 {
            posledni_udalost = Instant::now();
        } else if r.0 == WAIT_OBJECT_0.0 + 1 {
            // Požadavek si vezme další obrátka (schránka je první).
        } else if r != WAIT_TIMEOUT {
            return Err(format!("WaitForMultipleObjects: {:#x}", r.0));
        }
        if posledni_udalost.elapsed() > LIMIT_TICHA_ZARIZENI {
            return Err("výstup nebere data (zařízení stojí?)".into());
        }
    }
}

/// Zapíše rámce do fronty zařízení ve formátu směšovače.
fn zapis(render: &IAudioRenderClient, ramce: &[[f32; 2]], f: Format) -> Result<(), String> {
    let n = u32::try_from(ramce.len()).map_err(|_| "příliš mnoho rámců".to_string())?;
    // SAFETY: n ≤ volné místo ve frontě (kolik_dopsat).
    let cil = unsafe { render.GetBuffer(n) }.map_err(|e| format!("GetBuffer: {e}"))?;
    // SAFETY: GetBuffer vrátil místo pro n rámců (n × blok bajtů), platné
    // do ReleaseBuffer; nic jiného do něj nepíše.
    let cil = unsafe { std::slice::from_raw_parts_mut(cil, ramce.len() * f.bajtu_na_ramec()) };
    do_formatu(ramce, f, cil);
    // SAFETY: vrací právě n rámců z GetBuffer, bez příznaků.
    unsafe { render.ReleaseBuffer(n, 0) }.map_err(|e| format!("ReleaseBuffer: {e}"))
}

// ── Vlákno zvuku a veřejné rozhraní ──────────────────────────────────

/// Schránka vlákna zvuku; `None` = vlákno nejde spustit (zvuk nebude).
static VLAKNO: OnceLock<Option<Arc<Fronta>>> = OnceLock::new();

fn fronta() -> Option<&'static Arc<Fronta>> {
    VLAKNO.get_or_init(spust_vlakno).as_ref()
}

fn spust_vlakno() -> Option<Arc<Fronta>> {
    let fronta = match Fronta::new() {
        Ok(f) => Arc::new(f),
        Err(e) => {
            log::warn!("zvuk: vlákno nejde připravit ({e}) — bez zvuku");
            return None;
        }
    };
    let f = Arc::clone(&fronta);
    let vysledek = std::thread::Builder::new()
        .stack_size(ZASOBNIK)
        .name("keypad-zvuk".into())
        .spawn(move || {
            let mut prehravac = Prehravac::new(false);
            loop {
                // Nečinné vlákno spí bez limitu (princip 10).
                f.budik.cekej();
                // Chyba v kódu zvuku nesmí vlákno zvuku shodit: panic hook
                // ji zapíše do logu a příští zvuk se zkusí znovu.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if f.pripravit.swap(false, Ordering::AcqRel) {
                        prehravac.priprav_a_zaloguj();
                    }
                    // Ztišení bez rozehraného zvuku nemá co ztišit —
                    // výstup se kvůli němu neotevírá.
                    if let Some(p) = f.vezmi().filter(|p| p.0 != Polozka::Ztis) {
                        prehravac.prehraj(&f, p);
                    }
                }));
            }
        });
    match vysledek {
        Ok(_) => Some(fronta),
        Err(e) => {
            log::warn!("zvuk: vlákno nejde spustit ({e}) — bez zvuku");
            None
        }
    }
}

fn posli(p: Polozka) {
    if let Some(f) = fronta() {
        f.posli(p, ted_ms());
    }
}

/// Přehraje zvuk. Nečeká (volá ho vlákno okna): jen předá požadavek
/// vláknu zvuku (při prvním zvuku ho spustí). Rozehraný jiný zvuk
/// plynule dohasne, stejný se nezopakuje ([`Planovac`]).
pub fn prehraj(z: Zvuk) {
    posli(Polozka::Zvuk(z));
}

/// Ukázka (▷ v nastavení): Pauza, 0,6 s ticha a Hra jako jedna položka —
/// skutečný požadavek ji zruší celou.
pub fn prehraj_ukazku() {
    posli(Polozka::Ukazka);
}

/// Ztiší: rozehraný zvuk plynule dohasne (15 ms), čekající se zahodí.
/// Pro vypnutý ✓ Zvuk (i uprostřed ukázky ▷) a konec aplikace. Nečeká;
/// vlákno zvuku nespouští — když ještě neběží, nic nehraje.
pub fn zastav() {
    if let Some(Some(f)) = VLAKNO.get() {
        f.posli(Polozka::Ztis, ted_ms());
    }
}

/// Konec aplikace: počká nejvýš `limit`, až je výstup zavřený (rozehraný
/// zvuk po [`zastav`] dohasl), ať se zvuk neusekne s procesem (lupnutí).
/// `true` = nic nehraje. Bez vlákna zvuku hned.
pub fn pockej_na_ticho(limit: Duration) -> bool {
    match VLAKNO.get() {
        Some(Some(f)) => f.zavreno.cekej_nejvys(limit),
        _ => true,
    }
}

/// Předehřátí: vlákno zvuku načte knihovny zvuku a spočítá vzorky, nic
/// nehraje a zařízení nedrží. Volá se při prvním zapnutí ovladače (a při
/// zapnutí ✓ Zvuk se zapnutým ovladačem) — první Scroll Lock pak nečeká
/// na načítání knihoven. Opakované volání nic nedělá.
pub fn priprav() {
    if let Some(f) = fronta() {
        f.pripravit.store(true, Ordering::Release);
        f.budik.nastav();
    }
}

/// Souhrn podtečení po prvních zalogovaných zvucích — jeden řádek při
/// konci aplikace, ne u každého zvuku.
pub fn souhrn_pri_konci() {
    let n = PODTECENI_POZDEJI.swap(0, Ordering::AcqRel);
    if n > 0 {
        log::info!("zvuk: další podtečení za běh: {n}");
    }
}

/// Varianta Pauzy s glissandem — jen pro `zvuk_selftest -- pauza-glis`.
#[doc(hidden)]
#[allow(dead_code, reason = "volá jen příklad zvuk_selftest")]
pub fn prehraj_variantu_glis() {
    posli(Polozka::Glis);
}

/// Celá cesta přehrávání s NULOVÝMI vzorky, synchronně na volajícím
/// vlákně: COM, předehřátí, výchozí výstup, `Initialize` s událostí,
/// plnění po periodách, druhý požadavek do rozehraného prvního (doběh,
/// ticho, další zvuk), dozvuk, `Stop`. Jen pro příklad `zvuk_selftest`
/// — sonda podvržených DLL musí projít celou cestou a nic přitom nesmí
/// být slyšet. Vrací i paměť procesu po dohrání, kdy je klient zavřený
/// a COM ještě drží knihovny zvuku (jako vlákno zvuku aplikace).
#[doc(hidden)]
#[allow(dead_code, reason = "volá jen příklad zvuk_selftest")]
pub fn ticho_cela_cesta() -> Result<(Pripraveno, Souhrn, Option<Pamet>), String> {
    let fronta = Arc::new(Fronta::new()?);
    let mut p = Prehravac::new(true);
    let pripraveno = p.priprav()?;
    fronta.posli(Polozka::Zvuk(Zvuk::Pauza), ted_ms());
    let f2 = Arc::clone(&fronta);
    let pomocnik = std::thread::spawn(move || {
        // Druhý požadavek až do rozehraného prvního — jinak by se cesta
        // doběhu neprošla.
        let limit = Instant::now() + Duration::from_secs(3);
        while f2.zacato.load(Ordering::Acquire) == 0 && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::sleep(Duration::from_millis(60));
        f2.posli(Polozka::Zvuk(Zvuk::Hra), ted_ms());
    });
    let prvni = fronta.vezmi().ok_or("požadavek se ztratil")?;
    let vysledek = p.hraj(&fronta, prvni);
    let _ = pomocnik.join();
    let s = vysledek?;
    let pamet = Pamet::ted();
    Ok((pripraveno, s, pamet))
}

/// Vlákno zvuku běží? Jen pro testy, že nic nepřehrávají.
#[cfg(test)]
fn vlakno_bezi() -> bool {
    VLAKNO.get().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    const VZORKOVANI: [u32; 3] = [44_100, 48_000, 96_000];

    fn dbfs(spicka: f64) -> f64 {
        20.0 * spicka.log10()
    }

    fn rms_db(z: &Zaznam, vz: u32) -> f64 {
        dbfs(z.rms(pocet_vzorku(RMS_OKNO_MS, vz)))
    }

    // ── Syntéza ──

    /// Kritérium 1: 250–400 ms, první i poslední vzorek přesně nula.
    #[test]
    fn delka_a_okraje() {
        for vz in VZORKOVANI {
            let s = Sada::synteticka(vz);
            let mut g = Sada::synteticka(vz);
            g.zajisti(Polozka::Glis);
            for z in [&s.hra, &s.pauza, g.glis.as_ref().unwrap()] {
                let ms = z.delka() as f64 * 1000.0 / f64::from(vz);
                assert!((250.0..=400.0).contains(&ms), "{vz}: {ms} ms");
                assert_eq!(z.l[0], 0.0);
                assert_eq!(*z.l.last().unwrap(), 0.0);
                assert!(z.p.is_empty(), "syntetizované jsou mono");
            }
        }
    }

    /// Kritérium 1 (ticho za zvukem): plánovač za posledním zvukem vyrobí
    /// aspoň 30 ms digitálního ticha a teprve pak je hotovo.
    #[test]
    fn za_zvukem_digitalni_ticho() {
        for vz in VZORKOVANI {
            let s = Sada::synteticka(vz);
            for z in [Zvuk::Pauza, Zvuk::Hra] {
                let mut p = Planovac::new(vz);
                p.pozadavek(Polozka::Zvuk(z), 0);
                let mut out = vec![[1f32; 2]; 2 * vz as usize];
                let n = p.vyrob(&s, &mut out);
                assert!(p.hotovo());
                let delka = s.delka(Usek::Zaznam(if z == Zvuk::Hra {
                    Id::Hra
                } else {
                    Id::Pauza
                }));
                assert_eq!(n, delka + pocet_vzorku(DOZVUK_MS, vz), "{z:?} {vz}");
                assert!(out[delka..n].iter().all(|r| *r == [0.0; 2]));
            }
        }
    }

    /// Kritérium 2: obálka hlasu roste monotónně 10–15 ms (zvýšený
    /// kosinus) a teprve potom klesá; v místě přechodu je spojitá.
    #[test]
    fn nabeh_roste_pak_klesa() {
        for r in [&HRA, &PAUZA, &PAUZA_GLIS] {
            assert!((10.0..=15.0).contains(&r.nabeh_ms));
            let mut pred = -1.0;
            let mut t = 0.0;
            while t <= r.nabeh_ms {
                let e = obalka(t, r.nabeh_ms, r.tau_ms);
                assert!(e > pred || t == 0.0, "{t}");
                pred = e;
                t += 0.01;
            }
            assert!((obalka(r.nabeh_ms, r.nabeh_ms, r.tau_ms) - 1.0).abs() < 1e-12);
            let mut t = r.nabeh_ms;
            let mut pred = 1.0 + 1e-12;
            while t < f64::from(DELKA_MS) {
                let e = obalka(t, r.nabeh_ms, r.tau_ms);
                assert!(e < pred, "{t}");
                pred = e;
                t += 0.5;
            }
        }
        // I ve vzorcích: první 3 ms Pauzy pod polovinou špičky.
        for vz in VZORKOVANI {
            let s = Sada::synteticka(vz);
            let spicka = s.pauza.spicka() as f32;
            let tri = pocet_vzorku(3, vz);
            assert!(s.pauza.l[..tri].iter().all(|x| x.abs() <= spicka / 2.0));
        }
    }

    /// Kritérium 2: špička obou ≤ −14 dBFS (i po převodu na I16/I32),
    /// RMS prvních 250 ms se liší nejvýš o 1 dB a hlasitější (podle
    /// špičky) není potichu.
    #[test]
    fn spicka_a_hlasitost() {
        for vz in VZORKOVANI {
            let s = Sada::synteticka(vz);
            let (h, p) = (dbfs(s.hra.spicka()), dbfs(s.pauza.spicka()));
            assert!(h <= -14.0 + 1e-4 && p <= -14.0 + 1e-4, "{vz}: {h} {p}");
            assert!(
                h.max(p) > -14.1,
                "{vz}: {h} {p} — jeden má mít plnou špičku"
            );
            let (rh, rp) = (rms_db(&s.hra, vz), rms_db(&s.pauza, vz));
            assert!((rh - rp).abs() <= 1.0, "{vz}: RMS {rh} vs {rp}");
            let mut g = Sada::synteticka(vz);
            g.zajisti(Polozka::Glis);
            let glis = g.glis.as_ref().unwrap();
            assert!(dbfs(glis.spicka()) <= -14.0 + 1e-4);
            assert!((rms_db(glis, vz) - rh).abs() <= 1.0, "glissando");
            for typ in [TypVzorku::I16, TypVzorku::I32] {
                let f = Format {
                    vzorkovani: vz,
                    kanaly: 2,
                    typ,
                };
                for z in [&s.hra, &s.pauza] {
                    let r = dekoduj(&ramce(z, f), f);
                    let spicka = r.iter().flatten().fold(0f64, |a, &x| a.max(x.abs()));
                    assert!(dbfs(spicka) <= -14.0 + 1e-4, "{f:?}");
                }
            }
        }
    }

    /// Nejvyšší možný krok mezi sousedními vzorky receptu při plné
    /// obálce všech hlasů (součet parciál), se ziskem `zisk`.
    fn max_krok(r: &Recept, vz: u32, zisk: f64) -> f64 {
        let a = [1.0, z_db(r.harmonicke_db[0]), z_db(r.harmonicke_db[1])];
        r.hlasy
            .iter()
            .map(|h| {
                (1..=3)
                    .map(|k| {
                        let x = std::f64::consts::PI * k as f64 * h.vyska.max_hz() / f64::from(vz);
                        a[k - 1] * 2.0 * x.min(std::f64::consts::FRAC_PI_2).sin()
                    })
                    .sum::<f64>()
            })
            .sum::<f64>()
            * zisk
    }

    /// Zisk, kterým sada recept vynásobila (špička před a po).
    fn zisk(r: &Recept, z: &Zaznam, vz: u32) -> f64 {
        let surove = syntetizuj(r, vz);
        z.spicka() / surove.iter().fold(0f64, |a, &x| a.max(x.abs()))
    }

    fn nejvetsi_krok(v: impl Iterator<Item = f32>) -> f64 {
        let mut pred = 0f32;
        let mut m = 0f64;
        for x in v {
            m = m.max(f64::from((x - pred).abs()));
            pred = x;
        }
        m
    }

    /// Kritérium 3: rozdíl sousedních vzorků nikdy nepřekročí 1,05×
    /// teoretické maximum (žádný skok = žádné lupnutí) — v zvuku samém
    /// i v místě doběhu při přerušení plánovačem.
    #[test]
    fn hladkost() {
        for vz in VZORKOVANI {
            let mut s = Sada::synteticka(vz);
            s.zajisti(Polozka::Glis);
            let mez_h = max_krok(&HRA, vz, zisk(&HRA, &s.hra, vz));
            let mez_p = max_krok(&PAUZA, vz, zisk(&PAUZA, &s.pauza, vz));
            let glis = s.glis.as_ref().unwrap();
            let mez_g = max_krok(&PAUZA_GLIS, vz, zisk(&PAUZA_GLIS, glis, vz));
            assert!(
                nejvetsi_krok(s.hra.l.iter().copied()) <= 1.05 * mez_h,
                "Hra {vz}"
            );
            assert!(
                nejvetsi_krok(s.pauza.l.iter().copied()) <= 1.05 * mez_p,
                "Pauza {vz}"
            );
            assert!(
                nejvetsi_krok(glis.l.iter().copied()) <= 1.05 * mez_g,
                "glis {vz}"
            );
            // Přerušení v různých místech (náběh, plná obálka, doznění).
            let mez = mez_h.max(mez_p);
            for ms in [1, 7, 15, 40, 100, 200, 330, 355] {
                for (a, b) in [(Zvuk::Pauza, Zvuk::Hra), (Zvuk::Hra, Zvuk::Pauza)] {
                    let mut p = Planovac::new(vz);
                    p.pozadavek(Polozka::Zvuk(a), 0);
                    let mut out = vec![[0f32; 2]; pocet_vzorku(ms, vz)];
                    let mut proud = Vec::new();
                    let n = p.vyrob(&s, &mut out);
                    proud.extend_from_slice(&out[..n]);
                    p.pozadavek(Polozka::Zvuk(b), 0);
                    let mut zbytek = vec![[0f32; 2]; 2 * vz as usize];
                    let n = p.vyrob(&s, &mut zbytek);
                    proud.extend_from_slice(&zbytek[..n]);
                    assert!(p.hotovo());
                    let k = nejvetsi_krok(proud.iter().map(|r| r[0]));
                    assert!(k <= 1.05 * mez, "{vz} {a:?}→{b:?} v {ms} ms: {k} > {mez}");
                }
            }
        }
    }

    /// Mutant „doběh bez kosinu“: useknutý zvuk by test hladkosti
    /// zachytil — kontrola, že by ho opravdu zachytil.
    #[test]
    fn useknuti_by_hladkost_poznala() {
        let vz = 48_000;
        let s = Sada::synteticka(vz);
        let mez = max_krok(&PAUZA, vz, zisk(&PAUZA, &s.pauza, vz));
        let mut v = s.pauza.l[..pocet_vzorku(30, vz)].to_vec();
        v.extend(std::iter::repeat_n(0.0, 100));
        assert!(nejvetsi_krok(v.into_iter()) > 1.05 * mez);
    }

    /// Špičky po 10 ms oknech → počet nástupů (okno aspoň 1,5× hlasitější
    /// než předchozí).
    fn nastupy(v: &[f32], vz: u32) -> usize {
        let w = pocet_vzorku(10, vz);
        let spicky: Vec<f32> = v
            .chunks(w)
            .map(|c| c.iter().fold(0f32, |a, x| a.max(x.abs())))
            .collect();
        let prah = spicky.iter().fold(0f32, |a, &x| a.max(x)) * 0.05;
        let mut n = 0;
        let mut pred = 0f32;
        for &s in &spicky {
            if s > prah && s > 1.5 * pred {
                n += 1;
            }
            pred = s;
        }
        n
    }

    /// Průchody nulou za sekundu v úseku `od..do_` (vzorky).
    fn hustota_nul(v: &[f32], vz: u32, od: usize, do_: usize) -> f64 {
        let mut n = 0;
        let mut znamenko = 0f32;
        for &x in &v[od..do_] {
            if x != 0.0 {
                if znamenko != 0.0 && x.signum() != znamenko {
                    n += 1;
                }
                znamenko = x.signum();
            }
        }
        n as f64 * f64::from(vz) / (do_ - od) as f64
    }

    /// Kritérium 4: Hra má 3 nástupy a stoupá (v poslední třetině hustší
    /// průchody nulou než v první), Pauza 1 nástup a nižší rejstřík než
    /// začátek Hry.
    #[test]
    fn hra_a_pauza_se_lisi() {
        for vz in VZORKOVANI {
            let s = Sada::synteticka(vz);
            assert_eq!(nastupy(&s.hra.l, vz), 3, "Hra {vz}");
            assert_eq!(nastupy(&s.pauza.l, vz), 1, "Pauza {vz}");
            let n = s.hra.delka();
            let (prvni, posledni) = (
                hustota_nul(&s.hra.l, vz, 0, n / 3),
                hustota_nul(&s.hra.l, vz, n - n / 3, n),
            );
            assert!(posledni > prvni * 1.15, "Hra {vz}: {prvni} → {posledni}");
            let pauza = hustota_nul(&s.pauza.l, vz, 0, s.pauza.delka());
            assert!(pauza < prvni, "Pauza {vz}: {pauza} vs Hra {prvni}");
            assert_ne!(s.hra, s.pauza);
        }
    }

    // ── Plánovač ──

    /// Sada s konstantními záznamy dané délky: zdroj rámce pozná test
    /// podle hodnoty (Hra +0,5, Pauza −0,25).
    fn sada_konstant(vz: u32, ms: u32) -> Sada {
        let n = pocet_vzorku(ms, vz);
        Sada {
            vzorkovani: vz,
            hra: Zaznam::mono(vec![0.5; n]),
            pauza: Zaznam::mono(vec![-0.25; n]),
            glis: Some(Zaznam::mono(vec![-0.125; n])),
            ticha: false,
            vlastni: true,
        }
    }

    /// Výsledek simulace: výstupní proud a děje plánovače (pozice =
    /// rámec výstupu = čas přehrání).
    struct Simulace {
        vystup: Vec<f32>,
        deje: Vec<(usize, Dej)>,
        /// Pro každý požadavek: rámec, kdy přišel (čas přehrání), a kolik
        /// už bylo zapsáno.
        pozadavky: Vec<(usize, usize, Polozka)>,
    }

    /// Simulace zařízení jako naostro: každých 10 ms engine odebere data.
    /// Požadavky jdou do skutečné schránky ([`Fronta`], platí nejnovější)
    /// a budík vlákno hned probudí. Bez otevřeného výstupu vlákno vezme
    /// požadavek (ztišení výstup neotevře) a výstup otevírá `otevreni_ms`
    /// — požadavky mezitím čekají ve schránce. Pak plní jako `pumpuj`
    /// přes [`obratka`]: schránka, pak data do předstihu. Po dohrání
    /// a vyprázdnění fronty se výstup zavře a další požadavek otevře nový
    /// (nový plánovač).
    fn simuluj(s: &mut Sada, pozadavky: &[(u64, Polozka)]) -> Simulace {
        simuluj_s_otevrenim(s, pozadavky, 0)
    }

    fn simuluj_s_otevrenim(
        s: &mut Sada,
        pozadavky: &[(u64, Polozka)],
        otevreni_ms: u64,
    ) -> Simulace {
        let vz = s.vzorkovani;
        let predstih = pocet_vzorku(PREDSTIH_MS, vz);
        let kapacita = pocet_vzorku(BUFFER_MS, vz);
        let ramcu = |ms: u64| (ms * u64::from(vz) / 1000) as usize;
        let fronta = Fronta::new().unwrap();
        let mut sim = Simulace {
            vystup: Vec::new(),
            deje: Vec::new(),
            pozadavky: Vec::new(),
        };
        let mut planovac: Option<(Planovac, usize)> = None;
        // Vlákno vzalo první požadavek; výstup bude otevřený v daném ms.
        let mut otevira: Option<(u64, (Polozka, u64))> = None;
        let mut i = 0;
        let mut t = 0u64;
        let zavri = |p: (Planovac, usize), deje: &mut Vec<(usize, Dej)>| {
            deje.extend(p.0.historie.iter().map(|&(pos, d)| (pos + p.1, d)));
        };
        loop {
            let prehrano = ramcu(t);
            // Dohráno a fronta prázdná → výstup se zavře (před dalším
            // požadavkem, jako naostro).
            if planovac
                .as_ref()
                .is_some_and(|(p, _)| p.hotovo() && sim.vystup.len() <= prehrano)
            {
                zavri(planovac.take().unwrap(), &mut sim.deje);
            }
            if sim.vystup.len() < prehrano {
                // Výstup je zavřený — čas běží v tichu.
                assert!(planovac.is_none(), "podtečení v simulaci");
                sim.vystup.resize(prehrano, 0.0);
            }
            while let Some(&(tr, polozka)) = pozadavky.get(i) {
                if tr > t {
                    break;
                }
                fronta.posli(polozka, tr);
                i += 1;
            }
            if planovac.is_none() && otevira.is_none() {
                if let Some(prvni) = fronta.vezmi().filter(|p| p.0 != Polozka::Ztis) {
                    otevira = Some((t + otevreni_ms, prvni));
                }
            }
            if let Some((_, prvni)) = otevira.filter(|&(kdy, _)| kdy <= t) {
                otevira = None;
                let zaklad = sim.vystup.len();
                let mut p = Planovac::new(vz);
                sim.pozadavky.push((ramcu(prvni.1), zaklad, prvni.0));
                p.pozadavek(prvni.0, prvni.1);
                planovac = Some((p, zaklad));
            }
            if let Some((p, zaklad)) = planovac.as_mut() {
                let zapsano = *zaklad + p.vyrobeno;
                let ve_fronte = sim.vystup.len() - prehrano;
                let mut buf = vec![[0f32; 2]; predstih];
                let o = obratka(&fronta, s, p, ve_fronte, predstih, kapacita, &mut buf);
                if let Some((polozka, cas)) = o.prijato {
                    sim.pozadavky.push((ramcu(cas), zapsano, polozka));
                }
                sim.vystup.extend(buf[..o.ramcu].iter().map(|r| r[0]));
            }
            if planovac.is_none() && otevira.is_none() && i == pozadavky.len() {
                return sim;
            }
            t += 10;
            assert!(t < 60_000, "simulace neskončila");
        }
    }

    /// Obecná pravidla nad každou simulací (kritérium 5).
    fn over_pravidla(s: &Sada, sim: &Simulace) {
        let vz = s.vzorkovani;
        let mezera = pocet_vzorku(MEZERA_MS, vz);
        let dobeh = pocet_vzorku(PRERUSENI_MS, vz);
        let predstih = pocet_vzorku(PREDSTIH_MS, vz);
        let mut hraje = false;
        let mut konec: Option<usize> = None;
        let mut dobeh_od: Option<usize> = None;
        for &(pos, d) in &sim.deje {
            match d {
                Dej::Start(_) => {
                    assert!(!hraje, "dvě přehrávání přes sebe: {:?}", sim.deje);
                    if let Some(k) = konec {
                        assert!(pos - k >= mezera, "mezera {} < {mezera}", pos - k);
                    }
                    hraje = true;
                }
                Dej::Dobeh => {
                    assert!(hraje);
                    dobeh_od = Some(pos);
                }
                Dej::Konec => {
                    assert!(hraje);
                    if let Some(od) = dobeh_od.take() {
                        assert!(pos - od <= dobeh, "doběh {} > {dobeh}", pos - od);
                    }
                    hraje = false;
                    konec = Some(pos);
                }
                Dej::Usek(_) => assert!(hraje),
            }
        }
        assert!(!hraje, "nedohrálo");
        // Doběh začne hned na pozici zápisu v okamžiku požadavku, tedy
        // nejpozději za předstih od požadavku.
        for &(pos, d) in &sim.deje {
            if d == Dej::Dobeh {
                let (kdy, zapsano, _) = sim
                    .pozadavky
                    .iter()
                    .find(|r| r.1 == pos)
                    .copied()
                    .expect("doběh bez požadavku");
                assert!(
                    zapsano <= kdy + predstih,
                    "doběh {zapsano} > {kdy} + {predstih}"
                );
            }
        }
        // Poslední, co zazní, odpovídá poslednímu požadavku; po ztišení
        // už nezačne nic.
        let posledni_start = sim.deje.iter().rev().find_map(|&(pos, d)| match d {
            Dej::Start(p) => Some((pos, p)),
            _ => None,
        });
        match sim.pozadavky.last() {
            Some(&(_, zapsano, Polozka::Ztis)) => assert!(
                posledni_start.is_none_or(|(pos, _)| pos < zapsano),
                "po ztišení začal zvuk: {:?}",
                sim.deje
            ),
            posledni => assert_eq!(
                posledni_start.map(|s| s.1),
                posledni.map(|r| r.2),
                "{:?}",
                sim.deje
            ),
        }
        // Výstup nikdy neskočí víc než o hodnotu konstanty (žádný součet
        // dvou zvuků).
        assert!(sim.vystup.iter().all(|x| x.abs() <= 0.5 + 1e-6));
    }

    fn starty(sim: &Simulace) -> Vec<Polozka> {
        sim.deje
            .iter()
            .filter_map(|&(_, d)| match d {
                Dej::Start(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    fn pocet(sim: &Simulace, d: Dej) -> usize {
        sim.deje.iter().filter(|&&(_, x)| x == d).count()
    }

    const P: Polozka = Polozka::Zvuk(Zvuk::Pauza);
    const H: Polozka = Polozka::Zvuk(Zvuk::Hra);

    /// Dvakrát Scroll Lock do 0,1 s: nikdy celá Pauza + Hra — kousek
    /// Pauzy utnutý doběhem ≤ 15 ms, ≥ 50 ms ticha a celá Hra.
    #[test]
    fn dvakrat_scroll_lock() {
        for vz in VZORKOVANI {
            let mut s = Sada::synteticka(vz);
            for druhy in [20, 50, 80, 100] {
                let sim = simuluj(&mut s, &[(0, P), (druhy, H)]);
                over_pravidla(&s, &sim);
                assert_eq!(starty(&sim), [P, H], "{vz} {druhy}");
                assert_eq!(pocet(&sim, Dej::Dobeh), 1);
                // Hra dohrála celá: od startu po konec přesně její délka.
                let start = sim.deje.iter().find(|d| d.1 == Dej::Start(H)).unwrap().0;
                let konec = sim.deje.iter().rev().find(|d| d.1 == Dej::Konec).unwrap().0;
                assert_eq!(konec - start, s.hra.delka());
                // Z Pauzy zazněl jen kousek: do požadavku + předstih + doběh.
                let utnuto = sim.deje.iter().find(|d| d.1 == Dej::Konec).unwrap().0;
                let mez = pocet_vzorku(druhy as u32 + PREDSTIH_MS + PRERUSENI_MS, vz);
                assert!(utnuto <= mez, "{vz} {druhy}: {utnuto} > {mez}");
            }
        }
    }

    /// Požadavek ještě před začátkem hraní (otevírání zařízení) nahradí
    /// čekající — zazní jen nejnovější.
    #[test]
    fn pred_startem_jen_nejnovejsi() {
        let mut s = sada_konstant(48_000, 360);
        let sim = simuluj(&mut s, &[(0, P), (0, H)]);
        over_pravidla(&s, &sim);
        assert_eq!(starty(&sim), [H]);
        assert_eq!(pocet(&sim, Dej::Dobeh), 0);
        assert!(sim.vystup.iter().all(|&x| x >= 0.0), "z Pauzy nic");
    }

    /// Stejný požadavek během hraní se zahodí (nezopakuje se ani
    /// neutne); mutant „stejný zvuk se zopakuje“.
    #[test]
    fn stejny_se_nezopakuje() {
        let mut s = sada_konstant(48_000, 360);
        let sim = simuluj(&mut s, &[(0, P), (100, P), (300, P)]);
        over_pravidla(&s, &sim);
        assert_eq!(starty(&sim), [P]);
        assert_eq!(pocet(&sim, Dej::Dobeh), 0);
        // Po dohrání je to nový zvuk (nový stav) — zazní.
        let sim = simuluj(&mut s, &[(0, P), (1_000, P)]);
        over_pravidla(&s, &sim);
        assert_eq!(starty(&sim), [P, P]);
    }

    /// Mutant „fronta dvou zvuků za sebou“: rozehraný se při jiném
    /// požadavku nedohraje celý.
    #[test]
    fn jiny_rozehrany_utne() {
        let mut s = sada_konstant(48_000, 360);
        let sim = simuluj(&mut s, &[(0, P), (100, H)]);
        over_pravidla(&s, &sim);
        let konec_pauzy = sim.deje.iter().find(|d| d.1 == Dej::Konec).unwrap().0;
        assert!(konec_pauzy < s.pauza.delka(), "Pauza dohrála celá");
        // Požadavky během doběhu a ticha jen přepíšou čekající.
        let sim = simuluj(&mut s, &[(0, P), (100, H), (105, P), (110, H)]);
        over_pravidla(&s, &sim);
        assert_eq!(starty(&sim), [P, H]);
    }

    /// Scroll Lock 5× za 0,5 s: nic přes sebe, mezery, poslední = stav.
    #[test]
    fn rychle_za_sebou() {
        for vz in VZORKOVANI {
            let mut s = Sada::synteticka(vz);
            let sim = simuluj(&mut s, &[(0, P), (100, H), (200, P), (300, H), (400, P)]);
            over_pravidla(&s, &sim);
            assert_eq!(starty(&sim).last(), Some(&P));
        }
    }

    /// Ukázka je jedna položka: Pauza, 0,6 s ticha, Hra.
    #[test]
    fn ukazka_cela() {
        let vz = 48_000;
        let mut s = sada_konstant(vz, 360);
        let sim = simuluj(&mut s, &[(0, Polozka::Ukazka)]);
        over_pravidla(&s, &sim);
        assert_eq!(starty(&sim), [Polozka::Ukazka]);
        let useky: Vec<_> = sim
            .deje
            .iter()
            .filter_map(|&(pos, d)| match d {
                Dej::Usek(u) => Some((pos, u)),
                _ => None,
            })
            .collect();
        let n = s.pauza.delka();
        assert_eq!(
            useky,
            [
                (0, Usek::Zaznam(Id::Pauza)),
                (n, Usek::Ticho(UKAZKA_TICHO_MS)),
                (n + pocet_vzorku(UKAZKA_TICHO_MS, vz), Usek::Zaznam(Id::Hra)),
            ]
        );
    }

    /// Skutečný požadavek ukázku zruší celou — Hra z ukázky po něm
    /// nedohraje (mutant); stejný jako rozehraný úsek ukázky ji jen
    /// zkrátí.
    #[test]
    fn skutecny_pozadavek_zrusi_ukazku() {
        let vz = 48_000;
        let mut s = sada_konstant(vz, 360);
        let hra_z_ukazky = |sim: &Simulace, od: usize| {
            sim.deje
                .iter()
                .any(|&(pos, d)| pos >= od && d == Dej::Usek(Usek::Zaznam(Id::Hra)))
        };
        // Během ticha ukázky.
        let sim = simuluj(&mut s, &[(0, Polozka::Ukazka), (500, P)]);
        over_pravidla(&s, &sim);
        assert_eq!(starty(&sim), [Polozka::Ukazka, P]);
        assert!(!hra_z_ukazky(&sim, 0));
        // Během Pauzy ukázky jiný zvuk: doběh, pak Hra jako skutečný zvuk
        // (jediná Hra je ta nová).
        let sim = simuluj(&mut s, &[(0, Polozka::Ukazka), (100, H)]);
        over_pravidla(&s, &sim);
        assert_eq!(starty(&sim), [Polozka::Ukazka, H]);
        assert_eq!(
            sim.deje
                .iter()
                .filter(|d| d.1 == Dej::Usek(Usek::Zaznam(Id::Hra)))
                .count(),
            1
        );
        // Během Pauzy ukázky Pauza: rozehraná už ohlašuje stav — žádný
        // doběh, žádný nový start, a Hra z ukázky už nezazní.
        let sim = simuluj(&mut s, &[(0, Polozka::Ukazka), (100, P)]);
        assert_eq!(starty(&sim), [Polozka::Ukazka]);
        assert_eq!(pocet(&sim, Dej::Dobeh), 0);
        assert!(!hra_z_ukazky(&sim, 0));
        assert!(sim.vystup.iter().all(|&x| x <= 0.0), "Hra nezazněla");
        // Během Hry ukázky Hra: dohraje se, nic dalšího.
        let hra_od =
            (s.pauza.delka() + pocet_vzorku(UKAZKA_TICHO_MS, vz)) as u64 * 1000 / u64::from(vz);
        let sim = simuluj(&mut s, &[(0, Polozka::Ukazka), (hra_od + 50, H)]);
        assert_eq!(starty(&sim), [Polozka::Ukazka]);
        assert_eq!(pocet(&sim, Dej::Dobeh), 0);
        // Ukázka za ukázkou se nezopakuje.
        let sim = simuluj(&mut s, &[(0, Polozka::Ukazka), (100, Polozka::Ukazka)]);
        assert_eq!(starty(&sim), [Polozka::Ukazka]);
    }

    /// Plánovač zvládne i vlastní zvuky do 500 ms.
    #[test]
    fn dlouhe_zvuky() {
        for vz in VZORKOVANI {
            let mut s = sada_konstant(vz, MAX_ZVUK_MS);
            for pozadavky in [
                vec![(0, P), (450, H)],
                vec![(0, H), (100, P), (130, H), (600, P)],
                vec![(0, P), (520, H)],
            ] {
                let sim = simuluj(&mut s, &pozadavky);
                over_pravidla(&s, &sim);
            }
        }
    }

    /// Mutant „předstih bez stropu“: ve frontě nikdy víc než předstih,
    /// nikdy víc, než se vejde.
    #[test]
    fn predstih_ma_strop() {
        let (predstih, kapacita) = (2_400, 4_800);
        for ve_fronte in [0, 1, 100, 2_399, 2_400, 3_000, 4_800] {
            let n = kolik_dopsat(ve_fronte, predstih, kapacita);
            assert!(ve_fronte + n <= predstih.max(ve_fronte));
            assert!(ve_fronte + n <= kapacita.max(ve_fronte));
        }
        assert_eq!(kolik_dopsat(0, predstih, kapacita), predstih);
        assert_eq!(kolik_dopsat(0, predstih, 1_000), 1_000);
    }

    /// Po vyprázdnění se výstup zavře; další požadavek hraje hned celý.
    #[test]
    fn nova_relace_po_dohrani() {
        let mut s = sada_konstant(48_000, 360);
        let sim = simuluj(&mut s, &[(0, P), (2_000, H)]);
        over_pravidla(&s, &sim);
        assert_eq!(starty(&sim), [P, H]);
        assert_eq!(pocet(&sim, Dej::Dobeh), 0);
    }

    /// Požadavek, který přijde, zatímco se výstup otevírá (první otevření
    /// bez předehřátí trvalo u vlastníka 408 ms), nahradí čekající ještě
    /// před prvním zápisem — zazní jen Hra, ne „kousek Pauzy + Hra“
    /// (revize: schránka se dřív četla až po prvních 50 ms dat).
    #[test]
    fn pozadavek_behem_otevirani() {
        for otevreni in [10, 30, 400] {
            let mut s = sada_konstant(48_000, 360);
            let sim = simuluj_s_otevrenim(&mut s, &[(0, P), (5, H)], otevreni);
            over_pravidla(&s, &sim);
            assert_eq!(starty(&sim), [H], "otevření {otevreni} ms");
            assert_eq!(pocet(&sim, Dej::Dobeh), 0);
            assert!(sim.vystup.iter().all(|&x| x >= 0.0), "z Pauzy nic");
            // Víc přepnutí během otevírání: jen nejnovější.
            let sim = simuluj_s_otevrenim(&mut s, &[(0, P), (5, H), (8, P)], otevreni);
            over_pravidla(&s, &sim);
            assert_eq!(starty(&sim), [P], "otevření {otevreni} ms");
            assert!(sim.vystup.iter().all(|&x| x <= 0.0), "z Hry nic");
        }
        // Skutečné zvuky, všechna vzorkování.
        for vz in VZORKOVANI {
            let mut s = Sada::synteticka(vz);
            let sim = simuluj_s_otevrenim(&mut s, &[(0, P), (20, H)], 40);
            over_pravidla(&s, &sim);
            assert_eq!(starty(&sim), [H], "{vz}");
        }
    }

    /// Mezera ≥ 50 ms platí i přes zavření výstupu (revize): zvuk, který
    /// přijde těsně po zavření, dostane nový plánovač bez paměti ticha —
    /// dozvuk před zavřením proto trvá aspoň mezeru. Všechny časy kolem
    /// konce Pauzy (360 ms) a zavření.
    #[test]
    fn mezera_i_po_zavreni() {
        for vz in VZORKOVANI {
            let mut s = sada_konstant(vz, 360);
            for druhy in (350..=520).step_by(10) {
                let sim = simuluj(&mut s, &[(0, P), (druhy, H)]);
                over_pravidla(&s, &sim);
                assert_eq!(starty(&sim), [P, H], "{vz} {druhy}");
            }
            let mut s = Sada::synteticka(vz);
            let delka = (s.pauza.delka() as u64 * 1000 / u64::from(vz)) as u32;
            for druhy in (delka..=delka + 2 * DOZVUK_MS + PREDSTIH_MS).step_by(10) {
                let sim = simuluj(&mut s, &[(0, P), (u64::from(druhy), H)]);
                over_pravidla(&s, &sim);
            }
        }
    }

    /// Ztišení (vypnutý ✓ Zvuk, konec aplikace): rozehraný dohasne
    /// doběhem, čekající ani zbytek ukázky nezazní; bez rozehraného nic
    /// neotevře. Pak zase hraje normálně.
    #[test]
    fn ztiseni() {
        let vz = 48_000;
        let mut s = sada_konstant(vz, 360);
        let z = Polozka::Ztis;
        // Uprostřed Pauzy: doběh, ticho.
        let sim = simuluj(&mut s, &[(0, P), (100, z)]);
        over_pravidla(&s, &sim);
        assert_eq!(starty(&sim), [P]);
        assert_eq!(pocet(&sim, Dej::Dobeh), 1);
        let konec = sim.deje.iter().find(|d| d.1 == Dej::Konec).unwrap().0;
        assert!(konec < s.pauza.delka(), "Pauza dohrála celá");
        assert!(sim.vystup[konec..].iter().all(|&x| x == 0.0));
        // Ukázka ▷: ztišení po Pauze (v tichu ukázky) — Hra nezazní.
        let sim = simuluj(&mut s, &[(0, Polozka::Ukazka), (500, z)]);
        over_pravidla(&s, &sim);
        assert!(sim.vystup.iter().all(|&x| x <= 0.0), "Hra z ukázky zazněla");
        // Čekající (během doběhu) se zahodí.
        let sim = simuluj(&mut s, &[(0, P), (100, H), (105, z)]);
        over_pravidla(&s, &sim);
        assert_eq!(starty(&sim), [P]);
        assert!(sim.vystup.iter().all(|&x| x <= 0.0), "Hra zazněla");
        // Bez rozehraného nic neotevře a nic nezazní.
        let sim = simuluj(&mut s, &[(0, z)]);
        assert!(sim.deje.is_empty() && sim.pozadavky.is_empty());
        assert!(sim.vystup.iter().all(|&x| x == 0.0));
        // Po ztišení zase hraje (zapnutý zvuk), s mezerou.
        let sim = simuluj(&mut s, &[(0, P), (100, z), (300, H)]);
        over_pravidla(&s, &sim);
        assert_eq!(starty(&sim), [P, H]);
    }

    #[test]
    fn polozka_kod_tam_a_zpet() {
        for p in [P, H, Polozka::Ukazka, Polozka::Glis, Polozka::Ztis] {
            assert_eq!(Polozka::z_kodu(p.kod()), Some(p));
            assert!(p.kod() < 1 << (64 - KOD_POSUN));
        }
        assert_eq!(Polozka::z_kodu(0), None);
        assert_eq!(Polozka::z_kodu(9), None);
        let f = Fronta::new().unwrap();
        assert_eq!(f.vezmi(), None);
        f.posli(H, 1_234_567);
        f.posli(P, 7_654_321);
        assert_eq!(f.vezmi(), Some((P, 7_654_321)), "platí jen nejnovější");
        assert_eq!(f.vezmi(), None);
        assert!(!vlakno_bezi(), "test nesmí spustit vlákno zvuku");
    }

    // ── Formát směšovače ──

    /// Záznam jako rámce formátu `f`.
    fn ramce(z: &Zaznam, f: Format) -> Vec<u8> {
        let r: Vec<[f32; 2]> = (0..z.delka()).map(|i| z.ramec(i)).collect();
        let mut b = vec![0u8; r.len() * f.bajtu_na_ramec()];
        do_formatu(&r, f, &mut b);
        b
    }

    /// Rámce zpět na hodnoty ±1: `[rámec][kanál]`.
    fn dekoduj(b: &[u8], f: Format) -> Vec<Vec<f64>> {
        assert_eq!(b.len() % f.bajtu_na_ramec(), 0);
        b.chunks_exact(f.bajtu_na_ramec())
            .map(|r| {
                r.chunks_exact(f.typ.bajtu())
                    .map(|s| match f.typ {
                        TypVzorku::F32 => f64::from(f32::from_le_bytes([s[0], s[1], s[2], s[3]])),
                        TypVzorku::I16 => {
                            f64::from(i16::from_le_bytes([s[0], s[1]])) / f64::from(i16::MAX)
                        }
                        TypVzorku::I32 => {
                            f64::from(i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
                                / f64::from(i32::MAX)
                        }
                    })
                    .collect()
            })
            .collect()
    }

    /// Převod do formátu směšovače pro 44,1/48/96 kHz, 1/2/6 kanálů,
    /// float, 16 i 32 bitů: délka, stejný zvuk v předních kanálech,
    /// ostatní mlčí, špička.
    #[test]
    fn ramce_ve_formatu_smesovace() {
        for vz in VZORKOVANI {
            let s = Sada::synteticka(vz);
            for kanaly in [1u16, 2, 6] {
                for typ in [TypVzorku::F32, TypVzorku::I16, TypVzorku::I32] {
                    let f = Format {
                        vzorkovani: vz,
                        kanaly,
                        typ,
                    };
                    for z in [&s.hra, &s.pauza] {
                        let b = ramce(z, f);
                        assert_eq!(b.len(), z.delka() * usize::from(kanaly) * typ.bajtu());
                        let r = dekoduj(&b, f);
                        for (i, ramec) in r.iter().enumerate() {
                            let cekano = f64::from(z.l[i]);
                            assert!((ramec[0] - cekano).abs() < 1e-4, "{f:?} rámec {i}");
                            if kanaly > 1 {
                                assert_eq!(ramec[0], ramec[1], "{f:?} rámec {i}");
                            }
                            assert!(ramec.iter().skip(2).all(|&x| x == 0.0));
                        }
                        assert!(r[0].iter().all(|&x| x == 0.0));
                        assert!(r[r.len() - 1].iter().all(|&x| x == 0.0));
                    }
                }
            }
        }
    }

    /// Stereo (vlastní WAV): L a P do předních kanálů, mono zařízení
    /// průměr.
    #[test]
    fn stereo_do_formatu() {
        let r = [[0.5f32, -0.25], [0.0, 0.0], [0.1, 0.3]];
        let f2 = Format {
            vzorkovani: 48_000,
            kanaly: 2,
            typ: TypVzorku::F32,
        };
        let mut b = vec![0u8; 3 * f2.bajtu_na_ramec()];
        do_formatu(&r, f2, &mut b);
        let d = dekoduj(&b, f2);
        assert_eq!(d[0], [0.5, -0.25]);
        let f1 = Format { kanaly: 1, ..f2 };
        let mut b = vec![0u8; 3 * f1.bajtu_na_ramec()];
        do_formatu(&r, f1, &mut b);
        let d = dekoduj(&b, f1);
        assert_eq!(d[0], [0.125]);
        assert!((d[2][0] - 0.2).abs() < 1e-7);
    }

    /// `WAVEFORMATEX` (a s `podformat` i `WAVEFORMATEXTENSIBLE`) jako
    /// bajty.
    fn waveformat(
        tag: u16,
        kanaly: u16,
        vz: u32,
        bity: u16,
        blok: u16,
        rozsireni: Option<(u16, u32)>,
    ) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&tag.to_le_bytes());
        b.extend_from_slice(&kanaly.to_le_bytes());
        b.extend_from_slice(&vz.to_le_bytes());
        b.extend_from_slice(&(vz * u32::from(blok)).to_le_bytes());
        b.extend_from_slice(&blok.to_le_bytes());
        b.extend_from_slice(&bity.to_le_bytes());
        match rozsireni {
            None => b.extend_from_slice(&0u16.to_le_bytes()),
            Some((platne, podformat)) => {
                b.extend_from_slice(&22u16.to_le_bytes());
                b.extend_from_slice(&platne.to_le_bytes());
                b.extend_from_slice(&3u32.to_le_bytes()); // přední levý + pravý
                b.extend_from_slice(&podformat.to_le_bytes());
                b.extend_from_slice(&PODFORMAT_KONEC);
            }
        }
        b
    }

    #[test]
    fn format_smesovace() {
        let f = |vz, kanaly, typ| {
            Some(Format {
                vzorkovani: vz,
                kanaly,
                typ,
            })
        };
        // Typický sdílený režim: WAVEFORMATEXTENSIBLE, float, 48 kHz, stereo.
        let typicky = waveformat(TAG_EXTENSIBLE, 2, 48_000, 32, 8, Some((32, 3)));
        assert_eq!(typicky.len(), 40);
        assert_eq!(
            Format::z_waveformatex(&typicky),
            f(48_000, 2, TypVzorku::F32)
        );
        // Prostý WAVEFORMATEX: float i 16bit PCM.
        assert_eq!(
            Format::z_waveformatex(&waveformat(TAG_FLOAT, 2, 44_100, 32, 8, None)),
            f(44_100, 2, TypVzorku::F32)
        );
        assert_eq!(
            Format::z_waveformatex(&waveformat(TAG_PCM, 1, 96_000, 16, 2, None)),
            f(96_000, 1, TypVzorku::I16)
        );
        // EXTENSIBLE PCM: 16 bitů a 24 platných v 32bitovém kontejneru.
        assert_eq!(
            Format::z_waveformatex(&waveformat(TAG_EXTENSIBLE, 2, 48_000, 16, 4, Some((16, 1)))),
            f(48_000, 2, TypVzorku::I16)
        );
        assert_eq!(
            Format::z_waveformatex(&waveformat(
                TAG_EXTENSIBLE,
                6,
                48_000,
                32,
                24,
                Some((24, 1))
            )),
            f(48_000, 6, TypVzorku::I32)
        );
    }

    /// Neznámý nebo nesmyslný formát → `None` (ticho, ne šum).
    #[test]
    fn neznamy_format() {
        let nic = |b: Vec<u8>| assert_eq!(Format::z_waveformatex(&b), None, "{b:?}");
        // MP3, 24 bitů těsně, 64bitový float, 8bitové PCM.
        nic(waveformat(0x55, 2, 48_000, 0, 1, None));
        nic(waveformat(TAG_PCM, 2, 48_000, 24, 6, None));
        nic(waveformat(TAG_FLOAT, 2, 48_000, 64, 16, None));
        nic(waveformat(TAG_PCM, 1, 8_000, 8, 1, None));
        // EXTENSIBLE s cizím podformátem, krátké, víc platných bitů než
        // kontejner.
        nic(waveformat(
            TAG_EXTENSIBLE,
            2,
            48_000,
            32,
            8,
            Some((32, 0x92)),
        ));
        let mut kratke = waveformat(TAG_EXTENSIBLE, 2, 48_000, 32, 8, Some((32, 3)));
        kratke.truncate(30);
        nic(kratke);
        let mut cbsize = waveformat(TAG_EXTENSIBLE, 2, 48_000, 32, 8, Some((32, 3)));
        cbsize[16] = 0;
        nic(cbsize);
        nic(waveformat(TAG_EXTENSIBLE, 2, 48_000, 16, 4, Some((20, 1))));
        // Nesedí zarovnání bloku, nula kanálů, nesmyslné vzorkování.
        nic(waveformat(TAG_FLOAT, 2, 48_000, 32, 4, None));
        nic(waveformat(TAG_FLOAT, 0, 48_000, 32, 0, None));
        nic(waveformat(TAG_FLOAT, 2, 0, 32, 8, None));
        nic(waveformat(TAG_FLOAT, 2, 4_000_000, 32, 8, None));
        // Útržky.
        nic(Vec::new());
        nic(vec![3, 0, 2, 0]);
    }

    // ── WAV ──

    /// Blok RIFF.
    fn blok(id: &[u8; 4], telo: &[u8]) -> Vec<u8> {
        let mut b = id.to_vec();
        b.extend_from_slice(&(telo.len() as u32).to_le_bytes());
        b.extend_from_slice(telo);
        if telo.len() % 2 == 1 {
            b.push(0);
        }
        b
    }

    fn riff(bloky: &[Vec<u8>]) -> Vec<u8> {
        let telo: Vec<u8> = bloky.concat();
        let mut b = b"RIFF".to_vec();
        b.extend_from_slice(&(4 + telo.len() as u32).to_le_bytes());
        b.extend_from_slice(b"WAVE");
        b.extend_from_slice(&telo);
        b
    }

    /// Blok `fmt ` jako ho píší editory: PCM bez `cbSize` (16 B).
    fn fmt_pcm16(kanaly: u16, vz: u32) -> Vec<u8> {
        let mut f = waveformat(TAG_PCM, kanaly, vz, 16, 2 * kanaly, None);
        f.truncate(16);
        blok(b"fmt ", &f)
    }

    fn pcm16(v: &[i16]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    fn float32(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    /// Kritérium 7: syntetické soubory → správné vzorky nebo `None`.
    #[test]
    fn wav_dekodovani() {
        // PCM16 mono 48 kHz.
        let w = dekoduj_wav(&riff(&[
            fmt_pcm16(1, 48_000),
            blok(b"data", &pcm16(&[0, 16_384, -32_768, 32_767])),
        ]))
        .unwrap();
        assert_eq!(w.vzorkovani, 48_000);
        assert_eq!(w.l, [0.0, 0.5, -1.0, 32_767.0 / 32_768.0]);
        assert!(w.p.is_empty());
        // Float stereo 44,1 kHz, s blokem LIST (lichá délka + výplň) před
        // daty.
        let f = blok(b"fmt ", &waveformat(TAG_FLOAT, 2, 44_100, 32, 8, None));
        let w = dekoduj_wav(&riff(&[
            f,
            blok(b"LIST", b"INFOabc"),
            blok(b"data", &float32(&[0.25, -0.5, 1.0, 0.0])),
        ]))
        .unwrap();
        assert_eq!(
            (w.vzorkovani, w.l.clone(), w.p.clone()),
            (44_100, vec![0.25, 1.0], vec![-0.5, 0.0])
        );
        // EXTENSIBLE PCM16 i float; data před fmt.
        for (podformat, bity, data) in [
            (1u32, 16u16, pcm16(&[16_384, -16_384])),
            (3, 32, float32(&[0.5, -0.5])),
        ] {
            let f = blok(
                b"fmt ",
                &waveformat(
                    TAG_EXTENSIBLE,
                    1,
                    48_000,
                    bity,
                    bity / 8,
                    Some((bity, podformat)),
                ),
            );
            let w = dekoduj_wav(&riff(&[blok(b"data", &data), f])).unwrap();
            assert_eq!(w.l, [0.5, -0.5], "{podformat}");
        }
    }

    #[test]
    fn wav_nepodporovane_a_poskozene() {
        let dobre = riff(&[fmt_pcm16(1, 48_000), blok(b"data", &pcm16(&[1, 2, 3]))]);
        assert!(dekoduj_wav(&dobre).is_some());
        let nic = |b: &[u8], co: &str| assert_eq!(dekoduj_wav(b), None, "{co}");
        // Useknutý soubor (data kratší, než hlásí blok).
        nic(&dobre[..dobre.len() - 3], "useknutý");
        // Nesmyslná délka bloku.
        let mut b = dobre.clone();
        b[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        nic(&b, "délka bloku");
        // 24 bitů těsně, 24 platných ve 32bitovém kontejneru, 8 bitů.
        let f24 = blok(b"fmt ", &waveformat(TAG_PCM, 1, 48_000, 24, 3, None));
        nic(&riff(&[f24, blok(b"data", &[0; 6])]), "24 bit");
        let f2432 = blok(
            b"fmt ",
            &waveformat(TAG_EXTENSIBLE, 1, 48_000, 32, 4, Some((24, 1))),
        );
        nic(&riff(&[f2432, blok(b"data", &[0; 8])]), "24 v 32");
        let f8 = blok(b"fmt ", &waveformat(TAG_PCM, 1, 48_000, 8, 1, None));
        nic(&riff(&[f8, blok(b"data", &[0; 4])]), "8 bit");
        // Tři kanály, 4 kHz, 384 kHz.
        nic(
            &riff(&[fmt_pcm16(3, 48_000), blok(b"data", &[0; 12])]),
            "3 kanály",
        );
        nic(
            &riff(&[fmt_pcm16(1, 4_000), blok(b"data", &[0; 4])]),
            "4 kHz",
        );
        nic(
            &riff(&[fmt_pcm16(1, 384_000), blok(b"data", &[0; 4])]),
            "384 kHz",
        );
        // NaN ve floatu, prázdná data, bez fmt, bez dat, není RIFF/WAVE.
        let ff = blok(b"fmt ", &waveformat(TAG_FLOAT, 1, 48_000, 32, 4, None));
        nic(
            &riff(&[ff.clone(), blok(b"data", &float32(&[f32::NAN]))]),
            "NaN",
        );
        nic(&riff(&[ff, blok(b"data", &[])]), "prázdná data");
        nic(&riff(&[blok(b"data", &pcm16(&[1]))]), "bez fmt");
        nic(&riff(&[fmt_pcm16(1, 48_000)]), "bez dat");
        let mut b = dobre.clone();
        b[8..12].copy_from_slice(b"AVI ");
        nic(&b, "AVI");
        nic(&[], "prázdné");
        nic(b"RIFF", "útržek");
    }

    /// Float mimo plnou škálu (revize): obří konečný vzorek by převod
    /// 44,1 → 48 kHz posunul do ±inf a srovnání hlasitosti udělalo NaN.
    /// Dekodér ho odmítne a sada je chyba (hrají syntetizované) — nikdy
    /// nic nekonečného do zařízení.
    #[test]
    fn wav_float_mimo_skalu() {
        let wav = |vz: u32, v: &[f32]| {
            let f = blok(b"fmt ", &waveformat(TAG_FLOAT, 1, vz, 32, 4, None));
            riff(&[f, blok(b"data", &float32(v))])
        };
        let dobry = wav(48_000, &[0.0, 0.25, -0.5, 0.0]);
        for (v, co) in [
            (3e38f32, "3e38"),
            (-3e38, "−3e38"),
            (f32::MAX, "MAX"),
            (1.01, "1,01"),
            (f32::INFINITY, "inf"),
        ] {
            let zly = wav(44_100, &[0.0, 0.5, v, -0.5, 0.25, 0.0]);
            assert_eq!(dekoduj_wav(&zly), None, "{co}");
            assert!(Sada::z_wav(&zly, &dobry, 48_000).is_err(), "{co}");
            assert!(Sada::z_wav(&dobry, &zly, 48_000).is_err(), "{co}");
        }
        // Plná škála (i zaokrouhlení těsně nad) projde a výsledek je konečný.
        for v in [1.0f32, -1.0, 1.0005] {
            let w = wav(44_100, &[0.0, 0.5, v, -0.5, 0.25, 0.0]);
            assert!(dekoduj_wav(&w).is_some(), "{v}");
            let s = Sada::z_wav(&w, &dobry, 48_000).unwrap();
            assert!(s.hra.konecny() && s.pauza.konecny(), "{v}");
        }
        // Pojistka za dekodérem pozná NaN i nekonečno.
        assert!(!Zaznam::mono(vec![0.0, f32::NAN]).konecny());
        assert!(!Zaznam {
            l: vec![0.0],
            p: vec![f32::NEG_INFINITY]
        }
        .konecny());
        assert!(Zaznam::mono(vec![0.0, 1.0]).konecny());
    }

    /// Převzorkování zachová délku ±1 vzorek a sinus uvnitř přesně.
    #[test]
    fn prevzorkovani() {
        for (z, na) in [
            (44_100, 48_000),
            (48_000, 44_100),
            (22_050, 96_000),
            (96_000, 48_000),
        ] {
            let n = z as usize / 4;
            let f = 440.0;
            let x: Vec<f32> = (0..n)
                .map(|i| (std::f64::consts::TAU * f * i as f64 / f64::from(z)).sin() as f32)
                .collect();
            let y = prevzorkuj(&x, z, na);
            let cekano = n as f64 * f64::from(na) / f64::from(z);
            assert!(
                (y.len() as f64 - cekano).abs() <= 1.0,
                "{z}→{na}: {}",
                y.len()
            );
            // Na krajích interpolace bere vzorky mimo signál (nuly) —
            // porovnává se jen uvnitř, dva zdrojové vzorky od kraje.
            let okraj = 3 * (na as usize).div_ceil(z as usize) + 2;
            for (j, &v) in y.iter().enumerate().skip(okraj).take(y.len() - 2 * okraj) {
                let pravda = (std::f64::consts::TAU * f * j as f64 / f64::from(na)).sin();
                assert!((f64::from(v) - pravda).abs() < 2e-3, "{z}→{na} [{j}]");
            }
        }
        assert_eq!(prevzorkuj(&[0.1, 0.2], 48_000, 48_000), [0.1, 0.2]);
    }

    /// Vlastní WAV: hlasitý ztlumen na −14 dBFS, dvojice srovnaná podle
    /// RMS (±1 dB), konec doběhem na přesnou nulu, nic delšího než
    /// [`MAX_ZVUK_MS`], ticho na konci souboru se nepočítá.
    #[test]
    fn vlastni_zvuky() {
        let vz = 48_000;
        let sinus = |n: usize, a: f64, f: f64| -> Vec<i16> {
            (0..n)
                .map(|i| {
                    (a * 32_767.0 * (std::f64::consts::TAU * f * i as f64 / 48_000.0).sin()) as i16
                })
                .collect()
        };
        let wav = |v: &[i16]| riff(&[fmt_pcm16(1, 48_000), blok(b"data", &pcm16(v))]);
        let hlasity = wav(&sinus(14_400, 1.0, 600.0));
        let tichy = wav(&sinus(14_400, 0.05, 400.0));
        let s = Sada::z_wav(&hlasity, &tichy, vz).unwrap();
        assert!(s.vlastni);
        assert!(dbfs(s.hra.spicka()) <= -14.0 + 1e-4);
        assert!((rms_db(&s.hra, vz) - rms_db(&s.pauza, vz)).abs() <= 1.0);
        assert!(dbfs(s.pauza.spicka()) < -20.0, "tichý se nezesiluje");
        for z in [&s.hra, &s.pauza] {
            assert_eq!(z.l[0], 0.0);
            assert_eq!(*z.l.last().unwrap(), 0.0);
        }
        // Převzorkování 44,1 → 48 kHz, ticho na konci se odřízne.
        let mut s441 = sinus(13_230, 0.1, 500.0);
        s441.extend(std::iter::repeat_n(0, 20_000));
        let w441 = riff(&[fmt_pcm16(1, 44_100), blok(b"data", &pcm16(&s441))]);
        let s = Sada::z_wav(&w441, &w441, vz).unwrap();
        assert!((s.hra.delka() as i64 - 14_400).abs() <= 1);
        // Delší než strop, samé ticho, nesmysl.
        let dlouhy = wav(&sinus(pocet_vzorku(MAX_ZVUK_MS + 10, vz), 0.1, 510.0));
        assert!(Sada::z_wav(&dlouhy, &tichy, vz).is_err());
        assert!(Sada::z_wav(&tichy, &wav(&[0; 100]), vz).is_err());
        assert!(Sada::z_wav(b"nesmysl", &tichy, vz).is_err());
    }

    /// Vestavěné zvuky vlastníka dekodér přijme v každém běžném
    /// vzorkování zařízení: hrají ony, ne syntetizované, se špičkou
    /// nejvýš −14 dBFS, od nuly k nule a v délce podle souborů.
    #[test]
    fn vestavene_zvuky_vlastnika() {
        let (hra, pauza) = VLASTNI.expect("zvuky vlastníka jsou vestavěné");
        for vz in VZORKOVANI {
            let s = Sada::pro(vz);
            assert!(s.vlastni && !s.ticha, "{vz} Hz: hrají vlastní");
            assert_ne!(s.hra, Sada::synteticka(vz).hra);
            for (nazev, z, ms) in [("hra", &s.hra, 717), ("pauza", &s.pauza, 569)] {
                assert!(dbfs(z.spicka()) <= -14.0 + 1e-4, "{nazev} {vz} Hz: špička");
                assert_eq!(z.l[0], 0.0, "{nazev} {vz} Hz: začíná nulou");
                assert_eq!(*z.l.last().unwrap(), 0.0, "{nazev} {vz} Hz: končí nulou");
                let delka_ms = z.delka() as u64 * 1000 / u64::from(vz);
                // Konec doběhu souboru se zaokrouhlí na nuly a dekodér je
                // jako ticho odřízne — pár ms méně než délka souboru.
                assert!(
                    delka_ms <= ms && ms - delka_ms <= 10,
                    "{nazev} {vz} Hz: {delka_ms} ms"
                );
            }
        }
        assert!(Sada::z_wav(hra, pauza, 48_000).is_ok());
    }

    /// Bez vestavěných souborů (nebo s nepřijatými) hrají syntetizované.
    #[test]
    fn bez_vlastnich_syntetizovane() {
        let s = Sada::synteticka(48_000);
        assert!(!s.vlastni && !s.ticha);
        assert!(vlastni_se_vejdou(None));
        assert!(!vlastni_se_vejdou(Some((&[0; MAX_WAV_BAJTU + 1], &[]))));
    }

    /// Ticho sondy je stejně dlouhé jako zvuky a celé nulové.
    #[test]
    fn ticho_je_nulove() {
        let mut t = Sada::ticha(48_000);
        let s = Sada::synteticka(48_000);
        t.zajisti(Polozka::Glis);
        assert_eq!(t.hra.delka(), s.hra.delka());
        assert_eq!(t.pauza.delka(), s.pauza.delka());
        assert!(t.hra.kanaly().chain(t.pauza.kanaly()).all(|&x| x == 0.0));
    }

    #[test]
    fn souhrn_do_logu() {
        let f = Format {
            vzorkovani: 48_000,
            kanaly: 2,
            typ: TypVzorku::F32,
        };
        let mut s = Souhrn::new(Duration::ZERO, f);
        s.zacal(P, 12);
        s.zacal(H, 40);
        assert_eq!(s.zvuky(), "Pauza za 12 ms, Hra za 40 ms od požadavku");
        for _ in 0..4 {
            s.zacal(H, 1);
        }
        assert_eq!(s.zvuku, 6);
        assert!(
            s.zvuky().ends_with("a 2 dalších od požadavku"),
            "{}",
            s.zvuky()
        );
    }

    /// Přírůstek paměti do logu: KiB se znaménkem, i ubytek.
    #[test]
    fn pamet_prirustek() {
        let pred = Pamet {
            soukroma: 10 * 1024 * 1024,
            pracovni: 20 * 1024 * 1024,
        };
        let po = Pamet {
            soukroma: pred.soukroma + 612 * 1024 + 1000,
            pracovni: pred.pracovni - 3 * 1024,
        };
        assert_eq!(
            po.prirustek(pred),
            "soukromá +612 KiB, pracovní sada -3 KiB"
        );
        let ted = Pamet::ted().expect("paměť vlastního procesu");
        assert!(ted.soukroma > 0 && ted.pracovni > 0, "{ted:?}");
    }

    /// Bez omezeného hledání DLL se WASAPI neotevře a nic nenačte —
    /// AudioSes.dll si MMDevAPI bere jménem a vzala by i podvrženou
    /// kopii ze složky programu (sonda revize). S ním jde otevřít
    /// a formát směšovače umíme — jen enumerátor, výchozí výstup
    /// a GetMixFormat; NIC se neinicializuje ani nespustí, nic není
    /// slyšet. Bez zvukového zařízení (CI) se jen vypíše proč. Knihovny
    /// zvuku jsou ze System32, ne ze složky testu.
    #[test]
    fn wasapi_jen_s_hledanim_dll_v_system32() {
        use windows::core::{w, PCWSTR};
        use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
        use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

        // SAFETY: jen dotaz na už načtený modul.
        let nacteno = |dll: PCWSTR| unsafe { GetModuleHandleW(dll) }.is_ok();
        let _com = Com::zapni().expect("COM");

        assert!(
            !dll::omezeno(),
            "hledání DLL smí v testech omezit jen tenhle test"
        );
        let mmdevapi_pred = nacteno(w!("mmdevapi.dll"));
        let e = otevri()
            .err()
            .expect("bez omezeného hledání DLL se WASAPI nesmí otevřít");
        assert!(e.contains("SetDefaultDllDirectories"), "{e}");
        if !mmdevapi_pred {
            assert!(
                !nacteno(w!("mmdevapi.dll")),
                "odmítnuté otevření načetlo MMDevAPI"
            );
        }

        dll::jen_ze_system32().expect("SetDefaultDllDirectories");
        assert!(dll::omezeno());
        match otevri() {
            Ok((_klient, mix)) => {
                let f = mix.format();
                eprintln!("formát směšovače: {f:?} ({})", popis_formatu(mix.bajty()));
                assert!(f.is_ok(), "{}", popis_formatu(mix.bajty()));
            }
            Err(e) => eprintln!("WASAPI bez výstupu (v pořádku na PC bez zvuku): {e}"),
        }

        let mut buf = [0u16; 260];
        // SAFETY: buffer žije po celou dobu volání.
        let n = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
        let system32 = String::from_utf16_lossy(&buf[..n]).to_lowercase();
        for jmeno in [w!("mmdevapi.dll"), w!("audioses.dll")] {
            // SAFETY: jen dotaz na už načtený modul.
            let Ok(modul) = (unsafe { GetModuleHandleW(jmeno) }) else {
                continue;
            };
            // SAFETY: platný modul, buffer žije po celou dobu volání.
            let n = unsafe { GetModuleFileNameW(Some(modul), &mut buf) } as usize;
            let cesta = String::from_utf16_lossy(&buf[..n]).to_lowercase();
            assert!(cesta.starts_with(&system32), "{cesta}");
        }
        assert!(!vlakno_bezi(), "test nesmí spustit přehrávání");
    }
}

//! Krátký zvuk při pozastavení a návratu do hry (Fáze 4b).
//!
//! Během hry je okno schované a ikona v oznamovací oblasti bývá
//! v přetečení nebo pod hrou přes celou obrazovku — pozastavení Scroll
//! Lockem by jinak nešlo poznat. Dvojtón nic nekreslí přes hru
//! (princip 8) a hráč ho slyší, aniž by hledal lištu.
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
//! zvuku).
//!
//! Sdílený režim s výchozí relací: zvuk je ve Směšovači hlasitosti vidět
//! jako KeyPad (jde ztlumit zvlášť) a neumlčí ho vypnuté systémové
//! zvuky. Vypnout ho jde zaškrtnutím „Zvuk“ v nabídce ikony.
//!
//! Tón se počítá přímo ve formátu směšovače (vzorkování, kanály, typ
//! vzorku) — žádný převod vzorkování ani kodek. Formát, který neumíme,
//! znamená jeden řádek v logu a ticho, nikdy zkreslený zvuk.
//!
//! Klient se otevírá pro každý zvuk zvlášť a po dohrání se zavře:
//! nečinný KeyPad nedrží otevřený zvukový výstup a audio engine kvůli
//! němu nemíchá prázdný proud (princip 10).
//!
//! Přehrává vlastní malé vlákno. Otevření zvukového stacku trvá desítky
//! ms (poprvé, kdy se načítají knihovny, i víc) a dohrání skoro dvě
//! desetiny sekundy — vlákno okna, které o zvuku rozhoduje, zároveň
//! hlídá tep ovladačů a nesmí na nic čekat.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, WAVEFORMATEX,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};

use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::GetCurrentProcess;

use super::dll;
use super::slot::Budik;

/// Který zvuk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zvuk {
    /// Klesající dvojtón: klávesy teď jdou do Windows.
    Pauza,
    /// Stoupající dvojtón: klávesy zase ovládají ovladač.
    Hra,
}

impl Zvuk {
    /// Tóny (Hz, ms). G5 → C5 a zpět: dost vysoko, aby prošly i malými
    /// reproduktory notebooku, a dost krátce, aby nepřekryly hru.
    fn tony(self) -> &'static [(f32, u32)] {
        match self {
            Zvuk::Pauza => &[(784.0, 70), (523.0, 90)],
            Zvuk::Hra => &[(523.0, 70), (784.0, 90)],
        }
    }

    fn kod(self) -> u8 {
        match self {
            Zvuk::Pauza => 1,
            Zvuk::Hra => 2,
        }
    }

    fn z_kodu(k: u8) -> Option<Zvuk> {
        match k {
            1 => Some(Zvuk::Pauza),
            2 => Some(Zvuk::Hra),
            _ => None,
        }
    }
}

// ── Vzorky (čisté funkce, testují se bez přehrávání) ─────────────────

/// Špička. Tichý signál: systémové zvuky Windows mívají kolem −6 dBFS
/// a KeyPad nemá přehlušit hru ani hlasový chat.
const SPICKA_DBFS: f32 = -14.0;
/// Náběh a doběh každého tónu — bez nich by začátek a konec sinusovky
/// v reproduktoru lupl.
const NABEH_MS: u32 = 6;
/// Ticho mezi tóny, ať jsou slyšet jako dva.
const MEZERA_MS: u32 = 10;
/// Ticho za posledním tónem. Klient se zastaví, jakmile audio engine
/// vybere celý buffer; poslední milisekundy ještě míří do zařízení,
/// takže na konci musí být ticho, ne doznívající tón.
const DOZVUK_MS: u32 = 30;

/// Mono vzorky tónů `(Hz, ms)` za sebou ve vzorkování `vzorkovani`,
/// plná škála = ±1. Kosinový náběh a doběh [`NABEH_MS`], mezi tóny
/// [`MEZERA_MS`] ticha, špička [`SPICKA_DBFS`].
pub fn vzorky(tony: &[(f32, u32)], vzorkovani: u32) -> Vec<f32> {
    let amplituda = 10f32.powf(SPICKA_DBFS / 20.0);
    let nabeh = pocet_vzorku(NABEH_MS, vzorkovani);
    let mut v: Vec<f32> = Vec::new();
    for (i, &(frekvence, ms)) in tony.iter().enumerate() {
        if i > 0 {
            v.resize(v.len() + pocet_vzorku(MEZERA_MS, vzorkovani), 0.0);
        }
        let n = pocet_vzorku(ms, vzorkovani);
        for k in 0..n {
            // Fáze v f64: při 96 kHz by se f32 na konci tónu rozcházelo
            // o celé setiny periody.
            let t = k as f64 / f64::from(vzorkovani);
            let sinus = (std::f64::consts::TAU * f64::from(frekvence) * t).sin() as f32;
            v.push(sinus * amplituda * obalka(k, n, nabeh));
        }
    }
    v
}

fn pocet_vzorku(ms: u32, vzorkovani: u32) -> usize {
    (u64::from(vzorkovani) * u64::from(ms) / 1000) as usize
}

/// Hlasitost `k`-tého z `n` vzorků: kosinový náběh a doběh přes `r`
/// vzorků, mezi nimi plná. První i poslední vzorek je přesně nula.
fn obalka(k: usize, n: usize, r: usize) -> f32 {
    let od_kraje = k.min(n.saturating_sub(1).saturating_sub(k));
    if r == 0 || od_kraje >= r {
        return 1.0;
    }
    let x = od_kraje as f32 / r as f32;
    0.5 - 0.5 * (std::f32::consts::PI * x).cos()
}

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
        let kanaly = u16_na(2)?;
        let vzorkovani = u32_na(4)?;
        let blok = u16_na(12)?;
        let bity = u16_na(14)?;
        let druh = if tag == TAG_EXTENSIBLE {
            // cbSize ≥ 22: platné bity, maska kanálů, podformát (GUID).
            if u16_na(16)? < 22 || b.get(24 + 4..40)? != PODFORMAT_KONEC {
                return None;
            }
            let platne = u16_na(18)?;
            if platne > bity {
                return None;
            }
            u16::try_from(u32_na(24)?).ok()?
        } else {
            tag
        };
        let typ = match (druh, bity) {
            (TAG_FLOAT, 32) => TypVzorku::F32,
            (TAG_PCM, 16) => TypVzorku::I16,
            (TAG_PCM, 32) => TypVzorku::I32,
            _ => return None,
        };
        let f = Format {
            vzorkovani,
            kanaly,
            typ,
        };
        let rozumny = kanaly >= 1
            && (8_000..=768_000).contains(&vzorkovani)
            && usize::from(blok) == f.bajtu_na_ramec();
        rozumny.then_some(f)
    }

    /// Bajtů na rámec (všechny kanály jednoho okamžiku).
    pub fn bajtu_na_ramec(self) -> usize {
        usize::from(self.kanaly) * self.typ.bajtu()
    }
}

/// Mono vzorky jako prokládané rámce formátu `f` a za nimi
/// `ticho_ramcu` rámců ticha. Tón jde do prvních dvou kanálů (levý
/// a pravý přední, u mona do jediného), ostatní mlčí — u 5.1 by jinak
/// hrál i střed a subwoofer a zvuk by byl hlasitější, než má být.
pub fn do_formatu(mono: &[f32], f: Format, ticho_ramcu: usize) -> Vec<u8> {
    let blok = f.bajtu_na_ramec();
    let mut out = Vec::with_capacity((mono.len() + ticho_ramcu) * blok);
    for &v in mono {
        for kanal in 0..f.kanaly {
            let v = if kanal < 2 { v } else { 0.0 };
            match f.typ {
                TypVzorku::F32 => out.extend_from_slice(&v.to_le_bytes()),
                // Useknout k nule, ne zaokrouhlit: špička pak nikdy
                // nepřeleze SPICKA_DBFS ani o zlomek decibelu.
                TypVzorku::I16 => {
                    out.extend_from_slice(&((v * f32::from(i16::MAX)) as i16).to_le_bytes());
                }
                TypVzorku::I32 => out.extend_from_slice(
                    &((f64::from(v) * f64::from(i32::MAX)) as i32).to_le_bytes(),
                ),
            }
        }
    }
    out.resize(out.len() + ticho_ramcu * blok, 0);
    out
}

/// Co přehrát.
#[derive(Clone, Copy, Debug)]
enum Obsah {
    Ton(Zvuk),
    /// Nulové vzorky stejně dlouhé jako tón — jen pro ověření cesty
    /// (příklad `zvuk_selftest`), nic není slyšet.
    Ticho,
}

/// Celý buffer k přehrání ve formátu `f`: tón (nebo ticho stejné
/// délky) a za ním [`DOZVUK_MS`] ticha.
fn ramce(obsah: Obsah, f: Format) -> Vec<u8> {
    let dozvuk = pocet_vzorku(DOZVUK_MS, f.vzorkovani);
    let mono = match obsah {
        Obsah::Ton(z) => vzorky(z.tony(), f.vzorkovani),
        Obsah::Ticho => vec![0.0; vzorky(Zvuk::Pauza.tony(), f.vzorkovani).len()],
    };
    do_formatu(&mono, f, dozvuk)
}

// ── Přehrávání (WASAPI, sdílený režim) ───────────────────────────────

/// Kolik navíc oproti délce zvuku smí přehrávání trvat, než se vzdá.
/// Zařízení, které data nebere (odpojené sluchátka uprostřed zvuku),
/// nesmí vlákno zvuku zaseknout.
const LIMIT_NAVIC_MS: u64 = 1000;

/// Zásobník vlákna zvuku. Jen rezervace adresního prostoru, paměť se
/// bere až použitím; COM a zavaděč DLL (první otevření zvuku) potřebují
/// víc než pár desítek KiB.
#[doc(hidden)]
pub const ZASOBNIK: usize = 256 * 1024;

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

/// Co se změřilo při přehrání (do logu).
#[derive(Clone, Copy, Debug)]
#[doc(hidden)]
pub struct Prehrano {
    /// Od začátku po připravený klient (načtení knihoven, Initialize).
    pub otevreno: Duration,
    pub format: Format,
}

/// Paměť procesu v bajtech: soukromá (commit) a pracovní sada. Zvuk ji
/// měří před prvním zvukem a po něm — knihovny zvuku pak zůstávají
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

/// Otevře výchozí výstup, přehraje `obsah`, počká na dohrání a klient
/// zavře. Synchronní — volá se jen z vlákna zvuku (nebo z příkladu).
/// COM musí být na vlákně zapnutý.
fn hraj_hned(obsah: Obsah) -> Result<Prehrano, String> {
    let t = Instant::now();
    let (klient, mix) = otevri()?;
    let format = Format::z_waveformatex(mix.bajty())
        .ok_or_else(|| format!("formát směšovače neumím ({})", popis_formatu(mix.bajty())))?;
    let data = ramce(obsah, format);
    let blok = format.bajtu_na_ramec();
    let ramcu = data.len() / blok;
    // Buffer na celý zvuk: všechno se zapíše před Start, takže zvuk
    // nezadrhne, ani když hra vlákno zvuku na chvíli nepustí k CPU.
    let delka_hns = (ramcu as u64 * 10_000_000 / u64::from(format.vzorkovani)) as i64;
    // SAFETY: přesně formát směšovače z GetMixFormat (sdílený režim ho
    // vždy přijme), výchozí relace aplikace.
    unsafe {
        klient.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            0,
            delka_hns,
            0,
            mix.0.cast_const(),
            None,
        )
    }
    .map_err(|e| format!("Initialize: {e}"))?;
    drop(mix);
    // SAFETY: inicializovaný klient.
    let kapacita = unsafe { klient.GetBufferSize() }.map_err(|e| format!("GetBufferSize: {e}"))?;
    // SAFETY: inicializovaný klient.
    let render: IAudioRenderClient =
        unsafe { klient.GetService() }.map_err(|e| format!("IAudioRenderClient: {e}"))?;
    let otevreno = t.elapsed();

    let vysledek = posli(&klient, &render, &data, blok, kapacita, format.vzorkovani);
    // I po chybě: zastavený klient se uvolní hned (Drop), nic nevisí.
    // SAFETY: inicializovaný klient; Stop nespuštěného nic nedělá.
    let _ = unsafe { klient.Stop() };
    vysledek.map(|()| Prehrano { otevreno, format })
}

/// Zapíše `data` (celé rámce po `blok` bajtech) do bufferu klienta,
/// spustí ho a počká, až je audio engine vybere celé.
fn posli(
    klient: &IAudioClient,
    render: &IAudioRenderClient,
    data: &[u8],
    blok: usize,
    kapacita: u32,
    vzorkovani: u32,
) -> Result<(), String> {
    let ramcu = data.len() / blok;
    let ms = |ramce: u64| (ramce * 1000 / u64::from(vzorkovani)).max(1);
    let limit = Instant::now() + Duration::from_millis(ms(ramcu as u64) + LIMIT_NAVIC_MS);
    let vyprselo = || {
        if Instant::now() > limit {
            Err("výstup nebere data (zařízení stojí?)".to_string())
        } else {
            Ok(())
        }
    };
    let mut zapsano = 0usize;
    let mut bezi = false;
    // Plnění. S bufferem na celý zvuk jediná obrátka; kdyby ho Windows
    // daly menší, dopisuje se po polovinách.
    loop {
        // SAFETY: inicializovaný klient.
        let ve_fronte =
            unsafe { klient.GetCurrentPadding() }.map_err(|e| format!("GetCurrentPadding: {e}"))?;
        let zbyva = u32::try_from(ramcu - zapsano).unwrap_or(u32::MAX);
        let n = zbyva.min(kapacita.saturating_sub(ve_fronte));
        if n > 0 {
            let bajtu = n as usize * blok;
            // SAFETY: n ≤ volné místo v bufferu.
            let cil = unsafe { render.GetBuffer(n) }.map_err(|e| format!("GetBuffer: {e}"))?;
            // SAFETY: GetBuffer vrátil místo pro n rámců = `bajtu`
            // bajtů; zdroj leží celý v `data` (zapsano + n ≤ ramcu).
            unsafe {
                std::ptr::copy_nonoverlapping(data[zapsano * blok..].as_ptr(), cil, bajtu);
            }
            // SAFETY: vrací právě n rámců z GetBuffer, bez příznaků.
            unsafe { render.ReleaseBuffer(n, 0) }.map_err(|e| format!("ReleaseBuffer: {e}"))?;
            zapsano += n as usize;
        }
        if !bezi {
            // SAFETY: inicializovaný klient s předplněným bufferem.
            unsafe { klient.Start() }.map_err(|e| format!("Start: {e}"))?;
            bezi = true;
        }
        if zapsano == ramcu {
            break;
        }
        vyprselo()?;
        std::thread::sleep(Duration::from_millis(ms(u64::from(kapacita / 2))));
    }
    // Dohrání: spí se vždy jen tak dlouho, kolik toho ještě zbývá.
    loop {
        // SAFETY: inicializovaný klient.
        let ve_fronte =
            unsafe { klient.GetCurrentPadding() }.map_err(|e| format!("GetCurrentPadding: {e}"))?;
        if ve_fronte == 0 {
            return Ok(());
        }
        vyprselo()?;
        std::thread::sleep(Duration::from_millis(ms(u64::from(ve_fronte))));
    }
}

/// Přehraje TICHO (nulové vzorky) stejnou cestou jako tón: COM,
/// výchozí výstup, formát směšovače, Initialize, Start, dohrání, Stop.
/// Synchronně na volajícím vlákně. Jen pro příklad `zvuk_selftest` —
/// sonda podvržených DLL musí projít celou cestou, a nic přitom nesmí
/// být slyšet. Vrací i paměť procesu po dohrání, kdy je klient zavřený
/// a COM ještě drží knihovny zvuku (jako vlákno zvuku aplikace).
#[doc(hidden)]
#[allow(dead_code, reason = "volá jen příklad zvuk_selftest")]
pub fn prehraj_ticho_hned() -> Result<(Prehrano, Option<Pamet>), String> {
    let _com = Com::zapni()?;
    let p = hraj_hned(Obsah::Ticho)?;
    Ok((p, Pamet::ted()))
}

/// Přehraje zvuk. Nečeká: jen předá práci vláknu zvuku (při prvním
/// zvuku ho spustí). Rozehraný zvuk se neutne, dohraje celý (useknutá
/// sinusovka by lupla); ze zvuků, které přijdou mezitím, zazní hned po
/// něm jen poslední (otevřená otázka 45).
pub fn prehraj(z: Zvuk) {
    CEKA.store(z.kod(), Ordering::Release);
    if let Some(b) = VLAKNO.get_or_init(spust_vlakno) {
        b.probud();
    }
}

/// Zvuk, na který čeká vlákno zvuku (0 = žádný).
static CEKA: AtomicU8 = AtomicU8::new(0);
/// Budík vlákna zvuku; `None` = vlákno nejde spustit (zvuk nebude).
static VLAKNO: OnceLock<Option<Arc<Budik>>> = OnceLock::new();

fn spust_vlakno() -> Option<Arc<Budik>> {
    let budik = match Budik::new() {
        Ok(b) => Arc::new(b),
        Err(e) => {
            log::warn!("zvuk: vlákno nejde připravit ({e}) — bez zvuku");
            return None;
        }
    };
    let b = Arc::clone(&budik);
    let vysledek = std::thread::Builder::new()
        .stack_size(ZASOBNIK)
        .name("keypad-zvuk".into())
        .spawn(move || {
            let mut prehravac = Prehravac::default();
            loop {
                b.cekej(None);
                // Rozehraný zvuk se dohraje celý (useknutá sinusovka by
                // lupla); požadavky mezitím se slijí do posledního,
                // který zazní hned po něm (otevřená otázka 45, bod f).
                if let Some(z) = Zvuk::z_kodu(CEKA.swap(0, Ordering::AcqRel)) {
                    // Chyba v kódu zvuku nesmí vlákno zvuku shodit:
                    // panic hook ji zapíše do logu a příští zvuk se
                    // zkusí znovu.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        prehravac.hraj(z);
                    }));
                }
            }
        });
    match vysledek {
        Ok(_) => Some(budik),
        Err(e) => {
            log::warn!("zvuk: vlákno nejde spustit ({e}) — bez zvuku");
            None
        }
    }
}

/// Stav vlákna zvuku.
#[derive(Default)]
struct Prehravac {
    /// COM na vlákně zvuku: `None` = ještě nezapnutý; `Some(None)` =
    /// nejde, zvuk nebude. Zůstává zapnutý po celý život vlákna —
    /// knihovny zvuku se tak načítají jen jednou.
    com: Option<Option<Com>>,
    /// Kolikrát se hrálo (první otevření se měří).
    prehrano: u32,
    /// Poslední pokus selhal — chyba se loguje jen jednou za výpadek,
    /// ne u každého Scroll Locku.
    selhalo: bool,
    /// Paměť procesu před prvním zvukem (před COM a knihovnami zvuku).
    pamet_pred: Option<Pamet>,
}

impl Prehravac {
    fn hraj(&mut self, z: Zvuk) {
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
        if com.is_none() {
            return;
        }
        match hraj_hned(Obsah::Ton(z)) {
            Ok(p) => {
                self.prehrano += 1;
                if self.prehrano == 1 {
                    // Poprvé se načítají knihovny zvuku — kvůli tomu má
                    // zvuk vlastní vlákno. Čas i paměť do logu, ať jsou
                    // změřené na skutečném PC; klient je už zavřený,
                    // knihovny zůstávají načtené po celý běh.
                    let pamet = match (self.pamet_pred, Pamet::ted()) {
                        (Some(pred), Some(po)) => po.prirustek(pred),
                        _ => "nezměřená".into(),
                    };
                    log::info!(
                        "zvuk: první otevření WASAPI za {} ms ({} Hz, {} kanálů, {:?}; vlákno zvuku, okno nečeká); paměť procesu po prvním zvuku: {pamet}",
                        p.otevreno.as_millis(),
                        p.format.vzorkovani,
                        p.format.kanaly,
                        p.format.typ
                    );
                }
                self.selhalo = false;
            }
            Err(e) => {
                if !self.selhalo {
                    log::warn!("zvuk nejde přehrát ({e}) — KeyPad běží dál bez něj");
                }
                self.selhalo = true;
            }
        }
    }
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

    /// Délky tónů a mezery (vzorků) při vzorkování `vz`.
    fn delky(vz: u32) -> (usize, usize, usize) {
        (
            pocet_vzorku(70, vz),
            pocet_vzorku(MEZERA_MS, vz),
            pocet_vzorku(90, vz),
        )
    }

    fn dbfs(spicka: f64) -> f64 {
        20.0 * spicka.log10()
    }

    /// Délky přesně podle tónů: 70 ms + 10 ms ticha + 90 ms.
    #[test]
    fn vzorky_delky() {
        assert_eq!(delky(48_000), (3360, 480, 4320));
        assert_eq!(delky(44_100), (3087, 441, 3969));
        assert_eq!(delky(96_000), (6720, 960, 8640));
        for vz in VZORKOVANI {
            let (a, m, b) = delky(vz);
            for z in [Zvuk::Pauza, Zvuk::Hra] {
                assert_eq!(vzorky(z.tony(), vz).len(), a + m + b, "{z:?} {vz}");
            }
            assert!(vzorky(&[], vz).is_empty());
        }
    }

    /// Špička nejvýš −14 dBFS, ale zvuk není potichu; první a poslední
    /// vzorek každého tónu ≈ 0 (bez lupnutí), mezera je ticho, náběh je
    /// plynulý.
    #[test]
    fn vzorky_spicka_a_okraje() {
        for vz in VZORKOVANI {
            let (a, m, _) = delky(vz);
            for z in [Zvuk::Pauza, Zvuk::Hra] {
                let v = vzorky(z.tony(), vz);
                let spicka = v.iter().fold(0f32, |s, x| s.max(x.abs()));
                let d = dbfs(f64::from(spicka));
                // Tolerance jen na zaokrouhlení 10^(−14/20) do f32.
                assert!(d <= -14.0 + 1e-4, "{z:?} {vz}: {d} dBFS");
                assert!(d > -14.5, "{z:?} {vz}: {d} dBFS — skoro ticho");
                assert_eq!(v[0], 0.0);
                assert_eq!(*v.last().unwrap(), 0.0);
                // Konec prvního tónu, mezera, začátek druhého.
                assert!(v[a - 1].abs() < 0.002, "{}", v[a - 1]);
                assert!(v[a..a + m].iter().all(|&s| s == 0.0));
                assert!(v[a + m].abs() < 0.002);
                // Náběh je plynulý: 3 ms od začátku ještě pod polovinou
                // špičky.
                let pul_nabehu = pocet_vzorku(3, vz);
                assert!(v[..pul_nabehu].iter().all(|s| s.abs() <= spicka / 2.0));
            }
        }
    }

    #[test]
    fn pauza_a_hra_se_lisi() {
        for vz in VZORKOVANI {
            assert_ne!(vzorky(Zvuk::Pauza.tony(), vz), vzorky(Zvuk::Hra.tony(), vz));
        }
        for z in [Zvuk::Pauza, Zvuk::Hra] {
            assert_eq!(Zvuk::z_kodu(z.kod()), Some(z));
        }
        assert_eq!(Zvuk::z_kodu(0), None);
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

    /// Převod do formátu směšovače pro 44,1/48/96 kHz, mono i stereo,
    /// float, 16 i 32 bitů: délka, stejný tón v obou kanálech, špička,
    /// okraje a ticho na konci.
    #[test]
    fn ramce_ve_formatu_smesovace() {
        for vz in VZORKOVANI {
            for kanaly in [1u16, 2] {
                for typ in [TypVzorku::F32, TypVzorku::I16, TypVzorku::I32] {
                    let f = Format {
                        vzorkovani: vz,
                        kanaly,
                        typ,
                    };
                    for z in [Zvuk::Pauza, Zvuk::Hra] {
                        let mono = vzorky(z.tony(), vz);
                        let dozvuk = pocet_vzorku(DOZVUK_MS, vz);
                        let b = ramce(Obsah::Ton(z), f);
                        assert_eq!(
                            b.len(),
                            (mono.len() + dozvuk) * usize::from(kanaly) * typ.bajtu(),
                            "{f:?}"
                        );
                        let r = dekoduj(&b, f);
                        let mut spicka = 0f64;
                        for (i, ramec) in r.iter().enumerate() {
                            assert!(ramec.iter().all(|&s| s == ramec[0]), "{f:?} rámec {i}");
                            let cekano = mono.get(i).map_or(0.0, |&v| f64::from(v));
                            assert!((ramec[0] - cekano).abs() < 1e-4, "{f:?} rámec {i}");
                            spicka = spicka.max(ramec[0].abs());
                        }
                        let d = dbfs(spicka);
                        assert!(d <= -14.0 + 1e-4, "{f:?}: {d} dBFS");
                        assert!(d > -14.5, "{f:?}: {d} dBFS");
                        assert!(r[0].iter().all(|&s| s == 0.0));
                        assert!(r[mono.len() - 1].iter().all(|&s| s == 0.0));
                        assert!(r[mono.len()..].iter().flatten().all(|&s| s == 0.0));
                    }
                }
            }
        }
    }

    /// U 5.1 hraje jen levý a pravý přední kanál.
    #[test]
    fn vic_kanalu_jen_predni_par() {
        let f = Format {
            vzorkovani: 48_000,
            kanaly: 6,
            typ: TypVzorku::F32,
        };
        let r = dekoduj(&ramce(Obsah::Ton(Zvuk::Hra), f), f);
        assert!(r.iter().any(|ramec| ramec[0] != 0.0));
        for ramec in &r {
            assert_eq!(ramec[0], ramec[1]);
            assert!(ramec[2..].iter().all(|&s| s == 0.0));
        }
    }

    /// Ticho je stejně dlouhé jako tón a celé nulové.
    #[test]
    fn ticho_je_nulove() {
        let f = Format {
            vzorkovani: 48_000,
            kanaly: 2,
            typ: TypVzorku::F32,
        };
        let ticho = ramce(Obsah::Ticho, f);
        assert_eq!(ticho.len(), ramce(Obsah::Ton(Zvuk::Pauza), f).len());
        assert!(ticho.iter().all(|&b| b == 0));
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
                let f = Format::z_waveformatex(mix.bajty());
                eprintln!("formát směšovače: {f:?} ({})", popis_formatu(mix.bajty()));
                assert!(f.is_some(), "{}", popis_formatu(mix.bajty()));
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

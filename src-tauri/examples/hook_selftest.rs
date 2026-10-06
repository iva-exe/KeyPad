//! Ruční ověření hooku klávesnice (Fáze 3). Spouští vlastník, ne testy.
//!
//! ```text
//! cargo run -p keypad --release --example hook_selftest            (60 s)
//! cargo run -p keypad --release --example hook_selftest -- 120     (sekund)
//! cargo run -p keypad --release --example hook_selftest -- instalace
//! cargo run -p keypad --release --example hook_selftest -- mereni
//! cargo run -p keypad --release --example hook_selftest -- mereni-instalace
//! ```
//!
//! Výchozí mapování (WASD, šipky, IJKL…, přepnutí Scroll Lock). Začíná
//! na Klávesnici: nic se nepotlačuje, jen se SEM do konzole vypisují
//! stisknuté klávesy se scan kódy — nic se neukládá. Scroll Lock přepne
//! na Gamepad: namapované klávesy se potlačí (Poznámkový blok je
//! nedostane), ostatní projdou. Virtuální ovladač se NEPŘIPOJUJE — stav
//! padu se jen vypíše. Konec po zadaném čase nebo Ctrl+C (hook zmizí
//! s procesem).
//!
//! `instalace`: jen nainstaluje, přeinstaluje a odebere hook a nic
//! o klávesách nevypisuje. Mapuje jen F23/F24 (zkratka F23) — klávesy,
//! které nikdo nezmáčkne, takže ani na chvilku nic nepotlačí.
//!
//! `mereni` (Fáze 6): kolik stojí jedna událost klávesnice — touž
//! funkcí jako callback (`hook::zmer_zpracovani`) a se skutečným
//! výstupem aplikace (sloty padů, atomiky okna, `SetEvent`), bez živé
//! detekce i s ní (okno v popředí). Hook se do systému NEinstaluje,
//! klávesy jsou syntetické a nikam nejdou. Vypíše p50/p99 a skončí
//! chybou, když p99 přeleze 20 µs. Pro rozbor navíc cenu snímku
//! klávesnice na téhle ploše (smyčka hook vlákna, ne callback, OQ 57):
//! jen čte stav klávesnice do zahozeného enginu, nic nevypisuje.
//!
//! `mereni-instalace` (Fáze 6, riziko 2 a otázka 42): kolik stojí
//! instalace a odebrání hooku — bez zapnutého ovladače se to děje při
//! každém získání a ztrátě popředí oknem KeyPadu (Alt+Tab). Hook se
//! instaluje na VLASTNÍ SKRYTÉ PLOŠE: LL hook vidí jen vstup plochy,
//! na které běží jeho vlákno, a ta se nikdy nezobrazí — klávesnice
//! vlastníka k němu nedojde. Callback jen předává dál. Navíc změří
//! snímek klávesnice, který v aplikaci každou instalaci doprovází (OQ 57)
//! — na téže skryté ploše, kde Windows stav klávesnice nevracejí.
//!
//! Hook, výstup, sloty i názvy kláves jsou TYTÉŽ soubory, které používá
//! aplikace (`#[path]`).

#[allow(
    dead_code,
    reason = "příklad používá jen část hooku, zbytek je pro aplikaci"
)]
#[path = "../src/platform/windows/hook.rs"]
mod hook;
#[allow(dead_code, reason = "příklad používá jen názvy, ne krátké názvy")]
#[path = "../src/platform/windows/klavesy.rs"]
mod klavesy;
/// Hook ověřuje Raw Input klávesnice na začátku přiřazování (OQ 60);
/// příklad nepřiřazuje, modul jen potřebuje, aby se hook přeložil.
#[allow(dead_code, reason = "příklad nepřiřazuje — kontrolu volá jen hook")]
#[path = "../src/platform/windows/raw_input.rs"]
mod raw_input;
#[allow(dead_code, reason = "měření potřebuje jen zápis, čtení je pro okno")]
#[path = "../src/platform/windows/slot.rs"]
mod slot;
#[allow(dead_code, reason = "měření potřebuje jen zápis, čtení je pro okno")]
#[path = "../src/platform/windows/vystup.rs"]
mod vystup;

/// Hook volá `crate::logger::mark_realtime_thread()`; příklad logger
/// nemá (vypisuje do konzole sám).
mod logger {
    pub fn mark_realtime_thread() {}
}

use std::process::ExitCode;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hook::{Hook, HookPrikaz, Udalost, Vystup};
use keypad_core::{
    Action, Decision, KeyConflict, KeyId, Mapping, Mode, PadAction, PadButton, PadId, StickDir,
    MAX_PADS,
};
use windows::core::HSTRING;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::StationsAndDesktops::{
    CreateDesktopW, SetThreadDesktop, DESKTOP_CONTROL_FLAGS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VSC_TO_VK_EX};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, PeekMessageW, SetWindowsHookExW, UnhookWindowsHookEx, MSG, PM_REMOVE,
    WH_KEYBOARD_LL,
};

/// Kapacita fronty z callbacku. Hlavní vlákno ji vybírá každých 20 ms;
/// tolik událostí za tu dobu nikdo nenaťuká.
const N: usize = 1024;

/// Fronta bez zámků: zapisuje jen callback hooku, čte jen hlavní vlákno.
/// Záznam je jedno `u64` (viz [`zabal`]) — callback tak nic nealokuje
/// a na nic nečeká (princip 3 platí i pro příklad).
struct Fronta {
    sloty: [AtomicU64; N],
    zapsano: AtomicUsize,
    precteno: AtomicUsize,
    zahozeno: AtomicU32,
}

impl Fronta {
    fn new() -> Fronta {
        Fronta {
            sloty: std::array::from_fn(|_| AtomicU64::new(0)),
            zapsano: AtomicUsize::new(0),
            precteno: AtomicUsize::new(0),
            zahozeno: AtomicU32::new(0),
        }
    }

    fn vyber(&self, mut f: impl FnMut(u64)) {
        let konec = self.zapsano.load(Ordering::Acquire);
        let mut i = self.precteno.load(Ordering::Relaxed);
        while i != konec {
            f(self.sloty[i % N].load(Ordering::Relaxed));
            i = i.wrapping_add(1);
        }
        self.precteno.store(i, Ordering::Release);
    }
}

impl Vystup for Fronta {
    fn rozhodnuti(&self, u: Option<&Udalost>, d: &Decision, rezim: Mode) {
        let w = self.zapsano.load(Ordering::Relaxed);
        if w.wrapping_sub(self.precteno.load(Ordering::Acquire)) >= N {
            self.zahozeno.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.sloty[w % N].store(zabal(u, d, rezim), Ordering::Relaxed);
        self.zapsano.store(w.wrapping_add(1), Ordering::Release);
    }
}

/// Bity: 0–15 scan, 16–23 flags, 24–31 vk, 32 dolů, 33 potlačit,
/// 34 je událost, 35 je stav padu, 36–37 režim, 38–63 pad (tlačítka,
/// triggery, znaménka os).
fn zabal(u: Option<&Udalost>, d: &Decision, rezim: Mode) -> u64 {
    let mut x = 0u64;
    if let Some(u) = u {
        x |= u64::from(u.scan.min(0xFFFF));
        x |= u64::from(u.flags & 0xFF) << 16;
        x |= u64::from(u.vk & 0xFF) << 24;
        x |= u64::from(u.dolu) << 32;
        x |= 1 << 34;
    }
    x |= u64::from(d.suppress) << 33;
    let r = match rezim {
        Mode::Keyboard => 0,
        Mode::Gamepad => 1,
        Mode::Binding { .. } => 2,
        Mode::Disabled { .. } => 3,
    };
    x |= r << 36;
    // Výchozí mapování je celé na prvním ovladači, jiný se tu neukáže.
    if let Some(p) = d.pads.get(PadId::FIRST) {
        let osa = |v: i16| -> u64 {
            match v {
                0 => 0,
                v if v > 0 => 1,
                _ => 2,
            }
        };
        x |= 1 << 35;
        x |= u64::from(p.buttons) << 38;
        x |= u64::from(p.left_trigger > 0) << 54;
        x |= u64::from(p.right_trigger > 0) << 55;
        x |= osa(p.thumb_lx) << 56;
        x |= osa(p.thumb_ly) << 58;
        x |= osa(p.thumb_rx) << 60;
        x |= osa(p.thumb_ry) << 62;
    }
    x
}

fn rezim_text(r: u64) -> &'static str {
    match r {
        0 => "Klávesnice",
        1 => "Gamepad",
        2 => "Přiřazování",
        _ => "Vypnuto",
    }
}

fn pad_text(x: u64) -> String {
    let tlacitka = (x >> 38) as u16;
    let mut s: Vec<String> = PadButton::ALL
        .iter()
        .filter(|b| tlacitka & b.mask() != 0)
        .map(|b| format!("{b:?}"))
        .collect();
    if x >> 54 & 1 != 0 {
        s.push("LT".into());
    }
    if x >> 55 & 1 != 0 {
        s.push("RT".into());
    }
    let smer = |posun: u32, kladny: &str, zaporny: &str| match x >> posun & 3 {
        1 => kladny.to_string(),
        2 => zaporny.to_string(),
        _ => String::new(),
    };
    let leva = smer(56, "→", "←") + &smer(58, "↑", "↓");
    let prava = smer(60, "→", "←") + &smer(62, "↑", "↓");
    if !leva.is_empty() {
        s.push(format!("L{leva}"));
    }
    if !prava.is_empty() {
        s.push(format!("R{prava}"));
    }
    if s.is_empty() {
        "neutrál".into()
    } else {
        s.join(" ")
    }
}

fn vypis(x: u64, posledni_rezim: &mut u64, posledni_pad: &mut Option<u64>) {
    let rezim = x >> 36 & 3;
    if x >> 34 & 1 != 0 {
        let scan = (x & 0xFFFF) as u32;
        let flags = (x >> 16 & 0xFF) as u32;
        let vk = x >> 24 & 0xFF;
        let dolu = x >> 32 & 1 != 0;
        let potlacit = x >> 33 & 1 != 0;
        let vstrcena = flags & 0x10 != 0;
        let klavesa = KeyId {
            scan: scan as u16,
            extended: flags & 0x01 != 0,
        };
        let jmeno = if klavesa.is_mappable() {
            klavesy::nazev(klavesa)
        } else {
            "(nemapovatelná)".into()
        };
        println!(
            "{} {:<16} scan 0x{scan:03X}{} vk 0x{vk:02X}{}{}",
            if dolu { "↓" } else { "↑" },
            jmeno,
            if klavesa.extended { " E0" } else { "   " },
            if vstrcena { "  vstříknutá" } else { "" },
            if potlacit { "  POTLAČENO" } else { "" },
        );
    }
    if rezim != *posledni_rezim {
        println!("── režim: {}", rezim_text(rezim));
        *posledni_rezim = rezim;
    }
    if x >> 35 & 1 != 0 {
        let pad = x >> 38;
        if *posledni_pad != Some(pad) {
            println!("   pad: {}", pad_text(x));
            *posledni_pad = Some(pad);
        }
    }
}

fn jen_instalace() -> ExitCode {
    let fronta: Arc<dyn Vystup> = Arc::new(Fronta::new());
    let f23 = KeyId::new(0x6E);
    let f24 = KeyId::new(0x76);
    let Ok(mapovani) = Mapping::new(f23, [(f24, PadAction::first(Action::Button(PadButton::A)))])
    else {
        return ExitCode::FAILURE;
    };
    let mut h = match Hook::spust(mapovani, fronta) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("hook vlákno nejde spustit: {e}");
            return ExitCode::FAILURE;
        }
    };
    let st = Arc::clone(h.status());
    let cekej = |n: u32| {
        let konec = Instant::now() + Duration::from_secs(3);
        while st.instalaci() < n && Instant::now() < konec {
            std::thread::sleep(Duration::from_millis(5));
        }
        st.instalaci() >= n && st.nainstalovan()
    };
    let bez_ovladace = !st.nainstalovan();
    // Jako by se připojil ovladač: teprve teď hook do systému.
    let nainstalovano = h.posli(HookPrikaz::Povol(PadId::FIRST)) && cekej(1);
    let preinstalovano = h.posli(HookPrikaz::Preinstaluj) && cekej(2);
    let zastaveno = h.zastav() && !st.nainstalovan();
    println!(
        "bez ovladače bez hooku {}, instalace {}, přeinstalace {}, odebrání {}",
        if bez_ovladace { "ok" } else { "SELHALO" },
        if nainstalovano { "ok" } else { "SELHALA" },
        if preinstalovano { "ok" } else { "SELHALA" },
        if zastaveno { "ok" } else { "SELHALO" }
    );
    if bez_ovladace && nainstalovano && preinstalovano && zastaveno {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Kolik událostí se měří (v každém režimu).
const MERENI_N: usize = 500_000;
/// Strop p99 ze specifikace Fáze 6 (B4).
const MERENI_P99_NS: u64 = 20_000;

/// Percentil `q` ze seřazených časů.
fn percentil(serazene: &[u64], q: f64) -> u64 {
    let i = ((serazene.len().saturating_sub(1)) as f64 * q).round() as usize;
    serazene.get(i).copied().unwrap_or(0)
}

/// Virtuální klávesa, jakou by událost nesla ve skutečném callbacku —
/// podle ní callback sleduje Win (bity pravidla Win+klávesa).
fn vk_klavesy(k: KeyId) -> u32 {
    let scan = u32::from(k.scan) | if k.extended { 0xE000 } else { 0 };
    // SAFETY: jen převod kódu podle rozložení klávesnice, nic nemění.
    unsafe { MapVirtualKeyW(scan, MAPVK_VSC_TO_VK_EX) }
}

/// Výchozí mapování, ve kterém prvních šest kláves (ty měření mačká)
/// ovládá 4 vstupy dvou ovladačů — nejdražší sdílená klávesa (Fáze 7):
/// stav padu i živý stav se počítají ze všech jejích cílů.
fn sdilene_mapovani() -> Mapping {
    let mut m = Mapping::default();
    let druhy = PadId::ALL[1];
    let klavesy: Vec<KeyId> = m.keys().map(|(k, _)| k).take(6).collect();
    for k in klavesy {
        for a in [
            Action::LeftStick(StickDir::Up),
            Action::Button(PadButton::A),
            Action::RightTrigger,
        ] {
            let _ = m.bind(k, PadAction::new(druhy, a), KeyConflict::Share);
        }
    }
    m
}

fn mereni() -> ExitCode {
    let sloty: Result<Vec<Arc<slot::StavSlot>>, String> = (0..MAX_PADS)
        .map(|_| slot::StavSlot::new().map(Arc::new))
        .collect();
    let (sloty, budik) = match (sloty, slot::Budik::new()) {
        (Ok(s), Ok(b)) => (s, Arc::new(b)),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("události Windows nejde vytvořit: {e}");
            return ExitCode::FAILURE;
        }
    };
    let sloty: [Arc<slot::StavSlot>; MAX_PADS] = std::array::from_fn(|i| Arc::clone(&sloty[i]));
    let vystup: Arc<dyn Vystup> = Arc::new(vystup::HookVystup::new(sloty, budik));
    // Zahřátí (cache, větvení) se nepočítá.
    let _ = hook::zmer_zpracovani(
        20_000,
        Mapping::default(),
        Arc::clone(&vystup),
        true,
        vk_klavesy,
    );
    println!(
        "Měření zpracování události (syntetické klávesy, hook se neinstaluje), {MERENI_N} událostí:"
    );
    let mut v_limitu = true;
    // Na stav klávesnice se callback od opravy OQ 57 neptá vůbec —
    // dřívější třetí řádek „bez GetAsyncKeyState" je teď první dva.
    // Fáze 7: totéž se sdílenými klávesami o 4 cílech na dvou ovladačích.
    for (zive, sdilene, popis) in [
        (false, false, "bez živé detekce         "),
        (true, false, "s živou detekcí (popředí)"),
        (false, true, "sdílené 4 cíle, bez živé "),
        (true, true, "sdílené 4 cíle, s živou  "),
    ] {
        let mapovani = if sdilene {
            sdilene_mapovani()
        } else {
            Mapping::default()
        };
        let mut casy =
            hook::zmer_zpracovani(MERENI_N, mapovani, Arc::clone(&vystup), zive, vk_klavesy);
        casy.sort_unstable();
        let p50 = percentil(&casy, 0.50);
        let p99 = percentil(&casy, 0.99);
        let p999 = percentil(&casy, 0.999);
        let max = casy.last().copied().unwrap_or(0);
        println!(
            "  {popis}  p50 {p50:>6} ns   p99 {p99:>6} ns   p99,9 {p999:>7} ns   max {max:>8} ns"
        );
        v_limitu &= p99 < MERENI_P99_NS;
    }
    // Snímek klávesnice (smyčka hook vlákna po instalaci hooku a po
    // zapomenutí, ne callback) na téhle ploše — na skryté ploše
    // `mereni-instalace` jde Windows pomalejší cestou. Jen čte stav
    // klávesnice do zahozeného enginu, nic nevypisuje ani nevstřikuje.
    // Do limitu se nepočítá (neběží v callbacku).
    let mut snimky = hook::zmer_snimek(2_000, Mapping::default(), Arc::clone(&vystup));
    snimky.sort_unstable();
    println!(
        "  snímek klávesnice (smyčka)  p50 {:>6} ns   p99 {:>6} ns   max {:>8} ns",
        percentil(&snimky, 0.50),
        percentil(&snimky, 0.99),
        snimky.last().copied().unwrap_or(0)
    );
    if v_limitu {
        println!("p99 pod {} µs — v pořádku", MERENI_P99_NS / 1000);
        ExitCode::SUCCESS
    } else {
        println!("p99 PŘES {} µs", MERENI_P99_NS / 1000);
        ExitCode::FAILURE
    }
}

/// Kolikrát se hook nainstaluje a odebere v `mereni-instalace`.
const INSTALACE_N: usize = 2_000;

/// Callback měřeného hooku: nic nerozhoduje, jen předá dál.
unsafe extern "system" fn propust(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: předání dalšímu hooku v řetězu se stejnými parametry.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Kolik snímků klávesnice se změří v `mereni-instalace`.
const SNIMKY_N: usize = 2_000;

/// Instalace a odebrání hooku na skryté ploše: časy v ns (instalace,
/// odebrání) pro každé kolo, a pak časy snímku klávesnice, který každou
/// instalaci v aplikaci doprovází. Vlákno se na plochu přesune dřív, než
/// vytvoří jakékoli okno nebo hook (jinak `SetThreadDesktop` selže);
/// skrytá plocha není vstupní, takže snímek skutečnou klávesnici
/// vlastníka nečte (Windows tam vrací nuly, cena volání je stejná).
type Casy = (Vec<(u64, u64)>, Vec<u64>);

fn zmer_instalace(n: usize) -> Result<Casy, String> {
    let jmeno = HSTRING::from(format!("KeyPadMereni{}", std::process::id()));
    // SAFETY: nová plocha s výchozími právy; handle zůstává otevřený po
    // celé měření (SetThreadDesktop ho potřebuje) a zavře se na konci.
    let plocha = unsafe {
        CreateDesktopW(
            &jmeno,
            None,
            None,
            DESKTOP_CONTROL_FLAGS(0),
            0x1000_0000, // GENERIC_ALL
            None,
        )
    }
    .map_err(|e| format!("CreateDesktopW: {e}"))?;
    // SAFETY: platný handle plochy; vlákno ještě nemá okna ani hooky.
    unsafe { SetThreadDesktop(plocha) }.map_err(|e| format!("SetThreadDesktop: {e}"))?;
    // SAFETY: modul vlastního .exe.
    let modul = unsafe { GetModuleHandleW(None) }.map_err(|e| format!("GetModuleHandleW: {e}"))?;
    let mut casy = Vec::with_capacity(n);
    let mut msg = MSG::default();
    for _ in 0..n {
        let t0 = Instant::now();
        // SAFETY: callback je funkce tohoto modulu a žije po celý běh.
        let h = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(propust), Some(modul.into()), 0) }
            .map_err(|e| format!("SetWindowsHookExW: {e}"))?;
        let t1 = Instant::now();
        // SAFETY: handle právě vrácený SetWindowsHookExW, odebírá se jednou.
        let _ = unsafe { UnhookWindowsHookEx(h) };
        let t2 = Instant::now();
        casy.push(((t1 - t0).as_nanos() as u64, (t2 - t1).as_nanos() as u64));
        // Vlákno s hookem má pumpovat zprávy — kdyby přece něco přišlo.
        // SAFETY: platný ukazatel na MSG.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {}
    }
    let vystup: Arc<dyn Vystup> = Arc::new(Fronta::new());
    // Zahřátí se nepočítá.
    let _ = hook::zmer_snimek(100, Mapping::default(), Arc::clone(&vystup));
    let snimky = hook::zmer_snimek(SNIMKY_N, Mapping::default(), vystup);
    // Plochu zavře až konec vlákna (je mu přiřazená); handle se zahodí
    // s procesem. Plocha zanikne s posledním handlem a vláknem.
    Ok((casy, snimky))
}

fn mereni_instalace() -> ExitCode {
    let vysledek = std::thread::spawn(|| zmer_instalace(INSTALACE_N)).join();
    let (casy, snimky) = match vysledek {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            eprintln!("měření nejde: {e}");
            return ExitCode::FAILURE;
        }
        Err(_) => {
            eprintln!("vlákno měření spadlo");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "Instalace a odebrání hooku klávesnice (vlastní skrytá plocha, {INSTALACE_N} kol, \
         jen SetWindowsHookExW / UnhookWindowsHookEx):"
    );
    let radek = |popis: &str, mut v: Vec<u64>| {
        v.sort_unstable();
        println!(
            "  {popis:<20} p50 {:>7} ns   p99 {:>7} ns   max {:>8} ns",
            percentil(&v, 0.50),
            percentil(&v, 0.99),
            v.last().copied().unwrap_or(0)
        );
    };
    radek("instalace", casy.iter().map(|c| c.0).collect());
    radek("odebrání", casy.iter().map(|c| c.1).collect());
    radek(
        "instalace + odebrání",
        casy.iter().map(|c| c.0 + c.1).collect(),
    );
    println!(
        "Snímek klávesnice po instalaci (smyčka hook vlákna, ne callback; {SNIMKY_N} snímků, \
         GetAsyncKeyState přes všechny virtuální klávesy):"
    );
    radek("snímek", snimky);
    println!(
        "Bez zapnutého ovladače: 1 instalace (+ snímek), když okno KeyPadu získá popředí, \
         1 odebrání, když ho ztratí (se schovaným oknem ani se zapnutým ovladačem žádné)."
    );
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    if arg.as_deref() == Some("instalace") {
        return jen_instalace();
    }
    if arg.as_deref() == Some("mereni") {
        return mereni();
    }
    if arg.as_deref() == Some("mereni-instalace") {
        return mereni_instalace();
    }
    let sekund: u64 = match arg.as_deref().map(str::parse) {
        None => 60,
        Some(Ok(s)) if (1..=3600).contains(&s) => s,
        _ => {
            eprintln!(
                "použití: hook_selftest [sekund 1–3600 | instalace | mereni | mereni-instalace]"
            );
            return ExitCode::from(2);
        }
    };

    let fronta = Arc::new(Fronta::new());
    let vystup: Arc<dyn Vystup> = fronta.clone();
    let mut h = match Hook::spust(Mapping::default(), vystup) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("hook nejde nainstalovat: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Jako by se připojil pad: z „Vypnuto" na Klávesnici. Na Gamepad
    // přepne až Scroll Lock.
    h.posli(HookPrikaz::Povol(PadId::FIRST));
    println!(
        "Hook běží {sekund} s. Klávesy se vypisují jen sem, nic se neukládá.\n\
         Scroll Lock = Klávesnice ↔ Gamepad (WASD, šipky, IJKL… se v Gamepadu potlačí).\n\
         Virtuální ovladač se nepřipojuje. Ctrl+C = konec."
    );

    let konec = Instant::now() + Duration::from_secs(sekund);
    let mut rezim = 3;
    let mut pad = None;
    while Instant::now() < konec {
        std::thread::sleep(Duration::from_millis(20));
        fronta.vyber(|x| vypis(x, &mut rezim, &mut pad));
        if h.status().rozbity() {
            break;
        }
    }
    let st = Arc::clone(h.status());
    let zastaveno = h.zastav();
    fronta.vyber(|x| vypis(x, &mut rezim, &mut pad));
    println!(
        "konec: paniky {}, zahozeno {}, hook {}",
        st.paniky(),
        fronta.zahozeno.load(Ordering::Relaxed),
        if zastaveno { "odebrán" } else { "NEODEBRÁN" }
    );
    if zastaveno && st.paniky() == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

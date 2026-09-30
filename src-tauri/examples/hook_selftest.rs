//! Ruční ověření hooku klávesnice (Fáze 3). Spouští vlastník, ne testy.
//!
//! ```text
//! cargo run -p keypad --release --example hook_selftest            (60 s)
//! cargo run -p keypad --release --example hook_selftest -- 120     (sekund)
//! cargo run -p keypad --release --example hook_selftest -- instalace
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
//! Hook i názvy kláves jsou TYTÉŽ soubory, které používá aplikace
//! (`#[path]`).

#[allow(
    dead_code,
    reason = "příklad používá jen část hooku, zbytek je pro aplikaci"
)]
#[path = "../src/platform/windows/hook.rs"]
mod hook;
#[path = "../src/platform/windows/klavesy.rs"]
mod klavesy;

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
use keypad_core::{Action, Decision, KeyId, Mapping, Mode, PadAction, PadButton, PadId};

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

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    if arg.as_deref() == Some("instalace") {
        return jen_instalace();
    }
    let sekund: u64 = match arg.as_deref().map(str::parse) {
        None => 60,
        Some(Ok(s)) if (1..=3600).contains(&s) => s,
        _ => {
            eprintln!("použití: hook_selftest [sekund 1–3600 | instalace]");
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

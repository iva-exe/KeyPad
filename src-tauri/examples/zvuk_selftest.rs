//! Ruční ověření zvuku pozastavení (Fáze 4b, nové zvuky a plánovač
//! Fáze 7).
//!
//! ```text
//! cargo run -p keypad --release --example zvuk_selftest -- ticho
//! cargo run -p keypad --release --example zvuk_selftest -- pauza        (spouští jen vlastník)
//! cargo run -p keypad --release --example zvuk_selftest -- hra          (spouští jen vlastník)
//! cargo run -p keypad --release --example zvuk_selftest -- pauza-glis   (spouští jen vlastník)
//! cargo run -p keypad --release --example zvuk_selftest -- ukazka       (spouští jen vlastník)
//! cargo run -p keypad --release --example zvuk_selftest -- rychle       (spouští jen vlastník)
//! ```
//!
//! `ticho`: celá cesta jako naostro — COM, předehřátí (výchozí výstup,
//! formát směšovače, vzorky), `Initialize` s událostí, plnění po
//! periodách, druhý požadavek do rozehraného prvního (doběh, ticho, další
//! zvuk), dozvuk, `Stop` — ale se samými NULOVÝMI vzorky, nic není
//! slyšet. Na stejně velkém zásobníku jako vlákno zvuku v aplikaci. To je
//! cíl sondy podvržených DLL: binárka zkopírovaná do složky s podvrženými
//! DLL (MMDevAPI, AudioSes, wdmaud.drv…) žádnou z nich nesmí načíst.
//! Vypíše, odkud se načetly knihovny zvuku, a ohlásí, kdyby se do procesu
//! dostala winmm.dll. A kolik paměti procesu zvuk přidal: po dohrání
//! s COM, který drží knihovny zvuku (jako vlákno zvuku aplikace po celý
//! běh), a po `CoUninitialize` a konci vlákna. Exit 0 = v pořádku,
//! 1 = chyba, 4 = přehrálo se, ale fronta zařízení podtekla.
//!
//! Slyšitelné režimy jdou přes `prehraj` a vlákno zvuku aplikace (po
//! předehřátí, jako po zapnutí ovladače) — jen pro vlastníka, ne pro
//! testy a sondy:
//! - `pauza` / `hra`: jeden zvuk;
//! - `pauza-glis`: varianta Pauzy s glissandem A4 → G4 k porovnání;
//! - `ukazka`: Pauza, 0,6 s ticha, Hra (▷ v nastavení);
//! - `rychle`: jako Scroll Lock 5× za 0,5 s — nic se nesmí slít ani
//!   překrýt, zazní jen kousky utnuté doběhem a nakonec celá Pauza.
//!
//! Zvuk je TENTÝŽ soubor, který používá aplikace (`#[path]`).

#[path = "../src/platform/windows/dll.rs"]
mod dll;
#[allow(
    dead_code,
    reason = "příklad volá jen část rozhraní zvuku, zbytek používá aplikace"
)]
#[path = "../src/platform/windows/zvuk.rs"]
mod zvuk;

use std::process::ExitCode;
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};

/// Logy zvuku do konzole (aplikace je píše do souboru).
struct Konzole;

impl log::Log for Konzole {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, r: &log::Record) {
        eprintln!("[{}] {}", r.level(), r.args());
    }

    fn flush(&self) {}
}

static KONZOLE: Konzole = Konzole;

/// Odkud je načtený modul `dll` (`None` = v procesu není).
fn odkud(dll: PCWSTR) -> Option<String> {
    // SAFETY: jen dotaz na už načtený modul.
    let modul = unsafe { GetModuleHandleW(dll) }.ok()?;
    let mut buf = [0u16; 260];
    // SAFETY: platný modul, buffer žije po celou dobu volání.
    let n = unsafe { GetModuleFileNameW(Some(modul), &mut buf) } as usize;
    Some(String::from_utf16_lossy(&buf[..n]))
}

fn ticho() -> ExitCode {
    let pred = zvuk::Pamet::ted();
    let vlakno = std::thread::Builder::new()
        .stack_size(zvuk::ZASOBNIK)
        .name("keypad-zvuk".into())
        .spawn(zvuk::ticho_cela_cesta);
    let vysledek = match vlakno.map(|v| v.join()) {
        Ok(Ok(v)) => v,
        Ok(Err(_)) => {
            eprintln!("panika ve vlákně zvuku");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("vlákno nejde spustit: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut kod = match vysledek {
        Ok((pripraveno, s, s_com)) => {
            println!(
                "předehřátí: {} ms ({} Hz, {} kanálů, {:?}; {})",
                pripraveno.za.as_millis(),
                pripraveno.format.vzorkovani,
                pripraveno.format.kanaly,
                pripraveno.format.typ,
                pripraveno.popis
            );
            println!(
                "ticho přehráno: otevření {} ms, {}; doběh {}×, podtečení {}",
                s.otevreno.as_millis(),
                s.zvuky(),
                s.dobehu,
                s.podteceni
            );
            let bez_com = zvuk::Pamet::ted();
            match (pred, s_com, bez_com) {
                (Some(pred), Some(s_com), Some(bez_com)) => {
                    println!(
                        "paměť po zvuku, COM drží knihovny (jako aplikace): {}",
                        s_com.prirustek(pred)
                    );
                    println!(
                        "paměť po CoUninitialize a konci vlákna: {}",
                        bez_com.prirustek(pred)
                    );
                }
                _ => println!("paměť nezměřená"),
            }
            if s.zvuku != 2 || s.dobehu != 1 {
                println!(
                    "POZOR: cesta doběhu se neprošla celá (zvuků {}, doběhů {}) — zařízení se otevíralo příliš dlouho?",
                    s.zvuku, s.dobehu
                );
            }
            if s.podteceni > 0 {
                println!("POZOR: fronta zařízení podtekla {}×", s.podteceni);
                ExitCode::from(4)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            println!("ticho nejde přehrát: {e}");
            ExitCode::FAILURE
        }
    };
    for (jmeno, dll) in [
        ("MMDevAPI.dll", w!("mmdevapi.dll")),
        ("AudioSes.dll", w!("audioses.dll")),
    ] {
        println!(
            "{jmeno}: {}",
            odkud(dll).unwrap_or_else(|| "nenačtená".into())
        );
    }
    for (jmeno, dll) in [
        ("winmm.dll", w!("winmm.dll")),
        ("wdmaud.drv", w!("wdmaud.drv")),
    ] {
        if let Some(cesta) = odkud(dll) {
            println!("CHYBA: v procesu je {jmeno} ({cesta}) — zvuk má jít jen přes WASAPI");
            kod = ExitCode::FAILURE;
        }
    }
    kod
}

fn main() -> ExitCode {
    // ÚPLNĚ PRVNÍ, stejně jako v aplikaci a TÝMŽ kódem: sonda
    // podvržených DLL musí zkoušet totéž pořadí hledání, se kterým běží
    // KeyPad.exe. Bez něj zvuk nehraje.
    if let Err(e) = dll::jen_ze_system32() {
        eprintln!("SetDefaultDllDirectories: {e}");
        return ExitCode::FAILURE;
    }
    let _ = log::set_logger(&KONZOLE).map(|()| log::set_max_level(log::LevelFilter::Info));

    let rezim = std::env::args().nth(1).unwrap_or_default();
    let ms = Duration::from_millis;
    // Slyšitelné: napřed předehřátí jako po zapnutí ovladače, ať první
    // zvuk nečeká na načítání knihoven.
    let slysitelne = |f: &dyn Fn()| {
        zvuk::priprav();
        std::thread::sleep(ms(800));
        f();
    };
    match rezim.as_str() {
        "ticho" => return ticho(),
        "pauza" => slysitelne(&|| zvuk::prehraj(zvuk::Zvuk::Pauza)),
        "hra" => slysitelne(&|| zvuk::prehraj(zvuk::Zvuk::Hra)),
        "pauza-glis" => slysitelne(&zvuk::prehraj_variantu_glis),
        "ukazka" => slysitelne(&zvuk::prehraj_ukazku),
        "rychle" => slysitelne(&|| {
            for (i, z) in [
                zvuk::Zvuk::Pauza,
                zvuk::Zvuk::Hra,
                zvuk::Zvuk::Pauza,
                zvuk::Zvuk::Hra,
                zvuk::Zvuk::Pauza,
            ]
            .into_iter()
            .enumerate()
            {
                if i > 0 {
                    std::thread::sleep(ms(100));
                }
                zvuk::prehraj(z);
            }
        }),
        _ => {
            eprintln!("použití: zvuk_selftest ticho | pauza | hra | pauza-glis | ukazka | rychle");
            return ExitCode::from(2);
        }
    }
    // `prehraj` nečeká (vlákno okna nesmí); nejdelší (ukázka) i s otevřením
    // výstupu a dozvukem se vejde do dvou a půl sekundy.
    std::thread::sleep(ms(2_500));
    zvuk::souhrn_pri_konci();
    ExitCode::SUCCESS
}

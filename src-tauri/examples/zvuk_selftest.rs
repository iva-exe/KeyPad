//! Ruční ověření zvuku pozastavení (Fáze 4b).
//!
//! ```text
//! cargo run -p keypad --release --example zvuk_selftest -- ticho
//! cargo run -p keypad --release --example zvuk_selftest -- pauza   (spouští jen vlastník)
//! cargo run -p keypad --release --example zvuk_selftest -- hra     (spouští jen vlastník)
//! ```
//!
//! `ticho`: celá cesta WASAPI jako u tónu (COM, výchozí výstup, formát
//! směšovače, Initialize, Start, dohrání, Stop), ale se samými nulovými
//! vzorky — nic není slyšet. Na stejně velkém zásobníku jako vlákno
//! zvuku v aplikaci. To je cíl sondy podvržených DLL: binárka
//! zkopírovaná do složky s podvrženými DLL (MMDevAPI, AudioSes,
//! wdmaud.drv…) žádnou z nich nesmí načíst. Vypíše, odkud se načetly
//! knihovny zvuku, a ohlásí, kdyby se do procesu dostala winmm.dll.
//! A kolik paměti procesu zvuk přidal: po dohrání s COM, který drží
//! knihovny zvuku (jako vlákno zvuku aplikace po celý běh), a po
//! `CoUninitialize` a konci vlákna.
//!
//! `pauza` / `hra`: skutečný dvojtón přes `prehraj` a vlákno zvuku
//! aplikace. Slyšitelné — jen pro vlastníka, ne pro testy a sondy.
//!
//! Zvuk je TENTÝŽ soubor, který používá aplikace (`#[path]`).

#[path = "../src/platform/windows/dll.rs"]
mod dll;
#[allow(
    dead_code,
    reason = "příklad používá jen budík, zbytek slotu je pro pad vlákna"
)]
#[path = "../src/platform/windows/slot.rs"]
mod slot;
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
        .spawn(zvuk::prehraj_ticho_hned);
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
        Ok((p, s_com)) => {
            println!(
                "ticho přehráno: otevření {} ms, {} Hz, {} kanálů, {:?}",
                p.otevreno.as_millis(),
                p.format.vzorkovani,
                p.format.kanaly,
                p.format.typ
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
            ExitCode::SUCCESS
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
    let zvuk = match rezim.as_str() {
        "ticho" => return ticho(),
        "pauza" => zvuk::Zvuk::Pauza,
        "hra" => zvuk::Zvuk::Hra,
        _ => {
            eprintln!("použití: zvuk_selftest ticho | pauza | hra");
            return ExitCode::from(2);
        }
    };
    zvuk::prehraj(zvuk);
    // `prehraj` nečeká (vlákno okna nesmí); dvojtón i s prvním otevřením
    // výstupu se vejde do sekundy a půl.
    std::thread::sleep(Duration::from_millis(1500));
    ExitCode::SUCCESS
}

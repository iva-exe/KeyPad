//! Bezokenní ověření virtuálního padu na skutečném ViGEmBus (Fáze 2).
//!
//! ```text
//! cargo run -p keypad --release --example pad_selftest -- vse
//! ```
//!
//! Režimy: `popredi` (jen kontrola, jestli se smí připojit pad),
//! `xinput` (které sloty XInput jsou obsazené), `e2e`, `kill`, `vse`
//! (`e2e` + `kill`). `drz` je pomocný režim dítěte pro `kill`.
//!
//! Posílá JEN hodnoty pod mrtvými zónami XInput a žádná tlačítka, pad
//! je připojený nejvýš pár sekund — kdyby na PC zrovna běžela hra,
//! nic nezaregistruje. A před každým připojením se ptá, co je v popředí:
//! celoobrazovkové okno, běžící hra nebo herní launcher = nic nepřipojí
//! a skončí kódem 3.
//!
//! Klient ViGEmBus je TÝŽ soubor, který používá aplikace (`#[path]`).
//! XInput (xinput1_4.dll) importuje jen tenhle příklad, ne KeyPad.exe.

#[allow(
    dead_code,
    reason = "příklad používá jen část klienta, zbytek je pro aplikaci"
)]
#[path = "../src/platform/windows/vigem.rs"]
mod vigem;

use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use vigem::{Bus, VigemError, XusbReport, X360};
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::XboxController::{XInputGetState, XINPUT_STATE};
use windows::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId,
    IsZoomed, GWL_STYLE, WS_CAPTION,
};

/// Pod mrtvými zónami XInput (levá páčka 7849, pravá 8689, trigger 30),
/// bez tlačítek.
const SONDA: XusbReport = XusbReport {
    buttons: 0,
    lt: 7,
    rt: 13,
    lx: 1111,
    ly: -2222,
    rx: 3333,
    ry: -4444,
};
const NEUTRAL: XusbReport = XusbReport::NEUTRAL;

/// Dítě v režimu `drz` drží pad nejvýš takhle dlouho (pojistka).
const DRZ_MAX: Duration = Duration::from_secs(4);

/// Procesy, u kterých se pad nepřipojuje, když jsou v popředí: herní
/// klienti a streamování (Steam Big Picture, Remote Play…).
const HERNI_PROCESY: &[&str] = &[
    "steam.exe",
    "steamwebhelper.exe",
    "gameoverlayui.exe",
    "epicgameslauncher.exe",
    "battle.net.exe",
    "galaxyclient.exe",
    "eadesktop.exe",
    "origin.exe",
    "upc.exe",
    "riotclientservices.exe",
    "leagueclientux.exe",
    "minecraft.windows.exe",
    "javaw.exe",
    "robloxplayerbeta.exe",
    "parsecd.exe",
    "moonlight.exe",
    "xboxpcapp.exe",
    "gamebar.exe",
];

/// Části cest, ve kterých bydlí hry.
const HERNI_SLOZKY: &[&str] = &[
    "\\steamapps\\",
    "\\epic games\\",
    "\\xboxgames\\",
    "\\gog galaxy\\games\\",
    "\\riot games\\",
    "\\ubisoft game launcher\\games\\",
    "\\ea games\\",
];

/// Smí se teď připojit virtuální pad? `Err` = proč ne.
fn kontrola_popredi() -> Result<String, String> {
    // SAFETY: jen dotaz na stav, výstup do lokální proměnné.
    if let Ok(stav) = unsafe { SHQueryUserNotificationState() } {
        if stav == QUNS_RUNNING_D3D_FULL_SCREEN
            || stav == QUNS_BUSY
            || stav == QUNS_PRESENTATION_MODE
        {
            return Err(format!("běží celoobrazovková aplikace (QUNS {})", stav.0));
        }
    }
    // SAFETY: bez parametrů.
    let okno = unsafe { GetForegroundWindow() };
    if okno.is_invalid() {
        return Ok("v popředí není žádné okno".into());
    }
    let trida = trida_okna(okno);
    let exe = proces_okna(okno).unwrap_or_default();
    let jmeno = exe.rsplit('\\').next().unwrap_or("").to_ascii_lowercase();
    let popis = format!("{jmeno} [{trida}]");
    // Plocha (Progman/WorkerW) pokrývá celý monitor, ale hra to není.
    let plocha = jmeno == "explorer.exe" && (trida == "Progman" || trida == "WorkerW");
    // Maximalizované okno s titulkem taky (se skrývaným hlavním panelem
    // přesahuje monitor o rámeček) — celoobrazovkové hry a videa jsou
    // bez titulku a nemaximalizované.
    let maximalizovane = obycejne_maximalizovane(okno);
    if !plocha && !maximalizovane && pokryva_monitor(okno) {
        return Err(format!("okno v popředí je přes celou obrazovku: {popis}"));
    }
    if HERNI_PROCESY.contains(&jmeno.as_str()) {
        return Err(format!("v popředí je herní klient: {popis}"));
    }
    let cesta = exe.to_ascii_lowercase();
    if HERNI_SLOZKY.iter().any(|s| cesta.contains(s)) {
        return Err(format!("v popředí je hra: {exe}"));
    }
    Ok(format!("v popředí: {popis}"))
}

fn obycejne_maximalizovane(okno: HWND) -> bool {
    // SAFETY: jen dotazy na styl okna.
    unsafe {
        let styl = GetWindowLongW(okno, GWL_STYLE) as u32;
        IsZoomed(okno).as_bool() && styl & WS_CAPTION.0 == WS_CAPTION.0
    }
}

fn trida_okna(okno: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: buffer žije po celou dobu volání.
    let n = unsafe { GetClassNameW(okno, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn proces_okna(okno: HWND) -> Option<String> {
    let mut pid = 0u32;
    // SAFETY: výstup do lokální proměnné.
    unsafe { GetWindowThreadProcessId(okno, Some(&mut pid)) };
    if pid == 0 {
        return None;
    }
    // SAFETY: jen dotaz na jméno; handle se hned zavírá.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..n as usize]))
    }
}

fn pokryva_monitor(okno: HWND) -> bool {
    let mut r = RECT::default();
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: výstupy do lokálních proměnných správné velikosti.
    unsafe {
        if GetWindowRect(okno, &mut r).is_err() {
            return false;
        }
        let m = MonitorFromWindow(okno, MONITOR_DEFAULTTONEAREST);
        if !GetMonitorInfoW(m, &mut mi).as_bool() {
            return false;
        }
    }
    let mon = mi.rcMonitor;
    r.left <= mon.left && r.top <= mon.top && r.right >= mon.right && r.bottom >= mon.bottom
}

/// Stav slotu XInput; `Err(1167)` = nic nepřipojeno.
fn xinput(slot: u32) -> Result<XusbReport, u32> {
    let mut s = XINPUT_STATE::default();
    // SAFETY: výstup do lokální proměnné.
    let r = unsafe { XInputGetState(slot, &mut s) };
    if r != 0 {
        return Err(r);
    }
    let g = s.Gamepad;
    Ok(XusbReport {
        buttons: g.wButtons.0,
        lt: g.bLeftTrigger,
        rt: g.bRightTrigger,
        lx: g.sThumbLX,
        ly: g.sThumbLY,
        rx: g.sThumbRX,
        ry: g.sThumbRY,
    })
}

fn obsazene_sloty() -> Vec<u32> {
    (0..4).filter(|&i| xinput(i).is_ok()).collect()
}

/// Čeká (s krátkým spánkem), až `f` vrátí `Some`.
fn cekej<T>(limit: Duration, mut f: impl FnMut() -> Option<T>) -> Option<(T, Duration)> {
    let t = Instant::now();
    while t.elapsed() < limit {
        if let Some(v) = f() {
            return Some((v, t.elapsed()));
        }
        std::thread::sleep(Duration::from_micros(250));
    }
    None
}

/// Poslat stav; 170 (report zahozen) → znovu, jako pad vlákno.
fn posli(pad: &mut X360, r: XusbReport) -> Result<u32, VigemError> {
    let t = Instant::now();
    let mut pokusy = 0;
    loop {
        pokusy += 1;
        match pad.submit(r) {
            Ok(()) => return Ok(pokusy),
            Err(VigemError::Busy) if t.elapsed() < Duration::from_millis(250) => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Připojit a počkat na slot XInput — stejná pravidla jako pad vlákno
/// (wait_ready jednou; 483 není chyba, slot se zkouší do 10 s).
fn pripoj() -> Result<(X360, u32), String> {
    kontrola_popredi().map_err(|e| format!("NEPŘIPOJUJI: {e}"))?;
    let t = Instant::now();
    let bus = Bus::connect().map_err(|e| format!("spojení: {e}"))?;
    let spojeno = t.elapsed();
    let mut pad = X360::plug(bus).map_err(|(_, e)| format!("připojení: {e}"))?;
    let wr = pad.wait_ready();
    println!(
        "spojení {spojeno:?}, připojení + wait_ready {:?}: {wr:?}",
        t.elapsed()
    );
    if let Err(e) = wr {
        if e != VigemError::NotReadyYet {
            drop(pad.unplug());
            return Err(format!("wait_ready: {e}"));
        }
    }
    let Some((slot, za)) = cekej(Duration::from_secs(10), || pad.user_index().ok()) else {
        drop(pad.unplug());
        return Err("slot XInput do 10 s nepřidělen".into());
    };
    println!("slot XInput {slot} (hráč {}) po dalších {za:?}", slot + 1);
    let viditelny = cekej(Duration::from_secs(2), || xinput(slot).ok());
    println!(
        "slot {slot} viditelný v XInput: {:?}",
        viditelny.map(|v| v.1)
    );
    Ok((pad, slot))
}

fn e2e() -> Result<(), String> {
    let pred = obsazene_sloty();
    println!("obsazené sloty XInput před: {pred:?}");
    let (mut pad, slot) = pripoj()?;
    let t = Instant::now();
    let pokusy = posli(&mut pad, SONDA).map_err(|e| format!("sonda: {e}"))?;
    let sonda = cekej(Duration::from_millis(500), || {
        (xinput(slot) == Ok(SONDA)).then_some(())
    });
    println!(
        "sonda (pod mrtvou zónou): pokusů {pokusy}, v XInput za {:?}",
        sonda.as_ref().map(|_| t.elapsed())
    );
    posli(&mut pad, NEUTRAL).map_err(|e| format!("neutrál: {e}"))?;
    let neutral = cekej(Duration::from_millis(500), || {
        (xinput(slot) == Ok(NEUTRAL)).then_some(())
    })
    .is_some();
    println!("neutrál v XInput: {neutral}");
    // Keep-alive: nezměněný stav ovladač potvrdí bez doručení.
    let t = Instant::now();
    let znovu = pad.submit(NEUTRAL);
    println!("nezměněný stav znovu: {znovu:?} za {:?}", t.elapsed());
    let t = Instant::now();
    drop(pad.unplug());
    let pryc = cekej(Duration::from_secs(2), || xinput(slot).err());
    println!(
        "po odpojení slot {slot} pryč za {:?}",
        pryc.as_ref().map(|_| t.elapsed())
    );
    let po = obsazene_sloty();
    println!("obsazené sloty XInput po: {po:?}");
    if sonda.is_some() && neutral && znovu.is_ok() && pryc.is_some() && po == pred {
        Ok(())
    } else {
        Err(format!(
            "sonda {} neutrál {neutral} keep-alive {} pryč {} sloty {pred:?}→{po:?}",
            sonda.is_some(),
            znovu.is_ok(),
            pryc.is_some()
        ))
    }
}

/// Dítě pro `kill`: připojí, pošle neutrál, ohlásí slot a čeká na zabití.
fn drz() -> Result<(), String> {
    let (mut pad, slot) = pripoj()?;
    posli(&mut pad, NEUTRAL).map_err(|e| format!("neutrál: {e}"))?;
    println!("PRIPRAVEN {slot}");
    let _ = std::io::stdout().flush();
    std::thread::sleep(DRZ_MAX);
    drop(pad.unplug());
    Ok(())
}

/// Zabitý proces (TerminateProcess — žádný Drop, žádné odpojení) nesmí
/// nechat pad v systému: ovladač ho odpojí se zavřením handle.
fn kill() -> Result<(), String> {
    kontrola_popredi().map_err(|e| format!("NEPŘIPOJUJI: {e}"))?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut dite = Command::new(exe)
        .arg("drz")
        .stdout(Stdio::piped())
        .stdin(Stdio::null())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn()
        .map_err(|e| format!("dítě nejde spustit: {e}"))?;
    let Some(vystup) = dite.stdout.take() else {
        let _ = dite.kill();
        return Err("dítě bez výstupu".into());
    };
    let mut radky = BufReader::new(vystup).lines();
    let mut slot = None;
    for radek in radky.by_ref() {
        let Ok(radek) = radek else { break };
        println!("  dítě: {radek}");
        if let Some(s) = radek.strip_prefix("PRIPRAVEN ") {
            slot = s.trim().parse::<u32>().ok();
            break;
        }
    }
    let Some(slot) = slot else {
        let _ = dite.kill();
        let _ = dite.wait();
        return Err("dítě pad nepřipojilo".into());
    };
    let t = Instant::now();
    dite.kill().map_err(|e| format!("zabití: {e}"))?;
    let _ = dite.wait();
    let pryc = cekej(Duration::from_secs(1), || xinput(slot).err());
    println!(
        "dítě zabito; slot {slot} pryč za {:?}",
        pryc.as_ref().map(|_| t.elapsed())
    );
    pryc.map(|_| ())
        .ok_or_else(|| "slot je i 1 s po zabití procesu obsazený".into())
}

fn main() -> ExitCode {
    let rezim = std::env::args().nth(1).unwrap_or_default();
    let vysledek = match rezim.as_str() {
        "popredi" => match kontrola_popredi() {
            Ok(s) => {
                println!("OK: {s}");
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                println!("NE: {e}");
                return ExitCode::from(3);
            }
        },
        "xinput" => {
            println!("obsazené sloty XInput: {:?}", obsazene_sloty());
            Ok(())
        }
        "e2e" => e2e(),
        "kill" => kill(),
        "drz" => drz(),
        "vse" => e2e().and_then(|()| kill()),
        _ => Err("použití: pad_selftest popredi | xinput | e2e | kill | vse".into()),
    };
    match vysledek {
        Ok(()) => {
            println!("OK");
            ExitCode::SUCCESS
        }
        Err(e) if e.starts_with("NEPŘIPOJUJI") => {
            println!("{e}");
            ExitCode::from(3)
        }
        Err(e) => {
            println!("CHYBA: {e}");
            ExitCode::FAILURE
        }
    }
}

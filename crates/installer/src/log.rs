//! Záznam průběhu instalátoru: `%TEMP%\KeyPadSetup.log`.
//!
//! Okno ukazuje jen krátké věty (Fáze 2b: co nejméně textu a žádné kódy,
//! které uživateli nic neřeknou). Kódy instalátoru ViGEmBus, chyby
//! Windows a síťové detaily ale při pomoci na dálku potřeba jsou — ty
//! jdou sem. Headless režim je navíc vypisuje do konzole.
//!
//! Proč `%TEMP%`: existuje i tehdy, když se instalace KeyPadu nepovedla
//! a instalační složka ještě není, a zápis tam nic neruší. Odinstalace
//! ho maže s ostatními logy. Zápis nikdy nečeká ani nepadá — co se
//! nezapíše, zahodí se; instalace kvůli logu neselže.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

pub const FILE: &str = "KeyPadSetup.log";
/// Přerostlý log se začne psát znovu — k něčemu jsou jen poslední běhy.
const MAX_BYTES: u64 = 256 * 1024;

pub fn path() -> PathBuf {
    std::env::temp_dir().join(FILE)
}

/// Připíše řádek (víceřádkový text se spojí do jednoho).
pub fn line(msg: &str) {
    // Okno a pracovní vlákno píšou každé zvlášť — ať se řádky nepromíchají.
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock();
    let p = path();
    if std::fs::metadata(&p).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::remove_file(&p);
    }
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
    else {
        return;
    };
    let _ = writeln!(
        f,
        "{} [{}] {}",
        now(),
        std::process::id(),
        msg.trim().replace("\r\n", " | ").replace('\n', " | ")
    );
}

/// Místní čas pro záznam (bez knihovny kvůli jednomu razítku).
fn now() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: jen čte hodiny, nic nepředává.
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

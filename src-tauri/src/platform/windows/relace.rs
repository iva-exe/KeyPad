//! Vypnutí, restart a odhlášení Windows (Fáze 2b).
//!
//! Před koncem relace se aktivní virtuální ovladač vždy vypne (neutrál
//! → odpojit) a celá aplikace skončí — hra na druhém konci streamu tak
//! nikdy nedostane „zamrzlou" páčku a Windows nečekají na KeyPad.
//!
//! Proč vlastní vlákno se skrytým oknem NEJVYŠŠÍ úrovně:
//! `WM_QUERYENDSESSION` / `WM_ENDSESSION` Windows rozesílají jen oknům
//! nejvyšší úrovně — okno jen pro zprávy (HWND_MESSAGE) je nedostane.
//! Hlavní okno Tauri je sice dostane taky, jenže jeho vlákno může zrovna
//! stát (dialog WebView2, dlouhé volání) a na konec relace by se
//! nedostalo. Tohle vlákno nedělá nic jiného než čeká na zprávy.
//!
//! Okno se nikdy neukazuje (bez `WS_VISIBLE`) a `WS_EX_TOOLWINDOW` ho
//! drží mimo hlavní panel i Alt+Tab, kdyby ho přece jen někdo ukázal.
//!
//! Fáze 5: totéž okno dostává i zamčení a odpojení relace
//! (`WTSRegisterSessionNotification`) — key-upy kláves držených přes
//! zámek hook neuvidí, engine je proto zapomene a vynutí Klávesnici.

use std::fmt;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use windows::core::{s, w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleW, GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassExW,
    TranslateMessage, ENDSESSION_CLOSEAPP, ENDSESSION_CRITICAL, ENDSESSION_LOGOFF, MSG, WM_CLOSE,
    WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};

/// Třída skrytého okna. Podle ní ho najde i test (skrytá plocha,
/// `PostMessage` bez skutečného vypnutí PC).
pub const TRIDA: &str = "KeyPad.KonecRelace";

/// Vlákno jen čeká na zprávy; obsluha konce relace volá log a frontu
/// pad vlákna — nic náročného na zásobník.
const ZASOBNIK: usize = 256 * 1024;

/// Proč relace končí (`lParam` u `WM_ENDSESSION`) — jen do logu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Duvod(u32);

impl fmt::Display for Duvod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = self.0;
        let mut kus = Vec::new();
        if p & ENDSESSION_LOGOFF != 0 {
            kus.push("odhlášení");
        } else if p & ENDSESSION_CLOSEAPP != 0 {
            // Restart Manager — instalátor nebo aktualizace Windows
            // potřebuje soubor, který držíme.
            kus.push("žádost o zavření aplikací");
        } else {
            kus.push("vypnutí nebo restart");
        }
        if p & ENDSESSION_CRITICAL != 0 {
            kus.push("vynucené");
        }
        write!(f, "{}", kus.join(", "))
    }
}

/// `WM_WTSSESSION_CHANGE` a jeho důvody (WinUser.h / WtsApi32.h).
const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const WTS_CONSOLE_DISCONNECT: usize = 0x2;
const WTS_REMOTE_DISCONNECT: usize = 0x4;
const WTS_SESSION_LOCK: usize = 0x7;
/// `NOTIFY_FOR_THIS_SESSION`.
const JEN_TATO_RELACE: u32 = 0;

/// Co udělat se zprávou (čistá funkce — testuje se bez okna).
#[derive(Debug, PartialEq, Eq)]
enum Akce {
    /// Relace se zamkla nebo odpojila — jen do logu, proč.
    Zamceni(&'static str),
    /// `WM_QUERYENDSESSION`: souhlasit (TRUE). Nic se ještě nedělá —
    /// jiná aplikace může konec relace odmítnout.
    Souhlas,
    /// `WM_ENDSESSION(TRUE)`: relace opravdu končí.
    Konec(Duvod),
    /// Zpráva vyřízená bez akce (zrušený konec relace, WM_CLOSE).
    Nic,
    /// Ostatní zprávy → `DefWindowProcW`.
    Vychozi,
}

fn rozhodni(zprava: u32, wparam: usize, lparam: isize) -> Akce {
    match zprava {
        WM_QUERYENDSESSION => Akce::Souhlas,
        // wParam FALSE = konec relace se zrušil. Při QUERYENDSESSION se
        // nic nevypnulo, takže není co vracet.
        WM_ENDSESSION if wparam != 0 => Akce::Konec(Duvod(lparam as u32)),
        WM_ENDSESSION => Akce::Nic,
        // Okno nesmí zmizet (DefWindowProc by ho na WM_CLOSE zničil)
        // — pak by konec relace neměl kdo obsloužit.
        WM_CLOSE => Akce::Nic,
        WM_WTSSESSION_CHANGE => match wparam {
            WTS_SESSION_LOCK => Akce::Zamceni("zamčení"),
            WTS_CONSOLE_DISCONNECT => Akce::Zamceni("přepnutí uživatele"),
            WTS_REMOTE_DISCONNECT => Akce::Zamceni("odpojení vzdálené plochy"),
            // Odemčení, přihlášení…: nic — Klávesnice už je.
            _ => Akce::Nic,
        },
        _ => Akce::Vychozi,
    }
}

type Obsluha = Box<dyn Fn(Duvod) + Send + Sync>;
type ObsluhaZamceni = Box<dyn Fn(&'static str) + Send + Sync>;

/// Co udělat při zamčení relace (nastavuje `gamepad`).
static PRI_ZAMCENI: OnceLock<ObsluhaZamceni> = OnceLock::new();

/// Zaregistruje obsluhu zamčení a odpojení relace. Volá se z vlákna
/// okna — smí logovat, nesmí dlouho čekat.
pub fn pri_zamceni(f: impl Fn(&'static str) + Send + Sync + 'static) {
    let _ = PRI_ZAMCENI.set(Box::new(f));
}

/// Co udělat při konci relace. Procedura okna nemá jiný kontext.
static OBSLUHA: OnceLock<Obsluha> = OnceLock::new();
/// Konec relace se obsluhuje jednou (WM_ENDSESSION může přijít i od
/// testu a potom od Windows).
static PROBEHLO: AtomicBool = AtomicBool::new(false);

/// Založí vlákno se skrytým oknem. `pri_konci` se zavolá SYNCHRONNĚ
/// z `WM_ENDSESSION(TRUE)` — po návratu z něj mohou Windows proces
/// kdykoli ukončit, takže uklidit (pad, log) musí ještě v něm.
pub fn hlidej(pri_konci: impl Fn(Duvod) + Send + Sync + 'static) -> Result<(), String> {
    if OBSLUHA.set(Box::new(pri_konci)).is_err() {
        return Err("hlídání konce relace už běží".into());
    }
    let (tx, rx) = crossbeam_channel::bounded::<Result<(), String>>(1);
    std::thread::Builder::new()
        .name("keypad-relace".into())
        .stack_size(ZASOBNIK)
        .spawn(move || {
            let okno = vytvor_okno();
            let ok = okno.is_ok();
            let _ = tx.send(okno.map(|_| ()));
            if ok {
                smycka_zprav();
            }
        })
        .map_err(|e| format!("vlákno pro konec relace nejde spustit: {e}"))?;
    rx.recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| Err("okno pro konec relace se nevytvořilo včas".into()))
}

fn vytvor_okno() -> Result<HWND, String> {
    let trida: Vec<u16> = TRIDA.encode_utf16().chain([0]).collect();
    // SAFETY: NULL = modul tohoto procesu.
    let instance =
        unsafe { GetModuleHandleW(PCWSTR::null()) }.map_err(|e| format!("GetModuleHandle: {e}"))?;
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(procedura),
        hInstance: instance.into(),
        lpszClassName: PCWSTR(trida.as_ptr()),
        ..Default::default()
    };
    // SAFETY: struktura má správnou velikost, jméno třídy žije po celou
    // dobu volání (Windows si ho zkopíruje).
    if unsafe { RegisterClassExW(&wc) } == 0 {
        return Err(format!(
            "třída okna pro konec relace nejde zaregistrovat: {}",
            windows::core::Error::from_win32()
        ));
    }
    // SAFETY: registrovaná třída; bez rodiče = okno nejvyšší úrovně,
    // bez WS_VISIBLE = nikdy se neukáže.
    unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR(trida.as_ptr()),
            PCWSTR(trida.as_ptr()),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .map_err(|e| format!("okno pro konec relace nejde vytvořit: {e}"))
    .inspect(|&okno| hlidej_zamceni(okno))
}

/// `WTSRegisterSessionNotification` z wtsapi32.dll ze System32 — za běhu,
/// ne statickým importem (nová statická DLL = nové místo pro podvrženou
/// knihovnu vedle programu, viz `power.rs`). Selhání jen do logu:
/// zůstane hlídání přepnutí plochy v hook vlákně.
fn hlidej_zamceni(okno: HWND) {
    type Registrace = unsafe extern "system" fn(HWND, u32) -> i32;
    // SAFETY: jméno systémové DLL, hledá se jen v System32; knihovna se
    // neuvolňuje (registrace platí do konce procesu).
    let f = unsafe {
        LoadLibraryExW(w!("wtsapi32.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
            .ok()
            .and_then(|dll| GetProcAddress(dll, s!("WTSRegisterSessionNotification")))
    };
    let Some(f) = f else {
        log::warn!("zamčení relace nejde hlídat (wtsapi32.dll) — zůstává hlídání přepnutí plochy");
        return;
    };
    // SAFETY: podpis podle WtsApi32.h: (HWND, DWORD) -> BOOL.
    let registruj =
        unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, Registrace>(f) };
    // SAFETY: platné okno tohoto vlákna.
    if unsafe { registruj(okno, JEN_TATO_RELACE) } == 0 {
        log::warn!(
            "zamčení relace nejde hlídat: {} — zůstává hlídání přepnutí plochy",
            windows::core::Error::from_win32()
        );
    }
}

fn smycka_zprav() {
    let mut msg = MSG::default();
    // SAFETY: standardní smyčka zpráv vlákna, které okno vlastní.
    // GetMessageW vrací −1 při chybě — pak radši skončit než točit.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Procedura okna. Panika přes hranici FFI by shodila proces — proto
/// `catch_unwind`; konec relace se tak aspoň odsouhlasí.
unsafe extern "system" fn procedura(
    hwnd: HWND,
    zprava: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match rozhodni(zprava, wparam.0, lparam.0) {
        Akce::Souhlas => LRESULT(1),
        Akce::Konec(duvod) => {
            if !PROBEHLO.swap(true, Ordering::AcqRel) {
                if let Some(obsluha) = OBSLUHA.get() {
                    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| obsluha(duvod)));
                }
            }
            LRESULT(0)
        }
        Akce::Zamceni(duvod) => {
            if let Some(obsluha) = PRI_ZAMCENI.get() {
                let _ = std::panic::catch_unwind(AssertUnwindSafe(|| obsluha(duvod)));
            }
            LRESULT(0)
        }
        Akce::Nic => LRESULT(0),
        // SAFETY: parametry beze změny od Windows.
        Akce::Vychozi => unsafe { DefWindowProcW(hwnd, zprava, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotaz_na_konec_se_odsouhlasi_bez_akce() {
        assert_eq!(rozhodni(WM_QUERYENDSESSION, 0, 0), Akce::Souhlas);
        assert_eq!(
            rozhodni(WM_QUERYENDSESSION, 0, ENDSESSION_LOGOFF as isize),
            Akce::Souhlas
        );
    }

    #[test]
    fn konec_relace_jen_s_wparam_true() {
        assert_eq!(rozhodni(WM_ENDSESSION, 1, 0), Akce::Konec(Duvod(0)));
        assert_eq!(
            rozhodni(WM_ENDSESSION, 0, 0),
            Akce::Nic,
            "zrušený konec relace nic nedělá"
        );
    }

    /// Zamčení, přepnutí uživatele i odpojení vzdálené plochy vedou na
    /// zapomenutí kláves; odemčení nic nedělá.
    #[test]
    fn zamceni_relace() {
        assert_eq!(
            rozhodni(WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK, 1),
            Akce::Zamceni("zamčení")
        );
        assert!(matches!(
            rozhodni(WM_WTSSESSION_CHANGE, WTS_CONSOLE_DISCONNECT, 1),
            Akce::Zamceni(_)
        ));
        assert!(matches!(
            rozhodni(WM_WTSSESSION_CHANGE, WTS_REMOTE_DISCONNECT, 1),
            Akce::Zamceni(_)
        ));
        // WTS_SESSION_UNLOCK = 8, WTS_CONSOLE_CONNECT = 1
        assert_eq!(rozhodni(WM_WTSSESSION_CHANGE, 8, 1), Akce::Nic);
        assert_eq!(rozhodni(WM_WTSSESSION_CHANGE, 1, 1), Akce::Nic);
    }

    /// wtsapi32.dll jde načíst ze System32 a funkce v ní je.
    #[test]
    fn registrace_zamceni_se_najde_v_system32() {
        // SAFETY: jen načtení systémové DLL a dotaz na export.
        let f = unsafe {
            LoadLibraryExW(w!("wtsapi32.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
                .ok()
                .and_then(|dll| GetProcAddress(dll, s!("WTSRegisterSessionNotification")))
        };
        assert!(f.is_some());
    }

    #[test]
    fn okno_nejde_zavrit() {
        assert_eq!(rozhodni(WM_CLOSE, 0, 0), Akce::Nic);
        assert_eq!(rozhodni(0x0001 /* WM_CREATE */, 0, 0), Akce::Vychozi);
    }

    #[test]
    fn duvod_do_logu() {
        assert_eq!(Duvod(0).to_string(), "vypnutí nebo restart");
        // lParam je bitová maska v horních bitech — přes isize a zpět.
        let odhlaseni = rozhodni(WM_ENDSESSION, 1, ENDSESSION_LOGOFF as i32 as isize);
        assert_eq!(odhlaseni, Akce::Konec(Duvod(ENDSESSION_LOGOFF)));
        assert_eq!(Duvod(ENDSESSION_LOGOFF).to_string(), "odhlášení");
        assert_eq!(
            Duvod(ENDSESSION_CLOSEAPP | ENDSESSION_CRITICAL).to_string(),
            "žádost o zavření aplikací, vynucené"
        );
    }
}

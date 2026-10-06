//! Raw Input klávesnice v procesu KeyPadu (OQ 60).
//!
//! Proces, který má klávesnici zaregistrovanou pro Raw Input, podle
//! cizích nálezů nedostává s vlastním oknem v popředí volání vlastního
//! `WH_KEYBOARD_LL` hooku — Windows ho pro něj potichu přeskočí.
//! Nedokumentované chování (wry#1761 a tauri#13919 na týchž verzích,
//! legato PR#23). Nejspíš proto vydání 0.1.0+20261006.1504 nepřiřadilo
//! nic a živé svícení ani Scroll Lock s oknem KeyPadu v popředí nešly:
//! tao si při vytvoření smyčky událostí zaregistruje myš i klávesnici
//! (`DeviceEventFilter::Unfocused`, cíl je jeho skryté okno). Přímo
//! neověřeno (testy nesmí mačkat skutečné klávesy) — potvrdí vlastník
//! řádkem „callback N×" s N > 0 (ROADMAP Fáze 6c, bod 1).
//!
//! Pravidlo „žádný Raw Input klávesnice" platí preventivně tak jako tak:
//! `main.rs` proto Tauri nastaví `device_event_filter(Always)` (tao
//! registraci zruší) a tenhle modul to za běhu OVĚŘÍ, ne předpokládá
//! (princip 8): při startu a na začátku každého přiřazování. Zjištěnou
//! registraci klávesnice zruší a ověří znovu. Myš nechá — hooku klávesnice
//! nevadí a KeyPad ji jinak nepotřebuje řešit.
//!
//! Registrace patří celému procesu, ne vláknu: ověřit i zrušit ji jde
//! z kteréhokoli vlákna (hook vlákno). Nikdy z callbacku hooku — alokuje
//! a volá do jádra Windows (princip 3). `RegisterRawInputDevices`
//! i `GetRegisteredRawInputDevices` má user32 od Windows XP (princip 9).

use std::mem::size_of;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::{
    GetRegisteredRawInputDevices, RegisterRawInputDevices, RAWINPUTDEVICE, RIDEV_EXCLUDE,
    RIDEV_PAGEONLY, RIDEV_REMOVE,
};

/// Usage page „Generic Desktop" (HID) — myš, klávesnice, gamepady.
/// Vlastní konstanty: `Win32_Devices_HumanInterfaceDevice` by se kvůli
/// třem číslům nevyplatilo.
const OBECNA: u16 = 0x01;
/// Klávesnice (0x06) a samostatná numerická klávesnice (0x07).
const KLAVESNICE: [u16; 2] = [0x06, 0x07];
const VELIKOST: u32 = size_of::<RAWINPUTDEVICE>() as u32;
/// Pole režimu v `dwFlags` (`RIDEV_EXMODEMASK` z winuser.h; windows-rs ho
/// nemá): 0 = jen tahle usage, `RIDEV_EXCLUDE` 0x10, `RIDEV_PAGEONLY`
/// 0x20, `RIDEV_NOLEGACY` 0x30. Jsou to HODNOTY, ne samostatné bity —
/// NOLEGACY = EXCLUDE | PAGEONLY, takže `contains(RIDEV_EXCLUDE)` by
/// nejtvrdší registraci klávesnice vzal za výjimku a kontrola by ji
/// přehlédla (nález revize Fáze 6c).
const REZIM_MASKA: u32 = 0xF0;

/// Režim registrace (jedna z hodnot pole [`REZIM_MASKA`]).
fn rezim(d: &RAWINPUTDEVICE) -> u32 {
    d.dwFlags.0 & REZIM_MASKA
}

/// Výsledek kontroly ([`kontrola`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawInput {
    /// Klávesnice v Raw Input procesu není.
    Ne,
    /// Byla tam a zmizela (ověřeno znovu).
    Odregistrovano,
    /// Je tam a zrušit nejde — s oknem KeyPadu v popředí hook nejspíš nic
    /// nedostane (přiřazování, živé svícení; OQ 60).
    NejdeZrusit,
    /// Windows registrace neprozradily.
    Nezjisteno,
}

impl RawInput {
    /// Do logu („raw input klávesnice: …").
    pub fn text(self) -> &'static str {
        match self {
            RawInput::Ne => "ne",
            RawInput::Odregistrovano => "ano — odregistrováno",
            RawInput::NejdeZrusit => "ano — nejde zrušit",
            RawInput::Nezjisteno => "nezjištěno",
        }
    }
}

/// Registrace Raw Input celého procesu; `None` = Windows je neprozradily.
fn registrace() -> Option<Vec<RAWINPUTDEVICE>> {
    // Osm položek stačí skoro vždy (tao má dvě). Víc jen na druhý pokus;
    // mezi pokusy může jiné vlákno registrovat další, proto ne jen dva.
    let mut pole = vec![RAWINPUTDEVICE::default(); 8];
    for _ in 0..4 {
        let mut n = u32::try_from(pole.len()).ok()?;
        // SAFETY: pole má `n` položek a Windows zapíšou nejvýš tolik.
        let r = unsafe { GetRegisteredRawInputDevices(Some(pole.as_mut_ptr()), &mut n, VELIKOST) };
        if r != u32::MAX {
            pole.truncate(usize::try_from(r).ok()?);
            return Some(pole);
        }
        // Malé pole: Windows do `n` zapsaly, kolik potřebují. Jiná chyba
        // `n` nezvětší.
        let potreba = usize::try_from(n).ok()?;
        if potreba <= pole.len() {
            return None;
        }
        pole = vec![RAWINPUTDEVICE::default(); potreba];
    }
    None
}

/// Výjimka (`RIDEV_EXCLUDE`) ze stránky Generic Desktop — sama nic
/// nezaregistruje, jen vyjme usage z celé stránky.
fn vyjimka(d: &RAWINPUTDEVICE) -> bool {
    d.usUsagePage == OBECNA && rezim(d) == RIDEV_EXCLUDE.0
}

/// Celá stránka Generic Desktop (`RIDEV_PAGEONLY`) — klávesnice v ní je,
/// pokud ji výjimka nevylučuje.
fn cela_stranka(d: &RAWINPUTDEVICE) -> bool {
    d.usUsagePage == OBECNA && rezim(d) == RIDEV_PAGEONLY.0
}

/// Klávesnice zaregistrovaná přímo (usage 0x06 nebo 0x07): režim 0,
/// `RIDEV_NOLEGACY` — a kdyby Windows někdy vrátily jinou hodnotu pole,
/// taky. Raději registraci zbytečně zrušit, než ji přehlédnout.
fn primo(d: &RAWINPUTDEVICE) -> bool {
    d.usUsagePage == OBECNA && !vyjimka(d) && !cela_stranka(d) && KLAVESNICE.contains(&d.usUsage)
}

fn klavesnice_v(reg: &[RAWINPUTDEVICE]) -> bool {
    let vyjmuta = |u: u16| reg.iter().any(|d| vyjimka(d) && d.usUsage == u);
    reg.iter().any(primo)
        || (reg.iter().any(cela_stranka) && !KLAVESNICE.iter().all(|&u| vyjmuta(u)))
}

/// Má proces klávesnici zaregistrovanou pro Raw Input? `None` = Windows
/// registrace neprozradily.
pub fn klavesnice_v_raw_input() -> Option<bool> {
    registrace().map(|r| klavesnice_v(&r))
}

/// Registrace, kterými proces dostává klávesnici, převedené na jejich
/// zrušení: přímé (0x06, 0x07) a celá stránka Generic Desktop. Celá
/// stránka jde jen celá, i s myší — klávesnice má přednost. Přímou
/// registraci myši nechá.
fn zruseni(reg: &[RAWINPUTDEVICE]) -> Vec<RAWINPUTDEVICE> {
    reg.iter()
        .filter(|d| primo(d) || cela_stranka(d))
        .map(|d| RAWINPUTDEVICE {
            usUsagePage: d.usUsagePage,
            usUsage: d.usUsage,
            // PAGEONLY jen k celé stránce. Přímou registraci (i NOLEGACY)
            // ruší samotné RIDEV_REMOVE; REMOVE | PAGEONLY na ni Windows
            // odmítnou (chyba 87) a registrace zůstane (sonda revize).
            dwFlags: if cela_stranka(d) {
                RIDEV_REMOVE | RIDEV_PAGEONLY
            } else {
                RIDEV_REMOVE
            },
            // RIDEV_REMOVE vyžaduje prázdný cíl (dokumentace).
            hwndTarget: HWND::default(),
        })
        .collect()
}

fn zrus(reg: &[RAWINPUTDEVICE]) -> windows::core::Result<()> {
    // SAFETY: pole platných struktur; jen odregistrace vlastního procesu.
    unsafe { RegisterRawInputDevices(&zruseni(reg), VELIKOST) }
}

/// Ověří, že proces nemá klávesnici v Raw Input; když má, registraci
/// zruší a ověří znovu. Jen mimo callback hooku (alokuje, loguje chybu).
pub fn kontrola() -> RawInput {
    let Some(reg) = registrace() else {
        return RawInput::Nezjisteno;
    };
    if !klavesnice_v(&reg) {
        return RawInput::Ne;
    }
    if let Err(e) = zrus(&reg) {
        log::error!("Raw Input klávesnice nejde zrušit: {e}");
    }
    match klavesnice_v_raw_input() {
        Some(false) => RawInput::Odregistrovano,
        Some(true) => RawInput::NejdeZrusit,
        None => RawInput::Nezjisteno,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::windows::hook::testy_plocha;
    use windows::core::w;
    use windows::Win32::UI::Input::{
        RIDEV_APPKEYS, RIDEV_DEVNOTIFY, RIDEV_INPUTSINK, RIDEV_NOHOTKEYS, RIDEV_NOLEGACY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
    };

    fn zarizeni(usage: u16, flags: u32, cil: HWND) -> RAWINPUTDEVICE {
        RAWINPUTDEVICE {
            usUsagePage: OBECNA,
            usUsage: usage,
            dwFlags: windows::Win32::UI::Input::RAWINPUTDEVICE_FLAGS(flags),
            hwndTarget: cil,
        }
    }

    fn registruj(d: &[RAWINPUTDEVICE]) {
        // SAFETY: pole platných struktur, cíl je okno tohoto vlákna.
        unsafe { RegisterRawInputDevices(d, VELIKOST) }.expect("RegisterRawInputDevices");
    }

    fn mys_zustala() -> bool {
        registrace()
            .unwrap()
            .iter()
            .any(|d| d.usUsagePage == OBECNA && d.usUsage == 0x02)
    }

    /// Celý průběh v JEDNOM testu: registrace patří procesu a testy běží
    /// souběžně — dva testy by si ji navzájem měnily. (Hook v testech proto
    /// skutečnou kontrolu nevolá, má podvrh.) Žádný vstup se nesimuluje:
    /// cílem registrace je okno jen pro zprávy na vlastní skryté ploše —
    /// nikdy není v popředí, takže Raw Input nedostane ani jednu klávesu.
    #[test]
    fn klavesnice_se_najde_odregistruje_a_mys_zustane() {
        std::thread::spawn(|| {
            testy_plocha::na_skryte_plose().unwrap();
            // Proces testu nic nezaregistroval.
            assert_eq!(klavesnice_v_raw_input(), Some(false));
            assert_eq!(kontrola(), RawInput::Ne);

            // SAFETY: předdefinovaná třída, okno jen pro zprávy (HWND_MESSAGE)
            // — nikdy se neukáže; zruší se na konci testu.
            let okno = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("STATIC"),
                    w!(""),
                    WINDOW_STYLE(0),
                    0,
                    0,
                    0,
                    0,
                    Some(HWND_MESSAGE),
                    None,
                    None,
                    None,
                )
            }
            .expect("okno jen pro zprávy");

            // Jako tao (DeviceEventFilter::Unfocused): myš i klávesnice.
            registruj(&[
                zarizeni(0x02, RIDEV_DEVNOTIFY.0, okno),
                zarizeni(0x06, RIDEV_DEVNOTIFY.0, okno),
            ]);
            assert_eq!(klavesnice_v_raw_input(), Some(true));
            // Z jiného vlákna, jako hook vlákno aplikace (registrace patří
            // procesu, ne vláknu).
            let r = std::thread::spawn(kontrola).join().unwrap();
            assert_eq!(r, RawInput::Odregistrovano);
            assert_eq!(klavesnice_v_raw_input(), Some(false));
            assert!(mys_zustala(), "myš zůstala zaregistrovaná");
            assert_eq!(kontrola(), RawInput::Ne, "podruhé už není co rušit");

            // Samostatná numerická klávesnice taky.
            registruj(&[zarizeni(0x07, 0, okno)]);
            assert_eq!(klavesnice_v_raw_input(), Some(true));
            assert_eq!(kontrola(), RawInput::Odregistrovano);
            assert!(mys_zustala());

            // NOLEGACY — nejtvrdší registrace (režim 0x30 = EXCLUDE |
            // PAGEONLY). Dřív ji kontrola brala za výjimku a hlásila „ne"
            // (nález revize). Staré zprávy klávesnice potlačí jen oknům
            // tohoto procesu a test žádné okno s klávesnicí nemá; zruší se
            // hned.
            registruj(&[zarizeni(0x06, RIDEV_NOLEGACY.0, okno)]);
            assert_eq!(
                registrace().map(|r| r.iter().any(|d| rezim(d) == RIDEV_NOLEGACY.0)),
                Some(true),
                "Windows registraci vrátily s režimem NOLEGACY"
            );
            assert_eq!(klavesnice_v_raw_input(), Some(true));
            assert_eq!(kontrola(), RawInput::Odregistrovano);
            assert_eq!(klavesnice_v_raw_input(), Some(false));
            assert!(mys_zustala());

            // Úklid myši; pak celá stránka Generic Desktop (klávesnice v ní).
            registruj(&[zarizeni(0x02, RIDEV_REMOVE.0, HWND::default())]);
            registruj(&[zarizeni(0, RIDEV_PAGEONLY.0, okno)]);
            assert_eq!(klavesnice_v_raw_input(), Some(true));
            assert_eq!(kontrola(), RawInput::Odregistrovano);

            assert_eq!(registrace().map(|r| r.len()), Some(0), "nic nezůstalo");
            // SAFETY: okno tohoto vlákna, ruší se právě jednou.
            unsafe { DestroyWindow(okno) }.unwrap();
        })
        .join()
        .unwrap();
    }

    #[test]
    fn klavesnice_v_registracich() {
        let h = HWND::default();
        let e = RIDEV_EXCLUDE.0;
        let p = RIDEV_PAGEONLY.0;
        let jen_mys = [zarizeni(0x02, 0, h)];
        assert!(!klavesnice_v(&jen_mys));
        assert!(klavesnice_v(&[zarizeni(0x02, 0, h), zarizeni(0x06, 0, h)]));
        assert!(klavesnice_v(&[zarizeni(0x07, 0, h)]));
        // Celá stránka, i s výjimkou jen jedné z kláves.
        assert!(klavesnice_v(&[zarizeni(0, p, h)]));
        assert!(klavesnice_v(&[zarizeni(0, p, h), zarizeni(0x06, e, h)]));
        assert!(!klavesnice_v(&[
            zarizeni(0, p, h),
            zarizeni(0x06, e, h),
            zarizeni(0x07, e, h)
        ]));
        // Výjimka sama nic nezaregistruje; jiná stránka (spotřební
        // klávesy 0x0C) klávesnice není.
        assert!(!klavesnice_v(&[zarizeni(0x06, e, h)]));
        let spotrebni = RAWINPUTDEVICE {
            usUsagePage: 0x0C,
            usUsage: 0x01,
            ..zarizeni(0, 0, h)
        };
        assert!(!klavesnice_v(&[spotrebni]));
        assert!(!klavesnice_v(&[]));
    }

    /// Režim registrace je pole (0x10 výjimka, 0x20 celá stránka, 0x30
    /// NOLEGACY), ne bity. Dřív `contains(RIDEV_EXCLUDE)` vzal NOLEGACY za
    /// výjimku a kontrola nejtvrdší registraci klávesnice přehlédla (nález
    /// revize Fáze 6c).
    #[test]
    fn nolegacy_je_registrace_ne_vyjimka() {
        let h = HWND::default();
        let e = RIDEV_EXCLUDE.0;
        let p = RIDEV_PAGEONLY.0;
        let n = RIDEV_NOLEGACY.0;
        assert!(klavesnice_v(&[zarizeni(0x06, n, h)]));
        assert!(klavesnice_v(&[zarizeni(
            0x06,
            n | RIDEV_NOHOTKEYS.0 | RIDEV_APPKEYS.0,
            h
        )]));
        assert!(klavesnice_v(&[zarizeni(0x07, n | RIDEV_INPUTSINK.0, h)]));
        // NOLEGACY klávesnice není výjimka z celé stránky.
        assert!(klavesnice_v(&[
            zarizeni(0, p, h),
            zarizeni(0x06, n, h),
            zarizeni(0x07, e, h)
        ]));
        // Výjimka a celá stránka zůstanou poznané i s dalšími příznaky.
        assert!(!klavesnice_v(&[zarizeni(0x06, e | RIDEV_DEVNOTIFY.0, h)]));
        assert!(klavesnice_v(&[zarizeni(0, p | RIDEV_INPUTSINK.0, h)]));
        // Myš s NOLEGACY klávesnice není.
        assert!(!klavesnice_v(&[zarizeni(0x02, n, h)]));
    }

    /// Zrušení podle režimu: přímou registraci (i NOLEGACY) ruší samotné
    /// RIDEV_REMOVE — REMOVE | PAGEONLY na ni Windows odmítnou —, celou
    /// stránku REMOVE | PAGEONLY. Výjimka a myš se neruší; cíl je vždy
    /// prázdný.
    #[test]
    fn zruseni_podle_rezimu() {
        let h = HWND(0x1234 as *mut core::ffi::c_void);
        let reg = [
            zarizeni(0x02, RIDEV_DEVNOTIFY.0, h),
            zarizeni(0x06, RIDEV_NOLEGACY.0 | RIDEV_NOHOTKEYS.0, h),
            zarizeni(0x07, RIDEV_EXCLUDE.0, h),
            zarizeni(0, RIDEV_PAGEONLY.0 | RIDEV_DEVNOTIFY.0, h),
        ];
        let z: Vec<_> = zruseni(&reg)
            .iter()
            .map(|d| {
                (
                    d.usUsagePage,
                    d.usUsage,
                    d.dwFlags.0,
                    d.hwndTarget == HWND::default(),
                )
            })
            .collect();
        assert_eq!(
            z,
            [
                (OBECNA, 0x06, RIDEV_REMOVE.0, true),
                (OBECNA, 0, RIDEV_REMOVE.0 | RIDEV_PAGEONLY.0, true),
            ]
        );
    }

    #[test]
    fn texty_do_logu() {
        assert_eq!(RawInput::Ne.text(), "ne");
        assert_eq!(RawInput::Odregistrovano.text(), "ano — odregistrováno");
        assert_eq!(RawInput::NejdeZrusit.text(), "ano — nejde zrušit");
        assert_eq!(RawInput::Nezjisteno.text(), "nezjištěno");
    }
}

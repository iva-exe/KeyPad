//! Minimální klient ViGEmBus — jen virtuální Xbox 360 ovladač.
//!
//! Proč vlastní a ne crate `vigem-client` (rozbor v revizi Fáze 2):
//!
//! 1. Tam každé volání čeká na ovladač bez limitu
//!    (`GetOverlappedResult(bWait=TRUE)`) — pad vlákno by se mohlo
//!    zaseknout navždy. Tady má každé IOCTL časový limit.
//! 2. Chyby potřebujeme od sebe odlišit: 170/259 = ovladač report ZAHODIL
//!    (poslat znovu, jinak páčka zůstane vychýlená), 55 = target zmizel,
//!    483 = první instalace zařízení nestihla 1 s, 650 = bez slotu XInput.
//!    Crate je slévá, oficiální klient v C chybu 170 dokonce spolkne.
//! 3. Jeho `plugin()` zkouší sériová čísla 1..65534 při JAKÉKOLI chybě
//!    a skutečnou příčinu nahlásí jako „žádné volné místo".
//! 4. Závisí na `winapi` 0.3; tohle stojí na `windows`, které už máme.
//!
//! Protokol je zmrazený: ViGEmBus je archivovaný (poslední ovladač
//! 1.21.442), VIGEM_COMMON_VERSION = 1. Struktury a kódy IOCTL jsou
//! z jeho `BusShared.h` a velikosti hlídají `const` aserce níže.

use std::cell::Cell;
use std::ffi::c_void;
use std::fmt;
use std::mem::size_of;

use keypad_core::PadState;
// GUID rozhraní drží `updater::vigembus` — tentýž pro instalátor
// (zjištění stavu) i pro tenhle klient, ať se nemůžou rozejít.
use updater::vigembus::INTERFACE_GUID;
use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_List_SizeW,
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CR_BUFFER_SMALL, CR_SUCCESS,
};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_IO_INCOMPLETE,
    ERROR_IO_PENDING, ERROR_NOT_SUPPORTED, GENERIC_READ, GENERIC_WRITE, HANDLE, WAIT_TIMEOUT,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows::Win32::System::Threading::CreateEventW;
use windows::Win32::System::IO::{CancelIoEx, DeviceIoControl, GetOverlappedResultEx, OVERLAPPED};

// CTL_CODE(FILE_DEVICE_BUS_EXTENDER = 0x2A, 0x801 + i, METHOD_BUFFERED, …)
const IOCTL_PLUGIN_TARGET: u32 = 0x2A_A004;
const IOCTL_UNPLUG_TARGET: u32 = 0x2A_A008;
const IOCTL_CHECK_VERSION: u32 = 0x2A_A00C;
const IOCTL_WAIT_DEVICE_READY: u32 = 0x2A_A010;
const IOCTL_XUSB_SUBMIT_REPORT: u32 = 0x2A_A808;
const IOCTL_XUSB_GET_USER_INDEX: u32 = 0x2A_E81C;

const VIGEM_COMMON_VERSION: u32 = 1;
const TARGET_XBOX360_WIRED: u32 = 0;
/// Xbox 360 Controller for Windows (drátový) — to, co hry i Steam znají.
const VID_MICROSOFT: u16 = 0x045E;
const PID_XBOX360: u16 = 0x028E;

/// Nejvyšší zkoušené sériové číslo. Sériové číslo je součástí ID
/// zařízení (`USB\VID_045E&PID_028E\01`), takže stejné číslo = stejné
/// zařízení v PnP a žádná nová instalace ovladače. Víc padů od jiných
/// programů (DS4Windows…) prakticky nebývá — tohle je jen pojistka.
const MAX_SERIAL: u32 = 16;

/// Časové limity jednotlivých IOCTL. Normálně trvají mikrosekundy;
/// limit je jen proti ovladači, který by přestal odpovídat.
const LIMIT_MS: u32 = 1_000;
/// WAIT_DEVICE_READY drží ovladač nejvýš 1 s (vlastní pracovní vlákno,
/// EmulationTargetPDO.cpp) — limit má rezervu, ať ho nikdy neutneme my.
const LIMIT_PRIPRAVENOST_MS: u32 = 3_000;
/// Odeslání stavu trvá 10–40 µs (naměřeno); 200 ms = ovladač visí.
const LIMIT_REPORT_MS: u32 = 200;
/// Jak dlouho po CancelIoEx čekat, než požadavek ovladač opravdu vrátí.
const LIMIT_ZRUSENI_MS: u32 = 500;

#[repr(C)]
#[derive(Clone, Copy)]
struct CheckVersion {
    size: u32,
    version: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PluginTarget {
    size: u32,
    serial: u32,
    target_type: u32,
    vendor: u16,
    product: u16,
}

/// UNPLUG i WAIT_DEVICE_READY mají stejné tělo.
#[repr(C)]
#[derive(Clone, Copy)]
struct SerialOnly {
    size: u32,
    serial: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UserIndex {
    size: u32,
    serial: u32,
    index: u32,
}

/// `XUSB_REPORT` — bajt po bajtu `XINPUT_GAMEPAD` (12 B). Kladné Y =
/// nahoru, ovladač nic nepřevrací (naměřeno: ly −2222 se z XInput
/// vrátilo jako −2222).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XusbReport {
    pub buttons: u16,
    pub lt: u8,
    pub rt: u8,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SubmitReport {
    size: u32,
    serial: u32,
    report: XusbReport,
}

// Ovladač kontroluje pole `size` a jiná velikost = odmítnutí nebo, hůř,
// čtení vedle. Rozložení je zmrazené, tak ať ho hlídá překladač.
const _: () = assert!(size_of::<CheckVersion>() == 8);
const _: () = assert!(size_of::<PluginTarget>() == 16);
const _: () = assert!(size_of::<SerialOnly>() == 8);
const _: () = assert!(size_of::<UserIndex>() == 12);
const _: () = assert!(size_of::<XusbReport>() == 12);
const _: () = assert!(size_of::<SubmitReport>() == 20);

impl XusbReport {
    pub const NEUTRAL: XusbReport = XusbReport {
        buttons: 0,
        lt: 0,
        rt: 0,
        lx: 0,
        ly: 0,
        rx: 0,
        ry: 0,
    };
}

/// Pole po poli, nikdy `transmute`: `PadState` nemá `repr(C)` a pořadí
/// jeho polí si překladač smí zvolit sám.
impl From<&PadState> for XusbReport {
    fn from(s: &PadState) -> Self {
        XusbReport {
            buttons: s.buttons,
            lt: s.left_trigger,
            rt: s.right_trigger,
            lx: s.thumb_lx,
            ly: s.thumb_ly,
            rx: s.thumb_rx,
            ry: s.thumb_ry,
        }
    }
}

/// Chyby ViGEmBus rozdělené podle toho, co s nimi pad vlákno dělá.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VigemError {
    /// Rozhraní ViGEmBus v systému není (ovladač chybí nebo je zakázaný).
    BusMissing,
    /// Rozhraní je, ale nejde otevřít (kód Win32).
    BusAccess(u32),
    /// Ovladač na kontrolu verze skutečně ODPOVĚDĚL, že mluví jiným
    /// protokolem (viz [`chyba_verze`]). Ne vypršení limitu ani jiná
    /// porucha — ty jsou chybou padu, přeinstalace by je nespravila.
    VersionMismatch,
    /// Všechna zkoušená sériová čísla jsou obsazená.
    NoFreeSerial,
    /// 55 ERROR_DEV_NOT_EXIST — target (ještě / už) neexistuje.
    Gone,
    /// 170 ERROR_BUSY nebo 259 ERROR_NO_MORE_ITEMS — xusb22 zrovna
    /// nečeká na data a ovladač report ZAHODIL (neuloží si ho). Musí se
    /// poslat znovu.
    Busy,
    /// 483 — ovladač nedostal do 1 s „LED" od xusb22; typicky první
    /// připojení na novém PC, kdy Windows teprve instalují zařízení.
    NotReadyYet,
    /// 650 — XInput slot ještě nepřidělen, nebo už jsou v systému
    /// 4 ovladače (víc XInput neumí).
    NoUserIndex,
    /// Ovladač neodpověděl v časovém limitu.
    TimedOut,
    /// Cokoli jiného (kód Win32).
    Other(u32),
}

impl fmt::Display for VigemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VigemError::BusMissing => f.write_str(
                "ovladač ViGEmBus v systému není (nebo je zakázaný ve Správci zařízení)",
            ),
            VigemError::BusAccess(c) => write!(f, "ViGEmBus nejde otevřít (chyba {c})"),
            VigemError::VersionMismatch => f.write_str(
                "ovladač ViGEmBus má nepodporovanou verzi — odeber „ViGEm Bus Driver“ \
                 v Nastavení → Aplikace a klikni na Zkusit znovu, KeyPad pak nabídne \
                 instalaci té správné",
            ),
            VigemError::NoFreeSerial => {
                f.write_str("ViGEmBus už nemá volné místo pro další virtuální ovladač")
            }
            VigemError::Gone => f.write_str(
                "virtuální ovladač zmizel (odpojený ve Správci zařízení nebo restart ViGEmBus)",
            ),
            VigemError::Busy => f.write_str("virtuální ovladač nepřijímá data"),
            VigemError::NotReadyYet => f.write_str("virtuální ovladač se ještě nepřipravil"),
            VigemError::NoUserIndex => f.write_str("Windows ovladači nepřidělily číslo hráče"),
            VigemError::TimedOut => f.write_str("ViGEmBus neodpověděl včas"),
            VigemError::Other(c) => write!(f, "chyba ViGEmBus {c}"),
        }
    }
}

/// Kód Win32 z chyby windows-rs (HRESULT_FROM_WIN32 → spodních 16 bitů).
fn win32(e: &windows::core::Error) -> u32 {
    (e.code().0 as u32) & 0xFFFF
}

fn chyba_z_kodu(c: u32) -> VigemError {
    match c {
        55 => VigemError::Gone,
        // 259 = STATUS_NO_MORE_ENTRIES z WdfIoQueueRetrieveNextRequest
        // (XusbPdo.cpp, SubmitReportImpl): xusb22 nemá ve frontě žádný
        // požadavek na data, report se zahodil — totéž co 170. Naměřeno
        // 29. 9. 2026 hned po připojení, když v systému byl druhý
        // virtuální pad (sériové číslo 2); dřív končilo chybou padu.
        170 | 259 => VigemError::Busy,
        483 => VigemError::NotReadyYet,
        650 => VigemError::NoUserIndex,
        _ if c == WAIT_TIMEOUT.0 => VigemError::TimedOut,
        _ => VigemError::Other(c),
    }
}

/// Selhání CHECK_VERSION. Za jinou verzi se bere jen skutečná odpověď
/// ovladače: 50 = verze nesedí (STATUS_NOT_SUPPORTED, Queue.cpp),
/// 87 = jiná velikost struktury (starší ovladače), 1 = takové IOCTL
/// ovladač nezná (prastarý ViGEmBus bez kontroly verze). Vypršení limitu
/// nebo cokoli jiného je porucha padu — hláška „nepodporovaná verze" by
/// uživatele poslala přeinstalovat ovladač, který je v pořádku.
fn chyba_verze(c: u32) -> VigemError {
    if c == ERROR_NOT_SUPPORTED.0 || c == ERROR_INVALID_PARAMETER.0 || c == ERROR_INVALID_FUNCTION.0
    {
        VigemError::VersionMismatch
    } else {
        chyba_z_kodu(c)
    }
}

/// Požadavek pro ovladač. OVERLAPPED a data leží na haldě spolu: když
/// ovladač požadavek nedokončí ani po zrušení, musí obojí žít dál, až
/// do chvíle, kdy do nich jádro zapíše (jinak by psalo do cizí paměti).
#[repr(C)]
struct Pozadavek<T> {
    ov: OVERLAPPED,
    data: T,
}

/// Spojení s ViGEmBus.
///
/// Handle NENÍ dědičný (žádné SECURITY_ATTRIBUTES), takže ho nedostane
/// ani instalátor spuštěný z aplikace. Zavření handle — i pádem nebo
/// zabitím procesu — odpojí všechny targety tohoto handle (ovladač,
/// `Bus_FileClose`; naměřeno do 6 ms). Virtuální pad proto nikdy
/// nepřežije KeyPad.
pub struct Bus {
    handle: HANDLE,
    /// Ruční reset — doporučení MSDN pro OVERLAPPED. Resetuje ho I/O
    /// manažer na začátku každého požadavku.
    event: HANDLE,
    /// Požadavek, který ovladač nevrátil ani po zrušení, pořád drží
    /// `event`. Další požadavky by se s ním pletly, takže spojení se už
    /// nepoužije — volající ho zahodí (zavření handle target odpojí).
    otraveno: Cell<bool>,
}

// SAFETY: handly jádra platí v celém procesu a jde s nimi pracovat
// z libovolného vlákna. `Bus` vlastní vždy jedno vlákno (pad vlákno);
// `Cell` ho drží mimo `Sync`, takže sdílet ho mezi vlákny nejde.
unsafe impl Send for Bus {}

impl Bus {
    /// Najde ViGEmBus a ověří verzi protokolu. Trvá kolem 0,3 ms.
    pub fn connect() -> Result<Bus, VigemError> {
        let mut posledni = VigemError::BusMissing;
        for cesta in cesty_rozhrani()? {
            // SAFETY: `cesta` je řetězec zakončený nulou a žije po celou
            // dobu volání.
            let handle = match unsafe {
                CreateFileW(
                    PCWSTR(cesta.as_ptr()),
                    (GENERIC_READ | GENERIC_WRITE).0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                    None,
                )
            } {
                Ok(h) => h,
                Err(e) => {
                    posledni = VigemError::BusAccess(win32(&e));
                    continue;
                }
            };
            // SAFETY: bez jména a bez atributů zabezpečení.
            let event = match unsafe { CreateEventW(None, true, false, PCWSTR::null()) } {
                Ok(ev) => ev,
                Err(e) => {
                    // SAFETY: handle jsme právě otevřeli a nikde jinde není.
                    unsafe {
                        let _ = CloseHandle(handle);
                    }
                    return Err(VigemError::Other(win32(&e)));
                }
            };
            let bus = Bus {
                handle,
                event,
                otraveno: Cell::new(false),
            };
            let mut cv = CheckVersion {
                size: size_of::<CheckVersion>() as u32,
                version: VIGEM_COMMON_VERSION,
            };
            match bus.ioctl(IOCTL_CHECK_VERSION, &mut cv, false, LIMIT_MS) {
                Ok(()) => return Ok(bus),
                // `bus` se zavře v Drop a zkusí se další rozhraní.
                Err(c) => posledni = chyba_verze(c),
            }
        }
        Err(posledni)
    }

    /// Jedno IOCTL s časovým limitem.
    ///
    /// ViGEmBus (KMDF) vrací vždy ERROR_IO_PENDING a dokončí událostí
    /// i chybu (naměřeno), ale na to se nespoléhá — okamžitá chyba se
    /// vrátí rovnou, okamžitý úspěch projde stejnou cestou jako čekání.
    fn ioctl<T: Copy>(
        &self,
        kod: u32,
        data: &mut T,
        vystup: bool,
        limit_ms: u32,
    ) -> Result<(), u32> {
        if self.otraveno.get() {
            return Err(WAIT_TIMEOUT.0);
        }
        let mut p = Box::new(Pozadavek {
            ov: OVERLAPPED {
                hEvent: self.event,
                ..Default::default()
            },
            data: *data,
        });
        let delka = size_of::<T>() as u32;
        let buf: *mut c_void = (&raw mut p.data).cast();
        let ov: *mut OVERLAPPED = &raw mut p.ov;
        // SAFETY: `buf` a `ov` míří do `p` na haldě, který žije až do
        // dokončení požadavku (i při zrušení, viz `zrus`). METHOD_BUFFERED
        // — vstup i výstup sdílí jeden buffer správné velikosti.
        let r = unsafe {
            DeviceIoControl(
                self.handle,
                kod,
                Some(buf),
                delka,
                vystup.then_some(buf),
                if vystup { delka } else { 0 },
                None,
                Some(ov),
            )
        };
        if let Err(e) = r {
            let c = win32(&e);
            if c != ERROR_IO_PENDING.0 {
                // Požadavek se do ovladače vůbec nedostal — nic nevisí.
                return Err(c);
            }
        }
        let mut prenos = 0u32;
        // SAFETY: týž handle a týž OVERLAPPED, jaký dostal DeviceIoControl.
        match unsafe { GetOverlappedResultEx(self.handle, ov, &mut prenos, limit_ms, false) } {
            Ok(()) => {
                if vystup {
                    *data = p.data;
                }
                Ok(())
            }
            Err(e) if win32(&e) == WAIT_TIMEOUT.0 => {
                self.zrus(p);
                Err(WAIT_TIMEOUT.0)
            }
            Err(e) => Err(win32(&e)),
        }
    }

    /// Zruší požadavek, na který se čekalo příliš dlouho.
    ///
    /// Po CancelIoEx se MUSÍ počkat, až ho ovladač opravdu vrátí — do té
    /// doby smí jádro zapisovat do OVERLAPPED i dat. Když ho nevrátí ani
    /// po [`LIMIT_ZRUSENI_MS`], paměť požadavku se schválně nechá žít
    /// navždy (pár desítek bajtů) a spojení se označí jako otrávené:
    /// bezpečnější než čekat bez konce (pad vlákno by viselo) nebo paměť
    /// uvolnit (jádro by pak psalo do cizích dat).
    fn zrus<T>(&self, p: Box<Pozadavek<T>>) {
        let ov: *const OVERLAPPED = &raw const p.ov;
        let mut prenos = 0u32;
        // SAFETY: týž handle a OVERLAPPED jako u DeviceIoControl; `p` žije.
        let dokonceno = unsafe {
            let _ = CancelIoEx(self.handle, Some(ov));
            match GetOverlappedResultEx(self.handle, ov, &mut prenos, LIMIT_ZRUSENI_MS, false) {
                Ok(()) => true,
                Err(e) => {
                    let c = win32(&e);
                    c != WAIT_TIMEOUT.0 && c != ERROR_IO_INCOMPLETE.0
                }
            }
        };
        if !dokonceno {
            log::error!("ViGEmBus nevrátil zrušený požadavek — spojení se zahazuje");
            self.otraveno.set(true);
            Box::leak(p);
        }
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        // SAFETY: oba handly vlastníme a nikde jinde nejsou. Otrávené
        // spojení: nevrácený požadavek drží vlastní referenci na událost,
        // takže zavřít náš handle je bezpečné.
        unsafe {
            let _ = CloseHandle(self.event);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Cesty ke všem přítomným rozhraním ViGEmBus (řetězce zakončené nulou).
///
/// CfgMgr32 místo SetupAPI (co používá `vigem-client`): stejná
/// informace za 0,25 ms místo 5 ms a bez načítání setupapi.dll.
fn cesty_rozhrani() -> Result<Vec<Vec<u16>>, VigemError> {
    // Mezi zjištěním velikosti a čtením může rozhraní přibýt (CR_BUFFER_SMALL).
    // Pár pokusů stačí; nekonečná smyčka by šla jen při neustálé změně.
    for _ in 0..4 {
        let mut delka = 0u32;
        // SAFETY: výstupní ukazatel na lokální proměnnou, GUID je konstanta.
        let cr = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &mut delka,
                &INTERFACE_GUID,
                PCWSTR::null(),
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if cr != CR_SUCCESS {
            return Err(VigemError::BusMissing);
        }
        let mut buf = vec![0u16; delka as usize];
        // SAFETY: buffer má délku, kterou si CfgMgr32 právě řekl.
        let cr = unsafe {
            CM_Get_Device_Interface_ListW(
                &INTERFACE_GUID,
                PCWSTR::null(),
                &mut buf,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if cr == CR_BUFFER_SMALL {
            continue;
        }
        if cr != CR_SUCCESS {
            return Err(VigemError::BusMissing);
        }
        let cesty: Vec<Vec<u16>> = buf
            .split(|&c| c == 0)
            .filter(|s| !s.is_empty())
            .map(|s| s.iter().copied().chain([0]).collect())
            .collect();
        return if cesty.is_empty() {
            Err(VigemError::BusMissing)
        } else {
            Ok(cesty)
        };
    }
    Err(VigemError::BusMissing)
}

/// Připojený virtuální Xbox 360 ovladač.
pub struct X360 {
    bus: Bus,
    serial: u32,
}

impl X360 {
    /// Připojí target na první volné sériové číslo. Obsazené číslo vrací
    /// 87 → zkusí se další; jiná chyba je skutečná a vrátí se hned.
    pub fn plug(bus: Bus) -> Result<X360, (Bus, VigemError)> {
        for serial in 1..=MAX_SERIAL {
            let mut p = PluginTarget {
                size: size_of::<PluginTarget>() as u32,
                serial,
                target_type: TARGET_XBOX360_WIRED,
                vendor: VID_MICROSOFT,
                product: PID_XBOX360,
            };
            match bus.ioctl(IOCTL_PLUGIN_TARGET, &mut p, false, LIMIT_MS) {
                Ok(()) => return Ok(X360 { bus, serial }),
                Err(c) if c == ERROR_INVALID_PARAMETER.0 => continue,
                Err(c) => return Err((bus, chyba_z_kodu(c))),
            }
        }
        Err((bus, VigemError::NoFreeSerial))
    }

    /// Počká, až si pad převezme Windows (xusb22 pošle „LED").
    ///
    /// Volat PRÁVĚ JEDNOU po `plug` a před jakýmkoli `unplug`: ovladač
    /// drží požadavek nejvýš 1 s ve vlastním vlákně a dvojí volání nebo
    /// odpojení během té sekundy trefí chybu v ovladači
    /// (KeWaitForSingleObject na adresu HANDLE, EmulationTargetPDO.cpp).
    pub fn wait_ready(&mut self) -> Result<(), VigemError> {
        let mut w = SerialOnly {
            size: size_of::<SerialOnly>() as u32,
            serial: self.serial,
        };
        self.bus
            .ioctl(
                IOCTL_WAIT_DEVICE_READY,
                &mut w,
                false,
                LIMIT_PRIPRAVENOST_MS,
            )
            .map_err(chyba_z_kodu)
    }

    /// Slot XInput (0–3) = číslo hráče − 1. Dokud xusb22 neposlal LED,
    /// vrací [`VigemError::NoUserIndex`].
    pub fn user_index(&mut self) -> Result<u32, VigemError> {
        let mut u = UserIndex {
            size: size_of::<UserIndex>() as u32,
            serial: self.serial,
            index: 0,
        };
        self.bus
            .ioctl(IOCTL_XUSB_GET_USER_INDEX, &mut u, true, LIMIT_MS)
            .map_err(chyba_z_kodu)?;
        Ok(u.index)
    }

    /// Pošle stav padu.
    ///
    /// Stejný report jako minule ovladač rovnou potvrdí a nic neposílá
    /// (6–8 µs) — opakování tedy jen ověří, že pad pořád existuje. Jiný
    /// report přijme jen tehdy, když xusb22 čeká na data; jinak vrátí
    /// [`VigemError::Busy`] a report ZAHODÍ → volající musí poslat znovu.
    pub fn submit(&mut self, report: XusbReport) -> Result<(), VigemError> {
        let mut s = SubmitReport {
            size: size_of::<SubmitReport>() as u32,
            serial: self.serial,
            report,
        };
        self.bus
            .ioctl(IOCTL_XUSB_SUBMIT_REPORT, &mut s, false, LIMIT_REPORT_MS)
            .map_err(chyba_z_kodu)
    }

    /// Odpojí target a vrátí spojení (jeho zahozením se zavře).
    pub fn unplug(self) -> Bus {
        let mut u = SerialOnly {
            size: size_of::<SerialOnly>() as u32,
            serial: self.serial,
        };
        // Chyba se jen zaloguje: zavření spojení (Drop) target odpojí tak
        // jako tak.
        if let Err(c) = self.bus.ioctl(IOCTL_UNPLUG_TARGET, &mut u, false, LIMIT_MS) {
            log::warn!(
                "odpojení virtuálního ovladače vrátilo chybu {c} — odpojí ho zavření spojení"
            );
        }
        self.bus
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keypad_core::PadButton;
    use windows::Win32::UI::Input::XboxController as xi;

    /// Masky tlačítek z `keypad_core` jdou do ovladače beze změny — musí
    /// to být přesně bity XInput (nezávislý zdroj: hlavičky Windows SDK
    /// přes windows-rs).
    #[test]
    fn tlacitka_odpovidaji_xinput() {
        let ocekavane = [
            (PadButton::DpadUp, xi::XINPUT_GAMEPAD_DPAD_UP),
            (PadButton::DpadDown, xi::XINPUT_GAMEPAD_DPAD_DOWN),
            (PadButton::DpadLeft, xi::XINPUT_GAMEPAD_DPAD_LEFT),
            (PadButton::DpadRight, xi::XINPUT_GAMEPAD_DPAD_RIGHT),
            (PadButton::Start, xi::XINPUT_GAMEPAD_START),
            (PadButton::Back, xi::XINPUT_GAMEPAD_BACK),
            (PadButton::L3, xi::XINPUT_GAMEPAD_LEFT_THUMB),
            (PadButton::R3, xi::XINPUT_GAMEPAD_RIGHT_THUMB),
            (PadButton::Lb, xi::XINPUT_GAMEPAD_LEFT_SHOULDER),
            (PadButton::Rb, xi::XINPUT_GAMEPAD_RIGHT_SHOULDER),
            (PadButton::A, xi::XINPUT_GAMEPAD_A),
            (PadButton::B, xi::XINPUT_GAMEPAD_B),
            (PadButton::X, xi::XINPUT_GAMEPAD_X),
            (PadButton::Y, xi::XINPUT_GAMEPAD_Y),
        ];
        assert_eq!(ocekavane.len(), PadButton::ALL.len(), "každé tlačítko");
        for (tlacitko, xinput) in ocekavane {
            assert_eq!(tlacitko.mask(), xinput.0, "{tlacitko:?}");
        }
    }

    #[test]
    fn report_pole_po_poli() {
        let s = PadState {
            buttons: 0x1234,
            left_trigger: 7,
            right_trigger: 250,
            thumb_lx: -32_767,
            thumb_ly: 32_767,
            thumb_rx: 23_170,
            thumb_ry: -1,
        };
        assert_eq!(
            XusbReport::from(&s),
            XusbReport {
                buttons: 0x1234,
                lt: 7,
                rt: 250,
                lx: -32_767,
                ly: 32_767,
                rx: 23_170,
                ry: -1,
            }
        );
        assert_eq!(XusbReport::from(&PadState::NEUTRAL), XusbReport::NEUTRAL);
    }

    #[test]
    fn kody_chyb() {
        assert_eq!(chyba_z_kodu(55), VigemError::Gone);
        assert_eq!(chyba_z_kodu(170), VigemError::Busy);
        assert_eq!(chyba_z_kodu(259), VigemError::Busy, "report zahozen");
        assert_eq!(chyba_z_kodu(483), VigemError::NotReadyYet);
        assert_eq!(chyba_z_kodu(650), VigemError::NoUserIndex);
        assert_eq!(chyba_z_kodu(258), VigemError::TimedOut);
        assert_eq!(chyba_z_kodu(5), VigemError::Other(5));
    }

    /// Jen skutečná odpověď ovladače je „jiná verze"; vypršení limitu
    /// a ostatní chyby jsou porucha (revize: TimedOut končil jako
    /// „nepodporovaná verze" s nabídkou přeinstalace).
    #[test]
    fn kontrola_verze_rozlisi_odpoved_od_poruchy() {
        for c in [1, 50, 87] {
            assert_eq!(chyba_verze(c), VigemError::VersionMismatch, "{c}");
        }
        assert_eq!(chyba_verze(258), VigemError::TimedOut);
        assert_eq!(chyba_verze(55), VigemError::Gone);
        assert_eq!(chyba_verze(5), VigemError::Other(5));
    }

    #[test]
    fn kod_win32_z_hresult() {
        let e = windows::core::Error::from(windows::Win32::Foundation::WIN32_ERROR(170));
        assert_eq!(win32(&e), 170);
    }
}

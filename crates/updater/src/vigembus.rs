//! Ovladač ViGEmBus — jediný zdroj pravdy pro instalátor i aplikaci.
//!
//! KeyPad bez něj nevytvoří virtuální gamepad. Tady je:
//! - **napevno zapsané vydání** (adresa, zrcadlo, velikost, SHA-256,
//!   vydavatel, parametry) — instalátor nikdy nespustí nic jiného než
//!   přesně tenhle soubor (princip 8: cizí binárky jen v ověřené podobě),
//! - **zjištění stavu a verze** jen čtením (registr, správce zařízení) —
//!   sdílí ho instalátor (instalovat, když ovladač chybí, aktualizovat,
//!   když je starší) i aplikace (stavový řádek, rada, co dělat),
//! - **stažení s ověřením** do paměti: nejdřív oficiální adresa, pak
//!   zrcadlo v repu KeyPadu — obojí proti témuž otisku.
//!
//! Instalaci samotnou (zápis na disk, zámek, podpis, výzva UAC) dělá jen
//! `KeyPadSetup.exe` — aplikace o práva správce nikdy nežádá a má jedinou
//! cestu: spustit `KeyPadSetup.exe /vigembus` (viz `SETUP_ARG_VIGEMBUS`).
//!
//! Proč zrovna 1.22.0 napevno: projekt je archivovaný (poslední vydání
//! 2. 11. 2023), takže se pin nikdy nebude muset měnit. Otisk sedí se
//! třemi nezávislými zdroji (vlastní stažení, manifest winget-pkgs z roku
//! 2023, MD5 z Azure blobu z doby nahrání). Jakákoli změna na GitHubu,
//! v CDN, v zrcadle nebo proxy s inspekcí TLS vede k odmítnutí, nikdy ke
//! spuštění.

use windows::core::{GUID, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_Registry_PropertyW, CM_Get_DevNode_Status, CM_Get_Device_ID_ListW,
    CM_Get_Device_ID_List_SizeW, CM_Get_Device_Interface_List_SizeW, CM_Locate_DevNodeW,
    CM_DEVNODE_STATUS_FLAGS, CM_DRP_DRIVER, CM_DRP_HARDWAREID, CM_GETIDLIST_FILTER_ENUMERATOR,
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_LOCATE_DEVNODE_NORMAL, CM_PROB, CR_BUFFER_SMALL,
    CR_SUCCESS, DN_HAS_PROBLEM, DN_NEED_RESTART, DN_STARTED,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
    KEY_WOW64_32KEY, KEY_WOW64_64KEY, REG_SAM_FLAGS, RRF_RT_REG_SZ,
};

use crate::http;
use crate::sha256;

// ── Napevno zapsané vydání ─────────────────────────────────────────

/// Verze instalátoru (ovladač uvnitř je 1.21.442.0 — poslední vůbec).
pub const VERSION: &str = "1.22.0";
/// Oficiální adresa: `github.com` přesměruje (302) do CDN GitHubu,
/// WinHttp přesměrování z HTTPS na HTTPS sleduje samo. Za běhu se
/// neptáme API — žádný limit 60 dotazů, nic se nevybírá podle odpovědi.
pub const DOWNLOAD_HOST: &str = "github.com";
pub const DOWNLOAD_PATH: &str =
    "/nefarius/ViGEmBus/releases/download/v1.22.0/ViGEmBus_1.22.0_x64_x86_arm64.exe";
/// Zrcadlo v repu KeyPadu (`mirror/`, schválené vlastníkem 29. 9. 2026):
/// repo ViGEmBus je archivované a kdyby zmizelo, instalace ovladače by
/// jinak skončila na „nepodařilo se stáhnout". Zrcadlo se ověřuje týmž
/// otiskem jako originál, takže podstrčit nic jiného neumí — horší, než
/// že nedodá nic, být nemůže. Přes `raw` (bez API, bez limitu dotazů).
pub const MIRROR_HOST: &str = crate::RAW_HOST;
pub const MIRROR_PATH: &str = "/iva-exe/KeyPad/main/mirror/ViGEmBus_1.22.0_x64_x86_arm64.exe";
/// Jméno souboru, pod kterým se instalátor uloží (a ukáže se ve výzvě UAC).
pub const SETUP_FILE: &str = "ViGEmBus_1.22.0_x64_x86_arm64.exe";
/// Přesná velikost v bajtech.
pub const SETUP_SIZE: usize = 6_278_576;
/// SHA-256 souboru — totéž jako `InstallerSha256` v manifestu
/// winget-pkgs `ViGEm.ViGEmBus` 1.22.0.
pub const SETUP_SHA256: [u8; 32] = [
    0x89, 0x22, 0x0A, 0x78, 0x65, 0x07, 0x6B, 0x34, 0x28, 0x92, 0xF9, 0x88, 0x65, 0xF3, 0x49, 0x9F,
    0xB7, 0xC4, 0xCF, 0xD6, 0x73, 0x15, 0x9E, 0x89, 0xD3, 0x52, 0xC3, 0x60, 0xFD, 0x01, 0x4C, 0x6A,
];
/// Tentýž otisk textem — tak, jak ho ukazuje `Get-FileHash` a winget.
/// Test hlídá, že se s polem výš nerozešel.
pub const SETUP_SHA256_HEX: &str =
    "89220A7865076B342892F98865F3499FB7C4CFD673159E89D352C360FD014C6A";
/// Vydavatel v podpisu Authenticode (jednoduché jméno certifikátu).
/// Certifikát sám vypršel v únoru 2025 — podpis drží časové razítko
/// DigiCert; otisk souboru výš je hlavní kontrola, podpis druhá.
pub const SIGNER: &str = "Nefarius Software Solutions e.U.";
/// Tichá instalace: přepínače zavaděče Advanced Installer (`/exenoui`)
/// musí být před přepínači pro msiexec (`/qn /norestart`). Tytéž
/// používá winget.
pub const SETUP_ARGS: &str = "/exenoui /qn /norestart";
/// Rozhraní sběrnice — přes něj se k ní připojuje každý klient
/// (vigem-client i vlastní klient aplikace). Funkční pravda o tom,
/// jestli ovladač běží.
pub const INTERFACE_GUID: GUID = GUID::from_u128(0x96E42B22_F5E9_42F8_B043_ED0F932F014F);
/// ProductCode balíku MSI 1.22.0 (klíč v Aplikacích).
pub const PRODUCT_CODE: &str = "{966606F3-2745-49E9-BF15-5C3EAA4E9077}";
/// UpgradeCode řady 1.18–1.22 (jen pro úplnost; detekce jde přes
/// ProductCode a jméno v Aplikacích).
pub const UPGRADE_CODE: &str = "{67175F6C-AA18-43A7-AE60-2FC3FD10BF79}";
/// Jméno v Aplikacích (hledá se i ve starších verzích).
pub const DISPLAY_NAME: &str = "ViGEm Bus Driver";
/// Služba ovladače.
pub const SERVICE: &str = "ViGEmBus";
/// Hardware ID zařízení sběrnice: 1.17+ (`Gen1`) a starší (devcon).
pub const HARDWARE_IDS: [&str; 2] = [r"Nefarius\ViGEmBus\Gen1", r"Root\ViGEmBus"];
/// Kam zavaděč Advanced Installer rozbaluje MSI — relativně k
/// `%ProgramData%`. Instalátor odmítne pokračovat, když tu složka už je
/// (klasický vektor: standardní uživatel ji může založit předem a
/// podstrčit do ní DLL pro proces s právy správce).
pub const EXTRACTION_DIR: &str = r"Nefarius Software Solutions\ViGEm Bus Driver 1.22.0";
/// Složka výrobce v `%ProgramData%` (rodič [`EXTRACTION_DIR`]).
pub const VENDOR_DIR: &str = "Nefarius Software Solutions";
/// Stránka vydání — ruční cesta, když automatická nejde.
pub const RELEASES_URL: &str = "https://github.com/nefarius/ViGEmBus/releases";
/// Licence ovladače (text pro okno instalátoru a README).
pub const LICENSE: &str = "BSD-3-Clause";
/// Repozitář autora — cíl odkazu u creditu.
pub const REPO_URL: &str = "https://github.com/nefarius/ViGEmBus";
/// Credit, jak ho ukazuje instalátor i detaily aplikace (vždy s odkazem
/// na [`REPO_URL`]). Jedno místo, ať se texty nerozejdou.
pub const CREDIT: &str = "ViGEmBus — Nefarius Software Solutions e.U.";

/// Celá adresa ke stažení (pro výpis; stahuje se přes host + cestu).
pub fn download_url() -> String {
    Source::Official.url()
}

/// Odkud se instalátor stahuje — v tomhle pořadí.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Vydání v repu autora (github.com → CDN GitHubu).
    Official,
    /// Zrcadlo v repu KeyPadu ([`MIRROR_PATH`]).
    Mirror,
}

impl Source {
    pub const ALL: [Source; 2] = [Source::Official, Source::Mirror];

    pub fn host(self) -> &'static str {
        match self {
            Source::Official => DOWNLOAD_HOST,
            Source::Mirror => MIRROR_HOST,
        }
    }

    pub fn path(self) -> &'static str {
        match self {
            Source::Official => DOWNLOAD_PATH,
            Source::Mirror => MIRROR_PATH,
        }
    }

    pub fn url(self) -> String {
        format!("https://{}{}", self.host(), self.path())
    }
}

// ── Stav ───────────────────────────────────────────────────────────

/// Stav ovladače v systému.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusState {
    /// Rozhraní sběrnice existuje — gamepad jde vytvořit.
    Ready,
    /// Po ovladači není ani stopa: žádné rozhraní, žádná služba, žádný
    /// záznam v Aplikacích. Instalátor pak ovladač nainstaluje. Jinak
    /// instalátor ViGEmBus spouští jen nad ovladačem, o kterém ví, že je
    /// starší ([`needs_update`]) — MSI při „první instalaci" odebere
    /// stávající zařízení sběrnice a při upgradu chce restart, takže nad
    /// běžícím aktuálním nebo neznámým ovladačem nemá co dělat.
    NotInstalled,
    /// Ovladač v systému je (služba nebo záznam v Aplikacích), ale
    /// rozhraní ne. `device` = stav zařízení ve správci zařízení
    /// (`None` = zařízení se nenašlo). `in_apps` = má záznam
    /// v Aplikacích. Bez něj ho do systému dal jiný program (devcon,
    /// nefcon, přibalený instalátor) nebo po něm zbyla jen služba —
    /// rada pak nesmí posílat do Aplikací, kde nic není. KeyPadSetup ho
    /// přeinstaluje jen tehdy, když je prokazatelně starší.
    InstalledNotRunning {
        device: Option<DeviceStatus>,
        in_apps: bool,
    },
}

/// Stav zařízení sběrnice ve správci zařízení.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceStatus {
    /// Kód problému (22 = vypnuté, 14 = chce restart, 48/52 = blokované…).
    pub problem: Option<u32>,
    /// Windows chtějí restart, aby zařízení doběhlo.
    pub need_restart: bool,
    /// Ovladač zařízení je spuštěný.
    pub started: bool,
}

/// Co má uživatel udělat s ovladačem, který je, ale neběží.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advice {
    Restart,
    EnableDevice,
    /// Windows ovladač nenechaly naběhnout (39/48/52).
    Blocked(u32),
    /// Zařízení chybí — instalace je jen zčásti. `in_apps` = jde
    /// odebrat v Aplikacích (jinak jen ruční instalace).
    Reinstall {
        in_apps: bool,
    },
    Problem {
        code: u32,
        in_apps: bool,
    },
}

impl BusState {
    /// Rada pro stav [`BusState::InstalledNotRunning`]; jinak `None`.
    pub fn advice(&self) -> Option<Advice> {
        match self {
            BusState::InstalledNotRunning { device, in_apps } => {
                Some(advice_for(*device, *in_apps))
            }
            _ => None,
        }
    }
}

/// Čisté rozhodnutí: stav zařízení (a záznam v Aplikacích) → rada.
pub fn advice_for(device: Option<DeviceStatus>, in_apps: bool) -> Advice {
    let Some(d) = device else {
        return Advice::Reinstall { in_apps };
    };
    match d.problem {
        Some(22) => Advice::EnableDevice,
        // 39 = ovladač nešel načíst, 48 = zablokovaný (seznam
        // zranitelných ovladačů), 52 = neověřený podpis — s Integritou
        // paměti se nekompatibilní ovladač hlásí typicky jedním z nich.
        Some(c @ (39 | 48 | 52)) => Advice::Blocked(c),
        Some(14) => Advice::Restart,
        Some(code) if !d.need_restart => Advice::Problem { code, in_apps },
        // Bez problému, ale bez rozhraní: čerstvě nainstalovaný nebo
        // rozběhnutý napůl — restart je nejkratší cesta.
        _ => Advice::Restart,
    }
}

impl Advice {
    /// Vede rada k ruční instalaci ze stránky vydání? (Okno k ní pak
    /// nabídne tlačítko — adresu z textu v GDI okně zkopírovat nejde.)
    pub fn suggests_manual_install(&self) -> bool {
        matches!(
            self,
            Advice::Reinstall { in_apps: false } | Advice::Problem { in_apps: false, .. }
        )
    }

    /// Česká rada pro uživatele (instalátor i aplikace) — krátká a jen
    /// s tím, podle čeho se dá jednat. Kód problému v ní není (uživateli
    /// nic neřekne); do logu ho dává ten, kdo radu ukazuje (`{:?}` stavu).
    ///
    /// Aplikace se zmiňují jen tam, kde záznam opravdu je. Bez něj
    /// zbývá restart, Správce zařízení a ruční instalace.
    pub fn text(&self) -> String {
        match self {
            Advice::Restart => "Restartuj počítač — ovladač ViGEmBus pak naběhne.".into(),
            Advice::EnableDevice => "Ovladač ViGEmBus je vypnutý. Zapni ho ve Správci zařízení: \
                                     Systémová zařízení → Nefarius Virtual Gamepad Emulation Bus \
                                     → Povolit zařízení."
                .into(),
            // KeyPad do Izolace jádra nesahá — jen řekne, kde hledat.
            Advice::Blocked(_) => "Windows ovladač ViGEmBus zablokovaly. Zkontroluj Integritu \
                                   paměti (Zabezpečení Windows → Zabezpečení zařízení → Izolace \
                                   jádra)."
                .into(),
            // Záznam v Aplikacích je, zařízení sběrnice ne (typicky
            // upgrade, který starou verzi odebral a novou nepřidal).
            // Kořenové zařízení zakládá instalátor ViGEmBus — restart ho
            // nevytvoří, proto tu restart není.
            Advice::Reinstall { in_apps: true } => {
                "Ovladač ViGEmBus nefunguje. Odeber „ViGEm Bus Driver“ v Nastavení → Aplikace \
                 a spusť KeyPadSetup znovu."
                    .into()
            }
            Advice::Problem { in_apps: true, .. } => {
                "Ovladač ViGEmBus nefunguje. Restartuj počítač; když to nepomůže, odeber \
                 „ViGEm Bus Driver“ v Nastavení → Aplikace a spusť KeyPadSetup znovu."
                    .into()
            }
            // Bez záznamu v Aplikacích: ruční instalace (adresu okno
            // nabídne i tlačítkem — viz `suggests_manual_install`).
            // Restart tu smysl má: služba odebraného ovladače může
            // v registru zůstat až do restartu — a bez ní KeyPadSetup
            // nainstaluje ovladač sám.
            Advice::Reinstall { in_apps: false } | Advice::Problem { in_apps: false, .. } => {
                format!(
                    "Ovladač ViGEmBus nefunguje. Restartuj počítač; když to nepomůže, \
                     nainstaluj ho ručně z {RELEASES_URL}."
                )
            }
        }
    }
}

/// Zjistí stav ovladače. Jen čte (registr, správce zařízení) — nic
/// nezakládá a nic nemění; volat smí kdokoli bez práv správce.
pub fn state() -> BusState {
    if interface_present() {
        return BusState::Ready;
    }
    let in_apps = arp_entry_exists();
    if !in_apps && !service_key_exists() {
        return BusState::NotInstalled;
    }
    BusState::InstalledNotRunning {
        device: device_status(),
        in_apps,
    }
}

/// Existuje (přítomné) rozhraní sběrnice? Prázdný seznam má délku 1
/// (samotné ukončovací NUL).
pub fn interface_present() -> bool {
    let mut len = 0u32;
    // SAFETY: jen dotaz na délku seznamu; GUID žije po celé volání.
    let cr = unsafe {
        CM_Get_Device_Interface_List_SizeW(
            &mut len,
            &INTERFACE_GUID,
            PCWSTR::null(),
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        )
    };
    cr == CR_SUCCESS && len > 1
}

/// Otevře klíč pro čtení; `None`, když neexistuje.
fn open_key(root: HKEY, path: &str, view: REG_SAM_FLAGS) -> Option<Key> {
    let mut key = HKEY::default();
    // SAFETY: jen otevření pro čtení; klíč zavře `Key::drop`.
    let rc = unsafe { RegOpenKeyExW(root, &HSTRING::from(path), None, KEY_READ | view, &mut key) };
    rc.is_ok().then_some(Key(key))
}

/// Otevřený klíč registru, který se sám zavře.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: klíč pochází z RegOpenKeyExW a zavírá se právě jednou.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn service_key_exists() -> bool {
    open_key(HKEY_LOCAL_MACHINE, &service_path(), KEY_WOW64_64KEY).is_some()
}

fn service_path() -> String {
    format!(r"SYSTEM\CurrentControlSet\Services\{SERVICE}")
}

/// Záznam v Aplikacích — v 64bitovém i 32bitovém pohledu registru:
/// podle ProductCode 1.22.0 a podle jména u kterékoli verze. Veřejné
/// kvůli závěru odinstalace KeyPadu („odebereš ho v Aplikacích" jen
/// tam, kde záznam je).
pub fn arp_entry_exists() -> bool {
    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    [KEY_WOW64_64KEY, KEY_WOW64_32KEY].iter().any(|&view| {
        let Some(root) = open_key(HKEY_LOCAL_MACHINE, UNINSTALL, view) else {
            return false;
        };
        if open_key(root.0, PRODUCT_CODE, view).is_some() {
            return true;
        }
        subkeys(&root)
            .iter()
            .any(|name| read_sz(root.0, name, "DisplayName").is_some_and(|d| is_bus_name(&d)))
    })
}

/// Jméno v Aplikacích patří ViGEmBus (i starší verze, jinak psané).
fn is_bus_name(display_name: &str) -> bool {
    display_name
        .to_lowercase()
        .contains(&DISPLAY_NAME.to_lowercase())
}

/// Jména podklíčů.
fn subkeys(key: &Key) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..100_000u32 {
        // Jméno klíče má nejvýš 255 znaků.
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        // SAFETY: buffer a jeho délka jdou spolu; ostatní výstupy nechceme.
        let rc = unsafe {
            RegEnumKeyExW(
                key.0,
                i,
                Some(PWSTR(buf.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            )
        };
        if rc.is_err() {
            break;
        }
        out.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    out
}

/// Řetězcová hodnota `name` v podklíči `sub`.
fn read_sz(root: HKEY, sub: &str, name: &str) -> Option<String> {
    let sub = HSTRING::from(sub);
    let val = HSTRING::from(name);
    // SAFETY: velikost se nejdřív zjistí, pak se čte do bufferu té
    // velikosti; RegGetValueW nezapíše víc, než mu řekneme.
    unsafe {
        let mut size = 0u32;
        if RegGetValueW(root, &sub, &val, RRF_RT_REG_SZ, None, None, Some(&mut size)).is_err()
            || size == 0
        {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        RegGetValueW(
            root,
            &sub,
            &val,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
        .ok()
        .ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

/// Rozdělí seznam řetězců ukončených NUL (REG_MULTI_SZ, seznamy
/// z cfgmgr32) — končí prázdným řetězcem.
fn split_multi_sz(buf: &[u16]) -> Vec<String> {
    buf.split(|&c| c == 0)
        .take_while(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// Stav zařízení sběrnice, nebo `None`, když v systému není.
///
/// Nejdřív podle služby (`Services\ViGEmBus\Enum\0`, tak ho najde
/// i správce zařízení). Ten záznam ale drží jen zařízení, u kterých
/// ovladač běží — vypnuté zařízení v něm chybí, a právě to chceme umět
/// poznat. Proto záloha: kořenová zařízení třídy System (`ROOT\SYSTEM\…`,
/// tam je zakládá instalátor ViGEmBus) podle hardware ID.
fn device_status() -> Option<DeviceStatus> {
    bus_devnode().and_then(devnode_status)
}

/// Zařízení sběrnice (devnode), ať běží, nebo ne.
fn bus_devnode() -> Option<u32> {
    let from_service = open_key(
        HKEY_LOCAL_MACHINE,
        &format!(r"{}\Enum", service_path()),
        KEY_WOW64_64KEY,
    )
    .and_then(|k| read_sz(k.0, "", "0"));
    let mut ids: Vec<String> = from_service.into_iter().collect();
    ids.extend(root_system_devices().into_iter().filter(|id| {
        locate(id)
            .map(|dn| hardware_ids(dn).iter().any(|h| is_bus_hwid(h)))
            .unwrap_or(false)
    }));
    ids.iter().find_map(|id| locate(id))
}

/// Verze ovladače v posledním vydání ViGEmBus ([`VERSION`] ho obsahuje;
/// novější už nikdy nebude — projekt je archivovaný).
pub const DRIVER_VERSION: [u16; 4] = [1, 21, 442, 0];

/// Verze ovladače ViGEmBus, který je v systému (`DriverVersion` jeho
/// zařízení ve třídě zařízení). `None`, když zařízení není nebo verzi
/// nejde přečíst. Jen čte.
pub fn driver_version() -> Option<[u16; 4]> {
    let dn = bus_devnode()?;
    // Klíč ovladače zařízení, např. „{4d36e97d-…}\0012".
    let mut buf = [0u16; 256];
    let mut len = std::mem::size_of_val(&buf) as u32;
    // SAFETY: délka v bajtech odpovídá bufferu.
    let cr = unsafe {
        CM_Get_DevNode_Registry_PropertyW(
            dn,
            CM_DRP_DRIVER,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            &mut len,
            0,
        )
    };
    if cr != CR_SUCCESS {
        return None;
    }
    let driver_key = split_multi_sz(&buf[..(len as usize / 2).min(buf.len())])
        .into_iter()
        .next()?;
    let class = open_key(
        HKEY_LOCAL_MACHINE,
        &format!(r"SYSTEM\CurrentControlSet\Control\Class\{driver_key}"),
        KEY_WOW64_64KEY,
    )?;
    parse_version(&read_sz(class.0, "", "DriverVersion")?)
}

/// „1.21.442.0" → [1, 21, 442, 0]. Chybějící části jsou 0.
fn parse_version(s: &str) -> Option<[u16; 4]> {
    let mut out = [0u16; 4];
    let mut parts = s.trim().split('.');
    for slot in out.iter_mut() {
        match parts.next() {
            Some(p) => *slot = p.trim().parse().ok()?,
            None => break,
        }
    }
    parts.next().is_none().then_some(out)
}

/// Je v systému ovladač starší než ten z posledního vydání? Pak ho
/// instalátor aktualizuje. Neznámou verzi nikdy nepovažuje za starou —
/// na cizí ovladač, o kterém nic nevíme, se nesahá.
pub fn needs_update() -> bool {
    matches!(driver_version(), Some(v) if v < DRIVER_VERSION)
}

fn is_bus_hwid(id: &str) -> bool {
    HARDWARE_IDS.iter().any(|h| h.eq_ignore_ascii_case(id))
}

/// Instance `ROOT\SYSTEM\*`.
fn root_system_devices() -> Vec<String> {
    let filter = HSTRING::from(r"ROOT\SYSTEM");
    // Seznam se mezi zjištěním délky a čtením může změnit (zařízení
    // přibylo) — pak se to zkusí znovu.
    for _ in 0..3 {
        let mut len = 0u32;
        // SAFETY: jen dotaz na délku.
        let cr = unsafe {
            CM_Get_Device_ID_List_SizeW(&mut len, &filter, CM_GETIDLIST_FILTER_ENUMERATOR)
        };
        if cr != CR_SUCCESS || len == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        // SAFETY: buffer má délku, kterou jsme právě zjistili.
        let cr =
            unsafe { CM_Get_Device_ID_ListW(&filter, &mut buf, CM_GETIDLIST_FILTER_ENUMERATOR) };
        if cr == CR_SUCCESS {
            return split_multi_sz(&buf);
        }
        if cr != CR_BUFFER_SMALL {
            return Vec::new();
        }
    }
    Vec::new()
}

/// Najde přítomné zařízení podle ID instance.
fn locate(id: &str) -> Option<u32> {
    let mut dn = 0u32;
    // SAFETY: jen vyhledání; ID žije po celé volání.
    let cr = unsafe { CM_Locate_DevNodeW(&mut dn, &HSTRING::from(id), CM_LOCATE_DEVNODE_NORMAL) };
    (cr == CR_SUCCESS).then_some(dn)
}

fn hardware_ids(dn: u32) -> Vec<String> {
    let mut buf = [0u16; 1024];
    let mut len = std::mem::size_of_val(&buf) as u32;
    // SAFETY: délka v bajtech odpovídá bufferu.
    let cr = unsafe {
        CM_Get_DevNode_Registry_PropertyW(
            dn,
            CM_DRP_HARDWAREID,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            &mut len,
            0,
        )
    };
    if cr != CR_SUCCESS {
        return Vec::new();
    }
    split_multi_sz(&buf[..(len as usize / 2).min(buf.len())])
}

fn devnode_status(dn: u32) -> Option<DeviceStatus> {
    let mut status = CM_DEVNODE_STATUS_FLAGS(0);
    let mut problem = CM_PROB(0);
    // SAFETY: výstupy jsou platné proměnné.
    let cr = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, dn, 0) };
    if cr != CR_SUCCESS {
        return None;
    }
    Some(DeviceStatus {
        problem: status.contains(DN_HAS_PROBLEM).then_some(problem.0),
        need_restart: status.contains(DN_NEED_RESTART),
        started: status.contains(DN_STARTED),
    })
}

// ── Stažení ────────────────────────────────────────────────────────

/// Proč se stažení nepovedlo — podle toho instalátor radí „zkus znovu"
/// (síť), nebo ruční cestu (soubor není ten oficiální; opakování by
/// dopadlo stejně).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    Network(String),
    NotOfficial(String),
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::Network(e) => write!(f, "stažení se nepovedlo ({e})"),
            DownloadError::NotOfficial(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for DownloadError {}

/// Stáhne instalátor do paměti a ověří přesnou velikost a SHA-256
/// (oficiální adresa, pak zrcadlo — viz [`fetch_setup`]).
///
/// Vrací jen bajty, které prošly — nic jiného se nedá dál použít.
/// Na disk nic nepíše (to dělá instalátor, i s druhou kontrolou).
pub fn download_verified(mut progress: impl FnMut(usize)) -> Result<Vec<u8>, DownloadError> {
    fetch_setup(|_, n| progress(n)).map(|(data, _)| data)
}

/// Jako [`download_verified`], jen navíc řekne, odkud soubor přišel,
/// a `progress` dostává i zdroj (při přechodu na zrcadlo počítá znovu
/// od nuly).
pub fn fetch_setup(
    progress: impl FnMut(Source, usize),
) -> Result<(Vec<u8>, Source), DownloadError> {
    fetch_from(
        &Source::ALL,
        |src, p| {
            http::get_limited(src.host(), src.path(), SETUP_SIZE, p).map_err(|e| e.to_string())
        },
        verify,
        progress,
    )
}

/// Zdroje po řadě, dokud jeden nedodá soubor, který projde `verify`.
///
/// Špatný soubor ze zdroje se zahodí a zkusí se další (proxy s inspekcí
/// TLS nebo změna na GitHubu nemusí zasáhnout oba), spustit se ale nedá
/// nikdy — ven jdou jen ověřené bajty. Když neprojde nic, vyhrává
/// [`DownloadError::NotOfficial`] nad síťovou chybou: aspoň jeden zdroj
/// něco dodal a opakování by dopadlo stejně (instalátor pak radí ruční
/// cestu, ne „zkus znovu"). `fetch` a `verify` jsou parametry kvůli
/// testům — síť ani skutečný soubor v nich nejsou potřeba.
fn fetch_from<F, V>(
    sources: &[Source],
    mut fetch: F,
    verify: V,
    mut progress: impl FnMut(Source, usize),
) -> Result<(Vec<u8>, Source), DownloadError>
where
    F: FnMut(Source, &mut dyn FnMut(usize)) -> Result<Vec<u8>, String>,
    V: Fn(&[u8]) -> Result<(), String>,
{
    let mut network = Vec::new();
    let mut bad = Vec::new();
    for &src in sources {
        let mut report = |n: usize| progress(src, n);
        match fetch(src, &mut report) {
            Ok(data) => match verify(&data) {
                Ok(()) => return Ok((data, src)),
                Err(e) => bad.push(format!("{}: {e}", src.host())),
            },
            Err(e) => network.push(format!("{}: {e}", src.host())),
        }
    }
    if bad.is_empty() {
        Err(DownloadError::Network(network.join("; ")))
    } else {
        bad.extend(network);
        Err(DownloadError::NotOfficial(bad.join("; ")))
    }
}

/// Je to bajt po bajtu oficiální instalátor [`VERSION`]?
pub fn verify(data: &[u8]) -> Result<(), String> {
    if data.len() != SETUP_SIZE {
        return Err(format!(
            "stažený soubor má {} B místo {SETUP_SIZE} B — není to oficiální instalátor \
             ViGEmBus {VERSION}",
            data.len()
        ));
    }
    verify_digest(&sha256::sha256(data)?)
}

/// Sedí otisk s napevno zapsaným?
pub fn verify_digest(digest: &[u8; 32]) -> Result<(), String> {
    if *digest == SETUP_SHA256 {
        Ok(())
    } else {
        Err(format!(
            "otisk SHA-256 nesedí ({}…) — není to oficiální instalátor ViGEmBus {VERSION}",
            &sha256::to_hex(digest)[..16]
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otisk_pole_a_text_se_nerozesly() {
        assert_eq!(sha256::to_hex(&SETUP_SHA256), SETUP_SHA256_HEX);
    }

    #[test]
    fn adresa_jmeno_a_verze_sedi() {
        assert!(DOWNLOAD_PATH.ends_with(&format!("/{SETUP_FILE}")));
        assert!(DOWNLOAD_PATH.contains(&format!("/download/v{VERSION}/")));
        assert!(SETUP_FILE.contains(VERSION));
        assert!(EXTRACTION_DIR.starts_with(VENDOR_DIR));
        assert!(EXTRACTION_DIR.ends_with(&format!("{DISPLAY_NAME} {VERSION}")));
        assert_eq!(
            download_url(),
            "https://github.com/nefarius/ViGEmBus/releases/download/v1.22.0/\
             ViGEmBus_1.22.0_x64_x86_arm64.exe"
        );
        assert!(RELEASES_URL.starts_with("https://github.com/nefarius/ViGEmBus/"));
        assert!(RELEASES_URL.starts_with(REPO_URL));
        assert!(CREDIT.starts_with("ViGEmBus — ") && CREDIT.ends_with(SIGNER));
        // Přepínače zavaděče musí být před přepínači msiexec.
        assert!(SETUP_ARGS.find("/exenoui") < SETUP_ARGS.find("/qn"));
    }

    /// Zrcadlo leží v repu KeyPadu (větev main, složka mirror/) pod
    /// stejným jménem jako originál; zdroje jdou v pořadí originál → zrcadlo.
    #[test]
    fn zrcadlo_je_v_repu_keypadu() {
        assert_eq!(
            Source::Mirror.url(),
            "https://raw.githubusercontent.com/iva-exe/KeyPad/main/mirror/\
             ViGEmBus_1.22.0_x64_x86_arm64.exe"
        );
        assert_eq!(
            MIRROR_PATH,
            format!("/{}/main/mirror/{SETUP_FILE}", crate::REPO)
        );
        assert_eq!(Source::ALL, [Source::Official, Source::Mirror]);
        assert_eq!(Source::Official.url(), download_url());
    }

    /// Soubor zrcadla v repu je bajt po bajtu oficiální instalátor
    /// a README zrcadla uvádí tentýž otisk — kdyby se někdo pokusil
    /// zrcadlo „aktualizovat", test to chytí dřív než uživatelé
    /// (instalátor by ho stejně odmítl).
    ///
    /// Zrcadlo je redistribuce binárky: BSD-3-Clause k ní chce přiložený
    /// copyright, podmínky a zřeknutí se odpovědnosti — samotný odkaz
    /// nestačí. Proto vedle leží nezměněný `LICENSE.txt` z repa autora.
    #[test]
    fn soubor_zrcadla_sedi_s_pinem() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mirror");
        let data = std::fs::read(dir.join(SETUP_FILE)).expect("mirror/ v repu");
        verify(&data).expect("zrcadlo = oficiální instalátor");
        let readme = std::fs::read_to_string(dir.join("README.md")).expect("mirror/README.md");
        assert!(readme.contains(SETUP_SHA256_HEX), "README zrcadla: otisk");
        assert!(readme.contains(REPO_URL), "README zrcadla: odkaz na autora");
        assert!(readme.contains(LICENSE), "README zrcadla: licence");
        assert!(
            readme.contains("LICENSE.txt"),
            "README zrcadla: text licence"
        );
        let license =
            std::fs::read_to_string(dir.join("LICENSE.txt")).expect("mirror/LICENSE.txt v repu");
        for line in [
            "BSD 3-Clause License",
            "Copyright (c) 2016-2020, Nefarius Software Solutions e.U.",
            "2. Redistributions in binary form must reproduce the above copyright notice,",
            "THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS \"AS IS\"",
        ] {
            assert!(
                license.lines().any(|l| l.trim_end() == line),
                "LICENSE.txt zrcadla: chybí „{line}“"
            );
        }
    }

    /// Záloha naostro bez sítě: originál „404", zrcadlo vrátí skutečný
    /// soubor z `mirror/` → projde skutečným ověřením. Tentýž soubor
    /// s jediným změněným bajtem neprojde z žádného zdroje.
    #[test]
    fn zaloha_se_skutecnym_souborem_a_skutecnym_overenim() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../mirror")
            .join(SETUP_FILE);
        let good = std::fs::read(path).expect("mirror/ v repu");
        let mut bad = good.clone();
        bad[SETUP_SIZE / 2] ^= 1;
        let serve = |data: Vec<u8>| {
            move |src: Source, p: &mut dyn FnMut(usize)| match src {
                Source::Official => Err("soubor na serveru není".to_string()),
                Source::Mirror => {
                    p(data.len());
                    Ok(data.clone())
                }
            }
        };
        let (data, src) = fetch_from(&Source::ALL, serve(good.clone()), verify, |_, _| {})
            .expect("zrcadlo projde");
        assert_eq!((src, data.len()), (Source::Mirror, SETUP_SIZE));
        let e = fetch_from(&Source::ALL, serve(bad), verify, |_, _| {}).unwrap_err();
        match e {
            DownloadError::NotOfficial(d) => {
                assert!(
                    d.contains("raw.githubusercontent.com: otisk SHA-256 nesedí"),
                    "{d}"
                );
                assert!(d.contains("github.com: soubor na serveru není"), "{d}");
            }
            e => panic!("{e:?}"),
        }
    }

    /// Falešné „ověření" pro testy záložního zdroje: projde jen `OK`.
    fn fake_verify(d: &[u8]) -> Result<(), String> {
        (d == b"OK")
            .then_some(())
            .ok_or_else(|| "otisk nesedí".into())
    }

    /// Připravená odpověď zdroje: bajty, nebo síťová chyba.
    type Answer = Result<&'static [u8], &'static str>;
    type Fetched = Result<(Vec<u8>, Source), DownloadError>;

    /// Fetcher, který pro každý zdroj vrátí připravenou odpověď a zapíše,
    /// na co se ptal.
    fn run(official: Answer, mirror: Answer) -> (Fetched, Vec<Source>) {
        let mut asked = Vec::new();
        let mut seen = Vec::new();
        let r = fetch_from(
            &Source::ALL,
            |src, p| {
                asked.push(src);
                let r = match src {
                    Source::Official => official,
                    Source::Mirror => mirror,
                };
                r.map(|d| {
                    p(d.len());
                    d.to_vec()
                })
                .map_err(str::to_string)
            },
            fake_verify,
            |src, n| seen.push((src, n)),
        );
        // Průběh hlásí vždy zdroj, ze kterého se právě stahuje.
        assert!(seen.iter().all(|(s, _)| asked.contains(s)));
        (r, asked)
    }

    #[test]
    fn zrcadlo_je_jen_zaloha() {
        // Originál v pořádku → na zrcadlo se vůbec nesahá.
        let (r, asked) = run(Ok(b"OK"), Ok(b"OK"));
        assert_eq!(r.unwrap(), (b"OK".to_vec(), Source::Official));
        assert_eq!(asked, [Source::Official]);
        // Originál nedostupný → zrcadlo.
        let (r, asked) = run(Err("404"), Ok(b"OK"));
        assert_eq!(r.unwrap().1, Source::Mirror);
        assert_eq!(asked, Source::ALL);
        // Originál dodal něco jiného → zahodí se, zrcadlo ověřené projde.
        let (r, _) = run(Ok(b"jiny soubor"), Ok(b"OK"));
        assert_eq!(r.unwrap(), (b"OK".to_vec(), Source::Mirror));
    }

    #[test]
    fn spatny_soubor_z_kterehokoli_zdroje_neprojde() {
        // Oba nedostupné → síť (má smysl zkusit znovu).
        let (r, _) = run(Err("timeout"), Err("404"));
        match r.unwrap_err() {
            DownloadError::Network(e) => {
                assert!(e.contains("github.com: timeout"), "{e}");
                assert!(e.contains("raw.githubusercontent.com: 404"), "{e}");
            }
            e => panic!("{e:?}"),
        }
        // Aspoň jeden dodal jiný soubor → není oficiální (žádné bajty ven).
        for (o, m) in [
            (Ok(&b"x"[..]), Err("404")),
            (Err("timeout"), Ok(&b"x"[..])),
            (Ok(&b"x"[..]), Ok(&b"y"[..])),
        ] {
            let (r, asked) = run(o, m);
            assert!(matches!(r, Err(DownloadError::NotOfficial(_))), "{r:?}");
            assert_eq!(asked, Source::ALL);
        }
    }

    fn well_formed_guid(s: &str) -> bool {
        s.len() == 38
            && s.starts_with('{')
            && s.ends_with('}')
            && s[1..37].char_indices().all(|(i, c)| {
                if [8, 13, 18, 23].contains(&i) {
                    c == '-'
                } else {
                    c.is_ascii_hexdigit() && !c.is_ascii_lowercase()
                }
            })
    }

    #[test]
    fn guidy_maji_spravny_tvar() {
        assert!(well_formed_guid(PRODUCT_CODE));
        assert!(well_formed_guid(UPGRADE_CODE));
        assert!(!well_formed_guid("{966606F3-2745-49E9-BF15-5C3EAA4E907}"));
        // GUID rozhraní v textové podobě (jak ho píše dokumentace ViGEm).
        assert_eq!(
            format!("{{{INTERFACE_GUID:?}}}"),
            "{96E42B22-F5E9-42F8-B043-ED0F932F014F}"
        );
    }

    #[test]
    fn spatna_velikost_ani_obsah_neprojdou() {
        assert!(verify(b"MZ").unwrap_err().contains("místo 6278576 B"));
        let fake = vec![0u8; SETUP_SIZE];
        assert!(verify(&fake).unwrap_err().contains("otisk SHA-256 nesedí"));
        assert!(verify_digest(&SETUP_SHA256).is_ok());
    }

    #[test]
    fn jmeno_v_aplikacich() {
        assert!(is_bus_name("ViGEm Bus Driver"));
        assert!(is_bus_name("Nefarius ViGEm Bus Driver 1.16"));
        assert!(!is_bus_name("HidHide"));
        assert!(is_bus_hwid(r"NEFARIUS\VIGEMBUS\GEN1"));
        assert!(!is_bus_hwid(r"Nefarius\ViGEmBus\Gen2"));
    }

    #[test]
    fn seznam_s_nulami() {
        let w: Vec<u16> = "ROOT\\SYSTEM\\0000\0ROOT\\SYSTEM\\0002\0\0zbytek"
            .encode_utf16()
            .collect();
        assert_eq!(
            split_multi_sz(&w),
            vec![r"ROOT\SYSTEM\0000".to_string(), r"ROOT\SYSTEM\0002".into()]
        );
        assert!(split_multi_sz(&[0, 0]).is_empty());
        assert!(split_multi_sz(&[]).is_empty());
    }

    fn dev(problem: Option<u32>, need_restart: bool) -> Option<DeviceStatus> {
        Some(DeviceStatus {
            problem,
            need_restart,
            started: false,
        })
    }

    #[test]
    fn rady_podle_stavu_zarizeni() {
        for apps in [true, false] {
            assert_eq!(advice_for(None, apps), Advice::Reinstall { in_apps: apps });
            assert_eq!(advice_for(dev(Some(22), true), apps), Advice::EnableDevice);
            assert_eq!(advice_for(dev(Some(48), false), apps), Advice::Blocked(48));
            assert_eq!(advice_for(dev(Some(52), false), apps), Advice::Blocked(52));
            assert_eq!(advice_for(dev(Some(39), false), apps), Advice::Blocked(39));
            assert_eq!(advice_for(dev(Some(14), false), apps), Advice::Restart);
            assert_eq!(
                advice_for(dev(Some(10), false), apps),
                Advice::Problem {
                    code: 10,
                    in_apps: apps
                }
            );
            assert_eq!(advice_for(dev(Some(10), true), apps), Advice::Restart);
            assert_eq!(advice_for(dev(None, false), apps), Advice::Restart);
        }
        assert_eq!(BusState::Ready.advice(), None);
        assert_eq!(BusState::NotInstalled.advice(), None);
        assert_eq!(
            BusState::InstalledNotRunning {
                device: None,
                in_apps: false
            }
            .advice(),
            Some(Advice::Reinstall { in_apps: false })
        );
        assert!(Advice::Blocked(48).text().contains("Integritu paměti"));
        assert!(Advice::EnableDevice.text().contains("Správci zařízení"));
        // Kódy problémů uživateli nic neřeknou — do rady nepatří.
        for a in [
            Advice::Blocked(48),
            Advice::Problem {
                code: 10,
                in_apps: true,
            },
            Advice::Problem {
                code: 10,
                in_apps: false,
            },
        ] {
            assert!(!a.text().contains("kód"), "{a:?}");
            assert!(
                !a.text().contains("10") && !a.text().contains("48"),
                "{a:?}"
            );
        }
    }

    /// Do Aplikací rada posílá jen tam, kde záznam je; jinak restart,
    /// Správce zařízení a ruční instalace (KeyPadSetup ovladač se zbytky
    /// v systému nepřeinstaluje).
    #[test]
    fn rada_bez_zaznamu_v_aplikacich_tam_neposila() {
        let with = [
            Advice::Reinstall { in_apps: true },
            Advice::Problem {
                code: 10,
                in_apps: true,
            },
        ];
        for a in with {
            assert!(a.text().contains("Aplikace"), "{a:?}");
            assert!(a.text().contains("spusť KeyPadSetup znovu"), "{a:?}");
            assert!(!a.suggests_manual_install());
        }
        // Zařízení sběrnice chybí: restart ho nevytvoří (zakládá ho
        // instalátor), takže rada restart ani „pak naběhne" nenabízí.
        let t = Advice::Reinstall { in_apps: true }.text();
        assert!(!t.contains("Restartuj") && !t.contains("naběhne"), "{t}");
        let without = [
            Advice::Reinstall { in_apps: false },
            Advice::Problem {
                code: 10,
                in_apps: false,
            },
        ];
        for a in without {
            let t = a.text();
            assert!(!t.contains("Nastavení → Aplikace"), "{t}");
            assert!(!t.contains("spusť KeyPadSetup znovu"), "{t}");
            assert!(t.contains("Restartuj"), "{t}");
            assert!(t.contains(RELEASES_URL), "{t}");
            assert!(a.suggests_manual_install());
        }
    }

    #[test]
    fn verze_ovladace_se_cte_i_porovnava() {
        assert_eq!(parse_version("1.21.442.0"), Some([1, 21, 442, 0]));
        assert_eq!(parse_version("1.17"), Some([1, 17, 0, 0]));
        assert_eq!(parse_version("1.2.3.4.5"), None);
        assert_eq!(parse_version("x.1"), None);
        assert!(parse_version("1.17.333.0").unwrap() < DRIVER_VERSION);
        assert!(parse_version("1.21.442.0").unwrap() >= DRIVER_VERSION);
    }

    /// Na vývojovém PC je ViGEmBus nainstalovaný a běží — detekce to
    /// musí poznat (jen čte). Na PC bez ovladače by selhal, proto
    /// `#[ignore]`: `cargo test -p updater -- --ignored stav_na_tomto_pc`.
    #[test]
    #[ignore = "závisí na stroji: čeká nainstalovaný a běžící ViGEmBus"]
    fn stav_na_tomto_pc_je_ready() {
        assert_eq!(state(), BusState::Ready);
        // Ovladač 1.21.442 z vydání 1.22.0 — aktualizace se nenabízí.
        assert_eq!(driver_version(), Some(DRIVER_VERSION));
        assert!(!needs_update());
        // Když běží, musí ho najít i záložní cesty (služba, Aplikace,
        // zařízení podle hardware ID) — ty se jinak ověřit nedají.
        assert!(service_key_exists());
        assert!(arp_entry_exists());
        let d = device_status().expect("zařízení sběrnice");
        assert_eq!(d.problem, None);
        assert!(d.started);
        let by_hwid = root_system_devices().into_iter().any(|id| {
            locate(&id)
                .map(|dn| hardware_ids(dn).iter().any(|h| is_bus_hwid(h)))
                .unwrap_or(false)
        });
        assert!(by_hwid, "zařízení podle hardware ID");
    }

    /// Skutečné stažení z GitHubu + ověření (síť; nic se nespouští ani
    /// neukládá): `cargo test -p updater -- --ignored stazeni`.
    #[test]
    #[ignore = "stahuje 6 MB z GitHubu"]
    fn stazeni_a_overeni_oficialniho_instalatoru() {
        let mut last = 0;
        let (data, src) = fetch_setup(|_, n| last = n).expect("stažení");
        assert_eq!(src, Source::Official);
        assert_eq!(data.len(), SETUP_SIZE);
        assert_eq!(last, SETUP_SIZE);
        assert!(data.starts_with(b"MZ"));
    }

    /// Skutečné stažení ze zrcadla (až bude `mirror/` na GitHubu):
    /// `cargo test -p updater -- --ignored stazeni_ze_zrcadla`.
    #[test]
    #[ignore = "stahuje 6 MB z GitHubu; zrcadlo musí být pushnuté"]
    fn stazeni_ze_zrcadla() {
        let (data, src) = fetch_from(
            &[Source::Mirror],
            |s, p| http::get_limited(s.host(), s.path(), SETUP_SIZE, p).map_err(|e| e.to_string()),
            verify,
            |_, _| {},
        )
        .expect("zrcadlo");
        assert_eq!((data.len(), src), (SETUP_SIZE, Source::Mirror));
    }
}

//! Hook klávesnice (Fáze 3): `WH_KEYBOARD_LL` na vlastním vlákně.
//!
//! Vlákno vlastní [`Engine`] (callback hooku nemá kontext, proto
//! `thread_local!`) a točí smyčku `GetMessageW` — Windows volají
//! callback právě z ní. Callback jen přeloží událost na [`KeyId`],
//! zeptá se enginu a výsledek předá [`Vystup`]u. Nic víc: žádné I/O,
//! logování, alokace ani zámek sdílený s jiným vláknem (princip 3).
//! Windows jinak po `LowLevelHooksTimeout` hook potichu odeberou
//! a klávesy by šly do hry, i když má ovladač běžet.
//!
//! Co callback smí (Fáze 6, spec 2.3): zápis do slotů padů a do atomiků
//! [`Vystup`]u (režim, oznámení, revize mapování, živý stav), přičtení do
//! statických atomiků doručení ([`Doruceni`]) a při konci přiřazování
//! v callbacku (Esc, uložení) i jejich čtení, `SetEvent`
//! a `PostThreadMessageW` jen po panice do vlastní fronty. Nesmí kanál,
//! `Mutex`, alokaci ani uvolnění, `log::`, `emit`, klon mapování — snímek
//! mapování pro okno klonuje jen smyčka na povel [`HookPrikaz::Zverejni`]
//! — a od 6. 10. ani dotaz na stav klávesnice (`GetAsyncKeyState`): stav
//! OS se mění až po hooku, takže o klávese, o které callback rozhoduje,
//! nic neřekne, a právě dotazy dělaly ocas ceny callbacku (Fáze 6b).
//! Klávesy, které Windows drží bez vědomí enginu, zjišťuje jednorázový
//! snímek ve smyčce ([`snimek_klavesnice`]); na začátku přiřazování (okno
//! KeyPadu ověřeně v popředí) navíc zapomene klávesy OS a bit Win, které
//! Windows už nedrží — jejich key-up hook neviděl (okno s právy správce,
//! OQ 39) a přiřazování by jinak nevzalo nic, ani Esc.
//!
//! Že přiřazování ve vydáních …1208 a …1504 nevzalo nic, nejspíš
//! nezpůsoboval dotaz na stav klávesnice (výklad OQ 57), ale Raw Input:
//! tao si zaregistroval klávesnici a s oknem vlastního procesu v popředí
//! Windows LL hook téhož procesu podle cizích nálezů nevolají (OQ 60,
//! [`super::raw_input`]). Přímo neověřeno — log …1504 („nepřiřazeno:
//! nic") je silná nepřímá stopa, ne důkaz: některé stisky, souběh ani
//! rozbitý engine se tehdy nepočítaly. Proto začátek přiřazování
//! registraci ověří a řádek o konci přiřazování nese i počty doručení —
//! kolikrát Windows callback vůbec zavolaly; potvrdí vlastník (Fáze 6c).
//!
//! Všechno ostatní (příkazy z okna, časovač přiřazování, snímek
//! klávesnice, kontrola Raw Input, hlášení paniky a konce přiřazování do
//! logu, popředí okna) dělá táž smyčka mimo callback.
//!
//! Vlákno s enginem běží celou dobu, samotný hook je ale v systému JEN
//! tehdy, když ho engine potřebuje ([`potreba_hooku`]): je zapnutý aspoň
//! jeden ovladač (nebo se přiřazuje klávesa), nebo je okno KeyPadu
//! v popředí (živé klávesy v okně, Fáze 6). Jinak KeyPad na klávesnici
//! vůbec nesahá (princip 10) a nic nemůže zdržet psaní v jiných
//! programech.

use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use keypad_core::{
    BindKind, BindingCancel, BindingReject, Decision, DisabledReason, Engine, ForceReason, HeldKey,
    KeyId, LiveInputs, Mapping, MappingError, Mode, Owner, PadAction, PadId, PadState, PadUpdates,
    UiEvent, MAX_PADS,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyboardLayout, GetKeyboardLayoutList, MapVirtualKeyExW, HKL,
    MAPVK_VK_TO_VSC_EX,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetAncestor, GetForegroundWindow, GetMessageW,
    GetWindowThreadProcessId, KillTimer, PeekMessageW, PostThreadMessageW, SetTimer,
    SetWindowsHookExW, UnhookWindowsHookEx, EVENT_SYSTEM_DESKTOPSWITCH, EVENT_SYSTEM_FOREGROUND,
    GA_ROOT, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, MSG, PM_NOREMOVE,
    WH_KEYBOARD_LL, WINEVENT_OUTOFCONTEXT, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WM_TIMER,
};

// `super::`, ne `crate::platform::windows::` — soubor sdílí i příklad
// `hook_selftest` (`#[path]`), kde moduly leží přímo v kořeni.
use super::raw_input::RawInput;

/// Ve frontě kanálu čekají příkazy (probuzení smyčky).
const WM_PRIKAZ: u32 = WM_APP + 1;
/// Callback spadl do paniky — smyčka to zaloguje (callback logovat nesmí).
const WM_PANIKA: u32 = WM_APP + 2;
/// Okno KeyPadu získalo nebo ztratilo popředí (callback WinEventu): ať
/// smyčka projde hlídáním hooku. Callback WinEventu běží uvnitř
/// `GetMessageW`, takže sám smyčku neotočí.
const WM_POPREDI: u32 = WM_APP + 3;
/// Plocha se přepnula a držené klávesy se zapomněly (callback WinEventu):
/// ať smyčka udělá snímek klávesnice dřív, než přijde další klávesa.
const WM_SNIMEK: u32 = WM_APP + 4;

/// Krok časovače při přiřazování klávesy. Timeout přiřazování je 10 s;
/// o čtvrt vteřiny později je pořád „po deseti vteřinách". Časovač běží
/// jen během přiřazování — nečinný hook nikdy netiká (princip 10).
const TIK_MS: u32 = 250;

/// Levá a pravá Win jako virtuální klávesy (`Udalost::vk`). Vlastní
/// konstanty, ne `VIRTUAL_KEY` z `windows`: soubor sdílí i příklad
/// `hook_selftest` (`#[path]`) a snímek se ptá přes podvrhnutelné
/// `os_drzi`.
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;
/// Bity levé a pravé Win v `Stav::win`.
const WIN_L: u8 = 1;
const WIN_R: u8 = 2;
/// Jak dlouho po poslední události Win (stisk, autorepeat, uvolnění) platí
/// její bit. Uvolnění Win callback minout může i bez přepnutí plochy
/// (okno s právy správce v popředí, OQ 39, 44) — bez limitu by pak každý
/// stisk patřil Windows, dokud uživatel Win znovu nestiskne, i ve hře
/// a při přiřazování. Win+klávesa je krátká kombinace a samotná držená Win
/// se opakuje nejpozději po 1 s; pět vteřin pokryje i Win+Shift+S nebo
/// několik Win+šipek za sebou.
const WIN_PLATNOST_MS: u64 = 5_000;
/// Levý Ctrl, pravý Alt, pravý Shift a Print Screen jako virtuální
/// klávesy snímku klávesnice (`os_drzi` je podvrhnutelné, proto čísla).
const VK_LCONTROL: u32 = 0xA2;
const VK_RMENU: u32 = 0xA5;
const VK_RSHIFT: u32 = 0xA1;
const VK_SNAPSHOT: u32 = 0x2C;

/// Rozsah virtuálních kláves, které snímek klávesnice prochází. Pod
/// 0x08 jsou tlačítka myši (a Ctrl+Break), 0xFF žádná klávesa není.
const SNIMEK_VK: std::ops::RangeInclusive<u32> = 0x08..=0xFE;
/// Obecné Shift, Ctrl a Alt (VK_SHIFT, VK_CONTROL, VK_MENU): Windows je
/// hlásí dole při levé i pravé variantě, ale převod na scan kód dá vždy
/// levou. Levou a pravou zvlášť (0xA0–0xA5) snímek projde sám.
const SNIMEK_BEZ: [u32; 3] = [0x10, 0x11, 0x12];
/// Virtuální klávesy samostatných kláves s prefixem E0, u kterých
/// `MapVirtualKeyExW` prefix nevrátí (naměřeno na Windows 10 19045:
/// VK_UP dá 0x48 bez E0) — sdílí scan kód s numerickou klávesnicí:
/// PageUp, PageDown, End, Home, šipky, Insert, Delete, nabídka, dělení
/// na numerické klávesnici a NumLock. S vypnutým NumLockem posílá
/// numerická klávesnice tytéž VK bez E0 — snímek je od sebe nerozliší
/// (OQ 38) a bere samostatnou klávesu: šipky jsou ve výchozím mapování.
///
/// Pravý Shift: převod dá 0x36 bez E0, rozložení (kbdcz, kbdus) ho ale
/// vede s příznakem KBDEXT stejně jako NumLock, a ten LL hook hlásí s E0
/// (naměřeno v tabulce rozložení, hlášení hooku neověřeno — OQ 59).
/// Převzetí bez E0 by dalo modifikátor, jehož key-up nikdy nepřijde.
const VZDY_E0: [u32; 14] = [
    0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x2D, 0x2E, 0x5D, 0x6F, 0x90, VK_RSHIFT,
];
/// VK_PAUSE: převod dá E1 1D, LL hook ale Pause hlásí jako scan 0x45 bez
/// E0 — tak ji zná engine.
const VK_PAUSE: u32 = 0x13;

/// Watchdog (Fáze 5): ovladač, jehož pad vlákno déle nemluvilo s ViGEm,
/// je zaseknutý. Keep-alive chodí každých 200 ms — 1 s je pět
/// vynechaných a pořád dost pod tím, co by hráč považoval za „visí".
pub const WATCHDOG_MS: u64 = 1_000;

/// Jak dlouho se čeká na nainstalování a na konec vlákna. Obojí trvá
/// milisekundy; limit jen hlídá, aby okno nikdy nečekalo navždy.
const LIMIT: Duration = Duration::from_secs(3);

/// Jedna událost klávesnice, jak ji hook viděl.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Udalost {
    /// Klávesa pro engine. Scan kód, který se do `u16` nevejde, je tu
    /// `0` — nemapovatelný, engine ho propustí a nesleduje.
    pub klavesa: KeyId,
    /// Syrový `scanCode` (AltGr na CZ rozložení = falešný LCtrl 0x21D).
    pub scan: u32,
    pub vk: u32,
    /// `LLKHF_*`.
    pub flags: u32,
    pub dolu: bool,
}

impl Udalost {
    fn z(kb: &KBDLLHOOKSTRUCT, dolu: bool) -> Udalost {
        let klavesa = KeyId {
            // Useknout by bylo špatně: 0x1_0011 by se tvářil jako W.
            scan: u16::try_from(kb.scanCode).unwrap_or(0),
            extended: kb.flags.0 & LLKHF_EXTENDED.0 != 0,
        };
        Udalost {
            klavesa,
            scan: kb.scanCode,
            vk: kb.vkCode,
            flags: kb.flags.0,
            dolu,
        }
    }

    /// Událost poslal program (`SendInput`), ne klávesnice.
    ///
    /// Takové hook vždy propouští a engine se o nich nedozví: nejsou to
    /// stisky uživatele (AutoHotkey, klávesnice na obrazovce, makra)
    /// a kdyby je engine sledoval, vstříknutý key-up by mu „pustil"
    /// klávesu, kterou uživatel pořád drží.
    pub fn vstrcena(&self) -> bool {
        self.flags & LLKHF_INJECTED.0 != 0
    }
}

/// Kam hook předává rozhodnutí enginu.
///
/// **Volá se i z callbacku hooku** — implementace nesmí blokovat,
/// alokovat, logovat ani brát zámek sdílený s jiným vláknem: jen
/// atomiky a události Windows (princip 3). Výjimkou je jen
/// [`Vystup::mapovani`], které volá výhradně smyčka.
pub trait Vystup: Send + Sync {
    /// `udalost` je `None` u příkazů, přeinstalace, paniky a konce hooku.
    /// Vstříknuté klávesy přicházejí s `Decision::NONE` — engine je
    /// nevidí.
    fn rozhodnuti(&self, udalost: Option<&Udalost>, d: &Decision, rezim: Mode);

    /// Hook nejde dostat do systému (`true`), nebo zase jde (`false`).
    /// Volá jen smyčka, nikdy callback. Okno pak nesmí tvrdit, že
    /// klávesy ovládají ovladač (princip 8).
    fn hook_chyba(&self, _chyba: bool) {}

    /// Tep pad vlákna ovladače (`GetTickCount64`, ms) pro watchdog;
    /// `None` = výstup ovladače nezná (testy, příklad). Volá se
    /// z callbacku — jen čtení atomiku.
    fn tep_ms(&self, _pad: PadId) -> Option<u64> {
        None
    }

    /// Živý stav vstupů pro okno — jen s viditelným oknem. Callback
    /// i smyčka: jen atomiky.
    fn zive(&self, _z: &[LiveInputs; MAX_PADS]) {}

    /// Revize mapování enginu. Callback i smyčka: jen atomik.
    fn revize(&self, _rev: u64) {}

    /// Snímek mapování pro okno. JEN smyčka (povel `Zverejni`) — klonuje.
    fn mapovani(&self, _rev: u64, _m: &Mapping) {}
}

/// Příkazy hook vláknu. Každý vede na volání enginu a jeho rozhodnutí
/// jde do [`Vystup`]u stejně jako rozhodnutí o klávese.
#[derive(Debug)]
pub enum HookPrikaz {
    /// Ovladač se připojil — engine ho povolí (z `Disabled` na
    /// Klávesnici, nikdy sám na Gamepad).
    Povol(PadId),
    /// Ovladač není — jeho klávesy jdou zase do Windows; poslední
    /// vypnutý → `Disabled` (a hook ze systému zmizí, není-li okno
    /// v popředí).
    Zakaz(PadId, DisabledReason),
    /// Přepnout Klávesnice ↔ Gamepad (nabídka ikony).
    Prepni,
    /// Uživatel zapnul ovladač přepínačem: zachytávat. Z pozastavení se
    /// hook předtím nainstaluje znovu — Windows ho mohli potichu odebrat
    /// a zachytávání by jinak jen předstíralo, že běží.
    Zachytavej,
    /// Vynutit Klávesnici (fail-safe).
    Vynut(ForceReason),
    /// Vynutit Klávesnici a zapomenout držené klávesy — key-upy se
    /// ztratily (zamčení relace, odpojení relace).
    Zapomen(ForceReason),
    /// Odhooknout a nainstalovat znovu (Windows mohli hook potichu
    /// odebrat). Držené klávesy se zapomenou.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "posílá hook_selftest; aplikace přeinstaluje přes Zachytavej"
        )
    )]
    Preinstaluj,
    /// Hlavní okno je vidět (jeho HWND), nebo je schované či
    /// minimalizované (`None`). Jen s viditelným oknem hook počítá živý
    /// stav a hlídá popředí; schování zruší přiřazování.
    Okno(Option<isize>),
    /// Klik na čepičku: přiřazovat klávesu. Přijme se, jen když je okno
    /// V TU CHVÍLI v popředí — klávesu stisknutou jinde (hra, chat) by
    /// přiřazování spolklo.
    Prirad { cil: PadAction, druh: BindKind },
    /// Esc, klik jinam v okně: přiřazování skončí beze změny.
    ZrusPrirazeni,
    /// Úprava mapování z editoru, i za hry (bez pozastavení, OQ 43).
    /// Odpověď jde kanálem `bounded(1)` přes `try_send` — smyčka na okno
    /// nikdy nečeká. Bez odpovědi (rozbitý engine) skončí příkaz okna
    /// chybou kanálu.
    Uprav {
        zmena: Zmena,
        odpoved: Sender<Result<(), ChybaUpravy>>,
    },
    /// Vlákno okna chce snímek mapování (revize je novější než jeho
    /// zrcadlo): `Vystup::mapovani` s aktuální revizí. Klon jen tady,
    /// nikdy v callbacku.
    Zverejni,
    /// Syntetická klávesa pro testy okna na skryté ploše (místo
    /// `SendInput`, který by šel do OS). Jde STEJNOU funkcí jako callback
    /// ([`zpracuj_udalost`]). Jen v debug buildu.
    #[cfg(debug_assertions)]
    TestKlavesa { klavesa: KeyId, vk: u32, dolu: bool },
}

/// Co se má s mapováním udělat ([`HookPrikaz::Uprav`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Zmena {
    /// `×` nebo pravý klik na čepičku: vstup bez kláves.
    VyprazdniVstup(PadAction),
    /// Odebraný ovladač: všechny jeho klávesy pryč.
    VymazOvladac(PadId),
    /// „Výchozí klávesy" ovladače 1 (klávesy jiných ovladačů nebere, OQ 49).
    VychoziPrvni,
    /// „Zpět": vrátit předchozí mapování — jen když je mapování pořád
    /// v revizi `kdyz_revize` (nikdo ho mezitím nezměnil).
    Obnov {
        mapovani: Box<Mapping>,
        kdyz_revize: u64,
    },
}

/// Proč úprava mapování neprošla.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChybaUpravy {
    /// Pravidla mapování (poslední vazba, …).
    Mapovani(MappingError),
    /// Mapování se mezitím změnilo — „Zpět" by vrátilo něco jiného, než
    /// uživatel viděl.
    Zastarale,
}

/// Stav hooku pro ostatní vlákna.
#[derive(Debug, Default)]
pub struct HookStatus {
    nainstalovan: AtomicBool,
    /// Kolikrát se hook nainstaloval (první instalace + přeinstalace).
    instalaci: AtomicU32,
    paniky: AtomicU32,
    /// Po panice selhal i úklid: engine se už nepoužívá, hook je pryč.
    rozbity: AtomicBool,
    /// Poslední pokus o instalaci selhal — další neúspěch už jde do logu
    /// jen jako `debug` (bez zapnutého ovladače se zkouší s každou
    /// zprávou smyčky, dokud je okno v popředí).
    chyba: AtomicBool,
}

#[allow(
    dead_code,
    reason = "paniky a rozbitý hook čte hook_selftest; aplikace ve Fázi 5 (hlídání)"
)]
impl HookStatus {
    pub fn nainstalovan(&self) -> bool {
        self.nainstalovan.load(Ordering::Acquire)
    }

    pub fn instalaci(&self) -> u32 {
        self.instalaci.load(Ordering::Acquire)
    }

    pub fn paniky(&self) -> u32 {
        self.paniky.load(Ordering::Acquire)
    }

    pub fn rozbity(&self) -> bool {
        self.rozbity.load(Ordering::Acquire)
    }
}

/// Odesílatel příkazů hook vláknu — klonuje se (pad vlákna hlásí
/// připojení a odpojení ovladačů přímo). Nikdy ho nepoužívá callback.
#[derive(Clone)]
pub struct HookOdesilatel {
    tid: u32,
    tx: Sender<HookPrikaz>,
}

impl HookOdesilatel {
    /// Pošle příkaz a probudí smyčku. `false` = vlákno už neběží.
    pub fn posli(&self, p: HookPrikaz) -> bool {
        if self.tx.send(p).is_err() {
            return false;
        }
        // SAFETY: jen odeslání zprávy do fronty vlákna; neexistující
        // vlákno = chyba, kterou vrátí volání.
        unsafe { PostThreadMessageW(self.tid, WM_PRIKAZ, WPARAM(0), LPARAM(0)) }.is_ok()
    }
}

/// Běžící hook vlákno. Drop ho zastaví.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "status čtou testy a hook_selftest")
)]
pub struct Hook {
    tx: HookOdesilatel,
    status: Arc<HookStatus>,
    konec: Receiver<()>,
    vlakno: Option<std::thread::JoinHandle<()>>,
}

#[cfg_attr(
    not(test),
    allow(dead_code, reason = "status a posli používají testy a hook_selftest")
)]
impl Hook {
    /// Spustí vlákno s enginem. Samotný hook se do systému dostane až
    /// s prvním povoleným ovladačem ([`HookPrikaz::Povol`]) nebo s oknem
    /// v popředí ([`HookPrikaz::Okno`]).
    ///
    /// Engine startuje jako vždy v `Disabled { PadNotConnected }`.
    pub fn spust(mapovani: Mapping, vystup: Arc<dyn Vystup>) -> Result<Hook, String> {
        spust_s(mapovani, vystup, || Ok(()), je_v_popredi)
    }

    pub fn status(&self) -> &Arc<HookStatus> {
        &self.status
    }

    pub fn odesilatel(&self) -> HookOdesilatel {
        self.tx.clone()
    }

    /// Pošle příkaz a probudí smyčku. `false` = vlákno už neběží.
    pub fn posli(&self, p: HookPrikaz) -> bool {
        self.tx.posli(p)
    }

    /// Ukončí vlákno: engine nejdřív vynutí Klávesnici (neutrální stav
    /// všech ovladačů jde do [`Vystup`]u), pak se hook odebere.
    /// `false` = vlákno neskončilo v limitu (proces ho pak ukončí sám;
    /// hook zmizí s ním).
    pub fn zastav(&mut self) -> bool {
        let Some(vlakno) = self.vlakno.take() else {
            return true;
        };
        // SAFETY: jen odeslání zprávy do fronty vlákna.
        let _ = unsafe { PostThreadMessageW(self.tx.tid, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if self.konec.recv_timeout(LIMIT).is_err() {
            log::warn!("hook vlákno neskončilo do {} s", LIMIT.as_secs());
            return false;
        }
        let _ = vlakno.join();
        true
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        self.zastav();
    }
}

/// Jako [`Hook::spust`], jen vlákno nejdřív zavolá `pred` — testy tak
/// hook instalují na skryté ploše, ne na ploše vlastníka — a popředí
/// okna zjišťuje `aktivni` (testy ho podvrhnou: skrytá plocha popředí
/// nemá).
fn spust_s(
    mapovani: Mapping,
    vystup: Arc<dyn Vystup>,
    pred: impl FnOnce() -> Result<(), String> + Send + 'static,
    aktivni: fn(isize) -> bool,
) -> Result<Hook, String> {
    let status = Arc::new(HookStatus::default());
    let (tx, rx) = crossbeam_channel::unbounded();
    let (hotovo_tx, hotovo_rx) = crossbeam_channel::bounded::<Result<u32, String>>(1);
    let (konec_tx, konec_rx) = crossbeam_channel::bounded::<()>(1);
    let st = Arc::clone(&status);
    let vlakno = std::thread::Builder::new()
        .name("keypad-hook".into())
        .spawn(move || {
            // Panic hook na tomhle vlákně nesmí čekat na zápis logu.
            crate::logger::mark_realtime_thread();
            match pred() {
                Ok(()) => {
                    let nouze = Arc::clone(&vystup);
                    let stav = Arc::clone(&st);
                    let vysledek = catch_unwind(AssertUnwindSafe(|| {
                        let s = Stav::novy(Engine::new(mapovani), vystup, aktivni);
                        vlakno(s, stav, rx, &hotovo_tx)
                    }));
                    if vysledek.is_err() {
                        po_panice_vlakna(&*nouze, &st);
                    }
                }
                Err(e) => {
                    let _ = hotovo_tx.send(Err(e));
                }
            }
            let _ = konec_tx.send(());
        })
        .map_err(|e| format!("hook vlákno nejde spustit: {e}"))?;
    match hotovo_rx.recv_timeout(LIMIT) {
        Ok(Ok(tid)) => Ok(Hook {
            tx: HookOdesilatel { tid, tx },
            status,
            konec: konec_rx,
            vlakno: Some(vlakno),
        }),
        Ok(Err(e)) => {
            let _ = vlakno.join();
            Err(e)
        }
        Err(_) => Err("hook vlákno neodpovědělo".into()),
    }
}

/// Hook vlákno spadlo mimo callback (engine, výstup). Hook zmizí se
/// skončením vlákna (Windows ho odeberou samy), klávesy jdou do OS —
/// ale ovladače by si nechaly poslední stav a pad vlákna by ho dál
/// posílala. Proto všem neutrál a okno se dozví, že nic nezachytává
/// a nic nesvítí.
fn po_panice_vlakna(vystup: &dyn Vystup, status: &HookStatus) {
    status.nainstalovan.store(false, Ordering::Release);
    status.rozbity.store(true, Ordering::Release);
    status.paniky.fetch_add(1, Ordering::AcqRel);
    let mut pads = PadUpdates::NONE;
    for p in PadId::ALL {
        pads.set(p, PadState::NEUTRAL);
    }
    let d = Decision {
        pads,
        ..Decision::NONE
    };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        vystup.rozhodnuti(
            None,
            &d,
            Mode::Disabled {
                reason: DisabledReason::PadError,
            },
        );
        vystup.zive(&[LiveInputs::EMPTY; MAX_PADS]);
        vystup.hook_chyba(true);
    }));
    log::error!("hook vlákno spadlo — ovladače dostaly neutrál, klávesy jdou do Windows");
}

/// Stav, který potřebuje callback. Žije v `thread_local!` hook vlákna:
/// callback nemá kontext a jiné vlákno k enginu nesmí.
struct Stav {
    engine: Engine,
    vystup: Arc<dyn Vystup>,
    /// Po panice selhal i úklid — engine se už nevolá.
    rozbity: bool,
    /// Drží OS klávesu s daným VK? Skutečně [`os_drzi`], v testech
    /// podvrh (skutečný stav klávesnice testy měnit nesmí). Ptá se jen
    /// snímek klávesnice ve smyčce, NIKDY callback.
    os_drzi: fn(u32) -> bool,
    /// Snímek klávesnice čeká: hook se (pře)instaloval nebo engine
    /// zapomněl držené klávesy. Udělá ho smyčka, jakmile je hook
    /// v systému ([`dokonci_snimek`]).
    snimek: bool,
    /// Běžící přiřazování (diagnostika do logu), `None` mimo něj.
    prirazovani: Option<Prirazovani>,
    /// Skončené přiřazování, které ještě nezalogovala smyčka — callback
    /// logovat nesmí. Smyčku po konci v callbacku probudí časovač
    /// přiřazování (běží do další obrátky, [`hlidej_casovac`]).
    konec_prirazovani: Option<KonecPrirazovani>,
    /// Kolik skončených přiřazování se přepsalo dřív, než je smyčka
    /// zalogovala (nemělo by nastat — jen poctivý počet v logu).
    nezalogovano: u16,
    /// Je okno s tímhle HWND v popředí? Skutečně [`je_v_popredi`],
    /// v testech podvrh.
    aktivni: fn(isize) -> bool,
    /// HWND viditelného hlavního okna; `None` = schované nebo
    /// minimalizované. Jen s viditelným oknem se počítá živý stav.
    okno: Option<isize>,
    /// Okno je v popředí (podle posledního `EVENT_SYSTEM_FOREGROUND`).
    /// Jen tehdy jsou vidět i klávesy, které patří Windows (soukromí:
    /// psaní do jiného programu okno vidět nemá), a jen tehdy smí být hook
    /// v systému bez zapnutého ovladače.
    okno_aktivni: bool,
    /// Popředí viditelného okna se hlídá (WinEvent je zaregistrovaný).
    /// Bez toho se okno nikdy nebere jako aktivní a přiřazování se
    /// nepřijme — ztrátu popředí by nic neohlásilo a přiřazování by
    /// spolklo klávesu napsanou v jiném programu.
    sledovano: bool,
    /// Levá a pravá Win ([`WIN_L`], [`WIN_R`]), jak je callback viděl
    /// stisknout a pustit (OQ 44). Win jde vždy do OS, takže tudy projde
    /// každá její událost, i vstříknutá. Windows se callback neptá:
    /// uvolnění, které minul (zabezpečená plocha po Win+L, hook mimo
    /// systém), srovná vynulování a snímek po (pře)instalaci hooku, po
    /// přepnutí plochy a po zapomenutí držených kláves; to, které minul
    /// bez přepnutí plochy (okno s právy správce), omezí [`WIN_PLATNOST_MS`]
    /// a srovnání s Windows na začátku přiřazování a při získání popředí.
    win: u8,
    /// Kdy přišla poslední událost Win nebo kdy bity srovnal snímek
    /// ([`ted_ms`]) — od něj se počítá [`WIN_PLATNOST_MS`].
    win_ms: u64,
    /// Instalace hooku do systému. Skutečně [`nainstaluj`], v testech
    /// podvrh — selhání skutečné instalace se vyvolat nedá.
    instaluj: fn() -> Result<HHOOK, String>,
    /// Kdy engine ovladač povolil. Pad vlákno ohlásí „zapnuto" dřív,
    /// než zkopíruje tep do slotu — watchdog tedy bere novější z obou,
    /// jinak by čerstvě zapnutý ovladač mohl hned vypadat zaseknutý.
    povoleno_ms: [u64; MAX_PADS],
    /// Kontrola Raw Input klávesnice na začátku přiřazování ([`RAW_INPUT`]:
    /// skutečná, v testech podvrh). Jen smyčka, nikdy callback.
    raw_input: fn() -> RawInput,
    /// Výsledek poslední kontroly — do řádku o konci přiřazování.
    raw: RawInput,
}

/// Výchozí kontrola Raw Input: skutečná [`super::raw_input::kontrola`],
/// v testech „ne". Registrace patří celému procesu a test `raw_input` ji
/// mezitím souběžně mění — skutečná kontrola z testů hooku by mu ji rušila.
#[cfg(not(test))]
const RAW_INPUT: fn() -> RawInput = super::raw_input::kontrola;
#[cfg(test)]
const RAW_INPUT: fn() -> RawInput = || RawInput::Ne;

impl Stav {
    fn novy(engine: Engine, vystup: Arc<dyn Vystup>, aktivni: fn(isize) -> bool) -> Stav {
        Stav {
            engine,
            vystup,
            rozbity: false,
            os_drzi,
            snimek: false,
            prirazovani: None,
            konec_prirazovani: None,
            nezalogovano: 0,
            aktivni,
            okno: None,
            okno_aktivni: false,
            sledovano: false,
            win: 0,
            win_ms: 0,
            instaluj: nainstaluj,
            povoleno_ms: [0; MAX_PADS],
            raw_input: RAW_INPUT,
            raw: RawInput::Nezjisteno,
        }
    }
}

/// Drží OS klávesu? Asynchronní stav klávesnice — „dole" znamená, že OS
/// viděl key-down a key-up ještě ne (spolknuté události ho nemění).
/// Nečeká, nezamyká.
///
/// JEN ze snímku klávesnice ve smyčce, nikdy z callbacku: o klávese, o které
/// callback rozhoduje, stav OS nic neřekne (mění se až po hooku) a dotazy
/// dělaly ocas ceny callbacku (OQ 57 — dřívější výklad, že kvůli nim nešlo
/// přiřazovat, je nejspíš mylný; pravděpodobnější příčinou je Raw Input,
/// přímo neověřeno, OQ 60).
fn os_drzi(vk: u32) -> bool {
    let Ok(vk) = i32::try_from(vk) else {
        return false;
    };
    // SAFETY: jen čte asynchronní stav klávesy.
    (unsafe { GetAsyncKeyState(vk) } as u16) & 0x8000 != 0
}

/// Bit Win ([`WIN_L`], [`WIN_R`]) podle virtuální klávesy; `0` = jiná
/// klávesa. Podle VK, ne scan kódu: stav OS se vede po VK a vstříknutá
/// Win (klávesnice na obrazovce) scan kód mít nemusí.
fn bit_win(vk: u32) -> u8 {
    match vk {
        VK_LWIN => WIN_L,
        VK_RWIN => WIN_R,
        _ => 0,
    }
}

/// Engine zapomněl držené klávesy nebo se hook (pře)instaloval: bity Win
/// pryč (jejich uvolnění mohl hook minout) a smyčka udělá snímek
/// klávesnice, který je i s Win srovná s Windows. Jen zápis polí — smí
/// i z callbacku (po panice).
fn zapomen_klavesnici(s: &mut Stav) {
    s.win = 0;
    s.snimek = true;
}

/// Klávesa enginu z virtuální klávesy snímku (`MAPVK_VK_TO_VSC_EX`:
/// prefix E0 v horním bajtu). `None` = klávesa bez scan kódu, s prefixem
/// E1 (Pause) nebo nemapovatelná — engine ji nesleduje.
///
/// Podle rozložení vlákna v popředí: tím Windows z pozice klávesy
/// udělaly virtuální klávesu, kterou snímek vidí (Z a Y na QWERTZ).
fn klavesa_z_vk(vk: u32, hkl: HKL) -> Option<KeyId> {
    if vk == VK_PAUSE {
        return Some(KeyId::new(0x45));
    }
    // Převod dá 0x54 (Alt+SysRq), LL hook ale Print Screen hlásí jako
    // E0 0x37 — převzatá 0x54 by čekala na key-up, který nepřijde.
    if vk == VK_SNAPSHOT {
        return Some(KeyId::ext(0x37));
    }
    // SAFETY: jen převod kódu podle rozložení klávesnice, nic nemění;
    // neplatné rozložení dá 0.
    let sc = unsafe { MapVirtualKeyExW(vk, MAPVK_VK_TO_VSC_EX, Some(hkl)) };
    let extended = match sc >> 8 {
        0 => VZDY_E0.contains(&vk),
        0xE0 => true,
        _ => return None,
    };
    let k = KeyId {
        scan: u16::try_from(sc & 0xFF).ok()?,
        extended,
    };
    k.is_mappable().then_some(k)
}

/// Rozložení klávesnice vlákna, které vlastní okno v popředí (bez popředí
/// rozložení hook vlákna). Jen čte číslo vlákna okna — do cizího procesu
/// se nesahá (princip 8).
fn rozlozeni_popredi() -> HKL {
    // SAFETY: jen čte handle okna v popředí, číslo jeho vlákna
    // a rozložení; neplatné okno dá vlákno 0 = vlastní rozložení.
    unsafe {
        let f = GetForegroundWindow();
        let tid = if f.is_invalid() {
            0
        } else {
            GetWindowThreadProcessId(f, None)
        };
        GetKeyboardLayout(tid)
    }
}

/// Rozložení, podle kterých srovnání zjišťuje, co Windows drží: rozložení
/// okna v popředí a všechna nainstalovaná (nejvýš 15). Klávesu stisknutou
/// v programu s jiným rozložením (hra anglicky, KeyPad česky) by převod
/// jen podle popředí nenašel a srovnání by ji zapomnělo, i když ji Windows
/// drží. Jen čte seznam — nic nemění.
fn rozlozeni_pro_srovnani(popredi: HKL) -> ([HKL; 16], usize) {
    let mut seznam = [HKL::default(); 16];
    seznam[0] = popredi;
    // SAFETY: jen kopie identifikátorů rozložení do pevného pole; víc,
    // než se vejde, Windows nezapíšou.
    let n = unsafe { GetKeyboardLayoutList(Some(&mut seznam[1..])) };
    (seznam, 1 + usize::try_from(n).unwrap_or(0).min(15))
}

/// Výsledek snímku klávesnice (do logu jen počty, nikdy klávesy).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Snimek {
    /// Převzaté klávesy: Windows je drží a engine o nich nevěděl.
    prevzato: usize,
    /// Zapomenuté klávesy OS, které Windows nedrží (jen při srovnání).
    zapomenuto: usize,
    /// Bity Win před snímkem a po něm.
    win_pred: u8,
    win_po: u8,
}

/// Jednorázový snímek klávesnice MIMO callback: klávesy, které Windows
/// drží a engine o nich neví (hook se nainstaloval, když už byly dole;
/// engine je zapomněl po přepnutí plochy, zamčení nebo panice), převezme
/// jako klávesy OS (`adopt_os_key`) — jejich autorepeat i key-up pak jdou
/// do Windows a nic nevisí (OQ 8). Podle snímku nastaví i bity Win.
///
/// `srovnat` (jen začátek přiřazování, okno KeyPadu ověřeně v popředí):
/// navíc zapomene klávesy OS, které Windows už nedrží (`forget_os_key`) —
/// jejich key-up hook neviděl (okno s právy správce, OQ 39), nebo je
/// snímek převzal pod jinou identitou, než s jakou přišel key-up (OQ 38).
/// Záznam OS sám nezastará a zastaralý modifikátor by přiřazování
/// zablokoval i s Esc. Srovnává se jen scan kód bez ohledu na E0
/// a podle všech rozložení: klávesu, kterou Windows drží, zapomenout
/// nesmí (její autorepeat by byl nový stisk a key-up by se spolkl);
/// nechat zastaralou je jen chvilková nepříjemnost.
///
/// Proč tady a ne v callbacku: ve smyčce se zrovna nerozhoduje o žádné
/// klávese — stisk, který mezitím přijde, čeká ve frontě hooku, až se
/// smyčka vrátí do `GetMessageW`, a snímek ho tedy nemůže zahrnout. Kdyby
/// Windows asynchronní stav přece jen přepsaly dřív, než se zeptají
/// hooku, převezme se nanejvýš stisk z těch pár desítek mikrosekund
/// snímku a jde do Windows — bezpečná strana, nic nevisí.
///
/// Jestli asynchronní stav s oknem cizího procesu v popředí vrací nuly,
/// nevíme (OQ 58 — dřívější předpoklad stál na mylném výkladu OQ 57).
/// Kdyby ano, snímek nic nepřevezme a klávesa držená přes zapomenutí je
/// pro engine nový stisk. Na zabezpečené ploše nuly vrací; po návratu
/// z ní ale přijde další přepnutí plochy a s ním nový snímek. Proto se
/// zapomíná jen při srovnání s oknem KeyPadu v popředí.
fn snimek_klavesnice(srovnat: bool) -> Option<Snimek> {
    STAV.with(|s| {
        let mut g = s.borrow_mut();
        let s = g.as_mut().filter(|s| !s.rozbity)?;
        let mut v = Snimek {
            win_pred: s.win,
            ..Snimek::default()
        };
        s.snimek = false;
        s.win = 0;
        let hkl = rozlozeni_popredi();
        // Bez srovnání se držené scan kódy nezjišťují (žádné rozložení).
        let (rozlozeni, n_rozlozeni) = if srovnat {
            rozlozeni_pro_srovnani(hkl)
        } else {
            ([hkl; 16], 0)
        };
        let ted = ted_ms();
        // AltGr: k pravému Altu drží Windows i falešný levý Ctrl, jehož
        // key-up hook hlásí jako nemapovatelný 0x21D. Převzatý jako levý
        // Ctrl by ho nikdo nepustil a modifikátor by blokoval přiřazování.
        // Skutečný levý Ctrl držený s AltGr se tak nepřevezme — jeho key-up
        // bez záznamu jde Windows tak jako tak.
        let altgr = (s.os_drzi)(VK_RMENU);
        // Scan kódy, které Windows drží (bez ohledu na E0) — jen srovnání.
        let mut drzene = [false; 0x80];
        for vk in SNIMEK_VK {
            if SNIMEK_BEZ.contains(&vk) || !(s.os_drzi)(vk) {
                continue;
            }
            // Win engine nesleduje (patří Windows) — jen bit pravidla
            // Win+klávesa.
            let win = bit_win(vk);
            if win != 0 {
                s.win |= win;
                continue;
            }
            for &l in &rozlozeni[..n_rozlozeni] {
                if let Some(k) = klavesa_z_vk(vk, l) {
                    drzene[usize::from(k.scan & 0x7F)] = true;
                }
            }
            if vk == VK_LCONTROL && altgr {
                continue;
            }
            if let Some(k) = klavesa_z_vk(vk, hkl) {
                if s.engine.adopt_os_key(k, ted) {
                    v.prevzato += 1;
                }
            }
        }
        s.win_ms = ted;
        if srovnat {
            for scan in 1..0x80u16 {
                if drzene[usize::from(scan)] {
                    continue;
                }
                for k in [KeyId::new(scan), KeyId::ext(scan)] {
                    if s.engine.forget_os_key(k) {
                        v.zapomenuto += 1;
                    }
                }
            }
        }
        v.win_po = s.win;
        if v.prevzato + v.zapomenuto > 0 {
            // Klávesy Windows svítí v okně v popředí (živý stav).
            predej(s, None, &Decision::NONE);
        }
        Some(v)
    })
}

/// Bity Win podle Windows — okno KeyPadu právě získalo popředí, takže
/// stavu věřit lze (s cizím oknem v popředí to nevíme, OQ 58). Uvolnění
/// Win, které callback minul (okno s právy správce), se tak srovná
/// nejpozději návratem do okna. Jen smyčka, nikdy callback (OQ 57).
fn srovnej_win(s: &mut Stav) {
    s.win = 0;
    for vk in [VK_LWIN, VK_RWIN] {
        if (s.os_drzi)(vk) {
            s.win |= bit_win(vk);
        }
    }
    s.win_ms = ted_ms();
}

/// Srovnání se stavem Windows na začátku přiřazování ([`snimek_klavesnice`]
/// se `srovnat`); do logu jen, když se něco změnilo, a jen počty.
fn srovnej_pred_prirazovanim() {
    let Some(v) = snimek_klavesnice(true) else {
        return;
    };
    let win = |b: u8| if b == 0 { "puštěná" } else { "dole" };
    if v.prevzato + v.zapomenuto > 0 || v.win_pred != v.win_po {
        log::info!(
            "přiřazování: srovnání s Windows — převzato {}, zapomenuto {} kláves, Win {} → {}",
            v.prevzato,
            v.zapomenuto,
            win(v.win_pred),
            win(v.win_po)
        );
    }
}

/// Čekající snímek klávesnice, je-li hook v systému (bez něj by snímek
/// k ničemu nebyl — nainstalování ho stejně vyžádá znovu). Bez zapnutého
/// ovladače je hook v systému jen kvůli oknu v popředí: převzetí při
/// každém Alt+Tab do okna (Alt je ještě dole) jde do logu jen jako
/// `debug`.
fn dokonci_snimek(v_systemu: bool) {
    let (ceka, kvuli_oknu) = STAV.with(|s| {
        s.borrow().as_ref().map_or((false, false), |s| {
            (s.snimek, matches!(s.engine.mode(), Mode::Disabled { .. }))
        })
    });
    if !v_systemu || !ceka {
        return;
    }
    match snimek_klavesnice(false).map(|v| v.prevzato) {
        Some(n) if n > 0 && kvuli_oknu => {
            log::debug!("snímek klávesnice: {n} držených kláves patří Windows");
        }
        Some(n) if n > 0 => log::info!("snímek klávesnice: {n} držených kláves patří Windows"),
        _ => {}
    }
}

/// Podvrh [`os_drzi`]: OS nic nedrží. Testy a syntetické klávesy
/// (`KEYPAD_TEST_KLAVESY`) — skutečná klávesnice vlastníka nesmí
/// rozhodovat o klávese, kterou poslal test. Srovnání při kliku na
/// čepičku pak zapomene i klávesu OS, kterou test drží syntetickou
/// klávesou — scénáře `okno-test.ps1` přes klik žádnou nedrží.
#[cfg(any(test, debug_assertions))]
fn nic_nedrzi(_vk: u32) -> bool {
    false
}

// ── Podvrh „Windows drží všechno" pro test okna (jen debug build) ──
// Test okna běží se syntetickými klávesami a podvrhem „nic nedrží" —
// dotaz callbacku na stav klávesnice by tak prošel bez povšimnutí.
// S tímhle podvrhem by takový callback slyšel „drží" o každé klávese
// a nepřiřadil nic. (Že tohle nepřiřazovalo u vlastníka, je nejspíš
// mylný výklad OQ 57 — pravděpodobnější příčinou je Raw Input, přímo
// neověřeno, OQ 60; pravidlo „callback se neptá" platí dál a podvrh ho
// hlídá.)

#[cfg(debug_assertions)]
thread_local! {
    /// Zpracovává se událost klávesnice ([`zpracuj_udalost`]: callback,
    /// nebo syntetická klávesa testu touž funkcí).
    static V_UDALOSTI: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Kolikrát se událost zeptala podvrhu [`vse_drzi_v_callbacku`] —
    /// zaloguje smyčka ([`nahlas_dotazy_callbacku`]), callback logovat
    /// nesmí. Na vlákně, ne globálně: callback i smyčka běží na hook vlákně.
    static DOTAZY_CALLBACKU: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Po dobu zpracování události platí [`V_UDALOSTI`]; i při panice
/// (drop při rozvinutí zásobníku).
#[cfg(debug_assertions)]
struct VUdalosti;

#[cfg(debug_assertions)]
impl VUdalosti {
    fn zacni() -> VUdalosti {
        V_UDALOSTI.with(|v| v.set(true));
        VUdalosti
    }
}

#[cfg(debug_assertions)]
impl Drop for VUdalosti {
    fn drop(&mut self) {
        V_UDALOSTI.with(|v| v.set(false));
    }
}

/// Podvrh [`os_drzi`] pro `KEYPAD_TEST_OS_DRZI=vse` (s `KEYPAD_TEST_KLAVESY`):
/// zpracování události by Windows tvrdily, že drží všechno — i klávesu,
/// o které se právě rozhoduje. Smyčka (snímek, srovnání při kliku na
/// čepičku, popředí) slyší „nic nedrží" jako u testovacích kláves. Kdyby
/// se callback zase začal ptát, nepřiřadí se nic a Esc nezruší — test
/// okna to pozná i z logu.
#[cfg(debug_assertions)]
fn vse_drzi_v_callbacku(_vk: u32) -> bool {
    let v = V_UDALOSTI.with(std::cell::Cell::get);
    if v {
        DOTAZY_CALLBACKU.with(|d| d.set(d.get().saturating_add(1)));
    }
    v
}

/// Dotazy zpracování události na podvrh do logu (jen smyčka).
#[cfg(debug_assertions)]
fn nahlas_dotazy_callbacku() {
    let n = DOTAZY_CALLBACKU.with(|d| d.replace(0));
    if n > 0 {
        log::error!("KEYPAD_TEST_OS_DRZI: callback se {n}× zeptal na stav klávesnice (OQ 57)");
    }
}

// ── Diagnostika přiřazování (do logu, jen počty) ───────────────────
// Vlastník hlásil, že přiřazování „čeká, ale klávesu nevezme" — a log
// neřekl proč. Teď hook vlákno během přiřazování počítá, proč se stisky
// nepřiřadily, a na konci zapíše jeden řádek. Nikdy identitu klávesy
// (log se posílá při hlášení chyby a mohla by v něm být hesla, OQ 33).

/// Proč se stisk při přiřazování nepřiřadil. Pořadí = pořadí v logu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Neprirazeno {
    /// Modifikátor, nebo stisk s modifikátorem drženým Windows (OQ 55).
    Modifikator,
    /// Win (patří Windows).
    Win,
    /// Klávesa s drženou Win (Win+…, OQ 44).
    SWin,
    /// Bez použitelného scan kódu (AltGr, mediální klávesy).
    Nemapovatelna,
    /// Klávesa, kterou podle enginu drží Windows (převzatá snímkem,
    /// stisknutá před přiřazováním).
    Drzena,
    /// Zkratka pauzy.
    Zkratka,
    /// Poslal ji program, ne klávesnice — hook vstříknuté klávesy
    /// propouští a engine je nevidí.
    Vstrcena,
    /// Klávesa, kterou engine už držel jako klávesu ovladače nebo
    /// spolknutou (stisknutou před přiřazováním, za hry) — její stisk je
    /// pro engine autorepeat.
    UzDrzena,
    /// Nic z toho — kdyby engine stisk odbyl jinak, ať nezmizí bez počtu.
    Jine,
}

impl Neprirazeno {
    const VSE: [Neprirazeno; 9] = [
        Neprirazeno::Modifikator,
        Neprirazeno::Win,
        Neprirazeno::SWin,
        Neprirazeno::Nemapovatelna,
        Neprirazeno::Drzena,
        Neprirazeno::Zkratka,
        Neprirazeno::Vstrcena,
        Neprirazeno::UzDrzena,
        Neprirazeno::Jine,
    ];

    fn text(self) -> &'static str {
        match self {
            Neprirazeno::Modifikator => "modifikátor",
            Neprirazeno::Win => "Win",
            Neprirazeno::SWin => "s Win",
            Neprirazeno::Nemapovatelna => "nemapovatelná",
            Neprirazeno::Drzena => "držená Windows",
            Neprirazeno::Zkratka => "zkratka pauzy",
            Neprirazeno::Vstrcena => "vstříknutá",
            Neprirazeno::UzDrzena => "už držená",
            Neprirazeno::Jine => "jiné",
        }
    }
}

/// Počty nepřiřazených stisků jednoho přiřazování (podle
/// [`Neprirazeno`]). Pevné pole — počítá callback, nesmí alokovat.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Pocty([u16; Neprirazeno::VSE.len()]);

impl Pocty {
    fn pridej(&mut self, n: Neprirazeno) {
        let i = n as usize;
        self.0[i] = self.0[i].saturating_add(1);
    }

    fn pocet(&self, n: Neprirazeno) -> u16 {
        self.0[n as usize]
    }

    /// „2× modifikátor, 1× Win", nebo „nic". Jen smyčka (alokuje).
    fn text(&self) -> String {
        let casti: Vec<String> = Neprirazeno::VSE
            .iter()
            .filter(|&&n| self.pocet(n) > 0)
            .map(|&n| format!("{}× {}", self.pocet(n), n.text()))
            .collect();
        if casti.is_empty() {
            "nic".into()
        } else {
            casti.join(", ")
        }
    }
}

/// Proč přiřazování skončilo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Konec {
    Ulozeno,
    Esc,
    Limit,
    /// Klik jinam, Esc v okně, ztráta popředí, schované okno, klik na
    /// jiný vstup.
    Okno,
    /// Vynucení (pojistka, nepovedený hook, změna mapování…).
    Vynuceno,
}

impl Konec {
    fn text(self) -> &'static str {
        match self {
            Konec::Ulozeno => "uloženo",
            Konec::Esc => "Esc",
            Konec::Limit => "limit 10 s",
            Konec::Okno => "okno",
            Konec::Vynuceno => "vynuceno",
        }
    }

    /// Z oznámení, se kterým přiřazování skončilo. `Decision` nese jen
    /// jedno oznámení (novější vyhrává) — bez oznámení o konci se pozná
    /// aspoň vypršení limitu podle času.
    fn z(ui: Option<UiEvent>, od_ms: u64, ted_ms: u64) -> Konec {
        match ui {
            Some(UiEvent::BindingSaved { .. }) => Konec::Ulozeno,
            Some(UiEvent::BindingCancelled { reason }) => match reason {
                BindingCancel::Escape => Konec::Esc,
                BindingCancel::Timeout => Konec::Limit,
                BindingCancel::Gui => Konec::Okno,
                BindingCancel::Forced(_) | BindingCancel::PadStatus => Konec::Vynuceno,
            },
            _ if ted_ms.saturating_sub(od_ms) >= keypad_core::BINDING_TIMEOUT_MS => Konec::Limit,
            _ => Konec::Vynuceno,
        }
    }
}

// ── Doručení událostí (diagnostika, OQ 60) ─────────────────────────
// Kolikrát Windows callback vůbec zavolaly. Log vlastníka (vydání …1504)
// hlásil po každém přiřazování „nepřiřazeno: nic" a nešlo poznat, jestli
// callback stisky dostal a nějak je odbyl, nebo nedostal vůbec (Raw
// Input, OQ 60). Statické atomiky (Relaxed — jen počty, žádné pořadí):
// callback do nich přičítá, a když přiřazování skončí v něm (Esc,
// uložení — `sleduj_prirazovani` z `predej`), i je čte (`load`). Smyčka je
// čte na začátku přiřazování a u konců, které nastanou v ní (klik jinam,
// limit, vynucení). Obojí jen atomické operace — nic neblokuje
// (princip 3). Nikdy identita klávesy.

/// Každé volání callbacku (i se záporným kódem a jinou zprávou).
static VOLANI: AtomicU32 = AtomicU32::new(0);
/// Z toho stisky (WM_KEYDOWN, WM_SYSKEYDOWN), i autorepeat.
static DOLU: AtomicU32 = AtomicU32::new(0);
/// Událost našla stav enginu půjčený (zanořené volání) — propuštěna.
static SOUBEH: AtomicU32 = AtomicU32::new(0);
/// Událost bez stavu nebo s rozbitým enginem — propuštěna.
static ROZBITY: AtomicU32 = AtomicU32::new(0);

/// Počty doručení: stav čítačů, nebo rozdíl dvou stavů ([`Doruceni::od`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Doruceni {
    volani: u32,
    dolu: u32,
    soubeh: u32,
    rozbity: u32,
}

impl Doruceni {
    /// Teď. Jen čtení atomiků — smí i z callbacku (konec přiřazování).
    fn ted() -> Doruceni {
        Doruceni {
            volani: VOLANI.load(Ordering::Relaxed),
            dolu: DOLU.load(Ordering::Relaxed),
            soubeh: SOUBEH.load(Ordering::Relaxed),
            rozbity: ROZBITY.load(Ordering::Relaxed),
        }
    }

    /// Kolik přibylo od `zacatek` (čítače přetékají dokola).
    fn od(self, zacatek: Doruceni) -> Doruceni {
        Doruceni {
            volani: self.volani.wrapping_sub(zacatek.volani),
            dolu: self.dolu.wrapping_sub(zacatek.dolu),
            soubeh: self.soubeh.wrapping_sub(zacatek.soubeh),
            rozbity: self.rozbity.wrapping_sub(zacatek.rozbity),
        }
    }
}

/// Běžící přiřazování z pohledu diagnostiky.
#[derive(Clone, Copy, Debug)]
struct Prirazovani {
    cil: PadAction,
    od_ms: u64,
    pocty: Pocty,
    /// Poslední stisk (VK, scan, E0), dokud ho nepustí — další stisk
    /// téže klávesy je autorepeat a nepočítá se (Windows opakují jen
    /// naposledy stisknutou klávesu).
    posledni: Option<(u32, u32, bool)>,
    /// Čítače doručení na začátku.
    doruceni: Doruceni,
    /// Kontrola Raw Input při kliku na čepičku.
    raw: RawInput,
}

/// Skončené přiřazování pro log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KonecPrirazovani {
    duvod: Konec,
    pocty: Pocty,
    /// Doručení za dobu přiřazování (rozdíl čítačů).
    doruceni: Doruceni,
    raw: RawInput,
}

/// Sleduje začátek a konec přiřazování podle režimu po každém volání
/// enginu (callback i smyčka). Jen zápis polí — smí z callbacku.
fn sleduj_prirazovani(s: &mut Stav, d: &Decision) {
    let novy = match s.engine.mode() {
        Mode::Binding {
            target,
            started_at_ms,
        } => Some((target, started_at_ms)),
        _ => None,
    };
    // Beze změny (mimo přiřazování, nebo pořád totéž) nic — volá se
    // po každé klávese.
    if s.prirazovani.as_ref().map(|p| (p.cil, p.od_ms)) == novy {
        return;
    }
    if let Some(p) = s.prirazovani.take() {
        // Nové přiřazování místo běžícího = klik na jiný vstup.
        let duvod = if novy.is_some() {
            Konec::Okno
        } else {
            Konec::z(d.ui, p.od_ms, ted_ms())
        };
        if s.konec_prirazovani.is_some() {
            s.nezalogovano = s.nezalogovano.saturating_add(1);
        }
        s.konec_prirazovani = Some(KonecPrirazovani {
            duvod,
            pocty: p.pocty,
            doruceni: Doruceni::ted().od(p.doruceni),
            raw: p.raw,
        });
    }
    let raw = s.raw;
    s.prirazovani = novy.map(|(cil, od_ms)| Prirazovani {
        cil,
        od_ms,
        pocty: Pocty::default(),
        posledni: None,
        doruceni: Doruceni::ted(),
        raw,
    });
}

/// Započte stisk při přiřazování, který přiřazování neukončil (uložením
/// ani zrušením). `pred` = záznam klávesy před událostí, `s_win` = stisk
/// převzatý pravidlem Win+klávesa. Jen zápis polí — volá callback.
fn eviduj_stisk(s: &mut Stav, u: &Udalost, pred: Option<HeldKey>, s_win: bool, d: &Decision) {
    let po = s.engine.held(u.klavesa);
    let Some(p) = s.prirazovani.as_mut() else {
        return;
    };
    let id = (u.vk, u.scan, u.flags & LLKHF_EXTENDED.0 != 0);
    if !u.dolu {
        if p.posledni == Some(id) {
            p.posledni = None;
        }
        return;
    }
    // Týž stisk znovu = autorepeat, nepočítá se.
    if p.posledni.replace(id) == Some(id) {
        return;
    }
    let duvod = match d.ui {
        Some(UiEvent::BindingSaved { .. } | UiEvent::BindingCancelled { .. }) => return,
        Some(UiEvent::BindingRejected { reason, .. }) => match reason {
            BindingReject::Reserved => Neprirazeno::Win,
            BindingReject::Unmappable => Neprirazeno::Nemapovatelna,
            BindingReject::ToggleKey => Neprirazeno::Zkratka,
        },
        _ if u.vstrcena() => Neprirazeno::Vstrcena,
        _ if s_win => Neprirazeno::SWin,
        _ if pred.is_some_and(|h| h.owner == Owner::Os) => Neprirazeno::Drzena,
        _ if pred.is_none() && po.is_some_and(|h| h.owner == Owner::Os) => Neprirazeno::Modifikator,
        // Klávesa ovladače nebo spolknutá držená z dřívějška: engine ji
        // bere jako autorepeat a nic nerozhoduje.
        _ if pred.is_some() => Neprirazeno::UzDrzena,
        _ => Neprirazeno::Jine,
    };
    p.pocty.pridej(duvod);
}

/// Řádek o konci přiřazování (bez „přiřazování skončilo: "): důvod, počty
/// nepřiřazených stisků, doručení a kontrola Raw Input. Jen smyčka
/// (alokuje). Nikdy identita klávesy (OQ 33).
fn radek_konce(k: &KonecPrirazovani, ztraceno: u16) -> String {
    let navic = if ztraceno > 0 {
        format!(" (a {ztraceno} dřívějších bez záznamu)")
    } else {
        String::new()
    };
    let d = k.doruceni;
    format!(
        "{} — nepřiřazeno: {}{navic} · callback {}× (stisků {}, souběh {}, rozbitý {}) · raw input klávesnice: {}",
        k.duvod.text(),
        k.pocty.text(),
        d.volani,
        d.dolu,
        d.soubeh,
        d.rozbity,
        k.raw.text()
    )
}

/// Zaloguje skončené přiřazování (jen smyčka — callback logovat nesmí).
fn zaloguj_prirazovani() {
    let konec = STAV.with(|s| {
        s.borrow_mut().as_mut().and_then(|s| {
            Some((
                s.konec_prirazovani.take()?,
                std::mem::take(&mut s.nezalogovano),
            ))
        })
    });
    if let Some((k, ztraceno)) = konec {
        log::info!("přiřazování skončilo: {}", radek_konce(&k, ztraceno));
    }
}

/// Kontrola Raw Input klávesnice při kliku na čepičku (OQ 60): kdyby ji
/// proces měl, s oknem KeyPadu v popředí by callback podle cizích nálezů
/// nedostal nic. Ve
/// smyčce, nikdy v callbacku (alokuje, volá do jádra Windows). Výsledek
/// nese řádek o konci přiřazování; když nebyl „ne", zapíše se hned.
fn over_raw_input() {
    let Some(kontrola) = STAV.with(|s| s.borrow().as_ref().map(|s| s.raw_input)) else {
        return;
    };
    let r = kontrola();
    STAV.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.raw = r;
        }
    });
    match r {
        RawInput::Ne => {}
        RawInput::NejdeZrusit => log::error!(
            "přiřazování: raw input klávesnice: {} — hook klávesy nejspíš nedostane (OQ 60)",
            r.text()
        ),
        _ => log::warn!("přiřazování: raw input klávesnice: {} (OQ 60)", r.text()),
    }
}

/// Je hlavní okno (`hwnd`) v popředí? Kořen okna v popředí, ne
/// `WindowEvent::Focused` z tao: ten v Tauri skládá fokus WebView2, ne
/// popředí (tauri-runtime-wry 2.11, spec 3 bod 11). Jen porovnání HWND
/// — na cizí proces se nesahá (OQ 39).
fn je_v_popredi(hwnd: isize) -> bool {
    // SAFETY: jen čte handle okna v popředí a jeho kořen; neplatný
    // handle dá neplatný výsledek, nic víc.
    unsafe {
        let f = GetForegroundWindow();
        !f.is_invalid() && GetAncestor(f, GA_ROOT).0 as isize == hwnd
    }
}

/// Podvrh [`je_v_popredi`] pro test okna na skryté ploše
/// (`KEYPAD_TEST_POPREDI`): skrytá plocha popředí nemá, viditelné okno
/// se tam bere jako aktivní.
#[cfg(debug_assertions)]
fn vzdy_aktivni(_hwnd: isize) -> bool {
    true
}

/// Ladicí proměnná prostředí je nastavená na `1`.
#[cfg(debug_assertions)]
fn ladici(jmeno: &str) -> bool {
    std::env::var_os(jmeno).is_some_and(|v| v == "1")
}

/// Test okna na skryté ploše (B6): klávesy posílá test příkazem, ne
/// klávesnice, a skrytá plocha nemá popředí. Jen debug build — release
/// proměnné nečte vůbec.
#[cfg(debug_assertions)]
fn ladici_podvrhy(mut stav: Stav) -> Stav {
    let testovaci = ladici("KEYPAD_TEST_KLAVESY");
    if testovaci {
        log::warn!("KEYPAD_TEST_KLAVESY: hook nečte stav klávesnice (testovací klávesy)");
        stav.os_drzi = nic_nedrzi;
    }
    // Jen s testovacími klávesami: mimo zpracování události má podvrh
    // odpovídat „nic nedrží" — se skutečnou klávesnicí by smyčka
    // neviděla, co uživatel opravdu drží.
    if std::env::var_os("KEYPAD_TEST_OS_DRZI").is_some_and(|v| v == "vse") {
        if testovaci {
            log::warn!("KEYPAD_TEST_OS_DRZI=vse: callbacku by Windows hlásily, že drží všechny klávesy (OQ 57)");
            stav.os_drzi = vse_drzi_v_callbacku;
        } else {
            log::warn!("KEYPAD_TEST_OS_DRZI platí jen s KEYPAD_TEST_KLAVESY=1 — ignoruje se");
        }
    }
    if ladici("KEYPAD_TEST_POPREDI") {
        log::warn!("KEYPAD_TEST_POPREDI: viditelné okno se bere jako okno v popředí");
        stav.aktivni = vzdy_aktivni;
    }
    stav
}

thread_local! {
    static STAV: RefCell<Option<Stav>> = const { RefCell::new(None) };
}

/// Monotónní čas v ms pro engine.
///
/// `GetTickCount64`, ne čas události z `KBDLLHOOKSTRUCT`: ten je jen
/// 32bitový (po 49 dnech přeteče), u vstříknutých událostí si ho volající
/// vymýšlí a engine potřebuje čas, který nikdy necouvne. Zdržení hook
/// vlákna jsou milisekundy — na pravidla s vteřinovými limity (ztracený
/// key-up 1,5 s, přiřazování 10 s) to nemá vliv (otevřená otázka 15).
fn ted_ms() -> u64 {
    // SAFETY: bez parametrů, jen čte čítač.
    unsafe { GetTickCount64() }
}

fn vlakno(
    stav: Stav,
    status: Arc<HookStatus>,
    rx: Receiver<HookPrikaz>,
    hotovo: &Sender<Result<u32, String>>,
) {
    // Fronta zpráv vlákna vzniká až prvním voláním funkce na zprávy —
    // bez ní by PostThreadMessageW z okna selhal.
    let mut msg = MSG::default();
    // SAFETY: platný ukazatel na MSG; jen vytvoří frontu.
    let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
    // SAFETY: bez parametrů.
    let tid = unsafe { GetCurrentThreadId() };

    #[cfg(debug_assertions)]
    let stav = ladici_podvrhy(stav);
    STAV.with(|s| *s.borrow_mut() = Some(stav));
    // Hook zatím ne: bez zapnutého ovladače a okna v popředí ho engine
    // nepotřebuje.
    let mut hook = HHOOK::default();
    let plocha = hlidej_plochu();
    let mut popredi: Option<HWINEVENTHOOK> = None;
    let _ = hotovo.send(Ok(tid));

    let mut casovac = 0usize;
    loop {
        // SAFETY: platný ukazatel na MSG; čeká na zprávu vlákna.
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        // 0 = WM_QUIT, -1 = chyba — obojí konec.
        if r.0 == 0 || r.0 == -1 {
            break;
        }
        match msg.message {
            WM_PRIKAZ => {
                while let Ok(p) = rx.try_recv() {
                    match p {
                        HookPrikaz::Preinstaluj if !hook.is_invalid() => {
                            hook = preinstaluj(hook, &status);
                        }
                        HookPrikaz::Zachytavej => {
                            // Jen z pozastavení: při běžícím zachytávání
                            // (zapnutý druhý ovladač) by přeinstalace
                            // zapomněla klávesy, které hráč 1 právě drží.
                            if rezim() == Some(Mode::Keyboard) {
                                hook = preinstaluj(hook, &status);
                            }
                            // Bez hooku v systému by zachytávání jen
                            // předstíralo, že běží (princip 8).
                            if !hook.is_invalid() {
                                s_enginem(|e| e.capture(ted_ms()));
                            }
                        }
                        HookPrikaz::Povol(pad) => {
                            STAV.with(|s| {
                                if let Some(s) = s.borrow_mut().as_mut() {
                                    s.povoleno_ms[pad.index()] = ted_ms();
                                }
                            });
                            s_enginem(|e| e.enable(pad));
                        }
                        HookPrikaz::Okno(h) => {
                            popredi = hlidej_popredi(popredi, h.is_some());
                            okno(h, popredi.is_some());
                        }
                        p => proved(p),
                    }
                }
            }
            // Popředí se změnilo, nebo čeká snímek po přepnutí plochy —
            // stačí projít hlídáním hooku a snímkem níž.
            WM_POPREDI | WM_SNIMEK => {}
            WM_TIMER if msg.hwnd.is_invalid() => {
                s_enginem(|e| e.tick(ted_ms()));
            }
            WM_PANIKA => {
                let n = status.paniky.fetch_add(1, Ordering::AcqRel) + 1;
                let rozbity = STAV.with(|s| s.borrow().as_ref().is_none_or(|s| s.rozbity));
                if rozbity && !hook.is_invalid() {
                    // Engine se už nevolá, callback by jen propouštěl —
                    // hook tedy nemá smysl nechávat v systému.
                    odhookni(hook);
                    hook = HHOOK::default();
                    status.nainstalovan.store(false, Ordering::Release);
                    status.rozbity.store(true, Ordering::Release);
                    log::error!(
                        "panika v hooku klávesnice (celkem {n}) a selhal i úklid — hook odebrán"
                    );
                } else {
                    log::error!("panika v hooku klávesnice (celkem {n}) — klávesa propuštěna, držené klávesy zapomenuty");
                }
            }
            _ => {
                // SAFETY: zpráva z GetMessageW.
                unsafe { DispatchMessageW(&msg) };
            }
        }
        hook = hlidej_hook(hook, &status);
        // Snímek hned za instalací, ještě před návratem do GetMessageW:
        // dřív, než callback uvidí první klávesu.
        dokonci_snimek(!hook.is_invalid());
        // Konec přiřazování v callbacku: tahle obrátka přijde nejpozději
        // s dalším tikem časovače přiřazování (ten se zruší až níž).
        zaloguj_prirazovani();
        #[cfg(debug_assertions)]
        nahlas_dotazy_callbacku();
        casovac = hlidej_casovac(casovac);
    }

    // Pořadí jako u každého konce: neutrál → odhooknout (→ odpojit pady
    // udělá volající). Po vynucené Klávesnici hook už nic nepotlačí.
    s_enginem(|e| e.force_keyboard(ForceReason::Shutdown));
    zaloguj_prirazovani();
    if !hook.is_invalid() {
        odhookni(hook);
        log::info!("hook klávesnice odebrán (konec)");
    }
    status.nainstalovan.store(false, Ordering::Release);
    if casovac != 0 {
        // SAFETY: časovač vlákna vytvořený v `hlidej_casovac`.
        let _ = unsafe { KillTimer(None, casovac) };
    }
    for h in [plocha, popredi].into_iter().flatten() {
        // SAFETY: handle z SetWinEventHook, odebírá se právě jednou.
        let _ = unsafe { UnhookWinEvent(h) };
    }
    STAV.with(|s| s.borrow_mut().take());
}

/// Přepnutí plochy (Fáze 5): výzva UAC, Ctrl+Alt+Del, zamčení (Win+L)
/// přepnou na zabezpečenou plochu, kde LL hook key-upy nevidí. Klávesa
/// držená přes přepnutí by po návratu zůstala „dole" — ovladač by měl
/// vychýlenou páčku, dokud ji uživatel znovu nestiskne a nepustí.
///
/// Událost `EVENT_SYSTEM_DESKTOPSWITCH` chodí do smyčky tohohle vlákna
/// (mimo kontext, bez DLL v cizích procesech). `None` = hlídání nejde —
/// zůstává ztracený key-up (1,5 s) a zkratka.
fn hlidej_plochu() -> Option<HWINEVENTHOOK> {
    // SAFETY: callback je funkce tohoto modulu a žije po celý běh;
    // WINEVENT_OUTOFCONTEXT = volá se ze smyčky zpráv tohoto vlákna.
    let h = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_DESKTOPSWITCH,
            EVENT_SYSTEM_DESKTOPSWITCH,
            None,
            Some(plocha_se_prepnula),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    if h.is_invalid() {
        log::warn!("přepnutí plochy nejde hlídat — po výzvě UAC pomůže zkratka");
        None
    } else {
        Some(h)
    }
}

/// Callback přepnutí plochy. Běží ve smyčce hook vlákna (ne v LL hooku),
/// smí tedy logovat; panika nesmí přes hranici FFI.
unsafe extern "system" fn plocha_se_prepnula(
    _hook: HWINEVENTHOOK,
    _udalost: u32,
    _okno: HWND,
    _objekt: i32,
    _potomek: i32,
    _vlakno: u32,
    _cas: u32,
) {
    let _ = catch_unwind(AssertUnwindSafe(prepnuti_plochy));
}

/// Co udělat při přepnutí plochy (mimo FFI, ať to jde otestovat).
fn prepnuti_plochy() {
    if rezim().is_some_and(|m| !matches!(m, Mode::Disabled { .. })) {
        log::info!(
            "přepnutí plochy (UAC, Ctrl+Alt+Del, zamčení) — Klávesnice, držené klávesy zapomenuty"
        );
    }
    // Win+L: uvolnění Win je už na zabezpečené ploše a callback ho
    // nevidí — bity Win pryč, snímek je srovná.
    zapomen_drzene(ForceReason::DesktopSwitch);
    // Callback WinEventu běží uvnitř GetMessageW: smyčka se k snímku
    // dostane až s další zprávou — a ta nesmí být až další klávesa.
    // SAFETY: jen zpráva do fronty vlastního vlákna; nečeká.
    unsafe {
        let _ = PostThreadMessageW(GetCurrentThreadId(), WM_SNIMEK, WPARAM(0), LPARAM(0));
    }
}

/// Vynutit Klávesnici a zapomenout držené klávesy; bity Win pryč a
/// vyžádat snímek klávesnice (ten převezme klávesy, které Windows dál
/// drží). Jen smyčka a callbacky WinEventů, ne LL callback.
fn zapomen_drzene(duvod: ForceReason) {
    s_enginem(|e| e.reset_held(duvod));
    STAV.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            zapomen_klavesnici(s);
        }
    });
}

/// Popředí se hlídá jen s viditelným oknem (`chci`): schované okno
/// popředí mít nemůže a WinEvent by jen budil smyčku při každém Alt+Tab
/// (princip 10). `None` = hlídat nejde nebo není proč.
fn hlidej_popredi(h: Option<HWINEVENTHOOK>, chci: bool) -> Option<HWINEVENTHOOK> {
    match (h, chci) {
        (None, true) => {
            // SAFETY: callback je funkce tohoto modulu a žije po celý běh;
            // WINEVENT_OUTOFCONTEXT = volá se ze smyčky zpráv tohoto
            // vlákna, do cizích procesů se nic nevkládá.
            let h = unsafe {
                SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_FOREGROUND,
                    None,
                    Some(popredi_se_zmenilo),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                )
            };
            if h.is_invalid() {
                log::warn!(
                    "popředí okna nejde hlídat — okno neukáže živé klávesy a nepřijme přiřazování"
                );
                None
            } else {
                Some(h)
            }
        }
        (Some(h), false) => {
            // SAFETY: handle z SetWinEventHook, odebírá se právě jednou.
            let _ = unsafe { UnhookWinEvent(h) };
            None
        }
        (h, _) => h,
    }
}

/// Callback změny popředí. Běží ve smyčce hook vlákna (ne v LL hooku);
/// panika nesmí přes hranici FFI.
unsafe extern "system" fn popredi_se_zmenilo(
    _hook: HWINEVENTHOOK,
    _udalost: u32,
    _okno: HWND,
    _objekt: i32,
    _potomek: i32,
    _vlakno: u32,
    _cas: u32,
) {
    let _ = catch_unwind(AssertUnwindSafe(zmena_popredi));
}

/// Okno získalo nebo ztratilo popředí (mimo FFI, ať to jde otestovat).
///
/// Ztráta popředí zruší přiřazování: klávesu by jinak spolklo, i když
/// uživatel už píše jinam (Alt+Tab do chatu, OQ 42). Živý stav se
/// přepočítá (klávesy Windows jen v popředí) a smyčka si pošle
/// `WM_POPREDI`, ať projde hlídáním hooku.
fn zmena_popredi() {
    let zmeneno = STAV.with(|s| {
        let Ok(mut g) = s.try_borrow_mut() else {
            return false;
        };
        let Some(s) = g.as_mut().filter(|s| !s.rozbity) else {
            return false;
        };
        let Some(h) = s.okno else {
            return false;
        };
        let aktivni = (s.aktivni)(h);
        // Přiřazování ruší každá událost bez popředí, nejen přechod
        // z aktivního okna: zpráva o aktivaci mohla dojít až po přijetí
        // kliku, a pak by ztráta jako přechod nevypadala.
        let prirazuje = matches!(s.engine.mode(), Mode::Binding { .. });
        if aktivni == s.okno_aktivni && (aktivni || !prirazuje) {
            return false;
        }
        if aktivni {
            srovnej_win(s);
        }
        s.okno_aktivni = aktivni;
        let d = if aktivni {
            Decision::NONE
        } else {
            s.engine.cancel_binding(ted_ms())
        };
        predej(s, None, &d);
        true
    });
    if zmeneno {
        // SAFETY: jen zpráva do fronty vlastního vlákna; nečeká.
        unsafe {
            let _ = PostThreadMessageW(GetCurrentThreadId(), WM_POPREDI, WPARAM(0), LPARAM(0));
        }
    }
}

/// Okno je vidět (`Some(hwnd)`), nebo ne. `sledovano` = popředí se hlídá
/// (WinEvent je zaregistrovaný); bez toho se okno nikdy nebere jako
/// aktivní — hook by jinak mohl zůstat v systému i s oknem na pozadí.
fn okno(h: Option<isize>, sledovano: bool) {
    STAV.with(|s| {
        let mut g = s.borrow_mut();
        let Some(s) = g.as_mut().filter(|s| !s.rozbity) else {
            return;
        };
        match h {
            Some(h) => {
                s.okno = Some(h);
                s.sledovano = sledovano;
                let bylo = s.okno_aktivni;
                s.okno_aktivni = sledovano && (s.aktivni)(h);
                // Okno ukázané rovnou v popředí: zpráva o změně popředí
                // už přechod neuvidí (srovnej_win v zmena_popredi).
                if s.okno_aktivni && !bylo {
                    srovnej_win(s);
                }
                // Přiřazovat jde jen v okně v popředí, jehož popředí se
                // hlídá — jinak by ho nic nezrušilo.
                let d = if s.okno_aktivni {
                    Decision::NONE
                } else {
                    s.engine.cancel_binding(ted_ms())
                };
                predej(s, None, &d);
            }
            None => {
                s.okno = None;
                s.okno_aktivni = false;
                s.sledovano = false;
                // Přiřazovat jde jen v okně, které je vidět.
                let d = s.engine.cancel_binding(ted_ms());
                predej(s, None, &d);
                // Schované okno nic neukazuje — a příští ukázání nesmí
                // na okamžik rozsvítit klávesy, které už nikdo nedrží.
                s.vystup.zive(&[LiveInputs::EMPTY; MAX_PADS]);
            }
        }
    });
}

/// Smí začít přiřazování? Jen když je okno V TU CHVÍLI v popředí (ne
/// podle poslední události — klik mohl přijít z okna, které mezitím
/// popředí ztratilo) a jeho popředí se hlídá: bez hlídání by přiřazování
/// nezrušila ztráta popředí a spolklo by klávesu napsanou v jiném
/// programu (spec 2.4).
///
/// Přijetí rovnou zapíše `okno_aktivni`: zpráva o aktivaci okna ještě
/// nemusela dojít a ztráta popředí se pozná jako přechod z aktivního.
fn prijmi_prirazovani() -> Result<(), &'static str> {
    STAV.with(|s| {
        let mut g = s.borrow_mut();
        let Some(s) = g.as_mut().filter(|s| !s.rozbity) else {
            return Err("engine není");
        };
        let Some(h) = s.okno else {
            return Err("okno KeyPadu není vidět");
        };
        if !s.sledovano {
            return Err("popředí okna se nehlídá");
        }
        if !(s.aktivni)(h) {
            return Err("okno KeyPadu není v popředí");
        }
        s.okno_aktivni = true;
        Ok(())
    })
}

/// Režim enginu (`None` = stav je pryč nebo rozbitý).
fn rezim() -> Option<Mode> {
    STAV.with(|s| {
        s.borrow()
            .as_ref()
            .filter(|s| !s.rozbity)
            .map(|s| s.engine.mode())
    })
}

/// Má být hook v systému? (spec 2.4)
///
/// - zapnutý ovladač (hra, pauza) nebo přiřazování → ano, popředí okna na
///   to nikdy nesahá (neztratí se klávesy hráče);
/// - bez ovladače jen s oknem KeyPadu v popředí (živé klávesy, Scroll
///   Lock → „zapni ovladač");
/// - rozbitý engine (`None`) → ne, callback by jen propouštěl.
fn potreba_hooku(rezim: Option<Mode>, okno_aktivni: bool) -> bool {
    match rezim {
        None => false,
        Some(Mode::Disabled { .. }) => okno_aktivni,
        Some(Mode::Keyboard | Mode::Gamepad | Mode::Binding { .. }) => true,
    }
}

/// Hook je v systému právě tehdy, když ho engine potřebuje
/// ([`potreba_hooku`]). Po odebrání se držené klávesy zapomenou — jejich
/// key-upy hook neuvidí a zastaralý záznam by příští stisk téže klávesy
/// vzal jako autorepeat.
fn hlidej_hook(hook: HHOOK, status: &HookStatus) -> HHOOK {
    let (rezim, okno_aktivni) = STAV.with(|s| {
        s.borrow()
            .as_ref()
            .filter(|s| !s.rozbity)
            .map_or((None, false), |s| (Some(s.engine.mode()), s.okno_aktivni))
    });
    // Bez zapnutého ovladače je hook v systému jen kvůli oknu: instalace
    // a odebrání při každém Alt+Tab jdou do logu jen jako `debug`.
    let kvuli_oknu = matches!(rezim, Some(Mode::Disabled { .. }));
    match (potreba_hooku(rezim, okno_aktivni), hook.is_invalid()) {
        (true, true) => match instaluj() {
            Ok(h) => {
                status.nainstalovan.store(true, Ordering::Release);
                status.instalaci.fetch_add(1, Ordering::AcqRel);
                status.chyba.store(false, Ordering::Release);
                nahlas_chybu(false);
                if kvuli_oknu {
                    log::debug!("hook klávesnice nainstalován (okno v popředí)");
                } else {
                    log::info!("hook klávesnice nainstalován (zapnutý ovladač)");
                }
                h
            }
            Err(e) => {
                // Klávesy jdou do Windows — bezpečná strana (princip 1).
                // Zachytávání bez hooku by jen předstíralo, že běží,
                // a přiřazování by čekalo na klávesu, kterou nikdo
                // neuvidí.
                if matches!(rezim, Some(Mode::Gamepad | Mode::Binding { .. })) {
                    s_enginem(|e| e.force_keyboard(ForceReason::HookReinstalled));
                }
                nahlas_chybu(true);
                if status.chyba.swap(true, Ordering::AcqRel) {
                    log::debug!("hook klávesnice pořád nejde nainstalovat: {e}");
                } else {
                    log::error!("hook klávesnice nejde nainstalovat: {e}");
                }
                hook
            }
        },
        (false, true) => {
            // Hook už není potřeba — ani jeho chyba.
            status.chyba.store(false, Ordering::Release);
            nahlas_chybu(false);
            hook
        }
        (false, false) => {
            odhookni(hook);
            status.nainstalovan.store(false, Ordering::Release);
            zapomen_drzene(ForceReason::HookReinstalled);
            if kvuli_oknu {
                log::debug!("hook klávesnice odebrán (okno není v popředí)");
            } else {
                log::info!("hook klávesnice odebrán (žádný zapnutý ovladač)");
            }
            HHOOK::default()
        }
        _ => hook,
    }
}

/// Hook do systému ([`Stav::instaluj`]). Po instalaci vyžádá snímek
/// klávesnice (udělá ho smyčka hned za tím, [`dokonci_snimek`]): klávesy
/// stisknuté předtím, včetně Win, callback neviděl.
fn instaluj() -> Result<HHOOK, String> {
    let f = STAV.with(|s| {
        s.borrow()
            .as_ref()
            .map_or(nainstaluj as fn() -> Result<HHOOK, String>, |s| s.instaluj)
    });
    let h = f()?;
    STAV.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            zapomen_klavesnici(s);
        }
    });
    Ok(h)
}

fn nainstaluj() -> Result<HHOOK, String> {
    // SAFETY: modul vlastního .exe; callback je `extern "system"` funkce
    // téhož modulu, žije po celý běh procesu.
    unsafe {
        let modul = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(callback), Some(modul.into()), 0)
            .map_err(|e| format!("SetWindowsHookExW: {e}"))
    }
}

fn odhookni(hook: HHOOK) {
    // SAFETY: handle z SetWindowsHookExW, odebírá se právě jednou.
    let _ = unsafe { UnhookWindowsHookEx(hook) };
}

/// Starý hook pryč, nový dovnitř, držené klávesy zapomenout.
///
/// Nejdřív odhooknout, pak instalovat: dva hooky naráz by callback
/// volaly dvakrát na každou událost a engine by druhý key-down bral jako
/// autorepeat. Mezi tím projde pár klávesnicových událostí rovnou do
/// OS — přesně ty, které by se ztratily i při tichém odebrání; proto
/// `reset_held(HookReinstalled)`.
fn preinstaluj(stary: HHOOK, status: &HookStatus) -> HHOOK {
    if !stary.is_invalid() {
        odhookni(stary);
    }
    zapomen_drzene(ForceReason::HookReinstalled);
    match instaluj() {
        Ok(h) => {
            status.nainstalovan.store(true, Ordering::Release);
            status.instalaci.fetch_add(1, Ordering::AcqRel);
            status.chyba.store(false, Ordering::Release);
            nahlas_chybu(false);
            log::info!("hook klávesnice přeinstalován");
            h
        }
        Err(e) => {
            status.nainstalovan.store(false, Ordering::Release);
            status.chyba.store(true, Ordering::Release);
            nahlas_chybu(true);
            log::error!("hook klávesnice nejde znovu nainstalovat: {e}");
            HHOOK::default()
        }
    }
}

/// Ohlásí výstupu, že hook (ne)jde nainstalovat.
fn nahlas_chybu(chyba: bool) {
    STAV.with(|s| {
        if let Some(s) = s.borrow().as_ref() {
            s.vystup.hook_chyba(chyba);
        }
    });
}

/// Časovač běží jen během přiřazování klávesy.
fn hlidej_casovac(casovac: usize) -> usize {
    let prirazuje = STAV.with(|s| {
        s.borrow()
            .as_ref()
            .is_some_and(|s| matches!(s.engine.mode(), Mode::Binding { .. }))
    });
    // SAFETY: časovač vlákna (bez okna), zruší se týmž id.
    unsafe {
        match (prirazuje, casovac) {
            (true, 0) => SetTimer(None, 0, TIK_MS, None),
            (false, id) if id != 0 => {
                let _ = KillTimer(None, id);
                0
            }
            (_, id) => id,
        }
    }
}

/// Příkaz, který nepotřebuje stav smyčky (hook, WinEventy).
fn proved(p: HookPrikaz) {
    let ted = ted_ms();
    match p {
        HookPrikaz::Prirad { cil, druh } => match prijmi_prirazovani() {
            Ok(()) => {
                // Dřív, než se začne čekat na stisk: Raw Input klávesnice
                // by s oknem KeyPadu v popředí hook podle všeho umlčel
                // (OQ 60).
                over_raw_input();
                // Okno KeyPadu je teď ověřeně v popředí — jediná chvíle,
                // kdy stavu klávesnice Windows věříme natolik, abychom
                // podle něj zapomínali. Zastaralý modifikátor, Esc nebo bit
                // Win (key-up, který hook neviděl) by jinak přiřazování
                // zablokovaly: nic by se nepřiřadilo a Esc by nezrušil.
                srovnej_pred_prirazovanim();
                s_enginem(|e| e.start_binding(cil, druh, ted));
            }
            Err(proc) => log::debug!("přiřazování: {proc} — klik se nepřijímá"),
        },
        HookPrikaz::ZrusPrirazeni => s_enginem(|e| e.cancel_binding(ted)),
        // Klávesy, které Windows drží dál (zamčení s drženou klávesou),
        // převezme snímek na konci obrátky smyčky.
        HookPrikaz::Zapomen(duvod) => zapomen_drzene(duvod),
        HookPrikaz::Uprav { zmena, odpoved } => {
            if let Some(r) = uprav(zmena, ted) {
                let _ = odpoved.try_send(r);
            }
        }
        HookPrikaz::Zverejni => zverejni(),
        #[cfg(debug_assertions)]
        HookPrikaz::TestKlavesa { klavesa, vk, dolu } => {
            let u = Udalost {
                klavesa,
                scan: u32::from(klavesa.scan),
                vk,
                flags: if klavesa.extended {
                    LLKHF_EXTENDED.0
                } else {
                    0
                },
                dolu,
            };
            let _ = zpracuj_udalost(&u);
        }
        p => s_enginem(|e| prikaz(e, p, ted)),
    }
}

/// Příkazy, které jsou jen voláním enginu.
fn prikaz(e: &mut Engine, p: HookPrikaz, ted: u64) -> Decision {
    match p {
        HookPrikaz::Povol(pad) => e.enable(pad),
        HookPrikaz::Zakaz(pad, duvod) => e.disable(pad, duvod),
        HookPrikaz::Prepni => e.toggle(ted),
        HookPrikaz::Zachytavej => e.capture(ted),
        HookPrikaz::Vynut(duvod) => e.force_keyboard(duvod),
        HookPrikaz::Zapomen(duvod) => e.reset_held(duvod),
        // Přeinstalaci, okno, přiřazování, úpravy a snímek dělá smyčka
        // (`proved`); bez hooku v systému není co přeinstalovat.
        _ => Decision::NONE,
    }
}

/// Úprava mapování z editoru: klon mapování (ve smyčce, ne v callbacku)
/// → operace nad ním (všechno, nebo nic) → `replace_mapping` bez
/// vynucení Klávesnice (OQ 43). `None` = engine je rozbitý, odpověď se
/// neposílá.
fn uprav(zmena: Zmena, ted: u64) -> Option<Result<(), ChybaUpravy>> {
    STAV.with(|s| {
        let mut g = s.borrow_mut();
        let s = g.as_mut().filter(|s| !s.rozbity)?;
        let mut m = s.engine.mapping().clone();
        let r = match zmena {
            Zmena::VyprazdniVstup(t) => m.unbind_target(t).map(drop),
            Zmena::VymazOvladac(pad) => m.clear_pad(pad).map(drop),
            Zmena::VychoziPrvni => {
                m = m.defaults_for_first_pad();
                Ok(())
            }
            Zmena::Obnov {
                mapovani,
                kdyz_revize,
            } => {
                if s.engine.mapping_rev() != kdyz_revize {
                    return Some(Err(ChybaUpravy::Zastarale));
                }
                m = *mapovani;
                Ok(())
            }
        };
        if let Err(e) = r {
            return Some(Err(ChybaUpravy::Mapovani(e)));
        }
        let d = s.engine.replace_mapping(m, ted);
        predej(s, None, &d);
        Some(Ok(()))
    })
}

/// Snímek mapování pro okno — klon jen tady, ve smyčce.
fn zverejni() {
    STAV.with(|s| {
        if let Some(s) = s.borrow().as_ref().filter(|s| !s.rozbity) {
            s.vystup
                .mapovani(s.engine.mapping_rev(), s.engine.mapping());
        }
    });
}

/// Zavolá engine mimo callback a rozhodnutí předá dál.
fn s_enginem(f: impl FnOnce(&mut Engine) -> Decision) {
    STAV.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(s) = s.as_mut().filter(|s| !s.rozbity) {
            let d = f(&mut s.engine);
            predej(s, None, &d);
        }
    });
}

/// Rozhodnutí enginu → výstup: stavy padů a režim (s oznámením),
/// revize mapování a — jen s viditelným oknem — živý stav vstupů.
/// Společné pro callback i smyčku, ať okno dostane totéž, odkudkoli
/// změna přišla. Jen atomiky, `SetEvent` a zápis polí stavu (volá se
/// z callbacku).
fn predej(s: &mut Stav, u: Option<&Udalost>, d: &Decision) {
    sleduj_prirazovani(s, d);
    s.vystup.rozhodnuti(u, d, s.engine.mode());
    s.vystup.revize(s.engine.mapping_rev());
    if s.okno.is_some() {
        s.vystup.zive(&s.engine.live_inputs(s.okno_aktivni));
    }
}

/// Callback `WH_KEYBOARD_LL`.
///
/// Všechno uvnitř `catch_unwind`: panika nesmí přejít přes hranici
/// `extern "system"` (proces by skončil) a hlavně nesmí nechat
/// klávesnici v nejasném stavu (princip 1).
unsafe extern "system" fn callback(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Úplně první: že Windows callback vůbec zavolaly (OQ 60 — s Raw
    // Input klávesnice v procesu by podle všeho nezavolaly). Jen přičtení
    // do atomiku.
    VOLANI.fetch_add(1, Ordering::Relaxed);
    // Záporný kód = „nesahat, jen předat dál" (dokumentace hooku).
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        // SAFETY: u HC_ACTION ukazuje lParam na KBDLLHOOKSTRUCT platnou
        // po dobu volání.
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let potlacit = match catch_unwind(AssertUnwindSafe(|| zpracuj(wparam, kb))) {
            Ok(p) => p,
            Err(_) => {
                po_panice();
                false
            }
        };
        if potlacit {
            // Nenulová hodnota = událost nepředávat dál ani do OS.
            return LRESULT(1);
        }
    }
    // SAFETY: předání dalšímu hooku v řetězu se stejnými parametry.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Rozhodne o jedné události. `true` = potlačit.
fn zpracuj(wparam: WPARAM, kb: &KBDLLHOOKSTRUCT) -> bool {
    let dolu = match wparam.0 as u32 {
        WM_KEYDOWN | WM_SYSKEYDOWN => true,
        WM_KEYUP | WM_SYSKEYUP => false,
        _ => return false,
    };
    if dolu {
        DOLU.fetch_add(1, Ordering::Relaxed);
    }
    zpracuj_udalost(&Udalost::z(kb, dolu))
}

/// Jádro callbacku: jedna událost → engine → výstup. `true` = potlačit.
/// Touž funkcí jde i syntetická klávesa testů
/// ([`HookPrikaz::TestKlavesa`]) a měření `hook_selftest -- mereni`.
fn zpracuj_udalost(u: &Udalost) -> bool {
    #[cfg(debug_assertions)]
    let _v_udalosti = VUdalosti::zacni();
    STAV.with(|s| {
        // Zanořené volání (nemělo by nastat — callback nic nepumpuje)
        // nebo vlákno bez stavu: propustit, nikdy neblokovat. Ale ne bez
        // počtu — do řádku o konci přiřazování (OQ 60).
        let Ok(mut s) = s.try_borrow_mut() else {
            SOUBEH.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        let Some(s) = s.as_mut().filter(|s| !s.rozbity) else {
            ROZBITY.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        let ted = ted_ms();
        // Win sleduje callback sám z událostí (OQ 44): i vstříknutá mění
        // stav OS. Na stav klávesnice se callback Windows neptá vůbec
        // (OQ 57) — uvolnění Win, které minul, srovná snímek ve smyčce,
        // a když ho minul bez přepnutí plochy (okno s právy správce),
        // bit po WIN_PLATNOST_MS bez další události Win propadne. Jinak
        // by každý stisk patřil Windows: hra by nedostala nic a přiřazování
        // by nevzalo žádnou klávesu, ani Esc.
        let win = bit_win(u.vk);
        if win != 0 {
            s.win_ms = ted;
            if u.dolu {
                s.win |= win;
            } else {
                s.win &= !win;
            }
        } else if s.win != 0 && ted.saturating_sub(s.win_ms) >= WIN_PLATNOST_MS {
            s.win = 0;
        }
        let prirazuje = matches!(s.engine.mode(), Mode::Binding { .. });
        if u.vstrcena() {
            if prirazuje {
                eviduj_stisk(s, u, None, false, &Decision::NONE);
            }
            s.vystup
                .rozhodnuti(Some(u), &Decision::NONE, s.engine.mode());
            return false;
        }
        // Watchdog: klávesy do zaseknutého ovladače by jen mizely
        // (princip 1) — hra dál vidí jeho poslední stav a uživatel nemůže
        // ani psát. Proto Klávesnice; zachytávání vrátí zkratka.
        if s.engine.mode() == Mode::Gamepad && zaseknuty(s, ted) {
            let d = s.engine.force_keyboard(ForceReason::Watchdog);
            predej(s, None, &d);
        }
        // Drží-li Windows Win (Fáze 4b, OQ 44), nový stisk patří Windows:
        // Win+D, Win+E, Win+Tab i za hry a při přiřazování. Win sama jde
        // do OS vždy (engine ji nesleduje) — kdyby hook druhou klávesu
        // spolkl jako klávesu ovladače, Windows by viděly osamělou Win
        // a otevřely Start. Klávesa zůstane Windows až do uvolnění.
        // U stisku, o kterém engine nerozhoduje, dopadne převzetí stejně
        // jako nový stisk (klávesa OS bez efektu).
        let pred = s.engine.held(u.klavesa);
        let s_win = u.dolu && s.win != 0 && pred.is_none() && s.engine.adopt_os_key(u.klavesa, ted);
        let d = s.engine.on_key(u.klavesa, u.dolu, ted);
        if prirazuje {
            eviduj_stisk(s, u, pred, s_win, &d);
        }
        predej(s, Some(u), &d);
        d.suppress
    })
}

/// Je některý připravený ovladač zaseknutý (pad vlákno dlouho nemluvilo
/// s ViGEm)? Jen čtení atomiků — volá se z callbacku.
fn zaseknuty(s: &Stav, ted: u64) -> bool {
    PadId::ALL.iter().any(|&p| {
        s.engine.is_ready(p)
            && s.vystup.tep_ms(p).is_some_and(|t| {
                let zivy = t.max(s.povoleno_ms[p.index()]);
                ted.saturating_sub(zivy) > WATCHDOG_MS
            })
    })
}

/// Úklid po panice v callbacku: klávesu už callback propustil, engine
/// zapomene držené klávesy a vynutí Klávesnici — `reset_held`, ne
/// `force_keyboard`, jinak by key-up propuštěné klávesy spolkl a v OS
/// by visela (ROADMAP, Fáze 3). Když selže i tohle, engine se už
/// nepoužívá a smyčka hook odebere. Logovat smí až smyčka; snímek
/// klávesnice (klávesy, které Windows dál drží) udělá taky ona po
/// `WM_PANIKA`.
fn po_panice() {
    let uklid = catch_unwind(AssertUnwindSafe(|| {
        STAV.with(|s| {
            if let Ok(mut s) = s.try_borrow_mut() {
                if let Some(s) = s.as_mut() {
                    let d = s.engine.reset_held(ForceReason::HookPanic);
                    zapomen_klavesnici(s);
                    predej(s, None, &d);
                }
            }
        })
    }));
    if uklid.is_err() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            STAV.with(|s| {
                if let Ok(mut s) = s.try_borrow_mut() {
                    if let Some(s) = s.as_mut() {
                        s.rozbity = true;
                    }
                }
            })
        }));
    }
    // SAFETY: jen zpráva do fronty vlastního vlákna; nečeká.
    unsafe {
        let _ = PostThreadMessageW(GetCurrentThreadId(), WM_PANIKA, WPARAM(0), LPARAM(0));
    }
}

/// Měření pro `hook_selftest -- mereni`: `n` syntetických událostí
/// (stisky a uvolnění prvních kláves mapování, až šest držených naráz)
/// TOUŽ funkcí jako callback, na volajícím vlákně a bez hooku v systému
/// — nic nejde do OS ani do hry. Ovladač 1 zachytává, takže každá
/// událost projde celou cestou až do slotu padu; `zive` = s viditelným
/// oknem v popředí (živý stav navíc); `vk` = virtuální klávesa události,
/// jak by ji nesl skutečný callback (podle ní se sleduje Win). Na stav
/// klávesnice se callback od opravy OQ 57 neptá vůbec — `os_drzi` je
/// podvrh, který zpanikaří, kdyby se zeptal. Vrací ns na událost.
#[doc(hidden)]
#[allow(dead_code, reason = "měří jen příklad hook_selftest (-- mereni)")]
pub fn zmer_zpracovani(
    n: usize,
    mapovani: Mapping,
    vystup: Arc<dyn Vystup>,
    zive: bool,
    vk: fn(KeyId) -> u32,
) -> Vec<u64> {
    let klavesy: Vec<KeyId> = mapovani.bindings().map(|(k, _)| k).take(6).collect();
    let vk: Vec<u32> = klavesy.iter().map(|&k| vk(k)).collect();
    let mut engine = Engine::new(mapovani);
    let _ = engine.enable(PadId::FIRST);
    let _ = engine.capture(ted_ms());
    let mut s = Stav::novy(engine, vystup, je_v_popredi);
    // Watchdog se počítá, ale nesmí zachytávání vypnout: tep padu tu
    // nikdo nepíše.
    s.povoleno_ms = [u64::MAX; MAX_PADS];
    s.os_drzi = |_| panic!("callback se ptal na stav klávesnice");
    if zive {
        s.okno = Some(1);
        s.okno_aktivni = true;
    }
    let predchozi = STAV.with(|st| st.borrow_mut().replace(s));
    let mut drzeno = vec![false; klavesy.len()];
    let mut casy = Vec::with_capacity(n);
    for i in 0..n {
        let j = i % klavesy.len();
        let k = klavesy[j];
        drzeno[j] = !drzeno[j];
        let u = Udalost {
            klavesa: k,
            scan: u32::from(k.scan),
            vk: vk[j],
            flags: if k.extended { LLKHF_EXTENDED.0 } else { 0 },
            dolu: drzeno[j],
        };
        let t = std::time::Instant::now();
        let p = zpracuj_udalost(&u);
        let dt = t.elapsed();
        std::hint::black_box(p);
        casy.push(u64::try_from(dt.as_nanos()).unwrap_or(u64::MAX));
    }
    STAV.with(|st| *st.borrow_mut() = predchozi);
    casy
}

/// Měření pro `hook_selftest -- mereni-instalace`: `n` snímků klávesnice
/// ([`snimek_klavesnice`] — skutečný `GetAsyncKeyState` přes všechny
/// virtuální klávesy) na volajícím vlákně. Volající ho pouští na vlastní
/// skryté ploše: ta není vstupní, Windows tam vracejí nuly a skutečná
/// klávesnice vlastníka se nečte (cena volání je stejná). Snímek
/// doprovází každou instalaci hooku. Vrací ns na snímek.
#[doc(hidden)]
#[allow(
    dead_code,
    reason = "měří jen příklad hook_selftest (-- mereni-instalace)"
)]
pub fn zmer_snimek(n: usize, mapovani: Mapping, vystup: Arc<dyn Vystup>) -> Vec<u64> {
    let s = Stav::novy(Engine::new(mapovani), vystup, je_v_popredi);
    let predchozi = STAV.with(|st| st.borrow_mut().replace(s));
    let mut casy = Vec::with_capacity(n);
    for _ in 0..n {
        let t = std::time::Instant::now();
        let p = snimek_klavesnice(false);
        let dt = t.elapsed();
        std::hint::black_box(p);
        casy.push(u64::try_from(dt.as_nanos()).unwrap_or(u64::MAX));
    }
    STAV.with(|st| *st.borrow_mut() = predchozi);
    casy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::windows::slot::{Budik, StavSlot};
    use crate::platform::windows::vystup::HookVystup;
    use keypad_core::{
        Action, BindingCancel, ModeCause, PadAction, PadButton, PadState, StickDir, UiEvent,
    };
    use std::cell::Cell;
    use std::sync::atomic::AtomicU64;
    use std::sync::Mutex;
    use windows::Win32::UI::WindowsAndMessaging::{KBDLLHOOKSTRUCT_FLAGS, LLKHF_UP};

    /// F24 → A, zkratka F23: klávesy, které na ploše vlastníka nikdo
    /// nezmáčkne — kdyby test hooku přece jen viděl skutečnou klávesnici.
    const F23: KeyId = KeyId::new(0x6E);
    const F24: KeyId = KeyId::new(0x76);

    fn mapovani() -> Mapping {
        Mapping::new(F23, [(F24, PadAction::first(Action::Button(PadButton::A)))]).unwrap()
    }

    /// Jedno předané rozhodnutí: událost, rozhodnutí, režim po něm.
    type Radek = (Option<Udalost>, Decision, Mode);

    #[derive(Default)]
    struct Zaznam {
        rozhodnuti: Mutex<Vec<Radek>>,
        /// Kolikátým voláním zpanikařit (0 = nikdy).
        panikar: AtomicU32,
        volani: AtomicU32,
        /// Tep ovladačů pro watchdog (0 = výstup ho nezná).
        tep: AtomicU64,
        zive: Mutex<Vec<[LiveInputs; MAX_PADS]>>,
        revize: AtomicU64,
        mapovani: Mutex<Vec<(u64, Mapping)>>,
        hook_chyba: Mutex<Vec<bool>>,
    }

    impl Vystup for Zaznam {
        fn hook_chyba(&self, chyba: bool) {
            self.hook_chyba.lock().unwrap().push(chyba);
        }

        fn rozhodnuti(&self, u: Option<&Udalost>, d: &Decision, rezim: Mode) {
            let n = self.volani.fetch_add(1, Ordering::AcqRel) + 1;
            let p = self.panikar.load(Ordering::Acquire);
            if p != 0 && (n == p || p == u32::MAX) {
                panic!("zkušební panika");
            }
            self.rozhodnuti
                .lock()
                .unwrap()
                .push((u.copied(), *d, rezim));
        }

        fn tep_ms(&self, _pad: PadId) -> Option<u64> {
            Some(self.tep.load(Ordering::Acquire)).filter(|&t| t != 0)
        }

        fn zive(&self, z: &[LiveInputs; MAX_PADS]) {
            self.zive.lock().unwrap().push(*z);
        }

        fn revize(&self, rev: u64) {
            self.revize.store(rev, Ordering::Release);
        }

        fn mapovani(&self, rev: u64, m: &Mapping) {
            self.mapovani.lock().unwrap().push((rev, m.clone()));
        }
    }

    impl Zaznam {
        fn posledni(&self) -> Radek {
            *self.rozhodnuti.lock().unwrap().last().unwrap()
        }

        fn posledni_zive(&self) -> Option<[LiveInputs; MAX_PADS]> {
            self.zive.lock().unwrap().last().copied()
        }
    }

    thread_local! {
        /// Podvrh popředí okna pro testy na vlákně testu.
        static AKTIVNI: Cell<bool> = const { Cell::new(false) };
    }

    fn podvrh_aktivni(_hwnd: isize) -> bool {
        AKTIVNI.with(Cell::get)
    }

    fn nastav_popredi(v: bool) {
        AKTIVNI.with(|a| a.set(v));
    }

    /// Stav callbacku na tomhle vlákně bez skutečného hooku: engine
    /// povolený a přepnutý na Gamepad.
    fn priprav(z: &Arc<Zaznam>) {
        priprav_s(z, mapovani());
    }

    fn priprav_s(z: &Arc<Zaznam>, m: Mapping) {
        let mut engine = Engine::new(m);
        let _ = engine.enable(PadId::FIRST);
        let _ = engine.toggle(0);
        assert_eq!(engine.mode(), Mode::Gamepad);
        let vystup: Arc<dyn Vystup> = z.clone();
        let mut s = Stav::novy(engine, vystup, podvrh_aktivni);
        s.os_drzi = nic_nedrzi;
        STAV.with(|st| *st.borrow_mut() = Some(s));
    }

    /// OS drží všechno — i klávesu, o které se právě rozhoduje (výklad
    /// OQ 57, nejspíš mylný — OQ 60; callback se ptát nesmí tak jako tak).
    fn vse_drzi(_vk: u32) -> bool {
        true
    }

    /// OS drží jen levou Win (uživatel mačká Win+něco).
    fn jen_win(vk: u32) -> bool {
        vk == 0x5B
    }

    /// OS drží F24 (VK_F24) — klávesa testovacího mapování.
    fn jen_f24(vk: u32) -> bool {
        vk == 0x87
    }

    /// Callback se na stav klávesnice ptát nesmí (OQ 57).
    fn nesmi_se_ptat(_vk: u32) -> bool {
        panic!("callback se ptal na stav klávesnice")
    }

    fn podvrhni_os(f: fn(u32) -> bool) {
        STAV.with(|s| s.borrow_mut().as_mut().unwrap().os_drzi = f);
    }

    fn kb(scan: u32, flags: u32) -> KBDLLHOOKSTRUCT {
        KBDLLHOOKSTRUCT {
            vkCode: 0,
            scanCode: scan,
            flags: KBDLLHOOKSTRUCT_FLAGS(flags),
            time: 0,
            dwExtraInfo: 0,
        }
    }

    /// Událost s virtuální klávesou (Win, Alt — hook podle ní sleduje Win).
    fn kb_vk(scan: u32, flags: u32, vk: u32) -> KBDLLHOOKSTRUCT {
        KBDLLHOOKSTRUCT {
            vkCode: vk,
            ..kb(scan, flags)
        }
    }

    /// Levá (`false`) nebo pravá Win dolů / nahoru — skutečnými kódy
    /// (scan s E0 a VK), jak je posílá klávesnice.
    fn win(prava: bool, dolu: bool) -> bool {
        let (scan, vk) = if prava { (0x5C, 0x5C) } else { (0x5B, 0x5B) };
        let flags = LLKHF_EXTENDED.0 | if dolu { 0 } else { LLKHF_UP.0 };
        zavolej(
            0,
            if dolu { WM_KEYDOWN } else { WM_KEYUP },
            &kb_vk(scan, flags, vk),
        )
    }

    /// Levý Alt dolů / nahoru (VK_LMENU; s Altem chodí WM_SYS…).
    fn alt(dolu: bool) -> bool {
        zavolej(
            0,
            if dolu { WM_SYSKEYDOWN } else { WM_KEYUP },
            &kb_vk(0x38, if dolu { 0 } else { LLKHF_UP.0 }, 0xA4),
        )
    }

    /// Callback zavolaný přímo — tak, jak ho volají Windows.
    fn zavolej(code: i32, zprava: u32, kb: &KBDLLHOOKSTRUCT) -> bool {
        let r = unsafe {
            callback(
                code,
                WPARAM(zprava as usize),
                LPARAM(kb as *const _ as isize),
            )
        };
        r.0 != 0
    }

    fn drzeno(k: KeyId) -> bool {
        STAV.with(|s| s.borrow().as_ref().unwrap().engine.held(k).is_some())
    }

    fn rezim() -> Mode {
        STAV.with(|s| s.borrow().as_ref().unwrap().engine.mode())
    }

    fn s_stavem<T>(f: impl FnOnce(&mut Stav) -> T) -> T {
        STAV.with(|s| f(s.borrow_mut().as_mut().unwrap()))
    }

    #[test]
    fn bez_stavu_vse_propousti() {
        STAV.with(|s| *s.borrow_mut() = None);
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        assert!(!zavolej(0, WM_KEYUP, &kb(0x76, LLKHF_UP.0)));
    }

    #[test]
    fn namapovana_klavesa_se_potlaci_a_hybe_padem() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        assert!(zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        let (u, d, m) = z.posledni();
        assert_eq!(u.unwrap().klavesa, F24);
        assert!(d.suppress && m == Mode::Gamepad);
        assert!(d.pads.get(PadId::FIRST).unwrap().is_pressed(PadButton::A));
        // Autorepeat se potlačí taky, key-up vrátí neutrál.
        assert!(zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        assert!(zavolej(0, WM_KEYUP, &kb(0x76, LLKHF_UP.0)));
        assert_eq!(
            z.posledni().1.pads.get(PadId::FIRST),
            Some(PadState::NEUTRAL)
        );
        // WM_SYSKEYDOWN (s Altem) je taky stisk.
        assert!(zavolej(0, WM_SYSKEYDOWN, &kb(0x76, 0)));
        assert!(zavolej(0, WM_SYSKEYUP, &kb(0x76, LLKHF_UP.0)));
    }

    #[test]
    fn nenamapovana_klavesa_jde_do_os() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        // W v tomhle mapování není.
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x11, 0)));
        assert!(!zavolej(0, WM_KEYUP, &kb(0x11, LLKHF_UP.0)));
    }

    #[test]
    fn rozsirena_klavesa_je_jina_nez_bez_e0() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        // 0x76 s E0 není F24.
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x76, LLKHF_EXTENDED.0)));
        assert_eq!(z.posledni().0.unwrap().klavesa, KeyId::ext(0x76));
        assert!(!drzeno(F24));
    }

    #[test]
    fn zaporny_kod_jde_rovnou_dal_bez_enginu() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        assert!(!zavolej(-1, WM_KEYDOWN, &kb(0x76, 0)));
        assert_eq!(z.volani.load(Ordering::Acquire), 0);
        assert!(!drzeno(F24));
    }

    #[test]
    fn vstrcena_udalost_projde_a_engine_ji_nevidi() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x76, LLKHF_INJECTED.0)));
        assert!(!drzeno(F24));
        let (u, d, _) = z.posledni();
        assert!(u.unwrap().vstrcena());
        assert_eq!(d, Decision::NONE);
        // Uživatel drží F24, program vstříkne jeho key-up: engine klávesu
        // dál drží a skutečný key-up ji pustí.
        assert!(zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        assert!(!zavolej(
            0,
            WM_KEYUP,
            &kb(0x76, LLKHF_UP.0 | LLKHF_INJECTED.0)
        ));
        assert!(drzeno(F24));
        assert!(zavolej(0, WM_KEYUP, &kb(0x76, LLKHF_UP.0)));
        assert!(!drzeno(F24));
    }

    #[test]
    fn altgr_falesny_ctrl_a_prilis_velky_scan_projdou() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        // Falešný LCtrl z AltGr (CZ): scan 0x21D.
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x21D, 0)));
        assert!(!drzeno(KeyId::LEFT_CTRL));
        // Scan kód nad u16 se nesmí useknout na F24 (0x1_0076).
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x1_0076, 0)));
        assert_eq!(z.posledni().0.unwrap().klavesa.scan, 0);
        assert!(!drzeno(F24));
    }

    #[test]
    fn jina_zprava_nez_klavesa_projde() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        assert!(!zavolej(0, WM_TIMER, &kb(0x76, 0)));
        assert_eq!(z.volani.load(Ordering::Acquire), 0);
    }

    /// Panika při rozhodování: klávesa projde do OS, engine zapomene
    /// držené klávesy a je na Klávesnici — key-up té klávesy pak jde do
    /// OS taky (nic nevisí).
    #[test]
    fn panika_propusti_klavesu_a_zapomene_drzene() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        z.panikar.store(1, Ordering::Release);
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        assert_eq!(rezim(), Mode::Keyboard);
        assert!(!drzeno(F24));
        let (u, d, m) = z.posledni();
        assert_eq!((u, m), (None, Mode::Keyboard));
        assert_eq!(d.pads.get(PadId::FIRST), Some(PadState::NEUTRAL));
        assert_eq!(
            d.ui,
            Some(UiEvent::ModeChanged {
                mode: Mode::Keyboard,
                cause: ModeCause::Forced(ForceReason::HookPanic),
            })
        );
        assert!(!zavolej(0, WM_KEYUP, &kb(0x76, LLKHF_UP.0)));
        // Hook dál funguje.
        STAV.with(|s| assert!(!s.borrow().as_ref().unwrap().rozbity));
    }

    /// Selže i úklid: engine se už nevolá, všechno jde do OS.
    #[test]
    fn panika_i_pri_uklidu_vse_propousti() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        z.panikar.store(u32::MAX, Ordering::Release);
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        STAV.with(|s| assert!(s.borrow().as_ref().unwrap().rozbity));
        let volani = z.volani.load(Ordering::Acquire);
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        assert!(!zavolej(0, WM_KEYUP, &kb(0x76, LLKHF_UP.0)));
        assert_eq!(
            z.volani.load(Ordering::Acquire),
            volani,
            "engine se už nevolá"
        );
    }

    /// Callback nic nealokuje ani neuvolňuje — při stisku, autorepeatu,
    /// key-upu, přepnutí zkratkou, nemapovatelné či vstříknuté klávese,
    /// s drženou Win (klávesa se převezme jako klávesa OS) i bez ní,
    /// s oknem (živý stav) i bez něj a při přiřazování (oznámení uloženo,
    /// odmítnuto, ťuknutí Altem, počty diagnostiky a záznam o konci).
    /// Výstup je skutečný `HookVystup` (sloty, atomiky, `SetEvent`).
    #[test]
    fn callback_nealokuje() {
        // Scroll Lock (přepnutí / odmítnutí), AltGr, média, vstříknutá,
        // Alt (ťuknutí při přiřazování), W (uložení při přiřazování),
        // šipka, Win. (scan, příznaky, VK)
        let udalosti = [
            (0x46, 0, 0x91),
            (0x21D, 0, 0xA2),
            (0, 0, 0xB3),
            (0x1E, LLKHF_INJECTED.0, 0x41),
            (0x38, 0, 0xA4),
            (0x11, 0, 0x57),
            (0x48, LLKHF_EXTENDED.0, 0x26),
            (0x5B, LLKHF_EXTENDED.0, 0x5B),
        ];
        let cil = PadAction::first(Action::Button(PadButton::B));
        for s_win in [false, true] {
            for okno in [None, Some(7isize)] {
                for prirazuje in [false, true] {
                    let sloty: [Arc<StavSlot>; MAX_PADS] =
                        std::array::from_fn(|_| Arc::new(StavSlot::new().unwrap()));
                    let budik = Arc::new(Budik::new().unwrap());
                    let vystup: Arc<dyn Vystup> = Arc::new(HookVystup::new(sloty, budik));
                    let mut engine = Engine::new(Mapping::default());
                    let _ = engine.enable(PadId::FIRST);
                    let mut s = Stav::novy(engine, vystup, |_| true);
                    s.os_drzi = |_| panic!("callback se ptal na stav klávesnice");
                    s.okno = okno;
                    s.okno_aktivni = okno.is_some();
                    STAV.with(|st| *st.borrow_mut() = Some(s));
                    for _ in 0..3 {
                        if prirazuje {
                            s_enginem(|e| e.start_binding(cil, BindKind::Replace, ted_ms()));
                        }
                        let pred = crate::testy_alokace::pocet();
                        if s_win {
                            win(false, true);
                        }
                        for &(scan, flags, vk) in &udalosti {
                            let k = kb_vk(scan, flags, vk);
                            zavolej(0, WM_KEYDOWN, &k);
                            zavolej(0, WM_KEYDOWN, &k);
                            zavolej(0, WM_KEYUP, &kb_vk(scan, flags | LLKHF_UP.0, vk));
                        }
                        if s_win {
                            win(false, false);
                        }
                        assert_eq!(
                            crate::testy_alokace::pocet(),
                            pred,
                            "alokace v callbacku (Win {s_win}, okno {okno:?}, přiřazuje {prirazuje})"
                        );
                    }
                }
            }
        }
    }

    /// L jako klávesa hook (virtuální klávesa 0x4C).
    fn kb_l(flags: u32) -> KBDLLHOOKSTRUCT {
        KBDLLHOOKSTRUCT {
            vkCode: 0x4C,
            ..kb(0x26, flags)
        }
    }

    /// Win+L při hře (OQ 44): Win dole (podle událostí, ne podle dotazu
    /// na Windows) → L (ve výchozím mapování D-pad vpravo) jde do Windows
    /// celé, stisk i key-up, a stav padu se nezmění. Po uvolnění Win je
    /// L zase klávesa ovladače.
    #[test]
    fn win_s_klavesou_ve_hre_patri_windows() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        let l = KeyId::L;
        assert!(Mapping::default().target(l).is_some(), "L je namapovaná");
        // Kdyby se callback zeptal Windows, test spadne (OQ 57).
        podvrhni_os(nesmi_se_ptat);
        assert!(!win(false, true), "Win jde do Windows");
        assert!(!zavolej(0, WM_KEYDOWN, &kb_l(0)), "Win+L do Windows");
        let (u, d, m) = z.posledni();
        assert_eq!((u.unwrap().klavesa, m), (l, Mode::Gamepad));
        assert_eq!(d.pads, PadUpdates::NONE, "stav padu beze změny");
        assert_eq!(
            STAV.with(|s| s.borrow().as_ref().unwrap().engine.held(l).map(|h| h.owner)),
            Some(keypad_core::Owner::Os)
        );
        // Autorepeat i key-up zůstávají Windows, i když Win mezitím pustil.
        assert!(!win(false, false));
        assert_eq!(s_stavem(|s| s.win), 0, "Win puštěná");
        assert!(!zavolej(0, WM_KEYDOWN, &kb_l(0)));
        assert!(!zavolej(0, WM_KEYUP, &kb_l(LLKHF_UP.0)));
        assert_eq!(z.posledni().1.pads, PadUpdates::NONE);
        assert!(!drzeno(l));
        // Bez Win je L zase D-pad.
        assert!(zavolej(0, WM_KEYDOWN, &kb_l(0)));
        assert!(z
            .posledni()
            .1
            .pads
            .get(PadId::FIRST)
            .unwrap()
            .is_pressed(PadButton::DpadRight));
        assert!(zavolej(0, WM_KEYUP, &kb_l(LLKHF_UP.0)));
        // Pravá Win stejně, i vstříknutá (klávesnice na obrazovce) — i ta
        // mění stav Windows.
        assert!(!win(true, true));
        assert!(!zavolej(0, WM_KEYDOWN, &kb_l(0)), "pravá Win+L do Windows");
        assert!(!zavolej(0, WM_KEYUP, &kb_l(LLKHF_UP.0)));
        assert!(!win(true, false));
        assert!(!zavolej(
            0,
            WM_KEYDOWN,
            &kb_vk(0x5B, LLKHF_EXTENDED.0 | LLKHF_INJECTED.0, 0x5B)
        ));
        assert!(!zavolej(0, WM_KEYDOWN, &kb_l(0)), "vstříknutá Win+L");
        assert!(!zavolej(0, WM_KEYUP, &kb_l(LLKHF_UP.0)));
        assert!(!zavolej(
            0,
            WM_KEYUP,
            &kb_vk(0x5B, LLKHF_EXTENDED.0 | LLKHF_UP.0 | LLKHF_INJECTED.0, 0x5B)
        ));
        assert!(zavolej(0, WM_KEYDOWN, &kb_l(0)), "bez Win zase hra");
        assert!(zavolej(0, WM_KEYUP, &kb_l(LLKHF_UP.0)));
    }

    /// Win+L při přiřazování: L jde do Windows a nepřiřadí se;
    /// přiřazování čeká dál na klávesu bez Win.
    #[test]
    fn win_s_klavesou_pri_prirazovani_se_neprirazuje() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        let cil = PadAction::first(Action::Button(PadButton::B));
        let l = KeyId::L;
        STAV.with(|s| {
            let mut g = s.borrow_mut();
            let _ = g
                .as_mut()
                .unwrap()
                .engine
                .start_binding(cil, BindKind::Replace, ted_ms());
        });
        podvrhni_os(nesmi_se_ptat);
        assert!(!win(false, true));
        assert!(!zavolej(0, WM_KEYDOWN, &kb_l(0)), "Win+L do Windows");
        assert!(!zavolej(0, WM_KEYUP, &kb_l(LLKHF_UP.0)));
        let (_, d, m) = z.posledni();
        assert!(
            matches!(m, Mode::Binding { target, .. } if target == cil),
            "{m:?}"
        );
        assert_eq!(d.ui, None, "nic se neuložilo ani neodmítlo");
        let mapovani = STAV.with(|s| s.borrow().as_ref().unwrap().engine.mapping().clone());
        assert_ne!(mapovani.target(l), Some(cil));
        // Bez Win se L přiřadí (stisk přiřazování spolkne).
        assert!(!win(false, false));
        assert!(zavolej(0, WM_KEYDOWN, &kb_l(0)));
        assert!(
            matches!(z.posledni().1.ui, Some(UiEvent::BindingSaved { key, target, .. }) if key == l && target == cil)
        );
    }

    thread_local! {
        /// Kolikrát se kdo zeptal Windows na stav klávesy.
        static DOTAZY: Cell<u32> = const { Cell::new(0) };
    }

    fn dotazy() -> u32 {
        DOTAZY.with(Cell::get)
    }

    /// Počítaný podvrh: OS nic nedrží.
    fn pocitej_nic(_vk: u32) -> bool {
        DOTAZY.with(|d| d.set(d.get() + 1));
        false
    }

    /// Klávesa jako stisk / uvolnění bez VK (scan kód, E0 ne).
    fn klavesa(scan: u32, dolu: bool) -> bool {
        zavolej(
            0,
            if dolu { WM_KEYDOWN } else { WM_KEYUP },
            &kb(scan, if dolu { 0 } else { LLKHF_UP.0 }),
        )
    }

    /// Callback se na stav klávesnice neptá NIKDY (OQ 57) — při hře,
    /// v pauze, s drženou Win, při přiřazování, Esc, nemapovatelné,
    /// zkratce ani vstříknuté klávese. Ptá se jen snímek ve smyčce.
    #[test]
    fn callback_se_na_stav_klavesnice_nepta() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        DOTAZY.with(|d| d.set(0));
        podvrhni_os(pocitej_nic);
        // Hra: W, autorepeat, uvolnění, nenamapovaný Tab.
        assert!(klavesa(0x11, true), "W hraje");
        klavesa(0x11, true);
        klavesa(0x11, false);
        assert!(!klavesa(0x0F, true));
        klavesa(0x0F, false);
        // Pauza zkratkou a zpět.
        assert!(klavesa(0x46, true));
        klavesa(0x46, false);
        assert!(!klavesa(0x11, true));
        klavesa(0x11, false);
        assert!(klavesa(0x46, true));
        klavesa(0x46, false);
        // Win+W, AltGr, média, vstříknutá.
        assert!(!win(false, true));
        assert!(!klavesa(0x11, true), "Win+W Windows");
        klavesa(0x11, false);
        assert!(!win(false, false));
        assert!(!klavesa(0x21D, true));
        klavesa(0x21D, false);
        assert!(!zavolej(0, WM_KEYDOWN, &kb_vk(0, 0, 0xB3)));
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x1E, LLKHF_INJECTED.0)));
        assert_eq!(dotazy(), 0, "callback se Windows nezeptal");
        // Přiřazování: odmítnutí, Esc, uložení. Získání popředí a klik na
        // čepičku se Windows ptají (srovnání ve smyčce, mimo callback) —
        // počítá se jen to, co přijde potom.
        nastav_popredi(true);
        okno(Some(7), true);
        assert!(dotazy() > 0, "okno v popředí srovnalo bity Win");
        let cil = PadAction::first(Action::Button(PadButton::B));
        let prirad = || {
            proved(HookPrikaz::Prirad {
                cil,
                druh: BindKind::Replace,
            });
            assert!(dotazy() > 200, "klik na čepičku srovnal klávesnici");
            DOTAZY.with(|d| d.set(0));
        };
        prirad();
        assert!(klavesa(0x46, true), "zkratka se spolkne");
        klavesa(0x46, false);
        assert!(klavesa(0x01, true), "Esc zruší");
        klavesa(0x01, false);
        prirad();
        assert!(klavesa(0x21, true), "F se přiřadí");
        klavesa(0x21, false);
        assert_eq!(dotazy(), 0, "callback se Windows nezeptal");
        assert_eq!(
            z.rozhodnuti
                .lock()
                .unwrap()
                .iter()
                .filter(|r| matches!(r.1.ui, Some(UiEvent::BindingSaved { .. })))
                .count(),
            1
        );
        // Snímek se ptá (podvrh je zapojený tam, kam patří).
        s_stavem(zapomen_klavesnici);
        dokonci_snimek(true);
        assert!(dotazy() > 200, "snímek prošel virtuální klávesy");
    }

    /// Přiřazování a hra fungují, i když by Windows tvrdily, že drží
    /// všechno — tak OQ 57 vykládala hlášení vlastníka 6. 10. (nešlo
    /// přiřadit nic, ani Esc nerušil). Výklad je nejspíš mylný
    /// (pravděpodobnější příčinou je Raw Input, přímo neověřeno, OQ 60);
    /// test hlídá pravidlo „callback se neptá" dál.
    #[test]
    fn prirazovani_funguje_i_kdyz_os_drzi_vse() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        podvrhni_os(vse_drzi);
        // Hra: W jde ovladači.
        assert!(klavesa(0x11, true), "W hraje");
        assert!(z
            .posledni()
            .1
            .pads
            .get(PadId::FIRST)
            .is_some_and(|p| p.thumb_ly > 0));
        assert!(klavesa(0x11, false));
        // Přiřazování: F (X) na B. Smyčka (popředí, klik na čepičku) se
        // Windows ptá a dostane skutečný stav — nic držené; „Windows drží
        // všechno" platí jen pro to, na co by se zeptal callback.
        let b = PadAction::first(Action::Button(PadButton::B));
        let prirad = || {
            podvrhni_os(nic_nedrzi);
            nastav_popredi(true);
            okno(Some(7), true);
            proved(HookPrikaz::Prirad {
                cil: b,
                druh: BindKind::Replace,
            });
            podvrhni_os(vse_drzi);
        };
        prirad();
        assert!(klavesa(0x21, true), "F se při přiřazování spolkne");
        assert!(
            matches!(z.posledni().1.ui, Some(UiEvent::BindingSaved { key, target, .. }) if key == KeyId::F && target == b),
            "{:?}",
            z.posledni().1.ui
        );
        assert!(klavesa(0x21, false));
        assert_eq!(mapovani_enginu().target(KeyId::F), Some(b));
        // Esc zruší.
        prirad();
        assert!(matches!(rezim(), Mode::Binding { .. }));
        assert!(klavesa(0x01, true));
        assert_eq!(
            z.posledni().1.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Escape
            })
        );
        assert!(klavesa(0x01, false));
        assert_eq!(rezim(), Mode::Gamepad, "zpět do hry");
    }

    /// Podvrh `KEYPAD_TEST_OS_DRZI=vse` (test okna na skryté ploše):
    /// zpracování události slyší „Windows drží všechno", smyčka „nic"
    /// (popředí, klik na čepičku) — přesně rozdělení z testu výš, jen
    /// bez přepínání podvrhu ručně. Callback se neptá, takže přiřazení
    /// i Esc projdou a dotazů je nula. Opačným směrem: dotaz během
    /// události podvrh započte a hlásí „drží", i po panice v události
    /// se zase vrátí „nic".
    #[cfg(debug_assertions)]
    #[test]
    fn podvrh_os_drzi_vse_plati_jen_v_udalosti() {
        let dotazy = || DOTAZY_CALLBACKU.with(|d| d.replace(0));
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        podvrhni_os(vse_drzi_v_callbacku);
        dotazy();
        assert!(!vse_drzi_v_callbacku(0x46), "mimo událost nic");
        {
            let _v = VUdalosti::zacni();
            assert!(vse_drzi_v_callbacku(0x46), "v události všechno");
        }
        assert!(!vse_drzi_v_callbacku(0x46), "po události zase nic");
        assert_eq!(dotazy(), 1, "dotaz v události se započte");
        let _ = catch_unwind(|| {
            let _v = VUdalosti::zacni();
            panic!("panika v události (test)");
        });
        assert!(!vse_drzi_v_callbacku(0x46), "panika příznak nenechá");
        assert_eq!(dotazy(), 0);

        nastav_popredi(true);
        okno(Some(7), true);
        let b = PadAction::first(Action::Button(PadButton::B));
        let prirad = || {
            proved(HookPrikaz::Prirad {
                cil: b,
                druh: BindKind::Replace,
            });
            assert!(matches!(rezim(), Mode::Binding { .. }));
        };
        prirad();
        assert!(klavesa(0x21, true), "F se při přiřazování spolkne");
        assert!(
            matches!(z.posledni().1.ui, Some(UiEvent::BindingSaved { key, target, .. }) if key == KeyId::F && target == b),
            "{:?}",
            z.posledni().1.ui
        );
        klavesa(0x21, false);
        prirad();
        assert!(klavesa(0x01, true));
        assert_eq!(
            z.posledni().1.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Escape
            })
        );
        klavesa(0x01, false);
        assert_eq!(dotazy(), 0, "callback se Windows nezeptal");
    }

    /// Snímek po instalaci hooku převezme klávesu, kterou Windows drží
    /// a hook ji neviděl stisknout: její autorepeat i key-up jdou do
    /// Windows a ovladač se nehne (OQ 8). Nový stisk zase hraje. Bez
    /// hooku v systému se snímek nedělá.
    #[test]
    fn snimek_po_instalaci_prevezme_drzenou_klavesu() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        s_stavem(|s| s.instaluj = || Ok(HHOOK(std::ptr::without_provenance_mut(0x1000))));
        podvrhni_os(jen_f24);
        let status = HookStatus::default();
        // Podvržený handle se vrací volajícímu — nikdy se neodhookne.
        let h = hlidej_hook(HHOOK::default(), &status);
        assert!(!h.is_invalid() && status.nainstalovan());
        assert!(s_stavem(|s| s.snimek), "instalace vyžádala snímek");
        dokonci_snimek(false);
        assert!(!drzeno(F24), "bez hooku v systému žádný snímek");
        dokonci_snimek(true);
        assert!(!s_stavem(|s| s.snimek));
        assert_eq!(
            s_stavem(|s| s.engine.held(F24).map(|h| h.owner)),
            Some(keypad_core::Owner::Os),
            "F24 převzatá jako klávesa Windows"
        );
        podvrhni_os(nesmi_se_ptat);
        assert!(!klavesa(0x76, true), "autorepeat do Windows");
        assert!(!klavesa(0x76, true));
        assert!(!klavesa(0x76, false), "key-up do Windows");
        assert!(
            z.rozhodnuti.lock().unwrap().iter().all(|r| r
                .1
                .pads
                .get(PadId::FIRST)
                .is_none_or(|p| p == PadState::NEUTRAL)),
            "ovladač se nehnul"
        );
        assert!(klavesa(0x76, true), "nový stisk hraje");
        assert!(klavesa(0x76, false));
        // Snímek podle Windows nastaví i bity Win (Win držená před
        // instalací) — Win sama se nepřebírá.
        podvrhni_os(jen_win);
        s_stavem(zapomen_klavesnici);
        dokonci_snimek(true);
        assert_eq!(s_stavem(|s| s.win), WIN_L);
        assert!(!drzeno(KeyId::LEFT_WIN));
    }

    /// Převod virtuální klávesy snímku na klávesu enginu (prefix E0
    /// z `MAPVK_VK_TO_VSC_EX`) — klávesy, které mají na každém rozložení
    /// stejnou pozici.
    #[test]
    fn klavesa_z_vk_rozlisi_e0() {
        let hkl = rozlozeni_popredi();
        for (vk, cekam) in [
            (0x87, Some(F24)),               // F24
            (0xA0, Some(KeyId::LEFT_SHIFT)), // levý Shift
            (0xA3, Some(KeyId::RIGHT_CTRL)), // pravý Ctrl
            (0xA5, Some(KeyId::RIGHT_ALT)),  // pravý Alt
            (0x26, Some(KeyId::ARROW_UP)),   // šipka nahoru (převod E0 nevrátí)
            (0x25, Some(KeyId::ARROW_LEFT)), // šipka vlevo
            (0x2E, Some(KeyId::ext(0x53))),  // Delete
            (0x68, Some(KeyId::NUMPAD_8)),   // 8 na numerické
            (0x13, Some(KeyId::new(0x45))),  // Pause (jako v LL hooku)
            (0x90, Some(KeyId::ext(0x45))),  // NumLock (jako v LL hooku)
            (0xA1, Some(KeyId::ext(0x36))),  // pravý Shift (KBDEXT, OQ 59)
            (0x2C, Some(KeyId::ext(0x37))),  // Print Screen (jako v LL hooku)
            (0x0D, Some(KeyId::ENTER)),      // Enter: VK je jedna (OQ 38)
            (0x5B, None),                    // Win — engine ji nesleduje
            (0x01, None),                    // levé tlačítko myši
        ] {
            assert_eq!(klavesa_z_vk(vk, hkl), cekam, "VK 0x{vk:02X}");
        }
    }

    /// Bity Win vede callback sám z událostí: Win dolů → L patří
    /// Windows, Win nahoru → L zase ovladači. Přepnutí plochy bity
    /// vynuluje hned (uvolnění na zabezpečené ploše callback nevidí)
    /// a snímek je srovná s Windows.
    #[test]
    fn bity_win_z_udalosti_a_prepnuti_plochy() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        podvrhni_os(nesmi_se_ptat);
        assert!(!win(false, true));
        assert_eq!(s_stavem(|s| s.win), WIN_L);
        assert!(!zavolej(0, WM_KEYDOWN, &kb_l(0)), "Win+L Windows");
        assert!(!zavolej(0, WM_KEYUP, &kb_l(LLKHF_UP.0)));
        assert!(!win(false, false));
        assert_eq!(s_stavem(|s| s.win), 0);
        assert!(zavolej(0, WM_KEYDOWN, &kb_l(0)), "L ovladači");
        assert!(zavolej(0, WM_KEYUP, &kb_l(LLKHF_UP.0)));
        // Win+L: uvolnění Win je už na zabezpečené ploše.
        assert!(!win(false, true));
        prepnuti_plochy();
        assert_eq!(s_stavem(|s| s.win), 0, "přepnutí plochy bity vynuluje");
        assert!(s_stavem(|s| s.snimek), "a vyžádá snímek");
        assert_eq!(rezim(), Mode::Keyboard);
        podvrhni_os(nic_nedrzi);
        dokonci_snimek(true);
        assert_eq!(s_stavem(|s| s.win), 0, "Win puštěná na jiné ploše");
        // Zpátky ve hře L hraje (zkratka vrátila zachytávání).
        let _ = s_stavem(|s| s.engine.toggle(ted_ms()));
        podvrhni_os(nesmi_se_ptat);
        assert!(zavolej(0, WM_KEYDOWN, &kb_l(0)));
        assert!(zavolej(0, WM_KEYUP, &kb_l(LLKHF_UP.0)));
        // Zapomenutí po zamčení relace taky.
        assert!(!win(true, true));
        proved(HookPrikaz::Zapomen(ForceReason::SessionLock));
        assert_eq!(s_stavem(|s| (s.win, s.snimek)), (0, true));
    }

    /// Bit Win naposledy obnovený před `o` ms.
    fn zestarni_win(o: u64) {
        s_stavem(|s| s.win_ms = ted_ms().saturating_sub(o));
    }

    /// Revize opravy 6. 10. (OQ 39, 44): uvolnění Win callback minul BEZ
    /// přepnutí plochy (Win+1 do okna s právy správce — key-up dostane
    /// jen ono). Bit nesmí zůstat napořád, jinak by hra nedostala žádnou
    /// klávesu a přiřazování by nevzalo nic, ani Esc. Po WIN_PLATNOST_MS
    /// bez události Win patří L zase hře, autorepeat Win platnost obnoví;
    /// klik na čepičku a návrat popředí do okna srovnají bity s Windows
    /// hned.
    #[test]
    fn zmeskane_uvolneni_win_propadne_a_srovna_se() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        podvrhni_os(nesmi_se_ptat);
        let l = |dolu: bool| {
            zavolej(
                0,
                if dolu { WM_KEYDOWN } else { WM_KEYUP },
                &kb_l(if dolu { 0 } else { LLKHF_UP.0 }),
            )
        };
        assert!(!win(false, true));
        // Win-up se ztratil.
        assert!(!l(true), "Win+L Windows");
        assert!(!l(false));
        zestarni_win(WIN_PLATNOST_MS - 1_000);
        assert!(!l(true), "před limitem pořád Windows");
        assert!(!l(false));
        zestarni_win(WIN_PLATNOST_MS + 1);
        assert!(!win(false, true), "autorepeat Win");
        assert!(!l(true), "Win držená dál — Windows");
        assert!(!l(false));
        zestarni_win(WIN_PLATNOST_MS);
        assert!(l(true), "po limitu L ovladači");
        assert!(z
            .posledni()
            .1
            .pads
            .get(PadId::FIRST)
            .unwrap()
            .is_pressed(PadButton::DpadRight));
        assert_eq!(s_stavem(|s| s.win), 0, "bit propadl");
        assert!(l(false));

        // Přiřazování: Win „dole" bez uvolnění, klik na čepičku srovná
        // s Windows (Win nedrží) — L se přiřadí hned, bez čekání na limit.
        podvrhni_os(nic_nedrzi);
        nastav_popredi(true);
        okno(Some(7), true);
        assert!(!win(false, true));
        assert_eq!(s_stavem(|s| s.win), WIN_L);
        let cil = PadAction::first(Action::Button(PadButton::B));
        let prirad = || {
            proved(HookPrikaz::Prirad {
                cil,
                druh: BindKind::Replace,
            })
        };
        prirad();
        assert_eq!(s_stavem(|s| s.win), 0, "klik na čepičku srovnal Win");
        podvrhni_os(nesmi_se_ptat);
        assert!(l(true), "L se při přiřazování spolkne");
        assert!(
            matches!(z.posledni().1.ui, Some(UiEvent::BindingSaved { key, target, .. }) if key == KeyId::L && target == cil),
            "{:?}",
            z.posledni().1.ui
        );
        assert!(l(false));
        assert_eq!(rezim(), Mode::Gamepad, "zpět do hry");
        // Windows Win opravdu drží: srovnání bit nechá, L patří Windows.
        podvrhni_os(jen_win);
        prirad();
        assert_eq!(s_stavem(|s| s.win), WIN_L);
        podvrhni_os(nesmi_se_ptat);
        assert!(!l(true), "Win+L Windows");
        assert!(!l(false));
        assert!(matches!(rezim(), Mode::Binding { .. }), "nic nepřiřazeno");
        proved(HookPrikaz::ZrusPrirazeni);
        // Návrat popředí do okna srovná taky.
        nastav_popredi(false);
        zmena_popredi();
        assert_eq!(s_stavem(|s| s.win), WIN_L, "ztráta popředí nesrovnává");
        podvrhni_os(nic_nedrzi);
        nastav_popredi(true);
        zmena_popredi();
        assert_eq!(s_stavem(|s| s.win), 0, "návrat do okna srovnal Win");
    }

    /// OS drží AltGr: pravý Alt a k němu falešný levý Ctrl (VK_LCONTROL).
    fn altgr_drzi(vk: u32) -> bool {
        vk == 0xA2 || vk == 0xA5
    }

    /// Revize opravy 6. 10. (OQ 38): snímek s drženým AltGr (zapnutí
    /// ovladače nebo popředí okna při psaní „@"). Windows drží pravý Alt
    /// i falešný levý Ctrl, jehož key-up hook hlásí jako nemapovatelný
    /// 0x21D. Převzatý levý Ctrl by nikdo nepustil — modifikátor OS by
    /// pak přiřazování nevzal nic ani Esc. Snímek ho nepřevezme; pravý Alt
    /// ano (jeho key-up přijde jako E0 0x38) a po puštění AltGr nic nezbude.
    #[test]
    fn snimek_s_altgr_neprevezme_falesny_ctrl() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        let _ = s_stavem(|s| s.engine.toggle(ted_ms()));
        assert_eq!(rezim(), Mode::Keyboard);
        podvrhni_os(altgr_drzi);
        s_stavem(zapomen_klavesnici);
        dokonci_snimek(true);
        assert_eq!(
            s_stavem(|s| s.engine.held(KeyId::LEFT_CTRL)),
            None,
            "falešný Ctrl se nepřevzal"
        );
        assert_eq!(
            s_stavem(|s| s.engine.held(KeyId::RIGHT_ALT).map(|h| h.owner)),
            Some(Owner::Os)
        );
        podvrhni_os(nesmi_se_ptat);
        assert!(!zavolej(0, WM_KEYUP, &kb_vk(0x21D, LLKHF_UP.0, 0xA2)));
        assert!(!zavolej(
            0,
            WM_KEYUP,
            &kb_vk(0x38, LLKHF_EXTENDED.0 | LLKHF_UP.0, 0xA5)
        ));
        assert_eq!(s_stavem(|s| s.engine.held_len()), 0, "po AltGr nic nezbylo");
        // Přiřazování (i bez srovnání při kliku) vezme F a Esc ho zruší.
        let cil = PadAction::first(Action::Button(PadButton::B));
        let _ = s_stavem(|s| s.engine.start_binding(cil, BindKind::Replace, ted_ms()));
        assert!(klavesa(0x21, true), "F se přiřadí");
        assert!(matches!(
            z.posledni().1.ui,
            Some(UiEvent::BindingSaved { key, .. }) if key == KeyId::F
        ));
        assert!(klavesa(0x21, false));
        let _ = s_stavem(|s| s.engine.start_binding(cil, BindKind::Replace, ted_ms()));
        assert!(klavesa(0x01, true), "Esc zruší");
        assert_eq!(
            z.posledni().1.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Escape
            })
        );
        assert!(klavesa(0x01, false));
    }

    /// OS drží Enter (VK_RETURN — hlavní i numerický) a šipku nahoru
    /// (VK_UP — i 8 na numerické klávesnici s vypnutým NumLockem).
    fn enter_a_sipka(vk: u32) -> bool {
        vk == 0x0D || vk == 0x26
    }

    /// OS drží levý Ctrl, Enter a šipku nahoru.
    fn lctrl_enter_a_sipka(vk: u32) -> bool {
        vk == 0xA2 || enter_a_sipka(vk)
    }

    /// Revize opravy 6. 10. (OQ 39, 55): key-upy levého Ctrl, Shiftu a Esc
    /// hook neviděl (Ctrl+Shift+Esc → Správce úloh s právy správce, bez
    /// přepnutí plochy). Záznamy OS nezastarají — bez srovnání by F
    /// patřila Windows (Ctrl+F) a Esc byl „autorepeat", nic by se
    /// nepřiřadilo ani nezrušilo. Klik na čepičku srovná s Windows: co
    /// nedrží, zapomene; co drží, nechá — i klávesu, kterou Windows vedou
    /// pod jinou identitou (numerický Enter je pro ně VK_RETURN, 8 na
    /// numerické klávesnici s vypnutým NumLockem VK_UP = šipka s E0).
    #[test]
    fn klik_na_cepicku_zapomene_klavesy_ktere_windows_nedrzi() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        let _ = s_stavem(|s| s.engine.toggle(ted_ms()));
        assert_eq!(rezim(), Mode::Keyboard, "pauza se zapnutým ovladačem");
        podvrhni_os(nesmi_se_ptat);
        assert!(!zavolej(0, WM_KEYDOWN, &kb_vk(0x1D, 0, 0xA2)));
        assert!(!zavolej(0, WM_KEYDOWN, &kb_vk(0x2A, 0, 0xA0)));
        assert!(!zavolej(0, WM_KEYDOWN, &kb_vk(0x01, 0, 0x1B)));
        let numenter = |flags: u32| kb_vk(0x1C, LLKHF_EXTENDED.0 | flags, 0x0D);
        let num8 = |flags: u32| kb_vk(0x48, flags, 0x26);
        assert!(!zavolej(0, WM_KEYDOWN, &numenter(0)));
        assert!(!zavolej(0, WM_KEYDOWN, &num8(0)));
        let drzi = |k: KeyId| s_stavem(|s| s.engine.held(k).map(|h| h.owner));
        nastav_popredi(true);
        podvrhni_os(lctrl_enter_a_sipka);
        okno(Some(7), true);
        let cil = PadAction::first(Action::Button(PadButton::B));
        let prirad = |os: fn(u32) -> bool| {
            podvrhni_os(os);
            proved(HookPrikaz::Prirad {
                cil,
                druh: BindKind::Replace,
            });
            podvrhni_os(nesmi_se_ptat);
        };
        // Windows levý Ctrl drží: zůstane a F je zkratka Windows.
        prirad(lctrl_enter_a_sipka);
        assert_eq!(drzi(KeyId::LEFT_CTRL), Some(Owner::Os));
        assert_eq!((drzi(KeyId::LEFT_SHIFT), drzi(KeyId::ESC)), (None, None));
        assert_eq!(drzi(KeyId::ext(0x1C)), Some(Owner::Os), "numerický Enter");
        assert_eq!(drzi(KeyId::NUMPAD_8), Some(Owner::Os), "8 na numerické");
        assert!(!klavesa(0x21, true), "Ctrl+F Windows");
        assert_eq!(z.posledni().1.ui, None);
        assert!(!klavesa(0x21, false));
        assert!(matches!(rezim(), Mode::Binding { .. }));
        proved(HookPrikaz::ZrusPrirazeni);
        // Ctrl už Windows nedrží (key-up šel jen Správci úloh).
        prirad(enter_a_sipka);
        assert_eq!(drzi(KeyId::LEFT_CTRL), None);
        assert_eq!(
            (drzi(KeyId::ext(0x1C)), drzi(KeyId::NUMPAD_8)),
            (Some(Owner::Os), Some(Owner::Os)),
            "držené pod jinou identitou zůstaly"
        );
        assert!(klavesa(0x21, true), "F se přiřadí");
        assert!(
            matches!(z.posledni().1.ui, Some(UiEvent::BindingSaved { key, target, .. }) if key == KeyId::F && target == cil),
            "{:?}",
            z.posledni().1.ui
        );
        assert!(klavesa(0x21, false));
        prirad(enter_a_sipka);
        assert!(klavesa(0x01, true), "Esc zruší");
        assert_eq!(
            z.posledni().1.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Escape
            })
        );
        assert!(klavesa(0x01, false));
        assert_eq!(rezim(), Mode::Keyboard);
        // Snímek převzal i hlavní Enter a šipku (VK je jedna, OQ 38). Po
        // puštění numerických kláves je další srovnání zapomene.
        assert_eq!(
            (drzi(KeyId::ENTER), drzi(KeyId::ARROW_UP)),
            (Some(Owner::Os), Some(Owner::Os))
        );
        assert!(!zavolej(0, WM_KEYUP, &numenter(LLKHF_UP.0)));
        assert!(!zavolej(0, WM_KEYUP, &num8(LLKHF_UP.0)));
        prirad(nic_nedrzi);
        assert_eq!(s_stavem(|s| s.engine.held_len()), 0, "nic nezůstalo");
    }

    /// Diagnostika přiřazování (B): počty nepřiřazených stisků podle
    /// příčiny (autorepeat jen jednou) a důvod konce — do logu jde až ze
    /// smyčky, nikdy identita klávesy.
    #[test]
    fn diagnostika_prirazovani() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        nastav_popredi(true);
        okno(Some(7), true);
        let b = PadAction::first(Action::Button(PadButton::B));
        // Klik na čepičku se Windows ptá (srovnání ve smyčce) — nic
        // nedrží; callback se ptát nesmí.
        let prirad = |cil| {
            podvrhni_os(nic_nedrzi);
            proved(HookPrikaz::Prirad {
                cil,
                druh: BindKind::Replace,
            });
            podvrhni_os(nesmi_se_ptat);
        };
        // F24 drží Windows (převzatá snímkem; srovnání při kliku ji nechá).
        podvrhni_os(jen_f24);
        s_stavem(zapomen_klavesnici);
        dokonci_snimek(true);
        proved(HookPrikaz::Prirad {
            cil: b,
            druh: BindKind::Replace,
        });
        podvrhni_os(nesmi_se_ptat);
        assert!(s_stavem(|s| s.prirazovani.is_some()));
        // Levý Ctrl (modifikátor), F s drženým Ctrl, autorepeat F.
        assert!(!zavolej(0, WM_KEYDOWN, &kb_vk(0x1D, 0, 0xA2)));
        assert!(!zavolej(0, WM_KEYDOWN, &kb_vk(0x21, 0, 0x46)));
        assert!(!zavolej(0, WM_KEYDOWN, &kb_vk(0x21, 0, 0x46)));
        assert!(!zavolej(0, WM_KEYUP, &kb_vk(0x21, LLKHF_UP.0, 0x46)));
        assert!(!zavolej(0, WM_KEYUP, &kb_vk(0x1D, LLKHF_UP.0, 0xA2)));
        // Win (dvakrát — autorepeat), Win+L.
        assert!(!win(false, true));
        assert!(!win(false, true));
        assert!(!zavolej(0, WM_KEYDOWN, &kb_l(0)));
        assert!(!zavolej(0, WM_KEYUP, &kb_l(LLKHF_UP.0)));
        assert!(!win(false, false));
        // AltGr (falešný Ctrl 0x21D a pravý Alt), zkratka, vstříknutá,
        // F24 držená Windows.
        assert!(!zavolej(0, WM_KEYDOWN, &kb_vk(0x21D, 0, 0xA2)));
        assert!(!zavolej(
            0,
            WM_KEYDOWN,
            &kb_vk(0x38, LLKHF_EXTENDED.0, 0xA5)
        ));
        assert!(!zavolej(0, WM_KEYUP, &kb_vk(0x21D, LLKHF_UP.0, 0xA2)));
        assert!(!zavolej(
            0,
            WM_KEYUP,
            &kb_vk(0x38, LLKHF_EXTENDED.0 | LLKHF_UP.0, 0xA5)
        ));
        assert!(klavesa(0x46, true));
        assert!(klavesa(0x46, false));
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x1E, LLKHF_INJECTED.0)));
        assert!(!zavolej(0, WM_KEYDOWN, &kb_vk(0x76, 0, 0x87)));
        assert!(!zavolej(0, WM_KEYUP, &kb_vk(0x76, LLKHF_UP.0, 0x87)));
        assert!(
            matches!(rezim(), Mode::Binding { .. }),
            "pořád se přiřazuje"
        );
        assert!(klavesa(0x01, true), "Esc");
        assert!(klavesa(0x01, false));
        let konec = s_stavem(|s| s.konec_prirazovani).expect("konec zaznamenán");
        assert_eq!(konec.duvod, Konec::Esc);
        let p = konec.pocty;
        for (n, cekam) in [
            (Neprirazeno::Modifikator, 3), // Ctrl, Ctrl+F, pravý Alt z AltGr
            (Neprirazeno::Win, 1),
            (Neprirazeno::SWin, 1),
            (Neprirazeno::Nemapovatelna, 1),
            (Neprirazeno::Drzena, 1),
            (Neprirazeno::Zkratka, 1),
            (Neprirazeno::Vstrcena, 1),
        ] {
            assert_eq!(p.pocet(n), cekam, "{n:?}");
        }
        assert_eq!(
            p.text(),
            "3× modifikátor, 1× Win, 1× s Win, 1× nemapovatelná, 1× držená Windows, \
             1× zkratka pauzy, 1× vstříknutá"
        );
        zaloguj_prirazovani();
        assert_eq!(s_stavem(|s| s.konec_prirazovani), None, "smyčka zalogovala");
        assert_eq!(Pocty::default().text(), "nic");

        // Uloženo; jiný vstup; vynuceno; limit 10 s; okno.
        let konec_po = |f: &dyn Fn()| {
            f();
            s_stavem(|s| s.konec_prirazovani.take()).map(|k| k.duvod)
        };
        prirad(b);
        assert_eq!(
            konec_po(&|| {
                assert!(klavesa(0x76, true));
            }),
            Some(Konec::Ulozeno)
        );
        assert!(klavesa(0x76, false));
        prirad(b);
        assert_eq!(
            konec_po(&|| prirad(PadAction::first(Action::Button(PadButton::Y)))),
            Some(Konec::Okno),
            "klik na jiný vstup"
        );
        assert_eq!(
            konec_po(&|| s_enginem(|e| e.force_keyboard(ForceReason::Watchdog))),
            Some(Konec::Vynuceno)
        );
        prirad(b);
        assert_eq!(
            konec_po(&|| s_enginem(|e| e.tick(ted_ms() + 11_000))),
            Some(Konec::Limit)
        );
        prirad(b);
        assert_eq!(
            konec_po(&|| proved(HookPrikaz::ZrusPrirazeni)),
            Some(Konec::Okno)
        );
        assert!(s_stavem(|s| s.prirazovani.is_none()));
    }

    /// Stisk klávesy, kterou engine už drží jako klávesu ovladače
    /// (stisknutou za hry před klikem na čepičku), je „už držená" — dřív
    /// zmizel bez počtu. Co by engine odbyl jinak, je „jiné".
    #[test]
    fn uz_drzena_a_jine_se_pocitaji() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        assert!(zavolej(0, WM_KEYDOWN, &kb_vk(0x76, 0, 0x87)), "F24 hraje");
        nastav_popredi(true);
        okno(Some(7), true);
        proved(HookPrikaz::Prirad {
            cil: PadAction::first(Action::Button(PadButton::B)),
            druh: BindKind::Replace,
        });
        podvrhni_os(nesmi_se_ptat);
        assert!(matches!(rezim(), Mode::Binding { .. }));
        // Autorepeat F24 je pro engine opakování klávesy ovladače; počítá
        // se jednou.
        zavolej(0, WM_KEYDOWN, &kb_vk(0x76, 0, 0x87));
        zavolej(0, WM_KEYDOWN, &kb_vk(0x76, 0, 0x87));
        // „Jiné": stisk nikým nedržené klávesy, o kterém engine nic neřekl
        // (podvržené rozhodnutí bez oznámení — skutečný engine tak stisk
        // při přiřazování neodbude, počet je pojistka).
        s_stavem(|s| {
            let u = Udalost {
                klavesa: KeyId::new(0x22),
                scan: 0x22,
                vk: 0x47,
                flags: 0,
                dolu: true,
            };
            eviduj_stisk(s, &u, None, false, &Decision::NONE);
        });
        let p = s_stavem(|s| s.prirazovani.map(|p| p.pocty)).expect("přiřazuje se");
        assert_eq!(p.text(), "1× už držená, 1× jiné");
        assert!(klavesa(0x01, true), "Esc");
        assert!(klavesa(0x01, false));
    }

    /// Vstříknutý Esc (SendInput, klávesnice na obrazovce) hook při
    /// přiřazování jen započte a propustí — do okna tedy dojde a okno
    /// přiřazování zruší samo (`escRusiPrirazeni` → `ZrusPrirazeni`,
    /// Fáze 6c). Backend ho sám nezruší; v logu „okno — nepřiřazeno:
    /// 1× vstříknutá". Tak to popisují komentáře okna i ROADMAP (nález
    /// revize: dřív tvrdily, že do okna dojde jen Esc, který hook nedostal).
    #[test]
    fn vstriknuty_esc_projde_do_okna_a_zrusi_ho_okno() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        nastav_popredi(true);
        okno(Some(7), true);
        proved(HookPrikaz::Prirad {
            cil: PadAction::first(Action::Button(PadButton::B)),
            druh: BindKind::Replace,
        });
        podvrhni_os(nesmi_se_ptat);
        assert!(matches!(rezim(), Mode::Binding { .. }));
        assert!(
            !zavolej(0, WM_KEYDOWN, &kb_vk(0x01, LLKHF_INJECTED.0, 0x1B)),
            "vstříknutý Esc projde do okna"
        );
        assert!(!zavolej(
            0,
            WM_KEYUP,
            &kb_vk(0x01, LLKHF_INJECTED.0 | LLKHF_UP.0, 0x1B)
        ));
        assert!(
            matches!(rezim(), Mode::Binding { .. }),
            "backend vstříknutý Esc nebere jako zrušení"
        );
        assert_eq!(s_stavem(|s| s.konec_prirazovani), None);
        // Okno Esc dostalo a přiřazování zrušilo.
        proved(HookPrikaz::ZrusPrirazeni);
        let k = s_stavem(|s| s.konec_prirazovani).expect("konec zaznamenán");
        assert_eq!(k.duvod, Konec::Okno);
        assert_eq!(k.pocty.text(), "1× vstříknutá");
        assert!(s_stavem(|s| s.prirazovani.is_none()));
    }

    /// Čítače doručení (OQ 60) rostou s každým voláním callbacku — i se
    /// záporným kódem a jinou zprávou —, se stiskem, se souběhem (stav
    /// enginu půjčený) i s rozbitým enginem. Patří celému procesu a testy
    /// běží souběžně, proto „aspoň".
    #[test]
    fn doruceni_pocita_volani_stisky_soubeh_i_rozbity() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        let pred = Doruceni::ted();
        assert!(zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        assert!(zavolej(0, WM_KEYUP, &kb(0x76, LLKHF_UP.0)));
        assert!(!zavolej(-1, WM_KEYDOWN, &kb(0x76, 0)));
        assert!(!zavolej(0, WM_TIMER, &kb(0x76, 0)));
        let d = Doruceni::ted().od(pred);
        assert!(d.volani >= 4 && d.dolu >= 1, "{d:?}");
        // Souběh: zanořené volání najde stav půjčený — klávesa projde.
        let pred = Doruceni::ted();
        STAV.with(|s| {
            let _pujceno = s.borrow_mut();
            assert!(!zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        });
        let d = Doruceni::ted().od(pred);
        assert!(d.volani >= 1 && d.soubeh >= 1, "{d:?}");
        // Rozbitý engine.
        s_stavem(|s| s.rozbity = true);
        let pred = Doruceni::ted();
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        assert!(Doruceni::ted().od(pred).rozbity >= 1);
        // Čítače přetékají dokola.
        let zacatek = Doruceni {
            volani: u32::MAX,
            ..Doruceni::default()
        };
        let konec = Doruceni {
            volani: 2,
            ..Doruceni::default()
        };
        assert_eq!(konec.od(zacatek).volani, 3);
    }

    thread_local! {
        /// Kolikrát se kdo zeptal na Raw Input (podvrh kontroly).
        static RAW_KONTROLY: Cell<u32> = const { Cell::new(0) };
    }

    fn raw_odregistrovano() -> RawInput {
        RAW_KONTROLY.with(|k| k.set(k.get() + 1));
        RawInput::Odregistrovano
    }

    /// Klik na čepičku ověří Raw Input klávesnice (smyčka, ne callback)
    /// a konec přiřazování nese výsledek i doručení za dobu přiřazování
    /// (OQ 60).
    #[test]
    fn prirazovani_overi_raw_input_a_konec_nese_doruceni() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        nastav_popredi(true);
        okno(Some(7), true);
        s_stavem(|s| s.raw_input = raw_odregistrovano);
        RAW_KONTROLY.with(|k| k.set(0));
        proved(HookPrikaz::Prirad {
            cil: PadAction::first(Action::Button(PadButton::B)),
            druh: BindKind::Replace,
        });
        assert_eq!(RAW_KONTROLY.with(Cell::get), 1, "klik Raw Input ověřil");
        assert_eq!(
            s_stavem(|s| s.prirazovani.map(|p| p.raw)),
            Some(RawInput::Odregistrovano)
        );
        // Ctrl+F (zkratka Windows — samotné ťuknutí Ctrl by se přiřadilo),
        // pak Esc.
        assert!(!zavolej(0, WM_KEYDOWN, &kb_vk(0x1D, 0, 0xA2)));
        assert!(!zavolej(0, WM_KEYDOWN, &kb_vk(0x21, 0, 0x46)));
        assert!(!zavolej(0, WM_KEYUP, &kb_vk(0x21, LLKHF_UP.0, 0x46)));
        assert!(!zavolej(0, WM_KEYUP, &kb_vk(0x1D, LLKHF_UP.0, 0xA2)));
        assert!(klavesa(0x01, true), "Esc");
        assert_eq!(
            RAW_KONTROLY.with(Cell::get),
            1,
            "callback se na Raw Input neptá"
        );
        let k = s_stavem(|s| s.konec_prirazovani).expect("konec zaznamenán");
        assert_eq!((k.duvod, k.raw), (Konec::Esc, RawInput::Odregistrovano));
        assert!(
            k.doruceni.volani >= 3 && k.doruceni.dolu >= 2,
            "{:?}",
            k.doruceni
        );
        assert!(klavesa(0x01, false));
        zaloguj_prirazovani();
        assert_eq!(s_stavem(|s| s.konec_prirazovani), None);
    }

    /// Řádek o konci přiřazování: důvod, počty, doručení, Raw Input — nic
    /// jiného (OQ 33). Tvar čte i `tools\okno-test.ps1`.
    #[test]
    fn radek_konce_jen_duvod_a_pocty() {
        let mut pocty = Pocty::default();
        pocty.pridej(Neprirazeno::Win);
        pocty.pridej(Neprirazeno::UzDrzena);
        let k = KonecPrirazovani {
            duvod: Konec::Esc,
            pocty,
            doruceni: Doruceni {
                volani: 12,
                dolu: 5,
                soubeh: 1,
                rozbity: 0,
            },
            raw: RawInput::Ne,
        };
        assert_eq!(
            radek_konce(&k, 0),
            "Esc — nepřiřazeno: 1× Win, 1× už držená · callback 12× (stisků 5, souběh 1, \
             rozbitý 0) · raw input klávesnice: ne"
        );
        let k = KonecPrirazovani {
            duvod: Konec::Okno,
            pocty: Pocty::default(),
            doruceni: Doruceni::default(),
            raw: RawInput::Odregistrovano,
        };
        assert_eq!(
            radek_konce(&k, 2),
            "okno — nepřiřazeno: nic (a 2 dřívějších bez záznamu) · callback 0× (stisků 0, \
             souběh 0, rozbitý 0) · raw input klávesnice: ano — odregistrováno"
        );
    }

    /// Konec přiřazování v callbacku (Esc) smyčku sám neprobudí — callback
    /// nic nepošle. Probudí ji časovač přiřazování: běží, dokud ho smyčka
    /// v další obrátce nezruší, takže zalogování přijde do jednoho tiku.
    #[test]
    fn konec_prirazovani_v_callbacku_probudi_smycku() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        nastav_popredi(true);
        okno(Some(7), true);
        proved(HookPrikaz::Prirad {
            cil: PadAction::first(Action::Button(PadButton::B)),
            druh: BindKind::Replace,
        });
        let casovac = hlidej_casovac(0);
        assert_ne!(casovac, 0, "časovač přiřazování běží");
        assert!(klavesa(0x01, true), "Esc v callbacku");
        assert!(klavesa(0x01, false));
        assert!(s_stavem(|s| s.konec_prirazovani.is_some()));
        // Fronta vlákna dostane WM_TIMER bez další klávesy.
        let konec = std::time::Instant::now() + Duration::from_secs(2);
        let mut msg = MSG::default();
        let mut tik = false;
        while !tik && std::time::Instant::now() < konec {
            // SAFETY: platný ukazatel na MSG; jen zprávy tohoto vlákna.
            while unsafe {
                PeekMessageW(
                    &mut msg,
                    None,
                    0,
                    0,
                    windows::Win32::UI::WindowsAndMessaging::PM_REMOVE,
                )
            }
            .as_bool()
            {
                tik |= msg.message == WM_TIMER;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(tik, "časovač smyčku probudil");
        zaloguj_prirazovani();
        assert_eq!(s_stavem(|s| s.konec_prirazovani), None);
        assert_eq!(hlidej_casovac(casovac), 0, "a pak se zruší");
    }

    /// Nepovedená instalace hooku (spec 2.4, oprava B4): přiřazování —
    /// ze hry i bez ovladače s oknem v popředí — se zruší vynucením,
    /// na Gamepad se nevrací a okno se dozví, že klávesy sledovat nejde.
    /// Zachytávání bez hooku skončí na Klávesnici.
    #[test]
    fn nepovedena_instalace_zrusi_prirazovani_i_zachytavani() {
        let cil = PadAction::first(Action::Button(PadButton::A));
        for (prirazuje, s_ovladacem) in [(true, true), (true, false), (false, true)] {
            let z = Arc::new(Zaznam::default());
            priprav(&z);
            s_stavem(|s| s.instaluj = || Err("zkušební chyba".into()));
            if !s_ovladacem {
                let _ = s_stavem(|s| {
                    s.engine
                        .disable(PadId::FIRST, DisabledReason::PadNotConnected)
                });
                nastav_popredi(true);
                okno(Some(7), true);
            }
            if prirazuje {
                let _ = s_stavem(|s| s.engine.start_binding(cil, BindKind::Replace, ted_ms()));
            }
            let status = HookStatus::default();
            assert!(hlidej_hook(HHOOK::default(), &status).is_invalid());
            assert!(!status.nainstalovan());
            let m = rezim();
            let pripad = format!("přiřazuje {prirazuje}, ovladač {s_ovladacem}: {m:?}");
            assert!(
                !matches!(m, Mode::Binding { .. } | Mode::Gamepad),
                "{pripad}"
            );
            assert_eq!(matches!(m, Mode::Disabled { .. }), !s_ovladacem, "{pripad}");
            let cekane = if prirazuje {
                UiEvent::BindingCancelled {
                    reason: BindingCancel::Forced(ForceReason::HookReinstalled),
                }
            } else {
                UiEvent::ModeChanged {
                    mode: Mode::Keyboard,
                    cause: ModeCause::Forced(ForceReason::HookReinstalled),
                }
            };
            assert_eq!(z.posledni().1.ui, Some(cekane), "{pripad}");
            assert_eq!(z.hook_chyba.lock().unwrap().last(), Some(&true), "{pripad}");
        }
    }

    /// Alt+Tab z okna při přiřazování (kontrolní seznam vlastníka, bod
    /// 10; OQ 55): Alt i Tab jdou do Windows, ty přepnou okno a ztráta
    /// popředí přiřazování zruší — mapování beze změny. Samotné ťuknutí
    /// Altem se přiřadí při uvolnění.
    #[test]
    fn alt_tab_pri_prirazovani_ho_zrusi_a_tuknuti_altem_priradi() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        let cil = PadAction::first(Action::Button(PadButton::A));
        nastav_popredi(true);
        okno(Some(7), true);
        proved(HookPrikaz::Prirad {
            cil,
            druh: BindKind::Replace,
        });
        assert!(matches!(rezim(), Mode::Binding { .. }));
        let pred = mapovani_enginu();
        assert!(!alt(true), "Alt Windows");
        assert!(
            !zavolej(0, WM_SYSKEYDOWN, &kb_vk(0x0F, 0, 0x09)),
            "Tab Windows"
        );
        assert!(matches!(rezim(), Mode::Binding { .. }), "nic nepřiřazeno");
        nastav_popredi(false);
        zmena_popredi();
        assert_eq!(
            z.posledni().1.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Gui
            })
        );
        assert_eq!(rezim(), Mode::Gamepad, "zpět do hry");
        assert!(!zavolej(0, WM_KEYUP, &kb_vk(0x0F, LLKHF_UP.0, 0x09)));
        assert!(!alt(false), "key-up Altu Windows jako key-down");
        assert_eq!(mapovani_enginu(), pred);

        nastav_popredi(true);
        zmena_popredi();
        proved(HookPrikaz::Prirad {
            cil,
            druh: BindKind::Replace,
        });
        assert!(!alt(true));
        assert!(matches!(rezim(), Mode::Binding { .. }));
        assert!(!alt(false));
        assert_eq!(
            z.posledni().1.ui,
            Some(UiEvent::BindingSaved {
                key: KeyId::LEFT_ALT,
                target: cil,
                moved_from: None
            })
        );
        assert_eq!(mapovani_enginu().target(KeyId::LEFT_ALT), Some(cil));
    }

    /// Bez hlídaného popředí (WinEvent se nepodařilo zaregistrovat) se
    /// klik na čepičku nepřijme, i když okno v popředí je — nic by
    /// přiřazování při ztrátě popředí nezrušilo a spolklo by klávesu
    /// napsanou v jiném programu.
    #[test]
    fn prirad_bez_hlidaneho_popredi_se_neprijme() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        let prirad = || {
            proved(HookPrikaz::Prirad {
                cil: PadAction::first(Action::Button(PadButton::Y)),
                druh: BindKind::Replace,
            })
        };
        nastav_popredi(true);
        okno(Some(7), false);
        prirad();
        assert_eq!(rezim(), Mode::Gamepad, "klik se nepřijal");
        assert!(!s_stavem(|s| s.okno_aktivni));
        // S hlídáním ano.
        okno(Some(7), true);
        prirad();
        assert!(matches!(rezim(), Mode::Binding { .. }));
        // Okno ukázané znovu bez hlídání běžící přiřazování zruší.
        okno(Some(7), false);
        assert_eq!(rezim(), Mode::Gamepad);
        assert_eq!(
            z.posledni().1.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Gui
            })
        );
    }

    /// Zpráva o aktivaci okna dojde až po přijetí kliku (okno bylo vidět
    /// na pozadí, klik ho aktivoval): přijetí samo zapíše „aktivní",
    /// takže následná ztráta popředí přiřazování zruší. A ruší ho každá
    /// událost bez popředí, i když nevypadá jako přechod.
    #[test]
    fn ztrata_popredi_zrusi_prirazovani_i_bez_zpravy_o_aktivaci() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        let cil = PadAction::first(Action::Button(PadButton::X));
        nastav_popredi(false);
        okno(Some(7), true);
        assert!(!s_stavem(|s| s.okno_aktivni));
        nastav_popredi(true);
        proved(HookPrikaz::Prirad {
            cil,
            druh: BindKind::Replace,
        });
        assert!(matches!(rezim(), Mode::Binding { .. }));
        assert!(s_stavem(|s| s.okno_aktivni), "přijatý klik = aktivní");
        nastav_popredi(false);
        zmena_popredi();
        assert_eq!(rezim(), Mode::Gamepad, "ztráta popředí zrušila");
        // Přiřazování, o jehož popředí smyčka neví (jen pojistka).
        let _ = s_stavem(|s| s.engine.start_binding(cil, BindKind::Replace, ted_ms()));
        assert!(!s_stavem(|s| s.okno_aktivni));
        zmena_popredi();
        assert_eq!(rezim(), Mode::Gamepad, "zrušeno i bez přechodu");
        assert_eq!(
            z.posledni().1.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Gui
            })
        );
    }

    /// Kdy má být hook v systému (spec 2.4): se zapnutým ovladačem
    /// a při přiřazování vždy, bez ovladače jen s oknem v popředí,
    /// s rozbitým enginem nikdy.
    #[test]
    fn potreba_hooku_tabulkou() {
        let binding = Mode::Binding {
            target: PadAction::first(Action::Button(PadButton::A)),
            started_at_ms: 0,
        };
        for (rezim, bez_okna, s_oknem) in [
            (None, false, false),
            (
                Some(Mode::Disabled {
                    reason: DisabledReason::PadNotConnected,
                }),
                false,
                true,
            ),
            (
                Some(Mode::Disabled {
                    reason: DisabledReason::ViGEmMissing,
                }),
                false,
                true,
            ),
            (
                Some(Mode::Disabled {
                    reason: DisabledReason::PadError,
                }),
                false,
                true,
            ),
            (Some(Mode::Keyboard), true, true),
            (Some(Mode::Gamepad), true, true),
            (Some(binding), true, true),
        ] {
            assert_eq!(potreba_hooku(rezim, false), bez_okna, "{rezim:?} bez okna");
            assert_eq!(potreba_hooku(rezim, true), s_oknem, "{rezim:?} s oknem");
        }
    }

    /// Klik na čepičku se přijme, jen když je okno V TU CHVÍLI
    /// v popředí — ne podle poslední události ani jen proto, že je vidět.
    #[test]
    fn prirad_jen_s_oknem_v_popredi() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        let cil = PadAction::new(PadId::new(1).unwrap(), Action::Button(PadButton::B));
        let prirad = || {
            proved(HookPrikaz::Prirad {
                cil,
                druh: BindKind::Add,
            })
        };
        // Okno není vidět.
        nastav_popredi(true);
        prirad();
        assert_eq!(rezim(), Mode::Gamepad);
        // Okno je vidět a poslední událost tvrdí „v popředí", teď už ale
        // v popředí není.
        okno(Some(7), true);
        assert!(s_stavem(|s| s.okno_aktivni));
        nastav_popredi(false);
        prirad();
        assert_eq!(rezim(), Mode::Gamepad, "okno na pozadí klik nepřijme");
        nastav_popredi(true);
        prirad();
        assert!(
            matches!(rezim(), Mode::Binding { target, .. } if target == cil),
            "{:?}",
            rezim()
        );
        assert_eq!(s_stavem(|s| s.engine.binding_kind()), Some(BindKind::Add));
        // Zrušení z okna: zpět do hry.
        proved(HookPrikaz::ZrusPrirazeni);
        assert_eq!(rezim(), Mode::Gamepad);
        assert_eq!(
            z.posledni().1.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Gui
            })
        );
    }

    /// Okno ztratí popředí během přiřazování (Alt+Tab do chatu): zrušeno
    /// jako z okna, smyčka dostane `WM_POPREDI` a živý stav se přepočítá
    /// bez kláves Windows. Návrat popředí přiřazování neobnoví.
    #[test]
    fn ztrata_popredi_zrusi_prirazovani() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        nastav_popredi(true);
        okno(Some(7), true);
        proved(HookPrikaz::Prirad {
            cil: PadAction::first(Action::Button(PadButton::X)),
            druh: BindKind::Replace,
        });
        assert!(matches!(rezim(), Mode::Binding { .. }));
        let zive_pred = z.zive.lock().unwrap().len();
        nastav_popredi(false);
        zmena_popredi();
        let (_, d, m) = z.posledni();
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Gui
            })
        );
        assert_eq!(m, Mode::Gamepad, "zpět do hry");
        assert!(!s_stavem(|s| s.okno_aktivni));
        assert!(
            z.zive.lock().unwrap().len() > zive_pred,
            "živý stav přepočítán"
        );
        // Bez změny popředí se nic neděje.
        let n = z.rozhodnuti.lock().unwrap().len();
        zmena_popredi();
        assert_eq!(z.rozhodnuti.lock().unwrap().len(), n);
        // Návrat popředí: jen živý stav, přiřazování ne.
        nastav_popredi(true);
        zmena_popredi();
        assert!(s_stavem(|s| s.okno_aktivni));
        assert_eq!(rezim(), Mode::Gamepad);
        // Schované okno popředí neřeší.
        okno(None, true);
        nastav_popredi(false);
        let n = z.rozhodnuti.lock().unwrap().len();
        zmena_popredi();
        assert_eq!(z.rozhodnuti.lock().unwrap().len(), n);
    }

    /// Klávesy Windows (vlastník `Os`) jsou vidět jen s oknem v popředí;
    /// s oknem na pozadí jen klávesy hry. Bez okna se živý stav vůbec
    /// nepočítá.
    #[test]
    fn zive_klavesy_windows_jen_v_popredi() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        // Pozastaveno: W patří Windows.
        let _ = s_stavem(|s| s.engine.toggle(0));
        assert_eq!(rezim(), Mode::Keyboard);
        let w = |d: bool| {
            zavolej(
                0,
                if d { WM_KEYDOWN } else { WM_KEYUP },
                &kb(0x11, if d { 0 } else { LLKHF_UP.0 }),
            )
        };
        assert!(!w(true));
        assert_eq!(z.posledni_zive(), None, "bez okna se nepočítá");
        assert!(!w(false));
        nastav_popredi(false);
        okno(Some(9), true);
        assert!(!w(true));
        assert_eq!(
            z.posledni_zive().unwrap()[0],
            LiveInputs::EMPTY,
            "okno na pozadí"
        );
        assert!(!w(false));
        nastav_popredi(true);
        okno(Some(9), true);
        assert!(!w(true));
        let l = z.posledni_zive().unwrap()[0];
        assert!(l.held().contains(Action::LeftStick(StickDir::Up)));
        assert_eq!(l.left_stick(), (0, 1));
        assert!(!w(false));
    }

    /// Schované okno: přiřazování zrušené, živý stav vynulovaný a další
    /// klávesy ho už nepočítají.
    #[test]
    fn schovane_okno_nuluje_zivy_stav_a_rusi_prirazovani() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        nastav_popredi(true);
        okno(Some(7), true);
        assert!(zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        let a = z.posledni_zive().unwrap()[0];
        assert!(a.held().contains(Action::Button(PadButton::A)));
        proved(HookPrikaz::Prirad {
            cil: PadAction::first(Action::Button(PadButton::Y)),
            druh: BindKind::Replace,
        });
        assert!(matches!(rezim(), Mode::Binding { .. }));
        okno(None, true);
        assert_eq!(z.posledni_zive(), Some([LiveInputs::EMPTY; MAX_PADS]));
        assert_eq!(
            z.posledni().1.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Gui
            })
        );
        assert_eq!(rezim(), Mode::Gamepad);
        assert_eq!(s_stavem(|s| (s.okno, s.okno_aktivni)), (None, false));
        let n = z.zive.lock().unwrap().len();
        zavolej(0, WM_KEYUP, &kb(0x76, LLKHF_UP.0));
        assert_eq!(
            z.zive.lock().unwrap().len(),
            n,
            "bez okna se živý stav nepočítá"
        );
        // Bez hlídaného popředí se okno nikdy nebere jako aktivní.
        okno(Some(7), false);
        assert!(!s_stavem(|s| s.okno_aktivni));
    }

    fn uprav_s(zmena: Zmena) -> Result<(), ChybaUpravy> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        proved(HookPrikaz::Uprav { zmena, odpoved: tx });
        rx.try_recv().expect("odpověď hned")
    }

    fn mapovani_enginu() -> Mapping {
        s_stavem(|s| s.engine.mapping().clone())
    }

    fn revize_enginu() -> u64 {
        s_stavem(|s| s.engine.mapping_rev())
    }

    /// Úpravy z editoru: každá změna, prázdné nic nemění, „Zpět" jen
    /// z aktuální revize, poslední klávesu nejde odebrat — a režim hry
    /// zůstává (bez pozastavení, OQ 43).
    #[test]
    fn uprav_vsechny_zmeny() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        let x = PadAction::first(Action::Button(PadButton::X));
        let p2 = PadId::new(1).unwrap();
        let num8 = PadAction::new(p2, Action::LeftStick(StickDir::Up));

        assert_eq!(uprav_s(Zmena::VyprazdniVstup(x)), Ok(()));
        assert_eq!(mapovani_enginu().target(KeyId::F), None);
        assert_eq!((revize_enginu(), rezim()), (1, Mode::Gamepad));
        assert_eq!(z.revize.load(Ordering::Acquire), 1, "revize jde oknu");
        // Prázdný vstup znovu: v pořádku, revize stojí.
        assert_eq!(uprav_s(Zmena::VyprazdniVstup(x)), Ok(()));
        assert_eq!(revize_enginu(), 1);

        assert_eq!(uprav_s(Zmena::VychoziPrvni), Ok(()));
        assert_eq!(mapovani_enginu().target(KeyId::F), Some(x));
        assert_eq!(revize_enginu(), 2);

        let mut s_druhym = mapovani_enginu();
        s_druhym.bind(KeyId::NUMPAD_8, num8).unwrap();
        assert_eq!(
            uprav_s(Zmena::Obnov {
                mapovani: Box::new(s_druhym.clone()),
                kdyz_revize: 2
            }),
            Ok(())
        );
        assert_eq!(mapovani_enginu(), s_druhym);
        assert_eq!(revize_enginu(), 3);

        assert_eq!(uprav_s(Zmena::VymazOvladac(p2)), Ok(()));
        assert_eq!(mapovani_enginu().target(KeyId::NUMPAD_8), None);
        assert_eq!(revize_enginu(), 4);

        // „Zpět" ze staré revize: nic se nemění.
        assert_eq!(
            uprav_s(Zmena::Obnov {
                mapovani: Box::new(s_druhym),
                kdyz_revize: 3
            }),
            Err(ChybaUpravy::Zastarale)
        );
        assert_eq!(revize_enginu(), 4);
        assert_eq!(mapovani_enginu().target(KeyId::NUMPAD_8), None);

        // Poslední klávesa.
        let jedina = Mapping::new(KeyId::SCROLL_LOCK, [(KeyId::F, x)]).unwrap();
        assert_eq!(
            uprav_s(Zmena::Obnov {
                mapovani: Box::new(jedina.clone()),
                kdyz_revize: 4
            }),
            Ok(())
        );
        assert_eq!(
            uprav_s(Zmena::VyprazdniVstup(x)),
            Err(ChybaUpravy::Mapovani(MappingError::WouldBeEmpty))
        );
        assert_eq!(
            uprav_s(Zmena::VymazOvladac(PadId::FIRST)),
            Err(ChybaUpravy::Mapovani(MappingError::WouldBeEmpty))
        );
        assert_eq!(mapovani_enginu(), jedina);
        assert_eq!(rezim(), Mode::Gamepad, "úpravy hru nepozastavují");

        // Úprava během přiřazování ho zruší (cíl mohl zmizet).
        nastav_popredi(true);
        okno(Some(3), true);
        proved(HookPrikaz::Prirad {
            cil: x,
            druh: BindKind::Replace,
        });
        assert!(matches!(rezim(), Mode::Binding { .. }));
        assert_eq!(uprav_s(Zmena::VychoziPrvni), Ok(()));
        assert_eq!(rezim(), Mode::Gamepad);
        assert!(z.rozhodnuti.lock().unwrap().iter().any(|r| r.1.ui
            == Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Gui
            })));

        // Rozbitý engine neodpoví — příkaz okna skončí chybou kanálu.
        s_stavem(|s| s.rozbity = true);
        let (tx, rx) = crossbeam_channel::bounded(1);
        proved(HookPrikaz::Uprav {
            zmena: Zmena::VychoziPrvni,
            odpoved: tx,
        });
        assert_eq!(
            rx.try_recv(),
            Err(crossbeam_channel::TryRecvError::Disconnected)
        );
    }

    /// Snímek mapování pro okno pošle jen povel `Zverejni` (smyčka, klon),
    /// nikdy callback.
    #[test]
    fn zverejni_posle_snimek_mapovani() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        proved(HookPrikaz::Zverejni);
        assert_eq!(*z.mapovani.lock().unwrap(), vec![(0, Mapping::default())]);
        let x = PadAction::first(Action::Button(PadButton::X));
        assert_eq!(uprav_s(Zmena::VyprazdniVstup(x)), Ok(()));
        assert_eq!(z.mapovani.lock().unwrap().len(), 1, "úprava snímek nepošle");
        proved(HookPrikaz::Zverejni);
        let (rev, m) = z.mapovani.lock().unwrap().last().cloned().unwrap();
        assert_eq!((rev, m.target(KeyId::F)), (1, None));
        // Přiřazení klávesy v callbacku: jen revize, snímek ne.
        nastav_popredi(true);
        okno(Some(3), true);
        proved(HookPrikaz::Prirad {
            cil: x,
            druh: BindKind::Replace,
        });
        assert!(zavolej(0, WM_KEYDOWN, &kb(0x21, 0)));
        assert!(zavolej(0, WM_KEYUP, &kb(0x21, LLKHF_UP.0)));
        assert_eq!(z.revize.load(Ordering::Acquire), 2);
        assert_eq!(z.mapovani.lock().unwrap().len(), 2);
    }

    /// Syntetická klávesa testů jde touž cestou jako callback: stejná
    /// rozhodnutí, stejné režimy, stejné potlačení.
    #[cfg(debug_assertions)]
    #[test]
    fn test_klavesa_jako_callback() {
        // F24 dolů/nahoru, W (nenamapovaná), šipka nahoru (E0), Win,
        // zkratka F23 dvakrát (pauza a zpět), F24 při pauze.
        let udalosti: [(u16, bool, bool); 10] = [
            (0x76, false, true),
            (0x76, false, true),
            (0x76, false, false),
            (0x11, false, true),
            (0x48, true, true),
            (0x5B, true, true),
            (0x6E, false, true),
            (0x6E, false, false),
            (0x76, false, true),
            (0x76, false, false),
        ];
        let callbackem = {
            let z = Arc::new(Zaznam::default());
            priprav(&z);
            udalosti.map(|(scan, e0, dolu)| {
                let flags =
                    if e0 { LLKHF_EXTENDED.0 } else { 0 } | if dolu { 0 } else { LLKHF_UP.0 };
                let zprava = if dolu { WM_KEYDOWN } else { WM_KEYUP };
                let p = zavolej(0, zprava, &kb(u32::from(scan), flags));
                let (u, d, m) = z.posledni();
                let u = u.unwrap();
                (p, u.klavesa, u.dolu, d, m)
            })
        };
        let testem = {
            let z = Arc::new(Zaznam::default());
            priprav(&z);
            udalosti.map(|(scan, e0, dolu)| {
                let klavesa = KeyId { scan, extended: e0 };
                proved(HookPrikaz::TestKlavesa {
                    klavesa,
                    vk: 0,
                    dolu,
                });
                let (u, d, m) = z.posledni();
                let u = u.unwrap();
                (d.suppress, u.klavesa, u.dolu, d, m)
            })
        };
        assert_eq!(callbackem, testem);
    }

    /// Skutečný hook na skryté ploše: bez zapnutého ovladače v systému
    /// není, s prvním povoleným se nainstaluje, zachytávání z pozastavení
    /// ho přeinstaluje, s posledním zakázaným zmizí; konec pošle neutrál.
    /// Mapování jen na F23/F24 — na ploše vlastníka nic nezmění.
    #[test]
    fn hook_jen_se_zapnutym_ovladacem() {
        let z = Arc::new(Zaznam::default());
        let vystup: Arc<dyn Vystup> = z.clone();
        let mut hook = spust_s(
            mapovani(),
            vystup,
            testy_plocha::na_skryte_plose,
            je_v_popredi,
        )
        .unwrap();
        let st = Arc::clone(hook.status());
        assert!(!st.nainstalovan(), "bez ovladače hook v systému není");
        assert_eq!(st.instalaci(), 0);

        let pockej = |podminka: &dyn Fn(&Radek) -> bool| {
            let konec = std::time::Instant::now() + LIMIT;
            loop {
                if z.rozhodnuti.lock().unwrap().iter().any(podminka) {
                    return;
                }
                assert!(std::time::Instant::now() < konec, "vlákno neodpovědělo");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let p2 = PadId::new(1).unwrap();

        // Přeinstalace bez hooku v systému nic neinstaluje.
        assert!(hook.posli(HookPrikaz::Preinstaluj));
        assert!(hook.posli(HookPrikaz::Povol(PadId::FIRST)));
        pockej(&|r| r.2 == Mode::Keyboard);
        pockej_na(&|| st.nainstalovan());
        assert_eq!(st.instalaci(), 1);

        // Zapnutí přepínačem z pozastavení: hook znovu a zachytávat.
        assert!(hook.posli(HookPrikaz::Zachytavej));
        pockej(&|r| r.2 == Mode::Gamepad);
        pockej_na(&|| st.instalaci() == 2);
        // Druhý ovladač při běžícím zachytávání hook nepřeinstaluje.
        assert!(hook.posli(HookPrikaz::Povol(p2)));
        assert!(hook.posli(HookPrikaz::Zachytavej));
        assert!(hook.posli(HookPrikaz::Preinstaluj));
        pockej(&|r| {
            r.1.ui
                == Some(UiEvent::ModeChanged {
                    mode: Mode::Keyboard,
                    cause: ModeCause::Forced(ForceReason::HookReinstalled),
                })
        });
        pockej_na(&|| st.instalaci() == 3);
        assert!(st.nainstalovan());

        // Vypnutý jeden ovladač hook nechá, vypnutý poslední ho odebere.
        assert!(hook.posli(HookPrikaz::Zakaz(
            PadId::FIRST,
            DisabledReason::PadNotConnected
        )));
        assert!(hook.posli(HookPrikaz::Prepni));
        pockej(&|r| r.2 == Mode::Gamepad && r.0.is_none());
        assert!(st.nainstalovan());
        assert!(hook.posli(HookPrikaz::Zakaz(p2, DisabledReason::PadNotConnected)));
        pockej(&|r| matches!(r.2, Mode::Disabled { .. }));
        pockej_na(&|| !st.nainstalovan());
        assert_eq!(st.instalaci(), 3);

        assert!(hook.zastav());
        assert!(!st.nainstalovan());
        let (u, d, m) = z.posledni();
        assert_eq!(
            (u, d.pads.get(PadId::FIRST)),
            (None, Some(PadState::NEUTRAL))
        );
        assert!(matches!(m, Mode::Disabled { .. }));
        // Po konci vlákna příkazy neprojdou.
        assert!(!hook.posli(HookPrikaz::Prepni));
        assert!(hook.zastav(), "druhé zastavení nic nedělá");
    }

    fn pockej_na(co: &dyn Fn() -> bool) {
        let konec = std::time::Instant::now() + LIMIT;
        while !co() {
            assert!(std::time::Instant::now() < konec, "vlákno neodpovědělo");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Popředí okna pro hook vlákno testu na skryté ploše (fn ukazatel
    /// nic nezachytí — proto statika, každý test vlastní).
    static POPREDI_BEZ_OVLADACE: AtomicBool = AtomicBool::new(false);
    static POPREDI_S_OVLADACEM: AtomicBool = AtomicBool::new(false);

    fn popredi_bez_ovladace(_hwnd: isize) -> bool {
        POPREDI_BEZ_OVLADACE.load(Ordering::Acquire)
    }

    fn popredi_s_ovladacem(_hwnd: isize) -> bool {
        POPREDI_S_OVLADACEM.load(Ordering::Acquire)
    }

    /// Skutečný hook na skryté ploše bez zapnutého ovladače: okno
    /// v popředí ho nainstaluje (živé klávesy), okno na pozadí nebo
    /// schované ho zase odebere.
    #[test]
    fn hook_s_oknem_v_popredi_bez_ovladace() {
        let z = Arc::new(Zaznam::default());
        let vystup: Arc<dyn Vystup> = z.clone();
        let mut hook = spust_s(
            mapovani(),
            vystup,
            testy_plocha::na_skryte_plose,
            popredi_bez_ovladace,
        )
        .unwrap();
        let st = Arc::clone(hook.status());
        POPREDI_BEZ_OVLADACE.store(true, Ordering::Release);
        assert!(hook.posli(HookPrikaz::Okno(Some(42))));
        pockej_na(&|| st.nainstalovan());
        assert_eq!(st.instalaci(), 1);
        // Okno na pozadí.
        POPREDI_BEZ_OVLADACE.store(false, Ordering::Release);
        assert!(hook.posli(HookPrikaz::Okno(Some(42))));
        pockej_na(&|| !st.nainstalovan());
        // Zase v popředí, pak schované.
        POPREDI_BEZ_OVLADACE.store(true, Ordering::Release);
        assert!(hook.posli(HookPrikaz::Okno(Some(42))));
        pockej_na(&|| st.instalaci() == 2 && st.nainstalovan());
        assert!(hook.posli(HookPrikaz::Okno(None)));
        pockej_na(&|| !st.nainstalovan());
        assert_eq!(z.posledni_zive(), Some([LiveInputs::EMPTY; MAX_PADS]));
        assert!(hook.zastav());
    }

    /// Se zapnutým ovladačem popředí okna na hook nesahá — klávesy hráče
    /// se nesmí ztratit přeinstalací při každém Alt+Tab.
    #[test]
    fn se_zapnutym_ovladacem_popredi_hook_nemeni() {
        let z = Arc::new(Zaznam::default());
        let vystup: Arc<dyn Vystup> = z.clone();
        let mut hook = spust_s(
            mapovani(),
            vystup,
            testy_plocha::na_skryte_plose,
            popredi_s_ovladacem,
        )
        .unwrap();
        let st = Arc::clone(hook.status());
        assert!(hook.posli(HookPrikaz::Povol(PadId::FIRST)));
        assert!(hook.posli(HookPrikaz::Zachytavej));
        pockej_na(&|| {
            z.rozhodnuti
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.2 == Mode::Gamepad)
        });
        pockej_na(&|| st.nainstalovan());
        let n = st.instalaci();
        for i in 0..4 {
            POPREDI_S_OVLADACEM.store(i % 2 == 0, Ordering::Release);
            assert!(hook.posli(HookPrikaz::Okno(Some(5))));
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(hook.posli(HookPrikaz::Okno(None)));
        std::thread::sleep(Duration::from_millis(20));
        // Zpráva za vším předchozím: až dorazí, smyčka prošla hlídáním.
        assert!(hook.posli(HookPrikaz::Zverejni));
        pockej_na(&|| !z.mapovani.lock().unwrap().is_empty());
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(st.instalaci(), n);
        assert!(st.nainstalovan());
        assert_eq!(
            z.rozhodnuti.lock().unwrap().last().map(|r| r.2),
            Some(Mode::Gamepad)
        );
        assert!(hook.zastav());
    }

    /// Konec při zachytávání: neutrál všem ovladačům ještě před
    /// odebráním hooku (pořadí neutrál → odhooknout).
    #[test]
    fn konec_pri_zachytavani_posle_neutral() {
        let z = Arc::new(Zaznam::default());
        let vystup: Arc<dyn Vystup> = z.clone();
        let mut hook = spust_s(
            mapovani(),
            vystup,
            testy_plocha::na_skryte_plose,
            je_v_popredi,
        )
        .unwrap();
        assert!(hook.posli(HookPrikaz::Povol(PadId::FIRST)));
        assert!(hook.posli(HookPrikaz::Zachytavej));
        let konec = std::time::Instant::now() + LIMIT;
        while z.rozhodnuti.lock().unwrap().last().map(|r| r.2) != Some(Mode::Gamepad) {
            assert!(std::time::Instant::now() < konec);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(hook.zastav());
        let (_, d, m) = z.posledni();
        assert_eq!(m, Mode::Keyboard);
        for p in PadId::ALL {
            assert_eq!(d.pads.get(p), Some(PadState::NEUTRAL), "{p:?}");
        }
        assert_eq!(
            d.ui,
            Some(UiEvent::ModeChanged {
                mode: Mode::Keyboard,
                cause: ModeCause::Forced(ForceReason::Shutdown),
            })
        );
    }

    /// Revize Fáze 4 (teď snímkem, OQ 57): klávesu, kterou OS drží přes
    /// zapomenutí držených kláves (zamčení), převezme snímek jako klávesu
    /// OS — její autorepeat ani key-up se nespolknou, nic nevisí. Klávesu,
    /// kterou engine sleduje, snímek nemění.
    #[test]
    fn klavesa_drzena_os_pres_zapomenuti_nevisi() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        podvrhni_os(jen_f24);
        proved(HookPrikaz::Zapomen(ForceReason::SessionLock));
        dokonci_snimek(true);
        let _ = s_stavem(|s| s.engine.toggle(ted_ms()));
        assert_eq!(rezim(), Mode::Gamepad, "hra zase běží");
        // Od teď by Windows „tvrdily", že drží všechno — callback se jich
        // neptá.
        podvrhni_os(vse_drzi);
        // Autorepeat F24 (namapované) — OS ji drží, engine o ní ví jen ze
        // snímku.
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x76, 0)), "autorepeat do OS");
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        assert!(!zavolej(0, WM_KEYUP, &kb(0x76, LLKHF_UP.0)), "key-up do OS");
        assert!(
            z.rozhodnuti.lock().unwrap().iter().all(|r| r
                .1
                .pads
                .get(PadId::FIRST)
                .is_none_or(|p| p == PadState::NEUTRAL)),
            "ovladač se nehnul"
        );
        // Nový stisk patří ovladači — i když Windows „tvrdí", že drží
        // všechno: callback se jich neptá.
        assert!(zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        // Snímek klávesu, kterou engine sleduje, nepřebírá.
        podvrhni_os(jen_f24);
        s_stavem(zapomen_klavesnici);
        dokonci_snimek(true);
        assert!(zavolej(0, WM_KEYDOWN, &kb(0x76, 0)), "autorepeat ovladače");
        assert!(zavolej(0, WM_KEYUP, &kb(0x76, LLKHF_UP.0)));
    }

    /// Hook vlákno spadne mimo callback (tady výstup při příkazu): hook
    /// zmizí se skončením vlákna a ovladače dostanou neutrál — jinak by
    /// jim zůstal poslední stav a pad vlákna by ho dál posílala.
    #[test]
    fn panika_vlakna_mimo_callback_posle_neutral() {
        #[derive(Default)]
        struct Padajici(Mutex<Vec<Radek>>, AtomicBool);
        impl Vystup for Padajici {
            fn rozhodnuti(&self, u: Option<&Udalost>, d: &Decision, rezim: Mode) {
                let prepnuti_z_okna = d.ui
                    == Some(UiEvent::ModeChanged {
                        mode: Mode::Keyboard,
                        cause: ModeCause::Gui,
                    });
                if prepnuti_z_okna {
                    panic!("zkušební panika mimo callback");
                }
                self.0.lock().unwrap().push((u.copied(), *d, rezim));
            }
            fn hook_chyba(&self, chyba: bool) {
                self.1.store(chyba, Ordering::Release);
            }
        }
        let v = Arc::new(Padajici::default());
        let vystup: Arc<dyn Vystup> = v.clone();
        let mut hook = spust_s(
            mapovani(),
            vystup,
            testy_plocha::na_skryte_plose,
            je_v_popredi,
        )
        .unwrap();
        let st = Arc::clone(hook.status());
        assert!(hook.posli(HookPrikaz::Povol(PadId::FIRST)));
        assert!(hook.posli(HookPrikaz::Zachytavej));
        let konec = std::time::Instant::now() + LIMIT;
        while v.0.lock().unwrap().last().map(|r| r.2) != Some(Mode::Gamepad) {
            assert!(std::time::Instant::now() < konec);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(hook.posli(HookPrikaz::Prepni));
        let konec = std::time::Instant::now() + LIMIT;
        while !st.rozbity() {
            assert!(std::time::Instant::now() < konec, "panika se neprojevila");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(hook.zastav(), "vlákno skončilo (ne visí)");
        assert!(!st.nainstalovan());
        assert!(v.1.load(Ordering::Acquire), "okno se dozví, že hook není");
        let (u, d, m) = *v.0.lock().unwrap().last().unwrap();
        assert!(u.is_none() && matches!(m, Mode::Disabled { .. }));
        for p in PadId::ALL {
            assert_eq!(d.pads.get(p), Some(PadState::NEUTRAL), "{p:?}");
        }
        assert!(!hook.posli(HookPrikaz::Prepni), "mrtvé vlákno nic nepřijme");
    }

    /// Watchdog (Fáze 5): ovladač, jehož pad vlákno přes 1 s nemluvilo
    /// s ViGEm, dostane při nejbližší klávese Klávesnici — klávesa jde
    /// do OS, ne do zaseknutého ovladače. Živý ovladač nic nezmění.
    #[test]
    fn watchdog_zaseknuteho_ovladace() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        z.tep.store(ted_ms(), Ordering::Release);
        assert!(zavolej(0, WM_KEYDOWN, &kb(0x76, 0)), "živý ovladač hraje");
        assert!(zavolej(0, WM_KEYUP, &kb(0x76, LLKHF_UP.0)));
        z.tep.store(1, Ordering::Release);
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x76, 0)), "zaseknutý: do OS");
        assert_eq!(rezim(), Mode::Keyboard);
        let vynuceni = z.rozhodnuti.lock().unwrap().iter().any(|r| {
            r.1.ui
                == Some(UiEvent::ModeChanged {
                    mode: Mode::Keyboard,
                    cause: ModeCause::Forced(ForceReason::Watchdog),
                })
        });
        assert!(vynuceni);
        assert!(!zavolej(0, WM_KEYUP, &kb(0x76, LLKHF_UP.0)));
        // Čerstvé povolení se počítá jako tep (ohlášení „zapnuto" předbíhá
        // kopii tepu do slotu).
        STAV.with(|s| {
            let mut g = s.borrow_mut();
            let s = g.as_mut().unwrap();
            s.povoleno_ms[0] = ted_ms();
            let _ = s.engine.toggle(ted_ms());
        });
        assert!(
            zavolej(0, WM_KEYDOWN, &kb(0x76, 0)),
            "čerstvě povolený hraje"
        );
    }

    /// Přepnutí plochy (UAC, Ctrl+Alt+Del, zamčení): Klávesnice a držené
    /// klávesy zapomenuté — key-up po návratu jde do OS, ovladač neutrální.
    #[test]
    fn prepnuti_plochy_zapomene_klavesy() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        assert!(zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        prepnuti_plochy();
        assert_eq!(rezim(), Mode::Keyboard);
        assert!(!drzeno(F24));
        let (_, d, _) = z.posledni();
        assert_eq!(d.pads.get(PadId::FIRST), Some(PadState::NEUTRAL));
        assert!(!zavolej(0, WM_KEYUP, &kb(0x76, LLKHF_UP.0)));
        // Zamčení relace přes příkaz dělá totéž.
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        let d = STAV.with(|s| {
            let mut g = s.borrow_mut();
            prikaz(
                &mut g.as_mut().unwrap().engine,
                HookPrikaz::Zapomen(ForceReason::SessionLock),
                ted_ms(),
            )
        });
        assert_eq!(d.pads.get(PadId::FIRST), Some(PadState::NEUTRAL));
        assert!(!drzeno(F24));
    }

    /// Časovač přiřazování běží jen při přiřazování.
    #[test]
    fn casovac_jen_pri_prirazovani() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        assert_eq!(hlidej_casovac(0), 0);
        STAV.with(|s| {
            let mut s = s.borrow_mut();
            let e = &mut s.as_mut().unwrap().engine;
            let _ = e.force_keyboard(ForceReason::Shutdown);
            let _ = e.start_binding(
                PadAction::first(Action::Button(PadButton::B)),
                BindKind::Replace,
                ted_ms(),
            );
        });
        let id = hlidej_casovac(0);
        assert_ne!(id, 0);
        assert_eq!(hlidej_casovac(id), id, "běžící časovač se nezakládá znovu");
        STAV.with(|s| {
            let _ = s
                .borrow_mut()
                .as_mut()
                .unwrap()
                .engine
                .cancel_binding(ted_ms());
        });
        assert_eq!(hlidej_casovac(id), 0);
    }

    /// Měření pro `hook_selftest`: vrátí čas každé události, předchozí
    /// stav vlákna obnoví a nic nenechá v systému.
    #[test]
    fn mereni_vrati_casy_a_obnovi_stav() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        let jiny: Arc<dyn Vystup> = Arc::new(Zaznam::default());
        for zive in [false, true] {
            let casy = zmer_zpracovani(100, Mapping::default(), Arc::clone(&jiny), zive, |_| 0);
            assert_eq!(casy.len(), 100);
        }
        // Snímek klávesnice na skryté ploše (není vstupní — skutečná
        // klávesnice se nečte).
        let casy = std::thread::spawn(|| {
            testy_plocha::na_skryte_plose().unwrap();
            let v: Arc<dyn Vystup> = Arc::new(Zaznam::default());
            zmer_snimek(5, Mapping::default(), v)
        })
        .join()
        .unwrap();
        assert_eq!(casy.len(), 5);
        // Stav testu zůstal (zaznamenává dál do `z`).
        assert!(zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        assert_eq!(z.posledni().0.unwrap().klavesa, F24);
    }
}

/// Skrytá plocha pro testy: hook ani okna testů nesmí sahat na plochu
/// vlastníka (může na ní běžet hra).
#[cfg(test)]
pub(crate) mod testy_plocha {
    use windows::core::HSTRING;
    use windows::Win32::System::StationsAndDesktops::{
        CreateDesktopW, SetThreadDesktop, DESKTOP_CONTROL_FLAGS,
    };
    use windows::Win32::System::Threading::GetCurrentThreadId;

    /// Přesune volající vlákno na novou skrytou plochu. Plocha zanikne,
    /// až ji přestane používat poslední vlákno. Jméno s číslem vlákna:
    /// testy běží souběžně a druhé `CreateDesktopW` se stejným jménem by
    /// otevíralo cizí plochu (a dostalo „přístup odepřen").
    pub fn na_skryte_plose() -> Result<(), String> {
        // SAFETY: bez parametrů.
        let jmeno = HSTRING::from(format!("KeyPadTestHook{}", unsafe { GetCurrentThreadId() }));
        // SAFETY: vytvoření plochy s výchozími právy; handle se nechává
        // otevřený po celý život vlákna (SetThreadDesktop ho potřebuje).
        unsafe {
            let plocha = CreateDesktopW(
                &jmeno,
                None,
                None,
                DESKTOP_CONTROL_FLAGS(0),
                0x10000000, // GENERIC_ALL
                None,
            )
            .map_err(|e| format!("CreateDesktopW: {e}"))?;
            SetThreadDesktop(plocha).map_err(|e| format!("SetThreadDesktop: {e}"))
        }
    }
}

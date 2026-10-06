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
//! [`Vystup`]u (režim, oznámení, revize mapování, živý stav), `SetEvent`,
//! `GetAsyncKeyState` a `PostThreadMessageW` jen po panice do vlastní
//! fronty. Nesmí kanál, `Mutex`, alokaci ani uvolnění, `log::`, `emit`
//! ani klon mapování — snímek mapování pro okno klonuje jen smyčka na
//! povel [`HookPrikaz::Zverejni`].
//!
//! Všechno ostatní (příkazy z okna, časovač přiřazování, hlášení paniky
//! do logu, popředí okna) dělá táž smyčka mimo callback.
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
    BindKind, Decision, DisabledReason, Engine, ForceReason, KeyId, LiveInputs, Mapping,
    MappingError, Mode, PadAction, PadId, PadState, PadUpdates, MAX_PADS,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetAncestor, GetForegroundWindow, GetMessageW, KillTimer,
    PeekMessageW, PostThreadMessageW, SetTimer, SetWindowsHookExW, UnhookWindowsHookEx,
    EVENT_SYSTEM_DESKTOPSWITCH, EVENT_SYSTEM_FOREGROUND, GA_ROOT, HC_ACTION, HHOOK,
    KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, MSG, PM_NOREMOVE, WH_KEYBOARD_LL,
    WINEVENT_OUTOFCONTEXT, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
    WM_TIMER,
};

/// Ve frontě kanálu čekají příkazy (probuzení smyčky).
const WM_PRIKAZ: u32 = WM_APP + 1;
/// Callback spadl do paniky — smyčka to zaloguje (callback logovat nesmí).
const WM_PANIKA: u32 = WM_APP + 2;
/// Okno KeyPadu získalo nebo ztratilo popředí (callback WinEventu): ať
/// smyčka projde hlídáním hooku. Callback WinEventu běží uvnitř
/// `GetMessageW`, takže sám smyčku neotočí.
const WM_POPREDI: u32 = WM_APP + 3;

/// Krok časovače při přiřazování klávesy. Timeout přiřazování je 10 s;
/// o čtvrt vteřiny později je pořád „po deseti vteřinách". Časovač běží
/// jen během přiřazování — nečinný hook nikdy netiká (princip 10).
const TIK_MS: u32 = 250;

/// Levá a pravá Win jako virtuální klávesy (`Udalost::vk`). Vlastní
/// konstanty, ne `VIRTUAL_KEY` z `windows`: soubor sdílí i příklad
/// `hook_selftest` (`#[path]`) a dotaz jde přes podvrhnutelné `os_drzi`.
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;
/// Bity levé a pravé Win v `Stav::win`.
const WIN_L: u8 = 1;
const WIN_R: u8 = 2;

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
    /// podvrh (skutečný stav klávesnice testy měnit nesmí).
    os_drzi: fn(u32) -> bool,
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
    /// stisknout a pustit. Win jde vždy do OS, takže tudy projde každá
    /// její událost; Windows se pak na Win ptají, jen když je podle toho
    /// dole ([`drzi_win`]).
    win: u8,
    /// Instalace hooku do systému. Skutečně [`nainstaluj`], v testech
    /// podvrh — selhání skutečné instalace se vyvolat nedá.
    instaluj: fn() -> Result<HHOOK, String>,
    /// Kdy engine ovladač povolil. Pad vlákno ohlásí „zapnuto" dřív,
    /// než zkopíruje tep do slotu — watchdog tedy bere novější z obou,
    /// jinak by čerstvě zapnutý ovladač mohl hned vypadat zaseknutý.
    povoleno_ms: [u64; MAX_PADS],
}

impl Stav {
    fn novy(engine: Engine, vystup: Arc<dyn Vystup>, aktivni: fn(isize) -> bool) -> Stav {
        Stav {
            engine,
            vystup,
            rozbity: false,
            os_drzi,
            aktivni,
            okno: None,
            okno_aktivni: false,
            sledovano: false,
            win: 0,
            instaluj: nainstaluj,
            povoleno_ms: [0; MAX_PADS],
        }
    }
}

/// Drží OS klávesu? Asynchronní stav klávesnice: v LL hooku je to stav
/// PŘED touhle událostí a spolknuté události ho nemění — „dole" tedy
/// znamená, že OS viděl key-down a key-up ještě ne. Nečeká, nezamyká.
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

/// Drží OS Win (pravidlo Win+klávesa, OQ 44)? Bez Win „dole" podle
/// callbacku se Windows neptá — každý dotaz je volání jádra a právě ty
/// tvořily ocas p99 zpracování klávesy. Win „dole" se ale ověří a bit
/// opraví: její uvolnění mohl callback minout (zabezpečená plocha po
/// Win+L, hook mimo systém) a zastaralý bit by jinak posílal Windows
/// každý další stisk. Volá se z callbacku — jen `GetAsyncKeyState`.
fn drzi_win(s: &mut Stav) -> bool {
    if s.win == 0 {
        return false;
    }
    let mut drzi = 0;
    for (bit, vk) in [(WIN_L, VK_LWIN), (WIN_R, VK_RWIN)] {
        if s.win & bit != 0 && (s.os_drzi)(vk) {
            drzi |= bit;
        }
    }
    s.win = drzi;
    drzi != 0
}

/// Srovná bity Win se stavem Windows. Po instalaci hooku a po přepnutí
/// plochy: stisk Win, který callback neviděl (hook ještě nebyl v systému,
/// Win stisknutá na jiné ploše), by jinak pravidlo Win+klávesa minulo.
fn srovnej_win() {
    STAV.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.win = [(WIN_L, VK_LWIN), (WIN_R, VK_RWIN)]
                .into_iter()
                .filter(|&(_, vk)| (s.os_drzi)(vk))
                .fold(0, |a, (bit, _)| a | bit);
        }
    });
}

/// Podvrh [`os_drzi`]: OS nic nedrží. Testy a syntetické klávesy
/// (`KEYPAD_TEST_KLAVESY`) — skutečná klávesnice vlastníka nesmí
/// rozhodovat o klávese, kterou poslal test.
#[cfg(any(test, debug_assertions))]
fn nic_nedrzi(_vk: u32) -> bool {
    false
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
    if ladici("KEYPAD_TEST_KLAVESY") {
        log::warn!("KEYPAD_TEST_KLAVESY: hook nečte stav klávesnice (testovací klávesy)");
        stav.os_drzi = nic_nedrzi;
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
            // Popředí se změnilo — stačí projít hlídáním hooku níž.
            WM_POPREDI => {}
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
        casovac = hlidej_casovac(casovac);
    }

    // Pořadí jako u každého konce: neutrál → odhooknout (→ odpojit pady
    // udělá volající). Po vynucené Klávesnici hook už nic nepotlačí.
    s_enginem(|e| e.force_keyboard(ForceReason::Shutdown));
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
    s_enginem(|e| e.reset_held(ForceReason::DesktopSwitch));
    // Win+L: uvolnění Win je už na zabezpečené ploše a callback ho nevidí.
    srovnej_win();
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
                s.okno_aktivni = sledovano && (s.aktivni)(h);
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
            s_enginem(|e| e.reset_held(ForceReason::HookReinstalled));
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

/// Hook do systému ([`Stav::instaluj`]). Po instalaci srovná bity Win
/// s Windows — stisk Win před instalací callback neviděl.
fn instaluj() -> Result<HHOOK, String> {
    let f = STAV.with(|s| {
        s.borrow()
            .as_ref()
            .map_or(nainstaluj as fn() -> Result<HHOOK, String>, |s| s.instaluj)
    });
    let h = f()?;
    srovnej_win();
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
    s_enginem(|e| e.reset_held(ForceReason::HookReinstalled));
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
            Ok(()) => s_enginem(|e| e.start_binding(cil, druh, ted)),
            Err(proc) => log::debug!("přiřazování: {proc} — klik se nepřijímá"),
        },
        HookPrikaz::ZrusPrirazeni => s_enginem(|e| e.cancel_binding(ted)),
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
/// změna přišla. Jen atomiky a `SetEvent` (volá se z callbacku).
fn predej(s: &Stav, u: Option<&Udalost>, d: &Decision) {
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
    zpracuj_udalost(&Udalost::z(kb, dolu))
}

/// Jádro callbacku: jedna událost → engine → výstup. `true` = potlačit.
/// Touž funkcí jde i syntetická klávesa testů
/// ([`HookPrikaz::TestKlavesa`]) a měření `hook_selftest -- mereni`.
fn zpracuj_udalost(u: &Udalost) -> bool {
    STAV.with(|s| {
        // Zanořené volání (nemělo by nastat — callback nic nepumpuje)
        // nebo vlákno bez stavu: propustit, nikdy neblokovat.
        let Ok(mut s) = s.try_borrow_mut() else {
            return false;
        };
        let Some(s) = s.as_mut().filter(|s| !s.rozbity) else {
            return false;
        };
        // Win sleduje callback sám (OQ 44): i vstříknutá mění stav OS.
        let win = bit_win(u.vk);
        if win != 0 {
            if u.dolu {
                s.win |= win;
            } else {
                s.win &= !win;
            }
        }
        if u.vstrcena() {
            s.vystup
                .rozhodnuti(Some(u), &Decision::NONE, s.engine.mode());
            return false;
        }
        let ted = ted_ms();
        // Watchdog: klávesy do zaseknutého ovladače by jen mizely
        // (princip 1) — hra dál vidí jeho poslední stav a uživatel nemůže
        // ani psát. Proto Klávesnice; zachytávání vrátí zkratka.
        if s.engine.mode() == Mode::Gamepad && zaseknuty(s, ted) {
            let d = s.engine.force_keyboard(ForceReason::Watchdog);
            predej(s, None, &d);
        }
        // Key-down klávesy, o které engine neví, patří OS, když:
        // - OS tu klávesu drží: hook ji neviděl stisknout (nainstaloval se
        //   později, nebo se držené klávesy zapomněly). Je to autorepeat
        //   klávesy OS — ne nový stisk, který by se spolkl i s key-upem
        //   (klávesa by v OS visela);
        // - OS drží Win (Fáze 4b, OQ 44): Win+D, Win+E, Win+Tab patří
        //   Windows i za hry a při přiřazování. Win sama jde do OS vždy
        //   (engine ji nesleduje) — kdyby hook druhou klávesu spolkl jako
        //   klávesu ovladače, Windows by viděly osamělou Win a otevřely
        //   Start.
        // Ptá se jen u stisku, o kterém engine rozhoduje (zkratka,
        // přiřazování, klávesa ovladače při hře): jinde by převzetí
        // dopadlo stejně jako nový stisk a dotaz na Windows je drahý.
        if u.dolu
            && s.engine.held(u.klavesa).is_none()
            && s.engine.claims_new_press(u.klavesa)
            && ((s.os_drzi)(u.vk) || drzi_win(s))
        {
            let _ = s.engine.adopt_os_key(u.klavesa, ted);
        }
        let d = s.engine.on_key(u.klavesa, u.dolu, ted);
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
/// nepoužívá a smyčka hook odebere. Logovat smí až smyčka.
fn po_panice() {
    let uklid = catch_unwind(AssertUnwindSafe(|| {
        STAV.with(|s| {
            if let Ok(mut s) = s.try_borrow_mut() {
                if let Some(s) = s.as_mut() {
                    let d = s.engine.reset_held(ForceReason::HookPanic);
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
/// oknem v popředí (živý stav navíc); `stav_klavesnice` = `false` místo
/// `GetAsyncKeyState` podvrh „nic nedrží" (oddělí cenu dotazu na Windows
/// od ceny KeyPadu); `vk` = virtuální klávesa události — dotaz na
/// skutečnou VK nemusí stát tolik co na neplatnou nulu. Vrací ns na
/// událost.
#[doc(hidden)]
#[allow(dead_code, reason = "měří jen příklad hook_selftest (-- mereni)")]
pub fn zmer_zpracovani(
    n: usize,
    mapovani: Mapping,
    vystup: Arc<dyn Vystup>,
    zive: bool,
    stav_klavesnice: bool,
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
    if !stav_klavesnice {
        s.os_drzi = |_| false;
    }
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

    fn vse_drzi(_vk: u32) -> bool {
        true
    }

    /// OS drží jen levou Win (uživatel mačká Win+něco).
    fn jen_win(vk: u32) -> bool {
        vk == 0x5B
    }

    fn jen_pravou_win(vk: u32) -> bool {
        vk == 0x5C
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
    /// když OS drží Win (klávesa se převezme jako klávesa OS), s oknem
    /// (živý stav) i bez něj a při přiřazování (oznámení uloženo,
    /// odmítnuto, ťuknutí Altem). Výstup je skutečný `HookVystup` (sloty,
    /// atomiky, `SetEvent`).
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
        // Skutečný dotaz na stav klávesy — i ten musí být bez alokace;
        // podvrh „drží Win“ projde větví převzetí klávesy OS. Celé kolo
        // běží s Win dole podle callbacku: skutečný dotaz bit Win opraví,
        // podvrh ho potvrdí.
        let dotazy: [fn(u32) -> bool; 2] = [os_drzi, jen_win];
        let cil = PadAction::first(Action::Button(PadButton::B));
        for dotaz in dotazy {
            for okno in [None, Some(7isize)] {
                for prirazuje in [false, true] {
                    let sloty: [Arc<StavSlot>; MAX_PADS] =
                        std::array::from_fn(|_| Arc::new(StavSlot::new().unwrap()));
                    let budik = Arc::new(Budik::new().unwrap());
                    let vystup: Arc<dyn Vystup> = Arc::new(HookVystup::new(sloty, budik));
                    let mut engine = Engine::new(Mapping::default());
                    let _ = engine.enable(PadId::FIRST);
                    let mut s = Stav::novy(engine, vystup, |_| true);
                    s.os_drzi = dotaz;
                    s.okno = okno;
                    s.okno_aktivni = okno.is_some();
                    STAV.with(|st| *st.borrow_mut() = Some(s));
                    for _ in 0..3 {
                        if prirazuje {
                            s_enginem(|e| e.start_binding(cil, BindKind::Replace, ted_ms()));
                        }
                        let pred = crate::testy_alokace::pocet();
                        win(false, true);
                        for &(scan, flags, vk) in &udalosti {
                            let k = kb_vk(scan, flags, vk);
                            zavolej(0, WM_KEYDOWN, &k);
                            zavolej(0, WM_KEYDOWN, &k);
                            zavolej(0, WM_KEYUP, &kb_vk(scan, flags | LLKHF_UP.0, vk));
                        }
                        win(false, false);
                        assert_eq!(
                            crate::testy_alokace::pocet(),
                            pred,
                            "alokace v callbacku (okno {okno:?}, přiřazuje {prirazuje})"
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

    /// Win+L při hře (OQ 44): OS drží Win → L (ve výchozím mapování
    /// D-pad vpravo) jde do Windows celé, stisk i key-up, a stav padu se
    /// nezmění. Bez Win je L zase klávesa ovladače.
    #[test]
    fn win_s_klavesou_ve_hre_patri_windows() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        let l = KeyId::L;
        assert!(Mapping::default().target(l).is_some(), "L je namapovaná");
        podvrhni_os(jen_win);
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
        podvrhni_os(nic_nedrzi);
        assert!(!win(false, false));
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
        // Pravá Win stejně.
        podvrhni_os(jen_pravou_win);
        assert!(!win(true, true));
        assert!(!zavolej(0, WM_KEYDOWN, &kb_l(0)), "pravá Win+L do Windows");
        assert!(!zavolej(0, WM_KEYUP, &kb_l(LLKHF_UP.0)));
        assert!(!win(true, false));
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
        podvrhni_os(jen_win);
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
        podvrhni_os(nic_nedrzi);
        assert!(!win(false, false));
        assert!(zavolej(0, WM_KEYDOWN, &kb_l(0)));
        assert!(
            matches!(z.posledni().1.ui, Some(UiEvent::BindingSaved { key, target, .. }) if key == l && target == cil)
        );
    }

    thread_local! {
        /// Kolikrát se callback zeptal Windows na stav klávesy.
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

    /// Počítaný podvrh: OS drží levou Win.
    fn pocitej_win(vk: u32) -> bool {
        DOTAZY.with(|d| d.set(d.get() + 1));
        vk == VK_LWIN
    }

    /// Dotaz na Windows (volání jádra, ocas p99) jde jen od nového stisku,
    /// o kterém engine rozhoduje — a na Win jen tehdy, když ji callback
    /// viděl stisknout. Zastaralý bit Win (uvolnění na zabezpečené
    /// ploše) se při dotazu opraví.
    #[test]
    fn windows_se_pta_jen_kdyz_je_proc() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        DOTAZY.with(|d| d.set(0));
        podvrhni_os(pocitej_nic);
        let klavesa = |scan: u32, dolu: bool| {
            zavolej(
                0,
                if dolu { WM_KEYDOWN } else { WM_KEYUP },
                &kb(scan, if dolu { 0 } else { LLKHF_UP.0 }),
            )
        };
        // Klávesa ovladače při hře: jen na sebe, na Win ne.
        assert!(klavesa(0x11, true), "W hraje");
        assert_eq!(dotazy(), 1);
        // Autorepeat, key-up a nenamapovaný Tab se neptají vůbec.
        klavesa(0x11, true);
        klavesa(0x11, false);
        assert!(!klavesa(0x0F, true));
        klavesa(0x0F, false);
        assert_eq!(dotazy(), 1);
        // Pozastaveno: W patří OS tak jako tak.
        let _ = s_stavem(|s| s.engine.toggle(0));
        assert!(!klavesa(0x11, true));
        klavesa(0x11, false);
        assert_eq!(dotazy(), 1);
        let _ = s_stavem(|s| s.engine.toggle(0));

        // Win dole podle callbacku → ověří se u Windows a W jde Windows.
        podvrhni_os(pocitej_win);
        assert!(!win(false, true));
        assert!(!klavesa(0x11, true), "Win+W Windows");
        klavesa(0x11, false);
        assert_eq!(dotazy(), 3, "W a levá Win");
        // Uvolnění Win callback minul (zabezpečená plocha): Windows řeknou
        // „není dole", bit se opraví a další stisk se na Win neptá.
        podvrhni_os(pocitej_nic);
        assert!(klavesa(0x1E, true), "A hraje");
        assert_eq!(dotazy(), 5, "A a levá Win");
        assert_eq!(s_stavem(|s| s.win), 0);
        klavesa(0x1E, false);
        assert!(klavesa(0x20, true), "D hraje");
        assert_eq!(dotazy(), 6, "jen D");
        klavesa(0x20, false);
    }

    /// Po instalaci hooku a po přepnutí plochy se bity Win srovnají
    /// s Windows — Win stisknutou, když hook nebyl v systému, by pravidlo
    /// Win+klávesa jinak minulo.
    #[test]
    fn win_se_srovna_po_instalaci_a_prepnuti_plochy() {
        let z = Arc::new(Zaznam::default());
        priprav_s(&z, Mapping::default());
        s_stavem(|s| s.instaluj = || Ok(HHOOK(std::ptr::without_provenance_mut(0x1000))));
        podvrhni_os(jen_win);
        let status = HookStatus::default();
        // Podvržený handle se vrací volajícímu — nikdy se neodhookne.
        let h = hlidej_hook(HHOOK::default(), &status);
        assert!(!h.is_invalid() && status.nainstalovan());
        assert_eq!(s_stavem(|s| s.win), WIN_L, "Win držená před instalací");
        assert!(!zavolej(0, WM_KEYDOWN, &kb_l(0)), "Win+L Windows");
        assert!(!zavolej(0, WM_KEYUP, &kb_l(LLKHF_UP.0)));
        podvrhni_os(nic_nedrzi);
        prepnuti_plochy();
        assert_eq!(s_stavem(|s| s.win), 0, "Win puštěná na jiné ploše");
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

    /// Revize Fáze 4: klávesu, kterou OS drží (hook neviděl její stisk,
    /// protože se nainstaloval později), callback převezme jako klávesu
    /// OS — její autorepeat ani key-up se nespolknou, nic nevisí.
    #[test]
    fn klavesa_drzena_os_pri_instalaci_hooku_nevisi() {
        let z = Arc::new(Zaznam::default());
        priprav(&z);
        podvrhni_os(vse_drzi);
        // Autorepeat F24 (namapované) — OS ji drží, engine o ní neví.
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x76, 0)), "autorepeat do OS");
        assert!(!zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        assert!(!zavolej(0, WM_KEYUP, &kb(0x76, LLKHF_UP.0)), "key-up do OS");
        assert_eq!(
            z.posledni().1.pads.get(PadId::FIRST),
            None,
            "ovladač se nehnul"
        );
        // Nový stisk (OS ho ještě neviděl) už patří ovladači.
        podvrhni_os(nic_nedrzi);
        assert!(zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        // Klávesa, kterou engine sleduje, se podle OS nepřebírá.
        podvrhni_os(vse_drzi);
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
            let casy = zmer_zpracovani(
                100,
                Mapping::default(),
                Arc::clone(&jiny),
                zive,
                true,
                |_| 0,
            );
            assert_eq!(casy.len(), 100);
        }
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

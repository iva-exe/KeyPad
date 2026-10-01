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
//! Všechno ostatní (příkazy z okna, časovač přiřazování, hlášení paniky
//! do logu) dělá táž smyčka mimo callback.
//!
//! Vlákno s enginem běží celou dobu, samotný hook je ale v systému JEN
//! tehdy, když ho engine potřebuje — je zapnutý aspoň jeden ovladač
//! (nebo se přiřazuje klávesa). Jinak KeyPad na klávesnici vůbec nesahá
//! (princip 10) a nic nemůže zdržet psaní v jiných programech.

use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use keypad_core::{
    Decision, DisabledReason, Engine, ForceReason, KeyId, Mapping, Mode, PadId, PadState,
    PadUpdates, MAX_PADS,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, KillTimer, PeekMessageW, PostThreadMessageW,
    SetTimer, SetWindowsHookExW, UnhookWindowsHookEx, EVENT_SYSTEM_DESKTOPSWITCH, HC_ACTION, HHOOK,
    KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, MSG, PM_NOREMOVE, WH_KEYBOARD_LL,
    WINEVENT_OUTOFCONTEXT, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
    WM_TIMER,
};

/// Ve frontě kanálu čekají příkazy (probuzení smyčky).
const WM_PRIKAZ: u32 = WM_APP + 1;
/// Callback spadl do paniky — smyčka to zaloguje (callback logovat nesmí).
const WM_PANIKA: u32 = WM_APP + 2;

/// Krok časovače při přiřazování klávesy. Timeout přiřazování je 10 s;
/// o čtvrt vteřiny později je pořád „po deseti vteřinách". Časovač běží
/// jen během přiřazování — nečinný hook nikdy netiká (princip 10).
const TIK_MS: u32 = 250;

/// Levá a pravá Win jako virtuální klávesy (`Udalost::vk`). Vlastní
/// konstanty, ne `VIRTUAL_KEY` z `windows`: soubor sdílí i příklad
/// `hook_selftest` (`#[path]`) a dotaz jde přes podvrhnutelné `os_drzi`.
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;

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
/// atomiky a události Windows (princip 3). Ve Fázi 4 tudy jde stav
/// padu do atomického slotu pad vlákna a režim do okna.
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
}

/// Příkazy hook vláknu. Každý vede na volání enginu a jeho rozhodnutí
/// jde do [`Vystup`]u stejně jako rozhodnutí o klávese.
#[derive(Debug, Clone)]
#[allow(dead_code, reason = "Mapovani použije Fáze 6–7 (editor, konfigurace)")]
pub enum HookPrikaz {
    /// Nové mapování (v režimu Gamepad nejdřív vynutí Klávesnici).
    Mapovani(Box<Mapping>),
    /// Ovladač se připojil — engine ho povolí (z `Disabled` na
    /// Klávesnici, nikdy sám na Gamepad).
    Povol(PadId),
    /// Ovladač není — jeho klávesy jdou zase do Windows; poslední
    /// vypnutý → `Disabled` (a hook ze systému zmizí).
    Zakaz(PadId, DisabledReason),
    /// Přepnout Klávesnice ↔ Gamepad (tlačítko v okně).
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
    Preinstaluj,
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
    /// s prvním povoleným ovladačem ([`HookPrikaz::Povol`]).
    ///
    /// Engine startuje jako vždy v `Disabled { PadNotConnected }`.
    pub fn spust(mapovani: Mapping, vystup: Arc<dyn Vystup>) -> Result<Hook, String> {
        spust_s(mapovani, vystup, || Ok(()))
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
/// hook instalují na skryté ploše, ne na ploše vlastníka.
fn spust_s(
    mapovani: Mapping,
    vystup: Arc<dyn Vystup>,
    pred: impl FnOnce() -> Result<(), String> + Send + 'static,
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
                        vlakno(Engine::new(mapovani), vystup, stav, rx, &hotovo_tx)
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
/// posílala. Proto všem neutrál a okno se dozví, že nic nezachytává.
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
    /// Kdy engine ovladač povolil. Pad vlákno ohlásí „zapnuto" dřív,
    /// než zkopíruje tep do slotu — watchdog tedy bere novější z obou,
    /// jinak by čerstvě zapnutý ovladač mohl hned vypadat zaseknutý.
    povoleno_ms: [u64; MAX_PADS],
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
    engine: Engine,
    vystup: Arc<dyn Vystup>,
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

    STAV.with(|s| {
        *s.borrow_mut() = Some(Stav {
            engine,
            vystup,
            rozbity: false,
            os_drzi,
            povoleno_ms: [0; MAX_PADS],
        })
    });
    // Hook zatím ne: bez zapnutého ovladače ho engine nepotřebuje.
    let mut hook = HHOOK::default();
    let plocha = hlidej_plochu();
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
                        p => s_enginem(|e| prikaz(e, p, ted_ms())),
                    }
                }
            }
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
    if let Some(h) = plocha {
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

/// Hook je v systému právě tehdy, když ho engine potřebuje: mimo
/// `Disabled` (zapnutý ovladač, přiřazování). Po odebrání se držené
/// klávesy zapomenou — jejich key-upy hook neuvidí a zastaralý záznam
/// by příští stisk téže klávesy vzal jako autorepeat.
fn hlidej_hook(hook: HHOOK, status: &HookStatus) -> HHOOK {
    let potreba = rezim().is_some_and(|m| !matches!(m, Mode::Disabled { .. }));
    match (potreba, hook.is_invalid()) {
        (true, true) => match nainstaluj() {
            Ok(h) => {
                status.nainstalovan.store(true, Ordering::Release);
                status.instalaci.fetch_add(1, Ordering::AcqRel);
                nahlas_chybu(false);
                log::info!("hook klávesnice nainstalován (zapnutý ovladač)");
                h
            }
            Err(e) => {
                // Klávesy jdou do Windows — bezpečná strana (princip 1).
                // Zachytávání bez hooku by jen předstíralo, že běží.
                if rezim() == Some(Mode::Gamepad) {
                    s_enginem(|e| e.force_keyboard(ForceReason::HookReinstalled));
                }
                nahlas_chybu(true);
                log::error!("hook klávesnice nejde nainstalovat: {e}");
                hook
            }
        },
        (false, true) => {
            // Hook už není potřeba — ani jeho chyba.
            nahlas_chybu(false);
            hook
        }
        (false, false) => {
            odhookni(hook);
            status.nainstalovan.store(false, Ordering::Release);
            s_enginem(|e| e.reset_held(ForceReason::HookReinstalled));
            log::info!("hook klávesnice odebrán (žádný zapnutý ovladač)");
            HHOOK::default()
        }
        _ => hook,
    }
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
    match nainstaluj() {
        Ok(h) => {
            status.nainstalovan.store(true, Ordering::Release);
            status.instalaci.fetch_add(1, Ordering::AcqRel);
            nahlas_chybu(false);
            log::info!("hook klávesnice přeinstalován");
            h
        }
        Err(e) => {
            status.nainstalovan.store(false, Ordering::Release);
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

fn prikaz(e: &mut Engine, p: HookPrikaz, ted: u64) -> Decision {
    match p {
        HookPrikaz::Mapovani(m) => e.set_mapping(*m),
        HookPrikaz::Povol(pad) => e.enable(pad),
        HookPrikaz::Zakaz(pad, duvod) => e.disable(pad, duvod),
        HookPrikaz::Prepni => e.toggle(ted),
        HookPrikaz::Zachytavej => e.capture(ted),
        HookPrikaz::Vynut(duvod) => e.force_keyboard(duvod),
        HookPrikaz::Zapomen(duvod) => e.reset_held(duvod),
        // Přeinstalaci dělá smyčka; bez hooku v systému není co dělat.
        HookPrikaz::Preinstaluj => Decision::NONE,
    }
}

/// Zavolá engine mimo callback a rozhodnutí předá dál.
fn s_enginem(f: impl FnOnce(&mut Engine) -> Decision) {
    STAV.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(s) = s.as_mut().filter(|s| !s.rozbity) {
            let d = f(&mut s.engine);
            s.vystup.rozhodnuti(None, &d, s.engine.mode());
        }
    });
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
    let u = Udalost::z(kb, dolu);
    STAV.with(|s| {
        // Zanořené volání (nemělo by nastat — callback nic nepumpuje)
        // nebo vlákno bez stavu: propustit, nikdy neblokovat.
        let Ok(mut s) = s.try_borrow_mut() else {
            return false;
        };
        let Some(s) = s.as_mut().filter(|s| !s.rozbity) else {
            return false;
        };
        if u.vstrcena() {
            s.vystup
                .rozhodnuti(Some(&u), &Decision::NONE, s.engine.mode());
            return false;
        }
        let ted = ted_ms();
        // Watchdog: klávesy do zaseknutého ovladače by jen mizely
        // (princip 1) — hra dál vidí jeho poslední stav a uživatel nemůže
        // ani psát. Proto Klávesnice; zachytávání vrátí zkratka.
        if s.engine.mode() == Mode::Gamepad && zaseknuty(s, ted) {
            let d = s.engine.force_keyboard(ForceReason::Watchdog);
            s.vystup.rozhodnuti(None, &d, s.engine.mode());
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
        if dolu
            && s.engine.held(u.klavesa).is_none()
            && ((s.os_drzi)(u.vk) || (s.os_drzi)(VK_LWIN) || (s.os_drzi)(VK_RWIN))
        {
            let _ = s.engine.adopt_os_key(u.klavesa, ted);
        }
        let d = s.engine.on_key(u.klavesa, dolu, ted);
        s.vystup.rozhodnuti(Some(&u), &d, s.engine.mode());
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
                    s.vystup.rozhodnuti(None, &d, s.engine.mode());
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

#[cfg(test)]
mod tests {
    use super::*;
    use keypad_core::{Action, ModeCause, PadAction, PadButton, PadState, UiEvent};
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
        tep: std::sync::atomic::AtomicU64,
    }

    impl Vystup for Zaznam {
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
    }

    impl Zaznam {
        fn posledni(&self) -> Radek {
            *self.rozhodnuti.lock().unwrap().last().unwrap()
        }
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
        STAV.with(|s| {
            *s.borrow_mut() = Some(Stav {
                engine,
                vystup,
                rozbity: false,
                os_drzi: nic_nedrzi,
                povoleno_ms: [0; MAX_PADS],
            })
        });
    }

    fn nic_nedrzi(_vk: u32) -> bool {
        false
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

    /// Callback nic nealokuje — ani při stisku, autorepeatu, key-upu,
    /// přepnutí zkratkou, nemapovatelné či vstříknuté klávese, ani když
    /// OS drží Win a klávesa se převezme jako klávesa OS.
    #[test]
    fn callback_nealokuje() {
        struct Tichy(AtomicU32);
        impl Vystup for Tichy {
            fn rozhodnuti(&self, _: Option<&Udalost>, _: &Decision, _: Mode) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        // Skutečný dotaz na stav klávesy — i ten musí být bez alokace;
        // podvrh „drží Win“ projde větví převzetí klávesy OS.
        let dotazy: [fn(u32) -> bool; 2] = [os_drzi, jen_win];
        for dotaz in dotazy {
            let mut engine = Engine::new(Mapping::default());
            let _ = engine.enable(PadId::FIRST);
            let vystup: Arc<dyn Vystup> = Arc::new(Tichy(AtomicU32::new(0)));
            STAV.with(|s| {
                *s.borrow_mut() = Some(Stav {
                    engine,
                    vystup,
                    rozbity: false,
                    os_drzi: dotaz,
                    povoleno_ms: [0; MAX_PADS],
                })
            });
            // Scroll Lock (přepnutí), W, šipka, AltGr, média, vstříknutá,
            // Win.
            let udalosti = [
                (0x46, 0),
                (0x11, 0),
                (0x48, LLKHF_EXTENDED.0),
                (0x21D, 0),
                (0, 0),
                (0x1E, LLKHF_INJECTED.0),
                (0x5B, LLKHF_EXTENDED.0),
            ];
            let pred = crate::testy_alokace::pocet();
            for _ in 0..3 {
                for &(scan, flags) in &udalosti {
                    let k = kb(scan, flags);
                    zavolej(0, WM_KEYDOWN, &k);
                    zavolej(0, WM_KEYDOWN, &k);
                    zavolej(0, WM_KEYUP, &kb(scan, flags | LLKHF_UP.0));
                }
            }
            assert_eq!(crate::testy_alokace::pocet(), pred, "alokace v callbacku");
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
        assert!(!zavolej(0, WM_KEYDOWN, &kb_l(0)), "pravá Win+L do Windows");
        assert!(!zavolej(0, WM_KEYUP, &kb_l(LLKHF_UP.0)));
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
            let _ = g.as_mut().unwrap().engine.start_binding(cil, ted_ms());
        });
        podvrhni_os(jen_win);
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
        assert!(zavolej(0, WM_KEYDOWN, &kb_l(0)));
        assert!(
            matches!(z.posledni().1.ui, Some(UiEvent::BindingSaved { key, target, .. }) if key == l && target == cil)
        );
    }

    /// Skutečný hook na skryté ploše: bez zapnutého ovladače v systému
    /// není, s prvním povoleným se nainstaluje, zachytávání z pozastavení
    /// ho přeinstaluje, s posledním zakázaným zmizí; konec pošle neutrál.
    /// Mapování jen na F23/F24 — na ploše vlastníka nic nezmění.
    #[test]
    fn hook_jen_se_zapnutym_ovladacem() {
        let z = Arc::new(Zaznam::default());
        let vystup: Arc<dyn Vystup> = z.clone();
        let mut hook = spust_s(mapovani(), vystup, testy_plocha::na_skryte_plose).unwrap();
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
        let pockej_na = |co: &dyn Fn() -> bool| {
            let konec = std::time::Instant::now() + LIMIT;
            while !co() {
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

    /// Konec při zachytávání: neutrál všem ovladačům ještě před
    /// odebráním hooku (pořadí neutrál → odhooknout).
    #[test]
    fn konec_pri_zachytavani_posle_neutral() {
        let z = Arc::new(Zaznam::default());
        let vystup: Arc<dyn Vystup> = z.clone();
        let mut hook = spust_s(mapovani(), vystup, testy_plocha::na_skryte_plose).unwrap();
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
        let mut hook = spust_s(mapovani(), vystup, testy_plocha::na_skryte_plose).unwrap();
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
            let _ = e.start_binding(PadAction::first(Action::Button(PadButton::B)), ted_ms());
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

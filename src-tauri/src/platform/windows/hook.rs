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
//! do logu) dělá táž smyčka mimo callback. Zapnutí hooku z přepínače
//! přijde ve Fázi 4 — do té doby ho používá jen příklad `hook_selftest`
//! a testy.

// Aplikace hook zapne až přepínačem ve Fázi 4; teď ho volají jen testy
// a příklad `hook_selftest`, a to každý jinou část.
#![allow(dead_code)]

use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use keypad_core::{Decision, DisabledReason, Engine, ForceReason, KeyId, Mapping, Mode};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, KillTimer, PeekMessageW, PostThreadMessageW,
    SetTimer, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT,
    LLKHF_EXTENDED, LLKHF_INJECTED, MSG, PM_NOREMOVE, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_KEYUP,
    WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER,
};

/// Ve frontě kanálu čekají příkazy (probuzení smyčky).
const WM_PRIKAZ: u32 = WM_APP + 1;
/// Callback spadl do paniky — smyčka to zaloguje (callback logovat nesmí).
const WM_PANIKA: u32 = WM_APP + 2;

/// Krok časovače při přiřazování klávesy. Timeout přiřazování je 10 s;
/// o čtvrt vteřiny později je pořád „po deseti vteřinách". Časovač běží
/// jen během přiřazování — nečinný hook nikdy netiká (princip 10).
const TIK_MS: u32 = 250;

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
}

/// Příkazy hook vláknu. Každý vede na volání enginu a jeho rozhodnutí
/// jde do [`Vystup`]u stejně jako rozhodnutí o klávese.
#[derive(Debug, Clone)]
pub enum HookPrikaz {
    /// Nové mapování (v režimu Gamepad nejdřív vynutí Klávesnici).
    Mapovani(Box<Mapping>),
    /// Pad je připojený — z `Disabled` na Klávesnici.
    Povol,
    /// Pad není — do `Disabled`.
    Zakaz(DisabledReason),
    /// Přepnout Klávesnice ↔ Gamepad (tlačítko v okně).
    Prepni,
    /// Vynutit Klávesnici (fail-safe).
    Vynut(ForceReason),
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

/// Běžící hook vlákno. Drop ho zastaví.
pub struct Hook {
    tid: u32,
    tx: Sender<HookPrikaz>,
    status: Arc<HookStatus>,
    konec: Receiver<()>,
    vlakno: Option<std::thread::JoinHandle<()>>,
}

impl Hook {
    /// Spustí vlákno, nainstaluje hook a vrátí se, až hook běží.
    ///
    /// Engine startuje jako vždy v `Disabled { PadNotConnected }` —
    /// klávesy zachytávat začne až po [`HookPrikaz::Povol`] a přepnutí.
    pub fn spust(mapovani: Mapping, vystup: Arc<dyn Vystup>) -> Result<Hook, String> {
        spust_s(mapovani, vystup, || Ok(()))
    }

    pub fn status(&self) -> &Arc<HookStatus> {
        &self.status
    }

    /// Pošle příkaz a probudí smyčku. `false` = vlákno už neběží.
    pub fn posli(&self, p: HookPrikaz) -> bool {
        if self.tx.send(p).is_err() {
            return false;
        }
        // SAFETY: jen odeslání zprávy do fronty vlákna; neexistující
        // vlákno = chyba, kterou vrátí volání.
        unsafe { PostThreadMessageW(self.tid, WM_PRIKAZ, WPARAM(0), LPARAM(0)) }.is_ok()
    }

    /// Odhookne a ukončí vlákno. Engine před koncem vynutí Klávesnici
    /// (neutrální pad jde do [`Vystup`]u). `false` = vlákno neskončilo
    /// v limitu (proces ho pak ukončí sám; hook zmizí s ním).
    pub fn zastav(&mut self) -> bool {
        let Some(vlakno) = self.vlakno.take() else {
            return true;
        };
        // SAFETY: jen odeslání zprávy do fronty vlákna.
        let _ = unsafe { PostThreadMessageW(self.tid, WM_QUIT, WPARAM(0), LPARAM(0)) };
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
                Ok(()) => vlakno(Engine::new(mapovani), vystup, st, rx, &hotovo_tx),
                Err(e) => {
                    let _ = hotovo_tx.send(Err(e));
                }
            }
            let _ = konec_tx.send(());
        })
        .map_err(|e| format!("hook vlákno nejde spustit: {e}"))?;
    match hotovo_rx.recv_timeout(LIMIT) {
        Ok(Ok(tid)) => Ok(Hook {
            tid,
            tx,
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

/// Stav, který potřebuje callback. Žije v `thread_local!` hook vlákna:
/// callback nemá kontext a jiné vlákno k enginu nesmí.
struct Stav {
    engine: Engine,
    vystup: Arc<dyn Vystup>,
    /// Po panice selhal i úklid — engine se už nevolá.
    rozbity: bool,
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
        })
    });
    let mut hook = match nainstaluj() {
        Ok(h) => h,
        Err(e) => {
            STAV.with(|s| s.borrow_mut().take());
            let _ = hotovo.send(Err(e));
            return;
        }
    };
    status.nainstalovan.store(true, Ordering::Release);
    status.instalaci.fetch_add(1, Ordering::AcqRel);
    log::info!("hook klávesnice nainstalován");
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
                    if matches!(p, HookPrikaz::Preinstaluj) {
                        hook = preinstaluj(hook, &status);
                        continue;
                    }
                    s_enginem(|e| prikaz(e, p, ted_ms()));
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
        casovac = hlidej_casovac(casovac);
    }

    if !hook.is_invalid() {
        odhookni(hook);
    }
    status.nainstalovan.store(false, Ordering::Release);
    if casovac != 0 {
        // SAFETY: časovač vlákna vytvořený v `hlidej_casovac`.
        let _ = unsafe { KillTimer(None, casovac) };
    }
    // Hook je pryč, nic už klávesy nepotlačí: pad musí dostat neutrál.
    s_enginem(|e| e.force_keyboard(ForceReason::Shutdown));
    STAV.with(|s| s.borrow_mut().take());
    log::info!("hook klávesnice odebrán");
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
            log::info!("hook klávesnice přeinstalován");
            h
        }
        Err(e) => {
            status.nainstalovan.store(false, Ordering::Release);
            log::error!("hook klávesnice nejde znovu nainstalovat: {e}");
            HHOOK::default()
        }
    }
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
        HookPrikaz::Povol => e.enable(),
        HookPrikaz::Zakaz(duvod) => e.disable(duvod),
        HookPrikaz::Prepni => e.toggle(ted),
        HookPrikaz::Vynut(duvod) => e.force_keyboard(duvod),
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
        let d = s.engine.on_key(u.klavesa, dolu, ted_ms());
        s.vystup.rozhodnuti(Some(&u), &d, s.engine.mode());
        d.suppress
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
    use keypad_core::{Action, ModeCause, PadButton, PadState, UiEvent};
    use std::sync::Mutex;
    use windows::Win32::UI::WindowsAndMessaging::{KBDLLHOOKSTRUCT_FLAGS, LLKHF_UP};

    /// F24 → A, zkratka F23: klávesy, které na ploše vlastníka nikdo
    /// nezmáčkne — kdyby test hooku přece jen viděl skutečnou klávesnici.
    const F23: KeyId = KeyId::new(0x6E);
    const F24: KeyId = KeyId::new(0x76);

    fn mapovani() -> Mapping {
        Mapping::new(F23, [(F24, Action::Button(PadButton::A))]).unwrap()
    }

    /// Jedno předané rozhodnutí: událost, rozhodnutí, režim po něm.
    type Radek = (Option<Udalost>, Decision, Mode);

    #[derive(Default)]
    struct Zaznam {
        rozhodnuti: Mutex<Vec<Radek>>,
        /// Kolikátým voláním zpanikařit (0 = nikdy).
        panikar: AtomicU32,
        volani: AtomicU32,
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
    }

    impl Zaznam {
        fn posledni(&self) -> Radek {
            *self.rozhodnuti.lock().unwrap().last().unwrap()
        }
    }

    /// Stav callbacku na tomhle vlákně bez skutečného hooku: engine
    /// povolený a přepnutý na Gamepad.
    fn priprav(z: &Arc<Zaznam>) {
        let mut engine = Engine::new(mapovani());
        let _ = engine.enable();
        let _ = engine.toggle(0);
        assert_eq!(engine.mode(), Mode::Gamepad);
        let vystup: Arc<dyn Vystup> = z.clone();
        STAV.with(|s| {
            *s.borrow_mut() = Some(Stav {
                engine,
                vystup,
                rozbity: false,
            })
        });
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
        assert!(d.pad.unwrap().is_pressed(PadButton::A));
        // Autorepeat se potlačí taky, key-up vrátí neutrál.
        assert!(zavolej(0, WM_KEYDOWN, &kb(0x76, 0)));
        assert!(zavolej(0, WM_KEYUP, &kb(0x76, LLKHF_UP.0)));
        assert_eq!(z.posledni().1.pad, Some(PadState::NEUTRAL));
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
        assert_eq!(d.pad, Some(PadState::NEUTRAL));
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
    /// přepnutí zkratkou, nemapovatelné či vstříknuté klávese.
    #[test]
    fn callback_nealokuje() {
        struct Tichy(AtomicU32);
        impl Vystup for Tichy {
            fn rozhodnuti(&self, _: Option<&Udalost>, _: &Decision, _: Mode) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let mut engine = Engine::new(Mapping::default());
        let _ = engine.enable();
        let vystup: Arc<dyn Vystup> = Arc::new(Tichy(AtomicU32::new(0)));
        STAV.with(|s| {
            *s.borrow_mut() = Some(Stav {
                engine,
                vystup,
                rozbity: false,
            })
        });
        // Scroll Lock (přepnutí), W, šipka, AltGr, média, vstříknutá.
        let udalosti = [
            (0x46, 0),
            (0x11, 0),
            (0x48, LLKHF_EXTENDED.0),
            (0x21D, 0),
            (0, 0),
            (0x1E, LLKHF_INJECTED.0),
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

    /// Skutečný hook na skryté ploše: instalace, příkazy, přeinstalace
    /// a konec. Mapování jen na F23/F24 — na ploše vlastníka nic nezmění.
    #[test]
    fn vlakno_nainstaluje_prepina_preinstaluje_a_uklidi() {
        let z = Arc::new(Zaznam::default());
        let vystup: Arc<dyn Vystup> = z.clone();
        let mut hook = spust_s(mapovani(), vystup, testy_plocha::na_skryte_plose).unwrap();
        let st = Arc::clone(hook.status());
        assert!(st.nainstalovan());
        assert_eq!(st.instalaci(), 1);

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

        assert!(hook.posli(HookPrikaz::Povol));
        assert!(hook.posli(HookPrikaz::Prepni));
        pockej(&|r| r.2 == Mode::Gamepad);

        assert!(hook.posli(HookPrikaz::Preinstaluj));
        pockej(&|r| {
            r.1.ui
                == Some(UiEvent::ModeChanged {
                    mode: Mode::Keyboard,
                    cause: ModeCause::Forced(ForceReason::HookReinstalled),
                })
        });
        let konec = std::time::Instant::now() + LIMIT;
        while st.instalaci() < 2 {
            assert!(std::time::Instant::now() < konec);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(st.nainstalovan());

        assert!(hook.zastav());
        assert!(!st.nainstalovan());
        let (u, d, m) = z.posledni();
        assert_eq!(
            (u, m, d.pad),
            (None, Mode::Keyboard, Some(PadState::NEUTRAL))
        );
        // Po konci vlákna příkazy neprojdou.
        assert!(!hook.posli(HookPrikaz::Prepni));
        assert!(hook.zastav(), "druhé zastavení nic nedělá");
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
            let _ = e
                .start_binding(Action::Button(PadButton::B), ted_ms())
                .unwrap();
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
    use windows::core::w;
    use windows::Win32::System::StationsAndDesktops::{
        CreateDesktopW, SetThreadDesktop, DESKTOP_CONTROL_FLAGS,
    };

    /// Přesune volající vlákno na novou skrytou plochu. Plocha zanikne,
    /// až ji přestane používat poslední vlákno.
    pub fn na_skryte_plose() -> Result<(), String> {
        // SAFETY: vytvoření plochy s výchozími právy; handle se nechává
        // otevřený po celý život vlákna (SetThreadDesktop ho potřebuje).
        unsafe {
            let plocha = CreateDesktopW(
                w!("KeyPadTestHook"),
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

//! Okno instalátoru — vlastní kreslení přes GDI.
//!
//! Proč ne dialog z resource nebo hotová knihovna: instalátor je jediný
//! soubor, který dostane kamarád, a má vypadat jako zbytek aplikace —
//! tmavé plochy, tenké rámečky, klidné barvy. Standardní ovládací prvky
//! Windows tohle neumí a knihovna navíc by binárku nafoukla o megabajty
//! kvůli jednomu oknu. (Port okna z WinSentu — stejné tokeny i rozvržení.)
//!
//! Kreslí se do paměťového DC a teprve hotový obrázek se přenese na
//! obrazovku — jinak by při každém překreslení problikávalo pozadí.
//!
//! Barvy jsou tytéž tokeny jako v `ui/src/app.css`. Písmo je Segoe UI:
//! aplikace používá Space Grotesk, ale ten je v ní zabalený jako webfont
//! (woff2), který GDI neumí — Segoe je přesně ten fallback, který má
//! aplikace ve svém `--font-ui`.

use std::sync::{Arc, Mutex};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, InvalidateRect,
    RoundRect, ScreenToClient, SelectObject, SetBkMode, SetTextColor, ANTIALIASED_QUALITY,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DRAW_TEXT_FORMAT, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT,
    DT_NOCLIP, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, FF_DONTCARE, FW_BOLD, FW_NORMAL, HBRUSH,
    HDC, HFONT, HGDIOBJ, HPEN, OUT_TT_PRECIS, PAINTSTRUCT, PS_SOLID, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE, VK_RETURN, VK_SHIFT,
    VK_SPACE, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, DrawIconEx, GetClientRect,
    GetCursorPos, GetMessageW, GetSystemMetrics, LoadCursorW, LoadIconW, PostMessageW,
    PostQuitMessage, RegisterClassW, SetCursor, SetWindowPos, ShowWindow, TranslateMessage,
    CS_DROPSHADOW, CS_HREDRAW, CS_VREDRAW, DI_NORMAL, HICON, HTCAPTION, IDC_ARROW, IDC_HAND, MSG,
    SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW, WM_APP, WM_CLOSE, WM_DESTROY,
    WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_MOUSEMOVE,
    WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WM_SETCURSOR, WNDCLASSW, WS_POPUP, WS_VISIBLE,
};

/// windows-rs má tuhle zprávu v `Win32_UI_Controls` — kvůli jedné
/// konstantě se ta obří feature (celé common controls) netahá.
const WM_MOUSELEAVE: u32 = 0x02A3;

// ── Barvy (app.css) ────────────────────────────────────────────────
const BG: u32 = rgb(0x0e, 0x0f, 0x12);
const PANEL: u32 = rgb(0x16, 0x17, 0x1c);
const SURFACE: u32 = rgb(0x1a, 0x1b, 0x21);
const BORDER: u32 = rgb(0x27, 0x28, 0x2e);
const TEXT: u32 = rgb(0xec, 0xec, 0xef);
const TEXT_DIM: u32 = rgb(0x9a, 0x9a, 0xa1);
const TEXT_FAINT: u32 = rgb(0x5c, 0x5c, 0x63);
const ACCENT: u32 = rgb(0xff, 0xff, 0xff);
const OK: u32 = rgb(0x4a, 0xde, 0x80);
const WARN: u32 = rgb(0xf5, 0x9e, 0x0b);
const DANGER: u32 = rgb(0xef, 0x44, 0x44);

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

/// Logická velikost okna (při 100 % DPI).
///
/// O kus vyšší než ve WinSentu: závěrečná zpráva tu umí mít tři
/// odstavce (chybějící WebView2, výsledek instalace ovladače ViGEmBus
/// s adresou, plus případné varování) a musí se vejít mezi kroky a pruh,
/// aniž by se ořízla — a kroků je s ovladačem šest. Nejdelší skutečná
/// kombinace se vejde běžným písmem (test `nejdelsi_zprava_se_vejde_do_okna`
/// a ladicí náhled `KEYPAD_SETUP_TEST_NAHLED=nejdelsi`); na delší, než se
/// čekalo (dlouhá chybová hláška Windows, víc poznámek), má okno menší
/// písmo — viz [`Fit`].
const W: i32 = 560;
const H: i32 = 540;

/// Písma okna, v pořadí přednosti (viz `pick_face`).
const UI_FACES: &[&str] = &["Segoe UI Variable Text", "Segoe UI"];
const MONO_FACES: &[&str] = &["Cascadia Mono", "Consolas"];
/// Velikosti písem v bodech.
const PT_BODY: i32 = 10;
const PT_SMALL: i32 = 9;

/// Výška hlavičky — zároveň plocha, za kterou jde okno táhnout.
const HEAD_H: i32 = 62;

/// Zpráva „stav se změnil, překresli".
const WM_TICK: u32 = WM_APP + 1;

/// V jaké fázi instalátor je.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Čeká na uživatele.
    Ready,
    /// Pracuje se.
    Working,
    Done,
    Failed,
}

/// Stav jednoho kroku.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Waiting,
    Active,
    Done,
    Failed,
    /// Uživatel krok vypnul (ovladač ViGEmBus) — neprovede se.
    Skipped,
}

/// Odkaz, který po dokončení otevře hlavní tlačítko (stažení WebView2
/// nebo ViGEmBus). Adresa v textu zprávy se z GDI okna zkopírovat nedá
/// — tlačítko ano.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub label: String,
    pub url: String,
}

/// Co spustí hlavní tlačítko.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    /// Hlavní práce okna (instalace, odinstalace, v režimu `/vigembus`
    /// instalace ovladače).
    Main,
    /// Jen instalace ovladače ViGEmBus — „Zkusit znovu ovladač" po
    /// dokončené instalaci KeyPadu. KeyPad se znovu nestahuje.
    Driver,
}

/// Co nabídne hlavní tlačítko po dokončení.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Otevřít stránku ke stažení.
    Link(Link),
    /// Spustit [`Job::Driver`]; nese popisek tlačítka.
    Driver(String),
}

impl Next {
    fn label(&self) -> &str {
        match self {
            Next::Link(l) => &l.label,
            Next::Driver(label) => label,
        }
    }
}

/// Přepínač na úvodní obrazovce (instalace ovladače ViGEmBus).
///
/// Viditelný a vysvětlený předem (princip 8): co se stáhne, odkud,
/// a že Windows budou chtít povolení správce. Ovládá se myší i
/// klávesnicí (Tab na něj přesune fokus, mezerník ho přepne).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toggle {
    pub label: String,
    /// Menší text pod popiskem.
    pub note: String,
    pub on: bool,
    /// Krok, který přepínač zapíná a vypíná.
    pub step: usize,
}

/// Co okno kreslí. Sdílené s pracovním vláknem.
#[derive(Debug, Clone)]
pub struct State {
    pub phase: Phase,
    pub title: String,
    pub subtitle: String,
    /// Poznámka v patičce, dokud se nezačalo („co se stane").
    pub footer: String,
    /// Kroky a jejich stav.
    pub steps: Vec<(String, StepState)>,
    /// 0.0–1.0; `None` = neznámo (kreslí se neurčitý pruh).
    pub progress: Option<f32>,
    /// Řádek pod pruhem — co se právě děje.
    pub status: String,
    /// Závěrečná zpráva (hotovo / chyba).
    pub message: String,
    /// Hotovo, ale uživatel musí něco udělat nebo vědět (chybí WebView2,
    /// KeyPad se nespustil, ovladač se nenainstaloval, poznámka „Pozor:").
    /// Zpráva se kreslí výstražnou barvou a okno se samo nezavře ani
    /// v tichém režimu — jinak by pokyn zmizel dřív, než by ho kdo přečetl.
    pub attention: bool,
    /// Co nabídne hlavní tlačítko po dokončení.
    pub next: Option<Next>,
    /// Přepínač na úvodní obrazovce (kreslí se jen ve fázi `Ready`).
    pub option: Option<Toggle>,
    /// Naposledy spuštěná práce — „Zkusit znovu" po chybě spustí tutéž.
    pub job: Job,
    /// Popisek hlavního tlačítka; prázdný = tlačítko není.
    pub primary: String,
    pub secondary: String,
}

impl State {
    pub fn new(title: &str, subtitle: &str, footer: &str, steps: &[&str], primary: &str) -> Self {
        State {
            phase: Phase::Ready,
            title: title.into(),
            subtitle: subtitle.into(),
            footer: footer.into(),
            steps: steps
                .iter()
                .map(|s| ((*s).to_string(), StepState::Waiting))
                .collect(),
            progress: None,
            status: String::new(),
            message: String::new(),
            attention: false,
            next: None,
            option: None,
            job: Job::Main,
            primary: primary.into(),
            secondary: "Zavřít".into(),
        }
    }

    /// Přidá přepínač pro krok `step` (zapnutý podle `on`).
    pub fn with_option(mut self, toggle: Toggle) -> Self {
        let (step, on) = (toggle.step, toggle.on);
        self.option = Some(toggle);
        self.set_option(on);
        debug_assert!(step < self.steps.len());
        self
    }

    /// Zapne/vypne krok přepínače. Vypnutý krok zůstává v seznamu
    /// (s poznámkou „vynechá se") — jinak by se seznam při kliknutí na přepínač
    /// zkracoval a přepínač by uživateli ujel zpod myši.
    pub fn set_option(&mut self, on: bool) {
        if let Some(t) = self.option.as_mut() {
            t.on = on;
            if let Some(s) = self.steps.get_mut(t.step) {
                s.1 = if on {
                    StepState::Waiting
                } else {
                    StepState::Skipped
                };
            }
        }
    }

    /// Chce uživatel i krok přepínače (ovladač)?
    pub fn option_on(&self) -> bool {
        self.option.as_ref().is_some_and(|t| t.on)
    }

    /// Označí krok jako běžící a všechny předchozí jako hotové.
    /// Vypnuté kroky zůstávají vypnuté.
    pub fn step(&mut self, idx: usize, status: &str) {
        for (i, s) in self.steps.iter_mut().enumerate() {
            if s.1 == StepState::Skipped && i != idx {
                continue;
            }
            s.1 = match i.cmp(&idx) {
                std::cmp::Ordering::Less => StepState::Done,
                std::cmp::Ordering::Equal => StepState::Active,
                std::cmp::Ordering::Greater => StepState::Waiting,
            };
        }
        self.status = status.into();
    }

    pub fn finish(&mut self, message: &str, attention: bool, next: Option<Next>) {
        for s in self.steps.iter_mut() {
            if s.1 != StepState::Skipped {
                s.1 = StepState::Done;
            }
        }
        self.phase = Phase::Done;
        self.progress = Some(1.0);
        self.status = String::new();
        self.message = message.into();
        self.attention = attention;
        self.primary = next
            .as_ref()
            .map(|n| n.label().to_string())
            .unwrap_or_default();
        self.next = next;
        self.secondary = "Zavřít".into();
    }

    pub fn fail(&mut self, message: &str) {
        if let Some(s) = self.steps.iter_mut().find(|s| s.1 == StepState::Active) {
            s.1 = StepState::Failed;
        }
        self.phase = Phase::Failed;
        self.status = String::new();
        self.message = message.into();
        self.attention = false;
        self.next = None;
        self.primary = "Zkusit znovu".into();
        self.secondary = "Zavřít".into();
    }

    /// Nový běh (první spuštění, „Zkusit znovu", „Zkusit znovu ovladač").
    fn begin(&mut self, job: Job) {
        self.phase = Phase::Working;
        self.job = job;
        self.message.clear();
        self.status.clear();
        self.attention = false;
        self.next = None;
        self.progress = None;
        self.primary.clear();
        self.secondary.clear();
        let last = self.steps.len().saturating_sub(1);
        let skipped = self.option.as_ref().filter(|t| !t.on).map(|t| t.step);
        for (i, s) in self.steps.iter_mut().enumerate() {
            s.1 = match job {
                Job::Main if Some(i) == skipped => StepState::Skipped,
                Job::Main => StepState::Waiting,
                // Ovladač je vždy poslední krok; předchozí (KeyPad) už
                // hotové jsou — znovu se neprovádějí.
                Job::Driver if i < last => StepState::Done,
                Job::Driver => StepState::Waiting,
            };
        }
    }
}

pub type Shared = Arc<Mutex<State>>;

/// Okno pro vlákno, které mění stav — po každé změně si řekne o překreslení.
#[derive(Clone, Copy)]
pub struct Notifier(isize);
// SAFETY: posílá se jen HWND jako číslo; PostMessageW je z jiných vláken
// bezpečné volání (na rozdíl od SendMessageW).
unsafe impl Send for Notifier {}

impl Notifier {
    /// Okno instalátoru — vlastník výzvy UAC (ta se pak ukáže nad ním,
    /// ne schovaná za ostatními okny).
    pub fn hwnd(&self) -> isize {
        self.0
    }

    pub fn tick(&self) {
        if self.0 != 0 {
            // SAFETY: PostMessageW jen zařadí zprávu do fronty okna.
            unsafe {
                let _ = PostMessageW(Some(HWND(self.0 as *mut _)), WM_TICK, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// Co má okno udělat, když uživatel klikne na hlavní tlačítko.
pub type Action = Arc<dyn Fn(Job, Shared, Notifier) + Send + Sync + 'static>;

struct Win {
    state: Shared,
    action: Action,
    /// Spustit akci hned po otevření okna (tichý režim / aktualizace).
    autostart: bool,
    /// Zavřít okno samo, jakmile je hotovo — aktualizace z aplikace,
    /// kde uživatel klikl už jednou a nemá co potvrzovat podruhé.
    autoclose: bool,
    dpi: i32,
    /// Písma vybraná podle toho, co v systému opravdu je (viz `pick_face`).
    face_ui: HSTRING,
    face_mono: HSTRING,
    font_title: HFONT,
    font_body: HFONT,
    font_small: HFONT,
    font_mono: HFONT,
    icon: HICON,
    /// Obdélníky tlačítek (počítají se při kreslení, používají při kliku).
    btn_primary: RECT,
    btn_secondary: RECT,
    btn_close: RECT,
    btn_toggle: RECT,
    hot: u8,
    /// Ovládací prvek s fokusem klávesnice (`HOT_*`). Výchozí je hlavní
    /// tlačítko — Enter tak dělá totéž co dřív.
    focus: u8,
    /// Kreslit rámeček fokusu? Až po prvním Tabu — jako Windows: kdo
    /// ovládá myší, rámeček nepotřebuje.
    focus_visible: bool,
    /// Prvek, na kterém byl v tomhle okně stisknutý mezerník (`HOT_NONE`
    /// = žádný). Mezerník působí až při puštění, jako u tlačítek Windows.
    space_armed: u8,
    /// Je zapnuté hlídání odjezdu myši z okna (WM_MOUSELEAVE)?
    tracking: bool,
    /// Snímek neurčitého pruhu — posouvá se, dokud se pracuje.
    anim: i32,
}

const HOT_NONE: u8 = 0;
const HOT_PRIMARY: u8 = 1;
const HOT_SECONDARY: u8 = 2;
const HOT_CLOSE: u8 = 3;
const HOT_TOGGLE: u8 = 4;

/// Otevře okno a nechá ho běžet, dokud ho uživatel nezavře.
///
/// `action` se spustí na vlastním vlákně — kreslení nesmí čekat na síť
/// ani na zavírání aplikace, jinak okno zamrzne a vypadá jako spadlé.
pub fn run(window_title: &str, state: Shared, action: Action, autostart: bool, autoclose: bool) {
    // SAFETY: standardní životní cyklus okna; GDI objekty se uklízejí
    // ve WM_DESTROY, stav okna ve WM_NCDESTROY.
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        // Ikona z resource (build.rs ji zabuduje pod ID 1). Když ikona
        // při buildu chyběla, je handle neplatný a hlavička se kreslí bez ní.
        let icon = LoadIconW(Some(hinst.into()), PCWSTR(1 as _)).unwrap_or_default();
        let class = w!("KeyPadSetupWnd");
        let wc = WNDCLASSW {
            // Stín u popup okna — bez něj plave tmavý obdélník na ploše.
            style: CS_HREDRAW | CS_VREDRAW | CS_DROPSHADOW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            // Ikona na hlavním panelu a v Alt+Tab.
            hIcon: icon,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let win = Box::new(Win {
            state,
            action,
            autostart,
            autoclose,
            dpi: 96,
            face_ui: pick_face(UI_FACES),
            face_mono: pick_face(MONO_FACES),
            font_title: HFONT::default(),
            font_body: HFONT::default(),
            font_small: HFONT::default(),
            font_mono: HFONT::default(),
            icon,
            btn_primary: RECT::default(),
            btn_secondary: RECT::default(),
            btn_close: RECT::default(),
            btn_toggle: RECT::default(),
            hot: HOT_NONE,
            focus: HOT_PRIMARY,
            focus_visible: false,
            space_armed: HOT_NONE,
            tracking: false,
            anim: 0,
        });
        // Ladicí build: rámeček fokusu hned od startu (`KEYPAD_SETUP_TEST_FOKUS`
        // = `prepinac` | `hlavni` | `zavrit`) — kvůli snímku obrazovky bez
        // posílání kláves do okna.
        #[cfg(debug_assertions)]
        let win = {
            let mut win = win;
            if let Ok(f) = std::env::var("KEYPAD_SETUP_TEST_FOKUS") {
                win.focus_visible = true;
                win.focus = match f.as_str() {
                    "prepinac" => HOT_TOGGLE,
                    "zavrit" => HOT_SECONDARY,
                    _ => HOT_PRIMARY,
                };
            }
            win
        };
        let ptr = Box::into_raw(win);

        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);
        let hwnd = CreateWindowExW(
            Default::default(),
            class,
            &HSTRING::from(window_title),
            WS_POPUP | WS_VISIBLE,
            (sw - W) / 2,
            (sh - H) / 2,
            W,
            H,
            None,
            None,
            Some(hinst.into()),
            Some(ptr as *mut _),
        );
        let Ok(hwnd) = hwnd else {
            drop(Box::from_raw(ptr));
            return;
        };

        round_corners(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// DPI okna.
///
/// `GetDpiForWindow` je až od Windows 10 1607. Staticky importovaná by
/// instalátor na 1507/1511 vůbec nespustila („vstupní bod nenalezen")
/// dřív, než by stihl cokoli říct (princip 9) — proto se hledá za běhu.
/// Bez ní stačí systémové DPI z `GetDeviceCaps`; monitor s jiným
/// měřítkem pak pošle WM_DPICHANGED s novou hodnotou (manifest na
/// starých buildech hlásí per-monitor v1 přes `true/pm`).
fn dpi_of_window(hwnd: HWND) -> i32 {
    use std::sync::OnceLock;
    use windows::core::s;
    use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, LOGPIXELSX};
    use windows::Win32::System::LibraryLoader::GetProcAddress;

    type GetDpi = unsafe extern "system" fn(HWND) -> u32;
    static FN: OnceLock<Option<GetDpi>> = OnceLock::new();
    let f = *FN.get_or_init(|| {
        // SAFETY: user32 už v procesu je (statický import) — GetModuleHandle
        // nic nenačítá ani nehledá na disku (žádné podstrčení DLL).
        // Podpis funkce odpovídá dokumentaci (HWND → UINT).
        unsafe {
            let m = GetModuleHandleW(w!("user32.dll")).ok()?;
            GetProcAddress(m, s!("GetDpiForWindow"))
                .map(|p| std::mem::transmute::<unsafe extern "system" fn() -> isize, GetDpi>(p))
        }
    });
    let dpi = match f {
        // SAFETY: platné okno; funkce jen čte.
        Some(f) => (unsafe { f(hwnd) }) as i32,
        // SAFETY: DC okna se hned vrací.
        None => unsafe {
            let dc = GetDC(Some(hwnd));
            let d = GetDeviceCaps(Some(dc), LOGPIXELSX);
            ReleaseDC(Some(hwnd), dc);
            d
        },
    };
    dpi.max(96)
}

/// Zaoblené rohy na Windows 11. Na starších systémech atribut neexistuje
/// a volání tiše selže — okno je pak hranaté, což nic nerozbíjí.
fn round_corners(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE};
    // DWMWCP_ROUND
    let pref: u32 = 2;
    // SAFETY: atribut i velikost odpovídají dokumentaci.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const _,
            std::mem::size_of::<u32>() as u32,
        );
    }
}

unsafe fn win_of(hwnd: HWND) -> Option<&'static mut Win> {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, GWLP_USERDATA};
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Win;
    if p.is_null() {
        None
    } else {
        Some(&mut *p)
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowLongPtrW, CREATESTRUCTW, GWLP_USERDATA, WM_NCCREATE, WM_TIMER,
    };
    // SAFETY: standardní obsluha zpráv; ukazatel na Win drží okno až do
    // WM_NCDESTROY, kde se uvolní a z okna odpojí.
    unsafe {
        if msg == WM_NCCREATE {
            let cs = lp.0 as *const CREATESTRUCTW;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
            if let Some(win) = win_of(hwnd) {
                win.dpi = dpi_of_window(hwnd);
                make_fonts(win);
                // Okno se otevírá v logické velikosti — na displeji se
                // 150 % by jinak bylo o třetinu menší, než má být.
                let s = |v: i32| v * win.dpi / 96;
                let sw = GetSystemMetrics(SM_CXSCREEN);
                let sh = GetSystemMetrics(SM_CYSCREEN);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    (sw - s(W)) / 2,
                    (sh - s(H)) / 2,
                    s(W),
                    s(H),
                    SWP_NOZORDER,
                );
                // Animace neurčitého pruhu — 30 snímků za sekundu.
                use windows::Win32::UI::WindowsAndMessaging::SetTimer;
                SetTimer(Some(hwnd), 1, 33, None);
                if win.autostart {
                    start(hwnd, win, Job::Main);
                }
            }
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        if msg == WM_NCDESTROY {
            // Poslední zpráva okna: stav se odpojí a uvolní. Pracovní
            // vlákno drží vlastní Arc na State, takže o nic nepřijde.
            let p = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut Win;
            if !p.is_null() {
                drop(Box::from_raw(p));
            }
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let Some(win) = win_of(hwnd) else {
            return DefWindowProcW(hwnd, msg, wp, lp);
        };
        match msg {
            WM_ERASEBKGND => LRESULT(1),
            WM_TIMER | WM_TICK => {
                let phase = win.state.lock().map(|s| (s.phase, s.attention));
                // Aktualizace z aplikace: uživatel klikl v aplikaci, tak
                // se okno po dokončení zavře samo. U chyby a u `attention`
                // (chybí WebView2, KeyPad se nespustil, „Pozor:") zůstane —
                // tam je co číst.
                if win.autoclose && matches!(phase, Ok((Phase::Done, false))) {
                    let _ = DestroyWindow(hwnd);
                    return LRESULT(0);
                }
                // Časovač překresluje jen kvůli animaci pruhu. V klidu
                // (čeká se na klik, hotovo) by 30 překreslení za sekundu
                // jen pálilo procesor pro stále týž obrázek.
                let working = matches!(phase, Ok((Phase::Working, _)));
                if msg == WM_TICK || working {
                    if working {
                        win.anim = (win.anim + 1) % 1000;
                    }
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_PAINT => {
                paint(hwnd, win);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let x = (lp.0 & 0xffff) as i16 as i32;
                let y = ((lp.0 >> 16) & 0xffff) as i16 as i32;
                let hot = hit(win, x, y);
                if hot != win.hot {
                    win.hot = hot;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                // Odjezd myši z okna WM_MOUSEMOVE nepošle — bez hlídání
                // by zvýraznění křížku u okraje zůstalo svítit.
                if !win.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    win.tracking = TrackMouseEvent(&mut tme).is_ok();
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                win.tracking = false;
                if win.hot != HOT_NONE {
                    win.hot = HOT_NONE;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_SETCURSOR => {
                let mut p = POINT::default();
                let _ = GetCursorPos(&mut p);
                let _ = ScreenToClient(hwnd, &mut p);
                if hit(win, p.x, p.y) != HOT_NONE {
                    SetCursor(LoadCursorW(None, IDC_HAND).ok());
                    return LRESULT(1);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_LBUTTONDOWN => {
                let x = (lp.0 & 0xffff) as i16 as i32;
                let y = ((lp.0 >> 16) & 0xffff) as i16 as i32;
                match hit(win, x, y) {
                    HOT_PRIMARY => primary(hwnd, win),
                    HOT_SECONDARY | HOT_CLOSE => {
                        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                    HOT_TOGGLE => {
                        win.focus = HOT_TOGGLE;
                        flip_option(hwnd, win);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                // Celé okno jde projít bez myši: Tab přesouvá fokus mezi
                // přepínačem a tlačítky, mezerník aktivuje prvek s fokusem
                // (při puštění, viz WM_KEYUP), Enter tlačítko s fokusem
                // (u přepínače hlavní tlačítko — jako výchozí tlačítko
                // dialogu), Esc zavírá.
                //
                // Opakování podržené klávesy (bit 30 = klávesa už dole
                // byla) se ignoruje. Podržený mezerník by jinak přepínač
                // cvakal sem a tam podle rychlosti opakování a podržený
                // Enter — třeba ten, kterým uživatel instalátor spustil
                // v Průzkumníku nebo v seznamu stažených souborů — by
                // v čerstvě otevřeném okně rovnou spustil instalaci
                // i s ovladačem, dřív než by si kdo přečetl, co udělá.
                if is_repeat(lp) {
                    return LRESULT(0);
                }
                let controls = focusable(win);
                if !controls.contains(&win.focus) {
                    win.focus = controls.first().copied().unwrap_or(HOT_NONE);
                }
                match key_down(wp.0 as u16, win.focus, &mut win.space_armed) {
                    KeyAct::Tab if !controls.is_empty() => {
                        let back = GetKeyState(VK_SHIFT.0 as i32) < 0;
                        let i = controls.iter().position(|&c| c == win.focus).unwrap_or(0);
                        let n = controls.len();
                        win.focus = controls[if back { (i + n - 1) % n } else { (i + 1) % n }];
                        win.focus_visible = true;
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    KeyAct::Activate(c) => activate(hwnd, win, c),
                    KeyAct::Close => {
                        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_KEYUP => {
                if let KeyAct::Activate(c) = key_up(wp.0 as u16, win.focus, &mut win.space_armed) {
                    activate(hwnd, win, c);
                }
                LRESULT(0)
            }
            WM_KILLFOCUS => {
                // Mezerník stisknutý tady a puštěný v jiném okně nic nespustí.
                win.space_armed = HOT_NONE;
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_NCHITTEST => {
                // Tažení za hlavičku — okno nemá systémový rámeček.
                let mut p = POINT {
                    x: (lp.0 & 0xffff) as i16 as i32,
                    y: ((lp.0 >> 16) & 0xffff) as i16 as i32,
                };
                let _ = ScreenToClient(hwnd, &mut p);
                let s = |v: i32| v * win.dpi / 96;
                if p.y < s(HEAD_H) && hit(win, p.x, p.y) == HOT_NONE {
                    return LRESULT(HTCAPTION as isize);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_DPICHANGED => {
                // Přetažení na monitor s jiným měřítkem: nová písma
                // a velikost, kterou navrhnou Windows (drží okno pod
                // kurzorem). Bez toho by text zůstal ve staré velikosti.
                win.dpi = ((wp.0 & 0xffff) as i32).max(96);
                delete_fonts(win);
                make_fonts(win);
                let r = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_CLOSE => {
                // Během práce se zavřít nedá: přerušená instalace nechá
                // soubory půl na půl. Tlačítko je v té fázi schválně
                // skryté, křížek taky.
                if matches!(win.state.lock().map(|s| s.phase), Ok(Phase::Working)) {
                    return LRESULT(0);
                }
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                delete_fonts(win);
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Hlavní tlačítko: po dokončení otevře stránku ke stažení nebo spustí
/// instalaci ovladače, jinak spustí (znovu) práci.
fn primary(hwnd: HWND, win: &mut Win) {
    let (phase, next, job, has_button) = match win.state.lock() {
        Ok(s) => (s.phase, s.next.clone(), s.job, !s.primary.is_empty()),
        Err(_) => return,
    };
    if !has_button {
        return;
    }
    match (phase, next) {
        (Phase::Done, Some(Next::Link(l))) => open_url(&l.url),
        (Phase::Done, Some(Next::Driver(_))) => start(hwnd, win, Job::Driver),
        (Phase::Ready, _) => start(hwnd, win, Job::Main),
        (Phase::Failed, _) => start(hwnd, win, job),
        _ => {}
    }
}

/// Je WM_KEYDOWN jen opakování podržené klávesy? (Bit 30 `lParam` =
/// předchozí stav klávesy: už byla dole.)
fn is_repeat(lp: LPARAM) -> bool {
    (lp.0 >> 30) & 1 != 0
}

/// Co okno udělá s klávesou. Čisté rozhodnutí zvlášť od okna — posílat
/// oknu klávesy v testech nejde (a nesmí), takže se testuje tohle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyAct {
    None,
    /// Fokus na další / předchozí prvek (podle Shiftu).
    Tab,
    /// Aktivovat prvek (`HOT_*`).
    Activate(u8),
    Close,
}

/// První stisk klávesy (opakování sem nechodí — viz `is_repeat`).
/// `armed` = prvek, na kterém je stisknutý mezerník.
fn key_down(vk: u16, focus: u8, armed: &mut u8) -> KeyAct {
    match vk {
        v if v == VK_TAB.0 => {
            *armed = HOT_NONE;
            KeyAct::Tab
        }
        // Mezerník se jen „natáhne" — působí až při puštění.
        v if v == VK_SPACE.0 => {
            *armed = focus;
            KeyAct::None
        }
        // Enter hned při prvním stisku: tlačítko s fokusem, u přepínače
        // hlavní tlačítko (jako výchozí tlačítko dialogu).
        v if v == VK_RETURN.0 => KeyAct::Activate(match focus {
            HOT_SECONDARY => HOT_SECONDARY,
            _ => HOT_PRIMARY,
        }),
        v if v == VK_ESCAPE.0 => KeyAct::Close,
        _ => KeyAct::None,
    }
}

/// Puštění klávesy. Mezerník působí při puštění — jako tlačítka
/// a zaškrtávací políčka Windows — a jen když byl stisknutý v tomhle
/// okně na tomtéž prvku. Puštění klávesy stisknuté jinde (před
/// otevřením okna, v jiném okně) tak nic nespustí.
fn key_up(vk: u16, focus: u8, armed: &mut u8) -> KeyAct {
    if vk != VK_SPACE.0 {
        return KeyAct::None;
    }
    let was = std::mem::replace(armed, HOT_NONE);
    if was != HOT_NONE && was == focus {
        KeyAct::Activate(was)
    } else {
        KeyAct::None
    }
}

/// Aktivuje ovládací prvek z klávesnice (mezerník).
fn activate(hwnd: HWND, win: &mut Win, control: u8) {
    match control {
        HOT_TOGGLE => flip_option(hwnd, win),
        HOT_SECONDARY => {
            // SAFETY: PostMessageW jen zařadí zprávu do fronty okna.
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        HOT_PRIMARY => primary(hwnd, win),
        _ => {}
    }
}

/// Ovládací prvky, na které jde Tabem, v pořadí Tabu.
fn focusable(win: &Win) -> Vec<u8> {
    let Ok(st) = win.state.lock() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if st.phase == Phase::Ready && st.option.is_some() {
        out.push(HOT_TOGGLE);
    }
    if !st.primary.is_empty() {
        out.push(HOT_PRIMARY);
    }
    if !st.secondary.is_empty() {
        out.push(HOT_SECONDARY);
    }
    out
}

/// Přepne přepínač (jen dokud se nezačalo — pak už o ničem nerozhoduje).
fn flip_option(hwnd: HWND, win: &mut Win) {
    if let Ok(mut st) = win.state.lock() {
        if st.phase == Phase::Ready {
            let on = !st.option_on();
            st.set_option(on);
        }
    }
    // SAFETY: překreslení vlastního okna.
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Otevře adresu ve výchozím prohlížeči. Instalátor běží pod
/// uživatelem, takže prohlížeč dostane jeho běžná práva.
fn open_url(url: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    // SAFETY: řetězce žijí po celou dobu volání; vrácený pseudohandle
    // se nezavírá.
    unsafe {
        let _ = ShellExecuteW(
            None,
            &HSTRING::from("open"),
            &HSTRING::from(url),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// Spustí práci na vlastním vlákně.
fn start(hwnd: HWND, win: &mut Win, job: Job) {
    {
        let Ok(mut st) = win.state.lock() else { return };
        if st.phase == Phase::Working {
            return;
        }
        st.begin(job);
    }
    win.hot = HOT_NONE;
    let shared = Arc::clone(&win.state);
    let note = Notifier(hwnd.0 as isize);
    // Akce se pouští opakovaně (tlačítko "Zkusit znovu"), takže si ji
    // vlákno bere přes Arc — okno si svou kopii nechává.
    let f = Arc::clone(&win.action);
    std::thread::spawn(move || {
        f(job, Arc::clone(&shared), note);
        note.tick();
    });
    note.tick();
}

fn hit(win: &Win, x: i32, y: i32) -> u8 {
    let inside = |r: &RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
    if inside(&win.btn_close) {
        HOT_CLOSE
    } else if win.btn_primary.right != 0 && inside(&win.btn_primary) {
        HOT_PRIMARY
    } else if win.btn_secondary.right != 0 && inside(&win.btn_secondary) {
        HOT_SECONDARY
    } else if win.btn_toggle.right != 0 && inside(&win.btn_toggle) {
        HOT_TOGGLE
    } else {
        HOT_NONE
    }
}

/// První písmo ze seznamu, které v systému opravdu je.
///
/// GDI za neexistující jméno tiše dosadí „nejbližší" písmo — a to umí
/// být Arial místo Segoe nebo proporcionální místo neproporcionálního.
/// Segoe UI Variable je jen na Windows 11, Cascadia Mono jen s novějším
/// Windows / Terminalem; proto se to ověří, ne odhaduje.
fn pick_face(candidates: &[&str]) -> HSTRING {
    use windows::Win32::Graphics::Gdi::{GetDC, GetTextFaceW, ReleaseDC};
    for name in candidates {
        let face = HSTRING::from(*name);
        // SAFETY: dočasné písmo i DC obrazovky se v téže větvi uvolní.
        let found = unsafe {
            let font = CreateFontW(
                -12,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_TT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                ANTIALIASED_QUALITY,
                FF_DONTCARE.0 as u32,
                &face,
            );
            let hdc = GetDC(None);
            let old = SelectObject(hdc, font.into());
            let mut buf = [0u16; 64];
            let n = GetTextFaceW(hdc, Some(&mut buf)).max(0) as usize;
            SelectObject(hdc, old);
            ReleaseDC(None, hdc);
            let _ = DeleteObject(font.into());
            let got = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
            got.trim_end_matches('\0').eq_ignore_ascii_case(name)
        };
        if found {
            return face;
        }
    }
    // Poslední kandidát jako nouzovka — GDI si s ním nějak poradí.
    HSTRING::from(*candidates.last().unwrap_or(&"Segoe UI"))
}

/// Písmo `pt` bodů při `dpi`; uvolňuje volající (`DeleteObject`).
fn create_font(dpi: i32, pt: i32, weight: i32, face: &HSTRING) -> HFONT {
    // SAFETY: jen vytvoření fontu; uklidí ho volající.
    unsafe {
        CreateFontW(
            -(pt * dpi / 72),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_TT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            FF_DONTCARE.0 as u32,
            face,
        )
    }
}

fn make_fonts(win: &mut Win) {
    let normal = FW_NORMAL.0 as i32;
    win.font_title = create_font(win.dpi, 15, FW_BOLD.0 as i32, &win.face_ui);
    win.font_body = create_font(win.dpi, PT_BODY, normal, &win.face_ui);
    win.font_small = create_font(win.dpi, PT_SMALL, normal, &win.face_ui);
    // Mono stack aplikace: Fira Mono → Cascadia Mono → Consolas
    // (Fira je v aplikaci jen jako webfont).
    win.font_mono = create_font(win.dpi, 8, normal, &win.face_mono);
}

fn delete_fonts(win: &mut Win) {
    for f in [
        &mut win.font_title,
        &mut win.font_body,
        &mut win.font_small,
        &mut win.font_mono,
    ] {
        if !f.is_invalid() {
            // SAFETY: font vytvořil `make_fonts` a už není vybraný v DC.
            unsafe {
                let _ = DeleteObject((*f).into());
            }
        }
        *f = HFONT::default();
    }
}

fn paint(hwnd: HWND, win: &mut Win) {
    // SAFETY: veškeré GDI objekty se v této funkci i uvolní.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);

        // Dvojitý buffer — bez něj probliká pozadí při každém tiku.
        let mem = CreateCompatibleDC(Some(hdc));
        let bmp = CreateCompatibleBitmap(hdc, rc.right, rc.bottom);
        let old = SelectObject(mem, bmp.into());
        draw(mem, &rc, win);
        let _ = BitBlt(hdc, 0, 0, rc.right, rc.bottom, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
    }
}

fn fill(hdc: HDC, r: RECT, color: u32) {
    // SAFETY: štětec se hned po použití uvolní.
    unsafe {
        let br = CreateSolidBrush(COLORREF(color));
        FillRect(hdc, &r, br);
        let _ = DeleteObject(br.into());
    }
}

/// Zaoblený obdélník s výplní a volitelným rámečkem.
fn round_box(hdc: HDC, r: RECT, radius: i32, fill_c: Option<u32>, border_c: Option<u32>) {
    // SAFETY: pero i štětec se uvolní; při None se použije průhledná
    // varianta přes NULL_BRUSH / NULL_PEN.
    unsafe {
        use windows::Win32::Graphics::Gdi::{GetStockObject, NULL_BRUSH, NULL_PEN};
        let pen: HPEN = match border_c {
            Some(c) => CreatePen(PS_SOLID, 1, COLORREF(c)),
            None => HPEN(GetStockObject(NULL_PEN).0),
        };
        let brush: HBRUSH = match fill_c {
            Some(c) => CreateSolidBrush(COLORREF(c)),
            None => HBRUSH(GetStockObject(NULL_BRUSH).0),
        };
        let op = SelectObject(hdc, pen.into());
        let ob = SelectObject(hdc, brush.into());
        let _ = RoundRect(hdc, r.left, r.top, r.right, r.bottom, radius, radius);
        SelectObject(hdc, op);
        SelectObject(hdc, ob);
        if border_c.is_some() {
            let _ = DeleteObject(HGDIOBJ(pen.0));
        }
        if fill_c.is_some() {
            let _ = DeleteObject(HGDIOBJ(brush.0));
        }
    }
}

fn text(hdc: HDC, r: RECT, s: &str, font: HFONT, color: u32, flags: DRAW_TEXT_FORMAT) {
    // SAFETY: buffer žije po celou dobu volání DrawTextW.
    unsafe {
        let old = SelectObject(hdc, font.into());
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(color));
        let mut wide: Vec<u16> = s.encode_utf16().collect();
        let mut rr = r;
        if !wide.is_empty() {
            DrawTextW(hdc, &mut wide, &mut rr, flags);
        }
        SelectObject(hdc, old);
    }
}

/// Výška zalomeného textu v šířce `r` — ať se pod něj dá kreslit dál.
fn text_height(hdc: HDC, r: RECT, s: &str, font: HFONT) -> i32 {
    use windows::Win32::Graphics::Gdi::DT_CALCRECT;
    // SAFETY: DT_CALCRECT jen měří, nic nekreslí.
    unsafe {
        let old = SelectObject(hdc, font.into());
        let mut wide: Vec<u16> = s.encode_utf16().collect();
        let mut rr = r;
        if !wide.is_empty() {
            DrawTextW(
                hdc,
                &mut wide,
                &mut rr,
                DT_LEFT | DT_WORDBREAK | DT_CALCRECT,
            );
        }
        SelectObject(hdc, old);
        rr.bottom - rr.top
    }
}

/// Jak se zpráva vejde do okna — od nejlepšího. DrawText ořezává
/// potichu a u závěrečné zprávy bývá nejdůležitější právě konec
/// (adresa ruční instalace, „Pozor:"), proto se měří, ne odhaduje.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Běžným písmem mezi kroky a pruhem průběhu.
    Body,
    /// Menším písmem mezi kroky a pruhem.
    Small,
    /// Menším písmem až k tlačítkům; pruh se nekreslí (po dokončení je
    /// plný a nic neříká — zpráva je důležitější).
    SmallOverBar,
    /// Nevejde se ani tak: kreslí se jako `SmallOverBar` a konec se
    /// ořízne. Na nejdelší skutečnou zprávu nedojde (test v main.rs).
    Clipped,
}

/// Obdélníky zprávy při `dpi`: pod kroky a nad pruhem průběhu, a totéž
/// až k tlačítkům. Stejná čísla jako v `draw` (pruh `bottom - 108`,
/// tlačítka 34 vysoká s okrajem 24).
fn message_rects(dpi: i32, rc: &RECT, steps: usize) -> (RECT, RECT) {
    let s = |v: i32| v * dpi / 96;
    let normal = RECT {
        left: s(24),
        top: s(HEAD_H) + s(22) + s(26) * steps as i32 + s(8),
        right: rc.right - s(24),
        bottom: rc.bottom - s(108) - s(6),
    };
    let over_bar = RECT {
        bottom: rc.bottom - s(24) - s(34) - s(10),
        ..normal
    };
    (normal, over_bar)
}

/// Změří, jak se `text` vejde (`may_cover_bar` = pruh smí ustoupit —
/// jen když se nepracuje, jinak by zmizel neurčitý průběh).
fn fit_message(
    hdc: HDC,
    fonts: (HFONT, HFONT),
    text: &str,
    normal: RECT,
    over_bar: RECT,
    may_cover_bar: bool,
) -> Fit {
    let (body, small) = fonts;
    let fits = |font, r: RECT| text_height(hdc, r, text, font) <= r.bottom - r.top;
    if fits(body, normal) {
        Fit::Body
    } else if fits(small, normal) {
        Fit::Small
    } else if may_cover_bar && fits(small, over_bar) {
        Fit::SmallOverBar
    } else {
        Fit::Clipped
    }
}

/// Změří zprávu přesně tak, jak by ji kreslilo okno při 100 % DPI
/// (paměťové DC, žádné okno) — pro test délky a výběr nejdelší zprávy
/// v ladicím náhledu. Vrací výšku běžným písmem a jak se vejde (mimo
/// práci, kdy smí ustoupit pruh).
#[cfg(any(test, debug_assertions))]
fn measure_96(text: &str, steps: usize) -> (i32, Fit) {
    measure_96_with(text, steps, true)
}

#[cfg(any(test, debug_assertions))]
fn measure_96_with(text: &str, steps: usize, may_cover_bar: bool) -> (i32, Fit) {
    let face = pick_face(UI_FACES);
    let body = create_font(96, PT_BODY, FW_NORMAL.0 as i32, &face);
    let small = create_font(96, PT_SMALL, FW_NORMAL.0 as i32, &face);
    let rc = RECT {
        left: 0,
        top: 0,
        right: W,
        bottom: H,
    };
    let (normal, over_bar) = message_rects(96, &rc, steps);
    // SAFETY: paměťové DC i obě písma se v této funkci uvolní.
    unsafe {
        let hdc = CreateCompatibleDC(None);
        let h = text_height(hdc, normal, text, body);
        let fit = fit_message(hdc, (body, small), text, normal, over_bar, may_cover_bar);
        let _ = DeleteDC(hdc);
        let _ = DeleteObject(body.into());
        let _ = DeleteObject(small.into());
        (h, fit)
    }
}

/// Výška zprávy běžným písmem při 100 % DPI (px).
#[cfg(any(test, debug_assertions))]
pub fn message_height_96(text: &str, steps: usize) -> i32 {
    measure_96(text, steps).0
}

/// Jak se zpráva vejde do okna s `steps` kroky při 100 % DPI.
#[cfg(test)]
pub fn message_fit_96(text: &str, steps: usize) -> Fit {
    measure_96(text, steps).1
}

/// Rámeček fokusu klávesnice kolem `r`.
fn focus_ring(hdc: HDC, r: RECT, pad: i32, radius: i32) {
    round_box(
        hdc,
        RECT {
            left: r.left - pad,
            top: r.top - pad,
            right: r.right + pad,
            bottom: r.bottom + pad,
        },
        radius,
        None,
        Some(ACCENT),
    );
}

fn draw(hdc: HDC, rc: &RECT, win: &mut Win) {
    let s = |v: i32| v * win.dpi / 96;
    let st = match win.state.lock() {
        Ok(g) => g.clone(),
        Err(_) => return,
    };

    fill(hdc, *rc, BG);

    // ── Hlavička ──
    let head_h = s(HEAD_H);
    fill(
        hdc,
        RECT {
            left: 0,
            top: 0,
            right: rc.right,
            bottom: head_h,
        },
        PANEL,
    );
    fill(
        hdc,
        RECT {
            left: 0,
            top: head_h - 1,
            right: rc.right,
            bottom: head_h,
        },
        BORDER,
    );
    // Bez ikony (ještě nevygenerovaná při buildu) se text posune doleva,
    // ať v hlavičce nezeje díra.
    let text_left = if win.icon.is_invalid() {
        s(24)
    } else {
        // SAFETY: ikona patří modulu, neuvolňuje se.
        unsafe {
            let _ = DrawIconEx(
                hdc,
                s(20),
                s(15),
                win.icon,
                s(32),
                s(32),
                0,
                None,
                DI_NORMAL,
            );
        }
        s(64)
    };
    text(
        hdc,
        RECT {
            left: text_left,
            top: s(14),
            right: rc.right - s(50),
            bottom: s(34),
        },
        &st.title,
        win.font_title,
        TEXT,
        // Obdélník je přesně na výšku písma a DrawText ořezává — bez
        // NOCLIP by „y" v „KeyPad" přišlo o nožičku (WinSent v názvu
        // žádnou dolní dotahu nemá, proto to tam nebylo vidět).
        DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOCLIP,
    );
    text(
        hdc,
        RECT {
            left: text_left,
            top: s(33),
            right: rc.right - s(50),
            bottom: s(50),
        },
        &st.subtitle,
        win.font_small,
        TEXT_FAINT,
        // Podtitulek odinstalace je dlouhý výčet; kdyby se s jiným písmem
        // (Segoe UI Variable na Windows 11) nevešel, ať končí třemi
        // tečkami, ne useknutým písmenem pod křížkem.
        DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
    );

    // Křížek. Během práce se nekreslí — zavřít stejně nejde.
    if st.phase != Phase::Working {
        let c = RECT {
            left: rc.right - s(44),
            top: s(14),
            right: rc.right - s(14),
            bottom: s(44),
        };
        win.btn_close = c;
        if win.hot == HOT_CLOSE {
            round_box(hdc, c, s(6), Some(SURFACE), None);
        }
        text(
            hdc,
            c,
            "✕",
            win.font_body,
            if win.hot == HOT_CLOSE { TEXT } else { TEXT_DIM },
            DT_SINGLELINE | DT_VCENTER | DT_CENTER,
        );
    } else {
        win.btn_close = RECT::default();
    }

    // ── Kroky ──
    let mut y = head_h + s(22);
    for (label, state) in &st.steps {
        let dot = RECT {
            left: s(24),
            top: y + s(7),
            right: s(24) + s(8),
            bottom: y + s(15),
        };
        let (dot_c, txt_c) = match state {
            StepState::Waiting => (BORDER, TEXT_FAINT),
            StepState::Active => (ACCENT, TEXT),
            StepState::Done => (OK, TEXT_DIM),
            StepState::Failed => (DANGER, DANGER),
            StepState::Skipped => (BORDER, TEXT_FAINT),
        };
        // Vypnutý krok: prázdný kroužek a poznámka — aby bylo vidět, že
        // se na něj nezapomnělo, ale vědomě vynechá.
        let skipped = *state == StepState::Skipped;
        if skipped {
            round_box(hdc, dot, s(8), None, Some(dot_c));
        } else {
            round_box(hdc, dot, s(8), Some(dot_c), None);
        }
        let shown;
        let label = if skipped {
            shown = if st.phase == Phase::Done {
                format!("{label} — vynechán")
            } else {
                format!("{label} — vynechá se")
            };
            &shown
        } else {
            label
        };
        text(
            hdc,
            RECT {
                left: s(44),
                top: y,
                right: rc.right - s(24),
                bottom: y + s(22),
            },
            label,
            win.font_body,
            txt_c,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
        );
        y += s(26);
    }

    // ── Stavový řádek / zpráva ──
    // Zpráva sedí NAD pruhem: dole jsou tlačítka a poznámka, takže
    // delší hláška by se s nimi přetlačovala.
    let (mut msg_rect, mut over_bar) = message_rects(win.dpi, rc, st.steps.len());

    // ── Přepínač (jen před začátkem) ──
    win.btn_toggle = RECT::default();
    if let (Phase::Ready, Some(t)) = (st.phase, st.option.as_ref()) {
        let top = msg_rect.top + s(4);
        let sw = RECT {
            left: s(24),
            top: top + s(1),
            right: s(24) + s(36),
            bottom: top + s(1) + s(20),
        };
        let lab = RECT {
            left: sw.right + s(12),
            top,
            right: rc.right - s(24),
            bottom: msg_rect.bottom,
        };
        let lab_h = text_height(hdc, lab, &t.label, win.font_body).max(s(22));
        // Vypínač ve stylu aplikace: zapnutý = bílá dráha a tmavý
        // knoflík vpravo, vypnutý = tmavá dráha s rámečkem, knoflík vlevo.
        let knob = s(14);
        let kt = sw.top + (s(20) - knob) / 2;
        if t.on {
            round_box(hdc, sw, s(20), Some(ACCENT), None);
            let kr = RECT {
                left: sw.right - s(3) - knob,
                top: kt,
                right: sw.right - s(3),
                bottom: kt + knob,
            };
            round_box(hdc, kr, knob, Some(BG), None);
        } else {
            round_box(hdc, sw, s(20), Some(SURFACE), Some(BORDER));
            let kr = RECT {
                left: sw.left + s(3),
                top: kt,
                right: sw.left + s(3) + knob,
                bottom: kt + knob,
            };
            round_box(hdc, kr, knob, Some(TEXT_DIM), None);
        }
        let hot = win.hot == HOT_TOGGLE;
        text(
            hdc,
            lab,
            &t.label,
            win.font_body,
            if t.on || hot { TEXT } else { TEXT_DIM },
            DT_LEFT | DT_WORDBREAK,
        );
        let area = RECT {
            left: sw.left,
            top,
            right: lab.right,
            bottom: top + lab_h,
        };
        win.btn_toggle = area;
        if win.focus_visible && win.focus == HOT_TOGGLE {
            focus_ring(hdc, area, s(4), s(8));
        }
        let note_top = top + lab_h + s(6);
        if !t.note.is_empty() {
            let note = RECT {
                top: note_top,
                ..lab
            };
            text(
                hdc,
                note,
                &t.note,
                win.font_small,
                // Ne TEXT_FAINT: poznámka je vysvětlení k výzvě správce
                // a cizímu ovladači (souhlas) — musí se dát přečíst.
                TEXT_DIM,
                DT_LEFT | DT_WORDBREAK,
            );
            msg_rect.top = note_top + text_height(hdc, note, &t.note, win.font_small) + s(10);
        } else {
            msg_rect.top = note_top + s(4);
        }
    }
    over_bar.top = msg_rect.top;

    // Vejde se zpráva? Když ne, menší písmo, a když ani to ne, místo
    // pruhu (jen mimo práci — neurčitý pruh je jediný důkaz, že se
    // něco děje). Viz `Fit`.
    let fit = if st.message.is_empty() {
        Fit::Body
    } else {
        fit_message(
            hdc,
            (win.font_body, win.font_small),
            &st.message,
            msg_rect,
            over_bar,
            st.phase != Phase::Working,
        )
    };
    let covers_bar = st.phase != Phase::Working && matches!(fit, Fit::SmallOverBar | Fit::Clipped);

    // ── Pruh průběhu ──
    if !covers_bar {
        let bar_y = rc.bottom - s(108);
        let bar = RECT {
            left: s(24),
            top: bar_y,
            right: rc.right - s(24),
            bottom: bar_y + s(6),
        };
        round_box(hdc, bar, s(6), Some(SURFACE), None);
        let full = bar.right - bar.left;
        match st.progress {
            Some(p) if st.phase != Phase::Failed => {
                let wpx = (full as f32 * p.clamp(0.0, 1.0)) as i32;
                if wpx > s(6) {
                    let c = match st.phase {
                        Phase::Done if st.attention => WARN,
                        Phase::Done => OK,
                        _ => ACCENT,
                    };
                    round_box(
                        hdc,
                        RECT {
                            right: bar.left + wpx,
                            ..bar
                        },
                        s(6),
                        Some(c),
                        None,
                    );
                }
            }
            None if st.phase == Phase::Working => {
                // Neurčitý průběh: jezdec sem a tam. Stažení umí trvat
                // a zamrzlý pruh vypadá jako zamrzlý program.
                let seg = full / 4;
                let span = full - seg;
                let t = (win.anim % 120) as f32 / 120.0;
                let off = (((t * std::f32::consts::TAU).sin() * 0.5 + 0.5) * span as f32) as i32;
                round_box(
                    hdc,
                    RECT {
                        left: bar.left + off,
                        right: bar.left + off + seg,
                        ..bar
                    },
                    s(6),
                    Some(ACCENT),
                    None,
                );
            }
            _ => {}
        }
    }

    if !st.message.is_empty() {
        let (font, r) = match fit {
            Fit::Body => (win.font_body, msg_rect),
            Fit::Small => (win.font_small, msg_rect),
            _ if covers_bar => (win.font_small, over_bar),
            _ => (win.font_small, msg_rect),
        };
        text(
            hdc,
            r,
            &st.message,
            font,
            match st.phase {
                Phase::Failed => DANGER,
                Phase::Done if st.attention => WARN,
                Phase::Done => TEXT,
                _ => TEXT_DIM,
            },
            DT_LEFT | DT_WORDBREAK,
        );
    } else if !st.status.is_empty() {
        text(
            hdc,
            msg_rect,
            &st.status,
            win.font_mono,
            TEXT_DIM,
            DT_LEFT | DT_WORDBREAK,
        );
    }

    // ── Tlačítka ──
    let bh = s(34);
    let by = rc.bottom - s(24) - bh;
    let mut right = rc.right - s(24);
    win.btn_primary = RECT::default();
    win.btn_secondary = RECT::default();
    if !st.primary.is_empty() {
        let bw = s(150);
        let r = RECT {
            left: right - bw,
            top: by,
            right,
            bottom: by + bh,
        };
        let bg = if win.hot == HOT_PRIMARY {
            rgb(0xff, 0xff, 0xff)
        } else {
            rgb(0xe4, 0xe4, 0xe8)
        };
        round_box(hdc, r, s(6), Some(bg), None);
        text(
            hdc,
            r,
            &st.primary,
            win.font_body,
            rgb(0x10, 0x10, 0x14),
            DT_SINGLELINE | DT_VCENTER | DT_CENTER,
        );
        if win.focus_visible && win.focus == HOT_PRIMARY {
            focus_ring(hdc, r, s(3), s(9));
        }
        win.btn_primary = r;
        right -= bw + s(10);
    }
    if !st.secondary.is_empty() {
        let bw = s(110);
        let r = RECT {
            left: right - bw,
            top: by,
            right,
            bottom: by + bh,
        };
        round_box(
            hdc,
            r,
            s(6),
            Some(if win.hot == HOT_SECONDARY {
                SURFACE
            } else {
                PANEL
            }),
            Some(BORDER),
        );
        text(
            hdc,
            r,
            &st.secondary,
            win.font_body,
            if win.hot == HOT_SECONDARY {
                TEXT
            } else {
                TEXT_DIM
            },
            DT_SINGLELINE | DT_VCENTER | DT_CENTER,
        );
        if win.focus_visible && win.focus == HOT_SECONDARY {
            focus_ring(hdc, r, s(3), s(9));
        }
        win.btn_secondary = r;
    }

    // Patička s poznámkou o tom, co se stane — jen dokud se nezačalo.
    if st.phase == Phase::Ready && !st.footer.is_empty() {
        text(
            hdc,
            RECT {
                left: s(24),
                top: by,
                right: rc.right - s(290),
                bottom: by + bh,
            },
            &st.footer,
            win.font_small,
            TEXT_FAINT,
            DT_LEFT | DT_WORDBREAK,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st() -> State {
        State::new(
            "KeyPad",
            "podtitul",
            "patička",
            &["a", "b", "c"],
            "Nainstalovat",
        )
    }

    #[test]
    fn krok_oznaci_predchozi_jako_hotove() {
        let mut s = st();
        s.step(1, "dělám b");
        let states: Vec<_> = s.steps.iter().map(|x| x.1).collect();
        assert_eq!(
            states,
            [StepState::Done, StepState::Active, StepState::Waiting]
        );
        assert_eq!(s.status, "dělám b");
    }

    #[test]
    fn chyba_oznaci_bezici_krok_a_nabidne_opakovani() {
        let mut s = st();
        s.begin(Job::Main);
        s.step(2, "c");
        s.fail("spadlo to");
        assert_eq!(s.phase, Phase::Failed);
        assert_eq!(s.steps[2].1, StepState::Failed);
        assert_eq!(s.primary, "Zkusit znovu");
    }

    #[test]
    fn hotovo_s_odkazem_ma_tlacitko_na_stazeni() {
        let mut s = st();
        s.begin(Job::Main);
        s.finish(
            "chybí WebView2",
            true,
            Some(Next::Link(Link {
                label: "Stáhnout WebView2".into(),
                url: "https://example.invalid".into(),
            })),
        );
        assert_eq!(s.phase, Phase::Done);
        assert!(s.attention);
        assert_eq!(s.primary, "Stáhnout WebView2");
        assert!(s.steps.iter().all(|x| x.1 == StepState::Done));
    }

    #[test]
    fn hotovo_bez_odkazu_nema_hlavni_tlacitko() {
        let mut s = st();
        s.begin(Job::Main);
        s.finish("hotovo", false, None);
        assert!(s.primary.is_empty());
        assert_eq!(s.secondary, "Zavřít");
    }

    #[test]
    fn novy_beh_vymaze_minuly_vysledek() {
        let mut s = st();
        s.fail("x");
        s.begin(Job::Main);
        assert_eq!(s.phase, Phase::Working);
        assert!(s.message.is_empty() && s.primary.is_empty() && s.next.is_none());
        assert!(s.steps.iter().all(|x| x.1 == StepState::Waiting));
    }

    fn with_driver(on: bool) -> State {
        State::new("KeyPad", "", "", &["a", "b", "Ovladač"], "Nainstalovat").with_option(Toggle {
            label: "Nainstalovat i ovladač".into(),
            note: String::new(),
            on,
            step: 2,
        })
    }

    #[test]
    fn vypnuty_prepinac_krok_vynecha_a_zapnuty_vrati() {
        let mut s = with_driver(true);
        assert!(s.option_on());
        assert_eq!(s.steps[2].1, StepState::Waiting);
        s.set_option(false);
        assert!(!s.option_on());
        assert_eq!(s.steps[2].1, StepState::Skipped);
        // Seznam kroků se nezkracuje — přepínač neujede zpod myši.
        assert_eq!(s.steps.len(), 3);
        s.set_option(true);
        assert_eq!(s.steps[2].1, StepState::Waiting);
    }

    #[test]
    fn vynechany_krok_zustane_vynechany_az_do_konce() {
        let mut s = with_driver(false);
        s.begin(Job::Main);
        assert_eq!(s.steps[2].1, StepState::Skipped);
        s.step(1, "b");
        assert_eq!(s.steps[2].1, StepState::Skipped);
        s.finish("hotovo", false, None);
        assert_eq!(
            s.steps.iter().map(|x| x.1).collect::<Vec<_>>(),
            [StepState::Done, StepState::Done, StepState::Skipped]
        );
    }

    /// Čím delší zpráva, tím úspornější kreslení — a nikdy potichu
    /// useknutá, dokud se vejde aspoň menším písmem až k tlačítkům.
    /// Měří se GDI jako v okně (paměťové DC, žádné okno).
    #[test]
    fn dlouha_zprava_prejde_na_mensi_pismo() {
        let line = "Pozor: zástupce v nabídce Start se nepodařilo vytvořit — přístup odepřen.";
        let text = |n: usize| vec![line; n].join("\n");
        let fits: Vec<Fit> = (1..=16).map(|n| measure_96(&text(n), 6).1).collect();
        assert_eq!(fits[0], Fit::Body);
        assert_eq!(*fits.last().unwrap(), Fit::Clipped);
        // Pořadí se nikdy nevrací (Body → Small → SmallOverBar → Clipped)
        // a každý stupeň se opravdu použije.
        let rank = |f: &Fit| *f as u8;
        assert!(
            fits.windows(2).all(|w| rank(&w[0]) <= rank(&w[1])),
            "{fits:?}"
        );
        for f in [Fit::Body, Fit::Small, Fit::SmallOverBar, Fit::Clipped] {
            assert!(fits.contains(&f), "{f:?} v {fits:?}");
        }
        // Během práce pruh neustoupí: co by se vešlo jen místo pruhu, je
        // tam rovnou Clipped (neurčitý pruh je důkaz, že se pracuje).
        let n = fits.iter().position(|f| *f == Fit::SmallOverBar).unwrap() + 1;
        assert_eq!(measure_96_with(&text(n), 6, false).1, Fit::Clipped);
    }

    /// Opakování podržené klávesy (bit 30) se pozná; první stisk ne.
    #[test]
    fn opakovani_klavesy() {
        // Enter: počet opakování 1, scan kód 0x1C.
        assert!(!is_repeat(LPARAM(0x001C_0001)));
        assert!(is_repeat(LPARAM(0x401C_0001)));
    }

    /// Mezerník působí při puštění a jen na prvku, na kterém byl
    /// stisknutý; Enter hned (u přepínače hlavní tlačítko); Esc zavírá.
    #[test]
    fn klavesnice() {
        let (sp, en) = (VK_SPACE.0, VK_RETURN.0);
        let mut armed = HOT_NONE;
        // Stisk + puštění mezerníku na přepínači = jedno přepnutí.
        assert_eq!(key_down(sp, HOT_TOGGLE, &mut armed), KeyAct::None);
        assert_eq!(
            key_up(sp, HOT_TOGGLE, &mut armed),
            KeyAct::Activate(HOT_TOGGLE)
        );
        // Puštění bez stisku v tomhle okně (držel ho, když se okno
        // otevíralo) nic nedělá.
        assert_eq!(key_up(sp, HOT_PRIMARY, &mut armed), KeyAct::None);
        // Tab mezi stiskem a puštěním mezerník zruší.
        key_down(sp, HOT_TOGGLE, &mut armed);
        assert_eq!(key_down(VK_TAB.0, HOT_TOGGLE, &mut armed), KeyAct::Tab);
        assert_eq!(key_up(sp, HOT_PRIMARY, &mut armed), KeyAct::None);
        // Fokus se mezitím změnil jinak (myš) — taky nic.
        key_down(sp, HOT_TOGGLE, &mut armed);
        assert_eq!(key_up(sp, HOT_PRIMARY, &mut armed), KeyAct::None);
        // Mezerník na tlačítkách.
        key_down(sp, HOT_SECONDARY, &mut armed);
        assert_eq!(
            key_up(sp, HOT_SECONDARY, &mut armed),
            KeyAct::Activate(HOT_SECONDARY)
        );
        // Enter hned: tlačítko s fokusem, u přepínače hlavní.
        assert_eq!(
            key_down(en, HOT_TOGGLE, &mut armed),
            KeyAct::Activate(HOT_PRIMARY)
        );
        assert_eq!(
            key_down(en, HOT_PRIMARY, &mut armed),
            KeyAct::Activate(HOT_PRIMARY)
        );
        assert_eq!(
            key_down(en, HOT_SECONDARY, &mut armed),
            KeyAct::Activate(HOT_SECONDARY)
        );
        // Puštění Enteru nedělá nic (působí jen první stisk).
        assert_eq!(key_up(en, HOT_PRIMARY, &mut armed), KeyAct::None);
        assert_eq!(
            key_down(VK_ESCAPE.0, HOT_PRIMARY, &mut armed),
            KeyAct::Close
        );
    }

    #[test]
    fn dodatecny_ovladac_nezacina_instalaci_znovu() {
        let mut s = with_driver(false);
        s.begin(Job::Main);
        s.finish(
            "KeyPad hotový, ovladač vynechán",
            false,
            Some(Next::Driver("Nainstalovat ovladač".into())),
        );
        assert_eq!(s.primary, "Nainstalovat ovladač");
        s.begin(Job::Driver);
        assert_eq!(s.job, Job::Driver);
        assert_eq!(
            s.steps.iter().map(|x| x.1).collect::<Vec<_>>(),
            [StepState::Done, StepState::Done, StepState::Waiting]
        );
        // Chyba ovladače → „Zkusit znovu" spustí zase jen ovladač.
        s.step(2, "stahuji");
        s.fail("síť");
        assert_eq!(s.job, Job::Driver);
        assert_eq!(s.steps[2].1, StepState::Failed);
    }
}

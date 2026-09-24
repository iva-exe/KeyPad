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
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE, VK_RETURN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, DrawIconEx, GetClientRect,
    GetCursorPos, GetMessageW, GetSystemMetrics, LoadCursorW, LoadIconW, PostMessageW,
    PostQuitMessage, RegisterClassW, SetCursor, SetWindowPos, ShowWindow, TranslateMessage,
    CS_DROPSHADOW, CS_HREDRAW, CS_VREDRAW, DI_NORMAL, HICON, HTCAPTION, IDC_ARROW, IDC_HAND, MSG,
    SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW, WM_APP, WM_CLOSE, WM_DESTROY,
    WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_NCDESTROY,
    WM_NCHITTEST, WM_PAINT, WM_SETCURSOR, WNDCLASSW, WS_POPUP, WS_VISIBLE,
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
/// odstavce (chybějící WebView2 i ViGEmBus, oba s adresou, plus
/// případné varování) a musí se vejít mezi kroky a pruh, aniž by se
/// ořízla — ověřeno snímkem okna v nejdelší variantě (~8 řádků).
const W: i32 = 560;
const H: i32 = 492;

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
}

/// Odkaz, který po dokončení otevře hlavní tlačítko (stažení WebView2
/// nebo ViGEmBus). Adresa v textu zprávy se z GDI okna zkopírovat nedá
/// — tlačítko ano.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub label: String,
    pub url: String,
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
    /// KeyPad se nespustil, poznámka „Pozor:"). Zpráva se kreslí
    /// výstražnou barvou a okno se samo nezavře ani v tichém režimu —
    /// jinak by pokyn zmizel dřív, než by ho kdo přečetl.
    pub attention: bool,
    /// Kam vede hlavní tlačítko po dokončení.
    pub link: Option<Link>,
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
            link: None,
            primary: primary.into(),
            secondary: "Zavřít".into(),
        }
    }

    /// Označí krok jako běžící a všechny předchozí jako hotové.
    pub fn step(&mut self, idx: usize, status: &str) {
        for (i, s) in self.steps.iter_mut().enumerate() {
            s.1 = match i.cmp(&idx) {
                std::cmp::Ordering::Less => StepState::Done,
                std::cmp::Ordering::Equal => StepState::Active,
                std::cmp::Ordering::Greater => StepState::Waiting,
            };
        }
        self.status = status.into();
    }

    pub fn finish(&mut self, message: &str, attention: bool, link: Option<Link>) {
        for s in self.steps.iter_mut() {
            s.1 = StepState::Done;
        }
        self.phase = Phase::Done;
        self.progress = Some(1.0);
        self.status = String::new();
        self.message = message.into();
        self.attention = attention;
        self.primary = link.as_ref().map(|l| l.label.clone()).unwrap_or_default();
        self.link = link;
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
        self.link = None;
        self.primary = "Zkusit znovu".into();
        self.secondary = "Zavřít".into();
    }

    /// Nový běh (první spuštění i „Zkusit znovu").
    fn begin(&mut self) {
        self.phase = Phase::Working;
        self.message.clear();
        self.status.clear();
        self.attention = false;
        self.link = None;
        self.progress = None;
        self.primary.clear();
        self.secondary.clear();
        for s in self.steps.iter_mut() {
            s.1 = StepState::Waiting;
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
pub type Action = Arc<dyn Fn(Shared, Notifier) + Send + Sync + 'static>;

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
    hot: u8,
    /// Je zapnuté hlídání odjezdu myši z okna (WM_MOUSELEAVE)?
    tracking: bool,
    /// Snímek neurčitého pruhu — posouvá se, dokud se pracuje.
    anim: i32,
}

const HOT_NONE: u8 = 0;
const HOT_PRIMARY: u8 = 1;
const HOT_SECONDARY: u8 = 2;
const HOT_CLOSE: u8 = 3;

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
            face_ui: pick_face(&["Segoe UI Variable Text", "Segoe UI"]),
            face_mono: pick_face(&["Cascadia Mono", "Consolas"]),
            font_title: HFONT::default(),
            font_body: HFONT::default(),
            font_small: HFONT::default(),
            font_mono: HFONT::default(),
            icon,
            btn_primary: RECT::default(),
            btn_secondary: RECT::default(),
            btn_close: RECT::default(),
            hot: HOT_NONE,
            tracking: false,
            anim: 0,
        });
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
                win.dpi = GetDpiForWindow(hwnd).max(96) as i32;
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
                    start(hwnd, win);
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
                    _ => {}
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                // Enter = hlavní tlačítko, Esc = zavřít. Instalace se
                // tak dá projít i bez myši.
                let vk = wp.0 as u16;
                if vk == VK_RETURN.0 {
                    primary(hwnd, win);
                } else if vk == VK_ESCAPE.0 {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                LRESULT(0)
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

/// Hlavní tlačítko: po dokončení s odkazem otevře stránku ke stažení,
/// jinak spustí (znovu) práci.
fn primary(hwnd: HWND, win: &mut Win) {
    let (phase, link, has_button) = match win.state.lock() {
        Ok(s) => (s.phase, s.link.clone(), !s.primary.is_empty()),
        Err(_) => return,
    };
    if !has_button {
        return;
    }
    match (phase, link) {
        (Phase::Done, Some(l)) => open_url(&l.url),
        (Phase::Ready | Phase::Failed, _) => start(hwnd, win),
        _ => {}
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
fn start(hwnd: HWND, win: &mut Win) {
    {
        let Ok(mut st) = win.state.lock() else { return };
        if st.phase == Phase::Working {
            return;
        }
        st.begin();
    }
    win.hot = HOT_NONE;
    let shared = Arc::clone(&win.state);
    let note = Notifier(hwnd.0 as isize);
    // Akce se pouští opakovaně (tlačítko "Zkusit znovu"), takže si ji
    // vlákno bere přes Arc — okno si svou kopii nechává.
    let f = Arc::clone(&win.action);
    std::thread::spawn(move || {
        f(Arc::clone(&shared), note);
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

fn make_fonts(win: &mut Win) {
    let h = |pt: i32, weight: i32, face: &HSTRING| -> HFONT {
        // SAFETY: jen vytvoření fontu; uklidí se v `delete_fonts`.
        unsafe {
            CreateFontW(
                -(pt * win.dpi / 72),
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
    };
    win.font_title = h(15, FW_BOLD.0 as i32, &win.face_ui);
    win.font_body = h(10, FW_NORMAL.0 as i32, &win.face_ui);
    win.font_small = h(9, FW_NORMAL.0 as i32, &win.face_ui);
    // Mono stack aplikace: Fira Mono → Cascadia Mono → Consolas
    // (Fira je v aplikaci jen jako webfont).
    win.font_mono = h(8, FW_NORMAL.0 as i32, &win.face_mono);
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
        };
        round_box(hdc, dot, s(8), Some(dot_c), None);
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

    // ── Pruh průběhu ──
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

    // ── Stavový řádek / zpráva ──
    // Zpráva sedí NAD pruhem: dole jsou tlačítka a poznámka, takže
    // delší hláška by se s nimi přetlačovala.
    let msg_rect = RECT {
        left: s(24),
        top: head_h + s(22) + s(26) * st.steps.len() as i32 + s(8),
        right: rc.right - s(24),
        bottom: bar_y - s(6),
    };
    if !st.message.is_empty() {
        text(
            hdc,
            msg_rect,
            &st.message,
            win.font_body,
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
        s.begin();
        s.step(2, "c");
        s.fail("spadlo to");
        assert_eq!(s.phase, Phase::Failed);
        assert_eq!(s.steps[2].1, StepState::Failed);
        assert_eq!(s.primary, "Zkusit znovu");
    }

    #[test]
    fn hotovo_s_odkazem_ma_tlacitko_na_stazeni() {
        let mut s = st();
        s.begin();
        s.finish(
            "chybí WebView2",
            true,
            Some(Link {
                label: "Stáhnout WebView2".into(),
                url: "https://example.invalid".into(),
            }),
        );
        assert_eq!(s.phase, Phase::Done);
        assert!(s.attention);
        assert_eq!(s.primary, "Stáhnout WebView2");
        assert!(s.steps.iter().all(|x| x.1 == StepState::Done));
    }

    #[test]
    fn hotovo_bez_odkazu_nema_hlavni_tlacitko() {
        let mut s = st();
        s.begin();
        s.finish("hotovo", false, None);
        assert!(s.primary.is_empty());
        assert_eq!(s.secondary, "Zavřít");
    }

    #[test]
    fn novy_beh_vymaze_minuly_vysledek() {
        let mut s = st();
        s.fail("x");
        s.begin();
        assert_eq!(s.phase, Phase::Working);
        assert!(s.message.is_empty() && s.primary.is_empty() && s.link.is_none());
        assert!(s.steps.iter().all(|x| x.1 == StepState::Waiting));
    }
}

//! Zavření běžícího KeyPadu před přepsáním nebo smazáním jeho souborů.
//!
//! Pořadí je důležité: nejdřív SLUŠNĚ (WM_CLOSE hlavnímu oknu), teprve
//! když do ~3 s neskončí, NATVRDO. Slušné zavření dává aplikaci šanci
//! poslat neutrální stav padu, odhookovat klávesnici a odpojit virtuální
//! ovladač — v tomhle pořadí. Tvrdé ukončení to přeskočí; hook i pad
//! sice zmizí s procesem, ale hra by mohla zahlédnout poslední vychýlenou
//! páčku.
//!
//! WM_CLOSE posíláme sami, ne přes `taskkill` bez `/F`: ten pošle jedinou
//! zprávu prvnímu *viditelnému* oknu procesu v pořadí oken. U KeyPadu je
//! to často skryté okno pluginu single-instance (0×0, WS_EX_TOOLWINDOW,
//! přesto „viditelné") — hlavně když je hlavní okno minimalizované.
//! Zpráva pak zničí jen tohle pomocné okno, aplikace běží dál a po třech
//! vteřinách přijde `/F` (ověřeno na release buildu). Navíc by pak každé
//! další spuštění KeyPadu nenašlo běžící instanci a spustilo druhou.
//!
//! Hledá se podle **cesty** k binárce, ne jen podle jména: `taskkill
//! /IM KeyPad.exe` by zavřel i vývojový build z `target\` nebo
//! přenosnou kopii ze Stažených souborů — ty ale instalovaný soubor
//! nedrží a zavírat je není důvod.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowLongW, GetWindowThreadProcessId, IsWindowVisible,
    PostMessageW, GWL_EXSTYLE, WM_CLOSE, WS_EX_TOOLWINDOW,
};

/// Pomocné konzolové programy (taskkill, cmd) se spouští bez okna —
/// jinak by přes instalátor problikla černá konzole.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Cesta k programu ze `System32`.
///
/// Pomocné programy se volají plnou cestou, ne jménem: `Command::new`
/// by je jinak hledal nejdřív ve složce instalátoru — a ta bývá Stažené
/// soubory, kam může cizí `taskkill.exe` nebo `cmd.exe` podstrčit
/// kdejaká stránka.
pub fn system32(exe: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("windir"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    root.join("System32").join(exe)
}

/// Jak dlouho se čeká na slušné zavření.
const GRACEFUL: Duration = Duration::from_secs(3);
/// Jak dlouho se čeká po tvrdém ukončení. TerminateProcess je
/// asynchronní — soubor se uvolní až ve chvíli, kdy proces opravdu zmizí.
const FORCED: Duration = Duration::from_secs(2);
/// Po jaké době se během slušného čekání znovu hledají okna.
const POLL: Duration = Duration::from_millis(200);

/// Třída hlavního okna aplikace. Tauri ji dává každému oknu, pokud
/// `tauri.conf.json` nenastaví `windowClassname` jinak — kdyby ji
/// aplikace změnila, musí se změnit i tady (jinak zbude jen záložní
/// pravidlo v [`pick`]).
const MAIN_CLASS: &str = "Tauri Window";

/// Běžící proces s otevřeným handlem. Handle se drží po celou dobu
/// čekání: číslo procesu se po jeho konci může přidělit jinému
/// programu, handle ukazuje pořád na ten náš.
struct Running {
    pid: u32,
    handle: HANDLE,
}

impl Drop for Running {
    fn drop(&mut self) {
        // SAFETY: handle pochází z OpenProcess a zavírá se právě jednou.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

impl Running {
    /// Počká nejvýš `timeout`; `true` = proces skončil.
    fn wait(&self, timeout: Duration) -> bool {
        let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        // SAFETY: handle je platný (otevřený s PROCESS_SYNCHRONIZE).
        unsafe { WaitForSingleObject(self.handle, ms) == WAIT_OBJECT_0 }
    }
}

/// Jak zavírání dopadlo — pro hlášku v okně.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closed {
    /// Nic neběželo.
    NotRunning,
    /// Aplikace se zavřela sama po WM_CLOSE.
    Graceful,
    /// Musela se ukončit natvrdo.
    Forced,
}

/// Zavře všechny procesy spuštěné z `exe` (slušně, pak natvrdo).
///
/// `status` dostává průběžné hlášky, ať okno nevypadá zamrzle během
/// tří vteřin čekání.
pub fn close_app(exe: &Path, status: &mut dyn FnMut(&str)) -> Result<Closed, String> {
    let procs = find(exe);
    if procs.is_empty() {
        return Ok(Closed::NotRunning);
    }

    status("zavírám běžící KeyPad…");
    let alive = close_gracefully(procs);
    if alive.is_empty() {
        return Ok(Closed::Graceful);
    }

    status("KeyPad neodpovídá — ukončuji ho natvrdo…");
    kill(&alive);
    let deadline = Instant::now() + FORCED;
    let stuck = alive
        .iter()
        .filter(|p| !p.wait(deadline.saturating_duration_since(Instant::now())))
        .count();
    if stuck > 0 {
        // Typicky KeyPad spuštěný „jako správce": neprivilegovaný
        // instalátor mu nesmí poslat zprávu ani ho ukončit (UIPI).
        return Err(
            "Běžící KeyPad se nepodařilo zavřít (možná běží jako správce) — zavři ho ručně \
             a zkus to znovu."
                .into(),
        );
    }
    Ok(Closed::Forced)
}

/// Pošle WM_CLOSE hlavním oknům procesů a počká nejvýš [`GRACEFUL`].
/// Vrací procesy, které pořád běží.
///
/// Okna se hledají znovu každých [`POLL`]: KeyPad, který se právě
/// spouští, hlavní okno ještě nemusí mít — to, které vznikne během
/// čekání, dostane WM_CLOSE taky. Každé okno ale jen jednou; opakovaná
/// žádost o zavření by aplikaci nic nového neřekla.
fn close_gracefully(mut alive: Vec<Running>) -> Vec<Running> {
    let deadline = Instant::now() + GRACEFUL;
    let mut posted: Vec<isize> = Vec::new();
    loop {
        let pids: Vec<u32> = alive.iter().map(|p| p.pid).collect();
        for hwnd in pick(&top_windows(&pids)) {
            if posted.contains(&hwnd) {
                continue;
            }
            posted.push(hwnd);
            // SAFETY: PostMessageW jen zařadí zprávu do fronty cizího
            // vlákna; okno, které mezitím zaniklo, vrátí chybu a nic víc.
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if let Some(first) = alive.first() {
            first.wait(left.min(POLL));
        }
        alive.retain(|p| !p.wait(Duration::ZERO));
        if alive.is_empty() || Instant::now() >= deadline {
            return alive;
        }
    }
}

/// Okno nejvyšší úrovně některého z hledaných procesů.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TopWindow {
    pid: u32,
    hwnd: isize,
    /// Hlavní okno aplikace (třída [`MAIN_CLASS`]).
    main: bool,
    /// Viditelné a bez WS_EX_TOOLWINDOW — tak vypadá okno, které
    /// uživatel vidí na hlavním panelu (i minimalizované).
    plain: bool,
}

/// Kterým oknům poslat WM_CLOSE.
///
/// Proces, který má hlavní okno Tauri, dostane zprávu jen do něj —
/// ostatní okna jsou pomocná (single-instance, smyčka událostí) a jejich
/// zavření by aplikaci neukončilo, jen rozbilo. Bez hlavního okna
/// (jiná třída) je záloha „okna, která vidí uživatel": skrytá
/// a nástrojová se přeskakují ze stejného důvodu.
fn pick(wins: &[TopWindow]) -> Vec<isize> {
    wins.iter()
        .filter(|w| w.main || (w.plain && !wins.iter().any(|o| o.pid == w.pid && o.main)))
        .map(|w| w.hwnd)
        .collect()
}

/// Okna nejvyšší úrovně procesů `pids` (na ploše, kde běží instalátor —
/// jinde by je uživatel stejně neviděl).
fn top_windows(pids: &[u32]) -> Vec<TopWindow> {
    struct Scan<'a> {
        pids: &'a [u32],
        found: Vec<TopWindow>,
    }

    /// Nesmí panikařit — panika by přes hranici `extern "system"`
    /// shodila celý instalátor.
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        // SAFETY: `lp` je ukazatel na `Scan` z `top_windows`, který žije
        // po celou dobu EnumWindows; volání jsou synchronní v tomtéž vlákně.
        let scan = unsafe { &mut *(lp.0 as *mut Scan) };
        let mut pid = 0u32;
        // SAFETY: hwnd dodal EnumWindows; když okno mezitím zaniklo,
        // funkce jen vrátí nuly.
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        if scan.pids.contains(&pid) {
            let mut buf = [0u16; 64];
            // SAFETY: viz výše; délka bufferu jde s ním.
            let (len, ex, visible) = unsafe {
                (
                    GetClassNameW(hwnd, &mut buf),
                    GetWindowLongW(hwnd, GWL_EXSTYLE) as u32,
                    IsWindowVisible(hwnd).as_bool(),
                )
            };
            let class = &buf[..len.clamp(0, buf.len() as i32) as usize];
            scan.found.push(TopWindow {
                pid,
                hwnd: hwnd.0 as isize,
                main: class.iter().copied().eq(MAIN_CLASS.encode_utf16()),
                plain: visible && (ex & WS_EX_TOOLWINDOW.0) == 0,
            });
        }
        true.into()
    }

    let mut scan = Scan {
        pids,
        found: Vec::new(),
    };
    // SAFETY: callback dostane jen ukazatel na `scan`, který přežije
    // celé (synchronní) volání.
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut scan as *mut Scan as isize));
    }
    scan.found
}

/// `taskkill /F` na konkrétní procesy. Výstup se zahazuje (`.output()`),
/// jinak by v headless režimu vpadl doprostřed hlášení instalátoru.
fn kill(procs: &[Running]) {
    let mut cmd = std::process::Command::new(system32("taskkill.exe"));
    cmd.arg("/F");
    for p in procs {
        cmd.arg("/PID").arg(p.pid.to_string());
    }
    let _ = cmd.creation_flags(CREATE_NO_WINDOW).output();
}

/// Procesy, které běží přímo z `exe`.
fn find(exe: &Path) -> Vec<Running> {
    let Some(name) = exe.file_name().map(|n| n.to_string_lossy().to_lowercase()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // SAFETY: snímek se zavírá na konci; PROCESSENTRY32W má nastavenou
    // velikost, jak API vyžaduje; handly procesů hlídá `Running`.
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(260);
            let proc_name = String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase();
            // Jméno je levný předfiltr — otevírá se jen pár kandidátů.
            if proc_name == name {
                if let Some(r) = open_if_path(e.th32ProcessID, exe) {
                    out.push(r);
                }
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Otevře proces, pokud běží přesně z `exe`.
///
/// PROCESS_QUERY_LIMITED_INFORMATION jde otevřít i u procesu jiné
/// úrovně oprávnění; cizí uživatel ale neprojde — to nevadí, jeho
/// kopii v našem profilu mít spuštěnou nemůže.
fn open_if_path(pid: u32, exe: &Path) -> Option<Running> {
    // SAFETY: buffer má délku předanou v `size`; handle se při neshodě
    // zavře (Drop na `Running`).
    unsafe {
        let handle = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
        .ok()?;
        let r = Running { pid, handle };
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        QueryFullProcessImageNameW(
            r.handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
        .ok()?;
        let image = String::from_utf16_lossy(&buf[..size as usize]);
        same_file(Path::new(&image), exe).then_some(r)
    }
}

/// Ukazují dvě cesty na tentýž soubor?
///
/// Nejdřív levné porovnání bez ohledu na velikost písmen (Windows je
/// tak porovnávají taky), pak kanonická cesta — ta srovná i krátká
/// jména 8.3 a `..` v cestě. Kanonizace chce existující soubor; když
/// jeden z nich neexistuje, nejsou to tytéž soubory.
pub fn same_file(a: &Path, b: &Path) -> bool {
    let lower = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
    if lower(a) == lower(b) {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => lower(&x) == lower(&y),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stejna_cesta_bez_ohledu_na_velikost() {
        assert!(same_file(
            Path::new(r"C:\Users\A\AppData\Local\Programs\KeyPad\KeyPad.exe"),
            Path::new(r"c:\users\a\appdata\local\programs\keypad\keypad.EXE"),
        ));
        assert!(same_file(
            Path::new("C:/x/KeyPad.exe"),
            Path::new(r"C:\x\KeyPad.exe")
        ));
    }

    #[test]
    fn jina_cesta_neni_tentyz_soubor() {
        assert!(!same_file(
            Path::new(r"C:\dev\KeyPad\target\debug\KeyPad.exe"),
            Path::new(r"C:\Users\A\AppData\Local\Programs\KeyPad\KeyPad.exe"),
        ));
    }

    /// Soubor, který neexistuje, nesmí najít žádný běžící proces —
    /// jinak by instalátor zavíral cizí programy.
    #[test]
    fn neexistujici_binarka_nema_procesy() {
        let fake = std::env::temp_dir()
            .join("keypad-test-neexistuje")
            .join("KeyPad.exe");
        assert!(find(&fake).is_empty());
        let mut msgs = Vec::new();
        let r = close_app(&fake, &mut |s| msgs.push(s.to_string()));
        assert_eq!(r, Ok(Closed::NotRunning));
        assert!(msgs.is_empty());
    }

    /// Celá cesta slušně → natvrdo na skutečném procesu.
    ///
    /// Místo KeyPadu běží přejmenovaná kopie `ping.exe` bez okna: na
    /// WM_CLOSE nemá co odpovědět, takže slušné zavření musí vypršet
    /// a nastoupit `/F`. Originální ping běžící vedle se zavřít NESMÍ —
    /// hledá se podle cesty, ne podle jména. Kopie leží v `target\`,
    /// mimo profil i systém.
    #[test]
    fn neodpovidajici_proces_se_ukonci_natvrdo_a_cizi_zustane() {
        use std::process::{Command, Stdio};

        let deps = std::env::current_exe().unwrap();
        let dir = deps
            .parent()
            .unwrap()
            .join(format!("keypad-proc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("ping.exe");
        std::fs::copy(system32("ping.exe"), &exe).unwrap();

        let spawn = |p: &Path| {
            Command::new(p)
                .args(["-n", "30", "127.0.0.1"])
                .stdout(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .unwrap()
        };
        let mut ours = spawn(&exe);
        let mut foreign = spawn(&system32("ping.exe"));

        assert_eq!(find(&exe).len(), 1);
        let mut msgs = Vec::new();
        let r = close_app(&exe, &mut |s| msgs.push(s.to_string()));
        assert_eq!(r, Ok(Closed::Forced), "{msgs:?}");
        assert!(ours.try_wait().unwrap().is_some(), "náš proces měl skončit");
        assert!(
            foreign.try_wait().unwrap().is_none(),
            "cizí proces se stejným jménem měl běžet dál"
        );

        let _ = foreign.kill();
        let _ = foreign.wait();
        let _ = ours.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn win(pid: u32, hwnd: isize, main: bool, plain: bool) -> TopWindow {
        TopWindow {
            pid,
            hwnd,
            main,
            plain,
        }
    }

    /// Minimalizovaný KeyPad v pořadí oken: nahoře „viditelná" 0×0
    /// nástrojová okna (single-instance, smyčka událostí), dole hlavní
    /// okno. Zpráva smí jít jen hlavnímu — ne prvnímu viditelnému jako
    /// u `taskkill`.
    #[test]
    fn wm_close_jde_jen_hlavnimu_oknu() {
        let wins = [
            win(7, 1, false, false),
            win(7, 2, false, false),
            win(7, 3, false, true),
            win(7, 4, true, true),
        ];
        assert_eq!(pick(&wins), vec![4]);
        // Hlavní okno schované (třeba do lišty) dostane zprávu taky.
        assert_eq!(
            pick(&[win(7, 1, false, false), win(7, 4, true, false)]),
            vec![4]
        );
    }

    #[test]
    fn bez_hlavniho_okna_jen_bezna_viditelna() {
        let wins = [
            win(7, 1, false, false),
            win(7, 2, false, true),
            win(8, 3, true, true),
            win(8, 4, false, true),
        ];
        // Proces 7 hlavní okno nemá → záloha; proces 8 ho má → jen to.
        assert_eq!(pick(&wins), vec![2, 3]);
        assert!(pick(&[win(7, 1, false, false)]).is_empty());
    }

    /// Proměnná prostředí, podle které se testovací binárka spuštěná
    /// jako podřízený proces pozná a zahraje KeyPad.
    const CHILD_ENV: &str = "KEYPAD_TEST_OKNA";

    /// Podřízený proces pro test níž: okna ve tvaru KeyPadu — nahoře
    /// pomocné nástrojové okno (jako single-instance), pod ním hlavní
    /// okno třídy [`MAIN_CLASS`]. Proces skončí, jen když se zavře hlavní
    /// okno. Obě okna jsou SKRYTÁ — test nesmí nic ukázat na ploše.
    /// Bez proměnné prostředí hned skončí (kdyby ho někdo spustil
    /// přes `--ignored`).
    #[test]
    #[ignore = "spouští ho jako podřízený proces test slusne_zavreni_hlavniho_okna"]
    fn podrizeny_proces_s_okny_jako_keypad() {
        use windows::core::{w, PCWSTR};
        use windows::Win32::Foundation::LRESULT;
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage,
            RegisterClassW, MSG, WINDOW_EX_STYLE, WM_DESTROY, WNDCLASSW, WNDPROC,
            WS_OVERLAPPEDWINDOW, WS_POPUP,
        };

        unsafe extern "system" fn helper(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
            // SAFETY: předání zprávy výchozí obsluze.
            unsafe { DefWindowProcW(h, m, wp, lp) }
        }
        unsafe extern "system" fn main_wnd(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
            if m == WM_DESTROY {
                // SAFETY: běží ve vlákně, které okno vlastní.
                unsafe { PostQuitMessage(0) };
            }
            // SAFETY: předání zprávy výchozí obsluze.
            unsafe { DefWindowProcW(h, m, wp, lp) }
        }

        if std::env::var_os(CHILD_ENV).is_none() {
            return;
        }
        // SAFETY: standardní registrace tříd, vytvoření skrytých oken
        // a smyčka zpráv v jednom vlákně.
        unsafe {
            let hinst = GetModuleHandleW(None).unwrap();
            let make = |class: PCWSTR, proc: WNDPROC, ex: WINDOW_EX_STYLE, style| {
                RegisterClassW(&WNDCLASSW {
                    lpfnWndProc: proc,
                    hInstance: hinst.into(),
                    lpszClassName: class,
                    ..Default::default()
                });
                CreateWindowExW(
                    ex,
                    class,
                    w!("KeyPad test"),
                    style,
                    0,
                    0,
                    0,
                    0,
                    None,
                    None,
                    Some(hinst.into()),
                    None,
                )
                .unwrap();
            };
            make(
                w!("keypad-test-sic"),
                Some(helper),
                WS_EX_TOOLWINDOW,
                WS_POPUP,
            );
            make(
                w!("Tauri Window"),
                Some(main_wnd),
                WINDOW_EX_STYLE::default(),
                WS_OVERLAPPEDWINDOW,
            );
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                DispatchMessageW(&msg);
            }
        }
    }

    /// Slušná cesta na skutečném procesu: WM_CLOSE musí dojít hlavnímu
    /// oknu a proces skončit sám, bez `/F`. Místo KeyPadu běží kopie
    /// testovací binárky (jen skrytá okna, viz výš) v `target\`.
    #[test]
    fn slusne_zavreni_hlavniho_okna() {
        use std::process::{Command, Stdio};

        assert_eq!(
            MAIN_CLASS, "Tauri Window",
            "podřízený proces registruje třídu natvrdo"
        );
        let dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("keypad-close-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("KeyPad.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();

        let mut child = Command::new(&exe)
            .args([
                "--exact",
                "proc::tests::podrizeny_proces_s_okny_jako_keypad",
                "--ignored",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();

        // Počkat, až okna vzniknou — jinak by test měřil rychlost startu.
        let deadline = Instant::now() + Duration::from_secs(30);
        while !top_windows(&[child.id()]).iter().any(|w| w.main) {
            if Instant::now() > deadline || child.try_wait().unwrap().is_some() {
                let _ = child.kill();
                panic!("podřízený proces nevytvořil hlavní okno");
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let mut msgs = Vec::new();
        let r = close_app(&exe, &mut |s| msgs.push(s.to_string()));
        if r != Ok(Closed::Graceful) {
            let _ = child.kill();
        }
        let status = child.wait().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(r, Ok(Closed::Graceful), "{msgs:?}");
        assert!(status.success(), "proces měl skončit sám: {status:?}");
    }
}

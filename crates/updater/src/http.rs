//! Stahování přes WinHttp — součást Windows.
//!
//! Vědomě bez `reqwest`/`ureq`: TLS knihovna by binárky nafoukla
//! o megabajty a nesla vlastní seznam certifikátů. WinHttp používá
//! systémové úložiště, takže věří přesně tomu, čemu věří Windows.

use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_CONTENT_LENGTH, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

/// Věty pro okno instalátoru (Fáze 2b: žádné kódy ani vnitřnosti WinHttp
/// v okně — ty jdou do logu). Sdílené, ať se texty nerozejdou.
pub const NO_CONNECTION: &str =
    "Nepodařilo se spojit s GitHubem — zkontroluj připojení a zkus to znovu.";
pub const RATE_LIMITED: &str = "GitHub teď omezil počet dotazů — zkus to za hodinu.";
pub const NOT_PUBLISHED: &str = "Vydání KeyPadu na GitHubu teď není — zkus to později.";

/// Chyby stahování. `Display` jde do logu a do hlášek aplikace — známé
/// stavové kódy mají větu místo čísla (číslo nese `{:?}`). Okno
/// instalátoru ukazuje jen [`Error::sentence`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Connect(String),
    Http { status: u32 },
    Read(String),
}

impl Error {
    /// Jedna krátká věta pro okno instalátoru, bez kódů. Spojení,
    /// přerušený přenos i chyba serveru znamenají pro uživatele totéž —
    /// zkusit to znovu. Vlastní větu mají jen stavy, kde hned znovu
    /// nepomůže: vyčerpaný limit dotazů API (403) a nic vydaného (404/409).
    pub fn sentence(&self) -> &'static str {
        match self {
            Error::Http { status: 403 } => RATE_LIMITED,
            Error::Http { status: 404 | 409 } => NOT_PUBLISHED,
            _ => NO_CONNECTION,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Connect(d) => write!(f, "nepodařilo se spojit se serverem ({d})"),
            // GitHub API pouští nepřihlášené na 60 dotazů za hodinu z IP;
            // po vyčerpání vrací 403. Obecné „chyba 403" by radilo špatně.
            Error::Http { status: 403 } => {
                write!(f, "GitHub dočasně omezil počet dotazů — zkus to za hodinu")
            }
            // API tak odpovídá na repozitář bez jediného commitu.
            Error::Http { status: 409 } => {
                write!(f, "repozitář je zatím prázdný — nic není vydané")
            }
            Error::Http { status: 404 } => {
                write!(f, "soubor na serveru není — vydavatel ho ještě nenahrál")
            }
            // Neznámý kód: číslo je jediná informace, kterou máme.
            Error::Http { status } => write!(f, "server odpověděl chybou {status}"),
            Error::Read(d) => write!(f, "přenos se přerušil ({d})"),
        }
    }
}

impl std::error::Error for Error {}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Handle WinHttp, který se sám zavře. Bez tohohle by každá chybová
/// cesta musela handle uklízet ručně — a jednou by se zapomnělo.
struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: handle pochází z WinHttp* a zavírá se právě jednou.
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// Stáhne `https://{host}{path}` do paměti.
///
/// `progress` dostane počet dosud přenesených bajtů — instalátor podle
/// toho kreslí ukazatel, ať uživatel nekouká na zamrzlé okno.
pub fn get(host: &str, path: &str, progress: impl FnMut(usize)) -> Result<Vec<u8>, Error> {
    get_limited(host, path, usize::MAX, progress)
}

/// Jako [`get`], ale nejvýš `max` bajtů — víc se ani nezačne stahovat.
///
/// Pro soubory, jejichž přesnou velikost známe předem (instalátor
/// ViGEmBus): cokoli většího není ten soubor a nemá smysl kvůli tomu
/// plnit paměť. Přesměrování (github.com → CDN) WinHttp sleduje samo,
/// jen ne z HTTPS na HTTP (výchozí politika).
pub fn get_limited(
    host: &str,
    path: &str,
    max: usize,
    mut progress: impl FnMut(usize),
) -> Result<Vec<u8>, Error> {
    let wagent = wide("KeyPadSetup");
    let whost = wide(host);
    let wpath = wide(path);
    let wget = wide("GET");

    // SAFETY: všechny handly drží Handle (uzavře je Drop); buffery
    // mají velikost hlášenou API.
    unsafe {
        let session = Handle(WinHttpOpen(
            PCWSTR(wagent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(Error::Connect("WinHttpOpen".into()));
        }
        // Výchozí limity WinHttp jsou 60 s na spojení a 30 s na každou
        // odpověď; na zaseklé síti by kontrola verze visela minuty.
        // Resolve / connect / send / receive v milisekundách.
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);

        let conn = Handle(WinHttpConnect(session.0, PCWSTR(whost.as_ptr()), 443, 0));
        if conn.0.is_null() {
            return Err(Error::Connect(format!("spojení na {host}")));
        }

        let req = Handle(WinHttpOpenRequest(
            conn.0,
            PCWSTR(wget.as_ptr()),
            PCWSTR(wpath.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null_mut(),
            WINHTTP_FLAG_SECURE,
        ));
        if req.0.is_null() {
            return Err(Error::Connect("WinHttpOpenRequest".into()));
        }

        if WinHttpSendRequest(req.0, None, None, 0, 0, 0).is_err() {
            return Err(Error::Connect("odeslání požadavku".into()));
        }
        if WinHttpReceiveResponse(req.0, std::ptr::null_mut()).is_err() {
            return Err(Error::Connect("čekání na odpověď".into()));
        }

        // Stavový kód: bez téhle kontroly bychom uložili HTML stránku
        // s chybou, jako by to byla binárka.
        let mut status: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        let _ = WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut _ as *mut _),
            &mut len,
            std::ptr::null_mut(),
        );
        if status != 200 {
            return Err(Error::Http { status });
        }

        // Ohlášená délka těla. Když spojení skončí dřív, než dorazí
        // všechno, WinHttp to nemusí ohlásit jako chybu — a useknutá
        // binárka s hlavičkou „MZ" by prošla i kontrolou `looks_like_exe`.
        // Bez hlavičky (chunked přenos) se délka nekontroluje.
        let mut expected: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        let has_length = WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut expected as *mut _ as *mut _),
            &mut len,
            std::ptr::null_mut(),
        )
        .is_ok();
        let too_big = |n: usize| Error::Read(format!("server posílá {n} B, čekáno nejvýš {max} B"));
        if has_length && expected as usize > max {
            return Err(too_big(expected as usize));
        }

        let mut out = Vec::new();
        loop {
            let mut avail: u32 = 0;
            if WinHttpQueryDataAvailable(req.0, &mut avail).is_err() {
                return Err(Error::Read("dotaz na data".into()));
            }
            if avail == 0 {
                break;
            }
            if out.len().saturating_add(avail as usize) > max {
                return Err(too_big(out.len().saturating_add(avail as usize)));
            }
            let start = out.len();
            out.resize(start + avail as usize, 0);
            let mut read: u32 = 0;
            if WinHttpReadData(req.0, out[start..].as_mut_ptr() as *mut _, avail, &mut read)
                .is_err()
            {
                return Err(Error::Read("čtení dat".into()));
            }
            out.truncate(start + read as usize);
            progress(out.len());
        }
        if has_length && out.len() != expected as usize {
            return Err(Error::Read(format!(
                "dorazilo {} z {} B",
                out.len(),
                expected
            )));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Do okna instalátoru jde věta bez kódů a bez vnitřností WinHttp;
    /// podrobnosti zůstávají v `Display` (log, hlášky aplikace).
    #[test]
    fn veta_do_okna_je_bez_kodu() {
        let errs = [
            Error::Connect("WinHttpOpen".into()),
            Error::Connect("spojení na api.github.com".into()),
            Error::Connect("odeslání požadavku".into()),
            Error::Http { status: 0 },
            Error::Http { status: 403 },
            Error::Http { status: 404 },
            Error::Http { status: 409 },
            Error::Http { status: 500 },
            Error::Http { status: 502 },
            Error::Read("dorazilo 123 z 456 B".into()),
            Error::Read("server posílá 9 B, čekáno nejvýš 1 B".into()),
        ];
        for e in &errs {
            let s = e.sentence();
            assert!(!s.chars().any(|c| c.is_ascii_digit()), "{e:?}: {s}");
            for bad in ["WinHttp", "http", "(", " B"] {
                assert!(!s.contains(bad), "{e:?}: {s}");
            }
            assert!(s.ends_with('.') && s.chars().count() < 90, "{s}");
        }
        assert_eq!(errs[0].sentence(), NO_CONNECTION);
        assert_eq!(errs[4].sentence(), RATE_LIMITED);
        assert_eq!(errs[5].sentence(), NOT_PUBLISHED);
        assert_eq!(errs[8].sentence(), NO_CONNECTION);
        assert_eq!(errs[9].sentence(), NO_CONNECTION);
        // Podrobnosti pro log zůstaly.
        assert!(errs[0].to_string().contains("WinHttpOpen"));
        assert!(errs[8].to_string().contains("502"));
    }
}

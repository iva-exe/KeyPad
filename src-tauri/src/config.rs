//! Uložení kláves: `%APPDATA%\KeyPad\config.json` (`updater::config_path`).
//!
//! Soubor nese mapování všech ovladačů, zkratku a zapnutý zvuk. Stav
//! zapnutí ovladače v něm **není a nikdy nebude**: ovladač se připojuje
//! jen na povel uživatele (princip 11), takže po startu nemá co obnovovat.
//!
//! Proč JSON, a ne TOML, a proč takhle (princip 10, hranice +50 kB ze
//! specifikace Fáze 6; naměřeno release buildem přes Tauri CLI a mapou
//! linkeru, 1. 10. 2026):
//! - crate `toml` přidal `KeyPad.exe` 323 KiB;
//! - JSON přes odvozené (`derive`) struktury a frontu `crossbeam`
//!   97 KiB — obojí se v binárce rozvine znovu pro každý nový typ;
//! - JSON přes `serde_json::Value` (parser i `Value` Tauri v binárce už
//!   má) a vlákno s `Mutex` + `Condvar`: 31 KiB, z toho kód modulu
//!   24 kB. Mezi buildy s téměř stejným kódem to kolísá až o 20 kB
//!   (thin LTO jinak rozdělí kód Tauri), proto se nepřidávají další
//!   generické typy ani fronty.
//!
//! Pravidla načtení ([`nacti`]):
//!
//! | Soubor | Co se stane | Ukládat? |
//! |---|---|---|
//! | chybí | výchozí v paměti, soubor vznikne až první změnou | ano |
//! | platný | načte se (BOM se odstřihne, neznámé klíče se ignorují) | ano |
//! | nevalidní | přejmenuje se na `config.invalid.json`, platí výchozí | ano |
//! | nevalidní a zálohu nejde vytvořit | výchozí v paměti, soubor nedotčený, stav jako nečitelný | ne |
//! | `verze` > 1 | výchozí jen v paměti, soubor nedotčený | ne |
//! | nejde číst | výchozí v paměti | ne |
//!
//! Proč se soubor z novější verze, nečitelný ani neodložený nevalidní
//! soubor nepřepisuje: všechny můžou nést klávesy, které uživatel chce
//! zpátky (návrat ke starší verzi KeyPadu, soubor zrovna drží jiný
//! program, překlep v ruční úpravě). Výchozí klávesy v paměti stačí na
//! hraní a o nic se nepřijde — okno jen ukáže, že se klávesy neukládají.
//!
//! Zápis ([`uloz`]) je atomický — `.tmp`, `sync_all`, `rename` — takže
//! výpadek proudu uprostřed nenechá napůl zapsaný soubor, který by příští
//! start zahodil do zálohy. Zapisuje jediné vlákno `keypad-konfig`
//! ([`Ukladac`]): pomalý disk, antivirus nebo síťový profil tak nezdrží
//! vlákno okna, na kterém visí watchdog ovladačů (princip 1). Soubor se
//! nesleduje (princip 10); ruční úpravu za běhu přepíše další změna
//! z aplikace.
//!
//! Do logu jdou jen počty vazeb a chyby obsahu souboru — nikdy stisky.

use std::fmt::Write as _;
use std::io::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use keypad_core::{Action, KeyId, Mapping, MappingError, PadAction, PadId, MAX_PADS};
use serde::Serialize;
use serde_json::{Map, Value};

/// Verze formátu, kterou tahle aplikace píše a umí číst.
///
/// `verze` zůstane ve všech budoucích formátech celé číslo na nejvyšší
/// úrovni objektu — jen podle ní starší KeyPad pozná soubor, kterému
/// nerozumí, a nechá ho být.
pub const VERZE: i64 = 1;

/// Záloha nevalidního souboru, vedle `config.json`. Název je v updateru:
/// odinstalace ji podle něj nechává uživateli stejně jako konfiguraci.
pub const ZALOHA: &str = updater::CONFIG_BACKUP_FILE;

/// Zápis až tak dlouho po poslední změně: přiřazování několika kláves
/// za sebou je jeden zápis, ne deset.
pub const ODKLAD: Duration = Duration::from_millis(500);

/// Konce řádků CRLF: Poznámkový blok před Windows 10 1809 jiné neumí
/// a soubor by v něm byl jeden dlouhý řádek. Parser JSON bere oba.
const NL: &str = "\r\n";

/// JSON nemá komentáře; vysvětlení pro člověka je hodnota klíče, který
/// čtení ignoruje. Bez uvozovek a zpětných lomítek — píše se ručně.
const POZNAMKA: &str = "KeyPad — klávesy virtuálních ovladačů. Soubor zapisuje aplikace; \
                        nevalidní se přejmenuje na config.invalid.json a platí výchozí klávesy.";

/// Nejvýš tolik chyb obsahu jde do logu; zbytek jen počtem. Rozbitý
/// soubor se stovkami vazeb by jinak zaplavil frontu logu.
const MAX_CHYB_V_LOGU: usize = 10;

/// Co se ukládá.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Konfigurace {
    pub mapovani: Mapping,
    /// Zvuk pozastavení a pokračování (✓ Zvuk v nabídce ikony).
    pub zvuk: bool,
}

impl Default for Konfigurace {
    fn default() -> Self {
        Konfigurace {
            mapovani: Mapping::default(),
            zvuk: true,
        }
    }
}

/// Jak dopadlo načtení a poslední zápis — okno podle toho ukáže pruh nad
/// kartami („Klávesy nešly načíst — výchozí" / „Klávesy se neukládají").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StavKonfigurace {
    /// Načteno (nebo soubor zatím není) a poslední zápis prošel.
    Ok,
    /// Soubor byl nevalidní: je v záloze a platí výchozí klávesy.
    Obnovena,
    /// Soubor je z novější verze KeyPadu — platí výchozí a neukládá se.
    Novejsi,
    /// Soubor existuje, ale nejde přečíst, nebo je nevalidní a nejde
    /// odložit do zálohy — platí výchozí a neukládá se. Ne `Obnovena`:
    /// okno by tvrdilo, že je soubor odložený, a nevarovalo by, že se
    /// změny kláves ztratí (princip 8).
    Necitelna,
    /// Zápis selhal; další pokus při příští změně.
    Neulozena,
}

/// Výsledek [`nacti`].
#[derive(Debug)]
pub struct Nacteno {
    pub konfigurace: Konfigurace,
    pub stav: StavKonfigurace,
    /// Kam se odložil nevalidní soubor: u [`StavKonfigurace::Obnovena`]
    /// vždy, jinak `None`.
    pub zaloha: Option<PathBuf>,
    /// Smí se soubor přepsat? Ne u souboru z novější verze, nečitelného
    /// a nevalidního, který nešel zálohovat ([`StavKonfigurace::Necitelna`]).
    pub ukladat: bool,
    /// Chyby obsahu nevalidního souboru (věty pro bublinu okna a log).
    pub chyby: Vec<String>,
}

impl Nacteno {
    fn vychozi(stav: StavKonfigurace, ukladat: bool) -> Nacteno {
        Nacteno {
            konfigurace: Konfigurace::default(),
            stav,
            zaloha: None,
            ukladat,
            chyby: Vec::new(),
        }
    }
}

/// Proč text není použitelná konfigurace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChybaKonfigurace {
    /// `verze` je vyšší, než tahle aplikace umí — soubor zapsal novější
    /// KeyPad a přepsat se nesmí.
    Novejsi { verze: i64 },
    /// Soubor je nevalidní; všechny nalezené chyby najednou.
    Neplatna(Vec<ChybaObsahu>),
}

/// Jedna chyba obsahu nevalidního souboru.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChybaObsahu {
    /// Soubor není v UTF-8.
    Kodovani,
    /// Není to JSON, nebo to není objekt (`radek` od 1).
    Syntaxe {
        radek: Option<usize>,
        zprava: String,
    },
    /// Chybí `verze`.
    ChybiVerze,
    /// `verze` menší než 1.
    Verze(i64),
    /// Klíč chybí nebo má jiný typ či rozsah. `vazba` = pořadí ve
    /// `vazby` od 1 (`None` mimo vazby); prázdný `klic` = vazba celá.
    Hodnota {
        vazba: Option<usize>,
        klic: &'static str,
        ocekavano: &'static str,
    },
    /// `ovladac` mimo 1–4.
    Ovladac { vazba: usize, ovladac: u64 },
    /// Neznámý kód vstupu ([`Action::code`]).
    Vstup { vazba: usize, vstup: String },
    /// Porušené pravidlo mapování z jádra (duplicita, Win, Esc, prázdné…).
    Mapovani(MappingError),
}

impl std::fmt::Display for ChybaObsahu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChybaObsahu::Kodovani => write!(f, "soubor není v kódování UTF-8"),
            ChybaObsahu::Syntaxe {
                radek: Some(r),
                zprava,
            } => write!(f, "řádek {r}: {zprava}"),
            ChybaObsahu::Syntaxe {
                radek: None,
                zprava,
            } => write!(f, "{zprava}"),
            ChybaObsahu::ChybiVerze => write!(f, "chybí „verze“"),
            ChybaObsahu::Verze(v) => write!(f, "neplatná verze {v}"),
            ChybaObsahu::Hodnota {
                vazba,
                klic,
                ocekavano,
            } => {
                if let Some(n) = vazba {
                    write!(f, "vazba č. {n}")?;
                    if klic.is_empty() {
                        return write!(f, " musí být {ocekavano}");
                    }
                    f.write_str(": ")?;
                }
                write!(f, "„{klic}“ musí být {ocekavano}")
            }
            ChybaObsahu::Ovladac { vazba, ovladac } => write!(
                f,
                "vazba č. {vazba}: ovladač {ovladac} neexistuje (jen 1–{MAX_PADS})"
            ),
            ChybaObsahu::Vstup { vazba, vstup } => {
                // Text ze souboru může být libovolně dlouhý; do logu stačí
                // začátek, ať se překlep pozná (řez na hranici znaku).
                let konec = vstup.char_indices().nth(32).map_or(vstup.len(), |(i, _)| i);
                let zkraceny = &vstup[..konec];
                write!(f, "vazba č. {vazba}: neznámý vstup {zkraceny:?}")
            }
            ChybaObsahu::Mapovani(e) => write!(f, "{e}"),
        }
    }
}

impl std::fmt::Display for ChybaKonfigurace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChybaKonfigurace::Novejsi { verze } => {
                write!(f, "soubor je z novější verze KeyPadu (verze {verze})")
            }
            ChybaKonfigurace::Neplatna(chyby) => {
                for (i, c) in chyby.iter().enumerate() {
                    if i > 0 {
                        f.write_str("; ")?;
                    }
                    write!(f, "{c}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ChybaKonfigurace {}

// ── Čtení ──────────────────────────────────────────────────────────
// Přes `serde_json::Value`, ne přes odvozené struktury: parser hodnot
// v binárce už je (IPC Tauri) a ruční výběr klíčů dá české hlášky
// s pořadím vazby. Neznámé klíče se ignorují (doplní je novější verze
// stejného formátu, `poznamka`, nebo poznámky uživatele).

/// Chyba parseru jako jeden řádek s číslem řádku souboru (pozice se
/// z anglické zprávy odstřihne, nese ji `radek`).
fn chyba_json(e: &serde_json::Error) -> ChybaObsahu {
    let cela = e.to_string();
    let pozice = format!(" at line {} column {}", e.line(), e.column());
    let zprava = cela.strip_suffix(&pozice).unwrap_or(&cela).to_string();
    ChybaObsahu::Syntaxe {
        radek: (e.line() > 0).then_some(e.line()),
        zprava,
    }
}

/// Hodnota klíče; `null` platí jako chybějící klíč.
fn klic<'a>(o: &'a Map<String, Value>, jmeno: &str) -> Option<&'a Value> {
    o.get(jmeno).filter(|v| !v.is_null())
}

/// Čtení jednoho objektu: chyby se sbírají, místo chybné hodnoty se
/// vrátí `None` a pokračuje se — uživatel má vidět všechny chyby naráz.
struct Cteni<'a> {
    chyby: &'a mut Vec<ChybaObsahu>,
    vazba: Option<usize>,
}

impl Cteni<'_> {
    fn chyba(&mut self, klic: &'static str, ocekavano: &'static str) {
        self.chyby.push(ChybaObsahu::Hodnota {
            vazba: self.vazba,
            klic,
            ocekavano,
        });
    }

    fn scan(&mut self, o: &Map<String, Value>, jmeno: &'static str) -> Option<u16> {
        let v = klic(o, jmeno)
            .and_then(Value::as_u64)
            .and_then(|s| u16::try_from(s).ok());
        if v.is_none() {
            self.chyba(jmeno, "celé číslo 0–65535");
        }
        v
    }

    /// Volitelný příznak (chybí = `vychozi`).
    fn priznak(&mut self, o: &Map<String, Value>, jmeno: &'static str, vychozi: bool) -> bool {
        match klic(o, jmeno) {
            None => vychozi,
            Some(v) => v.as_bool().unwrap_or_else(|| {
                self.chyba(jmeno, "true nebo false");
                vychozi
            }),
        }
    }
}

/// Text souboru → konfigurace. Čistá funkce; obsah hlídají pravidla jádra
/// ([`Mapping::new`]), takže konfigurace projde, právě když by ji přijal
/// i engine.
pub fn z_textu(text: &str) -> Result<Konfigurace, ChybaKonfigurace> {
    // Poznámkový blok starších Windows ukládá UTF-8 s BOM; JSON ho nezná.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let jedna = |c| ChybaKonfigurace::Neplatna(vec![c]);

    let koren: Value =
        serde_json::from_slice(text.as_bytes()).map_err(|e| jedna(chyba_json(&e)))?;
    let Some(o) = koren.as_object() else {
        return Err(jedna(ChybaObsahu::Syntaxe {
            radek: None,
            zprava: "soubor musí být objekt JSON { … }".into(),
        }));
    };

    // Verze dřív než cokoli dalšího: soubor z novější verze může mít
    // ostatní klíče jinak a nesmí skončit jako „nevalidní" v záloze.
    let verze = match klic(o, "verze") {
        None => return Err(jedna(ChybaObsahu::ChybiVerze)),
        Some(v) => v.as_i64().ok_or_else(|| {
            jedna(ChybaObsahu::Hodnota {
                vazba: None,
                klic: "verze",
                ocekavano: "celé číslo",
            })
        })?,
    };
    if verze < 1 {
        return Err(jedna(ChybaObsahu::Verze(verze)));
    }
    if verze > VERZE {
        return Err(ChybaKonfigurace::Novejsi { verze });
    }

    let mut chyby = Vec::new();
    let mut c = Cteni {
        chyby: &mut chyby,
        vazba: None,
    };
    let zvuk = c.priznak(o, "zvuk", true);
    let zkratka = match klic(o, "zkratka") {
        // Chybí = výchozí Scroll Lock.
        None => Some(KeyId::SCROLL_LOCK),
        Some(Value::Object(z)) => {
            let scan = c.scan(z, "scan");
            let e0 = c.priznak(z, "e0", false);
            scan.map(|scan| KeyId { scan, extended: e0 })
        }
        Some(_) => {
            c.chyba("zkratka", "objekt { \"scan\": …, \"e0\": … }");
            None
        }
    };
    let prazdne = Vec::new();
    let polozky = match klic(o, "vazby") {
        None => &prazdne,
        Some(Value::Array(v)) => v,
        Some(_) => {
            c.chyba("vazby", "seznam [ … ]");
            &prazdne
        }
    };

    let mut vazby = Vec::with_capacity(polozky.len());
    for (i, polozka) in polozky.iter().enumerate() {
        let poradi = i + 1;
        let mut c = Cteni {
            chyby: &mut chyby,
            vazba: Some(poradi),
        };
        let Some(v) = polozka.as_object() else {
            c.chyba("", "objekt { \"ovladac\": …, \"vstup\": …, \"scan\": … }");
            continue;
        };
        let pad = match klic(v, "ovladac").and_then(Value::as_u64) {
            None => {
                c.chyba("ovladac", "číslo 1–4");
                None
            }
            Some(n) => {
                let pad = n
                    .checked_sub(1)
                    .and_then(|i| usize::try_from(i).ok())
                    .and_then(PadId::new);
                if pad.is_none() {
                    c.chyby.push(ChybaObsahu::Ovladac {
                        vazba: poradi,
                        ovladac: n,
                    });
                }
                pad
            }
        };
        let akce = match klic(v, "vstup").and_then(Value::as_str) {
            None => {
                c.chyba("vstup", "kód vstupu (\"ls_up\", \"a\", \"lt\"…)");
                None
            }
            Some(s) => {
                let akce = Action::from_code(s);
                if akce.is_none() {
                    c.chyby.push(ChybaObsahu::Vstup {
                        vazba: poradi,
                        vstup: s.to_string(),
                    });
                }
                akce
            }
        };
        let scan = c.scan(v, "scan");
        let e0 = c.priznak(v, "e0", false);
        if let (Some(pad), Some(akce), Some(scan)) = (pad, akce, scan) {
            let klavesa = KeyId { scan, extended: e0 };
            vazby.push((klavesa, PadAction::new(pad, akce)));
        }
    }

    // Se zkratkou, která nejde přečíst, se pravidla jádra (duplicity,
    // Win…) ověří s výchozí — chyba zkratky už je v seznamu.
    match Mapping::new(zkratka.unwrap_or(KeyId::SCROLL_LOCK), vazby) {
        Ok(mapovani) if chyby.is_empty() => Ok(Konfigurace { mapovani, zvuk }),
        Ok(_) => Err(ChybaKonfigurace::Neplatna(chyby)),
        Err(chyby_jadra) => {
            // „Prázdné" jen tehdy, když nevypadla žádná vazba — jinak by
            // k „neznámý vstup" přibylo matoucí „mapování je prázdné"
            // jen proto, že se ta vazba nepřidala (stejně jako v jádře).
            let vypadla = !chyby.is_empty();
            chyby.extend(
                chyby_jadra
                    .into_iter()
                    .filter(|e| !(vypadla && *e == MappingError::Empty))
                    .map(ChybaObsahu::Mapovani),
            );
            Err(ChybaKonfigurace::Neplatna(chyby))
        }
    }
}

/// Konfigurace → text souboru. Čistá funkce; vazby v pořadí
/// [`Mapping::bindings`], takže stejný obsah dá vždy stejný text.
///
/// Píše se ručně, ne serializérem: tvar je pevný (celá čísla,
/// `true`/`false`, kódy vstupů z `[a-z0-9_]` a poznámka bez znaků
/// k escapování) a jedna vazba na řádek se čte líp než odsazený strom.
pub fn do_textu(k: &Konfigurace) -> String {
    let m = &k.mapovani;
    let mut s = String::with_capacity(512 + m.len() * 64);
    let z = m.toggle_key();
    // Zápis do `String` selhat nemůže.
    let _ = write!(
        s,
        "{{{NL}  \"poznamka\": \"{POZNAMKA}\",{NL}  \"verze\": {VERZE},{NL}  \"zvuk\": {},{NL}  \
         \"zkratka\": {{ \"scan\": {}, \"e0\": {} }},{NL}  \"vazby\": [",
        k.zvuk, z.scan, z.extended
    );
    for (i, (klavesa, cil)) in m.bindings().enumerate() {
        let _ = write!(
            s,
            "{}{NL}    {{ \"ovladac\": {}, \"vstup\": \"{}\", \"scan\": {}, \"e0\": {} }}",
            if i > 0 { "," } else { "" },
            cil.pad.index() + 1,
            cil.action.code(),
            klavesa.scan,
            klavesa.extended
        );
    }
    let _ = write!(s, "{NL}  ]{NL}}}{NL}");
    s
}

/// Počty vazeb po ovladačích — do logu místo kláves.
fn pocty(m: &Mapping) -> [usize; MAX_PADS] {
    let mut n = [0; MAX_PADS];
    for (_, cil) in m.bindings() {
        n[cil.pad.index()] += 1;
    }
    n
}

/// Načte konfiguraci podle pravidel v dokumentaci modulu. Nikdy
/// nespadne: cokoli jiného než platný soubor dá výchozí klávesy.
pub fn nacti(cesta: &Path) -> Nacteno {
    let bajty = match std::fs::read(cesta) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::info!(
                "konfigurace: {} zatím není — výchozí klávesy, soubor vznikne první změnou",
                cesta.display()
            );
            return Nacteno::vychozi(StavKonfigurace::Ok, true);
        }
        Err(e) => {
            log::warn!(
                "konfigurace: {} nejde přečíst ({e}) — výchozí klávesy, soubor se nepřepisuje",
                cesta.display()
            );
            return Nacteno::vychozi(StavKonfigurace::Necitelna, false);
        }
    };
    let vysledek = match std::str::from_utf8(&bajty) {
        Ok(text) => z_textu(text),
        Err(_) => Err(ChybaKonfigurace::Neplatna(vec![ChybaObsahu::Kodovani])),
    };
    match vysledek {
        Ok(konfigurace) => {
            let n = pocty(&konfigurace.mapovani);
            log::info!(
                "konfigurace: načteno {} vazeb (ovladače 1–4: {}/{}/{}/{}), zvuk {}",
                konfigurace.mapovani.len(),
                n[0],
                n[1],
                n[2],
                n[3],
                if konfigurace.zvuk {
                    "zapnutý"
                } else {
                    "vypnutý"
                }
            );
            Nacteno {
                konfigurace,
                stav: StavKonfigurace::Ok,
                zaloha: None,
                ukladat: true,
                chyby: Vec::new(),
            }
        }
        Err(ChybaKonfigurace::Novejsi { verze }) => {
            log::warn!(
                "konfigurace: {} je z novější verze KeyPadu (verze {verze}) — výchozí klávesy, \
                 soubor se nepřepisuje",
                cesta.display()
            );
            Nacteno::vychozi(StavKonfigurace::Novejsi, false)
        }
        Err(ChybaKonfigurace::Neplatna(chyby)) => {
            let chyby: Vec<String> = chyby.iter().map(ChybaObsahu::to_string).collect();
            log::warn!(
                "konfigurace: {} je nevalidní ({} chyb, do logu nejvýš {MAX_CHYB_V_LOGU}) — \
                 výchozí klávesy",
                cesta.display(),
                chyby.len()
            );
            for c in chyby.iter().take(MAX_CHYB_V_LOGU) {
                log::warn!("konfigurace: {c}");
            }
            match zalohuj(cesta) {
                Some(zaloha) => Nacteno {
                    konfigurace: Konfigurace::default(),
                    stav: StavKonfigurace::Obnovena,
                    zaloha: Some(zaloha),
                    ukladat: true,
                    chyby,
                },
                // Bez zálohy se soubor přepsat nesmí — byl by to jediný
                // otisk uživatelových kláves. A „obnoveno" by bylo
                // nepravdivé: okno by ukázalo odložený soubor, který
                // neexistuje, a nevarovalo by, že se změny neukládají.
                None => Nacteno {
                    chyby,
                    ..Nacteno::vychozi(StavKonfigurace::Necitelna, false)
                },
            }
        }
    }
}

/// [`nacti`] pro cestu z `updater::config_path()`, která chybí, když
/// není nastavená proměnná `APPDATA`: pak výchozí klávesy a nic se
/// neukládá (není kam).
pub fn nacti_z(cesta: Option<&Path>) -> Nacteno {
    match cesta {
        Some(c) => nacti(c),
        None => {
            log::warn!("konfigurace: APPDATA není nastavená — výchozí klávesy, nic se neukládá");
            Nacteno::vychozi(StavKonfigurace::Necitelna, false)
        }
    }
}

/// Odloží nevalidní soubor do [`ZALOHA`] (starší zálohu přepíše).
///
/// Přejmenování selže, když soubor drží otevřený jiný program bez sdílení
/// mazání; kopie pak ještě projde a originál přepíše až další zápis.
fn zalohuj(cesta: &Path) -> Option<PathBuf> {
    let zaloha = cesta.with_file_name(ZALOHA);
    // Starší záloha jen pro čtení by zablokovala přejmenování i kopii
    // (ERROR_ACCESS_DENIED, ověřeno) a nevalidní soubor by nešel odložit.
    povol_prepis(&zaloha);
    let vysledek = match std::fs::rename(cesta, &zaloha) {
        Ok(()) => {
            log::warn!(
                "konfigurace: nevalidní soubor odložen do {}",
                zaloha.display()
            );
            Some(zaloha)
        }
        Err(e) => match std::fs::copy(cesta, &zaloha) {
            Ok(_) => {
                log::warn!(
                    "konfigurace: přejmenovat nešlo ({e}), nevalidní soubor zkopírován do {}",
                    zaloha.display()
                );
                Some(zaloha)
            }
            Err(e2) => {
                log::error!(
                    "konfigurace: nevalidní soubor nejde zálohovat ({e}; kopie: {e2}) — \
                     nepřepisuje se, změny kláves se neuloží"
                );
                None
            }
        },
    };
    // Přejmenování i kopie převezmou atribut „jen pro čtení" od
    // `config.json`, který si uživatel zamkl proti přepsání. Záloha je
    // ale soubor KeyPadu a příští nevalidní soubor ji musí přepsat.
    if let Some(z) = &vysledek {
        povol_prepis(z);
    }
    vysledek
}

/// Sundá z vlastního souboru KeyPadu atribut „jen pro čtení".
///
/// Jen u obyčejného souboru: u odkazu (symlinku) by se atribut změnil
/// cíli, který KeyPadu nepatří (princip 8).
#[allow(
    clippy::permissions_set_readonly_false,
    reason = "ve Windows sundá jen atribut FILE_ATTRIBUTE_READONLY (KeyPad je jen pro Windows), \
              práva jiných uživatelů nemění"
)]
fn povol_prepis(cesta: &Path) {
    let Ok(m) = std::fs::symlink_metadata(cesta) else {
        return;
    };
    let mut prava = m.permissions();
    if m.is_file() && prava.readonly() {
        prava.set_readonly(false);
        if let Err(e) = std::fs::set_permissions(cesta, prava) {
            log::warn!(
                "konfigurace: {} je jen pro čtení a nejde to změnit ({e})",
                cesta.display()
            );
        }
    }
}

/// `config.json` → `config.json.tmp` (vedle, ať je `rename` na stejném
/// svazku, a tedy atomický).
fn docasny(cesta: &Path) -> PathBuf {
    let mut s = cesta.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

/// Atomický zápis: `.tmp` → `write_all` → `sync_all` → `rename`.
///
/// Soubor je tak vždy celý starý, nebo celý nový — i po výpadku proudu.
/// Zbylý `.tmp` z dřívějška se přepíše; po neúspěchu se `.tmp` uklidí.
pub fn uloz(cesta: &Path, k: &Konfigurace) -> std::io::Result<()> {
    if let Some(slozka) = cesta.parent() {
        std::fs::create_dir_all(slozka)?;
    }
    let tmp = docasny(cesta);
    let vysledek =
        zapis_a_sync(&tmp, do_textu(k).as_bytes()).and_then(|()| std::fs::rename(&tmp, cesta));
    if vysledek.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    vysledek
}

/// Zapíše `data` a počká, až jsou na disku — dřív, než je `rename`
/// zviditelní. Jinak by po výpadku proudu mohl zůstat nový název
/// s prázdným obsahem.
fn zapis_a_sync(cesta: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(cesta)?;
    f.write_all(data)?;
    f.sync_all()
}

// ── Vlákno keypad-konfig ───────────────────────────────────────────
// Mezi oknem a vláknem je schránka pod `Mutex`em, ne fronta: na disk
// stejně jde jen poslední obsah a fronta (`crossbeam`) by se v binárce
// rozvinula pro každý typ zprávy znovu (desítky kB, naměřeno). Zámek
// drží obě strany jen na přepsání pár polí — I/O běží mimo něj, takže
// vlákno okna na disk nikdy nečeká.

/// Hlášení změny stavu z vlákna. Za `Mutex`em jen kvůli tomu, aby se
/// dalo zavolat i tehdy, když vlákno nejde spustit (closure by se
/// s nepovedeným `spawn` zahodila); zamyká ho vždy jen jedno vlákno.
type Hlaseni = Arc<Mutex<dyn FnMut(StavKonfigurace) + Send>>;

fn ohlas(hlaseni: &Hlaseni, stav: StavKonfigurace) {
    // Panika v obsluze okna nesmí zastavit ukládání.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let mut f = hlaseni.lock().unwrap_or_else(PoisonError::into_inner);
        (*f)(stav);
    }));
}

/// Schránka mezi [`Ukladac`] a vláknem.
#[derive(Default)]
struct Schranka {
    /// Poslední požadovaný obsah (starší nezapsaný nahradí) a kdy přišel.
    novy: Option<(Box<Konfigurace>, Instant)>,
    /// Pořadí poslední žádosti „zapiš hned" a poslední vyřízené.
    hned: u64,
    vyrizeno: u64,
    /// Výsledek posledního vyřízení „hned": je všechno na disku?
    vysledek: bool,
    /// [`Ukladac`] zanikl — zapsat, co čeká, a skončit.
    konec: bool,
}

#[derive(Default)]
struct Sdilene {
    schranka: Mutex<Schranka>,
    /// Vlákno: přišla změna, žádost nebo konec.
    prace: Condvar,
    /// `uloz_hned`: žádost je vyřízená.
    hotovo: Condvar,
}

impl Sdilene {
    fn zamkni(&self) -> MutexGuard<'_, Schranka> {
        // Panika jiného vlákna pod zámkem nechá schránku v platném stavu
        // (jen přiřazení polí); ukládání má jet dál.
        self.schranka.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Ukládá konfiguraci z vlastního vlákna `keypad-konfig`.
///
/// [`Ukladac::uloz`] jen přepíše schránku a hned se vrátí — volá ho
/// vlákno okna, které na disk čekat nesmí. Zapíše se jen obsah, který se
/// od posledního zápisu (nebo od načtení) liší, takže chybějící soubor
/// vznikne až první skutečnou změnou.
pub struct Ukladac {
    /// `None` = bez vlákna (neukládá se).
    sdilene: Option<Arc<Sdilene>>,
    /// Bez vlákna: `true` = neukládá se záměrně (soubor z novější verze,
    /// nečitelný, nevalidní bez zálohy — okno to hlásí od startu),
    /// `false` = ukládat se mělo, ale nejde (není cesta, vlákno nevzniklo).
    zamerne: bool,
}

impl Ukladac {
    /// Spustí ukládání.
    ///
    /// `nacteno` je výsledek [`nacti`]: jeho obsah je výchozí bod
    /// (stejný obsah se nezapisuje) a `ukladat` říká, jestli se smí
    /// soubor vůbec přepsat. `pri_zmene_stavu` se volá z vlákna
    /// `keypad-konfig`, jen když se stav změní (zápis selhal, zase
    /// prošel, první zápis po obnově).
    pub fn spust(
        cesta: Option<PathBuf>,
        nacteno: &Nacteno,
        pri_zmene_stavu: impl FnMut(StavKonfigurace) + Send + 'static,
    ) -> Ukladac {
        Ukladac::spust_s(cesta, nacteno, ODKLAD, uloz, pri_zmene_stavu)
    }

    /// [`Ukladac::spust`] s vlastním odkladem a zápisem (testy).
    fn spust_s(
        cesta: Option<PathBuf>,
        nacteno: &Nacteno,
        odklad: Duration,
        zapis: impl FnMut(&Path, &Konfigurace) -> std::io::Result<()> + Send + 'static,
        pri_zmene_stavu: impl FnMut(StavKonfigurace) + Send + 'static,
    ) -> Ukladac {
        let bez_vlakna = |zamerne| Ukladac {
            sdilene: None,
            zamerne,
        };
        if !nacteno.ukladat {
            log::debug!("konfigurace: neukládá se ({:?})", nacteno.stav);
            return bez_vlakna(true);
        }
        let hlaseni: Hlaseni = Arc::new(Mutex::new(pri_zmene_stavu));
        let Some(cesta) = cesta else {
            log::warn!("konfigurace: není kam ukládat — změny kláves se neuloží");
            ohlas(&hlaseni, StavKonfigurace::Neulozena);
            return bez_vlakna(false);
        };
        let sdilene = Arc::new(Sdilene::default());
        let vlakno = Vlakno {
            sdilene: Arc::clone(&sdilene),
            cesta,
            odklad,
            zapis,
            hlaseni: Arc::clone(&hlaseni),
            ulozeno: nacteno.konfigurace.clone(),
            stav: nacteno.stav,
            neulozeno: None,
        };
        let spusteno = std::thread::Builder::new()
            .name("keypad-konfig".into())
            .spawn(move || vlakno.bez());
        match spusteno {
            Ok(_) => Ukladac {
                sdilene: Some(sdilene),
                zamerne: false,
            },
            Err(e) => {
                log::error!(
                    "konfigurace: vlákno keypad-konfig nejde spustit ({e}) — změny kláves se neuloží"
                );
                ohlas(&hlaseni, StavKonfigurace::Neulozena);
                bez_vlakna(false)
            }
        }
    }

    /// Uloží `k` [`ODKLAD`] po poslední změně. Na disk nikdy nečeká.
    pub fn uloz(&self, k: Konfigurace) {
        if let Some(sd) = &self.sdilene {
            sd.zamkni().novy = Some((Box::new(k), Instant::now()));
            sd.prace.notify_one();
        }
    }

    /// Zapíše hned, co čeká (konec aplikace), a počká nejvýš `limit`.
    ///
    /// `true` = co se mělo uložit, je na disku (i když nebylo co, nebo
    /// se záměrně neukládá). `false` = zápis selhal, nebo se nestihl.
    /// Zápis, který selhal dřív, se tu zkusí ještě jednou — je to
    /// poslední příležitost.
    pub fn uloz_hned(&self, limit: Duration) -> bool {
        let Some(sd) = &self.sdilene else {
            return self.zamerne;
        };
        let konec = Instant::now() + limit;
        let mut s = sd.zamkni();
        s.hned += 1;
        let moje = s.hned;
        sd.prace.notify_one();
        while s.vyrizeno < moje {
            let ted = Instant::now();
            if ted >= konec {
                return false;
            }
            s = sd
                .hotovo
                .wait_timeout(s, konec - ted)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        s.vysledek
    }
}

impl Drop for Ukladac {
    fn drop(&mut self) {
        // Vlákno zapíše, co čeká, a skončí.
        if let Some(sd) = &self.sdilene {
            sd.zamkni().konec = true;
            sd.prace.notify_one();
        }
    }
}

/// Stav vlákna `keypad-konfig`.
struct Vlakno<W> {
    sdilene: Arc<Sdilene>,
    cesta: PathBuf,
    odklad: Duration,
    zapis: W,
    hlaseni: Hlaseni,
    /// Obsah, který je na disku — nebo výchozí bod po načtení (chybějící
    /// soubor = výchozí klávesy). Stejný obsah se znovu nezapisuje.
    ulozeno: Konfigurace,
    stav: StavKonfigurace,
    /// Obsah, jehož zápis selhal; zkusí se znovu při „zapiš hned",
    /// novější změna ho nahradí.
    neulozeno: Option<Box<Konfigurace>>,
}

/// Co má vlákno udělat po probuzení.
struct Ukol {
    obsah: Option<Box<Konfigurace>>,
    /// Pořadí vyřizované žádosti „zapiš hned".
    hned: Option<u64>,
}

impl<W> Vlakno<W>
where
    W: FnMut(&Path, &Konfigurace) -> std::io::Result<()>,
{
    fn bez(mut self) {
        while let Some(ukol) = self.cekej() {
            match ukol.obsah {
                Some(k) => {
                    self.neulozeno = None;
                    self.zapis_obsah(k);
                }
                None if ukol.hned.is_some() => {
                    if let Some(k) = self.neulozeno.take() {
                        self.zapis_obsah(k);
                    }
                }
                None => {}
            }
            if let Some(n) = ukol.hned {
                let ok = self.stav != StavKonfigurace::Neulozena;
                let mut s = self.sdilene.zamkni();
                s.vyrizeno = n;
                s.vysledek = ok;
                self.sdilene.hotovo.notify_all();
            }
        }
    }

    /// Čeká na práci; `None` = konec. Bez čekající změny se spí bez
    /// limitu (princip 10), se změnou do jejího odkladu.
    fn cekej(&self) -> Option<Ukol> {
        let sd = &self.sdilene;
        let mut s = sd.zamkni();
        loop {
            if s.hned != s.vyrizeno {
                return Some(Ukol {
                    obsah: s.novy.take().map(|(k, _)| k),
                    hned: Some(s.hned),
                });
            }
            let termin = s.novy.as_ref().map(|(_, od)| *od + self.odklad);
            match termin {
                Some(t) if s.konec || Instant::now() >= t => {
                    return Some(Ukol {
                        obsah: s.novy.take().map(|(k, _)| k),
                        hned: None,
                    });
                }
                Some(t) => {
                    let zbyva = t.saturating_duration_since(Instant::now());
                    s = sd
                        .prace
                        .wait_timeout(s, zbyva)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                None if s.konec => return None,
                None => s = sd.prace.wait(s).unwrap_or_else(PoisonError::into_inner),
            }
        }
    }

    fn zapis_obsah(&mut self, k: Box<Konfigurace>) {
        let novy = if *k == self.ulozeno {
            // Změny se vrátily k obsahu na disku: není co psát, a po
            // dřívějším selhání je soubor zase v pořádku.
            match self.stav {
                StavKonfigurace::Neulozena => StavKonfigurace::Ok,
                s => s,
            }
        } else {
            let vysledek = catch_unwind(AssertUnwindSafe(|| (self.zapis)(&self.cesta, &k)));
            match vysledek {
                Ok(Ok(())) => {
                    let n = pocty(&k.mapovani);
                    log::info!(
                        "konfigurace: uloženo {} vazeb (ovladače 1–4: {}/{}/{}/{})",
                        k.mapovani.len(),
                        n[0],
                        n[1],
                        n[2],
                        n[3]
                    );
                    self.ulozeno = *k;
                    StavKonfigurace::Ok
                }
                Ok(Err(e)) => {
                    log::warn!(
                        "konfigurace: zápis {} selhal ({e}) — další pokus při příští změně",
                        self.cesta.display()
                    );
                    self.neulozeno = Some(k);
                    StavKonfigurace::Neulozena
                }
                Err(_) => {
                    log::error!("konfigurace: zápis zpanikařil — další pokus při příští změně");
                    self.neulozeno = Some(k);
                    StavKonfigurace::Neulozena
                }
            }
        };
        if novy != self.stav {
            self.stav = novy;
            ohlas(&self.hlaseni, novy);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keypad_core::{PadButton, StickDir};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Dočasná složka testu — nikdy skutečný `%APPDATA%`. Smaže se na konci.
    struct Slozka(PathBuf);

    impl Slozka {
        fn nova(jmeno: &str) -> Slozka {
            let p =
                std::env::temp_dir().join(format!("keypad-konfig-{}-{jmeno}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Slozka(p)
        }

        fn konfigurace(&self) -> PathBuf {
            self.0.join(updater::CONFIG_FILE)
        }

        fn zaloha(&self) -> PathBuf {
            self.0.join(ZALOHA)
        }

        fn soubory(&self) -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        }
    }

    impl Drop for Slozka {
        fn drop(&mut self) {
            // Soubory jen pro čtení (testy zálohy) by úklid zablokovaly,
            // i když test spadne uprostřed.
            if let Ok(soubory) = std::fs::read_dir(&self.0) {
                for s in soubory.flatten() {
                    povol_prepis(&s.path());
                }
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn zamkni(cesta: &Path) {
        let mut prava = std::fs::metadata(cesta).unwrap().permissions();
        prava.set_readonly(true);
        std::fs::set_permissions(cesta, prava).unwrap();
    }

    fn zamceny(cesta: &Path) -> bool {
        std::fs::metadata(cesta).unwrap().permissions().readonly()
    }

    fn cil(pad: usize, akce: Action) -> PadAction {
        PadAction::new(PadId::ALL[pad], akce)
    }

    /// Ovladač 1 bez W a s F12, ovladač 2 s W a numerickou 8, ovladač 4
    /// se šipkou (E0), jiná zkratka, vypnutý zvuk.
    fn vice_ovladacu() -> Konfigurace {
        let mut m = Mapping::default();
        m.bind(KeyId::W, cil(1, Action::LeftStick(StickDir::Up)))
            .unwrap();
        m.bind(KeyId::NUMPAD_8, cil(1, Action::Button(PadButton::A)))
            .unwrap();
        m.bind(KeyId::ARROW_UP, cil(3, Action::RightTrigger))
            .unwrap();
        m.bind(KeyId::new(0x58), cil(0, Action::LeftStick(StickDir::Up)))
            .unwrap();
        m.set_toggle_key(KeyId::new(0x45)).unwrap();
        Konfigurace {
            mapovani: m,
            zvuk: false,
        }
    }

    fn neplatna(text: &str) -> Vec<ChybaObsahu> {
        match z_textu(text) {
            Err(ChybaKonfigurace::Neplatna(c)) => c,
            jinak => panic!("čekal jsem nevalidní, je {jinak:?}\n{text}"),
        }
    }

    /// Soubor verze 1 se zkratkou Scroll Lock a danými vazbami
    /// `(ovladac, vstup, scan, e0)`.
    fn s_vazbami(vazby: &[(u64, &str, u64, bool)]) -> String {
        let vazby: Vec<Value> = vazby
            .iter()
            .map(|&(o, v, s, e)| json!({ "ovladac": o, "vstup": v, "scan": s, "e0": e }))
            .collect();
        json!({ "verze": 1, "zvuk": true, "zkratka": { "scan": 70, "e0": false }, "vazby": vazby })
            .to_string()
    }

    fn hodnota(vazba: Option<usize>, klic: &'static str) -> impl Fn(&ChybaObsahu) -> bool {
        move |c| matches!(c, ChybaObsahu::Hodnota { vazba: v, klic: k, .. } if *v == vazba && *k == klic)
    }

    #[test]
    fn vychozi_tam_a_zpet() {
        let k = Konfigurace::default();
        let text = do_textu(&k);
        assert_eq!(z_textu(&text), Ok(k.clone()));
        assert_eq!(
            do_textu(&z_textu(&text).unwrap()),
            text,
            "pořadí je stabilní"
        );
        // Platný JSON s poznámkou pro člověka.
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["verze"], 1);
        assert_eq!(v["zvuk"], true);
        assert_eq!(v["zkratka"], json!({ "scan": 70, "e0": false }));
        assert_eq!(v["poznamka"], POZNAMKA);
        assert_eq!(v["vazby"].as_array().unwrap().len(), 24);
        // Žádný osamělý LF — Poznámkový blok starších Windows.
        assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());
        // Ruční zápis nic neescapuje: kódy vstupů a poznámka to nepotřebují.
        for a in Action::ALL {
            assert!(a
                .code()
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
        }
        assert!(!POZNAMKA.contains(['"', '\\']));
    }

    #[test]
    fn vice_ovladacu_tam_a_zpet() {
        let k = vice_ovladacu();
        let text = do_textu(&k);
        assert_eq!(z_textu(&text), Ok(k));
        assert!(text.contains("\"zvuk\": false"));
        assert!(text.contains("\"zkratka\": { \"scan\": 69, \"e0\": false }"));
        assert!(text.contains("{ \"ovladac\": 4, \"vstup\": \"rt\", \"scan\": 72, \"e0\": true }"));
        assert!(text.contains("{ \"ovladac\": 2, \"vstup\": \"a\", \"scan\": 72, \"e0\": false }"));
    }

    /// Přesný tvar souboru — mění se jen vědomě (starší KeyPady ho čtou).
    #[test]
    fn tvar_souboru() {
        let m = Mapping::new(
            KeyId::SCROLL_LOCK,
            [
                (KeyId::ARROW_UP, cil(0, Action::RightStick(StickDir::Up))),
                (KeyId::W, cil(2, Action::LeftStick(StickDir::Up))),
            ],
        )
        .unwrap();
        let text = do_textu(&Konfigurace {
            mapovani: m,
            zvuk: true,
        });
        let cekam = [
            "{",
            "  \"poznamka\": \"KeyPad — klávesy virtuálních ovladačů. Soubor zapisuje aplikace; \
             nevalidní se přejmenuje na config.invalid.json a platí výchozí klávesy.\",",
            "  \"verze\": 1,",
            "  \"zvuk\": true,",
            "  \"zkratka\": { \"scan\": 70, \"e0\": false },",
            "  \"vazby\": [",
            "    { \"ovladac\": 3, \"vstup\": \"ls_up\", \"scan\": 17, \"e0\": false },",
            "    { \"ovladac\": 1, \"vstup\": \"rs_up\", \"scan\": 72, \"e0\": true }",
            "  ]",
            "}",
            "",
        ]
        .join("\r\n");
        assert_eq!(text, cekam);
    }

    #[test]
    fn poradi_vazeb_nezalezi_na_souboru() {
        let k = vice_ovladacu();
        let text = do_textu(&k);
        // Vazby pozpátku, bez odsazení a s LF.
        let mut v: Value = serde_json::from_str(&text).unwrap();
        v["vazby"].as_array_mut().unwrap().reverse();
        let prehazeny = serde_json::to_string(&v).unwrap();
        let zpet = z_textu(&prehazeny).unwrap();
        assert_eq!(zpet, k);
        assert_eq!(do_textu(&zpet), text);
    }

    #[test]
    fn chybejici_zvuk_a_zkratka_jsou_vychozi_a_nezname_klice_nevadi() {
        let text = json!({
            "verze": 1,
            "moje": "poznámka",
            "budouci": { "x": 1 },
            "vazby": [{ "ovladac": 1, "vstup": "a", "scan": 57, "e0": false, "barva": 3 }]
        })
        .to_string();
        let k = z_textu(&text).unwrap();
        assert!(k.zvuk);
        assert_eq!(k.mapovani.toggle_key(), KeyId::SCROLL_LOCK);
        assert_eq!(
            k.mapovani.target(KeyId::SPACE),
            Some(cil(0, Action::Button(PadButton::A)))
        );
        // e0 chybí = běžná klávesa; null = chybí.
        let bez_e0 = r#"{"verze":1,"zvuk":null,"zkratka":null,
            "vazby":[{"ovladac":1,"vstup":"b","scan":46,"e0":null}]}"#;
        let k = z_textu(bez_e0).unwrap();
        assert_eq!(
            k.mapovani.target(KeyId::C),
            Some(cil(0, Action::Button(PadButton::B)))
        );
        assert_eq!(k.mapovani.toggle_key(), KeyId::SCROLL_LOCK);
        assert!(k.zvuk);
    }

    #[test]
    fn bom_se_odstrihne() {
        let k = vice_ovladacu();
        let text = format!("\u{feff}{}", do_textu(&k));
        assert_eq!(z_textu(&text), Ok(k.clone()));

        let s = Slozka::nova("bom");
        std::fs::write(s.konfigurace(), text.as_bytes()).unwrap();
        assert_eq!(
            &std::fs::read(s.konfigurace()).unwrap()[..3],
            b"\xEF\xBB\xBF"
        );
        let n = nacti(&s.konfigurace());
        assert_eq!(n.stav, StavKonfigurace::Ok);
        assert_eq!(n.konfigurace, k);
        assert!(n.ukladat);
    }

    #[test]
    fn nevalidni_obsah_pozna_pravidla_jadra() {
        // Win jako vazba i jako zkratka.
        let c = neplatna(&s_vazbami(&[(1, "a", 0x5B, true)]));
        assert!(
            matches!(
                c[..],
                [ChybaObsahu::Mapovani(MappingError::Reserved { .. })]
            ),
            "{c:?}"
        );
        let c = neplatna(
            r#"{"verze":1,"zkratka":{"scan":92,"e0":true},"vazby":[{"ovladac":1,"vstup":"a","scan":57}]}"#,
        );
        assert!(matches!(
            c[..],
            [ChybaObsahu::Mapovani(MappingError::Reserved {
                action: None,
                ..
            })]
        ));
        // Ovladač mimo 1–4.
        for o in [0, 5, 300] {
            let c = neplatna(&s_vazbami(&[(o, "a", 0x39, false)]));
            assert_eq!(
                c,
                vec![ChybaObsahu::Ovladac {
                    vazba: 1,
                    ovladac: o
                }]
            );
        }
        // Duplicita: jedna klávesa na dvou vstupech.
        let c = neplatna(&s_vazbami(&[(1, "a", 0x39, false), (2, "b", 0x39, false)]));
        assert!(matches!(
            c[..],
            [ChybaObsahu::Mapovani(MappingError::DuplicateKey { .. })]
        ));
        // Neznámý vstup — a „prázdné" se k němu nepřidá.
        let c = neplatna(&s_vazbami(&[(1, "ls_upp", 0x11, false)]));
        assert_eq!(
            c,
            vec![ChybaObsahu::Vstup {
                vazba: 1,
                vstup: "ls_upp".into()
            }]
        );
        assert_eq!(c[0].to_string(), "vazba č. 1: neznámý vstup \"ls_upp\"");
        // Zkratka Esc, prázdné mapování, zkratka zároveň vazbou,
        // nemapovatelná klávesa.
        let c = neplatna(
            r#"{"verze":1,"zkratka":{"scan":1},"vazby":[{"ovladac":1,"vstup":"a","scan":57}]}"#,
        );
        assert_eq!(c, vec![ChybaObsahu::Mapovani(MappingError::ToggleIsEscape)]);
        assert_eq!(
            neplatna(r#"{"verze":1,"zvuk":true}"#),
            vec![ChybaObsahu::Mapovani(MappingError::Empty)]
        );
        let c = neplatna(&s_vazbami(&[(1, "a", 70, false)]));
        assert!(matches!(
            c[..],
            [ChybaObsahu::Mapovani(MappingError::ToggleKeyMapped { .. })]
        ));
        let c = neplatna(&s_vazbami(&[(1, "a", 0, false)]));
        assert!(matches!(
            c[..],
            [ChybaObsahu::Mapovani(MappingError::Unmappable { .. })]
        ));
        // Víc chyb najednou, každá s pořadím vazby.
        let c = neplatna(&s_vazbami(&[
            (1, "a", 0x39, false),
            (9, "x", 0x2D, false),
            (1, "nic", 0x2E, false),
        ]));
        assert_eq!(c.len(), 2, "{c:?}");
        assert!(matches!(
            c[0],
            ChybaObsahu::Ovladac {
                vazba: 2,
                ovladac: 9
            }
        ));
        assert!(matches!(c[1], ChybaObsahu::Vstup { vazba: 3, .. }));
    }

    #[test]
    fn nevalidni_verze_a_tvar() {
        assert_eq!(neplatna(r#"{"zvuk":true}"#), vec![ChybaObsahu::ChybiVerze]);
        assert_eq!(neplatna(r#"{"verze":null}"#), vec![ChybaObsahu::ChybiVerze]);
        assert_eq!(neplatna(r#"{"verze":0}"#), vec![ChybaObsahu::Verze(0)]);
        assert_eq!(neplatna(r#"{"verze":-3}"#), vec![ChybaObsahu::Verze(-3)]);
        for t in [r#"{"verze":"1"}"#, r#"{"verze":1.5}"#] {
            let c = neplatna(t);
            assert!(c.len() == 1 && hodnota(None, "verze")(&c[0]), "{t}: {c:?}");
        }
        // Rozbitý JSON: chyba nese číslo řádku a zpráva už pozici neopakuje.
        let c = neplatna("{\r\n  \"verze\": 1,\r\n  \"zvuk\": tru\r\n}\r\n");
        match &c[..] {
            [ChybaObsahu::Syntaxe {
                radek: Some(3),
                zprava,
            }] => assert!(!zprava.contains("line"), "{zprava}"),
            jinak => panic!("{jinak:?}"),
        }
        assert!(c[0].to_string().starts_with("řádek 3: "));
        // Není to objekt, prázdný soubor, smetí za koncem.
        for t in ["", "[1, 2]", "1", r#"{"verze":1} navíc"#] {
            let c = neplatna(t);
            assert!(matches!(c[..], [ChybaObsahu::Syntaxe { .. }]), "{t}: {c:?}");
        }
        // Špatné typy a rozsahy: všechny chyby naráz, s klíčem a vazbou.
        let c = neplatna(
            r#"{"verze":1,"zvuk":"ano","zkratka":{"scan":-1},
                "vazby":[{"ovladac":1,"vstup":"a","scan":70000,"e0":1},
                         {"ovladac":"1","scan":57},
                         7]}"#,
        );
        let cekam: [(Option<usize>, &str); 7] = [
            (None, "zvuk"),
            (None, "scan"),
            (Some(1), "scan"),
            (Some(1), "e0"),
            (Some(2), "ovladac"),
            (Some(2), "vstup"),
            (Some(3), ""),
        ];
        assert_eq!(c.len(), cekam.len(), "{c:?}");
        for (chyba, (vazba, klic)) in c.iter().zip(cekam) {
            assert!(hodnota(vazba, klic)(chyba), "{chyba:?} ≠ {vazba:?} {klic}");
        }
        assert_eq!(c[0].to_string(), "„zvuk“ musí být true nebo false");
        assert_eq!(
            c[2].to_string(),
            "vazba č. 1: „scan“ musí být celé číslo 0–65535"
        );
        assert!(c[6].to_string().starts_with("vazba č. 3 musí být objekt"));
        let c = neplatna(r#"{"verze":1,"vazby":5,"zkratka":3}"#);
        assert!(
            hodnota(None, "zkratka")(&c[0]) && hodnota(None, "vazby")(&c[1]),
            "{c:?}"
        );
    }

    #[test]
    fn novejsi_verze_se_pozna_pred_obsahem() {
        // Soubor z budoucnosti smí mít úplně jiný tvar — nesmí skončit
        // jako nevalidní (to by ho zálohovalo a přepsalo).
        let t = r#"{"verze":2,"vazby":"jinak","zvuk":3}"#;
        assert_eq!(z_textu(t), Err(ChybaKonfigurace::Novejsi { verze: 2 }));
    }

    #[test]
    fn chybejici_soubor_dava_vychozi_a_nic_nezalozi() {
        let s = Slozka::nova("chybi");
        let n = nacti(&s.konfigurace());
        assert_eq!(n.stav, StavKonfigurace::Ok);
        assert_eq!(n.konfigurace, Konfigurace::default());
        assert!(n.ukladat);
        assert_eq!(n.zaloha, None);
        assert!(s.soubory().is_empty(), "{:?}", s.soubory());
    }

    #[test]
    fn nevalidni_soubor_jde_do_zalohy() {
        let pripady = [
            ("rozbity", "{\"verze\": 1, \"zvuk\": }".to_string()),
            ("win", s_vazbami(&[(1, "a", 0x5B, true)])),
            ("ovladac5", s_vazbami(&[(5, "a", 0x39, false)])),
            (
                "duplicita",
                s_vazbami(&[(1, "a", 0x39, false), (1, "b", 0x39, false)]),
            ),
            ("vstup", s_vazbami(&[(1, "skok", 0x39, false)])),
            (
                "verze0",
                r#"{"verze":0,"vazby":[{"ovladac":1,"vstup":"a","scan":57}]}"#.to_string(),
            ),
            (
                "bezverze",
                r#"{"vazby":[{"ovladac":1,"vstup":"a","scan":57}]}"#.to_string(),
            ),
            (
                "esc",
                r#"{"verze":1,"zkratka":{"scan":1,"e0":false}}"#.to_string(),
            ),
            ("prazdne", r#"{"verze":1}"#.to_string()),
        ];
        for (jmeno, text) in pripady {
            let s = Slozka::nova(&format!("nevalidni-{jmeno}"));
            std::fs::write(s.konfigurace(), &text).unwrap();
            let n = nacti(&s.konfigurace());
            assert_eq!(n.stav, StavKonfigurace::Obnovena, "{jmeno}");
            assert_eq!(n.konfigurace, Konfigurace::default(), "{jmeno}");
            assert!(n.ukladat, "{jmeno}");
            assert!(!n.chyby.is_empty(), "{jmeno}");
            assert_eq!(n.zaloha.as_deref(), Some(s.zaloha().as_path()), "{jmeno}");
            assert_eq!(
                std::fs::read_to_string(s.zaloha()).unwrap(),
                text,
                "{jmeno}"
            );
            assert_eq!(s.soubory(), vec![ZALOHA.to_string()], "{jmeno}");
        }
    }

    #[test]
    fn nevalidni_bajty_jdou_do_zalohy() {
        let s = Slozka::nova("utf8");
        std::fs::write(s.konfigurace(), b"{\"verze\": 1, \"x\": \"\xE8esky\"}").unwrap();
        let n = nacti(&s.konfigurace());
        assert_eq!(n.stav, StavKonfigurace::Obnovena);
        assert_eq!(n.chyby, vec!["soubor není v kódování UTF-8".to_string()]);
        assert!(s.zaloha().is_file());
    }

    #[test]
    fn starsi_zaloha_se_prepise() {
        let s = Slozka::nova("starsi-zaloha");
        std::fs::write(s.zaloha(), "stará záloha").unwrap();
        std::fs::write(s.konfigurace(), "nové = nesmysl").unwrap();
        let n = nacti(&s.konfigurace());
        assert_eq!(n.stav, StavKonfigurace::Obnovena);
        assert_eq!(
            std::fs::read_to_string(s.zaloha()).unwrap(),
            "nové = nesmysl"
        );
        assert!(!s.konfigurace().exists());
    }

    /// Uživatel si ručně upravený soubor zamkne („jen pro čtení"), aby ho
    /// KeyPad nepřepsal. Přejmenovaná záloha by zámek zdědila a příští
    /// nevalidní soubor by už nešel odložit (rename i copy: přístup
    /// odepřen) — okno by pak lhalo „odloženo vedle" a změny by se
    /// tiše ztrácely.
    #[test]
    fn zaloha_jen_pro_cteni_se_prepise() {
        let s = Slozka::nova("zaloha-zamcena");
        std::fs::write(s.konfigurace(), "první = nesmysl").unwrap();
        zamkni(&s.konfigurace());
        let n = nacti(&s.konfigurace());
        assert_eq!((n.stav, n.ukladat), (StavKonfigurace::Obnovena, true));
        assert!(!zamceny(&s.zaloha()), "záloha zámek od config.json nezdědí");

        // Starší záloha jen pro čtení (dřívější verze, uživatel) se přepíše.
        zamkni(&s.zaloha());
        std::fs::write(s.konfigurace(), "druhý = nesmysl").unwrap();
        zamkni(&s.konfigurace());
        let n = nacti(&s.konfigurace());
        assert_eq!((n.stav, n.ukladat), (StavKonfigurace::Obnovena, true));
        assert_eq!(n.zaloha.as_deref(), Some(s.zaloha().as_path()));
        assert_eq!(
            std::fs::read_to_string(s.zaloha()).unwrap(),
            "druhý = nesmysl"
        );
        assert_eq!(s.soubory(), vec![ZALOHA.to_string()]);
        assert!(!zamceny(&s.zaloha()));
    }

    /// Kopie (soubor drží jiný program bez sdílení mazání, přejmenovat
    /// nejde) přepíše i zálohu jen pro čtení; originál zůstane a přepíše
    /// ho až první zápis.
    #[cfg(windows)]
    #[test]
    fn zamceny_soubor_se_zkopiruje_i_pres_zalohu_jen_pro_cteni() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;

        let s = Slozka::nova("kopie-zamcena");
        std::fs::write(s.zaloha(), "stará záloha").unwrap();
        zamkni(&s.zaloha());
        std::fs::write(s.konfigurace(), "nové = nesmysl").unwrap();
        zamkni(&s.konfigurace());
        let drzi = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(s.konfigurace())
            .unwrap();
        let n = nacti(&s.konfigurace());
        drop(drzi);
        assert_eq!((n.stav, n.ukladat), (StavKonfigurace::Obnovena, true));
        assert_eq!(
            std::fs::read_to_string(s.zaloha()).unwrap(),
            "nové = nesmysl"
        );
        assert!(!zamceny(&s.zaloha()), "kopie převzala atribut, sundá se");
        assert!(s.konfigurace().is_file(), "originál zůstal");
    }

    /// Zálohu nejde vytvořit vůbec (na jejím místě je složka): soubor se
    /// nesmí přepsat a okno musí říct „Klávesy se neukládají", ne
    /// „odloženo vedle" — stav jako u nečitelného souboru.
    #[test]
    fn nevalidni_soubor_bez_zalohy_se_neprepisuje_a_hlasi_neukladani() {
        let s = Slozka::nova("bez-zalohy");
        std::fs::create_dir(s.zaloha()).unwrap();
        std::fs::write(s.zaloha().join("cizi.txt"), "x").unwrap();
        let text = "{\"verze\": 1, \"zvuk\": }";
        std::fs::write(s.konfigurace(), text).unwrap();
        let n = nacti(&s.konfigurace());
        assert_eq!(n.stav, StavKonfigurace::Necitelna);
        assert!(!n.ukladat);
        assert_eq!(n.zaloha, None);
        assert_eq!(n.konfigurace, Konfigurace::default());
        assert!(!n.chyby.is_empty(), "chyby obsahu zůstanou pro log");
        assert_eq!(serde_json::to_string(&n.stav).unwrap(), "\"necitelna\"");

        // Ani změna kláves soubor nepřepíše. Konec aplikace nemá co
        // dohánět: neukládá se záměrně a okno to hlásí od startu.
        let (u, zapisy, stavy) = ukladac(&s, &n, Duration::from_millis(10));
        u.uloz(vice_ovladacu());
        assert!(u.uloz_hned(Duration::from_secs(5)));
        assert_eq!(zapisy.load(Ordering::SeqCst), 0);
        assert!(stavy.lock().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(s.konfigurace()).unwrap(), text);
        assert!(s.zaloha().join("cizi.txt").is_file());
    }

    #[test]
    fn poznamka_v_souboru_jmenuje_zalohu() {
        // Poznámka je literál (ručně psaný JSON), název zálohy konstanta
        // v updateru — rozejít se nesmí.
        assert!(POZNAMKA.contains(ZALOHA), "{POZNAMKA}");
        assert_eq!(ZALOHA, "config.invalid.json");
    }

    #[test]
    fn novejsi_soubor_zustane_nedotceny() {
        let s = Slozka::nova("novejsi");
        let obsah = br#"{"verze": 2, "zvuk": true, "vazby": [{"ovladac": "P1"}]}"#;
        std::fs::write(s.konfigurace(), obsah).unwrap();
        let n = nacti(&s.konfigurace());
        assert_eq!(n.stav, StavKonfigurace::Novejsi);
        assert!(!n.ukladat);
        assert_eq!(n.zaloha, None);
        assert_eq!(n.konfigurace, Konfigurace::default());
        assert_eq!(std::fs::read(s.konfigurace()).unwrap(), obsah);
        assert_eq!(s.soubory(), vec![updater::CONFIG_FILE.to_string()]);
        // Ukládání je vypnuté: ani změna soubor nepřepíše.
        let zapisy = Arc::new(AtomicUsize::new(0));
        let z = Arc::clone(&zapisy);
        let u = Ukladac::spust_s(
            Some(s.konfigurace()),
            &n,
            Duration::from_millis(10),
            move |c, k| {
                // Počítá se až hotový zápis — test pak čte celý soubor.
                let r = uloz(c, k);
                z.fetch_add(1, Ordering::SeqCst);
                r
            },
            |_| {},
        );
        u.uloz(vice_ovladacu());
        assert!(u.uloz_hned(Duration::from_secs(5)));
        assert_eq!(zapisy.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read(s.konfigurace()).unwrap(), obsah);
    }

    #[test]
    fn necitelny_soubor_se_neprepisuje() {
        // Složka místo souboru: existuje, ale přečíst nejde.
        let s = Slozka::nova("necitelny");
        std::fs::create_dir(s.konfigurace()).unwrap();
        let n = nacti(&s.konfigurace());
        assert_eq!(n.stav, StavKonfigurace::Necitelna);
        assert!(!n.ukladat);
        assert_eq!(n.konfigurace, Konfigurace::default());
        assert!(s.konfigurace().is_dir());
        assert!(!s.zaloha().exists());

        let n = nacti_z(None);
        assert_eq!(n.stav, StavKonfigurace::Necitelna);
        assert!(!n.ukladat);
    }

    #[test]
    fn zapis_je_atomicky_a_neneche_tmp() {
        let s = Slozka::nova("zapis");
        // Zbylý .tmp z přerušeného dřívějšího zápisu nevadí.
        std::fs::write(docasny(&s.konfigurace()), "napůl zapsan").unwrap();
        let k = vice_ovladacu();
        uloz(&s.konfigurace(), &k).unwrap();
        assert_eq!(s.soubory(), vec![updater::CONFIG_FILE.to_string()]);
        assert_eq!(
            std::fs::read_to_string(s.konfigurace()).unwrap(),
            do_textu(&k)
        );
        // Přepis existujícího souboru.
        uloz(&s.konfigurace(), &Konfigurace::default()).unwrap();
        assert_eq!(s.soubory(), vec![updater::CONFIG_FILE.to_string()]);
        let n = nacti(&s.konfigurace());
        assert_eq!(
            (n.stav, n.konfigurace),
            (StavKonfigurace::Ok, Konfigurace::default())
        );
        // Chybějící složka (první spuštění) se založí.
        let hloubeji = s.0.join("KeyPad").join(updater::CONFIG_FILE);
        uloz(&hloubeji, &k).unwrap();
        assert_eq!(nacti(&hloubeji).konfigurace, k);
    }

    #[test]
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "jen úklid souboru testu ve Windows"
    )]
    fn zapis_kam_nejde_vrati_chybu() {
        // Atribut „jen pro čtení" u složky Windows při zápisu nevynucují;
        // nezapisovatelná složka se proto zkouší souborem v cestě
        // a cílem jen pro čtení.
        let s = Slozka::nova("nejde");
        let soubor_misto_slozky = s.0.join("soubor");
        std::fs::write(&soubor_misto_slozky, "x").unwrap();
        let cesta = soubor_misto_slozky.join(updater::CONFIG_FILE);
        assert!(uloz(&cesta, &Konfigurace::default()).is_err());

        let cil = s.konfigurace();
        std::fs::write(&cil, "puvodni").unwrap();
        let mut prava = std::fs::metadata(&cil).unwrap().permissions();
        prava.set_readonly(true);
        std::fs::set_permissions(&cil, prava.clone()).unwrap();
        assert!(uloz(&cil, &Konfigurace::default()).is_err());
        assert_eq!(std::fs::read_to_string(&cil).unwrap(), "puvodni");
        assert!(!docasny(&cil).exists(), ".tmp se po neúspěchu uklidí");
        prava.set_readonly(false);
        std::fs::set_permissions(&cil, prava).unwrap();
    }

    #[test]
    fn stav_pro_okno() {
        let json = |s| serde_json::to_string(&s).unwrap();
        assert_eq!(json(StavKonfigurace::Ok), "\"ok\"");
        assert_eq!(json(StavKonfigurace::Obnovena), "\"obnovena\"");
        assert_eq!(json(StavKonfigurace::Novejsi), "\"novejsi\"");
        assert_eq!(json(StavKonfigurace::Necitelna), "\"necitelna\"");
        assert_eq!(json(StavKonfigurace::Neulozena), "\"neulozena\"");
    }

    // ── Ukladac ────────────────────────────────────────────────────

    type Zaznam = Arc<Mutex<Vec<StavKonfigurace>>>;

    /// Ukladac s počítadlem zápisů a záznamem hlášených stavů.
    fn ukladac(
        s: &Slozka,
        nacteno: &Nacteno,
        odklad: Duration,
    ) -> (Ukladac, Arc<AtomicUsize>, Zaznam) {
        let zapisy = Arc::new(AtomicUsize::new(0));
        let stavy: Zaznam = Arc::default();
        let (z, st) = (Arc::clone(&zapisy), Arc::clone(&stavy));
        let u = Ukladac::spust_s(
            Some(s.konfigurace()),
            nacteno,
            odklad,
            move |c, k| {
                // Počítá se až hotový zápis — test pak čte celý soubor.
                let r = uloz(c, k);
                z.fetch_add(1, Ordering::SeqCst);
                r
            },
            move |stav| st.lock().unwrap().push(stav),
        );
        (u, zapisy, stavy)
    }

    fn pockej_na(zapisy: &AtomicUsize, n: usize) {
        let konec = Instant::now() + Duration::from_secs(10);
        while zapisy.load(Ordering::SeqCst) < n && Instant::now() < konec {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn dve_zmeny_do_odkladu_jsou_jeden_zapis() {
        let s = Slozka::nova("odklad");
        let n = nacti(&s.konfigurace());
        let (u, zapisy, stavy) = ukladac(&s, &n, ODKLAD);
        let k1 = Konfigurace {
            zvuk: false,
            ..Konfigurace::default()
        };
        let k2 = vice_ovladacu();
        u.uloz(k1);
        u.uloz(k2.clone());
        pockej_na(&zapisy, 1);
        // Ještě chvíli: druhý zápis přijít nesmí.
        std::thread::sleep(ODKLAD + Duration::from_millis(200));
        assert_eq!(zapisy.load(Ordering::SeqCst), 1);
        assert_eq!(nacti(&s.konfigurace()).konfigurace, k2);
        assert!(stavy.lock().unwrap().is_empty(), "stav zůstal Ok");
        assert_eq!(s.soubory(), vec![updater::CONFIG_FILE.to_string()]);
    }

    #[test]
    fn dalsi_zmena_posune_odklad() {
        let s = Slozka::nova("posun");
        let n = nacti(&s.konfigurace());
        let (u, zapisy, _) = ukladac(&s, &n, ODKLAD);
        // Změny po 50 ms (dohromady déle než odklad): dokud chodí, nic
        // se nepíše.
        for i in 0..14 {
            u.uloz(Konfigurace {
                zvuk: i % 2 == 0,
                ..vice_ovladacu()
            });
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(zapisy.load(Ordering::SeqCst), 0);
        pockej_na(&zapisy, 1);
        assert_eq!(zapisy.load(Ordering::SeqCst), 1);
        assert!(!nacti(&s.konfigurace()).konfigurace.zvuk, "poslední změna");
    }

    #[test]
    fn stejny_obsah_se_nezapisuje_a_soubor_nevznikne() {
        let s = Slozka::nova("beze-zmeny");
        let n = nacti(&s.konfigurace());
        let (u, zapisy, _) = ukladac(&s, &n, ODKLAD);
        u.uloz(Konfigurace::default());
        assert!(u.uloz_hned(Duration::from_secs(5)));
        assert_eq!(zapisy.load(Ordering::SeqCst), 0);
        assert!(s.soubory().is_empty());
        // Změna a návrat zpět před odkladem = nic.
        u.uloz(vice_ovladacu());
        u.uloz(Konfigurace::default());
        assert!(u.uloz_hned(Duration::from_secs(5)));
        assert_eq!(zapisy.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn uloz_hned_zapise_bez_odkladu() {
        let s = Slozka::nova("hned");
        let n = nacti(&s.konfigurace());
        // Odklad hodina: zapsat to může jen `uloz_hned`.
        let (u, zapisy, _) = ukladac(&s, &n, Duration::from_secs(3600));
        u.uloz(vice_ovladacu());
        assert!(u.uloz_hned(Duration::from_secs(5)));
        assert_eq!(zapisy.load(Ordering::SeqCst), 1);
        assert_eq!(nacti(&s.konfigurace()).konfigurace, vice_ovladacu());
        // Nic nečeká → hned true, bez zápisu.
        assert!(u.uloz_hned(Duration::from_secs(5)));
        assert_eq!(zapisy.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn uloz_hned_neceka_dele_nez_limit() {
        let s = Slozka::nova("limit");
        let n = nacti(&s.konfigurace());
        let pust = Arc::new((Mutex::new(false), Condvar::new()));
        let p = Arc::clone(&pust);
        let u = Ukladac::spust_s(
            Some(s.konfigurace()),
            &n,
            ODKLAD,
            move |c, k| {
                // Zaseknutý disk.
                let (zamek, cv) = &*p;
                let g = zamek.lock().unwrap();
                let _g = cv
                    .wait_timeout_while(g, Duration::from_secs(30), |pusteno| !*pusteno)
                    .unwrap();
                uloz(c, k)
            },
            |_| {},
        );
        u.uloz(vice_ovladacu());
        let start = Instant::now();
        assert!(!u.uloz_hned(Duration::from_millis(100)));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
        // Vlákno okna mezitím smí dál ukládat — na zaseknutý disk nečeká.
        let start = Instant::now();
        u.uloz(Konfigurace::default());
        assert!(start.elapsed() < Duration::from_secs(1));
        *pust.0.lock().unwrap() = true;
        pust.1.notify_all();
        assert!(u.uloz_hned(Duration::from_secs(10)));
        assert_eq!(nacti(&s.konfigurace()).konfigurace, Konfigurace::default());
    }

    #[test]
    fn selhany_zapis_hlasi_neulozeno_a_dalsi_zmena_to_zkusi_znovu() {
        let s = Slozka::nova("selhani");
        let n = nacti(&s.konfigurace());
        let selhat = Arc::new(AtomicBool::new(true));
        let stavy: Zaznam = Arc::default();
        let (sel, st) = (Arc::clone(&selhat), Arc::clone(&stavy));
        let u = Ukladac::spust_s(
            Some(s.konfigurace()),
            &n,
            Duration::from_secs(3600),
            move |c, k| {
                if sel.load(Ordering::SeqCst) {
                    Err(std::io::Error::other("disk plný"))
                } else {
                    uloz(c, k)
                }
            },
            move |stav| st.lock().unwrap().push(stav),
        );
        u.uloz(vice_ovladacu());
        assert!(!u.uloz_hned(Duration::from_secs(5)));
        assert_eq!(*stavy.lock().unwrap(), vec![StavKonfigurace::Neulozena]);
        assert!(!s.konfigurace().exists());
        // Konec aplikace zkusí selhaný zápis ještě jednou.
        selhat.store(false, Ordering::SeqCst);
        assert!(u.uloz_hned(Duration::from_secs(5)));
        assert_eq!(
            *stavy.lock().unwrap(),
            vec![StavKonfigurace::Neulozena, StavKonfigurace::Ok]
        );
        assert_eq!(nacti(&s.konfigurace()).konfigurace, vice_ovladacu());

        // Selhání a pak návrat k obsahu na disku: není co psát, stav Ok.
        selhat.store(true, Ordering::SeqCst);
        u.uloz(Konfigurace::default());
        assert!(!u.uloz_hned(Duration::from_secs(5)));
        u.uloz(vice_ovladacu());
        assert!(u.uloz_hned(Duration::from_secs(5)));
        assert_eq!(
            *stavy.lock().unwrap(),
            vec![
                StavKonfigurace::Neulozena,
                StavKonfigurace::Ok,
                StavKonfigurace::Neulozena,
                StavKonfigurace::Ok
            ]
        );
    }

    #[test]
    fn panika_v_zapisu_ani_v_hlaseni_ukladani_nezastavi() {
        let s = Slozka::nova("panika");
        let n = nacti(&s.konfigurace());
        let panikarit = Arc::new(AtomicBool::new(true));
        let p = Arc::clone(&panikarit);
        let u = Ukladac::spust_s(
            Some(s.konfigurace()),
            &n,
            Duration::from_secs(3600),
            move |c, k| {
                assert!(!p.load(Ordering::SeqCst), "zkušební panika zápisu");
                uloz(c, k)
            },
            |_| panic!("zkušební panika hlášení"),
        );
        u.uloz(vice_ovladacu());
        assert!(!u.uloz_hned(Duration::from_secs(5)));
        panikarit.store(false, Ordering::SeqCst);
        assert!(u.uloz_hned(Duration::from_secs(5)));
        assert_eq!(nacti(&s.konfigurace()).konfigurace, vice_ovladacu());
    }

    #[test]
    fn po_obnove_prvni_zapis_vrati_stav_ok() {
        let s = Slozka::nova("obnova");
        std::fs::write(s.konfigurace(), r#"{"verze":1}"#).unwrap();
        let n = nacti(&s.konfigurace());
        assert_eq!(n.stav, StavKonfigurace::Obnovena);
        let (u, zapisy, stavy) = ukladac(&s, &n, Duration::from_secs(3600));
        // Výchozí = obsah po obnově: soubor nevznikne, stav zůstane.
        u.uloz(Konfigurace::default());
        assert!(u.uloz_hned(Duration::from_secs(5)));
        assert_eq!(zapisy.load(Ordering::SeqCst), 0);
        assert!(stavy.lock().unwrap().is_empty());
        u.uloz(vice_ovladacu());
        assert!(u.uloz_hned(Duration::from_secs(5)));
        assert_eq!(*stavy.lock().unwrap(), vec![StavKonfigurace::Ok]);
        assert!(s.zaloha().is_file(), "záloha zůstává");
    }

    #[test]
    fn zanik_ukladace_zapise_cekajici_zmenu() {
        let s = Slozka::nova("zanik");
        let n = nacti(&s.konfigurace());
        let (u, zapisy, _) = ukladac(&s, &n, Duration::from_secs(3600));
        u.uloz(vice_ovladacu());
        drop(u);
        pockej_na(&zapisy, 1);
        assert_eq!(zapisy.load(Ordering::SeqCst), 1);
        assert_eq!(nacti(&s.konfigurace()).konfigurace, vice_ovladacu());
    }

    #[test]
    fn bez_cesty_se_hlasi_neulozeno() {
        let s = Slozka::nova("bez-cesty");
        let n = nacti_z(Some(&s.konfigurace()));
        assert!(n.ukladat);
        let stavy: Zaznam = Arc::default();
        let st = Arc::clone(&stavy);
        let u = Ukladac::spust(None, &n, move |stav| st.lock().unwrap().push(stav));
        assert_eq!(*stavy.lock().unwrap(), vec![StavKonfigurace::Neulozena]);
        u.uloz(vice_ovladacu());
        assert!(!u.uloz_hned(Duration::from_millis(100)));
    }
}

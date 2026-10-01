//! Znamení pozastavení mimo okno (Fáze 4b): kdy pípnout a co ukázat
//! v oznamovací oblasti. Čisté funkce — rozhodování se testuje celou
//! tabulkou, bez zvuku a bez ikony; vlákno okna je jen volá.
//!
//! Zvuk nese hlavní zprávu (ikonu hra přes celou obrazovku skryje),
//! proto pípá jen tam, kde hráč změnu sám nezpůsobil nebo ji nevidí:
//! pozastavení zkratkou, nabídkou a pojistkou, návrat do hry. Zapnutí
//! a vypnutí přepínačem v okně nepípá — uživatel se právě dívá do okna
//! a Windows hrají svůj zvuk připojení a odpojení zařízení. Zamčení
//! počítače taky ne; Windows ho ale ohlásí až po přepnutí plochy, a tak
//! tón k přepnutí plochy (výzva UAC) chvíli čeká ([`ODKLAD_PLOCHY_MS`]).

use keypad_core::{ForceReason, MAX_PADS};

use crate::platform::windows::pad::PadStav;
use crate::platform::windows::vystup::{Pricina, Rezim};
use crate::platform::windows::zvuk::Zvuk;
use crate::tray::{DruhIkony, TrayStav};

/// Zvuk k přechodu režimu `pred` → `ted` (tabulka „Pravidla zvuku“
/// ve specifikaci Fáze 6, OQ 45). Ticho při konci aplikace, vypnutém
/// zvuku a simulaci řeší [`smi_znit`], odklad tónu k přepnutí plochy
/// [`Znameni`].
pub fn zvuk_pro(pred: Rezim, ted: Rezim, pricina: Pricina) -> Option<Zvuk> {
    use Rezim::{Binding, Capturing, Disabled, NoHook, Paused};
    match (pred, ted) {
        // Přiřazování klávesy začíná i končí v okně, na které se hráč
        // právě dívá.
        (Binding, _) | (_, Binding) => None,
        (Capturing, Paused) => match pricina {
            Pricina::Zkratka | Pricina::Okno => Some(Zvuk::Pauza),
            // Zamčení, spánek a konec mají vlastní projev Windows; pípnutí
            // by jen rušilo. Zamčení sem dojde jen zřídka — Windows ho
            // hlásí až po přepnutí plochy a to pozastaví dřív; tón
            // k přepnutí plochy proto ruší zamčení v [`Znameni`].
            Pricina::Vynuceno(
                ForceReason::SessionLock | ForceReason::Suspend | ForceReason::Shutdown,
            ) => None,
            // Watchdog, chyba padu, panika, UAC, přeinstalace hooku, změna
            // mapování: hráč jinak nepozná, proč klávesy přestaly hrát.
            Pricina::Vynuceno(_) => Some(Zvuk::Pauza),
            Pricina::Nic | Pricina::Ovladac | Pricina::OvladacChyba | Pricina::Prirazovani => None,
        },
        (Capturing, NoHook) => Some(Zvuk::Pauza),
        // Jen výpadek chybou: vypnutí přepínačem a spánek (`Ovladac`)
        // doprovodí zvuk odpojení od Windows.
        (Capturing, Disabled) if pricina == Pricina::OvladacChyba => Some(Zvuk::Pauza),
        (Paused | NoHook, Capturing) => Some(Zvuk::Hra),
        _ => None,
    }
}

/// Smí vlákno okna pípnout: ✓ Zvuk je zaškrtnutý, aplikace nekončí
/// (konec vynucuje Klávesnici a zněl by jako pozastavení) a nejde
/// o simulaci ViGEmBus (testy okna nesmí pípat vlastníkovi).
pub fn smi_znit(zvuk_zapnuty: bool, konci: bool, simulace: bool) -> bool {
    zvuk_zapnuty && !konci && !simulace
}

/// Jak dlouho tón k přepnutí plochy čeká, jestli nejde o zamčení.
///
/// Zamčení počítače (Win+L, Start → Zamknout) Windows ohlásí nejdřív
/// jako přepnutí na zabezpečenou plochu a zamčení relace až po něm
/// (v logu vlastníka o 4–6 ms, a to ještě jde přes vlákno relace).
/// Pojistka plochy tak pozastaví první a příčinu zapíše ona; zamčení
/// už režim nemění. Rezerva je na počítač vytížený hrou — výzva UAC
/// pípne o tolik později, pořád ale spolu s ní.
pub const ODKLAD_PLOCHY_MS: u64 = 500;

/// Zvuk z pohledu vlákna okna mezi obrátkami: odkud se počítá příští
/// změna a který tón čeká na odklad. Čisté — čas, zamčení a jestli
/// smí znít dodá vlákno, takže celou posloupnost jde otestovat bez
/// okna a bez zvuku.
#[derive(Debug)]
pub struct Znameni {
    /// Odkud se počítá zvuk příští změny ([`zaklad_zvuku`]).
    zaklad: Rezim,
    /// Tón k přepnutí plochy čekající [`ODKLAD_PLOCHY_MS`] a kdy vlákno
    /// okna změnu vidělo (`GetTickCount64`).
    odlozeny: Option<(Zvuk, u64)>,
}

impl Znameni {
    /// `rezim` = režim, který vlákno okna vidí při startu.
    pub fn new(rezim: Rezim) -> Znameni {
        Znameni {
            zaklad: rezim,
            odlozeny: None,
        }
    }

    /// Jedna obrátka vlákna okna; vrátí tón, který se má přehrát teď.
    ///
    /// `zmena` = nový režim a příčina, když se od minulé obrátky změnilo
    /// číslo změny. `ted` = `GetTickCount64`. `zamek` = kdy se naposledy
    /// zamkla relace (0 = zatím ne). `smi` = [`smi_znit`].
    pub fn obratka(
        &mut self,
        zmena: Option<(Rezim, Pricina)>,
        ted: u64,
        zamek: u64,
        smi: bool,
    ) -> Option<Zvuk> {
        let mut zvuk = None;
        // Čekající tón zruší zamčení. Novější změna ho nahradí: ohlašoval
        // stav, který už neplatí (uspání hned po přepnutí plochy je tiché,
        // návrat do hry má svůj tón).
        if let Some((z, od)) = self.odlozeny.take() {
            if zmena.is_none() && !zamceno_kolem(od, zamek) {
                if ted.saturating_sub(od) >= ODKLAD_PLOCHY_MS {
                    zvuk = Some(z);
                } else {
                    self.odlozeny = Some((z, od));
                }
            }
        }
        if let Some((rezim, pricina)) = zmena {
            // Tón se počítá od PŘEDCHOZÍHO základu, teprve pak se základ
            // posune — obráceně by žádná změna nepípla.
            let z = zvuk_pro(self.zaklad, rezim, pricina);
            self.zaklad = zaklad_zvuku(rezim, pricina);
            if pricina == Pricina::Vynuceno(ForceReason::DesktopSwitch) {
                self.odlozeny = z.map(|z| (z, ted));
            } else {
                zvuk = z;
            }
        }
        if !smi {
            // I čekající tón: zvuk vypnutý během odkladu, konec aplikace.
            self.odlozeny = None;
            return None;
        }
        zvuk
    }

    /// Za kolik ms se má vlákno okna probudit kvůli čekajícímu tónu;
    /// `None` = nic nečeká a vlákno smí spát bez limitu (princip 10).
    pub fn probudit_za(&self, ted: u64) -> Option<u64> {
        self.odlozeny
            .map(|(_, od)| od.saturating_add(ODKLAD_PLOCHY_MS).saturating_sub(ted))
    }
}

/// Zamkla se relace kolem přepnutí plochy, které vlákno okna vidělo
/// v `od`? Zamčení obvykle přijde až po přepnutí, jenže vlákno okna
/// mohlo změnu vidět ještě později (a `GetTickCount64` jde po ~16 ms)
/// — proto rezerva i zpět. Staré zamčení (odemčeno, pak výzva UAC) se
/// nepočítá.
fn zamceno_kolem(od: u64, zamek: u64) -> bool {
    zamek != 0 && zamek.saturating_add(ODKLAD_PLOCHY_MS) >= od
}

/// Režim, od kterého se počítá zvuk příští změny.
///
/// Pozastavení s příčinou „ovladač“ vzniká jen prvním zapnutím
/// (`Disabled` → Klávesnice) a hned za ním jde zachytávání (přepínač
/// JE povel hrát). Pro zvuk se proto bere jako `Disabled` — jinak by
/// první zapnutí pípalo „Hra“ podle toho, jestli vlákno okna stihlo
/// mezikrok vidět, nebo ne.
pub fn zaklad_zvuku(ted: Rezim, pricina: Pricina) -> Rezim {
    match (ted, pricina) {
        (Rezim::Paused, Pricina::Ovladac) => Rezim::Disabled,
        _ => ted,
    }
}

/// Bublina má v `NOTIFYICONDATAW::szTip` 128 znaků UTF-16 včetně nuly.
const MAX_BUBLINA: usize = 127;

/// Ikona, bublina a položka Pozastavit/Pokračovat podle režimu a stavu
/// všech ovladačů (`None` = vlákno ovladače neběží = vypnutý).
/// `zkratka` je název klávesy pozastavení podle rozložení.
pub fn stav_oblasti(rezim: Rezim, pady: &[Option<PadStav>; MAX_PADS], zkratka: &str) -> TrayStav {
    let ma = |s: PadStav| pady.contains(&Some(s));
    let (ikona, popisek, pauza) = match rezim {
        Rezim::Capturing => {
            let zapnute = pady.iter().filter(|&&p| p == Some(PadStav::On)).count();
            let pocet = if zapnute > 1 {
                format!(" ({zapnute} ovladače)")
            } else {
                String::new()
            };
            (
                DruhIkony::Hraje,
                format!("KeyPad — hraje · {zkratka} pozastaví{pocet}"),
                Some(false),
            )
        }
        Rezim::Paused => (
            DruhIkony::Pauza,
            format!("KeyPad — pozastaveno · {zkratka} pokračuje"),
            Some(true),
        ),
        // Přiřazování zachytávání pozastaví a po konci se vrátí: se
        // zapnutým ovladačem do hry nebo pauzy, bez něj do „vypnuto“ —
        // ikona podle toho. Zkratka ani nabídka ho nepřepnou (engine
        // zkratku odmítne), bublina proto radu se zkratkou nedává.
        Rezim::Binding => {
            let ikona = if ma(PadStav::On) {
                DruhIkony::Pauza
            } else if ma(PadStav::Error) {
                DruhIkony::Pozor
            } else {
                DruhIkony::Vypnuto
            };
            (ikona, "KeyPad — přiřazuji klávesu".into(), None)
        }
        Rezim::NoHook => (DruhIkony::Pozor, "KeyPad — klávesy nejdou".into(), None),
        Rezim::Disabled if ma(PadStav::Error) => {
            (DruhIkony::Pozor, "KeyPad — chyba ovladače".into(), None)
        }
        Rezim::Disabled => {
            let text = if ma(PadStav::BusMissing) {
                "KeyPad — chybí ViGEmBus"
            } else if ma(PadStav::BusNotRunning) {
                "KeyPad — ViGEmBus neběží"
            } else {
                "KeyPad — vypnuto"
            };
            (DruhIkony::Vypnuto, text.into(), None)
        }
    };
    TrayStav {
        ikona,
        popisek: utni(popisek, MAX_BUBLINA),
        pauza,
    }
}

/// Utne text na `max` jednotek UTF-16 (na hranici znaku) — dlouhý název
/// zkratky nesmí bublinu přetéct.
fn utni(mut s: String, max: usize) -> String {
    let mut delka = 0;
    for (i, c) in s.char_indices() {
        delka += c.len_utf16();
        if delka > max {
            s.truncate(i);
            break;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::windows::hook::Vystup;
    use crate::platform::windows::vystup::HookVystup;

    const REZIMY: [Rezim; 5] = [
        Rezim::Disabled,
        Rezim::Paused,
        Rezim::Capturing,
        Rezim::Binding,
        Rezim::NoHook,
    ];

    const VYNUCENI: [ForceReason; 9] = [
        ForceReason::Watchdog,
        ForceReason::PadError,
        ForceReason::SessionLock,
        ForceReason::DesktopSwitch,
        ForceReason::Suspend,
        ForceReason::HookPanic,
        ForceReason::HookReinstalled,
        ForceReason::MappingChanged,
        ForceReason::Shutdown,
    ];

    fn priciny() -> Vec<Pricina> {
        let mut v = vec![
            Pricina::Nic,
            Pricina::Zkratka,
            Pricina::Okno,
            Pricina::Ovladac,
            Pricina::OvladacChyba,
            Pricina::Prirazovani,
        ];
        v.extend(VYNUCENI.map(Pricina::Vynuceno));
        v
    }

    /// Nezávislý přepis tabulky „Pravidla zvuku“ řádek po řádku — test
    /// jím prochází VŠECHNY kombinace, ne jen vybrané.
    fn podle_tabulky(pred: Rezim, ted: Rezim, p: Pricina) -> Option<Zvuk> {
        if pred == Rezim::Binding || ted == Rezim::Binding {
            return None;
        }
        if pred == Rezim::Capturing && ted == Rezim::Paused {
            let tiche = [
                ForceReason::SessionLock,
                ForceReason::Suspend,
                ForceReason::Shutdown,
            ];
            return match p {
                Pricina::Zkratka | Pricina::Okno => Some(Zvuk::Pauza),
                Pricina::Vynuceno(r) if !tiche.contains(&r) => Some(Zvuk::Pauza),
                _ => None,
            };
        }
        if pred == Rezim::Capturing && ted == Rezim::NoHook {
            return Some(Zvuk::Pauza);
        }
        if pred == Rezim::Capturing && ted == Rezim::Disabled {
            return (p == Pricina::OvladacChyba).then_some(Zvuk::Pauza);
        }
        if (pred == Rezim::Paused || pred == Rezim::NoHook) && ted == Rezim::Capturing {
            return Some(Zvuk::Hra);
        }
        None
    }

    #[test]
    fn zvuk_celou_tabulkou() {
        for pred in REZIMY {
            for ted in REZIMY {
                for p in priciny() {
                    assert_eq!(
                        zvuk_pro(pred, ted, p),
                        podle_tabulky(pred, ted, p),
                        "{pred:?} → {ted:?}, {p:?}"
                    );
                }
            }
        }
    }

    /// Vybrané řádky tabulky napřímo (kdyby se přepis výš spletl stejně).
    #[test]
    fn zvuk_hlavni_pripady() {
        use Rezim::*;
        let z = zvuk_pro;
        assert_eq!(z(Capturing, Paused, Pricina::Zkratka), Some(Zvuk::Pauza));
        assert_eq!(z(Paused, Capturing, Pricina::Zkratka), Some(Zvuk::Hra));
        assert_eq!(z(Capturing, Paused, Pricina::Okno), Some(Zvuk::Pauza));
        // Výzva UAC během hry pípne, zamčení a spánek ne (zamčení, které
        // přijde až po přepnutí plochy: `zamceni_za_hry_nepipa`).
        let uac = Pricina::Vynuceno(ForceReason::DesktopSwitch);
        assert_eq!(z(Capturing, Paused, uac), Some(Zvuk::Pauza));
        let zamek = Pricina::Vynuceno(ForceReason::SessionLock);
        assert_eq!(z(Capturing, Paused, zamek), None);
        // Vypnutí přepínačem nepípá, výpadek ovladače chybou ano.
        assert_eq!(z(Capturing, Disabled, Pricina::Ovladac), None);
        assert_eq!(
            z(Capturing, Disabled, Pricina::OvladacChyba),
            Some(Zvuk::Pauza)
        );
        assert_eq!(z(Capturing, NoHook, Pricina::Nic), Some(Zvuk::Pauza));
        assert_eq!(z(NoHook, Capturing, Pricina::Nic), Some(Zvuk::Hra));
        assert_eq!(z(Disabled, Capturing, Pricina::Okno), None);
        assert_eq!(z(Capturing, Binding, Pricina::Okno), None);
        assert_eq!(z(Binding, Capturing, Pricina::Prirazovani), None);
    }

    /// Posloupnost změn, jak ji vidí vlákno okna ([`Znameni`]): po každé
    /// změně se vlákno probudí ještě jednou, až vyprší odklad.
    fn zvuky(zmeny: &[(Rezim, Pricina)]) -> Vec<Zvuk> {
        let mut z = Znameni::new(Rezim::Disabled);
        let mut t = 1_000;
        let mut v = Vec::new();
        for &zmena in zmeny {
            v.extend(z.obratka(Some(zmena), t, 0, true));
            t += ODKLAD_PLOCHY_MS;
            v.extend(z.obratka(None, t, 0, true));
            t += 1_000;
        }
        v
    }

    /// První zapnutí ovladače nepípá, ať vlákno okna mezikrok
    /// „pozastaveno“ stihne vidět, nebo ne; zapnutí dalšího ovladače
    /// z pozastavení (OQ 34) a zkratka pípají.
    #[test]
    fn prvni_zapnuti_nepipa() {
        use Rezim::*;
        assert!(zvuky(&[(Paused, Pricina::Ovladac), (Capturing, Pricina::Okno)]).is_empty());
        assert!(zvuky(&[(Capturing, Pricina::Okno)]).is_empty());
        assert_eq!(
            zvuky(&[
                (Paused, Pricina::Ovladac),
                (Capturing, Pricina::Okno),
                (Paused, Pricina::Zkratka),
                (Capturing, Pricina::Okno),
                (Paused, Pricina::Okno),
                (Capturing, Pricina::Zkratka),
            ]),
            [Zvuk::Pauza, Zvuk::Hra, Zvuk::Pauza, Zvuk::Hra]
        );
        // Mezikrok jen pro zvuk — jinde se režim nemění.
        for r in REZIMY {
            for p in priciny() {
                let ocekavano = if r == Paused && p == Pricina::Ovladac {
                    Disabled
                } else {
                    r
                };
                assert_eq!(zaklad_zvuku(r, p), ocekavano);
            }
        }
    }

    /// Tón se počítá od předchozího režimu a základ se posune až po něm
    /// — obrácené pořadí by umlčelo všechno (i pauzu zkratkou).
    #[test]
    fn zaklad_se_posune_az_po_tonu() {
        use Rezim::*;
        let mut z = Znameni::new(Capturing);
        assert_eq!(
            z.obratka(Some((Paused, Pricina::Zkratka)), 0, 0, true),
            Some(Zvuk::Pauza)
        );
        assert_eq!(
            z.obratka(Some((Capturing, Pricina::Zkratka)), 10, 0, true),
            Some(Zvuk::Hra)
        );
        assert_eq!(z.obratka(None, 20, 0, true), None, "bez změny nic");
        let mut z = Znameni::new(Capturing);
        assert_eq!(
            z.obratka(Some((Disabled, Pricina::OvladacChyba)), 0, 0, true),
            Some(Zvuk::Pauza)
        );
    }

    #[test]
    fn smi_znit_celou_tabulkou() {
        for zapnuty in [false, true] {
            for konci in [false, true] {
                for simulace in [false, true] {
                    assert_eq!(
                        smi_znit(zapnuty, konci, simulace),
                        zapnuty && !konci && !simulace,
                        "zvuk {zapnuty}, konec {konci}, simulace {simulace}"
                    );
                }
            }
        }
        assert!(smi_znit(true, false, false));
        assert!(!smi_znit(true, true, false), "konec aplikace nepípá");
    }

    /// Když zvuk nesmí znít (✓ Zvuk odškrtnutý, konec, simulace), nezazní
    /// nic — ani žádný přechod, ani tón, který čekal na odklad.
    #[test]
    fn ticho_kdyz_nesmi_znit() {
        for pred in REZIMY {
            for ted in REZIMY {
                for p in priciny() {
                    let mut z = Znameni::new(pred);
                    assert_eq!(z.obratka(Some((ted, p)), 0, 0, false), None);
                    assert_eq!(z.probudit_za(0), None, "{pred:?} → {ted:?}, {p:?}");
                    assert_eq!(z.obratka(None, 10 * ODKLAD_PLOCHY_MS, 0, true), None);
                }
            }
        }
        // Zvuk vypnutý (nebo konec aplikace) během odkladu.
        let uac = Pricina::Vynuceno(ForceReason::DesktopSwitch);
        let mut z = Znameni::new(Rezim::Capturing);
        assert_eq!(z.obratka(Some((Rezim::Paused, uac)), 0, 0, true), None);
        assert_eq!(z.obratka(None, ODKLAD_PLOCHY_MS, 0, false), None);
        assert_eq!(z.obratka(None, 2 * ODKLAD_PLOCHY_MS, 0, true), None);
        assert_eq!(z.probudit_za(2 * ODKLAD_PLOCHY_MS), None);
    }

    /// Zamčení za hry: Windows napřed přepnou plochu (pozastaví
    /// s příčinou DesktopSwitch) a zamčení relace ohlásí o pár ms
    /// později — tón pauzy nezazní. Časy jako v logu vlastníka.
    #[test]
    fn zamceni_za_hry_nepipa() {
        use Rezim::*;
        let plocha = Pricina::Vynuceno(ForceReason::DesktopSwitch);
        let t = 1_000_000;
        // Vlákno se probudí zamčením hned, nebo až po odkladu.
        for (probuzeni, zamek) in [
            (t + 5, t + 5),
            (t + ODKLAD_PLOCHY_MS, t + 5),
            (t + ODKLAD_PLOCHY_MS, t + 16),
            // GetTickCount64 jde po ~16 ms a vlákno okna mohlo změnu
            // vidět až po zamčení.
            (t + ODKLAD_PLOCHY_MS, t - 10),
            (t + 3 * ODKLAD_PLOCHY_MS, t + ODKLAD_PLOCHY_MS - 1),
        ] {
            let mut z = Znameni::new(Capturing);
            assert_eq!(z.obratka(Some((Paused, plocha)), t, 0, true), None);
            assert_eq!(z.probudit_za(t), Some(ODKLAD_PLOCHY_MS));
            assert_eq!(z.probudit_za(t + 100), Some(ODKLAD_PLOCHY_MS - 100));
            assert_eq!(z.obratka(None, probuzeni, zamek, true), None, "{zamek}");
            assert_eq!(z.probudit_za(probuzeni), None, "zamčení tón zrušilo");
            assert_eq!(
                z.obratka(None, t + 10 * ODKLAD_PLOCHY_MS, zamek, true),
                None
            );
            // Po odemčení vrací hru zkratka — ta pípne.
            assert_eq!(
                z.obratka(Some((Capturing, Pricina::Zkratka)), t + 60_000, zamek, true),
                Some(Zvuk::Hra)
            );
        }
        // Obrácené pořadí (zamčení první) pípat nemá už podle tabulky.
        let mut z = Znameni::new(Capturing);
        let zamek = Pricina::Vynuceno(ForceReason::SessionLock);
        assert_eq!(z.obratka(Some((Paused, zamek)), t, t, true), None);
        assert_eq!(z.probudit_za(t), None);
    }

    /// Výzva UAC (a obrazovka Ctrl+Alt+Del) za hry pípne — o odklad
    /// později, když se mezitím nic nezamklo. Staré zamčení se nepočítá.
    #[test]
    fn vyzva_uac_pipne_po_odkladu() {
        use Rezim::*;
        let plocha = Pricina::Vynuceno(ForceReason::DesktopSwitch);
        let t = 1_000_000;
        for zamek in [0, t - ODKLAD_PLOCHY_MS - 1, t - 60_000] {
            let mut z = Znameni::new(Capturing);
            assert_eq!(z.obratka(Some((Paused, plocha)), t, zamek, true), None);
            assert_eq!(z.obratka(None, t + 100, zamek, true), None, "ještě ne");
            assert_eq!(
                z.obratka(None, t + ODKLAD_PLOCHY_MS, zamek, true),
                Some(Zvuk::Pauza),
                "{zamek}"
            );
            assert_eq!(z.probudit_za(t + ODKLAD_PLOCHY_MS), None);
            assert_eq!(
                z.obratka(None, t + 2 * ODKLAD_PLOCHY_MS, zamek, true),
                None,
                "jednou"
            );
        }
        // Ctrl+Alt+Del → Zamknout: zamčení až po odkladu tón nevrátí
        // ani nezopakuje.
        let mut z = Znameni::new(Capturing);
        assert_eq!(z.obratka(Some((Paused, plocha)), t, 0, true), None);
        assert_eq!(
            z.obratka(None, t + ODKLAD_PLOCHY_MS, 0, true),
            Some(Zvuk::Pauza)
        );
        assert_eq!(z.obratka(None, t + 5_000, t + 5_000, true), None);
        // Přepnutí plochy mimo hru nic neohlašuje, nic nečeká.
        let mut z = Znameni::new(Paused);
        assert_eq!(z.obratka(Some((Paused, plocha)), t, 0, true), None);
        assert_eq!(z.probudit_za(t), None);
    }

    /// Novější změna během odkladu tón nahradí: uspání hned po přepnutí
    /// plochy zůstane tiché, návrat do hry pípne jen „hra“.
    #[test]
    fn novejsi_zmena_nahradi_odlozeny_ton() {
        use Rezim::*;
        let plocha = Pricina::Vynuceno(ForceReason::DesktopSwitch);
        let t = 1_000_000;
        let mut z = Znameni::new(Capturing);
        assert_eq!(z.obratka(Some((Paused, plocha)), t, 0, true), None);
        assert_eq!(
            z.obratka(Some((Disabled, Pricina::Ovladac)), t + 50, 0, true),
            None
        );
        assert_eq!(z.probudit_za(t + 50), None);
        assert_eq!(z.obratka(None, t + 10 * ODKLAD_PLOCHY_MS, 0, true), None);

        let mut z = Znameni::new(Capturing);
        assert_eq!(z.obratka(Some((Paused, plocha)), t, 0, true), None);
        assert_eq!(
            z.obratka(Some((Capturing, Pricina::Zkratka)), t + 50, 0, true),
            Some(Zvuk::Hra)
        );
        assert_eq!(z.obratka(None, t + 10 * ODKLAD_PLOCHY_MS, 0, true), None);
    }

    /// Engine → HookVystup → vlákno okna za sebou, jako naostro (bez
    /// Tauri a bez zvuku): obratka vlákna okna jen přidá log a `emit`.
    struct Retez {
        e: keypad_core::Engine,
        v: HookVystup,
        seq: u64,
        z: Znameni,
    }

    impl Retez {
        fn new() -> Retez {
            let sloty = std::array::from_fn(|_| {
                std::sync::Arc::new(crate::platform::windows::slot::StavSlot::new().unwrap())
            });
            let budik = std::sync::Arc::new(crate::platform::windows::slot::Budik::new().unwrap());
            let v = HookVystup::new(sloty, budik);
            let (r, _) = v.info_s_pricinou();
            Retez {
                e: keypad_core::Engine::new(keypad_core::Mapping::default()),
                seq: r.seq,
                z: Znameni::new(r.rezim),
                v,
            }
        }

        /// Příkaz enginu v hook vlákně a obrátka vlákna okna po něm.
        fn krok(
            &mut self,
            f: impl FnOnce(&mut keypad_core::Engine) -> keypad_core::Decision,
            ted: u64,
            zamek: u64,
        ) -> Option<Zvuk> {
            let d = f(&mut self.e);
            self.v.rozhodnuti(None, &d, self.e.mode());
            self.obratka(ted, zamek)
        }

        fn obratka(&mut self, ted: u64, zamek: u64) -> Option<Zvuk> {
            let (r, p) = self.v.info_s_pricinou();
            let zmena = (r.seq != self.seq).then_some((r.rezim, p));
            self.seq = r.seq;
            self.z.obratka(zmena, ted, zamek, true)
        }
    }

    /// Win+L za hry celou cestou: přepnutí plochy vynutí Klávesnici
    /// první, zamčení relace (o 5 ms později) už režim nemění a příčinu
    /// nepřepíše — přesto nic nezapípá.
    #[test]
    fn retez_zamceni_po_prepnuti_plochy() {
        use keypad_core::{ForceReason as F, PadId};
        let p1 = PadId::new(0).unwrap();
        let t = 1_000_000;
        let mut r = Retez::new();
        assert_eq!(r.krok(|e| e.enable(p1), t, 0), None);
        assert_eq!(r.krok(|e| e.capture(t), t, 0), None, "první zapnutí");
        assert_eq!(r.krok(|e| e.reset_held(F::DesktopSwitch), t + 100, 0), None);
        let zamek = t + 105;
        assert_eq!(r.krok(|e| e.reset_held(F::SessionLock), zamek, zamek), None);
        assert_eq!(
            r.v.info_s_pricinou().1,
            Pricina::Vynuceno(F::DesktopSwitch),
            "zamčení příčinu nepřepíše"
        );
        assert_eq!(r.obratka(t + 100 + ODKLAD_PLOCHY_MS, zamek), None);
        // Odemčení přepne plochu zpět (režim beze změny), hru vrátí zkratka.
        assert_eq!(
            r.krok(|e| e.reset_held(F::DesktopSwitch), t + 60_000, zamek),
            None
        );
        assert_eq!(
            r.krok(|e| e.toggle(t + 61_000), t + 61_000, zamek),
            Some(Zvuk::Hra)
        );
        // Výzva UAC o hodinu později už pípne (po odkladu).
        let pozde = t + 3_600_000;
        assert_eq!(
            r.krok(|e| e.reset_held(F::DesktopSwitch), pozde, zamek),
            None
        );
        assert_eq!(
            r.obratka(pozde + ODKLAD_PLOCHY_MS, zamek),
            Some(Zvuk::Pauza)
        );
    }

    /// Vypnutí ovladače přepínačem nepípá, ani když jiný ovladač visí
    /// v chybě; výpadek posledního ovladače chybou pípne.
    #[test]
    fn retez_vypnuti_prepinacem_s_chybou_jineho() {
        use keypad_core::{DisabledReason as D, PadId};
        let (p1, p2) = (PadId::new(0).unwrap(), PadId::new(1).unwrap());
        let mut r = Retez::new();
        r.krok(|e| e.enable(p1), 0, 0);
        r.krok(|e| e.enable(p2), 0, 0);
        assert_eq!(r.krok(|e| e.capture(0), 0, 0), None);
        assert_eq!(
            r.krok(|e| e.disable(p2, D::PadError), 10, 0),
            None,
            "OQ 45(b)"
        );
        assert_eq!(r.krok(|e| e.disable(p1, D::PadNotConnected), 20, 0), None);
        assert_eq!(r.v.info().rezim, Rezim::Disabled);

        let mut r = Retez::new();
        r.krok(|e| e.enable(p1), 0, 0);
        r.krok(|e| e.capture(0), 0, 0);
        assert_eq!(
            r.krok(|e| e.disable(p1, D::PadError), 10, 0),
            Some(Zvuk::Pauza)
        );
    }

    const STAVY: [Option<PadStav>; 7] = [
        None,
        Some(PadStav::Off),
        Some(PadStav::Connecting),
        Some(PadStav::On),
        Some(PadStav::BusMissing),
        Some(PadStav::BusNotRunning),
        Some(PadStav::Error),
    ];

    /// Všechny režimy × stavy padů: bublina se vejde, začíná jménem
    /// aplikace a položka Pozastavit/Pokračovat je aktivní jen při hře
    /// a pauze.
    #[test]
    fn stav_oblasti_vsechny_kombinace() {
        for rezim in REZIMY {
            for a in STAVY {
                for b in STAVY {
                    for c in [None, Some(PadStav::On)] {
                        let pady = [a, b, None, c];
                        let s = stav_oblasti(rezim, &pady, "Scroll Lock");
                        assert!(s.popisek.encode_utf16().count() < 128, "{s:?}");
                        assert!(s.popisek.starts_with("KeyPad — "), "{s:?}");
                        let ocekavana_pauza = match rezim {
                            Rezim::Capturing => Some(false),
                            Rezim::Paused => Some(true),
                            _ => None,
                        };
                        assert_eq!(s.pauza, ocekavana_pauza, "{rezim:?}");
                        let zapnuty = pady.contains(&Some(PadStav::On));
                        let chyba = pady.contains(&Some(PadStav::Error));
                        let ikona = match rezim {
                            Rezim::Capturing => DruhIkony::Hraje,
                            Rezim::Paused => DruhIkony::Pauza,
                            Rezim::Binding if zapnuty => DruhIkony::Pauza,
                            Rezim::NoHook => DruhIkony::Pozor,
                            Rezim::Disabled | Rezim::Binding if chyba => DruhIkony::Pozor,
                            Rezim::Disabled | Rezim::Binding => DruhIkony::Vypnuto,
                        };
                        assert_eq!(s.ikona, ikona, "{rezim:?} {pady:?}");
                        if rezim == Rezim::Binding {
                            assert!(!s.popisek.contains("Scroll Lock"), "{s:?}");
                        }
                    }
                }
            }
        }
    }

    /// Přiřazování ukazuje stav, do kterého se vrátí (bez ovladače
    /// „vypnuto“, ne pauzu), a neradí zkratku — ta při přiřazování nic
    /// nepřepne.
    #[test]
    fn prirazovani_v_oblasti() {
        let s = stav_oblasti(Rezim::Binding, &[None; MAX_PADS], "Scroll Lock");
        assert_eq!(s.ikona, DruhIkony::Vypnuto);
        assert_eq!(s.popisek, "KeyPad — přiřazuji klávesu");
        assert_eq!(s.pauza, None);
        let vypnute = [Some(PadStav::Off), None, None, None];
        let s = stav_oblasti(Rezim::Binding, &vypnute, "Scroll Lock");
        assert_eq!(s.ikona, DruhIkony::Vypnuto);
        let hraje = [Some(PadStav::On), Some(PadStav::Off), None, None];
        let s = stav_oblasti(Rezim::Binding, &hraje, "Scroll Lock");
        assert_eq!(
            (s.ikona, s.popisek.as_str(), s.pauza),
            (DruhIkony::Pauza, "KeyPad — přiřazuji klávesu", None)
        );
    }

    #[test]
    fn stav_oblasti_texty() {
        let jeden = [Some(PadStav::On), None, None, None];
        let dva = [
            Some(PadStav::On),
            Some(PadStav::Off),
            Some(PadStav::On),
            None,
        ];
        let s = stav_oblasti(Rezim::Capturing, &jeden, "Scroll Lock");
        assert_eq!(s.popisek, "KeyPad — hraje · Scroll Lock pozastaví");
        let s = stav_oblasti(Rezim::Capturing, &dva, "Scroll Lock");
        assert_eq!(
            s.popisek,
            "KeyPad — hraje · Scroll Lock pozastaví (2 ovladače)"
        );
        let s = stav_oblasti(Rezim::Paused, &dva, "Pause");
        assert_eq!(s.popisek, "KeyPad — pozastaveno · Pause pokračuje");
        assert_eq!(
            stav_oblasti(Rezim::NoHook, &jeden, "x").popisek,
            "KeyPad — klávesy nejdou"
        );
        let vypnuto = |pady: [Option<PadStav>; MAX_PADS]| {
            stav_oblasti(Rezim::Disabled, &pady, "Scroll Lock").popisek
        };
        assert_eq!(vypnuto([None; MAX_PADS]), "KeyPad — vypnuto");
        assert_eq!(
            vypnuto([Some(PadStav::Off), None, None, None]),
            "KeyPad — vypnuto"
        );
        assert_eq!(
            vypnuto([Some(PadStav::BusMissing), None, None, None]),
            "KeyPad — chybí ViGEmBus"
        );
        assert_eq!(
            vypnuto([Some(PadStav::BusNotRunning), None, None, None]),
            "KeyPad — ViGEmBus neběží"
        );
        assert_eq!(
            vypnuto([Some(PadStav::BusMissing), Some(PadStav::Error), None, None]),
            "KeyPad — chyba ovladače",
            "chyba přebíjí"
        );
    }

    /// Dlouhý název zkratky bublinu nepřeteče (utne se na hranici znaku).
    #[test]
    fn dlouha_zkratka_se_utne() {
        let zkratka = "Ž".repeat(200);
        let pady = [Some(PadStav::On); MAX_PADS];
        let s = stav_oblasti(Rezim::Capturing, &pady, &zkratka);
        assert_eq!(s.popisek.encode_utf16().count(), MAX_BUBLINA);
        assert!(s.popisek.starts_with("KeyPad — hraje · ŽŽ"));
        // Náhradní dvojice UTF-16 se nerozetne.
        assert_eq!(utni("a😀".into(), 2), "a");
        assert_eq!(utni("a😀".into(), 3), "a😀");
    }
}

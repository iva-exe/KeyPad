//! Smlouva s oknem (Fáze 6): tvar odpovědí příkazů a obsahu událostí
//! `klavesy`, `zive`, `oznameni` (a `rezim` z `vystup`).
//!
//! Tvar pevně drží zlaté soubory `ui/src/lib/testdata/*.json`: testy
//! tady serializují vzorové hodnoty a porovnají je s nimi, okno je
//! ověřuje ve `vstupy.test.ts`. Rozjet se tak nemůže ani jedna strana
//! potichu — změna smlouvy znamená změnit zlatý soubor i okno.
//!
//! Čisté funkce: názvy kláves dodá volající (na hlavním vlákně podle
//! rozložení okna), takže se tu nic nevolá do Windows a testy nezávisí
//! na rozložení počítače.
//!
//! Soukromí (spec 1.5): okno nikdy nedostane identitu stisknuté klávesy
//! mimo přiřazování — živý stav nese jen bity vstupů, oznámení jen
//! klávesu, kterou uživatel při přiřazování sám stiskl.

use std::path::Path;

use keypad_core::{
    Action, ActionSet, BindingCancel, BindingReject, KeyId, LiveInputs, Mapping, ToggleReject,
    UiEvent, MAX_PADS,
};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};

use crate::config::StavKonfigurace;
use crate::platform::windows::klavesy;
use crate::platform::windows::vystup::CilInfo;

/// Krátký název na čepičku má nejvýš tolik znaků (spec 1.3), celý je
/// v bublině.
const MAX_KRATKY: usize = 5;

/// Chyb nevalidního config.json v bublině pruhu nejvýš tolik (spec 1.7)
/// — bublina má zůstat krátká, celý výčet je v logu.
pub const MAX_CHYB_V_OKNE: usize = 3;

/// Klávesa s názvy podle rozložení klávesnice (scan kód = pozice).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KlavesaInfo {
    pub scan: u16,
    pub e0: bool,
    /// Celý název (`GetKeyNameTextW`) — do bubliny.
    pub nazev: String,
    /// Na čepičku: symbol z tabulky ([`klavesy::kratky`]), jinak název
    /// utnutý na 5 znaků.
    pub kratky: String,
}

impl KlavesaInfo {
    pub fn z(k: KeyId, nazev: String) -> KlavesaInfo {
        let kratky = klavesy::kratky(k)
            .map_or_else(|| nazev.chars().take(MAX_KRATKY).collect(), String::from);
        KlavesaInfo {
            scan: k.scan,
            e0: k.extended,
            nazev,
            kratky,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VazbaInfo {
    /// Ovladač od 0 (jako `pad-stav`); od 1 počítá jen config.json.
    pub pad: u8,
    pub vstup: &'static str,
    pub klavesa: KlavesaInfo,
}

/// Odpověď příkazu `klavesy`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KlavesyInfo {
    /// Revize mapování — roste s každou skutečnou změnou.
    pub rev: u64,
    /// Pořadí bitů živého stavu (`Action::ALL`).
    pub vstupy: [&'static str; Action::COUNT],
    /// Zkratka pauzy.
    pub zkratka: KlavesaInfo,
    /// Všechny vazby v pořadí `Mapping::bindings()` (běžné klávesy podle
    /// scan kódu, pak E0) — okno z pořadí bere, která klávesa vstupu je
    /// na čepičce první.
    pub vazby: Vec<VazbaInfo>,
    /// Poslední změnu jde vrátit (zrcadlo zná jejího přesného
    /// předchůdce).
    pub zpet: bool,
    pub konfigurace: StavKonfigurace,
    /// Kam se odložil nevalidní config.json — jen u `obnovena`.
    pub zaloha: Option<String>,
    /// Co je v nevalidním config.json špatně (věty pro bublinu pruhu,
    /// nejvýš [`MAX_CHYB_V_OKNE`]) — jen u `obnovena` a `necitelna`
    /// (nevalidní soubor, který nešel odložit); jinak prázdné.
    pub chyby: Vec<String>,
}

/// Kódy vstupů v pořadí bitů živého stavu.
pub fn vstupy() -> [&'static str; Action::COUNT] {
    Action::ALL.map(Action::code)
}

/// [`KlavesyInfo`] ze zrcadla mapování; `nazev` = název klávesy podle
/// rozložení (`klavesy::nazev` na hlavním vlákně).
pub fn klavesy_info(
    rev: u64,
    m: &Mapping,
    zpet: bool,
    konfigurace: StavKonfigurace,
    zaloha: Option<&Path>,
    chyby: &[String],
    nazev: impl Fn(KeyId) -> String,
) -> KlavesyInfo {
    let klavesa = |k: KeyId| KlavesaInfo::z(k, nazev(k));
    KlavesyInfo {
        rev,
        vstupy: vstupy(),
        zkratka: klavesa(m.toggle_key()),
        vazby: m
            .bindings()
            .map(|(k, t)| VazbaInfo {
                pad: t.pad.index() as u8,
                vstup: t.action.code(),
                klavesa: klavesa(k),
            })
            .collect(),
        zpet,
        konfigurace,
        // Po prvním uložení soubor zase platí a cesta zálohy by v okně
        // jen strašila.
        zaloha: zaloha
            .filter(|_| konfigurace == StavKonfigurace::Obnovena)
            .map(|p| p.display().to_string()),
        // Chyby patří k souboru, který neplatí: po prvním uložení (`ok`)
        // už nic neříkají a u souboru z novější verze žádné nejsou.
        chyby: if matches!(
            konfigurace,
            StavKonfigurace::Obnovena | StavKonfigurace::Necitelna
        ) {
            chyby.iter().take(MAX_CHYB_V_OKNE).cloned().collect()
        } else {
            Vec::new()
        },
    }
}

/// Živý stav jednoho ovladače.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ZivyPad {
    /// Fyzicky držené vstupy (bity podle [`vstupy`]), i poražený směr
    /// SOCD a klávesy pozastavené hry.
    pub drzi: u32,
    /// Vstupy, které hra opravdu dostává (stav ve slotu padu).
    pub hra: u32,
    /// Výchylka levé páčky po SOCD: x, y ∈ {−1, 0, 1}, +y = nahoru.
    pub l: [i8; 2],
    pub p: [i8; 2],
}

impl ZivyPad {
    pub fn z(drzi: LiveInputs, hra: ActionSet) -> ZivyPad {
        let (lx, ly) = drzi.left_stick();
        let (px, py) = drzi.right_stick();
        ZivyPad {
            drzi: drzi.held().bits(),
            hra: hra.bits(),
            l: [lx, ly],
            p: [px, py],
        }
    }
}

/// Živý stav všech ovladačů.
pub fn zive_pady(drzi: [LiveInputs; MAX_PADS], hra: [ActionSet; MAX_PADS]) -> [ZivyPad; MAX_PADS] {
    std::array::from_fn(|i| ZivyPad::z(drzi[i], hra[i]))
}

/// Odpověď příkazu `zive` i obsah události `zive`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ZivaInfo {
    /// Pořadí snímku — starší (odpověď po novější události) okno zahodí.
    pub seq: u64,
    pub pady: [ZivyPad; MAX_PADS],
}

/// Klávesa v oznámení — jen pozice; názvy dodá příkaz `klavesy`.
#[derive(Serialize)]
struct KlavesaOznameni {
    scan: u16,
    e0: bool,
}

impl KlavesaOznameni {
    fn z(k: KeyId) -> KlavesaOznameni {
        KlavesaOznameni {
            scan: k.scan,
            e0: k.extended,
        }
    }
}

/// Událost `oznameni` — co se stalo při přiřazování a proč.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Oznameni {
    /// Pořadí ze schránky hooku (16 bitů, přetéká).
    pub seq: u16,
    /// Nějaké oznámení se mezitím ztratilo (přepsala ho novější
    /// schránka) — okno si načte stav znovu.
    pub mezera: bool,
    udalost: UiEvent,
}

impl Oznameni {
    /// `None` = událost, kterou okno nedostává: změnu režimu nese
    /// událost `rezim` a odmítnuté přepnutí během přiřazování okno
    /// nevyvolalo (přepíná jen nabídka ikony, ta je při přiřazování
    /// zakázaná).
    pub fn nove(seq: u16, mezera: bool, udalost: UiEvent) -> Option<Oznameni> {
        match udalost {
            UiEvent::ModeChanged { .. }
            | UiEvent::ToggleRejected {
                reason: ToggleReject::Binding,
            } => None,
            _ => Some(Oznameni {
                seq,
                mezera,
                udalost,
            }),
        }
    }
}

impl Serialize for Oznameni {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("seq", &self.seq)?;
        m.serialize_entry("mezera", &self.mezera)?;
        match self.udalost {
            UiEvent::BindingSaved {
                key,
                target,
                moved_from,
            } => {
                m.serialize_entry("typ", "ulozeno")?;
                m.serialize_entry("pad", &(target.pad.index() as u8))?;
                m.serialize_entry("vstup", target.action.code())?;
                m.serialize_entry("klavesa", &KlavesaOznameni::z(key))?;
                m.serialize_entry("odkud", &moved_from.map(CilInfo::z))?;
            }
            UiEvent::BindingRejected { key, reason } => {
                m.serialize_entry("typ", "odmitnuto")?;
                let duvod = match reason {
                    BindingReject::ToggleKey => "zkratka",
                    BindingReject::Reserved => "win",
                    BindingReject::Unmappable => "nejde",
                };
                m.serialize_entry("duvod", duvod)?;
                m.serialize_entry("klavesa", &KlavesaOznameni::z(key))?;
            }
            UiEvent::BindingCancelled { reason } => {
                m.serialize_entry("typ", "zruseno")?;
                let duvod = match reason {
                    BindingCancel::Escape => "esc",
                    BindingCancel::Timeout => "cas",
                    // Klik jinam, Esc v okně, ztráta popředí, schované okno.
                    BindingCancel::Gui => "okno",
                    BindingCancel::Forced(_) | BindingCancel::PadStatus => "vynuceno",
                };
                m.serialize_entry("duvod", duvod)?;
            }
            // Scroll Lock bez zapnutého ovladače (OQ 48).
            UiEvent::ToggleRejected { .. } | UiEvent::ModeChanged { .. } => {
                m.serialize_entry("typ", "zapni_ovladac")?;
            }
        }
        m.end()
    }
}

/// Argument `zmena` příkazu `uprav_klavesy`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "typ", rename_all = "snake_case")]
pub enum ZmenaOkna {
    /// `×` nebo pravý klik na čepičku.
    Vyprazdnit { pad: u8, vstup: String },
    /// ↺ Výchozí klávesy (ovladač 1).
    Vychozi,
    /// „Zpět" — `rev` je revize, kterou okno vidí.
    Zpet { rev: u64 },
}

/// Obsah události `klavesy-zmena` — jen revize, názvy si okno načte
/// příkazem `klavesy` (správné rozložení).
#[derive(Clone, Copy, Debug, Serialize)]
pub struct RevizeInfo {
    pub rev: u64,
}

/// Obsah události `konfigurace`.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct KonfiguraceInfo {
    pub stav: StavKonfigurace,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::windows::vystup::{Rezim, RezimInfo};
    use keypad_core::{
        DisabledReason, ForceReason, PadAction, PadButton, PadId, PadState, StickDir, AXIS_MAX,
        TRIGGER_MAX,
    };
    use serde_json::Value;

    fn zlaty(soubor: &str) -> Value {
        let cesta = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ui/src/lib/testdata")
            .join(soubor);
        let text =
            std::fs::read_to_string(&cesta).unwrap_or_else(|e| panic!("{}: {e}", cesta.display()));
        serde_json::from_str(&text).unwrap()
    }

    /// Názvy z českého rozložení, jak je vrací `GetKeyNameTextW` —
    /// pevně, ať test nezávisí na rozložení počítače.
    fn nazev_cz(k: KeyId) -> String {
        let n = match (k.scan, k.extended) {
            (0x02, false) => "+",
            (0x04, false) => "š",
            (0x0E, false) => "Backspace",
            (0x10, false) => "Q",
            (0x11, false) => "W",
            (0x12, false) => "E",
            (0x13, false) => "R",
            (0x17, false) => "I",
            (0x1C, false) => "Enter",
            (0x1E, false) => "A",
            (0x1F, false) => "S",
            (0x20, false) => "D",
            (0x21, false) => "F",
            (0x24, false) => "J",
            (0x25, false) => "K",
            (0x26, false) => "L",
            (0x2A, false) => "Shift",
            (0x2E, false) => "C",
            (0x2F, false) => "V",
            (0x38, false) => "Alt",
            (0x39, false) => "Mezerník",
            (0x46, false) => "Scroll Lock",
            (0x48, false) => "Num 8",
            (0x4B, false) => "Num 4",
            (0x4C, false) => "Num 5",
            (0x4D, false) => "Num 6",
            (0x52, false) => "Num 0",
            (0x48, true) => "Šipka nahoru",
            (0x4B, true) => "Šipka vlevo",
            (0x4D, true) => "Šipka vpravo",
            (0x50, true) => "Šipka dolů",
            _ => panic!("název pro {k:?} test nezná"),
        };
        n.into()
    }

    fn na(pad: usize, a: Action) -> PadAction {
        PadAction::new(PadId::new(pad).unwrap(), a)
    }

    /// Mapování zlatého souboru: výchozí ovladač 1 + levý Alt na LB
    /// (druhá klávesa vstupu a jantarová tečka) a numerická klávesnice
    /// na ovladači 2.
    fn mapovani_zlateho() -> Mapping {
        let mut m = Mapping::default();
        m.bind(KeyId::LEFT_ALT, na(0, Action::Button(PadButton::Lb)))
            .unwrap();
        for (scan, a) in [
            (0x48, Action::LeftStick(StickDir::Up)),
            (0x4B, Action::LeftStick(StickDir::Left)),
            (0x4C, Action::LeftStick(StickDir::Down)),
            (0x4D, Action::LeftStick(StickDir::Right)),
            (0x52, Action::Button(PadButton::A)),
        ] {
            m.bind(KeyId::new(scan), na(1, a)).unwrap();
        }
        m
    }

    /// `klavesy`: celý tvar včetně pořadí vazeb (`Mapping::bindings`)
    /// a krátkých názvů (tabulka i utnutí na 5 znaků).
    #[test]
    fn zlaty_klavesy() {
        let info = klavesy_info(
            12,
            &mapovani_zlateho(),
            true,
            StavKonfigurace::Ok,
            Some(Path::new(r"C:\x\config.invalid.json")),
            &["řádek 1: x".to_string()],
            nazev_cz,
        );
        assert_eq!(serde_json::to_value(&info).unwrap(), zlaty("klavesy.json"));
    }

    /// Pořadí bitů živého stavu = `Action::ALL` = zlatý soubor.
    #[test]
    fn vstupy_v_poradi_bitu() {
        let v = vstupy();
        assert_eq!(v.to_vec(), Action::ALL.map(Action::code).to_vec());
        assert_eq!(
            serde_json::to_value(v).unwrap(),
            zlaty("klavesy.json")["vstupy"]
        );
        for (i, kod) in v.iter().enumerate() {
            assert_eq!(Action::from_code(kod).map(Action::index), Some(i));
        }
    }

    /// Cesta zálohy jen u obnovené konfigurace.
    #[test]
    fn zaloha_jen_u_obnovene() {
        let m = Mapping::default();
        let p = Path::new(r"C:\Users\hrac\AppData\Roaming\KeyPad\config.invalid.json");
        for stav in [
            StavKonfigurace::Ok,
            StavKonfigurace::Obnovena,
            StavKonfigurace::Novejsi,
            StavKonfigurace::Necitelna,
            StavKonfigurace::Neulozena,
        ] {
            let i = klavesy_info(0, &m, false, stav, Some(p), &[], nazev_cz);
            let ocekavano = (stav == StavKonfigurace::Obnovena).then(|| p.display().to_string());
            assert_eq!(i.zaloha, ocekavano, "{stav:?}");
        }
        assert_eq!(
            klavesy_info(0, &m, false, StavKonfigurace::Obnovena, None, &[], nazev_cz).zaloha,
            None
        );
    }

    /// Chyby nevalidního souboru (spec 1.7: v bublině i v logu): jen
    /// u souboru, který neplatí, a nejvýš [`MAX_CHYB_V_OKNE`].
    #[test]
    fn chyby_jen_u_neplatneho_souboru() {
        let m = Mapping::default();
        let chyby: Vec<String> = (1..=5).map(|i| format!("vazba č. {i}: chyba")).collect();
        for stav in [
            StavKonfigurace::Ok,
            StavKonfigurace::Obnovena,
            StavKonfigurace::Novejsi,
            StavKonfigurace::Necitelna,
            StavKonfigurace::Neulozena,
        ] {
            let i = klavesy_info(0, &m, false, stav, None, &chyby, nazev_cz);
            let ocekavano =
                if matches!(stav, StavKonfigurace::Obnovena | StavKonfigurace::Necitelna) {
                    chyby[..MAX_CHYB_V_OKNE].to_vec()
                } else {
                    Vec::new()
                };
            assert_eq!(i.chyby, ocekavano, "{stav:?}");
        }
        let jedna = klavesy_info(
            0,
            &m,
            false,
            StavKonfigurace::Obnovena,
            None,
            &chyby[..1],
            nazev_cz,
        );
        assert_eq!(jedna.chyby, chyby[..1].to_vec());
    }

    /// Krátký název: tabulka, jinak prvních 5 znaků (ne bajtů).
    #[test]
    fn kratky_nazev() {
        let k = |scan, e0, n: &str| KlavesaInfo::z(KeyId { scan, extended: e0 }, n.into()).kratky;
        assert_eq!(k(0x46, false, "Scroll Lock"), "Scrol");
        assert_eq!(k(0x48, false, "Num 8"), "Num 8");
        assert_eq!(k(0x39, false, "Mezerník"), "␣");
        assert_eq!(k(0x48, true, "Šipka nahoru"), "↑");
        assert_eq!(k(0x29, false, "ů"), "ů");
        assert_eq!(k(0x1D, false, "Ctrl"), "Ctrl");
        assert_eq!(k(0x53, true, "Odstranit"), "Odstr");
        assert_eq!(k(0x1A, false, "ěščřžý"), "ěščřž");
    }

    /// `rezim`: přiřazuje se A ovladače 2.
    #[test]
    fn zlaty_rezim() {
        let r = RezimInfo {
            rezim: Rezim::Binding,
            seq: 42,
            cil: Some(CilInfo { pad: 1, vstup: "a" }),
            hook_chyba: false,
        };
        assert_eq!(serde_json::to_value(r).unwrap(), zlaty("rezim.json"));
    }

    /// `zive`: A+D (svítí obě, hra dostane D), šipka nahoru, mezerník (A)
    /// a RT na ovladači 1; na ovladači 2 drží W pozastavená hra (svítí, hra nic).
    #[test]
    fn zlaty_zive() {
        let set = |a: &[Action]| {
            let mut s = ActionSet::EMPTY;
            for &x in a {
                s.insert(x);
            }
            s
        };
        let drzi = [
            LiveInputs::new(
                set(&[
                    Action::LeftStick(StickDir::Left),
                    Action::LeftStick(StickDir::Right),
                    Action::RightStick(StickDir::Up),
                    Action::Button(PadButton::A),
                    Action::RightTrigger,
                ]),
                (1, 0),
                (0, 1),
            ),
            LiveInputs::new(set(&[Action::LeftStick(StickDir::Up)]), (0, 1), (0, 0)),
            LiveInputs::EMPTY,
            LiveInputs::EMPTY,
        ];
        let hra = [
            PadState {
                thumb_lx: AXIS_MAX,
                thumb_ry: AXIS_MAX,
                buttons: PadButton::A.mask(),
                right_trigger: TRIGGER_MAX,
                ..PadState::NEUTRAL
            }
            .active_inputs(),
            ActionSet::EMPTY,
            ActionSet::EMPTY,
            ActionSet::EMPTY,
        ];
        let z = ZivaInfo {
            seq: 1234,
            pady: zive_pady(drzi, hra),
        };
        assert_eq!(serde_json::to_value(z).unwrap(), zlaty("zive.json"));
    }

    /// `oznameni`: všechny druhy a důvody, mezera, pořadí na hranici
    /// 16 bitů.
    #[test]
    fn zlaty_oznameni() {
        let o = |seq, mezera, u| Oznameni::nove(seq, mezera, u).unwrap();
        let zruseno = |r| UiEvent::BindingCancelled { reason: r };
        let vse = vec![
            o(
                101,
                false,
                UiEvent::BindingSaved {
                    key: KeyId::F,
                    target: na(0, Action::Button(PadButton::X)),
                    moved_from: None,
                },
            ),
            o(
                102,
                false,
                UiEvent::BindingSaved {
                    key: KeyId::W,
                    target: na(1, Action::LeftStick(StickDir::Up)),
                    moved_from: Some(na(0, Action::LeftStick(StickDir::Up))),
                },
            ),
            o(
                103,
                true,
                UiEvent::BindingRejected {
                    key: KeyId::LEFT_WIN,
                    reason: BindingReject::Reserved,
                },
            ),
            o(
                104,
                false,
                UiEvent::BindingRejected {
                    key: KeyId::SCROLL_LOCK,
                    reason: BindingReject::ToggleKey,
                },
            ),
            o(
                105,
                false,
                UiEvent::BindingRejected {
                    key: KeyId::RIGHT_ALT,
                    reason: BindingReject::Unmappable,
                },
            ),
            o(106, false, zruseno(BindingCancel::Escape)),
            o(107, false, zruseno(BindingCancel::Timeout)),
            o(108, false, zruseno(BindingCancel::Gui)),
            o(
                109,
                false,
                zruseno(BindingCancel::Forced(ForceReason::HookReinstalled)),
            ),
            o(
                65535,
                false,
                UiEvent::ToggleRejected {
                    reason: ToggleReject::Disabled(DisabledReason::PadNotConnected),
                },
            ),
        ];
        assert_eq!(serde_json::to_value(vse).unwrap(), zlaty("oznameni.json"));
    }

    /// Každé vynucení a výpadek ovladače je pro okno „vynuceno", každé
    /// odmítnuté přepnutí bez ovladače „zapni ovladač"; změna režimu
    /// a odmítnutí při přiřazování oknu nejdou.
    #[test]
    fn oznameni_vsechny_duvody() {
        let typ = |u| serde_json::to_value(Oznameni::nove(1, false, u).unwrap()).unwrap();
        for r in ForceReason::ALL {
            let v = typ(UiEvent::BindingCancelled {
                reason: BindingCancel::Forced(r),
            });
            assert_eq!(v["duvod"], "vynuceno", "{r:?}");
        }
        assert_eq!(
            typ(UiEvent::BindingCancelled {
                reason: BindingCancel::PadStatus
            })["duvod"],
            "vynuceno"
        );
        for r in [
            DisabledReason::ViGEmMissing,
            DisabledReason::PadNotConnected,
            DisabledReason::PadError,
        ] {
            let v = typ(UiEvent::ToggleRejected {
                reason: ToggleReject::Disabled(r),
            });
            assert_eq!(v["typ"], "zapni_ovladac", "{r:?}");
        }
        assert!(Oznameni::nove(
            1,
            false,
            UiEvent::ToggleRejected {
                reason: ToggleReject::Binding
            }
        )
        .is_none());
        assert!(Oznameni::nove(
            1,
            false,
            UiEvent::ModeChanged {
                mode: keypad_core::Mode::Keyboard,
                cause: keypad_core::ModeCause::Hotkey,
            }
        )
        .is_none());
    }

    /// Argument `uprav_klavesy` tak, jak ho posílá okno (smlouva.ts).
    #[test]
    fn zmena_z_okna() {
        let z = |t: &str| serde_json::from_str::<ZmenaOkna>(t);
        assert_eq!(
            z(r#"{"typ":"vyprazdnit","pad":2,"vstup":"lb"}"#).unwrap(),
            ZmenaOkna::Vyprazdnit {
                pad: 2,
                vstup: "lb".into()
            }
        );
        assert_eq!(z(r#"{"typ":"vychozi"}"#).unwrap(), ZmenaOkna::Vychozi);
        assert_eq!(
            z(r#"{"typ":"zpet","rev":13}"#).unwrap(),
            ZmenaOkna::Zpet { rev: 13 }
        );
        assert!(z(r#"{"typ":"smaz_vse"}"#).is_err());
        assert!(z(r#"{"typ":"zpet"}"#).is_err());
        assert!(z(r#"{"typ":"vyprazdnit","pad":-1,"vstup":"a"}"#).is_err());
    }

    /// Události s revizí a stavem konfigurace.
    #[test]
    fn male_udalosti() {
        assert_eq!(
            serde_json::to_value(RevizeInfo { rev: 7 }).unwrap(),
            serde_json::json!({ "rev": 7 })
        );
        assert_eq!(
            serde_json::to_value(KonfiguraceInfo {
                stav: StavKonfigurace::Neulozena
            })
            .unwrap(),
            serde_json::json!({ "stav": "neulozena" })
        );
    }
}

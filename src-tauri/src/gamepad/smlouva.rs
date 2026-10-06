//! Smlouva s oknem (Fáze 6, 7): tvar odpovědí příkazů a obsahu událostí
//! `klavesy`, `zive`, `oznameni`, `nastaveni` (a `rezim` z `vystup`).
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
    Action, ActionSet, BindTarget, BindingCancel, BindingReject, KeyId, LiveInputs, Mapping,
    ToggleReject, UiEvent, MAX_PADS,
};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};

use crate::config::{Karty, Rozbalene, StavKonfigurace};
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
    /// Zkratka není F1–F24 (bez F4), Scroll Lock ani Pause (ručně zapsaná
    /// v config.json, OQ 69) — platí, ale Windows ji nedostanou, dokud je
    /// zapnutý ovladač; okno ji označí jantarovou tečkou.
    pub zkratka_mimo: bool,
    /// Všechny vazby v pořadí `Mapping::bindings()` (běžné klávesy podle
    /// scan kódu, pak E0; u sdílené klávesy — Fáze 7 — tatáž klávesa
    /// víckrát, každý cíl jednou, podle ovladače a `Action::index`). Okno
    /// z pořadí bere, která klávesa vstupu je na čepičce první, a sdílenost
    /// si spočítá samo.
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
    /// Ovladače s kartou v okně (uložené, OQ 52) a které jsou rozbalené.
    pub karty: KartyInfo,
    /// Volby z panelu ⓘ (Fáze 7).
    pub nastaveni: NastaveniInfo,
}

/// Karty ovladačů v okně (OQ 52, rozhodl vlastník 6. 10.: ukládají se;
/// Fáze 7: i jejich rozbalení, OQ 70). Část odpovědi `klavesy` a odpověď
/// příkazů `pridej_kartu`, `odeber_ovladac` a `rozbal_kartu`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KartyInfo {
    /// Pořadí změny seznamu — odpověď příkazu a načtení kláves se můžou
    /// předběhnout, starší seznam okno zahodí.
    pub rev: u64,
    /// Ovladače od 0, vzestupně; ovladač 0 vždy a ovladač s klávesami
    /// taky. Zapnutý ovladač okno ukáže i bez karty (nesmí zmizet).
    pub pady: Vec<u8>,
    /// Rozbalené karty — ovladače od 0, vzestupně, jen z `pady`.
    /// Zapnutý ovladač bez uložené karty se ukáže sbalený.
    pub rozbalene: Vec<u8>,
}

impl KartyInfo {
    pub fn z(rev: u64, karty: Karty, rozbalene: Rozbalene) -> KartyInfo {
        KartyInfo {
            rev,
            pady: karty.pady().map(|p| p.index() as u8).collect(),
            rozbalene: rozbalene
                .jen(karty)
                .pady()
                .map(|p| p.index() as u8)
                .collect(),
        }
    }
}

/// Volby z panelu ⓘ (Fáze 7 Z6): odpověď příkazů `nastaveni` a `nastav`,
/// obsah události `nastaveni` a část odpovědi `klavesy`. Zdrojem pravdy
/// je backend — „✓ Zvuk“ v nabídce ikony a v okně ukazuje vždy totéž.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct NastaveniInfo {
    /// Pořadí změny — starší odpověď (po novější události) okno zahodí.
    pub rev: u64,
    /// Zvuk pozastavení a pokračování.
    pub zvuk: bool,
    /// Jedna klávesa pro víc vstupů: přiřazení klávesy, která už patří
    /// jinam, ji sdílí místo přesunu (backend to přidá ke každému `prirad`).
    pub sdilene_klavesy: bool,
}

/// Argument `volba` příkazu `nastav`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Volba {
    Zvuk,
    SdileneKlavesy,
}

/// Kódy vstupů v pořadí bitů živého stavu.
pub fn vstupy() -> [&'static str; Action::COUNT] {
    Action::ALL.map(Action::code)
}

/// [`KlavesyInfo`] ze zrcadla mapování; `nazev` = název klávesy podle
/// rozložení (`klavesy::nazev` na hlavním vlákně).
#[allow(
    clippy::too_many_arguments,
    reason = "čistá funkce ze zrcadla; struktura navíc by jen opakovala KlavesyInfo"
)]
pub fn klavesy_info(
    rev: u64,
    m: &Mapping,
    zpet: bool,
    karty: KartyInfo,
    nastaveni: NastaveniInfo,
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
        zkratka_mimo: !m.toggle_key().is_toggle_candidate(),
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
        karty,
        nastaveni,
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
                moved_more,
                shared,
            } => {
                m.serialize_entry("typ", "ulozeno")?;
                let BindTarget::Input(target) = target else {
                    // Nová zkratka pozastavení (Fáze 7, Z6): bez ovladače a vstupu,
                    // nikomu nic nebere ani nesdílí.
                    m.serialize_entry("zkratka", &true)?;
                    m.serialize_entry("klavesa", &KlavesaOznameni::z(key))?;
                    return m.end();
                };
                m.serialize_entry("pad", &(target.pad.index() as u8))?;
                m.serialize_entry("vstup", target.action.code())?;
                m.serialize_entry("klavesa", &KlavesaOznameni::z(key))?;
                // Odkud klávesa odešla (přesun); u sdílené klávesy první
                // z vstupů, které o ni přišly, a kolik dalších (Fáze 7).
                m.serialize_entry("odkud", &moved_from.map(CilInfo::z))?;
                m.serialize_entry("odkud_dalsi", &moved_more)?;
                // Kolika dalším vstupům klávesa dál patří (sdílená klávesa).
                m.serialize_entry("sdileno", &shared)?;
            }
            UiEvent::BindingRejected { key, reason } => {
                m.serialize_entry("typ", "odmitnuto")?;
                let duvod = match reason {
                    BindingReject::ToggleKey => "zkratka",
                    BindingReject::Reserved => "win",
                    BindingReject::Unmappable => "nejde",
                    // Klávesa už ovládá 4 vstupy (Fáze 7); kam patří, si
                    // okno najde ve `vazby`.
                    BindingReject::TooManyTargets => "plno",
                    // Přiřazování zkratky pozastavení (Fáze 7, Z6): klávesa
                    // ovládá vstup (kam patří, najde okno ve `vazby`), nebo není
                    // F1–F24 (bez F4), Scroll Lock ani Pause.
                    BindingReject::Mapped => "namapovana",
                    BindingReject::NotToggleKey => "nevhodna",
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
    use crate::platform::windows::vystup::{CilRezimu, Rezim, RezimInfo};
    use keypad_core::{
        DisabledReason, ForceReason, KeyConflict, PadAction, PadButton, PadId, PadState, StickDir,
        AXIS_MAX, TRIGGER_MAX,
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
    /// (druhá klávesa vstupu a jantarová tečka), numerická klávesnice
    /// na ovladači 2 a F sdílená s tlačítkem B ovladače 2 (Fáze 7 — tatáž
    /// klávesa ve `vazby` dvakrát, podle ovladače).
    fn mapovani_zlateho() -> Mapping {
        let mut m = Mapping::default();
        m.bind(
            KeyId::LEFT_ALT,
            na(0, Action::Button(PadButton::Lb)),
            KeyConflict::Move,
        )
        .unwrap();
        for (scan, a) in [
            (0x48, Action::LeftStick(StickDir::Up)),
            (0x4B, Action::LeftStick(StickDir::Left)),
            (0x4C, Action::LeftStick(StickDir::Down)),
            (0x4D, Action::LeftStick(StickDir::Right)),
            (0x52, Action::Button(PadButton::A)),
        ] {
            m.bind(KeyId::new(scan), na(1, a), KeyConflict::Move)
                .unwrap();
        }
        m.bind(
            KeyId::F,
            na(1, Action::Button(PadButton::B)),
            KeyConflict::Share,
        )
        .unwrap();
        m
    }

    /// Jen karta ovladače 1, rozbalená.
    fn karty0() -> KartyInfo {
        KartyInfo::z(0, Karty::PRVNI, Rozbalene::PRVNI)
    }

    /// Výchozí volby.
    fn nastaveni0() -> NastaveniInfo {
        NastaveniInfo {
            rev: 0,
            zvuk: true,
            sdilene_klavesy: false,
        }
    }

    /// `klavesy`: celý tvar včetně pořadí vazeb (`Mapping::bindings`, se
    /// sdílenou klávesou), krátkých názvů (tabulka i utnutí na 5 znaků),
    /// karet (ovladač 4 s kartou bez kláves, OQ 52; rozbalené 1 a 4 —
    /// rozbalení ovladače 3 bez karty se neukáže) a voleb.
    #[test]
    fn zlaty_klavesy() {
        let m = mapovani_zlateho();
        let mut karty = Karty::PRVNI.s_klavesami(&m);
        karty.pridej(PadId::new(3).unwrap());
        let mut rozbalene = Rozbalene::PRVNI;
        rozbalene.nastav(PadId::new(2).unwrap(), true);
        rozbalene.nastav(PadId::new(3).unwrap(), true);
        let info = klavesy_info(
            12,
            &m,
            true,
            KartyInfo::z(3, karty, rozbalene),
            NastaveniInfo {
                rev: 2,
                zvuk: true,
                sdilene_klavesy: true,
            },
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
            let i = klavesy_info(
                0,
                &m,
                false,
                karty0(),
                nastaveni0(),
                stav,
                Some(p),
                &[],
                nazev_cz,
            );
            let ocekavano = (stav == StavKonfigurace::Obnovena).then(|| p.display().to_string());
            assert_eq!(i.zaloha, ocekavano, "{stav:?}");
        }
        assert_eq!(
            klavesy_info(
                0,
                &m,
                false,
                karty0(),
                nastaveni0(),
                StavKonfigurace::Obnovena,
                None,
                &[],
                nazev_cz
            )
            .zaloha,
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
            let i = klavesy_info(
                0,
                &m,
                false,
                karty0(),
                nastaveni0(),
                stav,
                None,
                &chyby,
                nazev_cz,
            );
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
            karty0(),
            nastaveni0(),
            StavKonfigurace::Obnovena,
            None,
            &chyby[..1],
            nazev_cz,
        );
        assert_eq!(jedna.chyby, chyby[..1].to_vec());
    }

    /// Karty: ovladače od 0, vzestupně, s pořadím změny; rozbalené jen
    /// z karet, které v okně jsou.
    #[test]
    fn karty_pro_okno() {
        assert_eq!(
            serde_json::to_value(karty0()).unwrap(),
            serde_json::json!({ "rev": 0, "pady": [0], "rozbalene": [0] })
        );
        let mut k = Karty::PRVNI;
        k.pridej(PadId::new(3).unwrap());
        k.pridej(PadId::new(1).unwrap());
        let mut r = Rozbalene::ZADNA;
        r.nastav(PadId::new(2).unwrap(), true);
        r.nastav(PadId::new(3).unwrap(), true);
        let i = KartyInfo::z(5, k, r);
        assert_eq!((i.pady, i.rozbalene), (vec![0, 1, 3], vec![3]));
        assert_eq!(
            KartyInfo::z(1, k, Rozbalene::ZADNA).rozbalene,
            Vec::<u8>::new()
        );
    }

    /// `nastaveni`: tvar volby z ⓘ (zlatý soubor) a argument `nastav`
    /// tak, jak ho posílá okno.
    #[test]
    fn zlaty_nastaveni_a_volba_z_okna() {
        let n = NastaveniInfo {
            rev: 7,
            zvuk: false,
            sdilene_klavesy: true,
        };
        assert_eq!(serde_json::to_value(n).unwrap(), zlaty("nastaveni.json"));
        let v = |t: &str| serde_json::from_str::<Volba>(t);
        assert_eq!(v(r#""zvuk""#).unwrap(), Volba::Zvuk);
        assert_eq!(v(r#""sdilene_klavesy""#).unwrap(), Volba::SdileneKlavesy);
        for spatne in [r#""Zvuk""#, r#""sdilene""#, r#""zkratka""#, "1"] {
            assert!(v(spatne).is_err(), "{spatne}");
        }
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
            cil: Some(CilRezimu::Vstup(CilInfo { pad: 1, vstup: "a" })),
            hook_chyba: false,
        };
        assert_eq!(serde_json::to_value(r).unwrap(), zlaty("rezim.json"));
        // Přiřazuje se zkratka pozastavení (ⓘ → Pauza, Fáze 7 Z6).
        let r = RezimInfo {
            rezim: Rezim::Binding,
            seq: 43,
            cil: Some(CilRezimu::Zkratka),
            hook_chyba: false,
        };
        assert_eq!(
            serde_json::to_value(r).unwrap(),
            zlaty("rezim_zkratka.json")
        );
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
                    target: na(0, Action::Button(PadButton::X)).into(),
                    moved_from: None,
                    moved_more: 0,
                    shared: 0,
                },
            ),
            o(
                102,
                false,
                UiEvent::BindingSaved {
                    key: KeyId::W,
                    target: na(1, Action::LeftStick(StickDir::Up)).into(),
                    moved_from: Some(na(0, Action::LeftStick(StickDir::Up))),
                    moved_more: 0,
                    shared: 0,
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
            // Fáze 7: bez volby se F přesunula ze dvou vstupů (sdílená
            // klávesa) — první a jeden další.
            o(
                110,
                false,
                UiEvent::BindingSaved {
                    key: KeyId::F,
                    target: na(2, Action::RightTrigger).into(),
                    moved_from: Some(na(0, Action::Button(PadButton::X))),
                    moved_more: 1,
                    shared: 0,
                },
            ),
            // S volbou: mezerník patří dál dvěma dalším vstupům.
            o(
                111,
                false,
                UiEvent::BindingSaved {
                    key: KeyId::SPACE,
                    target: na(1, Action::Button(PadButton::A)).into(),
                    moved_from: None,
                    moved_more: 0,
                    shared: 2,
                },
            ),
            o(
                112,
                false,
                UiEvent::BindingRejected {
                    key: KeyId::W,
                    reason: BindingReject::TooManyTargets,
                },
            ),
            // Zkratka pozastavení z okna (Fáze 7, Z6): uložena F9, odmítnuta
            // namapovaná F5 a Tab (není F1–F24, Scroll Lock ani Pause).
            o(
                113,
                false,
                UiEvent::BindingSaved {
                    key: KeyId::new(0x43),
                    target: BindTarget::Toggle,
                    moved_from: None,
                    moved_more: 0,
                    shared: 0,
                },
            ),
            o(
                114,
                false,
                UiEvent::BindingRejected {
                    key: KeyId::F5,
                    reason: BindingReject::Mapped,
                },
            ),
            o(
                115,
                false,
                UiEvent::BindingRejected {
                    key: KeyId::TAB,
                    reason: BindingReject::NotToggleKey,
                },
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

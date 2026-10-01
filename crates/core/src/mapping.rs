//! Mapování kláves na akce ovladačů + zkratka přepnutí, včetně validace.
//!
//! Invarianta: hodnota [`Mapping`] je VŽDY platná. Jinak se nedá
//! vyrobit — [`Mapping::new`] vrací chyby a upravující metody změnu
//! buď provedou celou, nebo vůbec. Engine (a hook vlákno, které ho
//! vlastní) se tak na platnost nemusí nikdy ptát.

use serde::{Deserialize, Serialize};

use crate::action::{Action, PadAction, PadButton, PadId, StickDir};
use crate::key::{KeyId, KEY_TABLE_SIZE};

/// Proč mapování není platné. GUI je ukazuje přímo u řádku (Fáze 6),
/// načtení konfigurace podle nich zálohuje nevalidní soubor (Fáze 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MappingError {
    /// Žádná klávesa není namapovaná (na žádném ovladači) — v režimu
    /// Gamepad by nešlo nic.
    Empty,
    /// Úprava by odebrala poslední vazbu (viz [`MappingError::Empty`]).
    WouldBeEmpty,
    /// Jedna klávesa u dvou různých akcí (nebo u téže akce dvou
    /// ovladačů). Stisk by nešel rozhodnout.
    DuplicateKey {
        key: KeyId,
        first: PadAction,
        second: PadAction,
    },
    /// Zkratka přepnutí je zároveň namapovaná na akci. Stisk zkratky
    /// vždy přepíná režim, akce by se nikdy nespustila.
    ToggleKeyMapped { key: KeyId, action: PadAction },
    /// Zkratkou přepnutí nesmí být Esc — Esc ruší přiřazování kláves
    /// a se zkratkou by se z přiřazování nedalo vycouvat.
    ToggleIsEscape,
    /// Klávesa patří Windows (Win, [`KeyId::is_reserved`]). Nemapovatelná
    /// je taky, ale hlásí se zvlášť a přednostně: okno uživateli řekne
    /// proč, ne jen „nejde". `action` je `None`, jde-li o zkratku přepnutí.
    Reserved {
        key: KeyId,
        action: Option<PadAction>,
    },
    /// Klávesa se mapovat nedá ([`KeyId::is_mappable`]). `action` je
    /// `None`, jde-li o zkratku přepnutí.
    Unmappable {
        key: KeyId,
        action: Option<PadAction>,
    },
    /// Odebíraná klávesa nemá vazbu.
    NotMapped { key: KeyId },
}

/// Akce s ovladačem pro hlášky v logu (hráči počítají od jedničky).
struct Cil(PadAction);

impl std::fmt::Display for Cil {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} na ovladači {}",
            self.0.action,
            self.0.pad.index() + 1
        )
    }
}

impl std::fmt::Display for MappingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MappingError::Empty => write!(f, "mapování je prázdné"),
            MappingError::WouldBeEmpty => write!(f, "poslední vazbu nelze odebrat"),
            MappingError::DuplicateKey { key, first, second } => write!(
                f,
                "klávesa {key} je přiřazena dvěma akcím ({} a {})",
                Cil(*first),
                Cil(*second)
            ),
            MappingError::ToggleKeyMapped { key, action } => write!(
                f,
                "klávesa {key} je zkratka přepnutí a nemůže zároveň ovládat {}",
                Cil(*action)
            ),
            MappingError::ToggleIsEscape => write!(f, "Esc nemůže být zkratka přepnutí"),
            MappingError::Reserved { key, .. } => write!(f, "klávesa {key} patří Windows"),
            MappingError::Unmappable { key, .. } => write!(f, "klávesu {key} nelze mapovat"),
            MappingError::NotMapped { key } => write!(f, "klávesa {key} nemá vazbu"),
        }
    }
}

impl std::error::Error for MappingError {}

/// Chyba pro klávesu, která nejde mapovat. Win je nemapovatelná taky,
/// ale hlásí se jako [`MappingError::Reserved`] — okno pak řekne „patří
/// Windows" místo obecného „nejde použít".
fn not_mappable(key: KeyId, action: Option<PadAction>) -> MappingError {
    if key.is_reserved() {
        MappingError::Reserved { key, action }
    } else {
        MappingError::Unmappable { key, action }
    }
}

/// Kompletní rozvržení kláves: vazby klávesa → akce na ovladači
/// a zkratka přepnutí.
///
/// Víc kláves na jednu akci je povoleno; jedna klávesa na dvě akce ne —
/// ani na dvou ovladačích (jeden stisk by hýbal dvěma hráči). Proto je
/// to tabulka podle klávesy a ovladač je součástí cíle. Ovladač bez
/// jediné klávesy je v pořádku (nově přidaný začíná prázdný); prázdné
/// nesmí být jen mapování jako celek.
///
/// Pevná tabulka ([`KeyId::index`]), ne `BTreeMap`: přiřazení klávesy
/// probíhá v hook callbacku a strom by při vkládání alokoval (revize
/// naměřila alokaci u 50 z 252 kláves). Pole nealokuje nikdy a vyhledání
/// je jeden přístup do paměti.
///
/// Konfigurace (Fáze 7) a GUI s ním pracují jako se seznamem
/// [`Mapping::bindings`] — ne jako s mapou podle `KeyId`, ta se do JSON
/// serializovat nedá (klíč musí být řetězec).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Mapping {
    toggle: KeyId,
    keys: [Option<PadAction>; KEY_TABLE_SIZE],
}

impl std::fmt::Debug for Mapping {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Jen obsazené řádky — 256 položek s `None` by v logu nikdo nečetl.
        f.debug_struct("Mapping")
            .field("toggle", &self.toggle)
            .field("bindings", &self.bindings().collect::<Vec<_>>())
            .finish()
    }
}

impl Mapping {
    /// Postaví a zvaliduje mapování. Vrací VŠECHNY nalezené chyby, ať je
    /// GUI může ukázat najednou, ne jednu po druhé.
    ///
    /// Stejná klávesa uvedená dvakrát u téže akce téhož ovladače chybou
    /// není — nic nerozbíjí, jen se sloučí.
    pub fn new(
        toggle: KeyId,
        bindings: impl IntoIterator<Item = (KeyId, PadAction)>,
    ) -> Result<Mapping, Vec<MappingError>> {
        let mut errors = Vec::new();
        if !toggle.is_mappable() {
            errors.push(not_mappable(toggle, None));
        }
        if toggle == KeyId::ESC {
            errors.push(MappingError::ToggleIsEscape);
        }

        let mut keys = [None; KEY_TABLE_SIZE];
        let mut rejected_binding = false;
        for (key, target) in bindings {
            let Some(slot) = key.index().and_then(|i| keys.get_mut(i)) else {
                errors.push(not_mappable(key, Some(target)));
                rejected_binding = true;
                continue;
            };
            if key == toggle {
                errors.push(MappingError::ToggleKeyMapped {
                    key,
                    action: target,
                });
                rejected_binding = true;
                continue;
            }
            match *slot {
                Some(first) if first != target => errors.push(MappingError::DuplicateKey {
                    key,
                    first,
                    second: target,
                }),
                Some(_) => {}
                None => *slot = Some(target),
            }
        }
        // Prázdné se hlásí, jen když žádná vazba nevypadla kvůli jiné
        // chybě — jinak by se k „klávesa X nejde mapovat" přidávalo
        // matoucí „mapování je prázdné" jen proto, že se X nepřidala.
        // Chyba zkratky ale prázdnotu nezakrývá: seznam vazeb prázdný
        // opravdu je a uživatel se to má dozvědět hned, ne až v dalším kole.
        if !rejected_binding && keys.iter().all(Option::is_none) {
            errors.push(MappingError::Empty);
        }

        if errors.is_empty() {
            Ok(Mapping { toggle, keys })
        } else {
            Err(errors)
        }
    }

    /// Zkratka přepnutí (pozastavení zachytávání).
    pub fn toggle_key(&self) -> KeyId {
        self.toggle
    }

    /// Akce a ovladač, na které je klávesa namapovaná.
    pub fn target(&self, key: KeyId) -> Option<PadAction> {
        key.index()
            .and_then(|i| self.keys.get(i).copied())
            .flatten()
    }

    /// Klávesy dané akce daného ovladače (může jich být víc).
    pub fn keys_for(&self, t: PadAction) -> impl Iterator<Item = KeyId> + '_ {
        self.bindings()
            .filter(move |&(_, b)| b == t)
            .map(|(k, _)| k)
    }

    /// Všechny vazby — nejdřív běžné klávesy podle scan kódu, pak
    /// rozšířené (E0).
    pub fn bindings(&self) -> impl Iterator<Item = (KeyId, PadAction)> + '_ {
        self.keys
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.map(|t| (KeyId::from_index(i), t)))
    }

    /// Vazby jednoho ovladače (ve stejném pořadí jako [`Mapping::bindings`]).
    pub fn pad_bindings(&self, pad: PadId) -> impl Iterator<Item = (KeyId, Action)> + '_ {
        self.bindings()
            .filter(move |(_, t)| t.pad == pad)
            .map(|(k, t)| (k, t.action))
    }

    /// Počet vazeb všech ovladačů.
    pub fn len(&self) -> usize {
        self.keys.iter().filter(|t| t.is_some()).count()
    }

    /// Vždy `false` — prázdné mapování nejde vyrobit. Metoda je tu kvůli
    /// konvenci k `len()`.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Přiřadí klávesu akci ovladače (přidá ji k případným dalším
    /// klávesám té akce). Patřila-li klávesa dosud JINÉ akci nebo jinému
    /// ovladači, vazba se přesune a vrátí se původní cíl — klávesa patří
    /// vždy jen jednomu ovladači a jedné akci.
    ///
    /// Nealokuje — volá se z hook callbacku.
    pub fn bind(&mut self, key: KeyId, t: PadAction) -> Result<Option<PadAction>, MappingError> {
        if key == self.toggle {
            return Err(MappingError::ToggleKeyMapped { key, action: t });
        }
        let Some(slot) = key.index().and_then(|i| self.keys.get_mut(i)) else {
            return Err(not_mappable(key, Some(t)));
        };
        let previous = slot.replace(t);
        Ok(previous.filter(|&p| p != t))
    }

    /// Odebere vazbu klávesy. Poslední vazbu (celého mapování) odebrat
    /// nelze.
    pub fn unbind(&mut self, key: KeyId) -> Result<PadAction, MappingError> {
        let Some(t) = self.target(key) else {
            return Err(MappingError::NotMapped { key });
        };
        if self.len() == 1 {
            return Err(MappingError::WouldBeEmpty);
        }
        if let Some(slot) = key.index().and_then(|i| self.keys.get_mut(i)) {
            *slot = None;
        }
        Ok(t)
    }

    /// Odebere všechny vazby ovladače (ovladač zmizel z okna) a vrátí,
    /// kolik jich bylo. Všechno, nebo nic: patří-li ovladači všechny
    /// vazby, mapování by zůstalo prázdné → [`MappingError::WouldBeEmpty`]
    /// a nic se neodebere.
    pub fn clear_pad(&mut self, pad: PadId) -> Result<usize, MappingError> {
        let n = self.pad_bindings(pad).count();
        if n > 0 && n == self.len() {
            return Err(MappingError::WouldBeEmpty);
        }
        for slot in &mut self.keys {
            if slot.is_some_and(|t| t.pad == pad) {
                *slot = None;
            }
        }
        Ok(n)
    }

    /// Změní zkratku přepnutí. Nesmí být namapovaná, Esc ani Win.
    pub fn set_toggle_key(&mut self, key: KeyId) -> Result<(), MappingError> {
        if !key.is_mappable() {
            return Err(not_mappable(key, None));
        }
        if key == KeyId::ESC {
            return Err(MappingError::ToggleIsEscape);
        }
        if let Some(action) = self.target(key) {
            return Err(MappingError::ToggleKeyMapped { key, action });
        }
        self.toggle = key;
        Ok(())
    }
}

impl Default for Mapping {
    /// Výchozí rozvržení podle POZICE kláves (ROADMAP.md, „Výchozí
    /// mapování"), celé na prvním ovladači: levá páčka WASD, pravá
    /// šipky, D-pad IJKL, A/B/X/Y mezerník/C/F/R, LB/RB Q/E, LT/RT 1/3,
    /// L3/R3 levý Shift/V, Start/Back Enter/Backspace, přepnutí Scroll
    /// Lock. Další ovladače začínají bez kláves.
    fn default() -> Self {
        use Action::{Button, LeftStick, LeftTrigger, RightStick, RightTrigger};
        use PadButton as B;
        use StickDir::{Down, Left, Right, Up};

        let bindings = [
            (KeyId::W, LeftStick(Up)),
            (KeyId::A, LeftStick(Left)),
            (KeyId::S, LeftStick(Down)),
            (KeyId::D, LeftStick(Right)),
            (KeyId::ARROW_UP, RightStick(Up)),
            (KeyId::ARROW_LEFT, RightStick(Left)),
            (KeyId::ARROW_DOWN, RightStick(Down)),
            (KeyId::ARROW_RIGHT, RightStick(Right)),
            (KeyId::I, Button(B::DpadUp)),
            (KeyId::J, Button(B::DpadLeft)),
            (KeyId::K, Button(B::DpadDown)),
            (KeyId::L, Button(B::DpadRight)),
            (KeyId::SPACE, Button(B::A)),
            (KeyId::C, Button(B::B)),
            (KeyId::F, Button(B::X)),
            (KeyId::R, Button(B::Y)),
            (KeyId::Q, Button(B::Lb)),
            (KeyId::E, Button(B::Rb)),
            (KeyId::DIGIT_1, LeftTrigger),
            (KeyId::DIGIT_3, RightTrigger),
            (KeyId::LEFT_SHIFT, Button(B::L3)),
            (KeyId::V, Button(B::R3)),
            (KeyId::ENTER, Button(B::Start)),
            (KeyId::BACKSPACE, Button(B::Back)),
        ];
        // Pevná data, platnost hlídá test `vychozi_mapovani_je_platne`.
        Mapping::new(
            KeyId::SCROLL_LOCK,
            bindings.map(|(k, a)| (k, PadAction::first(a))),
        )
        .unwrap_or_else(|e| unreachable!("výchozí mapování je neplatné: {e:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UP: PadAction = PadAction::first(Action::LeftStick(StickDir::Up));
    const A_BTN: PadAction = PadAction::first(Action::Button(PadButton::A));
    const P1: PadId = PadId::ALL[1];

    fn na(pad: PadId, t: PadAction) -> PadAction {
        PadAction::new(pad, t.action)
    }

    #[test]
    fn vychozi_mapovani_je_platne() {
        let d = Mapping::default();
        let znovu = Mapping::new(d.toggle_key(), d.bindings()).expect("výchozí je platné");
        assert_eq!(znovu, d);
        assert_eq!(d.len(), 24);
        assert_eq!(d.toggle_key(), KeyId::SCROLL_LOCK);
        assert_eq!(d.target(KeyId::W), Some(UP));
        assert_eq!(d.target(KeyId::NUMPAD_8), None);
        assert_eq!(
            d.target(KeyId::ARROW_UP),
            Some(PadAction::first(Action::RightStick(StickDir::Up)))
        );
        // Každá akce má ve výchozím mapování právě jednu klávesu, vše na
        // prvním ovladači; ostatní ovladače začínají prázdné.
        for a in Action::all() {
            assert_eq!(d.keys_for(PadAction::first(a)).count(), 1, "{a:?}");
        }
        assert_eq!(d.pad_bindings(PadId::FIRST).count(), 24);
        for p in &PadId::ALL[1..] {
            assert_eq!(d.pad_bindings(*p).count(), 0, "{p:?}");
        }
    }

    #[test]
    fn duplicitni_klavesa_pro_dve_akce_se_zamitne() {
        let e = Mapping::new(KeyId::SCROLL_LOCK, [(KeyId::W, UP), (KeyId::W, A_BTN)]).unwrap_err();
        assert_eq!(
            e,
            vec![MappingError::DuplicateKey {
                key: KeyId::W,
                first: UP,
                second: A_BTN
            }]
        );
    }

    #[test]
    fn klavesa_nesmi_patrit_dvema_ovladacum() {
        // Tatáž akce, jiný ovladač: jeden stisk by hýbal dvěma hráči.
        let e =
            Mapping::new(KeyId::SCROLL_LOCK, [(KeyId::W, UP), (KeyId::W, na(P1, UP))]).unwrap_err();
        assert_eq!(
            e,
            vec![MappingError::DuplicateKey {
                key: KeyId::W,
                first: UP,
                second: na(P1, UP)
            }]
        );
    }

    #[test]
    fn stejna_vazba_dvakrat_neni_chyba() {
        let m = Mapping::new(KeyId::SCROLL_LOCK, [(KeyId::W, UP), (KeyId::W, UP)]).unwrap();
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn prazdny_ovladac_neni_chyba() {
        // Jen druhý ovladač má klávesy — první je prázdný, a to je v pořádku.
        let m = Mapping::new(KeyId::SCROLL_LOCK, [(KeyId::W, na(P1, UP))]).unwrap();
        assert_eq!(m.pad_bindings(PadId::FIRST).count(), 0);
        assert_eq!(
            m.pad_bindings(P1).collect::<Vec<_>>(),
            vec![(KeyId::W, UP.action)]
        );
    }

    #[test]
    fn zkratka_prepnuti_nesmi_byt_namapovana() {
        let e = Mapping::new(
            KeyId::SCROLL_LOCK,
            [(KeyId::W, UP), (KeyId::SCROLL_LOCK, na(P1, A_BTN))],
        )
        .unwrap_err();
        assert_eq!(
            e,
            vec![MappingError::ToggleKeyMapped {
                key: KeyId::SCROLL_LOCK,
                action: na(P1, A_BTN)
            }]
        );
    }

    #[test]
    fn prazdne_mapovani_se_zamitne() {
        assert_eq!(
            Mapping::new(KeyId::SCROLL_LOCK, []).unwrap_err(),
            vec![MappingError::Empty]
        );
    }

    #[test]
    fn prazdne_se_hlasi_i_vedle_chyby_zkratky() {
        let e = Mapping::new(KeyId::ESC, []).unwrap_err();
        assert_eq!(e, vec![MappingError::ToggleIsEscape, MappingError::Empty]);
    }

    #[test]
    fn prazdne_se_nehlasi_kdyz_vazba_vypadla_jinou_chybou() {
        let e = Mapping::new(KeyId::SCROLL_LOCK, [(KeyId::ALTGR_FAKE_CTRL, UP)]).unwrap_err();
        assert_eq!(
            e,
            vec![MappingError::Unmappable {
                key: KeyId::ALTGR_FAKE_CTRL,
                action: Some(UP)
            }]
        );
    }

    #[test]
    fn vraci_vsechny_chyby_najednou() {
        let e = Mapping::new(
            KeyId::ESC,
            [
                (KeyId::ALTGR_FAKE_CTRL, UP),
                (KeyId::W, UP),
                (KeyId::W, A_BTN),
            ],
        )
        .unwrap_err();
        assert_eq!(e.len(), 3, "{e:?}");
        assert!(e.contains(&MappingError::ToggleIsEscape));
        assert!(e.contains(&MappingError::Unmappable {
            key: KeyId::ALTGR_FAKE_CTRL,
            action: Some(UP)
        }));
    }

    #[test]
    fn nemapovatelna_zkratka_se_zamitne() {
        let e = Mapping::new(KeyId::new(0), [(KeyId::W, UP)]).unwrap_err();
        assert_eq!(
            e,
            vec![MappingError::Unmappable {
                key: KeyId::new(0),
                action: None
            }]
        );
    }

    #[test]
    fn win_nejde_mapovat_ani_jako_zkratka() {
        // Vazba na Win: jen „patří Windows", ne navíc „nejde mapovat" ani
        // „prázdné" (vazba vypadla kvůli chybě, viz výše).
        let e = Mapping::new(KeyId::SCROLL_LOCK, [(KeyId::LEFT_WIN, UP)]).unwrap_err();
        assert_eq!(
            e,
            vec![MappingError::Reserved {
                key: KeyId::LEFT_WIN,
                action: Some(UP)
            }]
        );
        let e = Mapping::new(KeyId::RIGHT_WIN, [(KeyId::W, UP)]).unwrap_err();
        assert_eq!(
            e,
            vec![MappingError::Reserved {
                key: KeyId::RIGHT_WIN,
                action: None
            }]
        );
        assert!(e[0].to_string().contains("patří Windows"), "{}", e[0]);

        let mut m = Mapping::default();
        let pred = m.clone();
        assert_eq!(
            m.bind(KeyId::RIGHT_WIN, na(P1, A_BTN)),
            Err(MappingError::Reserved {
                key: KeyId::RIGHT_WIN,
                action: Some(na(P1, A_BTN))
            })
        );
        assert_eq!(
            m.set_toggle_key(KeyId::LEFT_WIN),
            Err(MappingError::Reserved {
                key: KeyId::LEFT_WIN,
                action: None
            })
        );
        assert_eq!(m, pred, "neúspěch nic nezměnil");
        assert_eq!(m.target(KeyId::LEFT_WIN), None);
        // Alt Windows nepatří — mapovat jde (okno jen varuje).
        assert_eq!(m.bind(KeyId::LEFT_ALT, UP), Ok(None));
    }

    #[test]
    fn bind_presune_klavesu_z_jine_akce() {
        let mut m = Mapping::default();
        assert_eq!(m.bind(KeyId::W, A_BTN), Ok(Some(UP)));
        assert_eq!(m.target(KeyId::W), Some(A_BTN));
        // Mezerník zůstává u A — akce má teď dvě klávesy.
        assert_eq!(m.keys_for(A_BTN).count(), 2);
        // Znovu totéž = nic se nepřesouvá.
        assert_eq!(m.bind(KeyId::W, A_BTN), Ok(None));
    }

    #[test]
    fn bind_presune_klavesu_na_jiny_ovladac() {
        let mut m = Mapping::default();
        // Tatáž akce na druhém ovladači: klávesa se přesune, ne zdvojí.
        assert_eq!(m.bind(KeyId::W, na(P1, UP)), Ok(Some(UP)));
        assert_eq!(m.target(KeyId::W), Some(na(P1, UP)));
        assert_eq!(m.keys_for(UP).count(), 0, "první ovladač o W přišel");
        assert_eq!(
            m.pad_bindings(P1).collect::<Vec<_>>(),
            vec![(KeyId::W, UP.action)]
        );
        assert_eq!(m.len(), 24, "přesun nic nepřidal");
        // A zpátky na jinou akci prvního ovladače.
        assert_eq!(m.bind(KeyId::W, A_BTN), Ok(Some(na(P1, UP))));
        assert_eq!(m.pad_bindings(P1).count(), 0);
    }

    #[test]
    fn bind_odmitne_zkratku_a_nemapovatelne() {
        let mut m = Mapping::default();
        let pred = m.clone();
        assert!(matches!(
            m.bind(KeyId::SCROLL_LOCK, UP),
            Err(MappingError::ToggleKeyMapped { .. })
        ));
        assert!(matches!(
            m.bind(KeyId::ALTGR_FAKE_CTRL, UP),
            Err(MappingError::Unmappable { .. })
        ));
        assert!(matches!(
            m.bind(KeyId::new(0), na(P1, UP)),
            Err(MappingError::Unmappable { .. })
        ));
        assert_eq!(m, pred, "neúspěch nic nezměnil");
    }

    #[test]
    fn posledni_vazbu_nelze_odebrat() {
        let mut m = Mapping::new(
            KeyId::SCROLL_LOCK,
            [(KeyId::W, UP), (KeyId::A, na(P1, A_BTN))],
        )
        .unwrap();
        assert_eq!(m.unbind(KeyId::A), Ok(na(P1, A_BTN)));
        assert_eq!(
            m.unbind(KeyId::A),
            Err(MappingError::NotMapped { key: KeyId::A })
        );
        assert_eq!(m.unbind(KeyId::W), Err(MappingError::WouldBeEmpty));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn clear_pad_odebere_jen_jeho_vazby() {
        let mut m = Mapping::default();
        m.bind(KeyId::X, na(P1, A_BTN)).unwrap();
        m.bind(KeyId::W, na(P1, UP)).unwrap();
        assert_eq!(m.clear_pad(P1), Ok(2));
        assert_eq!(m.pad_bindings(P1).count(), 0);
        assert_eq!(m.target(KeyId::W), None, "přesunutá klávesa je pryč");
        assert_eq!(m.len(), 23, "první ovladač beze změny");
        // Ovladač bez kláves: nic k odebrání, žádná chyba.
        assert_eq!(m.clear_pad(P1), Ok(0));
        assert_eq!(m.clear_pad(PadId::ALL[3]), Ok(0));
    }

    #[test]
    fn clear_pad_nevyprazdni_mapovani() {
        let mut m = Mapping::default();
        let pred = m.clone();
        assert_eq!(m.clear_pad(PadId::FIRST), Err(MappingError::WouldBeEmpty));
        assert_eq!(m, pred, "všechno, nebo nic");
        // Když má klávesy i jiný ovladač, první jde vyprázdnit.
        m.bind(KeyId::X, na(P1, A_BTN)).unwrap();
        assert_eq!(m.clear_pad(PadId::FIRST), Ok(24));
        assert_eq!(
            m.bindings().collect::<Vec<_>>(),
            vec![(KeyId::X, na(P1, A_BTN))]
        );
    }

    #[test]
    fn zmena_zkratky_se_validuje() {
        let mut m = Mapping::default();
        m.bind(KeyId::X, na(P1, A_BTN)).unwrap();
        assert!(matches!(
            m.set_toggle_key(KeyId::W),
            Err(MappingError::ToggleKeyMapped { .. })
        ));
        assert_eq!(
            m.set_toggle_key(KeyId::X),
            Err(MappingError::ToggleKeyMapped {
                key: KeyId::X,
                action: na(P1, A_BTN)
            }),
            "klávesa druhého ovladače taky nejde"
        );
        assert_eq!(
            m.set_toggle_key(KeyId::ESC),
            Err(MappingError::ToggleIsEscape)
        );
        assert_eq!(m.set_toggle_key(KeyId::NUMPAD_8), Ok(()));
        assert_eq!(m.toggle_key(), KeyId::NUMPAD_8);
    }

    #[test]
    fn debug_ukazuje_jen_vazby() {
        let m = Mapping::new(KeyId::SCROLL_LOCK, [(KeyId::W, UP)]).unwrap();
        let s = format!("{m:?}");
        assert!(s.contains("LeftStick(Up)"), "{s}");
        assert!(!s.contains("None"), "{s}");
    }

    #[test]
    fn hlaska_cisluje_ovladace_od_jednicky() {
        let e = MappingError::ToggleKeyMapped {
            key: KeyId::X,
            action: na(P1, A_BTN),
        };
        assert!(e.to_string().contains("na ovladači 2"), "{e}");
    }
}

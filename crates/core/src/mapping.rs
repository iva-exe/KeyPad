//! Mapování kláves na akce + zkratka přepnutí, včetně validace.
//!
//! Invarianta: hodnota [`Mapping`] je VŽDY platná. Jinak se nedá
//! vyrobit — [`Mapping::new`] vrací chyby a upravující metody změnu
//! buď provedou celou, nebo vůbec. Engine (a hook vlákno, které ho
//! vlastní) se tak na platnost nemusí nikdy ptát.

use serde::{Deserialize, Serialize};

use crate::action::{Action, PadButton, StickDir};
use crate::key::{KeyId, KEY_TABLE_SIZE};

/// Proč mapování není platné. GUI je ukazuje přímo u řádku (Fáze 6),
/// načtení konfigurace podle nich zálohuje nevalidní soubor (Fáze 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MappingError {
    /// Žádná klávesa není namapovaná — v režimu Gamepad by nešlo nic.
    Empty,
    /// Úprava by odebrala poslední vazbu (viz [`MappingError::Empty`]).
    WouldBeEmpty,
    /// Jedna klávesa u dvou různých akcí. Stisk by nešel rozhodnout.
    DuplicateKey {
        key: KeyId,
        first: Action,
        second: Action,
    },
    /// Zkratka přepnutí je zároveň namapovaná na akci. Stisk zkratky
    /// vždy přepíná režim, akce by se nikdy nespustila.
    ToggleKeyMapped { key: KeyId, action: Action },
    /// Zkratkou přepnutí nesmí být Esc — Esc ruší přiřazování kláves
    /// a se zkratkou by se z přiřazování nedalo vycouvat.
    ToggleIsEscape,
    /// Klávesa se mapovat nedá ([`KeyId::is_mappable`]). `action` je
    /// `None`, jde-li o zkratku přepnutí.
    Unmappable { key: KeyId, action: Option<Action> },
    /// Odebíraná klávesa nemá vazbu.
    NotMapped { key: KeyId },
}

impl std::fmt::Display for MappingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MappingError::Empty => write!(f, "mapování je prázdné"),
            MappingError::WouldBeEmpty => write!(f, "poslední vazbu nelze odebrat"),
            MappingError::DuplicateKey { key, first, second } => write!(
                f,
                "klávesa {key} je přiřazena dvěma akcím ({first:?} a {second:?})"
            ),
            MappingError::ToggleKeyMapped { key, action } => write!(
                f,
                "klávesa {key} je zkratka přepnutí a nemůže zároveň ovládat {action:?}"
            ),
            MappingError::ToggleIsEscape => write!(f, "Esc nemůže být zkratka přepnutí"),
            MappingError::Unmappable { key, .. } => write!(f, "klávesu {key} nelze mapovat"),
            MappingError::NotMapped { key } => write!(f, "klávesa {key} nemá vazbu"),
        }
    }
}

impl std::error::Error for MappingError {}

/// Kompletní rozvržení kláves: vazby klávesa → akce a zkratka přepnutí.
///
/// Víc kláves na jednu akci je povoleno; jedna klávesa na dvě akce ne.
/// Proto je to tabulka podle klávesy.
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
    keys: [Option<Action>; KEY_TABLE_SIZE],
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
    /// Stejná klávesa uvedená dvakrát u téže akce chybou není — nic
    /// nerozbíjí, jen se sloučí.
    pub fn new(
        toggle: KeyId,
        bindings: impl IntoIterator<Item = (KeyId, Action)>,
    ) -> Result<Mapping, Vec<MappingError>> {
        let mut errors = Vec::new();
        if !toggle.is_mappable() {
            errors.push(MappingError::Unmappable {
                key: toggle,
                action: None,
            });
        }
        if toggle == KeyId::ESC {
            errors.push(MappingError::ToggleIsEscape);
        }

        let mut keys = [None; KEY_TABLE_SIZE];
        let mut rejected_binding = false;
        for (key, action) in bindings {
            let Some(slot) = key.index().and_then(|i| keys.get_mut(i)) else {
                errors.push(MappingError::Unmappable {
                    key,
                    action: Some(action),
                });
                rejected_binding = true;
                continue;
            };
            if key == toggle {
                errors.push(MappingError::ToggleKeyMapped { key, action });
                rejected_binding = true;
                continue;
            }
            match *slot {
                Some(first) if first != action => errors.push(MappingError::DuplicateKey {
                    key,
                    first,
                    second: action,
                }),
                Some(_) => {}
                None => *slot = Some(action),
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

    /// Zkratka přepnutí Klávesnice ↔ Gamepad.
    pub fn toggle_key(&self) -> KeyId {
        self.toggle
    }

    /// Akce namapovaná na klávesu.
    pub fn action_for(&self, key: KeyId) -> Option<Action> {
        key.index()
            .and_then(|i| self.keys.get(i).copied())
            .flatten()
    }

    /// Klávesy dané akce (může jich být víc).
    pub fn keys_for(&self, action: Action) -> impl Iterator<Item = KeyId> + '_ {
        self.bindings()
            .filter(move |&(_, a)| a == action)
            .map(|(k, _)| k)
    }

    /// Všechny vazby — nejdřív běžné klávesy podle scan kódu, pak
    /// rozšířené (E0).
    pub fn bindings(&self) -> impl Iterator<Item = (KeyId, Action)> + '_ {
        self.keys
            .iter()
            .enumerate()
            .filter_map(|(i, a)| a.map(|a| (KeyId::from_index(i), a)))
    }

    pub fn len(&self) -> usize {
        self.keys.iter().filter(|a| a.is_some()).count()
    }

    /// Vždy `false` — prázdné mapování nejde vyrobit. Metoda je tu kvůli
    /// konvenci k `len()`.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Přiřadí klávesu akci (přidá ji k případným dalším klávesám té
    /// akce). Patřila-li klávesa dosud JINÉ akci, vazba se přesune
    /// a vrátí se původní akce — dvě akce na jedné klávese být nesmí.
    ///
    /// Nealokuje — volá se z hook callbacku.
    pub fn bind(&mut self, key: KeyId, action: Action) -> Result<Option<Action>, MappingError> {
        if key == self.toggle {
            return Err(MappingError::ToggleKeyMapped { key, action });
        }
        let Some(slot) = key.index().and_then(|i| self.keys.get_mut(i)) else {
            return Err(MappingError::Unmappable {
                key,
                action: Some(action),
            });
        };
        let previous = slot.replace(action);
        Ok(previous.filter(|&p| p != action))
    }

    /// Odebere vazbu klávesy. Poslední vazbu odebrat nelze.
    pub fn unbind(&mut self, key: KeyId) -> Result<Action, MappingError> {
        let Some(action) = self.action_for(key) else {
            return Err(MappingError::NotMapped { key });
        };
        if self.len() == 1 {
            return Err(MappingError::WouldBeEmpty);
        }
        if let Some(slot) = key.index().and_then(|i| self.keys.get_mut(i)) {
            *slot = None;
        }
        Ok(action)
    }

    /// Změní zkratku přepnutí. Nesmí být namapovaná ani Esc.
    pub fn set_toggle_key(&mut self, key: KeyId) -> Result<(), MappingError> {
        if !key.is_mappable() {
            return Err(MappingError::Unmappable { key, action: None });
        }
        if key == KeyId::ESC {
            return Err(MappingError::ToggleIsEscape);
        }
        if let Some(action) = self.action_for(key) {
            return Err(MappingError::ToggleKeyMapped { key, action });
        }
        self.toggle = key;
        Ok(())
    }
}

impl Default for Mapping {
    /// Výchozí rozvržení podle POZICE kláves (ROADMAP.md, „Výchozí
    /// mapování"): levá páčka WASD, pravá šipky, D-pad IJKL, A/B/X/Y
    /// mezerník/C/F/R, LB/RB Q/E, LT/RT 1/3, L3/R3 levý Shift/V,
    /// Start/Back Enter/Backspace, přepnutí Scroll Lock.
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
        Mapping::new(KeyId::SCROLL_LOCK, bindings)
            .unwrap_or_else(|e| unreachable!("výchozí mapování je neplatné: {e:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UP: Action = Action::LeftStick(StickDir::Up);
    const A_BTN: Action = Action::Button(PadButton::A);

    #[test]
    fn vychozi_mapovani_je_platne() {
        let d = Mapping::default();
        let znovu = Mapping::new(d.toggle_key(), d.bindings()).expect("výchozí je platné");
        assert_eq!(znovu, d);
        assert_eq!(d.len(), 24);
        assert_eq!(d.toggle_key(), KeyId::SCROLL_LOCK);
        assert_eq!(d.action_for(KeyId::W), Some(UP));
        assert_eq!(d.action_for(KeyId::NUMPAD_8), None);
        assert_eq!(
            d.action_for(KeyId::ARROW_UP),
            Some(Action::RightStick(StickDir::Up))
        );
        // Každá akce má ve výchozím mapování právě jednu klávesu.
        for a in Action::all() {
            assert_eq!(d.keys_for(a).count(), 1, "{a:?}");
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
    fn stejna_vazba_dvakrat_neni_chyba() {
        let m = Mapping::new(KeyId::SCROLL_LOCK, [(KeyId::W, UP), (KeyId::W, UP)]).unwrap();
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn zkratka_prepnuti_nesmi_byt_namapovana() {
        let e = Mapping::new(
            KeyId::SCROLL_LOCK,
            [(KeyId::W, UP), (KeyId::SCROLL_LOCK, A_BTN)],
        )
        .unwrap_err();
        assert_eq!(
            e,
            vec![MappingError::ToggleKeyMapped {
                key: KeyId::SCROLL_LOCK,
                action: A_BTN
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
    fn bind_presune_klavesu_z_jine_akce() {
        let mut m = Mapping::default();
        assert_eq!(m.bind(KeyId::W, A_BTN), Ok(Some(UP)));
        assert_eq!(m.action_for(KeyId::W), Some(A_BTN));
        // Mezerník zůstává u A — akce má teď dvě klávesy.
        assert_eq!(m.keys_for(A_BTN).count(), 2);
        // Znovu totéž = nic se nepřesouvá.
        assert_eq!(m.bind(KeyId::W, A_BTN), Ok(None));
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
            m.bind(KeyId::new(0), UP),
            Err(MappingError::Unmappable { .. })
        ));
        assert_eq!(m, pred, "neúspěch nic nezměnil");
    }

    #[test]
    fn posledni_vazbu_nelze_odebrat() {
        let mut m = Mapping::new(KeyId::SCROLL_LOCK, [(KeyId::W, UP), (KeyId::A, A_BTN)]).unwrap();
        assert_eq!(m.unbind(KeyId::A), Ok(A_BTN));
        assert_eq!(
            m.unbind(KeyId::A),
            Err(MappingError::NotMapped { key: KeyId::A })
        );
        assert_eq!(m.unbind(KeyId::W), Err(MappingError::WouldBeEmpty));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn zmena_zkratky_se_validuje() {
        let mut m = Mapping::default();
        assert!(matches!(
            m.set_toggle_key(KeyId::W),
            Err(MappingError::ToggleKeyMapped { .. })
        ));
        assert_eq!(
            m.set_toggle_key(KeyId::ESC),
            Err(MappingError::ToggleIsEscape)
        );
        assert_eq!(m.set_toggle_key(KeyId::X), Ok(()));
        assert_eq!(m.toggle_key(), KeyId::X);
    }

    #[test]
    fn debug_ukazuje_jen_vazby() {
        let m = Mapping::new(KeyId::SCROLL_LOCK, [(KeyId::W, UP)]).unwrap();
        let s = format!("{m:?}");
        assert!(s.contains("LeftStick(Up)"), "{s}");
        assert!(!s.contains("None"), "{s}");
    }
}

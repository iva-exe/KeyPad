//! Mapování kláves na akce ovladačů + zkratka přepnutí, včetně validace.
//!
//! Invarianta: hodnota [`Mapping`] je VŽDY platná. Jinak se nedá
//! vyrobit — [`Mapping::new`] vrací chyby a upravující metody změnu
//! buď provedou celou, nebo vůbec. Engine (a hook vlákno, které ho
//! vlastní) se tak na platnost nemusí nikdy ptát.
//!
//! Fáze 7: klávesa smí ovládat až [`MAX_TARGETS_PER_KEY`] vstupů
//! (sdílená klávesa, [`Targets`]). Jestli přiřazení klávesy, která už
//! patří jinam, klávesu přesune, nebo sdílí, řídí [`KeyConflict`] —
//! volba „Jedna klávesa pro víc vstupů" v okně. Platnost mapování na
//! volbě nezávisí: sdílená klávesa je platná vždy, neplatný je jen pátý
//! vstup.

use serde::{Deserialize, Serialize};

use crate::action::{Action, PadAction, PadButton, PadId, StickDir};
use crate::key::{KeyId, KEY_TABLE_SIZE};
use crate::targets::{Targets, MAX_TARGETS_PER_KEY};

/// Proč mapování není platné. GUI je ukazuje přímo u řádku (Fáze 6),
/// načtení konfigurace podle nich zálohuje nevalidní soubor (Fáze 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MappingError {
    /// Žádná klávesa není namapovaná (na žádném ovladači) — v režimu
    /// Gamepad by nešlo nic.
    Empty,
    /// Úprava by odebrala poslední vazbu (viz [`MappingError::Empty`]).
    WouldBeEmpty,
    /// Klávesa by ovládala víc než [`MAX_TARGETS_PER_KEY`] různých
    /// vstupů. Tatáž klávesa u víc vstupů chybou není (sdílená klávesa,
    /// Fáze 7) — jen pátý vstup.
    TooManyTargets { key: KeyId },
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
            MappingError::TooManyTargets { key } => write!(
                f,
                "klávesa {key} ovládá víc než {MAX_TARGETS_PER_KEY} vstupy"
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

/// Co udělat s přiřazovanou klávesou, která už patří jiným vstupům
/// (volba „Jedna klávesa pro víc vstupů", Fáze 7).
///
/// Klávese, která už patří i cílovému vstupu, se nestane nic ani
/// s `Move`: sdílenou klávesu nesmí vzít vstupům, na které uživatel
/// neklikl.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyConflict {
    /// Klávesa se přesune — dosavadní vstupy o ni přijdou (výchozí, bez
    /// volby; okno nabízí „Zpět").
    Move,
    /// Klávesa se sdílí — patří dosavadním vstupům i novému, nejvýš
    /// [`MAX_TARGETS_PER_KEY`] (s volbou).
    Share,
}

/// Co udělalo přiřazení klávesy ([`Mapping::bind`],
/// [`Mapping::bind_replacing`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rebind {
    /// Vstupy, které o klávesu přišly (přesun — i z jiného ovladače);
    /// prázdné = klávesa byla volná, sdílí se, nebo už cíli patřila.
    pub moved: Targets,
    /// Kolika dalším vstupům klávesa po přiřazení patří (0 = jen cíli).
    pub shared: usize,
    /// Kolik jiných kláves cíl ztratil (nahrazení).
    pub removed: usize,
}

/// Kompletní rozvržení kláves: cíle každé klávesy a zkratka přepnutí.
///
/// Víc kláves na jednu akci je povoleno. Jedna klávesa smí ovládat až
/// [`MAX_TARGETS_PER_KEY`] vstupů i různých ovladačů (sdílená klávesa —
/// jen s volbou v okně, Fáze 7). Proto je to tabulka podle klávesy a cíl
/// nese ovladač. Ovladač bez jediné klávesy je v pořádku (nově přidaný
/// začíná prázdný); prázdné nesmí být jen mapování jako celek.
///
/// Pevná tabulka ([`KeyId::index`]), ne `BTreeMap`: přiřazení klávesy
/// probíhá v hook callbacku a strom by při vkládání alokoval (revize
/// naměřila alokaci u 50 z 252 kláves). Pole nealokuje nikdy a vyhledání
/// je jeden přístup do paměti.
///
/// Konfigurace (Fáze 7) a GUI s ním pracují jako se seznamem dvojic
/// [`Mapping::bindings`] — ne jako s mapou podle `KeyId`, ta se do JSON
/// serializovat nedá (klíč musí být řetězec).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Mapping {
    toggle: KeyId,
    keys: [Targets; KEY_TABLE_SIZE],
}

impl std::fmt::Debug for Mapping {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Jen obsazené řádky — 256 prázdných položek by v logu nikdo nečetl.
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
    /// Tatáž klávesa u víc různých cílů je sdílená klávesa (platné);
    /// tatáž dvojice dvakrát chybou není — sloučí se. Chyba je až pátý
    /// různý cíl jedné klávesy ([`MappingError::TooManyTargets`], jednou
    /// za klávesu). Na pořadí vazeb nezáleží: tatáž množina dá totéž
    /// mapování.
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

        let mut keys = [Targets::EMPTY; KEY_TABLE_SIZE];
        let mut full = [false; KEY_TABLE_SIZE];
        let mut rejected_binding = false;
        for (key, target) in bindings {
            let Some(i) = key.index().filter(|&i| i < KEY_TABLE_SIZE) else {
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
            if !keys[i].insert(target) && !full[i] {
                full[i] = true;
                errors.push(MappingError::TooManyTargets { key });
            }
        }
        // Prázdné se hlásí, jen když žádná vazba nevypadla kvůli jiné
        // chybě — jinak by se k „klávesa X nejde mapovat" přidávalo
        // matoucí „mapování je prázdné" jen proto, že se X nepřidala.
        // Chyba zkratky ale prázdnotu nezakrývá: seznam vazeb prázdný
        // opravdu je a uživatel se to má dozvědět hned, ne až v dalším kole.
        if !rejected_binding && keys.iter().all(|t| t.is_empty()) {
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

    /// Cíle klávesy (prázdné = nenamapovaná nebo nemapovatelná).
    pub fn targets(&self, key: KeyId) -> Targets {
        key.index()
            .and_then(|i| self.keys.get(i).copied())
            .unwrap_or(Targets::EMPTY)
    }

    /// Klávesy dané akce daného ovladače (může jich být víc).
    pub fn keys_for(&self, t: PadAction) -> impl Iterator<Item = KeyId> + '_ {
        self.keys
            .iter()
            .enumerate()
            .filter(move |(_, s)| s.contains(t))
            .map(|(i, _)| KeyId::from_index(i))
    }

    /// Namapované klávesy s jejich cíli — nejdřív běžné klávesy podle
    /// scan kódu, pak rozšířené (E0). Každá klávesa jednou.
    pub fn keys(&self) -> impl Iterator<Item = (KeyId, Targets)> + '_ {
        self.keys
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.is_empty())
            .map(|(i, s)| (KeyId::from_index(i), *s))
    }

    /// Všechny vazby jako dvojice (klávesa, cíl) — klávesy jako
    /// [`Mapping::keys`], cíle sdílené klávesy v kanonickém pořadí
    /// [`Targets`] (ovladač, pak `Action::index`). Sdílená klávesa je
    /// tu tolikrát, kolik má cílů; stejný obsah dá vždy stejné pořadí.
    pub fn bindings(&self) -> impl Iterator<Item = (KeyId, PadAction)> + '_ {
        self.keys().flat_map(|(k, s)| s.iter().map(move |t| (k, t)))
    }

    /// Vazby jednoho ovladače (ve stejném pořadí jako [`Mapping::bindings`]).
    pub fn pad_bindings(&self, pad: PadId) -> impl Iterator<Item = (KeyId, Action)> + '_ {
        self.bindings()
            .filter(move |(_, t)| t.pad == pad)
            .map(|(k, t)| (k, t.action))
    }

    /// Počet vazeb (dvojic klávesa–cíl) všech ovladačů.
    pub fn len(&self) -> usize {
        self.keys.iter().map(|s| s.len()).sum()
    }

    /// Vždy `false` — prázdné mapování nejde vyrobit. Metoda je tu kvůli
    /// konvenci k `len()`.
    pub fn is_empty(&self) -> bool {
        self.keys.iter().all(|s| s.is_empty())
    }

    /// Ovládá některá klávesa víc vstupů? (`config.json` pak nese
    /// `verze` 2 — starší KeyPad by sdílenou klávesu vzal za chybu.)
    pub fn has_shared(&self) -> bool {
        self.keys.iter().any(|s| s.len() > 1)
    }

    /// Index klávesy, kterou jde přiřadit cíli (ne zkratka, ne Win, ne
    /// nemapovatelná).
    fn bindable(&self, key: KeyId, t: PadAction) -> Result<usize, MappingError> {
        if key == self.toggle {
            return Err(MappingError::ToggleKeyMapped { key, action: t });
        }
        key.index()
            .filter(|&i| i < KEY_TABLE_SIZE)
            .ok_or_else(|| not_mappable(key, Some(t)))
    }

    /// Nové cíle klávesy po přiřazení `t` a vstupy, které o ni přišly;
    /// `None` = pátý vstup sdílené klávesy.
    fn resolve(before: Targets, t: PadAction, conflict: KeyConflict) -> Option<(Targets, Targets)> {
        if before.contains(t) {
            // Klávesa už cíli patří: nic se nemění — ani bez volby se
            // sdílená klávesa neodebere vstupům, na které nikdo neklikl.
            return Some((before, Targets::EMPTY));
        }
        match conflict {
            KeyConflict::Move => Some((Targets::one(t), before)),
            KeyConflict::Share => {
                let mut s = before;
                s.insert(t).then_some((s, Targets::EMPTY))
            }
        }
    }

    /// Přiřadí klávesu akci ovladače a přidá ji k případným dalším
    /// klávesám té akce (`+` u čepičky). Patřila-li klávesa dosud jiným
    /// vstupům, `conflict` rozhodne, jestli se přesune, nebo sdílí.
    ///
    /// Všechno, nebo nic: chyby (zkratka, Win, nemapovatelná, pátý vstup)
    /// se hlásí před jakoukoli změnou. Nealokuje — volá se z hook callbacku.
    pub fn bind(
        &mut self,
        key: KeyId,
        t: PadAction,
        conflict: KeyConflict,
    ) -> Result<Rebind, MappingError> {
        let ki = self.bindable(key, t)?;
        let (new, moved) = Self::resolve(self.keys[ki], t, conflict)
            .ok_or(MappingError::TooManyTargets { key })?;
        self.keys[ki] = new;
        Ok(Rebind {
            moved,
            shared: new.len() - 1,
            removed: 0,
        })
    }

    /// Nahradí klávesy cíle: ostatní klávesy `t` o něj přijdou (sdílená
    /// klávesa si nechá své ostatní cíle) a `key` mu přiřadí jako
    /// [`Mapping::bind`] — klik na čepičku a stisk klávesy jako
    /// v nastavení her (Fáze 6, OQ 40).
    ///
    /// Všechno, nebo nic: chyby se hlásí před jakoukoli změnou (i pátý
    /// vstup — cíl pak o žádnou klávesu nepřijde). Prázdné mapování
    /// vzniknout nemůže — `key` po úspěchu vazbu má.
    ///
    /// Nealokuje — volá se z hook callbacku.
    pub fn bind_replacing(
        &mut self,
        key: KeyId,
        t: PadAction,
        conflict: KeyConflict,
    ) -> Result<Rebind, MappingError> {
        let ki = self.bindable(key, t)?;
        let (new, moved) = Self::resolve(self.keys[ki], t, conflict)
            .ok_or(MappingError::TooManyTargets { key })?;
        let mut removed = 0;
        for (i, s) in self.keys.iter_mut().enumerate() {
            if i != ki && s.remove(t) {
                removed += 1;
            }
        }
        self.keys[ki] = new;
        Ok(Rebind {
            moved,
            shared: new.len() - 1,
            removed,
        })
    }

    /// Bylo by mapování po odebrání cílů, které vybere `drop`, prázdné?
    fn empty_without(&self, drop: impl Fn(Targets) -> Targets) -> bool {
        self.keys.iter().all(|s| drop(*s).is_empty())
    }

    /// Vyprázdní vstup — odebere ho ze všech jeho kláves (sdílená klávesa
    /// si nechá ostatní cíle) a vrátí, kolik kláves ho měla (`Ok(0)` =
    /// vstup už byl prázdný). Všechno, nebo nic: jsou-li to poslední
    /// vazby celého mapování, [`MappingError::WouldBeEmpty`] a nic se
    /// neodebere.
    ///
    /// Celý vstup, ne „naposledy přidaná klávesa": tabulka pořadí přidání
    /// nezná.
    pub fn unbind_target(&mut self, t: PadAction) -> Result<usize, MappingError> {
        let n = self.keys_for(t).count();
        if n > 0 && self.empty_without(|s| s.minus(Targets::one(t))) {
            return Err(MappingError::WouldBeEmpty);
        }
        for s in &mut self.keys {
            s.remove(t);
        }
        Ok(n)
    }

    /// Mapování, ve kterém má první ovladač výchozí rozvržení (tlačítko
    /// „Výchozí klávesy").
    ///
    /// Cíle prvního ovladače zmizí ze všech kláves (sdílená klávesa si
    /// nechá cíle jiných ovladačů). Výchozí klávesu, která potom patří
    /// jinému ovladači nebo je zkratkou přepnutí, PŘESKOČÍ — vstup prvního
    /// ovladače zůstane prázdný (OQ 49). Brát klávesy jiným hráčům by bylo
    /// překvapení, o které nikdo nežádal, a sdílení ↺ nikdy nevytvoří
    /// (o to si uživatel neřekl). Ostatní ovladače i zkratka zůstávají
    /// beze změny.
    ///
    /// Prázdné být nemůže: buď mají klávesy jiné ovladače, nebo první
    /// dostane všechny výchozí kromě nejvýš zkratky.
    pub fn defaults_for_first_pad(&self) -> Mapping {
        let mut out = self.clone();
        for s in &mut out.keys {
            s.remove_pad(PadId::FIRST);
        }
        for (key, t) in Mapping::default().bindings() {
            if key == out.toggle {
                continue;
            }
            // Obsazená klávesa teď patří jen jiným ovladačům.
            if let Some(s) = key.index().and_then(|i| out.keys.get_mut(i)) {
                if s.is_empty() {
                    *s = Targets::one(t);
                }
            }
        }
        debug_assert!(!out.is_empty(), "výchozí klávesy daly prázdné mapování");
        out
    }

    /// Odebere všechny cíle klávesy a vrátí je. Poslední vazbu (celého
    /// mapování) odebrat nelze.
    pub fn unbind(&mut self, key: KeyId) -> Result<Targets, MappingError> {
        let s = self.targets(key);
        if s.is_empty() {
            return Err(MappingError::NotMapped { key });
        }
        if self.len() == s.len() {
            return Err(MappingError::WouldBeEmpty);
        }
        if let Some(slot) = key.index().and_then(|i| self.keys.get_mut(i)) {
            *slot = Targets::EMPTY;
        }
        Ok(s)
    }

    /// Odebere všechny vazby ovladače (ovladač zmizel z okna) a vrátí,
    /// kolik jich bylo; sdílená klávesa si nechá cíle jiných ovladačů.
    /// Všechno, nebo nic: patří-li ovladači všechny vazby, mapování by
    /// zůstalo prázdné → [`MappingError::WouldBeEmpty`] a nic se neodebere.
    pub fn clear_pad(&mut self, pad: PadId) -> Result<usize, MappingError> {
        let n = self.pad_bindings(pad).count();
        let without = |mut s: Targets| {
            s.remove_pad(pad);
            s
        };
        if n > 0 && self.empty_without(without) {
            return Err(MappingError::WouldBeEmpty);
        }
        for s in &mut self.keys {
            s.remove_pad(pad);
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
        if let Some(action) = self.targets(key).first() {
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
    use KeyConflict::{Move, Share};

    const UP: PadAction = PadAction::first(Action::LeftStick(StickDir::Up));
    const A_BTN: PadAction = PadAction::first(Action::Button(PadButton::A));
    const X_BTN: PadAction = PadAction::first(Action::Button(PadButton::X));
    const P1: PadId = PadId::ALL[1];

    fn na(pad: PadId, t: PadAction) -> PadAction {
        PadAction::new(pad, t.action)
    }

    fn cile(t: &[PadAction]) -> Targets {
        Targets::from_targets(t.iter().copied()).unwrap()
    }

    fn rebind(moved: &[PadAction], shared: usize, removed: usize) -> Rebind {
        Rebind {
            moved: cile(moved),
            shared,
            removed,
        }
    }

    #[test]
    fn vychozi_mapovani_je_platne() {
        let d = Mapping::default();
        let znovu = Mapping::new(d.toggle_key(), d.bindings()).expect("výchozí je platné");
        assert_eq!(znovu, d);
        assert_eq!(d.len(), 24);
        assert!(!d.has_shared());
        assert_eq!(d.toggle_key(), KeyId::SCROLL_LOCK);
        assert_eq!(d.targets(KeyId::W), Targets::one(UP));
        assert!(d.targets(KeyId::NUMPAD_8).is_empty());
        assert_eq!(
            d.targets(KeyId::ARROW_UP),
            Targets::one(PadAction::first(Action::RightStick(StickDir::Up)))
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
        assert_eq!(d.keys().count(), 24);
    }

    /// Fáze 7: tatáž klávesa u víc vstupů (i jiných ovladačů) je sdílená
    /// klávesa — platná. Tatáž dvojice dvakrát se sloučí.
    #[test]
    fn sdilena_klavesa_je_platna() {
        let m = Mapping::new(
            KeyId::SCROLL_LOCK,
            [
                (KeyId::W, UP),
                (KeyId::W, A_BTN),
                (KeyId::W, na(P1, UP)),
                (KeyId::W, UP),
            ],
        )
        .unwrap();
        assert_eq!(m.targets(KeyId::W), cile(&[UP, A_BTN, na(P1, UP)]));
        assert_eq!(m.len(), 3);
        assert!(m.has_shared());
        assert_eq!(m.keys().count(), 1);
        assert_eq!(
            m.bindings().collect::<Vec<_>>(),
            vec![(KeyId::W, UP), (KeyId::W, A_BTN), (KeyId::W, na(P1, UP))],
            "kanonické pořadí: ovladač, pak Action::index"
        );
        assert_eq!(m.keys_for(A_BTN).collect::<Vec<_>>(), vec![KeyId::W]);
        assert_eq!(
            m.pad_bindings(P1).collect::<Vec<_>>(),
            vec![(KeyId::W, UP.action)]
        );
    }

    /// Tatáž množina vazeb v jiném pořadí = totéž mapování (stejná
    /// revize, stejný text v config.json).
    #[test]
    fn poradi_vazeb_nezalezi() {
        let vazby = [
            (KeyId::W, na(P1, A_BTN)),
            (KeyId::W, UP),
            (KeyId::F, X_BTN),
            (KeyId::W, na(PadId::ALL[3], UP)),
        ];
        let a = Mapping::new(KeyId::SCROLL_LOCK, vazby).unwrap();
        let mut obracene = vazby;
        obracene.reverse();
        let b = Mapping::new(KeyId::SCROLL_LOCK, obracene).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            a.bindings().collect::<Vec<_>>(),
            b.bindings().collect::<Vec<_>>()
        );
    }

    #[test]
    fn paty_vstup_klavesy_se_zamitne() {
        let pet = [
            (KeyId::W, UP),
            (KeyId::W, A_BTN),
            (KeyId::W, na(P1, UP)),
            (KeyId::W, na(PadId::ALL[2], UP)),
            (KeyId::W, na(PadId::ALL[3], UP)),
            (KeyId::W, X_BTN),
            (KeyId::F, X_BTN),
        ];
        let e = Mapping::new(KeyId::SCROLL_LOCK, pet).unwrap_err();
        assert_eq!(
            e,
            vec![MappingError::TooManyTargets { key: KeyId::W }],
            "jednou za klávesu"
        );
        assert!(e[0].to_string().contains("víc než 4"), "{}", e[0]);
        // Čtyři jsou v pořádku.
        assert!(Mapping::new(KeyId::SCROLL_LOCK, pet[..4].iter().copied()).is_ok());
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
                (KeyId::SCROLL_LOCK, UP),
                (KeyId::W, UP),
                (KeyId::W, A_BTN),
                (KeyId::ESC, A_BTN),
            ],
        )
        .unwrap_err();
        assert_eq!(e.len(), 3, "{e:?}");
        assert!(e.contains(&MappingError::ToggleIsEscape));
        assert!(e.contains(&MappingError::Unmappable {
            key: KeyId::ALTGR_FAKE_CTRL,
            action: Some(UP)
        }));
        assert!(e.contains(&MappingError::ToggleKeyMapped {
            key: KeyId::ESC,
            action: A_BTN
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
        for k in [Move, Share] {
            assert_eq!(
                m.bind(KeyId::RIGHT_WIN, na(P1, A_BTN), k),
                Err(MappingError::Reserved {
                    key: KeyId::RIGHT_WIN,
                    action: Some(na(P1, A_BTN))
                })
            );
        }
        assert_eq!(
            m.set_toggle_key(KeyId::LEFT_WIN),
            Err(MappingError::Reserved {
                key: KeyId::LEFT_WIN,
                action: None
            })
        );
        assert_eq!(m, pred, "neúspěch nic nezměnil");
        assert!(m.targets(KeyId::LEFT_WIN).is_empty());
        // Alt Windows nepatří — mapovat jde (okno jen varuje).
        assert_eq!(m.bind(KeyId::LEFT_ALT, UP, Move), Ok(rebind(&[], 0, 0)));
    }

    #[test]
    fn bind_presune_klavesu_z_jine_akce() {
        let mut m = Mapping::default();
        assert_eq!(m.bind(KeyId::W, A_BTN, Move), Ok(rebind(&[UP], 0, 0)));
        assert_eq!(m.targets(KeyId::W), Targets::one(A_BTN));
        // Mezerník zůstává u A — akce má teď dvě klávesy.
        assert_eq!(m.keys_for(A_BTN).count(), 2);
        // Znovu totéž = nic se nepřesouvá.
        assert_eq!(m.bind(KeyId::W, A_BTN, Move), Ok(rebind(&[], 0, 0)));
    }

    #[test]
    fn bind_presune_klavesu_na_jiny_ovladac() {
        let mut m = Mapping::default();
        // Tatáž akce na druhém ovladači: bez volby se klávesa přesune.
        assert_eq!(m.bind(KeyId::W, na(P1, UP), Move), Ok(rebind(&[UP], 0, 0)));
        assert_eq!(m.targets(KeyId::W), Targets::one(na(P1, UP)));
        assert_eq!(m.keys_for(UP).count(), 0, "první ovladač o W přišel");
        assert_eq!(
            m.pad_bindings(P1).collect::<Vec<_>>(),
            vec![(KeyId::W, UP.action)]
        );
        assert_eq!(m.len(), 24, "přesun nic nepřidal");
        // A zpátky na jinou akci prvního ovladače.
        assert_eq!(
            m.bind(KeyId::W, A_BTN, Move),
            Ok(rebind(&[na(P1, UP)], 0, 0))
        );
        assert_eq!(m.pad_bindings(P1).count(), 0);
    }

    /// S volbou se klávesa sdílí: nikdo nic neztratí.
    #[test]
    fn bind_se_sdilenim() {
        let mut m = Mapping::default();
        assert_eq!(
            m.bind(KeyId::W, na(P1, A_BTN), Share),
            Ok(rebind(&[], 1, 0))
        );
        assert_eq!(m.targets(KeyId::W), cile(&[UP, na(P1, A_BTN)]));
        assert_eq!(m.keys_for(UP).collect::<Vec<_>>(), vec![KeyId::W]);
        assert_eq!(m.len(), 25);
        // Třetí a čtvrtý vstup.
        assert_eq!(m.bind(KeyId::W, X_BTN, Share), Ok(rebind(&[], 2, 0)));
        assert_eq!(
            m.bind(KeyId::W, na(PadId::ALL[3], UP), Share),
            Ok(rebind(&[], 3, 0))
        );
        // Pátý ne — a nic se nezmění.
        let pred = m.clone();
        assert_eq!(
            m.bind(KeyId::W, na(P1, UP), Share),
            Err(MappingError::TooManyTargets { key: KeyId::W })
        );
        assert_eq!(m, pred);
        // Už patří: beze změny, počet dalších zůstává.
        assert_eq!(m.bind(KeyId::W, X_BTN, Share), Ok(rebind(&[], 3, 0)));
        assert_eq!(m, pred);
        // Bez volby: klávesa, která cíli už patří, zůstane sdílená
        // (neodebere se vstupům, na které nikdo neklikl).
        assert_eq!(m.bind(KeyId::W, X_BTN, Move), Ok(rebind(&[], 3, 0)));
        assert_eq!(m, pred);
        // Bez volby jinam: všechny čtyři o ni přijdou.
        assert_eq!(
            m.bind(KeyId::W, na(P1, UP), Move),
            Ok(rebind(
                &[UP, X_BTN, na(P1, A_BTN), na(PadId::ALL[3], UP)],
                0,
                0
            ))
        );
        assert_eq!(m.targets(KeyId::W), Targets::one(na(P1, UP)));
    }

    #[test]
    fn bind_odmitne_zkratku_a_nemapovatelne() {
        let mut m = Mapping::default();
        let pred = m.clone();
        for k in [Move, Share] {
            assert!(matches!(
                m.bind(KeyId::SCROLL_LOCK, UP, k),
                Err(MappingError::ToggleKeyMapped { .. })
            ));
            assert!(matches!(
                m.bind(KeyId::ALTGR_FAKE_CTRL, UP, k),
                Err(MappingError::Unmappable { .. })
            ));
            assert!(matches!(
                m.bind(KeyId::new(0), na(P1, UP), k),
                Err(MappingError::Unmappable { .. })
            ));
        }
        assert_eq!(m, pred, "neúspěch nic nezměnil");
    }

    #[test]
    fn nahrazeni_necha_jen_novou_klavesu() {
        // A má mezerník a X; nahrazení F nechá jen F.
        let mut m = Mapping::default();
        m.bind(KeyId::X, A_BTN, Move).unwrap();
        assert_eq!(m.keys_for(A_BTN).count(), 2);
        assert_eq!(
            m.bind_replacing(KeyId::F, A_BTN, Move),
            Ok(rebind(&[X_BTN], 0, 2))
        );
        assert_eq!(m.keys_for(A_BTN).collect::<Vec<_>>(), vec![KeyId::F]);
        assert_eq!(m.keys_for(X_BTN).count(), 0, "F z tlačítka X odešlo");
        assert!(m.targets(KeyId::SPACE).is_empty());
        assert!(m.targets(KeyId::X).is_empty());
        assert_eq!(m.len(), 23, "24 + X − mezerník − X");
        let platne = Mapping::new(m.toggle_key(), m.bindings()).expect("platné");
        assert_eq!(platne, m);
    }

    /// Nahrazení se sdílením: ostatní klávesy cíle o něj přijdou, sdílená
    /// klávesa si nechá ostatní cíle a nová klávesa se sdílí.
    #[test]
    fn nahrazeni_se_sdilenim() {
        let mut m = Mapping::default();
        // X sdílí A prvního a A druhého ovladače.
        m.bind(KeyId::X, A_BTN, Move).unwrap();
        m.bind(KeyId::X, na(P1, A_BTN), Share).unwrap();
        // F (tlačítko X) dostane A prvního: mezerník i X o A přijdou,
        // X zůstane druhému ovladači, F se sdílí s tlačítkem X.
        assert_eq!(
            m.bind_replacing(KeyId::F, A_BTN, Share),
            Ok(rebind(&[], 1, 2))
        );
        assert_eq!(m.targets(KeyId::F), cile(&[A_BTN, X_BTN]));
        assert!(m.targets(KeyId::SPACE).is_empty());
        assert_eq!(m.targets(KeyId::X), Targets::one(na(P1, A_BTN)));
        // Pátý vstup se odmítne dřív, než cíl přijde o klávesy.
        let mut plna = Mapping::new(
            KeyId::SCROLL_LOCK,
            [
                (KeyId::W, UP),
                (KeyId::W, A_BTN),
                (KeyId::W, na(P1, UP)),
                (KeyId::W, na(P1, A_BTN)),
                (KeyId::F, X_BTN),
            ],
        )
        .unwrap();
        let pred = plna.clone();
        assert_eq!(
            plna.bind_replacing(KeyId::W, X_BTN, Share),
            Err(MappingError::TooManyTargets { key: KeyId::W })
        );
        assert_eq!(plna, pred, "F zůstalo tlačítku X");
    }

    #[test]
    fn pridani_necha_vsechny_klavesy() {
        let mut m = Mapping::default();
        m.bind(KeyId::X, A_BTN, Move).unwrap();
        assert_eq!(m.bind(KeyId::NUMPAD_8, A_BTN, Move), Ok(rebind(&[], 0, 0)));
        assert_eq!(
            m.keys_for(A_BTN).collect::<Vec<_>>(),
            vec![KeyId::X, KeyId::SPACE, KeyId::NUMPAD_8]
        );
    }

    #[test]
    fn nahrazeni_touz_klavesou() {
        // Klávesa už cíli patří: odeberou se jen ostatní, nic se nepřesouvá.
        let mut m = Mapping::default();
        m.bind(KeyId::X, A_BTN, Move).unwrap();
        assert_eq!(
            m.bind_replacing(KeyId::SPACE, A_BTN, Move),
            Ok(rebind(&[], 0, 1))
        );
        assert_eq!(m.keys_for(A_BTN).collect::<Vec<_>>(), vec![KeyId::SPACE]);
        // Jediná klávesa cíle: nic se nemění.
        let pred = m.clone();
        assert_eq!(
            m.bind_replacing(KeyId::SPACE, A_BTN, Move),
            Ok(rebind(&[], 0, 0))
        );
        assert_eq!(m, pred);
        // Prázdný cíl: nic se neodebírá.
        let lt2 = PadAction::new(P1, Action::LeftTrigger);
        assert_eq!(
            m.bind_replacing(KeyId::NUMPAD_8, lt2, Move),
            Ok(rebind(&[], 0, 0))
        );
        assert_eq!(m.targets(KeyId::NUMPAD_8), Targets::one(lt2));
    }

    #[test]
    fn nahrazeni_presune_klavesu_mezi_ovladaci() {
        let mut m = Mapping::default();
        m.bind(KeyId::X, na(P1, UP), Move).unwrap();
        // W z prvního ovladače nahradí X u druhého.
        assert_eq!(
            m.bind_replacing(KeyId::W, na(P1, UP), Move),
            Ok(rebind(&[UP], 0, 1))
        );
        assert_eq!(
            m.pad_bindings(P1).collect::<Vec<_>>(),
            vec![(KeyId::W, UP.action)]
        );
        assert_eq!(m.keys_for(UP).count(), 0, "první ovladač o W přišel");
        assert!(m.targets(KeyId::X).is_empty());
    }

    #[test]
    fn nahrazeni_s_chybou_nic_nezmeni() {
        let mut m = Mapping::default();
        m.bind(KeyId::X, A_BTN, Move).unwrap();
        let pred = m.clone();
        assert_eq!(
            m.bind_replacing(KeyId::SCROLL_LOCK, A_BTN, Move),
            Err(MappingError::ToggleKeyMapped {
                key: KeyId::SCROLL_LOCK,
                action: A_BTN
            })
        );
        for win in [KeyId::LEFT_WIN, KeyId::RIGHT_WIN] {
            assert_eq!(
                m.bind_replacing(win, A_BTN, Share),
                Err(MappingError::Reserved {
                    key: win,
                    action: Some(A_BTN)
                })
            );
        }
        for k in [KeyId::ALTGR_FAKE_CTRL, KeyId::new(0), KeyId::new(0x80)] {
            assert_eq!(
                m.bind_replacing(k, A_BTN, Move),
                Err(MappingError::Unmappable {
                    key: k,
                    action: Some(A_BTN)
                })
            );
        }
        assert_eq!(m, pred, "A má dál obě klávesy");
    }

    #[test]
    fn vyprazdneni_vstupu() {
        let mut m = Mapping::default();
        m.bind(KeyId::X, A_BTN, Move).unwrap();
        assert_eq!(m.unbind_target(A_BTN), Ok(2));
        assert_eq!(m.keys_for(A_BTN).count(), 0);
        assert!(m.targets(KeyId::SPACE).is_empty());
        assert!(m.targets(KeyId::X).is_empty());
        assert_eq!(m.len(), 23);
        // Prázdný vstup: nic, žádná chyba.
        let pred = m.clone();
        assert_eq!(m.unbind_target(A_BTN), Ok(0));
        assert_eq!(m.unbind_target(na(P1, UP)), Ok(0));
        assert_eq!(m, pred);
    }

    /// Vyprázdnění vstupu sdílené klávesy: klávesa zůstane ostatním.
    #[test]
    fn vyprazdneni_vstupu_sdilene_klavesy() {
        let mut m = Mapping::default();
        m.bind(KeyId::W, na(P1, UP), Share).unwrap();
        assert_eq!(m.unbind_target(UP), Ok(1));
        assert_eq!(m.targets(KeyId::W), Targets::one(na(P1, UP)));
        assert!(!m.has_shared(), "s jedním cílem už není sdílená");
        // Poslední cíl sdílené klávesy, která je poslední vazbou celého
        // mapování: nejde (všechno, nebo nic).
        let mut m = Mapping::new(KeyId::SCROLL_LOCK, [(KeyId::W, UP), (KeyId::W, A_BTN)]).unwrap();
        assert_eq!(m.unbind_target(UP), Ok(1));
        let pred = m.clone();
        assert_eq!(m.unbind_target(A_BTN), Err(MappingError::WouldBeEmpty));
        assert_eq!(m, pred);
    }

    #[test]
    fn vyprazdneni_poslednich_vazeb_se_odmitne() {
        let mut m = Mapping::new(
            KeyId::SCROLL_LOCK,
            [(KeyId::W, na(P1, UP)), (KeyId::I, na(P1, UP))],
        )
        .unwrap();
        let pred = m.clone();
        assert_eq!(m.unbind_target(na(P1, UP)), Err(MappingError::WouldBeEmpty));
        assert_eq!(m, pred, "všechno, nebo nic");
        // Tatáž akce na jiném ovladači je jiný vstup.
        assert_eq!(m.unbind_target(UP), Ok(0));
    }

    #[test]
    fn vychozi_pro_prvni_ovladac_nebere_klavesy_jinym() {
        let mut m = Mapping::default();
        // Druhý ovladač má W (z ls_up prvního) a X; první si mezitím
        // přemapoval mezerník na B.
        m.bind(KeyId::W, na(P1, UP), Move).unwrap();
        m.bind(KeyId::X, na(P1, A_BTN), Move).unwrap();
        m.bind(
            KeyId::SPACE,
            PadAction::first(Action::Button(PadButton::B)),
            Move,
        )
        .unwrap();
        let d = m.defaults_for_first_pad();
        assert_eq!(
            d.targets(KeyId::W),
            Targets::one(na(P1, UP)),
            "W zůstane druhému"
        );
        assert_eq!(d.keys_for(UP).count(), 0, "ls_up prvního zůstane prázdné");
        assert_eq!(d.targets(KeyId::X), Targets::one(na(P1, A_BTN)));
        assert_eq!(
            d.targets(KeyId::SPACE),
            Targets::one(A_BTN),
            "mezerník zpět na A"
        );
        assert_eq!(
            d.keys_for(PadAction::first(Action::Button(PadButton::B)))
                .count(),
            1
        );
        assert_eq!(d.pad_bindings(PadId::FIRST).count(), 23);
        assert_eq!(d.pad_bindings(P1).count(), 2, "druhý beze změny");
        assert_eq!(d.toggle_key(), m.toggle_key());
        assert_eq!(Mapping::new(d.toggle_key(), d.bindings()).unwrap(), d);
    }

    /// ↺ sdílení nevytvoří: výchozí klávesa, kterou sdílí první ovladač
    /// s druhým, zůstane po odebrání cílů prvního jen druhému a vstup
    /// prvního zůstane prázdný. Sdílení mezi prvním a druhým se rozpadne.
    #[test]
    fn vychozi_pro_prvni_ovladac_sdileni_nevytvori() {
        let mut m = Mapping::default();
        m.bind(KeyId::W, na(P1, UP), Share).unwrap();
        m.bind(KeyId::X, A_BTN, Share).unwrap();
        m.bind(KeyId::X, na(P1, A_BTN), Share).unwrap();
        let d = m.defaults_for_first_pad();
        assert_eq!(d.targets(KeyId::W), Targets::one(na(P1, UP)));
        assert_eq!(d.keys_for(UP).count(), 0);
        assert_eq!(d.targets(KeyId::X), Targets::one(na(P1, A_BTN)));
        assert_eq!(d.targets(KeyId::SPACE), Targets::one(A_BTN));
        assert!(!d.has_shared());
    }

    #[test]
    fn vychozi_pro_prvni_ovladac_preskoci_zkratku() {
        // Zkratka na F (výchozí klávesa tlačítka X): X zůstane prázdné.
        let mut m = Mapping::default();
        m.unbind(KeyId::F).unwrap();
        m.set_toggle_key(KeyId::F).unwrap();
        m.bind(KeyId::NUMPAD_8, A_BTN, Move).unwrap();
        let d = m.defaults_for_first_pad();
        assert_eq!(d.toggle_key(), KeyId::F);
        assert!(d.targets(KeyId::F).is_empty());
        assert_eq!(d.keys_for(X_BTN).count(), 0);
        assert!(
            d.targets(KeyId::NUMPAD_8).is_empty(),
            "vlastní klávesa prvního pryč"
        );
        assert_eq!(d.len(), 23);
        assert_eq!(Mapping::new(d.toggle_key(), d.bindings()).unwrap(), d);
        // Na výchozím mapování nic nemění.
        assert_eq!(
            Mapping::default().defaults_for_first_pad(),
            Mapping::default()
        );
    }

    #[test]
    fn vychozi_pro_prvni_ovladac_neni_nikdy_prazdne() {
        // Všechny výchozí klávesy má druhý ovladač: první zůstane prázdný,
        // druhý beze změny.
        let d = Mapping::default();
        let m = Mapping::new(
            KeyId::SCROLL_LOCK,
            d.bindings().map(|(k, t)| (k, na(P1, t))),
        )
        .unwrap();
        let v = m.defaults_for_first_pad();
        assert_eq!(v, m);
        assert_eq!(v.pad_bindings(PadId::FIRST).count(), 0);
        assert!(!v.is_empty());
        // Jen první ovladač, s jedinou cizí klávesou: dostane výchozí.
        let m = Mapping::new(KeyId::SCROLL_LOCK, [(KeyId::NUMPAD_8, UP)]).unwrap();
        assert_eq!(m.defaults_for_first_pad(), Mapping::default());
    }

    #[test]
    fn posledni_vazbu_nelze_odebrat() {
        let mut m = Mapping::new(
            KeyId::SCROLL_LOCK,
            [(KeyId::W, UP), (KeyId::A, na(P1, A_BTN))],
        )
        .unwrap();
        assert_eq!(m.unbind(KeyId::A), Ok(Targets::one(na(P1, A_BTN))));
        assert_eq!(
            m.unbind(KeyId::A),
            Err(MappingError::NotMapped { key: KeyId::A })
        );
        assert_eq!(m.unbind(KeyId::W), Err(MappingError::WouldBeEmpty));
        assert_eq!(m.len(), 1);
        // Sdílená poslední klávesa: taky ne.
        let mut m = Mapping::new(KeyId::SCROLL_LOCK, [(KeyId::W, UP), (KeyId::W, A_BTN)]).unwrap();
        assert_eq!(m.unbind(KeyId::W), Err(MappingError::WouldBeEmpty));
    }

    #[test]
    fn clear_pad_odebere_jen_jeho_vazby() {
        let mut m = Mapping::default();
        m.bind(KeyId::X, na(P1, A_BTN), Move).unwrap();
        m.bind(KeyId::W, na(P1, UP), Move).unwrap();
        assert_eq!(m.clear_pad(P1), Ok(2));
        assert_eq!(m.pad_bindings(P1).count(), 0);
        assert!(m.targets(KeyId::W).is_empty(), "přesunutá klávesa je pryč");
        assert_eq!(m.len(), 23, "první ovladač beze změny");
        // Ovladač bez kláves: nic k odebrání, žádná chyba.
        assert_eq!(m.clear_pad(P1), Ok(0));
        assert_eq!(m.clear_pad(PadId::ALL[3]), Ok(0));
    }

    /// 🗑 u sdílené klávesy: klávesa zůstane ostatním ovladačům.
    #[test]
    fn clear_pad_necha_sdilene_klavese_ostatni_cile() {
        let mut m = Mapping::default();
        m.bind(KeyId::W, na(P1, UP), Share).unwrap();
        m.bind(KeyId::W, na(P1, A_BTN), Share).unwrap();
        assert_eq!(m.clear_pad(P1), Ok(2));
        assert_eq!(m.targets(KeyId::W), Targets::one(UP));
        assert_eq!(m, Mapping::default());
    }

    #[test]
    fn clear_pad_nevyprazdni_mapovani() {
        let mut m = Mapping::default();
        let pred = m.clone();
        assert_eq!(m.clear_pad(PadId::FIRST), Err(MappingError::WouldBeEmpty));
        assert_eq!(m, pred, "všechno, nebo nic");
        // Když má klávesy i jiný ovladač, první jde vyprázdnit.
        m.bind(KeyId::X, na(P1, A_BTN), Move).unwrap();
        assert_eq!(m.clear_pad(PadId::FIRST), Ok(24));
        assert_eq!(
            m.bindings().collect::<Vec<_>>(),
            vec![(KeyId::X, na(P1, A_BTN))]
        );
        // Sdílená klávesa jen prvního a druhého: druhý ji udrží.
        let mut m =
            Mapping::new(KeyId::SCROLL_LOCK, [(KeyId::W, UP), (KeyId::W, na(P1, UP))]).unwrap();
        assert_eq!(m.clear_pad(PadId::FIRST), Ok(1));
        assert_eq!(m.clear_pad(P1), Err(MappingError::WouldBeEmpty));
    }

    #[test]
    fn zmena_zkratky_se_validuje() {
        let mut m = Mapping::default();
        m.bind(KeyId::X, na(P1, A_BTN), Move).unwrap();
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

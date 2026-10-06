//! Engine — čistý stavový automat: události dovnitř, [`Decision`] ven.
//!
//! Žádné I/O, žádné časovače, žádná vlákna. Čas dostává zvenku jako
//! parametr `now_ms` — monotónní milisekundy od libovolného počátku.
//! Hook vlákno (Fáze 3) předá čas události (`KBDLLHOOKSTRUCT::time`
//! rozšířený na 64 bitů) nebo `GetTickCount64()`; 32bitový čas sám po
//! 49 dnech přeteče. Čas couvající dozadu nic nerozbije (vše přes
//! `saturating_sub`), jen se tím nic nezpozdí ani neurychlí.
//!
//! Engine vlastní hook vlákno a volá ho z hook callbacku, takže každá
//! metoda musí být rychlá a nesmí blokovat (princip 3): držené klávesy
//! i mapování jsou pevné tabulky o 256 položkách, stavy ovladačů pevné
//! pole o [`MAX_PADS`] položkách, nic se nehashuje a za běhu nic
//! nealokuje.
//!
//! Jádrem je **pravidlo vlastnictví klávesy** (princip 2): kam patří
//! stisk, se rozhoduje JEDNOU při key-down a key-up (i autorepeat) jde
//! vždy stejnému vlastníkovi. Díky tomu přepnutí režimu ani výpadek
//! jednoho z ovladačů nikdy nezasekne klávesu ani páčku — OS dostane
//! key-up právě ke key-downům, které viděl, a ovladač ztratí jen
//! klávesy, které držel on.
//!
//! Víc ovladačů (Fáze 4): každý ovladač je zvlášť „připravený"
//! (připojený) nebo ne. Režim je ale jeden pro všechny — zkratka pozastaví
//! zachytávání celé klávesnice najednou, jinak by se do chatu psát
//! nedalo.
//!
//! Sdílené klávesy (Fáze 7): klávesa smí ovládat až
//! [`crate::MAX_TARGETS_PER_KEY`] vstupů i různých ovladačů ([`Targets`]).
//! Vlastníkem stisku je pak MNOŽINA cílů na ovladačích připravených při
//! key-down a key-up jde téže množině (princip 2 pro množinu); stav
//! každého ovladače se dál počítá celý z držených kláves (princip 7).
//!
//! Editor (Fáze 6) mění mapování i za hry ([`Engine::replace_mapping`])
//! bez pozastavení — jde to jen díky pravidlu vlastnictví: výměna mění
//! mapování, ne vlastníky, takže držená klávesa dohraje se starou akcí
//! a nový stisk se řídí novým mapováním.

use serde::{Deserialize, Serialize};

use crate::action::{Action, ActionSet, PadAction, PadId, MAX_PADS};
use crate::key::{KeyId, KEY_TABLE_SIZE};
use crate::mapping::{KeyConflict, Mapping, MappingError, Rebind};
use crate::pad_state::{compute_pad_state, live_from, LiveInputs, PadState, PadUpdates};
use crate::targets::Targets;

/// Po jaké době se přiřazování klávesy samo zruší.
pub const BINDING_TIMEOUT_MS: u64 = 10_000;

/// Po jaké pauze se key-down už držené (spolknuté) klávesy nebere jako
/// autorepeat, ale jako nový stisk po ztraceném key-upu.
///
/// Key-up se ztratí, když vstup na chvíli převezme zabezpečená plocha
/// (výzva UAC, Ctrl+Alt+Del) — LL hook ho tam nevidí — nebo když
/// Windows hook potichu odeberou. Bez tohohle pravidla by zbylý záznam
/// spolkl celý další stisk, případně první stisk zkratky nic nepřepnul.
///
/// Autorepeat přichází nejpozději po prodlevě opakování (nejvíc 1 s
/// v Nastavení Windows) a pak každých ≤ 400 ms; opakuje se jen naposledy
/// stisknutá klávesa a starší se po jejím uvolnění znovu nerozjede. Key-
/// down po pauze delší než 1,5 s tedy u opravdu držené klávesy nenastane.
///
/// Platí jen pro klávesy, které OS neviděl (`Pad`, `Swallow`). U kláves
/// patřících OS by „nový stisk" mohl OS nechat s key-downem bez key-upu,
/// takže tam zůstává pokračování — nanejvýš jeden úhoz navíc do OS.
pub const STALE_KEY_MS: u64 = 1_500;

/// Proč nejde zachytávat (žádný ovladač není připravený).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DisabledReason {
    /// Ovladač ViGEmBus není nainstalovaný.
    ViGEmMissing,
    /// Virtuální pad se ještě nepřipojil (start aplikace, nové připojení)
    /// nebo ho uživatel vypnul.
    PadNotConnected,
    /// Posílání stavu do ViGEm selhalo — čeká se na „Zkusit znovu".
    PadError,
}

/// Režim aplikace. Jeden pro všechny ovladače.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mode {
    /// Zachytávání pozastavené: vše jde do OS (výchozí a fail-safe
    /// režim). Připravené ovladače zůstávají připojené a neutrální.
    Keyboard,
    /// Zachytávání: klávesy připravených ovladačů je ovládají, ostatní
    /// (i klávesy nepřipravených ovladačů) jdou do OS.
    Gamepad,
    /// Čeká se na klávesu, která se přiřadí akci `target`. Jde se sem
    /// z kteréhokoli režimu (klávesy jde upravovat i za hry — Fáze 6)
    /// a po skončení se vrací: bez připraveného ovladače do Disabled,
    /// jinak na Gamepad, pokud se zachytávalo (nebo o to okno mezitím
    /// požádalo), jinak na Klávesnici.
    Binding {
        target: BindTarget,
        started_at_ms: u64,
    },
    /// Jako Klávesnice, ale zachytávat nejde: žádný ovladač není
    /// připravený.
    Disabled { reason: DisabledReason },
}

/// Komu patří stisknutá klávesa. Určí se při key-down a už se nemění —
/// s jedinou výjimkou: `Pad` se zmenšuje a nakonec mění na `Swallow`,
/// když se přestane zachytávat nebo když některý jeho ovladač vypadne
/// (OS key-down nikdy neviděl, tak nesmí dostat ani key-up).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Owner {
    /// Klávesa jde do OS.
    Os,
    /// Klávesa ovládá ovladače; nese cíle (akce a ovladače) určené při
    /// stisku — u sdílené klávesy všechny její cíle na připravených
    /// ovladačích, nikdy prázdné. Výpadek ovladače z nich jeho cíle
    /// odebere; nová vazba ani nově připravený ovladač je nemění.
    Pad(Targets),
    /// Klávesa nejde nikam (zkratka přepnutí, přiřazování, klávesa
    /// ovladače po konci zachytávání nebo po jeho výpadku).
    Swallow,
}

impl Owner {
    /// Potlačit událost před OS? Právě tehdy, když vlastníkem není OS.
    pub fn suppresses(self) -> bool {
        self != Owner::Os
    }
}

/// Záznam o držené klávese.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HeldKey {
    pub owner: Owner,
    /// Pořadí stisku (roste s každým novým stiskem) — pro SOCD.
    pub seq: u64,
    /// Čas poslední události klávesy (stisk nebo autorepeat) — pro
    /// poznání ztraceného key-upu ([`STALE_KEY_MS`]).
    pub last_ms: u64,
}

/// Proč se režim změnil.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModeCause {
    /// Zkratka přepnutí (Scroll Lock).
    Hotkey,
    /// Tlačítko nebo jiný příkaz z okna.
    Gui,
    /// Vynucený návrat na Klávesnici (fail-safe).
    Forced(ForceReason),
    /// Ovladač se stal (ne)dostupným — [`Engine::disable`] /
    /// [`Engine::enable`].
    PadStatus,
}

/// Důvod vynucení režimu Klávesnice. Engine je jen předává dál do UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ForceReason {
    /// Pad vlákno přestalo dávat známky života (Fáze 5).
    Watchdog,
    /// Chyba při posílání stavu do ViGEm (Fáze 2).
    PadError,
    /// Zamčení relace (Win+L) nebo přepnutí uživatele.
    SessionLock,
    /// Přepnutí na jinou plochu (výzva UAC, Ctrl+Alt+Del) — key-upy tam
    /// hook nevidí.
    DesktopSwitch,
    /// Uspání počítače.
    Suspend,
    /// Panika v hook callbacku.
    HookPanic,
    /// Hook byl znovu nainstalován (Windows ho mohli potichu odebrat
    /// a mezitím se ztratily události).
    HookReinstalled,
    /// Změna mapování (pad nesmí zůstat s vazbami, které už neplatí).
    MappingChanged,
    /// Zavírání aplikace.
    Shutdown,
}

impl ForceReason {
    /// Všechny důvody v pořadí [`ForceReason::index`].
    pub const ALL: [ForceReason; 9] = [
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

    /// Pozice v [`ForceReason::ALL`] — malé číslo do atomiků hook vlákna
    /// ([`UiEvent::pack`]). Výčtem, ne `as usize`, ať přeskládání variant
    /// potichu nepřečísluje; shodu s `ALL` hlídá test.
    pub const fn index(self) -> usize {
        match self {
            ForceReason::Watchdog => 0,
            ForceReason::PadError => 1,
            ForceReason::SessionLock => 2,
            ForceReason::DesktopSwitch => 3,
            ForceReason::Suspend => 4,
            ForceReason::HookPanic => 5,
            ForceReason::HookReinstalled => 6,
            ForceReason::MappingChanged => 7,
            ForceReason::Shutdown => 8,
        }
    }
}

/// Co udělá klávesa stisknutá při přiřazování s ostatními klávesami
/// vstupu.
///
/// Pole enginu, ne součást [`Mode::Binding`]: `Mode` se serializuje do
/// okna a celý zbytek aplikace ho rozebírá — druh přiřazování zajímá jen
/// uložení klávesy ([`Engine::binding_kind`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BindKind {
    /// Klávesa vstup převezme a jeho ostatní klávesy se odeberou (klik
    /// na čepičku — jako v nastavení her, OQ 40).
    Replace,
    /// Klávesa se přidá k ostatním klávesám vstupu (`+` u čepičky).
    Add,
}

/// Co se přiřazuje: vstup ovladače, nebo zkratka pozastavení (Fáze 7, Z6).
///
/// Zkratka jde stejným režimem `Binding` (pozastaví hru, 10 s, Esc, vrací
/// se, odkud začal) — jen stisk se vyhodnotí jinak: zkratkou smí být jen
/// klávesa, kterou nic jiného nepotřebuje ([`KeyId::is_toggle_candidate`]),
/// a nesmí mít cíl.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BindTarget {
    Input(PadAction),
    Toggle,
}

impl From<PadAction> for BindTarget {
    fn from(t: PadAction) -> BindTarget {
        BindTarget::Input(t)
    }
}

impl BindTarget {
    /// Vstup ovladače, nebo `None` u zkratky.
    pub fn input(self) -> Option<PadAction> {
        match self {
            BindTarget::Input(t) => Some(t),
            BindTarget::Toggle => None,
        }
    }
}

/// Proč se přiřazování klávesy zrušilo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BindingCancel {
    Escape,
    Timeout,
    Gui,
    Forced(ForceReason),
    /// Engine to od Fáze 4 nevydává: přiřazování bez připraveného
    /// ovladače je povolené, takže ho výpadek ovladače neruší (po konci
    /// se jen vrátí do Disabled). Varianta zůstává kvůli stávajícím
    /// odběratelům.
    PadStatus,
}

/// Proč stisknutou klávesu nejde přiřadit. Přiřazování běží dál.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BindingReject {
    /// Zkratku přepnutí nelze přiřadit akci.
    ToggleKey,
    /// Klávesa nemá použitelný scan kód (mediální klávesy, AltGr…).
    /// Taková klávesa jde do OS.
    Unmappable,
    /// Klávesa patří Windows (Win, [`KeyId::is_reserved`]). Jde do OS
    /// jako ostatní nemapovatelné (otevře Start), okno jen řekne proč.
    Reserved,
    /// Sdílet klávesu (volba „Jedna klávesa pro víc vstupů") nejde —
    /// už ovládá [`crate::MAX_TARGETS_PER_KEY`] vstupů (Fáze 7).
    TooManyTargets,
    /// Zkratkou pozastavení nemůže být klávesa, která ovládá vstup —
    /// stisk by vždy jen přepínal (Fáze 7, Z6). Kam patří, si okno najde
    /// ve vazbách.
    Mapped,
    /// Zkratkou pozastavení smí být jen F1–F24 kromě F4, Scroll Lock a Pause
    /// ([`KeyId::is_toggle_candidate`]) — i modifikátor se odmítne.
    NotToggleKey,
}

/// Proč přepnutí (nebo zapnutí zachytávání) neproběhlo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToggleReject {
    /// Žádný ovladač není připravený.
    Disabled(DisabledReason),
    /// Běží přiřazování klávesy.
    Binding,
}

/// Co se má dozvědět okno.
///
/// Stav sám (režim, mapování) je zdrojem pravdy v enginu — hook vlákno
/// ho po každém volání publikuje ([`Engine::mode`]) a okno kreslí podle
/// něj. Událost je jen oznámení, že se něco stalo a proč; `Decision`
/// nese nejvýš jednu, takže okno se na ni nesmí spoléhat jako na jediný
/// zdroj změny režimu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UiEvent {
    ModeChanged {
        mode: Mode,
        cause: ModeCause,
    },
    /// Klávesa byla přiřazena a přiřazování skončilo (režim podle
    /// [`Mode::Binding`]).
    ///
    /// - `moved_from` = vstup, kterému klávesa patřila dosud a o který
    ///   přišla (přesun — i z jiného ovladače); u sdílené klávesy první
    ///   v kanonickém pořadí [`Targets`] a `moved_more` = kolik dalších
    ///   (0–3). Bez přesunu `None` a 0.
    /// - `shared` = kolika dalším vstupům klávesa po uložení patří (0–3):
    ///   se sdílením ty, se kterými se teď dělí, a i bez volby ty, kterým
    ///   patřila spolu s cílem už dřív (sdílená klávesa se nikomu
    ///   nebere).
    ///
    /// U zkratky pozastavení (`target: Toggle`) je `key` nová zkratka
    /// (nebo dosavadní — uloženo beze změny), ostatní pole `None` a 0.
    BindingSaved {
        key: KeyId,
        target: BindTarget,
        moved_from: Option<PadAction>,
        moved_more: u8,
        shared: u8,
    },
    /// Stisknutou klávesu nejde přiřadit; čeká se dál na jinou.
    BindingRejected {
        key: KeyId,
        reason: BindingReject,
    },
    /// Přiřazování skončilo bez uložení; režim podle [`Mode::Binding`]
    /// (po vynuceném konci nikdy Gamepad).
    BindingCancelled {
        reason: BindingCancel,
    },
    ToggleRejected {
        reason: ToggleReject,
    },
}

// Rozložení `UiEvent::pack` (bity od nejnižšího). Šířky: druh 3, scan 16,
// E0 1, cíl 7 (ovladač·32 + akce, nejvýš 3·32 + 23 = 119; 127 = zkratka
// pozastavení, Fáze 7 Z6), příznak 1, odkud 7, důvod 5, kolik dalších
// přišlo o klávesu 2, kolika dalším patří 2 — celkem 44 bitů, do schránky
// `seq16 | obsah48` hook vlákna se vejde.
const PACK_KIND: (u32, u32) = (0, 3);
const PACK_SCAN: (u32, u32) = (3, 16);
const PACK_E0: (u32, u32) = (19, 1);
const PACK_TARGET: (u32, u32) = (20, 7);
const PACK_MOVED: (u32, u32) = (27, 1);
const PACK_MOVED_FROM: (u32, u32) = (28, 7);
const PACK_REASON: (u32, u32) = (35, 5);
const PACK_MOVED_MORE: (u32, u32) = (40, 2);
const PACK_SHARED: (u32, u32) = (42, 2);

const KIND_SAVED: u64 = 1;
const KIND_REJECTED: u64 = 2;
const KIND_CANCELLED: u64 = 3;
const KIND_TOGGLE_REJECTED: u64 = 4;

/// Hodnota do pole rozložení (šířka hodnoty je zaručená volajícím).
const fn put(field: (u32, u32), v: u64) -> u64 {
    debug_assert!(v < 1 << field.1);
    v << field.0
}

const fn get(bits: u64, field: (u32, u32)) -> u64 {
    (bits >> field.0) & ((1 << field.1) - 1)
}

const fn pack_key(k: KeyId) -> u64 {
    put(PACK_SCAN, k.scan as u64) | put(PACK_E0, k.extended as u64)
}

const fn pack_target(t: PadAction) -> u64 {
    (t.pad.index() * 32 + t.action.index()) as u64
}

/// Kód cíle zkratky pozastavení — mimo rozsah vstupů (nejvýš 119), takže
/// [`unpack_target`] ho nikdy nepřečte jako vstup.
const PACK_TOGGLE: u64 = 127;

const fn pack_bind_target(t: BindTarget) -> u64 {
    match t {
        BindTarget::Input(t) => pack_target(t),
        BindTarget::Toggle => PACK_TOGGLE,
    }
}

fn unpack_target(v: u64) -> Option<PadAction> {
    let v = usize::try_from(v).ok()?;
    Some(PadAction::new(
        PadId::new(v / 32)?,
        Action::from_index(v % 32)?,
    ))
}

fn unpack_bind_target(v: u64) -> Option<BindTarget> {
    if v == PACK_TOGGLE {
        Some(BindTarget::Toggle)
    } else {
        unpack_target(v).map(BindTarget::Input)
    }
}

impl UiEvent {
    /// Oznámení jako nejvýš 48 bitů — hook callback ho předá oknu jedním
    /// atomikem (žádný kanál, žádný zámek, princip 3).
    ///
    /// `ModeChanged` → `None`: režim (i jeho příčinu) nese zvláštní atomik
    /// stavu, oznámení by ho jen zdvojilo. Klávesa jde jako syrový scan
    /// kód (16 bitů) + E0, ne jako index tabulky enginu — i nemapovatelná
    /// klávesa („tuhle nejde použít") musí přijít celá.
    ///
    /// Rozložení: 0–2 druh (1 uloženo, 2 odmítnuto, 3 zrušeno, 4 přepnutí
    /// odmítnuto) | 3–18 scan | 19 E0 | 20–26 cíl (ovladač·32 +
    /// [`Action::index`], 127 = zkratka pozastavení) | 27 je odkud? |
    /// 28–34 odkud | 35–39 důvod | 40–41 kolik dalších přišlo o klávesu |
    /// 42–43 kolika dalším patří.
    ///
    /// Počty nad 3 (engine je nevydá — klávesa má nejvýš 4 cíle) a počet
    /// dalších bez `moved_from` se zabalit nedají: `None`.
    pub fn pack(&self) -> Option<u64> {
        Some(match *self {
            UiEvent::ModeChanged { .. } => return None,
            UiEvent::BindingSaved {
                key,
                target,
                moved_from,
                moved_more,
                shared,
            } => {
                if moved_more > 3 || shared > 3 || (moved_from.is_none() && moved_more > 0) {
                    return None;
                }
                // Zkratka nikomu klávesu nebere ani ji s nikým nesdílí.
                if target == BindTarget::Toggle
                    && (moved_from.is_some() || moved_more > 0 || shared > 0)
                {
                    return None;
                }
                put(PACK_KIND, KIND_SAVED)
                    | pack_key(key)
                    | put(PACK_TARGET, pack_bind_target(target))
                    | match moved_from {
                        Some(m) => put(PACK_MOVED, 1) | put(PACK_MOVED_FROM, pack_target(m)),
                        None => 0,
                    }
                    | put(PACK_MOVED_MORE, u64::from(moved_more))
                    | put(PACK_SHARED, u64::from(shared))
            }
            UiEvent::BindingRejected { key, reason } => {
                let r = match reason {
                    BindingReject::ToggleKey => 0,
                    BindingReject::Unmappable => 1,
                    BindingReject::Reserved => 2,
                    BindingReject::TooManyTargets => 3,
                    BindingReject::Mapped => 4,
                    BindingReject::NotToggleKey => 5,
                };
                put(PACK_KIND, KIND_REJECTED) | pack_key(key) | put(PACK_REASON, r)
            }
            UiEvent::BindingCancelled { reason } => {
                let r = match reason {
                    BindingCancel::Escape => 0,
                    BindingCancel::Timeout => 1,
                    BindingCancel::Gui => 2,
                    BindingCancel::PadStatus => 3,
                    BindingCancel::Forced(f) => 4 + f.index() as u64,
                };
                put(PACK_KIND, KIND_CANCELLED) | put(PACK_REASON, r)
            }
            UiEvent::ToggleRejected { reason } => {
                let r = match reason {
                    ToggleReject::Disabled(DisabledReason::ViGEmMissing) => 0,
                    ToggleReject::Disabled(DisabledReason::PadNotConnected) => 1,
                    ToggleReject::Disabled(DisabledReason::PadError) => 2,
                    ToggleReject::Binding => 3,
                };
                put(PACK_KIND, KIND_TOGGLE_REJECTED) | put(PACK_REASON, r)
            }
        })
    }

    /// Opak [`UiEvent::pack`]. Cokoli, co `pack` nevydá (neznámý druh nebo
    /// důvod, ovladač či akce mimo rozsah, bity v nepoužitých polích),
    /// je `None` — okno pak raději načte stav znovu, než aby ukázalo
    /// smyšlené oznámení.
    pub fn unpack(bits: u64) -> Option<UiEvent> {
        let key = KeyId {
            scan: get(bits, PACK_SCAN) as u16,
            extended: get(bits, PACK_E0) == 1,
        };
        let reason = get(bits, PACK_REASON);
        let e = match get(bits, PACK_KIND) {
            KIND_SAVED => UiEvent::BindingSaved {
                key,
                target: unpack_bind_target(get(bits, PACK_TARGET))?,
                moved_from: match get(bits, PACK_MOVED) {
                    1 => Some(unpack_target(get(bits, PACK_MOVED_FROM))?),
                    _ => None,
                },
                // Pole mají 2 bity — vždy ≤ 3.
                moved_more: get(bits, PACK_MOVED_MORE) as u8,
                shared: get(bits, PACK_SHARED) as u8,
            },
            KIND_REJECTED => UiEvent::BindingRejected {
                key,
                reason: match reason {
                    0 => BindingReject::ToggleKey,
                    1 => BindingReject::Unmappable,
                    2 => BindingReject::Reserved,
                    3 => BindingReject::TooManyTargets,
                    4 => BindingReject::Mapped,
                    5 => BindingReject::NotToggleKey,
                    _ => return None,
                },
            },
            KIND_CANCELLED => UiEvent::BindingCancelled {
                reason: match reason {
                    0 => BindingCancel::Escape,
                    1 => BindingCancel::Timeout,
                    2 => BindingCancel::Gui,
                    3 => BindingCancel::PadStatus,
                    r => BindingCancel::Forced(
                        *ForceReason::ALL.get(usize::try_from(r).ok()?.checked_sub(4)?)?,
                    ),
                },
            },
            KIND_TOGGLE_REJECTED => UiEvent::ToggleRejected {
                reason: match reason {
                    0 => ToggleReject::Disabled(DisabledReason::ViGEmMissing),
                    1 => ToggleReject::Disabled(DisabledReason::PadNotConnected),
                    2 => ToggleReject::Disabled(DisabledReason::PadError),
                    3 => ToggleReject::Binding,
                    _ => return None,
                },
            },
            _ => return None,
        };
        // Jen kanonická podoba: bity, které druh nepoužívá (klávesa u
        // zrušení, cíl u odmítnutí…), musí být nulové. Tím je `unpack`
        // přesný opak `pack` a dvě různé hodnoty nikdy nedají totéž.
        (e.pack() == Some(bits)).then_some(e)
    }
}

/// Výsledek zpracování události.
///
/// - `suppress`: hook má událost spolknout (nepředat OS). U příkazů,
///   které nejsou klávesou, je vždy `false` a nemá význam.
/// - `pads`: nové stavy ovladačů k odeslání do ViGEm; ovladač, který
///   v nich není, je beze změny. Při každé změně režimu a u vynucení
///   nese všechny ovladače.
/// - `ui`: oznámení pro okno. Když se v jednom volání stane víc věcí
///   (propadlé přiřazování + stisk zkratky), nese to novější.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[must_use]
pub struct Decision {
    pub suppress: bool,
    pub pads: PadUpdates,
    pub ui: Option<UiEvent>,
}

impl Decision {
    /// Nic se nestalo.
    pub const NONE: Decision = Decision {
        suppress: false,
        pads: PadUpdates::NONE,
        ui: None,
    };

    fn key(suppress: bool) -> Decision {
        Decision {
            suppress,
            ..Decision::NONE
        }
    }

    /// Doplní z dřívějšího kroku téhož volání, co tenhle nenastavil.
    fn or(self, earlier: Decision) -> Decision {
        Decision {
            suppress: self.suppress,
            pads: self.pads.or(earlier.pads),
            ui: self.ui.or(earlier.ui),
        }
    }
}

/// Stavový automat KeyPadu.
#[derive(Clone)]
pub struct Engine {
    mode: Mode,
    mapping: Mapping,
    /// Držené klávesy podle [`KeyId::index`]. Nemapovatelné klávesy se
    /// nesledují vůbec — jdou vždy do OS a sdílely by si jeden záznam.
    held: [Option<HeldKey>; KEY_TABLE_SIZE],
    held_count: usize,
    /// Pořadové číslo posledního nového stisku.
    seq: u64,
    /// Který ovladač je připojený a smí dostávat klávesy.
    ready: [bool; MAX_PADS],
    /// Naposledy vydaný stav každého ovladače — ať se neposílá znovu
    /// totéž.
    last_pads: [PadState; MAX_PADS],
    /// Důvod posledního [`Engine::disable`] — s ním se přiřazování vrací
    /// do Disabled, když mezitím vypadly všechny ovladače.
    disabled_reason: DisabledReason,
    /// Po konci přiřazování zase zachytávat. Mimo přiřazování vždy
    /// `false`.
    resume_capture: bool,
    /// Druh běžícího přiřazování; mimo přiřazování bez významu.
    bind_kind: BindKind,
    /// Přesunout, nebo sdílet klávesu, která už patří jinam (volba
    /// v okně, Fáze 7); mimo přiřazování bez významu.
    bind_conflict: KeyConflict,
    /// Modifikátor stisknutý při přiřazování, který se přiřadí při svém
    /// key-upu — jen když mezitím nepřišel jiný stisk (pak to byla
    /// zkratka Windows, třeba Alt+Tab). Mimo přiřazování vždy `None`.
    bind_tap: Option<KeyId>,
    /// Poslední stisk při přiřazování byl falešný levý Ctrl z AltGr
    /// ([`KeyId::ALTGR_FAKE_CTRL`]). Pravý Alt hned po něm je AltGr na
    /// českém rozložení, ne ťuknutí — okno by jinak po „Tuhle klávesu
    /// nejde použít" vzápětí uložilo „P Alt" a hra by s ním dostávala
    /// i falešný Ctrl, který jde vždy Windows. Mimo přiřazování vždy
    /// `false`.
    bind_altgr: bool,
    /// Revize mapování ([`Engine::mapping_rev`]).
    mapping_rev: u64,
}

/// Modifikátory ([`KeyId::is_modifier`]) — pevný výčet, ať se při stisku
/// neprochází celá tabulka držených kláves.
const MODIFIERS: [KeyId; 6] = [
    KeyId::LEFT_CTRL,
    KeyId::RIGHT_CTRL,
    KeyId::LEFT_SHIFT,
    KeyId::RIGHT_SHIFT,
    KeyId::LEFT_ALT,
    KeyId::RIGHT_ALT,
];

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held: Vec<(KeyId, HeldKey)> = self
            .held
            .iter()
            .enumerate()
            .filter_map(|(i, h)| h.map(|h| (KeyId::from_index(i), h)))
            .collect();
        f.debug_struct("Engine")
            .field("mode", &self.mode)
            .field("ready", &self.ready)
            .field("held", &held)
            .field("seq", &self.seq)
            .field("last_pads", &self.last_pads)
            .field("disabled_reason", &self.disabled_reason)
            .field("resume_capture", &self.resume_capture)
            .field("bind_kind", &self.bind_kind)
            .field("bind_conflict", &self.bind_conflict)
            .field("bind_tap", &self.bind_tap)
            .field("bind_altgr", &self.bind_altgr)
            .field("mapping_rev", &self.mapping_rev)
            .field("mapping", &self.mapping)
            .finish()
    }
}

impl Engine {
    /// Nový engine v režimu `Disabled { PadNotConnected }`, žádný
    /// ovladač připravený.
    ///
    /// Bezpečný výchozí stav: dokud pad vlákno neohlásí připojený
    /// virtuální ovladač ([`Engine::enable`]), zkratka ani tlačítko na
    /// Gamepad nepustí — jinak by stisk Scroll Locku po startu posílal
    /// WASD do padu, který neexistuje.
    pub fn new(mapping: Mapping) -> Engine {
        Engine {
            mode: Mode::Disabled {
                reason: DisabledReason::PadNotConnected,
            },
            mapping,
            held: [None; KEY_TABLE_SIZE],
            held_count: 0,
            seq: 0,
            ready: [false; MAX_PADS],
            last_pads: [PadState::NEUTRAL; MAX_PADS],
            disabled_reason: DisabledReason::PadNotConnected,
            resume_capture: false,
            bind_kind: BindKind::Replace,
            bind_conflict: KeyConflict::Move,
            bind_tap: None,
            bind_altgr: false,
            mapping_rev: 0,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn mapping(&self) -> &Mapping {
        &self.mapping
    }

    /// Revize mapování: roste při každé SKUTEČNÉ změně (uložená vazba,
    /// která něco změnila, [`Engine::replace_mapping`] i
    /// [`Engine::set_mapping`] s jiným obsahem), jinak stojí.
    ///
    /// Okno podle ní pozná, že má načíst klávesy znovu, a „Zpět" podle
    /// ní, že mapování mezitím nezměnil nikdo jiný. Hook ji publikuje
    /// atomikem — stav, ne událost, takže se nedá ztratit.
    pub fn mapping_rev(&self) -> u64 {
        self.mapping_rev
    }

    /// Druh běžícího přiřazování; mimo přiřazování `None`.
    pub fn binding_kind(&self) -> Option<BindKind> {
        matches!(self.mode, Mode::Binding { .. }).then_some(self.bind_kind)
    }

    /// Přesun, nebo sdílení klávesy u běžícího přiřazování; mimo
    /// přiřazování `None`.
    pub fn binding_conflict(&self) -> Option<KeyConflict> {
        matches!(self.mode, Mode::Binding { .. }).then_some(self.bind_conflict)
    }

    /// Živý stav vstupů všech ovladačů pro okno: které vstupy jsou
    /// fyzicky držené a kam po SOCD míří páčky.
    ///
    /// - `Pad(s)` → všechny cíle `s` (určené při stisku, ty dostává hra),
    /// - `Swallow` → cíle klávesy v aktuálním mapování (pauza, klávesa
    ///   právě přiřazená: svítí na novém místě),
    /// - `Os` → cíle v aktuálním mapování, jen s `include_os`. Klávesy
    ///   patřící Windows ukazuje okno jen v popředí (soukromí — psaní do
    ///   jiného programu okno vidět nemá).
    ///
    /// Sdílená klávesa svítí u všech svých vstupů. Nenamapovaná klávesa
    /// nerozsvítí nic. Nealokuje — počítá se v hook callbacku.
    pub fn live_inputs(&self, include_os: bool) -> [LiveInputs; MAX_PADS] {
        let drzene = self
            .held
            .iter()
            .enumerate()
            .filter_map(|(i, h)| h.map(|h| (KeyId::from_index(i), h)));
        live_from(drzene.flat_map(move |(k, h)| {
            let cile = match h.owner {
                Owner::Pad(s) => s,
                Owner::Swallow => self.mapping.targets(k),
                Owner::Os if include_os => self.mapping.targets(k),
                Owner::Os => Targets::EMPTY,
            };
            cile.iter().map(move |t| (t, h.seq))
        }))
    }

    /// Je ovladač připojený a smí dostávat klávesy?
    pub fn is_ready(&self, pad: PadId) -> bool {
        self.ready[pad.index()]
    }

    /// Stav ovladače spočítaný z aktuálně držených kláves.
    pub fn pad_state(&self, pad: PadId) -> PadState {
        self.compute_pad(pad)
    }

    /// Počet držených kláves, o kterých engine ví.
    pub fn held_len(&self) -> usize {
        self.held_count
    }

    /// Záznam o držené klávese.
    pub fn held(&self, key: KeyId) -> Option<HeldKey> {
        key.index()
            .and_then(|i| self.held.get(i).copied())
            .flatten()
    }

    // ── Klávesnice ───────────────────────────────────────────────────

    /// Klávesa, kterou drží OS, ale engine o ní neví: hook neviděl její
    /// stisk (nainstaloval se, až když už byla dole, nebo `reset_held`
    /// zapomněl držené klávesy). Hook to zjistí jednorázovým snímkem
    /// klávesnice MIMO callback, hned po instalaci a po zapomenutí — stav
    /// OS se mění až po hooku, takže o klávese, o které callback rozhoduje,
    /// nic neřekne, a dotazy z callbacku dělaly ocas jeho ceny (OQ 57;
    /// dřívější výklad, že kvůli nim nešlo přiřazovat, je nejspíš mylný,
    /// OQ 60).
    /// Totéž pro nový stisk s drženou Win (Win+D patří Windows, OQ 44),
    /// tehdy PŘED [`Engine::on_key`].
    ///
    /// Klávesa dostane vlastníka `Os`, takže její autorepeat i key-up jdou
    /// dál do OS (princip 2). Bez toho by autorepeat vypadal jako nový
    /// stisk: v Gamepadu by se spolkl i s key-upem a v OS by klávesa
    /// visela (s levým Shiftem = všechno velkými). Nic nespouští, stav
    /// ovladačů nemění. `true` = zapsáno; už sledovaná nebo nemapovatelná
    /// klávesa se nemění.
    pub fn adopt_os_key(&mut self, key: KeyId, now_ms: u64) -> bool {
        let Some(i) = key.index() else {
            return false;
        };
        if self.held.get(i).is_some_and(Option::is_some) {
            return false;
        }
        self.seq += 1;
        self.remember(
            i,
            HeldKey {
                owner: Owner::Os,
                seq: self.seq,
                last_ms: now_ms,
            },
        );
        true
    }

    /// Zapomene klávesu OS, kterou podle Windows už nikdo nedrží: hook
    /// neviděl její key-up (okno s právy správce v popředí, OQ 39), nebo
    /// ji snímek klávesnice převzal pod jinou identitou, než s jakou pak
    /// přišel key-up (falešný Ctrl z AltGr, OQ 38). Záznam vlastníka `Os`
    /// sám nezastará ([`STALE_KEY_MS`]) — u modifikátoru by při
    /// přiřazování každý další stisk patřil Windows a nepřiřadil se ani
    /// nezrušil nic, ani Esc (OQ 55).
    ///
    /// Jen záznam `Os`: klávesu ovladače ani spolknutou Windows neviděly,
    /// jejich stav o ní nic neříká. Režim ani stav ovladačů se nemění
    /// (klávesa OS do nich nepatří); ťuknutí modifikátorem čekající na
    /// tuhle klávesu se zahodí — její key-up už nic neuloží. Hook to volá
    /// mimo callback a jen tehdy, když stavu Windows věří (okno KeyPadu
    /// v popředí, začátek přiřazování): kdyby klávesu přece jen držely,
    /// její autorepeat by pro engine byl nový stisk. `true` = zapomenuto.
    pub fn forget_os_key(&mut self, key: KeyId) -> bool {
        let Some(i) = key.index() else {
            return false;
        };
        if !self.held(key).is_some_and(|h| h.owner == Owner::Os) {
            return false;
        }
        self.forget(i);
        if self.bind_tap == Some(key) {
            self.bind_tap = None;
        }
        true
    }

    /// Zpracuje událost klávesnice z hooku.
    pub fn on_key(&mut self, key: KeyId, down: bool, now_ms: u64) -> Decision {
        // Timeout přiřazování se kontroluje i tady, nejen v `tick`:
        // kdyby časovač hook vlákna nestihl tiknout, nesmí propadlé
        // přiřazování sníst klávesu, která už patří OS.
        let expired = self.expire_binding(now_ms);
        let d = if down {
            self.key_down(key, now_ms)
        } else {
            self.key_up(key)
        };
        d.or(expired)
    }

    fn key_down(&mut self, key: KeyId, now_ms: u64) -> Decision {
        let Some(i) = key.index() else {
            // Nemapovatelná klávesa (i Win) jde vždy do OS a nesleduje
            // se. Při přiřazování se okno aspoň dozví, proč se nic nestalo.
            // Je to ale jiný stisk: držený modifikátor už nebyl ťuknutí.
            // Falešný Ctrl z AltGr si přiřazování zapamatuje kvůli pravému
            // Altu, který Windows posílají hned po něm (`bind_altgr`).
            self.bind_tap = None;
            self.bind_altgr =
                matches!(self.mode, Mode::Binding { .. }) && key == KeyId::ALTGR_FAKE_CTRL;
            return Decision {
                ui: matches!(self.mode, Mode::Binding { .. }).then_some(UiEvent::BindingRejected {
                    key,
                    reason: if key.is_reserved() {
                        BindingReject::Reserved
                    } else {
                        BindingReject::Unmappable
                    },
                }),
                ..Decision::NONE
            };
        };

        if let Some(h) = self.held.get_mut(i).and_then(Option::as_mut) {
            let stale = h.owner != Owner::Os && now_ms.saturating_sub(h.last_ms) >= STALE_KEY_MS;
            if !stale {
                // Autorepeat: rozhodnutí padlo při prvním stisku — tady
                // se jen zopakuje. Žádná akce, žádné přepnutí.
                h.last_ms = now_ms;
                return Decision::key(h.owner.suppresses());
            }
            // Ztracený key-up (viz STALE_KEY_MS): starý záznam zahodit
            // a pokračovat jako nový stisk. OS tu klávesu neviděl, takže
            // se mu nic nerozejde.
            self.forget(i);
        }

        // Nový stisk: vlastník podle tabulky „Pravidla engine".
        self.seq += 1;
        let owner = self.owner_for_new_press(key);
        if let Mode::Binding { .. } = self.mode {
            // Ťuknutí modifikátorem se přiřadí až při key-upu; každý jiný
            // stisk mezitím (Tab z Alt+Tab, i druhý modifikátor) z něj
            // dělá zkratku Windows a nepřiřadí se nic. Počítá se před
            // zápisem nového stisku — ten sám zkratku netvoří. AltGr (pravý
            // Alt hned po falešném Ctrl) okno už odmítlo jako nepoužitelnou
            // klávesu — ťuknutím se nepřiřadí; pravý Alt bez falešného Ctrl
            // (anglické rozložení) ano.
            let altgr = std::mem::take(&mut self.bind_altgr) && key == KeyId::RIGHT_ALT;
            self.bind_tap =
                (owner == Owner::Os && key.is_modifier() && !self.os_modifier_held() && !altgr)
                    .then_some(key);
        }
        self.remember(
            i,
            HeldKey {
                owner,
                seq: self.seq,
                last_ms: now_ms,
            },
        );

        let effects = if let Mode::Binding {
            target: BindTarget::Toggle,
            ..
        } = self.mode
        {
            // Přiřazuje se zkratka: i dosavadní zkratka je tu jen klávesa
            // k přiřazení (uloženo beze změny), ne přepnutí ani odmítnutí.
            match owner {
                Owner::Os => Decision::NONE,
                Owner::Pad(_) | Owner::Swallow => self.binding_key(key, BindTarget::Toggle),
            }
        } else if key == self.mapping.toggle_key() {
            self.toggle_with(ModeCause::Hotkey)
        } else if let Mode::Binding { target, .. } = self.mode {
            match owner {
                // Modifikátor nebo stisk s drženým modifikátorem patří
                // Windows a přiřazování čeká dál.
                Owner::Os => Decision::NONE,
                Owner::Pad(_) | Owner::Swallow => self.binding_key(key, target),
            }
        } else {
            // Klávesa ovladače nebo klávesa, která zahodila zastaralý
            // záznam ovladače — obojí může změnit stav.
            Decision {
                pads: self.pads_if_changed(),
                ..Decision::NONE
            }
        };
        Decision {
            suppress: owner.suppresses(),
            ..effects
        }
    }

    /// Tabulka „Key-down, klávesa ještě není v held" v ROADMAP.md.
    fn owner_for_new_press(&self, key: KeyId) -> Owner {
        if key == self.mapping.toggle_key() {
            return Owner::Swallow;
        }
        match self.mode {
            // Modifikátor jde při přiřazování do Windows (přiřadí se až
            // ťuknutím, `bind_tap`) a s drženým modifikátorem i každá další
            // klávesa — Alt+Tab, Alt+F4 a Ctrl+Shift+Esc tak z okna vedou
            // tam, kam mají, a přiřazování zruší ztráta popředí (OQ 55).
            // Modifikátor spolknutý (klávesa ovladače ze hry) zkratku
            // netvoří: Windows ho neviděly.
            Mode::Binding { .. } if key.is_modifier() || self.os_modifier_held() => Owner::Os,
            Mode::Binding { .. } => Owner::Swallow,
            // Cíle klávesy jen na připravených ovladačích — sdílená
            // klávesa hraje za všechny z nich. Klávesa jen nepřipravených
            // ovladačů jde do OS: ovladač, který neexistuje, by ji jen
            // spolkl a uživateli by zmizela.
            Mode::Gamepad => {
                let s = self.mapping.targets(key).only_pads(&self.ready);
                if s.is_empty() {
                    Owner::Os
                } else {
                    Owner::Pad(s)
                }
            }
            Mode::Keyboard | Mode::Disabled { .. } => Owner::Os,
        }
    }

    /// Drží Windows některý modifikátor (vlastník `Os`)? Pak je nový stisk
    /// zkratka Windows, ne klávesa k přiřazení.
    fn os_modifier_held(&self) -> bool {
        MODIFIERS
            .iter()
            .any(|&k| self.held(k).is_some_and(|h| h.owner == Owner::Os))
    }

    fn key_up(&mut self, key: KeyId) -> Decision {
        let Some(h) = key.index().and_then(|i| self.forget(i)) else {
            // Key-up bez záznamu: klávesa držená už před spuštěním,
            // zapomenutá po zamčení relace, nebo nemapovatelná. OS její
            // key-down viděl, takže key-up patří jemu.
            return Decision::NONE;
        };
        let suppress = h.owner.suppresses();
        // Ťuknutí modifikátorem při přiřazování: teď se přiřadí. Key-up
        // jde Windows jako jeho key-down (princip 2) — spolknout ho by
        // nechalo modifikátor v OS viset. `bind_tap` je mimo přiřazování
        // vždy prázdné (maže ho každá změna režimu).
        if self.bind_tap == Some(key) {
            self.bind_tap = None;
            if let Mode::Binding { target, .. } = self.mode {
                return Decision {
                    suppress,
                    ..self.binding_key(key, target)
                };
            }
        }
        Decision {
            suppress,
            pads: match h.owner {
                Owner::Pad(_) => self.pads_if_changed(),
                Owner::Os | Owner::Swallow => PadUpdates::NONE,
            },
            ui: None,
        }
    }

    /// Klávesa stisknutá během přiřazování (u vstupu ne zkratka přepnutí).
    fn binding_key(&mut self, key: KeyId, target: BindTarget) -> Decision {
        if key == KeyId::ESC {
            return self.leave_binding(BindingCancel::Escape);
        }
        match target {
            BindTarget::Input(t) => self.bind_input(key, t),
            BindTarget::Toggle => self.bind_toggle(key),
        }
    }

    /// Nová zkratka pozastavení (Fáze 7, Z6). Pořadí: dosavadní zkratka
    /// = uloženo beze změny (revize neroste); mimo F1–F24 (bez F4), Scroll Lock
    /// a Pause odmítnuto — i modifikátor; klávesa s cílem odmítnuta; jinak
    /// uloženo. Esc, Win a nemapovatelné klávesy odchytí volající dřív.
    /// Odmítnutí přiřazování nekončí — čeká se na jinou klávesu.
    fn bind_toggle(&mut self, key: KeyId) -> Decision {
        let reject = |reason| Decision {
            ui: Some(UiEvent::BindingRejected { key, reason }),
            ..Decision::NONE
        };
        if key != self.mapping.toggle_key() {
            if !key.is_toggle_candidate() {
                return reject(BindingReject::NotToggleKey);
            }
            match self.mapping.set_toggle_key(key) {
                Ok(()) => self.mapping_rev += 1,
                Err(MappingError::ToggleKeyMapped { .. }) => {
                    return reject(BindingReject::Mapped);
                }
                // Kandidát je vždy mapovatelný a není Esc ani Win — jen
                // pojistka, kdyby se seznam někdy rozešel s pravidly
                // mapování.
                Err(MappingError::Reserved { .. }) => return reject(BindingReject::Reserved),
                Err(_) => return reject(BindingReject::Unmappable),
            }
        }
        // Stará zkratka držená v tuhle chvíli se dál spolkne až do
        // uvolnění (vlastník Swallow, princip 2); nová taky — stiskla se
        // při přiřazování.
        let to = self.mode_after_binding(true);
        Decision {
            suppress: false,
            pads: self.switch_mode(to),
            ui: Some(UiEvent::BindingSaved {
                key,
                target: BindTarget::Toggle,
                moved_from: None,
                moved_more: 0,
                shared: 0,
            }),
        }
    }

    /// Klávesa pro vstup ovladače (přesun, nebo sdílení).
    fn bind_input(&mut self, key: KeyId, target: PadAction) -> Decision {
        let before = self.mapping.targets(key);
        let saved = match self.bind_kind {
            BindKind::Add => self.mapping.bind(key, target, self.bind_conflict),
            BindKind::Replace => self.mapping.bind_replacing(key, target, self.bind_conflict),
        };
        match saved {
            Ok(Rebind {
                moved,
                shared,
                removed,
            }) => {
                // Revize jen se skutečnou změnou: klávesa, která cíli už
                // patří (jediná, nebo přidaná podruhé), nic nemění a okno
                // nemá co načítat znovu.
                if !before.contains(target) || removed > 0 {
                    self.mapping_rev += 1;
                }
                // Klávesa má vlastníka Swallow (stisknutá při
                // přiřazování) — i když se teď vrací zachytávání, její
                // key-up se spolkne a ovladač ji neuvidí. Ťuknutý
                // modifikátor se ukládá až při key-upu, nic už nedrží.
                let to = self.mode_after_binding(true);
                // Klávesa má nejvýš 4 cíle: přijít o ni můžou nejvýš 4
                // vstupy (první + 3 další) a dělit se s nejvýš 3 dalšími.
                let narrow = |n: usize| u8::try_from(n.min(3)).unwrap_or(3);
                Decision {
                    suppress: false,
                    pads: self.switch_mode(to),
                    ui: Some(UiEvent::BindingSaved {
                        key,
                        target: BindTarget::Input(target),
                        moved_from: moved.first(),
                        moved_more: narrow(moved.len().saturating_sub(1)),
                        shared: narrow(shared),
                    }),
                }
            }
            // Pátý vstup sdílené klávesy: přiřazování čeká dál na jinou
            // klávesu (nic se nezměnilo). Zkratku i nemapovatelné klávesy
            // odchytí key_down dřív; jejich větve jsou jen pojistka.
            Err(e) => Decision {
                ui: Some(UiEvent::BindingRejected {
                    key,
                    reason: match e {
                        MappingError::TooManyTargets { .. } => BindingReject::TooManyTargets,
                        MappingError::ToggleKeyMapped { .. } => BindingReject::ToggleKey,
                        MappingError::Reserved { .. } => BindingReject::Reserved,
                        _ => BindingReject::Unmappable,
                    },
                }),
                ..Decision::NONE
            },
        }
    }

    // ── Příkazy z okna a z platformy ─────────────────────────────────

    /// Přepne Klávesnice ↔ Gamepad (tlačítko v okně). Totéž dělá
    /// zkratka přepnutí.
    pub fn toggle(&mut self, now_ms: u64) -> Decision {
        let expired = self.expire_binding(now_ms);
        self.toggle_with(ModeCause::Gui).or(expired)
    }

    fn toggle_with(&mut self, cause: ModeCause) -> Decision {
        match self.mode {
            Mode::Keyboard => self.change_mode(Mode::Gamepad, cause),
            Mode::Gamepad => self.change_mode(Mode::Keyboard, cause),
            // Zkratka během přiřazování je pokus přiřadit ji akci — to
            // nejde, přiřazování čeká dál. Tlačítko v okně během
            // přiřazování nemá co přepínat.
            Mode::Binding { .. } => Decision {
                ui: Some(match cause {
                    ModeCause::Hotkey => UiEvent::BindingRejected {
                        key: self.mapping.toggle_key(),
                        reason: BindingReject::ToggleKey,
                    },
                    _ => UiEvent::ToggleRejected {
                        reason: ToggleReject::Binding,
                    },
                }),
                ..Decision::NONE
            },
            Mode::Disabled { reason } => Decision {
                ui: Some(UiEvent::ToggleRejected {
                    reason: ToggleReject::Disabled(reason),
                }),
                ..Decision::NONE
            },
        }
    }

    /// Začne zachytávat (v okně se zapnul ovladač = uživatel chce hrát).
    ///
    /// Na rozdíl od [`Engine::toggle`] nikdy nepozastaví: zachytávání
    /// už běží → nic. Během přiřazování si jen poznamená, že se po jeho
    /// konci má zachytávat — přiřazovanou klávesu by jinak dostal
    /// ovladač.
    pub fn capture(&mut self, now_ms: u64) -> Decision {
        let expired = self.expire_binding(now_ms);
        let d = match self.mode {
            Mode::Keyboard => self.change_mode(Mode::Gamepad, ModeCause::Gui),
            Mode::Gamepad => Decision::NONE,
            Mode::Disabled { reason } => Decision {
                ui: Some(UiEvent::ToggleRejected {
                    reason: ToggleReject::Disabled(reason),
                }),
                ..Decision::NONE
            },
            Mode::Binding { .. } => {
                self.resume_capture = true;
                Decision::NONE
            }
        };
        d.or(expired)
    }

    /// Vynutí režim Klávesnice (fail-safe: watchdog, chyba padu,
    /// zavírání…). Z Gamepadu okamžitě pošle neutrální stavy a klávesy
    /// ovladačů spolkne; běžící přiřazování zruší (a na Gamepad se po
    /// něm nevrací). Režim Disabled nechá — ten se chová jako Klávesnice
    /// a navíc nepustí na Gamepad.
    ///
    /// Stavy všech ovladačů vrací vždy (i když se nic nezměnilo): volá
    /// se, když něco selhalo, a potvrdit neutrál je tehdy levnější než
    /// zjišťovat, jestli ho ovladač opravdu má.
    ///
    /// Pro paniku v hook callbacku použij [`Engine::reset_held`], ne
    /// tohle: hook klávesu po panice propustí do OS, a kdyby její záznam
    /// zůstal jako `Swallow`, key-up by se spolkl a klávesa by v OS visela.
    pub fn force_keyboard(&mut self, reason: ForceReason) -> Decision {
        match self.mode {
            Mode::Gamepad => self.change_mode(Mode::Keyboard, ModeCause::Forced(reason)),
            Mode::Binding { .. } => self.leave_binding(BindingCancel::Forced(reason)),
            Mode::Keyboard | Mode::Disabled { .. } => Decision {
                pads: self.emit_all(),
                ..Decision::NONE
            },
        }
    }

    /// Vynutí Klávesnici a zapomene všechny držené klávesy.
    ///
    /// Pro chvíle, kdy se key-upy ztratí nebo se stav klávesy nedá
    /// věřit: zamčení relace, uspání, přepnutí plochy (UAC,
    /// Ctrl+Alt+Del), přeinstalace hooku a panika v hook callbacku.
    /// Key-up zapomenuté klávesy pak jde do OS (jako klávesa držená před
    /// spuštěním) — pro klávesy padu je to neškodný key-up navíc, pro
    /// klávesu propuštěnou po panice nutnost.
    pub fn reset_held(&mut self, reason: ForceReason) -> Decision {
        let d = self.force_keyboard(reason);
        self.held = [None; KEY_TABLE_SIZE];
        self.held_count = 0;
        Decision {
            pads: self.emit_all(),
            ..d
        }
    }

    /// Začne přiřazování klávesy akci ovladače — z kteréhokoli režimu
    /// (Fáze 6: klávesy jde upravovat i za hry, ovladač nemusí být
    /// připojený). Zachytávání se na dobu přiřazování pozastaví a po
    /// konci vrátí; běžící přiřazování začne znovu s novým cílem
    /// a druhem.
    ///
    /// `kind` určuje, co se stane s ostatními klávesami vstupu
    /// ([`BindKind`]), `conflict`, co se stane s klávesou, která už
    /// patří jiným vstupům ([`KeyConflict`] — volba „Jedna klávesa pro
    /// víc vstupů"; posílá ji backend podle uložené volby, ne okno).
    ///
    /// `target` může být i [`BindTarget::Toggle`] — přiřazování zkratky
    /// pozastavení (Fáze 7, Z6); `kind` a `conflict` pak nic neřídí.
    pub fn start_binding(
        &mut self,
        target: impl Into<BindTarget>,
        kind: BindKind,
        conflict: KeyConflict,
        now_ms: u64,
    ) -> Decision {
        // Propadlé přiřazování (časovač nestihl tiknout) se nejdřív
        // ukončí — o návratu zachytávání pak rozhoduje režim, do kterého
        // se vrátilo, ne zapomenutý příznak.
        let expired = self.expire_binding(now_ms);
        self.bind_kind = kind;
        self.bind_conflict = conflict;
        self.resume_capture = match self.mode {
            Mode::Gamepad => true,
            Mode::Binding { .. } => self.resume_capture,
            Mode::Keyboard | Mode::Disabled { .. } => false,
        };
        self.change_mode(
            Mode::Binding {
                target: target.into(),
                started_at_ms: now_ms,
            },
            ModeCause::Gui,
        )
        .or(expired)
    }

    /// Zruší přiřazování (tlačítko v okně). Mimo přiřazování nic nedělá.
    pub fn cancel_binding(&mut self, now_ms: u64) -> Decision {
        let expired = self.expire_binding(now_ms);
        match self.mode {
            Mode::Binding { .. } => self.leave_binding(BindingCancel::Gui),
            _ => expired,
        }
    }

    /// Pravidelné tiknutí (hook vlákno ho volá časovačem) — hlídá timeout
    /// přiřazování.
    pub fn tick(&mut self, now_ms: u64) -> Decision {
        self.expire_binding(now_ms)
    }

    fn expire_binding(&mut self, now_ms: u64) -> Decision {
        match self.mode {
            Mode::Binding { started_at_ms, .. }
                if now_ms.saturating_sub(started_at_ms) >= BINDING_TIMEOUT_MS =>
            {
                self.leave_binding(BindingCancel::Timeout)
            }
            _ => Decision::NONE,
        }
    }

    /// Ovladač není k dispozici (ViGEmBus chybí, odpojil se, chyba,
    /// uživatel ho vypnul).
    ///
    /// Klávesy, které ovladač držel, mu přestanou patřit a ovladač dostane
    /// neutrál; ostatní ovladače hrají dál — i se sdílenou klávesou, která
    /// hrála i za něj (jeho cíle se z ní jen odeberou). Klávesa, které
    /// nezbyl žádný cíl, se spolkne až do uvolnění (OS její key-down
    /// neviděl). Vypadl-li poslední připravený ovladač, Klávesnice
    /// i Gamepad končí v Disabled. Přiřazování běží dál — bez ovladače je
    /// povolené a po konci se vrátí do Disabled s tímhle důvodem.
    pub fn disable(&mut self, pad: PadId, reason: DisabledReason) -> Decision {
        self.disabled_reason = reason;
        if !self.is_ready(pad) {
            return match self.mode {
                // Jiný důvod se oznámí (např. chybějící ViGEmBus místo
                // „nepřipojeno"), stejný nic nemění.
                Mode::Disabled { reason: r } if r != reason => {
                    self.change_mode(Mode::Disabled { reason }, ModeCause::PadStatus)
                }
                _ => Decision::NONE,
            };
        }

        self.ready[pad.index()] = false;
        for h in self.held.iter_mut().flatten() {
            if let Owner::Pad(mut s) = h.owner {
                if s.remove_pad(pad) > 0 {
                    h.owner = if s.is_empty() {
                        Owner::Swallow
                    } else {
                        Owner::Pad(s)
                    };
                }
            }
        }
        match self.mode {
            Mode::Keyboard | Mode::Gamepad if !self.ready.contains(&true) => {
                self.change_mode(Mode::Disabled { reason }, ModeCause::PadStatus)
            }
            _ => {
                let mut pads = PadUpdates::NONE;
                pads.set(pad, self.emit(pad));
                Decision {
                    pads,
                    ..Decision::NONE
                }
            }
        }
    }

    /// Ovladač je připojený. Z Disabled jde VŽDY na Klávesnici, nikdy
    /// rovnou na Gamepad — o zachytávání rozhoduje uživatel. Klávesy
    /// držené v tu chvíli zůstávají, komu patřily (OS), až do uvolnění.
    pub fn enable(&mut self, pad: PadId) -> Decision {
        if self.is_ready(pad) {
            return Decision::NONE;
        }
        self.ready[pad.index()] = true;
        match self.mode {
            Mode::Disabled { .. } => self.change_mode(Mode::Keyboard, ModeCause::PadStatus),
            _ => Decision::NONE,
        }
    }

    /// Nahradí mapování. V režimu Gamepad se nejdřív vynutí Klávesnice
    /// (ovladač nesmí zůstat s vazbami, které už neplatí); běžící
    /// přiřazování se zruší.
    ///
    /// Pro úpravy z editoru je [`Engine::replace_mapping`] — tohle je
    /// fail-safe výměna celého rozvržení.
    pub fn set_mapping(&mut self, mapping: Mapping) -> Decision {
        let d = match self.mode {
            Mode::Gamepad | Mode::Binding { .. } => {
                self.force_keyboard(ForceReason::MappingChanged)
            }
            Mode::Keyboard | Mode::Disabled { .. } => Decision::NONE,
        };
        self.store_mapping(mapping);
        d
    }

    /// Živá výměna mapování z editoru (vyprázdnit vstup, odebrat
    /// ovladač, výchozí klávesy, „Zpět") — i za hry, bez pozastavení
    /// (OQ 43).
    ///
    /// Propadlé přiřazování nejdřív ukončí, běžící zruší s
    /// [`BindingCancel::Gui`] a vrátí se, kam patří (i na Gamepad) —
    /// cíl přiřazování mohl s novým mapováním zmizet (odebraný ovladač).
    ///
    /// Jinak NEMĚNÍ režim ani vlastníky držených kláves (princip 2):
    /// klávesa držená jako `Pad(t)` dohraje s `t` a její key-up se
    /// spolkne, nový stisk jde podle nového mapování. Stav ovladačů se
    /// počítá z vlastníků, ne z mapování, takže výměna sama nic
    /// neposílá. [`Engine::set_mapping`] (s vynucením Klávesnice) zůstává
    /// pro fail-safe.
    pub fn replace_mapping(&mut self, mapping: Mapping, now_ms: u64) -> Decision {
        let expired = self.expire_binding(now_ms);
        let d = match self.mode {
            Mode::Binding { .. } => self.leave_binding(BindingCancel::Gui),
            Mode::Keyboard | Mode::Gamepad | Mode::Disabled { .. } => Decision::NONE,
        };
        self.store_mapping(mapping);
        d.or(expired)
    }

    /// Uloží mapování; revize roste jen s jiným obsahem.
    fn store_mapping(&mut self, mapping: Mapping) {
        if mapping != self.mapping {
            self.mapping_rev += 1;
        }
        self.mapping = mapping;
    }

    // ── Vnitřní přechody ─────────────────────────────────────────────

    /// Změní režim a oznámí to.
    fn change_mode(&mut self, new: Mode, cause: ModeCause) -> Decision {
        Decision {
            suppress: false,
            pads: self.switch_mode(new),
            ui: Some(UiEvent::ModeChanged { mode: new, cause }),
        }
    }

    /// Kam vede konec přiřazování; zároveň zapomene příznak návratu
    /// zachytávání (mimo přiřazování nemá význam).
    ///
    /// Bez připraveného ovladače vždy Disabled — Klávesnice by jinak
    /// tvrdila, že jde zachytávat. Vynucený konec (`may_capture ==
    /// false`: fail-safe, změna mapování) na Gamepad nikdy nevede.
    fn mode_after_binding(&mut self, may_capture: bool) -> Mode {
        let resume = std::mem::take(&mut self.resume_capture);
        if !self.ready.contains(&true) {
            Mode::Disabled {
                reason: self.disabled_reason,
            }
        } else if resume && may_capture {
            Mode::Gamepad
        } else {
            Mode::Keyboard
        }
    }

    /// Ukončí přiřazování bez uložení.
    fn leave_binding(&mut self, reason: BindingCancel) -> Decision {
        let to = self.mode_after_binding(!matches!(reason, BindingCancel::Forced(_)));
        Decision {
            suppress: false,
            pads: self.switch_mode(to),
            ui: Some(UiEvent::BindingCancelled { reason }),
        }
    }

    /// Jediné místo, kde se mění `mode`.
    ///
    /// Odchod z Gamepadu: klávesy ovladačů se mění na `Swallow` — jejich
    /// key-up se spolkne (OS nikdy neviděl key-down) a ovladače se jich
    /// zbaví hned, ne až po uvolnění. Příchod do Gamepadu: klávesy
    /// držené s vlastníkem `Os` (i `Swallow`) zůstávají, komu patří, až
    /// do uvolnění.
    ///
    /// Stavy všech ovladačů se po každé změně režimu vydají vždy — po
    /// odchodu z Gamepadu je to okamžitý neutrál.
    fn switch_mode(&mut self, new: Mode) -> PadUpdates {
        if self.mode == Mode::Gamepad && new != Mode::Gamepad {
            for h in self.held.iter_mut().flatten() {
                if let Owner::Pad(_) = h.owner {
                    h.owner = Owner::Swallow;
                }
            }
        }
        // Ťuknutí patří jen tomu přiřazování, při kterém začalo — i nové
        // přiřazování (jiný cíl) začíná bez něj. Stejně AltGr.
        self.bind_tap = None;
        self.bind_altgr = false;
        self.mode = new;
        self.emit_all()
    }

    fn remember(&mut self, i: usize, h: HeldKey) {
        if let Some(slot) = self.held.get_mut(i) {
            if slot.replace(h).is_none() {
                self.held_count += 1;
            }
        }
    }

    fn forget(&mut self, i: usize) -> Option<HeldKey> {
        let h = self.held.get_mut(i).and_then(Option::take);
        if h.is_some() {
            self.held_count -= 1;
        }
        h
    }

    /// Stav jednoho ovladače — jen z cílů kláves, které patří jemu.
    /// Sdílená klávesa přispěje každým svým cílem na tomhle ovladači se
    /// svým `seq` (protichůdné směry jedné klávesy = shoda = neutrál).
    fn compute_pad(&self, pad: PadId) -> PadState {
        compute_pad_state(self.held.iter().flatten().flat_map(|h| {
            let akce = match h.owner {
                Owner::Pad(s) => s.actions(pad),
                Owner::Os | Owner::Swallow => ActionSet::EMPTY,
            };
            akce.iter().map(move |a| (a, h.seq))
        }))
    }

    /// Přepočítá stav ovladače a vydá ho vždy.
    fn emit(&mut self, pad: PadId) -> PadState {
        let s = self.compute_pad(pad);
        self.last_pads[pad.index()] = s;
        s
    }

    /// Přepočítá a vydá stavy všech ovladačů.
    fn emit_all(&mut self) -> PadUpdates {
        let mut pads = PadUpdates::NONE;
        for p in PadId::ALL {
            pads.set(p, self.emit(p));
        }
        pads
    }

    /// Přepočítá všechny ovladače a vydá jen ty, které se změnily (druhá
    /// klávesa téhož tlačítka nemá smysl posílat do ViGEm znovu).
    ///
    /// Počítají se všechny, ne jen ovladač stisknuté klávesy (princip 7):
    /// stav je vždy celý z držených kláves, žádné odvozování, kterého se
    /// změna „asi" týká.
    fn pads_if_changed(&mut self) -> PadUpdates {
        let mut pads = PadUpdates::NONE;
        for p in PadId::ALL {
            let new = self.compute_pad(p);
            if new != self.last_pads[p.index()] {
                self.last_pads[p.index()] = new;
                pads.set(p, new);
            }
        }
        pads
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{Action, PadButton, StickDir};
    use crate::pad_state::{AXIS_DIAGONAL, AXIS_MAX};

    const TOGGLE: KeyId = KeyId::SCROLL_LOCK;
    const T0: u64 = 1_000;
    const P0: PadId = PadId::FIRST;
    const P1: PadId = PadId::ALL[1];
    const P2: PadId = PadId::ALL[2];

    /// Akce na prvním ovladači.
    fn t0(a: Action) -> PadAction {
        PadAction::first(a)
    }

    /// Stav prvního ovladače k odeslání (`None` = beze změny).
    fn pad0(d: &Decision) -> Option<PadState> {
        d.pads.get(P0)
    }

    /// Engine s připojeným prvním ovladačem (režim Klávesnice).
    fn engine_with(m: Mapping) -> Engine {
        let mut e = Engine::new(m);
        let _ = e.enable(P0);
        assert_eq!(e.mode(), Mode::Keyboard);
        e
    }

    fn engine() -> Engine {
        engine_with(Mapping::default())
    }

    fn down(e: &mut Engine, k: KeyId) -> Decision {
        e.on_key(k, true, T0)
    }

    fn up(e: &mut Engine, k: KeyId) -> Decision {
        e.on_key(k, false, T0)
    }

    fn gamepad() -> Engine {
        let mut e = engine();
        let _ = down(&mut e, TOGGLE);
        let _ = up(&mut e, TOGGLE);
        assert_eq!(e.mode(), Mode::Gamepad);
        e
    }

    fn stick(p: PadState) -> (i16, i16) {
        (p.thumb_lx, p.thumb_ly)
    }

    /// Rozhodnutí nese všechny ovladače a všechny neutrální.
    fn vse_neutralni(d: &Decision) -> bool {
        PadId::ALL
            .into_iter()
            .all(|p| d.pads.get(p) == Some(PadState::NEUTRAL))
    }

    // ── Jmenované testy z ROADMAP.md, Fáze 1 ────────────────────────

    #[test]
    fn prepnuti_do_gamepadu_pri_drzenem_w() {
        let mut e = engine();
        // W stisknuté v režimu Klávesnice → OS.
        assert!(!down(&mut e, KeyId::W).suppress);
        let d = down(&mut e, TOGGLE);
        assert!(d.suppress, "zkratka se spolkne");
        assert_eq!(e.mode(), Mode::Gamepad);
        assert!(
            pad0(&d).expect("stav po přepnutí").is_neutral(),
            "gamepad W neukáže"
        );
        // Autorepeat W jde dál do OS a pad nehne.
        let r = down(&mut e, KeyId::W);
        assert!(!r.suppress);
        assert!(r.pads.is_empty());
        assert!(e.pad_state(P0).is_neutral());
        // W-up jde do OS (ten viděl key-down).
        let u = up(&mut e, KeyId::W);
        assert!(!u.suppress, "W-up musí dostat OS");
        assert!(u.pads.is_empty());
        // Nový stisk W už patří padu.
        let n = down(&mut e, KeyId::W);
        assert!(n.suppress);
        assert_eq!(stick(pad0(&n).unwrap()), (0, AXIS_MAX));
    }

    #[test]
    fn prepnuti_do_klavesnice_pri_drzenem_w() {
        let mut e = gamepad();
        let d = down(&mut e, KeyId::W);
        assert!(d.suppress);
        assert_eq!(stick(pad0(&d).unwrap()), (0, AXIS_MAX));

        let t = down(&mut e, TOGGLE);
        assert_eq!(e.mode(), Mode::Keyboard);
        assert!(vse_neutralni(&t), "páčka hned neutrální");
        assert_eq!(e.held(KeyId::W).unwrap().owner, Owner::Swallow);

        // Autorepeat i key-up W se spolknou — OS key-down nikdy neviděl.
        let r = down(&mut e, KeyId::W);
        assert!(r.suppress);
        assert!(r.pads.is_empty());
        let u = up(&mut e, KeyId::W);
        assert!(u.suppress, "W-up spolknut");
        assert!(u.pads.is_empty(), "pad už je neutrální, nic se neposílá");
        assert!(e.pad_state(P0).is_neutral());
        assert_eq!(e.held(KeyId::W), None);
    }

    #[test]
    fn autorepeat_nemeni_stav_ani_neprepina_opakovane() {
        let mut e = engine();
        let prvni = down(&mut e, TOGGLE);
        assert!(matches!(
            prvni.ui,
            Some(UiEvent::ModeChanged {
                mode: Mode::Gamepad,
                cause: ModeCause::Hotkey
            })
        ));
        // Držená zkratka generuje autorepeat — přepnout se nesmí znovu.
        // Opakování chodí po 33 ms, takže to není ztracený key-up.
        for n in 1..=100 {
            let r = e.on_key(TOGGLE, true, T0 + n * 33);
            assert!(r.suppress, "autorepeat zkratky se spolkne");
            assert_eq!(r.ui, None);
            assert!(r.pads.is_empty());
            assert_eq!(e.mode(), Mode::Gamepad);
        }
        assert!(e.on_key(TOGGLE, false, T0 + 3_400).suppress);
        assert_eq!(e.mode(), Mode::Gamepad);

        // Autorepeat klávesy padu stav nemění a nic neposílá.
        let d = down(&mut e, KeyId::D);
        let stav = pad0(&d).unwrap();
        for n in 1..=100 {
            let r = e.on_key(KeyId::D, true, T0 + n * 33);
            assert!(r.suppress);
            assert!(r.pads.is_empty());
            assert_eq!(r.ui, None);
        }
        assert_eq!(e.pad_state(P0), stav);
        assert_eq!(
            e.held(KeyId::D).unwrap().seq,
            2,
            "seq se autorepeatem nezvyšuje"
        );
    }

    #[test]
    fn key_up_bez_key_down_je_propusten() {
        for mut e in [engine(), gamepad()] {
            let d = up(&mut e, KeyId::W);
            assert_eq!(d, Decision::NONE, "v režimu {:?}", e.mode());
            // Ani key-up zkratky bez stisku nic nepřepne.
            let d = up(&mut e, TOGGLE);
            assert_eq!(d, Decision::NONE);
        }
    }

    #[test]
    fn socd_a_d_uvolnit_d_zpet_a() {
        let mut e = gamepad();
        let d = down(&mut e, KeyId::A);
        assert_eq!(stick(pad0(&d).unwrap()), (-AXIS_MAX, 0));
        let d = down(&mut e, KeyId::D);
        assert_eq!(stick(pad0(&d).unwrap()), (AXIS_MAX, 0), "vyhrává D");
        let d = up(&mut e, KeyId::D);
        assert_eq!(stick(pad0(&d).unwrap()), (-AXIS_MAX, 0), "zpět k A");
        let d = up(&mut e, KeyId::A);
        assert_eq!(stick(pad0(&d).unwrap()), (0, 0));
    }

    #[test]
    fn diagonala_w_d_a_samotne_w() {
        let mut e = gamepad();
        let d = down(&mut e, KeyId::W);
        assert_eq!(stick(pad0(&d).unwrap()), (0, 32_767));
        let d = down(&mut e, KeyId::D);
        assert_eq!(stick(pad0(&d).unwrap()), (23_170, 23_170));
        assert_eq!(AXIS_DIAGONAL, 23_170);
        let d = up(&mut e, KeyId::D);
        assert_eq!(stick(pad0(&d).unwrap()), (0, 32_767));
    }

    #[test]
    fn dve_klavesy_na_stejne_tlacitko() {
        let mut m = Mapping::default();
        m.bind(
            KeyId::X,
            t0(Action::Button(PadButton::A)),
            KeyConflict::Move,
        )
        .unwrap();
        let mut e = engine_with(m);
        let _ = e.toggle(T0);

        let d = down(&mut e, KeyId::SPACE);
        assert!(pad0(&d).unwrap().is_pressed(PadButton::A));
        // Druhá klávesa téhož tlačítka: stav se nemění, nic se neposílá.
        let d = down(&mut e, KeyId::X);
        assert!(d.suppress);
        assert!(d.pads.is_empty());
        // Uvolnění jedné nechá tlačítko stisknuté.
        let d = up(&mut e, KeyId::SPACE);
        assert!(d.suppress);
        assert!(d.pads.is_empty());
        assert!(e.pad_state(P0).is_pressed(PadButton::A));
        // Až poslední klávesa ho pustí.
        let d = up(&mut e, KeyId::X);
        assert!(!pad0(&d).unwrap().is_pressed(PadButton::A));
    }

    #[test]
    fn binding_esc_rusi() {
        let mut e = engine();
        let before = e.mapping().clone();
        let _ = e.start_binding(
            t0(Action::Button(PadButton::A)),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        let d = down(&mut e, KeyId::ESC);
        assert!(d.suppress, "Esc během přiřazování nejde do OS");
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Escape
            })
        );
        assert_eq!(e.mode(), Mode::Keyboard);
        assert_eq!(e.mapping(), &before);
        // Key-up Esc se spolkne taky (vlastník Swallow).
        assert!(up(&mut e, KeyId::ESC).suppress);
    }

    #[test]
    fn binding_timeout_rusi() {
        let mut e = engine();
        let _ = e.start_binding(
            t0(Action::LeftTrigger),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        assert_eq!(e.tick(T0 + BINDING_TIMEOUT_MS - 1), Decision::NONE);
        assert!(matches!(e.mode(), Mode::Binding { .. }));
        let d = e.tick(T0 + BINDING_TIMEOUT_MS);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Timeout
            })
        );
        assert_eq!(e.mode(), Mode::Keyboard);
    }

    #[test]
    fn binding_timeout_rusi_i_bez_ticku() {
        // Časovač nestihl tiknout: propadlé přiřazování nesmí sníst
        // klávesu, která už patří OS.
        let mut e = engine();
        let before = e.mapping().clone();
        let _ = e.start_binding(
            t0(Action::LeftTrigger),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        let d = e.on_key(KeyId::X, true, T0 + BINDING_TIMEOUT_MS + 5);
        assert!(!d.suppress, "klávesa po timeoutu jde do OS");
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Timeout
            })
        );
        assert_eq!(e.mapping(), &before);
        assert!(
            !e.on_key(KeyId::X, false, T0 + BINDING_TIMEOUT_MS + 9)
                .suppress
        );
    }

    #[test]
    fn binding_zkratku_nelze_priradit() {
        let mut e = engine();
        let before = e.mapping().clone();
        let _ = e.start_binding(
            t0(Action::Button(PadButton::Y)),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        let d = down(&mut e, TOGGLE);
        assert!(d.suppress);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingRejected {
                key: TOGGLE,
                reason: BindingReject::ToggleKey
            })
        );
        assert!(
            matches!(e.mode(), Mode::Binding { .. }),
            "přiřazování čeká dál"
        );
        assert_eq!(e.mapping(), &before);
        let _ = up(&mut e, TOGGLE);
        // Další klávesa se už přiřadí.
        let d = down(&mut e, KeyId::X);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingSaved {
                key: KeyId::X,
                target: t0(Action::Button(PadButton::Y)).into(),
                moved_from: None,
                moved_more: 0,
                shared: 0
            })
        );
        assert_eq!(
            e.mapping().targets(KeyId::X).first(),
            Some(t0(Action::Button(PadButton::Y)))
        );
    }

    // ── Další chování ────────────────────────────────────────────────

    #[test]
    fn novy_engine_nepusti_na_gamepad_bez_padu() {
        let mut e = Engine::new(Mapping::default());
        assert_eq!(
            e.mode(),
            Mode::Disabled {
                reason: DisabledReason::PadNotConnected
            }
        );
        for p in PadId::ALL {
            assert!(!e.is_ready(p));
        }
        let d = down(&mut e, TOGGLE);
        assert!(d.suppress);
        assert_eq!(
            d.ui,
            Some(UiEvent::ToggleRejected {
                reason: ToggleReject::Disabled(DisabledReason::PadNotConnected)
            })
        );
        assert!(!down(&mut e, KeyId::W).suppress, "klávesy jdou do OS");
    }

    #[test]
    fn binding_ulozi_klavesu_a_spolkne_ji() {
        let mut e = engine();
        let d = e.start_binding(
            t0(Action::RightTrigger),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        assert!(matches!(
            d.ui,
            Some(UiEvent::ModeChanged {
                mode: Mode::Binding { .. },
                cause: ModeCause::Gui
            })
        ));
        let d = down(&mut e, KeyId::W);
        assert!(d.suppress, "přiřazovaná klávesa nejde do OS");
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingSaved {
                key: KeyId::W,
                target: t0(Action::RightTrigger).into(),
                moved_from: Some(t0(Action::LeftStick(StickDir::Up))),
                moved_more: 0,
                shared: 0
            })
        );
        assert_eq!(e.mode(), Mode::Keyboard);
        // Autorepeat i key-up se spolknou (OS key-down neviděl)…
        assert!(down(&mut e, KeyId::W).suppress);
        assert!(up(&mut e, KeyId::W).suppress);
        // …a další stisk v Klávesnici už jde normálně do OS.
        assert!(!down(&mut e, KeyId::W).suppress);
    }

    #[test]
    fn nemapovatelne_klavesy_jdou_do_os_i_pri_prirazovani() {
        let mut e = engine();
        let before = e.mapping().clone();
        let _ = e.start_binding(
            t0(Action::LeftTrigger),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        for k in [KeyId::ALTGR_FAKE_CTRL, KeyId::new(0), KeyId::new(0x80)] {
            let d = down(&mut e, k);
            assert!(!d.suppress, "{k} jde do OS");
            assert_eq!(
                d.ui,
                Some(UiEvent::BindingRejected {
                    key: k,
                    reason: BindingReject::Unmappable
                })
            );
            assert!(!up(&mut e, k).suppress);
            assert!(matches!(e.mode(), Mode::Binding { .. }));
        }
        assert_eq!(e.held_len(), 0, "nemapovatelné se nesledují");
        assert_eq!(e.mapping(), &before);
    }

    #[test]
    fn win_jde_do_os_i_pri_prirazovani() {
        // Přiřazování (i ze hry): Win se nepřiřadí ani nespolkne — otevře
        // Start — a okno se dozví, že patří Windows.
        let mut e = gamepad();
        let before = e.mapping().clone();
        let _ = e.start_binding(
            t0(Action::LeftTrigger),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        for k in [KeyId::LEFT_WIN, KeyId::RIGHT_WIN] {
            let d = down(&mut e, k);
            assert!(!d.suppress, "{k} jde do OS");
            assert!(d.pads.is_empty());
            assert_eq!(
                d.ui,
                Some(UiEvent::BindingRejected {
                    key: k,
                    reason: BindingReject::Reserved
                })
            );
            assert_eq!(e.held(k), None, "{k} se nesleduje");
            assert_eq!(up(&mut e, k), Decision::NONE);
            assert!(matches!(e.mode(), Mode::Binding { .. }), "čeká se dál");
        }
        assert_eq!(e.held_len(), 0);
        assert_eq!(e.mapping(), &before);
        // Další klávesa se přiřadí a hra pokračuje.
        let d = down(&mut e, KeyId::X);
        assert!(matches!(d.ui, Some(UiEvent::BindingSaved { .. })));
        assert_eq!(e.mode(), Mode::Gamepad);
    }

    #[test]
    fn win_ve_hre_jde_do_os() {
        let mut e = gamepad();
        let _ = down(&mut e, KeyId::W);
        for k in [KeyId::LEFT_WIN, KeyId::RIGHT_WIN] {
            let d = down(&mut e, k);
            assert_eq!(d, Decision::NONE, "{k}: do OS, nic se nemění");
            assert_eq!(up(&mut e, k), Decision::NONE);
            // Hook ji ani nepřevezme jako klávesu OS — engine o ní neví.
            assert!(!e.adopt_os_key(k, T0));
        }
        assert_eq!(e.held_len(), 1, "jen W");
        assert_eq!(stick(e.pad_state(P0)), (0, AXIS_MAX));
    }

    /// Alt+Tab z okna KeyPadu při přiřazování (kontrolní seznam
    /// vlastníka, bod 10; OQ 55): Alt i Tab jdou do Windows a nic se
    /// nepřiřadí — přiřazování pak zruší ztráta popředí (hook). Totéž
    /// Alt+F4, z Klávesnice i ze hry.
    #[test]
    fn alt_tab_pri_prirazovani_patri_windows() {
        let tab = KeyId::new(0x0F);
        let f4 = KeyId::new(0x3E);
        for mut e in [engine(), gamepad()] {
            let pred = e.mapping().clone();
            let rev = e.mapping_rev();
            let _ = e.start_binding(
                t0(Action::Button(PadButton::A)),
                BindKind::Replace,
                KeyConflict::Move,
                T0,
            );
            for k in [tab, f4] {
                assert_eq!(down(&mut e, KeyId::LEFT_ALT), Decision::NONE, "Alt Windows");
                assert_eq!(down(&mut e, KeyId::LEFT_ALT), Decision::NONE, "autorepeat");
                assert_eq!(down(&mut e, k), Decision::NONE, "{k} Windows");
                assert_eq!(up(&mut e, k), Decision::NONE);
                assert_eq!(
                    up(&mut e, KeyId::LEFT_ALT),
                    Decision::NONE,
                    "Alt+{k} se nepřiřadí"
                );
                assert!(matches!(e.mode(), Mode::Binding { .. }), "čeká se dál");
            }
            assert_eq!(e.mapping(), &pred);
            assert_eq!(e.mapping_rev(), rev);
            assert_eq!(e.held_len(), 0);
        }
    }

    /// Samotné ťuknutí modifikátorem se přiřadí při uvolnění. Stisk
    /// i key-up jdou Windows (princip 2). Ze hry se pak hraje dál.
    #[test]
    fn tuknuti_modifikatorem_se_priradi_pri_uvolneni() {
        let cil = t0(Action::Button(PadButton::Lb));
        for k in MODIFIERS {
            let mut e = gamepad();
            let odkud = e.mapping().targets(k).first().filter(|&t| t != cil);
            let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, T0);
            assert_eq!(down(&mut e, k), Decision::NONE, "{k}: stisk Windows");
            assert_eq!(down(&mut e, k), Decision::NONE, "{k}: autorepeat nic");
            assert!(matches!(e.mode(), Mode::Binding { .. }), "{k}");
            let d = up(&mut e, k);
            assert!(!d.suppress, "{k}: key-up Windows jako key-down");
            assert_eq!(
                d.ui,
                Some(UiEvent::BindingSaved {
                    key: k,
                    target: cil.into(),
                    moved_from: odkud,
                    moved_more: 0,
                    shared: 0
                }),
                "{k}"
            );
            assert_eq!(e.mode(), Mode::Gamepad, "{k}: zpět do hry");
            assert_eq!(e.mapping().targets(k).first(), Some(cil));
            assert_eq!(e.held_len(), 0);
        }
    }

    /// Ctrl+Shift+Esc: Esc s drženým modifikátorem je zkratka Windows
    /// (Správce úloh), ne zrušení přiřazování. Po puštění modifikátorů
    /// se klávesa zase přiřazuje.
    #[test]
    fn ctrl_shift_esc_pri_prirazovani_patri_windows() {
        let mut e = engine();
        let _ = e.start_binding(
            t0(Action::Button(PadButton::A)),
            BindKind::Add,
            KeyConflict::Move,
            T0,
        );
        for k in [KeyId::LEFT_CTRL, KeyId::LEFT_SHIFT, KeyId::ESC] {
            assert_eq!(down(&mut e, k), Decision::NONE, "{k} Windows");
        }
        assert!(matches!(e.mode(), Mode::Binding { .. }), "Esc nezrušil");
        for k in [KeyId::ESC, KeyId::LEFT_CTRL, KeyId::LEFT_SHIFT] {
            assert_eq!(up(&mut e, k), Decision::NONE, "{k}: nic se nepřiřadí");
        }
        let d = down(&mut e, KeyId::X);
        assert!(d.suppress);
        assert!(matches!(
            d.ui,
            Some(UiEvent::BindingSaved { key: KeyId::X, .. })
        ));
    }

    /// Modifikátor, který Windows drží už z doby před kliknutím (Ctrl
    /// v Klávesnici), dělá zkratku i při přiřazování — Ctrl+C není vazba
    /// C. Modifikátor spolknutý ze hry (levý Shift = L3) Windows neviděly,
    /// takže zkratku netvoří.
    #[test]
    fn zkratku_dela_jen_modifikator_drzeny_windows() {
        let cil = t0(Action::Button(PadButton::A));
        let mut e = engine();
        assert!(!down(&mut e, KeyId::LEFT_CTRL).suppress);
        let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, T0);
        assert_eq!(down(&mut e, KeyId::C), Decision::NONE, "Ctrl+C Windows");
        assert_eq!(up(&mut e, KeyId::C), Decision::NONE);
        assert_eq!(
            up(&mut e, KeyId::LEFT_CTRL),
            Decision::NONE,
            "Ctrl se neťukl při přiřazování"
        );
        assert!(matches!(e.mode(), Mode::Binding { .. }));

        let mut e = gamepad();
        assert!(down(&mut e, KeyId::LEFT_SHIFT).suppress, "L3");
        let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, T0);
        let d = down(&mut e, KeyId::C);
        assert!(d.suppress);
        assert!(matches!(
            d.ui,
            Some(UiEvent::BindingSaved { key: KeyId::C, .. })
        ));
        assert!(up(&mut e, KeyId::LEFT_SHIFT).suppress, "key-up L3 spolknut");
    }

    /// Ťuknutí ruší každý jiný stisk (zkratka, Win, AltGr, druhý Alt)
    /// i konec přiřazování — key-up po novém začátku nic nepřiřadí.
    #[test]
    fn jiny_stisk_nebo_konec_zrusi_tuknuti() {
        let cil = t0(Action::Button(PadButton::A));
        for jiny in [
            TOGGLE,
            KeyId::LEFT_WIN,
            KeyId::ALTGR_FAKE_CTRL,
            KeyId::RIGHT_ALT,
        ] {
            let mut e = engine();
            let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, T0);
            let _ = down(&mut e, KeyId::LEFT_ALT);
            let _ = down(&mut e, jiny);
            let _ = up(&mut e, jiny);
            assert_eq!(up(&mut e, KeyId::LEFT_ALT).ui, None, "Alt s {jiny}");
            assert!(matches!(e.mode(), Mode::Binding { .. }), "{jiny}");
        }
        let mut e = engine();
        let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, T0);
        let _ = down(&mut e, KeyId::LEFT_ALT);
        let _ = e.start_binding(
            t0(Action::Button(PadButton::B)),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        assert_eq!(
            up(&mut e, KeyId::LEFT_ALT),
            Decision::NONE,
            "nové přiřazování"
        );
        let _ = down(&mut e, KeyId::LEFT_ALT);
        let _ = e.cancel_binding(T0);
        assert_eq!(up(&mut e, KeyId::LEFT_ALT), Decision::NONE, "zrušené");
        assert_eq!(e.mapping(), &Mapping::default());
    }

    /// AltGr na českém rozložení: falešný levý Ctrl (0x21D), pak pravý
    /// Alt; puštění v tomtéž pořadí. Okno dostane jediné odmítnutí
    /// („Tuhle klávesu nejde použít") a nic se nepřiřadí — ani pravý Alt
    /// ťuknutím. Obojí jde Windows. Přiřazování čeká dál a další klávesa
    /// se přiřadí. Pravý Alt bez falešného Ctrl (anglické rozložení) se
    /// ťuknutím přiřadí dál (`tuknuti_modifikatorem_se_priradi_pri_uvolneni`).
    #[test]
    fn altgr_pri_prirazovani_se_neprirazuje() {
        let cil = t0(Action::Button(PadButton::A));
        for zacatek in [Mode::Keyboard, Mode::Gamepad] {
            let mut e = if zacatek == Mode::Gamepad {
                gamepad()
            } else {
                engine()
            };
            let rev = e.mapping_rev();
            let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, T0);
            // Držený AltGr opakuje obě události (autorepeat).
            for opakovani in 0..2 {
                let d = down(&mut e, KeyId::ALTGR_FAKE_CTRL);
                assert_eq!(
                    d,
                    Decision {
                        ui: Some(UiEvent::BindingRejected {
                            key: KeyId::ALTGR_FAKE_CTRL,
                            reason: BindingReject::Unmappable
                        }),
                        ..Decision::NONE
                    },
                    "{zacatek:?} {opakovani}: falešný Ctrl"
                );
                assert_eq!(
                    down(&mut e, KeyId::RIGHT_ALT),
                    Decision::NONE,
                    "{zacatek:?} {opakovani}: pravý Alt Windows, nic"
                );
            }
            assert_eq!(up(&mut e, KeyId::ALTGR_FAKE_CTRL), Decision::NONE);
            assert_eq!(
                up(&mut e, KeyId::RIGHT_ALT),
                Decision::NONE,
                "{zacatek:?}: puštění AltGr nic nepřiřadí a jde Windows"
            );
            assert!(matches!(e.mode(), Mode::Binding { .. }), "{zacatek:?}");
            assert_eq!(e.mapping_rev(), rev, "{zacatek:?}");
            assert_eq!(e.mapping().targets(KeyId::RIGHT_ALT).first(), None);
            assert_eq!(e.held_len(), 0);

            // Přiřazování běží dál: příští klávesa se přiřadí, AltGr
            // z ní nic nedělá.
            let d = down(&mut e, KeyId::X);
            assert!(d.suppress);
            assert!(matches!(
                d.ui,
                Some(UiEvent::BindingSaved { key: KeyId::X, .. })
            ));
            assert!(up(&mut e, KeyId::X).suppress);
        }

        // Falešný Ctrl následovaný jinou klávesou (tady Win): pravý Alt až
        // potom je zase obyčejné ťuknutí.
        let mut e = engine();
        let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, T0);
        let _ = down(&mut e, KeyId::ALTGR_FAKE_CTRL);
        let _ = up(&mut e, KeyId::ALTGR_FAKE_CTRL);
        let _ = down(&mut e, KeyId::LEFT_WIN);
        let _ = up(&mut e, KeyId::LEFT_WIN);
        let _ = down(&mut e, KeyId::RIGHT_ALT);
        assert!(matches!(
            up(&mut e, KeyId::RIGHT_ALT).ui,
            Some(UiEvent::BindingSaved {
                key: KeyId::RIGHT_ALT,
                ..
            })
        ));

        // Falešný Ctrl mimo přiřazování nic nepamatuje: přiřazování
        // začaté až potom pravý Alt ťuknutím přiřadí.
        let mut e = engine();
        let _ = down(&mut e, KeyId::ALTGR_FAKE_CTRL);
        let _ = up(&mut e, KeyId::ALTGR_FAKE_CTRL);
        let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, T0);
        let _ = down(&mut e, KeyId::RIGHT_ALT);
        assert!(matches!(
            up(&mut e, KeyId::RIGHT_ALT).ui,
            Some(UiEvent::BindingSaved {
                key: KeyId::RIGHT_ALT,
                ..
            })
        ));
    }

    /// Klávesa převzatá jako klávesa Windows (snímek po instalaci hooku:
    /// držená před přiřazováním) se při přiřazování nepřiřadí — její
    /// autorepeat jde Windows. Po uvolnění se nový stisk přiřadí. Proto
    /// hook převzetí nesmí dělat podle stavu klávesnice v callbacku: kdyby
    /// Windows hlásily dole i právě stisknutou klávesu, převzala by se
    /// a nepřiřadilo by se nic, ani Esc by nerušil. Stav OS se navíc mění
    /// až po hooku, takže o rozhodované klávese nic neřekne, a dotazy
    /// dělaly ocas ceny callbacku (OQ 57; výklad, že tohle u vlastníka
    /// opravdu nastalo, je nejspíš mylný, OQ 60).
    #[test]
    fn prevzata_klavesa_se_neprirazuje_az_novy_stisk() {
        let mut e = Engine::new(Mapping::default());
        let _ = e.enable(P0);
        let cil = t0(Action::Button(PadButton::B));
        let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, T0);
        for k in [KeyId::F, KeyId::ESC] {
            assert!(e.adopt_os_key(k, T0));
            let d = e.on_key(k, true, T0);
            assert_eq!((d.suppress, d.ui), (false, None), "{k} jde Windows");
            assert!(matches!(e.mode(), Mode::Binding { .. }), "{k}");
            assert!(!e.on_key(k, false, T0).suppress);
        }
        let d = e.on_key(KeyId::F, true, T0);
        assert!(d.suppress);
        assert!(
            matches!(d.ui, Some(UiEvent::BindingSaved { key: KeyId::F, target, .. }) if target == cil.into()),
            "{:?}",
            d.ui
        );
    }

    #[test]
    fn klavesy_bez_scan_kodu_si_nesdileji_zaznam() {
        // Dvě mediální klávesy (obě scan 0) se v enginu nesmí plést:
        // žádná z nich se nesleduje, všechno jde do OS.
        let mut e = engine();
        let media = KeyId::new(0);
        assert!(!down(&mut e, media).suppress);
        let _ = e.start_binding(
            t0(Action::LeftTrigger),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        assert!(!down(&mut e, media).suppress);
        assert!(!up(&mut e, media).suppress);
        assert!(!down(&mut e, media).suppress);
        assert!(!up(&mut e, media).suppress);
        assert_eq!(e.held_len(), 0);
    }

    #[test]
    fn prikazy_berou_v_potaz_propadle_prirazovani() {
        // Časovač nestihl tiknout; příkaz z okna nesmí odpovědět, že se
        // pořád přiřazuje.
        let late = T0 + BINDING_TIMEOUT_MS + 1;

        let mut e = engine();
        let _ = e.start_binding(
            t0(Action::LeftTrigger),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        let d = e.start_binding(
            t0(Action::RightTrigger),
            BindKind::Replace,
            KeyConflict::Move,
            late,
        );
        assert_eq!(
            e.mode(),
            Mode::Binding {
                target: t0(Action::RightTrigger).into(),
                started_at_ms: late
            }
        );
        assert!(matches!(d.ui, Some(UiEvent::ModeChanged { .. })));

        let mut e = engine();
        let _ = e.start_binding(
            t0(Action::LeftTrigger),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        let _ = e.toggle(late);
        assert_eq!(e.mode(), Mode::Gamepad);

        let mut e = engine();
        let _ = e.start_binding(
            t0(Action::LeftTrigger),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        assert_eq!(
            e.cancel_binding(late).ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Timeout
            })
        );

        // capture po propadlém přiřazování zapne zachytávání hned.
        let mut e = engine();
        let _ = e.start_binding(
            t0(Action::LeftTrigger),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        let d = e.capture(late);
        assert_eq!(e.mode(), Mode::Gamepad);
        assert_eq!(
            d.ui,
            Some(UiEvent::ModeChanged {
                mode: Mode::Gamepad,
                cause: ModeCause::Gui
            }),
            "novější událost vyhrává"
        );
    }

    #[test]
    fn binding_autorepeat_drzene_klavesy_nic_neprirazuje() {
        // Klávesa držená už před začátkem přiřazování patří OS; její
        // autorepeat není nový stisk, takže se nepřiřadí.
        let mut e = engine();
        assert!(!down(&mut e, KeyId::W).suppress);
        let before = e.mapping().clone();
        let _ = e.start_binding(
            t0(Action::LeftTrigger),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        let r = down(&mut e, KeyId::W);
        assert!(!r.suppress);
        assert_eq!(r.ui, None);
        assert!(matches!(e.mode(), Mode::Binding { .. }));
        assert_eq!(e.mapping(), &before);
        assert!(!up(&mut e, KeyId::W).suppress);
    }

    #[test]
    fn nenamapovane_klavesy_v_gamepadu_jdou_do_os() {
        let mut e = gamepad();
        for k in [
            KeyId::X,
            KeyId::NUMPAD_8,
            KeyId::ALTGR_FAKE_CTRL,
            KeyId::LEFT_CTRL,
        ] {
            let d = down(&mut e, k);
            assert!(!d.suppress, "{k}");
            assert!(d.pads.is_empty());
            assert!(!up(&mut e, k).suppress, "{k}");
        }
    }

    #[test]
    fn altgr_nespousti_akci_na_lctrl() {
        let mut m = Mapping::default();
        m.bind(
            KeyId::LEFT_CTRL,
            t0(Action::Button(PadButton::B)),
            KeyConflict::Move,
        )
        .unwrap();
        let mut e = engine_with(m);
        let _ = e.toggle(T0);
        let d = down(&mut e, KeyId::ALTGR_FAKE_CTRL);
        assert!(!d.suppress);
        assert!(e.pad_state(P0).is_neutral());
    }

    #[test]
    fn klavesnice_nic_nepotlacuje_krome_zkratky() {
        let mut e = engine();
        for k in [KeyId::W, KeyId::SPACE, KeyId::ARROW_UP, KeyId::ESC] {
            assert!(!down(&mut e, k).suppress);
            assert!(!up(&mut e, k).suppress);
        }
        assert!(e.pad_state(P0).is_neutral());
    }

    #[test]
    fn toggle_z_okna_funguje_jako_zkratka() {
        let mut e = engine();
        let d = e.toggle(T0);
        assert!(!d.suppress);
        assert_eq!(
            d.ui,
            Some(UiEvent::ModeChanged {
                mode: Mode::Gamepad,
                cause: ModeCause::Gui
            })
        );
        let _ = down(&mut e, KeyId::W);
        let d = e.toggle(T0);
        assert_eq!(e.mode(), Mode::Keyboard);
        assert!(vse_neutralni(&d));
        assert!(up(&mut e, KeyId::W).suppress);
    }

    #[test]
    fn disabled_nepusti_na_gamepad_a_propousti_klavesy() {
        let mut e = engine();
        let _ = e.disable(P0, DisabledReason::ViGEmMissing);
        let d = down(&mut e, TOGGLE);
        assert!(d.suppress, "zkratka se spolkne vždy");
        assert_eq!(
            d.ui,
            Some(UiEvent::ToggleRejected {
                reason: ToggleReject::Disabled(DisabledReason::ViGEmMissing)
            })
        );
        assert!(!down(&mut e, KeyId::W).suppress);
        assert_eq!(
            e.toggle(T0).ui,
            Some(UiEvent::ToggleRejected {
                reason: ToggleReject::Disabled(DisabledReason::ViGEmMissing)
            })
        );
        let d = e.enable(P0);
        assert_eq!(
            e.mode(),
            Mode::Keyboard,
            "po zapnutí Klávesnice, ne Gamepad"
        );
        assert!(matches!(
            d.ui,
            Some(UiEvent::ModeChanged {
                cause: ModeCause::PadStatus,
                ..
            })
        ));
        assert_eq!(e.enable(P0), Decision::NONE, "podruhé nic");
    }

    #[test]
    fn chyba_padu_v_gamepadu_vynuti_klavesnici() {
        let mut e = gamepad();
        let _ = down(&mut e, KeyId::W);
        let d = e.disable(P0, DisabledReason::PadError);
        assert!(vse_neutralni(&d));
        assert_eq!(
            e.mode(),
            Mode::Disabled {
                reason: DisabledReason::PadError
            }
        );
        assert!(up(&mut e, KeyId::W).suppress, "W-up spolknut");
    }

    #[test]
    fn disable_nepripraveneho_ovladace_meni_jen_duvod() {
        let mut e = Engine::new(Mapping::default());
        // Stejný důvod: nic.
        assert_eq!(
            e.disable(P0, DisabledReason::PadNotConnected),
            Decision::NONE
        );
        // Jiný důvod se oznámí.
        let d = e.disable(P1, DisabledReason::ViGEmMissing);
        assert_eq!(
            d.ui,
            Some(UiEvent::ModeChanged {
                mode: Mode::Disabled {
                    reason: DisabledReason::ViGEmMissing
                },
                cause: ModeCause::PadStatus
            })
        );
        assert!(vse_neutralni(&d));
        // S připraveným ovladačem se režim kvůli jinému nemění.
        let _ = e.enable(P0);
        assert_eq!(e.disable(P1, DisabledReason::PadError), Decision::NONE);
        assert_eq!(e.mode(), Mode::Keyboard);
    }

    #[test]
    fn vynucena_klavesnice() {
        let mut e = gamepad();
        let _ = down(&mut e, KeyId::A);
        let d = e.force_keyboard(ForceReason::Watchdog);
        assert_eq!(e.mode(), Mode::Keyboard);
        assert!(vse_neutralni(&d));
        assert_eq!(
            d.ui,
            Some(UiEvent::ModeChanged {
                mode: Mode::Keyboard,
                cause: ModeCause::Forced(ForceReason::Watchdog)
            })
        );
        assert!(up(&mut e, KeyId::A).suppress);
        // V Klávesnici jen znovu potvrdí neutrál, bez oznámení.
        let d = e.force_keyboard(ForceReason::Shutdown);
        assert_eq!(d.ui, None);
        assert!(vse_neutralni(&d));
    }

    #[test]
    fn zamceni_relace_zapomene_drzene_klavesy() {
        let mut e = gamepad();
        let _ = down(&mut e, KeyId::W);
        let _ = down(&mut e, KeyId::X); // OS
        let d = e.reset_held(ForceReason::SessionLock);
        assert!(vse_neutralni(&d));
        assert_eq!(e.mode(), Mode::Keyboard);
        assert_eq!(e.held_len(), 0);
        // Key-upy po odemčení nemají záznam → jdou do OS.
        assert!(!up(&mut e, KeyId::W).suppress);
        assert!(!up(&mut e, KeyId::X).suppress);
    }

    /// Revize Fáze 4: uživatel drží W (OS jeho stisk viděl), mezitím se
    /// hook nainstaluje / přeinstaluje (engine o W neví) a zapne se
    /// zachytávání. Autorepeat W by byl „nový stisk" ovladače — spolkl
    /// by se i key-up a W by v OS viselo. Hook proto W předem ohlásí
    /// jako klávesu OS (`adopt_os_key`).
    #[test]
    fn klavesa_drzena_os_pred_zachytavanim_nevisi() {
        let mut e = engine();
        let _ = e.reset_held(ForceReason::HookReinstalled);
        let _ = e.capture(T0);
        assert_eq!(e.mode(), Mode::Gamepad);
        assert!(e.adopt_os_key(KeyId::LEFT_SHIFT, T0));
        assert!(!e.adopt_os_key(KeyId::LEFT_SHIFT, T0), "podruhé nic");
        let r = down(&mut e, KeyId::LEFT_SHIFT);
        assert!(!r.suppress, "autorepeat dál do OS");
        assert!(r.pads.is_empty() && r.ui.is_none());
        assert!(!up(&mut e, KeyId::LEFT_SHIFT).suppress, "key-up do OS");
        // Další stisk už patří ovladači (L3).
        assert!(down(&mut e, KeyId::LEFT_SHIFT).suppress);
        // Sledovaná ani nemapovatelná klávesa se nepřebírá.
        assert!(!e.adopt_os_key(KeyId::LEFT_SHIFT, T0));
        assert!(!e.adopt_os_key(KeyId::ALTGR_FAKE_CTRL, T0));
        assert!(!e.adopt_os_key(KeyId::new(0), T0));
        // Zkratka držená OS se přebere taky — její autorepeat nepřepíná.
        assert!(e.adopt_os_key(TOGGLE, T0));
        let r = down(&mut e, TOGGLE);
        assert!(!r.suppress && r.ui.is_none());
        assert_eq!(e.mode(), Mode::Gamepad);
    }

    /// Revize opravy 6. 10. (OQ 39, 55): klávesa OS, jejíž key-up hook
    /// neviděl (Ctrl+Shift+Esc → Správce úloh s právy správce), zůstane
    /// v `held` napořád — záznam OS nezastará. Levý Ctrl pak při
    /// přiřazování dělá z každého stisku zkratku Windows a Esc
    /// („autorepeat" klávesy OS) přiřazování nezruší. Hook na začátku
    /// přiřazování zapomene klávesy, které Windows nedrží
    /// (`forget_os_key`) — pak F i Esc fungují.
    #[test]
    fn zastaraly_zaznam_os_po_zapomenuti_neblokuje_prirazovani() {
        let cil = t0(Action::Button(PadButton::B));
        for zapomenout in [false, true] {
            let mut e = engine();
            // Pauza se zapnutým ovladačem: Ctrl, levý Shift a Esc patří
            // Windows a jejich key-upy se ztratily.
            let _ = down(&mut e, KeyId::LEFT_CTRL);
            let _ = down(&mut e, KeyId::LEFT_SHIFT);
            let _ = down(&mut e, KeyId::ESC);
            let pozdeji = T0 + 60_000;
            if zapomenout {
                for k in [KeyId::LEFT_CTRL, KeyId::LEFT_SHIFT, KeyId::ESC] {
                    assert!(e.forget_os_key(k), "{k}");
                }
                assert_eq!(e.held_len(), 0);
            }
            let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, pozdeji);
            let d = e.on_key(KeyId::F, true, pozdeji + 10);
            if zapomenout {
                assert!(d.suppress);
                assert!(matches!(d.ui, Some(UiEvent::BindingSaved { key, .. }) if key == KeyId::F));
                assert_eq!(e.mapping().targets(KeyId::F).first(), Some(cil));
            } else {
                // Bez zapomenutí: F patří Windows (Ctrl+F), nic se
                // neuloží, a Esc je „autorepeat" a přiřazování nezruší.
                assert!(!d.suppress && d.ui.is_none(), "{d:?}");
                let esc = e.on_key(KeyId::ESC, true, pozdeji + 20);
                assert!(!esc.suppress && esc.ui.is_none(), "{esc:?}");
                assert!(matches!(e.mode(), Mode::Binding { .. }));
            }
        }
        // Esc sám: zapomenutý se zase dá použít ke zrušení.
        let mut e = engine();
        let _ = down(&mut e, KeyId::ESC);
        assert!(e.forget_os_key(KeyId::ESC));
        let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, T0);
        let d = down(&mut e, KeyId::ESC);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Escape
            })
        );
    }

    /// `forget_os_key` bere jen záznamy OS — klávesu ovladače ani
    /// spolknutou Windows neviděly — a nemění režim ani ovladače.
    /// Čekající ťuknutí modifikátorem zapomenuté klávesy zmizí: její
    /// key-up už nic neuloží.
    #[test]
    fn forget_os_key_jen_klavesy_os() {
        let mut e = gamepad();
        assert!(down(&mut e, KeyId::W).suppress, "W ovladači");
        assert!(!down(&mut e, KeyId::X).suppress, "X Windows");
        assert!(!e.forget_os_key(KeyId::W), "klávesa ovladače zůstane");
        assert!(!e.forget_os_key(TOGGLE), "nedržená nic");
        assert!(
            !e.forget_os_key(KeyId::ALTGR_FAKE_CTRL),
            "nemapovatelná nic"
        );
        let pred = e.pad_state(P0);
        assert!(e.forget_os_key(KeyId::X));
        assert!(!e.forget_os_key(KeyId::X), "podruhé nic");
        assert_eq!((e.mode(), e.pad_state(P0)), (Mode::Gamepad, pred));
        assert_eq!(
            e.held(KeyId::W).unwrap().owner,
            Owner::Pad(t0(Action::LeftStick(StickDir::Up)).into())
        );
        // Spolknutá (Swallow po pauze) zůstane taky.
        let _ = e.toggle(T0);
        assert_eq!(e.held(KeyId::W).unwrap().owner, Owner::Swallow);
        assert!(!e.forget_os_key(KeyId::W));
        // Ťuknutí Altem při přiřazování: po zapomenutí Altu se key-up
        // nic neuloží (Windows ho mezitím pustily jinde).
        let cil = t0(Action::Button(PadButton::Y));
        let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, T0);
        assert!(!down(&mut e, KeyId::LEFT_ALT).suppress);
        assert!(e.forget_os_key(KeyId::LEFT_ALT));
        let d = up(&mut e, KeyId::LEFT_ALT);
        assert!(!d.suppress && d.ui.is_none(), "{d:?}");
        assert!(matches!(e.mode(), Mode::Binding { .. }));
        assert_ne!(e.mapping().targets(KeyId::LEFT_ALT).first(), Some(cil));
    }

    #[test]
    fn panika_v_hooku_nenecha_klavesu_viset_v_os() {
        // Hook dostal od enginu „spolknout W" (klávesa padu), pak zpanikařil
        // a klávesu podle fail-safe propustil do OS. reset_held zajistí,
        // že key-up W dostane OS taky.
        let mut e = gamepad();
        assert!(down(&mut e, KeyId::W).suppress);
        let _ = e.reset_held(ForceReason::HookPanic);
        assert!(!down(&mut e, KeyId::W).suppress, "autorepeat do OS");
        assert!(!up(&mut e, KeyId::W).suppress, "key-up do OS");
    }

    #[test]
    fn ztraceny_key_up_klavesy_padu() {
        // Uživatel drží W, objeví se výzva UAC (hook nevidí key-up).
        let mut e = gamepad();
        let _ = e.on_key(KeyId::W, true, T0);
        // Po návratu stiskne W znovu — to není autorepeat, ale nový stisk.
        let d = e.on_key(KeyId::W, true, T0 + 60_000);
        assert!(d.suppress);
        assert_eq!(e.held(KeyId::W).unwrap().seq, 3, "nový stisk");
        assert_eq!(stick(e.pad_state(P0)), (0, AXIS_MAX));
        let d = e.on_key(KeyId::W, false, T0 + 60_100);
        assert!(pad0(&d).unwrap().is_neutral(), "a jeho key-up páčku pustí");
    }

    #[test]
    fn ztraceny_key_up_nespolkne_dalsi_stisk_v_klavesnici() {
        let mut e = gamepad();
        let _ = e.on_key(KeyId::W, true, T0);
        // Přepnutí na Klávesnici udělá z W záznam Swallow.
        let _ = e.toggle(T0 + 10);
        // Key-up W se ztratil; o minutu později nový stisk v Klávesnici.
        let d = e.on_key(KeyId::W, true, T0 + 60_000);
        assert!(!d.suppress, "klávesnice se nesmí ztratit");
        assert!(!e.on_key(KeyId::W, false, T0 + 60_050).suppress);
    }

    #[test]
    fn ztraceny_key_up_zkratky_neblokuje_dalsi_prepnuti() {
        let mut e = engine();
        let _ = e.on_key(TOGGLE, true, T0);
        assert_eq!(e.mode(), Mode::Gamepad);
        // key-up zkratky se ztratil
        let d = e.on_key(TOGGLE, true, T0 + 5_000);
        assert_eq!(e.mode(), Mode::Keyboard, "druhý stisk přepnul");
        assert!(d.suppress);
    }

    #[test]
    fn os_klavesa_se_po_pauze_nepreklasifikuje() {
        // Klávesa patřící OS: OS viděl key-down, takže ani po dlouhé
        // pauze se nesmí začít polykat (jinak by v OS visela).
        let mut e = engine();
        let _ = e.on_key(KeyId::W, true, T0);
        let _ = e.toggle(T0 + 10);
        let d = e.on_key(KeyId::W, true, T0 + 60_000);
        assert!(!d.suppress);
        assert!(!e.on_key(KeyId::W, false, T0 + 60_010).suppress);
    }

    #[test]
    fn zmena_mapovani_v_gamepadu_vynuti_klavesnici() {
        let mut e = gamepad();
        let _ = down(&mut e, KeyId::W);
        let mut m = Mapping::default();
        m.unbind(KeyId::W).unwrap();
        let d = e.set_mapping(m);
        assert_eq!(e.mode(), Mode::Keyboard);
        assert!(vse_neutralni(&d));
        assert!(up(&mut e, KeyId::W).suppress);
        assert_eq!(e.mapping().targets(KeyId::W).first(), None);
    }

    #[test]
    fn prepnuti_nezasekne_os_klavesy_ani_pad() {
        // Klávesa stisknutá v Klávesnici, pak Gamepad, pak zpět: OS
        // dostane přesně jeden key-down a jeden key-up.
        let mut e = engine();
        assert!(!down(&mut e, KeyId::SPACE).suppress);
        let _ = e.toggle(T0);
        let _ = e.toggle(T0);
        let _ = e.toggle(T0);
        assert!(!up(&mut e, KeyId::SPACE).suppress);
        assert!(e.pad_state(P0).is_neutral());
        assert_eq!(e.held_len(), 0);
    }

    #[test]
    fn sipka_a_numpad_se_nepletou() {
        let mut e = gamepad();
        assert!(
            !down(&mut e, KeyId::NUMPAD_8).suppress,
            "numpad 8 není namapovaný"
        );
        let d = down(&mut e, KeyId::ARROW_UP);
        assert!(d.suppress);
        assert_eq!(pad0(&d).unwrap().thumb_ry, AXIS_MAX);
    }

    #[test]
    fn udalosti_jdou_serializovat() {
        let ev = UiEvent::ModeChanged {
            mode: Mode::Binding {
                target: PadAction::new(P1, Action::Button(PadButton::Lb)).into(),
                started_at_ms: 5,
            },
            cause: ModeCause::Forced(ForceReason::Watchdog),
        };
        let mut pads = PadUpdates::NONE;
        pads.set(P2, PadState::NEUTRAL);
        let d = Decision {
            suppress: true,
            pads,
            ui: Some(ev),
        };
        // serde_json v core není; stačí, že se typy dají předat
        // serializátoru — ověří to kompilace téhle funkce.
        fn je_serde<T: Serialize + for<'de> Deserialize<'de>>(_: &T) {}
        je_serde(&d);
        je_serde(&MappingError::Empty);
        je_serde(&P1);
        je_serde(&Owner::Pad(t0(Action::LeftTrigger).into()));
    }

    // ── Víc ovladačů (Fáze 4) ────────────────────────────────────────

    /// Výchozí mapování (první ovladač) + IJKL jako levá páčka druhého
    /// ovladače a X jako jeho tlačítko A.
    fn dva_ovladace() -> Mapping {
        let mut m = Mapping::default();
        for (k, d) in [
            (KeyId::I, StickDir::Up),
            (KeyId::J, StickDir::Left),
            (KeyId::K, StickDir::Down),
            (KeyId::L, StickDir::Right),
        ] {
            m.bind(
                k,
                PadAction::new(P1, Action::LeftStick(d)),
                KeyConflict::Move,
            )
            .unwrap();
        }
        m.bind(
            KeyId::X,
            PadAction::new(P1, Action::Button(PadButton::A)),
            KeyConflict::Move,
        )
        .unwrap();
        m
    }

    /// Oba ovladače připravené, zachytává se.
    fn hraji_dva() -> Engine {
        let mut e = engine_with(dva_ovladace());
        let _ = e.enable(P1);
        let _ = e.capture(T0);
        assert_eq!(e.mode(), Mode::Gamepad);
        e
    }

    #[test]
    fn klavesy_nepripraveneho_ovladace_jdou_do_os() {
        let mut e = engine_with(dva_ovladace());
        let _ = e.capture(T0);
        assert!(!e.is_ready(P1));
        // Klávesa druhého ovladače jde do OS i v Gamepadu.
        let d = down(&mut e, KeyId::X);
        assert!(!d.suppress);
        assert!(d.pads.is_empty());
        assert!(down(&mut e, KeyId::W).suppress, "první ovladač hraje");
        // Ovladač se připojí, X je pořád dole: patří dál OS (princip 2).
        assert_eq!(e.enable(P1), Decision::NONE);
        assert!(e.is_ready(P1));
        assert_eq!(e.mode(), Mode::Gamepad);
        let r = down(&mut e, KeyId::X);
        assert!(!r.suppress && r.pads.is_empty());
        assert!(!up(&mut e, KeyId::X).suppress);
        assert!(e.pad_state(P1).is_neutral());
        // Nový stisk už patří druhému ovladači.
        let d = down(&mut e, KeyId::X);
        assert!(d.suppress);
        assert_eq!(pad0(&d), None, "první ovladač se nemění");
        assert!(d.pads.get(P1).unwrap().is_pressed(PadButton::A));
    }

    #[test]
    fn dva_ovladace_maji_nezavisle_stavy() {
        let mut e = hraji_dva();
        let d = down(&mut e, KeyId::A);
        assert_eq!(stick(pad0(&d).unwrap()), (-AXIS_MAX, 0));
        assert_eq!(d.pads.get(P1), None);
        // L je vpravo na druhém ovladači — s A prvního se nepere.
        let d = down(&mut e, KeyId::L);
        assert_eq!(pad0(&d), None);
        assert_eq!(stick(d.pads.get(P1).unwrap()), (AXIS_MAX, 0));
        // SOCD jen v rámci ovladače: J (novější) přebije L…
        let d = down(&mut e, KeyId::J);
        assert_eq!(stick(d.pads.get(P1).unwrap()), (-AXIS_MAX, 0));
        // …a D přebije A, druhý ovladač se nehne.
        let d = down(&mut e, KeyId::D);
        assert_eq!(stick(pad0(&d).unwrap()), (AXIS_MAX, 0));
        assert_eq!(d.pads.get(P1), None);
        let d = up(&mut e, KeyId::J);
        assert_eq!(stick(d.pads.get(P1).unwrap()), (AXIS_MAX, 0), "zpět k L");
        assert_eq!(stick(e.pad_state(P0)), (AXIS_MAX, 0));
        for k in [KeyId::A, KeyId::L, KeyId::D] {
            assert!(up(&mut e, k).suppress);
        }
        assert!(e.pad_state(P0).is_neutral() && e.pad_state(P1).is_neutral());
        assert_eq!(e.held_len(), 0);
    }

    #[test]
    fn vypadek_jednoho_ovladace_spolkne_jen_jeho_klavesy() {
        let mut e = hraji_dva();
        let _ = down(&mut e, KeyId::W);
        let _ = down(&mut e, KeyId::I);
        let d = e.disable(P1, DisabledReason::PadError);
        assert_eq!(e.mode(), Mode::Gamepad, "první ovladač hraje dál");
        assert_eq!(d.ui, None);
        assert_eq!(
            d.pads.iter().collect::<Vec<_>>(),
            vec![(P1, PadState::NEUTRAL)],
            "neutrál jen pro vypadlý ovladač"
        );
        assert!(!e.is_ready(P1));
        assert_eq!(e.held(KeyId::I).unwrap().owner, Owner::Swallow);
        assert!(matches!(e.held(KeyId::W).unwrap().owner, Owner::Pad(_)));
        assert_eq!(stick(e.pad_state(P0)), (0, AXIS_MAX));
        // Klávesy druhého ovladače teď jdou do OS…
        let k = down(&mut e, KeyId::K);
        assert!(!k.suppress && k.pads.is_empty());
        assert!(!up(&mut e, KeyId::K).suppress);
        // …jen I, jehož key-down OS nevidělo, se spolkne až do konce.
        let u = up(&mut e, KeyId::I);
        assert!(u.suppress && u.pads.is_empty());
        let u = up(&mut e, KeyId::W);
        assert!(u.suppress);
        assert_eq!(pad0(&u), Some(PadState::NEUTRAL));
    }

    #[test]
    fn vypadek_posledniho_ovladace_vede_do_disabled() {
        let mut e = hraji_dva();
        let _ = down(&mut e, KeyId::W);
        let _ = e.disable(P1, DisabledReason::PadNotConnected);
        let d = e.disable(P0, DisabledReason::PadError);
        assert_eq!(
            e.mode(),
            Mode::Disabled {
                reason: DisabledReason::PadError
            }
        );
        assert_eq!(
            d.ui,
            Some(UiEvent::ModeChanged {
                mode: Mode::Disabled {
                    reason: DisabledReason::PadError
                },
                cause: ModeCause::PadStatus
            })
        );
        assert!(vse_neutralni(&d));
        assert!(up(&mut e, KeyId::W).suppress);
    }

    #[test]
    fn vypnuti_ovladace_v_klavesnici_posle_jen_jeho_neutral() {
        let mut e = engine_with(dva_ovladace());
        let _ = e.enable(P1);
        let d = e.disable(P1, DisabledReason::PadNotConnected);
        assert_eq!(e.mode(), Mode::Keyboard);
        assert_eq!(d.ui, None);
        assert_eq!(
            d.pads.iter().collect::<Vec<_>>(),
            vec![(P1, PadState::NEUTRAL)]
        );
        // Podruhé už není co vypínat.
        assert_eq!(
            e.disable(P1, DisabledReason::PadNotConnected),
            Decision::NONE
        );
    }

    #[test]
    fn odchod_ze_hry_neutralizuje_vsechny_ovladace() {
        let mut e = hraji_dva();
        let _ = down(&mut e, KeyId::W);
        let _ = down(&mut e, KeyId::I);
        let d = down(&mut e, TOGGLE);
        assert_eq!(e.mode(), Mode::Keyboard);
        assert!(vse_neutralni(&d));
        for k in [KeyId::W, KeyId::I] {
            assert_eq!(e.held(k).unwrap().owner, Owner::Swallow);
            assert!(up(&mut e, k).suppress);
        }
    }

    #[test]
    fn prirazovani_ze_hry_vrati_zachytavani() {
        let mut e = gamepad();
        let _ = down(&mut e, KeyId::W);
        let d = e.start_binding(
            t0(Action::Button(PadButton::Y)),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        assert!(matches!(e.mode(), Mode::Binding { .. }));
        assert!(vse_neutralni(&d), "přiřazování pozastaví zachytávání");
        assert_eq!(e.held(KeyId::W).unwrap().owner, Owner::Swallow);

        let d = down(&mut e, KeyId::X);
        assert!(d.suppress);
        assert!(matches!(d.ui, Some(UiEvent::BindingSaved { .. })));
        assert_eq!(e.mode(), Mode::Gamepad, "zpět do hry");
        assert!(vse_neutralni(&d));
        // Přiřazená klávesa byla stisknutá při přiřazování: ovladač ji
        // neuvidí ani teď a její key-up se spolkne.
        let r = down(&mut e, KeyId::X);
        assert!(r.suppress && r.pads.is_empty());
        let u = up(&mut e, KeyId::X);
        assert!(u.suppress && u.pads.is_empty());
        assert!(up(&mut e, KeyId::W).suppress);
        // Nový stisk X už hraje.
        let d = down(&mut e, KeyId::X);
        assert!(pad0(&d).unwrap().is_pressed(PadButton::Y));
    }

    #[test]
    fn kazdy_bezny_konec_prirazovani_ze_hry_vrati_zachytavani() {
        let akce = t0(Action::LeftTrigger);
        let mut e = gamepad();
        let _ = e.start_binding(akce, BindKind::Replace, KeyConflict::Move, T0);
        let _ = down(&mut e, KeyId::ESC);
        assert_eq!(e.mode(), Mode::Gamepad, "Esc");
        assert!(up(&mut e, KeyId::ESC).suppress);

        let mut e = gamepad();
        let _ = e.start_binding(akce, BindKind::Replace, KeyConflict::Move, T0);
        let _ = e.tick(T0 + BINDING_TIMEOUT_MS);
        assert_eq!(e.mode(), Mode::Gamepad, "timeout");

        let mut e = gamepad();
        let _ = e.start_binding(akce, BindKind::Replace, KeyConflict::Move, T0);
        let d = e.cancel_binding(T0);
        assert_eq!(e.mode(), Mode::Gamepad, "zrušení z okna");
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Gui
            })
        );
    }

    #[test]
    fn vynuceny_konec_prirazovani_nevraci_zachytavani() {
        let akce = t0(Action::LeftTrigger);
        let mut e = gamepad();
        let _ = e.start_binding(akce, BindKind::Replace, KeyConflict::Move, T0);
        let d = e.force_keyboard(ForceReason::Watchdog);
        assert_eq!(e.mode(), Mode::Keyboard);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Forced(ForceReason::Watchdog)
            })
        );
        assert!(vse_neutralni(&d));

        let mut e = gamepad();
        let _ = e.start_binding(akce, BindKind::Replace, KeyConflict::Move, T0);
        let _ = e.set_mapping(Mapping::default());
        assert_eq!(e.mode(), Mode::Keyboard, "změna mapování");

        let mut e = gamepad();
        let _ = e.start_binding(akce, BindKind::Replace, KeyConflict::Move, T0);
        let _ = e.reset_held(ForceReason::SessionLock);
        assert_eq!(e.mode(), Mode::Keyboard, "zamčení relace");
        // Příznak návratu se zapomněl: další přiřazování z Klávesnice se
        // vrací na Klávesnici.
        let _ = e.start_binding(akce, BindKind::Replace, KeyConflict::Move, T0);
        let _ = e.cancel_binding(T0);
        assert_eq!(e.mode(), Mode::Keyboard);
    }

    #[test]
    fn prirazovani_bez_ovladace_se_vrati_do_disabled() {
        let mut e = Engine::new(Mapping::default());
        let _ = e.disable(P0, DisabledReason::ViGEmMissing);
        let cil = PadAction::new(P2, Action::RightTrigger);
        let d = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, T0);
        assert!(matches!(e.mode(), Mode::Binding { .. }));
        assert!(matches!(d.ui, Some(UiEvent::ModeChanged { .. })));
        let d = down(&mut e, KeyId::W);
        assert!(d.suppress);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingSaved {
                key: KeyId::W,
                target: cil.into(),
                moved_from: Some(t0(Action::LeftStick(StickDir::Up))),
                moved_more: 0,
                shared: 0
            })
        );
        assert_eq!(
            e.mode(),
            Mode::Disabled {
                reason: DisabledReason::ViGEmMissing
            }
        );
        assert!(up(&mut e, KeyId::W).suppress);
    }

    #[test]
    fn prirazovani_prezije_vypadek_i_pripojeni_ovladace() {
        // Ze hry: během přiřazování vypadne jediný ovladač → konec do
        // Disabled s jeho důvodem.
        let mut e = gamepad();
        let _ = e.start_binding(
            t0(Action::LeftTrigger),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        let d = e.disable(P0, DisabledReason::PadError);
        assert!(matches!(e.mode(), Mode::Binding { .. }), "běží dál");
        assert_eq!(d.ui, None);
        assert_eq!(pad0(&d), Some(PadState::NEUTRAL));
        let d = down(&mut e, KeyId::ESC);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Escape
            })
        );
        assert_eq!(
            e.mode(),
            Mode::Disabled {
                reason: DisabledReason::PadError
            }
        );

        // Z Disabled: během přiřazování se ovladač připojí → Klávesnice.
        let mut e = Engine::new(Mapping::default());
        let _ = e.start_binding(
            t0(Action::LeftTrigger),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        assert_eq!(e.enable(P0), Decision::NONE);
        assert!(matches!(e.mode(), Mode::Binding { .. }));
        let _ = e.cancel_binding(T0);
        assert_eq!(e.mode(), Mode::Keyboard);
    }

    #[test]
    fn capture_v_kazdem_rezimu() {
        // Klávesnice → Gamepad.
        let mut e = engine();
        let d = e.capture(T0);
        assert_eq!(
            d.ui,
            Some(UiEvent::ModeChanged {
                mode: Mode::Gamepad,
                cause: ModeCause::Gui
            })
        );
        assert!(vse_neutralni(&d));
        // Gamepad: nic (nikdy nepozastaví).
        assert_eq!(e.capture(T0), Decision::NONE);
        assert_eq!(e.mode(), Mode::Gamepad);
        // Disabled: odmítne.
        let mut e = Engine::new(Mapping::default());
        assert_eq!(
            e.capture(T0).ui,
            Some(UiEvent::ToggleRejected {
                reason: ToggleReject::Disabled(DisabledReason::PadNotConnected)
            })
        );
        assert!(matches!(e.mode(), Mode::Disabled { .. }));
        // Přiřazování z Klávesnice: zapne se až po jeho konci.
        let mut e = engine();
        let _ = e.start_binding(
            t0(Action::LeftTrigger),
            BindKind::Replace,
            KeyConflict::Move,
            T0,
        );
        assert_eq!(e.capture(T0), Decision::NONE);
        assert!(matches!(e.mode(), Mode::Binding { .. }));
        let d = down(&mut e, KeyId::X);
        assert!(matches!(d.ui, Some(UiEvent::BindingSaved { .. })));
        assert_eq!(e.mode(), Mode::Gamepad);
        assert!(up(&mut e, KeyId::X).suppress, "přiřazená klávesa spolknuta");
    }

    #[test]
    fn nove_prirazovani_behem_prirazovani_zacne_znovu() {
        let mut e = gamepad();
        let prvni = t0(Action::LeftTrigger);
        let druhe = PadAction::new(P1, Action::Button(PadButton::B));
        let _ = e.start_binding(prvni, BindKind::Replace, KeyConflict::Move, T0);
        let pozde = T0 + 5_000;
        let d = e.start_binding(druhe, BindKind::Replace, KeyConflict::Move, pozde);
        assert_eq!(
            d.ui,
            Some(UiEvent::ModeChanged {
                mode: Mode::Binding {
                    target: druhe.into(),
                    started_at_ms: pozde
                },
                cause: ModeCause::Gui
            })
        );
        // Timeout se počítá od nového začátku.
        assert_eq!(e.tick(T0 + BINDING_TIMEOUT_MS), Decision::NONE);
        let d = down(&mut e, KeyId::NUMPAD_8);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingSaved {
                key: KeyId::NUMPAD_8,
                target: druhe.into(),
                moved_from: None,
                moved_more: 0,
                shared: 0
            })
        );
        assert_eq!(e.mapping().targets(KeyId::NUMPAD_8).first(), Some(druhe));
        assert_eq!(e.mapping().keys_for(prvni).count(), 1, "první beze změny");
        assert_eq!(e.mode(), Mode::Gamepad, "příznak návratu přežil restart");
    }

    // ── Editor (Fáze 6) ──────────────────────────────────────────────

    const A_BTN: Action = Action::Button(PadButton::A);
    const UP: Action = Action::LeftStick(StickDir::Up);

    /// Výchozí mapování + X jako druhá klávesa tlačítka A.
    fn a_se_dvema() -> Mapping {
        let mut m = Mapping::default();
        m.bind(KeyId::X, t0(A_BTN), KeyConflict::Move).unwrap();
        m
    }

    #[test]
    fn duvody_vynuceni_cisluje_all() {
        for (i, r) in ForceReason::ALL.into_iter().enumerate() {
            assert_eq!(r.index(), i, "{r:?}");
        }
        let mut s = ForceReason::ALL.to_vec();
        s.dedup();
        assert_eq!(s.len(), 9);
    }

    #[test]
    fn prirazeni_nahradi_ostatni_klavesy() {
        let mut e = engine_with(a_se_dvema());
        assert_eq!(e.binding_kind(), None);
        let _ = e.start_binding(t0(A_BTN), BindKind::Replace, KeyConflict::Move, T0);
        assert_eq!(e.binding_kind(), Some(BindKind::Replace));
        let d = down(&mut e, KeyId::NUMPAD_8);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingSaved {
                key: KeyId::NUMPAD_8,
                target: t0(A_BTN).into(),
                moved_from: None,
                moved_more: 0,
                shared: 0
            })
        );
        assert_eq!(
            e.mapping().keys_for(t0(A_BTN)).collect::<Vec<_>>(),
            vec![KeyId::NUMPAD_8],
            "mezerník a X pryč"
        );
        assert_eq!(e.binding_kind(), None, "přiřazování skončilo");
    }

    #[test]
    fn pridani_necha_ostatni_klavesy() {
        let mut e = engine_with(a_se_dvema());
        let _ = e.start_binding(t0(A_BTN), BindKind::Add, KeyConflict::Move, T0);
        assert_eq!(e.binding_kind(), Some(BindKind::Add));
        let d = down(&mut e, KeyId::NUMPAD_8);
        assert!(matches!(d.ui, Some(UiEvent::BindingSaved { .. })));
        assert_eq!(
            e.mapping().keys_for(t0(A_BTN)).collect::<Vec<_>>(),
            vec![KeyId::X, KeyId::SPACE, KeyId::NUMPAD_8]
        );
    }

    #[test]
    fn druh_plati_pro_posledni_start() {
        // Restart přiřazování s jiným druhem: platí nový.
        let mut e = engine_with(a_se_dvema());
        let _ = e.start_binding(t0(A_BTN), BindKind::Add, KeyConflict::Move, T0);
        let _ = e.start_binding(t0(A_BTN), BindKind::Replace, KeyConflict::Move, T0);
        let _ = down(&mut e, KeyId::NUMPAD_8);
        assert_eq!(e.mapping().keys_for(t0(A_BTN)).count(), 1);
    }

    #[test]
    fn nahrazeni_presune_klavesu_z_jineho_ovladace() {
        let mut e = engine_with(dva_ovladace());
        let _ = e.enable(P1);
        let cil = t0(A_BTN);
        let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, T0);
        let d = down(&mut e, KeyId::X);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingSaved {
                key: KeyId::X,
                target: cil.into(),
                moved_from: Some(PadAction::new(P1, A_BTN)),
                moved_more: 0,
                shared: 0
            })
        );
        assert_eq!(
            e.mapping().keys_for(cil).collect::<Vec<_>>(),
            vec![KeyId::X]
        );
        assert_eq!(e.mapping().keys_for(PadAction::new(P1, A_BTN)).count(), 0);
    }

    #[test]
    fn nahrazeni_se_zkratkou_ani_win_nic_nezmeni() {
        for kind in [BindKind::Replace, BindKind::Add] {
            let mut e = engine_with(a_se_dvema());
            let pred = e.mapping().clone();
            let _ = e.start_binding(t0(A_BTN), kind, KeyConflict::Move, T0);
            let d = down(&mut e, TOGGLE);
            assert_eq!(
                d.ui,
                Some(UiEvent::BindingRejected {
                    key: TOGGLE,
                    reason: BindingReject::ToggleKey
                })
            );
            let d = down(&mut e, KeyId::LEFT_WIN);
            assert_eq!(
                d.ui,
                Some(UiEvent::BindingRejected {
                    key: KeyId::LEFT_WIN,
                    reason: BindingReject::Reserved
                })
            );
            assert_eq!(e.mapping(), &pred, "{kind:?}");
            assert_eq!(e.mapping_rev(), 0);
            assert_eq!(e.binding_kind(), Some(kind), "čeká se dál");
        }
    }

    #[test]
    fn vymena_mapovani_nemeni_rezim_ani_vlastniky() {
        let mut e = gamepad();
        let d = down(&mut e, KeyId::W);
        let stav = pad0(&d).unwrap();
        let mut m = e.mapping().clone();
        m.unbind(KeyId::W).unwrap();
        let d = e.replace_mapping(m, T0);
        assert_eq!(d, Decision::NONE, "nic se nevynucuje ani neposílá");
        assert_eq!(e.mode(), Mode::Gamepad);
        assert_eq!(e.pad_state(P0), stav, "W dohrává se starou akcí");
        assert_eq!(e.held(KeyId::W).unwrap().owner, Owner::Pad(t0(UP).into()));
        assert_eq!(e.mapping().targets(KeyId::W).first(), None);
        // Autorepeat dál k ovladači, nic nespouští.
        let r = down(&mut e, KeyId::W);
        assert!(r.suppress && r.pads.is_empty() && r.ui.is_none());
        // Key-up se spolkne (OS key-down neviděl) a pad je neutrální.
        let u = up(&mut e, KeyId::W);
        assert!(u.suppress);
        assert_eq!(pad0(&u), Some(PadState::NEUTRAL));
        // Další W už jde podle nového mapování do OS.
        let n = down(&mut e, KeyId::W);
        assert!(!n.suppress && n.pads.is_empty());
        assert!(!up(&mut e, KeyId::W).suppress);
    }

    #[test]
    fn vymena_mapovani_v_ostatnich_rezimech_nic_nevynucuje() {
        let m = a_se_dvema();
        let mut e = engine();
        assert_eq!(e.replace_mapping(m.clone(), T0), Decision::NONE);
        assert_eq!(e.mode(), Mode::Keyboard);
        let mut e = Engine::new(Mapping::default());
        assert_eq!(e.replace_mapping(m.clone(), T0), Decision::NONE);
        assert!(matches!(e.mode(), Mode::Disabled { .. }));
        assert_eq!(e.mapping(), &m);
    }

    #[test]
    fn vymena_mapovani_zrusi_prirazovani_a_vrati_hru() {
        let mut e = gamepad();
        let _ = e.start_binding(t0(A_BTN), BindKind::Replace, KeyConflict::Move, T0);
        let d = e.replace_mapping(a_se_dvema(), T0);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Gui
            })
        );
        assert_eq!(
            e.mode(),
            Mode::Gamepad,
            "zpět do hry, ne vynucená Klávesnice"
        );
        assert!(vse_neutralni(&d));
        assert_eq!(e.mapping(), &a_se_dvema());
        // Propadlé přiřazování skončí časem, ne zrušením z okna.
        let mut e = engine();
        let _ = e.start_binding(t0(A_BTN), BindKind::Replace, KeyConflict::Move, T0);
        let d = e.replace_mapping(a_se_dvema(), T0 + BINDING_TIMEOUT_MS);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Timeout
            })
        );
        assert_eq!(e.mode(), Mode::Keyboard);
    }

    #[test]
    fn vymena_mapovani_zmeni_i_zkratku() {
        // Nová zkratka platí od dalšího stisku; stará je obyčejná klávesa.
        let mut e = engine();
        let mut m = Mapping::default();
        m.set_toggle_key(KeyId::NUMPAD_8).unwrap();
        let _ = e.replace_mapping(m, T0);
        assert!(!down(&mut e, TOGGLE).suppress);
        assert!(!up(&mut e, TOGGLE).suppress);
        let d = down(&mut e, KeyId::NUMPAD_8);
        assert!(d.suppress);
        assert_eq!(e.mode(), Mode::Gamepad);
    }

    #[test]
    fn revize_roste_jen_se_skutecnou_zmenou() {
        let mut e = engine();
        assert_eq!(e.mapping_rev(), 0);
        // Klávesy mimo přiřazování revizi nemění, ani přepnutí.
        for k in [KeyId::W, TOGGLE, KeyId::X] {
            let _ = down(&mut e, k);
            let _ = up(&mut e, k);
        }
        let _ = e.toggle(T0);
        assert_eq!(e.mapping_rev(), 0);

        // Vazba na tentýž cíl: jediná klávesa (nahradit) i znovu přidaná.
        let _ = e.start_binding(t0(UP), BindKind::Replace, KeyConflict::Move, T0);
        let d = down(&mut e, KeyId::W);
        assert!(matches!(d.ui, Some(UiEvent::BindingSaved { .. })));
        let _ = up(&mut e, KeyId::W);
        let _ = e.start_binding(t0(UP), BindKind::Add, KeyConflict::Move, T0);
        let _ = down(&mut e, KeyId::W);
        let _ = up(&mut e, KeyId::W);
        assert_eq!(e.mapping_rev(), 0);

        // Neúspěch ani zrušení nic nemění.
        let _ = e.start_binding(t0(UP), BindKind::Replace, KeyConflict::Move, T0);
        let _ = down(&mut e, TOGGLE);
        let _ = down(&mut e, KeyId::new(0));
        let _ = down(&mut e, KeyId::ESC);
        let _ = e.start_binding(t0(UP), BindKind::Replace, KeyConflict::Move, T0);
        let _ = e.cancel_binding(T0);
        let _ = e.start_binding(t0(UP), BindKind::Replace, KeyConflict::Move, T0);
        let _ = e.tick(T0 + BINDING_TIMEOUT_MS);
        assert_eq!(e.mapping_rev(), 0);

        // Skutečná změna: přidaná klávesa, pak nahrazení jen odebírá.
        let _ = e.start_binding(t0(UP), BindKind::Add, KeyConflict::Move, T0);
        let _ = e.on_key(KeyId::NUMPAD_8, true, T0 + 1);
        assert_eq!(e.mapping_rev(), 1);
        let _ = e.on_key(KeyId::NUMPAD_8, false, T0 + 1);
        let _ = e.start_binding(t0(UP), BindKind::Replace, KeyConflict::Move, T0 + 2);
        let _ = e.on_key(KeyId::W, true, T0 + 3);
        assert_eq!(e.mapping_rev(), 2, "W zůstalo, ubylo NUMPAD_8");

        // Výměna mapování: stejný obsah nic, clear_pad bez vazeb nic.
        let _ = e.replace_mapping(e.mapping().clone(), T0);
        let mut m = e.mapping().clone();
        assert_eq!(m.clear_pad(P2), Ok(0));
        let _ = e.replace_mapping(m, T0);
        assert_eq!(e.mapping_rev(), 2);
        let _ = e.set_mapping(e.mapping().clone());
        assert_eq!(e.mapping_rev(), 2);

        // clear_pad s vazbami, set_mapping s jiným obsahem: roste.
        let mut m = dva_ovladace();
        let _ = e.replace_mapping(m.clone(), T0);
        assert_eq!(e.mapping_rev(), 3);
        assert_eq!(m.clear_pad(P1), Ok(5));
        let _ = e.replace_mapping(m, T0);
        assert_eq!(e.mapping_rev(), 4);
        let _ = e.set_mapping(Mapping::default());
        assert_eq!(e.mapping_rev(), 5);
    }

    /// Živý stav ovladače `p` jako (držené akce, levá páčka).
    fn zive(e: &Engine, include_os: bool, p: PadId) -> (Vec<Action>, (i8, i8)) {
        let z = e.live_inputs(include_os)[p.index()];
        (z.held().iter().collect(), z.left_stick())
    }

    #[test]
    fn zivy_stav_bez_ovladace_jen_s_klavesami_os() {
        let mut e = Engine::new(Mapping::default());
        let _ = down(&mut e, KeyId::W);
        assert_eq!(zive(&e, true, P0), (vec![UP], (0, 1)));
        assert_eq!(e.live_inputs(false), [LiveInputs::EMPTY; MAX_PADS]);
        // Nenamapovaná klávesa nerozsvítí nic.
        let _ = up(&mut e, KeyId::W);
        let _ = down(&mut e, KeyId::NUMPAD_8);
        assert_eq!(e.live_inputs(true), [LiveInputs::EMPTY; MAX_PADS]);
    }

    #[test]
    fn zivy_stav_ve_hre_a_po_zkratce() {
        let mut e = gamepad();
        let _ = down(&mut e, KeyId::W);
        assert_eq!(e.held(KeyId::W).unwrap().owner, Owner::Pad(t0(UP).into()));
        assert_eq!(zive(&e, false, P0), (vec![UP], (0, 1)), "Pad svítí vždy");
        // Pauza: W je Swallow a pořád svítí (klávesa je dole).
        let _ = down(&mut e, TOGGLE);
        assert_eq!(e.held(KeyId::W).unwrap().owner, Owner::Swallow);
        assert_eq!(zive(&e, false, P0), (vec![UP], (0, 1)));
        // reset_held zapomene všechno.
        let _ = e.reset_held(ForceReason::SessionLock);
        assert_eq!(e.live_inputs(true), [LiveInputs::EMPTY; MAX_PADS]);
    }

    #[test]
    fn zivy_stav_a_pak_d_sviti_obe() {
        let mut e = gamepad();
        let _ = down(&mut e, KeyId::A);
        let _ = down(&mut e, KeyId::D);
        let left = Action::LeftStick(StickDir::Left);
        let right = Action::LeftStick(StickDir::Right);
        assert_eq!(zive(&e, false, P0), (vec![left, right], (1, 0)));
        // Hra dostává jen vítěze SOCD.
        assert_eq!(
            e.pad_state(P0).active_inputs().iter().collect::<Vec<_>>(),
            vec![right]
        );
        let _ = up(&mut e, KeyId::D);
        assert_eq!(zive(&e, false, P0), (vec![left], (-1, 0)));
    }

    #[test]
    fn zivy_stav_klavesy_druheho_ovladace() {
        let mut e = engine_with(dva_ovladace());
        // Bez zachytávání: klávesa OS svítí u svého ovladače (s popředím).
        let _ = down(&mut e, KeyId::I);
        assert_eq!(zive(&e, true, P1), (vec![UP], (0, 1)));
        assert_eq!(e.live_inputs(true)[P0.index()], LiveInputs::EMPTY);
        let _ = up(&mut e, KeyId::I);
        // Ve hře s připraveným druhým ovladačem i bez popředí.
        let _ = e.enable(P1);
        let _ = e.capture(T0);
        let _ = down(&mut e, KeyId::X);
        let z = e.live_inputs(false);
        assert_eq!(z[P1.index()].held().iter().collect::<Vec<_>>(), vec![A_BTN]);
        for p in [P0, P2, PadId::ALL[3]] {
            assert_eq!(z[p.index()], LiveInputs::EMPTY, "{p:?}");
        }
    }

    #[test]
    fn zivy_stav_prirazene_klavesy_sviti_na_novem_miste() {
        let mut e = engine();
        let cil = t0(Action::RightTrigger);
        let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, T0);
        let _ = down(&mut e, KeyId::W);
        assert_eq!(e.held(KeyId::W).unwrap().owner, Owner::Swallow);
        let z = e.live_inputs(false)[0];
        assert_eq!(
            z.held().iter().collect::<Vec<_>>(),
            vec![Action::RightTrigger]
        );
        assert_eq!(z.left_stick(), (0, 0));
    }

    /// Klávesy pro kódování: mapovatelné, nemapovatelné, Win, krajní scan.
    const KODOVANE_KLAVESY: [KeyId; 8] = [
        KeyId::W,
        KeyId::ARROW_UP,
        KeyId::LEFT_WIN,
        KeyId::ALTGR_FAKE_CTRL,
        KeyId::new(0),
        KeyId::ext(0),
        KeyId::new(0xFFFF),
        KeyId::ext(0xFFFF),
    ];

    /// Všechny události kromě `ModeChanged` (cíle a klávesy výběrem).
    fn vsechny_udalosti() -> Vec<UiEvent> {
        let cile: Vec<PadAction> = PadId::ALL
            .into_iter()
            .flat_map(|p| Action::ALL.map(|a| PadAction::new(p, a)))
            .collect();
        let mut v = Vec::new();
        for key in KODOVANE_KLAVESY {
            for &target in &cile {
                v.push(UiEvent::BindingSaved {
                    key,
                    target: target.into(),
                    moved_from: None,
                    moved_more: 0,
                    shared: 0,
                });
            }
            for &moved in &cile {
                v.push(UiEvent::BindingSaved {
                    key,
                    target: cile[(moved.action.index() * 7) % cile.len()].into(),
                    moved_from: Some(moved),
                    moved_more: 0,
                    shared: 0,
                });
            }
            // Sdílené klávesy (Fáze 7): kolik dalších přišlo o klávesu
            // (jen s `moved_from`) a kolika dalším klávesa patří.
            for (i, &target) in cile.iter().enumerate().take(16) {
                let n = (i % 4) as u8;
                v.push(UiEvent::BindingSaved {
                    key,
                    target: target.into(),
                    moved_from: Some(cile[95 - i]),
                    moved_more: n,
                    shared: 3 - n,
                });
                v.push(UiEvent::BindingSaved {
                    key,
                    target: target.into(),
                    moved_from: None,
                    moved_more: 0,
                    shared: 1 + (i % 3) as u8,
                });
            }
            // Zkratka pozastavení (Fáze 7, Z6): cíl 127, nikomu nic nebere.
            v.push(UiEvent::BindingSaved {
                key,
                target: BindTarget::Toggle,
                moved_from: None,
                moved_more: 0,
                shared: 0,
            });
            for reason in [
                BindingReject::ToggleKey,
                BindingReject::Unmappable,
                BindingReject::Reserved,
                BindingReject::TooManyTargets,
                BindingReject::Mapped,
                BindingReject::NotToggleKey,
            ] {
                v.push(UiEvent::BindingRejected { key, reason });
            }
        }
        for reason in [
            BindingCancel::Escape,
            BindingCancel::Timeout,
            BindingCancel::Gui,
            BindingCancel::PadStatus,
        ]
        .into_iter()
        .chain(ForceReason::ALL.map(BindingCancel::Forced))
        {
            v.push(UiEvent::BindingCancelled { reason });
        }
        for reason in [
            ToggleReject::Disabled(DisabledReason::ViGEmMissing),
            ToggleReject::Disabled(DisabledReason::PadNotConnected),
            ToggleReject::Disabled(DisabledReason::PadError),
            ToggleReject::Binding,
        ] {
            v.push(UiEvent::ToggleRejected { reason });
        }
        v
    }

    #[test]
    fn oznameni_tam_a_zpet() {
        let v = vsechny_udalosti();
        let mut bity = std::collections::HashSet::new();
        for e in &v {
            let b = e.pack().unwrap_or_else(|| panic!("{e:?} nejde zabalit"));
            assert!(b < 1 << 48, "{e:?}: {b:#x} přes 48 bitů");
            assert_ne!(b, 0, "{e:?}: nula je „nic\"");
            assert_eq!(UiEvent::unpack(b), Some(*e), "{b:#x}");
            assert!(bity.insert(b), "{e:?} sdílí bity s jinou událostí");
        }
        // Všechny moved_from i cíle (4 × 24), počty sdílení a všechny
        // důvody jsou vidět.
        assert_eq!(v.len(), 8 * (96 + 96 + 32 + 1 + 6) + 13 + 4);
        // Počty, které engine nevydá, se nezabalí (žádné smyšlené bity).
        let w = |moved_from, moved_more, shared| UiEvent::BindingSaved {
            key: KeyId::W,
            target: t0(A_BTN).into(),
            moved_from,
            moved_more,
            shared,
        };
        assert_eq!(w(None, 1, 0).pack(), None, "další bez prvního");
        assert_eq!(w(Some(t0(UP)), 4, 0).pack(), None);
        assert_eq!(w(None, 0, 4).pack(), None);
        assert!(w(Some(t0(UP)), 3, 3).pack().is_some());
        // Zkratka nikomu klávesu nebere ani ji nesdílí.
        let z = |moved_from, moved_more, shared| UiEvent::BindingSaved {
            key: KeyId::F5,
            target: BindTarget::Toggle,
            moved_from,
            moved_more,
            shared,
        };
        assert!(z(None, 0, 0).pack().is_some());
        assert_eq!(z(Some(t0(UP)), 0, 0).pack(), None);
        assert_eq!(z(None, 0, 1).pack(), None);
    }

    #[test]
    fn zmena_rezimu_se_nebali() {
        for (mode, cause) in [
            (Mode::Gamepad, ModeCause::Hotkey),
            (
                Mode::Binding {
                    target: t0(A_BTN).into(),
                    started_at_ms: u64::MAX,
                },
                ModeCause::Gui,
            ),
            (
                Mode::Disabled {
                    reason: DisabledReason::PadError,
                },
                ModeCause::Forced(ForceReason::Shutdown),
            ),
        ] {
            assert_eq!(UiEvent::ModeChanged { mode, cause }.pack(), None);
        }
    }

    #[test]
    fn rozbalit_odmitne_cokoli_mimo_pack() {
        let zrus = UiEvent::BindingCancelled {
            reason: BindingCancel::Gui,
        }
        .pack()
        .unwrap();
        let ulozeno = UiEvent::BindingSaved {
            key: KeyId::W,
            target: t0(A_BTN).into(),
            moved_from: None,
            moved_more: 0,
            shared: 0,
        }
        .pack()
        .unwrap();
        for b in [
            0,                            // druh 0
            5,                            // neznámý druh
            7,                            // neznámý druh
            zrus | 1 << 3,                // klávesa u zrušení
            zrus | 1 << 20,               // cíl u zrušení
            zrus | 1 << 47,               // bit nad rozložením
            zrus | 1 << 63,               // horní bity (seq hook vlákna)
            3 | 13 << 35,                 // důvod zrušení mimo rozsah
            2 | 6 << 35,                  // důvod odmítnutí mimo rozsah
            ulozeno | 1 << 40,            // další přišli o klávesu bez odkud
            ulozeno | 1 << 44,            // bit nad počtem sdílení
            4 | 4 << 35,                  // důvod přepnutí mimo rozsah
            ulozeno | 1 << 28,            // odkud bez příznaku
            1 | 24 << 20,                 // akce 24 neexistuje
            1 | 126 << 20,                // ovladač 3, akce 30
            1 | 120 << 20,                // ovladač 3, akce 24
            1 | 127 << 20 | 1 << 27,      // zkratka „odkud"
            1 | 127 << 20 | 1 << 42,      // zkratka sdílená
            ulozeno | 1 << 27 | 30 << 28, // odkud: akce 30
            ulozeno | 2 << 35,            // důvod u uložení
        ] {
            assert_eq!(UiEvent::unpack(b), None, "{b:#x}");
        }
    }

    #[test]
    fn prirazeni_presune_klavesu_na_jiny_ovladac() {
        let mut e = hraji_dva();
        let cil = PadAction::new(P1, Action::Button(PadButton::Y));
        let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, T0);
        let d = down(&mut e, KeyId::SPACE);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingSaved {
                key: KeyId::SPACE,
                target: cil.into(),
                moved_from: Some(t0(Action::Button(PadButton::A))),
                moved_more: 0,
                shared: 0
            })
        );
        assert!(up(&mut e, KeyId::SPACE).suppress);
        // Mezerník teď hraje za druhý ovladač.
        let d = down(&mut e, KeyId::SPACE);
        assert_eq!(pad0(&d), None);
        assert!(d.pads.get(P1).unwrap().is_pressed(PadButton::Y));
        assert!(e.pad_state(P0).is_neutral());
    }

    // ── Sdílené klávesy (Fáze 7) ─────────────────────────────────────

    const P3: PadId = PadId::ALL[3];
    const X_BTN: Action = Action::Button(PadButton::X);

    fn na(p: PadId, a: Action) -> PadAction {
        PadAction::new(p, a)
    }

    fn cile(t: &[PadAction]) -> Targets {
        Targets::from_targets(t.iter().copied()).unwrap()
    }

    /// [`dva_ovladace`] + W i jako levá páčka nahoru druhého ovladače
    /// a F (tlačítko X prvního) i jako tlačítko A druhého.
    fn sdilene() -> Mapping {
        let mut m = dva_ovladace();
        m.bind(KeyId::W, na(P1, UP), KeyConflict::Share).unwrap();
        m.bind(KeyId::F, na(P1, A_BTN), KeyConflict::Share).unwrap();
        m
    }

    fn hraji_sdilene() -> Engine {
        let mut e = engine_with(sdilene());
        let _ = e.enable(P1);
        let _ = e.capture(T0);
        assert_eq!(e.mode(), Mode::Gamepad);
        e
    }

    /// Stisk sdílené klávesy spustí všechny její vstupy na připravených
    /// ovladačích; autorepeat nic, key-up pustí všechny.
    #[test]
    fn sdilena_klavesa_hraje_za_vsechny_pripravene() {
        let mut e = hraji_sdilene();
        let d = down(&mut e, KeyId::W);
        assert!(d.suppress);
        assert_eq!(
            e.held(KeyId::W).unwrap().owner,
            Owner::Pad(cile(&[t0(UP), na(P1, UP)]))
        );
        assert_eq!(stick(pad0(&d).unwrap()), (0, AXIS_MAX));
        assert_eq!(stick(d.pads.get(P1).unwrap()), (0, AXIS_MAX));
        let r = down(&mut e, KeyId::W);
        assert!(r.suppress && r.pads.is_empty() && r.ui.is_none());
        let u = up(&mut e, KeyId::W);
        assert!(u.suppress);
        assert_eq!(pad0(&u), Some(PadState::NEUTRAL));
        assert_eq!(u.pads.get(P1), Some(PadState::NEUTRAL));
        // F: X na prvním a A na druhém zároveň.
        let d = down(&mut e, KeyId::F);
        assert!(pad0(&d).unwrap().is_pressed(PadButton::X));
        assert!(d.pads.get(P1).unwrap().is_pressed(PadButton::A));
        // A druhého drží i X: puštění F pustí X prvního, A druhého ne
        // (tlačítko drží kterákoli jeho klávesa).
        let _ = down(&mut e, KeyId::X);
        let u = up(&mut e, KeyId::F);
        assert!(!pad0(&u).unwrap().is_pressed(PadButton::X));
        assert_eq!(u.pads.get(P1), None, "A druhého drží X dál");
        assert!(e.pad_state(P1).is_pressed(PadButton::A));
        let u = up(&mut e, KeyId::X);
        assert_eq!(u.pads.get(P1), Some(PadState::NEUTRAL));
    }

    /// Množina se určí při stisku jen z připravených ovladačů a pak se
    /// nemění (princip 2): nově připojený ovladač dostane klávesu až
    /// dalším stiskem. Klávesa jen nepřipravených ovladačů jde do OS.
    #[test]
    fn sdilena_klavesa_jen_pro_pripravene_ovladace() {
        let mut e = engine_with(sdilene());
        let _ = e.capture(T0);
        let d = down(&mut e, KeyId::W);
        assert_eq!(
            e.held(KeyId::W).unwrap().owner,
            Owner::Pad(Targets::one(t0(UP)))
        );
        assert_eq!(d.pads.get(P1), None);
        assert!(!down(&mut e, KeyId::I).suppress, "I jen druhého → OS");
        assert!(!up(&mut e, KeyId::I).suppress);
        assert_eq!(e.enable(P1), Decision::NONE);
        assert_eq!(
            e.held(KeyId::W).unwrap().owner,
            Owner::Pad(Targets::one(t0(UP))),
            "držená W se nemění"
        );
        assert!(e.pad_state(P1).is_neutral());
        let u = up(&mut e, KeyId::W);
        assert_eq!(u.pads.get(P1), None);
        let d = down(&mut e, KeyId::W);
        assert_eq!(
            stick(d.pads.get(P1).unwrap()),
            (0, AXIS_MAX),
            "nový stisk za oba"
        );
    }

    /// Výpadek jednoho ovladače sdílené klávesy: jeho cíle z ní zmizí
    /// a dostane neutrál, ostatní hrají dál. Bez cíle se klávesa spolkne.
    #[test]
    fn vypadek_ovladace_sdilene_klavesy() {
        let mut e = hraji_sdilene();
        let _ = down(&mut e, KeyId::W);
        let d = e.disable(P1, DisabledReason::PadError);
        assert_eq!(e.mode(), Mode::Gamepad);
        assert_eq!(
            d.pads.iter().collect::<Vec<_>>(),
            vec![(P1, PadState::NEUTRAL)]
        );
        assert_eq!(
            e.held(KeyId::W).unwrap().owner,
            Owner::Pad(Targets::one(t0(UP))),
            "první hraje dál"
        );
        assert_eq!(stick(e.pad_state(P0)), (0, AXIS_MAX));
        let _ = e.disable(P0, DisabledReason::PadError);
        assert_eq!(e.held(KeyId::W).unwrap().owner, Owner::Swallow);
        let u = up(&mut e, KeyId::W);
        assert!(u.suppress && u.pads.is_empty());
    }

    /// Pauza: sdílená klávesa se spolkne a svítí u všech svých vstupů.
    #[test]
    fn odchod_ze_hry_spolkne_sdilenou_klavesu() {
        let mut e = hraji_sdilene();
        let _ = down(&mut e, KeyId::W);
        let d = down(&mut e, TOGGLE);
        assert!(vse_neutralni(&d));
        assert_eq!(e.held(KeyId::W).unwrap().owner, Owner::Swallow);
        assert_eq!(zive(&e, false, P0), (vec![UP], (0, 1)));
        assert_eq!(zive(&e, false, P1), (vec![UP], (0, 1)));
        assert!(up(&mut e, KeyId::W).suppress);
    }

    /// Změna mapování za hry: držená sdílená klávesa dohraje s množinou
    /// z key-downu a její key-up jde téže množině; nový stisk podle
    /// nového mapování (OQ 43).
    #[test]
    fn zmena_mapovani_za_hry_sdilena_klavesa_dohraje() {
        let mut e = hraji_sdilene();
        let _ = down(&mut e, KeyId::W);
        assert_eq!(e.replace_mapping(dva_ovladace(), T0), Decision::NONE);
        assert_eq!(
            e.held(KeyId::W).unwrap().owner,
            Owner::Pad(cile(&[t0(UP), na(P1, UP)]))
        );
        assert_eq!(stick(e.pad_state(P1)), (0, AXIS_MAX));
        let u = up(&mut e, KeyId::W);
        assert!(u.suppress);
        assert_eq!(u.pads.get(P1), Some(PadState::NEUTRAL));
        let d = down(&mut e, KeyId::W);
        assert_eq!(d.pads.get(P1), None, "nový stisk jen za první");
        assert_eq!(stick(pad0(&d).unwrap()), (0, AXIS_MAX));
    }

    /// Jedna klávesa na protichůdné směry téže páčky: oba směry mají
    /// totéž pořadí stisku → shoda → neutrál, dokud nepřijde novější
    /// stisk jiného směru (OQ 63).
    #[test]
    fn protichudne_smery_jednou_klavesou() {
        let left = Action::LeftStick(StickDir::Left);
        let right = Action::LeftStick(StickDir::Right);
        let m = Mapping::new(
            TOGGLE,
            [
                (KeyId::X, t0(left)),
                (KeyId::X, t0(right)),
                (KeyId::D, t0(right)),
                (KeyId::A, t0(left)),
            ],
        )
        .unwrap();
        let mut e = engine_with(m);
        let _ = e.capture(T0);
        let d = down(&mut e, KeyId::X);
        assert!(d.suppress);
        assert!(e.pad_state(P0).is_neutral());
        assert_eq!(zive(&e, false, P0), (vec![left, right], (0, 0)));
        let d = down(&mut e, KeyId::D);
        assert_eq!(stick(pad0(&d).unwrap()), (AXIS_MAX, 0), "novější vyhraje");
        let d = up(&mut e, KeyId::D);
        assert_eq!(stick(pad0(&d).unwrap()), (0, 0), "zase shoda");
        let _ = down(&mut e, KeyId::A);
        assert_eq!(stick(e.pad_state(P0)), (-AXIS_MAX, 0));
        let _ = up(&mut e, KeyId::A);
        let _ = up(&mut e, KeyId::X);
        assert!(e.pad_state(P0).is_neutral());
        assert_eq!(e.held_len(), 0);
    }

    /// Živý stav: klávesa OS (okno v popředí) i ve hře svítí u všech
    /// svých vstupů.
    #[test]
    fn zivy_stav_sdilene_klavesy() {
        let mut e = engine_with(sdilene());
        let _ = down(&mut e, KeyId::W);
        assert_eq!(zive(&e, true, P0), (vec![UP], (0, 1)));
        assert_eq!(zive(&e, true, P1), (vec![UP], (0, 1)));
        assert_eq!(e.live_inputs(false), [LiveInputs::EMPTY; MAX_PADS]);
        let _ = up(&mut e, KeyId::W);
        let _ = e.enable(P1);
        let _ = e.capture(T0);
        let _ = down(&mut e, KeyId::F);
        let z = e.live_inputs(false);
        assert_eq!(z[0].held().iter().collect::<Vec<_>>(), vec![X_BTN]);
        assert_eq!(z[1].held().iter().collect::<Vec<_>>(), vec![A_BTN]);
    }

    /// Přiřazení se sdílením: klávesa zůstane i dosavadnímu vstupu,
    /// nikdo nic neztratí, revize roste a hraje se dál.
    #[test]
    fn prirazeni_se_sdilenim() {
        let mut e = hraji_dva();
        let cil = na(P1, Action::Button(PadButton::Y));
        let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Share, T0);
        assert_eq!(e.binding_conflict(), Some(KeyConflict::Share));
        let d = down(&mut e, KeyId::SPACE);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingSaved {
                key: KeyId::SPACE,
                target: cil.into(),
                moved_from: None,
                moved_more: 0,
                shared: 1,
            })
        );
        assert_eq!(e.mapping_rev(), 1);
        assert_eq!(e.mapping().targets(KeyId::SPACE), cile(&[t0(A_BTN), cil]));
        assert_eq!(e.mode(), Mode::Gamepad);
        assert_eq!(e.binding_conflict(), None);
        assert!(up(&mut e, KeyId::SPACE).suppress);
        let d = down(&mut e, KeyId::SPACE);
        assert!(pad0(&d).unwrap().is_pressed(PadButton::A));
        assert!(d.pads.get(P1).unwrap().is_pressed(PadButton::Y));
    }

    /// Pátý vstup se odmítne a přiřazování čeká dál; bez volby se tatáž
    /// klávesa přesune ze všech čtyř (první + 3 další).
    #[test]
    fn paty_vstup_se_odmitne_a_prirazovani_ceka() {
        let m = Mapping::new(
            TOGGLE,
            [
                (KeyId::W, t0(UP)),
                (KeyId::W, na(P1, UP)),
                (KeyId::W, na(P2, UP)),
                (KeyId::W, na(P3, UP)),
                (KeyId::F, t0(X_BTN)),
            ],
        )
        .unwrap();
        let mut e = engine_with(m.clone());
        let cil = t0(A_BTN);
        let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Share, T0);
        let d = down(&mut e, KeyId::W);
        assert!(d.suppress);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingRejected {
                key: KeyId::W,
                reason: BindingReject::TooManyTargets
            })
        );
        assert!(matches!(e.mode(), Mode::Binding { .. }), "čeká dál");
        assert_eq!((e.mapping(), e.mapping_rev()), (&m, 0));
        assert!(up(&mut e, KeyId::W).suppress);
        let _ = e.start_binding(cil, BindKind::Replace, KeyConflict::Move, T0);
        let d = down(&mut e, KeyId::W);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingSaved {
                key: KeyId::W,
                target: cil.into(),
                moved_from: Some(t0(UP)),
                moved_more: 3,
                shared: 0,
            })
        );
        assert_eq!(e.mapping().targets(KeyId::W), Targets::one(cil));
        assert_eq!(e.mapping_rev(), 1);
    }

    /// Klávesa, která cíli už patří, se nemění s volbou ani bez ní —
    /// sdílená klávesa se nevezme vstupům, na které nikdo neklikl.
    /// Oznámení řekne, kolika dalším klávesa dál patří.
    #[test]
    fn klavesa_ktera_cili_uz_patri_se_nemeni() {
        for konflikt in [KeyConflict::Move, KeyConflict::Share] {
            let mut e = engine_with(sdilene());
            let pred = e.mapping().clone();
            let _ = e.start_binding(na(P1, UP), BindKind::Add, konflikt, T0);
            let d = down(&mut e, KeyId::W);
            assert_eq!(
                d.ui,
                Some(UiEvent::BindingSaved {
                    key: KeyId::W,
                    target: na(P1, UP).into(),
                    moved_from: None,
                    moved_more: 0,
                    shared: 1,
                }),
                "{konflikt:?}"
            );
            assert_eq!(e.mapping(), &pred, "{konflikt:?}");
            assert_eq!(e.mapping_rev(), 0, "{konflikt:?}: nic se nezměnilo");
        }
    }

    /// Klik na čepičku (nahradit) u vstupu se sdílenou klávesou: ostatní
    /// klávesy vstupu o něj přijdou, sdílená si nechá ostatní cíle.
    #[test]
    fn nahrazeni_u_sdilene_klavesy() {
        let mut e = engine_with(sdilene());
        // Levá páčka nahoru druhého má I a W (sdílená s prvním).
        let _ = e.start_binding(na(P1, UP), BindKind::Replace, KeyConflict::Move, T0);
        let d = down(&mut e, KeyId::NUMPAD_8);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingSaved {
                key: KeyId::NUMPAD_8,
                target: na(P1, UP).into(),
                moved_from: None,
                moved_more: 0,
                shared: 0,
            })
        );
        assert_eq!(e.mapping().targets(KeyId::W), Targets::one(t0(UP)));
        assert!(e.mapping().targets(KeyId::I).is_empty());
        assert_eq!(
            e.mapping().keys_for(na(P1, UP)).collect::<Vec<_>>(),
            vec![KeyId::NUMPAD_8]
        );
        assert_eq!(e.mapping_rev(), 1);
    }

    // ── Zkratka pozastavení z okna (Fáze 7, Z6) ─────────────────────

    fn odmitnuto(d: &Decision, key: KeyId, reason: BindingReject) -> bool {
        d.ui == Some(UiEvent::BindingRejected { key, reason })
    }

    fn zkratka_ulozena(key: KeyId) -> Option<UiEvent> {
        Some(UiEvent::BindingSaved {
            key,
            target: BindTarget::Toggle,
            moved_from: None,
            moved_more: 0,
            shared: 0,
        })
    }

    /// Přiřazování zkratky začaté ze hry: pozastaví, uloží F24 (revize
    /// roste), vrátí se do hry; od teď pozastavuje F24 a Scroll Lock je
    /// obyčejná nenamapovaná klávesa (jde do Windows).
    #[test]
    fn zkratka_ulozena_a_navrat_do_hry() {
        let mut e = gamepad();
        let d = e.start_binding(BindTarget::Toggle, BindKind::Replace, KeyConflict::Move, T0);
        assert_eq!(
            e.mode(),
            Mode::Binding {
                target: BindTarget::Toggle,
                started_at_ms: T0
            }
        );
        assert!(vse_neutralni(&d), "přiřazování hru pozastaví");
        let d = down(&mut e, KeyId::F24);
        assert!(d.suppress, "stisk při přiřazování se spolkne");
        assert_eq!(d.ui, zkratka_ulozena(KeyId::F24));
        assert_eq!(e.mode(), Mode::Gamepad, "zpět do hry");
        assert_eq!(e.mapping().toggle_key(), KeyId::F24);
        assert_eq!(e.mapping_rev(), 1);
        assert!(up(&mut e, KeyId::F24).suppress, "key-up téhož vlastníka");
        // Nová zkratka pozastavuje, stará jde do Windows.
        let _ = down(&mut e, KeyId::F24);
        assert_eq!(e.mode(), Mode::Keyboard);
        let _ = up(&mut e, KeyId::F24);
        assert!(
            !down(&mut e, TOGGLE).suppress,
            "Scroll Lock už není zkratka"
        );
        assert_eq!(e.mode(), Mode::Keyboard);
        let _ = up(&mut e, TOGGLE);
    }

    /// Dosavadní zkratka = uloženo beze změny (revize neroste); větev
    /// „zkratku nejde přiřadit vstupu" se tu neuplatní.
    #[test]
    fn dosavadni_zkratka_ulozena_beze_zmeny() {
        let mut e = engine();
        let _ = e.start_binding(BindTarget::Toggle, BindKind::Replace, KeyConflict::Move, T0);
        let d = down(&mut e, TOGGLE);
        assert_eq!(d.ui, zkratka_ulozena(TOGGLE));
        assert_eq!(e.mode(), Mode::Keyboard, "přiřazování skončilo, nepřeplo");
        assert_eq!(e.mapping_rev(), 0);
        assert_eq!(e.mapping().toggle_key(), TOGGLE);
        assert!(up(&mut e, TOGGLE).suppress);
    }

    /// Pořadí vyhodnocení: Esc ruší, Win a nemapovatelná se odmítnou
    /// (a jdou Windows), mimo F1–F24 bez F4 / Scroll Lock / Pause `NotToggleKey`
    /// (i modifikátor ťuknutím), klávesa s cílem `Mapped`. Odmítnutí
    /// přiřazování nekončí a nic nemění.
    #[test]
    fn zkratka_odmitne_nevhodne_klavesy() {
        let mut m = Mapping::default();
        m.bind(KeyId::F5, t0(A_BTN), KeyConflict::Move).unwrap();
        let mut e = engine_with(m.clone());
        let _ = e.start_binding(BindTarget::Toggle, BindKind::Replace, KeyConflict::Move, T0);
        let ceka = |e: &Engine| {
            matches!(
                e.mode(),
                Mode::Binding {
                    target: BindTarget::Toggle,
                    ..
                }
            )
        };

        let d = down(&mut e, KeyId::LEFT_WIN);
        assert!(!d.suppress && odmitnuto(&d, KeyId::LEFT_WIN, BindingReject::Reserved));
        let _ = up(&mut e, KeyId::LEFT_WIN);
        let d = down(&mut e, KeyId::ALTGR_FAKE_CTRL);
        assert!(!d.suppress && odmitnuto(&d, KeyId::ALTGR_FAKE_CTRL, BindingReject::Unmappable));
        let _ = up(&mut e, KeyId::ALTGR_FAKE_CTRL);
        // F4: se zkratkou F4 by Alt+F4 nezavřel okno (OQ 69).
        for k in [
            KeyId::TAB,
            KeyId::ENTER,
            KeyId::W,
            KeyId::BACKSPACE,
            KeyId::F4,
        ] {
            let d = down(&mut e, k);
            assert!(d.suppress, "{k}: stisk při přiřazování se spolkne");
            assert!(
                odmitnuto(&d, k, BindingReject::NotToggleKey),
                "{k}: {:?}",
                d.ui
            );
            assert!(up(&mut e, k).suppress);
            assert!(ceka(&e), "{k}: přiřazování čeká dál");
        }
        // Shift ťuknutím: stisk jde Windows, odmítne se při key-upu.
        let d = down(&mut e, KeyId::LEFT_SHIFT);
        assert!(!d.suppress && d.ui.is_none());
        let d = up(&mut e, KeyId::LEFT_SHIFT);
        assert!(!d.suppress);
        assert!(
            odmitnuto(&d, KeyId::LEFT_SHIFT, BindingReject::NotToggleKey),
            "{:?}",
            d.ui
        );
        let d = down(&mut e, KeyId::F5);
        assert!(
            odmitnuto(&d, KeyId::F5, BindingReject::Mapped),
            "{:?}",
            d.ui
        );
        let _ = up(&mut e, KeyId::F5);
        assert!(ceka(&e));
        assert_eq!(e.mapping(), &m, "nic se nezměnilo");
        assert_eq!(e.mapping_rev(), 0);
        let d = down(&mut e, KeyId::ESC);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Escape
            })
        );
        assert_eq!(e.mode(), Mode::Keyboard);
        assert_eq!(e.mapping().toggle_key(), TOGGLE);
    }

    /// Při přiřazování vstupu zkratka dál odmítnutá jako vazba
    /// (`ToggleKey`), nová zkratka po uložení taky; po uložení zkratky
    /// ji vstupu přiřadit nejde a stará zkratka jde přiřadit.
    #[test]
    fn nova_zkratka_neni_vstup() {
        let mut e = engine();
        let _ = e.start_binding(BindTarget::Toggle, BindKind::Replace, KeyConflict::Move, T0);
        let _ = down(&mut e, KeyId::PAUSE);
        let _ = up(&mut e, KeyId::PAUSE);
        assert_eq!(e.mapping().toggle_key(), KeyId::PAUSE);
        let _ = e.start_binding(t0(A_BTN), BindKind::Replace, KeyConflict::Move, T0);
        let d = down(&mut e, KeyId::PAUSE);
        assert!(odmitnuto(&d, KeyId::PAUSE, BindingReject::ToggleKey));
        let _ = up(&mut e, KeyId::PAUSE);
        let d = down(&mut e, TOGGLE);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingSaved {
                key: TOGGLE,
                target: t0(A_BTN).into(),
                moved_from: None,
                moved_more: 0,
                shared: 0,
            })
        );
        let _ = up(&mut e, TOGGLE);
    }

    /// Stará zkratka držená ve chvíli uložení nové se dál spolkne až do
    /// uvolnění (princip 2) a nic nepřepne.
    #[test]
    fn drzena_stara_zkratka_se_dal_spolkne() {
        let mut e = gamepad();
        assert!(down(&mut e, TOGGLE).suppress);
        assert_eq!(e.mode(), Mode::Keyboard);
        let _ = e.start_binding(BindTarget::Toggle, BindKind::Replace, KeyConflict::Move, T0);
        let _ = down(&mut e, KeyId::F1);
        let _ = up(&mut e, KeyId::F1);
        assert_eq!(e.mapping().toggle_key(), KeyId::F1);
        // Autorepeat staré zkratky i její key-up: spolknuto, nic se neděje.
        let d = down(&mut e, TOGGLE);
        assert!(d.suppress && d.ui.is_none());
        assert!(up(&mut e, TOGGLE).suppress);
        assert_eq!(e.mode(), Mode::Keyboard);
    }
}

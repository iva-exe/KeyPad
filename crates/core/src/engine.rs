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
//! i mapování jsou pevné tabulky o 256 položkách, nic se nehashuje
//! a za běhu nic nealokuje.
//!
//! Jádrem je **pravidlo vlastnictví klávesy** (princip 2): kam patří
//! stisk, se rozhoduje JEDNOU při key-down a key-up (i autorepeat) jde
//! vždy stejnému vlastníkovi. Díky tomu přepnutí režimu nikdy nezasekne
//! klávesu ani páčku — OS dostane key-up právě ke key-downům, které
//! viděl, a pad ztratí jen klávesy, které držel on.

use serde::{Deserialize, Serialize};

use crate::action::Action;
use crate::key::{KeyId, KEY_TABLE_SIZE};
use crate::mapping::{Mapping, MappingError};
use crate::pad_state::{compute_pad_state, PadState};

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

/// Proč nejde přepnout na Gamepad.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DisabledReason {
    /// Ovladač ViGEmBus není nainstalovaný.
    ViGEmMissing,
    /// Virtuální pad se ještě nepřipojil (start aplikace, nové připojení).
    PadNotConnected,
    /// Posílání stavu do ViGEm selhalo — čeká se na „Zkusit znovu".
    PadError,
}

/// Režim aplikace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mode {
    /// Vše jde do OS (výchozí a fail-safe režim).
    Keyboard,
    /// Namapované klávesy ovládají pad, ostatní jdou do OS.
    Gamepad,
    /// Čeká se na klávesu, která se přiřadí akci `action`. Jde se sem
    /// jen z režimu Klávesnice a vrací se tam.
    Binding { action: Action, started_at_ms: u64 },
    /// Jako Klávesnice, ale na Gamepad přepnout nejde (pad není).
    Disabled { reason: DisabledReason },
}

/// Komu patří stisknutá klávesa. Určí se při key-down a už se nemění —
/// s jedinou výjimkou: při odchodu z režimu Gamepad se `Pad` mění na
/// `Swallow` (OS key-down nikdy neviděl, tak nesmí dostat ani key-up).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Owner {
    /// Klávesa jde do OS.
    Os,
    /// Klávesa ovládá pad; nese akci určenou při stisku.
    Pad(Action),
    /// Klávesa nejde nikam (zkratka přepnutí, přiřazování, klávesa
    /// padu po odchodu z režimu Gamepad).
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
    /// Pad se stal (ne)dostupným — [`Engine::disable`] / [`Engine::enable`].
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
    /// Změna mapování (v režimu Gamepad se nemapuje, viz Fáze 6).
    MappingChanged,
    /// Zavírání aplikace.
    Shutdown,
}

/// Proč se přiřazování klávesy zrušilo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BindingCancel {
    Escape,
    Timeout,
    Gui,
    Forced(ForceReason),
    /// Pad přestal být dostupný (přiřazování skončilo přechodem do
    /// režimu Disabled).
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
}

/// Proč přepnutí neproběhlo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToggleReject {
    /// Pad není k dispozici.
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
    /// Klávesa byla přiřazena akci a režim je zpět Klávesnice.
    /// `moved_from` = akce, které klávesa patřila dosud (vazba se přesunula).
    BindingSaved {
        key: KeyId,
        action: Action,
        moved_from: Option<Action>,
    },
    /// Stisknutou klávesu nejde přiřadit; čeká se dál na jinou.
    BindingRejected {
        key: KeyId,
        reason: BindingReject,
    },
    /// Přiřazování skončilo bez uložení; režim je zpět Klávesnice
    /// (nebo Disabled, když zrušení způsobil pad).
    BindingCancelled {
        reason: BindingCancel,
    },
    ToggleRejected {
        reason: ToggleReject,
    },
}

/// Výsledek zpracování události.
///
/// - `suppress`: hook má událost spolknout (nepředat OS). U příkazů,
///   které nejsou klávesou, je vždy `false` a nemá význam.
/// - `pad`: nový stav padu k odeslání do ViGEm; `None` = beze změny.
///   Při každé změně režimu je `Some`.
/// - `ui`: oznámení pro okno. Když se v jednom volání stane víc věcí
///   (propadlé přiřazování + stisk zkratky), nese to novější.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[must_use]
pub struct Decision {
    pub suppress: bool,
    pub pad: Option<PadState>,
    pub ui: Option<UiEvent>,
}

impl Decision {
    /// Nic se nestalo.
    pub const NONE: Decision = Decision {
        suppress: false,
        pad: None,
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
            pad: self.pad.or(earlier.pad),
            ui: self.ui.or(earlier.ui),
        }
    }
}

/// Chyby příkazů z okna.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandError {
    /// Přiřazovat klávesy jde jen z režimu Klávesnice.
    NotInKeyboardMode(Mode),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::NotInKeyboardMode(m) => {
                write!(
                    f,
                    "přiřazovat klávesy jde jen v režimu Klávesnice (teď {m:?})"
                )
            }
        }
    }
}

impl std::error::Error for CommandError {}

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
    /// Naposledy vydaný stav padu — ať se neposílá znovu totéž.
    last_pad: PadState,
}

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
            .field("held", &held)
            .field("seq", &self.seq)
            .field("last_pad", &self.last_pad)
            .field("mapping", &self.mapping)
            .finish()
    }
}

impl Engine {
    /// Nový engine v režimu `Disabled { PadNotConnected }`.
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
            last_pad: PadState::NEUTRAL,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn mapping(&self) -> &Mapping {
        &self.mapping
    }

    /// Stav padu spočítaný z aktuálně držených kláves.
    pub fn pad_state(&self) -> PadState {
        self.compute_pad()
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
            // Nemapovatelná klávesa jde vždy do OS a nesleduje se.
            // Při přiřazování okno aspoň dozví, proč se nic nestalo.
            return Decision {
                ui: matches!(self.mode, Mode::Binding { .. }).then_some(UiEvent::BindingRejected {
                    key,
                    reason: BindingReject::Unmappable,
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
        self.remember(
            i,
            HeldKey {
                owner,
                seq: self.seq,
                last_ms: now_ms,
            },
        );

        let effects = if key == self.mapping.toggle_key() {
            self.toggle_with(ModeCause::Hotkey)
        } else if let Mode::Binding { action, .. } = self.mode {
            self.binding_key(key, action)
        } else {
            // Klávesa padu nebo klávesa, která zahodila zastaralý záznam
            // padu — obojí může změnit stav.
            Decision {
                pad: self.pad_if_changed(),
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
            Mode::Binding { .. } => Owner::Swallow,
            Mode::Gamepad => self.mapping.action_for(key).map_or(Owner::Os, Owner::Pad),
            Mode::Keyboard | Mode::Disabled { .. } => Owner::Os,
        }
    }

    fn key_up(&mut self, key: KeyId) -> Decision {
        let Some(h) = key.index().and_then(|i| self.forget(i)) else {
            // Key-up bez záznamu: klávesa držená už před spuštěním,
            // zapomenutá po zamčení relace, nebo nemapovatelná. OS její
            // key-down viděl, takže key-up patří jemu.
            return Decision::NONE;
        };
        Decision {
            suppress: h.owner.suppresses(),
            pad: match h.owner {
                Owner::Pad(_) => self.pad_if_changed(),
                Owner::Os | Owner::Swallow => None,
            },
            ui: None,
        }
    }

    /// Klávesa stisknutá během přiřazování (ne zkratka přepnutí).
    fn binding_key(&mut self, key: KeyId, action: Action) -> Decision {
        if key == KeyId::ESC {
            return self.leave_binding(Mode::Keyboard, BindingCancel::Escape);
        }
        match self.mapping.bind(key, action) {
            Ok(moved_from) => {
                let pad = self.switch_mode(Mode::Keyboard);
                Decision {
                    suppress: false,
                    pad: Some(pad),
                    ui: Some(UiEvent::BindingSaved {
                        key,
                        action,
                        moved_from,
                    }),
                }
            }
            // Zkratku i nemapovatelné klávesy odchytí key_down dřív;
            // tahle větev je jen pojistka, kdyby bind přibyla jiná chyba.
            Err(e) => Decision {
                ui: Some(UiEvent::BindingRejected {
                    key,
                    reason: match e {
                        MappingError::ToggleKeyMapped { .. } => BindingReject::ToggleKey,
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

    /// Vynutí režim Klávesnice (fail-safe: watchdog, chyba padu,
    /// zavírání…). Z Gamepadu okamžitě pošle neutrální stav a klávesy
    /// padu spolkne; běžící přiřazování zruší. Režim Disabled nechá —
    /// ten se chová jako Klávesnice a navíc nepustí na Gamepad.
    ///
    /// Stav padu vrací vždy (i když se nic nezměnilo): volá se, když
    /// něco selhalo, a potvrdit neutrál je tehdy levnější než zjišťovat,
    /// jestli ho pad opravdu má.
    ///
    /// Pro paniku v hook callbacku použij [`Engine::reset_held`], ne
    /// tohle: hook klávesu po panice propustí do OS, a kdyby její záznam
    /// zůstal jako `Swallow`, key-up by se spolkl a klávesa by v OS visela.
    pub fn force_keyboard(&mut self, reason: ForceReason) -> Decision {
        match self.mode {
            Mode::Gamepad => self.change_mode(Mode::Keyboard, ModeCause::Forced(reason)),
            Mode::Binding { .. } => {
                self.leave_binding(Mode::Keyboard, BindingCancel::Forced(reason))
            }
            Mode::Keyboard | Mode::Disabled { .. } => Decision {
                pad: Some(self.emit_pad()),
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
            pad: Some(self.emit_pad()),
            ..d
        }
    }

    /// Začne přiřazování klávesy akci. Jen z režimu Klávesnice.
    pub fn start_binding(&mut self, action: Action, now_ms: u64) -> Result<Decision, CommandError> {
        // Propadlé přiřazování (časovač nestihl tiknout) nesmí nové
        // odmítnout s tím, že se pořád přiřazuje.
        let expired = self.expire_binding(now_ms);
        if self.mode != Mode::Keyboard {
            return Err(CommandError::NotInKeyboardMode(self.mode));
        }
        Ok(self
            .change_mode(
                Mode::Binding {
                    action,
                    started_at_ms: now_ms,
                },
                ModeCause::Gui,
            )
            .or(expired))
    }

    /// Zruší přiřazování (tlačítko v okně). Mimo přiřazování nic nedělá.
    pub fn cancel_binding(&mut self, now_ms: u64) -> Decision {
        let expired = self.expire_binding(now_ms);
        match self.mode {
            Mode::Binding { .. } => self.leave_binding(Mode::Keyboard, BindingCancel::Gui),
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
                self.leave_binding(Mode::Keyboard, BindingCancel::Timeout)
            }
            _ => Decision::NONE,
        }
    }

    /// Pad není k dispozici (ViGEmBus chybí, pad se odpojil, chyba).
    /// Z Gamepadu se chová jako vynucená Klávesnice; přiřazování zruší.
    pub fn disable(&mut self, reason: DisabledReason) -> Decision {
        let new = Mode::Disabled { reason };
        match self.mode {
            Mode::Binding { .. } => self.leave_binding(new, BindingCancel::PadStatus),
            m if m == new => Decision::NONE,
            _ => self.change_mode(new, ModeCause::PadStatus),
        }
    }

    /// Pad je připojený. Z Disabled jde VŽDY na Klávesnici, nikdy rovnou
    /// na Gamepad — o přepnutí rozhoduje uživatel.
    pub fn enable(&mut self) -> Decision {
        match self.mode {
            Mode::Disabled { .. } => self.change_mode(Mode::Keyboard, ModeCause::PadStatus),
            _ => Decision::NONE,
        }
    }

    /// Nahradí mapování. V režimu Gamepad se nejdřív vynutí Klávesnice
    /// (pad nesmí zůstat s vazbami, které už neplatí); běžící
    /// přiřazování se zruší.
    pub fn set_mapping(&mut self, mapping: Mapping) -> Decision {
        let d = match self.mode {
            Mode::Gamepad | Mode::Binding { .. } => {
                self.force_keyboard(ForceReason::MappingChanged)
            }
            Mode::Keyboard | Mode::Disabled { .. } => Decision::NONE,
        };
        self.mapping = mapping;
        d
    }

    // ── Vnitřní přechody ─────────────────────────────────────────────

    /// Změní režim a oznámí to.
    fn change_mode(&mut self, new: Mode, cause: ModeCause) -> Decision {
        let pad = self.switch_mode(new);
        Decision {
            suppress: false,
            pad: Some(pad),
            ui: Some(UiEvent::ModeChanged { mode: new, cause }),
        }
    }

    /// Ukončí přiřazování přechodem do `to`.
    fn leave_binding(&mut self, to: Mode, reason: BindingCancel) -> Decision {
        let pad = self.switch_mode(to);
        Decision {
            suppress: false,
            pad: Some(pad),
            ui: Some(UiEvent::BindingCancelled { reason }),
        }
    }

    /// Jediné místo, kde se mění `mode`.
    ///
    /// Odchod z Gamepadu: klávesy padu se mění na `Swallow` — jejich
    /// key-up se spolkne (OS nikdy neviděl key-down) a pad se jich
    /// zbaví hned, ne až po uvolnění. Příchod do Gamepadu: klávesy
    /// držené s vlastníkem `Os` zůstávají OS až do uvolnění.
    ///
    /// Stav padu se po každé změně režimu vydá vždy — po odchodu
    /// z Gamepadu je to okamžitý neutrál.
    fn switch_mode(&mut self, new: Mode) -> PadState {
        if self.mode == Mode::Gamepad && new != Mode::Gamepad {
            for h in self.held.iter_mut().flatten() {
                if let Owner::Pad(_) = h.owner {
                    h.owner = Owner::Swallow;
                }
            }
        }
        self.mode = new;
        self.emit_pad()
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

    fn compute_pad(&self) -> PadState {
        compute_pad_state(self.held.iter().flatten().filter_map(|h| match h.owner {
            Owner::Pad(action) => Some((action, h.seq)),
            Owner::Os | Owner::Swallow => None,
        }))
    }

    /// Přepočítá stav a vydá ho vždy.
    fn emit_pad(&mut self) -> PadState {
        self.last_pad = self.compute_pad();
        self.last_pad
    }

    /// Přepočítá stav a vydá ho jen při změně (druhá klávesa téhož
    /// tlačítka nemá smysl posílat do ViGEm znovu).
    fn pad_if_changed(&mut self) -> Option<PadState> {
        let new = self.compute_pad();
        (new != self.last_pad).then(|| {
            self.last_pad = new;
            new
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{PadButton, StickDir};
    use crate::pad_state::{AXIS_DIAGONAL, AXIS_MAX};

    const TOGGLE: KeyId = KeyId::SCROLL_LOCK;
    const T0: u64 = 1_000;

    /// Engine s připojeným padem (režim Klávesnice).
    fn engine_with(m: Mapping) -> Engine {
        let mut e = Engine::new(m);
        let _ = e.enable();
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
            d.pad.expect("stav po přepnutí").is_neutral(),
            "gamepad W neukáže"
        );
        // Autorepeat W jde dál do OS a pad nehne.
        let r = down(&mut e, KeyId::W);
        assert!(!r.suppress);
        assert_eq!(r.pad, None);
        assert!(e.pad_state().is_neutral());
        // W-up jde do OS (ten viděl key-down).
        let u = up(&mut e, KeyId::W);
        assert!(!u.suppress, "W-up musí dostat OS");
        assert_eq!(u.pad, None);
        // Nový stisk W už patří padu.
        let n = down(&mut e, KeyId::W);
        assert!(n.suppress);
        assert_eq!(stick(n.pad.unwrap()), (0, AXIS_MAX));
    }

    #[test]
    fn prepnuti_do_klavesnice_pri_drzenem_w() {
        let mut e = gamepad();
        let d = down(&mut e, KeyId::W);
        assert!(d.suppress);
        assert_eq!(stick(d.pad.unwrap()), (0, AXIS_MAX));

        let t = down(&mut e, TOGGLE);
        assert_eq!(e.mode(), Mode::Keyboard);
        assert!(
            t.pad.expect("okamžitý neutrál").is_neutral(),
            "páčka hned neutrální"
        );
        assert_eq!(e.held(KeyId::W).unwrap().owner, Owner::Swallow);

        // Autorepeat i key-up W se spolknou — OS key-down nikdy neviděl.
        let r = down(&mut e, KeyId::W);
        assert!(r.suppress);
        assert_eq!(r.pad, None);
        let u = up(&mut e, KeyId::W);
        assert!(u.suppress, "W-up spolknut");
        assert_eq!(u.pad, None, "pad už je neutrální, nic se neposílá");
        assert!(e.pad_state().is_neutral());
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
            assert_eq!(r.pad, None);
            assert_eq!(e.mode(), Mode::Gamepad);
        }
        assert!(e.on_key(TOGGLE, false, T0 + 3_400).suppress);
        assert_eq!(e.mode(), Mode::Gamepad);

        // Autorepeat klávesy padu stav nemění a nic neposílá.
        let d = down(&mut e, KeyId::D);
        let stav = d.pad.unwrap();
        for n in 1..=100 {
            let r = e.on_key(KeyId::D, true, T0 + n * 33);
            assert!(r.suppress);
            assert_eq!(r.pad, None);
            assert_eq!(r.ui, None);
        }
        assert_eq!(e.pad_state(), stav);
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
        assert_eq!(stick(down(&mut e, KeyId::A).pad.unwrap()), (-AXIS_MAX, 0));
        assert_eq!(
            stick(down(&mut e, KeyId::D).pad.unwrap()),
            (AXIS_MAX, 0),
            "vyhrává D"
        );
        assert_eq!(
            stick(up(&mut e, KeyId::D).pad.unwrap()),
            (-AXIS_MAX, 0),
            "zpět k A"
        );
        assert_eq!(stick(up(&mut e, KeyId::A).pad.unwrap()), (0, 0));
    }

    #[test]
    fn diagonala_w_d_a_samotne_w() {
        let mut e = gamepad();
        assert_eq!(stick(down(&mut e, KeyId::W).pad.unwrap()), (0, 32_767));
        let p = down(&mut e, KeyId::D).pad.unwrap();
        assert_eq!(stick(p), (23_170, 23_170));
        assert_eq!(AXIS_DIAGONAL, 23_170);
        assert_eq!(stick(up(&mut e, KeyId::D).pad.unwrap()), (0, 32_767));
    }

    #[test]
    fn dve_klavesy_na_stejne_tlacitko() {
        let mut m = Mapping::default();
        m.bind(KeyId::X, Action::Button(PadButton::A)).unwrap();
        let mut e = engine_with(m);
        let _ = e.toggle(T0);

        let d = down(&mut e, KeyId::SPACE);
        assert!(d.pad.unwrap().is_pressed(PadButton::A));
        // Druhá klávesa téhož tlačítka: stav se nemění, nic se neposílá.
        let d = down(&mut e, KeyId::X);
        assert!(d.suppress);
        assert_eq!(d.pad, None);
        // Uvolnění jedné nechá tlačítko stisknuté.
        let d = up(&mut e, KeyId::SPACE);
        assert!(d.suppress);
        assert_eq!(d.pad, None);
        assert!(e.pad_state().is_pressed(PadButton::A));
        // Až poslední klávesa ho pustí.
        let d = up(&mut e, KeyId::X);
        assert!(!d.pad.unwrap().is_pressed(PadButton::A));
    }

    #[test]
    fn binding_esc_rusi() {
        let mut e = engine();
        let before = e.mapping().clone();
        let _ = e.start_binding(Action::Button(PadButton::A), T0).unwrap();
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
        let _ = e.start_binding(Action::LeftTrigger, T0).unwrap();
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
        let _ = e.start_binding(Action::LeftTrigger, T0).unwrap();
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
        let _ = e.start_binding(Action::Button(PadButton::Y), T0).unwrap();
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
                action: Action::Button(PadButton::Y),
                moved_from: None
            })
        );
        assert_eq!(
            e.mapping().action_for(KeyId::X),
            Some(Action::Button(PadButton::Y))
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
        let d = e.start_binding(Action::RightTrigger, T0).unwrap();
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
                action: Action::RightTrigger,
                moved_from: Some(Action::LeftStick(StickDir::Up))
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
        let _ = e.start_binding(Action::LeftTrigger, T0).unwrap();
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
    fn klavesy_bez_scan_kodu_si_nesdileji_zaznam() {
        // Dvě mediální klávesy (obě scan 0) se v enginu nesmí plést:
        // žádná z nich se nesleduje, všechno jde do OS.
        let mut e = engine();
        let media = KeyId::new(0);
        assert!(!down(&mut e, media).suppress);
        let _ = e.start_binding(Action::LeftTrigger, T0).unwrap();
        assert!(!down(&mut e, media).suppress);
        assert!(!up(&mut e, media).suppress);
        assert!(!down(&mut e, media).suppress);
        assert!(!up(&mut e, media).suppress);
        assert_eq!(e.held_len(), 0);
    }

    #[test]
    fn binding_jen_z_klavesnice() {
        let mut e = gamepad();
        assert_eq!(
            e.start_binding(Action::LeftTrigger, T0),
            Err(CommandError::NotInKeyboardMode(Mode::Gamepad))
        );
        let mut e = engine();
        let _ = e.disable(DisabledReason::ViGEmMissing);
        assert!(e.start_binding(Action::LeftTrigger, T0).is_err());
    }

    #[test]
    fn prikazy_berou_v_potaz_propadle_prirazovani() {
        // Časovač nestihl tiknout; příkaz z okna nesmí odpovědět, že se
        // pořád přiřazuje.
        let late = T0 + BINDING_TIMEOUT_MS + 1;

        let mut e = engine();
        let _ = e.start_binding(Action::LeftTrigger, T0).unwrap();
        let d = e.start_binding(Action::RightTrigger, late).unwrap();
        assert_eq!(
            e.mode(),
            Mode::Binding {
                action: Action::RightTrigger,
                started_at_ms: late
            }
        );
        assert!(matches!(d.ui, Some(UiEvent::ModeChanged { .. })));

        let mut e = engine();
        let _ = e.start_binding(Action::LeftTrigger, T0).unwrap();
        let _ = e.toggle(late);
        assert_eq!(e.mode(), Mode::Gamepad);

        let mut e = engine();
        let _ = e.start_binding(Action::LeftTrigger, T0).unwrap();
        assert_eq!(
            e.cancel_binding(late).ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Timeout
            })
        );
    }

    #[test]
    fn binding_autorepeat_drzene_klavesy_nic_neprirazuje() {
        // Klávesa držená už před začátkem přiřazování patří OS; její
        // autorepeat není nový stisk, takže se nepřiřadí.
        let mut e = engine();
        assert!(!down(&mut e, KeyId::W).suppress);
        let before = e.mapping().clone();
        let _ = e.start_binding(Action::LeftTrigger, T0).unwrap();
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
            assert_eq!(d.pad, None);
            assert!(!up(&mut e, k).suppress, "{k}");
        }
    }

    #[test]
    fn altgr_nespousti_akci_na_lctrl() {
        let mut m = Mapping::default();
        m.bind(KeyId::LEFT_CTRL, Action::Button(PadButton::B))
            .unwrap();
        let mut e = engine_with(m);
        let _ = e.toggle(T0);
        let d = down(&mut e, KeyId::ALTGR_FAKE_CTRL);
        assert!(!d.suppress);
        assert!(e.pad_state().is_neutral());
    }

    #[test]
    fn klavesnice_nic_nepotlacuje_krome_zkratky() {
        let mut e = engine();
        for k in [KeyId::W, KeyId::SPACE, KeyId::ARROW_UP, KeyId::ESC] {
            assert!(!down(&mut e, k).suppress);
            assert!(!up(&mut e, k).suppress);
        }
        assert!(e.pad_state().is_neutral());
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
        assert!(d.pad.unwrap().is_neutral());
        assert!(up(&mut e, KeyId::W).suppress);
    }

    #[test]
    fn disabled_nepusti_na_gamepad_a_propousti_klavesy() {
        let mut e = engine();
        let _ = e.disable(DisabledReason::ViGEmMissing);
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
        let d = e.enable();
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
    }

    #[test]
    fn chyba_padu_v_gamepadu_vynuti_klavesnici() {
        let mut e = gamepad();
        let _ = down(&mut e, KeyId::W);
        let d = e.disable(DisabledReason::PadError);
        assert!(d.pad.unwrap().is_neutral());
        assert_eq!(
            e.mode(),
            Mode::Disabled {
                reason: DisabledReason::PadError
            }
        );
        assert!(up(&mut e, KeyId::W).suppress, "W-up spolknut");
    }

    #[test]
    fn vynucena_klavesnice() {
        let mut e = gamepad();
        let _ = down(&mut e, KeyId::A);
        let d = e.force_keyboard(ForceReason::Watchdog);
        assert_eq!(e.mode(), Mode::Keyboard);
        assert!(d.pad.unwrap().is_neutral());
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
        assert!(d.pad.unwrap().is_neutral());
    }

    #[test]
    fn zamceni_relace_zapomene_drzene_klavesy() {
        let mut e = gamepad();
        let _ = down(&mut e, KeyId::W);
        let _ = down(&mut e, KeyId::X); // OS
        let d = e.reset_held(ForceReason::SessionLock);
        assert!(d.pad.unwrap().is_neutral());
        assert_eq!(e.mode(), Mode::Keyboard);
        assert_eq!(e.held_len(), 0);
        // Key-upy po odemčení nemají záznam → jdou do OS.
        assert!(!up(&mut e, KeyId::W).suppress);
        assert!(!up(&mut e, KeyId::X).suppress);
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
        assert_eq!(stick(e.pad_state()), (0, AXIS_MAX));
        let d = e.on_key(KeyId::W, false, T0 + 60_100);
        assert!(d.pad.unwrap().is_neutral(), "a jeho key-up páčku pustí");
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
        assert!(d.pad.unwrap().is_neutral());
        assert!(up(&mut e, KeyId::W).suppress);
        assert_eq!(e.mapping().action_for(KeyId::W), None);
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
        assert!(e.pad_state().is_neutral());
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
        assert_eq!(d.pad.unwrap().thumb_ry, AXIS_MAX);
    }

    #[test]
    fn udalosti_jdou_serializovat() {
        let ev = UiEvent::ModeChanged {
            mode: Mode::Binding {
                action: Action::Button(PadButton::Lb),
                started_at_ms: 5,
            },
            cause: ModeCause::Forced(ForceReason::Watchdog),
        };
        let d = Decision {
            suppress: true,
            pad: Some(PadState::NEUTRAL),
            ui: Some(ev),
        };
        // serde_json v core není; stačí, že se typy dají předat
        // serializátoru — ověří to kompilace téhle funkce.
        fn je_serde<T: Serialize + for<'de> Deserialize<'de>>(_: &T) {}
        je_serde(&d);
        je_serde(&MappingError::Empty);
        je_serde(&CommandError::NotInKeyboardMode(Mode::Gamepad));
    }
}

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
//! Víc ovladačů (Fáze 4): každá klávesa patří nejvýš jednomu ovladači
//! ([`PadAction`]) a každý ovladač je zvlášť „připravený" (připojený)
//! nebo ne. Režim je ale jeden pro všechny — zkratka pozastaví
//! zachytávání celé klávesnice najednou, jinak by se do chatu psát
//! nedalo.

use serde::{Deserialize, Serialize};

use crate::action::{PadAction, PadId, MAX_PADS};
use crate::key::{KeyId, KEY_TABLE_SIZE};
use crate::mapping::{Mapping, MappingError};
use crate::pad_state::{compute_pad_state, PadState, PadUpdates};

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
        target: PadAction,
        started_at_ms: u64,
    },
    /// Jako Klávesnice, ale zachytávat nejde: žádný ovladač není
    /// připravený.
    Disabled { reason: DisabledReason },
}

/// Komu patří stisknutá klávesa. Určí se při key-down a už se nemění —
/// s jedinou výjimkou: `Pad` se mění na `Swallow`, když se přestane
/// zachytávat nebo když jeho ovladač vypadne (OS key-down nikdy neviděl,
/// tak nesmí dostat ani key-up).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Owner {
    /// Klávesa jde do OS.
    Os,
    /// Klávesa ovládá ovladač; nese akci a ovladač určené při stisku.
    Pad(PadAction),
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
    /// [`Mode::Binding`]). `moved_from` = cíl, kterému klávesa patřila
    /// dosud (vazba se přesunula — i z jiného ovladače).
    BindingSaved {
        key: KeyId,
        target: PadAction,
        moved_from: Option<PadAction>,
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
            .field("ready", &self.ready)
            .field("held", &held)
            .field("seq", &self.seq)
            .field("last_pads", &self.last_pads)
            .field("disabled_reason", &self.disabled_reason)
            .field("resume_capture", &self.resume_capture)
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
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn mapping(&self) -> &Mapping {
        &self.mapping
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
    /// zapomněl držené klávesy). Hook to pozná z asynchronního stavu
    /// klávesnice u key-downu bez záznamu a zavolá tohle PŘED
    /// [`Engine::on_key`].
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
        } else if let Mode::Binding { target, .. } = self.mode {
            self.binding_key(key, target)
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
            Mode::Binding { .. } => Owner::Swallow,
            // Klávesa nepřipraveného ovladače jde do OS: ovladač, který
            // neexistuje, by ji jen spolkl a uživateli by zmizela.
            Mode::Gamepad => match self.mapping.target(key) {
                Some(t) if self.is_ready(t.pad) => Owner::Pad(t),
                _ => Owner::Os,
            },
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
            pads: match h.owner {
                Owner::Pad(_) => self.pads_if_changed(),
                Owner::Os | Owner::Swallow => PadUpdates::NONE,
            },
            ui: None,
        }
    }

    /// Klávesa stisknutá během přiřazování (ne zkratka přepnutí).
    fn binding_key(&mut self, key: KeyId, target: PadAction) -> Decision {
        if key == KeyId::ESC {
            return self.leave_binding(BindingCancel::Escape);
        }
        match self.mapping.bind(key, target) {
            Ok(moved_from) => {
                // Klávesa má vlastníka Swallow (stisknutá při
                // přiřazování) — i když se teď vrací zachytávání, její
                // key-up se spolkne a ovladač ji neuvidí.
                let to = self.mode_after_binding(true);
                Decision {
                    suppress: false,
                    pads: self.switch_mode(to),
                    ui: Some(UiEvent::BindingSaved {
                        key,
                        target,
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
    /// konci vrátí; běžící přiřazování začne znovu s novým cílem.
    pub fn start_binding(&mut self, target: PadAction, now_ms: u64) -> Decision {
        // Propadlé přiřazování (časovač nestihl tiknout) se nejdřív
        // ukončí — o návratu zachytávání pak rozhoduje režim, do kterého
        // se vrátilo, ne zapomenutý příznak.
        let expired = self.expire_binding(now_ms);
        self.resume_capture = match self.mode {
            Mode::Gamepad => true,
            Mode::Binding { .. } => self.resume_capture,
            Mode::Keyboard | Mode::Disabled { .. } => false,
        };
        self.change_mode(
            Mode::Binding {
                target,
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
    /// Klávesy, které ovladač držel, se spolknou (OS jejich key-down
    /// neviděl) a ovladač dostane neutrál; ostatní ovladače hrají dál.
    /// Vypadl-li poslední připravený ovladač, Klávesnice i Gamepad
    /// končí v Disabled. Přiřazování běží dál — bez ovladače je povolené
    /// a po konci se vrátí do Disabled s tímhle důvodem.
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
            if matches!(h.owner, Owner::Pad(t) if t.pad == pad) {
                h.owner = Owner::Swallow;
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

    /// Stav jednoho ovladače — jen z kláves, které patří jemu.
    fn compute_pad(&self, pad: PadId) -> PadState {
        compute_pad_state(self.held.iter().flatten().filter_map(|h| match h.owner {
            Owner::Pad(t) if t.pad == pad => Some((t.action, h.seq)),
            Owner::Pad(_) | Owner::Os | Owner::Swallow => None,
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
        m.bind(KeyId::X, t0(Action::Button(PadButton::A))).unwrap();
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
        let _ = e.start_binding(t0(Action::Button(PadButton::A)), T0);
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
        let _ = e.start_binding(t0(Action::LeftTrigger), T0);
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
        let _ = e.start_binding(t0(Action::LeftTrigger), T0);
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
        let _ = e.start_binding(t0(Action::Button(PadButton::Y)), T0);
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
                target: t0(Action::Button(PadButton::Y)),
                moved_from: None
            })
        );
        assert_eq!(
            e.mapping().target(KeyId::X),
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
        let d = e.start_binding(t0(Action::RightTrigger), T0);
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
                target: t0(Action::RightTrigger),
                moved_from: Some(t0(Action::LeftStick(StickDir::Up)))
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
        let _ = e.start_binding(t0(Action::LeftTrigger), T0);
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
        let _ = e.start_binding(t0(Action::LeftTrigger), T0);
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
        let _ = e.start_binding(t0(Action::LeftTrigger), T0);
        let d = e.start_binding(t0(Action::RightTrigger), late);
        assert_eq!(
            e.mode(),
            Mode::Binding {
                target: t0(Action::RightTrigger),
                started_at_ms: late
            }
        );
        assert!(matches!(d.ui, Some(UiEvent::ModeChanged { .. })));

        let mut e = engine();
        let _ = e.start_binding(t0(Action::LeftTrigger), T0);
        let _ = e.toggle(late);
        assert_eq!(e.mode(), Mode::Gamepad);

        let mut e = engine();
        let _ = e.start_binding(t0(Action::LeftTrigger), T0);
        assert_eq!(
            e.cancel_binding(late).ui,
            Some(UiEvent::BindingCancelled {
                reason: BindingCancel::Timeout
            })
        );

        // capture po propadlém přiřazování zapne zachytávání hned.
        let mut e = engine();
        let _ = e.start_binding(t0(Action::LeftTrigger), T0);
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
        let _ = e.start_binding(t0(Action::LeftTrigger), T0);
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
        m.bind(KeyId::LEFT_CTRL, t0(Action::Button(PadButton::B)))
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
        assert_eq!(e.mapping().target(KeyId::W), None);
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
                target: PadAction::new(P1, Action::Button(PadButton::Lb)),
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
        je_serde(&Owner::Pad(t0(Action::LeftTrigger)));
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
            m.bind(k, PadAction::new(P1, Action::LeftStick(d))).unwrap();
        }
        m.bind(KeyId::X, PadAction::new(P1, Action::Button(PadButton::A)))
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
        let d = e.start_binding(t0(Action::Button(PadButton::Y)), T0);
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
        let _ = e.start_binding(akce, T0);
        let _ = down(&mut e, KeyId::ESC);
        assert_eq!(e.mode(), Mode::Gamepad, "Esc");
        assert!(up(&mut e, KeyId::ESC).suppress);

        let mut e = gamepad();
        let _ = e.start_binding(akce, T0);
        let _ = e.tick(T0 + BINDING_TIMEOUT_MS);
        assert_eq!(e.mode(), Mode::Gamepad, "timeout");

        let mut e = gamepad();
        let _ = e.start_binding(akce, T0);
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
        let _ = e.start_binding(akce, T0);
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
        let _ = e.start_binding(akce, T0);
        let _ = e.set_mapping(Mapping::default());
        assert_eq!(e.mode(), Mode::Keyboard, "změna mapování");

        let mut e = gamepad();
        let _ = e.start_binding(akce, T0);
        let _ = e.reset_held(ForceReason::SessionLock);
        assert_eq!(e.mode(), Mode::Keyboard, "zamčení relace");
        // Příznak návratu se zapomněl: další přiřazování z Klávesnice se
        // vrací na Klávesnici.
        let _ = e.start_binding(akce, T0);
        let _ = e.cancel_binding(T0);
        assert_eq!(e.mode(), Mode::Keyboard);
    }

    #[test]
    fn prirazovani_bez_ovladace_se_vrati_do_disabled() {
        let mut e = Engine::new(Mapping::default());
        let _ = e.disable(P0, DisabledReason::ViGEmMissing);
        let cil = PadAction::new(P2, Action::RightTrigger);
        let d = e.start_binding(cil, T0);
        assert!(matches!(e.mode(), Mode::Binding { .. }));
        assert!(matches!(d.ui, Some(UiEvent::ModeChanged { .. })));
        let d = down(&mut e, KeyId::W);
        assert!(d.suppress);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingSaved {
                key: KeyId::W,
                target: cil,
                moved_from: Some(t0(Action::LeftStick(StickDir::Up)))
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
        let _ = e.start_binding(t0(Action::LeftTrigger), T0);
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
        let _ = e.start_binding(t0(Action::LeftTrigger), T0);
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
        let _ = e.start_binding(t0(Action::LeftTrigger), T0);
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
        let _ = e.start_binding(prvni, T0);
        let pozde = T0 + 5_000;
        let d = e.start_binding(druhe, pozde);
        assert_eq!(
            d.ui,
            Some(UiEvent::ModeChanged {
                mode: Mode::Binding {
                    target: druhe,
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
                target: druhe,
                moved_from: None
            })
        );
        assert_eq!(e.mapping().target(KeyId::NUMPAD_8), Some(druhe));
        assert_eq!(e.mapping().keys_for(prvni).count(), 1, "první beze změny");
        assert_eq!(e.mode(), Mode::Gamepad, "příznak návratu přežil restart");
    }

    #[test]
    fn prirazeni_presune_klavesu_na_jiny_ovladac() {
        let mut e = hraji_dva();
        let cil = PadAction::new(P1, Action::Button(PadButton::Y));
        let _ = e.start_binding(cil, T0);
        let d = down(&mut e, KeyId::SPACE);
        assert_eq!(
            d.ui,
            Some(UiEvent::BindingSaved {
                key: KeyId::SPACE,
                target: cil,
                moved_from: Some(t0(Action::Button(PadButton::A)))
            })
        );
        assert!(up(&mut e, KeyId::SPACE).suppress);
        // Mezerník teď hraje za druhý ovladač.
        let d = down(&mut e, KeyId::SPACE);
        assert_eq!(pad0(&d), None);
        assert!(d.pads.get(P1).unwrap().is_pressed(PadButton::Y));
        assert!(e.pad_state(P0).is_neutral());
    }
}

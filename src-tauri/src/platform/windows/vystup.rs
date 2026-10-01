//! Kam hook předává rozhodnutí enginu (Fáze 4): stavy ovladačů do
//! slotů jejich pad vláken, režim a příčinu jeho změny (Fáze 4b: ikona
//! a zvuk) pro okno.
//!
//! Volá se z callbacku hooku — proto jen atomiky a události Windows,
//! žádný zámek, alokace ani logování (princip 3). Okno se o změně
//! režimu dozví přes [`Budik`]: probudí vlákno, které teprve vydá
//! Tauri událost (z callbacku `emit` nikdy).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use keypad_core::{
    BindingCancel, Decision, DisabledReason, ForceReason, Mode, ModeCause, PadId, UiEvent, MAX_PADS,
};
use serde::Serialize;

use super::hook::{Udalost, Vystup};
use super::slot::{Budik, StavSlot};

/// Režim, jak ho ukazuje okno.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rezim {
    /// Žádný ovladač není zapnutý — klávesnice patří Windows.
    Disabled,
    /// Zachytávání pozastavené (zkratka) — klávesy jdou do Windows,
    /// zapnuté ovladače stojí v neutrálu.
    Paused,
    /// Zachytávání běží — klávesy zapnutých ovladačů ovládají je.
    Capturing,
    /// Přiřazuje se klávesa.
    Binding,
    /// Ovladač je zapnutý, ale hook nejde dostat do systému — klávesy
    /// jdou do Windows. Okno nesmí tvrdit, že ovladač ovládají.
    NoHook,
}

/// Režim pro okno s pořadovým číslem změny: odpověď příkazu `rezim`
/// a událost se můžou předběhnout — okno si nechá novější.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RezimInfo {
    pub rezim: Rezim,
    pub seq: u64,
}

impl Rezim {
    pub fn z(m: Mode) -> Rezim {
        match m {
            Mode::Disabled { .. } => Rezim::Disabled,
            Mode::Keyboard => Rezim::Paused,
            Mode::Gamepad => Rezim::Capturing,
            Mode::Binding { .. } => Rezim::Binding,
        }
    }

    fn kod(self) -> u64 {
        match self {
            Rezim::Disabled => 0,
            Rezim::Paused => 1,
            Rezim::Capturing => 2,
            Rezim::Binding => 3,
            Rezim::NoHook => 4,
        }
    }

    fn z_kodu(k: u64) -> Rezim {
        match k {
            1 => Rezim::Paused,
            2 => Rezim::Capturing,
            3 => Rezim::Binding,
            4 => Rezim::NoHook,
            _ => Rezim::Disabled,
        }
    }
}

/// Proč se režim naposledy změnil (Fáze 4b). Podle ní vlákno okna
/// vybere zvuk: pozastavení zkratkou nebo pojistkou pípne, vypnutí
/// přepínačem ne (Windows při odpojení ovladače hrají svůj zvuk), výpadek
/// posledního ovladače chybou ano.
///
/// Odvozuje se z oznámení enginu (`Decision::ui`), které změnu režimu
/// provází; okno ho samo nedostane včas (nese ho jen callback).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pricina {
    /// Beze zprávy od enginu: hook (ne)jde nainstalovat, konec vlákna.
    Nic,
    /// Zkratka přepnutí (Scroll Lock).
    Zkratka,
    /// Příkaz z okna nebo z nabídky ikony.
    Okno,
    /// Ovladač se připojil, nebo ho vypnul uživatel (přepínač, spánek,
    /// instalátor ViGEmBus).
    Ovladac,
    /// Poslední připravený ovladač vypadl s chybou (`Disabled { PadError }`).
    /// Pozná se podle důvodu přechodu, ne podle stavu padů: jiný ovladač
    /// může být v chybě dávno (chyba jednoho z hrajících je tichá, OQ 45)
    /// a vypnutí posledního přepínačem by pak pípalo.
    OvladacChyba,
    /// Konec přiřazování klávesy (uloženo, Esc, časový limit, okno).
    Prirazovani,
    /// Pojistka (watchdog, zamčení, UAC, spánek…).
    Vynuceno(ForceReason),
}

impl Pricina {
    /// Příčina z oznámení, které změnu režimu provází. Oznámení bez
    /// změny režimu (odmítnutá klávesa) sem nedojdou — příčina se
    /// zapisuje jen se změnou.
    fn z_udalosti(ui: Option<UiEvent>) -> Pricina {
        match ui {
            Some(UiEvent::ModeChanged { mode, cause }) => match cause {
                ModeCause::Hotkey => Pricina::Zkratka,
                ModeCause::Gui => Pricina::Okno,
                ModeCause::PadStatus
                    if matches!(
                        mode,
                        Mode::Disabled {
                            reason: DisabledReason::PadError
                        }
                    ) =>
                {
                    Pricina::OvladacChyba
                }
                ModeCause::PadStatus => Pricina::Ovladac,
                ModeCause::Forced(r) => Pricina::Vynuceno(r),
            },
            Some(UiEvent::BindingSaved { .. }) => Pricina::Prirazovani,
            Some(UiEvent::BindingCancelled { reason }) => match reason {
                BindingCancel::Escape | BindingCancel::Timeout | BindingCancel::Gui => {
                    Pricina::Prirazovani
                }
                BindingCancel::Forced(r) => Pricina::Vynuceno(r),
                // Engine to od Fáze 4 nevydává; kdyby ano, zrušil ho výpadek
                // ovladače.
                BindingCancel::PadStatus => Pricina::Ovladac,
            },
            Some(UiEvent::BindingRejected { .. } | UiEvent::ToggleRejected { .. }) | None => {
                Pricina::Nic
            }
        }
    }

    /// Kód do 8 bitů atomiku `stav`.
    fn kod(self) -> u64 {
        match self {
            Pricina::Nic => 0,
            Pricina::Zkratka => 1,
            Pricina::Okno => 2,
            Pricina::Ovladac => 3,
            Pricina::Prirazovani => 4,
            Pricina::OvladacChyba => 5,
            Pricina::Vynuceno(r) => {
                8 + match r {
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
    }

    /// Opak [`Pricina::kod`]; neznámý kód = `Nic`.
    fn z_kodu(k: u64) -> Pricina {
        let r = match k {
            1 => return Pricina::Zkratka,
            2 => return Pricina::Okno,
            3 => return Pricina::Ovladac,
            4 => return Pricina::Prirazovani,
            5 => return Pricina::OvladacChyba,
            8 => ForceReason::Watchdog,
            9 => ForceReason::PadError,
            10 => ForceReason::SessionLock,
            11 => ForceReason::DesktopSwitch,
            12 => ForceReason::Suspend,
            13 => ForceReason::HookPanic,
            14 => ForceReason::HookReinstalled,
            15 => ForceReason::MappingChanged,
            16 => ForceReason::Shutdown,
            _ => return Pricina::Nic,
        };
        Pricina::Vynuceno(r)
    }
}

// Bity atomiku `stav`:
//   0–2 režim enginu | 3 hook nejde | 4–11 příčina poslední změny |
//   12–19 cíl přiřazování (Fáze 6, zatím 0) | 20–63 číslo změny.
// Číslo změny má 44 bitů — přeteklo by po 17 bilionech změn.
const REZIM: u64 = 0b111;
const CHYBA: u64 = 1 << 3;
const PRICINA_POSUN: u32 = 4;
const PRICINA: u64 = 0xFF << PRICINA_POSUN;
const SEQ_POSUN: u32 = 20;
/// Část, jejíž změna zvyšuje číslo změny (příčina ji jen provází).
const OBSAH: u64 = REZIM | CHYBA;

pub struct HookVystup {
    sloty: [Arc<StavSlot>; MAX_PADS],
    /// Režim, chyba hooku, příčina a číslo změny v JEDNOM atomiku — okno
    /// tak nikdy nedostane nový režim se starým číslem nebo cizí
    /// příčinou. Zapisuje jen hook vlákno (callback i smyčka), proto
    /// stačí načíst a uložit.
    stav: AtomicU64,
    budik: Arc<Budik>,
}

impl HookVystup {
    /// `budik` probudí vlákno, které změnu režimu ohlásí oknu.
    pub fn new(sloty: [Arc<StavSlot>; MAX_PADS], budik: Arc<Budik>) -> HookVystup {
        HookVystup {
            sloty,
            stav: AtomicU64::new(Rezim::Disabled.kod()),
            budik,
        }
    }

    /// Režim s číslem změny pro okno. Chyba hooku přebíjí zapnuté stavy
    /// (bez hooku se nic nezachytává); bez ovladače je to prostě
    /// `Disabled`.
    pub fn info(&self) -> RezimInfo {
        self.info_s_pricinou().0
    }

    /// Režim a příčina jeho poslední změny — z jednoho načtení atomiku,
    /// takže k sobě vždy patří.
    pub fn info_s_pricinou(&self) -> (RezimInfo, Pricina) {
        let s = self.stav.load(Ordering::Acquire);
        let rezim = match Rezim::z_kodu(s & REZIM) {
            Rezim::Disabled => Rezim::Disabled,
            _ if s & CHYBA != 0 => Rezim::NoHook,
            r => r,
        };
        (
            RezimInfo {
                rezim,
                seq: s >> SEQ_POSUN,
            },
            Pricina::z_kodu((s & PRICINA) >> PRICINA_POSUN),
        )
    }

    /// Nový obsah atomiku; při změně zapíše příčinu, zvýší číslo
    /// a probudí okno. Beze změny se nesahá ani na příčinu — patří ke
    /// změně, kterou okno možná ještě nepřečetlo.
    fn zmen(&self, pricina: Pricina, f: impl FnOnce(u64) -> u64) {
        let stary = self.stav.load(Ordering::Acquire);
        let obsah = f(stary & OBSAH) & OBSAH;
        if obsah != stary & OBSAH {
            let seq = (stary >> SEQ_POSUN).wrapping_add(1);
            self.stav.store(
                seq << SEQ_POSUN | pricina.kod() << PRICINA_POSUN | obsah,
                Ordering::Release,
            );
            self.budik.probud();
        }
    }
}

impl Vystup for HookVystup {
    fn rozhodnuti(&self, _u: Option<&Udalost>, d: &Decision, rezim: Mode) {
        for (pad, stav) in d.pads.iter() {
            if let Some(slot) = self.sloty.get(pad.index()) {
                slot.zapis(stav);
            }
        }
        let kod = Rezim::z(rezim).kod();
        self.zmen(Pricina::z_udalosti(d.ui), |s| s & CHYBA | kod);
    }

    fn hook_chyba(&self, chyba: bool) {
        self.zmen(Pricina::Nic, |s| if chyba { s | CHYBA } else { s & !CHYBA });
    }

    fn tep_ms(&self, pad: PadId) -> Option<u64> {
        self.sloty.get(pad.index()).map(|s| s.tep())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keypad_core::{
        Action, Engine, KeyId, Mapping, PadAction, PadButton, PadState, PadUpdates, AXIS_MAX,
    };

    fn vystup() -> (HookVystup, [Arc<StavSlot>; MAX_PADS], Arc<Budik>) {
        let sloty: [Arc<StavSlot>; MAX_PADS] =
            std::array::from_fn(|_| Arc::new(StavSlot::new().unwrap()));
        let budik = Arc::new(Budik::new().unwrap());
        (
            HookVystup::new(sloty.clone(), Arc::clone(&budik)),
            sloty,
            budik,
        )
    }

    fn stav() -> PadState {
        PadState {
            thumb_ly: AXIS_MAX,
            ..PadState::NEUTRAL
        }
    }

    /// Stav jde jen do slotu svého ovladače; okno se budí jen změnou
    /// režimu, ne každou klávesou.
    #[test]
    fn stav_do_sveho_slotu_rezim_jen_pri_zmene() {
        let (v, sloty, budik) = vystup();
        let mut pads = PadUpdates::NONE;
        pads.set(PadId::new(2).unwrap(), stav());
        let d = Decision {
            pads,
            ..Decision::NONE
        };
        v.rozhodnuti(None, &d, Mode::Gamepad);
        assert_eq!(sloty[2].cti().unwrap().stav, stav());
        assert_eq!(sloty[2].cti().unwrap().cislo, 1);
        for i in [0, 1, 3] {
            assert_eq!(sloty[i].cti().unwrap().cislo, 0, "slot {i}");
        }
        assert_eq!(v.info().rezim, Rezim::Capturing);
        assert_eq!(v.info().seq, 1);
        assert!(budik.cekej(Some(0)));
        v.rozhodnuti(None, &d, Mode::Gamepad);
        assert!(!budik.cekej(Some(0)), "stejný režim okno nebudí");
        assert_eq!(v.info().seq, 1, "ani nemění číslo");
        v.rozhodnuti(
            None,
            &Decision::NONE,
            Mode::Disabled {
                reason: DisabledReason::PadError,
            },
        );
        assert_eq!(v.info().rezim, Rezim::Disabled);
        assert!(budik.cekej(Some(0)));
        assert_eq!(v.info().seq, 2);
    }

    /// Hook nejde: zapnutý ovladač se v okně nesmí tvářit, že klávesy
    /// ovládají ho. Bez ovladače je to jen „vypnuto". Každá změna zvýší
    /// číslo (okno zahodí starší odpověď).
    #[test]
    fn chyba_hooku_je_videt_a_cisluje_se() {
        let (v, _sloty, budik) = vystup();
        v.hook_chyba(true);
        assert_eq!(v.info().rezim, Rezim::Disabled, "bez ovladače nic nechybí");
        assert!(budik.cekej(Some(0)));
        v.rozhodnuti(None, &Decision::NONE, Mode::Keyboard);
        assert_eq!(v.info().rezim, Rezim::NoHook);
        v.rozhodnuti(None, &Decision::NONE, Mode::Gamepad);
        assert_eq!(v.info().rezim, Rezim::NoHook);
        let pred = v.info().seq;
        v.hook_chyba(true);
        assert_eq!(v.info().seq, pred, "stejná chyba nic nemění");
        v.hook_chyba(false);
        assert_eq!(
            v.info(),
            RezimInfo {
                rezim: Rezim::Capturing,
                seq: pred + 1
            }
        );
    }

    /// Výstup běží v callbacku hooku — nesmí alokovat.
    #[test]
    fn vystup_nealokuje() {
        let (v, _sloty, _budik) = vystup();
        let mut pads = PadUpdates::NONE;
        for p in PadId::ALL {
            pads.set(p, stav());
        }
        let d = Decision {
            suppress: true,
            pads,
            ui: None,
        };
        let pred = crate::testy_alokace::pocet();
        for m in [Mode::Gamepad, Mode::Keyboard, Mode::Gamepad] {
            v.rozhodnuti(None, &d, m);
        }
        assert_eq!(crate::testy_alokace::pocet(), pred);
    }

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

    fn vsechny_priciny() -> Vec<Pricina> {
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

    /// Kód příčiny se vejde do svých 8 bitů, je jedinečný a vrátí se
    /// zpět; neznámý kód je `Nic`.
    #[test]
    fn kod_priciny_tam_a_zpet() {
        let vsechny = vsechny_priciny();
        for p in &vsechny {
            assert!(p.kod() <= 0xFF, "{p:?}");
            assert_eq!(Pricina::z_kodu(p.kod()), *p);
        }
        let mut kody: Vec<u64> = vsechny.iter().map(|p| p.kod()).collect();
        kody.sort_unstable();
        kody.dedup();
        assert_eq!(kody.len(), vsechny.len(), "kódy se nesmí překrývat");
        for k in [6, 7, 17, 0xFF] {
            assert_eq!(Pricina::z_kodu(k), Pricina::Nic);
        }
    }

    /// Každá změna režimu nese příčinu podle oznámení enginu (tabulka
    /// „Jak HookVystup určí příčinu“) a okno ji přečte spolu s režimem.
    #[test]
    fn pricina_tam_a_zpet_pres_atomik() {
        let (v, _sloty, _budik) = vystup();
        let mut pripady: Vec<(UiEvent, Pricina)> = Vec::new();
        for (cause, p) in [
            (ModeCause::Hotkey, Pricina::Zkratka),
            (ModeCause::Gui, Pricina::Okno),
            (ModeCause::PadStatus, Pricina::Ovladac),
        ]
        .into_iter()
        .chain(
            VYNUCENI
                .iter()
                .map(|&r| (ModeCause::Forced(r), Pricina::Vynuceno(r))),
        ) {
            pripady.push((
                UiEvent::ModeChanged {
                    mode: Mode::Keyboard,
                    cause,
                },
                p,
            ));
        }
        for (reason, p) in [
            (BindingCancel::Escape, Pricina::Prirazovani),
            (BindingCancel::Timeout, Pricina::Prirazovani),
            (BindingCancel::Gui, Pricina::Prirazovani),
            (BindingCancel::PadStatus, Pricina::Ovladac),
        ]
        .into_iter()
        .chain(
            VYNUCENI
                .iter()
                .map(|&r| (BindingCancel::Forced(r), Pricina::Vynuceno(r))),
        ) {
            pripady.push((UiEvent::BindingCancelled { reason }, p));
        }
        let a = PadAction::first(Action::Button(PadButton::A));
        pripady.push((
            UiEvent::BindingSaved {
                key: KeyId::W,
                target: a,
                moved_from: None,
            },
            Pricina::Prirazovani,
        ));

        // Režimy se střídají, ať je každé rozhodnutí změnou.
        for (i, (ui, ocekavano)) in pripady.into_iter().enumerate() {
            let m = if i % 2 == 0 {
                Mode::Gamepad
            } else {
                Mode::Keyboard
            };
            let d = Decision {
                ui: Some(ui),
                ..Decision::NONE
            };
            let seq = v.info().seq;
            v.rozhodnuti(None, &d, m);
            let (r, p) = v.info_s_pricinou();
            assert_eq!((r.rezim, r.seq), (Rezim::z(m), seq + 1), "{ui:?}");
            assert_eq!(p, ocekavano, "{ui:?}");
        }
    }

    /// Bez změny režimu se příčina nepřepíše — okno by jinak k pozastavení
    /// zkratkou dostalo příčinu pozdějšího odmítnutí nebo klávesy.
    #[test]
    fn pricina_se_bez_zmeny_rezimu_nemeni() {
        let (v, _sloty, _budik) = vystup();
        let zkratka = Decision {
            ui: Some(UiEvent::ModeChanged {
                mode: Mode::Gamepad,
                cause: ModeCause::Hotkey,
            }),
            ..Decision::NONE
        };
        v.rozhodnuti(None, &zkratka, Mode::Gamepad);
        let (r, p) = v.info_s_pricinou();
        assert_eq!((r.rezim, p), (Rezim::Capturing, Pricina::Zkratka));
        let odmitnuti = Decision {
            ui: Some(UiEvent::ToggleRejected {
                reason: keypad_core::ToggleReject::Binding,
            }),
            ..Decision::NONE
        };
        for d in [Decision::NONE, odmitnuti] {
            v.rozhodnuti(None, &d, Mode::Gamepad);
            assert_eq!(v.info_s_pricinou(), (r, Pricina::Zkratka));
        }
        // Stejná chyba hooku taky nic nemění; nová chyba má příčinu Nic.
        v.hook_chyba(false);
        assert_eq!(v.info_s_pricinou(), (r, Pricina::Zkratka));
        v.hook_chyba(true);
        let (r2, p2) = v.info_s_pricinou();
        assert_eq!(
            (r2.rezim, r2.seq, p2),
            (Rezim::NoHook, r.seq + 1, Pricina::Nic)
        );
    }

    /// Chyba ovladače se pozná podle důvodu přechodu. Ovladač 2 visí
    /// v chybě (za hry tiše, OQ 45) a vypnutí ovladače 1 přepínačem je
    /// pořád jen „ovladač“ — pípnout nesmí. Výpadek posledního chybou ano.
    #[test]
    fn chyba_ovladace_podle_duvodu_prechodu() {
        let (v, _sloty, _budik) = vystup();
        let (p1, p2) = (PadId::new(0).unwrap(), PadId::new(1).unwrap());
        let mut e = Engine::new(Mapping::default());
        let krok = |e: &mut Engine, f: &dyn Fn(&mut Engine) -> Decision| {
            let d = f(e);
            v.rozhodnuti(None, &d, e.mode());
            v.info_s_pricinou()
        };
        krok(&mut e, &|e| e.enable(p1));
        krok(&mut e, &|e| e.enable(p2));
        let (r, _) = krok(&mut e, &|e| e.capture(0));
        assert_eq!(r.rezim, Rezim::Capturing);
        let (r2, p) = krok(&mut e, &|e| e.disable(p2, DisabledReason::PadError));
        assert_eq!(
            (r2, p),
            (r, Pricina::Okno),
            "chyba jednoho z hrajících režim nemění"
        );
        let (r, p) = krok(&mut e, &|e| e.disable(p1, DisabledReason::PadNotConnected));
        assert_eq!((r.rezim, p), (Rezim::Disabled, Pricina::Ovladac));

        let mut e = Engine::new(Mapping::default());
        krok(&mut e, &|e| e.enable(p1));
        krok(&mut e, &|e| e.capture(0));
        let (r, p) = krok(&mut e, &|e| e.disable(p1, DisabledReason::PadError));
        assert_eq!((r.rezim, p), (Rezim::Disabled, Pricina::OvladacChyba));
    }

    /// Příčina se v callbacku počítá bez alokace i s oznámením.
    #[test]
    fn vystup_s_pricinou_nealokuje() {
        let (v, _sloty, _budik) = vystup();
        let d = |mode, cause| Decision {
            suppress: true,
            pads: PadUpdates::NONE,
            ui: Some(UiEvent::ModeChanged { mode, cause }),
        };
        let pred = crate::testy_alokace::pocet();
        for (m, c) in [
            (Mode::Gamepad, ModeCause::Hotkey),
            (Mode::Keyboard, ModeCause::Forced(ForceReason::Watchdog)),
            (Mode::Gamepad, ModeCause::Gui),
        ] {
            v.rozhodnuti(None, &d(m, c), m);
            let _ = v.info_s_pricinou();
        }
        assert_eq!(crate::testy_alokace::pocet(), pred);
    }
}

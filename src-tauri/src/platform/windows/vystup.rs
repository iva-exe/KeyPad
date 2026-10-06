//! Kam hook předává rozhodnutí enginu (Fáze 4): stavy ovladačů do
//! slotů jejich pad vláken; režim a příčinu jeho změny (Fáze 4b: ikona
//! a zvuk); cíl přiřazování, poslední oznámení, revizi mapování a živý
//! stav vstupů (Fáze 6) pro okno.
//!
//! Volá se z callbacku hooku — proto jen atomiky a události Windows,
//! žádný zámek, alokace ani logování (princip 3). Okno se o změně
//! dozví přes [`Budik`]: probudí vlákno, které teprve vydá Tauri
//! událost (z callbacku `emit` nikdy).
//!
//! Jediný zámek je schránka snímku mapování ([`HookVystup::vezmi_mapovani`]):
//! plní ji jen smyčka hook vlákna na povel `Zverejni`, nikdy callback
//! (snímek je klon celé tabulky). Callback publikuje jen revizi —
//! číslo, podle kterého si vlákno okna o snímek řekne.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use keypad_core::{
    Action, ActionSet, BindingCancel, Decision, DisabledReason, ForceReason, LiveInputs, Mapping,
    Mode, ModeCause, PadId, UiEvent, MAX_PADS,
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

/// Vstup konkrétního ovladače pro okno (cíl přiřazování, odkud se
/// klávesa přesunula). Ovladače od 0 jako všude ve smlouvě s oknem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CilInfo {
    pub pad: u8,
    /// Stabilní kód vstupu ([`Action::code`]).
    pub vstup: &'static str,
}

impl CilInfo {
    pub fn z(t: keypad_core::PadAction) -> CilInfo {
        CilInfo {
            pad: t.pad.index() as u8,
            vstup: t.action.code(),
        }
    }
}

/// Režim pro okno s pořadovým číslem změny: odpověď příkazu `rezim`
/// a událost se můžou předběhnout — okno si nechá novější.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RezimInfo {
    pub rezim: Rezim,
    pub seq: u64,
    /// Co se právě přiřazuje (jen v `binding`).
    pub cil: Option<CilInfo>,
    /// Windows nedovolily hook klávesnice. I bez zapnutého ovladače:
    /// okno v popředí pak neukáže živé klávesy a musí říct proč.
    pub hook_chyba: bool,
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
            Pricina::Vynuceno(r) => 8 + r.index() as u64,
        }
    }

    /// Opak [`Pricina::kod`]; neznámý kód = `Nic`.
    fn z_kodu(k: u64) -> Pricina {
        match k {
            1 => Pricina::Zkratka,
            2 => Pricina::Okno,
            3 => Pricina::Ovladac,
            4 => Pricina::Prirazovani,
            5 => Pricina::OvladacChyba,
            _ => k
                .checked_sub(8)
                .and_then(|i| ForceReason::ALL.get(i as usize))
                .map_or(Pricina::Nic, |&r| Pricina::Vynuceno(r)),
        }
    }
}

// Bity atomiku `stav`:
//   0–2 režim enginu | 3 hook nejde | 4–11 příčina poslední změny |
//   12–19 cíl přiřazování (12 platný | 13–14 ovladač | 15–19
//   Action::index) | 20–63 číslo změny.
// Číslo změny má 44 bitů — přeteklo by po 17 bilionech změn.
const REZIM: u64 = 0b111;
const CHYBA: u64 = 1 << 3;
const PRICINA_POSUN: u32 = 4;
const PRICINA: u64 = 0xFF << PRICINA_POSUN;
const CIL_POSUN: u32 = 12;
const CIL: u64 = 0xFF << CIL_POSUN;
const SEQ_POSUN: u32 = 20;
/// Část, jejíž změna zvyšuje číslo změny (příčina ji jen provází). Cíl
/// patří k ní: nové přiřazování jiné čepičky je změna, kterou okno musí
/// vidět, i když režim zůstal „přiřazuji".
const OBSAH: u64 = REZIM | CHYBA | CIL;

/// Cíl přiřazování do bitů 12–19 atomiku `stav`; mimo přiřazování 0.
fn kod_cile(m: Mode) -> u64 {
    match m {
        Mode::Binding { target, .. } => {
            (1 | (target.pad.index() as u64) << 1 | (target.action.index() as u64) << 3)
                << CIL_POSUN
        }
        _ => 0,
    }
}

fn cil_z_kodu(s: u64) -> Option<CilInfo> {
    let c = (s & CIL) >> CIL_POSUN;
    if c & 1 == 0 {
        return None;
    }
    let pad = PadId::new((c >> 1 & 0b11) as usize)?;
    let action = Action::from_index((c >> 3) as usize)?;
    Some(CilInfo::z(keypad_core::PadAction::new(pad, action)))
}

// Schránka oznámení: 0–47 `UiEvent::pack` | 48–63 pořadí (přetéká).
// Platí jen poslední oznámení — přepsané okno pozná podle mezery
// v pořadí a načte si stav znovu (režim a mapování jsou zdroj pravdy,
// oznámení je jen „co se stalo a proč").
const OZNAMENI_SEQ_POSUN: u32 = 48;
/// Obsah schránky oznámení bez pořadí.
pub const OZNAMENI_OBSAH: u64 = (1 << OZNAMENI_SEQ_POSUN) - 1;

pub struct HookVystup {
    sloty: [Arc<StavSlot>; MAX_PADS],
    /// Režim, chyba hooku, příčina, cíl a číslo změny v JEDNOM atomiku —
    /// okno tak nikdy nedostane nový režim se starým číslem nebo cizí
    /// příčinou. Zapisuje jen hook vlákno (callback i smyčka), proto
    /// stačí načíst a uložit.
    stav: AtomicU64,
    /// Poslední oznámení (viz `OZNAMENI_SEQ_POSUN`).
    oznameni: AtomicU64,
    /// Živý stav vstupů ovladačů (`LiveInputs::bits`) — jen s viditelným
    /// oknem, jinak ho hook nepočítá.
    zive: [AtomicU32; MAX_PADS],
    /// Revize mapování enginu.
    revize: AtomicU64,
    /// Snímek mapování s revizí. Plní JEN smyčka hook vlákna (povel
    /// `Zverejni`), bere JEN vlákno okna — callback na zámek nesahá.
    mapovani: Mutex<Option<(u64, Arc<Mapping>)>>,
    budik: Arc<Budik>,
}

impl HookVystup {
    /// `budik` probudí vlákno, které změny ohlásí oknu.
    pub fn new(sloty: [Arc<StavSlot>; MAX_PADS], budik: Arc<Budik>) -> HookVystup {
        HookVystup {
            sloty,
            stav: AtomicU64::new(Rezim::Disabled.kod()),
            oznameni: AtomicU64::new(0),
            zive: std::array::from_fn(|_| AtomicU32::new(0)),
            revize: AtomicU64::new(0),
            mapovani: Mutex::new(None),
            budik,
        }
    }

    /// Režim s číslem změny pro okno. Chyba hooku přebíjí zapnuté stavy
    /// (bez hooku se nic nezachytává); bez ovladače je to prostě
    /// `Disabled` a přiřazování zůstává přiřazováním (smyčka ho bez hooku
    /// hned zruší) — chybu pak nese jen `hook_chyba`.
    pub fn info(&self) -> RezimInfo {
        self.info_s_pricinou().0
    }

    /// Režim a příčina jeho poslední změny — z jednoho načtení atomiku,
    /// takže k sobě vždy patří.
    pub fn info_s_pricinou(&self) -> (RezimInfo, Pricina) {
        let s = self.stav.load(Ordering::Acquire);
        let chyba = s & CHYBA != 0;
        let rezim = match Rezim::z_kodu(s & REZIM) {
            r @ (Rezim::Disabled | Rezim::Binding) => r,
            _ if chyba => Rezim::NoHook,
            r => r,
        };
        (
            RezimInfo {
                rezim,
                seq: s >> SEQ_POSUN,
                cil: cil_z_kodu(s),
                hook_chyba: chyba,
            },
            Pricina::z_kodu((s & PRICINA) >> PRICINA_POSUN),
        )
    }

    /// Schránka oznámení: pořadí v bitech 48–63, obsah
    /// ([`OZNAMENI_OBSAH`]) je `UiEvent::pack`.
    pub fn oznameni(&self) -> u64 {
        self.oznameni.load(Ordering::Acquire)
    }

    /// Fyzicky držené vstupy ovladačů (jak je naposledy poslal hook).
    pub fn zive_vstupy(&self) -> [LiveInputs; MAX_PADS] {
        std::array::from_fn(|i| LiveInputs::from_bits(self.zive[i].load(Ordering::Acquire)))
    }

    /// Vstupy, které hra opravdu dostává: stav ve slotu padu (ten pad
    /// vlákno posílá do ViGEm).
    pub fn hra(&self) -> [ActionSet; MAX_PADS] {
        std::array::from_fn(|i| {
            self.sloty[i]
                .cti()
                .map_or(ActionSet::EMPTY, |p| p.stav.active_inputs())
        })
    }

    /// Revize mapování enginu.
    pub fn revize_mapovani(&self) -> u64 {
        self.revize.load(Ordering::Acquire)
    }

    /// Vezme snímek mapování (každý jen jednou).
    pub fn vezmi_mapovani(&self) -> Option<(u64, Arc<Mapping>)> {
        self.mapovani
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
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

    /// Zapíše oznámení s dalším pořadím a probudí okno.
    fn oznam(&self, obsah: u64) {
        let stary = self.oznameni.load(Ordering::Acquire);
        let seq = ((stary >> OZNAMENI_SEQ_POSUN) as u16).wrapping_add(1);
        self.oznameni.store(
            u64::from(seq) << OZNAMENI_SEQ_POSUN | obsah & OZNAMENI_OBSAH,
            Ordering::Release,
        );
        self.budik.probud();
    }
}

impl Vystup for HookVystup {
    fn rozhodnuti(&self, _u: Option<&Udalost>, d: &Decision, rezim: Mode) {
        for (pad, stav) in d.pads.iter() {
            if let Some(slot) = self.sloty.get(pad.index()) {
                slot.zapis(stav);
            }
        }
        let kod = Rezim::z(rezim).kod() | kod_cile(rezim);
        self.zmen(Pricina::z_udalosti(d.ui), |s| s & CHYBA | kod);
        // Až po režimu: vlákno okna čte oznámení dřív než režim, takže
        // kdo vidí nové oznámení, vidí i režim, který k němu patří, a
        // okno dostane `rezim` vždy před `oznameni` (spec B4).
        if let Some(obsah) = d.ui.and_then(|u| u.pack()) {
            self.oznam(obsah);
        }
    }

    fn hook_chyba(&self, chyba: bool) {
        self.zmen(Pricina::Nic, |s| if chyba { s | CHYBA } else { s & !CHYBA });
    }

    fn tep_ms(&self, pad: PadId) -> Option<u64> {
        self.sloty.get(pad.index()).map(|s| s.tep())
    }

    fn zive(&self, z: &[LiveInputs; MAX_PADS]) {
        let mut zmena = false;
        for (a, l) in self.zive.iter().zip(z) {
            if a.load(Ordering::Relaxed) != l.bits() {
                a.store(l.bits(), Ordering::Release);
                zmena = true;
            }
        }
        // Budí se jen změnou — autorepeat držené klávesy okno nebudí.
        if zmena {
            self.budik.probud();
        }
    }

    fn revize(&self, rev: u64) {
        if self.revize.load(Ordering::Relaxed) != rev {
            self.revize.store(rev, Ordering::Release);
            self.budik.probud();
        }
    }

    fn mapovani(&self, rev: u64, m: &Mapping) {
        let snimek = Some((rev, Arc::new(m.clone())));
        *self.mapovani.lock().unwrap_or_else(PoisonError::into_inner) = snimek;
        self.budik.probud();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keypad_core::{
        BindKind, BindingReject, Engine, KeyId, PadAction, PadButton, PadState, PadUpdates,
        StickDir, ToggleReject, AXIS_MAX,
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
    /// ovládají ho. Bez ovladače je to jen „vypnuto" a chybu nese
    /// `hook_chyba` (okno v popředí neukáže živé klávesy). Každá změna
    /// zvýší číslo (okno zahodí starší odpověď).
    #[test]
    fn chyba_hooku_je_videt_a_cisluje_se() {
        let (v, _sloty, budik) = vystup();
        v.hook_chyba(true);
        assert_eq!(v.info().rezim, Rezim::Disabled, "bez ovladače nic nechybí");
        assert!(v.info().hook_chyba);
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
                seq: pred + 1,
                cil: None,
                hook_chyba: false,
            }
        );
    }

    /// Cíl přiřazování jde oknu v atomiku `stav` spolu s režimem; nový
    /// cíl za běžícího přiřazování je změna (nové číslo), konec
    /// přiřazování cíl smaže.
    #[test]
    fn cil_prirazovani_v_atomiku_stav() {
        let (v, _sloty, budik) = vystup();
        let binding = |pad: usize, a: Action| Mode::Binding {
            target: PadAction::new(PadId::new(pad).unwrap(), a),
            started_at_ms: 5,
        };
        v.rozhodnuti(
            None,
            &Decision::NONE,
            binding(1, Action::Button(PadButton::A)),
        );
        let i = v.info();
        assert_eq!(i.rezim, Rezim::Binding);
        assert_eq!(i.cil, Some(CilInfo { pad: 1, vstup: "a" }));
        assert!(budik.cekej(Some(0)));
        // Jiný cíl = nové číslo, i když režim zůstává.
        v.rozhodnuti(None, &Decision::NONE, binding(3, Action::RightTrigger));
        assert_eq!(v.info().seq, i.seq + 1);
        assert_eq!(
            v.info().cil,
            Some(CilInfo {
                pad: 3,
                vstup: "rt"
            })
        );
        // Všechny cíle tam a zpět.
        for pad in PadId::ALL {
            for a in Action::ALL {
                v.rozhodnuti(None, &Decision::NONE, binding(pad.index(), a));
                assert_eq!(
                    v.info().cil,
                    Some(CilInfo::z(PadAction::new(pad, a))),
                    "{pad:?} {a:?}"
                );
            }
        }
        v.rozhodnuti(None, &Decision::NONE, Mode::Keyboard);
        assert_eq!(v.info().cil, None);
        // Přiřazování s nefunkčním hookem zůstává přiřazováním (smyčka ho
        // hned zruší) a chybu nese `hook_chyba`.
        v.hook_chyba(true);
        v.rozhodnuti(
            None,
            &Decision::NONE,
            binding(0, Action::Button(PadButton::B)),
        );
        let i = v.info();
        assert_eq!((i.rezim, i.hook_chyba), (Rezim::Binding, true));
    }

    /// Každé oznámení (kromě změny režimu, tu nese `stav`) jde do
    /// schránky s dalším pořadím a vrátí se celé; pořadí přetéká.
    #[test]
    fn oznameni_tam_a_zpet() {
        let (v, _sloty, budik) = vystup();
        let a = PadAction::new(PadId::new(1).unwrap(), Action::LeftStick(StickDir::Up));
        let mut udalosti = vec![
            UiEvent::BindingSaved {
                key: KeyId::W,
                target: a,
                moved_from: None,
            },
            UiEvent::BindingSaved {
                key: KeyId::ext(0x48),
                target: a,
                moved_from: Some(PadAction::first(Action::Button(PadButton::Y))),
            },
            UiEvent::ToggleRejected {
                reason: ToggleReject::Binding,
            },
        ];
        for reason in [
            BindingReject::ToggleKey,
            BindingReject::Unmappable,
            BindingReject::Reserved,
        ] {
            udalosti.push(UiEvent::BindingRejected {
                key: KeyId {
                    scan: 0xFFFF,
                    extended: true,
                },
                reason,
            });
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
            udalosti.push(UiEvent::BindingCancelled { reason });
        }
        for r in [
            DisabledReason::ViGEmMissing,
            DisabledReason::PadNotConnected,
            DisabledReason::PadError,
        ] {
            udalosti.push(UiEvent::ToggleRejected {
                reason: ToggleReject::Disabled(r),
            });
        }
        assert_eq!(v.oznameni(), 0);
        for (i, u) in udalosti.iter().enumerate() {
            let d = Decision {
                ui: Some(*u),
                ..Decision::NONE
            };
            v.rozhodnuti(None, &d, Mode::Keyboard);
            let b = v.oznameni();
            assert_eq!(b >> 48, i as u64 + 1, "{u:?}");
            assert_eq!(UiEvent::unpack(b & OZNAMENI_OBSAH), Some(*u));
            assert!(budik.cekej(Some(0)));
        }
        // Změna režimu oznámení nepřepíše (nese ji `stav`).
        let pred = v.oznameni();
        let d = Decision {
            ui: Some(UiEvent::ModeChanged {
                mode: Mode::Gamepad,
                cause: ModeCause::Hotkey,
            }),
            ..Decision::NONE
        };
        v.rozhodnuti(None, &d, Mode::Gamepad);
        assert_eq!(v.oznameni(), pred);
        // Pořadí je 16 bitů a přeteče na 0.
        v.oznameni.store(0xFFFF << 48, Ordering::Release);
        let d = Decision {
            ui: Some(udalosti[0]),
            ..Decision::NONE
        };
        v.rozhodnuti(None, &d, Mode::Keyboard);
        assert_eq!(v.oznameni() >> 48, 0);
        assert_eq!(
            UiEvent::unpack(v.oznameni() & OZNAMENI_OBSAH),
            Some(udalosti[0])
        );
    }

    /// Živý stav se zapíše a okno probudí jen při změně (autorepeat ani
    /// klávesa jiného vstupu téhož stavu okno nebudí).
    #[test]
    fn zive_zapise_a_probudi_jen_pri_zmene() {
        let (v, _sloty, budik) = vystup();
        let mut set = ActionSet::EMPTY;
        set.insert(Action::LeftStick(StickDir::Up));
        let mut z = [LiveInputs::EMPTY; MAX_PADS];
        v.zive(&z);
        assert!(
            !budik.cekej(Some(0)),
            "prázdný stav na začátku = beze změny"
        );
        z[2] = LiveInputs::new(set, (0, 1), (0, 0));
        v.zive(&z);
        assert!(budik.cekej(Some(0)));
        assert_eq!(v.zive_vstupy(), z);
        v.zive(&z);
        assert!(!budik.cekej(Some(0)), "stejný stav okno nebudí");
        v.zive(&[LiveInputs::EMPTY; MAX_PADS]);
        assert!(budik.cekej(Some(0)));
        assert_eq!(v.zive_vstupy(), [LiveInputs::EMPTY; MAX_PADS]);
    }

    /// Revize budí jen změnou; snímek mapování jde do schránky a vlákno
    /// okna si ho vezme právě jednou (novější přepíše starší).
    #[test]
    fn revize_a_schranka_mapovani() {
        let (v, _sloty, budik) = vystup();
        v.revize(0);
        assert!(!budik.cekej(Some(0)));
        v.revize(3);
        assert!(budik.cekej(Some(0)));
        assert_eq!(v.revize_mapovani(), 3);
        assert!(v.vezmi_mapovani().is_none());
        let m = Mapping::default();
        let mut m2 = m.clone();
        m2.unbind(KeyId::W).unwrap();
        v.mapovani(3, &m);
        assert!(budik.cekej(Some(0)));
        v.mapovani(4, &m2);
        let (rev, snimek) = v.vezmi_mapovani().unwrap();
        assert_eq!((rev, &*snimek), (4, &m2));
        assert!(v.vezmi_mapovani().is_none(), "snímek se bere jen jednou");
    }

    /// Hra = stav ve slotu padu (to, co pad vlákno posílá do ViGEm).
    #[test]
    fn hra_ze_slotu_padu() {
        let (v, sloty, _budik) = vystup();
        assert_eq!(v.hra(), [ActionSet::EMPTY; MAX_PADS]);
        sloty[1].zapis(stav());
        let mut ocekavano = ActionSet::EMPTY;
        ocekavano.insert(Action::LeftStick(StickDir::Up));
        assert_eq!(v.hra()[1], ocekavano);
        assert_eq!(v.hra()[0], ActionSet::EMPTY);
    }

    /// Výstup běží v callbacku hooku — nesmí alokovat ani uvolňovat,
    /// ani se živým stavem, oznámením a revizí.
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
        let ulozeno = Decision {
            ui: Some(UiEvent::BindingSaved {
                key: KeyId::W,
                target: PadAction::first(Action::Button(PadButton::A)),
                moved_from: Some(PadAction::first(Action::Button(PadButton::B))),
            }),
            ..d
        };
        let mut set = ActionSet::EMPTY;
        set.insert(Action::Button(PadButton::A));
        let zive = [LiveInputs::new(set, (1, -1), (0, 1)); MAX_PADS];
        let binding = Mode::Binding {
            target: PadAction::first(Action::Button(PadButton::X)),
            started_at_ms: 0,
        };
        let pred = crate::testy_alokace::pocet();
        for m in [Mode::Gamepad, Mode::Keyboard, binding, Mode::Gamepad] {
            v.rozhodnuti(None, &d, m);
            v.rozhodnuti(None, &ulozeno, m);
            v.revize(7);
            v.zive(&zive);
            v.zive(&[LiveInputs::EMPTY; MAX_PADS]);
            v.hook_chyba(true);
            v.hook_chyba(false);
        }
        assert_eq!(crate::testy_alokace::pocet(), pred);
    }

    fn vsechny_priciny() -> Vec<Pricina> {
        let mut v = vec![
            Pricina::Nic,
            Pricina::Zkratka,
            Pricina::Okno,
            Pricina::Ovladac,
            Pricina::OvladacChyba,
            Pricina::Prirazovani,
        ];
        v.extend(ForceReason::ALL.map(Pricina::Vynuceno));
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
            ForceReason::ALL
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
            ForceReason::ALL
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
                reason: ToggleReject::Binding,
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

    /// Přiřazování z enginu: cíl i oznámení přijdou oknu přes atomiky,
    /// uložení nese příčinu „přiřazování“.
    #[test]
    fn prirazovani_z_enginu_pres_atomiky() {
        let (v, _sloty, _budik) = vystup();
        let mut e = Engine::new(Mapping::default());
        let cil = PadAction::first(Action::Button(PadButton::Y));
        let d = e.start_binding(cil, BindKind::Replace, 0);
        v.rozhodnuti(None, &d, e.mode());
        assert_eq!(v.info().cil, Some(CilInfo { pad: 0, vstup: "y" }));
        let d = e.on_key(KeyId::F, true, 1);
        v.rozhodnuti(None, &d, e.mode());
        let (r, p) = v.info_s_pricinou();
        assert_eq!(
            (r.rezim, r.cil, p),
            (Rezim::Disabled, None, Pricina::Prirazovani)
        );
        assert_eq!(
            UiEvent::unpack(v.oznameni() & OZNAMENI_OBSAH),
            Some(UiEvent::BindingSaved {
                key: KeyId::F,
                target: cil,
                moved_from: Some(PadAction::first(Action::Button(PadButton::X))),
            })
        );
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

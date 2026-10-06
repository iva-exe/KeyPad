//! KeyPad — čistá logika bez závislosti na Windows.
//!
//! - [`KeyId`] — fyzická klávesa (scan kód + E0),
//! - [`Action`] — co klávesa udělá na gamepadu, [`PadAction`] — na kterém
//!   z až [`MAX_PADS`] ovladačů ([`PadId`]),
//! - [`Mapping`] — vazby klávesa → akce ovladače + zkratka přepnutí, vždy
//!   platné; klávesa smí ovládat až [`MAX_TARGETS_PER_KEY`] vstupů
//!   ([`Targets`], sdílená klávesa — Fáze 7),
//! - [`Engine`] — stavový automat: událost klávesnice → [`Decision`]
//!   (potlačit? nové stavy ovladačů? oznámení pro okno?),
//! - [`PadState`] + [`compute_pad_state`] — stav Xbox ovladače jako čistá
//!   funkce držených kláves; [`PadUpdates`] — které ovladače poslat,
//! - [`LiveInputs`] / [`ActionSet`] — živý stav vstupů pro okno (jedno
//!   `u32`, ať projde atomikem z hook callbacku).
//!
//! Tenhle crate nesmí záviset na `windows`, `vigem-client` ani `tauri`
//! (princip 4) — hlídá to CI. Díky tomu jde celé rozhodování otestovat
//! bez hooku, ovladače i okna.

mod action;
mod engine;
mod key;
mod mapping;
mod pad_state;
mod targets;

pub use action::{
    Action, ActionSet, InvalidPadId, PadAction, PadButton, PadId, StickDir, MAX_PADS,
};
pub use engine::{
    BindKind, BindTarget, BindingCancel, BindingReject, Decision, DisabledReason, Engine,
    ForceReason, HeldKey, Mode, ModeCause, Owner, ToggleReject, UiEvent, BINDING_TIMEOUT_MS,
    STALE_KEY_MS,
};
pub use key::KeyId;
pub use mapping::{KeyConflict, Mapping, MappingError, Rebind};
pub use pad_state::{
    compute_pad_state, LiveInputs, PadState, PadUpdates, AXIS_DIAGONAL, AXIS_MAX, TRIGGER_MAX,
};
pub use targets::{Targets, TargetsIter, TooManyTargets, MAX_TARGETS_PER_KEY};

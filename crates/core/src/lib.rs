//! KeyPad — čistá logika bez závislosti na Windows.
//!
//! - [`KeyId`] — fyzická klávesa (scan kód + E0),
//! - [`Action`] — co klávesa udělá na gamepadu, [`PadAction`] — na kterém
//!   z až [`MAX_PADS`] ovladačů ([`PadId`]),
//! - [`Mapping`] — vazby klávesa → akce ovladače + zkratka přepnutí, vždy
//!   platné,
//! - [`Engine`] — stavový automat: událost klávesnice → [`Decision`]
//!   (potlačit? nové stavy ovladačů? oznámení pro okno?),
//! - [`PadState`] + [`compute_pad_state`] — stav Xbox ovladače jako čistá
//!   funkce držených kláves; [`PadUpdates`] — které ovladače poslat.
//!
//! Tenhle crate nesmí záviset na `windows`, `vigem-client` ani `tauri`
//! (princip 4) — hlídá to CI. Díky tomu jde celé rozhodování otestovat
//! bez hooku, ovladače i okna.

mod action;
mod engine;
mod key;
mod mapping;
mod pad_state;

pub use action::{Action, InvalidPadId, PadAction, PadButton, PadId, StickDir, MAX_PADS};
pub use engine::{
    BindingCancel, BindingReject, Decision, DisabledReason, Engine, ForceReason, HeldKey, Mode,
    ModeCause, Owner, ToggleReject, UiEvent, BINDING_TIMEOUT_MS, STALE_KEY_MS,
};
pub use key::KeyId;
pub use mapping::{Mapping, MappingError};
pub use pad_state::{
    compute_pad_state, PadState, PadUpdates, AXIS_DIAGONAL, AXIS_MAX, TRIGGER_MAX,
};

//! Win32 kód aplikace: hook klávesnice, virtuální pad (ViGEmBus),
//! uspání, konec relace Windows, ukončení z instalátoru, spuštění
//! instalátoru ViGEmBus a odkazů, zvuk pozastavení.

pub mod dll;
pub mod hook;
pub mod klavesy;
pub mod pad;
pub mod power;
pub mod relace;
pub mod shell;
pub mod slot;
pub mod ukonceni;
pub mod vigem;
pub mod vystup;
pub mod zvuk;

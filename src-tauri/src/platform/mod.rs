//! Kód závislý na Windows (Fáze 2+). KeyPad je jen pro Windows 10/11,
//! jiná platforma se neřeší — oddělení je kvůli přehlednosti: tady žije
//! všechno, co volá Win32 kvůli padu, hooku a systému, zbytek aplikace
//! je Tauri a čistá logika z `keypad_core`.

pub mod windows;

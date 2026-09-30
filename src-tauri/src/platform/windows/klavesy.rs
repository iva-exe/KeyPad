//! Názvy kláves pro okno (Fáze 3) — z rozložení klávesnice Windows.
//!
//! Engine zná klávesu jen jako scan kód (pozice), ale uživatel má vidět
//! to, co je na klávese napsané: na české QWERTZ je na pozici Y
//! americké klávesnice „Z". `GetKeyNameTextW` bere právě scan kód
//! a vrátí popisek podle rozložení — i česky („Mezerník").

// Názvy potřebuje až editor kláves (Fáze 6) a příklad `hook_selftest`.
#![allow(dead_code)]

use keypad_core::KeyId;
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyNameTextW;

/// Popisek klávesy podle rozložení volajícího vlákna.
///
/// Rozložení se bere z vlákna, které funkci volá (GUI vlákno má to, se
/// kterým aplikace startovala). Když Windows název neznají, vrátí se
/// krátké „#5B" — lepší než prázdné pole, a kódy víc neukazujeme.
pub fn nazev(k: KeyId) -> String {
    // lParam jako u WM_KEYDOWN: bity 16–23 scan kód, bit 24 prefix E0.
    let lparam = ((k.scan as i32 & 0xFF) << 16) | if k.extended { 1 << 24 } else { 0 };
    let mut buf = [0u16; 64];
    // SAFETY: buffer žije po celou dobu volání, délku funkce zná.
    let n = unsafe { GetKeyNameTextW(lparam, &mut buf) };
    match usize::try_from(n) {
        Ok(n) if n > 0 => String::from_utf16_lossy(&buf[..n.min(buf.len())]),
        _ => format!("#{:02X}", k.scan),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Písmena mají název na každém rozložení; rozšířená šipka se liší
    /// od osmičky na numerické klávesnici (stejný scan kód bez E0).
    #[test]
    fn zakladni_klavesy_maji_nazev() {
        for k in [
            KeyId::W,
            KeyId::A,
            KeyId::S,
            KeyId::D,
            KeyId::SPACE,
            KeyId::ENTER,
        ] {
            let n = nazev(k);
            assert!(!n.is_empty() && !n.starts_with('#'), "{k:?}: {n}");
        }
        assert_eq!(nazev(KeyId::W), "W");
        assert_ne!(nazev(KeyId::ARROW_UP), nazev(KeyId::NUMPAD_8));
    }

    #[test]
    fn neznama_klavesa_ma_kratky_zastupny_nazev() {
        // 0x00 žádný název nemá.
        assert_eq!(nazev(KeyId::new(0)), "#00");
    }
}

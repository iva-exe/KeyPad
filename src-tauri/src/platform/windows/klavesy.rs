//! Názvy kláves pro okno (Fáze 3) — z rozložení klávesnice Windows.
//!
//! Engine zná klávesu jen jako scan kód (pozice), ale uživatel má vidět
//! to, co je na klávese napsané: na české QWERTZ je na pozici Y
//! americké klávesnice „Z". `GetKeyNameTextW` bere právě scan kód
//! a vrátí popisek podle rozložení — i česky („Mezerník").

use keypad_core::KeyId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyNameTextW, MapVirtualKeyW, MAPVK_VSC_TO_VK_EX, VK_F1, VK_F13, VK_F24,
};

/// Popisek klávesy podle rozložení volajícího vlákna.
///
/// Rozložení se bere z vlákna, které funkci volá (GUI vlákno má to, se
/// kterým aplikace startovala). Když Windows název neznají, zkusí se
/// [`zalozni_nazev`]; teprve pak krátké „#5B" — lepší než prázdné pole,
/// a kódy víc neukazujeme.
pub fn nazev(k: KeyId) -> String {
    // lParam jako u WM_KEYDOWN: bity 16–23 scan kód, bit 24 prefix E0.
    let lparam = ((k.scan as i32 & 0xFF) << 16) | if k.extended { 1 << 24 } else { 0 };
    let mut buf = [0u16; 64];
    // SAFETY: buffer žije po celou dobu volání, délku funkce zná.
    let n = unsafe { GetKeyNameTextW(lparam, &mut buf) };
    match usize::try_from(n) {
        Ok(n) if n > 0 => String::from_utf16_lossy(&buf[..n.min(buf.len())]),
        _ => zalozni_nazev(k).unwrap_or_else(|| format!("#{:02X}", k.scan)),
    }
}

/// Název klávesy, kterou `GetKeyNameTextW` nepojmenuje.
///
/// F13–F24 (makro klávesy herních klávesnic, scan 0x64–0x6E a 0x76)
/// tabulka názvů rozložení nezná — na čepičce by bylo jen „#76".
/// Virtuální klávesu k nim ale rozložení zná, takže se název odvodí
/// z ní, ne z pevné tabulky scan kódů (princip 5: co klávesa znamená,
/// říká rozložení).
fn zalozni_nazev(k: KeyId) -> Option<String> {
    let scan = u32::from(k.scan) | if k.extended { 0xE000 } else { 0 };
    // SAFETY: jen převod kódu podle rozložení volajícího vlákna, nic nemění.
    let vk = unsafe { MapVirtualKeyW(scan, MAPVK_VSC_TO_VK_EX) };
    let (f1, f13, f24) = (u32::from(VK_F1.0), u32::from(VK_F13.0), u32::from(VK_F24.0));
    (f13..=f24)
        .contains(&vk)
        .then(|| format!("F{}", vk - f1 + 1))
}

/// Krátký název na čepičku (Fáze 6, spec 1.3) pro klávesy, jejichž
/// název podle rozložení je na čepičku dlouhý („Šipka nahoru",
/// „Mezerník") nebo nic neříká. Ostatní klávesy (`None`) dostanou
/// [`nazev`] utnutý na 5 znaků. Symboly vykreslí systémové písmo okna.
pub fn kratky(k: KeyId) -> Option<&'static str> {
    Some(match (k.scan, k.extended) {
        (0x48, true) => "↑",
        (0x50, true) => "↓",
        (0x4B, true) => "←",
        (0x4D, true) => "→",
        (0x39, false) => "␣",
        (0x1C, false) => "↵",
        (0x0E, false) => "⌫",
        (0x0F, false) => "⇥",
        (0x2A, false) => "⇧",
        (0x36, false) => "P⇧",
        (0x3A, false) => "Caps",
        (0x1D, true) => "P Ctrl",
        (0x38, true) => "P Alt",
        _ => return None,
    })
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

    /// F13–F24 Windows nepojmenují; název se odvodí z virtuální klávesy
    /// rozložení (scan kódy podle sady 1, jak je hlásí hook). Dřív na
    /// čepičce stálo „#76" (test okna na skryté ploše).
    #[test]
    fn f13_az_f24_maji_nazev() {
        let scany = [
            0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x76,
        ];
        for (i, scan) in scany.into_iter().enumerate() {
            assert_eq!(nazev(KeyId::new(scan)), format!("F{}", 13 + i), "{scan:#x}");
        }
        // Záložní název nesmí přebít ten od Windows ani si vymýšlet.
        assert_eq!(zalozni_nazev(KeyId::W), None);
        assert_eq!(zalozni_nazev(KeyId::new(0)), None);
    }

    /// Tabulka krátkých názvů ze specifikace (1.3). Bez E0 jsou šipky
    /// numerická klávesnice a Enter/Ctrl/Alt levé — ty krátký název
    /// nemají (dostanou utnutý název podle rozložení).
    #[test]
    fn kratke_nazvy_tabulkou() {
        let tabulka = [
            (KeyId::ARROW_UP, "↑"),
            (KeyId::ARROW_DOWN, "↓"),
            (KeyId::ARROW_LEFT, "←"),
            (KeyId::ARROW_RIGHT, "→"),
            (KeyId::SPACE, "␣"),
            (KeyId::ENTER, "↵"),
            (KeyId::BACKSPACE, "⌫"),
            (KeyId::new(0x0F), "⇥"),
            (KeyId::LEFT_SHIFT, "⇧"),
            (KeyId::new(0x36), "P⇧"),
            (KeyId::new(0x3A), "Caps"),
            (KeyId::ext(0x1D), "P Ctrl"),
            (KeyId::RIGHT_ALT, "P Alt"),
        ];
        for (k, ocekavano) in tabulka {
            assert_eq!(kratky(k), Some(ocekavano), "{k:?}");
        }
        for k in [
            KeyId::NUMPAD_8,
            KeyId::new(0x50),
            KeyId::ext(0x1C),
            KeyId::LEFT_CTRL,
            KeyId::LEFT_ALT,
            KeyId::W,
            KeyId::SCROLL_LOCK,
            KeyId::LEFT_WIN,
        ] {
            assert_eq!(kratky(k), None, "{k:?}");
        }
    }
}

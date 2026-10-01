//! Varianty ikony v oznamovací oblasti podle režimu (Fáze 4b).
//!
//! Skládají se za běhu z ikony aplikace: žádné další obrázky v binárce
//! ani skript, který by je vyráběl a rozcházel se s ikonou. Čistá funkce
//! nad RGBA — testuje se po pixelech, bez okna.
//!
//! Odznak je vpravo dole jako u ikon Windows (štít UAC, šipka zástupce):
//! zbytek ikony zůstává poznat a stav se čte jednou tečkou. Pozastavení
//! není jantarové — jantar patří poruše (`Pozor`), pauza je běžný stav.

/// Strana výsledné ikony. Windows ji v oznamovací oblasti zmenší na
/// 16 px (100 %) nebo ukáže celou (200 %).
pub const STRANA: u32 = 32;

/// Která varianta.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DruhIkony {
    /// Žádný ovladač nehraje: ikona v odstínech šedi, poloprůhledná.
    Vypnuto,
    /// Klávesy ovládají ovladač: zelená tečka.
    Hraje,
    /// Pozastaveno (Scroll Lock) nebo přiřazování: dvě bílé čárky.
    Pauza,
    /// Klávesy nejdou (hook) nebo chyba ovladače: jantarový „!“.
    Pozor,
}

/// Průměr odznaku vůči straně ikony.
const ODZNAK: f32 = 0.44;
/// Tmavý lem odznaku a podklad čárek pauzy (pozadí okna KeyPadu) — odznak
/// je tak vidět na světlé i tmavé liště.
const LEM: [u8; 3] = [0x0e, 0x0f, 0x12];
/// `--ok` z okna.
const ZELENA: [u8; 3] = [0x4a, 0xde, 0x80];
/// `--warn` z okna.
const JANTAR: [u8; 3] = [0xf5, 0x9e, 0x0b];
const BILA: [u8; 3] = [0xff, 0xff, 0xff];

/// Varianta ikony: libovolné RGBA `w`×`h` zmenšené na [`STRANA`]² RGBA
/// s odznakem podle `druh`. Poškozený vstup (nulová velikost, krátká
/// data) dá průhlednou ikonu s odznakem — nikdy paniku.
pub fn varianta(rgba: &[u8], w: u32, h: u32, druh: DruhIkony) -> Vec<u8> {
    let mut px = zmensi(rgba, w, h);
    match druh {
        DruhIkony::Vypnuto => odbarvi(&mut px),
        DruhIkony::Hraje | DruhIkony::Pauza | DruhIkony::Pozor => odznak(&mut px, druh),
    }
    px
}

/// Zmenšení (i zvětšení) průměrováním plochy. Barvy se průměrují
/// vážené alfou: průhledné okolí ikony jinak ztmaví její okraje.
fn zmensi(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let s = STRANA as usize;
    let mut px = vec![0u8; s * s * 4];
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 || rgba.len() / 4 / w < h {
        return px;
    }
    for ty in 0..s {
        let (y0, y1) = rozsah(ty, h, s);
        for tx in 0..s {
            let (x0, x1) = rozsah(tx, w, s);
            let mut soucet = [0u64; 4];
            let mut n = 0u64;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 4;
                    let a = u64::from(rgba[i + 3]);
                    for k in 0..3 {
                        soucet[k] += u64::from(rgba[i + k]) * a;
                    }
                    soucet[3] += a;
                    n += 1;
                }
            }
            let o = (ty * s + tx) * 4;
            // Úplně průhledná plocha barvu nemá — zůstane nula.
            for k in 0..3 {
                if let Some(v) = (soucet[k] + soucet[3] / 2).checked_div(soucet[3]) {
                    px[o + k] = v as u8;
                }
            }
            px[o + 3] = ((soucet[3] + n / 2) / n.max(1)) as u8;
        }
    }
    px
}

/// Zdrojové pixely `[od, do)`, které padnou do cílového pixelu `t`.
/// Nikdy prázdné: menší zdroj než cíl dá nejbližší pixel.
fn rozsah(t: usize, zdroj: usize, cil: usize) -> (usize, usize) {
    let od = (t * zdroj / cil).min(zdroj - 1);
    let konec = ((t + 1) * zdroj).div_ceil(cil).clamp(od + 1, zdroj);
    (od, konec)
}

/// Odstíny šedi (jas podle Rec. 601) a alfa × 0,6: „vypnuto“ jako
/// neaktivní ikony Windows.
fn odbarvi(px: &mut [u8]) {
    for p in px.chunks_exact_mut(4) {
        let jas =
            (299 * u32::from(p[0]) + 587 * u32::from(p[1]) + 114 * u32::from(p[2]) + 500) / 1000;
        p[0] = jas as u8;
        p[1] = jas as u8;
        p[2] = jas as u8;
        p[3] = ((u32::from(p[3]) * 6 + 5) / 10) as u8;
    }
}

/// Vyhlazený kruhový odznak vpravo dole (4×4 vzorků na pixel), složený
/// přes ikonu.
fn odznak(px: &mut [u8], druh: DruhIkony) {
    let s = STRANA as usize;
    let polomer = STRANA as f32 * ODZNAK / 2.0;
    let stred = STRANA as f32 - polomer;
    let od = (stred - polomer).floor().max(0.0) as usize;
    for y in od..s {
        for x in od..s {
            let mut soucet = [0f32; 3];
            let mut pokryto = 0u32;
            for j in 0..4 {
                for i in 0..4 {
                    let dx = x as f32 + (i as f32 + 0.5) / 4.0 - stred;
                    let dy = y as f32 + (j as f32 + 0.5) / 4.0 - stred;
                    if let Some(b) = barva_odznaku(druh, dx, dy, polomer) {
                        for k in 0..3 {
                            soucet[k] += f32::from(b[k]);
                        }
                        pokryto += 1;
                    }
                }
            }
            if pokryto == 0 {
                continue;
            }
            // Složení „přes“ (source-over) s nepremultiplikovaným cílem.
            let o = (y * s + x) * 4;
            let pa = pokryto as f32 / 16.0;
            let da = f32::from(px[o + 3]) / 255.0;
            let oa = pa + da * (1.0 - pa);
            for k in 0..3 {
                let barva = soucet[k] / pokryto as f32;
                let v = (barva * pa + f32::from(px[o + k]) * da * (1.0 - pa)) / oa;
                px[o + k] = v.round().clamp(0.0, 255.0) as u8;
            }
            px[o + 3] = (oa * 255.0).round() as u8;
        }
    }
}

/// Barva odznaku v bodě (`dx`, `dy` od středu), `None` = mimo odznak.
/// Uvnitř lemu jsou souřadnice poměrné k vnitřnímu poloměru (±1).
fn barva_odznaku(druh: DruhIkony, dx: f32, dy: f32, polomer: f32) -> Option<[u8; 3]> {
    let d = (dx * dx + dy * dy).sqrt();
    if d > polomer {
        return None;
    }
    if d > polomer - 1.0 {
        return Some(LEM);
    }
    let (u, v) = (dx / (polomer - 1.0), dy / (polomer - 1.0));
    Some(match druh {
        DruhIkony::Hraje => ZELENA,
        DruhIkony::Pauza => {
            let carka = v.abs() <= 0.5 && (0.12..=0.45).contains(&u.abs());
            if carka {
                BILA
            } else {
                LEM
            }
        }
        DruhIkony::Pozor => {
            let nozka = u.abs() <= 0.15 && (-0.65..=0.1).contains(&v);
            let tecka = (u * u + (v - 0.45) * (v - 0.45)).sqrt() <= 0.17;
            if nozka || tecka {
                LEM
            } else {
                JANTAR
            }
        }
        DruhIkony::Vypnuto => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: usize = STRANA as usize;

    /// Neprůhledná ikona 32×32 s barevným přechodem (ať je co odbarvit).
    fn ikona() -> Vec<u8> {
        let mut v = Vec::with_capacity(S * S * 4);
        for y in 0..S {
            for x in 0..S {
                v.extend_from_slice(&[(x * 8) as u8, (y * 8) as u8, 0x80, 0xff]);
            }
        }
        v
    }

    fn pixel(px: &[u8], x: usize, y: usize) -> [u8; 4] {
        let o = (y * S + x) * 4;
        [px[o], px[o + 1], px[o + 2], px[o + 3]]
    }

    fn s_alfou(b: [u8; 3]) -> [u8; 4] {
        [b[0], b[1], b[2], 0xff]
    }

    const VSECHNY: [DruhIkony; 4] = [
        DruhIkony::Vypnuto,
        DruhIkony::Hraje,
        DruhIkony::Pauza,
        DruhIkony::Pozor,
    ];

    #[test]
    fn velikost_je_vzdy_32x32() {
        let i = ikona();
        for druh in VSECHNY {
            assert_eq!(varianta(&i, 32, 32, druh).len(), S * S * 4);
        }
    }

    /// Vypnuto: jen odstíny šedi a alfa × 0,6, žádný barevný odznak.
    #[test]
    fn vypnuto_je_sede_a_pruhledne() {
        let px = varianta(&ikona(), 32, 32, DruhIkony::Vypnuto);
        for p in px.chunks_exact(4) {
            assert!(p[0] == p[1] && p[1] == p[2], "{p:?}");
            assert_eq!(p[3], 153);
        }
    }

    /// Hraje: střed ikony beze změny, odznak zelený s tmavým lemem.
    #[test]
    fn hraje_ma_zelenou_tecku_s_lemem() {
        let i = ikona();
        let px = varianta(&i, 32, 32, DruhIkony::Hraje);
        assert_eq!(pixel(&px, 16, 16), pixel(&i, 16, 16));
        assert_eq!(pixel(&px, 0, 0), pixel(&i, 0, 0));
        assert_eq!(pixel(&px, 24, 24), s_alfou(ZELENA));
        assert_eq!(pixel(&px, 31, 24), s_alfou(LEM), "lem vpravo");
        assert_eq!(pixel(&px, 24, 31), s_alfou(LEM), "lem dole");
    }

    /// Pauza: bílá čárka na tmavém kolečku, nad čárkami tma.
    #[test]
    fn pauza_ma_bile_carky_na_tmavem() {
        let i = ikona();
        let px = varianta(&i, 32, 32, DruhIkony::Pauza);
        assert_eq!(pixel(&px, 16, 16), pixel(&i, 16, 16));
        assert_eq!(pixel(&px, 23, 24), s_alfou(BILA), "levá čárka");
        assert_eq!(pixel(&px, 26, 24), s_alfou(BILA), "pravá čárka");
        assert_eq!(pixel(&px, 24, 19), s_alfou(LEM), "nad čárkami");
    }

    /// Pozor: jantarový kruh s tmavým „!“.
    #[test]
    fn pozor_je_jantar_s_vykricnikem() {
        let i = ikona();
        let px = varianta(&i, 32, 32, DruhIkony::Pozor);
        assert_eq!(pixel(&px, 16, 16), pixel(&i, 16, 16));
        assert_eq!(pixel(&px, 21, 24), s_alfou(JANTAR));
        assert_eq!(pixel(&px, 24, 22), s_alfou(LEM), "nožka vykřičníku");
    }

    /// Odznak je vidět i na průhledné ikoně (plná alfa uvnitř, vyhlazený
    /// okraj) a mimo kruh zůstává průhlednost.
    #[test]
    fn odznak_na_pruhledne_ikone() {
        let px = varianta(&[0; 16], 2, 2, DruhIkony::Hraje);
        assert_eq!(pixel(&px, 24, 24), s_alfou(ZELENA));
        assert_eq!(pixel(&px, 0, 0)[3], 0);
        assert_eq!(pixel(&px, 18, 31)[3], 0, "roh mimo kruh");
        let okraj = pixel(&px, 19, 20)[3];
        assert!(okraj > 0 && okraj < 0xff, "vyhlazený okraj: {okraj}");
    }

    /// Zmenšení průměruje plochu vážené alfou: bílá s průhlednou dá
    /// bílou s poloviční alfou (ne šedou).
    #[test]
    fn zmenseni_prumeruje_plochu() {
        let mut v = Vec::new();
        for y in 0..64 {
            for x in 0..64 {
                let bila = (x + y) % 2 == 0;
                v.extend_from_slice(if bila {
                    &[255, 255, 255, 255]
                } else {
                    &[0, 0, 0, 0]
                });
            }
        }
        let px = zmensi(&v, 64, 64);
        for p in px.chunks_exact(4) {
            assert_eq!(p, [255, 255, 255, 128]);
        }
    }

    /// Okrajové velikosti ani poškozený vstup nepanikaří.
    #[test]
    fn okrajove_velikosti() {
        let cervena = |w: usize, h: usize| [255u8, 0, 0, 255].repeat(w * h);
        for (w, h) in [(1, 1), (16, 16), (256, 256), (7, 300), (300, 7)] {
            let i = cervena(w, h);
            for druh in VSECHNY {
                let px = varianta(&i, w as u32, h as u32, druh);
                assert_eq!(px.len(), S * S * 4, "{w}×{h}");
                if druh != DruhIkony::Vypnuto {
                    assert_eq!(pixel(&px, 2, 2), [255, 0, 0, 255], "{w}×{h} {druh:?}");
                }
            }
        }
        for druh in VSECHNY {
            assert_eq!(varianta(&[], 0, 0, druh).len(), S * S * 4);
            assert_eq!(varianta(&[1, 2, 3], 16, 16, druh).len(), S * S * 4);
            assert_eq!(varianta(&[], u32::MAX, u32::MAX, druh).len(), S * S * 4);
        }
    }
}

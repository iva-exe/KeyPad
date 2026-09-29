//! SHA-256 přes CNG (`bcrypt.dll`) — součást Windows.
//!
//! Vlastní implementace ani crate navíc: kryptografii, na které visí
//! rozhodnutí „spustit cizí binárku s právy správce", má dělat systém,
//! ne pár set řádků, které jsme si napsali sami.
//!
//! Schválně klasická čtveřice `BCryptOpenAlgorithmProvider` →
//! `CreateHash` → `HashData` → `FinishHash` (Windows Vista), ne
//! jednorázové `BCryptHash` ani pseudohandly `BCRYPT_SHA256_ALG_HANDLE`
//! (obojí až Windows 10): u starších funkcí nemusíme řešit, od kterého
//! buildu desítek existují (princip 9), a umí počítat po kouscích —
//! instalátor tak hashuje soubor přímo z uzamčeného handlu, bez druhé
//! kopie v paměti.

use windows::core::PCWSTR;
use windows::Win32::Foundation::NTSTATUS;
use windows::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash,
    BCryptHashData, BCryptOpenAlgorithmProvider, BCRYPT_ALG_HANDLE, BCRYPT_HASH_HANDLE,
    BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM,
};

/// Délka otisku v bajtech.
pub const LEN: usize = 32;

fn check(what: &str, st: NTSTATUS) -> Result<(), String> {
    // NT_SUCCESS: nezáporný stav je úspěch (i informativní).
    if st.0 >= 0 {
        Ok(())
    } else {
        Err(format!("SHA-256: {what} selhalo (0x{:08X})", st.0 as u32))
    }
}

/// Průběžný výpočet SHA-256. Handly se uvolní v `Drop` — i když se
/// výpočet nedokončí (chyba čtení uprostřed souboru).
pub struct Sha256 {
    alg: BCRYPT_ALG_HANDLE,
    hash: BCRYPT_HASH_HANDLE,
}

impl Sha256 {
    pub fn new() -> Result<Self, String> {
        let mut alg = BCRYPT_ALG_HANDLE::default();
        // SAFETY: výstupní handle je platný ukazatel; název algoritmu je
        // statický řetězec z windows-rs.
        check("otevření algoritmu", unsafe {
            BCryptOpenAlgorithmProvider(
                &mut alg,
                BCRYPT_SHA256_ALGORITHM,
                PCWSTR::null(),
                BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
            )
        })?;
        let mut me = Sha256 {
            alg,
            hash: BCRYPT_HASH_HANDLE::default(),
        };
        // Objekt hashe si od Windows 7 alokuje CNG sám (pbHashObject NULL).
        // SAFETY: `alg` je otevřený; výstupní handle patří `me` a uvolní
        // ho Drop.
        check("založení hashe", unsafe {
            BCryptCreateHash(me.alg, &mut me.hash, None, None, 0)
        })?;
        Ok(me)
    }

    pub fn update(&mut self, data: &[u8]) -> Result<(), String> {
        // cbInput je u32 — obří vstup po kouscích, ať se délka nepřetočí.
        for part in data.chunks(1 << 30) {
            // SAFETY: handle hashe žije, dokud žije `self`.
            check("výpočet", unsafe { BCryptHashData(self.hash, part, 0) })?;
        }
        Ok(())
    }

    pub fn finish(self) -> Result<[u8; LEN], String> {
        let mut out = [0u8; LEN];
        // SAFETY: výstupní buffer má přesně délku otisku SHA-256.
        check("dokončení", unsafe {
            BCryptFinishHash(self.hash, &mut out, 0)
        })?;
        Ok(out)
    }
}

impl Drop for Sha256 {
    fn drop(&mut self) {
        // SAFETY: handly pochází z CNG a uvolňují se právě jednou
        // (neplatné — nezaložené — se přeskočí).
        unsafe {
            if !self.hash.is_invalid() {
                let _ = BCryptDestroyHash(self.hash);
            }
            if !self.alg.is_invalid() {
                let _ = BCryptCloseAlgorithmProvider(self.alg, 0);
            }
        }
    }
}

/// SHA-256 celého bufferu.
pub fn sha256(data: &[u8]) -> Result<[u8; LEN], String> {
    let mut h = Sha256::new()?;
    h.update(data)?;
    h.finish()
}

/// SHA-256 všeho, co jde přečíst z `r` (soubor přes uzamčený handle).
pub fn sha256_reader(r: &mut impl std::io::Read) -> Result<[u8; LEN], String> {
    let mut h = Sha256::new()?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = r
            .read(&mut buf)
            .map_err(|e| format!("čtení pro SHA-256: {e}"))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n])?;
    }
    h.finish()
}

/// Otisk jako text (velká písmena, jak ho píše `Get-FileHash`).
pub fn to_hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02X}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Testovací vektory z FIPS 180-2 / NIST (příklady SHA-256).
    #[test]
    fn testovaci_vektory_nist() {
        let cases: &[(&[u8], &str)] = &[
            (
                b"",
                "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855",
            ),
            (
                b"abc",
                "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
            ),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248D6A61D20638B8E5C026930C3E6039A33CE45964FF2167F6ECEDD419DB06C1",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(to_hex(&sha256(input).unwrap()), *want, "{input:?}");
        }
    }

    /// Milion „a" — delší vstup, víc bloků kompresní funkce.
    #[test]
    fn milion_a() {
        let data = vec![b'a'; 1_000_000];
        assert_eq!(
            to_hex(&sha256(&data).unwrap()),
            "CDC76E5C9914FB9281A1C7E284D73E67F1809A48A497200E046D39CCC7112CD0"
        );
    }

    /// Po kouscích (z readeru, přes hranice bloků) musí vyjít totéž
    /// co najednou — tak se hashuje soubor z uzamčeného handlu.
    #[test]
    fn po_kouscich_jako_najednou() {
        let data: Vec<u8> = (0..200_003u32).map(|i| (i * 31 % 251) as u8).collect();
        let whole = sha256(&data).unwrap();
        let mut h = Sha256::new().unwrap();
        for part in data.chunks(777) {
            h.update(part).unwrap();
        }
        assert_eq!(h.finish().unwrap(), whole);
        let mut cur = std::io::Cursor::new(&data);
        assert_eq!(sha256_reader(&mut cur).unwrap(), whole);
    }

    /// Nedokončený výpočet se uklidí bez paniky (Drop).
    #[test]
    fn nedokonceny_vypocet_se_uklidi() {
        let mut h = Sha256::new().unwrap();
        h.update(b"x").unwrap();
        drop(h);
    }
}

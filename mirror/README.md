# Zrcadlo ViGEmBus

`ViGEmBus_1.22.0_x64_x86_arm64.exe` je **nezměněná kopie** oficiálního
instalátoru ovladače ViGEmBus 1.22.0 z
<https://github.com/nefarius/ViGEmBus/releases/tag/v1.22.0>.

- Autor: Nefarius Software Solutions e.U., licence BSD-3-Clause
  (<https://github.com/nefarius/ViGEmBus/blob/master/LICENSE>).
- SHA-256: `89220A7865076B342892F98865F3499FB7C4CFD673159E89D352C360FD014C6A`
  (stejný otisk uvádí i winget), podpis: Nefarius Software Solutions e.U.

Proč tu je: projekt ViGEmBus je archivovaný. Kdyby jeho repozitář
zmizel, KeyPadSetup si instalátor stáhne odsud. Ověřuje ho **stejným
napevno zapsaným otiskem** jako originál (`crates/updater/src/vigembus.rs`),
takže zrcadlo nemůže podstrčit nic jiného — jakákoli změna souboru
znamená, že se nespustí.

# KeyPad

KeyPad udělá z klávesnice herní ovladač. Vybrané klávesy převádí na
**virtuální ovladač Xbox 360** a Steam ho při **Remote Play Together**
pošle hostiteli jako dalšího hráče. Hra u kamaráda tě tak vidí jako
hráče s gamepadem, i když hraješ na klávesnici.

```
tvoje klávesnice → KeyPad → virtuální Xbox ovladač → Steam → Remote Play → hra u hostitele
```

Mezi režimy **Klávesnice** a **Gamepad** se přepíná klávesou
**Scroll Lock** nebo tlačítkem v okně. V režimu Gamepad jdou do
ovladače jen namapované klávesy (WASD, šipky…), všechno ostatní funguje
dál normálně — Alt+Tab, Win, Alt+F4.

> **Poctivě: tahle verze je zatím kostra.** Instalace, okno
> a aktualizace fungují, samotný převod kláves na gamepad přibude
> v dalších verzích. Nic nemusíš stahovat znovu — nové verze ti
> KeyPad nabídne sám (viz [Aktualizace](#aktualizace)).

## Co budeš potřebovat

- Windows 10 nebo 11, 64bitové
- **Microsoft Edge WebView2 Runtime** — skoro jistě už ho máš (viz níž)
- **ViGEmBus** — ovladač, který vytváří virtuální gamepad (viz níž)

## Instalace

1. Stáhni **`KeyPadSetup.exe`** (pošlu ti ho, nebo
   <https://github.com/iva-exe/KeyPad/raw/main/release/KeyPadSetup.exe>).
   Prohlížeč může varovat, že se soubor běžně nestahuje — zvol **Zachovat**.
2. Spusť ho dvojklikem. Stáhne aktuální KeyPad a nainstaluje ho do tvého
   profilu, do `%LOCALAPPDATA%\Programs\KeyPad`. **Nepotřebuje práva
   správce** — žádné okno „Chcete povolit této aplikaci…“ nevyskočí.
3. V nabídce Start přibude **KeyPad**. Instalátor ho na konci rovnou
   spustí.

### „Systém Windows ochránil váš počítač“

Při prvním spuštění se může objevit modré okno SmartScreenu. Klikni na
**Další informace** → **Přesto spustit**.

Proč se to ukazuje: KeyPad není podepsaný placeným certifikátem
a stáhlo ho zatím málo lidí. SmartScreen varuje u každého takového
programu, ne proto, že by v něm něco našel. Týká se to jen souboru
staženého prohlížečem — aktualizace z aplikace už okno nevyvolají.

### WebView2 Runtime (nutné)

Okno KeyPadu vykresluje **Microsoft Edge WebView2 Runtime** — součástka
od Microsoftu, na které běží spousta aplikací. Windows 11 ho mají
v sobě, Windows 10 ho skoro vždy dostaly s prohlížečem Edge.

Když chybí, instalátor to pozná, KeyPad nespustí a dá ti odkaz na malý
instalátor od Microsoftu:
<https://go.microsoft.com/fwlink/p/?LinkId=2124703>
Po jeho doinstalování spusť `KeyPadSetup.exe` znovu.

### ViGEmBus (nutné pro gamepad)

Ovladač, díky kterému Windows (a Steam) vidí virtuální Xbox ovladač.

1. Otevři <https://github.com/nefarius/ViGEmBus/releases>.
2. U nejnovějšího vydání (nahoře, označené „Latest“) stáhni instalátor
   `ViGEmBus_…_x64_x86_arm64.exe` a nainstaluj ho.
3. Tohle je **jediné místo, kde Windows chtějí práva správce** — jde
   o ovladač a instaluje se jen jednou.

Projekt ViGEmBus už se dál nevyvíjí (je archivovaný), ale na Windows 10
i 11 funguje. Když ovladač chybí, instalátor KeyPadu na to upozorní.

## Aktualizace

KeyPad si sám hlídá, jestli je venku nová verze. Když ano, ukáže
jantarový pruh **„Je dostupná nová verze“** — stačí kliknout na
**Aktualizovat**. KeyPad se zavře, aktualizuje a znovu spustí.

KeyPad nabízí jen verzi **novější**, než je ta tvoje. Starší nikdy —
GitHub drží informaci o verzi v cache až 5 minut, a hned po
aktualizaci by se jinak mohla nabídnout ta předchozí.

Druhá možnost: spusť `KeyPadSetup.exe` znovu. Nainstaluje přesně to,
co je zrovna vydané; když už to máš, jen zkontroluje instalaci
a KeyPad spustí. Tudy vede i **návrat ke starší verzi**: když vydání
vrátím zpátky, aplikace ti ho nenabídne (je starší než to, co máš),
ale `KeyPadSetup.exe` ho nainstaluje.

## Odinstalace

**Nastavení → Aplikace → KeyPad → Odinstalovat.**
Nebo z příkazové řádky: `KeyPadSetup.exe /uninstall`.

Odinstalace odebere program, zástupce v nabídce Start a záznam
v Aplikacích. Smaže i logy (`keypad.log` — jestli mi ho chceš poslat,
udělej to předtím), data okna (WebView2, `%LOCALAPPDATA%\cz.hexel.keypad`)
a instalátory stažené při aktualizacích (`%TEMP%\keypad-update`).
**Tvoje nastavení (`config.toml`) nechá** — kdybys KeyPad instaloval
znovu, mapování kláves tě počká. Když ho už nechceš, smaž ho ručně.

## Kde co leží

| Co | Kde |
|---|---|
| Program | `%LOCALAPPDATA%\Programs\KeyPad` (`KeyPad.exe`, `KeyPadSetup.exe`, `version.txt`) |
| Log | `keypad.log` vedle `KeyPad.exe` (když tam nejde zapisovat, tak `%APPDATA%\KeyPad`) |
| Nastavení | přibude později — ve stejné složce nebo v `%APPDATA%\KeyPad` |
| Data okna (WebView2) | `%LOCALAPPDATA%\cz.hexel.keypad` — cache a data, která si okno ukládá samo (desítky MB) |
| Stažený instalátor | `%TEMP%\keypad-update` — `KeyPadSetup-….exe` z tlačítka Aktualizovat |

Složku otevřeš rychle přes **Win+R** → `%LOCALAPPDATA%\Programs\KeyPad` → Enter.
Když něco nefunguje, pošli mi `keypad.log`.

## Antivirus hlásí KeyPad

Může se to stát. KeyPad sleduje klávesnici (tzv. keyboard hook), aby
mohl vybrané klávesy poslat do gamepadu místo do hry — a přesně tohle
dělají i keyloggery, takže na to některé antiviry reagují podezřením.

KeyPad stisky nikam neposílá. Po síti jen zjišťuje na GitHubu, jestli
je nová verze. Zdrojový kód je veřejný:
<https://github.com/iva-exe/KeyPad>.

## Bezpečnost: klávesnice se nikdy neztratí

- Při jakékoli chybě se KeyPad sám přepne zpátky do režimu
  **Klávesnice**. Nikdy naopak.
- V režimu Gamepad fungují všechny klávesy, které nejsou namapované —
  Alt+Tab, Win, Alt+F4.
- **Poslední záchrana:** **Ctrl+Alt+Del** → **Správce úloh** → KeyPad →
  **Ukončit úlohu**. Ctrl+Alt+Del žádný program zachytit nemůže.
  Sledování klávesnice i virtuální ovladač zmizí spolu s procesem.
- **Nespouštěj Steam jako správce.** KeyPad běží bez práv správce
  a Windows nedovolí takovému programu zablokovat klávesy programu se
  správcovskými právy — do streamu by pak šly i klávesy, které mají jít
  jen do gamepadu. Kontrola: pravým na zástupce Steamu → Vlastnosti →
  Kompatibilita → „Spustit tento program jako správce“ musí být
  vypnuté.

_Přesný postup ve Steamu doplním, až ho vyzkoušíme naostro._

## Závislosti binárky (pro vývojáře)

Otázka: potřebují naše `.exe` knihovnu `VCRUNTIME140.dll`? Ta není
součástí Windows (je z Visual C++ Redistributable) a na čistém PC chybí.
Univerzální CRT (`api-ms-win-crt-*`) součástí Windows 10+ je, takže
nevadí.

Změřeno 24. 9. 2026 (Rust 1.97.0, tauri 2.11.6, tauri-build 2.6.3,
@tauri-apps/cli 2.11.5, MSVC 14.44). `dumpbin /DEPENDENTS`
a `tools\check-imports.ps1` daly shodné seznamy.

| Binárka a jak vznikla | `+crt-static` | static vcruntime od Tauri | CRT knihovny v importech | Velikost | check-imports |
|---|---|---|---|---|---|
| a) KeyPad.exe, `tools\tauri.ps1 build` (**dnešní stav**) | ano | ano | 7× `api-ms-win-crt-*` | 9 360 896 B | prošlo |
| b) KeyPad.exe, totéž s `RUSTFLAGS=` | ne | ano | 7× `api-ms-win-crt-*` | 9 360 896 B | prošlo |
| c) KeyPad.exe, holý `cargo build --release -p keypad` s `RUSTFLAGS=` | ne | ne | **`VCRUNTIME140.dll`, `VCRUNTIME140_1.dll`** + 7× `api-ms-win-crt-*` | 9 012 224 B (bez UI) | **neprošlo** |
| d) KeyPad.exe, `tauri build` se `STATIC_VCRUNTIME=false` | ano | ne | žádné | 9 455 104 B | prošlo |
| KeyPadSetup.exe, `cargo build --release -p installer` (**dnešní stav**) | ano | — | žádné | 598 528 B | prošlo |
| KeyPadSetup.exe, totéž s `RUSTFLAGS=` | ne | — | **`VCRUNTIME140.dll`** + 5× `api-ms-win-crt-*` | 496 640 B | **neprošlo** |

Ostatní importy jsou jen systémové DLL. KeyPad.exe jich má 15
(kernel32, user32, shell32, winhttp, dwmapi…), KeyPadSetup.exe 11.
Binárky a) a b) se liší jen ve 24 bajtech, takže `+crt-static` je
v KeyPad.exe z `tauri build` dnes bez účinku.

**Co Tauri dělá samo.** `tauri-build` (volá ho `src-tauri\build.rs`)
má modul `static_vcruntime`. Ten zapne jen proměnná prostředí
`STATIC_VCRUNTIME=true` a tu nastavuje **jen Tauri CLI při
`tauri build`**, pokud už není nastavená na `false`. Holý `cargo build`,
`cargo test` ani clippy ji nemají, proto c) importuje VCRUNTIME140.
Modul pošle linkeru `/NODEFAULTLIB:msvcrt.lib`, `/NODEFAULTLIB:libucrt.lib`
(a další) a `/DEFAULTLIB:libcmt.lib`, `libvcruntime.lib`, `ucrt.lib`.
VC++ runtime tak skončí uvnitř exe, ale UCRT se bere dynamicky
z Windows. Tyhle argumenty přebijí statický UCRT z `+crt-static`. Proto
a) importuje `api-ms-win-crt-*` a d), kde Tauri nezasahuje, nemá
v importech CRT vůbec.

**Past v Tauri.** `tauri-build` neříká cargu, že závisí na
`STATIC_VCRUNTIME` (chybí mu `rerun-if-env-changed`). Cargo si proto
pamatuje výstup build skriptu z prvního běhu. Ověřeno: po d) postavil
další `tauri build` do stejného `target\` exe zase bez static vcruntime,
protože build skript se znovu nespustil. Bez `+crt-static` by takové
exe importovalo VCRUNTIME140. **Uzavřeno:** `src-tauri\build.rs` teď
cargu hlásí `cargo:rerun-if-env-changed=STATIC_VCRUNTIME`, takže změna
proměnné build skript spustí znovu.

**Linker nevaruje.** KeyPad.exe se s `+crt-static` i argumenty Tauri
slinkuje i s `/WX` (varování linkeru = chyba). Pro kontrolu: skutečný
konflikt (`+crt-static` + `/DEFAULTLIB:msvcrt.lib`) s `/WX` spadne na
LNK4098. Pozor, rustc 1.97 varování linkeru sám nevypíše (ani
s `-W linker-messages`), takže čistý výstup cargo nic nedokazuje.

**Závěr: `+crt-static` v `.cargo\config.toml` zůstává.**
- **KeyPadSetup.exe** přes `tauri-build` vůbec nejde. Bez `+crt-static`
  importuje VCRUNTIME140 a na čistém PC by se nespustil právě
  instalátor, tedy první věc, kterou kamarád spustí.
- **KeyPad.exe** pak nezávisí na proměnné z Tauri CLI ani na tom, co si
  cargo pamatuje z build skriptu. Bez VCRUNTIME140 jsou i binárky z holého
  `cargo build`.
- Nic to nestojí: KeyPad.exe se nezmění, instalátor je o ~100 KB větší.

**Pojistka.** `tools\check-imports.ps1` čte importní tabulku exe
a zastaví vydání v `publish.ps1` (krok 5/7) i v CI, když se objeví
VCRUNTIME, MSVCP a podobné. Pozor na `RUSTFLAGS`: jakákoli nastavená
hodnota, i prázdná, přebije rustflags z `.cargo\config.toml` celé,
včetně `+crt-static` (ověřeno na příkazových řádcích rustc v `cargo -v`).
Při měření šla prázdná hodnota z Git Bashe (`RUSTFLAGS= …`). Windows
PowerShell 5.1 proměnnou s `$env:RUSTFLAGS = ''` smaže, pwsh 7 ji nechá
prázdnou.

**Podvržená DLL ze složky s .exe.** Obě binárky jsou slinkované
s `/DEPENDENTLOADFLAG:0x800` (LOAD_LIBRARY_SEARCH_SYSTEM32): DLL ze
statických importů se hledají jen v System32. Bez toho by Windows
hledalo `dwmapi.dll` a `winhttp.dll` (nejsou mezi KnownDLLs) nejdřív
ve složce s .exe — a `KeyPadSetup.exe` se spouští ze Stažených
souborů, kam umí prohlížeč uložit cizí soubor bez ptaní. Podvržená DLL
by běžela v našem procesu dřív než `main()`. Příznak čte
`tools\check-imports.ps1 -RequireDependentLoadFlag` z PE (load config)
a bez něj vydání v `publish.ps1` (krok 5/7) i CI zastaví.

## Pro vývojáře

Potřeba: Rust přes rustup (správnou verzi si vezme z
`rust-toolchain.toml`), Visual Studio Build Tools (MSVC), [bun](https://bun.sh).
Skripty se spouštějí `powershell -ExecutionPolicy Bypass -File tools\…`.

| Příkaz | Co dělá |
|---|---|
| `tools\check.ps1` | všechny brány: fmt, clippy, testy, svelte-check (`tools\check.ps1 clippy` = jen jedna) |
| `tools\tauri.ps1 dev` | aplikace ve vývojovém režimu, UI se přenačítá za běhu |
| `tools\publish.ps1` | vydání: kontrola gitu → brány → build → kontroly → `release\` → push |
| `tools\check-imports.ps1 [-RequireDependentLoadFlag] <exe>` | nepotřebuje binárka DLL, která na čistém PC chybí? (a s přepínačem: hledá je jen v System32?) |

`tools\publish.ps1` vydává jen zdrojový kód, který je commitnutý
a pushnutý na GitHub (necommitnuté změny i nepushnuté commity mimo
`release\` ho zastaví ještě před buildem). Binárky tak vždycky
odpovídají veřejnému kódu a zpráva commitu vydání nese commit, ze
kterého vznikly: `release: 0.1.0+20260924.2235 (a1b2c3d)`.

**Instalátor ze skriptu.** `KeyPadSetup.exe /headless` vypisuje do
konzole, ale je to program s oknem (subsystém Windows): PowerShell ani
interaktivní cmd na něj nečekají a návratový kód (0 = hotovo,
1 = chyba) se ztratí. Počkat a kód získat jde takhle:

```bat
start "" /wait KeyPadSetup.exe /headless
echo %errorlevel%
```

```powershell
(Start-Process .\KeyPadSetup.exe -ArgumentList '/headless' -Wait -PassThru).ExitCode
```

Podrobnosti a pravidla projektu jsou v `CLAUDE.md` a `ROADMAP.md`.

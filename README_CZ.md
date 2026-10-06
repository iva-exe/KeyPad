# KeyPad

KeyPad udělá z klávesnice herní ovladač. Vybrané klávesy převádí na
**virtuální ovladač Xbox 360** a Steam ho při **Remote Play Together**
pošle hostiteli jako dalšího hráče. Hra u kamaráda tě tak vidí jako
hráče s gamepadem, i když hraješ na klávesnici.

```
tvoje klávesnice → KeyPad → virtuální Xbox ovladač → Steam → Remote Play → hra u hostitele
```

Ovladač zapneš **přepínačem v okně** — od té chvíle jdou namapované
klávesy (WASD, šipky…) do ovladače, všechno ostatní funguje dál
normálně: Alt+Tab, Win, Alt+F4. Klávesou **Scroll Lock** převádění
**pozastavíš** (třeba kvůli psaní do chatu) — ovladač zůstane
připojený, jen stojí; dalším Scroll Lockem ho zase pustíš.

Klávesy si nastavíš přímo v okně, jako v menu kláves ve hře, a z jedné
klávesnice můžou hrát až **čtyři ovladače** — každý má svoje klávesy
a svoji barvu (viz [Klávesy a víc ovladačů](#klávesy-a-víc-ovladačů)).
Nové verze ti KeyPad nabídne sám (viz [Aktualizace](#aktualizace)).

## Co budeš potřebovat

- Windows 10 (verze 1709 a novější, včetně LTSC) nebo Windows 11, 64bitové
  — doporučeno Windows 10 22H2 nebo 11. Starší Windows 10 KeyPad spustí
  taky, jen je Microsoft už nepodporuje pro WebView2 (viz níž).
- **Microsoft Edge WebView2 Runtime** — skoro jistě už ho máš (viz níž)
- **ViGEmBus** — ovladač, který vytváří virtuální gamepad; když chybí,
  **nainstaluje ho KeyPadSetup sám** (viz níž)

## Instalace

1. Stáhni **`KeyPadSetup.exe`** (pošlu ti ho, nebo
   <https://github.com/iva-exe/KeyPad/raw/main/release/KeyPadSetup.exe>).
   Prohlížeč může varovat, že se soubor běžně nestahuje — zvol **Zachovat**.
2. Spusť ho dvojklikem. Stáhne aktuální KeyPad a nainstaluje ho do tvého
   profilu, do `%LOCALAPPDATA%\Programs\KeyPad`. KeyPad sám **práva
   správce nepotřebuje nikdy**.
3. Když ti chybí ovladač ViGEmBus (nebo máš starou verzi), instalátor ho
   nainstaluje sám a Windows se jednou zeptají na povolení správce —
   patří oficiálnímu instalátoru ovladače, ne KeyPadu.
4. V nabídce Start přibude **KeyPad**. Instalátor ho na konci rovnou
   spustí. S Windows se KeyPad **nikdy nespouští sám** — jen když ho
   zapneš.

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
Autor: Nefarius Software Solutions e.U., licence BSD-3-Clause
(<https://github.com/nefarius/ViGEmBus>). Projekt už se dál nevyvíjí
(je archivovaný), ale na Windows 10 i 11 funguje.

**Nemusíš nic stahovat sám.** KeyPadSetup ovladač automaticky
nainstaluje, když chybí, a aktualizuje, když je starší než poslední
verze. Co se přitom děje:

- stáhne se **oficiální** instalátor ViGEmBus 1.22.0 z GitHubu autora
  (kdyby tam už nebyl, z kopie v repu KeyPadu) — vždycky přesně tentýž
  soubor; KeyPad ho před spuštěním ověří otiskem SHA-256 a podpisem
  vydavatele, jinak ho nespustí;
- Windows se zeptají na povolení správce (ukážou ověřeného vydavatele
  „Nefarius Software Solutions e.U.“) — ovladač se bez toho nainstalovat
  nedá. Při aktualizaci staré verze se KeyPad na chvilku zavře a pak
  sám znovu spustí (ovladač nesmí být během aktualizace používaný);
  někdy je potom potřeba restart;
- když odmítneš, KeyPad se nainstaluje i tak — ovladač doinstaluješ
  později tlačítkem **Nainstalovat ovladač** v aplikaci;
- po instalaci KeyPad ověří, že ovladač opravdu běží.

Aktuální, funkční ViGEmBus KeyPad nikdy nepřeinstalovává. Když je
nainstalovaný, ale neběží (čeká na restart, je vypnutý ve Správci
zařízení…), jen poradí, co s tím. Odinstalace KeyPadu ViGEmBus **nechá**
— může ho používat i jiný program; odebereš ho v Nastavení → Aplikace.

Ruční cesta, kdyby cokoli selhalo: <https://github.com/nefarius/ViGEmBus/releases>
→ nejnovější vydání → `ViGEmBus_…_x64_x86_arm64.exe`.

## Okno a oznamovací oblast

KeyPad sedí v oznamovací oblasti vedle hodin (jako WinSent). Zavřením
okna se KeyPad **neukončí** — jen se schová a přestane zatěžovat
počítač. Okno otevřeš kliknutím na ikonu, **ukončíš** ho pravým
tlačítkem na ikoně → **Ukončit**. Verzi, autora ovladače ViGEmBus
a log najdeš pod ⓘ v liště okna.

Při hraní je okno schované, a tak stav ukazuje **ikona**: zelená tečka
= hraje, dvě bílé čárky = pozastaveno (Scroll Lock), šedá = vypnuto,
jantarový vykřičník = klávesy nejdou nebo ovladač hlásí chybu. Při
pozastavení a pokračování KeyPad krátce **pípne** (klesající a stoupající
tón; vypneš ho v nabídce ikony odškrtnutím **Zvuk**). V nabídce ikony je
i **Pozastavit / Pokračovat**. Přes hru KeyPad nikdy žádné okno ani
upozornění neukazuje.

> **Tip:** Windows nové ikony schovávají do přetečení (šipka **^** vedle
> hodin), kde při hře přes celou obrazovku nejsou vidět. Chyť ikonu
> KeyPadu myší a **přetáhni ji z přetečení na lištu** — pak stav uvidíš
> pořád. KeyPad to sám nezařídí (musel by sahat do registru).

**Virtuální ovladač zapíná jen přepínač v okně.** Po spuštění KeyPadu
ani po probuzení počítače žádný ovladač v systému není. Před uspáním
počítače se ovladač vypne (jinak hrozí modrá obrazovka — známá chyba
ViGEmBus) a vypnutý zůstane; před vypnutím nebo restartem počítače se
vypne a KeyPad skončí.

| Stav | Co to znamená |
|---|---|
| **Vypnutý** | ovladač v systému není; zapneš ho přepínačem |
| **Nezapnul se** | zapnutí se nepovedlo — důvod v bublině, zkus to znovu |
| **Zapínám…** | Windows ovladač nastavují (napoprvé na novém PC chvíli trvá) |
| **Zapnutý** | ovladač běží a klávesy ho ovládají — **Vyzkoušet** s ním krátce zakrouží (Win+R → `joy.cpl`) |
| **Pozastaveno** | ovladač běží, ale klávesy jdou do Windows (Scroll Lock) — tečka jen obrysem |
| **Klávesy nejdou** | ovladač běží, ale Windows nedovolily sledovat klávesnici — klávesy jdou do Windows; vypni a zapni ovladač |
| **Chybí ViGEmBus** | tlačítko **Nainstalovat ovladač** |
| **ViGEmBus neběží** | nainstalovaný, ale nespustil se — rada v bublině (restart, Správce zařízení…) |

## Klávesy a víc ovladačů

Každý ovladač má v okně svou **kartu**: barevný pruh a číslo (1 modrá,
2 růžová, 3 fialová, 4 azurová), jedno slovo stavu a přepínač. Rozbalená
karta ukazuje ovladač ve tvaru Xboxu a na každém tlačítku, páčce
a spoušti je klávesa, která ho ovládá.

- **Změnit klávesu:** klikni na tlačítko a stiskni klávesu — nahradí tu
  dosavadní, jako v menu kláves ve hře. Esc, klik jinam nebo 10 s bez
  stisku přiřazování zruší.
- **Další klávesa na totéž tlačítko:** najeď myší na tlačítko a klikni
  na malé **+**. **Vyprázdnit** tlačítko: **×** v rohu nebo pravý klik.
- Když stiskneš klávesu, kterou už má jiné tlačítko (nebo jiný ovladač),
  **přesune se** — okno to napíše („Přesunuto z ovladače 2 · A“)
  a pár vteřin nabízí **Zpět**.
- **Win** přiřadit nejde (patří Windows, Win+D a spol. musí fungovat
  i ve hře), Scroll Lock taky ne (to je pauza).
- **↺** na kartě ovladače 1 vrátí jeho výchozí klávesy (dva kliky); klávesy,
  které mezitím dostal jiný ovladač, mu nebere.
- **+ Ovladač** přidá kartu dalšího hráče (až 4) — začíná bez kláves,
  přiřaď mu je stejně jako prvnímu a zapni ho jeho přepínačem. **🗑**
  odebere vypnutý ovladač i s klávesami (dva kliky).
- Stisknutá klávesa se na kartě **rozsvítí**, i když ovladač neběží —
  obrysem, co držíš, plnou barvou, co hra opravdu dostává (A+D: svítí
  obě, plně ta, která vyhrála). Bez zapnutého ovladače se to ukazuje
  jen s oknem KeyPadu v popředí. Se zapnutým ovladačem svítí klávesy
  hry i s oknem na pozadí (dokud je vidět); klávesy, které jdou do
  Windows, ukazuje okno jen v popředí.
- **AltGr** (pravý Alt na české klávesnici) přiřadit nejde — Windows
  s ním posílají i levý Ctrl a ten by šel do hry s ním. Mediální
  klávesy taky ne. Okno napíše „Tuhle klávesu nejde použít“.
- Klávesy jdou upravit i za hry. Během přiřazování nové klávesy hra na
  chvíli nedostává nic (všechny ovladače stojí), pak hraje dál.

Klávesy se **ukládají samy** do `%APPDATA%\KeyPad\config.json` (vznikne
při první změně, do té doby platí výchozí). Když ho ručně rozbiješ,
KeyPad ho odloží jako `config.invalid.json`, vezme výchozí klávesy
a nahoře v okně ukáže „Klávesy nešly načíst — výchozí“. Zapnutý
ovladač se neukládá nikdy — po spuštění je vždycky vypnutý.

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
Log instalátoru (`%TEMP%\KeyPadSetup.log` — kdyby instalace nešla,
pošli mi ho) smaže taky.
**Tvoje klávesy (`%APPDATA%\KeyPad\config.json`, případně i záloha
`config.invalid.json`) nechá** — kdybys KeyPad instaloval znovu,
mapování kláves tě počká. Když ho už nechceš, smaž složku ručně.
**Ovladač ViGEmBus taky nechá** (může ho používat i jiný program);
odebereš ho v Nastavení → Aplikace → „ViGEm Bus Driver“.

## Kde co leží

| Co | Kde |
|---|---|
| Program | `%LOCALAPPDATA%\Programs\KeyPad` (`KeyPad.exe`, `KeyPadSetup.exe`, `version.txt`) |
| Log | `keypad.log` vedle `KeyPad.exe` (když tam nejde zapisovat, tak `%APPDATA%\KeyPad`) |
| Klávesy a zvuk | `%APPDATA%\KeyPad\config.json` — vznikne při první změně; nevalidní se odloží jako `config.invalid.json` |
| Data okna (WebView2) | `%LOCALAPPDATA%\cz.hexel.keypad` — cache a data, která si okno ukládá samo (desítky MB) |
| Stažený instalátor | `%TEMP%\keypad-update` — `KeyPadSetup-….exe` z tlačítka Aktualizovat |
| Log instalátoru | `%TEMP%\KeyPadSetup.log` — podrobnosti instalace včetně kódů chyb |

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
  **Klávesnice** (klávesy jdou do Windows). Nikdy naopak.
- Sledovat klávesnici KeyPad začne, až když zapneš ovladač, a s vypnutím
  posledního ovladače přestane. Bez ovladače ji sleduje jen tehdy, když
  je okno KeyPadu v popředí (aby ukázalo stisknuté klávesy) — přepneš-li
  jinam nebo okno schováš, přestane. Stisky kláves nikam neukládá — ani
  do logu.
- Po zamčení počítače (Win+L), výzvě UAC nebo Ctrl+Alt+Del se převádění
  **pozastaví** a držené klávesy se pustí — zpátky ho zapneš Scroll Lockem.
  Totéž, když virtuální ovladač přestane odpovídat (déle než 1 s).
- Kdyby se KeyPad zasekl, Windows ho po chvíli přestanou čekat a klávesy
  jdou normálně do Windows.
- V režimu Gamepad fungují všechny klávesy, které nejsou namapované —
  Alt+Tab, Win, Alt+F4. Win s čímkoli (Win+D, Win+E, Win+Tab) patří
  vždycky Windows, i když je druhá klávesa namapovaná pro hru.
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

**Přírůstek Fáze 6** (editor kláves, víc ovladačů, živé svícení,
konfigurace). Změřeno 3. 10. 2026 stejně jako výš (`tools\tauri.ps1
build --no-bundle`):

| Binárka | Vydání 4b (`0.1.0+20261001.1859`) | Fáze 6 | Rozdíl |
|---|---|---|---|
| KeyPad.exe | 9 863 680 B | 10 088 448 B | +224 768 B (+219,5 KiB) |
| KeyPadSetup.exe | 749 568 B | 749 568 B | 0 |

Konfigurace je JSON (`config.json`) přes `serde_json`, který v binárce
už je: celý modul uložení přidal asi 31 KiB. Crate `toml` by přidal
323 KiB — proto ne `config.toml`, jak plánovala roadmapa (naměřeno
mapou linkeru, podrobnosti v `src-tauri\src\config.rs`). Zbytek přírůstku
je nové UI okna a backend editoru.

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
| `tools\check.ps1` | všechny brány: fmt, clippy, testy, svelte-check, testy okna (`bun test`) — totéž dělá CI (`tools\check.ps1 clippy` = jen jedna) |
| `tools\tauri.ps1 dev` | aplikace ve vývojovém režimu, UI se přenačítá za běhu |
| `tools\publish.ps1` | vydání: kontrola gitu → brány → build → kontroly → `release\` → push |
| `tools\check-imports.ps1 [-RequireDependentLoadFlag] <exe>` | nepotřebuje binárka DLL, která na čistém PC chybí? Nevolá funkci novější než Windows 10 1507? (a s přepínačem: hledá DLL jen v System32?) |
| `cargo run -p keypad --release --example pad_selftest -- vse` | virtuální pad bez okna: připojení, stav přes XInput, odpojení, pád procesu. Když je v popředí hra nebo celoobrazovková aplikace, nic nepřipojí (exit 3). |
| `cargo run -p keypad --release --example hook_selftest` | hook klávesnice bez okna na 60 s: vypisuje stisknuté klávesy se scan kódy **jen do konzole** (nic neukládá); Scroll Lock přepne na Gamepad a WASD, šipky… se potlačí. Virtuální ovladač nepřipojuje. `-- instalace` jen ověří instalaci a odebrání hooku; `-- mereni` změří cenu jedné klávesy (hook se neinstaluje), `-- mereni-instalace` cenu instalace a odebrání hooku (na vlastní skryté ploše). |
| `$env:KEYPAD_BEZ_VIGEM = "1"` | aplikace se chová, jako by ViGEmBus chyběl (`vypnuty` = nainstalovaný a vypnutý, `zbytek` = pozůstatek bez zařízení; `pad` = simulovaný připojený ovladač, jen debug build) — na PC, kde se pad objevit nemá |
| `tools\okno-test.ps1` | okno naostro, ale na **skryté ploše**: debug build, simulovaný ovladač, izolované `APPDATA`/`LOCALAPPDATA`, klávesy jen testovacím příkazem (žádný `SendInput`), přes DevTools projde editor kláves (přiřadit, přidat, vyprázdnit, výchozí, odebrat ovladač, přesun a „Zpět“, Win, Win+klávesa, AltGr, klik jinam, 10 s, minimalizace), klávesy psané do okna (mezerník, Enter, šipky, F5, Ctrl+R, Ctrl+P — jen do okna testu, ne do systému), živé svícení, 380/440 px, restart a poškozenou konfiguraci, prázdnou kartu ovladače 2 po ukončení procesu, přiřazení a Esc s podvrhem „Windows drží všechno“, Esc do okna, který hook nevidí, a v logu řádky „přiřazování skončilo“ (bez kláves) a „raw input klávesnice: ne“ (KeyPad nesmí mít klávesnici v Raw Input, jinak by jeho hook s oknem v popředí podle všeho nic nedostal) a uloží snímky okna (`-Snimky`). Nic neukáže na tvé ploše a nesáhne na nainstalovaný KeyPad. |

**Instalátor — režimy.** `KeyPadSetup.exe` (okno) · `/quiet`
(z aplikace; chybějící nebo starší ovladač nainstaluje či aktualizuje taky) · `/headless` · `/uninstall`
(`/uninstall /quiet` sám začne i skončí) · `/vigembus` (jen ovladač
ViGEmBus; kód 0 = běží, 3010 = poběží po restartu, 1 = jinak).

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

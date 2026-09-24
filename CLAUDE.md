# KeyPad

Windows aplikace, která převádí vybrané klávesy na virtuální Xbox 360 ovladač (ViGEmBus) pro Steam Remote Play Together. Závazná specifikace je **`ROADMAP.md`** — před prací si přečti fázi, na které se dělá, a sekci „Otevřené otázky“.

Jen Windows 10/11 x64; macOS/Linux se neřeší. UI i komentáře česky, v duchu WinSentu (`..\WinSent`): komentář vysvětluje **proč**, ne co.

## Nevyjednatelné principy

1. **Klávesnice se nikdy nesmí „ztratit“.** Každá chyba, panika, výpadek vlákna nebo nejasný stav vede do režimu *Klávesnice* (fail-safe). Nikdy ne naopak.
2. **Pravidlo vlastnictví klávesy:** o tom, kam patří stisk, se rozhoduje **při key-down** a key-up jde vždy stejnému vlastníkovi. Tím vznikají nulové „zaseknuté“ klávesy při přepínání režimu.
3. **Hook callback nikdy neblokuje.** Žádné I/O, žádné logování na disk, žádné volání ViGEm, žádný zámek sdílený s GUI. Windows jinak hook potichu odebere (LowLevelHooksTimeout).
4. **Čistá logika je oddělená od Windows.** `crates/core` nesmí importovat `windows`, `vigem-client` ani `tauri`; `cargo test -p core` běží bez Windows API (hlídá CI).
5. **Mapování podle scan kódů**, ne virtuálních kláves – jinak se rozbije české rozložení QWERTZ (Z/Y, horní řada čísel).
6. **Žádná administrátorská práva** — aplikace ani instalátor nikdy nevyvolají UAC; aplikace nezapisuje do registru. Instalace je per-user (`%LOCALAPPDATA%\Programs\KeyPad`), jediný zápis do registru je záznam v Aplikacích (HKCU) od instalátoru.
7. **Stav gamepadu se vždy přepočítává celý** z množiny držených kláves (čistá funkce), nikdy se inkrementálně nepřičítá/neodečítá.

## Pravidla workspace

| Crate | Balíček / binárka | Smí záviset na |
|---|---|---|
| `crates/core` | balíček `core`, knihovna **`keypad_core`** | jen `serde` (+ `proptest` v testech). **Žádné** `windows` / `vigem-client` / `tauri` |
| `crates/updater` | `updater` | `windows` (WinHttp). Kanál vydání + cesty instalace — **jediný** zdroj pravdy pro instalátor i aplikaci |
| `crates/installer` | `installer` → `KeyPadSetup.exe` | `updater`, `windows` |
| `src-tauri` | `keypad` → `KeyPad.exe` | `keypad-core`, `updater`, `tauri`, `windows`; Windows kód hooku/padu ve Fázi 2+ do `src/platform/windows/` |
| `ui/` | Svelte 5 + Vite + TypeScript | **Ne SvelteKit**, žádný router — jedno okno |

- Balíček `core` má knihovnu pojmenovanou `keypad_core`: crate se jménem `core` by v závislých crates zastínil vestavěný `::core` a rozbil makra (serde derive, `format_args!`…). V `Cargo.toml` závislých crates: `keypad-core = { workspace = true }`.
- **Engine je čistý stavový automat**: událost dovnitř, `Decision { suppress, pad, ui }` ven. Žádné I/O, žádná vlákna, žádné časovače — čas (timeout přiřazování) jde dovnitř jako parametr `now_ms` (monotónní ms, v hooku `GetTickCount64()`); timeout hlídá `Engine::tick(now_ms)` volané časovačem hook vlákna.
- `Mapping` je **vždy platné** — nevalidní hodnotu nejde vyrobit (`Mapping::new` vrací všechny chyby, úpravy jsou všechno-nebo-nic).
- Stav padu: vždy `compute_pad_state` z držených kláves (`Owner::Pad(akce)` + `seq`). Nikdy nenegovat `i16::MIN`; kladné Y = nahoru (XInput); diagonála 23170; autorepeat nikdy nic nespouští.
- Na cestě `on_key` nic nealokovat ani nehashovat: držené klávesy i mapování jsou pevné tabulky 256 položek (`KeyId::index`). Nemapovatelné klávesy engine nesleduje a vždy je propouští.
- `Engine::new` startuje v `Disabled { PadNotConnected }` — na Klávesnici ho pustí až `enable()` od pad vlákna. Režim je zdroj pravdy (`Engine::mode()`), `UiEvent` je jen oznámení (nejvýš jedno na `Decision`).
- Panika v hook callbacku → propustit klávesu + `reset_held(HookPanic)`, **ne** `force_keyboard` (jinak visí propuštěná klávesa v OS). Změny `crates/core` ověřuj i `PROPTEST_CASES=20000 cargo test -p core` — property test porovnává engine s nezávislým referenčním modelem.
- GUI → backend jen přes Tauri commands; backend → GUI přes Tauri events. Z hook callbacku nikdy přímo `emit` (princip 3) — hook zapíše stav do atomik / pošle do kanálu a event vydá jiné vlákno.
- `src-tauri/capabilities/default.json` je **výčet** oprávnění, ne `core:default`. Každé nové JS API okna potřebuje své oprávnění (jinak „not allowed by ACL“); poslech událostí z backendu (Fáze 2+) = `core:event:allow-listen` + `core:event:allow-unlisten`. Vlastní příkazy aplikace oprávnění nepotřebují.
- `tauri.conf.json` má CSP (`default-src 'self'` …) — externí URL a `data:` v release tiše selžou (dev na devUrl CSP nemá). Fonty a obrázky vždy lokálně.
- Instalátor zavírá KeyPad zprávou WM_CLOSE oknu třídy **„Tauri Window“** (výchozí z tauri-runtime-wry; `taskkill` bez /F by trefil skryté okno pluginu). Kdyby se v `tauri.conf.json` nastavil `windowClassname`, upravit `crates/installer/src/proc.rs`. WM_CLOSE musí aplikaci vždy opravdu ukončit (žádné „schovat do tray“).
- Logování: `log::…` makra. Logger zapisuje z vlastního vlákna a na disk nikdy nečeká. **Z hook callbacku ale `log::` nevolat**: `format!` alokuje a `try_send` crossbeamu si může krátce vzít vnitřní zámek sdílený s ostatními vlákny (princip 3) — hook předá pevný záznam jinému vláknu a loguje až to. Vlákna, která nesmí čekat (hook), volají `logger::mark_realtime_thread()` — panic hook na nich pak nečeká na flush.
- Obě binárky se linkují s `/DEPENDENTLOADFLAG:0x800` (statické importy DLL jen ze System32 — jinak by `KeyPadSetup.exe` spuštěný ze Stažených souborů načetl podvrženou `dwmapi.dll`/`winhttp.dll`, ověřeno revizí). `tools\check-imports.ps1` to v publish i CI vyžaduje. Pomocné programy (`taskkill`, `cmd`, `explorer`) spouštět plnou cestou, nikdy jménem.
- Aktualizace se v aplikaci nabízí jen **novější** verze (`updater::is_newer`) a jen kopii běžící z instalační složky; `raw` CDN drží `version.txt` až 5 min, takže „jiná" by hned po aktualizaci nabízela tu předchozí.
- Release profil má `panic = "unwind"` a **musí** ho mít: hook callback (Fáze 3) běží v `catch_unwind`.
- Nové závislosti přidávej přes `[workspace.dependencies]` v kořenovém `Cargo.toml`.
- Release build aplikace **jen přes Tauri CLI** (`tools\tauri.ps1 build --no-bundle`). Holý `cargo build` vestaví jen adresu vývojového serveru — nainstalovaná aplikace by ukázala „localhost se odmítl připojit“.
- PowerShell skripty v `tools\` musí zůstat v **UTF-8 s BOM** (PowerShell 5.1), jinak se rozsype diakritika.
- Nejasnost ve specifikaci se nerozhoduje potichu — zapiš ji do „Otevřených otázek“ v `ROADMAP.md` (i s tím, jak to kód dělá teď).

## Příkazy

```powershell
# testy čisté logiky (rychlé, bez Windows API)
cargo test -p core

# celý workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all

# všechny brány najednou (fmt, clippy, testy, svelte-check) — před každým vydáním
powershell -ExecutionPolicy Bypass -File tools\check.ps1

# frontend
cd ui; bun install; bun run check; bun run build

# aplikace ve vývojovém režimu (okno + HMR)
powershell -ExecutionPolicy Bypass -File tools\tauri.ps1 dev

# release binárky
powershell -ExecutionPolicy Bypass -File tools\tauri.ps1 build --no-bundle   # target\release\KeyPad.exe
cargo build --release -p installer                                            # target\release\KeyPadSetup.exe

# žádná závislost na vcruntime140.dll apod. + DLL jen ze System32
powershell -ExecutionPolicy Bypass -File tools\check-imports.ps1 -RequireDependentLoadFlag target\release\KeyPad.exe target\release\KeyPadSetup.exe

# vydání: zdroják musí být commitnutý A pushnutý (publish to hlídá) →
# brány → build (binárky se před buildem mažou) → kontroly → release\ →
# commit JEN release\ „release: <verze> (<sha zdrojáku>)" → push
powershell -ExecutionPolicy Bypass -File tools\publish.ps1
```

CI (`.github/workflows/build.yml`) dělá kroky 1–5 publish.ps1 (app přes `tools/tauri.ps1 build --no-bundle -- --locked`) a nahraje binárky jako artefakt.

Instalátor: `KeyPadSetup.exe` (okno) · `/quiet` (z aplikace; zavře se sám jen při čistém úspěchu — když se KeyPad nespustí, chybí WebView2 nebo je co hlásit, okno zůstane) · `/headless` (konzole) · `/uninstall`. Je to GUI binárka — ze skriptu `start "" /wait KeyPadSetup.exe /headless` (nebo `Start-Process -Wait -PassThru`), jinak se na ni nečeká a exit kód se ztratí. Vydání čte z `release/` v repu `iva-exe/KeyPad` (konstanty v `crates/updater/src/lib.rs`).

Testování GUI: okna aplikace ani instalátoru nespouštět na ploše vlastníka a nesimulovat vstup (může mít spuštěnou hru) — na samostatné skryté ploše (`CreateDesktop` + `STARTUPINFO.lpDesktop`).

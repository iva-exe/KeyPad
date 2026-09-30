# KeyPad

Windows aplikace, která převádí vybrané klávesy na virtuální Xbox 360 ovladač (ViGEmBus) pro Steam Remote Play Together. Závazná specifikace je **`ROADMAP.md`** — před prací si přečti fázi, na které se dělá, a sekci „Otevřené otázky“.

Jen Windows 10/11 x64; macOS/Linux se neřeší. UI i komentáře česky, v duchu WinSentu (`..\WinSent`): komentář vysvětluje **proč**, ne co.

## Nevyjednatelné principy

1. **Klávesnice se nikdy nesmí „ztratit“.** Každá chyba, panika, výpadek vlákna nebo nejasný stav vede do režimu *Klávesnice* (fail-safe). Nikdy ne naopak.
2. **Pravidlo vlastnictví klávesy:** o tom, kam patří stisk, se rozhoduje **při key-down** a key-up jde vždy stejnému vlastníkovi. Tím vznikají nulové „zaseknuté“ klávesy při přepínání režimu.
3. **Hook callback nikdy neblokuje.** Žádné I/O, žádné logování na disk, žádné volání ViGEm, žádný zámek sdílený s GUI. Windows jinak hook potichu odebere (LowLevelHooksTimeout).
4. **Čistá logika je oddělená od Windows.** `crates/core` nesmí importovat `windows`, `vigem-client` ani `tauri`; `cargo test -p core` běží bez Windows API (hlídá CI).
5. **Mapování podle scan kódů**, ne virtuálních kláves – jinak se rozbije české rozložení QWERTZ (Z/Y, horní řada čísel).
6. **Žádná administrátorská práva** — aplikace ani instalátor s nimi nikdy neběží; aplikace nezapisuje do registru. Instalace je per-user (`%LOCALAPPDATA%\Programs\KeyPad`), jediný zápis do registru je záznam v Aplikacích (HKCU) od instalátoru. **Jediná výjimka:** ovladač ViGEmBus — KeyPadSetup ho **automaticky nainstaluje, když chybí, a aktualizuje, když je prokazatelně starší** než 1.21.442 (i při aktualizaci z aplikace); aktuální, neznámý nebo na restart čekající ovladač nikdy nepřeinstalovává. Souhlas = výzva UAC od Windows. S právy správce běží výhradně oficiální podepsaný instalátor ViGEmBus 1.22.0 ověřený napevno zapsaným SHA-256 + podpisem (z repa autora, záložně ze zrcadla `mirror/` v našem repu — stejný otisk). Podrobnosti ROADMAP, princip 6.
11. **Virtuální ovladač jen na povel uživatele** — nikdy se nepřipojí sám po startu, probuzení ani aktualizaci. KeyPad se nikdy nespouští s Windows. Před vypnutím / restartem / odhlášením PC se ovladač odpojí a aplikace skončí; před uspáním se ovladač vypne a zůstane vypnutý.
7. **Stav gamepadu se vždy přepočítává celý** z množiny držených kláves (čistá funkce), nikdy se inkrementálně nepřičítá/neodečítá.
8. **Stejně bezpečný jako WinSent — nic agresivního v systému.** Žádné „optimalizace“, zásahy do registru mimo vlastní záznam v HKCU, služby, naplánované úlohy, zásahy do cizích procesů, firewallu ani nastavení Windows. Každou změnu systému spouští uživatel vlastním kliknutím, aplikace ji vysvětlí předem a ověří potom (plán → provedení → ověření). Nikdy se neskrývá (vlastní procesy, soubory, log jsou vidět) a nikdy nepředstírá záruku, kterou nemá. Cizí binárky se spouštějí jen v přesně ověřené podobě (napevno zapsaný SHA-256 + podpis).
9. **Všechny verze Windows 10 a 11 (x64).** Testuj schopnost, ne verzi (WinSent INFRA 1.1): nové API volat až po ověření, že existuje (dynamicky přes `GetProcAddress`), nikdy ho staticky neimportovat, pokud by starší build nenaběhl („entry point not found“). Funkce Windows 11 (zaoblené rohy…) musí na desítkách tiše odpadnout.
10. **Co nejlehčí a nejrychlejší.** Nečinná aplikace nesmí brát měřitelné CPU (žádné zbytečné pollování — události a čekání), minimum paměti, rychlý start, malé binárky, žádné těžké závislosti. Výkon se měří, ne slibuje.

## Pravidla workspace

| Crate | Balíček / binárka | Smí záviset na |
|---|---|---|
| `crates/core` | balíček `core`, knihovna **`keypad_core`** | jen `serde` (+ `proptest` v testech). **Žádné** `windows` / `vigem-client` / `tauri` |
| `crates/updater` | `updater` | `windows` (WinHttp, CNG, cfgmgr32, registr — jen čtení). Kanál vydání, cesty instalace, `vigembus` (pinned instalátor + stav ovladače), `sha256` — **jediný** zdroj pravdy pro instalátor i aplikaci |
| `crates/installer` | `installer` → `KeyPadSetup.exe` | `updater`, `windows` |
| `src-tauri` | `keypad` → `KeyPad.exe` | `keypad-core`, `updater`, `tauri`, `windows`, `webview2-com` (jen paměť schovaného WebView, verze = ta z wry); Windows kód v `src/platform/windows/` (`hook.rs` hook klávesnice + engine, `klavesy.rs` názvy kláves, `vystup.rs` rozhodnutí enginu → sloty padů a režim okna, `slot.rs` stav padu bez zámku, `vigem.rs` vlastní klient ViGEmBus, `pad.rs` pad vlákna, `power.rs` spánek, `relace.rs` konec relace Windows, `ukonceni.rs` událost pro ukončení, `shell.rs`) |
| `ui/` | Svelte 5 + Vite + TypeScript | **Ne SvelteKit**, žádný router — jedno okno |

- Balíček `core` má knihovnu pojmenovanou `keypad_core`: crate se jménem `core` by v závislých crates zastínil vestavěný `::core` a rozbil makra (serde derive, `format_args!`…). V `Cargo.toml` závislých crates: `keypad-core = { workspace = true }`.
- **Engine je čistý stavový automat**: událost dovnitř, `Decision { suppress, pads, ui }` ven (`pads` = změněné stavy až 4 ovladačů). Žádné I/O, žádná vlákna, žádné časovače — čas (timeout přiřazování) jde dovnitř jako parametr `now_ms` (monotónní ms, v hooku `GetTickCount64()`); timeout hlídá `Engine::tick(now_ms)` volané časovačem hook vlákna.
- `Mapping` je **vždy platné** — nevalidní hodnotu nejde vyrobit (`Mapping::new` vrací všechny chyby, úpravy jsou všechno-nebo-nic).
- Stav padu: vždy `compute_pad_state` z držených kláves daného ovladače (`Owner::Pad(PadAction)` + `seq`); klávesa patří nejvýš jednomu ovladači a jde mu jen tehdy, když je připravený (`enable(pad)`). Nikdy nenegovat `i16::MIN`; kladné Y = nahoru (XInput); diagonála 23170; autorepeat nikdy nic nespouští.
- Na cestě `on_key` nic nealokovat ani nehashovat: držené klávesy i mapování jsou pevné tabulky 256 položek (`KeyId::index`). Nemapovatelné klávesy engine nesleduje a vždy je propouští.
- `Engine::new` startuje v `Disabled { PadNotConnected }` — na Klávesnici ho pustí až `enable(pad)` (připojený ovladač), na Gamepad až `capture()` (povel přepínače) nebo zkratka. Režim je zdroj pravdy (`Engine::mode()`), `UiEvent` je jen oznámení (nejvýš jedno na `Decision`).
- Panika v hook callbacku → propustit klávesu + `reset_held(HookPanic)`, **ne** `force_keyboard` (jinak visí propuštěná klávesa v OS). Změny `crates/core` ověřuj i `PROPTEST_CASES=20000 cargo test -p core` — property test porovnává engine s nezávislým referenčním modelem.
- GUI → backend jen přes Tauri commands; backend → GUI přes Tauri events. Z hook callbacku nikdy přímo `emit` (princip 3) — hook zapíše stav do atomik a nastaví událost Windows (`slot::Budik`), event vydá jiné vlákno (`keypad-rezim`).
- **Hook → pad jen přes `slot::StavSlot`** (dva atomiky s číslem zápisu + auto-reset událost), nikdy frontou crossbeamu (bere zámek sdílený s GUI a alokuje). Pad vlákno čeká na tutéž událost i kvůli příkazům — posílat mu jen přes `PadOdesilatel`/`Pady`, které ji po `send` nastaví.
- **Každý virtuální ovladač má vlastní pad vlákno** (`Pady`, až 4, vznikají až prvním zapnutím; první hned kvůli ověření sběrnice) — `wait_ready` jednoho (až 3 s) nesmí zdržet stavy ostatních. Globální příkazy (spánek, instalátor, konec) jdou všem přes `Pady`.
- **Hook klávesnice je v systému, jen když je zapnutý aspoň jeden ovladač** (engine mimo `Disabled`); jinak KeyPad na klávesnici nesahá. Připojení ovladače (přechod na `On`, vždy po kliknutí na přepínač) = `Povol(pad)` + `Zachytavej`; zkratka zachytávání jen pozastaví. Opakované ohlášení téhož stavu zachytávání nespouští.
- `src-tauri/capabilities/default.json` je **výčet** oprávnění, ne `core:default`. Každé nové JS API okna potřebuje své oprávnění (jinak „not allowed by ACL“); poslech událostí z backendu (Fáze 2+) = `core:event:allow-listen` + `core:event:allow-unlisten`. Vlastní příkazy aplikace oprávnění nepotřebují.
- `tauri.conf.json` má CSP (`default-src 'self'` …) — externí URL a `data:` v release tiše selžou (dev na devUrl CSP nemá). Fonty a obrázky vždy lokálně.
- Zavření okna KeyPad jen **schová do oznamovací oblasti** a uspí WebView (jako WinSent); ukončit jde z menu v trayi. Instalátor proto ukončuje KeyPad pojmenovanou událostí `updater::QUIT_EVENT_NAME` (uklizený konec: neutrální pad → odpojení), teprve jako záloha WM_CLOSE oknu třídy **„Tauri Window“** (pro vydání 0.1.0 bez události; kdyby se v `tauri.conf.json` nastavil `windowClassname`, upravit `crates/installer/src/proc.rs`) a nakonec `taskkill /F`.
- Konec relace Windows: skryté okno nejvyšší úrovně `KeyPad.KonecRelace` (`platform/windows/relace.rs`, ignoruje WM_CLOSE) na `WM_ENDSESSION(TRUE)` synchronně odpojí pad a ukončí aplikaci; `RunEvent::Exit` končí `cleanup_before_exit()` + `process::exit(0)` (jinak tao po WM_ENDSESSION zpanikaří).
- Pozadí okna volí `okno::nastav_pozadi` podle buildu (RtlGetVersion): Windows 10 blur jako WinSent, Windows 11 Mica; `tauri.conf.json` má `theme: Dark` (jinak by tao Micu ve světlém motivu zesvětlil).
- Okno se do popředí dostává přes `SetForegroundWindow`, **nikdy** přes `set_focus()` z tao — ten při odepřeném popředí vstříkne do systému falešný stisk Alt (SendInput), který by dopadl do běžící hry (princip 8).
- Logování: `log::…` makra. Logger zapisuje z vlastního vlákna a na disk nikdy nečeká. **Z hook callbacku ale `log::` nevolat**: `format!` alokuje a `try_send` crossbeamu si může krátce vzít vnitřní zámek sdílený s ostatními vlákny (princip 3) — hook předá pevný záznam jinému vláknu a loguje až to. Vlákna, která nesmí čekat (hook), volají `logger::mark_realtime_thread()` — panic hook na nich pak nečeká na flush.
- Obě binárky se linkují s `/DEPENDENTLOADFLAG:0x800` (statické importy DLL jen ze System32 — jinak by `KeyPadSetup.exe` spuštěný ze Stažených souborů načetl podvrženou `dwmapi.dll`/`winhttp.dll`, ověřeno revizí). `tools\check-imports.ps1` to v publish i CI vyžaduje. Pomocné programy (`taskkill`, `cmd`, `explorer`) spouštět plnou cestou, nikdy jménem.
- **První příkaz `main()` obou binárek je `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)`** — DLL, které si systémové knihovny načítají za běhu (WinVerifyTrust → CRYPTSP, WinHttp → IPHLPAPI, SHGetKnownFolderPath → profapi), by jinak šly ze složky programu. Ověřeno podstrčenými DLL.
- Systémové DLL, které si při inicializaci samy načítají další ne-KnownDLL (např. `powrprof.dll` → `UMPDC.dll` ještě **před** `main`), se **nesmí** importovat staticky — načítají se za běhu přes `LoadLibraryExW(…, LOAD_LIBRARY_SEARCH_SYSTEM32)` (viz `platform/windows/power.rs`). Nová statická DLL = ověřit podstrčenou DLL (harness ve scratchpadu revizí, `tools\check-imports.ps1`).
- Nová API: minimum je Windows 10 **1507** — `check-imports.ps1` kontroluje i jednotlivé funkce, API sety a zpožděné importy. Novější funkce jen dynamicky (`GetProcAddress`), viz `GetDpiForWindow` v instalátoru.
- Aktualizace se v aplikaci nabízí jen **novější** verze (`updater::is_newer`) a jen kopii běžící z instalační složky; `raw` CDN drží `version.txt` až 5 min, takže „jiná" by hned po aktualizaci nabízela tu předchozí.
- Release profil má `panic = "unwind"` a **musí** ho mít: hook callback (Fáze 3) běží v `catch_unwind`.
- Nové závislosti přidávej přes `[workspace.dependencies]` v kořenovém `Cargo.toml`.
- Release build aplikace **jen přes Tauri CLI** (`tools\tauri.ps1 build --no-bundle`). Holý `cargo build` vestaví jen adresu vývojového serveru — nainstalovaná aplikace by ukázala „localhost se odmítl připojit“.
- PowerShell skripty v `tools\` musí zůstat v **UTF-8 s BOM** (PowerShell 5.1), jinak se rozsype diakritika.
- **Zkratky KeyPadu nikdy s klávesou Win** (vlastník 30. 9.: Win+L zamyká počítač); potřebuje-li zkratka modifikátor, je to Alt. Win nejde přiřadit ani akci ovladače.
- **Žádné okno přes hru v popředí** (překryv, toast, vyskakovací okno) — znamení stavu jen ikonou v oznamovací oblasti a zvukem.
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

```powershell
# virtuální pad bez okna (vlastní klient ViGEmBus + kontrola přes XInput); exit 3 = v popředí
# je celoobrazovková aplikace/hra → nic se nepřipojí. Posílá jen hodnoty pod mrtvou zónou.
cargo run -p keypad --release --example pad_selftest -- vse     # nebo e2e | kill | popredi | xinput

# hook klávesnice bez okna (spouští VLASTNÍK: vypisuje stisky do konzole, Scroll Lock potlačí WASD…);
# `instalace` = jen nainstalovat, přeinstalovat, odebrat — nic o klávesách nevypisuje
cargo run -p keypad --release --example hook_selftest -- 60      # nebo instalace

# aplikace bez ViGEmBus (simulace): 1 = nenainstalovaný, vypnuty = nainstalovaný a vypnutý,
# zbytek = pozůstatek bez zařízení i záznamu v Aplikacích; bez proměnné = skutečný ovladač
$env:KEYPAD_BEZ_VIGEM = "1"
# simulace staršího ViGEmBus (nabídka aktualizace); s kteroukoli z nich aplikace instalátor nespustí
$env:KEYPAD_VIGEM_STARY = "1"
```

Pozor při testech: na PC vlastníka může běžet jeho nainstalovaný KeyPad — nikdy nenastavuj skutečnou událost `Local\KeyPad.Ukoncit` (sdílená, auto-reset) a nesahej na jeho proces; na skryté ploše single-instance cizí okno nenajde a spustí se vedle něj.

CI (`.github/workflows/build.yml`) dělá kroky 1–5 publish.ps1 (app přes `tools/tauri.ps1 build --no-bundle -- --locked`) a nahraje binárky jako artefakt.

Instalátor: `KeyPadSetup.exe` (okno; ViGEmBus nainstaluje / aktualizuje automaticky, viz princip 6; podrobnosti a kódy do `%TEMP%\KeyPadSetup.log`, okno jen krátké věty) · `/quiet` (z aplikace; ovladač také instaluje/aktualizuje; zavře se sám jen při čistém úspěchu — když se KeyPad nespustí, chybí WebView2 nebo je co hlásit, okno zůstane) · `/headless` (konzole) · `/uninstall` (`/uninstall /quiet` = sám začne i skončí; ViGEmBus nechává) · `/vigembus` (jen krok ovladače, spouští ho aplikace; `/quiet` se s ním ignoruje, `/uninstall` má přednost; exit kód podle ověřeného stavu: 0 = běží, 3010 = poběží po restartu, 1 = jinak). Ladicí přepínače jen v debug buildu: `KEYPAD_SETUP_TEST_VIGEMBUS`, `KEYPAD_SETUP_TEST_NAHLED`, `KEYPAD_SETUP_TEST_FOKUS`, `KEYPAD_SETUP_TEST_KNIHOVNY` (viz `crates/installer/src/main.rs`). **Z Git Bashe nikdy nespouštěj KeyPadSetup s lomítkovými přepínači bez `MSYS_NO_PATHCONV=1`** — MSYS z `/headless` udělá cestu a otevře se okno na ploše (stalo se); bezpečně přes PowerShell `Start-Process -ArgumentList`. Je to GUI binárka — ze skriptu `start "" /wait KeyPadSetup.exe /headless` (nebo `Start-Process -Wait -PassThru`), jinak se na ni nečeká a exit kód se ztratí. Vydání čte z `release/` v repu `iva-exe/KeyPad` (konstanty v `crates/updater/src/lib.rs`).

Testování GUI: okna aplikace ani instalátoru nespouštět na ploše vlastníka a nesimulovat vstup (může mít spuštěnou hru) — na samostatné skryté ploše (`CreateDesktop` + `STARTUPINFO.lpDesktop`).

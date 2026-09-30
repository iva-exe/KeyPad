# KeyPad – roadmapa pro Claude Code

## Cíl

Jednoduchá Windows aplikace **KeyPad**, která na počítači **hosta streamu (kamaráda)** převádí vybrané klávesy na **virtuální Xbox 360 ovladač**. Přepíná se tlačítkem v okně a klávesovou zkratkou. Steam klient kamaráda tento ovladač přepošle hostiteli Remote Play Together jako samostatného hráče.

```
[klávesnice kamaráda] → [KeyPad: LL hook → engine → ViGEmBus] → [virtuální Xbox pad]
        → [Steam klient kamaráda] → Remote Play → [hra u hostitele: hráč 2 = gamepad]
```

Distribuce: jeden soubor **`KeyPadSetup.exe`** (stejný model jako WinSent) — stáhne aktuální `KeyPad.exe` z GitHubu a nainstaluje ho **do profilu uživatele bez práv správce**. Aplikace sama hlásí novou verzi a aktualizuje se jedním kliknutím.

Externí závislosti u kamaráda: ovladač **ViGEmBus** (github.com/nefarius/ViGEmBus, poslední release; projekt je archivovaný, ale na Windows 10/11 funkční) a **Microsoft Edge WebView2 Runtime** (Windows 11 ho má, Windows 10 skoro vždy s Edgem; instalátor chybějící runtime pozná a pošle odkaz — viz `README_CZ.md`).

Jen Windows 10/11 x64. macOS ani Linux se neřeší.

---

## Nevyjednatelné principy (zkopírováno do `CLAUDE.md`)

1. **Klávesnice se nikdy nesmí „ztratit“.** Každá chyba, panika, výpadek vlákna nebo nejasný stav vede do režimu *Klávesnice* (fail-safe). Nikdy ne naopak.
2. **Pravidlo vlastnictví klávesy:** o tom, kam patří stisk, se rozhoduje **při key-down** a key-up jde vždy stejnému vlastníkovi. Tím vznikají nulové „zaseknuté“ klávesy při přepínání režimu.
3. **Hook callback nikdy neblokuje.** Žádné I/O, žádné logování na disk, žádné volání ViGEm, žádný zámek sdílený s GUI. Windows jinak hook potichu odebere (LowLevelHooksTimeout).
4. **Čistá logika je oddělená od Windows.** Crate `crates/core` nesmí importovat `windows`, ViGEm klienta ani `tauri`; `cargo test -p core` musí běžet bez jakéhokoli Windows API (hlídá CI).
5. **Mapování podle scan kódů**, ne virtuálních kláves – jinak se rozbije české rozložení QWERTZ (Z/Y, horní řada čísel).
6. **Žádná administrátorská práva** — aplikace ani instalátor nikdy nevyvolají UAC. Aplikace nezapisuje do registru. Instalaci a aktualizace dělá `KeyPadSetup.exe` **per-user** (`%LOCALAPPDATA%\Programs\KeyPad`); jediný zápis do registru je záznam v Aplikacích (`HKCU\…\Uninstall\KeyPad`), a ten dělá instalátor, ne aplikace.
   _(Změna oproti původní verzi, kde stálo „žádný instalátor“: vlastník chce stejný instalátor a updater jako u WinSentu. Per-user instalace drží zbytek principu — žádná admin práva.)_
   **Jediná výjimka (29. 9. 2026, na přání vlastníka): ovladač ViGEmBus z KeyPadSetup.** Ovladač jádra bez práv správce nainstalovat nejde. KeyPadSetup ho **automaticky nainstaluje, když chybí, a aktualizuje, když je prokazatelně starší** než ovladač 1.21.442 (i při aktualizaci z aplikace); aktuální, neznámý nebo na restart čekající ovladač nikdy nepřeinstalovává a před aktualizací ovladače KeyPad slušně ukončí (nesmí držet sběrnici). Souhlas = výzva UAC od Windows. S právy správce běží **výhradně** oficiální podepsaný instalátor ViGEmBus 1.22.0 z repa nefarius/ViGEmBus, ověřený napevno zapsaným SHA-256 a podpisem; výzvu UAC ukazuje Windows. KeyPad ani KeyPadSetup samy s právy správce nikdy neběží. Proč to není „stahování ovladačů z webu“, které WinSent zakazuje: jde o jediný přesně známý soubor (hash v naší binárce, finální vydání archivovaného projektu — nikdy se nezmění), totéž, co dělá winget.
7. **Stav gamepadu se vždy přepočítává celý** z množiny držených kláves (čistá funkce), nikdy se inkrementálně nepřičítá/neodečítá.
8. **Stejně bezpečný jako WinSent — nic agresivního v systému.** Žádné tweaky, zásahy do registru mimo vlastní záznam v HKCU, služby, naplánované úlohy, zásahy do cizích procesů ani nastavení Windows. Každou změnu systému spouští uživatel kliknutím, aplikace ji předem vysvětlí a potom ověří. Nic se neskrývá, nepředstírají se záruky, které nemáme. Cizí binárky jen v přesně ověřené podobě (pinned SHA-256 + podpis). _(Přidáno 29. 9. 2026 na přání vlastníka.)_
9. **Všechny verze Windows 10 a 11 (x64).** Testuj schopnost, ne verzi; žádné statické importy API novějších, než je podporovaný minimální build; funkce Windows 11 na desítkách tiše odpadnou. _(29. 9. 2026)_
10. **Co nejlehčí a nejrychlejší.** Nečinnost = žádné měřitelné CPU, minimum paměti, rychlý start, žádné zbytečné pollování ani těžké závislosti; výkon se měří. _(29. 9. 2026)_

---

## Stack

| Účel | Crate / nástroj |
|---|---|
| Win32 API (hook, zprávy, WTS) | `windows` 0.61 (features podle potřeby: `Win32_UI_WindowsAndMessaging`, `Win32_UI_Input_KeyboardAndMouse`, `Win32_System_Threading`, `Win32_System_RemoteDesktop`, `Win32_System_LibraryLoader`, …) |
| Virtuální gamepad | vlastní klient ViGEmBus `src-tauri/src/platform/windows/vigem.rs` (IOCTL nad `windows`, časové limity; `vigem-client` zamítnut — viz Fáze 2) |
| GUI | **Tauri 2**, frontend **Svelte 5 + Vite + TypeScript** (ne SvelteKit — jedno okno, bez routingu), vzhled podle WinSentu (tokeny z `app.css`, Space Grotesk + Fira Mono, vlastní titlebar) |
| GUI → backend | Tauri commands |
| Backend → GUI | Tauri events (`AppHandle::emit`) — náhrada za `egui::Context::request_repaint()` |
| Jedna instance | `tauri-plugin-single-instance` (druhé spuštění ukáže okno běžící instance) |
| Konfigurace | `serde` + `toml` |
| Kanály mezi vlákny | `crossbeam-channel` |
| Logování | `log` + vlastní souborový logger (neblokující, přes kanál, vlastní vlákno) |
| Aktualizace | crate `updater`: WinHttp (součást Windows, žádná TLS knihovna), zdroj `release/` v repu `iva-exe/KeyPad` |
| Testy | `cargo test`, `proptest` |

Nastavení buildu:

- `.cargo/config.toml` → `[target.x86_64-pc-windows-msvc] rustflags = ["-C", "target-feature=+crt-static"]`, aby na kamarádově PC nechybělo `vcruntime140.dll`. Co by Tauri zvládlo samo, je **změřené** v `README_CZ.md` („Závislosti binárky“); `tools\check-imports.ps1` to hlídá v `publish.ps1` i v CI.
- `src-tauri/src/main.rs` → `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` (release bez konzole).
- Release profil: `panic = "unwind"` (výchozí) — **ne** `abort`: hook callback běží v `catch_unwind` (Fáze 3).
- Release build aplikace **jen přes Tauri CLI** (`tools\tauri.ps1 build --no-bundle`) — holý `cargo build` vestaví jen adresu vývojového serveru a nainstalovaná aplikace by ukázala „localhost se odmítl připojit“. `publish.ps1` to kontroluje.
- CI: GitHub Actions `windows-latest` (žádný macOS).

---

## Architektura

### Vlákna

| Vlákno | Vlastní | Úkol |
|---|---|---|
| **GUI** (hlavní, Tauri + WebView2) | okno, commands | Zobrazení stavu, tlačítko přepnutí, editor mapování, náhled gamepadu. Příkazy posílá hook vláknu. |
| **Hook** | `Engine`, LL hook, message-only okno | `SetWindowsHookExW(WH_KEYBOARD_LL)`, smyčka `GetMessageW`, rozhodování o potlačení kláves, zkratka přepnutí, WTS notifikace, časovač pro `Engine::tick`. |
| **Pad** | ViGEm `Client` + target | Přijímá `PadState`, vyprázdní frontu na nejnovější hodnotu a pošle ho do ViGEm. Zapisuje heartbeat. |
| **Logger** | soubor `keypad.log` | Jediné místo, které píše na disk; ostatní mu posílají zprávy přes kanál (`try_send`, nikdy neblokuje). |

Komunikace:

- GUI → Hook: Tauri command → `crossbeam` kanál s příkazy + `PostThreadMessageW(WM_APP)` pro probuzení smyčky.
- Hook → Pad: kanál `PadState` (pad vlákno vždy zpracuje jen poslední hodnotu).
- Hook/Pad → GUI: `Arc<Status>` (atomiky + `Mutex` jen pro text chyby, nikdy nezamykaný v hooku) + Tauri event (`emit`) z GUI-strany, nikdy přímo z hook callbacku.
- Pad → Hook: `AtomicU64 last_heartbeat_ms`.

### Struktura (Cargo workspace)

```
Cargo.toml                    // workspace
crates/
  core/                       // balíček `core`, knihovna `keypad_core` — BEZ Windows/ViGEm/Tauri
    src/key.rs                // KeyId { scan: u16, extended: bool } + pojmenované klávesy
    src/action.rs             // Action, PadButton, StickDir
    src/mapping.rs            // Mapping (vždy platné) + MappingError, výchozí mapování
    src/engine.rs             // Mode, Owner, Engine, Decision, UiEvent
    src/pad_state.rs          // PadState + compute_pad_state()
    tests/engine_props.rs     // proptest
  updater/                    // kanál vydání na GitHubu (WinHttp) + cesty instalace
  installer/                  // KeyPadSetup.exe (per-user instalace, aktualizace, odinstalace)
src-tauri/                    // aplikace KeyPad.exe (Tauri 2)
  src/main.rs, logger.rs, …
  src/platform/windows/       // hook.rs (Fáze 3), klavesy.rs (názvy kláves), pad.rs, vigem.rs, power.rs, relace.rs…
  src/config.rs               // (Fáze 7) načtení, validace, atomický zápis
ui/                           // Svelte 5 + Vite + TypeScript
tools/                        // check.ps1, tauri.ps1, publish.ps1, check-imports.ps1
release/                      // co si stahuje instalátor (plní publish.ps1)
```

Crate `core` se jmenuje jako balíček `core` (příkazy `cargo test -p core`), ale knihovna je `keypad_core`: crate pojmenovaný `core` by v závislých crates zastínil vestavěný `::core` a rozbil makra, která na něj odkazují.

### Klíčové typy (skutečné, `crates/core`)

```rust
struct KeyId { scan: u16, extended: bool }        // z KBDLLHOOKSTRUCT (LLKHF_EXTENDED)
enum Action { Button(PadButton), LeftStick(StickDir), RightStick(StickDir), LeftTrigger, RightTrigger }
enum Mode { Keyboard, Gamepad, Binding { action, started_at_ms: u64 }, Disabled { reason: DisabledReason } }
enum Owner { Os, Pad(Action), Swallow }           // Pad nese akci určenou při key-down
struct HeldKey { owner: Owner, seq: u64, last_ms: u64 }  // seq = pořadí stisku (SOCD), last_ms = ztracený key-up

fn Engine::on_key(&mut self, key: KeyId, down: bool, now_ms: u64) -> Decision
struct Decision { suppress: bool, pad: Option<PadState>, ui: Option<UiEvent> }
// příkazy: toggle(now_ms), start_binding(action, now_ms), cancel_binding(now_ms), tick(now_ms),
//          force_keyboard(reason), reset_held(reason), disable(reason), enable(), set_mapping(m)
```

Čas je parametr (`now_ms` = monotónní ms; v hooku čas události rozšířený na 64 bitů nebo `GetTickCount64()`), engine nemá časovače ani I/O. `Mapping` je vždy platné — nevalidní hodnotu nejde vyrobit. Držené klávesy i mapování jsou pevné tabulky o 256 položkách (index = scan kód + bit E0): v hook callbacku se nic nehashuje a nic nealokuje. Všechny typy mají `Serialize`/`Deserialize` (pro Tauri events a konfiguraci; mapování se ukládá jako **seznam** `{klávesa, akce}`, ne jako mapa podle `KeyId`).

### Pravidla engine (jádro celé aplikace)

**Key-down, klávesa ještě není v `held` (nový stisk):**

| Podmínka | Vlastník | Potlačit? | Efekt |
|---|---|---|---|
| Klávesa = zkratka přepnutí | Swallow | ano | přepnout režim |
| `Mode::Binding` | Swallow | ano | Esc = zrušit, jinak uložit vazbu |
| `Mode::Gamepad` a klávesa je namapovaná | Pad | ano | přepočítat `PadState` |
| cokoli jiného | Os | ne | – |

**Key-down, klávesa už je v `held` (autorepeat):** potlačit právě tehdy, když `owner != Os`. Žádná akce, žádné přepnutí režimu.

**Key-up:** vzít vlastníka z `held`, odebrat záznam, potlačit právě tehdy, když `owner != Os`. Pokud byl `Pad`, přepočítat `PadState`. **Key-up bez záznamu** (klávesa držená před spuštěním aplikace) → propustit do OS.

**Přechod Klávesnice → Gamepad:** klávesy držené s vlastníkem `Os` zůstávají `Os` až do uvolnění (OS dostane jejich key-up, nic se nezasekne). Při každém zapnutí hook přeinstalovat (ochrana proti tichému odebrání Windows).

**Přechod Gamepad → Klávesnice (ručně i vynuceně):** okamžitě poslat neutrální `PadState`, všechny klávesy s vlastníkem `Pad` přepsat na `Swallow`. Jejich key-up se tak spolkne (OS nikdy neviděl jejich key-down) a gamepad nezůstane s vychýlenou páčkou.

**`Mode::Binding`:** povoleno jen z režimu Klávesnice. Automatické zrušení po 10 s. Esc ruší. Zkratku přepnutí nelze přiřadit akci.

**Zpřesnění, jak je engine implementuje (Fáze 1)** — kde tabulka mlčela; sporné body jsou i v „Otevřených otázkách“:

- Zkratka přepnutí se spolkne **v každém režimu**. V `Binding` ale nepřepíná — je to pokus přiřadit zkratku, ten se odmítne (`BindingRejected { ToggleKey }`) a přiřazování čeká dál. V `Disabled` se odmítne přepnutí (`ToggleRejected`).
- `Disabled { reason }` se chová jako Klávesnice (vše do OS), jen nepustí na Gamepad. `enable()` vede vždy na Klávesnici, nikdy rovnou na Gamepad.
- Timeout přiřazování hlídá `tick(now_ms)` **i** `on_key`: propadlé přiřazování nesmí sníst klávesu, která už patří OS.
- Přiřazení klávesy, která patří jiné akci, vazbu **přesune** (`BindingSaved { moved_from }`); „Přiřadit“ klávesu k akci **přidává** (akce může mít víc kláves).
- `set_mapping` v režimu Gamepad nejdřív vynutí Klávesnici; běžící přiřazování zruší.
- `reset_held(reason)` = vynutit Klávesnici + zapomenout `held` (zamčení relace, uspání, přepnutí plochy, přeinstalace hooku, **panika v hooku**).
- `Decision::pad` je `Some` při každé změně režimu a při každé změně stavu; druhá klávesa téhož tlačítka nic neposílá.
- Nemapovatelné klávesy (`scan == 0`, `scan > 0x7F` — falešný LCtrl z AltGr 0x21D) nejdou mapovat ani přiřadit, engine je **vůbec nesleduje** a jdou vždy do OS — i během přiřazování (okno dostane `BindingRejected { Unmappable }`). Sledovat je nejde: všechny klávesy se `scan == 0` by si sdílely jeden záznam.
- **Ztracený key-up** (přidáno po revizi): key-down klávesy, kterou engine drží jako `Pad`/`Swallow` a jejíž poslední událost je starší než `STALE_KEY_MS` = 1,5 s, není autorepeat, ale nový stisk (starý záznam se zahodí). Autorepeat chodí nejpozději po 1 s a opakuje se jen naposledy stisknutá klávesa, takže u opravdu držené klávesy tahle pauza nenastane. U kláves patřících OS pravidlo neplatí (OS viděl key-down; „nový stisk“ by ho mohl nechat bez key-upu).
- Příkazy `toggle`, `start_binding`, `cancel_binding` berou `now_ms` a propadlé přiřazování (časovač nestihl tiknout) nejdřív zruší.
- `Engine::new` startuje v `Disabled { PadNotConnected }`; na Klávesnici ho pustí až `enable()` od pad vlákna. Bez padu tak zkratka nikdy nepošle WASD do neexistujícího ovladače.
- `Decision` nese **nejvýš jednu** `UiEvent`; když se v jednom volání stanou dvě věci (propadlé přiřazování + zkratka), nese novější. Zdrojem pravdy o režimu je `Engine::mode()` — hook vlákno ho po každém volání publikuje do `Status` a okno kreslí podle něj, ne podle událostí.

### Výpočet `PadState` (čistá funkce)

- Vstup: množina držených kláves s vlastníkem `Pad` + jejich `seq`.
- Tlačítko je stisknuté, pokud ho drží **kterákoli** jeho klávesa (více kláves na jednu akci je povoleno).
- **SOCD** (A+D současně): na každé ose vyhrává **naposledy stisknutý** směr (vyšší `seq`). Po uvolnění se vrací ke staršímu drženému směru. Drží-li směr víc kláves, platí jeho nejnovější stisk.
- Páčka: `x, y ∈ {-1, 0, 1}`; osa = `32767`, diagonála = `23170` (32767/√2). **Pozor: v XInput je kladné Y nahoru** → W = `+Y`. Nikdy nenegovat `i16::MIN` (záporné hodnoty vznikají jako `-32767` z kladné konstanty).
- Triggery: `255` při stisku, jinak `0`.

---

## Fáze

### Fáze 0 – Kostra a build ✅

- [x] Cargo workspace (`crates/core`, `crates/updater`, `crates/installer`, `src-tauri`, `ui/`), `CLAUDE.md` s principy.
- [x] `.cargo/config.toml` s `crt-static`, podmíněný `windows_subsystem`, zamčený toolchain 1.97.0.
- [x] Tauri 2 projekt s prázdným oknem ve stylu WinSentu (vlastní titlebar, tmavé tokeny, ikona).
- [x] Souborový logger do `keypad.log` vedle `.exe` (fallback `%APPDATA%\KeyPad\`), zápis z vlastního vlákna přes kanál, `try_send` nikdy neblokuje.
- [x] Globální panic hook → zápis do logu (mimo realtime vlákna počká na flush).
- [x] CI: GitHub Actions `windows-latest` → `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, strážce izolace `core`, build obou `.exe`, kontrola importů, artefakt.
- [x] Instalátor `KeyPadSetup.exe` a aktualizace z aplikace (viz níže „Instalace a aktualizace“).

**Hotovo, když:** release `.exe` se spustí na čistých Windows bez chybějících DLL — importní tabulka ověřená `tools\check-imports.ps1` (viz `README_CZ.md`).

### Fáze 1 – Core logika a testy ✅

- [x] `KeyId`, `Action`, `Mapping` včetně validace (duplicitní klávesa, konflikt se zkratkou, prázdné mapování, nemapovatelná klávesa, Esc jako zkratka).
- [x] `Engine` s pravidly výše.
- [x] `compute_pad_state()`.
- [x] Unit testy minimálně pro:
  - přepnutí do Gamepad při drženém W → W-up jde do OS, gamepad W neukáže,
  - přepnutí do Klávesnice při drženém W → neutrální pad, W-up spolknut,
  - autorepeat nemění stav ani neprovádí toggle opakovaně,
  - key-up bez key-down → propuštěn,
  - SOCD A→D→uvolnit D → zpět A,
  - diagonála W+D = (23170, 23170), W samotné = (0, 32767),
  - dvě klávesy na stejné tlačítko: uvolnění jedné nechá tlačítko stisknuté,
  - binding: Esc ruší, timeout ruší, zkratku nelze přiřadit,
  - property test (`proptest`): libovolná sekvence událostí + přepnutí → po uvolnění všech kláves je `PadState` neutrální a `held` prázdné. Navíc se engine po každém kroku porovnává s **nezávislým referenčním modelem** (režim, mapování, vlastník/pořadí/čas každé klávesy, pad spočítaný vlastním SOCD, odeslaný pad, oznámení) a hlídá se: všechny události jednoho stisku mají stejné potlačení, autorepeat nic nespouští, mimo Gamepad neutrál. Generátor umí i posun času bez tiku, skoky času (i dozadu, `u64::MAX`), tři mapování (jiná zkratka, Esc namapovaný, dvě klávesy na směr). Ověřeno mutacemi: 5 záměrně pokažených enginů test zabije do 0,5 s.

**Hotovo, když:** `cargo test -p core` projde (Windows; crate nemá žádné Windows API).

### Instalace a aktualizace ✅ (nová sekce — stejný model jako WinSent)

- [x] `KeyPadSetup.exe`: vlastní GDI okno ve stylu aplikace, režimy bez parametrů / `/quiet` (z aplikace) / `/headless` / `/uninstall`.
- [x] Per-user instalace do `%LOCALAPPDATA%\Programs\KeyPad`, zástupce v nabídce Start uživatele, záznam v Aplikacích (HKCU). Bez UAC — s jedinou výjimkou volitelné instalace ViGEmBus (princip 6).
- [x] Vše se stáhne do paměti z jednoho commitu (SHA přes GitHub API — obchází 5min cache `raw`), teprve pak se sahá na disk; `version.txt` se zapisuje poslední.
- [x] Běžící KeyPad se nejdřív zavře slušně: pojmenovaná událost `updater::QUIT_EVENT_NAME` (uklizené ukončení i ze schovaného okna), jako záloha pro 0.1.0 WM_CLOSE hlavnímu oknu, teprve po 3 s natvrdo. Zavírá se jen proces běžící z instalační složky (podle cesty, ne jména).
- [x] Kontrola WebView2 Runtime (chybí → odkaz, aplikace se nespouští) a ViGEmBus (chybí → nabídne instalaci oficiálního ovladače, viz princip 6; nainstalovaný, ale neběží → jen rada). Aplikace sama při chybějícím WebView2 ukáže českou hlášku a skončí.
- [x] Aplikace: kontrola verze (`raw …/main/release/version.txt`, bez limitu API), jantarový banner „Je dostupná nová verze“ → stáhne instalátor a spustí `/quiet`. Nabízí jen **novější** verzi a jen kopii běžící z instalace; návrat ke starší verzi = ručně spustit `KeyPadSetup.exe`.
- [x] Odinstalace smaže program, zástupce, záznam v Aplikacích, logy, data okna (WebView2) a stažené instalátory; `config.toml` nechá.
- [x] `tools\publish.ps1`: zdroják commitnutý a pushnutý → brány → build → kontrola vestavěného frontendu → kontrola importů a `DependentLoadFlags` → `release/` → commit jen `release/` → push.
- [x] Bezpečnost: obě binárky `/DEPENDENTLOADFLAG:0x800` (DLL jen ze System32), pomocné programy plnou cestou, explicitní oprávnění Tauri + CSP.

### Fáze 2 – Virtuální gamepad (ViGEm)

Rozhodnutí z výzkumu (29. 9. 2026): **vlastní malý klient ViGEmBus** (~250 řádků nad `windows`, žádný nový crate) místo `vigem-client` — ten čeká na každé IOCTL bez časového limitu a skrývá chybu 170, při které ovladač report **zahodí** (bez opakování by páčka zůstala vychýlená). Ověřeno měřením: při pádu procesu ovladač pad sám odpojí do ~6 ms.

- [x] Vlastní klient (`platform/windows/vigem.rs`): časové limity na všech voláních, chyby rozlišené (chybí ovladač / 170 = poslat znovu / 55 = pad zmizel / 483 = ještě se připravuje / 650 = bez XInput slotu).
- [x] Pravidla pad vlákna: `wait_ready` právě jednou a nikdy odpojit dřív, než se vrátí (chyba v ovladači); 483 při prvním připojení na novém PC není chyba („připojuji…“, až 10 s); 170 → opakovat až 250 ms, pak chyba.
- [x] **Před uspáním pad odpojit** (po probuzení zůstane vypnutý — Fáze 2b) (otevřený BSOD ViGEmBus #160 při probuzení s připojeným padem) — `PowerRegisterSuspendResumeNotification`, bez okna a bez pollování.
- [x] Oznamovací oblast jako WinSent: ikona, menu Otevřít/Ukončit, zavření okna = schovat + uspat WebView. Instalátor ukončuje KeyPad pojmenovanou událostí (`updater::QUIT_EVENT_NAME`), ne WM_CLOSE.
- [x] Instalace ViGEmBus z KeyPadSetup (viz výjimka v principu 6) + režim `/vigembus`, který spouští aplikace, když ovladač chybí.
- [x] Pad vlákno: připojení ke sběrnici; při neúspěchu stav „ViGEmBus chybí“ (resp. „nainstalovaný, ale neběží“ s radou), aplikace **běží dál**.
- [ ] Engine startuje v `Disabled { PadNotConnected }`, dokud se target nepřipojí (toggle je do té doby zakázaný) — napojení engine na stav padu je Fáze 4 (engine žije v hook vlákně); místa v kódu jsou označená.
- [x] ~~Xbox 360 target připojit **hned po startu**~~ — nahrazeno Fází 2b: připojit jen na povel uživatele.
- [x] Smyčka `recv_timeout(200 ms)`: vyprázdnit frontu na poslední `PadState`, `update()`, zapsat heartbeat.
- [x] Chyba `update()` → nahlásit do stavu padu, nabídnout „Zkusit znovu“ (`Engine::disable(PadError)` přibude s napojením engine ve Fázi 4).
- [x] Při ukončení target odpojit. **Ověřeno:** i při pádu procesu ho ovladač odebere sám (~6 ms).

**Hotovo, když:** v `joy.cpl` se objeví Xbox 360 ovladač a testovací tlačítko v GUI pohne páčkou. _Stav: automaticky ověřeno bez okna (pad_selftest přes XInput: připojení, stav, odpojení, pád procesu); kontrola v `joy.cpl` s tlačítkem „Vyzkoušet páčku“ čeká na vlastníka. Neověřeno naostro: spánek/probuzení, instalace ViGEmBus (potřebuje čistý virtuál)._

### Fáze 2b – Doladění podle vlastníka (29. 9. 2026)

Zpětná vazba po vyzkoušení Fáze 2. Jde první, dřív než hook — mění chování toho, co už je hotové.

- [x] **Ovladač jen na povel.** Virtuální ovladač se připojí **výhradně** po interakci uživatele (přepínač „ovladač zapnutý / vypnutý“) — nikdy sám po startu aplikace, po probuzení ani po aktualizaci. Nahrazuje původní „připojit hned po startu“. Vypnutí = neutrál → odpojit.
- [x] **Žádný autostart.** KeyPad se nikdy nespouští s Windows, jen když ho uživatel zapne (otevřená otázka 16 uzavřena).
- [x] **Vypnutí / restart / odhlášení PC:** vždy před tím aktivní ovladač deaktivovat (neutrál → odpojit) a celou aplikaci ukončit (skryté okno nejvyšší úrovně s `WM_QUERYENDSESSION` / `WM_ENDSESSION`). **Uspání / hibernace:** ovladač vypnout a po probuzení nechat vypnutý (bod 1).
- [ ] **Tahání okna bez zadrhávání jako ve WinSentu** (ten přešel z acrylic na blur). Windows 10: blur jako WinSent; Windows 11 22H2+ (kde blur podle autorů knihovny zadrhává): systémový Mica. Rozhoduje číslo buildu, ne předpoklad. _(Windows 11: Mica hotová. Windows 10: nastavení je už totožné s WinSentem, žádná příčina specifická pro KeyPad se nenašla — čeká na potvrzení vlastníka, viz otevřená otázka 29.)_
- [x] **Zrcadlo ViGEmBus** v `iva-exe/KeyPad` (`mirror/ViGEmBus_1.22.0_x64_x86_arm64.exe`, schváleno) — záložní adresa, když repo autora zmizí; ověřuje se stejným napevno zapsaným SHA-256.
- [x] **Credit ViGEmBus** (Nefarius Software Solutions e.U., odkaz na github.com/nefarius/ViGEmBus) v instalátoru i v detailech aplikace.
- [x] **ViGEmBus automaticky — instalace i aktualizace.** KeyPadSetup ho nainstaluje bez zaškrtávátka, když chybí, a aktualizuje, když je starší než poslední vydání (ovladač < 1.21.442). Platí i pro aktualizaci z aplikace. Výzva UAC od Windows zůstává (ovladač jinak nejde). Nainstalovaný a běžící ViGEmBus v aktuální verzi se nikdy nepřeinstalovává.
- [x] **Méně textu:** v okně aplikace i instalátoru co nejméně vysvětlivek, žádné chybové kódy (ty patří do logu).
- [x] Vyzkoušet, co jde bez zásahu do systému, a opravit známé chyby. _(Naostro neověřeno: skutečné vypnutí/uspání PC, instalace a aktualizace ViGEmBus s UAC — potřebuje virtuál.)_

**Hotovo, když:** po startu KeyPadu není v `joy.cpl` žádný ovladač; objeví se až po zapnutí přepínače; vypnutí PC s aktivním ovladačem aplikaci ukončí.

### Fáze 3 – Keyboard hook

- [x] Jednoinstanční zámek: řeší `tauri-plugin-single-instance` (registruje se jako první plugin, tedy **před** instalací hooku); druhé spuštění ukáže okno běžící instance a skončí.
- [x] Hook vlákno (`platform/windows/hook.rs`): instalace `WH_KEYBOARD_LL` (`hMod` z `GetModuleHandleW(None)`), smyčka `GetMessageW`, `logger::mark_realtime_thread()`. Z callbacku **nevolat `log::`** (alokace + krátký zámek uvnitř crossbeamu) — pevný záznam do fronty bez zámků, logovat z jiného vlákna. _(Callback předává rozhodnutí přes `trait Vystup` — jen atomiky; panika se loguje ze smyčky přes `WM_APP`. Testem ověřeno, že callback **nic nealokuje** — počítadlo alokací v testech.)_
- [x] Callback: `nCode < 0` → rovnou `CallNextHookEx`. Ignorovat (propustit) události s `LLKHF_INJECTED`.
- [x] `KeyId` ze `scanCode` + `LLKHF_EXTENDED`. Scan kód nad `u16` se neusekává (0x1_0011 by jinak byl W), ale bere jako nemapovatelný. Klávesy se `scanCode == 0` (mediální apod.) nelze mapovat → propustit (`KeyId::is_mappable`).
- [x] **AltGr na českém rozložení** generuje falešný LCtrl (typicky `scanCode 0x21D`) → nikdy nemapovat, propustit. _(Test s 0x21D; naostro ověří vlastník v `hook_selftest` — AltGr vypíše „(nemapovatelná) scan 0x21D“.)_
- [x] Celé tělo callbacku v `catch_unwind`; při panice nastavit atomický příznak „fail-safe“, propustit klávesu a zavolat **`reset_held(HookPanic)`**, ne `force_keyboard`: engine mohl klávesu už zaznamenat jako `Pad`, `force_keyboard` by z ní udělal `Swallow` a key-up propuštěné klávesy by se spolkl → v OS by visela (s levým Shiftem = vše velkými). Nalezeno revizí. _(Když selže i úklid, engine se už nevolá a smyčka hook odebere — klávesy jdou do OS.)_
- [x] Po každé (pře)instalaci hooku `reset_held(HookReinstalled)` — během tichého odebrání se události ztrácely.
- [x] Engine je vlastněn hook vláknem (přístup přes `thread_local!`/`static` jen z tohoto vlákna – callback nemá kontext).
- [x] Zobrazované názvy kláves přes `GetKeyNameTextW` (`platform/windows/klavesy.rs`; podle rozložení, česky).

**Hotovo, když:** log ukazuje scan kódy, Notepad funguje normálně a v testovacím režimu lze potlačit vybranou klávesu. _Stav: automaticky ověřeno (13 testů callbacku a vlákna, skutečná instalace/přeinstalace/odebrání na skryté ploše i `hook_selftest -- instalace`). Scan kódy vypisuje `hook_selftest` **jen do konzole** — aplikace stisky nikdy neloguje (otevřená otázka 33). Ruční zkoušku (Poznámkový blok, Scroll Lock potlačí WASD) udělá vlastník: `cargo run -p keypad --release --example hook_selftest`. Aplikace hook zatím neinstaluje — zapne ho přepínač ve Fázi 4._

### Fáze 4 – Propojení a přepínání

- [ ] **Hook → Pad bez zámku:** hook NESMÍ posílat stav přes dnešní crossbeam kanál pad vlákna (bere std Mutex sdílený s odesílateli z GUI a alokuje — princip 3). Rozhodnuto: atomický slot „poslední stav“ + auto-reset událost, na kterou pad vlákno čeká spolu s kanálem příkazů; GUI a uspání zůstávají na kanálu.
- [ ] Hook → Pad kanál, GUI → Hook příkazy (`Toggle`, `SetMapping`, `StartBinding`, `CancelBinding`, `ForceKeyboard`) přes Tauri commands; stav do GUI přes Tauri events.
- [ ] **Přepínač „ovladač zapnutý / vypnutý“** (z Fáze 2b) napojený na engine: zapnutí = připojit ovladač a začít zachytávat jeho klávesy; vypnutí = neutrál, odpojit. **Zkratka (Scroll Lock) jen pozastaví** zachytávání (ovladače zůstanou připojené a neutrální), aby šlo psát do chatu, aniž by hra ztratila hráče.
- [ ] **Víc ovladačů z jedné klávesnice** (až 4 — strop XInputu): každý vlastní klávesy, každý **barevně odlišený** (barva v okně i u kláves). Jádro: `Binding { pad, action }`, vlastník `Pad(pad, akce)`, `PadState` a stav „připraven“ pro každý ovladač zvlášť; klávesa smí patřit jen jednomu ovladači. Nový ovladač začíná bez kláves. Property test rozšířit na víc ovladačů.
- [ ] Hook se instaluje jen tehdy, když je potřeba: zapnutý ovladač nebo otevřené okno (živá detekce stisků). Jinak KeyPad na klávesnici vůbec nesahá (princip 10).
- [ ] Zkratka přepnutí (výchozí **Scroll Lock**, nastavitelná), reaguje jen na první key-down, nikdy na autorepeat.
- [ ] Zachytávání jde zapnout jen pro připojené ovladače.
- [ ] (Bez dlouhých pokynů v GUI — minimum textu; zkratka je vidět v nastavení.)
- [ ] Volba „vždy navrchu“ (v nastavení, Fáze 7).

**Hotovo, když:** v gamepad režimu WASD hýbe páčkou v `joy.cpl`, Notepad písmena WASD nedostává, ostatní klávesy fungují.

### Fáze 5 – Pojistky proti softlocku

- [ ] **Watchdog padu:** hook vlákno při každé události v režimu Gamepad kontroluje heartbeat; starší než 1 s → vynutit Klávesnici. Totéž kontroluje GUI strana každých 500 ms.
- [ ] **Uzamčení / spánek:** message-only okno v hook vlákně, `WTSRegisterSessionNotification` a `WM_POWERBROADCAST`; při `WTS_SESSION_LOCK` / `PBT_APMSUSPEND` → `Engine::reset_held` (vynutí Klávesnici a vyprázdní `held`; key-up během zámku nepřijdou).
- [ ] **Zabezpečená plocha** (výzva UAC, Ctrl+Alt+Del) neposílá `WTS_SESSION_LOCK`, ale LL hook tam taky nevidí key-upy: `SetWinEventHook(EVENT_SYSTEM_DESKTOPSWITCH)` v hook vlákně → `reset_held(DesktopSwitch)`. Bez toho by po návratu zůstala páčka vychýlená, dokud se klávesa znovu nestiskne a nepustí.
- [ ] Zvážit totéž při přepnutí popředí na okno s vyššími právy (UIPI — hook mu klávesy nevidí).
- [ ] Nenamapované klávesy **se v režimu Gamepad vždy propouštějí** (Alt+Tab, Win, Alt+F4 zůstávají funkční).
- [ ] Mapování nesmí obsahovat zkratku přepnutí (validace v GUI i při načtení konfigurace).
- [ ] Pokud se hook vlákno zasekne, Windows hook po timeoutu přeskočí → klávesy jdou do OS. Zdokumentovat jako přijatelné selhání (není softlock).
- [ ] Poslední záchrana (do README): Ctrl+Alt+Del nelze hookem zachytit → Správce úloh → ukončit aplikaci; hook i virtuální pad zmizí s procesem.
- [ ] **Ukončení** (menu v oznamovací oblasti nebo událost `QUIT_EVENT_NAME` od instalátoru) = neutrální pad, odhook, odpojení targetu, v tomto pořadí. Zavření okna jen schovává do trayе. Otevřená otázka 21: má schování okna v režimu Gamepad vynutit Klávesnici?

**Hotovo, když:** projdou všechny scénáře v tabulce edge cases níže.

### Fáze 6 – GUI

- [ ] Stavový řádek: ViGEmBus (OK / chybí + „Nainstalovat ViGEmBus“ / neběží + rada + „Zkusit znovu“), režim, poslední chyba. _(Stav padu a instalace ovladače hotové ve Fázi 2.)_
- [ ] **Nastavení kláves jako keybinds ve hře:** seznam vstupů ovladače (páčky, D-pad, A/B/X/Y, LB/RB, LT/RT, L3/R3, Start/Back) s přiřazenými klávesami (názvy přes `GetKeyNameTextW`); klik na vstup → „stiskni klávesu“ (Esc ruší, 10 s timeout) → uloženo; odebrání klávesy; „Obnovit výchozí“. Barva ovladače u kláves.
- [ ] **Živá vizuální detekce stisků:** stisknutá klávesa hned rozsvítí svůj vstup (i když ovladač neběží) — jako v menu kláves ve hře. Zároveň živý náhled ovladače (páčky, tlačítka, triggery).
- [ ] **Jednoduché a minimalistické:** co nejméně textu, vysvětlivek a kódů; stav poznat z barvy a ikon, podrobnosti v tooltipu nebo logu.
- [ ] Upozornění na konflikty přímo u řádku (`MappingError` nese klávesu i akci).
- [ ] Úpravy kláves i za běhu ovladače: přiřazování dočasně pozastaví zachytávání a pak ho vrátí; držené klávesy si drží vlastníka (princip 2).

### Fáze 7 – Konfigurace a balení

- [ ] **Nastavení v aplikaci** (ozubené kolo): zkratka pozastavení, vždy navrchu, obnovit výchozí klávesy, aktualizace, **O aplikaci** (verze, credit ViGEmBus s odkazem, log).
- [ ] `config.toml` — klávesy všech ovladačů, zkratka, volby; návrh (otevřená otázka 11): vždy `%APPDATA%\KeyPad\`. Dřív: vedle `.exe` v per-user instalaci je zapisovatelné; fallback `%APPDATA%\KeyPad\config.toml`).
- [ ] Chybějící soubor → vytvořit výchozí. Nevalidní → přejmenovat na `config.invalid.toml`, načíst výchozí, zobrazit varování (nikdy nepadat).
- [ ] Atomický zápis (dočasný soubor + přejmenování).
- [ ] Pole `version` pro budoucí migrace.
- [x] `README_CZ.md` pro kamaráda: instalace (KeyPadSetup), WebView2, ViGEmBus, SmartScreen („Další informace“ → „Přesto spustit“), možné falešné varování antiviru (keyboard hook), aktualizace, odinstalace. _(Postup ve Steamu doplnit po Fázi 8.)_

### Fáze 8 – End-to-end test s Remote Play Together

- [ ] Hostitel: Shift+Tab → Remote Play → u hosta **vypnout klávesnici** (pojistka navíc), gamepad zapnout.
- [ ] Ověřit, že hostitel vidí druhý ovladač a hra ho přiřadí hráči 2.
- [ ] Ověřit, zda Steam klient kamaráda přeposílá gamepad i ve chvíli, kdy má fokus okno aplikace (a ne okno streamu). Výsledek zapsat do README.
- [ ] Ověřit, že potlačené klávesy se skutečně nedostanou do streamu (LL hook by měl blokovat i Raw Input – ověřit prakticky).
- [ ] Ověřit latenci (subjektivně) a stabilitu 30+ minut.

---

## Výchozí mapování (podle pozice kláves)

| Akce | Klávesa |
|---|---|
| Levá páčka | W / A / S / D |
| Pravá páčka | šipky |
| D-pad | I / J / K / L |
| A / B / X / Y | Mezerník / C / F / R |
| LB / RB | Q / E |
| LT / RT | 1 / 3 |
| L3 / R3 | Levý Shift / V |
| Start / Back | Enter / Backspace |
| Přepnutí režimu | Scroll Lock |

---

## Edge cases – kontrolní tabulka

| Situace | Očekávané chování |
|---|---|
| Přepnutí na Gamepad při drženém W | W zůstává v OS až do uvolnění, gamepad ho ignoruje |
| Přepnutí na Klávesnici při drženém W | Páčka okamžitě neutrální, W-up spolknut |
| Držení zkratky přepnutí (autorepeat) | Přepne se jen jednou |
| Klávesa držená už při spuštění | Key-up propuštěn do OS |
| A+D současně | Vyhrává naposledy stisknutá |
| W+D | Diagonála délky 1, ne √2 |
| AltGr (CZ) | Nespouští akci namapovanou na LCtrl |
| Stejná klávesa pro dvě akce | Validace zamítne |
| Zkratka přepnutí přiřazena akci | Validace zamítne |
| ViGEmBus není nainstalován | Aplikace běží, přepnutí zakázáno, tlačítko „Nainstalovat ViGEmBus“ (KeyPadSetup /vigembus, UAC od Windows) |
| ViGEmBus nainstalovaný, ale neběží (zakázaný, čeká na restart, blokovaný) | Aplikace běží, rada podle stavu zařízení; instalátor se spustí jen tehdy, když je ovladač prokazatelně starší a nečeká na restart |
| ViGEmBus starší než 1.21.442 | KeyPadSetup ho aktualizuje (UAC); předtím KeyPad slušně ukončí a po aktualizaci ho zase spustí |
| Chyba ViGEm během hry | Vynucená Klávesnice + hláška + „Zkusit znovu“ |
| Pad vlákno spadne / zasekne se | Watchdog do 1 s vynutí Klávesnici |
| Panika v hook callbacku | Klávesa propuštěna, režim Klávesnice, `held` vyprázdněn (`reset_held`) — i její key-up dostane OS |
| Win+L / uspání | Vynucená Klávesnice, `held` vyprázdněn |
| Výzva UAC / Ctrl+Alt+Del při držené klávese | Přepnutí plochy → vynucená Klávesnice, `held` vyprázdněn |
| Ztracený key-up (klávesa padu / spolknutá) | Další stisk po ≥ 1,5 s je nový stisk — nic se nespolkne navíc, zkratka přepne |
| Nemapovatelná klávesa (média, AltGr) kdykoli | Do OS, engine ji nesleduje |
| Druhá instance | Ukáže okno běžící instance, konec před instalací hooku |
| Nevalidní `config.toml` | Záloha, výchozí hodnoty, varování |
| Složka s `.exe` jen pro čtení | Konfigurace v `%APPDATA%` |
| Zavření okna | Okno se schová do oznamovací oblasti, WebView se uspí; ukončení z menu = neutrál → odhook → odpojení padu |
| Uspání počítače s připojeným padem | Pad se před uspáním odpojí a vypne (BSOD ViGEmBus #160); po probuzení zůstane vypnutý |
| Vypnutí / restart / odhlášení PC | Pad se odpojí a KeyPad skončí (skryté okno `KeyPad.KonecRelace`, WM_ENDSESSION) |
| Start KeyPadu | Žádný virtuální ovladač — objeví se až po zapnutí přepínače |
| Pád KeyPadu | Ovladač virtuální pad odpojí sám do ~6 ms (změřeno) |
| Pád procesu | Hook i virtuální pad zmizí s procesem |
| Editace mapování v režimu Gamepad | Zakázáno / nejdřív přepnout |
| Binding bez stisku klávesy | Automatické zrušení po 10 s |
| Steam klient spuštěn jako správce | Hook z neprivilegovaného procesu mu klávesy neblokuje → varování v README: nespouštět Steam jako správce |
| Zkratka přepnutí během přiřazování | Nepřepne; odmítnuta jako vazba, přiřazování čeká dál |
| Esc jako zkratka přepnutí | Validace zamítne (Esc ruší přiřazování) |
| Aktualizace při běžícím KeyPadu | Instalátor nastaví událost `QUIT_EVENT_NAME` → uklizené ukončení (i schovaného v trayi); záloha WM_CLOSE hlavnímu oknu (vydání 0.1.0), po 3 s natvrdo |
| Chybí WebView2 Runtime | Instalátor to pozná, aplikaci nespustí, pošle odkaz; spuštěná aplikace to ohlásí česky a skončí (žádný neviditelný proces) |
| Hned po aktualizaci CDN ještě vrací starou `version.txt` | Aplikace nabízí jen novější verzi — starou nenabídne |
| `KeyPadSetup.exe` / `KeyPad.exe` ve Stažených vedle podvržené DLL | Statické importy jen ze System32 (`/DEPENDENTLOADFLAG:0x800`) a první příkaz `SetDefaultDllDirectories(SYSTEM32)` i pro DLL načítané za běhu; ověřeno podstrčenými DLL |

---

## Otevřené otázky

Nejasnosti ve specifikaci, na které se narazilo. U každé je, jak to **teď** dělá kód — nic z toho není rozhodnuté potichu; stačí odpovědět a upraví se to.

1. **SOCD pro D-pad?** Specifikace dává SOCD jen páčkám; D-pad je v XInput sada tlačítek, takže J+L teď drží DpadLeft **i** DpadRight současně (doslovně „tlačítko drží kterákoli klávesa“). Řada her na protilehlé směry D-padu reaguje špatně. _Návrh:_ dát D-padu stejné SOCD jako páčkám (naposledy stisknutý vyhrává).
2. **Zkratka přepnutí během přiřazování.** První řádek tabulky („zkratka → přepnout režim“) nemá podmínku na režim. Teď: v `Binding` zkratka nepřepíná, odmítne se jako vazba a přiřazování čeká dál. Alternativa: přiřazování zrušit.
3. **Přiřazení klávesy, která už patří jiné akci.** Teď se vazba **přesune** (stará akce o klávesu přijde, UI dostane `moved_from`). Alternativa: odmítnout a nechat uživatele nejdřív odebrat starou vazbu.
4. **„Přiřadit“ = přidat, nebo nahradit?** Teď přidává (akce může mít víc kláves, odebírá se po jedné). Alternativa: nahradit všechny klávesy akce.
5. **Přiřazování v režimu Disabled** (ViGEmBus chybí). Doslovně „jen z režimu Klávesnice“ → teď zakázané. Kamarád by si ale mohl chtít rozvržení připravit dřív, než nainstaluje ViGEmBus. _Návrh:_ povolit a po skončení vrátit do `Disabled`.
6. **Prázdné mapování.** Teď je chybou validace a poslední vazbu nejde odebrat (hodnota `Mapping` je vždy platná). Alternativa: jen varování.
7. **Esc jako zkratka přepnutí** — teď zakázané validací (jinak by se z přiřazování nedalo vycouvat Esc). Specifikace to neřeší.
8. **Klávesa držená přes zamčení relace.** `reset_held` zapomene `held`; LL hook nerozliší autorepeat od nového stisku, takže autorepeat té klávesy po odemčení je pro engine nový stisk. Kdyby mezitím uživatel přepnul na Gamepad a klávesa byla namapovaná, OS by viděl key-down (před zámkem), ale key-up by se spolkl → klávesa „visí“ v OS do dalšího stisku. Velmi nepravděpodobné (držet klávesu přes zámek a mezitím přepnout); teď se to přijímá. Alternativa: po zámku nechat staré záznamy jako `Os` „na dožití“ (pak by se naopak první stisk po odemčení mohl ztratit padu).
9. **Instalace per-user vs. Program Files.** WinSent instaluje do Program Files s právy správce; KeyPad kvůli principu 6 do `%LOCALAPPDATA%\Programs\KeyPad` bez UAC (jako VS Code, Discord). OK?
10. **Druhá instance** — roadmapa chtěla hlášku a konec; teď (`tauri-plugin-single-instance`) druhé spuštění jen ukáže okno běžící instance. Plynulejší, ale jiné, než stálo v zadání.
11. **Kde má bydlet `config.toml`?** Roadmapa: vedle `.exe`. V per-user instalaci je to zapisovatelné, jenže se to plete s binárkami (odinstalace ho musí obcházet). _Návrh:_ vždy `%APPDATA%\KeyPad\config.toml` (data odděleně od programu, přežijí přeinstalaci).
12. **Interval kontroly aktualizací** — teď při startu, pak každých 30 min a při zobrazení okna, pokud je poslední kontrola starší než 5 min (princip 10; WinSent 30 s). `raw` drží cache 5 minut, častěji to nemá smysl.
13. **Tlačítko Guide (Xbox logo)** není mezi akcemi — výchozí mapování ho nemá a Steam ho zachytává pro svůj overlay. Přidat?
14. **Pravidlo ztraceného key-upu (1,5 s)** — přidáno po revizi, ve specifikaci nebylo; mění doslovné „klávesa v `held` = autorepeat“ pro klávesy `Pad`/`Swallow`. Opírá se o to, že Windows opakují jen naposledy stisknutou klávesu a starší se po jejím uvolnění znovu nerozjede. Kdyby nějaká klávesnice/ovladač opakování starší klávesy obnovil, stane se nanejvýš: klávesa padu dostane nové pořadí pro SOCD, nebo spolknutá klávesa pošle do OS jeden úhoz navíc. OK?
15. ✅ _Rozhodnuto ve Fázi 3: `GetTickCount64()`._ **Čím se v hooku měří čas** — čas události (`KBDLLHOOKSTRUCT::time`) je jen 32bitový (po 49 dnech přeteče) a u vstříknutých událostí si ho volající vymýšlí; engine potřebuje čas, který necouvne. Zdržení hook vlákna jsou milisekundy, pravidla mají vteřinové limity (1,5 s, 10 s).
16. ✅ _Rozhodnuto vlastníkem 29. 9.: žádný autostart, KeyPad spouští jen uživatel._ **Spouštění po přihlášení** (WinSent ho má) — teď NENÍ. Zápis do `HKCU\…\Run` by porušil princip 6 (aplikace nezapisuje do registru; šla by zkratka ve složce Po spuštění) a hlavně: běžící KeyPad drží připojený virtuální Xbox ovladač, takže by ho hry a Steam viděly pořád, i když KeyPad nepoužíváš. _Návrh:_ nechat bez, případně volitelně přes zástupce ve složce Po spuštění.
17. ✅ _Rozhodnuto 29. 9.: ovladač jen po interakci uživatele (Fáze 2b)._ **Připojení padu hned po startu** (roadmapa kvůli stabilnímu pořadí hráčů) znamená zvuk „zařízení připojeno“ při každém startu KeyPadu a ovladač viditelný pro hry po celou dobu běhu (i schovaného v trayi). Alternativa: připojit až při prvním přepnutí na Gamepad.
18. **Oficiálně podporované minimum Windows.** Naše binárky mají běžet na každém Windows 10 od 1507 (po opravě instalátoru žádný novější statický import). Microsoft ale oficiálně podporuje WebView2 až od Windows 10 **1709** (+ LTSC 2015/2016). _Návrh do README:_ „Windows 10 1709 a novější (vč. LTSC) a Windows 11, 64bit; doporučeno 22H2 / 11“. Na 1507/1511 navíc chybí ochrana `/DEPENDENTLOADFLAG` (Windows ji ignorují) — přijatelné u nepodporovaných buildů?
19. ✅ _Rozhodnuto 29. 9.: jako WinSent — blur na Windows 10, Mica na Windows 11 22H2+ (Fáze 2b)._ **Rozmazané pozadí okna (blur)** — vzhled jako WinSent, ale jde přes nedokumentované API, na Windows 10 před 1809 chybí a na Windows 11 22H2 podle autora knihovny zpomaluje tažení okna. Kvůli principu 10 zvážit neprůhledné pozadí.
20. ✅ _Schváleno 29. 9.: zrcadlit do `iva-exe/KeyPad` (Fáze 2b)._ **Záloha instalátoru ViGEmBus** — repo ViGEmBus je archivované; kdyby zmizelo, instalace ovladače z KeyPadSetup přestane fungovat (bezpečně — jen „nepodařilo se stáhnout“). Zrcadlit ho jako asset vydání v `iva-exe/KeyPad` (stejný hash), nebo ne?
21. **Schované okno v režimu Gamepad** — zavření okna ho jen schová do trayе a pad zůstává připojený; režim se nemění. Má schování v režimu Gamepad vynutit Klávesnici? _Návrh:_ ne — schovat okno a hrát je hlavní scénář (okno nepřekáží streamu).
22. **„Vyzkoušet páčku“** jede plnou výchylkou (kruh ~1,2 s, pak neutrál). Ve Fázi 4 povolit jen v režimu Klávesnice; stav z klávesnice test přeruší.
23. **Paměť WebView2 schovaného v trayi** — _rozhodnuto:_ při schování `MemoryUsageTargetLevel = LOW`. Fyzická paměť (co ukazuje Správce úloh) klesne z ~345 MB na ~50 MB (po 2 min ~120 MB), soukromá zůstává ~160 MB; CPU ~0. Víc bez zavření WebView nejde.
24. ✅ _Rozhodnuto 29. 9.: ViGEmBus se instaluje i aktualizuje automaticky, bez zaškrtávátka, i při aktualizaci z aplikace (Fáze 2b); souhlas = výzva UAC od Windows._ **Zaškrtávátko „Nainstalovat i ovladač ViGEmBus“ je předvyplněné**, když ovladač chybí — na přání vlastníka, ať kamarád nepotřebuje nic dalšího. Souhlas = viditelné zaškrtávátko s vysvětlením + klik + výzva UAC od Windows. Nabízí se i při ručním spuštění KeyPadSetup nad už nainstalovaným KeyPadem, nikdy při aktualizaci z aplikace (`/quiet`). Nechat předvyplněné?
25. **Zbytkové riziko instalace ViGEmBus:** oficiální instalátor (s právy správce) běží ze složky v %TEMP%, do které může psát i uživatel. Soubor sám je zamčený a ověřený, ale jestli si instalátor Advanced Installer bezpečně načítá své DLL, závisí na jeho vlastním zabezpečení (neověřeno spuštěním). Okno = výzva UAC + běh instalace. KeyPad je přesto bezpečnější než WinSent, jehož instalátor běží celý jako správce ze Stažených souborů.
26. **Instalace ViGEmBus není ověřená naostro** — na tomhle PC je ViGEmBus nainstalovaný (nesmí se měnit), cesta s UAC se dá otestovat jen ve virtuálu (čistý Windows 10/11). Do té doby ověřeno po kouscích: stažení + hash, zámek souboru, podpis, stav ovladače, obrazovky.
27. ✅ _Zastaralé: „Připojit znovu“ zmizelo (Fáze 2b) — po probuzení zůstává ovladač vypnutý; kliknutí těsně před spánkem hlídá 5s pojistka._ **„Připojit znovu“ ve spánku** — když po probuzení nepřijde oznámení (Modern Standby…), tlačítko pad připojí ručně. Teoreticky kdyby ho někdo zmáčkl v mezičase mezi oznámením o uspání a skutečným spánkem, pad by se připojil těsně před spánkem (oblast BSOD #160). Přijatelné?
28. **Nesouhlasná verze ovladače** (odpověď 1/50/87 na kontrolu verze) → chyba padu s radou odebrat „ViGEm Bus Driver“ v Aplikacích a zkusit znovu; potom aplikace nabídne instalaci.
29. **Tahání okna na Windows 10** — KeyPad má na Windows 10 přesně stejné nastavení pozadí jako WinSent (blur, bez vlastních rohů a rámečku). Pokud tahání zadrhává i tak, je potřeba vědět na jakém PC/buildu — ověřit se to dá jen skutečným tažením myší (zakázané v testech). Na Windows 11 je Mica.
30. **Číslo hráče u víc ovladačů** — ViGEmBus vrací při dvou virtuálních ovladačích stejný index pro oba (naměřeno), okno proto „hráč N“ neukazuje. Pro Fázi 4 (víc ovladačů) najít jiný spolehlivý zdroj.
31. **Aktualizace ViGEmBus, která starý ovladač odebere a nový nepřidá** (známá chyba dodavatele „spusť instalaci dvakrát“): co udělá instalátor MSI nad už zaregistrovaným produktem, se dá ověřit jen ve virtuálu. Do té doby KeyPad v tomhle stavu nic neopakuje (druhé automatické spuštění instalátoru bylo odebráno — nikdy víc než jedno spuštění) a poradí odebrat „ViGEm Bus Driver“ v Aplikacích a spustit KeyPadSetup znovu.
32. **Aktualizace ovladače zavře KeyPad** (nesmí držet sběrnici) a pak ho znovu spustí — ve všech režimech, i z tlačítka v aplikaci. Tlačítko „Aktualizovat ovladač“ se ukazuje i při zapnutém ovladači; kliknutí ovladač nejdřív vypne.
33. **Logování stisků.** Fáze 3 chtěla „log ukazuje scan kódy“. Aplikace ale stisky do `keypad.log` **nikdy** nezapisuje — log by jinak obsahoval i hesla a kamarád by ho posílal při hlášení chyby. Scan kódy vypisuje jen příklad `hook_selftest`, a to jen do konzole. Kdyby byla potřeba diagnostika u kamaráda, návrh: jen nemapovatelné klávesy (AltGr, média) a jen scan kód, zapnuté proměnnou prostředí.

---

## Mimo rozsah verze 1

Myš jako pravá páčka, podpora macOS/Linux, analogový „chůze“ modifikátor, podpis kódu. Architektura je nesmí znemožnit, ale neimplementují se. _(Automatické aktualizace a víc virtuálních ovladačů už mimo rozsah nejsou — viz „Instalace a aktualizace“ a Fázi 4.)_

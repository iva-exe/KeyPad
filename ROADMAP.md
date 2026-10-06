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
| Konfigurace | `serde_json` (`config.json`; `toml` by přidal 323 KiB — Fáze 6/7) |
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
| **GUI** (hlavní, Tauri + WebView2) | okno, commands, ikona v oznamovací oblasti | Karty ovladačů, editor kláves, živý náhled. Příkazy posílá hook vláknu a pad vláknům. Názvy kláves (`klavesy`) synchronně tady — `GetKeyNameTextW` bere rozložení okna. |
| **Hook** (`keypad-hook`) | `Engine`, LL hook, message-only okno | `SetWindowsHookExW(WH_KEYBOARD_LL)` jen podle `potreba_hooku` (zapnutý ovladač nebo okno v popředí), smyčka `GetMessageW`, rozhodování o potlačení kláves, zkratka přepnutí, WinEvent přepnutí plochy a (jen s viditelným oknem) `EVENT_SYSTEM_FOREGROUND`, časovač pro `Engine::tick`, příkazy editoru (`Prirad`, `Uprav`, `Zverejni`…). |
| **Pad** (až 4, jedno na ovladač) | vlastní klient ViGEmBus + target | Čeká na slot a příkazy, pošle nejnovější `PadState` do ViGEmBus, zapisuje tep. `wait_ready` jednoho nezdrží ostatní. |
| **Okno** (`keypad-okno`) | zrcadlo mapování, ikona a zvuk | Čeká na budík bez limitu (limit jen za hry kvůli watchdogu a pro 16ms rozestup živého stavu). Z atomik hooku vydá `rezim`, `oznameni`, `zive`, `klavesy-zmena`, přepne ikonu, pustí zvuk, předá změnu mapování ukladači. Každá obrátka v `catch_unwind`. |
| **Konfigurace** (`keypad-konfig`) | `config.json` | Jediné vlákno, které konfiguraci zapisuje: 0,5 s po poslední změně, atomicky. Pomalý disk tak nezdrží vlákno okna, na kterém visí watchdog (princip 1). |
| **Zvuk** | klient WASAPI | Přehraje tón pozastavení / pokračování; COM jen tady (Fáze 4b). |
| **Logger** | soubor `keypad.log` | Jediné místo, které píše na disk mimo konfiguraci; ostatní mu posílají zprávy přes kanál (`try_send`, nikdy neblokuje). |

Komunikace:

- GUI → Hook: Tauri command → `crossbeam` kanál s příkazy + `PostThreadMessageW(WM_APP)` pro probuzení smyčky. Úpravy mapování (`Uprav`) mají odpověď přes `bounded(1)` s limitem 1 s.
- Hook → Pad: `slot::StavSlot` (dva atomiky s číslem zápisu + auto-reset událost) — žádný kanál, žádný zámek, žádná alokace (princip 3).
- Hook → okno: callback i smyčka zapisují jen atomiky ve `vystup.rs` — `stav` (režim, chyba hooku, příčina, cíl přiřazování, pořadí), `oznameni` (poslední `UiEvent` zabalená do 48 bitů + 16bitové pořadí; mezeru okno pozná a načte vše znovu), `zive[p]` (`LiveInputs` jen s viditelným oknem) a `revize` mapování — a budík (`SetEvent`) jen při změně. Snímek mapování (klon) vzniká jen ve smyčce hook vlákna na povel `Zverejni`, který pošle vlákno okna, když `revize` předběhne jeho zrcadlo. Nikdy `emit` ani `log::` z callbacku.
- Pad → Hook: tep v `AtomicU64` slotu. Pad → okno: `Oznam` + budík.

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
  src/config.rs               // (Fáze 6/7) config.json: načtení, validace, atomický zápis (vlákno keypad-konfig)
  src/gamepad.rs              // vlákno keypad-okno, příkazy okna; gamepad/smlouva.rs = typy smlouvy s oknem
ui/                           // Svelte 5 + Vite + TypeScript
tools/                        // check.ps1, tauri.ps1, publish.ps1, check-imports.ps1, okno-test.ps1
release/                      // co si stahuje instalátor (plní publish.ps1)
```

Crate `core` se jmenuje jako balíček `core` (příkazy `cargo test -p core`), ale knihovna je `keypad_core`: crate pojmenovaný `core` by v závislých crates zastínil vestavěný `::core` a rozbil makra, která na něj odkazují.

### Klíčové typy (skutečné, `crates/core`)

```rust
struct KeyId { scan: u16, extended: bool }        // z KBDLLHOOKSTRUCT (LLKHF_EXTENDED)
enum Action { Button(PadButton), LeftStick(StickDir), RightStick(StickDir), LeftTrigger, RightTrigger }
struct PadId(u8)                                  // 0..MAX_PADS (4 = strop XInputu)
struct PadAction { pad: PadId, action: Action }   // na tohle se klávesa mapuje (Fáze 4)
enum Mode { Keyboard, Gamepad, Binding { target: PadAction, started_at_ms: u64 }, Disabled { reason: DisabledReason } }
enum Owner { Os, Pad(PadAction), Swallow }        // Pad nese ovladač a akci určené při key-down
struct HeldKey { owner: Owner, seq: u64, last_ms: u64 }  // seq = pořadí stisku (SOCD), last_ms = ztracený key-up

fn Engine::on_key(&mut self, key: KeyId, down: bool, now_ms: u64) -> Decision
struct Decision { suppress: bool, pads: PadUpdates, ui: Option<UiEvent> }   // PadUpdates = změněné stavy ovladačů
// příkazy: toggle(now_ms), capture(now_ms), start_binding(target, kind, now_ms), cancel_binding(now_ms), tick(now_ms),
//          force_keyboard(reason), reset_held(reason), enable(pad), disable(pad, reason), set_mapping(m)
// Fáze 6: BindKind { Replace, Add }; replace_mapping(m, now_ms) (bez vynucení); mapping_rev();
//          live_inputs(include_os) -> [LiveInputs; 4]; UiEvent::pack/unpack (48 bitů pro atomik);
//          Mapping::bind_replacing / unbind_target / defaults_for_first_pad; KeyId::is_reserved (Win)
```

**Víc ovladačů (Fáze 4):** `Keyboard` = zachytávání pozastavené, `Gamepad` = zachytává, `Disabled` = žádný ovladač není připravený. Klávesa jde ovladači jen tehdy, když je jeho ovladač připravený (`enable(pad)`); jinak patří OS. `disable(pad)` spolkne key-upy jeho držených kláves a pošle mu neutrál, ostatní ovladače hrají dál; poslední → `Disabled`. `capture()` = povel přepínače „hrát“. Přiřazování jde i během hry (zachytávání pozastaví a po uložení/zrušení vrátí) i bez ovladače (vrátí se do `Disabled`).

Čas je parametr (`now_ms` = monotónní ms; v hooku čas události rozšířený na 64 bitů nebo `GetTickCount64()`), engine nemá časovače ani I/O. `Mapping` je vždy platné — nevalidní hodnotu nejde vyrobit. Držené klávesy i mapování jsou pevné tabulky o 256 položkách (index = scan kód + bit E0): v hook callbacku se nic nehashuje a nic nealokuje. Všechny typy mají `Serialize`/`Deserialize` (pro Tauri events a konfiguraci; mapování se ukládá jako **seznam** `{klávesa, akce}`, ne jako mapa podle `KeyId`).

### Pravidla engine (jádro celé aplikace)

**Key-down, klávesa ještě není v `held` (nový stisk):**

| Podmínka | Vlastník | Potlačit? | Efekt |
|---|---|---|---|
| Klávesa = zkratka přepnutí | Swallow | ano | přepnout režim |
| `Mode::Binding` a klávesa je modifikátor (Ctrl, Shift, Alt), nebo Windows drží modifikátor (vlastník `Os`) | Os | ne | samotný modifikátor se přiřadí až při key-upu, když mezitím nepřišel jiný stisk; jinak nic (zkratka Windows, otázka 55) |
| `Mode::Binding` (ostatní) | Swallow | ano | Esc = zrušit, jinak uložit vazbu |
| `Mode::Gamepad` a klávesa je namapovaná | Pad | ano | přepočítat `PadState` |
| cokoli jiného | Os | ne | – |

**Key-down, klávesa už je v `held` (autorepeat):** potlačit právě tehdy, když `owner != Os`. Žádná akce, žádné přepnutí režimu.

**Key-up:** vzít vlastníka z `held`, odebrat záznam, potlačit právě tehdy, když `owner != Os`. Pokud byl `Pad`, přepočítat `PadState`. Ťuknutý modifikátor při přiřazování se teď uloží (key-up jde OS jako jeho key-down). **Key-up bez záznamu** (klávesa držená před spuštěním aplikace) → propustit do OS.

**Přechod Klávesnice → Gamepad:** klávesy držené s vlastníkem `Os` zůstávají `Os` až do uvolnění (OS dostane jejich key-up, nic se nezasekne). Při každém zapnutí hook přeinstalovat (ochrana proti tichému odebrání Windows).

**Přechod Gamepad → Klávesnice (ručně i vynuceně):** okamžitě poslat neutrální `PadState`, všechny klávesy s vlastníkem `Pad` přepsat na `Swallow`. Jejich key-up se tak spolkne (OS nikdy neviděl jejich key-down) a gamepad nezůstane s vychýlenou páčkou.

**`Mode::Binding`:** povoleno z Klávesnice, Gamepadu i `Disabled` a vrací se tam, odkud začalo (otázka 5; dřív „jen z Klávesnice“). Automatické zrušení po 10 s. Esc ruší. Zkratku přepnutí nelze přiřadit akci, Win taky ne (`BindingRejected { Reserved }`, Win jde do Windows). Klik na vstup = **nahradit** (`BindKind::Replace`: ostatní klávesy vstupu se odeberou), `+` = **přidat** (`BindKind::Add`) — otázka 40. Aplikace přiřazování přijme jen s oknem KeyPadu v popředí a zruší ho ztráta popředí, schování okna i nepovedená instalace hooku.

**Zpřesnění, jak je engine implementuje (Fáze 1)** — kde tabulka mlčela; sporné body jsou i v „Otevřených otázkách“:

- Zkratka přepnutí se spolkne **v každém režimu**. V `Binding` ale nepřepíná — je to pokus přiřadit zkratku, ten se odmítne (`BindingRejected { ToggleKey }`) a přiřazování čeká dál. V `Disabled` se odmítne přepnutí (`ToggleRejected`); hook tam je jen s oknem KeyPadu v popředí a okno řekne „Nejdřív zapni ovladač“ (otázka 48).
- `Disabled { reason }` se chová jako Klávesnice (vše do OS), jen nepustí na Gamepad. `enable()` vede vždy na Klávesnici, nikdy rovnou na Gamepad.
- Timeout přiřazování hlídá `tick(now_ms)` **i** `on_key`: propadlé přiřazování nesmí sníst klávesu, která už patří OS.
- Přiřazení klávesy, která patří jiné akci (i jiného ovladače), vazbu **přesune** (`BindingSaved { moved_from }`); okno 5 s nabízí „Zpět“ (otázka 41). Klik na vstup klávesy akce **nahradí**, `+` **přidá** další (akce může mít víc kláves) — otázky 4 a 40.
- `set_mapping` v režimu Gamepad nejdřív vynutí Klávesnici; běžící přiřazování zruší. Editor (Fáze 6) používá **`replace_mapping`**: nic nevynucuje a nemění režim ani vlastníky držených kláves (princip 2) — klávesa držená jako `Pad(t)` dohraje s `t` a její key-up se spolkne, nový stisk jde podle nového mapování; běžící přiřazování zruší (`BindingCancelled { Gui }`). Otázka 43.
- `mapping_rev()` roste jen se skutečnou změnou mapování (uložená vazba, `replace_mapping`, `set_mapping` s jiným obsahem); okno podle ní pozná, že má načíst klávesy, a „Zpět“ (`Obnov { kdyz_revize }`) se použije jen na přesně předchozí stav.
- **Win** (levá i pravá, `KeyId::is_reserved`) nejde mapovat, přiřadit ani použít jako zkratku; engine ji nesleduje a jde vždy do Windows. Drží-li Windows Win, hook každý nový stisk převezme jako klávesu OS (`adopt_os_key`) — Win+D, Win+E fungují i za hry (otázka 44).
- `live_inputs(include_os)` = fyzicky držené vstupy každého ovladače pro okno (vlastník `Pad` a `Swallow` vždy, `Os` jen s oknem v popředí); co hra opravdu dostává, je `PadState::active_inputs()` ze slotu padu. Bez alokace, v callbacku jen s viditelným oknem.
- `reset_held(reason)` = vynutit Klávesnici + zapomenout `held` (zamčení relace, uspání, přepnutí plochy, přeinstalace hooku, **panika v hooku**).
- `Decision::pad` je `Some` při každé změně režimu a při každé změně stavu; druhá klávesa téhož tlačítka nic neposílá.
- Nemapovatelné klávesy (`scan == 0`, `scan > 0x7F` — falešný LCtrl z AltGr 0x21D) nejdou mapovat ani přiřadit, engine je **vůbec nesleduje** a jdou vždy do OS — i během přiřazování (okno dostane `BindingRejected { Unmappable }`). Sledovat je nejde: všechny klávesy se `scan == 0` by si sdílely jeden záznam. **AltGr** při přiřazování (falešný LCtrl a hned pravý Alt) se odmítne celý: pravý Alt bezprostředně po falešném Ctrl se ťuknutím nepřiřadí (`bind_altgr`, otázka 56).
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
- [x] Odinstalace smaže program, zástupce, záznam v Aplikacích, logy, data okna (WebView2) a stažené instalátory; konfiguraci (`config.json` i zálohu `config.invalid.json`) nechá (otázka 54).
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
- [x] **Tahání okna bez zadrhávání jako ve WinSentu** (ten přešel z acrylic na blur). Windows 10: blur jako WinSent; Windows 11 22H2+ (kde blur podle autorů knihovny zadrhává): systémový Mica. Rozhoduje číslo buildu, ne předpoklad. _(Windows 11: Mica hotová. Windows 10: nastavení je už totožné s WinSentem, žádná příčina specifická pro KeyPad se nenašla — potvrzeno vlastníkem 30. 9.: na jeho Windows 10 v pořádku.)_
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

- [x] **Hook → Pad bez zámku:** hook NESMÍ posílat stav přes dnešní crossbeam kanál pad vlákna (bere std Mutex sdílený s odesílateli z GUI a alokuje — princip 3). Rozhodnuto: atomický slot „poslední stav“ + auto-reset událost, na kterou pad vlákno čeká spolu s kanálem příkazů; GUI a uspání zůstávají na kanálu. _(`platform/windows/slot.rs`: dva atomiky se shodným číslem zápisu — roztržené čtení se pozná; `vystup.rs` do nich píše z callbacku; testem ověřeno bez alokace a bez smíchání dvou zápisů.)_
- [x] Hook → Pad kanál, GUI → Hook příkazy (`Toggle`, `SetMapping`, `StartBinding`, `CancelBinding`, `ForceKeyboard`) přes Tauri commands; stav do GUI přes Tauri events. _(Teď: `pad_on`/`pad_off`/`pad_test`/`pad_status` s číslem ovladače, `rezim` + událost `rezim`. Fáze 6 přidala příkazy editoru `klavesy`, `pady`, `zive`, `prirad`, `zrus_prirazeni`, `uprav_klavesy`, `odeber_ovladac` a události `oznameni`, `zive`, `klavesy-zmena`, `konfigurace`.)_
- [x] **Přepínač „ovladač zapnutý / vypnutý“** (z Fáze 2b) napojený na engine: zapnutí = připojit ovladač a začít zachytávat jeho klávesy; vypnutí = neutrál, odpojit. **Zkratka (Scroll Lock) jen pozastaví** zachytávání (ovladače zůstanou připojené a neutrální), aby šlo psát do chatu, aniž by hra ztratila hráče.
- [x] **Víc ovladačů z jedné klávesnice** (až 4 — strop XInputu): každý vlastní klávesy, každý **barevně odlišený** (barva v okně i u kláves). Jádro: `Binding { pad, action }`, vlastník `Pad(pad, akce)`, `PadState` a stav „připraven“ pro každý ovladač zvlášť; klávesa smí patřit jen jednomu ovladači. Nový ovladač začíná bez kláves. Property test rozšířit na víc ovladačů. _(Jádro — nezávislý referenční model s víc ovladači, 12 mutací enginu odhaleno — a backend: každý ovladač vlastní pad vlákno (`Pady`), sloty, příkazy s číslem ovladače. Okno (Fáze 6): karta každého ovladače s vlastním přepínačem, „+ Ovladač“ (až 4), 🗑 odebere vypnutý ovladač 2–4 i s klávesami (dva kliky). Barva je pevně daná číslem — 1 modrá `#60a5fa`, 2 růžová `#f472b6`, 3 fialová `#a78bfa`, 4 azurová `#67e8f9` (otázka 51) — a nikdy nestojí sama, vždy s číslem: pruh a odznak karty, obrys a svícení čepiček. Karty se neukládají, odvozují se (otázka 52).)_
- [x] Hook se instaluje jen tehdy, když je potřeba: zapnutý ovladač nebo okno **v popředí** (živá detekce stisků). Jinak KeyPad na klávesnici vůbec nesahá (princip 10). _(Pravidlo `potreba_hooku` v `hook.rs`: režim mimo `Disabled`, nebo okno KeyPadu v popředí. Popředí hlídá hook vlákno přes `EVENT_SYSTEM_FOREGROUND` jen s viditelným oknem; okno jen viditelné, ale na pozadí, hook nedostane — viděl by klávesy psané jinam a spolkl by Scroll Lock v celém systému (otázka 42). Instalace a odebrání kvůli oknu jdou do logu jen jako `debug`.)_
- [x] Zkratka přepnutí (výchozí **Scroll Lock**, nastavitelná), reaguje jen na první key-down, nikdy na autorepeat. _(Nastavení zkratky v okně: Fáze 7. Bez zapnutého ovladače se zkratka s oknem v popředí spolkne a okno řekne „Nejdřív zapni ovladač“, otázka 48.)_
- [x] Zachytávání jde zapnout jen pro připojené ovladače.
- [x] (Bez dlouhých pokynů v GUI — minimum textu; zkratka je vidět v nastavení.) _(Karta ukazuje „Pozastaveno“ a tečku jen obrysem; co dělá Scroll Lock, řekne bublina.)_
- [ ] Volba „vždy navrchu“ (v nastavení, Fáze 7).

**Hotovo, když:** v gamepad režimu WASD hýbe páčkou v `joy.cpl`, Notepad písmena WASD nedostává, ostatní klávesy fungují. _Stav: automaticky ověřeno po kouscích (callback bez alokací, slot, výstup do slotů, instalace hooku jen se zapnutým ovladačem na skryté ploše, pad vlákna se stavem ze slotu až do ovladače, víc ovladačů, spánek, instalátor, konec). Skutečné klávesy → páčka v `joy.cpl` ověří vlastník — testy nesmí simulovat vstup a vstříknuté klávesy hook stejně propouští._

### Fáze 4b – Zpětná vazba vlastníka (30. 9. 2026)

Ověřeno vlastníkem na vydání `0.1.0+20260930.2241`: `joy.cpl` měří vstup z kláves, Scroll Lock převádění pozastaví a vrátí (funguje jen se zapnutým ovladačem — hook je v systému jen tehdy, viz Fáze 4).

- [x] **Znamení pozastavení mimo okno** — během hry je okno schované, a pozastavení (Scroll Lock) teď nejde poznat. Ikona v oznamovací oblasti podle režimu (zachytává / pozastaveno / vypnuto) a krátký zvuk při pozastavení a obnovení (vypne ho „✓ Zvuk“ v nabídce ikony; nastavení ve Fázi 7 volbu jen převezme). Žádné okno přes hru v popředí (princip 8). _(Ikona ve čtyřech variantách skládaných za běhu z ikony aplikace (`tray/ikona.rs`): šedá poloprůhledná = vypnuto, zelená tečka = hraje, dvě bílé čárky = pozastaveno, jantarový „!“ = klávesy nejdou nebo chyba ovladače; bublina říká stav a zkratku („KeyPad — hraje · Scroll Lock pozastaví (2 ovladače)“). Při přiřazování klávesy (Fáze 6) ikona ukazuje stav, do kterého se přiřazování vrátí (bez zapnutého ovladače „vypnuto“, jinak čárky), a bublina „KeyPad — přiřazuji klávesu“ — zkratka tehdy nic nepřepne, takže ji bublina neradí. Nabídka ikony: Otevřít · Pozastavit/Pokračovat · ✓ Zvuk · Ukončit. Zvuk: dvojtón přes WASAPI ve sdíleném režimu, generovaný přímo ve formátu směšovače; přehrává vlastní vlákno v relaci aplikace a klient se po každém zvuku zavře. Ne `PlaySoundW`: winmm si `wdmaud.drv` načte i ze složky programu, i po `SetDefaultDllDirectories` (sonda s podvrženými DLL). Ani WASAPI ale není bezpečná sama od sebe: COM načte absolutní cestou z registru jen MMDevApi.dll, AudioSes.dll si MMDevAPI dotahuje jménem — před podvrženou kopií chrání jen `SetDefaultDllDirectories(SYSTEM32)` jako první příkaz `main` (sonda: bez něj vznikl `AUDIOSES.DLL.LOADED`, s ním se nenačetl žádný ze 70 podvrhů); na rozdíl od winmm → wdmaud.drv tahle cesta omezené hledání respektuje. Zvuk se proto bez něj vůbec neotevře (`platform/windows/dll.rs`, test `wasapi_jen_s_hledanim_dll_v_system32`) a `check-imports.ps1` hlídá, že KeyPad.exe neimportuje winmm, MMDevAPI ani AudioSes. **Paměť** (`zvuk_selftest -- ticho`, jen nulové vzorky, Windows 10 19045, sdílený režim 48 kHz stereo float, dva běhy): první zvuk přidá procesu +0,7–0,8 MiB soukromé paměti a +2,9 MiB pracovní sady (hlavně sdílené obrazy MMDevAPI, AudioSes a jejich závislostí), první otevření 13–20 ms; KeyPad.exe zapíše svoje čísla do logu při prvním zvuku. COM na vlákně zvuku zůstává zapnutý po celý běh — `CoUninitialize` by ušetřilo jen asi 0,2 MiB soukromé paměti, knihovny zvuku v procesu zůstávají i po něm (pracovní sada neklesla). Kdy pípá a co při rychlém přepínání, viz otevřená otázka 45. Volba zvuku se od Fáze 6 ukládá do `config.json` (klíč `zvuk`). Na hlavním panelu bývá ikona schovaná v přetečení (^) — README radí ji přetáhnout na lištu, nic se nevynucuje registrem. Žádné balónky, toasty ani LED Scroll Locku.)_
- [x] **Zkratky KeyPadu nikdy s klávesou Win** (vlastník: Win+L zamyká počítač). Když zkratka potřebuje modifikátor, je to **Alt**. Platí pro nastavení zkratky (Fáze 7) i výchozí hodnoty; Win nejde ani přiřadit akci ovladače (Win musí zůstat funkční, Fáze 5). _(Win nejde namapovat, přiřadit ani použít jako zkratku (`KeyId::is_reserved`, `MappingError::Reserved`, přiřazování hlásí `BindingRejected { Reserved }` a Win jde do Windows). Drží-li OS Win, každý nový stisk patří Windows — i za hry a při přiřazování (otevřená otázka 44). Modifikátor přijde s nastavením zkratky ve Fázi 7 — jen levý Alt, jako volba (otázka 47). Okno při přiřazování Win odmítne větou „Win patří Windows“ a Win otevře Start.)_

_Ověří vlastník (hook ani zvuk testy na jeho ploše nespouští):_ ovladač zapnutý, okno schované, ve hře Scroll Lock → klesající tón a ikona s čárkami, znovu → stoupající tón a zelená tečka; Pozastavit/Pokračovat v nabídce ikony zní, odškrtnutý „Zvuk“ = ticho; při hře Win+D ukáže plochu a Win+E otevře Průzkumníka, Start se sám neotevře; výzva UAC během hry → tón pauzy (o půl sekundy později, viz otázka 45). Win+L záměrně ne (zamyká počítač). Do logu se zapíše, jak dlouho trvalo první otevření WASAPI a kolik paměti KeyPadu první zvuk přidal (číslo přímo z KeyPad.exe zatím chybí — testy aplikaci nespouštějí).

### Fáze 5 – Pojistky proti softlocku

- [x] **Watchdog padu:** hook vlákno při každé události v režimu Gamepad kontroluje heartbeat; starší než 1 s → vynutit Klávesnici. Totéž kontroluje GUI strana každých 500 ms. _(Pad vlákno kopíruje tep do slotu; callback ho kontroluje u každé klávesy v Gamepadu — čerstvé povolení ovladače se počítá jako tep, ohlášení „zapnuto“ totiž předbíhá kopii. Okno kontroluje `PadStatus` každých 500 ms, jen během hraní.)_
- [x] **Uzamčení / spánek:** message-only okno v hook vlákně, `WTSRegisterSessionNotification` a `WM_POWERBROADCAST`; při `WTS_SESSION_LOCK` / `PBT_APMSUSPEND` → `Engine::reset_held` (vynutí Klávesnici a vyprázdní `held`; key-up během zámku nepřijdou). _(Zamčení, přepnutí uživatele a odpojení vzdálené plochy: `WTSRegisterSessionNotification` ve skrytém okně relace (`relace.rs`, wtsapi32 za běhu ze System32) → `Zapomen(SessionLock)`. Spánek: pady se vypnou (Fáze 2b) → poslední vypnutý ovladač → `Disabled` → hook odebrán a držené klávesy zapomenuty.)_
- [x] **Zabezpečená plocha** (výzva UAC, Ctrl+Alt+Del) neposílá `WTS_SESSION_LOCK`, ale LL hook tam taky nevidí key-upy: `SetWinEventHook(EVENT_SYSTEM_DESKTOPSWITCH)` v hook vlákně → `reset_held(DesktopSwitch)`. Bez toho by po návratu zůstala páčka vychýlená, dokud se klávesa znovu nestiskne a nepustí. _(`SetWinEventHook(EVENT_SYSTEM_DESKTOPSWITCH)` mimo kontext v hook vlákně → `reset_held(DesktopSwitch)`; klávesy držené přes přepnutí pak převezme `adopt_os_key`.)_
- [ ] Zvážit totéž při přepnutí popředí na okno s vyššími právy (UIPI — hook mu klávesy nevidí). _(Zatím ne — otevřená otázka 39.)_
- [x] Nenamapované klávesy **se v režimu Gamepad vždy propouštějí** (Alt+Tab, Win, Alt+F4 zůstávají funkční). _(Fáze 1.)_
- [x] Mapování nesmí obsahovat zkratku přepnutí (validace v GUI i při načtení konfigurace). _(Jádro: `Mapping` to nedovolí vyrobit. Okno zkratku při přiřazování odmítne („Scroll Lock je pauza“) a `config.json` se zkratkou ve vazbách je nevalidní → záloha a výchozí klávesy (Fáze 6/7).)_
- [x] Pokud se hook vlákno zasekne, Windows hook po timeoutu přeskočí → klávesy jdou do OS. Zdokumentovat jako přijatelné selhání (není softlock). _(README, „Bezpečnost“.)_
- [x] Poslední záchrana (do README): Ctrl+Alt+Del nelze hookem zachytit → Správce úloh → ukončit aplikaci; hook i virtuální pad zmizí s procesem. _(README.)_
- [x] **Ukončení** (menu v oznamovací oblasti nebo událost `QUIT_EVENT_NAME` od instalátoru) = neutrální pad, odhook, odpojení targetu, v tomto pořadí. Zavření okna jen schovává do trayе. Otevřená otázka 21: má schování okna v režimu Gamepad vynutit Klávesnici? _(Fáze 4: neutrál → odhooknout → odpojit, celý konec pod jedním zámkem.)_

**Hotovo, když:** projdou všechny scénáře v tabulce edge cases níže. _Stav: pojistky ověřené testy (watchdog, přepnutí plochy, zapomenutí, panika mimo callback, klávesa držená OS). Skutečné Win+L, UAC a Ctrl+Alt+Del s drženou klávesou ověří vlastník — testy nesmí přepínat plochy ani simulovat vstup._

### Fáze 6 – GUI

- [x] Stavový řádek: ViGEmBus (OK / chybí + „Nainstalovat ViGEmBus“ / neběží + rada + „Zkusit znovu“), režim, poslední chyba. _(Stav padu a instalace ovladače hotové ve Fázi 2. Teď jeden pruh nad kartami (`Sbernice.svelte`), jen když je co hlásit: sběrnice (chybí / neběží / starší / instalátor běží) a konfigurace — jantarově „Klávesy nešly načíst — výchozí“ (cesta k záloze v bublině) nebo „Klávesy se neukládají“ (důvod v bublině). Režim nese každá karta jedním slovem a tečkou, chyby jsou v bublině a v logu.)_
- [x] **Nastavení kláves jako keybinds ve hře:** seznam vstupů ovladače (páčky, D-pad, A/B/X/Y, LB/RB, LT/RT, L3/R3, Start/Back) s přiřazenými klávesami (názvy přes `GetKeyNameTextW`); klik na vstup → „stiskni klávesu“ (Esc ruší, 10 s timeout) → uloženo; odebrání klávesy; „Obnovit výchozí“. Barva ovladače u kláves. _(Místo seznamu schéma ve tvaru Xboxu: každý vstup je „čepička“ s krátkým názvem klávesy (šipky, ␣, ↵, ⌫, ⇧… z `klavesy::kratky`, jinak název podle rozložení utnutý na 5 znaků; F13–F24, které Windows nepojmenují, dostanou název podle virtuální klávesy rozložení — dřív „#76“). Karty ovladačů jako akordeon — rozbalená je vždy jedna. Klik na čepičku klávesu **nahradí** (jako ve hrách), `+` po najetí myší **přidá** další, `×` nebo pravý klik vstup vyprázdní (otázka 40); poslední klávesu mapování odebrat nejde (`×` neaktivní, otázka 6). Přiřazování: čepička pulzuje, 10 s ubývá proužek (s „Omezit pohyb“ text „ještě N s“), Esc, klik jinam, ztráta popředí nebo schování okna ho zruší. „↺ Výchozí klávesy“ jen u ovladače 1 a bez brání kláves jiným ovladačům (otázka 49), dva kliky. Klávesy psané do vlastního okna nic nepřepnou: globální `keydown` dostane `preventDefault` (kromě Tabu), Enter a mezerník projdou jen s fokusem z klávesnice a tlačítko po kliknutí myší fokus pustí — mezerník = vstup A by jinak „klikl“ na přepínač s fokusem a připojil ovladač (princip 11). Zkratky prohlížeče WebView2 (F5, Ctrl+R, Ctrl+P…) jsou vypnuté (`ICoreWebView2Settings3::put_AreBrowserAcceleratorKeysEnabled(FALSE)`; na starém runtime tiše nic).)_
- [x] **Živá vizuální detekce stisků:** stisknutá klávesa hned rozsvítí svůj vstup (i když ovladač neběží) — jako v menu kláves ve hře. Zároveň živý náhled ovladače (páčky, tlačítka, triggery). _(Editor, živá detekce i náhled jsou jedna plocha. Plně svítí jen to, co hra opravdu dostává (stav padu ze slotu), držená klávesa, kterou hra nedostává, svítí obrysem („náhled“) — A+D svítí obě, vítěz SOCD plně. Hlavička páčky (L3/R3) se posouvá ve směru výchylky. Pruh karty se při stisku klávesy ovladače na 120 ms rozjasní i u sbalené karty. Nenamapovaná klávesa nerozsvítí nic: okno nikdy nedostane identitu stisknuté klávesy, jen bity vstupů (`LiveInputs`). Klávesy, které patří Windows, se ukazují jen s oknem v popředí; hook je kvůli tomu v systému jen s oknem v popředí (Fáze 4, otázka 42). Cena v callbacku (`hook_selftest -- mereni`, release, skutečné VK): p99 1,5–13,5 µs ve 42 bězích, typicky kolem 2 µs, žádný běh nad limitem 20 µs; živá detekce přidává asi 0,4 µs na p50 (otázka 44). Nečinnost: release na skryté ploše s viditelným oknem bez ovladače, 120 s — všechna vlákna KeyPadu 0,000 ms CPU.)_
- [x] **Jednoduché a minimalistické:** co nejméně textu, vysvětlivek a kódů; stav poznat z barvy a ikon, podrobnosti v tooltipu nebo logu. _(Hlavička karty: číslo, „Ovladač N“, jedno slovo stavu, tečka, přepínač. Jeden řádek nápovědy normálním písmem, ikony ▷ ↺ 🗑 vpravo. Kódy chyb jen v logu.)_
- [x] Upozornění na konflikty přímo u řádku (`MappingError` nese klávesu i akci). _(U čepičky: klávesa jiného vstupu nebo ovladače se hned **přesune** — stará čepička blikne jantarově (u jiného ovladače odznak jeho karty) a nápověda 5 s nabízí „Přesunuto z ovladače 2 · A · **Zpět**“ (otázka 41; „Zpět“ hlídá revize mapování). **Odmítnutí** zatřese čepičkou: „Win patří Windows“, „Scroll Lock je pauza“, „Tuhle klávesu nejde použít“. Alt na vstupu má jantarovou tečku („Při hraní nepůjde Alt+Tab“).)_
- [x] Úpravy kláves i za běhu ovladače: přiřazování dočasně pozastaví zachytávání a pak ho vrátí; držené klávesy si drží vlastníka (princip 2). _(Přiřazování pozastaví zachytávání všech ovladačů a vrátí ho (otázka 5). Vyprázdnění, výchozí klávesy, odebrání ovladače a „Zpět“ jdou za hry bez pozastavení přes `Engine::replace_mapping`: režim ani vlastníci držených kláves se nemění, držená klávesa dohraje se starou akcí a nový stisk jde podle nového mapování (otázka 43). Změna se do 0,5 s uloží do `config.json` (Fáze 7).)_

**Hotovo, když:** klávesy jde přiřadit, odebrat a vrátit v okně; stisk se ukáže na schématu; víc ovladačů má oddělené klávesy a barvy; nic z toho neohrozí klávesnici ani hru. _Stav: automaticky ověřeno testy (jádro: nahradit/přidat, `replace_mapping`, revize, živý stav, kódování oznámení — referenční model a mutanty; backend: příkazy, hlídání popředí, výstup hooku bez alokace, zlaté soubory smlouvy s oknem; okno: `bun test`) a **naostro na skryté ploše** `tools\okno-test.ps1` — debug build, simulovaný ovladač, klávesy do hooku jen příkazem `test_klavesa`, žádný systémový vstup: 116 kontrol (karty a barvy; klávesy psané do okna — mezerník a Enter na přepínači s fokusem, šipky, PageDown, End, F5, Ctrl+R, Ctrl+P jako důvěryhodné události CDP jen do WebView testu, Tab propuštěn, a v logu vypnuté zkratky WebView2; přiřazení, přesun a „Zpět“, `+` přidat, `×` i pravý klik vyprázdnit a „Zpět“, ↺ výchozí klávesy, 🗑 odebrat ovladač, klik jinam, 10s limit, Win, AltGr, Scroll Lock, živé svícení i za hry, rozvržení 440 i 380 × 620 / 480 px, minimalizace během přiřazování, restart, poškozená konfigurace i s chybou v bublině, F13–F24 s názvem), dva běhy po sobě, bez zbylého procesu i plochy. Ztrátu popředí bez minimalizace (Alt+Tab) skrytá plocha nemá — kryjí ji testy hooku. Skutečnou klávesnici, `joy.cpl` a Alt+Tab ověří vlastník (níž)._

_Ověří vlastník (kontrolní seznam; Win+L záměrně chybí — zamyká počítač; body 1–4 jsou u Fáze 4b):_

5. Okno v popředí, ovladač vypnutý: W A S D rozsvítí čepičky obrysem a hlavička páčky se hýbe; A+D svítí obě; mezerník nepřepne přepínač, šipky neposouvají panel, F5 neobnoví stránku.
6. Zapnout ovladač → čepičky svítí plně a `joy.cpl` ukazuje totéž.
7. Klik na „A“, stisk F → F přejde z X na A, X zůstane čárkované a „Zpět“ to vrátí.
8. Přiřazování a Win → otevře se Start, nic se nepřiřadí, nápověda „Win patří Windows“.
9. „+ Ovladač“, klik na vstup ovladače 2 a stisk W → „Přesunuto z ovladače 1 · levá ↑“. V `joy.cpl` jsou dva ovladače s oddělenými klávesami.
10. Klik na vstup, pak Alt+Tab do Poznámkového bloku a psaní → přiřazování je zrušené a písmena jdou do Poznámkového bloku (a samotné ťuknutí Altem v okně KeyPadu nic nespustí — otázka 55).
11. Restart KeyPadu → klávesy i ovladač 2 zůstanou. Smazat `%APPDATA%\KeyPad\config.json` → výchozí klávesy a soubor nevznikne, dokud se nic nezmění.
12. Česká QWERTZ: Z a Y mají správné názvy, číselná řada podle rozložení (otázka 50).
13. Idle: 2 min se schovaným i otevřeným oknem bez ovladače → CPU 0 ve Správci úloh. Paměť před prvním zvukem a po něm.
14. „Omezit pohyb“ ve Windows → odpočet přiřazování je dál čitelný.
15. Ikona přetažená z přetečení (^) na lištu je čitelná při 100 % i 150 %.

### Fáze 7 – Konfigurace a balení

- [ ] **Nastavení v aplikaci** (ozubené kolo): zkratka pozastavení, vždy navrchu, obnovit výchozí klávesy, aktualizace, **O aplikaci** (verze, credit ViGEmBus s odkazem, log). _(Zatím otevřené. „O aplikaci“ je pod ⓘ, výchozí klávesy jsou ↺ na kartě ovladače 1 (Fáze 6), zvuk je v nabídce ikony (Fáze 4b). Zkratku pozastavení jde zatím změnit jen ručně v `config.json` při ukončeném KeyPadu (klíč `zkratka`; běžící KeyPad soubor nesleduje a další změnou kláves by ho přepsal). Zkratka s modifikátorem: jen levý Alt, otázka 47.)_
- [x] `config.toml` — klávesy všech ovladačů, zkratka, volby; návrh (otevřená otázka 11): vždy `%APPDATA%\KeyPad\`. Dřív: vedle `.exe` v per-user instalaci je zapisovatelné; fallback `%APPDATA%\KeyPad\config.toml`). _(Hotové s Fází 6 — editor bez uložení by byl po restartu k ničemu. **`%APPDATA%\KeyPad\config.json`**, ne TOML: crate `toml` přidal `KeyPad.exe` 323 KiB, JSON přes `serde_json`, který v binárce už je, 31 KiB (naměřeno, princip 10). Nese klávesy všech ovladačů (`ovladac` 1–4, `vstup` = stabilní kód `Action::code`, `scan`, `e0`), zkratku a ✓ Zvuk; stav zapnutí ovladače nikdy (princip 11). Cestu má jen `updater::config_path()` — aplikace i odinstalace (ta konfiguraci i zálohu nechává). Soubor se nesleduje: ruční úpravu za běhu přepíše další změna z aplikace.)_
- [x] Chybějící soubor → vytvořit výchozí. Nevalidní → přejmenovat na `config.invalid.toml`, načíst výchozí, zobrazit varování (nikdy nepadat). _(Chybějící soubor vznikne až **první změnou** (otázka 46), start nic nezapisuje. Nevalidní (nejde parsovat, neznámý `vstup`, `ovladac` mimo 1–4, duplicita, Win, zkratka Esc, prázdné, `verze` chybí nebo 0) → `config.invalid.json`, výchozí klávesy, pruh „Klávesy nešly načíst — výchozí“ (bublina: cesta k záloze a nejvýš 3 chyby souboru — „řádek 1: …“, „vazba č. 3: neznámý vstup …“; v logu prvních 10 a počet). `verze` > 1 nebo nečitelný soubor → výchozí jen v paměti, soubor nedotčený, „Klávesy se neukládají“. Když nevalidní soubor nejde odložit, viz otázka 53. Ověřeno i naostro (`tools\okno-test.ps1`).)_
- [x] Atomický zápis (dočasný soubor + přejmenování). _(`.tmp` → `sync_all` → `rename`; píše jen vlákno `keypad-konfig`, 0,5 s po poslední změně a jen změněný obsah, při ukončení nejvýš 0,5 s čeká na dopsání. Selhání zápisu → pruh „Klávesy se neukládají“, další pokus při příští změně.)_
- [x] Pole `version` pro budoucí migrace. _(`verze = 1`; soubor z novější verze KeyPad nepřepíše.)_
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
| AltGr (CZ) při přiřazování | Odmítnut („Tuhle klávesu nejde použít“), nic se nepřiřadí — ani pravý Alt ťuknutím; přiřazování čeká dál (otázka 56) |
| Stejná klávesa pro dvě akce | Validace zamítne |
| Zkratka přepnutí přiřazena akci | Validace zamítne |
| ViGEmBus není nainstalován | Aplikace běží, přepnutí zakázáno, tlačítko „Nainstalovat ViGEmBus“ (KeyPadSetup /vigembus, UAC od Windows) |
| ViGEmBus nainstalovaný, ale neběží (zakázaný, čeká na restart, blokovaný) | Aplikace běží, rada podle stavu zařízení; instalátor se spustí jen tehdy, když je ovladač prokazatelně starší a nečeká na restart |
| ViGEmBus starší než 1.21.442 | KeyPadSetup ho aktualizuje (UAC); předtím KeyPad slušně ukončí a po aktualizaci ho zase spustí |
| Chyba ViGEm během hry | Vynucená Klávesnice + hláška + „Zkusit znovu“ |
| Pad vlákno spadne / zasekne se | Watchdog do 1 s vynutí Klávesnici |
| Klávesa držená při zapnutí ovladače / instalaci hooku / po zapomenutí držených kláves | Hook ji podle `GetAsyncKeyState` převezme jako klávesu OS — autorepeat i key-up jdou do OS, nic nevisí |
| Hook nejde nainstalovat | Zachytávání se nezapne (ani z Gamepadu nezůstane), okno ukáže „Klávesy nejdou“ |
| Hook vlákno spadne mimo callback | Ovladače dostanou neutrál, hook zmizí s vláknem, okno ukáže „vypnuto“ |
| Panika v hook callbacku | Klávesa propuštěna, režim Klávesnice, `held` vyprázdněn (`reset_held`) — i její key-up dostane OS |
| Win+L / uspání | Vynucená Klávesnice, `held` vyprázdněn |
| Výzva UAC / Ctrl+Alt+Del při držené klávese | Přepnutí plochy → vynucená Klávesnice, `held` vyprázdněn |
| Ztracený key-up (klávesa padu / spolknutá) | Další stisk po ≥ 1,5 s je nový stisk — nic se nespolkne navíc, zkratka přepne |
| Nemapovatelná klávesa (média, AltGr) kdykoli | Do OS, engine ji nesleduje |
| Druhá instance | Ukáže okno běžící instance, konec před instalací hooku |
| Nevalidní `config.json` | Záloha `config.invalid.json`, výchozí klávesy, pruh „Klávesy nešly načíst — výchozí“ (v bublině cesta k záloze a nejvýš 3 chyby souboru, v logu prvních 10 a počet; soubor znovu vznikne až první změnou) |
| Konfigurace z novější verze KeyPadu (`verze` > 1) | Nepřepsána, neukládá se; výchozí klávesy jen v paměti, pruh „Klávesy se neukládají“ |
| Konfigurace nejde uložit | Pruh „Klávesy se neukládají“ (důvod v bublině), další pokus při příští změně |
| Složka s `.exe` jen pro čtení | Konfigurace je vždy v `%APPDATA%\KeyPad`, log jde tam jako náhradní místo |
| Zavření okna | Okno se schová do oznamovací oblasti, WebView se uspí; ukončení z menu = neutrál → odhook → odpojení padu |
| Uspání počítače s připojeným padem | Pad se před uspáním odpojí a vypne (BSOD ViGEmBus #160); po probuzení zůstane vypnutý |
| Vypnutí / restart / odhlášení PC | Pad se odpojí a KeyPad skončí (skryté okno `KeyPad.KonecRelace`, WM_ENDSESSION) |
| Start KeyPadu | Žádný virtuální ovladač — objeví se až po zapnutí přepínače |
| Pád KeyPadu | Ovladač virtuální pad odpojí sám do ~6 ms (změřeno) |
| Pád procesu | Hook i virtuální pad zmizí s procesem |
| Editace mapování v režimu Gamepad | Povoleno bez pozastavení; držená klávesa dohraje se starou akcí, key-up spolknut; nový stisk podle nového mapování (otázka 43). Přiřazování klávesy za hry hru na tu dobu pozastaví všem ovladačům a pak ji vrátí |
| Binding bez stisku klávesy | Automatické zrušení po 10 s |
| Steam klient spuštěn jako správce | Hook z neprivilegovaného procesu mu klávesy neblokuje → varování v README: nespouštět Steam jako správce |
| Zkratka přepnutí během přiřazování | Nepřepne; odmítnuta jako vazba („Scroll Lock je pauza“), přiřazování čeká dál |
| Win při hře nebo při přiřazování | Vždy Windows (otevře Start), nic se nepřiřadí — „Win patří Windows“ |
| Win + klávesa při hře (Win+D, Win+E, Win+Tab) | Klávesa jde Windows, ne hře, až do svého uvolnění (otázka 44) |
| Okno KeyPadu ztratí popředí nebo se schová během přiřazování | Přiřazování zrušeno, nic se neuloží; přiřazování začaté ze hry se vrátí do hry |
| Hook nejde nainstalovat během přiřazování | Přiřazování zrušeno („Klávesy teď nejde sledovat“), ze hry vynucená Klávesnice |
| Scroll Lock s oknem KeyPadu v popředí bez zapnutého ovladače | Spolknut; přepínače jednou pulznou a okno řekne „Nejdřív zapni ovladač“ (otázka 48) |
| Klávesy psané do okna KeyPadu (mezerník, Enter, šipky, F5, Ctrl+R, Ctrl+P) | Nic nepřepnou ani neobnoví — okno je aplikace, ne prohlížeč (Tab a navigace klávesnicí fungují) |
| Bez zapnutého ovladače, okno schované nebo na pozadí | Žádný hook — KeyPad na klávesnici nesahá |
| Alt+Tab, Alt+F4, Ctrl+Shift+Esc během přiřazování | Celé jde do Windows, nic se nepřiřadí; přepnutí okna přiřazování zruší. Samotné ťuknutí modifikátorem se přiřadí při uvolnění (otázka 55) |
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
3. **Přiřazení klávesy, která už patří jiné akci.** Teď se vazba **přesune** (stará akce o klávesu přijde, UI dostane `moved_from`). Alternativa: odmítnout a nechat uživatele nejdřív odebrat starou vazbu. _Fáze 6: v okně viz otázka 41 (přesun hned a „Zpět“)._
4. **„Přiřadit“ = přidat, nebo nahradit?** Teď přidává (akce může mít víc kláves, odebírá se po jedné). Alternativa: nahradit všechny klávesy akce. _Fáze 6: klik na vstup nahradí, `+` přidá — otázka 40._
5. ✅ _Rozhodnuto Fází 6 (úpravy kláves i za běhu) a zavedeno ve Fázi 4: přiřazování jde z Klávesnice, Gamepadu i Disabled a vrací se tam, odkud začalo (z Gamepadu zpět na Gamepad, jen když mezitím nepřišlo vynucení)._ **Přiřazování v režimu Disabled** (ViGEmBus chybí). Doslovně „jen z režimu Klávesnice“ → teď zakázané. Kamarád by si ale mohl chtít rozvržení připravit dřív, než nainstaluje ViGEmBus. _Návrh:_ povolit a po skončení vrátit do `Disabled`.
6. **Prázdné mapování.** Teď je chybou validace a poslední vazbu nejde odebrat (hodnota `Mapping` je vždy platná). Alternativa: jen varování. _Fáze 4: pravidlo platí pro celé mapování; jednotlivý ovladač bez kláves chybou není (nový ovladač začíná prázdný). Důsledek: když má klávesy jen druhý ovladač, nejde ho odebrat (`clear_pad` → `WouldBeEmpty`)._ _Fáze 6: v okně jsou `×` i pravý klik u vstupu s poslední klávesou mapování neaktivní (bublina „Poslední klávesu nejde odebrat“) a 🗑 u ovladače se všemi klávesami taky („Nejdřív dej klávesy jinému ovladači“)._
7. **Esc jako zkratka přepnutí** — teď zakázané validací (jinak by se z přiřazování nedalo vycouvat Esc). Specifikace to neřeší.
8. ✅ _Vyřešeno revizí Fáze 4: hook u key-downu klávesy, o které engine neví, zjistí z asynchronního stavu klávesnice (`GetAsyncKeyState`), jestli ji OS drží — pak ji převezme jako klávesu OS (`Engine::adopt_os_key`) a autorepeat i key-up jdou do OS. Týká se i klávesy držené při instalaci hooku a při zapnutí ovladače (revize to prokázala testem: levý Shift = L3 → vše velkými)._ **Klávesa držená přes zamčení relace.** `reset_held` zapomene `held`; LL hook nerozliší autorepeat od nového stisku, takže autorepeat té klávesy po odemčení je pro engine nový stisk. Kdyby mezitím uživatel přepnul na Gamepad a klávesa byla namapovaná, OS by viděl key-down (před zámkem), ale key-up by se spolkl → klávesa „visí“ v OS do dalšího stisku. Velmi nepravděpodobné (držet klávesu přes zámek a mezitím přepnout); teď se to přijímá. Alternativa: po zámku nechat staré záznamy jako `Os` „na dožití“ (pak by se naopak první stisk po odemčení mohl ztratit padu).
9. **Instalace per-user vs. Program Files.** WinSent instaluje do Program Files s právy správce; KeyPad kvůli principu 6 do `%LOCALAPPDATA%\Programs\KeyPad` bez UAC (jako VS Code, Discord). OK?
10. **Druhá instance** — roadmapa chtěla hlášku a konec; teď (`tauri-plugin-single-instance`) druhé spuštění jen ukáže okno běžící instance. Plynulejší, ale jiné, než stálo v zadání.
11. **Kde má bydlet `config.toml`?** Roadmapa: vedle `.exe`. V per-user instalaci je to zapisovatelné, jenže se to plete s binárkami (odinstalace ho musí obcházet). _Návrh:_ vždy `%APPDATA%\KeyPad\config.toml` (data odděleně od programu, přežijí přeinstalaci). _Kód (Fáze 6/7):_ vždy `%APPDATA%\KeyPad\config.json` (`updater::config_path()`) — JSON místo TOML kvůli velikosti binárky (Fáze 7); vzniká až první změnou (otázka 46).
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
22. **„Vyzkoušet páčku“** jede plnou výchylkou (kruh ~1,2 s, pak neutrál). ~~Ve Fázi 4 povolit jen v režimu Klávesnice~~ _Fáze 4: jde i při zachytávání — zapnutí ovladače zachytávání rovnou spouští, takže by tlačítko jinak skoro nikdy nešlo. Stisk klávesy ovladače kruh přeruší (vstup z klávesnice má přednost)._
23. **Paměť WebView2 schovaného v trayi** — _rozhodnuto:_ při schování `MemoryUsageTargetLevel = LOW`. Fyzická paměť (co ukazuje Správce úloh) klesne z ~345 MB na ~50 MB (po 2 min ~120 MB), soukromá zůstává ~160 MB; CPU ~0. Víc bez zavření WebView nejde.
24. ✅ _Rozhodnuto 29. 9.: ViGEmBus se instaluje i aktualizuje automaticky, bez zaškrtávátka, i při aktualizaci z aplikace (Fáze 2b); souhlas = výzva UAC od Windows._ **Zaškrtávátko „Nainstalovat i ovladač ViGEmBus“ je předvyplněné**, když ovladač chybí — na přání vlastníka, ať kamarád nepotřebuje nic dalšího. Souhlas = viditelné zaškrtávátko s vysvětlením + klik + výzva UAC od Windows. Nabízí se i při ručním spuštění KeyPadSetup nad už nainstalovaným KeyPadem, nikdy při aktualizaci z aplikace (`/quiet`). Nechat předvyplněné?
25. **Zbytkové riziko instalace ViGEmBus:** oficiální instalátor (s právy správce) běží ze složky v %TEMP%, do které může psát i uživatel. Soubor sám je zamčený a ověřený, ale jestli si instalátor Advanced Installer bezpečně načítá své DLL, závisí na jeho vlastním zabezpečení (neověřeno spuštěním). Okno = výzva UAC + běh instalace. KeyPad je přesto bezpečnější než WinSent, jehož instalátor běží celý jako správce ze Stažených souborů.
26. **Instalace ViGEmBus není ověřená naostro** — na tomhle PC je ViGEmBus nainstalovaný (nesmí se měnit), cesta s UAC se dá otestovat jen ve virtuálu (čistý Windows 10/11). Do té doby ověřeno po kouscích: stažení + hash, zámek souboru, podpis, stav ovladače, obrazovky.
27. ✅ _Zastaralé: „Připojit znovu“ zmizelo (Fáze 2b) — po probuzení zůstává ovladač vypnutý; kliknutí těsně před spánkem hlídá 5s pojistka._ **„Připojit znovu“ ve spánku** — když po probuzení nepřijde oznámení (Modern Standby…), tlačítko pad připojí ručně. Teoreticky kdyby ho někdo zmáčkl v mezičase mezi oznámením o uspání a skutečným spánkem, pad by se připojil těsně před spánkem (oblast BSOD #160). Přijatelné?
28. **Nesouhlasná verze ovladače** (odpověď 1/50/87 na kontrolu verze) → chyba padu s radou odebrat „ViGEm Bus Driver“ v Aplikacích a zkusit znovu; potom aplikace nabídne instalaci.
29. ✅ _Vlastník 30. 9.: průhlednost i tahání na Windows 10 v pořádku._ **Tahání okna na Windows 10** — KeyPad má na Windows 10 přesně stejné nastavení pozadí jako WinSent (blur, bez vlastních rohů a rámečku). Pokud tahání zadrhává i tak, je potřeba vědět na jakém PC/buildu — ověřit se to dá jen skutečným tažením myší (zakázané v testech). Na Windows 11 je Mica.
30. **Číslo hráče u víc ovladačů** — ViGEmBus vrací při dvou virtuálních ovladačích stejný index pro oba (naměřeno), okno proto „hráč N“ neukazuje. Pro Fázi 4 (víc ovladačů) najít jiný spolehlivý zdroj.
31. **Aktualizace ViGEmBus, která starý ovladač odebere a nový nepřidá** (známá chyba dodavatele „spusť instalaci dvakrát“): co udělá instalátor MSI nad už zaregistrovaným produktem, se dá ověřit jen ve virtuálu. Do té doby KeyPad v tomhle stavu nic neopakuje (druhé automatické spuštění instalátoru bylo odebráno — nikdy víc než jedno spuštění) a poradí odebrat „ViGEm Bus Driver“ v Aplikacích a spustit KeyPadSetup znovu.
32. **Aktualizace ovladače zavře KeyPad** (nesmí držet sběrnici) a pak ho znovu spustí — ve všech režimech, i z tlačítka v aplikaci. Tlačítko „Aktualizovat ovladač“ se ukazuje i při zapnutém ovladači; kliknutí ovladač nejdřív vypne.
33. **Logování stisků.** Fáze 3 chtěla „log ukazuje scan kódy“. Aplikace ale stisky do `keypad.log` **nikdy** nezapisuje — log by jinak obsahoval i hesla a kamarád by ho posílal při hlášení chyby. Scan kódy vypisuje jen příklad `hook_selftest`, a to jen do konzole. Kdyby byla potřeba diagnostika u kamaráda, návrh: jen nemapovatelné klávesy (AltGr, média) a jen scan kód, zapnuté proměnnou prostředí.
34. **Zapnutí ovladače = hned hrát.** Připojený ovladač (vždy po kliknutí na přepínač) engine povolí a spustí zachytávání (`capture`). Když uživatel předtím zachytávání pozastavil Scroll Lockem a zapne DALŠÍ ovladač, pozastavení se tím zruší pro všechny. Alternativa: zapnutí dalšího ovladače pozastavení respektuje.
35. **Upřesnění přiřazování (Fáze 4, zjistil agent při implementaci):** (a) `capture()` během přiřazování bez připraveného ovladače jen nastaví „pak zachytávat“ — po konci jde engine do `Disabled` a příznak zanikne; (b) nové `start_binding` se stejným cílem pošle `ModeChanged` i beze změny režimu; (c) `disable()` nepřipraveného ovladače si pamatuje důvod — po konci přiřazování v `Disabled` tak může být vidět důvod od jiného ovladače; (d) přiřazování začaté ve hře nejde zkratkou ani tlačítkem „přepnout“ změnit na „po uložení nehrát“ (zkratka se odmítne jako vazba) — jen vynucením nebo změnou mapování; (e) odmítnuté `capture()` hlásí `ToggleRejected { Disabled }`.
36. **Stav padů při změně režimu jde všem 4 ovladačům** (i nepřipojeným — vždy neutrál). Aplikace ho zapíše do jejich slotů; vlákno nepřipojeného ovladače neběží nebo stav nepošle. Engine při každé klávese přepočítá všechny 4 ovladače (4× tabulka 256 položek, bez alokace) — princip 7 má přednost před mikrooptimalizací.
37. **Hook se instaluje jen se zapnutým ovladačem** (nebo při přiřazování; od Fáze 6 i s oknem KeyPadu v popředí — živá detekce, otázka 42); po vypnutí posledního ovladače zmizí a držené klávesy se zapomenou. Zapnutí z pozastavení hook vždy přeinstaluje (ochrana proti tichému odebrání), zapnutí druhého ovladače během hry ne (zapomněly by se klávesy, které hráč 1 drží).
38. **Převzetí klávesy OS podle virtuální klávesy.** `GetAsyncKeyState` se ptá na VK z události. Dvě fyzické klávesy se stejnou VK (šipka nahoru a 8 na numerické klávesnici při vypnutém NumLocku) se tak pletou: drží-li uživatel jednu v OS a stiskne druhou (namapovanou), převezme se jako klávesa OS a ovladač ji do uvolnění nedostane. Velmi vzácné, bezpečná strana (klávesa jde do Windows, nic nevisí). Alternativa: ptát se přes scan kód (`MapVirtualKey`) — dražší a na rozloženích nejednoznačné.
39. **Okno s právy správce v popředí (UIPI).** Hook z neprivilegovaného KeyPadu nevidí klávesy mířící do okna spuštěného jako správce — key-up klávesy držené při přepnutí do takového okna se ztratí. Teď to řeší jen obecné pojistky: ztracený key-up (1,5 s) a převzetí klávesy OS. Zjišťovat práva popředí (`EVENT_SYSTEM_FOREGROUND` + token cizího procesu) by znamenalo sahat na cizí procesy (princip 8) — proto zatím ne. README radí nespouštět Steam jako správce.

40. **Klik na vstup klávesu nahradí, nebo přidá?** _Kód (Fáze 6): nahradí_ — jako v menu kláves her (`BindKind::Replace`: ostatní klávesy vstupu se odeberou). Další klávesu přidá malé `+`, které se ukáže po najetí myší na čepičku (`BindKind::Add`); `×` nebo pravý klik vstup vyprázdní. Shift+klik (návrh) zamítnut: skryté gesto a za hry by levý Shift (= L3) spolkl hook. Alternativa: klik přidá (dřívější chování, otázka 4).
41. **Klávesa, která už patří jinému vstupu nebo ovladači.** _Kód: přesune se hned._ Stará čepička blikne jantarově (u jiného ovladače blikne odznak jeho karty) a nápověda 5 s nabízí „Přesunuto z ovladače 2 · A · **Zpět**“. „Zpět“ vrátí přesně předchozí mapování, jen když ho mezitím nic nezměnilo (revize; jinak „Vrátit už nejde.“). Alternativy: druhý stisk téže klávesy jako potvrzení (zamítnuto — přidává stav a je slabý), nebo odmítnout.
42. **Živá detekce jen s oknem v popředí, nebo už s viditelným oknem?** _Kód: jen v popředí_ (hook vlákno hlídá `EVENT_SYSTEM_FOREGROUND` a porovná `GetAncestor(GetForegroundWindow(), GA_ROOT)` s oknem KeyPadu — `WindowEvent::Focused` z Tauri je fokus WebView, ne popředí). S oknem jen viditelným by hook bez zapnutého ovladače viděl klávesy psané do jiných programů (soukromí) a spolkl by Scroll Lock v celém systému. Cena: časté instalace a odebírání hooku při Alt+Tab (jen bez zapnutého ovladače; log jen `debug`) — kdyby si antivirus stěžoval, zvážit dozvuk. **Změřeno** (riziko 2 spec Fáze 6, `hook_selftest -- mereni-instalace`, release, vlastní skrytá plocha, 4 běhy po 2 000 kolech, Windows 10 19045): `SetWindowsHookExW` p50 1,4–1,5 µs, `UnhookWindowsHookEx` p50 1,1–1,2 µs; dvojice p99 7,7–34 µs, nejhorší 0,24 ms. Četnost: bez zapnutého ovladače 1 instalace, když okno KeyPadu získá popředí, a 1 odebrání, když ho ztratí — s oknem schovaným nebo minimalizovaným ani se zapnutým ovladačem žádná. Na jedno přepnutí okna jsou to jednotky mikrosekund (princip 10); jak se k častým instalacím staví antiviry, tímhle měřením zjistit nejde. Alternativa: i s oknem jen viditelným.
43. **Úpravy kláves za hry bez pozastavení.** Tabulka okrajových případů měla „Editace mapování v režimu Gamepad | Zakázáno“. _Kód: povoleno_ (`Engine::replace_mapping`): vyprázdnění, výchozí klávesy, odebrání ovladače a „Zpět“ za hry nic nevynucují; držená klávesa dohraje se starou akcí a její key-up se spolkne (princip 2), nový stisk jde podle nového mapování. Přiřazování samo hru na chvíli pozastaví všem (otázka 5). Ověřeno referenčním modelem a mutanty. Alternativa: za hry editor zamknout.

44. **Win + klávesa při hře patří Windows.** _Kód: ano._ Drží-li OS levou nebo pravou Win (`GetAsyncKeyState`), hook nový stisk jakékoli klávesy převezme jako klávesu OS (`adopt_os_key`) — ve všech režimech i při přiřazování. Win+D, Win+E a Win+Tab tak fungují i za hry a osamělá Win neotevře Start (Windows by jinak viděly Win bez druhé klávesy, kterou spolkl ovladač). Klávesa zůstane Windows až do uvolnění, i když Win pustíš dřív. Důsledek: hráč s drženou Win nepošle klávesu do hry — bezpečná strana (klávesa jde do Windows, nic nevisí). Alternativa: jen mimo Gamepad. _Jak se to zjišťuje (revize B4, kvůli p99):_ stisk a uvolnění Win vidí callback sám (Win jde vždy do OS, i vstříknutá), takže se Windows (`GetAsyncKeyState`) ptá na Win jen tehdy, když je podle toho dole — a výsledek bit opraví (uvolnění na zabezpečené ploše po Win+L callback nevidí). Po instalaci hooku a po přepnutí plochy se stav Win srovná s Windows. Na samotnou klávesu (převzetí klávesy OS, otázka 8) se hook ptá jen u stisku, o kterém engine rozhoduje (zkratka, přiřazování, klávesa připraveného ovladače při hře, `Engine::claims_new_press`) — jinde by převzetí dopadlo stejně jako nový stisk (hlídá property test). Zbývá jediný dotaz na stisk ovladače; `hook_selftest -- mereni` se skutečnými VK: p99 1,5–13,5 µs ve 42 bězích, dřív 3 dotazy a 6 z 21 běhů přes 20 µs. Okrajový případ: kdyby Win-down callback minul (zahlcený hook, jiný hook před ním) a Windows ji přesto držely, klávesa s ní by šla hře, ne Windows.
45. **Kdy pípat.** _Kód_ (`gamepad::znameni`: `zvuk_pro` testovaný celou tabulkou, posloupnosti přes `Znameni` i celou cestou engine → výstup hooku → vlákno okna): klesající tón při pozastavení zkratkou, nabídkou ikony a pojistkou (watchdog, chyba padu, panika hooku, výzva UAC, přeinstalace hooku, změna mapování), když hook přestane jít, a když za hry vypadne poslední ovladač s chybou (pozná se podle důvodu přechodu `Disabled { PadError }`, ne podle toho, že je některý ovladač v chybě — vypnutí posledního přepínačem tak nepípne, ani když jiný v chybě visí); stoupající při návratu do hry z pauzy (i zapnutím dalšího ovladače, otázka 34) a po obnově hooku. Ticho: zamčení, spánek, konec aplikace, vypnutí ovladače přepínačem, první zapnutí ovladače (Windows hrají svůj zvuk připojení a odpojení), cokoli s přiřazováním, simulace ViGEmBus. „Zvuk“ jde vypnout v nabídce ikony. **Zamčení a výzva UAC:** zamčení (Win+L, Start → Zamknout) Windows ohlásí nejdřív jako přepnutí na zabezpečenou plochu a zamčení relace až po něm (log vlastníka z 0.1.0: o 4–6 ms), takže pozastaví pojistka plochy a příčinu zapíše ona. Tón k přepnutí plochy proto čeká 500 ms (`ODKLAD_PLOCHY_MS`) a zamčení v té době ho zruší — výzva UAC pípne o půl sekundy později. Obrazovka Ctrl+Alt+Del je taky zabezpečená plocha: pípne jako výzva UAC, následné „Zamknout“ už ne. Novější změna režimu během odkladu tón nahradí (uspání hned po přepnutí plochy zůstane tiché, návrat do hry pípne jen „hra“). **Tóny rychle za sebou** (`zvuk.rs`): rozehraný tón se neutne, dohraje celý (asi 0,2 s — useknutá sinusovka by lupla), a z požadavků, které mezitím přijdou, zazní hned po něm jen poslední. Nic se nepřekrývá a ve frontě nikdy nečeká víc než jeden tón. WASAPI předchozí zvuk sama neutne, jak to dělal `PlaySoundW` se `SND_ASYNC`, se kterým počítala specifikace (a proto zamítla 150ms odklad zvuku). Dvakrát Scroll Lock do 0,1 s tak zahraje celou „pauzu“ a po ní „hru“ a tón skutečného stavu přijde asi o 0,1–0,15 s později než s utnutím. _Na rozhodnutí:_ (a) spadne-li celé hook vlákno (panika mimo callback), je to přechod do vypnuto bez chyby ovladače → jen ikona, bez tónu; (b) chyba jednoho ze dvou hrajících ovladačů režim nemění → ticho a ikona dál „hraje“ (chybu ukáže okno); (c) „Pokračovat“ v nabídce posílá `Zachytavej`, ne `Prepni`: z pozastavení nejdřív přeinstaluje hook jako zapnutí přepínačem (Windows ho mohli potichu odebrat; stisk Scroll Locku naopak sám dokazuje, že hook žije); (d) bublina během zapínání ovladače říká „vypnuto“; (e) místo odkladu by přepnutí plochy mohlo nepípat vůbec (výzva UAC sama ztmaví obrazovku) — jednodušší, jenže hráč by nepoznal, proč klávesy přestaly hrát, když výzva vyskočí pod hrou přes celou obrazovku; (f) tóny rychle za sebou (výš) nechat, nebo rozehraný tón utnout krátkým doběhem (buffer plnit po kouscích, při novém požadavku dopsat asi 5 ms doběhu a pak `Stop`); levnější mezikrok: starý tón zahodit, když nový přijde ještě před `Start` (pomůže hlavně při prvním, pomalejším otevření).

46. **Kdy vznikne konfigurace.** Fáze 7: „chybějící soubor → vytvořit výchozí“. _Kód: až první změnou_ (přiřazení, vyprázdnění, výchozí klávesy, ✓ Zvuk…). Start nic nezapisuje, takže samotné spuštění KeyPadu po sobě v `%APPDATA%` nic nenechá a výchozí klávesy v příští verzi se projeví i tomu, kdo nic neměnil. Alternativa: zapsat výchozí hned při prvním startu.
47. **Modifikátor zkratky pozastavení (Fáze 7).** Vlastník: zkratky nikdy s Win, modifikátor jen Alt. _Návrh:_ jen **levý** Alt (pravý je na české klávesnici AltGr) a jen jako volba; výchozí zůstává samotný Scroll Lock — osamělé ťuknutí Altem by v okenních programech otevřelo nabídku. Do konfigurace se zapíše (`alt`) až s `verze = 2`, aby starší KeyPad soubor nepřepsal. _Kód:_ zatím nic (zkratka je jedna klávesa).
48. **Scroll Lock v okně KeyPadu bez zapnutého ovladače.** Hook je tehdy v systému jen kvůli živé detekci (okno v popředí) a zkratka se v každém režimu spolkne. _Kód: spolknout_ — přepínače karet jednou pulznou a nápověda řekne „Nejdřív zapni ovladač“ (zmizí po 2,5 s, nebo hned, jakmile se ovladač zapne). Alternativa: v `Disabled` zkratku pustit do Windows (přepne LED Scroll Locku, nic neřekne).
49. **„Výchozí klávesy“ (↺).** _Kód:_ jen u ovladače 1 a jen jeho klávesy (dva kliky). Klávesy, které mezitím patří jinému ovladači, ani zkratku nebere — příslušný vstup ovladače 1 zůstane čárkovaný (`Mapping::defaults_for_first_pad`). Jinak by obnovení výchozích potichu sebralo klávesy druhému hráči. Alternativa: vrátit výchozí všem (ostatní ovladače bez kláves).
50. **Číselná řada na české klávesnici.** Mapování je podle pozice (scan kód, princip 5), názvy podle rozložení: LT a RT (výchozí klávesy 1 a 3) mají na čepičce „+“ a „š“ (vidět na snímcích z `tools\okno-test.ps1`). _Kód: podle rozložení._ Alternativa: u horní řady ukazovat „1 2 3“ (co je na klávese napsané menším písmem).
51. **Barvy ovladačů.** _Kód: pevně podle čísla_ — 1 modrá, 2 růžová (nejvzdálenější odstín pro nejčastější dvojici), 3 fialová, 4 světle azurová; žádná se neplete se zelenou (zapnuto), jantarovou (pozor) ani červenou (chyba) a nikdy nestojí bez čísla. Bez volby. Alternativa: výběr barvy v nastavení (Fáze 7).
52. **Přidaný prázdný ovladač po restartu zmizí.** _Kód:_ seznam karet se neukládá, odvozuje se — vidět je ovladač 1, ovladač s klávesami, zapnutý ovladač a ovladač přidaný v tomhle sezení („+ Ovladač“, nebo kterému uživatel právě vyprázdnil poslední klávesu). Ověřeno `tools\okno-test.ps1`: po restartu zůstane karta 2 s klávesou, prázdné karty 3 a 4 zmizí. Alternativa: ukládat seznam karet (riziko skrytých vazeb mezi seznamem a mapováním).

53. **Nevalidní konfigurace, kterou nejde odložit.** Specifikace: nevalidní soubor se přejmenuje na `config.invalid.json` (přepíše starší zálohu), platí výchozí klávesy a dál se ukládá. Neřeší, co když zálohu vytvořit nejde (selže přejmenování i kopie — na místě zálohy je složka, nebo ji drží jiný program). _Kód_ (`config::nacti`): soubor se pak nepřepíše (byl by to jediný otisk uživatelových kláves), výchozí klávesy platí jen v paměti a stav je `necitelna` — okno ukáže „Klávesy se neukládají“, ne „Klávesy nešly načíst — výchozí“ s odloženým souborem, který neexistuje (princip 8). Atribut „jen pro čtení“ KeyPad ze zálohy sundá před přepsáním i po něm: přejmenovaná záloha ho zdědí od `config.json`, který si uživatel zamkl proti přepsání, a příští nevalidní soubor by pak už nešel odložit (ověřeno: přístup odepřen u přejmenování i kopie). Alternativa: vlastní stav s větou „Nevalidní soubor nejde odložit“.
54. **Co odinstalace hlásí v `%APPDATA%\KeyPad`.** Specifikace: hláška ukáže složku, když existuje `config.json`. _Kód_ (`clean_roaming` v instalátoru): ukáže ji, kdykoli složka po úklidu zůstala. Maže se jen log a `version.txt` a složka jen prázdná, takže cokoli dalšího patří uživateli — i samotná záloha `config.invalid.json` po obnově nevalidního souboru (`config.json` vznikne až první změnou, otázka 46), `.tmp` přerušeného zápisu nebo jeho vlastní soubor. Název zálohy je v `updater` (`CONFIG_BACKUP_FILE`), jako název konfigurace.
55. **Alt, Ctrl a Shift při přiřazování.** Specifikace si protiřečí: Alt jde přiřadit (jantarová tečka „Při hraní nepůjde Alt+Tab“), a zároveň „klik na vstup, pak Alt+Tab do Poznámkového bloku → přiřazování je zrušené a písmena jdou do Poznámkového bloku“ (kontrolní seznam vlastníka, bod 10). Dřív engine Alt uložil a spolkl už při key-downu, Windows Alt neviděly a Tab jen posunul fokus v okně KeyPadu — vstup potichu dostal Alt a okno se nepřepnulo; totéž Ctrl z Ctrl+Shift+Esc a Alt z Alt+F4. _Kód (revize B4):_ při přiřazování jde stisk modifikátoru (levý i pravý Ctrl, Shift, Alt) do Windows a přiřadí se až při uvolnění, když mezitím nepřišel žádný jiný stisk (ťuknutí). Drží-li Windows modifikátor, každý další stisk patří Windows a nic se nepřiřadí — i Esc (Ctrl+Shift+Esc, Shift+Esc přiřazování neruší); po puštění modifikátorů se přiřazuje dál. Alt+Tab tak přepne okno a přiřazování zruší ztráta popředí. Modifikátor spolknutý ze hry (levý Shift = L3) zkratku netvoří, Windows ho neviděly. Důsledky: modifikátor se přiřadí až po puštění (do té doby čepička nebliká), a Windows při přiřazování dostanou samotné ťuknutí Altu — okno KeyPadu nemá nabídku, takže by nemělo nic spustit; neověřeno naostro (test okna klávesy posílá mimo Windows), ověří vlastník u bodu 10. Alternativy: (b) chování nechat a bod 10 kontrolního seznamu dělat myší; (c) modifikátory při přiřazování odmítnout (`BindingRejected`).
56. **AltGr při přiřazování.** Specifikace (1.4): AltGr → zatřesení a „Tuhle klávesu nejde použít“. Na české klávesnici ale Windows k AltGr pošlou falešný levý Ctrl (scan 0x21D, nemapovatelný) **a** skutečný pravý Alt (E0 0x38), a ten se podle otázky 55 ťuknutím přiřadil: okno napsalo „Tuhle klávesu nejde použít“ a vzápětí uložilo „P Alt“ (nalezeno revizí úplnosti, ne během). Hra by s takovou vazbou dostávala i falešný Ctrl, který jde vždy Windows. _Kód (`Engine`, `bind_altgr`):_ falešný Ctrl při přiřazování si engine zapamatuje a pravý Alt **hned** po něm se ťuknutím nepřiřadí; jde Windows jako každý modifikátor. Okno dostane jediné odmítnutí, přiřazování čeká dál. Pravý Alt bez falešného Ctrl (anglické rozložení) se ťuknutím přiřadí dál a ručně zapsaný v `config.json` platí. Ověřeno testem enginu, referenčním modelem (operace AltGr v property testu, mutant „pravý Alt po falešném Ctrl se přiřadí“ zabit za 4 s) a naostro na skryté ploše (`tools\okno-test.ps1`, syntetické klávesy 0x21D + E0 0x38). Alternativa: AltGr přiřadit jako „P Alt“ (falešný Ctrl by ale dál šel do hry).

---

## Mimo rozsah verze 1

Myš jako pravá páčka, podpora macOS/Linux, analogový „chůze“ modifikátor, podpis kódu. Architektura je nesmí znemožnit, ale neimplementují se. _(Automatické aktualizace a víc virtuálních ovladačů už mimo rozsah nejsou — viz „Instalace a aktualizace“ a Fázi 4.)_

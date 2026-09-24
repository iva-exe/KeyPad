# tauri.ps1 — Tauri CLI pro rozložení tohohle repozitáře.
#
# Použití (odkudkoli, cesty se berou od kořene repozitáře):
#   tools\tauri.ps1 dev                  vývoj: Vite na :1420 + aplikace s hot reloadem
#   tools\tauri.ps1 build --no-bundle    release: target\release\KeyPad.exe s vestavěným UI
#   tools\tauri.ps1 icon src-tauri\icons\icon.svg -o src-tauri\icons
#
# Všechny argumenty jdou do CLI beze změny (i `--`, za kterým jdou
# argumenty pro cargo: `build --no-bundle -- --locked`) a skript končí
# jeho návratovým kódem ($LASTEXITCODE). Když se CLI nespustí vůbec,
# končí kódem 1. Na tom stojí tools\publish.ps1 — tuhle smlouvu neměnit.
#
# PROČ WRAPPER
# src-tauri\ a ui\ jsou sourozenci. CLI (leží v ui\node_modules) si
# projekt hledá procházením složek od AKTUÁLNÍHO adresáře dolů:
#   - spuštěné z ui\ src-tauri\ nenajde vůbec („Couldn't recognize the
#     current folder as a Tauri project"),
#   - spuštěné z kořene ho najde, ale jen heuristikou (první
#     tauri.conf.json a package.json do hloubky 3) — stačí další
#     package.json v repozitáři a CLI může sáhnout vedle.
# Proto se cesty říkají výslovně proměnnými TAURI_APP_PATH (složka
# s tauri.conf.json) a TAURI_FRONTEND_PATH (složka s package.json)
# a CLI se pouští z kořene. Ověřeno s @tauri-apps/cli 2.11 (`tauri info`
# z ui\ s proměnnými vidí aplikaci i frontend).
#
# beforeDevCommand/beforeBuildCommand v tauri.conf.json mají "cwd": "../ui"
# — relativně ke src-tauri\, kam se CLI před spuštěním přesune.
#
# PROČ NE `cargo build -p keypad`
# `tauri::generate_context!` vestaví soubory frontendu do binárky jen
# s feature tauri/custom-protocol, kterou zapíná CLI. Holý cargo build
# vyrobí binárku, která místo UI ukáže „localhost se odmítl připojit".

# Schválně 'Continue', ne 'Stop' — a výslovně, protože PowerShell tuhle
# proměnnou dědí z volajícího (publish.ps1 má 'Stop'). CLI píše průběh
# na stderr a PowerShell 5.1 z každého takového řádku při přesměrování
# (volající s 2>&1 nebo *>) udělá NativeCommandError; se 'Stop' by build
# „spadl" hned na prvním informačním řádku. Neúspěch se pozná podle
# návratového kódu CLI, ne podle stderr.
$ErrorActionPreference = 'Continue'

# Výstup přesměrovaný do roury (log CI) by se kódoval kódovou stránkou
# konzole a z češtiny by zbyly otazníky — stejně jako v ostatních
# skriptech v tools\.
if ([Console]::IsOutputRedirected) { [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false) }

$root = Split-Path -Parent $PSScriptRoot
$cli = Join-Path $root 'ui\node_modules\.bin\tauri.exe'
if (-not (Test-Path $cli)) {
    Write-Host "Chybí Tauri CLI: $cli" -ForegroundColor Red
    Write-Host 'Nejdřív nainstaluj závislosti frontendu:  cd ui; bun install' -ForegroundColor Red
    exit 1
}

# bun (beforeBuildCommand) a cargo nemusí být v PATH, třeba v nově
# otevřené nebo elevované konzoli — instalují se do profilu uživatele.
foreach ($bin in @('.bun\bin', '.cargo\bin')) {
    $dir = Join-Path $env:USERPROFILE $bin
    if ((Test-Path $dir) -and ($env:Path -notlike "*$dir*")) {
        $env:Path = "$dir;$env:Path"
    }
}

# Proměnné se po doběhu vracejí: skript se volá i z jiných skriptů
# (publish.ps1) ve stejné relaci a ty by je jinak zdědily.
$oldApp = $env:TAURI_APP_PATH
$oldFront = $env:TAURI_FRONTEND_PATH
$env:TAURI_APP_PATH = Join-Path $root 'src-tauri'
$env:TAURI_FRONTEND_PATH = Join-Path $root 'ui'

# Výchozí kód je NEÚSPĚCH. Když se CLI vůbec nespustí (poškozený nebo
# antivirem zablokovaný tauri.exe, rozbitá instalace z přerušeného
# `bun install`), PowerShell s 'Continue' jen vypíše chybu a pokračuje
# dál — $LASTEXITCODE se nenastaví a `exit $null` je `exit 0`.
# publish.ps1 by pak vydal starou KeyPad.exe z minulého buildu pod
# novou verzí. Naměřeno: s tauri.exe = bajty „MZ-broken" skript vracel 0.
$code = 1
Push-Location $root
try {
    & $cli @args
    $code = $LASTEXITCODE
} catch {
    # Selhání startu je pro PowerShell chyba příkazu, ne skriptu —
    # try/catch ji chytí i při 'Continue'. Kód zůstává 1.
    Write-Host "Tauri CLI se nepodařilo spustit: $cli" -ForegroundColor Red
    Write-Host $_ -ForegroundColor Red
    $code = 1
} finally {
    Pop-Location
    $env:TAURI_APP_PATH = $oldApp
    $env:TAURI_FRONTEND_PATH = $oldFront
}
# Pojistka pro cestu, kde CLI neběželo a výjimka nevznikla: bez kódu
# od CLI nejde tvrdit, že se něco postavilo.
if ($null -eq $code) { $code = 1 }
exit $code

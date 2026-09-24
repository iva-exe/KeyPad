# Vydání nové verze — jeden příkaz.
#
#   powershell -ExecutionPolicy Bypass -File tools\publish.ps1
#
# Projde brány, postaví release binárky, položí je do release/ (odkud
# si je stahuje KeyPadSetup.exe i aktualizace v aplikaci) a pushne do
# repozitáře iva-exe/KeyPad. Kamarádovi pak aplikace sama ukáže „Je
# dostupná nová verze", případně stačí spustit instalátor znovu.
#
# Staví se jen commitnutý zdrojový kód, který už je na GitHubu — jinak
# skript skončí ještě před buildem (viz „Zdroj = to, co je na GitHubu").
#
# POZOR na kódování: soubor musí zůstat v UTF-8 s BOM (PowerShell 5.1).

#Requires -Version 5.1
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root

# Výstup přesměrovaný do roury by se kódoval kódovou stránkou konzole
# a čeština by se rozsypala. Do okna konzole píše PowerShell Unicode
# napřímo, tam se nic měnit nemusí.
if ([Console]::IsOutputRedirected) { [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false) }

# ── Pomocné funkce ─────────────────────────────────────────────────

# Verze z [workspace.package] v kořenovém Cargo.toml. Čte se jen tahle
# sekce: `version = …` stojí v Cargo.toml i u závislostí a první výskyt
# v souboru nemusí být ten pravý.
function Get-WorkspaceVersion([string]$CargoToml) {
    $inPkg = $false
    foreach ($line in (Get-Content -LiteralPath $CargoToml -Encoding UTF8)) {
        if ($line -match '^\s*\[workspace\.package\]') { $inPkg = $true; continue }
        if ($inPkg -and $line -match '^\s*\[') { break }
        if ($inPkg -and $line -match '^\s*version\s*=\s*"([^"]+)"') { return $Matches[1] }
    }
    $null
}

# Dílčí skripty běží v samostatném procesu PowerShellu (stejné edice
# jako tenhle). Volání `& x.ps1` by běželo tady: jejich Set-Location,
# $ErrorActionPreference i `exit` by se promítly sem.
#
# Volají se přímo, ne přes pomocnou funkci: výstup programu spuštěného
# ve funkci se stane jejím návratovým výstupem, takže `$code = f` by ho
# spolklo (nebyl by vidět) a místo kódu vrátilo pole řádků.
$psExe = if ($PSVersionTable.PSEdition -eq 'Core') { 'pwsh.exe' } else { 'powershell.exe' }
$psExe = Join-Path $PSHOME $psExe

# Je v binárce vestavěný frontend? Hledají se klíče assetů z Vite
# („/assets/index-…js/css"): Tauri je ukládá jako obyčejné řetězce,
# kdežto obsah souborů je komprimovaný a hledat se v něm nedá.
# Opačný test („není tam adresa vývojového serveru") nefunguje:
# localhost:1420 je součástí vestavěné konfigurace a leží i ve správně
# postavené binárce. Ověřeno: Tauri build → klíče ano, holý
# `cargo build -p keypad` → ne.
#
# Latin-1 (kódová stránka 28591) převede každý bajt na jeden znak 1:1,
# takže hledání v řetězci = hledání v bajtech. Změřeno na 9MB
# KeyPad.exe: IndexOf ~40 ms, smyčka přes bajty v PowerShellu (jak to
# dělal WinSent) ~16 s.
# ([Text.Encoding]::Latin1 v PowerShellu 5.1 / .NET Framework není.)
function Test-EmbeddedFrontend([string]$Exe, [string]$Needle = 'assets/index-') {
    $bytes = [IO.File]::ReadAllBytes($Exe)
    $text = [Text.Encoding]::GetEncoding(28591).GetString($bytes)
    $text.IndexOf($Needle, [StringComparison]::Ordinal) -ge 0
}

function Write-Step([string]$Text) {
    Write-Host ""
    Write-Host $Text -ForegroundColor White
}

function Write-Done([Diagnostics.Stopwatch]$Sw) {
    Write-Host ("     hotovo za {0:N0} s" -f $Sw.Elapsed.TotalSeconds) -ForegroundColor DarkGray
}

# Řádky výstupu gitu. Git píše UTF-8, PowerShell ho ale čte kódovou
# stránkou konzole — česká zpráva commitu nebo název souboru by se
# rozsypaly. Na dobu čtení se proto přepne na UTF-8 a pak vrátí:
# natrvalo přepnutá stránka by zůstala konzoli i po skončení skriptu.
#
# Argumenty jako jedno pole, ne přes $args: `--` (konec přepínačů, za
# ním cesty) PowerShell při volání FUNKCE spolkne. Ověřeno v 5.1 i 7:
# `f log -- .` dostane jen „log .". Do nativního programu `--` projde.
function Get-GitLines([string[]]$GitArgs) {
    $enc = [Console]::OutputEncoding
    try {
        [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
        $out = @(& git @GitArgs)
        $code = $LASTEXITCODE
    } finally {
        [Console]::OutputEncoding = $enc
    }
    if ($code -ne 0) { throw "git $($GitArgs -join ' ') selhal ($code)" }
    $out
}

# Změny mimo release\, které nejsou v commitu — i nesledované soubory
# (nový modul, na který se zapomnělo `git add`, se do binárky dostane
# taky). release\ se nepočítá: tam zapisuje tenhle skript sám.
function Get-UncommittedSource {
    Get-GitLines @('-c', 'core.quotePath=false', 'status', '--porcelain', '--', '.', ':!release')
}

function Write-GitLines([string[]]$Lines) {
    $Lines | Select-Object -First 15 | ForEach-Object { Write-Host "  $_" }
    if ($Lines.Count -gt 15) { Write-Host ("  … a další ({0})" -f ($Lines.Count - 15)) }
}

# Stará binárka se před buildem maže. Kdyby se build tiše nerozběhl
# (tools\tauri.ps1 dřív vracel 0, i když se Tauri CLI vůbec nespustilo),
# zůstala by v target\release binárka z minula, prošla by všemi
# kontrolami níž a vydala by se pod novou verzí.
#
# Proč ne „čas zápisu novější než start kroku": cargo binárku, na které
# se nic nezměnilo, nepřepisuje. Naměřeno: build beze změn nechá starý
# čas a smazanou binárku jen obnoví hardlinkem z target\release\deps —
# zase se starým časem. Taková kontrola by zastavila každé vydání, ve
# kterém se instalátor nezměnil. Smazání pozná i to: build, který
# neproběhl, binárku neobnoví.
function Remove-BuildOutput([string]$File) {
    if (-not (Test-Path -LiteralPath $File)) { return }
    try {
        Remove-Item -LiteralPath $File -Force
    } catch {
        throw "starou binárku $File nejde před buildem smazat ($($_.Exception.Message)) — nemá ji něco otevřenou?"
    }
}

# ── Verze ──────────────────────────────────────────────────────────
# Verze = číslo z Cargo.toml + datum a čas buildu. Instalátor
# i aplikace podle ní poznají, že je co stahovat — samotné 0.1.0 by se
# během vývoje neměnilo a aktualizace by se přeskakovala.
$base = Get-WorkspaceVersion (Join-Path $root 'Cargo.toml')
if (-not $base) { throw "verzi se nepodařilo přečíst z Cargo.toml ([workspace.package] → version)" }
$version = "$base+$(Get-Date -Format 'yyyyMMdd.HHmm')"

Write-Host "KeyPad — vydávám $version" -ForegroundColor Cyan
$total = [Diagnostics.Stopwatch]::StartNew()

# ── Předletová kontrola ────────────────────────────────────────────
# Git se ověřuje PŘED stavěním: build trvá minuty a zjistit až na konci,
# že není kam pushnout, je zbytečně ztracený čas.
#
# `git config --get` a `symbolic-ref -q` schválně: při neúspěchu jen
# vrátí kód a nic nepíšou na stderr. Windows PowerShell 5.1 by jinak
# se $ErrorActionPreference = 'Stop' bral výstup na stderr jako chybu.
$origin = & git config --get remote.origin.url
if ($LASTEXITCODE -ne 0 -or -not $origin) {
    Write-Host ""
    Write-Host "Repozitář nemá vzdálený 'origin' — není kam vydávat." -ForegroundColor Red
    Write-Host "Instalátor i aplikace čtou vydání z GitHubu (iva-exe/KeyPad, složka release/)." -ForegroundColor Red
    Write-Host ""
    Write-Host "Jednorázově:"
    Write-Host "  1. Na GitHubu založ repozitář iva-exe/KeyPad jako VEŘEJNÝ (Public)."
    Write-Host "     Soukromý nejde — kamarád stahuje bez účtu a bez přihlášení."
    Write-Host "  2. git remote add origin https://github.com/iva-exe/KeyPad.git"
    Write-Host "  3. Spusť tools\publish.ps1 znovu."
    exit 1
}

$branch = & git symbolic-ref -q --short HEAD
if ($branch -ne 'main') {
    Write-Host ""
    Write-Host "Vydává se z větve 'main', teď je aktivní '$branch'." -ForegroundColor Red
    Write-Host "Instalátor i aplikace čtou verzi z main (release/version.txt na větvi main)." -ForegroundColor Red
    if ($branch -eq 'master') {
        Write-Host "Přejmenování větve: git branch -m master main"
    } else {
        Write-Host "Přepni se: git switch main"
    }
    exit 1
}

# ── Zdroj = to, co je na GitHubu ───────────────────────────────────
# README slibuje „Zdrojový kód je veřejný" a vydání se má dát dohledat
# ke commitu, ze kterého vzniklo (nese ho zpráva commitu vydání).
# Skript ale staví z pracovního stromu a commituje JEN release\, takže:
#  - necommitnutá změna by skončila v binárce u kamaráda, ale nikde
#    v repozitáři — a CI by ji nikdy neprověřilo;
#  - commit, který na GitHubu ještě není, by se pushnul až spolu
#    s vydáním a CI by ho prověřilo, až když si ho kamarádi stáhli.
$dirty = @(Get-UncommittedSource)
if ($dirty.Count -gt 0) {
    Write-Host ""
    Write-Host "Zdrojový kód má necommitnuté změny — binárka by obsahovala kód, který v repozitáři není:" -ForegroundColor Red
    Write-GitLines $dirty
    Write-Host ""
    Write-Host "Commitni je (nebo zahoď, případně přidej do .gitignore), pushni, nech doběhnout CI"
    Write-Host "a spusť tools\publish.ps1 znovu."
    exit 1
}

# Porovnává se s čerstvým stavem GitHubu, ne s origin/main z posledního
# fetche — ten může být libovolně starý.
& git fetch --quiet origin
if ($LASTEXITCODE -ne 0) {
    Write-Host ""
    Write-Host "Stav repozitáře na GitHubu se nepodařilo načíst (git fetch origin, kód $LASTEXITCODE)." -ForegroundColor Red
    Write-Host "Bez spojení s GitHubem se vydat nedá — push na konci by stejně neprošel."
    exit 1
}
# `--verify -q`: při neúspěchu jen kód, nic na stderr (viz výš).
& git rev-parse --verify -q 'refs/remotes/origin/main' | Out-Null
if ($LASTEXITCODE -ne 0) {
    Write-Host ""
    Write-Host "Na GitHubu ještě není větev main — zdrojový kód tam není vůbec." -ForegroundColor Red
    Write-Host "Nejdřív ho pushni:  git push -u origin main"
    Write-Host "Pak nech doběhnout CI a spusť tools\publish.ps1 znovu."
    exit 1
}
$behind = [int](Get-GitLines @('rev-list', '--count', 'HEAD..origin/main') | Select-Object -First 1)
if ($behind -gt 0) {
    Write-Host ""
    Write-Host "Na GitHubu jsou commity, které tady nejsou ($behind) — push na konci by neprošel." -ForegroundColor Red
    Write-Host "Nejdřív je stáhni:  git pull"
    exit 1
}
# Commity vydání (mění jen release\) se nepočítají: ty zdroj nenesou
# a pushne je tenhle skript sám (třeba minulé vydání, kterému selhal
# push). Pathspec to rozliší podle obsahu, ne podle zprávy commitu.
$unpushed = @(Get-GitLines @('-c', 'core.quotePath=false', 'log', '--format=%h %s', 'origin/main..HEAD', '--', '.', ':!release'))
if ($unpushed.Count -gt 0) {
    Write-Host ""
    Write-Host "Zdrojový kód není na GitHubu — tyhle commity tam ještě nejsou:" -ForegroundColor Red
    Write-GitLines $unpushed
    Write-Host ""
    Write-Host "Nejdřív je pushni (git push origin main), nech doběhnout CI a spusť tools\publish.ps1 znovu."
    exit 1
}
# Commit, ze kterého se staví — jde do zprávy commitu vydání.
$sourceSha = (Get-GitLines @('rev-parse', '--short', 'HEAD') | Select-Object -First 1)

$appExe = Join-Path $root 'target\release\KeyPad.exe'
$setupExe = Join-Path $root 'target\release\KeyPadSetup.exe'

# Běžící binárku z target\release cargo nepřepíše („failed to remove
# file … Přístup byl odepřen") a build spadne až po minutách překladu.
# Nainstalovaný KeyPad (z %LOCALAPPDATA%) nevadí — je to jiný soubor.
$running = @(Get-Process -Name 'KeyPad', 'KeyPadSetup' -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $appExe -or $_.Path -eq $setupExe })
if ($running.Count -gt 0) {
    Write-Host ""
    foreach ($p in $running) {
        Write-Host ("Běží {0} (PID {1}) přímo z target\release — build by ho nemohl přepsat." -f $p.Path, $p.Id) -ForegroundColor Red
    }
    Write-Host "Zavři ho a spusť tools\publish.ps1 znovu."
    exit 1
}

# ── 1/7 Brány ──────────────────────────────────────────────────────
# Vydat rozbitý build je horší než chvíli počkat: kamarád ho dostane
# automatickou aktualizací a vrátit se neumí.
Write-Step "1/7  Brány (tools\check.ps1)"
$sw = [Diagnostics.Stopwatch]::StartNew()
& $psExe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'tools\check.ps1')
if ($LASTEXITCODE -ne 0) { throw "brány neprošly ($LASTEXITCODE) — vydání se zastavilo" }
Write-Done $sw

# ── 2/7 Aplikace ───────────────────────────────────────────────────
# Aplikace se MUSÍ stavět přes Tauri CLI, ne přes holé `cargo build`.
# `tauri::generate_context!` se podle prostředí rozhoduje, jestli do
# binárky vestaví soubory frontendu, nebo jen adresu vývojového
# serveru. Bez CLI vyhraje ta druhá možnost a nainstalovaná aplikace
# ukáže „localhost se odmítl připojit" — vypadá to jako rozbitá
# aplikace, přitom jde jen o špatně postavenou binárku.
# CLI si samo pustí `beforeBuildCommand` (vite build), takže se
# frontend staví v rámci tohohle kroku.
Write-Step "2/7  Aplikace (Tauri build)"
$sw = [Diagnostics.Stopwatch]::StartNew()
$tauri = Join-Path $root 'tools\tauri.ps1'
if (-not (Test-Path -LiteralPath $tauri)) { throw "chybí tools\tauri.ps1 (spouštěč Tauri CLI)" }
Remove-BuildOutput $appExe
& $psExe -NoProfile -ExecutionPolicy Bypass -File $tauri build --no-bundle
if ($LASTEXITCODE -ne 0) { throw "tauri build selhal ($LASTEXITCODE)" }
if (-not (Test-Path -LiteralPath $appExe)) { throw "tauri build nevyrobil $appExe (a přesto ohlásil úspěch)" }
Write-Done $sw

# ── 3/7 Instalátor ─────────────────────────────────────────────────
Write-Step "3/7  Instalátor (cargo build --release)"
$sw = [Diagnostics.Stopwatch]::StartNew()
Remove-BuildOutput $setupExe
& cargo build --release -p installer
if ($LASTEXITCODE -ne 0) { throw "cargo build instalátoru selhal ($LASTEXITCODE)" }
if (-not (Test-Path -LiteralPath $setupExe)) { throw "cargo build nevyrobil $setupExe (a přesto ohlásil úspěch)" }
Write-Done $sw

# ── 4/7 Frontend v binárce ─────────────────────────────────────────
# Špatně postavená binárka (viz krok 2) se jinak pozná až u kamaráda
# prázdným oknem s „localhost se odmítl připojit" — a vypadá to jako
# rozbitá aplikace, ne jako rozbitý build.
Write-Step "4/7  Frontend vestavěný v KeyPad.exe"
if (-not (Test-EmbeddedFrontend $appExe)) {
    throw "KeyPad.exe neobsahuje soubory frontendu — postav aplikaci přes tools\tauri.ps1 build, ne přes 'cargo build -p keypad'"
}
Write-Host "     ok" -ForegroundColor DarkGray

# ── 5/7 Importy ────────────────────────────────────────────────────
# Chybějící DLL (typicky VCRUNTIME140.dll) se na vývojářském PC
# neprojeví nikdy — tady ji má Visual Studio. Kamarádovi by se KeyPad
# vůbec nespustil.
#
# -RequireDependentLoadFlag: obě .exe musí mít /DEPENDENTLOADFLAG:0x800,
# tedy statické importy jen ze System32. Bez něj Windows hledá
# dwmapi.dll a winhttp.dll (nejsou mezi KnownDLLs) nejdřív ve složce
# s .exe — a KeyPadSetup.exe se spouští ze Stažených souborů, kam
# prohlížeč umí podstrčit cizí DLL. Podrobnosti v tools\check-imports.ps1.
Write-Step "5/7  Importované DLL"
& $psExe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'tools\check-imports.ps1') -RequireDependentLoadFlag $appExe $setupExe
if ($LASTEXITCODE -ne 0) { throw "kontrola importů neprošla ($LASTEXITCODE) — binárka by se na čistém PC nespustila nebo by si nechala podstrčit DLL" }

# Build nesmí změnit zdroj — typicky Cargo.lock, který brány přepíšou,
# když nesedí k Cargo.toml. Binárka by pak nebyla z commitu $sourceSha,
# i když zpráva vydání tvrdí opak.
$dirty = @(Get-UncommittedSource)
if ($dirty.Count -gt 0) {
    Write-Host ""
    Write-Host "Během buildu se změnil zdrojový kód — binárka neodpovídá commitu $($sourceSha):" -ForegroundColor Red
    Write-GitLines $dirty
    throw "zdroj se během buildu změnil — commitni změny, pushni a vydávej znovu"
}

# ── 6/7 release/ = to, co si stahuje instalátor ────────────────────
Write-Step "6/7  Skládám release\"
$rel = Join-Path $root 'release'
New-Item -ItemType Directory -Force -Path $rel | Out-Null
Copy-Item -LiteralPath $appExe -Destination (Join-Path $rel 'KeyPad.exe') -Force
# Instalátor leží vedle: odsud si ho stahuje aktualizace v aplikaci
# a odsud si ho stáhneš ty, když ho chceš někomu poslat.
Copy-Item -LiteralPath $setupExe -Destination (Join-Path $rel 'KeyPadSetup.exe') -Force
# Bez BOM a bez konce řádku — instalátor obsah jen trimuje a porovnává.
[IO.File]::WriteAllText((Join-Path $rel 'version.txt'), $version, (New-Object Text.UTF8Encoding($false)))
Write-Host "     KeyPad.exe, KeyPadSetup.exe, version.txt" -ForegroundColor DarkGray

# ── 7/7 Push ───────────────────────────────────────────────────────
Write-Step "7/7  Pushuji do repozitáře"
& git add release
if ($LASTEXITCODE -ne 0) { throw "git add selhal ($LASTEXITCODE)" }
# Commit JEN release/ — bez omezení cestou by se svezlo cokoliv, co má
# člověk zrovna nachystané, a zpráva „release: …" by pak popisovala
# úplně jinou práci. Naletěl jsem na to.
#
# „Není co commitovat" se zjišťuje předem, ne z chybového kódu commitu:
# ten vrací 1 i tehdy, když commit opravdu selže (třeba chybí
# user.email) — skript by pak pushnul starý stav a ohlásil „Vydáno".
& git diff --cached --quiet -- release
if ($LASTEXITCODE -eq 0) {
    Write-Host "     (release\ se nezměnil — není co commitovat)" -ForegroundColor Yellow
} else {
    # Commit zdroje ve zprávě: z výpisu historie je hned vidět, z čeho
    # binárky vznikly (předletová kontrola zaručila, že je na GitHubu).
    & git commit -m "release: $version ($sourceSha)" -- release
    if ($LASTEXITCODE -ne 0) { throw "git commit selhal ($LASTEXITCODE)" }
}
& git push origin main
if ($LASTEXITCODE -ne 0) { throw "git push selhal ($LASTEXITCODE)" }

Write-Host ""
Write-Host ("Vydáno: {0}  (celkem {1:N0} s)" -f $version, $total.Elapsed.TotalSeconds) -ForegroundColor Green
# Aplikace čte verzi přes raw.githubusercontent.com, který drží soubory
# v cache CDN až 5 minut — banner se proto neukáže hned.
# Jednoduché uvozovky schválně: české „ a “ bere PowerShell uvnitř
# "…" jako konec řetězce.
Write-Host 'Kamarádovi se v aplikaci do pár minut ukáže „Je dostupná nová verze“.'
Write-Host "Instalátor k rozeslání: $(Join-Path $rel 'KeyPadSetup.exe')"

# ── Co je NAINSTALOVANÉ ────────────────────────────────────────────
# Tenhle skript nic neinstaluje: staví, kopíruje do release\ a pushuje.
# Do %LOCALAPPDATA%\Programs\KeyPad zapisuje jen KeyPadSetup.exe. Mezi
# „vydáno" a „nainstalováno" tak bylo slepé místo a skript mlčky končil
# nad zastaralou instalací.
#
# Stálo to (ve WinSentu) dvě kola ladění naslepo: dvě opravy vzhledu se
# vydaly, nenainstalovaly a hodnotil se pořád tentýž starý build.
# Binárky mají navíc shodnou velikost, takže ani ruční porovnání nic
# neodhalilo — liší se až hash.
$instVer = Join-Path $env:LOCALAPPDATA 'Programs\KeyPad\version.txt'
if (Test-Path -LiteralPath $instVer) {
    $ted = (Get-Content -LiteralPath $instVer -Raw).Trim()
    if ($ted -ne $version) {
        Write-Host ""
        Write-Host "POZOR: nainstalovaná verze je pořád $ted" -ForegroundColor Yellow
        Write-Host "       Dokud nespustíš instalátor (nebo Aktualizovat v aplikaci), testuješ starou binárku." -ForegroundColor Yellow
    } else {
        Write-Host "Nainstalovaná verze souhlasí." -ForegroundColor DarkGray
    }
} else {
    Write-Host "KeyPad tu není nainstalovaný — není co porovnávat." -ForegroundColor DarkGray
}

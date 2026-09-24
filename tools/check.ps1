# Brány kvality — všechny za sebou, rychle, na první chybě konec.
#
#   powershell -ExecutionPolicy Bypass -File tools\check.ps1
#   powershell -ExecutionPolicy Bypass -File tools\check.ps1 clippy test
#
# Pořadí je od nejlevnější brány po nejdražší. Formátování trvá
# vteřinu a chytí nejčastější chybu (zapomenutý `cargo fmt`). Clippy
# s --all-targets projde i testy, ale bez generování kódu — chyba
# v testu se tak ukáže dřív, než se testy začnou překládat naostro.
# Končí se na první chybě: další brány by stavěly nad rozbitým kódem
# a jen by zasypaly výstup chybami, které z té první plynou.
#
# Stejné kroky dělá CI (.github/workflows/build.yml), jen s --locked.
# Tady --locked schválně chybí: při přidávání závislosti se Cargo.lock
# mění a brána nemá bránit práci, jen ji kontrolovat. Že je zámek
# v pořádku, pohlídá CI.
#
# POZOR na kódování: soubor musí zůstat v UTF-8 s BOM (PowerShell 5.1).

#Requires -Version 5.1
param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Only)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root

# Výstup přesměrovaný do roury (zachycení z publish.ps1, z jiného
# programu) by se jinak kódoval kódovou stránkou konzole a čeština by
# se rozsypala. Do okna konzole píše PowerShell Unicode napřímo.
if ([Console]::IsOutputRedirected) { [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false) }

$ui = Join-Path $root 'ui'

# -q u clippy a testů: bez něj zaplní obrazovku stovky řádků
# „Checking …/Compiling …" a varování se v nich ztratí. Diagnostiku
# a selhané testy -q neschová.
$all = @(
    [pscustomobject]@{ Name = 'fmt'; Dir = $root; Exe = 'cargo'
        Args = @('fmt', '--all', '--check')
        Hint = 'Oprava: cargo fmt --all' }
    [pscustomobject]@{ Name = 'clippy'; Dir = $root; Exe = 'cargo'
        Args = @('clippy', '--workspace', '--all-targets', '-q', '--', '-D', 'warnings')
        Hint = 'Každé varování clippy je tu chyba (-D warnings), stejně jako v CI.' }
    [pscustomobject]@{ Name = 'test'; Dir = $root; Exe = 'cargo'
        Args = @('test', '--workspace', '-q')
        Hint = 'Selhaný test je vypsaný výš. Jen jeden balíček: cargo test -p <balíček>' }
    [pscustomobject]@{ Name = 'ui'; Dir = $ui; Exe = 'bun'
        Args = @('run', 'check')
        Hint = 'svelte-check našel chybu v typech nebo v šabloně (výpis výš).' }
)

if ($Only) {
    $names = $all | ForEach-Object { $_.Name }
    $unknown = @($Only | Where-Object { $names -notcontains $_ })
    if ($unknown.Count -gt 0) {
        Write-Host "Neznámá brána: $($unknown -join ', '). Existují: $($names -join ', ')" -ForegroundColor Red
        exit 2
    }
    $gates = @($all | Where-Object { $Only -contains $_.Name })
} else {
    $gates = $all
}

# Chybějící nástroj se má ohlásit srozumitelně, ne výjimkou
# „The term 'bun' is not recognized…" uprostřed běhu.
foreach ($exe in @($gates | ForEach-Object { $_.Exe } | Select-Object -Unique)) {
    if (-not (Get-Command $exe -ErrorAction SilentlyContinue)) {
        $kde = if ($exe -eq 'bun') { 'https://bun.sh' } else { 'https://rustup.rs' }
        Write-Host "$exe není v PATH — nainstaluj ho ($kde) a otevři nový terminál." -ForegroundColor Red
        exit 2
    }
}

Write-Host "KeyPad — brány ($($gates.Count))" -ForegroundColor Cyan
$total = [Diagnostics.Stopwatch]::StartNew()
$done = @()

for ($i = 0; $i -lt $gates.Count; $i++) {
    $g = $gates[$i]
    Write-Host ""
    Write-Host ("── {0}  ({1} {2})" -f $g.Name, $g.Exe, ($g.Args -join ' ')) -ForegroundColor DarkGray
    $sw = [Diagnostics.Stopwatch]::StartNew()
    Push-Location $g.Dir
    try {
        # Čerstvý klon nemá node_modules a svelte-check by spadl na
        # „command not found" — to není chyba kódu, jen chybějící
        # instalace. Jednou ji udělat je levnější než to vysvětlovat.
        if ($g.Exe -eq 'bun' -and -not (Test-Path (Join-Path $g.Dir 'node_modules'))) {
            Write-Host "   ui\node_modules chybí — instaluji závislosti (jednorázově)" -ForegroundColor DarkGray
            & bun install
            if ($LASTEXITCODE -ne 0) { throw "bun install selhal ($LASTEXITCODE)" }
        }
        & $g.Exe $g.Args
        $code = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    $secs = $sw.Elapsed.TotalSeconds

    if ($code -ne 0) {
        Write-Host ("  {0,-7} {1,6:N1} s  SELHALO (kód {2})" -f $g.Name, $secs, $code) -ForegroundColor Red
        Write-Host ""
        Write-Host ('Brána „{0}“ neprošla. {1}' -f $g.Name, $g.Hint) -ForegroundColor Red
        $rest = @($gates | Select-Object -Skip ($i + 1) | ForEach-Object { $_.Name })
        if ($rest.Count -gt 0) {
            Write-Host "Nespuštěné brány: $($rest -join ', ')" -ForegroundColor DarkGray
        }
        Write-Host ("Celkem {0:N1} s · prošlo {1} z {2}" -f $total.Elapsed.TotalSeconds, $done.Count, $gates.Count) -ForegroundColor DarkGray
        exit 1
    }
    Write-Host ("  {0,-7} {1,6:N1} s  ok" -f $g.Name, $secs) -ForegroundColor Green
    $done += [pscustomobject]@{ Name = $g.Name; Seconds = $secs }
}

# Souhrn na konci: výstup nástrojů odsune jednotlivé řádky „ok" daleko
# nahoru, a právě časy ukazují, která brána zdržuje.
Write-Host ""
foreach ($d in $done) {
    Write-Host ("  {0,-7} {1,6:N1} s" -f $d.Name, $d.Seconds) -ForegroundColor DarkGray
}
Write-Host ("Celkem {0:N1} s · prošlo všech {1}" -f $total.Elapsed.TotalSeconds, $gates.Count) -ForegroundColor Cyan
exit 0

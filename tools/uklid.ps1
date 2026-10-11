# Úklid pracovních souborů vývoje — target\ a zbytky testu okna.
#
#   powershell -ExecutionPolicy Bypass -File tools\uklid.ps1          # jen nad limitem
#   powershell -ExecutionPolicy Bypass -File tools\uklid.ps1 -Vse     # všechno hned
#   powershell -ExecutionPolicy Bypass -File tools\uklid.ps1 -LimitGB 4
#
# Proč: cargo do target\ jen přidává. Starší verze testovacích binárek,
# inkrementální cache a sestavení s jinými volbami (příklady v release,
# Tauri CLI s jinými featurami) nikdo nemaže — 11. 10. 2026 tak měl
# target\ na PC vlastníka 18 GB, ačkoli čisté sestavení všeho (brány
# 2,7 GB + vydání 1,9 GB) má 4,6 GB (změřeno týž den). Spouští ho tools\check.ps1 hned na začátku,
# tedy i každé vydání (publish.ps1 začíná bránami).
#
# Nad limitem nejdřív inkrementální cache (cargo si ji znovu vytvoří,
# další překlad je jen o něco pomalejší), a když to nestačí, celý
# `cargo clean` — pak se jednou staví od nuly (pár minut). Mazat
# jednotlivé „staré" soubory v deps\ by šetřilo čas, ale cargo by pak
# mohlo pracovat s rozbitou kopií; celý úklid je jistý.
#
# POZOR na kódování: soubor musí zůstat v UTF-8 s BOM (PowerShell 5.1).

#Requires -Version 5.1
param(
    # Strop velikosti target\. Čisté sestavení bran a vydání (4,6 GB)
    # se vejde s rezervou na pár dalších překladů.
    [double]$LimitGB = 6,
    [switch]$Vse
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root
if ([Console]::IsOutputRedirected) { [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false) }

function Get-Velikost([string]$Cesta) {
    if (-not (Test-Path -LiteralPath $Cesta)) { return 0 }
    $s = (Get-ChildItem -LiteralPath $Cesta -Recurse -File -Force -ErrorAction SilentlyContinue |
        Measure-Object Length -Sum).Sum
    if ($s) { [double]$s } else { 0 }
}

function Format-GB([double]$B) { '{0:N1} GB' -f ($B / 1GB) }

$target = Join-Path $root 'target'
$pred = Get-Velikost $target

# Zbytky testu okna: po úspěchu se běh maže sám, po selhání zůstane
# kvůli logu a snímkům — ale ne napořád.
$okno = Join-Path $env:TEMP 'keypad-okno-test'
if (Test-Path -LiteralPath $okno) {
    $stare = Get-ChildItem -LiteralPath $okno -Directory -ErrorAction SilentlyContinue |
        Where-Object { $Vse -or $_.LastWriteTime -lt (Get-Date).AddDays(-1) }
    foreach ($d in $stare) { Remove-Item -LiteralPath $d.FullName -Recurse -Force -ErrorAction SilentlyContinue }
    if (-not (Get-ChildItem -LiteralPath $okno -Force -ErrorAction SilentlyContinue)) {
        Remove-Item -LiteralPath $okno -Force -ErrorAction SilentlyContinue
    }
}

if (-not $Vse -and $pred -le $LimitGB * 1GB) {
    Write-Host ("úklid: target {0} (limit {1:0.#} GB) — nic" -f (Format-GB $pred), $LimitGB) -ForegroundColor DarkGray
    exit 0
}

if (-not $Vse) {
    foreach ($inc in Get-ChildItem -LiteralPath $target -Directory -ErrorAction SilentlyContinue) {
        $cesta = Join-Path $inc.FullName 'incremental'
        # Zamčený soubor (běžící překlad) nesmí úklid shodit — zbytek
        # dořeší cargo clean níž, nebo příští úklid.
        if (Test-Path -LiteralPath $cesta) { Remove-Item -LiteralPath $cesta -Recurse -Force -ErrorAction SilentlyContinue }
    }
}
$ted = Get-Velikost $target
if ($Vse -or $ted -gt $LimitGB * 1GB) {
    # Běžící binárku z target\ (nainstalovaná aplikace odtud neběží, ale
    # ruční spuštění ano) cargo nesmaže a skončí chybou — úklid nesmí
    # shodit brány, jen to ohlásí.
    & cargo clean -q 2>&1 | Out-Host
    if ($LASTEXITCODE -ne 0) { Write-Warning "úklid: cargo clean selhal ($LASTEXITCODE) — běží něco z target\?" }
    $ted = Get-Velikost $target
}
Write-Host ("úklid: target {0} → {1} (limit {2:0.#} GB)" -f (Format-GB $pred), (Format-GB $ted), $LimitGB) -ForegroundColor Cyan
exit 0

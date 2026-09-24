# Kontrola, že binárka nepotřebuje DLL, která na čistém PC chybí,
# a že si systémové DLL nenechá podstrčit ze své složky.
#
#   powershell -ExecutionPolicy Bypass -File tools\check-imports.ps1 target\release\KeyPad.exe target\release\KeyPadSetup.exe
#   powershell -ExecutionPolicy Bypass -File tools\check-imports.ps1 -RequireDependentLoadFlag target\release\KeyPad.exe target\release\KeyPadSetup.exe
#
# Kód návratu: 0 = vše v pořádku, 1 = našla se problémová DLL (nebo
# s -RequireDependentLoadFlag chybí příznak níž), 2 = soubor nejde
# zkontrolovat (chybí, není to .exe, je poškozený).
#
# Proč to vůbec hlídat: chybějící DLL se na vývojářském PC neprojeví
# NIKDY — Visual Studio, Windows SDK i každá druhá hra si do System32
# nainstalují vlastní runtime. Chyba se ukáže až kamarádovi hláškou
# „Spuštění kódu nemůže pokračovat, protože systém nenalezl
# VCRUNTIME140.dll" a z dálky se špatně ladí. Proto brána v publish.ps1
# i v CI, ne jen jednorázové ruční ověření.
#
# Proč vlastní čtení PE a ne dumpbin: dumpbin je jen ve Visual Studiu,
# jeho cesta se mění s každou verzí MSVC a jeho výstup je text určený
# lidem. Seznam DLL je v PE na pevně daných místech a na jeho přečtení
# stačí pár desítek řádků. Parser je ověřený proti `dumpbin /IMPORTS`
# a `/DEPENDENTS` (binárky WinSentu, notepad.exe, mspaint.exe)
# a `/LOADCONFIG` (x64 i x86 s příznakem 0x800, 0xA00 i bez něj,
# binárky WinSentu, System32, SysWOW64, git.exe bez load config).
#
# Proč i tabulka zpožděného načítání (delay-load): takovou DLL Windows
# načte až při prvním volání funkce z ní. Program na čistém PC naběhne
# a spadne až uprostřed práce — ještě horší než pád hned při startu.
#
# Proč DependentLoadFlags (-RequireDependentLoadFlag, publish.ps1 i CI):
# DLL ze statických importů hledá Windows NEJDŘÍV VE SLOŽCE S .EXE.
# Výjimkou jsou jen KnownDLLs (kernel32, user32, shell32…), ty jdou
# vždy ze System32 — dwmapi.dll a winhttp.dll, které importují obě
# naše .exe, mezi nimi ale nejsou. KeyPadSetup.exe se spouští přímo ze Stažených souborů a tam umí prohlížeč uložit
# soubor ze stránky bez ptaní (drive-by download). Podvržená dwmapi.dll
# vedle instalátoru pak běží v našem procesu dřív než main(). Ověřeno
# při review: podstrčená dwmapi.dll ukončila instalátor kódem 77.
# KeyPad.exe jde stáhnout taky (release\KeyPad.exe na GitHubu).
# Linker s /DEPENDENTLOADFLAG:0x800 (LOAD_LIBRARY_SEARCH_SYSTEM32) zapíše
# do PE pokyn „statické importy jen ze System32" (Windows 10 1607+).
# Za běhu volané LoadLibrary to neovlivní — ty musí cestu řešit samy.
#
# POZOR na kódování: soubor musí zůstat v UTF-8 s BOM (PowerShell 5.1).

#Requires -Version 5.1
param(
    # Každá binárka musí mít DependentLoadFlags s 0x800 a bez příznaků,
    # které by do hledání vrátily složku s .exe. Bez přepínače se
    # hodnota jen vypíše.
    [switch]$RequireDependentLoadFlag,
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$Path
)

$ErrorActionPreference = 'Stop'

# Výstup přesměrovaný do roury (log CI, zachycení jiným programem) se
# kóduje kódovou stránkou konzole a z češtiny zbydou otazníky. Do okna
# konzole píše PowerShell Unicode napřímo — tam se nic měnit nemusí.
if ([Console]::IsOutputRedirected) { [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false) }

# ── Čtení PE ───────────────────────────────────────────────────────
# Všechno se převádí na [long]. PowerShell počítá s [uint32] po svém:
# výsledek, který se do uint32 nevejde (záporný rozdíl, velký součet),
# tiše přejde na [double]. Jednotný [long] drží adresovou aritmetiku
# celočíselnou a -band nad ní předvídatelný.

function Assert-PeRange([byte[]]$Bytes, [long]$Offset, [long]$Length) {
    if ($Offset -lt 0 -or $Offset + $Length -gt $Bytes.Length) {
        throw ("poškozený soubor: čtení mimo rozsah (offset 0x{0:X})" -f $Offset)
    }
}

function Read-PeU16([byte[]]$Bytes, [long]$Offset) {
    Assert-PeRange $Bytes $Offset 2
    [long][BitConverter]::ToUInt16($Bytes, [int]$Offset)
}

function Read-PeU32([byte[]]$Bytes, [long]$Offset) {
    Assert-PeRange $Bytes $Offset 4
    [long][BitConverter]::ToUInt32($Bytes, [int]$Offset)
}

function Read-PeU64([byte[]]$Bytes, [long]$Offset) {
    Assert-PeRange $Bytes $Offset 8
    # ImageBase 64bitových binárek se do [long] vejde (kanonické adresy
    # uživatelského prostoru mají horní bit nulový).
    [long][BitConverter]::ToUInt64($Bytes, [int]$Offset)
}

# Název DLL je ASCII řetězec ukončený nulou. Limit délky chrání před
# tím, aby se poškozený soubor četl až do konce.
function Read-PeAsciiZ([byte[]]$Bytes, [long]$Offset) {
    Assert-PeRange $Bytes $Offset 1
    $end = $Offset
    $max = [Math]::Min($Bytes.Length, $Offset + 512)
    while ($end -lt $max -and $Bytes[$end] -ne 0) { $end++ }
    if ($end -ge $max) { throw ("poškozený soubor: neukončený název DLL (offset 0x{0:X})" -f $Offset) }
    [Text.Encoding]::ASCII.GetString($Bytes, [int]$Offset, [int]($end - $Offset))
}

# RVA (adresa v paměti po zavedení) → pozice v souboru. Sekce leží
# v paměti jinde než v souboru, takže se adresa musí přepočítat přes
# tabulku sekcí. VirtualSize bývá u některých linkerů nula — proto
# maximum s velikostí dat v souboru.
function ConvertTo-PeFileOffset([long]$Rva, $Sections, [long]$SizeOfHeaders) {
    foreach ($s in $Sections) {
        $size = [Math]::Max($s.VirtualSize, $s.RawSize)
        if ($Rva -ge $s.VirtualAddress -and $Rva -lt $s.VirtualAddress + $size) {
            return $Rva - $s.VirtualAddress + $s.RawPointer
        }
    }
    # Hlavičky se mapují 1:1 a některé linkery do nich data vkládají.
    if ($Rva -lt $SizeOfHeaders) { return $Rva }
    throw ("poškozený soubor: adresa 0x{0:X} neleží v žádné sekci" -f $Rva)
}

# Vrátí DLL z importní tabulky (Static) a z tabulky zpožděného
# načítání (Delay) v pořadí, v jakém v souboru jsou — tak, jak je
# vypisuje dumpbin, aby se daly porovnat — a DependentLoadFlags
# z load config.
function Get-PeInfo([string]$File) {
    $b = [IO.File]::ReadAllBytes($File)
    if ($b.Length -lt 0x40 -or $b[0] -ne 0x4D -or $b[1] -ne 0x5A) {
        throw "není to spustitelný soubor (chybí hlavička MZ)"
    }
    $pe = Read-PeU32 $b 0x3C
    if ((Read-PeU32 $b $pe) -ne 0x00004550) {
        throw "není to soubor PE (chybí podpis PE)"
    }

    $machine = Read-PeU16 $b ($pe + 4)
    $sectionCount = Read-PeU16 $b ($pe + 6)
    $optSize = Read-PeU16 $b ($pe + 20)
    $opt = $pe + 24

    # PE32+ (64 bitů) a PE32 (32 bitů) se liší šířkou ImageBase a polí
    # pro zásobník a haldu — tím se posouvá i tabulka datových adresářů.
    $magic = Read-PeU16 $b $opt
    switch ($magic) {
        0x20B { $imageBase = Read-PeU64 $b ($opt + 24); $countOff = 108; $dirOff = 112 }
        0x10B { $imageBase = Read-PeU32 $b ($opt + 28); $countOff = 92; $dirOff = 96 }
        default { throw ("neznámý typ volitelné hlavičky 0x{0:X}" -f $magic) }
    }
    $sizeOfHeaders = Read-PeU32 $b ($opt + 60)
    $dirCount = Read-PeU32 $b ($opt + $countOff)

    $sections = @()
    $sec = $opt + $optSize
    for ($i = 0; $i -lt $sectionCount; $i++) {
        $o = $sec + 40 * $i
        $sections += [pscustomobject]@{
            VirtualSize    = Read-PeU32 $b ($o + 8)
            VirtualAddress = Read-PeU32 $b ($o + 12)
            RawSize        = Read-PeU32 $b ($o + 16)
            RawPointer     = Read-PeU32 $b ($o + 20)
        }
    }

    $dirRva = {
        param([int]$Index)
        if ($Index -ge $dirCount) { return 0 }
        Read-PeU32 $b ($opt + $dirOff + 8 * $Index)
    }

    # Datový adresář 1: IMAGE_IMPORT_DESCRIPTOR po 20 bajtech, pole Name
    # na offsetu 12. Tabulku ukončuje záznam s nulovým jménem.
    $static = New-Object System.Collections.Generic.List[string]
    $rva = & $dirRva 1
    if ($rva -ne 0) {
        $o = ConvertTo-PeFileOffset $rva $sections $sizeOfHeaders
        # Strop počtu záznamů: poškozený soubor bez ukončovacího záznamu
        # by jinak četl nesmysly až do konce.
        for ($n = 0; $n -lt 4096; $n++) {
            $nameRva = Read-PeU32 $b ($o + 20 * $n + 12)
            if ($nameRva -eq 0) { break }
            $static.Add((Read-PeAsciiZ $b (ConvertTo-PeFileOffset $nameRva $sections $sizeOfHeaders)))
        }
    }

    # Datový adresář 13: IMAGE_DELAYLOAD_DESCRIPTOR po 32 bajtech,
    # Attributes na 0 a DllNameRVA na 4. Bit 0 v Attributes = adresy
    # jsou RVA; bez něj jde o prastarý formát (Visual C++ 6), kde jsou
    # to absolutní adresy a musí se od nich odečíst ImageBase.
    $delay = New-Object System.Collections.Generic.List[string]
    $rva = & $dirRva 13
    if ($rva -ne 0) {
        $o = ConvertTo-PeFileOffset $rva $sections $sizeOfHeaders
        for ($n = 0; $n -lt 4096; $n++) {
            $attrs = Read-PeU32 $b ($o + 32 * $n)
            $nameRva = Read-PeU32 $b ($o + 32 * $n + 4)
            if ($nameRva -eq 0) { break }
            if (($attrs -band 1) -eq 0) { $nameRva -= $imageBase }
            $delay.Add((Read-PeAsciiZ $b (ConvertTo-PeFileOffset $nameRva $sections $sizeOfHeaders)))
        }
    }

    # Datový adresář 10: IMAGE_LOAD_CONFIG_DIRECTORY. DependentLoadFlags
    # je WORD na 0x4E (PE32+) nebo 0x36 (PE32) — ověřeno proti
    # `dumpbin /LOADCONFIG` („Dependent Load Flag"). První DWORD (Size)
    # říká, kolik struktury linker zapsal: starší linkery psaly kratší
    # verzi a pole za jejím koncem zavaděč bere jako nulu. Bez load
    # config (nebo s krátkou) je to taky 0 = výchozí pořadí hledání.
    $loadFlags = 0
    $rva = & $dirRva 10
    if ($rva -ne 0) {
        $o = ConvertTo-PeFileOffset $rva $sections $sizeOfHeaders
        $lcSize = Read-PeU32 $b $o
        $flagOff = if ($magic -eq 0x20B) { 0x4E } else { 0x36 }
        if ($lcSize -ge $flagOff + 2) { $loadFlags = Read-PeU16 $b ($o + $flagOff) }
    }

    [pscustomobject]@{
        Machine            = $machine
        Static             = [string[]]$static.ToArray()
        Delay              = [string[]]$delay.ToArray()
        DependentLoadFlags = $loadFlags
    }
}

# Příznaky LOAD_LIBRARY_SEARCH_*, které do hledání vracejí složku
# s .exe nebo jinou složku, kam smí zapisovat uživatel:
# DLL_LOAD_DIR 0x100, APPLICATION_DIR 0x200, USER_DIRS 0x400,
# DEFAULT_DIRS 0x1000 (= aplikace + System32 + USER_DIRS).
$LoadSearchSystem32 = 0x800
$LoadSearchUnsafe = 0x100 -bor 0x200 -bor 0x400 -bor 0x1000

# $null = příznak je v pořádku, jinak text, proč vadí.
function Get-LoadFlagProblem([long]$Flags) {
    if (($Flags -band $LoadSearchSystem32) -eq 0) {
        return 'statické importy se hledají nejdřív ve složce s .exe — podvržená dwmapi.dll nebo winhttp.dll vedle staženého souboru by se načetla místo systémové. Oprava: linkovat s /DEPENDENTLOADFLAG:0x800 (v build.rs: cargo:rustc-link-arg-bin=<binárka>=/DEPENDENTLOADFLAG:0x800).'
    }
    if (($Flags -band $LoadSearchUnsafe) -ne 0) {
        return 'kromě System32 (0x800) jsou nastavené i příznaky, které do hledání vracejí složku s .exe nebo jiné složky mimo System32 (0x100/0x200/0x400/0x1000). Má tam být jen 0x800.'
    }
    $null
}

function Format-LoadFlags([long]$Flags) {
    if ($Flags -eq 0) { return '0x0000 (výchozí pořadí: nejdřív složka s .exe)' }
    $names = @()
    foreach ($f in @(
            @{ Bit = 0x100; Name = 'DLL_LOAD_DIR' }, @{ Bit = 0x200; Name = 'APPLICATION_DIR' },
            @{ Bit = 0x400; Name = 'USER_DIRS' }, @{ Bit = 0x800; Name = 'SYSTEM32' },
            @{ Bit = 0x1000; Name = 'DEFAULT_DIRS' })) {
        if ($Flags -band $f.Bit) { $names += $f.Name }
    }
    $rest = $Flags -band -bnot 0x1F00
    if ($rest) { $names += ('0x{0:X}' -f $rest) }
    '0x{0:X4} ({1})' -f $Flags, ($names -join ' | ')
}

# ── Posouzení DLL ──────────────────────────────────────────────────

# Kde hledat systémové DLL. 32bitový PowerShell vidí pod „System32"
# ve skutečnosti SysWOW64 (přesměrování WOW64) — skutečnou System32
# mu ukáže jen alias Sysnative. 32bitová binárka zase naopak hledá
# své DLL v SysWOW64.
function Get-SystemDllDir([long]$Machine) {
    $root = $env:SystemRoot
    if ($Machine -eq 0x14C) {
        $wow = Join-Path $root 'SysWOW64'
        if (Test-Path -LiteralPath $wow) { return $wow }
    }
    $native = Join-Path $root 'Sysnative'
    if (Test-Path -LiteralPath $native) { return $native }
    Join-Path $root 'System32'
}

# $null = DLL je v pořádku, jinak text, proč vadí.
#
# Pořadí je podstatné: seznam zakázaných vyhrává nad „existuje
# v System32". vcruntime140.dll tam na vývojářském PC typicky JE —
# nainstaloval ji Visual C++ Redistributable s Visual Studiem nebo
# s nějakou hrou — a ucrtbased.dll tam dává Windows SDK. Na čistém
# PC ani jedna není. Samotná existence souboru by tak chybu schovala
# přesně tam, kde ji hledáme.
#
# Slepé místo, které zůstává: DLL, kterou do System32 nahrál cizí
# instalátor a která na seznamu není, projde. Proto seznam pokrývá
# všechny obvyklé redistribuovatelné balíky, ne jen CRT.
function Get-DllProblem([string]$Name, [string]$SystemDir) {
    $n = $Name.ToLowerInvariant()

    # Knihovny z Visual C++ Redistributable (2010 a novější). Tříciferná
    # verze u msvcp/msvcr/mfc je schválně: msvcrt.dll, msvcp60.dll
    # a mfc42u.dll jsou prastaré kopie, které jsou SOUČÁSTÍ Windows
    # (mspaint.exe třeba importuje MFC42u.dll) — vadit nesmí.
    $redist = '^(vcruntime\d|msvcp\d{3}|msvcr\d{3}|concrt\d|vccorlib\d|vcomp\d|vcamp\d|mfcm?\d{3})'

    # Ladicí varianty mají „d" před příponou nebo před podtržítkem
    # (vcruntime140d.dll, msvcp140d_atomic_wait.dll). Obecné „*d.dll"
    # by chytilo i systémové hid.dll, proto jen v rodině výše.
    if ($n -eq 'ucrtbased.dll' -or ($n -match $redist -and $n -match 'd(_[a-z_]+)?\.dll$')) {
        return 'ladicí runtime C/C++ — je jen na PC s Visual Studiem nebo Windows SDK a šířit se nesmí. Do binárky se dostal ladicí build nebo ladicí knihovna v závislostech.'
    }

    if ($n -match $redist) {
        return 'je z Visual C++ Redistributable, ne z Windows. Tady ji máš (přineslo ji Visual Studio nebo nějaká hra), na čistém PC chybí a program se nespustí. Oprava: statický CRT (+crt-static v .cargo\config.toml) — a hlídej, aby ho nepřebila proměnná RUSTFLAGS.'
    }

    if ($n -eq 'webview2loader.dll') {
        return 'zavaděč WebView2 jako samostatná DLL. Runtime WebView2 ji nepřináší, musela by ležet vedle .exe. Tauri ho linkuje staticky — nějaká závislost ho přitáhla dynamicky.'
    }

    # DirectX End-User Runtime (2010). Hry ho doinstalovávají, takže na
    # herním PC „je", ale Windows 10/11 ho samy nemají. Hlídá se kvůli
    # gamepadu: stará xinput1_3.dll je přesně ten druh DLL, který by
    # sem mohla zavléct herní knihovna.
    if ($n -match '^(d3dx\d+(_\d+)?|xinput1_[123]|xaudio2_[0-7]|x3daudio1_\d+|xapofx1_\d+)\.dll$') {
        return 'je ze starého DirectX End-User Runtime, ne z Windows 10/11. Na čistém PC chybí. Použij systémovou variantu (např. xinput1_4.dll).'
    }

    # Sady API (api-ms-win-*, ext-ms-win-*) nejsou soubory, ale jména,
    # která zavaděč Windows přesměruje na skutečnou systémovou DLL.
    # Univerzální CRT (api-ms-win-crt-*) je v nich od Windows 10.
    if ($n.StartsWith('api-ms-win-') -or $n.StartsWith('ext-ms-win-')) { return $null }

    if ([IO.File]::Exists((Join-Path $SystemDir $Name))) { return $null }

    'není v System32 a není to ani sada API Windows — na čistém PC ji nikdo nedodá. Zjisti, která závislost ji přitáhla, a nalinkuj ji staticky.'
}

# ── Hlavní část ────────────────────────────────────────────────────

if (-not $Path -or $Path.Count -eq 0) {
    Write-Host "Použití: tools\check-imports.ps1 [-RequireDependentLoadFlag] <soubor.exe> [další.exe …]" -ForegroundColor Yellow
    exit 2
}

$machineNames = @{ 0x8664 = 'x64'; 0x14C = 'x86'; 0xAA64 = 'ARM64' }
$problems = @()
$flagProblems = @()
$broken = @()

foreach ($p in $Path) {
    Write-Host ""
    if (-not (Test-Path -LiteralPath $p -PathType Leaf)) {
        Write-Host "$p — soubor neexistuje (postavil se vůbec?)" -ForegroundColor Red
        $broken += $p
        continue
    }
    $full = (Resolve-Path -LiteralPath $p).ProviderPath
    $leaf = Split-Path -Leaf $full
    try {
        $imp = Get-PeInfo $full
    } catch {
        Write-Host "$leaf — nejde přečíst: $($_.Exception.Message)" -ForegroundColor Red
        $broken += $p
        continue
    }

    $sysDir = Get-SystemDllDir $imp.Machine
    $arch = $machineNames[[int]$imp.Machine]
    if (-not $arch) { $arch = "stroj 0x{0:X}" -f $imp.Machine }

    # Tatáž DLL bývá v tabulce víckrát, jen jinak napsaná: Rust std
    # importuje „KERNEL32.dll", crate windows (raw-dylib) „kernel32.dll".
    # Pro Windows je to jeden soubor, takže se vypíše i posoudí jednou.
    $seen = @{}
    $rows = @()
    foreach ($d in $imp.Static) {
        if ($seen.ContainsKey($d.ToLowerInvariant())) { continue }
        $seen[$d.ToLowerInvariant()] = $true
        $rows += [pscustomobject]@{ Name = $d; Delay = $false }
    }
    foreach ($d in $imp.Delay) {
        if ($seen.ContainsKey($d.ToLowerInvariant())) { continue }
        $seen[$d.ToLowerInvariant()] = $true
        $rows += [pscustomobject]@{ Name = $d; Delay = $true }
    }
    $delayCount = @($rows | Where-Object { $_.Delay }).Count
    Write-Host ("{0}  ({1}, {2} DLL, z toho {3} zpožděně)" -f $leaf, $arch, $rows.Count, $delayCount) -ForegroundColor Cyan

    foreach ($r in $rows) {
        $why = Get-DllProblem $r.Name $sysDir
        $note = if ($r.Delay) { '  (zpožděně)' } else { '' }
        if ($why) {
            Write-Host ("  !!  {0}{1}" -f $r.Name, $note) -ForegroundColor Red
            $problems += [pscustomobject]@{ File = $leaf; Name = $r.Name; Why = $why }
        } else {
            Write-Host ("  ok  {0}{1}" -f $r.Name, $note) -ForegroundColor DarkGray
        }
    }

    $flags = $imp.DependentLoadFlags
    $flagWhy = Get-LoadFlagProblem $flags
    $flagText = "DependentLoadFlags {0}" -f (Format-LoadFlags $flags)
    if (-not $flagWhy) {
        Write-Host ("  ok  {0}" -f $flagText) -ForegroundColor DarkGray
    } elseif ($RequireDependentLoadFlag) {
        Write-Host ("  !!  {0}" -f $flagText) -ForegroundColor Red
        $flagProblems += [pscustomobject]@{ File = $leaf; Why = $flagWhy }
    } else {
        # Bez přepínače jen informace: nástroj se používá i na cizí
        # binárky (System32, WinSent), kde příznak chybět smí.
        Write-Host ("  --  {0}" -f $flagText) -ForegroundColor Yellow
    }
}

Write-Host ""
if ($problems.Count -gt 0 -or $flagProblems.Count -gt 0) {
    if ($problems.Count -gt 0) {
        Write-Host "NEPROŠLO: binárka potřebuje DLL, která na čistém PC chybí." -ForegroundColor Red
        foreach ($x in $problems) {
            Write-Host ""
            Write-Host ("  {0} → {1}" -f $x.File, $x.Name) -ForegroundColor Red
            Write-Host ("    {0}" -f $x.Why)
        }
        Write-Host ""
        Write-Host "Na tomhle PC se chyba neprojeví — Windows si DLL najde. Kamarádovi se KeyPad nespustí." -ForegroundColor Yellow
    }
    if ($flagProblems.Count -gt 0) {
        if ($problems.Count -gt 0) { Write-Host "" }
        Write-Host "NEPROŠLO: binárka si nechá podstrčit DLL ze své složky (DependentLoadFlags)." -ForegroundColor Red
        foreach ($x in $flagProblems) {
            Write-Host ""
            Write-Host ("  {0}" -f $x.File) -ForegroundColor Red
            Write-Host ("    {0}" -f $x.Why)
        }
    }
    exit 1
}
if ($broken.Count -gt 0) {
    Write-Host ("NEPROŠLO: {0} soubor(y) nejde zkontrolovat." -f $broken.Count) -ForegroundColor Red
    exit 2
}
if ($RequireDependentLoadFlag) {
    Write-Host "Importy v pořádku — jen systémové DLL, a ty jen ze System32." -ForegroundColor Green
} else {
    Write-Host "Importy v pořádku — jen systémové DLL." -ForegroundColor Green
}
exit 0

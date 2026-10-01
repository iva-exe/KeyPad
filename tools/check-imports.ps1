# Kontrola, že binárka nepotřebuje DLL, která na čistém PC chybí,
# že si systémové DLL nenechá podstrčit ze své složky a že naběhne
# na KAŽDÉM Windows 10 (od 1507) — žádný statický import funkce, kterou
# starší build nemá.
#
#   powershell -ExecutionPolicy Bypass -File tools\check-imports.ps1 target\release\KeyPad.exe target\release\KeyPadSetup.exe
#   powershell -ExecutionPolicy Bypass -File tools\check-imports.ps1 -RequireDependentLoadFlag target\release\KeyPad.exe target\release\KeyPadSetup.exe
#   powershell -ExecutionPolicy Bypass -File tools\check-imports.ps1 -ShowFunctions target\release\KeyPadSetup.exe
#
# Kód návratu: 0 = vše v pořádku, 1 = našel se problém (DLL, funkce,
# sada API, zpožděný import, nebo s -RequireDependentLoadFlag chybí
# příznak níž), 2 = soubor nejde zkontrolovat (chybí, není to .exe, je
# poškozený). -ShowFunctions navíc vypíše všechny importované funkce
# (na porovnání s `dumpbin /IMPORTS`).
#
# Proč to vůbec hlídat: chybějící DLL se na vývojářském PC neprojeví
# NIKDY — Visual Studio, Windows SDK i každá druhá hra si do System32
# nainstalují vlastní runtime. Chyba se ukáže až kamarádovi hláškou
# „Spuštění kódu nemůže pokračovat, protože systém nenalezl
# VCRUNTIME140.dll" a z dálky se špatně ladí. Proto brána v publish.ps1
# i v CI, ne jen jednorázové ruční ověření.
#
# Proč i jednotlivé FUNKCE (princip 9 — všechny buildy Windows 10 a 11):
# DLL na starém buildu je, jen jí chybí novější funkce. Staticky
# importovaná funkce, kterou build nemá, zastaví start programu hláškou
# „Vstupní bod procedury … nebyl nalezen" dřív, než program stihne
# cokoli říct (instalátor 0.1.0 tak neběžel na 1507/1511 kvůli
# GetDpiForWindow z 1607). Kontroluje se:
#  - kurátorský seznam funkcí novějších než Windows 10 1507 (DPI API
#    z 1607+, SetThreadDescription, GetTempPath2W, VirtualAlloc2…),
#  - sady API (api-ms-win-*): jen ty, o kterých víme, že je 10240 má.
#    windows-rs (raw-dylib) umí funkci navázat na novou sadu potichu
#    a tahle brána je jediné místo, kde se to ukáže,
#  - žádné zpožděné načítání (delay-load, důvody níž).
# Seznam funkcí není úplný (úplný by byl celý rozdíl SDK) — pokrývá, co
# reálně hrozí z Rust std, windows-rs, Tauri a Win32 kódu KeyPadu. Nová
# funkce se ověřuje v dokumentaci („Minimum supported client"); když je
# novější než Windows 10 1507, patří na seznam a v kódu za GetProcAddress.
#
# Proč vlastní čtení PE a ne dumpbin: dumpbin je jen ve Visual Studiu,
# jeho cesta se mění s každou verzí MSVC a jeho výstup je text určený
# lidem. Tabulky importů jsou v PE na pevně daných místech. Parser je
# ověřený proti `dumpbin /IMPORTS` — seznamy DLL i funkcí (i zpožděných,
# vázaných a podle ordinálu) sedí u binárek KeyPadu a WinSentu,
# notepad.exe a mspaint.exe (x64 i x86), explorer.exe a mmc.exe —
# a proti `/LOADCONFIG` (x64 i x86 s příznakem 0x800, 0xA00 i bez něj,
# binárky WinSentu, System32, SysWOW64, git.exe bez load config).
#
# Proč je zakázané zpožděné načítání (delay-load): takovou DLL Windows
# načtou až při prvním volání funkce z ní — zavaděč ji při startu
# nezkontroluje, takže chybějící DLL nebo funkce shodí program až
# uprostřed práce (hůř než pád hned při startu) a tahle brána by ji
# neviděla jako statický import. A načítá ji pomocná rutina linkeru, na
# kterou se `/DEPENDENTLOADFLAG` spolehlivě nevztahuje. Na volitelné API
# je správná cesta LoadLibraryExW(…, LOAD_LIBRARY_SEARCH_SYSTEM32)
# + GetProcAddress s náhradou.
#
# Proč DependentLoadFlags (-RequireDependentLoadFlag, publish.ps1 i CI):
# DLL ze statických importů hledá Windows NEJDŘÍV VE SLOŽCE S .EXE.
# Výjimkou jsou jen KnownDLLs (kernel32, user32, shell32…), ty jdou
# vždy ze System32 — dwmapi.dll, winhttp.dll, wintrust.dll, crypt32.dll,
# cfgmgr32.dll a bcrypt.dll, které naše .exe importují, mezi nimi ale
# nejsou. KeyPadSetup.exe se spouští přímo ze Stažených souborů a tam
# umí prohlížeč uložit soubor ze stránky bez ptaní (drive-by download).
# Podvržená dwmapi.dll vedle instalátoru pak běží v našem procesu dřív
# než main(). Ověřeno při review: podstrčená dwmapi.dll ukončila
# instalátor kódem 77. KeyPad.exe jde stáhnout taky (release\KeyPad.exe
# na GitHubu). Linker s /DEPENDENTLOADFLAG:0x800
# (LOAD_LIBRARY_SEARCH_SYSTEM32) zapíše do PE pokyn „statické importy
# jen ze System32" (Windows 10 1607+; 1507/1511 příznak neznají).
#
# Příznak ale chrání JEN statické importy samotného .exe. DLL načítané
# za běhu — i ty, které si za běhu tahají samy systémové DLL — se hledají
# postaru, nejdřív ve složce s .exe. Ověřeno při review podstrčenými
# DLL: WinVerifyTrust načte CRYPTSP.dll a CRYPTBASE.dll,
# SHGetKnownFolderPath profapi.dll a WinHttp přes HTTPS IPHLPAPI.DLL
# ze Stažených, a to v instalátoru těsně před výzvou UAC pro ovladač.
# To řeší volání SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)
# jako úplně první příkaz main() — v KeyPadSetupu i v KeyPadu (s ním se
# ani jedna z nich nenačetla). Volání v PE vidět není, jen import
# funkce — skript proto u každé binárky vypíše, jestli
# SetDefaultDllDirectories importuje. Je to jen informace, bránou to
# není: import neříká, jestli se funkce opravdu volá a jestli hned na
# začátku. To hlídá kód (komentář u main) a test podstrčených DLL.
#
# POZOR na kódování: soubor musí zůstat v UTF-8 s BOM (PowerShell 5.1).

#Requires -Version 5.1
param(
    # Každá binárka musí mít DependentLoadFlags s 0x800 a bez příznaků,
    # které by do hledání vrátily složku s .exe. Bez přepínače se
    # hodnota jen vypíše.
    [switch]$RequireDependentLoadFlag,
    # Vypsat všechny importované funkce (kontrola parseru proti dumpbin).
    [switch]$ShowFunctions,
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
    # uživatelského prostoru mají horní bit nulový). Položky tabulek
    # importů se tudy NEčtou — ty mají horní bit jako příznak ordinálu.
    [long][BitConverter]::ToUInt64($Bytes, [int]$Offset)
}

# Název (DLL, funkce) je ASCII řetězec ukončený nulou. Limit délky
# chrání před tím, aby se poškozený soubor četl až do konce.
function Read-PeAsciiZ([byte[]]$Bytes, [long]$Offset) {
    Assert-PeRange $Bytes $Offset 1
    $end = $Offset
    $max = [Math]::Min($Bytes.Length, $Offset + 512)
    while ($end -lt $max -and $Bytes[$end] -ne 0) { $end++ }
    if ($end -ge $max) { throw ("poškozený soubor: neukončený název (offset 0x{0:X})" -f $Offset) }
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

# Funkce z tabulky jmen importů (Import Lookup/Name Table) jedné DLL.
# Položka má 8 bajtů (PE32+) nebo 4 (PE32); nejvyšší bit = import podle
# ordinálu (jen číslo, bez jména), jinak je ve spodních 31 bitech RVA
# na IMAGE_IMPORT_BY_NAME = hint (2 bajty) + jméno. Nula tabulku končí.
# Horní DWORD 64bitové položky se čte zvlášť: s nastaveným horním bitem
# by se hodnota do [long] nevešla.
function Read-PeThunks([byte[]]$Bytes, [long]$TableRva, [bool]$Pe64, $Sections, [long]$SizeOfHeaders) {
    $names = New-Object System.Collections.Generic.List[string]
    if ($TableRva -eq 0) { return , $names }
    $o = ConvertTo-PeFileOffset $TableRva $Sections $SizeOfHeaders
    $step = if ($Pe64) { 8 } else { 4 }
    for ($n = 0; $n -lt 65536; $n++) {
        $lo = Read-PeU32 $Bytes ($o + $step * $n)
        $hi = if ($Pe64) { Read-PeU32 $Bytes ($o + $step * $n + 4) } else { 0 }
        if ($lo -eq 0 -and $hi -eq 0) { return , $names }
        $ordinal = if ($Pe64) { ($hi -band 0x80000000) -ne 0 } else { ($lo -band 0x80000000) -ne 0 }
        if ($ordinal) {
            $names.Add(('#{0}' -f ($lo -band 0xFFFF)))
        } else {
            $hint = ConvertTo-PeFileOffset ($lo -band 0x7FFFFFFF) $Sections $SizeOfHeaders
            $names.Add((Read-PeAsciiZ $Bytes ($hint + 2)))
        }
    }
    throw "poškozený soubor: neukončená tabulka importů"
}

# Vrátí importy (Static) a zpožděné importy (Delay) v pořadí, v jakém
# jsou v souboru — tak, jak je vypisuje dumpbin, aby se daly porovnat —
# každý jako { Dll; Functions }, a DependentLoadFlags z load config.
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
    $pe64 = $magic -eq 0x20B
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

    # Datový adresář 1: IMAGE_IMPORT_DESCRIPTOR po 20 bajtech —
    # OriginalFirstThunk (tabulka jmen) na 0, Name na 12, FirstThunk
    # (IAT) na 16. Když tabulka jmen chybí (staré linkery), jsou jména
    # v IAT — na disku, před zavedením, obsahuje totéž. Tabulku ukončuje
    # záznam s nulovým jménem.
    $static = New-Object System.Collections.Generic.List[object]
    $rva = & $dirRva 1
    if ($rva -ne 0) {
        $o = ConvertTo-PeFileOffset $rva $sections $sizeOfHeaders
        # Strop počtu záznamů: poškozený soubor bez ukončovacího záznamu
        # by jinak četl nesmysly až do konce.
        for ($n = 0; $n -lt 4096; $n++) {
            $d = $o + 20 * $n
            $nameRva = Read-PeU32 $b ($d + 12)
            if ($nameRva -eq 0) { break }
            $ilt = Read-PeU32 $b $d
            if ($ilt -eq 0) { $ilt = Read-PeU32 $b ($d + 16) }
            $static.Add([pscustomobject]@{
                    Dll       = Read-PeAsciiZ $b (ConvertTo-PeFileOffset $nameRva $sections $sizeOfHeaders)
                    Functions = [string[]](Read-PeThunks $b $ilt $pe64 $sections $sizeOfHeaders).ToArray()
                })
        }
    }

    # Datový adresář 13: IMAGE_DELAYLOAD_DESCRIPTOR po 32 bajtech,
    # Attributes na 0, DllNameRVA na 4, ImportNameTableRVA na 16. Bit 0
    # v Attributes = adresy jsou RVA; bez něj jde o prastarý formát
    # (Visual C++ 6), kde jsou to absolutní adresy a musí se od nich
    # odečíst ImageBase.
    $delay = New-Object System.Collections.Generic.List[object]
    $rva = & $dirRva 13
    if ($rva -ne 0) {
        $o = ConvertTo-PeFileOffset $rva $sections $sizeOfHeaders
        for ($n = 0; $n -lt 4096; $n++) {
            $d = $o + 32 * $n
            $attrs = Read-PeU32 $b $d
            $nameRva = Read-PeU32 $b ($d + 4)
            if ($nameRva -eq 0) { break }
            $int = Read-PeU32 $b ($d + 16)
            if (($attrs -band 1) -eq 0) {
                $nameRva -= $imageBase
                if ($int -ne 0) { $int -= $imageBase }
            }
            $delay.Add([pscustomobject]@{
                    Dll       = Read-PeAsciiZ $b (ConvertTo-PeFileOffset $nameRva $sections $sizeOfHeaders)
                    Functions = [string[]](Read-PeThunks $b $int $pe64 $sections $sizeOfHeaders).ToArray()
                })
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
        Static             = $static.ToArray()
        Delay              = $delay.ToArray()
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

# Sady API, o kterých víme, že je má už Windows 10 1507 (build 10240).
# Sada je jen jméno, které zavaděč přesměruje na skutečnou DLL — když ji
# build nezná, program nenaběhne stejně jako u chybějící DLL. Novou sadu
# sem přidávej až po ověření, že ji 10240 má (dokumentace funkce: sada
# a „Minimum supported client"), s komentářem odkud to víš.
$ApiSetAllowList = @(
    # WaitOnAddress/WakeByAddress* (Windows 8) — Rust std, v obou binárkách.
    'api-ms-win-core-synch-l1-2-0.dll'
)
# Univerzální CRT (hybridní CRT z tauri-build): verze l1-1-0 jsou
# součástí Windows 10 od 10240 (UCRT je komponenta systému).
$CrtApiSet = '^api-ms-win-crt-[a-z0-9]+(-[a-z0-9]+)*-l1-1-0\.dll$'

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
    # Projde jen sada ze seznamu výš — novější sadu starší build nezná.
    if ($n.StartsWith('api-ms-win-') -or $n.StartsWith('ext-ms-win-')) {
        if ($n -match $CrtApiSet -or $ApiSetAllowList -contains $n) { return $null }
        return 'sada API, o které nevíme, že ji má Windows 10 1507 — na starším buildu by program nenaběhl. Obvykle ji potichu vybere windows-rs pro novou funkci. Ověř funkci v dokumentaci; je-li z 1507, přidej sadu do $ApiSetAllowList v tools\check-imports.ps1 (s odkazem), jinak funkci hledej za běhu přes GetProcAddress.'
    }

    if ([IO.File]::Exists((Join-Path $SystemDir $Name))) { return $null }

    'není v System32 a není to ani sada API Windows — na čistém PC ji nikdo nedodá. Zjisti, která závislost ji přitáhla, a nalinkuj ji staticky.'
}

# ── DLL jen za běhu ────────────────────────────────────────────────
# Binárka (jméno souboru malými písmeny) → DLL, které nesmí být mezi
# jejími importy, statickými ani zpožděnými. Dnes zvuk pozastavení
# (Fáze 4b), který hraje přes WASAPI: MMDevAPI.dll se načte až za běhu
# přes COM (absolutní cesta z registru) a AudioSes.dll si MMDevAPI
# dotáhne sama JMÉNEM — před kopií vedle KeyPad.exe ji chrání jen
# SetDefaultDllDirectories(SYSTEM32) na začátku main (bez něj sonda
# s podvrženými DLL načetla podvrženou AudioSes.dll), žádná z nich
# není mezi KnownDLLs. Statický import by zvukovou knihovnu natáhl
# při každém startu, i když zvuk nikdy nezazní (princip 10), a na
# Windows 10 1507/1511 (bez /DEPENDENTLOADFLAG) by vzal kopii
# podstrčenou vedle KeyPad.exe ještě před main. winmm.dll (a
# winmmbase.dll, její dvojče ze starších buildů) KeyPad nesmí používat
# vůbec: PlaySound hraje přes starou cestu zvukových ovladačů
# a wdmaud.drv si načte i ze složky programu, a to i po
# SetDefaultDllDirectories (ověřeno sondou s podvrženými DLL).
#
# Tyhle DLL ve Windows jsou — nález neznamená „na čistém PC se
# nespustí", ale podvrženou DLL nebo zbytečnou zátěž. Proto se hlásí
# zvlášť ($runtimeProblems), ne mezi chybějícími DLL.
$RuntimeOnlyDlls = @{
    'keypad.exe' = @('winmm.dll', 'winmmbase.dll', 'mmdevapi.dll', 'audioses.dll')
}

# $null = DLL smí být v importech binárky, jinak text, proč ne.
function Get-RuntimeOnlyProblem([string]$Binary, [string]$Name) {
    $list = $RuntimeOnlyDlls[$Binary.ToLowerInvariant()]
    if ($list -and $list -contains $Name.ToLowerInvariant()) {
        return 'tuhle DLL binárka nesmí importovat (viz platform\windows\zvuk.rs) — statický import ji natáhne při každém startu a na Windows 10 1507/1511 i z podvržené kopie vedle .exe; winmm.dll navíc načítá zvukové ovladače i ze složky programu. Najdi, kdo ji importuje (windows-rs funkce z ní, nová závislost). Zvuk patří jen do WASAPI přes COM (a jen po SetDefaultDllDirectories v main), jiné funkce za GetProcAddress po LoadLibraryExW ze System32.'
    }
    $null
}

# ── Funkce novější než Windows 10 1507 ─────────────────────────────
# Jméno exportu → od kdy ho Windows mají (MS Learn, „Minimum supported
# client"). Posuzuje se jen jméno, ne DLL: tatáž funkce jde importovat
# i přes sadu API (která by ale neprošla už výš).
$PostRtmFunctions = @{
    # user32 — per-monitor DPI v2 a spol.
    'GetDpiForWindow'                     = 'Windows 10 1607'
    'GetDpiForSystem'                     = 'Windows 10 1607'
    'GetSystemMetricsForDpi'              = 'Windows 10 1607'
    'SystemParametersInfoForDpi'          = 'Windows 10 1607'
    'AdjustWindowRectExForDpi'            = 'Windows 10 1607'
    'EnableNonClientDpiScaling'           = 'Windows 10 1607'
    'SetThreadDpiAwarenessContext'        = 'Windows 10 1607'
    'GetThreadDpiAwarenessContext'        = 'Windows 10 1607'
    'GetWindowDpiAwarenessContext'        = 'Windows 10 1607'
    'GetAwarenessFromDpiAwarenessContext' = 'Windows 10 1607'
    'AreDpiAwarenessContextsEqual'        = 'Windows 10 1607'
    'IsValidDpiAwarenessContext'          = 'Windows 10 1607'
    'SetProcessDpiAwarenessContext'       = 'Windows 10 1703'
    'SetDialogDpiChangeBehavior'          = 'Windows 10 1703'
    'GetDialogDpiChangeBehavior'          = 'Windows 10 1703'
    'SetDialogControlDpiChangeBehavior'   = 'Windows 10 1703'
    'GetDialogControlDpiChangeBehavior'   = 'Windows 10 1703'
    'GetSystemDpiForProcess'              = 'Windows 10 1803'
    'GetDpiFromDpiAwarenessContext'       = 'Windows 10 1803'
    'GetDpiAwarenessContextForProcess'    = 'Windows 10 1803'
    'SetThreadDpiHostingBehavior'         = 'Windows 10 1803'
    'GetThreadDpiHostingBehavior'         = 'Windows 10 1803'
    'GetWindowDpiHostingBehavior'         = 'Windows 10 1803'
    # shcore
    'GetDpiForShellUIComponent'           = 'Windows 10 1607'
    # kernel32 / kernelbase
    'SetThreadDescription'                = 'Windows 10 1607'
    'GetThreadDescription'                = 'Windows 10 1607'
    'IsWow64Process2'                     = 'Windows 10 1709'
    'GetUserDefaultGeoName'               = 'Windows 10 1709'
    'SetUserGeoName'                      = 'Windows 10 1709'
    'VirtualAlloc2'                       = 'Windows 10 1803'
    'VirtualAlloc2FromApp'                = 'Windows 10 1803'
    'MapViewOfFile3'                      = 'Windows 10 1803'
    'MapViewOfFile3FromApp'               = 'Windows 10 1803'
    'CreatePseudoConsole'                 = 'Windows 10 1809'
    'ResizePseudoConsole'                 = 'Windows 10 1809'
    'ClosePseudoConsole'                  = 'Windows 10 1809'
    'GetTempPath2W'                       = 'Windows 11 (na desítkách až s pozdními aktualizacemi)'
    'GetTempPath2A'                       = 'Windows 11 (na desítkách až s pozdními aktualizacemi)'
    'GetMachineTypeAttributes'            = 'Windows 11'
    'GetFileInformationByName'            = 'Windows 11 24H2'
    'CreateFile3'                         = 'Windows 11 24H2'
    'CreateDirectory2W'                   = 'Windows 11 24H2'
    'CreateDirectory2A'                   = 'Windows 11 24H2'
    'RemoveDirectory2W'                   = 'Windows 11 24H2'
    'RemoveDirectory2A'                   = 'Windows 11 24H2'
    'DeleteFile2W'                        = 'Windows 11 24H2'
    'DeleteFile2A'                        = 'Windows 11 24H2'
}

# ── Hlavní část ────────────────────────────────────────────────────

if (-not $Path -or $Path.Count -eq 0) {
    Write-Host "Použití: tools\check-imports.ps1 [-RequireDependentLoadFlag] [-ShowFunctions] <soubor.exe> [další.exe …]" -ForegroundColor Yellow
    exit 2
}

$machineNames = @{ 0x8664 = 'x64'; 0x14C = 'x86'; 0xAA64 = 'ARM64' }
$problems = @()
$runtimeProblems = @()
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
    # Pro Windows je to jeden soubor, takže se vypíše i posoudí jednou;
    # funkce obou záznamů se sečtou.
    $seen = @{}
    $rows = New-Object System.Collections.Generic.List[object]
    foreach ($kind in @(@{ List = $imp.Static; Delay = $false }, @{ List = $imp.Delay; Delay = $true })) {
        foreach ($d in $kind.List) {
            $key = $d.Dll.ToLowerInvariant() + $(if ($kind.Delay) { '|delay' } else { '' })
            if ($seen.ContainsKey($key)) {
                $seen[$key].Functions += $d.Functions
                continue
            }
            $row = [pscustomobject]@{ Name = $d.Dll; Delay = $kind.Delay; Functions = @($d.Functions) }
            $seen[$key] = $row
            $rows.Add($row)
        }
    }
    $delayCount = @($rows | Where-Object { $_.Delay }).Count
    $fnCount = 0
    foreach ($r in $rows) { $fnCount += $r.Functions.Count }
    Write-Host ("{0}  ({1}, {2} DLL, {3} funkcí, z toho {4} DLL zpožděně)" -f $leaf, $arch, $rows.Count, $fnCount, $delayCount) -ForegroundColor Cyan

    foreach ($r in $rows) {
        $why = Get-DllProblem $r.Name $sysDir
        # DLL jen za běhu: ve Windows je, vadí import (viz hlavička).
        $runtimeWhy = $null
        if (-not $why) { $runtimeWhy = Get-RuntimeOnlyProblem $leaf $r.Name }
        if (-not $why -and -not $runtimeWhy -and $r.Delay) {
            $why = 'zpožděně načítaná DLL (delay-load) — zavaděč ji při startu nekontroluje, chybějící funkce shodí program až uprostřed práce a /DEPENDENTLOADFLAG ji spolehlivě nechrání. Importuj staticky, nebo volitelné API načti přes LoadLibraryExW(…, LOAD_LIBRARY_SEARCH_SYSTEM32) + GetProcAddress s náhradou.'
        }
        $note = if ($r.Delay) { '  (zpožděně)' } else { '' }
        if ($runtimeWhy) {
            Write-Host ("  !!  {0}{1}" -f $r.Name, $note) -ForegroundColor Red
            $runtimeProblems += [pscustomobject]@{ File = $leaf; Name = $r.Name; Why = $runtimeWhy }
        } elseif ($why) {
            Write-Host ("  !!  {0}{1}" -f $r.Name, $note) -ForegroundColor Red
            $problems += [pscustomobject]@{ File = $leaf; Name = $r.Name; Why = $why }
        } else {
            Write-Host ("  ok  {0}{1}  ({2})" -f $r.Name, $note, $r.Functions.Count) -ForegroundColor DarkGray
        }
        foreach ($f in $r.Functions) {
            if ($PostRtmFunctions.ContainsKey($f)) {
                $since = $PostRtmFunctions[$f]
                Write-Host ("  !!    {0}!{1}  (až {2})" -f $r.Name, $f, $since) -ForegroundColor Red
                $problems += [pscustomobject]@{
                    File = $leaf
                    Name = '{0}!{1}' -f $r.Name, $f
                    # Jednoduché uvozovky: PowerShell bere „ a “ jako
                    # uvozovky a v "…" by řetězec ukončily.
                    Why  = 'funkce je až od {0}. Staticky importovaná zastaví start programu na starších Windows 10 hláškou „vstupní bod nenalezen“ dřív, než program cokoli řekne. Hledej ji za běhu (GetModuleHandleW/LoadLibraryExW + GetProcAddress) a měj náhradu pro starší build (princip 9).' -f $since
                }
            }
        }
        if ($ShowFunctions) {
            foreach ($f in $r.Functions) { Write-Host ("        {0}" -f $f) -ForegroundColor DarkGray }
        }
    }

    # DLL načítané za běhu (viz hlavička): jen informace o importu funkce.
    $safeSearch = $false
    foreach ($r in $rows) {
        if (-not $r.Delay -and $r.Name -ieq 'kernel32.dll' -and $r.Functions -contains 'SetDefaultDllDirectories') { $safeSearch = $true }
    }
    if ($safeSearch) {
        Write-Host '  ok  SetDefaultDllDirectories (DLL načítané za běhu může omezit na System32)' -ForegroundColor DarkGray
    } else {
        Write-Host '  --  bez SetDefaultDllDirectories (DLL načítané za běhu se hledají i ve složce s .exe)' -ForegroundColor Yellow
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
if ($problems.Count -gt 0 -or $runtimeProblems.Count -gt 0 -or $flagProblems.Count -gt 0) {
    if ($problems.Count -gt 0) {
        Write-Host "NEPROŠLO: binárka potřebuje DLL nebo funkci, která na čistém nebo starším PC chybí." -ForegroundColor Red
        foreach ($x in $problems) {
            Write-Host ""
            Write-Host ("  {0} → {1}" -f $x.File, $x.Name) -ForegroundColor Red
            Write-Host ("    {0}" -f $x.Why)
        }
        Write-Host ""
        Write-Host "Na tomhle PC se chyba neprojeví — Windows si DLL i funkci najde. Kamarádovi se KeyPad nespustí." -ForegroundColor Yellow
    }
    if ($runtimeProblems.Count -gt 0) {
        if ($problems.Count -gt 0) { Write-Host "" }
        # Ne „kamarádovi se nespustí": tyhle DLL Windows mají, KeyPad by
        # běžel — vadí podvržená DLL a zátěž (viz „DLL jen za běhu").
        Write-Host "NEPROŠLO: binárka importuje DLL, kterou smí načítat jen za běhu (podvržené DLL, zvukové ovladače ze složky programu)." -ForegroundColor Red
        foreach ($x in $runtimeProblems) {
            Write-Host ""
            Write-Host ("  {0} → {1}" -f $x.File, $x.Name) -ForegroundColor Red
            Write-Host ("    {0}" -f $x.Why)
        }
        Write-Host ""
        Write-Host "Tyhle DLL Windows mají (KeyPad se spustí i u kamaráda) — jde o bezpečnost a lehkost, ne o kompatibilitu." -ForegroundColor Yellow
    }
    if ($flagProblems.Count -gt 0) {
        if ($problems.Count -gt 0 -or $runtimeProblems.Count -gt 0) { Write-Host "" }
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
    Write-Host "Importy v pořádku — jen systémové DLL a funkce z Windows 10 1507, DLL jen ze System32." -ForegroundColor Green
} else {
    Write-Host "Importy v pořádku — jen systémové DLL a funkce z Windows 10 1507." -ForegroundColor Green
}
exit 0

# okno-test.ps1 — okno KeyPadu naostro, ale jen na skryté ploše (Fáze 6, B6).
#
#   powershell -ExecutionPolicy Bypass -File tools\okno-test.ps1
#   powershell -ExecutionPolicy Bypass -File tools\okno-test.ps1 -BezBuildu -Snimky C:\kam\snimky
#
# Co dělá: postaví debug build přes Tauri CLI, spustí ho na vlastní
# skryté ploše (CreateDesktopW + STARTUPINFO.lpDesktop) s izolovaným
# APPDATA a LOCALAPPDATA a přes Chrome DevTools Protocol (CDP, .NET
# ClientWebSocket) projde scénáře editoru kláves: karty ovladačů 2–4
# a jejich barvy, klávesy psané do okna (mezerník, Enter, šipky, F5,
# Ctrl+R, Ctrl+P — spec 1.8), přiřazení, přesun a „Zpět", přidání (+),
# vyprázdnění (× i pravý klik), výchozí klávesy (↺), odebrání ovladače
# (🗑), klik jinam, 10s limit, Win a AltGr, živé svícení, zapnutí
# simulovaného ovladače, rozvržení při 380 a 440 px, minimalizaci během
# přiřazování, restart (klávesy i karta zůstanou) a poškozenou
# konfiguraci. Snímky okna ukládá do -Snimky (Page.captureScreenshot),
# na konci uklidí proces i plochu.
#
# PROČ SKRYTÁ PLOCHA: na počítači vlastníka může běžet hra. Okno na jeho
# ploše by mu vyskočilo přes ni a simulovaný vstup (SendInput) by šel do
# hry. Proto:
#   - žádný SendInput — kliky jsou element.click() v okně, klávesy jdou
#     příkazem `test_klavesa` do hooku stejnou funkcí jako skutečné
#     (jen debug build a jen s KEYPAD_TEST_KLAVESY=1). Klávesy psané do
#     samotného okna (spec 1.8) posílá CDP Input.dispatchKeyEvent rovnou
#     do WebView testovací instance — systémem neprojdou, hook ani jiný
#     program je neuvidí;
#   - KEYPAD_BEZ_VIGEM=pad: simulovaný ovladač, ViGEmBus vlastníka se
#     nepoužije a nic se nepřehraje (simulace je vždy potichu);
#   - pojmenovaná událost Local\KeyPad.Ukoncit se NIKDY nenastavuje
#     (je sdílená s nainstalovaným KeyPadem; simulace ji ani nehlídá).
#     Konec je taskkill.exe plnou cestou na PID testovací instance —
#     zavření okna by ho jen schovalo do oznamovací oblasti;
#   - před spuštěním se ověří, že FindWindowW z testovací plochy
#     nevidí okno pluginu single-instance (třída `<identifier>-sic`).
#     Kdyby ho viděla, testovací instance by místo vlastního startu
#     ukázala okno nainstalovaného KeyPadu na ploše vlastníka — pak
#     skript nic nespouští a končí kódem 3.
#
# Proč PowerShell 5.1 a Add-Type: žádná nová závislost (bun ani Node se
# k okenním testům nepotřebují) a Windows ho mají vždycky.
#
# Návratový kód: 0 = všechny kontroly prošly · 1 = některá selhala ·
# 2 = harness nedoběhl (build, spuštění, CDP) · 3 = zastaveno kvůli
# bezpečnosti (viz výš).
#
# POZOR na kódování: soubor musí zůstat v UTF-8 s BOM (PowerShell 5.1).
# A české uvozovky „ “ ” PowerShell bere jako " — uvnitř řetězce v "…"
# ho ukončí. Text s nimi patří do '…'.

#Requires -Version 5.1
param(
    # Exe už je postavené (druhý běh za sebou) — build se přeskočí.
    [switch]$BezBuildu,
    # Kam uložit snímky okna (PNG). Výchozí: snimky\ v pracovní složce.
    [string]$Snimky,
    # Pracovní složka běhu (APPDATA, LOCALAPPDATA, kopie exe, log).
    # Výchozí: %TEMP%\keypad-okno-test. Po úspěchu se běh smaže.
    [string]$Prac,
    # Nechat složku běhu i po úspěchu (log, config.json).
    [switch]$Nechat
)

$ErrorActionPreference = 'Stop'
if ([Console]::IsOutputRedirected) { [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false) }

if ($PSVersionTable.PSEdition -eq 'Core') {
    # ClientWebSocket a HttpWebRequest jsou v .NET Core v jiných
    # sestaveních a Add-Type by je bez výčtu nenašel; skript je psaný
    # pro Windows PowerShell 5.1, který mají všechny Windows.
    Write-Host 'Spusť přes Windows PowerShell 5.1: powershell -ExecutionPolicy Bypass -File tools\okno-test.ps1' -ForegroundColor Red
    exit 2
}

$root = Split-Path -Parent $PSScriptRoot
$exeZdroj = Join-Path $root 'target\debug\KeyPad.exe'
$conf = Get-Content (Join-Path $root 'src-tauri\tauri.conf.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$identifier = $conf.identifier
if (-not $Prac) { $Prac = Join-Path $env:TEMP 'keypad-okno-test' }
$taskkill = Join-Path $env:SystemRoot 'System32\taskkill.exe'

# ── P/Invoke a CDP ─────────────────────────────────────────────────
# C# 5 (kompilátor .NET Frameworku v PowerShellu 5.1): bez interpolace
# řetězců, `?.` a `nameof`.
$cs = @'
using System;
using System.Collections;
using System.Collections.Generic;
using System.IO;
using System.Net;
using System.Net.WebSockets;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

namespace KeyPadOknoTest {
    public static class Plocha {
        [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        static extern IntPtr CreateDesktopW(string name, IntPtr device, IntPtr devmode, uint flags, uint access, IntPtr sa);
        [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        static extern IntPtr OpenDesktopW(string name, uint flags, bool inherit, uint access);
        [DllImport("user32.dll", SetLastError = true)]
        static extern bool CloseDesktop(IntPtr h);
        [DllImport("user32.dll", SetLastError = true)]
        static extern bool SetThreadDesktop(IntPtr h);
        [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        static extern IntPtr FindWindowW(string cls, string name);

        const uint GENERIC_ALL = 0x10000000;
        const uint DESKTOP_READOBJECTS = 0x0001;

        public static IntPtr Vytvor(string jmeno) {
            IntPtr h = CreateDesktopW(jmeno, IntPtr.Zero, IntPtr.Zero, 0, GENERIC_ALL, IntPtr.Zero);
            if (h == IntPtr.Zero) throw new Exception("CreateDesktopW selhalo: " + Marshal.GetLastWin32Error());
            return h;
        }

        public static bool Zavri(IntPtr h) { return CloseDesktop(h); }

        /// Plocha zaniká s posledním handlem a vláknem — po úklidu už nesmí jít otevřít.
        public static bool Existuje(string jmeno) {
            IntPtr h = OpenDesktopW(jmeno, 0, false, DESKTOP_READOBJECTS);
            if (h == IntPtr.Zero) return false;
            CloseDesktop(h);
            return true;
        }

        /// FindWindowW na vlastní ploše (vlákno PowerShellu) — jen pro výpis.
        public static bool OknoNaMePlose(string trida, string nazev) {
            return FindWindowW(trida, nazev) != IntPtr.Zero;
        }

        /// Totéž FindWindowW, které udělá plugin single-instance v testovací
        /// instanci: z vlákna připojeného na skrytou plochu. "" = nic nenašel.
        public static string NajdiOkno(string jmeno, string trida, string nazev) {
            string vysledek = null;
            IntPtr h = IntPtr.Zero;
            Thread t = new Thread(() => {
                h = OpenDesktopW(jmeno, 0, false, GENERIC_ALL);
                if (h == IntPtr.Zero) { vysledek = "chyba OpenDesktopW " + Marshal.GetLastWin32Error(); return; }
                if (!SetThreadDesktop(h)) { vysledek = "chyba SetThreadDesktop " + Marshal.GetLastWin32Error(); return; }
                IntPtr w = FindWindowW(trida, nazev);
                vysledek = w == IntPtr.Zero ? "" : ("0x" + w.ToInt64().ToString("X"));
            });
            t.Start();
            t.Join();
            // Handle plochy jde zavřít, až vlákno, které ji používalo, skončí.
            if (h != IntPtr.Zero) CloseDesktop(h);
            return vysledek;
        }
    }

    public sealed class Proces : IDisposable {
        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        struct STARTUPINFO {
            public int cb; public string lpReserved; public string lpDesktop; public string lpTitle;
            public int dwX, dwY, dwXSize, dwYSize, dwXCountChars, dwYCountChars, dwFillAttribute, dwFlags;
            public short wShowWindow, cbReserved2; public IntPtr lpReserved2, hStdInput, hStdOutput, hStdError;
        }
        [StructLayout(LayoutKind.Sequential)]
        struct PROCESS_INFORMATION { public IntPtr hProcess, hThread; public int dwProcessId, dwThreadId; }
        [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        static extern bool CreateProcessW(string app, StringBuilder cmd, IntPtr pa, IntPtr ta, bool inherit, uint flags,
            IntPtr env, string cwd, ref STARTUPINFO si, out PROCESS_INFORMATION pi);
        [DllImport("kernel32.dll")] static extern uint WaitForSingleObject(IntPtr h, uint ms);
        [DllImport("kernel32.dll")] static extern bool GetExitCodeProcess(IntPtr h, out uint code);
        [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);

        // Debug build je konzolová aplikace: bez CREATE_NO_WINDOW by psala
        // do konzole skriptu (nebo by si otevřela okno konzole na ploše vlastníka).
        const uint CREATE_UNICODE_ENVIRONMENT = 0x00000400;
        const uint CREATE_NO_WINDOW = 0x08000000;

        public int Pid;
        IntPtr hProc;

        /// `zmeny`: "JMENO=hodnota" přepíše, "JMENO=" odebere; zbytek prostředí
        /// se zdědí. Prostředí jde jen dítěti — skript sám se nemění.
        public static Proces Spust(string plocha, string exe, string cwd, string[] zmeny) {
            SortedDictionary<string, string> env = new SortedDictionary<string, string>(StringComparer.OrdinalIgnoreCase);
            foreach (DictionaryEntry e in Environment.GetEnvironmentVariables()) env[(string)e.Key] = (string)e.Value;
            foreach (string z in zmeny) {
                int i = z.IndexOf('=');
                string k = z.Substring(0, i), v = z.Substring(i + 1);
                if (v.Length == 0) env.Remove(k); else env[k] = v;
            }
            StringBuilder blok = new StringBuilder();
            foreach (KeyValuePair<string, string> kv in env) blok.Append(kv.Key).Append('=').Append(kv.Value).Append('\0');
            blok.Append('\0');
            IntPtr pEnv = Marshal.StringToHGlobalUni(blok.ToString());
            try {
                STARTUPINFO si = new STARTUPINFO();
                si.cb = Marshal.SizeOf(typeof(STARTUPINFO));
                si.lpDesktop = "WinSta0\\" + plocha;
                PROCESS_INFORMATION pi;
                StringBuilder cmd = new StringBuilder("\"" + exe + "\"");
                if (!CreateProcessW(exe, cmd, IntPtr.Zero, IntPtr.Zero, false, CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
                        pEnv, cwd, ref si, out pi))
                    throw new Exception("CreateProcessW selhalo: " + Marshal.GetLastWin32Error());
                CloseHandle(pi.hThread);
                Proces p = new Proces();
                p.Pid = pi.dwProcessId;
                p.hProc = pi.hProcess;
                return p;
            } finally {
                Marshal.FreeHGlobal(pEnv);
            }
        }

        /// true = proces skončil.
        public bool Ceka(int ms) { return WaitForSingleObject(hProc, (uint)ms) == 0; }

        public long Kod() { uint k; return GetExitCodeProcess(hProc, out k) ? (long)k : -1L; }

        public void Dispose() {
            if (hProc != IntPtr.Zero) { CloseHandle(hProc); hProc = IntPtr.Zero; }
        }
    }

    /// Minimální klient CDP: jen požadavek → odpověď se stejným `id`,
    /// události se zahazují (žádná doména se nezapíná).
    public sealed class Cdp : IDisposable {
        ClientWebSocket ws;
        int dalsi;

        public static string Http(string url, int ms) {
            HttpWebRequest req = (HttpWebRequest)WebRequest.Create(url);
            req.Timeout = ms;
            req.Proxy = null;
            using (HttpWebResponse r = (HttpWebResponse)req.GetResponse())
            using (StreamReader sr = new StreamReader(r.GetResponseStream(), Encoding.UTF8))
                return sr.ReadToEnd();
        }

        public void Pripoj(string url, int ms) {
            ws = new ClientWebSocket();
            ws.Options.Proxy = null;
            using (CancellationTokenSource c = new CancellationTokenSource(ms))
                ws.ConnectAsync(new Uri(url), c.Token).Wait();
        }

        /// Celá odpověď jako text JSON (`{"id":N,"result":…}` nebo `"error"`).
        public string Volej(string metoda, string parametry, int ms) {
            int id = ++dalsi;
            string zprava = "{\"id\":" + id + ",\"method\":\"" + metoda + "\",\"params\":" + (parametry ?? "{}") + "}";
            byte[] b = Encoding.UTF8.GetBytes(zprava);
            using (CancellationTokenSource c = new CancellationTokenSource(ms)) {
                ws.SendAsync(new ArraySegment<byte>(b), WebSocketMessageType.Text, true, c.Token).Wait();
                // Chromium píše `id` jako první klíč odpovědi; události začínají `{"method"`.
                string zacatek = "{\"id\":" + id + ",";
                while (true) {
                    string m = Prijmi(c.Token);
                    if (m.StartsWith(zacatek, StringComparison.Ordinal)) return m;
                }
            }
        }

        string Prijmi(CancellationToken ct) {
            byte[] buf = new byte[65536];
            using (MemoryStream ms = new MemoryStream()) {
                while (true) {
                    WebSocketReceiveResult r = ws.ReceiveAsync(new ArraySegment<byte>(buf), ct).Result;
                    if (r.MessageType == WebSocketMessageType.Close) throw new Exception("CDP zavřelo spojení");
                    ms.Write(buf, 0, r.Count);
                    if (r.EndOfMessage) return Encoding.UTF8.GetString(ms.ToArray());
                }
            }
        }

        /// Snímek okna do PNG; "" = uloženo, jinak začátek odpovědi.
        /// Base64 se vybírá přímo z textu: ConvertFrom-Json v PowerShellu
        /// 5.1 má na velikost dokumentu limit.
        public string Snimek(string cesta, int ms) {
            string r = Volej("Page.captureScreenshot", "{\"format\":\"png\"}", ms);
            int i = r.IndexOf("\"data\":\"", StringComparison.Ordinal);
            if (i < 0) return r.Length > 300 ? r.Substring(0, 300) : r;
            i += 8;
            int j = r.IndexOf('"', i);
            File.WriteAllBytes(cesta, Convert.FromBase64String(r.Substring(i, j - i)));
            return "";
        }

        public void Dispose() {
            if (ws != null) { ws.Dispose(); ws = null; }
        }
    }
}
'@
if (-not ('KeyPadOknoTest.Cdp' -as [type])) { Add-Type -TypeDefinition $cs -Language CSharp }

# ── Pomocníci v okně ───────────────────────────────────────────────
# Vkládá se před každým dotazem, který ho ještě nemá (po restartu je
# stránka nová). Čeká se v okně (`cekej`), ne smyčkou dotazů přes CDP.
$jsPomocnik = @'
window.__kpt = (() => {
  const q = (s) => document.querySelector(s);
  const qa = (s) => [...document.querySelectorAll(s)];
  const app = () => q('.app');
  const kartaSel = (n) => 'section.karta[data-pad="' + n + '"]';
  const karta = (n) => q(kartaSel(n));
  const cep = (n, v) => q(kartaSel(n) + ' [data-vstup="' + v + '"]');
  const cekej = (f, ms) => new Promise((hotovo) => {
    const konec = Date.now() + (ms || 5000);
    const zkus = () => {
      let v = false;
      try { v = !!f(); } catch (e) { v = false; }
      if (v) hotovo(true); else if (Date.now() > konec) hotovo(false); else setTimeout(zkus, 40);
    };
    zkus();
  });
  const inv = (c, a) => window.__TAURI_INTERNALS__.invoke(c, a || {})
    .then((v) => ({ ok: true, v }), (e) => ({ ok: false, e: String(e) }));
  const klavesa = async (scan, e0, dolu) => {
    const r = await inv('test_klavesa', { scan, e0: !!e0, dolu: !!dolu });
    if (!r.ok) throw new Error('test_klavesa: ' + r.e);
    return true;
  };
  const klik = (s) => { const e = q(s); if (!e) return false; e.click(); return true; };
  const text = (e) => (e ? e.textContent.replace(/\s+/g, ' ').trim() : null);
  const napoveda = (n) => text(q(kartaSel(n) + ' .napoveda'));
  const nazev = (n, v) => text(q(kartaSel(n) + ' [data-vstup="' + v + '"] .nazev'));
  const barva = (n, s) => { const e = q(kartaSel(n) + ' ' + s); return e ? getComputedStyle(e).backgroundColor : null; };
  const karty = () => qa('section.karta').map((k) => Number(k.dataset.pad));
  const sviti = () => qa('[data-sviti]').map((e) => e.closest('section.karta').dataset.pad + ':' + e.dataset.vstup + '=' + e.dataset.sviti);
  // Rozvržení: nic nesmí přetéct okno ani kartu a čepičky se nesmí
  // překrývat. Vrací seznam problémů (prázdný = v pořádku).
  const pretek = () => {
    const chyby = [];
    const de = document.documentElement;
    if (de.scrollWidth > innerWidth + 0.5) chyby.push('stránka je širší než okno: ' + de.scrollWidth + ' > ' + innerWidth);
    for (const k of qa('section.karta')) {
      const rk = k.getBoundingClientRect();
      const uvnitr = (r) => r.left >= rk.left - 0.5 && r.right <= rk.right + 0.5;
      for (const s of k.querySelectorAll('.hlava > *')) {
        if (!uvnitr(s.getBoundingClientRect())) chyby.push('karta ' + k.dataset.pad + ': hlavička přetéká (' + s.className + ')');
      }
      const ceps = [...k.querySelectorAll('.cepicka')].map((c) => [c.dataset.vstup, c.querySelector('.telo').getBoundingClientRect()]);
      for (const [v, r] of ceps) if (!uvnitr(r)) chyby.push('karta ' + k.dataset.pad + ': čepička ' + v + ' mimo kartu');
      for (let i = 0; i < ceps.length; i++)
        for (let j = i + 1; j < ceps.length; j++) {
          const a = ceps[i][1], b = ceps[j][1];
          const x = Math.min(a.right, b.right) - Math.max(a.left, b.left);
          const y = Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top);
          if (x > 1 && y > 1) chyby.push('karta ' + k.dataset.pad + ': ' + ceps[i][0] + ' překrývá ' + ceps[j][0]);
        }
      for (const t of k.querySelectorAll('.cepicka .nazev')) {
        const r = t.getBoundingClientRect(), rt = t.closest('.telo').getBoundingClientRect();
        if (r.left < rt.left - 0.5 || r.right > rt.right + 0.5) chyby.push('karta ' + k.dataset.pad + ': název „' + t.textContent + '" se nevejde do čepičky');
      }
    }
    return chyby;
  };
  // Pro posudek čitelnosti: nejmenší písmo viditelného textu a velikost čepičky.
  const miry = () => {
    let min = 99, kde = '';
    for (const e of qa('.app *')) {
      if (!e.childNodes.length || ![...e.childNodes].some((n) => n.nodeType === 3 && n.textContent.trim())) continue;
      const r = e.getBoundingClientRect();
      if (!r.width || !r.height) continue;
      const f = parseFloat(getComputedStyle(e).fontSize);
      if (f < min) { min = f; kde = (e.className || e.tagName) + ' „' + e.textContent.trim().slice(0, 20) + '"'; }
    }
    const t = q('.cepicka .telo');
    return { pismoMin: min, kde, cepicka: t ? Math.round(t.getBoundingClientRect().width) : null, sirka: innerWidth, vyska: innerHeight };
  };
  return { q, qa, app, karta, cep, cekej, inv, klavesa, klik, text, napoveda, nazev, barva, karty, sviti, pretek, miry };
})();
'@

# ── Výsledky ───────────────────────────────────────────────────────
$script:kontrol = 0
$script:chyb = 0
$script:selhane = New-Object Collections.Generic.List[string]

function Over([string]$Co, [bool]$Ok, $Detail = $null) {
    $script:kontrol++
    if ($Ok) {
        Write-Host "  OK     $Co"
    } else {
        $script:chyb++
        $d = if ($null -ne $Detail) { ' → ' + (ConvertTo-Json -InputObject $Detail -Compress -Depth 6) } else { '' }
        Write-Host "  CHYBA  $Co$d" -ForegroundColor Red
        $script:selhane.Add($Co)
    }
}

function Krok([string]$Text) { Write-Host ''; Write-Host $Text -ForegroundColor Cyan }

# ── CDP ────────────────────────────────────────────────────────────
$script:cdp = $null

# Výraz JS (smí být `await`), výsledek přes JSON zpět jako objekt PowerShellu.
function Js([string]$Vyraz, [int]$Ms = 20000) {
    $expr = "(async () => { if (!window.__kpt) { $jsPomocnik } const __v = await ($Vyraz); return JSON.stringify(__v === undefined ? null : __v); })()"
    $p = '{"expression":' + (ConvertTo-Json -InputObject $expr -Compress) + ',"awaitPromise":true,"returnByValue":true}'
    try {
        $raw = $script:cdp.Volej('Runtime.evaluate', $p, $Ms)
    } catch {
        # Task.Wait() balí chybu do AggregateException („One or more errors…").
        $e = $_.Exception
        while ($e.InnerException) { $e = $e.InnerException }
        throw "CDP: $($e.Message)"
    }
    $r = ConvertFrom-Json -InputObject $raw
    if ($r.error) { throw "CDP: $($r.error.message)" }
    if ($r.result.exceptionDetails) {
        $ex = $r.result.exceptionDetails
        $txt = if ($ex.exception -and $ex.exception.description) { $ex.exception.description } else { $ex.text }
        throw "JS: $txt"
    }
    $s = $r.result.result.value
    if ($null -eq $s) { return $null }
    return (ConvertFrom-Json -InputObject $s)
}

function Cekej([string]$Podminka, [int]$Ms = 5000) {
    return [bool](Js "__kpt.cekej(() => ($Podminka), $Ms)" ($Ms + 10000))
}

function CekejNa([scriptblock]$Podminka, [int]$Ms = 5000) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    do {
        try { if (& $Podminka) { return $true } } catch { }
        Start-Sleep -Milliseconds 60
    } while ($sw.ElapsedMilliseconds -lt $Ms)
    return $false
}

function Klavesa([int]$Scan, [bool]$E0, [bool]$Dolu) {
    $null = Js ("__kpt.klavesa($Scan, " + $E0.ToString().ToLower() + ', ' + $Dolu.ToString().ToLower() + ')')
}

function Stisk([int]$Scan, [bool]$E0 = $false) {
    Klavesa $Scan $E0 $true
    Klavesa $Scan $E0 $false
}

function Klik([string]$Selektor) {
    return [bool](Js ('__kpt.klik(' + (ConvertTo-Json -InputObject $Selektor -Compress) + ')'))
}

# Klávesa do WebView přes CDP (důvěryhodná událost jako od klávesnice,
# ale jen v okně testovací instance — ne SendInput, systém ji nevidí).
# $Mod: 2 = Ctrl. S $Text jde keyDown (napíše znak), bez něj rawKeyDown.
function OknuKlavesa([string]$Key, [string]$Code, [int]$Vk, [int]$Mod = 0, [string]$Text = '') {
    foreach ($typ in @($(if ($Text) { 'keyDown' } else { 'rawKeyDown' }), 'keyUp')) {
        $p = @{ type = $typ; key = $Key; code = $Code; windowsVirtualKeyCode = $Vk; nativeVirtualKeyCode = $Vk; modifiers = $Mod }
        if ($Text -and $typ -ne 'keyUp') { $p.text = $Text; $p.unmodifiedText = $Text }
        $r = ConvertFrom-Json -InputObject ($script:cdp.Volej('Input.dispatchKeyEvent', (ConvertTo-Json -InputObject $p -Compress), 10000))
        if ($r.error) { throw "CDP Input.dispatchKeyEvent $Key`: $($r.error.message)" }
    }
}

function PravyKlik([string]$Selektor) {
    $s = ConvertTo-Json -InputObject $Selektor -Compress
    return [bool](Js "(() => { const e = __kpt.q($s); if (!e) return false; e.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2 })); return true; })()")
}

# Stisk myši mimo čepičky (pointerdown, jako začátek kliku) — okno ho
# bere jako „klik jinam".
function StiskMysi([string]$Selektor) {
    $s = ConvertTo-Json -InputObject $Selektor -Compress
    return [bool](Js "(() => { const e = __kpt.q($s); if (!e) return false; e.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, cancelable: true })); return true; })()")
}

function Prikaz([string]$Nazev, [string]$ArgumentyJson = '{}') {
    return Js ('__kpt.inv(' + (ConvertTo-Json -InputObject $Nazev -Compress) + ", $ArgumentyJson)")
}

function Klavesy { (Prikaz 'klavesy').v }

function Vazba($K, [int]$Scan, [bool]$E0 = $false) {
    @($K.vazby | Where-Object { $_.klavesa.scan -eq $Scan -and [bool]$_.klavesa.e0 -eq $E0 }) | Select-Object -First 1
}

function Snimek([string]$Jmeno) {
    # Krátké animace (rozbalení karty, záblesk, zatřesení) nechat doběhnout,
    # ať snímek ukazuje stav, ne mezistav; nekonečný pulz čepičky a 10s
    # odpočet přiřazování se nečekají.
    $null = Cekej 'document.getAnimations().filter((a) => a.playState === "running" && a.effect && a.effect.getComputedTiming().endTime < 1000).length === 0' 2000
    $cesta = Join-Path $script:snimky "$Jmeno.png"
    $chyba = $script:cdp.Snimek($cesta, 20000)
    if ($chyba) { Write-Host "  (snímek $Jmeno se nepovedl: $chyba)" -ForegroundColor Yellow }
    else { Write-Host "  snímek  $cesta" -ForegroundColor DarkGray }
}

function Pretek([string]$Kde) {
    $p = @(Js '__kpt.pretek()')
    Over "rozvržení $Kde bez přetečení a překryvů" ($p.Count -eq 0) $p
    $m = Js '__kpt.miry()'
    Write-Host ("         okno {0}×{1} px, čepička {2} px, nejmenší písmo {3} px ({4})" -f $m.sirka, $m.vyska, $m.cepicka, $m.pismoMin, $m.kde) -ForegroundColor DarkGray
}

# ── Proces na skryté ploše ─────────────────────────────────────────
$script:proces = $null
$script:stop = $false

# taskkill píše chyby na stderr; s 'Stop' by z nich PowerShell 5.1 při
# přesměrování udělal výjimku (NativeCommandError) uprostřed úklidu.
function Taskkill([string[]]$Argumenty) {
    $ErrorActionPreference = 'Continue'
    & $taskkill @Argumenty 2>&1 | Out-Null
}

# Plugin single-instance v testovací instanci najde okno běžící instance
# přes FindWindowW na SVÉ ploše. Kdyby z testovací plochy bylo vidět okno
# nainstalovaného KeyPadu, testovací instance by mu poslala WM_COPYDATA
# a jeho okno by vyskočilo na ploše vlastníka — pak nic nespouštět.
function OverSingleInstance {
    $najde = [KeyPadOknoTest.Plocha]::NajdiOkno($script:plocha, "$identifier-sic", "$identifier-siw")
    if ($null -eq $najde -or $najde.StartsWith('chyba')) { throw "kontrolu single-instance na skryté ploše nejde provést: $najde" }
    if ($najde -ne '') {
        $script:stop = $true
        throw "STOP: z testovací plochy je vidět okno KeyPadu ($najde) — testovací instance by ho ukázala na ploše vlastníka"
    }
}

function Spust {
    OverSingleInstance
    # Starý DevToolsActivePort po tvrdém konci zůstává — port by byl mrtvý.
    Get-ChildItem $script:localApp -Recurse -Filter 'DevToolsActivePort' -ErrorAction SilentlyContinue |
        Remove-Item -Force -ErrorAction SilentlyContinue
    $zmeny = @(
        "APPDATA=$($script:appData)",
        "LOCALAPPDATA=$($script:localApp)",
        'KEYPAD_BEZ_VIGEM=pad',
        'KEYPAD_TEST_KLAVESY=1',
        'KEYPAD_TEST_POPREDI=1',
        'WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=0',
        # Tauri bere složku dat WebView2 ze známé složky Windows
        # (SHGetKnownFolderPath), ne z proměnné LOCALAPPDATA — bez tohohle
        # by testovací instance psala do dat okna nainstalovaného KeyPadu
        # (%LOCALAPPDATA%\<identifier>\EBWebView; naměřeno). Proměnná WebView2
        # má přednost před složkou od aplikace; EBWebView si přidá sám.
        "WEBVIEW2_USER_DATA_FOLDER=$(Join-Path $script:localApp $identifier)",
        # Zděděný „starý ViGEmBus" by test obrátil jinam.
        'KEYPAD_VIGEM_STARY='
    )
    $script:proces = [KeyPadOknoTest.Proces]::Spust($script:plocha, $script:exe, $script:beh, $zmeny)
    Write-Host "  proces $($script:proces.Pid) na ploše $($script:plocha)" -ForegroundColor DarkGray
    Pripoj
}

function Pripoj {
    $soubor = Join-Path $script:localApp "$identifier\EBWebView\DevToolsActivePort"
    $ok = CekejNa { (Test-Path $soubor) -and ((Get-Content $soubor -ErrorAction Stop | Select-Object -First 1) -match '^\d+$') } 30000
    if (-not $ok) {
        if ($script:proces.Ceka(0)) { throw "KeyPad skončil hned po startu (kód $($script:proces.Kod())) — viz keypad.log v $($script:beh)" }
        throw "WebView2 nenapsal DevToolsActivePort ($soubor)"
    }
    $port = [int](Get-Content $soubor | Select-Object -First 1)
    $null = CekejNa {
        $seznam = ConvertFrom-Json -InputObject ([KeyPadOknoTest.Cdp]::Http("http://127.0.0.1:$port/json/list", 2000))
        $script:cilCdp = @($seznam | Where-Object { $_.type -eq 'page' }) | Select-Object -First 1
        $null -ne $script:cilCdp
    } 15000
    $cil = $script:cilCdp
    if (-not $cil) { throw "CDP na portu $port nemá stránku" }
    if ($script:cdp) { $script:cdp.Dispose() }
    $script:cdp = New-Object KeyPadOknoTest.Cdp
    $script:cdp.Pripoj($cil.webSocketDebuggerUrl, 10000)
    # Stránka může být ještě about:blank, než Tauri načte UI — dotaz do
    # kontextu, který navigace zahodí, by skončil chybou.
    $nacteno = CekejNa { Js 'document.readyState === "complete" && location.hostname === "tauri.localhost" && !!window.__TAURI_INTERNALS__' 5000 } 30000
    if (-not $nacteno) { throw 'UI se nenačetlo (tauri.localhost)' }
    Write-Host "  CDP 127.0.0.1:$port" -ForegroundColor DarkGray
}

function Ukonci {
    if ($script:cdp) { $script:cdp.Dispose(); $script:cdp = $null }
    if (-not $script:proces) { return }
    if (-not $script:proces.Ceka(0)) {
        # /T: i procesy WebView2, které KeyPad spustil. Zavření okna by
        # ho jen schovalo (ikona v oznamovací oblasti vzniká i tady).
        Taskkill @('/F', '/T', '/PID', "$($script:proces.Pid)")
        if (-not $script:proces.Ceka(10000)) { Write-Host "  proces $($script:proces.Pid) neskončil ani po taskkill" -ForegroundColor Red }
    }
    $script:proces.Dispose()
    $script:proces = $null
    # Další start se stejnou složkou dat by se jinak mohl připojit ke
    # končícímu prohlížeči WebView2 předchozí instance.
    $null = CekejNa { @(Zbytky).Count -eq 0 } 10000
}

# Procesy, které po testu nesmí zůstat: testovací KeyPad a WebView2
# s daty v pracovní složce běhu — podle cesty a příkazové řádky, ne
# podle jména ani PID (PID se po konci procesu může přidělit cizímu).
# KeyPad a Edge vlastníka se tak nikdy nedotkne.
function Zbytky {
    $vse = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue)
    @($vse | Where-Object {
            ($_.CommandLine -and $_.CommandLine.IndexOf($script:beh, [StringComparison]::OrdinalIgnoreCase) -ge 0) -or
            ($_.ExecutablePath -and $_.ExecutablePath -ieq $script:exe)
        })
}

function CekejNaKonfiguraci([scriptblock]$Overeni) {
    # Jméno se nesmí krýt s parametrem CekejNa: blok níž se volá z jejího
    # rozsahu a proměnné se hledají dynamicky.
    $cfg = Join-Path $script:appData 'KeyPad\config.json'
    return (CekejNa {
            if (-not (Test-Path $cfg)) { return $false }
            $j = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($cfg))
            & $Overeni $j
        } 5000)
}

# ── Scénáře ────────────────────────────────────────────────────────
function ScenarStart {
    Krok '1/11  Start (prázdné APPDATA)'
    Over 'okno načteno, ovladač 1 má 24 čepiček' (Cekej '!!__kpt.app() && __kpt.qa("section.karta[data-pad=\"1\"] [data-vstup]").length === 24 && !__kpt.cep(1, "a").hasAttribute("data-prazdna")' 30000)
    $sirka = Js 'innerWidth'
    Over "šířka okna 440 px (je $sirka)" ($sirka -ge 430 -and $sirka -le 450)
    # Skrytá plocha se nikdy nezobrazí: kdyby stránka platila za skrytou,
    # stály by animace a časovače by se brzdily — snímky i zprávy s limitem
    # by pak neodpovídaly tomu, co vidí uživatel.
    $pr = Js 'new Promise((ok) => { const t0 = performance.now(); setTimeout(() => ok({ vis: document.visibilityState, cas: Math.round(performance.now() - t0) }), 100); })'
    Write-Host ("         stránka: {0}, setTimeout(100) za {1} ms" -f $pr.vis, $pr.cas) -ForegroundColor DarkGray
    Over 'stránka je „visible“ a časovače neběží pozdě' ($pr.vis -eq 'visible' -and $pr.cas -lt 400) $pr
    $karty = @(Js '__kpt.karty()')
    Over 'po startu jen karta ovladače 1, rozbalená' (($karty -join ',') -eq '1' -and (Js '__kpt.karta(1).dataset.rozbalena === "true"')) $karty
    Over 'režim „vypnuto“, žádné přiřazování' (Js '__kpt.app().dataset.rezim === "disabled"')
    Over 'simulovaná sběrnice: pruh nad kartami není' (Js '!__kpt.q("div.pruh[data-sbernice]")')
    Over 'nabídka „+ Ovladač“ pro ovladač 2' (Js '!!__kpt.q("[data-pridat=\"2\"]")')
    $hlava = Js '["odznak", "jmeno", "veta"].map((c) => __kpt.text(__kpt.q("section.karta[data-pad=\"1\"] .rozbal ." + c)))'
    Over 'hlavička karty: 1 · Ovladač 1 · Vypnutý' (($hlava -join '|') -eq '1|Ovladač 1|Vypnutý') $hlava
    Over 'bez řádku nápovědy (ovladač má klávesy, nic se neděje)' ((Js '__kpt.napoveda(1)') -eq '')  (Js '__kpt.napoveda(1)')
    Pretek '440 px'
    Snimek '01-start-440'
}

function ScenarKarty {
    Krok '2/11  Ovladače 2–4 a jejich barvy'
    foreach ($n in 2, 3, 4) {
        $klik = Klik "[data-pridat=`"$n`"]"
        # Pozor: „ a “ jsou pro PowerShell uvozovky — v textu jen v '…'.
        Over ('„+ Ovladač“ přidá kartu ' + $n + ' a rozbalí ji') ($klik -and (Cekej "!!__kpt.karta($n) && __kpt.karta($n).dataset.rozbalena === `"true`""))
    }
    Over 'při čtyřech kartách „+ Ovladač“ zmizí' (Cekej '!__kpt.q("[data-pridat]")')
    Over 'rozbalená je jen jedna karta' ((Js '__kpt.qa("section.karta[data-rozbalena=\"true\"]").length') -eq 1)
    $cekane = @('rgb(96, 165, 250)', 'rgb(244, 114, 182)', 'rgb(167, 139, 250)', 'rgb(103, 232, 249)')
    $odznaky = @(Js '[1, 2, 3, 4].map((n) => __kpt.barva(n, ".odznak"))')
    $pruhy = @(Js '[1, 2, 3, 4].map((n) => __kpt.barva(n, ".pruh"))')
    Over 'barvy odznaků 1–4 = --pad-1..4 (modrá, růžová, fialová, azurová)' ((($odznaky -join '|') -eq ($cekane -join '|'))) $odznaky
    Over 'barvy pruhů 1–4 = --pad-1..4' ((($pruhy -join '|') -eq ($cekane -join '|'))) $pruhy
    Over 'odznak nese číslo ovladače' (Js '[1, 2, 3, 4].every((n) => __kpt.text(__kpt.q(`section.karta[data-pad="${n}"] .odznak`)) === String(n))')
    Over 'prázdný ovladač 4: všechny čepičky čárkované' ((Js '__kpt.qa("section.karta[data-pad=\"4\"] .cepicka[data-prazdna]").length') -eq 24)
    Over 'prázdný ovladač 4: „Klikni na vstup a stiskni klávesu“' ((Js '__kpt.napoveda(4)') -eq 'Klikni na vstup a stiskni klávesu') (Js '__kpt.napoveda(4)')
    Over 'prázdný ovladač: tečka jen obrysem („Nemá klávesy“)' (Js '__kpt.q("section.karta[data-pad=\"4\"] .dot").classList.contains("prazdny") && __kpt.q("section.karta[data-pad=\"4\"] .dot").title.startsWith("Nemá klávesy")')
    Pretek '440 px, 4 karty'
    Snimek '02-ctyri-karty'
}

function ScenarKlavesyOkna {
    Krok '3/11  Klávesy psané do okna (spec 1.8) — důvěryhodné události přes CDP'
    # Nižší okno, ať má panel co posouvat (4 karty, jedna rozbalená).
    $null = $script:cdp.Volej('Emulation.setDeviceMetricsOverride', '{"width":440,"height":480,"deviceScaleFactor":0,"mobile":false}', 10000)
    $null = Cekej 'innerHeight === 480'
    # Skrytá plocha nemá popředí — s emulací fokusu bere stránka klávesy
    # jako stisknuté v okně, které fokus má (jako u uživatele).
    $null = $script:cdp.Volej('Emulation.setFocusEmulationEnabled', '{"enabled":true}', 10000)
    # Záznam keydown až po stráži (ta poslouchá v zachytávací fázi): co
    # došlo, jestli to byla důvěryhodná událost a jestli stráž zrušila
    # výchozí akci. Značka v `window` zmizí, kdyby se stránka obnovila.
    $pripraveno = Js '(() => {
        window.__kptZnacka = 1;
        window.__kptKlavesy = [];
        window.__kptTisk = false;
        addEventListener("keydown", (e) => __kptKlavesy.push({ k: e.key, p: e.defaultPrevented, t: e.isTrusted }));
        addEventListener("beforeprint", () => { window.__kptTisk = true; });
        const panel = __kpt.q("main.panel");
        panel.scrollTop = 0;
        const sw = __kpt.q("section.karta[data-pad=\"1\"] button[role=\"switch\"]");
        sw.focus();
        return { posouva: panel.scrollHeight > panel.clientHeight + 20, fokus: document.activeElement === sw };
    })()'
    Over 'panel se dá posouvat a přepínač ovladače 1 má fokus (bez Tabu)' ($pripraveno.posouva -and $pripraveno.fokus) $pripraveno

    OknuKlavesa ' ' 'Space' 32 0 ' '
    OknuKlavesa 'Enter' 'Enter' 13 0 "`r"
    foreach ($k in @(@('ArrowDown', 40), @('PageDown', 34), @('End', 35))) { OknuKlavesa $k[0] $k[0] $k[1] }
    Start-Sleep -Milliseconds 500
    $po = Js '({ stav: __kpt.karta(1).dataset.stav, posun: __kpt.q("main.panel").scrollTop, klavesy: __kptKlavesy })'
    $kl = @($po.klavesy)
    Over 'klávesy došly do stránky jako důvěryhodné (isTrusted) — je co ověřovat' ((($kl | ForEach-Object { $_.k }) -join ',') -eq ' ,Enter,ArrowDown,PageDown,End' -and -not @($kl | Where-Object { -not $_.t })) $kl
    Over 'stráž jim zrušila výchozí akci (preventDefault)' ($kl.Count -gt 0 -and -not @($kl | Where-Object { -not $_.p })) $kl
    $r = (Prikaz 'rezim').v
    Over 'mezerník ani Enter nepřepnuly přepínač s fokusem (ovladač vypnutý, backend: vypnuto)' ($po.stav -eq 'off' -and $r.rezim -eq 'disabled') @{ stav = $po.stav; rezim = $r.rezim }
    Over 'šipka, PageDown, End ani mezerník panel neposunuly' ($po.posun -eq 0) $po.posun

    $null = Js '(() => { __kptKlavesy.length = 0; return true; })()'
    OknuKlavesa 'F5' 'F5' 116
    OknuKlavesa 'r' 'KeyR' 82 2
    OknuKlavesa 'p' 'KeyP' 80 2
    Start-Sleep -Milliseconds 1000
    $po = Js '({ znacka: window.__kptZnacka === 1, tisk: window.__kptTisk === true, klavesy: window.__kptKlavesy || [] })'
    $kl = @($po.klavesy)
    Over 'F5 ani Ctrl+R stránku neobnovily (okno je aplikace, ne prohlížeč)' ($po.znacka) $po
    Over 'Ctrl+P neotevřelo tisk' ($po.znacka -and -not $po.tisk) $po
    Over 'F5, Ctrl+R a Ctrl+P došly do stránky a stráž je zrušila' ((($kl | ForEach-Object { $_.k }) -join ',') -eq 'F5,r,p' -and -not @($kl | Where-Object { -not $_.p -or -not $_.t })) $kl

    # Kontrola opačným směrem: Tab (navigace klávesnicí) stráž propustí.
    $null = Js '(() => { __kptKlavesy.length = 0; return true; })()'
    OknuKlavesa 'Tab' 'Tab' 9
    Start-Sleep -Milliseconds 200
    $kl = @(Js '__kptKlavesy')
    Over 'Tab stráž propustí (navigace klávesnicí funguje)' ($kl.Count -eq 1 -and $kl[0].k -eq 'Tab' -and -not $kl[0].p) $kl

    # Úklid: stisk myši vrátí stráž do výchozího stavu (fokus už není
    # z Tabu), fokus pryč, okno zpět na 440 × 620.
    $null = StiskMysi 'main.panel'
    $null = Js '(() => { if (document.activeElement) document.activeElement.blur(); return true; })()'
    $null = $script:cdp.Volej('Emulation.setFocusEmulationEnabled', '{"enabled":false}', 10000)
    $null = $script:cdp.Volej('Emulation.clearDeviceMetricsOverride', '{}', 10000)
    $null = Cekej 'innerHeight > 500'
}

function ScenarPrirazeni {
    Krok '4/11  Přiřazení F24 k A ovladače 2'
    $null = Klik 'section.karta[data-pad="2"] .rozbal'
    Over 'klik na hlavičku rozbalí kartu 2' (Cekej '__kpt.karta(2).dataset.rozbalena === "true" && !!__kpt.cep(2, "a")')
    $null = Klik 'section.karta[data-pad="2"] [data-vstup="a"] .telo'
    Over 'klik na čepičku A → přiřazování (data-rezim=binding)' (Cekej '__kpt.app().dataset.rezim === "binding" && __kpt.cep(2, "a").hasAttribute("data-cil")')
    $r = (Prikaz 'rezim').v
    Over 'rezim.cil = ovladač 2 (index 1), vstup a' ($r.cil -and $r.cil.pad -eq 1 -and $r.cil.vstup -eq 'a') $r
    Over 'nápověda „Stiskni klávesu · Esc zruší“' (Cekej '(__kpt.napoveda(2) || "").startsWith("Stiskni klávesu · Esc zruší")') (Js '__kpt.napoveda(2)')
    Snimek '03-prirazovani'
    Klavesa 0x76 $false $true
    Over 'F24 → oznámení „uloženo“' (Cekej '__kpt.app().dataset.oznameni === "ulozeno"')
    Klavesa 0x76 $false $false
    Over 'po uložení konec přiřazování' (Cekej '__kpt.app().dataset.rezim === "disabled" && !__kpt.cep(2, "a").hasAttribute("data-cil")')
    $k = Klavesy
    $v = Vazba $k 0x76
    Over 'F24 je na A ovladače 2 (backend)' ($v -and $v.pad -eq 1 -and $v.vstup -eq 'a') $v
    # Windows F13–F24 nepojmenují; dřív tu bylo „#76".
    Over 'F24 má název „F24“ i na čepičce' ($v -and $v.klavesa.nazev -eq 'F24' -and $v.klavesa.kratky -eq 'F24') $(if ($v) { $v.klavesa })
    $kr = if ($v) { $v.klavesa.kratky } else { '?' }
    Over ('čepička A ovladače 2 ukazuje „' + $kr + '“') (Cekej ('__kpt.nazev(2, "a") === ' + (ConvertTo-Json -InputObject $kr -Compress))) (Js '__kpt.nazev(2, "a")')
    Over 'karta 2 už nemá řádek „Klikni na vstup…“' (Cekej '__kpt.napoveda(2) !== "Klikni na vstup a stiskni klávesu"')
}

function ScenarPresun {
    Krok '5/11  Přesun F24 na ovladač 1 a „Zpět“'
    $null = Klik 'section.karta[data-pad="1"] .rozbal'
    $null = Cekej '__kpt.karta(1).dataset.rozbalena === "true" && !!__kpt.cep(1, "b")'
    $rev0 = (Klavesy).rev
    $null = Klik 'section.karta[data-pad="1"] [data-vstup="b"] .telo'
    Over 'klik na B ovladače 1 → přiřazování' (Cekej '__kpt.app().dataset.rezim === "binding" && __kpt.cep(1, "b").hasAttribute("data-cil")')
    Stisk 0x76
    Over 'F24 → uloženo na B ovladače 1' (Cekej '__kpt.app().dataset.oznameni === "ulozeno" && __kpt.app().dataset.rezim === "disabled"')
    Over 'nápověda „Přesunuto z ovladače 2 · A“ se „Zpět“' (Cekej '(__kpt.napoveda(1) || "").startsWith("Přesunuto z ovladače 2 · A") && !!__kpt.q("section.karta[data-pad=\"1\"] .napoveda .zpet")') (Js '__kpt.napoveda(1)')
    $k = Klavesy
    $f24 = Vazba $k 0x76
    Over 'F24 na B ovladače 1, C už ne (klik = nahradit), A ovladače 2 prázdné' ($f24.pad -eq 0 -and $f24.vstup -eq 'b' -and -not (Vazba $k 0x2E) -and -not @($k.vazby | Where-Object { $_.pad -eq 1 })) @{ f24 = $f24; c = (Vazba $k 0x2E) }
    Snimek '04-presun'
    $null = Klik 'section.karta[data-pad="1"] .napoveda .zpet'
    $ok = CekejNa { $k = Klavesy; $k.rev -gt $rev0 + 1 -and (Vazba $k 0x76).pad -eq 1 -and (Vazba $k 0x2E).vstup -eq 'b' }
    $k = Klavesy
    Over '„Zpět“ vrátí F24 na A ovladače 2 a C na B ovladače 1' $ok @{ f24 = (Vazba $k 0x76); c = (Vazba $k 0x2E) }
    Over 'po „Zpět“ zmizí řádek se „Zpět“' (Cekej '!__kpt.q("section.karta[data-pad=\"1\"] .napoveda .zpet")')
}

function ScenarWin {
    Krok '7/11  Win a AltGr při přiřazování'
    $rev0 = (Klavesy).rev
    $null = Klik 'section.karta[data-pad="1"] [data-vstup="x"] .telo'
    Over 'klik na X ovladače 1 → přiřazování' (Cekej '__kpt.app().dataset.rezim === "binding" && __kpt.cep(1, "x").hasAttribute("data-cil")')
    Klavesa 0x5B $true $true
    Over 'Win (0x5B + E0) → oznámení „odmítnuto“' (Cekej '__kpt.app().dataset.oznameni === "odmitnuto"')
    Over 'nápověda „Win patří Windows“' (Cekej '__kpt.napoveda(1) === "Win patří Windows"') (Js '__kpt.napoveda(1)')
    Over 'přiřazování běží dál' (Js '__kpt.app().dataset.rezim === "binding"')
    Snimek '05-win'
    Klavesa 0x5B $true $false
    Stisk 0x01
    Over 'Esc přiřazování zruší' (Cekej '__kpt.app().dataset.oznameni === "zruseno" && __kpt.app().dataset.rezim === "disabled"')
    Over 'Win ani Esc nic nepřiřadily' ((Klavesy).rev -eq $rev0)
    # Zkratka pauzy při přiřazování se odmítne a přiřazování čeká dál.
    $null = Klik 'section.karta[data-pad="1"] [data-vstup="x"] .telo'
    $null = Cekej '__kpt.app().dataset.rezim === "binding"'
    Stisk 0x46
    Over 'Scroll Lock při přiřazování → „Scroll Lock je pauza“, přiřazuje se dál' ((Cekej '/ je pauza$/.test(__kpt.napoveda(1) || "")') -and (Js '__kpt.app().dataset.rezim === "binding"')) (Js '__kpt.napoveda(1)')
    Stisk 0x01
    $null = Cekej '__kpt.app().dataset.rezim === "disabled"'

    # AltGr na české klávesnici, jak ho posílají Windows: falešný levý Ctrl
    # (scan 0x21D), pravý Alt (E0 0x38), puštění v tomtéž pořadí. Odmítne
    # se celý — pravý Alt se pak nesmí přiřadit ťuknutím.
    $rev0 = (Klavesy).rev
    $null = Klik 'section.karta[data-pad="1"] [data-vstup="x"] .telo'
    $null = Cekej '__kpt.app().dataset.rezim === "binding"'
    Klavesa 0x21D $false $true
    Over 'AltGr → „Tuhle klávesu nejde použít“' (Cekej '__kpt.napoveda(1) === "Tuhle klávesu nejde použít"') (Js '__kpt.napoveda(1)')
    Klavesa 0x38 $true $true
    Klavesa 0x21D $false $false
    Klavesa 0x38 $true $false
    Start-Sleep -Milliseconds 400
    $st = Js '({ rezim: __kpt.app().dataset.rezim, oznameni: __kpt.app().dataset.oznameni, napoveda: __kpt.napoveda(1) })'
    Over 'AltGr nic nepřiřadil (ani pravý Alt ťuknutím), přiřazuje se dál' ((Klavesy).rev -eq $rev0 -and $st.rezim -eq 'binding' -and $st.oznameni -eq 'odmitnuto') $st
    Stisk 0x01
    Over 'Esc po AltGr přiřazování zruší' (Cekej '__kpt.app().dataset.rezim === "disabled"')
}

function ScenarUpravy {
    Krok '6/11  Přidat, vyprázdnit, výchozí klávesy, odebrat ovladač, klik jinam, 10 s'
    $y = 'section.karta[data-pad="1"] [data-vstup="y"]'
    $vazbyY = { param($k) @($k.vazby | Where-Object { $_.pad -eq 0 -and $_.vstup -eq 'y' }) }

    # + po najetí myší: přidá klávesu k těm, co vstup má.
    $null = Klik "$y .plus"
    Over '+ na Y → přiřazování „Přidej klávesu · Esc zruší“' (Cekej '__kpt.app().dataset.rezim === "binding" && (__kpt.napoveda(1) || "").startsWith("Přidej klávesu · Esc zruší")') (Js '__kpt.napoveda(1)')
    Stisk 0x6E
    Over 'F23 → uloženo, konec přiřazování' (Cekej '__kpt.app().dataset.oznameni === "ulozeno" && __kpt.app().dataset.rezim === "disabled"')
    $k = Klavesy
    $yk = & $vazbyY $k
    Over 'Y ovladače 1 má R i F23 (přidat nebere původní)' ((($yk | ForEach-Object { $_.klavesa.nazev }) -join ',') -eq 'R,F23') $yk
    Over 'čepička Y: první klávesa R a „+1“, bublina „také F23“' (Cekej ('__kpt.nazev(1, "y") === "R" && __kpt.text(__kpt.q(' + (ConvertTo-Json "$y .vic" -Compress) + ')) === "+1" && __kpt.q(' + (ConvertTo-Json "$y .telo" -Compress) + ').title.includes("také F23")')) (Js ('__kpt.q(' + (ConvertTo-Json "$y .telo" -Compress) + ').title'))

    # × vyprázdní vstup, „Zpět" ho vrátí.
    $null = Klik "$y .krizek"
    Over '× vyprázdní Y: čárkovaná, „Vyprázdněno: Y“ se „Zpět“' (Cekej ('__kpt.cep(1, "y").hasAttribute("data-prazdna") && (__kpt.napoveda(1) || "").startsWith("Vyprázdněno: Y") && !!__kpt.q("section.karta[data-pad=\"1\"] .napoveda .zpet")')) (Js '__kpt.napoveda(1)')
    Over 'backend: Y bez kláves' (CekejNa { (& $vazbyY (Klavesy)).Count -eq 0 })
    $null = Klik 'section.karta[data-pad="1"] .napoveda .zpet'
    Over '„Zpět“ vrátí R i F23' (CekejNa { (& $vazbyY (Klavesy)).Count -eq 2 }) (& $vazbyY (Klavesy))
    # Pravý klik vyprázdní taky.
    $null = Cekej '!__kpt.cep(1, "y").hasAttribute("data-prazdna")'
    $null = PravyKlik "$y .telo"
    Over 'pravý klik vyprázdní Y' ((Cekej '__kpt.cep(1, "y").hasAttribute("data-prazdna")') -and (CekejNa { (& $vazbyY (Klavesy)).Count -eq 0 }))

    # ↺ výchozí klávesy ovladače 1: dva kliky do 3 s.
    $vychozi = 'section.karta[data-pad="1"] .ikony button[aria-label="Výchozí klávesy"]'
    $vj = ConvertTo-Json $vychozi -Compress
    $null = Klik $vychozi
    Over '↺ první klik jen potvrzuje („Znovu = potvrdit“), nic nemění' ((Cekej "__kpt.q($vj).classList.contains('potvrdit') && __kpt.q($vj).title === 'Znovu = potvrdit'") -and (Js '__kpt.cep(1, "y").hasAttribute("data-prazdna")'))
    $null = Klik $vychozi
    Over '↺ druhý klik vrátí výchozí klávesy (Y = R)' (Cekej '!__kpt.cep(1, "y").hasAttribute("data-prazdna") && __kpt.nazev(1, "y") === "R"')
    $k = Klavesy
    $f24 = Vazba $k 0x76
    Over 'výchozí: ovladač 1 má 24 vazeb, F24 zůstala ovladači 2 (OQ 49), F23 pryč' (@($k.vazby | Where-Object { $_.pad -eq 0 }).Count -eq 24 -and $f24.pad -eq 1 -and -not (Vazba $k 0x6E)) @{ pad1 = @($k.vazby | Where-Object { $_.pad -eq 0 }).Count; f24 = $f24 }

    # 🗑 odebere vypnutý ovladač 2–4: dva kliky.
    $null = Klik 'section.karta[data-pad="4"] .rozbal'
    $null = Cekej '__kpt.karta(4).dataset.rozbalena === "true"'
    $kos = ConvertTo-Json 'section.karta[data-pad="4"] .ikony button[aria-label="Odebrat ovladač"]' -Compress
    $null = Js "__kpt.klik($kos)"
    Over '🗑 první klik jen potvrzuje, karta 4 zůstává' ((Cekej "!!__kpt.q($kos) && __kpt.q($kos).classList.contains('potvrdit')") -and (Js '!!__kpt.karta(4)'))
    $null = Js "__kpt.klik($kos)"
    Over '🗑 druhý klik odebere ovladač 4 (karta zmizí, „+ Ovladač“ nabízí 4)' (Cekej '!__kpt.karta(4) && !!__kpt.q("[data-pridat=\"4\"]")') (Js '__kpt.karty()')

    # Klik jinam (stisk myši mimo čepičky) přiřazování zruší.
    $null = Klik 'section.karta[data-pad="1"] .rozbal'
    $null = Cekej '__kpt.karta(1).dataset.rozbalena === "true" && !!__kpt.cep(1, "x")'
    $rev0 = (Klavesy).rev
    $null = Klik 'section.karta[data-pad="1"] [data-vstup="x"] .telo'
    $null = Cekej '__kpt.app().dataset.rezim === "binding"'
    $null = StiskMysi 'main.panel'
    Over 'klik jinam přiřazování zruší' (Cekej '__kpt.app().dataset.rezim === "disabled" && __kpt.app().dataset.oznameni === "zruseno"')

    # 10 s bez klávesy: přiřazování skončí samo (limit jádra).
    $null = Klik 'section.karta[data-pad="1"] [data-vstup="x"] .telo'
    Over 'přiřazování běží a ubývá čas (proužek, nebo „ještě N s“ s Omezit pohyb)' (Cekej '__kpt.app().dataset.rezim === "binding" && (!!__kpt.q("section.karta[data-pad=\"1\"] .odpocet") || /ještě \d+ s/.test(__kpt.napoveda(1) || ""))')
    $t0 = [Diagnostics.Stopwatch]::StartNew()
    $konec = Cekej '__kpt.app().dataset.rezim === "disabled"' 14000
    $s = $t0.Elapsed.TotalSeconds
    Over ('bez klávesy se přiřazování za 10 s samo zruší (za {0:N1} s)' -f $s) ($konec -and $s -ge 8.5 -and $s -lt 12.5) $s
    Over 'klik jinam ani limit nic nepřiřadily' ((Klavesy).rev -eq $rev0)
}

function ScenarMinimalizace {
    Krok '10/11  Minimalizace okna během přiřazování'
    $null = Klik 'section.karta[data-pad="1"] .rozbal'
    $null = Cekej '__kpt.karta(1).dataset.rozbalena === "true" && !!__kpt.cep(1, "x")'
    $rev0 = (Klavesy).rev
    $null = Klik 'section.karta[data-pad="1"] [data-vstup="x"] .telo'
    Over 'přiřazování běží' (Cekej '__kpt.app().dataset.rezim === "binding"')
    # Tlačítko titulku (element.click(), žádný stisk myši = žádný „klik
    # jinam"): zruší ho až minimalizace (WM_SIZE → okno není vidět).
    $null = Klik 'button[aria-label="Minimalizovat"]'
    $ok = CekejNa { $r = (Prikaz 'rezim').v; $r.rezim -eq 'disabled' -and -not $r.cil } 5000
    Over 'minimalizace přiřazování zruší (backend: vypnuto, bez cíle)' $ok (Prikaz 'rezim').v
    Over 'a nic se nepřiřadí' ((Klavesy).rev -eq $rev0)
    # Minimalizované okno nic neukazuje: živý stav je prázdný.
    Klavesa 0x11 $false $true
    Start-Sleep -Milliseconds 300
    $z = (Prikaz 'zive').v
    Over 'W při minimalizovaném okně nic nerozsvítí' (-not @($z.pady | Where-Object { $_.drzi -ne 0 })) $z
    Klavesa 0x11 $false $false
    # Obnovit okno test neumí (oprávnění unminimize okno nemá) — další
    # scénář začíná restartem.
}

function ScenarZive {
    Krok '8/11  Živé svícení, simulovaný ovladač'
    Klavesa 0x11 $false $true
    Over 'W dolů → čepička ↑ levé páčky obrysem (data-sviti=nahled)' (Cekej '__kpt.cep(1, "ls_up").dataset.sviti === "nahled"') (Js '__kpt.sviti()')
    Klavesa 0x1E $false $true
    Klavesa 0x20 $false $true
    Over 'A+D → svítí obě (náhled)' (Cekej '__kpt.cep(1, "ls_left").dataset.sviti === "nahled" && __kpt.cep(1, "ls_right").dataset.sviti === "nahled"') (Js '__kpt.sviti()')
    Over 'hlavička páčky se posune (W+D = vpravo nahoru)' (Cekej '/^[1-9][0-9.]*px -[1-9]/.test(__kpt.q("section.karta[data-pad=\"1\"] [data-vstup=\"l3\"] .telo").style.translate)') (Js '__kpt.q("section.karta[data-pad=\"1\"] [data-vstup=\"l3\"] .telo").style.translate')
    Snimek '06-nahled-wad'
    foreach ($s in 0x11, 0x1E, 0x20) { Klavesa $s $false $false }
    Over 'vše puštěno → nic nesvítí' (Cekej '__kpt.sviti().length === 0') (Js '__kpt.sviti()')
    Klavesa 0x2C $false $true
    Start-Sleep -Milliseconds 300
    $z = (Prikaz 'zive').v
    Over 'nenamapovaná klávesa nic nerozsvítí' ((@(Js '__kpt.sviti()').Count -eq 0) -and -not @($z.pady | Where-Object { $_.drzi -ne 0 })) $z
    Klavesa 0x2C $false $false

    Stisk 0x46
    Over 'Scroll Lock bez ovladače → „Nejdřív zapni ovladač“' ((Cekej '__kpt.app().dataset.oznameni === "zapni_ovladac"') -and (Cekej '__kpt.napoveda(1) === "Nejdřív zapni ovladač"')) (Js '__kpt.napoveda(1)')
    $t0 = [Diagnostics.Stopwatch]::StartNew()
    $zmizela = Cekej '__kpt.napoveda(1) === ""' 6000
    Over ('rada sama zmizí (limit 2,5 s; za {0:N1} s)' -f $t0.Elapsed.TotalSeconds) ($zmizela -and $t0.Elapsed.TotalSeconds -lt 4) (Js '__kpt.napoveda(1)')
    # Znovu, a teď radu poslechnout: zapnout ovladač, dokud ještě svítí.
    Stisk 0x46
    $null = Cekej '__kpt.napoveda(1) === "Nejdřív zapni ovladač"'

    $null = Klik 'section.karta[data-pad="1"] button[role="switch"]'
    Over 'přepínač zapne simulovaný ovladač 1 (Zapnutý, hraje)' (Cekej '__kpt.karta(1).dataset.stav === "on" && __kpt.app().dataset.rezim === "capturing" && __kpt.text(__kpt.q("section.karta[data-pad=\"1\"] .veta")) === "Zapnutý"' 10000) (Js '[__kpt.karta(1).dataset.stav, __kpt.app().dataset.rezim]')
    Over 'se zapnutým ovladačem rada „Nejdřív zapni ovladač“ zmizí' (Cekej '__kpt.napoveda(1) !== "Nejdřív zapni ovladač"') (Js '__kpt.napoveda(1)')
    Klavesa 0x11 $false $true
    Over 'W při hře → ↑ svítí plně (data-sviti=hra)' (Cekej '__kpt.cep(1, "ls_up").dataset.sviti === "hra"') (Js '__kpt.sviti()')
    Klavesa 0x1E $false $true
    Klavesa 0x20 $false $true
    Over 'A+D při hře → D plně (vyhrává poslední), A obrysem' (Cekej '__kpt.cep(1, "ls_right").dataset.sviti === "hra" && __kpt.cep(1, "ls_left").dataset.sviti === "nahled"') (Js '__kpt.sviti()')
    $z = (Prikaz 'zive').v
    Over 'zive: drží ↑ ← →, hra jen ↑ →, páčka (1, 1)' ($z.pady[0].l[0] -eq 1 -and $z.pady[0].l[1] -eq 1 -and $z.pady[0].hra -ne $z.pady[0].drzi) $z.pady[0]
    Snimek '07-hra-wad'
    foreach ($s in 0x11, 0x1E, 0x20) { Klavesa $s $false $false }
    Over 'puštěno → nic nesvítí' (Cekej '__kpt.sviti().length === 0') (Js '__kpt.sviti()')
    Stisk 0x46
    Over 'Scroll Lock při hře → Pozastaveno' (Cekej '__kpt.app().dataset.rezim === "paused" && __kpt.text(__kpt.q("section.karta[data-pad=\"1\"] .veta")) === "Pozastaveno"')
    Snimek '08-pozastaveno'
    Stisk 0x46
    Over 'Scroll Lock znovu → hraje' (Cekej '__kpt.app().dataset.rezim === "capturing"')
    $null = Klik 'section.karta[data-pad="1"] button[role="switch"]'
    Over 'přepínač ovladač vypne' (Cekej '__kpt.karta(1).dataset.stav === "off" && __kpt.app().dataset.rezim === "disabled"' 10000)
}

function ScenarSirka {
    Krok '9/11  Rozvržení 380 px'
    foreach ($v in @(@(380, 620), @(380, 480))) {
        $null = $script:cdp.Volej('Emulation.setDeviceMetricsOverride', ('{"width":' + $v[0] + ',"height":' + $v[1] + ',"deviceScaleFactor":0,"mobile":false}'), 10000)
        $null = Cekej ('innerWidth === ' + $v[0])
        Start-Sleep -Milliseconds 300
        Pretek ("{0}×{1} px" -f $v[0], $v[1])
        Snimek ('09-{0}x{1}' -f $v[0], $v[1])
    }
    $null = Klik 'section.karta[data-pad="2"] .rozbal'
    $null = Cekej '__kpt.karta(2).dataset.rozbalena === "true"'
    Start-Sleep -Milliseconds 300
    Pretek '380×480 px, karta 2'
    Snimek '10-380x480-karta2'
    $null = $script:cdp.Volej('Emulation.clearDeviceMetricsOverride', '{}', 10000)
    $null = Cekej 'innerWidth > 420'
}

function ScenarRestart {
    Krok '11/11  Restart a poškozená konfigurace'
    $ulozeno = CekejNaKonfiguraci { param($j) @($j.vazby | Where-Object { $_.ovladac -eq 2 -and $_.vstup -eq 'a' -and $_.scan -eq 0x76 }).Count -eq 1 }
    Over 'config.json má F24 u ovladače 2 (zápis do 0,5 s po změně)' $ulozeno
    Ukonci
    Spust
    Over 'po restartu okno načteno' (Cekej '!!__kpt.app() && __kpt.qa("section.karta[data-pad=\"1\"] [data-vstup]").length === 24' 30000)
    $k = Klavesy
    Over 'po restartu F24 zůstala na A ovladače 2' ((Vazba $k 0x76).pad -eq 1 -and (Vazba $k 0x76).vstup -eq 'a' -and $k.konfigurace -eq 'ok') @{ f24 = (Vazba $k 0x76); konfigurace = $k.konfigurace }
    $karty = @(Js '__kpt.karty()')
    Over 'po restartu karty 1 a 2; prázdné 3 a 4 zmizely (OQ 52)' (($karty -join ',') -eq '1,2') $karty
    $null = Klik 'section.karta[data-pad="2"] .rozbal'
    Over 'karta 2 po restartu ukazuje F24 na A' (Cekej ('__kpt.nazev(2, "a") === ' + (ConvertTo-Json -InputObject ((Vazba $k 0x76).klavesa.kratky) -Compress)))
    Snimek '11-po-restartu'

    Ukonci
    $cfg = Join-Path $script:appData 'KeyPad\config.json'
    $zaloha = Join-Path $script:appData 'KeyPad\config.invalid.json'
    $rozbity = "{ `"verze`": 1, `"vazby`": [ rozbité"
    [IO.File]::WriteAllText($cfg, $rozbity, (New-Object Text.UTF8Encoding($false)))
    Spust
    Over 's poškozeným config.json okno načteno' (Cekej '!!__kpt.app() && __kpt.qa("section.karta[data-pad=\"1\"] [data-vstup]").length === 24' 30000)
    Over 'pruh „Klávesy nešly načíst — výchozí“' (Cekej '!!__kpt.q("div.pruh[data-konfigurace=\"obnovena\"]") && __kpt.text(__kpt.q("div.pruh[data-sbernice]")).includes("Klávesy nešly načíst — výchozí")') (Js '__kpt.text(__kpt.q("div.pruh[data-sbernice]"))')
    Over 'bublina pruhu ukazuje zálohu' (Js '__kpt.qa("div.pruh[data-sbernice] [title]").some((e) => e.title.includes("config.invalid.json"))') (Js '__kpt.qa("div.pruh[data-sbernice] [title]").map((e) => e.title)')
    # Spec 1.7: chyby nevalidního souboru v bublině (ne jen „jsou v logu").
    Over 'bublina pruhu ukazuje i chybu souboru („řádek 1: …“)' (Js '__kpt.qa("div.pruh[data-sbernice] [title]").some((e) => e.title.includes("Co je v souboru špatně:") && /· řádek 1: /.test(e.title))') (Js '__kpt.qa("div.pruh[data-sbernice] [title]").map((e) => e.title)')
    Over 'config.invalid.json má původní obsah' ((Test-Path $zaloha) -and ([IO.File]::ReadAllText($zaloha) -eq $rozbity))
    Over 'config.json nevznikl (až první změnou, OQ 46)' (-not (Test-Path $cfg))
    $k = Klavesy
    Over 'platí výchozí klávesy (24 vazeb ovladače 1), stav „obnovena“' ($k.vazby.Count -eq 24 -and -not @($k.vazby | Where-Object { $_.pad -ne 0 }) -and $k.konfigurace -eq 'obnovena' -and $k.zaloha) @{ n = $k.vazby.Count; konfigurace = $k.konfigurace; zaloha = $k.zaloha }
    Pretek '440 px s pruhem'
    Snimek '12-konfigurace-obnovena'
}

# ── Běh ────────────────────────────────────────────────────────────
$vysledek = 2
$script:beh = $null
$script:plocha = $null
$hPlochy = [IntPtr]::Zero
try {
    if (-not $BezBuildu) {
        Krok 'Build: tools\tauri.ps1 build --debug --no-bundle'
        $psExe = Join-Path $PSHOME 'powershell.exe'
        # CLI píše průběh na stderr; s 'Stop' by z prvního řádku byla výjimka
        # (stejný důvod jako v tauri.ps1). O úspěchu rozhoduje návratový kód.
        $kod = & {
            $ErrorActionPreference = 'Continue'
            & $psExe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'tools\tauri.ps1') build --debug --no-bundle 2>&1 |
                ForEach-Object { "$_" } | Where-Object { $_ -match 'Finished|error|Built application' } |
                ForEach-Object { Write-Host "  $_" -ForegroundColor DarkGray }
            $LASTEXITCODE
        }
        if ($kod -ne 0) { throw "build selhal ($kod)" }
    }
    if (-not (Test-Path $exeZdroj)) { throw "chybí $exeZdroj — nejdřív build (bez -BezBuildu)" }
    # Holý `cargo build` by dal exe s adresou vývojového serveru místo UI.
    $text = [Text.Encoding]::GetEncoding(28591).GetString([IO.File]::ReadAllBytes($exeZdroj))
    if ($text.IndexOf('assets/index-', [StringComparison]::Ordinal) -lt 0) { throw "$exeZdroj nemá vestavěné UI — postav ho přes tools\tauri.ps1 build --debug --no-bundle" }

    # Pracovní složka běhu: kopie exe (log `keypad.log` jde vedle ní),
    # APPDATA a LOCALAPPDATA jen pro test.
    New-Item -ItemType Directory -Force $Prac | Out-Null
    $script:beh = Join-Path (Resolve-Path $Prac).Path ('beh-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + $PID)
    $script:appData = Join-Path $script:beh 'appdata'
    $script:localApp = Join-Path $script:beh 'localappdata'
    New-Item -ItemType Directory $script:beh, $script:appData, $script:localApp | Out-Null
    $script:exe = Join-Path $script:beh 'KeyPad.exe'
    Copy-Item $exeZdroj $script:exe
    if (-not $Snimky) { $Snimky = Join-Path $script:beh 'snimky' }
    New-Item -ItemType Directory -Force $Snimky | Out-Null
    $script:snimky = (Resolve-Path $Snimky).Path

    # Data okna nainstalovaného KeyPadu (známá složka, ne proměnná): test
    # do nich nesmí sáhnout. Bez ladicího portu je KeyPad nikdy nenapíše,
    # takže nový DevToolsActivePort by znamenal únik testu.
    $script:portVlastnika = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) "$identifier\EBWebView\DevToolsActivePort"
    $script:portVlastnikaPred = if (Test-Path $script:portVlastnika) { (Get-Item $script:portVlastnika).LastWriteTimeUtc } else { $null }

    Krok 'Plocha a bezpečnost'
    $script:plocha = "KeyPadTest$PID"
    $hPlochy = [KeyPadOknoTest.Plocha]::Vytvor($script:plocha)
    Write-Host "  skrytá plocha WinSta0\$($script:plocha), běh $($script:beh)" -ForegroundColor DarkGray
    $vlastnik = [KeyPadOknoTest.Plocha]::OknoNaMePlose("$identifier-sic", "$identifier-siw")
    Write-Host ('  okno single-instance KeyPadu na ploše vlastníka: ' + $(if ($vlastnik) { 'ano (KeyPad běží)' } else { 'ne' })) -ForegroundColor DarkGray
    # Ověřuje se znovu před každým spuštěním (Spust), tady jen do výsledků.
    OverSingleInstance
    Over 'z testovací plochy okno KeyPadu vlastníka vidět není (single-instance ho nenajde)' $true

    Spust
    # Výjimka ve scénáři (chyba JS, vypršený dotaz) je selhaná kontrola,
    # ne konec běhu: další scénáře ukážou, jestli je okno jinak v pořádku.
    # Bezpečnostní stop a ztracené spojení končí hned.
    $scenare = 'ScenarStart', 'ScenarKarty', 'ScenarKlavesyOkna', 'ScenarPrirazeni', 'ScenarPresun', 'ScenarUpravy',
        'ScenarWin', 'ScenarZive', 'ScenarSirka', 'ScenarMinimalizace', 'ScenarRestart'
    foreach ($s in $scenare) {
        try {
            & $s
        } catch {
            if ($script:stop -or -not $script:proces -or $script:proces.Ceka(0)) { throw }
            Over "$s doběhl bez výjimky" $false $_.Exception.Message
        }
    }
    $vysledek = if ($script:chyb -eq 0) { 0 } else { 1 }
} catch {
    Write-Host ''
    Write-Host "Harness nedoběhl: $($_.Exception.Message)" -ForegroundColor Red
    Write-Host $_.ScriptStackTrace -ForegroundColor DarkGray
    $vysledek = if ($script:stop) { 3 } else { 2 }
} finally {
    Krok 'Úklid'
    try { Ukonci } catch { Write-Host "  ukončení: $($_.Exception.Message)" -ForegroundColor Red }
    if ($script:beh) {
        # Procesy WebView2 končí s KeyPadem (taskkill /T); kdyby některý
        # zůstal, je to náš (data v pracovní složce běhu).
        $null = CekejNa { @(Zbytky).Count -eq 0 } 10000
        foreach ($p in @(Zbytky)) {
            Write-Host "  zbyl proces $($p.ProcessId) $($p.Name) — taskkill" -ForegroundColor Yellow
            Taskkill @('/F', '/PID', "$($p.ProcessId)")
        }
        $zbyle = @(Zbytky)
        Over 'nezůstal žádný proces testovací instance (KeyPad, WebView2)' ($zbyle.Count -eq 0) @($zbyle | ForEach-Object { "$($_.ProcessId) $($_.Name)" })
        $po = if (Test-Path $script:portVlastnika) { (Get-Item $script:portVlastnika).LastWriteTimeUtc } else { $null }
        Over 'data okna nainstalovaného KeyPadu nedotčená (WebView2 jen v pracovní složce)' ($po -eq $script:portVlastnikaPred) @{ pred = $script:portVlastnikaPred; po = $po }
        $log = Join-Path $script:beh 'keypad.log'
        if (Test-Path $log) {
            $radky = @(Get-Content $log -Encoding UTF8)
            $paniky = @($radky | Where-Object { $_ -match 'PANIKA' })
            Over 'v logu není panika' ($paniky.Count -eq 0) $paniky
            # Spec 1.8: zkratky prohlížeče (F5, Ctrl+R, Ctrl+P) vypíná backend
            # ve WebView2 — z okna to poznat nejde (stráž klávesy ruší sama),
            # proto log (ladicí build píše i úroveň debug).
            $zkratky = @($radky | Where-Object { $_ -match 'WebView2: zkratky prohlížeče vypnuté' })
            Over 'WebView2: zkratky prohlížeče vypnuté (log)' ($zkratky.Count -ge 1) $zkratky.Count
            $chyby = @($radky | Where-Object { $_ -cmatch '\bERROR\b' })
            if ($chyby.Count) {
                Write-Host "  ERROR v logu ($($chyby.Count)):" -ForegroundColor DarkGray
                $chyby | Select-Object -Last 8 | ForEach-Object { Write-Host "    $_" -ForegroundColor DarkGray }
            }
        }
    }
    if ($hPlochy -ne [IntPtr]::Zero) {
        $null = [KeyPadOknoTest.Plocha]::Zavri($hPlochy)
        $pryc = CekejNa { -not [KeyPadOknoTest.Plocha]::Existuje($script:plocha) } 10000
        Over "skrytá plocha $($script:plocha) zanikla" $pryc
    }
    if ($script:chyb -gt 0 -and $vysledek -eq 0) { $vysledek = 1 }
    Write-Host ''
    if ($vysledek -eq 0) {
        Write-Host "VÝSLEDEK: všech $($script:kontrol) kontrol prošlo" -ForegroundColor Green
        if (-not $Nechat -and $script:beh -and $script:snimky -notlike "$($script:beh)*") {
            # Data WebView2 může systém ještě chvíli držet — pár pokusů.
            for ($i = 0; $i -lt 10 -and (Test-Path $script:beh); $i++) {
                Remove-Item -LiteralPath $script:beh -Recurse -Force -ErrorAction SilentlyContinue
                if (Test-Path $script:beh) { Start-Sleep -Milliseconds 500 }
            }
        }
    } elseif ($vysledek -eq 3) {
        Write-Host 'VÝSLEDEK: zastaveno kvůli bezpečnosti — testovací instance se nespustila' -ForegroundColor Red
    } else {
        Write-Host "VÝSLEDEK: $($script:chyb) z $($script:kontrol) kontrol selhalo" -ForegroundColor Red
        $script:selhane | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
        if ($script:beh) { Write-Host "  log a data běhu: $($script:beh)" -ForegroundColor DarkGray }
    }
    if ($script:snimky) { Write-Host "  snímky: $($script:snimky)" -ForegroundColor DarkGray }
}
exit $vysledek

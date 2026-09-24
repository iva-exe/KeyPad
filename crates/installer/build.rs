//! Ikona, manifest a informace o verzi instalátoru.
//!
//! Manifest s `asInvoker` je důležitý dvakrát: KeyPad se instaluje do
//! profilu uživatele a práva správce nepotřebuje, a zároveň výslovná
//! úroveň vypíná heuristiku Windows, která by (u 32bitového buildu)
//! program se „Setup" ve jménu sama spustila přes UAC (viz komentář
//! v `installer.manifest`).
//!
//! Kompletní blok VERSIONINFO tu není kosmetika. Heuristiky antivirů
//! berou „malý .exe bez jména výrobce, bez popisu a bez verze, který
//! stahuje a spouští další .exe" jako typický profil downloaderu.
//! Vyplněná metadata ten profil rozbíjejí — a hlavně jsou pravdivá:
//! uživatel ve vlastnostech souboru uvidí, co to je a od koho.
//!
//! Na obrazovku „Windows ochránily váš počítač" (SmartScreen) to samo
//! nestačí. Ta se ukazuje u každého programu, který nemá podpis
//! a reputaci; jediné, co ji spolehlivě odstraní, je podepsat binárku
//! certifikátem pro podpis kódu.

use std::path::Path;

fn main() {
    if !cfg!(target_os = "windows") {
        return;
    }
    // Ikonu sdílí instalátor s aplikací — ta ji má v src-tauri/icons.
    let ico = "../../src-tauri/icons/icon.ico";
    // Na chybějící soubor cargo build skript pouští pokaždé znovu; to
    // tu chceme — jakmile ikona vznikne, příští build ji zabuduje sám.
    println!("cargo:rerun-if-changed={ico}");
    println!("cargo:rerun-if-changed=installer.manifest");

    // Systémové DLL (dwmapi, winhttp…) jen ze System32. Instalátor se
    // spouští ze Stažených souborů a výchozí pořadí hledání bere NEJDŘÍV
    // složku .exe — cizí `dwmapi.dll`, kterou tam podstrčí kdejaká
    // stránka, by se načetla a spustila ještě před `main()` (ověřeno:
    // podvržená DLL ukončila proces dřív, než instalátor cokoli udělal).
    // dwmapi ani winhttp nejsou v KnownDLLs, takže je nic jiného nechrání.
    // 0x800 = LOAD_LIBRARY_SEARCH_SYSTEM32 pro statické importy (Windows 10
    // 1607+; starší systém příznak ignoruje a chová se jako dřív). Plné
    // cesty k taskkill/cmd v proc.rs řeší totéž pro spouštěné programy.
    println!("cargo:rustc-link-arg-bin=KeyPadSetup=/DEPENDENTLOADFLAG:0x800");

    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".into());

    let mut res = winresource::WindowsResource::new();
    // Ikona se generuje zvlášť (a v čerstvém stromu ještě nemusí být).
    // Instalátor bez ikony je pořád funkční instalátor — build kvůli
    // tomu padat nesmí, jen na to upozorní.
    if Path::new(ico).is_file() {
        res.set_icon(ico);
    } else {
        println!("cargo:warning=ikona {ico} zatím neexistuje — KeyPadSetup.exe bude bez ikony");
    }
    res.set_manifest_file("installer.manifest");
    res.set("FileDescription", "KeyPad — instalace a aktualizace");
    res.set("ProductName", "KeyPad");
    res.set("CompanyName", "KeyPad");
    res.set("InternalName", "KeyPadSetup");
    res.set("OriginalFilename", "KeyPadSetup.exe");
    res.set("LegalCopyright", "© KeyPad");
    res.set(
        "Comments",
        "Nainstaluje a aktualizuje KeyPad (klávesnice jako Xbox ovladač) v profilu uživatele, bez práv správce.",
    );
    res.set("FileVersion", &version);
    res.set("ProductVersion", &version);
    // Bez rc.exe se resource nezkompiluje — instalátor pak nemá ikonu
    // ani manifest; build kvůli tomu padat nesmí, ale vědět se to musí.
    if let Err(e) = res.compile() {
        println!("cargo:warning=resource se nepodařilo zabudovat: {e}");
    }
}

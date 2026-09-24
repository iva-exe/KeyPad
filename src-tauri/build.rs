fn main() {
    // tauri-build statický vcruntime zapíná podle STATIC_VCRUNTIME (nastaví
    // ho jen `tauri build`), ale cargu neřekne, že na ní závisí — výstup
    // build skriptu pak zůstane z minula a změna proměnné se neprojeví.
    // Naměřeno, viz README_CZ.md „Závislosti binárky".
    println!("cargo:rerun-if-env-changed=STATIC_VCRUNTIME");
    // Statické importy (dwmapi.dll, winhttp.dll…) jen ze System32.
    // KeyPad.exe jde stáhnout i přímým odkazem na release a spustit ze
    // Stažených souborů — a tam může kdejaká stránka podstrčit vlastní
    // dwmapi.dll, kterou by Windows ve výchozím pořadí hledání načetly
    // dřív než systémovou (nejsou v KnownDLLs). Její kód by běžel ještě
    // před `main`. 0x800 = LOAD_LIBRARY_SEARCH_SYSTEM32 (Windows 10 1607+,
    // starší systémy příznak ignorují). WebView2 se načítá plnou cestou
    // z instalace runtime, toho se příznak netýká.
    println!("cargo:rustc-link-arg-bin=KeyPad=/DEPENDENTLOADFLAG:0x800");
    // Vygeneruje kontext pro `tauri::generate_context!` (konfigurace,
    // oprávnění) a vloží do binárky ikonu z icons/icon.ico.
    tauri_build::build();
}

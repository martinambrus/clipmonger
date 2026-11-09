// Included by the build scripts of the three programs.

/// Link the application icon into a Windows GNU build. `WINDRES` overrides the
/// resource compiler. A release must not lose its icon quietly, so a missing
/// compiler stops the build.
fn embed_windows_icon() {
    let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/windows");
    println!(
        "cargo::rerun-if-changed={}",
        folder.join("clipmonger.rc").display()
    );
    println!(
        "cargo::rerun-if-changed={}",
        folder.join("clipmonger.ico").display()
    );
    println!("cargo::rerun-if-env-changed=WINDRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu") {
        println!("cargo::warning=icon not embedded: only the GNU toolchain is set up for it");
        return;
    }
    let windres = std::env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into());
    let object = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"))
        .join("clipmonger-icon.o");
    let status = std::process::Command::new(&windres)
        .arg("--include-dir")
        .arg(&folder)
        .arg("--input")
        .arg(folder.join("clipmonger.rc"))
        .arg("--output-format=coff")
        .arg("--output")
        .arg(&object)
        .status()
        .unwrap_or_else(|e| panic!("{windres} could not run: {e}"));
    assert!(status.success(), "{windres} failed: {status}");
    println!("cargo::rustc-link-arg-bins={}", object.display());
}

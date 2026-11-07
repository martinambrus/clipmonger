//! Intel macOS: weak-link the system frameworks of the UI binary.
//!
//! The menu-bar icon's menu library references `AppKit` constants that macOS
//! added in 10.13 (About panel options Clipmonger never shows). Strong
//! references stop the app loading on 10.12 ("dyld: Symbol not found");
//! weak ones resolve to null there and are never used. Apple Silicon starts
//! at macOS 11, where every symbol exists.
fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if os == "macos" && arch == "x86_64" {
        for framework in ["AppKit", "Foundation", "CoreGraphics", "QuartzCore"] {
            println!("cargo::rustc-link-arg-bins=-Wl,-weak_framework,{framework}");
        }
    }
}

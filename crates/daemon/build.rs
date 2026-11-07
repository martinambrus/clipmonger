//! `linux_x11` / `linux_wayland`: the X11 and Wayland backends are built only
//! on Unix desktops other than macOS (macOS uses `backend-macos`).
fn main() {
    println!("cargo::rustc-check-cfg=cfg(linux_x11)");
    println!("cargo::rustc-check-cfg=cfg(linux_wayland)");
    let unix = std::env::var("CARGO_CFG_UNIX").is_ok();
    let macos = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "macos");
    let desktop_unix = unix && !macos;
    if desktop_unix && std::env::var_os("CARGO_FEATURE_BACKEND_X11").is_some() {
        println!("cargo::rustc-cfg=linux_x11");
    }
    if desktop_unix && std::env::var_os("CARGO_FEATURE_BACKEND_WAYLAND").is_some() {
        println!("cargo::rustc-cfg=linux_wayland");
    }
}

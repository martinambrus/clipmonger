//! Linux Wayland backend (seg-06): pure logic plus the Wayland session backend.
//!
//! Pure modules (no compositor connection) work everywhere; session code
//! (`monitor`, `transfer`, `paste`, `probe_current`, `wlprobe`) needs a
//! Unix Wayland session. Paste writes the clipboard (seg-06 S7); the daemon
//! then presses the paste chord where the compositor allows it
//! (`fake_input` on `KWin`, `virtual_keyboard` with `toplevel` focus
//! tracking on wlroots-family compositors and COSMIC), else shows a toast. This crate
//! stays `tokio`-free (seg-02 S8); the portal session lives in the daemon.

pub mod caps;
pub mod merge;
pub mod paste_logic;
pub mod portal;
pub mod primary;
pub mod probe;

#[cfg(unix)]
pub mod cosmic_protocol;
#[cfg(unix)]
pub mod cosmic_toplevel;
#[cfg(unix)]
pub mod fake_input;
#[cfg(unix)]
pub mod monitor;
#[cfg(unix)]
pub mod paste;
#[cfg(unix)]
pub mod probe_current;
#[cfg(unix)]
pub mod toplevel;
#[cfg(unix)]
pub mod transfer;
#[cfg(unix)]
pub mod virtual_keyboard;

#[cfg(unix)]
pub use monitor::{WlMonitor, WlMonitorOpts};
#[cfg(unix)]
pub use paste::{PasteOutcome, WlPaste};
#[cfg(unix)]
pub use probe_current::probe_current;
#[cfg(unix)]
pub use transfer::WlTransfer;

/// Block until the compositor named by `$WAYLAND_DISPLAY` closes our
/// connection, as it does when the session ends. The daemon uses this to
/// stop with the session it started in. The connection never speaks the
/// protocol: it only waits for the socket to close.
///
/// # Errors
///
/// [`clipmonger_platform::types::Error::Unavailable`] without a
/// `$WAYLAND_DISPLAY` socket to connect to.
#[cfg(unix)]
pub fn wait_compositor_closed() -> clipmonger_platform::types::Result<()> {
    use clipmonger_platform::types::Error;
    use std::io::Read as _;
    let name = std::env::var_os("WAYLAND_DISPLAY")
        .ok_or_else(|| Error::Unavailable("no WAYLAND_DISPLAY".to_string()))?;
    let name = std::path::PathBuf::from(name);
    let path = if name.is_absolute() {
        name
    } else {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .ok_or_else(|| Error::Unavailable("no XDG_RUNTIME_DIR".to_string()))?;
        std::path::PathBuf::from(runtime).join(name)
    };
    let mut socket = std::os::unix::net::UnixStream::connect(&path)
        .map_err(|e| Error::Unavailable(format!("wayland socket: {e}")))?;
    let mut buf = [0u8; 256];
    // EOF or an error: the compositor is gone.
    while matches!(socket.read(&mut buf), Ok(n) if n > 0) {}
    Ok(())
}

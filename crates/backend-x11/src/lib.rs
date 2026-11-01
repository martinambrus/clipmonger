//! Linux X11 backend (seg-05): pure logic plus the X session backend.
//!
//! Pure modules (no X connection) work everywhere; session modules
//! (`monitor`, `transfer`, `hotkeys`, `paste`, `autostart`, `foreground`)
//! need a Unix X11 session and report `Unavailable` elsewhere.

pub mod caps;
pub mod desktop;
pub mod incr;
pub mod lockmods;
pub mod manager_logic;
pub mod mime;
pub mod primary;

#[cfg(unix)]
pub mod autostart;
#[cfg(unix)]
pub mod foreground;
#[cfg(unix)]
pub mod hotkeys;
#[cfg(unix)]
pub mod monitor;
#[cfg(unix)]
pub mod paste;
#[cfg(unix)]
mod session;
#[cfg(unix)]
pub mod transfer;

#[cfg(unix)]
pub use autostart::X11Autostart;
#[cfg(unix)]
pub use foreground::X11Foreground;
#[cfg(unix)]
pub use hotkeys::X11Hotkeys;
#[cfg(unix)]
pub use monitor::{MonitorOpts, X11Monitor};
#[cfg(unix)]
pub use paste::X11Paste;
#[cfg(unix)]
pub use transfer::X11Transfer;

/// Capability probe for `display` (`None` = `$DISPLAY`): the `XTest`
/// extension decides whether paste is `Full` or `Degraded`.
///
/// The probe argument replaces the earlier zero-arg constructor (the
/// frozen harness API is `caps(display)`).
#[must_use]
pub fn caps(display: Option<&str>) -> clipmonger_platform::types::BackendCaps {
    #[cfg(not(unix))]
    {
        let _ = display;
        caps::caps_for(false, false)
    }
    #[cfg(unix)]
    {
        caps::caps_for(probe_xtest(display), false)
    }
}

/// Whether the `XTest` extension is present on `display` (paste needs it
/// for synthetic input; its absence degrades paste, never the backend).
#[cfg(unix)]
fn probe_xtest(display: Option<&str>) -> bool {
    let Ok((conn, _)) = session::connect(session::display_name(display).as_deref()) else {
        return false;
    };
    xproto_query(&conn)
}

/// Query the XTEST extension presence on `conn`.
#[cfg(unix)]
fn xproto_query(conn: &x11rb14::rust_connection::RustConnection) -> bool {
    x11rb14::protocol::xproto::query_extension(conn, b"XTEST")
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|reply| reply.present)
}

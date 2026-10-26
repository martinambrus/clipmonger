//! Linux Wayland backend (seg-06): pure logic plus the Wayland session backend.
//!
//! Pure modules (no compositor connection) work everywhere; session code
//! (`monitor`, `transfer`, `paste`, `probe_current`, `wlprobe`) needs a
//! Unix Wayland session. v1 paste is the seg-06 S7 fallback (write + toast)
//! only. This crate stays `tokio`-free (seg-02 S8); the portal session
//! lives in the daemon.

pub mod caps;
pub mod merge;
pub mod paste_logic;
pub mod portal;
pub mod primary;
pub mod probe;

#[cfg(unix)]
pub mod monitor;
#[cfg(unix)]
pub mod paste;
#[cfg(unix)]
pub mod probe_current;
#[cfg(unix)]
pub mod transfer;

#[cfg(unix)]
pub use monitor::{WlMonitor, WlMonitorOpts};
#[cfg(unix)]
pub use paste::{PasteOutcome, WlPaste};
#[cfg(unix)]
pub use probe_current::probe_current;
#[cfg(unix)]
pub use transfer::WlTransfer;

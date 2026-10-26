//! Windows session backend (seg-04): monitor, transfer, hotkeys, paste,
//! foreground/integrity, and autostart over direct Win32 APIs.
//!
//! Pure logic (`html`, `optout`, `formats`, `caps`, `autostart_logic`) is
//! headless-testable. Session modules (`sys`, `monitor`, `transfer`,
//! `hotkeys`, `paste`, `foreground`, `autostart`) compile everywhere but only
//! operate on Windows; elsewhere they report `Error::Unavailable`.
//!
//! Unsafe policy (WIN-F1): `unsafe` appears only in `sys.rs`.

pub mod autostart;
pub mod autostart_logic;
pub mod caps;
pub mod foreground;
pub mod formats;
pub mod hotkeys;
pub mod html;
pub mod monitor;
pub mod optout;
pub mod paste;
#[cfg(windows)]
pub(crate) mod sys;
pub mod transfer;

pub use autostart::WinAutostart;
pub use foreground::{current_process_integrity, target_integrity, Integrity};
pub use hotkeys::WinHotkeys;
pub use monitor::WinMonitor;
pub use paste::WinPaste;
pub use transfer::WinTransfer;

/// Healthy capability matrix for this backend (carries the `image/png`
/// passthrough FNF note via [`caps::normal_caps`]).
#[must_use]
pub fn caps() -> clipmonger_platform::types::BackendCaps {
    caps::normal_caps()
}

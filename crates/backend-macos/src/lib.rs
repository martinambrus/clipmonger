//! macOS backend (seg-08): pasteboard monitor and transfer, paste engine,
//! global hotkeys, foreground app, `LaunchAgent` autostart.
//!
//! Pure logic (`mime`, `text`, `uri`, `keys`, `plist`, `caps`) is tested on
//! every OS. Session types compile everywhere and report
//! `Error::Unavailable` off macOS: their OS calls go through `os`, which is
//! `appkit` (objc2) on macOS and `stub` elsewhere.
//!
//! Unsafe policy: `unsafe` lives only in `sys.rs` (plain C: `CGEvent`,
//! Accessibility, Secure Input, `CoreServices` constants) and `appkit.rs`
//! (Objective-C sends the generated bindings lack), each with a
//! module-level allowance.

pub mod autostart;
pub mod caps;
pub mod foreground;
pub mod hotkeys;
pub mod keys;
pub mod main_loop;
pub mod mime;
pub mod monitor;
pub mod paste;
pub mod plist;
pub mod text;
pub mod transfer;
pub mod ui;
pub mod uri;

#[cfg(target_os = "macos")]
mod appkit;
#[cfg(target_os = "macos")]
mod sys;
#[cfg(target_os = "macos")]
use appkit as os;
#[cfg(not(target_os = "macos"))]
mod stub;
#[cfg(not(target_os = "macos"))]
use stub as os;

pub use autostart::{remove_stale_agent, MacAutostart, BUNDLE_ID};
pub use caps::PasteboardAccess;
pub use foreground::{pid_of, MacForeground, RunningApp};
pub use hotkeys::MacHotkeys;
pub use monitor::{MacMonitor, MonitorOpts};
pub use paste::{
    accessibility_trusted, secure_input_enabled, wait_modifiers_released, ClipboardWriteHook,
    MacPaste,
};
pub use transfer::MacTransfer;

/// Pasteboard privacy state (macOS 15.4+ `accessBehavior`; older systems
/// and other OSes report [`PasteboardAccess::Unrestricted`]).
#[must_use]
pub fn pasteboard_access() -> PasteboardAccess {
    os::access()
}

/// Capability probe: Accessibility trust (no prompt) and pasteboard access.
#[must_use]
pub fn caps() -> clipmonger_platform::types::BackendCaps {
    caps::caps_for(accessibility_trusted(false), pasteboard_access())
}

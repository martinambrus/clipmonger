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
pub mod dib;
pub mod elevated_task;
pub mod foreground;
pub mod formats;
pub mod helper;
pub mod hotkeys;
pub mod html;
pub mod monitor;
pub mod optout;
pub mod paste;
#[cfg(windows)]
pub mod sys;
pub mod transfer;
pub mod uri;

pub use autostart::WinAutostart;
pub use foreground::{current_process_integrity, target_integrity, Integrity};
pub use hotkeys::WinHotkeys;
pub use monitor::WinMonitor;
pub use paste::WinPaste;
/// Elevated-helper spawn/auth primitives (Windows only; the runas served
/// leg is manual QA, never automated).
#[cfg(windows)]
pub use sys::{
    allow_set_foreground, foreground_window_id, hwnd_owner_pid, pipe_client_pid, pipe_server_pid,
    process_image_path, process_token_high, program_files_dir, show_owned_window,
    spawn_elevated_worker,
};
pub use transfer::WinTransfer;

/// Healthy capability matrix for this backend (carries the `image/png`
/// note via [`caps::normal_caps`]).
#[must_use]
pub fn caps() -> clipmonger_platform::types::BackendCaps {
    caps::normal_caps()
}

//! Linux Wayland backend (seg-06): pure logic plus the Wayland session backend.
//!
//! Pure modules (no compositor connection) work everywhere; session code
//! (`wlprobe`, later monitor/transfer/portal/paste) needs a Unix Wayland
//! session. v1 paste is the seg-06 S7 fallback (write + toast) only.

pub mod caps;
pub mod merge;
pub mod paste_logic;
pub mod portal;
pub mod primary;
pub mod probe;

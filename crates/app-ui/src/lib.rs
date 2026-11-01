//! Popup, tray and lifecycle logic plus the IPC bridge.

#[cfg(feature = "gui")]
pub mod app;
pub mod chord;
pub mod config_model;
pub mod foreground;
pub mod ipc_bridge;
pub mod keymap;
pub mod lifecycle;
pub mod linux_tray;
pub mod permanents;
pub mod placement;
pub mod popup;
pub mod push;
#[cfg(feature = "gui")]
pub mod runner;
pub mod search;
pub mod shell;
pub mod strings;
pub mod target_session;
pub mod tray_logic;

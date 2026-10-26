//! Backend traits; every trait declares its thread affinity.

use std::time::Duration;

use crate::types::{
    Affinity, AppId, CaptureEvent, ClipRef, HotkeyId, PasteStrategy, PasteTarget, PayloadMap,
    Result, SubscribeKind, TrayEvent, TrayState,
};

/// Clipboard change observations (poll-based for all backends).
pub trait ClipboardMonitor: Send {
    /// Thread affinity of this service.
    const AFFINITY: Affinity;

    /// Subscribe to clipboard kinds.
    ///
    /// # Errors
    ///
    /// Returns [`crate::types::Error`] when the backend refuses.
    fn subscribe(&mut self, kinds: &[SubscribeKind]) -> Result<()>;

    /// Return queued capture events (empty when idle).
    ///
    /// # Errors
    ///
    /// Returns [`crate::types::Error`] when the backend fails.
    fn poll(&mut self, timeout: Duration) -> Result<Vec<CaptureEvent>>;
}

/// Synchronous clipboard data transfer.
pub trait ClipboardTransfer: Send {
    /// Thread affinity of this service.
    const AFFINITY: Affinity;

    /// List available format names.
    ///
    /// # Errors
    ///
    /// Returns [`crate::types::Error`] when the backend fails.
    fn formats(&self) -> Result<Vec<String>>;

    /// Read one format, capped at `max_bytes`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::types::Error`] when the backend fails.
    fn read(&self, mime: &str, max_bytes: u64) -> Result<Vec<u8>>;

    /// Multi-format atomic write.
    ///
    /// # Errors
    ///
    /// Returns [`crate::types::Error`] when the backend fails.
    fn write(&self, payload: &PayloadMap) -> Result<()>;

    /// Backend unchanged-indicator (sequence number / changeCount /
    /// owner+serial / offer token) for the guarded-restore check.
    fn unchanged_indicator(&self) -> Option<String>;
}

/// Global hotkey registration and firing.
pub trait HotkeyManager: Send {
    /// Thread affinity of this service.
    const AFFINITY: Affinity;

    /// Register `id` for `chord` (parsed via [`crate::chord::parse`]).
    ///
    /// # Errors
    ///
    /// Returns [`crate::types::Error`] when registration fails.
    fn register(&mut self, id: HotkeyId, chord: &str) -> Result<()>;

    /// Remove a registration.
    ///
    /// # Errors
    ///
    /// Returns [`crate::types::Error`] when removal fails.
    fn unregister(&mut self, id: HotkeyId) -> Result<()>;

    /// Return fired hotkey ids (empty when idle).
    ///
    /// # Errors
    ///
    /// Returns [`crate::types::Error`] when the backend fails.
    fn poll(&mut self, timeout: Duration) -> Result<Vec<HotkeyId>>;
}

/// Paste delivery into a revalidated target.
pub trait PasteEngine: Send {
    /// Thread affinity of this service.
    const AFFINITY: Affinity;

    /// Delivery strategy of this backend.
    fn strategy(&self) -> PasteStrategy;

    /// Capture the current paste target (before the popup opens).
    ///
    /// # Errors
    ///
    /// Returns [`crate::types::Error`] when no target is available.
    fn capture_target(&self) -> Result<PasteTarget>;

    /// Paste into `target` after revalidating it; stale targets error,
    /// never paste into the popup itself.
    ///
    /// # Errors
    ///
    /// Returns [`crate::types::Error`] on stale/dead targets or failure.
    fn paste_to(&self, target: &PasteTarget, clip: &ClipRef) -> Result<()>;
}

/// System tray icon + menu, hosted by app-ui.
pub trait TrayController: Send {
    /// Thread affinity of this service.
    const AFFINITY: Affinity;

    /// Set attention state.
    fn set_state(&mut self, state: TrayState);

    /// Set tooltip text.
    fn set_tooltip(&mut self, tooltip: &str);

    /// Replace the menu items.
    fn set_menu(&mut self, items: &[String]);

    /// Drain queued tray UI events.
    fn poll_events(&mut self) -> Vec<TrayEvent>;
}

/// Per-OS autostart (login item / registry / desktop file).
pub trait AutostartManager: Send {
    /// Thread affinity of this service.
    const AFFINITY: Affinity;

    /// Enable autostart.
    ///
    /// # Errors
    ///
    /// Returns [`crate::types::Error`] when the OS call fails.
    fn enable(&mut self) -> Result<()>;

    /// Disable autostart.
    ///
    /// # Errors
    ///
    /// Returns [`crate::types::Error`] when the OS call fails.
    fn disable(&mut self) -> Result<()>;

    /// Query autostart state.
    ///
    /// # Errors
    ///
    /// Returns [`crate::types::Error`] when the OS call fails.
    fn is_enabled(&self) -> Result<bool>;
}

/// Foreground application query for `AutoSwitch` (v2).
pub trait ForegroundApp: Send {
    /// Thread affinity of this service.
    const AFFINITY: Affinity;

    /// Currently active application, if known.
    fn active(&self) -> Option<AppId>;
}

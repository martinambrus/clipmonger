//! Paste engine: integrity-first routing with Unicode injection preferred.
//!
//! Order is load-bearing (seg-04 S4): the target's integrity is detected
//! FIRST; elevated targets take the helper route ONLY and never see blind
//! `SendInput` (UIPI drops it silently). All strategies run after the
//! target re-focus step. Rich payloads (or text the target ignores) fall
//! back to `ClipboardRestore` with a guarded restore: the saved value is
//! restored only when the clipboard still equals our temporary write.

#[cfg(windows)]
use clipmonger_platform::paste_policy::check_target;
use clipmonger_platform::traits::PasteEngine;
use clipmonger_platform::types::{Affinity, ClipRef, Error, PasteStrategy, PasteTarget, Result};

/// Resolved paste delivery route for one payload set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryRoute {
    /// Clipboard write + Ctrl+V with guarded restore.
    Clipboard,
    /// Synthetic Unicode keystrokes.
    Inject,
}

/// Pick the delivery route: clipboard-first always uses the clipboard;
/// otherwise text within `unicode_limit` injects and everything else
/// (oversized or non-text payloads) uses the clipboard.
#[must_use]
pub fn delivery_route(
    clipboard_first: bool,
    text_len: Option<usize>,
    unicode_limit: usize,
) -> DeliveryRoute {
    if clipboard_first {
        return DeliveryRoute::Clipboard;
    }
    match text_len {
        Some(len) if len <= unicode_limit => DeliveryRoute::Inject,
        _ => DeliveryRoute::Clipboard,
    }
}

/// Paste delivery engine.
pub struct WinPaste {
    #[cfg_attr(not(windows), allow(dead_code))]
    unicode_limit: usize,
    #[cfg_attr(not(windows), allow(dead_code))]
    dwell_ms: u64,
    #[cfg_attr(not(windows), allow(dead_code))]
    clipboard_first: bool,
    #[cfg_attr(not(windows), allow(dead_code))]
    restore_clipboard: bool,
}

impl WinPaste {
    /// Create with the `popup.unicode_limit` byte budget.
    #[must_use]
    pub fn new(unicode_limit: usize) -> Self {
        Self {
            unicode_limit,
            dwell_ms: 1000,
            clipboard_first: false,
            restore_clipboard: true,
        }
    }

    /// Override the `ClipboardRestore` dwell before restore validation.
    #[must_use]
    pub fn with_dwell_ms(mut self, dwell_ms: u64) -> Self {
        self.dwell_ms = dwell_ms;
        self
    }

    /// Deliver via the clipboard first when `clipboard_first` is set.
    #[must_use]
    pub fn with_clipboard_first(mut self, clipboard_first: bool) -> Self {
        self.clipboard_first = clipboard_first;
        self
    }

    #[must_use]
    pub fn with_restore_clipboard(mut self, restore: bool) -> Self {
        self.restore_clipboard = restore;
        self
    }

    /// Paste stored `payloads` into a revalidated `target`.
    ///
    /// The daemon calls this with the clip's stored payloads; the trait
    /// `paste_to` (no payload bytes) reports `Unsupported`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unavailable`] without a Windows session, or when
    /// the target needs the elevated-helper route (the backend layer never
    /// spawns the helper itself; the daemon owns that channel),
    /// [`Error::Denied`] for stale/dead targets, and [`Error::Unsupported`]
    /// when no portable payload is available.
    pub fn paste_payloads(
        &self,
        target: &PasteTarget,
        clip: &ClipRef,
        payloads: &[(String, Vec<u8>)],
    ) -> Result<()> {
        #[cfg(windows)]
        {
            let _ = clip;
            if crate::foreground::target_integrity(&target.handle)
                == crate::foreground::Integrity::High
            {
                // Route::Helper decision surface: the backend layer never
                // spawns the helper; the daemon owns that channel, so until
                // it provides one this is HelperUnavailable.
                return Err(Error::Unavailable(
                    "helper unavailable: elevated target needs the helper route".to_string(),
                ));
            }
            // Safety: handle round-trips through HWND; it is only queried
            // and activated, never freed.
            let raw = hwnd_of(&target.handle) as windows_sys::Win32::Foundation::HWND;
            settle_foreground(raw)?;
            let current = crate::foreground::capture_foreground().ok();
            check_target(target, current.as_ref())?;
            let text = payloads
                .iter()
                .find(|(mime, _)| mime == "text/plain")
                .map(|(_, bytes)| bytes.clone());
            match delivery_route(
                self.clipboard_first,
                text.as_ref().map(Vec::len),
                self.unicode_limit,
            ) {
                DeliveryRoute::Inject => inject_text(&text.unwrap_or_default())
                    .map_err(|detail| Error::Io(format!("unicode inject failed: {detail}"))),
                DeliveryRoute::Clipboard => {
                    clipboard_restore(payloads, self.dwell_ms, self.restore_clipboard)
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (target, clip, payloads);
            Err(Error::Unavailable(
                "paste needs a Windows session".to_string(),
            ))
        }
    }
}

#[cfg(windows)]
fn hwnd_of(handle: &clipmonger_platform::types::PlatformHandle) -> i64 {
    match handle {
        clipmonger_platform::types::PlatformHandle::WinHwnd(hwnd) => *hwnd,
        _ => 0,
    }
}

/// Wait briefly for `hwnd` to be the foreground window, activating it when
/// Windows has not settled on it after the popup hid (the popup grants
/// foreground rights before hiding). Never types into a different window.
#[cfg(windows)]
fn settle_foreground(hwnd: windows_sys::Win32::Foundation::HWND) -> Result<()> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut next_activate = Instant::now();
    while crate::sys::foreground_hwnd() != Some(hwnd) {
        if Instant::now() >= deadline {
            return Err(Error::Denied(
                "target window is not in the foreground".to_string(),
            ));
        }
        if Instant::now() >= next_activate {
            crate::sys::activate_window(hwnd);
            next_activate = Instant::now() + Duration::from_millis(250);
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    Ok(())
}

/// Inject one helper-job payload as synthetic input. Elevated-worker entry
/// point: the daemon worker validates target foreground + high integrity
/// per job before calling here, and pins the target process handle across
/// the call (no PID-reuse race).
///
/// # Errors
///
/// Returns a message when the payload exceeds 64 KiB, is not usable UTF-8,
/// or the injection backend refuses it.
#[cfg(windows)]
pub fn worker_send_input_batch(payload: &str) -> std::result::Result<(), String> {
    if payload.len() > 64 * 1024 {
        return Err(format!("payload {} bytes exceeds 65536", payload.len()));
    }
    inject_text(payload.as_bytes())
}

/// Press Ctrl+V as synthetic input. Elevated-worker entry point for the
/// clipboard route: the daemon (medium integrity) owns the clipboard
/// write; the helper only presses this chord.
///
/// # Errors
///
/// Returns a message when the injection backend refuses the chord.
pub fn worker_press_paste_chord() -> std::result::Result<(), String> {
    #[cfg(windows)]
    {
        use enigo::{Direction, Enigo, Key, Keyboard, Settings};
        let mut enigo =
            Enigo::new(&Settings::default()).map_err(|e| format!("enigo unavailable: {e}"))?;
        enigo
            .key(Key::Control, Direction::Press)
            .and_then(|()| enigo.key(Key::V, Direction::Click))
            .and_then(|()| enigo.key(Key::Control, Direction::Release))
            .map_err(|e| format!("{e}"))
    }
    #[cfg(not(windows))]
    {
        Err("paste chord needs a Windows session".to_string())
    }
}

/// Formats a guarded restore puts back, by registered name (text, bitmaps
/// and file lists have fixed ids). The opt-out markers come back too, so a
/// restored secret stays out of clipboard histories, ours included.
#[cfg(windows)]
const RESTORE_FORMATS: [&str; 8] = [
    "HTML Format",
    "Rich Text Format",
    crate::transfer::imp::PNG_FORMAT,
    crate::transfer::imp::DROP_EFFECT_FORMAT,
    crate::optout::EXCLUDE_FORMAT,
    crate::optout::HISTORY_OPT_OUT,
    crate::optout::CLOUD_OPT_OUT,
    crate::optout::VIEWER_IGNORE,
];

/// Largest single format the restore snapshot keeps: a raw 32-bit bitmap
/// of three 4K screens (about 100 MB) still fits.
#[cfg(windows)]
const RESTORE_MAX_BYTES: u64 = 128 * 1024 * 1024;

/// Longest the snapshot may delay a paste before the restore is dropped.
#[cfg(windows)]
const RESTORE_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

/// Clipboard swap for the helper route: the daemon writes the payloads,
/// the helper presses Ctrl+V, then [`ClipboardSwap::finish`] restores the
/// saved clipboard only while it still equals the temporary write.
pub struct ClipboardSwap {
    #[cfg_attr(not(windows), allow(dead_code))]
    saved: Option<Vec<(u32, Vec<u8>)>>,
    #[cfg_attr(not(windows), allow(dead_code))]
    pre_state: Option<String>,
    #[cfg_attr(not(windows), allow(dead_code))]
    post_state: Option<String>,
    #[cfg_attr(not(windows), allow(dead_code))]
    temp_text: Option<Vec<u8>>,
}

impl ClipboardSwap {
    /// Save the current clipboard when `restore` is set, then write
    /// `payloads` over it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] when there are no payload bytes, or
    /// the transfer layer's error when the write fails.
    pub fn begin(payloads: &[(String, Vec<u8>)], restore: bool) -> Result<Self> {
        #[cfg(windows)]
        {
            use super::transfer::{imp, WinTransfer};
            use clipmonger_platform::traits::ClipboardTransfer as _;
            if payloads.is_empty() {
                return Err(Error::Unsupported(
                    "clipboard restore needs stored payload bytes".to_string(),
                ));
            }
            let transfer = WinTransfer::new();
            let saved = if restore {
                let mut ids = vec![imp::CF_UNICODETEXT, imp::CF_DIB, imp::CF_HDROP];
                ids.extend(
                    RESTORE_FORMATS
                        .iter()
                        .filter_map(|name| imp::register(name).ok()),
                );
                imp::snapshot(&ids, RESTORE_MAX_BYTES, RESTORE_BUDGET)
                    .filter(|saved| !saved.is_empty())
            } else {
                None
            };
            let pre_state = transfer.unchanged_indicator();
            transfer.write(&payloads.to_vec())?;
            let post_state = transfer.unchanged_indicator();
            let temp_text = payloads
                .iter()
                .find(|(mime, _)| mime == "text/plain")
                .map(|(_, bytes)| bytes.clone());
            Ok(Self {
                saved,
                pre_state,
                post_state,
                temp_text,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (payloads, restore);
            Err(Error::Unavailable(
                "clipboard swap needs a Windows session".to_string(),
            ))
        }
    }

    /// Sleep `dwell_ms`, then guarded-restore if `restore` is true.
    pub fn finish(self, dwell_ms: u64, restore: bool) {
        #[cfg(windows)]
        {
            if !restore {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(dwell_ms));
            guarded_restore(&self);
        }
        #[cfg(not(windows))]
        {
            let _ = (dwell_ms, restore);
        }
    }
}

/// Restore `swap.saved` only when the clipboard still equals the swap's
/// temporary write (sequence indicator AND text content). Shared tail of
/// `clipboard_restore` and [`ClipboardSwap::finish`]; user copies win.
#[cfg(windows)]
fn guarded_restore(swap: &ClipboardSwap) {
    use super::transfer::WinTransfer;
    use clipmonger_platform::traits::ClipboardTransfer as _;
    let transfer = WinTransfer::new();
    let unchanged = transfer.unchanged_indicator() == swap.post_state;
    let current = transfer.read("text/plain", 4 * 1024 * 1024).ok();
    let hash_same = match (&current, &swap.temp_text) {
        (Some(now), Some(temp)) => now == temp,
        (None, None) => true,
        _ => false,
    };
    if clipmonger_platform::paste_policy::guarded_restore_decision(unchanged, hash_same) {
        if let Some(saved) = &swap.saved {
            let _ = super::transfer::imp::restore(saved);
        }
        let _ = &swap.pre_state;
    }
}

/// Unicode injection via enigo text (preferred, no clipboard clobber).
#[cfg(windows)]
fn inject_text(bytes: &[u8]) -> std::result::Result<(), String> {
    use enigo::{Enigo, Keyboard, Settings};
    let text = std::str::from_utf8(bytes).map_err(|e| format!("payload is not UTF-8: {e}"))?;
    if text.contains('\0') {
        return Err("payload contains NUL".to_string());
    }
    let mut enigo =
        Enigo::new(&Settings::default()).map_err(|e| format!("enigo unavailable: {e}"))?;
    enigo.text(text).map_err(|e| format!("{e}"))
}

/// `ClipboardRestore`: save, write payloads, Ctrl+V, restore only when the
/// slot still equals our temporary write (user copies always win).
#[cfg(windows)]
fn clipboard_restore(payloads: &[(String, Vec<u8>)], dwell_ms: u64, restore: bool) -> Result<()> {
    let swap = ClipboardSwap::begin(payloads, restore)?;
    worker_press_paste_chord().map_err(|e| Error::Io(format!("paste chord failed: {e}")))?;
    swap.finish(dwell_ms, restore);
    Ok(())
}

impl PasteEngine for WinPaste {
    const AFFINITY: Affinity = Affinity::Worker;

    fn strategy(&self) -> PasteStrategy {
        if self.clipboard_first {
            PasteStrategy::ClipboardRestore
        } else {
            PasteStrategy::UnicodeInject
        }
    }

    fn capture_target(&self) -> Result<PasteTarget> {
        crate::foreground::capture_foreground()
    }

    fn paste_to(&self, _target: &PasteTarget, _clip: &ClipRef) -> Result<()> {
        Err(Error::Unsupported(
            "paste_to needs stored payload bytes; use paste_payloads".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_clipboard_first_always_clipboard() {
        assert_eq!(delivery_route(true, Some(1), 10), DeliveryRoute::Clipboard);
        assert_eq!(
            delivery_route(true, Some(999), 10),
            DeliveryRoute::Clipboard
        );
        assert_eq!(delivery_route(true, None, 10), DeliveryRoute::Clipboard);
    }

    #[test]
    fn route_inject_within_limit() {
        assert_eq!(delivery_route(false, Some(0), 10), DeliveryRoute::Inject);
        assert_eq!(delivery_route(false, Some(10), 10), DeliveryRoute::Inject);
    }

    #[test]
    fn route_clipboard_over_limit_or_no_text() {
        assert_eq!(
            delivery_route(false, Some(11), 10),
            DeliveryRoute::Clipboard
        );
        assert_eq!(delivery_route(false, None, 10), DeliveryRoute::Clipboard);
    }
}

//! Paste engine: integrity-first routing with Unicode injection preferred.
//!
//! Target integrity is checked before input delivery; elevated targets take
//! the helper route. All strategies run after target re-focus. Rich payloads
//! use a guarded clipboard swap with a conservative in-open sequence check.

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

    /// Enable guarded clipboard restoration after delivery.
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
        self.paste_payloads_with(target, clip, payloads, None)
    }

    /// Paste with notification of clipboard writes and guarded restores.
    ///
    /// # Errors
    /// Returns the same errors as [`Self::paste_payloads`].
    pub fn paste_payloads_with(
        &self,
        target: &PasteTarget,
        clip: &ClipRef,
        payloads: &[(String, Vec<u8>)],
        observer: Option<&dyn ClipboardWriteObserver>,
    ) -> Result<()> {
        #[cfg(windows)]
        {
            let _ = clip;
            if crate::foreground::target_integrity(&target.handle)
                == crate::foreground::Integrity::High
            {
                return Err(Error::Unavailable(
                    "helper unavailable: elevated target needs the helper route".to_string(),
                ));
            }
            let raw = hwnd_of(&target.handle);
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
                    clipboard_restore(payloads, self.dwell_ms, self.restore_clipboard, observer)
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (target, clip, payloads, observer);
            Err(Error::Unavailable(
                "paste delivery needs a Windows session".to_string(),
            ))
        }
    }
}

#[cfg(windows)]
fn hwnd_of(
    handle: &clipmonger_platform::types::PlatformHandle,
) -> windows_sys::Win32::Foundation::HWND {
    match handle {
        clipmonger_platform::types::PlatformHandle::WinHwnd(hwnd) => {
            *hwnd as windows_sys::Win32::Foundation::HWND
        }
        _ => std::ptr::null_mut(),
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
        let mut driver = crate::clip_orchestrator::EnigoChordDriver::new();
        crate::clip_orchestrator::run_chord(&mut driver)
    }
    #[cfg(not(windows))]
    {
        Err("paste chord needs a Windows session".to_string())
    }
}

/// Formats a guarded restore puts back, by registered name (text, bitmaps
/// and file lists have fixed ids). The opt-out markers come back too, so a
/// restored secret stays out of clipboard histories, ours included.
#[cfg(any(windows, test))]
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

#[cfg(any(windows, test))]
pub(crate) fn restore_ids() -> Result<Vec<u32>> {
    use crate::transfer::imp;
    let mut ids = vec![imp::CF_UNICODETEXT, imp::CF_DIB, imp::CF_HDROP];
    for name in RESTORE_FORMATS {
        ids.push(imp::register(name)?);
    }
    Ok(ids)
}

/// Largest single format the restore snapshot keeps: a raw 32-bit bitmap
/// of three 4K screens (about 100 MB) still fits.
#[cfg(windows)]
const RESTORE_MAX_BYTES: u64 = 128 * 1024 * 1024;

/// Snapshot deadline checked around native calls; delayed rendering cannot
/// be preempted. An expired snapshot refuses publication before emptying.
#[cfg(windows)]
const RESTORE_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

/// Clipboard swap for the helper route: the daemon writes the payloads,
/// the helper presses Ctrl+V, then [`ClipboardSwap::finish`] restores the
/// saved clipboard only while it still equals the temporary write.
pub struct ClipboardSwap {
    #[cfg(windows)]
    saved: crate::clip_orchestrator::SnapshotKind,
    #[cfg(windows)]
    expected_seq: Option<String>,
    #[cfg(windows)]
    saved_file_list: Option<Vec<u8>>,
    #[cfg(windows)]
    temp_text: Option<Vec<u8>>,
}

/// Receives the lifecycle of a clipboard write owned by a paste operation.
pub trait ClipboardWriteObserver {
    /// Register the projected capture before a native write.
    fn begin(&self, payloads: &[(String, Vec<u8>)]) -> Option<u64>;
    /// Finalize the sequence while the native clipboard is still open.
    fn written(&self, id: u64, sequence: Option<u32>);
    /// Cancel a failed write.
    fn failed(&self, id: u64);
    /// Capture limit for a file list, if the monitor would read it.
    fn snapshot_file_limit(&self) -> Option<u64> {
        None
    }
    /// Register a capture-visible projection of a saved native snapshot.
    #[cfg(any(windows, test))]
    fn begin_restore(
        &self,
        saved: &[(u32, Vec<u8>)],
        name_for_id: &dyn Fn(u32) -> Option<String>,
        saved_file_list: Option<&[u8]>,
    ) -> Option<u64> {
        let projected = super::transfer::project_saved_snapshot_with_file(
            saved,
            name_for_id,
            16 * 1024 * 1024,
            |_| true,
            saved_file_list,
        )?;
        self.begin(&projected)
    }
}

impl ClipboardSwap {
    /// Save the current clipboard when `restore` is set, then write
    /// `payloads` over it.
    ///
    /// # Errors
    ///
    /// Returns an error before emptying for invalid payloads or an unavailable
    /// required snapshot. Publication errors attempt same-session rollback;
    /// the first error wins and bounded cleanup status goes to diagnostics.
    /// A failed close returns an error without an active swap.
    pub fn begin(payloads: &[(String, Vec<u8>)], restore: bool) -> Result<Self> {
        Self::begin_with(payloads, restore, None)
    }

    /// Begin a swap while reporting the actual write boundary to an observer.
    ///
    /// # Errors
    /// Returns the same errors as [`Self::begin`].
    pub fn begin_with(
        payloads: &[(String, Vec<u8>)],
        restore: bool,
        observer: Option<&dyn ClipboardWriteObserver>,
    ) -> Result<Self> {
        #[cfg(windows)]
        {
            let ids = if restore { restore_ids()? } else { Vec::new() };
            let mut backend = crate::clip_orchestrator::WinBackend::new()?;
            match crate::clip_orchestrator::begin_transaction_observed(
                &mut backend,
                payloads,
                restore,
                &ids,
                RESTORE_MAX_BYTES,
                RESTORE_BUDGET,
                observer,
            ) {
                Ok(succ) => Ok(Self {
                    saved: succ.saved,
                    expected_seq: succ.seq,
                    saved_file_list: succ.saved_file_list,
                    temp_text: succ.temp_text,
                }),
                Err(e) => Err(e.into_primary()),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (payloads, restore, observer);
            Err(Error::Unavailable(
                "clipboard swap needs a Windows session".to_string(),
            ))
        }
    }

    /// Sleep `dwell_ms`, then guarded-restore if `restore` is true.
    pub fn finish(self, dwell_ms: u64, restore: bool) {
        self.finish_with(dwell_ms, restore, None);
    }

    /// Finish the swap and report a guarded restore write when one occurs.
    pub fn finish_with(
        self,
        dwell_ms: u64,
        restore: bool,
        observer: Option<&dyn ClipboardWriteObserver>,
    ) {
        #[cfg(windows)]
        {
            if !restore {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(dwell_ms));
            let Ok(mut backend) = crate::clip_orchestrator::WinBackend::new() else {
                return;
            };
            let _ = crate::clip_orchestrator::finish_transaction_observed(
                &mut backend,
                &self.saved,
                self.expected_seq.as_deref(),
                self.temp_text.as_deref(),
                restore,
                observer,
                self.saved_file_list.as_deref(),
            );
        }
        #[cfg(not(windows))]
        {
            let _ = (dwell_ms, restore, observer);
        }
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
/// slot passes the conservative in-open sequence and bounded text checks.
#[cfg(windows)]
fn clipboard_restore(
    payloads: &[(String, Vec<u8>)],
    dwell_ms: u64,
    restore: bool,
    observer: Option<&dyn ClipboardWriteObserver>,
) -> Result<()> {
    let swap = ClipboardSwap::begin_with(payloads, restore, observer)?;
    finalize_paste(
        swap,
        dwell_ms,
        restore,
        worker_press_paste_chord,
        |swap, dwell, restore| swap.finish_with(dwell, restore, observer),
    )
}

#[cfg(any(windows, test))]
fn finalize_paste<S>(
    swap: S,
    dwell_ms: u64,
    restore: bool,
    chord: impl FnOnce() -> std::result::Result<(), String>,
    finish: impl FnOnce(S, u64, bool),
) -> Result<()> {
    let result = chord().map_err(|e| Error::Io(format!("paste chord failed: {e}")));
    finish(swap, if result.is_ok() { dwell_ms } else { 0 }, restore);
    result
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

#[cfg(test)]
mod finalization_tests {
    use super::finalize_paste;
    use crate::clip_orchestrator::{run_chord, ChordAction, ChordDriver};
    use std::cell::RefCell;

    struct Driver<'a> {
        success: bool,
        events: &'a RefCell<Vec<&'static str>>,
    }
    impl ChordDriver for Driver<'_> {
        fn init(&mut self) -> std::result::Result<(), String> {
            self.events.borrow_mut().push("init");
            Ok(())
        }
        fn perform(&mut self, action: ChordAction) -> std::result::Result<(), String> {
            match action {
                ChordAction::PressCtrl => self.events.borrow_mut().push("press"),
                ChordAction::ClickV => {
                    self.events.borrow_mut().push("click");
                    if !self.success {
                        return Err("click failed".into());
                    }
                }
                ChordAction::ReleaseCtrl => self.events.borrow_mut().push("release"),
            }
            Ok(())
        }
    }
    #[test]
    fn c2_real_caller_finalizes_once_on_success_and_error() {
        for success in [false, true] {
            for restore in [false, true] {
                let events = RefCell::new(Vec::new());
                let mut driver = Driver {
                    success,
                    events: &events,
                };
                let mut calls = Vec::new();
                let result = finalize_paste(
                    17,
                    250,
                    restore,
                    || run_chord(&mut driver),
                    |swap, dwell, enabled| {
                        events.borrow_mut().push("finish");
                        calls.push((swap, dwell, enabled));
                    },
                );
                assert_eq!(
                    events.into_inner(),
                    ["init", "press", "click", "release", "finish"]
                );
                assert_eq!(calls, [(17, if success { 250 } else { 0 }, restore)]);
                assert_eq!(result.is_ok(), success);
                if !success {
                    assert_eq!(
                        result.unwrap_err().to_string(),
                        "io: paste chord failed: click failed"
                    );
                }
            }
        }
    }
}

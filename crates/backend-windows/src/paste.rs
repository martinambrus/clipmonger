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

/// Paste delivery engine.
pub struct WinPaste {
    #[cfg_attr(not(windows), allow(dead_code))]
    unicode_limit: usize,
    #[cfg_attr(not(windows), allow(dead_code))]
    dwell_ms: u64,
}

impl WinPaste {
    /// Create with the `popup.unicode_limit` byte budget.
    #[must_use]
    pub fn new(unicode_limit: usize) -> Self {
        Self {
            unicode_limit,
            dwell_ms: 1000,
        }
    }

    /// Override the `ClipboardRestore` dwell before restore validation.
    #[must_use]
    pub fn with_dwell_ms(mut self, dwell_ms: u64) -> Self {
        self.dwell_ms = dwell_ms;
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
            let current = crate::foreground::capture_foreground().ok();
            check_target(target, current.as_ref())?;
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
            // Safety: handle was validated above; focus only queries it.
            let raw = hwnd_of(&target.handle) as windows_sys::Win32::Foundation::HWND;
            crate::sys::focus_hwnd(raw);
            let text = payloads
                .iter()
                .find(|(mime, _)| mime == "text/plain")
                .map(|(_, bytes)| bytes.clone());
            match text {
                Some(bytes) if bytes.len() <= self.unicode_limit => inject_text(&bytes)
                    .map_err(|detail| Error::Io(format!("unicode inject failed: {detail}"))),
                _ => clipboard_restore(payloads, self.dwell_ms),
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
fn clipboard_restore(payloads: &[(String, Vec<u8>)], dwell_ms: u64) -> Result<()> {
    use super::transfer::WinTransfer;
    use clipmonger_platform::traits::ClipboardTransfer as _;
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};

    let transfer = WinTransfer::new();
    let saved = transfer.read("text/plain", 4 * 1024 * 1024).ok();
    let pre_state = transfer.unchanged_indicator();
    let map: Vec<(String, Vec<u8>)> = payloads.to_vec();
    if map.is_empty() {
        return Err(Error::Unsupported(
            "clipboard restore needs stored payload bytes".to_string(),
        ));
    }
    transfer.write(&map)?;
    let post_state = transfer.unchanged_indicator();
    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|e| Error::Io(format!("enigo unavailable: {e}")))?;
    enigo
        .key(Key::Control, Direction::Press)
        .and_then(|()| enigo.key(Key::V, Direction::Click))
        .and_then(|()| enigo.key(Key::Control, Direction::Release))
        .map_err(|e| Error::Io(format!("paste chord failed: {e}")))?;
    std::thread::sleep(std::time::Duration::from_millis(dwell_ms));
    // Guarded restore: only when sequence AND content still match our write.
    let unchanged = transfer.unchanged_indicator() == post_state;
    let current = transfer.read("text/plain", 4 * 1024 * 1024).ok();
    let temp_text = map
        .iter()
        .find(|(mime, _)| mime == "text/plain")
        .map(|(_, bytes)| bytes.clone());
    let hash_same = match (&current, &temp_text) {
        (Some(now), Some(temp)) => now == temp,
        (None, None) => true,
        _ => false,
    };
    if clipmonger_platform::paste_policy::guarded_restore_decision(unchanged, hash_same) {
        if let Some(bytes) = saved {
            let saved_map = vec![("text/plain".to_string(), bytes)];
            let _ = transfer.write(&saved_map);
        }
        let _ = pre_state;
    }
    Ok(())
}

impl PasteEngine for WinPaste {
    const AFFINITY: Affinity = Affinity::Worker;

    fn strategy(&self) -> PasteStrategy {
        PasteStrategy::UnicodeInject
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

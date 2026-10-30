//! Foreground capture target + integrity detection (seg-04 S4/S5/S8).
//!
//! Integrity is detected FIRST, before any input simulation: UIPI drops
//! `SendInput` to higher-integrity windows silently, so blind attempts are
//! forbidden. Elevated targets take the helper route only (a later task).

#[cfg(windows)]
use clipmonger_platform::types::AppId;
use clipmonger_platform::types::{Error, PasteTarget, PlatformHandle, Result};

/// Process integrity level relevant to UIPI gating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Integrity {
    /// Standard user (or unknown-but-usable) integrity.
    Medium,
    /// Elevated integrity: input simulation from here is silently dropped.
    High,
    /// Could not be determined; callers must treat it as non-elevated.
    Unknown,
}

/// Integrity of the process owning `handle` (`WinHwnd` only).
#[must_use]
pub fn target_integrity(handle: &PlatformHandle) -> Integrity {
    match handle {
        PlatformHandle::WinHwnd(hwnd) => hwnd_integrity(*hwnd),
        _ => Integrity::Unknown,
    }
}

/// Integrity of the current process (self-check for T-408).
#[must_use]
pub fn current_process_integrity() -> Integrity {
    #[cfg(windows)]
    {
        match crate::sys::current_process_elevated() {
            Some(true) => Integrity::High,
            Some(false) => Integrity::Medium,
            None => Integrity::Unknown,
        }
    }
    #[cfg(not(windows))]
    {
        Integrity::Unknown
    }
}

/// Capture the current foreground window as a paste target.
pub(crate) fn capture_foreground() -> Result<PasteTarget> {
    #[cfg(windows)]
    {
        let hwnd = crate::sys::foreground_hwnd()
            .ok_or_else(|| Error::Unavailable("no foreground window".to_string()))?;
        Ok(target_of(hwnd))
    }
    #[cfg(not(windows))]
    {
        Err(Error::Unavailable(
            "foreground capture needs a Windows session".to_string(),
        ))
    }
}

/// Paste target for an explicit top-level window (`hwnd` as captured when
/// the popup opened), instead of whatever is foreground right now.
///
/// # Errors
///
/// Returns [`Error::Unavailable`] when the window no longer exists or
/// there is no Windows session.
pub fn target_for_hwnd(hwnd: i64) -> Result<PasteTarget> {
    #[cfg(windows)]
    {
        // Safety: i64 handle round-trips through HWND on 64-bit Windows (the
        // only target this backend supports); it is only validated/queried.
        let handle = hwnd as windows_sys::Win32::Foundation::HWND;
        if hwnd == 0 || !crate::sys::is_window(handle) {
            return Err(Error::Unavailable(
                "paste target window is gone".to_string(),
            ));
        }
        Ok(target_of(handle))
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        Err(Error::Unavailable(
            "foreground capture needs a Windows session".to_string(),
        ))
    }
}

/// Describe `hwnd` as a paste target (owner exe, title hash, handle).
#[cfg(windows)]
fn target_of(hwnd: windows_sys::Win32::Foundation::HWND) -> PasteTarget {
    let pid = crate::sys::window_pid(hwnd);
    let exe = pid
        .and_then(crate::sys::process_basename)
        .unwrap_or_else(|| "unknown".to_string());
    let title_hash = crate::sys::window_title_hash(hwnd);
    PasteTarget {
        app: AppId { exe, title_hash },
        window_title_hash: title_hash,
        // Safety: HWND round-trips through i64 on 64-bit Windows (the
        // only target this backend supports).
        handle: PlatformHandle::WinHwnd(hwnd as i64),
        captured_ms: now_ms(),
    }
}

#[cfg(windows)]
fn hwnd_integrity(hwnd: i64) -> Integrity {
    // Safety: i64 handle round-trips through HWND on 64-bit Windows (the only
    // target this backend supports); the sys calls only query it, never free it.
    let handle = hwnd as windows_sys::Win32::Foundation::HWND;
    match crate::sys::window_pid(handle).and_then(crate::sys::process_elevated) {
        Some(true) => Integrity::High,
        Some(false) => Integrity::Medium,
        None => Integrity::Unknown,
    }
}

#[cfg(not(windows))]
fn hwnd_integrity(_hwnd: i64) -> Integrity {
    Integrity::Unknown
}

#[cfg_attr(not(windows), allow(dead_code))]
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

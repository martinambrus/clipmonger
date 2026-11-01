#![allow(clippy::missing_errors_doc, clippy::pedantic)]
//! Paste engine: integrity-first routing with Unicode injection preferred.
#[cfg(windows)]
use clipmonger_platform::paste_policy::check_target;
use clipmonger_platform::traits::PasteEngine;
use clipmonger_platform::types::{Affinity, ClipRef, Error, PasteStrategy, PasteTarget, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryRoute {
    Clipboard,
    Inject,
}

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
    #[must_use]
    pub fn new(unicode_limit: usize) -> Self {
        Self {
            unicode_limit,
            dwell_ms: 1000,
            clipboard_first: false,
            restore_clipboard: true,
        }
    }

    #[must_use]
    pub fn with_dwell_ms(mut self, dwell_ms: u64) -> Self {
        self.dwell_ms = dwell_ms;
        self
    }

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
                return Err(Error::Unavailable(
                    "helper unavailable: elevated target needs the helper route".to_string(),
                ));
            }
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

#[cfg(windows)]
pub fn worker_send_input_batch(payload: &str) -> std::result::Result<(), String> {
    if payload.len() > 64 * 1024 {
        return Err(format!("payload {} bytes exceeds 65536", payload.len()));
    }
    inject_text(payload.as_bytes())
}

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

#[cfg(windows)]
const RESTORE_MAX_BYTES: u64 = 128 * 1024 * 1024;

#[cfg(windows)]
const RESTORE_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

pub struct ClipboardSwap {
    #[cfg_attr(not(windows), allow(dead_code))]
    saved: crate::clip_orchestrator::SnapshotKind,
    #[cfg_attr(not(windows), allow(dead_code))]
    expected_seq: Option<String>,
    #[cfg_attr(not(windows), allow(dead_code))]
    temp_text: Option<Vec<u8>>,
    #[cfg_attr(not(windows), allow(dead_code))]
    temp_has_text: bool,
}

impl ClipboardSwap {
    pub fn begin(payloads: &[(String, Vec<u8>)], restore: bool) -> Result<Self> {
        #[cfg(windows)]
        {
            let mut ids = vec![
                crate::transfer::imp::CF_UNICODETEXT,
                crate::transfer::imp::CF_DIB,
                crate::transfer::imp::CF_HDROP,
            ];
            ids.extend(
                RESTORE_FORMATS
                    .iter()
                    .filter_map(|name| crate::transfer::imp::register(name).ok()),
            );
            let mut backend = crate::clip_orchestrator::WinBackend::new()?;
            match crate::clip_orchestrator::begin_transaction(
                &mut backend,
                payloads,
                restore,
                &ids,
                RESTORE_MAX_BYTES,
                RESTORE_BUDGET,
            ) {
                Ok(succ) => Ok(Self {
                    saved: succ.saved,
                    expected_seq: succ.seq,
                    temp_text: succ.temp_text,
                    temp_has_text: succ.temp_has_text,
                }),
                Err(e) => Err(e.primary),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (payloads, restore);
            Err(Error::Unavailable(
                "clipboard swap needs a Windows session".to_string(),
            ))
        }
    }

    pub fn finish(self, dwell_ms: u64, restore: bool) {
        #[cfg(windows)]
        {
            if !restore {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(dwell_ms));
            let mut backend = match crate::clip_orchestrator::WinBackend::new() {
                Ok(b) => b,
                Err(_) => return,
            };
            let _ = crate::clip_orchestrator::finish_transaction(
                &mut backend,
                &self.saved,
                &self.expected_seq,
                &self.temp_text,
                self.temp_has_text,
                restore,
            );
        }
        #[cfg(not(windows))]
        {
            let _ = (dwell_ms, restore);
        }
    }
}

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

#[cfg(windows)]
fn clipboard_restore(payloads: &[(String, Vec<u8>)], dwell_ms: u64, restore: bool) -> Result<()> {
    let swap = ClipboardSwap::begin(payloads, restore)?;
    let chord_res =
        worker_press_paste_chord().map_err(|e| Error::Io(format!("paste chord failed: {e}")));
    let dwell = if chord_res.is_ok() { dwell_ms } else { 0 };
    swap.finish(dwell, restore);
    chord_res
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

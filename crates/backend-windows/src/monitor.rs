//! Clipboard change monitor: hidden `HWND_MESSAGE` window plus
//! `AddClipboardFormatListener`; `WM_CLIPBOARDUPDATE` wakes are deduplicated
//! by clipboard sequence number and expanded into `CaptureEvent`s on poll.
//!
//! Privacy opt-outs (seg-04 S1) are honored here: skipped captures still
//! produce an event with `sensitive=true` so the daemon counts them in
//! `privacy_skips` without storing anything.

#[cfg(windows)]
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;

use clipmonger_platform::traits::ClipboardMonitor;
use clipmonger_platform::types::{Affinity, CaptureEvent, Error, Result, SubscribeKind};
#[cfg(windows)]
use clipmonger_platform::types::{CaptureOrigin, FormatOffer};

/// Session clipboard monitor.
pub struct WinMonitor {
    #[cfg(windows)]
    pump: Option<crate::sys::PumpHandle>,
    #[cfg(windows)]
    wakes: Option<Receiver<()>>,
    #[cfg(windows)]
    subscribed_clipboard: bool,
    #[cfg(windows)]
    last_seq: Option<u32>,
}

impl WinMonitor {
    /// Start the pump thread and install the clipboard listener.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unavailable`] when the session cannot provide a
    /// clipboard listener (non-Windows builds always fail this way).
    pub fn new() -> Result<Self> {
        #[cfg(windows)]
        {
            let (tx, rx): (Sender<()>, Receiver<()>) = std::sync::mpsc::channel();
            let pump = crate::sys::start_pump(tx)
                .map_err(|e| Error::Unavailable(format!("monitor unavailable: {e}")))?;
            Ok(Self {
                pump: Some(pump),
                wakes: Some(rx),
                subscribed_clipboard: false,
                last_seq: None,
            })
        }
        #[cfg(not(windows))]
        {
            Err(Error::Unavailable(
                "clipboard monitor needs a Windows session".to_string(),
            ))
        }
    }
}

impl Drop for WinMonitor {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            if let Some(pump) = self.pump.as_mut() {
                pump.stop();
            }
        }
    }
}

#[cfg(windows)]
fn current_seq() -> Option<u32> {
    clipboard_win::raw::seq_num().map(std::num::NonZeroU32::get)
}

/// Best-effort read of a small registered format as a string (opt-out values).
#[cfg(windows)]
fn read_format_string(id: u32) -> Option<String> {
    let mut out = Vec::new();
    clipboard_win::raw::get_vec(id, &mut out).ok()?;
    if out.len() > 64 {
        return None;
    }
    let text = String::from_utf8_lossy(&out).into_owned();
    Some(text.trim_matches('\0').trim().to_string())
}

/// Enumerate current offers plus opt-out `(name, value)` pairs.
#[cfg(windows)]
type Offers = (Vec<FormatOffer>, Vec<(String, Option<String>)>);

#[cfg(windows)]
fn enumerate_offers() -> Result<Offers> {
    crate::transfer::imp::with_open("enumerate", || {
        let mut offers = Vec::new();
        for id in clipboard_win::raw::EnumFormats::new() {
            if let Some(mime) = crate::transfer::imp::mime_for_id(id) {
                offers.push(FormatOffer {
                    mime,
                    bytes_hint: None,
                });
            }
        }
        let mut probe = Vec::new();
        for name in [
            crate::optout::EXCLUDE_FORMAT,
            crate::optout::HISTORY_OPT_OUT,
            crate::optout::CLOUD_OPT_OUT,
            crate::optout::VIEWER_IGNORE,
        ] {
            if let Some(id) = clipboard_win::raw::register_format(name) {
                let id = id.get();
                if clipboard_win::raw::is_format_avail(id) {
                    probe.push((name.to_string(), read_format_string(id)));
                }
            }
        }
        Ok((offers, probe))
    })
}

/// Build the capture event for the current clipboard content.
#[cfg(windows)]
fn current_event() -> Result<CaptureEvent> {
    let (offers, probe) = enumerate_offers()?;
    
    let sensitive = crate::optout::should_skip(&probe);
    Ok(CaptureEvent {
        offers,
        origin: CaptureOrigin::Clipboard,
        sensitive,
    })
}

impl ClipboardMonitor for WinMonitor {
    const AFFINITY: Affinity = Affinity::Worker;

    fn subscribe(&mut self, kinds: &[SubscribeKind]) -> Result<()> {
        #[cfg(windows)]
        {
            self.subscribed_clipboard = kinds.contains(&SubscribeKind::Clipboard);
            self.last_seq = current_seq();
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = kinds;
            Err(Error::Unavailable(
                "clipboard monitor needs a Windows session".to_string(),
            ))
        }
    }

    fn poll(&mut self, timeout: Duration) -> Result<Vec<CaptureEvent>> {
        #[cfg(windows)]
        {
            if !self.subscribed_clipboard {
                return Ok(Vec::new());
            }
            let rx = self
                .wakes
                .as_ref()
                .ok_or_else(|| Error::Unavailable("monitor pump is not running".to_string()))?;
            let deadline = Instant::now() + timeout;
            let mut events = Vec::new();
            loop {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                match rx.recv_timeout(deadline - now) {
                    Ok(()) => {
                        // One wake may race transient lock contention (another
                        // process holding the clipboard open); resample until
                        // the deadline so a Timeout never loses the update.
                        // Each attempt self-paces (open retries ~100 ms), so
                        // no extra sleep is needed and the deadline bounds us.
                        let mut retries = 15;
                        while retries > 0 && Instant::now() < deadline {
                            retries -= 1;
                            let seq = current_seq();
                            if seq == self.last_seq {
                                std::thread::sleep(std::time::Duration::from_millis(20));
                                continue;
                            }
                            match current_event() {
                                Ok(event) => {
                                    if event.offers.is_empty() {
                                        std::thread::sleep(std::time::Duration::from_millis(40));
                                        continue;
                                    }
                                    self.last_seq = seq;
                                    events.push(event);
                                    std::thread::sleep(std::time::Duration::from_millis(30));
                                    continue;
                                }
                                Err(Error::Timeout(_)) => {
                                    std::thread::sleep(std::time::Duration::from_millis(20));
                                    continue;
                                }
                                Err(e) => return Err(e),
                            }
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(Error::Unavailable("monitor pump thread exited".to_string()));
                    }
                }
            }
            if events.is_empty() {
                if let Some(seq) = current_seq() {
                    if Some(seq) != self.last_seq {
                        match current_event() {
                            Ok(event) => {
                                self.last_seq = Some(seq);
                                events.push(event);
                            }
                            Err(Error::Timeout(_)) => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
            }
            Ok(events)
        }
        #[cfg(not(windows))]
        {
            let _ = timeout;
            Err(Error::Unavailable(
                "clipboard monitor needs a Windows session".to_string(),
            ))
        }
    }
}

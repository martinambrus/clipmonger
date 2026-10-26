//! Synchronous Wayland clipboard transfer via `wl-clipboard-rs`.
//!
//! Reads use short-lived per-call connections (`paste::get_contents`);
//! writes use `copy::Options::copy_multi`, which (with the default
//! `foreground(false)`) spawns a serving thread owned by the copy
//! operation — the daemon is long-lived, so serving paste requests after
//! `write` returns is natural (seg-06 S2 lifetime rule). The
//! `unchanged_indicator` is a synthesized `offer:<n>` counter bumped on
//! every own write; Wayland offers no native owner token, so guarded
//! restores treat it as weaker than the X11 owner+serial indicator.

use std::io::Read as _;
use std::sync::Mutex;

use clipmonger_platform::traits::ClipboardTransfer;
use clipmonger_platform::types::{Affinity, Error, PayloadMap, Result};

/// Wayland clipboard transfer.
pub struct WlTransfer {
    /// `offer:<n>` counter bumped on every own write.
    serial: Mutex<u64>,
}

impl WlTransfer {
    /// Connect to the compositor (`$WAYLAND_DISPLAY`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unavailable`] without a Wayland session.
    pub fn connect() -> Result<Self> {
        if wayland_client::Connection::connect_to_env().is_err() {
            return Err(Error::Unavailable(
                "wayland transfer needs a Wayland session".to_string(),
            ));
        }
        Ok(Self {
            serial: Mutex::new(0),
        })
    }
}

impl ClipboardTransfer for WlTransfer {
    const AFFINITY: Affinity = Affinity::Worker;

    fn formats(&self) -> Result<Vec<String>> {
        wl_clipboard_rs::paste::get_mime_types(
            wl_clipboard_rs::paste::ClipboardType::Regular,
            wl_clipboard_rs::paste::Seat::Unspecified,
        )
        .map(|types| types.into_iter().collect())
        .map_err(|e| Error::Unavailable(format!("wayland formats: {e}")))
    }

    fn read(&self, mime: &str, max_bytes: u64) -> Result<Vec<u8>> {
        let (mut reader, _actual) = wl_clipboard_rs::paste::get_contents(
            wl_clipboard_rs::paste::ClipboardType::Regular,
            wl_clipboard_rs::paste::Seat::Unspecified,
            wl_clipboard_rs::paste::MimeType::Specific(mime),
        )
        .map_err(|e| Error::Unavailable(format!("wayland read({mime}): {e}")))?;
        let mut out = Vec::new();
        reader
            .read_to_end(&mut out)
            .map_err(|e| Error::Unavailable(format!("wayland read body({mime}): {e}")))?;
        if out.len() as u64 > max_bytes {
            return Err(Error::TooLarge(format!(
                "wayland read({mime}): {} bytes over cap {max_bytes}",
                out.len()
            )));
        }
        Ok(out)
    }

    fn write(&self, payload: &PayloadMap) -> Result<()> {
        if payload.is_empty() {
            return Err(Error::Unsupported("nothing to write".to_string()));
        }
        let mut options = wl_clipboard_rs::copy::Options::new();
        options.clipboard(wl_clipboard_rs::copy::ClipboardType::Regular);
        let sources = payload
            .iter()
            .map(|(mime, bytes)| wl_clipboard_rs::copy::MimeSource {
                source: wl_clipboard_rs::copy::Source::Bytes(bytes.clone().into_boxed_slice()),
                mime_type: wl_clipboard_rs::copy::MimeType::Specific(mime.clone()),
            })
            .collect();
        options
            .copy_multi(sources)
            .map_err(|e| Error::Unavailable(format!("wayland write: {e}")))?;
        if let Ok(mut serial) = self.serial.lock() {
            *serial = serial.wrapping_add(1);
        }
        Ok(())
    }

    fn unchanged_indicator(&self) -> Option<String> {
        self.serial
            .lock()
            .ok()
            .and_then(|serial| (*serial > 0).then(|| format!("offer:{serial}")))
    }
}

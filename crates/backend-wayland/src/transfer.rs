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

fn read_bounded(reader: impl std::io::Read, max_bytes: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    reader
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|error| Error::Unavailable(format!("wayland read body: {error}")))?;
    if out.len() as u64 > max_bytes {
        return Err(Error::TooLarge("wayland payload over cap".to_string()));
    }
    Ok(out)
}

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

    /// Multi-format atomic write of any payload slice.
    ///
    /// The slice form also accepts array literals and `&Vec` (via deref);
    /// the [`ClipboardTransfer`] impl delegates here.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] for an empty payload or
    /// [`Error::Unavailable`] when the compositor write fails.
    pub fn write(&self, payload: &[(String, Vec<u8>)]) -> Result<()> {
        if payload.is_empty() {
            return Err(Error::Unsupported("nothing to write".to_string()));
        }
        let mut options = wl_clipboard_rs::copy::Options::new();
        options.clipboard(wl_clipboard_rs::copy::ClipboardType::Regular);
        // Offer exactly `payload`. By default wl-clipboard-rs adds plain-text
        // types to any `text/*` source (HTML would be offered as plain text),
        // and the capture of our own write would then not match it.
        options.omit_additional_text_mime_types(true);
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
}

/// The payload set a Wayland write offers: `payload`, plus `text/plain`
/// when only `text/plain;charset=utf-8` is present (or the reverse), so
/// apps that ask for either type find the text.
#[must_use]
pub fn with_text_twin(payload: &[(String, Vec<u8>)]) -> Vec<(String, Vec<u8>)> {
    const UTF8: &str = "text/plain;charset=utf-8";
    const PLAIN: &str = "text/plain";
    let mut out = payload.to_vec();
    let has = |mime: &str| payload.iter().any(|(m, _)| m == mime);
    if let Some((_, bytes)) = payload.iter().find(|(m, _)| m == UTF8) {
        if !has(PLAIN) {
            out.push((PLAIN.to_string(), bytes.clone()));
        }
    } else if let Some((_, bytes)) = payload.iter().find(|(m, _)| m == PLAIN) {
        out.push((UTF8.to_string(), bytes.clone()));
    }
    out
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
        let (reader, _actual) = wl_clipboard_rs::paste::get_contents(
            wl_clipboard_rs::paste::ClipboardType::Regular,
            wl_clipboard_rs::paste::Seat::Unspecified,
            wl_clipboard_rs::paste::MimeType::Specific(mime),
        )
        .map_err(|e| Error::Unavailable(format!("wayland read({mime}): {e}")))?;
        read_bounded(reader, max_bytes)
    }

    fn write(&self, payload: &PayloadMap) -> Result<()> {
        self.write(payload)
    }

    fn unchanged_indicator(&self) -> Option<String> {
        self.serial
            .lock()
            .ok()
            .and_then(|serial| (*serial > 0).then(|| format!("offer:{serial}")))
    }
}

#[cfg(test)]
mod bounded_tests {
    use super::*;
    use std::cell::Cell;

    struct FiniteOversize<'a> {
        remaining: usize,
        read_bytes: &'a Cell<usize>,
    }
    impl std::io::Read for FiniteOversize<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Ok(0);
            }
            let n = out.len().min(self.remaining);
            out[..n].fill(1);
            self.remaining -= n;
            self.read_bytes.set(self.read_bytes.get() + n);
            Ok(n)
        }
    }

    #[test]
    fn stream_stops_at_cap_plus_one_and_returns_typed_oversize() {
        let read = Cell::new(0);
        let reader = FiniteOversize {
            remaining: 12,
            read_bytes: &read,
        };
        assert!(matches!(read_bounded(reader, 4), Err(Error::TooLarge(_))));
        assert_eq!(read.get(), 5);
        assert_eq!(read_bounded(&b"abcd"[..], 4).unwrap(), b"abcd");
    }
}

#[cfg(test)]
mod twin_tests {
    use super::with_text_twin;

    fn p(items: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
        items.iter().map(|(m, b)| ((*m).to_string(), b.as_bytes().to_vec())).collect()
    }

    #[test]
    fn text_gets_its_twin_and_nothing_else_changes() {
        assert_eq!(
            with_text_twin(&p(&[("text/plain;charset=utf-8", "hi")])),
            p(&[("text/plain;charset=utf-8", "hi"), ("text/plain", "hi")])
        );
        assert_eq!(
            with_text_twin(&p(&[("text/plain", "hi")])),
            p(&[("text/plain", "hi"), ("text/plain;charset=utf-8", "hi")])
        );
        let both = p(&[("text/plain", "a"), ("text/plain;charset=utf-8", "b")]);
        assert_eq!(with_text_twin(&both), both);
        let html = p(&[("text/html", "<b>x</b>")]);
        assert_eq!(with_text_twin(&html), html);
    }
}

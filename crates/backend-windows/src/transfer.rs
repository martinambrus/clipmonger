//! Synchronous clipboard data transfer over direct Win32 clipboard APIs.
//!
//! Writes are one `OpenClipboard` / `EmptyClipboard` / `SetClipboardData*n` /
//! `CloseClipboard` sequence (atomic). Opening retries 5x20 ms; exhaustion
//! maps to `Error::Timeout` (the clipboard is a shared lock).

use clipmonger_platform::traits::ClipboardTransfer;
use clipmonger_platform::types::{Affinity, Error, PayloadMap, Result};

// Decode incrementally: UTF-16 can expand in UTF-8, so a native size
// preflight alone is insufficient to bound the normalized text allocation.
#[cfg(any(windows, test))]
fn decode_unicode_bounded(raw: &[u8], max_bytes: u64) -> Result<Vec<u8>> {
    let units = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|unit| *unit != 0);
    let mut out = Vec::new();
    for character in char::decode_utf16(units) {
        let character = character.unwrap_or(char::REPLACEMENT_CHARACTER);
        let mut encoded = [0; 4];
        let encoded = character.encode_utf8(&mut encoded).as_bytes();
        if (out.len() as u64).saturating_add(encoded.len() as u64) > max_bytes {
            return Err(Error::TooLarge("clipboard text over cap".to_string()));
        }
        out.extend_from_slice(encoded);
    }
    Ok(out)
}

#[cfg(any(windows, test))]
fn html_payload_bounded(raw: &[u8], max_bytes: u64) -> Result<Vec<u8>> {
    const MAX_HEADER_BYTES: usize = 64 * 1024;
    let prefix = &raw[..raw.len().min(MAX_HEADER_BYTES)];
    let header = match std::str::from_utf8(prefix) {
        Ok(header) => header,
        // A bounded prefix can end within a body code point. Offsets in the
        // preceding complete header remain readable without copying it.
        Err(error) => std::str::from_utf8(&prefix[..error.valid_up_to()])
            .map_err(|_| Error::Internal("HTML header UTF-8 boundary".to_string()))?,
    };
    let mut fragment = match crate::html::cf_html_fragment_range(header, raw.len()) {
        Ok(range) => &raw[range],
        Err(_) => raw, // Keep the existing malformed-CF_HTML raw fallback.
    };
    if fragment.len() as u64 > max_bytes {
        return Err(Error::TooLarge(
            "clipboard HTML fragment over cap".to_string(),
        ));
    }
    if std::str::from_utf8(fragment).is_err() {
        fragment = raw;
        if fragment.len() as u64 > max_bytes {
            return Err(Error::TooLarge(
                "clipboard malformed HTML over cap".to_string(),
            ));
        }
    }
    Ok(fragment.to_vec())
}

#[cfg(any(windows, test))]
fn read_text_candidates(mut read: impl FnMut(u32) -> Result<Vec<u8>>) -> Result<Vec<u8>> {
    for id in [13, 1, 7] {
        // CF_UNICODETEXT, CF_TEXT, CF_OEMTEXT
        match read(id) {
            Ok(bytes) if !bytes.is_empty() => return Ok(bytes),
            Err(error @ Error::TooLarge(_)) => return Err(error),
            _ => {}
        }
    }
    Err(Error::Unavailable("text/plain not available".to_string()))
}

// The callback checks native availability and reads only within the raw DIB
// cap. The PNG cap applies after conversion, not to the raw bitmap.
#[cfg(any(windows, test))]
fn read_dib_candidates(
    ids: [u32; 2],
    max_bytes: u64,
    mut read: impl FnMut(u32) -> Option<Result<Vec<u8>>>,
) -> Result<Vec<u8>> {
    let mut failure = Error::Unavailable("image/png not available".to_string());
    for id in ids {
        let dib = match read(id) {
            None => continue,
            Some(Ok(bytes)) => bytes,
            Some(Err(error @ Error::TooLarge(_))) => return Err(error),
            Some(Err(error)) => {
                failure = error;
                continue;
            }
        };
        match crate::dib::dib_to_png_classified(&dib) {
            Ok(png) => {
                if png.len() as u64 > max_bytes {
                    return Err(Error::TooLarge(format!(
                        "clipboard payload {} bytes over cap {max_bytes}",
                        png.len()
                    )));
                }
                return Ok(png);
            }
            Err(crate::dib::DibDecodeError::Limit(msg)) => return Err(Error::TooLarge(msg)),
            Err(crate::dib::DibDecodeError::Unsupported(msg)) => failure = Error::Unsupported(msg),
        }
    }
    Err(failure)
}

/// Direct-OS clipboard transfer (seg-04 S2).
pub struct WinTransfer;

impl WinTransfer {
    /// Infallible constructor (no session handle needed up front).
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Multi-format atomic write of any payload slice.
    ///
    /// The slice form also accepts array literals and `&Vec` (via deref);
    /// the [`ClipboardTransfer`] impl delegates here.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] for unknown formats or a file list
    /// without Windows paths (before touching the clipboard),
    /// [`Error::Timeout`] when the shared lock cannot be opened, or
    /// [`Error::Unavailable`] without a Windows session.
    pub fn write(&self, payload: &[(String, Vec<u8>)]) -> Result<()> {
        #[cfg(windows)]
        {
            use std::borrow::Cow;
            // Pre-validate every MIME (and build the bitmap for a PNG) before
            // touching the clipboard so a bad entry fails without clobbering
            // the current content.
            let mut ops: Vec<(u32, &str, Cow<'_, [u8]>)> = Vec::with_capacity(payload.len() + 1);
            for (mime, bytes) in payload {
                let id = imp::id_for_mime(mime)?;
                ops.push((id, mime.as_str(), Cow::Borrowed(bytes.as_slice())));
                if mime == "image/png" {
                    // Apps without PNG support paste the bitmap instead;
                    // an image over the decode guard goes out as PNG only.
                    if let Ok(dib) = crate::dib::png_to_dib(bytes) {
                        ops.push((imp::CF_DIB, mime.as_str(), Cow::Owned(dib)));
                    }
                }
                if mime == "text/uri-list" {
                    if std::str::from_utf8(bytes)
                        .map(crate::uri::uri_list_to_paths)
                        .unwrap_or_default()
                        .is_empty()
                    {
                        return Err(Error::Unsupported(
                            "text/uri-list names no Windows file".to_string(),
                        ));
                    }
                    // Pair the file list with its drop effect like Explorer
                    // does: a lone CF_HDROP overflows the clipboard sync of
                    // FreeRDP-based tools (RustDesk 1.4.9). Paste copies.
                    if !payload.iter().any(|(m, _)| m == imp::DROP_EFFECT_MIME) {
                        ops.push((
                            imp::register(imp::DROP_EFFECT_FORMAT)?,
                            imp::DROP_EFFECT_MIME,
                            Cow::Owned(imp::DROPEFFECT_COPY.to_le_bytes().to_vec()),
                        ));
                    }
                }
            }
            imp::with_open("write", || {
                clipboard_win::raw::empty().map_err(|e| imp::clip_error("empty", e))?;
                for (id, mime, bytes) in &ops {
                    imp::write_one(*id, mime, bytes)?;
                }
                Ok(())
            })
        }
        #[cfg(not(windows))]
        {
            let _ = payload;
            Err(Error::Unavailable(
                "clipboard transfer needs a Windows session".to_string(),
            ))
        }
    }
}

impl Default for WinTransfer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
pub(crate) mod imp {
    use super::{Error, Result};
    use clipboard_win::raw;

    /// Attempts and pause between clipboard opens (spec retry budget).
    const OPEN_ATTEMPTS: u32 = 5;
    const OPEN_RETRY_MS: u64 = 20;

    /// Standard (predefined, nameless) format ids we understand.
    pub(crate) const CF_TEXT: u32 = 1;
    pub(crate) const CF_OEMTEXT: u32 = 7;
    pub(crate) const CF_DIB: u32 = 8;
    const CF_DIBV5: u32 = 17;
    pub(crate) const CF_HDROP: u32 = 15;
    pub(crate) const CF_UNICODETEXT: u32 = 13;

    /// Registered format most apps use for PNG bytes.
    pub(crate) const PNG_FORMAT: &str = "PNG";
    const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

    /// Copy-or-move hint Explorer publishes beside every `CF_HDROP`.
    pub(crate) const DROP_EFFECT_FORMAT: &str = "Preferred DropEffect";
    pub(crate) const DROP_EFFECT_MIME: &str = "x-os/Preferred DropEffect";
    pub(crate) const DROPEFFECT_COPY: u32 = 1;

    pub(crate) fn clip_error(op: &str, err: impl std::fmt::Display) -> Error {
        Error::Io(format!("clipboard {op} failed: {err}"))
    }

    /// Run `body` with the clipboard open, retrying the open 5x20 ms. A
    /// window of our own owns the session: an `OpenClipboard(NULL)` session
    /// is not exclusive, another app opening with NULL can close it under us.
    pub(crate) fn with_open<T>(op: &str, body: impl FnOnce() -> Result<T>) -> Result<T> {
        let owner = crate::sys::ClipboardWindow::new().ok_or_else(|| {
            Error::Unavailable(format!(
                "clipboard {op}: no owner window: {}",
                crate::sys::last_error()
            ))
        })?;
        let mut attempts = OPEN_ATTEMPTS;
        loop {
            match raw::open_for(owner.hwnd()) {
                Ok(()) => break,
                Err(e) => {
                    attempts -= 1;
                    if attempts == 0 {
                        return Err(Error::Timeout(format!(
                            "clipboard {op} timed out opening (shared lock): {e}"
                        )));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(OPEN_RETRY_MS));
                }
            }
        }
        let result = body();
        let _ = raw::close();
        result
    }

    /// Canonical MIME for a clipboard format id, or `None` to skip it.
    pub(crate) fn mime_for_id(id: u32) -> Option<String> {
        match id {
            CF_UNICODETEXT | CF_TEXT | CF_OEMTEXT => Some("text/plain".to_string()),
            CF_DIB | CF_DIBV5 => Some("image/png".to_string()),
            CF_HDROP => Some("text/uri-list".to_string()),
            _ => {
                let name = raw::format_name_big(id)?;
                if crate::optout::is_optout_name(&name) {
                    return None;
                }
                match crate::formats::windows_to_canonical(&name) {
                    Some(canonical) => Some(canonical.to_string()),
                    None => Some(format!("x-os/{name}")),
                }
            }
        }
    }

    /// Resolve a canonical or `x-os/` MIME to a clipboard format id.
    /// Standard formats have fixed ids and no registered name (registering
    /// `CF_HDROP` would create an unrelated private format); `image/png`
    /// is the registered "PNG" format (writes add `CF_DIB`).
    pub(crate) fn id_for_mime(mime: &str) -> Result<u32> {
        match mime {
            "text/plain" | "text/plain;charset=utf-8" => return Ok(CF_UNICODETEXT),
            "text/uri-list" => return Ok(CF_HDROP),
            "image/png" => return register(PNG_FORMAT),
            _ => {}
        }
        if let Some(windows) = crate::formats::canonical_to_windows(mime) {
            return register(windows);
        }
        if let Some(name) = mime.strip_prefix("x-os/") {
            if name.is_empty() {
                return Err(Error::Unsupported(format!("bad format name '{mime}'")));
            }
            return register(name);
        }
        Err(Error::Unsupported(format!("unsupported format '{mime}'")))
    }

    pub(crate) fn register(name: &str) -> Result<u32> {
        raw::register_format(name)
            .map(std::num::NonZeroU32::get)
            .ok_or_else(|| Error::Io(format!("cannot register clipboard format '{name}'")))
    }

    fn check_cap(len: usize, max_bytes: u64) -> Result<()> {
        if len as u64 > max_bytes {
            return Err(Error::TooLarge(format!(
                "clipboard payload {len} bytes over cap {max_bytes}"
            )));
        }
        Ok(())
    }

    /// `image/png`: the "PNG" format when it holds a real PNG, else a
    /// bitmap converted to PNG (`CF_DIBV5` first: it states its alpha).
    pub(crate) fn read_png(max_bytes: u64) -> Result<Vec<u8>> {
        let png_id = register(PNG_FORMAT)?;
        if raw::is_format_avail(png_id) {
            match read_one(png_id, "image/png", max_bytes) {
                Ok(png) if png.starts_with(PNG_SIGNATURE) => return Ok(png),
                Err(e @ Error::TooLarge(_)) => return Err(e),
                _ => {}
            }
        }
        super::read_dib_candidates([CF_DIBV5, CF_DIB], max_bytes, |id| {
            if !raw::is_format_avail(id) {
                return None;
            }
            // Checked before reading: a bitmap can be hundreds of MB.
            if raw::size(id).is_some_and(|n| n.get() as u64 > crate::dib::MAX_DIB_BYTES) {
                return Some(Err(Error::TooLarge(
                    "clipboard bitmap over the decode limit".to_string(),
                )));
            }
            // Raw DIB size is limited by the 64 MP native allocation guard,
            // never by the normalized PNG cap: a larger DIB may compress well.
            Some(read_native_bounded(id, crate::dib::MAX_DIB_BYTES))
        })
    }

    /// `text/uri-list` from the `CF_HDROP` file list.
    pub(crate) fn read_uri_list(max_bytes: u64) -> Result<Vec<u8>> {
        // DROPFILES header plus UTF-16 paths; guard before path-vector copies.
        let native_cap = max_bytes.saturating_mul(2).saturating_add(20);
        let len = raw::size(CF_HDROP)
            .ok_or_else(|| Error::Unavailable("file list has no data".to_string()))?;
        check_cap(len.get(), native_cap)?;
        let mut paths = Vec::new();
        raw::get_file_list(&mut paths).map_err(|e| clip_error("read file list", e))?;
        if paths.is_empty() {
            return Err(Error::Unavailable(
                "text/uri-list not available".to_string(),
            ));
        }
        let list = crate::uri::paths_to_uri_list(&paths).into_bytes();
        check_cap(list.len(), max_bytes)?;
        Ok(list)
    }

    /// Raw bytes of the formats in `ids` that the clipboard holds, in
    /// clipboard order, for [`restore`]. `None` when one is over
    /// `max_bytes` or reading outlasts `budget` (owners render delayed
    /// formats on demand, and the paste waits for this).
    pub(crate) fn snapshot(
        ids: &[u32],
        max_bytes: u64,
        budget: std::time::Duration,
    ) -> Option<Vec<(u32, Vec<u8>)>> {
        let started = std::time::Instant::now();
        with_open("snapshot", || {
            let mut saved = Vec::new();
            for id in raw::EnumFormats::new() {
                if !ids.contains(&id) {
                    continue;
                }
                if started.elapsed() > budget
                    || raw::size(id).is_some_and(|n| n.get() as u64 > max_bytes)
                {
                    return Err(Error::TooLarge(
                        "clipboard snapshot over budget".to_string(),
                    ));
                }
                let mut bytes = Vec::new();
                if raw::get_vec(id, &mut bytes).is_ok() {
                    saved.push((id, bytes));
                }
            }
            Ok(saved)
        })
        .ok()
    }

    /// Put a [`snapshot`] back in one atomic write.
    pub(crate) fn restore(saved: &[(u32, Vec<u8>)]) -> Result<()> {
        with_open("restore", || {
            raw::empty().map_err(|e| clip_error("empty", e))?;
            for (id, bytes) in saved {
                raw::set_without_clear(*id, bytes).map_err(|e| clip_error("restore", e))?;
            }
            Ok(())
        })
    }

    /// Check native size while borrowed; copy only after the cap passes.
    /// Every caller holds `with_open`'s exclusive clipboard session.
    pub(crate) fn read_native_bounded(id: u32, max_raw_bytes: u64) -> Result<Vec<u8>> {
        crate::sys::with_clipboard_bytes(id, |bytes| {
            check_cap(bytes.len(), max_raw_bytes)?;
            Ok(bytes.to_vec())
        })
    }

    pub(crate) fn read_one(id: u32, mime: &str, max_bytes: u64) -> Result<Vec<u8>> {
        if mime == "text/plain" && id == CF_UNICODETEXT {
            return crate::sys::with_clipboard_bytes(id, |bytes| {
                super::decode_unicode_bounded(bytes, max_bytes)
            });
        }
        if mime == "text/html" {
            return crate::sys::with_clipboard_bytes(id, |bytes| {
                super::html_payload_bounded(bytes, max_bytes)
            });
        }
        read_native_bounded(id, max_bytes)
    }

    pub(crate) fn write_one(id: u32, mime: &str, bytes: &[u8]) -> Result<()> {
        use clipboard_win::{options::NoClear, raw};
        if mime == "text/plain" || mime == "text/plain;charset=utf-8" {
            let text = std::str::from_utf8(bytes)
                .map_err(|e| Error::Internal(format!("text payload is not UTF-8: {e}")))?;
            return raw::set_string_with(text, NoClear).map_err(|e| clip_error("write text", e));
        }
        if mime == "text/html" {
            if crate::html::parse_cf_html(bytes).is_ok() {
                return raw::set_without_clear(id, bytes).map_err(|e| clip_error("write html", e));
            }
            let fragment = std::str::from_utf8(bytes)
                .map_err(|e| Error::Internal(format!("html payload is not UTF-8: {e}")))?;
            return raw::set_html_with(id, fragment, NoClear)
                .map_err(|e| clip_error("write html", e));
        }
        if mime == "text/uri-list" {
            let text = std::str::from_utf8(bytes)
                .map_err(|e| Error::Internal(format!("uri-list payload is not UTF-8: {e}")))?;
            let paths = crate::uri::uri_list_to_paths(text);
            return raw::set_file_list_with(&paths, NoClear)
                .map_err(|e| clip_error("write uri-list", e));
        }
        raw::set_without_clear(id, bytes).map_err(|e| clip_error("write", e))
    }
}

impl ClipboardTransfer for WinTransfer {
    const AFFINITY: Affinity = Affinity::Worker;

    fn formats(&self) -> Result<Vec<String>> {
        #[cfg(windows)]
        {
            imp::with_open("formats", || {
                let mut mimes = Vec::new();
                for id in clipboard_win::raw::EnumFormats::new() {
                    if let Some(mime) = imp::mime_for_id(id) {
                        if !mimes.contains(&mime) {
                            mimes.push(mime);
                        }
                    }
                }
                Ok(mimes)
            })
        }
        #[cfg(not(windows))]
        {
            Err(Error::Unavailable(
                "clipboard transfer needs a Windows session".to_string(),
            ))
        }
    }

    fn read(&self, mime: &str, max_bytes: u64) -> Result<Vec<u8>> {
        #[cfg(windows)]
        {
            imp::with_open("read", || {
                if mime == "text/plain" || mime == "text/plain;charset=utf-8" {
                    return read_text_candidates(|id| imp::read_one(id, "text/plain", max_bytes));
                }
                if mime == "image/png" {
                    return imp::read_png(max_bytes);
                }
                if mime == "text/uri-list" {
                    return imp::read_uri_list(max_bytes);
                }
                let id = imp::id_for_mime(mime)?;
                imp::read_one(id, mime, max_bytes)
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (mime, max_bytes);
            Err(Error::Unavailable(
                "clipboard transfer needs a Windows session".to_string(),
            ))
        }
    }

    fn write(&self, payload: &PayloadMap) -> Result<()> {
        self.write(payload)
    }

    fn unchanged_indicator(&self) -> Option<String> {
        #[cfg(windows)]
        {
            clipboard_win::raw::seq_num().map(|n| n.get().to_string())
        }
        #[cfg(not(windows))]
        {
            None
        }
    }
}

#[cfg(test)]
mod bounded_tests {
    use super::*;

    #[test]
    fn typed_text_oversize_is_not_lost_to_unavailable_or_ansi_fallback() {
        let mut calls = Vec::new();
        let result = read_text_candidates(|id| {
            calls.push(id);
            Err(Error::TooLarge("synthetic".to_string()))
        });
        assert!(matches!(result, Err(Error::TooLarge(_))));
        assert_eq!(calls, [13]);
    }

    #[test]
    fn html_native_envelope_larger_than_cap_keeps_small_valid_fragment() {
        let raw = crate::html::build_cf_html("hello");
        assert!(raw.len() > 5);
        assert_eq!(html_payload_bounded(&raw, 5).unwrap(), b"hello");
        assert!(matches!(
            html_payload_bounded(&raw, 4),
            Err(Error::TooLarge(_))
        ));
    }

    #[test]
    fn unicode_expansion_respects_normalized_cap() {
        let raw = [0xac, 0x20, 0, 0]; // EURO SIGN: 2 native bytes, 3 UTF-8 bytes
        assert!(matches!(
            decode_unicode_bounded(&raw, 2),
            Err(Error::TooLarge(_))
        ));
        assert_eq!(decode_unicode_bounded(&raw, 3).unwrap(), "€".as_bytes());
        assert_eq!(decode_unicode_bounded(&[65, 0, 0, 0], 1).unwrap(), b"A");
    }

    #[test]
    fn dib_dimension_guard_maps_to_too_large_not_unsupported() {
        let dib = dib_header(5213, 12277);
        assert!((dib.len() as u64) < crate::dib::MAX_DIB_BYTES);
        let classified = crate::dib::dib_to_png_classified(&dib);
        assert!(matches!(
            classified,
            Err(crate::dib::DibDecodeError::Limit(_))
        ));
        let mapped = read_dib_candidates([17, 8], 1024, |id| (id == 17).then(|| Ok(dib.clone())));
        assert!(matches!(mapped, Err(Error::TooLarge(_))));

        let garbage = b"not a bitmap";
        let mapped2 =
            read_dib_candidates([17, 8], 1024, |id| (id == 17).then(|| Ok(garbage.to_vec())));
        assert!(matches!(mapped2, Err(Error::Unsupported(_))));
    }

    #[test]
    fn dib_too_large_does_not_fallback_to_next_format() {
        let over = dib_header(8000, 8001);
        let fallback = solid_dib(1, 1);
        let mut calls = Vec::new();
        let result = read_dib_candidates([17, 8], 1024, |id| {
            calls.push(id);
            Some(Ok(if id == 17 {
                over.clone()
            } else {
                fallback.clone()
            }))
        });
        assert!(matches!(result, Err(Error::TooLarge(_))));
        assert_eq!(calls, [17], "must not read fallback after TooLarge");
    }

    fn dib_header(width: i32, height: i32) -> Vec<u8> {
        let mut dib = Vec::new();
        dib.extend_from_slice(&40_u32.to_le_bytes());
        dib.extend_from_slice(&width.to_le_bytes());
        dib.extend_from_slice(&height.to_le_bytes());
        dib.extend_from_slice(&1_u16.to_le_bytes());
        dib.extend_from_slice(&32_u16.to_le_bytes());
        dib.extend_from_slice(&[0; 24]);
        dib
    }

    fn solid_dib(width: i32, height: i32) -> Vec<u8> {
        let mut dib = dib_header(width, height);
        for _ in 0..width * height {
            dib.extend_from_slice(&[10, 20, 30, 255]);
        }
        dib
    }

    #[test]
    fn malformed_dib_falls_back_to_valid_wide_dib() {
        let wide = solid_dib(9000, 1);
        let mut calls = Vec::new();
        let png = read_dib_candidates([17, 8], 100_000, |id| {
            calls.push(id);
            Some(Ok(if id == 17 {
                b"not a bitmap".to_vec()
            } else {
                wide.clone()
            }))
        })
        .unwrap();
        assert_eq!(calls, [17, 8]);
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (9000, 1));
        assert_eq!(decoded.get_pixel(8999, 0).0, [30, 20, 10, 255]);
    }

    #[test]
    fn raw_dib_above_png_cap_is_accepted_when_encoded_png_fits() {
        let dib = solid_dib(128, 128);
        let cap = 4096;
        assert!(dib.len() > cap);
        assert!((dib.len() as u64) < crate::dib::MAX_DIB_BYTES);
        let png = read_dib_candidates([17, 8], cap as u64, |id| {
            (id == 17).then(|| Ok(dib.clone()))
        })
        .unwrap();
        assert!(png.len() < cap);
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (128, 128));
        assert_eq!(decoded.get_pixel(0, 0).0, [30, 20, 10, 255]);
        assert!(matches!(
            read_dib_candidates([17, 8], 1, |id| { (id == 17).then(|| Ok(dib.clone())) }),
            Err(Error::TooLarge(_))
        ));
    }
}

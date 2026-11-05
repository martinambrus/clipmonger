//! Synchronous clipboard data transfer over direct Win32 clipboard APIs.
//!
//! Writes are one `OpenClipboard` / `EmptyClipboard` / `SetClipboardData*n` /
//! `CloseClipboard` sequence. This excludes concurrent writers but does not
//! roll back partial errors. Opening retries 5x20 ms; exhaustion
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

/// Project a native snapshot with the same bounded conversions used by capture.
#[cfg(test)]
pub(crate) fn project_saved_snapshot(
    saved: &[(u32, Vec<u8>)],
    name_for_id: impl Fn(u32) -> Option<String>,
) -> Option<PayloadMap> {
    project_saved_snapshot_filtered(saved, name_for_id, 16 * 1024 * 1024, |_| true)
}

#[cfg(any(windows, test))]
pub(crate) fn ansi_snapshot_file_list(
    saved: &[(u32, Vec<u8>)],
    cap: u64,
    within_budget: impl Fn() -> bool,
    name_for_id: impl Fn(u32) -> Option<String>,
    read: impl FnOnce(u64) -> Result<Vec<u8>>,
) -> Option<Vec<u8>> {
    let raw = saved.iter().find(|(id, _)| *id == 15)?.1.as_slice();
    if raw.len() < 20 || u32::from_le_bytes(raw[16..20].try_into().ok()?) != 0 || !within_budget() {
        return None;
    }
    for (id, bytes) in saved {
        if *id >= 0xc000 {
            let name = name_for_id(*id)?;
            if crate::optout::is_optout_name(&name) {
                let value = crate::optout::decode_marker(bytes)?;
                if crate::optout::should_skip(&[(name, Some(value))]) {
                    return None;
                }
            }
        }
    }
    let list = read(cap).ok()?;
    (within_budget() && list.len() as u64 <= cap).then_some(list)
}

/// Project a saved native snapshot under the active capture policy.
#[cfg(any(windows, test))]
#[must_use]
pub fn project_saved_snapshot_filtered(
    saved: &[(u32, Vec<u8>)],
    name_for_id: impl Fn(u32) -> Option<String>,
    cap: u64,
    wanted: impl Fn(&str) -> bool,
) -> Option<PayloadMap> {
    project_saved_snapshot_with_file(saved, name_for_id, cap, wanted, None)
}

/// Project a snapshot with an optional file list captured under its native lock.
#[cfg(any(windows, test))]
#[must_use]
pub fn project_saved_snapshot_with_file(
    saved: &[(u32, Vec<u8>)],
    name_for_id: impl Fn(u32) -> Option<String>,
    cap: u64,
    wanted: impl Fn(&str) -> bool,
    saved_file_list: Option<&[u8]>,
) -> Option<PayloadMap> {
    let mut out = Vec::new();
    let mut bitmap: Option<&[u8]> = None;
    let mut png: Option<&[u8]> = None;
    for (id, bytes) in saved {
        if *id >= 0xc000 {
            let name = name_for_id(*id)?;
            if crate::optout::is_optout_name(&name) {
                let value = crate::optout::decode_marker(bytes)?;
                if crate::optout::should_skip(&[(name, Some(value))]) {
                    return None;
                }
            }
        }
    }
    for (id, bytes) in saved {
        let entry = match *id {
            13 if wanted("text/plain") => {
                Some(("text/plain", decode_unicode_bounded(bytes, cap).ok()?))
            }
            8 => {
                if wanted("image/png") {
                    bitmap = Some(bytes);
                }
                None
            }
            15 if wanted("text/uri-list") => Some((
                "text/uri-list",
                match saved_file_list {
                    Some(list) if list.len() as u64 <= cap => list.to_vec(),
                    Some(_) => return None,
                    None => crate::uri::dropfiles_to_uri_list(bytes, cap)?,
                },
            )),
            13 | 15 => None,
            _ => match name_for_id(*id)?.as_str() {
                "HTML Format" if wanted("text/html") => {
                    Some(("text/html", html_payload_bounded(bytes, cap).ok()?))
                }
                "Rich Text Format" if wanted("text/rtf") && bytes.len() as u64 <= cap => {
                    Some(("text/rtf", bytes.clone()))
                }
                "Rich Text Format" if wanted("text/rtf") => return None,
                "PNG" => {
                    if wanted("image/png") {
                        png = Some(bytes);
                    }
                    None
                }
                name if crate::optout::is_optout_name(name) => None,
                name if !wanted(&format!("x-os/{name}")) => None,
                _ => return None,
            },
        };
        if let Some((mime, bytes)) = entry {
            if out.iter().any(|(name, _)| name == mime) {
                return None;
            }
            out.push((mime.to_string(), bytes));
        }
    }
    if png.is_some() || bitmap.is_some() {
        let image = match png.filter(|bytes| bytes.starts_with(b"\x89PNG\r\n\x1a\n")) {
            Some(bytes) if bytes.len() as u64 <= cap => bytes.to_vec(),
            Some(_) => return None,
            None => {
                let bytes = bitmap?;
                if bytes.len() as u64 > crate::dib::MAX_DIB_BYTES {
                    return None;
                }
                crate::dib::dib_to_png(bytes).ok()?
            }
        };
        if image.len() as u64 > cap {
            return None;
        }
        out.push(("image/png".to_string(), image));
    }
    (!out.is_empty()).then_some(out)
}

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

/// Project portable writes using the same format conversions as capture.
/// Returns `None` when a complete representation cannot be established.
#[must_use]
pub fn project_write_payloads(
    payloads: &[(String, Vec<u8>)],
    max_bytes: u64,
) -> Option<PayloadMap> {
    project_write_payloads_filtered(payloads, max_bytes, |_| true)
}

/// Project only the formats selected by the capture policy.
#[must_use]
pub fn project_write_payloads_filtered(
    payloads: &[(String, Vec<u8>)],
    max_bytes: u64,
    wanted: impl Fn(&str) -> bool,
) -> Option<PayloadMap> {
    // File-list writes also emit a native hint, which capture cannot suppress.
    if wanted("x-os/Preferred DropEffect")
        && payloads.iter().any(|(mime, _)| mime == "text/uri-list")
    {
        return None;
    }
    let mut out: PayloadMap = Vec::new();
    for (mime, bytes) in payloads {
        if let Some(name) = mime
            .strip_prefix("x-os/")
            .filter(|name| crate::optout::is_optout_name(name))
        {
            let value = crate::optout::decode_marker(bytes)?;
            if crate::optout::should_skip(&[(name.to_string(), Some(value))]) {
                return None;
            }
            continue;
        }
        let canonical = match mime.as_str() {
            "text/plain;charset=utf-8" => "text/plain",
            other => other,
        };
        if !wanted(canonical) {
            continue;
        }
        let (canonical, projected) = match mime.as_str() {
            "text/plain" | "text/plain;charset=utf-8" => {
                if bytes.contains(&0) || std::str::from_utf8(bytes).is_err() {
                    return None;
                }
                ("text/plain", bytes.clone())
            }
            "text/html" => {
                if crate::html::parse_cf_html(bytes).is_err() && std::str::from_utf8(bytes).is_err()
                {
                    return None;
                }
                ("text/html", html_payload_bounded(bytes, max_bytes).ok()?)
            }
            "text/rtf" => ("text/rtf", bytes.clone()),
            "image/png" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => ("image/png", bytes.clone()),
            "text/uri-list" => {
                if bytes.len() as u64 > max_bytes.saturating_mul(3).saturating_add(1024) {
                    return None;
                }
                let paths = crate::uri::uri_list_to_paths(std::str::from_utf8(bytes).ok()?);
                if paths.is_empty() {
                    return None;
                }
                (
                    "text/uri-list",
                    crate::uri::paths_to_uri_list(&paths).into_bytes(),
                )
            }
            _ => return None,
        };
        if projected.len() as u64 > max_bytes || out.iter().any(|(name, _)| name == canonical) {
            return None;
        }
        out.push((canonical.to_string(), projected));
    }
    (!out.is_empty()).then_some(out)
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

/// Bitmap formats tried for `image/png`, best first: `CF_DIBV5` states its
/// alpha, `CF_DIB` is the classic fallback.
#[cfg(any(windows, test))]
const DIB_CANDIDATES: [u32; 2] = [17, 8];

/// One raw clipboard copy with the clipboard open (closed again on return).
#[cfg(any(windows, test))]
type OpenRead<'a> =
    &'a dyn Fn(&mut dyn FnMut() -> Result<Option<Vec<u8>>>) -> Result<Option<Vec<u8>>>;

/// `image/png` with the clipboard open only while raw bytes are copied.
/// `open` runs one copy with the clipboard open (for capture also its
/// sequence/owner guard), `png_raw` copies a registered PNG, `dib_raw` one
/// bitmap format. Converting a large bitmap to PNG can take a second, and no
/// other app can open the clipboard meanwhile (an immediate Ctrl+V fails),
/// so the conversion runs only after `open` closed the clipboard.
#[cfg(any(windows, test))]
fn read_png_outside(
    max_bytes: u64,
    open: OpenRead<'_>,
    png_raw: &dyn Fn() -> Result<Option<Vec<u8>>>,
    dib_raw: &dyn Fn(u32) -> Result<Option<Vec<u8>>>,
) -> Result<Vec<u8>> {
    if let Some(png) = open(&mut || png_raw())? {
        return Ok(png);
    }
    read_dib_candidates(DIB_CANDIDATES, max_bytes, |id| {
        open(&mut || dib_raw(id)).transpose()
    })
}

/// Direct-OS clipboard transfer (seg-04 S2).
pub struct WinTransfer;

impl WinTransfer {
    /// Infallible constructor (no session handle needed up front).
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Multi-format exclusive write of any payload slice; errors may leave partial data.
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
        self.write_with_sequence(payload, |_| {})
    }

    /// Write and observe the resulting sequence before releasing the clipboard.
    ///
    /// # Errors
    /// Returns the same errors as [`Self::write`].
    pub fn write_with_sequence(
        &self,
        payload: &[(String, Vec<u8>)],
        done: impl FnOnce(Option<u32>),
    ) -> Result<()> {
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
                done(clipboard_win::raw::seq_num().map(std::num::NonZeroU32::get));
                Ok(())
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (payload, done);
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

#[cfg(all(not(windows), test))]
pub(crate) mod imp {
    use super::{Error, Result};
    pub(crate) const CF_DIB: u32 = 8;
    pub(crate) const CF_HDROP: u32 = 15;
    pub(crate) const CF_UNICODETEXT: u32 = 13;
    pub(crate) const PNG_FORMAT: &str = "PNG";
    pub(crate) const DROP_EFFECT_FORMAT: &str = "Preferred DropEffect";
    pub(crate) const DROP_EFFECT_MIME: &str = "x-os/Preferred DropEffect";
    pub(crate) const DROPEFFECT_COPY: u32 = 1;
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
    #[allow(clippy::cast_possible_truncation, clippy::unnecessary_wraps)]
    pub(crate) fn register(name: &str) -> Result<u32> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        name.hash(&mut h);
        let id = (h.finish() as u32) | 0xC000;
        if id == 0 {
            Ok(0xC001)
        } else {
            Ok(id)
        }
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

    pub(crate) fn format_name(id: u32) -> Option<String> {
        raw::format_name_big(id)
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

    /// `image/png` for a caller that already holds the clipboard open: the
    /// "PNG" format when it holds a real PNG, else a bitmap converted to PNG.
    /// Readers that open the clipboard themselves use `read_png_outside`, so
    /// the conversion runs with the clipboard closed.
    pub(crate) fn read_png(max_bytes: u64) -> Result<Vec<u8>> {
        super::read_png_outside(max_bytes, &|body| body(), &|| png_raw(max_bytes), &dib_raw)
    }

    /// A registered "PNG" format holding a real PNG (clipboard open).
    pub(crate) fn png_raw(max_bytes: u64) -> Result<Option<Vec<u8>>> {
        let png_id = register(PNG_FORMAT)?;
        if !raw::is_format_avail(png_id) {
            return Ok(None);
        }
        match read_one(png_id, "image/png", max_bytes) {
            Ok(png) if png.starts_with(PNG_SIGNATURE) => Ok(Some(png)),
            Err(e @ Error::TooLarge(_)) => Err(e),
            _ => Ok(None),
        }
    }

    /// The raw bytes of one bitmap format (clipboard open).
    pub(crate) fn dib_raw(id: u32) -> Result<Option<Vec<u8>>> {
        if !raw::is_format_avail(id) {
            return Ok(None);
        }
        // Checked before reading: a bitmap can be hundreds of MB.
        if raw::size(id).is_some_and(|n| n.get() as u64 > crate::dib::MAX_DIB_BYTES) {
            return Err(Error::TooLarge(
                "clipboard bitmap over the decode limit".to_string(),
            ));
        }
        // Raw DIB size is limited by the 64 MP native allocation guard,
        // never by the normalized PNG cap: a larger DIB may compress well.
        read_native_bounded(id, crate::dib::MAX_DIB_BYTES).map(Some)
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
        let native = super::encode_native(mime, bytes)?;
        raw::set_without_clear(id, &native).map_err(|e| clip_error("write", e))
    }
}

/// Prepare bytes for the native format before destructive swap publication.
/// Raw snapshots deliberately bypass this canonical MIME conversion.
#[cfg(any(windows, test))]
pub(crate) fn encode_native(mime: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    let utf8 = || {
        std::str::from_utf8(bytes)
            .map_err(|e| Error::Internal(format!("{mime} payload is not UTF-8: {e}")))
    };
    match mime {
        "text/plain" | "text/plain;charset=utf-8" => Ok(utf8()?
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect()),
        "text/html" if crate::html::parse_cf_html(bytes).is_err() => {
            Ok(crate::html::build_cf_html(utf8()?))
        }
        "text/uri-list" => {
            let paths = crate::uri::uri_list_to_paths(utf8()?);
            if paths.is_empty() {
                return Err(Error::Unsupported(
                    "text/uri-list names no Windows file".into(),
                ));
            }
            // DROPFILES: pFiles=20, POINT=(0,0), fNC=0, fWide=TRUE.
            let mut out = [20_u32, 0, 0, 0, 1]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>();
            for path in paths {
                out.extend(
                    path.encode_utf16()
                        .chain(std::iter::once(0))
                        .flat_map(u16::to_le_bytes),
                );
            }
            out.extend([0, 0]);
            Ok(out)
        }
        _ => Ok(bytes.to_vec()),
    }
}

#[cfg(windows)]
fn read_open(mime: &str, max_bytes: u64) -> Result<Vec<u8>> {
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
}

/// Transfer used only by native capture, bound to its observed sequence.
pub struct WinCaptureTransfer {
    sequence: Option<u32>,
    policy: crate::capture_policy::CapturePolicy,
}
impl WinCaptureTransfer {
    #[must_use]
    pub fn new(sequence: Option<u32>, policy: crate::capture_policy::CapturePolicy) -> Self {
        Self { sequence, policy }
    }
}
impl ClipboardTransfer for WinCaptureTransfer {
    const AFFINITY: Affinity = Affinity::Worker;
    fn formats(&self) -> Result<Vec<String>> {
        Err(Error::Unsupported("capture format enumeration".into()))
    }
    fn read(&self, mime: &str, max_bytes: u64) -> Result<Vec<u8>> {
        #[cfg(windows)]
        {
            if mime == "image/png" {
                // Each raw copy holds the clipboard (and passes the guard);
                // the bitmap conversion runs after it is closed again.
                let open = |body: &mut dyn FnMut() -> Result<Option<Vec<u8>>>| {
                    imp::with_open("capture read", || {
                        let actual = clipboard_win::raw::seq_num().map(std::num::NonZeroU32::get);
                        let entries = (self.policy)();
                        let owner = crate::sys::clipboard_owner_executable();
                        match crate::capture_policy::guarded(
                            self.sequence,
                            actual,
                            owner.as_deref(),
                            entries.as_deref(),
                            body,
                        ) {
                            Ok(result) => result,
                            Err(crate::capture_policy::Admission::OwnerDenied) => {
                                Err(Error::CaptureExcluded)
                            }
                            Err(_) => Err(Error::CaptureChanged),
                        }
                    })
                };
                return read_png_outside(
                    max_bytes,
                    &open,
                    &|| imp::png_raw(max_bytes),
                    &imp::dib_raw,
                );
            }
            imp::with_open("capture read", || {
                let actual = clipboard_win::raw::seq_num().map(std::num::NonZeroU32::get);
                let entries = (self.policy)();
                let owner = crate::sys::clipboard_owner_executable();
                match crate::capture_policy::guarded(
                    self.sequence,
                    actual,
                    owner.as_deref(),
                    entries.as_deref(),
                    || read_open(mime, max_bytes),
                ) {
                    Ok(result) => result,
                    Err(crate::capture_policy::Admission::OwnerDenied) => {
                        Err(Error::CaptureExcluded)
                    }
                    Err(_) => Err(Error::CaptureChanged),
                }
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (mime, max_bytes, self.sequence, &self.policy);
            Err(Error::Unavailable(
                "clipboard transfer needs a Windows session".into(),
            ))
        }
    }
    fn write(&self, _: &PayloadMap) -> Result<()> {
        Err(Error::Unsupported("capture write".into()))
    }
    fn unchanged_indicator(&self) -> Option<String> {
        None
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
            if mime == "image/png" {
                return read_png_outside(
                    max_bytes,
                    &|body| imp::with_open("read", body),
                    &|| imp::png_raw(max_bytes),
                    &imp::dib_raw,
                );
            }
            imp::with_open("read", || read_open(mime, max_bytes))
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
    fn windows_write_projection_matches_file_list_capture() {
        let uri = b"file:///C:/Users/A%20B.txt\r\nfile://server/share/x.txt".to_vec();
        let paths = crate::uri::uri_list_to_paths(std::str::from_utf8(&uri).unwrap());
        let expected = crate::uri::paths_to_uri_list(&paths).into_bytes();
        let payloads = [("text/uri-list".into(), uri)];
        assert_eq!(
            project_write_payloads_filtered(&payloads, 1024, |mime| mime == "text/uri-list"),
            Some(vec![("text/uri-list".into(), expected)])
        );
        assert!(project_write_payloads_filtered(&payloads, 1024, |mime| {
            matches!(mime, "text/uri-list" | "x-os/Preferred DropEffect")
        })
        .is_none());
        assert!(project_write_payloads(&payloads, 1024).is_none());
    }

    #[test]
    fn native_file_restore_projection_matches_capture_paths() {
        let mut raw = vec![0u8; 20];
        raw[0..4].copy_from_slice(&20u32.to_le_bytes());
        raw[16..20].copy_from_slice(&1u32.to_le_bytes());
        for unit in "C:\\Users\\A B.txt\0\\\\server\\share\\x.txt\0\0".encode_utf16() {
            raw.extend_from_slice(&unit.to_le_bytes());
        }
        let expected = crate::uri::paths_to_uri_list(&[
            r"C:\Users\A B.txt".into(),
            r"\\server\share\x.txt".into(),
        ])
        .into_bytes();
        assert_eq!(
            project_saved_snapshot(&[(15, raw)], |_| None),
            Some(vec![("text/uri-list".into(), expected)])
        );
    }

    #[test]
    fn native_restore_ignores_unselected_drop_effect() {
        let saved = [(13, vec![b'h', 0, b'i', 0, 0, 0]), (99, vec![1, 0, 0, 0])];
        assert_eq!(
            project_saved_snapshot_filtered(
                &saved,
                |id| (id == 99).then(|| "Preferred DropEffect".into()),
                1024,
                |mime| mime == "text/plain"
            ),
            Some(vec![("text/plain".into(), b"hi".to_vec())])
        );
    }

    #[test]
    fn projection_applies_selected_formats_before_caps() {
        let size = 16 * 1024 * 1024 + 1;
        let text = vec![b'h', 0, b'i', 0, 0, 0];
        let html = vec![b'a'; size];
        let saved = [(13, text), (100, html.clone())];
        let name = |id| (id == 100).then(|| "HTML Format".to_string());
        assert_eq!(
            project_saved_snapshot_filtered(&saved, name, 10, |mime| mime == "text/plain"),
            Some(vec![("text/plain".into(), b"hi".to_vec())])
        );
        let projected =
            project_saved_snapshot_filtered(&saved, name, size as u64, |mime| mime == "text/html")
                .unwrap();
        assert_eq!(projected, vec![("text/html".into(), html.clone())]);
        let written = [
            ("text/plain".to_string(), b"hi".to_vec()),
            ("text/html".to_string(), html),
        ];
        assert_eq!(
            project_write_payloads_filtered(&written, 10, |mime| mime == "text/plain"),
            Some(vec![("text/plain".into(), b"hi".to_vec())])
        );
    }

    #[test]
    fn dropfiles_rejects_truncation_invalid_offsets_narrow_and_oversize() {
        assert!(crate::uri::dropfiles_to_uri_list(&[0; 19], 100).is_none());
        let mut raw = vec![0; 20];
        raw[0..4].copy_from_slice(&20u32.to_le_bytes());
        raw[16..20].copy_from_slice(&1u32.to_le_bytes());
        for unit in "C:\\x\0\0".encode_utf16() {
            raw.extend_from_slice(&unit.to_le_bytes());
        }
        assert!(crate::uri::dropfiles_to_uri_list(&raw, 100).is_some());
        assert!(crate::uri::dropfiles_to_uri_list(&raw[..raw.len() - 2], 100).is_none());
        assert!(crate::uri::dropfiles_to_uri_list(&raw, 1).is_none());
        raw[0..4].copy_from_slice(&19u32.to_le_bytes());
        assert!(crate::uri::dropfiles_to_uri_list(&raw, 100).is_none());
        raw[0..4].copy_from_slice(&20u32.to_le_bytes());
        raw[16..20].copy_from_slice(&0u32.to_le_bytes());
        assert!(crate::uri::dropfiles_to_uri_list(&raw, 100).is_none());
    }

    #[test]
    fn ansi_snapshot_uses_bounded_reader_only_for_admitted_snapshot() {
        use std::cell::Cell;
        let mut raw = vec![0; 20];
        raw[0..4].copy_from_slice(&20u32.to_le_bytes());
        let saved = [(15, raw)];
        let calls = Cell::new(0);
        let observed = ansi_snapshot_file_list(
            &saved,
            64,
            || true,
            |_| None,
            |cap| {
                assert_eq!(cap, 64);
                calls.set(calls.get() + 1);
                Ok(b"file:///C:/a".to_vec())
            },
        )
        .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(
            project_saved_snapshot_with_file(
                &saved,
                |_| None,
                64,
                |mime| mime == "text/uri-list",
                Some(&observed)
            ),
            Some(vec![("text/uri-list".into(), observed)])
        );
        assert!(ansi_snapshot_file_list(
            &saved,
            64,
            || false,
            |_| None,
            |_| panic!("expired budget must skip native read")
        )
        .is_none());
        let checks = Cell::new(0);
        assert!(ansi_snapshot_file_list(
            &saved,
            64,
            || {
                checks.set(checks.get() + 1);
                checks.get() == 1
            },
            |_| None,
            |_| Ok(b"file:///C:/a".to_vec())
        )
        .is_none());
        assert!(project_saved_snapshot_with_file(
            &saved,
            |_| None,
            1,
            |mime| mime == "text/uri-list",
            Some(b"file:///C:/a")
        )
        .is_none());
        let mut sensitive = saved.to_vec();
        sensitive.push((0xc001, 0u32.to_le_bytes().to_vec()));
        assert!(ansi_snapshot_file_list(
            &sensitive,
            64,
            || true,
            |id| (id == 0xc001).then(|| crate::optout::HISTORY_OPT_OUT.into()),
            |_| panic!("privacy must skip native read")
        )
        .is_none());
    }

    #[test]
    fn snapshot_privacy_markers_precede_format_filtering() {
        let saved = [
            (13, vec![b'h', 0, b'i', 0, 0, 0]),
            (0xc001, 0u32.to_le_bytes().to_vec()),
        ];
        let name = |id| (id == 0xc001).then(|| crate::optout::HISTORY_OPT_OUT.to_string());
        assert!(
            project_saved_snapshot_filtered(&saved, name, 1024, |mime| mime == "text/plain")
                .is_none()
        );
        let opted_in = [
            (13, vec![b'h', 0, b'i', 0, 0, 0]),
            (0xc001, 1u32.to_le_bytes().to_vec()),
        ];
        assert_eq!(
            project_saved_snapshot_filtered(&opted_in, name, 1024, |mime| mime == "text/plain"),
            Some(vec![("text/plain".into(), b"hi".to_vec())])
        );
    }

    #[test]
    fn write_projection_matches_capture_text_and_html_conversions() {
        let payloads = vec![
            ("text/plain;charset=utf-8".into(), b"hello".to_vec()),
            ("text/html".into(), crate::html::build_cf_html("<b>hi</b>")),
        ];
        assert_eq!(
            project_write_payloads(&payloads, 100),
            Some(vec![
                ("text/plain".into(), b"hello".to_vec()),
                ("text/html".into(), b"<b>hi</b>".to_vec()),
            ])
        );
        assert_eq!(
            project_write_payloads(&[("text/uri-list".into(), b"file:///x".to_vec())], 100),
            None
        );
    }

    #[test]
    fn restore_projection_uses_capture_decoders_and_rejects_unknown_formats() {
        let raw = vec![b'h', 0, b'i', 0, 0, 0];
        assert_eq!(
            project_saved_snapshot(&[(13, raw.clone())], |_| None),
            Some(vec![("text/plain".into(), b"hi".to_vec())])
        );
        let mixed = vec![
            (13, raw.clone()),
            (100, crate::html::build_cf_html("<b>hi</b>")),
            (101, b"{\\rtf1 hi}".to_vec()),
        ];
        assert_eq!(
            project_saved_snapshot(&mixed, |id| match id {
                100 => Some("HTML Format".into()),
                101 => Some("Rich Text Format".into()),
                _ => None,
            }),
            Some(vec![
                ("text/plain".into(), b"hi".to_vec()),
                ("text/html".into(), b"<b>hi</b>".to_vec()),
                ("text/rtf".into(), b"{\\rtf1 hi}".to_vec())
            ])
        );
        assert_eq!(
            project_saved_snapshot(&[(13, raw), (15, vec![1, 2, 3])], |_| None),
            None
        );
        assert_eq!(project_saved_snapshot(&[], |_| None), None);
    }

    #[test]
    fn restored_image_projection_uses_capture_png_precedence_and_dib_decoder() {
        let dib = solid_dib(1, 1);
        let expected =
            read_dib_candidates([17, 8], 1024, |id| (id == 8).then(|| Ok(dib.clone()))).unwrap();
        assert_eq!(
            project_saved_snapshot(&[(8, dib.clone())], |_| None),
            Some(vec![("image/png".into(), expected)])
        );
        let png = b"\x89PNG\r\n\x1a\nsynthetic".to_vec();
        assert_eq!(
            project_saved_snapshot(&[(8, dib), (100, png.clone())], |id| (id == 100)
                .then(|| "PNG".into())),
            Some(vec![("image/png".into(), png)])
        );
    }

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

    /// Model of the clipboard lock for `read_png_outside`: `open` marks it
    /// held while one raw copy runs; every raw read must see it held.
    fn png_outside_rig(
        png: Option<&[u8]>,
        dibs: &[(u32, Vec<u8>)],
    ) -> (super::Result<Vec<u8>>, usize) {
        let held = std::cell::Cell::new(false);
        let opens = std::cell::Cell::new(0);
        let open = |body: &mut dyn FnMut() -> super::Result<Option<Vec<u8>>>| {
            assert!(!held.get(), "opens never nest");
            held.set(true);
            opens.set(opens.get() + 1);
            let result = body();
            held.set(false);
            result
        };
        let result = read_png_outside(
            1 << 30,
            &open,
            &|| {
                assert!(held.get(), "PNG copied with the clipboard open");
                Ok(png.map(<[u8]>::to_vec))
            },
            &|id| {
                assert!(held.get(), "bitmap copied with the clipboard open");
                Ok(dibs
                    .iter()
                    .find(|(dib_id, _)| *dib_id == id)
                    .map(|(_, b)| b.clone()))
            },
        );
        (result, opens.get())
    }

    #[test]
    fn png_read_converts_bitmaps_with_the_clipboard_closed() {
        let real_png = b"\x89PNG\r\n\x1a\nrest".to_vec();
        let (result, opens) = png_outside_rig(Some(&real_png), &[]);
        assert_eq!(result.unwrap(), real_png, "a registered PNG wins");
        assert_eq!(opens, 1);

        let (result, opens) = png_outside_rig(None, &[(17, solid_dib(2, 2))]);
        let decoded = image::load_from_memory(&result.unwrap())
            .unwrap()
            .to_rgba8();
        assert_eq!(decoded.dimensions(), (2, 2));
        assert_eq!(
            opens, 2,
            "PNG probe, then one CF_DIBV5 copy; the conversion holds nothing"
        );

        let (result, opens) = png_outside_rig(None, &[(17, vec![1, 2, 3]), (8, solid_dib(1, 1))]);
        assert!(result.is_ok(), "a malformed CF_DIBV5 falls back to CF_DIB");
        assert_eq!(opens, 3, "the fallback copy opens the clipboard again");

        let (result, _) = png_outside_rig(None, &[]);
        assert!(result.is_err(), "no image format");
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

#[cfg(test)]
mod native_encoding_tests {
    use super::encode_native;

    #[test]
    fn native_html_offsets_delimit_context_and_multibyte_fragment() {
        for fragment in ["<p>héllo 🌍 日本語</p>", "<b>native</b>", ""] {
            let bytes = encode_native("text/html", fragment.as_bytes()).unwrap();
            let context_start = bytes
                .windows(b"<html>".len())
                .position(|part| part == b"<html>")
                .unwrap();
            let header = std::str::from_utf8(&bytes[..context_start]).unwrap();
            let offset = |key: &str| -> usize {
                let field = header
                    .lines()
                    .find_map(|line| line.strip_prefix(key))
                    .unwrap();
                assert!(field.bytes().all(|byte| byte.is_ascii_digit()));
                field.parse().unwrap()
            };
            let start_html = offset("StartHTML:");
            let end_html = offset("EndHTML:");
            let start_fragment = offset("StartFragment:");
            let end_fragment = offset("EndFragment:");
            assert_eq!(start_html, context_start);
            assert_eq!(end_html, bytes.len());
            assert_eq!(
                &bytes[start_html..start_fragment],
                b"<html>\r\n<body>\r\n<!--StartFragment-->"
            );
            assert_eq!(&bytes[start_fragment..end_fragment], fragment.as_bytes());
            assert_eq!(
                &bytes[end_fragment..end_html],
                b"<!--EndFragment-->\r\n</body>\r\n</html>"
            );
            // The already-framed publication route must remain byte-for-byte raw.
            assert_eq!(encode_native("text/html", &bytes).unwrap(), bytes);
        }
    }
}

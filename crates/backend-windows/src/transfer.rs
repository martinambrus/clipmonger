//! Synchronous clipboard data transfer over direct Win32 clipboard APIs.
//!
//! Writes are one `OpenClipboard` / `EmptyClipboard` / `SetClipboardData*n` /
//! `CloseClipboard` sequence (atomic). Opening retries 5x20 ms; exhaustion
//! maps to `Error::Timeout` (the clipboard is a shared lock).

use clipmonger_platform::traits::ClipboardTransfer;
use clipmonger_platform::types::{Affinity, Error, PayloadMap, Result};

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
    /// Returns [`Error::Unsupported`] for unknown formats (before touching
    /// the clipboard), [`Error::Timeout`] when the shared lock cannot be
    /// opened, or [`Error::Unavailable`] without a Windows session.
    pub fn write(&self, payload: &[(String, Vec<u8>)]) -> Result<()> {
        #[cfg(windows)]
        {
            // Pre-validate every MIME before touching the clipboard so a bad
            // entry fails without clobbering the current content.
            let mut ops: Vec<(u32, &str, &[u8])> = Vec::with_capacity(payload.len());
            for (mime, bytes) in payload {
                let id = imp::id_for_mime(mime)?;
                ops.push((id, mime.as_str(), bytes.as_slice()));
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
    const CF_DIB: u32 = 8;
    const CF_DIBV5: u32 = 17;
    const CF_HDROP: u32 = 15;
    pub(crate) const CF_UNICODETEXT: u32 = 13;

    pub(crate) fn clip_error(op: &str, err: impl std::fmt::Display) -> Error {
        Error::Io(format!("clipboard {op} failed: {err}"))
    }

    /// Run `body` with the clipboard open, retrying the open 5x20 ms.
    pub(crate) fn with_open<T>(op: &str, body: impl FnOnce() -> Result<T>) -> Result<T> {
        let mut attempts = OPEN_ATTEMPTS;
        loop {
            match raw::open() {
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
            CF_UNICODETEXT => Some("text/plain".to_string()),
            CF_TEXT => Some("text/plain".to_string()),
            CF_OEMTEXT => Some("text/plain".to_string()),
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
    pub(crate) fn id_for_mime(mime: &str) -> Result<u32> {
        if mime == "text/plain" {
            return Ok(CF_UNICODETEXT);
        }
        if let Some(windows) = crate::formats::canonical_to_windows(mime) {
            return raw::register_format(windows)
                .map(std::num::NonZeroU32::get)
                .ok_or_else(|| Error::Io(format!("cannot register clipboard format '{windows}'")));
        }
        if let Some(name) = mime.strip_prefix("x-os/") {
            if name.is_empty() {
                return Err(Error::Unsupported(format!("bad format name '{mime}'")));
            }
            return raw::register_format(name)
                .map(std::num::NonZeroU32::get)
                .ok_or_else(|| Error::Io(format!("cannot register clipboard format '{name}'")));
        }
        Err(Error::Unsupported(format!("unsupported format '{mime}'")))
    }

    pub(crate) fn read_one(id: u32, mime: &str, max_bytes: u64) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        if mime == "text/html" {
            raw::get_html(id, &mut out).map_err(|e| clip_error("read html", e))?;
            if let Ok(fragment) = crate::html::parse_cf_html(&out) {
                out = fragment.into_bytes();
            }
        } else if mime == "text/plain" {
            if id == CF_TEXT || id == CF_OEMTEXT {
                raw::get_vec(id, &mut out).map_err(|e| clip_error("read text", e))?;
            } else {
                raw::get_string(&mut out).map_err(|e| clip_error("read text", e))?;
            }
        } else {
            raw::get_vec(id, &mut out).map_err(|e| clip_error("read", e))?;
        }
        if out.len() as u64 > max_bytes {
            return Err(Error::TooLarge(format!(
                "clipboard payload {} bytes over cap {max_bytes}",
                out.len()
            )));
        }
        Ok(out)
    }

    pub(crate) fn write_one(id: u32, mime: &str, bytes: &[u8]) -> Result<()> {
        use clipboard_win::{options::NoClear, raw};
        if mime == "text/plain" {
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
            let paths: Vec<String> = text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| {
                    line.strip_prefix("file://")
                        .unwrap_or(line)
                        .replace('/', "\\")
                })
                .collect();
            let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
            return raw::set_file_list_with(&refs, NoClear)
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
                if mime == "text/plain" {
                    for cand in [imp::CF_UNICODETEXT, imp::CF_TEXT, imp::CF_OEMTEXT] {
                        if let Ok(bytes) = imp::read_one(cand, mime, max_bytes) {
                            if !bytes.is_empty() {
                                return Ok(bytes);
                            }
                        }
                    }
                    return Err(Error::Unavailable("text/plain not available".to_string()));
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

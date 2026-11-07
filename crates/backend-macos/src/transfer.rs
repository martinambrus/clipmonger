//! Clipboard transfer on `NSPasteboard.generalPasteboard` (seg-08 §2).
//!
//! Reads refuse oversized data before copying it out of the `NSData`;
//! writes are one `clearContents` + `writeObjects`, so a write bumps
//! `changeCount` exactly once (the value self-write suppression keys on).

use clipmonger_platform::traits::ClipboardTransfer;
use clipmonger_platform::types::{Affinity, Error, PayloadMap, Result};

use crate::caps::{PasteboardAccess, PASTEBOARD_GUIDANCE};
use crate::mime::{self, Item, Kind, TypeNames};
use crate::os;

/// TIFF is uncompressed: accept this many times the PNG limit before
/// converting, then cap the PNG itself.
const TIFF_FACTOR: u64 = 8;

/// Low 32 bits of a `changeCount` (the monitor's `native_sequence`).
#[must_use]
pub fn sequence_of(change_count: isize) -> u32 {
    let wide = i64::try_from(change_count).unwrap_or_default();
    u32::try_from(wide.rem_euclid(1_i64 << 32)).unwrap_or(0)
}

/// General-pasteboard transfer.
pub struct MacTransfer {
    names: TypeNames,
}

impl MacTransfer {
    /// Bind to the general pasteboard.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] off macOS or without a user session (the
    /// pasteboard is nil in system-daemon contexts).
    pub fn new() -> Result<Self> {
        Ok(Self {
            names: os::type_names()?,
        })
    }

    /// Pasteboard type names in use.
    #[must_use]
    pub fn names(&self) -> &TypeNames {
        &self.names
    }

    /// Current `changeCount`.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] without a pasteboard.
    pub fn change_count(&self) -> Result<isize> {
        os::change_count()
    }

    /// Write and return the `changeCount` our write produced.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] when nothing maps, [`Error::Io`] when
    /// `AppKit` refuses the write.
    pub fn write_counted(&self, payload: &[(String, Vec<u8>)]) -> Result<isize> {
        let plan = mime::plan_write(payload, &self.names, os::png_to_tiff)?;
        os::write(&plan)
    }

    /// Read `mime` only while the pasteboard is still at `sequence` (the
    /// monitor event's `native_sequence`).
    ///
    /// # Errors
    ///
    /// [`Error::CaptureChanged`] when the pasteboard changed before or
    /// during the read; otherwise as [`ClipboardTransfer::read`].
    pub fn read_at(&self, sequence: u32, mime: &str, max_bytes: u64) -> Result<Vec<u8>> {
        if sequence_of(os::change_count()?) != sequence {
            return Err(Error::CaptureChanged);
        }
        let bytes = self.read(mime, max_bytes)?;
        if sequence_of(os::change_count()?) != sequence {
            return Err(Error::CaptureChanged);
        }
        Ok(bytes)
    }

    /// Every item with all its raw types (for restore).
    ///
    /// # Errors
    ///
    /// [`Error::TooLarge`] past `max_total` bytes.
    pub fn snapshot(&self, max_total: u64) -> Result<Vec<Item>> {
        guard_access()?;
        os::snapshot(max_total)
    }

    /// Put a [`Self::snapshot`] back; returns the new `changeCount`.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when `AppKit` refuses the write.
    pub fn restore(&self, items: &[Item]) -> Result<isize> {
        os::restore(items)
    }

    /// Canonical view of a snapshot (what a capture would read back).
    #[must_use]
    pub fn canonical(&self, items: &[Item]) -> PayloadMap {
        mime::canonical_items(items, &self.names, os::tiff_to_png)
    }
}

/// Refuse reads the user denied (macOS 15.4+ pasteboard privacy).
fn guard_access() -> Result<()> {
    if os::access() == PasteboardAccess::Denied {
        return Err(Error::Denied(format!(
            "clipboard access denied: {PASTEBOARD_GUIDANCE}"
        )));
    }
    Ok(())
}

fn missing(mime: &str) -> Error {
    Error::Unsupported(format!("{mime} not on the pasteboard"))
}

fn cap(bytes: Vec<u8>, max: u64) -> Result<Vec<u8>> {
    if bytes.len() as u64 > max {
        return Err(Error::TooLarge(format!(
            "{} bytes, limit {max}",
            bytes.len()
        )));
    }
    Ok(bytes)
}

impl ClipboardTransfer for MacTransfer {
    const AFFINITY: Affinity = Affinity::Worker;

    fn formats(&self) -> Result<Vec<String>> {
        // Type names only: no data access, so no privacy prompt.
        Ok(mime::offers_for(&os::types()?, &self.names))
    }

    fn read(&self, mime: &str, max_bytes: u64) -> Result<Vec<u8>> {
        guard_access()?;
        let names = &self.names;
        match mime::canonical_kind(mime).ok_or_else(|| missing(mime))? {
            Kind::Text => {
                if let Some(bytes) = os::data(&names.string, max_bytes)? {
                    return Ok(bytes);
                }
                // UTF-16-only providers: AppKit converts.
                os::string_utf8(max_bytes)?.ok_or_else(|| missing(mime))
            }
            Kind::Html => os::data(&names.html, max_bytes)?.ok_or_else(|| missing(mime)),
            Kind::Rtf => os::data(&names.rtf, max_bytes)?.ok_or_else(|| missing(mime)),
            Kind::Png => {
                if let Some(bytes) = os::data(&names.png, max_bytes)? {
                    return Ok(bytes);
                }
                let tiff = os::data(&names.tiff, max_bytes.saturating_mul(TIFF_FACTOR))?
                    .ok_or_else(|| missing(mime))?;
                let png = os::tiff_to_png(&tiff)
                    .ok_or_else(|| Error::Internal("TIFF to PNG conversion failed".into()))?;
                cap(png, max_bytes)
            }
            Kind::UriList => {
                let urls = os::file_urls(&names.file_url)?;
                if urls.is_empty() {
                    return Err(missing(mime));
                }
                cap(crate::uri::join(&urls).into_bytes(), max_bytes)
            }
            Kind::Passthrough(uti) => os::data(uti, max_bytes)?.ok_or_else(|| missing(mime)),
        }
    }

    fn write(&self, payload: &PayloadMap) -> Result<()> {
        self.write_counted(payload).map(drop)
    }

    fn unchanged_indicator(&self) -> Option<String> {
        os::change_count().ok().map(|count| count.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_wraps_to_u32() {
        assert_eq!(sequence_of(7), 7);
        assert_eq!(sequence_of(-1), u32::MAX);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(sequence_of((1isize << 32) + 5), 5);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unavailable_off_macos() {
        assert!(matches!(MacTransfer::new(), Err(Error::Unavailable(_))));
    }
}

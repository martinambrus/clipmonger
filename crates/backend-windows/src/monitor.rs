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
#[cfg(any(windows, test))]
use clipmonger_platform::types::CaptureOrigin;
use clipmonger_platform::types::{
    Affinity, CaptureEvent, Error, FormatOffer, Result, SubscribeKind,
};

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
    #[cfg(windows)]
    last_hash: Option<String>,
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
                last_hash: None,
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

#[cfg(any(windows, test))]
const MAX_FINGERPRINT_BYTES: usize = 64 * 1024;
#[cfg(any(windows, test))]
const MARKER_MAX_BYTES: usize = 64;

#[cfg(any(windows, test))]
#[derive(Clone, Copy)]
enum FingerprintFormat {
    Text,
    OemText,
}

#[cfg(any(windows, test))]
trait FingerprintSource {
    fn size_unicode(&self) -> Option<usize>;
    fn size_of(&self, format: FingerprintFormat) -> Option<usize>;
    fn get_string(&self, out: &mut Vec<u8>) -> std::result::Result<(), String>;
    fn get_vec(
        &self,
        format: FingerprintFormat,
        out: &mut Vec<u8>,
    ) -> std::result::Result<(), String>;
}

#[cfg(windows)]
struct NativeFingerprintSource;

#[cfg(windows)]
impl NativeFingerprintSource {
    fn format_id(format: FingerprintFormat) -> u32 {
        match format {
            FingerprintFormat::Text => crate::transfer::imp::CF_TEXT,
            FingerprintFormat::OemText => crate::transfer::imp::CF_OEMTEXT,
        }
    }
}

#[cfg(windows)]
impl FingerprintSource for NativeFingerprintSource {
    fn size_unicode(&self) -> Option<usize> {
        clipboard_win::raw::size(crate::transfer::imp::CF_UNICODETEXT).map(std::num::NonZero::get)
    }
    fn size_of(&self, format: FingerprintFormat) -> Option<usize> {
        clipboard_win::raw::size(Self::format_id(format)).map(std::num::NonZero::get)
    }
    fn get_string(&self, out: &mut Vec<u8>) -> std::result::Result<(), String> {
        clipboard_win::raw::get_string(out)
            .map(|_| ())
            .map_err(|e| format!("{e}"))
    }
    fn get_vec(
        &self,
        format: FingerprintFormat,
        out: &mut Vec<u8>,
    ) -> std::result::Result<(), String> {
        clipboard_win::raw::get_vec(Self::format_id(format), out)
            .map(|_| ())
            .map_err(|e| format!("{e}"))
    }
}

#[cfg(any(windows, test))]
trait MarkerSource {
    #[cfg(windows)]
    fn is_avail(&self, format: u32) -> bool;
    fn size_of(&self, format: u32) -> Option<usize>;
    fn get_vec(&self, format: u32, out: &mut Vec<u8>) -> std::result::Result<(), String>;
}

#[cfg(windows)]
struct NativeMarkerSource;

#[cfg(windows)]
impl MarkerSource for NativeMarkerSource {
    fn is_avail(&self, format: u32) -> bool {
        clipboard_win::raw::is_format_avail(format)
    }
    fn size_of(&self, format: u32) -> Option<usize> {
        clipboard_win::raw::size(format).map(std::num::NonZero::get)
    }
    fn get_vec(&self, format: u32, out: &mut Vec<u8>) -> std::result::Result<(), String> {
        clipboard_win::raw::get_vec(format, out)
            .map(|_| ())
            .map_err(|e| format!("{e}"))
    }
}

#[cfg(any(windows, test))]
fn fingerprint_with_source(event: &CaptureEvent, source: &dyn FingerprintSource) -> String {
    if event.sensitive {
        return fingerprint_parts(None, &event.offers);
    }
    let mut out = Vec::new();
    if let Some(size) = source.size_unicode() {
        if size <= MAX_FINGERPRINT_BYTES {
            out.clear();
            if source.get_string(&mut out).is_ok() && !out.is_empty() {
                return fingerprint_parts(Some(&out), &event.offers);
            }
        }
    }
    for format in [FingerprintFormat::Text, FingerprintFormat::OemText] {
        if let Some(size) = source.size_of(format) {
            if size <= MAX_FINGERPRINT_BYTES {
                out.clear();
                if source.get_vec(format, &mut out).is_ok() && !out.is_empty() {
                    return fingerprint_parts(Some(&out), &event.offers);
                }
            }
        }
    }
    fingerprint_parts(None, &event.offers)
}

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq)]
enum MarkerValue {
    Decoded(Option<String>),
    Refused,
}

#[cfg(any(windows, test))]
fn marker_value_with_source(id: u32, source: &dyn MarkerSource) -> MarkerValue {
    let Some(size) = source.size_of(id) else {
        return MarkerValue::Refused;
    };
    if size > MARKER_MAX_BYTES {
        return MarkerValue::Refused;
    }
    let mut out = Vec::new();
    if source.get_vec(id, &mut out).is_err() {
        return MarkerValue::Refused;
    }
    MarkerValue::Decoded(crate::optout::decode_marker(&out))
}

#[cfg(any(windows, test))]
fn probe_value_for_name(name: &str, id: u32, source: &dyn MarkerSource) -> MarkerValue {
    if name == crate::optout::EXCLUDE_FORMAT || name == crate::optout::VIEWER_IGNORE {
        return MarkerValue::Decoded(None);
    }
    marker_value_with_source(id, source)
}

// A refused read of a present value marker is different from an admitted
// payload whose decoder returns None. Preserve the legacy decoder policy.
#[cfg(any(windows, test))]
fn capture_event_from_offers(
    offers: Vec<FormatOffer>,
    probe: Vec<(String, MarkerValue)>,
) -> CaptureEvent {
    let refused_privacy_marker = probe.iter().any(|(name, value)| {
        matches!(
            name.as_str(),
            crate::optout::HISTORY_OPT_OUT | crate::optout::CLOUD_OPT_OUT
        ) && matches!(value, MarkerValue::Refused)
    });
    let decoded: Vec<_> = probe
        .into_iter()
        .map(|(name, value)| {
            let value = match value {
                MarkerValue::Decoded(value) => value,
                MarkerValue::Refused => None,
            };
            (name, value)
        })
        .collect();
    CaptureEvent {
        offers,
        origin: CaptureOrigin::Clipboard,
        sensitive: refused_privacy_marker || crate::optout::should_skip(&decoded),
    }
}

#[cfg(windows)]
fn enumerate_with_source(marker_source: &dyn MarkerSource) -> Offers {
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
            if marker_source.is_avail(id) {
                let value = probe_value_for_name(name, id, marker_source);
                probe.push((name.to_string(), value));
            }
        }
    }
    (offers, probe)
}

/// Enumerate current offers plus opt-out `(name, value)` pairs.
#[cfg(windows)]
type Offers = (Vec<FormatOffer>, Vec<(String, MarkerValue)>);

#[cfg(windows)]
fn enumerate_offers() -> Result<Offers> {
    crate::transfer::imp::with_open("enumerate", || {
        Ok(enumerate_with_source(&NativeMarkerSource))
    })
}

/// Build the capture event for the current clipboard content.
#[cfg(windows)]
fn current_event() -> Result<CaptureEvent> {
    let (offers, probe) = enumerate_offers()?;
    Ok(capture_event_from_offers(offers, probe))
}

#[allow(dead_code)]
fn fingerprint_parts(text: Option<&[u8]>, offers: &[FormatOffer]) -> String {
    let mut hash = clipmonger_core::hash::FNV_OFFSET_BASIS;
    if let Some(bytes) = text {
        hash = clipmonger_core::hash::fnv1a64_update(hash, bytes);
        for o in offers {
            hash = clipmonger_core::hash::fnv1a64_update(hash, o.mime.as_bytes());
        }
        return format!("{hash:x}:{}", bytes.len());
    }
    for o in offers {
        hash = clipmonger_core::hash::fnv1a64_update(hash, o.mime.as_bytes());
    }
    format!("{hash:x}")
}

#[cfg(windows)]
fn content_fingerprint(event: &CaptureEvent) -> String {
    if event.sensitive {
        return fingerprint_parts(None, &event.offers);
    }
    let result: Result<String> = crate::transfer::imp::with_open("fingerprint", || {
        let source = NativeFingerprintSource;
        Ok(fingerprint_with_source(event, &source))
    });
    result.unwrap_or_else(|_| fingerprint_parts(None, &event.offers))
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
                                    self.last_hash = Some(content_fingerprint(&event));
                                    events.push(event);
                                    std::thread::sleep(std::time::Duration::from_millis(30));
                                }
                                Err(Error::Timeout(_)) => {
                                    std::thread::sleep(std::time::Duration::from_millis(20));
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
                                let fp = content_fingerprint(&event);
                                self.last_seq = Some(seq);
                                self.last_hash = Some(fp);
                                events.push(event);
                            }
                            Err(Error::Timeout(_)) => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
                if events.is_empty() {
                    if let Ok(event) = current_event() {
                        if !event.offers.is_empty() {
                            let fp = content_fingerprint(&event);
                            if self.last_hash.as_deref() != Some(&fp) {
                                self.last_hash = Some(fp);
                                events.push(event);
                            }
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

#[cfg(test)]
mod tests {
    use super::fingerprint_parts;
    use clipmonger_platform::types::FormatOffer;

    fn offer(mime: &str) -> FormatOffer {
        FormatOffer {
            mime: mime.to_string(),
            bytes_hint: None,
        }
    }

    #[test]
    fn fingerprint_empty_no_text_no_offers_is_offset_basis_unpadded() {
        assert_eq!(fingerprint_parts(None, &[]), "cbf29ce484222325");
    }

    #[test]
    fn fingerprint_text_only_orders_and_lengths() {
        assert_eq!(
            fingerprint_parts(Some(b"hello"), &[]),
            format!("{:x}:5", clipmonger_core::hash::fnv1a64(b"hello"))
        );
        assert_eq!(fingerprint_parts(Some(b"a"), &[]), "af63dc4c8601ec8c:1");
        assert_eq!(fingerprint_parts(Some(b"fo"), &[]), "8985907b541d342:2");
        assert!(
            !fingerprint_parts(Some(b"fo"), &[]).starts_with('0'),
            "monitor must use unpadded hex, fo proves no leading zero"
        );
    }

    #[test]
    fn fingerprint_with_offers_hashes_text_then_mimes_in_order() {
        let text = b"hello";
        let offers = [offer("text/plain"), offer("text/html")];
        let mut expected = clipmonger_core::hash::FNV_OFFSET_BASIS;
        expected = clipmonger_core::hash::fnv1a64_update(expected, text);
        expected = clipmonger_core::hash::fnv1a64_update(expected, b"text/plain");
        expected = clipmonger_core::hash::fnv1a64_update(expected, b"text/html");
        assert_eq!(
            fingerprint_parts(Some(text), &offers),
            format!("{expected:x}:{}", text.len())
        );
        let swapped = [offer("text/html"), offer("text/plain")];
        assert_ne!(
            fingerprint_parts(Some(text), &offers),
            fingerprint_parts(Some(text), &swapped),
            "offer order must be preserved"
        );
    }

    #[test]
    fn fingerprint_fallback_hashes_only_offers_without_length() {
        let offers = [offer("text/plain")];
        let mut expected = clipmonger_core::hash::FNV_OFFSET_BASIS;
        expected = clipmonger_core::hash::fnv1a64_update(expected, b"text/plain");
        assert_eq!(fingerprint_parts(None, &offers), format!("{expected:x}"));
        assert_eq!(fingerprint_parts(None, &offers), "1f0110dcf88682e5");
        let empty_offers: &[FormatOffer] = &[];
        assert_eq!(fingerprint_parts(None, empty_offers), "cbf29ce484222325");
    }

    #[test]
    fn fingerprint_padding_and_length_semantics() {
        let empty: &[FormatOffer] = &[];
        assert_eq!(fingerprint_parts(Some(b""), empty), "cbf29ce484222325:0");
        assert_eq!(fingerprint_parts(Some(b"a"), empty), "af63dc4c8601ec8c:1");
        let text = b"hello world";
        assert!(fingerprint_parts(Some(text), empty).ends_with(":11"));
    }

    struct MockFingerprintSource {
        size_unicode: Option<usize>,
        size_text: Option<usize>,
        size_oem: Option<usize>,
        unicode_bytes: Vec<u8>,
        text_bytes: Vec<u8>,
        oem_bytes: Vec<u8>,
        get_string_calls: std::cell::Cell<usize>,
        get_vec_calls: std::cell::Cell<usize>,
    }

    impl super::FingerprintSource for MockFingerprintSource {
        fn size_unicode(&self) -> Option<usize> {
            self.size_unicode
        }
        fn size_of(&self, format: super::FingerprintFormat) -> Option<usize> {
            match format {
                super::FingerprintFormat::Text => self.size_text,
                super::FingerprintFormat::OemText => self.size_oem,
            }
        }
        fn get_string(&self, out: &mut Vec<u8>) -> std::result::Result<(), String> {
            self.get_string_calls.set(self.get_string_calls.get() + 1);
            out.extend_from_slice(&self.unicode_bytes);
            Ok(())
        }
        fn get_vec(
            &self,
            format: super::FingerprintFormat,
            out: &mut Vec<u8>,
        ) -> std::result::Result<(), String> {
            self.get_vec_calls.set(self.get_vec_calls.get() + 1);
            match format {
                super::FingerprintFormat::Text => out.extend_from_slice(&self.text_bytes),
                super::FingerprintFormat::OemText => out.extend_from_slice(&self.oem_bytes),
            }
            Ok(())
        }
    }

    #[test]
    fn sensitive_fingerprint_does_not_invoke_reader() {
        let offers = [offer("text/plain")];
        let event = clipmonger_platform::types::CaptureEvent {
            offers: offers.to_vec(),
            origin: clipmonger_platform::types::CaptureOrigin::Clipboard,
            sensitive: true,
        };
        let mock = MockFingerprintSource {
            size_unicode: Some(5),
            size_text: Some(5),
            size_oem: Some(5),
            unicode_bytes: b"hello".to_vec(),
            text_bytes: b"hello".to_vec(),
            oem_bytes: b"hello".to_vec(),
            get_string_calls: std::cell::Cell::new(0),
            get_vec_calls: std::cell::Cell::new(0),
        };
        let fp = super::fingerprint_with_source(&event, &mock);
        assert_eq!(
            mock.get_string_calls.get(),
            0,
            "sensitive must not read clipboard"
        );
        assert_eq!(mock.get_vec_calls.get(), 0);
        assert_eq!(fp, super::fingerprint_parts(None, &offers));
    }

    #[test]
    fn oversize_fingerprint_falls_back_without_reading() {
        let offers = [offer("text/plain")];
        let event = clipmonger_platform::types::CaptureEvent {
            offers: offers.to_vec(),
            origin: clipmonger_platform::types::CaptureOrigin::Clipboard,
            sensitive: false,
        };
        let mock = MockFingerprintSource {
            size_unicode: Some(100_000),
            size_text: Some(100_000),
            size_oem: None,
            unicode_bytes: b"hello".to_vec(),
            text_bytes: b"hello".to_vec(),
            oem_bytes: Vec::new(),
            get_string_calls: std::cell::Cell::new(0),
            get_vec_calls: std::cell::Cell::new(0),
        };
        let fp = super::fingerprint_with_source(&event, &mock);
        assert_eq!(mock.get_string_calls.get(), 0);
        assert_eq!(mock.get_vec_calls.get(), 0);
        assert_eq!(fp, super::fingerprint_parts(None, &offers));
    }

    #[test]
    fn unknown_size_fingerprint_falls_back_without_reading() {
        let offers = [offer("text/plain")];
        let event = clipmonger_platform::types::CaptureEvent {
            offers: offers.to_vec(),
            origin: clipmonger_platform::types::CaptureOrigin::Clipboard,
            sensitive: false,
        };
        let mock = MockFingerprintSource {
            size_unicode: None,
            size_text: None,
            size_oem: None,
            unicode_bytes: Vec::new(),
            text_bytes: Vec::new(),
            oem_bytes: Vec::new(),
            get_string_calls: std::cell::Cell::new(0),
            get_vec_calls: std::cell::Cell::new(0),
        };
        let fp = super::fingerprint_with_source(&event, &mock);
        assert_eq!(mock.get_string_calls.get(), 0);
        assert_eq!(mock.get_vec_calls.get(), 0);
        assert_eq!(fp, super::fingerprint_parts(None, &offers));
    }

    #[test]
    fn normal_text_fingerprint_stable() {
        let offers = [offer("text/plain"), offer("text/html")];
        let event = clipmonger_platform::types::CaptureEvent {
            offers: offers.to_vec(),
            origin: clipmonger_platform::types::CaptureOrigin::Clipboard,
            sensitive: false,
        };
        let mock = MockFingerprintSource {
            size_unicode: Some(5),
            size_text: None,
            size_oem: None,
            unicode_bytes: b"hello".to_vec(),
            text_bytes: Vec::new(),
            oem_bytes: Vec::new(),
            get_string_calls: std::cell::Cell::new(0),
            get_vec_calls: std::cell::Cell::new(0),
        };
        let fp = super::fingerprint_with_source(&event, &mock);
        let mut expected = clipmonger_core::hash::FNV_OFFSET_BASIS;
        expected = clipmonger_core::hash::fnv1a64_update(expected, b"hello");
        expected = clipmonger_core::hash::fnv1a64_update(expected, b"text/plain");
        expected = clipmonger_core::hash::fnv1a64_update(expected, b"text/html");
        assert_eq!(fp, format!("{expected:x}:5"));
        assert_eq!(mock.get_string_calls.get(), 1);
    }

    struct MockMarkerSource {
        size: Option<usize>,
        bytes: Vec<u8>,
        read_error: bool,
        size_calls: std::cell::Cell<usize>,
        get_vec_calls: std::cell::Cell<usize>,
    }

    impl MockMarkerSource {
        fn new(size: Option<usize>, bytes: &[u8], read_error: bool) -> Self {
            Self {
                size,
                bytes: bytes.to_vec(),
                read_error,
                size_calls: std::cell::Cell::new(0),
                get_vec_calls: std::cell::Cell::new(0),
            }
        }
    }

    impl super::MarkerSource for MockMarkerSource {
        #[cfg(windows)]
        fn is_avail(&self, _format: u32) -> bool {
            true
        }
        fn size_of(&self, _format: u32) -> Option<usize> {
            self.size_calls.set(self.size_calls.get() + 1);
            self.size
        }
        fn get_vec(&self, _format: u32, out: &mut Vec<u8>) -> std::result::Result<(), String> {
            self.get_vec_calls.set(self.get_vec_calls.get() + 1);
            if self.read_error {
                return Err("marker read failed".to_string());
            }
            out.extend_from_slice(&self.bytes);
            Ok(())
        }
    }

    #[test]
    fn refused_present_markers_make_capture_sensitive() {
        for name in [crate::optout::HISTORY_OPT_OUT, crate::optout::CLOUD_OPT_OUT] {
            for (size, read_error, expected_reads) in
                [(Some(65), false, 0), (None, false, 0), (Some(4), true, 1)]
            {
                let source = MockMarkerSource::new(size, &[1, 0, 0, 0], read_error);
                let value = super::probe_value_for_name(name, 1, &source);
                assert_eq!(value, super::MarkerValue::Refused);
                let event = super::capture_event_from_offers(
                    vec![offer("text/plain")],
                    vec![(name.to_string(), value)],
                );
                assert!(event.sensitive, "refused {name} size {size:?}");
                assert_eq!(source.get_vec_calls.get(), expected_reads);
            }
        }
    }

    #[test]
    fn admitted_markers_keep_legacy_capture_decision() {
        let cases: &[(&[u8], Option<&str>, bool)] = &[
            (&[0, 0, 0, 0], Some("0"), true),
            (&[1, 0, 0, 0], Some("1"), false),
            (&[0; 8], Some("0"), true),
            (&[0; 64], Some("0"), true),
            (&[1, 0, 0, 0, 0, 0, 0, 0], Some("1"), false),
            (b"0", Some("0"), true),
            (b" 0\0", Some("0"), true),
            (b"0\0\0\0", Some("0"), true),
            (b"1", Some("1"), false),
            (b"hello\0", Some("hello"), false),
            (b"", Some(""), false),
            (&[2, 0, 0, 0], Some("\u{2}"), false),
            (&[0, 1, 0, 0], Some("\u{1}"), false),
            (&[255; 4], Some("\u{fffd}\u{fffd}\u{fffd}\u{fffd}"), false),
        ];
        for name in [crate::optout::HISTORY_OPT_OUT, crate::optout::CLOUD_OPT_OUT] {
            for &(bytes, expected, sensitive) in cases {
                let source = MockMarkerSource::new(Some(bytes.len()), bytes, false);
                let value = super::probe_value_for_name(name, 1, &source);
                assert_eq!(
                    value,
                    super::MarkerValue::Decoded(expected.map(str::to_string))
                );
                let event = super::capture_event_from_offers(
                    vec![offer("text/plain")],
                    vec![(name.to_string(), value)],
                );
                assert_eq!(event.sensitive, sensitive, "{name}: {bytes:?}");
                assert_eq!(source.get_vec_calls.get(), 1);
            }
        }
    }

    #[test]
    fn decoded_none_and_missing_markers_keep_existing_policy() {
        assert!(!super::capture_event_from_offers(Vec::new(), Vec::new()).sensitive);
        for name in [crate::optout::HISTORY_OPT_OUT, crate::optout::CLOUD_OPT_OUT] {
            let event = super::capture_event_from_offers(
                Vec::new(),
                vec![(name.to_string(), super::MarkerValue::Decoded(None))],
            );
            assert!(!event.sensitive, "missing value for {name}");
            // Preserve decode_marker's None contract even if a source returns
            // more bytes than its size admission reported.
            let source = MockMarkerSource::new(Some(64), &[b'x'; 65], false);
            let value = super::probe_value_for_name(name, 1, &source);
            assert_eq!(value, super::MarkerValue::Decoded(None));
            let event =
                super::capture_event_from_offers(Vec::new(), vec![(name.to_string(), value)]);
            assert!(!event.sensitive);
        }
    }

    #[test]
    fn name_only_markers_make_capture_sensitive_without_size_or_value_read() {
        for name in [crate::optout::EXCLUDE_FORMAT, crate::optout::VIEWER_IGNORE] {
            let source = MockMarkerSource::new(None, b"should_not_be_read", true);
            let value = super::probe_value_for_name(name, 1, &source);
            assert_eq!(value, super::MarkerValue::Decoded(None));
            let event = super::capture_event_from_offers(
                vec![offer("text/plain")],
                vec![(name.to_string(), value)],
            );
            assert!(event.sensitive, "presence of {name}");
            assert_eq!(source.size_calls.get(), 0);
            assert_eq!(source.get_vec_calls.get(), 0);
        }
    }
}

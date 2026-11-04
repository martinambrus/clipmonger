//! Clipboard change monitor: hidden `HWND_MESSAGE` window plus
//! `AddClipboardFormatListener`; `WM_CLIPBOARDUPDATE` wakes are deduplicated
//! by clipboard sequence number and expanded into `CaptureEvent`s on poll.
//!
//! Privacy opt-outs (seg-04 S1) are honored here: skipped captures still
//! produce an event with `sensitive=true` so the daemon counts them in
//! `privacy_skips` without storing anything.

#[cfg(any(windows, test))]
use std::sync::mpsc::Receiver;
#[cfg(windows)]
use std::sync::mpsc::Sender;
use std::time::Duration;
#[cfg(any(windows, test))]
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
    poll_state: PollState,
    #[cfg(windows)]
    policy: crate::capture_policy::CapturePolicy,
    #[cfg(windows)]
    self_writes: Option<SelfWriteProbe>,
}

/// Reports whether a clipboard sequence number belongs to a write the
/// daemon registered as its own, without consuming the registration.
pub type SelfWriteProbe = std::sync::Arc<dyn Fn(u32) -> bool + Send + Sync>;

/// How long the monitor leaves the clipboard alone after a self-write.
///
/// A paste writes the clipboard and presses Ctrl+V about 2 ms later. Reading
/// the write at once holds the clipboard open while the target handles the
/// chord, and its `OpenClipboard` then fails silently, so nothing is pasted.
/// The deferred read still reaches the capture boundary, where the
/// registration suppresses it as before.
#[cfg(any(windows, test))]
const SELF_WRITE_DEFER: Duration = Duration::from_millis(150);

impl WinMonitor {
    /// Start the pump thread and install the clipboard listener.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unavailable`] when the session cannot provide a
    /// clipboard listener (non-Windows builds always fail this way).
    pub fn new() -> Result<Self> {
        Self::with_policy(std::sync::Arc::new(|| Some(Vec::new())))
    }

    /// Start a capture monitor with a live owner policy.
    ///
    /// # Errors
    /// Returns [`Error::Unavailable`] if the clipboard listener cannot start.
    pub fn with_policy(policy: crate::capture_policy::CapturePolicy) -> Result<Self> {
        #[cfg(windows)]
        {
            let (tx, rx): (Sender<()>, Receiver<()>) = std::sync::mpsc::channel();
            let pump = crate::sys::start_pump(tx)
                .map_err(|e| Error::Unavailable(format!("monitor unavailable: {e}")))?;
            Ok(Self {
                pump: Some(pump),
                wakes: Some(rx),
                subscribed_clipboard: false,
                poll_state: PollState::default(),
                policy,
                self_writes: None,
            })
        }
        #[cfg(not(windows))]
        {
            drop(policy);
            Err(Error::Unavailable(
                "clipboard monitor needs a Windows session".to_string(),
            ))
        }
    }

    /// Defer reading clipboard updates that `probe` reports as self-writes.
    #[must_use]
    pub fn with_self_writes(mut self, probe: SelfWriteProbe) -> Self {
        #[cfg(windows)]
        {
            self.self_writes = Some(probe);
        }
        #[cfg(not(windows))]
        {
            drop(probe);
        }
        self
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
        native_sequence: None,
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

#[cfg(windows)]
type Offers = (Vec<FormatOffer>, Vec<(String, MarkerValue)>);

/// Build the capture event for the current clipboard content.
#[cfg(windows)]
fn current_event(policy: &crate::capture_policy::CapturePolicy) -> Result<CaptureEvent> {
    crate::transfer::imp::with_open("enumerate", || {
        let sequence = current_seq();
        let entries = policy();
        let owner = crate::sys::clipboard_owner_executable();
        match crate::capture_policy::admission(
            sequence,
            sequence,
            owner.as_deref(),
            entries.as_deref(),
        ) {
            crate::capture_policy::Admission::Stale => {
                return Err(Error::Timeout("clipboard sequence unavailable".into()))
            }
            crate::capture_policy::Admission::OwnerDenied => {
                return Ok(CaptureEvent {
                    native_sequence: sequence,
                    offers: vec![FormatOffer {
                        mime: "text/plain".into(),
                        bytes_hint: None,
                    }],
                    origin: CaptureOrigin::Clipboard,
                    sensitive: true,
                })
            }
            crate::capture_policy::Admission::Allow => {}
        }
        let (offers, probe) = enumerate_with_source(&NativeMarkerSource);
        let mut event = capture_event_from_offers(offers, probe);
        event.native_sequence = sequence;
        Ok(event)
    })
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
fn content_fingerprint(
    event: &CaptureEvent,
    policy: &crate::capture_policy::CapturePolicy,
) -> String {
    if event.sensitive {
        return fingerprint_parts(None, &event.offers);
    }
    let result: Result<String> = crate::transfer::imp::with_open("fingerprint", || {
        let entries = policy();
        let owner = crate::sys::clipboard_owner_executable();
        Ok(crate::capture_policy::guarded(
            event.native_sequence,
            current_seq(),
            owner.as_deref(),
            entries.as_deref(),
            || fingerprint_with_source(event, &NativeFingerprintSource),
        )
        .unwrap_or_else(|_| fingerprint_parts(None, &event.offers)))
    });
    result.unwrap_or_else(|_| fingerprint_parts(None, &event.offers))
}

impl ClipboardMonitor for WinMonitor {
    const AFFINITY: Affinity = Affinity::Worker;

    fn subscribe(&mut self, kinds: &[SubscribeKind]) -> Result<()> {
        #[cfg(windows)]
        {
            self.subscribed_clipboard = kinds.contains(&SubscribeKind::Clipboard);
            self.poll_state.last_seq = current_seq();
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
            let self_writes = self.self_writes.clone();
            self.poll_state.poll(
                rx,
                timeout,
                current_seq,
                |seq| self_writes.as_ref().is_some_and(|probe| probe(seq)),
                || current_event(&self.policy),
                |event| content_fingerprint(event, &self.policy),
                std::thread::sleep,
            )
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

/// Poll bookkeeping shared by the native wake reader and channel controls.
#[cfg(any(windows, test))]
#[derive(Default)]
struct PollState {
    last_seq: Option<u32>,
    last_hash: Option<String>,
    pending_error: Option<Error>,
    /// A self-write sequence and the instant its read becomes allowed.
    self_write: Option<(u32, Instant)>,
}

#[cfg(any(windows, test))]
impl PollState {
    /// Whether reading `seq` must wait because it is a recent self-write.
    /// The probe is asked once per sequence; once its deferral ends, the
    /// sequence is read like any other update.
    fn defers(&mut self, seq: Option<u32>, is_self_write: &mut impl FnMut(u32) -> bool) -> bool {
        let Some(seq) = seq else {
            return false;
        };
        match self.self_write {
            Some((deferred, until)) if deferred == seq => Instant::now() < until,
            _ if is_self_write(seq) => {
                self.self_write = Some((seq, Instant::now() + SELF_WRITE_DEFER));
                true
            }
            _ => false,
        }
    }

    /// The end of a pending self-write deferral, if one is still running.
    fn deferral_end(&self) -> Option<Instant> {
        self.self_write
            .map(|(_, until)| until)
            .filter(|until| Instant::now() < *until)
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn poll(
        &mut self,
        rx: &Receiver<()>,
        timeout: Duration,
        mut current_seq: impl FnMut() -> Option<u32>,
        mut is_self_write: impl FnMut(u32) -> bool,
        mut current_event: impl FnMut() -> Result<CaptureEvent>,
        mut content_fingerprint: impl FnMut(&CaptureEvent) -> String,
        mut sleep: impl FnMut(Duration),
    ) -> Result<Vec<CaptureEvent>> {
        if let Some(error) = self.pending_error.take() {
            return Err(error);
        }
        let deadline = Instant::now() + timeout;
        let mut events = Vec::new();
        let mut deferred: Option<Error> = None;
        loop {
            let now = Instant::now();
            // A deferred self-write is read when its deferral ends, not
            // after a full poll timeout, so its registration is still live.
            let limit = self
                .deferral_end()
                .map_or(deadline, |until| until.min(deadline));
            if now >= limit {
                break;
            }
            match rx.recv_timeout(limit - now) {
                Ok(()) => {
                    let mut retries = 15;
                    while retries > 0 && Instant::now() < deadline {
                        retries -= 1;
                        let seq = current_seq();
                        if seq == self.last_seq || self.defers(seq, &mut is_self_write) {
                            sleep(std::time::Duration::from_millis(20));
                            continue;
                        }
                        match current_event() {
                            Ok(event) => {
                                if event.offers.is_empty() {
                                    sleep(std::time::Duration::from_millis(40));
                                    continue;
                                }
                                self.last_seq = event.native_sequence.or(seq);
                                self.last_hash = Some(content_fingerprint(&event));
                                events.push(event);
                                sleep(std::time::Duration::from_millis(30));
                            }
                            Err(Error::Timeout(_)) => {
                                sleep(std::time::Duration::from_millis(20));
                            }
                            Err(e) => {
                                if events.is_empty() {
                                    return Err(e);
                                }
                                deferred = Some(e);
                                break;
                            }
                        }
                    }
                    if deferred.is_some() {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    if events.is_empty() {
                        return Err(Error::Unavailable("monitor pump thread exited".to_string()));
                    }
                    deferred = Some(Error::Unavailable("monitor pump thread exited".to_string()));
                    break;
                }
            }
        }
        let idle = deferred.is_none() && events.is_empty();
        let seq = if idle { current_seq() } else { None };
        if idle && !self.defers(seq, &mut is_self_write) && self.deferral_end().is_none() {
            if let Some(seq) = seq {
                if Some(seq) != self.last_seq {
                    match current_event() {
                        Ok(event) => {
                            let fp = content_fingerprint(&event);
                            self.last_seq = event.native_sequence.or(Some(seq));
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
        if let Some(err) = deferred {
            self.pending_error = Some(err);
        }
        Ok(events)
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
            native_sequence: None,
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
            native_sequence: None,
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
            native_sequence: None,
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
            native_sequence: None,
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

    #[test]
    fn wake_read_error_delivers_committed_events_then_exact_error() {
        use super::PollState;
        use clipmonger_platform::types::{CaptureEvent, CaptureOrigin, Error};
        use std::cell::Cell;
        use std::time::Duration;
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(()).unwrap();
        let sequence = Cell::new(0);
        let reads = Cell::new(0);
        let mut state = PollState::default();
        let events = state
            .poll(
                &rx,
                Duration::from_secs(1),
                || {
                    sequence.set(sequence.get() + 1);
                    Some(sequence.get())
                },
                |_| false,
                || {
                    reads.set(reads.get() + 1);
                    if reads.get() == 1 {
                        Ok(CaptureEvent {
                            native_sequence: None,
                            offers: vec![offer("text/plain")],
                            origin: CaptureOrigin::Clipboard,
                            sensitive: true,
                        })
                    } else {
                        Err(Error::Io("exact read error".into()))
                    }
                },
                |_| "committed fingerprint".into(),
                |_| {},
            )
            .unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].sensitive);
        assert_eq!(state.last_seq, Some(1));
        assert_eq!(state.last_hash.as_deref(), Some("committed fingerprint"));
        let error = state
            .poll(
                &rx,
                Duration::ZERO,
                || panic!("read before pending error"),
                |_| panic!("probe before pending error"),
                || panic!("read before pending error"),
                |_| panic!("hash before pending error"),
                |_| {},
            )
            .unwrap_err();
        assert!(matches!(error, Error::Io(detail) if detail == "exact read error"));
        assert_eq!(reads.get(), 2);
        assert_eq!(state.last_seq, Some(1));
    }

    #[test]
    fn queued_wake_then_disconnect_delivers_before_unavailable() {
        use super::PollState;
        use clipmonger_platform::types::{CaptureEvent, CaptureOrigin, Error};
        use std::time::Duration;
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(()).unwrap();
        drop(tx);
        let mut state = PollState::default();
        let events = state
            .poll(
                &rx,
                Duration::from_secs(1),
                || Some(9),
                |_| false,
                || {
                    Ok(CaptureEvent {
                        native_sequence: None,
                        offers: vec![offer("text/html")],
                        origin: CaptureOrigin::Clipboard,
                        sensitive: false,
                    })
                },
                |_| "hash".into(),
                |_| {},
            )
            .unwrap();
        assert_eq!(events.len(), 1);
        let error = state
            .poll(
                &rx,
                Duration::ZERO,
                || None,
                |_| false,
                || panic!("no reads"),
                |_| String::new(),
                |_| {},
            )
            .unwrap_err();
        assert!(
            matches!(error, Error::Unavailable(detail) if detail == "monitor pump thread exited")
        );
    }

    fn text_event(sequence: u32) -> clipmonger_platform::types::CaptureEvent {
        clipmonger_platform::types::CaptureEvent {
            native_sequence: Some(sequence),
            offers: vec![offer("text/plain")],
            origin: clipmonger_platform::types::CaptureOrigin::Clipboard,
            sensitive: false,
        }
    }

    #[test]
    fn self_write_wake_leaves_clipboard_closed_until_deferral_ends() {
        use super::{PollState, SELF_WRITE_DEFER};
        use std::cell::Cell;
        use std::time::{Duration, Instant};
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(()).unwrap();
        let probes = Cell::new(0);
        let first_read: Cell<Option<Instant>> = Cell::new(None);
        let mut state = PollState::default();
        let start = Instant::now();
        let events = state
            .poll(
                &rx,
                Duration::from_secs(1),
                || Some(7),
                |seq| {
                    probes.set(probes.get() + 1);
                    seq == 7
                },
                || {
                    if first_read.get().is_none() {
                        first_read.set(Some(Instant::now()));
                    }
                    Ok(text_event(7))
                },
                |_| "hash".into(),
                |_| {},
            )
            .unwrap();
        let read_at = first_read
            .get()
            .expect("self-write is still read once deferred");
        assert!(read_at.duration_since(start) >= SELF_WRITE_DEFER);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "read when deferral ends"
        );
        assert_eq!(events.len(), 1, "the capture boundary still sees the write");
        assert_eq!(state.last_seq, Some(7));
        assert_eq!(probes.get(), 1, "the probe is asked once per sequence");
    }

    #[test]
    fn foreign_write_wake_is_read_without_deferral() {
        use super::{PollState, SELF_WRITE_DEFER};
        use std::time::{Duration, Instant};
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(()).unwrap();
        let mut state = PollState::default();
        let start = Instant::now();
        let mut read_after = None;
        let events = state
            .poll(
                &rx,
                Duration::from_millis(50),
                || Some(8),
                |_| false,
                || {
                    read_after.get_or_insert_with(|| start.elapsed());
                    Ok(text_event(8))
                },
                |_| "hash".into(),
                |_| {},
            )
            .unwrap();
        assert_eq!(events.len(), 1);
        assert!(read_after.unwrap() < SELF_WRITE_DEFER);
    }

    #[test]
    fn idle_poll_does_not_read_a_self_write_during_deferral() {
        use super::{PollState, SELF_WRITE_DEFER};
        use std::time::Duration;
        let (_tx, rx) = std::sync::mpsc::channel();
        let mut state = PollState::default();
        for _ in 0..2 {
            let events = state
                .poll(
                    &rx,
                    Duration::ZERO,
                    || Some(11),
                    |seq| seq == 11,
                    || panic!("clipboard opened during self-write deferral"),
                    |_| panic!("clipboard fingerprinted during self-write deferral"),
                    |_| {},
                )
                .unwrap();
            assert_eq!(events.len(), 0);
        }
        std::thread::sleep(SELF_WRITE_DEFER);
        let events = state
            .poll(
                &rx,
                Duration::ZERO,
                || Some(11),
                |_| panic!("an expired deferral is not probed again"),
                || Ok(text_event(11)),
                |_| "hash".into(),
                |_| {},
            )
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(state.last_seq, Some(11));
    }
}

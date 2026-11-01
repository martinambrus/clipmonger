//! Private clipboard swap orchestration. Native calls cannot be preempted by
//! the snapshot deadline; completed reads are rejected after the deadline.
use clipmonger_platform::types::{Error, Result};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChordAction {
    PressCtrl,
    ClickV,
    ReleaseCtrl,
}

pub(crate) trait ChordDriver {
    fn init(&mut self) -> std::result::Result<(), String>;
    fn perform(&mut self, action: ChordAction) -> std::result::Result<(), String>;
}

pub(crate) fn run_chord<D: ChordDriver>(driver: &mut D) -> std::result::Result<(), String> {
    driver.init()?;
    driver.perform(ChordAction::PressCtrl)?;
    let v_res = driver.perform(ChordAction::ClickV);
    let r_res = driver.perform(ChordAction::ReleaseCtrl);
    match (v_res, r_res) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(v), _) => Err(v),
        (Ok(()), Err(r)) => Err(r),
    }
}

#[cfg(windows)]
pub(crate) struct EnigoChordDriver {
    enigo: Option<enigo::Enigo>,
}

#[cfg(windows)]
impl EnigoChordDriver {
    pub(crate) fn new() -> Self {
        Self { enigo: None }
    }
}

#[cfg(windows)]
impl ChordDriver for EnigoChordDriver {
    fn init(&mut self) -> std::result::Result<(), String> {
        let e = enigo::Enigo::new(&enigo::Settings::default()).map_err(|e| format!("{e}"))?;
        self.enigo = Some(e);
        Ok(())
    }
    fn perform(&mut self, action: ChordAction) -> std::result::Result<(), String> {
        use enigo::Keyboard;
        let enigo = self.enigo.as_mut().ok_or("chord driver not initialized")?;
        match action {
            ChordAction::PressCtrl => enigo
                .key(enigo::Key::Control, enigo::Direction::Press)
                .map_err(|e| format!("{e}")),
            ChordAction::ClickV => enigo
                .key(enigo::Key::V, enigo::Direction::Click)
                .map_err(|e| format!("{e}")),
            ChordAction::ReleaseCtrl => enigo
                .key(enigo::Key::Control, enigo::Direction::Release)
                .map_err(|e| format!("{e}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SnapshotKind {
    Available(Vec<(u32, Vec<u8>)>),
    VerifiedEmpty,
    Disabled,
}

#[derive(Debug)]
pub(crate) struct BeginSuccess {
    pub(crate) saved: SnapshotKind,
    pub(crate) seq: Option<String>,
    pub(crate) temp_text: Option<Vec<u8>>,
}

#[derive(Debug)]
pub(crate) struct BeginError {
    pub(crate) primary: Error,
    pub(crate) rollback: Option<Error>,
    pub(crate) close: Option<Error>,
}

impl BeginError {
    // Only bounded status flags cross the diagnostic boundary, never content
    // or platform error strings supplied by clipboard owners.
    pub(crate) fn into_primary(self) -> Error {
        if self.rollback.is_some() || self.close.is_some() {
            eprintln!(
                "clipboard publication cleanup: rollback_failed={} close_failed={}",
                self.rollback.is_some(),
                self.close.is_some()
            );
        }
        self.primary
    }
}

type NativeOps = Vec<(u32, Vec<u8>)>;

fn prepare_ops(payloads: &[(String, Vec<u8>)]) -> Result<NativeOps> {
    use crate::transfer::imp;
    if payloads.is_empty() {
        return Err(Error::Unsupported(
            "clipboard restore needs stored payload bytes".into(),
        ));
    }
    let needs_text = payloads
        .iter()
        .any(|(mime, _)| mime == "text/plain" || mime == "text/plain;charset=utf-8");
    let text_locale = if needs_text {
        Some(crate::text_encoding::text_locale_and_pages()?)
    } else {
        None
    };
    let mut ops = Vec::with_capacity(payloads.len() * 4 + 2);
    for (mime, bytes) in payloads {
        if mime == "text/plain" || mime == "text/plain;charset=utf-8" {
            let text = std::str::from_utf8(bytes)
                .map_err(|e| Error::Internal(format!("{mime} payload is not UTF-8: {e}")))?;
            let (lcid, ansi_cp, oem_cp) = text_locale.unwrap();
            let companions = crate::text_encoding::encode_text_companions_with_locale(
                text, lcid, ansi_cp, oem_cp,
            )?;
            ops.extend(companions);
        } else {
            let id = imp::id_for_mime(mime)?;
            ops.push((id, crate::transfer::encode_native(mime, bytes)?));
            if mime == "image/png" {
                if let Ok(dib) = crate::dib::png_to_dib(bytes) {
                    ops.push((imp::CF_DIB, dib));
                }
            }
            if mime == "text/uri-list" && !payloads.iter().any(|(m, _)| m == imp::DROP_EFFECT_MIME)
            {
                ops.push((
                    imp::register(imp::DROP_EFFECT_FORMAT)?,
                    imp::DROPEFFECT_COPY.to_le_bytes().to_vec(),
                ));
            }
        }
    }
    Ok(ops)
}

pub(crate) trait ClipBackend {
    fn open(&mut self) -> Result<()>;
    fn close(&mut self) -> Result<()>;
    fn empty(&mut self) -> Result<()>;
    // Both publication and restoration receive native bytes, never MIME bytes.
    fn set(&mut self, id: u32, bytes: &[u8]) -> Result<()>;
    fn get(&mut self, id: u32, cap: u64) -> Result<Vec<u8>>;
    fn text_available(&mut self) -> Result<bool>;
    fn enum_formats(&mut self) -> Result<Vec<u32>>;
    fn seq(&self) -> Option<String>;
    fn size(&self, id: u32) -> Option<u64>;
}

#[cfg(windows)]
pub(crate) struct WinBackend {
    owner: crate::sys::ClipboardWindow,
}

#[cfg(windows)]
impl WinBackend {
    pub(crate) fn new() -> Result<Self> {
        let owner = crate::sys::ClipboardWindow::new().ok_or_else(|| {
            Error::Unavailable(format!(
                "clipboard open: no owner window: {}",
                crate::sys::last_error()
            ))
        })?;
        Ok(Self { owner })
    }
}

#[cfg(windows)]
impl ClipBackend for WinBackend {
    fn open(&mut self) -> Result<()> {
        for attempt in 0..5 {
            match clipboard_win::raw::open_for(self.owner.hwnd()) {
                Ok(()) => return Ok(()),
                Err(e) if attempt == 4 => {
                    return Err(Error::Timeout(format!("clipboard open timed out: {e}")))
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        unreachable!("bounded open retry")
    }
    fn close(&mut self) -> Result<()> {
        clipboard_win::raw::close().map_err(|e| crate::transfer::imp::clip_error("close", e))
    }
    fn empty(&mut self) -> Result<()> {
        clipboard_win::raw::empty().map_err(|e| crate::transfer::imp::clip_error("empty", e))
    }
    fn set(&mut self, id: u32, bytes: &[u8]) -> Result<()> {
        clipboard_win::raw::set_without_clear(id, bytes)
            .map_err(|e| crate::transfer::imp::clip_error("set", e))
    }
    fn get(&mut self, id: u32, cap: u64) -> Result<Vec<u8>> {
        let size = clipboard_win::raw::size(id)
            .ok_or_else(|| Error::Io("clipboard allocation size unavailable".into()))?
            .get();
        check_size(size as u64, cap)?;
        let mut bytes = vec![0; size];
        let copied = clipboard_win::raw::get(id, &mut bytes)
            .map_err(|e| crate::transfer::imp::clip_error("get", e))?;
        if copied != size
            || clipboard_win::raw::size(id).map(std::num::NonZeroUsize::get) != Some(size)
        {
            return Err(Error::Io("clipboard allocation changed during read".into()));
        }
        Ok(bytes)
    }
    fn text_available(&mut self) -> Result<bool> {
        // IsClipboardFormatAvailable is a typed availability query: FALSE
        // means absent; GetClipboardData errors never stand in for absence.
        Ok(clipboard_win::raw::is_format_avail(
            crate::transfer::imp::CF_UNICODETEXT,
        ))
    }
    fn enum_formats(&mut self) -> Result<Vec<u32>> {
        crate::sys::clipboard_formats_checked()
    }
    fn seq(&self) -> Option<String> {
        clipboard_win::raw::seq_num().map(|n| n.get().to_string())
    }
    fn size(&self, id: u32) -> Option<u64> {
        clipboard_win::raw::size(id).map(|n| n.get() as u64)
    }
}

fn check_size(size: u64, cap: u64) -> Result<()> {
    if size > cap {
        return Err(Error::TooLarge("clipboard data exceeds cap".into()));
    }
    Ok(())
}

fn snapshot_opened<B: ClipBackend>(
    backend: &mut B,
    ids: &[u32],
    max_bytes: u64,
    budget: Duration,
) -> Result<SnapshotKind> {
    let started = Instant::now();
    let formats = backend.enum_formats()?;
    let empty = formats.is_empty();
    let mut saved = Vec::new();
    for id in formats {
        if !ids.contains(&id) {
            continue;
        }
        let size = backend
            .size(id)
            .ok_or_else(|| Error::Io("clipboard snapshot size unavailable".into()))?;
        check_size(size, max_bytes)?;
        if started.elapsed() > budget {
            return Err(Error::TooLarge("clipboard snapshot over budget".into()));
        }
        let bytes = backend.get(id, max_bytes)?;
        check_size(bytes.len() as u64, max_bytes)?;
        if started.elapsed() > budget {
            return Err(Error::TooLarge("clipboard snapshot over budget".into()));
        }
        saved.push((id, bytes));
    }
    if started.elapsed() > budget {
        return Err(Error::TooLarge("clipboard snapshot over budget".into()));
    }
    if empty {
        Ok(SnapshotKind::VerifiedEmpty)
    } else if saved.is_empty() {
        Err(Error::Unsupported(
            "clipboard has no recoverable formats".into(),
        ))
    } else {
        Ok(SnapshotKind::Available(saved))
    }
}

fn restore_opened<B: ClipBackend>(backend: &mut B, saved: &SnapshotKind) -> Result<()> {
    if matches!(saved, SnapshotKind::Disabled) {
        return Ok(());
    }
    backend.empty()?;
    if let SnapshotKind::Available(formats) = saved {
        for (id, bytes) in formats {
            backend.set(*id, bytes)?;
        }
    }
    Ok(())
}

pub(crate) fn begin_transaction<B: ClipBackend>(
    backend: &mut B,
    payloads: &[(String, Vec<u8>)],
    restore: bool,
    snapshot_ids: &[u32],
    max_bytes: u64,
    budget: Duration,
) -> std::result::Result<BeginSuccess, BeginError> {
    let before_open_error = |primary| BeginError {
        primary,
        rollback: None,
        close: None,
    };
    let ops = prepare_ops(payloads).map_err(before_open_error)?;
    let temp_text = payloads
        .iter()
        .rev()
        .find(|(mime, _)| mime == "text/plain" || mime == "text/plain;charset=utf-8")
        .map(|(_, bytes)| bytes.clone());
    backend.open().map_err(before_open_error)?;
    let saved = if restore {
        match snapshot_opened(backend, snapshot_ids, max_bytes, budget) {
            Ok(saved) => saved,
            Err(primary) => {
                return Err(BeginError {
                    primary,
                    rollback: None,
                    close: backend.close().err(),
                })
            }
        }
    } else {
        SnapshotKind::Disabled
    };
    let publish = (|| {
        backend.empty()?;
        for (id, bytes) in &ops {
            backend.set(*id, bytes)?;
        }
        Ok(())
    })();
    if let Err(primary) = publish {
        let rollback = if restore {
            restore_opened(backend, &saved).err()
        } else {
            None
        };
        return Err(BeginError {
            primary,
            rollback,
            close: backend.close().err(),
        });
    }
    // This in-open observation is deliberately never promoted to a later
    // value: a legitimate close-side change can conservatively refuse finish.
    let seq = backend
        .seq()
        .filter(|s| s.parse::<u32>().is_ok_and(|n| n != 0));
    if seq.is_none() {
        let primary = Error::Io("clipboard sequence unavailable after publish".into());
        let rollback = if restore {
            restore_opened(backend, &saved).err()
        } else {
            None
        };
        return Err(BeginError {
            primary,
            rollback,
            close: backend.close().err(),
        });
    }
    backend.close().map_err(before_open_error)?;
    Ok(BeginSuccess {
        saved,
        seq,
        temp_text,
    })
}

fn decode_text(bytes: &[u8]) -> Result<String> {
    if !bytes.len().is_multiple_of(2) {
        return Err(Error::Io("clipboard text has an odd byte count".into()));
    }
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|unit| *unit != 0)
        .collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|_| Error::Io("clipboard text is not valid UTF-16".into()))
}

pub(crate) fn finish_transaction<B: ClipBackend>(
    backend: &mut B,
    saved: &SnapshotKind,
    expected_seq: Option<&str>,
    temp_text: Option<&[u8]>,
    restore: bool,
) -> bool {
    const GUARD_CAP: u64 = 4 * 1024 * 1024;
    if !restore || matches!(saved, SnapshotKind::Disabled) {
        return false;
    }
    let Some(expected) = expected_seq.filter(|s| s.parse::<u32>().is_ok_and(|n| n != 0)) else {
        return false;
    };
    if backend.open().is_err() {
        return false;
    }
    let result: Result<bool> = (|| {
        if backend.seq().as_deref() != Some(expected) {
            return Ok(false);
        }
        let available = backend.text_available()?;
        let content_ok = match temp_text {
            Some(text) if available => {
                decode_text(&backend.get(crate::transfer::imp::CF_UNICODETEXT, GUARD_CAP)?)?
                    .as_bytes()
                    == text
            }
            None => !available,
            Some(_) => false,
        };
        if !content_ok || backend.seq().as_deref() != Some(expected) {
            return Ok(false);
        }
        restore_opened(backend, saved)?;
        Ok(true)
    })();
    let close = backend.close();
    result.unwrap_or(false) && close.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::imp::{self, CF_UNICODETEXT};
    use std::cell::Cell;

    // Independent fault toggles belong only to this test adapter.
    #[allow(clippy::struct_excessive_bools)]
    #[derive(Default)]
    struct Backend {
        data: NativeOps,
        events: Vec<&'static str>,
        locked: bool,
        sequence: Cell<Option<u32>>,
        fail_open: bool,
        fail_enum: bool,
        fail_get: bool,
        fail_availability: bool,
        unknown_size: bool,
        oversized_size: bool,
        oversized_read: bool,
        slow_read: bool,
        fail_sets: Vec<usize>,
        sets: usize,
        fail_empty: bool,
        fail_close: bool,
        close_bump: bool,
        read_bump: bool,
        attempt_writer_on_read: bool,
        writer_blocked: bool,
        writer_succeeded: bool,
        caps: Vec<u64>,
        set_ids: Vec<u32>,
        expected_at_close: Option<NativeOps>,
    }

    impl Backend {
        fn original() -> Self {
            crate::text_encoding::testing::ensure_injected_api();
            Self {
                data: vec![(CF_UNICODETEXT, native("old"))],
                sequence: Cell::new(Some(1)),
                ..Self::default()
            }
        }
        fn foreign(&mut self, append: bool) {
            if self.locked {
                self.writer_blocked = true;
                return;
            }
            self.writer_succeeded = true;
            self.events.push("foreign_write");
            if append {
                self.data.push((900, b"foreign append".to_vec()));
            } else {
                self.data = vec![(CF_UNICODETEXT, native("new"))];
            }
            self.sequence
                .set(self.sequence.get().map(|s| if s == 0 { 0 } else { s + 1 }));
        }
        fn bump(&self) {
            self.sequence
                .set(self.sequence.get().map(|s| if s == 0 { 0 } else { s + 1 }));
        }
    }
    impl ClipBackend for Backend {
        fn open(&mut self) -> Result<()> {
            self.events.push("open");
            if self.fail_open || self.locked {
                return Err(Error::Io("open".into()));
            }
            self.locked = true;
            Ok(())
        }
        fn close(&mut self) -> Result<()> {
            self.events.push("close");
            assert!(self.locked);
            if let Some(expected) = &self.expected_at_close {
                assert_eq!(
                    &self.data, expected,
                    "original raw bytes must be restored before close"
                );
            }
            self.locked = false;
            // A writer waiting during the guard gets its opportunity if the
            // implementation releases before the destructive restore.
            if self.attempt_writer_on_read && self.events.iter().rev().nth(1) == Some(&"get") {
                self.foreign(true);
            }
            if self.close_bump {
                self.bump();
            }
            if self.fail_close {
                Err(Error::Io("close".into()))
            } else {
                Ok(())
            }
        }
        fn empty(&mut self) -> Result<()> {
            assert!(self.locked);
            self.events.push("empty");
            if self.fail_empty {
                return Err(Error::Io("empty".into()));
            }
            self.data.clear();
            self.bump();
            Ok(())
        }
        fn set(&mut self, id: u32, bytes: &[u8]) -> Result<()> {
            assert!(self.locked);
            self.events.push("set");
            self.set_ids.push(id);
            self.sets += 1;
            if self.fail_sets.contains(&self.sets) {
                return Err(Error::Io(format!("set {}", self.sets)));
            }
            if let Some((_, value)) = self.data.iter_mut().find(|(key, _)| *key == id) {
                *value = bytes.to_vec();
            } else {
                self.data.push((id, bytes.to_vec()));
            }
            self.bump();
            Ok(())
        }
        fn get(&mut self, id: u32, cap: u64) -> Result<Vec<u8>> {
            assert!(self.locked);
            self.events.push("get");
            self.caps.push(cap);
            if self.attempt_writer_on_read {
                self.foreign(true);
            }
            if self.read_bump {
                self.bump();
            }
            if self.fail_get {
                return Err(Error::Io("read: localized platform failure".into()));
            }
            if self.slow_read {
                std::thread::sleep(Duration::from_millis(3));
            }
            if self.oversized_read {
                return Ok(vec![0; 65]);
            }
            let bytes = self
                .data
                .iter()
                .find(|(key, _)| *key == id)
                .ok_or_else(|| Error::Io("missing data despite read request".into()))?
                .1
                .clone();
            check_size(bytes.len() as u64, cap)?;
            Ok(bytes)
        }
        fn text_available(&mut self) -> Result<bool> {
            self.events.push("available");
            if self.fail_availability {
                Err(Error::Io("availability".into()))
            } else {
                Ok(self.data.iter().any(|(id, _)| *id == CF_UNICODETEXT))
            }
        }
        fn enum_formats(&mut self) -> Result<Vec<u32>> {
            self.events.push("enum");
            if self.fail_enum {
                Err(Error::Io("enumeration".into()))
            } else {
                Ok(self.data.iter().map(|(id, _)| *id).collect())
            }
        }
        fn seq(&self) -> Option<String> {
            self.sequence.get().map(|s| s.to_string())
        }
        fn size(&self, id: u32) -> Option<u64> {
            if self.unknown_size {
                None
            } else if self.oversized_size {
                Some(u64::MAX)
            } else {
                self.data
                    .iter()
                    .find(|(key, _)| *key == id)
                    .map(|(_, bytes)| bytes.len() as u64)
            }
        }
    }
    fn native(text: &str) -> Vec<u8> {
        crate::transfer::encode_native("text/plain", text.as_bytes()).unwrap()
    }
    fn payload() -> Vec<(String, Vec<u8>)> {
        vec![("text/plain".into(), b"new".to_vec())]
    }
    fn begin(backend: &mut Backend) -> std::result::Result<BeginSuccess, BeginError> {
        begin_transaction(
            backend,
            &payload(),
            true,
            &[CF_UNICODETEXT],
            64,
            Duration::from_secs(1),
        )
    }
    fn finish(backend: &mut Backend, swap: &BeginSuccess) -> bool {
        finish_transaction(
            backend,
            &swap.saved,
            swap.seq.as_deref(),
            swap.temp_text.as_deref(),
            true,
        )
    }

    struct Chord {
        events: Vec<ChordAction>,
        fail: Option<ChordAction>,
        init_error: bool,
        release_error: bool,
    }
    impl ChordDriver for Chord {
        fn init(&mut self) -> std::result::Result<(), String> {
            if self.init_error {
                Err("init".into())
            } else {
                Ok(())
            }
        }
        fn perform(&mut self, action: ChordAction) -> std::result::Result<(), String> {
            self.events.push(action.clone());
            if self.fail.as_ref() == Some(&action)
                || (self.release_error && action == ChordAction::ReleaseCtrl)
            {
                Err(format!("{action:?}"))
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn c1_chord_release_order_and_primary_error() {
        for fail in [
            None,
            Some(ChordAction::PressCtrl),
            Some(ChordAction::ClickV),
            Some(ChordAction::ReleaseCtrl),
        ] {
            let mut chord = Chord {
                events: vec![],
                fail: fail.clone(),
                init_error: false,
                release_error: true,
            };
            let error = run_chord(&mut chord).unwrap_err();
            if fail == Some(ChordAction::PressCtrl) {
                assert_eq!(chord.events, [ChordAction::PressCtrl]);
                assert_eq!(error, "PressCtrl");
            } else {
                assert_eq!(
                    chord.events,
                    [
                        ChordAction::PressCtrl,
                        ChordAction::ClickV,
                        ChordAction::ReleaseCtrl
                    ]
                );
                assert_eq!(
                    error,
                    if fail == Some(ChordAction::ClickV) {
                        "ClickV"
                    } else {
                        "ReleaseCtrl"
                    }
                );
            }
        }
        let mut chord = Chord {
            events: vec![],
            fail: None,
            init_error: false,
            release_error: false,
        };
        assert!(run_chord(&mut chord).is_ok());
        chord.events.clear();
        chord.init_error = true;
        assert_eq!(run_chord(&mut chord).unwrap_err(), "init");
        assert_eq!(chord.events, []);
    }
    #[test]
    fn c3_first_and_later_publication_errors_restore_before_only_close() {
        for failure in [1, 2, 3, 4, 5] {
            let mut backend = Backend::original();
            backend.data = vec![
                (13, vec![65, 0, 0, 0, 99, 0]),
                (16, vec![0x09, 4, 0, 0]),
                (1, vec![0xe9, 0, 99]),
                (7, vec![0x82, 0, 88]),
            ];
            let original = backend.data.clone();
            backend.expected_at_close = Some(original.clone());
            backend.fail_sets = vec![failure];
            let payloads = vec![
                ("text/plain".into(), b"new".to_vec()),
                ("x-os/extra".into(), vec![1]),
            ];
            let error = begin_transaction(
                &mut backend,
                &payloads,
                true,
                &[13, 16, 1, 7],
                64,
                Duration::from_secs(1),
            )
            .unwrap_err();
            assert_eq!(error.primary.to_string(), format!("io: set {failure}"));
            assert!(error.rollback.is_none());
            assert!(error.close.is_none());
            assert_eq!(backend.data, original);
            let emitted = [13, 16, 1, 7, imp::id_for_mime("x-os/extra").unwrap()];
            assert_eq!(&backend.set_ids[..failure], &emitted[..failure]);
            assert_eq!(&backend.set_ids[failure..], &[13, 16, 1, 7]);
            assert_eq!(backend.events.iter().filter(|e| **e == "open").count(), 1);
            assert_eq!(backend.events.iter().filter(|e| **e == "close").count(), 1);
            assert_eq!(backend.events.last(), Some(&"close"));
        }
    }
    #[test]
    fn c4_snapshot_uncertainty_never_empties() {
        for fault in 0..8 {
            let mut backend = Backend::original();
            match fault {
                0 => backend.fail_enum = true,
                1 => backend.fail_get = true,
                2 => backend.unknown_size = true,
                3 => backend.oversized_size = true,
                4 => backend.oversized_read = true,
                5 => backend.slow_read = true,
                6 => backend.data = vec![(999, vec![1])],
                7 => backend.fail_open = true,
                _ => unreachable!(),
            }
            let original = backend.data.clone();
            let budget = if fault == 5 {
                Duration::from_millis(1)
            } else {
                Duration::from_secs(1)
            };
            assert!(begin_transaction(
                &mut backend,
                &payload(),
                true,
                &[CF_UNICODETEXT],
                64,
                budget
            )
            .is_err());
            assert_eq!(backend.data, original);
            assert!(!backend.events.contains(&"empty"));
        }
    }
    #[test]
    fn c4_validation_and_disabled_restore_and_verified_empty() {
        for payloads in [
            vec![],
            vec![("text/plain".into(), vec![255])],
            vec![("text/html".into(), vec![255])],
            vec![("text/uri-list".into(), b"https://example.com".to_vec())],
        ] {
            let mut backend = Backend::original();
            assert!(
                begin_transaction(&mut backend, &payloads, true, &[], 64, Duration::ZERO).is_err()
            );
            assert_eq!(backend.events, [] as [&str; 0]);
        }
        let mut backend = Backend::original();
        backend.fail_enum = true;
        backend.fail_sets = vec![1];
        assert!(
            begin_transaction(&mut backend, &payload(), false, &[], 0, Duration::ZERO).is_err()
        );
        assert!(!backend.events.contains(&"enum"));
        assert_eq!(backend.events.iter().filter(|e| **e == "empty").count(), 1);
        let mut backend = Backend::original();
        backend.data.clear();
        let swap = begin(&mut backend).unwrap();
        assert_eq!(swap.saved, SnapshotKind::VerifiedEmpty);
        assert!(finish(&mut backend, &swap));
        assert_eq!(backend.data, []);
        backend.events.clear();
        assert!(!finish_transaction(
            &mut backend,
            &SnapshotKind::Disabled,
            swap.seq.as_deref(),
            swap.temp_text.as_deref(),
            true
        ));
        assert_eq!(backend.events, [] as [&str; 0]);
    }
    #[test]
    fn c4_primary_rollback_close_fail_independently() {
        for rollback_fail in [false, true] {
            for close_fail in [false, true] {
                let mut backend = Backend::original();
                backend.fail_sets = if rollback_fail { vec![1, 2] } else { vec![1] };
                backend.fail_close = close_fail;
                let error = begin(&mut backend).unwrap_err();
                assert_eq!(error.rollback.is_some(), rollback_fail);
                assert_eq!(error.close.is_some(), close_fail);
                assert_eq!(error.into_primary().to_string(), "io: set 1");
                assert_eq!(backend.events.last(), Some(&"close"));
            }
        }
        let mut backend = Backend::original();
        backend.fail_close = true;
        assert_eq!(
            begin(&mut backend).unwrap_err().into_primary().to_string(),
            "io: close"
        );
        assert_eq!(backend.events.iter().filter(|e| **e == "empty").count(), 1);
        let mut backend = Backend::original();
        backend.fail_empty = true;
        let error = begin(&mut backend).unwrap_err();
        assert!(error.rollback.is_some());
    }
    #[test]
    fn c4_missing_publish_receipt_rolls_back_without_fresh_adoption() {
        for sequence in [None, Some(0)] {
            let mut backend = Backend::original();
            let original = backend.data.clone();
            backend.sequence.set(sequence);
            let error = begin(&mut backend).unwrap_err();
            assert!(error.primary.to_string().contains("sequence unavailable"));
            assert_eq!(backend.data, original);
            assert_eq!(backend.events.last(), Some(&"close"));
        }
    }
    #[test]
    fn c5_same_text_republish_and_no_empty_append_win() {
        for append in [false, true] {
            let mut backend = Backend::original();
            let swap = begin(&mut backend).unwrap();
            backend.foreign(append);
            let foreign = backend.data.clone();
            backend.events.clear();
            assert!(!finish(&mut backend, &swap));
            assert_eq!(backend.data, foreign);
            assert!(!backend.events.contains(&"empty"));
        }
    }
    #[test]
    fn c5_guard_writer_attempt_is_blocked_through_restore() {
        let mut backend = Backend::original();
        let original = backend.data.clone();
        let swap = begin(&mut backend).unwrap();
        backend.events.clear();
        backend.attempt_writer_on_read = true;
        assert!(finish(&mut backend, &swap));
        assert!(backend.writer_blocked);
        assert!(!backend.writer_succeeded);
        assert_eq!(backend.data, original);
        assert_eq!(
            backend.events,
            ["open", "available", "get", "empty", "set", "close"]
        );
    }
    #[test]
    fn c6_sequence_content_and_read_uncertainty_refuse() {
        for fault in 0..10 {
            let mut backend = Backend::original();
            let mut swap = begin(&mut backend).unwrap();
            match fault {
                0 => swap.seq = None,
                1 => swap.seq = Some("0".into()),
                2 => backend.sequence.set(None),
                3 => backend.sequence.set(Some(0)),
                4 => backend.close_bump = true,
                5 => backend.read_bump = true,
                6 => backend.data[0].1 = native("different"),
                7 => backend.fail_get = true,
                8 => backend.fail_availability = true,
                9 => backend.data[0].1 = vec![0; 4 * 1024 * 1024 + 1],
                _ => unreachable!(),
            }
            if fault == 4 {
                backend.bump();
            }
            let current = backend.data.clone();
            backend.events.clear();
            assert!(!finish(&mut backend, &swap), "fault {fault}");
            assert_eq!(backend.data, current);
            assert!(!backend.events.contains(&"empty"));
            assert!(backend.caps.iter().all(|cap| *cap <= 4 * 1024 * 1024));
        }
    }
    #[test]
    fn c6_close_side_sequence_change_is_never_promoted() {
        let mut backend = Backend::original();
        backend.close_bump = true;
        let swap = begin(&mut backend).unwrap();
        assert_ne!(backend.seq(), swap.seq);
        let current = backend.data.clone();
        backend.events.clear();
        assert!(!finish(&mut backend, &swap));
        assert_eq!(backend.data, current);
        assert!(!backend.events.contains(&"empty"));
    }
    #[test]
    fn c6_text_guard_decodes_native_utf16_and_refuses_malformed() {
        for invalid in [false, true] {
            let mut backend = Backend::original();
            let swap = begin(&mut backend).unwrap();
            if invalid {
                backend.data[0].1 = vec![0, 0xd8, 0, 0];
            } else {
                backend.data[0].1.extend([0, 0, 0, 0]);
            }
            assert_eq!(finish(&mut backend, &swap), !invalid);
        }
    }
    #[test]
    fn c6_nontext_typed_absence_and_read_failure_are_distinct() {
        for text_present in [false, true] {
            for read_failure in [false, true] {
                let mut backend = Backend::original();
                let swap = begin_transaction(
                    &mut backend,
                    &[("x-os/extra".into(), vec![1])],
                    true,
                    &[CF_UNICODETEXT],
                    64,
                    Duration::from_secs(1),
                )
                .unwrap();
                if text_present {
                    backend.data.push((CF_UNICODETEXT, native("foreign")));
                }
                backend.fail_get = read_failure;
                backend.events.clear();
                assert_eq!(finish(&mut backend, &swap), !text_present);
                assert!(!backend.events.contains(&"get"));
            }
        }
        let mut backend = Backend::original();
        let swap = begin(&mut backend).unwrap();
        backend.fail_get = true;
        assert!(!finish(&mut backend, &swap));
        assert!(backend.events.contains(&"get"));
    }
    #[test]
    fn c7_publication_native_framing_control() {
        let mut backend = Backend::original();
        let payloads = vec![
            ("text/html".into(), b"<b>native</b>".to_vec()),
            ("text/uri-list".into(), b"file:///C:/A.txt\r\n".to_vec()),
        ];
        begin_transaction(
            &mut backend,
            &payloads,
            true,
            &[CF_UNICODETEXT],
            1024,
            Duration::from_secs(1),
        )
        .unwrap();
        let html = backend
            .data
            .iter()
            .find(|(id, _)| *id == imp::id_for_mime("text/html").unwrap())
            .unwrap();
        assert_eq!(
            crate::html::parse_cf_html(&html.1).unwrap(),
            "<b>native</b>"
        );
        let files = backend
            .data
            .iter()
            .find(|(id, _)| *id == imp::CF_HDROP)
            .unwrap();
        assert_eq!(
            &files.1[..20],
            &[20_u32, 0, 0, 0, 1]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            &files.1[20..],
            "C:\\A.txt\0\0"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        );
    }
    fn one_pixel_png() -> Vec<u8> {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        png.into_inner()
    }

    #[test]
    fn c7_native_encoders_and_raw_snapshot_order_full_bytes() {
        crate::text_encoding::set_injected_locale(0x0409, 1252, 437);
        let files = b"file:///C:/A%20B.txt\r\nfile:///D:/music.txt\r\n";
        let html = b"<b>hello</b>";
        let payloads = vec![
            (
                "text/plain;charset=utf-8".into(),
                "hello 🌍".as_bytes().to_vec(),
            ),
            ("text/html".into(), html.to_vec()),
            ("text/uri-list".into(), files.to_vec()),
            ("image/png".into(), one_pixel_png()),
        ];
        let ops = prepare_ops(&payloads).unwrap();
        assert_eq!(ops.len(), 9);
        assert_eq!(ops[0].0, crate::text_encoding::CF_UNICODETEXT);
        assert_eq!(
            ops[0].1,
            "hello 🌍"
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        );
        assert_eq!(ops[1].0, crate::text_encoding::CF_LOCALE);
        assert_eq!(ops[1].1.len(), 4);
        assert_eq!(ops[2].0, crate::text_encoding::CF_TEXT);
        assert_eq!(ops[3].0, crate::text_encoding::CF_OEMTEXT);
        assert_eq!(
            crate::html::parse_cf_html(&ops[4].1).unwrap(),
            "<b>hello</b>"
        );
        assert_eq!(
            &ops[5].1[..20],
            &[20_u32, 0, 0, 0, 1]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            &ops[5].1[20..],
            "C:\\A B.txt\0D:\\music.txt\0\0"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            ops[6],
            (
                imp::register(imp::DROP_EFFECT_FORMAT).unwrap(),
                1_u32.to_le_bytes().to_vec()
            )
        );
        assert_eq!(ops[7].1, payloads[3].1);
        assert_eq!(
            ops[8],
            (imp::CF_DIB, crate::dib::png_to_dib(&payloads[3].1).unwrap())
        );
        let explicit = vec![
            ("text/uri-list".into(), files.to_vec()),
            (imp::DROP_EFFECT_MIME.into(), 2_u32.to_le_bytes().to_vec()),
        ];
        assert_eq!(prepare_ops(&explicit).unwrap().len(), 2);
        let framed = crate::html::build_cf_html("<p>raw</p>");
        assert_eq!(
            crate::transfer::encode_native("text/html", &framed).unwrap(),
            framed
        );
        let mut original = vec![
            (CF_UNICODETEXT, vec![65, 0, 0, 0, 99, 0]),
            (imp::CF_DIB, vec![3, 0, 2]),
            (imp::CF_HDROP, vec![4, 0, 5]),
        ];
        for name in [
            "HTML Format",
            "Rich Text Format",
            "PNG",
            imp::DROP_EFFECT_FORMAT,
            crate::optout::EXCLUDE_FORMAT,
            crate::optout::HISTORY_OPT_OUT,
            crate::optout::CLOUD_OPT_OUT,
            crate::optout::VIEWER_IGNORE,
        ] {
            original.push((imp::register(name).unwrap(), name.as_bytes().to_vec()));
        }
        let ids = crate::paste::restore_ids().unwrap();
        let mut backend = Backend::original();
        backend.data.clone_from(&original);
        let swap = begin_transaction(
            &mut backend,
            &payloads,
            true,
            &ids,
            1024,
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(backend.data, ops);
        assert_eq!(swap.saved, SnapshotKind::Available(original.clone()));
        assert!(finish(&mut backend, &swap));
        assert_eq!(backend.data, original);
    }

    // Small acceptance set B: production-shared preparation controls.
    // This suite uses the actual production prepare_ops and verifies emitted IDs/order/bytes
    // via independent expected values, not by calling the same converter to generate expectations.
    // Linux injected converter is explicitly NOT a native Windows API proof; real WideCharToMultiByte
    // behavior requires future Windows Go execution.

    #[test]
    fn b_actual_emitted_operations_and_independent_bytes() {
        crate::text_encoding::set_injected_locale(0x0409, 1252, 437);
        for (text, unicode, ansi, oem) in [
            ("", vec![0, 0], vec![0], vec![0]),
            (
                "café",
                vec![99, 0, 97, 0, 102, 0, 0xe9, 0, 0, 0],
                vec![99, 97, 102, 0xe9, 0],
                vec![99, 97, 102, 0x82, 0],
            ),
        ] {
            let actual = prepare_ops(&[("text/plain".into(), text.as_bytes().to_vec())]).unwrap();
            assert_eq!(
                actual,
                vec![(13, unicode), (16, vec![9, 4, 0, 0]), (1, ansi), (7, oem)]
            );
        }
        crate::text_encoding::clear_injected_locale();
    }

    #[test]
    fn b_text_empty_produces_four_ops_with_nul_terminators() {
        crate::text_encoding::clear_injected_locale();
        crate::text_encoding::set_injected_locale(0x0409, 1252, 437);
        let payloads = vec![("text/plain".into(), b"".to_vec())];
        let ops = prepare_ops(&payloads).unwrap();
        assert_eq!(ops.len(), 4);
        assert_eq!(ops[0].0, crate::text_encoding::CF_UNICODETEXT);
        assert_eq!(ops[1].0, crate::text_encoding::CF_LOCALE);
        assert_eq!(ops[2].0, crate::text_encoding::CF_TEXT);
        assert_eq!(ops[3].0, crate::text_encoding::CF_OEMTEXT);
        // Unicode: UTF-16LE NUL only
        assert_eq!(ops[0].1, vec![0, 0]);
        // Locale: 0x0409 LE
        assert_eq!(ops[1].1, 0x0409_u32.to_le_bytes().to_vec());
        // ANSI/OEM empty: single NUL byte each
        assert_eq!(ops[2].1, vec![0]);
        assert_eq!(ops[3].1, vec![0]);
        crate::text_encoding::clear_injected_locale();
    }

    #[test]
    fn b_text_e_acute_distinguishes_ansi_437_vs_oem() {
        crate::text_encoding::clear_injected_locale();
        crate::text_encoding::set_injected_locale(0x040C, 1252, 437);
        let payloads = vec![("text/plain".into(), "café".as_bytes().to_vec())];
        let ops = prepare_ops(&payloads).unwrap();
        assert_eq!(ops.len(), 4);
        // Independent expected bytes (not derived from converter under test)
        let unicode_expected: Vec<u8> = "café"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(ops[0].1, unicode_expected);
        assert_eq!(ops[1].1, 0x040C_u32.to_le_bytes().to_vec());
        // 1252: é -> 0xE9, 437: é -> 0x82 (distinct)
        assert_eq!(ops[2].1, vec![b'c', b'a', b'f', 0xE9, 0]);
        assert_eq!(ops[3].1, vec![b'c', b'a', b'f', 0x82, 0]);
        crate::text_encoding::clear_injected_locale();
    }

    #[test]
    fn b_text_multibyte_page_932_encodes_japanese() {
        crate::text_encoding::clear_injected_locale();
        crate::text_encoding::set_injected_locale(0x0411, 932, 437);
        let payloads = vec![("text/plain".into(), "あ".as_bytes().to_vec())];
        let ops = prepare_ops(&payloads).unwrap();
        assert_eq!(ops.len(), 4);
        let unicode_expected: Vec<u8> = "あ"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(ops[0].1, unicode_expected);
        // 932 Shift-JIS: あ -> 0x82 0xA0
        assert_eq!(ops[2].1, vec![0x82, 0xA0, 0]);
        // OEM still 437, unrepresentable -> '?'
        // Our fake uses '?' for unrepresentable in 437 for this char
        assert_eq!(ops[3].1, vec![b'?', 0]);
        crate::text_encoding::clear_injected_locale();
    }

    #[test]
    fn b_text_supplementary_and_replacement_preserves_unicode() {
        crate::text_encoding::clear_injected_locale();
        crate::text_encoding::set_injected_locale(0x0409, 1252, 437);
        let payloads = vec![("text/plain".into(), "a🌍b".as_bytes().to_vec())];
        let ops = prepare_ops(&payloads).unwrap();
        assert_eq!(ops.len(), 4);
        let unicode_expected: Vec<u8> = "a🌍b"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(ops[0].1, unicode_expected);
        // Supplementary U+1F30D cannot be represented in 1252/437 -> '?' 0x3F, Unicode authoritative exact
        assert_eq!(ops[2].1, vec![b'a', b'?', b'b', 0]);
        assert_eq!(ops[3].1, vec![b'a', b'?', b'b', 0]);
        crate::text_encoding::clear_injected_locale();
    }

    #[test]
    fn b_text_invalid_utf8_and_embedded_nul_fail_before_open() {
        for bad in [vec![0xFF, 0xFF], b"a\0b".to_vec()] {
            crate::text_encoding::clear_injected_locale();
            let mut backend = Backend::original();
            let original = backend.data.clone();
            let payloads = vec![("text/plain".into(), bad)];
            let err = begin_transaction(
                &mut backend,
                &payloads,
                true,
                &[CF_UNICODETEXT],
                64,
                Duration::from_secs(1),
            )
            .unwrap_err();
            assert!(
                err.primary.to_string().contains("UTF-8")
                    || err.primary.to_string().contains("NUL")
                    || err.primary.to_string().contains("payload"),
                "primary={}",
                err.primary
            );
            assert_eq!(backend.data, original);
            assert!(!backend.events.contains(&"open"));
            assert!(!backend.events.contains(&"empty"));
            crate::text_encoding::clear_injected_locale();
        }
    }

    #[test]
    fn b_text_failed_locale_conversion_sizing_fail_before_open() {
        use crate::text_encoding::testing::{inject_fault, Fault};
        let payloads = vec![("text/plain".into(), b"hi".to_vec())];
        for (lcid, ansi, oem) in [
            (0, 1252, 437),
            (0x007f, 1252, 437),
            (0x0400, 1252, 437),
            (0x0800, 1252, 437),
            (0x0c00, 1252, 437),
            (0x1000, 1252, 437),
            (0x1400, 1252, 437),
            (0x0501, 1252, 437),
            (0x09ff, 1252, 437),
            (0x05fe, 1252, 437),
            (0x105fe, 1252, 437),
            (0x0100_0409, 1252, 437),
            (0x0409, 0, 437),
            (0x0409, 1200, 437),
            (0x0409, 1252, 0),
            (0x0409, 1252, 65000),
        ] {
            crate::text_encoding::set_injected_locale(lcid, ansi, oem);
            let mut backend = Backend::original();
            let original = backend.data.clone();
            assert!(begin_transaction(
                &mut backend,
                &payloads,
                true,
                &[CF_UNICODETEXT],
                64,
                Duration::from_secs(1)
            )
            .is_err());
            assert_eq!(backend.data, original);
            assert_eq!(backend.events, [] as [&str; 0]);
        }
        for fault in [
            Fault::LocaleReturn(0),
            Fault::LocaleReturn(1),
            Fault::LocaleReturn(3),
            Fault::Size(1252, 0),
            Fault::Size(437, 0),
            Fault::Size(1252, -1),
            Fault::Write(1252, 0),
            Fault::Write(437, 0),
            Fault::Write(1252, 2),
            Fault::Write(1252, 4),
            Fault::MissingNul(1252),
            Fault::MissingNul(437),
        ] {
            crate::text_encoding::set_injected_locale(0x0409, 1252, 437);
            inject_fault(fault);
            let mut backend = Backend::original();
            let original = backend.data.clone();
            let error = begin_transaction(
                &mut backend,
                &payloads,
                true,
                &[CF_UNICODETEXT],
                64,
                Duration::from_secs(1),
            )
            .unwrap_err();
            assert!(error.primary.to_string().contains("87"));
            assert_eq!(backend.data, original);
            assert_eq!(backend.events, [] as [&str; 0]);
        }
        // An installed-page/API error is refused rather than retried with
        // weaker flags or used as a Unicode-only publication.
        crate::text_encoding::set_injected_locale(0x0409, 12345, 437);
        inject_fault(Fault::Size(12345, 0));
        let mut backend = Backend::original();
        assert!(begin_transaction(
            &mut backend,
            &payloads,
            true,
            &[CF_UNICODETEXT],
            64,
            Duration::from_secs(1)
        )
        .is_err());
        assert_eq!(backend.events, [] as [&str; 0]);
        crate::text_encoding::clear_injected_locale();
    }

    #[test]
    fn b_text_concrete_lcid_and_actual_conversion_call_contract() {
        use crate::text_encoding::testing::{calls, Call};
        // sah-RU is a real concrete LCID; page availability is a separate check.
        for (ansi, oem, flags) in [(1252, 437, 0x400), (65001, 54936, 8)] {
            crate::text_encoding::set_injected_locale(0x0485, ansi, oem);
            let ops = prepare_ops(&[("text/plain".into(), b"hi".to_vec())]).unwrap();
            assert_eq!(ops[1], (16, vec![0x85, 4, 0, 0]));
            assert_eq!(
                calls(),
                vec![
                    Call::Lcid,
                    Call::Ansi(0x0485),
                    Call::Oem,
                    Call::Convert {
                        cp: ansi,
                        flags,
                        wide: vec![104, 105, 0],
                        count: 3,
                        output: 0
                    },
                    Call::Convert {
                        cp: ansi,
                        flags,
                        wide: vec![104, 105, 0],
                        count: 3,
                        output: 3
                    },
                    Call::Convert {
                        cp: oem,
                        flags,
                        wide: vec![104, 105, 0],
                        count: 3,
                        output: 0
                    },
                    Call::Convert {
                        cp: oem,
                        flags,
                        wide: vec![104, 105, 0],
                        count: 3,
                        output: 3
                    },
                ]
            );
        }
        crate::text_encoding::clear_injected_locale();
    }

    #[test]
    fn b_injected_converter_is_software_evidence_on_both_platforms() {
        // This test documents that Linux headless verification uses a fake injected
        // converter and does NOT prove native Windows WideCharToMultiByte behavior.
        // Real converter execution requires a Windows Go environment with actual Win32 APIs.
        crate::text_encoding::clear_injected_locale();
        crate::text_encoding::set_injected_locale(0x0409, 1252, 437);
        let payloads = vec![("text/plain".into(), "é".as_bytes().to_vec())];
        let ops = prepare_ops(&payloads).unwrap();
        // Fake distinguishes 1252 vs 437, but on Windows the actual bytes must be verified natively.
        assert_eq!(ops[2].1, vec![0xE9, 0]);
        assert_eq!(ops[3].1, vec![0x82, 0]);
        crate::text_encoding::clear_injected_locale();
    }

    #[test]
    #[cfg(windows)]
    fn b_windows_converter_compiles_and_requires_go() {
        // This test compiles now on Windows and verifies the actual Win32 path can be invoked.
        // It requires a future Windows Go to execute and is not run on Linux.
        crate::text_encoding::clear_injected_locale();
        let payloads = vec![("text/plain".into(), "héllo 🌍".as_bytes().to_vec())];
        let ops = prepare_ops(&payloads).unwrap();
        assert_eq!(ops.len(), 4);
        assert_eq!(ops[0].0, 13);
        assert_eq!(ops[1].0, 16);
        assert_eq!(ops[2].0, 1);
        assert_eq!(ops[3].0, 7);
        // Unicode must be exact with supplementary and NUL
        let expected_unicode: Vec<u8> = "héllo 🌍"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(ops[0].1, expected_unicode);
    }

    #[test]
    fn b_text_locale_selected_once_per_batch() {
        crate::text_encoding::clear_injected_locale();
        crate::text_encoding::set_injected_locale(0x0409, 1252, 437);
        let payloads = vec![
            ("text/plain".into(), b"a".to_vec()),
            ("text/plain;charset=utf-8".into(), b"b".to_vec()),
        ];
        let ops = prepare_ops(&payloads).unwrap();
        // Two text payloads should each produce 4 ops, sharing the same LCID, total 8
        assert_eq!(ops.len(), 8);
        assert_eq!(ops[0].0, crate::text_encoding::CF_UNICODETEXT);
        assert_eq!(ops[4].0, crate::text_encoding::CF_UNICODETEXT);
        assert_eq!(ops[1].1, ops[5].1);
        let calls = crate::text_encoding::testing::calls();
        assert_eq!(
            calls
                .iter()
                .filter(|call| **call == crate::text_encoding::testing::Call::Lcid)
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| matches!(call, crate::text_encoding::testing::Call::Ansi(_)))
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| **call == crate::text_encoding::testing::Call::Oem)
                .count(),
            1
        );
        crate::text_encoding::clear_injected_locale();
    }
}

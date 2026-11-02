//! Task 71: controls traverse the actual lazy-read adapter and real SQLite Store.
//! Source only until the lead runs the authorized positive/negative gates.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use clipmonger_core::{Config, Store};
use clipmonger_daemon::{DaemonState, OversizeEntry};
use clipmonger_platform::traits::ClipboardTransfer;
use clipmonger_platform::types::{
    Affinity, CaptureEvent, CaptureOrigin, Error, FormatOffer, PayloadMap,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

static NEXT_PROFILE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    path: PathBuf,
    state: DaemonState,
}

impl Fixture {
    fn new(config: Config) -> Self {
        let nonce = NEXT_PROFILE.fetch_add(1, Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("cm-task71-{}-{now}-{nonce}", std::process::id()));
        std::fs::create_dir(&path).expect("fresh owned test directory");
        let state = DaemonState::with_config(Store::open(&path).expect("real store"), config);
        Self { path, state }
    }

    fn history(&self) -> Vec<Value> {
        self.state
            .handle("history.list", &json!({}))
            .expect("history list")["clips"]
            .as_array()
            .expect("history array")
            .clone()
    }

    fn scalar(&self, sql: &str) -> i64 {
        Connection::open_with_flags(
            self.path.join("clipstore.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("read-only SQL observer")
        .query_row(sql, [], |row| row.get(0))
        .expect("actual SQL scalar")
    }

    fn assert_empty(&self, privacy_skips: i64) {
        assert!(self.history().is_empty());
        for table in ["clips", "clip_payloads", "clip_blobs", "clips_fts"] {
            assert_eq!(
                self.scalar(&format!("SELECT COUNT(*) FROM {table}")),
                0,
                "{table}"
            );
        }
        assert_eq!(
            self.scalar("SELECT CAST(v AS INTEGER) FROM meta WHERE k='privacy_skips'"),
            privacy_skips
        );
        assert_eq!(blob_files(&self.path.join("blobs")), 0);
    }

    fn assert_stub(&self, mime: &str) -> Value {
        let history = self.history();
        assert_eq!(history.len(), 1, "whole capture has one row");
        let clip = &history[0];
        assert!(clip["preview"].as_str().unwrap().contains("Too large"));
        assert!(clip["preview"].as_str().unwrap().contains(mime));
        assert!(clip["preview"].as_str().unwrap().chars().count() <= 200);
        assert!(clip["search_text"].as_str().unwrap().len() <= 820);
        assert_eq!(
            clip["formats"],
            json!([]),
            "Store derives formats from actual payload rows"
        );
        assert_eq!(clip["bytes_total"], json!(0));
        assert!(clip["source_app"].is_null());
        assert_eq!(self.scalar("SELECT COUNT(*) FROM clip_payloads"), 0);
        assert_eq!(self.scalar("SELECT COUNT(*) FROM clip_blobs"), 0);
        assert_eq!(blob_files(&self.path.join("blobs")), 0);
        let id = clip["id"].as_str().unwrap().parse().expect("UUID");
        let store = Store::open(&self.path).expect("second real reader");
        let (stored, payloads) = store
            .get_clip(id)
            .expect("actual payload reader")
            .expect("persisted row");
        assert!(payloads.is_empty());
        assert!(stored.formats.is_empty());
        let got = self
            .state
            .handle("history.get", &json!({"clip": id.to_string()}))
            .expect("history.get");
        assert_eq!(got["clip"]["bytes_total"], json!(0));
        assert_eq!(got["clip"]["formats"], json!([]));
        clip.clone()
    }
}

fn blob_files(path: &Path) -> usize {
    std::fs::read_dir(path)
        .expect("blob directory")
        .map(|entry| {
            let path = entry.expect("blob entry").path();
            if path.is_dir() {
                blob_files(&path)
            } else {
                1
            }
        })
        .sum()
}

fn config(cap: u64) -> Config {
    let mut config = Config::default();
    config.monitor.max_bytes = cap;
    config
}

enum Read {
    Bytes(Vec<u8>),
    TooLarge,
    Io,
}

struct Transfer {
    script: BTreeMap<String, Read>,
    reads: RefCell<Vec<(String, u64)>>,
}

impl Transfer {
    fn new(script: impl IntoIterator<Item = (&'static str, Read)>) -> Self {
        Self {
            script: script
                .into_iter()
                .map(|(mime, read)| (mime.to_string(), read))
                .collect(),
            reads: RefCell::default(),
        }
    }
}

impl ClipboardTransfer for Transfer {
    const AFFINITY: Affinity = Affinity::Worker;
    fn formats(&self) -> clipmonger_platform::types::Result<Vec<String>> {
        Ok(self.script.keys().cloned().collect())
    }
    fn read(&self, mime: &str, cap: u64) -> clipmonger_platform::types::Result<Vec<u8>> {
        self.reads.borrow_mut().push((mime.to_string(), cap));
        match self.script.get(mime).expect("unexpected production read") {
            Read::Bytes(bytes) => Ok(bytes.clone()),
            Read::TooLarge => Err(Error::TooLarge(
                "bounded synthetic native guard".to_string(),
            )),
            Read::Io => Err(Error::Io(
                "TooLarge text is not a typed oversize".to_string(),
            )),
        }
    }
    fn write(&self, _: &PayloadMap) -> clipmonger_platform::types::Result<()> {
        panic!("capture must not write clipboard")
    }
    fn unchanged_indicator(&self) -> Option<String> {
        None
    }
}

fn event(offers: &[(&str, Option<u64>)], sensitive: bool) -> CaptureEvent {
    CaptureEvent {
        origin: CaptureOrigin::Clipboard,
        sensitive,
        offers: offers
            .iter()
            .map(|(mime, bytes_hint)| FormatOffer {
                mime: (*mime).to_string(),
                bytes_hint: *bytes_hint,
            })
            .collect(),
    }
}

fn capture(fixture: &Fixture, event: &CaptureEvent, transfer: &Transfer) -> bool {
    fixture
        .state
        .capture_event("windows", event, transfer, |_| {
            panic!("unexpected producer error")
        })
        .expect("actual adapter result")
}

#[test]
fn known_too_large_preflights_all_wanted_offers_before_any_read() {
    let fixture = Fixture::new(config(4));
    let transfer = Transfer::new([]);
    assert!(capture(
        &fixture,
        &event(&[("text/plain", Some(2)), ("image/png", Some(5))], false),
        &transfer
    ));
    assert!(transfer.reads.borrow().is_empty());
    let clip = fixture.assert_stub("image/png");
    assert!(clip["preview"].as_str().unwrap().contains("5 B"));
}

#[test]
fn stored_metadata_capture_can_be_pinned_and_unpinned() {
    let fixture = Fixture::new(config(4));
    let transfer = Transfer::new([]);
    assert!(capture(
        &fixture,
        &event(&[("image/png", Some(5))], false),
        &transfer
    ));
    let original = fixture.assert_stub("image/png");
    let id = original["id"].as_str().unwrap();
    assert_eq!(original["pinned"], json!(false));

    for pinned in [true, false] {
        let result = fixture
            .state
            .handle("history.pin", &json!({"clip": id, "pinned": pinned}))
            .expect("pin update succeeds for a stored metadata capture");
        assert_eq!(result, json!({"pinned": pinned}));
        let mut expected = original.clone();
        expected["pinned"] = json!(pinned);
        assert_eq!(fixture.assert_stub("image/png"), expected);
        assert_eq!(
            fixture
                .state
                .handle("history.get", &json!({"clip": id}))
                .unwrap()["clip"],
            expected
        );
        assert_eq!(fixture.scalar("SELECT COUNT(*) FROM clips"), 1);
        assert_eq!(fixture.scalar("SELECT COUNT(*) FROM clip_payloads"), 0);
        assert_eq!(fixture.scalar("SELECT COUNT(*) FROM clips_fts"), 1);
    }
}

#[test]
fn unknown_typed_guard_whole_mixed_capture_has_one_stub_and_stops_reads() {
    let fixture = Fixture::new(config(4));
    let transfer = Transfer::new([
        ("text/plain", Read::Bytes(b"ok".to_vec())),
        ("text/html", Read::TooLarge),
    ]);
    assert!(capture(
        &fixture,
        &event(
            &[
                ("text/plain", None),
                ("text/html", Some(1)),
                ("image/png", None)
            ],
            false
        ),
        &transfer
    ));
    assert_eq!(
        *transfer.reads.borrow(),
        [("text/plain".to_string(), 4), ("text/html".to_string(), 4)]
    );
    let clip = fixture.assert_stub("text/html");
    assert!(clip["preview"].as_str().unwrap().contains("size unknown"));
    assert!(!clip["preview"].as_str().unwrap().contains(">4"));
}

#[test]
fn below_equal_above_cap_traverse_actual_adapter_and_payload_reader() {
    for size in [3, 4, 5] {
        let fixture = Fixture::new(config(4));
        let bytes = vec![b'a'; size];
        let transfer = Transfer::new([("text/plain", Read::Bytes(bytes.clone()))]);
        assert!(capture(
            &fixture,
            &event(&[("text/plain", Some(size as u64))], false),
            &transfer
        ));
        if size > 4 {
            assert!(transfer.reads.borrow().is_empty());
            fixture.assert_stub("text/plain");
        } else {
            assert_eq!(*transfer.reads.borrow(), [("text/plain".to_string(), 4)]);
            let id = fixture.history()[0]["id"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap();
            let (_, payloads) = Store::open(&fixture.path)
                .unwrap()
                .get_clip(id)
                .unwrap()
                .unwrap();
            assert_eq!(payloads, [("text/plain".to_string(), bytes)]);
            assert_eq!(fixture.scalar("SELECT COUNT(*) FROM clip_payloads"), 1);
        }
    }
}

#[test]
fn backend_success_over_cap_is_also_stubbed_at_shared_boundary() {
    let fixture = Fixture::new(config(4));
    let transfer = Transfer::new([("text/plain", Read::Bytes(b"12345".to_vec()))]);
    assert!(capture(
        &fixture,
        &event(&[("text/plain", None)], false),
        &transfer
    ));
    fixture.assert_stub("text/plain");
}

#[test]
fn duplicate_mime_is_read_once_and_produces_one_stub() {
    let fixture = Fixture::new(config(4));
    let transfer = Transfer::new([("image/png", Read::TooLarge)]);
    assert!(capture(
        &fixture,
        &event(&[("image/png", None), ("image/png", None)], false),
        &transfer
    ));
    assert_eq!(transfer.reads.borrow().len(), 1);
    let clip = fixture.assert_stub("image/png");
    assert_eq!(
        clip["preview"]
            .as_str()
            .unwrap()
            .matches("image/png")
            .count(),
        1
    );
}

#[test]
fn distinct_stubs_are_retained_without_sleep_or_proc_dependency() {
    let fixture = Fixture::new(config(4));
    for _ in 0..2 {
        assert!(capture(
            &fixture,
            &event(&[("image/png", Some(5))], false),
            &Transfer::new([])
        ));
    }
    let rows = fixture.history();
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0]["id"], rows[1]["id"]);
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM clip_payloads"), 0);
}

#[test]
fn sensitive_skips_before_reads_metadata_validation_and_stores_only_counter() {
    let fixture = Fixture::new(config(4));
    let transfer = Transfer::new([]);
    let observation = event(&[("private source payload", Some(u64::MAX))], true);
    assert!(!capture(&fixture, &observation, &transfer));
    assert!(transfer.reads.borrow().is_empty());
    fixture.assert_empty(1);
    let result = fixture
        .state
        .handle(
            "capture.report",
            &json!({
                "event": {"sensitive": true, "source_app": "secret", "truncated": [false]},
                "payloads": {"text/plain": "malformed"}
            }),
        )
        .expect("sensitive IPC before decode");
    assert_eq!(result["accepted"], false);
    fixture.assert_empty(2);
}

#[test]
fn strict_nontext_has_no_row_metadata_blob_or_reads_and_counts_skip() {
    let mut cfg = config(4);
    cfg.privacy.strict = true;
    let fixture = Fixture::new(cfg);
    let transfer = Transfer::new([]);
    assert!(!capture(
        &fixture,
        &event(&[("image/png", Some(5))], false),
        &transfer
    ));
    assert!(!fixture.state.ingest_oversize(
        "windows",
        &[OversizeEntry {
            mime: "image/png".to_string(),
            bytes_hint: Some(5)
        }]
    ));
    assert!(transfer.reads.borrow().is_empty());
    fixture.assert_empty(2);
}

#[test]
fn strict_mixed_capture_filters_nontext_before_size_preflight_and_reads() {
    let mut cfg = config(4);
    cfg.privacy.strict = true;
    let fixture = Fixture::new(cfg);
    let transfer = Transfer::new([("text/plain", Read::Bytes(b"ok".to_vec()))]);
    assert!(capture(
        &fixture,
        &event(&[("image/png", Some(5)), ("text/plain", Some(2))], false),
        &transfer
    ));
    assert_eq!(*transfer.reads.borrow(), [("text/plain".to_string(), 4)]);
    assert_eq!(fixture.history()[0]["preview"], "ok");
    assert_eq!(fixture.history()[0]["formats"], json!(["text/plain"]));
}

#[test]
fn paused_and_disabled_format_paths_create_nothing_and_do_not_read() {
    let mut cfg = config(4);
    cfg.monitor.formats = vec!["text/plain".to_string()];
    let fixture = Fixture::new(cfg);
    let transfer = Transfer::new([]);
    assert!(!capture(
        &fixture,
        &event(&[("image/png", Some(5))], false),
        &transfer
    ));
    assert_eq!(
        fixture
            .state
            .handle(
                "capture.report",
                &json!({"event":{"truncated":["image/png"]}})
            )
            .unwrap()["accepted"],
        false
    );
    fixture
        .state
        .handle("monitor.set", &json!({"state":"Off"}))
        .expect("pause");
    assert!(!capture(
        &fixture,
        &event(&[("text/plain", Some(5))], false),
        &transfer
    ));
    assert!(!fixture.state.ingest_oversize(
        "windows",
        &[OversizeEntry {
            mime: "text/plain".to_string(),
            bytes_hint: Some(5)
        }]
    ));
    assert!(transfer.reads.borrow().is_empty());
    fixture.assert_empty(0);
}

#[test]
fn ordinary_io_error_is_logged_without_becoming_stub() {
    let fixture = Fixture::new(config(4));
    let transfer = Transfer::new([("text/plain", Read::Io)]);
    let mut errors = Vec::new();
    assert!(!fixture
        .state
        .capture_event(
            "x11",
            &event(&[("text/plain", None)], false),
            &transfer,
            |line| errors.push(line.to_string())
        )
        .expect("ordinary read error is skipped"));
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("read skipped"));
    fixture.assert_empty(0);
}

#[test]
fn ordinary_capture_and_existing_dedupe_or_merge_remain_unchanged() {
    let fixture = Fixture::new(config(4));
    let transfer = Transfer::new([("text/plain", Read::Bytes(b"ok".to_vec()))]);
    let observation = event(&[("text/plain", None)], false);
    assert!(capture(&fixture, &observation, &transfer));
    let second = capture(&fixture, &observation, &transfer);
    #[cfg(all(unix, feature = "backend-wayland"))]
    assert!(!second, "existing 2-second merger suppresses duplicate");
    #[cfg(not(all(unix, feature = "backend-wayland")))]
    assert!(second, "existing Store moves dedupe hit to top");
    assert_eq!(fixture.history().len(), 1);
    assert_eq!(fixture.history()[0]["preview"], "ok");
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM clip_payloads"), 1);
}

#[test]
fn gnome_mixed_truncation_precedes_inline_decode_and_preserves_unknown_size() {
    let fixture = Fixture::new(config(4));
    let result = fixture
        .state
        .handle(
            "capture.report",
            &json!({
                "event": {"source_app":"editor", "truncated":["image/png"]},
                "payloads": {"text/plain":"invalid base64 is never decoded"}
            }),
        )
        .expect("oversize IPC");
    assert_eq!(result["accepted"], true);
    let clip = fixture.assert_stub("image/png");
    assert!(clip["preview"].as_str().unwrap().contains("size unknown"));
}

#[test]
fn gnome_known_size_config_source_filter_and_strict_nontext() {
    let fixture = Fixture::new(config(4));
    assert_eq!(
        fixture
            .state
            .handle(
                "capture.report",
                &json!({"event":{
                    "offers":[{"mime":"text/plain", "bytes_hint":5}], "truncated":["text/plain"]
                }})
            )
            .unwrap()["accepted"],
        true
    );
    assert!(fixture.assert_stub("text/plain")["preview"]
        .as_str()
        .unwrap()
        .contains("5 B"));
    let mut cfg = config(4);
    cfg.privacy.strict = true;
    cfg.privacy.denylist = vec!["secret-app".to_string()];
    let private = Fixture::new(cfg);
    assert_eq!(
        private
            .state
            .handle(
                "capture.report",
                &json!({"event":{
                    "source_app":"Secret-App.desktop", "truncated":["text/plain"]
                }})
            )
            .unwrap()["accepted"],
        false
    );
    private.assert_empty(0);
    assert_eq!(
        private
            .state
            .handle(
                "capture.report",
                &json!({"event":{
                    "source_app":"editor", "truncated":["image/png"]
                }})
            )
            .unwrap()["accepted"],
        false
    );
    private.assert_empty(1);
}

#[test]
fn malformed_or_unbounded_truncated_metadata_is_rejected_without_partial_row() {
    let fixture = Fixture::new(config(4));
    for bad_event in [
        json!({"truncated":"image/png"}),
        json!({"truncated":[1]}),
        json!({"truncated":[format!("{}/é", "a".repeat(63))]}),
        json!({"truncated":vec!["image/png";33]}),
        json!({"source_app":"a".repeat(257), "truncated":["image/png"]}),
        json!({"offers":[{"mime":"image/png", "bytes_hint":-1}], "truncated":["image/png"]}),
        json!({"offers":[{"mime":"image/png", "bytes_hint":1.5}], "truncated":["image/png"]}),
        json!({"offers":[{"mime":"image/png", "bytes_hint":u64::MAX}], "truncated":["image/png"]}),
        json!({"offers":[{"mime":"image/png", "bytes_hint":"huge"}], "truncated":["image/png"]}),
    ] {
        fixture
            .state
            .handle(
                "capture.report",
                &json!({"event":bad_event, "payloads":{"text/plain":"b2s="}}),
            )
            .expect_err("malformed metadata cannot store smaller payload");
        fixture.assert_empty(0);
    }
    assert_eq!(
        fixture
            .state
            .handle(
                "capture.report",
                &json!({"event":{"truncated":["x-private/custom"]}})
            )
            .unwrap()["accepted"],
        false
    );
    fixture.assert_empty(0);
}

#[test]
fn readonly_stub_and_blob_eof_storage_errors_are_truthfully_published() {
    let fixture = Fixture::new(config(4));
    let readonly = DaemonState::with_config(
        Store::open_readonly_recovery(&fixture.path).unwrap(),
        config(4),
    );
    let transfer = Transfer::new([]);
    let error = readonly
        .capture_event(
            "windows",
            &event(&[("image/png", Some(5))], false),
            &transfer,
            |_| panic!("unexpected read"),
        )
        .expect_err("readonly stub must publish storage failure");
    assert!(error.message.contains("read-only"));
    readonly
        .handle(
            "capture.report",
            &json!({"event":{"truncated":["image/png"]}}),
        )
        .expect_err("IPC storage failure");
    for _ in 0..2 {
        readonly.handle("blob.put", &json!({"clip":"test", "mime":"text/plain", "seq":0, "eof":true, "data_b64":"b2s="}))
            .expect_err("blob EOF cannot acknowledge failed storage or a false merger result");
    }
    fixture.assert_empty(0);
}

#[test]
fn adapter_typed_too_large_replaces_previous_format_with_stub() {
    // The transfer supplies a typed error; this exercises the daemon adapter.
    let fixture = Fixture::new(config(4));
    let transfer = Transfer::new([
        ("text/plain", Read::Bytes(b"ok".to_vec())),
        ("image/png", Read::TooLarge),
    ]);
    assert!(capture(
        &fixture,
        &event(&[("text/plain", None), ("image/png", None)], false),
        &transfer
    ));
    assert_eq!(
        *transfer.reads.borrow(),
        [("text/plain".to_string(), 4), ("image/png".to_string(), 4)]
    );
    let clip = fixture.assert_stub("image/png");
    assert!(clip["preview"].as_str().unwrap().contains("size unknown"));
}

#[test]
fn adapter_io_error_preserves_previous_format_without_stub() {
    let fixture = Fixture::new(config(4));
    let transfer = Transfer::new([
        ("text/plain", Read::Bytes(b"keep".to_vec())),
        ("image/png", Read::Io),
    ]);
    let mut errors = Vec::new();
    assert!(fixture
        .state
        .capture_event(
            "windows",
            &event(&[("text/plain", None), ("image/png", None)], false),
            &transfer,
            |line| errors.push(line.to_string())
        )
        .expect("actual adapter result"));
    assert_eq!(fixture.history().len(), 1);
    assert_eq!(fixture.history()[0]["preview"], "keep");
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM clip_payloads"), 1);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("read skipped"));
    assert!(errors[0].contains("image/png"));
    assert_eq!(
        *transfer.reads.borrow(),
        [("text/plain".to_string(), 4), ("image/png".to_string(), 4)]
    );
}

#[test]
fn generic_blob_over_cap_keeps_existing_error_contract_and_never_creates_stub() {
    let fixture = Fixture::new(config(4));
    assert_eq!(fixture.state.handle("blob.put", &json!({"clip":"test", "mime":"text/plain", "seq":0, "eof":false, "data_b64":"YWJjZA=="})).unwrap()["ok"], true);
    let over = fixture
        .state
        .handle(
            "blob.put",
            &json!({"clip":"test", "mime":"text/plain", "seq":1, "eof":true, "data_b64":"ZQ=="}),
        )
        .expect_err("over-cap assembly error");
    assert!(over.message.contains("exceeds max_bytes"));
    let next = fixture
        .state
        .handle(
            "blob.put",
            &json!({"clip":"test", "mime":"text/plain", "seq":1, "eof":true, "data_b64":"ZQ=="}),
        )
        .expect_err("aborted assembly cannot be reused");
    assert!(next.message.contains("non-contiguous"));
    fixture.assert_empty(0);
}

#[test]
fn gnome_strict_mixed_capture_keeps_only_plain_text() {
    let mut cfg = config(4);
    cfg.privacy.strict = true;
    let fixture = Fixture::new(cfg);
    assert_eq!(fixture.state.handle("capture.report", &json!({
        "event":{"truncated":["image/png"], "offers":[{"mime":"text/plain", "bytes_hint":2}]},
        "payloads":{"text/plain":"b2s="}
    })).unwrap()["accepted"], true);
    assert_eq!(fixture.history()[0]["preview"], "ok");
    assert_eq!(fixture.history()[0]["formats"], json!(["text/plain"]));
}

use super::DaemonState;
use clipmonger_core::Store;
use rusqlite::Connection;
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    sync::{mpsc, Arc, Barrier},
    thread,
    time::Duration,
};
use uuid::Uuid;

struct Fixture {
    path: PathBuf,
    state: Arc<DaemonState>,
    archive: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("daemon-restore-{}", Uuid::now_v7()));
        let state = Arc::new(DaemonState::new(Store::open(&path).unwrap()));
        state.set_profile_dir(&path);
        assert!(state.ingest_capture(
            "test",
            false,
            &[("text/plain".into(), b"original".to_vec())]
        ));
        let archive = path.join("source.zip");
        state
            .handle("backup.create", &json!({"dest": archive}))
            .unwrap();
        Self {
            path,
            state,
            archive,
        }
    }
    fn restore(&self) -> Result<serde_json::Value, clipmonger_ipc::proto::IpcError> {
        self.state
            .handle("backup.restore", &json!({"src": self.archive}))
    }
    fn texts(&self) -> Vec<String> {
        self.state.handle("history.list", &json!({})).unwrap()["clips"]
            .as_array()
            .unwrap()
            .iter()
            .map(|clip| clip["preview"].as_str().unwrap().to_owned())
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn actual_candidate_open_failure_keeps_same_daemon_usable() {
    let fixture = Fixture::new();
    archive_config(&fixture);
    fs::write(fixture.path.join("config.toml"), b"# retained config\n").unwrap();
    *fixture.state.restore_hook.lock().unwrap() = Some(Box::new(|prepared| {
        Connection::open(prepared.incoming().join("clipstore.sqlite3"))
            .unwrap()
            .execute_batch("PRAGMA user_version=99")
            .unwrap();
    }));
    let error = fixture.restore().unwrap_err();
    assert_eq!(error.code, "downgrade_refused");
    assert_eq!(fixture.texts(), ["original"]);
    assert!(fixture.state.ingest_capture(
        "test",
        false,
        &[("text/plain".into(), b"later capture".to_vec())]
    ));
    assert!(fixture.texts().contains(&"later capture".to_owned()));
    assert_eq!(
        fs::read(fixture.path.join("config.toml")).unwrap(),
        b"# retained config\n"
    );
    let reopened = Store::open(&fixture.path).unwrap();
    assert_eq!(reopened.list_history(100, 0, false).unwrap().len(), 2);
}

#[test]
fn actual_old_reopen_failure_reports_unavailable_and_retains_recovery() {
    let fixture = Fixture::new();
    archive_config(&fixture);
    let profile = fixture.path.clone();
    *fixture.state.restore_hook.lock().unwrap() = Some(Box::new(move |prepared| {
        // Real primary rename failure followed by real old-DB open refusal.
        fs::remove_file(prepared.incoming().join("clipstore.sqlite3")).unwrap();
        Connection::open(profile.join("clipstore.sqlite3"))
            .unwrap()
            .execute_batch("PRAGMA user_version=99")
            .unwrap();
    }));
    let error = fixture.restore().unwrap_err();
    assert_eq!(error.code, "unavailable");
    assert!(error.message.contains("restore failed:"));
    assert!(error.message.contains("recovery failed:"));
    assert!(fixture.state.store.read().unwrap().is_none());
    let recovery: Vec<_> = fs::read_dir(&fixture.path)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".restore-"))
        .collect();
    assert_eq!(recovery.len(), 1);
    assert!(recovery[0].path().is_dir());
    let original = Connection::open(fixture.path.join("clipstore.sqlite3")).unwrap();
    let preview: String = original
        .query_row("SELECT preview FROM clips", [], |row| row.get(0))
        .unwrap();
    assert_eq!(preview, "original");
    assert_eq!(
        fs::read(fixture.path.join("config.toml")).unwrap(),
        b"# original config\n"
    );
    for (method, params) in [
        ("history.list", json!({})),
        ("history.get", json!({"clip":"missing"})),
        ("history.delete", json!({"clip":"missing"})),
        ("history.pin", json!({"clip":"missing"})),
        ("capture.report", json!({"payloads":{"text/plain":"bmV3"}})),
        ("capture.report", json!({"event":{"sensitive":true}})),
        (
            "blob.put",
            json!({"clip":"id","mime":"text/plain","seq":0,"data_b64":"bmV3","eof":true}),
        ),
    ] {
        assert_eq!(
            fixture.state.handle(method, &params).unwrap_err().code,
            "unavailable",
            "{method}"
        );
    }
}

#[test]
fn busy_checkpoint_preserves_live_store_and_moves_nothing() {
    let fixture = Fixture::new();
    let reader = Connection::open(fixture.path.join("clipstore.sqlite3")).unwrap();
    reader.execute_batch("BEGIN; SELECT * FROM clips").unwrap();
    assert!(fixture.state.ingest_capture(
        "test",
        false,
        &[("text/plain".into(), b"committed WAL".to_vec())]
    ));
    let error = fixture.restore().unwrap_err();
    assert!(error.message.contains("busy"));
    assert!(fixture.state.store.read().unwrap().is_some());
    assert!(fixture.texts().contains(&"committed WAL".to_owned()));
    assert!(fixture.archive.exists());
    assert!(!fs::read_dir(&fixture.path)
        .unwrap()
        .map(Result::unwrap)
        .any(|entry| entry.file_name().to_string_lossy().starts_with(".restore-")));
    reader.execute_batch("ROLLBACK").unwrap();
    assert!(fixture.state.ingest_capture(
        "test",
        false,
        &[("text/plain".into(), b"after close refusal".to_vec())]
    ));
}

#[test]
fn concurrent_restore_backup_and_config_writers_wait_for_whole_transaction() {
    let fixture = Fixture::new();
    archive_config(&fixture);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *fixture.state.restore_hook.lock().unwrap() = Some(Box::new(move |_| {
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }));
    thread::scope(|scope| {
        let first = scope.spawn(|| fixture.restore());
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(fixture.state.profile_op.try_lock().is_err());
        assert!(fixture.state.store.try_read().is_err());
        let start = Arc::new(Barrier::new(4));
        let (done_tx, done_rx) = mpsc::channel();
        let mut workers = Vec::new();
        for (method, params) in [
            ("backup.restore", json!({"src":fixture.archive})),
            (
                "backup.create",
                json!({"dest":fixture.path.join("concurrent.zip")}),
            ),
            ("config.set", json!({"key":"history.max_items","value":17})),
        ] {
            let start = Arc::clone(&start);
            let done_tx = done_tx.clone();
            let state = &fixture.state;
            workers.push(scope.spawn(move || {
                start.wait();
                let result = state.handle(method, &params);
                done_tx.send(()).unwrap();
                result
            }));
        }
        start.wait();
        assert!(matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert_eq!(
            fs::read_dir(&fixture.path)
                .unwrap()
                .map(Result::unwrap)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with(".restore-"))
                .count(),
            1
        );
        release_tx.send(()).unwrap();
        first.join().unwrap().unwrap();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
    });
    let live = fixture
        .state
        .handle("config.get", &json!({"key":"history.max_items"}))
        .unwrap()["config"]
        .as_u64()
        .unwrap();
    // Either serialized writer can finish last once the first restore releases
    // the gate. Disk and live state must describe that same final writer.
    assert!([1, 17].contains(&live));
    assert_eq!(
        clipmonger_core::Config::load(&fixture.path.join("config.toml"))
            .unwrap()
            .history
            .max_items,
        live
    );
    assert!(fixture.path.join("concurrent.zip").exists());
}

#[test]
fn capture_waits_for_cutover_before_taking_retention_config_snapshot() {
    let fixture = Fixture::new();
    archive_config(&fixture);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *fixture.state.restore_hook.lock().unwrap() = Some(Box::new(move |_| {
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }));
    let capture_started = Barrier::new(2);
    thread::scope(|scope| {
        let restore = scope.spawn(|| fixture.restore());
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let capture = scope.spawn(|| {
            capture_started.wait();
            fixture.state.handle(
                "capture.report",
                &json!({"payloads":{"text/plain":"YWZ0ZXIgY3V0b3Zlcg=="}}),
            )
        });
        capture_started.wait();
        assert!(fixture.state.profile_op.try_lock().is_err());
        release_tx.send(()).unwrap();
        restore.join().unwrap().unwrap();
        assert_eq!(capture.join().unwrap().unwrap()["accepted"], true);
    });
    assert_eq!(fixture.texts(), ["after cutover"]);
    assert_eq!(
        Store::open(&fixture.path)
            .unwrap()
            .list_history(100, 0, false)
            .unwrap()
            .len(),
        1
    );
}

const RAW_CONFIG: &[u8] = b"# restored comment\nconfig_version = 99\nfuture = 'retained'\n[history]\nmax_items = 1\nfuture_limit = 42\n[popup]\nunicode_limit = 23\ndelivery_dwell_ms = 45\ndelivery = 'unicode_inject'\nrestore_clipboard = false\n";
fn archive_config(fixture: &Fixture) {
    fs::write(fixture.path.join("config.toml"), RAW_CONFIG).unwrap();
    fixture
        .state
        .handle("backup.create", &json!({"dest":fixture.archive}))
        .unwrap();
    fs::write(fixture.path.join("config.toml"), b"# original config\n").unwrap();
}
#[test]
fn config_restore_publishes_same_daemon_settings_caps_and_preserves_unknown() {
    use clipmonger_platform::types::{BackendCaps, Level, PasteCaps, PasteStrategy};
    let fixture = Fixture::new();
    archive_config(&fixture);
    fixture.state.register_backend(
        "windows",
        BackendCaps {
            monitor: Level::Full,
            transfer: Level::Full,
            hotkey: Level::Full,
            paste: PasteCaps {
                level: Level::Full,
                strategy: PasteStrategy::UnicodeInject,
                reason: String::new(),
            },
            tray: Level::Full,
            autostart: false,
            primary_selection: false,
            notes: Vec::new(),
        },
    );
    fixture.restore().unwrap();
    assert_eq!(
        fs::read(fixture.path.join("config.toml")).unwrap(),
        RAW_CONFIG
    );
    assert_eq!(
        fixture
            .state
            .handle("config.get", &json!({"key":"history.max_items"}))
            .unwrap()["config"],
        1
    );
    assert_eq!(
        fixture.state.paste_budget(),
        (23, 45, clipmonger_core::PasteDelivery::UnicodeInject, false)
    );
    assert_eq!(
        fixture.state.caps.snapshot()["backends"]["windows"]["paste"]["strategy"],
        "unicode_inject"
    );
    assert!(fixture.state.ingest_capture(
        "test",
        false,
        &[("text/plain".into(), b"new retention".to_vec())]
    ));
    assert_eq!(fixture.texts(), ["new retention"]);
    fixture
        .state
        .handle(
            "config.set",
            &json!({"key":"popup.unicode_limit", "value":31}),
        )
        .unwrap();
    let saved = clipmonger_core::Config::load(&fixture.path.join("config.toml")).unwrap();
    assert_eq!(saved.extra["future"].as_str(), Some("retained"));
    assert_eq!(saved.history.extra["future_limit"].as_integer(), Some(42));
    assert_eq!(saved.popup.unicode_limit, 31);
    assert_eq!(fixture.state.paste_budget().0, 31);
}
#[test]
fn legacy_zip4_keeps_existing_live_config_and_fresh_absence() {
    for existing in [false, true] {
        let fixture = Fixture::new();
        let mut config = clipmonger_core::Config::default();
        config.history.max_items = 67;
        config.popup.unicode_limit = 91;
        config.popup.restore_clipboard = false;
        fixture.state.set_config(config);
        if existing {
            fs::write(fixture.path.join("config.toml"), RAW_CONFIG).unwrap();
        }
        let before = fixture.state.config_json();
        let budget = fixture.state.paste_budget();
        let zip = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../core/tests/fixtures/backup-zip4.zip");
        fixture
            .state
            .handle("backup.restore", &json!({"src": zip}))
            .unwrap();
        assert_eq!(fixture.state.config_json(), before);
        assert_eq!(fixture.state.paste_budget(), budget);
        if existing {
            assert_eq!(
                fs::read(fixture.path.join("config.toml")).unwrap(),
                RAW_CONFIG
            );
        } else {
            assert!(!fixture.path.join("config.toml").exists());
        }
        assert_ne!(fixture.texts(), Vec::<String>::new());
    }
}
#[test]
fn paste_snapshot_is_wholly_old_or_new_during_actual_restore_publication() {
    let fixture = Fixture::new();
    archive_config(&fixture);
    fixture.state.set_paste_limits(71, 83);
    let old = fixture.state.paste_budget();
    let new = (23, 45, clipmonger_core::PasteDelivery::UnicodeInject, false);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *fixture.state.publication_hook.lock().unwrap() = Some(Box::new(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }));
    thread::scope(|scope| {
        let restore = scope.spawn(|| fixture.restore());
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        // The real writer has replaced Config but has not published the paste
        // tuple. The actual helper must return one complete generation.
        assert_eq!(
            fixture.state.config_json()["popup"]["delivery"],
            "unicode_inject"
        );
        let (writer_started_tx, writer_started_rx) = mpsc::channel();
        let (writer_done_tx, writer_done_rx) = mpsc::channel();
        let state = &fixture.state;
        let writer = scope.spawn(move || {
            writer_started_tx.send(()).unwrap();
            let result = state.handle("config.set", &json!({"key":"history.max_items","value":17}));
            writer_done_tx.send(()).unwrap();
            result
        });
        writer_started_rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        assert!(fixture.state.profile_op.try_lock().is_err());
        assert!(matches!(
            writer_done_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        let observed = fixture.state.paste_budget();
        assert!(
            observed == old || observed == new,
            "mixed paste settings: {observed:?}"
        );
        release_tx.send(()).unwrap();
        restore.join().unwrap().unwrap();
        writer.join().unwrap().unwrap();
    });
    assert_eq!(fixture.state.paste_budget(), new);
}
#[test]
fn unsafe_destination_config_is_rejected_without_closing_live_store() {
    let fixture = Fixture::new();
    archive_config(&fixture);
    fs::remove_file(fixture.path.join("config.toml")).unwrap();
    fs::create_dir(fixture.path.join("config.toml")).unwrap();
    *fixture.state.restore_hook.lock().unwrap() = Some(Box::new(|_| {
        panic!("Store was closed before config preflight")
    }));
    assert!(fixture.restore().is_err());
    assert_eq!(fixture.texts(), ["original"]);
    assert!(fixture.state.restore_hook.lock().unwrap().is_some());
    assert!(fixture.state.ingest_capture(
        "test",
        false,
        &[("text/plain".into(), b"still usable".to_vec())]
    ));
}

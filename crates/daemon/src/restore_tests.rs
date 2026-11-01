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
    assert_eq!(
        fixture
            .state
            .handle("config.get", &json!({"key":"history.max_items"}))
            .unwrap()["config"],
        17
    );
    assert!(fixture.path.join("concurrent.zip").exists());
}

#[test]
fn capture_waits_for_cutover_before_taking_retention_config_snapshot() {
    let fixture = Fixture::new();
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
        // Simulates the next checkpoint's config publication while the restore
        // owns the gate; a waiting capture must snapshot this value afterwards.
        fixture.state.config.lock().unwrap().history.max_items = 1;
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

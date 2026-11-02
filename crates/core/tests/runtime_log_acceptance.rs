//! Real filesystem controls for the production runtime sink. No service/profile access.
use clipmonger_core::runtime_log::{self, Role, RuntimeSink, Severity, SinkConfig};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "cm-runtime-sink-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).expect("owned unique directory");
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn config() -> SinkConfig {
    SinkConfig {
        max_active_bytes: 512,
        max_record_bytes: 256,
        max_tail_bytes: 512,
        max_tail_lines: 1000,
    }
}
fn records(path: PathBuf) -> Vec<Value> {
    let text = fs::read_to_string(path).expect("read actual file");
    assert!(text.ends_with('\n'));
    text.lines()
        .map(|line| {
            assert!(line.len() < 256);
            serde_json::from_str(line).expect("complete JSON record")
        })
        .collect()
}
#[test]
fn rotations_keep_exact_newest_and_one_bounded_backup() {
    let dir = Temp::new();
    let sink = RuntimeSink::new_with_config(&dir.0, Role::Daemon, config()).unwrap();
    let mut expected_active: Vec<u64> = Vec::new();
    let mut expected_backup = Vec::new();
    let mut previous_len = 0;
    let mut rotations = 0;
    for n in 0..40_u64 {
        sink.emit(
            Severity::Info,
            "rotation.accepted",
            &[("count", &n.to_string())],
        )
        .unwrap();
        let active = records(sink.active_path());
        let last = active.last().unwrap();
        assert_eq!(last["count"], n.to_string());
        let len = fs::metadata(sink.active_path()).unwrap().len();
        if len < previous_len {
            expected_backup = expected_active.clone();
            expected_active.clear();
            rotations += 1;
        }
        expected_active.push(n);
        let actual: Vec<u64> = active
            .iter()
            .map(|v| v["count"].as_str().unwrap().parse().unwrap())
            .collect();
        assert_eq!(actual, expected_active);
        if !expected_backup.is_empty() {
            let backup: Vec<u64> = records(sink.backup_path())
                .iter()
                .map(|v| v["count"].as_str().unwrap().parse().unwrap())
                .collect();
            assert_eq!(backup, expected_backup);
            assert!(fs::metadata(sink.backup_path()).unwrap().len() <= 512);
        }
        assert!(len <= 512);
        previous_len = len;
    }
    assert!(rotations >= 3, "multiple actual renames");
    let mut names: Vec<_> = fs::read_dir(dir.0.join("logs"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["daemon.log", "daemon.log.1", "daemon.log.lock"]);
    let active_lines: Vec<_> = records(sink.active_path())
        .iter()
        .map(|v| v["count"].as_str().unwrap().to_string())
        .collect();
    let tailed: Vec<_> = sink
        .tail(1000)
        .unwrap()
        .iter()
        .map(|s| {
            serde_json::from_str::<Value>(s).unwrap()["count"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(tailed, active_lines, "tail reads only active, never backup");
}
#[test]
fn encoder_bounds_fields_and_escaping_before_copying_input() {
    let dir = Temp::new();
    let sink = RuntimeSink::new_with_config(&dir.0, Role::Ui, config()).unwrap();
    let huge = "é\n\r\t\0\\\"".repeat(100_000);
    let fields = vec![("class", huge.as_str()); 100_000];
    sink.emit(Severity::Warn, &huge, &fields).unwrap();
    let record = records(sink.active_path()).pop().unwrap();
    assert!(record["ts"].is_number());
    assert_eq!(record["sev"], "warn");
    assert!(record["event"].as_str().unwrap().starts_with('é'));
    assert!(fs::metadata(sink.active_path()).unwrap().len() <= 256);
    let encoded = fs::read_to_string(sink.active_path()).unwrap();
    assert!(encoded.matches("\"class\"").count() <= runtime_log::MAX_FIELDS);
    let mut tiny = config();
    tiny.max_record_bytes = 1;
    assert!(
        RuntimeSink::new_with_config(&dir.0, Role::Daemon, tiny).is_err(),
        "tiny caps explicitly refused"
    );
    for cap in 128..=256 {
        let other = Temp::new();
        let mut limits = config();
        limits.max_record_bytes = cap;
        let bounded = RuntimeSink::new_with_config(&other.0, Role::Ui, limits).unwrap();
        bounded.emit(Severity::Error, &huge, &fields).unwrap();
        let bytes = fs::read(bounded.active_path()).unwrap();
        assert!(bytes.len() <= cap);
        serde_json::from_slice::<Value>(&bytes).unwrap();
    }
}
#[test]
fn concurrent_writers_produce_whole_utf8_records() {
    let dir = Temp::new();
    let limits = SinkConfig {
        max_active_bytes: 1024 * 1024,
        ..SinkConfig::default()
    };
    let sink = Arc::new(RuntimeSink::new_with_config(&dir.0, Role::Daemon, limits).unwrap());
    let threads: Vec<_> = (0..8)
        .map(|worker| {
            let sink = sink.clone();
            std::thread::spawn(move || {
                for n in 0..100 {
                    sink.emit(
                        Severity::Info,
                        "concurrent.accepted",
                        &[
                            ("operation", "é\n\0🦀"),
                            ("count", &(worker * 100 + n).to_string()),
                        ],
                    )
                    .unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let data = fs::read_to_string(sink.active_path()).unwrap();
    let mut ids = std::collections::HashSet::new();
    for line in data.lines() {
        let v: Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["operation"], "é\n\0🦀");
        assert!(ids.insert(v["count"].as_str().unwrap().to_string()));
    }
    assert_eq!(ids.len(), 800);
}
#[test]
fn oversized_existing_file_errors_locks_and_status_use_real_paths() {
    let dir = Temp::new();
    fs::create_dir(dir.0.join("logs")).unwrap();
    fs::write(dir.0.join("logs/daemon.log"), "old-complete\n".repeat(300)).unwrap();
    let sink = RuntimeSink::new_with_config(&dir.0, Role::Daemon, config()).unwrap();
    assert!(fs::metadata(sink.backup_path()).unwrap().len() <= 512);
    assert!(!sink.active_path().exists());
    sink.emit(Severity::Info, "after.oversize", &[]).unwrap();
    assert_eq!(records(sink.active_path())[0]["event"], "after.oversize");
    let error = RuntimeSink::new_with_config(&dir.0, Role::Daemon, config())
        .err()
        .expect("held lock refuses second writer");
    assert_eq!(error, "runtime_log.lock_unavailable");
    drop(sink);
    assert!(
        RuntimeSink::new_with_config(&dir.0, Role::Daemon, config()).is_ok(),
        "drop releases companion lock"
    );
    let bad = Temp::new();
    fs::write(bad.0.join("logs"), "obstruct").unwrap();
    assert!(RuntimeSink::new(&bad.0, Role::Ui).is_err());
    let obstructed = Temp::new();
    let sink = RuntimeSink::new_with_config(&obstructed.0, Role::Ui, config()).unwrap();
    sink.emit(Severity::Info, "before.failure", &[]).unwrap();
    fs::create_dir(sink.backup_path()).unwrap();
    let mut failed = false;
    for _ in 0..20 {
        if sink.emit(Severity::Error, "rotation.failure", &[]).is_err() {
            failed = true;
            break;
        }
    }
    assert!(failed);
    assert!(sink.last_error().is_some());
    assert!(
        !sink.tail(100).unwrap().is_empty(),
        "readable lines survive failed rotation"
    );
    let init_bad = Temp::new();
    fs::create_dir(init_bad.0.join("logs")).unwrap();
    fs::write(init_bad.0.join("logs/ui.log"), vec![b'x'; 1024]).unwrap();
    fs::create_dir(init_bad.0.join("logs/ui.log.1")).unwrap();
    assert!(RuntimeSink::new_with_config(&init_bad.0, Role::Ui, config()).is_err());
    // Check only the owned tree, never inspect a machine's real Temp/profile.
    assert_eq!(
        fs::read_dir(&bad.0).unwrap().count(),
        1,
        "no fallback output in owned profile"
    );
}
#[test]
fn active_tail_handles_counts_bytes_utf8_missing_and_real_refusals() {
    assert!(
        runtime_log::tail(std::path::Path::new(""), Role::Ui, 100, &config()).is_err(),
        "no cwd fallback"
    );
    let dir = Temp::new();
    fs::create_dir(dir.0.join("logs")).unwrap();
    fs::write(dir.0.join("logs/ui.log.1"), "BACKUP_SECRET\n").unwrap();
    assert_eq!(
        runtime_log::tail(&dir.0, Role::Ui, 100, &config()).unwrap(),
        Vec::<String>::new()
    );
    fs::write(dir.0.join("logs/ui.log"), "ééé\nnext\nlast\n").unwrap();
    let mut limits = config();
    limits.max_tail_bytes = 11;
    assert_eq!(
        runtime_log::tail(&dir.0, Role::Ui, 100, &limits).unwrap(),
        ["next", "last"]
    );
    assert_eq!(
        runtime_log::tail(&dir.0, Role::Ui, 0, &limits).unwrap(),
        Vec::<String>::new()
    );
    fs::remove_file(dir.0.join("logs/ui.log")).unwrap();
    fs::create_dir(dir.0.join("logs/ui.log")).unwrap();
    assert!(
        runtime_log::tail(&dir.0, Role::Ui, 0, &limits).is_err(),
        "refusal even at zero count"
    );
}
#[cfg(unix)]
#[test]
fn reader_and_writer_refuse_the_same_symlink_directory_and_files() {
    use std::os::unix::fs::symlink;
    let dir = Temp::new();
    let outside = Temp::new();
    fs::write(outside.0.join("ui.log"), "OUTSIDE_SECRET\n").unwrap();
    symlink(&outside.0, dir.0.join("logs")).unwrap();
    assert!(RuntimeSink::new(&dir.0, Role::Ui).is_err());
    assert!(runtime_log::global_tail(&dir.0, Role::Ui, 100).is_err());
    let file_dir = Temp::new();
    fs::create_dir(file_dir.0.join("logs")).unwrap();
    symlink(outside.0.join("ui.log"), file_dir.0.join("logs/ui.log")).unwrap();
    assert!(RuntimeSink::new(&file_dir.0, Role::Ui).is_err());
    assert!(runtime_log::global_tail(&file_dir.0, Role::Ui, 100).is_err());
}
#[test]
fn process_facade_retains_failed_initialization_and_write_status() {
    let failed = Temp::new();
    fs::write(failed.0.join("logs"), "obstructed").unwrap();
    assert!(runtime_log::init_global(&failed.0, Role::Ui).is_err());
    assert_eq!(
        runtime_log::global_status(Role::Ui).as_deref(),
        Some("runtime_log.refused_logs_directory")
    );
    let owned = Temp::new();
    runtime_log::init_global_with_config(&owned.0, Role::Ui, config()).unwrap();
    runtime_log::global_emit(Role::Ui, Severity::Info, "before.obstruction", &[]).unwrap();
    fs::remove_file(owned.0.join("logs/ui.log")).unwrap();
    fs::create_dir(owned.0.join("logs/ui.log")).unwrap();
    assert!(runtime_log::global_emit(Role::Ui, Severity::Error, "write.failure", &[]).is_err());
    assert_eq!(
        runtime_log::global_status(Role::Ui).as_deref(),
        Some("runtime_log.refused_file")
    );
}

#[test]
fn typed_error_classification_excludes_raw_paths_and_messages() {
    use clipmonger_core::{BackendKind, CoreError};
    let dir = Temp::new();
    let sink = RuntimeSink::new(&dir.0, Role::Ui).unwrap();
    let errors = [
        CoreError::Corrupt {
            message: "PROFILE_PATH_SECRET /private/sentinel".into(),
        },
        CoreError::Io(std::io::Error::other("PAYLOAD_MESSAGE_SECRET")),
        CoreError::Backend {
            backend: "sqlite".into(),
            kind: BackendKind::Internal,
            message: "SQLITE_MESSAGE_SECRET".into(),
        },
    ];
    for error in &errors {
        sink.emit(
            Severity::Error,
            "classified.failure",
            &[
                ("operation", "store_insert"),
                ("class", runtime_log::core_class(error)),
                ("code", &runtime_log::core_code(error)),
            ],
        )
        .unwrap();
    }
    let persisted = fs::read_to_string(sink.active_path()).unwrap();
    assert!(!persisted.contains("SECRET"));
    let values: Vec<Value> = persisted
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(values[0]["class"], "corrupt");
    assert_eq!(values[1]["class"], "io");
    assert_eq!(values[2]["class"], "sqlite");
    let os_error = CoreError::Io(std::io::Error::from_raw_os_error(5));
    assert_eq!(runtime_log::core_code(&os_error), "5");
}

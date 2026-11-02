//! Actual daemon dispatch, real sink and failed Store ingestion, isolated filesystem.
use clipmonger_core::{
    runtime_log::{self, Role, RuntimeSink, Severity, SinkConfig},
    Store,
};
use clipmonger_daemon::DaemonState;
use serde_json::{json, Value};
use std::fmt::Write as _;
use std::fs;

fn lines(state: &DaemonState, params: &Value) -> Vec<String> {
    state.handle("logs.tail", params).expect("actual logs.tail")["lines"]
        .as_array()
        .expect("lines array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}
fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let w = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            out.push(if i < (c.len() * 8).div_ceil(6) {
                A[((w >> (18 - i * 6)) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "exercise one process-wide sink lifetime and all routing contracts"
)]
fn runtime_api_and_failed_ingestion_observe_actual_production_behavior() {
    // A separate integration-test binary owns its one process-global daemon sink.
    let dir = std::env::temp_dir().join(format!(
        "cm-runtime-daemon-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&dir).expect("unique owned profile");
    let state = DaemonState::new(Store::open(&dir).unwrap());
    state.set_profile_dir(&dir);
    fs::write(dir.join("audit.log"), "AUDIT_ONLY\naudit-last\n").unwrap();
    fs::create_dir(dir.join("logs")).unwrap();
    fs::write(dir.join("logs/daemon.log"), "DAEMON_ONLY\n").unwrap();
    fs::write(dir.join("logs/ui.log"), "UI_ONLY\n").unwrap();
    fs::write(dir.join("logs/daemon.log.1"), "BACKUP_EXCLUDED\n").unwrap();
    fs::write(dir.join("payload-secret"), "PAYLOAD_SECRET_EXCLUDED").unwrap();
    assert_eq!(lines(&state, &json!({})), ["AUDIT_ONLY", "audit-last"]);
    assert_eq!(
        lines(&state, &json!({"source":"audit","lines":1})),
        ["audit-last"]
    );
    assert_eq!(lines(&state, &json!({"source":"daemon"})), ["DAEMON_ONLY"]);
    assert_eq!(lines(&state, &json!({"source":"ui"})), ["UI_ONLY"]);
    for source in [
        json!(null),
        json!(12),
        json!([]),
        json!("../audit.log"),
        json!("logs/ui.log"),
        json!("unknown"),
    ] {
        assert_eq!(
            state
                .handle("logs.tail", &json!({"source":source}))
                .unwrap_err()
                .code,
            "invalid_params"
        );
    }
    for source in ["audit", "daemon", "ui"] {
        assert_eq!(
            lines(&state, &json!({"source":source,"lines":0})),
            Vec::<String>::new()
        );
    }
    fs::remove_file(dir.join("logs/ui.log")).unwrap();
    assert_eq!(lines(&state, &json!({"source":"ui"})), Vec::<String>::new());
    fs::create_dir(dir.join("logs/ui.log")).unwrap();
    assert_eq!(
        state
            .handle("logs.tail", &json!({"source":"ui"}))
            .unwrap_err()
            .code,
        "backend"
    );
    fs::remove_dir(dir.join("logs/ui.log")).unwrap();
    let many = (0..1400).fold(String::new(), |mut out, n| {
        writeln!(out, "ui-{n}").unwrap();
        out
    });
    fs::write(dir.join("logs/ui.log"), many).unwrap();
    let capped = lines(&state, &json!({"source":"ui","lines":5000}));
    assert_eq!(capped.len(), 1000);
    assert_eq!(capped.first().unwrap(), "ui-400");
    assert_eq!(capped.last().unwrap(), "ui-1399");
    assert_eq!(lines(&state, &json!({"source":"ui"})).len(), 100);
    let large = (0..1200).fold(String::new(), |mut out, n| {
        writeln!(out, "ui-{n}-{}", "é".repeat(200)).unwrap();
        out
    });
    fs::write(dir.join("logs/ui.log"), large).unwrap();
    let capped = lines(&state, &json!({"source":"ui","lines":5000}));
    assert!(capped.iter().map(|s| s.len() + 1).sum::<usize>() <= runtime_log::MAX_TAIL_BYTES);
    assert!(capped.len() < 1000);
    assert!(capped.last().unwrap().starts_with("ui-1199-"));
    let long_audit = "A".repeat(runtime_log::MAX_TAIL_BYTES + 1024);
    fs::write(dir.join("audit.log"), long_audit.clone() + "\n").unwrap();
    assert_eq!(
        lines(&state, &json!({})),
        vec![long_audit],
        "legacy audit is not subject to runtime byte truncation"
    );
    // Same fixed active-file route must reflect real production rotation.
    let sink = RuntimeSink::new_with_config(
        &dir,
        Role::Ui,
        SinkConfig {
            max_active_bytes: 512,
            max_record_bytes: 256,
            max_tail_bytes: 512,
            max_tail_lines: 1000,
        },
    )
    .unwrap();
    for n in 0..30 {
        sink.emit(Severity::Info, "api.rotation", &[("count", &n.to_string())])
            .unwrap();
    }
    let served = lines(&state, &json!({"source":"ui","lines":1000}));
    assert_eq!(served, sink.tail(1000).unwrap());
    assert_eq!(
        serde_json::from_str::<Value>(served.last().unwrap()).unwrap()["count"],
        "29"
    );
    assert!(!served
        .iter()
        .any(|s| s.contains("BACKUP_EXCLUDED") || s.contains("PAYLOAD_SECRET_EXCLUDED")));
    drop(sink);
    #[cfg(unix)]
    {
        fs::rename(dir.join("logs"), dir.join("owned-logs")).unwrap();
        std::os::unix::fs::symlink(dir.join("owned-logs"), dir.join("logs")).unwrap();
        assert_eq!(
            state
                .handle("logs.tail", &json!({"source":"ui"}))
                .unwrap_err()
                .code,
            "backend"
        );
        assert!(RuntimeSink::new(&dir, Role::Ui).is_err());
        fs::remove_file(dir.join("logs")).unwrap();
        fs::rename(dir.join("owned-logs"), dir.join("logs")).unwrap();
    }
    // Initialization failure survives in daemon process status, alongside old lines.
    fs::create_dir(dir.join("logs/daemon.log.lock")).unwrap();
    assert!(runtime_log::init_global(&dir, Role::Daemon).is_err());
    let failed_init = state
        .handle("logs.tail", &json!({"source":"daemon"}))
        .unwrap();
    assert_eq!(failed_init["lines"], json!(["DAEMON_ONLY"]));
    assert!(failed_init["sink_error"].as_str().is_some());
    fs::remove_dir(dir.join("logs/daemon.log.lock")).unwrap();
    runtime_log::init_global(&dir, Role::Daemon).unwrap();
    // The real capture loop logs once per failure streak and once on recovery.
    let shutdown = std::sync::atomic::AtomicBool::new(false);
    let monitor = clipmonger_platform::fakes::FakeMonitor::with_results(vec![
        Err(clipmonger_platform::types::Error::Io(
            "MONITOR_SECRET_EXCLUDED".into(),
        )),
        Err(clipmonger_platform::types::Error::Io(
            "MONITOR_SECRET_EXCLUDED".into(),
        )),
        Ok(Vec::new()),
    ]);
    let mut waits = Vec::new();
    clipmonger_daemon::capture::run(
        monitor,
        std::time::Duration::ZERO,
        &shutdown,
        || panic!("transient errors retain monitor"),
        |_| shutdown.store(true, std::sync::atomic::Ordering::Relaxed),
        |delay| waits.push(delay),
        |_| {},
    );
    assert_eq!(
        waits,
        [
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(200)
        ]
    );
    let monitor_lines = lines(&state, &json!({"source":"daemon","lines":1000}));
    assert_eq!(
        monitor_lines
            .iter()
            .filter(|s| s.contains("capture.monitor_failed"))
            .count(),
        1
    );
    assert_eq!(
        monitor_lines
            .iter()
            .filter(|s| s.contains("capture.monitor_recovered"))
            .count(),
        1
    );
    assert!(!monitor_lines.join("\n").contains("MONITOR_SECRET_EXCLUDED"));
    // A real managed blob-directory obstruction reaches Store::insert_clip.
    // No injected log message, mock Store, changed acceptance assertion or #76 fix.
    fs::remove_dir(dir.join("blobs")).unwrap();
    fs::write(dir.join("blobs"), "obstructed").unwrap();
    let secret = "PAYLOAD_SECRET_EXCLUDED";
    let payload = secret.repeat(20_000).into_bytes(); // > production 256 KiB inline limit
    assert!(!state.ingest_capture("windows", false, &[("text/plain".into(), payload.clone())]));
    let after = lines(&state, &json!({"source":"daemon","lines":1000}));
    let failures: Vec<Value> = after
        .iter()
        .filter_map(|s| serde_json::from_str::<Value>(s).ok())
        .filter(|v| v["event"] == "capture.store_failed")
        .collect();
    assert_eq!(failures.len(), 1, "one event from the real failed insert");
    assert_eq!(failures[0]["operation"], "store_insert");
    assert!(failures[0]["class"].as_str().is_some());
    assert!(failures[0]["code"].as_str().is_some());
    assert!(!after.join("\n").contains(secret));
    assert!(!after.join("\n").contains(dir.to_str().unwrap()));
    // Preserve checked capture refusal and existing blob.put successful envelope.
    let mut second_payload = payload.clone();
    second_payload.push(b'2');
    let mut third_payload = payload.clone();
    third_payload.push(b'3');
    let report = state
        .handle(
            "capture.report",
            &json!({"event":{"sensitive":false},"payloads":{"text/plain":b64(&second_payload)}}),
        )
        .unwrap();
    assert_eq!(report["accepted"], false);
    let blob = state.handle("blob.put",&json!({"clip":"secret-job-excluded","mime":"text/plain","seq":0,"eof":true,"data_b64":b64(&third_payload)})).unwrap();
    assert_eq!(blob, json!({"ok":true}));
    let after = lines(&state, &json!({"source":"daemon","lines":1000}));
    let failures = after
        .iter()
        .filter(|s| s.contains("capture.store_failed"))
        .count();
    assert_eq!(failures, 3, "each production failed ingestion emits once");
    assert!(!state.ingest_capture("windows", true, &[("text/plain".into(), payload)]));
    assert_eq!(
        lines(&state, &json!({"source":"daemon","lines":1000})),
        after,
        "sensitive suppression adds no runtime record"
    );
    assert!(!after.join("\n").contains("secret-job-excluded"));
    // Write failure status crosses the real API without hiding old readable lines.
    fs::create_dir(dir.join("logs/daemon.log.1.obstruction")).unwrap();
    fs::remove_file(dir.join("logs/daemon.log.1")).unwrap();
    fs::rename(
        dir.join("logs/daemon.log.1.obstruction"),
        dir.join("logs/daemon.log.1"),
    )
    .unwrap();
    let fill = "x".repeat(usize::try_from(runtime_log::MAX_ACTIVE_BYTES).unwrap());
    fs::write(dir.join("logs/daemon.log"), fill + "\n").unwrap();
    assert!(runtime_log::global_emit(Role::Daemon, Severity::Error, "write.failure", &[]).is_err());
    let response = state
        .handle("logs.tail", &json!({"source":"daemon"}))
        .unwrap();
    assert!(response["sink_error"].as_str().is_some());
    // Inspect only owned fixture outputs; this does not check for writes elsewhere.
    assert!(!dir.join("monitor.log").exists());
    drop(state); // Global sink holds its companion lock until this test binary exits.
}

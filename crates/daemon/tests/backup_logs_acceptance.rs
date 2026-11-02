//! Backup/logs daemon acceptance (Step 6 task 2).
//!
//! CONTROLS FIRST: drives the `backup.create`, `backup.restore` and
//! `logs.tail` arms of the public `DaemonState::handle` plus the full
//! 27-row config matrix persistence (set -> save -> load -> get,
//! including the `extra`-map rows and `ui.theme`). Fails on the
//! pre-arm daemon with `unknown_method`.
//!
//! Run: `cargo test -p clipmonger-daemon --test backup_logs_acceptance`

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use clipmonger_core::Store;
use clipmonger_daemon::DaemonState;
use serde_json::{json, Value};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn fresh_state(tag: &str) -> (PathBuf, DaemonState) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("cm-backup-logs-{tag}-{n}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp profile must be creatable");
    let store = Store::open(&dir).expect("open store");
    let state = DaemonState::new(store);
    state.set_profile_dir(&dir);
    (dir, state)
}

fn seed_text(state: &DaemonState, text: &str) {
    let encoded = base64_encode(text.as_bytes());
    let got = state.handle(
        "capture.report",
        &json!({
            "event": {"sensitive": false, "origin": "Clipboard"},
            "payloads": {"text/plain": encoded},
        }),
    );
    assert_eq!(
        got.expect("capture.report").get("accepted"),
        Some(&Value::Bool(true)),
        "seed clip must be accepted"
    );
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let mut word: u32 = 0;
        for (i, &byte) in chunk.iter().enumerate() {
            word |= u32::from(byte) << (16 - 8 * i);
        }
        let chars = (chunk.len() * 8).div_ceil(6);
        for i in 0..4 {
            if i < chars {
                out.push(ALPHABET[((word >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn history_texts(state: &DaemonState) -> Vec<String> {
    state
        .handle("history.list", &json!({}))
        .expect("history.list")
        .get("clips")
        .and_then(Value::as_array)
        .map(|clips| {
            clips
                .iter()
                .map(|clip| {
                    clip.get("search_text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

// Backup roundtrip: create a ZIP, then restore it live and serve the
// restored clips from the same DaemonState without a restart.
#[test]
fn backup_create_restore_roundtrip_serves_restored_clips() {
    let (dir, state) = fresh_state("roundtrip");
    seed_text(&state, "hello backup");
    let dest = dir.join("backup.zip");
    let created = state
        .handle("backup.create", &json!({"dest": dest.to_string_lossy()}))
        .expect("backup.create");
    assert_eq!(
        created.get("path").and_then(Value::as_str),
        Some(dest.to_string_lossy().as_ref()),
        "backup.create must echo the dest path"
    );
    assert!(dest.is_file(), "backup ZIP must exist at dest");

    seed_text(&state, "post backup clip");
    state
        .handle("backup.restore", &json!({"src": dest.to_string_lossy()}))
        .expect("backup.restore");
    let texts = history_texts(&state);
    assert!(
        texts.iter().any(|t| t == "hello backup"),
        "restored clips must be served live, got {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t == "post backup clip"),
        "pre-restore clip must be gone after restore, got {texts:?}"
    );
    seed_text(&state, "capture after restore");
    assert!(history_texts(&state)
        .iter()
        .any(|text| text == "capture after restore"));
    drop(state);
    let reopened = DaemonState::new(Store::open(&dir).unwrap());
    assert!(history_texts(&reopened)
        .iter()
        .any(|text| text == "capture after restore"));
}

// Bad restore source: typed error, daemon keeps serving the old DB.
#[test]
fn backup_restore_bad_src_is_typed_and_keeps_old_clips() {
    let (dir, state) = fresh_state("badsrc");
    seed_text(&state, "old clip survives");
    let bad = dir.join("bad.zip");
    std::fs::write(&bad, b"not a zip at all").expect("write bad zip");
    let err = state
        .handle("backup.restore", &json!({"src": bad.to_string_lossy()}))
        .expect_err("corrupt src must fail");
    assert_eq!(
        err.code, "backend",
        "corrupt zip must be typed, got {}",
        err.code
    );
    let texts = history_texts(&state);
    assert!(
        texts.iter().any(|t| t == "old clip survives"),
        "daemon must stay on the old DB, got {texts:?}"
    );

    let missing = dir.join("missing.zip");
    let err = state
        .handle("backup.restore", &json!({"src": missing.to_string_lossy()}))
        .expect_err("missing src must fail");
    assert_ne!(err.code, "unknown_method", "missing src must be typed");
    assert!(
        history_texts(&state)
            .iter()
            .any(|t| t == "old clip survives"),
        "daemon must stay on the old DB after missing src"
    );
}

// Logs tail: last N audit lines, hard cap at 1000, missing file is
// an empty Ok, never clipboard payload bytes.
#[test]
fn logs_tail_serves_last_n_audit_lines_with_cap() {
    let (dir, state) = fresh_state("logs");
    let lines: Vec<String> = (0..5).map(|i| format!("{{\"line\":{i}}}")).collect();
    std::fs::write(dir.join("audit.log"), lines.join("\n") + "\n").expect("seed audit.log");
    let got = state
        .handle("logs.tail", &json!({"lines": 2}))
        .expect("logs.tail");
    let tail: Vec<String> = got
        .get("lines")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(tail, vec![lines[3].clone(), lines[4].clone()]);

    let many: Vec<String> = (0..1500).map(|i| format!("{{\"line\":{i}}}")).collect();
    std::fs::write(dir.join("audit.log"), many.join("\n") + "\n").expect("seed big audit.log");
    let got = state
        .handle("logs.tail", &json!({"lines": 5000}))
        .expect("logs.tail capped");
    let capped = got
        .get("lines")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(capped.len(), 1000, "logs.tail must cap at 1000 lines");
    assert_eq!(
        capped.first().and_then(Value::as_str),
        Some(many[500].as_str()),
        "cap must keep the most recent lines"
    );
    assert_eq!(
        capped.last().and_then(Value::as_str),
        Some(many[1499].as_str())
    );

    let (empty_dir, empty_state) = fresh_state("logs-missing");
    let got = empty_state
        .handle("logs.tail", &json!({"lines": 100}))
        .expect("missing audit.log must be Ok");
    assert_eq!(
        got.get("lines"),
        Some(&Value::Array(Vec::new())),
        "missing audit.log must be an empty Ok, in {empty_dir:?}"
    );
}

// Config matrix: every one of the 27 ROWS keys plus ui.theme must
// survive set -> save -> load -> get, including the extra-map rows.
#[test]
fn config_all_rows_persist_across_reload() {
    let (dir, state) = fresh_state("config25");
    let cases: &[(&str, Value)] = &[
        ("monitor.formats", json!(["text/plain"])),
        ("monitor.max_bytes", json!(1024)),
        ("monitor.min_len", json!(3)),
        ("monitor.ignores", json!(["nope"])),
        ("monitor.poll_ms", json!(250)),
        ("monitor.capture_primary", json!(true)),
        ("monitor.primary_debounce_ms", json!(100)),
        ("history.max_items", json!(50)),
        ("history.max_store_bytes", json!(1_048_576)),
        ("history.max_age_days", json!(7)),
        ("history.dedupe_window", json!(10)),
        ("popup.hotkey", json!("ctrl+alt+x")),
        ("popup.placement", json!("center")),
        ("popup.preview_width", json!(42)),
        ("popup.dismiss_on_focus_loss", json!(false)),
        ("popup.keys", json!({"copy": "ctrl+c"})),
        (
            "popup.paste_chords",
            json!([{"app": "*", "chord": "ctrl+v"}]),
        ),
        ("popup.unicode_limit", json!(100)),
        ("popup.delivery_dwell_ms", json!(200)),
        ("permanents.groups", json!(["g1"])),
        ("privacy.strict", json!(true)),
        ("privacy.denylist", json!(["classified"])),
        ("tray.enabled", json!(false)),
        ("autostart.enabled", json!(true)),
        ("updates.check", json!(false)),
        ("logging.level", json!("debug")),
        ("popup.delivery", json!("unicode_inject")),
        ("ui.theme", json!({"mode": "dark", "font_scale": 1.25})),
    ];
    assert_eq!(
        cases.len(),
        28,
        "27 ROWS keys (popup.keys.* by wire) + ui.theme"
    );
    for (key, value) in cases {
        state
            .handle("config.set", &json!({"key": key, "value": value}))
            .unwrap_or_else(|e| panic!("config.set {key} must persist: {e}"));
    }
    drop(state);
    let loaded = clipmonger_core::Config::load(&dir.join("config.toml")).expect("reload config");
    let doc = serde_json::to_value(&loaded).expect("config to json");
    for (key, value) in cases {
        let mut cursor = &doc;
        for part in key.split('.') {
            cursor = cursor
                .get(part)
                .unwrap_or_else(|| panic!("reloaded config must carry {key} (in {doc:?})"));
        }
        assert_eq!(*cursor, *value, "reloaded value for {key} must match");
    }
    let state2 = DaemonState::new(Store::open(&dir).expect("reopen store"));
    state2.set_profile_dir(&dir);
    state2.set_config(loaded);
    for (key, value) in cases {
        let got = state2
            .handle("config.get", &json!({"key": key}))
            .expect("config.get");
        assert_eq!(
            got.get("config"),
            Some(value),
            "config.get {key} must serve the reloaded value"
        );
    }
}

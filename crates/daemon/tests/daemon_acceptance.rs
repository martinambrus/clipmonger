//! Daemon black-box acceptance (foundation-v1, task DAEMON+CLI).
//!
//! RULER, not implementation. Pins the frozen binary contract from
//! `.agents/tmp/foundation-v1/daemon-cli/harness-spec.md`:
//! serving/headless flags, single-instance lock (`already_running`),
//! rendezvous JSON `{socket, protocol}` with NO token, headless `--script`
//! replay (one JSON report line per input line), and `status` over IPC
//! carrying caps + daemon-up.
//!
//! The builder MUST make this file compile and pass with no edit here.
//! Assumptions the builder must match are listed in
//! `.agents/tmp/foundation-v1/daemon-cli/harness/README.md` (A1..A10).
//!
//! Run: `cargo test -p clipmonger-daemon --test daemon_acceptance`

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

static CTR: AtomicU64 = AtomicU64::new(0);

fn fresh_profile(tag: &str) -> PathBuf {
    let n = CTR.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "clipmonger-daemon-harness-{}-{tag}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("harness temp profile must be creatable");
    dir
}

fn daemon_bin() -> &'static str {
    env!("CARGO_BIN_EXE_clipmonger-daemon")
}

fn profile_arg(profile: &Path) -> String {
    profile.to_string_lossy().into_owned()
}

/// Prefer `<profile>/rendezvous.json`; else any JSON file in the profile dir
/// carrying a top-level `socket` string (A3).
fn find_rendezvous(profile: &Path) -> Option<(PathBuf, Value)> {
    let mut fallback = None;
    let entries = std::fs::read_dir(profile).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        if v.get("socket").and_then(Value::as_str).is_none() {
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some("rendezvous.json") {
            return Some((path, v));
        }
        if fallback.is_none() {
            fallback = Some((path, v));
        }
    }
    fallback
}

fn read_token_file(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() || trimmed.starts_with('{') || trimmed.lines().count() != 1 {
        return None;
    }
    Some(trimmed)
}

/// Token file: `<profile>/token` preferred, then other `*.token` /
/// `*token*` names, single-line content (A4).
fn find_token(profile: &Path) -> Option<String> {
    for name in ["token", "daemon.token", "auth.token", "ipc.token"] {
        if let Some(token) = read_token_file(&profile.join(name)) {
            return Some(token);
        }
    }
    let entries = std::fs::read_dir(profile).ok()?;
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let is_token_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains("token"));
        if path.is_file() && is_token_name {
            if let Some(token) = read_token_file(&path) {
                return Some(token);
            }
        }
    }
    None
}

fn profile_entries(profile: &Path) -> Vec<String> {
    std::fs::read_dir(profile).map_or_else(
        |_| vec!["<unreadable>".to_string()],
        |entries| {
            let mut names: Vec<String> = entries
                .flatten()
                .map(|e| e.path().display().to_string())
                .collect();
            names.sort();
            names
        },
    )
}

/// Long-running daemon; kills the child on drop (test cleanup).
struct Serving {
    child: Child,
}

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn a serving daemon: `--profile-dir P` first, then
/// `--profile-dir P --headless` (A1). Ready = rendezvous with `socket`
/// appears. Total budget 10 s; a child that exits early moves to the
/// next flag set immediately (no dead waiting).
fn spawn_serving(profile: &Path) -> Result<Serving, String> {
    let bin = daemon_bin();
    let arg = profile_arg(profile);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = String::from("no spawn attempted");
    for extra in [&[] as &[&str], &["--headless"]] {
        let mut child = Command::new(bin)
            .arg("--profile-dir")
            .arg(&arg)
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn daemon: {e}"))?;
        loop {
            if find_rendezvous(profile).is_some() {
                return Ok(Serving { child });
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    last = format!("daemon exited early with {status} (extra args {extra:?})");
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    last = format!("try_wait failed: {e}");
                    break;
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "daemon not ready within 10s for profile {}",
                    profile.display()
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        if Instant::now() >= deadline {
            break;
        }
    }
    Err(format!(
        "daemon never served profile {}: {last}",
        profile.display()
    ))
}

/// Second instance on the same profile must refuse the lock (T-202).
#[test]
fn t202_lock_second_daemon_exits_already_running() {
    let profile = fresh_profile("lock");
    let _first = spawn_serving(&profile).expect("first daemon must serve");
    let mut second = Command::new(daemon_bin())
        .arg("--profile-dir")
        .arg(profile_arg(&profile))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("second daemon must spawn");
    let mut stdout_pipe = second.stdout.take().expect("piped stdout");
    let mut stderr_pipe = second.stderr.take().expect("piped stderr");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = second.try_wait().expect("try_wait must work") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "second daemon must exit promptly while the lock is held"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = second.wait();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let _ = stdout_pipe.read_to_end(&mut stdout);
    let _ = stderr_pipe.read_to_end(&mut stderr);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    assert!(
        !status.success(),
        "second daemon must exit non-zero, got {status}"
    );
    assert!(
        combined.contains("already_running"),
        "second daemon output must contain already_running, got: {combined:?}"
    );
}

/// Rendezvous file: JSON `{socket, protocol}`, never a token (T-203).
#[test]
fn t203_rendezvous_has_socket_and_protocol_without_token() {
    let profile = fresh_profile("rendezvous");
    let _daemon = spawn_serving(&profile).expect("daemon must serve");
    let (path, rendezvous) = find_rendezvous(&profile).unwrap_or_else(|| {
        panic!(
            "rendezvous JSON with socket must exist; saw {:?}",
            profile_entries(&profile)
        )
    });
    let socket = rendezvous
        .get("socket")
        .and_then(Value::as_str)
        .expect("rendezvous must carry a socket path string");
    assert!(!socket.is_empty(), "rendezvous socket must be non-empty");
    assert!(
        rendezvous.get("protocol").is_some(),
        "rendezvous must carry a protocol version, got: {rendezvous}"
    );
    let raw = std::fs::read_to_string(&path).expect("rendezvous must be readable");
    assert!(
        !raw.contains("token"),
        "rendezvous must NEVER contain a token: {}",
        path.display()
    );
    assert!(
        Path::new(socket).exists(),
        "rendezvous socket path must exist, got: {socket:?}"
    );
}

/// `status` over IPC reports caps and daemon-up (T-203, via client).
#[test]
fn t203_caps_status_reports_caps_and_up() {
    let profile = fresh_profile("caps");
    let _daemon = spawn_serving(&profile).expect("daemon must serve");
    let (_, rendezvous) = find_rendezvous(&profile).expect("rendezvous must exist");
    let socket = rendezvous
        .get("socket")
        .and_then(Value::as_str)
        .expect("rendezvous must carry socket")
        .to_string();
    let token = find_token(&profile)
        .unwrap_or_else(|| panic!("token file must exist; saw {:?}", profile_entries(&profile)));
    let client = clipmonger_ipc::client::Client::connect(Path::new(&socket), &token)
        .expect("IPC connect with the profile token must succeed");
    let result = client
        .call("status", Value::Object(serde_json::Map::default()))
        .expect("status call must succeed");
    assert!(
        result.get("caps").is_some(),
        "status result must contain caps, got: {result}"
    );
    assert!(
        mentions_up(&result),
        "status result must show the daemon is up, got: {result}"
    );
}

fn mentions_up(value: &Value) -> bool {
    match value {
        Value::String(text) => {
            let lower = text.to_lowercase();
            lower.contains("up")
                || lower.contains("running")
                || lower.contains("ready")
                || lower.contains("online")
        }
        Value::Array(items) => items.iter().any(mentions_up),
        Value::Object(map) => {
            map.keys().any(|k| {
                let lower = k.to_lowercase();
                lower == "up"
                    || lower == "running"
                    || lower == "ready"
                    || lower == "online"
                    || lower == "daemon"
            }) || map.values().any(mentions_up)
        }
        _ => false,
    }
}

/// Headless `--script` replay: 2 capture lines -> 2 JSON report lines,
/// store file created and readable via core `Store` (T-209).
#[test]
fn t209_headless_script_replays_two_lines_to_reports() {
    let profile = fresh_profile("headless");
    let script = profile.join("script.jsonl");
    // A5: script lines are CaptureEvent-shaped JSON.
    let line1 = r#"{"offers":[],"origin":"Clipboard","sensitive":false}"#;
    let line2 = r#"{"offers":[],"origin":"Clipboard","sensitive":true}"#;
    std::fs::write(&script, format!("{line1}\n{line2}\n")).expect("script must be writable");
    let output = Command::new(daemon_bin())
        .arg("--profile-dir")
        .arg(profile_arg(&profile))
        .arg("--headless")
        .arg("--script")
        .arg(&script)
        .output()
        .expect("headless daemon must run");
    assert!(
        output.status.success(),
        "headless script run must exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let reports: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        reports.len(),
        2,
        "one JSON report line per input line, got: {stdout:?}"
    );
    for report in &reports {
        let _: Value = serde_json::from_str(report).expect("report line must be JSON");
    }
    // Store::open CREATES the file, so existence must be checked first:
    // the daemon run itself must have created it.
    let db = profile.join("clipstore.sqlite3");
    assert!(
        db.is_file(),
        "headless run must create the store file, saw {:?}",
        profile_entries(&profile)
    );
    let store =
        clipmonger_core::Store::open(&profile).expect("stored profile must open via core Store");
    store
        .list_history(100, 0, false)
        .expect("history must be readable after replay");
}

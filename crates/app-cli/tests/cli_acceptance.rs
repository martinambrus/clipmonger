//! CLI black-box acceptance (foundation-v1, task DAEMON+CLI).
//!
//! RULER, not implementation. Pins the frozen binary contract from
//! `.agents/tmp/foundation-v1/daemon-cli/harness-spec.md`:
//! `clipmonger-cli --profile-dir <path> <cmd>`; daemon down -> exit 1 +
//! stderr `daemon not running`; daemon up -> JSON on stdout, exit 0.
//! Covers the daemon-down refusal plus status/list roundtrips and stop
//! against a spawned daemon.
//!
//! The builder MUST make this file compile and pass with no edit here.
//! Assumptions are listed in
//! `.agents/tmp/foundation-v1/daemon-cli/harness/README.md` (A1..A10).
//!
//! Run: `cargo test -p clipmonger-app-cli --test cli_acceptance`
//!
//! The daemon binary is located as a target-dir sibling of this test
//! executable (override with `CLIPMONGER_DAEMON_BIN`).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

static CTR: AtomicU64 = AtomicU64::new(0);

fn fresh_profile(tag: &str) -> PathBuf {
    let n = CTR.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "clipmonger-cli-harness-{}-{tag}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("harness temp profile must be creatable");
    dir
}

fn cli_bin() -> &'static str {
    env!("CARGO_BIN_EXE_clipmonger-cli")
}

/// Target-dir sibling of this test executable:
/// `<target>/<profile>/deps/<test>-<hash>` -> `<target>/<profile>/clipmonger-daemon`.
fn daemon_bin() -> PathBuf {
    if let Ok(path) = std::env::var("CLIPMONGER_DAEMON_BIN") {
        return PathBuf::from(path);
    }
    let exe = std::env::current_exe().expect("current exe path");
    let sibling = exe
        .parent()
        .and_then(|deps| deps.parent())
        .map(|dir| dir.join(format!("clipmonger-daemon{}", std::env::consts::EXE_SUFFIX)));
    sibling.expect("daemon binary must sit beside the test executable")
}

fn run_cli(profile: &Path, args: &[&str]) -> Output {
    Command::new(cli_bin())
        .arg("--profile-dir")
        .arg(profile)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("CLI must run")
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

/// Wait until `cli --profile-dir P status` exits 0 with a JSON result on
/// stdout (bounded 10 s). Exit 0 alone is NOT enough (a stub exits 0 with
/// no output). Breaks early if the daemon child exits (fast FAIL on stubs).
fn wait_cli_up(daemon: &mut Child, profile: &Path) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let out = run_cli(profile, &["status"]);
        if out.status.success()
            && serde_json::from_str::<Value>(String::from_utf8_lossy(&out.stdout).trim()).is_ok()
        {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        if let Ok(Some(status)) = daemon.try_wait() {
            return Err(format!(
                "daemon exited ({status}) before CLI status succeeded; last stderr: {stderr:?}"
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "CLI status never succeeded within 10s; last stderr: {stderr:?}"
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Spawn a serving daemon the CLI can talk to: `--profile-dir P` first,
/// then `--profile-dir P --headless` (A1). Total budget 10 s.
fn spawn_serving(profile: &Path) -> Result<Serving, String> {
    let bin = daemon_bin();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = String::from("no spawn attempted");
    for extra in [&[] as &[&str], &["--headless"]] {
        let mut child = Command::new(&bin)
            .arg("--profile-dir")
            .arg(profile)
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn daemon: {e}"))?;
        match wait_cli_up(&mut child, profile) {
            Ok(()) => return Ok(Serving { child }),
            Err(e) => {
                last = format!("{e} (extra args {extra:?})");
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    Err(format!(
        "daemon never served profile {}: {last}",
        profile.display()
    ))
}

fn stdout_json(output: &Output, cmd: &str) -> Value {
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "`{cmd}` must exit 0, got {} stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(text.trim())
        .unwrap_or_else(|_| panic!("`{cmd}` stdout must be JSON, got: {text:?}"))
}

/// Daemon down: `status` and `list` exit 1 with `daemon not running` (D-F21).
#[test]
fn cli_daemon_down_is_exit_1_with_message() {
    let profile = fresh_profile("down");
    for cmd in ["status", "list"] {
        let output = run_cli(&profile, &[cmd]);
        assert_eq!(
            output.status.code(),
            Some(1),
            "`{cmd}` with no daemon must exit 1, got {}",
            output.status
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("daemon not running"),
            "`{cmd}` stderr must contain `daemon not running`, got: {stderr:?}"
        );
    }
}

/// Daemon up: `status` prints a JSON result, exit 0.
#[test]
fn cli_status_roundtrip_against_daemon() {
    let profile = fresh_profile("status");
    let _daemon = spawn_serving(&profile).expect("daemon must serve");
    let output = run_cli(&profile, &["status"]);
    let value = stdout_json(&output, "status");
    assert!(
        value.is_object() || value.is_array(),
        "`status` must print a JSON result, got: {value}"
    );
}

/// Daemon up: `list` prints a JSON result, exit 0.
#[test]
fn cli_list_roundtrip_against_daemon() {
    let profile = fresh_profile("list");
    let _daemon = spawn_serving(&profile).expect("daemon must serve");
    let output = run_cli(&profile, &["list"]);
    let value = stdout_json(&output, "list");
    assert!(
        value.is_object() || value.is_array(),
        "`list` must print a JSON result, got: {value}"
    );
}

/// Daemon up: `stop` exits 0 and the daemon process actually exits.
#[test]
fn cli_stop_stops_the_daemon() {
    let profile = fresh_profile("stop");
    let mut daemon = spawn_serving(&profile).expect("daemon must serve");
    let output = run_cli(&profile, &["stop"]);
    assert!(
        output.status.success(),
        "`stop` must exit 0, got {} stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    if !text.trim().is_empty() {
        serde_json::from_str::<Value>(text.trim())
            .unwrap_or_else(|_| panic!("non-empty `stop` stdout must be JSON, got: {text:?}"));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if daemon
            .child
            .try_wait()
            .expect("try_wait must work")
            .is_some()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "daemon must exit within 10s of `stop`"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

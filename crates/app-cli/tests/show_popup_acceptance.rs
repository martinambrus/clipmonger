//! `clipmonger-cli show-popup` asks the daemon to open the popup
//! (`popup.open`); with no UI attached the daemon answers `delivered:false`
//! and would start the UI (turned off here with `CLIPMONGER_NO_UI_SPAWN`).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

fn sibling(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("test exe path");
    exe.parent()
        .and_then(Path::parent)
        .map(|dir| dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)))
        .expect("binaries sit beside the test executable")
}

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn cli(profile: &Path, args: &[&str]) -> std::process::Output {
    Command::new(sibling("clipmonger-cli"))
        .arg("--profile-dir")
        .arg(profile)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("CLI must run")
}

#[test]
fn show_popup_opens_via_the_daemon() {
    let profile = std::env::temp_dir().join(format!("cm-cli-popup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    std::fs::create_dir_all(&profile).expect("profile dir");
    let daemon = Daemon(
        Command::new(sibling("clipmonger-daemon"))
            .arg("--profile-dir")
            .arg(&profile)
            .env("CLIPMONGER_NO_UI_SPAWN", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn daemon"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cli(&profile, &["status"]).status.success() {
        assert!(Instant::now() < deadline, "daemon never served");
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = cli(&profile, &["show-popup"]);
    assert!(
        out.status.success(),
        "show-popup must exit 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let answer: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("JSON answer");
    assert_eq!(answer, serde_json::json!({"delivered": false}));
    drop(daemon);
}

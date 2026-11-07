//! `clipmonger-cli`: thin JSON-over-IPC client for the daemon.
//!
//! Daemon down means exit 1 with `daemon not running` on stderr; daemon up
//! prints the JSON result on stdout with exit 0.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::Value;

#[derive(Parser)]
#[command(name = "clipmonger-cli")]
struct Cli {
    /// Profile directory shared with the daemon.
    #[arg(long)]
    profile_dir: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Daemon status including capabilities.
    Status,
    /// Recent history as JSON.
    List,
    /// One clip by id.
    Get {
        /// Clip id.
        id: String,
    },
    /// Paste one clip by id.
    Paste {
        /// Clip id.
        id: String,
    },
    /// Ask the daemon to show its popup.
    #[command(name = "show-popup")]
    ShowPopup,
    /// Toggle clipboard monitoring.
    #[command(name = "toggle-monitor")]
    ToggleMonitor,
    /// Execute a permanent clip by id.
    #[command(name = "exec-permanent")]
    ExecPermanent {
        /// Permanent clip id.
        id: String,
    },
    /// List permanent clips as JSON.
    #[command(name = "permanent-list")]
    PermanentList,
    /// Create a permanent clip.
    #[command(name = "permanent-create")]
    PermanentCreate {
        /// Group name.
        group: String,
        /// Clip name.
        name: String,
        /// Text to paste.
        body: String,
        /// Optional per-clip hotkey chord.
        #[arg(long)]
        hotkey: Option<String>,
    },
    /// Delete a permanent clip by id (its hotkey is released).
    #[command(name = "permanent-delete")]
    PermanentDelete {
        /// Permanent clip id.
        id: String,
    },
    /// Purge soft-deleted clips.
    #[command(name = "purge-removed")]
    PurgeRemoved,
    /// Stop the serving daemon.
    Stop,
    /// macOS: stop Clipmonger, remove its start-on-login agent and, with
    /// --delete-history, the profile; then drag Clipmonger.app to the Trash.
    Uninstall {
        /// Also delete the profile (history, settings, permanent clips).
        #[arg(long)]
        delete_history: bool,
    },
}

/// Exit-1 refusal used whenever the daemon cannot be reached.
fn daemon_down() -> ! {
    eprintln!("daemon not running");
    std::process::exit(1);
}

/// Prefer `<profile>/rendezvous.json`, else any `*.json` with a `socket` key.
fn find_rendezvous(profile: &Path) -> Option<Value> {
    let preferred = profile.join("rendezvous.json");
    if let Ok(text) = std::fs::read_to_string(&preferred) {
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            if value.get("socket").and_then(Value::as_str).is_some() {
                return Some(value);
            }
        }
    }
    let entries = std::fs::read_dir(profile).ok()?;
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        if value.get("socket").and_then(Value::as_str).is_some() {
            return Some(value);
        }
    }
    None
}

/// Read one trimmed non-JSON line from a token file candidate.
fn read_token_file(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() || trimmed.starts_with('{') || trimmed.lines().count() != 1 {
        return None;
    }
    Some(trimmed)
}

/// Prefer `<profile>/token`, else another `*token*` file.
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
        let is_token = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains("token"));
        if path.is_file() && is_token {
            if let Some(token) = read_token_file(&path) {
                return Some(token);
            }
        }
    }
    None
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let profile = cli.profile_dir;
    if let Cmd::Uninstall { delete_history } = cli.cmd {
        return uninstall(&profile, delete_history);
    }

    let rendezvous = find_rendezvous(&profile).unwrap_or_else(|| daemon_down());
    let socket = rendezvous
        .get("socket")
        .and_then(Value::as_str)
        .unwrap_or_else(|| daemon_down())
        .to_string();
    let token = find_token(&profile).unwrap_or_else(|| daemon_down());
    let client = clipmonger_ipc::client::Client::connect(Path::new(&socket), &token)
        .unwrap_or_else(|_| daemon_down());

    let (method, params, is_stop) = match &cli.cmd {
        Cmd::Status => ("status", serde_json::json!({}), false),
        Cmd::List => ("history.list", serde_json::json!({}), false),
        Cmd::Get { id } => ("history.get", serde_json::json!({"clip": id}), false),
        Cmd::Paste { id } => ("paste.execute", serde_json::json!({"clip": id}), false),
        Cmd::ShowPopup => (
            "popup.open",
            serde_json::json!({"capture_target": true}),
            false,
        ),
        Cmd::ToggleMonitor => ("monitor.set", serde_json::json!({"toggle": true}), false),
        Cmd::ExecPermanent { id } => ("permanent.exec", serde_json::json!({"id": id}), false),
        Cmd::PermanentList => ("permanent.list", serde_json::json!({}), false),
        Cmd::PermanentCreate {
            group,
            name,
            body,
            hotkey,
        } => (
            "permanent.create",
            match hotkey {
                Some(hotkey) => {
                    serde_json::json!({"group": group, "name": name, "body": body, "hotkey": hotkey})
                }
                None => serde_json::json!({"group": group, "name": name, "body": body}),
            },
            false,
        ),
        Cmd::PermanentDelete { id } => ("permanent.delete", serde_json::json!({"id": id}), false),
        Cmd::PurgeRemoved => ("history.purge_removed", serde_json::json!({}), false),
        Cmd::Stop => ("daemon.stop", serde_json::json!({}), true),
        Cmd::Uninstall { .. } => unreachable!("handled before connecting"),
    };

    match client.call(method, params) {
        Ok(value) => {
            println!(
                "{}",
                serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string())
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            if is_stop {
                // The daemon may exit before the response flushes; a
                // reachable daemon that accepted the connection counts as
                // stopped.
                ExitCode::SUCCESS
            } else if matches!(
                e.code.as_str(),
                "connection_refused" | "connection_closed" | "io_failed"
            ) {
                daemon_down();
            } else {
                eprintln!("{e}");
                ExitCode::from(1)
            }
        }
    }
}

/// Files only a Clipmonger profile has; `--delete-history` refuses any
/// other folder.
#[cfg(target_os = "macos")]
fn is_profile(dir: &Path) -> bool {
    dir.join("clipstore.sqlite3").is_file()
        && (dir.join("config.toml").exists() || dir.join("blobs").is_dir())
}

/// Stop the daemon if it answers; a stopped daemon is fine.
#[cfg(target_os = "macos")]
fn stop_daemon(profile: &Path) {
    let Some(socket) = find_rendezvous(profile)
        .and_then(|r| r.get("socket").and_then(Value::as_str).map(str::to_string))
    else {
        return;
    };
    let Some(token) = find_token(profile) else {
        return;
    };
    if let Ok(client) = clipmonger_ipc::client::Client::connect(Path::new(&socket), &token) {
        let _ = client.call("daemon.stop", serde_json::json!({}));
    }
}

#[cfg(target_os = "macos")]
fn uninstall(profile: &Path, delete_history: bool) -> ExitCode {
    const LABEL: &str = "com.martinambrus.clipmonger";
    stop_daemon(profile);
    // The UI and any daemon that did not answer.
    for name in ["clipmonger", "clipmonger-daemon"] {
        let _ = std::process::Command::new("/usr/bin/pkill")
            .args(["-x", name])
            .status();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let agent = home.join(format!("Library/LaunchAgents/{LABEL}.plist"));
    if agent.exists() {
        let uid = std::process::Command::new("/usr/bin/id")
            .arg("-u")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let _ = std::process::Command::new("/bin/launchctl")
            .args(["bootout", &format!("gui/{uid}/{LABEL}")])
            .stderr(std::process::Stdio::null())
            .status();
        if let Err(e) = std::fs::remove_file(&agent) {
            eprintln!("could not remove {}: {e}", agent.display());
            return ExitCode::from(1);
        }
        println!("Removed the start-on-login agent.");
    }
    if delete_history {
        if !is_profile(profile) {
            eprintln!(
                "{} is not a Clipmonger profile; nothing deleted",
                profile.display()
            );
            return ExitCode::from(1);
        }
        if let Err(e) = std::fs::remove_dir_all(profile) {
            eprintln!("could not delete {}: {e}", profile.display());
            return ExitCode::from(1);
        }
        println!("Deleted the profile {}.", profile.display());
    } else {
        println!("Kept your history in {}.", profile.display());
    }
    println!(
        "Clipmonger is stopped. Drag Clipmonger.app from Applications to the Trash to finish."
    );
    ExitCode::SUCCESS
}

#[cfg(not(target_os = "macos"))]
fn uninstall(profile: &Path, delete_history: bool) -> ExitCode {
    let _ = (profile, delete_history);
    eprintln!(
        "uninstall is for macOS; use the Windows uninstaller, your package manager, \
         or install.sh --uninstall on Linux"
    );
    ExitCode::from(2)
}

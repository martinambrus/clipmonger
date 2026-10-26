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
    /// Purge soft-deleted clips.
    #[command(name = "purge-removed")]
    PurgeRemoved,
    /// Stop the serving daemon.
    Stop,
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
        Cmd::ShowPopup => ("ui.show_popup", serde_json::json!({}), false),
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
        Cmd::PurgeRemoved => ("history.purge_removed", serde_json::json!({}), false),
        Cmd::Stop => ("daemon.stop", serde_json::json!({}), true),
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

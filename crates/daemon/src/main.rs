//! `clipmonger-daemon`: serving mode binds the profile socket over IPC;
//! headless `--script` mode replays capture lines and exits.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clipmonger_daemon::{replay_script, write_rendezvous, DaemonLock, DaemonState};
use clipmonger_ipc::framing::socket_path_for;

fn usage() -> ! {
    eprintln!("usage: clipmonger-daemon --profile-dir <path> [--headless] [--script <file>]");
    std::process::exit(2);
}

fn profile_arg(args: &[String]) -> PathBuf {
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--profile-dir" && index + 1 < args.len() {
            return PathBuf::from(&args[index + 1]);
        }
        index += 1;
    }
    usage();
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn flag_value(args: &[String], name: &str) -> Option<PathBuf> {
    let mut index = 0;
    while index < args.len() {
        if args[index] == name && index + 1 < args.len() {
            return Some(PathBuf::from(&args[index + 1]));
        }
        index += 1;
    }
    None
}

/// Generate a single-line token for `<profile>/token`.
fn generate_token() -> String {
    if let Ok(raw) = fs::read_to_string("/proc/sys/kernel/random/uuid") {
        let compact: String = raw.trim().chars().filter(|c| *c != '-').collect();
        if !compact.is_empty() {
            return compact;
        }
    }
    format!(
        "skeleton-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    )
}

/// Write `<profile>/token` with owner-only permissions; returns the token.
fn write_token(profile: &Path) -> Result<String, String> {
    let token = generate_token();
    let path = profile.join("token");
    fs::write(&path, format!("{token}\n")).map_err(|e| format!("token write failed: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut p = fs::metadata(&path)
            .map_err(|e| format!("token stat failed: {e}"))?
            .permissions();
        p.set_mode(0o600);
        fs::set_permissions(&path, p).map_err(|e| format!("token chmod failed: {e}"))?;
    }
    Ok(token)
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from)
}

fn run_batch(profile: &Path, script: &Path) -> i32 {
    let store = match clipmonger_core::Store::open(profile) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("headless: store open failed: {e}");
            return 1;
        }
    };
    let text = match fs::read_to_string(script) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("headless: script read failed: {e}");
            return 1;
        }
    };
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    for report in replay_script(&store, lines.into_iter()) {
        println!("{report}");
    }
    0
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let profile = profile_arg(&args);

    if let Some(script) = flag_value(&args, "--script") {
        std::process::exit(run_batch(&profile, &script));
    }
    let _headless = flag(&args, "--headless");

    let _lock = match DaemonLock::acquire(&profile) {
        Ok(lock) => lock,
        Err(e) => {
            eprintln!("daemon refused: {e}");
            std::process::exit(1);
        }
    };

    let store = match clipmonger_core::Store::open(&profile) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("daemon: store open failed: {e}");
            std::process::exit(1);
        }
    };

    let token = match write_token(&profile) {
        Ok(token) => token,
        Err(e) => {
            eprintln!("daemon: {e}");
            std::process::exit(1);
        }
    };

    let socket = socket_path_for(&profile, &runtime_dir());
    if let Some(parent) = socket.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("daemon: socket dir failed: {e}");
            std::process::exit(1);
        }
    }

    let state = Arc::new(DaemonState::new(store));
    let stop: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());
    let stop_handler = Arc::clone(&stop);
    let handler: clipmonger_ipc::server::Handler = Arc::new(move |method, params| {
        if method == "daemon.stop" {
            stop_handler.notify_one();
            return Ok(serde_json::json!({"ok": true}));
        }
        state.handle(&method, &params)
    });

    if let Err(e) = clipmonger_ipc::server::Server::bind(&socket, token, handler).await {
        eprintln!("daemon: bind failed: {e}");
        std::process::exit(1);
    }
    if let Err(e) = write_rendezvous(&profile, &socket) {
        eprintln!("daemon: rendezvous failed: {e}");
        std::process::exit(1);
    }
    eprintln!(
        "daemon serving profile {} socket {}",
        profile.display(),
        socket.display()
    );

    tokio::select! {
        () = stop.notified() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    let _ = fs::remove_file(profile.join("rendezvous.json"));
    let _ = fs::remove_file(&socket);
}

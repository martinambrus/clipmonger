//! `clipmonger-daemon`: serving mode binds the profile socket over IPC;
//! headless `--script` mode replays capture lines and exits.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clipmonger_core::Config;
use clipmonger_daemon::{replay_script, write_rendezvous, DaemonLock, DaemonState};
#[cfg(windows)]
use clipmonger_ipc::framing::pipe_name_for;
#[cfg(unix)]
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

/// Whether a config format entry selects `mime`.
///
/// Config uses canonical names (`text/plain;charset=utf-8` and `text/plain`
/// both); transfer offers use the short form.
#[cfg(all(windows, feature = "backend-windows"))]
fn wanted_format(mime: &str, formats: &[String]) -> bool {
    formats.iter().any(|format| {
        format == mime
            || (mime == "text/plain" && format == "text/plain;charset=utf-8")
            || (format == "text/plain" && mime == "text/plain;charset=utf-8")
    })
}

/// Construct the live Windows backend: typed caps, monitor thread (real
/// payloads via transfer honoring opt-out + privacy counter), and the
/// popup-hotkey listener (daemon-event only: no UI push channel in v1).
#[cfg(all(windows, feature = "backend-windows"))]
#[allow(clippy::too_many_lines)]
fn setup_windows_backend(
    profile: &Path,
    state: &Arc<DaemonState>,
    config: &Config,
    shutdown: &Arc<std::sync::atomic::AtomicBool>,
) {
    use clipmonger_backend_windows::{
        caps as backend_caps, WinAutostart, WinHotkeys, WinMonitor, WinTransfer,
    };
    use clipmonger_platform::traits::{
        AutostartManager, ClipboardMonitor, ClipboardTransfer, HotkeyManager,
    };
    use clipmonger_platform::types::{HotkeyId, Level, SubscribeKind};

    let transfer = Arc::new(WinTransfer::new());

    let mut monitor_ok = true;
    let monitor = match WinMonitor::new() {
        Ok(mut monitor) => {
            if monitor.subscribe(&[SubscribeKind::Clipboard]).is_err() {
                monitor_ok = false;
            }
            Some(monitor)
        }
        Err(e) => {
            eprintln!("daemon: windows monitor unavailable: {e}");
            monitor_ok = false;
            None
        }
    };

    let mut hotkeys = WinHotkeys::new();
    let mut hotkey_conflict = false;
    if let Err(e) = hotkeys.register(HotkeyId(1), &config.popup.hotkey) {
        eprintln!("daemon: popup hotkey unavailable: {e}");
        hotkey_conflict = true;
    }

    // Autostart handle for future UI wiring; never enabled at startup.
    // Portable when the profile lives beside the daemon binary.
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("clipmonger-daemon"));
    let exe_dir = exe
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let autostart = WinAutostart::new(exe, profile.starts_with(&exe_dir));
    let _ = autostart.is_enabled();

    let mut caps = backend_caps::caps_for(hotkey_conflict, false);
    if !monitor_ok {
        caps.monitor = Level::Degraded;
        caps.notes
            .push("monitor Degraded: session clipboard unavailable".to_string());
    }
    state.register_backend("windows", caps);

    if let Some(mut monitor) = monitor {
        let state = Arc::clone(state);
        let transfer = Arc::clone(&transfer);
        let formats: Vec<String> = config.monitor.formats.clone();
        let max_bytes = config.monitor.max_bytes;
        let poll_ms = config.monitor.poll_ms;
        let shutdown = Arc::clone(shutdown);
        std::thread::spawn(move || {
            let poll_timeout = std::time::Duration::from_millis(poll_ms.clamp(50, 5000));
            while !shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                let events = match monitor.poll(poll_timeout) {
                    Ok(events) => events,
                    Err(e) => {
                        eprintln!("daemon: monitor poll failed: {e}");
                        break;
                    }
                };
                for event in &events {
                    if event.sensitive {
                        state.ingest_capture("windows", true, &[]);
                        continue;
                    }
                    let mut payloads = Vec::new();
                    for offer in &event.offers {
                        if !wanted_format(&offer.mime, &formats) {
                            continue;
                        }
                        match transfer.read(&offer.mime, max_bytes) {
                            Ok(bytes) => payloads.push((offer.mime.clone(), bytes)),
                            Err(e) => {
                                eprintln!("daemon: payload read skipped ({}): {e}", offer.mime);
                            }
                        }
                    }
                    if !payloads.is_empty() {
                        state.ingest_capture("windows", false, &payloads);
                    }
                }
            }
        });
    }

    let shutdown = Arc::clone(shutdown);
    let state = Arc::clone(state);
    std::thread::spawn(move || {
        while !shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            match hotkeys.poll(std::time::Duration::from_millis(200)) {
                Ok(ids) => {
                    for id in ids {
                        // Gesture mint: hotkey/popup trigger path ONLY. The
                        // token is never logged and never minted from the
                        // generic paste.execute arm; the popup flow presents
                        // it back within 30 s for elevated targets.
                        state.mint_gesture_token();
                        eprintln!("daemon: hotkey fired id={} (daemon-event only)", id.0);
                    }
                }
                Err(e) => {
                    eprintln!("daemon: hotkey poll failed: {e}");
                    break;
                }
            }
        }
    });
}

#[cfg(unix)]
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

/// Elevated-helper worker entry point (Windows only): connect to the
/// daemon's reverse-connect pipe and serve `send_input_batch` jobs.
///
/// Input-only by design: no profile lock, no store, no monitor, no config.
/// Every job revalidates foreground + high integrity with the target pinned
/// by process handle; disconnect self-exits. Never logs payload bytes.
#[cfg(all(windows, feature = "backend-windows"))]
fn run_elevated_worker(pipe_name: &str) -> i32 {
    use std::io::{BufRead as _, Write as _};

    let mut pipe = None;
    for _ in 0..200 {
        if let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe_name)
        {
            pipe = Some(file);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let Some(pipe) = pipe else {
        eprintln!("helper worker: pipe connect timed out");
        return 1;
    };
    let writer = match pipe.try_clone() {
        Ok(writer) => std::sync::Arc::new(std::sync::Mutex::new(writer)),
        Err(e) => {
            eprintln!("helper worker: pipe clone failed: {e}");
            return 1;
        }
    };
    std::thread::spawn({
        let writer = std::sync::Arc::clone(&writer);
        move || {
            let now_ms = || {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis())
            };
            loop {
                std::thread::sleep(std::time::Duration::from_secs(15));
                let Ok(mut writer) = writer.lock() else {
                    return;
                };
                if writeln!(*writer, "{{\"heartbeat\":{}}}", now_ms()).is_err() {
                    return;
                }
                if writer.flush().is_err() {
                    return;
                }
            }
        }
    });

    let mut reader = std::io::BufReader::new(pipe);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return 0,
            Ok(_) => {}
            Err(_) => return 1,
        }
        let job = match clipmonger_backend_windows::helper::decode_job(&line) {
            Ok(job) => job,
            Err(e) => {
                respond(&writer, 0, &Err(format!("refused: {e}")));
                continue;
            }
        };
        let outcome = (|| {
            let pinned = clipmonger_backend_windows::helper::pin_helper_target(job.target_pid)?;
            clipmonger_backend_windows::paste::worker_send_input_batch(&job.payload)
                .map_err(|detail| format!("inject failed: {detail}"))?;
            let _ = pinned;
            Ok::<(), String>(())
        })();
        eprintln!(
            "helper worker: job={} target={} ok={}",
            job.job_id,
            job.target_pid,
            outcome.is_ok()
        );
        respond(&writer, job.job_id, &outcome);
    }
}

/// Write one result line to the daemon pipe (metadata only, lock-shared
/// with the heartbeat thread so lines never interleave).
#[cfg(all(windows, feature = "backend-windows"))]
fn respond(
    writer: &std::sync::Arc<std::sync::Mutex<std::fs::File>>,
    job_id: u64,
    outcome: &Result<(), String>,
) {
    use std::io::Write as _;
    let line = match outcome {
        Ok(()) => serde_json::json!({"job_id": job_id, "ok": true}),
        Err(e) => serde_json::json!({"job_id": job_id, "ok": false, "error": e}),
    };
    let mut text = serde_json::to_string(&line).unwrap_or_default();
    text.push('\n');
    let Ok(mut writer) = writer.lock() else {
        std::process::exit(1);
    };
    if writer.write_all(text.as_bytes()).is_err() {
        std::process::exit(1);
    }
    if writer.flush().is_err() {
        std::process::exit(1);
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(all(windows, feature = "backend-windows"))]
    if let Some(index) = args.iter().position(|arg| arg == "__elevated-worker") {
        let pipe_name = args.get(index + 1).cloned().unwrap_or_default();
        if pipe_name.is_empty() {
            eprintln!("helper worker: missing pipe name");
            std::process::exit(2);
        }
        std::process::exit(run_elevated_worker(&pipe_name));
    }
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

    #[cfg(unix)]
    let socket = socket_path_for(&profile, &runtime_dir());
    #[cfg(windows)]
    let socket = PathBuf::from(pipe_name_for(&profile));
    #[cfg(unix)]
    if let Some(parent) = socket.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("daemon: socket dir failed: {e}");
            std::process::exit(1);
        }
    }

    // WIN-F2: load `<profile>/config.toml`, defaults when missing. The popup
    // hotkey and paste budget below are wired from it on Windows.
    let config = Config::load(&profile.join("config.toml")).unwrap_or_default();

    let state = Arc::new(DaemonState::new(store));
    state.set_profile_dir(&profile);
    state.set_paste_limits(config.popup.unicode_limit, config.popup.delivery_dwell_ms);
    let backend_shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    #[cfg(all(windows, feature = "backend-windows"))]
    setup_windows_backend(&profile, &state, &config, &backend_shutdown);
    let stop: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());
    let stop_handler = Arc::clone(&stop);
    let backend_stop = Arc::clone(&backend_shutdown);
    let handler: clipmonger_ipc::server::Handler = Arc::new(move |method, params| {
        if method == "daemon.stop" {
            backend_stop.store(true, std::sync::atomic::Ordering::Relaxed);
            stop_handler.notify_one();
            return Ok(serde_json::json!({"ok": true}));
        }
        state.handle(&method, &params)
    });

    #[cfg(unix)]
    if let Err(e) = clipmonger_ipc::server::Server::bind(&socket, token, handler).await {
        eprintln!("daemon: bind failed: {e}");
        std::process::exit(1);
    }
    #[cfg(windows)]
    if let Err(e) =
        clipmonger_ipc::server::Server::bind_pipe(&socket.to_string_lossy(), token, handler)
    {
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
    #[cfg(unix)]
    let _ = fs::remove_file(&socket);
}

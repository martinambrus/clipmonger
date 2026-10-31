//! `clipmonger-daemon`: serving mode binds the profile socket over IPC;
//! headless `--script` mode replays capture lines and exits.
// Background process (daemon, elevated helper at logon): never open a
// console window on Windows. Piped stdio from a parent still works.
#![cfg_attr(windows, windows_subsystem = "windows")]

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

/// The popup UI binary next to this daemon.
fn sibling_ui_exe() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("clipmonger-daemon"));
    let dir = exe
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    dir.join(format!("clipmonger{}", std::env::consts::EXE_SUFFIX))
}

/// Whether process `pid` runs the binary at `exe`.
fn process_runs(pid: u32, exe: &Path) -> bool {
    #[cfg(all(windows, feature = "backend-windows"))]
    {
        clipmonger_backend_windows::process_image_path(pid)
            .is_some_and(|path| path.eq_ignore_ascii_case(&exe.to_string_lossy()))
    }
    #[cfg(unix)]
    {
        fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|path| path == exe)
    }
    #[cfg(not(any(all(windows, feature = "backend-windows"), unix)))]
    {
        let _ = (pid, exe);
        false
    }
}

/// Only the sibling UI binary may receive popup pushes (they carry gesture
/// tokens), and only with a popup window it owns.
fn ui_subscribe_guard() -> clipmonger_ipc::server::SubscribeGuard {
    use clipmonger_ipc::proto::methods::SHOW_POPUP_TOPIC;
    let ui_exe = sibling_ui_exe();
    Arc::new(move |topic, peer_pid, params| {
        if topic != SHOW_POPUP_TOPIC {
            return true;
        }
        let Some(pid) = peer_pid else {
            return false;
        };
        if !process_runs(pid, &ui_exe) {
            return false;
        }
        #[cfg(all(windows, feature = "backend-windows"))]
        if let Some(hwnd) = params.get("popup_hwnd").and_then(serde_json::Value::as_u64) {
            if clipmonger_backend_windows::hwnd_owner_pid(hwnd.cast_signed()) != Some(pid) {
                return false;
            }
        }
        let _ = params;
        true
    })
}

/// Start the sibling UI with its popup showing. `CLIPMONGER_NO_UI_SPAWN`
/// turns this off (binary tests).
fn ui_launcher(profile: PathBuf) -> clipmonger_daemon::popup::UiLauncher {
    Arc::new(move || {
        if std::env::var_os("CLIPMONGER_NO_UI_SPAWN").is_some() {
            return false;
        }
        let spawned = std::process::Command::new(sibling_ui_exe())
            .arg("--profile-dir")
            .arg(&profile)
            .arg("--popup")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match spawned {
            Ok(child) => {
                // A press gave this process the foreground right; pass it on.
                #[cfg(all(windows, feature = "backend-windows"))]
                let _ = clipmonger_backend_windows::allow_set_foreground(child.id());
                // Reap the child when it exits (no zombies).
                #[cfg(unix)]
                {
                    let mut child = child;
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                }
                #[cfg(not(unix))]
                drop(child);
                true
            }
            Err(e) => {
                eprintln!("daemon: UI start failed: {e}");
                false
            }
        }
    })
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
#[cfg(any(
    all(windows, feature = "backend-windows"),
    all(unix, feature = "backend-x11"),
    all(unix, feature = "backend-wayland")
))]
fn wanted_format(mime: &str, formats: &[String]) -> bool {
    formats.iter().any(|format| {
        format == mime
            || (mime == "text/plain" && format == "text/plain;charset=utf-8")
            || (format == "text/plain" && mime == "text/plain;charset=utf-8")
    })
}

/// Construct the live Windows backend: typed caps, monitor thread (real
/// payloads via transfer honoring opt-out + privacy counter), the popup
/// chord thread, and the admin-window consent watcher.
#[cfg(all(windows, feature = "backend-windows"))]
#[allow(clippy::too_many_lines)]
fn setup_windows_backend(
    profile: &Path,
    state: &Arc<DaemonState>,
    config: &Config,
    shutdown: &Arc<std::sync::atomic::AtomicBool>,
) {
    use clipmonger_backend_windows::{caps as backend_caps, WinAutostart, WinMonitor, WinTransfer};
    use clipmonger_platform::traits::{AutostartManager, ClipboardMonitor, ClipboardTransfer};
    use clipmonger_platform::types::{Level, SubscribeKind};

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

    // Autostart handle for future UI wiring; never enabled at startup.
    // Portable when the profile lives beside the daemon binary.
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("clipmonger-daemon"));
    let exe_dir = exe
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let autostart = WinAutostart::new(exe, profile.starts_with(&exe_dir));
    let _ = autostart.is_enabled();

    let mut caps = backend_caps::caps_for(false, false);
    if !monitor_ok {
        caps.monitor = Level::Degraded;
        caps.notes
            .push("monitor Degraded: session clipboard unavailable".to_string());
    }
    {
        // The chord thread registers the caps with its hotkey state; wait
        // for its first try so `status` reports a refused chord from the
        // start.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let state = Arc::clone(state);
        let shutdown = Arc::clone(shutdown);
        std::thread::spawn(move || popup_chord_loop(&state, &caps, &shutdown, ready_tx));
        let _ = ready_rx.recv_timeout(std::time::Duration::from_secs(5));
    }
    {
        let profile = profile.to_path_buf();
        let state = Arc::clone(state);
        let shutdown = Arc::clone(shutdown);
        std::thread::spawn(move || admin_consent_loop(&profile, &state, &shutdown));
    }

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
                    let mut seen = std::collections::HashSet::new();
                    for offer in &event.offers {
                        if !wanted_format(&offer.mime, &formats) {
                            continue;
                        }
                        if !seen.insert(offer.mime.clone()) {
                            continue;
                        }
                        match transfer.read(&offer.mime, max_bytes) {
                            Ok(bytes) => payloads.push((offer.mime.clone(), bytes)),
                            Err(e) => {
                                eprintln!("daemon: payload read skipped ({}): {e}", offer.mime);
                                let _ = std::fs::OpenOptions::new()
                                    .create(true)
                                    .append(true)
                                    .open("C:\\Temp\\monitor.log")
                                    .and_then(|mut f| {
                                        use std::io::Write;
                                        writeln!(f, "read skipped {}: {}", offer.mime, e)
                                    });
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
}

/// Popup chord (Windows), the one owner of the daemon's hotkeys. A press
/// lands here, so the daemon may raise the UI's popup (seg-02 §9).
/// `popup.hotkey` is read live; a refused chord keeps the previous one and
/// is retried; caps carry the result. `ready` fires after the first try.
#[cfg(all(windows, feature = "backend-windows"))]
fn popup_chord_loop(
    state: &DaemonState,
    base_caps: &clipmonger_platform::types::BackendCaps,
    shutdown: &std::sync::atomic::AtomicBool,
    ready: std::sync::mpsc::Sender<()>,
) {
    use clipmonger_backend_windows::WinHotkeys;
    use clipmonger_daemon::popup::{ChordStatus, PopupChord, PopupSource};
    use clipmonger_platform::traits::HotkeyManager;
    use clipmonger_platform::types::{HotkeyId, Level};

    // The `global-hotkey` window belongs to the thread that builds the
    // manager and fires only while that thread pumps (inside `poll`).
    let mut hotkeys = WinHotkeys::new();
    let mut chord = PopupChord::default();
    let mut ready = Some(ready);
    while !shutdown.load(std::sync::atomic::Ordering::Relaxed) {
        let wanted = state.popup_hotkey();
        if let Some(status) = chord.sync(&mut hotkeys, &wanted, std::time::Instant::now()) {
            let mut caps = base_caps.clone();
            if let ChordStatus::Refused(why) = status {
                eprintln!("daemon: popup hotkey unavailable: {why}");
                caps.hotkey = Level::Degraded;
                caps.notes.push(format!("hotkey Degraded: {why}"));
            }
            state.register_backend("windows", caps);
        }
        if let Some(ready) = ready.take() {
            let _ = ready.send(());
        }
        match hotkeys.poll(std::time::Duration::from_millis(200)) {
            Ok(ids) => {
                if ids.contains(&HotkeyId(1)) {
                    state.trigger_popup(PopupSource::Press);
                }
            }
            Err(e) => {
                eprintln!("daemon: hotkey poll failed: {e}");
                break;
            }
        }
    }
}

/// Admin-window support (Windows): when an elevated window is focused and
/// no elevated helper is connected, ask once per daemon run, then start the
/// helper behind a single UAC prompt. Installed (Program Files) builds keep
/// a logon task so later logons start the helper silently; portable builds
/// never create a task (a task pointing at a user-writable exe would let any
/// same-user process run code elevated without UAC) and their helper lives
/// until the daemon exits.
#[cfg(all(windows, feature = "backend-windows"))]
fn admin_consent_loop(
    profile: &Path,
    state: &DaemonState,
    shutdown: &std::sync::atomic::AtomicBool,
) {
    use clipmonger_backend_windows::{elevated_task, helper, sys};
    use std::time::{Duration, Instant};

    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("clipmonger-daemon"));
    let installed =
        helper::protected_path_ok(&exe, &clipmonger_backend_windows::program_files_dir());
    let mut snoozed = false;
    let mut last_task_check: Option<Instant> = None;
    let mut launched_at: Option<Instant> = None;
    while !shutdown.load(std::sync::atomic::Ordering::Relaxed) {
        std::thread::sleep(Duration::from_secs(1));
        if state.helper_connected() {
            launched_at = None;
            continue;
        }
        if snoozed {
            continue;
        }
        if let Some(at) = launched_at {
            // Give a just-launched helper time to connect before asking again
            // (it retries for 30 s); one that never connects stops the asking
            // until the next start.
            if at.elapsed() < Duration::from_secs(30) {
                continue;
            }
            state.note_elevation_refused("elevation refused: helper never connected");
            launched_at = None;
            snoozed = true;
            continue;
        }
        let admin_focused = sys::foreground_hwnd()
            .and_then(sys::window_pid)
            .and_then(sys::process_elevated)
            .unwrap_or(false);
        if !admin_focused {
            continue;
        }
        if installed {
            // Consent given earlier: start the logon task (no UAC). Checked
            // at most once a minute so a failing helper cannot loop.
            if last_task_check.is_some_and(|at| at.elapsed() < Duration::from_secs(60)) {
                continue;
            }
            last_task_check = Some(Instant::now());
            if elevated_task::task_matches(&exe, profile) {
                if let Err(e) = elevated_task::run_task() {
                    eprintln!("daemon: helper task start failed: {e}");
                }
                continue;
            }
        }
        if !elevated_task::prompt_consent(installed) {
            snoozed = true;
            continue;
        }
        let launched = if installed {
            elevated_task::install_via_uac(&exe, profile).and_then(|()| elevated_task::run_task())
        } else {
            clipmonger_backend_windows::spawn_elevated_worker(&exe.to_string_lossy(), profile)
        };
        match launched {
            Ok(()) => launched_at = Some(Instant::now()),
            Err(e) => {
                // UAC cancelled or launch blocked: never re-prompt in a loop;
                // the next daemon start asks again.
                state.note_elevation_refused(&format!("elevation refused: {e}"));
                snoozed = true;
            }
        }
    }
}

/// Construct the live X11 backend: typed caps, monitor thread (real
/// payloads via transfer honoring the denylist + privacy counter), and
/// the popup-hotkey listener (presses trigger the popup, like Windows).
#[cfg(all(unix, feature = "backend-x11"))]
#[allow(clippy::too_many_lines)]
fn setup_x11_backend(
    profile: &Path,
    state: &Arc<DaemonState>,
    config: &Config,
    shutdown: &Arc<std::sync::atomic::AtomicBool>,
) {
    use clipmonger_backend_x11::{
        caps as backend_caps, MonitorOpts, X11Autostart, X11Hotkeys, X11Monitor, X11Transfer,
    };
    use clipmonger_platform::traits::{
        AutostartManager, ClipboardMonitor, ClipboardTransfer, HotkeyManager,
    };
    use clipmonger_platform::types::{HotkeyId, Level, SubscribeKind};

    let transfer = match X11Transfer::connect(None) {
        Ok(transfer) => Some(Arc::new(transfer)),
        Err(e) => {
            eprintln!("daemon: x11 transfer unavailable: {e}");
            None
        }
    };

    let mut denylist = config.monitor.ignores.clone();
    denylist.extend(config.privacy.denylist.iter().cloned());
    let mut monitor_ok = transfer.is_some();
    let monitor = match X11Monitor::connect(
        None,
        MonitorOpts {
            capture_primary: config.monitor.capture_primary,
            debounce_ms: config.monitor.primary_debounce_ms,
            denylist,
            max_bytes: config.monitor.max_bytes,
        },
    ) {
        Ok(mut monitor) => {
            let mut kinds = vec![SubscribeKind::Clipboard];
            if config.monitor.capture_primary {
                kinds.push(SubscribeKind::Primary);
            }
            if monitor.subscribe(&kinds).is_err() {
                monitor_ok = false;
            }
            Some(monitor)
        }
        Err(e) => {
            eprintln!("daemon: x11 monitor unavailable: {e}");
            monitor_ok = false;
            None
        }
    };

    let mut hotkeys = match X11Hotkeys::connect(None) {
        Ok(hotkeys) => Some(hotkeys),
        Err(e) => {
            eprintln!("daemon: x11 hotkeys unavailable: {e}");
            None
        }
    };
    let mut hotkey_conflict = false;
    if let Some(hotkeys) = hotkeys.as_mut() {
        // Registered once: a `popup.hotkey` change needs a daemon restart on
        // X11 (live on Windows via `popup_chord_loop`; ADR-004 open item 1).
        if let Err(e) = hotkeys.register(HotkeyId(1), &config.popup.hotkey) {
            eprintln!("daemon: popup hotkey unavailable: {e}");
            hotkey_conflict = true;
        }
    } else {
        hotkey_conflict = true;
    }

    // Autostart handle for future UI wiring; never enabled at startup.
    // Portable when the profile lives beside the daemon binary.
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("clipmonger-daemon"));
    let exe_dir = exe
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let home = std::env::var_os("HOME").map_or_else(|| profile.to_path_buf(), PathBuf::from);
    let autostart = X11Autostart::new(exe, home.join(".config"), profile.starts_with(&exe_dir));
    let _ = autostart.is_enabled();

    let display = std::env::var("DISPLAY").ok().filter(|d| !d.is_empty());
    let mut caps = backend_caps(display.as_deref());
    if hotkey_conflict {
        caps.hotkey = Level::Degraded;
        caps.notes
            .push("hotkey Degraded: popup chord already registered".to_string());
    }
    if !monitor_ok {
        caps.monitor = Level::Degraded;
        caps.notes
            .push("monitor Degraded: X session clipboard unavailable".to_string());
    }
    state.register_backend("x11", caps);

    if let (Some(mut monitor), Some(transfer)) = (monitor, transfer) {
        let state = Arc::clone(state);
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
                        state.ingest_capture("x11", true, &[]);
                        continue;
                    }
                    let mut payloads = Vec::new();
                    let mut seen = std::collections::HashSet::new();
                    for offer in &event.offers {
                        if !wanted_format(&offer.mime, &formats) {
                            continue;
                        }
                        if !seen.insert(offer.mime.clone()) {
                            continue;
                        }
                        match transfer.read(&offer.mime, max_bytes) {
                            Ok(bytes) => payloads.push((offer.mime.clone(), bytes)),
                            Err(e) => {
                                eprintln!("daemon: payload read skipped ({}): {e}", offer.mime);
                                let _ = std::fs::OpenOptions::new()
                                    .create(true)
                                    .append(true)
                                    .open("C:\\Temp\\monitor.log")
                                    .and_then(|mut f| {
                                        use std::io::Write;
                                        writeln!(f, "read skipped {}: {}", offer.mime, e)
                                    });
                            }
                        }
                    }
                    if !payloads.is_empty() {
                        state.ingest_capture("x11", false, &payloads);
                    }
                }
            }
        });
    }

    if let Some(mut hotkeys) = hotkeys {
        let shutdown = Arc::clone(shutdown);
        let state = Arc::clone(state);
        std::thread::spawn(move || {
            while !shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                match hotkeys.poll(std::time::Duration::from_millis(200)) {
                    Ok(ids) => {
                        if ids.contains(&HotkeyId(1)) {
                            state.trigger_popup(clipmonger_daemon::popup::PopupSource::Press);
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
}

/// Which session backends to start: X11 when `DISPLAY` is set, Wayland
/// when `WAYLAND_DISPLAY` is set (seg-06 S8 coexistence: both at once).
/// Neither set means offline mode still serves history.
#[cfg(any(
    all(unix, feature = "backend-x11"),
    all(unix, feature = "backend-wayland")
))]
fn want_backends(display: Option<&str>, wayland_display: Option<&str>) -> (bool, bool) {
    let want_x11 = display.is_some_and(|d| !d.is_empty());
    let want_wayland = wayland_display.is_some_and(|d| !d.is_empty());
    (want_x11, want_wayland)
}

/// Construct the live Wayland backend: probe-driven caps, monitor thread
/// (real payloads via transfer honoring the config formats), and the
/// `GlobalShortcuts` portal session for the popup hotkey (presses trigger
/// the popup, like X11/Windows). A missing compositor only degrades caps; the daemon
/// keeps running (offline rule, seg-02 S7).
#[cfg(all(unix, feature = "backend-wayland"))]
#[allow(clippy::too_many_lines)]
fn setup_wayland_backend(
    state: &Arc<DaemonState>,
    config: &Config,
    shutdown: &Arc<std::sync::atomic::AtomicBool>,
) {
    use clipmonger_backend_wayland::{
        caps as backend_caps, probe_current, WlMonitor, WlMonitorOpts, WlTransfer,
    };
    use clipmonger_platform::traits::{ClipboardMonitor, ClipboardTransfer};

    let capture_primary = config.monitor.capture_primary;
    let probe = probe_current();
    let caps = backend_caps::caps_for(&probe, capture_primary, false);

    let transfer = match WlTransfer::connect() {
        Ok(transfer) => Some(Arc::new(transfer)),
        Err(e) => {
            eprintln!("daemon: wayland transfer unavailable: {e}");
            None
        }
    };

    let mut monitor_ok = transfer.is_some();
    let monitor = match WlMonitor::connect(WlMonitorOpts {
        primary: capture_primary,
        debounce_ms: config.monitor.primary_debounce_ms,
        // No core config knob (core is frozen); W-F6 fixes min_len 3.
        min_len: 3,
        capture_primary,
    }) {
        Ok(mut monitor) => {
            let mut kinds = vec![clipmonger_platform::types::SubscribeKind::Clipboard];
            if capture_primary {
                kinds.push(clipmonger_platform::types::SubscribeKind::Primary);
            }
            if monitor.subscribe(&kinds).is_err() {
                monitor_ok = false;
            }
            Some(monitor)
        }
        Err(e) => {
            eprintln!("daemon: wayland monitor unavailable: {e}");
            monitor_ok = false;
            None
        }
    };

    let mut caps = caps;
    if !monitor_ok {
        caps.monitor = clipmonger_platform::types::Level::Degraded;
        caps.notes
            .push("monitor Degraded: Wayland session clipboard unavailable".to_string());
    }
    state.register_backend("wayland", caps);

    if let (Some(mut monitor), Some(transfer)) = (monitor, transfer) {
        let state = Arc::clone(state);
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
                        eprintln!("daemon: wayland monitor poll failed: {e}");
                        break;
                    }
                };
                for event in &events {
                    if event.sensitive {
                        state.ingest_capture("wayland", true, &[]);
                        continue;
                    }
                    let mut payloads = Vec::new();
                    let mut seen = std::collections::HashSet::new();
                    for offer in &event.offers {
                        if !wanted_format(&offer.mime, &formats) {
                            continue;
                        }
                        if !seen.insert(offer.mime.clone()) {
                            continue;
                        }
                        match transfer.read(&offer.mime, max_bytes) {
                            Ok(bytes) => payloads.push((offer.mime.clone(), bytes)),
                            Err(e) => {
                                eprintln!(
                                    "daemon: wayland payload read skipped ({}): {e}",
                                    offer.mime
                                );
                            }
                        }
                    }
                    if !payloads.is_empty() {
                        state.ingest_capture("wayland", false, &payloads);
                    }
                }
            }
        });
    }

    // Portal session owns the popup hotkey (first bind may show a system
    // dialog; `status.portal_bind_pending` forewarns the UI per W-F4).
    clipmonger_daemon::portal::spawn_portal_session(
        Arc::clone(state),
        config.popup.hotkey.clone(),
        capture_primary,
    );
}

/// Start the session backends selected by `want_backends` (X11 and/or
/// Wayland on Linux; Windows keeps its single-backend path). With neither
/// display set this is a no-op and the daemon serves history offline.
fn setup_session_backends(
    profile: &Path,
    state: &Arc<DaemonState>,
    config: &Config,
    backend_shutdown: &Arc<std::sync::atomic::AtomicBool>,
) {
    #[cfg(all(windows, feature = "backend-windows"))]
    setup_windows_backend(profile, state, config, backend_shutdown);
    #[cfg(all(unix, feature = "backend-x11"))]
    {
        let (want_x11, _) = want_backends(
            std::env::var("DISPLAY").ok().as_deref(),
            std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        );
        if want_x11 {
            setup_x11_backend(profile, state, config, backend_shutdown);
        }
    }
    #[cfg(all(unix, feature = "backend-wayland"))]
    {
        let (_, want_wayland) = want_backends(
            std::env::var("DISPLAY").ok().as_deref(),
            std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        );
        if want_wayland {
            setup_wayland_backend(state, config, backend_shutdown);
        }
    }
    let _ = (profile, state, config, backend_shutdown);
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

/// Elevated helper entry point (Windows only): connect DOWN to the daemon's
/// helper pipe for `profile`, check that the pipe server is our own daemon
/// binary, then serve jobs strictly one at a time on this thread (read job,
/// inject, write result). One thread means no concurrent I/O on the
/// synchronous pipe handle. Input-only by design: no hooks, no store, no
/// config, no clipboard; payload bytes are never logged.
///
/// `reconnect` keeps a logon-task helper waiting for the next daemon; a
/// UAC-launched worker exits together with its daemon.
#[cfg(all(windows, feature = "backend-windows"))]
fn serve_helper(profile: &Path, reconnect: bool) -> i32 {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;
    /// `SECURITY_IDENTIFICATION` quality of service: the pipe server may
    /// identify this elevated client but never impersonate its token.
    const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;

    // Elevated now: drop a helper task left by older builds (it may run a
    // user-writable binary elevated at every logon).
    clipmonger_backend_windows::elevated_task::remove_legacy_task();
    let pipe_name = clipmonger_backend_windows::helper::persistent_pipe_name(profile);
    let own_exe = std::env::current_exe()
        .map(|exe| exe.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    // A worker was just started for a running daemon; give up if it never
    // shows. A logon-task helper waits for daemons indefinitely.
    let mut attempts_left: u32 = 30;
    loop {
        if let Ok(pipe) = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(&pipe_name)
        {
            let server_ok = clipmonger_backend_windows::pipe_server_pid(pipe.as_raw_handle())
                .and_then(clipmonger_backend_windows::process_image_path)
                .is_some_and(|image| image.to_ascii_lowercase() == own_exe);
            if !server_ok {
                eprintln!("helper: pipe server is not this daemon binary; dropped");
            } else if serve_jobs(pipe) && !reconnect {
                return 0;
            }
            if !reconnect {
                return 1;
            }
        } else if !reconnect {
            attempts_left = attempts_left.saturating_sub(1);
            if attempts_left == 0 {
                eprintln!("helper: daemon pipe never appeared");
                return 1;
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// Serve jobs on one connected pipe until the daemon disconnects; returns
/// true once the daemon went away (a clean end of this session).
#[cfg(all(windows, feature = "backend-windows"))]
fn serve_jobs(pipe: fs::File) -> bool {
    use std::io::{BufRead as _, Write as _};

    let Ok(mut writer) = pipe.try_clone() else {
        return false;
    };
    let mut reader = std::io::BufReader::new(pipe);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return true,
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let (job_id, outcome) = match clipmonger_backend_windows::helper::decode_job(&line) {
            Ok(job) => (job.job_id, run_helper_job(&job)),
            Err(e) => (0, Err(format!("refused: {e}"))),
        };
        let reply = match &outcome {
            Ok(()) => serde_json::json!({"job_id": job_id, "ok": true}),
            Err(e) => serde_json::json!({"job_id": job_id, "ok": false, "error": e}),
        };
        let mut text = reply.to_string();
        text.push('\n');
        if writer
            .write_all(text.as_bytes())
            .and_then(|()| writer.flush())
            .is_err()
        {
            return true;
        }
    }
}

/// Run one job: revalidate and pin the target, then inject.
#[cfg(all(windows, feature = "backend-windows"))]
fn run_helper_job(job: &clipmonger_backend_windows::helper::Job) -> Result<(), String> {
    use clipmonger_backend_windows::helper::JobOp;
    use clipmonger_backend_windows::paste::{worker_press_paste_chord, worker_send_input_batch};

    let pinned = clipmonger_backend_windows::helper::pin_helper_target(job)?;
    match job.op {
        JobOp::SendInputBatch => worker_send_input_batch(&job.payload),
        JobOp::PasteChord => worker_press_paste_chord(),
    }
    .map_err(|detail| format!("inject failed: {detail}"))?;
    drop(pinned);
    Ok(())
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(all(windows, feature = "backend-windows"))]
    if args.iter().any(|a| a == "__elevated-persistent") {
        let profile = profile_arg(&args);
        std::process::exit(serve_helper(&profile, true));
    }
    #[cfg(all(windows, feature = "backend-windows"))]
    if args.iter().any(|a| a == "__elevated-worker") {
        let profile = profile_arg(&args);
        std::process::exit(serve_helper(&profile, false));
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
    state.set_config(config.clone());
    state.set_paste_limits(config.popup.unicode_limit, config.popup.delivery_dwell_ms);
    // Before the backends start: a hotkey thread may trigger the popup at once.
    let hub = clipmonger_ipc::server::EventHub::new(vec![clipmonger_ipc::server::TopicRule {
        topic: clipmonger_ipc::proto::methods::SHOW_POPUP_TOPIC.to_string(),
        exclusive: true,
    }])
    .with_guard(ui_subscribe_guard());
    state.attach_event_hub(hub.clone());
    state.set_ui_launcher(ui_launcher(profile.clone()));
    let backend_shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    setup_session_backends(&profile, &state, &config, &backend_shutdown);
    let stop: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());
    let stop_handler = Arc::clone(&stop);
    let backend_stop = Arc::clone(&backend_shutdown);
    let handler_state = Arc::clone(&state);
    let handler: clipmonger_ipc::server::Handler = Arc::new(move |method, params| {
        if method == "daemon.stop" {
            backend_stop.store(true, std::sync::atomic::Ordering::Relaxed);
            stop_handler.notify_one();
            return Ok(serde_json::json!({"ok": true}));
        }
        handler_state.handle(&method, &params)
    });

    let ext_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lifecycle_state = Arc::clone(&state);
    let lifecycle: clipmonger_ipc::server::LifecycleCb = Arc::new(move |event| {
        use clipmonger_ipc::server::LifecycleEvent;
        match event {
            LifecycleEvent::Hello { client } if client == "gnome-ext" => {
                ext_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                lifecycle_state.set_extension_connected(true);
            }
            LifecycleEvent::Disconnected { client } if client == "gnome-ext" => {
                let prev = ext_count.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                if prev <= 1 {
                    ext_count.store(0, std::sync::atomic::Ordering::SeqCst);
                    lifecycle_state.set_extension_connected(false);
                }
            }
            _ => {}
        }
    });

    let options = clipmonger_ipc::server::ServeOptions {
        lifecycle: Some(lifecycle),
        hub: Some(hub),
        #[cfg(all(windows, feature = "backend-windows"))]
        pipe_peer_pid: Some(Arc::new(|handle| {
            clipmonger_backend_windows::pipe_client_pid(handle as *mut std::ffi::c_void)
        })),
        #[cfg(not(all(windows, feature = "backend-windows")))]
        pipe_peer_pid: None,
    };
    #[cfg(unix)]
    if let Err(e) =
        clipmonger_ipc::server::Server::bind_with(&socket, token, handler, options).await
    {
        eprintln!("daemon: bind failed: {e}");
        std::process::exit(1);
    }
    #[cfg(windows)]
    if let Err(e) = clipmonger_ipc::server::Server::bind_pipe_with(
        &socket.to_string_lossy(),
        token,
        handler,
        options,
    ) {
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
        () = async {
            // A windowless daemon may have no console to take Ctrl+C from;
            // then only `daemon.stop` ends it.
            if tokio::signal::ctrl_c().await.is_err() {
                std::future::pending::<()>().await;
            }
        } => {}
    }
    let _ = fs::remove_file(profile.join("rendezvous.json"));
    #[cfg(unix)]
    let _ = fs::remove_file(&socket);
}

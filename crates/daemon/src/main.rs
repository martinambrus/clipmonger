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

/// Whether the X11 hotkey thread should run: not on `KWin` Wayland (it
/// triggers an input-control prompt) nor on wlroots-family sessions (an
/// `XWayland` grab sees keys only while an X11 window has focus; the user
/// binds the popup command in the compositor instead).
#[cfg(linux_x11)]
fn x11_hotkeys_wanted() -> bool {
    #[cfg(linux_wayland)]
    {
        !clipmonger_daemon::kwin::is_kwin_wayland() && !clipmonger_daemon::wlr::is_wlr_wayland()
    }
    #[cfg(not(linux_wayland))]
    {
        true
    }
}

/// Stop the daemon once the session's display server closes: Wayland when
/// the session is Wayland, else X11. Without a reachable display (headless,
/// tests without one) nothing is watched.
#[cfg(unix)]
fn watch_session_end(
    stop: Arc<tokio::sync::Notify>,
    backend_shutdown: Arc<std::sync::atomic::AtomicBool>,
) {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let x11 = std::env::var_os("DISPLAY").is_some();
    if !wayland && !x11 {
        return;
    }
    std::thread::spawn(move || {
        #[allow(unused_mut)]
        let mut closed: Option<bool> = None;
        #[cfg(linux_wayland)]
        if wayland {
            closed = Some(clipmonger_backend_wayland::wait_compositor_closed().is_ok());
        }
        #[cfg(linux_x11)]
        if closed.is_none() && x11 {
            closed = Some(clipmonger_backend_x11::wait_display_closed(None).is_ok());
        }
        if closed == Some(true) {
            eprintln!("daemon: the graphical session ended; stopping");
            backend_shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
            stop.notify_one();
        }
    });
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
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|path| path == exe)
    }
    #[cfg(target_os = "macos")]
    {
        clipmonger_platform::process::image_path(pid).is_some_and(|path| {
            path == exe || fs::canonicalize(&path).ok() == fs::canonicalize(exe).ok()
        })
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
                let _ = clipmonger_core::runtime_log::global_emit(
                    clipmonger_core::runtime_log::Role::Daemon,
                    clipmonger_core::runtime_log::Severity::Warn,
                    "ui.spawn_failed",
                    &[
                        ("class", "io"),
                        ("code", &e.raw_os_error().unwrap_or(0).to_string()),
                    ],
                );
                eprintln!("daemon: UI start failed: {e}");
                false
            }
        }
    })
}

/// Generate a single-line token for `<profile>/token`.
fn generate_token() -> String {
    // 128 bits from the operating system (getrandom: every supported OS).
    if let (Ok(high), Ok(low)) = (
        clipmonger_core::entropy::random_u64(),
        clipmonger_core::entropy::random_u64(),
    ) {
        return format!("{high:016x}{low:016x}");
    }
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
    use clipmonger_backend_windows::{
        caps as backend_caps, CapturePolicy, SelfWriteProbe, WinCaptureTransfer, WinMonitor,
    };
    use clipmonger_platform::types::SubscribeKind;

    let policy: CapturePolicy = {
        let state = Arc::clone(state);
        Arc::new(move || state.windows_capture_exclusions())
    };
    let self_writes: SelfWriteProbe = {
        let state = Arc::clone(state);
        Arc::new(move |sequence| state.expects_self_write(sequence))
    };
    let monitor = {
        let policy = Arc::clone(&policy);
        move || {
            WinMonitor::with_policy(Arc::clone(&policy))
                .map(|monitor| monitor.with_self_writes(Arc::clone(&self_writes)))
        }
    };
    let initial_monitor =
        clipmonger_daemon::capture::subscribed(&monitor, &[SubscribeKind::Clipboard])
            .map_err(|e| eprintln!("daemon: windows monitor unavailable: {e}"))
            .ok();
    let monitor_ok = initial_monitor.is_some();

    let base_caps = backend_caps::caps_for(false, false);
    state.register_backend("windows", base_caps);
    state.set_monitor_available("windows", monitor_ok);
    state.set_hotkey_status(
        "windows",
        &clipmonger_daemon::popup::ChordStatus::Refused("popup hotkey starting".into()),
    );
    {
        // The chord thread registers the caps with its hotkey state; wait
        // for its first try so `status` reports a refused chord from the
        // start.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let state = Arc::clone(state);
        let shutdown = Arc::clone(shutdown);
        std::thread::spawn(move || popup_chord_loop(&state, &shutdown, ready_tx));
        let _ = ready_rx.recv_timeout(std::time::Duration::from_secs(5));
    }
    {
        let profile = profile.to_path_buf();
        let state = Arc::clone(state);
        let shutdown = Arc::clone(shutdown);
        std::thread::spawn(move || admin_consent_loop(&profile, &state, &shutdown));
    }

    {
        let state = Arc::clone(state);
        let shutdown = Arc::clone(shutdown);
        std::thread::spawn(move || {
            let mut edge = clipmonger_daemon::capture::ScrollLockEdge::new(
                clipmonger_backend_windows::sys::scroll_lock_down(),
            );
            while !shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                let down = clipmonger_backend_windows::sys::scroll_lock_down();
                if edge.observe(state.scroll_lock_toggle_enabled(), down) {
                    let _ = state.toggle_monitor_from_scroll_lock();
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });
    }

    {
        let state = Arc::clone(state);
        let policy = Arc::clone(&policy);
        let poll_ms = config.monitor.poll_ms;
        let shutdown = Arc::clone(shutdown);
        std::thread::spawn(move || {
            let poll_timeout = std::time::Duration::from_millis(poll_ms.clamp(50, 5000));
            clipmonger_daemon::capture::run_backend(
                initial_monitor,
                poll_timeout,
                &shutdown,
                &state,
                "windows",
                || clipmonger_daemon::capture::resubscribed(&monitor, &[SubscribeKind::Clipboard]),
                |events| {
                    for event in events {
                        let transfer =
                            WinCaptureTransfer::new(event.native_sequence, Arc::clone(&policy));
                        if let Err(error) =
                            state.capture_event("windows", event, &transfer, |line| {
                                eprintln!("daemon: windows {line}");
                            })
                        {
                            let _ = clipmonger_core::runtime_log::global_emit(
                                clipmonger_core::runtime_log::Role::Daemon,
                                clipmonger_core::runtime_log::Severity::Error,
                                "capture.unavailable",
                                &[("operation", "capture"), ("class", "unavailable")],
                            );
                            eprintln!("daemon: windows capture unavailable: {error}");
                        }
                    }
                },
                |line| eprintln!("daemon: windows {line}"),
            );
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
    shutdown: &std::sync::atomic::AtomicBool,
    ready: std::sync::mpsc::Sender<()>,
) {
    use clipmonger_backend_windows::WinHotkeys;
    let mut ready = Some(ready);
    clipmonger_daemon::popup::run_hotkeys(
        None,
        shutdown,
        || Ok(WinHotkeys::new()),
        || state.popup_hotkey(),
        || {
            if let Some(ready) = ready.take() {
                let _ = ready.send(());
            }
        },
        |status| {
            if let clipmonger_daemon::popup::ChordStatus::Refused(why) = &status {
                eprintln!("daemon: windows popup hotkey unavailable: {why}");
            } else {
                eprintln!("daemon: windows popup hotkey available");
            }
            state.set_hotkey_status("windows", &status);
        },
        || {
            state.trigger_popup(clipmonger_daemon::popup::PopupSource::Press);
        },
        |delay| {
            clipmonger_daemon::capture::sleep_interruptible(shutdown, delay, std::thread::sleep);
        },
        Some(state),
        |id, chord, pause| state.press_permanent(id, chord, "windows", pause),
    );
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

    // An elevated daemon pastes into admin windows itself: no helper, no
    // consent prompt (the helper refuses an elevated requester).
    if clipmonger_backend_windows::current_process_integrity()
        == clipmonger_backend_windows::Integrity::High
    {
        return;
    }
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
            elevated_task::install_via_uac(&exe, profile)
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
#[cfg(linux_x11)]
#[allow(clippy::too_many_lines)]
fn setup_x11_backend(
    _profile: &Path,
    state: &Arc<DaemonState>,
    config: &Config,
    shutdown: &Arc<std::sync::atomic::AtomicBool>,
) {
    use clipmonger_backend_x11::{
        caps as backend_caps, MonitorOpts, X11Hotkeys, X11Monitor, X11Transfer,
    };
    use clipmonger_platform::types::SubscribeKind;

    let transfer = match X11Transfer::connect(None) {
        Ok(transfer) => Some(Arc::new(transfer)),
        Err(e) => {
            let _ = clipmonger_core::runtime_log::global_emit(
                clipmonger_core::runtime_log::Role::Daemon,
                clipmonger_core::runtime_log::Severity::Warn,
                "capture.transfer_init_failed",
                &[("class", clipmonger_daemon::runtime_platform_class(&e))],
            );
            eprintln!("daemon: x11 transfer unavailable: {e}");
            None
        }
    };

    let mut denylist = config.monitor.ignores.clone();
    denylist.extend(config.privacy.denylist.iter().cloned());
    let monitor_opts = MonitorOpts {
        capture_primary: config.monitor.capture_primary,
        debounce_ms: config.monitor.primary_debounce_ms,
        denylist,
        max_bytes: config.monitor.max_bytes,
    };
    let mut kinds = vec![SubscribeKind::Clipboard];
    if config.monitor.capture_primary {
        kinds.push(SubscribeKind::Primary);
    }
    let monitor = clipmonger_daemon::capture::subscribed(
        || X11Monitor::connect(None, monitor_opts.clone()),
        &kinds,
    )
    .map_err(|e| eprintln!("daemon: x11 monitor unavailable: {e}"))
    .ok();

    let display = std::env::var("DISPLAY").ok().filter(|d| !d.is_empty());
    state.register_backend("x11", backend_caps(display.as_deref()));
    let monitor = if transfer.is_some() { monitor } else { None };
    state.set_monitor_available("x11", monitor.is_some());
    state.set_hotkey_status(
        "x11",
        &clipmonger_daemon::popup::ChordStatus::Refused("popup hotkey starting".into()),
    );

    // Capture thread always starts; shared recovery retries factory with backoff.
    {
        let state = Arc::clone(state);
        let transfer = transfer.clone();
        let poll_ms = config.monitor.poll_ms;
        let shutdown = Arc::clone(shutdown);
        let monitor_opts_clone = monitor_opts.clone();
        let kinds_clone = kinds.clone();
        let initial_monitor_opt = monitor;
        std::thread::spawn(move || {
            let poll_timeout = std::time::Duration::from_millis(poll_ms.clamp(50, 5000));
            let transfer = std::cell::RefCell::new(transfer);
            clipmonger_daemon::capture::run_backend(
                initial_monitor_opt,
                poll_timeout,
                &shutdown,
                &state,
                "x11",
                || {
                    let missing_transfer = transfer.borrow().is_none();
                    if missing_transfer {
                        *transfer.borrow_mut() = Some(Arc::new(X11Transfer::connect(None)?));
                    }
                    clipmonger_daemon::capture::resubscribed(
                        || X11Monitor::connect(None, monitor_opts_clone.clone()),
                        &kinds_clone,
                    )
                },
                |events| {
                    let transfer = transfer.borrow();
                    let Some(transfer) = transfer.as_ref() else {
                        return;
                    };
                    for event in events {
                        if let Err(error) =
                            state.capture_event("x11", event, transfer.as_ref(), |line| {
                                eprintln!("daemon: x11 {line}");
                            })
                        {
                            let _ = clipmonger_core::runtime_log::global_emit(
                                clipmonger_core::runtime_log::Role::Daemon,
                                clipmonger_core::runtime_log::Severity::Error,
                                "capture.unavailable",
                                &[("operation", "capture"), ("class", "unavailable")],
                            );
                            eprintln!("daemon: x11 capture unavailable: {error}");
                        }
                    }
                },
                |line| eprintln!("daemon: x11 {line}"),
            );
        });
    }

    // On KWin Wayland the portal holds every chord. KWin also forwards
    // modifier chords to X11 clients, so an XWayland grab would fire too:
    // a second popup toggle, or an X11 paste that KWin stops with a
    // "control input devices" prompt.
    if x11_hotkeys_wanted() {
        let shutdown = Arc::clone(shutdown);
        let state = Arc::clone(state);
        let display = display.clone();
        std::thread::spawn(move || {
            clipmonger_daemon::popup::run_hotkeys_with_activation(
                None,
                &shutdown,
                || X11Hotkeys::connect(None),
                || state.popup_hotkey(),
                || {},
                |status| {
                    if let clipmonger_daemon::popup::ChordStatus::Refused(why) = &status {
                        eprintln!("daemon: x11 popup hotkey unavailable: {why}");
                    } else {
                        eprintln!("daemon: x11 popup hotkey available");
                        // The display answers now: re-probe paste (XTest),
                        // which start-up may have probed with no display.
                        state.refresh_paste_caps("x11", &backend_caps(display.as_deref()));
                    }
                    state.set_hotkey_status("x11", &status);
                },
                |activation| {
                    state.trigger_popup_activation(activation);
                },
                |delay| {
                    clipmonger_daemon::capture::sleep_interruptible(
                        &shutdown,
                        delay,
                        std::thread::sleep,
                    );
                },
                Some(&state),
                |id, chord, pause| state.press_permanent(id, chord, "x11", pause),
            );
        });
    }
}

/// Which session backends to start: X11 when `DISPLAY` is set, Wayland
/// when `WAYLAND_DISPLAY` is set (seg-06 S8 coexistence: both at once).
/// Neither set means offline mode still serves history.
#[cfg(any(linux_x11, linux_wayland))]
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
#[cfg(linux_wayland)]
#[allow(clippy::too_many_lines)]
fn setup_wayland_backend(
    state: &Arc<DaemonState>,
    config: &Config,
    shutdown: &Arc<std::sync::atomic::AtomicBool>,
) {
    use clipmonger_backend_wayland::{
        caps as backend_caps, probe_current, WlMonitor, WlMonitorOpts, WlTransfer,
    };

    let capture_primary = config.monitor.capture_primary;
    let probe = probe_current();
    let mut caps = backend_caps::caps_for(&probe, capture_primary, false);
    // wlroots family and COSMIC: the daemon presses the chord through a
    // virtual keyboard.
    if clipmonger_daemon::wlr::auto_paste_available() {
        caps = backend_caps::with_virtual_keyboard_paste(caps);
    }

    let transfer = match WlTransfer::connect() {
        Ok(transfer) => Some(Arc::new(transfer)),
        Err(e) => {
            let _ = clipmonger_core::runtime_log::global_emit(
                clipmonger_core::runtime_log::Role::Daemon,
                clipmonger_core::runtime_log::Severity::Warn,
                "capture.transfer_init_failed",
                &[("class", clipmonger_daemon::runtime_platform_class(&e))],
            );
            eprintln!("daemon: wayland transfer unavailable: {e}");
            None
        }
    };

    let monitor_opts = WlMonitorOpts {
        primary: capture_primary,
        debounce_ms: config.monitor.primary_debounce_ms,
        // No core config knob (core is frozen); W-F6 fixes min_len 3.
        min_len: 3,
        capture_primary,
    };
    let mut kinds = vec![clipmonger_platform::types::SubscribeKind::Clipboard];
    if capture_primary {
        kinds.push(clipmonger_platform::types::SubscribeKind::Primary);
    }
    let monitor =
        clipmonger_daemon::capture::subscribed(|| WlMonitor::connect(monitor_opts), &kinds)
            .map_err(|e| eprintln!("daemon: wayland monitor unavailable: {e}"))
            .ok();

    state.register_backend("wayland", caps);
    let monitor = if transfer.is_some() { monitor } else { None };
    state.set_monitor_available("wayland", monitor.is_some());

    {
        let state = Arc::clone(state);
        let poll_ms = config.monitor.poll_ms;
        let shutdown = Arc::clone(shutdown);
        let transfer_opt = transfer.clone();
        let initial_monitor_opt = monitor;
        std::thread::spawn(move || {
            let poll_timeout = std::time::Duration::from_millis(poll_ms.clamp(50, 5000));
            let transfer = std::cell::RefCell::new(transfer_opt);
            clipmonger_daemon::capture::run_backend(
                initial_monitor_opt,
                poll_timeout,
                &shutdown,
                &state,
                "wayland",
                || {
                    let missing_transfer = transfer.borrow().is_none();
                    if missing_transfer {
                        *transfer.borrow_mut() = Some(Arc::new(WlTransfer::connect()?));
                    }
                    clipmonger_daemon::capture::resubscribed(
                        || WlMonitor::connect(monitor_opts),
                        &kinds,
                    )
                },
                |events| {
                    let transfer = transfer.borrow();
                    let Some(transfer) = transfer.as_ref() else {
                        return;
                    };
                    for event in events {
                        if let Err(error) =
                            state.capture_event("wayland", event, transfer.as_ref(), |line| {
                                eprintln!("daemon: wayland {line}");
                            })
                        {
                            let _ = clipmonger_core::runtime_log::global_emit(
                                clipmonger_core::runtime_log::Role::Daemon,
                                clipmonger_core::runtime_log::Severity::Error,
                                "capture.unavailable",
                                &[("operation", "capture"), ("class", "unavailable")],
                            );
                            eprintln!("daemon: wayland capture unavailable: {error}");
                        }
                    }
                },
                |line| eprintln!("daemon: wayland {line}"),
            );
        });
    }

    // Portal session owns the popup hotkey (first bind may show a system
    // dialog; `status.portal_bind_pending` forewarns the UI per W-F4).
    clipmonger_daemon::portal::spawn_portal_session(Arc::clone(state), capture_primary);
    // KWin, wlroots family and COSMIC: track the active window for auto-paste
    // (no-op elsewhere).
    clipmonger_daemon::kwin::spawn_focus_tracker(Arc::clone(state));
    clipmonger_daemon::wlr::spawn_focus_tracker(Arc::clone(state));
}

/// Construct the live macOS backend: caps (Accessibility and pasteboard
/// access), the changeCount capture thread, and the hotkey thread
/// (hotkeys live on the main thread's `NSApplication` loop).
#[cfg(macos_backend)]
#[allow(clippy::too_many_lines)]
fn setup_macos_backend(
    profile: &Path,
    state: &Arc<DaemonState>,
    config: &Config,
    shutdown: &Arc<std::sync::atomic::AtomicBool>,
) {
    use clipmonger_backend_macos::{MacHotkeys, MacMonitor, MacTransfer, MonitorOpts};
    use clipmonger_platform::types::SubscribeKind;

    // A LaunchAgent left behind by an app that was moved to the Trash.
    if let Some(agents) = clipmonger_backend_macos::autostart::default_agents_dir() {
        let _ = clipmonger_backend_macos::remove_stale_agent(&agents);
    }
    let _ = profile;
    // Ask once for Accessibility (the system shows its prompt); paste reports
    // Degraded until it is granted.
    let trusted = clipmonger_backend_macos::accessibility_trusted(true);
    state.register_backend(
        "macos",
        clipmonger_backend_macos::caps::caps_for(
            trusted,
            clipmonger_backend_macos::pasteboard_access(),
        ),
    );
    let transfer = match MacTransfer::new() {
        Ok(transfer) => Some(Arc::new(transfer)),
        Err(e) => {
            eprintln!("daemon: macos transfer unavailable: {e}");
            None
        }
    };
    let mut denylist = config.monitor.ignores.clone();
    denylist.extend(config.privacy.denylist.iter().cloned());
    let monitor_opts = MonitorOpts {
        poll_ms: config.monitor.poll_ms,
        denylist,
        max_bytes: config.monitor.max_bytes,
    };
    let kinds = vec![SubscribeKind::Clipboard];
    let monitor =
        clipmonger_daemon::capture::subscribed(|| MacMonitor::new(monitor_opts.clone()), &kinds)
            .map_err(|e| eprintln!("daemon: macos monitor unavailable: {e}"))
            .ok();
    let monitor = if transfer.is_some() { monitor } else { None };
    state.set_monitor_available("macos", monitor.is_some());
    state.set_hotkey_status(
        "macos",
        &clipmonger_daemon::popup::ChordStatus::Refused("popup hotkey starting".into()),
    );

    {
        let state = Arc::clone(state);
        let poll_ms = config.monitor.poll_ms;
        let shutdown = Arc::clone(shutdown);
        std::thread::spawn(move || {
            let poll_timeout = std::time::Duration::from_millis(poll_ms.clamp(50, 5000));
            let transfer = std::cell::RefCell::new(transfer);
            clipmonger_daemon::capture::run_backend(
                monitor,
                poll_timeout,
                &shutdown,
                &state,
                "macos",
                || {
                    let missing_transfer = transfer.borrow().is_none();
                    if missing_transfer {
                        *transfer.borrow_mut() = Some(Arc::new(MacTransfer::new()?));
                    }
                    clipmonger_daemon::capture::resubscribed(
                        || MacMonitor::new(monitor_opts.clone()),
                        &kinds,
                    )
                },
                |events| {
                    let transfer = transfer.borrow();
                    let Some(transfer) = transfer.as_ref() else {
                        return;
                    };
                    for event in events {
                        if let Err(error) =
                            state.capture_event("macos", event, transfer.as_ref(), |line| {
                                eprintln!("daemon: macos {line}");
                            })
                        {
                            eprintln!("daemon: macos capture unavailable: {error}");
                        }
                    }
                },
                |line| eprintln!("daemon: macos {line}"),
            );
        });
    }

    // Hotkeys need the main thread's NSApplication loop (main_loop::run).
    if clipmonger_backend_macos::main_loop::is_running() {
        let shutdown = Arc::clone(shutdown);
        let state = Arc::clone(state);
        std::thread::spawn(move || {
            clipmonger_daemon::popup::run_hotkeys_with_activation(
                None,
                &shutdown,
                MacHotkeys::new,
                || state.popup_hotkey(),
                || {},
                |status| {
                    if let clipmonger_daemon::popup::ChordStatus::Refused(why) = &status {
                        eprintln!("daemon: macos popup hotkey unavailable: {why}");
                    }
                    state.set_hotkey_status("macos", &status);
                },
                |activation| {
                    state.trigger_popup_activation(activation);
                },
                |delay| {
                    clipmonger_daemon::capture::sleep_interruptible(
                        &shutdown,
                        delay,
                        std::thread::sleep,
                    );
                },
                Some(&state),
                |id, chord, pause| state.press_permanent(id, chord, "macos", pause),
            );
        });
    }
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
    #[cfg(macos_backend)]
    setup_macos_backend(profile, state, config, backend_shutdown);
    #[cfg(linux_x11)]
    {
        let (want_x11, _) = want_backends(
            std::env::var("DISPLAY").ok().as_deref(),
            std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        );
        if want_x11 {
            setup_x11_backend(profile, state, config, backend_shutdown);
        }
    }
    #[cfg(linux_wayland)]
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
    use clipmonger_backend_windows::paste::{worker_press_paste_chord, worker_send_input_batch};

    clipmonger_backend_windows::helper::run_pinned_job(
        job,
        clipmonger_backend_windows::helper::pin_helper_target,
        || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        },
        worker_send_input_batch,
        worker_press_paste_chord,
    )
}

fn main() {
    #[cfg(feature = "backend-windows")]
    if let Some(request) = clipmonger_backend_windows::elevated_task::parse_registrar_invocation(
        &std::env::args_os().skip(1).collect::<Vec<_>>(),
    ) {
        use clipmonger_backend_windows::elevated_task::{registrar_exit_code, RegistrarError};
        let result = request.map_err(RegistrarError::from).and_then(|args| {
            clipmonger_backend_windows::elevated_task::register_from_request(&args)
        });
        if let Err(error) = &result {
            eprintln!("elevated helper registration failed: {error}");
        }
        std::process::exit(registrar_exit_code(&result));
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("__thumbnail-decode") {
        if args.len() != 1 {
            eprintln!("thumbnail-decode: unexpected args");
            std::process::exit(2);
        }
        std::process::exit(clipmonger_daemon::thumbnail::run_thumbnail_child());
    }
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
    let run = move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap_or_else(|e| {
                eprintln!("daemon: runtime build failed: {e}");
                std::process::exit(1);
            });
        rt.block_on(async_main(args));
    };
    // macOS: global hotkeys need an NSApplication loop on the main thread;
    // the daemon itself runs on a worker thread. Headless runs skip it.
    #[cfg(macos_backend)]
    if !std::env::args().any(|a| a == "--headless" || a == "--script") {
        clipmonger_backend_macos::main_loop::run(move || {
            run();
            0
        });
    }
    run();
}

#[allow(clippy::too_many_lines)]
async fn async_main(args: Vec<String>) {
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

    if let Err(e) = clipmonger_core::runtime_log::init_global(
        &profile,
        clipmonger_core::runtime_log::Role::Daemon,
    ) {
        eprintln!("daemon: runtime log init failed: {e}");
    }

    let store = match clipmonger_core::Store::open(&profile) {
        Ok(store) => store,
        Err(e) => {
            let _ = clipmonger_core::runtime_log::global_emit(
                clipmonger_core::runtime_log::Role::Daemon,
                clipmonger_core::runtime_log::Severity::Error,
                "startup.store_open_failed",
                &[
                    ("operation", "store_open"),
                    ("class", clipmonger_core::runtime_log::core_class(&e)),
                    ("code", &clipmonger_core::runtime_log::core_code(&e)),
                ],
            );
            eprintln!("daemon: store open failed: {e}");
            std::process::exit(1);
        }
    };

    let token = match write_token(&profile) {
        Ok(token) => token,
        Err(e) => {
            let _ = clipmonger_core::runtime_log::global_emit(
                clipmonger_core::runtime_log::Role::Daemon,
                clipmonger_core::runtime_log::Severity::Error,
                "startup.token_failed",
                &[("class", "io")],
            );
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
            let _ = clipmonger_core::runtime_log::global_emit(
                clipmonger_core::runtime_log::Role::Daemon,
                clipmonger_core::runtime_log::Severity::Error,
                "startup.socket_dir_failed",
                &[
                    ("class", "io"),
                    ("code", &e.raw_os_error().unwrap_or(0).to_string()),
                ],
            );
            eprintln!("daemon: socket dir failed: {e}");
            std::process::exit(1);
        }
    }

    // WIN-F2: load `<profile>/config.toml`, defaults when missing. The popup
    // hotkey and paste budget below are wired from it on Windows.
    let config = Config::load(&profile.join("config.toml")).unwrap_or_else(|error| {
        let _ = clipmonger_core::runtime_log::global_emit(
            clipmonger_core::runtime_log::Role::Daemon,
            clipmonger_core::runtime_log::Severity::Warn,
            "startup.config_failed",
            &[("class", clipmonger_core::runtime_log::core_class(&error))],
        );
        Config::default()
    });

    let state = Arc::new(DaemonState::new(store));
    let _compact_maintenance = clipmonger_daemon::CompactMaintenance::start(&state);
    state.set_profile_dir(&profile);
    state.set_config(config.clone());
    state.set_paste_limits(config.popup.unicode_limit, config.popup.delivery_dwell_ms);
    // Before the backends start: a hotkey thread may trigger the popup at once.
    let hub = clipmonger_ipc::server::EventHub::new(vec![
        clipmonger_ipc::server::TopicRule {
            topic: clipmonger_ipc::proto::methods::SHOW_POPUP_TOPIC.to_string(),
            exclusive: true,
        },
        clipmonger_ipc::server::TopicRule {
            topic: clipmonger_ipc::proto::methods::CONFIG_CHANGED_TOPIC.to_string(),
            exclusive: false,
        },
        clipmonger_ipc::server::TopicRule {
            topic: clipmonger_ipc::proto::methods::HISTORY_CHANGED_TOPIC.to_string(),
            exclusive: false,
        },
        clipmonger_ipc::server::TopicRule {
            topic: clipmonger_ipc::proto::methods::CAPS_CHANGED_TOPIC.to_string(),
            exclusive: false,
        },
        clipmonger_ipc::server::TopicRule {
            topic: clipmonger_ipc::proto::methods::PASTE_ASSIST_TOPIC.to_string(),
            exclusive: false,
        },
        clipmonger_ipc::server::TopicRule {
            topic: clipmonger_ipc::proto::methods::PERMANENTS_CHANGED_TOPIC.to_string(),
            exclusive: false,
        },
    ])
    .with_guard(ui_subscribe_guard());
    state.attach_event_hub(hub.clone());
    state.set_ui_launcher(ui_launcher(profile.clone()));
    // Autostart: exactly one daemon-owned service, independent of display backend selection.
    // Portable performs ZERO manager calls; feature/platform unsupported => unavailable.
    // Startup false => zero OS writes; startup true => enable exactly once.
    {
        let service = std::env::current_exe().ok().map_or_else(
            clipmonger_daemon::autostart::AutostartService::unconfigured,
            |exe| clipmonger_daemon::autostart::AutostartService::from_profile(&profile, &exe),
        );
        let _ = state.set_autostart_service(service);
        let enabled = config.autostart.enabled;
        if let Err(e) = state.apply_autostart_startup(enabled).await {
            let _ = clipmonger_core::runtime_log::global_emit(
                clipmonger_core::runtime_log::Role::Daemon,
                clipmonger_core::runtime_log::Severity::Warn,
                "autostart.startup_failed",
                &[
                    ("class", clipmonger_daemon::runtime_platform_class(&e)),
                    ("code", "startup_failed"),
                ],
            );
        }
    }
    // Desktop entries give the app id the portal and KWin need. Test
    // daemons (profiles under the temp folder) never write them.
    #[cfg(target_os = "linux")]
    if !profile.starts_with(std::env::temp_dir()) {
        let daemon_exe =
            std::env::current_exe().unwrap_or_else(|_| PathBuf::from("clipmonger-daemon"));
        clipmonger_daemon::desktop_entry::ensure(&sibling_ui_exe(), &daemon_exe);
    }
    let backend_shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    setup_session_backends(&profile, &state, &config, &backend_shutdown);
    let stop: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());
    let stop_handler = Arc::clone(&stop);
    let backend_stop = Arc::clone(&backend_shutdown);
    // Stop with the graphical session this daemon started in: a daemon
    // left running after a logout keeps the old session's display,
    // portal and environment, and the next UI would attach to it.
    #[cfg(unix)]
    watch_session_end(Arc::clone(&stop), Arc::clone(&backend_shutdown));
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
    let context = DaemonState::thumbnail_context_handler(Arc::clone(&state));
    #[cfg(unix)]
    if let Err(e) =
        clipmonger_ipc::server::Server::bind_with_context(&socket, token, handler, options, context)
            .await
    {
        let _ = clipmonger_core::runtime_log::global_emit(
            clipmonger_core::runtime_log::Role::Daemon,
            clipmonger_core::runtime_log::Severity::Warn,
            "startup.bind_failed",
            &[
                ("operation", "ipc_bind"),
                ("class", "ipc"),
                ("code", "bind_failed"),
            ],
        );
        eprintln!("daemon: bind failed: {e}");
        std::process::exit(1);
    }
    #[cfg(windows)]
    if let Err(e) = clipmonger_ipc::server::Server::bind_pipe_with_context(
        &socket.to_string_lossy(),
        token,
        handler,
        options,
        context,
    ) {
        let _ = clipmonger_core::runtime_log::global_emit(
            clipmonger_core::runtime_log::Role::Daemon,
            clipmonger_core::runtime_log::Severity::Warn,
            "startup.bind_failed",
            &[
                ("operation", "ipc_bind"),
                ("class", "ipc"),
                ("code", "bind_failed"),
            ],
        );
        eprintln!("daemon: bind failed: {e}");
        std::process::exit(1);
    }
    if let Err(e) = write_rendezvous(&profile, &socket) {
        let _ = clipmonger_core::runtime_log::global_emit(
            clipmonger_core::runtime_log::Role::Daemon,
            clipmonger_core::runtime_log::Severity::Warn,
            "startup.rendezvous_failed",
            &[
                ("class", clipmonger_core::runtime_log::core_class(&e)),
                ("code", &clipmonger_core::runtime_log::core_code(&e)),
            ],
        );
        eprintln!("daemon: rendezvous failed: {e}");
        if !shutdown_thumbnails(&state).await {
            eprintln!("daemon: thumbnail cleanup task failed");
        }
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
    let cleanup_ok = shutdown_thumbnails(&state).await;
    let _ = fs::remove_file(profile.join("rendezvous.json"));
    #[cfg(unix)]
    let _ = fs::remove_file(&socket);
    if !cleanup_ok {
        eprintln!("daemon: thumbnail cleanup task failed");
        std::process::exit(1);
    }
}

async fn shutdown_thumbnails(state: &Arc<DaemonState>) -> bool {
    let thumb_state = Arc::clone(state);
    if let Err(error) =
        tokio::task::spawn_blocking(move || thumb_state.thumbnail_shutdown_and_wait()).await
    {
        eprintln!("daemon: thumbnail cleanup join failed: {error}");
        // Attempt cleanup again, but retain the failure status.
        state.thumbnail_shutdown_and_wait();
        return false;
    }
    true
}

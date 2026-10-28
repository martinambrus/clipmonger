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

    // The global-hotkey HWND lives on the thread that constructs the
    // manager and only fires while that thread pumps messages, so
    // construction + registration happen on the polling thread itself;
    // its result is reported back before caps are computed.
    let (hk_ready_tx, hk_ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    {
        let shutdown = Arc::clone(shutdown);
        let state = Arc::clone(state);
        let chord = config.popup.hotkey.clone();
        std::thread::spawn(move || {
            let mut hotkeys = WinHotkeys::new();
            let registered = hotkeys.register(HotkeyId(1), &chord);
            let ok = registered.is_ok();
            let _ = hk_ready_tx.send(registered.map_err(|e| e.to_string()));
            if !ok {
                return;
            }
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
    let mut hotkey_conflict = false;
    match hk_ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!("daemon: popup hotkey unavailable: {e}");
            hotkey_conflict = true;
        }
        Err(e) => {
            eprintln!("daemon: popup hotkey setup timed out/failed: {e}");
            hotkey_conflict = true;
        }
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
    // Elevated helper auto-install / admin-window consent (one-time UAC, snoozed per boot)
    {
        let profile = profile.to_path_buf();
        let shutdown = std::sync::Arc::clone(shutdown);
        std::thread::spawn(move || {
            use std::sync::atomic::{AtomicBool, Ordering};
            static SNOOZED: AtomicBool = AtomicBool::new(false);
            // If already opted-in, ensure helper task + hotkey helper running
            let cfg = clipmonger_core::Config::load(&profile.join("config.toml")).unwrap_or_default();
            if cfg.windows.helper_auto_elevate {
                let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("clipmonger-daemon"));
                let needs_uac = !clipmonger_backend_windows::elevated_task::is_installed();
                if needs_uac {
                    let _ = clipmonger_backend_windows::elevated_task::install_via_uac(&exe, &profile);
                } else {
                    let _ = clipmonger_backend_windows::elevated_task::ensure_persistent_task(&exe, &profile);
                }
                // spawn High IL hotkey helper now if not already via task
                if !shutdown.load(Ordering::Relaxed) {
                    // helper will be started via scheduled task on next logon; for immediate use the helper pipe path already handles paste
                    // fallback: direct spawn of hotkey helper via runas schtasks path is task-based; for now spawn_elevated for paste worker path not ideal
                }
            }
            while !shutdown.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_secs(5));
                if SNOOZED.load(Ordering::Relaxed) { continue; }
                let cfg = clipmonger_core::Config::load(&profile.join("config.toml")).unwrap_or_default();
                if cfg.windows.helper_auto_elevate { continue; }
                // Check if foreground is High IL
                let is_high = {
                    #[cfg(windows)]
                    {
                        clipmonger_backend_windows::sys::foreground_hwnd()
                            .and_then(|hw| clipmonger_backend_windows::sys::window_pid(hw))
                            .and_then(|pid| clipmonger_backend_windows::sys::process_elevated(pid))
                            .unwrap_or(false)
                    }
                    #[cfg(not(windows))]
                    { false }
                };
                if is_high {
                    if clipmonger_backend_windows::elevated_task::prompt_consent() {
                        let mut cfg2 = clipmonger_core::Config::load(&profile.join("config.toml")).unwrap_or_default();
                        cfg2.windows.helper_auto_elevate = true;
                        let _ = cfg2.save(&profile.join("config.toml"));
                        let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("clipmonger-daemon"));
                        let _ = clipmonger_backend_windows::elevated_task::install_via_uac(&exe, &profile);
                    } else {
                        SNOOZED.store(true, Ordering::Relaxed);
                    }
                }
            }
        });
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
                        if !seen.insert(offer.mime.clone()) { continue; }
                        match transfer.read(&offer.mime, max_bytes) {
                            Ok(bytes) => payloads.push((offer.mime.clone(), bytes)),
                            Err(e) => {
                                eprintln!("daemon: payload read skipped ({}): {e}", offer.mime);
                                let _ = std::fs::OpenOptions::new().create(true).append(true).open("C:\\Temp\\monitor.log").and_then(|mut f| { use std::io::Write; writeln!(f, "read skipped {}: {}", offer.mime, e) });
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

/// Construct the live X11 backend: typed caps, monitor thread (real
/// payloads via transfer honoring the denylist + privacy counter), and
/// the popup-hotkey listener (daemon-event only, like Windows).
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
                        if !seen.insert(offer.mime.clone()) { continue; }
                        match transfer.read(&offer.mime, max_bytes) {
                            Ok(bytes) => payloads.push((offer.mime.clone(), bytes)),
                            Err(e) => {
                                eprintln!("daemon: payload read skipped ({}): {e}", offer.mime);
                                let _ = std::fs::OpenOptions::new().create(true).append(true).open("C:\\Temp\\monitor.log").and_then(|mut f| { use std::io::Write; writeln!(f, "read skipped {}: {}", offer.mime, e) });
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
                        for id in ids {
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
/// `GlobalShortcuts` portal session for the popup hotkey (daemon-event only,
/// like X11/Windows). A missing compositor only degrades caps; the daemon
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
                        if !seen.insert(offer.mime.clone()) { continue; }
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

/// Elevated hotkey helper: High IL hotkey forwarder for admin windows.
/// Registers Ctrl+Alt+H at High IL and forwards WM_HOTKEY to the
/// fallback popup + mints gesture via daemon IPC.
#[cfg(all(windows, feature = "backend-windows"))]
fn run_elevated_hotkey_helper(profile: &std::path::Path) -> i32 {
    use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{CallNextHookEx, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, WM_KEYDOWN, WM_SYSKEYDOWN, HC_ACTION, WH_KEYBOARD_LL, MSG};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_MENU};
    static mut G_PIPE: Option<String> = None;
    static mut G_TOKEN: Option<String> = None;
    unsafe extern "system" fn ll_proc(n_code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
        if n_code as u32 == HC_ACTION && (w_param as u32 == WM_KEYDOWN || w_param as u32 == WM_SYSKEYDOWN) {
            let kb = &*(l_param as *const KBDLLHOOKSTRUCT);
            if kb.vkCode == 0x48 {
                let ctrl = (GetAsyncKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0;
                let alt = (GetAsyncKeyState(VK_MENU as i32) as u16 & 0x8000) != 0;
                if ctrl && alt {
                    if let (Some(pipe), Some(token)) = (G_PIPE.clone(), G_TOKEN.clone()) {
                        if let Ok(client) = clipmonger_ipc::client::Client::connect_pipe(&pipe, &token) {
                            let mut c = client; let _ = c.call("hotkey.fired", serde_json::json!({"id": 1}));
                        }
                    }
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open("C:\\Temp\\helper.log").and_then(|mut f| { use std::io::Write; writeln!(f, "helper fired at {:?}", std::time::SystemTime::now()) });
                    let _ = clipmonger_backend_windows::elevated_task::notify_popup();
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open("C:\\Temp\\helper.log").and_then(|mut f| { use std::io::Write; writeln!(f, "notify done") });

                }
            }
        }
        CallNextHookEx(std::ptr::null_mut(), n_code, w_param, l_param)
    }
    let pipe = clipmonger_ipc::framing::pipe_name_for(profile);
    let token = std::fs::read_to_string(profile.join("token")).unwrap_or_default().trim().to_string();
    unsafe { G_PIPE = Some(pipe); G_TOKEN = Some(token); }
    let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_proc), std::ptr::null_mut(), 0) };
    if hook.is_null() { eprintln!("elevated hotkey helper: hook failed"); return 1; }
    eprintln!("elevated hotkey helper: LL hook at High IL for Ctrl+Alt+H");
    let mut msg = std::mem::MaybeUninit::<MSG>::uninit();
    unsafe { while GetMessageW(msg.as_mut_ptr(), std::ptr::null_mut(), 0, 0) != 0 {} }
    unsafe { UnhookWindowsHookEx(hook) };
    0
}

#[cfg(all(windows, feature = "backend-windows"))]
fn run_elevated_persistent(profile: &std::path::Path) -> i32 {
    use std::io::{BufRead as _, Write as _};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{CallNextHookEx, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, HC_ACTION, WH_KEYBOARD_LL, MSG, WM_KEYDOWN, WM_SYSKEYDOWN};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
    let hotkey_str = std::fs::read_to_string(profile.join("config.toml")).ok().and_then(|c| c.lines().find(|l| l.trim_start().starts_with("hotkey")).and_then(|l| l.split('=').nth(1)).map(|v| v.trim().trim_matches('"').to_string())).unwrap_or_else(|| "ctrl+shift+h".to_string());
    let hotkey_lower = hotkey_str.to_ascii_lowercase();
    let want_ctrl = hotkey_lower.contains("ctrl");
    let want_alt = hotkey_lower.contains("alt");
    let want_shift = hotkey_lower.contains("shift");
    let vk = hotkey_lower.split('+').last().and_then(|k| k.trim().chars().next()).map(|c| (c.to_ascii_uppercase() as u32) & 0xFF).unwrap_or(0x48);
    static mut G_PIPE2: Option<String> = None;
    static mut G_TOKEN2: Option<String> = None;
    static mut G_VK: u32 = 0;
    static mut G_CTRL: bool = false;
    static mut G_ALT: bool = false;
    static mut G_SHIFT: bool = false;
    static mut G_PROFILE: Option<std::path::PathBuf> = None;
    unsafe { G_VK = vk; G_CTRL = want_ctrl; G_ALT = want_alt; G_SHIFT = want_shift; G_PROFILE = Some(profile.to_path_buf()); }
    let pipe = clipmonger_ipc::framing::pipe_name_for(profile);
    let token = std::fs::read_to_string(profile.join("token")).unwrap_or_default().trim().to_string();
    unsafe { G_PIPE2 = Some(pipe.clone()); G_TOKEN2 = Some(token.clone()); }
    unsafe extern "system" fn ll_persist(n_code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
        if n_code as u32 == HC_ACTION && (w_param as u32 == WM_KEYDOWN || w_param as u32 == WM_SYSKEYDOWN) {
            let kb = &*(l_param as *const KBDLLHOOKSTRUCT);
            if kb.vkCode == G_VK {
                let ctrl = (GetAsyncKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0;
                let alt = (GetAsyncKeyState(VK_MENU as i32) as u16 & 0x8000) != 0;
                let shift = (GetAsyncKeyState(VK_SHIFT as i32) as u16 & 0x8000) != 0;
                if ctrl == G_CTRL && alt == G_ALT && shift == G_SHIFT {
                    let (p, t) = if let (Some(p), Some(t)) = (G_PIPE2.clone(), G_TOKEN2.clone()) { (p, t) } else if let Some(prof) = unsafe { G_PROFILE.clone() } { (clipmonger_ipc::framing::pipe_name_for(&prof), std::fs::read_to_string(prof.join("token")).unwrap_or_default().trim().to_string()) } else { (String::new(), String::new()) };
                    if !p.is_empty() && !t.is_empty() {
                        if let Ok(client) = clipmonger_ipc::client::Client::connect_pipe(&p, &t) {
                            let _ = client.call("hotkey.fired", serde_json::json!({"id": 1}));
                        } else if let Some(prof) = unsafe { G_PROFILE.clone() } {
                            let fresh_p = clipmonger_ipc::framing::pipe_name_for(&prof);
                            let fresh_t = std::fs::read_to_string(prof.join("token")).unwrap_or_default().trim().to_string();
                            if !fresh_t.is_empty() {
                                if let Ok(client) = clipmonger_ipc::client::Client::connect_pipe(&fresh_p, &fresh_t) {
                                    let _ = client.call("hotkey.fired", serde_json::json!({"id": 1}));
                                    unsafe { G_PIPE2 = Some(fresh_p); G_TOKEN2 = Some(fresh_t); }
                                }
                            }
                        }
                    }
                    let _ = clipmonger_backend_windows::elevated_task::notify_popup();
                }
            }
        }
        CallNextHookEx(std::ptr::null_mut(), n_code, w_param, l_param)
    }
    let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_persist), std::ptr::null_mut(), 0) };
    if hook.is_null() { eprintln!("persistent helper: hook failed"); return 1; }
    eprintln!("persistent helper: LL hook for {} at High IL", hotkey_str);
    let persist_pipe = clipmonger_backend_windows::helper::persistent_pipe_name(profile);
    let persist_pipe_thread = persist_pipe.clone();
    std::thread::spawn(move || {
        use std::io::{BufRead as _, Write as _};
        loop {
            let mut pipe = None;
            for _ in 0..60 {
                if let Ok(f) = std::fs::OpenOptions::new().read(true).write(true).open(&persist_pipe_thread) { pipe = Some(f); break; }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            let Some(pipe) = pipe else { std::thread::sleep(std::time::Duration::from_secs(2)); continue; };
            eprintln!("persistent helper: connected to daemon pipe {}", persist_pipe_thread);
            let writer = match pipe.try_clone() { Ok(w) => std::sync::Arc::new(std::sync::Mutex::new(w)), Err(_) => { std::thread::sleep(std::time::Duration::from_secs(1)); continue; } };
            let mut reader = std::io::BufReader::new(pipe);
            // heartbeat thread
            let hb_writer = std::sync::Arc::clone(&writer);
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(15));
                    let Ok(mut w) = hb_writer.lock() else { return; };
                    if writeln!(*w, "{{\"heartbeat\":{}}}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())).is_err() { return; }
                    if w.flush().is_err() { return; }
                }
            });
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {},
                    Err(_) => break,
                }
                if line.trim().is_empty() { continue; }
                if line.contains("heartbeat") { continue; }
                let job = match clipmonger_backend_windows::helper::decode_job(&line) {
                    Ok(j) => j,
                    Err(e) => { let resp = serde_json::json!({"job_id": 0, "ok": false, "error": format!("refused: {}", e)}).to_string() + "\n"; if let Ok(mut w) = writer.lock() { let _ = w.write_all(resp.as_bytes()); let _ = w.flush(); } continue; }
                };
                let outcome = (|| {
                    let pinned = clipmonger_backend_windows::helper::pin_helper_target(job.target_pid)?;
                    match job.op {
                        clipmonger_backend_windows::helper::JobOp::SendInputBatch => clipmonger_backend_windows::paste::worker_send_input_batch(&job.payload).map_err(|d| format!("inject failed: {}", d))?,
                        clipmonger_backend_windows::helper::JobOp::PasteChord => clipmonger_backend_windows::paste::worker_press_paste_chord().map_err(|d| format!("inject failed: {}", d))?,
                    }
                    let _ = pinned;
                    Ok::<(), String>(())
                })();
                let resp = match outcome {
                    Ok(()) => serde_json::json!({"job_id": job.job_id, "ok": true}).to_string() + "\n",
                    Err(e) => serde_json::json!({"job_id": job.job_id, "ok": false, "error": e}).to_string() + "\n",
                };
                let mut w = match writer.lock() { Ok(w) => w, Err(_) => break };
                if w.write_all(resp.as_bytes()).is_err() { break; }
                if w.flush().is_err() { break; }
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
    let mut msg = std::mem::MaybeUninit::<MSG>::uninit();
    unsafe { while GetMessageW(msg.as_mut_ptr(), std::ptr::null_mut(), 0, 0) != 0 {} }
    unsafe { UnhookWindowsHookEx(hook) };
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
            match job.op {
                clipmonger_backend_windows::helper::JobOp::SendInputBatch => {
                    clipmonger_backend_windows::paste::worker_send_input_batch(&job.payload)
                        .map_err(|detail| format!("inject failed: {detail}"))?;
                }
                clipmonger_backend_windows::helper::JobOp::PasteChord => {
                    clipmonger_backend_windows::paste::worker_press_paste_chord()
                        .map_err(|detail| format!("inject failed: {detail}"))?;
                }
            }
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
#[allow(clippy::too_many_lines)]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(all(windows, feature = "backend-windows"))]
    if args.iter().any(|a| a == "__elevated-hotkey-helper") {
        let profile = profile_arg(&args);
        std::process::exit(run_elevated_hotkey_helper(&profile));
    }
    #[cfg(all(windows, feature = "backend-windows"))]
    if args.iter().any(|a| a == "__elevated-persistent") {
        let profile = profile_arg(&args);
        std::process::exit(run_elevated_persistent(&profile));
    }
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
    state.set_config(config.clone());
    state.set_paste_limits(config.popup.unicode_limit, config.popup.delivery_dwell_ms);
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

    #[cfg(unix)]
    if let Err(e) = clipmonger_ipc::server::Server::bind_with_lifecycle(
        &socket,
        token,
        handler,
        Some(lifecycle.clone()),
    )
    .await
    {
        eprintln!("daemon: bind failed: {e}");
        std::process::exit(1);
    }
    #[cfg(windows)]
    if let Err(e) = clipmonger_ipc::server::Server::bind_pipe_with_lifecycle(
        &socket.to_string_lossy(),
        token,
        handler,
        Some(lifecycle),
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
        _ = tokio::signal::ctrl_c() => {}
    }
    let _ = fs::remove_file(profile.join("rendezvous.json"));
    #[cfg(unix)]
    let _ = fs::remove_file(&socket);
}

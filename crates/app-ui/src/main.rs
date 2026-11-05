//! Launcher shell: ensure the daemon, own the IPC link, run the popup.
// Tray app: no console window on Windows (closing one would kill the app).
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::any::Any;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clipmonger_app_ui::runner;
#[cfg(any(windows, target_os = "macos"))]
use clipmonger_app_ui::tray_logic;
use clipmonger_app_ui::tray_logic::TrayAction;
use clipmonger_app_ui::{app, foreground, ipc_bridge, lifecycle, placement, push, strings};
#[cfg(target_os = "linux")]
use clipmonger_app_ui::{linux_tray, tray_logic};
use clipmonger_core::config::Config;
use clipmonger_ipc::client::Client;
use clipmonger_ipc::proto::IpcError;
use serde_json::Value;

/// Daemon spawn wait: tries times interval.
const SPAWN_TRIES: u32 = 50;
/// Interval between spawn-wait tries.
const SPAWN_WAIT: Duration = Duration::from_millis(100);
/// Least time between reconnect attempts while the daemon is away.
const RECONNECT_EVERY: Duration = Duration::from_secs(2);

/// Bridge transport to the daemon. Without a link (daemon not up yet, or
/// gone) ordinary calls reconnect, starting the daemon if needed, at most
/// every [`RECONNECT_EVERY`]. An offline Stop returns unavailable without
/// connecting or starting a daemon. A failed call is never retried, so a
/// paste can never run twice.
struct DaemonTransport {
    profile: PathBuf,
    client: Option<Client>,
    last_attempt: Option<Instant>,
}

impl DaemonTransport {
    fn new(profile: PathBuf, client: Option<Client>) -> Self {
        Self {
            profile,
            client,
            last_attempt: Option::None,
        }
    }
}

fn is_stop_method(method: &str) -> bool {
    method == "daemon.stop"
}

impl DaemonTransport {
    fn call_inner<C>(
        client: &mut Option<C>,
        last_attempt: &mut Option<Instant>,
        method: &str,
        params: &Value,
        mut try_connect: impl FnMut() -> Option<C>,
        mut spawn_daemon: impl FnMut() -> bool,
        mut do_call: impl FnMut(&C, &str, Value) -> Result<Value, IpcError>,
    ) -> Result<Value, IpcError> {
        if client.is_none()
            && !is_stop_method(method)
            && last_attempt.is_none_or(|at| at.elapsed() >= RECONNECT_EVERY)
        {
            *last_attempt = Some(Instant::now());
            *client = try_connect().or_else(|| {
                if spawn_daemon() {
                    try_connect()
                } else {
                    Option::None
                }
            });
        }
        let Some(target) = client.as_ref() else {
            return Err(IpcError {
                code: "unavailable".to_string(),
                message: "daemon not running".to_string(),
                retryable: true,
                backend: Option::None,
            });
        };
        let outcome = do_call(target, method, params.clone());
        if outcome
            .as_ref()
            .is_err_and(|e| matches!(e.code.as_str(), "io_failed" | "connection_closed"))
        {
            *client = Option::None;
        }
        outcome
    }
}

impl ipc_bridge::Transport for DaemonTransport {
    fn call(&mut self, method: &str, params: &Value) -> Result<Value, IpcError> {
        DaemonTransport::call_inner(
            &mut self.client,
            &mut self.last_attempt,
            method,
            params,
            || try_connect(&self.profile),
            || spawn_daemon(&self.profile),
            clipmonger_ipc::client::Client::call,
        )
    }
}

/// Listen for daemon pushes (popup requests, history changes) on their own
/// connection, and start the daemon again when it crashed.
fn start_push_listener(
    profile: PathBuf,
    extra: Value,
    events: mpsc::Sender<ipc_bridge::UiEvent>,
    wake: push::Wake,
) {
    let restart_profile = profile.clone();
    push::spawn_listener(
        move || try_connect(&profile),
        move || {
            if lifecycle::daemon_crashed(&restart_profile) {
                let _ = spawn_daemon(&restart_profile);
            }
        },
        extra,
        events,
        wake,
    );
}

/// Another UI holds the profile: have the daemon show that UI's popup.
fn hand_over_to_running_ui(profile: &Path) {
    // Started by the user, this process may pass on the foreground right,
    // so the running popup can come to the front.
    foreground::allow_any();
    if let Some(client) = try_connect(profile) {
        let _ = client.call("popup.open", serde_json::json!({}));
    }
}

/// Try one IPC connect for the profile.
fn try_connect(profile: &Path) -> Option<Client> {
    let connection = clipmonger_app_ui::profile_connection::resolve(profile).ok()?;
    Client::connect(&connection.endpoint, &connection.token).ok()
}

/// Daemon binary next to the running shell binary.
fn sibling_daemon() -> PathBuf {
    let mut dir = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("clipmonger"));
    dir.pop();
    dir.join(format!("clipmonger-daemon{}", std::env::consts::EXE_SUFFIX))
}

/// Spawn the sibling daemon and wait for its rendezvous file. A thread waits
/// on the child so a daemon that later exits is reaped, not left defunct.
fn spawn_daemon(profile: &Path) -> bool {
    let outcome = std::process::Command::new(sibling_daemon())
        .arg("--profile-dir")
        .arg(profile)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = outcome else {
        return false;
    };
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    for _ in 0..SPAWN_TRIES {
        if profile.join("rendezvous.json").exists() {
            return true;
        }
        std::thread::sleep(SPAWN_WAIT);
    }
    false
}

/// Shell args: `--profile-dir <dir>`, and `--minimized` or `--popup` to start
/// with the popup hidden (the daemon holds a popup request for the UI it
/// starts). Returns the profile dir and whether to start hidden.
fn parse_args() -> (Option<PathBuf>, bool) {
    let mut profile: Option<PathBuf> = Option::None;
    let mut hidden = false;
    let mut args = std::env::args_os().skip(1).peekable();
    while let Some(arg) = args.next() {
        if arg == "--profile-dir" {
            profile = args.next().map(PathBuf::from);
        } else if arg == "--minimized" || arg == "--popup" {
            hidden = true;
        }
    }
    (profile, hidden)
}

/// Default profile dir: beside the binary in portable mode (a
/// `portable.marker` file next to it, seg-03 S6), else the OS data dir.
fn default_profile() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
    let dir = exe
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let portable = dir.join("portable.marker").is_file();
    Config::resolve_paths(portable, &dir).data_dir
}

/// What the platform tray gives the app.
struct TrayParts {
    /// Shows the current state (pause/resume label) and drains the actions
    /// clicked since the last call.
    poll: Box<dyn FnMut(tray_logic::TrayInputs) -> Vec<TrayAction>>,
    /// Kept alive for the life of the app.
    hold: Option<Box<dyn Any>>,
}

/// Real OS tray (Windows/macOS via tray-icon); left click toggles the popup, a
/// menu click or a left click wakes the runner. Creation failure is degraded,
/// never fatal.
#[cfg(any(windows, target_os = "macos"))]
fn create_tray(wake: push::Wake, online: bool) -> TrayParts {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
    let inputs = tray_logic::TrayInputs {
        paused: false,
        degraded: false,
        attached: online,
    };
    let (tx, rx) = mpsc::channel::<TrayAction>();
    let menu = Menu::new();
    let mut ids = std::collections::HashMap::new();
    let mut pause_item = None;
    for entry in tray_logic::menu_entries(&inputs) {
        match entry {
            tray_logic::MenuEntry::Separator => {
                let _ = menu.append(&PredefinedMenuItem::separator());
            }
            tray_logic::MenuEntry::Item(id) => {
                let item = MenuItem::new(strings::text(id), true, None);
                ids.insert(
                    item.id().0.clone(),
                    clipmonger_app_ui::linux_tray::action_for_id(id),
                );
                let _ = menu.append(&item);
                if id == "tray.pause" || id == "tray.resume" {
                    pause_item = Some(item);
                }
            }
        }
    }
    let click_tx = tx.clone();
    let click_wake = wake.clone();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        let button_event = match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Down,
                ..
            } => Some(tray_logic::LeftButtonEvent::Down),
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } => Some(tray_logic::LeftButtonEvent::Up),
            TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => Some(tray_logic::LeftButtonEvent::DoubleClick),
            _ => None,
        };
        if let Some(action) =
            button_event.and_then(|event| tray_logic::left_button_action(event, Instant::now()))
        {
            let _ = click_tx.send(action);
            click_wake();
        }
    }));
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if let Some(action) = ids.get(&event.id().0) {
            let _ = tx.send(action.clone());
            wake();
        }
    }));
    let icon = tray_icon::Icon::from_rgba([0x33_u8, 0x99, 0xcc, 0xff].repeat(16 * 16), 16, 16);
    let built = icon.ok().and_then(|icon| {
        tray_icon::TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_icon(icon)
            .with_tooltip(strings::text("tray.tooltip"))
            .build()
            .ok()
    });
    if let Some(tray) = built {
        TrayParts {
            poll: windows_tray_poll(inputs, pause_item, rx),
            hold: Some(Box::new(tray)),
        }
    } else {
        eprintln!("tray unavailable: degraded, window still opens");
        let _ = clipmonger_core::runtime_log::global_emit(
            clipmonger_core::runtime_log::Role::Ui,
            clipmonger_core::runtime_log::Severity::Warn,
            "tray.unavailable",
            &[("class", "tray_build_failed")],
        );
        TrayParts {
            poll: Box::new(|_| Vec::new()),
            hold: Option::None,
        }
    }
}

/// Tray poll for the tray-icon menu: relabel the pause item when the
/// paused state changes, then drain the clicked actions.
#[cfg(any(windows, target_os = "macos"))]
fn windows_tray_poll(
    mut shown: tray_logic::TrayInputs,
    pause_item: Option<tray_icon::menu::MenuItem>,
    rx: mpsc::Receiver<TrayAction>,
) -> Box<dyn FnMut(tray_logic::TrayInputs) -> Vec<TrayAction>> {
    Box::new(move |now| {
        if now.paused != shown.paused {
            if let Some(item) = &pause_item {
                item.set_text(strings::text(if now.paused {
                    "tray.resume"
                } else {
                    "tray.pause"
                }));
            }
        }
        shown = now;
        rx.try_iter().collect()
    })
}

/// Linux tray over ksni; a click wakes the runner.
#[cfg(target_os = "linux")]
fn create_tray(wake: push::Wake, online: bool) -> TrayParts {
    let inputs = tray_logic::TrayInputs {
        paused: false,
        degraded: false,
        attached: online,
    };
    let mut tray = linux_tray::LinuxTray::spawn(&inputs, wake);
    TrayParts {
        poll: Box::new(move |now| {
            tray.show(now);
            tray.poll()
        }),
        hold: Option::None,
    }
}

/// No tray on other platforms.
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn create_tray(wake: push::Wake, online: bool) -> TrayParts {
    let _ = (wake, online);
    TrayParts {
        poll: Box::new(|_| Vec::new()),
        hold: Option::None,
    }
}

/// Build the app once the runner is up: the bridge (its replies wake the
/// runner), the push listener (registering the popup window on Windows), the
/// tray and the application.
fn build_app(
    startup: &runner::Startup,
    profile: PathBuf,
    transport: Box<dyn ipc_bridge::Transport>,
    online: bool,
    open: bool,
) -> app::PopupApp {
    let wake = startup.handle.waker();
    let (cmds, push_events, events) =
        ipc_bridge::Bridge::spawn_shared_with_wake(transport, wake.clone());
    let extra = serde_json::json!({ "popup_hwnd": startup.popup_hwnd, "popup_x11_window": startup.popup_x11_window });
    start_push_listener(profile.clone(), extra, push_events, wake.clone());
    let worker =
        clipmonger_app_ui::thumbnail_worker::Worker::start(profile.clone(), wake.clone()).ok();
    let tray = create_tray(wake, online);
    let mut app = app::PopupApp::new(app::AppParts {
        profile,
        cmds,
        events,
        tray: tray.poll,
        hold: tray.hold,
        open,
        status_period: app::STATUS_PERIOD,
        trace: app::Trace::from_env(),
        allow_foreground: Box::new(foreground::allow_any),
    });
    if let Some(worker) = worker {
        app.attach_thumbnails(worker);
    }
    app
}

#[allow(clippy::cast_precision_loss)]
fn main() -> ExitCode {
    let (profile_arg, hidden) = parse_args();
    let profile = profile_arg.unwrap_or_else(default_profile);
    // One UI per profile; a second launch shows the running UI's popup.
    let _instance = match lifecycle::claim_single_instance(&profile) {
        lifecycle::InstanceClaim::Primary(lock) => lock,
        lifecycle::InstanceClaim::Secondary => {
            hand_over_to_running_ui(&profile);
            return ExitCode::SUCCESS;
        }
    };
    if let Err(e) =
        clipmonger_core::runtime_log::init_global(&profile, clipmonger_core::runtime_log::Role::Ui)
    {
        eprintln!("ui: runtime log init failed: {e}");
    }
    let client = try_connect(&profile).or_else(|| {
        if spawn_daemon(&profile) {
            try_connect(&profile)
        } else {
            Option::None
        }
    });
    let online = lifecycle::start_mode(client.is_some()) == lifecycle::StartMode::Attached;
    let transport: Box<dyn ipc_bridge::Transport> =
        Box::new(DaemonTransport::new(profile.clone(), client));
    let (popup_w, popup_h) = placement::window_size_for(false);
    let (settings_w, settings_h) = placement::window_size_for(true);
    let config = runner::Config {
        popup_title: "clipmonger".to_string(),
        popup_size: [popup_w as f32, popup_h as f32],
        popup_position: Option::None,
        settings_title: strings::text("config.title").to_string(),
        settings_size: [settings_w as f32, settings_h as f32],
        settings_state_path: Some(profile.join("ui-window-state.json")),
    };
    let result = runner::run(config, move |startup| {
        build_app(&startup, profile, transport, online, !hidden)
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = clipmonger_core::runtime_log::global_emit(
                clipmonger_core::runtime_log::Role::Ui,
                clipmonger_core::runtime_log::Severity::Error,
                "ui.shell_failed",
                &[("class", error.runtime_class())],
            );
            eprintln!("shell failed: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod daemon_transport_tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct FakeClient(u32);

    fn io_failed() -> IpcError {
        IpcError {
            code: "io_failed".to_string(),
            message: "io".to_string(),
            retryable: false,
            backend: None,
        }
    }

    fn connection_closed() -> IpcError {
        IpcError {
            code: "connection_closed".to_string(),
            message: "closed".to_string(),
            retryable: false,
            backend: None,
        }
    }

    fn server_error() -> IpcError {
        IpcError {
            code: "internal".to_string(),
            message: "boom".to_string(),
            retryable: false,
            backend: None,
        }
    }

    #[test]
    fn offline_stop_zero_attempts_unavailable() {
        let mut client: Option<FakeClient> = None;
        let mut last: Option<Instant> = None;
        let connect_calls = Rc::new(Cell::new(0));
        let spawn_calls = Rc::new(Cell::new(0));
        let cc = connect_calls.clone();
        let sc = spawn_calls.clone();
        let result = DaemonTransport::call_inner(
            &mut client,
            &mut last,
            "daemon.stop",
            &serde_json::json!({}),
            move || {
                cc.set(cc.get() + 1);
                None
            },
            move || {
                sc.set(sc.get() + 1);
                true
            },
            |_, _, _| panic!("should not call"),
        );
        let err = result.unwrap_err();
        assert_eq!(err.code, "unavailable");
        assert_eq!(err.message, "daemon not running");
        assert!(err.retryable);
        assert!(err.backend.is_none());
        assert_eq!(connect_calls.get(), 0);
        assert_eq!(spawn_calls.get(), 0);
        assert!(client.is_none());
        assert!(last.is_none());
        // A second offline Stop also leaves the daemon unstarted.
        let mut client2: Option<FakeClient> = None;
        let mut last2: Option<Instant> = None;
        let sc2 = Rc::new(Cell::new(0));
        let sc2c = sc2.clone();
        let err2 = DaemonTransport::call_inner(
            &mut client2,
            &mut last2,
            "daemon.stop",
            &serde_json::json!({}),
            || None,
            move || {
                sc2c.set(sc2c.get() + 1);
                true
            },
            |_, _, _| Ok(serde_json::json!({})),
        )
        .unwrap_err();
        assert_eq!(err2.code, "unavailable");
        assert_eq!(sc2.get(), 0);
        assert!(last2.is_none());
    }

    #[test]
    fn offline_stop_preserves_recent_ordinary_throttle() {
        let mut client: Option<FakeClient> = None;
        let mut last = Some(Instant::now());
        let original_last = last;
        let connect_calls = Rc::new(Cell::new(0));
        let spawn_calls = Rc::new(Cell::new(0));
        let cc = connect_calls.clone();
        let sc = spawn_calls.clone();
        let result = DaemonTransport::call_inner(
            &mut client,
            &mut last,
            "history.list",
            &serde_json::json!({}),
            move || {
                cc.set(cc.get() + 1);
                Some(FakeClient(1))
            },
            move || {
                sc.set(sc.get() + 1);
                true
            },
            |_, _, _| Ok(serde_json::json!({"ok": true})),
        );
        assert_eq!(result.unwrap_err().code, "unavailable");
        assert_eq!(connect_calls.get(), 0);
        assert_eq!(spawn_calls.get(), 0);
        assert!(client.is_none());
        let cc2 = connect_calls.clone();
        let sc2 = spawn_calls.clone();
        // Stop under the same throttle makes no attempt and leaves it unchanged.
        let mut call_seen = false;
        let result2 = DaemonTransport::call_inner(
            &mut client,
            &mut last,
            "daemon.stop",
            &serde_json::json!({}),
            move || {
                cc2.set(cc2.get() + 1);
                Some(FakeClient(99))
            },
            move || {
                sc2.set(sc2.get() + 1);
                true
            },
            |c, m, _| {
                assert_eq!(c, &FakeClient(99));
                assert_eq!(m, "daemon.stop");
                call_seen = true;
                Ok(serde_json::json!({"stopped": true}))
            },
        );
        assert_eq!(result2.unwrap_err().code, "unavailable");
        assert!(!call_seen);
        assert_eq!(connect_calls.get(), 0);
        assert_eq!(spawn_calls.get(), 0);
        assert!(client.is_none());
        assert_eq!(last, original_last);
    }

    #[test]
    fn offline_ordinary_connect_spawn_reconnect_once_and_throttle() {
        let mut client: Option<FakeClient> = None;
        let mut last: Option<Instant> = None;
        let connect_calls = Rc::new(Cell::new(0));
        let spawn_calls = Rc::new(Cell::new(0));
        let cc = connect_calls.clone();
        let sc = spawn_calls.clone();
        let mut call_count = 0;
        let result = DaemonTransport::call_inner(
            &mut client,
            &mut last,
            "history.list",
            &serde_json::json!({}),
            {
                let cc = cc.clone();
                move || {
                    let n = cc.get();
                    cc.set(n + 1);
                    if n == 0 {
                        None
                    } else {
                        Some(FakeClient(2))
                    }
                }
            },
            move || {
                sc.set(sc.get() + 1);
                true
            },
            |c, m, _| {
                call_count += 1;
                assert_eq!(c, &FakeClient(2));
                assert_eq!(m, "history.list");
                Ok(serde_json::json!({"ok": true}))
            },
        );
        assert!(result.is_ok());
        assert_eq!(call_count, 1);
        assert_eq!(connect_calls.get(), 2);
        assert_eq!(spawn_calls.get(), 1);
        assert!(client.is_some());
        assert!(last.is_some());
        let mut client2: Option<FakeClient> = None;
        let mut last2 = Some(Instant::now());
        let cc2 = Rc::new(Cell::new(0));
        let sc2 = Rc::new(Cell::new(0));
        let cc2c = cc2.clone();
        let sc2c = sc2.clone();
        let result2 = DaemonTransport::call_inner(
            &mut client2,
            &mut last2,
            "history.list",
            &serde_json::json!({}),
            move || {
                cc2c.set(cc2c.get() + 1);
                Some(FakeClient(3))
            },
            move || {
                sc2c.set(sc2c.get() + 1);
                true
            },
            |_, _, _| Ok(serde_json::json!({})),
        );
        assert!(result2.is_err());
        assert_eq!(result2.unwrap_err().code, "unavailable");
        assert_eq!(cc2.get(), 0);
        assert_eq!(sc2.get(), 0);
    }

    #[test]
    fn connected_stop_forwarded_once() {
        let mut client = Some(FakeClient(10));
        let mut last: Option<Instant> = None;
        let connect_calls = Rc::new(Cell::new(0));
        let spawn_calls = Rc::new(Cell::new(0));
        let cc = connect_calls.clone();
        let sc = spawn_calls.clone();
        let mut call_count = 0;
        let mut seen_method = String::new();
        let result = DaemonTransport::call_inner(
            &mut client,
            &mut last,
            "daemon.stop",
            &serde_json::json!({}),
            move || {
                cc.set(cc.get() + 1);
                Some(FakeClient(11))
            },
            move || {
                sc.set(sc.get() + 1);
                true
            },
            |c, m, p| {
                call_count += 1;
                seen_method = m.to_string();
                assert_eq!(c, &FakeClient(10));
                assert_eq!(p, serde_json::json!({}));
                Ok(serde_json::json!({"ok": true}))
            },
        );
        assert!(result.is_ok());
        assert_eq!(call_count, 1);
        assert_eq!(seen_method, "daemon.stop");
        assert_eq!(connect_calls.get(), 0);
        assert_eq!(spawn_calls.get(), 0);
        assert!(client.is_some());
    }

    #[test]
    fn stale_stop_io_failed_clears_link_no_retry() {
        let mut client = Some(FakeClient(20));
        let mut last: Option<Instant> = None;
        let connect_calls = Rc::new(Cell::new(0));
        let spawn_calls = Rc::new(Cell::new(0));
        let cc = connect_calls.clone();
        let sc = spawn_calls.clone();
        let result = DaemonTransport::call_inner(
            &mut client,
            &mut last,
            "daemon.stop",
            &serde_json::json!({}),
            move || {
                cc.set(cc.get() + 1);
                Some(FakeClient(21))
            },
            move || {
                sc.set(sc.get() + 1);
                true
            },
            |_, _, _| Err(io_failed()),
        );
        assert_eq!(result.unwrap_err().code, "io_failed");
        assert!(client.is_none());
        assert_eq!(connect_calls.get(), 0);
        assert_eq!(spawn_calls.get(), 0);
        let mut client2 = Some(FakeClient(20));
        let mut last2: Option<Instant> = None;
        let cc2 = Rc::new(Cell::new(0));
        let cc2c = cc2.clone();
        let result2 = DaemonTransport::call_inner(
            &mut client2,
            &mut last2,
            "daemon.stop",
            &serde_json::json!({}),
            move || {
                cc2c.set(cc2c.get() + 1);
                None
            },
            || true,
            |_, _, _| Err(connection_closed()),
        );
        assert_eq!(result2.unwrap_err().code, "connection_closed");
        assert!(client2.is_none());
        assert_eq!(cc2.get(), 0);
    }

    #[test]
    fn stale_stop_connection_closed_clears_link_no_retry() {
        let mut client = Some(FakeClient(30));
        let mut last: Option<Instant> = None;
        let result = DaemonTransport::call_inner(
            &mut client,
            &mut last,
            "daemon.stop",
            &serde_json::json!({}),
            || Some(FakeClient(31)),
            || true,
            |_, _, _| Err(connection_closed()),
        );
        assert_eq!(result.unwrap_err().code, "connection_closed");
        assert!(client.is_none());
    }

    #[test]
    fn non_io_server_error_retains_link() {
        let mut client = Some(FakeClient(40));
        let mut last: Option<Instant> = None;
        let connect_calls = Rc::new(Cell::new(0));
        let cc = connect_calls.clone();
        let result = DaemonTransport::call_inner(
            &mut client,
            &mut last,
            "daemon.stop",
            &serde_json::json!({}),
            move || {
                cc.set(cc.get() + 1);
                Some(FakeClient(41))
            },
            || true,
            |_, _, _| Err(server_error()),
        );
        assert_eq!(result.unwrap_err().code, "internal");
        assert!(client.is_some());
        assert_eq!(client, Some(FakeClient(40)));
        assert_eq!(connect_calls.get(), 0);
        let mut client2 = Some(FakeClient(50));
        let mut last2: Option<Instant> = None;
        let result2 = DaemonTransport::call_inner(
            &mut client2,
            &mut last2,
            "history.list",
            &serde_json::json!({}),
            || Some(FakeClient(51)),
            || true,
            |_, _, _| Err(server_error()),
        );
        assert_eq!(result2.unwrap_err().code, "internal");
        assert!(client2.is_some());
    }

    #[test]
    fn is_stop_method_exact_only() {
        assert!(is_stop_method("daemon.stop"));
        assert!(!is_stop_method("daemon.stop.extra"));
        assert!(!is_stop_method("daemon.stopping"));
        assert!(!is_stop_method("history.list"));
        assert!(!is_stop_method(""));
    }

    #[test]
    fn offline_stop_order_no_attempt() {
        let mut client: Option<FakeClient> = None;
        let mut last: Option<Instant> = None;
        let order = Rc::new(Cell::new(Vec::<String>::new()));
        let order_c = order.clone();
        let order_s = order.clone();
        let cc = Rc::new(Cell::new(0));
        let cc_c = cc.clone();
        let sc = Rc::new(Cell::new(0));
        let sc_c = sc.clone();
        let result = DaemonTransport::call_inner(
            &mut client,
            &mut last,
            "daemon.stop",
            &serde_json::json!({}),
            move || {
                let mut v = order_c.take();
                v.push("connect".to_string());
                order_c.set(v);
                cc_c.set(cc_c.get() + 1);
                None
            },
            move || {
                let mut v = order_s.take();
                v.push("spawn".to_string());
                order_s.set(v);
                sc_c.set(sc_c.get() + 1);
                true
            },
            |_, _, _| Ok(serde_json::json!({})),
        );
        assert_eq!(result.unwrap_err().code, "unavailable");
        assert_eq!(cc.get(), 0);
        assert_eq!(sc.get(), 0);
        assert_eq!(order.take(), [] as [String; 0]);
        assert!(last.is_none());
    }

    #[test]
    fn offline_ordinary_order_connect_spawn_connect() {
        let mut client: Option<FakeClient> = None;
        let mut last: Option<Instant> = None;
        let order = Rc::new(Cell::new(Vec::<String>::new()));
        let order_c = order.clone();
        let order_s = order.clone();
        let cc = Rc::new(Cell::new(0));
        let cc_c = cc.clone();
        let sc = Rc::new(Cell::new(0));
        let sc_c = sc.clone();
        let result = DaemonTransport::call_inner(
            &mut client,
            &mut last,
            "history.list",
            &serde_json::json!({}),
            move || {
                let mut v = order_c.take();
                v.push("connect".to_string());
                order_c.set(v);
                let n = cc_c.get();
                cc_c.set(n + 1);
                if n == 0 {
                    None
                } else {
                    Some(FakeClient(99))
                }
            },
            move || {
                let mut v = order_s.take();
                v.push("spawn".to_string());
                order_s.set(v);
                sc_c.set(sc_c.get() + 1);
                true
            },
            |c, _, _| Ok(serde_json::json!({"ok": c.0})),
        );
        assert!(result.is_ok());
        assert_eq!(cc.get(), 2);
        assert_eq!(sc.get(), 1);
        assert_eq!(order.take(), vec!["connect", "spawn", "connect"]);
    }
}

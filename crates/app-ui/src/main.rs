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
use clipmonger_ipc::framing;
use clipmonger_ipc::proto::IpcError;
use serde_json::Value;

#[cfg(all(windows, feature = "software"))]
use clipmonger_app_ui::app::ConfigWindowState;
#[cfg(all(windows, feature = "software"))]
use clipmonger_app_ui::{config_model, popup};
#[cfg(all(windows, feature = "software"))]
mod fallback;
#[cfg(all(windows, feature = "software"))]
mod fallback_config;

/// Daemon spawn wait: tries times interval.
const SPAWN_TRIES: u32 = 50;
/// Interval between spawn-wait tries.
const SPAWN_WAIT: Duration = Duration::from_millis(100);
/// Least time between reconnect attempts while the daemon is away.
const RECONNECT_EVERY: Duration = Duration::from_secs(2);

/// Bridge transport to the daemon. Without a link (daemon not up yet, or
/// gone) a call first reconnects, starting the daemon if needed, at most
/// every [`RECONNECT_EVERY`]. A failed call is never retried, so a paste can
/// never run twice; while offline every call fails as unavailable and the
/// window stays open.
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

impl ipc_bridge::Transport for DaemonTransport {
    fn call(&mut self, method: &str, params: &Value) -> Result<Value, IpcError> {
        if self.client.is_none()
            && self
                .last_attempt
                .is_none_or(|at| at.elapsed() >= RECONNECT_EVERY)
        {
            self.last_attempt = Some(Instant::now());
            self.client = try_connect(&self.profile).or_else(|| {
                if spawn_daemon(&self.profile) {
                    try_connect(&self.profile)
                } else {
                    Option::None
                }
            });
        }
        let Some(client) = &self.client else {
            return Err(IpcError {
                code: "unavailable".to_string(),
                message: "daemon not running".to_string(),
                retryable: true,
                backend: Option::None,
            });
        };
        let outcome = client.call(method, params.clone());
        if outcome
            .as_ref()
            .is_err_and(|e| matches!(e.code.as_str(), "io_failed" | "connection_closed"))
        {
            self.client = Option::None;
        }
        outcome
    }
}

/// Listen for daemon pushes (popup requests) on their own connection.
fn start_push_listener(
    profile: PathBuf,
    extra: Value,
    events: mpsc::Sender<ipc_bridge::UiEvent>,
    wake: push::Wake,
) {
    push::spawn_listener(move || try_connect(&profile), extra, events, wake);
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

/// Read the profile token file.
fn read_token(profile: &Path) -> Option<String> {
    let text = std::fs::read_to_string(profile.join("token")).ok()?;
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        Option::None
    } else {
        Some(trimmed)
    }
}

/// Resolve the daemon socket from rendezvous or the path helper.
fn socket_for(profile: &Path) -> PathBuf {
    if let Ok(text) = std::fs::read_to_string(profile.join("rendezvous.json")) {
        if let Ok(body) = serde_json::from_str::<Value>(&text) {
            if let Some(socket) = body.get("socket").and_then(Value::as_str) {
                return PathBuf::from(socket);
            }
        }
    }
    let runtime =
        std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    framing::socket_path_for(profile, &runtime)
}

/// Try one IPC connect for the profile.
fn try_connect(profile: &Path) -> Option<Client> {
    let socket = socket_for(profile);
    let token = read_token(profile)?;
    Client::connect(&socket, &token).ok()
}

/// Daemon binary next to the running shell binary.
fn sibling_daemon() -> PathBuf {
    let mut dir = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("clipmonger"));
    dir.pop();
    dir.join(format!("clipmonger-daemon{}", std::env::consts::EXE_SUFFIX))
}

/// Spawn the sibling daemon and wait for its rendezvous file.
fn spawn_daemon(profile: &Path) -> bool {
    let outcome = std::process::Command::new(sibling_daemon())
        .arg("--profile-dir")
        .arg(profile)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if outcome.is_err() {
        return false;
    }
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
    /// Drains the actions clicked since the last call.
    poll: Box<dyn FnMut() -> Vec<TrayAction>>,
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
            }
        }
    }
    let click_tx = tx.clone();
    let click_wake = wake.clone();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let _ = click_tx.send(TrayAction::Toggle);
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
            poll: Box::new(move || rx.try_iter().collect()),
            hold: Some(Box::new(tray)),
        }
    } else {
        eprintln!("tray unavailable: degraded, window still opens");
        TrayParts {
            poll: Box::new(Vec::new),
            hold: Option::None,
        }
    }
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
        poll: Box::new(move || tray.poll()),
        hold: Option::None,
    }
}

/// No tray on other platforms.
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn create_tray(wake: push::Wake, online: bool) -> TrayParts {
    let _ = (wake, online);
    TrayParts {
        poll: Box::new(Vec::new),
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
    let extra = startup.popup_hwnd.map_or_else(
        || serde_json::json!({}),
        |hwnd| serde_json::json!({ "popup_hwnd": hwnd }),
    );
    start_push_listener(profile.clone(), extra, push_events, wake.clone());
    let tray = create_tray(wake, online);
    app::PopupApp::new(app::AppParts {
        profile,
        cmds,
        events,
        tray: tray.poll,
        hold: tray.hold,
        open,
        status_period: app::STATUS_PERIOD,
        trace: app::Trace::from_env(),
        allow_foreground: Box::new(foreground::allow_any),
    })
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
    };
    let result = runner::run(config, move |startup| {
        build_app(&startup, profile, transport, online, !hidden)
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("shell failed: {error}");
            ExitCode::from(1)
        }
    }
}

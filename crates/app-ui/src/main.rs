//! Launcher shell: ensure the daemon, own the IPC link, render the popup.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clipmonger_app_ui::{ipc_bridge, keymap, placement, popup, strings, tray_logic};
use clipmonger_core::config::Config;
use clipmonger_ipc::client::Client;
use clipmonger_ipc::framing;
use clipmonger_ipc::proto::IpcError;
use clipmonger_platform::fakes::FakeTray;
use serde_json::Value;

/// Status re-poll period.
const STATUS_PERIOD: Duration = Duration::from_secs(30);
/// Daemon spawn wait: tries times interval.
const SPAWN_TRIES: u32 = 50;
/// Interval between spawn-wait tries.
const SPAWN_WAIT: Duration = Duration::from_millis(100);
/// Max bridge events drained per frame.
const DRAIN_CAP: usize = 32;
/// Popup window size.
const POPUP_SIZE: (i32, i32) = (480, 360);

/// Blocking client wrapped as bridge transport.
struct ClientTransport {
    client: Client,
}

impl ipc_bridge::Transport for ClientTransport {
    fn call(&mut self, method: &str, params: &Value) -> Result<Value, IpcError> {
        self.client.call(method, params.clone())
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

/// Shell args: `--profile-dir <dir>` and `--minimized`.
fn parse_args() -> (Option<PathBuf>, bool) {
    let mut profile: Option<PathBuf> = Option::None;
    let mut minimized = false;
    let mut args = std::env::args_os().skip(1).peekable();
    while let Some(arg) = args.next() {
        if arg == "--profile-dir" {
            profile = args.next().map(PathBuf::from);
        } else if arg == "--minimized" {
            minimized = true;
        }
    }
    (profile, minimized)
}

/// Default profile dir from the OS data dir.
fn default_profile() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
    let dir = exe
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    Config::resolve_paths(false, &dir).data_dir
}

/// Popup application state.
struct ShellApp {
    vm: popup::PopupVm,
    cmds: mpsc::Sender<ipc_bridge::UiCmd>,
    events: mpsc::Receiver<ipc_bridge::UiEvent>,
    tray: tray_logic::TrayLogic,
    fake_tray: FakeTray,
    inputs: tray_logic::TrayInputs,
    open: bool,
    error: String,
    last_poll: Instant,
}

impl ShellApp {
    /// Send one bridge command, ignoring a torn-down bridge.
    fn send(&self, cmd: ipc_bridge::UiCmd) {
        let _ = self.cmds.send(cmd);
    }

    /// Drain bridge events into view-model state.
    fn drain_events(&mut self) {
        for _ in 0..DRAIN_CAP {
            let Ok(event) = self.events.try_recv() else {
                break;
            };
            match event {
                ipc_bridge::UiEvent::Status(body) => {
                    self.inputs.attached = true;
                    self.vm.set_offline(false);
                    let paused = body
                        .get("monitoring_paused")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    self.vm.set_paused(paused);
                    self.inputs.paused = paused;
                    self.inputs.degraded = body
                        .get("degraded")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                }
                ipc_bridge::UiEvent::History(body) => {
                    if self.vm.apply_history(&body) {
                        self.inputs.attached = true;
                        self.vm.set_offline(false);
                    } else {
                        self.vm.set_offline(true);
                        self.error = strings::text("app.error").to_string();
                    }
                }
                ipc_bridge::UiEvent::Deleted(id) => self.vm.remove_clip(id),
                ipc_bridge::UiEvent::Pinned(id, pinned) => self.vm.apply_pin(id, pinned),
                ipc_bridge::UiEvent::PasteDone(_) | ipc_bridge::UiEvent::Stopped => {
                    self.open = false;
                }
                ipc_bridge::UiEvent::PasteUnavailable => {
                    self.error = strings::text("popup.paste_blocked").to_string();
                }
                ipc_bridge::UiEvent::Error(message) => {
                    self.error = message;
                }
            }
        }
        self.tray.sync(&mut self.fake_tray, &self.inputs);
    }

    /// Apply one view-model effect to commands and window state.
    fn apply_effect(&mut self, effect: popup::VmEffect) {
        match effect {
            popup::VmEffect::None => {}
            popup::VmEffect::Paste(id) => self.send(ipc_bridge::UiCmd::PasteClip(id)),
            popup::VmEffect::Dismiss => self.open = false,
            popup::VmEffect::Remove(id) => self.send(ipc_bridge::UiCmd::DeleteClip(id)),
            popup::VmEffect::Pin(id) => {
                let pinned = !self.vm.is_pinned(id);
                self.send(ipc_bridge::UiCmd::PinClip(id, pinned));
            }
            popup::VmEffect::Refresh => self.send(ipc_bridge::UiCmd::ListHistory),
        }
    }

    /// Forward special keys to the view-model; typing stays in the field.
    fn handle_keys(&mut self, ctx: &egui::Context, search_focused: bool) {
        let pressed: Vec<(String, bool, bool)> = ctx.input(|input| {
            let mut out = Vec::new();
            let ctrl = input.modifiers.ctrl;
            let alt = input.modifiers.alt;
            let pairs = [
                (egui::Key::Enter, "Enter"),
                (egui::Key::Escape, "Escape"),
                (egui::Key::Delete, "Delete"),
                (egui::Key::Home, "Home"),
                (egui::Key::End, "End"),
                (egui::Key::PageUp, "PageUp"),
                (egui::Key::PageDown, "PageDown"),
                (egui::Key::ArrowUp, "ArrowUp"),
                (egui::Key::ArrowDown, "ArrowDown"),
                (egui::Key::F5, "F5"),
                (egui::Key::P, "p"),
                (egui::Key::E, "e"),
            ];
            for (key, name) in pairs {
                if input.key_pressed(key) {
                    out.push((name.to_string(), ctrl, alt));
                }
            }
            out
        });
        for (name, ctrl, alt) in pressed {
            let editing = search_focused && !ctrl && !alt;
            if editing && name != "Enter" && name != "Escape" && name != "F5" {
                continue;
            }
            let event = keymap::KeyEvent::new(name, ctrl, alt, false);
            let effect = self.vm.on_key(event);
            self.apply_effect(effect);
        }
    }

    /// Render the popup window contents.
    fn render(&mut self, ui: &mut egui::Ui) {
        let mut query = self.vm.query.clone();
        let mut clicked_row: Option<usize> = Option::None;
        let mut search_focused = false;
        egui::CentralPanel::default().show(ui, |ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut query)
                    .hint_text(strings::text("popup.search_hint")),
            );
            search_focused = field.has_focus();
            if let Some(current) = self.vm.current() {
                ui.label(strings::text("popup.current"));
                ui.label(&current.preview);
            }
            let count = self.vm.visible().len();
            if count == 0 {
                ui.label(strings::text("popup.empty"));
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                for row in 0..count {
                    let Some(clip) = self.vm.clip_at(row) else {
                        continue;
                    };
                    if ui
                        .selectable_label(self.vm.selected == row, &clip.preview)
                        .clicked()
                    {
                        clicked_row = Some(row);
                    }
                }
            });
            if !self.vm.banner().is_empty() {
                ui.label(self.vm.banner());
            }
            ui.label(self.vm.status_line());
            if !self.error.is_empty() {
                ui.label(&self.error);
            }
        });
        if query != self.vm.query {
            self.vm.query = query;
            self.vm.selected = 0;
        }
        if let Some(row) = clicked_row {
            if row == self.vm.selected {
                let effect = self.vm.on_key(keymap::KeyEvent::new(
                    "Enter".to_string(),
                    false,
                    false,
                    false,
                ));
                self.apply_effect(effect);
            } else {
                self.vm.selected = row;
            }
        }
        self.handle_keys(ui.ctx(), search_focused);
    }
}

impl eframe::App for ShellApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if ui.ctx().input(|input| input.viewport().close_requested()) {
            self.send(ipc_bridge::UiCmd::Stop);
        }
        self.drain_events();
        if self.last_poll.elapsed() >= STATUS_PERIOD {
            self.last_poll = Instant::now();
            self.send(ipc_bridge::UiCmd::Status);
        }
        if self.open {
            self.render(ui);
        } else {
            let mut reopen = false;
            egui::CentralPanel::default().show(ui, |ui| {
                if ui.button(strings::text("popup.show")).clicked() {
                    reopen = true;
                }
            });
            if reopen {
                self.open = true;
                self.send(ipc_bridge::UiCmd::ListHistory);
            }
        }
        ui.ctx().request_repaint_after(Duration::from_millis(500));
    }
}

#[allow(clippy::cast_precision_loss)]
fn main() -> ExitCode {
    let (profile_arg, minimized) = parse_args();
    let profile = profile_arg.unwrap_or_else(default_profile);
    let client = try_connect(&profile).or_else(|| {
        if spawn_daemon(&profile) {
            try_connect(&profile)
        } else {
            Option::None
        }
    });
    let Some(client) = client else {
        eprintln!("daemon not running");
        return ExitCode::from(1);
    };
    let (cmds, events) = ipc_bridge::Bridge::spawn(Box::new(ClientTransport { client }));
    let _ = cmds.send(ipc_bridge::UiCmd::Status);
    let _ = cmds.send(ipc_bridge::UiCmd::ListHistory);
    let monitor = placement::Rect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1080,
    };
    let (px, py) = placement::place(placement::Placement::Center, (0, 0), &[monitor], POPUP_SIZE);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([POPUP_SIZE.0 as f32, POPUP_SIZE.1 as f32])
            .with_position(egui::pos2(px as f32, py as f32)),
        ..Default::default()
    };
    let app = ShellApp {
        vm: popup::PopupVm::new(),
        cmds,
        events,
        tray: tray_logic::TrayLogic::new(),
        fake_tray: FakeTray::new(Vec::new()),
        inputs: tray_logic::TrayInputs {
            paused: false,
            degraded: false,
            attached: true,
        },
        open: !minimized,
        error: String::new(),
        last_poll: Instant::now(),
    };
    match eframe::run_native("clipmonger", options, Box::new(|_| Ok(Box::new(app)))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("shell failed: {error}");
            ExitCode::from(1)
        }
    }
}

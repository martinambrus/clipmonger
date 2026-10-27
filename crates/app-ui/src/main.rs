//! Launcher shell: ensure the daemon, own the IPC link, render the popup.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clipmonger_app_ui::{
    config_model, ipc_bridge, keymap, lifecycle, linux_tray, permanents, placement, popup, strings,
    tray_logic,
};
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

/// Popup position from the runtime monitor size, if reported yet.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn popup_position_for_monitor(monitor_size: Option<egui::Vec2>) -> Option<egui::Pos2> {
    let size = monitor_size?;
    let monitor = placement::monitor_rect_from_size(Some((size.x as u32, size.y as u32)));
    let (px, py) = placement::place(placement::Placement::Center, (0, 0), &[monitor], POPUP_SIZE);
    Some(egui::pos2(px as f32, py as f32))
}

/// Blocking client wrapped as bridge transport.
struct ClientTransport {
    client: Client,
}

impl ipc_bridge::Transport for ClientTransport {
    fn call(&mut self, method: &str, params: &Value) -> Result<Value, IpcError> {
        self.client.call(method, params.clone())
    }
}

/// Transport used when no daemon is reachable: every call fails as
/// unavailable so the window opens offline instead of exiting.
struct OfflineTransport;

impl ipc_bridge::Transport for OfflineTransport {
    fn call(&mut self, _method: &str, _params: &Value) -> Result<Value, IpcError> {
        Err(IpcError {
            code: "unavailable".to_string(),
            message: "daemon not running".to_string(),
            retryable: true,
            backend: Option::None,
        })
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

/// Real OS tray (Windows/macOS via tray-icon); creation failure is
/// degraded, never fatal.
#[cfg(any(windows, target_os = "macos"))]
struct RealTray {
    _tray: tray_icon::TrayIcon,
    ids: std::collections::HashMap<String, tray_logic::TrayAction>,
}

#[cfg(any(windows, target_os = "macos"))]
impl RealTray {
    fn create(inputs: &tray_logic::TrayInputs) -> Option<Self> {
        use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
        let menu = Menu::new();
        let mut ids = std::collections::HashMap::new();
        for entry in tray_logic::menu_entries(inputs) {
            match entry {
                tray_logic::MenuEntry::Separator => {
                    let _ = menu.append(&PredefinedMenuItem::separator());
                }
                tray_logic::MenuEntry::Item(id) => {
                    let item = MenuItem::new(strings::text(id), true, None);
                    let action = match id {
                        "tray.show" => tray_logic::TrayAction::OpenPopup,
                        "tray.pause" | "tray.resume" => tray_logic::TrayAction::TogglePause,
                        "tray.pause_1h" => tray_logic::TrayAction::PauseFor1h,
                        "tray.config" => tray_logic::TrayAction::OpenConfig,
                        "tray.about" => tray_logic::TrayAction::About,
                        _ => tray_logic::TrayAction::Quit,
                    };
                    ids.insert(item.id().0.clone(), action);
                    let _ = menu.append(&item);
                }
            }
        }
        let icon =
            tray_icon::Icon::from_rgba(vec![0x33_u8, 0x99, 0xcc, 0xff].repeat(16 * 16), 16, 16)
                .ok()?;
        let tray = tray_icon::TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_tooltip(strings::text("tray.tooltip"))
            .build()
            .ok()?;
        Some(Self { _tray: tray, ids })
    }

    fn poll(&self) -> Vec<tray_logic::TrayAction> {
        let mut out = Vec::new();
        while let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            if let Some(action) = self.ids.get(&event.id().0) {
                out.push(action.clone());
            }
        }
        while let Ok(_event) = tray_icon::TrayIconEvent::receiver().try_recv() {}
        out
    }
}

/// Popup application state.
#[allow(clippy::struct_excessive_bools)]
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
    config_open: bool,
    config_doc: Option<config_model::ConfigDoc>,
    config_filter: String,
    config_tab: String,
    config_edits: std::collections::HashMap<String, String>,
    config_error: String,
    caps_text: String,
    version_text: String,
    profile: PathBuf,
    backup_path: String,
    log_lines: Vec<String>,
    dark: bool,
    font_scale: f32,
    pause_deadline: Option<Instant>,
    pending_apply: Vec<String>,
    perm_vm: permanents::PermanentsVm,
    perm_source: Option<usize>,
    perm_group_new: String,
    perm_confirm_delete: Option<uuid::Uuid>,
    viewport_corrected: bool,
    window_config_sized: bool,
    was_focused: bool,
    #[cfg(any(windows, target_os = "macos"))]
    real_tray: Option<RealTray>,
    #[cfg(target_os = "linux")]
    linux_tray: linux_tray::LinuxTray,
}

/// Text form of a config value for an edit field.
fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        other => other.to_string(),
    }
}

/// Parse an edit field back into a value for one row kind.
fn parse_edit(kind: config_model::Kind, text: &str) -> Option<Value> {
    use config_model::Kind as K;
    match kind {
        K::Bool => text.parse::<bool>().ok().map(Value::from),
        K::Int => text.trim().parse::<i64>().ok().map(Value::from),
        K::Text | K::Chord | K::Enum => Some(Value::String(text.trim().to_string())),
        K::StringList => Some(Value::Array(
            text.split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(|part| Value::String(part.to_string()))
                .collect(),
        )),
        K::ChordMap | K::PasteChords => serde_json::from_str(text).ok(),
    }
}

impl ShellApp {
    /// Send one bridge command, ignoring a torn-down bridge.
    fn send(&self, cmd: ipc_bridge::UiCmd) {
        let _ = self.cmds.send(cmd);
    }

    /// Drain bridge events into view-model state.
    #[allow(clippy::too_many_lines)]
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
                    if let Some(version) = body.get("version").and_then(Value::as_str) {
                        self.version_text = version.to_string();
                    }
                    self.inputs.degraded = body
                        .get("degraded")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                }
                ipc_bridge::UiEvent::History(body) => {
                    if self.vm.apply_history(&body) {
                        let current = self.vm.clips().first().cloned();
                        self.vm.set_current(current);
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
                ipc_bridge::UiEvent::Config(body) => {
                    let doc = config_model::ConfigDoc::from_snapshot(&body);
                    // Theme prefs live in the ui.theme extra key.
                    if let Some(theme) = doc.get("ui.theme") {
                        self.dark = theme.get("mode").and_then(Value::as_str) != Some("light");
                        if let Some(scale) = theme.get("font_scale").and_then(Value::as_f64) {
                            #[allow(clippy::cast_possible_truncation)]
                            {
                                self.font_scale = scale as f32;
                            }
                        }
                    }
                    // popup.keys live-apply (U1-F8).
                    if let Some(keys_value) = doc.get("popup.keys") {
                        if let Ok(keys) = serde_json::from_value::<
                            std::collections::BTreeMap<String, String>,
                        >(keys_value.clone())
                        {
                            let _ = self.vm.apply_keys(&keys);
                        }
                    }
                    self.config_doc = Some(doc);
                    self.config_edits.clear();
                }
                ipc_bridge::UiEvent::ConfigApplied(key) => {
                    self.pending_apply.retain(|pending| pending != &key);
                    if let Some(doc) = &mut self.config_doc {
                        let wire_key = if key == "popup.keys" {
                            "popup.keys.*"
                        } else {
                            key.as_str()
                        };
                        doc.mark_applied(wire_key);
                    }
                    self.config_edits.remove(&key);
                }
                ipc_bridge::UiEvent::Caps(body) => {
                    self.caps_text = body.to_string();
                }
                ipc_bridge::UiEvent::HistoryClip(body) => {
                    if let Some(clip_value) = body.get("clip") {
                        if let Ok(clip) = serde_json::from_value::<clipmonger_core::model::Clip>(
                            clip_value.clone(),
                        ) {
                            self.vm.set_current(Some(clip));
                        }
                    }
                }
                ipc_bridge::UiEvent::MonitorPaused(paused) => {
                    self.inputs.paused = paused;
                    self.vm.set_paused(paused);
                    if !paused {
                        self.pause_deadline = None;
                    }
                }
                ipc_bridge::UiEvent::Permanents(body) => {
                    self.perm_vm.apply_list(&body);
                }
                ipc_bridge::UiEvent::PermGroups(body) => {
                    self.perm_vm.apply_groups(&body);
                }
                ipc_bridge::UiEvent::PermanentSaved => {
                    self.perm_vm.draft = None;
                    self.send(ipc_bridge::UiCmd::ListPermanents);
                    self.send(ipc_bridge::UiCmd::PermGroups);
                }
                ipc_bridge::UiEvent::PermanentDeleted(id) => {
                    self.perm_vm.remove_confirmed(id);
                    self.perm_confirm_delete = None;
                    self.send(ipc_bridge::UiCmd::PermGroups);
                }
                ipc_bridge::UiEvent::PermGroupsChanged => {
                    self.perm_group_new.clear();
                    self.send(ipc_bridge::UiCmd::ListPermanents);
                    self.send(ipc_bridge::UiCmd::PermGroups);
                }
                ipc_bridge::UiEvent::Purged(_) => {}
                ipc_bridge::UiEvent::BackupDone(path) => {
                    self.config_error = format!("{} {path}", strings::text("config.backup_saved"));
                }
                ipc_bridge::UiEvent::RestoreDone => {
                    self.config_error = strings::text("config.restore_done").to_string();
                    self.send(ipc_bridge::UiCmd::ListHistory);
                }
                ipc_bridge::UiEvent::LogsTail(lines) => {
                    self.log_lines = lines;
                }
                ipc_bridge::UiEvent::ConfigError(message) => {
                    self.config_error = message;
                }
                ipc_bridge::UiEvent::Error(message) => {
                    self.error = message.clone();
                    if !self.pending_apply.is_empty() {
                        self.config_error = message;
                        self.pending_apply.clear();
                    }
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
            popup::VmEffect::GroupTab(n) => {
                self.perm_vm.select_group(usize::from(n.saturating_sub(1)));
                self.perm_source = Some(self.perm_vm.active_group);
            }
            popup::VmEffect::ExecPermanent(id) => {
                self.send(ipc_bridge::UiCmd::ExecPermanent(id));
            }
        }
    }

    /// Run one tray action.
    #[allow(clippy::needless_pass_by_value)]
    fn run_tray_action(&mut self, action: tray_logic::TrayAction) {
        match action {
            tray_logic::TrayAction::OpenPopup => {
                self.open = true;
                self.vm.target_session.show();
                self.send(ipc_bridge::UiCmd::ListHistory);
                self.send(ipc_bridge::UiCmd::ListPermanents);
                self.send(ipc_bridge::UiCmd::PermGroups);
            }
            tray_logic::TrayAction::TogglePause => {
                self.pause_deadline = None;
                self.send(ipc_bridge::UiCmd::MonitorToggle);
            }
            tray_logic::TrayAction::PauseFor1h => {
                // Client-side deadline (U1-F3); a daemon restart loses it.
                if !self.inputs.paused {
                    self.pause_deadline = Some(Instant::now() + Duration::from_secs(3600));
                    self.send(ipc_bridge::UiCmd::MonitorToggle);
                }
            }
            tray_logic::TrayAction::OpenConfig => {
                self.config_open = true;
                self.send(ipc_bridge::UiCmd::ConfigGet);
                self.send(ipc_bridge::UiCmd::CapsGet);
                self.send(ipc_bridge::UiCmd::ListPermanents);
                self.send(ipc_bridge::UiCmd::PermGroups);
            }
            tray_logic::TrayAction::About => {
                self.config_open = true;
                self.config_tab = "about".to_string();
                self.send(ipc_bridge::UiCmd::CapsGet);
            }
            tray_logic::TrayAction::Quit => self.send(ipc_bridge::UiCmd::Stop),
        }
    }

    /// Poll tray controllers (fake logic tray plus the real OS tray).
    fn poll_tray(&mut self) {
        let actions = self.tray.on_events(&mut self.fake_tray);
        for action in actions {
            self.run_tray_action(action);
        }
        #[cfg(any(windows, target_os = "macos"))]
        if let Some(real) = &self.real_tray {
            for action in real.poll() {
                self.run_tray_action(action);
            }
        }
        #[cfg(target_os = "linux")]
        for action in self.linux_tray.poll() {
            self.run_tray_action(action);
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
                (egui::Key::Num1, "1"),
                (egui::Key::Num2, "2"),
                (egui::Key::Num3, "3"),
                (egui::Key::Num4, "4"),
                (egui::Key::Num5, "5"),
                (egui::Key::Num6, "6"),
                (egui::Key::Num7, "7"),
                (egui::Key::Num8, "8"),
                (egui::Key::Num9, "9"),
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
            if editing && !popup::forwards_key_when_editing(&name) {
                continue;
            }
            let event = keymap::KeyEvent::new(name, ctrl, alt, false);
            let effect = self.vm.on_key(event);
            self.apply_effect(effect);
        }
    }

    /// Render the maximalist config window.
    #[allow(clippy::too_many_lines)]
    fn render_config(&mut self, ctx: &egui::Context) {
        if !self.config_open {
            return;
        }
        let mut open = true;
        let mut do_apply = false;
        let mut do_revert = false;
        let mut do_purge = false;
        let mut do_backup = false;
        let mut do_restore = false;
        let mut do_logs = false;
        let mut stage_edits: Vec<(String, Value)> = Vec::new();
        let mut theme_changed = false;
        let mut perm_send: Vec<ipc_bridge::UiCmd> = Vec::new();
        let mut perm_save = false;
        let mut perm_cancel = false;
        let mut perm_begin_new = false;
        let mut perm_begin_edit: Option<uuid::Uuid> = None;
        let mut perm_select: Option<usize> = None;
        let mut perm_confirm: Option<uuid::Uuid> = None;
        egui::Window::new(strings::text("config.title"))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.config_filter)
                        .hint_text(strings::text("config.filter_hint")),
                );
                let tabs: &[(&str, &str)] = &[
                    ("History", "config.tab_history"),
                    ("Popup", "config.tab_popup"),
                    ("Tray", "config.tab_tray"),
                    ("Startup", "config.tab_startup"),
                    ("Privacy", "config.tab_privacy"),
                    ("Advanced", "config.tab_advanced"),
                    ("PermanentClips", "config.tab_permanent_clips"),
                    ("General", "config.tab_general"),
                    ("databases", "config.tab_databases"),
                    ("about", "config.tab_about"),
                ];
                ui.horizontal_wrapped(|ui| {
                    for (name, label) in tabs {
                        if ui
                            .selectable_label(self.config_tab == *name, strings::text(label))
                            .clicked()
                        {
                            self.config_tab = (*name).to_string();
                        }
                    }
                });
                if self.config_tab == "databases" {
                    ui.label(strings::text("config.profile_path"));
                    ui.label(self.profile.display().to_string());
                    if ui.button(strings::text("config.purge_removed")).clicked() {
                        do_purge = true;
                    }
                    ui.label(strings::text("config.backup_path"));
                    ui.add(egui::TextEdit::singleline(&mut self.backup_path));
                    if ui.button(strings::text("config.backup_create")).clicked() {
                        do_backup = true;
                    }
                    if ui.button(strings::text("config.backup_restore")).clicked() {
                        do_restore = true;
                    }
                } else if self.config_tab == "about" {
                    ui.label(strings::text("config.help_title"));
                    ui.label(strings::text("config.about_version"));
                    ui.label(&self.version_text);
                    ui.label(strings::text("config.accesskit_note"));
                    ui.label(strings::text("config.caps_title"));
                    ui.label(&self.caps_text);
                    ui.label(strings::text("config.logs_title"));
                    if ui.button(strings::text("config.logs_refresh")).clicked() {
                        do_logs = true;
                    }
                    for line in &self.log_lines {
                        ui.label(line);
                    }
                } else {
                    // Theme controls persist through the ui.theme extra key.
                    if ui
                        .checkbox(&mut self.dark, strings::text("config.theme_dark"))
                        .changed()
                    {
                        theme_changed = true;
                    }
                    let scale = egui::Slider::new(&mut self.font_scale, 0.8..=1.6)
                        .text(strings::text("config.font_scale"));
                    if ui.add(scale).changed() {
                        theme_changed = true;
                    }
                    ui.label(strings::text("config.non_atomic_note"));
                    if self.config_tab == "PermanentClips" {
                        ui.label(strings::text("perm.immediate_note"));
                        ui.label(strings::text("perm.hotkey_note"));
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.perm_group_new));
                            if ui.button(strings::text("perm.new_group")).clicked() {
                                perm_send.push(ipc_bridge::UiCmd::PermGroupCreate(
                                    self.perm_group_new.clone(),
                                ));
                            }
                        });
                        let group_names = self.perm_vm.group_names();
                        for (index, name) in group_names.iter().enumerate() {
                            if ui
                                .selectable_label(self.perm_vm.active_group == index, name)
                                .clicked()
                            {
                                perm_select = Some(index);
                            }
                        }
                        let active_count = self
                            .perm_vm
                            .group_infos()
                            .iter()
                            .find(|g| group_names.get(self.perm_vm.active_group) == Some(&g.name))
                            .map_or(0, |g| g.count);
                        if active_count > 0 {
                            ui.label(strings::text("perm.group_not_empty"));
                        }
                        if let Some(name) = group_names.get(self.perm_vm.active_group).cloned() {
                            if ui
                                .add_enabled(
                                    active_count == 0,
                                    egui::Button::new(strings::text("perm.delete_group")),
                                )
                                .clicked()
                            {
                                perm_send.push(ipc_bridge::UiCmd::PermGroupDelete(name));
                            }
                        }
                        let visible_rows: Vec<(uuid::Uuid, String, String)> = self
                            .perm_vm
                            .visible("")
                            .iter()
                            .map(|&i| {
                                let clip = &self.perm_vm.permanents[i];
                                (
                                    clip.id,
                                    clip.name.clone(),
                                    clip.hotkey.clone().unwrap_or_default(),
                                )
                            })
                            .collect();
                        if visible_rows.is_empty() {
                            ui.label(strings::text("perm.empty_group"));
                        }
                        for (row, (id, name, hotkey)) in visible_rows.iter().enumerate() {
                            let label = if hotkey.is_empty() {
                                name.clone()
                            } else {
                                format!("{name} {hotkey}")
                            };
                            if ui
                                .selectable_label(self.perm_vm.selected == row, label)
                                .clicked()
                            {
                                self.perm_vm.selected = row;
                                perm_begin_edit = Some(*id);
                            }
                        }
                        if ui.button(strings::text("perm.new_clip")).clicked() {
                            perm_begin_new = true;
                        }
                        if let Some(id) = self.perm_vm.activate_selected() {
                            if ui.button(strings::text("perm.delete_clip")).clicked() {
                                perm_confirm = Some(id);
                            }
                            if self.perm_confirm_delete == Some(id) {
                                ui.label(strings::text("perm.delete_confirm"));
                                if ui.button(strings::text("perm.delete_clip")).clicked() {
                                    perm_send.push(ipc_bridge::UiCmd::PermDelete(id));
                                }
                            }
                        }
                        if let Some(draft) = &mut self.perm_vm.draft {
                            ui.label(strings::text("perm.name"));
                            ui.add(egui::TextEdit::singleline(&mut draft.name));
                            ui.label(strings::text("perm.group"));
                            ui.add(egui::TextEdit::singleline(&mut draft.group));
                            ui.label(strings::text("perm.hotkey"));
                            ui.add(egui::TextEdit::singleline(&mut draft.hotkey));
                            ui.label(strings::text("perm.body"));
                            ui.add(egui::TextEdit::multiline(&mut draft.body));
                            if ui.button(strings::text("perm.save")).clicked() {
                                perm_save = true;
                            }
                            if ui.button(strings::text("perm.cancel_edit")).clicked() {
                                perm_cancel = true;
                            }
                        }
                    }
                    let Some(doc) = &self.config_doc else {
                        ui.label(strings::text("popup.offline"));
                        return;
                    };
                    let filtered = config_model::filter_rows(&self.config_filter);
                    let mut rows: Vec<(String, config_model::Kind, String, String, bool)> =
                        Vec::new();
                    for index in filtered {
                        let row = config_model::ROWS[index];
                        if format!("{:?}", row.tab) != self.config_tab {
                            continue;
                        }
                        if self.config_tab == "PermanentClips" && row.key == "permanents.groups" {
                            continue;
                        }
                        let saved = doc.get(row.key);
                        let staged = doc.staged(row.key);
                        let current = staged.or(saved);
                        let text = current.map_or_else(String::new, value_text);
                        rows.push((
                            row.key.to_string(),
                            row.kind,
                            strings::text(row.label).to_string(),
                            text,
                            doc.is_dirty(row.key),
                        ));
                        if row.key == "popup.hotkey" {}
                    }
                    egui::ScrollArea::vertical()
                        .max_height(380.0)
                        .show(ui, |ui| {
                            for (key, kind, label, current, _dirty) in &rows {
                                ui.label(label);
                                if *kind == config_model::Kind::Bool {
                                    let mut flag = current == "true";
                                    if ui.checkbox(&mut flag, label.as_str()).changed() {
                                        stage_edits.push((key.clone(), Value::Bool(flag)));
                                    }
                                } else {
                                    let entry = self
                                        .config_edits
                                        .entry(key.clone())
                                        .or_insert_with(|| current.clone());
                                    if ui.add(egui::TextEdit::singleline(entry)).changed() {
                                        if let Some(value) = parse_edit(*kind, entry) {
                                            stage_edits.push((key.clone(), value));
                                        }
                                    }
                                }
                                if key == "popup.hotkey" {
                                    ui.label(strings::text("config.applies_on_restart"));
                                }
                            }
                        });
                    let dirty = doc.dirty_keys();
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !dirty.is_empty(),
                                egui::Button::new(strings::text("config.apply")),
                            )
                            .clicked()
                        {
                            do_apply = true;
                        }
                        if ui
                            .add_enabled(
                                !dirty.is_empty(),
                                egui::Button::new(strings::text("config.revert")),
                            )
                            .clicked()
                        {
                            do_revert = true;
                        }
                    });
                    if !self.config_error.is_empty() {
                        ui.label(&self.config_error);
                    }
                }
                if theme_changed { /* handled after the window */ }
            });
        self.config_open = open;
        if let Some(index) = perm_select {
            self.perm_vm.select_group(index);
        }
        if perm_begin_new {
            self.perm_vm.begin_new();
        }
        if let Some(id) = perm_begin_edit {
            self.perm_vm.begin_edit(id);
        }
        if let Some(id) = perm_confirm {
            self.perm_confirm_delete = Some(id);
        }
        if perm_cancel {
            self.perm_vm.draft = None;
        }
        if perm_save {
            match self.perm_vm.validate_draft() {
                Ok(params) => {
                    self.config_error.clear();
                    let draft_id = self.perm_vm.draft.as_ref().and_then(|d| d.id);
                    if let Some(id) = draft_id {
                        if let Some(mut clip) =
                            self.perm_vm.permanents.iter().find(|c| c.id == id).cloned()
                        {
                            clip.group = params.group;
                            clip.name = params.name;
                            clip.hotkey = params.hotkey;
                            clip.body = params.body;
                            self.send(ipc_bridge::UiCmd::PermUpdate(clip));
                        }
                    } else {
                        self.send(ipc_bridge::UiCmd::PermCreate(params));
                    }
                }
                Err(id) => self.config_error = strings::text(id).to_string(),
            }
        }
        for cmd in perm_send {
            self.send(cmd);
        }
        if theme_changed {
            let mode = if self.dark { "dark" } else { "light" };
            self.send(ipc_bridge::UiCmd::ConfigSet(
                "ui.theme".to_string(),
                serde_json::json!({ "mode": mode, "font_scale": self.font_scale }),
            ));
        }
        for (key, value) in stage_edits {
            if let Some(doc) = &mut self.config_doc {
                match doc.stage(&key, value) {
                    Ok(()) => self.config_error.clear(),
                    Err(id) => self.config_error = strings::text(&id).to_string(),
                }
            }
        }
        if do_purge {
            self.send(ipc_bridge::UiCmd::PurgeRemoved);
        }
        if do_backup {
            self.send(ipc_bridge::UiCmd::BackupCreate(self.backup_path.clone()));
        }
        if do_restore {
            self.send(ipc_bridge::UiCmd::BackupRestore(self.backup_path.clone()));
        }
        if do_logs {
            self.send(ipc_bridge::UiCmd::LogsTail(100));
        }
        if do_revert {
            if let Some(doc) = &mut self.config_doc {
                doc.revert();
            }
            self.config_edits.clear();
        }
        if do_apply {
            if let Some(doc) = &self.config_doc {
                self.pending_apply = doc.dirty_keys();
                for (key, value) in doc.apply_plan() {
                    let wire_key = if key == "popup.keys.*" {
                        "popup.keys".to_string()
                    } else {
                        key
                    };
                    self.send(ipc_bridge::UiCmd::ConfigSet(wire_key, value));
                }
                // popup.keys live-applies in the Config event handler on reload.
                self.send(ipc_bridge::UiCmd::ConfigGet);
            }
        }
    }

    /// Render the popup window contents.
    #[allow(clippy::too_many_lines)]
    fn render(&mut self, ui: &mut egui::Ui) {
        let mut query = self.vm.query.clone();
        let mut clicked_row: Option<usize> = Option::None;
        let mut clicked_perm_row: Option<usize> = Option::None;
        let mut perm_tab_click: Option<Option<usize>> = None;
        let mut pin_row: Option<usize> = Option::None;
        let mut delete_row: Option<usize> = Option::None;
        let mut search_focused = false;
        egui::CentralPanel::default().show(ui, |ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut query)
                    .hint_text(strings::text("popup.search_hint")),
            );
            search_focused = field.has_focus();
            if let Some(current) = self.vm.current() {
                ui.label(strings::text("popup.current"));
                ui.label(strings::text(popup::preview_kind(current)));
                ui.label(&current.preview);
            }
            let group_names = self.perm_vm.group_names();
            if !group_names.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .selectable_label(
                            self.perm_source.is_none(),
                            strings::text("perm.tab_history"),
                        )
                        .clicked()
                    {
                        perm_tab_click = Some(None);
                    }
                    for (index, name) in group_names.iter().enumerate() {
                        if ui
                            .selectable_label(self.perm_source == Some(index), name)
                            .clicked()
                        {
                            perm_tab_click = Some(Some(index));
                        }
                    }
                });
            }
            if self.perm_source.is_some() && !group_names.is_empty() {
                let rows = self.perm_vm.visible(&query);
                if rows.is_empty() {
                    ui.label(strings::text("popup.empty"));
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (row, index) in rows.iter().enumerate() {
                        let clip = &self.perm_vm.permanents[*index];
                        if ui
                            .selectable_label(self.perm_vm.selected == row, &clip.name)
                            .clicked()
                        {
                            clicked_perm_row = Some(row);
                        }
                    }
                });
            } else {
                let count = self.vm.visible().len();
                if count == 0 {
                    ui.label(strings::text("popup.empty"));
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for row in 0..count {
                        let Some(clip) = self.vm.clip_at(row) else {
                            continue;
                        };
                        let clip_id = clip.id;
                        let pinned = clip.pinned;
                        let response = ui.selectable_label(self.vm.selected == row, &clip.preview);
                        if response.clicked() {
                            clicked_row = Some(row);
                        }
                        response.context_menu(|ui| {
                            let pin_label = if pinned {
                                strings::text("popup.unpin")
                            } else {
                                strings::text("popup.pin")
                            };
                            if ui.button(pin_label).clicked() {
                                pin_row = Some(row);
                                ui.close();
                            }
                            if ui.button(strings::text("popup.delete")).clicked() {
                                delete_row = Some(row);
                                ui.close();
                            }
                            let _ = clip_id;
                        });
                    }
                });
            }
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
        if let Some(tab) = perm_tab_click {
            match tab {
                None => self.perm_source = None,
                Some(index) => {
                    self.perm_vm.select_group(index);
                    self.perm_source = Some(self.perm_vm.active_group);
                }
            }
        }
        if let Some(row) = clicked_perm_row {
            if row == self.perm_vm.selected {
                if let Some(id) = self.perm_vm.activate_selected() {
                    self.apply_effect(popup::VmEffect::ExecPermanent(id));
                }
            } else {
                self.perm_vm.selected = row;
            }
        }
        if let Some(row) = pin_row {
            if let Some(clip) = self.vm.clip_at(row) {
                let id = clip.id;
                let pinned = !self.vm.is_pinned(id);
                self.send(ipc_bridge::UiCmd::PinClip(id, pinned));
            }
        }
        if let Some(row) = delete_row {
            if let Some(clip) = self.vm.clip_at(row) {
                let id = clip.id;
                self.send(ipc_bridge::UiCmd::DeleteClip(id));
            }
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
        if !self.viewport_corrected {
            let monitor_size = ui.ctx().input(|input| input.viewport().monitor_size);
            if let Some(pos) = popup_position_for_monitor(monitor_size) {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
                self.viewport_corrected = true;
            }
        }
        if ui.ctx().input(|input| input.viewport().close_requested()) {
            self.send(ipc_bridge::UiCmd::Stop);
        }
        self.drain_events();
        self.poll_tray();
        if let Some(deadline) = self.pause_deadline {
            if Instant::now() >= deadline && self.inputs.paused {
                self.pause_deadline = None;
                self.send(ipc_bridge::UiCmd::MonitorToggle);
            }
        }
        ui.ctx().set_visuals(if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
        ui.ctx().set_pixels_per_point(self.font_scale);
        let focused = ui
            .ctx()
            .input(|input| input.viewport().focused.unwrap_or(true));
        let focus_gained = focused && !self.was_focused;
        self.was_focused = focused;
        let size_mismatch = ui.ctx().input(|input| {
            input.viewport().inner_rect.is_none_or(|rect| {
                let (w, h) = placement::window_size_for(self.config_open);
                #[allow(clippy::cast_precision_loss)]
                let want = egui::vec2(w as f32, h as f32);
                (rect.width() - want.x).abs() > 8.0 || (rect.height() - want.y).abs() > 8.0
            })
        });
        if (self.config_open != self.window_config_sized || focus_gained) && size_mismatch {
            let (w, h) = placement::window_size_for(self.config_open);
            #[allow(clippy::cast_precision_loss)]
            let size = egui::vec2(w as f32, h as f32);
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Resizable(self.config_open));
            self.window_config_sized = self.config_open;
        }
        self.render_config(ui.ctx());
        if self.last_poll.elapsed() >= STATUS_PERIOD {
            self.last_poll = Instant::now();
            self.send(ipc_bridge::UiCmd::Status);
        }
        if self.open {
            self.render(ui);
        } else {
            let mut reopen = false;
            let mut open_config = false;
            egui::CentralPanel::default().show(ui, |ui| {
                if ui.button(strings::text("popup.show")).clicked() {
                    reopen = true;
                }
                if ui.button(strings::text("config.open_config")).clicked() {
                    open_config = true;
                }
            });
            if open_config {
                self.config_open = true;
                self.send(ipc_bridge::UiCmd::ConfigGet);
                self.send(ipc_bridge::UiCmd::CapsGet);
                self.send(ipc_bridge::UiCmd::ListPermanents);
                self.send(ipc_bridge::UiCmd::PermGroups);
            }
            if reopen {
                self.open = true;
                self.send(ipc_bridge::UiCmd::ListHistory);
                self.send(ipc_bridge::UiCmd::ListPermanents);
                self.send(ipc_bridge::UiCmd::PermGroups);
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
    let mode = lifecycle::start_mode(client.is_some());
    let transport: Box<dyn ipc_bridge::Transport> = match client {
        Some(client) => Box::new(ClientTransport { client }),
        Option::None => Box::new(OfflineTransport),
    };
    let online = mode == lifecycle::StartMode::Attached;
    let (cmds, events) = ipc_bridge::Bridge::spawn(transport);
    let _ = cmds.send(ipc_bridge::UiCmd::Status);
    let _ = cmds.send(ipc_bridge::UiCmd::ListHistory);
    let _ = cmds.send(ipc_bridge::UiCmd::ListPermanents);
    let _ = cmds.send(ipc_bridge::UiCmd::PermGroups);
    let monitor = placement::monitor_rect_from_size(None);
    let (px, py) = placement::place(placement::Placement::Center, (0, 0), &[monitor], POPUP_SIZE);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([POPUP_SIZE.0 as f32, POPUP_SIZE.1 as f32])
            .with_position(egui::pos2(px as f32, py as f32))
            .with_decorations(false)
            .with_window_level(egui::WindowLevel::AlwaysOnTop)
            .with_taskbar(false)
            .with_active(true)
            .with_resizable(false),
        ..Default::default()
    };
    let mut vm = popup::PopupVm::new();
    vm.set_offline(!online);
    let initial_inputs = tray_logic::TrayInputs {
        paused: false,
        degraded: false,
        attached: online,
    };
    let app = ShellApp {
        vm,
        cmds,
        events,
        tray: tray_logic::TrayLogic::new(),
        fake_tray: FakeTray::new(Vec::new()),
        inputs: initial_inputs,
        open: !minimized,
        error: String::new(),
        last_poll: Instant::now(),
        config_open: false,
        config_doc: None,
        config_filter: String::new(),
        config_tab: "History".to_string(),
        config_edits: std::collections::HashMap::new(),
        config_error: String::new(),
        caps_text: String::new(),
        version_text: String::new(),
        profile: profile.clone(),
        backup_path: profile.join("backup.zip").display().to_string(),
        log_lines: Vec::new(),
        dark: true,
        font_scale: 1.0,
        pause_deadline: None,
        pending_apply: Vec::new(),
        perm_vm: permanents::PermanentsVm::new(),
        perm_source: None,
        perm_group_new: String::new(),
        perm_confirm_delete: None,
        viewport_corrected: false,
        window_config_sized: false,
        was_focused: true,
        #[cfg(any(windows, target_os = "macos"))]
        real_tray: match RealTray::create(&tray_logic::TrayInputs::default()) {
            Some(tray) => Some(tray),
            None => {
                eprintln!("tray unavailable: degraded, window still opens");
                None
            }
        },
        #[cfg(target_os = "linux")]
        linux_tray: linux_tray::LinuxTray::spawn(&initial_inputs),
    };
    match eframe::run_native("clipmonger", options, Box::new(|_| Ok(Box::new(app)))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("shell failed: {error}");
            ExitCode::from(1)
        }
    }
}

//! The popup application on the runner: the glue between the bridge, the
//! tray, the shell state machine and the two egui windows (popup and
//! Settings). `logic` runs on every pass, hidden or not; `ui` only paints.

mod popup_ui;
mod settings_ui;
pub mod trace;

use std::any::Any;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use clipmonger_ipc::proto::methods::ShowPopupEvent;
use serde_json::Value;
use uuid::Uuid;

pub use settings_ui::{render_config_viewport, ConfigWindowState};
pub use trace::{Trace, TraceEvent};

use crate::ipc_bridge::{UiCmd, UiEvent};
use crate::runner::{self, WindowKind};
use crate::shell::{Effect, Event, Millis, Shell};
use crate::tray_logic::TrayAction;
use crate::{config_model, placement, popup, strings};

/// Status re-poll period.
pub const STATUS_PERIOD: Duration = Duration::from_secs(30);
/// Max bridge events drained per pass.
pub const DRAIN_CAP: usize = 32;
/// How long "pause for one hour" pauses.
pub const PAUSE_PERIOD: Duration = Duration::from_secs(3600);

/// Unix milliseconds now (the pick time of a paste).
#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// What the process hands to the app.
pub struct AppParts {
    /// Profile directory (shown on the Databases tab).
    pub profile: PathBuf,
    /// Commands to the bridge thread.
    pub cmds: mpsc::Sender<UiCmd>,
    /// Events from the bridge thread: replies and daemon pushes.
    pub events: mpsc::Receiver<UiEvent>,
    /// Drains the tray actions clicked since the last call.
    pub tray: Box<dyn FnMut() -> Vec<TrayAction>>,
    /// Kept alive for the life of the app (the OS tray icon).
    pub hold: Option<Box<dyn Any>>,
    /// Start with the popup open (no `--minimized`, no `--popup`).
    pub open: bool,
    /// Period of the status poll; [`STATUS_PERIOD`] outside tests.
    pub status_period: Duration,
    /// The UI trace, when enabled.
    pub trace: Option<Trace>,
    /// Runs for [`Effect::AllowForeground`], while the popup still is the
    /// foreground window: `foreground::allow_any` on Windows.
    pub allow_foreground: Box<dyn Fn()>,
}

/// The application behind the runner's windows.
#[allow(clippy::struct_excessive_bools)]
pub struct PopupApp {
    shell: Shell,
    vm: popup::PopupVm,
    config: Arc<Mutex<ConfigWindowState>>,
    cmds: mpsc::Sender<UiCmd>,
    events: mpsc::Receiver<UiEvent>,
    tray: Box<dyn FnMut() -> Vec<TrayAction>>,
    _hold: Option<Box<dyn Any>>,
    allow_foreground: Box<dyn Fn()>,
    trace: Option<Trace>,
    status_period: Duration,
    /// Origin of the shell clock.
    start: Instant,
    last_poll: Instant,
    pause_deadline: Option<Instant>,
    paused: bool,
    /// The shell closed since the last `Hidden`: confirm it at the next pass.
    hide_unconfirmed: bool,
    /// The popup has been seen focused since it opened.
    seen_focus: bool,
    /// The popup window was moved to the monitor centre.
    centered: bool,
    /// Put the keyboard focus into the search box at the next render.
    focus_search: bool,
    /// A permanent exec was sent and is waiting for its paste result.
    exec_in_flight: Option<Uuid>,
    /// Theme last applied to the contexts: popup, settings (dark, font scale bits).
    theme_applied: [Option<(bool, u32)>; 2],
}

impl PopupApp {
    /// Build the app around `parts`: a shell with the popup open or closed, an
    /// empty popup view-model, fresh Settings state for the profile, the status
    /// poll clock started now. Sends the commands of `Shell::start()` to the
    /// bridge, in their order. With `open`, the search box takes the keyboard
    /// focus at the first render and the trace records
    /// `Open { source: "start", target: false, gesture: false }`.
    #[must_use]
    pub fn new(parts: AppParts) -> Self {
        let AppParts {
            profile,
            cmds,
            events,
            tray,
            hold,
            open,
            status_period,
            trace,
            allow_foreground,
        } = parts;
        let now = Instant::now();
        let mut app = Self {
            shell: Shell::new(open),
            vm: popup::PopupVm::new(),
            config: Arc::new(Mutex::new(ConfigWindowState::new(profile))),
            cmds,
            events,
            tray,
            _hold: hold,
            allow_foreground,
            trace,
            status_period,
            start: now,
            last_poll: now,
            pause_deadline: None,
            paused: false,
            hide_unconfirmed: false,
            seen_focus: false,
            centered: false,
            focus_search: open,
            exec_in_flight: None,
            theme_applied: [None, None],
        };
        app.apply_effects(Shell::start());
        if open {
            app.record(&TraceEvent::Open {
                source: "start",
                target: false,
                gesture: false,
            });
        }
        app
    }

    /// The shell state machine.
    #[must_use]
    pub fn shell(&self) -> &Shell {
        &self.shell
    }

    /// The popup view-model.
    #[must_use]
    pub fn vm(&self) -> &popup::PopupVm {
        &self.vm
    }

    /// The Settings state shared by both windows.
    #[must_use]
    pub fn config(&self) -> &Arc<Mutex<ConfigWindowState>> {
        &self.config
    }

    /// Send one bridge command, ignoring a torn-down bridge.
    fn send(&self, cmd: UiCmd) {
        let _ = self.cmds.send(cmd);
    }

    /// Milliseconds on the shell clock.
    fn millis(&self) -> Millis {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Append one line to the trace, when there is one.
    fn record(&mut self, event: &TraceEvent) {
        if let Some(trace) = &mut self.trace {
            trace.record(event);
        }
    }

    /// The only door into the shell. Takes a snapshot (popup open, context
    /// holds a target, context holds a gesture, error text), calls
    /// `Shell::handle(event, millis())` and then, in this order:
    /// 1. traces the transition: closed to open is
    ///    `Open { source: trace::open_source(&event), target, gesture }` (the
    ///    flags of the new context); open to closed is
    ///    `Hide { reason: trace::hide_reason(&event) }`; an error line that is
    ///    new or changed, or that an event of the kind `PasteFailed`, `Error` or
    ///    `PasteUnavailable` set again, is `Error` while the popup is open;
    /// 2. `apply_effects`;
    /// 3. closed to open: sends `ListPermanents` and `PermGroups` (the shell
    ///    sends only `ListHistory`) and clears `seen_focus`; open to closed:
    ///    sets `hide_unconfirmed` and clears `seen_focus`.
    fn feed(&mut self, event: Event) {
        let was_open = self.shell.desired().popup_open;
        let old_error = self.shell.error().map(str::to_owned);
        let error_kind = matches!(
            &event,
            Event::Bridge(UiEvent::PasteFailed(_) | UiEvent::Error(_) | UiEvent::PasteUnavailable)
        );
        let source = trace::open_source(&event);
        let reason = trace::hide_reason(&event);
        let effects = self.shell.handle(event, self.millis());
        let is_open = self.shell.desired().popup_open;
        if !was_open && is_open {
            let context = self.shell.context();
            self.record(&TraceEvent::Open {
                source,
                target: context.target.is_some(),
                gesture: context.gesture.is_some(),
            });
        } else if was_open && !is_open {
            self.record(&TraceEvent::Hide { reason });
        }
        let error_shown = is_open
            && self
                .shell
                .error()
                .is_some_and(|error| error_kind || old_error.as_deref() != Some(error));
        if error_shown {
            self.record(&TraceEvent::Error);
        }
        self.apply_effects(effects);
        if !was_open && is_open {
            self.send(UiCmd::ListPermanents);
            self.send(UiCmd::PermGroups);
            self.seen_focus = false;
        } else if was_open && !is_open {
            self.hide_unconfirmed = true;
            self.seen_focus = false;
        }
    }

    /// Apply shell effects in order: `Send` goes to the bridge (and is traced
    /// as `Paste { target }` when it is a `PasteClipTo`, with `target` true when
    /// it names a window, or an `ExecPermanent`, with `target` false);
    /// `AllowForeground` calls the `allow_foreground` hook; `Hide` and `Raise`
    /// do nothing; `ClearSearch` empties the search text and selects row 0;
    /// `SelectNewest` selects row 0; `FocusSearch` sets `focus_search`;
    /// `ApplyTheme` stores `dark` and, when present, the font scale in the
    /// Settings state; `ApplyKeys` applies the key overrides to the view-model
    /// (a bad override is ignored).
    #[allow(clippy::cast_possible_truncation)]
    fn apply_effects(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Send(cmd) => {
                    match &cmd {
                        UiCmd::PasteClipTo(_, target, _, _) => self.record(&TraceEvent::Paste {
                            target: target.is_some(),
                        }),
                        UiCmd::ExecPermanent(id) => {
                            self.exec_in_flight = Some(*id);
                            self.record(&TraceEvent::Paste { target: false });
                        }
                        _ => {}
                    }
                    self.send(cmd);
                }
                Effect::AllowForeground => (self.allow_foreground)(),
                Effect::Hide | Effect::Raise => {}
                Effect::ClearSearch => {
                    self.vm.query.clear();
                    self.vm.selected = 0;
                }
                Effect::SelectNewest => self.vm.selected = 0,
                Effect::FocusSearch => self.focus_search = true,
                Effect::ApplyTheme { dark, font_scale } => {
                    if let Ok(mut config) = self.config.lock() {
                        config.dark = dark;
                        if let Some(scale) = font_scale {
                            config.font_scale = scale as f32;
                        }
                    }
                }
                Effect::ApplyKeys(keys) => {
                    let _ = self.vm.apply_keys(&keys);
                }
            }
        }
    }

    /// Handle one bridge event: update the view-model and the Settings state as
    /// the old shell did, and hand the events the shell has rules for to
    /// `feed(Event::Bridge(..))`.
    /// - `Status`: `monitoring_paused` sets `paused` and the view-model's
    ///   paused flag, `version` goes to the Settings state; the shell hears it.
    /// - `History`: `PopupVm::apply_history`; `Changed` also sets the
    ///   current-clipboard slot to the first clip; the shell hears it (it
    ///   decides offline and the error line for a malformed reply).
    /// - `Deleted`, `Pinned`, `HistoryClip`, `Caps`, `Permanents`, `PermGroups`,
    ///   `PermanentSaved`, `PermanentDeleted`, `PermGroupsChanged`,
    ///   `BackupDone`, `RestoreDone`, `LogsTail`, `ConfigError`,
    ///   `MonitorPaused`, `ConfigApplied`, `Purged`: exactly what the old shell's
    ///   `drain_events` did (see git history of `main.rs`); `PasteDone` does
    ///   nothing (the popup hid at the pick).
    /// - `Config`: the Settings document is replaced and its edits cleared; the
    ///   shell hears it and answers with `ApplyTheme` and `ApplyKeys` effects.
    /// - `Error`: while the Settings window is open or an Apply is pending
    ///   (`pending_apply` not empty) the text goes to the Settings error line
    ///   and `pending_apply` is cleared, and the shell does not hear it;
    ///   otherwise the shell hears it (popup error line).
    /// - A sent `ExecPermanent` consumes its success or failure reply. Its
    ///   `Error` or `PasteUnavailable` becomes `PasteFailed` so the shell
    ///   reopens the popup with the error, even when Settings is open.
    /// - `ShowPopup`, `PasteFailed`, `PasteUnavailable`, `Offline`,
    ///   `StopFailed`, `Stopped`, `AnotherInstance`: the shell hears them.
    #[allow(clippy::too_many_lines)]
    fn on_bridge(&mut self, event: UiEvent) {
        let event = match event {
            UiEvent::Error(message) if self.exec_in_flight.is_some() => {
                self.exec_in_flight = None;
                UiEvent::PasteFailed(message)
            }
            UiEvent::PasteUnavailable if self.exec_in_flight.is_some() => {
                self.exec_in_flight = None;
                UiEvent::PasteFailed(strings::text("popup.paste_blocked").to_string())
            }
            UiEvent::PasteDone(id) => {
                if self.exec_in_flight == Some(id) {
                    self.exec_in_flight = None;
                }
                UiEvent::PasteDone(id)
            }
            event => event,
        };
        match event {
            UiEvent::Status(body) => {
                let paused = body
                    .get("monitoring_paused")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.paused = paused;
                self.vm.set_paused(paused);
                if let Some(version) = body.get("version").and_then(Value::as_str) {
                    if let Ok(mut config) = self.config.lock() {
                        config.version_text = version.to_string();
                    }
                }
                self.feed(Event::Bridge(UiEvent::Status(body)));
            }
            UiEvent::History(body) => {
                if self.vm.apply_history(&body) == popup::HistoryApply::Changed {
                    let current = self.vm.clips().first().cloned();
                    self.vm.set_current(current);
                }
                self.feed(Event::Bridge(UiEvent::History(body)));
            }
            UiEvent::Deleted(id) => self.vm.remove_clip(id),
            UiEvent::Pinned(id, pinned) => self.vm.apply_pin(id, pinned),
            UiEvent::PasteDone(_) | UiEvent::Purged(_) => {}
            UiEvent::HistoryClip(body) => {
                let clip = body.get("clip").and_then(|value| {
                    serde_json::from_value::<clipmonger_core::model::Clip>(value.clone()).ok()
                });
                if let Some(clip) = clip {
                    self.vm.set_current(Some(clip));
                }
            }
            UiEvent::Caps(body) => {
                if let Ok(mut config) = self.config.lock() {
                    config.caps_text = body.to_string();
                }
            }
            UiEvent::Permanents(body) => {
                if let Ok(mut config) = self.config.lock() {
                    config.perm_vm.apply_list(&body);
                }
            }
            UiEvent::PermGroups(body) => {
                if let Ok(mut config) = self.config.lock() {
                    config.perm_vm.apply_groups(&body);
                }
            }
            UiEvent::PermanentSaved => {
                if let Ok(mut config) = self.config.lock() {
                    config.perm_vm.draft = None;
                }
                self.send(UiCmd::ListPermanents);
                self.send(UiCmd::PermGroups);
            }
            UiEvent::PermanentDeleted(id) => {
                if let Ok(mut config) = self.config.lock() {
                    config.perm_vm.remove_confirmed(id);
                    config.perm_confirm_delete = None;
                }
                self.send(UiCmd::PermGroups);
            }
            UiEvent::PermGroupsChanged => {
                if let Ok(mut config) = self.config.lock() {
                    config.perm_group_new.clear();
                }
                self.send(UiCmd::ListPermanents);
                self.send(UiCmd::PermGroups);
            }
            UiEvent::BackupDone(path) => {
                if let Ok(mut config) = self.config.lock() {
                    config.error = format!("{} {path}", strings::text("config.backup_saved"));
                }
            }
            UiEvent::RestoreDone => {
                if let Ok(mut config) = self.config.lock() {
                    config.error = strings::text("config.restore_done").to_string();
                }
                self.send(UiCmd::ListHistory);
            }
            UiEvent::LogsTail(lines) => {
                if let Ok(mut config) = self.config.lock() {
                    config.log_lines = lines;
                }
            }
            UiEvent::ConfigError(message) => {
                if let Ok(mut config) = self.config.lock() {
                    config.error = message;
                }
            }
            UiEvent::MonitorPaused(paused) => {
                self.paused = paused;
                self.vm.set_paused(paused);
                if !paused {
                    self.pause_deadline = None;
                }
            }
            UiEvent::ConfigApplied(key) => {
                if let Ok(mut config) = self.config.lock() {
                    config.pending_apply.retain(|pending| pending != &key);
                    if let Some(doc) = &mut config.doc {
                        let wire_key = if key == "popup.keys" {
                            "popup.keys.*"
                        } else {
                            key.as_str()
                        };
                        doc.mark_applied(wire_key);
                    }
                    config.edits.remove(&key);
                }
            }
            UiEvent::Config(body) => {
                let doc = config_model::ConfigDoc::from_snapshot(&body);
                if let Ok(mut config) = self.config.lock() {
                    config.doc = Some(doc);
                    config.edits.clear();
                }
                self.feed(Event::Bridge(UiEvent::Config(body)));
            }
            UiEvent::Error(message) => {
                let settings_open = self.shell.desired().settings_open;
                let routed = self.config.lock().is_ok_and(|mut config| {
                    let routed = settings_open || !config.pending_apply.is_empty();
                    if routed {
                        config.error.clone_from(&message);
                        config.pending_apply.clear();
                    }
                    routed
                });
                if !routed {
                    self.feed(Event::Bridge(UiEvent::Error(message)));
                }
            }
            event @ (UiEvent::ShowPopup(_)
            | UiEvent::PasteFailed(_)
            | UiEvent::PasteUnavailable
            | UiEvent::Offline(_)
            | UiEvent::StopFailed(_)
            | UiEvent::Stopped
            | UiEvent::AnotherInstance) => self.feed(Event::Bridge(event)),
        }
    }

    /// Handle one tray action:
    /// - `Toggle`: `feed(Event::TrayToggle)`.
    /// - `OpenPopup`: a request push, `feed(Event::Bridge(UiEvent::ShowPopup(
    ///   ShowPopupEvent { source: "request", ..Default::default() })))`.
    /// - `TogglePause`: clear the pause deadline, send `MonitorToggle`.
    /// - `PauseFor1h`: unless paused, set the pause deadline [`PAUSE_PERIOD`]
    ///   from now and send `MonitorToggle`.
    /// - `OpenConfig`: `open_settings(None)`. `About`: `open_settings(Some("about"))`.
    /// - `Quit`: `feed(Event::Quit)`.
    fn on_tray(&mut self, action: &TrayAction) {
        match action {
            TrayAction::Toggle => self.feed(Event::TrayToggle),
            TrayAction::OpenPopup => self.feed(Event::Bridge(UiEvent::ShowPopup(ShowPopupEvent {
                source: "request".to_string(),
                ..ShowPopupEvent::default()
            }))),
            TrayAction::TogglePause => {
                self.pause_deadline = None;
                self.send(UiCmd::MonitorToggle);
            }
            TrayAction::PauseFor1h => {
                if !self.paused {
                    self.pause_deadline = Some(Instant::now() + PAUSE_PERIOD);
                    self.send(UiCmd::MonitorToggle);
                }
            }
            TrayAction::OpenConfig => self.open_settings(None),
            TrayAction::About => self.open_settings(Some("about")),
            TrayAction::Quit => self.feed(Event::Quit),
        }
    }

    /// Open the Settings window: `feed(Event::SettingsOpen)`, select the tab
    /// when one is given, send `ConfigGet`, `CapsGet`, `ListPermanents` and
    /// `PermGroups`.
    fn open_settings(&mut self, tab: Option<&str>) {
        self.feed(Event::SettingsOpen);
        if let Some(tab) = tab {
            if let Ok(mut config) = self.config.lock() {
                config.tab = tab.to_string();
            }
        }
        self.send(UiCmd::ConfigGet);
        self.send(UiCmd::CapsGet);
        self.send(UiCmd::ListPermanents);
        self.send(UiCmd::PermGroups);
    }

    /// Focus loss, as an edge on the popup window's focus. While the shell is
    /// open: the ROOT entry of the context's input viewports reporting
    /// `focused == Some(true)` sets `seen_focus`; reporting `Some(false)` after
    /// that clears it and calls `feed(Event::FocusLost)`. While the shell is
    /// closed `seen_focus` is cleared. Never read the egui focus events: a
    /// stale one from a hidden pass would hide a fresh popup.
    fn track_focus(&mut self, ctx: &egui::Context) {
        if !self.shell.desired().popup_open {
            self.seen_focus = false;
            return;
        }
        let focused = ctx.input(|input| {
            input
                .raw
                .viewports
                .get(&egui::ViewportId::ROOT)
                .and_then(|info| info.focused)
        });
        match focused {
            Some(true) => self.seen_focus = true,
            Some(false) if self.seen_focus => {
                self.seen_focus = false;
                self.feed(Event::FocusLost);
            }
            _ => {}
        }
    }

    /// Apply the Settings state's theme to `ctx` when it differs from what was
    /// applied to that kind of window: dark or light visuals and
    /// `ctx.set_zoom_factor(font_scale)` (so the OS scale still counts). Compare
    /// the scale by `f32::to_bits`.
    fn apply_theme(&mut self, ctx: &egui::Context, kind: WindowKind) {
        let (dark, scale) = self
            .config
            .lock()
            .map_or((true, 1.0), |config| (config.dark, config.font_scale));
        let slot = match kind {
            WindowKind::Popup => 0,
            WindowKind::Settings => 1,
        };
        let wanted = Some((dark, scale.to_bits()));
        if self.theme_applied[slot] == wanted {
            return;
        }
        self.theme_applied[slot] = wanted;
        ctx.set_theme(if dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        });
        ctx.set_visuals(if dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
        ctx.set_zoom_factor(scale);
    }

    /// Ask the runner for one pass at the earliest future deadline: the status
    /// poll (`last_poll + status_period`), the pause deadline while paused, and
    /// `Shell::next_deadline()` (shell clock, origin `start`). One
    /// `ctx.request_repaint_after(delay)`; no request when none is in the
    /// future; never `request_repaint()` and no fixed heartbeat (a hidden pass is
    /// not throttled).
    fn schedule_wake(&self, ctx: &egui::Context) {
        let now = Instant::now();
        let mut deadlines = vec![self.last_poll.checked_add(self.status_period)];
        if self.paused {
            deadlines.push(self.pause_deadline);
        }
        deadlines.push(
            self.shell
                .next_deadline()
                .and_then(|ms| self.start.checked_add(Duration::from_millis(ms))),
        );
        let next = deadlines
            .into_iter()
            .flatten()
            .filter_map(|deadline| deadline.checked_duration_since(now))
            .filter(|delay| !delay.is_zero())
            .min();
        if let Some(delay) = next {
            ctx.request_repaint_after(delay);
        }
    }

    /// Move the popup to the monitor centre once: when the ROOT viewport knows
    /// its monitor size, send `ViewportCommand::OuterPosition` with
    /// `placement::place(Center, ..)` over that monitor and set `centered`.
    fn center_popup(&mut self, ctx: &egui::Context) {
        let size = ctx.input(|input| {
            input
                .raw
                .viewports
                .get(&egui::ViewportId::ROOT)
                .and_then(|info| info.monitor_size)
        });
        if let Some(size) = size {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                popup_position_for_monitor(size),
            ));
            self.centered = true;
        }
    }
}

impl runner::App for PopupApp {
    /// Popup, in this order:
    /// 1. `hide_unconfirmed` set: clear it and `feed(Event::Hidden)`;
    /// 2. the ROOT viewport events contain Close: `feed(Event::CloseRequest)`;
    /// 3. drain at most [`DRAIN_CAP`] bridge events through `on_bridge`;
    /// 4. drain the tray closure through `on_tray`;
    /// 5. `track_focus`;
    /// 6. `Shell::tick(millis())` effects through `apply_effects`;
    /// 7. timers: send `Status` once `status_period` has passed since
    ///    `last_poll` (and reset it); when the pause deadline has passed while
    ///    paused, clear it and send `MonitorToggle`;
    /// 8. `vm.set_offline(shell.offline())`, `apply_theme`;
    /// 9. `center_popup` while not centered;
    /// 10. `schedule_wake`.
    ///
    /// Settings: `apply_theme`; when the ROOT viewport events contain Close,
    /// `feed(Event::SettingsClose)`.
    fn logic(&mut self, ctx: &egui::Context, kind: WindowKind) {
        let close_requested = ctx.input(|input| {
            input
                .raw
                .viewports
                .get(&egui::ViewportId::ROOT)
                .is_some_and(egui::ViewportInfo::close_requested)
        });
        match kind {
            WindowKind::Popup => {
                if std::mem::take(&mut self.hide_unconfirmed) {
                    self.feed(Event::Hidden);
                }
                if close_requested {
                    self.feed(Event::CloseRequest);
                }
                for _ in 0..DRAIN_CAP {
                    let Ok(event) = self.events.try_recv() else {
                        break;
                    };
                    self.on_bridge(event);
                }
                for action in (self.tray)() {
                    self.on_tray(&action);
                }
                self.track_focus(ctx);
                let effects = self.shell.tick(self.millis());
                self.apply_effects(effects);
                if self.last_poll.elapsed() >= self.status_period {
                    self.last_poll = Instant::now();
                    self.send(UiCmd::Status);
                }
                if self.paused && self.pause_deadline.is_some_and(|at| Instant::now() >= at) {
                    self.pause_deadline = None;
                    self.send(UiCmd::MonitorToggle);
                }
                self.vm.set_offline(self.shell.offline());
                self.apply_theme(ctx, kind);
                if !self.centered {
                    self.center_popup(ctx);
                }
                self.schedule_wake(ctx);
            }
            WindowKind::Settings => {
                self.apply_theme(ctx, kind);
                if close_requested {
                    self.feed(Event::SettingsClose);
                }
            }
        }
    }

    /// Popup: the contents while the shell is open, an empty panel while it is
    /// closed (the daemon shows the window before the push arrives, so a
    /// closed shell must not paint stale rows). Settings: the Settings body.
    fn ui(&mut self, ui: &mut egui::Ui, kind: WindowKind) {
        // The runner creates this Ui before logic applies the configured theme.
        ui.set_style(ui.ctx().global_style());
        match kind {
            WindowKind::Popup => {
                if self.shell.desired().popup_open {
                    self.render(ui);
                } else {
                    egui::CentralPanel::default().show(ui, |_| {});
                }
            }
            WindowKind::Settings => render_config_viewport(ui, &self.config, &self.cmds),
        }
    }

    fn desired(&self) -> runner::Desired {
        let want = self.shell.desired();
        runner::Desired {
            popup_open: want.popup_open,
            settings_open: want.settings_open,
            exit: want.exit,
        }
    }
}

/// The popup centre for a monitor of `size` points.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn popup_position_for_monitor(size: egui::Vec2) -> egui::Pos2 {
    let monitor = placement::monitor_rect_from_size(Some((size.x as u32, size.y as u32)));
    let (px, py) = placement::place(
        placement::Placement::Center,
        (0, 0),
        &[monitor],
        placement::POPUP_DEFAULT,
    );
    egui::pos2(px as f32, py as f32)
}

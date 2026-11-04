//! The popup application on the runner: the glue between the bridge, the
//! tray, the shell state machine and the two egui windows (popup and
//! Settings). `logic` runs on every pass, hidden or not; `ui` only paints.

mod compact;
mod popup_ui;
mod settings_ui;
mod thumbnail_ui;
pub mod trace;

use std::any::Any;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use clipmonger_ipc::proto::methods::ShowPopupEvent;
use serde_json::Value;

pub use settings_ui::{render_config_viewport, ConfigWindowState};
pub use trace::{Trace, TraceEvent};

use crate::ipc_bridge::{self, UiCmd, UiEvent};
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
    /// One event peeked after the drain cap; handled first on the next pass.
    pending_event: Option<UiEvent>,
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
    /// Placement pending for next desktop context.
    placement_pending: bool,
    placement_waiting_for_geometry: bool,
    placement: placement::Placement,
    pending_physical_pos: Option<(i32, i32)>,
    /// Put the keyboard focus into the search box at the next render.
    focus_search: bool,
    /// Theme last applied to the contexts: popup, settings (dark, font scale bits).
    theme_applied: [Option<(bool, u32)>; 2],
    preview_width: u32,
    preview_lines: u32,
    pending_activation: bool,
    pending_activation_metadata: Option<runner::PopupActivation>,
    pending_reveal: Option<(Option<String>, usize, uuid::Uuid)>,
    last_reveal: Option<(Option<String>, usize, uuid::Uuid)>,
    last_query: String,
    thumbnails: Option<thumbnail_ui::Thumbnails>,
    compact: compact::Compact,
    current_generation: Option<u64>,
    current_pending: Option<u64>,
    current_wanted: bool,
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
            pending_event: None,
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
            placement_pending: open,
            placement_waiting_for_geometry: false,
            placement: placement::Placement::Center,
            pending_physical_pos: None,
            focus_search: open,
            theme_applied: [None, None],
            preview_width: 80,
            preview_lines: 3,
            pending_activation: false,
            pending_activation_metadata: None,
            pending_reveal: None,
            last_reveal: None,
            last_query: String::new(),
            thumbnails: None,
            compact: compact::Compact::new(),
            current_generation: Some(0),
            current_pending: None,
            current_wanted: false,
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

    /// Attach the owned thumbnail worker to this popup.
    pub fn attach_thumbnails(&mut self, worker: crate::thumbnail_worker::Worker) {
        self.thumbnails = Some(thumbnail_ui::Thumbnails::new(worker));
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

    pub fn vm_mut(&mut self) -> &mut popup::PopupVm {
        &mut self.vm
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
    ///    new or changed, or that an event of the kind `PasteFailed`,
    ///    `ExecFailed` or `Error` set again, is `Error` while the popup is open;
    /// 2. `apply_effects`;
    /// 3. closed to open: sends `ListPermanents` and `PermGroups` (the shell
    ///    sends only `ListHistory`) and clears `seen_focus`; open to closed:
    ///    sets `hide_unconfirmed` and clears `seen_focus`.
    fn feed(&mut self, event: Event) {
        self.feed_at(event, self.millis());
    }

    /// Feed an event with its occurrence time on the shell clock.
    fn feed_at(&mut self, event: Event, at: Millis) {
        let activation = match &event {
            Event::Bridge(UiEvent::ShowPopup(show)) if !show.toggle => {
                Some(runner::PopupActivation {
                    x11_time: show.x11_time,
                    activation_token: show.activation_token.clone(),
                })
            }
            _ => None,
        };
        let was_open = self.shell.desired().popup_open;
        let old_error = self.shell.error().map(str::to_owned);
        let error_kind = matches!(
            &event,
            Event::Bridge(UiEvent::PasteFailed(_) | UiEvent::ExecFailed(..) | UiEvent::Error(_))
        );
        let source = trace::open_source(&event);
        let reason = trace::hide_reason(&event);
        let effects = self.shell.handle(event, at);
        let is_open = self.shell.desired().popup_open;
        if is_open {
            if let Some(activation) = activation {
                self.pending_activation_metadata = Some(activation);
            }
        } else {
            self.pending_activation_metadata = None;
        }
        if !was_open && is_open {
            let context = self.shell.context();
            self.record(&TraceEvent::Open {
                source,
                target: context.target.is_some(),
                gesture: context.gesture.is_some(),
            });
            // Pending placement for closed-to-open transition; not for raise while open.
            self.placement_pending = true;
            self.placement_waiting_for_geometry = false;
        } else if was_open && !is_open {
            self.pending_physical_pos = None;
            self.placement_pending = false;
            self.placement_waiting_for_geometry = false;
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
            if let Some(thumbs) = &mut self.thumbnails {
                thumbs.revoke_visibility();
            }
            if let Some(cancel) = self.compact.reset(self.vm.query.clone(), false, false) {
                self.send(cancel);
            }
            self.invalidate_current();
            self.hide_unconfirmed = true;
            self.seen_focus = false;
        }
    }

    /// Apply shell effects in order: `Send` goes to the bridge (and is traced
    /// as `Paste { target }` when it is a `PasteClipTo`, with `target` true when
    /// it names a window, or an `ExecPermanent`, with `target` false);
    /// `AllowForeground` calls the `allow_foreground` hook; `Hide` clears a
    /// pending activation and `Raise` sets it; `ClearSearch` empties the search
    /// text and selects row 0;
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
                        UiCmd::ExecPermanent(_) => {
                            self.record(&TraceEvent::Paste { target: false });
                        }
                        _ => {}
                    }
                    if matches!(cmd, UiCmd::ListHistory) {
                        self.reset_compact(false);
                        if self.shell.desired().popup_open {
                            self.refresh_current();
                        }
                    } else {
                        self.send(cmd);
                    }
                }
                Effect::AllowForeground => (self.allow_foreground)(),
                Effect::Hide => self.pending_activation = false,
                Effect::Raise => self.pending_activation = true,
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

    fn invalidate_current(&mut self) {
        self.current_generation = self.current_generation.and_then(|n| n.checked_add(1));
        self.current_wanted = false;
    }

    fn refresh_current(&mut self) {
        self.current_generation = self.current_generation.and_then(|n| n.checked_add(1));
        self.current_wanted = self.current_generation.is_some();
        self.send_current();
    }

    fn send_current(&mut self) {
        if self.current_wanted && self.current_pending.is_none() {
            let Some(tag) = self.current_generation else {
                return;
            };
            self.current_pending = Some(tag);
            self.current_wanted = false;
            self.send(UiCmd::CompactCurrent(tag));
        }
    }

    fn reset_compact(&mut self, restore: bool) {
        if restore {
            self.vm.set_current(None);
        }
        if let Some(thumbs) = &mut self.thumbnails {
            thumbs.revoke_visibility();
        }
        self.vm.set_compact_rows(Vec::new());
        if let Some(cancel) = self.compact.reset(
            self.vm.query.clone(),
            self.shell.desired().popup_open,
            restore,
        ) {
            self.send(cancel);
        }
        if let Some(cmd) = self.compact.command() {
            self.send(cmd);
        }
    }

    fn restore_error_message(&self, message: &str) -> String {
        self.config
            .lock()
            .ok()
            .and_then(|config| config.restore_notice.clone())
            .map_or_else(
                || message.to_owned(),
                |notice| format!("{notice} {message}"),
            )
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
    /// - `ConfigSetFailed`: while the Settings window is open the text goes
    ///   under that setting (or to the error line for a key without a row),
    ///   its text field takes focus and only its key leaves `pending_apply`;
    ///   otherwise the shell hears it as `Error`.
    /// - `Error`: while the Settings window is open or an Apply is pending
    ///   (`pending_apply` not empty) the text goes to the Settings error line
    ///   and `pending_apply` is cleared, and the shell does not hear it;
    ///   otherwise the shell hears it (popup error line).
    /// - `ShowPopup`, `PasteFailed`, `ExecFailed`, `Offline`, `StopFailed`,
    ///   `Stopped`, `AnotherInstance`: the shell hears them, including paste
    ///   and execution failures while Settings is open.
    #[allow(clippy::too_many_lines)]
    fn on_bridge(&mut self, event: UiEvent) {
        match event {
            UiEvent::Status(body) => {
                let recovering = self.shell.offline();
                let paused = body
                    .get("monitoring_paused")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.paused = paused;
                self.vm.set_paused(paused);
                if let Ok(mut config) = self.config.lock() {
                    if let Some(version) = body.get("version").and_then(Value::as_str) {
                        config.version_text = version.to_string();
                    }
                    config.autostart_portable = body
                        .get("autostart_portable")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                }
                self.feed(Event::Bridge(UiEvent::Status(body)));
                if recovering && self.shell.desired().popup_open {
                    self.reset_compact(false);
                    self.refresh_current();
                }
            }
            UiEvent::CompactHistory(tag, outcome) => {
                let transport_error = self.shell.desired().popup_open
                    && self.compact.accepts(&tag)
                    && outcome
                        .as_ref()
                        .is_err_and(crate::ipc_bridge::is_transport_error);
                let message = outcome.as_ref().err().map(|error| error.message.clone());
                let (rows, cancel) = self.compact.result(tag, outcome);
                if transport_error {
                    self.feed(Event::Bridge(UiEvent::Offline(message.unwrap_or_default())));
                }
                if let Some(cancel) = cancel {
                    self.send(cancel);
                }
                if let Some(rows) = rows {
                    if let Some(thumbs) = &mut self.thumbnails {
                        thumbs.revoke_visibility();
                    }
                    self.vm.set_compact_rows(rows);
                }
            }
            UiEvent::CompactCurrent(tag, outcome) => {
                if self.current_pending == Some(tag) {
                    self.current_pending = None;
                    if Some(tag) == self.current_generation && self.shell.desired().popup_open {
                        if let Err(error) = &outcome {
                            if crate::ipc_bridge::is_transport_error(error) {
                                self.feed(Event::Bridge(UiEvent::Offline(error.message.clone())));
                            }
                        }
                        if let Ok(body) = outcome {
                            if let Some(clips) = body.get("clips").and_then(Value::as_array) {
                                if clips.len() <= 1 {
                                    if clips.is_empty() {
                                        self.vm.set_current(None);
                                    } else if let Ok(clip) =
                                        serde_json::from_value::<clipmonger_core::model::Clip>(
                                            clips[0].clone(),
                                        )
                                    {
                                        self.vm.set_current(Some(clip));
                                    }
                                }
                            }
                        }
                    }
                    self.send_current();
                }
            }
            UiEvent::History(body) => {
                if self.vm.apply_history(&body) == popup::HistoryApply::Changed {
                    if let Some(thumbs) = &mut self.thumbnails {
                        thumbs.revoke_visibility();
                    }
                    let current = self.vm.clips().first().cloned();
                    self.vm.set_current(current);
                }
                self.feed(Event::Bridge(UiEvent::History(body)));
            }
            UiEvent::Deleted(id) => {
                if let Some(thumbs) = &mut self.thumbnails {
                    thumbs.reset();
                }
                self.vm.remove_clip(id);
                if self.vm.current().is_some_and(|clip| clip.id == id) {
                    self.vm.set_current(None);
                }
            }
            UiEvent::Pinned(id, pinned) => self.vm.apply_pin(id, pinned),
            UiEvent::CompactCancelled | UiEvent::PasteDone(_) | UiEvent::Purged(_) => {}
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
                    if config.perm_source.is_some()
                        && self.shell.desired().popup_open
                        && !self.shell.desired().settings_open
                    {
                        config.perm_vm.clamp_for(&self.vm.query);
                        config.perm_source = Some(config.perm_vm.active_group);
                    }
                }
            }
            UiEvent::PermGroups(body) => {
                if let Ok(mut config) = self.config.lock() {
                    config.perm_vm.apply_groups(&body);
                    if config.perm_source.is_some()
                        && self.shell.desired().popup_open
                        && !self.shell.desired().settings_open
                    {
                        config.perm_vm.clamp_for(&self.vm.query);
                        config.perm_source = Some(config.perm_vm.active_group);
                    }
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
                if self.shell.desired().settings_open {
                    if let Ok(mut config) = self.config.lock() {
                        config.error = format!("{} {path}", strings::text("config.backup_saved"));
                    }
                }
            }
            UiEvent::RestoreDone { warning } => {
                if let Some(thumbs) = &mut self.thumbnails {
                    thumbs.reset();
                }
                self.reset_compact(true);
                if self.shell.desired().popup_open {
                    self.refresh_current();
                }
                let mut message = strings::text("config.restore_done").to_string();
                if let Some(ref warning) = warning {
                    let id = match warning {
                        ipc_bridge::RestoreWarning::ApplyFailed => "config.restore_warning_apply",
                        ipc_bridge::RestoreWarning::Unavailable => {
                            "config.restore_warning_unavailable"
                        }
                        ipc_bridge::RestoreWarning::Portable => "config.restore_warning_portable",
                        ipc_bridge::RestoreWarning::Unknown => "config.restore_warning_unknown",
                    };
                    message.push(' ');
                    message.push_str(strings::text(id));
                }
                if let Ok(mut config) = self.config.lock() {
                    config.restore_notice = warning.map(|_| message.clone());
                    if self.shell.desired().settings_open {
                        config.error.clone_from(&message);
                    }
                }
                if !self.shell.desired().settings_open {
                    self.feed(Event::Bridge(UiEvent::Error(message)));
                }
                self.send(UiCmd::ConfigGetAfterRestore);
            }
            UiEvent::RestoreFailed(message) => {
                self.on_bridge(UiEvent::ConfigError(message));
                self.send(UiCmd::ConfigGetAfterRestore);
            }
            UiEvent::RestoredConfigError(message) => {
                if let Ok(mut config) = self.config.lock() {
                    config.restore_pending = false;
                }
                self.on_bridge(UiEvent::ConfigError(message));
            }
            UiEvent::RestoredConfig(body) => {
                if let Ok(mut config) = self.config.lock() {
                    config.restore_pending = false;
                    config.dark = settings_ui::DEFAULT_DARK;
                    config.font_scale = settings_ui::DEFAULT_FONT_SCALE;
                }
                self.on_bridge(UiEvent::Config(body));
            }
            UiEvent::LogsTail {
                source,
                lines,
                sink_error,
            } => {
                if let Ok(mut config) = self.config.lock() {
                    if config.log_source == source {
                        config.log_lines = lines;
                        config.log_error = sink_error.unwrap_or_default();
                    }
                }
            }
            UiEvent::LogsTailError { source, message } => {
                if let Ok(mut config) = self.config.lock() {
                    if config.log_source == source {
                        config.log_error = message;
                    }
                }
            }
            UiEvent::ConfigError(message) => {
                let message = self.restore_error_message(&message);
                if self.shell.desired().settings_open {
                    if let Ok(mut config) = self.config.lock() {
                        config.error = message;
                    }
                } else {
                    self.feed(Event::Bridge(UiEvent::Error(message)));
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
                    let row_key = if key == "popup.keys" {
                        "popup.keys.*"
                    } else {
                        key.as_str()
                    };
                    config.field_errors.remove(row_key);
                }
            }
            UiEvent::ConfigSetFailed { key, message } => {
                let message = self.restore_error_message(&message);
                let key = if key == "popup.keys" {
                    "popup.keys.*".to_string()
                } else {
                    key
                };
                let settings_open = self.shell.desired().settings_open;
                let routed = self.config.lock().is_ok_and(|mut config| {
                    if settings_open {
                        config.pending_apply.retain(|pending| pending != &key);
                        if config_model::ROWS.iter().any(|row| row.key == key) {
                            config.field_errors.insert(key.clone(), message.clone());
                            config.focus_field = Some(key.clone());
                        } else {
                            config.error.clone_from(&message);
                        }
                    }
                    settings_open
                });
                if !routed {
                    self.feed(Event::Bridge(UiEvent::Error(message)));
                }
            }
            UiEvent::Config(body) => {
                if self
                    .config
                    .lock()
                    .is_ok_and(|config| config.restore_pending)
                {
                    return;
                }
                let doc = config_model::ConfigDoc::from_snapshot(&body);
                let width = doc
                    .get("popup.preview_width")
                    .and_then(Value::as_u64)
                    .map_or(80, |n| u32::try_from(n).unwrap_or(80));
                let lines = doc
                    .get("popup.preview_lines")
                    .and_then(Value::as_u64)
                    .map_or(3, |n| u32::try_from(n).unwrap_or(3));
                let placement_str = doc
                    .get("popup.placement")
                    .and_then(Value::as_str)
                    .unwrap_or("center");
                let new_placement = match placement_str {
                    "cursor" => placement::Placement::Cursor,
                    _ => placement::Placement::Center,
                };
                let placement_changed = new_placement != self.placement;
                if let Ok(mut config) = self.config.lock() {
                    let config = &mut *config;
                    config.doc = Some(doc);
                    // Keep the typed text of settings that show an error.
                    let errors = &config.field_errors;
                    config.edits.retain(|key, _| errors.contains_key(key));
                }
                self.preview_width = width;
                self.preview_lines = lines;
                if placement_changed {
                    self.placement = new_placement;
                    self.placement_waiting_for_geometry = false;
                    // Reposition once for an effective mode change while open.
                    if self.shell.desired().popup_open {
                        self.placement_pending = true;
                    }
                }
                self.feed(Event::Bridge(UiEvent::Config(body)));
            }
            UiEvent::Error(message) => {
                let message = self.restore_error_message(&message);
                let settings_open = self.shell.desired().settings_open;
                let routed = self.config.lock().is_ok_and(|mut config| {
                    let routed = settings_open;
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
            | UiEvent::ExecFailed(..)
            | UiEvent::Offline(_)
            | UiEvent::StopFailed(_)
            | UiEvent::Stopped
            | UiEvent::AnotherInstance) => self.feed(Event::Bridge(event)),
        }
    }

    /// Handle one tray action:
    /// - `Toggle`: `feed(Event::TrayToggle)`.
    /// - `ToggleAt`: `feed_at(Event::TrayToggle, press time on the shell clock)`.
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
            TrayAction::ToggleAt(pressed) => {
                let at = u64::try_from(pressed.saturating_duration_since(self.start).as_millis())
                    .unwrap_or(u64::MAX);
                self.feed_at(Event::TrayToggle, at);
            }
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

    /// Ask the runner for one pass at the earliest deadline: the status
    /// poll (`last_poll + status_period`), the pause deadline while paused, and
    /// `Shell::next_deadline()` (shell clock, origin `start`). One
    /// `ctx.request_repaint_after(delay)`; clamp elapsed deadlines to 1 ms so
    /// a deadline that passed after tick is still served. No fixed heartbeat;
    /// a zero status period arms no timer (manual per-pass polling).
    fn schedule_wake(&self, ctx: &egui::Context) {
        let now = Instant::now();
        // A zero poll period is a manual per-pass mode and arms no timer.
        let mut deadlines = vec![(!self.status_period.is_zero())
            .then(|| self.last_poll.checked_add(self.status_period))
            .flatten()];
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
            .map(|deadline| {
                deadline
                    .saturating_duration_since(now)
                    .max(Duration::from_millis(1))
            })
            .min();
        if let Some(delay) = next {
            ctx.request_repaint_after(delay);
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn handle_pending_placement(&mut self, ctx: &egui::Context) {
        if let Some(pending_pos) = self.pending_physical_pos.take() {
            let ppp = ctx.pixels_per_point();
            let egui_pos = egui::pos2(pending_pos.0 as f32 / ppp, pending_pos.1 as f32 / ppp);
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui_pos));
        }
    }
}

impl PopupApp {
    fn compute_reveal_key(&self) -> Option<(Option<String>, usize, uuid::Uuid)> {
        let config = self.config.lock().ok()?;
        if config.perm_source.is_some() {
            let group = config
                .perm_vm
                .group_names()
                .get(config.perm_vm.active_group)?
                .clone();
            Some((
                Some(group),
                config.perm_vm.selected,
                config.perm_vm.selected_id(&self.vm.query)?,
            ))
        } else {
            Some((
                None,
                self.vm.selected,
                self.vm
                    .compact_rows()
                    .and_then(|rows| rows.get(self.vm.selected))
                    .map(|row| row.id)
                    .or_else(|| self.vm.clip_at(self.vm.selected).map(|clip| clip.id))?,
            ))
        }
    }

    fn request_reveal(&mut self, ctx: &egui::Context) {
        self.pending_reveal = self.compute_reveal_key();
        if self.pending_reveal.is_some() {
            ctx.request_repaint();
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
    /// 9. emit a position supplied by the runner for this open;
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
                if let Some(thumbs) = &mut self.thumbnails {
                    thumbs.context(ctx);
                }
                if std::mem::take(&mut self.hide_unconfirmed) {
                    self.feed(Event::Hidden);
                }
                if close_requested {
                    self.feed(Event::CloseRequest);
                }
                for _ in 0..DRAIN_CAP {
                    let Some(event) = self
                        .pending_event
                        .take()
                        .or_else(|| self.events.try_recv().ok())
                    else {
                        break;
                    };
                    self.on_bridge(event);
                }
                if let Some(cmd) = self.compact.command() {
                    self.send(cmd);
                }
                self.pending_event = self.events.try_recv().ok();
                if self.pending_event.is_some() {
                    ctx.request_repaint();
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
                if let Some(thumbs) = &mut self.thumbnails {
                    if !self.shell.desired().popup_open {
                        thumbs.revoke_visibility();
                    }
                    thumbs.drain();
                }
                self.vm.set_offline(self.shell.offline());
                self.apply_theme(ctx, kind);
                self.handle_pending_placement(ctx);
                self.schedule_wake(ctx);
            }
            WindowKind::Settings => {
                self.apply_theme(ctx, kind);
                if close_requested {
                    self.feed(Event::SettingsClose);
                    if let Ok(mut config) = self.config.lock() {
                        config.error.clear();
                        config.pending_apply.clear();
                    }
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
                    if let Some(thumbs) = &mut self.thumbnails {
                        thumbs.revoke_visibility();
                    }
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

    fn preserve_native_super(&self, kind: runner::WindowKind) -> bool {
        kind == runner::WindowKind::Popup
    }

    fn native_key_override(
        &self,
        kind: runner::WindowKind,
        key: egui::Key,
        modifiers: egui::Modifiers,
    ) -> bool {
        if kind != runner::WindowKind::Popup {
            return false;
        }
        let Some(key) = Self::egui_key_to_chord_name(key) else {
            return false;
        };
        self.vm.has_binding(&crate::keymap::KeyEvent {
            key,
            ctrl: modifiers.ctrl,
            alt: modifiers.alt,
            shift: modifiers.shift,
            super_key: modifiers.mac_cmd,
        })
    }

    fn take_popup_activation_metadata(&mut self) -> Option<runner::PopupActivation> {
        self.pending_activation_metadata.take()
    }

    fn take_popup_activation(&mut self) -> bool {
        std::mem::take(&mut self.pending_activation)
    }

    fn wants_desktop_context(&self) -> bool {
        self.shell.desired().popup_open && self.placement_pending
    }

    fn provide_desktop_context(&mut self, ctx: crate::placement::DesktopContext) {
        if !self.wants_desktop_context() {
            return;
        }
        self.placement_pending = false;
        self.placement_waiting_for_geometry = ctx.supports_global_position
            && !ctx
                .monitors
                .iter()
                .any(|monitor| monitor.w > 0 && monitor.h > 0);
        self.pending_physical_pos = if self.placement_waiting_for_geometry {
            None
        } else {
            crate::placement::place_with_context(self.placement, &ctx)
        };
    }

    fn desktop_geometry_changed(&mut self) {
        if self.shell.desired().popup_open && self.placement_waiting_for_geometry {
            self.placement_pending = true;
            self.placement_waiting_for_geometry = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_x5_elapsed_deadline_requests_a_prompt_follow_up() {
        let (cmds, _) = mpsc::channel();
        let (_, events) = mpsc::channel();
        let mut app = PopupApp::new(AppParts {
            profile: PathBuf::from("deadline-rig"),
            cmds,
            events,
            tray: Box::new(Vec::new),
            hold: None,
            open: false,
            status_period: Duration::from_secs(3600),
            trace: None,
            allow_foreground: Box::new(|| {}),
        });
        app.paused = true;
        app.pause_deadline = Some(app.start);
        let ctx = egui::Context::default();
        let raw = egui::RawInput::default();
        for _ in 0..3 {
            let _ = ctx.run_logic(&raw, |_| {});
        }
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        ctx.set_request_repaint_callback(move |info| {
            captured.lock().expect("requests").push(info.delay);
        });
        let _ = ctx.run_logic(&raw, |ctx| app.schedule_wake(ctx));
        assert_eq!(
            *requests.lock().expect("requests"),
            vec![Duration::from_millis(1)],
            "a deadline that elapsed after tick must still wake the runner"
        );
    }
    #[test]
    fn fix_x7_held_tray_press_does_not_reopen_a_focus_loss_hide() {
        use crate::tray_logic::{left_button_action, LeftButtonEvent};
        for focus_first in [true, false] {
            let (cmds, _) = mpsc::channel();
            let (_, events) = mpsc::channel();
            let mut app = PopupApp::new(AppParts {
                profile: PathBuf::from("tray-rig"),
                cmds,
                events,
                tray: Box::new(Vec::new),
                hold: None,
                open: true,
                status_period: STATUS_PERIOD,
                trace: None,
                allow_foreground: Box::new(|| {}),
            });
            app.start = Instant::now()
                .checked_sub(Duration::from_secs(1))
                .expect("tray clock origin");
            let pressed = app.start + Duration::from_millis(10);
            if focus_first {
                app.feed_at(Event::FocusLost, 10);
            }
            if let Some(action) = left_button_action(LeftButtonEvent::Down, pressed) {
                app.on_tray(&action);
            }
            if !focus_first {
                app.feed_at(Event::FocusLost, 10);
            }
            if let Some(action) =
                left_button_action(LeftButtonEvent::Up, pressed + Duration::from_secs(1))
            {
                app.on_tray(&action);
            }
            assert!(!app.shell.desired().popup_open, "focus_first={focus_first}");
        }
    }
}

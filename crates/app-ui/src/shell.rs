//! Shell state machine: the popup's logical state and the rules that turn
//! events into effects. Pure: no window, clock or I/O. The host feeds
//! [`Event`]s to [`Shell::handle`] and the time to [`Shell::tick`], applies the
//! returned [`Effect`]s in order and reads [`Shell::desired`]. The shell never
//! asks for a repaint; the host wakes at [`Shell::next_deadline`].

use std::collections::BTreeMap;

use serde_json::Value;
use uuid::Uuid;

use clipmonger_ipc::proto::methods::ShowPopupEvent;

use crate::config_model::ConfigDoc;
use crate::ipc_bridge::{UiCmd, UiEvent};
use crate::popup;
use crate::strings;

/// Monotonic milliseconds on a clock the host chooses; only differences matter.
pub type Millis = u64;

/// A tray click at most this long after a focus-loss hide keeps the popup
/// hidden: the click itself took the focus.
pub const TRAY_GUARD_MS: Millis = 300;

/// A pick without a paste target is sent this long after the popup is
/// confirmed hidden.
pub const PASTE_DELAY_MS: Millis = 150;

/// Quit exits at the latest this long after the stop request.
pub const QUIT_TIMEOUT_MS: Millis = 6_000;

/// Where a paste goes: the window the hotkey was pressed in. Both fields are
/// `None` for a popup that was not opened by a hotkey press.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Context {
    /// Paste target of the press (Windows HWND value, X11 window id).
    pub target: Option<u64>,
    /// Single-use gesture token minted for the press.
    pub gesture: Option<u64>,
}

/// The window state the shell wants; the same fields as the runner's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Desired {
    /// The popup is open.
    pub popup_open: bool,
    /// The Settings window is open.
    pub settings_open: bool,
    /// Leave the event loop.
    pub exit: bool,
}

/// One input to the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A bridge event as it arrives (replies and daemon pushes). Handled:
    /// `ShowPopup`, `PasteFailed`, `ExecFailed`, `Error`, `Offline`,
    /// `StopFailed`, `Status`, `History`, `Config`, `Stopped` and
    /// `AnotherInstance`; every other variant changes nothing (a `PasteDone`
    /// reply never shows or hides the popup).
    Bridge(UiEvent),
    /// The dismiss key (Esc by default) was pressed in the popup.
    Dismiss,
    /// The popup lost the keyboard focus.
    FocusLost,
    /// The window manager asked the popup window to close.
    CloseRequest,
    /// Left click on the tray icon.
    TrayToggle,
    /// The user picked a clip. `picked_ms` is the wall-clock unix time of the
    /// pick in milliseconds (the daemon refuses a paste that would land late).
    Pick {
        /// The picked clip.
        id: Uuid,
        /// Unix milliseconds of the pick.
        picked_ms: i64,
    },
    /// The user picked a permanent clip.
    PickPermanent {
        /// The picked permanent.
        id: Uuid,
    },
    /// The host confirms the popup window is hidden now.
    Hidden,
    /// Any key press in the popup.
    Key,
    /// F5.
    Refresh,
    /// The Settings window was opened.
    SettingsOpen,
    /// The Settings window was closed.
    SettingsClose,
    /// Tray Quit.
    Quit,
}

/// One action for the host, applied in the order returned.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Send a command to the bridge.
    Send(UiCmd),
    /// Let any process take the foreground (Windows: `AllowSetForegroundWindow`
    /// with `ASFW_ANY`, nothing elsewhere). Runs while the popup is still the
    /// foreground window, so before [`Effect::Hide`].
    AllowForeground,
    /// The popup was hidden (the state already says so, see [`Shell::desired`]).
    Hide,
    /// The popup was already open: bring it to the front again.
    Raise,
    /// Empty the search box.
    ClearSearch,
    /// Select the newest entry, the first row.
    SelectNewest,
    /// Put the keyboard focus into the search box.
    FocusSearch,
    /// Apply the saved theme.
    ApplyTheme {
        /// Dark unless the saved mode is `"light"`.
        dark: bool,
        /// The saved font scale, when there is one.
        font_scale: Option<f64>,
    },
    /// Apply the saved popup key overrides.
    ApplyKeys(BTreeMap<String, String>),
}

/// A pick without a paste target, waiting for the popup to be confirmed hidden.
#[derive(Clone, Debug)]
struct PendingPaste {
    id: Uuid,
    picked_ms: i64,
    gesture: Option<u64>,
}

/// The shell state machine.
#[derive(Clone, Debug)]
pub struct Shell {
    want: Desired,
    context: Context,
    error: Option<String>,
    offline: bool,
    dismiss_on_focus_loss: bool,
    focus_hide_at: Option<Millis>,
    hidden_at: Option<Millis>,
    pending_paste: Option<PendingPaste>,
    pending_exec: Option<Uuid>,
    quit_at: Option<Millis>,
}

impl Shell {
    /// A shell with the popup open or closed. Focus-loss dismissal is on until
    /// a config reply says otherwise; no error, not offline, no context.
    #[must_use]
    pub fn new(open: bool) -> Self {
        Self {
            want: Desired {
                popup_open: open,
                settings_open: false,
                exit: false,
            },
            context: Context::default(),
            error: None,
            offline: false,
            dismiss_on_focus_loss: true,
            focus_hide_at: None,
            hidden_at: None,
            pending_paste: None,
            pending_exec: None,
            quit_at: None,
        }
    }

    /// The commands to send at start, in this order: `Status`, `ConfigGet`,
    /// `ListHistory`, `ListPermanents`, `PermGroups`.
    #[must_use]
    pub fn start() -> Vec<Effect> {
        vec![
            Effect::Send(UiCmd::Status),
            Effect::Send(UiCmd::ConfigGet),
            Effect::Send(UiCmd::ListHistory),
            Effect::Send(UiCmd::ListPermanents),
            Effect::Send(UiCmd::PermGroups),
        ]
    }

    /// Open the popup with `context`, reset what an open resets and return
    /// the open sequence.
    fn open_popup(&mut self, context: Context) -> Vec<Effect> {
        self.want.popup_open = true;
        self.context = context;
        self.error = None;
        self.focus_hide_at = None;
        self.hidden_at = None;
        self.pending_paste = None;
        self.pending_exec = None;
        vec![
            Effect::ClearSearch,
            Effect::Send(UiCmd::ListHistory),
            Effect::SelectNewest,
            Effect::FocusSearch,
        ]
    }

    /// Close the popup and clear its context; `focus_loss_at` is the time to
    /// store for the tray guard when the hide came from a focus loss.
    fn hide(&mut self, focus_loss_at: Option<Millis>) -> Vec<Effect> {
        self.want.popup_open = false;
        self.context = Context::default();
        self.hidden_at = None;
        self.focus_hide_at = focus_loss_at;
        vec![Effect::Hide]
    }

    /// Apply a daemon `ui.show_popup` push.
    fn on_push(&mut self, event: &ShowPopupEvent) -> Vec<Effect> {
        if event.toggle {
            if self.want.popup_open {
                return self.hide(None);
            }
            return Vec::new();
        }
        if event.source == "hotkey" {
            let context = Context {
                target: event.target,
                gesture: event.gesture,
            };
            if self.want.popup_open {
                let mut effects = vec![Effect::Raise];
                effects.extend(self.open_popup(context));
                effects
            } else {
                self.open_popup(context)
            }
        } else if self.want.popup_open {
            vec![Effect::Raise]
        } else {
            self.open_popup(Context::default())
        }
    }

    /// Read the focus-loss flag from a config reply and return the theme and
    /// key effects it holds.
    fn on_config(&mut self, body: &Value) -> Vec<Effect> {
        let doc = ConfigDoc::from_snapshot(body);
        if let Some(value) = doc.get("popup.dismiss_on_focus_loss") {
            if let Some(flag) = value.as_bool() {
                self.dismiss_on_focus_loss = flag;
            }
        }
        let mut effects = Vec::new();
        if let Some(theme) = doc.get("ui.theme") {
            let dark = theme.get("mode").and_then(Value::as_str) != Some("light");
            let font_scale = theme.get("font_scale").and_then(Value::as_f64);
            effects.push(Effect::ApplyTheme { dark, font_scale });
        }
        if let Some(keys) = doc.get("popup.keys") {
            if let Ok(map) = serde_json::from_value::<BTreeMap<String, String>>(keys.clone()) {
                effects.push(Effect::ApplyKeys(map));
            }
        }
        effects
    }

    /// A well-formed history reply means online, a malformed one offline.
    fn on_history(&mut self, body: &Value) -> Vec<Effect> {
        if popup::parse_history(body).is_some() {
            self.offline = false;
        } else {
            self.offline = true;
            self.error = Some(strings::text("app.error").to_string());
        }
        Vec::new()
    }

    /// Show a paste failure, opening the popup without context when closed.
    fn on_paste_failed(&mut self, message: String) -> Vec<Effect> {
        let effects = if self.want.popup_open {
            Vec::new()
        } else {
            self.open_popup(Context::default())
        };
        self.error = Some(message);
        effects
    }

    /// Apply one bridge event; events without a rule change nothing.
    fn on_bridge(&mut self, event: UiEvent) -> Vec<Effect> {
        match event {
            UiEvent::ShowPopup(push) => self.on_push(&push),
            UiEvent::PasteFailed(message) => self.on_paste_failed(message),
            UiEvent::ExecFailed(_, error) => {
                let message = if error.code == "unavailable" || error.code == "denied" {
                    strings::text("popup.paste_blocked").to_string()
                } else {
                    error.message
                };
                self.on_paste_failed(message)
            }
            UiEvent::Error(message) => {
                self.error = Some(message);
                Vec::new()
            }
            UiEvent::Offline(_) => {
                self.offline = true;
                Vec::new()
            }
            UiEvent::Status(_) => {
                self.offline = false;
                Vec::new()
            }
            UiEvent::History(body) => self.on_history(&body),
            UiEvent::Config(body) => self.on_config(&body),
            UiEvent::Stopped | UiEvent::StopFailed(_) | UiEvent::AnotherInstance => {
                self.want.exit = true;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Hide an open popup.
    fn on_dismiss(&mut self) -> Vec<Effect> {
        if self.want.popup_open {
            self.hide(None)
        } else {
            Vec::new()
        }
    }

    /// Hide an open popup on focus loss when the config asks for it.
    fn on_focus_lost(&mut self, now: Millis) -> Vec<Effect> {
        if self.want.popup_open && self.dismiss_on_focus_loss {
            self.hide(Some(now))
        } else {
            Vec::new()
        }
    }

    /// Hide an open popup the window manager asked to close.
    fn on_close_request(&mut self) -> Vec<Effect> {
        if self.want.popup_open {
            self.hide(None)
        } else {
            Vec::new()
        }
    }

    /// Toggle the popup from the tray, unless the click only followed a
    /// focus-loss hide.
    fn on_tray_toggle(&mut self, now: Millis) -> Vec<Effect> {
        if self.want.popup_open {
            return self.hide(None);
        }
        if let Some(at) = self.focus_hide_at {
            if now.saturating_sub(at) <= TRAY_GUARD_MS {
                return Vec::new();
            }
        }
        self.open_popup(Context::default())
    }

    /// Hide the popup and paste the picked clip: at once with a target, else
    /// once the popup is confirmed hidden (see [`Shell::tick`]).
    fn on_pick(&mut self, id: Uuid, picked_ms: i64) -> Vec<Effect> {
        if !self.want.popup_open {
            return Vec::new();
        }
        let context = self.context;
        let mut effects = vec![Effect::AllowForeground];
        effects.extend(self.hide(None));
        if let Some(target) = context.target {
            effects.push(Effect::Send(UiCmd::PasteClipTo(
                id,
                Some(target),
                picked_ms,
                context.gesture,
            )));
        } else {
            self.pending_paste = Some(PendingPaste {
                id,
                picked_ms,
                gesture: context.gesture,
            });
        }
        effects
    }

    /// Hide the popup and execute the picked permanent once the popup is
    /// confirmed hidden (see [`Shell::tick`]).
    fn on_pick_permanent(&mut self, id: Uuid) -> Vec<Effect> {
        if !self.want.popup_open {
            return Vec::new();
        }
        let mut effects = vec![Effect::AllowForeground];
        effects.extend(self.hide(None));
        self.pending_exec = Some(id);
        effects
    }

    /// Store the first confirmation that the closed popup is hidden.
    fn on_hidden(&mut self, now: Millis) -> Vec<Effect> {
        if !self.want.popup_open && self.hidden_at.is_none() {
            self.hidden_at = Some(now);
        }
        Vec::new()
    }

    /// A key press clears the error line.
    fn on_key(&mut self) -> Vec<Effect> {
        self.error = None;
        Vec::new()
    }

    /// F5 asks for a fresh history list.
    fn on_refresh() -> Vec<Effect> {
        vec![Effect::Send(UiCmd::ListHistory)]
    }

    /// Record that the Settings window is open.
    fn on_settings_open(&mut self) -> Vec<Effect> {
        self.want.settings_open = true;
        Vec::new()
    }

    /// Record that the Settings window is closed.
    fn on_settings_close(&mut self) -> Vec<Effect> {
        self.want.settings_open = false;
        Vec::new()
    }

    /// Send Stop on the first Quit and start the exit timer.
    fn on_quit(&mut self, now: Millis) -> Vec<Effect> {
        if self.quit_at.is_some() {
            return Vec::new();
        }
        self.quit_at = Some(now);
        vec![Effect::Send(UiCmd::Stop)]
    }

    /// Apply one event at time `now` and return the effects, in order.
    ///
    /// Open sequence: the effects `ClearSearch`, `Send(ListHistory)`,
    /// `SelectNewest`, `FocusSearch`, in this order. Every open also clears
    /// the error line, the stored focus-loss hide time and the confirmed-hidden
    /// time, and drops a pending paste. Hide: the popup closes, the context is
    /// cleared, the effect `Hide` is returned, the confirmed-hidden time is
    /// cleared; the stored focus-loss hide time is cleared too, except for a
    /// focus-loss hide, which stores `now`.
    ///
    /// Per event (what is not listed changes nothing and returns no effects):
    /// - `Bridge(ShowPopup(p))` with `p.toggle`: open, hide; closed, nothing.
    ///   Its gesture is never stored.
    /// - `Bridge(ShowPopup(p))` with `p.source == "hotkey"` and no toggle:
    ///   open if closed, else `Raise` first; the context becomes
    ///   `{p.target, p.gesture}` (replacing any old one); then the open sequence.
    /// - `Bridge(ShowPopup(p))`, any other source (a request), no toggle:
    ///   closed: open with an empty context and the open sequence; open: only
    ///   `Raise`, the context stays.
    /// - `Bridge(PasteFailed(msg))`: closed: open with an empty context, the
    ///   open sequence, then the error line is `msg`; open: the error line is
    ///   `msg`, nothing else.
    /// - `Bridge(ExecFailed(_, error))`: the same as `PasteFailed`, with the
    ///   text `popup.paste_blocked` for unavailable or denied failures and
    ///   the daemon message for other errors.
    /// - `Bridge(Error(msg))`: the error line is `msg`.
    /// - `Bridge(Status(_))`: not offline. `Bridge(Offline(_))`: offline, error
    ///   line unchanged.
    /// - `Bridge(History(body))`: a well-formed reply (see
    ///   `popup::parse_history`; an empty or unchanged list is well-formed)
    ///   makes the shell not offline; a malformed one makes it offline and the
    ///   error line the text `app.error`.
    /// - `Bridge(Config(body))`: `"popup.dismiss_on_focus_loss"` when it is a
    ///   bool sets the flag (absent or not a bool keeps it). Effects: when
    ///   `"ui.theme"` exists `ApplyTheme { dark, font_scale }` (`dark` unless
    ///   its `"mode"` is the string `"light"`, so also when the mode is
    ///   missing; `font_scale` from the number `"font_scale"`, else `None`),
    ///   then when `"popup.keys"` parses as a string map `ApplyKeys(map)`
    ///   (an empty map included). Keys are dotted paths in the
    ///   `{"config": ..}` snapshot, see `config_model::ConfigDoc`.
    /// - `Bridge(Stopped | StopFailed(_) | AnotherInstance)`: exit.
    /// - `Dismiss`, `CloseRequest`: open: hide.
    /// - `FocusLost`: open and the focus-loss flag on: hide, storing `now` as
    ///   the focus-loss hide time.
    /// - `TrayToggle`: open: hide. Closed: if a focus-loss hide time is stored
    ///   and `now - time <= TRAY_GUARD_MS`, nothing (the stored time stays);
    ///   else open with an empty context and the open sequence.
    /// - `Pick { id, picked_ms }`: only while open. The context is captured
    ///   first, then the effects `AllowForeground`, `Hide` (a hide as above),
    ///   then with a target `Send(PasteClipTo(id, target, picked_ms, gesture))`;
    ///   without a target nothing more now: the paste waits as pending and
    ///   [`Shell::tick`] sends `PasteClipTo(id, None, picked_ms, gesture)`.
    /// - `PickPermanent { id }`: only while open. The effects `AllowForeground`,
    ///   `Hide` (a hide as above), nothing more now, whatever the context holds:
    ///   the exec waits as pending and [`Shell::tick`] sends `ExecPermanent(id)`
    ///   (it has no target, the daemon pastes into the foreground window). Any
    ///   open drops it, like a pending paste.
    /// - `Hidden`: only while closed; stores `now` as the confirmed-hidden time
    ///   unless one is stored already (the first confirmation counts).
    /// - `Key`: clears the error line.
    /// - `Refresh`: `Send(ListHistory)`, in any state.
    /// - `SettingsOpen`, `SettingsClose`: only the Settings flag changes.
    /// - `Quit`: the first one returns `Send(Stop)` and stores `now`; later
    ///   ones change nothing.
    #[must_use]
    pub fn handle(&mut self, event: Event, now: Millis) -> Vec<Effect> {
        match event {
            Event::Bridge(bridge) => self.on_bridge(bridge),
            Event::Dismiss => self.on_dismiss(),
            Event::FocusLost => self.on_focus_lost(now),
            Event::CloseRequest => self.on_close_request(),
            Event::TrayToggle => self.on_tray_toggle(now),
            Event::Pick { id, picked_ms } => self.on_pick(id, picked_ms),
            Event::PickPermanent { id } => self.on_pick_permanent(id),
            Event::Hidden => self.on_hidden(now),
            Event::Key => self.on_key(),
            Event::Refresh => Self::on_refresh(),
            Event::SettingsOpen => self.on_settings_open(),
            Event::SettingsClose => self.on_settings_close(),
            Event::Quit => self.on_quit(now),
        }
    }

    /// Advance the timers to `now` and return what is due; call it before
    /// reading [`Shell::desired`]. A pending paste is released as
    /// `Send(PasteClipTo(id, None, picked_ms, gesture))` once the popup is
    /// closed, a confirmed-hidden time is stored and
    /// `now >= hidden_at + PASTE_DELAY_MS`; a pending permanent exec is
    /// released as `Send(ExecPermanent(id))` under the same condition. Exit is
    /// set once a Quit is stored and `now >= quit + QUIT_TIMEOUT_MS`; it stays
    /// set.
    #[must_use]
    pub fn tick(&mut self, now: Millis) -> Vec<Effect> {
        let mut effects = Vec::new();
        if let Some(pending) = self.pending_paste.clone() {
            if let Some(hidden_at) = self.hidden_at {
                if !self.want.popup_open && now >= hidden_at.saturating_add(PASTE_DELAY_MS) {
                    self.pending_paste = None;
                    effects.push(Effect::Send(UiCmd::PasteClipTo(
                        pending.id,
                        None,
                        pending.picked_ms,
                        pending.gesture,
                    )));
                }
            }
        }
        if let Some(id) = self.pending_exec {
            if let Some(hidden_at) = self.hidden_at {
                if !self.want.popup_open && now >= hidden_at.saturating_add(PASTE_DELAY_MS) {
                    self.pending_exec = None;
                    effects.push(Effect::Send(UiCmd::ExecPermanent(id)));
                }
            }
        }
        if let Some(quit_at) = self.quit_at {
            if !self.want.exit && now >= quit_at.saturating_add(QUIT_TIMEOUT_MS) {
                self.want.exit = true;
            }
        }
        effects
    }

    /// The earliest time at which [`Shell::tick`] has something to do: the
    /// quit deadline while a Quit is stored and exit is not set, and the paste
    /// deadline while a pending paste or exec has a confirmed-hidden time; the
    /// smaller of the two, `None` when there is neither.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Millis> {
        let mut deadlines = Vec::new();
        if let Some(quit_at) = self.quit_at {
            if !self.want.exit {
                deadlines.push(quit_at.saturating_add(QUIT_TIMEOUT_MS));
            }
        }
        if self.pending_paste.is_some() || self.pending_exec.is_some() {
            if let Some(hidden_at) = self.hidden_at {
                deadlines.push(hidden_at.saturating_add(PASTE_DELAY_MS));
            }
        }
        deadlines.into_iter().min()
    }

    /// The window state the shell wants: the popup is open, the Settings window
    /// is open, the loop should exit.
    #[must_use]
    pub fn desired(&self) -> Desired {
        self.want
    }

    /// The paste context of the open popup, empty while closed.
    #[must_use]
    pub fn context(&self) -> Context {
        self.context
    }

    /// The error line, if any.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether the daemon link counts as down.
    #[must_use]
    pub fn offline(&self) -> bool {
        self.offline
    }
}

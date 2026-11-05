//! Popup trigger path (seg-02 §6, §9): every hotkey press and `popup.open`
//! request ends here. The daemon captures the paste target at the press,
//! mints the gesture for presses only, and pushes `ui.show_popup` to the
//! UI, starting the UI when none is subscribed.

use std::time::{Duration, Instant};

use clipmonger_ipc::proto::methods::{
    ShowPopupEvent, CAPS_CHANGED_TOPIC, CONFIG_CHANGED_TOPIC, HISTORY_CHANGED_TOPIC,
    PASTE_ASSIST_TOPIC, SHOW_POPUP_TOPIC,
};
use clipmonger_ipc::server::{EventHub, SubscriberInfo};
use clipmonger_platform::traits::HotkeyManager;
use clipmonger_platform::types::HotkeyId;

use crate::DaemonState;

/// Starts the UI process; returns whether one was started.
pub type UiLauncher = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

/// What asked for the popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupSource {
    /// A hotkey press: captures the target and mints a gesture.
    Press,
    /// A `popup.open` request (tray, second UI instance): neither.
    Request,
    /// A `popup.open` request asking for the target (`clipmonger-cli
    /// show-popup`): captures the target like a press, never toggles and
    /// never mints a gesture.
    CliRequest,
}

/// Presses closer together than this are one press (key repeat, or the
/// extension and the portal both reporting one chord).
const PRESS_DEBOUNCE: Duration = Duration::from_millis(300);
/// While no UI subscribes, the daemon starts at most one per this window.
const SPAWN_INTERVAL: Duration = Duration::from_secs(10);
/// How long a paste waits for the GNOME extension's answer.
const ASSIST_WAIT: Duration = Duration::from_secs(3);
/// Largest clip the extension pastes (base64 must fit one IPC frame).
const ASSIST_MAX_BYTES: usize = 512 * 1024;
/// Toast id telling the user to press Ctrl+V (the paste was not injected).
pub(crate) const PRESS_CTRL_V_TOAST: &str = "paste.press_ctrl_v";

/// Trigger-path state kept by the daemon.
#[derive(Default)]
pub(crate) struct PopupTrigger {
    hub: Option<EventHub>,
    launcher: Option<UiLauncher>,
    last_press: Option<Instant>,
    last_spawn: Option<Instant>,
    /// Extension pastes waiting for `paste.assist_done`, by request id.
    assists: std::collections::HashMap<u64, std::sync::mpsc::Sender<serde_json::Value>>,
    next_assist: u64,
}

impl DaemonState {
    /// Route popup events through `hub` (call once at startup).
    pub fn attach_event_hub(&self, hub: EventHub) {
        if let Ok(mut trigger) = self.popup.lock() {
            trigger.hub = Some(hub);
        }
    }

    /// Tell `config.changed` subscribers to re-read the config. Nothing is
    /// held while none is subscribed: a subscriber reads it on connect.
    pub(crate) fn notify_config_changed(&self) {
        self.publish_event(CONFIG_CHANGED_TOPIC);
    }

    /// Tell `history.changed` subscribers (the UI) a captured clip was
    /// stored. Nothing is held: the popup lists the history when it opens.
    pub(crate) fn notify_history_changed(&self) {
        self.publish_event(HISTORY_CHANGED_TOPIC);
    }

    /// Tell `caps.changed` subscribers (the UI) the popup-hotkey state
    /// changed, so a conflict banner appears or clears at once.
    pub(crate) fn notify_caps_changed(&self) {
        self.publish_event(CAPS_CHANGED_TOPIC);
    }

    /// Paste through the GNOME extension (GNOME Wayland): push the clip on
    /// `paste.assist` and wait for its `paste.assist_done`. `None` when no
    /// extension listens (the caller uses its own path). The extension
    /// writes the clip to the clipboard and presses Ctrl+V when the window
    /// focused at the popup press is in front again, else it toasts.
    pub(crate) fn paste_via_extension(
        &self,
        payloads: &[(String, Vec<u8>)],
    ) -> Option<Result<serde_json::Value, clipmonger_ipc::proto::IpcError>> {
        use clipmonger_ipc::proto::IpcError;
        let hub = self
            .popup
            .lock()
            .ok()
            .and_then(|trigger| trigger.hub.clone())?;
        if hub.subscribers(PASTE_ASSIST_TOPIC).is_empty() {
            return None;
        }
        let (mime, bytes) = assist_payload(payloads)?;
        if bytes.len() > ASSIST_MAX_BYTES {
            return Some(Err(IpcError::new(
                "unavailable",
                format!(
                    "paste.execute: clip over {} KiB; copy it manually",
                    ASSIST_MAX_BYTES / 1024
                ),
            )));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let id = {
            let mut trigger = self.popup.lock().ok()?;
            trigger.next_assist = trigger.next_assist.wrapping_add(1);
            let id = trigger.next_assist;
            trigger.assists.insert(id, tx);
            id
        };
        let params = serde_json::json!({
            "id": id,
            "mime": mime,
            "data_b64": crate::encode_thumbnail_png(bytes),
            "inject": true,
        });
        let delivered = hub.publish(PASTE_ASSIST_TOPIC, params) > 0;
        let answer = delivered
            .then(|| rx.recv_timeout(ASSIST_WAIT).ok())
            .flatten();
        if let Ok(mut trigger) = self.popup.lock() {
            trigger.assists.remove(&id);
        }
        let Some(answer) = answer else {
            return Some(Err(IpcError::new(
                "unavailable",
                "paste.execute: the GNOME extension did not answer".to_string(),
            )));
        };
        if !answer
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            let why = answer
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("extension paste failed");
            return Some(Err(IpcError::new(
                "unavailable",
                format!("paste.execute: {why}"),
            )));
        }
        let injected = answer
            .get("injected")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let mut result = serde_json::json!({"pasted": true, "injected": injected});
        if !injected {
            result["toast"] = serde_json::Value::from(PRESS_CTRL_V_TOAST);
        }
        Some(Ok(result))
    }

    /// The extension's `paste.assist_done`: hand it to the waiting paste.
    pub(crate) fn assist_done(&self, params: &serde_json::Value) -> serde_json::Value {
        let waiting = params
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .and_then(|id| self.popup.lock().ok()?.assists.remove(&id));
        let known = waiting.is_some_and(|tx| tx.send(params.clone()).is_ok());
        serde_json::json!({"ok": known})
    }

    /// Publish an empty event on `topic` to its current subscribers.
    fn publish_event(&self, topic: &str) {
        let hub = self
            .popup
            .lock()
            .ok()
            .and_then(|trigger| trigger.hub.clone());
        if let Some(hub) = hub {
            let _ = hub.publish(topic, serde_json::json!({}));
        }
    }

    /// Start the UI with `launcher` when a popup is wanted and no UI is
    /// subscribed.
    pub fn set_ui_launcher(&self, launcher: UiLauncher) {
        if let Ok(mut trigger) = self.popup.lock() {
            trigger.launcher = Some(launcher);
        }
    }

    /// Show the popup for `source`; returns whether a running UI got it.
    pub fn trigger_popup(&self, source: PopupSource) -> bool {
        self.trigger_popup_with(
            source,
            Instant::now(),
            press_target,
            &mut clipmonger_core::entropy::random_u64,
        )
    }

    /// The GNOME extension's press report. `focused_popup_pid` is the
    /// process of the focused Shell window titled like the popup; when it is
    /// the subscribed UI, the popup is in front and the press hides it
    /// (`toggle`, as on Windows and X11).
    pub fn trigger_extension_press(&self, focused_popup_pid: Option<u32>) -> bool {
        self.trigger_popup_with(
            PopupSource::Press,
            Instant::now(),
            |ui| {
                extension_press_decision(ui, focused_popup_pid).unwrap_or_else(|| press_target(ui))
            },
            &mut clipmonger_core::entropy::random_u64,
        )
    }

    /// Show a popup for a hotkey carrying optional platform activation metadata.
    pub fn trigger_popup_activation(
        &self,
        activation: clipmonger_platform::types::HotkeyActivation,
    ) -> bool {
        self.trigger_popup_with_activation(
            PopupSource::Press,
            Instant::now(),
            press_target,
            &mut clipmonger_core::entropy::random_u64,
            activation,
        )
    }

    fn trigger_popup_with<F>(
        &self,
        source: PopupSource,
        now: Instant,
        capture: impl FnOnce(Option<&SubscriberInfo>) -> (Option<u64>, bool),
        draw: &mut F,
    ) -> bool
    where
        F: FnMut() -> Result<u64, clipmonger_core::entropy::Error>,
    {
        self.trigger_popup_with_activation(
            source,
            now,
            capture,
            draw,
            clipmonger_platform::types::HotkeyActivation::default(),
        )
    }

    fn trigger_popup_with_activation<F>(
        &self,
        source: PopupSource,
        now: Instant,
        capture: impl FnOnce(Option<&SubscriberInfo>) -> (Option<u64>, bool),
        draw: &mut F,
        activation: clipmonger_platform::types::HotkeyActivation,
    ) -> bool
    where
        F: FnMut() -> Result<u64, clipmonger_core::entropy::Error>,
    {
        let hub = {
            let Ok(mut trigger) = self.popup.lock() else {
                return false;
            };
            if source == PopupSource::Press {
                if trigger
                    .last_press
                    .is_some_and(|at| now.duration_since(at) < PRESS_DEBOUNCE)
                {
                    return false;
                }
                trigger.last_press = Some(now);
            }
            trigger.hub.clone()
        };
        let Some(hub) = hub else {
            return false;
        };
        let ui = hub.subscribers(SHOW_POPUP_TOPIC).into_iter().next();
        let (target, toggle) = match source {
            PopupSource::Press => capture(ui.as_ref()),
            PopupSource::Request => (None, false),
            // The popup in front is no target; the request only raises it.
            PopupSource::CliRequest => (capture(ui.as_ref()).0, false),
        };
        let gesture = if source == PopupSource::Press && !toggle {
            let at_ms = crate::now_ms();
            if let Ok(token) = self.mint_gesture_token_with(target, at_ms, draw) {
                Some(token)
            } else {
                crate::daemon_runtime_emit(
                    clipmonger_core::runtime_log::Severity::Warn,
                    "popup.gesture_failed",
                    &[("class", "entropy_unavailable")],
                );
                eprintln!("daemon: gesture entropy unavailable");
                None
            }
        } else {
            None
        };
        if !toggle {
            let ui = hub.subscribers(SHOW_POPUP_TOPIC).into_iter().next();
            activate_popup(ui.as_ref(), source == PopupSource::Press);
        }
        let event = ShowPopupEvent {
            source: match source {
                PopupSource::Press => "hotkey",
                PopupSource::Request | PopupSource::CliRequest => "request",
            }
            .to_string(),
            toggle,
            target,
            gesture,
            x11_time: activation.x11_time,
            activation_token: activation.activation_token,
        };
        let params = serde_json::to_value(&event).unwrap_or_default();
        if hub.publish_or_hold(SHOW_POPUP_TOPIC, params) > 0 {
            return true;
        }
        self.spawn_ui();
        false
    }

    /// Start the UI unless one was started within [`SPAWN_INTERVAL`].
    fn spawn_ui(&self) {
        let launcher = {
            let Ok(mut trigger) = self.popup.lock() else {
                return;
            };
            let now = Instant::now();
            if trigger
                .last_spawn
                .is_some_and(|at| now.duration_since(at) < SPAWN_INTERVAL)
            {
                return;
            }
            trigger.last_spawn = Some(now);
            trigger.launcher.clone()
        };
        if let Some(launch) = launcher {
            if !launch() {
                crate::daemon_runtime_emit(
                    clipmonger_core::runtime_log::Severity::Warn,
                    "popup.spawn_failed",
                    &[("class", "launcher_unavailable")],
                );
                eprintln!("daemon: could not start the UI for the popup");
            }
        }
    }
}

/// A refused popup chord is tried again after this long (its holder may
/// let go).
const CHORD_RETRY: Duration = Duration::from_secs(5);

/// What caps must show for the popup chord.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChordStatus {
    /// The configured chord is registered.
    Held,
    /// The configured chord was refused (why); the previous chord, if any,
    /// stays registered.
    Refused(String),
}

/// Popup chord registration that follows `popup.hotkey` without a restart.
#[derive(Default)]
pub struct PopupChord {
    held: Option<String>,
    refused: Option<(String, Instant)>,
}

impl PopupChord {
    /// Register `wanted` as hotkey 1 when it changed, or when its refusal
    /// is due for a retry. Returns the status when caps must change.
    pub fn sync<H: HotkeyManager>(
        &mut self,
        hotkeys: &mut H,
        wanted: &str,
        now: Instant,
    ) -> Option<ChordStatus> {
        if self.held.as_deref() == Some(wanted) {
            // Changed back to the held chord after a refused change.
            return self.refused.take().map(|_| ChordStatus::Held);
        }
        let retry = match &self.refused {
            Some((chord, at)) if chord == wanted => {
                if now.duration_since(*at) < CHORD_RETRY {
                    return None;
                }
                true
            }
            _ => false,
        };
        match hotkeys.register(HotkeyId(1), wanted) {
            Ok(()) => {
                self.held = Some(wanted.to_string());
                self.refused = None;
                Some(ChordStatus::Held)
            }
            Err(e) => {
                if !retry {
                    crate::daemon_runtime_emit(
                        clipmonger_core::runtime_log::Severity::Warn,
                        "hotkey.register_failed",
                        &[("class", crate::runtime_platform_class(&e))],
                    );
                }
                self.refused = Some((wanted.to_string(), now));
                (!retry).then(|| ChordStatus::Refused(format!("{wanted}: {e}")))
            }
        }
    }
}

/// Own a popup hotkey manager through factory, registration and poll
/// failures. Every replacement starts with fresh chord-registration state.
#[allow(clippy::too_many_arguments)]
pub fn run_hotkeys<H: HotkeyManager>(
    hotkeys: Option<H>,
    shutdown: &std::sync::atomic::AtomicBool,
    factory: impl FnMut() -> clipmonger_platform::types::Result<H>,
    wanted: impl FnMut() -> String,
    ready: impl FnMut(),
    publish: impl FnMut(ChordStatus),
    mut press: impl FnMut(),
    sleep: impl FnMut(Duration),
    permanents: Option<&DaemonState>,
    permanent_press: impl FnMut(uuid::Uuid, &str, &mut dyn FnMut() -> bool),
) {
    run_hotkeys_with_activation(
        hotkeys,
        shutdown,
        factory,
        wanted,
        ready,
        publish,
        |_| press(),
        sleep,
        permanents,
        permanent_press,
    );
}

/// Own hotkeys and deliver popup activation metadata for each firing.
#[allow(clippy::too_many_arguments)]
#[expect(
    clippy::too_many_lines,
    reason = "keep the single owner and recovery loop together"
)]
pub fn run_hotkeys_with_activation<H: HotkeyManager>(
    mut hotkeys: Option<H>,
    shutdown: &std::sync::atomic::AtomicBool,
    mut factory: impl FnMut() -> clipmonger_platform::types::Result<H>,
    mut wanted: impl FnMut() -> String,
    mut ready: impl FnMut(),
    mut publish: impl FnMut(ChordStatus),
    mut press: impl FnMut(clipmonger_platform::types::HotkeyActivation),
    mut sleep: impl FnMut(Duration),
    permanents: Option<&DaemonState>,
    mut permanent_press: impl FnMut(uuid::Uuid, &str, &mut dyn FnMut() -> bool),
) {
    use std::sync::atomic::Ordering;
    let mut chord = PopupChord::default();
    let mut last_status = None;
    let mut registration = None;
    let mut last_working: Option<String> = None;
    let mut delay = Duration::from_millis(100);
    let mut permanent_chords = PermanentChords::default();
    let mut last_bindings: Option<(String, Vec<(uuid::Uuid, String)>)> = None;
    while !shutdown.load(Ordering::Relaxed) {
        let mut report = |status: ChordStatus| {
            if last_status.as_ref() != Some(&status) {
                publish(status.clone());
                last_status = Some(status);
            }
        };
        if hotkeys.is_none() {
            match factory() {
                Ok(mut replacement) => {
                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    // A refused configuration change may have kept an older
                    // working chord. Re-register that fallback on the new
                    // manager before trying the current configuration.
                    let fallback = last_working.clone();
                    if permanents.is_some() && last_bindings.is_some() {
                        // Drain the Windows process-wide event channel before
                        // binding any chord to a new logical owner. X11's new
                        // connection has its own empty event queue.
                        if let Err(error) = replacement.discard_pending() {
                            report(ChordStatus::Refused(error.to_string()));
                            drop(replacement);
                            sleep(delay);
                            delay = (delay * 2).min(Duration::from_secs(2));
                            continue;
                        }
                    }
                    chord = PopupChord::default();
                    permanent_chords = PermanentChords::default();
                    registration = fallback
                        .and_then(|held| chord.sync(&mut replacement, &held, Instant::now()));
                    hotkeys = Some(replacement);
                }
                Err(error) => {
                    crate::daemon_runtime_emit(
                        clipmonger_core::runtime_log::Severity::Warn,
                        "hotkey.init_failed",
                        &[("class", crate::runtime_platform_class(&error))],
                    );
                    report(ChordStatus::Refused(error.to_string()));
                    ready();
                    sleep(delay);
                    delay = (delay * 2).min(Duration::from_secs(2));
                    continue;
                }
            }
        }
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        let popup_wanted = wanted();
        let current = permanents.map(DaemonState::permanent_hotkeys);
        if current.as_ref().is_some_and(Result::is_err) {
            report(ChordStatus::Refused(
                "permanent hotkey list unavailable".into(),
            ));
            drop(hotkeys.take());
            sleep(delay);
            delay = (delay * 2).min(Duration::from_secs(2));
            continue;
        }
        if let Some(Ok(rows)) = &current {
            let bindings = (popup_wanted.clone(), rows.clone());
            if last_bindings.as_ref().is_some_and(|old| old != &bindings) {
                last_bindings = Some(bindings);
                drop(hotkeys.take());
                continue;
            }
            last_bindings = Some(bindings);
        }
        let Some(manager) = hotkeys.as_mut() else {
            continue;
        };
        if let Some(status) = chord.sync(manager, &popup_wanted, Instant::now()) {
            if matches!(&status, ChordStatus::Refused(_)) {
                report(status.clone());
            }
            registration = Some(status);
        }
        if let Some(held) = &chord.held {
            last_working = Some(held.clone());
        }
        let mut permanent_status = None;
        if let Some(current) = &current {
            permanent_status = Some(permanent_chords.sync(
                manager,
                current.as_deref(),
                chord.held.as_deref(),
                &popup_wanted,
                Instant::now(),
            ));
        }
        ready();
        if chord.held.is_none() && permanent_chords.held.is_empty() {
            // An initially refused registration is retried with a new owner;
            // a refused change keeps the previous working chord instead.
            drop(hotkeys.take());
            sleep(delay);
            delay = (delay * 2).min(Duration::from_secs(2));
            continue;
        }
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        match manager.poll_events(Duration::from_millis(200)) {
            Ok(ids) => {
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
                if let Some(status) = &registration {
                    if matches!(status, ChordStatus::Refused(_)) {
                        report(status.clone());
                    } else if let Some(status) = &permanent_status {
                        report(status.clone());
                    } else {
                        report(status.clone());
                    }
                }
                delay = Duration::from_millis(100);
                let mut dispatched = false;
                let mut drain_error = None;
                if let Some(event) = ids.iter().find(|event| event.id == HotkeyId(1)) {
                    press(event.activation.clone());
                    dispatched = permanents.is_some();
                }
                if !dispatched {
                    if let Some(state) = permanents {
                        for event in ids {
                            if let Some((clip, wanted)) = permanent_chords.held.get(&event.id) {
                                if state.permanent_hotkeys().is_ok_and(|rows| {
                                    rows.iter().any(|row| row.0 == *clip && row.1 == *wanted)
                                }) {
                                    let mut pause = || {
                                        let Some(mut owner) = hotkeys.take() else {
                                            return false;
                                        };
                                        let result = owner.discard_pending();
                                        drop(owner);
                                        if let Err(error) = result {
                                            drain_error = Some(error);
                                            false
                                        } else {
                                            true
                                        }
                                    };
                                    permanent_press(*clip, wanted, &mut pause);
                                    dispatched = true;
                                    break;
                                }
                            }
                        }
                    }
                }
                if dispatched {
                    // Delivery can block while the foreground changes. Drop
                    // the rest of this batch and all events accumulated by
                    // this owner, then re-create it before accepting presses.
                    if let Some(mut owner) = hotkeys.take() {
                        if let Err(error) = owner.discard_pending() {
                            drain_error = Some(error);
                        }
                        drop(owner);
                    }
                    if let Some(error) = drain_error {
                        report(ChordStatus::Refused(error.to_string()));
                        sleep(delay);
                        delay = (delay * 2).min(Duration::from_secs(2));
                    }
                }
            }
            Err(error) => {
                crate::daemon_runtime_emit(
                    clipmonger_core::runtime_log::Severity::Warn,
                    "hotkey.poll_failed",
                    &[
                        ("operation", "hotkey_poll"),
                        ("class", crate::runtime_platform_class(&error)),
                    ],
                );
                report(ChordStatus::Refused(error.to_string()));
                // Drop before waiting/constructing the replacement, including
                // its native window and registrations on the owning thread.
                drop(hotkeys.take());
                sleep(delay);
                delay = (delay * 2).min(Duration::from_secs(2));
            }
        }
    }
}

#[derive(Default)]
struct PermanentChords {
    held: std::collections::HashMap<HotkeyId, (uuid::Uuid, String)>,
    next: u32,
    failures: std::collections::HashMap<uuid::Uuid, Instant>,
}

fn chord_key(chord: &str) -> Option<(Vec<String>, String)> {
    let mut parsed = clipmonger_platform::chord::parse(chord).ok()?;
    parsed.mods.sort();
    Some((parsed.mods, parsed.key))
}

impl PermanentChords {
    fn sync<H: HotkeyManager>(
        &mut self,
        manager: &mut H,
        wanted: Result<&[(uuid::Uuid, String)], &clipmonger_ipc::proto::IpcError>,
        popup: Option<&str>,
        popup_wanted: &str,
        now: Instant,
    ) -> ChordStatus {
        let Ok(wanted) = wanted else {
            for id in self.held.keys().copied().collect::<Vec<_>>() {
                let _ = manager.unregister(id);
            }
            self.held.clear();
            return ChordStatus::Refused("permanent hotkey list unavailable".into());
        };
        let mut reserved = std::collections::HashSet::new();
        if let Some(key) = popup.and_then(chord_key) {
            reserved.insert(key);
        }
        if let Some(key) = chord_key(popup_wanted) {
            reserved.insert(key);
        }
        let mut selected = Vec::new();
        let mut failed = false;
        for (clip, chord) in wanted {
            if let Some(key) = chord_key(chord) {
                if reserved.insert(key) {
                    selected.push((*clip, chord.clone()));
                    continue;
                }
            }
            failed = true;
        }
        let removed: Vec<_> = self
            .held
            .iter()
            .filter(|(_, entry)| !selected.contains(entry))
            .map(|(id, _)| *id)
            .collect();
        for id in removed {
            if manager.unregister(id).is_err() {
                failed = true;
            }
            self.held.remove(&id);
        }
        self.failures
            .retain(|id, _| selected.iter().any(|entry| entry.0 == *id));
        for (clip, chord) in selected {
            if self
                .held
                .values()
                .any(|entry| entry.0 == clip && entry.1 == chord)
            {
                continue;
            }
            if self
                .failures
                .get(&clip)
                .is_some_and(|at| now.duration_since(*at) < CHORD_RETRY)
            {
                failed = true;
                continue;
            }
            let Some(next) = self.next.checked_add(1) else {
                failed = true;
                continue;
            };
            self.next = next.max(2);
            let id = HotkeyId(self.next);
            if manager.register(id, &chord).is_ok() {
                self.held.insert(id, (clip, chord));
                self.failures.remove(&clip);
            } else {
                self.failures.insert(clip, now);
                failed = true;
            }
        }
        if failed {
            ChordStatus::Refused(
                "permanent hotkey conflict, invalid chord, or registration failure".into(),
            )
        } else {
            ChordStatus::Held
        }
    }
}

/// Paste target of a press, and whether the press should hide the popup
/// instead (the UI's popup is the window in front).
#[cfg(all(windows, feature = "backend-windows"))]
fn press_target(ui: Option<&SubscriberInfo>) -> (Option<u64>, bool) {
    let foreground = clipmonger_backend_windows::foreground_window_id();
    press_decision(
        foreground,
        foreground.and_then(clipmonger_backend_windows::hwnd_owner_pid),
        ui.and_then(|ui| ui.peer_pid),
        ui.and_then(registered_popup_hwnd),
    )
}

/// The press decision without the Win32 calls. The popup is in front only
/// when the foreground window is the popup window the UI registered, so any
/// other window of the UI (Settings) is an ordinary paste target. A UI that
/// registered no popup window is told apart by its process.
#[cfg(any(test, all(windows, feature = "backend-windows")))]
fn press_decision(
    foreground: Option<i64>,
    foreground_pid: Option<u32>,
    ui_pid: Option<u32>,
    popup_hwnd: Option<u64>,
) -> (Option<u64>, bool) {
    let Some(foreground) = foreground else {
        return (None, false);
    };
    let popup_in_front = match popup_hwnd {
        Some(popup) => foreground.cast_unsigned() == popup,
        None => ui_pid.is_some() && foreground_pid == ui_pid,
    };
    if popup_in_front {
        (None, true)
    } else {
        (Some(foreground.cast_unsigned()), false)
    }
}

/// The popup window the UI registered when it subscribed.
#[cfg(all(windows, feature = "backend-windows"))]
fn registered_popup_hwnd(ui: &SubscriberInfo) -> Option<u64> {
    ui.params
        .get("popup_hwnd")
        .and_then(serde_json::Value::as_u64)
}

/// `Some((None, true))` (hide) when the extension saw the subscribed UI's
/// popup focused; `None` leaves the decision to the platform capture.
fn extension_press_decision(
    ui: Option<&SubscriberInfo>,
    focused_popup_pid: Option<u32>,
) -> Option<(Option<u64>, bool)> {
    let ui_pid = ui.and_then(|ui| ui.peer_pid)?;
    (focused_popup_pid == Some(ui_pid)).then_some((None, true))
}

#[cfg(all(unix, any(test, feature = "backend-x11")))]
fn x11_press_decision(target: Option<u64>, popup: Option<u64>) -> (Option<u64>, bool) {
    if target.is_some() && target == popup {
        (None, true)
    } else {
        (target, false)
    }
}

#[cfg(all(unix, any(test, feature = "backend-x11")))]
fn x11_press_target_for_ui(
    target: Option<u64>,
    ui: Option<&SubscriberInfo>,
) -> (Option<u64>, bool) {
    let popup = ui
        .and_then(|ui| ui.params.get("popup_x11_window"))
        .and_then(serde_json::Value::as_u64);
    x11_press_decision(target, popup)
}

/// Paste target of a press, unless the registered X11 popup is foreground.
#[cfg(all(unix, feature = "backend-x11"))]
fn press_target(ui: Option<&SubscriberInfo>) -> (Option<u64>, bool) {
    use clipmonger_platform::types::PlatformHandle;
    if std::env::var_os("DISPLAY").is_none() {
        return (None, false);
    }
    let target = clipmonger_backend_x11::X11Foreground::connect(None)
        .and_then(|foreground| foreground.capture_target())
        .ok();
    let target = match target.map(|target| target.handle) {
        Some(PlatformHandle::X11Window(window)) => Some(window),
        _ => None,
    };
    x11_press_target_for_ui(target, ui)
}

/// No foreground target on this platform (Wayland, headless).
#[cfg(not(any(
    all(windows, feature = "backend-windows"),
    all(unix, feature = "backend-x11")
)))]
fn press_target(_ui: Option<&SubscriberInfo>) -> (Option<u64>, bool) {
    (None, false)
}

/// Windows: bring the UI's popup to the front right here, inside the press
/// handling. Only the process that received the hotkey may take the
/// foreground, and only until the next input (releasing the chord), so a
/// later activation by the UI itself would race the key release. A request
/// still tries (a second launch passes its foreground right on first) but
/// usually has no right, so only a failed press is reported.
#[cfg(all(windows, feature = "backend-windows"))]
fn activate_popup(ui: Option<&SubscriberInfo>, pressed: bool) {
    let Some(ui) = ui else {
        return;
    };
    if let (Some(hwnd), Some(pid)) = (registered_popup_hwnd(ui), ui.peer_pid) {
        report_popup_activation(
            clipmonger_backend_windows::show_owned_window(hwnd.cast_signed(), pid),
            pressed,
            |event, fields| {
                crate::daemon_runtime_emit(
                    clipmonger_core::runtime_log::Severity::Warn,
                    event,
                    fields,
                );
            },
        );
    }
}

#[cfg(any(test, all(windows, feature = "backend-windows")))]
fn report_popup_activation(ok: bool, pressed: bool, emit: impl FnOnce(&str, &[(&str, &str)])) {
    if !ok && pressed {
        emit("popup.activation_failed", &[("class", "unavailable")]);
    }
}

#[cfg(test)]
mod activation_report_tests {
    use super::report_popup_activation;

    #[test]
    fn failure_emits_only_fixed_metadata_and_success_is_silent() {
        let calls = std::cell::RefCell::new(Vec::new());
        report_popup_activation(false, true, |event, fields| {
            calls.borrow_mut().push((
                event.to_owned(),
                fields
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect::<Vec<_>>(),
            ));
        });
        report_popup_activation(true, true, |_, _| panic!("success must be silent"));
        report_popup_activation(false, false, |_, _| {
            panic!("a request without the foreground right must be silent")
        });
        assert_eq!(
            *calls.borrow(),
            vec![(
                "popup.activation_failed".to_owned(),
                vec![("class".to_owned(), "unavailable".to_owned())]
            )]
        );
    }
}

/// The payload the extension pastes: text when the clip has it, else the
/// first format. `None` for an empty clip.
fn assist_payload(payloads: &[(String, Vec<u8>)]) -> Option<(&str, &[u8])> {
    payloads
        .iter()
        .find(|(mime, _)| mime.split(';').next().map(str::trim) == Some("text/plain"))
        .or_else(|| payloads.first())
        .map(|(mime, bytes)| (mime.as_str(), bytes.as_slice()))
}

/// Elsewhere the UI raises its own window when the push arrives.
#[cfg(not(all(windows, feature = "backend-windows")))]
fn activate_popup(_ui: Option<&SubscriberInfo>, _pressed: bool) {}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    use clipmonger_ipc::client::{Client, EventStream};
    use clipmonger_ipc::proto::methods::{
        ShowPopupEvent, CAPS_CHANGED_TOPIC, CONFIG_CHANGED_TOPIC, HISTORY_CHANGED_TOPIC,
        PASTE_ASSIST_TOPIC, SHOW_POPUP_TOPIC,
    };
    use clipmonger_ipc::server::{EventHub, ServeOptions, Server, TopicRule};
    use serde_json::json;

    use super::{PopupSource, PRESS_DEBOUNCE};
    use crate::{now_ms, DaemonState};

    const TOKEN: &str = "popup-token";

    /// Daemon state with a hub served on a fresh socket, plus a counting
    /// launcher standing in for the UI.
    async fn served(tag: &str) -> (Arc<DaemonState>, std::path::PathBuf, Arc<AtomicUsize>) {
        let dir = std::env::temp_dir().join(format!("cm-popup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = clipmonger_core::Store::open(&dir).expect("store");
        let state = Arc::new(DaemonState::new(store));
        let hub = EventHub::new(vec![
            TopicRule {
                topic: SHOW_POPUP_TOPIC.to_string(),
                exclusive: true,
            },
            TopicRule {
                topic: CONFIG_CHANGED_TOPIC.to_string(),
                exclusive: false,
            },
            TopicRule {
                topic: HISTORY_CHANGED_TOPIC.to_string(),
                exclusive: false,
            },
            TopicRule {
                topic: CAPS_CHANGED_TOPIC.to_string(),
                exclusive: false,
            },
            TopicRule {
                topic: PASTE_ASSIST_TOPIC.to_string(),
                exclusive: false,
            },
        ]);
        state.attach_event_hub(hub.clone());
        let launches = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&launches);
        state.set_ui_launcher(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            true
        }));
        let sock = dir.join("p.sock");
        let handler_state = Arc::clone(&state);
        let handler: clipmonger_ipc::server::Handler =
            Arc::new(move |method, params| handler_state.handle(&method, &params));
        let options = ServeOptions {
            hub: Some(hub),
            ..ServeOptions::default()
        };
        let server = Server::bind_with(&sock, TOKEN.to_string(), handler, options)
            .await
            .expect("bind");
        std::mem::forget(server);
        (state, sock, launches)
    }

    fn subscribe(sock: &std::path::Path) -> EventStream {
        Client::connect(sock, TOKEN)
            .expect("connect")
            .subscribe(&[SHOW_POPUP_TOPIC], json!({}))
            .expect("subscribe")
    }

    /// Next pushed show event within `limit` (the stream comes back).
    fn next_show(
        mut stream: EventStream,
        limit: Duration,
    ) -> Option<(EventStream, ShowPopupEvent)> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let got = stream.next_event();
            let _ = tx.send((stream, got));
        });
        match rx.recv_timeout(limit) {
            Ok((stream, Ok((_, params)))) => Some((
                stream,
                serde_json::from_value(params).expect("show payload"),
            )),
            _ => None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn request_without_ui_is_held_starts_one_ui_and_reaches_it() {
        let (state, sock, launches) = served("held").await;
        let answer = state.handle("popup.open", &json!({})).expect("popup.open");
        assert_eq!(answer, json!({"delivered": false}));
        assert!(!state.trigger_popup(PopupSource::Request));
        assert_eq!(
            launches.load(Ordering::SeqCst),
            1,
            "requests while the UI starts launch it once"
        );
        let (_stream, show) =
            next_show(subscribe(&sock), Duration::from_secs(2)).expect("held show arrives");
        assert_eq!(show.source, "request");
        assert_eq!(
            (show.gesture, show.target, show.toggle),
            (None, None, false)
        );
        assert!(
            state.trigger_popup(PopupSource::Request),
            "the UI is there now"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn config_set_pushes_config_changed_to_every_subscriber() {
        let (state, sock, _) = served("config-changed").await;
        state.set_profile_dir(
            &std::env::temp_dir().join(format!("cm-popup-config-changed-{}", std::process::id())),
        );
        let streams: Vec<EventStream> = (0..2)
            .map(|_| {
                Client::connect(&sock, TOKEN)
                    .expect("connect")
                    .subscribe(&[CONFIG_CHANGED_TOPIC], json!({}))
                    .expect("config.changed is not exclusive")
            })
            .collect();
        state
            .handle(
                "config.set",
                &json!({"key": "popup.hotkey", "value": "ctrl+alt+j"}),
            )
            .expect("config.set");
        for mut stream in streams {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(stream.next_event().map(|(method, _)| method));
            });
            let method = rx
                .recv_timeout(Duration::from_secs(2))
                .expect("config.changed arrives")
                .expect("event");
            assert_eq!(method, CONFIG_CHANGED_TOPIC);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn press_mints_a_target_bound_gesture_and_requests_never_do() {
        let (state, sock, _) = served("gesture").await;
        let stream = subscribe(&sock);
        assert!(state.trigger_popup(PopupSource::Press));
        let (stream, press) = next_show(stream, Duration::from_secs(2)).expect("press show");
        assert_eq!(press.source, "hotkey");
        let gesture = press.gesture.expect("a press carries a gesture");
        assert!(
            state.consume_gesture_token(gesture, now_ms(), press.target),
            "the pushed gesture spends for the captured target"
        );
        std::thread::sleep(PRESS_DEBOUNCE);
        assert!(state.trigger_popup(PopupSource::Request));
        let (_stream, request) = next_show(stream, Duration::from_secs(2)).expect("request show");
        assert_eq!(
            (request.source.as_str(), request.gesture),
            ("request", None)
        );
    }

    #[test]
    fn extension_press_hides_only_the_subscribed_ui_popup() {
        let ui = clipmonger_ipc::server::SubscriberInfo {
            peer_pid: Some(41),
            params: json!({}),
        };
        assert_eq!(
            super::extension_press_decision(Some(&ui), Some(41)),
            Some((None, true))
        );
        assert_eq!(super::extension_press_decision(Some(&ui), Some(42)), None);
        assert_eq!(super::extension_press_decision(Some(&ui), None), None);
        assert_eq!(super::extension_press_decision(None, Some(41)), None);
        let unknown = clipmonger_ipc::server::SubscriberInfo {
            peer_pid: None,
            params: json!({}),
        };
        assert_eq!(super::extension_press_decision(Some(&unknown), None), None);
    }

    /// A fake extension: answer every `paste.assist` with `answer(params)`.
    fn fake_extension(
        sock: &std::path::Path,
        answer: impl Fn(&serde_json::Value) -> Option<serde_json::Value> + Send + 'static,
    ) -> mpsc::Receiver<serde_json::Value> {
        let mut stream = Client::connect(sock, TOKEN)
            .expect("connect")
            .subscribe(&[PASTE_ASSIST_TOPIC], json!({}))
            .expect("paste.assist is not exclusive");
        let replier = Client::connect(sock, TOKEN).expect("reply connect");
        let (seen_tx, seen_rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok((_, params)) = stream.next_event() {
                let _ = seen_tx.send(params.clone());
                if let Some(done) = answer(&params) {
                    let _ = replier.call("paste.assist_done", done);
                }
            }
        });
        seen_rx
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn extension_paste_round_trips_and_reports_injection() {
        let (state, sock, _) = served("assist").await;
        let text = vec![
            ("text/html".to_string(), b"<b>hi</b>".to_vec()),
            (
                "text/plain;charset=utf-8".to_string(),
                "héllo".as_bytes().to_vec(),
            ),
        ];
        assert!(
            state.paste_via_extension(&text).is_none(),
            "no extension listens: the caller keeps its own path"
        );
        let seen = fake_extension(&sock, |params| {
            Some(json!({"id": params["id"], "ok": true, "injected": true}))
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while state
            .popup
            .lock()
            .unwrap()
            .hub
            .as_ref()
            .unwrap()
            .subscribers(PASTE_ASSIST_TOPIC)
            .is_empty()
        {
            assert!(Instant::now() < deadline, "extension subscribes");
            std::thread::sleep(Duration::from_millis(20));
        }
        let result = state
            .paste_via_extension(&text)
            .expect("the extension listens")
            .expect("pasted");
        assert_eq!(result, json!({"pasted": true, "injected": true}));
        let params = seen
            .recv_timeout(Duration::from_secs(2))
            .expect("assist pushed");
        assert_eq!(
            params["mime"], "text/plain;charset=utf-8",
            "text is preferred"
        );
        assert_eq!(params["inject"], true);
        assert_eq!(
            crate::b64_decode(params["data_b64"].as_str().unwrap()).unwrap(),
            "héllo".as_bytes()
        );
        let big = vec![(
            "text/plain".to_string(),
            vec![b'x'; super::ASSIST_MAX_BYTES + 1],
        )];
        let refused = state
            .paste_via_extension(&big)
            .expect("listens")
            .unwrap_err();
        assert!(refused.message.contains("copy it manually"), "{refused:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn extension_paste_toasts_when_not_injected_and_times_out_without_answer() {
        let (state, sock, _) = served("assist-toast").await;
        let _seen = fake_extension(&sock, |params| {
            (params["mime"] == "text/plain")
                .then(|| json!({"id": params["id"], "ok": true, "injected": false}))
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while state
            .popup
            .lock()
            .unwrap()
            .hub
            .as_ref()
            .unwrap()
            .subscribers(PASTE_ASSIST_TOPIC)
            .is_empty()
        {
            assert!(Instant::now() < deadline, "extension subscribes");
            std::thread::sleep(Duration::from_millis(20));
        }
        let toast = state
            .paste_via_extension(&[("text/plain".to_string(), b"hi".to_vec())])
            .expect("listens")
            .expect("pasted");
        assert_eq!(
            toast,
            json!({"pasted": true, "injected": false, "toast": super::PRESS_CTRL_V_TOAST})
        );
        let silent = state
            .paste_via_extension(&[("image/png".to_string(), b"png".to_vec())])
            .expect("listens")
            .unwrap_err();
        assert!(silent.message.contains("did not answer"), "{silent:?}");
        assert_eq!(
            state.assist_done(&json!({"id": 999_999, "ok": true})),
            json!({"ok": false}),
            "an unknown id is not a waiting paste"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hotkey_state_changes_push_caps_changed() {
        let (state, sock, _) = served("caps-changed").await;
        let mut stream = Client::connect(&sock, TOKEN)
            .expect("connect")
            .subscribe(&[CAPS_CHANGED_TOPIC], json!({}))
            .expect("caps.changed is not exclusive");
        state.set_hotkey_status(
            "x11",
            &super::ChordStatus::Refused("ctrl+alt+k: denied: held".into()),
        );
        state.set_hotkey_status("x11", &super::ChordStatus::Held);
        state
            .handle(
                "hotkey.conflict",
                &json!({"chord": "super+v", "cleared": true}),
            )
            .expect("hotkey.conflict");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..3 {
                let _ = tx.send(stream.next_event().map(|(method, _)| method));
            }
        });
        for which in ["refused", "held", "conflict cleared"] {
            let method = rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap_or_else(|_| panic!("{which} pushes caps.changed"))
                .expect("event");
            assert_eq!(method, CAPS_CHANGED_TOPIC, "{which}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stored_capture_pushes_history_changed_once() {
        let (state, sock, _) = served("history-changed").await;
        let mut stream = Client::connect(&sock, TOKEN)
            .expect("connect")
            .subscribe(&[HISTORY_CHANGED_TOPIC], json!({}))
            .expect("history.changed is not exclusive");
        let payload = |text: &str| vec![("text/plain".to_string(), text.as_bytes().to_vec())];
        assert!(state.ingest_capture("test", false, &payload("first")));
        assert!(
            !state.ingest_capture("test", true, &payload("secret")),
            "a sensitive capture stores nothing"
        );
        assert!(state.ingest_capture("test", false, &payload("second")));
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..3 {
                let _ = tx.send(stream.next_event().map(|(method, _)| method));
            }
        });
        let next = || rx.recv_timeout(Duration::from_secs(2));
        assert_eq!(
            next().expect("first event").expect("event"),
            HISTORY_CHANGED_TOPIC
        );
        assert_eq!(
            next().expect("second event").expect("event"),
            HISTORY_CHANGED_TOPIC
        );
        assert!(
            next().is_err(),
            "one event per stored clip, none for the skip"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn extension_press_with_the_ui_popup_focused_pushes_toggle() {
        let (state, sock, _) = served("ext-toggle").await;
        let stream = subscribe(&sock);
        let ui_pid = std::process::id();
        state
            .handle(
                "hotkey.fired",
                &json!({"id": 1, "focused_popup_pid": ui_pid}),
            )
            .expect("hotkey.fired");
        let (stream, hide) = next_show(stream, Duration::from_secs(2)).expect("press show");
        assert!(hide.toggle, "the UI's own popup in front hides it");
        std::thread::sleep(PRESS_DEBOUNCE);
        state
            .handle(
                "hotkey.fired",
                &json!({"id": 1, "focused_popup_pid": ui_pid + 1}),
            )
            .expect("hotkey.fired");
        let (_stream, show) = next_show(stream, Duration::from_secs(2)).expect("press show");
        assert!(
            !show.toggle,
            "another process's window never hides the popup"
        );
    }

    #[test]
    fn x11_popup_toggles_without_capturing_it_and_settings_remains_target() {
        assert_eq!(super::x11_press_decision(Some(21), Some(21)), (None, true));
        assert_eq!(
            super::x11_press_decision(Some(22), Some(21)),
            (Some(22), false)
        );
        assert_eq!(super::x11_press_decision(Some(22), None), (Some(22), false));
        assert_eq!(super::x11_press_decision(None, Some(21)), (None, false));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn registered_x11_popup_toggles_through_subscriber_without_gesture() {
        let (state, sock, _) = served("x11-popup-toggle").await;
        let stream = Client::connect(&sock, TOKEN)
            .unwrap()
            .subscribe(&[SHOW_POPUP_TOPIC], json!({"popup_x11_window": 21}))
            .unwrap();
        let now = Instant::now();
        let mut draw = || Ok(321);
        assert!(state.trigger_popup_with(
            PopupSource::Press,
            now,
            |ui| super::x11_press_target_for_ui(Some(21), ui),
            &mut draw
        ));
        let (stream, toggle) = next_show(stream, Duration::from_secs(2)).unwrap();
        assert_eq!(
            (toggle.toggle, toggle.target, toggle.gesture),
            (true, None, None)
        );
        assert!(state.trigger_popup_with(
            PopupSource::Press,
            now + PRESS_DEBOUNCE,
            |ui| super::x11_press_target_for_ui(Some(22), ui),
            &mut draw
        ));
        let (_, settings) = next_show(stream, Duration::from_secs(2)).unwrap();
        assert_eq!(
            (settings.toggle, settings.target, settings.gesture),
            (false, Some(22), Some(321))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn press_activation_metadata_reaches_the_subscriber() {
        let (state, sock, _) = served("activation-metadata").await;
        let stream = subscribe(&sock);
        assert!(
            state.trigger_popup_activation(clipmonger_platform::types::HotkeyActivation {
                x11_time: Some(12345),
                activation_token: Some("token-from-compositor".into()),
            })
        );
        let (_, event) = next_show(stream, Duration::from_secs(2)).expect("show");
        assert_eq!(event.x11_time, Some(12345));
        assert_eq!(
            event.activation_token.as_deref(),
            Some("token-from-compositor")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn presses_inside_the_debounce_are_one_press() {
        let (state, sock, _) = served("debounce").await;
        let stream = subscribe(&sock);
        assert!(state.trigger_popup(PopupSource::Press));
        assert!(
            !state.trigger_popup(PopupSource::Press),
            "a second report of the same press is dropped"
        );
        let (stream, _) = next_show(stream, Duration::from_secs(2)).expect("one show");
        assert!(
            next_show(stream, Duration::from_millis(300)).is_none(),
            "and no second show follows"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cli_request_captures_the_target_but_never_toggles_or_mints() {
        let (state, sock, _) = served("cli-request").await;
        let stream = subscribe(&sock);
        let now = Instant::now();
        let mut draw = || -> Result<u64, clipmonger_core::entropy::Error> {
            panic!("a CLI request draws no gesture")
        };
        assert!(state.trigger_popup_with(
            PopupSource::CliRequest,
            now,
            |_| (Some(0x77), false),
            &mut draw
        ));
        let (stream, open) = next_show(stream, Duration::from_secs(2)).expect("cli show");
        assert_eq!(
            (open.source.as_str(), open.target, open.gesture, open.toggle),
            ("request", Some(0x77), None, false)
        );
        assert!(state.trigger_popup_with(
            PopupSource::CliRequest,
            now,
            |_| (None, true),
            &mut draw
        ));
        let (_, raise) = next_show(stream, Duration::from_secs(2)).expect("raise show");
        assert_eq!(
            (raise.target, raise.gesture, raise.toggle),
            (None, None, false),
            "the popup in front is raised, not toggled, and is no target"
        );
        assert!(state.gesture_tokens.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn popup_open_captures_only_when_asked() {
        let (state, sock, _) = served("cli-param").await;
        let stream = subscribe(&sock);
        let answer = state
            .handle("popup.open", &json!({"capture_target": false}))
            .expect("popup.open");
        assert_eq!(answer, json!({"delivered": true}));
        let (_, show) = next_show(stream, Duration::from_secs(2)).expect("show");
        assert_eq!(
            (show.source.as_str(), show.target, show.gesture),
            ("request", None, None)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn popup_entropy_decisions_use_the_production_path() {
        let (state, sock, _) = served("entropy-decisions").await;
        let stream = subscribe(&sock);
        let now = Instant::now();
        let mut draws = 0;
        let mut draw = || {
            draws += 1;
            Ok(123)
        };
        assert!(state.trigger_popup_with(
            PopupSource::Request,
            now,
            |_| panic!("request captures no target"),
            &mut draw
        ));
        let (stream, request) = next_show(stream, Duration::from_secs(2)).expect("request");
        assert_eq!(
            (request.gesture, request.target, request.toggle),
            (None, None, false)
        );
        assert!(state.trigger_popup_with(PopupSource::Press, now, |_| (None, true), &mut draw));
        let (stream, toggle) = next_show(stream, Duration::from_secs(2)).expect("toggle");
        assert_eq!(
            (toggle.gesture, toggle.target, toggle.toggle),
            (None, None, true)
        );
        assert!(!state.trigger_popup_with(
            PopupSource::Press,
            now,
            |_| panic!("debounced press captures no target"),
            &mut draw
        ));
        assert_eq!(
            draws, 0,
            "requests, toggles and debounced presses draw nothing"
        );
        assert!(state.gesture_tokens.lock().unwrap().is_empty());
        let mut failures = 0;
        assert!(state.trigger_popup_with(
            PopupSource::Press,
            now + PRESS_DEBOUNCE,
            |_| (Some(0x1234), false),
            &mut || {
                failures += 1;
                Err(clipmonger_core::entropy::Error::UNSUPPORTED)
            }
        ));
        let (stream, failure) =
            next_show(stream, Duration::from_secs(2)).expect("normal popup on entropy failure");
        assert_eq!(failures, 1);
        assert_eq!(
            (failure.gesture, failure.target, failure.toggle),
            (None, Some(0x1234), false)
        );
        assert!(
            !*state.elevation_refused.lock().unwrap(),
            "transient failure never latches refusal"
        );
        assert!(state.gesture_tokens.lock().unwrap().is_empty());
        assert!(state.trigger_popup_with(
            PopupSource::Press,
            now + PRESS_DEBOUNCE * 2,
            |_| (Some(0x5678), false),
            &mut || Ok(456)
        ));
        let (_, success) = next_show(stream, Duration::from_secs(2)).expect("success popup");
        assert_eq!(
            (success.gesture, success.target, success.toggle),
            (Some(456), Some(0x5678), false)
        );
        assert!(state.consume_gesture_token(456, crate::now_ms(), Some(0x5678)));
        assert!(!state.consume_gesture_token(456, crate::now_ms(), Some(0x5678)));
    }
}

#[cfg(test)]
mod chord_tests {
    use std::collections::{HashMap, HashSet};
    use std::time::{Duration, Instant};

    use clipmonger_platform::traits::HotkeyManager;
    use clipmonger_platform::types::{Affinity, Error, HotkeyId, Result};

    use super::{ChordStatus, PermanentChords, PopupChord, CHORD_RETRY};

    /// Chords in `taken` belong to another app; a refused change keeps the
    /// held chord, like the Windows manager.
    #[derive(Default)]
    struct Keys {
        taken: HashSet<String>,
        held: HashMap<HotkeyId, String>,
        calls: usize,
    }

    impl HotkeyManager for Keys {
        const AFFINITY: Affinity = Affinity::Worker;

        fn register(&mut self, id: HotkeyId, chord: &str) -> Result<()> {
            self.calls += 1;
            if self.taken.contains(chord) {
                return Err(Error::Denied(format!("already registered ({chord})")));
            }
            self.held.insert(id, chord.to_string());
            Ok(())
        }

        fn unregister(&mut self, id: HotkeyId) -> Result<()> {
            self.held.remove(&id);
            Ok(())
        }

        fn poll(&mut self, _timeout: Duration) -> Result<Vec<HotkeyId>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn popup_chord_follows_config_and_keeps_the_old_chord_on_refusal() {
        let mut keys = Keys::default();
        keys.taken.insert("ctrl+alt+x".to_string());
        let mut chord = PopupChord::default();
        let start = Instant::now();

        assert_eq!(
            chord.sync(&mut keys, "ctrl+alt+h", start),
            Some(ChordStatus::Held)
        );
        assert_eq!(chord.sync(&mut keys, "ctrl+alt+h", start), None);
        assert_eq!(keys.calls, 1, "an unchanged chord is not registered again");

        let refused = chord.sync(&mut keys, "ctrl+alt+x", start);
        assert!(
            matches!(&refused, Some(ChordStatus::Refused(why)) if why.starts_with("ctrl+alt+x: ")),
            "{refused:?}"
        );
        assert_eq!(keys.held[&HotkeyId(1)], "ctrl+alt+h", "the old chord stays");
        assert_eq!(chord.sync(&mut keys, "ctrl+alt+x", start), None);
        assert_eq!(keys.calls, 2, "no retry before it is due");
        let due = start + CHORD_RETRY;
        assert_eq!(chord.sync(&mut keys, "ctrl+alt+x", due), None);
        assert_eq!(
            keys.calls, 3,
            "retried once due; still refused, caps unchanged"
        );

        assert_eq!(
            chord.sync(&mut keys, "ctrl+alt+h", due),
            Some(ChordStatus::Held),
            "back to the held chord clears the refusal"
        );
        assert_eq!(keys.calls, 3, "without registering it again");

        assert!(matches!(
            chord.sync(&mut keys, "ctrl+alt+x", due),
            Some(ChordStatus::Refused(_))
        ));
        keys.taken.clear();
        let later = due + CHORD_RETRY;
        assert_eq!(
            chord.sync(&mut keys, "ctrl+alt+x", later),
            Some(ChordStatus::Held),
            "a released chord is picked up on retry"
        );
        assert_eq!(keys.held[&HotkeyId(1)], "ctrl+alt+x");
    }

    #[test]
    fn permanent_registration_conflicts_retry_and_removal() {
        let mut keys = Keys::default();
        keys.taken.insert("ctrl+alt+j".into());
        let mut chords = PermanentChords::default();
        let a = uuid::Uuid::from_u128(1);
        let b = uuid::Uuid::from_u128(2);
        let now = Instant::now();
        let desired = vec![(a, "ctrl+alt+j".to_string()), (b, "ALT+CTRL+h".to_string())];
        assert!(matches!(
            chords.sync(
                &mut keys,
                Ok(&desired),
                Some("ctrl+alt+h"),
                "ctrl+alt+h",
                now
            ),
            ChordStatus::Refused(_)
        ));
        assert!(
            chords.held.is_empty(),
            "failed and popup-conflicting chords cannot fire"
        );
        assert_eq!(keys.calls, 1);
        keys.taken.clear();
        chords.sync(
            &mut keys,
            Ok(&desired),
            Some("ctrl+alt+h"),
            "ctrl+alt+h",
            now + CHORD_RETRY,
        );
        assert_eq!(chords.held.len(), 1);
        let old = *chords.held.keys().next().unwrap();
        assert_eq!(chords.held[&old].0, a);
        assert_eq!(keys.held.len(), 1);
        assert!(matches!(
            chords.sync(
                &mut keys,
                Ok(&[]),
                Some("ctrl+alt+h"),
                "ctrl+alt+h",
                now + CHORD_RETRY
            ),
            ChordStatus::Held
        ));
        assert!(keys.held.is_empty());
        assert!(chords.held.is_empty());
        chords.sync(
            &mut keys,
            Ok(&desired[..1]),
            Some("ctrl+alt+h"),
            "ctrl+alt+h",
            now + CHORD_RETRY,
        );
        assert!(
            !chords.held.contains_key(&old),
            "retired IDs must not be reused"
        );
    }
}

#[cfg(test)]
mod press_tests {
    use super::press_decision;

    const UI_PID: u32 = 100;
    const OTHER_PID: u32 = 200;
    const POPUP: u64 = 0x1234;
    const SETTINGS: u64 = 0x5678;
    const OTHER: u64 = 0x9abc;

    fn fg(hwnd: u64) -> i64 {
        hwnd.cast_signed()
    }

    #[test]
    fn registered_popup_in_front_toggles() {
        assert_eq!(
            press_decision(Some(fg(POPUP)), Some(UI_PID), Some(UI_PID), Some(POPUP)),
            (None, true)
        );
    }

    #[test]
    fn another_ui_window_in_front_is_a_normal_press_with_that_target() {
        assert_eq!(
            press_decision(Some(fg(SETTINGS)), Some(UI_PID), Some(UI_PID), Some(POPUP)),
            (Some(SETTINGS), false)
        );
    }

    #[test]
    fn foreign_window_in_front_is_a_normal_press_with_that_target() {
        assert_eq!(
            press_decision(Some(fg(OTHER)), Some(OTHER_PID), Some(UI_PID), Some(POPUP)),
            (Some(OTHER), false)
        );
    }

    #[test]
    fn without_a_registered_popup_the_ui_process_decides() {
        assert_eq!(
            press_decision(Some(fg(SETTINGS)), Some(UI_PID), Some(UI_PID), None),
            (None, true)
        );
        assert_eq!(
            press_decision(Some(fg(OTHER)), Some(OTHER_PID), Some(UI_PID), None),
            (Some(OTHER), false)
        );
        assert_eq!(
            press_decision(Some(fg(OTHER)), None, Some(UI_PID), None),
            (Some(OTHER), false),
            "an unknown owner is not the UI"
        );
        assert_eq!(
            press_decision(Some(fg(OTHER)), None, None, None),
            (Some(OTHER), false),
            "no UI subscribed: nothing to toggle"
        );
    }

    #[test]
    fn no_foreground_window_is_a_press_without_target() {
        assert_eq!(
            press_decision(None, None, Some(UI_PID), Some(POPUP)),
            (None, false)
        );
        assert_eq!(
            press_decision(None, None, Some(UI_PID), None),
            (None, false)
        );
    }
}

#[cfg(test)]
mod hotkey_recovery_tests {
    use super::*;
    use clipmonger_platform::types::{Affinity, Error, Result};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn popup_dispatch_uses_its_own_event_metadata_in_a_batch() {
        use clipmonger_platform::types::{HotkeyActivation, HotkeyEvent};
        struct Batched;
        impl HotkeyManager for Batched {
            const AFFINITY: Affinity = Affinity::Worker;
            fn register(&mut self, _: HotkeyId, _: &str) -> Result<()> {
                Ok(())
            }
            fn unregister(&mut self, _: HotkeyId) -> Result<()> {
                Ok(())
            }
            fn poll(&mut self, _: Duration) -> Result<Vec<HotkeyId>> {
                unreachable!()
            }
            fn poll_events(&mut self, _: Duration) -> Result<Vec<HotkeyEvent>> {
                Ok(vec![
                    HotkeyEvent {
                        id: HotkeyId(2),
                        activation: HotkeyActivation {
                            x11_time: Some(111),
                            ..HotkeyActivation::default()
                        },
                    },
                    HotkeyEvent {
                        id: HotkeyId(1),
                        activation: HotkeyActivation {
                            x11_time: Some(222),
                            ..HotkeyActivation::default()
                        },
                    },
                ])
            }
        }
        let stop = AtomicBool::new(false);
        let mut received = None;
        run_hotkeys_with_activation(
            Some(Batched),
            &stop,
            || Ok(Batched),
            || "ctrl+alt+h".into(),
            || {},
            |_| {},
            |activation| {
                received = activation.x11_time;
                stop.store(true, Ordering::SeqCst);
            },
            |_| {},
            None,
            |_, _, _| unreachable!(),
        );
        assert_eq!(received, Some(222));
    }

    struct Manager {
        held: bool,
        register_fails: bool,
        poll_fails: bool,
        live: Arc<AtomicUsize>,
        registered: Arc<AtomicUsize>,
    }
    impl Drop for Manager {
        fn drop(&mut self) {
            self.live.fetch_sub(1, Ordering::SeqCst);
        }
    }
    impl HotkeyManager for Manager {
        const AFFINITY: Affinity = Affinity::Worker;
        fn register(&mut self, id: HotkeyId, chord: &str) -> Result<()> {
            assert_eq!(id, HotkeyId(1));
            assert_eq!(chord, "ctrl+alt+h");
            if self.register_fails {
                return Err(Error::Denied("registration conflict".into()));
            }
            self.held = true;
            self.registered.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn unregister(&mut self, _: HotkeyId) -> Result<()> {
            self.held = false;
            Ok(())
        }
        fn poll(&mut self, _: Duration) -> Result<Vec<HotkeyId>> {
            assert!(
                self.held,
                "recreated manager must actually register before polling"
            );
            if self.poll_fails {
                return Err(Error::Unavailable("manager exited".into()));
            }
            Ok(vec![HotkeyId(1)])
        }
    }

    #[test]
    fn factory_registration_poll_failure_recreate_and_shutdown_use_real_loop() {
        let stop = AtomicBool::new(false);
        let live = Arc::new(AtomicUsize::new(0));
        let registered = Arc::new(AtomicUsize::new(0));
        // Factory unavailable, refused initial registration, registered but
        // dead poll owner, then a fully working replacement.
        let mut script = VecDeque::from([
            None,
            Some((true, false)),
            Some((false, true)),
            Some((false, false)),
        ]);
        let mut statuses = Vec::new();
        let mut waits = Vec::new();
        let mut presses = 0;
        run_hotkeys(
            None,
            &stop,
            || {
                assert_eq!(
                    live.load(Ordering::SeqCst),
                    0,
                    "old native owner must drop before factory"
                );
                let Some((register_fails, poll_fails)) = script.pop_front().unwrap() else {
                    return Err(Error::Unavailable("factory unavailable".into()));
                };
                live.fetch_add(1, Ordering::SeqCst);
                Ok(Manager {
                    held: false,
                    register_fails,
                    poll_fails,
                    live: Arc::clone(&live),
                    registered: Arc::clone(&registered),
                })
            },
            || "ctrl+alt+h".to_string(),
            || {},
            |s| statuses.push(s),
            || {
                presses += 1;
                stop.store(true, Ordering::Relaxed);
            },
            |d| waits.push(d.as_millis()),
            None,
            |_, _, _| {},
        );
        assert_eq!(presses, 1);
        assert_eq!(registered.load(Ordering::SeqCst), 2);
        assert_eq!(live.load(Ordering::SeqCst), 0);
        assert_eq!(waits, [100, 200, 400]);
        assert!(matches!(statuses.first(), Some(ChordStatus::Refused(_))));
        assert_eq!(statuses.last(), Some(&ChordStatus::Held));
    }

    #[test]
    fn recreation_rebinds_previous_working_chord_after_refused_change() {
        use std::cell::Cell;
        use std::sync::Mutex;
        struct FallbackManager {
            dies: bool,
            polls: usize,
            held: bool,
            registrations: Arc<Mutex<Vec<String>>>,
        }
        impl HotkeyManager for FallbackManager {
            const AFFINITY: Affinity = Affinity::Worker;
            fn register(&mut self, _: HotkeyId, chord: &str) -> Result<()> {
                self.registrations.lock().unwrap().push(chord.to_string());
                if chord == "ctrl+alt+j" {
                    return Err(Error::Denied("new chord occupied".into()));
                }
                self.held = true;
                Ok(())
            }
            fn unregister(&mut self, _: HotkeyId) -> Result<()> {
                Ok(())
            }
            fn poll(&mut self, _: Duration) -> Result<Vec<HotkeyId>> {
                assert!(self.held, "fallback registration lost on replacement");
                self.polls += 1;
                if self.dies {
                    if self.polls == 1 {
                        return Ok(Vec::new());
                    }
                    return Err(Error::Unavailable("manager exited".into()));
                }
                Ok(vec![HotkeyId(1)])
            }
        }
        let stop = AtomicBool::new(false);
        let registrations = Arc::new(Mutex::new(Vec::new()));
        let reads = Cell::new(0);
        let mut attempts = 0;
        run_hotkeys(
            None,
            &stop,
            || {
                attempts += 1;
                Ok(FallbackManager {
                    dies: attempts == 1,
                    polls: 0,
                    held: false,
                    registrations: Arc::clone(&registrations),
                })
            },
            || {
                reads.set(reads.get() + 1);
                if reads.get() == 1 {
                    "ctrl+alt+h".into()
                } else {
                    "ctrl+alt+j".into()
                }
            },
            || {},
            |_| {},
            || stop.store(true, Ordering::Relaxed),
            |_| {},
            None,
            |_, _, _| {},
        );
        assert_eq!(attempts, 2);
        assert_eq!(
            *registrations.lock().unwrap(),
            ["ctrl+alt+h", "ctrl+alt+j", "ctrl+alt+h", "ctrl+alt+j"]
        );
    }

    #[test]
    fn stop_during_backoff_does_not_create_replacement() {
        let stop = AtomicBool::new(false);
        let mut attempts = 0;
        let mut slices = Vec::new();
        run_hotkeys::<Manager>(
            None,
            &stop,
            || {
                attempts += 1;
                Err(Error::Unavailable("factory unavailable".into()))
            },
            || "ctrl+alt+h".into(),
            || {},
            |_| {},
            || panic!("press after stop"),
            |delay| {
                crate::capture::sleep_interruptible(&stop, delay, |slice| {
                    slices.push(slice);
                    stop.store(true, Ordering::Relaxed);
                });
            },
            None,
            |_, _, _| {},
        );
        assert_eq!(attempts, 1);
        assert_eq!(slices, [Duration::from_millis(20)]);
    }
}

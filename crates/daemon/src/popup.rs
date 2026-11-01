//! Popup trigger path (seg-02 §6, §9): every hotkey press and `popup.open`
//! request ends here. The daemon captures the paste target at the press,
//! mints the gesture for presses only, and pushes `ui.show_popup` to the
//! UI, starting the UI when none is subscribed.

use std::time::{Duration, Instant};

use clipmonger_ipc::proto::methods::{ShowPopupEvent, SHOW_POPUP_TOPIC};
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
    /// A `popup.open` request (CLI, tray, second UI instance): neither.
    Request,
}

/// Presses closer together than this are one press (key repeat, or the
/// extension and the portal both reporting one chord).
const PRESS_DEBOUNCE: Duration = Duration::from_millis(300);
/// While no UI subscribes, the daemon starts at most one per this window.
const SPAWN_INTERVAL: Duration = Duration::from_secs(10);

/// Trigger-path state kept by the daemon.
#[derive(Default)]
pub(crate) struct PopupTrigger {
    hub: Option<EventHub>,
    launcher: Option<UiLauncher>,
    last_press: Option<Instant>,
    last_spawn: Option<Instant>,
}

impl DaemonState {
    /// Route popup events through `hub` (call once at startup).
    pub fn attach_event_hub(&self, hub: EventHub) {
        if let Ok(mut trigger) = self.popup.lock() {
            trigger.hub = Some(hub);
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
        };
        let gesture = if source == PopupSource::Press && !toggle {
            let at_ms = crate::now_ms();
            if let Ok(token) = self.mint_gesture_token_with(target, at_ms, draw) {
                Some(token)
            } else {
                eprintln!("daemon: gesture entropy unavailable");
                None
            }
        } else {
            None
        };
        if !toggle {
            let ui = hub.subscribers(SHOW_POPUP_TOPIC).into_iter().next();
            activate_popup(ui.as_ref());
        }
        let event = ShowPopupEvent {
            source: match source {
                PopupSource::Press => "hotkey",
                PopupSource::Request => "request",
            }
            .to_string(),
            toggle,
            target,
            gesture,
            x11_time: None,
            activation_token: None,
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
                self.refused = Some((wanted.to_string(), now));
                (!retry).then(|| ChordStatus::Refused(format!("{wanted}: {e}")))
            }
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

/// Paste target of a press: the active X11 window (the UI decides toggling).
#[cfg(all(unix, feature = "backend-x11"))]
fn press_target(_ui: Option<&SubscriberInfo>) -> (Option<u64>, bool) {
    use clipmonger_platform::types::PlatformHandle;
    if std::env::var_os("DISPLAY").is_none() {
        return (None, false);
    }
    let target = clipmonger_backend_x11::X11Foreground::connect(None)
        .and_then(|foreground| foreground.capture_target())
        .ok();
    match target.map(|target| target.handle) {
        Some(PlatformHandle::X11Window(window)) => (Some(window), false),
        _ => (None, false),
    }
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
/// later activation by the UI itself would race the key release.
#[cfg(all(windows, feature = "backend-windows"))]
fn activate_popup(ui: Option<&SubscriberInfo>) {
    let Some(ui) = ui else {
        return;
    };
    if let (Some(hwnd), Some(pid)) = (registered_popup_hwnd(ui), ui.peer_pid) {
        let _ = clipmonger_backend_windows::show_owned_window(hwnd.cast_signed(), pid);
    }
}

/// Elsewhere the UI raises its own window when the push arrives.
#[cfg(not(all(windows, feature = "backend-windows")))]
fn activate_popup(_ui: Option<&SubscriberInfo>) {}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    use clipmonger_ipc::client::{Client, EventStream};
    use clipmonger_ipc::proto::methods::{ShowPopupEvent, SHOW_POPUP_TOPIC};
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
        let hub = EventHub::new(vec![TopicRule {
            topic: SHOW_POPUP_TOPIC.to_string(),
            exclusive: true,
        }]);
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

    use super::{ChordStatus, PopupChord, CHORD_RETRY};

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

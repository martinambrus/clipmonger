//! Daemon pushes for the UI (seg-02 §9): a dedicated connection subscribed
//! to `ui.show_popup` and `history.changed`, reconnecting with backoff while
//! the daemon is away and restarting a daemon that crashed.

use std::sync::mpsc::Sender;
use std::sync::Arc;

use clipmonger_ipc::client::Client;
use clipmonger_ipc::proto::methods::{
    ShowPopupEvent, CAPS_CHANGED_TOPIC, HISTORY_CHANGED_TOPIC, SHOW_POPUP_TOPIC,
};
use serde_json::Value;

use crate::ipc_bridge::UiEvent;
use crate::lifecycle::next_backoff;

/// Wakes the shell after an event was queued (egui repaint, Win32 message).
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// Topics the listener subscribes to. A daemon that answers `unknown_topic`
/// (one without the change topics) gets the popup topic alone.
const TOPICS: &[&str] = &[SHOW_POPUP_TOPIC, HISTORY_CHANGED_TOPIC, CAPS_CHANGED_TOPIC];

/// The shell event for one pushed frame, if the UI handles that topic.
#[must_use]
pub fn push_event(topic: &str, params: Value) -> Option<UiEvent> {
    match topic {
        SHOW_POPUP_TOPIC => serde_json::from_value::<ShowPopupEvent>(params)
            .ok()
            .map(UiEvent::ShowPopup),
        HISTORY_CHANGED_TOPIC => Some(UiEvent::HistoryChanged),
        CAPS_CHANGED_TOPIC => Some(UiEvent::CapsChanged),
        _ => None,
    }
}

/// Listen for pushes on a thread of its own. `connect` opens a fresh daemon
/// connection; `restart` runs each time no daemon answers (it decides whether
/// to start one); `extra` travels with the subscription (the popup window).
/// Ends when the shell is gone or another instance owns the popup.
pub fn spawn_listener(
    connect: impl Fn() -> Option<Client> + Send + 'static,
    restart: impl Fn() + Send + 'static,
    extra: Value,
    events: Sender<UiEvent>,
    wake: Wake,
) {
    std::thread::spawn(move || {
        let mut attempt = 0_u32;
        let mut topics = TOPICS;
        loop {
            let Some(client) = connect() else {
                restart();
                std::thread::sleep(next_backoff(attempt));
                attempt = attempt.saturating_add(1);
                continue;
            };
            match client.subscribe(topics, extra.clone()) {
                Ok(mut stream) => {
                    attempt = 0;
                    while let Ok((topic, params)) = stream.next_event() {
                        if let Some(event) = push_event(&topic, params) {
                            if events.send(event).is_err() {
                                return;
                            }
                            wake();
                        }
                    }
                }
                Err(e) if e.code == "already_subscribed" => {
                    let _ = events.send(UiEvent::AnotherInstance);
                    wake();
                    return;
                }
                Err(e) if e.code == "unknown_topic" && topics.len() > 1 => {
                    topics = &TOPICS[..1];
                    continue;
                }
                Err(_) => {}
            }
            std::thread::sleep(next_backoff(attempt));
            attempt = attempt.saturating_add(1);
        }
    });
}

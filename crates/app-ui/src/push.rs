//! Daemon pushes for the UI (seg-02 §9): a dedicated connection subscribed
//! to `ui.show_popup`, reconnecting with backoff while the daemon is away.

use std::sync::mpsc::Sender;
use std::sync::Arc;

use clipmonger_ipc::client::Client;
use clipmonger_ipc::proto::methods::{ShowPopupEvent, SHOW_POPUP_TOPIC};
use serde_json::Value;

use crate::ipc_bridge::UiEvent;
use crate::lifecycle::next_backoff;

/// Wakes the shell after an event was queued (egui repaint, Win32 message).
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// The shell event for one pushed frame, if the UI handles that topic.
#[must_use]
pub fn push_event(topic: &str, params: Value) -> Option<UiEvent> {
    if topic != SHOW_POPUP_TOPIC {
        return None;
    }
    serde_json::from_value::<ShowPopupEvent>(params)
        .ok()
        .map(UiEvent::ShowPopup)
}

/// Listen for pushes on a thread of its own. `connect` opens a fresh daemon
/// connection; `extra` travels with the subscription (the popup window).
/// Ends when the shell is gone or another instance owns the popup.
pub fn spawn_listener(
    connect: impl Fn() -> Option<Client> + Send + 'static,
    extra: Value,
    events: Sender<UiEvent>,
    wake: Wake,
) {
    std::thread::spawn(move || {
        let mut attempt = 0_u32;
        loop {
            match connect().map(|client| client.subscribe(&[SHOW_POPUP_TOPIC], extra.clone())) {
                Some(Ok(mut stream)) => {
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
                Some(Err(e)) if e.code == "already_subscribed" => {
                    let _ = events.send(UiEvent::AnotherInstance);
                    wake();
                    return;
                }
                _ => {}
            }
            std::thread::sleep(next_backoff(attempt));
            attempt = attempt.saturating_add(1);
        }
    });
}

//! Dedicated IPC thread owning the blocking client transport.

use std::sync::mpsc;
use std::thread;

use clipmonger_ipc::proto::IpcError;
use serde_json::Value;
use uuid::Uuid;

/// Blocking IPC call used by the bridge thread.
pub trait Transport: Send {
    /// Call one IPC method and wait for its reply.
    ///
    /// # Errors
    ///
    /// Returns the daemon wire error when the call fails.
    fn call(&mut self, method: &str, params: &Value) -> Result<Value, IpcError>;
}

/// Command sent from the shell to the bridge thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCmd {
    /// Fetch daemon status.
    Status,
    /// Fetch one history page.
    ListHistory,
    /// Delete one clip.
    DeleteClip(Uuid),
    /// Pin or unpin one clip.
    PinClip(Uuid, bool),
    /// Paste one clip through the validated target.
    PasteClip(Uuid),
    /// Stop the daemon.
    Stop,
}

/// Event delivered from the bridge thread to the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEvent {
    /// Status reply body.
    Status(Value),
    /// History-list reply body.
    History(Value),
    /// Server-confirmed delete.
    Deleted(Uuid),
    /// Server-confirmed pin with the stored flag.
    Pinned(Uuid, bool),
    /// Paste accepted by the daemon.
    PasteDone(Uuid),
    /// Daemon refused paste; the popup stays open.
    PasteUnavailable,
    /// Daemon acknowledged stop.
    Stopped,
    /// Anything else, carrying the guard message.
    Error(String),
}

/// Bridge handle spawner; the thread owns the transport alone.
#[derive(Debug)]
pub struct Bridge;

impl Bridge {
    /// Spawn the thread and return the shell-side channel ends.
    #[must_use]
    pub fn spawn(transport: Box<dyn Transport>) -> (mpsc::Sender<UiCmd>, mpsc::Receiver<UiEvent>) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<UiCmd>();
        let (evt_tx, evt_rx) = mpsc::channel::<UiEvent>();
        thread::spawn(move || {
            let mut transport = transport;
            while let Ok(cmd) = cmd_rx.recv() {
                let outcome = transport.call(method_for(&cmd), &params_for(&cmd));
                let _ = evt_tx.send(handle(cmd, outcome));
            }
        });
        (cmd_tx, evt_rx)
    }
}

/// Map one command to its IPC method name.
fn method_for(cmd: &UiCmd) -> &'static str {
    match cmd {
        UiCmd::Status => "status",
        UiCmd::ListHistory => "history.list",
        UiCmd::DeleteClip(_) => "history.delete",
        UiCmd::PinClip(_, _) => "history.pin",
        UiCmd::PasteClip(_) => "paste.execute",
        UiCmd::Stop => "daemon.stop",
    }
}

/// Build the params object for one command.
fn params_for(cmd: &UiCmd) -> Value {
    match cmd {
        UiCmd::Status | UiCmd::ListHistory | UiCmd::Stop => serde_json::json!({}),
        UiCmd::DeleteClip(id) | UiCmd::PasteClip(id) => {
            serde_json::json!({ "id": id.to_string() })
        }
        UiCmd::PinClip(id, pinned) => {
            serde_json::json!({ "id": id.to_string(), "pinned": pinned })
        }
    }
}

/// Turn one transport outcome into shell events.
fn handle(cmd: UiCmd, outcome: Result<Value, IpcError>) -> UiEvent {
    match (cmd, outcome) {
        (UiCmd::Status, Ok(body)) => UiEvent::Status(body),
        (UiCmd::ListHistory, Ok(body)) => UiEvent::History(body),
        (UiCmd::DeleteClip(id), Ok(body)) => {
            if body.get("deleted").and_then(Value::as_bool) == Some(true) {
                UiEvent::Deleted(id)
            } else {
                UiEvent::Error(format!("unconfirmed delete for {id}"))
            }
        }
        (UiCmd::DeleteClip(id), Err(error)) => {
            UiEvent::Error(format!("delete failed for {id}: {}", error.message))
        }
        (UiCmd::PinClip(id, _), Ok(body)) => {
            if let Some(pinned) = body.get("pinned").and_then(Value::as_bool) {
                UiEvent::Pinned(id, pinned)
            } else {
                UiEvent::Error(format!("unconfirmed pin for {id}"))
            }
        }
        (UiCmd::PinClip(id, _), Err(error)) => {
            UiEvent::Error(format!("pin failed for {id}: {}", error.message))
        }
        (UiCmd::PasteClip(id), Ok(_)) => UiEvent::PasteDone(id),
        (UiCmd::Stop, Ok(_)) => UiEvent::Stopped,
        (UiCmd::Status | UiCmd::ListHistory | UiCmd::Stop, Err(error)) => {
            UiEvent::Error(error.message)
        }
        (UiCmd::PasteClip(_), Err(error)) if error.code == "unavailable" => {
            UiEvent::PasteUnavailable
        }
        (UiCmd::PasteClip(_), Err(error)) => UiEvent::Error(error.message),
    }
}

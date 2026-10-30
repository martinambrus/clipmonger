//! Dedicated IPC thread owning the blocking client transport.

use std::sync::mpsc;
use std::thread;

use clipmonger_core::model::PermanentClip;
use clipmonger_ipc::proto::methods::PermanentCreateParams;
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
    /// Paste one clip picked in the popup: into the target window (HWND
    /// value captured when the hotkey opened the popup) when known, else
    /// into the foreground window; the pick time (unix ms) lets the daemon
    /// refuse a request that would land late.
    PasteClipTo(Uuid, Option<u64>, i64),
    /// Mint a single-use gesture token (user picked a clip in the popup).
    HotkeyFired,
    /// Stop the daemon.
    Stop,
    /// Fetch the full config document.
    ConfigGet,
    /// Apply one config key.
    ConfigSet(String, Value),
    /// Fetch capabilities.
    CapsGet,
    /// Toggle monitoring pause.
    MonitorToggle,
    /// Fetch one clip payload.
    HistoryGet(Uuid),
    /// Purge removed clips.
    PurgeRemoved,
    /// Fetch all permanents.
    ListPermanents,
    /// Create one permanent.
    PermCreate(PermanentCreateParams),
    /// Update one permanent.
    PermUpdate(PermanentClip),
    /// Delete one permanent.
    PermDelete(Uuid),
    /// Fetch permanent groups.
    PermGroups,
    /// Create one permanent group.
    PermGroupCreate(String),
    /// Delete one permanent group.
    PermGroupDelete(String),
    /// Execute one permanent.
    ExecPermanent(Uuid),
    /// Create a backup ZIP at the given dest path.
    BackupCreate(String),
    /// Restore a backup ZIP over the profile.
    BackupRestore(String),
    /// Fetch the last N audit log lines.
    LogsTail(u64),
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
    /// Targeted paste failed; carries the daemon message.
    PasteFailed(String),
    /// Gesture token minted (`hotkey.fired` acknowledged).
    HotkeyAck,
    /// Daemon acknowledged stop.
    Stopped,
    /// Anything else, carrying the guard message.
    Error(String),
    /// Config document reply body.
    Config(Value),
    /// One config key applied.
    ConfigApplied(String),
    /// Capabilities reply body.
    Caps(Value),
    /// One clip reply body.
    HistoryClip(Value),
    /// Monitoring paused flag after a toggle.
    MonitorPaused(bool),
    /// Removed clips purged.
    Purged(u64),
    /// Permanents-list reply body.
    Permanents(Value),
    /// Permanent create/update confirmed.
    PermanentSaved,
    /// Server-confirmed permanent delete.
    PermanentDeleted(Uuid),
    /// Permanent-groups reply body.
    PermGroups(Value),
    /// Permanent group set changed.
    PermGroupsChanged,
    /// Backup created at the given path.
    BackupDone(String),
    /// Restore completed; history now serves restored clips.
    RestoreDone,
    /// Audit log tail lines.
    LogsTail(Vec<String>),
    /// Config-window error text (never an offline misreport).
    ConfigError(String),
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
#[must_use]
pub fn method_for(cmd: &UiCmd) -> &'static str {
    match cmd {
        UiCmd::Status => "status",
        UiCmd::ListHistory => "history.list",
        UiCmd::DeleteClip(_) => "history.delete",
        UiCmd::PinClip(_, _) => "history.pin",
        UiCmd::PasteClip(_) | UiCmd::PasteClipTo(..) => "paste.execute",
        UiCmd::HotkeyFired => "hotkey.fired",
        UiCmd::Stop => "daemon.stop",
        UiCmd::ConfigGet => "config.get",
        UiCmd::ConfigSet(_, _) => "config.set",
        UiCmd::CapsGet => "caps.get",
        UiCmd::MonitorToggle => "monitor.set",
        UiCmd::HistoryGet(_) => "history.get",
        UiCmd::PurgeRemoved => "history.purge_removed",
        UiCmd::ListPermanents => "permanent.list",
        UiCmd::PermCreate(_) => "permanent.create",
        UiCmd::PermUpdate(_) => "permanent.update",
        UiCmd::PermDelete(_) => "permanent.delete",
        UiCmd::PermGroups => "permanent.groups",
        UiCmd::PermGroupCreate(_) => "permanent.group_create",
        UiCmd::PermGroupDelete(_) => "permanent.group_delete",
        UiCmd::ExecPermanent(_) => "permanent.exec",
        UiCmd::BackupCreate(_) => "backup.create",
        UiCmd::BackupRestore(_) => "backup.restore",
        UiCmd::LogsTail(_) => "logs.tail",
    }
}

/// Build the params object for one command.
#[must_use]
pub fn params_for(cmd: &UiCmd) -> Value {
    match cmd {
        UiCmd::Status
        | UiCmd::ListHistory
        | UiCmd::Stop
        | UiCmd::ConfigGet
        | UiCmd::CapsGet
        | UiCmd::PurgeRemoved
        | UiCmd::ListPermanents
        | UiCmd::PermGroups => serde_json::json!({}),
        UiCmd::ConfigSet(key, value) => serde_json::json!({ "key": key, "value": value }),
        UiCmd::MonitorToggle => serde_json::json!({ "toggle": true }),
        UiCmd::HistoryGet(id) => serde_json::json!({ "clip": id.to_string() }),
        UiCmd::DeleteClip(id) | UiCmd::PasteClip(id) => {
            serde_json::json!({ "clip": id.to_string() })
        }
        UiCmd::PasteClipTo(id, target, picked_ms) => {
            let mut params = serde_json::json!({ "clip": id.to_string(), "picked_ms": picked_ms });
            if let Some(hwnd) = target {
                params["target_hwnd"] = Value::from(*hwnd);
            }
            params
        }
        UiCmd::HotkeyFired => serde_json::json!({ "id": 1 }),
        UiCmd::PermDelete(id) | UiCmd::ExecPermanent(id) => {
            serde_json::json!({ "id": id.to_string() })
        }
        UiCmd::PermCreate(params) => serde_json::to_value(params).unwrap_or(Value::Null),
        UiCmd::PermUpdate(clip) => {
            serde_json::json!({ "permanent": serde_json::to_value(clip).unwrap_or(Value::Null) })
        }
        UiCmd::PermGroupCreate(name) | UiCmd::PermGroupDelete(name) => {
            serde_json::json!({ "name": name })
        }
        UiCmd::BackupCreate(dest) => serde_json::json!({ "dest": dest }),
        UiCmd::BackupRestore(src) => serde_json::json!({ "src": src }),
        UiCmd::LogsTail(lines) => serde_json::json!({ "lines": lines }),
        UiCmd::PinClip(id, pinned) => {
            serde_json::json!({ "clip": id.to_string(), "pinned": pinned })
        }
    }
}

/// Turn one transport outcome into shell events.
#[allow(clippy::match_same_arms)]
#[allow(clippy::too_many_lines)]
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
        (UiCmd::ListPermanents, Ok(body)) => UiEvent::Permanents(body),
        (UiCmd::PermGroups, Ok(body)) => UiEvent::PermGroups(body),
        (UiCmd::PermCreate(_), Ok(_)) => UiEvent::PermanentSaved,
        (UiCmd::PermUpdate(_), Ok(body)) => {
            if body.get("updated").and_then(Value::as_bool) == Some(true) {
                UiEvent::PermanentSaved
            } else {
                UiEvent::Error("unconfirmed permanent update".to_string())
            }
        }
        (UiCmd::PermDelete(id), Ok(body)) => {
            if body.get("deleted").and_then(Value::as_bool) == Some(true) {
                UiEvent::PermanentDeleted(id)
            } else {
                UiEvent::Error(format!("unconfirmed permanent delete for {id}"))
            }
        }
        (UiCmd::PermGroupCreate(_), Ok(_)) => UiEvent::PermGroupsChanged,
        (UiCmd::PermGroupDelete(_), Ok(body)) => {
            if body.get("deleted").and_then(Value::as_bool) == Some(true) {
                UiEvent::PermGroupsChanged
            } else {
                UiEvent::Error("unconfirmed permanent group delete".to_string())
            }
        }
        (UiCmd::ExecPermanent(id), Ok(_)) => UiEvent::PasteDone(id),
        (UiCmd::PasteClip(id) | UiCmd::PasteClipTo(id, ..), Ok(_)) => UiEvent::PasteDone(id),
        (UiCmd::HotkeyFired, Ok(_)) => UiEvent::HotkeyAck,
        (UiCmd::Stop, Ok(_)) => UiEvent::Stopped,
        (UiCmd::ConfigGet, Ok(body)) => UiEvent::Config(body),
        (UiCmd::CapsGet, Ok(body)) => UiEvent::Caps(body),
        (UiCmd::HistoryGet(_), Ok(body)) => UiEvent::HistoryClip(body),
        (UiCmd::ConfigSet(key, _), Ok(body)) => {
            if body.get("ok").and_then(Value::as_bool) == Some(true) {
                UiEvent::ConfigApplied(key)
            } else {
                UiEvent::Error("unconfirmed config.set".to_string())
            }
        }
        (UiCmd::MonitorToggle, Ok(body)) => UiEvent::MonitorPaused(
            body.get("monitoring_paused")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
        (UiCmd::PurgeRemoved, Ok(body)) => {
            UiEvent::Purged(body.get("purged").and_then(Value::as_u64).unwrap_or(0))
        }
        (UiCmd::BackupCreate(_), Ok(body)) => {
            if let Some(path) = body.get("path").and_then(Value::as_str) {
                UiEvent::BackupDone(path.to_string())
            } else {
                UiEvent::ConfigError("unconfirmed backup.create".to_string())
            }
        }
        (UiCmd::BackupRestore(_), Ok(body)) => {
            if body.get("ok").and_then(Value::as_bool) == Some(true) {
                UiEvent::RestoreDone
            } else {
                UiEvent::ConfigError("unconfirmed backup.restore".to_string())
            }
        }
        (UiCmd::LogsTail(_), Ok(body)) => {
            if let Some(items) = body.get("lines").and_then(Value::as_array) {
                UiEvent::LogsTail(
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToString::to_string)
                        .collect(),
                )
            } else {
                UiEvent::ConfigError("unconfirmed logs.tail".to_string())
            }
        }
        (UiCmd::BackupCreate(_) | UiCmd::BackupRestore(_) | UiCmd::LogsTail(_), Err(error)) => {
            UiEvent::ConfigError(error.message)
        }
        (
            UiCmd::ConfigGet
            | UiCmd::CapsGet
            | UiCmd::HistoryGet(_)
            | UiCmd::ConfigSet(_, _)
            | UiCmd::MonitorToggle
            | UiCmd::PurgeRemoved
            | UiCmd::ListPermanents
            | UiCmd::PermGroups
            | UiCmd::PermCreate(_)
            | UiCmd::PermUpdate(_)
            | UiCmd::PermDelete(_)
            | UiCmd::PermGroupCreate(_)
            | UiCmd::PermGroupDelete(_),
            Err(error),
        ) => UiEvent::Error(error.message),
        (UiCmd::Status | UiCmd::ListHistory | UiCmd::Stop, Err(error)) => {
            UiEvent::Error(error.message)
        }
        (UiCmd::PasteClip(_) | UiCmd::ExecPermanent(_), Err(error))
            if error.code == "unavailable" || error.code == "denied" =>
        {
            UiEvent::PasteUnavailable
        }
        (UiCmd::PasteClip(_), Err(error)) => UiEvent::Error(error.message),
        (UiCmd::PasteClipTo(..), Err(error)) => UiEvent::PasteFailed(error.message),
        (UiCmd::HotkeyFired, Err(error)) => UiEvent::Error(error.message),
        (UiCmd::ExecPermanent(_), Err(error)) => UiEvent::Error(error.message),
    }
}

//! Daemon skeleton: single-instance lock, rendezvous files, capability
//! aggregation, headless script replay, and the IPC request handler.
//!
//! Implements the frozen binary contract (seg-02 S1/S9, D-F20/F21/F22).
//! No payload bytes are ever logged; only metadata reaches stderr.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use clipmonger_core::{CoreError, Store};
use clipmonger_ipc::proto::IpcError;
use serde_json::Value;

/// Protocol tag written to the rendezvous file.
pub const PROTOCOL: &str = "ipc/v1";
/// Daemon crate version reported by `status`.
pub const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Note carried by every skeleton capability snapshot.
pub const SKELETON_NOTE: &str = "no backends in foundation skeleton";

/// Single-instance profile lock backed by `std::fs::File::try_lock`.
///
/// The lock file is `<profile>/daemon.lock`. A second `acquire` on the same
/// profile (same or other process) fails with a message containing
/// `already_running`. The lock releases when the guard drops.
pub struct DaemonLock {
    /// Held open for the guard lifetime; drop closes and releases.
    _file: fs::File,
}

impl DaemonLock {
    /// Acquire the exclusive profile lock.
    ///
    /// # Errors
    ///
    /// Returns an error containing `already_running` when another daemon
    /// holds the lock, or an I/O error when the profile cannot be prepared.
    pub fn acquire(profile_dir: &Path) -> Result<Self, CoreError> {
        fs::create_dir_all(profile_dir).map_err(CoreError::Io)?;
        let file = fs::File::create(profile_dir.join("daemon.lock")).map_err(CoreError::Io)?;
        file.try_lock().map_err(|_| CoreError::Unsupported {
            message: "already_running: another daemon holds the profile lock".to_string(),
        })?;
        Ok(Self { _file: file })
    }
}

/// Write `<profile>/rendezvous.json` as `{socket, protocol}` (never a token).
///
/// # Errors
///
/// Returns an I/O error when the profile dir or file cannot be written.
pub fn write_rendezvous(profile_dir: &Path, socket_path: &Path) -> Result<(), CoreError> {
    fs::create_dir_all(profile_dir).map_err(CoreError::Io)?;
    let body = serde_json::json!({
        "socket": socket_path.to_string_lossy(),
        "protocol": PROTOCOL,
    });
    let text = serde_json::to_string(&body).unwrap_or_else(|_| {
        format!(
            r#"{{"socket":"{}","protocol":"{PROTOCOL}"}}"#,
            socket_path.to_string_lossy()
        )
    });
    fs::write(profile_dir.join("rendezvous.json"), text).map_err(CoreError::Io)?;
    Ok(())
}

/// Read `<profile>/rendezvous.json`, returning `(socket_path, protocol)`.
///
/// # Errors
///
/// Returns an error when the file is missing or lacks a `socket` string.
pub fn read_rendezvous(profile_dir: &Path) -> Result<(PathBuf, String), CoreError> {
    let raw = fs::read_to_string(profile_dir.join("rendezvous.json")).map_err(CoreError::Io)?;
    let value: Value = serde_json::from_str(&raw).map_err(|e| CoreError::Config {
        message: format!("rendezvous is not JSON: {e}"),
    })?;
    let socket = value
        .get("socket")
        .and_then(Value::as_str)
        .ok_or_else(|| CoreError::Config {
            message: "rendezvous lacks a socket path".to_string(),
        })?;
    let protocol = value
        .get("protocol")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL)
        .to_string();
    Ok((PathBuf::from(socket), protocol))
}

/// Skeleton capability aggregator over registered backend snapshots.
///
/// The foundation skeleton has no live backends: `snapshot` reports the
/// store-backed monitor as degraded and everything else unavailable with a
/// skeleton note. `set_extension_connected(false)` marks the (future) GNOME
/// entry unavailable via an extra note.
pub struct CapsAggregator {
    /// Backend snapshots by name, kept for future aggregation.
    registered: Mutex<HashMap<String, String>>,
    /// GNOME extension link state.
    extension_connected: Mutex<bool>,
}

impl CapsAggregator {
    /// Create an aggregator with the extension link up and no backends.
    #[must_use]
    pub fn new() -> Self {
        Self {
            registered: Mutex::new(HashMap::new()),
            extension_connected: Mutex::new(true),
        }
    }

    /// Register one backend's capability summary under `name`.
    pub fn register(&self, name: String, summary: String) {
        if let Ok(mut map) = self.registered.lock() {
            map.insert(name, summary);
        }
    }

    /// Flip the GNOME extension link state.
    pub fn set_extension_connected(&self, connected: bool) {
        if let Ok(mut flag) = self.extension_connected.lock() {
            *flag = connected;
        }
    }

    /// Aggregate snapshot as JSON (levels as lowercase strings).
    #[must_use]
    pub fn snapshot(&self) -> Value {
        let extension_up = self.extension_connected.lock().map_or(true, |f| *f);
        let mut notes = vec![Value::from(SKELETON_NOTE)];
        if !extension_up {
            notes.push(Value::from(
                "gnome extension disconnected: paste unavailable",
            ));
        }
        serde_json::json!({
            "monitor": {"level": "degraded", "reason": SKELETON_NOTE},
            "transfer": {"level": "unavailable", "reason": SKELETON_NOTE},
            "hotkey": {"level": "unavailable", "reason": SKELETON_NOTE},
            "paste": {"level": "unavailable", "strategy": "unavailable", "reason": SKELETON_NOTE},
            "tray": {"level": "unavailable", "reason": SKELETON_NOTE},
            "autostart": false,
            "primary_selection": false,
            "extension_connected": extension_up,
            "notes": notes,
        })
    }
}

impl Default for CapsAggregator {
    fn default() -> Self {
        Self::new()
    }
}

/// Current wall-clock time in unix millis.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

/// Fresh random id string for a replayed clip.
fn new_id(index: u64) -> String {
    if let Ok(raw) = fs::read_to_string("/proc/sys/kernel/random/uuid") {
        let trimmed = raw.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }
    format!("01900000-0000-7000-8000-{index:012x}")
}

/// Trivial hex fingerprint of bytes for the stored `content_hash` field.
///
/// The store computes its own canonical dedupe hash from payloads; this only
/// fills the row metadata.
fn fnv_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Store one capture event; returns the JSON report line for it.
fn process_capture(store: &Store, line: &str, index: u64) -> String {
    let event: Value = match serde_json::from_str(line) {
        Ok(event) => event,
        Err(_) => {
            return serde_json::json!({
                "accepted": false, "stored": false,
                "reason": "invalid_json", "index": index,
            })
            .to_string();
        }
    };
    if event.get("sensitive").and_then(Value::as_bool) == Some(true) {
        return serde_json::json!({
            "accepted": false, "stored": false,
            "reason": "sensitive", "index": index,
        })
        .to_string();
    }
    let now = now_ms();
    let origin = event
        .get("origin")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let payload = format!("headless replay {index}").into_bytes();
    let clip_json = serde_json::json!({
        "id": new_id(index),
        "created_ms": now,
        "source_os": "linux",
        "pinned": false,
        "removed": false,
        "preview": format!("headless replay {index}"),
        "search_text": format!("headless replay {index}"),
        "formats": ["text/plain"],
        "bytes_total": payload.len(),
        "content_hash": fnv_hex(&payload),
        "rev": {"wall_ms": now, "dev": "daemon", "ctr": 0},
        "last_used_ms": now,
        "origin_dev": "daemon",
        "origin": origin,
    });
    let clip: clipmonger_core::model::Clip = match serde_json::from_value(clip_json) {
        Ok(clip) => clip,
        Err(e) => {
            return serde_json::json!({
                "accepted": false, "stored": false,
                "reason": format!("clip build failed: {e}"), "index": index,
            })
            .to_string();
        }
    };
    match store.insert_clip(&clip, &[("text/plain".to_string(), payload)]) {
        Ok(_) => serde_json::json!({
            "accepted": true, "stored": true, "index": index,
        })
        .to_string(),
        Err(e) => serde_json::json!({
            "accepted": false, "stored": false,
            "reason": format!("store failed: {e}"), "index": index,
        })
        .to_string(),
    }
}

/// Headless `--script` semantics: one JSON report line per input line.
///
/// Sensitive events are counted but record nothing. Pure apart from the
/// store writes; no sockets involved.
#[must_use]
pub fn replay_script(store: &Store, lines: impl Iterator<Item = String>) -> Vec<String> {
    lines
        .enumerate()
        .map(|(index, line)| {
            process_capture(store, &line, u64::try_from(index).unwrap_or_default())
        })
        .collect()
}

/// Shared daemon request state behind the IPC handler.
pub struct DaemonState {
    /// Clip store for history methods.
    store: Store,
    /// Capability aggregator for status/caps methods.
    caps: CapsAggregator,
    /// Monitor pause flag toggled by `monitor.set`.
    monitoring_paused: Mutex<bool>,
}

impl DaemonState {
    /// Wrap an opened store with fresh skeleton caps.
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self {
            store,
            caps: CapsAggregator::new(),
            monitoring_paused: Mutex::new(false),
        }
    }

    /// Capability snapshot for `status`/`caps.*`.
    #[must_use]
    pub fn caps_snapshot(&self) -> Value {
        self.caps.snapshot()
    }

    /// Find a stored clip id by its string form without naming the id type.
    fn find_clip(&self, id: &str) -> Option<clipmonger_core::model::Clip> {
        let clips = self
            .store
            .list_history(u64::MAX, 0, true)
            .unwrap_or_default();
        clips.into_iter().find(|c| c.id.to_string() == id)
    }

    /// Whether the monitor is currently paused.
    fn paused(&self) -> bool {
        self.monitoring_paused.lock().is_ok_and(|flag| *flag)
    }

    /// Requested clip id from `params`, looked up in the store.
    fn requested_clip(&self, params: &Value) -> Option<clipmonger_core::model::Clip> {
        let id = params
            .get("clip")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.find_clip(&id)
    }

    /// Dispatch one `history.*` method.
    fn handle_history(&self, method: &str, params: &Value) -> Result<Value, IpcError> {
        match method {
            "history.list" => {
                let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(100);
                let offset = params.get("offset").and_then(Value::as_u64).unwrap_or(0);
                let include_removed = params
                    .get("include_removed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let clips = self
                    .store
                    .list_history(limit, offset, include_removed)
                    .map_err(|e| IpcError::new("backend", e.to_string()))?;
                Ok(serde_json::json!({"clips": clips}))
            }
            "history.get" => match self.requested_clip(params) {
                Some(found) => match self.store.get_clip(found.id) {
                    Ok(Some((clip, _))) => Ok(serde_json::json!({"clip": clip})),
                    Ok(None) => Ok(serde_json::json!({"clip": null})),
                    Err(e) => Err(IpcError::new("backend", e.to_string())),
                },
                None => Ok(serde_json::json!({"clip": null})),
            },
            "history.delete" => match self.requested_clip(params) {
                Some(found) => self
                    .store
                    .soft_delete(found.id)
                    .map_err(|e| IpcError::new("backend", e.to_string()))
                    .map(|()| serde_json::json!({"deleted": true})),
                None => Ok(serde_json::json!({"deleted": false})),
            },
            "history.pin" => {
                let pinned = params
                    .get("pinned")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                match self.requested_clip(params) {
                    Some(found) => match self.store.get_clip(found.id) {
                        Ok(Some((mut clip, payloads))) => {
                            clip.pinned = pinned;
                            self.store
                                .insert_clip(&clip, &payloads)
                                .map_err(|e| IpcError::new("backend", e.to_string()))
                                .map(|_| serde_json::json!({"pinned": pinned}))
                        }
                        Ok(None) => Ok(serde_json::json!({"pinned": false})),
                        Err(e) => Err(IpcError::new("backend", e.to_string())),
                    },
                    None => Ok(serde_json::json!({"pinned": false})),
                }
            }
            _ => {
                let purged = self.store.purge_removed();
                Ok(serde_json::json!({"purged": purged}))
            }
        }
    }

    /// Dispatch `monitor.set`: explicit state wins, `toggle` flips the flag.
    fn handle_monitor(&self, params: &Value) -> Result<Value, IpcError> {
        let mut flag = self
            .monitoring_paused
            .lock()
            .map_err(|_| IpcError::new("internal", "monitor flag poisoned".to_string()))?;
        if params.get("toggle").and_then(Value::as_bool) == Some(true) {
            *flag = !*flag;
        } else if let Some(state) = params.get("state") {
            *flag = state.as_str().unwrap_or("On") != "On";
        }
        let paused = *flag;
        drop(flag);
        Ok(serde_json::json!({"ok": true, "monitoring_paused": paused}))
    }

    /// Dispatch `capture.report` without recording sensitive events.
    fn handle_capture(&self, params: &Value) -> Value {
        let sensitive = params
            .get("event")
            .and_then(|event| event.get("sensitive"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if sensitive {
            return serde_json::json!({"accepted": false});
        }
        let line = params
            .get("event")
            .map_or_else(|| "{}".to_string(), Value::to_string);
        let stamp = u64::try_from(now_ms().max(0)).unwrap_or_default();
        let report = process_capture(&self.store, &line, stamp);
        let accepted = serde_json::from_str::<Value>(&report)
            .ok()
            .and_then(|value| value.get("accepted").and_then(Value::as_bool))
            .unwrap_or(false);
        serde_json::json!({"accepted": accepted})
    }

    /// Dispatch one IPC method to a JSON result or typed wire error.
    ///
    /// # Errors
    ///
    /// Returns the store's error for failing history methods, or the
    /// skeleton `unavailable` error for `paste.execute`.
    pub fn handle(&self, method: &str, params: &Value) -> Result<Value, IpcError> {
        match method {
            "status" => Ok(serde_json::json!({
                "caps": self.caps.snapshot(),
                "up": true,
                "running": true,
                "version": DAEMON_VERSION,
                "monitoring_paused": self.paused(),
            })),
            "caps.get" | "caps.refresh" => Ok(self.caps.snapshot()),
            "ping" => Ok(serde_json::json!({"pong": true})),
            "paste.execute" => Err(IpcError::new(
                "unavailable",
                format!("paste unavailable: {SKELETON_NOTE}"),
            )),
            method if method.starts_with("history.") => self.handle_history(method, params),
            "monitor.set" => self.handle_monitor(params),
            "capture.report" => Ok(self.handle_capture(params)),
            _ => Ok(serde_json::json!({"ok": true})),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::CapsAggregator;

    #[test]
    fn caps_extension_flip_marks_gnome_unavailable() {
        let caps = CapsAggregator::new();
        let connected = caps.snapshot();
        assert_eq!(
            connected
                .get("extension_connected")
                .and_then(Value::as_bool),
            Some(true)
        );
        caps.set_extension_connected(false);
        let flipped = caps.snapshot();
        assert_eq!(
            flipped.get("extension_connected").and_then(Value::as_bool),
            Some(false)
        );
        let gnome_noted = flipped
            .get("notes")
            .and_then(Value::as_array)
            .is_some_and(|notes| {
                notes
                    .iter()
                    .any(|note| note.as_str().unwrap_or_default().contains("gnome"))
            });
        assert!(
            gnome_noted,
            "flipped snapshot must note the gnome entry, got: {flipped}"
        );
        assert_eq!(
            flipped
                .get("monitor")
                .and_then(|m| m.get("level"))
                .and_then(Value::as_str),
            Some("degraded")
        );
    }
}

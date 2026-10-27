//! Backup/logs bridge mapping (Step 6 task 2).
//!
//! Pins `UiCmd::{BackupCreate, BackupRestore, LogsTail}` to their IPC
//! methods and params, and the bridge outcomes to `UiEvent`
//! (`BackupDone`/`RestoreDone`/`LogsTail`/`ConfigError`, never a
//! hang or an offline misreport).
//!
//! Run: `cargo test -p clipmonger-app-ui --test backup_logs_ui_acceptance`

use std::time::Duration;

use clipmonger_app_ui::ipc_bridge::{self, UiCmd, UiEvent};
use serde_json::{json, Value};

struct Fake {
    respond: fn(&str, &Value) -> Result<Value, clipmonger_ipc::proto::IpcError>,
}

impl ipc_bridge::Transport for Fake {
    fn call(
        &mut self,
        method: &str,
        params: &Value,
    ) -> Result<Value, clipmonger_ipc::proto::IpcError> {
        (self.respond)(method, params)
    }
}

#[allow(clippy::unnecessary_wraps)] // trait-shaped test double: must return Result
fn ok_backup(_method: &str, _params: &Value) -> Result<Value, clipmonger_ipc::proto::IpcError> {
    Ok(json!({"path": "/tmp/p/backup.zip"}))
}

#[allow(clippy::unnecessary_wraps)] // trait-shaped test double: must return Result
fn ok_restore(_method: &str, _params: &Value) -> Result<Value, clipmonger_ipc::proto::IpcError> {
    Ok(json!({"ok": true}))
}

#[allow(clippy::unnecessary_wraps)] // trait-shaped test double: must return Result
fn ok_tail(_method: &str, _params: &Value) -> Result<Value, clipmonger_ipc::proto::IpcError> {
    Ok(json!({"lines": ["{\"a\":1}", "{\"a\":2}"]}))
}

fn denied(_method: &str, _params: &Value) -> Result<Value, clipmonger_ipc::proto::IpcError> {
    Err(clipmonger_ipc::proto::IpcError::new(
        "backend",
        "backup.create: dest required".to_string(),
    ))
}

#[test]
fn backup_cmds_reach_daemon_methods_with_params() {
    assert_eq!(
        ipc_bridge::method_for(&UiCmd::BackupCreate("/x".into())),
        "backup.create"
    );
    assert_eq!(
        ipc_bridge::params_for(&UiCmd::BackupCreate("/x/b.zip".into())),
        json!({"dest": "/x/b.zip"})
    );
    assert_eq!(
        ipc_bridge::method_for(&UiCmd::BackupRestore("/x".into())),
        "backup.restore"
    );
    assert_eq!(
        ipc_bridge::params_for(&UiCmd::BackupRestore("/x/b.zip".into())),
        json!({"src": "/x/b.zip"})
    );
    assert_eq!(ipc_bridge::method_for(&UiCmd::LogsTail(100)), "logs.tail");
    assert_eq!(
        ipc_bridge::params_for(&UiCmd::LogsTail(100)),
        json!({"lines": 100})
    );
}

#[test]
fn backup_bridge_success_events_carry_daemon_replies() {
    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Fake { respond: ok_backup }));
    tx.send(UiCmd::BackupCreate("/tmp/p/backup.zip".into()))
        .expect("send backup");
    let ev = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("backup event");
    assert!(
        matches!(&ev, UiEvent::BackupDone(path) if path == "/tmp/p/backup.zip"),
        "backup.create ok must yield BackupDone, got {ev:?}"
    );

    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Fake {
        respond: ok_restore,
    }));
    tx.send(UiCmd::BackupRestore("/tmp/p/backup.zip".into()))
        .expect("send restore");
    let ev = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("restore event");
    assert!(
        matches!(ev, UiEvent::RestoreDone),
        "backup.restore ok must yield RestoreDone, got {ev:?}"
    );

    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Fake { respond: ok_tail }));
    tx.send(UiCmd::LogsTail(100)).expect("send tail");
    let ev = rx.recv_timeout(Duration::from_secs(2)).expect("tail event");
    assert!(
        matches!(&ev, UiEvent::LogsTail(lines) if lines == &vec!["{\"a\":1}".to_string(), "{\"a\":2}".to_string()]),
        "logs.tail ok must yield LogsTail lines, got {ev:?}"
    );
}

#[test]
fn backup_bridge_errors_surface_as_config_error() {
    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Fake { respond: denied }));
    tx.send(UiCmd::BackupCreate(String::new()))
        .expect("send backup");
    let ev = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("backup event");
    assert!(
        matches!(&ev, UiEvent::ConfigError(m) if m.contains("dest required")),
        "backup error must surface as ConfigError text, got {ev:?}"
    );

    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Fake { respond: denied }));
    tx.send(UiCmd::LogsTail(100)).expect("send tail");
    let ev = rx.recv_timeout(Duration::from_secs(2)).expect("tail event");
    assert!(
        matches!(&ev, UiEvent::ConfigError(_)),
        "logs error must surface as ConfigError text, got {ev:?}"
    );
}

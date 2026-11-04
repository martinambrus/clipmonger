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
        matches!(ev, UiEvent::RestoreDone { warning: None }),
        "backup.restore ok must yield RestoreDone, got {ev:?}"
    );

    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Fake { respond: ok_tail }));
    tx.send(UiCmd::LogsTail(100)).expect("send tail");
    let ev = rx.recv_timeout(Duration::from_secs(2)).expect("tail event");
    assert!(
        matches!(&ev, UiEvent::LogsTail { source, lines, sink_error } if source == "audit" && sink_error.is_none() && lines == &vec!["{\"a\":1}".to_string(), "{\"a\":2}".to_string()]),
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
        matches!(&ev, UiEvent::LogsTailError { source, message } if source == "audit" && message.contains("dest required")),
        "logs error must retain audit source and error text, got {ev:?}"
    );
}

#[test]
fn runtime_bridge_preserves_source_lines_sink_error_and_malformed_identity() {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "callback signature required by test transport contract"
    )]
    fn runtime_reply(
        method: &str,
        params: &Value,
    ) -> Result<Value, clipmonger_ipc::proto::IpcError> {
        assert_eq!(method, "logs.tail");
        assert_eq!(params, &json!({"source":"daemon","lines":2}));
        Ok(json!({"lines":["readable diagnostic"],"sink_error":"runtime_log.refused_file"}))
    }
    #[expect(
        clippy::unnecessary_wraps,
        reason = "callback signature required by test transport contract"
    )]
    fn malformed(_: &str, _: &Value) -> Result<Value, clipmonger_ipc::proto::IpcError> {
        Ok(json!({"lines":[12]}))
    }
    #[expect(
        clippy::unnecessary_wraps,
        reason = "callback signature required by test transport contract"
    )]
    fn missing(_: &str, _: &Value) -> Result<Value, clipmonger_ipc::proto::IpcError> {
        Ok(json!({}))
    }
    let cmd = UiCmd::LogsTailSource {
        source: "daemon".into(),
        lines: 2,
    };
    assert_eq!(
        ipc_bridge::params_for(&cmd),
        json!({"source":"daemon","lines":2})
    );
    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Fake {
        respond: runtime_reply,
    }));
    tx.send(cmd).unwrap();
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        UiEvent::LogsTail {
            source: "daemon".into(),
            lines: vec!["readable diagnostic".into()],
            sink_error: Some("runtime_log.refused_file".into())
        }
    );
    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Fake { respond: malformed }));
    tx.send(UiCmd::LogsTailSource {
        source: "ui".into(),
        lines: 2,
    })
    .unwrap();
    assert!(
        matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(),UiEvent::LogsTailError {source,message} if source=="ui" && message.contains("invalid"))
    );
    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Fake { respond: missing }));
    tx.send(UiCmd::LogsTail(100)).unwrap();
    assert!(
        matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(),UiEvent::LogsTailError {source,..} if source=="audit")
    );
    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Fake { respond: denied }));
    tx.send(UiCmd::LogsTailSource {
        source: "daemon".into(),
        lines: 2,
    })
    .unwrap();
    assert!(
        matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(),UiEvent::LogsTailError {source,message} if source=="daemon" && message.contains("dest required"))
    );
}

#[test]
fn ui_owned_initialization_status_enters_actual_bridge_reply() {
    use clipmonger_core::runtime_log::{self, Role};
    let dir = std::env::temp_dir().join(format!(
        "cm-runtime-ui-bridge-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("logs"), "obstruction").unwrap();
    assert!(runtime_log::init_global(&dir, Role::Ui).is_err());
    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Fake { respond: ok_tail }));
    tx.send(UiCmd::LogsTailSource {
        source: "ui".into(),
        lines: 100,
    })
    .unwrap();
    let event = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        matches!(event,UiEvent::LogsTail {source,lines,sink_error:Some(error)} if source=="ui" && lines.len()==2 && error=="runtime_log.refused_logs_directory")
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn restore_warning_codes_and_malformed_payload_remain_successes() {
    use ipc_bridge::RestoreWarning;
    struct Reply(Value);
    impl ipc_bridge::Transport for Reply {
        fn call(&mut self, _: &str, _: &Value) -> Result<Value, clipmonger_ipc::proto::IpcError> {
            Ok(self.0.clone())
        }
    }
    for (warning, expected) in [
        (json!({"code":"apply_failed"}), RestoreWarning::ApplyFailed),
        (json!({"code":"unavailable"}), RestoreWarning::Unavailable),
        (json!({"code":"portable"}), RestoreWarning::Portable),
        (json!({"code":"future"}), RestoreWarning::Unknown),
        (json!(null), RestoreWarning::Unknown),
    ] {
        let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Reply(
            json!({"ok":true,"autostart_warning":warning}),
        )));
        tx.send(UiCmd::BackupRestore("backup.zip".into())).unwrap();
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), UiEvent::RestoreDone { warning: Some(actual) } if actual == expected)
        );
    }
}

#[test]
fn config_set_refusal_names_the_key() {
    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Fake { respond: denied }));
    tx.send(UiCmd::ConfigSet("history.max_items".into(), json!(0)))
        .expect("send config.set");
    let ev = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("config.set event");
    assert!(
        matches!(&ev, UiEvent::ConfigSetFailed { key, message } if key == "history.max_items" && message.contains("dest required")),
        "config.set error must yield ConfigSetFailed for its key, got {ev:?}"
    );
}

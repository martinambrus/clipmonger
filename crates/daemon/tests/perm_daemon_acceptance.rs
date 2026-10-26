//! PERM daemon acceptance rulers D1-D8 (permanents v1, perm.md §2/§5).
//!
//! Failing baseline: drives the public `DaemonState::handle` arms the
//! builder must add (`permanent.list/create/update/delete/groups/
//! group_create/group_delete/exec` via one `permanent.*` dispatch arm;
//! unknown sub-methods stay `unknown_method`). No backend feature is
//! enabled in this build, so exec delivery is the honest skeleton
//! `unavailable` error (P-F5); real delivery is D6, backend-gated.
//! Param key is `id`, never `clip`, for permanents.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use clipmonger_core::Store;
use clipmonger_daemon::DaemonState;
use serde_json::{json, Value};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn fresh_state(tag: &str) -> (PathBuf, DaemonState) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "cm-perm-daemon-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir).expect("open store");
    (dir, DaemonState::new(store))
}

fn create_sig(state: &DaemonState) -> Value {
    state
        .handle(
            "permanent.create",
            &json!({ "group": "Work", "name": "Sig", "body": "Regards, A" }),
        )
        .expect("permanent.create")
        .get("permanent")
        .cloned()
        .expect("create result carries permanent")
}

fn list(state: &DaemonState) -> Vec<Value> {
    state
        .handle("permanent.list", &json!({}))
        .expect("permanent.list")
        .get("permanents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

// D1 CRUD over IPC.
#[test]
fn d1_crud_over_ipc() {
    let (_dir, state) = fresh_state("d1");
    let created = create_sig(&state);
    let id = created
        .get("id")
        .and_then(Value::as_str)
        .expect("string id")
        .to_string();
    assert_eq!(created.get("kind").and_then(Value::as_str), Some("Paste"));
    assert_eq!(created.get("group").and_then(Value::as_str), Some("Work"));
    assert_eq!(created.get("name").and_then(Value::as_str), Some("Sig"));
    assert_eq!(created.get("js"), Some(&Value::Null));

    let listed = list(&state);
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].get("id").and_then(Value::as_str),
        Some(id.as_str())
    );
    // Group filter arm.
    assert_eq!(
        state
            .handle("permanent.list", &json!({ "group": "Work" }))
            .expect("filtered")
            .get("permanents")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        state
            .handle("permanent.list", &json!({ "group": "Nope" }))
            .expect("filtered")
            .get("permanents")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0)
    );

    let groups = state
        .handle("permanent.groups", &json!({}))
        .expect("groups");
    assert_eq!(
        groups,
        json!({ "groups": [{ "name": "Work", "system": false, "count": 1 }] })
    );

    let mut edited = created.clone();
    edited["name"] = json!("Signature");
    assert_eq!(
        state
            .handle("permanent.update", &json!({ "permanent": edited }))
            .expect("update"),
        json!({ "updated": true })
    );
    assert_eq!(
        list(&state)[0].get("name").and_then(Value::as_str),
        Some("Signature")
    );

    assert_eq!(
        state
            .handle("permanent.delete", &json!({ "id": id }))
            .expect("delete"),
        json!({ "deleted": true })
    );
    assert_eq!(list(&state), [] as [serde_json::Value; 0]);
}

// D2 unknown/missing honesty.
#[test]
fn d2_unknown_and_missing_honesty() {
    let (_dir, state) = fresh_state("d2");
    let missing = "11111111-1111-7111-8111-111111111111";
    assert_eq!(
        state
            .handle("permanent.delete", &json!({ "id": missing }))
            .expect("delete missing is Ok(false)"),
        json!({ "deleted": false })
    );
    let ghost = json!({
        "id": missing, "group": "Work", "name": "Ghost", "hotkey": null,
        "kind": "Paste", "body": "boo", "js": null,
        "rev": { "wall_ms": 1, "dev": "test", "ctr": 0 }
    });
    assert_eq!(
        state
            .handle("permanent.update", &json!({ "permanent": ghost }))
            .expect("update missing is Ok(false)"),
        json!({ "updated": false })
    );
    let err = state
        .handle("permanent.bogus", &json!({}))
        .expect_err("bogus sub-method");
    assert_eq!(err.code, "unknown_method");
    let err = state
        .handle("permanent.exec", &json!({ "id": missing }))
        .expect_err("exec of unknown permanent");
    assert_eq!(err.code, "backend", "got {err:?}");
    assert!(
        err.message.contains("unknown permanent"),
        "exec missing message must say unknown permanent: {}",
        err.message
    );
    // The history family's `clip` key must not be accepted as a permanent id.
    let err = state
        .handle("permanent.exec", &json!({ "clip": missing }))
        .expect_err("exec with clip key must not resolve a permanent");
    assert_ne!(
        err.code, "unknown_method",
        "permanent.exec itself is a real method"
    );
}

// D3 validation surfaces + updated unknown-method pinning.
#[test]
fn d3_validation_surfaces() {
    let (_dir, state) = fresh_state("d3");
    let err = state
        .handle(
            "permanent.create",
            &json!({ "group": "Work", "name": "", "body": "x" }),
        )
        .expect_err("empty name");
    assert_eq!(err.code, "config", "got {err:?}");
    let err = state
        .handle(
            "permanent.create",
            &json!({ "group": "Work", "name": "Sig", "body": "" }),
        )
        .expect_err("empty body");
    assert_eq!(err.code, "config", "got {err:?}");
    let err = state
        .handle(
            "permanent.create",
            &json!({ "group": "", "name": "Sig", "body": "x" }),
        )
        .expect_err("empty group");
    assert_eq!(err.code, "config", "got {err:?}");
    assert!(list(&state).is_empty(), "invalid creates write nothing");

    state
        .handle(
            "permanent.create",
            &json!({ "group": "Work", "name": "One", "hotkey": "ctrl+alt+1", "body": "one" }),
        )
        .expect("first hotkey");
    let err = state
        .handle(
            "permanent.create",
            &json!({ "group": "Work", "name": "Two", "hotkey": "ctrl+alt+1", "body": "two" }),
        )
        .expect_err("duplicate hotkey");
    assert_eq!(err.code, "config", "got {err:?}");

    // permanent.exec is real now; genuinely unknown methods stay unknown.
    for method in [
        "permanent.bogus",
        "hotkey.register",
        "hotkey.unregister",
        "ui.show_popup",
    ] {
        let err = state
            .handle(method, &json!({}))
            .expect_err("must stay unknown");
        assert_eq!(err.code, "unknown_method", "{method}");
    }
    let err = state
        .handle(
            "permanent.exec",
            &json!({ "id": "11111111-1111-7111-8111-111111111111" }),
        )
        .expect_err("exec is dispatched, not unknown");
    assert_ne!(err.code, "unknown_method");
}

// D4 exec with no backend: honest unavailable; stored body never in errors/logs.
#[test]
fn d4_exec_no_backend_unavailable_and_body_never_surfaced() {
    let (_dir, state) = fresh_state("d4");
    let created = state
        .handle(
            "permanent.create",
            &json!({ "group": "Work", "name": "Secret", "body": "SECRET-BODY-TEXT-12345" }),
        )
        .expect("create")
        .get("permanent")
        .cloned()
        .unwrap();
    let id = created
        .get("id")
        .and_then(Value::as_str)
        .unwrap()
        .to_string();
    let err = state
        .handle("permanent.exec", &json!({ "id": id }))
        .expect_err("no-backend build must refuse exec honestly");
    assert_eq!(err.code, "unavailable", "got {err:?}");
    assert!(
        !err.message.contains("SECRET-BODY-TEXT-12345"),
        "stored body must never appear in error/log text: {}",
        err.message
    );
    // Literal paste pin (perm.md §7): [RUN]/[VAR] text is data in v1; the
    // refusal above must be about delivery, and the stored body is verbatim.
    let listed = list(&state);
    assert_eq!(
        listed[0].get("body").and_then(Value::as_str),
        Some("SECRET-BODY-TEXT-12345")
    );
}

// D5 Macro/System kinds rejected at create/update with config (P-F3);
// a Paste row can therefore never be exec'd as a macro through IPC.
#[test]
fn d5_macro_kind_rejected() {
    let (_dir, state) = fresh_state("d5");
    let created = create_sig(&state);
    let mut macro_clip = created.clone();
    macro_clip["kind"] = json!("Macro");
    let err = state
        .handle("permanent.update", &json!({ "permanent": macro_clip }))
        .expect_err("Macro update must be rejected in v1");
    assert_eq!(err.code, "config", "got {err:?}");
    let mut system_clip = created;
    system_clip["kind"] = json!("System");
    let err = state
        .handle("permanent.update", &json!({ "permanent": system_clip }))
        .expect_err("System update must be rejected in v1");
    assert_eq!(err.code, "config", "got {err:?}");
    assert_eq!(
        list(&state)[0].get("kind").and_then(Value::as_str),
        Some("Paste")
    );
}

// D6 exec delivery (backend-gated): real backends only, manual/backend CI.
// Ignored in the headless no-backend harness by design (perm.md D6: do not
// fake delivery the daemon cannot perform headlessly).
#[test]
#[ignore = "backend delivery: run with a real backend feature (wayland/windows)"]
fn d6_exec_delivery_backend_gated() {
    let (_dir, state) = fresh_state("d6");
    let created = create_sig(&state);
    let id = created
        .get("id")
        .and_then(Value::as_str)
        .unwrap()
        .to_string();
    let ok = state
        .handle("permanent.exec", &json!({ "id": id }))
        .expect("backend build must deliver Paste permanents");
    assert_eq!(ok, json!({ "ok": true }));
}

// D7 group arms over IPC: create empty, delete empty, refuse non-empty.
#[test]
fn d7_group_arms() {
    let (_dir, state) = fresh_state("d7");
    assert_eq!(
        state
            .handle("permanent.group_create", &json!({ "name": "Empty" }))
            .expect("group_create"),
        json!({ "created": true })
    );
    assert_eq!(
        state
            .handle("permanent.group_create", &json!({ "name": "Empty" }))
            .expect("group_create again"),
        json!({ "created": false })
    );
    assert_eq!(
        state
            .handle("permanent.groups", &json!({}))
            .expect("groups"),
        json!({ "groups": [{ "name": "Empty", "system": false, "count": 0 }] })
    );
    create_sig(&state);
    assert_eq!(
        state
            .handle("permanent.group_delete", &json!({ "name": "Work" }))
            .expect("delete non-empty"),
        json!({ "deleted": false }),
        "non-empty group delete refused, clips intact"
    );
    assert_eq!(list(&state).len(), 1);
    assert_eq!(
        state
            .handle("permanent.group_delete", &json!({ "name": "Empty" }))
            .expect("delete empty"),
        json!({ "deleted": true })
    );
    assert_eq!(
        state
            .handle("permanent.group_delete", &json!({ "name": "Missing" }))
            .expect("delete missing"),
        json!({ "deleted": false })
    );
}

// D8 exec/delete by id keeps the daemon responsive (CLI `exec-permanent`
// smoke analogue at handle level; CLI surface stays exec-only, no CRUD verbs).
#[test]
fn d8_exec_missing_keeps_daemon_responsive() {
    let (_dir, state) = fresh_state("d8");
    let missing = "22222222-2222-7222-8222-222222222222";
    let err = state
        .handle("permanent.exec", &json!({ "id": missing }))
        .expect_err("missing permanent exec fails");
    assert_eq!(err.code, "backend");
    let status = state
        .handle("status", &json!({}))
        .expect("daemon still up after failed exec");
    assert_eq!(status.get("up").and_then(Value::as_bool), Some(true));
    // And the full create -> exec-attempt cycle still works afterwards.
    let created = create_sig(&state);
    let id = created
        .get("id")
        .and_then(Value::as_str)
        .unwrap()
        .to_string();
    let err = state
        .handle("permanent.exec", &json!({ "id": id }))
        .expect_err("no backend");
    assert_eq!(err.code, "unavailable");
    assert_eq!(
        state
            .handle("status", &json!({}))
            .expect("still up")
            .get("up")
            .and_then(Value::as_bool),
        Some(true)
    );
}

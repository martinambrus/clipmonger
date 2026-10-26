//! PERM UI acceptance rulers U1-U10 (permanents v1, perm.md §3/§5).
//! Pure logic only: no egui runtime, no daemon process.
//!
//! Failing baseline: names the builder API from perm.md §3 —
//!   new module `clipmonger_app_ui::permanents`:
//!     `PermanentDraft { id: Option<Uuid>, group: String, name: String,
//!                       hotkey: String /* "" = none */, body: String }`
//!     `PermanentsVm { permanents: Vec<PermanentClip>, active_group: usize,
//!                     selected: usize, draft: Option<PermanentDraft>, .. }`
//!     `PermanentsVm::new()`, `apply_list(&Value) -> bool`,
//!     `apply_groups(&Value) -> bool`, `group_names() -> Vec<String>`,
//!     `visible(query: &str) -> Vec<usize>` (indices into `permanents`),
//!     `select_group(n: usize)` (clamps, resets `selected`),
//!     `begin_new()`, `begin_edit(id: Uuid)`,
//!     `validate_draft(&self) -> Result<PermanentCreateParams, &'static str>`
//!       (Err is a strings-module id, never a literal sentence),
//!     `remove_confirmed(id: Uuid)` (only after `{"deleted":true}`),
//!     `activate_selected() -> Option<Uuid>` (ruler sharpening: the pure
//!       hook the shell turns into `VmEffect::ExecPermanent(id)`).
//!   `popup::VmEffect::GroupTab(u8)` and `VmEffect::ExecPermanent(Uuid)`.
//!   `ipc_bridge::UiCmd::{ListPermanents, PermCreate(PermanentCreateParams),
//!     PermUpdate(PermanentClip), PermDelete(Uuid), PermGroups,
//!     PermGroupCreate(String), PermGroupDelete(String), ExecPermanent(Uuid)}`
//!   `ipc_bridge::UiEvent::{Permanents(Value), PermanentSaved,
//!     PermanentDeleted(Uuid), PermGroups(Value), PermGroupsChanged}`
//!     with `permanent.*` wire params using `{"id": ..}`, never `{"clip": ..}`.
//!   `config_model::visible_rows(tab: Tab) -> Vec<usize>` (ruler sharpening:
//!     pure row set the Permanent Clips tab renders; perm.md §1.2/U10).
//! Freeze: P-F1 DB groups authoritative (ROWS entry kept, tab excludes it),
//! P-F4 hotkeys stored/shown not registered, P-F6 confirm-then-delete,
//! P-F8 saves are immediate per-action IPC, never `ConfigDoc` Apply staging.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use clipmonger_app_ui::config_model::{self, Tab};
use clipmonger_app_ui::ipc_bridge::{self, UiCmd, UiEvent};
use clipmonger_app_ui::permanents::{PermanentDraft, PermanentsVm};
use clipmonger_app_ui::popup::{PopupVm, VmEffect};
use clipmonger_app_ui::{chord, keymap, strings};
use clipmonger_core::model::PermanentClip;
use clipmonger_ipc::proto::methods::{PermanentCreateParams, PermanentExecParams};
use clipmonger_ipc::proto::IpcError;
use serde_json::{json, Value};
use uuid::Uuid;

const ID_A: &str = "11111111-1111-7111-8111-111111111111";
const ID_B: &str = "22222222-2222-7222-8222-222222222222";
const ID_C: &str = "33333333-3333-7333-8333-333333333333";
const ID_SYS: &str = "44444444-4444-7444-8444-444444444444";

fn perm_json(
    id: &str,
    group: &str,
    name: &str,
    hotkey: Option<&str>,
    kind: &str,
    body: &str,
) -> Value {
    json!({
        "id": id, "group": group, "name": name, "hotkey": hotkey,
        "kind": kind, "body": body, "js": null,
        "rev": { "wall_ms": 1, "dev": "test-dev", "ctr": 0 }
    })
}

fn loaded_vm() -> PermanentsVm {
    let mut vm = PermanentsVm::new();
    assert!(vm.apply_groups(&json!({ "groups": [
        { "name": "Home", "system": false, "count": 1 },
        { "name": "Work", "system": false, "count": 2 }
    ] })));
    assert!(vm.apply_list(&json!({ "permanents": [
        perm_json(ID_A, "Home", "Address", None, "Paste", "home address"),
        perm_json(ID_B, "Work", "Sig", Some("ctrl+alt+1"), "Paste", "Regards, A"),
        perm_json(ID_C, "Work", "Ticket", None, "Paste", "sig ref in body")
    ] })));
    vm
}

fn draft(group: &str, name: &str, hotkey: &str, body: &str) -> PermanentDraft {
    PermanentDraft {
        id: None,
        group: group.to_string(),
        name: name.to_string(),
        hotkey: hotkey.to_string(),
        body: body.to_string(),
    }
}

// U1 VM list/groups: order preserved, malformed body rejected untouched.
#[test]
fn u1_vm_list_and_groups() {
    let mut vm = loaded_vm();
    assert_eq!(
        vm.group_names(),
        vec!["Home".to_string(), "Work".to_string()]
    );
    assert_eq!(vm.permanents.len(), 3);
    let before = vm.permanents.clone();
    assert!(
        !vm.apply_list(&json!({ "permanents": 42 })),
        "malformed list must be refused"
    );
    assert_eq!(
        vm.permanents, before,
        "refused reply leaves state unchanged"
    );
    assert!(!vm.apply_groups(&json!({ "groups": "Work" })));
    assert_eq!(
        vm.group_names(),
        vec!["Home".to_string(), "Work".to_string()]
    );
}

// U2 bridge wire shapes: permanent.* uses {"id"}, never {"clip"}.
#[test]
fn u2_bridge_wire_shapes() {
    let id = Uuid::parse_str(ID_B).unwrap();
    assert_eq!(
        ipc_bridge::method_for(&UiCmd::ExecPermanent(id)),
        "permanent.exec"
    );
    let params = ipc_bridge::params_for(&UiCmd::ExecPermanent(id));
    assert_eq!(params, json!({ "id": id.to_string() }));
    assert!(
        params.get("clip").is_none(),
        "exec must not send clip: {params}"
    );
    // The typed exec params agree with the bridge shape.
    assert_eq!(
        serde_json::to_value(PermanentExecParams { id }).unwrap(),
        json!({ "id": id.to_string() })
    );

    assert_eq!(
        ipc_bridge::method_for(&UiCmd::PermDelete(id)),
        "permanent.delete"
    );
    assert_eq!(
        ipc_bridge::params_for(&UiCmd::PermDelete(id)),
        json!({ "id": id.to_string() })
    );

    let create = PermanentCreateParams {
        group: "Work".to_string(),
        name: "Sig".to_string(),
        hotkey: None,
        body: "Regards, A".to_string(),
    };
    assert_eq!(
        ipc_bridge::method_for(&UiCmd::PermCreate(create.clone())),
        "permanent.create"
    );
    let params = ipc_bridge::params_for(&UiCmd::PermCreate(create));
    assert_eq!(
        params,
        json!({ "group": "Work", "name": "Sig", "body": "Regards, A" })
    );
    assert!(
        params.get("hotkey").is_none(),
        "hotkey:None omitted on the wire: {params}"
    );

    let clip: PermanentClip =
        serde_json::from_value(perm_json(ID_B, "Work", "Sig", None, "Paste", "x")).unwrap();
    assert_eq!(
        ipc_bridge::method_for(&UiCmd::PermUpdate(clip.clone())),
        "permanent.update"
    );
    assert_eq!(
        ipc_bridge::params_for(&UiCmd::PermUpdate(clip)),
        json!({ "permanent": perm_json(ID_B, "Work", "Sig", None, "Paste", "x") })
    );

    assert_eq!(
        ipc_bridge::method_for(&UiCmd::ListPermanents),
        "permanent.list"
    );
    assert_eq!(ipc_bridge::params_for(&UiCmd::ListPermanents), json!({}));
    assert_eq!(
        ipc_bridge::method_for(&UiCmd::PermGroups),
        "permanent.groups"
    );
    assert_eq!(ipc_bridge::params_for(&UiCmd::PermGroups), json!({}));
    assert_eq!(
        ipc_bridge::method_for(&UiCmd::PermGroupCreate("Work".into())),
        "permanent.group_create"
    );
    assert_eq!(
        ipc_bridge::params_for(&UiCmd::PermGroupCreate("Work".into())),
        json!({ "name": "Work" })
    );
    assert_eq!(
        ipc_bridge::method_for(&UiCmd::PermGroupDelete("Work".into())),
        "permanent.group_delete"
    );
    assert_eq!(
        ipc_bridge::params_for(&UiCmd::PermGroupDelete("Work".into())),
        json!({ "name": "Work" })
    );
}

/// Fake transport with one canned outcome; records every (method, params).
struct Canned {
    seen: Arc<Mutex<Vec<(String, Value)>>>,
    outcome: Result<Value, IpcError>,
}
impl ipc_bridge::Transport for Canned {
    fn call(&mut self, method: &str, params: &Value) -> Result<Value, IpcError> {
        self.seen
            .lock()
            .unwrap()
            .push((method.to_string(), params.clone()));
        self.outcome.clone()
    }
}

#[allow(clippy::type_complexity)]
fn spawn_canned(
    outcome: Result<Value, IpcError>,
) -> (
    std::sync::mpsc::Sender<UiCmd>,
    std::sync::mpsc::Receiver<UiEvent>,
    Arc<Mutex<Vec<(String, Value)>>>,
) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Canned {
        seen: seen.clone(),
        outcome,
    }));
    (tx, rx, seen)
}

fn recv(rx: &std::sync::mpsc::Receiver<UiEvent>) -> UiEvent {
    rx.recv_timeout(Duration::from_secs(2))
        .expect("bridge event")
}

// U3 confirmed-delete discipline: row drops only on {"deleted":true}.
#[test]
fn u3_confirmed_delete_discipline() {
    let id = Uuid::parse_str(ID_B).unwrap();
    let (tx, rx, _seen) = spawn_canned(Ok(json!({ "deleted": false })));
    tx.send(UiCmd::PermDelete(id)).expect("send delete");
    match recv(&rx) {
        UiEvent::Error(msg) => assert_ne!(msg.len(), 0),
        other => panic!("unconfirmed delete must be Error, got {other:?}"),
    }
    let mut vm = loaded_vm();
    assert_eq!(
        vm.permanents.len(),
        3,
        "unconfirmed delete keeps the row in the VM"
    );

    let (tx, rx, seen) = spawn_canned(Ok(json!({ "deleted": true })));
    tx.send(UiCmd::PermDelete(id)).expect("send delete");
    match recv(&rx) {
        UiEvent::PermanentDeleted(got) => assert_eq!(got, id),
        other => panic!("confirmed delete must be PermanentDeleted, got {other:?}"),
    }
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec![(
            "permanent.delete".to_string(),
            json!({ "id": id.to_string() })
        )]
    );
    vm.remove_confirmed(id);
    assert_eq!(vm.permanents.len(), 2);
    assert!(vm.permanents.iter().all(|c| c.id != id));
}

// U4 exec result mapping (copies PasteClip arms).
#[test]
fn u4_exec_result_mapping() {
    let id = Uuid::parse_str(ID_B).unwrap();
    for code in ["unavailable", "denied"] {
        let (tx, rx, _s) = spawn_canned(Err(IpcError::new(code, "no paste".to_string())));
        tx.send(UiCmd::ExecPermanent(id)).expect("send exec");
        assert!(
            matches!(recv(&rx), UiEvent::PasteUnavailable),
            "code {code} must map to PasteUnavailable"
        );
    }
    let (tx, rx, _s) = spawn_canned(Err(IpcError::new("backend", "boom".to_string())));
    tx.send(UiCmd::ExecPermanent(id)).expect("send exec");
    assert!(
        matches!(recv(&rx), UiEvent::Error(_)),
        "backend error must map to Error"
    );
    let (tx, rx, seen) = spawn_canned(Ok(json!({ "ok": true })));
    tx.send(UiCmd::ExecPermanent(id)).expect("send exec");
    match recv(&rx) {
        UiEvent::PasteDone(got) => assert_eq!(got, id),
        other => panic!("exec Ok must map to PasteDone, got {other:?}"),
    }
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec![(
            "permanent.exec".to_string(),
            json!({ "id": id.to_string() })
        )]
    );
}

// U5 tabs/keys: group switching, GroupTab effect, clamping, exec selection.
#[test]
fn u5_tabs_and_keys() {
    let mut vm = loaded_vm();
    vm.select_group(0);
    assert_eq!(vm.visible("").len(), 1, "Home has one permanent");
    vm.select_group(1);
    assert_eq!(vm.visible("").len(), 2, "Work has two permanents");
    assert_eq!(vm.selected, 0, "switching groups resets selection");
    vm.select_group(9);
    assert_eq!(
        vm.active_group, 1,
        "out-of-range group clamps to last, no panic"
    );
    // Enter-on-permanent hook: selected Work row resolves to its id.
    vm.select_group(1);
    assert_eq!(vm.activate_selected(), Some(Uuid::parse_str(ID_B).unwrap()));
    let _exec_effect = VmEffect::ExecPermanent(Uuid::parse_str(ID_B).unwrap());

    // Popup Alt+2 (existing keymap binding) must surface as GroupTab(2).
    let mut popup = PopupVm::new();
    let effect = popup.on_key(keymap::KeyEvent::new("2".to_string(), false, true, false));
    assert_eq!(effect, VmEffect::GroupTab(2));
    // History Enter behaviour is untouched.
    let mut popup = PopupVm::new();
    let clip: clipmonger_core::model::Clip = serde_json::from_value(json!({
        "id": ID_A, "created_ms": 1, "source_os": "linux",
        "rev": { "wall_ms": 1, "dev": "test-dev" }
    }))
    .unwrap();
    popup.set_clips(vec![clip]);
    let effect = popup.on_key(keymap::KeyEvent::new(
        "Enter".to_string(),
        false,
        false,
        false,
    ));
    assert_eq!(effect, VmEffect::Paste(Uuid::parse_str(ID_A).unwrap()));
}

// U6 popup filtering: active source only; _System/System never appear.
#[test]
fn u6_popup_filtering_and_system_hidden() {
    let mut vm = PermanentsVm::new();
    assert!(vm.apply_groups(&json!({ "groups": [
        { "name": "_System", "system": true, "count": 1 },
        { "name": "Work", "system": false, "count": 2 }
    ] })));
    assert!(vm.apply_list(&json!({ "permanents": [
        perm_json(ID_SYS, "_System", "SysThing", None, "System", "sys body"),
        perm_json(ID_B, "Work", "Signature", None, "Paste", "Regards"),
        perm_json(ID_C, "Work", "Ticket", None, "Paste", "a sig in the body")
    ] })));
    assert_eq!(
        vm.group_names(),
        vec!["Work".to_string()],
        "_System is never a tab"
    );
    vm.select_group(0);
    let hits: Vec<&str> = vm
        .visible("sig")
        .iter()
        .map(|&i| vm.permanents[i].name.as_str())
        .collect();
    assert_eq!(
        hits,
        vec!["Signature", "Ticket"],
        "query matches name and body, case-insensitive"
    );
    assert!(
        vm.visible("sys").is_empty(),
        "System rows never appear in visible()"
    );
    assert!(vm
        .visible("")
        .iter()
        .all(|&i| vm.permanents[i].id != Uuid::parse_str(ID_SYS).unwrap()));
}

// U7 draft validation: string-id errors, chord normalisation, hotkey conflicts.
#[test]
fn u7_draft_validation() {
    let mut vm = loaded_vm();
    vm.draft = Some(draft("Work", "", "", "body"));
    assert_eq!(vm.validate_draft(), Err("perm.invalid_name"));
    vm.draft = Some(draft("", "Name", "", "body"));
    assert_eq!(vm.validate_draft(), Err("perm.invalid_group"));
    vm.draft = Some(draft("Work", "Name", "", ""));
    assert_eq!(vm.validate_draft(), Err("perm.invalid_body"));
    vm.draft = Some(draft("Work", "Name", "ctrl+", "body"));
    assert_eq!(vm.validate_draft(), Err("config.invalid_chord"));

    vm.draft = Some(draft("Work", "Name", "Ctrl+Alt+1", "body"));
    // ctrl+alt+1 is Sig's hotkey in the loaded list -> conflict for a new clip.
    assert_eq!(vm.validate_draft(), Err("perm.hotkey_conflict"));

    vm.draft = Some(draft("Work", "Name", "Ctrl+Alt+9", "body"));
    let params = vm.validate_draft().expect("valid draft");
    let expected = chord::format_chord(&chord::parse_chord("Ctrl+Alt+9").expect("chord"));
    assert_eq!(
        params.hotkey.as_deref(),
        Some(expected.as_str()),
        "hotkey normalised via format_chord"
    );
    assert_eq!(params.group, "Work");
    assert_eq!(params.name, "Name");
    assert_eq!(params.body, "body");

    // Same hotkey on the clip being edited is not a conflict.
    vm.begin_edit(Uuid::parse_str(ID_B).unwrap());
    let editing = vm.draft.clone().expect("begin_edit must set a draft");
    assert_eq!(editing.id, Some(Uuid::parse_str(ID_B).unwrap()));
    assert!(
        vm.validate_draft().is_ok(),
        "own hotkey must pass: {:?}",
        vm.validate_draft()
    );
    vm.begin_new();
    assert_eq!(vm.draft.as_ref().map(|d| d.id), Some(None));
}

// U8 empty state: zero permanents behaves exactly as pre-permanents.
#[test]
fn u8_empty_state() {
    let vm = PermanentsVm::new();
    assert_eq!(vm.group_names().len(), 0);
    assert_eq!(vm.visible("anything").len(), 0);
    assert_eq!(vm.activate_selected(), None);
    let popup = PopupVm::new();
    assert_eq!(popup.visible().len(), 0);
    assert_eq!(popup.clips().len(), 0);
}

// U9 strings: every perm.* id in §3.6 exists in ENTRIES (via text) and IDS.
#[test]
fn u9_perm_strings_exist() {
    const PERM_IDS: &[&str] = &[
        "perm.name",
        "perm.group",
        "perm.hotkey",
        "perm.body",
        "perm.new_clip",
        "perm.new_group",
        "perm.delete_clip",
        "perm.delete_group",
        "perm.delete_confirm",
        "perm.group_not_empty",
        "perm.save",
        "perm.cancel_edit",
        "perm.invalid_name",
        "perm.invalid_group",
        "perm.invalid_body",
        "perm.hotkey_conflict",
        "perm.hotkey_note",
        "perm.immediate_note",
        "perm.empty_group",
        "perm.tab_history",
        "perm.exec_failed",
    ];
    for id in PERM_IDS {
        assert!(strings::IDS.contains(id), "{id} must be in strings::IDS");
        assert_ne!(
            strings::text(id),
            *id,
            "{id} must have real text in ENTRIES"
        );
        assert!(!strings::text(id).is_empty(), "{id} text must be non-empty");
    }
}

// U10 config tab model: Permanent Clips tab excludes the permanents.groups
// ROWS entry while the global 26-row matrix is untouched (P-F1).
#[test]
fn u10_config_tab_excludes_legacy_groups_row() {
    assert_eq!(config_model::ROWS.len(), 26, "T-909 matrix stays 26 rows");
    assert!(
        config_model::ROWS
            .iter()
            .any(|r| r.key == "permanents.groups"),
        "permanents.groups ROWS entry is kept globally"
    );
    let tab_rows = config_model::visible_rows(Tab::PermanentClips);
    let keys: Vec<&str> = tab_rows
        .iter()
        .map(|&i| config_model::ROWS[i].key)
        .collect();
    assert!(
        !keys.contains(&"permanents.groups"),
        "Permanent Clips tab must not render the legacy groups row: {keys:?}"
    );
    // Other tabs keep their rows; every ROWS entry belongs to exactly one tab.
    let mut total = 0;
    for tab in [
        Tab::History,
        Tab::Popup,
        Tab::Advanced,
        Tab::PermanentClips,
        Tab::Privacy,
        Tab::Tray,
        Tab::Startup,
        Tab::General,
    ] {
        total += config_model::visible_rows(tab).len();
    }
    assert_eq!(total, 26, "visible_rows partitions the 26-row matrix");
}

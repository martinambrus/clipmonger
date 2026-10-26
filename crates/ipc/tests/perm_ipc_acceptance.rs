//! PERM IPC acceptance rulers I1-I2 (permanents v1, perm.md §2.2/§5).
//!
//! Failing baseline: names the typed `permanent.*` structs the builder must
//! add to `clipmonger_ipc::proto::methods` (all `Clone, Debug, PartialEq,
//! Eq, Serialize, Deserialize`):
//!   `PermanentListParams` { group: Option<String> }
//!   `PermanentListResult` { permanents: Vec<PermanentClip> }
//!   `PermanentCreateParams` { group, name, hotkey: Option<String>, body }
//!   `PermanentCreateResult` { permanent: `PermanentClip` }
//!   `PermanentUpdateParams` { permanent: `PermanentClip` }
//!   `PermanentUpdateResult` { updated: bool }
//!   `PermanentDeleteParams` { id: Uuid } / `PermanentDeleteResult` { deleted: bool }
//!   `PermanentGroupsParams` {} / `PermanentGroupInfo` { name, system, count }
//!   `PermanentGroupsResult` { groups: Vec<PermanentGroupInfo> }
//!   `PermanentGroupCreateParams` { name } / `PermanentGroupCreateResult` { created }
//!   `PermanentGroupDeleteParams` { name } / `PermanentGroupDeleteResult` { deleted }
//! Wire keys are `id`, never `clip`, for permanents (perm.md §2.2 trap).

use clipmonger_core::model::{PermanentClip, PermanentKind, Rev};
use clipmonger_ipc::proto::methods::*;
use serde::de::DeserializeOwned;
use uuid::Uuid;

fn roundtrip<T>(v: &T)
where
    T: serde::Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let wire = serde_json::to_value(v).expect("must serialize");
    let back: T = serde_json::from_value(wire).expect("must deserialize");
    assert_eq!(v, &back, "roundtrip must preserve value");
}

fn sample_clip() -> PermanentClip {
    PermanentClip {
        id: Uuid::now_v7(),
        group: "Work".to_string(),
        name: "Sig".to_string(),
        hotkey: Some("ctrl+alt+1".to_string()),
        kind: PermanentKind::Paste,
        body: "Regards, A".to_string(),
        js: None,
        rev: Rev {
            wall_ms: 1_700_000_000_000,
            dev: "harness-dev".to_string(),
            ctr: 7,
        },
    }
}

// I1 roundtrips for every new permanent.* struct, with exact wire shapes.
#[test]
#[allow(clippy::too_many_lines)]
fn i1_permanent_struct_roundtrips_and_wire_shapes() {
    roundtrip(&PermanentListParams {
        group: Some("Work".to_string()),
    });
    roundtrip(&PermanentListParams { group: None });
    // Absent filter is omitted on the wire, never null.
    assert_eq!(
        serde_json::to_value(PermanentListParams { group: None }).unwrap(),
        serde_json::json!({})
    );
    assert_eq!(
        serde_json::to_value(PermanentListParams {
            group: Some("Work".to_string())
        })
        .unwrap(),
        serde_json::json!({ "group": "Work" })
    );

    let clip = sample_clip();
    let list = PermanentListResult {
        permanents: vec![clip.clone()],
    };
    roundtrip(&list);
    let wire = serde_json::to_value(&list).unwrap();
    assert_eq!(wire["permanents"][0]["kind"], serde_json::json!("Paste"));
    assert_eq!(wire["permanents"][0]["js"], serde_json::Value::Null);
    assert_eq!(
        wire["permanents"][0]["hotkey"],
        serde_json::json!("ctrl+alt+1")
    );
    assert_eq!(wire["permanents"][0]["group"], serde_json::json!("Work"));

    roundtrip(&PermanentCreateParams {
        group: "Work".to_string(),
        name: "Sig".to_string(),
        hotkey: Some("ctrl+alt+1".to_string()),
        body: "Regards, A".to_string(),
    });
    let no_key = PermanentCreateParams {
        group: "Work".to_string(),
        name: "Sig".to_string(),
        hotkey: None,
        body: "Regards, A".to_string(),
    };
    roundtrip(&no_key);
    let wire = serde_json::to_value(&no_key).unwrap();
    assert!(
        wire.get("hotkey").is_none(),
        "hotkey:None must be omitted, got {wire}"
    );
    assert_eq!(
        wire,
        serde_json::json!({ "group": "Work", "name": "Sig", "body": "Regards, A" })
    );

    roundtrip(&PermanentCreateResult {
        permanent: clip.clone(),
    });
    roundtrip(&PermanentUpdateParams {
        permanent: clip.clone(),
    });
    assert_eq!(
        serde_json::to_value(PermanentUpdateParams {
            permanent: clip.clone()
        })
        .unwrap()["permanent"]["name"],
        serde_json::json!("Sig")
    );
    roundtrip(&PermanentUpdateResult { updated: true });
    roundtrip(&PermanentUpdateResult { updated: false });
    assert_eq!(
        serde_json::to_value(PermanentUpdateResult { updated: true }).unwrap(),
        serde_json::json!({ "updated": true })
    );

    let id = Uuid::now_v7();
    roundtrip(&PermanentDeleteParams { id });
    // Delete/exec carry `id`, never the history family's `clip` key.
    let wire = serde_json::to_value(PermanentDeleteParams { id }).unwrap();
    assert_eq!(wire, serde_json::json!({ "id": id.to_string() }));
    assert!(
        wire.get("clip").is_none(),
        "permanents must not use clip key: {wire}"
    );
    roundtrip(&PermanentDeleteResult { deleted: true });
    assert_eq!(
        serde_json::to_value(PermanentDeleteResult { deleted: false }).unwrap(),
        serde_json::json!({ "deleted": false })
    );

    roundtrip(&PermanentGroupsParams {});
    assert_eq!(
        serde_json::to_value(PermanentGroupsParams {}).unwrap(),
        serde_json::json!({})
    );
    let groups = PermanentGroupsResult {
        groups: vec![PermanentGroupInfo {
            name: "Work".to_string(),
            system: false,
            count: 3,
        }],
    };
    roundtrip(&groups);
    assert_eq!(
        serde_json::to_value(&groups).unwrap(),
        serde_json::json!({ "groups": [{ "name": "Work", "system": false, "count": 3 }] })
    );
    roundtrip(&PermanentGroupsResult { groups: Vec::new() });

    roundtrip(&PermanentGroupCreateParams {
        name: "Work".to_string(),
    });
    roundtrip(&PermanentGroupCreateResult { created: true });
    roundtrip(&PermanentGroupDeleteParams {
        name: "Work".to_string(),
    });
    roundtrip(&PermanentGroupDeleteResult { deleted: false });
}

// I2 frozen exec shape: PermanentExecParams{id} / PermanentExecResult{ok}
// are NOT modified by permanents v1 (already pinned; keep passing unchanged).
#[test]
fn i2_frozen_exec_shape_unchanged() {
    let id = Uuid::now_v7();
    roundtrip(&PermanentExecParams { id });
    roundtrip(&PermanentExecResult { ok: true });
    let wire = serde_json::to_value(PermanentExecParams { id }).unwrap();
    assert_eq!(
        wire,
        serde_json::json!({ "id": id.to_string() }),
        "exec is {{id}}, not {{clip}}"
    );
    assert!(wire.get("clip").is_none());
    assert_eq!(
        serde_json::to_value(PermanentExecResult { ok: true }).unwrap(),
        serde_json::json!({ "ok": true })
    );
    // A caller-supplied body/kind on exec is not part of the frozen shape:
    // deserializing the canonical shape must be the only contract.
    let parsed: PermanentExecParams =
        serde_json::from_value(serde_json::json!({ "id": id.to_string() })).expect("exec params");
    assert_eq!(parsed.id, id);
}

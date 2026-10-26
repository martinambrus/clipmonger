//! PERM core acceptance rulers C1-C13 (permanents v1, perm.md §1/§5).
//!
//! Failing baseline: names the frozen `Store` permanents API the builder
//! must land in `crates/core/src/store.rs`:
//!   `PermGroup { name: String, system: bool, count: u64 }` (in `store`)
//!   `create_permanent(&self, group, name, hotkey: Option<&str>, body)
//!       -> Result<PermanentClip, CoreError>` (store mints v7 id, forces
//!       kind=Paste, js=None, rev.ctr=0)
//!   `get_permanent(&self, id) -> Result<Option<PermanentClip>, CoreError>`
//!   `list_permanents(&self, group: Option<&str>)
//!       -> Result<Vec<PermanentClip>, CoreError>` (ORDER BY grp,name,id)
//!   `update_permanent(&self, clip: &PermanentClip) -> Result<bool, CoreError>`
//!   `delete_permanent(&self, id) -> Result<bool, CoreError>` (hard delete)
//!   `list_perm_groups(&self) -> Result<Vec<PermGroup>, CoreError>`
//!   `create_perm_group(&self, name) -> Result<bool, CoreError>`
//!   `delete_perm_group(&self, name) -> Result<bool, CoreError>`
//! Freeze: P-F2 alphabetical, P-F3 Macro/System rejected at update with
//! `config`, P-F6 hard delete, schema frozen (`user_version` stays 1).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use clipmonger_core::model::{Clip, PermanentClip, PermanentKind, Rev};
use clipmonger_core::policy::Limits;
use clipmonger_core::store::{PermGroup, Store};
use uuid::Uuid;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("cm-perm-core-{}-{}-{}", tag, std::process::id(), n));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open(tag: &str) -> (PathBuf, Store) {
    let dir = unique_dir(tag);
    let store = Store::open(&dir).expect("open store");
    (dir, store)
}

fn sample_clip(id: Uuid, text: &str, last_used_ms: i64) -> Clip {
    Clip {
        id,
        created_ms: last_used_ms,
        source_app: Some("test-app".to_string()),
        source_os: "linux".to_string(),
        pinned: false,
        removed: false,
        preview: text.to_string(),
        search_text: text.to_string(),
        formats: vec!["text/plain;charset=utf-8".to_string()],
        bytes_total: text.len() as u64,
        content_hash: format!("hash-{}", id.as_simple()),
        rev: Rev {
            wall_ms: last_used_ms,
            dev: "dev-test".to_string(),
            ctr: 0,
        },
        last_used_ms,
        origin_dev: "dev-test".to_string(),
        origin: "x11".to_string(),
    }
}

fn group_tuples(store: &Store) -> Vec<(String, bool, u64)> {
    store
        .list_perm_groups()
        .expect("list_perm_groups")
        .iter()
        .map(|g: &PermGroup| (g.name.clone(), g.system, g.count))
        .collect()
}

fn names(clips: &[PermanentClip]) -> Vec<&str> {
    clips.iter().map(|c| c.name.as_str()).collect()
}

// C1 create/get roundtrip.
#[test]
fn c1_create_get_roundtrip() {
    let (_dir, store) = open("c1");
    let created = store
        .create_permanent("Work", "Sig", Some("ctrl+alt+1"), "Regards, A")
        .expect("create_permanent");
    assert!(!created.id.is_nil(), "store must mint an id");
    assert_eq!(created.id.get_version_num(), 7, "id must be uuid v7");
    assert_eq!(created.group, "Work");
    assert_eq!(created.name, "Sig");
    assert_eq!(created.hotkey.as_deref(), Some("ctrl+alt+1"));
    assert_eq!(created.kind, PermanentKind::Paste, "v1 create forces Paste");
    assert_eq!(created.js, None, "v1 create forces js=None");
    assert_eq!(created.body, "Regards, A");
    assert_eq!(created.rev.ctr, 0);
    let got = store
        .get_permanent(created.id)
        .expect("get_permanent")
        .expect("created permanent must be readable");
    assert_eq!(got, created, "get must equal create field-for-field");
    assert_eq!(
        store.get_permanent(Uuid::now_v7()).expect("get missing"),
        None
    );
}

// C2 implicit group creation in the same transaction.
#[test]
fn c2_implicit_group() {
    let (_dir, store) = open("c2");
    store
        .create_permanent("Work", "Sig", Some("ctrl+alt+1"), "Regards, A")
        .expect("create");
    assert_eq!(group_tuples(&store), vec![("Work".to_string(), false, 1)]);
}

// C3 list filter + ordering (grp ASC, name ASC, id ASC).
#[test]
fn c3_list_filter_and_ordering() {
    let (_dir, store) = open("c3");
    store
        .create_permanent("Work", "beta", None, "b")
        .expect("c");
    store
        .create_permanent("Home", "alpha", None, "a")
        .expect("c");
    store
        .create_permanent("Work", "alpha", None, "a")
        .expect("c");
    let all = store.list_permanents(None).expect("list all");
    let keys: Vec<(&str, &str)> = all
        .iter()
        .map(|c| (c.group.as_str(), c.name.as_str()))
        .collect();
    assert_eq!(
        keys,
        vec![("Home", "alpha"), ("Work", "alpha"), ("Work", "beta")]
    );
    let work = store.list_permanents(Some("Work")).expect("list Work");
    assert_eq!(names(&work), vec!["alpha", "beta"]);
    assert_eq!(
        store.list_permanents(Some("Nope")).expect("list Nope"),
        [] as [clipmonger_core::model::PermanentClip; 0]
    );
}

// C4 update bumps rev, moves group; missing id is false, never an upsert.
#[test]
fn c4_update_bumps_rev_and_moves_group() {
    let (_dir, store) = open("c4");
    let created = store
        .create_permanent("Work", "Sig", Some("ctrl+alt+1"), "Regards, A")
        .expect("create");
    let mut edited = created.clone();
    edited.name = "Signature".to_string();
    edited.group = "Home".to_string();
    edited.hotkey = None;
    assert!(store.update_permanent(&edited).expect("update"));
    let stored = store.get_permanent(created.id).expect("get").expect("row");
    assert_eq!(stored.name, "Signature");
    assert_eq!(stored.group, "Home");
    assert_eq!(stored.hotkey, None);
    assert_eq!(stored.rev.ctr, 1, "update must bump rev.ctr");
    assert_eq!(stored.rev.dev, created.rev.dev, "update preserves rev.dev");
    assert!(stored.rev.wall_ms >= created.rev.wall_ms);
    assert_eq!(
        group_tuples(&store),
        vec![
            ("Home".to_string(), false, 1),
            ("Work".to_string(), false, 0)
        ],
        "empty group row survives a move"
    );
    let ghost = PermanentClip {
        id: Uuid::now_v7(),
        ..edited
    };
    assert!(!store.update_permanent(&ghost).expect("update ghost"));
    assert_eq!(store.get_permanent(ghost.id).expect("get ghost"), None);
}

// C5 hard delete.
#[test]
fn c5_delete() {
    let (_dir, store) = open("c5");
    let created = store.create_permanent("Work", "Sig", None, "x").expect("c");
    assert!(store.delete_permanent(created.id).expect("delete"));
    assert_eq!(store.get_permanent(created.id).expect("get"), None);
    assert!(!store.delete_permanent(created.id).expect("delete again"));
    assert!(!store
        .delete_permanent(Uuid::now_v7())
        .expect("delete random"));
}

// C6 retention exemption (regression pin): permanents never purge, never count.
#[test]
fn c6_retention_exemption() {
    let (_dir, store) = open("c6");
    for i in 0..3 {
        let clip = sample_clip(Uuid::now_v7(), &format!("h{i}"), 1_700_000_000_000 + i);
        store
            .insert_clip(
                &clip,
                &[(
                    "text/plain;charset=utf-8".to_string(),
                    format!("h{i}").into_bytes(),
                )],
            )
            .expect("insert history");
    }
    let p1 = store
        .create_permanent("Work", "One", None, "one")
        .expect("p1");
    let p2 = store
        .create_permanent("Work", "Two", None, "two")
        .expect("p2");
    let limits = Limits {
        max_items: 1,
        max_store_bytes: u64::MAX,
        max_age_days: 0,
    };
    let _ = store.enforce_retention(&limits).expect("enforce_retention");
    let remaining = store.list_permanents(None).expect("list permanents");
    assert_eq!(remaining.len(), 2, "permanents survive retention purge");
    assert!(remaining.iter().any(|c| c.id == p1.id));
    assert!(remaining.iter().any(|c| c.id == p2.id));
    // Permanents do not count toward the cap: 1 clip under max_items=1 survives.
    let history = store.list_history(100, 0, false).expect("history");
    assert_eq!(
        history.len(),
        1,
        "permanents must not consume the history cap"
    );
}

// C7 validation: empty name/group/body -> config, zero rows; body verbatim;
// Macro/System kind via update rejected with config (P-F3).
#[test]
fn c7_validation() {
    let (_dir, store) = open("c7");
    for (group, name, body) in [
        ("", "n", "b"),
        ("  ", "n", "b"),
        ("g", "", "b"),
        ("g", "  ", "b"),
        ("g", "n", ""),
    ] {
        let err = store
            .create_permanent(group, name, None, body)
            .expect_err("invalid create must fail");
        assert_eq!(err.code(), "config", "got {err:?}");
    }
    assert!(
        store.list_permanents(None).expect("list").is_empty(),
        "failed creates must write zero rows"
    );
    let padded = store
        .create_permanent("Work", "Pad", None, "  padded  ")
        .expect("padded body is valid");
    assert_eq!(
        padded.body, "  padded  ",
        "body stored verbatim, never trimmed"
    );
    assert_eq!(
        store
            .get_permanent(padded.id)
            .expect("get")
            .expect("row")
            .body,
        "  padded  "
    );
    for kind in [PermanentKind::Macro, PermanentKind::System] {
        let mut smuggled = padded.clone();
        smuggled.kind = kind;
        let err = store
            .update_permanent(&smuggled)
            .expect_err("non-Paste kind must be rejected in v1");
        assert_eq!(err.code(), "config", "kind {kind:?}: got {err:?}");
    }
    assert_eq!(
        store
            .get_permanent(padded.id)
            .expect("get")
            .expect("row")
            .kind,
        PermanentKind::Paste,
        "rejected update must not change the stored row"
    );
}

// C8 duplicate hotkey: rejected across clips, allowed on self / when None.
#[test]
fn c8_duplicate_hotkey() {
    let (_dir, store) = open("c8");
    let first = store
        .create_permanent("Work", "One", Some("ctrl+alt+1"), "one")
        .expect("first");
    let err = store
        .create_permanent("Work", "Two", Some("ctrl+alt+1"), "two")
        .expect_err("duplicate hotkey must fail");
    assert_eq!(err.code(), "config", "got {err:?}");
    assert!(store
        .update_permanent(&first)
        .expect("self update with same hotkey"));
    store
        .create_permanent("Work", "NoKeyA", None, "a")
        .expect("none a");
    store
        .create_permanent("Work", "NoKeyB", None, "b")
        .expect("none b");
}

// C9 groups CRUD + authoritative list.
#[test]
fn c9_groups_crud() {
    let (_dir, store) = open("c9");
    store
        .create_permanent("Work", "Sig", None, "x")
        .expect("seed Work");
    assert!(store.create_perm_group("Empty").expect("create group"));
    assert!(!store
        .create_perm_group("Empty")
        .expect("create group again"));
    assert_eq!(
        group_tuples(&store),
        vec![
            ("Empty".to_string(), false, 0),
            ("Work".to_string(), false, 1)
        ]
    );
    assert!(store.delete_perm_group("Empty").expect("delete empty"));
    assert!(
        !store.delete_perm_group("Work").expect("delete non-empty"),
        "non-empty group delete must be refused"
    );
    assert_eq!(
        store.list_permanents(None).expect("list").len(),
        1,
        "clips intact"
    );
    assert!(!store.delete_perm_group("Missing").expect("delete missing"));
    let err = store.create_perm_group("  ").expect_err("blank group name");
    assert_eq!(err.code(), "config", "got {err:?}");
}

// C10 read-only recovery handle refuses every permanents write.
#[test]
fn c10_readonly_refusal() {
    let (dir, store) = open("c10");
    let created = store
        .create_permanent("Work", "Sig", None, "x")
        .expect("seed");
    drop(store);
    let ro = Store::open_readonly_recovery(&dir).expect("recovery open");
    assert_eq!(ro.list_permanents(None).expect("reads work").len(), 1);
    for result in [
        ro.create_permanent("Work", "New", None, "y").map(|_| ()),
        ro.update_permanent(&created).map(|_| ()),
        ro.delete_permanent(created.id).map(|_| ()),
        ro.create_perm_group("G").map(|_| ()),
        ro.delete_perm_group("Work").map(|_| ()),
    ] {
        let err = result.expect_err("write on recovery handle must fail");
        assert_eq!(err.code(), "unsupported", "got {err:?}");
    }
    assert_eq!(
        ro.get_permanent(created.id).expect("get").expect("row"),
        created
    );
}

// C11 pre-existing DB opens with no migration: user_version stays 1.
#[test]
fn c11_preexisting_db_user_version_stays_1() {
    let (dir, store) = open("c11");
    let clip = sample_clip(Uuid::now_v7(), "hi", 1_700_000_000_000);
    store
        .insert_clip(
            &clip,
            &[("text/plain;charset=utf-8".to_string(), b"hi".to_vec())],
        )
        .expect("insert history");
    drop(store);
    let store2 = Store::open(&dir).expect("reopen pre-existing DB");
    let created = store2
        .create_permanent("Work", "Sig", None, "x")
        .expect("perm flow");
    assert_eq!(
        store2.get_permanent(created.id).expect("get").expect("row"),
        created
    );
    drop(store2);
    let path = dir.join("clipstore.sqlite3");
    let conn = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1, "permanents must not bump user_version");
    let store3 = Store::open(&dir).expect("reopen with permanents rows");
    assert_eq!(store3.list_permanents(None).expect("list").len(), 1);
}

// C12 kind encoding: reader tolerates Macro rows, Bogus is corrupt.
#[test]
fn c12_kind_encoding() {
    let (dir, store) = open("c12");
    drop(store);
    let path = dir.join("clipstore.sqlite3");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("INSERT OR IGNORE INTO perm_groups(name, system) VALUES('Work', 0);")
        .unwrap();
    let macro_id = Uuid::now_v7();
    conn.execute(
        "INSERT INTO permanents(id, grp, name, hotkey, kind, body, js, rev_ms, rev_dev, rev_ctr)
         VALUES(?1, 'Work', 'OldMacro', NULL, 'Macro', 'body', NULL, 1, 'dev', 0)",
        [macro_id.to_string()],
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&dir).expect("reopen");
    let listed = store.list_permanents(None).expect("Macro row must read");
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].kind,
        PermanentKind::Macro,
        "reader tolerates v2 kinds"
    );
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "INSERT INTO permanents(id, grp, name, hotkey, kind, body, js, rev_ms, rev_dev, rev_ctr)
         VALUES(?1, 'Work', 'Bad', NULL, 'Bogus', 'body', NULL, 1, 'dev', 0)",
        [Uuid::now_v7().to_string()],
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&dir).expect("reopen");
    let err = store
        .list_permanents(None)
        .expect_err("unknown kind text must be corrupt, never coerced");
    assert_eq!(err.code(), "corrupt", "got {err:?}");
}

// C13 backup roundtrip carries permanents (whole-DB backup pin).
#[test]
fn c13_backup_roundtrip() {
    let (dir, store) = open("c13");
    let created = store
        .create_permanent("Work", "Sig", Some("ctrl+alt+1"), "Regards, A")
        .expect("create");
    let dest = dir.join("backup.zip");
    store.backup_zip(&dest).expect("backup_zip");
    drop(store);
    let restored_dir = unique_dir("c13-restored");
    Store::restore_zip(&dest, &restored_dir).expect("restore_zip");
    let restored = Store::open(&restored_dir).expect("open restored");
    assert_eq!(
        restored
            .get_permanent(created.id)
            .expect("get")
            .expect("row"),
        created,
        "backup/restore must carry the permanent with the same id"
    );
}

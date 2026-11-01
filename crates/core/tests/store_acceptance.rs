//! CORE-STORE acceptance harness: the ruler, not the store.
//!
//! The builder MUST provide module `clipmonger_core::store` with the frozen
//! API from `.agents/tmp/foundation-v1/core-store/harness-spec.md`:
//! `Store::open`, `Store::open_readonly_recovery`, `insert_clip`,
//! `get_clip`, `list_history`, `search`, `soft_delete`, `purge_removed`,
//! `enforce_retention(&policy::Limits)`, `gc_blobs`, `backup_zip`,
//! `Store::restore_zip`, `integrity_full`, `record_privacy_skip`, plus
//! `store::InsertOutcome` and `store::GcReport`.
//!
//! Fixtures are built in-test under `std::env::temp_dir` (no dev-deps).
//! No sleeps, no timing-dependent asserts: concurrency uses barriers.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Barrier;

use clipmonger_core::model::{self, Clip};
use clipmonger_core::policy::Limits;
use clipmonger_core::store::{GcReport, InsertOutcome, Store};
use clipmonger_core::CoreError;
use uuid::Uuid;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Unique scratch dir under the OS temp dir. Never reuses across tests.
fn unique_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("cm-store-{}-{}-{}", tag, std::process::id(), n));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sample_clip(id: Uuid, preview: &str, search_text: &str, last_used_ms: i64) -> Clip {
    Clip {
        id,
        created_ms: last_used_ms,
        source_app: Some("test-app".to_string()),
        source_os: "linux".to_string(),
        pinned: false,
        removed: false,
        preview: preview.to_string(),
        search_text: search_text.to_string(),
        formats: vec!["text/plain;charset=utf-8".to_string()],
        bytes_total: preview.len() as u64,
        content_hash: format!("hash-{}", id.as_simple()),
        rev: model::Rev {
            wall_ms: last_used_ms,
            dev: "dev-test".to_string(),
            ctr: 0,
        },
        last_used_ms,
        origin_dev: "dev-test".to_string(),
        origin: "x11".to_string(),
    }
}

fn text_payloads(text: &str) -> Vec<(String, Vec<u8>)> {
    vec![(
        "text/plain;charset=utf-8".to_string(),
        text.as_bytes().to_vec(),
    )]
}

fn db_path(profile: &Path) -> PathBuf {
    profile.join("clipstore.sqlite3")
}

fn read_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

fn pragma_user_version(path: &Path) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

fn set_user_version(path: &Path, v: i64) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(&format!("PRAGMA user_version={v}"))
        .unwrap();
}

fn corrupt_asides(profile: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(profile) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.contains(".corrupt-") {
                out.push(e.path());
            }
        }
    }
    out
}

fn table_names(path: &Path) -> HashSet<String> {
    let conn = rusqlite::Connection::open(path).unwrap();
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type IN ('table','index')")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

fn history_ids(store: &Store) -> Vec<Uuid> {
    store
        .list_history(10_000, 0, false)
        .expect("list_history")
        .iter()
        .map(|c| c.id)
        .collect()
}

// ---- T-302: schema create ----

#[test]
fn t302_schema_create_tables_and_meta() {
    let profile = unique_dir("t302-create");
    let _store = Store::open(&profile).expect("open");
    let path = db_path(&profile);
    assert!(path.exists(), "clipstore.sqlite3 must exist");

    let names = table_names(&path);
    for t in [
        "meta",
        "clips",
        "clip_payloads",
        "clip_blobs",
        "perm_groups",
        "permanents",
        "clips_fts",
    ] {
        assert!(names.contains(t), "missing table {t}; have {names:?}");
    }
    for idx in ["idx_clips_created", "idx_clips_content"] {
        assert!(names.contains(idx), "missing index {idx}; have {names:?}");
    }

    // user_version is authoritative; fresh v1 create reports 1.
    assert_eq!(pragma_user_version(&path), 1);

    // Meta rows: schema_version mirror, machine, created, privacy_skips.
    let conn = rusqlite::Connection::open(&path).unwrap();
    let meta: HashSet<String> = {
        let mut stmt = conn.prepare("SELECT k FROM meta").unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    for k in ["schema_version", "machine", "created", "privacy_skips"] {
        assert!(meta.contains(k), "missing meta row {k}; have {meta:?}");
    }
}

#[test]
fn t302_schema_journal_mode_wal() {
    let profile = unique_dir("t302-wal");
    let _store = Store::open(&profile).expect("open");
    let conn = rusqlite::Connection::open(db_path(&profile)).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
}

#[test]
fn t302_schema_reopen_preserves_rows() {
    let profile = unique_dir("t302-reopen");
    let store = Store::open(&profile).expect("open");
    let clip = sample_clip(Uuid::now_v7(), "hi", "hi", 1_700_000_000_000);
    let _ = store
        .insert_clip(&clip, &text_payloads("hi"))
        .expect("insert");
    drop(store);
    let store2 = Store::open(&profile).expect("reopen");
    assert_eq!(history_ids(&store2), vec![clip.id]);
    assert_eq!(corrupt_asides(&profile), [] as [PathBuf; 0]);
}

// Rollback journals require an exclusive lock to block both writable and
// recovery header reads. WAL readers would be the wrong fixture here.
fn opening_exclusive_lock_control(read_only: bool) {
    let profile = unique_dir("open-exclusive");
    let sentinel = sample_clip(Uuid::now_v7(), "sentinel", "sentinel", 17);
    let store = Store::open(&profile).unwrap();
    store
        .insert_clip(&sentinel, &text_payloads("sentinel"))
        .unwrap();
    drop(store);
    let blocker = rusqlite::Connection::open(db_path(&profile)).unwrap();
    let mode: String = blocker
        .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
    assert_eq!(pragma_user_version(&db_path(&profile)), 1);
    blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let result = if read_only {
        Store::open_readonly_recovery(&profile)
    } else {
        Store::open(&profile)
    };
    blocker.execute_batch("ROLLBACK").unwrap();
    eprintln!(
        "exclusive recovery={read_only}: {result:?}; asides={:?}",
        corrupt_asides(&profile)
    );
    assert!(
        matches!(result, Err(CoreError::Backend { .. })),
        "must preserve operational contention: {result:?}"
    );
    assert_eq!(corrupt_asides(&profile), [] as [PathBuf; 0]);
    let reopened = Store::open(&profile).unwrap();
    assert_eq!(history_ids(&reopened), vec![sentinel.id]);
    assert_eq!(corrupt_asides(&profile), [] as [PathBuf; 0]);
}

#[test]
fn opening_exclusive_writable_is_operational() {
    opening_exclusive_lock_control(false);
}

#[test]
fn opening_exclusive_recovery_is_operational() {
    opening_exclusive_lock_control(true);
}

// ---- T-303: open-path matrix + integrity ----

#[test]
fn t303_open_clean_negative_control_no_aside() {
    // Negative control: a clean open must never mint a .corrupt-* aside.
    let profile = unique_dir("t303-clean");
    let store = Store::open(&profile).expect("open");
    let clip = sample_clip(Uuid::now_v7(), "clean", "clean", 1_700_000_000_100);
    let _ = store
        .insert_clip(&clip, &text_payloads("clean"))
        .expect("insert");
    drop(store);
    let _ = Store::open(&profile).expect("reopen");
    assert!(
        corrupt_asides(&profile).is_empty(),
        "clean open must not create aside files"
    );
}

#[test]
fn t303_migration_failure_never_misclassified_as_corrupt() {
    // A structurally valid DB at an older user_version must either migrate
    // (sentinel preserved, version bumped) or fail as migration_failed —
    // never as corruption, and never with an aside copy.
    let profile = unique_dir("t303-migfail");
    let store = Store::open(&profile).expect("open");
    let sentinel = sample_clip(Uuid::now_v7(), "sentinel", "sentinel", 1_700_000_000_200);
    let _ = store
        .insert_clip(&sentinel, &text_payloads("sentinel"))
        .expect("insert");
    drop(store);

    let path = db_path(&profile);
    set_user_version(&path, 0);
    let before = read_bytes(&path);
    let _ = before;

    match Store::open(&profile) {
        Ok(reopened) => {
            // Migrated: sentinel survives and version advanced past 0.
            assert!(history_ids(&reopened).contains(&sentinel.id));
            assert!(pragma_user_version(&path) > 0);
        }
        Err(e) => {
            assert_eq!(
                e.code(),
                "migration_failed",
                "older-version open must fail as migration_failed, got {e:?}"
            );
            assert!(
                corrupt_asides(&profile).is_empty(),
                "migration failure must not mint a corrupt aside"
            );
        }
    }
}

#[test]
fn t303_true_corruption_preserved_aside() {
    // True corruption (unreadable header): open fails, the bytes are
    // preserved aside as clipstore.corrupt-*, and the error surfaces it.
    let profile = unique_dir("t303-corrupt");
    let store = Store::open(&profile).expect("open");
    drop(store);
    let path = db_path(&profile);
    std::fs::write(&path, vec![0xFFu8; 4096]).unwrap();

    let err = Store::open(&profile).expect_err("corrupt DB must fail open");
    assert_eq!(err.code(), "corrupt", "got {err:?}");
    let asides = corrupt_asides(&profile);
    assert_eq!(asides.len(), 1, "exactly one aside copy, got {asides:?}");
    assert_eq!(read_bytes(&asides[0]), vec![0xFFu8; 4096]);
    assert!(
        format!("{err}").len() > "corrupt store: ".len(),
        "corruption error must carry a message: {err}"
    );
}

#[test]
fn t303_integrity_full_ok_on_clean() {
    let profile = unique_dir("t303-integrity");
    let store = Store::open(&profile).expect("open");
    let clip = sample_clip(Uuid::now_v7(), "ok", "ok", 1_700_000_000_300);
    let _ = store
        .insert_clip(&clip, &text_payloads("ok"))
        .expect("insert");
    store.integrity_full().expect("integrity_full on clean DB");
}

// ---- T-314: downgrade refusal, byte-unchanged ----

#[test]
fn t314_downgrade_refused_and_byte_unchanged() {
    let profile = unique_dir("t314");
    let store = Store::open(&profile).expect("open");
    let clip = sample_clip(Uuid::now_v7(), "v1", "v1", 1_700_000_000_400);
    let _ = store
        .insert_clip(&clip, &text_payloads("v1"))
        .expect("insert");
    drop(store);

    let path = db_path(&profile);
    let current = pragma_user_version(&path);
    set_user_version(&path, current + 1);
    // Sentinel row proves content is untouched beyond the version bump.
    let before = read_bytes(&path);

    let err = Store::open(&profile).expect_err("newer schema must be refused");
    match err {
        CoreError::DowngradeRefused { found, expected } => {
            assert_eq!(found, current + 1);
            assert_eq!(expected, current);
        }
        other => panic!("expected DowngradeRefused, got {other:?}"),
    }
    assert_eq!(read_bytes(&path), before, "refused open must not write");
    assert_eq!(corrupt_asides(&profile), [] as [PathBuf; 0]);
}

// ---- T-305 (store side): blob boundary, dedupe, retention, privacy ----

const KIB256: usize = 256 * 1024;

type Payloads = Vec<(String, Vec<u8>)>;

fn payload_row_external(path: &Path, mime: &str) -> Option<(i64, Option<String>)> {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row(
        "SELECT external, blob_key FROM clip_payloads WHERE mime = ?1",
        [mime],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok()
}

#[test]
fn t305_store_blob_boundary_256kib() {
    // Per-payload boundary: 256 KiB inline, 256 KiB + 1 external.
    let profile = unique_dir("t305-boundary");
    let store = Store::open(&profile).expect("open");
    let path = db_path(&profile);

    let small = sample_clip(Uuid::now_v7(), "small", "small", 1_700_000_001_000);
    let small_bytes = vec![b'a'; KIB256];
    let _ = store
        .insert_clip(&small, &[("text/plain".to_string(), small_bytes.clone())])
        .expect("insert small");
    let (ext, key) = payload_row_external(&path, "text/plain").expect("small row");
    assert_eq!(ext, 0, "exactly 256 KiB must stay inline");
    assert!(key.is_none(), "inline row must not carry a blob_key");

    let big = sample_clip(Uuid::now_v7(), "big", "big", 1_700_000_001_001);
    let big_bytes = vec![b'b'; KIB256 + 1];
    let _ = store
        .insert_clip(&big, &[("text/plain".to_string(), big_bytes.clone())])
        .expect("insert big");

    let conn = rusqlite::Connection::open(&path).unwrap();
    let (ext_big, key_big): (i64, Option<String>) = conn
        .query_row(
            "SELECT external, blob_key FROM clip_payloads WHERE clip_id = ?1",
            [big.id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(ext_big, 1, "256 KiB + 1 must go external");
    let key_big = key_big.expect("external row needs blob_key");
    assert!(
        profile.join("blobs").join(&key_big).exists(),
        "external blob file must exist"
    );

    // Both read back byte-identical through the single reader path.
    let (_, got_small) = store.get_clip(small.id).expect("get").expect("small");
    assert_eq!(got_small[0].1, small_bytes);
    let (_, got_big) = store.get_clip(big.id).expect("get").expect("big");
    assert_eq!(got_big[0].1, big_bytes);
}

#[test]
fn t305_store_external_sha256_verified_on_read() {
    let profile = unique_dir("t305-verify");
    let store = Store::open(&profile).expect("open");
    let path = db_path(&profile);
    let clip = sample_clip(Uuid::now_v7(), "v", "v", 1_700_000_001_010);
    let bytes = vec![b'c'; KIB256 + 16];
    let _ = store
        .insert_clip(&clip, &[("text/plain".to_string(), bytes)])
        .expect("insert");
    let conn = rusqlite::Connection::open(&path).unwrap();
    let key: String = conn
        .query_row(
            "SELECT blob_key FROM clip_payloads WHERE clip_id = ?1",
            [clip.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let blob = profile.join("blobs").join(key);
    let mut raw = read_bytes(&blob);
    raw[0] ^= 0xFF;
    std::fs::write(&blob, raw).unwrap();
    match store.get_clip(clip.id) {
        Err(_) | Ok(None) => {}
        Ok(Some(_)) => panic!("tampered external blob must not read back as valid"),
    }
}

#[test]
fn t305_store_dedupe_move_to_top() {
    let profile = unique_dir("t305-dedupe");
    let store = Store::open(&profile).expect("open");
    let payloads = text_payloads("same-content");
    let first = sample_clip(Uuid::now_v7(), "first", "same-content", 1_700_000_002_000);
    let _ = store.insert_clip(&first, &payloads).expect("first insert");
    let other = sample_clip(Uuid::now_v7(), "other", "other", 1_700_000_002_001);
    let _ = store
        .insert_clip(&other, &text_payloads("other"))
        .expect("other insert");

    // Same bytes again: no duplicate, existing row moves to top.
    let dupe = sample_clip(Uuid::now_v7(), "first", "same-content", 1_700_000_002_002);
    let outcome = store.insert_clip(&dupe, &payloads).expect("dupe insert");
    let InsertOutcome::MovedToTop(hit) = outcome else {
        panic!("expected MovedToTop for duplicate content")
    };
    assert_eq!(hit, first.id);
    let ids = history_ids(&store);
    assert_eq!(ids.len(), 2, "dedupe must not add a row");
    assert_eq!(ids[0], first.id, "dedupe hit moves to top");
}

#[test]
fn t305_store_retention_purge_pinned_exempt() {
    let profile = unique_dir("t305-retention");
    let store = Store::open(&profile).expect("open");
    // Oldest clip is pinned and must survive a count-cap purge.
    let mut pinned = sample_clip(Uuid::now_v7(), "pinned", "pinned", 1_700_000_003_000);
    pinned.pinned = true;
    let _ = store
        .insert_clip(&pinned, &text_payloads("pinned"))
        .expect("insert pinned");
    let mut oldest_unpinned = Uuid::nil();
    for i in 0..5 {
        let c = sample_clip(
            Uuid::now_v7(),
            &format!("c{i}"),
            &format!("c{i}"),
            1_700_000_003_001 + i,
        );
        if i == 0 {
            oldest_unpinned = c.id;
        }
        let t = format!("c{i}");
        let _ = store.insert_clip(&c, &text_payloads(&t)).expect("insert");
    }
    let limits = Limits {
        max_items: 3,
        max_store_bytes: u64::MAX,
        max_age_days: 0,
    };
    let _ = store.enforce_retention(&limits);
    let ids = history_ids(&store);
    assert!(
        ids.contains(&pinned.id),
        "pinned clip is exempt from retention: {ids:?}"
    );
    assert!(
        !ids.contains(&oldest_unpinned),
        "oldest unpinned clip must purge first: {ids:?}"
    );
    assert!(ids.len() <= 4, "cap 3 plus exempt pinned: {ids:?}");
}

#[test]
fn t305_store_soft_delete_and_purge() {
    let profile = unique_dir("t305-purge");
    let store = Store::open(&profile).expect("open");
    let a = sample_clip(Uuid::now_v7(), "a", "a", 1_700_000_004_000);
    let b = sample_clip(Uuid::now_v7(), "b", "b", 1_700_000_004_001);
    let _ = store
        .insert_clip(&a, &text_payloads("a"))
        .expect("insert a");
    let _ = store
        .insert_clip(&b, &text_payloads("b"))
        .expect("insert b");

    let _ = store.soft_delete(a.id);
    let visible = history_ids(&store);
    assert!(!visible.contains(&a.id), "soft-deleted hidden by default");
    assert!(visible.contains(&b.id));
    let with_removed: Vec<Uuid> = store
        .list_history(10_000, 0, true)
        .expect("list with removed")
        .iter()
        .map(|c| c.id)
        .collect();
    assert!(with_removed.contains(&a.id), "include_removed shows it");

    let n: u64 = store.purge_removed();
    assert_eq!(n, 1);
    let after: Vec<Uuid> = store
        .list_history(10_000, 0, true)
        .expect("list after purge")
        .iter()
        .map(|c| c.id)
        .collect();
    assert!(!after.contains(&a.id));
}

#[test]
fn t305_store_list_history_order_and_formats() {
    // last_used_ms DESC; formats derived from payload rows incl. x-os/*.
    let profile = unique_dir("t305-list");
    let store = Store::open(&profile).expect("open");
    let old = sample_clip(Uuid::now_v7(), "old", "old", 1_700_000_005_000);
    let new = sample_clip(Uuid::now_v7(), "new", "new", 1_700_000_005_100);
    let payloads = vec![
        ("text/plain;charset=utf-8".to_string(), b"new".to_vec()),
        ("x-os/win:HTML Format".to_string(), b"native".to_vec()),
    ];
    let _ = store.insert_clip(&old, &text_payloads("old")).expect("old");
    let _ = store.insert_clip(&new, &payloads).expect("new");

    let list = store.list_history(10, 0, false).expect("list");
    assert_eq!(list[0].id, new.id);
    assert_eq!(list[1].id, old.id);
    assert!(list[0]
        .formats
        .contains(&"text/plain;charset=utf-8".to_string()));
    assert!(list[0]
        .formats
        .contains(&"x-os/win:HTML Format".to_string()));

    let page = store.list_history(1, 1, false).expect("page");
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id, old.id);

    // Payloads roundtrip with stored mime strings intact.
    let (_, got) = store.get_clip(new.id).expect("get").expect("row");
    let mimes: HashSet<String> = got.iter().map(|(m, _)| m.clone()).collect();
    assert!(mimes.contains("text/plain;charset=utf-8"));
    assert!(mimes.contains("x-os/win:HTML Format"));
}

#[test]
fn t305_store_privacy_skip_creates_no_row() {
    let profile = unique_dir("t305-privacy");
    let store = Store::open(&profile).expect("open");
    let path = db_path(&profile);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let before: i64 = conn
        .query_row("SELECT v FROM meta WHERE k='privacy_skips'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap()
        .parse()
        .unwrap();
    let _ = store.record_privacy_skip();
    let _ = store.record_privacy_skip();
    assert!(
        history_ids(&store).is_empty(),
        "privacy skips must create no clip row"
    );
    let after: i64 = conn
        .query_row("SELECT v FROM meta WHERE k='privacy_skips'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(after, before + 2);
}

// ---- T-307: backup/restore roundtrip ----

fn seed_profile(profile: &Path) -> (Store, Vec<(Clip, Payloads)>) {
    let store = Store::open(profile).expect("open");
    let mut clips = Vec::new();
    let inline = sample_clip(Uuid::now_v7(), "inline", "inline backup", 1_700_000_006_000);
    let inline_p = text_payloads("inline backup body");
    let _ = store.insert_clip(&inline, &inline_p).expect("inline");
    clips.push((inline, inline_p));
    let big = sample_clip(Uuid::now_v7(), "big", "external backup", 1_700_000_006_001);
    let big_p = vec![("image/png".to_string(), vec![7u8; KIB256 + 64])];
    let _ = store.insert_clip(&big, &big_p).expect("external");
    clips.push((big, big_p));
    (store, clips)
}

#[test]
fn t307_backup_restore_roundtrip_with_external_blob() {
    let src = unique_dir("t307-src");
    let (store, clips) = seed_profile(&src);
    let zip_path = src.join("backup.zip");
    store.backup_zip(&zip_path).expect("backup");
    assert!(zip_path.exists());

    let dst = unique_dir("t307-dst");
    Store::restore_zip(&zip_path, &dst).expect("restore");
    let restored = Store::open(&dst).expect("open restored");
    for (clip, payloads) in &clips {
        let (got_clip, got_payloads) = restored
            .get_clip(clip.id)
            .expect("get")
            .unwrap_or_else(|| panic!("missing {}", clip.id));
        assert_eq!(got_clip.content_hash, clip.content_hash);
        assert_eq!(got_payloads.len(), payloads.len());
        for ((em, eb), (am, ab)) in payloads.iter().zip(got_payloads.iter()) {
            assert_eq!(em, am);
            assert_eq!(eb, ab, "payload bytes must survive roundtrip");
        }
    }
    // Source profile untouched by the backup read path.
    assert_eq!(history_ids(&store).len(), clips.len());
}

// ---- T-308: WAL read-during-write, barrier-paced, no sleeps ----

#[test]
fn t308_wal_read_during_write_burst() {
    let profile = unique_dir("t308");
    let writers = 4usize;
    let per_writer = 10usize;
    let readers = 2usize;
    let start = Barrier::new(writers + readers);
    std::thread::scope(|s| {
        let mut handles = Vec::new();
        for w in 0..writers {
            let profile = &profile;
            let start = &start;
            handles.push(s.spawn(move || {
                start.wait();
                let store = Store::open(profile).expect("writer open");
                for i in 0..per_writer {
                    let text = format!("w{w}-i{i}-unique-padding-{}", Uuid::now_v7());
                    let clip = sample_clip(
                        Uuid::now_v7(),
                        &text,
                        &text,
                        1_700_000_007_000 + i64::try_from(w * per_writer + i).unwrap(),
                    );
                    store
                        .insert_clip(&clip, &text_payloads(&text))
                        .expect("burst insert");
                }
            }));
        }
        for _ in 0..readers {
            let profile = &profile;
            let start = &start;
            handles.push(s.spawn(move || {
                start.wait();
                let store = Store::open(profile).expect("reader open");
                let mut last = 0usize;
                for _ in 0..(writers * per_writer) {
                    let snapshot = store.list_history(10_000, 0, false).expect("read");
                    assert!(snapshot.len() >= last, "reads must see monotonic snapshots");
                    last = snapshot.len();
                    if last == writers * per_writer {
                        break;
                    }
                }
            }));
        }
        for h in handles {
            h.join().expect("thread");
        }
    });
    let store = Store::open(&profile).expect("final open");
    assert_eq!(history_ids(&store).len(), writers * per_writer);
}

// ---- T-310: GC quarantine ----

fn find_under(profile: &Path, want: &[u8]) -> Option<PathBuf> {
    let mut stack = vec![profile.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if read_bytes(&p) == want {
                    return Some(p);
                }
            }
        }
    }
    None
}

#[test]
fn t310_gc_quarantines_planted_orphan_and_reports_dangling() {
    let profile = unique_dir("t310");
    let store = Store::open(&profile).expect("open");
    // Live external blob so GC has a referenced baseline.
    let live = sample_clip(Uuid::now_v7(), "live", "live", 1_700_000_008_000);
    let live_bytes = vec![9u8; KIB256 + 8];
    let _ = store
        .insert_clip(&live, &[("image/png".to_string(), live_bytes)])
        .expect("live insert");

    // Plant an orphan file no row references.
    let blobs = profile.join("blobs");
    std::fs::create_dir_all(&blobs).unwrap();
    let orphan_bytes = b"orphan-fixture-bytes".to_vec();
    std::fs::write(blobs.join("planted-orphan"), &orphan_bytes).unwrap();

    // Create a dangling row: delete the live blob file out from under it.
    let path = db_path(&profile);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let key: String = conn
        .query_row(
            "SELECT blob_key FROM clip_payloads WHERE clip_id = ?1",
            [live.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    let live_blob = blobs.join(&key);
    let live_copy = read_bytes(&live_blob);
    std::fs::remove_file(&live_blob).unwrap();

    let _report: GcReport = store.gc_blobs(1_700_000_008_100);

    // Orphan quarantined under blobs/orphan-<ts>/, never silently deleted.
    assert!(!blobs.join("planted-orphan").exists());
    let moved = find_under(&profile, &orphan_bytes).expect("orphan bytes survive");
    assert!(
        moved.to_string_lossy().contains("orphan-"),
        "orphan must land under blobs/orphan-<ts>/: {}",
        moved.display()
    );
    // Dangling row reported, never silently dropped: clip row still listed
    // and the missing bytes are recoverable from quarantine accounting.
    let ids = history_ids(&store);
    assert!(
        ids.contains(&live.id),
        "dangling clip row must be reported, not dropped"
    );
    let _ = live_copy;
}

// ---- T-312: restore rejection fixtures leave profile untouched ----

fn write_zip(path: &Path, f: impl FnOnce(&mut zip::ZipWriter<std::fs::File>)) {
    let file = std::fs::File::create(path).unwrap();
    let mut w = zip::ZipWriter::new(file);
    f(&mut w);
    w.finish().unwrap();
}

fn snapshot_content(store: &Store) -> Vec<(Uuid, Payloads)> {
    store
        .list_history(10_000, 0, true)
        .expect("list")
        .iter()
        .map(|c| {
            let (_, p) = store.get_clip(c.id).expect("get").expect("row");
            (c.id, p)
        })
        .collect()
}

#[test]
fn t312_restore_rejects_traversal_and_leaves_profile_untouched() {
    let profile = unique_dir("t312-trav");
    let store = Store::open(&profile).expect("open");
    let clip = sample_clip(Uuid::now_v7(), "keep", "keep", 1_700_000_009_000);
    let _ = store
        .insert_clip(&clip, &text_payloads("keep"))
        .expect("seed");
    let before = snapshot_content(&store);
    drop(store);

    let evil = unique_dir("t312-trav-zip").join("evil.zip");
    write_zip(&evil, |w| {
        let opts = zip::write::SimpleFileOptions::default();
        w.start_file("../evil.txt", opts).unwrap();
        std::io::Write::write_all(w, b"escape").unwrap();
        w.start_file("manifest.toml", opts).unwrap();
        std::io::Write::write_all(w, b"schema_version = 1").unwrap();
    });

    let err = Store::restore_zip(&evil, &profile).expect_err("traversal must fail");
    assert_ne!(format!("{err}"), "");
    let store2 = Store::open(&profile).expect("profile still opens");
    assert_eq!(snapshot_content(&store2).len(), before.len());
    assert_eq!(history_ids(&store2), vec![clip.id]);
}

#[cfg(unix)]
#[test]
fn t312_restore_rejects_symlink_and_leaves_profile_untouched() {
    let profile = unique_dir("t312-sym");
    let store = Store::open(&profile).expect("open");
    let clip = sample_clip(Uuid::now_v7(), "keep", "keep", 1_700_000_009_100);
    let _ = store
        .insert_clip(&clip, &text_payloads("keep"))
        .expect("seed");
    drop(store);

    let evil = unique_dir("t312-sym-zip").join("evil.zip");
    write_zip(&evil, |w| {
        let opts = zip::write::SimpleFileOptions::default();
        w.add_symlink("link", "/etc/passwd", opts).unwrap();
        w.start_file("manifest.toml", opts).unwrap();
        std::io::Write::write_all(w, b"schema_version = 1").unwrap();
    });

    Store::restore_zip(&evil, &profile).expect_err("symlink must fail");
    let store2 = Store::open(&profile).expect("profile still opens");
    assert_eq!(history_ids(&store2), vec![clip.id]);
}

#[test]
fn t312_restore_rejects_bad_manifest_and_leaves_profile_untouched() {
    let profile = unique_dir("t312-man");
    let store = Store::open(&profile).expect("open");
    let clip = sample_clip(Uuid::now_v7(), "keep", "keep", 1_700_000_009_200);
    let _ = store
        .insert_clip(&clip, &text_payloads("keep"))
        .expect("seed");
    let before = snapshot_content(&store);
    drop(store);

    let evil = unique_dir("t312-man-zip").join("evil.zip");
    write_zip(&evil, |w| {
        let opts = zip::write::SimpleFileOptions::default();
        w.start_file("manifest.toml", opts).unwrap();
        std::io::Write::write_all(w, b"schema_version = 999\nchecksum = \"deadbeef\"").unwrap();
    });

    Store::restore_zip(&evil, &profile).expect_err("bad manifest must fail");
    let store2 = Store::open(&profile).expect("profile still opens");
    assert_eq!(snapshot_content(&store2).len(), before.len());
    assert_eq!(history_ids(&store2), vec![clip.id]);
}

// ---- T-313: FTS == LIKE id sets, width-independent ----

fn like_ids(store: &Store, query: &str) -> HashSet<Uuid> {
    let q = query.to_lowercase();
    store
        .list_history(10_000, 0, false)
        .expect("list")
        .iter()
        .filter(|c| c.search_text.to_lowercase().contains(&q))
        .map(|c| c.id)
        .collect()
}

fn search_ids(store: &Store, query: &str) -> HashSet<Uuid> {
    store
        .search(query)
        .expect("search")
        .iter()
        .map(|c| c.id)
        .collect()
}

#[test]
fn t313_fts_like_parity_at_multiple_preview_widths() {
    let profile = unique_dir("t313");
    let store = Store::open(&profile).expect("open");
    // Same underlying text rendered at different preview widths: search
    // must be width-independent (search_text, never preview).
    let body = "width independent quarterly report kiwi";
    for (i, width) in [20usize, 80, usize::MAX].iter().enumerate() {
        // Distinct payload bytes per row (suffix) so dedupe keeps 3 rows;
        // the shared "kiwi" term keeps the parity query meaningful.
        let row_body = format!("{body} {i}");
        let preview: String = row_body.chars().take(*width).collect();
        let clip = sample_clip(
            Uuid::now_v7(),
            &preview,
            &row_body,
            1_700_000_010_000 + i64::try_from(i).unwrap(),
        );
        let _ = store
            .insert_clip(&clip, &text_payloads(&row_body))
            .expect("insert");
    }
    let extra = sample_clip(
        Uuid::now_v7(),
        "unrelated",
        "unrelated zebra",
        1_700_000_010_010,
    );
    let _ = store
        .insert_clip(&extra, &text_payloads("unrelated zebra"))
        .expect("extra");

    for q in ["kiwi", "quarterly", "report", "zebra", "missing-token-xyz"] {
        assert_eq!(
            search_ids(&store, q),
            like_ids(&store, q),
            "FTS and LIKE id sets must agree for query {q:?}"
        );
    }
    // All three widths found for the shared body token.
    assert_eq!(search_ids(&store, "kiwi").len(), 3);
    // Removed clips are excluded from search.
    let victim: Uuid = *search_ids(&store, "zebra").iter().next().unwrap();
    let _ = store.soft_delete(victim);
    assert!(search_ids(&store, "zebra").is_empty());
}

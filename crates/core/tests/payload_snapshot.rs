use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use clipmonger_core::hash::content_hash;
use clipmonger_core::model::{Clip, Rev};
use clipmonger_core::store::{PayloadSnapshot, Store};
use clipmonger_core::CoreError;
use sha2::{Digest, Sha256};
use uuid::Uuid;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("cm-payload-{tag}-{}-{n}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
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
        formats: vec![],
        bytes_total: 0,
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

fn hex_of(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 15) as usize] as char);
    }
    out
}

fn inline_payload(text: &str) -> Vec<(String, Vec<u8>)> {
    vec![("text/plain".to_string(), text.as_bytes().to_vec())]
}

fn make_external_data() -> Vec<u8> {
    vec![0xAB_u8; 256 * 1024 + 64]
}

fn insert_clip_with_payloads(store: &Store, clip: &Clip, payloads: &[(String, Vec<u8>)]) {
    store.insert_clip(clip, payloads).unwrap();
}

const EXTERNAL_READ_SUPPORTED: bool = cfg!(any(
    windows,
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
));

fn assert_external_open_error(err: CoreError) {
    if EXTERNAL_READ_SUPPORTED {
        assert!(
            matches!(err, CoreError::Corrupt { .. }),
            "expected corrupt, got {err:?}"
        );
    } else {
        assert!(
            matches!(err, CoreError::Unsupported { .. }),
            "expected unsupported, got {err:?}"
        );
    }
}

#[test]
fn inline_selected_payload() {
    let profile = unique_dir("inline");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "inline", "inline", 1_700_000_000_000);
    let data = b"hello world inline".to_vec();
    insert_clip_with_payloads(&store, &clip, &[("text/plain".to_string(), data.clone())]);
    let snap = store
        .get_payload(clip.id, "text/plain", 1024)
        .unwrap()
        .expect("must exist");
    assert_eq!(snap.clip_id, clip.id);
    assert_eq!(snap.mime, "text/plain");
    assert_eq!(snap.data, data);
    assert!(!snap.content_hash.is_empty());
}

#[test]
fn external_selected_payload() {
    let profile = unique_dir("external");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "ext", "ext", 1_700_000_000_001);
    let data = make_external_data();
    insert_clip_with_payloads(&store, &clip, &[("image/png".to_string(), data.clone())]);
    let result = store.get_payload(clip.id, "image/png", 512 * 1024);
    if EXTERNAL_READ_SUPPORTED {
        let snap = result.unwrap().expect("must exist");
        assert_eq!(snap.data, data);
        assert_eq!(snap.mime, "image/png");
    } else {
        assert!(matches!(result, Err(CoreError::Unsupported { .. })));
        let (_, payloads) = store.get_clip(clip.id).unwrap().unwrap();
        assert_eq!(payloads, vec![("image/png".to_string(), data)]);
    }
}

#[test]
fn mixed_mime_unrelated_missing_still_succeeds() {
    let profile = unique_dir("mixed-missing");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "mixed", "mixed", 1_700_000_000_002);
    let inline_data = b"keep inline".to_vec();
    let ext_data = make_external_data();
    insert_clip_with_payloads(
        &store,
        &clip,
        &[
            ("text/plain".to_string(), inline_data.clone()),
            ("image/png".to_string(), ext_data.clone()),
        ],
    );
    // Find blob key for image/png and delete file to simulate missing unrelated blob
    let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    let key: String = conn
        .query_row(
            "SELECT blob_key FROM clip_payloads WHERE clip_id=?1 AND mime='image/png'",
            [clip.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    let blob_path = profile.join("blobs").join(&key);
    fs::remove_file(&blob_path).unwrap();
    // Selecting inline should still succeed even though unrelated external blob missing
    let snap = store
        .get_payload(clip.id, "text/plain", 1024)
        .unwrap()
        .expect("inline must succeed despite unrelated missing");
    assert_eq!(snap.data, inline_data);
}

#[test]
fn mixed_mime_unrelated_corrupt_still_succeeds() {
    let profile = unique_dir("mixed-corrupt");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "mixed2", "mixed2", 1_700_000_000_003);
    let inline_data = b"keep inline2".to_vec();
    let ext_data = make_external_data();
    insert_clip_with_payloads(
        &store,
        &clip,
        &[
            ("text/plain".to_string(), inline_data.clone()),
            ("image/png".to_string(), ext_data.clone()),
        ],
    );
    let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    let key: String = conn
        .query_row(
            "SELECT blob_key FROM clip_payloads WHERE clip_id=?1 AND mime='image/png'",
            [clip.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    let blob_path = profile.join("blobs").join(&key);
    // Corrupt unrelated blob
    fs::write(&blob_path, b"corrupted").unwrap();
    let snap = store
        .get_payload(clip.id, "text/plain", 1024)
        .unwrap()
        .expect("inline must succeed despite unrelated corrupt");
    assert_eq!(snap.data, inline_data);
}

#[test]
fn selected_external_corrupt_refused() {
    let profile = unique_dir("sel-corrupt");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "c", "c", 1_700_000_000_004);
    let data = make_external_data();
    insert_clip_with_payloads(&store, &clip, &[("image/png".to_string(), data.clone())]);
    let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    let key: String = conn
        .query_row(
            "SELECT blob_key FROM clip_payloads WHERE clip_id=?1",
            [clip.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    let blob_path = profile.join("blobs").join(&key);
    fs::write(&blob_path, b"bad data").unwrap();
    let err = store
        .get_payload(clip.id, "image/png", 512 * 1024)
        .unwrap_err();
    assert_external_open_error(err);
}

#[test]
fn selected_missing_blob_refused() {
    let profile = unique_dir("sel-missing");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "m", "m", 1_700_000_000_005);
    let data = make_external_data();
    insert_clip_with_payloads(&store, &clip, &[("image/png".to_string(), data.clone())]);
    let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    let key: String = conn
        .query_row(
            "SELECT blob_key FROM clip_payloads WHERE clip_id=?1",
            [clip.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    let blob_path = profile.join("blobs").join(&key);
    fs::remove_file(&blob_path).unwrap();
    let err = store
        .get_payload(clip.id, "image/png", 512 * 1024)
        .unwrap_err();
    assert_external_open_error(err);
}

#[test]
fn selected_link_escape_refused() {
    let profile = unique_dir("link-escape");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "link", "link", 1_700_000_000_006);
    let data = make_external_data();
    insert_clip_with_payloads(&store, &clip, &[("image/png".to_string(), data.clone())]);
    let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    let key: String = conn
        .query_row(
            "SELECT blob_key FROM clip_payloads WHERE clip_id=?1",
            [clip.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    let blob_path = profile.join("blobs").join(&key);
    fs::remove_file(&blob_path).unwrap();
    #[cfg(unix)]
    {
        let target = unique_dir("link-target");
        let target_file = target.join("evil");
        fs::write(&target_file, b"evil").unwrap();
        std::os::unix::fs::symlink(&target_file, &blob_path).unwrap();
        let err = store
            .get_payload(clip.id, "image/png", 512 * 1024)
            .unwrap_err();
        // Should be corrupt due to symlink containment
        assert!(
            matches!(err, CoreError::Corrupt { .. }),
            "expected symlink corrupt, got {err:?}"
        );
    }
    #[cfg(not(unix))]
    {
        // On non-unix, just check missing case already covered
        let err = store
            .get_payload(clip.id, "image/png", 512 * 1024)
            .unwrap_err();
        assert_external_open_error(err);
    }
}

#[test]
fn cap_inline_below_equal_above() {
    let profile = unique_dir("cap-inline");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "capinline", "capinline", 1_700_000_000_007);
    let data = b"1234567890".to_vec(); // 10 bytes
    insert_clip_with_payloads(&store, &clip, &[("text/plain".to_string(), data.clone())]);
    // below
    let err = store.get_payload(clip.id, "text/plain", 5).unwrap_err();
    assert!(
        matches!(err, CoreError::TooLarge { .. }),
        "below cap should be TooLarge, got {err:?}"
    );
    // equal
    let snap = store
        .get_payload(clip.id, "text/plain", 10)
        .unwrap()
        .expect("equal cap should succeed");
    assert_eq!(snap.data, data);
    // above
    let snap = store
        .get_payload(clip.id, "text/plain", 20)
        .unwrap()
        .expect("above cap should succeed");
    assert_eq!(snap.data, data);
}

#[test]
fn cap_external_below_equal_above() {
    let profile = unique_dir("cap-ext");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "capext", "capext", 1_700_000_000_008);
    let data = make_external_data(); // 262208
    let len = data.len();
    insert_clip_with_payloads(&store, &clip, &[("image/png".to_string(), data.clone())]);
    let err = store
        .get_payload(clip.id, "image/png", len - 1)
        .unwrap_err();
    assert!(
        matches!(err, CoreError::TooLarge { .. }),
        "below cap external should be TooLarge, got {err:?}"
    );
    if EXTERNAL_READ_SUPPORTED {
        let snap = store
            .get_payload(clip.id, "image/png", len)
            .unwrap()
            .expect("equal cap external succeed");
        assert_eq!(snap.data.len(), len);
        let snap = store
            .get_payload(clip.id, "image/png", len + 1)
            .unwrap()
            .expect("above cap external succeed");
        assert_eq!(snap.data.len(), len);
    } else {
        assert!(matches!(
            store.get_payload(clip.id, "image/png", len),
            Err(CoreError::Unsupported { .. })
        ));
        assert!(matches!(
            store.get_payload(clip.id, "image/png", len + 1),
            Err(CoreError::Unsupported { .. })
        ));
    }
}

#[test]
fn cap_empty_payload() {
    let profile = unique_dir("cap-empty");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "empty", "empty", 1_700_000_000_009);
    let data = Vec::new();
    insert_clip_with_payloads(&store, &clip, &[("text/plain".to_string(), data.clone())]);
    let snap = store
        .get_payload(clip.id, "text/plain", 0)
        .unwrap()
        .expect("empty with cap 0 should succeed");
    assert_eq!(snap.data, data);
    let snap = store
        .get_payload(clip.id, "text/plain", 1)
        .unwrap()
        .expect("empty with cap 1 should succeed");
    assert_eq!(snap.data, data);
}

#[test]
fn cap_overflow_rejected() {
    let profile = unique_dir("overflow");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "ov", "ov", 1_700_000_000_010);
    insert_clip_with_payloads(&store, &clip, &[("text/plain".to_string(), b"hi".to_vec())]);
    let err = store
        .get_payload(clip.id, "text/plain", usize::MAX)
        .unwrap_err();
    assert!(
        matches!(err, CoreError::Backend { .. }),
        "MAX cap should overflow, got {err:?}"
    );
}

#[test]
fn missing_clip_returns_none() {
    let profile = unique_dir("missing-clip");
    let store = Store::open(&profile).unwrap();
    let id = Uuid::now_v7();
    let res = store.get_payload(id, "text/plain", 1024).unwrap();
    assert!(res.is_none(), "missing clip should be None");
}

#[test]
fn removed_clip_returns_none() {
    let profile = unique_dir("removed");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "rem", "rem", 1_700_000_000_011);
    insert_clip_with_payloads(
        &store,
        &clip,
        &[("text/plain".to_string(), b"to be removed".to_vec())],
    );
    store.soft_delete(clip.id).unwrap();
    let res = store.get_payload(clip.id, "text/plain", 1024).unwrap();
    assert!(res.is_none(), "removed clip should be None");
}

#[test]
fn missing_mime_returns_none() {
    let profile = unique_dir("missing-mime");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "mm", "mm", 1_700_000_000_012);
    insert_clip_with_payloads(
        &store,
        &clip,
        &[("text/plain".to_string(), b"only text".to_vec())],
    );
    let res = store.get_payload(clip.id, "image/png", 1024).unwrap();
    assert!(res.is_none(), "missing mime should be None");
}

#[test]
fn metadata_only_stub_returns_none() {
    let profile = unique_dir("stub");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "stub", "stub", 1_700_000_000_013);
    // Insert with empty payloads => stubbed
    let outcome = store.insert_clip(&clip, &[]).unwrap();
    assert!(matches!(
        outcome,
        clipmonger_core::store::InsertOutcome::Stubbed(_)
    ));
    let res = store.get_payload(clip.id, "text/plain", 1024).unwrap();
    assert!(
        res.is_none(),
        "stub with no payloads should be None for any mime"
    );
    let res = store.get_payload(clip.id, "image/png", 1024).unwrap();
    assert!(res.is_none());
}

#[test]
fn metadata_hash_coherent() {
    let profile = unique_dir("coherent");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "coh", "coh", 1_700_000_000_014);
    let data = b"coherent data".to_vec();
    let payloads = vec![("text/plain".to_string(), data.clone())];
    let expected_hash = content_hash(&payloads);
    let mut clip_with_hash = clip.clone();
    clip_with_hash.content_hash = expected_hash.clone();
    insert_clip_with_payloads(&store, &clip_with_hash, &payloads);
    let snap = store
        .get_payload(clip.id, "text/plain", 1024)
        .unwrap()
        .expect("must exist");
    assert_eq!(snap.content_hash, expected_hash);
    assert_eq!(snap.data, data);
    // Also verify via get_clip that content_hash matches
    let (fetched_clip, _) = store.get_clip(clip.id).unwrap().unwrap();
    assert_eq!(fetched_clip.content_hash, snap.content_hash);
}

#[test]
fn external_file_growth_beyond_cap_detected() {
    let profile = unique_dir("growth");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "growth", "growth", 1_700_000_000_015);
    let data = make_external_data();
    insert_clip_with_payloads(&store, &clip, &[("image/png".to_string(), data.clone())]);
    let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    let key: String = conn
        .query_row(
            "SELECT blob_key FROM clip_payloads WHERE clip_id=?1",
            [clip.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    let blob_path = profile.join("blobs").join(&key);
    // Grow file by 1 byte beyond recorded size (tamper) but still within cap
    let mut grown = data.clone();
    grown.push(0xFF);
    fs::write(&blob_path, &grown).unwrap();
    // Cap is len+1 which would allow grown file if only cap check, but recorded len check should still detect size mismatch
    let err = store
        .get_payload(clip.id, "image/png", grown.len())
        .unwrap_err();
    // Supported leaf opens detect the size mismatch; other targets stop first.
    assert_external_open_error(err);
}

#[test]
fn external_row_with_inline_data_is_corrupt() {
    let profile = unique_dir("external-inline-corrupt");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "bad", "bad", 1_700_000_000_016);
    insert_clip_with_payloads(
        &store,
        &clip,
        &[("image/png".to_string(), make_external_data())],
    );
    let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    conn.execute(
        "UPDATE clip_payloads SET data=?1 WHERE clip_id=?2 AND mime='image/png'",
        rusqlite::params![b"bad".as_slice(), clip.id.to_string()],
    )
    .unwrap();
    assert!(matches!(
        store.get_clip(clip.id),
        Err(CoreError::Corrupt { .. })
    ));
    assert!(matches!(
        store.get_payload(clip.id, "image/png", 512 * 1024),
        Err(CoreError::Corrupt { .. })
    ));
}

#[test]
fn null_inline_data_matches_existing_reader() {
    let profile = unique_dir("null-inline");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "null", "null", 1_700_000_000_017);
    insert_clip_with_payloads(&store, &clip, &[("text/plain".to_string(), b"x".to_vec())]);
    let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    conn.execute(
        "UPDATE clip_payloads SET data=NULL WHERE clip_id=?1 AND mime='text/plain'",
        [clip.id.to_string()],
    )
    .unwrap();
    assert_eq!(
        store.get_clip(clip.id).unwrap().unwrap().1[0].1,
        Vec::<u8>::new()
    );
    assert_eq!(
        store
            .get_payload(clip.id, "text/plain", 0)
            .unwrap()
            .unwrap()
            .data,
        Vec::<u8>::new()
    );
}

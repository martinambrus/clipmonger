//! Blob-key filesystem containment regression controls.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use clipmonger_core::model::{Clip, Rev};
use clipmonger_core::store::Store;
use clipmonger_core::CoreError;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use uuid::Uuid;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("cm-contain-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sample_clip(id: Uuid, preview: &str, last_used_ms: i64) -> Clip {
    Clip {
        id,
        created_ms: last_used_ms,
        source_app: Some("test-app".to_string()),
        source_os: "linux".to_string(),
        pinned: false,
        removed: false,
        preview: preview.to_string(),
        search_text: preview.to_string(),
        formats: vec!["text/plain;charset=utf-8".to_string()],
        bytes_total: preview.len() as u64,
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

fn valid_blob_key_for(data: &[u8]) -> (String, String) {
    let hex = hex_of(&Sha256::digest(data));
    let short = hex[..32].to_string();
    (format!("{short}-{}", data.len()), hex)
}

fn make_store_with_external_blob(profile: &Path) -> (Store, String, Vec<u8>) {
    let store = Store::open(profile).unwrap();
    let data = vec![0x42u8; 256 * 1024 + 64];
    let clip = sample_clip(Uuid::now_v7(), "blob-clip", 1_700_000_000_000);
    store
        .insert_clip(
            &clip,
            &[("application/octet-stream".to_string(), data.clone())],
        )
        .unwrap();
    let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    let key: String = conn
        .query_row(
            "SELECT blob_key FROM clip_payloads WHERE clip_id=?1",
            [clip.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    (store, key, data)
}

fn sentinel_path(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let p = std::env::temp_dir().join(format!("cm-sentinel-{tag}-{n}"));
    std::fs::write(&p, b"sentinel-must-survive").unwrap();
    p
}

fn insert_hostile_orphan_blob(profile: &Path, hostile_key: &str) {
    let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    let sha = "aa".repeat(32);
    conn.execute(
        "INSERT INTO clip_blobs(blob_key, sha256, bytes, codec, created_ms) VALUES(?1, ?2, 0, 'raw', 1)",
        rusqlite::params![hostile_key, sha],
    )
    .unwrap();
}

// --- Safe key acceptance ---

#[test]
fn safe_generated_key_accepted_on_open_and_restore() {
    let profile = unique_dir("safe-key");
    let data = vec![9u8; 256 * 1024 + 8];
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "safe", 1_700_000_000_100);
    store
        .insert_clip(&clip, &[("image/png".to_string(), data.clone())])
        .unwrap();
    let (key, _sha) = valid_blob_key_for(&data);
    drop(store);
    let _reopened = Store::open(&profile).expect("safe key must be accepted on open");

    let scratch = unique_dir("safe-restore");
    let zip_path = scratch.join("safe.zip");
    Store::open(&profile)
        .unwrap()
        .backup_zip(&zip_path)
        .unwrap();
    let restored = scratch.join("restored");
    Store::restore_zip(&zip_path, &restored).expect("safe key archive must restore");
    let store2 = Store::open(&restored).unwrap();
    let (_, payloads) = store2.get_clip(clip.id).unwrap().unwrap();
    assert_eq!(payloads[0].1, data);
    assert!(restored.join("blobs").join(&key).exists());
}

// --- Hostile manifest key: must be rejected as Config with owned sentinel ---

#[test]
fn hostile_manifest_traversal_key_rejected_as_config() {
    check_hostile_manifest_key("../evil");
}

#[test]
fn hostile_manifest_absolute_key_rejected_as_config() {
    // Use owned TMPDIR sentinel as absolute target, not /etc/passwd
    let sentinel = unique_dir("manifest-abs-sentinel");
    let sentinel_file = sentinel.join("must-survive.bin");
    std::fs::write(&sentinel_file, vec![1u8; 256 * 1024 + 8]).unwrap();
    // Create hostile key using absolute path to owned file
    let hostile = sentinel_file.to_str().unwrap().to_string();
    check_hostile_manifest_key_owned(&hostile, &sentinel_file);
}

#[test]
fn hostile_manifest_drive_key_rejected_as_config() {
    check_hostile_manifest_key("Z:/clipmonger-owned-lexical-only/missing");
}

#[test]
fn hostile_manifest_backslash_key_rejected() {
    // Construct via typed TOML value to preserve backslash
    let sentinel = unique_dir("manifest-bs-sentinel");
    let sentinel_file = sentinel.join("bs-target");
    std::fs::write(&sentinel_file, b"bs-sentinel-content").unwrap();
    let hostile = "a\\b";
    // Use typed construction: create toml table with backslash key
    check_hostile_manifest_key_typed(hostile, &sentinel_file);
}

fn check_hostile_manifest_key(hostile_key: &str) {
    // Generic version for traversal/drive cases using owned sentinel matching size/hash
    let sentinel_dir = unique_dir("hostile-manifest-sentinel");
    let sentinel_file = sentinel_dir.join("sentinel.bin");
    // Create sentinel with bytes matching original blob's expected size/hash logic
    // We'll use sentinel data of same length as victim data, but distinct content
    // For bypass to succeed, sentinel must exist with matching size/hash that hostile key declares
    let victim_data = vec![1u8; 256 * 1024 + 8];
    let (good_key, _good_sha) = valid_blob_key_for(&victim_data);
    let sentinel_data = victim_data.clone(); // same size/hash as good to make bypass succeed if not validated
    std::fs::write(&sentinel_file, &sentinel_data).unwrap();

    // Now do manifest tampering similar to original but with owned sentinel
    let profile = unique_dir("hostile-manifest");
    let data = victim_data.clone();
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "victim", 1_700_000_000_000);
    store
        .insert_clip(
            &clip,
            &[("application/octet-stream".to_string(), data.clone())],
        )
        .unwrap();
    drop(store);
    let scratch = unique_dir("hostile-manifest-scratch");
    let zip_path = scratch.join("orig.zip");
    Store::open(&profile)
        .unwrap()
        .backup_zip(&zip_path)
        .unwrap();

    let extract = scratch.join("extract");
    std::fs::create_dir_all(&extract).unwrap();
    let file = std::fs::File::open(&zip_path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let out = extract.join(entry.enclosed_name().unwrap());
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        if !entry.is_dir() {
            let mut f = std::fs::File::create(&out).unwrap();
            std::io::copy(&mut entry, &mut f).unwrap();
        }
    }
    let manifest_path = extract.join("manifest.toml");
    let text = std::fs::read_to_string(&manifest_path).unwrap();
    // Replace good key with hostile key; ensure hostile key's declared size matches sentinel (which equals good size) so bypass would not fail on size
    let new_text = text.replace(&good_key, hostile_key);
    std::fs::write(&manifest_path, &new_text).unwrap();

    // If hostile is traversal like ../evil, create file at expected bypass location with matching bytes/hash
    // For ../evil, the path blobs/../evil resolves to extract/evil
    if hostile_key.contains("..") {
        let evil_path = extract.join("evil");
        std::fs::write(&evil_path, &sentinel_data).unwrap();
        // Also ensure blobs/evil exists for other patterns
        std::fs::write(extract.join("blobs").join("evil"), &sentinel_data).unwrap();
    } else if hostile_key.contains('\\') {
        // Backslash case: file name contains backslash, create matching file
        let bs_path = extract.join("blobs").join(hostile_key);
        if let Some(parent) = bs_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&bs_path, &sentinel_data).unwrap();
    } else {
        // Generic: create file at blobs/hostile (though hostile contains slash, it's already handled)
    }

    // Also create sentinel file at blobs/hostile if possible for bypass
    // For absolute path case, the blob path would be blobs/<absolute> which is weird; but we already handled owned sentinel
    // For our generic case, ensure the sentinel file outside is untouched

    // Recompute db sha not needed for this manifest test (DB unchanged)
    let repacked = scratch.join("repacked.zip");
    {
        let file = std::fs::File::create(&repacked).unwrap();
        let mut w = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for entry in walk_files(&extract) {
            let name = entry
                .strip_prefix(&extract)
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\\', "/");
            w.start_file(name, opts).unwrap();
            w.write_all(&std::fs::read(&entry).unwrap()).unwrap();
        }
        w.finish().unwrap();
    }
    let dest = scratch.join("profile-out");
    let sentinel_before = std::fs::read(&sentinel_file).unwrap();
    let err = Store::restore_zip(&repacked, &dest).unwrap_err();
    assert!(
        matches!(err, CoreError::Config { .. }),
        "hostile manifest key {hostile_key:?} must be Config, got {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("invalid blob key"),
        "error must be key-specific, got: {msg:?}"
    );
    assert!(!msg.is_empty() && msg.len() < 2000, "bounded error");
    // Destination bytes unchanged (restore should not have created profile blobbiness)
    assert_eq!(
        std::fs::read(&sentinel_file).unwrap(),
        sentinel_before,
        "sentinel must survive"
    );
    // Destination should not contain hostile file content
    assert!(
        !dest.exists(),
        "invalid restore must not create destination"
    );
}

fn check_hostile_manifest_key_owned(hostile_key: &str, sentinel_file: &Path) {
    let profile = unique_dir("hostile-manifest-owned");
    let data = vec![1u8; 256 * 1024 + 8];
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "victim2", 1_700_000_000_000);
    store
        .insert_clip(
            &clip,
            &[("application/octet-stream".to_string(), data.clone())],
        )
        .unwrap();
    drop(store);
    let (good_key, _) = valid_blob_key_for(&data);
    let scratch = unique_dir("hostile-manifest-owned-scratch");
    let zip_path = scratch.join("orig.zip");
    Store::open(&profile)
        .unwrap()
        .backup_zip(&zip_path)
        .unwrap();
    let extract = scratch.join("extract");
    std::fs::create_dir_all(&extract).unwrap();
    let file = std::fs::File::open(&zip_path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let out = extract.join(entry.enclosed_name().unwrap());
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        if !entry.is_dir() {
            let mut f = std::fs::File::create(&out).unwrap();
            std::io::copy(&mut entry, &mut f).unwrap();
        }
    }
    let manifest_path = extract.join("manifest.toml");
    let text = std::fs::read_to_string(&manifest_path).unwrap();
    let new_text = text.replace(&good_key, hostile_key);
    std::fs::write(&manifest_path, &new_text).unwrap();
    // Ensure sentinel file exists with data that would make bypass succeed if key not checked
    // The sentinel already exists at absolute path; we need to ensure its bytes are not used unless bypass
    let repacked = scratch.join("repacked.zip");
    {
        let file = std::fs::File::create(&repacked).unwrap();
        let mut w = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for entry in walk_files(&extract) {
            let name = entry
                .strip_prefix(&extract)
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\\', "/");
            w.start_file(name, opts).unwrap();
            w.write_all(&std::fs::read(&entry).unwrap()).unwrap();
        }
        w.finish().unwrap();
    }
    let sentinel_before = std::fs::read(sentinel_file).unwrap();
    let dest = scratch.join("out");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("keep"), b"destination-before").unwrap();
    let err = Store::restore_zip(&repacked, &dest).unwrap_err();
    assert_eq!(
        std::fs::read(dest.join("keep")).unwrap(),
        b"destination-before"
    );
    assert!(
        err.to_string().contains("invalid blob key"),
        "key-specific refusal: {err:?}"
    );
    assert!(
        matches!(err, CoreError::Config { .. }),
        "absolute owned manifest key must be Config, got {err:?}"
    );
    assert!(err.to_string().len() < 2000);
    assert_eq!(std::fs::read(sentinel_file).unwrap(), sentinel_before);
}

fn check_hostile_manifest_key_typed(hostile_key: &str, sentinel_file: &Path) {
    // Typed TOML construction to preserve backslash
    let profile = unique_dir("hostile-manifest-typed");
    let data = vec![1u8; 256 * 1024 + 8];
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "victim3", 1_700_000_000_000);
    store
        .insert_clip(
            &clip,
            &[("application/octet-stream".to_string(), data.clone())],
        )
        .unwrap();
    drop(store);
    let scratch = unique_dir("hostile-manifest-typed-scratch");
    let zip_path = scratch.join("orig.zip");
    Store::open(&profile)
        .unwrap()
        .backup_zip(&zip_path)
        .unwrap();
    let extract = scratch.join("extract");
    std::fs::create_dir_all(&extract).unwrap();
    let file = std::fs::File::open(&zip_path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let out = extract.join(entry.enclosed_name().unwrap());
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        if !entry.is_dir() {
            let mut f = std::fs::File::create(&out).unwrap();
            std::io::copy(&mut entry, &mut f).unwrap();
        }
    }
    // Use typed TOML construction: parse manifest, modify blobs[0].key via Value, then serialize
    let manifest_path = extract.join("manifest.toml");
    let text = std::fs::read_to_string(&manifest_path).unwrap();
    let mut man: toml::Value = toml::from_str(&text).unwrap();
    if let Some(blobs) = man.get_mut("blobs").and_then(|v| v.as_array_mut()) {
        if let Some(first) = blobs.get_mut(0) {
            if let Some(tbl) = first.as_table_mut() {
                tbl.insert(
                    "key".to_string(),
                    toml::Value::String(hostile_key.to_string()),
                );
            }
        }
    }
    let new_text = toml::to_string(&man).unwrap();
    assert!(
        new_text.contains("a\\b") || new_text.contains("a\\\\b"),
        "backslash must be preserved via typed TOML: {new_text}"
    );
    std::fs::write(&manifest_path, new_text).unwrap();
    // Ensure we didn't lose good_key replacement effect
    let repacked = scratch.join("repacked.zip");
    {
        let file = std::fs::File::create(&repacked).unwrap();
        let mut w = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for entry in walk_files(&extract) {
            let name = entry
                .strip_prefix(&extract)
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\\', "/");
            w.start_file(name, opts).unwrap();
            w.write_all(&std::fs::read(&entry).unwrap()).unwrap();
        }
        w.finish().unwrap();
    }
    let sentinel_before = std::fs::read(sentinel_file).unwrap();
    let dest = scratch.join("out");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("keep"), b"destination-before").unwrap();
    let err = Store::restore_zip(&repacked, &dest).unwrap_err();
    assert_eq!(
        std::fs::read(dest.join("keep")).unwrap(),
        b"destination-before"
    );
    assert!(
        err.to_string().contains("invalid blob key"),
        "key-specific refusal: {err:?}"
    );
    assert!(
        matches!(err, CoreError::Config { .. }),
        "backslash manifest key must be Config, got {err:?}"
    );
    assert!(err.to_string().len() < 2000);
    assert_eq!(std::fs::read(sentinel_file).unwrap(), sentinel_before);
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

// --- Hostile DB keys with recomputed db checksum ---

#[test]
fn hostile_orphan_db_key_rejected_on_open_as_corrupt() {
    check_hostile_db_key_rejected("../evil", true);
}

#[test]
fn hostile_payload_db_key_rejected_on_open_as_corrupt() {
    check_hostile_db_key_rejected("/absolute/evil", false);
}

#[test]
fn hostile_db_key_colon_rejected_on_open() {
    check_hostile_db_key_rejected("abc:def", true);
}

#[test]
fn hostile_db_key_uppercase_hex_rejected() {
    check_hostile_db_key_rejected("AA000000000000000000000000000000-123", true);
}

fn check_hostile_db_key_rejected(hostile_key: &str, as_orphan: bool) {
    let profile = unique_dir("hostile-db");
    let (store, _good_key, _data) = make_store_with_external_blob(&profile);
    drop(store);
    if as_orphan {
        insert_hostile_orphan_blob(&profile, hostile_key);
    } else {
        let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
        let clip_id: String = conn
            .query_row("SELECT id FROM clips LIMIT 1", [], |r| r.get(0))
            .unwrap();
        let sha = "bb".repeat(32);
        conn.execute(
            "UPDATE clip_payloads SET blob_key=?1 WHERE clip_id=?2",
            rusqlite::params![hostile_key, clip_id],
        )
        .unwrap();
        // Ensure matching blob metadata exists but don't use same invalid key as orphan to mask diagnosis
        // Create a distinct valid blob row for the original good key still exists, but we insert hostile metadata separately
        conn.execute(
            "INSERT OR REPLACE INTO clip_blobs(blob_key, sha256, bytes, codec, created_ms) VALUES(?1, ?2, 0, 'raw', 1)",
            rusqlite::params![hostile_key, sha],
        )
        .unwrap();
        drop(conn);
    }
    let err = Store::open(&profile).unwrap_err();
    assert!(
        matches!(err, CoreError::Corrupt { .. }),
        "hostile DB key {hostile_key:?} must be Corrupt, got {err:?}"
    );
    assert!(err.to_string().len() < 2000 && !err.to_string().is_empty());
}

#[test]
fn hostile_db_key_rejected_in_restore_as_corrupt_or_config() {
    let profile = unique_dir("hostile-restore-db");
    let data = vec![5u8; 256 * 1024 + 8];
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "r", 1_700_000_000_000);
    store
        .insert_clip(
            &clip,
            &[("application/octet-stream".to_string(), data.clone())],
        )
        .unwrap();
    let scratch = unique_dir("restore-db-manip");
    let zip_path = scratch.join("orig.zip");
    store.backup_zip(&zip_path).unwrap();
    drop(store);

    let extract = scratch.join("extract");
    std::fs::create_dir_all(&extract).unwrap();
    let file = std::fs::File::open(&zip_path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let out = extract.join(entry.enclosed_name().unwrap());
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        if !entry.is_dir() {
            let mut f = std::fs::File::create(&out).unwrap();
            std::io::copy(&mut entry, &mut f).unwrap();
        }
    }
    let hostile = "../../evil-123";
    let conn = rusqlite::Connection::open(extract.join("clipstore.sqlite3")).unwrap();
    conn.execute(
        "INSERT INTO clip_blobs(blob_key, sha256, bytes, codec, created_ms) VALUES(?1, ?2, 0, 'raw', 1)",
        rusqlite::params![hostile, "cc".repeat(32)],
    )
    .unwrap();
    drop(conn);
    let db_bytes = std::fs::read(extract.join("clipstore.sqlite3")).unwrap();
    let new_sha = hex_of(&Sha256::digest(&db_bytes));
    let manifest = std::fs::read_to_string(extract.join("manifest.toml")).unwrap();
    let new_manifest = if manifest.contains("db_sha256") {
        let re_sha = manifest
            .lines()
            .find(|l| l.contains("db_sha256"))
            .unwrap()
            .to_string();
        manifest.replacen(&re_sha, &format!("db_sha256 = {new_sha:?}"), 1)
    } else {
        manifest
    };
    std::fs::write(extract.join("manifest.toml"), new_manifest).unwrap();

    let repacked = scratch.join("repacked.zip");
    {
        let file = std::fs::File::create(&repacked).unwrap();
        let mut w = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for entry in walk_files(&extract) {
            let name = entry
                .strip_prefix(&extract)
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\\', "/");
            w.start_file(name, opts).unwrap();
            w.write_all(&std::fs::read(&entry).unwrap()).unwrap();
        }
        w.finish().unwrap();
    }
    let err = Store::restore_zip(&repacked, &scratch.join("out-profile")).unwrap_err();
    assert!(
        matches!(err, CoreError::Corrupt { .. } | CoreError::Config { .. }),
        "hostile orphan DB key in restore must be Corrupt/Config, got {err:?}"
    );
    assert!(err.to_string().len() < 2000);
}

#[test]
fn payload_only_hostile_key_rejected_in_restore() {
    // Payload-only mutation with corrected checksum must be Corrupt
    let profile = unique_dir("hostile-restore-payload-only");
    let data = vec![7u8; 256 * 1024 + 8];
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "payload-only", 1_700_000_000_000);
    store
        .insert_clip(
            &clip,
            &[("application/octet-stream".to_string(), data.clone())],
        )
        .unwrap();
    let scratch = unique_dir("restore-payload-only");
    let zip_path = scratch.join("orig.zip");
    store.backup_zip(&zip_path).unwrap();
    drop(store);
    let extract = scratch.join("extract");
    std::fs::create_dir_all(&extract).unwrap();
    let file = std::fs::File::open(&zip_path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let out = extract.join(entry.enclosed_name().unwrap());
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        if !entry.is_dir() {
            let mut f = std::fs::File::create(&out).unwrap();
            std::io::copy(&mut entry, &mut f).unwrap();
        }
    }
    // Mutate only payload blob_key, ensure clip_blobs does NOT already have that hostile key (so we can test missing metadata case)
    // First get original payload key
    let conn = rusqlite::Connection::open(extract.join("clipstore.sqlite3")).unwrap();
    let orig_key: String = conn
        .query_row("SELECT blob_key FROM clip_payloads LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    let hostile = "payload-only-hostile-../evil";
    // Update payload to hostile, but do NOT insert hostile into clip_blobs (so external metadata missing)
    conn.execute(
        "UPDATE clip_payloads SET blob_key=?1",
        rusqlite::params![hostile],
    )
    .unwrap();
    // Remove the old blob row to ensure orphan not masking?
    // Keep old blob row? If we keep it, then hostile key would be dangling (missing metadata) which is what we want to test
    // Ensure we didn't accidentally insert hostile into clip_blobs elsewhere
    drop(conn);
    let db_bytes = std::fs::read(extract.join("clipstore.sqlite3")).unwrap();
    let new_sha = hex_of(&Sha256::digest(&db_bytes));
    let manifest = std::fs::read_to_string(extract.join("manifest.toml")).unwrap();
    let new_manifest = if manifest.contains("db_sha256") {
        let re = manifest
            .lines()
            .find(|l| l.contains("db_sha256"))
            .unwrap()
            .to_string();
        manifest.replacen(&re, &format!("db_sha256 = {new_sha:?}"), 1)
    } else {
        manifest
    };
    std::fs::write(extract.join("manifest.toml"), new_manifest).unwrap();
    let repacked = scratch.join("repacked.zip");
    {
        let file = std::fs::File::create(&repacked).unwrap();
        let mut w = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for entry in walk_files(&extract) {
            let name = entry
                .strip_prefix(&extract)
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\\', "/");
            w.start_file(name, opts).unwrap();
            w.write_all(&std::fs::read(&entry).unwrap()).unwrap();
        }
        w.finish().unwrap();
    }
    let err = Store::restore_zip(&repacked, &scratch.join("out2")).unwrap_err();
    assert!(
        matches!(err, CoreError::Corrupt { .. }),
        "payload-only hostile must be Corrupt, got {err:?} orig {orig_key:?}"
    );
    assert!(
        err.to_string().contains("blob")
            || err.to_string().contains("invalid")
            || err.to_string().contains("missing"),
        "should mention blob validation: {err:?}"
    );
}

// --- Already-open Store tampered via another connection ---

#[test]
fn tampered_absolute_orphan_preserves_sentinel_on_purge() {
    check_tampered_preserves_sentinel(true, false);
}

#[test]
fn tampered_traversal_orphan_preserves_sentinel_on_retention() {
    check_tampered_preserves_sentinel(false, true);
}

#[test]
fn tampered_zero_removal_purge_preserves_sentinel() {
    check_tampered_zero_removal_preserves_sentinel();
}

fn check_tampered_preserves_sentinel(use_absolute: bool, use_retention: bool) {
    let profile = unique_dir("tamper-purge");
    let (store, _key, _data) = make_store_with_external_blob(&profile);
    let sentinel = sentinel_path("purge");
    let hostile = if use_absolute {
        sentinel.to_str().unwrap().to_string()
    } else {
        format!("../../{}", sentinel.file_name().unwrap().to_str().unwrap())
    };
    {
        let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
        let victim: String = conn
            .query_row("SELECT id FROM clips LIMIT 1", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "UPDATE clips SET removed=1 WHERE id=?1",
            rusqlite::params![victim],
        )
        .unwrap();
        insert_hostile_orphan_blob(&profile, &hostile);
        drop(conn);
    }
    let before = db_rows(&profile);
    if use_retention {
        let limits = clipmonger_core::policy::Limits {
            max_items: 1,
            max_age_days: 0,
            max_store_bytes: u64::MAX,
        };
        assert!(matches!(
            store.enforce_retention(&limits).unwrap_err(),
            CoreError::Corrupt { .. }
        ));
    } else {
        assert_eq!(store.purge_removed(), 0);
    }
    assert_eq!(db_rows(&profile), before);
    assert!(
        sentinel.exists(),
        "sentinel {} must survive hostile orphan {hostile:?}",
        sentinel.display()
    );
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"sentinel-must-survive");
    std::fs::remove_file(&sentinel).unwrap();
}

fn check_tampered_zero_removal_preserves_sentinel() {
    let profile = unique_dir("tamper-zero");
    let (store, _key, _data) = make_store_with_external_blob(&profile);
    let sentinel = sentinel_path("zero");
    let hostile = sentinel.to_str().unwrap().to_string();
    // No clip marked removed; purge should do zero work but still must preserve sentinel
    {
        let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
        insert_hostile_orphan_blob(&profile, &hostile);
        drop(conn);
    }
    let before = db_rows(&profile);
    let n = store.purge_removed();
    assert_eq!(n, 0);
    assert_eq!(db_rows(&profile), before);
    assert!(
        sentinel.exists(),
        "zero-removal purge must preserve sentinel {} hostile {hostile:?}",
        sentinel.display()
    );
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"sentinel-must-survive");
    std::fs::remove_file(&sentinel).unwrap();
}

// --- D1 multi-blob normal+linked refusal test ---
#[test]
#[cfg(unix)]
fn multi_blob_purge_preserves_all_on_linked_refusal() {
    // Create clip with two external blobs
    let profile = unique_dir("multi-blob");
    let store = Store::open(&profile).unwrap();
    let data1 = vec![0x11u8; 300 * 1024];
    let data2 = vec![0x22u8; 300 * 1024];
    let clip = sample_clip(Uuid::now_v7(), "two-blobs", 1_700_000_000_000);
    store
        .insert_clip(
            &clip,
            &[
                ("application/octet-stream".to_string(), data1.clone()),
                ("image/png".to_string(), data2.clone()),
            ],
        )
        .unwrap();
    let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    let keys: Vec<String> = conn
        .prepare("SELECT blob_key FROM clip_payloads WHERE clip_id=?1 ORDER BY rowid")
        .unwrap()
        .query_map([clip.id.to_string()], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(keys.len(), 2);
    let first_key = keys[0].clone();
    let second_key = keys[1].clone();
    // Get original file bytes for verification
    let blob1_path = profile.join("blobs").join(&first_key);
    let blob2_path = profile.join("blobs").join(&second_key);
    let orig1 = std::fs::read(&blob1_path).unwrap();
    let orig2 = std::fs::read(&blob2_path).unwrap();
    assert_eq!(orig1, data1);
    assert_eq!(orig2, data2);
    // Mark clip removed
    conn.execute(
        "UPDATE clips SET removed=1 WHERE id=?1",
        rusqlite::params![clip.id.to_string()],
    )
    .unwrap();
    drop(conn);
    // Tamper second blob to be symlink to owned file with same data (so bypass would succeed)
    let target_dir = unique_dir("multi-blob-target");
    let target_file = target_dir.join("target");
    std::fs::write(&target_file, &data2).unwrap();
    #[cfg(unix)]
    {
        std::fs::remove_file(&blob2_path).unwrap();
        std::os::unix::fs::symlink(&target_file, &blob2_path).unwrap();
    }
    // Attempt purge: should fail or preserve all rows/files
    let before_conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    let before_blobs: Vec<String> = before_conn
        .prepare("SELECT blob_key FROM clip_blobs")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let before_count = before_blobs.len();
    drop(before_conn);

    let before = db_rows(&profile);
    assert_eq!(store.purge_removed(), 0);
    assert_eq!(db_rows(&profile), before);
    // The public purge API returns zero on refusal.

    let after_conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    let after_blobs: Vec<String> = after_conn
        .prepare("SELECT blob_key FROM clip_blobs")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        after_blobs.len(),
        before_count,
        "all blob rows must survive"
    );
    assert!(after_blobs.contains(&first_key) && after_blobs.contains(&second_key));
    drop(after_conn);
    // Files must survive with original bytes (first file still exists, second still symlink but target unchanged)
    assert_eq!(std::fs::read(&blob1_path).unwrap(), orig1);
    // The second path is still symlink, but its target must still have original bytes and not be deleted
    assert_eq!(std::fs::read(&target_file).unwrap(), data2);
    // Also verify DB rows for clips/payloads still present (transaction rollback)
    let conn2 = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    let clip_exists: i64 = conn2
        .query_row(
            "SELECT COUNT(*) FROM clips WHERE id=?1",
            [clip.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(clip_exists, 1, "clip row must survive rollback");
    drop(conn2);
    // Cleanup
    std::fs::remove_file(&target_file).unwrap();
}

#[test]
#[cfg(unix)]
fn multi_blob_purge_both_orderings_preserved() {
    for order in [0, 1] {
        let profile = unique_dir(&format!("multi-blob-order-{order}"));
        let store = Store::open(&profile).unwrap();
        let d1 = vec![0x33u8; 300 * 1024];
        let d2 = vec![0x44u8; 300 * 1024];
        let clip = sample_clip(Uuid::now_v7(), "two-blobs-order", 1_700_000_000_000);
        store
            .insert_clip(
                &clip,
                &[
                    ("a.bin".to_string(), d1.clone()),
                    ("b.bin".to_string(), d2.clone()),
                ],
            )
            .unwrap();
        let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
        let mut keys: Vec<String> = conn
            .prepare("SELECT blob_key FROM clip_payloads WHERE clip_id=?1 ORDER BY rowid")
            .unwrap()
            .query_map([clip.id.to_string()], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        if order == 1 {
            keys.reverse();
        }
        let k1 = keys[0].clone();
        let k2 = keys[1].clone();
        conn.execute(
            "UPDATE clips SET removed=1 WHERE id=?1",
            rusqlite::params![clip.id.to_string()],
        )
        .unwrap();
        drop(conn);
        let p1 = profile.join("blobs").join(&k1);
        let p2 = profile.join("blobs").join(&k2);
        let orig1 = std::fs::read(&p1).unwrap();
        let orig2 = std::fs::read(&p2).unwrap();
        // Make k2 symlink with original bytes in target
        let tdir = unique_dir(&format!("multi-order-target-{order}"));
        let tgt = tdir.join("tgt");
        std::fs::write(&tgt, &orig2).unwrap();
        #[cfg(unix)]
        {
            std::fs::remove_file(&p2).unwrap();
            std::os::unix::fs::symlink(&tgt, &p2).unwrap();
        }
        let before = db_rows(&profile);
        assert_eq!(store.purge_removed(), 0);
        assert_eq!(db_rows(&profile), before);
        // After attempt, both blob rows must remain and first file unchanged, second target unchanged
        let conn2 = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
        let cnt: i64 = conn2
            .query_row("SELECT COUNT(*) FROM clip_blobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt, 2, "order {order} must preserve all rows");
        drop(conn2);
        assert_eq!(std::fs::read(&tgt).unwrap(), orig2);
        // The first file should still have original bytes (if it was deleted then rollback would have restored DB but file gone)
        // Since we preflight all leaves before deletion, first file should still exist with same bytes
        assert!(!std::fs::symlink_metadata(&p1)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read(&p1).unwrap(),
            orig1,
            "order {order} first file must survive"
        );
        assert_eq!(std::fs::read_link(&p2).unwrap(), tgt);
        assert_rows(&profile, &[1, 2, 2]);
    }
}

// --- Malformed / overflow key rejection via DB ---

#[test]
fn malformed_key_leading_zero_rejected_on_open() {
    check_hostile_db_key_rejected("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-001", true);
}

#[test]
fn overflow_key_rejected_on_open() {
    check_hostile_db_key_rejected("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-9223372036854775808", true);
}

#[test]
fn key_with_nul_rejected_on_open() {
    check_hostile_db_key_rejected("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-1\x00", true);
}

#[test]
fn empty_key_rejected_on_open() {
    check_hostile_db_key_rejected("", true);
}

#[test]
fn key_missing_dash_rejected_on_open() {
    check_hostile_db_key_rejected("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa123", true);
}

// --- Unix link containment ---

#[cfg(unix)]
#[test]
fn blob_leaf_symlink_not_followed_on_get() {
    let profile = unique_dir("leaf-link");
    let data = vec![2u8; 256 * 1024 + 8];
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "link-test", 1_700_000_000_000);
    store
        .insert_clip(
            &clip,
            &[("application/octet-stream".to_string(), data.clone())],
        )
        .unwrap();
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
    let target = unique_dir("link-target");
    let secret = target.join("secret");
    // Write original matching bytes/hash so unguarded read would succeed
    std::fs::write(&secret, &data).unwrap();
    std::fs::remove_file(&blob_path).unwrap();
    std::os::unix::fs::symlink(&secret, &blob_path).unwrap();
    let result = store.get_clip(clip.id);
    assert!(
        result.is_err(),
        "symlinked blob leaf must be rejected, got {result:?}"
    );
    let err = result.unwrap_err();
    assert!(
        matches!(err, CoreError::Corrupt { .. }),
        "must be Corrupt, got {err:?}"
    );
    assert_eq!(std::fs::read(&secret).unwrap(), data);
}

#[cfg(unix)]
#[test]
fn blob_root_symlink_rejected_on_read() {
    let outer = unique_dir("root-link-outer");
    let profile = outer.join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    let data = vec![3u8; 256 * 1024 + 8];
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "rootlink", 1_700_000_000_000);
    store
        .insert_clip(
            &clip,
            &[("application/octet-stream".to_string(), data.clone())],
        )
        .unwrap();
    let blobs = profile.join("blobs");
    let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    drop(conn);
    drop(store);
    let real_blobs = outer.join("real-blobs");
    std::fs::rename(&blobs, &real_blobs).unwrap();
    std::os::unix::fs::symlink(&real_blobs, &blobs).unwrap();
    std::fs::set_permissions(&real_blobs, std::fs::Permissions::from_mode(0o750)).unwrap();
    let target_mode_before = std::fs::metadata(&real_blobs).unwrap().permissions().mode();
    let err = Store::open(&profile).unwrap_err();
    assert!(matches!(err, CoreError::Corrupt { .. }));
    assert_eq!(
        std::fs::metadata(&real_blobs).unwrap().permissions().mode(),
        target_mode_before
    );
}

#[cfg(unix)]
#[test]
fn gc_skips_symlinked_orphan_and_preserves_target() {
    let profile = unique_dir("gc-link");
    let store = Store::open(&profile).unwrap();
    let data = vec![4u8; 256 * 1024 + 8];
    let clip = sample_clip(Uuid::now_v7(), "gc-link-clip", 1_700_000_000_000);
    store
        .insert_clip(
            &clip,
            &[("application/octet-stream".to_string(), data.clone())],
        )
        .unwrap();
    let target = unique_dir("gc-link-target");
    let secret = target.join("secret");
    std::fs::write(&secret, b"gc-secret").unwrap();
    std::os::unix::fs::symlink(&secret, profile.join("blobs").join("evil-link")).unwrap();
    let report = store.gc_blobs(1_700_000_000_123);
    assert!(std::path::Path::new(&secret).exists());
    assert_eq!(std::fs::read(&secret).unwrap(), b"gc-secret");
    assert!(
        profile.join("blobs").join("evil-link").exists()
            || std::fs::symlink_metadata(profile.join("blobs").join("evil-link")).is_ok()
    );
    assert_eq!(report.orphaned_files, 0);
}

#[cfg(unix)]
#[test]
fn quarantine_dir_symlink_rejected() {
    let profile = unique_dir("quarantine-link");
    let store = Store::open(&profile).unwrap();
    let data = vec![6u8; 256 * 1024 + 8];
    let clip = sample_clip(Uuid::now_v7(), "q", 1_700_000_000_000);
    store
        .insert_clip(
            &clip,
            &[("application/octet-stream".to_string(), data.clone())],
        )
        .unwrap();
    // Plant movable orphan
    let orphan_path = profile.join("blobs").join("orphan-arbitrary-123");
    std::fs::write(&orphan_path, b"orphan-data").unwrap();
    let target = unique_dir("q-target");
    std::fs::write(target.join("keep"), b"keep").unwrap();
    let link = profile.join("blobs").join("orphan-1");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let before_orphan = std::fs::read(&orphan_path).unwrap();
    let before_target = std::fs::read(target.join("keep")).unwrap();
    assert_eq!(store.gc_blobs(1).orphaned_files, 0);
    // Orphan must stay in place (not moved) because quarantine is symlink
    assert!(
        orphan_path.exists(),
        "movable orphan must stay when quarantine is symlink"
    );
    assert_eq!(std::fs::read(&orphan_path).unwrap(), before_orphan);
    // Target must remain byte-for-byte empty/unchanged (we didn't write there)
    assert_eq!(std::fs::read(target.join("keep")).unwrap(), before_target);
    assert!(target.exists());
    assert!(
        !target.join("orphan-arbitrary-123").exists(),
        "quarantine target must not receive files"
    );
}

#[cfg(unix)]
#[test]
fn blob_temp_symlink_rejected_on_write() {
    let profile = unique_dir("temp-link");
    let store = Store::open(&profile).unwrap();
    let data = vec![7u8; 300 * 1024];
    let (key, _) = valid_blob_key_for(&data);
    // Isolate this test in a fresh test process so the attempted temp counter is known.
    if std::env::var_os("CLIPMONGER_TEMP_LINK_CHILD").is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "blob_temp_symlink_rejected_on_write",
                "--nocapture",
            ])
            .env("CLIPMONGER_TEMP_LINK_CHILD", "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    // Pre-plant temp path as symlink
    let tmp_name = format!(".tmp-{key}-{}-{}", std::process::id(), 0);
    let tmp_path = profile.join("blobs").join(&tmp_name);
    let target = unique_dir("temp-link-target");
    std::os::unix::fs::symlink(target.join("victim"), &tmp_path).unwrap();
    std::fs::write(target.join("victim"), b"victim").unwrap();
    // Try to write blob; should reject without following link
    let clip = sample_clip(Uuid::now_v7(), "temp-test", 1_700_000_000_000);
    // This will attempt write_blob_file internally via insert
    let res = store.insert_clip(
        &clip,
        &[("application/octet-stream".to_string(), data.clone())],
    );
    let err = res.unwrap_err();
    assert!(matches!(err, CoreError::Corrupt { .. }));
    assert!(
        err.to_string().contains("temp"),
        "must refuse planted attempted temp: {err:?}"
    );
    assert_eq!(
        std::fs::read_link(&tmp_path).unwrap(),
        target.join("victim")
    );
    assert!(target.join("victim").exists());
    assert_eq!(std::fs::read(target.join("victim")).unwrap(), b"victim");
}

#[cfg(unix)]
#[test]
fn blob_final_symlink_not_followed_on_write() {
    let profile = unique_dir("final-link");
    let store = Store::open(&profile).unwrap();
    let data = vec![8u8; 300 * 1024];
    let (key, _) = valid_blob_key_for(&data);
    let final_path = profile.join("blobs").join(&key);
    let target = unique_dir("final-link-target");
    let victim = target.join("victim");
    std::fs::write(&victim, b"original-victim").unwrap();
    std::os::unix::fs::symlink(&victim, &final_path).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "final-test", 1_700_000_000_000);
    let res = store.insert_clip(
        &clip,
        &[("application/octet-stream".to_string(), data.clone())],
    );
    assert!(matches!(res.unwrap_err(), CoreError::Corrupt { .. }));
    assert_eq!(std::fs::read_link(&final_path).unwrap(), victim);
    // Should not have overwritten victim via symlink
    assert_eq!(std::fs::read(&victim).unwrap(), b"original-victim");
}

#[test]
fn retention_enforces_with_real_pressure_and_preserves_on_hostile() {
    let profile = unique_dir("retention-pressure");
    let store = Store::open(&profile).unwrap();
    // Create 3 live clips with external blobs
    let mut clips = Vec::new();
    for i in 0u8..3 {
        let data = vec![i + 10; 300 * 1024];
        let clip = sample_clip(
            Uuid::now_v7(),
            &format!("retain-{i}"),
            1_700_000_000_000 + i64::from(i),
        );
        store
            .insert_clip(&clip, &[("application/octet-stream".to_string(), data)])
            .unwrap();
        clips.push(clip);
    }
    // Insert hostile orphan
    let sentinel = sentinel_path("retention");
    let hostile = sentinel.to_str().unwrap();
    insert_hostile_orphan_blob(&profile, hostile);
    // Now enforce with max_items=1 (real pressure: 3 live >1)
    let limits = clipmonger_core::policy::Limits {
        max_items: 1,
        max_age_days: 0,
        max_store_bytes: u64::MAX,
    };
    let before = db_rows(&profile);
    let before_files: Vec<_> = std::fs::read_dir(profile.join("blobs"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let data = std::fs::read(&path).unwrap();
            (path, data)
        })
        .collect();
    let res = store.enforce_retention(&limits);
    assert!(matches!(res.unwrap_err(), CoreError::Corrupt { .. }));
    assert_eq!(db_rows(&profile), before);
    for (path, data) in before_files {
        assert_eq!(std::fs::read(path).unwrap(), data);
    }
    assert_rows(&profile, &[3, 3, 4]);
    assert!(
        sentinel.exists(),
        "retention hostile should preserve sentinel"
    );
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"sentinel-must-survive");
    // DB should still have at least 1 live clip (sweep should have been refused before deletion)
    let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    let live: i64 = conn
        .query_row("SELECT COUNT(*) FROM clips WHERE removed=0", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(live, 3, "all live clips must survive hostile retention");
    std::fs::remove_file(&sentinel).unwrap();
}

use std::io::Write as _;

type TableRows = Vec<Vec<rusqlite::types::Value>>;

fn db_rows(profile: &Path) -> Vec<TableRows> {
    let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    ["clips", "clip_payloads", "clip_blobs"]
        .iter()
        .map(|table| {
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .unwrap();
            let n = stmt.column_count();
            stmt.query_map([], |row| {
                (0..n).map(|i| row.get(i)).collect::<Result<Vec<_>, _>>()
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
        })
        .collect()
}

fn assert_rows(profile: &Path, counts: &[usize]) {
    let rows = db_rows(profile);
    assert_eq!(rows.iter().map(Vec::len).collect::<Vec<_>>(), counts);
}

#[cfg(unix)]
#[test]
fn byte_retention_preflights_all_orphans_before_unlinking() {
    for linked_oldest in [false, true] {
        let profile = unique_dir("byte-retention-preflight");
        let store = Store::open(&profile).unwrap();
        let mut paths = Vec::new();
        for i in 0u8..2 {
            let data = vec![31 + i; 300 * 1024];
            let mut clip = sample_clip(
                Uuid::now_v7(),
                "byte-retain",
                1_700_000_000_000 + i64::from(i),
            );
            clip.bytes_total = u64::try_from(data.len()).unwrap();
            store
                .insert_clip(&clip, &[("image/png".into(), data.clone())])
                .unwrap();
            paths.push((
                profile.join("blobs").join(valid_blob_key_for(&data).0),
                data,
            ));
        }
        let linked = usize::from(!linked_oldest);
        let normal = 1 - linked;
        let target = unique_dir("byte-retention-target").join("owned");
        std::fs::write(&target, &paths[linked].1).unwrap();
        std::fs::remove_file(&paths[linked].0).unwrap();
        std::os::unix::fs::symlink(&target, &paths[linked].0).unwrap();
        let before = db_rows(&profile);
        let limits = clipmonger_core::policy::Limits {
            max_items: u64::MAX,
            max_age_days: 0,
            max_store_bytes: 0,
        };
        let err = store.enforce_retention(&limits).unwrap_err();
        assert!(matches!(err, CoreError::Corrupt { .. }));
        assert_eq!(db_rows(&profile), before, "all rows must roll back");
        assert_eq!(std::fs::read(&paths[normal].0).unwrap(), paths[normal].1);
        assert_eq!(std::fs::read_link(&paths[linked].0).unwrap(), target);
        assert_eq!(std::fs::read(&target).unwrap(), paths[linked].1);
    }
}

#[test]
fn purge_validates_metadata_before_cascade_and_zero_removal_sweep() {
    for mutation in [
        "UPDATE clip_payloads SET blob_key='../owned'",
        "UPDATE clip_payloads SET external=2",
        "UPDATE clip_payloads SET data=X'61'",
        "UPDATE clip_blobs SET bytes=bytes+1",
    ] {
        for removed in [true, false] {
            let profile = unique_dir("pre-cascade");
            let (store, key, data) = make_store_with_external_blob(&profile);
            let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
            conn.execute(mutation, []).unwrap();
            if removed {
                conn.execute("UPDATE clips SET removed=1", []).unwrap();
            }
            let before = db_rows(&profile);
            assert_eq!(store.purge_removed(), 0, "must refuse {mutation}");
            assert_eq!(db_rows(&profile), before, "must not cascade {mutation}");
            assert_eq!(
                std::fs::read(profile.join("blobs").join(key)).unwrap(),
                data
            );
        }
    }
}

#[test]
fn backup_rejects_content_and_metadata_tamper_before_destination_write() {
    for mutation in ["file", "suffix", "prefix", "bytes"] {
        let profile = unique_dir("backup-tamper");
        let (store, key, data) = make_store_with_external_blob(&profile);
        let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
        match mutation {
            "file" => {
                std::fs::write(profile.join("blobs").join(&key), vec![0x43u8; data.len()]).unwrap();
            }
            "suffix" => {
                conn.execute("UPDATE clip_blobs SET sha256=substr(sha256,1,63)||CASE substr(sha256,64,1) WHEN '0' THEN '1' ELSE '0' END", []).unwrap();
            }
            "prefix" => {
                conn.execute("UPDATE clip_blobs SET sha256=CASE substr(sha256,1,1) WHEN '0' THEN '1' ELSE '0' END||substr(sha256,2)", []).unwrap();
            }
            "bytes" => {
                conn.execute("UPDATE clip_blobs SET bytes=bytes+1", [])
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let destination = unique_dir("backup-destination").join("existing.zip");
        std::fs::write(&destination, b"original-destination").unwrap();
        let err = store.backup_zip(&destination).unwrap_err();
        assert!(
            matches!(err, CoreError::Corrupt { .. }),
            "{mutation}: {err:?}"
        );
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"original-destination"
        );
    }
}

#[test]
fn local_read_and_dedupe_reject_invalid_candidate_metadata() {
    for mutation in [
        "UPDATE clip_payloads SET external=2",
        "UPDATE clip_payloads SET data=X'61'",
        "UPDATE clip_blobs SET bytes=bytes+1",
        "UPDATE clip_blobs SET sha256=CASE substr(sha256,1,1) WHEN '0' THEN '1' ELSE '0' END||substr(sha256,2)",
        "DELETE FROM clip_blobs",
    ] {
        let profile = unique_dir("local-metadata");
        let (store, _key, data) = make_store_with_external_blob(&profile);
        let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
        let id: String = conn.query_row("SELECT id FROM clips", [], |r| r.get(0)).unwrap();
        conn.execute(mutation, []).unwrap();
        let err = store.get_clip(id.parse().unwrap()).unwrap_err();
        assert!(matches!(err, CoreError::Corrupt { .. }), "{mutation}: {err:?}");
        let next = sample_clip(Uuid::now_v7(), "dedupe-candidate", 1_700_000_000_100);
        let outcome = store.insert_clip(&next, &[("application/octet-stream".into(), data)]).unwrap();
        assert_eq!(outcome, clipmonger_core::store::InsertOutcome::Inserted(next.id), "invalid candidate must be skipped: {mutation}");
    }
}

#[test]
fn get_validates_later_candidate_before_first_external_read() {
    let profile = unique_dir("candidate-preflight");
    let store = Store::open(&profile).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "two-candidates", 1_700_000_000_000);
    let data = vec![88u8; 300 * 1024];
    store
        .insert_clip(
            &clip,
            &[
                ("first".into(), data.clone()),
                ("second".into(), data.clone()),
            ],
        )
        .unwrap();
    let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    conn.execute(
        "UPDATE clip_payloads SET external=2 WHERE mime='second'",
        [],
    )
    .unwrap();
    std::fs::remove_file(profile.join("blobs").join(valid_blob_key_for(&data).0)).unwrap();
    let err = store.get_clip(clip.id).unwrap_err();
    assert!(matches!(err, CoreError::Corrupt { .. }));
    assert!(
        err.to_string().contains("invalid external flag"),
        "must validate later metadata before missing first blob: {err:?}"
    );
}

#[test]
fn open_rejects_invalid_payload_types_and_reference_metadata() {
    for mutation in [
        "UPDATE clip_payloads SET external=2",
        "UPDATE clip_payloads SET external='bad'",
        "UPDATE clip_payloads SET data=X'61'",
        "UPDATE clip_payloads SET external=0, blob_key=''",
        "UPDATE clip_blobs SET bytes='bad'",
        "DELETE FROM clip_blobs",
    ] {
        let profile = unique_dir("open-metadata");
        let (store, _key, _data) = make_store_with_external_blob(&profile);
        drop(store);
        let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
        conn.execute(mutation, []).unwrap();
        assert!(
            matches!(
                Store::open(&profile).unwrap_err(),
                CoreError::Corrupt { .. }
            ),
            "{mutation}"
        );
        assert!(
            matches!(
                Store::open_readonly_recovery(&profile).unwrap_err(),
                CoreError::Corrupt { .. }
            ),
            "{mutation}"
        );
    }
}

#[cfg(unix)]
#[test]
fn trusted_profile_and_backup_parent_links_remain_supported() {
    let outer = unique_dir("trusted-anchors");
    let profile = outer.join("profile");
    let destination = outer.join("destination");
    std::fs::create_dir(&profile).unwrap();
    std::fs::create_dir(&destination).unwrap();
    let profile_link = outer.join("profile-link");
    let destination_link = outer.join("destination-link");
    std::os::unix::fs::symlink(&profile, &profile_link).unwrap();
    std::os::unix::fs::symlink(&destination, &destination_link).unwrap();
    let (store, _key, _data) = make_store_with_external_blob(&profile_link);
    store
        .backup_zip(&destination_link.join("backup.zip"))
        .unwrap();
    assert!(destination.join("backup.zip").is_file());
}

#[test]
fn dedupe_skips_external_candidate_with_inline_data() {
    let profile = unique_dir("dedupe-inline-metadata");
    let (store, _key, data) = make_store_with_external_blob(&profile);
    let conn = rusqlite::Connection::open(profile.join("clipstore.sqlite3")).unwrap();
    conn.execute("UPDATE clip_payloads SET data=X'61'", [])
        .unwrap();
    let next = sample_clip(Uuid::now_v7(), "dedupe-inline", 1_700_000_000_100);
    let outcome = store
        .insert_clip(&next, &[("application/octet-stream".into(), data)])
        .unwrap();
    assert_eq!(
        outcome,
        clipmonger_core::store::InsertOutcome::Inserted(next.id)
    );
}

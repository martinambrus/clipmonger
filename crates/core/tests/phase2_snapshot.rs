use clipmonger_core::model::{Clip, Rev};
use clipmonger_core::store::Store;
use clipmonger_core::CoreError;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use uuid::Uuid;
static COUNTER: AtomicU64 = AtomicU64::new(0);
fn unique_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("cm-phase2-{tag}-{}-{n}", std::process::id()));
    fs::create_dir_all(&d).unwrap();
    d
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
fn make_external_profile() -> (PathBuf, PathBuf, Uuid) {
    let dir = unique_dir("src");
    let store = Store::open(&dir).unwrap();
    let id = Uuid::now_v7();
    let clip = sample_clip(id, "external-clip", 1_700_000_010_000);
    let payload = vec![(
        "application/octet-stream".to_string(),
        vec![7u8; 256 * 1024 + 64],
    )];
    store.insert_clip(&clip, &payload).unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(dir.join("clipstore.sqlite3")).unwrap();
    conn.execute("UPDATE clips SET removed=1", []).unwrap();
    drop(conn);
    let store = Store::open(&dir).unwrap();
    let zip = dir.join("valid.zip");
    store.backup_zip(&zip).unwrap();
    assert!(zip.is_file());
    (dir, zip, id)
}
fn read_zip_entries(zip_path: &Path) -> Vec<(String, Vec<u8>)> {
    let file = fs::File::open(zip_path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let name = entry.name().unwrap().to_string();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
        entries.push((name, bytes));
    }
    entries
}
fn write_zip(entries: Vec<(String, Vec<u8>)>, dest: &Path) {
    let f = fs::File::create(dest).unwrap();
    let mut w = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o600);
    for (name, data) in entries {
        w.start_file(name, opts).unwrap();
        w.write_all(&data).unwrap();
    }
    w.finish().unwrap();
}
fn rewrite_manifest(entries: &mut [(String, Vec<u8>)], new_manifest: &str) {
    for (n, d) in entries.iter_mut() {
        if n == "manifest.toml" {
            *d = new_manifest.as_bytes().to_vec();
        }
    }
}

fn digest(bytes: &[u8]) -> String {
    let mut out = String::new();
    for byte in Sha256::digest(bytes) {
        write!(&mut out, "{byte:02x}").unwrap();
    }
    out
}

fn manifest(entries: &[(String, Vec<u8>)]) -> toml::Value {
    toml::from_str(
        std::str::from_utf8(
            &entries
                .iter()
                .find(|(n, _)| n == "manifest.toml")
                .unwrap()
                .1,
        )
        .unwrap(),
    )
    .unwrap()
}

fn change_manifest(entries: &mut [(String, Vec<u8>)], change: impl FnOnce(&mut toml::Value)) {
    let mut man = manifest(entries);
    change(&mut man);
    rewrite_manifest(entries, &toml::to_string(&man).unwrap());
}

fn profile_files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn visit(root: &Path, dir: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                ));
            }
        }
    }
    let mut files = Vec::new();
    visit(root, root, &mut files);
    files.sort();
    files
}

fn assert_invalid(entries: Vec<(String, Vec<u8>)>, expected: &str) {
    let archive = unique_dir("invalid").join("invalid.zip");
    write_zip(entries, &archive);
    let dst = unique_dir("destination");
    let store = Store::open(&dst).unwrap();
    let clip = sample_clip(Uuid::now_v7(), "distinct destination", 1_700_000_020_000);
    store
        .insert_clip(
            &clip,
            &[(
                "application/octet-stream".to_string(),
                vec![9; 256 * 1024 + 64],
            )],
        )
        .unwrap();
    drop(store);
    fs::write(
        dst.join("config.toml"),
        b"distinct destination config bytes",
    )
    .unwrap();
    let before = profile_files(&dst);
    let err = Store::restore_zip(&archive, &dst).unwrap_err();
    assert!(
        format!("{err:?}").contains(expected),
        "expected {expected:?}, got {err:?}"
    );
    assert_eq!(
        profile_files(&dst),
        before,
        "invalid restore changed destination"
    );
}

fn external_entries() -> Vec<(String, Vec<u8>)> {
    let (_, zip, _) = make_external_profile();
    read_zip_entries(&zip)
}

#[test]
fn backup_valid_archive_produces_matching_counts() {
    let entries = external_entries();
    let man = manifest(&entries);
    let db_bytes = &entries
        .iter()
        .find(|(n, _)| n == "clipstore.sqlite3")
        .unwrap()
        .1;
    assert_eq!(
        man["db_bytes"].as_integer().unwrap(),
        i64::try_from(db_bytes.len()).unwrap()
    );
    assert_eq!(man["db_sha256"].as_str().unwrap(), digest(db_bytes));
    let db = unique_dir("snapshot").join("snapshot.sqlite3");
    fs::write(&db, db_bytes).unwrap();
    let conn = rusqlite::Connection::open(db).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
        .unwrap();
    assert_eq!(man["clips"].as_integer().unwrap(), count);
    let removed: i64 = conn
        .query_row("SELECT COUNT(*) FROM clips WHERE removed=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(removed, 1);
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT blob_key FROM clip_payloads WHERE external=1 AND blob_key IS NOT NULL",
        )
        .unwrap();
    let referenced: HashSet<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let manifest_blobs = man["blobs"].as_array().unwrap();
    let manifest_keys: HashSet<String> = manifest_blobs
        .iter()
        .map(|v| v["key"].as_str().unwrap().to_string())
        .collect();
    let zip_keys: HashSet<String> = entries
        .iter()
        .filter_map(|(n, _)| n.strip_prefix("blobs/").map(str::to_string))
        .collect();
    assert_eq!(manifest_keys.len(), manifest_blobs.len());
    assert_eq!(referenced, manifest_keys);
    assert_eq!(referenced, zip_keys);
    for blob in manifest_blobs {
        let key = blob["key"].as_str().unwrap();
        let (sha, bytes): (String, i64) = conn
            .query_row(
                "SELECT sha256,bytes FROM clip_blobs WHERE blob_key=?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let data = &entries
            .iter()
            .find(|(n, _)| n == &format!("blobs/{key}"))
            .unwrap()
            .1;
        assert_eq!(bytes, i64::try_from(data.len()).unwrap());
        assert_eq!(sha, digest(data));
        assert_eq!(blob["bytes"].as_integer().unwrap(), bytes);
        assert_eq!(blob["sha256"].as_str().unwrap(), sha);
    }
}

#[test]
fn restore_rejects_missing_blob_when_manifest_omits_entry() {
    let mut entries = external_entries();
    entries.retain(|(n, _)| !n.starts_with("blobs/"));
    change_manifest(&mut entries, |m| {
        m["blobs"] = toml::Value::Array(Vec::new());
    });
    assert_invalid(entries, "blob reference set mismatch");
}

#[test]
fn restore_rejects_manifest_missing_entry_with_file_present() {
    let mut entries = external_entries();
    change_manifest(&mut entries, |m| {
        m["blobs"] = toml::Value::Array(Vec::new());
    });
    assert_invalid(entries, "blob reference set mismatch");
}

#[test]
fn restore_rejects_missing_manifest_blobs() {
    let mut entries = external_entries();
    change_manifest(&mut entries, |m| {
        m.as_table_mut().unwrap().remove("blobs");
    });
    assert_invalid(entries, "blob reference set mismatch");
}

#[test]
fn restore_rejects_missing_manifest_blob_file() {
    let mut entries = external_entries();
    entries.retain(|(n, _)| !n.starts_with("blobs/"));
    assert_invalid(entries, "missing external blob");
}

#[test]
fn restore_rejects_duplicate_manifest_entry() {
    let mut entries = external_entries();
    change_manifest(&mut entries, |m| {
        let blobs = m["blobs"].as_array_mut().unwrap();
        blobs.push(blobs[0].clone());
    });
    assert_invalid(entries, "duplicate blob key");
}

fn reject_db_bytes(value: toml::Value) {
    let mut entries = external_entries();
    change_manifest(&mut entries, |m| m["db_bytes"] = value);
    assert_invalid(entries, "db_bytes");
}

#[test]
fn restore_rejects_wrong_db_bytes() {
    reject_db_bytes(toml::Value::Integer(1));
}
#[test]
fn restore_rejects_wrong_db_bytes_type() {
    reject_db_bytes(toml::Value::String("bad".to_string()));
}
#[test]
fn restore_rejects_negative_db_bytes() {
    reject_db_bytes(toml::Value::Integer(-1));
}

#[test]
fn backup_preserves_existing_dest_zip_when_src_missing_blob() {
    let (src, dest, _) = make_external_profile();
    let before = fs::read(&dest).unwrap();
    let key = fs::read_dir(src.join("blobs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::remove_file(key).unwrap();
    let err = Store::open(&src).unwrap().backup_zip(&dest).unwrap_err();
    assert!(
        matches!(err, CoreError::Corrupt { .. } | CoreError::Io(_)),
        "{err:?}"
    );
    assert_eq!(before, fs::read(dest).unwrap());
}

#[test]
fn restore_rejects_extra_manifest_entry() {
    let mut entries = external_entries();
    // This fully valid, unreferenced entry must fail the reference-set check.
    let extra: toml::Value = toml::from_str(
        r#"key="409a7f83ac6b31dc8c77e3ec18038f20-123"
bytes=123
sha256="409a7f83ac6b31dc8c77e3ec18038f209bd2f545e0f4177c2e2381aa4e067b49""#,
    )
    .unwrap();
    change_manifest(&mut entries, |m| {
        m["blobs"].as_array_mut().unwrap().push(extra);
    });
    entries.push((
        "blobs/409a7f83ac6b31dc8c77e3ec18038f20-123".to_string(),
        vec![0; 123],
    ));
    assert_invalid(entries, "blob reference set mismatch");
}

#[test]
fn restore_checks_db_digest_when_manifest_sha_omitted() {
    let mut entries = external_entries();
    change_manifest(&mut entries, |m| {
        m["blobs"][0].as_table_mut().unwrap().remove("sha256");
    });
    entries
        .iter_mut()
        .find(|(n, _)| n.starts_with("blobs/"))
        .unwrap()
        .1[0] ^= 1;
    assert_invalid(entries, "external blob failed verification");
}

#[test]
fn restore_checks_changed_db_digest_when_manifest_sha_omitted() {
    let mut entries = external_entries();
    let db = unique_dir("mutated-db").join("db.sqlite3");
    let index = entries
        .iter()
        .position(|(n, _)| n == "clipstore.sqlite3")
        .unwrap();
    fs::write(&db, &entries[index].1).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    let mut sha: String = conn
        .query_row("SELECT sha256 FROM clip_blobs LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let last = sha.pop().unwrap();
    sha.push(if last == '0' { '1' } else { '0' });
    conn.execute("UPDATE clip_blobs SET sha256=?1", [&sha])
        .unwrap();
    drop(conn);
    let bytes = fs::read(db).unwrap();
    let db_digest = digest(&bytes);
    change_manifest(&mut entries, |m| {
        m["db_sha256"] = toml::Value::String(db_digest);
        m["db_bytes"] = toml::Value::Integer(i64::try_from(bytes.len()).unwrap());
        m["blobs"][0].as_table_mut().unwrap().remove("sha256");
    });
    entries[index].1 = bytes;
    assert_invalid(entries, "external blob failed verification");
}

#[test]
fn restore_rejects_malformed_optional_types() {
    for field in ["bytes", "sha256"] {
        let mut entries = external_entries();
        change_manifest(&mut entries, |m| {
            m["blobs"][0][field] = toml::Value::Boolean(true);
        });
        assert_invalid(entries, &format!("blob {field} not"));
    }
    let mut entries = external_entries();
    change_manifest(&mut entries, |m| m["db_sha256"] = toml::Value::Integer(1));
    assert_invalid(entries, "db_sha256 not a string");
}

#[test]
fn restore_accepts_legacy_optional_omissions() {
    let mut entries = external_entries();
    change_manifest(&mut entries, |m| {
        let table = m.as_table_mut().unwrap();
        table.remove("db_bytes");
        table.remove("db_sha256");
        let blob = m["blobs"][0].as_table_mut().unwrap();
        blob.remove("bytes");
        blob.remove("sha256");
    });
    let zip = unique_dir("legacy").join("legacy.zip");
    write_zip(entries, &zip);
    let dst = unique_dir("legacy-destination");
    Store::restore_zip(&zip, &dst).unwrap();
    assert_eq!(fs::read_dir(dst.join("blobs")).unwrap().count(), 1);
}

#[test]
fn restore_accepts_no_external_references_without_blob_array() {
    let src = unique_dir("empty-source");
    let zip = src.join("empty.zip");
    Store::open(&src).unwrap().backup_zip(&zip).unwrap();
    let mut entries = read_zip_entries(&zip);
    change_manifest(&mut entries, |m| {
        m.as_table_mut().unwrap().remove("blobs");
    });
    write_zip(entries, &zip);
    Store::restore_zip(&zip, &unique_dir("empty-destination")).unwrap();
}

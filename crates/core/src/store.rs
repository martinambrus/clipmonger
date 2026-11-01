//! SQLite clip store: schema, open path, blobs, retention, backup/restore.
//!
//! Implements seg-03 S3/S4/S6 under driver freeze D-F1..F5, D-F9, D-F10.

use std::collections::HashSet;
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::error::{BackendKind, CoreError};
use crate::hash::content_hash;
use crate::model::{Clip, PermanentClip, PermanentKind, Rev};
use crate::policy::{self, DedupeCandidate, Limits};

/// Schema version this code reads and writes (D-F1).
const SCHEMA_VERSION: i64 = 1;
/// Per-payload inline limit in bytes (D-F3, KiB = 1024).
const INLINE_LIMIT: usize = 256 * 1024;
/// Dedupe scan window (matches default `history.dedupe_window`).
const DEDUPE_WINDOW: usize = 100;
/// Largest single ZIP entry accepted by restore.
const RESTORE_MAX_ENTRY: u64 = 1024 * 1024 * 1024;
/// Largest total ZIP payload accepted by restore.
const RESTORE_MAX_TOTAL: u64 = 4 * 1024 * 1024 * 1024;
/// Maximum raw `config.toml` bytes accepted for backup or restore.
/// Documented reasonable bound: 1 MiB (typical config is < 10 KiB but allows
/// extensive comments and future unknown fields; prevents unbounded allocation).
const CONFIG_MAX_BYTES: u64 = 1024 * 1024;
/// DB file name inside a profile directory.
const DB_NAME: &str = "clipstore.sqlite3";
/// DDL for tables and indexes (the FTS table is created separately).
const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS clips(
  id TEXT PRIMARY KEY, created_ms INTEGER NOT NULL, source_app TEXT,
  source_os TEXT NOT NULL, pinned INTEGER NOT NULL DEFAULT 0,
  removed INTEGER NOT NULL DEFAULT 0, preview TEXT NOT NULL DEFAULT '',
  search_text TEXT NOT NULL DEFAULT '', bytes_total INTEGER NOT NULL DEFAULT 0,
  content_hash TEXT NOT NULL, rev_ms INTEGER NOT NULL, rev_dev TEXT NOT NULL,
  rev_ctr INTEGER NOT NULL DEFAULT 0, last_used_ms INTEGER NOT NULL,
  origin_dev TEXT NOT NULL DEFAULT '', origin TEXT NOT NULL DEFAULT '');
CREATE INDEX IF NOT EXISTS idx_clips_created ON clips(created_ms DESC);
CREATE INDEX IF NOT EXISTS idx_clips_content ON clips(content_hash);
CREATE TABLE IF NOT EXISTS clip_payloads(
  clip_id TEXT NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
  mime TEXT NOT NULL, external INTEGER NOT NULL DEFAULT 0,
  data BLOB, blob_key TEXT, native_id TEXT,
  PRIMARY KEY(clip_id, mime));
CREATE TABLE IF NOT EXISTS clip_blobs(
  blob_key TEXT PRIMARY KEY, sha256 TEXT NOT NULL, bytes INTEGER NOT NULL,
  codec TEXT NOT NULL DEFAULT 'raw', created_ms INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS perm_groups(name TEXT PRIMARY KEY, system INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS permanents(
  id TEXT PRIMARY KEY, grp TEXT NOT NULL REFERENCES perm_groups(name),
  name TEXT NOT NULL, hotkey TEXT, kind TEXT NOT NULL,
  body TEXT NOT NULL, js TEXT,
  rev_ms INTEGER NOT NULL, rev_dev TEXT NOT NULL, rev_ctr INTEGER NOT NULL DEFAULT 0);
";

/// Counter for unique scratch directory names.
static TMP_CTR: AtomicU64 = AtomicU64::new(0);

/// One clip's payloads: MIME plus raw bytes.
pub type ClipPayloads = Vec<(String, Vec<u8>)>;

/// Payload row joined with its blob metadata for verification.
type VerifiedPayload = (
    String,
    i64,
    Option<Vec<u8>>,
    Option<String>,
    Option<String>,
    Option<i64>,
);

/// Outcome of [`Store::insert_clip`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertOutcome {
    /// New row stored.
    Inserted(Uuid),
    /// Duplicate content: the existing row moved to top.
    MovedToTop(Uuid),
    /// Metadata-only stub (no payloads stored).
    Stubbed(Uuid),
}

/// Result of [`Store::gc_blobs`].
#[derive(Debug, Clone)]
pub struct GcReport {
    /// Orphan files moved to quarantine.
    pub orphaned_files: u64,
    /// Blob rows with no live payload reference (kept, reported).
    pub dangling_blobs: u64,
    /// Directory orphans were moved to.
    pub quarantine_dir: PathBuf,
}

/// One named permanent group (row of `perm_groups` + live clip count).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermGroup {
    /// Group name (primary key).
    pub name: String,
    /// System groups are hidden from popup tabs.
    pub system: bool,
    /// Number of permanents in the group.
    pub count: u64,
}

/// Columns selected for a permanent row, in order.
const PERM_COLS: &str = "id, grp, name, hotkey, kind, body, js, rev_ms, rev_dev, rev_ctr";

/// Raw permanent row tuple over [`PERM_COLS`].
type PermRow = (
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    Option<String>,
    i64,
    String,
    i64,
);

/// Convert one raw permanent row, rejecting unknown kind text as corrupt.
fn perm_from_row(row: PermRow) -> Result<PermanentClip, CoreError> {
    let (id, group, name, hotkey, kind, body, js, wall_ms, dev, ctr) = row;
    let kind = match kind.as_str() {
        "Paste" => PermanentKind::Paste,
        "Macro" => PermanentKind::Macro,
        "System" => PermanentKind::System,
        other => {
            return Err(CoreError::Corrupt {
                message: format!("unknown permanent kind {other}"),
            })
        }
    };
    let id = id.parse::<Uuid>().map_err(|e| CoreError::Corrupt {
        message: format!("bad permanent id: {e}"),
    })?;
    Ok(PermanentClip {
        id,
        group,
        name,
        hotkey,
        kind,
        body,
        js,
        rev: Rev {
            wall_ms,
            dev,
            ctr: u64::try_from(ctr.max(0)).unwrap_or(u64::MAX),
        },
    })
}

/// Validate and trim permanent group/name/body; body stays verbatim.
fn validate_perm_fields(
    group: &str,
    name: &str,
    body: &str,
) -> Result<(String, String), CoreError> {
    let group = group.trim();
    let name = name.trim();
    if group.is_empty() || group.chars().any(char::is_control) {
        return Err(CoreError::Config {
            message: "permanent group must be non-empty without control characters".to_string(),
        });
    }
    if name.is_empty() {
        return Err(CoreError::Config {
            message: "permanent name must be non-empty".to_string(),
        });
    }
    if body.is_empty() {
        return Err(CoreError::Config {
            message: "permanent body must be non-empty".to_string(),
        });
    }
    Ok((group.to_string(), name.to_string()))
}

/// Normalise an optional hotkey: trimmed, empty becomes `None`.
fn norm_hotkey(hotkey: Option<&str>) -> Option<String> {
    hotkey
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(ToString::to_string)
}

// These operation failures exist only in unit-test builds, never in the runtime API.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreFault {
    OldMove(usize),
    NewMove(usize),
    NewOpen,
    RollbackRemove,
    RollbackRename,
    OldOpen,
    Close,
}
#[cfg(test)]
thread_local! {
    static RESTORE_FAULTS: std::cell::RefCell<Vec<RestoreFault>> = const { std::cell::RefCell::new(Vec::new()) };
}
#[cfg(test)]
fn check_restore_fault(point: RestoreFault) -> Result<(), CoreError> {
    RESTORE_FAULTS.with(|faults| {
        let mut faults = faults.borrow_mut();
        if let Some(index) = faults.iter().position(|fault| *fault == point) {
            faults.remove(index);
            return Err(CoreError::Io(std::io::Error::other(format!(
                "injected {point:?}"
            ))));
        }
        Ok(())
    })
}
macro_rules! restore_point {
    ($point:expr) => {{
        #[cfg(test)]
        {
            check_restore_fault($point)
        }
        #[cfg(not(test))]
        {
            Ok::<(), CoreError>(())
        }
    }};
}

/// SQLite-backed clip store.
#[derive(Debug)]
pub struct Store {
    /// Path to `blobs/`.
    blobs: PathBuf,
    /// Path to DB file.
    db_path: PathBuf,
    /// Connection guarded for shared `&self` use across threads.
    conn: Mutex<Connection>,
    /// True when FTS5 is available (probed at open).
    fts: bool,
    /// True for recovery handles: every write is refused.
    read_only: bool,
}

/// Wrap a `rusqlite` failure as a backend error.
fn db_err(e: &rusqlite::Error) -> CoreError {
    CoreError::Backend {
        backend: "sqlite".to_string(),
        kind: BackendKind::Internal,
        message: e.to_string(),
    }
}

/// Wrap a `zip` failure as a backend error.
fn zip_err(e: &zip::result::ZipError) -> CoreError {
    CoreError::Backend {
        backend: "zip".to_string(),
        kind: BackendKind::Internal,
        message: e.to_string(),
    }
}

/// Refusal returned by every write on a recovery handle.
fn readonly_err() -> CoreError {
    CoreError::Unsupported {
        message: "store is open read-only for recovery".to_string(),
    }
}

/// Current wall-clock time in unix millis.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .map_or(i64::MAX, |m| i64::try_from(m).unwrap_or(i64::MAX))
}

/// Machine label for `meta.machine` (disambiguation only).
fn machine_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Create a directory with user-only permissions on Unix.
fn mkdir(dir: &Path) -> Result<(), CoreError> {
    fs::create_dir_all(dir).map_err(CoreError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = fs::metadata(dir).map_err(CoreError::Io)?.permissions();
        perms.set_mode(0o700);
        fs::set_permissions(dir, perms).map_err(CoreError::Io)?;
    }
    Ok(())
}

fn mkdir_if_missing(dir: &Path) -> Result<(), CoreError> {
    if dir.exists() {
        // The selected anchor is trusted; pin its canonical target in preparation.
        let meta = fs::metadata(dir).map_err(CoreError::Io)?;
        if !meta.is_dir() {
            return Err(CoreError::Corrupt {
                message: "profile exists as file".to_string(),
            });
        }
        return Ok(());
    }
    fs::create_dir_all(dir).map_err(CoreError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = fs::metadata(dir).map_err(CoreError::Io)?.permissions();
        perms.set_mode(0o700);
        fs::set_permissions(dir, perms).map_err(CoreError::Io)?;
    }
    Ok(())
}

fn create_stage_root(profile: &Path) -> Result<PathBuf, CoreError> {
    mkdir_if_missing(profile)?;
    for _ in 0..16 {
        let name = format!(
            ".restore-{}-{}",
            std::process::id(),
            Uuid::now_v7().as_simple()
        );
        let dir = profile.join(&name);
        #[cfg(unix)]
        let result = {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new().mode(0o700).create(&dir)
        };
        #[cfg(not(unix))]
        let result = fs::create_dir(&dir);
        match result {
            Ok(()) => {
                let incoming = dir.join("incoming");
                let old = dir.join("old");
                let children = (|| -> Result<(), CoreError> {
                    fs::create_dir(&incoming).map_err(CoreError::Io)?;
                    fs::create_dir(&old).map_err(CoreError::Io)?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt as _;
                        for p in [&incoming, &old] {
                            let mut perms = fs::metadata(p).map_err(CoreError::Io)?.permissions();
                            perms.set_mode(0o700);
                            fs::set_permissions(p, perms).map_err(CoreError::Io)?;
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = children {
                    let _ = fs::remove_dir_all(&dir);
                    return Err(error);
                }
                return Ok(dir);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(CoreError::Io(e)),
        }
    }
    Err(CoreError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "too many restore collisions",
    )))
}

fn check_managed_not_symlink(path: &Path) -> Result<(), CoreError> {
    match fs::symlink_metadata(path) {
        Ok(m) => {
            if m.file_type().is_symlink() {
                return Err(CoreError::Unsupported {
                    message: format!("managed component is symlink: {}", path.display()),
                });
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                if (m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                    return Err(CoreError::Unsupported {
                        message: format!("managed component is reparse point: {}", path.display()),
                    });
                }
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CoreError::Io(e)),
    }
}

fn same_file(left: &Path, right: &Path) -> bool {
    if let (Ok(left), Ok(right)) = (left.canonicalize(), right.canonicalize()) {
        if left == right {
            return true;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if let (Ok(left), Ok(right)) = (fs::metadata(left), fs::metadata(right)) {
            return left.dev() == right.dev() && left.ino() == right.ino();
        }
    }
    false
}

fn is_ancestor_managed(src: &Path, profile: &Path) -> Result<bool, CoreError> {
    let source = src.canonicalize().map_err(CoreError::Io)?;
    for name in [
        DB_NAME,
        "clipstore.sqlite3-wal",
        "clipstore.sqlite3-shm",
        "blobs",
    ] {
        let managed = profile.join(name);
        if same_file(src, &managed) || source.starts_with(&managed) {
            return Ok(true);
        }
        if name == "blobs" && fs::symlink_metadata(&managed).is_ok_and(|m| m.is_dir()) {
            // Hard links cannot be resolved lexically. Inspect the managed blob leaves.
            for entry in fs::read_dir(&managed).map_err(CoreError::Io)? {
                if same_file(src, &entry.map_err(CoreError::Io)?.path()) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn check_restore_components(profile: &Path) -> Result<(), CoreError> {
    #[cfg(unix)]
    let anchor_device = {
        use std::os::unix::fs::MetadataExt as _;
        fs::metadata(profile).map_err(CoreError::Io)?.dev()
    };
    for name in [
        DB_NAME,
        "clipstore.sqlite3-wal",
        "clipstore.sqlite3-shm",
        "blobs",
    ] {
        let path = profile.join(name);
        check_managed_not_symlink(&path)?;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(CoreError::Io(error)),
        };
        if (name == "blobs" && !metadata.is_dir()) || (name != "blobs" && !metadata.is_file()) {
            return Err(CoreError::Unsupported {
                message: format!("wrong managed component type: {}", path.display()),
            });
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if metadata.dev() != anchor_device {
                return Err(CoreError::Unsupported {
                    message: format!("managed component crosses filesystem: {}", path.display()),
                });
            }
        }
    }
    Ok(())
}

fn ensure_profile_container(profile: &Path) -> Result<(), CoreError> {
    mkdir_if_missing(profile)
}

fn create_private_dir(prefix: &str) -> Result<PathBuf, CoreError> {
    for _ in 0..16 {
        let name = format!(
            "{}-{}-{}-{}",
            prefix,
            std::process::id(),
            TMP_CTR.fetch_add(1, Ordering::SeqCst),
            Uuid::now_v7().as_simple()
        );
        let dir = std::env::temp_dir().join(name);
        #[cfg(unix)]
        let result = {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new().mode(0o700).create(&dir)
        };
        #[cfg(not(unix))]
        let result = fs::create_dir(&dir);
        match result {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(CoreError::Io(e)),
        }
    }
    Err(CoreError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "too many temp collisions",
    )))
}

/// Best-effort directory fsync after renames (Unix only).
fn fsync_dir(dir: &Path) {
    #[cfg(unix)]
    {
        if let Ok(f) = fs::File::open(dir) {
            let _ = f.sync_all();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

/// Lowercase hex of bytes.
fn hex_of(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let hi = HEX.get((b >> 4) as usize).copied().unwrap_or(b'0');
        let lo = HEX.get((b & 15) as usize).copied().unwrap_or(b'0');
        out.push(hi as char);
        out.push(lo as char);
    }
    out
}

fn parse_blob_key(key: &str) -> Option<(String, u64)> {
    let bytes = key.as_bytes();
    if bytes.len() < 34 || bytes.len() > 52 {
        return None;
    }
    if bytes[32] != b'-' {
        return None;
    }
    let prefix = &key[..32];
    let size_str = &key[33..];
    if size_str.is_empty() || size_str.len() > 19 {
        return None;
    }
    for &b in prefix.as_bytes() {
        if !matches!(b, b'0'..=b'9' | b'a'..=b'f') {
            return None;
        }
    }
    if size_str == "0" {
    } else {
        if size_str.as_bytes()[0] == b'0' {
            return None;
        }
        for &b in size_str.as_bytes() {
            if !b.is_ascii_digit() {
                return None;
            }
        }
    }
    for &b in size_str.as_bytes() {
        if !b.is_ascii_digit() {
            return None;
        }
    }
    let size: u64 = size_str.parse().ok()?;
    if size > i64::MAX as u64 {
        return None;
    }
    if size_str != size.to_string() {
        return None;
    }
    Some((prefix.to_string(), size))
}

fn is_canonical_hex(s: &str, len: usize) -> bool {
    if s.len() != len {
        return false;
    }
    s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn validate_blob_metadata(key: &str, sha256: &str, bytes: i64) -> Result<(), CoreError> {
    let (prefix, size) = parse_blob_key(key).ok_or_else(|| CoreError::Corrupt {
        message: "invalid blob key".to_string(),
    })?;
    if !is_canonical_hex(sha256, 64) {
        return Err(CoreError::Corrupt {
            message: "invalid blob sha256".to_string(),
        });
    }
    if !sha256.starts_with(&prefix) {
        return Err(CoreError::Corrupt {
            message: "blob sha prefix mismatch".to_string(),
        });
    }
    if bytes < 0 {
        return Err(CoreError::Corrupt {
            message: "invalid blob bytes".to_string(),
        });
    }
    if u64::try_from(bytes).ok() != Some(size) {
        return Err(CoreError::Corrupt {
            message: "blob bytes mismatch".to_string(),
        });
    }
    Ok(())
}

fn config_bytes_for_backup(profile: &Path) -> Result<Option<(Vec<u8>, u64, String)>, CoreError> {
    let path = profile.join("config.toml");
    match fs::symlink_metadata(&path) {
        Ok(m) => {
            if m.file_type().is_symlink() {
                return Err(CoreError::Unsupported {
                    message: format!("config is symlink: {}", path.display()),
                });
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt as _;
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                if (m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                    return Err(CoreError::Unsupported {
                        message: format!("config is reparse point: {}", path.display()),
                    });
                }
            }
            if !m.is_file() {
                return Err(CoreError::Unsupported {
                    message: format!("config wrong type: {}", path.display()),
                });
            }
            if m.len() > CONFIG_MAX_BYTES {
                return Err(CoreError::Config {
                    message: "config exceeds size cap".to_string(),
                });
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(CoreError::Io(e)),
    }
    check_managed_not_symlink(&path)?;
    let file = fs::File::open(&path).map_err(CoreError::Io)?;
    let mut buf = Vec::new();
    let n = file
        .take(CONFIG_MAX_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(CoreError::Io)?;
    if n as u64 > CONFIG_MAX_BYTES {
        return Err(CoreError::Config {
            message: "config exceeds size cap".to_string(),
        });
    }
    let text = String::from_utf8(buf.clone()).map_err(|_| CoreError::Config {
        message: "config not utf8".to_string(),
    })?;
    toml::from_str::<crate::Config>(&text).map_err(|e| CoreError::Config {
        message: e.to_string(),
    })?;
    let sha = hex_of(&Sha256::digest(&buf));
    let len = buf.len() as u64;
    Ok(Some((buf, len, sha)))
}

fn validate_config_in_profile(tmp: &Path, man: &toml::Value) -> Result<(), CoreError> {
    let bad = |msg: &str| CoreError::Config {
        message: msg.to_string(),
    };
    let config_path = tmp.join("config.toml");
    let file_meta = fs::symlink_metadata(&config_path);
    let file_exists = matches!(&file_meta, Ok(m) if m.is_file());
    let is_symlink = matches!(&file_meta, Ok(m) if m.file_type().is_symlink());
    if is_symlink {
        return Err(bad("config symlink rejected"));
    }
    if let Ok(m) = &file_meta {
        if m.is_dir() {
            return Err(bad("config wrong type"));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
            if (m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                return Err(bad("config reparse point rejected"));
            }
        }
        if m.len() > CONFIG_MAX_BYTES {
            return Err(bad("config exceeds size cap"));
        }
    }
    let manifest_bytes = man.get("config_bytes");
    let manifest_sha = man.get("config_sha256");
    if let (false, true) = (
        file_exists,
        manifest_bytes.is_some() || manifest_sha.is_some(),
    ) {
        return Err(bad("config metadata without file"));
    }
    if let Some(v) = manifest_bytes {
        let n = v
            .as_integer()
            .ok_or_else(|| bad("config_bytes not an integer"))?;
        if n < 0 {
            return Err(bad("config_bytes not a nonnegative integer"));
        }
        let _ = u64::try_from(n).map_err(|_| bad("config_bytes not a nonnegative integer"))?;
    }
    if let Some(v) = manifest_sha {
        let s = v
            .as_str()
            .ok_or_else(|| bad("config_sha256 not a string"))?;
        if !is_canonical_hex(s, 64) {
            return Err(bad("config sha256 invalid"));
        }
    }
    if file_exists {
        let file = fs::File::open(&config_path).map_err(CoreError::Io)?;
        let mut buf = Vec::new();
        let n = file
            .take(CONFIG_MAX_BYTES + 1)
            .read_to_end(&mut buf)
            .map_err(CoreError::Io)?;
        if n as u64 > CONFIG_MAX_BYTES {
            return Err(bad("config exceeds size cap"));
        }
        let text = String::from_utf8(buf.clone()).map_err(|_| bad("config not utf8"))?;
        toml::from_str::<crate::Config>(&text).map_err(|e| bad(&format!("config invalid: {e}")))?;
        if let Some(v) = manifest_bytes {
            let expected = u64::try_from(v.as_integer().unwrap()).unwrap();
            if expected != buf.len() as u64 {
                return Err(bad("config size mismatch"));
            }
        }
        if let Some(v) = manifest_sha {
            let expected = v.as_str().unwrap();
            if hex_of(&Sha256::digest(&buf)) != expected {
                return Err(bad("config checksum mismatch"));
            }
        }
    }
    Ok(())
}

fn blobs_root_ok(blobs: &Path) -> Result<(), CoreError> {
    match fs::symlink_metadata(blobs) {
        Ok(m) => {
            if m.file_type().is_symlink() {
                return Err(CoreError::Corrupt {
                    message: "blobs root is symlink".to_string(),
                });
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                if (m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                    return Err(CoreError::Corrupt {
                        message: "blobs root is reparse point".to_string(),
                    });
                }
            }
            if !m.is_dir() {
                return Err(CoreError::Corrupt {
                    message: "blobs root not a directory".to_string(),
                });
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(CoreError::Io(e)),
    }
    Ok(())
}

fn safe_blob_path(blobs: &Path, key: &str) -> Result<PathBuf, CoreError> {
    let (prefix, size) = parse_blob_key(key).ok_or_else(|| CoreError::Corrupt {
        message: "invalid blob key".to_string(),
    })?;
    let _ = prefix;
    let _ = size;
    blobs_root_ok(blobs)?;
    let path = blobs.join(key);
    match fs::symlink_metadata(&path) {
        Ok(m) => {
            if m.file_type().is_symlink() {
                return Err(CoreError::Corrupt {
                    message: "blob path is symlink".to_string(),
                });
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                if (m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                    return Err(CoreError::Corrupt {
                        message: "blob reparse point".to_string(),
                    });
                }
            }
            if !m.is_file() {
                return Err(CoreError::Corrupt {
                    message: "blob not a regular file".to_string(),
                });
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(CoreError::Io(e)),
    }
    Ok(path)
}

fn safe_blob_read(blobs: &Path, key: &str, expect_sha: &str) -> Result<Vec<u8>, CoreError> {
    let (prefix, size) = parse_blob_key(key).ok_or_else(|| CoreError::Corrupt {
        message: "invalid blob key".to_string(),
    })?;
    let _ = prefix;
    let path = safe_blob_path(blobs, key)?;
    let meta = fs::symlink_metadata(&path).map_err(|_| CoreError::Corrupt {
        message: "missing external blob".to_string(),
    })?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(CoreError::Corrupt {
            message: "blob not a regular file".to_string(),
        });
    }
    let file_len = meta.len();
    if file_len != size {
        return Err(CoreError::Corrupt {
            message: "external blob size mismatch".to_string(),
        });
    }
    let file = fs::File::open(&path).map_err(|_| CoreError::Corrupt {
        message: "missing external blob".to_string(),
    })?;
    let mut buf = Vec::new();
    let n = file
        .take(size + 1)
        .read_to_end(&mut buf)
        .map_err(CoreError::Io)?;
    if n as u64 != size {
        return Err(CoreError::Corrupt {
            message: "external blob size mismatch".to_string(),
        });
    }
    let mut h = Sha256::new();
    h.update(&buf);
    if hex_of(&h.finalize()) != expect_sha {
        return Err(CoreError::Corrupt {
            message: "external blob failed verification".to_string(),
        });
    }
    Ok(buf)
}

fn validate_db_keys(conn: &Connection) -> Result<(), CoreError> {
    let mut stmt = conn
        .prepare("SELECT blob_key, sha256, bytes FROM clip_blobs")
        .map_err(|e| db_err(&e))?;
    let rows: Vec<(String, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| db_err(&e))?
        .collect::<Result<_, _>>()
        .map_err(|_| CoreError::Corrupt {
            message: "invalid blob metadata types".to_string(),
        })?;
    drop(stmt);
    let mut blob_keys = HashSet::new();
    for (key, sha, bytes) in &rows {
        validate_blob_metadata(key, sha, *bytes)?;
        blob_keys.insert(key.clone());
    }
    let mut stmt = conn
        .prepare("SELECT external, blob_key, (data IS NOT NULL) FROM clip_payloads")
        .map_err(|e| db_err(&e))?;
    let payload_rows: Vec<(i64, Option<String>, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| db_err(&e))?
        .collect::<Result<_, _>>()
        .map_err(|_| CoreError::Corrupt {
            message: "invalid payload types".to_string(),
        })?;
    drop(stmt);
    for (ext, key_opt, has_data) in &payload_rows {
        if *ext != 0 && *ext != 1 {
            return Err(CoreError::Corrupt {
                message: "invalid external flag".to_string(),
            });
        }
        if *ext == 1 {
            let k = key_opt.as_deref().ok_or_else(|| CoreError::Corrupt {
                message: "missing external blob_key".to_string(),
            })?;
            parse_blob_key(k).ok_or_else(|| CoreError::Corrupt {
                message: "invalid blob key".to_string(),
            })?;
            if *has_data != 0 {
                return Err(CoreError::Corrupt {
                    message: "external payload has inline data".to_string(),
                });
            }
            if !blob_keys.contains(k) {
                return Err(CoreError::Corrupt {
                    message: "missing external blob metadata".to_string(),
                });
            }
        } else if let Some(k) = key_opt.as_deref() {
            parse_blob_key(k).ok_or_else(|| CoreError::Corrupt {
                message: "invalid blob key".to_string(),
            })?;
            return Err(CoreError::Corrupt {
                message: "inline payload has blob_key".to_string(),
            });
        }
    }
    Ok(())
}

/// Validate one complete candidate before opening any of its blob files.
fn load_payload_rows(blobs: &Path, rows: Vec<VerifiedPayload>) -> Result<ClipPayloads, CoreError> {
    for (_, external, data, key, sha, bytes) in &rows {
        match external {
            1 => {
                if data.is_some() {
                    return Err(CoreError::Corrupt {
                        message: "external payload has inline data".to_string(),
                    });
                }
                let (Some(key), Some(sha), Some(bytes)) = (key, sha, bytes) else {
                    return Err(CoreError::Corrupt {
                        message: "missing external blob metadata".to_string(),
                    });
                };
                validate_blob_metadata(key, sha, *bytes)?;
            }
            0 if key.is_none() => {}
            0 => {
                return Err(CoreError::Corrupt {
                    message: "inline payload has blob_key".to_string(),
                })
            }
            _ => {
                return Err(CoreError::Corrupt {
                    message: "invalid external flag".to_string(),
                })
            }
        }
    }
    rows.into_iter()
        .map(|(mime, _, data, key, sha, _)| {
            let bytes = if let (Some(key), Some(sha)) = (key, sha) {
                safe_blob_read(blobs, &key, &sha)?
            } else {
                data.unwrap_or_default()
            };
            Ok((mime, bytes))
        })
        .collect()
}

/// Create an exclusive quarantine directory without following existing links.
fn create_quarantine(blobs: &Path, now: i64) -> Result<PathBuf, CoreError> {
    for counter in 0..=100 {
        let name = if counter == 0 {
            format!("orphan-{now}")
        } else {
            format!("orphan-{now}-{counter}")
        };
        let path = blobs.join(name);
        match fs::symlink_metadata(&path) {
            Ok(m) => {
                if m.file_type().is_symlink() {
                    return Err(CoreError::Corrupt {
                        message: "quarantine is symlink".to_string(),
                    });
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt as _;
                    if m.file_attributes() & 0x400 != 0 {
                        return Err(CoreError::Corrupt {
                            message: "quarantine is reparse point".to_string(),
                        });
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&path).map_err(CoreError::Io)?;
                blobs_root_ok(&path)?;
                return Ok(path);
            }
            Err(e) => return Err(CoreError::Io(e)),
        }
    }
    Err(CoreError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "too many quarantine collisions",
    )))
}

/// Fallible `u64` to `i64` for SQL parameters.
fn i64_from_u64(v: u64) -> Result<i64, CoreError> {
    i64::try_from(v).map_err(|e| CoreError::Backend {
        backend: "store".to_string(),
        kind: BackendKind::Internal,
        message: e.to_string(),
    })
}

/// Fallible `usize` to `i64` for SQL parameters.
fn i64_from_usize(v: usize) -> Result<i64, CoreError> {
    let u = u64::try_from(v).map_err(|e| CoreError::Backend {
        backend: "store".to_string(),
        kind: BackendKind::Internal,
        message: e.to_string(),
    })?;
    i64_from_u64(u)
}

/// Open a connection with busy timeout, WAL, and foreign keys.
fn open_conn(db: &Path, read_only: bool) -> Result<Connection, CoreError> {
    let conn = if read_only {
        Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
    } else {
        Connection::open(db)
    }
    .map_err(|e| db_err(&e))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| db_err(&e))?;
    if !read_only {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| db_err(&e))?;
    }
    Ok(conn)
}

/// True when `PRAGMA quick_check` reports exactly `ok`.
fn quick_ok(conn: &Connection) -> bool {
    let Ok(mut stmt) = conn.prepare("PRAGMA quick_check") else {
        return false;
    };
    let Ok(mapped) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
        return false;
    };
    match mapped.collect::<Result<Vec<_>, _>>() {
        Ok(v) => v.len() == 1 && v.first().map(String::as_str) == Some("ok"),
        Err(_) => false,
    }
}

/// Probe FTS5 support with a temp virtual table (no main-DB writes).
fn probe_fts(conn: &Connection) -> bool {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE temp.fts_probe USING fts5(x); DROP TABLE temp.fts_probe;",
    )
    .is_ok()
}

/// Create the search index: FTS5 when available, plain table otherwise.
fn create_fts_on(conn: &Connection, fts: bool) -> Result<(), rusqlite::Error> {
    if fts {
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS clips_fts USING fts5(clip_id UNINDEXED, text);",
        )
    } else {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS clips_fts(clip_id TEXT PRIMARY KEY, text TEXT NOT NULL);",
        )
    }
}

/// Insert missing `meta` rows (fresh create and migration alike).
fn ensure_meta(conn: &Connection, now: i64) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT OR IGNORE INTO meta(k,v) VALUES('schema_version','1')",
        [],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO meta(k,v) VALUES('machine',?1)",
        params![machine_name()],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO meta(k,v) VALUES('created',?1)",
        params![now.to_string()],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO meta(k,v) VALUES('privacy_skips','0')",
        [],
    )?;
    Ok(())
}

/// Run the v1 DDL, create the search index, ensure `meta`, stamp version.
fn ensure_schema(conn: &Connection, now: i64) -> Result<(), CoreError> {
    conn.execute_batch(SCHEMA_SQL).map_err(|e| db_err(&e))?;
    let fts = probe_fts(conn);
    create_fts_on(conn, fts).map_err(|e| db_err(&e))?;
    ensure_meta(conn, now).map_err(|e| db_err(&e))?;
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| db_err(&e))?;
    if v < SCHEMA_VERSION {
        conn.execute_batch("PRAGMA user_version=1")
            .map_err(|e| db_err(&e))?;
    }
    Ok(())
}

/// Migrate an older store to v1 inside one transaction.
fn migrate_on(conn: &mut Connection) -> Result<(), CoreError> {
    let fail = |e: rusqlite::Error| CoreError::MigrationFailed {
        version: 1,
        message: e.to_string(),
    };
    let tx = conn.transaction().map_err(fail)?;
    tx.execute_batch(SCHEMA_SQL).map_err(fail)?;
    create_fts_on(&tx, probe_fts(&tx)).map_err(fail)?;
    ensure_meta(&tx, now_ms()).map_err(fail)?;
    tx.execute_batch("PRAGMA user_version=1").map_err(fail)?;
    tx.commit().map_err(fail)?;
    Ok(())
}

/// Copy DB plus WAL/SHM sidecars aside; returns the main aside path.
fn preserve_aside(profile: &Path, db: &Path) -> Result<PathBuf, CoreError> {
    let aside = profile.join(format!("clipstore.corrupt-{}.sqlite3", now_ms()));
    fs::copy(db, &aside).map_err(CoreError::Io)?;
    for suffix in ["-wal", "-shm"] {
        let src = PathBuf::from(format!("{}{suffix}", db.display()));
        if src.is_file() {
            let dst = PathBuf::from(format!("{}{suffix}", aside.display()));
            fs::copy(&src, &dst).map_err(CoreError::Io)?;
        }
    }
    Ok(aside)
}

/// Removes a scratch directory on drop.
struct TmpGuard {
    /// Directory to remove.
    path: PathBuf,
}

impl Drop for TmpGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Columns selected for a clip row, in order.
const CLIP_COLS: &str = "id, created_ms, source_app, source_os, pinned, removed, \
    preview, search_text, bytes_total, content_hash, rev_ms, rev_dev, rev_ctr, \
    last_used_ms, origin_dev, origin";

/// Build a `Clip` from a row over [`CLIP_COLS`] (formats filled by caller).
fn clip_of(row: &rusqlite::Row<'_>) -> rusqlite::Result<Clip> {
    let pinned: i64 = row.get(4)?;
    let removed: i64 = row.get(5)?;
    let bytes_i: i64 = row.get(8)?;
    let ctr_i: i64 = row.get(12)?;
    Ok(Clip {
        id: row.get::<_, String>(0)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        created_ms: row.get(1)?,
        source_app: row.get(2)?,
        source_os: row.get(3)?,
        pinned: pinned != 0,
        removed: removed != 0,
        preview: row.get(6)?,
        search_text: row.get(7)?,
        formats: Vec::new(),
        bytes_total: u64::try_from(bytes_i.max(0)).unwrap_or(u64::MAX),
        content_hash: row.get(9)?,
        rev: Rev {
            wall_ms: row.get(10)?,
            dev: row.get(11)?,
            ctr: u64::try_from(ctr_i.max(0)).unwrap_or(u64::MAX),
        },
        last_used_ms: row.get(13)?,
        origin_dev: row.get(14)?,
        origin: row.get(15)?,
    })
}

impl Store {
    /// Open or create a store at `profile/clipstore.sqlite3`.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::DowngradeRefused`] for newer schemas (zero writes),
    /// [`CoreError::MigrationFailed`] when migration fails, or
    /// [`CoreError::Corrupt`] when the DB fails `quick_check`.
    pub fn open(profile: &Path) -> Result<Self, CoreError> {
        Self::open_inner(profile, false)
    }

    /// Open an existing store read-only for recovery (writes are refused).
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::DowngradeRefused`] for newer schemas,
    /// [`CoreError::MigrationFailed`] when migration would be needed, or
    /// [`CoreError::Corrupt`] when the DB is unreadable.
    pub fn open_readonly_recovery(profile: &Path) -> Result<Self, CoreError> {
        Self::open_inner(profile, true)
    }

    /// Shared open path (D-F2): version gate, migrate, `quick_check`.
    fn open_inner(profile: &Path, read_only: bool) -> Result<Self, CoreError> {
        mkdir(profile)?;
        let db_path = profile.join(DB_NAME);
        let blobs = profile.join("blobs");
        blobs_root_ok(&blobs)?;
        mkdir(&blobs)?;
        blobs_root_ok(&blobs)?;
        if read_only {
            return Self::open_recovery(profile, &db_path, &blobs);
        }
        // Single connection: the version gate runs before any write, so a
        // refused open leaves the file byte-identical (D-F2).
        let mut conn = Connection::open(&db_path).map_err(|e| db_err(&e))?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|e| db_err(&e))?;
        let v = conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0));
        match v {
            Err(_) => {
                drop(conn);
                let aside = preserve_aside(profile, &db_path)?;
                return Err(CoreError::Corrupt {
                    message: format!(
                        "unreadable store header; preserved aside at {}",
                        aside.display()
                    ),
                });
            }
            Ok(v) if v > SCHEMA_VERSION => {
                return Err(CoreError::DowngradeRefused {
                    found: v,
                    expected: SCHEMA_VERSION,
                });
            }
            Ok(v) if v < SCHEMA_VERSION => migrate_on(&mut conn)?,
            Ok(_) => {}
        }
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| db_err(&e))?;
        ensure_schema(&conn, now_ms())?;
        if !quick_ok(&conn) {
            drop(conn);
            let aside = preserve_aside(profile, &db_path)?;
            return Err(CoreError::Corrupt {
                message: format!("quick_check failed; preserved aside at {}", aside.display()),
            });
        }
        validate_db_keys(&conn)?;
        let fts = probe_fts(&conn);
        Ok(Self {
            blobs: blobs.clone(),
            db_path: db_path.clone(),
            conn: Mutex::new(conn),
            fts,
            read_only,
        })
    }

    /// Read-only recovery open: version gate, no writes, no schema changes.
    fn open_recovery(profile: &Path, db_path: &Path, blobs: &Path) -> Result<Self, CoreError> {
        if !db_path.exists() {
            return Err(CoreError::Corrupt {
                message: "no store to recover".to_string(),
            });
        }
        let conn = open_conn(db_path, true)?;
        match conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)) {
            Err(_) => {
                drop(conn);
                let aside = preserve_aside(profile, db_path)?;
                return Err(CoreError::Corrupt {
                    message: format!(
                        "unreadable store header; preserved aside at {}",
                        aside.display()
                    ),
                });
            }
            Ok(v) if v > SCHEMA_VERSION => {
                return Err(CoreError::DowngradeRefused {
                    found: v,
                    expected: SCHEMA_VERSION,
                });
            }
            Ok(v) if v < SCHEMA_VERSION => {
                return Err(CoreError::MigrationFailed {
                    version: 1,
                    message: "migration needs a writable store".to_string(),
                });
            }
            Ok(_) => {}
        }
        if !quick_ok(&conn) {
            return Err(CoreError::Corrupt {
                message: "restore database failed quick_check".to_string(),
            });
        }
        validate_db_keys(&conn)?;
        blobs_root_ok(blobs)?;
        let fts = probe_fts(&conn);
        Ok(Self {
            blobs: blobs.to_path_buf(),
            db_path: db_path.to_path_buf(),
            conn: Mutex::new(conn),
            fts,
            read_only: true,
        })
    }

    /// Lock the connection (poison heals by taking the guard).
    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Flush committed WAL frames, refusing a busy checkpoint.
    ///
    /// # Errors
    /// Returns a database error or Unavailable if another reader blocks truncation.
    pub fn checkpoint(&self) -> Result<(), CoreError> {
        let busy: i64 = self
            .lock()
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .map_err(|error| db_err(&error))?;
        if busy != 0 {
            return Err(CoreError::Backend {
                backend: "sqlite".to_string(),
                kind: BackendKind::Unavailable,
                message: "checkpoint is busy".to_string(),
            });
        }
        Ok(())
    }

    /// Checkpoint and close the actual SQLite connection. Failure returns the
    /// original Store so callers can keep serving it without moving any files.
    ///
    /// # Errors
    /// Returns the original Store and the checkpoint or connection-close error.
    #[allow(clippy::result_large_err)]
    pub fn close(self) -> Result<(), (Self, CoreError)> {
        if let Err(error) = self.checkpoint() {
            return Err((self, error));
        }
        #[cfg(test)]
        if let Err(error) = check_restore_fault(RestoreFault::Close) {
            return Err((self, error));
        }
        let Self {
            blobs,
            db_path,
            conn,
            fts,
            read_only,
        } = self;
        let connection = conn
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        connection.close().map_err(|(connection, error)| {
            (
                Self {
                    blobs,
                    db_path,
                    conn: Mutex::new(connection),
                    fts,
                    read_only,
                },
                db_err(&error),
            )
        })
    }

    /// Content key and hex digest for one external blob.
    fn blob_key_for(bytes: &[u8]) -> (String, String) {
        let mut h = Sha256::new();
        h.update(bytes);
        let hex = hex_of(&h.finalize());
        let short = hex.get(..32).unwrap_or(&hex).to_string();
        (format!("{short}-{}", bytes.len()), hex)
    }

    /// Crash-safe external blob write: temp, fsync, atomic rename.
    fn write_blob_file(&self, key: &str, bytes: &[u8]) -> Result<(), CoreError> {
        parse_blob_key(key).ok_or_else(|| CoreError::Corrupt {
            message: "invalid blob key".to_string(),
        })?;
        blobs_root_ok(&self.blobs)?;
        let final_path = safe_blob_path(&self.blobs, key)?;
        if fs::symlink_metadata(&final_path).is_ok() {
            return Ok(());
        }
        let tmp_name = format!(
            ".tmp-{key}-{}-{}",
            std::process::id(),
            TMP_CTR.fetch_add(1, Ordering::SeqCst)
        );
        let tmp = self.blobs.join(&tmp_name);
        if fs::symlink_metadata(&tmp).is_ok() {
            return Err(CoreError::Corrupt {
                message: "blob temp path exists as symlink".to_string(),
            });
        }
        let mut created = false;
        let res: Result<(), CoreError> = (|| {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .map_err(CoreError::Io)?;
            created = true;
            f.write_all(bytes).map_err(CoreError::Io)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mut perms = fs::metadata(&tmp).map_err(CoreError::Io)?.permissions();
                perms.set_mode(0o600);
                fs::set_permissions(&tmp, perms).map_err(CoreError::Io)?;
            }
            f.sync_all().map_err(CoreError::Io)?;
            drop(f);
            if fs::symlink_metadata(&tmp).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(CoreError::Corrupt {
                    message: "blob temp became symlink".to_string(),
                });
            }
            if fs::symlink_metadata(&final_path).is_ok() {
                return Ok(());
            }
            fs::rename(&tmp, &final_path).map_err(CoreError::Io)?;
            fsync_dir(&self.blobs);
            Ok(())
        })();
        if created {
            let _ = fs::remove_file(&tmp);
        }
        res
    }

    /// Best-effort payload load for dedupe hashing (`None` on any failure).
    fn try_load_payloads(conn: &Connection, blobs: &Path, id: &str) -> Option<ClipPayloads> {
        let mut stmt = conn
            .prepare(
                "SELECT p.mime, p.external, p.data, p.blob_key, b.sha256, b.bytes FROM clip_payloads p LEFT JOIN clip_blobs b ON b.blob_key=p.blob_key WHERE p.clip_id=?1 ORDER BY p.rowid",
            )
            .ok()?;
        let rows: Vec<VerifiedPayload> = stmt
            .query_map(params![id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .ok()?
            .collect::<Result<_, _>>()
            .ok()?;
        load_payload_rows(blobs, rows).ok()
    }

    /// Find a dedupe hit among the last live clips (D-F10).
    fn dedupe_hit(&self, incoming: &str, exclude: &Uuid) -> Result<Option<Uuid>, CoreError> {
        let conn = self.lock();
        let window = i64_from_usize(DEDUPE_WINDOW)?;
        let mut stmt = conn
            .prepare("SELECT id FROM clips WHERE removed=0 ORDER BY last_used_ms DESC LIMIT ?1")
            .map_err(|e| db_err(&e))?;
        let ids: Vec<String> = stmt
            .query_map(params![window], |r| r.get(0))
            .map_err(|e| db_err(&e))?
            .collect::<Result<_, _>>()
            .map_err(|e| db_err(&e))?;
        drop(stmt);
        let skip = exclude.to_string();
        let mut cands = Vec::new();
        for id in &ids {
            if *id == skip {
                continue;
            }
            let Some(payloads) = Self::try_load_payloads(&conn, &self.blobs, id) else {
                continue;
            };
            let Ok(uuid) = id.parse::<Uuid>() else {
                continue;
            };
            cands.push(DedupeCandidate {
                id: uuid,
                content_hash: content_hash(&payloads),
                removed: false,
                last_used_ms: 0,
            });
        }
        Ok(policy::find_dedupe(&cands, incoming, DEDUPE_WINDOW))
    }

    /// Insert or upsert a clip with payloads; dedupe moves the hit to top.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Unsupported`] on a recovery handle, or a backend
    /// error when the write fails.
    pub fn insert_clip(
        &self,
        clip: &Clip,
        payloads: &[(String, Vec<u8>)],
    ) -> Result<InsertOutcome, CoreError> {
        if self.read_only {
            return Err(readonly_err());
        }
        let incoming = content_hash(payloads);
        if let Some(hit) = self.dedupe_hit(&incoming, &clip.id)? {
            self.lock()
                .execute(
                    "UPDATE clips SET last_used_ms=?1 WHERE id=?2",
                    params![clip.last_used_ms, hit.to_string()],
                )
                .map_err(|e| db_err(&e))?;
            return Ok(InsertOutcome::MovedToTop(hit));
        }
        let mut ext: Vec<(String, String, String)> = Vec::with_capacity(payloads.len());
        for (mime, bytes) in payloads {
            if bytes.len() > INLINE_LIMIT {
                let (key, sha) = Self::blob_key_for(bytes);
                self.write_blob_file(&key, bytes)?;
                ext.push((mime.clone(), key, sha));
            } else {
                ext.push((mime.clone(), String::new(), String::new()));
            }
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(|e| db_err(&e))?;
        tx.execute(
            "INSERT INTO clips(id, created_ms, source_app, source_os, pinned, removed, \
            preview, search_text, bytes_total, content_hash, rev_ms, rev_dev, rev_ctr, \
            last_used_ms, origin_dev, origin) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16) \
            ON CONFLICT(id) DO UPDATE SET created_ms=excluded.created_ms, source_app=excluded.source_app, \
            source_os=excluded.source_os, pinned=excluded.pinned, removed=excluded.removed, preview=excluded.preview, \
            search_text=excluded.search_text, bytes_total=excluded.bytes_total, content_hash=excluded.content_hash, \
            rev_ms=excluded.rev_ms, rev_dev=excluded.rev_dev, rev_ctr=excluded.rev_ctr, \
            last_used_ms=excluded.last_used_ms, origin_dev=excluded.origin_dev, origin=excluded.origin",
            params![
                clip.id.to_string(), clip.created_ms, clip.source_app, clip.source_os,
                i64::from(clip.pinned), i64::from(clip.removed), clip.preview, clip.search_text,
                i64_from_u64(clip.bytes_total)?, clip.content_hash, clip.rev.wall_ms, clip.rev.dev,
                i64_from_u64(clip.rev.ctr)?, clip.last_used_ms, clip.origin_dev, clip.origin,
            ],
        ).map_err(|e| db_err(&e))?;
        tx.execute(
            "DELETE FROM clip_payloads WHERE clip_id=?1",
            params![clip.id.to_string()],
        )
        .map_err(|e| db_err(&e))?;
        for ((mime, bytes), (_, key, sha)) in payloads.iter().zip(ext.iter()) {
            if key.is_empty() {
                let native = native_id_of(mime);
                tx.execute(
                    "INSERT INTO clip_payloads(clip_id, mime, external, data, blob_key, native_id) \
                    VALUES(?1,?2,0,?3,NULL,?4)",
                    params![clip.id.to_string(), mime, bytes, native],
                ).map_err(|e| db_err(&e))?;
            } else {
                tx.execute(
                    "INSERT OR IGNORE INTO clip_blobs(blob_key, sha256, bytes, codec, created_ms) \
                    VALUES(?1,?2,?3,'raw',?4)",
                    params![key, sha, i64_from_usize(bytes.len())?, clip.created_ms],
                )
                .map_err(|e| db_err(&e))?;
                let native = native_id_of(mime);
                tx.execute(
                    "INSERT INTO clip_payloads(clip_id, mime, external, data, blob_key, native_id) \
                    VALUES(?1,?2,1,NULL,?3,?4)",
                    params![clip.id.to_string(), mime, key, native],
                ).map_err(|e| db_err(&e))?;
            }
        }
        tx.execute(
            "DELETE FROM clips_fts WHERE clip_id=?1",
            params![clip.id.to_string()],
        )
        .map_err(|e| db_err(&e))?;
        tx.execute(
            "INSERT INTO clips_fts(clip_id, text) VALUES(?1,?2)",
            params![clip.id.to_string(), clip.search_text],
        )
        .map_err(|e| db_err(&e))?;
        tx.commit().map_err(|e| db_err(&e))?;
        if payloads.is_empty() {
            Ok(InsertOutcome::Stubbed(clip.id))
        } else {
            Ok(InsertOutcome::Inserted(clip.id))
        }
    }

    /// Bump clip to top of history.
    ///
    /// # Errors
    /// Returns error if store is read-only or SQLite fails.
    pub fn bump_clip(&self, id: Uuid, now_ms: i64) -> Result<bool, CoreError> {
        if self.read_only {
            return Err(readonly_err());
        }
        let changed = self
            .lock()
            .execute(
                "UPDATE clips SET last_used_ms=?1 WHERE id=?2",
                params![now_ms, id.to_string()],
            )
            .map_err(|e| db_err(&e))?;
        Ok(changed > 0)
    }

    /// Fetch one clip with its payloads (external blobs verified).
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Corrupt`] when an external blob is missing or
    /// fails verification, or a backend error when the read fails.
    pub fn get_clip(&self, id: Uuid) -> Result<Option<(Clip, ClipPayloads)>, CoreError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!("SELECT {CLIP_COLS} FROM clips WHERE id=?1"))
            .map_err(|e| db_err(&e))?;
        let mapped = stmt
            .query_map(params![id.to_string()], clip_of)
            .map_err(|e| db_err(&e))?;
        let mut clips: Vec<Clip> = mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| db_err(&e))?;
        drop(stmt);
        let Some(clip) = clips.first_mut() else {
            return Ok(None);
        };
        let mut stmt = conn
            .prepare(
                "SELECT p.mime, p.external, p.data, p.blob_key, b.sha256, b.bytes FROM clip_payloads p \
                LEFT JOIN clip_blobs b ON b.blob_key=p.blob_key WHERE p.clip_id=?1 ORDER BY p.rowid",
            )
            .map_err(|e| db_err(&e))?;
        let mapped = stmt
            .query_map(params![id.to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                ))
            })
            .map_err(|e| db_err(&e))?;
        let rows: Vec<VerifiedPayload> =
            mapped
                .collect::<Result<_, _>>()
                .map_err(|_| CoreError::Corrupt {
                    message: "invalid payload types".to_string(),
                })?;
        drop(stmt);
        let out = load_payload_rows(&self.blobs, rows)?;
        clip.formats = out.iter().map(|(m, _)| m.clone()).collect();
        Ok(Some((clip.clone(), out)))
    }

    /// Create a Paste permanent, minting its v7 id and implicit group.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Config`] for invalid fields or a duplicate
    /// hotkey, [`CoreError::Unsupported`] on a recovery handle, or a
    /// backend error when the write fails.
    pub fn create_permanent(
        &self,
        group: &str,
        name: &str,
        hotkey: Option<&str>,
        body: &str,
    ) -> Result<PermanentClip, CoreError> {
        if self.read_only {
            return Err(readonly_err());
        }
        let (group, name) = validate_perm_fields(group, name, body)?;
        let hotkey = norm_hotkey(hotkey);
        let clip = PermanentClip {
            id: Uuid::now_v7(),
            group,
            name,
            hotkey,
            kind: PermanentKind::Paste,
            body: body.to_string(),
            js: None,
            rev: Rev {
                wall_ms: now_ms(),
                dev: machine_name(),
                ctr: 0,
            },
        };
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(|e| db_err(&e))?;
        if let Some(hk) = &clip.hotkey {
            let dup: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM permanents WHERE hotkey=?1)",
                    params![hk],
                    |r| r.get(0),
                )
                .map_err(|e| db_err(&e))?;
            if dup {
                return Err(CoreError::Config {
                    message: "permanent hotkey already in use".to_string(),
                });
            }
        }
        tx.execute(
            "INSERT OR IGNORE INTO perm_groups(name, system) VALUES(?1, 0)",
            params![clip.group],
        )
        .map_err(|e| db_err(&e))?;
        tx.execute(
            "INSERT INTO permanents(id, grp, name, hotkey, kind, body, js, rev_ms, rev_dev, rev_ctr) \
             VALUES(?1,?2,?3,?4,'Paste',?5,NULL,?6,?7,0)",
            params![
                clip.id.to_string(),
                clip.group,
                clip.name,
                clip.hotkey,
                clip.body,
                clip.rev.wall_ms,
                clip.rev.dev
            ],
        )
        .map_err(|e| db_err(&e))?;
        tx.commit().map_err(|e| db_err(&e))?;
        Ok(clip)
    }

    /// Fetch one permanent by id.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Corrupt`] for unknown kind text, or a backend
    /// error when the read fails.
    pub fn get_permanent(&self, id: Uuid) -> Result<Option<PermanentClip>, CoreError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!("SELECT {PERM_COLS} FROM permanents WHERE id=?1"))
            .map_err(|e| db_err(&e))?;
        let rows: Vec<PermRow> = stmt
            .query_map(params![id.to_string()], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ))
            })
            .map_err(|e| db_err(&e))?
            .collect::<Result<_, _>>()
            .map_err(|e| db_err(&e))?;
        drop(stmt);
        rows.into_iter().next().map(perm_from_row).transpose()
    }

    /// List permanents ordered by group, name, id; optionally one group.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Corrupt`] for unknown kind text, or a backend
    /// error when the read fails.
    pub fn list_permanents(&self, group: Option<&str>) -> Result<Vec<PermanentClip>, CoreError> {
        let conn = self.lock();
        let rows: Vec<PermRow> = if let Some(g) = group {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {PERM_COLS} FROM permanents WHERE grp=?1 ORDER BY grp ASC, name ASC, id ASC"
                ))
                .map_err(|e| db_err(&e))?;
            let rows = stmt
                .query_map(params![g], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                    ))
                })
                .map_err(|e| db_err(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_err(&e))?;
            rows
        } else {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {PERM_COLS} FROM permanents ORDER BY grp ASC, name ASC, id ASC"
                ))
                .map_err(|e| db_err(&e))?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                    ))
                })
                .map_err(|e| db_err(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_err(&e))?;
            rows
        };
        rows.into_iter().map(perm_from_row).collect()
    }

    /// Replace editable fields of one permanent, bumping `rev.ctr`.
    /// Returns `false` when no such id exists (never an upsert).
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Config`] for non-Paste kinds, invalid fields
    /// or a duplicate hotkey, [`CoreError::Unsupported`] on a recovery
    /// handle, or a backend error when the write fails.
    pub fn update_permanent(&self, clip: &PermanentClip) -> Result<bool, CoreError> {
        if self.read_only {
            return Err(readonly_err());
        }
        if clip.kind != PermanentKind::Paste {
            return Err(CoreError::Config {
                message: "permanent kind is v2 in this build".to_string(),
            });
        }
        let (group, name) = validate_perm_fields(&clip.group, &clip.name, &clip.body)?;
        let hotkey = norm_hotkey(clip.hotkey.as_deref());
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(|e| db_err(&e))?;
        let existing: Option<(String, i64)> = {
            let mut stmt = tx
                .prepare("SELECT rev_dev, rev_ctr FROM permanents WHERE id=?1")
                .map_err(|e| db_err(&e))?;
            let rows: Vec<(String, i64)> = stmt
                .query_map(params![clip.id.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| db_err(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_err(&e))?;
            rows.into_iter().next()
        };
        let Some((dev, ctr)) = existing else {
            return Ok(false);
        };
        if let Some(hk) = &hotkey {
            let dup: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM permanents WHERE hotkey=?1 AND id!=?2)",
                    params![hk, clip.id.to_string()],
                    |r| r.get(0),
                )
                .map_err(|e| db_err(&e))?;
            if dup {
                return Err(CoreError::Config {
                    message: "permanent hotkey already in use".to_string(),
                });
            }
        }
        tx.execute(
            "INSERT OR IGNORE INTO perm_groups(name, system) VALUES(?1, 0)",
            params![group],
        )
        .map_err(|e| db_err(&e))?;
        let new_ctr = ctr.saturating_add(1);
        tx.execute(
            "UPDATE permanents SET grp=?1, name=?2, hotkey=?3, kind='Paste', body=?4, js=?5, \
             rev_ms=?6, rev_dev=?7, rev_ctr=?8 WHERE id=?9",
            params![
                group,
                name,
                hotkey,
                clip.body,
                clip.js,
                now_ms(),
                dev,
                new_ctr,
                clip.id.to_string()
            ],
        )
        .map_err(|e| db_err(&e))?;
        tx.commit().map_err(|e| db_err(&e))?;
        Ok(true)
    }

    /// Hard-delete one permanent; `false` when no such id exists.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Unsupported`] on a recovery handle, or a
    /// backend error when the write fails.
    pub fn delete_permanent(&self, id: Uuid) -> Result<bool, CoreError> {
        if self.read_only {
            return Err(readonly_err());
        }
        let n = self
            .lock()
            .execute(
                "DELETE FROM permanents WHERE id=?1",
                params![id.to_string()],
            )
            .map_err(|e| db_err(&e))?;
        Ok(n > 0)
    }

    /// List groups with live clip counts, ordered by name.
    ///
    /// # Errors
    ///
    /// Returns a backend error when the read fails.
    pub fn list_perm_groups(&self) -> Result<Vec<PermGroup>, CoreError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT g.name, g.system, COUNT(p.id) FROM perm_groups g \
                 LEFT JOIN permanents p ON p.grp=g.name GROUP BY g.name ORDER BY g.name ASC",
            )
            .map_err(|e| db_err(&e))?;
        let groups: Vec<PermGroup> = stmt
            .query_map([], |r| {
                let system: i64 = r.get(1)?;
                let count: i64 = r.get(2)?;
                Ok(PermGroup {
                    name: r.get(0)?,
                    system: system != 0,
                    count: u64::try_from(count.max(0)).unwrap_or(u64::MAX),
                })
            })
            .map_err(|e| db_err(&e))?
            .collect::<Result<_, _>>()
            .map_err(|e| db_err(&e))?;
        Ok(groups)
    }

    /// Create an empty group; `false` when it already exists.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Config`] for a blank/control-char name,
    /// [`CoreError::Unsupported`] on a recovery handle, or a backend error.
    pub fn create_perm_group(&self, name: &str) -> Result<bool, CoreError> {
        if self.read_only {
            return Err(readonly_err());
        }
        let name = name.trim();
        if name.is_empty() || name.chars().any(char::is_control) {
            return Err(CoreError::Config {
                message: "permanent group must be non-empty without control characters".to_string(),
            });
        }
        let n = self
            .lock()
            .execute(
                "INSERT OR IGNORE INTO perm_groups(name, system) VALUES(?1, 0)",
                params![name],
            )
            .map_err(|e| db_err(&e))?;
        Ok(n > 0)
    }

    /// Delete an empty group; `false` when missing or non-empty.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Unsupported`] on a recovery handle, or a
    /// backend error when the write fails.
    pub fn delete_perm_group(&self, name: &str) -> Result<bool, CoreError> {
        if self.read_only {
            return Err(readonly_err());
        }
        let name = name.trim();
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(|e| db_err(&e))?;
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM permanents WHERE grp=?1",
                params![name],
                |r| r.get(0),
            )
            .map_err(|e| db_err(&e))?;
        if count > 0 {
            return Ok(false);
        }
        let n = tx
            .execute("DELETE FROM perm_groups WHERE name=?1", params![name])
            .map_err(|e| db_err(&e))?;
        tx.commit().map_err(|e| db_err(&e))?;
        Ok(n > 0)
    }

    /// List clips newest-first with formats derived from payload rows.
    ///
    /// # Errors
    ///
    /// Returns a backend error when the read fails.
    pub fn list_history(
        &self,
        limit: u64,
        offset: u64,
        include_removed: bool,
    ) -> Result<Vec<Clip>, CoreError> {
        let conn = self.lock();
        let sql = if include_removed {
            format!("SELECT {CLIP_COLS} FROM clips ORDER BY last_used_ms DESC LIMIT ?1 OFFSET ?2")
        } else {
            format!("SELECT {CLIP_COLS} FROM clips WHERE removed=0 ORDER BY last_used_ms DESC LIMIT ?1 OFFSET ?2")
        };
        let mut stmt = conn.prepare(&sql).map_err(|e| db_err(&e))?;
        let mut clips: Vec<Clip> = stmt
            .query_map(
                params![i64_from_u64(limit)?, i64_from_u64(offset)?],
                clip_of,
            )
            .map_err(|e| db_err(&e))?
            .collect::<Result<_, _>>()
            .map_err(|e| db_err(&e))?;
        drop(stmt);
        for c in &mut clips {
            c.formats = mime_list(&conn, &c.id.to_string())?;
        }
        Ok(clips)
    }

    /// Full-text search over `search_text` (FTS5, LIKE fallback).
    ///
    /// # Errors
    ///
    /// Returns a backend error when the read fails.
    pub fn search(&self, query: &str) -> Result<Vec<Clip>, CoreError> {
        let tokens: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(str::to_lowercase)
            .collect();
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        if self.fts {
            let match_q = tokens
                .iter()
                .map(|t| format!("\"{t}\""))
                .collect::<Vec<_>>()
                .join(" AND ");
            if let Some(mut clips) = fts_search(&conn, &match_q) {
                for c in &mut clips {
                    c.formats = mime_list(&conn, &c.id.to_string())?;
                }
                return Ok(clips);
            }
        }
        let mut sql = format!("SELECT {CLIP_COLS} FROM clips WHERE removed=0");
        for _ in &tokens {
            sql.push_str(" AND LOWER(search_text) LIKE ? ESCAPE '\\'");
        }
        sql.push_str(" ORDER BY last_used_ms DESC");
        let mut stmt = conn.prepare(&sql).map_err(|e| db_err(&e))?;
        let like: Vec<String> = tokens
            .iter()
            .map(|t| format!("%{}%", escape_like(t)))
            .collect();
        let mut clips: Vec<Clip> = stmt
            .query_map(rusqlite::params_from_iter(like), clip_of)
            .map_err(|e| db_err(&e))?
            .collect::<Result<_, _>>()
            .map_err(|e| db_err(&e))?;
        drop(stmt);
        for c in &mut clips {
            c.formats = mime_list(&conn, &c.id.to_string())?;
        }
        Ok(clips)
    }

    /// Mark one clip removed (soft delete).
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Unsupported`] on a recovery handle, or a backend
    /// error when the write fails.
    pub fn soft_delete(&self, id: Uuid) -> Result<(), CoreError> {
        if self.read_only {
            return Err(readonly_err());
        }
        self.lock()
            .execute(
                "UPDATE clips SET removed=1 WHERE id=?1",
                params![id.to_string()],
            )
            .map_err(|e| db_err(&e))?;
        Ok(())
    }

    /// Hard-delete every removed clip; returns the purge count.
    #[must_use]
    pub fn purge_removed(&self) -> u64 {
        self.purge_removed_inner().unwrap_or(0)
    }

    /// Purging work behind [`Store::purge_removed`].
    fn purge_removed_inner(&self) -> Result<u64, CoreError> {
        if self.read_only {
            return Ok(0);
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(|e| db_err(&e))?;
        validate_db_keys(&tx)?;
        let ids = removed_ids(&tx)?;
        let n = ids.len() as u64;
        delete_clips(&tx, &ids)?;
        sweep_blobs(&tx, &self.blobs)?;
        tx.commit().map_err(|e| db_err(&e))?;
        Ok(n)
    }

    /// Enforce count, byte, and age caps oldest-first (pinned exempt).
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Unsupported`] on a recovery handle, or a backend
    /// error when the write fails.
    pub fn enforce_retention(&self, limits: &Limits) -> Result<u64, CoreError> {
        if self.read_only {
            return Err(readonly_err());
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(|e| db_err(&e))?;
        validate_db_keys(&tx)?;
        let mut purged = 0_u64;
        purged += enforce_count(&tx, limits.max_items)?;
        purged += enforce_age(&tx, limits.max_age_days, now_ms())?;
        purged += enforce_bytes(&tx, limits.max_store_bytes)?;
        sweep_blobs(&tx, &self.blobs)?;
        tx.commit().map_err(|e| db_err(&e))?;
        Ok(purged)
    }

    /// Quarantine orphan blob files; report dangling rows (never delete).
    #[must_use]
    pub fn gc_blobs(&self, now: i64) -> GcReport {
        let empty = || GcReport {
            orphaned_files: 0,
            dangling_blobs: 0,
            quarantine_dir: self.blobs.join(format!("orphan-{now}")),
        };
        self.gc_inner(now).unwrap_or_else(|_| empty())
    }

    /// GC work behind [`Store::gc_blobs`].
    fn gc_inner(&self, now: i64) -> Result<GcReport, CoreError> {
        let quarantine_dir = self.blobs.join(format!("orphan-{now}"));
        if self.read_only {
            return Ok(GcReport {
                orphaned_files: 0,
                dangling_blobs: 0,
                quarantine_dir,
            });
        }
        blobs_root_ok(&self.blobs)?;
        let conn = self.lock();
        let referenced = referenced_keys(&conn)?;
        let mut stmt = conn
            .prepare("SELECT blob_key FROM clip_blobs")
            .map_err(|e| db_err(&e))?;
        let stored: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .map_err(|e| db_err(&e))?
            .collect::<Result<_, _>>()
            .map_err(|e| db_err(&e))?;
        drop(stmt);
        let dangling_blobs = stored
            .iter()
            .filter(|k| !referenced.contains(k.as_str()))
            .count();
        let dangling = u64::try_from(dangling_blobs).unwrap_or(u64::MAX);
        let quarantine_dir = create_quarantine(&self.blobs, now)?;
        let mut orphaned_files = 0_u64;
        let entries = fs::read_dir(&self.blobs).map_err(CoreError::Io)?;
        for e in entries {
            let e = e.map_err(CoreError::Io)?;
            let ft = e.file_type().map_err(CoreError::Io)?;
            if ft.is_symlink() {
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            if referenced.contains(name.as_str()) {
                continue;
            }
            let src_meta = fs::symlink_metadata(e.path()).map_err(CoreError::Io)?;
            if src_meta.file_type().is_symlink() || !src_meta.is_file() {
                continue;
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                if (src_meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                    continue;
                }
            }
            fs::rename(e.path(), quarantine_dir.join(&name)).map_err(CoreError::Io)?;
            orphaned_files += 1;
        }
        fsync_dir(&self.blobs);
        Ok(GcReport {
            orphaned_files,
            dangling_blobs: dangling,
            quarantine_dir,
        })
    }

    /// Write a ZIP backup: manifest, DB copy (backup API), referenced blobs.
    ///
    /// # Errors
    ///
    /// Returns a backend, config, or I/O error when the backup cannot be written.
    /// A missing `config.toml` is not an error; other config read/type/link/
    /// parse/bounded size failures are reported before opening `dest`.
    #[allow(clippy::too_many_lines)]
    pub fn backup_zip(&self, dest: &Path) -> Result<(), CoreError> {
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                mkdir(parent)?;
            }
        }
        let profile = self.db_path.parent().unwrap_or_else(|| Path::new("."));
        let config_data = config_bytes_for_backup(profile)?;
        let tmp = create_private_dir("cm-backup")?;
        let _guard = TmpGuard { path: tmp.clone() };
        let tmp_db = tmp.join("clipstore.sqlite3");
        let tmp_blobs = tmp.join("blobs");
        let (clips, referenced, blob_entries, db_bytes) = {
            let conn = self.lock();
            {
                let mut out_db = Connection::open(&tmp_db).map_err(|e| db_err(&e))?;
                let backup =
                    rusqlite::backup::Backup::new(&conn, &mut out_db).map_err(|e| db_err(&e))?;
                if !matches!(
                    backup.step(-1).map_err(|e| db_err(&e))?,
                    rusqlite::backup::StepResult::Done
                ) {
                    return Err(CoreError::Backend {
                        backend: "sqlite".to_string(),
                        kind: BackendKind::Internal,
                        message: "backup snapshot did not complete".to_string(),
                    });
                }
            }
            let snap_conn = Connection::open(&tmp_db).map_err(|e| db_err(&e))?;
            snap_conn
                .busy_timeout(Duration::from_secs(5))
                .map_err(|e| db_err(&e))?;
            validate_db_keys(&snap_conn)?;
            let clips: i64 = snap_conn
                .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
                .map_err(|e| db_err(&e))?;
            let referenced = referenced_keys(&snap_conn)?;
            let mut blob_metadata = Vec::with_capacity(referenced.len());
            for key in &referenced {
                let (sha, bytes): (String, i64) = snap_conn
                    .query_row(
                        "SELECT sha256, bytes FROM clip_blobs WHERE blob_key=?1",
                        params![key],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .map_err(|e| db_err(&e))?;
                validate_blob_metadata(key, &sha, bytes)?;
                blob_metadata.push((key.clone(), sha));
            }
            let db_bytes = fs::read(&tmp_db).map_err(CoreError::Io)?;
            mkdir(&tmp_blobs)?;
            let mut blob_entries = Vec::new();
            for (key, sha) in &blob_metadata {
                let bytes = safe_blob_read(&self.blobs, key, sha)?;
                fs::write(tmp_blobs.join(key), &bytes).map_err(CoreError::Io)?;
                blob_entries.push(format!(
                    "{{key={key:?},bytes={},sha256={sha:?}}}",
                    bytes.len()
                ));
            }
            drop(snap_conn);
            (clips, referenced, blob_entries, db_bytes)
        };
        let db_sha = hex_of(&Sha256::digest(&db_bytes));
        let mut manifest = format!(
            "schema_version = 1\ncreated_ms = {}\nclips = {clips}\ndb_sha256 = {db_sha:?}\n\
            db_bytes = {}\nblobs = [{}]\n",
            now_ms(),
            db_bytes.len(),
            blob_entries.join(",")
        );
        if let Some((_, cfg_len, cfg_sha)) = &config_data {
            use std::fmt::Write as _;
            let _ = write!(
                manifest,
                "config_bytes = {cfg_len}\nconfig_sha256 = {cfg_sha:?}\n"
            );
        }
        fs::write(tmp.join("manifest.toml"), manifest).map_err(CoreError::Io)?;
        let file = fs::File::create(dest).map_err(CoreError::Io)?;
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o600);
        zip.start_file("manifest.toml", opts)
            .map_err(|e| zip_err(&e))?;
        zip.write_all(
            fs::read(tmp.join("manifest.toml"))
                .map_err(CoreError::Io)?
                .as_slice(),
        )
        .map_err(CoreError::Io)?;
        zip.start_file("clipstore.sqlite3", opts)
            .map_err(|e| zip_err(&e))?;
        zip.write_all(&db_bytes).map_err(CoreError::Io)?;
        if let Some((cfg_bytes, _, _)) = &config_data {
            zip.start_file("config.toml", opts)
                .map_err(|e| zip_err(&e))?;
            zip.write_all(cfg_bytes).map_err(CoreError::Io)?;
        }
        for key in &referenced {
            let path = tmp_blobs.join(key);
            zip.start_file(format!("blobs/{key}"), opts)
                .map_err(|e| zip_err(&e))?;
            zip.write_all(fs::read(&path).map_err(CoreError::Io)?.as_slice())
                .map_err(CoreError::Io)?;
        }
        zip.finish().map_err(|e| zip_err(&e))?;
        Ok(())
    }

    /// Transactionally restore a ZIP backup over `profile`.
    ///
    /// This static method is for callers without an open Store handle.
    /// It cannot close arbitrary external processes' connections; the caller
    /// must ensure no other process holds the DB open.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Unsupported`] for traversal, absolute or
    /// drive-prefixed names, symlinks, or entries whose declared size is over
    /// the cap, [`CoreError::Io`] when entry data exceeds its declared size,
    /// [`CoreError::Config`] for bad manifests, or
    /// [`CoreError::Corrupt`] when the embedded DB fails validation.
    pub fn restore_zip(src: &Path, profile: &Path) -> Result<(), CoreError> {
        let prepared = PreparedRestore::prepare(src, profile)?;
        prepared
            .install_and_open()
            .map(drop)
            .map_err(|failure| failure.error)
    }

    /// Validate a destination-local restore while the live Store is still open.
    ///
    /// # Errors
    /// Returns archive-validation, unsafe-component or I/O errors before replacement.
    pub fn prepare_restore(src: &Path, profile: &Path) -> Result<PreparedRestore, CoreError> {
        PreparedRestore::prepare(src, profile)
    }

    /// Run `integrity_check` plus `foreign_key_check` (repair path).
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Corrupt`] when either check fails.
    pub fn integrity_full(&self) -> Result<(), CoreError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("PRAGMA integrity_check")
            .map_err(|e| db_err(&e))?;
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .map_err(|e| db_err(&e))?
            .collect::<Result<_, _>>()
            .map_err(|e| db_err(&e))?;
        drop(stmt);
        if rows.len() != 1 || rows.first().map(String::as_str) != Some("ok") {
            return Err(CoreError::Corrupt {
                message: "integrity_check failed".to_string(),
            });
        }
        let bad: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .map_err(|e| db_err(&e))?;
        if bad != 0 {
            return Err(CoreError::Corrupt {
                message: "foreign_key_check failed".to_string(),
            });
        }
        Ok(())
    }

    /// Count one OS-privacy skip; creates no clip row.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Unsupported`] on a recovery handle, or a backend
    /// error when the write fails.
    pub fn record_privacy_skip(&self) -> Result<(), CoreError> {
        if self.read_only {
            return Err(readonly_err());
        }
        self.lock()
            .execute(
                "UPDATE meta SET v = CAST(CAST(v AS INTEGER) + 1 AS TEXT) WHERE k='privacy_skips'",
                [],
            )
            .map_err(|e| db_err(&e))?;
        Ok(())
    }
}

/// A validated archive bound to its canonical destination and private stage.
pub struct PreparedRestore {
    stage_root: PathBuf,
    incoming: PathBuf,
    old_dir: PathBuf,
    profile: PathBuf,
    cleanup: bool,
}

/// Failed installation, with a usable original Store after successful rollback.
/// When recovery fails, `store` is absent and `recovery_dir` is retained.
/// An initially absent database also has no original Store to return.
#[derive(Debug)]
pub struct RestoreFailure {
    pub error: CoreError,
    pub store: Option<Store>,
    pub recovery_dir: Option<PathBuf>,
}

impl Drop for PreparedRestore {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = fs::remove_dir_all(&self.stage_root);
        }
    }
}

impl PreparedRestore {
    fn prepare(src: &Path, profile: &Path) -> Result<Self, CoreError> {
        ensure_profile_container(profile)?;
        let profile = profile.canonicalize().map_err(CoreError::Io)?;
        check_restore_components(&profile)?;
        if is_ancestor_managed(src, &profile)? {
            return Err(CoreError::Unsupported {
                message: "restore source aliases managed component".to_string(),
            });
        }
        let stage_root = create_stage_root(&profile)?;
        let prepared = Self {
            incoming: stage_root.join("incoming"),
            old_dir: stage_root.join("old"),
            stage_root,
            profile,
            cleanup: true,
        };
        extract_zip(src, &prepared.incoming)?;
        validate_profile(&prepared.incoming)?;
        let blobs = prepared.incoming.join("blobs");
        if !blobs.exists() {
            mkdir(&blobs)?;
        }
        check_restore_components(&prepared.incoming)?;
        Ok(prepared)
    }

    #[must_use]
    pub fn stage_root(&self) -> &Path {
        &self.stage_root
    }
    #[must_use]
    pub fn incoming(&self) -> &Path {
        &self.incoming
    }

    /// Install with every external Store handle closed. The returned connection
    /// is the validated serving connection; recovery is retained until it opens.
    ///
    /// # Errors
    /// Returns the primary error and the reopened original Store after rollback.
    /// Failed rollback or reopen returns no Store and retains the recovery path.
    #[allow(clippy::result_large_err)]
    #[allow(clippy::too_many_lines)]
    pub fn install_and_open(mut self) -> Result<Store, RestoreFailure> {
        let had_database = self.profile.join(DB_NAME).is_file();
        let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut installed: Vec<PathBuf> = Vec::new();
        let install = (|| {
            check_restore_components(&self.profile)?;
            for (index, name) in [
                DB_NAME,
                "clipstore.sqlite3-wal",
                "clipstore.sqlite3-shm",
                "blobs",
            ]
            .iter()
            .enumerate()
            {
                let original = self.profile.join(name);
                match fs::symlink_metadata(&original) {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(CoreError::Io(error)),
                }
                restore_point!(RestoreFault::OldMove(index))?;
                let backup = self.old_dir.join(name);
                fs::rename(&original, &backup).map_err(CoreError::Io)?;
                moved.push((backup, original));
                let _ = index;
            }
            let has_incoming_config = matches!(fs::symlink_metadata(self.incoming.join("config.toml")), Ok(m) if m.is_file());
            if has_incoming_config {
                let name = "config.toml";
                let original = self.profile.join(name);
                match fs::symlink_metadata(&original) {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(CoreError::Io(error)),
                }
                if let Ok(m) = fs::symlink_metadata(&original) {
                    if m.file_type().is_symlink() {
                        return Err(CoreError::Unsupported {
                            message: format!(
                                "managed component is symlink: {}",
                                original.display()
                            ),
                        });
                    }
                    #[cfg(windows)]
                    {
                        use std::os::windows::fs::MetadataExt as _;
                        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                        if (m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                            return Err(CoreError::Unsupported {
                                message: format!(
                                    "managed component is reparse point: {}",
                                    original.display()
                                ),
                            });
                        }
                    }
                    if !m.is_file() {
                        return Err(CoreError::Unsupported {
                            message: format!(
                                "wrong managed component type: {}",
                                original.display()
                            ),
                        });
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt as _;
                        let anchor_dev = fs::metadata(&self.profile).map_err(CoreError::Io)?.dev();
                        if m.dev() != anchor_dev {
                            return Err(CoreError::Unsupported {
                                message: format!(
                                    "managed component crosses filesystem: {}",
                                    original.display()
                                ),
                            });
                        }
                    }
                    restore_point!(RestoreFault::OldMove(4))?;
                    let backup = self.old_dir.join(name);
                    fs::rename(&original, &backup).map_err(CoreError::Io)?;
                    moved.push((backup, original));
                }
            }
            for (index, name) in ["blobs", DB_NAME].iter().enumerate() {
                restore_point!(RestoreFault::NewMove(index))?;
                let target = self.profile.join(name);
                fs::rename(self.incoming.join(name), &target).map_err(CoreError::Io)?;
                installed.push(target);
                let _ = index;
            }
            if has_incoming_config {
                let name = "config.toml";
                restore_point!(RestoreFault::NewMove(2))?;
                let target = self.profile.join(name);
                fs::rename(self.incoming.join(name), &target).map_err(CoreError::Io)?;
                installed.push(target);
            }
            // Opening SQLite can create sidecars even when opening later fails.
            installed.push(self.profile.join("clipstore.sqlite3-wal"));
            installed.push(self.profile.join("clipstore.sqlite3-shm"));
            restore_point!(RestoreFault::NewOpen)?;
            Store::open(&self.profile)
        })();
        match install {
            Ok(store) => {
                fsync_dir(&self.profile);
                // Cleanup errors leave an owned recovery directory; never roll back
                // after a serving connection has been published to the caller.
                Ok(store)
            }
            Err(primary) => {
                if !had_database && moved.is_empty() && installed.is_empty() {
                    return Err(RestoreFailure {
                        error: primary,
                        store: None,
                        recovery_dir: None,
                    });
                }
                let rollback = Self::rollback_moves(&mut moved, &mut installed).and_then(|()| {
                    if !had_database {
                        return Ok(None);
                    }
                    restore_point!(RestoreFault::OldOpen)?;
                    Store::open(&self.profile).map(Some)
                });
                match rollback {
                    Ok(store) => Err(RestoreFailure {
                        error: primary,
                        store,
                        recovery_dir: None,
                    }),
                    Err(recovery) => {
                        self.cleanup = false;
                        Err(RestoreFailure {
                            error: CoreError::Backend { backend: "restore".to_string(), kind: BackendKind::Unavailable,
                                message: format!("restore failed: {primary}; recovery failed: {recovery}; recovery at {} (original components may also be in {})", self.stage_root.display(), self.profile.display()) },
                            store: None, recovery_dir: Some(self.stage_root.clone()),
                        })
                    }
                }
            }
        }
    }

    fn rollback_moves(
        moved: &mut Vec<(PathBuf, PathBuf)>,
        installed: &mut Vec<PathBuf>,
    ) -> Result<(), CoreError> {
        while let Some(path) = installed.last() {
            restore_point!(RestoreFault::RollbackRemove)?;
            match fs::symlink_metadata(path) {
                Ok(metadata) => {
                    if metadata.is_dir() {
                        fs::remove_dir_all(path)
                    } else {
                        fs::remove_file(path)
                    }
                    .map_err(CoreError::Io)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(CoreError::Io(error)),
            }
            installed.pop();
        }
        while let Some((backup, original)) = moved.last() {
            restore_point!(RestoreFault::RollbackRename)?;
            fs::rename(backup, original).map_err(CoreError::Io)?;
            moved.pop();
        }
        Ok(())
    }
}

/// `native_id` for passthrough formats, `None` for canonical ones.
fn native_id_of(mime: &str) -> Option<String> {
    if mime.starts_with("x-os/") || mime.starts_with("x-os:") {
        Some(mime.to_string())
    } else {
        None
    }
}

/// MIME strings of one clip's payload rows, in insertion order.
fn mime_list(conn: &Connection, id: &str) -> Result<Vec<String>, CoreError> {
    let mut stmt = conn
        .prepare("SELECT mime FROM clip_payloads WHERE clip_id=?1 ORDER BY rowid")
        .map_err(|e| db_err(&e))?;
    let mapped = stmt
        .query_map(params![id], |r| r.get(0))
        .map_err(|e| db_err(&e))?;
    mapped.collect::<Result<_, _>>().map_err(|e| db_err(&e))
}

/// Escape one LIKE token (`%`, `_`, `\`).
fn escape_like(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    for c in token.chars() {
        if c == '%' || c == '_' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// IDs of every removed clip.
fn removed_ids(conn: &Connection) -> Result<Vec<String>, CoreError> {
    let mut stmt = conn
        .prepare("SELECT id FROM clips WHERE removed=1")
        .map_err(|e| db_err(&e))?;
    let mapped = stmt.query_map([], |r| r.get(0)).map_err(|e| db_err(&e))?;
    mapped.collect::<Result<_, _>>().map_err(|e| db_err(&e))
}

/// Delete clip and FTS rows; the caller sweeps after all SQL deletions.
fn delete_clips(conn: &Connection, ids: &[String]) -> Result<(), CoreError> {
    for id in ids {
        conn.execute("DELETE FROM clips_fts WHERE clip_id=?1", params![id])
            .map_err(|e| db_err(&e))?;
        conn.execute("DELETE FROM clips WHERE id=?1", params![id])
            .map_err(|e| db_err(&e))?;
    }
    Ok(())
}

/// Delete blob rows no payload references, with their files.
fn sweep_blobs(conn: &Connection, blobs: &Path) -> Result<(), CoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT blob_key FROM clip_blobs WHERE blob_key NOT IN \
            (SELECT blob_key FROM clip_payloads WHERE blob_key IS NOT NULL)",
        )
        .map_err(|e| db_err(&e))?;
    let keys: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .map_err(|e| db_err(&e))?
        .collect::<Result<_, _>>()
        .map_err(|e| db_err(&e))?;
    drop(stmt);
    for k in &keys {
        parse_blob_key(k).ok_or_else(|| CoreError::Corrupt {
            message: "invalid blob key".to_string(),
        })?;
    }
    blobs_root_ok(blobs)?;
    let mut pending: Vec<(String, PathBuf, bool)> = Vec::with_capacity(keys.len());
    for k in &keys {
        let path = safe_blob_path(blobs, k)?;
        let exists = fs::symlink_metadata(&path).is_ok();
        pending.push((k.clone(), path, exists));
    }
    for (k, path, exists) in pending {
        conn.execute("DELETE FROM clip_blobs WHERE blob_key=?1", params![k])
            .map_err(|e| db_err(&e))?;
        if exists {
            let m = fs::symlink_metadata(&path).map_err(CoreError::Io)?;
            if m.file_type().is_symlink() || !m.is_file() {
                continue;
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                if (m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                    continue;
                }
            }
            fs::remove_file(&path).map_err(CoreError::Io)?;
        }
    }
    Ok(())
}

/// Keys referenced by all external payload rows, including removed clips.
fn referenced_keys(conn: &Connection) -> Result<HashSet<String>, CoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT blob_key FROM clip_payloads WHERE external=1 AND blob_key IS NOT NULL",
        )
        .map_err(|e| db_err(&e))?;
    let mapped = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| db_err(&e))?;
    mapped.collect::<Result<_, _>>().map_err(|e| db_err(&e))
}

/// Delete oldest unpinned clips while the live count exceeds `max`.
fn enforce_count(tx: &rusqlite::Transaction<'_>, max: u64) -> Result<u64, CoreError> {
    let live: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM clips WHERE removed=0 AND pinned=0",
            [],
            |r| r.get(0),
        )
        .map_err(|e| db_err(&e))?;
    let live_u = u64::try_from(live.max(0)).unwrap_or(u64::MAX);
    if live_u <= max {
        return Ok(0);
    }
    let mut stmt = tx
        .prepare("SELECT id FROM clips WHERE removed=0 AND pinned=0 ORDER BY last_used_ms ASC, created_ms ASC LIMIT ?1")
        .map_err(|e| db_err(&e))?;
    let ids: Vec<String> = stmt
        .query_map(params![i64_from_u64(live_u - max)?], |r| r.get(0))
        .map_err(|e| db_err(&e))?
        .collect::<Result<_, _>>()
        .map_err(|e| db_err(&e))?;
    drop(stmt);
    let n = ids.len() as u64;
    delete_clips(tx, &ids)?;
    Ok(n)
}

/// Delete oldest unpinned clips older than `max_age_days` (0 = off).
fn enforce_age(
    tx: &rusqlite::Transaction<'_>,
    max_age_days: u64,
    now: i64,
) -> Result<u64, CoreError> {
    if max_age_days == 0 {
        return Ok(0);
    }
    let cutoff = now.saturating_sub(i64_from_u64(max_age_days)?.saturating_mul(86_400_000));
    let mut stmt = tx
        .prepare("SELECT id FROM clips WHERE removed=0 AND pinned=0 AND last_used_ms < ?1 ORDER BY last_used_ms ASC")
        .map_err(|e| db_err(&e))?;
    let ids: Vec<String> = stmt
        .query_map(params![cutoff], |r| r.get(0))
        .map_err(|e| db_err(&e))?
        .collect::<Result<_, _>>()
        .map_err(|e| db_err(&e))?;
    drop(stmt);
    let n = ids.len() as u64;
    delete_clips(tx, &ids)?;
    Ok(n)
}

/// Delete oldest unpinned clips while logical bytes exceed `max`.
fn enforce_bytes(tx: &rusqlite::Transaction<'_>, max: u64) -> Result<u64, CoreError> {
    let total_i: i64 = tx
        .query_row(
            "SELECT COALESCE(SUM(bytes_total),0) FROM clips WHERE removed=0",
            [],
            |r| r.get(0),
        )
        .map_err(|e| db_err(&e))?;
    let mut total = u128::try_from(total_i.max(0)).unwrap_or(u128::MAX);
    let cap = u128::from(max);
    let mut purged = 0_u64;
    while total > cap {
        let id: Option<String> = tx
            .query_row(
                "SELECT id FROM clips WHERE removed=0 AND pinned=0 ORDER BY last_used_ms ASC, created_ms ASC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .map_err(|e| db_err(&e))?;
        let Some(id) = id else { break };
        let bytes_i: i64 = tx
            .query_row(
                "SELECT bytes_total FROM clips WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .map_err(|e| db_err(&e))?;
        delete_clips(tx, std::slice::from_ref(&id))?;
        total = total.saturating_sub(u128::try_from(bytes_i.max(0)).unwrap_or(u128::MAX));
        purged += 1;
    }
    Ok(purged)
}

/// Reject symlinks via file type bits 0o12 in the Unix mode `zip` reports.
fn is_symlink_entry<R: std::io::Read + std::io::Seek>(e: &zip::read::ZipFile<'_, R>) -> bool {
    e.unix_mode().is_some_and(|m| m >> 12 == 0o12)
}

/// FTS5 search; `None` on any failure so callers fall back to LIKE.
fn fts_search(conn: &Connection, match_q: &str) -> Option<Vec<Clip>> {
    let sql = format!(
        "SELECT {CLIP_COLS} FROM clips JOIN clips_fts ON clips_fts.clip_id=clips.id \
        WHERE clips.removed=0 AND clips_fts MATCH ?1 ORDER BY clips.last_used_ms DESC"
    );
    let mut stmt = conn.prepare(&sql).ok()?;
    let mapped = stmt.query_map(params![match_q], clip_of).ok()?;
    mapped.collect::<Result<Vec<_>, _>>().ok()
}

/// Rejection error for unsafe or oversize restore entries.
fn unsupported(msg: &str) -> CoreError {
    CoreError::Unsupported {
        message: msg.to_string(),
    }
}

/// Validate every ZIP name, then extract into `tmp` (profile untouched).
fn extract_zip(src: &Path, tmp: &Path) -> Result<(), CoreError> {
    let file = fs::File::open(src).map_err(CoreError::Io)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| zip_err(&e))?;
    let mut indices = Vec::with_capacity(zip.len());
    let mut total = 0_u64;
    for i in 0..zip.len() {
        let e = zip.by_index(i).map_err(|e| zip_err(&e))?;
        let name = e.name().map_err(|e| zip_err(&e))?;
        let bytes = name.as_bytes();
        if name.starts_with(['/', '\\'])
            || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        {
            return Err(unsupported("zip absolute or drive-prefixed entry rejected"));
        }
        e.enclosed_name()
            .ok_or_else(|| unsupported("zip entry escapes target directory"))?;
        if is_symlink_entry(&e) {
            return Err(unsupported("zip symlink entry rejected"));
        }
        if !e.is_dir() {
            if e.size() > RESTORE_MAX_ENTRY {
                return Err(unsupported("zip entry exceeds size cap"));
            }
            total = total.saturating_add(e.size());
            if total > RESTORE_MAX_TOTAL {
                return Err(unsupported("zip archive exceeds size cap"));
            }
            indices.push(i);
        }
    }
    for i in indices {
        let mut e = zip.by_index(i).map_err(|e| zip_err(&e))?;
        let out = tmp.join(
            e.enclosed_name()
                .ok_or_else(|| unsupported("zip entry escapes target directory"))?,
        );
        if let Some(parent) = out.parent() {
            mkdir(parent)?;
        }
        let mut out_f = fs::File::create(&out).map_err(CoreError::Io)?;
        let mut take = (&mut e).take(RESTORE_MAX_ENTRY + 1);
        let n = std::io::copy(&mut take, &mut out_f).map_err(CoreError::Io)?;
        if n > RESTORE_MAX_ENTRY {
            return Err(unsupported("zip entry exceeds size cap"));
        }
    }
    Ok(())
}

/// Preflight every manifest blob before reading any blob data.
fn validated_manifest_blobs(man: &toml::Value) -> Result<Vec<(String, u64, String)>, CoreError> {
    let bad = |msg: &str| CoreError::Config {
        message: msg.to_string(),
    };
    let Some(value) = man.get("blobs") else {
        return Ok(Vec::new());
    };
    let blobs = value
        .as_array()
        .ok_or_else(|| bad("restore manifest blobs not an array"))?;
    let mut out = Vec::with_capacity(blobs.len());
    let mut keys = HashSet::with_capacity(blobs.len());
    for blob in blobs {
        let key = blob
            .get("key")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| bad("restore manifest blob key missing or not a string"))?;
        let (prefix, size) =
            parse_blob_key(key).ok_or_else(|| bad("restore manifest invalid blob key"))?;
        if !keys.insert(key) {
            return Err(bad("restore manifest duplicate blob key"));
        }
        if size > RESTORE_MAX_ENTRY {
            return Err(bad("restore blob exceeds size cap"));
        }
        if let Some(value) = blob.get("bytes") {
            let bytes = value
                .as_integer()
                .ok_or_else(|| bad("restore manifest blob bytes not an integer"))?;
            if u64::try_from(bytes).ok() != Some(size) {
                return Err(bad("restore blob size mismatch"));
            }
        }
        let sha = if let Some(value) = blob.get("sha256") {
            let sha = value
                .as_str()
                .ok_or_else(|| bad("restore manifest blob sha256 not a string"))?;
            if !is_canonical_hex(sha, 64) {
                return Err(bad("restore blob sha256 invalid"));
            }
            if !sha.starts_with(&prefix) {
                return Err(bad("restore blob sha prefix mismatch"));
            }
            sha.to_string()
        } else {
            String::new()
        };
        if blob.get("db_sha256").is_some_and(|v| !v.is_str()) {
            return Err(bad("restore manifest db_sha256 not a string"));
        }
        out.push((key.to_string(), size, sha));
    }
    Ok(out)
}

/// Validate an extracted profile: manifest, checksums, DB, blobs.
fn validate_profile(tmp: &Path) -> Result<(), CoreError> {
    let bad = |msg: &str| CoreError::Config {
        message: msg.to_string(),
    };
    let text = fs::read_to_string(tmp.join("manifest.toml"))
        .map_err(|_| bad("restore manifest missing"))?;
    let man: toml::Value =
        toml::from_str(&text).map_err(|e| bad(&format!("restore manifest invalid: {e}")))?;
    let ver = man
        .get("schema_version")
        .and_then(toml::Value::as_integer)
        .ok_or_else(|| bad("restore manifest missing schema_version"))?;
    if ver != 1 {
        return Err(bad("restore manifest has unsupported schema_version"));
    }
    if let Some(v) = man.get("db_sha256") {
        if !v.is_str() {
            return Err(bad("restore manifest db_sha256 not a string"));
        }
    }

    let db = tmp.join("clipstore.sqlite3");
    if !db.is_file() {
        return Err(bad("restore archive missing clipstore.sqlite3"));
    }
    let bytes = fs::read(&db).map_err(CoreError::Io)?;
    if let Some(value) = man.get("db_bytes") {
        let expected = value
            .as_integer()
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| bad("restore manifest db_bytes not a nonnegative integer"))?;
        if u64::try_from(bytes.len()).ok() != Some(expected) {
            return Err(bad("restore database db_bytes mismatch"));
        }
    }
    if let Some(sha) = man.get("db_sha256").and_then(toml::Value::as_str) {
        if hex_of(&Sha256::digest(&bytes)) != sha {
            return Err(bad("restore database checksum mismatch"));
        }
    }
    let probe = open_conn(&db, true)?;
    let v: i64 = probe
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| db_err(&e))?;
    if v != SCHEMA_VERSION {
        return Err(bad("restore database has unsupported user_version"));
    }
    if !quick_ok(&probe) {
        return Err(CoreError::Corrupt {
            message: "restore database failed quick_check".to_string(),
        });
    }
    validate_db_keys(&probe)?;
    let manifest_blobs = validated_manifest_blobs(&man)?;
    let referenced = referenced_keys(&probe)?;
    let manifest_keys: HashSet<_> = manifest_blobs
        .iter()
        .map(|(key, _, _)| key.clone())
        .collect();
    if manifest_keys != referenced {
        return Err(bad("restore manifest blob reference set mismatch"));
    }
    let blobs_root = tmp.join("blobs");
    for (key, _, manifest_sha) in &manifest_blobs {
        let db_sha: String = probe
            .query_row(
                "SELECT sha256 FROM clip_blobs WHERE blob_key=?1",
                params![key],
                |r| r.get(0),
            )
            .map_err(|e| db_err(&e))?;
        let bytes = safe_blob_read(&blobs_root, key, &db_sha)?;
        if !manifest_sha.is_empty() && hex_of(&Sha256::digest(&bytes)) != *manifest_sha {
            return Err(bad("restore blob checksum mismatch"));
        }
    }
    validate_config_in_profile(tmp, &man)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{escape_like, InsertOutcome, Store};
    use crate::model::{self, Clip};
    use crate::policy::Limits;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Barrier;
    use uuid::Uuid;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Unique scratch dir under the OS temp dir.
    fn unique_dir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("cm-self-{tag}-{}-{n}", std::process::id()));
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
            origin: "test".to_string(),
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

    fn user_version(path: &Path) -> i64 {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    }

    fn history_ids(store: &Store) -> Vec<Uuid> {
        store
            .list_history(10_000, 0, false)
            .unwrap()
            .iter()
            .map(|c| c.id)
            .collect()
    }

    /// Blob key shape follows `sha256hex[0..32]-<bytes>`.
    #[test]
    fn blob_key_shape() {
        let (key, sha) = Store::blob_key_for(b"abc");
        assert_eq!(
            sha,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(key.starts_with(&sha[..32]));
        assert!(key.ends_with("-3"));
    }

    /// LIKE metacharacters are escaped.
    #[test]
    fn like_escape() {
        assert_eq!(escape_like("100%_\\"), "100\\%\\_\\\\");
    }

    /// Schema tables, indexes, `user_version`, and meta rows on fresh create.
    #[test]
    fn self_schema_create() {
        let profile = unique_dir("schema");
        let _store = Store::open(&profile).unwrap();
        let path = db_path(&profile);
        assert!(path.exists());
        let conn = rusqlite::Connection::open(&path).unwrap();
        let names: HashSet<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type IN ('table','index')")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        for t in [
            "meta",
            "clips",
            "clip_payloads",
            "clip_blobs",
            "perm_groups",
            "permanents",
            "clips_fts",
            "idx_clips_created",
            "idx_clips_content",
        ] {
            assert!(names.contains(t), "missing {t}; have {names:?}");
        }
        // FTS5 virtual-table shadows prove the FTS branch (not LIKE fallback).
        assert!(
            names.contains("clips_fts_data"),
            "missing FTS5 shadows; have {names:?}"
        );
        assert_eq!(user_version(&path), 1);
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    /// Downgrade refusal names versions and writes zero bytes.
    #[test]
    fn self_downgrade_refused() {
        let profile = unique_dir("downgrade");
        let store = Store::open(&profile).unwrap();
        let clip = sample_clip(Uuid::now_v7(), "v1", "v1", 1_700_000_000_400);
        let _ = store.insert_clip(&clip, &text_payloads("v1")).unwrap();
        drop(store);
        let path = db_path(&profile);
        let current = user_version(&path);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(&format!("PRAGMA user_version={}", current + 1))
            .unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        let err = Store::open(&profile).expect_err("newer schema must be refused");
        match err {
            crate::CoreError::DowngradeRefused { found, expected } => {
                assert_eq!(found, current + 1);
                assert_eq!(expected, current);
            }
            other => panic!("expected DowngradeRefused, got {other:?}"),
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    /// Older version migrates with rows preserved.
    #[test]
    fn self_migration_preserves_rows() {
        let profile = unique_dir("migrate");
        let store = Store::open(&profile).unwrap();
        let sentinel = sample_clip(Uuid::now_v7(), "s", "s", 1_700_000_000_200);
        let _ = store.insert_clip(&sentinel, &text_payloads("s")).unwrap();
        drop(store);
        let path = db_path(&profile);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA user_version=0").unwrap();
        drop(conn);
        let reopened = Store::open(&profile).unwrap();
        assert!(history_ids(&reopened).contains(&sentinel.id));
        assert!(user_version(&path) > 0);
    }

    /// True corruption is preserved aside and reported.
    #[test]
    fn self_corruption_aside() {
        let profile = unique_dir("corrupt");
        let store = Store::open(&profile).unwrap();
        drop(store);
        let path = db_path(&profile);
        std::fs::write(&path, vec![0xFFu8; 4096]).unwrap();
        let err = Store::open(&profile).expect_err("corrupt DB must fail open");
        assert_eq!(err.code(), "corrupt");
        let mut asides = Vec::new();
        for e in std::fs::read_dir(&profile).unwrap().flatten() {
            if e.file_name().to_string_lossy().contains(".corrupt-") {
                asides.push(e.path());
            }
        }
        assert_eq!(asides.len(), 1);
        assert_eq!(std::fs::read(&asides[0]).unwrap(), vec![0xFFu8; 4096]);
    }

    /// Blob boundary, sha verification, dedupe, retention, purge, order, privacy.
    #[test]
    fn self_store_behaviors() {
        const KIB256: usize = 256 * 1024;
        let profile = unique_dir("behaviors");
        let store = Store::open(&profile).unwrap();
        let path = db_path(&profile);
        // Boundary: exactly 256 KiB inline.
        let small = sample_clip(Uuid::now_v7(), "small", "small", 1_700_000_001_000);
        let small_bytes = vec![b'a'; KIB256];
        let _ = store
            .insert_clip(&small, &[("text/plain".to_string(), small_bytes.clone())])
            .unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        let (ext, key): (i64, Option<String>) = conn
            .query_row(
                "SELECT external, blob_key FROM clip_payloads WHERE clip_id=?1",
                [small.id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(ext, 0);
        assert!(key.is_none());
        drop(conn);
        // 256 KiB + 1 external with blob file.
        let big = sample_clip(Uuid::now_v7(), "big", "big", 1_700_000_001_001);
        let big_bytes = vec![b'b'; KIB256 + 1];
        let _ = store
            .insert_clip(&big, &[("text/plain".to_string(), big_bytes.clone())])
            .unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        let (ext_big, key_big): (i64, Option<String>) = conn
            .query_row(
                "SELECT external, blob_key FROM clip_payloads WHERE clip_id=?1",
                [big.id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(ext_big, 1);
        let key_big = key_big.unwrap();
        assert!(profile.join("blobs").join(&key_big).exists());
        drop(conn);
        let (_, got_small) = store.get_clip(small.id).unwrap().unwrap();
        assert_eq!(got_small[0].1, small_bytes);
        let (_, got_big) = store.get_clip(big.id).unwrap().unwrap();
        assert_eq!(got_big[0].1, big_bytes);
        // Tamper is not readable.
        let blob = profile.join("blobs").join(&key_big);
        let mut raw = std::fs::read(&blob).unwrap();
        raw[0] ^= 0xFF;
        std::fs::write(&blob, raw).unwrap();
        match store.get_clip(big.id) {
            Err(_) | Ok(None) => {}
            Ok(Some(_)) => panic!("tampered blob must not read back"),
        }
        // Dedupe: same bytes move the first row to top.
        let first = sample_clip(Uuid::now_v7(), "first", "same-content", 1_700_000_002_000);
        let _ = store
            .insert_clip(&first, &text_payloads("same-content"))
            .unwrap();
        let other = sample_clip(Uuid::now_v7(), "other", "other", 1_700_000_002_001);
        let _ = store.insert_clip(&other, &text_payloads("other")).unwrap();
        let dupe = sample_clip(Uuid::now_v7(), "first", "same-content", 1_700_000_002_002);
        match store
            .insert_clip(&dupe, &text_payloads("same-content"))
            .unwrap()
        {
            InsertOutcome::MovedToTop(id) => assert_eq!(id, first.id),
            other => panic!("expected MovedToTop, got {other:?}"),
        }
        // Retention tested on a fresh profile below; purge here.
        let _ = store.soft_delete(other.id);
        assert!(!history_ids(&store).contains(&other.id));
        assert_eq!(store.purge_removed(), 1);
        // Privacy skips create no rows and bump the counter.
        let conn = rusqlite::Connection::open(&path).unwrap();
        let before: i64 = conn
            .query_row("SELECT v FROM meta WHERE k='privacy_skips'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
            .parse()
            .unwrap();
        drop(conn);
        let rows_before = store.list_history(10_000, 0, true).unwrap().len();
        store.record_privacy_skip().unwrap();
        store.record_privacy_skip().unwrap();
        assert_eq!(
            store.list_history(10_000, 0, true).unwrap().len(),
            rows_before
        );
        let conn = rusqlite::Connection::open(&path).unwrap();
        let after: i64 = conn
            .query_row("SELECT v FROM meta WHERE k='privacy_skips'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(after, before + 2);
    }

    /// Pinned clips survive count-cap retention; oldest unpinned purges first.
    #[test]
    fn self_retention_pinned_exempt() {
        let profile = unique_dir("retention");
        let store = Store::open(&profile).unwrap();
        let mut pinned = sample_clip(Uuid::now_v7(), "pinned", "pinned", 1_700_000_003_000);
        pinned.pinned = true;
        let _ = store
            .insert_clip(&pinned, &text_payloads("pinned"))
            .unwrap();
        let mut oldest = Uuid::nil();
        for i in 0..5 {
            let c = sample_clip(
                Uuid::now_v7(),
                &format!("c{i}"),
                &format!("c{i}"),
                1_700_000_003_001 + i,
            );
            if i == 0 {
                oldest = c.id;
            }
            let t = format!("c{i}");
            let _ = store.insert_clip(&c, &text_payloads(&t)).unwrap();
        }
        let limits = Limits {
            max_items: 3,
            max_store_bytes: u64::MAX,
            max_age_days: 0,
        };
        let _ = store.enforce_retention(&limits);
        let ids = history_ids(&store);
        assert!(ids.contains(&pinned.id));
        assert!(!ids.contains(&oldest));
        assert!(ids.len() <= 4);
    }

    /// List order, formats passthrough, and FTS/LIKE parity.
    #[test]
    fn self_list_search_parity() {
        let profile = unique_dir("search");
        let store = Store::open(&profile).unwrap();
        let body = "width independent quarterly report kiwi";
        for (i, width) in [20usize, 80, usize::MAX].iter().enumerate() {
            let preview: String = body.chars().take(*width).collect();
            let clip = sample_clip(
                Uuid::now_v7(),
                &preview,
                body,
                1_700_000_010_000 + i64::try_from(i).unwrap(),
            );
            let _ = store.insert_clip(&clip, &text_payloads(body)).unwrap();
        }
        let extra = sample_clip(
            Uuid::now_v7(),
            "unrelated",
            "unrelated zebra",
            1_700_000_010_010,
        );
        let _ = store
            .insert_clip(&extra, &text_payloads("unrelated zebra"))
            .unwrap();
        for q in ["kiwi", "quarterly", "report", "zebra", "missing-token-xyz"] {
            let fts: HashSet<Uuid> = store.search(q).unwrap().iter().map(|c| c.id).collect();
            let like: HashSet<Uuid> = store
                .list_history(10_000, 0, false)
                .unwrap()
                .iter()
                .filter(|c| c.search_text.to_lowercase().contains(&q.to_lowercase()))
                .map(|c| c.id)
                .collect();
            assert_eq!(fts, like, "parity for {q:?}");
        }
        // Identical payloads dedupe per D-F10, so one row answers `kiwi`.
        assert_eq!(store.search("kiwi").unwrap().len(), 1);
        // Formats passthrough incl. native mime.
        let new = sample_clip(Uuid::now_v7(), "new", "new", 1_700_000_011_000);
        let payloads = vec![
            ("text/plain;charset=utf-8".to_string(), b"new".to_vec()),
            ("x-os/win:HTML Format".to_string(), b"native".to_vec()),
        ];
        let _ = store.insert_clip(&new, &payloads).unwrap();
        let list = store.list_history(10, 0, false).unwrap();
        assert_eq!(list[0].id, new.id);
        assert!(list[0]
            .formats
            .contains(&"x-os/win:HTML Format".to_string()));
        store.integrity_full().unwrap();
    }

    /// Backup roundtrip with an external blob; source untouched.
    #[test]
    fn self_backup_restore_roundtrip() {
        const KIB256: usize = 256 * 1024;
        let src = unique_dir("backup-src");
        let store = Store::open(&src).unwrap();
        let inline = sample_clip(Uuid::now_v7(), "inline", "inline backup", 1_700_000_006_000);
        let inline_p = text_payloads("inline backup body");
        let _ = store.insert_clip(&inline, &inline_p).unwrap();
        let big = sample_clip(Uuid::now_v7(), "big", "external backup", 1_700_000_006_001);
        let big_p = vec![("image/png".to_string(), vec![7u8; KIB256 + 64])];
        let _ = store.insert_clip(&big, &big_p).unwrap();
        let zip_path = src.join("backup.zip");
        store.backup_zip(&zip_path).unwrap();
        assert!(zip_path.exists());
        let dst = unique_dir("backup-dst");
        Store::restore_zip(&zip_path, &dst).unwrap();
        let restored = Store::open(&dst).unwrap();
        for (clip, payloads) in [(&inline, &inline_p), (&big, &big_p)] {
            let (_, got) = restored
                .get_clip(clip.id)
                .unwrap()
                .unwrap_or_else(|| panic!("missing {}", clip.id));
            assert_eq!(got.len(), payloads.len());
            for ((em, eb), (am, ab)) in payloads.iter().zip(got.iter()) {
                assert_eq!(em, am);
                assert_eq!(eb, ab);
            }
        }
        assert_eq!(history_ids(&store).len(), 2);
    }

    /// Restore rejects traversal, symlink, and bad-manifest fixtures.
    #[test]
    fn self_restore_rejections() {
        let profile = unique_dir("restore-rej");
        let store = Store::open(&profile).unwrap();
        let keep = sample_clip(Uuid::now_v7(), "keep", "keep", 1_700_000_009_000);
        let _ = store.insert_clip(&keep, &text_payloads("keep")).unwrap();
        drop(store);
        let evil = unique_dir("restore-evil").join("evil.zip");
        {
            let file = std::fs::File::create(&evil).unwrap();
            let mut w = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file("../evil.txt", opts).unwrap();
            std::io::Write::write_all(&mut w, b"escape").unwrap();
            w.start_file("manifest.toml", opts).unwrap();
            std::io::Write::write_all(&mut w, b"schema_version = 1").unwrap();
            w.finish().unwrap();
        }
        assert!(Store::restore_zip(&evil, &profile).is_err());
        let bad = unique_dir("restore-bad").join("bad.zip");
        {
            let file = std::fs::File::create(&bad).unwrap();
            let mut w = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file("manifest.toml", opts).unwrap();
            std::io::Write::write_all(&mut w, b"schema_version = 999\nchecksum = \"deadbeef\"")
                .unwrap();
            w.finish().unwrap();
        }
        assert!(Store::restore_zip(&bad, &profile).is_err());
        let store2 = Store::open(&profile).unwrap();
        assert_eq!(history_ids(&store2), vec![keep.id]);
    }

    /// GC quarantines planted orphans and reports dangling rows.
    #[test]
    fn self_gc_quarantine() {
        const KIB256: usize = 256 * 1024;
        let profile = unique_dir("gc");
        let store = Store::open(&profile).unwrap();
        let live = sample_clip(Uuid::now_v7(), "live", "live", 1_700_000_008_000);
        let _ = store
            .insert_clip(&live, &[("image/png".to_string(), vec![9u8; KIB256 + 8])])
            .unwrap();
        let orphan_bytes = b"orphan-fixture-bytes".to_vec();
        std::fs::write(profile.join("blobs").join("planted-orphan"), &orphan_bytes).unwrap();
        let path = profile.join("clipstore.sqlite3");
        let conn = rusqlite::Connection::open(&path).unwrap();
        let key: String = conn
            .query_row(
                "SELECT blob_key FROM clip_payloads WHERE clip_id=?1",
                [live.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        drop(conn);
        std::fs::remove_file(profile.join("blobs").join(&key)).unwrap();
        let _report = store.gc_blobs(1_700_000_008_100);
        assert!(!profile.join("blobs").join("planted-orphan").exists());
        let mut found = false;
        let mut stack = vec![profile.clone()];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if std::fs::read(&p).unwrap() == orphan_bytes {
                    assert!(p.to_string_lossy().contains("orphan-"));
                    found = true;
                }
            }
        }
        assert!(found, "orphan bytes must survive in quarantine");
        assert!(history_ids(&store).contains(&live.id));
    }

    /// Concurrent writers and readers share one profile via WAL.
    #[test]
    fn self_concurrent_burst() {
        let profile = unique_dir("burst");
        let writers = 4usize;
        let per_writer = 10usize;
        let readers = 2usize;
        let start = Barrier::new(writers + readers);
        std::thread::scope(|s| {
            let mut handles = Vec::new();
            let profile = &profile;
            let start = &start;
            for w in 0..writers {
                handles.push(s.spawn(move || {
                    start.wait();
                    let store = Store::open(profile).expect("writer open");
                    for i in 0..per_writer {
                        let text = format!("w{w}-i{i}-unique-padding-{}", Uuid::now_v7());
                        let at = 1_700_000_007_000 + i64::try_from(w * per_writer + i).unwrap();
                        let clip = sample_clip(Uuid::now_v7(), &text, &text, at);
                        store
                            .insert_clip(&clip, &text_payloads(&text))
                            .expect("burst insert");
                    }
                }));
            }
            for _ in 0..readers {
                handles.push(s.spawn(|| {
                    start.wait();
                    let store = Store::open(profile).expect("reader open");
                    let mut seen = 0usize;
                    for _ in 0..(writers * per_writer) {
                        let rows = store.list_history(10_000, 0, false).expect("read");
                        assert!(rows.len() >= seen);
                        seen = rows.len();
                        if seen == writers * per_writer {
                            break;
                        }
                    }
                }));
            }
            for h in handles {
                h.join().expect("thread");
            }
        });
        let store = Store::open(&profile).unwrap();
        assert_eq!(history_ids(&store).len(), writers * per_writer);
    }
}

#[cfg(test)]
#[path = "restore_tests.rs"]
mod restore_tests;

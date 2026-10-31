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

/// Stored payload row: MIME, external flag, inline bytes, blob key.
type StoredPayload = (String, i64, Option<Vec<u8>>, Option<String>);

/// Payload row joined with its blob digest for verification.
type VerifiedPayload = (String, i64, Option<Vec<u8>>, Option<String>, Option<String>);

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

/// SQLite-backed clip store.
#[derive(Debug)]
pub struct Store {
    /// Path to `clipstore.sqlite3`.
    db_path: PathBuf,
    /// Path to `blobs/`.
    blobs: PathBuf,
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
        mkdir(&blobs)?;
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
        let fts = probe_fts(&conn);
        Ok(Self {
            db_path,
            blobs,
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
        let fts = probe_fts(&conn);
        Ok(Self {
            db_path: db_path.to_path_buf(),
            blobs: blobs.to_path_buf(),
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
        let final_path = self.blobs.join(key);
        if final_path.is_file() {
            return Ok(());
        }
        let tmp = self.blobs.join(format!(
            ".tmp-{key}-{}-{}",
            std::process::id(),
            TMP_CTR.fetch_add(1, Ordering::SeqCst)
        ));
        fs::write(&tmp, bytes).map_err(CoreError::Io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut perms = fs::metadata(&tmp).map_err(CoreError::Io)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&tmp, perms).map_err(CoreError::Io)?;
        }
        // Write access: Windows refuses to flush a read-only handle.
        let f = fs::OpenOptions::new()
            .write(true)
            .open(&tmp)
            .map_err(CoreError::Io)?;
        f.sync_all().map_err(CoreError::Io)?;
        drop(f);
        fs::rename(&tmp, &final_path).map_err(CoreError::Io)?;
        fsync_dir(&self.blobs);
        Ok(())
    }

    /// Read one external blob and verify its digest.
    fn read_verified(&self, key: &str, expect_sha: &str) -> Result<Vec<u8>, CoreError> {
        let bytes = fs::read(self.blobs.join(key)).map_err(|_| CoreError::Corrupt {
            message: format!("missing external blob {key}"),
        })?;
        let mut h = Sha256::new();
        h.update(&bytes);
        if hex_of(&h.finalize()) != expect_sha {
            return Err(CoreError::Corrupt {
                message: format!("external blob {key} failed verification"),
            });
        }
        Ok(bytes)
    }

    /// Best-effort payload load for dedupe hashing (`None` on any failure).
    fn try_load_payloads(conn: &Connection, blobs: &Path, id: &str) -> Option<ClipPayloads> {
        let mut stmt = conn.prepare(
            "SELECT mime, external, data, blob_key FROM clip_payloads WHERE clip_id=?1 ORDER BY rowid",
        ).ok()?;
        let rows: Vec<StoredPayload> = stmt
            .query_map(params![id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .ok()?
            .collect::<Result<_, _>>()
            .ok()?;
        let mut out = Vec::with_capacity(rows.len());
        for (mime, ext, data, key) in rows {
            if ext != 0 {
                let bytes = fs::read(blobs.join(key?)).ok()?;
                out.push((mime, bytes));
            } else {
                out.push((mime, data.unwrap_or_default()));
            }
        }
        Some(out)
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
                "SELECT p.mime, p.external, p.data, p.blob_key, b.sha256 FROM clip_payloads p \
                LEFT JOIN clip_blobs b ON b.blob_key=p.blob_key WHERE p.clip_id=?1 ORDER BY p.rowid",
            )
            .map_err(|e| db_err(&e))?;
        let mapped = stmt
            .query_map(params![id.to_string()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .map_err(|e| db_err(&e))?;
        let rows: Vec<VerifiedPayload> =
            mapped.collect::<Result<_, _>>().map_err(|e| db_err(&e))?;
        drop(stmt);
        let mut out = Vec::with_capacity(rows.len());
        for (mime, ext, data, key, sha) in rows {
            if ext != 0 {
                let k = key.ok_or_else(|| CoreError::Corrupt {
                    message: "dangling external payload".to_string(),
                })?;
                let s = sha.ok_or_else(|| CoreError::Corrupt {
                    message: "dangling external payload".to_string(),
                })?;
                out.push((mime, self.read_verified(&k, &s)?));
            } else {
                out.push((mime, data.unwrap_or_default()));
            }
        }
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
        let ids = removed_ids(&tx)?;
        let n = ids.len() as u64;
        delete_clips(&tx, &self.blobs, &ids)?;
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
        let mut purged = 0_u64;
        purged += enforce_count(&tx, &self.blobs, limits.max_items)?;
        purged += enforce_age(&tx, &self.blobs, limits.max_age_days, now_ms())?;
        purged += enforce_bytes(&tx, &self.blobs, limits.max_store_bytes)?;
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
        mkdir(&quarantine_dir)?;
        let mut orphaned_files = 0_u64;
        let entries = fs::read_dir(&self.blobs).map_err(CoreError::Io)?;
        for e in entries {
            let e = e.map_err(CoreError::Io)?;
            if !e.file_type().map_err(CoreError::Io)?.is_file() {
                continue;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            if referenced.contains(name.as_str()) {
                continue;
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
    /// Returns a backend or I/O error when the backup cannot be written.
    pub fn backup_zip(&self, dest: &Path) -> Result<(), CoreError> {
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                mkdir(parent)?;
            }
        }
        let tag = TMP_CTR.fetch_add(1, Ordering::SeqCst);
        let tmp = std::env::temp_dir().join(format!("cm-backup-{}-{tag}", std::process::id()));
        mkdir(&tmp)?;
        let _guard = TmpGuard { path: tmp.clone() };
        let tmp_db = tmp.join("clipstore.sqlite3");
        {
            let src = open_conn(&self.db_path, true)?;
            let mut out_db = Connection::open(&tmp_db).map_err(|e| db_err(&e))?;
            let backup =
                rusqlite::backup::Backup::new(&src, &mut out_db).map_err(|e| db_err(&e))?;
            backup.step(-1).map_err(|e| db_err(&e))?;
        }
        let conn = self.lock();
        let clips: i64 = conn
            .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
            .map_err(|e| db_err(&e))?;
        let referenced = referenced_keys(&conn)?;
        drop(conn);
        let db_bytes = fs::read(&tmp_db).map_err(CoreError::Io)?;
        let db_sha = hex_of(&Sha256::digest(&db_bytes));
        let tmp_blobs = tmp.join("blobs");
        mkdir(&tmp_blobs)?;
        let mut blob_entries = Vec::new();
        for key in &referenced {
            let Ok(bytes) = fs::read(self.blobs.join(key)) else {
                continue;
            };
            fs::write(tmp_blobs.join(key), &bytes).map_err(CoreError::Io)?;
            let sha = hex_of(&Sha256::digest(&bytes));
            blob_entries.push(format!(
                "{{key={key:?},bytes={},sha256={sha:?}}}",
                bytes.len()
            ));
        }
        let manifest = format!(
            "schema_version = 1\ncreated_ms = {}\nclips = {clips}\ndb_sha256 = {db_sha:?}\n\
            db_bytes = {}\nblobs = [{}]\n",
            now_ms(),
            db_bytes.len(),
            blob_entries.join(",")
        );
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
        for key in &referenced {
            let path = tmp_blobs.join(key);
            if !path.is_file() {
                continue;
            }
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
    /// # Errors
    ///
    /// Returns [`CoreError::Unsupported`] for traversal, symlink, or oversize
    /// entries, [`CoreError::Config`] for bad manifests, or
    /// [`CoreError::Corrupt`] when the embedded DB fails validation.
    pub fn restore_zip(src: &Path, profile: &Path) -> Result<(), CoreError> {
        let tag = TMP_CTR.fetch_add(1, Ordering::SeqCst);
        let tmp = std::env::temp_dir().join(format!("cm-restore-{}-{tag}", std::process::id()));
        mkdir(&tmp)?;
        let _guard = TmpGuard { path: tmp.clone() };
        extract_zip(src, &tmp)?;
        validate_profile(&tmp)?;
        swap_profile(&tmp, profile)?;
        Ok(())
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

/// Delete clips with FTS rows, then sweep newly unreferenced blobs.
fn delete_clips(conn: &Connection, blobs: &Path, ids: &[String]) -> Result<(), CoreError> {
    for id in ids {
        conn.execute("DELETE FROM clips_fts WHERE clip_id=?1", params![id])
            .map_err(|e| db_err(&e))?;
        conn.execute("DELETE FROM clips WHERE id=?1", params![id])
            .map_err(|e| db_err(&e))?;
    }
    sweep_blobs(conn, blobs)
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
        conn.execute("DELETE FROM clip_blobs WHERE blob_key=?1", params![k])
            .map_err(|e| db_err(&e))?;
        let p = blobs.join(k);
        if p.is_file() {
            fs::remove_file(&p).map_err(CoreError::Io)?;
        }
    }
    Ok(())
}

/// Keys referenced by live external payload rows.
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
fn enforce_count(tx: &rusqlite::Transaction<'_>, blobs: &Path, max: u64) -> Result<u64, CoreError> {
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
    delete_clips(tx, blobs, &ids)?;
    Ok(n)
}

/// Delete oldest unpinned clips older than `max_age_days` (0 = off).
fn enforce_age(
    tx: &rusqlite::Transaction<'_>,
    blobs: &Path,
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
    delete_clips(tx, blobs, &ids)?;
    Ok(n)
}

/// Delete oldest unpinned clips while logical bytes exceed `max`.
fn enforce_bytes(tx: &rusqlite::Transaction<'_>, blobs: &Path, max: u64) -> Result<u64, CoreError> {
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
        delete_clips(tx, blobs, std::slice::from_ref(&id))?;
        total = total.saturating_sub(u128::try_from(bytes_i.max(0)).unwrap_or(u128::MAX));
        purged += 1;
    }
    Ok(purged)
}

/// Reject symlinks via Unix mode bits (portable across `zip` versions).
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
    let db = tmp.join("clipstore.sqlite3");
    if !db.is_file() {
        return Err(bad("restore archive missing clipstore.sqlite3"));
    }
    let bytes = fs::read(&db).map_err(CoreError::Io)?;
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
    if let Some(blobs) = man.get("blobs").and_then(toml::Value::as_array) {
        for b in blobs {
            let key = b
                .get("key")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| bad("restore manifest blob missing key"))?;
            let size = b
                .get("bytes")
                .and_then(toml::Value::as_integer)
                .unwrap_or(-1);
            let sha = b.get("sha256").and_then(toml::Value::as_str).unwrap_or("");
            let bytes = fs::read(tmp.join("blobs").join(key))
                .map_err(|_| bad("restore archive missing blob file"))?;
            if size >= 0 && i64::try_from(bytes.len()).unwrap_or(i64::MAX) != size {
                return Err(bad("restore blob size mismatch"));
            }
            if !sha.is_empty() && hex_of(&Sha256::digest(&bytes)) != sha {
                return Err(bad("restore blob checksum mismatch"));
            }
        }
    }
    Ok(())
}

/// Atomically swap a validated profile in, keeping rollback copies on failure.
fn swap_profile(tmp: &Path, profile: &Path) -> Result<(), CoreError> {
    mkdir(profile)?;
    let cur_db = profile.join(DB_NAME);
    let cur_blobs = profile.join("blobs");
    let prev_db = profile.join(".clipstore.prev.sqlite3");
    let prev_blobs = profile.join(".blobs.prev");
    let _ = fs::remove_file(&prev_db);
    let _ = fs::remove_dir_all(&prev_blobs);
    for suffix in ["-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{suffix}", cur_db.display()));
        let _ = fs::remove_file(p);
    }
    let had_db = cur_db.is_file();
    if had_db {
        fs::rename(&cur_db, &prev_db).map_err(CoreError::Io)?;
    }
    let had_blobs = cur_blobs.is_dir();
    if had_blobs {
        fs::rename(&cur_blobs, &prev_blobs).map_err(CoreError::Io)?;
    }
    let rollback = |err: CoreError| {
        let _ = fs::remove_file(&cur_db);
        let _ = fs::remove_dir_all(&cur_blobs);
        if had_db {
            let _ = fs::rename(&prev_db, &cur_db);
        }
        if had_blobs {
            let _ = fs::rename(&prev_blobs, &cur_blobs);
        }
        err
    };
    if let Err(e) = fs::rename(tmp.join("clipstore.sqlite3"), &cur_db).map_err(CoreError::Io) {
        return Err(rollback(e));
    }
    let tmp_blobs = tmp.join("blobs");
    if tmp_blobs.is_dir() {
        if let Err(e) = fs::rename(&tmp_blobs, &cur_blobs).map_err(CoreError::Io) {
            return Err(rollback(e));
        }
    } else if let Err(e) = mkdir(&cur_blobs) {
        return Err(rollback(e));
    }
    let _ = fs::remove_file(&prev_db);
    let _ = fs::remove_dir_all(&prev_blobs);
    fsync_dir(profile);
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

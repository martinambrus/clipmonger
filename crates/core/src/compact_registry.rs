//! Owned read snapshots for the future compact history scanner.
//!
//! Calls serialize on the registry mutex; they never acquire the main connection
//! mutex. Expiry is lazy or explicitly requested, not an active TTL: the future
//! daemon must schedule maintenance to release abandoned snapshots.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags};

use super::Store;

const MAX_SNAPSHOTS: usize = 8;
const MAX_QUERY_BYTES: usize = 4096;
const MAX_AGE: Duration = Duration::from_secs(60);
const BUSY_WAIT: Duration = Duration::from_millis(50);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Opaque identity: OS entropy separates process lifetimes; the checked process
/// sequence prevents reuse by any Store instance in this process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SnapshotToken {
    nonce: [u8; 16],
    sequence: u64,
}

#[derive(Debug)]
pub(crate) enum SnapshotError {
    QueryTooLong,
    Capacity,
    Entropy,
    IdentityExhausted,
    Unknown,
    OwnerMismatch,
    Expired,
    Sql(rusqlite::Error),
}

impl From<rusqlite::Error> for SnapshotError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

struct Snapshot {
    token: SnapshotToken,
    owner: u64,
    created: Instant,
    query: String,
    conn: Connection,
}

#[derive(Default)]
pub(super) struct Registry {
    entries: Vec<Snapshot>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry").field("entries", &self.entries.len()).finish()
    }
}

impl Registry {
    fn expire_at(&mut self, now: Instant) -> usize {
        let before = self.entries.len();
        self.entries.retain(|entry| now.saturating_duration_since(entry.created) < MAX_AGE);
        before - self.entries.len()
    }

    fn find(&mut self, owner: u64, token: SnapshotToken, now: Instant) -> Result<usize, SnapshotError> {
        let index = self.entries.iter().position(|entry| entry.token == token).ok_or(SnapshotError::Unknown)?;
        if self.entries[index].owner != owner {
            return Err(SnapshotError::OwnerMismatch);
        }
        if now.saturating_duration_since(self.entries[index].created) >= MAX_AGE {
            self.entries.remove(index);
            return Err(SnapshotError::Expired);
        }
        Ok(index)
    }
}

impl Store {
    /// Begin an owned snapshot. Query and capacity are checked before opening
    /// the read-only connection. No primary-connection lock is acquired.
    pub(crate) fn begin_compact_snapshot(&self, owner: u64, query: &str) -> Result<SnapshotToken, SnapshotError> {
        if query.len() > MAX_QUERY_BYTES {
            return Err(SnapshotError::QueryTooLong);
        }
        let mut registry = self.compact_snapshots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.expire_at(Instant::now());
        if registry.entries.len() >= MAX_SNAPSHOTS {
            return Err(SnapshotError::Capacity);
        }
        let sequence = NEXT_ID.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| SnapshotError::IdentityExhausted)?;
        let mut nonce = [0; 16];
        nonce[..8].copy_from_slice(&crate::entropy::random_u64().map_err(|_| SnapshotError::Entropy)?.to_ne_bytes());
        nonce[8..].copy_from_slice(&crate::entropy::random_u64().map_err(|_| SnapshotError::Entropy)?.to_ne_bytes());
        let conn = Connection::open_with_flags(&self.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(BUSY_WAIT)?;
        conn.execute_batch("BEGIN")?;
        // BEGIN is deferred: this actual table read pins the database snapshot,
        // including when clips is empty.
        conn.query_row("SELECT EXISTS(SELECT 1 FROM main.clips LIMIT 1)", [], |row| row.get::<_, i64>(0))?;
        let token = SnapshotToken { nonce, sequence };
        registry.entries.push(Snapshot { token, owner, created: Instant::now(), query: query.to_owned(), conn });
        Ok(token)
    }

    /// Run a bounded scanner operation while the registry owns the connection.
    /// The higher-ranked callback cannot return a borrow of connection or query.
    pub(crate) fn use_compact_snapshot<T>(
        &self, owner: u64, token: SnapshotToken,
        f: impl for<'a> FnOnce(&'a Connection, &'a str) -> rusqlite::Result<T>,
    ) -> Result<T, SnapshotError> {
        let mut registry = self.compact_snapshots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = registry.find(owner, token, Instant::now())?;
        let entry = &registry.entries[index];
        f(&entry.conn, &entry.query).map_err(SnapshotError::Sql)
    }

    pub(crate) fn cancel_compact_snapshot(&self, owner: u64, token: SnapshotToken) -> Result<(), SnapshotError> {
        let mut registry = self.compact_snapshots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = registry.find(owner, token, Instant::now())?;
        registry.entries.remove(index);
        Ok(())
    }

    /// Explicit maintenance. Expired entries may persist until this or a lazy
    /// check runs; this does not promise a hard deadline for SQLite operations.
    pub(crate) fn expire_compact_snapshots(&self) -> usize {
        self.compact_snapshots.lock().unwrap_or_else(std::sync::PoisonError::into_inner).expire_at(Instant::now())
    }

    pub(super) fn drain_compact_snapshots(&self) {
        self.compact_snapshots.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("compact-registry-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }
    fn value(store: &Store, owner: u64, token: SnapshotToken) -> Result<String, SnapshotError> {
        store.use_compact_snapshot(owner, token, |conn, query| {
            assert_eq!(query, "needle");
            conn.query_row("SELECT preview FROM clips WHERE id = 'fixture'", [], |row| row.get(0))
        })
    }
    fn seed(store: &Store) {
        store.lock().execute_batch("INSERT INTO clips(id,created_ms,source_os,preview,search_text,content_hash,rev_ms,rev_dev,last_used_ms) VALUES('fixture',1,'test','old','old','hash',1,'test',1)").unwrap();
    }

    #[test]
    fn snapshot_is_stable_across_writer_and_new_snapshot_observes_change() {
        let scratch = Scratch::new();
        let store = Store::open(&scratch.0).unwrap();
        seed(&store);
        let old = store.begin_compact_snapshot(1, "needle").unwrap();
        store.lock().execute("UPDATE clips SET preview='new' WHERE id='fixture'", []).unwrap();
        let new = store.begin_compact_snapshot(1, "needle").unwrap();
        assert_eq!(value(&store, 1, old).unwrap(), "old");
        assert_eq!(value(&store, 1, new).unwrap(), "new");
    }

    #[test]
    fn empty_table_read_also_pins_snapshot() {
        let scratch = Scratch::new();
        let store = Store::open(&scratch.0).unwrap();
        let old = store.begin_compact_snapshot(1, "").unwrap();
        seed(&store);
        let count = |token| store.use_compact_snapshot(1, token, |conn, _| {
            conn.query_row("SELECT count(*) FROM clips", [], |row| row.get::<_, i64>(0))
        }).unwrap();
        assert_eq!(count(old), 0);
        let new = store.begin_compact_snapshot(1, "").unwrap();
        assert_eq!(count(new), 1);
    }

    #[test]
    fn limits_owner_cancel_and_expiry() {
        let scratch = Scratch::new();
        let store = Store::open(&scratch.0).unwrap();
        assert!(matches!(store.begin_compact_snapshot(1, &"é".repeat(2049)), Err(SnapshotError::QueryTooLong)));
        let limit = store.begin_compact_snapshot(1, &"é".repeat(2048)).unwrap();
        store.cancel_compact_snapshot(1, limit).unwrap();
        let first = store.begin_compact_snapshot(1, "needle").unwrap();
        assert!(matches!(store.cancel_compact_snapshot(2, first), Err(SnapshotError::OwnerMismatch)));
        let mut tokens = vec![first];
        for _ in 1..MAX_SNAPSHOTS { tokens.push(store.begin_compact_snapshot(1, "needle").unwrap()); }
        assert!(matches!(store.begin_compact_snapshot(1, "needle"), Err(SnapshotError::Capacity)));
        store.cancel_compact_snapshot(1, first).unwrap();
        assert!(matches!(store.cancel_compact_snapshot(1, first), Err(SnapshotError::Unknown)));
        let replacement = store.begin_compact_snapshot(1, "needle").unwrap();
        assert_ne!(first, replacement);
        {
            let mut registry = store.compact_snapshots.lock().unwrap();
            let entry = registry.entries.iter_mut().find(|entry| entry.token == replacement).unwrap();
            entry.created = Instant::now().checked_sub(MAX_AGE).unwrap();
        }
        assert!(matches!(store.cancel_compact_snapshot(1, replacement), Err(SnapshotError::Expired)));
        {
            let mut registry = store.compact_snapshots.lock().unwrap();
            for entry in &mut registry.entries { entry.created = Instant::now().checked_sub(MAX_AGE).unwrap(); }
        }
        assert_eq!(store.expire_compact_snapshots(), MAX_SNAPSHOTS - 1);
        assert!(matches!(store.cancel_compact_snapshot(1, tokens[1]), Err(SnapshotError::Unknown)));
    }

    #[test]
    fn close_drains_snapshot_before_checkpoint_and_file_rename() {
        let scratch = Scratch::new();
        let store = Store::open(&scratch.0).unwrap();
        seed(&store);
        let stale = store.begin_compact_snapshot(1, "needle").unwrap();
        store.lock().execute("UPDATE clips SET preview='new' WHERE id='fixture'", []).unwrap();
        store.close().unwrap();
        let db = scratch.0.join(super::super::DB_NAME);
        let renamed = scratch.0.join("moved.sqlite3");
        std::fs::rename(&db, &renamed).unwrap();
        std::fs::rename(&renamed, &db).unwrap();
        let reopened = Store::open(&scratch.0).unwrap();
        let fresh = reopened.begin_compact_snapshot(1, "needle").unwrap();
        assert!(matches!(value(&reopened, 1, stale), Err(SnapshotError::Unknown)));
        assert_eq!(value(&reopened, 1, fresh).unwrap(), "new");
    }
}

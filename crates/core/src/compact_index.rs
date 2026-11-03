//! Explicit, cooperative preparation for a future ordered history scan.
//! This is deliberately separate from normal store opening and schema setup.
use super::{db_err, Store};
use crate::error::{BackendKind, CoreError};
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const NAME: &str = "idx_clips_compact_history";
const MAX_BUDGET: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq, Eq)]
pub(super) enum IndexStatus {
    Ready,
    NotReadyReadOnly,
    NotReadyBusy,
    NotReadyInterrupted,
}

fn invalid_index() -> CoreError {
    CoreError::Backend {
        backend: "sqlite".into(),
        kind: BackendKind::Internal,
        message: format!("{NAME} has an unexpected definition"),
    }
}

// Check ownership, partial/unique flags, key columns, collations and directions;
// the auxiliary rowid entry in index_xinfo is not a key column.
enum CheckError {
    Sql(rusqlite::Error),
    Invalid,
}
impl From<rusqlite::Error> for CheckError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}
fn index_present(conn: &Connection) -> Result<bool, CheckError> {
    let owner: Option<(String, String)> = conn
        .query_row(
            "SELECT type, tbl_name FROM main.sqlite_schema WHERE name = ?1 COLLATE NOCASE",
            [NAME],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((kind, table)) = owner else {
        return Ok(false);
    };
    if kind != "index" || table != "clips" {
        return Err(CheckError::Invalid);
    }
    let flags: Option<(i64, i64)> = conn.query_row(
        "SELECT [unique], partial FROM pragma_index_list('clips', 'main') WHERE name = ?1 COLLATE NOCASE",
        [NAME], |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?;
    if flags != Some((0, 0)) {
        return Err(CheckError::Invalid);
    }
    let mut stmt = conn.prepare(
        "SELECT name, [desc], coll FROM pragma_index_xinfo(?1, 'main') WHERE [key] = 1 ORDER BY seqno"
    )?;
    let keys = stmt
        .query_map([NAME], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let expected = [("removed", 0), ("last_used_ms", 1), ("id", 1)];
    if keys.len() != expected.len()
        || !keys
            .iter()
            .zip(expected)
            .all(|((name, desc, coll), (want, direction))| {
                name.as_deref() == Some(want)
                    && *desc == direction
                    && coll
                        .as_deref()
                        .is_some_and(|c| c.eq_ignore_ascii_case("BINARY"))
            })
    {
        return Err(CheckError::Invalid);
    }
    Ok(true)
}

fn classify(error: rusqlite::Error) -> Result<IndexStatus, CoreError> {
    if let rusqlite::Error::SqliteFailure(ref inner, _) = error {
        match inner.code {
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => {
                return Ok(IndexStatus::NotReadyBusy)
            }
            ErrorCode::OperationInterrupted => return Ok(IndexStatus::NotReadyInterrupted),
            _ => {}
        }
    }
    Err(db_err(&error))
}

impl Store {
    /// Prepare outside critical startup. The deadline is cooperative, not an OS
    /// deadline or a bound on SQLite's total memory. Cancellation is shared.
    #[allow(dead_code)]
    pub(super) fn prepare_compact_history_index(
        &self,
        budget: Duration,
        cancel: Arc<AtomicBool>,
    ) -> Result<IndexStatus, CoreError> {
        self.prepare_compact_history_index_inner(budget, cancel, None)
    }

    fn prepare_compact_history_index_inner(
        &self,
        budget: Duration,
        cancel: Arc<AtomicBool>,
        interrupt_after: Option<(usize, Arc<AtomicBool>)>,
    ) -> Result<IndexStatus, CoreError> {
        let deadline = Instant::now() + budget.min(MAX_BUDGET);
        if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
            return Ok(IndexStatus::NotReadyInterrupted);
        }
        let flags = if self.read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        };
        let conn = match Connection::open_with_flags(&self.db_path, flags) {
            Ok(c) => c,
            Err(e) => return classify(e),
        };
        conn.busy_timeout(
            Duration::from_millis(25).min(deadline.saturating_duration_since(Instant::now())),
        )
        .map_err(|e| db_err(&e))?;
        let ddl_active = Arc::new(AtomicBool::new(false));
        let in_ddl = Arc::clone(&ddl_active);
        let mut calls = 0usize;
        let progress_ops = if interrupt_after.is_some() { 1 } else { 1000 };
        conn.progress_handler(
            progress_ops,
            Some(move || {
                if in_ddl.load(Ordering::Relaxed) {
                    calls += 1;
                    if let Some((n, triggered)) = &interrupt_after {
                        if calls >= *n {
                            triggered.store(true, Ordering::Relaxed);
                            return true;
                        }
                    }
                }
                cancel.load(Ordering::Relaxed) || Instant::now() >= deadline
            }),
        )
        .map_err(|e| db_err(&e))?;
        // Each CREATE INDEX statement is atomic in SQLite; on interruption its
        // implicit transaction is rolled back before the connection is dropped.
        let check = || match index_present(&conn) {
            Ok(value) => Ok(Ok(value)),
            Err(CheckError::Invalid) => Err(invalid_index()),
            Err(CheckError::Sql(error)) => classify(error).map(Err),
        };
        let result = (|| {
            let present = match check()? {
                Ok(value) => value,
                Err(status) => return Ok(status),
            };
            if present {
                return Ok(IndexStatus::Ready);
            }
            if self.read_only {
                return Ok(IndexStatus::NotReadyReadOnly);
            }
            ddl_active.store(true, Ordering::Relaxed);
            if let Err(e) = conn.execute_batch("CREATE INDEX IF NOT EXISTS main.idx_clips_compact_history ON clips(removed, last_used_ms DESC, id DESC)") {
                return classify(e);
            }
            ddl_active.store(false, Ordering::Relaxed);
            match check()? {
                Ok(true) => Ok(IndexStatus::Ready),
                Ok(false) => Err(invalid_index()),
                Err(status) => Ok(status),
            }
        })();
        // The handler belongs only to this connection, which is dropped here.
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicU64;

    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn profile() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "compact-index-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    fn prepare(store: &Store) -> IndexStatus {
        store
            .prepare_compact_history_index(MAX_BUDGET, Arc::new(AtomicBool::new(false)))
            .unwrap()
    }
    fn catalog(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT count(*) FROM main.sqlite_schema WHERE name = ?1",
            [NAME],
            |r| r.get(0),
        )
        .unwrap()
    }
    fn profile_bytes(path: &Path) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<_> = std::fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|p| p.is_file() && !p.to_string_lossy().ends_with("-shm"))
            .map(|p| {
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    std::fs::read(p).unwrap(),
                )
            })
            .collect();
        files.sort();
        files
    }
    fn insert_rows(conn: &Connection, count: usize) {
        for n in 0..count {
            conn.execute("INSERT INTO main.clips(id, created_ms, source_os, removed, content_hash, rev_ms, rev_dev, last_used_ms) VALUES (?1, 0, 'test', ?2, ?1, 0, 'test', ?3)",
                rusqlite::params![format!("{n:05}"), (n % 7 == 0) as i64, (n / 4) as i64]).unwrap();
        }
    }

    #[test]
    fn compact_index_prepare_plan_and_keyset() {
        let path = profile();
        let store = Store::open(&path).unwrap();
        let conn = store.conn.lock().unwrap();
        assert_eq!(catalog(&conn), 0);
        insert_rows(&conn, 64);
        drop(conn);
        assert_eq!(prepare(&store), IndexStatus::Ready);
        let conn = store.conn.lock().unwrap();
        assert_eq!(catalog(&conn), 1);
        let version: i64 = conn
            .query_row("PRAGMA main.schema_version", [], |r| r.get(0))
            .unwrap();
        drop(conn);
        assert_eq!(prepare(&store), IndexStatus::Ready);
        let conn = store.conn.lock().unwrap();
        assert_eq!(
            version,
            conn.query_row("PRAGMA main.schema_version", [], |r| r.get::<_, i64>(0))
                .unwrap()
        );
        let sql = "SELECT id, last_used_ms FROM main.clips WHERE removed=0 AND (last_used_ms,id)<(?1,?2) ORDER BY last_used_ms DESC,id DESC LIMIT 7";
        let plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map(rusqlite::params![1000, "z"], |r| r.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(plan.iter().any(|p| p.contains(NAME)), "{plan:?}");
        assert!(!plan.iter().any(|p| p.contains("TEMP B-TREE")), "{plan:?}");
        let expected: Vec<(String, i64)> = (0..64)
            .rev()
            .filter(|n| n % 7 != 0)
            .map(|n| (format!("{n:05}"), (n / 4) as i64))
            .collect();
        let mut actual = Vec::new();
        let mut cursor = (1000_i64, "z".to_string());
        loop {
            let page: Vec<(String, i64)> = conn
                .prepare(sql)
                .unwrap()
                .query_map(rusqlite::params![cursor.0, cursor.1], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            if page.is_empty() {
                break;
            }
            let last = page.last().unwrap();
            cursor = (last.1, last.0.clone());
            actual.extend(page);
        }
        assert_eq!(actual, expected);
        drop(conn);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn compact_index_interrupt_rolls_back_and_retries() {
        let path = profile();
        let store = Store::open(&path).unwrap();
        let conn = store.conn.lock().unwrap();
        insert_rows(&conn, 500);
        let before: (i64, i64) = conn
            .query_row(
                "SELECT count(*), sum(last_used_ms) FROM main.clips",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let version: i64 = conn
            .query_row("PRAGMA main.schema_version", [], |r| r.get(0))
            .unwrap();
        drop(conn);
        let cancel = Arc::new(AtomicBool::new(true));
        assert_eq!(
            store
                .prepare_compact_history_index(MAX_BUDGET, cancel)
                .unwrap(),
            IndexStatus::NotReadyInterrupted
        );
        assert_eq!(
            store
                .prepare_compact_history_index(Duration::ZERO, Arc::new(AtomicBool::new(false)))
                .unwrap(),
            IndexStatus::NotReadyInterrupted
        );
        let triggered = Arc::new(AtomicBool::new(false));
        assert_eq!(
            store
                .prepare_compact_history_index_inner(
                    MAX_BUDGET,
                    Arc::new(AtomicBool::new(false)),
                    Some((1000, Arc::clone(&triggered)))
                )
                .unwrap(),
            IndexStatus::NotReadyInterrupted
        );
        assert!(
            triggered.load(Ordering::Relaxed),
            "DDL progress trigger did not fire"
        );
        let conn = store.conn.lock().unwrap();
        assert_eq!(catalog(&conn), 0);
        assert_eq!(
            version,
            conn.query_row("PRAGMA main.schema_version", [], |r| r.get::<_, i64>(0))
                .unwrap()
        );
        assert_eq!(
            before,
            conn.query_row(
                "SELECT count(*), sum(last_used_ms) FROM main.clips",
                [],
                |r| Ok((r.get(0)?, r.get(1)?))
            )
            .unwrap()
        );
        drop(conn);
        assert_eq!(prepare(&store), IndexStatus::Ready);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn compact_index_rejects_wrong_definition_and_readonly_preserves_profile() {
        let path = profile();
        let store = Store::open(&path).unwrap();
        drop(store);
        let readonly = Store::open_readonly_recovery(&path).unwrap();
        let before = profile_bytes(&path);
        assert_eq!(prepare(&readonly), IndexStatus::NotReadyReadOnly);
        assert_eq!(profile_bytes(&path), before);
        drop(readonly);
        let store = Store::open(&path).unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .execute_batch("CREATE INDEX main.idx_clips_compact_history ON clips(last_used_ms)")
            .unwrap();
        assert!(store
            .prepare_compact_history_index(MAX_BUDGET, Arc::new(AtomicBool::new(false)))
            .is_err());
        store
            .conn
            .lock()
            .unwrap()
            .execute_batch("DROP INDEX main.idx_clips_compact_history")
            .unwrap();
        assert_eq!(prepare(&store), IndexStatus::Ready);
        drop(store);
        let readonly = Store::open_readonly_recovery(&path).unwrap();
        let before = profile_bytes(&path);
        assert_eq!(prepare(&readonly), IndexStatus::Ready);
        assert_eq!(profile_bytes(&path), before);
        drop(readonly);
        std::fs::remove_dir_all(path).unwrap();
    }
}

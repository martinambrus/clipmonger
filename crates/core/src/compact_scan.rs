//! Bounded, snapshot-consistent indexed history scan. Registry operations hold
//! its mutex and never acquire the primary connection lock. Statements and blobs
//! stay inside the operation; the connection and partial row remain registry-owned.
use std::num::NonZeroUsize;
use std::time::Instant;

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use super::compact_registry::{SnapshotError, SnapshotToken};
use super::Store;
use crate::sql_text_stream::{TextColumn, TextCursor, TextStreamError};
use crate::substring_matcher::SubstringMatcher;

const MAX_BYTES: usize = 64 * 1024;
const MAX_CANDIDATES: usize = 32;
const MAX_ROWS: usize = 50;
const MAX_OUTPUT: usize = 192 * 1024;
const PREVIEW_BYTES: usize = 4096;

#[derive(Debug)]
pub(crate) enum ScanError {
    Snapshot(SnapshotError),
    Text(TextStreamError),
    InvalidBudget,
    InvalidId,
    WrongSession,
    Serialization(serde_json::Error),
}
impl From<SnapshotError> for ScanError {
    fn from(e: SnapshotError) -> Self {
        Self::Snapshot(e)
    }
}
impl From<rusqlite::Error> for ScanError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Snapshot(SnapshotError::Sql(e))
    }
}
impl From<TextStreamError> for ScanError {
    fn from(e: TextStreamError) -> Self {
        Self::Text(e)
    }
}
impl From<serde_json::Error> for ScanError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serialization(e)
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct CompactHit {
    pub(crate) id: String,
    pub(crate) pinned: bool,
    pub(crate) last_used_ms: i64,
    pub(crate) preview: String,
    pub(crate) preview_truncated: bool,
}
pub(crate) struct ScanChunk {
    pub(crate) rows: Vec<CompactHit>,
    pub(crate) bytes_read: usize,
    pub(crate) candidates: usize,
    pub(crate) done: bool,
}

pub(super) struct ScanState {
    query: String,
    last: Option<(i64, String)>,
    partial: Option<PartialRow>,
    pending: Option<CompactHit>,
}
struct PartialRow {
    rowid: i64,
    pinned: bool,
    time: i64,
    id: String,
    preview: String,
    truncated: bool,
    phase: Phase,
    cursor: TextCursor,
    matcher: SubstringMatcher,
}
#[derive(PartialEq)]
enum Phase {
    Id,
    Search,
    Preview,
}
impl ScanState {
    pub(super) fn new(query: &str) -> Self {
        Self {
            query: query.to_owned(),
            last: None,
            partial: None,
            pending: None,
        }
    }
}

impl Store {
    pub(crate) fn begin_compact_scan(
        &self,
        owner: u64,
        query: &str,
    ) -> Result<SnapshotToken, ScanError> {
        Ok(self.begin_snapshot(owner, query, true)?)
    }
    pub(crate) fn cancel_compact_scan(
        &self,
        owner: u64,
        token: SnapshotToken,
    ) -> Result<(), ScanError> {
        Ok(self.cancel_compact_snapshot(owner, token)?)
    }
    /// Budgets count raw text bytes, touched candidates (including a resumed
    /// row), and returned rows. Empty chunks with continuation are progress.
    pub(crate) fn step_compact_scan(
        &self,
        owner: u64,
        token: SnapshotToken,
        byte_budget: usize,
        candidate_budget: usize,
        row_budget: usize,
    ) -> Result<ScanChunk, ScanError> {
        let mut registry = self.lock_compact_registry();
        let index = registry.find(owner, token, Instant::now())?;
        if !(1..=MAX_BYTES).contains(&byte_budget)
            || !(1..=MAX_CANDIDATES).contains(&candidate_budget)
            || !(1..=MAX_ROWS).contains(&row_budget)
        {
            registry.entries.remove(index);
            return Err(ScanError::InvalidBudget);
        }
        let entry = &mut registry.entries[index];
        if entry.conn.is_autocommit() {
            registry.entries.remove(index);
            return Err(SnapshotError::TransactionEnded.into());
        }
        let result = match entry.scan.as_mut() {
            Some(state) => state.step(&entry.conn, byte_budget, candidate_budget, row_budget),
            None => Err(ScanError::WrongSession),
        };
        if entry.conn.is_autocommit() {
            registry.entries.remove(index);
            return Err(SnapshotError::TransactionEnded.into());
        }
        if result.as_ref().map_or(true, |chunk| chunk.done) {
            registry.entries.remove(index);
        }
        result
    }
}

impl ScanState {
    fn step(
        &mut self,
        conn: &rusqlite::Connection,
        bytes: usize,
        candidates: usize,
        row_limit: usize,
    ) -> Result<ScanChunk, ScanError> {
        let mut chunk = ScanChunk {
            rows: Vec::new(),
            bytes_read: 0,
            candidates: 0,
            done: false,
        };
        if let Some(hit) = self.pending.take() {
            chunk.rows.push(hit);
        }
        while chunk.rows.len() < row_limit
            && chunk.candidates < candidates
            && chunk.bytes_read < bytes
        {
            if self.partial.is_none() {
                let row: Option<(i64, i64, i64)> = match &self.last {
                    None => conn.query_row("SELECT rowid, pinned, last_used_ms FROM main.clips INDEXED BY idx_clips_compact_history WHERE removed = 0 ORDER BY last_used_ms DESC, id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?,
                    Some((time, id)) => conn.query_row("SELECT rowid, pinned, last_used_ms FROM main.clips INDEXED BY idx_clips_compact_history WHERE removed = 0 AND (last_used_ms, id) < (?1, ?2) ORDER BY last_used_ms DESC, id DESC LIMIT 1", params![time, id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?,
                };
                let Some((rowid, pinned, time)) = row else {
                    chunk.done = true;
                    break;
                };
                self.partial = Some(PartialRow {
                    rowid,
                    pinned: pinned != 0,
                    time,
                    id: String::new(),
                    preview: String::new(),
                    truncated: false,
                    phase: Phase::Id,
                    cursor: TextCursor::new(conn, rowid, TextColumn::Id)?,
                    matcher: SubstringMatcher::new(&self.query),
                });
            }
            chunk.candidates += 1;
            let row = self.partial.as_mut().unwrap();
            loop {
                let remaining = bytes - chunk.bytes_read;
                if remaining == 0 {
                    break;
                }
                let mut invalid_id = false;
                let progress =
                    row.cursor
                        .step(conn, NonZeroUsize::new(remaining).unwrap(), |c| {
                            match row.phase {
                                Phase::Id => {
                                    if row.id.len() + c.len_utf8() <= 36 {
                                        row.id.push(c);
                                    } else {
                                        invalid_id = true;
                                    }
                                }
                                Phase::Search => row.matcher.feed(c),
                                Phase::Preview => {
                                    row.matcher.feed(c);
                                    if !row.truncated {
                                        if row.preview.len() + c.len_utf8() <= PREVIEW_BYTES {
                                            row.preview.push(c);
                                        } else {
                                            row.truncated = true;
                                        }
                                    }
                                }
                            }
                        })?;
                if invalid_id {
                    return Err(ScanError::InvalidId);
                }
                chunk.bytes_read += progress.bytes_read;
                if !progress.eof {
                    break;
                }
                match row.phase {
                    Phase::Id => {
                        let parsed =
                            uuid::Uuid::parse_str(&row.id).map_err(|_| ScanError::InvalidId)?;
                        if parsed.hyphenated().to_string() != row.id {
                            return Err(ScanError::InvalidId);
                        }
                        row.phase = Phase::Search;
                        row.cursor = TextCursor::new(conn, row.rowid, TextColumn::SearchText)?;
                    }
                    Phase::Search => {
                        row.matcher.feed('\n');
                        row.phase = Phase::Preview;
                        row.cursor = TextCursor::new(conn, row.rowid, TextColumn::Preview)?;
                    }
                    Phase::Preview => break,
                }
            }
            if row.phase != Phase::Preview {
                continue;
            }
            // An unfinished preview may have consumed the entire budget.
            if chunk.bytes_read == bytes {
                continue;
            }
            // A zero-budget-free EOF probe is unnecessary: the last step's EOF
            // must be remembered even when it ends exactly at the budget boundary.
            let row = self.partial.take().unwrap();
            self.last = Some((row.time, row.id.clone()));
            if row.matcher.finish() {
                let hit = CompactHit {
                    id: row.id,
                    pinned: row.pinned,
                    last_used_ms: row.time,
                    preview: row.preview,
                    preview_truncated: row.truncated,
                };
                chunk.rows.push(hit);
                if serde_json::to_vec(&chunk.rows)?.len() > MAX_OUTPUT {
                    self.pending = chunk.rows.pop();
                    break;
                }
            }
        }
        Ok(chunk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("scan-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn setup() -> (Scratch, Store) {
        let s = Scratch::new();
        let db = Store::open(&s.0).unwrap();
        db.lock().execute_batch("CREATE INDEX idx_clips_compact_history ON clips(removed, last_used_ms DESC, id DESC)").unwrap();
        (s, db)
    }
    fn id(n: usize) -> String {
        format!("00000000-0000-0000-0000-{n:012x}")
    }
    fn insert(db: &Store, n: usize, time: i64, search: &str, preview: &str, removed: bool) {
        db.lock().execute("INSERT INTO clips(id,created_ms,source_os,preview,search_text,content_hash,rev_ms,rev_dev,last_used_ms,removed) VALUES(?1,1,'test',?2,?3,'hash',1,'test',?4,?5)", params![id(n), preview, search, time, removed]).unwrap();
    }
    fn collect(
        db: &Store,
        token: SnapshotToken,
        query_budget: usize,
        candidates: usize,
        limit: usize,
    ) -> (Vec<CompactHit>, usize) {
        let mut rows = Vec::new();
        let mut empty = 0;
        loop {
            let c = db
                .step_compact_scan(1, token, query_budget, candidates, limit)
                .unwrap();
            assert!(
                c.bytes_read <= query_budget && c.candidates <= candidates && c.rows.len() <= limit
            );
            assert!(serde_json::to_vec(&c.rows).unwrap().len() <= MAX_OUTPUT);
            if c.rows.is_empty() && !c.done {
                empty += 1;
            }
            rows.extend(c.rows);
            if c.done {
                break;
            }
        }
        (rows, empty)
    }
    #[test]
    fn compact_scan_order_predicate_partial_and_snapshot() {
        let (_s, db) = setup();
        let mut fixtures = Vec::new();
        for n in 0..130 {
            let search = if n % 19 == 0 {
                "Σigma\0needle"
            } else {
                "ordinary"
            };
            let preview = if n % 23 == 0 { "tail\nPRE" } else { "value" };
            insert(&db, n, (n / 4) as i64, search, preview, n == 114);
            fixtures.push((n, search.to_owned(), preview.to_owned()));
        }
        let old = db.begin_compact_scan(1, "needle").unwrap();
        let first = db.step_compact_scan(1, old, 7, 2, 2).unwrap();
        assert!(first.rows.is_empty() && !first.done);
        db.lock()
            .execute(
                "UPDATE clips SET search_text='needle' WHERE id=?1",
                [id(129)],
            )
            .unwrap();
        let (mut got, empty) = collect(&db, old, 13, 3, 2);
        assert!(empty > 0);
        got.splice(0..0, first.rows);
        let mut expected: Vec<_> = fixtures
            .iter()
            .filter(|(n, s, p)| *n != 114 && format!("{s}\n{p}").to_lowercase().contains("needle"))
            .map(|(n, _, _)| *n)
            .collect();
        expected.sort_by_key(|n| std::cmp::Reverse((n / 4, *n)));
        assert_eq!(
            got.iter().map(|h| h.id.clone()).collect::<Vec<_>>(),
            expected.iter().map(|n| id(*n)).collect::<Vec<_>>()
        );
        let new = db.begin_compact_scan(1, "needle").unwrap();
        let (got, _) = collect(&db, new, MAX_BYTES, 32, 50);
        assert_eq!(got[0].id, id(129));
        let sep = db.begin_compact_scan(1, "ordinary\nvalue").unwrap();
        let (got, _) = collect(&db, sep, MAX_BYTES, 32, 50);
        assert!(!got.is_empty());
    }
    #[test]
    fn compact_scan_large_fields_pending_and_invalid_tail() {
        let (_s, db) = setup();
        insert(
            &db,
            100,
            100,
            &format!("{}late", "x".repeat(1_100_000)),
            "",
            false,
        );
        insert(
            &db,
            101,
            101,
            "",
            &format!("{}late-preview", "x".repeat(5000)),
            false,
        );
        let preview_token = db.begin_compact_scan(1, "late-preview").unwrap();
        let (preview_hits, _) = collect(&db, preview_token, MAX_BYTES, 32, 50);
        assert_eq!(preview_hits.len(), 1);
        assert_eq!(preview_hits[0].id, id(101));
        assert!(preview_hits[0].preview_truncated);
        let t = db.begin_compact_scan(1, "late").unwrap();
        let (got, empty) = collect(&db, t, MAX_BYTES, 32, 50);
        assert_eq!(got.len(), 2);
        assert!(empty > 0);
        for n in 0..50 {
            insert(&db, n, n as i64, "hit", &"\"".repeat(5000), false);
        }
        let t = db.begin_compact_scan(1, "hit").unwrap();
        let (got, _) = collect(&db, t, MAX_BYTES, 32, 50);
        assert_eq!(got.len(), 50);
        assert!(got
            .iter()
            .all(|x| x.preview.len() == 4096 && x.preview_truncated));
        insert(&db, 200, 200, "match", "ok", false);
        db.lock()
            .execute(
                "UPDATE clips SET preview=CAST(x'ff' AS TEXT) WHERE id=?1",
                [id(200)],
            )
            .unwrap();
        let t = db.begin_compact_scan(1, "match").unwrap();
        assert!(matches!(
            db.step_compact_scan(1, t, MAX_BYTES, 32, 50),
            Err(ScanError::Text(TextStreamError::InvalidText))
        ));
        assert!(matches!(
            db.step_compact_scan(1, t, MAX_BYTES, 32, 50),
            Err(ScanError::Snapshot(SnapshotError::Unknown))
        ));
    }
    #[test]
    fn compact_scan_encoded_limit_pending_and_row_limit() {
        let (_s, db) = setup();
        for n in 0..50 {
            insert(&db, n, n as i64, "hit", &"\0".repeat(4096), false);
        }
        let token = db.begin_compact_scan(1, "hit").unwrap();
        let mut ids = Vec::new();
        let mut saw_pending = false;
        loop {
            let chunk = db
                .step_compact_scan(1, token, MAX_BYTES, MAX_CANDIDATES, MAX_ROWS)
                .unwrap();
            assert!(serde_json::to_vec(&chunk.rows).unwrap().len() <= MAX_OUTPUT);
            {
                let registry = db.lock_compact_registry();
                if !chunk.done {
                    saw_pending |= registry
                        .entries
                        .iter()
                        .any(|e| e.scan.as_ref().is_some_and(|s| s.pending.is_some()));
                }
            }
            ids.extend(chunk.rows.into_iter().map(|h| h.id));
            if chunk.done {
                break;
            }
        }
        assert!(saw_pending);
        assert_eq!(ids, (0..50).rev().map(id).collect::<Vec<_>>());
        let token = db.begin_compact_scan(1, "hit").unwrap();
        let first = db
            .step_compact_scan(1, token, MAX_BYTES, MAX_CANDIDATES, 1)
            .unwrap();
        assert_eq!(first.rows.len(), 1);
        let (rest, _) = collect(&db, token, MAX_BYTES, MAX_CANDIDATES, 1);
        assert_eq!(
            first
                .rows
                .into_iter()
                .chain(rest)
                .map(|h| h.id)
                .collect::<Vec<_>>(),
            ids
        );
    }
    #[test]
    fn compact_scan_real_query_plan() {
        let (_s, db) = setup();
        let conn = db.lock();
        for sql in [
            "SELECT rowid, pinned, last_used_ms FROM main.clips INDEXED BY idx_clips_compact_history WHERE removed = 0 ORDER BY last_used_ms DESC, id DESC LIMIT 1",
            "SELECT rowid, pinned, last_used_ms FROM main.clips INDEXED BY idx_clips_compact_history WHERE removed = 0 AND (last_used_ms, id) < (100, 'z') ORDER BY last_used_ms DESC, id DESC LIMIT 1",
        ] {
            let plan: Vec<String> = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap().query_map([], |r| r.get(3)).unwrap().collect::<Result<_,_>>().unwrap();
            assert!(plan.iter().any(|s| s.contains("idx_clips_compact_history")), "{plan:?}");
            assert!(!plan.iter().any(|s| s.contains("TEMP B-TREE")), "{plan:?}");
        }
    }
    #[test]
    fn compact_scan_index_and_contract() {
        let scratch = Scratch::new();
        let db = Store::open(&scratch.0).unwrap();
        assert!(matches!(
            db.begin_compact_scan(1, ""),
            Err(ScanError::Snapshot(SnapshotError::IndexMissing))
        ));
        let readonly = Store::open_readonly_recovery(&scratch.0).unwrap();
        assert!(matches!(
            readonly.begin_compact_scan(1, ""),
            Err(ScanError::Snapshot(SnapshotError::IndexMissingReadOnly))
        ));
        drop(readonly);
        db.lock()
            .execute_batch("CREATE INDEX idx_clips_compact_history ON clips(id)")
            .unwrap();
        assert!(matches!(
            db.begin_compact_scan(1, ""),
            Err(ScanError::Snapshot(SnapshotError::IndexInvalid))
        ));
        db.lock().execute_batch("DROP INDEX idx_clips_compact_history; CREATE INDEX idx_clips_compact_history ON clips(removed,last_used_ms DESC,id DESC)").unwrap();
        let t = db.begin_compact_scan(1, "").unwrap();
        assert!(matches!(
            db.step_compact_scan(1, t, 0, 1, 1),
            Err(ScanError::InvalidBudget)
        ));
        assert!(matches!(
            db.cancel_compact_scan(1, t),
            Err(ScanError::Snapshot(SnapshotError::Unknown))
        ));
        let t = db.begin_compact_scan(1, "").unwrap();
        assert!(matches!(
            db.cancel_compact_scan(2, t),
            Err(ScanError::Snapshot(SnapshotError::OwnerMismatch))
        ));
        db.cancel_compact_scan(1, t).unwrap();
        let t = db.begin_compact_snapshot(1, "").unwrap();
        assert!(matches!(
            db.use_compact_snapshot(1, t, |conn, _| conn.execute_batch("COMMIT")),
            Err(SnapshotError::TransactionEnded)
        ));
        assert!(matches!(
            db.cancel_compact_snapshot(1, t),
            Err(SnapshotError::Unknown)
        ));
        let t = db.begin_compact_scan(1, "").unwrap();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = db.use_compact_snapshot(1, t, |conn, _| -> rusqlite::Result<()> {
                conn.execute_batch("COMMIT")?;
                panic!("fixture")
            });
        }));
        assert!(matches!(
            db.step_compact_scan(1, t, 1, 1, 1),
            Err(ScanError::Snapshot(SnapshotError::Unknown))
        ));
        let fresh = db.begin_compact_scan(1, "").unwrap();
        assert!(db.step_compact_scan(1, fresh, 1, 1, 1).unwrap().done);
    }
}

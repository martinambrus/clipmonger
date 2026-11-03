//! Bounded, snapshot-consistent indexed history scan. Registry operations hold
//! its mutex and never acquire the primary connection lock. Statements and blobs
//! stay inside the operation; the connection and partial row remain registry-owned.
use std::num::NonZeroUsize;
use std::time::Instant;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::compact_registry::{SnapshotError, SnapshotToken};
use super::Store;
use crate::sql_text_stream::{TextColumn, TextCursor, TextStreamError};
use crate::substring_matcher::SubstringMatcher;

const MAX_BYTES: usize = 64 * 1024;
const MAX_CANDIDATES: usize = 32;
const MAX_ROWS: usize = 50;
const MAX_OUTPUT: usize = 192 * 1024;
const PREVIEW_BYTES: usize = 4096;
// Metadata is bounded separately per touched matching candidate: at most 33
// rowids, 32 MIME reads of <=256 raw bytes and a <=128-byte hash read.
// With at most 32 candidates this is <=266,240 raw metadata bytes per step.
// SQLite page-cache and OS I/O are not bounded by this accounting.
const MAX_MIMES: usize = 32;
const MAX_MIME_BYTES: usize = 256;
const MAX_HASH_BYTES: usize = 128;

#[derive(Debug)]
pub enum ScanError {
    Snapshot(SnapshotError),
    Text(TextStreamError),
    InvalidBudget,
    InvalidId,
    /// A clip has more than 32 MIME rows or one exceeds 256 stored bytes.
    MetadataTooLarge,
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

/// Popup format classification, in display precedence order.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CompactKind {
    Image,
    Html,
    File,
    Text,
    Other,
}

/// Bounded metadata from one pinned history snapshot.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactHit {
    pub id: String,
    pub pinned: bool,
    pub last_used_ms: i64,
    pub preview: String,
    pub preview_truncated: bool,
    pub kind: CompactKind,
    /// Valid lowercase clip content hash, only when image/png exists.
    pub thumbnail_hash: Option<String>,
}
pub struct ScanChunk {
    pub rows: Vec<CompactHit>,
    pub bytes_read: usize,
    pub candidates: usize,
    pub done: bool,
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
    pub fn begin_compact_scan(&self, owner: u64, query: &str) -> Result<SnapshotToken, ScanError> {
        Ok(self.begin_snapshot(owner, query, true)?)
    }
    pub fn cancel_compact_scan(&self, owner: u64, token: SnapshotToken) -> Result<(), ScanError> {
        Ok(self.cancel_compact_snapshot(owner, token)?)
    }
    /// Verify that a continuation belongs to the same session and exact query.
    pub fn validate_compact_query(
        &self,
        owner: u64,
        token: SnapshotToken,
        query: &str,
    ) -> Result<(), ScanError> {
        let mut registry = self.lock_compact_registry();
        let index = registry.find(owner, token, Instant::now())?;
        if registry.entries[index].query != query {
            return Err(SnapshotError::QueryMismatch.into());
        }
        Ok(())
    }
    /// Budgets count raw text bytes, touched candidates (including a resumed
    /// row), and returned rows. Empty chunks with continuation are progress.
    /// Metadata is separately limited per matching candidate to 32 MIME rows
    /// of 256 stored bytes and one 128-byte hash. Up to 266,240 raw metadata
    /// bytes and 1,056 MIME index entries may be inspected per step; they are
    /// not included in `bytes_read`. SQLite page and OS I/O are not bounded.
    pub fn step_compact_scan(
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
                let (kind, thumbnail_hash) = project_metadata(conn, row.rowid, &row.id)?;
                let hit = CompactHit {
                    id: row.id,
                    pinned: row.pinned,
                    last_used_ms: row.time,
                    preview: row.preview,
                    preview_truncated: row.truncated,
                    kind,
                    thumbnail_hash,
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

/// Read only bounded metadata. The indexed clip_id prefix and LIMIT cap the
/// number of visited MIME records; blob lengths are obtained without a SQL
/// length/substring or UTF conversion of an unrestricted stored value.
fn project_metadata(
    conn: &rusqlite::Connection,
    rowid: i64,
    id: &str,
) -> Result<(CompactKind, Option<String>), ScanError> {
    let mut stmt =
        conn.prepare("SELECT rowid FROM main.clip_payloads INDEXED BY idx_compact_payload_clip WHERE clip_id=?1 LIMIT 33")?;
    let ids = stmt
        .query_map([id], |r| r.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if ids.len() > MAX_MIMES {
        return Err(ScanError::MetadataTooLarge);
    }
    let mut image = false;
    let mut html = false;
    let mut file = false;
    let mut text = ids.is_empty();
    let mut png = false;
    for mime_id in ids {
        let mut cursor = TextCursor::new(conn, mime_id, TextColumn::Mime)?;
        if cursor.len() > MAX_MIME_BYTES {
            return Err(ScanError::MetadataTooLarge);
        }
        let mut mime = String::new();
        cursor.step(conn, NonZeroUsize::new(MAX_MIME_BYTES).unwrap(), |c| {
            mime.push(c)
        })?;
        image |= mime.starts_with("image/");
        html |= mime.contains("html");
        file |= mime.contains("uri-list");
        text |= mime
            .split(';')
            .next()
            .is_some_and(|base| base.trim().starts_with("text/"));
        png |= mime == "image/png";
    }
    let kind = if image {
        CompactKind::Image
    } else if html {
        CompactKind::Html
    } else if file {
        CompactKind::File
    } else if text {
        CompactKind::Text
    } else {
        CompactKind::Other
    };
    let hash = if png {
        match TextCursor::new(conn, rowid, TextColumn::ContentHash) {
            Ok(mut cursor) if cursor.len() <= MAX_HASH_BYTES => {
                let mut value = String::new();
                match cursor.step(conn, NonZeroUsize::new(MAX_HASH_BYTES).unwrap(), |c| {
                    value.push(c)
                }) {
                    Ok(_) if crate::model::valid_clip_content_hash(&value) => Some(value),
                    Ok(_) | Err(TextStreamError::InvalidText | TextStreamError::WrongType) => None,
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(_) | Err(TextStreamError::WrongType) => None,
            Err(e) => return Err(e.into()),
        }
    } else {
        None
    };
    Ok((kind, hash))
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
        db.lock().execute_batch("CREATE INDEX idx_clips_compact_history ON clips(removed, last_used_ms DESC, id DESC); CREATE INDEX idx_compact_payload_clip ON clip_payloads(clip_id)").unwrap();
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
    fn compact_projection_rejects_oversized_mime() {
        let (_s, db) = setup();
        insert(&db, 1, 1, "", "row", false);
        db.lock()
            .execute(
                "INSERT INTO clip_payloads(clip_id,mime) VALUES(?1,?2)",
                params![id(1), "x".repeat(257)],
            )
            .unwrap();
        let token = db.begin_compact_scan(1, "").unwrap();
        let result = db.step_compact_scan(1, token, 65536, 32, 50);
        assert!(
            matches!(result, Err(ref e) if format!("{e:?}").contains("MetadataTooLarge")),
            "oversized MIME must fail explicitly"
        );
        assert!(
            db.cancel_compact_scan(1, token).is_err(),
            "failure drains snapshot"
        );
    }

    #[test]
    fn compact_projection_caps_mime_count_and_closes_failed_scan() {
        let (_s, db) = setup();
        insert(&db, 1, 1, "", "row", false);
        for n in 0..33 {
            db.lock()
                .execute(
                    "INSERT INTO clip_payloads(clip_id,mime) VALUES(?1,?2)",
                    params![id(1), format!("application/{n:02}")],
                )
                .unwrap();
        }
        let token = db.begin_compact_scan(1, "").unwrap();
        assert!(matches!(
            db.step_compact_scan(1, token, 65536, 32, 50),
            Err(ScanError::MetadataTooLarge)
        ));
        assert!(matches!(
            db.cancel_compact_scan(1, token),
            Err(ScanError::Snapshot(SnapshotError::Unknown))
        ));
    }

    #[test]
    fn compact_projection_late_markers_and_single_byte_progress() {
        let (_s, db) = setup();
        insert(&db, 1, 1, "", "row", false);
        insert(&db, 2, 2, "", "row", false);
        let html = format!("{}html", "a".repeat(240));
        let uri = format!("{}uri-list", "a".repeat(240));
        db.lock()
            .execute(
                "INSERT INTO clip_payloads(clip_id,mime) VALUES(?1,?2)",
                params![id(1), html],
            )
            .unwrap();
        db.lock()
            .execute(
                "INSERT INTO clip_payloads(clip_id,mime) VALUES(?1,?2)",
                params![id(2), uri],
            )
            .unwrap();
        let token = db.begin_compact_scan(1, "").unwrap();
        let mut rows = Vec::new();
        for _ in 0..256 {
            let chunk = db.step_compact_scan(1, token, 1, 1, 50).unwrap();
            assert!(chunk.bytes_read <= 1 && chunk.candidates <= 1);
            rows.extend(chunk.rows);
            if chunk.done {
                break;
            }
        }
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter().find(|r| r.id == id(1)).unwrap().kind,
            CompactKind::Html
        );
        assert_eq!(
            rows.iter().find(|r| r.id == id(2)).unwrap().kind,
            CompactKind::File
        );
        assert!(matches!(
            db.cancel_compact_scan(1, token),
            Err(ScanError::Snapshot(SnapshotError::Unknown))
        ));
    }

    #[test]
    fn compact_projection_uses_pinned_snapshot_and_valid_hash() {
        let (_s, db) = setup();
        for n in 1..=9 {
            insert(&db, n, n as i64, "", "row", false);
        }
        let valid = "a".repeat(64);
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE clips SET content_hash=?1 WHERE id IN (?2,?3,?4)",
                params![valid, id(1), id(2), id(3)],
            )
            .unwrap();
            conn.execute(
                "UPDATE clips SET content_hash=?1 WHERE id=?2",
                params!["a".repeat(65), id(4)],
            )
            .unwrap();
            conn.execute(
                "UPDATE clips SET content_hash=?1 WHERE id=?2",
                params!["g".repeat(64), id(6)],
            )
            .unwrap();
            conn.execute(
                "UPDATE clips SET content_hash=CAST(?1 AS BLOB) WHERE id=?2",
                params![valid, id(7)],
            )
            .unwrap();
            conn.execute(
                "UPDATE clips SET content_hash=?1 WHERE id=?2",
                params!["a".repeat(100000), id(8)],
            )
            .unwrap();
            conn.execute(
                "UPDATE clips SET content_hash=?1 WHERE id=?2",
                params!["af63dc4c8601ec8c", id(9)],
            )
            .unwrap();
            for (n, mime) in [
                (9, "image/png"),
                (1, "text/html"),
                (1, "image/png"),
                (2, "image/jpeg"),
                (3, "text/uri-list"),
                (3, "text/html"),
                (4, "image/png"),
                (5, "application/example"),
                (6, "image/png"),
                (7, "image/png"),
                (8, "image/png"),
            ] {
                conn.execute(
                    "INSERT INTO clip_payloads(clip_id,mime) VALUES(?1,?2)",
                    params![id(n), mime],
                )
                .unwrap();
            }
        }
        let token = db.begin_compact_scan(1, "").unwrap();
        {
            let conn = db.lock();
            conn.execute(
                "DELETE FROM clip_payloads WHERE clip_id=?1 AND mime='image/png'",
                [id(1)],
            )
            .unwrap();
        }
        let (rows, _) = collect(&db, token, 65536, 32, 50);
        let row = |n| rows.iter().find(|row| row.id == id(n)).unwrap();
        assert_eq!(row(1).kind, CompactKind::Image);
        assert_eq!(row(1).thumbnail_hash.as_deref(), Some(valid.as_str()));
        assert_eq!(row(2).kind, CompactKind::Image);
        assert_eq!(row(2).thumbnail_hash, None);
        assert_eq!(row(3).kind, CompactKind::Html);
        assert_eq!(row(4).thumbnail_hash, None);
        assert_eq!(row(5).kind, CompactKind::Other);
        assert_eq!(row(9).thumbnail_hash.as_deref(), Some("af63dc4c8601ec8c"));
        for n in 6..=8 {
            assert_eq!(row(n).thumbnail_hash, None);
        }
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
        db.lock().execute_batch("DROP INDEX idx_clips_compact_history; CREATE INDEX idx_clips_compact_history ON clips(removed,last_used_ms DESC,id DESC); CREATE INDEX IF NOT EXISTS idx_compact_payload_clip ON clip_payloads(clip_id)").unwrap();
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

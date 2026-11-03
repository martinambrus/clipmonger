//! Incremental reading of the two history TEXT fields.
//!
//! The caller must keep the same connection, read transaction, and store generation
//! alive and unchanged for the entire cursor lifetime. In particular, neither rowid
//! nor length establishes snapshot coherence. A future session must own that
//! connection and invalidate cursors on transaction end, writes, or store replacement.

use std::num::NonZeroUsize;

use rusqlite::{Connection, MAIN_DB};

const MAX_STEP: usize = 64 * 1024;

#[derive(Clone, Copy)]
pub(crate) enum TextColumn {
    SearchText,
    Preview,
    Id,
    Mime,
    ContentHash,
    UriNames,
}

impl TextColumn {
    fn name(self) -> &'static str {
        match self {
            Self::SearchText => "search_text",
            Self::Preview => "preview",
            Self::Id => "id",
            Self::Mime => "mime",
            Self::ContentHash => "content_hash",
            Self::UriNames => "names",
        }
    }
    fn table(self) -> &'static str {
        match self {
            Self::Mime => "clip_payloads",
            Self::UriNames => "uri_search",
            _ => "clips",
        }
    }
    fn type_sql(self) -> &'static str {
        match self {
            Self::SearchText => "SELECT typeof(search_text) FROM main.clips WHERE rowid = ?1",
            Self::Preview => "SELECT typeof(preview) FROM main.clips WHERE rowid = ?1",
            Self::Id => "SELECT typeof(id) FROM main.clips WHERE rowid = ?1",
            Self::Mime => "SELECT typeof(mime) FROM main.clip_payloads WHERE rowid = ?1",
            Self::ContentHash => "SELECT typeof(content_hash) FROM main.clips WHERE rowid = ?1",
            Self::UriNames => "SELECT typeof(names) FROM main.uri_search WHERE rowid = ?1",
        }
    }
}

#[derive(Clone, Copy)]
enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// Errors encountered while validating stored text incrementally.
#[derive(Debug)]
pub enum TextStreamError {
    Sql(rusqlite::Error),
    TransactionRequired,
    WrongType,
    UnsupportedEncoding,
    InvalidText,
    LengthChanged,
}
impl From<rusqlite::Error> for TextStreamError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sql(value)
    }
}

pub(crate) struct TextStep {
    pub(crate) bytes_read: usize,
    pub(crate) eof: bool,
}

pub(crate) struct TextCursor {
    rowid: i64,
    column: TextColumn,
    encoding: Encoding,
    length: usize,
    offset: usize,
    carry: [u8; 4],
    carry_len: usize,
    finished: bool,
}

impl TextCursor {
    pub(crate) fn new(
        conn: &Connection,
        rowid: i64,
        column: TextColumn,
    ) -> Result<Self, TextStreamError> {
        if conn.is_autocommit() {
            return Err(TextStreamError::TransactionRequired);
        }
        let ty: String = conn.query_row(column.type_sql(), [rowid], |r| r.get(0))?;
        if ty != "text" {
            return Err(TextStreamError::WrongType);
        }
        let encoding: String = conn.query_row("PRAGMA main.encoding", [], |r| r.get(0))?;
        let encoding = match encoding.as_str() {
            "UTF-8" => Encoding::Utf8,
            "UTF-16le" => Encoding::Utf16Le,
            "UTF-16be" => Encoding::Utf16Be,
            _ => return Err(TextStreamError::UnsupportedEncoding),
        };
        let blob = conn.blob_open(MAIN_DB, column.table(), column.name(), rowid, true)?;
        Ok(Self {
            rowid,
            column,
            encoding,
            length: blob.len(),
            offset: 0,
            carry: [0; 4],
            carry_len: 0,
            finished: false,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.length
    }

    /// Reads at most `min(budget, 64 KiB)` raw bytes and emits complete scalars.
    /// On error the cursor must be discarded, including any scalars already emitted.
    pub(crate) fn step(
        &mut self,
        conn: &Connection,
        budget: NonZeroUsize,
        mut emit: impl FnMut(char),
    ) -> Result<TextStep, TextStreamError> {
        if conn.is_autocommit() {
            return Err(TextStreamError::TransactionRequired);
        }
        if self.finished {
            return Ok(TextStep {
                bytes_read: 0,
                eof: true,
            });
        }
        let blob = conn.blob_open(
            MAIN_DB,
            self.column.table(),
            self.column.name(),
            self.rowid,
            true,
        )?;
        if blob.len() != self.length {
            return Err(TextStreamError::LengthChanged);
        }
        let n = (self.length - self.offset).min(budget.get()).min(MAX_STEP);
        // A fixed, bounded buffer avoids a heap allocation on every stream step.
        #[allow(clippy::large_stack_arrays)]
        let mut buffer = [0u8; MAX_STEP];
        blob.read_at_exact(&mut buffer[..n], self.offset)?;
        self.offset += n;
        for &byte in &buffer[..n] {
            self.carry[self.carry_len] = byte;
            self.carry_len += 1;
            match self.encoding {
                Encoding::Utf8 => self.utf8(&mut emit)?,
                Encoding::Utf16Le | Encoding::Utf16Be => self.utf16(false, &mut emit)?,
            }
        }
        if self.offset == self.length {
            match self.encoding {
                Encoding::Utf8 if self.carry_len != 0 => return Err(TextStreamError::InvalidText),
                Encoding::Utf8 => {}
                _ => self.utf16(true, &mut emit)?,
            }
            self.finished = true;
        }
        Ok(TextStep {
            bytes_read: n,
            eof: self.finished,
        })
    }

    fn utf8(&mut self, emit: &mut impl FnMut(char)) -> Result<(), TextStreamError> {
        match std::str::from_utf8(&self.carry[..self.carry_len]) {
            Ok(s) => {
                for c in s.chars() {
                    emit(c);
                }
                self.carry_len = 0;
                Ok(())
            }
            Err(e) if e.error_len().is_none() && self.carry_len < 4 => Ok(()),
            Err(_) => Err(TextStreamError::InvalidText),
        }
    }

    fn utf16(&mut self, end: bool, emit: &mut impl FnMut(char)) -> Result<(), TextStreamError> {
        if self.carry_len < 2 {
            if end {
                self.carry_len = 0;
            }
            return Ok(());
        }
        let word = |a, b| match self.encoding {
            Encoding::Utf16Le => u16::from_le_bytes([a, b]),
            _ => u16::from_be_bytes([a, b]),
        };
        let first = word(self.carry[0], self.carry[1]);
        let surrogate = (0xd800..0xe000).contains(&first);
        if surrogate && self.carry_len < 4 && !end {
            return Ok(());
        }
        let (value, consumed) = if surrogate && self.carry_len >= 4 {
            let second = u32::from(word(self.carry[2], self.carry[3]));
            let c = u32::from(first);
            (
                (second & 0x3ff) + ((c & 0x3f) << 10) + (((c & 0x3c0) + 0x40) << 10),
                4,
            )
        } else {
            (u32::from(first), 2)
        };
        let c = char::from_u32(value).ok_or(TextStreamError::InvalidText)?;
        emit(c);
        self.carry.copy_within(consumed..self.carry_len, 0);
        self.carry_len -= consumed;
        if end {
            self.carry_len = 0;
        } // SQLite ignores a final odd byte.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::substring_matcher::SubstringMatcher;
    use rusqlite::params;

    fn db(encoding: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("PRAGMA encoding = '{encoding}'; CREATE TABLE clips(search_text TEXT, preview TEXT); BEGIN;" )).unwrap();
        conn
    }
    fn collect(
        conn: &Connection,
        column: TextColumn,
        budget: usize,
    ) -> Result<String, TextStreamError> {
        let mut cursor = TextCursor::new(conn, 1, column)?;
        let mut output = String::new();
        loop {
            let step = cursor.step(conn, NonZeroUsize::new(budget).unwrap(), |c| output.push(c))?;
            assert!(step.bytes_read <= budget.min(MAX_STEP));
            if step.eof {
                break;
            }
            assert!(step.bytes_read > 0);
        }
        Ok(output)
    }
    #[test]
    fn real_sql_differential_and_matcher_composition() {
        for encoding in ["UTF-8", "UTF-16le", "UTF-16be"] {
            let conn = db(encoding);
            for input in ["", "ascii\0end", "İΟΣ\u{301}a😀", "xé😀Σ"] {
                conn.execute("DELETE FROM clips", []).unwrap();
                conn.execute(
                    "INSERT INTO clips(rowid,search_text,preview) VALUES(1,?1,?2)",
                    params![input, "preview İ"],
                )
                .unwrap();
                let reference: String = conn
                    .query_row("SELECT search_text FROM clips", [], |r| r.get(0))
                    .unwrap();
                for budget in 1..=9 {
                    assert_eq!(
                        collect(&conn, TextColumn::SearchText, budget).unwrap(),
                        reference,
                        "{encoding} budget {budget}"
                    );
                    let mut matcher = SubstringMatcher::new("σ\npre");
                    for c in collect(&conn, TextColumn::SearchText, budget)
                        .unwrap()
                        .chars()
                        .chain("\n".chars())
                        .chain(collect(&conn, TextColumn::Preview, budget).unwrap().chars())
                    {
                        matcher.feed(c);
                    }
                    assert_eq!(
                        matcher.finish(),
                        format!("{input}\npreview İ")
                            .to_lowercase()
                            .contains(&"σ\npre".to_lowercase())
                    );
                }
            }
        }
    }
    #[test]
    fn malformed_sql_text_agrees_with_column_read() {
        for encoding in ["UTF-8", "UTF-16le", "UTF-16be"] {
            let conn = db(encoding);
            // CAST may normalize or convert bytes; inspect the actual TEXT payload.
            // Separate raw UTF-16 fixtures below guarantee malformed code units.
            for bytes in [
                &[0xff][..],
                &[0x00, 0xd8][..],
                &[0x00, 0xd8, 0x41, 0x00][..],
                &[0xdc, 0x00, 0x00, 0x41, 0xff][..],
                &[0xf0, 0x9f, 0x98][..],
            ] {
                conn.execute("DELETE FROM clips", []).unwrap();
                conn.execute(
                    "INSERT INTO clips(rowid,search_text,preview) VALUES(1,CAST(?1 AS TEXT),'')",
                    [bytes],
                )
                .unwrap();
                let blob = conn
                    .blob_open(MAIN_DB, "clips", "search_text", 1, true)
                    .unwrap();
                let mut stored = vec![0; blob.len()];
                blob.read_at_exact(&mut stored, 0).unwrap();
                drop(blob);
                let reference: rusqlite::Result<String> =
                    conn.query_row("SELECT search_text FROM clips", [], |r| r.get(0));
                eprintln!(
                    "CAST {encoding}: input={bytes:?}, stored={stored:?}, reference={reference:?}"
                );
                for budget in 1..=5 {
                    let actual = collect(&conn, TextColumn::SearchText, budget);
                    match &reference {
                        Ok(s) => assert_eq!(actual.unwrap(), *s, "{encoding} {bytes:?}"),
                        Err(_) => assert!(
                            matches!(actual, Err(TextStreamError::InvalidText)),
                            "{encoding} {bytes:?}"
                        ),
                    }
                }
            }
        }
    }
    #[test]
    fn malformed_utf16_raw_units_agree_with_sql() {
        for (encoding, little_endian) in [("UTF-16le", true), ("UTF-16be", false)] {
            let conn = db(encoding);
            // Writable blob access is fixture setup only. This keeps the TEXT storage
            // class while inserting exact UTF-16 code units into the record.
            for units in [
                &[0xd800u16][..],
                &[0xdc00u16][..],
                &[0xd800u16, 0x0041][..],
                &[0xdc00u16, 0xdc01][..],
                &[0xd83du16, 0xde00][..],
                &[0x0041u16, 0xd800, 0x0042][..],
            ] {
                let bytes: Vec<u8> = units
                    .iter()
                    .flat_map(|u| {
                        if little_endian {
                            u.to_le_bytes()
                        } else {
                            u.to_be_bytes()
                        }
                    })
                    .collect();
                conn.execute("DELETE FROM clips", []).unwrap();
                conn.execute(
                    "INSERT INTO clips(rowid,search_text,preview) VALUES(1,?1,'')",
                    ["A".repeat(units.len())],
                )
                .unwrap();
                let mut blob = conn
                    .blob_open(MAIN_DB, "clips", "search_text", 1, false)
                    .unwrap();
                assert_eq!(blob.len(), bytes.len(), "{encoding} {units:?}");
                blob.write_at(&bytes, 0).unwrap();
                drop(blob);
                let blob = conn
                    .blob_open(MAIN_DB, "clips", "search_text", 1, true)
                    .unwrap();
                let mut stored = vec![0; blob.len()];
                blob.read_at_exact(&mut stored, 0).unwrap();
                assert_eq!(stored, bytes, "raw fixture {encoding} {units:?}");
                drop(blob);
                let reference: rusqlite::Result<String> =
                    conn.query_row("SELECT search_text FROM clips WHERE rowid=1", [], |r| {
                        r.get(0)
                    });
                for budget in 1..=5 {
                    let actual = collect(&conn, TextColumn::SearchText, budget);
                    match &reference {
                        Ok(s) => {
                            assert_eq!(actual.unwrap(), *s, "{encoding} {units:?} budget {budget}");
                        }
                        Err(_) => assert!(
                            matches!(actual, Err(TextStreamError::InvalidText)),
                            "{encoding} {units:?} budget {budget}"
                        ),
                    }
                }
            }
        }
    }
    #[test]
    fn large_field_is_not_truncated_and_limits_are_observed() {
        let conn = db("UTF-8");
        let input = format!("{}tail😀", "a".repeat(1024 * 1024 + 23));
        conn.execute(
            "INSERT INTO clips(rowid,search_text,preview) VALUES(1,?1,'')",
            [&input],
        )
        .unwrap();
        let mut cursor = TextCursor::new(&conn, 1, TextColumn::SearchText).unwrap();
        let mut matcher = SubstringMatcher::new("tail😀");
        let mut total = 0;
        loop {
            let step = cursor
                .step(&conn, NonZeroUsize::new(usize::MAX).unwrap(), |c| {
                    matcher.feed(c);
                })
                .unwrap();
            assert!(step.bytes_read <= 65536);
            total += step.bytes_read;
            if step.eof {
                break;
            }
            assert!(step.bytes_read > 0);
        }
        assert_eq!(total, input.len());
        assert!(matcher.finish());
    }
    #[test]
    fn main_blob_is_rejected_when_temp_text_shadows_it() {
        let conn = db("UTF-8");
        conn.execute(
            "INSERT INTO main.clips(rowid,search_text,preview) VALUES(1,?1,?1)",
            [&[0x61u8][..]],
        )
        .unwrap();
        conn.execute_batch("CREATE TEMP TABLE clips(search_text TEXT, preview TEXT); INSERT INTO temp.clips(rowid,search_text,preview) VALUES(1,'temp','temp');").unwrap();
        for column in [TextColumn::SearchText, TextColumn::Preview] {
            assert!(matches!(
                TextCursor::new(&conn, 1, column),
                Err(TextStreamError::WrongType)
            ));
        }
    }
    #[test]
    fn main_text_is_read_when_temp_blob_shadows_it() {
        let conn = db("UTF-8");
        conn.execute("INSERT INTO main.clips(rowid,search_text,preview) VALUES(1,'main search','main preview')", []).unwrap();
        conn.execute_batch("CREATE TEMP TABLE clips(search_text TEXT, preview TEXT); INSERT INTO temp.clips(rowid,search_text,preview) VALUES(1,x'61',x'61');").unwrap();
        assert_eq!(
            collect(&conn, TextColumn::SearchText, 2).unwrap(),
            "main search"
        );
        assert_eq!(
            collect(&conn, TextColumn::Preview, 2).unwrap(),
            "main preview"
        );
    }
    #[test]
    fn errors_and_transaction_requirement() {
        let conn = db("UTF-8");
        assert!(matches!(
            TextCursor::new(&conn, 99, TextColumn::Preview),
            Err(TextStreamError::Sql(_))
        ));
        conn.execute(
            "INSERT INTO clips(rowid,search_text,preview) VALUES(1,?1,'')",
            [&[1u8, 2][..]],
        )
        .unwrap();
        assert!(matches!(
            TextCursor::new(&conn, 1, TextColumn::SearchText),
            Err(TextStreamError::WrongType)
        ));
        conn.execute_batch("COMMIT").unwrap();
        assert!(matches!(
            TextCursor::new(&conn, 1, TextColumn::Preview),
            Err(TextStreamError::TransactionRequired)
        ));
    }
}

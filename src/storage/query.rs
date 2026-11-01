//! Reads: list, get, nth, alias lookup, counts and FTS5 search.

use super::{ClipEntry, Counts, EntryId, Source, Storage};
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, Row};

/// Columns for `entry_from_row`, in order.
pub(crate) const ENTRY_COLUMNS: &str =
    "id, created_at, last_used, size, preview, pinned, alias, source, mime";

pub(crate) fn entry_from_row(row: &Row<'_>) -> rusqlite::Result<ClipEntry> {
    let size: i64 = row.get(3)?;
    let source: String = row.get(7)?;
    Ok(ClipEntry {
        id: row.get(0)?,
        created_at: row.get(1)?,
        last_used: row.get(2)?,
        size: usize::try_from(size).unwrap_or(0),
        preview: row.get(4)?,
        pinned: row.get(5)?,
        alias: row.get(6)?,
        source: Source::from_db(&source),
        mime: row.get(8)?,
    })
}

impl Storage {
    pub fn get(&self, id: EntryId) -> Result<Option<ClipEntry>> {
        let conn = self.conn();
        let sql = format!("SELECT {ENTRY_COLUMNS} FROM entries WHERE id = ?1");
        Ok(conn.prepare_cached(&sql)?.query_row([id], entry_from_row).optional()?)
    }

    /// The pinned entry with this alias.
    pub fn by_alias(&self, alias: &str) -> Result<Option<ClipEntry>> {
        let conn = self.conn();
        let sql = format!("SELECT {ENTRY_COLUMNS} FROM entries WHERE alias = ?1");
        Ok(conn.prepare_cached(&sql)?.query_row([alias], entry_from_row).optional()?)
    }

    pub fn counts(&self) -> Result<Counts> {
        let conn = self.conn();
        let (total, pinned, total_bytes, oldest, newest): (i64, i64, i64, Option<i64>, Option<i64>) =
            conn.prepare_cached(
                "SELECT count(*), coalesce(sum(pinned), 0), coalesce(sum(size), 0),
                        min(last_used), max(last_used)
                 FROM entries",
            )?
            .query_row([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
        Ok(Counts {
            total: usize::try_from(total)?,
            pinned: usize::try_from(pinned)?,
            total_bytes: u64::try_from(total_bytes)?,
            oldest,
            newest,
        })
    }

    /// Full content of an entry; an error if it doesn't exist.
    pub fn load_content(&self, id: EntryId) -> Result<String> {
        let conn = self.conn();
        conn.prepare_cached("SELECT content FROM entries WHERE id = ?1")?
            .query_row([id], |r| r.get(0))
            .optional()?
            .with_context(|| format!("No entry with id {id}"))
    }
}

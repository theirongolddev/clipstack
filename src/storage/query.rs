//! Reads: list, get, nth, alias lookup, counts and FTS5 search.

use super::{ClipEntry, Counts, EntryId, ListOrder, Source, Storage};
use rusqlite::types::Value;
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

/// Shortest token the trigram index can match.
const MIN_FTS_TOKEN_CHARS: usize = 3;

/// FTS5 query for the search tokens of 3+ characters: each one a phrase
/// (internal `"` doubled, so no FTS syntax gets through), joined by AND.
/// `None` when no token is long enough.
pub(crate) fn fts_query(query: &str) -> Option<String> {
    let phrases: Vec<String> = query
        .split_whitespace()
        .filter(|t| t.chars().count() >= MIN_FTS_TOKEN_CHARS)
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    (!phrases.is_empty()).then(|| phrases.join(" AND "))
}

fn order_by(order: ListOrder) -> &'static str {
    match order {
        ListOrder::Recency => "last_used DESC, id DESC",
        ListOrder::PinnedFirst => "pinned DESC, last_used DESC, id DESC",
    }
}

/// SQLite `LIMIT` value; -1 means none.
fn sql_limit(limit: Option<usize>) -> i64 {
    limit.and_then(|n| i64::try_from(n).ok()).unwrap_or(-1)
}

impl Storage {
    pub fn list(&self, order: ListOrder, limit: Option<usize>) -> Result<Vec<ClipEntry>> {
        let conn = self.conn();
        let sql = format!("SELECT {ENTRY_COLUMNS} FROM entries ORDER BY {} LIMIT ?1", order_by(order));
        let mut stmt = conn.prepare_cached(&sql)?;
        let entries = stmt.query_map([sql_limit(limit)], entry_from_row)?;
        Ok(entries.collect::<rusqlite::Result<_>>()?)
    }

    /// The entry at 1-based `position` in recency order.
    pub fn nth(&self, position: usize) -> Result<Option<ClipEntry>> {
        let Some(offset) = position.checked_sub(1) else {
            return Ok(None);
        };
        let conn = self.conn();
        let sql = format!(
            "SELECT {ENTRY_COLUMNS} FROM entries ORDER BY {} LIMIT 1 OFFSET ?1",
            order_by(ListOrder::Recency)
        );
        Ok(conn.prepare_cached(&sql)?.query_row([i64::try_from(offset)?], entry_from_row).optional()?)
    }

    /// Entries containing every whitespace-separated token (case-insensitive),
    /// pinned first, then by recency. Tokens of 3+ characters use the trigram
    /// index (first 256 Ki characters of each entry); shorter ones a substring
    /// scan. An empty query lists everything.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<ClipEntry>> {
        let mut filters = Vec::new();
        let mut params: Vec<Value> = Vec::new();
        if let Some(fts) = fts_query(query) {
            params.push(Value::Text(fts));
            filters.push(format!(
                "id IN (SELECT rowid FROM entries_fts WHERE entries_fts MATCH ?{})",
                params.len()
            ));
        }
        for token in query.split_whitespace().filter(|t| t.chars().count() < MIN_FTS_TOKEN_CHARS) {
            params.push(Value::Text(token.to_string()));
            filters.push(format!("instr(lower(content), lower(?{})) > 0", params.len()));
        }
        let filter = if filters.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", filters.join(" AND "))
        };
        params.push(Value::Integer(sql_limit(Some(limit))));
        let sql = format!(
            "SELECT {ENTRY_COLUMNS} FROM entries {filter} ORDER BY {} LIMIT ?{}",
            order_by(ListOrder::PinnedFirst),
            params.len()
        );

        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let entries = stmt.query_map(rusqlite::params_from_iter(params), entry_from_row)?;
        Ok(entries.collect::<rusqlite::Result<_>>()?)
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{ListOrder, test_storage};

    fn add(storage: &Storage, content: &str, at: i64) -> EntryId {
        match storage.ingest_at(content, Source::Copy, &[], at).unwrap() {
            crate::storage::ingest::IngestOutcome::Inserted(e) => e.id,
            other => panic!("not inserted: {other:?}"),
        }
    }

    fn ids(entries: Vec<ClipEntry>) -> Vec<EntryId> {
        entries.into_iter().map(|e| e.id).collect()
    }

    #[test]
    fn list_orders_by_recency_or_pinned_first() {
        let (storage, _dir) = test_storage();
        let a = add(&storage, "a", 1);
        let b = add(&storage, "b", 2);
        let c = add(&storage, "c", 3);
        storage.set_pinned(a, true).unwrap();

        assert_eq!(ids(storage.list(ListOrder::Recency, None).unwrap()), [c, b, a]);
        assert_eq!(ids(storage.list(ListOrder::PinnedFirst, None).unwrap()), [a, c, b]);
        assert_eq!(ids(storage.list(ListOrder::Recency, Some(2)).unwrap()), [c, b]);

        // A bump moves an entry to the front
        storage.ingest_at("b", Source::Copy, &[], 4).unwrap();
        assert_eq!(ids(storage.list(ListOrder::Recency, None).unwrap()), [b, c, a]);
    }

    #[test]
    fn nth_is_one_based_in_recency_order() {
        let (storage, _dir) = test_storage();
        let a = add(&storage, "a", 1);
        let b = add(&storage, "b", 2);
        storage.set_pinned(a, true).unwrap();
        assert_eq!(storage.nth(1).unwrap().map(|e| e.id), Some(b), "pins don't reorder");
        assert_eq!(storage.nth(2).unwrap().map(|e| e.id), Some(a));
        assert_eq!(storage.nth(3).unwrap(), None);
        assert_eq!(storage.nth(0).unwrap(), None);
    }

    #[test]
    fn search_finds_term_beyond_preview() {
        let (storage, _dir) = test_storage();
        let id = add(&storage, &format!("{}\nthe hidden needle here", "padding ".repeat(50)), 1);
        add(&storage, "something else entirely", 2);
        assert_eq!(ids(storage.search("needle", 200).unwrap()), [id]);
        assert_eq!(ids(storage.search("hidden needle", 200).unwrap()), [id], "tokens are ANDed");
        assert!(storage.search("needle missing", 200).unwrap().is_empty());
    }

    #[test]
    fn search_case_insensitive() {
        let (storage, _dir) = test_storage();
        let id = add(&storage, "Hello World", 1);
        assert_eq!(ids(storage.search("hello", 200).unwrap()), [id]);
        assert_eq!(ids(storage.search("WORLD", 200).unwrap()), [id]);
        assert_eq!(ids(storage.search("wO", 200).unwrap()), [id], "short-token fallback too");
    }

    #[test]
    fn search_escapes_fts_syntax() {
        let (storage, _dir) = test_storage();
        let id = add(&storage, r#"say "a" OR b* now"#, 1);
        add(&storage, "a or b", 2);
        assert_eq!(ids(storage.search(r#""a" OR b*"#, 200).unwrap()), [id]);
        for q in ["NEAR(", "col:umn", "a AND", "\"\"\"", "*", "(((", "-x", "^st"] {
            storage.search(q, 200).unwrap_or_else(|e| panic!("{q:?}: {e:#}"));
        }
    }

    #[test]
    fn fts_query_quotes_long_tokens_only() {
        assert_eq!(fts_query(r#"ab "quoted" word"#).as_deref(), Some(r#""""quoted""" AND "word""#));
        assert_eq!(fts_query("ab c"), None);
        assert_eq!(fts_query("日本語"), Some("\"日本語\"".to_string()), "3 chars, not bytes");
    }

    #[test]
    fn short_tokens_use_substring_fallback() {
        let (storage, _dir) = test_storage();
        let ab = add(&storage, "xx ab yy", 1);
        let abc = add(&storage, "abc", 2);
        add(&storage, "zz", 3);
        assert_eq!(ids(storage.search("ab", 200).unwrap()), [abc, ab]);
        assert_eq!(ids(storage.search("ab yy", 200).unwrap()), [ab], "mixed short and FTS tokens");
        assert_eq!(ids(storage.search("x", 200).unwrap()), [ab]);
    }

    #[test]
    fn search_orders_pinned_then_recency_and_limits() {
        let (storage, _dir) = test_storage();
        let old = add(&storage, "match one", 1);
        let mid = add(&storage, "match two", 2);
        let new = add(&storage, "match three", 3);
        add(&storage, "no", 4);
        storage.set_pinned(old, true).unwrap();
        assert_eq!(ids(storage.search("match", 200).unwrap()), [old, new, mid]);
        assert_eq!(ids(storage.search("match", 2).unwrap()), [old, new]);
        assert_eq!(ids(storage.search("  ", 200).unwrap()).len(), 4, "empty query lists all");
    }

    #[test]
    fn fts_index_limited_to_first_256k_chars() {
        let (storage, _dir) = test_storage();
        // 262 144 chars ending in the term, built from 2-byte chars: the
        // window counts characters, not bytes.
        let inside = add(&storage, &format!("{}inside", "é".repeat(262_144 - 6)), 1);
        add(&storage, &format!("{}outside", "b".repeat(262_144)), 2);
        assert_eq!(ids(storage.search("inside", 200).unwrap()), [inside]);
        assert!(storage.search("outside", 200).unwrap().is_empty(), "past the window");
    }

    #[test]
    fn fts_rebuild_matches_trigger_index() {
        let (storage, _dir) = test_storage();
        for i in 0..20 {
            add(&storage, &format!("entry number {i} with text"), i);
        }
        let victim = storage.nth(5).unwrap().unwrap().id;
        storage.delete_entry(victim).unwrap();
        storage.ingest_at("entry number 3 with text", Source::Copy, &[], 100).unwrap();
        let before = ids(storage.search("number", 200).unwrap());
        storage.with_conn(|c| {
            c.execute("INSERT INTO entries_fts(entries_fts, rank) VALUES ('integrity-check', 1)", [])
                .unwrap();
            c.execute("INSERT INTO entries_fts(entries_fts) VALUES ('rebuild')", []).unwrap();
        });
        assert_eq!(ids(storage.search("number", 200).unwrap()), before);
        assert_eq!(before.len(), 19);
    }
}

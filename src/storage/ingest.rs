//! The single ingest path: policy (secret filtering, size caps, retention).

use super::query::{ENTRY_COLUMNS, entry_from_row};
use super::{ClipEntry, Source, Storage};
use crate::util;
use anyhow::Result;
use regex::RegexSet;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use std::time::Duration;

/// Default `[storage] max_entry_bytes`: larger copies are not saved.
pub const DEFAULT_MAX_ENTRY_BYTES: u64 = 10 * 1024 * 1024;
/// Default `[storage] max_total_bytes` of unpinned content.
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// What `ingest` refuses to save and how much history it keeps.
/// Built from the config file by `Config::ingest_policy`.
#[derive(Debug, Clone)]
pub struct IngestPolicy {
    /// User `ignore_patterns` first, then the built-in secret patterns
    ignore: RegexSet,
    user_patterns: usize,
    max_entry_bytes: usize,
    max_total_bytes: u64,
    retention: Option<Duration>,
}

impl Default for IngestPolicy {
    /// No ignore patterns, default size caps, no retention.
    fn default() -> Self {
        Self {
            ignore: RegexSet::empty(),
            user_patterns: 0,
            max_entry_bytes: DEFAULT_MAX_ENTRY_BYTES as usize,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            retention: None,
        }
    }
}

impl IngestPolicy {
    /// `ignore` holds `user_patterns` user patterns followed by any built-ins.
    pub fn new(
        ignore: RegexSet,
        user_patterns: usize,
        max_entry_bytes: usize,
        max_total_bytes: u64,
        retention: Option<Duration>,
    ) -> Self {
        Self { ignore, user_patterns, max_entry_bytes, max_total_bytes, retention }
    }

    /// Index of the first ignore pattern matching `content`.
    pub fn ignored_by(&self, content: &str) -> Option<usize> {
        self.ignore.matches(content).iter().next()
    }

    /// Human name of an ignore pattern index, for skip messages.
    pub fn pattern_label(&self, index: usize) -> String {
        if index < self.user_patterns {
            format!("ignore_patterns[{index}]")
        } else {
            "built-in secret pattern".to_string()
        }
    }

    pub fn max_entry_bytes(&self) -> usize {
        self.max_entry_bytes
    }

    pub fn max_total_bytes(&self) -> u64 {
        self.max_total_bytes
    }

    /// `None` = keep unpinned entries forever.
    pub fn retention(&self) -> Option<Duration> {
        self.retention
    }

    /// Number of user patterns (built-ins excluded).
    pub fn user_pattern_count(&self) -> usize {
        self.user_patterns
    }

    /// Total patterns, user and built-in.
    pub fn pattern_count(&self) -> usize {
        self.ignore.len()
    }
}

/// Result of `Storage::ingest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    /// New content: a new entry
    Inserted(ClipEntry),
    /// Content already in history: its `last_used` was refreshed
    Bumped(ClipEntry),
    /// Not saved, by policy
    Skipped(SkipReason),
}

/// Why `ingest` did not save content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    Empty,
    /// The offer carried `x-kde-passwordManagerHint`
    Sensitive,
    /// Index into the policy's pattern set (`IngestPolicy::pattern_label`)
    IgnoredPattern { index: usize },
    TooLarge { size: usize, limit: usize },
}

/// Preview length in characters.
pub const MAX_PREVIEW_CHARS: usize = 100;

/// First `MAX_PREVIEW_CHARS` characters, control characters as spaces.
pub fn make_preview(content: &str) -> String {
    content
        .chars()
        .take(MAX_PREVIEW_CHARS)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

impl Storage {
    /// The single save path: daemon, `copy`, socket and picker undo.
    pub fn ingest(&self, content: &str, source: Source, mime_types: &[String]) -> Result<IngestOutcome> {
        self.ingest_at(content, source, mime_types, now_ms())
    }

    /// `ingest` with an injected clock, for tests.
    pub(crate) fn ingest_at(
        &self,
        content: &str,
        source: Source,
        mime_types: &[String],
        now_ms: i64,
    ) -> Result<IngestOutcome> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (entry, inserted) = insert_or_bump(&tx, content, source, mime_types, now_ms)?;
        tx.commit()?;
        Ok(if inserted { IngestOutcome::Inserted(entry) } else { IngestOutcome::Bumped(entry) })
    }
}

/// Mime type recorded for content whose offer listed no `text/` type.
pub const DEFAULT_MIME: &str = "text/plain;charset=utf-8";

/// The first `text/` type offered, else `DEFAULT_MIME`.
fn chosen_mime(mime_types: &[String]) -> &str {
    mime_types
        .iter()
        .map(String::as_str)
        .find(|t| t.starts_with("text/"))
        .unwrap_or(DEFAULT_MIME)
}

/// Refresh `last_used` of the entry with this content, or insert a new one.
/// Returns the entry and whether it was inserted. Pin, alias, source and
/// `created_at` of an existing entry are kept.
pub(crate) fn insert_or_bump(
    tx: &Transaction<'_>,
    content: &str,
    source: Source,
    mime_types: &[String],
    now_ms: i64,
) -> Result<(ClipEntry, bool)> {
    let hash = util::compute_hash(content);
    let bump = format!("UPDATE entries SET last_used = ?1 WHERE hash = ?2 RETURNING {ENTRY_COLUMNS}");
    if let Some(entry) = tx
        .prepare_cached(&bump)?
        .query_row(params![now_ms, hash], entry_from_row)
        .optional()?
    {
        return Ok((entry, false));
    }

    let insert = format!(
        "INSERT INTO entries (content, hash, size, preview, created_at, last_used, source, mime)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7) RETURNING {ENTRY_COLUMNS}"
    );
    let entry = tx.prepare_cached(&insert)?.query_row(
        params![
            content,
            hash,
            i64::try_from(content.len())?,
            make_preview(content),
            now_ms,
            source.as_str(),
            chosen_mime(mime_types),
        ],
        entry_from_row,
    )?;
    Ok((entry, true))
}

/// Current time in unix milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::test_storage;

    fn inserted(outcome: IngestOutcome) -> ClipEntry {
        match outcome {
            IngestOutcome::Inserted(e) => e,
            other => panic!("expected Inserted, got {other:?}"),
        }
    }

    fn bumped(outcome: IngestOutcome) -> ClipEntry {
        match outcome {
            IngestOutcome::Bumped(e) => e,
            other => panic!("expected Bumped, got {other:?}"),
        }
    }

    #[test]
    fn inserts_new_entry() {
        let (storage, _dir) = test_storage();
        let types = vec!["TEXT".to_string(), "text/plain;charset=utf-8".to_string()];
        let entry = inserted(storage.ingest_at("Hello, clipboard!", Source::Copy, &types, 1_000).unwrap());

        assert!(entry.id > 0);
        assert_eq!(entry.created_at, 1_000);
        assert_eq!(entry.last_used, 1_000);
        assert_eq!(entry.size, 17);
        assert_eq!(entry.preview, "Hello, clipboard!");
        assert!(!entry.pinned);
        assert_eq!(entry.alias, None);
        assert_eq!(entry.source, Source::Copy);
        assert_eq!(entry.mime, "text/plain;charset=utf-8", "first text/ type");

        assert_eq!(storage.get(entry.id).unwrap(), Some(entry.clone()));
        assert_eq!(storage.load_content(entry.id).unwrap(), "Hello, clipboard!");

        let other = inserted(storage.ingest_at("other", Source::Poll, &[], 2_000).unwrap());
        assert!(other.id > entry.id);
        assert_eq!(other.mime, "text/plain;charset=utf-8", "default mime");
    }

    #[test]
    fn duplicate_bumps_last_used_keeps_id() {
        let (storage, _dir) = test_storage();
        let first = inserted(storage.ingest_at("recopied", Source::Poll, &[], 1_000).unwrap());
        inserted(storage.ingest_at("in between", Source::Poll, &[], 2_000).unwrap());

        let again = bumped(storage.ingest_at("recopied", Source::Copy, &[], 3_000).unwrap());
        assert_eq!(again.id, first.id, "same entry, not a duplicate");
        assert_eq!(again.created_at, 1_000, "created_at kept");
        assert_eq!(again.last_used, 3_000, "re-copy counts as most recent use");
        assert_eq!(again.source, Source::Poll, "source of the first save kept");
        assert_eq!(storage.get(first.id).unwrap().unwrap().last_used, 3_000, "persisted");

        let count: i64 = storage.with_conn(|c| {
            c.query_row("SELECT count(*) FROM entries", [], |r| r.get(0)).unwrap()
        });
        assert_eq!(count, 2);
    }

    #[test]
    fn duplicate_preserves_pin_and_alias() {
        let (storage, _dir) = test_storage();
        let entry = inserted(storage.ingest_at("signature", Source::Copy, &[], 1_000).unwrap());
        storage.with_conn(|c| {
            c.execute("UPDATE entries SET pinned = 1, alias = 'sig' WHERE id = ?1", [entry.id])
                .unwrap()
        });

        let again = bumped(storage.ingest_at("signature", Source::Poll, &[], 2_000).unwrap());
        assert!(again.pinned);
        assert_eq!(again.alias.as_deref(), Some("sig"));
    }

    #[test]
    fn preview_truncates_chars_and_sanitizes() {
        let (storage, _dir) = test_storage();
        let entry = inserted(
            storage.ingest_at("line1\nline2\ttab\rcr\u{7}bell", Source::Copy, &[], 1).unwrap(),
        );
        assert_eq!(entry.preview, "line1 line2 tab cr bell");

        // 200 four-byte chars: preview is 100 chars, content is untouched
        let long = "🎉".repeat(200);
        let entry = inserted(storage.ingest_at(&long, Source::Copy, &[], 2).unwrap());
        assert_eq!(entry.preview.chars().count(), MAX_PREVIEW_CHARS);
        assert_eq!(entry.size, 800);
        assert_eq!(storage.load_content(entry.id).unwrap(), long);

        assert_eq!(make_preview("x".repeat(500).as_str()).len(), MAX_PREVIEW_CHARS);
    }

    #[test]
    fn unicode_content_roundtrips() {
        let (storage, _dir) = test_storage();
        for (i, content) in ["Hello 世界 🎉 émojis 日本語テスト", "a\0b", "\u{feff}bom", "\r\n"]
            .iter()
            .enumerate()
        {
            let entry = inserted(storage.ingest_at(content, Source::Copy, &[], i as i64).unwrap());
            assert_eq!(&storage.load_content(entry.id).unwrap(), content);
            assert_eq!(entry.size, content.len());
        }
    }
}

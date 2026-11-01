//! The single ingest path: policy (secret filtering, size caps, retention).

use super::query::{ENTRY_COLUMNS, entry_from_row};
use super::{ClipEntry, Source, Storage};
use crate::util;
use anyhow::Result;
use regex::RegexSet;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
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

    /// One-line reason for a skip log or message. Never includes content.
    pub fn describe_skip(&self, reason: &SkipReason) -> String {
        match reason {
            SkipReason::Empty => "empty".to_string(),
            SkipReason::Sensitive => "marked sensitive by a password manager".to_string(),
            SkipReason::IgnoredPattern { index } => format!("matches {}", self.pattern_label(*index)),
            SkipReason::TooLarge { size, limit } => {
                format!("too large ({size} bytes; max_entry_bytes is {limit})")
            }
        }
    }

    /// Total patterns, user and built-in.
    #[cfg(test)]
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

impl SkipReason {
    /// Short machine-readable reason, as in the socket's `reason=`:
    /// `empty`, `sensitive`, `ignored` or `too_large`.
    pub fn code(&self) -> &'static str {
        match self {
            SkipReason::Empty => "empty",
            SkipReason::Sensitive => "sensitive",
            SkipReason::IgnoredPattern { .. } => "ignored",
            SkipReason::TooLarge { .. } => "too_large",
        }
    }
}

/// Why `content` must not be saved, or `None`. Pure; checks run in order:
/// empty, password-manager hint, size, ignore patterns (only on content
/// within the size cap, so regex work is bounded).
pub fn classify(content: &str, mime_types: &[String], policy: &IngestPolicy) -> Option<SkipReason> {
    if content.is_empty() {
        return Some(SkipReason::Empty);
    }
    if crate::clipboard::is_sensitive_offer(mime_types) {
        return Some(SkipReason::Sensitive);
    }
    if content.len() > policy.max_entry_bytes {
        return Some(SkipReason::TooLarge { size: content.len(), limit: policy.max_entry_bytes });
    }
    policy.ignored_by(content).map(|index| SkipReason::IgnoredPattern { index })
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
        if let Some(reason) = classify(content, mime_types, &self.policy) {
            return Ok(IngestOutcome::Skipped(reason));
        }
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (entry, inserted) = insert_or_bump(&tx, content, source, mime_types, now_ms)?;
        let pruned = enforce_limits(&tx, self.max_entries, &self.policy, now_ms)?;
        tx.commit()?;
        if pruned > 0 {
            super::scrub_deleted(&conn);
        }
        Ok(if inserted { IngestOutcome::Inserted(entry) } else { IngestOutcome::Bumped(entry) })
    }

    /// Apply max entries, retention and the total-size cap without an
    /// insert. Run by `open()` and the daemon tick. Returns how many entries
    /// were deleted.
    pub fn expire(&self) -> Result<usize> {
        self.expire_at(now_ms())
    }

    /// `expire` with an injected clock. Takes the write lock only when a
    /// cheap read shows something to delete.
    pub(crate) fn expire_at(&self, now_ms: i64) -> Result<usize> {
        let mut conn = self.conn();
        if !limits_exceeded(&conn, self.max_entries, &self.policy, now_ms)? {
            return Ok(0);
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deleted = enforce_limits(&tx, self.max_entries, &self.policy, now_ms)?;
        tx.commit()?;
        if deleted > 0 {
            super::scrub_deleted(&conn);
        }
        Ok(deleted)
    }
}

/// `last_used` before which unpinned entries expire, if retention is on.
fn retention_cutoff(policy: &IngestPolicy, now_ms: i64) -> Option<i64> {
    policy
        .retention
        .map(|d| now_ms.saturating_sub(i64::try_from(d.as_millis()).unwrap_or(i64::MAX)))
}

/// Read-only check: would `enforce_limits` delete anything?
fn limits_exceeded(
    conn: &Connection,
    max_entries: usize,
    policy: &IngestPolicy,
    now_ms: i64,
) -> Result<bool> {
    let exceeded = conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM entries WHERE pinned = 0
                        ORDER BY last_used DESC, id DESC LIMIT 1 OFFSET ?1)
             OR (?2 IS NOT NULL AND EXISTS (SELECT 1 FROM entries WHERE pinned = 0 AND last_used < ?2))
             OR (SELECT coalesce(sum(size), 0) FROM entries WHERE pinned = 0) > ?3",
    )?
    .query_row(
        params![
            i64::try_from(max_entries)?,
            retention_cutoff(policy, now_ms),
            i64::try_from(policy.max_total_bytes).unwrap_or(i64::MAX),
        ],
        |r| r.get(0),
    )?;
    Ok(exceeded)
}

/// Delete the oldest unpinned entries beyond `max_entries`.
pub(crate) fn prune_to_max_entries(conn: &Connection, max_entries: usize) -> Result<usize> {
    Ok(conn
        .prepare_cached(
            "DELETE FROM entries WHERE id IN (
               SELECT id FROM entries WHERE pinned = 0
               ORDER BY last_used DESC, id DESC LIMIT -1 OFFSET ?1)",
        )?
        .execute([i64::try_from(max_entries)?])?)
}

/// Delete unpinned entries beyond `max_entries`, older than the retention
/// cutoff, or beyond `max_total_bytes` (newest kept first). Pinned entries
/// are never touched. Returns how many were deleted.
pub(crate) fn enforce_limits(
    conn: &Connection,
    max_entries: usize,
    policy: &IngestPolicy,
    now_ms: i64,
) -> Result<usize> {
    let mut deleted = prune_to_max_entries(conn, max_entries)?;

    if let Some(cutoff) = retention_cutoff(policy, now_ms) {
        deleted += conn
            .prepare_cached("DELETE FROM entries WHERE pinned = 0 AND last_used < ?1")?
            .execute([cutoff])?;
    }

    deleted += conn.prepare_cached(
        "DELETE FROM entries WHERE id IN (
           SELECT id FROM (SELECT id, sum(size) OVER (ORDER BY last_used DESC, id DESC) AS running
                           FROM entries WHERE pinned = 0)
           WHERE running > ?1)",
    )?
    .execute([i64::try_from(policy.max_total_bytes).unwrap_or(i64::MAX)])?;
    Ok(deleted)
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
    use crate::clipboard::PASSWORD_HINT_MIME;
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

    fn storage_with(policy: IngestPolicy) -> (Storage, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let storage = Storage::open(dir.path().to_path_buf(), None, policy).unwrap();
        (storage, dir)
    }

    fn policy(patterns: &[&str], max_entry_bytes: usize) -> IngestPolicy {
        IngestPolicy::new(RegexSet::new(patterns).unwrap(), patterns.len(), max_entry_bytes, 1 << 20, None)
    }

    fn entry_count(storage: &Storage) -> i64 {
        storage.with_conn(|c| c.query_row("SELECT count(*) FROM entries", [], |r| r.get(0)).unwrap())
    }

    #[test]
    fn empty_is_skipped() {
        let (storage, _dir) = test_storage();
        assert_eq!(
            storage.ingest_at("", Source::Copy, &[], 1).unwrap(),
            IngestOutcome::Skipped(SkipReason::Empty)
        );
        assert_eq!(entry_count(&storage), 0);
    }

    #[test]
    fn password_hint_mime_is_skipped() {
        let (storage, _dir) = test_storage();
        let types = vec!["text/plain".to_string(), PASSWORD_HINT_MIME.to_string()];
        assert_eq!(
            storage.ingest_at("hunter2", Source::Watch, &types, 1).unwrap(),
            IngestOutcome::Skipped(SkipReason::Sensitive)
        );
        assert_eq!(entry_count(&storage), 0);
    }

    #[test]
    fn oversize_is_skipped() {
        let (storage, _dir) = storage_with(policy(&[], 16));
        assert_eq!(
            storage.ingest_at(&"x".repeat(17), Source::Copy, &[], 1).unwrap(),
            IngestOutcome::Skipped(SkipReason::TooLarge { size: 17, limit: 16 })
        );
        assert!(matches!(
            storage.ingest_at(&"x".repeat(16), Source::Copy, &[], 2).unwrap(),
            IngestOutcome::Inserted(_)
        ));
        assert_eq!(entry_count(&storage), 1);
    }

    #[test]
    fn ignore_pattern_match_is_skipped() {
        let policy = policy(&["^never$", "(?i)password:"], 1024);
        let label = policy.pattern_label(1);
        let (storage, _dir) = storage_with(policy);
        let outcome = storage.ingest_at("Password: hunter2", Source::Copy, &[], 1).unwrap();
        assert_eq!(outcome, IngestOutcome::Skipped(SkipReason::IgnoredPattern { index: 1 }));
        assert_eq!(label, "ignore_patterns[1]");
        let IngestOutcome::Skipped(reason) = outcome else { unreachable!() };
        assert!(storage.policy().describe_skip(&reason).contains("ignore_patterns[1]"));
        assert!(matches!(
            storage.ingest_at("just text", Source::Copy, &[], 2).unwrap(),
            IngestOutcome::Inserted(_)
        ));
        assert_eq!(entry_count(&storage), 1);
    }

    fn limits(max_entry_bytes: usize, max_total_bytes: u64, retention_days: u64) -> IngestPolicy {
        let retention = (retention_days > 0).then(|| Duration::from_secs(retention_days * DAY_SECS));
        IngestPolicy::new(RegexSet::empty(), 0, max_entry_bytes, max_total_bytes, retention)
    }

    fn storage_limited(max_entries: usize, policy: IngestPolicy) -> (Storage, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let storage = Storage::open(dir.path().to_path_buf(), Some(max_entries), policy).unwrap();
        (storage, dir)
    }

    fn contents(storage: &Storage) -> Vec<String> {
        storage.with_conn(|c| {
            let mut stmt = c.prepare("SELECT content FROM entries ORDER BY last_used, id").unwrap();
            stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
        })
    }

    fn pin(storage: &Storage, id: crate::storage::EntryId) {
        storage.with_conn(|c| c.execute("UPDATE entries SET pinned = 1 WHERE id = ?1", [id]).unwrap());
    }

    const DAY_SECS: u64 = 86_400;
    const DAY_MS: i64 = 86_400_000;

    #[test]
    fn prunes_unpinned_beyond_max_entries() {
        let (storage, _dir) = storage_limited(3, IngestPolicy::default());
        for (i, c) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            storage.ingest_at(c, Source::Copy, &[], i as i64).unwrap();
        }
        assert_eq!(contents(&storage), ["c", "d", "e"], "oldest pruned first");

        // A bump makes an old entry newest, so it survives the next prune
        storage.ingest_at("c", Source::Copy, &[], 10).unwrap();
        storage.ingest_at("f", Source::Copy, &[], 11).unwrap();
        assert_eq!(contents(&storage), ["e", "c", "f"]);
    }

    #[test]
    fn pinned_survive_pruning() {
        let (storage, _dir) = storage_limited(2, IngestPolicy::default());
        let keep = inserted(storage.ingest_at("keep me", Source::Copy, &[], 1).unwrap());
        pin(&storage, keep.id);
        for (i, c) in ["b", "c", "d"].iter().enumerate() {
            storage.ingest_at(c, Source::Copy, &[], 10 + i as i64).unwrap();
        }
        assert_eq!(contents(&storage), ["keep me", "c", "d"], "pinned don't count toward the limit");
    }

    #[test]
    fn expires_unpinned_older_than_retention() {
        let (storage, _dir) = storage_limited(100, limits(1024, 1 << 20, 7));
        let t0 = 1_000 * DAY_MS;
        let old_pin = inserted(storage.ingest_at("old pinned", Source::Copy, &[], t0).unwrap());
        pin(&storage, old_pin.id);
        storage.ingest_at("old", Source::Copy, &[], t0).unwrap();
        storage.ingest_at("six days", Source::Copy, &[], t0 + 2 * DAY_MS).unwrap();

        storage.ingest_at("new", Source::Copy, &[], t0 + 8 * DAY_MS).unwrap();
        assert_eq!(contents(&storage), ["old pinned", "six days", "new"]);

        // expire() applies the same rule without an insert
        assert_eq!(storage.expire_at(t0 + 10 * DAY_MS).unwrap(), 1);
        assert_eq!(contents(&storage), ["old pinned", "new"]);
        assert_eq!(storage.expire_at(t0 + 10 * DAY_MS).unwrap(), 0);
    }

    #[test]
    fn retention_zero_disables_expiry() {
        let config = crate::config::Config::parse("[storage]\nretention_days = 0\n").unwrap();
        let policy = config.ingest_policy().unwrap();
        assert_eq!(policy.retention(), None);

        let (storage, _dir) = storage_limited(100, policy);
        storage.ingest_at("ancient", Source::Copy, &[], 0).unwrap();
        storage.ingest_at("today", Source::Copy, &[], 20_000 * DAY_MS).unwrap();
        assert_eq!(storage.expire_at(40_000 * DAY_MS).unwrap(), 0);
        assert_eq!(contents(&storage), ["ancient", "today"]);
    }

    #[test]
    fn total_bytes_cap_prunes_oldest_unpinned() {
        let (storage, _dir) = storage_limited(100, limits(10, 10, 0));
        let big_pin = inserted(storage.ingest_at("PPPPPPPPPP", Source::Copy, &[], 1).unwrap());
        pin(&storage, big_pin.id);
        storage.ingest_at("aaaa", Source::Copy, &[], 2).unwrap();
        storage.ingest_at("bbbb", Source::Copy, &[], 3).unwrap();
        assert_eq!(contents(&storage).len(), 3, "pinned bytes don't count toward the cap");

        storage.ingest_at("cccc", Source::Copy, &[], 4).unwrap();
        assert_eq!(contents(&storage), ["PPPPPPPPPP", "bbbb", "cccc"], "12 > 10 bytes: oldest pruned");

        // An entry exactly at the cap pushes out every older unpinned entry
        storage.ingest_at("dddddddddd", Source::Copy, &[], 5).unwrap();
        assert_eq!(contents(&storage), ["PPPPPPPPPP", "dddddddddd"]);
    }

    #[test]
    fn classify_order_sensitive_before_size() {
        let policy = policy(&["secret"], 8);
        let hint = vec![PASSWORD_HINT_MIME.to_string()];
        let big_secret = "secret ".repeat(10);
        assert_eq!(classify("", &hint, &policy), Some(SkipReason::Empty));
        assert_eq!(classify(&big_secret, &hint, &policy), Some(SkipReason::Sensitive));
        assert_eq!(
            classify(&big_secret, &[], &policy),
            Some(SkipReason::TooLarge { size: 70, limit: 8 }),
            "size is checked before patterns"
        );
        assert_eq!(classify("secret", &[], &policy), Some(SkipReason::IgnoredPattern { index: 0 }));
        assert_eq!(classify("fine", &[], &policy), None);
    }
}

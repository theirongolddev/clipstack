//! SQLite storage (WAL mode, FTS5 search index). An old `index.json` +
//! content-file store is imported once by `legacy`.

mod ingest;
mod legacy;
mod query;
mod schema;

pub use ingest::{
    DEFAULT_MAX_ENTRY_BYTES, DEFAULT_MAX_IMAGE_BYTES, DEFAULT_MAX_TOTAL_BYTES, IngestOutcome, IngestPolicy, now_ms,
};

use std::fmt;

/// Storage failures callers may need to tell apart (`cli::exit_code_for`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageError {
    /// The database was written by a newer ClipStack.
    NewerSchema { found: i64, supported: i64 },
    /// No entry with this id
    NotFound { id: EntryId },
    /// Pinning would exceed `MAX_PINNED`
    PinLimit { max: usize },
    /// Another entry already has this alias
    AliasTaken(String),
    /// Not `^[a-z0-9][a-z0-9_-]{0,31}$`
    InvalidAlias(String),
    /// `check` found damage it didn't (or couldn't) repair
    Corrupt(String),
    /// `load_content` on an image entry
    NotText { id: EntryId },
    /// `load_image` on a text entry
    NotImage { id: EntryId },
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::NewerSchema { found, supported } => write!(
                f,
                "database schema version {found} is newer than this clipstack supports \
                 ({supported}); upgrade clipstack"
            ),
            StorageError::NotFound { id } => write!(f, "Entry id:{id} not found"),
            StorageError::PinLimit { max } => {
                write!(f, "Maximum pinned entries ({max}) reached. Unpin something first.")
            }
            StorageError::AliasTaken(alias) => {
                write!(f, "Alias @{alias} is already used by another entry")
            }
            StorageError::InvalidAlias(alias) => write!(
                f,
                "Invalid alias {alias:?}: use 1-32 characters from a-z, 0-9, '_' and '-', \
                 starting with a letter or digit"
            ),
            StorageError::Corrupt(what) => write!(f, "{what}"),
            StorageError::NotText { id } => write!(f, "Entry id:{id} is an image, not text"),
            StorageError::NotImage { id } => write!(f, "Entry id:{id} is text, not an image"),
        }
    }
}

impl std::error::Error for StorageError {}

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// SQLite rowid (AUTOINCREMENT, so never reused).
pub type EntryId = i64;

/// Database file name inside the storage dir.
pub const DB_FILE: &str = "clipstack.db";
/// History length when nothing is configured or saved.
pub const DEFAULT_MAX_ENTRIES: usize = 100;
/// Upper bound for `max_entries`.
pub const ABSOLUTE_MAX_ENTRIES: usize = 10_000;
/// Most pinned entries, so the pin list stays a short list.
pub const MAX_PINNED: usize = 25;

/// Longest alias, in characters.
pub const MAX_ALIAS_LEN: usize = 32;

/// `^[a-z0-9][a-z0-9_-]{0,31}$`: safe after `@` in a shell word.
pub fn is_valid_alias(alias: &str) -> bool {
    let mut chars = alias.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && alias.len() <= MAX_ALIAS_LEN
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Sort order of `list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListOrder {
    /// Most recently used first (`list`, `nth`, the `N` entry reference)
    Recency,
    /// Pinned entries first, each group by recency (picker)
    PinnedFirst,
}

/// State of the FTS search index found by `check`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtsState {
    Ok,
    /// Doesn't match the entries; `check --repair` rebuilds it
    Damaged,
    /// Was damaged and `check --repair` rebuilt it
    Rebuilt,
}

/// What `Storage::check` found.
#[derive(Debug, Clone)]
pub struct CheckReport {
    /// `PRAGMA integrity_check` problems; empty when the database is sound
    pub integrity_errors: Vec<String>,
    pub fts: FtsState,
    pub counts: Counts,
    pub legacy_backups: Vec<PathBuf>,
}

impl CheckReport {
    pub fn is_ok(&self) -> bool {
        self.integrity_errors.is_empty() && self.fts != FtsState::Damaged
    }
}

/// History totals for `stats`/`status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub total: usize,
    pub pinned: usize,
    /// Content bytes, pinned included
    pub total_bytes: u64,
    /// Smallest and largest `last_used`
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
}

/// One history entry, without its content (`load_content` reads that).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClipEntry {
    pub id: EntryId,
    /// Unix ms, first time this content was seen
    pub created_at: i64,
    /// Unix ms, last copy or re-copy; the recency sort key
    pub last_used: i64,
    /// Content size in bytes
    pub size: usize,
    /// First 100 chars, control chars replaced by spaces
    pub preview: String,
    pub pinned: bool,
    /// Only set on pinned entries
    pub alias: Option<String>,
    pub source: Source,
    /// Chosen text mime type, e.g. "text/plain;charset=utf-8"; "image/png" for images
    pub mime: String,
    pub kind: Kind,
    /// Image entries only: pixel size from the PNG header
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// What an entry holds: text (`load_content`) or a PNG image (`load_image`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Text,
    Image,
}

impl Kind {
    /// Parse a stored `kind` column (the schema allows only these two).
    pub fn from_db(s: &str) -> Kind {
        if s == "image" { Kind::Image } else { Kind::Text }
    }
}

/// Where an entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Watch,
    Poll,
    Copy,
    Remote,
    Picker,
    Edit,
    Import,
    Unknown,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Watch => "watch",
            Source::Poll => "poll",
            Source::Copy => "copy",
            Source::Remote => "remote",
            Source::Picker => "picker",
            Source::Edit => "edit",
            Source::Import => "import",
            Source::Unknown => "unknown",
        }
    }

    /// Parse a stored `source` column; unknown names map to `Unknown`.
    pub fn from_db(s: &str) -> Source {
        match s {
            "watch" => Source::Watch,
            "poll" => Source::Poll,
            "copy" => Source::Copy,
            "remote" => Source::Remote,
            "picker" => Source::Picker,
            "edit" => Source::Edit,
            "import" => Source::Import,
            _ => Source::Unknown,
        }
    }
}

/// Clipboard history in `<dir>/clipstack.db`. `Send + Sync`; each process
/// opens its own, and SQLite (WAL, `BEGIN IMMEDIATE` writes) arbitrates.
pub struct Storage {
    conn: Mutex<Connection>,
    base_dir: PathBuf,
    db_path: PathBuf,
    max_entries: usize,
    limit_saved: bool,
    policy: IngestPolicy,
}

impl Storage {
    /// Open (creating if needed) the database in `dir`.
    ///
    /// `max_entries` is the CLI/env limit: `Some(n)` is saved in the database
    /// (and prunes at once); `None` uses the saved limit, else the default.
    pub fn open(dir: PathBuf, max_entries: Option<usize>, policy: IngestPolicy) -> Result<Storage> {
        let db_path = dir.join(DB_FILE);
        let mut conn = schema::open_connection(&db_path)?;
        let import = if legacy::detect(&dir) {
            let report = legacy::import(&mut conn, &dir, max_entries, &policy, ingest::now_ms())
                .with_context(|| format!("Failed to import the old store in {dir:?}"))?;
            Some(report)
        } else {
            None
        };

        let stored = get_setting(&conn, SETTING_MAX_ENTRIES)?;
        let limit_saved = stored.is_some() || max_entries.is_some();
        let max_entries = max_entries
            .or_else(|| stored.as_deref().and_then(|v| v.parse().ok()))
            .unwrap_or(DEFAULT_MAX_ENTRIES)
            .clamp(1, ABSOLUTE_MAX_ENTRIES);
        let value = max_entries.to_string();
        if limit_saved && stored.as_deref() != Some(value.as_str()) {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            set_setting(&tx, SETTING_MAX_ENTRIES, &value)?;
            tx.commit()?;
        }

        let storage = Storage {
            conn: Mutex::new(conn),
            base_dir: dir,
            db_path,
            max_entries,
            limit_saved,
            policy,
        };
        if let Some(report) = import {
            // Retention and the size caps wait for the next open, so the
            // notice can warn before anything imported expires.
            storage.print_import_notice(&report)?;
            return Ok(storage);
        }
        // Expired entries are gone before any read. Another process holding
        // the write lock past the busy timeout is not fatal: it expires next time.
        match storage.expire() {
            Err(e) if is_busy(&e) => {}
            other => {
                other.context("Failed to apply storage limits")?;
            }
        }
        Ok(storage)
    }

    /// The one-time stderr message after a legacy import.
    fn print_import_notice(&self, report: &legacy::ImportReport) -> Result<()> {
        let Some(backup) = &report.backup_dir else {
            return Ok(());
        };
        let left_out = report.pruned + report.skipped + report.not_text;
        let advice = if left_out == 0 { " (delete after checking)" } else { "" };
        eprintln!(
            "Imported {} entries from index.json; old files kept in {}/{advice}",
            report.kept,
            backup.display()
        );
        if report.pruned > 0 {
            eprintln!(
                "{} older entries were over the max_entries limit ({}) and were not kept",
                report.pruned, report.limit
            );
        }
        if report.unpinned > 0 {
            eprintln!(
                "{} older pinned entries were unpinned to stay within the {MAX_PINNED}-pin limit",
                report.unpinned
            );
        }
        if report.skipped > 0 {
            eprintln!(
                "{} entries were not imported because the storage policy rejects them \
                 (empty, too large, or matching an ignore or secret pattern)",
                report.skipped
            );
        }
        if report.not_text > 0 {
            eprintln!("{} content files are not UTF-8 text and were not imported", report.not_text);
        }
        if left_out > 0 {
            eprintln!(
                "The backup dir is the only copy of the {left_out} entries that were not imported or \
                 not kept; keep it until you have saved what you need from it"
            );
        }
        if let Some(retention) = self.policy.retention() {
            let cutoff = ingest::now_ms()
                .saturating_sub(i64::try_from(retention.as_millis()).unwrap_or(i64::MAX));
            let old: i64 = self.conn().query_row(
                "SELECT count(*) FROM entries WHERE pinned = 0 AND last_used < ?1",
                [cutoff],
                |r| r.get(0),
            )?;
            if old > 0 {
                eprintln!(
                    "{old} entries are older than {} days and will expire at the next start",
                    retention.as_secs() / 86_400
                );
            }
        }
        Ok(())
    }

    /// `legacy-backup-*` dirs left by the import, oldest first.
    pub fn legacy_backup_dirs(&self) -> Vec<PathBuf> {
        legacy::backup_dirs(&self.base_dir)
    }

    /// Delete the `legacy-backup-*` dirs (old history as plaintext files):
    /// only regular files inside them, then the dirs; symlinks are never
    /// followed. Returns how many dirs were removed and those left because
    /// they hold something else.
    pub fn remove_legacy_backups(&self) -> Result<(usize, Vec<PathBuf>)> {
        legacy::remove_backup_dirs(&self.base_dir)
    }

    /// Run SQLite's `integrity_check` and the FTS index's `integrity-check`
    /// (rank 1: compared against the entries). With `repair`, a damaged FTS
    /// index is rebuilt from the entries, then the WAL is checkpointed.
    /// Core database damage is reported, never repaired here.
    pub fn check(&self, repair: bool) -> Result<CheckReport> {
        let (integrity_errors, fts) = {
            let mut conn = self.conn();
            let integrity_errors: Vec<String> = conn
                .prepare("PRAGMA integrity_check")?
                .query_map([], |r| r.get::<_, String>(0))?
                .filter(|row| !matches!(row.as_deref(), Ok("ok")))
                .collect::<rusqlite::Result<_>>()?;
            let mut fts = fts_state(&conn)?;
            if repair {
                if fts == FtsState::Damaged {
                    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    tx.execute("INSERT INTO entries_fts(entries_fts) VALUES ('rebuild')", [])
                        .context("Failed to rebuild the search index")?;
                    tx.commit()?;
                    if fts_state(&conn)? == FtsState::Ok {
                        fts = FtsState::Rebuilt;
                    }
                }
                // Best effort: busy while another process reads
                let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
            }
            (integrity_errors, fts)
        };
        Ok(CheckReport {
            integrity_errors,
            fts,
            counts: self.counts()?,
            legacy_backups: self.legacy_backup_dirs(),
        })
    }

    pub fn default_dir() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("clipd")
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// True when a limit is saved in the database (status label "saved").
    pub fn limit_is_saved(&self) -> bool {
        self.limit_saved
    }

    pub fn policy(&self) -> &IngestPolicy {
        &self.policy
    }

    /// Flip an entry's pin; returns the new state. Unpinning clears the alias.
    pub fn toggle_pin(&self, id: EntryId) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pinned = pinned_state(&tx, id)?.ok_or(StorageError::NotFound { id })?;
        write_pin(&tx, id, !pinned)?;
        tx.commit()?;
        Ok(!pinned)
    }

    /// Pin or unpin (unpinning clears the alias). A missing id is a no-op,
    /// so undoing a change to an entry deleted meanwhile does nothing.
    pub fn set_pinned(&self, id: EntryId, pinned: bool) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        match pinned_state(&tx, id)? {
            Some(current) if current != pinned => write_pin(&tx, id, pinned)?,
            _ => {}
        }
        tx.commit()?;
        Ok(())
    }

    /// Set (pinning the entry if needed) or remove an alias. Removing keeps
    /// the pin.
    pub fn set_alias(&self, id: EntryId, alias: Option<&str>) -> Result<()> {
        if let Some(name) = alias.filter(|a| !is_valid_alias(a)) {
            return Err(StorageError::InvalidAlias(name.to_string()).into());
        }
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pinned = pinned_state(&tx, id)?.ok_or(StorageError::NotFound { id })?;
        match alias {
            Some(name) => {
                if !pinned {
                    check_pin_room(&tx)?;
                }
                let result = tx
                    .prepare_cached("UPDATE entries SET pinned = 1, alias = ?2 WHERE id = ?1")?
                    .execute(rusqlite::params![id, name]);
                match result {
                    Err(e) if is_unique_violation(&e) => {
                        return Err(StorageError::AliasTaken(name.to_string()).into());
                    }
                    other => {
                        other?;
                    }
                }
            }
            None => {
                tx.prepare_cached("UPDATE entries SET alias = NULL WHERE id = ?1")?.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Delete one entry; false if it didn't exist.
    pub fn delete_entry(&self, id: EntryId) -> Result<bool> {
        let conn = self.conn();
        let deleted = conn.prepare_cached("DELETE FROM entries WHERE id = ?1")?.execute([id])? > 0;
        if deleted {
            scrub_deleted(&conn);
        }
        Ok(deleted)
    }

    /// Delete every entry, pinned ones too; settings are kept. Returns the count.
    pub fn clear(&self) -> Result<usize> {
        let conn = self.conn();
        let deleted = conn.execute("DELETE FROM entries", [])?;
        if deleted > 0 {
            scrub_deleted(&conn);
        }
        Ok(deleted)
    }

    /// Rerun a scrub that an earlier delete (in any process) couldn't finish.
    /// `None` if none is pending, else whether it completed this time.
    pub fn retry_pending_scrub(&self) -> Result<Option<bool>> {
        let conn = self.conn();
        if get_setting(&conn, SETTING_SCRUB_PENDING)?.is_none() {
            return Ok(None);
        }
        Ok(Some(scrub_deleted(&conn)))
    }

    /// The connection. A panic while it was held can't leave SQLite in a bad
    /// state (transactions roll back on drop), so a poisoned lock is reused.
    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Raw connection access for tests that inspect or damage the database.
    #[cfg(test)]
    pub(crate) fn with_conn<R>(&self, f: impl FnOnce(&Connection) -> R) -> R {
        f(&self.conn())
    }
}

/// An entry's pin state, or `None` if it doesn't exist.
fn pinned_state(conn: &Connection, id: EntryId) -> Result<Option<bool>> {
    Ok(conn
        .prepare_cached("SELECT pinned FROM entries WHERE id = ?1")?
        .query_row([id], |r| r.get(0))
        .optional()?)
}

fn check_pin_room(conn: &Connection) -> Result<()> {
    let pinned: i64 = conn
        .prepare_cached("SELECT count(*) FROM entries WHERE pinned = 1")?
        .query_row([], |r| r.get(0))?;
    if usize::try_from(pinned)? >= MAX_PINNED {
        return Err(StorageError::PinLimit { max: MAX_PINNED }.into());
    }
    Ok(())
}

/// Pin (checking `MAX_PINNED`) or unpin and clear the alias.
fn write_pin(conn: &Connection, id: EntryId, pinned: bool) -> Result<()> {
    if pinned {
        check_pin_room(conn)?;
        conn.prepare_cached("UPDATE entries SET pinned = 1 WHERE id = ?1")?.execute([id])?;
    } else {
        conn.prepare_cached("UPDATE entries SET pinned = 0, alias = NULL WHERE id = ?1")?
            .execute([id])?;
    }
    Ok(())
}

fn is_unique_violation(err: &rusqlite::Error) -> bool {
    matches!(err, rusqlite::Error::SqliteFailure(e, _)
        if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE)
}

/// `settings` key of the saved history length.
const SETTING_MAX_ENTRIES: &str = "max_entries";

fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached("SELECT value FROM settings WHERE key = ?1")?
        .query_row([key], |r| r.get(0))
        .optional()?)
}

fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
    )?
    .execute([key, value])?;
    Ok(())
}

/// `SQLITE_BUSY`/`SQLITE_LOCKED`: another connection holds the lock.
fn is_busy(err: &anyhow::Error) -> bool {
    err.downcast_ref::<rusqlite::Error>().is_some_and(schema::is_busy)
}

/// The FTS index's own consistency check against the entries table.
fn fts_state(conn: &Connection) -> Result<FtsState> {
    match conn.execute("INSERT INTO entries_fts(entries_fts, rank) VALUES ('integrity-check', 1)", []) {
        Ok(_) => Ok(FtsState::Ok),
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::DatabaseCorrupt => {
            Ok(FtsState::Damaged)
        }
        Err(e) => Err(e).context("Search index check failed"),
    }
}

/// `settings` key set while a scrub couldn't finish; the daemon tick retries.
const SETTING_SCRUB_PENDING: &str = "scrub_pending";

/// After a deleting write: move the WAL into the database and truncate it
/// (so frames that carried deleted plaintext are gone), then return free
/// pages to the filesystem. Best effort: while another connection holds a
/// read transaction the checkpoint can't finish; that is recorded in
/// `settings` (shared by every process) so the daemon's tick, or the next
/// delete, retries. Returns whether the checkpoint completed.
fn scrub_deleted(conn: &Connection) -> bool {
    // Row: (busy, wal frames, checkpointed frames); busy = 0 means truncated
    let done = matches!(
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get::<_, i64>(0)),
        Ok(0)
    );
    let _ = conn.execute_batch("PRAGMA incremental_vacuum");
    let pending = matches!(get_setting(conn, SETTING_SCRUB_PENDING), Ok(Some(_)));
    if !done && !pending {
        let _ = set_setting(conn, SETTING_SCRUB_PENDING, "1");
    } else if done && pending {
        let _ = conn.execute("DELETE FROM settings WHERE key = ?1", [SETTING_SCRUB_PENDING]);
    }
    done
}

/// A `Storage` in a fresh temp dir with the default policy.
#[cfg(test)]
pub(crate) fn test_storage() -> (Storage, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let storage = Storage::open(dir.path().to_path_buf(), None, IngestPolicy::default()).unwrap();
    (storage, dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    // ======== check (1-15) ========

    /// Wipe the FTS index while the entries stay: `check` must see the mismatch.
    fn damage_fts(storage: &Storage) {
        storage.with_conn(|c| {
            c.execute("INSERT INTO entries_fts(entries_fts) VALUES ('delete-all')", []).unwrap();
        });
    }

    #[test]
    fn check_ok_on_fresh_db() {
        let (storage, _dir) = test_storage();
        storage.ingest_at("hello world", Source::Copy, &[], 1).unwrap();
        let report = storage.check(false).unwrap();
        assert!(report.is_ok(), "{report:?}");
        assert!(report.integrity_errors.is_empty());
        assert_eq!(report.fts, FtsState::Ok);
        assert_eq!(report.counts.total, 1);
        assert!(report.legacy_backups.is_empty());
    }

    #[test]
    fn check_without_repair_reports_damaged_fts() {
        let (storage, _dir) = test_storage();
        storage.ingest_at("hello world", Source::Copy, &[], 1).unwrap();
        damage_fts(&storage);
        let report = storage.check(false).unwrap();
        assert_eq!(report.fts, FtsState::Damaged);
        assert!(!report.is_ok());
        assert!(storage.search("world", 10).unwrap().is_empty(), "index really is empty");
    }

    #[test]
    fn check_repair_rebuilds_fts_after_delete_all() {
        let (storage, _dir) = test_storage();
        storage.ingest_at("hello world", Source::Copy, &[], 1).unwrap();
        storage.ingest_at("another entry", Source::Copy, &[], 2).unwrap();
        damage_fts(&storage);

        let report = storage.check(true).unwrap();
        assert_eq!(report.fts, FtsState::Rebuilt);
        assert!(report.is_ok(), "{report:?}");
        assert_eq!(storage.search("world", 10).unwrap().len(), 1);
        assert_eq!(storage.check(false).unwrap().fts, FtsState::Ok);
    }

    #[test]
    fn check_repair_on_healthy_db_changes_nothing() {
        let (storage, _dir) = test_storage();
        storage.ingest_at("hello world", Source::Copy, &[], 1).unwrap();
        let report = storage.check(true).unwrap();
        assert_eq!(report.fts, FtsState::Ok, "no rebuild when the index is fine");
        assert!(report.is_ok());
    }

    const DAY_MS: i64 = 86_400_000;

    fn retention_days(days: u64) -> IngestPolicy {
        IngestPolicy::new(
            regex::RegexSet::empty(),
            0,
            1024,
            1 << 20,
            Some(Duration::from_secs(days * 86_400)),
        )
    }

    fn entry_count(storage: &Storage) -> i64 {
        storage.with_conn(|c| c.query_row("SELECT count(*) FROM entries", [], |r| r.get(0)).unwrap())
    }

    fn open(dir: &tempfile::TempDir, max_entries: Option<usize>) -> Storage {
        Storage::open(dir.path().to_path_buf(), max_entries, IngestPolicy::default()).unwrap()
    }

    fn fill(storage: &Storage, n: usize) {
        for i in 0..n {
            storage.ingest_at(&format!("entry {i}"), Source::Copy, &[], i as i64 + 1).unwrap();
        }
    }

    #[test]
    fn test_open_without_limit_uses_stored_limit_and_does_not_prune() {
        let dir = tempfile::TempDir::new().unwrap();
        // User configured a higher limit once and filled history past the default
        fill(&open(&dir, Some(500)), 150);

        // A later invocation with no flag/env (e.g. auto-started daemon, `list`)
        let storage = open(&dir, None);
        assert_eq!(storage.max_entries(), 500, "should reuse stored limit");
        assert_eq!(entry_count(&storage), 150, "must not prune to the default");
        assert!(storage.limit_is_saved());
    }

    #[test]
    fn test_open_without_limit_on_fresh_dir_uses_default() {
        let dir = tempfile::TempDir::new().unwrap();
        let storage = open(&dir, None);
        assert_eq!(storage.max_entries(), DEFAULT_MAX_ENTRIES);
        assert!(!storage.limit_is_saved(), "nothing stored until a limit is given");
        let rows: i64 = storage
            .with_conn(|c| c.query_row("SELECT count(*) FROM settings", [], |r| r.get(0)).unwrap());
        assert_eq!(rows, 0);
    }

    #[test]
    fn test_open_with_explicit_limit_persists_and_prunes() {
        let dir = tempfile::TempDir::new().unwrap();
        fill(&open(&dir, None), 20);

        let storage = open(&dir, Some(5));
        assert_eq!(entry_count(&storage), 5, "pruned at open");

        // Limit is remembered by the next unconfigured invocation
        assert_eq!(open(&dir, None).max_entries(), 5);
    }

    #[test]
    fn test_max_entries_clamps_low() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(open(&dir, Some(0)).max_entries(), 1);
        assert_eq!(open(&dir, None).max_entries(), 1, "the clamped value is stored");
    }

    #[test]
    fn test_max_entries_clamps_high() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(open(&dir, Some(999_999)).max_entries(), ABSOLUTE_MAX_ENTRIES);
    }

    #[test]
    fn test_max_entries_clamps_stored_value() {
        let dir = tempfile::TempDir::new().unwrap();
        open(&dir, None).with_conn(|c| {
            c.execute("INSERT INTO settings (key, value) VALUES ('max_entries', '0')", []).unwrap()
        });
        assert_eq!(open(&dir, None).max_entries(), 1);
    }

    #[test]
    fn limit_is_saved_after_explicit_limit() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(!open(&dir, None).limit_is_saved());
        assert!(open(&dir, Some(42)).limit_is_saved());
        let storage = open(&dir, None);
        assert!(storage.limit_is_saved());
        assert_eq!(storage.max_entries(), 42);
    }

    fn add(storage: &Storage, content: &str, at: i64) -> ClipEntry {
        match storage.ingest_at(content, Source::Copy, &[], at).unwrap() {
            ingest::IngestOutcome::Inserted(e) | ingest::IngestOutcome::Bumped(e) => e,
            other => panic!("not saved: {other:?}"),
        }
    }

    fn storage_error(err: &anyhow::Error) -> Option<&StorageError> {
        err.downcast_ref::<StorageError>()
    }

    #[test]
    fn test_toggle_pin() {
        let (storage, _dir) = test_storage();
        let entry = add(&storage, "test content", 1);
        assert!(!entry.pinned);

        assert!(storage.toggle_pin(entry.id).unwrap());
        assert!(storage.get(entry.id).unwrap().unwrap().pinned, "persisted");
        assert!(!storage.toggle_pin(entry.id).unwrap());
        assert!(!storage.get(entry.id).unwrap().unwrap().pinned);
    }

    #[test]
    fn test_toggle_pin_nonexistent() {
        let (storage, _dir) = test_storage();
        let err = storage.toggle_pin(999).unwrap_err();
        assert_eq!(storage_error(&err), Some(&StorageError::NotFound { id: 999 }));
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn toggle_pin_respects_max_pinned() {
        let (storage, _dir) = test_storage();
        let ids: Vec<EntryId> = (0..MAX_PINNED)
            .map(|i| {
                let e = add(&storage, &format!("content {i}"), i as i64);
                assert!(storage.toggle_pin(e.id).unwrap());
                e.id
            })
            .collect();
        let extra = add(&storage, "extra", 100);

        let err = storage.toggle_pin(extra.id).unwrap_err();
        assert_eq!(storage_error(&err), Some(&StorageError::PinLimit { max: MAX_PINNED }));
        assert!(err.to_string().contains("Maximum pinned"), "{err}");
        let err = storage.set_pinned(extra.id, true).unwrap_err();
        assert_eq!(storage_error(&err), Some(&StorageError::PinLimit { max: MAX_PINNED }));
        assert_eq!(storage.counts().unwrap().pinned, MAX_PINNED);

        // Unpinning one frees a slot
        assert!(!storage.toggle_pin(ids[0]).unwrap());
        assert!(storage.toggle_pin(extra.id).unwrap());
        // Re-pinning an already pinned entry is not a new pin
        storage.set_pinned(extra.id, true).unwrap();
    }

    #[test]
    fn test_set_pinned() {
        let (storage, _dir) = test_storage();
        let entry = add(&storage, "test content", 1);
        storage.set_pinned(entry.id, true).unwrap();
        assert!(storage.get(entry.id).unwrap().unwrap().pinned);
        storage.set_pinned(entry.id, false).unwrap();
        assert!(!storage.get(entry.id).unwrap().unwrap().pinned);
        // A missing id is a no-op (picker undo of a since-deleted entry)
        storage.set_pinned(999, true).unwrap();
    }

    #[test]
    fn test_pinned_count() {
        let (storage, _dir) = test_storage();
        assert_eq!(storage.counts().unwrap().pinned, 0);
        let one = add(&storage, "one", 1);
        let two = add(&storage, "two", 2);
        add(&storage, "three", 3);
        storage.toggle_pin(one.id).unwrap();
        storage.toggle_pin(two.id).unwrap();
        assert_eq!(storage.counts().unwrap().pinned, 2);
        storage.toggle_pin(one.id).unwrap();

        let counts = storage.counts().unwrap();
        assert_eq!(
            counts,
            Counts { total: 3, pinned: 1, total_bytes: 11, oldest: Some(1), newest: Some(3) }
        );
        storage.clear().unwrap();
        assert_eq!(
            storage.counts().unwrap(),
            Counts { total: 0, pinned: 0, total_bytes: 0, oldest: None, newest: None }
        );
    }

    #[test]
    fn set_alias_validates_name() {
        let (storage, _dir) = test_storage();
        let entry = add(&storage, "addr", 1);
        let long = "a".repeat(33);
        for bad in ["", "Sig", "-sig", "_sig", "s g", "sig!", "é", long.as_str()] {
            let err = storage.set_alias(entry.id, Some(bad)).unwrap_err();
            assert_eq!(
                storage_error(&err),
                Some(&StorageError::InvalidAlias(bad.to_string())),
                "{bad:?}"
            );
        }
        let max = "a".repeat(32);
        for good in ["sig", "0", "my-addr_2", max.as_str()] {
            storage.set_alias(entry.id, Some(good)).unwrap();
            assert_eq!(storage.by_alias(good).unwrap().map(|e| e.id), Some(entry.id));
        }
        let err = storage.set_alias(999, Some("ok")).unwrap_err();
        assert_eq!(storage_error(&err), Some(&StorageError::NotFound { id: 999 }));
    }

    #[test]
    fn alias_is_unique() {
        let (storage, _dir) = test_storage();
        let a = add(&storage, "a", 1);
        let b = add(&storage, "b", 2);
        storage.set_alias(a.id, Some("sig")).unwrap();
        storage.set_alias(a.id, Some("sig")).unwrap(); // same entry again is fine

        let err = storage.set_alias(b.id, Some("sig")).unwrap_err();
        assert_eq!(storage_error(&err), Some(&StorageError::AliasTaken("sig".to_string())));
        assert_eq!(storage.by_alias("sig").unwrap().map(|e| e.id), Some(a.id));
        assert!(!storage.get(b.id).unwrap().unwrap().pinned, "failed alias doesn't pin");
    }

    #[test]
    fn set_alias_pins_entry() {
        let (storage, _dir) = test_storage();
        let entry = add(&storage, "addr", 1);
        storage.set_alias(entry.id, Some("home")).unwrap();
        let got = storage.get(entry.id).unwrap().unwrap();
        assert!(got.pinned);
        assert_eq!(got.alias.as_deref(), Some("home"));

        // Removing the alias keeps the pin
        storage.set_alias(entry.id, None).unwrap();
        let got = storage.get(entry.id).unwrap().unwrap();
        assert!(got.pinned);
        assert_eq!(got.alias, None);
        assert_eq!(storage.by_alias("home").unwrap(), None);
    }

    #[test]
    fn set_alias_respects_max_pinned() {
        let (storage, _dir) = test_storage();
        for i in 0..MAX_PINNED {
            let e = add(&storage, &format!("p{i}"), i as i64);
            storage.set_pinned(e.id, true).unwrap();
        }
        let extra = add(&storage, "extra", 100);
        let err = storage.set_alias(extra.id, Some("x")).unwrap_err();
        assert_eq!(storage_error(&err), Some(&StorageError::PinLimit { max: MAX_PINNED }));
    }

    #[test]
    fn unpin_clears_alias() {
        let (storage, _dir) = test_storage();
        let entry = add(&storage, "addr", 1);
        storage.set_alias(entry.id, Some("home")).unwrap();
        assert!(!storage.toggle_pin(entry.id).unwrap());
        assert_eq!(storage.get(entry.id).unwrap().unwrap().alias, None);

        storage.set_alias(entry.id, Some("home")).unwrap();
        storage.set_pinned(entry.id, false).unwrap();
        assert_eq!(storage.get(entry.id).unwrap().unwrap().alias, None);
        assert_eq!(storage.by_alias("home").unwrap(), None);
    }

    #[test]
    fn test_delete_entry() {
        let (storage, _dir) = test_storage();
        let entry = add(&storage, "to delete", 1);
        assert!(storage.delete_entry(entry.id).unwrap());
        assert_eq!(storage.get(entry.id).unwrap(), None);
        assert_eq!(entry_count(&storage), 0);
        // Deleting a missing entry is not an error
        assert!(!storage.delete_entry(entry.id).unwrap());
    }

    #[test]
    fn clear_removes_all() {
        let dir = tempfile::TempDir::new().unwrap();
        let storage = open(&dir, Some(42));
        add(&storage, "one", 1);
        let two = add(&storage, "two", 2);
        storage.set_alias(two.id, Some("two")).unwrap();

        assert_eq!(storage.clear().unwrap(), 2, "pinned entries too");
        assert_eq!(entry_count(&storage), 0);
        assert_eq!(storage.clear().unwrap(), 0);
        // The saved limit survives a clear
        assert_eq!(open(&dir, None).max_entries(), 42);
    }

    /// Every file SQLite keeps for the database, as one byte string.
    fn db_file_bytes(storage: &Storage) -> Vec<u8> {
        let mut bytes = Vec::new();
        for suffix in ["", "-wal", "-shm"] {
            let mut path = storage.db_path().as_os_str().to_owned();
            path.push(suffix);
            if let Ok(b) = std::fs::read(&path) {
                bytes.extend(b);
            }
        }
        bytes
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    #[test]
    fn deleted_secret_not_in_db_files() {
        // One connection, so nothing blocks the TRUNCATE checkpoint.
        let (storage, _dir) = test_storage();
        add(&storage, "keep this ordinary text", 1);

        let small = "SMALLSECRET-7f3a9c1e";
        // Large enough for overflow pages
        let large = format!("LARGESECRET-b42d {}", "q".repeat(20_000));
        let small_entry = add(&storage, small, 2);
        add(&storage, &large, 3);
        let bytes = db_file_bytes(&storage);
        assert!(contains(&bytes, small) && contains(&bytes, "LARGESECRET-b42d"), "sanity: stored");

        assert!(storage.delete_entry(small_entry.id).unwrap());
        assert!(!contains(&db_file_bytes(&storage), small), "deleted entry left plaintext");

        storage.clear().unwrap();
        let bytes = db_file_bytes(&storage);
        assert!(!contains(&bytes, "LARGESECRET-b42d"), "cleared entry left plaintext");
        assert!(!contains(&bytes, "ordinary text"));
    }

    #[test]
    fn pruned_entry_not_in_db_files() {
        // One connection, so nothing blocks the TRUNCATE checkpoint.
        let dir = tempfile::TempDir::new().unwrap();
        let storage = open(&dir, Some(2));
        let marker = "ROLLEDOFF-c81e5d0a";
        add(&storage, marker, 1);
        assert!(contains(&db_file_bytes(&storage), marker), "sanity: stored");

        add(&storage, "second entry", 2);
        add(&storage, "third entry", 3);
        assert_eq!(entry_count(&storage), 2, "marker pruned by max_entries");
        assert!(!contains(&db_file_bytes(&storage), marker), "pruned entry left plaintext");
    }

    #[test]
    fn blocked_scrub_is_pending_until_retried() {
        let (storage, _dir) = test_storage();
        // Don't wait the full busy timeout for the reader below
        storage.with_conn(|c| c.busy_timeout(Duration::from_millis(50)).unwrap());
        let marker = "BLOCKEDSCRUB-5e2b77d1";
        let entry = add(&storage, marker, 1);
        assert_eq!(storage.retry_pending_scrub().unwrap(), None, "nothing pending yet");

        // An open read transaction (a picker) blocks the TRUNCATE checkpoint
        let reader = Connection::open(storage.db_path()).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let _: i64 = reader.query_row("SELECT count(*) FROM entries", [], |r| r.get(0)).unwrap();
        assert!(storage.delete_entry(entry.id).unwrap());
        assert!(contains(&db_file_bytes(&storage), marker), "sanity: WAL still holds it");
        assert_eq!(storage.retry_pending_scrub().unwrap(), Some(false), "still blocked");

        reader.execute_batch("ROLLBACK").unwrap();
        assert_eq!(storage.retry_pending_scrub().unwrap(), Some(true));
        assert!(!contains(&db_file_bytes(&storage), marker), "retried scrub removed it");
        assert_eq!(storage.retry_pending_scrub().unwrap(), None, "flag cleared");
    }

    #[test]
    fn test_concurrent_writers_lose_no_updates() {
        // Separate Storage handles model separate processes (daemon, picker,
        // serve) each with their own connection.
        let dir = tempfile::TempDir::new().unwrap();
        open(&dir, None);
        let handles: Vec<_> = (0..8)
            .map(|t| {
                let base = dir.path().to_path_buf();
                std::thread::spawn(move || {
                    let storage = Storage::open(base, None, IngestPolicy::default()).unwrap();
                    for i in 0..10 {
                        storage.ingest(&format!("writer {t} item {i}"), Source::Copy, &[]).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(entry_count(&open(&dir, None)), 80, "every concurrent save must survive");
    }

    #[test]
    fn test_concurrent_saves_dont_corrupt() {
        let (storage, _dir) = test_storage();
        let storage = std::sync::Arc::new(storage);
        let handles: Vec<_> = (0..10)
            .map(|i| {
                let storage = std::sync::Arc::clone(&storage);
                std::thread::spawn(move || {
                    storage.ingest(&format!("thread {i} content"), Source::Copy, &[]).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(entry_count(&storage), 10, "no concurrent save may be lost");
        let check: String =
            storage.with_conn(|c| c.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap());
        assert_eq!(check, "ok");
    }

    #[test]
    fn concurrent_first_open_creates_schema_once() {
        let dir = tempfile::TempDir::new().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|t| {
                let base = dir.path().to_path_buf();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let storage = Storage::open(base, Some(50), IngestPolicy::default()).unwrap();
                    storage.ingest(&format!("first open {t}"), Source::Copy, &[]).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let storage = open(&dir, None);
        assert_eq!(entry_count(&storage), 8);
        assert_eq!(storage.max_entries(), 50);
    }

    #[test]
    fn open_expires_old_unpinned_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let now = ingest::now_ms();
        {
            let storage = Storage::open(dir.path().to_path_buf(), None, IngestPolicy::default()).unwrap();
            storage.ingest_at("ten days old", Source::Copy, &[], now - 10 * DAY_MS).unwrap();
            storage.ingest_at("yesterday", Source::Copy, &[], now - DAY_MS).unwrap();
        }
        let storage = Storage::open(dir.path().to_path_buf(), None, retention_days(7)).unwrap();
        let left: Vec<String> = storage.with_conn(|c| {
            let mut stmt = c.prepare("SELECT content FROM entries").unwrap();
            stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
        });
        assert_eq!(left, ["yesterday"], "open() expires before any read");
    }

    #[test]
    fn expire_returns_zero_without_write_when_nothing_expired() {
        let dir = tempfile::TempDir::new().unwrap();
        let storage = Storage::open(dir.path().to_path_buf(), None, retention_days(7)).unwrap();
        storage.ingest("fresh", Source::Copy, &[]).unwrap();

        // Another connection holds the write lock: a writing expire() would
        // wait out the busy timeout and fail.
        let other = Connection::open(storage.db_path()).unwrap();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        let start = Instant::now();
        assert_eq!(storage.expire().unwrap(), 0);
        assert!(start.elapsed() < Duration::from_secs(1), "took {:?}", start.elapsed());
        other.execute_batch("ROLLBACK").unwrap();
        assert_eq!(entry_count(&storage), 1);
    }
}

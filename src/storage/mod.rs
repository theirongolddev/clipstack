//! SQLite storage (WAL mode, FTS5 search index).
//!
//! `json` is the old `index.json` + content-file store. It stays the live
//! storage for the CLI, daemon and picker until commit 1-12.

// The SQLite storage has no callers until commit 1-12 switches the CLI,
// daemon and picker to it; this allow is removed there.
#![allow(dead_code)]

#[warn(dead_code)] // json is the live storage until 1-12: keep its lint on
pub mod json;
mod ingest;
mod query;
mod schema;

pub use ingest::{DEFAULT_MAX_ENTRY_BYTES, DEFAULT_MAX_TOTAL_BYTES, IngestPolicy};

use std::fmt;

/// Storage failures callers may need to tell apart (exit 4 in Step 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageError {
    /// The database was written by a newer ClipStack.
    NewerSchema { found: i64, supported: i64 },
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::NewerSchema { found, supported } => write!(
                f,
                "database schema version {found} is newer than this clipstack supports \
                 ({supported}); upgrade clipstack"
            ),
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
    /// Chosen text mime type, e.g. "text/plain;charset=utf-8"
    pub mime: String,
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
    matches!(
        err.downcast_ref::<rusqlite::Error>().and_then(rusqlite::Error::sqlite_error_code),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

/// After a deleting write: move the WAL into the database and truncate it
/// (so frames that carried deleted plaintext are gone), then return free
/// pages to the filesystem. Best effort: while another connection holds a
/// read transaction the checkpoint can't finish, and the next delete retries.
fn scrub_deleted(conn: &Connection) {
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    let _ = conn.execute_batch("PRAGMA incremental_vacuum");
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

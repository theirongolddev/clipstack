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
use rusqlite::Connection;
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
    policy: IngestPolicy,
}

impl Storage {
    /// Open (creating if needed) the database in `dir`.
    pub fn open(dir: PathBuf, max_entries: Option<usize>, policy: IngestPolicy) -> Result<Storage> {
        let db_path = dir.join(DB_FILE);
        let conn = schema::open_connection(&db_path)?;
        let max_entries = max_entries
            .unwrap_or(DEFAULT_MAX_ENTRIES)
            .clamp(1, ABSOLUTE_MAX_ENTRIES);
        let storage = Storage { conn: Mutex::new(conn), base_dir: dir, db_path, max_entries, policy };
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

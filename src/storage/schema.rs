//! Connection setup, PRAGMAs and schema migrations.

use super::StorageError;
use crate::perms;
use anyhow::{Context, Result};
use rusqlite::{Connection, TransactionBehavior};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Newest schema this binary understands (`PRAGMA user_version`).
pub const SCHEMA_VERSION: i64 = 2;

/// `PRAGMA application_id`: "CLPS".
pub const APPLICATION_ID: i64 = 0x434C_5053;

/// How long a writer waits for another process's write transaction.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Schema v1 (`user_version` 1). `content` is the last stored column, so
/// reading the small columns never walks a large entry's overflow pages.
/// `search_text` is a virtual generated column, so only the first 256 Ki
/// characters of each entry are indexed; the FTS table is external content
/// over it and is kept in sync by the triggers. A new database is created as
/// v1 and then migrated, so the migration runs on every store.
const SCHEMA_V1: &str = "
CREATE TABLE entries (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  hash        BLOB    NOT NULL UNIQUE CHECK (length(hash) = 32),
  size        INTEGER NOT NULL CHECK (size >= 0),
  preview     TEXT    NOT NULL,
  created_at  INTEGER NOT NULL,
  last_used   INTEGER NOT NULL,
  pinned      INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
  alias       TEXT    UNIQUE CHECK (alias IS NULL OR (pinned = 1 AND length(alias) BETWEEN 1 AND 32)),
  source      TEXT    NOT NULL DEFAULT 'unknown',
  mime        TEXT    NOT NULL DEFAULT 'text/plain;charset=utf-8',
  content     TEXT    NOT NULL,
  search_text TEXT    GENERATED ALWAYS AS (substr(content, 1, 262144)) VIRTUAL
);

CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;

CREATE VIRTUAL TABLE entries_fts USING fts5(
  search_text, content='entries', content_rowid='id', tokenize='trigram'
);
INSERT INTO entries_fts(entries_fts, rank) VALUES ('secure-delete', 1);
";

/// Indexes and FTS triggers of `entries` (v1 and v2 alike). Dropping the
/// table drops them, so the v2 rebuild creates them again.
const ENTRIES_INDEXES_AND_TRIGGERS: &str = "
CREATE INDEX entries_by_recency      ON entries (last_used DESC, id DESC);
CREATE INDEX entries_unpinned_by_age ON entries (pinned, last_used);

CREATE TRIGGER entries_ai AFTER INSERT ON entries BEGIN
  INSERT INTO entries_fts(rowid, search_text) VALUES (new.id, new.search_text);
END;
CREATE TRIGGER entries_ad AFTER DELETE ON entries BEGIN
  INSERT INTO entries_fts(entries_fts, rowid, search_text) VALUES ('delete', old.id, old.search_text);
END;
CREATE TRIGGER entries_au AFTER UPDATE OF content ON entries BEGIN
  INSERT INTO entries_fts(entries_fts, rowid, search_text) VALUES ('delete', old.id, old.search_text);
  INSERT INTO entries_fts(rowid, search_text) VALUES (new.id, new.search_text);
END;
";

/// Migration v1 -> v2 (images). The plan's additive `ALTER TABLE ADD COLUMN`
/// would append `kind`/`width`/`height` after `content`, so every list read
/// would walk the overflow pages of large text entries; the table is
/// rebuilt instead with the small columns first and the two large ones
/// (`content`, then the PNG `image`) last. Ids are copied, so the FTS index
/// (keyed by rowid) stays valid, and the AUTOINCREMENT counter is carried
/// over by the caller. An image entry has `content = ''` (nothing indexed).
const ENTRIES_V2: &str = "
CREATE TABLE entries_v2 (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  hash        BLOB    NOT NULL UNIQUE CHECK (length(hash) = 32),
  size        INTEGER NOT NULL CHECK (size >= 0),
  preview     TEXT    NOT NULL,
  created_at  INTEGER NOT NULL,
  last_used   INTEGER NOT NULL,
  pinned      INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
  alias       TEXT    UNIQUE CHECK (alias IS NULL OR (pinned = 1 AND length(alias) BETWEEN 1 AND 32)),
  source      TEXT    NOT NULL DEFAULT 'unknown',
  mime        TEXT    NOT NULL DEFAULT 'text/plain;charset=utf-8',
  kind        TEXT    NOT NULL DEFAULT 'text' CHECK (kind IN ('text', 'image')),
  width       INTEGER CHECK (width IS NULL OR width > 0),
  height      INTEGER CHECK (height IS NULL OR height > 0),
  content     TEXT    NOT NULL,
  search_text TEXT    GENERATED ALWAYS AS (substr(content, 1, 262144)) VIRTUAL,
  image       BLOB,
  CHECK (CASE kind
           WHEN 'image' THEN image IS NOT NULL AND width IS NOT NULL AND height IS NOT NULL
           ELSE image IS NULL AND width IS NULL AND height IS NULL
         END)
);
INSERT INTO entries_v2 (id, hash, size, preview, created_at, last_used, pinned, alias, source, mime, content)
  SELECT id, hash, size, preview, created_at, last_used, pinned, alias, source, mime, content FROM entries;
DROP TABLE entries;
ALTER TABLE entries_v2 RENAME TO entries;
";

/// Rebuild `entries` as v2 inside the caller's transaction.
fn migrate_v1_to_v2(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    use rusqlite::OptionalExtension;
    // DROP TABLE removes the sqlite_sequence row; without it a deleted top
    // id would be handed out again
    let seq: Option<i64> = tx
        .query_row("SELECT seq FROM sqlite_sequence WHERE name = 'entries'", [], |r| r.get(0))
        .optional()?;
    tx.execute_batch(ENTRIES_V2)?;
    tx.execute_batch(ENTRIES_INDEXES_AND_TRIGGERS)?;
    if let Some(seq) = seq {
        tx.execute("DELETE FROM sqlite_sequence WHERE name = 'entries'", [])?;
        tx.execute("INSERT INTO sqlite_sequence (name, seq) VALUES ('entries', ?1)", [seq])?;
    }
    Ok(())
}

/// Open `path`, set the connection PRAGMAs and bring the schema to
/// `SCHEMA_VERSION`.
pub fn open_connection(path: &Path) -> Result<Connection> {
    prepare_private_files(path)?;
    let mut conn =
        Connection::open(path).with_context(|| format!("Failed to open database: {path:?}"))?;
    conn.busy_timeout(BUSY_TIMEOUT)?;

    // Creation-only, and it must come before `journal_mode = WAL`: switching
    // to WAL writes the header of a new file, after which auto_vacuum can no
    // longer be enabled without a VACUUM. On an existing file this is a no-op.
    conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;

    enable_wal(&conn, path)?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "secure_delete", "ON")?;
    conn.pragma_update(None, "trusted_schema", "OFF")?;
    // Sorts and temp indexes stay in memory: no plaintext temp files
    conn.pragma_update(None, "temp_store", "MEMORY")?;

    register_fold(&conn)?;

    apply_migrations(&mut conn).with_context(|| format!("Failed to migrate database: {path:?}"))?;
    Ok(conn)
}

/// SQL `fold(text)`: Rust's Unicode `to_lowercase`, so the short-token
/// search folds case the same way for "É" as for "E" (SQLite's `lower()`
/// only folds ASCII). The query side is folded with the same function.
fn register_fold(conn: &Connection) -> Result<()> {
    use rusqlite::functions::FunctionFlags;
    conn.create_scalar_function(
        "fold",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC | FunctionFlags::SQLITE_INNOCUOUS,
        |ctx| Ok(ctx.get::<Option<String>>(0)?.map(|s| fold(&s))),
    )?;
    Ok(())
}

/// Case folding shared by the SQL `fold()` and the search query.
pub(super) fn fold(text: &str) -> String {
    text.to_lowercase()
}

/// `SQLITE_BUSY`/`SQLITE_LOCKED`: another connection holds the lock.
pub(super) fn is_busy(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

/// `journal_mode = WAL`. Switching a new file to WAL needs an exclusive lock
/// and SQLite fails at once (no busy handler) while another process is
/// opening the same file, so retry until `BUSY_TIMEOUT`. On a file that is
/// already WAL this succeeds first time.
fn enable_wal(conn: &Connection, path: &Path) -> Result<()> {
    let deadline = Instant::now() + BUSY_TIMEOUT;
    loop {
        match conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0)) {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => return Ok(()),
            Ok(mode) => anyhow::bail!("Failed to enable WAL mode on {path:?} (got {mode})"),
            Err(e) if is_busy(&e) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to enable WAL mode on {path:?}"));
            }
        }
    }
}

/// `<db>-wal` and `<db>-shm`, which SQLite creates next to the database.
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

/// Make the storage dir 0700 and pre-create the database 0600, so SQLite
/// (which gives `-wal`/`-shm` the database file's mode) never writes a
/// world-readable file. Existing files left by an older umask are tightened.
fn prepare_private_files(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        perms::ensure_private_dir(dir)
            .with_context(|| format!("Failed to create storage dir: {dir:?}"))?;
    }
    perms::private_file_options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("Failed to create database: {path:?}"))?;

    for file in [path.to_path_buf(), sidecar(path, "-wal"), sidecar(path, "-shm")] {
        let Ok(meta) = fs::symlink_metadata(&file) else {
            continue; // -wal/-shm only exist while a connection is open
        };
        if meta.file_type().is_file() && meta.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(&file, fs::Permissions::from_mode(perms::FILE_MODE))
                .with_context(|| format!("Failed to tighten permissions: {file:?}"))?;
        }
    }
    Ok(())
}

fn user_version(conn: &Connection) -> Result<i64> {
    Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

fn check_supported(found: i64) -> Result<()> {
    if found > SCHEMA_VERSION {
        return Err(StorageError::NewerSchema { found, supported: SCHEMA_VERSION }.into());
    }
    Ok(())
}

/// Create or upgrade the schema. The version is re-read under the write lock
/// so two processes opening a fresh file can't both create the tables.
fn apply_migrations(conn: &mut Connection) -> Result<()> {
    let found = user_version(conn)?;
    check_supported(found)?;
    if found == SCHEMA_VERSION {
        return Ok(());
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let found = user_version(&tx)?;
    check_supported(found)?;
    if found == 0 {
        tx.pragma_update(None, "application_id", APPLICATION_ID)?;
        tx.execute_batch(SCHEMA_V1)?;
        tx.execute_batch(ENTRIES_INDEXES_AND_TRIGGERS)?;
        tx.pragma_update(None, "user_version", 1)?;
    }
    if found <= 1 {
        migrate_v1_to_v2(&tx)?;
        tx.pragma_update(None, "user_version", 2)?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn db(dir: &TempDir) -> std::path::PathBuf {
        dir.path().join("clipstack.db")
    }

    fn names(conn: &Connection, kind: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_schema WHERE type = ?1 ORDER BY name")
            .unwrap();
        stmt.query_map([kind], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn pragma_i64(conn: &Connection, name: &str) -> i64 {
        conn.pragma_query_value(None, name, |r| r.get(0)).unwrap()
    }

    #[test]
    fn content_and_image_are_the_last_stored_columns() {
        // Small columns first: reading size/pinned/last_used/kind (limits,
        // lists) must not walk a large entry's overflow pages.
        let dir = TempDir::new().unwrap();
        let conn = open_connection(&db(&dir)).unwrap();
        let mut stmt = conn.prepare("SELECT name, hidden FROM pragma_table_xinfo('entries')").unwrap();
        let stored: Vec<String> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .filter(|(_, hidden)| *hidden == 0)
            .map(|(name, _)| name)
            .collect();
        assert_eq!(stored[stored.len() - 2..], ["content", "image"], "{stored:?}");
    }

    /// A v1 database as the Step 1 binary left it, with `rows` entries
    /// (the newest one pinned with an alias) and the top id deleted.
    fn v1_store(path: &Path, rows: i64) {
        let conn = Connection::open(path).unwrap();
        conn.pragma_update(None, "application_id", APPLICATION_ID).unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute_batch(ENTRIES_INDEXES_AND_TRIGGERS).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        for i in 1..=rows + 1 {
            let mut hash = [0u8; 32];
            hash[0] = u8::try_from(i).unwrap();
            conn.execute(
                "INSERT INTO entries (content, hash, size, preview, created_at, last_used, source)
                 VALUES (?1, ?2, length(?1), ?1, ?3, ?3, 'copy')",
                rusqlite::params![format!("old entry number {i}"), hash.to_vec(), i * 1000],
            )
            .unwrap();
        }
        conn.execute("UPDATE entries SET pinned = 1, alias = 'sig' WHERE id = ?1", [rows]).unwrap();
        conn.execute("DELETE FROM entries WHERE id = ?1", [rows + 1]).unwrap();
    }

    #[test]
    fn migrates_populated_v1_store_to_v2() {
        let dir = TempDir::new().unwrap();
        let path = db(&dir);
        v1_store(&path, 5);

        let conn = open_connection(&path).unwrap();
        assert_eq!(pragma_i64(&conn, "user_version"), 2);
        type Row = (i64, String, String, Option<i64>, Option<i64>, bool, Option<String>);
        let rows: Vec<Row> = conn
            .prepare("SELECT id, content, kind, width, height, image IS NULL, alias FROM entries ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows.len(), 5);
        for (i, (id, content, kind, width, height, no_image, alias)) in rows.iter().enumerate() {
            assert_eq!(*id, i as i64 + 1);
            assert_eq!(content, &format!("old entry number {id}"));
            assert_eq!(kind, "text");
            assert_eq!((*width, *height, *no_image), (None, None, true));
            assert_eq!(alias.as_deref(), (*id == 5).then_some("sig"));
        }

        // The search index, triggers and indexes survive the rebuild
        let hits: i64 = conn
            .query_row("SELECT count(*) FROM entries_fts WHERE entries_fts MATCH '\"number 3\"'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hits, 1);
        conn.execute("INSERT INTO entries_fts(entries_fts, rank) VALUES ('integrity-check', 1)", [])
            .unwrap();
        assert_eq!(names(&conn, "trigger"), ["entries_ad", "entries_ai", "entries_au"]);
        let indexes = names(&conn, "index");
        assert!(indexes.contains(&"entries_by_recency".to_string()), "{indexes:?}");
        assert!(indexes.contains(&"entries_unpinned_by_age".to_string()), "{indexes:?}");
        assert!(!names(&conn, "table").contains(&"entries_v2".to_string()));

        // AUTOINCREMENT: the deleted top id 6 is never reused
        conn.execute(
            "INSERT INTO entries (content, hash, size, preview, created_at, last_used)
             VALUES ('new', zeroblob(32), 3, 'new', 1, 1)",
            [],
        )
        .unwrap();
        assert_eq!(conn.last_insert_rowid(), 7);
        let check: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
        assert_eq!(check, "ok");
    }

    #[test]
    fn kind_and_image_must_agree() {
        let dir = TempDir::new().unwrap();
        let conn = open_connection(&db(&dir)).unwrap();
        let insert = |kind: &str, image: Option<&[u8]>, w: Option<i64>| {
            conn.execute(
                "INSERT INTO entries (content, hash, size, preview, created_at, last_used, kind, image, width, height)
                 VALUES ('', randomblob(32), 1, 'p', 1, 1, ?1, ?2, ?3, ?3)",
                rusqlite::params![kind, image, w],
            )
        };
        assert!(insert("image", Some(b"png"), Some(1)).is_ok());
        assert!(insert("image", None, Some(1)).is_err(), "image entry without bytes");
        assert!(insert("image", Some(b"png"), None).is_err(), "image entry without dimensions");
        assert!(insert("text", Some(b"png"), None).is_err(), "text entry with bytes");
        assert!(insert("gif", None, None).is_err());
    }

    #[test]
    fn creates_entries_settings_and_fts() {
        let dir = TempDir::new().unwrap();
        let conn = open_connection(&db(&dir)).unwrap();

        let tables = names(&conn, "table");
        for t in ["entries", "settings", "entries_fts"] {
            assert!(tables.contains(&t.to_string()), "{t} missing from {tables:?}");
        }
        let triggers = names(&conn, "trigger");
        assert_eq!(triggers, ["entries_ad", "entries_ai", "entries_au"]);
        let indexes = names(&conn, "index");
        assert!(indexes.contains(&"entries_by_recency".to_string()));
        assert!(indexes.contains(&"entries_unpinned_by_age".to_string()));

        assert_eq!(pragma_i64(&conn, "user_version"), SCHEMA_VERSION);
        assert_eq!(pragma_i64(&conn, "application_id"), 0x434C_5053);
        assert_eq!(pragma_i64(&conn, "auto_vacuum"), 2, "INCREMENTAL");

        // The trigger keeps the trigram index in sync
        conn.execute(
            "INSERT INTO entries (content, hash, size, preview, created_at, last_used)
             VALUES ('Hello World', zeroblob(32), 11, 'Hello World', 1, 1)",
            [],
        )
        .unwrap();
        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM entries_fts WHERE entries_fts MATCH '\"WORLD\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }

    #[test]
    fn open_is_idempotent() {
        let dir = TempDir::new().unwrap();
        {
            let conn = open_connection(&db(&dir)).unwrap();
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('max_entries', '42')",
                [],
            )
            .unwrap();
        }
        let conn = open_connection(&db(&dir)).unwrap();
        let value: String = conn
            .query_row("SELECT value FROM settings WHERE key = 'max_entries'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(value, "42", "reopening must not recreate the schema");
        assert_eq!(pragma_i64(&conn, "user_version"), SCHEMA_VERSION);
    }

    #[test]
    fn rejects_newer_user_version() {
        let dir = TempDir::new().unwrap();
        {
            let conn = open_connection(&db(&dir)).unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
        }
        let err = open_connection(&db(&dir)).unwrap_err();
        match err.downcast_ref::<super::super::StorageError>() {
            Some(super::super::StorageError::NewerSchema { found, supported }) => {
                assert_eq!(*found, SCHEMA_VERSION + 1);
                assert_eq!(*supported, SCHEMA_VERSION);
            }
            other => panic!("expected NewerSchema, got {other:?} ({err:#})"),
        }
    }

    #[test]
    fn pragmas_wal_secure_delete_temp_store_memory() {
        let dir = TempDir::new().unwrap();
        let conn = open_connection(&db(&dir)).unwrap();
        let journal: String = conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
        assert_eq!(pragma_i64(&conn, "synchronous"), 1, "NORMAL");
        assert_eq!(pragma_i64(&conn, "secure_delete"), 1);
        assert_eq!(pragma_i64(&conn, "trusted_schema"), 0);
        assert_eq!(pragma_i64(&conn, "temp_store"), 2, "MEMORY");
        assert_eq!(pragma_i64(&conn, "busy_timeout"), 5000);

        // FTS5 secure-delete is a persistent option of the index
        let secure: i64 = conn
            .query_row(
                "SELECT v FROM entries_fts_config WHERE k = 'secure-delete'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(secure, 1);
    }

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn sidecars(db: &Path) -> [std::path::PathBuf; 3] {
        let with = |suffix: &str| {
            let mut name = db.as_os_str().to_owned();
            name.push(suffix);
            std::path::PathBuf::from(name)
        };
        [db.to_path_buf(), with("-wal"), with("-shm")]
    }

    #[test]
    fn db_and_wal_files_are_0600() {
        let dir = TempDir::new().unwrap();
        let path = db(&dir);
        let conn = open_connection(&path).unwrap();
        conn.execute("INSERT INTO settings (key, value) VALUES ('k', 'v')", []).unwrap();

        // -wal and -shm exist while the connection is open
        for file in sidecars(&path) {
            assert!(file.exists(), "{file:?} should exist");
            assert_eq!(mode(&file), 0o600, "{file:?}");
        }
    }

    #[test]
    fn tightens_existing_0644_db() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = db(&dir);
        let first = open_connection(&path).unwrap();
        first.execute("INSERT INTO settings (key, value) VALUES ('k', 'v')", []).unwrap();
        // Simulate files left world-readable by an older umask
        for file in sidecars(&path) {
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        }

        let _second = open_connection(&path).unwrap();
        for file in sidecars(&path) {
            assert_eq!(mode(&file), 0o600, "{file:?}");
        }
    }
}

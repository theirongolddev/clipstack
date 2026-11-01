//! Connection setup, PRAGMAs and schema migrations.

use super::StorageError;
use crate::perms;
use anyhow::{Context, Result};
use rusqlite::{Connection, TransactionBehavior};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Newest schema this binary understands (`PRAGMA user_version`).
pub const SCHEMA_VERSION: i64 = 1;

/// `PRAGMA application_id`: "CLPS".
pub const APPLICATION_ID: i64 = 0x434C_5053;

/// How long a writer waits for another process's write transaction.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Schema v1. `search_text` is a virtual generated column, so only the first
/// 256 Ki characters of each entry are indexed; the FTS table is external
/// content over it and is kept in sync by the triggers.
const SCHEMA_V1: &str = "
CREATE TABLE entries (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  content     TEXT    NOT NULL,
  hash        BLOB    NOT NULL UNIQUE CHECK (length(hash) = 32),
  size        INTEGER NOT NULL CHECK (size >= 0),
  preview     TEXT    NOT NULL,
  created_at  INTEGER NOT NULL,
  last_used   INTEGER NOT NULL,
  pinned      INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
  alias       TEXT    UNIQUE CHECK (alias IS NULL OR (pinned = 1 AND length(alias) BETWEEN 1 AND 32)),
  source      TEXT    NOT NULL DEFAULT 'unknown',
  mime        TEXT    NOT NULL DEFAULT 'text/plain;charset=utf-8',
  search_text TEXT    GENERATED ALWAYS AS (substr(content, 1, 262144)) VIRTUAL
);
CREATE INDEX entries_by_recency      ON entries (last_used DESC, id DESC);
CREATE INDEX entries_unpinned_by_age ON entries (pinned, last_used);

CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;

CREATE VIRTUAL TABLE entries_fts USING fts5(
  search_text, content='entries', content_rowid='id', tokenize='trigram'
);
INSERT INTO entries_fts(entries_fts, rank) VALUES ('secure-delete', 1);

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

    let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        anyhow::bail!("Failed to enable WAL mode on {path:?} (got {mode})");
    }
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "secure_delete", "ON")?;
    conn.pragma_update(None, "trusted_schema", "OFF")?;
    // Sorts and temp indexes stay in memory: no plaintext temp files
    conn.pragma_update(None, "temp_store", "MEMORY")?;

    apply_migrations(&mut conn).with_context(|| format!("Failed to migrate database: {path:?}"))?;
    Ok(conn)
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
        tx.pragma_update(None, "user_version", 1)?;
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

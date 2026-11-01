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

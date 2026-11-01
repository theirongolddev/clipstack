//! One-time import of the old JSON store (`index.json` + `<ms>[_nnn].txt`).
//!
//! Crash-safe and idempotent: entries are upserted by content hash in one
//! transaction, and the old files are moved to `legacy-backup-<ms>/` only
//! after it commits. Nothing is deleted: content the ingest policy rejects
//! (empty, ignore and secret patterns, oversize) is not imported, but its
//! file still goes to the backup.

use super::ingest::{DEFAULT_MIME, IngestPolicy, classify, make_preview, prune_to_max_entries};
use super::{
    ABSOLUTE_MAX_ENTRIES, DEFAULT_MAX_ENTRIES, MAX_PINNED, SETTING_MAX_ENTRIES, get_setting,
    set_setting,
};
use crate::{perms, util};
use anyhow::{Context, Result};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const INDEX_FILE: &str = "index.json";
/// The old store's lock; taken so a still-running old daemon can't rewrite
/// `index.json` mid-import. It holds no data and is never moved.
const LOCK_FILE: &str = "index.lock";
const BACKUP_PREFIX: &str = "legacy-backup-";

/// `index.json`. Only the fields the import needs; serde ignores the rest
/// (`size`, `preview`, `hash`: recomputed from the content).
#[derive(Deserialize)]
struct LegacyIndex {
    max_entries: Option<usize>,
    #[serde(default)]
    entries: Vec<LegacyEntry>,
}

#[derive(Deserialize)]
struct LegacyEntry {
    id: String,
    timestamp: i64,
    #[serde(default)]
    pinned: bool,
}

/// What a scan of the store dir found.
struct LegacyScan {
    index_file: bool,
    /// `index.json` `max_entries`, when it could be parsed
    max_entries: Option<usize>,
    /// Indexed entries with a content file, then orphan content files
    items: Vec<Item>,
    /// `.tmp` leftovers of interrupted old writes
    tmp_files: Vec<PathBuf>,
}

struct Item {
    path: PathBuf,
    created_at: i64,
    last_used: i64,
    pinned: bool,
}

/// Result of an import, for the one-time message.
#[derive(Debug, Default)]
pub(super) struct ImportReport {
    /// Distinct imported entries still in the history after max_entries pruning
    pub kept: usize,
    /// Distinct imported entries max_entries pruning removed again
    pub pruned: usize,
    /// The max_entries limit the import pruned to
    pub limit: usize,
    /// Pinned entries unpinned to stay within `MAX_PINNED` (oldest first)
    pub unpinned: usize,
    /// Content files that are not UTF-8 text: moved to the backup, not imported
    pub not_text: usize,
    /// Content files the ingest policy rejects: moved to the backup, not imported
    pub skipped: usize,
    /// Where the old files went; `None` if there was nothing to move
    pub backup_dir: Option<PathBuf>,
}

/// Points where tests inject a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage {
    BeforeCommit,
    BeforeMove,
}

/// Unix ms prefix of an old content file stem: `<ms>` or `<ms>_<n>`.
fn id_millis(stem: &str) -> Option<i64> {
    let mut parts = stem.split('_');
    let ms = parts.next()?;
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let valid = all_digits(ms) && parts.next().is_none_or(all_digits) && parts.next().is_none();
    valid.then(|| ms.parse().ok()).flatten()
}

/// `^\d+(_\d+)?\.txt$`
fn content_stem(name: &str) -> Option<&str> {
    name.strip_suffix(".txt").filter(|stem| id_millis(stem).is_some())
}

/// True if `dir` holds an old store: `index.json` or a content file.
pub(super) fn detect(dir: &Path) -> bool {
    if dir.join(INDEX_FILE).exists() {
        return true;
    }
    fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.file_name().to_str().is_some_and(|n| content_stem(n).is_some()))
    })
}

/// Regular files directly in `dir` (never follows symlinks).
fn regular_files(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("Failed to read {dir:?}"))? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        if let Ok(name) = entry.file_name().into_string() {
            files.push((name, entry.path()));
        }
    }
    Ok(files)
}

fn scan(dir: &Path) -> Result<LegacyScan> {
    let index_path = dir.join(INDEX_FILE);
    let index_file = index_path.is_file();
    let index = if index_file {
        match fs::read(&index_path).map_err(anyhow::Error::from).and_then(|data| {
            serde_json::from_slice::<LegacyIndex>(&data).map_err(anyhow::Error::from)
        }) {
            Ok(index) => Some(index),
            Err(e) => {
                eprintln!("[import] index.json is unreadable ({e}); importing the content files alone");
                None
            }
        }
    } else {
        None
    };

    // Content files by stem (id)
    let mut content: HashMap<String, PathBuf> = HashMap::new();
    let mut tmp_files = Vec::new();
    for (name, path) in regular_files(dir)? {
        if let Some(stem) = content_stem(&name) {
            content.insert(stem.to_owned(), path);
        } else if name.ends_with(".tmp") {
            tmp_files.push(path);
        }
    }

    let mut items = Vec::new();
    let max_entries = index.as_ref().and_then(|i| i.max_entries);
    for entry in index.map(|i| i.entries).unwrap_or_default() {
        // Only ids that name a content file found above: never a path built
        // from index text. Removing it also drops repeated ids.
        let Some(path) = content.remove(&entry.id) else {
            continue;
        };
        items.push(Item {
            path,
            created_at: id_millis(&entry.id).unwrap_or(entry.timestamp),
            last_used: entry.timestamp,
            pinned: entry.pinned,
        });
    }
    // What's left are orphans the index doesn't list
    for (stem, path) in content {
        let ms = id_millis(&stem).unwrap_or(0);
        items.push(Item { path, created_at: ms, last_used: ms, pinned: false });
    }
    Ok(LegacyScan { index_file, max_entries, items, tmp_files })
}

/// Import the old store in `dir`. Each entry goes through `classify`, like
/// any ingest. `explicit_limit` is the CLI/env `max_entries`; the import
/// prunes to the effective limit (retention and the total-size cap are left
/// to the next open, so the notice can warn first).
pub(super) fn import(
    conn: &mut Connection,
    dir: &Path,
    explicit_limit: Option<usize>,
    policy: &IngestPolicy,
    now_ms: i64,
) -> Result<ImportReport> {
    import_with(conn, dir, explicit_limit, policy, now_ms, &|_| Ok(()))
}

pub(super) fn import_with(
    conn: &mut Connection,
    dir: &Path,
    explicit_limit: Option<usize>,
    policy: &IngestPolicy,
    now_ms: i64,
    hook: &dyn Fn(Stage) -> Result<()>,
) -> Result<ImportReport> {
    let lock_path = dir.join(LOCK_FILE);
    let lock = perms::private_file_options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("Failed to open {lock_path:?}"))?;
    lock.lock().with_context(|| format!("Failed to lock {lock_path:?}"))?;

    // Re-scan under the lock: another process may have imported meanwhile
    let mut scan = scan(dir)?;
    scan.items.sort_by_key(|i| (i.last_used, i.created_at));

    let mut report = ImportReport::default();
    let mut to_move: Vec<PathBuf> = Vec::new();
    let mut hashes: HashSet<Vec<u8>> = HashSet::new();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    {
        let mut upsert = tx.prepare(
            "INSERT INTO entries (hash, size, preview, created_at, last_used, pinned, source, mime, content)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'import', ?7, ?8)
             ON CONFLICT (hash) DO UPDATE SET
               last_used  = max(last_used, excluded.last_used),
               created_at = min(created_at, excluded.created_at),
               pinned     = max(pinned, excluded.pinned)",
        )?;
        for item in scan.items {
            let bytes = match fs::read(&item.path) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e).with_context(|| format!("Failed to read {:?}", item.path)),
            };
            to_move.push(item.path);
            let Ok(content) = String::from_utf8(bytes) else {
                report.not_text += 1;
                continue;
            };
            if classify(&content, &[], policy).is_some() {
                report.skipped += 1;
                continue;
            }
            let hash = util::compute_hash(&content);
            upsert.execute(params![
                &hash,
                i64::try_from(content.len())?,
                make_preview(&content),
                item.created_at,
                item.last_used,
                item.pinned,
                DEFAULT_MIME,
                content,
            ])?;
            hashes.insert(hash);
        }
    }

    if get_setting(&tx, SETTING_MAX_ENTRIES)?.is_none()
        && let Some(limit) = scan.max_entries
    {
        let limit = limit.clamp(1, ABSOLUTE_MAX_ENTRIES);
        set_setting(&tx, SETTING_MAX_ENTRIES, &limit.to_string())?;
    }
    let limit = explicit_limit
        .or_else(|| get_setting(&tx, SETTING_MAX_ENTRIES).ok().flatten()?.parse().ok())
        .unwrap_or(DEFAULT_MAX_ENTRIES)
        .clamp(1, ABSOLUTE_MAX_ENTRIES);
    // The old store had no pin limit: keep the most recently used pins
    report.unpinned = tx.execute(
        "UPDATE entries SET pinned = 0, alias = NULL WHERE id IN (
           SELECT id FROM entries WHERE pinned = 1
           ORDER BY last_used DESC, id DESC LIMIT -1 OFFSET ?1)",
        [i64::try_from(MAX_PINNED)?],
    )?;
    prune_to_max_entries(&tx, limit)?;
    report.limit = limit;
    {
        let mut exists = tx.prepare("SELECT EXISTS (SELECT 1 FROM entries WHERE hash = ?1)")?;
        for hash in &hashes {
            if exists.query_row([hash], |r| r.get::<_, bool>(0))? {
                report.kept += 1;
            } else {
                report.pruned += 1;
            }
        }
    }
    if report.kept > 0 {
        set_setting(&tx, "legacy_imported_at", &now_ms.to_string())?;
        set_setting(&tx, "legacy_imported_count", &report.kept.to_string())?;
    }
    hook(Stage::BeforeCommit)?;
    tx.commit()?;

    hook(Stage::BeforeMove)?;
    if scan.index_file {
        to_move.push(dir.join(INDEX_FILE));
    }
    to_move.extend(scan.tmp_files);
    if !to_move.is_empty() {
        report.backup_dir = Some(move_to_backup(dir, &to_move, now_ms)?);
    }
    drop(lock);
    Ok(report)
}

/// Create `legacy-backup-<ms>/` (0700) and move `files` into it (0600).
fn move_to_backup(dir: &Path, files: &[PathBuf], now_ms: i64) -> Result<PathBuf> {
    let mut stamp = now_ms;
    let backup = loop {
        let candidate = dir.join(format!("{BACKUP_PREFIX}{stamp}"));
        if !candidate.exists() {
            break candidate;
        }
        stamp += 1;
    };
    perms::ensure_private_dir(&backup).with_context(|| format!("Failed to create {backup:?}"))?;
    for file in files {
        let Some(name) = file.file_name() else { continue };
        fs::rename(file, backup.join(name))
            .with_context(|| format!("Failed to move {file:?} into {backup:?}"))?;
    }
    perms::tighten_dir(&backup).with_context(|| format!("Failed to tighten {backup:?}"))?;
    Ok(backup)
}

/// `legacy-backup-*` dirs in the store, oldest first.
pub(super) fn backup_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with(BACKUP_PREFIX)))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs
}

/// Delete the `backup_dirs` of `dir`: the regular files directly inside
/// each, then the dir. Nothing else is deleted and no symlink is followed, so
/// a dir holding anything but regular files is left in place (emptied of
/// its regular files). Returns how many dirs were removed and the ones left.
pub(super) fn remove_backup_dirs(dir: &Path) -> Result<(usize, Vec<PathBuf>)> {
    let mut removed = 0;
    let mut left = Vec::new();
    for backup in backup_dirs(dir) {
        let mut other = false;
        for entry in fs::read_dir(&backup).with_context(|| format!("Failed to read {backup:?}"))? {
            let entry = entry?;
            // DirEntry::file_type doesn't follow symlinks
            if entry.file_type()?.is_file() {
                fs::remove_file(entry.path())
                    .with_context(|| format!("Failed to delete {:?}", entry.path()))?;
            } else {
                other = true;
            }
        }
        if other {
            left.push(backup);
        } else {
            fs::remove_dir(&backup).with_context(|| format!("Failed to delete {backup:?}"))?;
            removed += 1;
        }
    }
    Ok((removed, left))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{DB_FILE, IngestPolicy, ListOrder, Source, Storage, schema};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use tempfile::TempDir;

    struct Legacy<'a> {
        id: &'a str,
        timestamp: i64,
        pinned: bool,
        content: &'a [u8],
    }

    fn entry<'a>(id: &'a str, timestamp: i64, content: &'a str) -> Legacy<'a> {
        Legacy { id, timestamp, pinned: false, content: content.as_bytes() }
    }

    /// Write an old-format store: index.json (newest first, like the old
    /// code) plus one content file per entry.
    fn write_legacy(dir: &Path, max_entries: usize, entries: &[Legacy<'_>]) {
        let index_entries: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id, "timestamp": e.timestamp, "size": e.content.len(),
                    "preview": "p", "hash": "not-checked", "pinned": e.pinned,
                })
            })
            .collect();
        let index = serde_json::json!({ "max_entries": max_entries, "entries": index_entries });
        fs::write(dir.join("index.json"), index.to_string()).unwrap();
        for e in entries {
            fs::write(dir.join(format!("{}.txt", e.id)), e.content).unwrap();
        }
    }

    fn open(dir: &TempDir) -> Storage {
        Storage::open(dir.path().to_path_buf(), None, IngestPolicy::default()).unwrap()
    }

    fn contents(storage: &Storage) -> Vec<String> {
        storage
            .list(ListOrder::Recency, None)
            .unwrap()
            .iter()
            .map(|e| storage.load_content(e.id).unwrap())
            .collect()
    }

    fn root_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| e.file_type().unwrap().is_file())
            .map(|e| e.file_name().into_string().unwrap())
            .filter(|n| !n.starts_with(DB_FILE))
            .collect();
        names.sort();
        names
    }

    fn sorted_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        names
    }

    #[test]
    fn detects_index_or_content_files_only() {
        let dir = TempDir::new().unwrap();
        assert!(!detect(dir.path()));
        fs::write(dir.path().join("index.lock"), "").unwrap();
        fs::write(dir.path().join("notes.txt"), "x").unwrap();
        fs::write(dir.path().join("12_ab.txt"), "x").unwrap();
        assert!(!detect(dir.path()), "index.lock and other .txt files are not a legacy store");
        fs::write(dir.path().join("1700000000000_001.txt"), "x").unwrap();
        assert!(detect(dir.path()));

        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("index.json"), "{}").unwrap();
        assert!(detect(dir.path()));
    }

    #[test]
    fn imports_index_and_content() {
        let dir = TempDir::new().unwrap();
        write_legacy(
            dir.path(),
            100,
            &[entry("3000_001", 3000, "newest"), entry("2000", 2000, "middle"), entry("1000", 1000, "oldest")],
        );
        let storage = open(&dir);
        assert_eq!(contents(&storage), ["newest", "middle", "oldest"]);
        let entries = storage.list(ListOrder::Recency, None).unwrap();
        assert!(entries.iter().all(|e| e.source == Source::Import));
        assert!(entries[2].id < entries[0].id, "inserted oldest first");
        assert_eq!(root_files(dir.path()), ["index.lock"], "originals moved out of the root");

        let backups = storage.legacy_backup_dirs();
        assert_eq!(backups.len(), 1);
        assert_eq!(sorted_names(&backups[0]), ["1000.txt", "2000.txt", "3000_001.txt", "index.json"]);
        let imported: String = storage.with_conn(|c| {
            c.query_row("SELECT value FROM settings WHERE key = 'legacy_imported_count'", [], |r| r.get(0))
                .unwrap()
        });
        assert_eq!(imported, "3");
    }

    #[test]
    fn preserves_pin_recency_and_max_entries() {
        let dir = TempDir::new().unwrap();
        let mut pinned = entry("1000", 5000, "pinned old");
        pinned.pinned = true;
        write_legacy(dir.path(), 7, &[pinned, entry("2000_003", 4000, "recent")]);
        let storage = open(&dir);

        assert_eq!(storage.max_entries(), 7, "limit taken from index.json");
        assert!(storage.limit_is_saved());
        let entries = storage.list(ListOrder::PinnedFirst, None).unwrap();
        assert_eq!(entries[0].preview, "pinned old");
        assert!(entries[0].pinned);
        assert_eq!((entries[0].created_at, entries[0].last_used), (1000, 5000), "created_at from id");
        assert_eq!((entries[1].created_at, entries[1].last_used), (2000, 4000));
        assert!(!entries[1].pinned);
    }

    #[test]
    fn saved_limit_wins_over_index_json() {
        let dir = TempDir::new().unwrap();
        Storage::open(dir.path().to_path_buf(), Some(3), IngestPolicy::default()).unwrap();
        write_legacy(dir.path(), 500, &[entry("1000", 1000, "a")]);
        assert_eq!(open(&dir).max_entries(), 3);
    }

    #[test]
    fn import_prunes_to_max_entries_only() {
        let dir = TempDir::new().unwrap();
        let entries: Vec<String> = (0..5).map(|i| format!("entry {i}")).collect();
        let ids: Vec<String> = (0..5).map(|i| format!("{}", 1000 + i)).collect();
        let legacy: Vec<Legacy<'_>> =
            (0..5).map(|i| entry(&ids[i], 1000 + i as i64, &entries[i])).collect();
        write_legacy(dir.path(), 3, &legacy);
        let policy = IngestPolicy::new(regex::RegexSet::empty(), 0, 8, 8, None);
        let storage = Storage::open(dir.path().to_path_buf(), None, policy).unwrap();
        // Total-size cap (8 bytes, one entry) is not applied by the importing open
        assert_eq!(contents(&storage), ["entry 4", "entry 3", "entry 2"]);
    }

    #[test]
    fn import_skips_what_the_ingest_policy_rejects() {
        let dir = TempDir::new().unwrap();
        let key = format!("-----BEGIN RSA {} KEY-----\nMIIE\n", "PRIVATE");
        let big = "b".repeat(65);
        write_legacy(
            dir.path(),
            100,
            &[
                entry("5000", 5000, "kept entry"),
                entry("4000", 4000, ""),
                entry("3000", 3000, &key),
                entry("2000", 2000, &big),
                entry("1000", 1000, "password: hunter2"),
            ],
        );
        let config = crate::config::Config::parse(
            "[storage]\nmax_entry_bytes = 64\nmax_total_bytes = 1024\nignore_patterns = ['^password:']\n",
        )
        .unwrap();
        let policy = config.ingest_policy().unwrap();
        let mut conn = connection(&dir);
        let report = import_with(&mut conn, dir.path(), None, &policy, 9_000, &|_| Ok(())).unwrap();
        assert_eq!(report.kept, 1);
        assert_eq!(report.skipped, 4, "empty, built-in pattern, oversize, ignore pattern");
        let left: Vec<String> = conn
            .prepare("SELECT content FROM entries")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(left, ["kept entry"]);
        // Skipped files are only in the backup, which is now their one copy
        let backup = &backup_dirs(dir.path())[0];
        assert_eq!(
            sorted_names(backup),
            ["1000.txt", "2000.txt", "3000.txt", "4000.txt", "5000.txt", "index.json"]
        );
        assert_eq!(root_files(dir.path()), ["index.lock"]);
    }

    #[test]
    fn report_counts_kept_and_pruned_entries_after_dedupe() {
        let dir = TempDir::new().unwrap();
        write_legacy(
            dir.path(),
            2,
            &[
                entry("4000", 4000, "d"),
                entry("3000", 3000, "c"),
                entry("2500", 2500, "c"),
                entry("2000", 2000, "b"),
                entry("1000", 1000, "a"),
            ],
        );
        let mut conn = connection(&dir);
        let report =
            import_with(&mut conn, dir.path(), None, &IngestPolicy::default(), 9_000, &|_| Ok(())).unwrap();
        assert_eq!((report.kept, report.pruned, report.limit), (2, 2, 2), "{report:?}");
        assert_eq!(entry_count(&conn), 2);
        let count: String = conn
            .query_row("SELECT value FROM settings WHERE key = 'legacy_imported_count'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, "2", "the entries actually kept");
    }

    #[test]
    fn import_caps_pins_at_max_pinned_keeping_most_recent() {
        use crate::storage::MAX_PINNED;
        let dir = TempDir::new().unwrap();
        let n = MAX_PINNED + 5;
        let ids: Vec<String> = (0..n).map(|i| format!("{}", 1000 + i)).collect();
        let texts: Vec<String> = (0..n).map(|i| format!("pin {i}")).collect();
        let legacy: Vec<Legacy<'_>> = (0..n)
            .map(|i| Legacy { pinned: true, ..entry(&ids[i], 1000 + i as i64, &texts[i]) })
            .collect();
        write_legacy(dir.path(), 100, &legacy);
        let mut conn = connection(&dir);
        let report =
            import_with(&mut conn, dir.path(), None, &IngestPolicy::default(), 9_000, &|_| Ok(())).unwrap();
        assert_eq!(report.unpinned, 5);
        assert_eq!(report.kept, n, "unpinned entries stay in the history");
        let pinned: Vec<String> = conn
            .prepare("SELECT content FROM entries WHERE pinned = 1 ORDER BY last_used")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(pinned.len(), MAX_PINNED);
        assert_eq!(pinned[0], "pin 5", "the oldest five lost their pin");
    }

    #[test]
    fn includes_orphan_txt_files() {
        let dir = TempDir::new().unwrap();
        write_legacy(dir.path(), 100, &[entry("2000", 2000, "indexed")]);
        fs::write(dir.path().join("1500_007.txt"), "orphan").unwrap();
        fs::write(dir.path().join("1600.txt.tmp"), "half written").unwrap();
        let storage = open(&dir);
        assert_eq!(contents(&storage), ["indexed", "orphan"]);
        let orphan = storage.nth(2).unwrap().unwrap();
        assert_eq!((orphan.created_at, orphan.last_used), (1500, 1500));
        assert_eq!(
            sorted_names(&storage.legacy_backup_dirs()[0]),
            ["1500_007.txt", "1600.txt.tmp", "2000.txt", "index.json"],
            ".tmp leftovers are moved too"
        );
    }

    #[test]
    fn handles_corrupt_index_json() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("index.json"), "{\"max_entries\": 5, \"entr").unwrap();
        fs::write(dir.path().join("1000.txt"), "survivor").unwrap();
        let storage = open(&dir);
        assert_eq!(contents(&storage), ["survivor"]);
        assert_eq!(storage.max_entries(), crate::storage::DEFAULT_MAX_ENTRIES);
        assert_eq!(sorted_names(&storage.legacy_backup_dirs()[0]), ["1000.txt", "index.json"]);
    }

    #[test]
    fn ignores_index_ids_that_are_not_content_file_names() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("secret.txt"), "not ours").unwrap();
        write_legacy(dir.path(), 100, &[entry("1000", 1000, "fine")]);
        let index = r#"{"max_entries":100,"entries":[
            {"id":"../secret","timestamp":9,"size":1,"preview":"","hash":""},
            {"id":"secret","timestamp":9,"size":1,"preview":"","hash":""},
            {"id":"1000","timestamp":1000,"size":4,"preview":"","hash":""}]}"#;
        fs::write(dir.path().join("index.json"), index).unwrap();
        let storage = open(&dir);
        assert_eq!(contents(&storage), ["fine"]);
        assert!(dir.path().join("secret.txt").exists(), "not moved");
    }

    #[test]
    fn dedupes_by_recomputed_hash() {
        let dir = TempDir::new().unwrap();
        let mut pinned = entry("1000", 1000, "same text");
        pinned.pinned = true;
        write_legacy(dir.path(), 100, &[entry("3000", 3000, "same text"), pinned]);
        let storage = open(&dir);
        let entries = storage.list(ListOrder::Recency, None).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].pinned, "pin kept from either copy");
        assert_eq!(entries[0].last_used, 3000, "latest use kept");
        assert_eq!(entries[0].created_at, 1000, "first copy's created_at");
    }

    fn connection(dir: &TempDir) -> rusqlite::Connection {
        schema::open_connection(&dir.path().join(DB_FILE)).unwrap()
    }

    fn entry_count(conn: &rusqlite::Connection) -> i64 {
        conn.query_row("SELECT count(*) FROM entries", [], |r| r.get(0)).unwrap()
    }

    fn fail_at(stage: Stage) -> impl Fn(Stage) -> anyhow::Result<()> {
        move |at| if at == stage { anyhow::bail!("injected failure at {at:?}") } else { Ok(()) }
    }

    #[test]
    fn moves_files_to_backup_only_after_commit() {
        let dir = TempDir::new().unwrap();
        write_legacy(dir.path(), 100, &[entry("1000", 1000, "a"), entry("2000", 2000, "b")]);
        let mut conn = connection(&dir);
        let err = import_with(&mut conn, dir.path(), None, &IngestPolicy::default(), 5_000, &fail_at(Stage::BeforeCommit))
            .unwrap_err();
        assert!(err.to_string().contains("injected"), "{err:#}");
        assert_eq!(entry_count(&conn), 0, "rolled back");
        assert_eq!(root_files(dir.path()), ["1000.txt", "2000.txt", "index.json", "index.lock"]);
        assert!(backup_dirs(dir.path()).is_empty());
    }

    #[test]
    fn rerun_after_crash_before_move_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let mut pinned = entry("1000", 1000, "a");
        pinned.pinned = true;
        write_legacy(dir.path(), 100, &[pinned, entry("2000", 2000, "b")]);
        let mut conn = connection(&dir);
        import_with(&mut conn, dir.path(), None, &IngestPolicy::default(), 5_000, &fail_at(Stage::BeforeMove)).unwrap_err();
        assert_eq!(entry_count(&conn), 2, "committed");
        assert!(dir.path().join("index.json").exists(), "but nothing moved");

        let report = import_with(&mut conn, dir.path(), None, &IngestPolicy::default(), 6_000, &|_| Ok(())).unwrap();
        assert_eq!(report.kept, 2);
        assert_eq!(entry_count(&conn), 2, "no duplicates");
        let pinned: i64 = conn.query_row("SELECT sum(pinned) FROM entries", [], |r| r.get(0)).unwrap();
        assert_eq!(pinned, 1);
        assert_eq!(root_files(dir.path()), ["index.lock"]);
    }

    #[test]
    fn keeps_non_utf8_files_in_backup_without_importing() {
        let dir = TempDir::new().unwrap();
        write_legacy(
            dir.path(),
            100,
            &[Legacy { id: "1000", timestamp: 1000, pinned: false, content: b"\xff\xfe bad" }, entry("2000", 2000, "good")],
        );
        let storage = open(&dir);
        assert_eq!(contents(&storage), ["good"]);
        let backup = &storage.legacy_backup_dirs()[0];
        assert_eq!(fs::read(backup.join("1000.txt")).unwrap(), b"\xff\xfe bad", "kept as-is");
        assert!(!detect(dir.path()), "nothing left to re-detect on the next open");
    }

    #[test]
    fn backup_dir_is_0700() {
        let dir = TempDir::new().unwrap();
        write_legacy(dir.path(), 100, &[entry("1000", 1000, "a")]);
        fs::set_permissions(dir.path().join("1000.txt"), fs::Permissions::from_mode(0o644)).unwrap();
        let storage = open(&dir);
        let backup = &storage.legacy_backup_dirs()[0];
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(backup), 0o700);
        assert_eq!(mode(&backup.join("1000.txt")), 0o600);
        assert_eq!(mode(&backup.join("index.json")), 0o600);
        assert!(backup.file_name().unwrap().to_str().unwrap().starts_with("legacy-backup-"));
    }

    #[test]
    fn reimports_index_written_by_old_daemon() {
        let dir = TempDir::new().unwrap();
        write_legacy(dir.path(), 100, &[entry("1000", 1000, "first")]);
        drop(open(&dir));

        // An old daemon still running rewrites index.json with what it knows
        write_legacy(dir.path(), 100, &[entry("9000", 9000, "first"), entry("8000", 8000, "later")]);
        let storage = open(&dir);
        assert_eq!(contents(&storage), ["first", "later"]);
        assert_eq!(storage.nth(1).unwrap().unwrap().last_used, 9000, "merged by hash");
        assert_eq!(storage.legacy_backup_dirs().len(), 2);
        assert_eq!(root_files(dir.path()), ["index.lock"]);
    }

    #[test]
    fn remove_backups_deletes_regular_files_and_never_follows_symlinks() {
        let dir = TempDir::new().unwrap();
        write_legacy(dir.path(), 100, &[entry("1000", 1000, "a")]);
        let storage = open(&dir);
        let imported = storage.legacy_backup_dirs()[0].clone();

        // A backup dir holding a symlink to a file outside it
        let outside = TempDir::new().unwrap();
        let target = outside.path().join("precious.txt");
        fs::write(&target, "keep me").unwrap();
        let odd = dir.path().join("legacy-backup-9");
        fs::create_dir(&odd).unwrap();
        fs::write(odd.join("2000.txt"), "b").unwrap();
        std::os::unix::fs::symlink(&target, odd.join("link.txt")).unwrap();
        // A symlink named like a backup dir is not one
        std::os::unix::fs::symlink(outside.path(), dir.path().join("legacy-backup-8")).unwrap();

        let (removed, left) = storage.remove_legacy_backups().unwrap();
        assert_eq!(removed, 1);
        assert_eq!(left, std::slice::from_ref(&odd));
        assert!(!imported.exists());
        assert!(!odd.join("2000.txt").exists(), "regular files go even when the dir stays");
        assert!(fs::symlink_metadata(odd.join("link.txt")).is_ok(), "symlink left alone");
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep me");
        assert!(dir.path().join("legacy-backup-8").exists(), "symlinked dir untouched");
        assert!(outside.path().join("precious.txt").exists());
    }

    #[test]
    fn no_import_when_no_legacy_files() {
        let dir = TempDir::new().unwrap();
        let storage = open(&dir);
        assert!(storage.legacy_backup_dirs().is_empty());
        assert!(!dir.path().join("index.lock").exists(), "lock only taken for an import");
    }
}

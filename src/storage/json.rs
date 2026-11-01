use crate::perms;
use crate::util;
use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_PREVIEW_LEN: usize = 100;
// Configurable max entries constants
const DEFAULT_MAX_ENTRIES: usize = 100;
const ABSOLUTE_MAX_ENTRIES: usize = 10000; // Safety limit
const MAX_PINNED: usize = 25; // Prevents users from pinning everything

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipEntry {
    pub id: String,
    pub timestamp: i64,
    pub size: usize,
    pub preview: String,
    pub hash: String,
    /// Whether this entry is protected from automatic pruning
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipIndex {
    pub max_entries: usize,
    pub entries: Vec<ClipEntry>,
}

impl Default for ClipIndex {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_MAX_ENTRIES,
            entries: Vec::new(),
        }
    }
}

pub struct Storage {
    base_dir: PathBuf,
    max_entries: usize, // Cached limit for CLI/env override
}

impl Storage {
    /// Create storage with specified max entries
    pub fn new(base_dir: PathBuf, max_entries: usize) -> Result<Self> {
        perms::ensure_private_dir(&base_dir)
            .with_context(|| format!("Failed to create storage dir: {:?}", base_dir))?;

        // Clamp to valid range
        let max_entries = max_entries.clamp(1, ABSOLUTE_MAX_ENTRIES);

        let storage = Self { base_dir, max_entries };

        {
            // Hold the index lock so cleanup can't delete another process's
            // in-flight temp file and pruning can't race other writers
            let _lock = storage.lock_index()?;

            // Stores written by older versions were 0755/0644
            let report = perms::tighten_dir(&storage.base_dir)
                .with_context(|| format!("Failed to tighten permissions: {:?}", storage.base_dir))?;
            if report.files_changed > 0 {
                eprintln!(
                    "[storage] tightened permissions on {} files",
                    report.files_changed
                );
            }

            // Clean up any orphaned temp files from interrupted operations
            storage.cleanup_temp_files()?;

            // Sync to stored index (prunes if needs)
            storage.sync_max_entries()?;
        }

        Ok(storage)
    }

    /// Open storage, resolving the entry limit.
    ///
    /// `Some(n)` is an explicit user choice (CLI flag / env var): it is persisted
    /// to the index and prunes immediately if lower than the current history.
    /// `None` reuses the limit stored in the index (or the default for a fresh
    /// store), so unconfigured invocations never prune history behind the
    /// user's back.
    pub fn open(base_dir: PathBuf, max_entries: Option<usize>) -> Result<Self> {
        let limit = match max_entries {
            Some(n) => n,
            None => Self::stored_max_entries(&base_dir).unwrap_or(DEFAULT_MAX_ENTRIES),
        };
        Self::new(base_dir, limit)
    }

    /// Read the persisted limit from an existing, valid index
    fn stored_max_entries(base_dir: &Path) -> Option<usize> {
        let data = fs::read_to_string(base_dir.join("index.json")).ok()?;
        serde_json::from_str::<ClipIndex>(&data)
            .ok()
            .map(|index| index.max_entries)
    }

    /// Convenience constructor with default max_entries
    #[allow(dead_code)]
    pub fn with_defaults(base_dir: PathBuf) -> Result<Self> {
        Self::new(base_dir, DEFAULT_MAX_ENTRIES)
    }

    /// Sync max_entries to stored index and prune if necessary
    fn sync_max_entries(&self) -> Result<()> {
        // If index is corrupted or doesn't exist, skip sync (recovery will handle it)
        let mut index = match self.load_index() {
            Ok(idx) => idx,
            Err(_) => return Ok(()),
        };
        let mut changed = false;

        if index.max_entries != self.max_entries {
            index.max_entries = self.max_entries;
            changed = true;
        }

        // Prune UNPINNED entries if limit was reduced
        if self.prune_oldest_unpinned(&mut index) {
            changed = true;
        }

        if changed {
            self.save_index(&index)?;
        }

        Ok(())
    }

    /// Clean up orphaned temp files from interrupted operations
    fn cleanup_temp_files(&self) -> Result<()> {
        if let Ok(entries) = fs::read_dir(&self.base_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "tmp") {
                    eprintln!("[cleanup] Removing orphaned temp file: {:?}", path);
                    let _ = fs::remove_file(&path);
                }
            }
        }
        Ok(())
    }

    /// Remove oldest unpinned entries until within max_entries limit.
    /// Returns true if any entries were removed.
    fn prune_oldest_unpinned(&self, index: &mut ClipIndex) -> bool {
        let mut changed = false;
        while index.entries.iter().filter(|e| !e.pinned).count() > self.max_entries {
            if let Some(pos) = index.entries.iter().rposition(|e| !e.pinned) {
                let old = index.entries.remove(pos);
                let _ = fs::remove_file(self.content_path(&old.id));
                changed = true;
            } else {
                break; // All entries are pinned
            }
        }
        changed
    }

    /// Atomically write data to a file using write-then-rename pattern.
    ///
    /// This guarantees that file writes are atomic:
    /// 1. Write to temporary file (unique .tmp extension)
    /// 2. fsync() to ensure data is on disk
    /// 3. Atomic rename() to final path
    /// 4. fsync() parent directory for full durability
    ///
    /// If interrupted at any point, the original file remains intact.
    fn atomic_write(&self, path: &Path, data: &[u8]) -> Result<()> {
        // Generate unique temp file name using nanosecond timestamp and process ID
        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp_name = format!("{}.{}.tmp", std::process::id(), unique_id);
        let tmp_path = path.with_file_name(tmp_name);

        // Step 1: Write to a private (0600) temporary file; rename keeps the mode
        let mut file = perms::private_file_options()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .with_context(|| format!("Failed to create temp file: {:?}", tmp_path))?;

        file.write_all(data)
            .with_context(|| format!("Failed to write temp file: {:?}", tmp_path))?;

        // Step 2: Ensure data is flushed to disk
        file.sync_all()
            .with_context(|| format!("Failed to sync temp file: {:?}", tmp_path))?;

        // Step 3: Close file before rename (required on some platforms)
        drop(file);

        // Step 4: Atomic rename (POSIX guarantees atomicity)
        fs::rename(&tmp_path, path)
            .with_context(|| format!("Failed to rename {:?} to {:?}", tmp_path, path))?;

        // Step 5: Sync parent directory for full durability
        if let Some(parent) = path.parent()
            && let Ok(dir) = fs::File::open(parent)
        {
            let _ = dir.sync_all();
        }

        Ok(())
    }

    /// Take the cross-process index lock (blocking). Every read-modify-write
    /// of the index happens under it so concurrent daemon/picker/serve
    /// processes can't lose each other's updates. Released when dropped.
    fn lock_index(&self) -> Result<fs::File> {
        let path = self.base_dir.join("index.lock");
        let file = perms::private_file_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("Failed to open index lock: {:?}", path))?;
        file.lock_exclusive()
            .with_context(|| format!("Failed to lock index: {:?}", path))?;
        Ok(file)
    }

    pub fn base_dir(&self) -> &PathBuf {
        &self.base_dir
    }

    fn index_path(&self) -> PathBuf {
        self.base_dir.join("index.json")
    }

    fn content_path(&self, id: &str) -> PathBuf {
        self.base_dir.join(format!("{}.txt", id))
    }

    pub fn load_index(&self) -> Result<ClipIndex> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(ClipIndex::default());
        }
        let data = match fs::read_to_string(&path) {
            Ok(data) => data,
            Err(e) => {
                eprintln!("[storage] Warning: Cannot read index ({}), returning empty", e);
                return Ok(ClipIndex {
                    max_entries: self.max_entries,
                    entries: Vec::new(),
                });
            }
        };
        match serde_json::from_str(&data) {
            Ok(index) => Ok(index),
            Err(e) => {
                eprintln!("[storage] Warning: Index corrupted ({}), returning empty", e);
                eprintln!("[storage] Run 'clipstack recover' to rebuild from content files");
                Ok(ClipIndex {
                    max_entries: self.max_entries,
                    entries: Vec::new(),
                })
            }
        }
    }

    /// Write the index. Callers must hold `lock_index()`.
    fn save_index(&self, index: &ClipIndex) -> Result<()> {
        let path = self.index_path();
        let data = serde_json::to_string_pretty(index)?;
        self.atomic_write(&path, data.as_bytes())
    }

    pub fn save_entry(&self, content: &str) -> Result<ClipEntry> {
        let _lock = self.lock_index()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let (timestamp, id) = self.generate_id(now);

        let hash = util::compute_hash_string(content);

        // Check for duplicate - move existing entry to front instead of duplicating
        let mut index = self.load_index()?;
        if let Some(pos) = index.entries.iter().position(|e| e.hash == hash) {
            let mut existing = index.entries.remove(pos);
            existing.timestamp = timestamp; // Re-copy counts as most recent use
            index.entries.insert(0, existing.clone());
            self.save_index(&index)?;
            return Ok(existing);
        }

        // Create preview (first N chars, single line)
        let preview: String = content
            .chars()
            .take(MAX_PREVIEW_LEN)
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();

        let entry = ClipEntry {
            id: id.clone(),
            timestamp,
            size: content.len(),
            preview,
            hash,
            pinned: false,
        };

        // Save content to file (atomic write prevents corruption)
        let content_path = self.content_path(&id);
        self.atomic_write(&content_path, content.as_bytes())?;

        // Update index
        index.entries.insert(0, entry.clone());

        // Prune old unpinned entries only
        self.prune_oldest_unpinned(&mut index);

        self.save_index(&index)?;
        Ok(entry)
    }

    /// Build a unique entry ID "{millis}_{nnn}" from a single clock reading.
    ///
    /// The suffix starts at the sub-millisecond microseconds and is bumped
    /// past any ID whose content file already exists, so a same-millisecond
    /// save can never overwrite another entry's content.
    fn generate_id(&self, now: std::time::Duration) -> (i64, String) {
        let timestamp = now.as_millis() as i64;
        let start = now.subsec_micros() % 1000;
        for offset in 0..1000 {
            let id = format!("{}_{:03}", timestamp, (start + offset) % 1000);
            if !self.content_path(&id).exists() {
                return (timestamp, id);
            }
        }
        // All 1000 suffixes taken within one millisecond: fall back to nanos
        (timestamp, format!("{}_{}", timestamp, now.subsec_nanos()))
    }

    pub fn load_content(&self, id: &str) -> Result<String> {
        let path = self.content_path(id);
        fs::read_to_string(&path).with_context(|| format!("Failed to read content: {:?}", path))
    }

    pub fn delete_entry(&self, id: &str) -> Result<()> {
        let _lock = self.lock_index()?;
        let mut index = self.load_index()?;
        index.entries.retain(|e| e.id != id);
        self.save_index(&index)?;

        let path = self.content_path(id);
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Toggle pin status of an entry.
    /// Returns new pinned state, or error if at pin limit.
    pub fn toggle_pin(&self, id: &str) -> Result<bool> {
        let _lock = self.lock_index()?;
        let mut index = self.load_index()?;

        // Count pinned before mutable borrow to satisfy borrow checker
        let pinned_count = index.entries.iter().filter(|e| e.pinned).count();

        let entry = index.entries.iter_mut().find(|e| e.id == id);

        match entry {
            Some(entry) => {
                // Check limit only when pinning (not unpinning)
                if !entry.pinned && pinned_count >= MAX_PINNED {
                    anyhow::bail!(
                        "Maximum pinned entries ({}) reached. Unpin something first.",
                        MAX_PINNED
                    );
                }

                entry.pinned = !entry.pinned;
                let new_status = entry.pinned;
                self.save_index(&index)?;
                Ok(new_status)
            }
            None => anyhow::bail!("Entry not found: {}", id),
        }
    }

    /// Explicitly set pin status (used for undo restore)
    pub fn set_pinned(&self, id: &str, pinned: bool) -> Result<()> {
        let _lock = self.lock_index()?;
        let mut index = self.load_index()?;

        // Count pinned before mutable borrow to satisfy borrow checker
        let pinned_count = index.entries.iter().filter(|e| e.pinned).count();

        if let Some(entry) = index.entries.iter_mut().find(|e| e.id == id) {
            // Check limit if pinning
            if pinned && !entry.pinned && pinned_count >= MAX_PINNED {
                anyhow::bail!("Maximum pinned entries reached");
            }
            entry.pinned = pinned;
            self.save_index(&index)?;
        }
        Ok(())
    }

    /// Get count of pinned entries
    #[allow(dead_code)]
    pub fn pinned_count(&self) -> Result<usize> {
        let index = self.load_index()?;
        Ok(index.entries.iter().filter(|e| e.pinned).count())
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_storage() -> (Storage, TempDir) {
        let dir = TempDir::new().unwrap();
        let storage = Storage::with_defaults(dir.path().to_path_buf()).unwrap();
        (storage, dir)
    }

    #[test]
    fn test_save_and_load_entry() {
        let (storage, _dir) = test_storage();
        let content = "Hello, clipboard!";

        let entry = storage.save_entry(content).unwrap();
        assert_eq!(entry.size, content.len());
        assert_eq!(entry.preview, content);

        let loaded = storage.load_content(&entry.id).unwrap();
        assert_eq!(loaded, content);
    }

    #[test]
    fn test_large_content_preview_truncated() {
        let (storage, _dir) = test_storage();
        let content = "x".repeat(500_000); // 500KB

        let entry = storage.save_entry(&content).unwrap();
        assert_eq!(entry.size, 500_000);
        assert_eq!(entry.preview.len(), MAX_PREVIEW_LEN);

        let loaded = storage.load_content(&entry.id).unwrap();
        assert_eq!(loaded.len(), 500_000);
    }

    #[test]
    fn test_index_persistence() {
        let (storage, _dir) = test_storage();

        storage.save_entry("first").unwrap();
        storage.save_entry("second").unwrap();

        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.entries[0].preview, "second"); // Most recent first
        assert_eq!(index.entries[1].preview, "first");
    }

    #[test]
    fn test_duplicate_detection() {
        let (storage, _dir) = test_storage();
        let content = "duplicate content";

        storage.save_entry(content).unwrap();
        storage.save_entry(content).unwrap();

        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), 1); // Only one entry
    }

    #[test]
    fn test_duplicate_moves_to_front() {
        let (storage, _dir) = test_storage();

        // Save three entries
        storage.save_entry("first").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        storage.save_entry("second").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        storage.save_entry("third").unwrap();

        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), 3);
        assert_eq!(index.entries[0].preview, "third"); // Most recent first
        assert_eq!(index.entries[2].preview, "first"); // Oldest last

        // Re-save "first" - should move to front
        storage.save_entry("first").unwrap();

        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), 3); // Still 3 entries
        assert_eq!(index.entries[0].preview, "first"); // Now at front
        assert_eq!(index.entries[1].preview, "third");
        assert_eq!(index.entries[2].preview, "second");
    }

    #[test]
    fn test_generate_id_derives_timestamp_from_same_clock_reading() {
        let (storage, _dir) = test_storage();
        let now = std::time::Duration::new(1_736_789_123, 456_789_000); // .456789s
        let (timestamp, id) = storage.generate_id(now);
        assert_eq!(timestamp, 1_736_789_123_456);
        assert_eq!(id, "1736789123456_789");
    }

    #[test]
    fn test_generate_id_skips_ids_already_on_disk() {
        let (storage, _dir) = test_storage();
        let now = std::time::Duration::new(1_736_789_123, 456_789_000);
        fs::write(storage.content_path("1736789123456_789"), "existing").unwrap();
        fs::write(storage.content_path("1736789123456_790"), "existing").unwrap();

        let (_, id) = storage.generate_id(now);
        assert_eq!(id, "1736789123456_791", "must never overwrite another entry's file");
    }

    #[test]
    fn test_rapid_saves_have_unique_ids() {
        let (storage, _dir) = test_storage();
        let ids: std::collections::HashSet<String> = (0..50)
            .map(|i| storage.save_entry(&format!("rapid {}", i)).unwrap().id)
            .collect();
        assert_eq!(ids.len(), 50);
    }

    #[test]
    fn test_duplicate_refreshes_timestamp() {
        let (storage, _dir) = test_storage();

        let original = storage.save_entry("recopied").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let again = storage.save_entry("recopied").unwrap();

        assert_eq!(again.id, original.id, "dedup keeps the same entry/file");
        assert!(
            again.timestamp > original.timestamp,
            "re-copy should refresh the timestamp"
        );
        let index = storage.load_index().unwrap();
        assert_eq!(index.entries[0].timestamp, again.timestamp, "persisted");
    }

    #[test]
    fn test_unicode_content_handling() {
        let (storage, _dir) = test_storage();
        let content = "Hello 世界 🎉 émojis 日本語テスト";

        let entry = storage.save_entry(content).unwrap();

        // Verify content is saved and loaded correctly
        let loaded = storage.load_content(&entry.id).unwrap();
        assert_eq!(loaded, content);

        // Verify preview handles Unicode without panic
        assert!(!entry.preview.is_empty());
        assert!(entry.preview.len() <= MAX_PREVIEW_LEN * 4); // UTF-8 can use up to 4 bytes per char
    }

    #[test]
    fn test_long_unicode_content_preview_truncation() {
        let (storage, _dir) = test_storage();
        // Create content with 200 emoji characters (each is 4 bytes in UTF-8)
        let content = "🎉".repeat(200);

        let entry = storage.save_entry(&content).unwrap();

        // Preview should be truncated to MAX_PREVIEW_LEN characters, not bytes
        assert_eq!(entry.preview.chars().count(), MAX_PREVIEW_LEN);
        // But full content should be preserved
        let loaded = storage.load_content(&entry.id).unwrap();
        assert_eq!(loaded, content);
    }

    #[test]
    fn test_empty_and_whitespace_content() {
        let (storage, _dir) = test_storage();

        // Empty content should still be saved (edge case)
        let entry = storage.save_entry("").unwrap();
        assert_eq!(entry.size, 0);
        assert!(entry.preview.is_empty());

        let loaded = storage.load_content(&entry.id).unwrap();
        assert!(loaded.is_empty());

        // Whitespace-only content should be saved with sanitized preview
        let ws_content = "   \n\t\r   ";
        let ws_entry = storage.save_entry(ws_content).unwrap();
        assert_eq!(ws_entry.size, ws_content.len());
        // Control chars should be replaced with spaces in preview
        assert!(!ws_entry.preview.contains('\n'));
        assert!(!ws_entry.preview.contains('\t'));
    }

    #[test]
    fn test_delete_nonexistent_entry() {
        let (storage, _dir) = test_storage();

        // Deleting nonexistent entry should not error
        let result = storage.delete_entry("nonexistent-id");
        assert!(result.is_ok());
    }

    #[test]
    fn test_pruning_old_entries() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::with_defaults(dir.path().to_path_buf()).unwrap();

        // Save max_entries + 5 items
        for i in 0..(DEFAULT_MAX_ENTRIES + 5) {
            storage.save_entry(&format!("content {}", i)).unwrap();
        }

        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), DEFAULT_MAX_ENTRIES);

        // Oldest entries should be pruned
        assert!(index.entries[0].preview.contains(&(DEFAULT_MAX_ENTRIES + 4).to_string()));
    }

    #[test]
    fn test_delete_entry() {
        let (storage, _dir) = test_storage();

        let entry = storage.save_entry("to delete").unwrap();
        storage.delete_entry(&entry.id).unwrap();

        let index = storage.load_index().unwrap();
        assert!(index.entries.is_empty());
    }

    #[test]
    fn test_preview_sanitizes_control_chars() {
        let (storage, _dir) = test_storage();
        let content = "line1\nline2\ttab\rcarriage";

        let entry = storage.save_entry(content).unwrap();
        assert!(!entry.preview.contains('\n'));
        assert!(!entry.preview.contains('\t'));
        assert!(!entry.preview.contains('\r'));
    }

    #[test]
    fn test_performance_large_entries() {
        use std::time::Instant;

        let dir = TempDir::new().unwrap();
        let storage = Storage::with_defaults(dir.path().to_path_buf()).unwrap();

        // Generate 100 entries of 500KB
        let base_content = "x".repeat(500_000);

        let start = Instant::now();
        for i in 0..100 {
            let unique_content = format!("{:03}{}", i, &base_content[..base_content.len() - 3]);
            storage.save_entry(&unique_content).unwrap();
        }
        let gen_time = start.elapsed();
        println!("Generated 100 x 500KB entries in {:?}", gen_time);

        // Index load should be < 10ms
        let start = Instant::now();
        for _ in 0..100 {
            let _ = storage.load_index().unwrap();
        }
        let index_time = start.elapsed();
        let avg_index_time = index_time / 100;
        println!("Average index load: {:?}", avg_index_time);
        assert!(
            avg_index_time.as_millis() < 10,
            "Index load too slow: {:?}",
            avg_index_time
        );

        // Content load should be < 50ms for 500KB
        let index = storage.load_index().unwrap();
        let start = Instant::now();
        for _ in 0..10 {
            let _ = storage.load_content(&index.entries[0].id).unwrap();
        }
        let content_time = start.elapsed();
        let avg_content_time = content_time / 10;
        println!("Average 500KB content load: {:?}", avg_content_time);
        assert!(
            avg_content_time.as_millis() < 50,
            "Content load too slow: {:?}",
            avg_content_time
        );
    }

    #[test]
    fn test_atomic_write_basic() {
        let (storage, _dir) = test_storage();
        let test_file = storage.base_dir.join("test_atomic.txt");
        let test_data = b"Hello, atomic world!";

        // Write data atomically
        storage.atomic_write(&test_file, test_data).unwrap();

        // Verify file exists and contains correct data
        assert!(test_file.exists());
        let loaded = fs::read_to_string(&test_file).unwrap();
        assert_eq!(loaded, "Hello, atomic world!");

        // Verify no temp file left behind
        let tmp_file = test_file.with_extension("tmp");
        assert!(!tmp_file.exists(), "Temp file should be cleaned up");
    }

    #[test]
    fn test_atomic_write_overwrites_existing() {
        let (storage, _dir) = test_storage();
        let test_file = storage.base_dir.join("test_overwrite.txt");

        // Write initial data
        fs::write(&test_file, "initial data").unwrap();
        assert_eq!(fs::read_to_string(&test_file).unwrap(), "initial data");

        // Atomically overwrite
        storage.atomic_write(&test_file, b"new data").unwrap();
        assert_eq!(fs::read_to_string(&test_file).unwrap(), "new data");
    }

    #[test]
    fn test_atomic_write_large_data() {
        let (storage, _dir) = test_storage();
        let test_file = storage.base_dir.join("test_large_atomic.txt");
        let large_data = "x".repeat(1_000_000); // 1MB

        storage
            .atomic_write(&test_file, large_data.as_bytes())
            .unwrap();

        let loaded = fs::read_to_string(&test_file).unwrap();
        assert_eq!(loaded.len(), 1_000_000);
    }

    #[test]
    fn test_cleanup_temp_files() {
        let dir = TempDir::new().unwrap();
        let base_dir = dir.path().to_path_buf();
        fs::create_dir_all(&base_dir).unwrap();

        // Create some orphaned temp files
        fs::write(base_dir.join("file1.tmp"), "orphaned1").unwrap();
        fs::write(base_dir.join("file2.tmp"), "orphaned2").unwrap();
        fs::write(base_dir.join("normal.txt"), "keep this").unwrap();
        fs::write(base_dir.join("index.json.tmp"), "orphaned index").unwrap();

        // Create storage - cleanup should run automatically
        let storage = Storage::with_defaults(base_dir.clone()).unwrap();

        // Temp files should be removed
        assert!(!base_dir.join("file1.tmp").exists());
        assert!(!base_dir.join("file2.tmp").exists());
        assert!(!base_dir.join("index.json.tmp").exists());

        // Normal files should remain
        assert!(base_dir.join("normal.txt").exists());
        assert_eq!(
            fs::read_to_string(base_dir.join("normal.txt")).unwrap(),
            "keep this"
        );

        // Verify storage works normally after cleanup
        storage.save_entry("test content").unwrap();
        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), 1);
    }

    // Max entries configuration tests
    #[test]
    fn test_custom_max_entries() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 5).unwrap();

        // Fill beyond limit
        for i in 0..10 {
            storage.save_entry(&format!("entry {}", i)).unwrap();
        }

        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), 5);
        assert_eq!(index.max_entries, 5);
    }

    #[test]
    fn test_reducing_max_entries_prunes_immediately() {
        let dir = TempDir::new().unwrap();

        // Create with high limit
        let storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();
        for i in 0..50 {
            storage.save_entry(&format!("entry {}", i)).unwrap();
        }

        // Recreate with lower limit - should prune
        let storage = Storage::new(dir.path().to_path_buf(), 10).unwrap();
        let index = storage.load_index().unwrap();

        assert_eq!(index.entries.len(), 10);
    }

    #[test]
    fn test_concurrent_saves_dont_corrupt() {
        use std::sync::Arc;
        use std::thread;

        let dir = TempDir::new().unwrap();
        let storage = Arc::new(Storage::with_defaults(dir.path().to_path_buf()).unwrap());

        let mut handles = vec![];
        for i in 0..10 {
            let storage = Arc::clone(&storage);
            handles.push(thread::spawn(move || {
                storage.save_entry(&format!("thread {} content", i)).unwrap();
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Atomic writes prevent torn files; the index lock prevents lost updates
        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), 10, "no concurrent save may be lost");

        // Verify index is valid JSON (not corrupted/truncated)
        let json = serde_json::to_string(&index).unwrap();
        assert!(!json.is_empty());

        // All entries in index should have valid content files
        for entry in &index.entries {
            let content_path = dir.path().join(format!("{}.txt", entry.id));
            assert!(
                content_path.exists(),
                "Content file for entry {} should exist",
                entry.id
            );
            let content = fs::read_to_string(&content_path).unwrap();
            assert!(
                content.starts_with("thread "),
                "Content should be valid thread content"
            );
        }
    }

    #[test]
    fn test_concurrent_writers_lose_no_updates() {
        use std::thread;

        // Separate Storage handles model separate processes (daemon, picker,
        // serve) doing read-modify-write on the same index.
        let dir = TempDir::new().unwrap();
        let base = dir.path().to_path_buf();
        Storage::with_defaults(base.clone()).unwrap();

        let handles: Vec<_> = (0..8)
            .map(|t| {
                let base = base.clone();
                thread::spawn(move || {
                    let storage = Storage::open(base, None).unwrap();
                    for i in 0..10 {
                        storage.save_entry(&format!("writer {} item {}", t, i)).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let storage = Storage::open(base, None).unwrap();
        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), 80, "every concurrent save must survive");
    }

    // ==================== Pin functionality tests ====================

    #[test]
    fn test_toggle_pin() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::with_defaults(dir.path().to_path_buf()).unwrap();

        let entry = storage.save_entry("test content").unwrap();
        assert!(!entry.pinned);

        // Toggle on
        let pinned = storage.toggle_pin(&entry.id).unwrap();
        assert!(pinned);

        // Verify persisted
        let index = storage.load_index().unwrap();
        assert!(index.entries[0].pinned);

        // Toggle off
        let pinned = storage.toggle_pin(&entry.id).unwrap();
        assert!(!pinned);
    }

    #[test]
    fn test_toggle_pin_nonexistent() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::with_defaults(dir.path().to_path_buf()).unwrap();

        let result = storage.toggle_pin("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_toggle_pin_respects_max_pinned() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::with_defaults(dir.path().to_path_buf()).unwrap();

        // Create MAX_PINNED entries and pin them all
        let mut entry_ids = Vec::new();
        for i in 0..MAX_PINNED {
            let entry = storage.save_entry(&format!("content {}", i)).unwrap();
            entry_ids.push(entry.id.clone());
            storage.toggle_pin(&entry.id).unwrap();
        }

        // Create one more entry
        let extra_entry = storage.save_entry("extra").unwrap();

        // Trying to pin should fail
        let result = storage.toggle_pin(&extra_entry.id);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Maximum pinned"));

        // But unpinning an existing one should work
        let result = storage.toggle_pin(&entry_ids[0]);
        assert!(result.is_ok());
        assert!(!result.unwrap()); // Now unpinned

        // And now pinning the extra should work
        let result = storage.toggle_pin(&extra_entry.id);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_set_pinned() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::with_defaults(dir.path().to_path_buf()).unwrap();

        let entry = storage.save_entry("test content").unwrap();

        // Set to true
        storage.set_pinned(&entry.id, true).unwrap();
        let index = storage.load_index().unwrap();
        assert!(index.entries[0].pinned);

        // Set to false
        storage.set_pinned(&entry.id, false).unwrap();
        let index = storage.load_index().unwrap();
        assert!(!index.entries[0].pinned);

        // Setting nonexistent id is a no-op (no error)
        storage.set_pinned("nonexistent", true).unwrap();
    }

    #[test]
    fn test_pinned_count() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::with_defaults(dir.path().to_path_buf()).unwrap();

        assert_eq!(storage.pinned_count().unwrap(), 0);

        // Use sleeps to ensure unique timestamps for each entry
        let entry1 = storage.save_entry("one").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let entry2 = storage.save_entry("two").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        storage.save_entry("three").unwrap();

        storage.toggle_pin(&entry1.id).unwrap();
        assert_eq!(storage.pinned_count().unwrap(), 1);

        storage.toggle_pin(&entry2.id).unwrap();
        assert_eq!(storage.pinned_count().unwrap(), 2);

        storage.toggle_pin(&entry1.id).unwrap();
        assert_eq!(storage.pinned_count().unwrap(), 1);
    }

    #[test]
    fn test_pinned_survives_pruning() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 5).unwrap(); // Small limit

        // Create and pin an entry
        let pinned_entry = storage.save_entry("keep me").unwrap();
        storage.toggle_pin(&pinned_entry.id).unwrap();

        // Fill beyond limit with sleeps to ensure unique timestamps
        for i in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(2));
            storage.save_entry(&format!("filler {}", i)).unwrap();
        }

        // Verify pinned entry still exists
        let index = storage.load_index().unwrap();
        let found = index.entries.iter().find(|e| e.id == pinned_entry.id);
        assert!(found.is_some(), "Pinned entry should survive pruning");
        assert!(found.unwrap().pinned, "Should still be pinned");

        // Verify unpinned count is at limit
        let unpinned = index.entries.iter().filter(|e| !e.pinned).count();
        assert_eq!(unpinned, 5, "Unpinned should be capped at max_entries");
    }

    #[test]
    fn test_duplicate_preserves_pin_status() {
        let (storage, _dir) = test_storage();

        // Create and pin an entry
        let original = storage.save_entry("duplicate me").unwrap();
        storage.toggle_pin(&original.id).unwrap();

        // Add other entries
        std::thread::sleep(std::time::Duration::from_millis(2));
        storage.save_entry("other 1").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        storage.save_entry("other 2").unwrap();

        // Re-copy same content
        std::thread::sleep(std::time::Duration::from_millis(2));
        let dup = storage.save_entry("duplicate me").unwrap();

        // Should be same entry, moved to front, still pinned
        assert_eq!(dup.id, original.id, "Should return same entry ID");

        let index = storage.load_index().unwrap();
        assert_eq!(index.entries[0].id, original.id, "Should be moved to front");
        assert!(index.entries[0].pinned, "Pin status should be preserved");
    }

    #[test]
    fn test_backwards_compat_missing_pinned_field() {
        let dir = TempDir::new().unwrap();
        let index_path = dir.path().join("index.json");

        // Write old-format index (no pinned field)
        std::fs::write(
            &index_path,
            r#"{
            "max_entries": 100,
            "entries": [{
                "id": "12345",
                "timestamp": 12345,
                "size": 4,
                "preview": "test",
                "hash": "sha256:abc"
            }]
        }"#,
        )
        .unwrap();

        std::fs::write(dir.path().join("12345.txt"), "test").unwrap();

        let storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();
        let index = storage.load_index().unwrap();

        assert_eq!(index.entries.len(), 1, "Should load old format");
        assert!(!index.entries[0].pinned, "Should default to false");
    }

    // ======== Permission Tests ========

    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn test_new_storage_dir_is_0700() {
        let dir = TempDir::new().unwrap();
        let base = dir.path().join("nested").join("clipd");
        let _storage = Storage::with_defaults(base.clone()).unwrap();
        assert_eq!(mode_of(&base), 0o700);
    }

    #[test]
    fn test_saved_content_and_index_are_0600() {
        let (storage, _dir) = test_storage();
        let entry = storage.save_entry("secret token").unwrap();
        assert_eq!(mode_of(&storage.content_path(&entry.id)), 0o600);
        assert_eq!(mode_of(&storage.index_path()), 0o600);
        assert_eq!(mode_of(&storage.base_dir().join("index.lock")), 0o600);
    }

    #[test]
    fn test_open_tightens_existing_store() {
        use std::os::unix::fs::PermissionsExt;
        let chmod = |p: &Path, m: u32| fs::set_permissions(p, fs::Permissions::from_mode(m)).unwrap();

        let dir = TempDir::new().unwrap();
        let base = dir.path().join("clipd");
        {
            let storage = Storage::with_defaults(base.clone()).unwrap();
            storage.save_entry("old world-readable entry").unwrap();
        }
        // Simulate a store written by an older version
        chmod(&base, 0o755);
        for entry in fs::read_dir(&base).unwrap() {
            chmod(&entry.unwrap().path(), 0o644);
        }

        let storage = Storage::open(base.clone(), None).unwrap();
        assert_eq!(mode_of(&base), 0o700);
        for entry in fs::read_dir(&base).unwrap() {
            let path = entry.unwrap().path();
            assert_eq!(mode_of(&path), 0o600, "{:?}", path);
        }
        assert_eq!(storage.load_index().unwrap().entries.len(), 1);
    }
}

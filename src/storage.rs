use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
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
        fs::create_dir_all(&base_dir)
            .with_context(|| format!("Failed to create storage dir: {:?}", base_dir))?;

        // Clamp to valid range
        let max_entries = max_entries.clamp(1, ABSOLUTE_MAX_ENTRIES);

        let storage = Self { base_dir, max_entries };

        // Clean up any orphaned temp files from interrupted operations
        storage.cleanup_temp_files()?;

        // Sync to stored index (prunes if needs)
        storage.sync_max_entries()?;

        Ok(storage)
    }

    /// Convenience constructor with default max_entries
    #[allow(dead_code)]
    pub fn with_defaults(base_dir: PathBuf) -> Result<Self> {
        Self::new(base_dir, DEFAULT_MAX_ENTRIES)
    }

    /// Get the configured max entries
    pub fn max_entries(&self) -> usize {
        self.max_entries
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
        // Only count unpinned entries against the limit
        while index.entries.iter().filter(|e| !e.pinned).count() > self.max_entries {
            // Find oldest (last) unpinned entry
            if let Some(pos) = index.entries.iter().rposition(|e| !e.pinned) {
                let old = index.entries.remove(pos);
                let old_path = self.content_path(&old.id);
                let _ = fs::remove_file(old_path);
                changed = true;
            } else {
                break; // All entries are pinned
            }
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

    /// Prune old UNPINNED entries to stay within max_entries limit.
    /// Pinned entries are never pruned by this method.
    fn prune_unpinned_entries(&self, index: &mut ClipIndex) -> Result<()> {
        // Only count unpinned entries against the limit
        while index.entries.iter().filter(|e| !e.pinned).count() > self.max_entries {
            // Find oldest (last) unpinned entry
            if let Some(pos) = index.entries.iter().rposition(|e| !e.pinned) {
                let old = index.entries.remove(pos);
                let old_path = self.content_path(&old.id);
                let _ = fs::remove_file(old_path);
            } else {
                break; // All entries are pinned
            }
        }
        Ok(())
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
        // Use unique temp file name to avoid race conditions when multiple threads
        // write to the same target path. Format: originalname.UNIQUE.tmp
        // This ensures .tmp extension is preserved for cleanup detection.
        let file_stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file");
        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp_name = format!("{}.{:?}_{}.tmp", file_stem, std::thread::current().id(), unique_id);
        let tmp_path = path.with_file_name(tmp_name);

        // Step 1: Write to temporary file
        let mut file = fs::File::create(&tmp_path)
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

    pub fn base_dir(&self) -> &PathBuf {
        &self.base_dir
    }

    pub fn default_dir() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("clipd")
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

    pub fn save_index(&self, index: &ClipIndex) -> Result<()> {
        let path = self.index_path();
        let data = serde_json::to_string_pretty(index)?;
        self.atomic_write(&path, data.as_bytes())
    }

    pub fn save_entry(&self, content: &str) -> Result<ClipEntry> {
        let timestamp = chrono::Utc::now().timestamp_millis();
        let id = timestamp.to_string();

        // Compute hash
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let hash = format!("sha256:{:x}", hasher.finalize());

        // Check for duplicate - move existing entry to front instead of duplicating
        let mut index = self.load_index()?;
        if let Some(pos) = index.entries.iter().position(|e| e.hash == hash) {
            let existing = index.entries.remove(pos);
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

        // Prune old UNPINNED entries only
        self.prune_unpinned_entries(&mut index)?;

        self.save_index(&index)?;
        Ok(entry)
    }

    pub fn load_content(&self, id: &str) -> Result<String> {
        let path = self.content_path(id);
        fs::read_to_string(&path).with_context(|| format!("Failed to read content: {:?}", path))
    }

    pub fn delete_entry(&self, id: &str) -> Result<()> {
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

    pub fn clear(&self) -> Result<()> {
        let index = self.load_index()?;
        for entry in &index.entries {
            let path = self.content_path(&entry.id);
            let _ = fs::remove_file(path);
        }
        self.save_index(&ClipIndex {
            max_entries: self.max_entries,
            entries: Vec::new(),
        })
    }

    /// Attempt to recover from corrupted storage.
    /// Rebuilds index from existing content files.
    pub fn attempt_recovery(&self) -> Result<usize> {
        eprintln!("[recovery] Starting storage recovery...");

        let index_path = self.index_path();
        let mut recovered_entries: Vec<ClipEntry> = Vec::new();

        // Try to load existing index entries first
        if index_path.exists() {
            match fs::read_to_string(&index_path) {
                Ok(data) => match serde_json::from_str::<ClipIndex>(&data) {
                    Ok(index) => {
                        eprintln!(
                            "[recovery] Loaded {} entries from existing index",
                            index.entries.len()
                        );
                        recovered_entries = index.entries;
                    }
                    Err(e) => {
                        eprintln!("[recovery] Index corrupted ({}), scanning files...", e);
                    }
                },
                Err(e) => {
                    eprintln!("[recovery] Cannot read index ({}), scanning files...", e);
                }
            }
        }

        // Collect IDs of entries we already have
        let known_ids: HashSet<_> =
            recovered_entries.iter().map(|e| e.id.clone()).collect();

        // Scan for orphaned content files
        let mut orphan_count = 0;
        for entry in fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "txt") {
                let id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();

                if known_ids.contains(&id) {
                    continue;
                }

                if let Ok(content) = fs::read_to_string(&path) {
                    let timestamp: i64 = id.parse().unwrap_or(0);

                    let mut hasher = Sha256::new();
                    hasher.update(content.as_bytes());
                    let hash = format!("sha256:{:x}", hasher.finalize());

                    let preview: String = content
                        .chars()
                        .take(MAX_PREVIEW_LEN)
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .collect();

                    recovered_entries.push(ClipEntry {
                        id,
                        timestamp,
                        size: content.len(),
                        preview,
                        hash,
                        pinned: false,
                    });
                    orphan_count += 1;
                }
            }
        }

        eprintln!("[recovery] Found {} orphaned content files", orphan_count);

        // Sort by timestamp descending, then by pinned (true first) to prefer pinned during dedup
        recovered_entries.sort_by(|a, b| {
            b.timestamp
                .cmp(&a.timestamp)
                .then_with(|| b.pinned.cmp(&a.pinned))
        });

        // Deduplicate by hash, preferring pinned entries
        // Use a map to track which entries we've seen, and prefer pinned ones
        let mut hash_to_entry: std::collections::HashMap<String, ClipEntry> =
            std::collections::HashMap::new();
        for entry in recovered_entries {
            match hash_to_entry.get(&entry.hash) {
                Some(existing) if !existing.pinned && entry.pinned => {
                    // Replace unpinned with pinned
                    hash_to_entry.insert(entry.hash.clone(), entry);
                }
                None => {
                    // First entry with this hash
                    hash_to_entry.insert(entry.hash.clone(), entry);
                }
                _ => {
                    // Already have a pinned entry or same pin state, keep existing
                }
            }
        }

        // Collect back into vec and sort by timestamp descending
        let mut recovered_entries: Vec<ClipEntry> = hash_to_entry.into_values().collect();
        recovered_entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        let total = recovered_entries.len();
        eprintln!("[recovery] Total entries after dedup: {}", total);

        // Save recovered index
        let index = ClipIndex {
            max_entries: self.max_entries,
            entries: recovered_entries,
        };
        self.save_index(&index)?;

        eprintln!("[recovery] Recovery complete");
        Ok(total)
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
    fn test_clear() {
        let (storage, _dir) = test_storage();

        storage.save_entry("one").unwrap();
        storage.save_entry("two").unwrap();
        storage.clear().unwrap();

        let index = storage.load_index().unwrap();
        assert!(index.entries.is_empty());
    }

    #[test]
    fn test_clear_preserves_custom_max_entries() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 42).unwrap();

        storage.save_entry("test").unwrap();
        storage.clear().unwrap();

        // Verify max_entries is preserved after clear
        let index = storage.load_index().unwrap();
        assert!(index.entries.is_empty());
        assert_eq!(index.max_entries, 42, "clear() should preserve configured max_entries");
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
    fn test_max_entries_clamps_low() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 0).unwrap();
        assert_eq!(storage.max_entries(), 1);
    }

    #[test]
    fn test_max_entries_clamps_high() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 999999).unwrap();
        assert_eq!(storage.max_entries(), 10000);
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
    fn test_max_entries_getter() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 42).unwrap();
        assert_eq!(storage.max_entries(), 42);
    }

    #[test]
    fn test_with_defaults_uses_100() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::with_defaults(dir.path().to_path_buf()).unwrap();
        assert_eq!(storage.max_entries(), 100);
    }

    #[test]
    fn test_recovery_from_orphaned_files() {
        let dir = TempDir::new().unwrap();
        let base_dir = dir.path().to_path_buf();
        fs::create_dir_all(&base_dir).unwrap();

        // Create some orphaned content files (without index entries)
        let timestamp1 = 1000i64;
        let timestamp2 = 2000i64;
        fs::write(base_dir.join(format!("{}.txt", timestamp1)), "orphan content 1").unwrap();
        fs::write(base_dir.join(format!("{}.txt", timestamp2)), "orphan content 2").unwrap();

        // Create an empty index
        let empty_index = ClipIndex::default();
        fs::write(
            base_dir.join("index.json"),
            serde_json::to_string(&empty_index).unwrap(),
        )
        .unwrap();

        // Create storage and run recovery
        let storage = Storage::with_defaults(base_dir).unwrap();
        let recovered = storage.attempt_recovery().unwrap();

        // Should have recovered both orphaned files
        assert_eq!(recovered, 2);

        // Verify index now has entries
        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), 2);

        // Entries should be sorted by timestamp descending (newest first)
        assert_eq!(index.entries[0].timestamp, timestamp2);
        assert_eq!(index.entries[1].timestamp, timestamp1);
    }

    #[test]
    fn test_recovery_with_corrupted_index() {
        let dir = TempDir::new().unwrap();
        let base_dir = dir.path().to_path_buf();

        // First create valid storage
        let storage = Storage::with_defaults(base_dir.clone()).unwrap();

        // Save an entry normally
        storage.save_entry("saved content").unwrap();
        let index = storage.load_index().unwrap();
        let entry_id = index.entries[0].id.clone();

        // Now corrupt the index (simulating crash/corruption)
        fs::write(base_dir.join("index.json"), "not valid json {{{").unwrap();

        // Verify load_index returns empty (graceful degradation on corruption)
        let corrupted_index = storage.load_index().unwrap();
        assert!(
            corrupted_index.entries.is_empty(),
            "Corrupted index should return empty"
        );

        // Run recovery
        let recovered = storage.attempt_recovery().unwrap();

        // Should have recovered the content file
        assert_eq!(recovered, 1);

        // Verify index is valid now
        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].id, entry_id);
    }

    #[test]
    fn test_recovery_deduplicates_by_hash() {
        let dir = TempDir::new().unwrap();
        let base_dir = dir.path().to_path_buf();
        fs::create_dir_all(&base_dir).unwrap();

        // Create content files with same content (same hash)
        fs::write(base_dir.join("1000.txt"), "duplicate content").unwrap();
        fs::write(base_dir.join("2000.txt"), "duplicate content").unwrap();

        // Create empty index
        let empty_index = ClipIndex::default();
        fs::write(
            base_dir.join("index.json"),
            serde_json::to_string(&empty_index).unwrap(),
        )
        .unwrap();

        // Create storage and run recovery
        let storage = Storage::with_defaults(base_dir).unwrap();
        let recovered = storage.attempt_recovery().unwrap();

        // Should keep only one (most recent = 2000)
        assert_eq!(recovered, 1);
        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].timestamp, 2000);
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
                // Add small sleep to avoid timestamp collisions
                thread::sleep(std::time::Duration::from_millis(i * 5));
                let _ = storage.save_entry(&format!("thread {} content", i));
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Atomic writes prevent corruption, but race conditions may cause
        // some entries to be overwritten. The key is that the index is valid.
        let index = storage.load_index().unwrap();

        // Should have at least some entries (not zero from total corruption)
        assert!(
            !index.entries.is_empty(),
            "Index should have entries, not be empty from corruption"
        );

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
}

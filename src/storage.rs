use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

const MAX_PREVIEW_LEN: usize = 100;
const MAX_ENTRIES: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipEntry {
    pub id: String,
    pub timestamp: i64,
    pub size: usize,
    pub preview: String,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipIndex {
    pub max_entries: usize,
    pub entries: Vec<ClipEntry>,
}

impl Default for ClipIndex {
    fn default() -> Self {
        Self {
            max_entries: MAX_ENTRIES,
            entries: Vec::new(),
        }
    }
}

pub struct Storage {
    base_dir: PathBuf,
}

impl Storage {
    pub fn new(base_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&base_dir)
            .with_context(|| format!("Failed to create storage dir: {:?}", base_dir))?;
        Ok(Self { base_dir })
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
        let data = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read index: {:?}", path))?;
        serde_json::from_str(&data).with_context(|| "Failed to parse index")
    }

    pub fn save_index(&self, index: &ClipIndex) -> Result<()> {
        let path = self.index_path();
        let data = serde_json::to_string_pretty(index)?;
        fs::write(&path, data).with_context(|| format!("Failed to write index: {:?}", path))
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
        };

        // Save content to file
        let content_path = self.content_path(&id);
        fs::write(&content_path, content)
            .with_context(|| format!("Failed to write content: {:?}", content_path))?;

        // Update index
        index.entries.insert(0, entry.clone());

        // Prune old entries
        while index.entries.len() > index.max_entries {
            if let Some(old) = index.entries.pop() {
                let old_path = self.content_path(&old.id);
                let _ = fs::remove_file(old_path);
            }
        }

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

    pub fn clear(&self) -> Result<()> {
        let index = self.load_index()?;
        for entry in &index.entries {
            let path = self.content_path(&entry.id);
            let _ = fs::remove_file(path);
        }
        self.save_index(&ClipIndex::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_storage() -> (Storage, TempDir) {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf()).unwrap();
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
        let storage = Storage::new(dir.path().to_path_buf()).unwrap();

        // Save max_entries + 5 items
        for i in 0..(MAX_ENTRIES + 5) {
            storage.save_entry(&format!("content {}", i)).unwrap();
        }

        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), MAX_ENTRIES);

        // Oldest entries should be pruned
        assert!(index.entries[0].preview.contains(&(MAX_ENTRIES + 4).to_string()));
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
        let storage = Storage::new(dir.path().to_path_buf()).unwrap();

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
}

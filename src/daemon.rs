use crate::clipboard::Clipboard;
use crate::storage::Storage;
use anyhow::{Context, Result};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct Daemon {
    storage: Storage,
    running: Arc<AtomicBool>,
    poll_interval: Duration,
    _lock_file: File, // Keep lock file open to maintain lock
}

impl Daemon {
    /// Get the default path to the daemon lock file
    pub fn lock_file_path() -> PathBuf {
        dirs::runtime_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("clipstack.lock")
    }

    /// Check if daemon is currently running by testing the lock file
    pub fn is_running() -> bool {
        let lock_path = Self::lock_file_path();
        if let Ok(file) = File::open(&lock_path) {
            // Try to acquire exclusive lock - if fails, daemon is running
            file.try_lock_exclusive().is_err()
        } else {
            false
        }
    }

    pub fn new(storage_dir: Option<PathBuf>, max_entries: usize) -> Result<Self> {
        Self::new_with_lock(storage_dir, max_entries, false)
    }

    /// Create daemon with option to use local lock file (for tests)
    pub fn new_with_lock(
        storage_dir: Option<PathBuf>,
        max_entries: usize,
        use_local_lock: bool,
    ) -> Result<Self> {
        let base_dir = storage_dir.unwrap_or_else(Storage::default_dir);
        let storage = Storage::new(base_dir.clone(), max_entries)?;

        // Use storage-local lock file only when explicitly requested (for tests),
        // otherwise use global lock file path
        let lock_path = if use_local_lock {
            base_dir.join("clipstack.lock")
        } else {
            Self::lock_file_path()
        };

        // Acquire exclusive lock - fails if another daemon is running
        let lock_file = File::create(&lock_path)
            .with_context(|| format!("Failed to create lock file: {:?}", lock_path))?;
        lock_file
            .try_lock_exclusive()
            .context("Daemon already running (lock file is held)")?;

        Ok(Self {
            storage,
            running: Arc::new(AtomicBool::new(false)),
            poll_interval: Duration::from_millis(250),
            _lock_file: lock_file,
        })
    }

    #[allow(dead_code)]
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Run the daemon, monitoring clipboard and saving changes
    pub fn run(&self) -> Result<()> {
        self.running.store(true, Ordering::SeqCst);

        let mut last_clipboard_hash: Option<Vec<u8>> = None;
        let mut last_primary_hash: Option<Vec<u8>> = None;

        eprintln!("clipstack daemon started, monitoring clipboard + primary selection...");

        while self.running.load(Ordering::SeqCst) {
            // Check regular clipboard
            self.check_and_save(Clipboard::paste(), &mut last_clipboard_hash, "clipboard");

            // Check PRIMARY selection (mouse selection, used by terminals)
            self.check_and_save(Clipboard::paste_primary(), &mut last_primary_hash, "primary");

            std::thread::sleep(self.poll_interval);
        }

        eprintln!("clipstack daemon stopped");
        Ok(())
    }

    fn check_and_save(
        &self,
        result: Result<String>,
        last_hash: &mut Option<Vec<u8>>,
        source: &str,
    ) {
        match result {
            Ok(content) if !content.is_empty() => {
                let mut hasher = Sha256::new();
                hasher.update(content.as_bytes());
                let hash = hasher.finalize().to_vec();

                if last_hash.as_ref() != Some(&hash) {
                    *last_hash = Some(hash);

                    match self.storage.save_entry(&content) {
                        Ok(entry) => {
                            // Use chars().take() for safe Unicode truncation
                            let preview: String = entry.preview.chars().take(40).collect();
                            eprintln!(
                                "[{}] Saved: {} bytes, preview: {}...",
                                source,
                                entry.size,
                                preview
                            );
                        }
                        Err(e) => {
                            eprintln!("[{}] Error saving entry: {}", source, e);
                        }
                    }
                }
            }
            Ok(_) => {} // Empty, ignore
            Err(_) => {} // Silently ignore errors (selection might be empty)
        }
    }

    /// Stop the daemon
    #[allow(dead_code)]
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Get a handle to stop the daemon from another thread
    pub fn stop_handle(&self) -> Arc<AtomicBool> {
        self.running.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_daemon_creation() {
        let dir = TempDir::new().unwrap();
        // Use local lock file for test isolation
        let daemon = Daemon::new_with_lock(Some(dir.path().to_path_buf()), 100, true).unwrap();
        assert!(!daemon.running.load(Ordering::SeqCst));
    }

    #[test]
    fn test_daemon_stop_handle() {
        let dir = TempDir::new().unwrap();
        // Use local lock file for test isolation
        let daemon = Daemon::new_with_lock(Some(dir.path().to_path_buf()), 100, true).unwrap();

        let handle = daemon.stop_handle();
        daemon.running.store(true, Ordering::SeqCst);
        assert!(handle.load(Ordering::SeqCst));

        daemon.stop();
        assert!(!handle.load(Ordering::SeqCst));
    }
}

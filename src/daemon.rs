use crate::clipboard::Clipboard;
use crate::perms;
use crate::storage::Storage;
use crate::util;
use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
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

    /// `max_entries`: explicit limit, or `None` to reuse the stored limit
    /// Read the PID recorded in a lock file
    pub fn read_pid(lock_path: &Path) -> Option<u32> {
        std::fs::read_to_string(lock_path).ok()?.trim().parse().ok()
    }

    /// PID of the running daemon, if one holds the global lock
    pub fn running_pid() -> Option<u32> {
        if Self::is_running() {
            Self::read_pid(&Self::lock_file_path())
        } else {
            None
        }
    }

    pub fn new(storage_dir: Option<PathBuf>, max_entries: Option<usize>) -> Result<Self> {
        Self::new_with_lock(storage_dir, max_entries, false)
    }

    /// Create daemon with option to use local lock file (for tests)
    pub fn new_with_lock(
        storage_dir: Option<PathBuf>,
        max_entries: Option<usize>,
        use_local_lock: bool,
    ) -> Result<Self> {
        let base_dir = storage_dir.unwrap_or_else(Storage::default_dir);
        let storage = Storage::open(base_dir.clone(), max_entries)?;

        // Use storage-local lock file only when explicitly requested (for tests),
        // otherwise use global lock file path
        let lock_path = if use_local_lock {
            base_dir.join("clipstack.lock")
        } else {
            Self::lock_file_path()
        };

        // Acquire exclusive lock - fails if another daemon is running.
        // Open without truncating so a failed attempt can't wipe the running
        // daemon's PID. Created 0600 (an existing file keeps its mode).
        let mut lock_file = perms::private_file_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("Failed to create lock file: {:?}", lock_path))?;
        lock_file
            .try_lock_exclusive()
            .context("Daemon already running (lock file is held)")?;

        // Record our PID so `clipstack kill` can signal us precisely
        lock_file.set_len(0)?;
        write!(lock_file, "{}", std::process::id())?;
        lock_file.sync_all()?;

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

        eprintln!("clipstack daemon started, monitoring clipboard...");

        while self.running.load(Ordering::SeqCst) {
            // Check regular clipboard (Ctrl+C copies only)
            self.check_and_save(Clipboard::paste(), &mut last_clipboard_hash, "clipboard");

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
                let hash = util::compute_hash(&content);

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
        let daemon = Daemon::new_with_lock(Some(dir.path().to_path_buf()), Some(100), true).unwrap();
        assert!(!daemon.running.load(Ordering::SeqCst));
    }

    #[test]
    fn test_lock_file_records_daemon_pid() {
        let dir = TempDir::new().unwrap();
        let _daemon = Daemon::new_with_lock(Some(dir.path().to_path_buf()), Some(100), true).unwrap();

        let pid = Daemon::read_pid(&dir.path().join("clipstack.lock"));
        assert_eq!(pid, Some(std::process::id()));
    }

    #[test]
    fn test_second_daemon_fails_without_clobbering_pid() {
        let dir = TempDir::new().unwrap();
        let lock = dir.path().join("clipstack.lock");
        let _first = Daemon::new_with_lock(Some(dir.path().to_path_buf()), Some(100), true).unwrap();

        let second = Daemon::new_with_lock(Some(dir.path().to_path_buf()), Some(100), true);
        assert!(second.is_err(), "second daemon must not acquire the lock");
        assert_eq!(Daemon::read_pid(&lock), Some(std::process::id()));
    }

    #[test]
    fn test_read_pid_handles_missing_and_garbage() {
        let dir = TempDir::new().unwrap();
        let lock = dir.path().join("clipstack.lock");
        assert_eq!(Daemon::read_pid(&lock), None);
        std::fs::write(&lock, "not a pid").unwrap();
        assert_eq!(Daemon::read_pid(&lock), None);
    }

    #[test]
    fn test_daemon_stop_handle() {
        let dir = TempDir::new().unwrap();
        // Use local lock file for test isolation
        let daemon = Daemon::new_with_lock(Some(dir.path().to_path_buf()), Some(100), true).unwrap();

        let handle = daemon.stop_handle();
        daemon.running.store(true, Ordering::SeqCst);
        assert!(handle.load(Ordering::SeqCst));

        daemon.stop();
        assert!(!handle.load(Ordering::SeqCst));
    }

    #[test]
    fn test_lock_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let _daemon = Daemon::new_with_lock(Some(dir.path().to_path_buf()), Some(100), true).unwrap();

        let meta = std::fs::metadata(dir.path().join("clipstack.lock")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
}

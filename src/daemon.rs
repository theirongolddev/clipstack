use crate::clipboard::Clipboard;
use crate::perms;
use crate::storage::{IngestOutcome, Source, Storage};
use crate::util;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How often a running daemon applies retention and the size caps, so
/// entries age out without a new copy or command.
const EXPIRE_INTERVAL: Duration = Duration::from_secs(60);

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
        lock_is_held(&Self::lock_file_path())
    }

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

    /// A daemon saving into `storage`, holding the global lock file.
    pub fn new(storage: Storage) -> Result<Self> {
        Self::with_lock_path(storage, &Self::lock_file_path())
    }

    /// A daemon holding the lock at `lock_path` (tests use a temp path).
    pub fn with_lock_path(storage: Storage, lock_path: &Path) -> Result<Self> {
        // Acquire exclusive lock - fails if another daemon is running.
        let mut lock_file = open_lock_file(lock_path)?;
        lock_file
            .try_lock()
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
        let mut last_expire = Instant::now();

        eprintln!("clipstack daemon started, monitoring clipboard...");

        while self.running.load(Ordering::SeqCst) {
            // Check regular clipboard (Ctrl+C copies only)
            self.check_and_save(Clipboard::paste(), &mut last_clipboard_hash, "clipboard");

            if last_expire.elapsed() >= EXPIRE_INTERVAL {
                last_expire = Instant::now();
                if let Err(e) = self.storage.expire() {
                    eprintln!("[storage] expiry failed: {e:#}");
                }
            }

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

                    match self.storage.ingest(&content, Source::Poll, &[]) {
                        Ok(IngestOutcome::Inserted(entry) | IngestOutcome::Bumped(entry)) => {
                            // Use chars().take() for safe Unicode truncation
                            let preview: String = entry.preview.chars().take(40).collect();
                            eprintln!(
                                "[{}] Saved: {} bytes, preview: {}...",
                                source,
                                entry.size,
                                preview
                            );
                        }
                        Ok(IngestOutcome::Skipped(reason)) => {
                            let reason = self.storage.policy().describe_skip(&reason);
                            eprintln!("[{source}] skipped: {reason}");
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

/// Whether some process holds the exclusive lock on `lock_path`. The lock is
/// `flock`-based (std `File::try_lock` on Linux), so a daemon from a version
/// that locked with fs2 is detected too. A successful probe lock is released
/// when the file closes.
fn lock_is_held(lock_path: &Path) -> bool {
    match File::open(lock_path) {
        Ok(file) => matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        Err(_) => false,
    }
}

/// Open (or create) the daemon lock file without truncating, so a failed
/// attempt can't wipe the running daemon's PID. The file is made 0600 even if
/// an older version created it world-readable.
fn open_lock_file(lock_path: &Path) -> Result<File> {
    let file = perms::private_file_options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .with_context(|| format!("Failed to create lock file: {:?}", lock_path))?;
    let mut permissions = file.metadata()?.permissions();
    if permissions.mode() & 0o777 != perms::FILE_MODE {
        permissions.set_mode(perms::FILE_MODE);
        file.set_permissions(permissions)?;
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{IngestPolicy, ListOrder};
    use tempfile::TempDir;

    /// A daemon on a temp store with its lock in the same temp dir.
    fn test_daemon(dir: &TempDir) -> Result<Daemon> {
        let storage = Storage::open(dir.path().to_path_buf(), Some(100), IngestPolicy::default())?;
        Daemon::with_lock_path(storage, &dir.path().join("clipstack.lock"))
    }

    #[test]
    fn test_daemon_creation() {
        let dir = TempDir::new().unwrap();
        // Use local lock file for test isolation
        let daemon = test_daemon(&dir).unwrap();
        assert!(!daemon.running.load(Ordering::SeqCst));
    }

    #[test]
    fn test_lock_file_records_daemon_pid() {
        let dir = TempDir::new().unwrap();
        let _daemon = test_daemon(&dir).unwrap();

        let pid = Daemon::read_pid(&dir.path().join("clipstack.lock"));
        assert_eq!(pid, Some(std::process::id()));
    }

    #[test]
    fn test_second_daemon_fails_without_clobbering_pid() {
        let dir = TempDir::new().unwrap();
        let lock = dir.path().join("clipstack.lock");
        let _first = test_daemon(&dir).unwrap();

        let second = test_daemon(&dir);
        assert!(second.is_err(), "second daemon must not acquire the lock");
        assert_eq!(Daemon::read_pid(&lock), Some(std::process::id()));
    }

    #[test]
    fn saves_new_clipboard_content_once_with_poll_source() {
        let dir = TempDir::new().unwrap();
        let daemon = test_daemon(&dir).unwrap();
        let mut last = None;
        daemon.check_and_save(Ok("copied text".to_string()), &mut last, "clipboard");
        daemon.check_and_save(Ok("copied text".to_string()), &mut last, "clipboard");
        daemon.check_and_save(Ok(String::new()), &mut last, "clipboard");

        let entries = daemon.storage.list(ListOrder::Recency, None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, Source::Poll);
        assert_eq!(daemon.storage.load_content(entries[0].id).unwrap(), "copied text");
    }

    #[test]
    fn lock_is_held_while_a_daemon_runs_and_released_on_drop() {
        let dir = TempDir::new().unwrap();
        let lock = dir.path().join("clipstack.lock");
        assert!(!lock_is_held(&lock), "no lock file yet");

        let daemon = test_daemon(&dir).unwrap();
        assert!(lock_is_held(&lock));
        assert!(lock_is_held(&lock), "checking doesn't steal or release it");

        drop(daemon);
        assert!(!lock_is_held(&lock));
        let _again = test_daemon(&dir).expect("lock is free after the daemon exits");
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
        let daemon = test_daemon(&dir).unwrap();

        let handle = daemon.stop_handle();
        daemon.running.store(true, Ordering::SeqCst);
        assert!(handle.load(Ordering::SeqCst));

        daemon.stop();
        assert!(!handle.load(Ordering::SeqCst));
    }

    #[test]
    fn test_lock_file_is_0600() {
        let dir = TempDir::new().unwrap();
        let _daemon = test_daemon(&dir).unwrap();

        let meta = std::fs::metadata(dir.path().join("clipstack.lock")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn test_open_lock_file_tightens_existing_world_readable_file() {
        let dir = TempDir::new().unwrap();
        let lock = dir.path().join("clipstack.lock");
        std::fs::write(&lock, "").unwrap();
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _file = open_lock_file(&lock).unwrap();

        let meta = std::fs::metadata(&lock).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
}

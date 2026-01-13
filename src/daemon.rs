use crate::clipboard::Clipboard;
use crate::storage::Storage;
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct Daemon {
    storage: Storage,
    running: Arc<AtomicBool>,
    poll_interval: Duration,
}

impl Daemon {
    pub fn new(storage_dir: Option<PathBuf>) -> Result<Self> {
        let base_dir = storage_dir.unwrap_or_else(Storage::default_dir);
        let storage = Storage::new(base_dir)?;

        Ok(Self {
            storage,
            running: Arc::new(AtomicBool::new(false)),
            poll_interval: Duration::from_millis(250),
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

        eprintln!("clipd daemon started, monitoring clipboard + primary selection...");

        while self.running.load(Ordering::SeqCst) {
            // Check regular clipboard
            self.check_and_save(Clipboard::paste(), &mut last_clipboard_hash, "clipboard");

            // Check PRIMARY selection (mouse selection, used by terminals)
            self.check_and_save(Clipboard::paste_primary(), &mut last_primary_hash, "primary");

            std::thread::sleep(self.poll_interval);
        }

        eprintln!("clipd daemon stopped");
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
                            eprintln!(
                                "[{}] Saved: {} bytes, preview: {}...",
                                source,
                                entry.size,
                                &entry.preview[..entry.preview.len().min(40)]
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
        let daemon = Daemon::new(Some(dir.path().to_path_buf())).unwrap();
        assert!(!daemon.running.load(Ordering::SeqCst));
    }

    #[test]
    fn test_daemon_stop_handle() {
        let dir = TempDir::new().unwrap();
        let daemon = Daemon::new(Some(dir.path().to_path_buf())).unwrap();

        let handle = daemon.stop_handle();
        daemon.running.store(true, Ordering::SeqCst);
        assert!(handle.load(Ordering::SeqCst));

        daemon.stop();
        assert!(!handle.load(Ordering::SeqCst));
    }
}

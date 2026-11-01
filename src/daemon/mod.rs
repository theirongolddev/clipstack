pub mod capture;

use crate::clipboard::Clipboard;
use crate::config::{CaptureConfig, SocketConfig};
use crate::perms;
use crate::storage::Storage;
use anyhow::{Context, Result};
use capture::Capture;
use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How often a running daemon applies retention and the size caps, so
/// entries age out without a new copy or command.
const EXPIRE_INTERVAL: Duration = Duration::from_secs(60);
/// The main loop's tick (watcher checks, `running` checks); polling ticks
/// at its own interval when that is shorter.
const TICK: Duration = Duration::from_millis(100);

/// What the daemon runs with, from the config file and `main`.
#[derive(Debug, Clone)]
pub struct DaemonOptions {
    pub capture: CaptureConfig,
    #[expect(dead_code, reason = "read by the socket server")]
    pub socket: SocketConfig,
    pub clipboard: Clipboard,
    /// The command `wl-paste --watch` runs per selection:
    /// `<clipstack> --storage-dir <dir> [--config <file>] __watch-hook`
    pub hook: Vec<OsString>,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            capture: CaptureConfig::default(),
            socket: SocketConfig::default(),
            clipboard: Clipboard::system(),
            hook: Vec::new(),
        }
    }
}

pub struct Daemon {
    storage: Storage,
    options: DaemonOptions,
    running: Arc<AtomicBool>,
    _lock_file: File, // Keep lock file open to maintain lock
}

impl Daemon {
    /// The daemon lock file: `<runtime dir>/clipstack.lock`.
    pub fn lock_file_path() -> Result<PathBuf> {
        Ok(runtime_dir()?.join("clipstack.lock"))
    }

    /// Check if daemon is currently running by testing the lock file
    pub fn is_running() -> bool {
        Self::lock_file_path().is_ok_and(|path| lock_is_held(&path))
    }

    /// Read the PID recorded in a lock file
    pub fn read_pid(lock_path: &Path) -> Option<u32> {
        std::fs::read_to_string(lock_path).ok()?.trim().parse().ok()
    }

    /// PID of the running daemon, if one holds the global lock
    pub fn running_pid() -> Option<u32> {
        let path = Self::lock_file_path().ok()?;
        if lock_is_held(&path) { Self::read_pid(&path) } else { None }
    }

    /// A daemon saving into `storage`, holding the global lock file.
    pub fn new(storage: Storage, options: DaemonOptions) -> Result<Self> {
        Self::with_lock_path(storage, options, &Self::lock_file_path()?)
    }

    /// A daemon holding the lock at `lock_path` (tests use a temp path).
    pub fn with_lock_path(storage: Storage, options: DaemonOptions, lock_path: &Path) -> Result<Self> {
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
            options,
            running: Arc::new(AtomicBool::new(false)),
            _lock_file: lock_file,
        })
    }

    /// Run until stopped (SIGTERM/SIGINT/SIGHUP or `stop()`). Must be called
    /// from the main thread: the watch child is spawned from it.
    pub fn run(&self) -> Result<()> {
        self.running.store(true, Ordering::SeqCst);
        eprintln!("clipstack daemon started");

        let poll_interval = Duration::from_millis(self.options.capture.poll_interval_ms);
        let mut capture = Capture::new(
            self.options.capture.mode,
            poll_interval,
            self.options.clipboard.clone(),
            self.options.hook.clone(),
        );
        let mut last_expire = Instant::now();

        let result = loop {
            if !self.running.load(Ordering::SeqCst) {
                break Ok(());
            }
            let now = Instant::now();
            if let Err(e) = capture.step(&self.storage, now) {
                break Err(e);
            }
            if now.saturating_duration_since(last_expire) >= EXPIRE_INTERVAL {
                last_expire = now;
                if let Err(e) = self.storage.expire() {
                    eprintln!("[storage] expiry failed: {e:#}");
                }
            }
            let tick = if capture.is_polling() { TICK.min(poll_interval) } else { TICK };
            std::thread::sleep(tick);
        };

        capture.stop();
        eprintln!("clipstack daemon stopped");
        result
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

/// The private runtime dir for the lock and socket: `$XDG_RUNTIME_DIR`, else
/// `/tmp/clipstack-<uid>` (created 0700; it must be a real directory owned by
/// us with no group/other access, or another user could pre-create it).
pub fn runtime_dir() -> Result<PathBuf> {
    runtime_dir_from(std::env::var_os("XDG_RUNTIME_DIR"), Path::new("/tmp"))
}

/// `runtime_dir` with the environment and `/tmp` injected for tests. An
/// empty or relative `XDG_RUNTIME_DIR` is ignored, as the XDG spec says.
fn runtime_dir_from(xdg: Option<std::ffi::OsString>, tmp: &Path) -> Result<PathBuf> {
    if let Some(dir) = xdg.map(PathBuf::from).filter(|p| p.is_absolute()) {
        return Ok(dir);
    }
    let uid = current_uid();
    let dir = tmp.join(format!("clipstack-{uid}"));
    match std::fs::DirBuilder::new().mode(perms::DIR_MODE).create(&dir) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
            return Err(anyhow::Error::new(e).context(format!("Failed to create {}", dir.display())));
        }
        _ => {}
    }
    // symlink_metadata: a symlink planted at the path is rejected, not followed
    let meta = std::fs::symlink_metadata(&dir)
        .with_context(|| format!("Failed to inspect {}", dir.display()))?;
    if !meta.is_dir() {
        anyhow::bail!("runtime dir {} is not a directory (a symlink?)", dir.display());
    }
    if meta.uid() != uid {
        anyhow::bail!("runtime dir {} is owned by uid {}, not {uid}", dir.display(), meta.uid());
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        anyhow::bail!("runtime dir {} has mode {mode:04o}; it must be 0700", dir.display());
    }
    Ok(dir)
}

/// Our real uid.
fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
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
    use crate::storage::IngestPolicy;
    use tempfile::TempDir;

    /// A daemon on a temp store with its lock in the same temp dir.
    fn test_daemon(dir: &TempDir) -> Result<Daemon> {
        let storage = Storage::open(dir.path().to_path_buf(), Some(100), IngestPolicy::default())?;
        Daemon::with_lock_path(storage, DaemonOptions::default(), &dir.path().join("clipstack.lock"))
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
    fn runtime_dir_prefers_xdg() {
        let tmp = TempDir::new().unwrap();
        let xdg = tmp.path().join("run-user");
        assert_eq!(runtime_dir_from(Some(xdg.clone().into()), tmp.path()).unwrap(), xdg);
        // Unset, empty or relative XDG_RUNTIME_DIR falls back to /tmp/clipstack-<uid>
        let fallback = tmp.path().join(format!("clipstack-{}", current_uid()));
        for bad in [None, Some("".into()), Some("relative/dir".into())] {
            assert_eq!(runtime_dir_from(bad, tmp.path()).unwrap(), fallback);
        }
    }

    #[test]
    fn fallback_dir_created_0700() {
        let tmp = TempDir::new().unwrap();
        let dir = runtime_dir_from(None, tmp.path()).unwrap();
        assert_eq!(dir, tmp.path().join(format!("clipstack-{}", current_uid())));
        let meta = std::fs::symlink_metadata(&dir).unwrap();
        assert!(meta.is_dir());
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
        // A second call reuses it
        assert_eq!(runtime_dir_from(None, tmp.path()).unwrap(), dir);
    }

    #[test]
    fn rejects_fallback_with_wrong_mode_or_symlink() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(format!("clipstack-{}", current_uid()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = runtime_dir_from(None, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("0755"), "{err}");

        let tmp = TempDir::new().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, tmp.path().join(format!("clipstack-{}", current_uid())))
            .unwrap();
        let err = runtime_dir_from(None, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("not a directory"), "{err}");
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

//! Isolation harness for tests that run the `clipstack` binary.
//!
//! Every daemon, socket and CLI integration test goes through `TestEnv`:
//! temp `XDG_RUNTIME_DIR`, `XDG_DATA_HOME`, `XDG_CONFIG_HOME` and `HOME`,
//! `--storage-dir`, `tests/fakes` first on `PATH` and no `WAYLAND_DISPLAY`,
//! so a test can never reach the user's real clipboard, store or daemon.

#![allow(dead_code)] // each test crate uses a different subset

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

pub struct TestEnv {
    root: TempDir,
    pub runtime_dir: PathBuf,
    pub data_home: PathBuf,
    pub config_home: PathBuf,
    pub home: PathBuf,
    pub storage_dir: PathBuf,
    pub fake_clip_dir: PathBuf,
    children: Vec<Child>,
}

/// Exit status and captured stderr of a spawned child.
pub struct Finished {
    pub status: ExitStatus,
    pub stderr: String,
}

impl TestEnv {
    pub fn new() -> Self {
        let root = TempDir::new().expect("create test root");
        let path = |name: &str| {
            let p = root.path().join(name);
            std::fs::create_dir_all(&p).expect("create test dir");
            p
        };
        let env = Self {
            runtime_dir: path("runtime"),
            data_home: path("data"),
            config_home: path("config"),
            home: path("home"),
            storage_dir: root.path().join("storage"),
            fake_clip_dir: path("fake-clip"),
            root,
            children: Vec::new(),
        };
        // XDG_RUNTIME_DIR must be 0700 for well-behaved tools
        set_mode(&env.runtime_dir, 0o700);
        env.assert_isolated();
        env
    }

    /// Refuse to run if any path could be the user's real runtime dir or store.
    fn assert_isolated(&self) {
        let uid = std::fs::metadata(self.root.path())
            .map(|m| std::os::unix::fs::MetadataExt::uid(&m))
            .expect("stat test root");
        let real_runtime = PathBuf::from(format!("/run/user/{uid}"));
        for dir in [&self.runtime_dir, &self.data_home, &self.storage_dir] {
            assert!(
                !dir.starts_with("/run/user"),
                "test dir {dir:?} is under /run/user"
            );
        }
        assert_ne!(self.runtime_dir, real_runtime, "runtime dir is the real one");
        assert!(self.runtime_dir.starts_with(self.root.path()));
        assert!(self.storage_dir.starts_with(self.root.path()));
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }

    pub fn fakes_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fakes")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.runtime_dir.join("clipstack.lock")
    }

    /// `clipstack --storage-dir <temp>` with the isolated environment.
    pub fn cmd(&self) -> Command {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![Self::fakes_dir()];
        paths.extend(std::env::split_paths(&path));

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_clipstack"));
        cmd.env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("XDG_DATA_HOME", &self.data_home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("HOME", &self.home)
            .env("FAKE_CLIP_DIR", &self.fake_clip_dir)
            .env("PATH", std::env::join_paths(paths).expect("join PATH"))
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .env_remove("CLIPSTACK_MAX_ENTRIES")
            .arg("--storage-dir")
            .arg(&self.storage_dir)
            .stdin(Stdio::null());
        cmd
    }

    /// Spawn a child with piped stdout/stderr. Returns its PID; the child is
    /// killed when the `TestEnv` is dropped unless it was waited for.
    pub fn spawn(&mut self, mut cmd: Command) -> u32 {
        let child = cmd
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn clipstack");
        let pid = child.id();
        self.children.push(child);
        pid
    }

    /// Wait up to `timeout` for a spawned child to exit.
    pub fn wait_for_exit(&mut self, pid: u32, timeout: Duration) -> Option<Finished> {
        let pos = self.children.iter().position(|c| c.id() == pid)?;
        let deadline = Instant::now() + timeout;
        loop {
            if self.children[pos].try_wait().expect("try_wait").is_some() {
                let mut child = self.children.remove(pos);
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                // Already reaped by try_wait; wait() returns the cached status
                let status = child.wait().expect("wait");
                return Some(Finished { status, stderr });
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Send `signal` (e.g. "TERM") to a child this env spawned. Panics for
    /// any other PID so a test can never signal a process it doesn't own.
    pub fn signal(&self, pid: u32, signal: &str) {
        assert!(
            self.children.iter().any(|c| c.id() == pid),
            "refusing to signal pid {pid}: not spawned by this TestEnv"
        );
        let status = Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(pid.to_string())
            .status()
            .expect("run kill");
        assert!(status.success(), "kill -{signal} {pid} failed");
    }

    /// Wait until the daemon lock in our runtime dir records `pid`.
    pub fn wait_for_lock_pid(&self, pid: u32, timeout: Duration) -> bool {
        wait_until(timeout, || {
            std::fs::read_to_string(self.lock_path())
                .is_ok_and(|s| s.trim() == pid.to_string())
        })
    }
}

impl Default for TestEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        // Only ever the children we spawned ourselves
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    done()
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

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

    pub fn socket_path(&self) -> PathBuf {
        self.runtime_dir.join("clipstack.sock")
    }

    /// Write `$XDG_CONFIG_HOME/clipstack/config.toml`.
    pub fn write_config(&self, toml: &str) {
        let dir = self.config_home.join("clipstack");
        std::fs::create_dir_all(&dir).expect("create config dir");
        std::fs::write(dir.join("config.toml"), toml).expect("write config");
    }

    /// `clipstack export` parsed: one JSON value per entry.
    pub fn export(&self) -> Vec<serde_json::Value> {
        let out = self.cmd().arg("export").output().expect("run export");
        assert!(out.status.success(), "export failed: {out:?}");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).expect("export line is JSON"))
            .collect()
    }

    /// Wait until the history holds `n` entries; returns them.
    pub fn wait_for_entries(&self, n: usize) -> Vec<serde_json::Value> {
        let mut entries = Vec::new();
        let done = wait_until(Duration::from_secs(5), || {
            entries = self.export();
            entries.len() >= n
        });
        assert!(done, "expected {n} entries, have {entries:?}");
        entries
    }

    /// Spawn `clipstack daemon` and wait until it holds the lock.
    pub fn start_daemon(&mut self) -> u32 {
        let mut cmd = self.cmd();
        cmd.arg("daemon");
        let pid = self.spawn(cmd);
        assert!(self.wait_for_lock_pid(pid, Duration::from_secs(5)), "daemon did not take the lock");
        pid
    }

    /// `clipstack --storage-dir <temp>` with the isolated environment.
    pub fn cmd(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_clipstack"));
        self.isolate(&mut cmd);
        cmd.arg("--storage-dir").arg(&self.storage_dir);
        cmd
    }

    /// One of the `tests/fakes` programs (e.g. "wl-paste") with the isolated
    /// environment, for testing the fakes themselves.
    pub fn fake_cmd(&self, name: &str) -> Command {
        let fake = match name {
            "wl-paste" => "wl-paste",
            "wl-copy" => "wl-copy",
            "wtype" => "wtype",
            other => unreachable!("no fake named {other:?}"),
        };
        let mut cmd = Command::new(Self::fakes_dir().join(fake));
        self.isolate(&mut cmd);
        cmd
    }

    /// Temp XDG dirs and HOME, fakes first on PATH, no Wayland, null stdin.
    fn isolate(&self, cmd: &mut Command) {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![Self::fakes_dir()];
        paths.extend(std::env::split_paths(&path));
        cmd.env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("XDG_DATA_HOME", &self.data_home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("HOME", &self.home)
            .env("FAKE_CLIP_DIR", &self.fake_clip_dir)
            .env("PATH", std::env::join_paths(paths).expect("join PATH"))
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .env_remove("CLIPSTACK_MAX_ENTRIES")
            .env_remove("CLIPBOARD_STATE")
            .env_remove("FAKE_WATCH_UNSUPPORTED")
            .stdin(Stdio::null());
    }

    /// Put `content` on the fake clipboard, offered as `types`.
    pub fn set_clipboard(&self, content: &str, types: &[&str]) {
        std::fs::write(self.fake_clip_dir.join("content"), content).expect("write fake content");
        let mut list = types.join("\n");
        list.push('\n');
        std::fs::write(self.fake_clip_dir.join("types"), list).expect("write fake types");
    }

    /// What the fake wl-copy last stored, if anything.
    pub fn clipboard_content(&self) -> Option<String> {
        std::fs::read_to_string(self.fake_clip_dir.join("content")).ok()
    }

    /// PID of the running fake `wl-paste --watch`, from `$FAKE_CLIP_DIR/watch.pid`.
    pub fn watch_pid(&self) -> Option<u32> {
        std::fs::read_to_string(self.fake_clip_dir.join("watch.pid")).ok()?.trim().parse().ok()
    }

    /// Wait for a fake `wl-paste --watch` to start and return its PID.
    pub fn wait_for_watch_pid(&self, timeout: Duration) -> Option<u32> {
        let mut pid = None;
        wait_until(timeout, || {
            pid = self.watch_pid();
            pid.is_some()
        });
        pid
    }

    /// Write `content` to a file and tell the fake watcher a new selection
    /// with `state` ("data" or "sensitive") holds it.
    pub fn watch_offer(&self, state: &str, content: &[u8]) {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let file = self.fake_clip_dir.join(format!("offer-{n}"));
        std::fs::write(&file, content).expect("write offer");
        self.watch_event(&format!("{state} {}", file.display()));
    }

    /// Send one event line to the fake watcher's FIFO. Opening a FIFO blocks
    /// until the watcher reads it, so this gives up (panics) after 5 s.
    pub fn watch_event(&self, line: &str) {
        let fifo = self.fake_clip_dir.join("events");
        assert!(
            wait_until(Duration::from_secs(5), || fifo.exists()),
            "fake wl-paste --watch never created {fifo:?}"
        );
        let line = format!("{line}\n");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(std::fs::write(&fifo, line));
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("fake wl-paste --watch is not reading its events FIFO")
            .expect("write watch event");
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
        // ...and a fake watcher one of our daemons started, recognised by
        // the storage dir in its command line so a reused PID is never hit
        if let Some(pid) = self.watch_pid() {
            let ours = std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|cmdline| {
                let needle = self.storage_dir.as_os_str().as_encoded_bytes();
                cmdline.windows(needle.len()).any(|w| w == needle)
            });
            if ours {
                let _ = Command::new("kill").arg("-KILL").arg(pid.to_string()).status();
            }
        }
    }
}

/// Whether a process with this PID exists and is not a zombie.
pub fn process_alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // "pid (comm) S ..." — the state follows the last ')'
        Ok(stat) => stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z" && state != "X"),
        Err(_) => false,
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

/// CRC-32 as PNG chunks use it.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A small PNG of `width` x `height` (signature, IHDR, a private chunk
/// holding `tag` so images of one size can differ, empty IDAT, IEND).
pub fn test_png(width: u32, height: u32, tag: &[u8]) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"tEXt", tag);
    chunk(&mut out, b"IDAT", &[]);
    chunk(&mut out, b"IEND", &[]);
    out
}

impl TestEnv {
    /// Store `png` as an image entry the way the daemon would: a watch hook
    /// run with the fake clipboard offering only `image/png` (wl-paste
    /// `--watch` without `--type` passes it as data on stdin).
    pub fn put_image(&self, png: &[u8]) {
        std::fs::write(self.fake_clip_dir.join("content"), png).expect("write fake content");
        std::fs::write(self.fake_clip_dir.join("types"), "image/png\n").expect("write fake types");
        let status = self
            .cmd()
            .arg("__watch-hook")
            .env("CLIPBOARD_STATE", "data")
            .stdin(std::fs::File::open(self.fake_clip_dir.join("content")).expect("open fake content"))
            .status()
            .expect("run watch hook");
        assert!(status.success());
    }
}

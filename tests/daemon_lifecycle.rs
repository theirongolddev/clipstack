//! Daemon lifecycle tests. All run through the isolated `TestEnv`.

mod common;

use common::{process_alive, wait_until, TestEnv};
use std::time::Duration;

#[test]
fn harness_isolates_runtime_dir() {
    let mut env = TestEnv::new();
    let mut cmd = env.cmd();
    cmd.arg("daemon");
    let pid = env.spawn(cmd);

    assert!(
        env.wait_for_lock_pid(pid, Duration::from_secs(5)),
        "daemon lock with our PID should appear under the temp XDG_RUNTIME_DIR"
    );
    assert!(env.lock_path().starts_with(env.root()));

    // The daemon watches through the fake, never the real wl-paste
    let calls = env.fake_clip_dir.join("wl-paste.calls");
    assert!(
        wait_until(Duration::from_secs(5), || calls.exists()),
        "daemon should have called tests/fakes/wl-paste"
    );
    let log = std::fs::read_to_string(&calls).unwrap();
    assert!(
        log.lines().any(|l| l.starts_with("--no-newline --type text --watch ")),
        "calls: {log:?}"
    );

    // Storage went to --storage-dir, not $XDG_DATA_HOME/clipd
    assert!(env.storage_dir.is_dir());
    assert!(!env.data_home.join("clipd").exists());
}

#[test]
fn daemon_exits_zero_on_sigterm() {
    let mut env = TestEnv::new();
    let mut cmd = env.cmd();
    cmd.arg("daemon");
    let pid = env.spawn(cmd);
    assert!(env.wait_for_lock_pid(pid, Duration::from_secs(5)));

    env.signal(pid, "TERM");
    let done = env
        .wait_for_exit(pid, Duration::from_secs(2))
        .expect("daemon should exit within 2 s of SIGTERM");
    assert_eq!(
        done.status.code(),
        Some(0),
        "expected a clean exit, got {:?}; stderr: {}",
        done.status,
        done.stderr
    );
    assert!(done.stderr.contains("daemon stopped"), "stderr: {}", done.stderr);
}

#[test]
fn sigterm_stops_watch_child_and_removes_socket() {
    let mut env = TestEnv::new();
    let pid = env.start_daemon();
    let watch = env.wait_for_watch_pid(Duration::from_secs(5)).expect("daemon runs wl-paste --watch");
    let sock = env.socket_path();
    assert!(wait_until(Duration::from_secs(5), || sock.exists()), "socket created");

    env.signal(pid, "TERM");
    let done = env.wait_for_exit(pid, Duration::from_secs(3)).expect("daemon exits after SIGTERM");
    assert_eq!(done.status.code(), Some(0), "stderr: {}", done.stderr);
    assert!(!process_alive(watch), "watch child {watch} still running after the daemon stopped");
    assert!(!sock.exists(), "socket left behind at {}", sock.display());
    assert!(done.stderr.trim_end().ends_with("clipstack daemon stopped"), "stderr: {}", done.stderr);
}

#[test]
fn sigkill_daemon_terminates_watch_child() {
    let mut env = TestEnv::new();
    let pid = env.start_daemon();
    let watch = env.wait_for_watch_pid(Duration::from_secs(5)).expect("daemon runs wl-paste --watch");
    assert!(process_alive(watch));

    // SIGKILL runs no cleanup: only PR_SET_PDEATHSIG can stop the child
    env.signal(pid, "KILL");
    env.wait_for_exit(pid, Duration::from_secs(3)).expect("daemon dies on SIGKILL");
    assert!(
        wait_until(Duration::from_secs(5), || !process_alive(watch)),
        "watch child {watch} outlived the killed daemon"
    );
}

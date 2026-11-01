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

#[test]
fn serve_prints_migration_message_exits_2() {
    let mut env = TestEnv::new();
    for args in [&["serve"][..], &["serve", "--port", "7779"], &["serve", "-p", "1"]] {
        // Spawned and waited with a timeout: the old TCP server never exits
        let mut cmd = env.cmd();
        cmd.args(args);
        let pid = env.spawn(cmd);
        let done = env.wait_for_exit(pid, Duration::from_secs(5)).expect("serve exits at once");
        let stderr = done.stderr;
        assert_eq!(done.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains("serve was removed"), "{stderr}");
        let sock = env.socket_path();
        assert!(stderr.contains(&*sock.to_string_lossy()), "names the socket: {stderr}");
        assert!(stderr.contains("Remote clipboard"), "{stderr}");
    }
    // Exits before opening (and so creating) the store
    assert!(!env.storage_dir.exists(), "serve opened the store");
}

fn status(env: &TestEnv) -> String {
    let out = env.cmd().arg("status").output().expect("run clipstack status");
    assert!(out.status.success(), "status failed: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn status_line<'a>(status: &'a str, label: &str) -> &'a str {
    status.lines().find(|l| l.starts_with(label)).unwrap_or_else(|| panic!("no {label} line in:\n{status}"))
}

#[test]
fn status_reports_socket_reachable_and_capture_mode() {
    let mut env = TestEnv::new();
    let sock = env.socket_path().display().to_string();
    let idle = status(&env);
    assert!(status_line(&idle, "Daemon:").contains("not running"), "{idle}");
    let line = status_line(&idle, "Socket:");
    assert!(line.contains(&sock) && line.contains("not listening"), "{idle}");

    // Running, watching
    let pid = env.start_daemon();
    env.wait_for_watch_pid(Duration::from_secs(5)).expect("daemon runs wl-paste --watch");
    assert!(wait_until(Duration::from_secs(5), || env.socket_path().exists()));
    let running = status(&env);
    assert!(status_line(&running, "Daemon:").contains(&format!("running (pid {pid})")), "{running}");
    let line = status_line(&running, "Socket:");
    assert!(line.contains(&sock) && line.contains("reachable"), "{running}");
    assert!(line.contains("capture=watch") && line.contains("pull=off"), "{running}");
    env.signal(pid, "TERM");
    env.wait_for_exit(pid, Duration::from_secs(3)).expect("daemon stops");

    // Polling, pull allowed
    env.write_config("[capture]\nmode = \"poll\"\n[socket]\nallow_pull = true\n");
    let pid = env.start_daemon();
    assert!(wait_until(Duration::from_secs(5), || env.socket_path().exists()));
    let line = status(&env);
    let line = status_line(&line, "Socket:");
    assert!(line.contains("capture=poll") && line.contains("pull=on"), "{line}");
    env.signal(pid, "TERM");
    env.wait_for_exit(pid, Duration::from_secs(3)).expect("daemon stops");

    // Lock held but nothing answers (an old daemon, or the socket disabled)
    env.write_config("[socket]\nenabled = false\n");
    let pid = env.start_daemon();
    let line = status(&env);
    assert!(status_line(&line, "Daemon:").contains("running"), "{line}");
    let line = status_line(&line, "Socket:");
    assert!(line.contains("capture=unknown"), "{line}");
    env.signal(pid, "TERM");
    env.wait_for_exit(pid, Duration::from_secs(3)).expect("daemon stops");
}

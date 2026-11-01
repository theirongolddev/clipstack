//! Daemon lifecycle tests. All run through the isolated `TestEnv`.

mod common;

use common::{wait_until, TestEnv};
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

    // The daemon polls the fake, never the real wl-paste
    let calls = env.fake_clip_dir.join("wl-paste.calls");
    assert!(
        wait_until(Duration::from_secs(5), || calls.exists()),
        "daemon should have called tests/fakes/wl-paste"
    );
    let log = std::fs::read_to_string(&calls).unwrap();
    assert!(log.lines().any(|l| l == "--no-newline"), "calls: {log:?}");

    // Storage went to --storage-dir, not $XDG_DATA_HOME/clipd
    assert!(env.storage_dir.is_dir());
    assert!(!env.data_home.join("clipd").exists());
}

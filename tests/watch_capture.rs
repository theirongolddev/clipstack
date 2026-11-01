//! Clipboard capture: the fake wl-clipboard, the `__watch-hook` and the
//! daemon's watch/poll capture. All run through the isolated `TestEnv`.

mod common;

use common::{TestEnv, wait_until};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

fn output(mut cmd: Command) -> Output {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).output().expect("run command")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn fake_clipboard_roundtrip() {
    let mut env = TestEnv::new();

    // Empty clipboard: paste and --list-types say "No selection"
    let mut paste = env.fake_cmd("wl-paste");
    paste.args(["--no-newline", "--type", "text"]);
    let out = output(paste);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("No selection"));

    // wl-copy stores stdin and offers text types
    let mut copy = env.fake_cmd("wl-copy");
    copy.stdin(Stdio::piped());
    let mut child = copy.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), b"hello\nworld").unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(env.clipboard_content().as_deref(), Some("hello\nworld"));

    let mut paste = env.fake_cmd("wl-paste");
    paste.args(["--no-newline", "--type", "text"]);
    let out = output(paste);
    assert!(out.status.success());
    assert_eq!(stdout(&out), "hello\nworld");

    let mut types = env.fake_cmd("wl-paste");
    types.arg("--list-types");
    let listed = stdout(&output(types));
    assert!(listed.lines().any(|t| t == "text/plain;charset=utf-8"), "{listed:?}");
    assert!(!listed.contains("x-kde-passwordManagerHint"), "{listed:?}");

    // wl-copy --sensitive TEXT adds the password-manager hint
    let mut copy = env.fake_cmd("wl-copy");
    copy.args(["--sensitive", "hunter2"]);
    assert!(output(copy).status.success());
    assert_eq!(env.clipboard_content().as_deref(), Some("hunter2"));
    let mut types = env.fake_cmd("wl-paste");
    types.arg("--list-types");
    let listed = stdout(&output(types));
    assert!(listed.lines().any(|t| t == "x-kde-passwordManagerHint"), "{listed:?}");

    // Types set directly by a test are listed as given
    env.set_clipboard("<b>hi</b>", &["text/html", "text/plain"]);
    let mut types = env.fake_cmd("wl-paste");
    types.arg("--list-types");
    assert_eq!(stdout(&output(types)), "text/html\ntext/plain\n");

    // --watch runs the command once per event with CLIPBOARD_STATE and stdin
    let log = env.fake_clip_dir.join("watch.log");
    let mut watch = env.fake_cmd("wl-paste");
    watch.args(["--type", "text", "--watch", "sh", "-c"]).arg(format!(
        "printf '%s:' \"$CLIPBOARD_STATE\" >> '{0}'; cat >> '{0}'; echo >> '{0}'",
        log.display()
    ));
    let pid = env.spawn(watch);
    assert_eq!(env.wait_for_watch_pid(Duration::from_secs(5)), Some(pid));

    env.watch_offer("data", b"first");
    env.watch_offer("sensitive", b"secret");
    env.watch_event("nil");
    let expected = "data:first\nsensitive:secret\nnil:\n";
    assert!(
        wait_until(Duration::from_secs(5), || {
            std::fs::read_to_string(&log).is_ok_and(|s| s == expected)
        }),
        "watch log: {:?}",
        std::fs::read_to_string(&log)
    );
    env.signal(pid, "TERM");
    assert!(env.wait_for_exit(pid, Duration::from_secs(2)).is_some(), "watcher exits on SIGTERM");

    // FAKE_WATCH_UNSUPPORTED=1: --watch fails like a compositor without data-control
    let mut watch = env.fake_cmd("wl-paste");
    watch.args(["--type", "text", "--watch", "true"]).env("FAKE_WATCH_UNSUPPORTED", "1");
    let out = output(watch);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("data-control"), "{out:?}");

}

const TEXT_TYPES: &[&str] = &["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];

/// The whole history, oldest first, via `clipstack export`.
fn export(env: &TestEnv) -> Vec<serde_json::Value> {
    let mut cmd = env.cmd();
    cmd.arg("export");
    let out = output(cmd);
    assert!(out.status.success(), "export failed: {out:?}");
    stdout(&out).lines().map(|l| serde_json::from_str(l).expect("export line is JSON")).collect()
}

fn write_config(env: &TestEnv, toml: &str) {
    let dir = env.config_home.join("clipstack");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), toml).unwrap();
}

/// What `clipstack __watch-hook` did with `input` under `CLIPBOARD_STATE=state`.
struct Hook {
    status: std::process::ExitStatus,
    stderr: String,
    /// Whether all of `input` could be written (no EPIPE)
    wrote_all: bool,
}

fn run_hook(env: &TestEnv, state: Option<&str>, input: &[u8]) -> Hook {
    let mut cmd = env.cmd();
    cmd.arg("__watch-hook").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(state) = state {
        cmd.env("CLIPBOARD_STATE", state);
    }
    let mut child = cmd.spawn().expect("spawn hook");
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || std::io::Write::write_all(&mut stdin, &input).is_ok());
    let out = child.wait_with_output().expect("wait for hook");
    Hook {
        status: out.status,
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        wrote_all: writer.join().unwrap(),
    }
}

#[test]
fn hook_ingests_stdin() {
    let env = TestEnv::new();
    env.set_clipboard("hello from watch", TEXT_TYPES);
    let hook = run_hook(&env, Some("data"), b"hello from watch");
    assert!(hook.status.success(), "{}", hook.stderr);
    assert!(hook.wrote_all);

    let entries = export(&env);
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["content"], "hello from watch");
    assert_eq!(entries[0]["source"], "watch");
    assert_eq!(entries[0]["mime"], "text/plain;charset=utf-8");
    assert!(!hook.stderr.contains("hello from watch"), "content must not be logged: {}", hook.stderr);

    // No CLIPBOARD_STATE (older wl-paste) is treated as data
    let hook = run_hook(&env, None, b"second copy");
    assert!(hook.status.success(), "{}", hook.stderr);
    assert_eq!(export(&env).len(), 2);
}

#[test]
fn hook_skips_sensitive_state_and_drains_stdin() {
    let env = TestEnv::new();
    let secret = vec![b's'; 1 << 20];
    let hook = run_hook(&env, Some("sensitive"), &secret);
    assert!(hook.status.success(), "{}", hook.stderr);
    assert!(hook.wrote_all, "the hook must read all of stdin so the source sees no EPIPE");
    assert!(hook.stderr.contains("[watch] skipped sensitive"), "{}", hook.stderr);
    assert!(export(&env).is_empty());
}

#[test]
fn hook_skips_when_types_include_password_hint() {
    let env = TestEnv::new();
    env.set_clipboard("hunter2", &["text/plain;charset=utf-8", "x-kde-passwordManagerHint"]);
    let hook = run_hook(&env, Some("data"), b"hunter2");
    assert!(hook.status.success(), "{}", hook.stderr);
    assert!(hook.stderr.contains("[watch] skipped: marked sensitive"), "{}", hook.stderr);
    assert!(export(&env).is_empty());
}

#[test]
fn hook_rejects_non_utf8() {
    let env = TestEnv::new();
    let hook = run_hook(&env, Some("data"), b"\xff\xfe binary");
    assert!(hook.status.success(), "{}", hook.stderr);
    assert!(hook.stderr.contains("not UTF-8"), "{}", hook.stderr);
    assert!(export(&env).is_empty());
}

#[test]
fn hook_ignores_nil_state() {
    let env = TestEnv::new();
    for state in ["nil", "clear"] {
        let hook = run_hook(&env, Some(state), b"");
        assert!(hook.status.success(), "{state}: {}", hook.stderr);
    }
    // An unknown state is not saved either
    let hook = run_hook(&env, Some("something-new"), b"maybe secret");
    assert!(hook.status.success(), "{}", hook.stderr);
    assert!(hook.wrote_all);
    assert!(export(&env).is_empty());
}

#[test]
fn hook_caps_read_at_max_entry_bytes() {
    let env = TestEnv::new();
    write_config(&env, "[storage]\nmax_entry_bytes = 1024\n");
    env.set_clipboard("", TEXT_TYPES);
    let hook = run_hook(&env, Some("data"), &vec![b'a'; 4096]);
    assert!(hook.status.success(), "{}", hook.stderr);
    assert!(hook.wrote_all, "the rest of stdin is drained, not left unread");
    assert!(hook.stderr.contains("too large (4096 bytes"), "{}", hook.stderr);
    assert!(export(&env).is_empty());

    // Exactly at the cap is saved
    let hook = run_hook(&env, Some("data"), &vec![b'b'; 1024]);
    assert!(hook.status.success(), "{}", hook.stderr);
    assert_eq!(export(&env).len(), 1);
}

const HINT: &str = "x-kde-passwordManagerHint";

/// Start `clipstack daemon` in `env`; returns its PID.
fn start_daemon(env: &mut TestEnv) -> u32 {
    let mut cmd = env.cmd();
    cmd.arg("daemon");
    let pid = env.spawn(cmd);
    assert!(env.wait_for_lock_pid(pid, Duration::from_secs(5)), "daemon did not take the lock");
    pid
}

/// Wait until the history holds `n` entries; returns them.
fn wait_for_entries(env: &TestEnv, n: usize) -> Vec<serde_json::Value> {
    let mut entries = Vec::new();
    let done = wait_until(Duration::from_secs(5), || {
        entries = export(env);
        entries.len() >= n
    });
    assert!(done, "expected {n} entries, have {entries:?}");
    entries
}

#[test]
fn daemon_watch_captures_copy() {
    let mut env = TestEnv::new();
    start_daemon(&mut env);
    env.wait_for_watch_pid(Duration::from_secs(5)).expect("daemon should run wl-paste --watch");

    let calls = std::fs::read_to_string(env.fake_clip_dir.join("wl-paste.calls")).unwrap();
    let watch = calls.lines().find(|l| l.contains("--watch")).expect("a --watch call");
    let storage = env.storage_dir.display().to_string();
    assert!(watch.starts_with("--no-newline --type text --watch "), "{watch}");
    assert!(watch.ends_with(&format!("--storage-dir {storage} __watch-hook")), "{watch}");

    env.set_clipboard("copied in a browser", TEXT_TYPES);
    env.watch_offer("data", b"copied in a browser");
    let entries = wait_for_entries(&env, 1);
    assert_eq!(entries[0]["content"], "copied in a browser");
    assert_eq!(entries[0]["source"], "watch");

    // A sensitive offer is not stored; a later one still is
    env.set_clipboard("hunter2", &["text/plain", HINT]);
    env.watch_offer("sensitive", b"hunter2");
    env.set_clipboard("after the secret", TEXT_TYPES);
    env.watch_offer("data", b"after the secret");
    let entries = wait_for_entries(&env, 2);
    let contents: Vec<_> = entries.iter().map(|e| e["content"].as_str().unwrap()).collect();
    assert_eq!(contents, ["copied in a browser", "after the secret"]);
}

#[test]
fn daemon_falls_back_to_poll_when_watch_unsupported() {
    let mut env = TestEnv::new();
    write_config(&env, "[capture]\npoll_interval_ms = 50\n");
    let mut cmd = env.cmd();
    cmd.arg("daemon").env("FAKE_WATCH_UNSUPPORTED", "1");
    let pid = env.spawn(cmd);
    assert!(env.wait_for_lock_pid(pid, Duration::from_secs(5)));

    env.set_clipboard("polled text", TEXT_TYPES);
    let entries = wait_for_entries(&env, 1);
    assert_eq!(entries[0]["content"], "polled text");
    assert_eq!(entries[0]["source"], "poll");

    env.signal(pid, "TERM");
    let done = env.wait_for_exit(pid, Duration::from_secs(2)).expect("daemon exits");
    assert!(
        done.stderr.contains("[capture] polling every 50 ms: wl-paste --watch exited at once")
            && done.stderr.contains("data-control"),
        "stderr: {}",
        done.stderr
    );
}

/// A daemon in poll mode, polling every 50 ms.
fn start_polling_daemon(env: &mut TestEnv) -> u32 {
    write_config(env, "[capture]\nmode = \"poll\"\npoll_interval_ms = 50\n");
    start_daemon(env)
}

/// Wait until the daemon has polled the fake clipboard `n` more times.
fn wait_for_polls(env: &TestEnv, n: usize) {
    let calls = env.fake_clip_dir.join("wl-paste.calls");
    let count = || {
        std::fs::read_to_string(&calls)
            .map(|s| s.lines().filter(|l| *l == "--no-newline --type text").count())
            .unwrap_or(0)
    };
    let target = count() + n;
    assert!(wait_until(Duration::from_secs(5), || count() >= target), "daemon is not polling");
}

#[test]
fn poll_skips_password_hint_types() {
    let mut env = TestEnv::new();
    start_polling_daemon(&mut env);
    env.set_clipboard("hunter2", &["text/plain;charset=utf-8", HINT]);
    wait_for_polls(&env, 3);
    assert!(export(&env).is_empty(), "a password-manager secret was saved");

    // The secret was read once, not on every poll
    let calls = std::fs::read_to_string(env.fake_clip_dir.join("wl-paste.calls")).unwrap();
    assert_eq!(calls.lines().filter(|l| *l == "--list-types").count(), 1, "{calls}");

    env.set_clipboard("ordinary", TEXT_TYPES);
    assert_eq!(wait_for_entries(&env, 1)[0]["content"], "ordinary");
}

#[test]
fn poll_passes_mime_types_to_ingest() {
    let mut env = TestEnv::new();
    start_polling_daemon(&mut env);
    env.set_clipboard("<b>bold</b>", &["image/png", "text/html", "text/plain;charset=utf-8"]);
    let entries = wait_for_entries(&env, 1);
    assert_eq!(entries[0]["mime"], "text/html");
    assert_eq!(entries[0]["source"], "poll");
}

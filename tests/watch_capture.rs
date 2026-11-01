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

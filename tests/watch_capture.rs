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

#[test]
fn fake_clipboard_handles_binary_png() {
    let env = TestEnv::new();
    let png = common::test_png(2, 3, b"\x00\xff binary");

    // wl-copy --type image/png stores the bytes as they are and offers only that type
    let mut copy = env.fake_cmd("wl-copy");
    copy.args(["--type", "image/png"]).stdin(Stdio::piped());
    let mut child = copy.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), &png).unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(std::fs::read(env.fake_clip_dir.join("content")).unwrap(), png);
    let mut types = env.fake_cmd("wl-paste");
    types.arg("--list-types");
    assert_eq!(stdout(&output(types)), "image/png\n");

    let mut paste = env.fake_cmd("wl-paste");
    paste.args(["--no-newline", "--type", "image/png"]);
    let out = output(paste);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, png);

    // No text type offered: --type text fails like the real wl-paste
    let mut paste = env.fake_cmd("wl-paste");
    paste.args(["--no-newline", "--type", "text"]);
    let out = output(paste);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not available as requested type"), "{out:?}");

    // ...and a text selection is not available as image/png
    env.set_clipboard("words", TEXT_TYPES);
    let mut paste = env.fake_cmd("wl-paste");
    paste.args(["--no-newline", "--type", "image/png"]);
    let out = output(paste);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not available as requested type"), "{out:?}");
}

const TEXT_TYPES: &[&str] = &["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];

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

    let entries = env.export();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["content"], "hello from watch");
    assert_eq!(entries[0]["source"], "watch");
    assert_eq!(entries[0]["mime"], "text/plain;charset=utf-8");
    assert!(!hook.stderr.contains("hello from watch"), "content must not be logged: {}", hook.stderr);

    // No CLIPBOARD_STATE (older wl-paste) is treated as data
    let hook = run_hook(&env, None, b"second copy");
    assert!(hook.status.success(), "{}", hook.stderr);
    assert_eq!(env.export().len(), 2);
}

#[test]
fn hook_skips_sensitive_state_and_drains_stdin() {
    let env = TestEnv::new();
    let secret = vec![b's'; 1 << 20];
    let hook = run_hook(&env, Some("sensitive"), &secret);
    assert!(hook.status.success(), "{}", hook.stderr);
    assert!(hook.wrote_all, "the hook must read all of stdin so the source sees no EPIPE");
    assert!(hook.stderr.contains("[watch] skipped sensitive"), "{}", hook.stderr);
    assert!(env.export().is_empty());
}

#[test]
fn hook_skips_when_types_include_password_hint() {
    let env = TestEnv::new();
    env.set_clipboard("hunter2", &["text/plain;charset=utf-8", "x-kde-passwordManagerHint"]);
    let hook = run_hook(&env, Some("data"), b"hunter2");
    assert!(hook.status.success(), "{}", hook.stderr);
    assert!(hook.stderr.contains("[watch] skipped: marked sensitive"), "{}", hook.stderr);
    assert!(env.export().is_empty());
}

#[test]
fn hook_rejects_non_utf8() {
    let env = TestEnv::new();
    let hook = run_hook(&env, Some("data"), b"\xff\xfe binary");
    assert!(hook.status.success(), "{}", hook.stderr);
    assert!(hook.stderr.contains("not UTF-8"), "{}", hook.stderr);
    assert!(env.export().is_empty());
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
    assert!(env.export().is_empty());
}

#[test]
fn hook_caps_read_at_max_entry_bytes() {
    let env = TestEnv::new();
    env.write_config("[storage]\nmax_entry_bytes = 1024\n");
    env.set_clipboard("", TEXT_TYPES);
    let hook = run_hook(&env, Some("data"), &vec![b'a'; 4096]);
    assert!(hook.status.success(), "{}", hook.stderr);
    assert!(hook.wrote_all, "the rest of stdin is drained, not left unread");
    assert!(hook.stderr.contains("too large (4096 bytes"), "{}", hook.stderr);
    assert!(env.export().is_empty());

    // Exactly at the cap is saved
    let hook = run_hook(&env, Some("data"), &vec![b'b'; 1024]);
    assert!(hook.status.success(), "{}", hook.stderr);
    assert_eq!(env.export().len(), 1);
}

const HINT: &str = "x-kde-passwordManagerHint";

#[test]
fn daemon_watch_captures_copy() {
    let mut env = TestEnv::new();
    env.start_daemon();
    env.wait_for_watch_pid(Duration::from_secs(5)).expect("daemon should run wl-paste --watch");

    let calls = std::fs::read_to_string(env.fake_clip_dir.join("wl-paste.calls")).unwrap();
    let watch = calls.lines().find(|l| l.contains("--watch")).expect("a --watch call");
    let storage = env.storage_dir.display().to_string();
    assert!(watch.starts_with("--no-newline --watch "), "{watch}");
    assert!(watch.ends_with(&format!("--storage-dir {storage} __watch-hook")), "{watch}");

    env.set_clipboard("copied in a browser", TEXT_TYPES);
    env.watch_offer("data", b"copied in a browser");
    let entries = env.wait_for_entries(1);
    assert_eq!(entries[0]["content"], "copied in a browser");
    assert_eq!(entries[0]["source"], "watch");

    // A sensitive offer is not stored; a later one still is
    env.set_clipboard("hunter2", &["text/plain", HINT]);
    env.watch_offer("sensitive", b"hunter2");
    env.set_clipboard("after the secret", TEXT_TYPES);
    env.watch_offer("data", b"after the secret");
    let entries = env.wait_for_entries(2);
    let contents: Vec<_> = entries.iter().map(|e| e["content"].as_str().unwrap()).collect();
    assert_eq!(contents, ["copied in a browser", "after the secret"]);
}

#[test]
fn daemon_falls_back_to_poll_when_watch_unsupported() {
    let mut env = TestEnv::new();
    env.write_config("[capture]\npoll_interval_ms = 50\n");
    let mut cmd = env.cmd();
    cmd.arg("daemon").env("FAKE_WATCH_UNSUPPORTED", "1");
    let pid = env.spawn(cmd);
    assert!(env.wait_for_lock_pid(pid, Duration::from_secs(5)));

    env.set_clipboard("polled text", TEXT_TYPES);
    let entries = env.wait_for_entries(1);
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
    env.write_config("[capture]\nmode = \"poll\"\npoll_interval_ms = 50\n");
    env.start_daemon()
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

/// Wait until the daemon has listed the fake clipboard's types `n` more times.
fn wait_for_list_types(env: &TestEnv, n: usize) {
    let calls = env.fake_clip_dir.join("wl-paste.calls");
    let count = || {
        std::fs::read_to_string(&calls)
            .map(|s| s.lines().filter(|l| *l == "--list-types").count())
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
    assert!(env.export().is_empty(), "a password-manager secret was saved");

    // The secret's types were listed once, not on every poll (polls of the
    // empty clipboard before it was set list types too, to look for images)
    let list_calls = || {
        let calls = std::fs::read_to_string(env.fake_clip_dir.join("wl-paste.calls")).unwrap();
        calls.lines().filter(|l| *l == "--list-types").count()
    };
    let before = list_calls();
    wait_for_polls(&env, 3);
    assert_eq!(list_calls(), before, "types are listed on every poll");

    env.set_clipboard("ordinary", TEXT_TYPES);
    assert_eq!(env.wait_for_entries(1)[0]["content"], "ordinary");
}

#[test]
fn poll_passes_mime_types_to_ingest() {
    let mut env = TestEnv::new();
    start_polling_daemon(&mut env);
    env.set_clipboard("<b>bold</b>", &["image/png", "text/html", "text/plain;charset=utf-8"]);
    let entries = env.wait_for_entries(1);
    assert_eq!(entries[0]["mime"], "text/html");
    assert_eq!(entries[0]["source"], "poll");
}

// ======== images (5.2) ========

/// Put `png` on the fake clipboard offered as `types`.
fn set_clipboard_bytes(env: &TestEnv, png: &[u8], types: &[&str]) {
    std::fs::write(env.fake_clip_dir.join("content"), png).unwrap();
    std::fs::write(env.fake_clip_dir.join("types"), format!("{}\n", types.join("\n"))).unwrap();
}

fn decode_image(entry: &serde_json::Value) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(entry["image_base64"].as_str().unwrap()).unwrap()
}

#[test]
fn fake_watch_selection_follows_wl_paste_type_choice() {
    // wl-paste 2.3 with --type text never runs the command for an offer
    // with no text type; without --type it passes the first offered type
    let mut env = TestEnv::new();
    let log = env.fake_clip_dir.join("hook.log");
    let script = format!("printf '%s:' \"$CLIPBOARD_STATE\" >> '{0}'; wc -c >> '{0}'", log.display());
    let mut watch = env.fake_cmd("wl-paste");
    watch.args(["--type", "text", "--watch", "sh", "-c", &script]);
    let pid = env.spawn(watch);
    assert_eq!(env.wait_for_watch_pid(Duration::from_secs(5)), Some(pid));
    set_clipboard_bytes(&env, &common::test_png(2, 2, b""), &["image/png"]);
    env.watch_event("selection");
    let skips = env.fake_clip_dir.join("watch-skips.log");
    assert!(wait_until(Duration::from_secs(5), || skips.exists()), "a PNG-only offer is skipped");
    env.set_clipboard("abc", &["application/json"]);
    env.watch_event("selection");
    assert!(wait_until(Duration::from_secs(5), || std::fs::read_to_string(&log).is_ok_and(|s| s == "data:3\n")));
    assert_eq!(std::fs::read_to_string(&skips).unwrap().lines().count(), 1);
    env.signal(pid, "TERM");
    env.wait_for_exit(pid, Duration::from_secs(2)).expect("watcher exits");
}

#[test]
fn hook_saves_png_when_no_text_type_is_offered() {
    let env = TestEnv::new();
    let png = common::test_png(32, 16, b"screenshot");
    set_clipboard_bytes(&env, &png, &["image/png"]);
    // Without --type, wl-paste --watch passes a PNG-only offer as data
    let hook = run_hook(&env, Some("data"), &png);
    assert!(hook.status.success(), "{}", hook.stderr);
    assert!(hook.wrote_all);
    let entries = env.export();
    assert_eq!(entries.len(), 1, "{}", hook.stderr);
    assert_eq!(entries[0]["kind"], "image");
    assert_eq!(entries[0]["source"], "watch");
    assert_eq!((entries[0]["width"].as_u64(), entries[0]["height"].as_u64()), (Some(32), Some(16)));
    assert_eq!(decode_image(&entries[0]), png);
    assert!(hook.stderr.contains("saved image id:"), "{}", hook.stderr);

    // The same image again bumps it
    let hook = run_hook(&env, Some("data"), &png);
    assert!(hook.stderr.contains("bumped"), "{}", hook.stderr);
    assert_eq!(env.export().len(), 1);

    // A PNG larger than max_entry_bytes is still an image, not "too large" text
    env.write_config("[storage]\nmax_entry_bytes = 16\n");
    let big = common::test_png(64, 64, &[b'x'; 100]);
    set_clipboard_bytes(&env, &big, &["image/png"]);
    let hook = run_hook(&env, Some("data"), &big);
    assert!(hook.wrote_all);
    assert!(hook.stderr.contains("saved image id:"), "{}", hook.stderr);
    assert_eq!(env.export().len(), 2);
}

#[test]
fn hook_prefers_text_and_skips_sensitive_or_bad_images() {
    let env = TestEnv::new();
    let png = common::test_png(4, 4, b"");
    // Both offered: wl-paste passes the text, which is saved
    set_clipboard_bytes(&env, &png, &["image/png", "text/plain;charset=utf-8"]);
    let hook = run_hook(&env, Some("data"), b"caption text");
    assert!(hook.status.success(), "{}", hook.stderr);
    // Plain text among other text types wins too
    set_clipboard_bytes(&env, &png, &["text/html", "image/png", "UTF8_STRING"]);
    let hook = run_hook(&env, Some("data"), b"cell value");
    assert!(hook.status.success(), "{}", hook.stderr);
    // nil is an empty clipboard: nothing to save
    set_clipboard_bytes(&env, &png, &["image/png"]);
    let hook = run_hook(&env, Some("nil"), b"");
    assert!(hook.status.success(), "{}", hook.stderr);
    let entries = env.export();
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert!(entries.iter().all(|e| e["kind"] == "text"), "{entries:?}");
    assert_eq!(entries[0]["content"], "caption text");
    assert_eq!(entries[1]["content"], "cell value");

    set_clipboard_bytes(&env, &png, &["image/png", HINT]);
    let hook = run_hook(&env, Some("data"), &png);
    assert!(hook.stderr.contains("skipped: marked sensitive"), "{}", hook.stderr);

    set_clipboard_bytes(&env, b"GIF89a, not a png", &["image/png"]);
    let hook = run_hook(&env, Some("data"), b"GIF89a, not a png");
    assert!(hook.stderr.contains("skipped: not a PNG"), "{}", hook.stderr);

    // A PNG over max_image_bytes is not read past the cap
    env.write_config("[storage]\nmax_image_bytes = 10\n");
    set_clipboard_bytes(&env, &png, &["image/png"]);
    let hook = run_hook(&env, Some("data"), &png);
    assert!(hook.stderr.contains("[watch] skipped: image too large (> 10 bytes)"), "{}", hook.stderr);

    // Other image types are not stored, nor is an explicit clear
    std::fs::remove_file(env.config_home.join("clipstack/config.toml")).unwrap();
    set_clipboard_bytes(&env, &png, &["image/jpeg"]);
    let hook = run_hook(&env, Some("data"), &png);
    assert!(hook.wrote_all);
    set_clipboard_bytes(&env, &png, &["image/png"]);
    run_hook(&env, Some("clear"), b"");
    assert_eq!(env.export().len(), 2);

    // Nor past max_total_bytes when that is smaller (which prunes the history)
    env.write_config("[storage]\nmax_entry_bytes = 8\nmax_total_bytes = 20\n");
    let hook = run_hook(&env, Some("data"), &png);
    assert!(hook.stderr.contains("[watch] skipped: image too large (> 20 bytes)"), "{}", hook.stderr);
}

#[test]
fn hook_saves_png_offered_with_only_incidental_text() {
    // A browser's "Copy image": wl-paste passes the <img> HTML on stdin, but
    // without a plain-text type the PNG is what gets saved
    let env = TestEnv::new();
    let png = common::test_png(12, 9, b"browser");
    let html = b"<meta charset='utf-8'><img src=\"https://example.com/cat.png\"/>";
    for offer in [
        &["text/html", "image/png"][..],
        &["text/x-moz-url", "text/_moz_htmlcontext", "text/html", "image/png", "image/jpeg"][..],
        &["image/png", "text/uri-list", "application/json"][..],
    ] {
        set_clipboard_bytes(&env, &png, offer);
        let hook = run_hook(&env, Some("data"), html);
        assert!(hook.status.success(), "{}", hook.stderr);
        assert!(hook.wrote_all);
    }
    let entries = env.export();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["kind"], "image");
    assert_eq!(decode_image(&entries[0]), png);
}

#[test]
fn daemon_watch_captures_image() {
    let mut env = TestEnv::new();
    env.start_daemon();
    env.wait_for_watch_pid(Duration::from_secs(5)).expect("daemon should run wl-paste --watch");
    let png = common::test_png(8, 6, b"watched");
    set_clipboard_bytes(&env, &png, &["image/png"]);
    env.watch_event("selection");
    let entries = env.wait_for_entries(1);
    assert_eq!(entries[0]["kind"], "image");
    assert_eq!(decode_image(&entries[0]), png);

    // Text offers still arrive as text
    env.set_clipboard("text after the image", TEXT_TYPES);
    env.watch_event("selection");
    let entries = env.wait_for_entries(2);
    assert_eq!(entries[1]["content"], "text after the image");
}

#[test]
fn poll_captures_image_once() {
    let mut env = TestEnv::new();
    start_polling_daemon(&mut env);
    let png = common::test_png(5, 5, b"polled");
    set_clipboard_bytes(&env, &png, &["image/png"]);
    let entries = env.wait_for_entries(1);
    assert_eq!(entries[0]["kind"], "image");
    assert_eq!(entries[0]["source"], "poll");
    assert_eq!(decode_image(&entries[0]), png);

    // Later polls see the same image and leave the entry alone
    wait_for_polls(&env, 3);
    let again = env.export();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0]["last_used"], entries[0]["last_used"], "not bumped on every poll");

    // Text wins when both are offered
    set_clipboard_bytes(&env, b"poll text", &["text/plain", "image/png"]);
    let entries = env.wait_for_entries(2);
    assert_eq!(entries[1]["kind"], "text");
    assert_eq!(entries[1]["content"], "poll text");
}

#[test]
fn poll_skips_image_over_the_cap_and_says_so_once() {
    let mut env = TestEnv::new();
    env.write_config("[storage]\nmax_image_bytes = 64\n[capture]\nmode = \"poll\"\npoll_interval_ms = 50\n");
    let pid = env.start_daemon();
    set_clipboard_bytes(&env, &common::test_png(9, 9, &[b'x'; 200]), &["image/png"]);
    wait_for_list_types(&env, 5);
    assert!(env.export().is_empty());
    env.signal(pid, "TERM");
    let done = env.wait_for_exit(pid, Duration::from_secs(2)).expect("daemon exits");
    let line = "[poll] skipped: image too large (> 64 bytes)";
    assert_eq!(done.stderr.matches(line).count(), 1, "{}", done.stderr);
}

#[test]
fn poll_saves_png_offered_with_only_incidental_text() {
    let mut env = TestEnv::new();
    start_polling_daemon(&mut env);
    // As text the offer reads as the <img> fragment
    let png = common::test_png(7, 3, b"browser");
    // (types before content: until `content` exists there is no selection)
    std::fs::write(env.fake_clip_dir.join("content.text"), "<img src=\"cat.png\">").unwrap();
    std::fs::write(env.fake_clip_dir.join("types"), "text/html\nimage/png\n").unwrap();
    std::fs::write(env.fake_clip_dir.join("content"), &png).unwrap();
    let entries = env.wait_for_entries(1);
    assert_eq!(entries[0]["kind"], "image", "{entries:?}");
    assert_eq!(decode_image(&entries[0]), png);
    wait_for_list_types(&env, 3);
    assert_eq!(env.export().len(), 1);
}

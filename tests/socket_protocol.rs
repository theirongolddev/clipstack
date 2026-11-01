//! The daemon's Unix socket: PUSH, PULL and PING over protocol v1. Every
//! daemon runs through the isolated `TestEnv` (temp runtime dir, fake
//! wl-clipboard).

mod common;

#[allow(dead_code)] // the server half isn't used here
#[path = "../src/protocol.rs"]
mod protocol;

use common::{TestEnv, wait_until};
use protocol::{ErrCode, Request, Response};
use std::io::BufReader;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::time::Duration;

const HINT: &str = "x-kde-passwordManagerHint";

/// Start a daemon (with `config`, if any) and wait for its socket.
fn start(env: &mut TestEnv, config: &str) {
    if !config.is_empty() {
        env.write_config(config);
    }
    env.start_daemon();
    let sock = env.socket_path();
    assert!(
        wait_until(Duration::from_secs(5), || UnixStream::connect(&sock).is_ok()),
        "daemon socket never accepted at {}",
        sock.display()
    );
}

fn connect(env: &TestEnv) -> UnixStream {
    let stream = UnixStream::connect(env.socket_path()).expect("connect to daemon socket");
    stream.set_read_timeout(Some(Duration::from_secs(10))).expect("set read timeout");
    stream.set_write_timeout(Some(Duration::from_secs(10))).expect("set write timeout");
    stream
}

fn request(env: &TestEnv, req: &Request) -> Response {
    let mut stream = connect(env);
    protocol::write_request(&mut stream, req).expect("send request");
    protocol::read_response(&mut BufReader::new(stream), protocol::MAX_BODY).expect("read response")
}

fn assert_err(resp: &Response, code: ErrCode) -> &str {
    match resp {
        Response::Err { code: got, message } if *got == code => message,
        other => panic!("expected ERR {code}, got {other:?}"),
    }
}

#[test]
fn socket_is_0600_in_runtime_dir() {
    let mut env = TestEnv::new();
    start(&mut env, "");
    let meta = std::fs::symlink_metadata(env.socket_path()).expect("socket exists");
    assert!(meta.file_type().is_socket());
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert!(env.socket_path().starts_with(env.root()));
}

#[test]
fn push_ingests_and_copies() {
    let mut env = TestEnv::new();
    start(&mut env, "");
    let resp = request(&env, &Request::Push(b"from the remote\nhost".to_vec()));
    assert_eq!(resp.meta("stored"), Some("1"), "{resp:?}");
    let id = resp.meta("id").expect("id= in response");

    let entries = env.wait_for_entries(1);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["content"], "from the remote\nhost");
    assert_eq!(entries[0]["source"], "remote");
    assert_eq!(entries[0]["id"].to_string(), id);
    assert_eq!(env.clipboard_content().as_deref(), Some("from the remote\nhost"));

    // Not UTF-8: refused, nothing copied
    let resp = request(&env, &Request::Push(vec![0xff, 0xfe]));
    assert_err(&resp, ErrCode::NotUtf8);
    assert_eq!(env.clipboard_content().as_deref(), Some("from the remote\nhost"));
}

#[test]
fn push_ignored_pattern_copies_but_not_stored() {
    let mut env = TestEnv::new();
    start(&mut env, "[storage]\nignore_patterns = [\"^otp:\"]\n");
    let resp = request(&env, &Request::Push(b"otp: 123456".to_vec()));
    assert_eq!(resp.meta("stored"), Some("0"), "{resp:?}");
    assert_eq!(resp.meta("reason"), Some("ignored"));
    assert_eq!(resp.meta("id"), None);
    // The user sent it explicitly, so it is still copied
    assert_eq!(env.clipboard_content().as_deref(), Some("otp: 123456"));
    assert!(env.export().is_empty());

    let resp = request(&env, &Request::Push(Vec::new()));
    assert_eq!(resp.meta("reason"), Some("empty"), "{resp:?}");
}

#[test]
fn pull_forbidden_by_default() {
    let mut env = TestEnv::new();
    start(&mut env, "");
    env.set_clipboard("local text", &["text/plain"]);
    let resp = request(&env, &Request::Pull);
    assert!(assert_err(&resp, ErrCode::Forbidden).contains("socket.allow_pull"), "{resp:?}");
}

#[test]
fn pull_returns_clipboard_when_allowed() {
    let mut env = TestEnv::new();
    start(&mut env, "[socket]\nallow_pull = true\n");
    env.set_clipboard("local\ntext\n", &["text/plain;charset=utf-8", "text/plain"]);
    let resp = request(&env, &Request::Pull);
    assert_eq!(resp, Response::ok(b"local\ntext\n".to_vec()));
}

#[test]
fn pull_refuses_sensitive_clipboard() {
    let mut env = TestEnv::new();
    start(&mut env, "[socket]\nallow_pull = true\n");
    env.set_clipboard("hunter2", &["text/plain", HINT]);
    let resp = request(&env, &Request::Pull);
    assert_err(&resp, ErrCode::Sensitive);
}

#[test]
fn ping_reports_capture_and_version() {
    let mut env = TestEnv::new();
    start(&mut env, "");
    env.wait_for_watch_pid(Duration::from_secs(5)).expect("daemon runs wl-paste --watch");
    let resp = request(&env, &Request::Ping);
    assert_eq!(resp.meta("version"), Some(env!("CARGO_PKG_VERSION")), "{resp:?}");
    assert_eq!(resp.meta("capture"), Some("watch"));
    assert_eq!(resp.meta("pull"), Some("0"));

    let mut env = TestEnv::new();
    start(&mut env, "[capture]\nmode = \"poll\"\n[socket]\nallow_pull = true\n");
    let resp = request(&env, &Request::Ping);
    assert_eq!(resp.meta("capture"), Some("poll"), "{resp:?}");
    assert_eq!(resp.meta("pull"), Some("1"));
}

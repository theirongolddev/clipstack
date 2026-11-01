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
use std::time::{Duration, Instant};

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

/// Read whatever the server sends until it closes the connection.
fn read_all(stream: &mut UnixStream) -> String {
    let mut out = Vec::new();
    std::io::Read::read_to_end(stream, &mut out).expect("read until close");
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn idle_client_gets_timeout() {
    let mut env = TestEnv::new();
    start(&mut env, "[socket]\ntimeout_ms = 200\n");
    let mut idle = connect(&env);
    let started = Instant::now();
    let reply = read_all(&mut idle);
    assert!(reply.starts_with("ERR timeout "), "{reply:?}");
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(150) && took < Duration::from_secs(2), "{took:?}");
}

#[test]
fn slow_client_hits_overall_deadline() {
    // One byte every 100 ms never trips the 200 ms read timeout, but the
    // whole request must arrive within 2 x timeout_ms
    let mut env = TestEnv::new();
    start(&mut env, "[socket]\ntimeout_ms = 200\n");
    let mut slow = connect(&env);
    let started = Instant::now();
    for b in b"CLIPSTACK/1 PUSH 5\nhel" {
        if std::io::Write::write_all(&mut slow, &[*b]).is_err() {
            break; // the server already gave up
        }
        std::thread::sleep(Duration::from_millis(100));
        if started.elapsed() > Duration::from_secs(2) {
            break;
        }
    }
    let reply = read_all(&mut slow);
    assert!(reply.starts_with("ERR timeout "), "{reply:?}");
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
}

#[test]
fn second_client_served_after_first_times_out() {
    let mut env = TestEnv::new();
    start(&mut env, "[socket]\ntimeout_ms = 300\n");
    let mut idle = connect(&env);
    // Give the server a moment to pick up the idle connection first
    std::thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    let resp = request(&env, &Request::Ping);
    assert!(resp.meta("version").is_some(), "{resp:?}");
    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    assert!(read_all(&mut idle).starts_with("ERR timeout "));
}

#[test]
fn oversize_push_rejected_without_reading_body() {
    let mut env = TestEnv::new();
    start(&mut env, "[storage]\nmax_entry_bytes = 1024\nmax_total_bytes = 1048576\n");
    let mut stream = connect(&env);
    // Header only: the server must answer without waiting for 2000 bytes
    std::io::Write::write_all(&mut stream, b"CLIPSTACK/1 PUSH 2000\n").unwrap();
    let started = Instant::now();
    let reply = read_all(&mut stream);
    assert!(reply.starts_with("ERR too_large "), "{reply:?}");
    assert!(reply.contains("2000") && reply.contains("1024"), "{reply:?}");
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    assert!(env.export().is_empty());

    // At the limit is fine
    let resp = request(&env, &Request::Push(vec![b'x'; 1024]));
    assert_eq!(resp.meta("stored"), Some("1"), "{resp:?}");
}

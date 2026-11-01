//! Scripting interface: exit codes, `get`, `list --json`, `search`, `alias`.
//! All run through the isolated `TestEnv`.

mod common;

use common::TestEnv;
use std::io::Write;
use std::process::{Output, Stdio};

/// Run `clipstack <args>` to completion, feeding `stdin` if given.
fn run(env: &TestEnv, args: &[&str], stdin: Option<&str>) -> Output {
    let mut cmd = env.cmd();
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().expect("spawn clipstack");
    if let Some(input) = stdin {
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    }
    child.wait_with_output().expect("wait for clipstack")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Exit code, with stdout/stderr in the failure message.
fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or_else(|| panic!("killed by a signal: {out:?}"))
}

#[test]
fn newer_schema_exits_4() {
    let env = TestEnv::new();
    assert_eq!(code(&run(&env, &["stats"], None)), 0);
    let db = env.storage_dir.join("clipstack.db");
    rusqlite::Connection::open(&db).unwrap().pragma_update(None, "user_version", 99).unwrap();

    let out = run(&env, &["stats"], None);
    assert_eq!(code(&out), 4, "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("newer"), "stderr: {}", stderr(&out));
}

#[test]
fn bad_config_exits_5() {
    let env = TestEnv::new();
    env.write_config("[storage]\nno_such_key = 1\n");
    let out = run(&env, &["stats"], None);
    assert_eq!(code(&out), 5, "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("config error"), "stderr: {}", stderr(&out));

    let missing = env.root().join("nope.toml");
    let out = run(&env, &["--config", missing.to_str().unwrap(), "stats"], None);
    assert_eq!(code(&out), 5, "stderr: {}", stderr(&out));
}

#[test]
fn damaged_index_exits_4() {
    let env = TestEnv::new();
    run(&env, &["copy"], Some("some text"));
    let db = env.storage_dir.join("clipstack.db");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute("INSERT INTO entries_fts(entries_fts) VALUES ('delete-all')", [])
        .unwrap();
    let out = run(&env, &["check"], None);
    assert_eq!(code(&out), 4, "stdout: {}\nstderr: {}", stdout(&out), stderr(&out));
}

#[test]
fn clipboard_tool_failure_exits_6() {
    let env = TestEnv::new();
    // FAKE_CLIP_DIR is a file, so the fake wl-copy fails
    std::fs::remove_dir(&env.fake_clip_dir).unwrap();
    std::fs::write(&env.fake_clip_dir, "not a dir").unwrap();
    let out = run(&env, &["copy"], Some("text"));
    assert_eq!(code(&out), 6, "stderr: {}", stderr(&out));
}

#[test]
fn usage_error_exits_2() {
    let env = TestEnv::new();
    let out = run(&env, &["list", "--no-such-flag"], None);
    assert_eq!(code(&out), 2, "stderr: {}", stderr(&out));
}

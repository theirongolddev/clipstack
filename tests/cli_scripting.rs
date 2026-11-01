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

// ======== get (3-4) ========

/// Save each text through `copy` (fake wl-copy), oldest first.
fn copy_all(env: &TestEnv, texts: &[&str]) {
    for text in texts {
        let out = run(env, &["copy"], Some(text));
        assert_eq!(code(&out), 0, "copy {text:?}: {}", stderr(&out));
    }
}

/// Entry id of `content`, from `export`.
fn id_of(env: &TestEnv, content: &str) -> i64 {
    env.export()
        .iter()
        .find(|e| e["content"] == content)
        .and_then(|e| e["id"].as_i64())
        .unwrap_or_else(|| panic!("no entry {content:?}"))
}

/// Pin `id` with `alias` directly in the database.
fn set_alias_in_db(env: &TestEnv, id: i64, alias: &str) {
    rusqlite::Connection::open(env.storage_dir.join("clipstack.db"))
        .unwrap()
        .execute("UPDATE entries SET pinned = 1, alias = ?2 WHERE id = ?1", rusqlite::params![id, alias])
        .unwrap();
}

#[test]
fn get_1_prints_newest_without_newline() {
    let env = TestEnv::new();
    copy_all(&env, &["older", "newest\nline two"]);
    let out = run(&env, &["get", "1"], None);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "newest\nline two");
    assert_eq!(stdout(&run(&env, &["get", "2"], None)), "older");
}

#[test]
fn get_by_id() {
    let env = TestEnv::new();
    copy_all(&env, &["first", "second", "third"]);
    let id = id_of(&env, "first");
    let out = run(&env, &["get", &format!("id:{id}")], None);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "first");
}

#[test]
fn get_by_alias() {
    let env = TestEnv::new();
    copy_all(&env, &["-- \nBest regards,\nMe", "something newer"]);
    set_alias_in_db(&env, id_of(&env, "-- \nBest regards,\nMe"), "sig");
    let out = run(&env, &["get", "@sig"], None);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "-- \nBest regards,\nMe");
}

#[test]
fn get_missing_exits_1() {
    let env = TestEnv::new();
    copy_all(&env, &["only"]);
    for reference in ["2", "id:999", "@nope"] {
        let out = run(&env, &["get", reference], None);
        assert_eq!(code(&out), 1, "{reference}: {}", stderr(&out));
        assert!(stdout(&out).is_empty());
        assert!(stderr(&out).contains(&format!("no entry for {reference}")), "{}", stderr(&out));
    }
    // A malformed reference is a usage error
    for reference in ["0", "#1", "@Bad"] {
        let out = run(&env, &["get", reference], None);
        assert_eq!(code(&out), 2, "{reference}: {}", stderr(&out));
    }
}

#[test]
fn get_copy_puts_content_on_fake_clipboard() {
    let env = TestEnv::new();
    copy_all(&env, &["pick me", "newer"]);
    let id = id_of(&env, "pick me");
    // No daemon runs: get --copy bumps the entry itself
    let out = run(&env, &["get", "2", "--copy"], None);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "pick me");
    assert_eq!(env.clipboard_content().as_deref(), Some("pick me"));
    assert_eq!(stdout(&run(&env, &["get", "1"], None)), "pick me", "bumped to newest");
    assert_eq!(id_of(&env, "pick me"), id, "same entry, not a new one");
    assert_eq!(env.export().len(), 2);
}

//! CLI tests for the SQLite storage: legacy import, policy skips, status.
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

fn ok(env: &TestEnv, args: &[&str], stdin: Option<&str>) -> (String, String) {
    let out = run(env, args, stdin);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{args:?} failed: {:?}\nstdout: {stdout}\nstderr: {stderr}", out.status);
    (stdout, stderr)
}

/// An old-format store: index.json plus `<id>.txt` files.
fn write_legacy(env: &TestEnv, entries: &[(&str, i64, bool, &str)]) {
    std::fs::create_dir_all(&env.storage_dir).unwrap();
    let index: Vec<serde_json::Value> = entries
        .iter()
        .map(|(id, ts, pinned, content)| {
            serde_json::json!({ "id": id, "timestamp": ts, "size": content.len(),
                                "preview": content, "hash": "x", "pinned": pinned })
        })
        .collect();
    let index = serde_json::json!({ "max_entries": 100, "entries": index });
    std::fs::write(env.storage_dir.join("index.json"), index.to_string()).unwrap();
    for (id, _, _, content) in entries {
        std::fs::write(env.storage_dir.join(format!("{id}.txt")), content).unwrap();
    }
}

fn write_config(env: &TestEnv, toml: &str) {
    let dir = env.config_home.join("clipstack");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), toml).unwrap();
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[test]
fn stats_after_import_matches_legacy_count() {
    let env = TestEnv::new();
    let now = now_ms();
    write_legacy(
        &env,
        &[
            (&format!("{}", now - 1_000), now - 1_000, false, "newest entry"),
            (&format!("{}_001", now - 2_000), now - 2_000, true, "pinned entry"),
            (&format!("{}", now - 3_000), now - 3_000, false, "oldest entry"),
        ],
    );

    let (stdout, stderr) = ok(&env, &["stats"], None);
    assert!(stderr.contains("Imported 3 entries from index.json"), "stderr: {stderr}");
    assert!(stderr.contains("legacy-backup-"), "stderr: {stderr}");
    assert!(stdout.contains("Entries:     3"), "stdout: {stdout}");
    assert!(stdout.contains("Pinned:    1"), "stdout: {stdout}");
    assert!(stdout.contains("Regular:   2/100 (saved)"), "stdout: {stdout}");

    let (stdout, stderr) = ok(&env, &["list"], None);
    assert!(!stderr.contains("Imported"), "the notice is one-time: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "stdout: {stdout}");
    assert!(lines[0].ends_with("newest entry") && lines[2].ends_with("oldest entry"), "{stdout}");
}

#[test]
fn copy_skipped_by_policy_still_copies_and_reports() {
    let env = TestEnv::new();
    write_config(&env, "[storage]\nignore_patterns = [\"^secret\"]\n");

    let (_, stderr) = ok(&env, &["copy"], Some("secret value"));
    let copied = std::fs::read_to_string(env.fake_clip_dir.join("content")).unwrap();
    assert_eq!(copied, "secret value", "still copied to the clipboard");
    assert!(stderr.contains("not saved to history"), "stderr: {stderr}");
    assert!(stderr.contains("ignore_patterns[0]"), "stderr: {stderr}");
    assert!(!stderr.contains("secret value"), "skip messages never echo content");

    let (_, stderr) = ok(&env, &["copy"], Some("ordinary text"));
    assert!(!stderr.contains("not saved"), "stderr: {stderr}");
    let (stdout, _) = ok(&env, &["stats"], None);
    assert!(stdout.contains("Entries:     1"), "stdout: {stdout}");
}

#[test]
fn import_notice_warns_before_retention_expires() {
    let env = TestEnv::new();
    write_config(&env, "[storage]\nretention_days = 7\n");
    let now = now_ms();
    write_legacy(
        &env,
        &[
            (&format!("{now}"), now, false, "recent"),
            ("1000", 1_000, false, "from 1970"),
            ("2000", 2_000, true, "pinned 1970"),
        ],
    );

    let (stdout, stderr) = ok(&env, &["stats"], None);
    assert!(
        stderr.contains("1 entries are older than 7 days and will expire at the next start"),
        "stderr: {stderr}"
    );
    assert!(stdout.contains("Entries:     3"), "nothing expires at the importing open: {stdout}");

    let (stdout, _) = ok(&env, &["stats"], None);
    assert!(stdout.contains("Entries:     2"), "expired at the next start: {stdout}");
}

#[test]
fn missing_explicit_config_is_an_error() {
    let env = TestEnv::new();
    let missing = env.root().join("nope.toml");
    let out = run(&env, &["--config", missing.to_str().unwrap(), "stats"], None);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("config error"), "stderr: {stderr}");
}

#[test]
fn bad_config_is_reported_before_opening_storage() {
    let env = TestEnv::new();
    write_config(&env, "[storage]\nignore_patterns = [\"(\"]\n");
    let out = run(&env, &["stats"], None);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("ignore_patterns[0]"), "stderr: {stderr}");
    assert!(!env.storage_dir.join("clipstack.db").exists(), "storage not opened");
}

#[test]
fn status_shows_database_config_and_backups() {
    let env = TestEnv::new();
    write_config(&env, "[storage]\nretention_days = 30\nignore_patterns = [\"a\", \"b\"]\n");
    write_legacy(&env, &[(&format!("{}", now_ms()), now_ms(), false, "x")]);

    let (stdout, _) = ok(&env, &["status"], None);
    let db = env.storage_dir.join("clipstack.db");
    assert!(stdout.contains(&db.display().to_string()), "stdout: {stdout}");
    assert!(stdout.contains("Retention:   30 days"), "stdout: {stdout}");
    assert!(stdout.contains("2 patterns + built-in secret patterns"), "stdout: {stdout}");
    let config = env.config_home.join("clipstack").join("config.toml");
    assert!(stdout.contains(&config.display().to_string()), "stdout: {stdout}");
    assert!(stdout.contains("legacy-backup-"), "status warns about the backup: {stdout}");
}

#[test]
fn clear_reports_count() {
    let env = TestEnv::new();
    ok(&env, &["copy"], Some("one"));
    ok(&env, &["copy"], Some("two"));
    let (stdout, _) = ok(&env, &["clear"], None);
    assert!(stdout.contains("Clipboard history cleared (2 entries)"), "stdout: {stdout}");
}

/// The `content` field of each exported JSONL line.
fn exported_contents(jsonl: &str) -> Vec<String> {
    jsonl
        .lines()
        .map(|l| {
            let row: serde_json::Value = serde_json::from_str(l).expect("one JSON object per line");
            row["content"].as_str().unwrap().to_string()
        })
        .collect()
}

fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn export_jsonl_roundtrips_content() {
    let env = TestEnv::new();
    let contents = ["plain", "multi\nline\n", "quotes \" and \\ backslash", "unicode \u{e9}\u{1f600}"];
    for c in contents {
        ok(&env, &["copy"], Some(c));
    }
    let (stdout, _) = ok(&env, &["export"], None);
    assert_eq!(exported_contents(&stdout), contents, "oldest first, byte-exact");
}

#[test]
fn export_file_created_0600() {
    let env = TestEnv::new();
    ok(&env, &["copy"], Some("secret-ish"));
    let path = env.root().join("backup.jsonl");
    let (stdout, stderr) = ok(&env, &["export", "-o", path.to_str().unwrap()], None);
    assert!(stdout.is_empty(), "nothing on stdout with -o: {stdout}");
    assert!(stderr.contains("Exported 1 entries"), "stderr: {stderr}");
    assert_eq!(mode(&path), 0o600);
    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(exported_contents(&written), ["secret-ish"]);
}

#[test]
fn export_refuses_overwrite_without_force() {
    use std::os::unix::fs::PermissionsExt;
    let env = TestEnv::new();
    ok(&env, &["copy"], Some("fresh"));
    let path = env.root().join("existing.jsonl");
    std::fs::write(&path, "keep me").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let out = run(&env, &["export", "-o", path.to_str().unwrap()], None);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--force"), "stderr: {stderr}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");

    ok(&env, &["export", "-o", path.to_str().unwrap(), "--force"], None);
    assert_eq!(exported_contents(&std::fs::read_to_string(&path).unwrap()), ["fresh"]);
    assert_eq!(mode(&path), 0o600, "an overwritten file is tightened too");
}


#[test]
fn export_to_closed_pipe_exits_quietly() {
    let env = TestEnv::new();
    // Bigger than a pipe buffer, so the write fails with EPIPE
    let big = "x".repeat(256 * 1024);
    ok(&env, &["copy"], Some(&big));

    let mut child = env
        .cmd()
        .arg("export")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn clipstack");
    drop(child.stdout.take()); // like `clipstack export | head -0`
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "status {:?}, stderr: {stderr}", out.status);
    assert!(stderr.is_empty(), "stderr: {stderr}");
}

/// Wipe the FTS index of the test store directly, leaving the entries.
fn damage_fts(env: &TestEnv) {
    let conn = rusqlite::Connection::open(env.storage_dir.join("clipstack.db")).unwrap();
    conn.execute("INSERT INTO entries_fts(entries_fts) VALUES ('delete-all')", []).unwrap();
}

#[test]
fn check_reports_ok_counts_and_backups() {
    let env = TestEnv::new();
    write_legacy(&env, &[(&format!("{}", now_ms()), now_ms(), false, "imported")]);
    let (stdout, _) = ok(&env, &["check"], None);
    assert!(stdout.contains("Integrity:    ok"), "stdout: {stdout}");
    assert!(stdout.contains("Search index: ok"), "stdout: {stdout}");
    assert!(stdout.contains("Entries:      1"), "stdout: {stdout}");
    assert!(stdout.contains("legacy-backup-"), "stdout: {stdout}");
}

#[test]
fn check_fails_on_damaged_index_until_repaired() {
    let env = TestEnv::new();
    ok(&env, &["copy"], Some("findable text"));
    damage_fts(&env);

    let out = run(&env, &["check"], None);
    assert!(!out.status.success(), "a damaged index fails the check");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("check --repair"), "stderr: {stderr}");

    let (stdout, _) = ok(&env, &["check", "--repair"], None);
    assert!(stdout.contains("Search index: rebuilt"), "stdout: {stdout}");
    let (stdout, _) = ok(&env, &["check"], None);
    assert!(stdout.contains("Search index: ok"), "stdout: {stdout}");
}

#[test]
fn recover_alias_prints_recovery_complete() {
    let env = TestEnv::new();
    ok(&env, &["copy"], Some("one"));
    ok(&env, &["copy"], Some("two"));
    damage_fts(&env);
    let (stdout, _) = ok(&env, &["recover"], None);
    assert!(stdout.contains("Recovery complete. 2 entries."), "stdout: {stdout}");
    let (stdout, _) = ok(&env, &["check"], None);
    assert!(stdout.contains("Search index: ok"), "recover repaired it: {stdout}");

    let (help, _) = ok(&env, &["--help"], None);
    assert!(!help.contains("recover"), "recover is hidden: {help}");
    assert!(help.contains("check"), "help: {help}");
}

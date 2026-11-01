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
fn damaged_image_row_fails_check_with_exit_4() {
    let env = TestEnv::new();
    run(&env, &["copy"], Some("some text"));
    let db = env.storage_dir.join("clipstack.db");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute(
            "INSERT INTO entries (content, hash, size, preview, created_at, last_used, mime, kind, width, height, image)
             VALUES ('', randomblob(32), 4, 'Image', 1, 1, 'image/png', 'image', 1, 1, x'00010203')",
            [],
        )
        .unwrap();
    let out = run(&env, &["check"], None);
    assert_eq!(code(&out), 4, "stdout: {}\nstderr: {}", stdout(&out), stderr(&out));
    assert!(stdout(&out).contains("Images:"), "{}", stdout(&out));
    assert!(stdout(&out).contains("not a PNG"), "{}", stdout(&out));
    assert!(stderr(&out).contains("clipstack delete id:"), "{}", stderr(&out));
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

// ======== list --json (3-5) ========

fn json(out: &Output) -> serde_json::Value {
    assert_eq!(code(out), 0, "stderr: {}", stderr(out));
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {}", stdout(out)))
}

#[test]
fn list_json_fields_and_positions() {
    let env = TestEnv::new();
    copy_all(&env, &["first", "second\twith tab", "third"]);
    set_alias_in_db(&env, id_of(&env, "first"), "one");

    let list = json(&run(&env, &["list", "--json"], None));
    let list = list.as_array().expect("an array");
    assert_eq!(list.len(), 3);
    let previews: Vec<_> = list.iter().map(|e| e["preview"].as_str().unwrap()).collect();
    assert_eq!(previews, ["third", "second with tab", "first"], "newest first");
    for (i, e) in list.iter().enumerate() {
        assert_eq!(e["position"], i + 1);
        let keys: Vec<_> = e.as_object().unwrap().keys().map(String::as_str).collect();
        for key in ["position", "id", "created_at", "last_used", "size", "preview", "pinned", "alias", "source", "mime"] {
            assert!(keys.contains(&key), "{key} missing from {e}");
        }
        assert!(e.get("content").is_none(), "list does not include content");
        assert!(e["last_used"].as_i64().unwrap() > 1_600_000_000_000, "unix ms: {e}");
        assert_eq!(e["source"], "copy");
    }
    assert_eq!(list[2]["pinned"], true);
    assert_eq!(list[2]["alias"], "one");
    assert_eq!(list[0]["alias"], serde_json::Value::Null);
    assert_eq!(list[1]["size"], "second\twith tab".len());
    // position N is what `get N` prints
    assert_eq!(stdout(&run(&env, &["get", "2"], None)), "second\twith tab");
}

#[test]
fn list_json_respects_count() {
    let env = TestEnv::new();
    copy_all(&env, &["a1", "a2", "a3", "a4"]);
    let list = json(&run(&env, &["list", "--json", "-c", "2"], None));
    let previews: Vec<_> = list.as_array().unwrap().iter().map(|e| e["preview"].clone()).collect();
    assert_eq!(previews, ["a4", "a3"]);
    // No "... and N more" line in JSON mode
    assert!(!stdout(&run(&env, &["list", "--json", "-c", "2"], None)).contains("more"));
}

#[test]
fn list_json_empty_array() {
    let env = TestEnv::new();
    let out = run(&env, &["list", "--json"], None);
    assert_eq!(json(&out), serde_json::json!([]));
    assert_eq!(stdout(&out).trim_end(), "[]");
}

#[test]
fn list_pinned_keeps_recency_positions() {
    let env = TestEnv::new();
    copy_all(&env, &["pinned old", "plain", "pinned new", "newest"]);
    set_alias_in_db(&env, id_of(&env, "pinned old"), "old");
    set_alias_in_db(&env, id_of(&env, "pinned new"), "new");

    let list = json(&run(&env, &["list", "--pinned", "--json"], None));
    let got: Vec<_> = list.as_array().unwrap().iter().map(|e| (e["position"].clone(), e["preview"].clone())).collect();
    assert_eq!(got, [(2.into(), "pinned new".into()), (4.into(), "pinned old".into())]);

    let human = stdout(&run(&env, &["list", "--pinned"], None));
    assert!(human.contains("pinned new") && human.contains("pinned old"), "{human}");
    assert!(!human.contains("plain") && !human.contains("newest"), "{human}");
}

// ======== search (3-6) ========

#[test]
fn search_finds_deep_match() {
    let env = TestEnv::new();
    let deep = format!("{}needle-in-the-haystack at the end", "filler ".repeat(100));
    copy_all(&env, &[&deep, "unrelated", "another needle-in-the-haystack, short"]);

    let out = run(&env, &["search", "needle-in-the-haystack"], None);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let lines: Vec<_> = stdout(&out).lines().map(str::to_string).collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].contains("another needle"), "newest first: {lines:?}");
    assert!(lines[1].contains("filler filler"), "match past the preview: {lines:?}");

    // Every word must match, case-insensitively
    let out = run(&env, &["search", "NEEDLE", "end"], None);
    assert_eq!(stdout(&out).lines().count(), 1, "{}", stdout(&out));
}

#[test]
fn search_json_shape() {
    let env = TestEnv::new();
    copy_all(&env, &["alpha match", "beta", "gamma match"]);
    set_alias_in_db(&env, id_of(&env, "alpha match"), "a");

    let list = json(&run(&env, &["search", "match", "--json"], None));
    let list = list.as_array().unwrap();
    // Pinned first, then newest; positions are recency positions for `get N`
    let got: Vec<_> = list.iter().map(|e| (e["preview"].clone(), e["position"].clone())).collect();
    assert_eq!(got, [("alpha match".into(), 3.into()), ("gamma match".into(), 1.into())]);
    for e in list {
        for key in ["position", "id", "created_at", "last_used", "size", "preview", "pinned", "alias", "source", "mime"] {
            assert!(e.get(key).is_some(), "{key} missing from {e}");
        }
    }
    assert_eq!(list[0]["alias"], "a");

    let limited = json(&run(&env, &["search", "match", "--json", "-c", "1"], None));
    assert_eq!(limited.as_array().unwrap().len(), 1);
}

#[test]
fn search_no_match_exits_1() {
    let env = TestEnv::new();
    copy_all(&env, &["something"]);
    let out = run(&env, &["search", "nothing-like-it"], None);
    assert_eq!(code(&out), 1, "stderr: {}", stderr(&out));
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert!(stderr(&out).is_empty(), "silent like grep: {}", stderr(&out));

    let out = run(&env, &["search", "nothing-like-it", "--json"], None);
    assert_eq!(code(&out), 1);
    assert_eq!(stdout(&out).trim_end(), "[]");

    let out = run(&env, &["search", " "], None);
    assert_eq!(code(&out), 2, "an empty query is a usage error: {}", stderr(&out));
}

// ======== alias (3-7) ========

/// (pinned, alias) of the entry holding `content`, from `export`.
fn pin_state(env: &TestEnv, content: &str) -> (bool, Option<String>) {
    let entries = env.export();
    let e = entries.iter().find(|e| e["content"] == content).expect("entry exists");
    (e["pinned"].as_bool().unwrap(), e["alias"].as_str().map(str::to_string))
}

#[test]
fn alias_pins_and_names() {
    let env = TestEnv::new();
    copy_all(&env, &["my signature", "newer"]);
    let out = run(&env, &["alias", "2", "sig"], None);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("@sig"), "{}", stdout(&out));
    assert_eq!(pin_state(&env, "my signature"), (true, Some("sig".into())));
    assert_eq!(stdout(&run(&env, &["get", "@sig"], None)), "my signature");

    // A leading @ on the name is accepted; renaming replaces the old alias
    let id = id_of(&env, "my signature");
    assert_eq!(code(&run(&env, &["alias", &format!("id:{id}"), "@signature"], None)), 0);
    assert_eq!(pin_state(&env, "my signature"), (true, Some("signature".into())));
    assert_eq!(code(&run(&env, &["get", "@sig"], None)), 1);

    // Invalid names are usage errors
    for bad in ["Sig", "-x", "a b", ""] {
        let out = run(&env, &["alias", "1", bad], None);
        assert_eq!(code(&out), 2, "{bad:?}: {}", stderr(&out));
    }
    assert_eq!(code(&run(&env, &["alias", "1"], None)), 2, "NAME or --remove is required");
}

#[test]
fn alias_conflict_exits_1() {
    let env = TestEnv::new();
    copy_all(&env, &["first", "second"]);
    let first = id_of(&env, "first");
    assert_eq!(code(&run(&env, &["alias", &format!("id:{first}"), "x"], None)), 0);

    let out = run(&env, &["alias", "1", "x"], None);
    assert_eq!(code(&out), 1, "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains(&format!("alias @x already used by id:{first}")), "{}", stderr(&out));
    assert_eq!(pin_state(&env, "second"), (false, None), "nothing changed");
    // Naming an entry with the alias it already has is fine
    assert_eq!(code(&run(&env, &["alias", &format!("id:{first}"), "x"], None)), 0);
    // A missing entry is not found
    assert_eq!(code(&run(&env, &["alias", "id:999", "y"], None)), 1);
}

#[test]
fn alias_remove_keeps_pin() {
    let env = TestEnv::new();
    copy_all(&env, &["named"]);
    assert_eq!(code(&run(&env, &["alias", "1", "n"], None)), 0);
    let out = run(&env, &["alias", "--remove", "@n"], None);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(pin_state(&env, "named"), (true, None));
    assert_eq!(code(&run(&env, &["get", "@n"], None)), 1);
    // Removing from an entry without an alias is a no-op
    assert_eq!(code(&run(&env, &["alias", "--remove", "1"], None)), 0);
    // A name and --remove together are a usage error
    assert_eq!(code(&run(&env, &["alias", "--remove", "1", "n"], None)), 2);
}

#[test]
fn alias_at_pin_limit_exits_1() {
    let env = TestEnv::new();
    let texts: Vec<String> = (0..26).map(|i| format!("entry {i}")).collect();
    copy_all(&env, &texts.iter().map(String::as_str).collect::<Vec<_>>());
    // Pin the 25 oldest directly
    rusqlite::Connection::open(env.storage_dir.join("clipstack.db"))
        .unwrap()
        .execute("UPDATE entries SET pinned = 1 WHERE content != 'entry 25'", [])
        .unwrap();

    let out = run(&env, &["alias", "1", "one-too-many"], None);
    assert_eq!(code(&out), 1, "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("Maximum pinned entries (25)"), "{}", stderr(&out));
    assert_eq!(pin_state(&env, "entry 25"), (false, None));
    // An entry that is already pinned can still be named
    assert_eq!(code(&run(&env, &["alias", "2", "fine"], None)), 0);
}

/// A `PATH` dir holding only the fake wl-copy/wl-paste and the tools they
/// use: no wtype, so `pick --paste` must copy and skip the paste.
fn path_without_wtype(env: &TestEnv) -> std::path::PathBuf {
    let bin = env.root().join("bin-no-wtype");
    std::fs::create_dir_all(&bin).unwrap();
    for fake in ["wl-copy", "wl-paste"] {
        std::os::unix::fs::symlink(TestEnv::fakes_dir().join(fake), bin.join(fake)).unwrap();
    }
    let system = std::env::var_os("PATH").unwrap_or_default();
    for tool in ["cat", "mv"] {
        let real = std::env::split_paths(&system)
            .map(|dir| dir.join(tool))
            .find(|p| p.is_file())
            .unwrap_or_else(|| panic!("{tool} not on PATH"));
        std::os::unix::fs::symlink(real, bin.join(tool)).unwrap();
    }
    assert!(!bin.join("wtype").exists());
    bin
}

#[test]
fn pick_paste_without_wtype_still_copies() {
    let env = TestEnv::new();
    assert_eq!(code(&run(&env, &["copy"], Some("first"))), 0);
    assert_eq!(code(&run(&env, &["copy"], Some("second"))), 0);
    env.set_clipboard("something else", &["text/plain"]);

    let out = env
        .cmd()
        .args(["pick", "--paste"])
        .env("PATH", path_without_wtype(&env))
        // Debug builds only: pick entry 2 without the TUI or daemon autostart
        .env("CLIPSTACK_TEST_PICK", "2")
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("wtype not found"), "stderr: {}", stderr(&out));
    assert_eq!(env.clipboard_content().as_deref(), Some("first"));
    assert!(!env.lock_path().exists(), "the test hook starts no daemon");
}

#[test]
fn pick_paste_types_the_configured_keys_with_wtype() {
    let env = TestEnv::new();
    assert_eq!(code(&run(&env, &["copy"], Some("text"))), 0);
    env.write_config("[picker]\npaste_after_pick = true\npaste_keys = \"ctrl+shift+v\"\n");
    // The fake wtype (first on PATH) only logs its arguments
    let out = env.cmd().arg("pick").env("CLIPSTACK_TEST_PICK", "1").output().unwrap();
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(env.clipboard_content().as_deref(), Some("text"));

    // The helper runs detached after a short delay
    let calls = env.fake_clip_dir.join("wtype.calls");
    assert!(common::wait_until(std::time::Duration::from_secs(5), || calls.exists()), "wtype never ran");
    assert!(common::wait_until(std::time::Duration::from_secs(2), || {
        std::fs::read_to_string(&calls).is_ok_and(|c| c.ends_with('\n'))
    }));
    assert_eq!(std::fs::read_to_string(&calls).unwrap(), "-M ctrl -M shift -k v -m shift -m ctrl\n");
}

#[test]
fn pick_without_paste_never_runs_wtype() {
    let env = TestEnv::new();
    assert_eq!(code(&run(&env, &["copy"], Some("text"))), 0);
    let out = env.cmd().arg("pick").env("CLIPSTACK_TEST_PICK", "1").output().unwrap();
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert!(!env.fake_clip_dir.join("wtype.calls").exists());
}

#[test]
fn completions_generate_for_every_shell() {
    let env = TestEnv::new();
    for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
        let out = run(&env, &["completions", shell], None);
        assert_eq!(code(&out), 0, "{shell}: {}", stderr(&out));
        assert!(stdout(&out).contains("alias"), "{shell} completions lack subcommands");
        for hidden in ["watch-hook", "serve", "recover"] {
            assert!(!stdout(&out).contains(hidden), "{shell} completions offer the hidden {hidden}");
        }
    }
}

//! Entry commands: `pin`, `unpin`, `delete`, `edit`, `get --paste`, and
//! `get` on images (`--copy`, `--image-path`). All run through the isolated
//! `TestEnv` and the fake wl-clipboard/wtype.

mod common;

use common::{TestEnv, test_png};
use serde_json::Value;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
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

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Exit code; a run killed by a signal fails the test.
fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or_else(|| panic!("killed by a signal: {out:?}"))
}

/// Run and require exit 0.
fn ok(env: &TestEnv, args: &[&str], stdin: Option<&str>) -> Output {
    let out = run(env, args, stdin);
    assert_eq!(code(&out), 0, "{args:?}: stdout {} stderr {}", stdout(&out), stderr(&out));
    out
}

/// `list --json` with room for everything.
fn list(env: &TestEnv) -> Vec<Value> {
    serde_json::from_str(&stdout(&ok(env, &["list", "--json", "-c", "100"], None))).unwrap()
}

/// Where `get --image-path` caches entry `id`.
fn cache_path(env: &TestEnv, id: u64) -> PathBuf {
    env.runtime_dir.join("clipstack-images").join(format!("{id}.png"))
}

#[test]
fn pin_and_unpin() {
    let env = TestEnv::new();
    ok(&env, &["copy"], Some("older"));
    ok(&env, &["copy"], Some("newer"));

    let out = ok(&env, &["pin", "2"], None);
    assert!(stdout(&out).contains("pinned"), "{}", stdout(&out));
    assert_eq!(list(&env)[1]["pinned"], true);
    // Pinning again changes nothing and still succeeds
    let out = ok(&env, &["pin", "2"], None);
    assert!(stdout(&out).contains("already pinned"), "{}", stdout(&out));

    ok(&env, &["alias", "2", "sig"], None);
    let out = ok(&env, &["unpin", "@sig"], None);
    assert!(stdout(&out).contains("@sig"), "the removed alias is mentioned: {}", stdout(&out));
    let entries = list(&env);
    assert_eq!((&entries[1]["pinned"], &entries[1]["alias"]), (&Value::Bool(false), &Value::Null));
    let out = ok(&env, &["unpin", "2"], None);
    assert!(stdout(&out).contains("not pinned"), "{}", stdout(&out));

    for args in [["pin", "9"], ["unpin", "id:99"]] {
        let out = run(&env, &args, None);
        assert_eq!(code(&out), 1, "{args:?}");
        assert!(stderr(&out).contains("no entry for"), "{}", stderr(&out));
    }
}

#[test]
fn delete_removes_one_entry() {
    let env = TestEnv::new();
    ok(&env, &["copy"], Some("keep"));
    ok(&env, &["copy"], Some("drop"));
    let out = ok(&env, &["delete", "1"], None);
    assert!(stdout(&out).contains("Deleted id:"), "{}", stdout(&out));
    let entries = list(&env);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["preview"], "keep");
    assert_eq!(code(&run(&env, &["delete", "5"], None)), 1);
}

#[test]
fn json_entries_carry_kind_and_dimensions() {
    let env = TestEnv::new();
    ok(&env, &["copy"], Some("some text"));
    env.put_image(&test_png(640, 480, b"a"));

    let entries = list(&env);
    assert_eq!(entries[0]["kind"], "image");
    assert_eq!((entries[0]["width"].as_u64(), entries[0]["height"].as_u64()), (Some(640), Some(480)));
    assert_eq!(entries[0]["preview"], "Image \u{b7} 640x480");
    assert_eq!(entries[0]["mime"], "image/png");
    assert_eq!(entries[1]["kind"], "text");
    assert_eq!((&entries[1]["width"], &entries[1]["height"]), (&Value::Null, &Value::Null));

    let found: Vec<Value> = serde_json::from_str(&stdout(&ok(&env, &["search", "--json", "some"], None))).unwrap();
    assert_eq!(found[0]["kind"], "text");
    assert!(found[0].get("width").is_some_and(Value::is_null), "{found:?}");
}

#[test]
fn get_writes_image_bytes_and_copies_them_as_png() {
    let env = TestEnv::new();
    let png = test_png(32, 16, b"get");
    env.put_image(&png);

    let out = ok(&env, &["get", "1"], None);
    assert_eq!(out.stdout, png, "the PNG bytes as stored");

    env.set_clipboard("something else", &["text/plain"]);
    let out = ok(&env, &["get", "1", "--copy"], None);
    assert!(out.stdout.is_empty(), "--copy of an image writes nothing to stdout");
    assert!(stderr(&out).contains("32x16"), "{}", stderr(&out));
    assert_eq!(std::fs::read(env.fake_clip_dir.join("content")).unwrap(), png);
    assert_eq!(std::fs::read_to_string(env.fake_clip_dir.join("types")).unwrap(), "image/png\n");
    let calls = std::fs::read_to_string(env.fake_clip_dir.join("wl-copy.calls")).unwrap();
    assert!(calls.lines().any(|l| l == "--type image/png"), "{calls}");
    assert_eq!(list(&env).len(), 1, "copying bumps, it does not duplicate");
}

#[test]
fn image_path_caches_the_png_privately() {
    let env = TestEnv::new();
    let png = test_png(8, 8, b"cache");
    env.put_image(&png);
    let id = list(&env)[0]["id"].as_u64().unwrap();

    let out = ok(&env, &["get", "1", "--image-path"], None);
    let path = cache_path(&env, id);
    assert_eq!(stdout(&out), format!("{}\n", path.display()));
    assert_eq!(std::fs::read(&path).unwrap(), png);
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);

    // A damaged cache file is rewritten; no temp files are left behind
    std::fs::write(&path, b"junk").unwrap();
    ok(&env, &["get", &format!("id:{id}"), "--image-path"], None);
    assert_eq!(std::fs::read(&path).unwrap(), png);
    let names: Vec<String> =
        std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    assert_eq!(names, [format!("{id}.png")]);

    ok(&env, &["copy"], Some("text"));
    let out = run(&env, &["get", "1", "--image-path"], None);
    assert_eq!(code(&out), 1, "text has no image path");
    assert!(stderr(&out).contains("is text, not an image"), "{}", stderr(&out));
}

#[test]
fn delete_and_clear_remove_cached_images() {
    let env = TestEnv::new();
    env.put_image(&test_png(4, 4, b"one"));
    env.put_image(&test_png(5, 5, b"two"));
    let ids: Vec<u64> = list(&env).iter().map(|e| e["id"].as_u64().unwrap()).collect();
    for id in &ids {
        ok(&env, &["get", &format!("id:{id}"), "--image-path"], None);
        assert!(cache_path(&env, *id).exists());
    }

    ok(&env, &["delete", &format!("id:{}", ids[0])], None);
    assert!(!cache_path(&env, ids[0]).exists(), "delete removes the cached file");
    assert!(cache_path(&env, ids[1]).exists());

    ok(&env, &["clear"], None);
    assert!(!cache_path(&env, ids[1]).exists(), "clear removes cached files");
}

#[test]
fn image_path_prunes_files_of_entries_that_are_gone() {
    let env = TestEnv::new();
    env.put_image(&test_png(4, 4, b"one"));
    let id = list(&env)[0]["id"].as_u64().unwrap();
    // Left by an entry that retention or the entry limit removed
    let dir = env.runtime_dir.join("clipstack-images");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("999.png"), b"old").unwrap();
    std::fs::write(dir.join("notes.txt"), b"not ours").unwrap();

    ok(&env, &["get", &format!("id:{id}"), "--image-path"], None);
    assert!(!dir.join("999.png").exists());
    assert!(dir.join("notes.txt").exists(), "only <id>.png files are touched");
}


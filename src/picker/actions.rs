//! Side effects the picker starts: editing an entry in `$EDITOR`, and the
//! wtype keystrokes of `pick --paste`.

use crate::perms;
use crate::storage::EntryId;
use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `$VISUAL`, else `$EDITOR`, else `vi` (empty values are skipped).
pub fn choose_editor(visual: Option<&str>, editor: Option<&str>) -> String {
    [visual, editor]
        .into_iter()
        .flatten()
        .find(|e| !e.trim().is_empty())
        .unwrap_or("vi")
        .to_string()
}

/// The wtype argv that types `keys` (`"shift+Insert"`, `"ctrl+shift+v"`):
/// modifiers pressed in order and released in reverse around the key.
pub fn paste_command(keys: &str) -> Result<Vec<String>, String> {
    let bad = |why: String| format!("paste_keys = {keys:?}: {why}");
    let mut parts: Vec<&str> = keys.split('+').map(str::trim).collect();
    let key = parts.pop().unwrap_or_default();
    let mut modifiers: Vec<&str> = Vec::new();
    for part in parts {
        let modifier = match part.to_ascii_lowercase().as_str() {
            "shift" => "shift",
            "ctrl" | "control" => "ctrl",
            "alt" => "alt",
            "altgr" => "altgr",
            "super" | "logo" | "win" => "logo",
            _ => return Err(bad(format!("unknown modifier {part:?} (use shift, ctrl, alt, altgr or super)"))),
        };
        if modifiers.contains(&modifier) {
            return Err(bad(format!("{part:?} given twice")));
        }
        modifiers.push(modifier);
    }
    let key = match key.to_ascii_lowercase().as_str() {
        "insert" => "Insert".to_string(),
        "return" | "enter" => "Return".to_string(),
        k if k.len() == 1 && k.bytes().all(|b| b.is_ascii_alphanumeric()) => k.to_string(),
        _ => return Err(bad(format!("unknown key {key:?} (use Insert, Return, or one letter or digit)"))),
    };
    let mut argv = vec!["wtype".to_string()];
    for m in &modifiers {
        argv.extend(["-M".to_string(), m.to_string()]);
    }
    argv.extend(["-k".to_string(), key]);
    for m in modifiers.iter().rev() {
        argv.extend(["-m".to_string(), m.to_string()]);
    }
    Ok(argv)
}

/// How long the paste helper waits, so the picker's window can close and
/// focus return to the previous window.
pub const PASTE_DELAY: &str = "0.15";

/// Run `wtype args` after `PASTE_DELAY`, detached: its own process group
/// (closing the terminal must not SIGHUP it), null stdio, never waited on.
pub fn spawn_paste(wtype: &Path, args: &[String]) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    // Never waited on: init reaps it once clipstack exits, a moment from now
    Command::new("sh")
        .arg("-c")
        .arg(format!("sleep {PASTE_DELAY}; exec \"$@\""))
        .arg("sh")
        .arg(wtype)
        .args(args)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

/// Removes the edit file however editing ends.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Edit `content` with `editor` (a shell command line, like git's
/// `core.editor`) in `<dir>/clipstack-edit-<pid>-<id>.txt`, created `0600`
/// (never an existing file) and removed afterwards. Returns the new text,
/// or `None` if the editor failed or the text is unchanged or blank. A
/// final newline the editor adds to content that had none is dropped.
pub fn edit_in_editor(content: &str, editor: &str, dir: &Path, id: EntryId) -> Result<Option<String>> {
    let path = dir.join(format!("clipstack-edit-{}-{id}.txt", std::process::id()));
    let mut file = perms::private_file_options()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    let guard = RemoveOnDrop(path);
    file.write_all(content.as_bytes())?;
    drop(file);

    // The editor runs on the terminal: stdio is inherited
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(&guard.0)
        .status();
    if !status.is_ok_and(|s| s.success()) {
        return Ok(None);
    }
    let bytes = std::fs::read(&guard.0).with_context(|| format!("cannot read {}", guard.0.display()))?;
    let mut edited = String::from_utf8(bytes).context("the edited text is not UTF-8")?;
    if !content.ends_with('\n') && edited.ends_with('\n') {
        edited.pop();
        if edited.ends_with('\r') {
            edited.pop();
        }
    }
    Ok((edited != content && !edited.trim().is_empty()).then_some(edited))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    /// An editor that replaces the file with `text` (a printf format)
    fn writes(text: &str) -> String {
        format!("sh -c 'printf \"{text}\" > \"$1\"' --")
    }

    #[test]
    fn edit_returns_modified_content() {
        let dir = TempDir::new().unwrap();
        let edited = edit_in_editor("before", &writes("edited"), dir.path(), 7).unwrap();
        assert_eq!(edited.as_deref(), Some("edited"));
        // The editor sees the original text
        let edited = edit_in_editor("abc", "sh -c 'printf x >> \"$1\"' --", dir.path(), 7).unwrap();
        assert_eq!(edited.as_deref(), Some("abcx"));
    }

    #[test]
    fn edit_unchanged_returns_none() {
        let dir = TempDir::new().unwrap();
        assert_eq!(edit_in_editor("same", "true", dir.path(), 1).unwrap(), None);
        assert_eq!(edit_in_editor("same", &writes(""), dir.path(), 1).unwrap(), None, "emptied");
        assert_eq!(edit_in_editor("same", &writes("  \n"), dir.path(), 1).unwrap(), None, "blank");
        // An editor that adds a final newline (vim) changes nothing
        let adds_newline = "sh -c 'printf \"\\n\" >> \"$1\"' --";
        assert_eq!(edit_in_editor("same", adds_newline, dir.path(), 1).unwrap(), None);
        // ...but a real edit keeps the content's lack of one
        let edits_and_adds = "sh -c 'printf \"other\\n\" > \"$1\"' --";
        assert_eq!(edit_in_editor("same", edits_and_adds, dir.path(), 1).unwrap().as_deref(), Some("other"));
        // Content that ends with a newline keeps what the editor wrote
        let edited = edit_in_editor("a\n", "sh -c 'printf \"b\\n\" > \"$1\"' --", dir.path(), 1).unwrap();
        assert_eq!(edited.as_deref(), Some("b\n"));
    }

    #[test]
    fn editor_failure_returns_none() {
        let dir = TempDir::new().unwrap();
        let fails = "sh -c 'printf changed > \"$1\"; exit 3' --";
        assert_eq!(edit_in_editor("before", fails, dir.path(), 2).unwrap(), None);
        assert_eq!(edit_in_editor("before", "/nonexistent/editor", dir.path(), 2).unwrap(), None);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "temp file removed on failure");
    }

    #[test]
    fn edit_temp_file_is_0600_in_runtime_dir_and_removed() {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("seen");
        // The editor records the file's path and mode
        let editor = format!("sh -c 'printf \"%s %s\" \"$1\" $(stat -c %a \"$1\") > {}' --", log.display());
        assert_eq!(edit_in_editor("secret", &editor, dir.path(), 42).unwrap(), None);
        let seen = std::fs::read_to_string(&log).unwrap();
        let (path, mode) = seen.rsplit_once(' ').unwrap();
        let expected = dir.path().join(format!("clipstack-edit-{}-42.txt", std::process::id()));
        assert_eq!(Path::new(path), expected);
        assert_eq!(mode, "600");
        assert!(!expected.exists(), "removed after editing");

        // A file already at that path is never followed or reused
        std::fs::write(&expected, "planted").unwrap();
        std::fs::set_permissions(&expected, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(edit_in_editor("secret", "true", dir.path(), 42).is_err());
        assert_eq!(std::fs::read_to_string(&expected).unwrap(), "planted");
    }

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn paste_command_for_shift_insert() {
        let shift_insert = argv(&["wtype", "-M", "shift", "-k", "Insert", "-m", "shift"]);
        assert_eq!(paste_command("shift+Insert").unwrap(), shift_insert);
        assert_eq!(paste_command(" Shift + insert ").unwrap(), shift_insert, "case and spaces");
        assert_eq!(
            paste_command("ctrl+shift+v").unwrap(),
            argv(&["wtype", "-M", "ctrl", "-M", "shift", "-k", "v", "-m", "shift", "-m", "ctrl"]),
            "modifiers are released in reverse"
        );
        assert_eq!(paste_command("super+V").unwrap(), argv(&["wtype", "-M", "logo", "-k", "v", "-m", "logo"]));
        assert_eq!(paste_command("Insert").unwrap(), argv(&["wtype", "-k", "Insert"]));
        assert_eq!(paste_command("control+alt+Return").unwrap()[6], "Return");
    }

    #[test]
    fn paste_command_rejects_unknown_key() {
        for keys in ["", "shift+", "+Insert", "shift+F13", "hyper+v", "shift+shift+v", "ctrl+v+x", "shift+Ins;rm", "ctrl+é"] {
            let err = paste_command(keys).unwrap_err();
            assert!(err.contains("paste_keys"), "{keys:?}: {err}");
        }
    }

    #[test]
    fn editor_choice_order() {
        assert_eq!(choose_editor(Some("code --wait"), Some("nano")), "code --wait");
        assert_eq!(choose_editor(None, Some("nano")), "nano");
        assert_eq!(choose_editor(Some(""), Some("nano")), "nano");
        assert_eq!(choose_editor(Some(" "), None), "vi");
        assert_eq!(choose_editor(None, None), "vi");
    }
}

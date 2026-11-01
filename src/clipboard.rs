//! wl-clipboard (`wl-copy`, `wl-paste`) behind a struct holding the program
//! paths, so tests can point it at fakes.

use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const CLIPBOARD_TROUBLESHOOT: &str = "\
Troubleshooting:
  • Is wl-clipboard installed? (which wl-paste)
  • Are you in a Wayland session? (echo $WAYLAND_DISPLAY)
  • Is your compositor running?";

/// Offer mime type that password managers (and `wl-copy --sensitive`) set
/// on secrets. Content offered with it is never saved.
pub const PASSWORD_HINT_MIME: &str = "x-kde-passwordManagerHint";

/// How long `list_types` waits for wl-paste.
const LIST_TYPES_TIMEOUT: Duration = Duration::from_secs(1);
/// How long `paste_text` waits for wl-paste (the source client sends the data).
const PASTE_TIMEOUT: Duration = Duration::from_secs(5);

/// The wl-clipboard programs to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clipboard {
    pub paste_bin: PathBuf,
    pub copy_bin: PathBuf,
}

impl Clipboard {
    /// `wl-paste` and `wl-copy` from PATH.
    pub fn system() -> Self {
        Self { paste_bin: PathBuf::from("wl-paste"), copy_bin: PathBuf::from("wl-copy") }
    }

    /// Put `content` on the clipboard with wl-copy.
    pub fn copy(&self, content: &str) -> Result<()> {
        let mut child = Command::new(&self.copy_bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            // Inherited, not piped: wl-copy forks to serve the selection, and
            // the forked child would keep a stderr pipe open indefinitely.
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("Failed to run wl-copy.\n{CLIPBOARD_TROUBLESHOOT}"))?;

        {
            let mut stdin = child.stdin.take().context("Failed to get wl-copy stdin")?;
            stdin.write_all(content.as_bytes()).context("Failed to write to wl-copy stdin")?;
            // Dropping stdin sends EOF
        }

        // The wl-copy parent exits once its forked child holds the selection
        let status = child.wait()?;
        if !status.success() {
            anyhow::bail!("wl-copy failed with status: {status}");
        }
        Ok(())
    }

    /// The clipboard as text (`wl-paste --no-newline --type text`). No
    /// selection, or an offer without any text type, is `""`.
    pub fn paste_text(&self) -> Result<String> {
        let mut cmd = Command::new(&self.paste_bin);
        cmd.args(["--no-newline", "--type", "text"]);
        let output = run_with_timeout(cmd, PASTE_TIMEOUT)
            .with_context(|| format!("Failed to run wl-paste.\n{CLIPBOARD_TROUBLESHOOT}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if is_no_text(&stderr) {
                return Ok(String::new());
            }
            anyhow::bail!("wl-paste failed: {}", stderr.trim());
        }
        String::from_utf8(output.stdout).context("Clipboard content is not valid UTF-8")
    }

    /// The mime types the current selection is offered as
    /// (`wl-paste --list-types`); empty when there is no selection. Gives up
    /// after 1 s, so a stuck source client can't stall capture.
    #[cfg_attr(not(test), expect(dead_code, reason = "first caller is the watch hook (commit 2-4)"))]
    pub fn list_types(&self) -> Result<Vec<String>> {
        let mut cmd = Command::new(&self.paste_bin);
        cmd.arg("--list-types");
        let output = run_with_timeout(cmd, LIST_TYPES_TIMEOUT).context("wl-paste --list-types")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if is_no_text(&stderr) {
                return Ok(Vec::new());
            }
            anyhow::bail!("wl-paste --list-types failed: {}", stderr.trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect())
    }
}

/// wl-paste's messages for "nothing to paste as text".
fn is_no_text(stderr: &str) -> bool {
    stderr.contains("No selection") || stderr.contains("not available as requested type")
}

/// Run `cmd` with stdout and stderr captured, killing it after `timeout`.
/// The pipes are drained on threads so a large paste can't fill them.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Result<Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to run {}", cmd.get_program().to_string_lossy()))?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!(
                "{} timed out after {} ms",
                cmd.get_program().to_string_lossy(),
                timeout.as_millis()
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    Ok(Output { status, stdout, stderr })
}

/// Read a child pipe to the end on its own thread.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use tempfile::TempDir;

    /// A `/bin/sh` script standing in for wl-paste or wl-copy. Each gets a
    /// new name: rewriting a script that was just run can fail with ETXTBSY.
    fn fake_bin(dir: &Path, name: &str, body: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = dir.join(format!("{name}-{n}"));
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn fake_clipboard(dir: &Path, paste_body: &str, copy_body: &str) -> Clipboard {
        Clipboard {
            paste_bin: fake_bin(dir, "wl-paste", paste_body),
            copy_bin: fake_bin(dir, "wl-copy", copy_body),
        }
    }

    #[test]
    fn system_uses_wl_clipboard_from_path() {
        let clipboard = Clipboard::system();
        assert_eq!(clipboard.paste_bin, PathBuf::from("wl-paste"));
        assert_eq!(clipboard.copy_bin, PathBuf::from("wl-copy"));
    }

    #[test]
    fn list_types_parses_lines() {
        let dir = TempDir::new().unwrap();
        let args = dir.path().join("args");
        let clipboard = fake_clipboard(
            dir.path(),
            &format!(
                "echo \"$*\" > '{}'\nprintf 'text/plain;charset=utf-8\\r\\n\\n  x-kde-passwordManagerHint \\n'",
                args.display()
            ),
            "exit 1",
        );
        assert_eq!(
            clipboard.list_types().unwrap(),
            vec!["text/plain;charset=utf-8".to_string(), PASSWORD_HINT_MIME.to_string()]
        );
        assert_eq!(std::fs::read_to_string(args).unwrap(), "--list-types\n");

        let empty = fake_clipboard(dir.path(), "echo 'No selection' >&2; exit 1", "exit 1");
        assert_eq!(empty.list_types().unwrap(), Vec::<String>::new());

        let broken = fake_clipboard(dir.path(), "echo 'Failed to connect' >&2; exit 1", "exit 1");
        let err = broken.list_types().unwrap_err();
        assert!(format!("{err:#}").contains("Failed to connect"), "{err:#}");
    }

    #[test]
    fn list_types_gives_up_after_timeout() {
        let dir = TempDir::new().unwrap();
        let clipboard = fake_clipboard(dir.path(), "exec sleep 10", "exit 1");
        let started = std::time::Instant::now();
        let err = clipboard.list_types().unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
        assert!(format!("{err:#}").contains("timed out"), "{err:#}");
    }

    #[test]
    fn paste_text_no_selection_is_empty() {
        let dir = TempDir::new().unwrap();
        let args = dir.path().join("args");
        let clipboard = fake_clipboard(
            dir.path(),
            &format!("echo \"$*\" > '{}'\necho 'No selection' >&2\nexit 1", args.display()),
            "exit 1",
        );
        assert_eq!(clipboard.paste_text().map_err(|e| format!("{e:#}")), Ok(String::new()));
        assert_eq!(std::fs::read_to_string(args).unwrap(), "--no-newline --type text\n");

        // An offer without any text type is no text either
        let image = fake_clipboard(
            dir.path(),
            "echo 'Clipboard content is not available as requested type \"text\"' >&2; exit 1",
            "exit 1",
        );
        assert_eq!(image.paste_text().unwrap(), "");

        let text = fake_clipboard(dir.path(), "printf 'caf\\303\\251\\n'", "exit 1");
        assert_eq!(text.paste_text().unwrap(), "caf\u{e9}\n");

        let binary = fake_clipboard(dir.path(), "printf '\\377'", "exit 1");
        assert!(binary.paste_text().is_err(), "non-UTF-8 is an error");
    }

    #[test]
    fn copy_writes_content() {
        let dir = TempDir::new().unwrap();
        let out = dir.path().join("copied");
        let clipboard = fake_clipboard(dir.path(), "exit 1", &format!("cat > '{}'", out.display()));
        clipboard.copy("line 1\nline 2 \u{1f389}").unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "line 1\nline 2 \u{1f389}");

        let failing = fake_clipboard(dir.path(), "exit 1", "cat > /dev/null; exit 3");
        assert!(failing.copy("x").is_err());

        let missing = Clipboard {
            paste_bin: dir.path().join("nope"),
            copy_bin: dir.path().join("nope"),
        };
        assert!(missing.copy("x").is_err());
        assert!(missing.paste_text().is_err());
    }

    // These tests overwrite the user's real desktop clipboard, so `--ignored`
    // alone doesn't run them: they also need CLIPSTACK_REAL_CLIPBOARD_TESTS=1.
    fn real_clipboard_opt_in() -> bool {
        if std::env::var_os("CLIPSTACK_REAL_CLIPBOARD_TESTS").is_some() {
            return true;
        }
        eprintln!("skipped: set CLIPSTACK_REAL_CLIPBOARD_TESTS=1 to touch the real clipboard");
        false
    }

    #[test]
    #[ignore] // CLIPSTACK_REAL_CLIPBOARD_TESTS=1 cargo test -- --ignored
    fn test_copy_and_paste() {
        if !real_clipboard_opt_in() {
            return;
        }
        let content = "test clipboard content";
        Clipboard::system().copy(content).unwrap();

        let pasted = Clipboard::system().paste_text().unwrap();
        assert_eq!(pasted, content);
    }

    #[test]
    #[ignore]
    fn test_large_content() {
        if !real_clipboard_opt_in() {
            return;
        }
        let content = "x".repeat(500_000); // 500KB
        Clipboard::system().copy(&content).unwrap();

        let pasted = Clipboard::system().paste_text().unwrap();
        assert_eq!(pasted.len(), 500_000);
    }

    #[test]
    #[ignore]
    fn test_unicode_content() {
        if !real_clipboard_opt_in() {
            return;
        }
        let content = "Hello 世界 🎉 émojis";
        Clipboard::system().copy(content).unwrap();

        let pasted = Clipboard::system().paste_text().unwrap();
        assert_eq!(pasted, content);
    }
}

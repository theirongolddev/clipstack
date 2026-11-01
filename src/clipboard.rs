//! wl-clipboard (`wl-copy`, `wl-paste`) behind a struct holding the program
//! paths, so tests can point it at fakes.

use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const CLIPBOARD_TROUBLESHOOT: &str = "\
Troubleshooting:
  • Is wl-clipboard installed? (command -v wl-paste)
  • Are you in a Wayland session? (echo $WAYLAND_DISPLAY)
  • Is your compositor running?";

/// Offer mime type that password managers (and `wl-copy --sensitive`) set
/// on secrets. Content offered with it is never saved.
pub const PASSWORD_HINT_MIME: &str = "x-kde-passwordManagerHint";

/// Whether an offer's types mark it as a password manager's secret.
pub fn is_sensitive_offer(types: &[String]) -> bool {
    types.iter().any(|t| t == PASSWORD_HINT_MIME)
}

/// Whether an offer includes a type wl-paste reads as text: text is
/// captured in preference to an image.
pub fn offers_text(types: &[String]) -> bool {
    types.iter().any(|t| is_text_mime(t))
}

/// wl-clipboard 2.3's `mime_type_is_text`: `text/*`, `TEXT`, `STRING`,
/// `UTF8_STRING`, anything with `json`, a `script`/`xml`/`yaml`/`csv`/`ini`
/// suffix, and PGP/SSH keys. `wl-paste --type text` (and `--watch` without
/// `--type`) picks such a type whenever one is offered, so the daemon must
/// agree with it on whether an offer is text.
pub fn is_text_mime(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(mime, "TEXT" | "STRING" | "UTF8_STRING")
        || mime.contains("json")
        || ["script", "xml", "yaml", "csv", "ini", "pgp-keys"].iter().any(|s| mime.ends_with(s))
        || mime.contains("application/vnd.ms-publisher")
}

/// A type that holds plain text: `text/plain` (with any parameters) or one
/// of the X11 names `UTF8_STRING`, `STRING`, `TEXT`.
pub fn is_plain_text_mime(mime: &str) -> bool {
    mime.starts_with("text/plain") || matches!(mime, "UTF8_STRING" | "STRING" | "TEXT")
}

/// What the daemon saves from an offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    Text,
    /// The `image/png` type
    Image,
    /// Nothing the daemon saves (no text, no PNG)
    Neither,
}

/// Which part of an offer the daemon saves, in watch and poll mode alike.
/// The PNG wins unless a plain-text type is offered too: a browser copying
/// an image adds only incidental text (an `<img>` HTML fragment, a URL),
/// while an app that offers plain text next to a picture of it (a
/// spreadsheet cell) is copying text. Without a PNG, anything wl-paste
/// reads as text is text.
pub fn choose_offer(types: &[String]) -> Offer {
    let png = types.iter().any(|t| t == crate::png::MIME);
    if png && !types.iter().any(|t| is_plain_text_mime(t)) {
        Offer::Image
    } else if offers_text(types) {
        Offer::Text
    } else {
        Offer::Neither
    }
}

/// wl-copy or wl-paste failed (exit code 6). It wraps the underlying error
/// without changing its message or cause chain, so it only marks the error.
#[derive(Debug)]
pub struct ClipboardError(anyhow::Error);

impl From<anyhow::Error> for ClipboardError {
    fn from(err: anyhow::Error) -> Self {
        ClipboardError(err)
    }
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for ClipboardError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

/// Mark a wl-clipboard failure as a `ClipboardError`.
fn tool_failure(err: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(ClipboardError(err))
}

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
        self.run_copy(content.as_bytes(), None).map_err(tool_failure)
    }

    /// Put `bytes` on the clipboard offered as `mime` (`wl-copy --type MIME`).
    pub fn copy_bytes(&self, bytes: &[u8], mime: &str) -> Result<()> {
        self.run_copy(bytes, Some(mime)).map_err(tool_failure)
    }

    fn run_copy(&self, content: &[u8], mime: Option<&str>) -> Result<()> {
        let mut cmd = Command::new(&self.copy_bin);
        if let Some(mime) = mime {
            cmd.args(["--type", mime]);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            // Inherited, not piped: wl-copy forks to serve the selection, and
            // the forked child would keep a stderr pipe open indefinitely.
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("Failed to run wl-copy.\n{CLIPBOARD_TROUBLESHOOT}"))?;

        {
            let mut stdin = child.stdin.take().context("Failed to get wl-copy stdin")?;
            stdin.write_all(content).context("Failed to write to wl-copy stdin")?;
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
        self.run_paste_text().map_err(tool_failure)
    }

    fn run_paste_text(&self) -> Result<String> {
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

    /// The selection as `mime` (`wl-paste --no-newline --type MIME`), as raw
    /// bytes. No selection, or one no longer offered as `mime`, is empty.
    pub fn paste_bytes(&self, mime: &str) -> Result<Vec<u8>> {
        self.run_paste_bytes(mime).map_err(tool_failure)
    }

    fn run_paste_bytes(&self, mime: &str) -> Result<Vec<u8>> {
        let mut cmd = Command::new(&self.paste_bin);
        cmd.args(["--no-newline", "--type", mime]);
        let output = run_with_timeout(cmd, PASTE_TIMEOUT)
            .with_context(|| format!("Failed to run wl-paste.\n{CLIPBOARD_TROUBLESHOOT}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if is_no_text(&stderr) {
                return Ok(Vec::new());
            }
            anyhow::bail!("wl-paste --type {mime} failed: {}", stderr.trim());
        }
        Ok(output.stdout)
    }

    /// The mime types the current selection is offered as
    /// (`wl-paste --list-types`); empty when there is no selection. Gives up
    /// after 1 s, so a stuck source client can't stall capture.
    pub fn list_types(&self) -> Result<Vec<String>> {
        self.run_list_types().map_err(tool_failure)
    }

    fn run_list_types(&self) -> Result<Vec<String>> {
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

/// wl-paste's messages for "nothing to paste" (as text, or as the requested type).
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
    use std::path::Path;
    use tempfile::TempDir;

    /// A `/bin/sh` script standing in for wl-paste or wl-copy, under a new
    /// name each time. A child `sh` writes it: had this process opened it for
    /// writing, a test thread forking at that moment would hold the write fd
    /// until its exec, and running the script would fail with ETXTBSY.
    fn fake_bin(dir: &Path, name: &str, body: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = dir.join(format!("{name}-{n}"));
        let mut writer = Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(&path)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = writer.stdin.take().unwrap();
        stdin.write_all(format!("#!/bin/sh\n{body}\n").as_bytes()).unwrap();
        drop(stdin);
        assert!(writer.wait().unwrap().success());
        path
    }

    fn fake_clipboard(dir: &Path, paste_body: &str, copy_body: &str) -> Clipboard {
        Clipboard {
            paste_bin: fake_bin(dir, "wl-paste", paste_body),
            copy_bin: fake_bin(dir, "wl-copy", copy_body),
        }
    }

    fn types(list: &[&str]) -> Vec<String> {
        list.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn image_wins_over_incidental_text() {
        // Firefox "Copy Image": HTML fragments and URLs, no plain text
        let firefox = types(&[
            "text/x-moz-url",
            "text/_moz_htmlcontext",
            "text/_moz_htmlinfo",
            "text/html",
            "application/x-moz-nativeimage",
            "image/png",
            "image/jpeg",
            "image/gif",
            "application/x-moz-file-promise",
        ]);
        assert_eq!(choose_offer(&firefox), Offer::Image);
        // Chromium "Copy image": an <img> fragment and the PNG
        let chromium = types(&["chromium/x-internal-source-rfh-token", "text/html", "image/png"]);
        assert_eq!(choose_offer(&chromium), Offer::Image);
        assert_eq!(choose_offer(&types(&["image/png", "text/uri-list"])), Offer::Image);
        // GIMP: many image types, no text
        let gimp = types(&["image/png", "image/bmp", "image/x-bmp", "image/tiff", "image/jpeg", "image/x-xcf"]);
        assert_eq!(choose_offer(&gimp), Offer::Image);
        // grim | wl-copy
        assert_eq!(choose_offer(&types(&["image/png"])), Offer::Image);
        // A secret image is still an image offer; ingest skips it as sensitive
        assert_eq!(choose_offer(&types(&["image/png", PASSWORD_HINT_MIME])), Offer::Image);
        // Text that isn't plain text no longer beats a PNG either
        assert_eq!(choose_offer(&types(&["image/png", "application/json"])), Offer::Image);
    }

    #[test]
    fn plain_text_wins_over_png() {
        // LibreOffice Calc cell: text and a rendering of it
        let calc = types(&[
            "application/x-openoffice-embed-source-xml;windows_formatname=\"Star Embed Source (XML)\"",
            "text/plain;charset=utf-8",
            "UTF8_STRING",
            "text/html",
            "text/richtext",
            "image/png",
            "image/bmp",
        ]);
        assert_eq!(choose_offer(&calc), Offer::Text);
        for plain in ["text/plain", "text/plain;charset=utf-8", "UTF8_STRING", "STRING", "TEXT"] {
            assert_eq!(choose_offer(&types(&["text/html", "image/png", plain])), Offer::Text, "{plain}");
        }
        // A terminal text copy
        let terminal = types(&["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "TEXT", "STRING"]);
        assert_eq!(choose_offer(&terminal), Offer::Text);
    }

    #[test]
    fn text_without_png_is_text_and_other_binaries_are_neither() {
        // Any type wl-paste reads as text is text when no PNG is offered
        for text in ["text/html", "application/json", "application/javascript", "application/xml", "text/csv", "application/x-yaml", "application/pgp-keys"] {
            assert!(offers_text(&types(&[text])), "{text}");
            assert_eq!(choose_offer(&types(&[text, "image/jpeg"])), Offer::Text, "{text}");
        }
        assert!(!offers_text(&types(&["image/png", "application/x-text"])));
        assert_eq!(choose_offer(&types(&["image/jpeg"])), Offer::Neither);
        assert_eq!(choose_offer(&types(&["application/x-text"])), Offer::Neither);
        assert_eq!(choose_offer(&[]), Offer::Neither);
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

    #[test]
    fn paste_bytes_returns_binary_of_the_given_type() {
        let dir = TempDir::new().unwrap();
        let args = dir.path().join("args");
        let clipboard = fake_clipboard(
            dir.path(),
            &format!("echo \"$*\" > '{}'\nprintf '\\211PNG\\000\\377\\n'", args.display()),
            "exit 1",
        );
        assert_eq!(clipboard.paste_bytes("image/png").unwrap(), b"\x89PNG\x00\xff\n");
        assert_eq!(std::fs::read_to_string(args).unwrap(), "--no-newline --type image/png\n");

        let gone = fake_clipboard(
            dir.path(),
            "echo 'Clipboard content is not available as requested type \"image/png\"' >&2; exit 1",
            "exit 1",
        );
        assert_eq!(gone.paste_bytes("image/png").unwrap(), b"", "the selection changed meanwhile");
        let broken = fake_clipboard(dir.path(), "echo 'Failed to connect' >&2; exit 1", "exit 1");
        let err = broken.paste_bytes("image/png").unwrap_err();
        assert!(err.chain().any(|c| c.is::<ClipboardError>()) || err.is::<ClipboardError>(), "{err:#}");
    }

    #[test]
    fn copy_bytes_offers_the_given_type() {
        let dir = TempDir::new().unwrap();
        let out = dir.path().join("copied");
        let args = dir.path().join("args");
        let clipboard = fake_clipboard(
            dir.path(),
            "exit 1",
            &format!("echo \"$*\" > '{}'; cat > '{}'", args.display(), out.display()),
        );
        let png = b"\x89PNG\r\n\x1a\n\x00\xff";
        clipboard.copy_bytes(png, "image/png").unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), png);
        assert_eq!(std::fs::read_to_string(&args).unwrap(), "--type image/png\n");

        // Plain copy passes no type, as before
        clipboard.copy("text").unwrap();
        assert_eq!(std::fs::read_to_string(&args).unwrap(), "\n");

        let failing = fake_clipboard(dir.path(), "exit 1", "cat > /dev/null; exit 3");
        assert!(failing.copy_bytes(png, "image/png").is_err());
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

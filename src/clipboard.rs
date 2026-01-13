use anyhow::{Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

pub struct Clipboard;

impl Clipboard {
    /// Copy content to the system clipboard using wl-copy
    pub fn copy(content: &str) -> Result<()> {
        let mut child = Command::new("wl-copy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn wl-copy. Is wl-clipboard installed?")?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(content.as_bytes())
                .context("Failed to write to wl-copy stdin")?;
        }

        let output = child.wait_with_output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("wl-copy failed: {}", stderr);
        }

        Ok(())
    }

    /// Paste content from the system clipboard using wl-paste
    pub fn paste() -> Result<String> {
        Self::paste_selection(false)
    }

    /// Paste content from PRIMARY selection (mouse selection)
    pub fn paste_primary() -> Result<String> {
        Self::paste_selection(true)
    }

    fn paste_selection(primary: bool) -> Result<String> {
        let mut cmd = Command::new("wl-paste");
        cmd.arg("--no-newline");
        if primary {
            cmd.arg("--primary");
        }

        let output = cmd
            .output()
            .context("Failed to run wl-paste. Is wl-clipboard installed?")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Empty clipboard is not an error
            if stderr.contains("No selection") {
                return Ok(String::new());
            }
            anyhow::bail!("wl-paste failed: {}", stderr);
        }

        String::from_utf8(output.stdout).context("Clipboard content is not valid UTF-8")
    }

    /// Watch clipboard for changes using polling
    #[allow(dead_code)]
    pub fn watch<F>(mut on_change: F) -> Result<()>
    where
        F: FnMut(String) -> Result<()>,
    {
        use sha2::{Digest, Sha256};
        use std::thread;
        use std::time::Duration;

        let mut last_hash: Option<Vec<u8>> = None;

        loop {
            match Self::paste() {
                Ok(content) if !content.is_empty() => {
                    let mut hasher = Sha256::new();
                    hasher.update(content.as_bytes());
                    let hash = hasher.finalize().to_vec();

                    if last_hash.as_ref() != Some(&hash) {
                        last_hash = Some(hash);
                        on_change(content)?;
                    }
                }
                _ => {}
            }

            thread::sleep(Duration::from_millis(250));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require wl-clipboard to be installed and a Wayland session
    // They are integration tests that actually interact with the system clipboard

    #[test]
    #[ignore] // Run with: cargo test -- --ignored
    fn test_copy_and_paste() {
        let content = "test clipboard content";
        Clipboard::copy(content).unwrap();

        let pasted = Clipboard::paste().unwrap();
        assert_eq!(pasted, content);
    }

    #[test]
    #[ignore]
    fn test_large_content() {
        let content = "x".repeat(500_000); // 500KB
        Clipboard::copy(&content).unwrap();

        let pasted = Clipboard::paste().unwrap();
        assert_eq!(pasted.len(), 500_000);
    }

    #[test]
    #[ignore]
    fn test_unicode_content() {
        let content = "Hello 世界 🎉 émojis";
        Clipboard::copy(content).unwrap();

        let pasted = Clipboard::paste().unwrap();
        assert_eq!(pasted, content);
    }
}

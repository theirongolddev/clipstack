use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

/// Compute SHA256 hash of content and return as raw bytes
pub fn compute_hash(content: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hasher.finalize().to_vec()
}

/// Format bytes into human-readable size
pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Format timestamp as relative time (e.g., "5m ago", "2h ago")
pub fn format_relative_time(timestamp: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    // Clamp to 0 to handle future timestamps (clock skew)
    let diff_secs = ((now - timestamp) / 1000).max(0);

    match diff_secs {
        0..=59 => format!("{}s ago", diff_secs),
        60..=3599 => format!("{}m ago", diff_secs / 60),
        3600..=86399 => format!("{}h ago", diff_secs / 3600),
        _ => format!("{}d ago", diff_secs / 86400),
    }
}

/// The first executable file named `name` in the `PATH`-style list `path`.
/// Empty entries are skipped (not taken as the working dir).
pub fn find_in_path(name: &str, path: &std::ffi::OsStr) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .find(|file| std::fs::metadata(file).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_in_path_finds_executables_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("tool"), "not executable").unwrap();
        std::fs::write(b.join("tool"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(b.join("tool"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir(a.join("dir-tool")).unwrap();
        std::fs::set_permissions(a.join("dir-tool"), std::fs::Permissions::from_mode(0o755)).unwrap();

        let path = std::env::join_paths(["", "/nonexistent", a.to_str().unwrap(), b.to_str().unwrap()]).unwrap();
        assert_eq!(find_in_path("tool", &path), Some(b.join("tool")));
        assert_eq!(find_in_path("dir-tool", &path), None, "directories don't count");
        assert_eq!(find_in_path("missing", &path), None);
        assert_eq!(find_in_path("tool", std::ffi::OsStr::new("")), None);
    }


    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(500), "500B");
        assert_eq!(format_size(1024), "1.0KB");
        assert_eq!(format_size(1536), "1.5KB");
        assert_eq!(format_size(1048576), "1.0MB");
        assert_eq!(format_size(1572864), "1.5MB");
    }

    #[test]
    fn test_format_relative_time() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        // Just now
        assert_eq!(format_relative_time(now), "0s ago");

        // 30 seconds ago
        assert_eq!(format_relative_time(now - 30_000), "30s ago");

        // 5 minutes ago
        assert_eq!(format_relative_time(now - 300_000), "5m ago");

        // 2 hours ago
        assert_eq!(format_relative_time(now - 7_200_000), "2h ago");

        // 3 days ago
        assert_eq!(format_relative_time(now - 259_200_000), "3d ago");
    }

    #[test]
    fn test_format_relative_time_future() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        // Future timestamp (clock skew) should show "0s ago" not negative
        assert_eq!(format_relative_time(now + 60_000), "0s ago");
        assert_eq!(format_relative_time(now + 3_600_000), "0s ago");
    }
}

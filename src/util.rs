use std::time::{SystemTime, UNIX_EPOCH};

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
    let diff_secs = (now - timestamp) / 1000;

    match diff_secs {
        0..=59 => format!("{}s ago", diff_secs),
        60..=3599 => format!("{}m ago", diff_secs / 60),
        3600..=86399 => format!("{}h ago", diff_secs / 3600),
        _ => format!("{}d ago", diff_secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

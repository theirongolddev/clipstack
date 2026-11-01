//! The single ingest path: policy (secret filtering, size caps, retention).

use regex::RegexSet;
use std::time::Duration;

/// Default `[storage] max_entry_bytes`: larger copies are not saved.
pub const DEFAULT_MAX_ENTRY_BYTES: u64 = 10 * 1024 * 1024;
/// Default `[storage] max_total_bytes` of unpinned content.
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// What `ingest` refuses to save and how much history it keeps.
/// Built from the config file by `Config::ingest_policy`.
#[derive(Debug, Clone)]
pub struct IngestPolicy {
    /// User `ignore_patterns` first, then the built-in secret patterns
    ignore: RegexSet,
    user_patterns: usize,
    max_entry_bytes: usize,
    max_total_bytes: u64,
    retention: Option<Duration>,
}

impl Default for IngestPolicy {
    /// No ignore patterns, default size caps, no retention.
    fn default() -> Self {
        Self {
            ignore: RegexSet::empty(),
            user_patterns: 0,
            max_entry_bytes: DEFAULT_MAX_ENTRY_BYTES as usize,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            retention: None,
        }
    }
}

impl IngestPolicy {
    /// `ignore` holds `user_patterns` user patterns followed by any built-ins.
    pub fn new(
        ignore: RegexSet,
        user_patterns: usize,
        max_entry_bytes: usize,
        max_total_bytes: u64,
        retention: Option<Duration>,
    ) -> Self {
        Self { ignore, user_patterns, max_entry_bytes, max_total_bytes, retention }
    }

    /// Index of the first ignore pattern matching `content`.
    pub fn ignored_by(&self, content: &str) -> Option<usize> {
        self.ignore.matches(content).iter().next()
    }

    /// Human name of an ignore pattern index, for skip messages.
    pub fn pattern_label(&self, index: usize) -> String {
        if index < self.user_patterns {
            format!("ignore_patterns[{index}]")
        } else {
            "built-in secret pattern".to_string()
        }
    }

    pub fn max_entry_bytes(&self) -> usize {
        self.max_entry_bytes
    }

    pub fn max_total_bytes(&self) -> u64 {
        self.max_total_bytes
    }

    /// `None` = keep unpinned entries forever.
    pub fn retention(&self) -> Option<Duration> {
        self.retention
    }

    /// Number of user patterns (built-ins excluded).
    pub fn user_pattern_count(&self) -> usize {
        self.user_patterns
    }

    /// Total patterns, user and built-in.
    pub fn pattern_count(&self) -> usize {
        self.ignore.len()
    }
}

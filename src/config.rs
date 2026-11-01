//! Config file: `--config PATH`, else `$XDG_CONFIG_HOME/clipstack/config.toml`.
//!
//! Every key is optional. A missing default file means defaults; a missing
//! `--config` file is an error. Invalid values are config errors (exit code
//! 5), and messages name the bad key or pattern.

pub use crate::daemon::capture::CaptureMode;
use crate::storage::IngestPolicy;
use anyhow::Result;
use serde::Deserialize;
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub storage: StorageConfig,
    pub capture: CaptureConfig,
    pub socket: SocketConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// Unpinned entries older than this are deleted; 0 = keep forever
    pub retention_days: u32,
    pub max_entry_bytes: u64,
    pub max_total_bytes: u64,
    pub ignore_patterns: Vec<String>,
    pub builtin_secret_patterns: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            retention_days: 0,
            max_entry_bytes: crate::storage::DEFAULT_MAX_ENTRY_BYTES,
            max_total_bytes: crate::storage::DEFAULT_MAX_TOTAL_BYTES,
            ignore_patterns: Vec::new(),
            builtin_secret_patterns: true,
        }
    }
}

/// `[capture]`: how the daemon sees new selections.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureConfig {
    pub mode: CaptureMode,
    /// How often polling reads the clipboard (also with `auto` after a fallback)
    pub poll_interval_ms: u64,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self { mode: CaptureMode::Auto, poll_interval_ms: 250 }
    }
}

/// `[socket]`: the daemon's Unix socket for remote copy (and paste).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SocketConfig {
    pub enabled: bool,
    /// Serve `PULL` (rpaste); off by default because it exposes the clipboard
    pub allow_pull: bool,
    /// Per-read/write timeout; a whole request gets twice this
    pub timeout_ms: u64,
}

impl Default for SocketConfig {
    fn default() -> Self {
        Self { enabled: true, allow_pull: false, timeout_ms: 5000 }
    }
}

/// Allowed `[capture] poll_interval_ms`.
pub const POLL_INTERVAL_RANGE_MS: std::ops::RangeInclusive<u64> = 50..=5000;
/// Allowed `[socket] timeout_ms`.
pub const SOCKET_TIMEOUT_RANGE_MS: std::ops::RangeInclusive<u64> = 100..=60_000;

/// Where the config came from, for `status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// `--config PATH`
    Explicit(PathBuf),
    /// The default path, which exists
    File(PathBuf),
    /// No file (the default path, if one could be determined, is missing)
    Defaults(Option<PathBuf>),
}

/// A bad config file or value (exit code 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "config error: {}", self.0)
    }
}

impl std::error::Error for ConfigError {}

/// Largest allowed `max_entry_bytes` (64 MiB).
pub const MAX_ENTRY_BYTES_LIMIT: u64 = 64 * 1024 * 1024;
/// Most user `ignore_patterns`.
pub const MAX_USER_PATTERNS: usize = 64;
/// Compiled-size limit for the pattern set (`RegexSetBuilder::size_limit`).
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// Skipped when `builtin_secret_patterns = true`. The private-key header
/// (PEM, OpenSSH and PGP) matches anywhere; the token formats must be the
/// whole (trimmed) content, so prose or a script that merely contains a
/// token is still saved. README lists these under "Configuration file".
pub const BUILTIN_SECRET_PATTERNS: &[&str] = &[
    r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY( BLOCK)?-----",
    r"\A\s*gh[pousr]_[A-Za-z0-9]{36,255}\s*\z",
    r"\A\s*github_pat_[A-Za-z0-9_]{80,}\s*\z",
    r"\A\s*(AKIA|ASIA)[0-9A-Z]{16}\s*\z",
    r"\A\s*xox[abprs]-[A-Za-z0-9-]{10,}\s*\z",
    r"\A\s*sk-[A-Za-z0-9_-]{20,}\s*\z",
    r"\A\s*sk_(live|test)_[A-Za-z0-9]{20,}\s*\z",
];

impl Config {
    /// The default file: `$XDG_CONFIG_HOME/clipstack/config.toml`.
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("clipstack").join("config.toml"))
    }

    /// Load `explicit`, else the default file (missing => defaults).
    pub fn load(explicit: Option<&Path>) -> Result<(Config, ConfigSource)> {
        match explicit {
            Some(path) => Self::load_path(path, true),
            None => match Self::default_path() {
                Some(path) => Self::load_path(&path, false),
                None => Ok((Config::default(), ConfigSource::Defaults(None))),
            },
        }
    }

    /// Load one file; `explicit` decides whether a missing file is an error.
    /// The policy is compiled once here so a bad pattern fails at load time.
    pub fn load_path(path: &Path, explicit: bool) -> Result<(Config, ConfigSource)> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => {
                return Ok((Config::default(), ConfigSource::Defaults(Some(path.to_path_buf()))));
            }
            Err(e) => {
                return Err(ConfigError(format!("cannot read {}: {e}", path.display())).into());
            }
        };
        let config = Self::parse(&text)
            .and_then(|c| c.check_daemon_sections().map(|()| c))
            .and_then(|c| c.ingest_policy().map(|_| c))
            .map_err(|e| ConfigError(format!("{}: {}", path.display(), e.0)))?;
        let source = if explicit {
            ConfigSource::Explicit(path.to_path_buf())
        } else {
            ConfigSource::File(path.to_path_buf())
        };
        Ok((config, source))
    }

    /// Parse config text (unknown keys are rejected).
    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError(e.to_string().trim_end().to_string()))
    }

    /// Validate the `[capture]` and `[socket]` sections.
    pub fn check_daemon_sections(&self) -> Result<(), ConfigError> {
        let interval = self.capture.poll_interval_ms;
        if !POLL_INTERVAL_RANGE_MS.contains(&interval) {
            return Err(ConfigError(format!(
                "[capture] poll_interval_ms = {interval} must be between {} and {}",
                POLL_INTERVAL_RANGE_MS.start(),
                POLL_INTERVAL_RANGE_MS.end()
            )));
        }
        let timeout = self.socket.timeout_ms;
        if !SOCKET_TIMEOUT_RANGE_MS.contains(&timeout) {
            return Err(ConfigError(format!(
                "[socket] timeout_ms = {timeout} must be between {} and {}",
                SOCKET_TIMEOUT_RANGE_MS.start(),
                SOCKET_TIMEOUT_RANGE_MS.end()
            )));
        }
        Ok(())
    }

    /// Validate the `[storage]` section and compile its patterns.
    pub fn ingest_policy(&self) -> Result<IngestPolicy, ConfigError> {
        let s = &self.storage;
        if !(1..=MAX_ENTRY_BYTES_LIMIT).contains(&s.max_entry_bytes) {
            return Err(ConfigError(format!(
                "[storage] max_entry_bytes = {} must be between 1 and {MAX_ENTRY_BYTES_LIMIT} (64 MiB)",
                s.max_entry_bytes
            )));
        }
        if s.max_total_bytes < s.max_entry_bytes {
            return Err(ConfigError(format!(
                "[storage] max_total_bytes = {} must be at least max_entry_bytes = {}",
                s.max_total_bytes, s.max_entry_bytes
            )));
        }
        if s.ignore_patterns.len() > MAX_USER_PATTERNS {
            return Err(ConfigError(format!(
                "[storage] ignore_patterns has {} patterns; at most {MAX_USER_PATTERNS} are allowed",
                s.ignore_patterns.len()
            )));
        }
        // Compile each user pattern alone first so the error names it
        for (i, pattern) in s.ignore_patterns.iter().enumerate() {
            regex::RegexBuilder::new(pattern)
                .size_limit(REGEX_SIZE_LIMIT)
                .build()
                .map_err(|e| {
                    ConfigError(format!("[storage] ignore_patterns[{i}] {pattern:?} is invalid: {e}"))
                })?;
        }

        let builtins = if s.builtin_secret_patterns { BUILTIN_SECRET_PATTERNS } else { &[] };
        let patterns = s.ignore_patterns.iter().map(String::as_str).chain(builtins.iter().copied());
        let set = regex::RegexSetBuilder::new(patterns)
            .size_limit(REGEX_SIZE_LIMIT)
            .build()
            .map_err(|e| ConfigError(format!("[storage] ignore_patterns are too large together: {e}")))?;

        let retention = (s.retention_days > 0)
            .then(|| std::time::Duration::from_secs(u64::from(s.retention_days) * 86_400));
        Ok(IngestPolicy::new(
            set,
            s.ignore_patterns.len(),
            s.max_entry_bytes as usize,
            s.max_total_bytes,
            retention,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn policy(toml: &str) -> IngestPolicy {
        Config::parse(toml).unwrap().ingest_policy().unwrap()
    }

    fn github_token() -> String {
        // Built at runtime so the source never contains a token-shaped literal
        format!("ghp_{}", "aB3".repeat(12))
    }

    fn private_key() -> String {
        format!(
            "-----BEGIN OPENSSH {} KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH {} KEY-----\n",
            "PRIVATE", "PRIVATE"
        )
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("clipstack").join("config.toml");
        let (config, source) = Config::load_path(&path, false).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(source, ConfigSource::Defaults(Some(path.clone())));
        assert_eq!(config.storage.retention_days, 0, "retention is off by default");
        assert!(config.storage.builtin_secret_patterns);

        // An explicit --config that doesn't exist is an error, not defaults
        let err = Config::load_path(&path, true).unwrap_err();
        assert!(err.to_string().contains("config.toml"), "{err}");
    }

    #[test]
    fn parses_storage_section() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[storage]\nretention_days = 30\nmax_entry_bytes = 1024\nmax_total_bytes = 4096\n\
             ignore_patterns = ['^secret:']\nbuiltin_secret_patterns = false\n",
        )
        .unwrap();
        let (config, source) = Config::load_path(&path, true).unwrap();
        assert_eq!(source, ConfigSource::Explicit(path));
        assert_eq!(
            config.storage,
            StorageConfig {
                retention_days: 30,
                max_entry_bytes: 1024,
                max_total_bytes: 4096,
                ignore_patterns: vec!["^secret:".to_string()],
                builtin_secret_patterns: false,
            }
        );

        let policy = config.ingest_policy().unwrap();
        assert_eq!(policy.retention(), Some(std::time::Duration::from_secs(30 * 86_400)));
        assert_eq!(policy.max_entry_bytes(), 1024);
        assert_eq!(policy.max_total_bytes(), 4096);
        assert_eq!(policy.pattern_count(), 1, "built-ins disabled");
        assert_eq!(policy.ignored_by("secret: hunter2"), Some(0));
        assert_eq!(policy.pattern_label(0), "ignore_patterns[0]");
    }

    #[test]
    fn capture_and_socket_defaults() {
        let config = Config::default();
        assert_eq!(config.capture, CaptureConfig { mode: CaptureMode::Auto, poll_interval_ms: 250 });
        assert_eq!(
            config.socket,
            SocketConfig { enabled: true, allow_pull: false, timeout_ms: 5000 }
        );
        assert_eq!(Config::parse("").unwrap(), config);
    }

    #[test]
    fn parses_capture_and_socket_sections() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[capture]\nmode = \"poll\"\npoll_interval_ms = 50\n\
             [socket]\nenabled = false\nallow_pull = true\ntimeout_ms = 200\n",
        )
        .unwrap();
        let (config, _) = Config::load_path(&path, true).unwrap();
        assert_eq!(config.capture, CaptureConfig { mode: CaptureMode::Poll, poll_interval_ms: 50 });
        assert_eq!(config.socket, SocketConfig { enabled: false, allow_pull: true, timeout_ms: 200 });
        for (text, mode) in [("auto", CaptureMode::Auto), ("watch", CaptureMode::Watch)] {
            let config = Config::parse(&format!("[capture]\nmode = \"{text}\"\n")).unwrap();
            assert_eq!(config.capture.mode, mode);
        }
        let err = Config::parse("[capture]\nmode = \"inotify\"\n").unwrap_err();
        assert!(err.to_string().contains("inotify"), "{err}");
        let err = Config::parse("[socket]\nport = 7779\n").unwrap_err();
        assert!(err.to_string().contains("port"), "{err}");
    }

    #[test]
    fn rejects_poll_interval_and_timeout_out_of_range() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        for (text, key) in [
            ("[capture]\npoll_interval_ms = 49\n", "poll_interval_ms"),
            ("[capture]\npoll_interval_ms = 5001\n", "poll_interval_ms"),
            ("[socket]\ntimeout_ms = 99\n", "timeout_ms"),
            ("[socket]\ntimeout_ms = 60001\n", "timeout_ms"),
        ] {
            std::fs::write(&path, text).unwrap();
            let err = Config::load_path(&path, true).unwrap_err();
            assert!(err.to_string().contains(key), "{text}: {err}");
        }
        for text in ["[capture]\npoll_interval_ms = 50\n", "[capture]\npoll_interval_ms = 5000\n",
                     "[socket]\ntimeout_ms = 100\n", "[socket]\ntimeout_ms = 60000\n"] {
            std::fs::write(&path, text).unwrap();
            assert!(Config::load_path(&path, true).is_ok(), "{text}");
        }
    }

    #[test]
    fn rejects_unknown_key() {
        let err = Config::parse("[storage]\nretention_dayz = 3\n").unwrap_err();
        assert!(err.to_string().contains("retention_dayz"), "{err}");
        let err = Config::parse("[storag]\n").unwrap_err();
        assert!(err.to_string().contains("storag"), "{err}");
    }

    #[test]
    fn invalid_regex_error_names_pattern() {
        let config = Config::parse("[storage]\nignore_patterns = ['ok', '(unclosed']\n").unwrap();
        let err = config.ingest_policy().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ignore_patterns[1]"), "{msg}");
        assert!(msg.contains("(unclosed"), "{msg}");

        let many: Vec<String> = (0..65).map(|i| format!("'p{i}'")).collect();
        let text = format!("[storage]\nignore_patterns = [{}]\n", many.join(", "));
        let err = Config::parse(&text).unwrap().ingest_policy().unwrap_err();
        assert!(err.to_string().contains("64"), "{err}");
    }

    #[test]
    fn rejects_total_cap_below_entry_cap() {
        let err = Config::parse("[storage]\nmax_entry_bytes = 2048\nmax_total_bytes = 1024\n")
            .unwrap()
            .ingest_policy()
            .unwrap_err();
        assert!(err.to_string().contains("max_total_bytes"), "{err}");

        for bad in ["0", "67108865"] {
            let text = format!("[storage]\nmax_entry_bytes = {bad}\nmax_total_bytes = 999999999\n");
            let err = Config::parse(&text).unwrap().ingest_policy().unwrap_err();
            assert!(err.to_string().contains("max_entry_bytes"), "{bad}: {err}");
        }
        // The 64 MiB upper bound itself is allowed
        let text = "[storage]\nmax_entry_bytes = 67108864\nmax_total_bytes = 67108864\n";
        assert!(Config::parse(text).unwrap().ingest_policy().is_ok());
    }

    #[test]
    fn builtin_patterns_match_github_token_and_private_key() {
        let policy = policy("");
        let user = policy.user_pattern_count();
        assert_eq!(user, 0);
        for secret in [
            github_token(),
            format!("  {}\n", github_token()),
            private_key(),
            format!("key follows:\n{}", private_key()),
            format!("github_pat_{}", "A1_b".repeat(21)),
            format!("AKIA{}", "Q7ZX".repeat(4)),
            format!("xoxb-{}", "1234-abcd-".repeat(2)),
            format!("sk-{}", "Ab9_-".repeat(5)),
            format!("-----BEGIN PGP {} KEY BLOCK-----\nlQOYBF\n", "PRIVATE"),
            format!("sk_{}_{}", "live", "4eC39HqLyjWDarjtT1zdp7dc"),
            format!(" sk_{}_{}\n", "test", "Ab12".repeat(5)),
            format!("ASIA{}", "Q7ZX".repeat(4)),
        ] {
            let index = policy.ignored_by(&secret);
            assert!(index.is_some(), "should skip {secret:?}");
            assert_eq!(policy.pattern_label(index.unwrap()), "built-in secret pattern");
        }
    }

    #[test]
    fn builtin_patterns_ignore_prose_mentioning_token() {
        let policy = policy("");
        for prose in [
            format!("my old token {} was revoked", github_token()),
            "tokens look like ghp_ followed by 36 characters".to_string(),
            format!("export AWS_KEY=AKIA{}", "Q7ZX".repeat(4)),
            "sk-short".to_string(),
            "the BEGIN PRIVATE KEY header".to_string(),
            "-----BEGIN PGP PUBLIC KEY BLOCK-----\nmQINBF\n".to_string(),
            format!("stripe key sk_{}_{} in prose", "live", "Ab12".repeat(6)),
            format!("sk_{}_{}", "live", "tooShort12"),
            format!("ASIA{} is a session key", "Q7ZX".repeat(4)),
            // A token inside a longer script is saved; only a bare token is skipped
            format!("#!/bin/sh\nexport GITHUB_TOKEN={}\nexec gh pr list\n", github_token()),
        ] {
            assert_eq!(policy.ignored_by(&prose), None, "should keep {prose:?}");
        }
    }
}

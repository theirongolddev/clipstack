//! Command-line definitions (clap) and argument resolution.

use crate::clipboard::ClipboardError;
use crate::config::ConfigError;
use crate::storage::{EntryId, StorageError, is_valid_alias};
use clap::{Parser, Subcommand};
use clap_complete::Shell;
use std::fmt;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Parser)]
#[command(name = "clipstack")]
#[command(about = "Fast clipboard manager with lazy-loading history")]
#[command(version)]
pub struct Cli {
    /// Custom storage directory
    #[arg(long, global = true)]
    pub storage_dir: Option<PathBuf>,

    /// Config file (default: $XDG_CONFIG_HOME/clipstack/config.toml)
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Maximum entries to store (1-10000, default: 100)
    /// Can also be set via CLIPSTACK_MAX_ENTRIES environment variable
    #[arg(long, global = true, value_parser = clap::value_parser!(u32).range(1..=10000))]
    pub max_entries: Option<u32>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Copy stdin to clipboard
    Copy,

    /// Paste clipboard to stdout
    Paste,

    /// Open picker UI to select from history
    Pick,

    /// List clipboard history
    List {
        /// Number of entries to show
        #[arg(short, long, default_value = "10")]
        count: usize,
    },

    /// Print one entry's content (no newline added); --copy also puts it on the clipboard
    Get {
        /// N (position, 1 = newest), id:ID or @alias
        #[arg(value_name = "REF")]
        entry: EntryRef,

        /// Also copy it to the clipboard and make it the newest entry
        #[arg(long)]
        copy: bool,
    },

    /// Clear clipboard history
    Clear,

    /// Run the clipboard monitoring daemon
    Daemon,

    /// Stop the running daemon
    Kill,

    /// Show storage statistics
    Stats,

    /// Check daemon status and system health
    Status,

    /// Check the database and search index; --repair rebuilds a damaged index
    Check {
        /// Rebuild a damaged search index and checkpoint the WAL
        #[arg(long)]
        repair: bool,
    },

    /// Old name for `check --repair`
    #[command(hide = true)]
    Recover,

    /// Write the whole history as JSON Lines, oldest first
    Export {
        /// Write to FILE (created 0600) instead of stdout
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,

        /// Overwrite FILE if it exists
        #[arg(long, requires = "output")]
        force: bool,
    },

    /// Removed TCP server: prints where the daemon's socket is and exits 2
    #[command(hide = true, disable_help_flag = true)]
    Serve {
        /// Old flags (`--port`) are accepted and ignored
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
        _args: Vec<String>,
    },

    /// Run by `wl-paste --watch` once per new selection (internal)
    #[command(name = "__watch-hook", hide = true)]
    WatchHook,

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },
}

/// Which entry a command means: `N` (1-based position, newest first),
/// `id:ID` (the stable entry id) or `@alias`. Not `#ID`: a shell drops a
/// word starting with `#` as a comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryRef {
    Position(NonZeroUsize),
    Id(EntryId),
    Alias(String),
}

impl FromStr for EntryRef {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        const FORMS: &str = "use N (position, 1 = newest), id:ID or @alias";
        /// Non-empty and all ASCII digits (`usize::from_str` also takes `+3`).
        fn all_digits(s: &str) -> bool {
            !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
        }
        if let Some(name) = s.strip_prefix('@') {
            return if is_valid_alias(name) {
                Ok(EntryRef::Alias(name.to_string()))
            } else {
                Err(format!(
                    "invalid alias {s:?}: 1-32 characters from a-z, 0-9, '_' and '-', \
                     starting with a letter or digit"
                ))
            };
        }
        if let Some(digits) = s.strip_prefix("id:") {
            return match digits.parse::<EntryId>() {
                Ok(id) if id > 0 && all_digits(digits) => Ok(EntryRef::Id(id)),
                _ => Err(format!("invalid entry id {s:?}: {FORMS}")),
            };
        }
        if let Some(id) = s.strip_prefix('#').filter(|id| all_digits(id)) {
            return Err(format!("{s:?}: write id:{id} (a shell drops words starting with #)"));
        }
        match s.parse::<usize>().ok().filter(|_| all_digits(s)).map(NonZeroUsize::new) {
            Some(Some(n)) => Ok(EntryRef::Position(n)),
            Some(None) => Err(format!("position 0: positions start at 1 (the newest entry); {FORMS}")),
            None => Err(format!("invalid entry {s:?}: {FORMS}")),
        }
    }
}

impl fmt::Display for EntryRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntryRef::Position(n) => write!(f, "{n}"),
            EntryRef::Id(id) => write!(f, "id:{id}"),
            EntryRef::Alias(name) => write!(f, "@{name}"),
        }
    }
}

/// Exit status, stable for scripts (README "Scripting"). 3 (daemon or
/// socket unavailable) is used by `rcopy` and `rpaste`; no clipstack
/// subcommand needs the daemon, so the binary never exits 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Exit {
    /// Not found or no match (grep-like); also refused (alias taken, pin
    /// limit) and any failure not listed below
    NotFound = 1,
    /// Bad arguments (clap exits 2 itself for parse errors)
    Usage = 2,
    /// Storage error: locked, corrupt, newer schema
    Storage = 4,
    /// Config file error
    Config = 5,
    /// wl-copy or wl-paste failed
    Clipboard = 6,
}

impl From<Exit> for std::process::ExitCode {
    fn from(exit: Exit) -> Self {
        std::process::ExitCode::from(exit as u8)
    }
}

/// An error that carries its exit status. An empty message prints nothing
/// (`search` with no match exits 1 silently, like grep).
#[derive(Debug)]
pub struct CliError {
    pub code: Exit,
    pub msg: String,
}

impl CliError {
    pub fn new(code: Exit, msg: impl Into<String>) -> Self {
        Self { code, msg: msg.into() }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for CliError {}

/// The exit status for an error: the first cause in its chain that has one.
/// Anything unrecognised exits 1, as every error did before Step 3.
pub fn exit_code_for(err: &anyhow::Error) -> Exit {
    err.chain().find_map(cause_exit).unwrap_or(Exit::NotFound)
}

fn cause_exit(cause: &(dyn std::error::Error + 'static)) -> Option<Exit> {
    if let Some(e) = cause.downcast_ref::<CliError>() {
        return Some(e.code);
    }
    if cause.is::<ConfigError>() {
        return Some(Exit::Config);
    }
    if cause.is::<ClipboardError>() {
        return Some(Exit::Clipboard);
    }
    if let Some(e) = cause.downcast_ref::<StorageError>() {
        return Some(match e {
            StorageError::NewerSchema { .. } | StorageError::Corrupt(_) => Exit::Storage,
            StorageError::InvalidAlias(_) => Exit::Usage,
            StorageError::NotFound { .. } | StorageError::PinLimit { .. } | StorageError::AliasTaken(_) => {
                Exit::NotFound
            }
        });
    }
    // Locked (busy past the timeout), I/O or corruption reported by SQLite
    cause.is::<rusqlite::Error>().then_some(Exit::Storage)
}

/// Where the effective max_entries limit came from
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LimitSource {
    Cli,
    Env,
    /// Not configured this run: reuse the limit saved in the database (or default)
    Stored,
}

impl LimitSource {
    /// `limit_saved`: whether the database holds a saved limit
    pub fn label(self, limit_saved: bool) -> &'static str {
        match self {
            LimitSource::Cli => "cli",
            LimitSource::Env => "env",
            LimitSource::Stored if limit_saved => "saved",
            LimitSource::Stored => "default",
        }
    }
}

/// Resolve the explicit entry limit: CLI flag > env var > none (use stored).
/// Invalid env values are ignored rather than silently resetting the limit.
pub fn resolve_max_entries(cli: Option<u32>, env: Option<&str>) -> (Option<usize>, LimitSource) {
    if let Some(n) = cli {
        return (Some((n as usize).clamp(1, 10000)), LimitSource::Cli);
    }
    if let Some(n) = env.and_then(|s| s.trim().parse::<usize>().ok()) {
        return (Some(n.clamp(1, 10000)), LimitSource::Env);
    }
    (None, LimitSource::Stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage;

    #[test]
    fn test_resolve_limit_prefers_cli_over_env() {
        assert_eq!(resolve_max_entries(Some(500), Some("42")), (Some(500), LimitSource::Cli));
    }

    #[test]
    fn test_resolve_limit_uses_env_when_no_cli() {
        assert_eq!(resolve_max_entries(None, Some("42")), (Some(42), LimitSource::Env));
    }

    #[test]
    fn test_resolve_limit_clamps_env() {
        assert_eq!(resolve_max_entries(None, Some("0")), (Some(1), LimitSource::Env));
        assert_eq!(resolve_max_entries(None, Some("99999")), (Some(10000), LimitSource::Env));
    }

    #[test]
    fn test_resolve_limit_ignores_invalid_env() {
        // Unparseable env must not override the stored limit
        assert_eq!(resolve_max_entries(None, Some("lots")), (None, LimitSource::Stored));
    }

    #[test]
    fn test_limit_label_distinguishes_saved_from_default() {
        assert_eq!(LimitSource::Stored.label(true), "saved");
        assert_eq!(LimitSource::Stored.label(false), "default");
        assert_eq!(LimitSource::Cli.label(false), "cli");
        assert_eq!(LimitSource::Env.label(true), "env");
    }

    #[test]
    fn limit_label_saved_when_settings_row_exists() {
        let dir = tempfile::TempDir::new().unwrap();
        let open = |limit| {
            storage::Storage::open(dir.path().to_path_buf(), limit, storage::IngestPolicy::default())
                .unwrap()
        };
        assert_eq!(LimitSource::Stored.label(open(None).limit_is_saved()), "default");
        open(Some(5));
        assert_eq!(LimitSource::Stored.label(open(None).limit_is_saved()), "saved");
    }

    #[test]
    fn parses_position_id_alias() {
        let parse = |s: &str| s.parse::<EntryRef>();
        assert_eq!(parse("1"), Ok(EntryRef::Position(NonZeroUsize::new(1).unwrap())));
        assert_eq!(parse("42"), Ok(EntryRef::Position(NonZeroUsize::new(42).unwrap())));
        assert_eq!(parse("id:42"), Ok(EntryRef::Id(42)));
        assert_eq!(parse("@sig"), Ok(EntryRef::Alias("sig".into())));
        assert_eq!(parse("@my-alias_2"), Ok(EntryRef::Alias("my-alias_2".into())));
        // Display gives back the form that was parsed (used in messages)
        for s in ["7", "id:42", "@sig"] {
            assert_eq!(parse(s).unwrap().to_string(), s);
        }
    }

    #[test]
    fn rejects_zero_position() {
        let err = "0".parse::<EntryRef>().unwrap_err();
        assert!(err.contains("1"), "{err}");
        assert!("id:0".parse::<EntryRef>().is_err());
        assert!("id:-3".parse::<EntryRef>().is_err());
        assert!("-1".parse::<EntryRef>().is_err());
    }

    #[test]
    fn rejects_invalid_alias_chars() {
        for bad in ["@", "@Sig", "@a b", "@-lead", "@a.b", &format!("@{}", "a".repeat(33))] {
            assert!(bad.parse::<EntryRef>().is_err(), "{bad:?} accepted");
        }
        for bad in ["", "abc", "id:", "id:x", "+3", " 3"] {
            assert!(bad.parse::<EntryRef>().is_err(), "{bad:?} accepted");
        }
        // `#42` is what a shell turns into a comment; say what to use instead
        let err = "#42".parse::<EntryRef>().unwrap_err();
        assert!(err.contains("id:42"), "{err}");
    }

    #[test]
    fn exit_code_for_storage_config_clipboard_errors() {
        use crate::clipboard::ClipboardError;
        use crate::config::ConfigError;
        use storage::StorageError;
        let code = |err: anyhow::Error| exit_code_for(&err);

        assert_eq!(code(StorageError::NewerSchema { found: 2, supported: 1 }.into()), Exit::Storage);
        assert_eq!(code(StorageError::Corrupt("damaged".into()).into()), Exit::Storage);
        // Found through added context too
        let wrapped = anyhow::Error::new(StorageError::Corrupt("damaged".into())).context("opening");
        assert_eq!(code(wrapped), Exit::Storage);
        let busy = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY), None);
        assert_eq!(code(anyhow::Error::new(busy).context("Failed to apply storage limits")), Exit::Storage);

        assert_eq!(code(ConfigError("bad key".into()).into()), Exit::Config);
        assert_eq!(code(ClipboardError::from(anyhow::anyhow!("wl-copy failed")).into()), Exit::Clipboard);

        assert_eq!(code(CliError::new(Exit::NotFound, "no entry for 3").into()), Exit::NotFound);
        assert_eq!(code(CliError::new(Exit::Usage, "bad").into()), Exit::Usage);
        assert_eq!(code(StorageError::NotFound { id: 3 }.into()), Exit::NotFound);
        assert_eq!(code(StorageError::PinLimit { max: 25 }.into()), Exit::NotFound);
        assert_eq!(code(StorageError::AliasTaken("x".into()).into()), Exit::NotFound);
        assert_eq!(code(StorageError::InvalidAlias("X".into()).into()), Exit::Usage);
        // Anything else keeps the old exit status 1
        assert_eq!(code(anyhow::anyhow!("something else")), Exit::NotFound);
    }

    #[test]
    fn test_resolve_limit_none_means_stored() {
        assert_eq!(resolve_max_entries(None, None), (None, LimitSource::Stored));
    }
}

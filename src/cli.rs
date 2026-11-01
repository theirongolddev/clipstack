//! Command-line definitions (clap) and argument resolution.

use clap::{Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;

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
    fn test_resolve_limit_none_means_stored() {
        assert_eq!(resolve_max_entries(None, None), (None, LimitSource::Stored));
    }
}

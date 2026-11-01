mod clipboard;
mod config;
mod daemon;
mod perms;
mod picker;
mod storage;
mod util;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::net::TcpListener;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use storage::{IngestOutcome, ListOrder, Source};

#[derive(Parser)]
#[command(name = "clipstack")]
#[command(about = "Fast clipboard manager with lazy-loading history")]
#[command(version)]
struct Cli {
    /// Custom storage directory
    #[arg(long, global = true)]
    storage_dir: Option<PathBuf>,

    /// Config file (default: $XDG_CONFIG_HOME/clipstack/config.toml)
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Maximum entries to store (1-10000, default: 100)
    /// Can also be set via CLIPSTACK_MAX_ENTRIES environment variable
    #[arg(long, global = true, value_parser = clap::value_parser!(u32).range(1..=10000))]
    max_entries: Option<u32>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
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

    /// Start a TCP server for remote clipboard (use with SSH reverse tunnel)
    Serve {
        /// Port to listen on
        #[arg(short, long, default_value = "7779")]
        port: u16,
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    if matches!(cli.command, Some(Commands::WatchHook)) {
        // Always exit 0: a failing command would make wl-paste --watch stop
        if let Err(e) = watch_hook(&cli) {
            eprintln!("[watch] {e:#}");
            let _ = io::copy(&mut io::stdin().lock(), &mut io::sink());
        }
        return Ok(());
    }

    // Check dependencies on commands that need clipboard access
    if matches!(
        cli.command,
        None | Some(Commands::Pick)
            | Some(Commands::Copy)
            | Some(Commands::Paste)
            | Some(Commands::Daemon)
    ) {
        check_dependencies()?;
    }

    // Determine max_entries: CLI > env > limit saved in the database (> default)
    let env_limit = std::env::var("CLIPSTACK_MAX_ENTRIES").ok();
    let (max_entries, limit_source) = resolve_max_entries(cli.max_entries, env_limit.as_deref());

    // The config is loaded (and its patterns compiled) before storage is opened
    let (config, config_source) = config::Config::load(cli.config.as_deref())?;
    let policy = config.ingest_policy()?;

    let storage_dir = cli.storage_dir.unwrap_or_else(storage::Storage::default_dir);
    let storage = storage::Storage::open(storage_dir, max_entries, policy)?;

    match cli.command {
        None | Some(Commands::Pick) => {
            // Default action: open picker
            picker::pick_and_paste(storage)?;
        }

        Some(Commands::Copy) => {
            let mut content = String::new();
            io::stdin().read_to_string(&mut content)?;

            clipboard::Clipboard::system().copy(&content)?;
            match storage.ingest(&content, Source::Copy, &[])? {
                IngestOutcome::Skipped(reason) => eprintln!(
                    "Copied {} bytes (not saved to history: {})",
                    content.len(),
                    storage.policy().describe_skip(&reason)
                ),
                IngestOutcome::Inserted(_) | IngestOutcome::Bumped(_) => {
                    eprintln!("Copied {} bytes", content.len());
                }
            }
        }

        Some(Commands::Paste) => {
            let content = clipboard::Clipboard::system().paste_text()?;
            io::stdout().write_all(content.as_bytes())?;
        }

        Some(Commands::List { count }) => {
            let entries = storage.list(ListOrder::Recency, Some(count))?;
            let total = storage.counts()?.total;

            for entry in &entries {
                let time = util::format_relative_time(entry.last_used);
                let size = util::format_size(entry.size);
                let preview: String = entry
                    .preview
                    .chars()
                    .take(50)
                    .collect::<String>()
                    .replace('\n', " ");

                println!("{:>5} [{:>6}] {}", time, size, preview);
            }

            if total > entries.len() {
                println!("... and {} more", total - entries.len());
            }
        }

        Some(Commands::Clear) => {
            let cleared = storage.clear()?;
            println!("Clipboard history cleared ({cleared} entries)");
        }

        Some(Commands::Daemon) => {
            let options = daemon::DaemonOptions {
                capture: config.capture.clone(),
                socket: config.socket.clone(),
                clipboard: clipboard::Clipboard::system(),
                hook: watch_hook_command(storage.base_dir(), cli.config.as_deref())?,
            };
            // The lock file is global even with a custom storage dir
            let daemon = daemon::Daemon::new(storage, options)?;

            // Stop cleanly on SIGINT, SIGTERM and SIGHUP (ctrlc "termination")
            let running = daemon.stop_handle();
            ctrlc_handler(running);

            daemon.run()?;
        }

        Some(Commands::Kill) => {
            if !daemon::Daemon::is_running() {
                println!("Daemon is not running");
                return Ok(());
            }

            // Signal the exact PID recorded in the daemon's lock file
            let Some(pid) = daemon::Daemon::running_pid() else {
                eprintln!("Daemon is running but its PID is unknown (started by an older version?)");
                eprintln!("Stop it manually, e.g.: pkill -x clipstack");
                std::process::exit(1);
            };

            match Command::new("kill").arg(pid.to_string()).status() {
                Ok(s) if s.success() => {
                    // Wait briefly for the lock to be released
                    for _ in 0..20 {
                        if !daemon::Daemon::is_running() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    println!("Daemon stopped (pid {})", pid);
                }
                Ok(_) => {
                    eprintln!("Failed to signal daemon (pid {})", pid);
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("Failed to kill daemon: {}", e);
                    std::process::exit(1);
                }
            }
        }

        Some(Commands::Stats) => {
            let counts = storage.counts()?;

            println!("Entries:     {}", counts.total);
            println!("  Pinned:    {} (protected)", counts.pinned);
            println!(
                "  Regular:   {}/{} ({})",
                counts.total - counts.pinned,
                storage.max_entries(),
                limit_source.label(storage.limit_is_saved())
            );
            println!("Total size:  {}", util::format_size(usize::try_from(counts.total_bytes)?));

            if let Some(oldest) = counts.oldest {
                println!("Oldest:      {}", util::format_relative_time(oldest));
            }
            if let Some(newest) = counts.newest {
                println!("Newest:      {}", util::format_relative_time(newest));
            }
        }

        Some(Commands::Status) => {
            print_status(&storage, limit_source, &config, &config_source)?;
        }

        Some(Commands::Check { repair }) => {
            let report = storage.check(repair)?;
            print_check(&storage, &report)?;
            check_result(&storage, &report)?;
        }

        Some(Commands::Recover) => {
            let report = storage.check(true)?;
            check_result(&storage, &report)?;
            println!("Recovery complete. {} entries.", report.counts.total);
        }

        Some(Commands::Export { output, force }) => match output {
            Some(path) => {
                let mut file = BufWriter::new(create_export_file(&path, force)?);
                let n = storage.export_jsonl(&mut file)?;
                file.into_inner()
                    .map_err(|e| e.into_error())?
                    .sync_all()
                    .with_context(|| format!("Failed to write {}", path.display()))?;
                eprintln!("Exported {n} entries to {}", path.display());
            }
            None => {
                let mut out = BufWriter::new(io::stdout().lock());
                if let Err(e) = storage.export_jsonl(&mut out)
                    && !is_broken_pipe(&e)
                {
                    return Err(e);
                }
            }
        },

        Some(Commands::Serve { port }) => {
            serve_clipboard(storage, port)?;
        }

        Some(Commands::Completions { shell }) => {
            generate_completions(shell);
        }

        Some(Commands::WatchHook) => unreachable!("handled before storage is opened"),
    }

    Ok(())
}

/// The command `wl-paste --watch` runs per selection: this executable with
/// the daemon's storage dir and config file (absolute, so the hook's working
/// dir doesn't matter) and `__watch-hook`. The environment is inherited, so
/// `CLIPSTACK_MAX_ENTRIES` resolves the same in the hook.
fn watch_hook_command(storage_dir: &Path, config: Option<&Path>) -> Result<Vec<OsString>> {
    let exe = std::env::current_exe().context("Cannot find the clipstack executable")?;
    // After an upgrade replaced the binary, /proc/self/exe reads "<path> (deleted)"
    let exe = match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(path) => PathBuf::from(path),
        None => exe,
    };
    let mut args = vec![
        exe.into_os_string(),
        "--storage-dir".into(),
        std::path::absolute(storage_dir)?.into_os_string(),
    ];
    if let Some(config) = config {
        args.push("--config".into());
        args.push(std::path::absolute(config)?.into_os_string());
    }
    args.push("__watch-hook".into());
    Ok(args)
}

/// `__watch-hook`: save the selection wl-paste passes on stdin.
fn watch_hook(cli: &Cli) -> Result<()> {
    let env_limit = std::env::var("CLIPSTACK_MAX_ENTRIES").ok();
    let (max_entries, _) = resolve_max_entries(cli.max_entries, env_limit.as_deref());
    let (config, _) = config::Config::load(cli.config.as_deref())?;
    let storage_dir = cli.storage_dir.clone().unwrap_or_else(storage::Storage::default_dir);
    let storage = storage::Storage::open(storage_dir, max_entries, config.ingest_policy()?)?;
    let state = std::env::var("CLIPBOARD_STATE").ok();
    daemon::capture::run_hook(
        &storage,
        &clipboard::Clipboard::system(),
        state.as_deref(),
        &mut io::stdin().lock(),
    )
}

/// Where the effective max_entries limit came from
#[derive(Clone, Copy, Debug, PartialEq)]
enum LimitSource {
    Cli,
    Env,
    /// Not configured this run: reuse the limit saved in the database (or default)
    Stored,
}

impl LimitSource {
    /// `limit_saved`: whether the database holds a saved limit
    fn label(self, limit_saved: bool) -> &'static str {
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
fn resolve_max_entries(cli: Option<u32>, env: Option<&str>) -> (Option<usize>, LimitSource) {
    if let Some(n) = cli {
        return (Some((n as usize).clamp(1, 10000)), LimitSource::Cli);
    }
    if let Some(n) = env.and_then(|s| s.trim().parse::<usize>().ok()) {
        return (Some(n.clamp(1, 10000)), LimitSource::Env);
    }
    (None, LimitSource::Stored)
}

/// Check if required dependencies (wl-clipboard) are installed
fn check_dependencies() -> Result<()> {
    // Check for wl-paste
    let wl_paste_check = Command::new("which").arg("wl-paste").output();

    match wl_paste_check {
        Ok(output) if output.status.success() => Ok(()),
        _ => {
            eprintln!("Error: wl-clipboard not found");
            eprintln!();
            eprintln!("ClipStack requires wl-clipboard for Wayland clipboard access.");
            eprintln!();
            eprintln!("Install it with:");
            eprintln!("  Arch:   sudo pacman -S wl-clipboard");
            eprintln!("  Debian: sudo apt install wl-clipboard");
            eprintln!("  Fedora: sudo dnf install wl-clipboard");
            eprintln!();
            eprintln!("Also ensure you're running in a Wayland session:");
            eprintln!("  echo $WAYLAND_DISPLAY");
            std::process::exit(1);
        }
    }
}

/// Print daemon and system status
fn print_status(
    storage: &storage::Storage,
    limit_source: LimitSource,
    config: &config::Config,
    config_source: &config::ConfigSource,
) -> Result<()> {
    // Check daemon status
    let daemon_running = daemon::Daemon::is_running();

    if daemon_running {
        println!("Daemon:  \x1b[32mrunning\x1b[0m");
    } else {
        println!("Daemon:  \x1b[33mnot running\x1b[0m");
        println!("         Start with: clipstack daemon");
        println!("         Or just run: clipstack (auto-starts daemon)");
    }

    println!();

    // Storage info
    let counts = storage.counts()?;

    println!("Storage:  {}", storage.base_dir().display());
    println!("Database: {}", storage.db_path().display());
    println!("Entries:  {} ({} pinned)", counts.total, counts.pinned);
    println!("Size:     {}", util::format_size(usize::try_from(counts.total_bytes)?));

    if let Some(newest) = counts.newest {
        println!("Latest:   {}", util::format_relative_time(newest));
    }

    println!();

    // Configuration info
    let policy = storage.policy();
    let file = match config_source {
        config::ConfigSource::Explicit(path) => format!("{} (--config)", path.display()),
        config::ConfigSource::File(path) => path.display().to_string(),
        config::ConfigSource::Defaults(Some(path)) => {
            format!("{} (not found; using defaults)", path.display())
        }
        config::ConfigSource::Defaults(None) => "none (using defaults)".to_string(),
    };
    println!("Config:");
    println!("  File:        {file}");
    println!(
        "  Max entries: {} ({})",
        storage.max_entries(),
        limit_source.label(storage.limit_is_saved())
    );
    match policy.retention() {
        Some(retention) => println!("  Retention:   {} days", retention.as_secs() / 86_400),
        None => println!("  Retention:   off"),
    }
    println!("  Entry cap:   {}", util::format_size(policy.max_entry_bytes()));
    println!(
        "  Total cap:   {} (unpinned)",
        util::format_size(usize::try_from(policy.max_total_bytes()).unwrap_or(usize::MAX))
    );
    let builtins = if config.storage.builtin_secret_patterns {
        " + built-in secret patterns"
    } else {
        ""
    };
    println!("  Ignore:      {} patterns{builtins}", policy.user_pattern_count());

    let backups = storage.legacy_backup_dirs();
    if !backups.is_empty() {
        println!();
        println!("\x1b[33mLegacy backup\x1b[0m (old plaintext files; delete after checking):");
        for dir in backups {
            println!("  {}", dir.display());
        }
    }

    println!();

    // Wayland check
    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        println!("Wayland: \x1b[32mdetected\x1b[0m");
    } else {
        println!("Wayland: \x1b[31mnot detected\x1b[0m");
        println!("         ClipStack requires a Wayland session");
    }

    Ok(())
}

/// Generate shell completions
/// Print what `check` found.
fn print_check(storage: &storage::Storage, report: &storage::CheckReport) -> Result<()> {
    println!("Database:     {}", storage.db_path().display());
    if report.integrity_errors.is_empty() {
        println!("Integrity:    ok");
    } else {
        println!("Integrity:    \x1b[31m{} problems\x1b[0m", report.integrity_errors.len());
        for problem in report.integrity_errors.iter().take(10) {
            println!("  {problem}");
        }
    }
    let fts = match report.fts {
        storage::FtsState::Ok => "ok",
        storage::FtsState::Damaged => "\x1b[31mdamaged\x1b[0m",
        storage::FtsState::Rebuilt => "rebuilt",
    };
    println!("Search index: {fts}");
    let counts = &report.counts;
    println!(
        "Entries:      {} ({} pinned, {})",
        counts.total,
        counts.pinned,
        util::format_size(usize::try_from(counts.total_bytes)?)
    );
    for dir in &report.legacy_backups {
        println!("Legacy backup: {} (delete after checking)", dir.display());
    }
    Ok(())
}

/// A failed check as an error with what to do next.
fn check_result(storage: &storage::Storage, report: &storage::CheckReport) -> Result<()> {
    if report.is_ok() {
        return Ok(());
    }
    if !report.integrity_errors.is_empty() {
        return Err(storage::StorageError::Corrupt(format!(
            "the database is damaged ({} problems). Save what can still be read with \
             `clipstack export -o backup.jsonl`, then try \
             `sqlite3 {} .recover | sqlite3 recovered.db`",
            report.integrity_errors.len(),
            storage.db_path().display()
        ))
        .into());
    }
    Err(storage::StorageError::Corrupt(
        "the search index is damaged; run `clipstack check --repair`".to_string(),
    )
    .into())
}

/// Create the `export -o` file 0600. Without `force` an existing file is an
/// error; with it the file is truncated and tightened to 0600 before any
/// content is written.
fn create_export_file(path: &Path, force: bool) -> Result<File> {
    use std::os::unix::fs::PermissionsExt;
    let mut options = perms::private_file_options();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let file = options.open(path).map_err(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists {
            anyhow::anyhow!("{} already exists; use --force to overwrite it", path.display())
        } else {
            anyhow::Error::new(e).context(format!("Failed to create {}", path.display()))
        }
    })?;
    file.set_permissions(std::fs::Permissions::from_mode(perms::FILE_MODE))
        .with_context(|| format!("Failed to chmod {}", path.display()))?;
    Ok(file)
}

/// Whether an error came from writing to a closed pipe (`export | head`).
fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        let kind = cause
            .downcast_ref::<io::Error>()
            .map(io::Error::kind)
            .or_else(|| cause.downcast_ref::<serde_json::Error>().and_then(|e| e.io_error_kind()));
        kind == Some(io::ErrorKind::BrokenPipe)
    })
}

fn generate_completions(shell: Shell) {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    generate(shell, &mut cmd, name, &mut io::stdout());
}

/// Maximum size for incoming TCP data (10MB)
const MAX_TCP_CONTENT_SIZE: usize = 10 * 1024 * 1024;

fn serve_clipboard(storage: storage::Storage, port: u16) -> Result<()> {
    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr)?;
    eprintln!("Clipboard server listening on {}", addr);
    eprintln!("SSH usage: ssh -R {}:localhost:{} remote", port, port);
    eprintln!("Remote usage: cat file | nc localhost {}", port);
    eprintln!("Max content size: {} bytes", MAX_TCP_CONTENT_SIZE);

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                // Read with size limit to prevent memory exhaustion attacks
                let mut content = Vec::new();
                let mut buffer = [0u8; 8192];
                let mut total_read = 0;

                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) => break, // EOF
                        Ok(n) => {
                            total_read += n;
                            if total_read > MAX_TCP_CONTENT_SIZE {
                                eprintln!(
                                    "Connection rejected: content exceeds {} byte limit",
                                    MAX_TCP_CONTENT_SIZE
                                );
                                break;
                            }
                            content.extend_from_slice(&buffer[..n]);
                        }
                        Err(e) => {
                            eprintln!("Error reading from connection: {}", e);
                            break;
                        }
                    }
                }

                if total_read > MAX_TCP_CONTENT_SIZE || content.is_empty() {
                    continue;
                }

                // Convert to string, rejecting non-UTF-8 content
                let content = match String::from_utf8(content) {
                    Ok(s) => s,
                    Err(_) => {
                        eprintln!("Rejected non-UTF-8 content");
                        continue;
                    }
                };

                // Save to storage and clipboard
                match storage.ingest(&content, Source::Remote, &[]) {
                    Ok(outcome) => {
                        if let Err(e) = clipboard::Clipboard::system().copy(&content) {
                            eprintln!("Warning: couldn't copy to system clipboard: {}", e);
                        }
                        match outcome {
                            IngestOutcome::Inserted(entry) | IngestOutcome::Bumped(entry) => {
                                // Use chars().take() for safe Unicode truncation
                                let preview: String = entry.preview.chars().take(40).collect();
                                eprintln!("Received {} bytes: {}...", entry.size, preview);
                            }
                            IngestOutcome::Skipped(reason) => eprintln!(
                                "[remote] skipped: {}",
                                storage.policy().describe_skip(&reason)
                            ),
                        }
                    }
                    Err(e) => eprintln!("Error saving entry: {}", e),
                }
            }
            Err(e) => eprintln!("Connection error: {}", e),
        }
    }

    Ok(())
}

fn ctrlc_handler(running: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    ctrlc::set_handler(move || {
        running.store(false, std::sync::atomic::Ordering::SeqCst);
    })
    .expect("Error setting signal handler");
}

#[cfg(test)]
mod tests {
    use super::*;

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

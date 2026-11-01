mod clipboard;
mod daemon;
mod picker;
mod storage;
mod util;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

#[derive(Parser)]
#[command(name = "clipstack")]
#[command(about = "Fast clipboard manager with lazy-loading history")]
#[command(version)]
struct Cli {
    /// Custom storage directory
    #[arg(long, global = true)]
    storage_dir: Option<PathBuf>,

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

    /// Attempt to recover from corrupted storage
    Recover,

    /// Start a TCP server for remote clipboard (use with SSH reverse tunnel)
    Serve {
        /// Port to listen on
        #[arg(short, long, default_value = "7779")]
        port: u16,
    },

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

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

    // Determine max_entries: CLI > env > limit stored in index (> default)
    let env_limit = std::env::var("CLIPSTACK_MAX_ENTRIES").ok();
    let (max_entries, limit_source) = resolve_max_entries(cli.max_entries, env_limit.as_deref());

    let storage_dir = cli.storage_dir.unwrap_or_else(storage::Storage::default_dir);
    let storage = storage::Storage::open(storage_dir, max_entries)?;

    match cli.command {
        None | Some(Commands::Pick) => {
            // Default action: open picker
            picker::pick_and_paste(storage)?;
        }

        Some(Commands::Copy) => {
            let mut content = String::new();
            io::stdin().read_to_string(&mut content)?;

            clipboard::Clipboard::copy(&content)?;
            storage.save_entry(&content)?;

            eprintln!("Copied {} bytes", content.len());
        }

        Some(Commands::Paste) => {
            let content = clipboard::Clipboard::paste()?;
            io::stdout().write_all(content.as_bytes())?;
        }

        Some(Commands::List { count }) => {
            let index = storage.load_index()?;

            for entry in index.entries.iter().take(count) {
                let time = util::format_relative_time(entry.timestamp);
                let size = util::format_size(entry.size);
                let preview: String = entry
                    .preview
                    .chars()
                    .take(50)
                    .collect::<String>()
                    .replace('\n', " ");

                println!("{:>5} [{:>6}] {}", time, size, preview);
            }

            if index.entries.len() > count {
                println!("... and {} more", index.entries.len() - count);
            }
        }

        Some(Commands::Clear) => {
            storage.clear()?;
            println!("Clipboard history cleared");
        }

        Some(Commands::Daemon) => {
            // Use custom storage dir if provided, but always use global lock file
            let daemon = daemon::Daemon::new(Some(storage.base_dir().to_path_buf()), max_entries)?;

            // Handle Ctrl+C
            let running = daemon.stop_handle();
            ctrlc_handler(running);

            daemon.run()?;
        }

        Some(Commands::Kill) => {
            if !daemon::Daemon::is_running() {
                println!("Daemon is not running");
                return Ok(());
            }

            // Use pkill to find and kill the daemon process
            let status = std::process::Command::new("pkill")
                .args(["-f", "clipstack daemon"])
                .status();

            match status {
                Ok(s) if s.success() => {
                    println!("Daemon stopped");
                }
                Ok(_) => {
                    // pkill returns non-zero if no process found
                    println!("Daemon is not running");
                }
                Err(e) => {
                    eprintln!("Failed to kill daemon: {}", e);
                    std::process::exit(1);
                }
            }
        }

        Some(Commands::Stats) => {
            let index = storage.load_index()?;
            let total_size: usize = index.entries.iter().map(|e| e.size).sum();
            let pinned_count = index.entries.iter().filter(|e| e.pinned).count();
            let unpinned_count = index.entries.len() - pinned_count;

            println!("Entries:     {}", index.entries.len());
            println!("  Pinned:    {} (protected)", pinned_count);
            println!(
                "  Regular:   {}/{} ({})",
                unpinned_count,
                storage.max_entries(),
                limit_source.label()
            );
            println!("Total size:  {}", util::format_size(total_size));

            if let Some(oldest) = index.entries.last() {
                println!("Oldest:      {}", util::format_relative_time(oldest.timestamp));
            }
            if let Some(newest) = index.entries.first() {
                println!("Newest:      {}", util::format_relative_time(newest.timestamp));
            }
        }

        Some(Commands::Status) => {
            print_status(&storage, limit_source)?;
        }

        Some(Commands::Recover) => {
            match storage.attempt_recovery() {
                Ok(count) => {
                    println!("Recovery complete. Recovered {} entries.", count);
                }
                Err(e) => {
                    eprintln!("Recovery failed: {}", e);
                    std::process::exit(1);
                }
            }
        }

        Some(Commands::Serve { port }) => {
            serve_clipboard(storage, port)?;
        }

        Some(Commands::Completions { shell }) => {
            generate_completions(shell);
        }
    }

    Ok(())
}

/// Where the effective max_entries limit came from
#[derive(Clone, Copy, Debug, PartialEq)]
enum LimitSource {
    Cli,
    Env,
    /// Not configured this run: reuse the limit saved in the index (or default)
    Stored,
}

impl LimitSource {
    fn label(self) -> &'static str {
        match self {
            LimitSource::Cli => "--max-entries",
            LimitSource::Env => "CLIPSTACK_MAX_ENTRIES",
            LimitSource::Stored => "saved",
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
fn print_status(storage: &storage::Storage, limit_source: LimitSource) -> Result<()> {
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
    let index = storage.load_index()?;
    let total_size: usize = index.entries.iter().map(|e| e.size).sum();

    println!("Storage: {:?}", storage.base_dir());
    println!("Entries: {}/{}", index.entries.len(), index.max_entries);
    println!("Size:    {}", util::format_size(total_size));

    if let Some(newest) = index.entries.first() {
        println!("Latest:  {}", util::format_relative_time(newest.timestamp));
    }

    println!();

    // Configuration info
    println!("Config:");
    println!(
        "  Max entries: {} ({})",
        storage.max_entries(),
        limit_source.label()
    );

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
                match storage.save_entry(&content) {
                    Ok(entry) => {
                        if let Err(e) = clipboard::Clipboard::copy(&content) {
                            eprintln!("Warning: couldn't copy to system clipboard: {}", e);
                        }
                        // Use chars().take() for safe Unicode truncation
                        let preview: String = entry.preview.chars().take(40).collect();
                        eprintln!(
                            "Received {} bytes: {}...",
                            entry.size,
                            preview
                        );
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
    .expect("Error setting Ctrl-C handler");
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
    fn test_resolve_limit_none_means_stored() {
        assert_eq!(resolve_max_entries(None, None), (None, LimitSource::Stored));
    }
}

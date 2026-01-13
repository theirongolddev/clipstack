mod clipboard;
mod daemon;
mod picker;
mod storage;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "clipstack")]
#[command(about = "Fast clipboard manager with lazy-loading history")]
#[command(version)]
struct Cli {
    /// Custom storage directory
    #[arg(long, global = true)]
    storage_dir: Option<PathBuf>,

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

    /// Show storage statistics
    Stats,

    /// Start a TCP server for remote clipboard (use with SSH reverse tunnel)
    Serve {
        /// Port to listen on
        #[arg(short, long, default_value = "7779")]
        port: u16,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let storage_dir = cli.storage_dir.unwrap_or_else(storage::Storage::default_dir);
    let storage = storage::Storage::new(storage_dir)?;

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
                let time = format_timestamp(entry.timestamp);
                let size = format_size(entry.size);
                let preview: String = entry
                    .preview
                    .chars()
                    .take(50)
                    .collect::<String>()
                    .replace('\n', "↵");

                println!("{} [{:>7}] {}", time, size, preview);
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
            let daemon = daemon::Daemon::new(Some(storage.base_dir().to_path_buf()))?;

            // Handle Ctrl+C
            let running = daemon.stop_handle();
            ctrlc_handler(running);

            daemon.run()?;
        }

        Some(Commands::Stats) => {
            let index = storage.load_index()?;
            let total_size: usize = index.entries.iter().map(|e| e.size).sum();

            println!("Entries: {}", index.entries.len());
            println!("Max entries: {}", index.max_entries);
            println!("Total size: {}", format_size(total_size));

            if let Some(oldest) = index.entries.last() {
                println!("Oldest: {}", format_timestamp(oldest.timestamp));
            }
            if let Some(newest) = index.entries.first() {
                println!("Newest: {}", format_timestamp(newest.timestamp));
            }
        }

        Some(Commands::Serve { port }) => {
            serve_clipboard(storage, port)?;
        }
    }

    Ok(())
}

fn serve_clipboard(storage: storage::Storage, port: u16) -> Result<()> {
    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr)?;
    eprintln!("Clipboard server listening on {}", addr);
    eprintln!("SSH usage: ssh -R {}:localhost:{} remote", port, port);
    eprintln!("Remote usage: cat file | nc localhost {}", port);

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let mut content = String::new();
                if let Err(e) = stream.read_to_string(&mut content) {
                    eprintln!("Error reading from connection: {}", e);
                    continue;
                }

                if content.is_empty() {
                    continue;
                }

                // Save to storage and clipboard
                match storage.save_entry(&content) {
                    Ok(entry) => {
                        if let Err(e) = clipboard::Clipboard::copy(&content) {
                            eprintln!("Warning: couldn't copy to system clipboard: {}", e);
                        }
                        eprintln!(
                            "Received {} bytes: {}...",
                            entry.size,
                            &entry.preview[..entry.preview.len().min(40)]
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

fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn format_timestamp(timestamp: i64) -> String {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_millis_opt(timestamp)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "????-??-?? ??:??:??".to_string())
}

fn ctrlc_handler(running: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    ctrlc::set_handler(move || {
        running.store(false, std::sync::atomic::Ordering::SeqCst);
    })
    .expect("Error setting Ctrl-C handler");
}

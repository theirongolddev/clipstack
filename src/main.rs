mod cli;
mod clipboard;
mod commands;
mod config;
mod daemon;
mod perms;
mod protocol;
mod picker;
mod storage;
mod util;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};

fn main() -> Result<()> {
    let cli = Cli::parse();

    if matches!(cli.command, Some(Commands::WatchHook)) {
        // Always exit 0: a failing command would make wl-paste --watch stop
        if let Err(e) = commands::watch_hook(&cli) {
            eprintln!("[watch] {e:#}");
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
        }
        return Ok(());
    }

    if matches!(cli.command, Some(Commands::Serve { .. })) {
        commands::serve_removed();
    }

    // Check dependencies on commands that need clipboard access
    if matches!(
        cli.command,
        None | Some(Commands::Pick)
            | Some(Commands::Copy)
            | Some(Commands::Paste)
            | Some(Commands::Daemon)
    ) {
        commands::check_dependencies()?;
    }

    // Determine max_entries: CLI > env > limit saved in the database (> default)
    let env_limit = std::env::var("CLIPSTACK_MAX_ENTRIES").ok();
    let (max_entries, limit_source) = cli::resolve_max_entries(cli.max_entries, env_limit.as_deref());

    // The config is loaded (and its patterns compiled) before storage is opened
    let (config, config_source) = config::Config::load(cli.config.as_deref())?;
    let policy = config.ingest_policy()?;

    let storage_dir = cli.storage_dir.unwrap_or_else(storage::Storage::default_dir);
    let storage = storage::Storage::open(storage_dir, max_entries, policy)?;

    match cli.command {
        // Default action: open picker
        None | Some(Commands::Pick) => picker::pick_and_paste(storage).map(drop),
        Some(Commands::Copy) => commands::copy(&storage),
        Some(Commands::Paste) => commands::paste(),
        Some(Commands::List { count }) => commands::list(&storage, count),
        Some(Commands::Clear) => commands::clear(&storage),
        Some(Commands::Daemon) => commands::run_daemon(storage, &config, cli.config.as_deref()),
        Some(Commands::Kill) => commands::kill(),
        Some(Commands::Stats) => commands::stats(&storage, limit_source),
        Some(Commands::Status) => commands::status(&storage, limit_source, &config, &config_source),
        Some(Commands::Check { repair }) => commands::check(&storage, repair),
        Some(Commands::Recover) => commands::recover(&storage),
        Some(Commands::Export { output, force }) => commands::export(&storage, output, force),
        Some(Commands::Completions { shell }) => {
            commands::completions(shell);
            Ok(())
        }
        Some(Commands::Serve { .. } | Commands::WatchHook) => {
            unreachable!("handled before storage is opened")
        }
    }
}

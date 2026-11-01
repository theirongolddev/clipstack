//! One function per subcommand. `main` parses the command line, loads the
//! config, opens storage and dispatches here.

use crate::cli::{Cli, CliError, Exit, LimitSource, resolve_max_entries};
use crate::storage::{self, IngestOutcome, ListOrder, Source, Storage};
use crate::{clipboard, config, daemon, perms, protocol, util};
use anyhow::{Context, Result};
use clap::CommandFactory;
use clap_complete::{Shell, generate};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// `serve`: the TCP server is gone; the error says where the socket is (exit 2).
pub fn serve_removed() -> CliError {
    let sock = daemon::Daemon::socket_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "$XDG_RUNTIME_DIR/clipstack.sock".to_string());
    CliError::new(
        Exit::Usage,
        format!(
            "serve was removed: the daemon listens on {sock}; see README \"Remote clipboard\" for rcopy over ssh"
        ),
    )
}

/// `copy`: stdin to the clipboard and the history.
pub fn copy(storage: &Storage) -> Result<()> {
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
    Ok(())
}

/// `paste`: the clipboard to stdout.
pub fn paste() -> Result<()> {
    let content = clipboard::Clipboard::system().paste_text()?;
    io::stdout().write_all(content.as_bytes())?;
    Ok(())
}

/// `list`: the newest `count` entries, one line each.
pub fn list(storage: &Storage, count: usize) -> Result<()> {
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
    Ok(())
}

/// `clear`: every entry and the legacy backup dirs.
pub fn clear(storage: &Storage) -> Result<()> {
    let cleared = storage.clear()?;
    println!("Clipboard history cleared ({cleared} entries)");
    let (removed, left) = storage.remove_legacy_backups()?;
    if removed > 0 {
        println!("Removed {removed} legacy backup dirs");
    }
    for dir in left {
        println!(
            "Kept {} (it holds something other than regular files; delete it yourself)",
            dir.display()
        );
    }
    Ok(())
}

/// `daemon`: capture and serve the socket until stopped.
pub fn run_daemon(storage: Storage, config: &config::Config, config_path: Option<&Path>) -> Result<()> {
    let options = daemon::DaemonOptions {
        capture: config.capture.clone(),
        socket: config.socket.clone(),
        clipboard: clipboard::Clipboard::system(),
        hook: watch_hook_command(storage.base_dir(), config_path)?,
    };
    // The lock file is global even with a custom storage dir
    let daemon = daemon::Daemon::new(storage, options)?;

    // Stop cleanly on SIGINT, SIGTERM and SIGHUP (ctrlc "termination")
    let running = daemon.stop_handle();
    ctrlc_handler(running);

    daemon.run()
}

/// `kill`: signal the daemon recorded in the lock file.
pub fn kill() -> Result<()> {
    if !daemon::Daemon::is_running() {
        println!("Daemon is not running");
        return Ok(());
    }

    // Signal the exact PID recorded in the daemon's lock file
    let Some(pid) = daemon::Daemon::running_pid() else {
        return Err(CliError::new(
            Exit::NotFound,
            "Daemon is running but its PID is unknown (started by an older version?)\n\
             Stop it manually, e.g.: pkill -x clipstack",
        )
        .into());
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
        Ok(_) => anyhow::bail!("Failed to signal daemon (pid {pid})"),
        Err(e) => anyhow::bail!("Failed to kill daemon: {e}"),
    }
    Ok(())
}

/// `stats`: entry counts, sizes and the limit.
pub fn stats(storage: &Storage, limit_source: LimitSource) -> Result<()> {
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
    Ok(())
}

/// `check [--repair]`.
pub fn check(storage: &Storage, repair: bool) -> Result<()> {
    let report = storage.check(repair)?;
    print_check(storage, &report)?;
    check_result(storage, &report)
}

/// `recover`: the old name for `check --repair`, with the old message.
pub fn recover(storage: &Storage) -> Result<()> {
    let report = storage.check(true)?;
    check_result(storage, &report)?;
    println!("Recovery complete. {} entries.", report.counts.total);
    Ok(())
}

/// `export [-o FILE [--force]]`: JSON Lines, oldest first.
pub fn export(storage: &Storage, output: Option<PathBuf>, force: bool) -> Result<()> {
    match output {
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
pub fn watch_hook(cli: &Cli) -> Result<()> {
    let env_limit = std::env::var("CLIPSTACK_MAX_ENTRIES").ok();
    let (max_entries, _) = resolve_max_entries(cli.max_entries, env_limit.as_deref());
    let (config, _) = config::Config::load(cli.config.as_deref())?;
    let storage_dir = cli.storage_dir.clone().unwrap_or_else(Storage::default_dir);
    let storage = Storage::open(storage_dir, max_entries, config.ingest_policy()?)?;
    let state = std::env::var("CLIPBOARD_STATE").ok();
    daemon::capture::run_hook(
        &storage,
        &clipboard::Clipboard::system(),
        state.as_deref(),
        &mut io::stdin().lock(),
    )
}

/// The first executable file named `name` in the `PATH`-style list `path`.
/// Empty entries are skipped (not taken as the working dir).
fn find_in_path(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .find(|file| std::fs::metadata(file).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// Check if required dependencies (wl-clipboard) are installed (exit 6 if not)
pub fn check_dependencies() -> Result<()> {
    // Look wl-paste up on PATH ourselves: `which` isn't installed everywhere
    let path = std::env::var_os("PATH").unwrap_or_default();
    match find_in_path("wl-paste", &path) {
        Some(_) => Ok(()),
        None => Err(clipboard::ClipboardError::from(anyhow::anyhow!(
            "wl-clipboard not found\n\n\
             ClipStack requires wl-clipboard for Wayland clipboard access.\n\n\
             Install it with:\n  \
               Arch:   sudo pacman -S wl-clipboard\n  \
               Debian: sudo apt install wl-clipboard\n  \
               Fedora: sudo dnf install wl-clipboard\n\n\
             Also ensure you're running in a Wayland session:\n  \
               echo $WAYLAND_DISPLAY"
        ))
        .into()),
    }
}

/// `status`: daemon, socket, storage and config.
pub fn status(
    storage: &Storage,
    limit_source: LimitSource,
    config: &config::Config,
    config_source: &config::ConfigSource,
) -> Result<()> {
    // Check daemon status
    let daemon_running = daemon::Daemon::is_running();

    match daemon::Daemon::running_pid() {
        Some(pid) => println!("Daemon:  \x1b[32mrunning (pid {pid})\x1b[0m"),
        None if daemon_running => println!("Daemon:  \x1b[32mrunning\x1b[0m"),
        None => {
            println!("Daemon:  \x1b[33mnot running\x1b[0m");
            println!("         Start with: clipstack daemon");
            println!("         Or just run: clipstack (auto-starts daemon)");
        }
    }
    match daemon::Daemon::socket_path() {
        Ok(path) => println!("Socket:  {} ({})", path.display(), socket_state(&path, daemon_running)),
        Err(e) => println!("Socket:  unavailable ({e:#})"),
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

/// Print what `check` found.
fn print_check(storage: &Storage, report: &storage::CheckReport) -> Result<()> {
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
fn check_result(storage: &Storage, report: &storage::CheckReport) -> Result<()> {
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

/// What PING on the daemon socket says, for `status`.
fn socket_state(path: &Path, daemon_running: bool) -> String {
    match daemon::socket::ping(path, std::time::Duration::from_secs(1)) {
        Ok(resp @ protocol::Response::Ok { .. }) => format!(
            "reachable, capture={}, pull={}",
            resp.meta("capture").unwrap_or("unknown"),
            if resp.meta("pull") == Some("1") { "on" } else { "off" }
        ),
        Ok(protocol::Response::Err { code, message }) => format!("error: {code} {message}"),
        // An old daemon (no socket) or `[socket] enabled = false`
        Err(_) if daemon_running => "not reachable; capture=unknown".to_string(),
        Err(_) => "not listening".to_string(),
    }
}

/// `completions <shell>`.
pub fn completions(shell: Shell) {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    generate(shell, &mut cmd, name, &mut io::stdout());
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
}

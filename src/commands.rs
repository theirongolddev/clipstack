//! One function per subcommand. `main` parses the command line, loads the
//! config, opens storage and dispatches here.

use crate::cli::{Cli, CliError, EntryRef, Exit, LimitSource, resolve_max_entries};
use crate::picker::Picked;
use crate::storage::{self, ClipEntry, EntryId, IngestOutcome, Kind, ListOrder, Source, Storage};
use std::collections::HashMap;
use crate::{clipboard, config, daemon, image_cache, perms, png, protocol, util};
use anyhow::{Context, Result};
use clap::CommandFactory;
use clap_complete::{Shell, generate};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufWriter, IsTerminal, Read, Write};
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

/// `list [-c N] [--json] [--pinned]`: the newest `count` entries (only
/// pinned ones with `pinned`), one line each or as a JSON array.
pub fn list(storage: &Storage, count: usize, json: bool, pinned: bool) -> Result<()> {
    let (entries, total) = if pinned {
        let all: Vec<_> = positioned(storage.list(ListOrder::Recency, None)?)
            .into_iter()
            .filter(|(_, e)| e.pinned)
            .collect();
        let total = all.len();
        (all.into_iter().take(count).collect(), total)
    } else {
        (positioned(storage.list(ListOrder::Recency, Some(count))?), storage.counts()?.total)
    };

    if json {
        return print_json(&entries);
    }
    print_lines(entries.iter().map(|(_, e)| e));
    if total > entries.len() {
        println!("... and {} more", total - entries.len());
    }
    Ok(())
}

/// `search QUERY [-c N] [--json]`: entries holding every word, pinned
/// first, then newest. No match exits 1 silently (`[]` with `--json`).
pub fn search(storage: &Storage, query: &str, count: usize, json: bool) -> Result<()> {
    if query.trim().is_empty() {
        return Err(CliError::new(Exit::Usage, "the search query is empty").into());
    }
    let mut found = storage.search(query, count.saturating_add(1))?;
    let more = found.len() > count;
    found.truncate(count);

    if json {
        // Positions in the full recency order, so `get N` finds the same
        // entry (one deleted between the two queries is left out)
        let positions: HashMap<EntryId, usize> =
            positioned(storage.list(ListOrder::Recency, None)?).into_iter().map(|(n, e)| (e.id, n)).collect();
        let entries: Vec<_> = found.into_iter().filter_map(|e| Some((*positions.get(&e.id)?, e))).collect();
        print_json(&entries)?;
        return no_match_if(entries.is_empty());
    }
    print_lines(found.iter());
    if more {
        println!("... more matches (show more with -c N)");
    }
    no_match_if(found.is_empty())
}

/// Exit 1 without a message when nothing matched (grep-like).
fn no_match_if(empty: bool) -> Result<()> {
    if empty { Err(CliError::new(Exit::NotFound, "").into()) } else { Ok(()) }
}

/// Entries in recency order, each with its 1-based position (`get N`).
fn positioned(entries: Vec<ClipEntry>) -> Vec<(usize, ClipEntry)> {
    (1..).zip(entries).collect()
}

/// The human `list` format: age, size and a one-line preview.
fn print_lines<'a>(entries: impl Iterator<Item = &'a ClipEntry>) {
    for entry in entries {
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
}

/// One element of the `--json` array: the entry's fields plus `position`.
#[derive(serde::Serialize)]
struct JsonEntry<'a> {
    position: usize,
    #[serde(flatten)]
    entry: &'a ClipEntry,
}

/// A JSON array of entries on one line. A closed stdout ends it quietly.
fn print_json(entries: &[(usize, ClipEntry)]) -> Result<()> {
    let items: Vec<_> = entries.iter().map(|(position, entry)| JsonEntry { position: *position, entry }).collect();
    let mut out = io::stdout().lock();
    let written = serde_json::to_writer(&mut out, &items)
        .map_err(io::Error::from)
        .and_then(|()| writeln!(out))
        .and_then(|()| out.flush());
    match written {
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(e.into()),
        _ => Ok(()),
    }
}

/// The entry `reference` names; not found is exit 1 (`no entry for REF`).
fn resolve(storage: &Storage, reference: &EntryRef) -> Result<ClipEntry> {
    let entry = match reference {
        EntryRef::Position(n) => storage.nth(n.get())?,
        EntryRef::Id(id) => storage.get(*id)?,
        EntryRef::Alias(name) => storage.by_alias(name)?,
    };
    entry.ok_or_else(|| CliError::new(Exit::NotFound, format!("no entry for {reference}")).into())
}

/// `get REF [--copy | --image-path]`: the content to stdout as stored, with
/// no newline added. `--copy` also puts it on the clipboard and bumps it
/// through `ingest`, so it is the newest entry without needing the daemon
/// (the watch hook's own bump, if a daemon runs, changes nothing). An image
/// is written as PNG bytes, but never to a terminal (exit 2); with `--copy`
/// it is copied as `image/png` and not written. `--image-path` prints the
/// path of the image's cached file instead (exit 1 for text).
/// `--paste` (which implies `copy`) then types `[picker] paste_keys` with
/// wtype, as `pick --paste` does.
pub fn get(storage: &Storage, reference: &EntryRef, get: Get, picker: &config::PickerConfig) -> Result<()> {
    let entry = resolve(storage, reference)?;
    if get.image_path {
        let path = image_cache::ensure(storage, &entry)?;
        return write_stdout(format!("{}\n", path.display()).as_bytes());
    }
    if entry.kind == Kind::Image {
        get_image(storage, &entry, reference, get.copy)?;
    } else {
        let content = storage.load_content(entry.id)?;
        if get.copy {
            clipboard::Clipboard::system().copy(&content)?;
            if let IngestOutcome::Skipped(reason) = storage.ingest(&content, Source::Copy, &[])? {
                eprintln!("Copied, but not kept in the history: {}", storage.policy().describe_skip(&reason));
            }
        }
        write_stdout(content.as_bytes())?;
    }
    if get.paste {
        paste_into_focused_window(picker)?;
    }
    Ok(())
}

/// `get` flags.
#[derive(Debug, Clone, Copy)]
pub struct Get {
    pub copy: bool,
    pub paste: bool,
    pub image_path: bool,
}

/// `get` of an image entry: copy it as `image/png`, or write the PNG to a
/// stdout that is not a terminal.
fn get_image(storage: &Storage, entry: &ClipEntry, reference: &EntryRef, copy: bool) -> Result<()> {
    if !copy && io::stdout().is_terminal() {
        return Err(image_on_terminal(entry, reference).into());
    }
    let png = storage.load_image(entry.id)?;
    if !copy {
        return write_stdout(&png);
    }
    clipboard::Clipboard::system().copy_bytes(&png, png::MIME)?;
    match storage.ingest_image(&png, Source::Copy, &[])? {
        IngestOutcome::Skipped(reason) => eprintln!(
            "Copied {}, but not kept in the history: {}",
            image_label(entry),
            storage.policy().describe_skip(&reason)
        ),
        IngestOutcome::Inserted(_) | IngestOutcome::Bumped(_) => eprintln!("Copied {}", image_label(entry)),
    }
    Ok(())
}

/// "image 640x480 (12.3 KB)".
fn image_label(entry: &ClipEntry) -> String {
    let (width, height) = (entry.width.unwrap_or(0), entry.height.unwrap_or(0));
    format!("image {width}x{height} ({})", util::format_size(entry.size))
}

/// Exit 2: PNG bytes would garble the terminal.
fn image_on_terminal(entry: &ClipEntry, reference: &EntryRef) -> CliError {
    CliError::new(
        Exit::Usage,
        format!(
            "id:{} is an {}; not writing it to a terminal. Redirect it \
             (clipstack get {reference} > image.png), or use --copy or --image-path",
            entry.id,
            image_label(entry)
        ),
    )
}

/// Write `bytes` to stdout; a closed stdout ends quietly.
fn write_stdout(bytes: &[u8]) -> Result<()> {
    let mut out = io::stdout().lock();
    match out.write_all(bytes).and_then(|()| out.flush()) {
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(e.into()),
        _ => Ok(()),
    }
}

/// `pin REF`: pin the entry; already pinned is fine.
pub fn pin(storage: &Storage, reference: &EntryRef) -> Result<()> {
    let entry = resolve(storage, reference)?;
    if entry.pinned {
        println!("id:{} is already pinned", entry.id);
        return Ok(());
    }
    storage.set_pinned(entry.id, true)?;
    println!("id:{} is now pinned", entry.id);
    Ok(())
}

/// `unpin REF`: unpin the entry, which also removes its alias.
pub fn unpin(storage: &Storage, reference: &EntryRef) -> Result<()> {
    let entry = resolve(storage, reference)?;
    if !entry.pinned {
        println!("id:{} is not pinned", entry.id);
        return Ok(());
    }
    storage.set_pinned(entry.id, false)?;
    match entry.alias {
        Some(alias) => println!("id:{} is no longer pinned (its alias @{alias} was removed)", entry.id),
        None => println!("id:{} is no longer pinned", entry.id),
    }
    Ok(())
}

/// `delete REF`: delete one entry, and its cached image file if any.
pub fn delete(storage: &Storage, reference: &EntryRef) -> Result<()> {
    let entry = resolve(storage, reference)?;
    if !storage.delete_entry(entry.id)? {
        return Err(CliError::new(Exit::NotFound, format!("no entry for {reference}")).into());
    }
    if entry.kind == Kind::Image {
        image_cache::remove(entry.id);
    }
    println!("Deleted id:{}", entry.id);
    Ok(())
}

/// `pick [--paste]`: copy the entry chosen in the picker; with `paste` (or
/// `[picker] paste_after_pick`), wtype then types the paste keys into the
/// window that had focus. Without wtype it still copies and exits 0.
pub fn pick(storage: Storage, config: &config::PickerConfig, paste: bool) -> Result<()> {
    let paste = paste || config.paste_after_pick;
    let Some(picked) = choose(storage, config)? else {
        return Ok(());
    };
    match picked {
        Picked::Text(content) => {
            clipboard::Clipboard::system().copy(&content)?;
            eprintln!("Copied {} bytes to clipboard", content.len());
        }
        Picked::Image(png) => {
            clipboard::Clipboard::system().copy_bytes(&png, png::MIME)?;
            eprintln!("Copied a PNG image ({}) to clipboard", util::format_size(png.len()));
        }
    }
    if paste {
        paste_into_focused_window(config)?;
    }
    Ok(())
}

/// Type `[picker] paste_keys` with wtype, detached and after a short delay,
/// so the paste lands in the window that had focus. Without wtype this only
/// says so: the content is already on the clipboard.
fn paste_into_focused_window(config: &config::PickerConfig) -> Result<()> {
    let argv = crate::picker::paste_command(&config.paste_keys).map_err(|e| config::ConfigError(format!("[picker] {e}")))?;
    let args = argv.split_first().map_or(&[][..], |(_wtype, args)| args);
    let path = std::env::var_os("PATH").unwrap_or_default();
    match util::find_in_path("wtype", &path) {
        Some(wtype) => crate::picker::spawn_paste(&wtype, args).context("cannot start wtype")?,
        None => eprintln!("wtype not found: copied but not pasted (install wtype for --paste)"),
    }
    Ok(())
}

/// `edit REF`: edit a text entry in `$VISUAL`/`$EDITOR` (as the picker's
/// `e` does: a 0600 file in the runtime dir, removed afterwards). Changed
/// text is saved as a new entry (source `edit`) and copied; the original
/// stays. Unchanged text, or a failed editor, changes nothing (exit 0).
pub fn edit(storage: &Storage, reference: &EntryRef) -> Result<()> {
    let entry = resolve(storage, reference)?;
    let content = storage.load_content(entry.id)?;
    let dir = daemon::runtime_dir()?;
    let env = |name| std::env::var(name).ok();
    let editor = crate::picker::choose_editor(env("VISUAL").as_deref(), env("EDITOR").as_deref());
    let Some(text) = crate::picker::edit_in_editor(&content, &editor, &dir, entry.id)? else {
        eprintln!("Edit: not changed (unchanged, empty, or the editor failed)");
        return Ok(());
    };
    clipboard::Clipboard::system().copy(&text)?;
    match storage.ingest(&text, Source::Edit, &[])? {
        IngestOutcome::Inserted(saved) | IngestOutcome::Bumped(saved) => {
            eprintln!("Saved the edited text as id:{} and copied it", saved.id);
        }
        IngestOutcome::Skipped(reason) => eprintln!(
            "Copied the edited text (not saved to history: {})",
            storage.policy().describe_skip(&reason)
        ),
    }
    Ok(())
}

/// The picker's choice. Debug builds take `CLIPSTACK_TEST_PICK=REF` instead,
/// so tests can drive `pick` without a terminal or a daemon.
fn choose(storage: Storage, config: &config::PickerConfig) -> Result<Option<Picked>> {
    #[cfg(debug_assertions)]
    if let Ok(reference) = std::env::var("CLIPSTACK_TEST_PICK") {
        let reference: EntryRef = reference.parse().map_err(|e: String| CliError::new(Exit::Usage, e))?;
        let entry = resolve(&storage, &reference)?;
        return Ok(Some(match entry.kind {
            Kind::Image => Picked::Image(storage.load_image(entry.id)?),
            Kind::Text => Picked::Text(storage.load_content(entry.id)?),
        }));
    }
    crate::picker::pick(storage, config)
}

/// `alias REF NAME` names the entry (pinning it if needed); with `name`
/// `None` (`alias --remove REF`) the alias goes and the pin stays.
pub fn alias(storage: &Storage, reference: &EntryRef, name: Option<&str>) -> Result<()> {
    let entry = resolve(storage, reference)?;
    let Some(name) = name else {
        storage.set_alias(entry.id, None)?;
        match entry.alias {
            Some(old) => println!("Removed @{old} from id:{} (still pinned)", entry.id),
            None => println!("id:{} has no alias", entry.id),
        }
        return Ok(());
    };
    if let Some(owner) = storage.by_alias(name)?.filter(|owner| owner.id != entry.id) {
        return Err(CliError::new(Exit::NotFound, format!("alias @{name} already used by id:{}", owner.id)).into());
    }
    storage.set_alias(entry.id, Some(name))?;
    println!("id:{} is now @{name} (pinned)", entry.id);
    Ok(())
}

/// `clear`: every entry and the legacy backup dirs.
pub fn clear(storage: &Storage) -> Result<()> {
    let cleared = storage.clear()?;
    image_cache::prune(storage);
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

/// Check if required dependencies (wl-clipboard) are installed (exit 6 if not)
pub fn check_dependencies() -> Result<()> {
    // Look wl-paste up on PATH ourselves: `which` isn't installed everywhere
    let path = std::env::var_os("PATH").unwrap_or_default();
    match util::find_in_path("wl-paste", &path) {
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
    if report.image_errors.is_empty() {
        println!("Images:       ok");
    } else {
        println!("Images:       \x1b[31m{} damaged\x1b[0m", report.image_errors.len());
        for problem in report.image_errors.iter().take(10) {
            println!("  {problem}");
        }
    }
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
    if report.fts == storage::FtsState::Damaged {
        return Err(storage::StorageError::Corrupt(
            "the search index is damaged; run `clipstack check --repair`".to_string(),
        )
        .into());
    }
    let ids: Vec<&str> = report.image_errors.iter().filter_map(|e| e.split(':').nth(1)).collect();
    Err(storage::StorageError::Corrupt(format!(
        "{} image entries are damaged; remove them with `clipstack delete id:N` (id:{})",
        ids.len(),
        ids.join(", id:")
    ))
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
    let mut cmd = completion_command(Cli::command());
    let name = cmd.get_name().to_string();
    generate(shell, &mut cmd, name, &mut io::stdout());
}

/// `cli` without its hidden subcommands (`serve`, `recover`,
/// `__watch-hook`), which clap_complete would offer anyway; the bash
/// generator also panics on a name containing "__".
fn completion_command(cli: clap::Command) -> clap::Command {
    let mut cmd = clap::Command::new("clipstack").version(env!("CARGO_PKG_VERSION")).args(cli.get_arguments().cloned());
    if let Some(about) = cli.get_about() {
        cmd = cmd.about(about.clone());
    }
    cmd.subcommands(cli.get_subcommands().filter(|sub| !sub.is_hide_set()).cloned())
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
    fn image_on_a_terminal_is_a_usage_error_with_a_hint() {
        let entry = ClipEntry {
            id: 7,
            created_at: 0,
            last_used: 0,
            size: 2048,
            preview: "Image \u{b7} 640x480".to_string(),
            pinned: false,
            alias: None,
            source: Source::Watch,
            mime: png::MIME.to_string(),
            kind: Kind::Image,
            width: Some(640),
            height: Some(480),
        };
        let err = image_on_terminal(&entry, &EntryRef::Id(7));
        assert_eq!(err.code, Exit::Usage);
        for part in ["id:7", "640x480", "clipstack get id:7 > image.png", "--copy", "--image-path"] {
            assert!(err.msg.contains(part), "{part:?} missing: {}", err.msg);
        }
    }
}

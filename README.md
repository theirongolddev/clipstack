# ClipStack

A fast, keyboard-driven clipboard manager for Linux/Wayland. ClipStack keeps its history in a single SQLite database and provides vim-style navigation, fuzzy search with full-content matching, and remote clipboard support via SSH tunnels.

Built for developers who live in the terminal and want a clipboard manager that stays out of the way until needed.

## Features

**Core Functionality**
- **Clipboard History**: Automatically saves clipboard entries with SHA256 deduplication
- **Pinned Entries**: Mark important items with `p` to protect them from automatic pruning
- **TUI Picker**: Fuzzy-searchable history picker with live preview and vim keybindings
- **Remote Clipboard**: Copy from SSH sessions to your local clipboard via TCP tunnel
- **Secret Filtering**: Built-in patterns skip private keys and API tokens; add your own regexes in the config file
- **Retention and Size Caps**: Optionally expire old entries and cap entry and total size

**Performance**
- **SQLite Storage**: One WAL-mode database; lists read only metadata and full content is loaded on demand
- **Full-Content Search**: An FTS5 trigram index covers the first 256 K characters of every entry
- **Transactional Writes**: Every change is a SQLite transaction, so a crash never leaves a half-written history
- **Sub-100ms Startup**: Picker opens instantly even with large history

**User Experience**
- **Vim-Style Navigation**: Modal interface with `j/k`, `gg/G`, `/` search
- **Preview Scrolling**: Press `Tab` to enter scrollable preview mode for long content
- **Fuzzy Search**: Type to filter with highlighted matches and `[content]` indicator for deep matches
- **Delete with Undo**: Remove entries with 5-second undo window
- **Configurable Limits**: Set 1-10,000 max entries via CLI or environment variable; retention and size caps in a config file
- **Shell Completions**: Tab completion for bash, zsh, fish, elvish, powershell

## Why ClipStack?

**Problem**: Standard clipboard managers either lack features (like search), require heavy GUI frameworks, or don't integrate well with terminal-centric workflows.

**Solution**: ClipStack is designed for developers who:
- Spend most of their time in terminals and text editors
- Want vim-style keybindings without learning a new paradigm
- Need to copy text from remote servers to local clipboard
- Value simplicity: single binary, one SQLite file, configuration optional

**Comparison with alternatives:**

| Feature | ClipStack | CopyQ | Parcellite | wl-clipboard-history |
|---------|-----------|-------|------------|----------------------|
| TUI interface | Yes | No (GUI) | No (GUI) | No |
| Vim keybindings | Yes | No | No | No |
| Fuzzy search | Yes | Yes | No | No |
| Deep content search | Yes | No | No | No |
| Pinned entries | Yes | Yes | No | No |
| Remote clipboard | Yes | No | No | No |
| Single binary | Yes | No | No | Yes |

## Requirements

### System Dependencies

```bash
# Arch Linux
sudo pacman -S wl-clipboard

# Debian/Ubuntu
sudo apt install wl-clipboard

# Fedora
sudo dnf install wl-clipboard
```

**Note**: ClipStack requires a Wayland session (Sway, Hyprland, GNOME Wayland, etc.). X11 is not supported.

### Build Dependencies

- Rust 1.89+ (2024 edition)
- Cargo
- A C compiler (`cc`): SQLite is compiled into the binary (bundled), so no system SQLite is needed

## Installation

### From Source

```bash
# Clone the repository
git clone https://github.com/yourusername/clipstack.git
cd clipstack

# Build and install
cargo install --path .

# Verify installation
clipstack --version
```

### Manual Installation

```bash
cargo build --release
cp target/release/clipstack ~/.cargo/bin/
```

## Usage

### Commands

| Command | Description |
|---------|-------------|
| `clipstack` | Open the picker UI (default action) |
| `clipstack pick` | Open the picker UI |
| `clipstack copy` | Copy stdin to clipboard |
| `clipstack paste` | Paste clipboard contents to stdout |
| `clipstack list [-c N]` | List last N entries (default: 10) |
| `clipstack clear` | Clear clipboard history |
| `clipstack daemon` | Run the background monitoring daemon |
| `clipstack kill` | Stop the running daemon |
| `clipstack stats` | Show storage statistics |
| `clipstack status` | Check daemon and system health |
| `clipstack check [--repair]` | Check the database and search index; `--repair` rebuilds a damaged search index |
| `clipstack export [-o FILE] [--force]` | Write the whole history as JSON Lines, oldest first (`-o` creates FILE `0600`; `--force` overwrites) |
| `clipstack serve [-p PORT]` | Start TCP server for remote clipboard (default: 7779) |
| `clipstack completions <shell>` | Generate shell completions (bash, zsh, fish, elvish, powershell) |

### Global Options

| Option | Description |
|--------|-------------|
| `--storage-dir PATH` | Custom storage directory |
| `--config PATH` | Config file (default: `$XDG_CONFIG_HOME/clipstack/config.toml`); a missing explicit file is an error |
| `--max-entries N` | Maximum unpinned entries to store (1-10000, default: 100). Saved in the database and reused by later runs |

### Examples

```bash
# Copy from stdin
echo "Hello, World!" | clipstack copy

# Copy file contents
clipstack copy < /path/to/file.txt

# Paste to stdout
clipstack paste

# Paste to file
clipstack paste > output.txt

# Pipe clipboard through commands
clipstack paste | grep "pattern" | clipstack copy

# View recent history
clipstack list -c 20

# Use custom max entries limit
clipstack --max-entries 500 daemon

# Check the database; rebuild a damaged search index
clipstack check
clipstack check --repair

# Back up the history, or query it with jq
clipstack export -o ~/clipstack-backup.jsonl
clipstack export | jq -s 'map(select(.pinned)) | .[].content'

# Check storage stats
clipstack stats

# Check system health
clipstack status
```

### Shell Completions

Generate and install shell completions for tab-completion of commands and options:

```bash
# Bash - add to ~/.bashrc
clipstack completions bash > ~/.local/share/bash-completion/completions/clipstack

# Zsh - add to ~/.zshrc or place in fpath
clipstack completions zsh > ~/.local/share/zsh/site-functions/_clipstack

# Fish
clipstack completions fish > ~/.config/fish/completions/clipstack.fish

# Elvish
clipstack completions elvish >> ~/.elvish/rc.elv

# PowerShell
clipstack completions powershell >> $PROFILE
```

### Picker UI

Launch with `clipstack` or `clipstack pick`:

```
┌─Search (/ to search, type to filter)────────────────┐
│                                                     │
└─────────────────────────────────────────────────────┘
┌─History (5/5) - 1 pinned─────┐┌─Preview─────5m─1.2KB──┐
│> ★ 14:32 [  1.2KB] function  ││const example = () => │
│    14:30 [   45B] hello worl ││  return "hello";     │
│    14:28 [  256B] {"type":"j ││};                    │
│    14:25 [   12B] quick text ││                      │
│    14:20 [  3.1KB] import Re ││                      │
└──────────────────────────────┘└──────────────────────┘
[NORMAL] j/k:Nav  /:Search  Tab:Preview  Enter:Paste  p:Pin  d:Del  u:Undo  q:Quit
```

The picker uses vim-style modal navigation with multiple modes:

**Normal Mode** (default):
| Key | Action |
|-----|--------|
| `j` / `↓` | Move selection down |
| `k` / `↑` | Move selection up |
| `G` | Jump to last entry |
| `gg` | Jump to first entry |
| `Ctrl+D` / `Page Down` | Jump down 10 entries |
| `Ctrl+U` / `Page Up` | Jump up 10 entries |
| `/` | Enter search mode |
| `p` | Toggle pin on selected entry (★) |
| `d` | Delete selected entry |
| `u` | Undo delete (5 second window) |
| `Tab` | Enter preview scroll mode |
| `Enter` | Copy selected entry to clipboard and exit |
| `Esc` / `q` | Exit without copying |
| _any letter_ | Start typing to filter (enters search mode) |

**Search Mode** (active when typing):
| Key | Action |
|-----|--------|
| _type_ | Filter entries by fuzzy search |
| `↑` / `↓` | Navigate while searching |
| `Ctrl+N` / `Ctrl+P` | Navigate (vim style) |
| `Backspace` | Delete character (exits search if empty) |
| `Enter` | Copy selected entry to clipboard and exit |
| `Esc` | Exit search mode (return to normal) |

**Preview Mode** (after pressing `Tab`):
| Key | Action |
|-----|--------|
| `j` / `k` / `↑` / `↓` | Scroll one line |
| `Page Up` / `Page Down` | Scroll one page |
| `g` / `Home` | Jump to top of content |
| `G` / `End` | Jump to bottom of content |
| `Tab` / `Esc` / `q` | Exit preview mode |

**Visual Features:**
- Mode indicator shows `[NORMAL]`, `[SEARCH]`, or `[PREVIEW]` in status bar
- Pinned entries show `★` indicator and appear at the top of the list
- Matched characters highlighted in **yellow** during fuzzy search
- `[content]` indicator for matches found in full content (not just preview)
- Scrollbar shows position in long lists
- Relative timestamps (e.g., "5m ago", "2h ago")
- Entry size displayed in human-readable format (e.g., "1.2KB")
- Status messages for actions (delete confirmation, undo countdown)
- Preview shows `[+N lines, Tab to scroll]` for truncated content

**Auto-Start:** Opening the picker automatically starts the background daemon if it isn't running. The daemon is the same executable as the picker (not whichever `clipstack` is first on `PATH`), uses the same `--storage-dir` and keeps the saved entry limit.

### Search Features

The picker fuzzy-matches your query against each entry's 100-character preview, then asks the database for entries whose full content contains it:

- Every word of the query must appear in the content (case-insensitive substring match, in any order)
- Words of 3 or more characters use an FTS5 trigram index over the first 256 K characters (262,144) of each entry; shorter words are matched with a substring scan
- Up to 200 full-content matches are shown, after the preview matches, with a `[content]` indicator

There is no cap on how many entries are searched, and nothing is read from disk file by file.

### Pinned Entries

Pin frequently-used text to protect it from automatic pruning:

- Press `p` to toggle pin status on selected entry
- Pinned entries show a `★` indicator
- Pinned entries always appear at the top of the list (most recently used first)
- Maximum 25 pinned entries allowed
- Pinned entries survive when the history limit is exceeded
- Re-copying pinned content moves it to the top while preserving pin status

## Running the Daemon

The daemon monitors your clipboard, automatically saving new entries to history.

### Manual Start

```bash
# Run in foreground (for testing)
clipstack daemon

# Run in background
nohup clipstack daemon > /tmp/clipstack.log 2>&1 &

# Stop the running daemon
clipstack kill
```

The daemon stops cleanly on SIGINT, SIGTERM and SIGHUP: Ctrl+C, `clipstack kill`, `kill <pid>` and `systemctl --user stop` all make it print `clipstack daemon stopped` and exit with status 0.

### Systemd User Service

```bash
# Install the service file
mkdir -p ~/.config/systemd/user
cp systemd/clipd.service ~/.config/systemd/user/clipstack.service

# Enable and start
systemctl --user enable clipstack.service
systemctl --user start clipstack.service

# Check status
systemctl --user status clipstack.service

# View logs
journalctl --user -u clipstack.service -f
```

### Hyprland Autostart

Add to `~/.config/hypr/autostart.conf`:

```bash
exec-once = clipstack daemon
```

To bind the picker to a hotkey, add to `~/.config/hypr/bindings.conf`:

```bash
bind = $mainMod CTRL, B, exec, [float;size 800 600;center] alacritty --class clipstack-picker -e clipstack pick
```

And add window rules to `~/.config/hypr/hyprland.conf`:

```bash
windowrulev2 = float, class:^(clipstack-picker)$
windowrulev2 = size 800 600, class:^(clipstack-picker)$
windowrulev2 = center, class:^(clipstack-picker)$
```

### Sway Autostart

Add to `~/.config/sway/config`:

```bash
exec clipstack daemon
bindsym $mod+Ctrl+b exec alacritty --class clipstack-picker -e clipstack pick
for_window [app_id="clipstack-picker"] floating enable, resize set 800 600
```

## Remote Clipboard (SSH)

Copy text from a remote server to your local clipboard via SSH tunnel.

### Setup

**On your local machine:**

```bash
# Start the clipboard server
clipstack serve
# Output: Clipboard server listening on 127.0.0.1:7779
```

**SSH to remote with reverse tunnel:**

```bash
ssh -R 7779:localhost:7779 user@remote-server
```

**On the remote server:**

```bash
# Install the rcopy script (from this repo)
sudo cp scripts/rcopy /usr/local/bin/
sudo chmod +x /usr/local/bin/rcopy

# Usage examples:
echo "text from remote" | rcopy
cat /var/log/syslog | head -50 | rcopy
rcopy < /etc/hostname
```

### How It Works

1. `clipstack serve` listens on localhost:7779
2. SSH reverse tunnel forwards remote:7779 to local:7779
3. `rcopy` sends stdin to localhost:7779 via netcat
4. Local `clipstack serve` receives data, saves to history, and copies to system clipboard

### Security

The TCP server includes protections:
- Reads data in 8KB chunks with a 10MB total limit
- Rejects non-UTF-8 content
- Binds only to localhost by default

### Persistent SSH Tunnel

Add to `~/.ssh/config`:

```
Host myserver
    HostName server.example.com
    User myuser
    RemoteForward 7779 localhost:7779
```

Then simply: `ssh myserver`

## Storage

Clipboard history is stored in `~/.local/share/clipd/`:

```
~/.local/share/clipd/
├── clipstack.db          # SQLite database: entries, settings and the FTS5 search index
├── clipstack.db-wal      # Write-ahead log (SQLite WAL mode)
├── clipstack.db-shm      # WAL index
└── legacy-backup-<ms>/   # Only after an upgrade: the old files, see below
```

Clipboard history often contains passwords and tokens, so the storage directory is created `0700` and the database, WAL and SHM files `0600` (owner only). Stores written by older versions (`0755` directory, `0644` files) are tightened automatically the next time any command opens them; ClipStack prints `[storage] tightened permissions on N files` once when that happens. Only regular files directly inside the directory are changed: symlinks and subdirectories are never followed, and files owned by another user are left alone.

Deleting an entry (or `clear`, or expiry) overwrites its text inside the database (SQLite `secure_delete` and the FTS5 `secure-delete` option) and then truncates the WAL, so the deleted text doesn't linger in the files. If another process (an open picker) is reading at that moment, the WAL is truncated by the next delete instead.

### Upgrading from the JSON store

Versions before the SQLite store kept `index.json` plus one `<id>.txt` file per entry. **Run `clipstack kill` before the first command of the new version**: an old daemon left running keeps writing `index.json`, and every later command would import those writes again and leave another backup directory.

The first command of the new version imports the old store automatically:

- It takes the old `index.lock`, so a still-running old daemon can't change the files mid-import, and imports every entry with its timestamp and pinned flag in one transaction. Identical contents are merged. Only the entry limit applies during the import (the old version enforced it too); ignore patterns never apply to imported entries, and retention and the size caps apply from the next start.
- Only after the transaction commits are `index.json`, the `.txt` files and any `.tmp` leftovers moved into `legacy-backup-<ms>/` (`0700`). `index.lock` stays where it is. Nothing is deleted.
- It prints `Imported N entries from index.json; old files kept in .../legacy-backup-<ms>/ (delete after checking)` once. If retention is on, it also says how many imported entries will expire at the next start.
- `status` and `check` keep listing the backup until you delete it. The backup holds your old history as plain text files, so delete it once `clipstack list` looks right.

**Downgrading:** an older version doesn't read `clipstack.db`. It starts with an empty history and writes a new `index.json`; the next command of this version imports that as well. A database written by a newer version than the one running is refused with an "upgrade clipstack" error.

### Storage Limits

| Limit | Default | Notes |
|-------|---------|-------|
| Max entries | 100 | Unpinned entries; 1-10,000 via `--max-entries` or `CLIPSTACK_MAX_ENTRIES`. Oldest unpinned entries are deleted first. The limit is saved in the database |
| Max pinned | 25 | Pinned entries are exempt from every limit below |
| Retention | off | `retention_days` in the config file: unpinned entries last used longer ago are deleted at open, on each copy and once a minute in the daemon |
| Max entry size | 10 MiB | `max_entry_bytes` (up to 64 MiB). Larger copies still reach the clipboard but aren't saved |
| Total size | 256 MiB | `max_total_bytes`: unpinned content above this total is deleted, oldest first |
| Search window | 256 K characters | Full-content search covers the first 262,144 characters of each entry |
| Preview | 100 characters | Shown in lists |

### Data Safety

- **Private permissions**: storage dir `0700`, database files `0600`; existing stores are tightened on open
- **Transactions**: every change (save, pin, delete, prune) is one SQLite transaction in WAL mode, so a crash or power loss leaves the history as it was before or after the change, never in between
- **Concurrent processes**: the daemon, picker, CLI and server each open the database; writers wait up to 5 seconds for each other and readers never block writers, so concurrent changes are never lost
- **Integrity check**: `clipstack check` runs SQLite's integrity check and the search index's consistency check; `clipstack check --repair` rebuilds a damaged search index from the entries
- **Duplicate detection**: identical content is deduplicated by SHA256 hash
- **Secret filtering**: copies that match the built-in secret patterns or your `ignore_patterns` are never written to the database

### Custom Storage Location

```bash
clipstack --storage-dir /path/to/custom/dir daemon
clipstack --storage-dir /path/to/custom/dir list
```

The directory is treated as ClipStack's own: it is created if missing and chmod'ed to `0700`, and its files to `0600`, even if it already existed. Don't point `--storage-dir` at a shared directory.

### Inspecting Storage

`clipstack export` writes one JSON object per entry, oldest first, with the fields `id`, `created_at`, `last_used` (Unix ms), `size`, `pinned`, `alias`, `source`, `mime` and `content`:

```bash
# The 5 most recent entries, without their content
clipstack export | jq -s 'reverse | .[:5] | map(del(.content))'

# Full content of the most recent entry
clipstack export | jq -rs '.[-1].content'

# Count entries
clipstack export | jq -s 'length'

# Large entries (over 10KB)
clipstack export | jq -c 'select(.size > 10000) | {id, size, last_used}'

# Pinned entries
clipstack export | jq -c 'select(.pinned) | {id, content}'

# Save a backup (created 0600; refuses to overwrite without --force)
clipstack export -o ~/clipstack-backup.jsonl
```

You can also read the database directly. Open it read-only, so you can't block or race the daemon:

```bash
sqlite3 -readonly ~/.local/share/clipd/clipstack.db \
  "SELECT id, datetime(last_used / 1000, 'unixepoch'), pinned, preview
   FROM entries ORDER BY last_used DESC LIMIT 10"
```

## Configuration

### Configuration File

ClipStack reads `$XDG_CONFIG_HOME/clipstack/config.toml` (usually `~/.config/clipstack/config.toml`), or the file given with `--config PATH`. The file is optional; without it the defaults below apply. Every key is optional, and an unknown key or invalid value is an error that names the key, reported before anything is opened. The full file with the defaults:

```toml
[storage]
# Delete unpinned entries last used more than this many days ago; 0 keeps them forever.
# The first start after an upgrade imports the whole old history, so a non-zero value
# deletes the old part of it at the start after that.
retention_days = 0

# Copies larger than this still reach the clipboard but aren't saved (1 byte to 64 MiB).
max_entry_bytes = 10485760   # 10 MiB

# Unpinned content above this total is deleted, oldest first (at least max_entry_bytes).
max_total_bytes = 268435456  # 256 MiB

# Copies matching any of these regexes (Rust regex syntax, up to 64) aren't saved.
# A pattern matches anywhere in the content unless anchored with ^/$ or \A/\z.
ignore_patterns = []

# Also skip private keys anywhere in a copy, and copies that are nothing but a GitHub,
# AWS access key, Slack or sk- style API token.
builtin_secret_patterns = true
```

When a `clipstack copy` isn't saved because of these settings, the text is still copied to the clipboard and ClipStack says why (`Copied N bytes (not saved to history: matches ignore_patterns[0])`); the reason never includes the copied text. The entry limit is not in this file: it comes from `--max-entries`, `CLIPSTACK_MAX_ENTRIES` or the saved value.

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `CLIPSTACK_MAX_ENTRIES` | Maximum entries to store (1-10000); overridden by `--max-entries` | saved limit, else `100` |
| `CB_PORT` | Port for remote clipboard server/client | `7779` |

### Recommended Setup

Add to your shell rc file:

```bash
# Set custom history limit
export CLIPSTACK_MAX_ENTRIES=500

# Start daemon on login (if not using systemd)
pgrep -x clipstack > /dev/null || clipstack daemon &
```

### Status Command

The `clipstack status` command provides a comprehensive health check:

```
$ clipstack status
Daemon:  running  (or: not running)

Storage:  /home/user/.local/share/clipd
Database: /home/user/.local/share/clipd/clipstack.db
Entries:  42 (3 pinned)
Size:     156.3KB
Latest:   2m ago

Config:
  File:        /home/user/.config/clipstack/config.toml (not found; using defaults)
  Max entries: 100 (saved)
  Retention:   off
  Entry cap:   10.0MB
  Total cap:   256.0MB (unpinned)
  Ignore:      0 patterns + built-in secret patterns

Wayland: detected  (or: not detected)
```

This helps diagnose issues with:
- **Daemon not running**: Entries won't be saved automatically
- **Wayland not detected**: ClipStack requires a Wayland session
- **Storage issues**: Shows where data is stored and current usage, and lists any `legacy-backup-*` directory left by an upgrade
- **Unexpected limit**: Shows where the entry limit comes from: `cli` (`--max-entries`), `env` (`CLIPSTACK_MAX_ENTRIES`), `saved` (in the database), or `default`
- **Unexpected skips**: Shows which config file is in effect, the retention, the size caps and how many ignore patterns apply

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        ClipStack                                │
├─────────────────────────────────────────────────────────────────┤
│  CLI (main.rs)                                                  │
│  ├── copy/paste    → clipboard.rs (wl-copy/wl-paste wrapper)   │
│  ├── pick          → picker.rs (TUI with ratatui)              │
│  ├── daemon        → daemon.rs (polling loop + PID lock file)  │
│  ├── kill          → Signal the daemon PID from its lock file  │
│  ├── serve         → TCP server for remote clipboard           │
│  ├── check/export  → Integrity check, JSONL export             │
│  ├── status        → Health check (daemon, wayland, storage)   │
│  └── list/stats    → storage/ (SQLite queries)                 │
├─────────────────────────────────────────────────────────────────┤
│  Config (config.rs)                                             │
│  └── config.toml   → retention, size caps, ignore patterns     │
├─────────────────────────────────────────────────────────────────┤
│  Storage (storage/)                                             │
│  ├── schema.rs     → clipstack.db: WAL mode, entries + FTS5    │
│  ├── ingest.rs     → Save/dedup, skip rules, limits, expiry    │
│  ├── query.rs      → list, search, counts, export              │
│  ├── legacy.rs     → One-time import of the old JSON store     │
│  └── perms.rs      → 0700 dir / 0600 files, tightened on open  │
├─────────────────────────────────────────────────────────────────┤
│  Picker (picker.rs)                                             │
│  ├── Search (preview fuzzy match + FTS5 content search)        │
│  ├── Focus modes (List, Preview)                               │
│  └── Pin management                                             │
├─────────────────────────────────────────────────────────────────┤
│  External                                                       │
│  ├── wl-copy       → Write to Wayland clipboard                │
│  └── wl-paste      → Read from Wayland clipboard               │
└─────────────────────────────────────────────────────────────────┘
```

### Key Design Decisions

1. **Metadata-First Reads**: Lists read only metadata and the 100-char preview; the content column is stored last, so listing never touches a large entry's text. Full content is loaded on demand.
2. **Indexed Search**: An FTS5 trigram index (external content, first 256 K characters per entry) answers full-content queries without scanning entries one by one.
3. **SQLite Transactions**: Every change is one transaction in WAL mode. Crashes never leave the history half-written.
4. **Polling vs Events**: Uses 250ms polling instead of `wl-paste --watch` for reliability across different Wayland compositors.
5. **SHA256 Deduplication**: Hashes content to prevent duplicates; re-copying moves the existing entry to the top of the list and refreshes its timestamp.
6. **One Database File**: SQLite (compiled in) instead of a JSON index plus one file per entry. `clipstack export` and `sqlite3 -readonly` keep the data inspectable.
7. **Locking**: A global daemon lock (which also records the daemon PID) allows only one daemon at a time. SQLite's own locking serializes writes from every process.
8. **Modal UI Pattern**: Vim-style normal/search/preview modes keep navigation keyboard-only and predictable.
9. **Pin Protection**: Pinned entries exempt from automatic pruning, with configurable limits to prevent abuse.

### Design Philosophy

ClipStack is built around several core principles:

**Simplicity Over Features**
- Single binary with zero runtime dependencies (besides wl-clipboard)
- One SQLite file, inspectable with `clipstack export | jq` or `sqlite3 -readonly`
- No database server, no background services (except the daemon), an optional config file

**Performance by Design**
- Listing 100 entries: < 10ms, whatever their size
- Content load time: < 20ms even for 500KB entries
- 250ms polling interval balances responsiveness with CPU usage
- The search index answers full-content queries without reading every entry

**Reliability First**
- SQLite transactions prevent data corruption on crash or power loss
- A lock file prevents multiple daemon instances; SQLite prevents lost updates between processes
- SHA256 hashing ensures exact duplicate detection
- `clipstack check --repair` verifies the database and rebuilds the search index
- Graceful handling of empty clipboard, missing files, and Unicode edge cases
- Preview sanitizes control characters to prevent display corruption

**Vim-Native Workflow**
- Modal interface feels natural to vim users
- `j/k` navigation, `gg/G` jumps, `/` for search
- Single-key commands (`d` delete, `u` undo, `p` pin)
- `Tab` switches between list and preview focus
- Auto-start typing enters search mode without explicit `/`

**Unix Philosophy**
- Does one thing well: manages clipboard history
- Composable with pipes: `cat file | clipstack copy`, `clipstack paste | grep pattern`
- Works with SSH tunnels for remote clipboard support
- Generates shell completions for better terminal integration

### Performance Benchmarks

Targets checked by `cargo test --release -- --ignored perf_` with 100 entries of 500 KB each (measured on a development machine in brackets):

| Operation | Target | Notes |
|-----------|--------|-------|
| List entries | < 10ms (0.1ms) | 100 entries, metadata only |
| Content load | < 20ms (0.3ms) | 500KB entry |
| Save entry | < 100ms (85ms) | 500KB, including hash and indexing its first 256 K characters |
| Full-content search | < 20ms (0.5ms) | Across all 100 entries |

**Storage efficiency:**
- Deduplication: Re-copying identical content reuses existing entry
- Automatic pruning: Old unpinned entries removed when a limit is reached
- The search index is about 3-5x the size of the indexed text (at most the first 256 K characters per entry)

### Data Handling

ClipStack handles various content types and edge cases gracefully:

**Unicode Support**
- Full UTF-8 support for all operations (copy, paste, search, preview)
- Multi-byte characters (emoji, CJK, etc.) handled correctly in previews
- Line wrapping uses character count, not byte count
- Fuzzy search works across all Unicode text

**Edge Cases**
- Empty clipboard: Silently ignored, no error entries created
- Whitespace-only content: Saved with sanitized preview
- Binary content: Treated as text; non-UTF-8 bytes may cause errors
- Very large entries: Saved up to `max_entry_bytes` (10 MiB by default); larger copies reach the clipboard but aren't saved
- Control characters: Stripped from preview display, preserved in content

**Preview Generation**
- First 100 *characters* (not bytes) for correct Unicode truncation
- Control characters (tabs, newlines, etc.) replaced with spaces
- Original content preserved in full in the database

## Troubleshooting

### "Failed to spawn wl-copy"

```bash
# Check wl-clipboard is installed
which wl-copy wl-paste

# Check you're in a Wayland session
echo $WAYLAND_DISPLAY
# Should output something like "wayland-0" or "wayland-1"
```

### Daemon not saving entries

```bash
# Check daemon is running
clipstack status

# Run in foreground to see output
clipstack daemon
# Should print "[clipboard] Saved: X bytes..." on copy
```

### Corrupted storage

```bash
# Check the database and the search index
clipstack check

# Search misses entries it should find: rebuild the search index
clipstack check --repair
```

If `check` reports database damage, it prints what to do: save what can still be read with `clipstack export -o backup.jsonl`, then rebuild the database with the `sqlite3` command it shows.

### Remote copy not working

```bash
# On local: verify server is running
ss -tlnp | grep 7779

# On remote: verify tunnel exists
ss -tlnp | grep 7779

# On remote: test manually
echo "test" | nc localhost 7779
```

### Picker UI looks broken

```bash
# Ensure terminal supports alternate screen
echo $TERM
# Should be something like "xterm-256color" or "alacritty"

# Try a different terminal emulator
alacritty -e clipstack pick
```

## Development

### Running Tests

```bash
# Unit and integration tests (no Wayland required)
cargo test

# Performance targets (release build only)
cargo test --release -- --ignored perf_

# Clipboard round-trip tests: these use the REAL wl-copy/wl-paste and
# overwrite your desktop clipboard (requires a Wayland session). They are
# skipped unless CLIPSTACK_REAL_CLIPBOARD_TESTS is set, even with --ignored.
CLIPSTACK_REAL_CLIPBOARD_TESTS=1 cargo test -- --ignored test_copy_and_paste test_large_content test_unicode_content

# Shell suites (need a release build and jq)
cargo build --release
for t in tests/e2e_*.sh tests/integration/*.sh; do bash "$t" || break; done
```

Tests that run the `clipstack` binary are isolated from your real clipboard, history and daemon. Rust tests use `TestEnv` in `tests/common/mod.rs`; every shell suite sources `tests/lib.sh` and calls `isolate_env`. The shell suites build old-format stores and check the import through `clipstack export | jq`. `TestEnv` and `isolate_env` point `XDG_RUNTIME_DIR`, `XDG_DATA_HOME`, `XDG_CONFIG_HOME` and `HOME` at a temp directory, put the fake `wl-paste`/`wl-copy` in `tests/fakes/` first on `PATH`, unset `WAYLAND_DISPLAY`, and only ever kill processes they started themselves.

### Building

```bash
# Debug build
cargo build

# Release build (recommended for installation)
cargo build --release

# Check for warnings
cargo clippy
```

### Project Structure

```
clipstack/
├── Cargo.toml           # Dependencies and metadata
├── src/
│   ├── main.rs          # CLI entry point, subcommands
│   ├── clipboard.rs     # Wayland clipboard operations
│   ├── config.rs        # config.toml loading and validation
│   ├── daemon.rs        # Background monitoring daemon
│   ├── perms.rs         # Private (0700/0600) storage permissions
│   ├── picker.rs        # TUI history picker
│   ├── storage/         # SQLite storage
│   │   ├── mod.rs       # Storage: open, pin/alias/delete/clear, check
│   │   ├── schema.rs    # Schema, PRAGMAs, migrations
│   │   ├── ingest.rs    # Saving, skip rules, limits and expiry
│   │   ├── query.rs     # list, search, counts, export
│   │   └── legacy.rs    # One-time import of the old JSON store
│   └── util.rs          # Formatting utilities (size, time)
├── tests/
│   ├── common/mod.rs    # TestEnv isolation harness for binary tests
│   ├── daemon_lifecycle.rs  # Daemon start/stop integration tests
│   ├── migration_cli.rs     # Import, config, export and check CLI tests
│   ├── fakes/           # Fake wl-paste/wl-copy (never touch the real clipboard)
│   ├── lib.sh           # isolate_env/cleanup for shell suites
│   ├── e2e_*.sh         # Shell end-to-end suites
│   └── integration/     # Shell integration suites
├── scripts/
│   └── rcopy            # Remote copy helper script
└── systemd/
    └── clipd.service    # Systemd user service file
```

## License

MIT

## Credits

Built with:
- [clap](https://github.com/clap-rs/clap) - CLI argument parsing
- [ratatui](https://github.com/ratatui/ratatui) - Terminal UI framework
- [crossterm](https://github.com/crossterm-rs/crossterm) - Terminal manipulation
- [fuzzy-matcher](https://github.com/lotabout/fuzzy-matcher) - Fuzzy search
- [wl-clipboard](https://github.com/bugaevc/wl-clipboard) - Wayland clipboard utilities

# ClipStack

A fast, keyboard-driven clipboard manager for Linux/Wayland. ClipStack provides lazy-loading history, vim-style navigation, fuzzy search with deep content matching, and remote clipboard support via SSH tunnels.

Built for developers who live in the terminal and want a clipboard manager that stays out of the way until needed.

## Features

**Core Functionality**
- **Clipboard History**: Automatically saves clipboard entries with SHA256 deduplication
- **Pinned Entries**: Mark important items with `p` to protect them from automatic pruning
- **TUI Picker**: Fuzzy-searchable history picker with live preview and vim keybindings
- **Remote Clipboard**: Copy from SSH sessions to your local clipboard via TCP tunnel

**Performance**
- **Lazy Loading**: Index contains only metadata; full content loaded on demand
- **Two-Phase Search**: Searches previews first (fast), then full content for deep matches
- **Atomic Writes**: Crash-safe storage with write-then-rename pattern prevents data corruption
- **Sub-100ms Startup**: Picker opens instantly even with large history

**User Experience**
- **Vim-Style Navigation**: Modal interface with `j/k`, `gg/G`, `/` search
- **Preview Scrolling**: Press `Tab` to enter scrollable preview mode for long content
- **Fuzzy Search**: Type to filter with highlighted matches and `[content]` indicator for deep matches
- **Delete with Undo**: Remove entries with 5-second undo window
- **Configurable Limits**: Set 1-10,000 max entries via CLI or environment variable
- **Shell Completions**: Tab completion for bash, zsh, fish, elvish, powershell

## Why ClipStack?

**Problem**: Standard clipboard managers either lack features (like search), require heavy GUI frameworks, or don't integrate well with terminal-centric workflows.

**Solution**: ClipStack is designed for developers who:
- Spend most of their time in terminals and text editors
- Want vim-style keybindings without learning a new paradigm
- Need to copy text from remote servers to local clipboard
- Value simplicity: single binary, JSON storage, zero configuration

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

- Rust 1.85+ (2024 edition)
- Cargo

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
| `clipstack recover` | Rebuild index from content files (for corrupted storage) |
| `clipstack serve [-p PORT]` | Start TCP server for remote clipboard (default: 7779) |
| `clipstack completions <shell>` | Generate shell completions (bash, zsh, fish, elvish, powershell) |

### Global Options

| Option | Description |
|--------|-------------|
| `--storage-dir PATH` | Custom storage directory |
| `--max-entries N` | Maximum entries to store (1-10000, default: 100). Saved to the index and reused by later runs |

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

# Recover from corrupted storage
clipstack recover

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

**Auto-Start:** Opening the picker automatically starts the background daemon if it isn't running. The daemon uses the same `--storage-dir` as the picker and keeps the saved entry limit.

### Search Features

ClipStack uses a two-phase search algorithm:

1. **Phase 1 (Fast)**: Searches the 100-character preview stored in memory
2. **Phase 2 (Deep)**: For non-matches, loads and searches full content from disk

When a match is found in full content but not in the preview, a `[content]` indicator appears in the list. This allows finding text that appears deep within clipboard entries.

Search performance limits ensure responsive UI:
- Stops after finding 10 content matches
- Stops after loading 50 files

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
├── index.json              # Metadata index (timestamps, hashes, previews)
├── index.lock              # Lock that serializes index updates across processes
└── {timestamp}_{nnn}.txt   # Full content files (named by unique ID)
```

### Storage Limits

| Limit | Value | Notes |
|-------|-------|-------|
| Max entries | 100 (default) | Configurable 1-10,000; oldest unpinned entries pruned. The limit is saved in `index.json` |
| Max pinned | 25 | Pinned entries exempt from pruning |
| Max preview | 100 characters | Stored in index for fast display |
| Max entry size | Unlimited | Each entry stored in separate file |

### Index Format

The `index.json` file contains entry metadata for fast loading:

```json
{
  "max_entries": 100,
  "entries": [
    {
      "id": "1736789123456_123",
      "timestamp": 1736789123456,
      "size": 1234,
      "preview": "First 100 characters of content...",
      "hash": "sha256:a1b2c3d4e5...",
      "pinned": false
    }
  ]
}
```

| Field | Description |
|-------|-------------|
| `id` | Unique identifier: millisecond timestamp + 3-digit suffix that never reuses an existing ID |
| `timestamp` | Unix timestamp in milliseconds of the last copy (refreshed when identical content is copied again) |
| `size` | Content size in bytes |
| `preview` | First 100 characters (control chars sanitized) |
| `hash` | SHA256 hash for deduplication |
| `pinned` | Whether entry is protected from pruning |

### Data Safety

- **Atomic writes**: All file writes use write-to-temp, fsync, then atomic rename
- **Index lock**: Daemon, picker, and server updates are serialized via `index.lock`, so concurrent changes are never lost
- **Orphaned temp cleanup**: Interrupted writes are cleaned up on startup
- **Recovery command**: `clipstack recover` rebuilds the index from content files
- **Duplicate detection**: Identical content is deduplicated by hash

### Custom Storage Location

```bash
clipstack --storage-dir /path/to/custom/dir daemon
clipstack --storage-dir /path/to/custom/dir list
```

### Inspecting Storage Manually

The storage format is designed to be human-readable:

```bash
# Read the index directly
cat ~/.local/share/clipd/index.json | jq '.entries[:5]'

# Get full content of most recent entry
ID=$(cat ~/.local/share/clipd/index.json | jq -r '.entries[0].id')
cat ~/.local/share/clipd/${ID}.txt

# Count entries
cat ~/.local/share/clipd/index.json | jq '.entries | length'

# Find large entries (over 10KB)
cat ~/.local/share/clipd/index.json | jq '.entries[] | select(.size > 10000)'

# Find pinned entries
cat ~/.local/share/clipd/index.json | jq '.entries[] | select(.pinned)'
```

## Configuration

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

Storage: "/home/user/.local/share/clipd"
Entries: 42/100
Size:    156.3KB
Latest:  2m ago

Config:
  Max entries: 100 (saved)

Wayland: detected  (or: not detected)
```

This helps diagnose issues with:
- **Daemon not running**: Entries won't be saved automatically
- **Wayland not detected**: ClipStack requires a Wayland session
- **Storage issues**: Shows where data is stored and current usage
- **Unexpected limit**: Shows where the entry limit comes from: `--max-entries`, `CLIPSTACK_MAX_ENTRIES`, `saved` (from the index), or `default`

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
│  ├── recover       → Rebuild index from content files          │
│  ├── status        → Health check (daemon, wayland, storage)   │
│  └── list/stats    → storage.rs (JSON index + content files)   │
├─────────────────────────────────────────────────────────────────┤
│  Storage (storage.rs)                                           │
│  ├── index.json    → Entry metadata (id, timestamp, hash, etc) │
│  ├── {id}.txt      → Full content files (one per entry)        │
│  ├── index.lock    → Cross-process lock for index updates      │
│  └── atomic_write  → Write-temp-fsync-rename pattern           │
├─────────────────────────────────────────────────────────────────┤
│  Picker (picker.rs)                                             │
│  ├── Two-phase search (preview fast, content lazy)             │
│  ├── Focus modes (List, Preview)                               │
│  └── Pin management                                             │
├─────────────────────────────────────────────────────────────────┤
│  External                                                       │
│  ├── wl-copy       → Write to Wayland clipboard                │
│  └── wl-paste      → Read from Wayland clipboard               │
└─────────────────────────────────────────────────────────────────┘
```

### Key Design Decisions

1. **Lazy Loading**: Index contains only metadata + 100-char preview. Full content loaded on demand, keeping memory usage low.
2. **Two-Phase Search**: Previews searched first (instant), then full content loaded only for non-matches. Performance limits prevent UI freezes.
3. **Atomic Writes**: All file operations use write-to-temp, fsync, atomic-rename pattern. Crashes never corrupt storage.
4. **Polling vs Events**: Uses 250ms polling instead of `wl-paste --watch` for reliability across different Wayland compositors.
5. **SHA256 Deduplication**: Hashes content to prevent duplicates; re-copying moves the existing entry to the top of the list and refreshes its timestamp.
6. **File-Based Storage**: Simple JSON index + separate content files. Human-readable, inspectable, no database required.
7. **Lock File Synchronization**: A global daemon lock (which also records the daemon PID) allows only one daemon at a time. An `index.lock` in the storage directory serializes index updates from every process.
8. **Modal UI Pattern**: Vim-style normal/search/preview modes keep navigation keyboard-only and predictable.
9. **Pin Protection**: Pinned entries exempt from automatic pruning, with configurable limits to prevent abuse.

### Design Philosophy

ClipStack is built around several core principles:

**Simplicity Over Features**
- Single binary with zero runtime dependencies (besides wl-clipboard)
- Human-readable JSON storage you can inspect and edit manually
- No database, no background services (except the daemon), no complex configuration

**Performance by Design**
- Index load time: < 10ms (tested with 100 entries)
- Content load time: < 50ms even for 500KB entries
- 250ms polling interval balances responsiveness with CPU usage
- Preview truncation keeps index small regardless of entry size
- Two-phase search prevents scanning entire history for every keystroke

**Reliability First**
- Atomic writes prevent data corruption on crash or power loss
- Lock files prevent multiple daemon instances and lost updates between processes
- SHA256 hashing ensures exact duplicate detection
- Recovery command can rebuild from orphaned content files
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

Tested on a typical development machine:

| Operation | Time | Notes |
|-----------|------|-------|
| Index load | < 10ms | 100 entries, any content size |
| Content load | < 50ms | 500KB entry |
| Picker startup | < 100ms | Including index load |
| Search filter | < 5ms | Fuzzy match across 100 entries |
| Save entry | < 20ms | Including hash + index update |

**Memory footprint:**
- Daemon idle: ~2MB RSS
- Picker with 100 entries: ~8MB RSS
- Index file: ~50KB for 100 entries (regardless of content size)

**Storage efficiency:**
- Deduplication: Re-copying identical content reuses existing entry
- Automatic pruning: Old unpinned entries removed when limit reached
- Separate content files: Large entries don't bloat the index

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
- Very large entries (>1MB): Supported but may impact performance
- Control characters: Stripped from preview display, preserved in content

**Preview Generation**
- First 100 *characters* (not bytes) for correct Unicode truncation
- Control characters (tabs, newlines, etc.) replaced with spaces
- Original content preserved in full in separate file

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
# Check for orphaned temp files or corrupted index
ls ~/.local/share/clipd/

# Rebuild index from content files
clipstack recover
```

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
# Unit tests (no Wayland required)
cargo test

# Integration tests (requires Wayland session)
cargo test -- --ignored
```

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
│   ├── daemon.rs        # Background monitoring daemon
│   ├── picker.rs        # TUI history picker
│   ├── storage.rs       # History storage management
│   └── util.rs          # Formatting utilities (size, time)
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

# ClipStack

A fast, keyboard-driven clipboard manager for Linux/Wayland with lazy-loading history, fuzzy search, and remote clipboard support via SSH tunnels.

## Features

- **Clipboard History**: Automatically saves clipboard entries with deduplication
- **Dual Selection Support**: Monitors both the regular clipboard (Ctrl+C) and PRIMARY selection (mouse highlight)
- **TUI Picker**: Fuzzy-searchable history picker with live preview
- **Remote Clipboard**: Copy from SSH sessions to your local clipboard via TCP tunnel
- **Lazy Loading**: Only loads full content when needed, keeping the UI snappy even with large entries
- **Content Deduplication**: SHA256 hashing prevents duplicate entries; re-copying moves existing entry to top

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
| `clipstack stats` | Show storage statistics |
| `clipstack serve [-p PORT]` | Start TCP server for remote clipboard (default: 7779) |

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

# Check storage stats
clipstack stats
```

### Picker UI

Launch with `clipstack` or `clipstack pick`:

```
┌─Search──────────────────────────────────────────────┐
│                                                     │
└─────────────────────────────────────────────────────┘
┌─History (5/5)──────────────┐┌─Preview──────────────┐
│▶ 14:32 [  1.2KB] function  ││const example = () => │
│  14:30 [   45B] hello worl ││  return "hello";     │
│  14:28 [  256B] {"type":"j ││};                    │
│  14:25 [   12B] quick text ││                      │
│  14:20 [  3.1KB] import Re ││                      │
└────────────────────────────┘└──────────────────────┘
↑↓:Navigate  Enter:Paste  Esc:Cancel  Ctrl+D:Delete
```

**Keybindings:**
- `↑`/`↓` - Navigate entries
- `Page Up`/`Page Down` - Jump 10 entries
- `Enter` - Copy selected entry to clipboard and exit
- `Esc` - Cancel and exit
- `Ctrl+D` - Delete selected entry
- Type to fuzzy search

## Running the Daemon

The daemon monitors your clipboard and PRIMARY selection, automatically saving new entries to history.

### Manual Start

```bash
# Run in foreground (for testing)
clipstack daemon

# Run in background
nohup clipstack daemon > /tmp/clipstack.log 2>&1 &
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

# Or install via cargo (if clipstack is installed on remote)
# The rcopy script will be in ~/.cargo/bin/

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
├── index.json          # Metadata index (timestamps, hashes, previews)
└── *.txt               # Full content files (named by timestamp ID)
```

### Storage Limits

- **Max entries**: 100 (oldest entries are automatically pruned)
- **Max preview**: 100 characters (stored in index)
- **Full content**: Unlimited size per entry

### Custom Storage Location

```bash
clipstack --storage-dir /path/to/custom/dir daemon
clipstack --storage-dir /path/to/custom/dir list
```

## Configuration for AI Agents

### Integration with Claude Code / AI Assistants

ClipStack can be used by AI coding assistants to manage clipboard operations:

```bash
# AI can copy generated code to clipboard
echo "const x = 42;" | clipstack copy

# AI can read current clipboard
clipstack paste

# AI can search clipboard history
clipstack list -c 50
```

### Programmatic Access

The storage format is simple JSON, making it easy to integrate:

```bash
# Read the index directly
cat ~/.local/share/clipd/index.json | jq '.entries[:5]'

# Get full content of most recent entry
ID=$(cat ~/.local/share/clipd/index.json | jq -r '.entries[0].id')
cat ~/.local/share/clipd/${ID}.txt
```

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `CB_PORT` | Port for remote clipboard server/client | `7779` |

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        ClipStack                                │
├─────────────────────────────────────────────────────────────────┤
│  CLI (main.rs)                                                  │
│  ├── copy/paste    → clipboard.rs (wl-copy/wl-paste wrapper)   │
│  ├── pick          → picker.rs (TUI with ratatui)              │
│  ├── daemon        → daemon.rs (polling loop)                  │
│  ├── serve         → TCP server (main.rs)                      │
│  └── list/stats    → storage.rs (JSON index + content files)   │
├─────────────────────────────────────────────────────────────────┤
│  Storage (storage.rs)                                           │
│  ├── index.json    → Entry metadata (id, timestamp, hash, etc) │
│  └── {id}.txt      → Full content files                        │
├─────────────────────────────────────────────────────────────────┤
│  External                                                       │
│  ├── wl-copy       → Write to Wayland clipboard                │
│  └── wl-paste      → Read from Wayland clipboard/PRIMARY       │
└─────────────────────────────────────────────────────────────────┘
```

### Key Design Decisions

1. **Lazy Loading**: Index contains only metadata + 100-char preview. Full content loaded on demand.
2. **Polling vs Events**: Uses 250ms polling instead of wl-paste --watch for reliability across compositors.
3. **Dual Selection**: Monitors both clipboard (Ctrl+C) and PRIMARY (mouse selection) for complete coverage.
4. **SHA256 Deduplication**: Prevents duplicate entries; re-copying existing content moves it to top.
5. **File-Based Storage**: Simple, inspectable, git-friendly. No database required.

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
pgrep -f "clipstack daemon"

# Run in foreground to see output
clipstack daemon
# Should print "[clipboard] Saved: X bytes..." on copy
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
│   └── storage.rs       # History storage management
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

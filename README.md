# ClipStack

A fast, keyboard-driven clipboard manager for Linux/Wayland. ClipStack keeps its history in a single SQLite database and provides vim-style navigation, fuzzy search with full-content matching, a scriptable command line (`get`, `search`, JSON output, stable exit codes), and remote copy and paste from SSH sessions over a forwarded Unix socket.

Built for developers who live in the terminal and want a clipboard manager that stays out of the way until needed.

## Features

**Core Functionality**
- **Clipboard History**: Automatically saves clipboard entries with SHA256 deduplication; event-driven through `wl-paste --watch`, with polling where the compositor can't watch
- **Pinned Entries**: Mark important items with `p` to protect them from automatic pruning
- **TUI Picker**: Fuzzy-searchable history picker with live preview and vim keybindings
- **Remote Clipboard**: Copy from SSH sessions to your local clipboard with `rcopy`, and paste from it with `rpaste` (opt-in), over a forwarded Unix socket
- **Scripting**: `get` an entry by position, id or alias, `list --json`, `search --json` over the full-content index, and stable exit codes
- **Named Snippets**: `clipstack alias 3 sig` pins an entry as `@sig`; `clipstack get @sig` prints it
- **Images**: PNG copies (screenshots) are saved too, up to `max_image_bytes`; `get` writes them back as PNG, `get --copy` and the picker put them back on the clipboard as `image/png`
- **Secret Filtering**: Password-manager copies are never saved; built-in patterns skip private keys and API tokens; add your own regexes in the config file
- **Retention and Size Caps**: Optionally expire old entries and cap entry and total size

**Performance**
- **SQLite Storage**: One WAL-mode database; lists read only metadata and full content is loaded on demand
- **Full-Content Search**: An FTS5 trigram index covers the first 256 K characters of every entry
- **Transactional Writes**: Every change is a SQLite transaction, so a crash never leaves a half-written history
- **Sub-100ms Startup**: Picker opens instantly even with large history

**User Experience**
- **Vim-Style Navigation**: Modal interface with `j/k`, `gg/G`, `/` search
- **Preview Scrolling**: Press `Tab` to enter scrollable preview mode for long content
- **Fuzzy Search**: Press `/` and type to filter, with highlighted matches and a `[content]` indicator for deep matches
- **Delete with Undo**: Remove entries with `dd`, with a 5-second undo window (`u`)
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

**Optional:** [wtype](https://github.com/atx/wtype) for `clipstack pick --paste`, which pastes the chosen entry into the window you were in (`sudo pacman -S wtype`, `sudo apt install wtype`, `sudo dnf install wtype`). It needs a compositor with the virtual-keyboard protocol (Hyprland, Sway and other wlroots compositors; not GNOME). Without it `--paste` still copies.

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
| `clipstack pick --paste` | Open the picker; after copying, paste into the window that had focus (needs `wtype`; see [Paste after picking](#paste-after-picking)) |
| `clipstack copy` | Copy stdin to clipboard |
| `clipstack paste` | Paste clipboard contents to stdout |
| `clipstack list [-c N] [--json] [--pinned]` | List the last N entries, newest first (default: 10); `--json` prints a JSON array, `--pinned` only pinned entries |
| `clipstack get REF [--copy \| --paste \| --image-path]` | Print one entry's content exactly as stored, no newline added (an image's PNG bytes, refused on a terminal); `--copy` also puts it on the clipboard and makes it the newest entry (an image is copied as `image/png` and not printed); `--paste` copies it, then pastes it into the window that had focus like `pick --paste`; `--image-path` prints the path of the image's PNG file (see [Images](#images)). `REF` is `N` (position, 1 = newest), `id:ID` or `@alias` |
| `clipstack pin REF` / `clipstack unpin REF` | Pin an entry, or unpin it (which removes its alias) |
| `clipstack delete REF` | Delete one entry |
| `clipstack edit REF` | Edit a text entry in `$VISUAL` / `$EDITOR`; a changed result is saved as a new entry and copied (as the picker's `e`) |
| `clipstack search QUERY... [-c N] [--json]` | Entries containing every word, pinned first, then newest (default: 10 matches); exits 1 when nothing matches |
| `clipstack alias REF NAME` | Name an entry `@NAME`, pinning it if needed |
| `clipstack alias --remove REF` | Remove an entry's alias; it stays pinned |
| `clipstack clear` | Clear clipboard history, pinned entries included, and delete any `legacy-backup-*` directories |
| `clipstack daemon` | Run the background daemon: clipboard capture and the `rcopy`/`rpaste` socket |
| `clipstack kill` | Stop the running daemon (it also stops its watcher and removes its socket) |
| `clipstack stats` | Show storage statistics |
| `clipstack status` | Check daemon, socket, capture mode and system health |
| `clipstack check [--repair]` | Check the database and search index; `--repair` rebuilds a damaged search index |
| `clipstack export [-o FILE] [--force]` | Write the whole history as JSON Lines, oldest first (`-o` creates FILE `0600`; `--force` overwrites) |
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

# Print the second-newest entry, or copy it back to the clipboard
clipstack get 2
clipstack get 2 --copy

# Find entries mentioning both words
clipstack search docker compose

# Name a snippet and use it
clipstack alias 1 sig
clipstack get @sig

# Pin, unpin, delete or edit an entry
clipstack pin 3
clipstack unpin @sig
clipstack delete id:42
clipstack edit 1

# Save the newest entry, a screenshot, as a file
clipstack get 1 > shot.png

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

### Scripting

`get`, `list --json`, `search --json`, `alias`, `pin`, `unpin`, `delete` and `edit` are meant for scripts. Their output and exit codes are stable.

**Entry references.** `get`, `alias`, `pin`, `unpin`, `delete` and `edit` take a `REF`:

| Form | Means |
|------|-------|
| `N` | Position in the history, newest first: `1` is the most recently used entry, as in `list` |
| `id:ID` | The entry's id, which never changes and is never reused (`list --json` shows it) |
| `@NAME` | The pinned entry named `NAME` with `clipstack alias` |

Positions shift as you copy; use `id:ID` or an alias to keep referring to the same entry. (Not `#42`: a shell treats a word starting with `#` as a comment.) Alias names are 1-32 characters from `a-z`, `0-9`, `_` and `-`, starting with a letter or digit. An alias pins its entry, so at most 25 entries can have one.

**JSON.** `list --json` and `search --json` print one JSON array on one line. Each element is an entry without its content (use `get` or `export` for that):

```json
[{"position":1,"id":42,"created_at":1761998400000,"last_used":1762001234567,"size":118,
  "preview":"git log --oneline | head","pinned":true,"alias":"gl","source":"watch",
  "mime":"text/plain;charset=utf-8","kind":"text","width":null,"height":null}]
```

| Field | Meaning |
|-------|---------|
| `position` | Position in the full history, newest first (`get N` prints this entry). With `--pinned` and in search results it is still the position in the whole history |
| `id` | Entry id (`get id:ID`) |
| `created_at`, `last_used` | Unix time in milliseconds: first saved, and last copied (the sort key) |
| `size` | Content size in bytes (for an image, of the PNG) |
| `preview` | First 100 characters, control characters replaced by spaces; `Image · WxH` for an image |
| `pinned`, `alias` | Pin state, and the alias or `null` |
| `source` | How it was saved: `watch`, `poll`, `copy`, `remote`, `picker`, `edit`, `import` or `unknown` |
| `mime` | The text type it was copied as; `image/png` for an image |
| `kind` | `text` or `image` |
| `width`, `height` | An image's size in pixels; `null` for text |

An empty history prints `[]` and exits 0; a search with no match prints `[]` and exits 1. `search` lists pinned entries first, then the newest.

**Exit codes:**

| Code | Meaning |
|------|---------|
| 0 | OK |
| 1 | Not found or no match (like grep): no entry for `REF`, `search` with no match (silent), `alias` with the name taken, `alias` or `pin` with 25 entries already pinned, `get --image-path` of a text entry, `edit` of an image; also any failure not listed below |
| 2 | Usage error: bad arguments, an invalid `REF` or alias name, an empty search, `get` of an image to a terminal, `serve` |
| 3 | Daemon or socket unavailable (`rcopy` and `rpaste`; no `clipstack` command needs the daemon) |
| 4 | Storage error: database locked past the 5 s busy timeout, damaged (`check`), or written by a newer ClipStack |
| 5 | Config file error |
| 6 | `wl-copy` or `wl-paste` failed or isn't installed |

**Examples:**

```bash
# Paste a named snippet into a command
git commit -m "$(clipstack get @msg)"

# Copy the newest entry that mentions "ssh-ed25519" back to the clipboard
id=$(clipstack search ssh-ed25519 --json -c 1 | jq -r '.[0].id') && clipstack get "id:$id" --copy

# Pinned entries and their aliases
clipstack list --pinned --json -c 25 | jq -r '.[] | "\(.alias // "-")\t\(.preview)"'

# Branch on whether something is in the history
if clipstack search "BEGIN CERTIFICATE" >/dev/null; then echo "a certificate is in the history"; fi
```

### Images

When the clipboard offers a PNG and no text (a screenshot tool, "Copy image" in a browser), the daemon saves the PNG as an image entry. When both are offered, the text is saved. Only `image/png` is kept; the size comes from the PNG header, which is checked (an offer that isn't a PNG is skipped). Images up to `max_image_bytes` (8 MiB by default, and never more than `max_total_bytes`) are saved; they count towards `max_entries` and `max_total_bytes` like text, can be pinned and named, and are skipped when a password manager marks the copy secret. They are not searchable.

- `clipstack get REF > file.png` writes the PNG. It refuses to write to a terminal (exit 2).
- `clipstack get REF --copy` (and Enter in the picker) puts it back on the clipboard as `image/png`.
- `clipstack get REF --image-path` writes the PNG to `$XDG_RUNTIME_DIR/clipstack-images/<id>.png` (directory `0700`, file `0600`, written to a temporary file and renamed) unless it is already there, and prints that path, for programs that show images from files. The files are removed with their entries (`delete`, `clear`, expiry, pruning; at the latest when the daemon next checks, once a minute), and the runtime directory is emptied at logout.
- The picker shows an image as `Image · WxH` and describes it in the preview pane; it doesn't draw it. `e` on an image says it can't be edited.
- `export` writes an image with `"kind":"image"`, `width`, `height` and the PNG as standard base64 in `image_base64` (its `content` is empty); `check` verifies that each image is a PNG of the recorded size.

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
┌─Search (/ to search)─────────────────────────────────────┐
│                                                          │
└──────────────────────────────────────────────────────────┘
┌─History (5/5) - 1 pinned─────┐┌─Preview - 1.2KB - 5m ago─┐
│> ★   5m [1.2KB] function     ││const example = () => {   │
│     12m [  45B] hello world  ││  return "hello";         │
│      2h [ 256B] {"type":"jso ││};                        │
│      3h [  12B] quick text   ││                          │
│     40d [3.1KB] import React ││                          │
└──────────────────────────────┘└──────────────────────────┘
[NORMAL] j/k:Nav  /:Search  Tab:Preview  Enter:Copy  e:Edit  p:Pin  P:Alias  dd:Del  u:Undo  q:Quit
```

The picker uses vim-style modal navigation. In normal mode every letter is a command, so typing a word there never filters (or deletes): press `/` first to search.

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
| `p` | Toggle pin on selected entry (★); unpinning removes its alias |
| `P` | Name the selected entry `@name` (pins it); see the alias prompt below |
| `e` | Edit the selected entry in `$VISUAL` / `$EDITOR` (else `vi`); saving a change copies the new text, adds it to the history and exits. Images can't be edited |
| `dd` | Delete selected entry (a single `d` only arms it) |
| `u` | Undo delete (5 second window) |
| `Tab` | Enter preview scroll mode |
| `Enter` | Copy selected entry to clipboard and exit (an image is copied as `image/png`) |
| `Esc` / `q` / `Ctrl+C` | Exit without copying |

**Search Mode** (after `/`):
| Key | Action |
|-----|--------|
| _type_ | Filter entries by fuzzy search (every letter is text here, including `d`, `p`, `q`) |
| `↑` / `↓` | Navigate while searching |
| `Ctrl+N` / `Ctrl+P` | Navigate (vim style) |
| `Backspace` | Delete character (exits search if empty) |
| `Enter` | Copy selected entry to clipboard and exit |
| `Esc` | Leave search mode; the filter stays (Backspace it away, or `/` to edit) |
| `Ctrl+C` | Exit without copying |

**Editing with `e`** (and `clipstack edit REF`, which works the same way from the shell). The picker hands the terminal to your editor with the entry in `$XDG_RUNTIME_DIR/clipstack-edit-<pid>-<id>.txt` (mode `0600`, never in `/tmp`), and deletes the file when the editor exits. `$VISUAL` or `$EDITOR` is run by `sh`, so `EDITOR="code --wait"` works. If you save a change, the new text becomes a new entry (the original stays) and is copied. If the editor exits non-zero, or the text is unchanged or blank, you are back in the picker with nothing copied. A final newline the editor adds to text that had none is dropped.

**Alias prompt** (after `P`; it replaces the search box):
| Key | Action |
|-----|--------|
| _type_ | The name: 1-32 characters from `a-z`, `0-9`, `_` and `-`, starting with a letter or digit (a leading `@` is ignored); it starts with the entry's current name |
| `Enter` | Save the name and pin the entry; with the name cleared, remove the alias. A refused name (invalid, taken, 25 entries already pinned) is reported in the status line and the prompt stays open |
| `Esc` | Cancel |
| `Ctrl+C` | Exit without copying |

**Preview Mode** (after pressing `Tab`):
| Key | Action |
|-----|--------|
| `j` / `k` / `↑` / `↓` | Scroll one line |
| `Page Up` / `Page Down` | Scroll one page |
| `g` / `Home` | Jump to top of content |
| `G` / `End` | Jump to bottom of content |
| `w` | Toggle visible whitespace (`·` space, `→` tab, `␍` CR, `¶` newline) |
| `n` | Toggle line numbers |
| `Tab` / `Esc` / `q` | Exit preview mode |
| `Ctrl+C` | Exit without copying |

**Visual Features:**
- Mode indicator shows `[NORMAL]`, `[SEARCH]`, or `[PREVIEW]` in status bar
- Pinned entries show `★` indicator and appear at the top of the list; named ones show `@name` in green
- Matched characters highlighted in **yellow** during fuzzy search
- `[content]` indicator for matches found in full content (not just preview)
- Scrollbar shows position in long lists
- A fixed-width age column (`  5s`, ` 12m`, `  3h`, ` 40d`, capped at `999d`), so the rows stay aligned
- Entry size displayed in human-readable format (e.g., "1.2KB")
- Status messages for actions (delete confirmation, undo countdown)
- The preview wraps to the pane's width (wide CJK characters count as two columns; tabs expand to stops of 4) and shows `[+N lines, Tab to scroll]` when the wrapped text is taller than the pane
- The whitespace and line-number toggles stay on for the rest of the session, in both the list's preview pane and preview mode

**Auto-Start:** Opening the picker automatically starts the background daemon if it isn't running. The daemon is the same executable as the picker (not whichever `clipstack` is first on `PATH`), uses the same `--storage-dir` and keeps the saved entry limit.

### Search Features

The picker fuzzy-matches your query against each entry's 100-character preview, then asks the database for entries whose full content contains it:

- Every word of the query must appear in the content (case-insensitive substring match, in any order)
- Words of 3 or more characters use an FTS5 trigram index over the first 256 K characters (262,144) of each entry; shorter words are matched with a substring scan of the same 256 K characters, with full Unicode case folding (`ü` finds `Ü`, `é` finds `É`)
- Up to 200 full-content matches are shown, after the preview matches, with a `[content]` indicator

There is no cap on how many entries are searched, and nothing is read from disk file by file.

### Pinned Entries

Pin frequently-used text to protect it from automatic pruning:

- Press `p` to toggle pin status on selected entry, or run `clipstack pin REF` / `clipstack unpin REF`
- Pinned entries show a `★` indicator
- Pinned entries always appear at the top of the list (most recently used first)
- Maximum 25 pinned entries allowed
- Pinned entries survive when the history limit is exceeded
- Press `P` to name the selected entry (pinning it); the list shows the name as `@name` before the preview text. Clear the name in the prompt to remove it (the entry stays pinned). From the shell: `clipstack alias REF NAME`, then `clipstack get @NAME` (see [Scripting](#scripting))
- Unpinning removes the name; the picker warns `Unpinned (alias @name removed)`
- Re-copying pinned content moves it to the top while preserving pin status

## Running the Daemon

The daemon saves every new clipboard selection to the history. Run one per login session; a second `clipstack daemon` exits with "Daemon already running". The picker starts one for you if none is running.

### Capture modes

By default (`[capture] mode = "auto"`) the daemon runs `wl-paste --watch`, which calls back into ClipStack once per new selection, so nothing is polled while the clipboard is idle. Watching needs a compositor with the wlroots data-control protocol (Hyprland, Sway and other wlroots compositors, KDE Plasma). Where it is missing (GNOME), `wl-paste --watch` exits at once and the daemon falls back to reading the clipboard every `poll_interval_ms` (250 ms by default), logging why:

```
[capture] polling every 250 ms: wl-paste --watch exited at once (Watch mode requires a compositor that supports the wlroots data-control protocol)
```

If the watcher dies later it is restarted after 1, 2, 4 ... up to 30 seconds; after 5 restarts within a minute the daemon falls back to polling. `mode = "watch"` exits with an error instead of polling, and `mode = "poll"` never watches. `clipstack status` shows which mode is in use (`capture=watch` or `capture=poll`).

Either way, copies a password manager marks as secret (the `x-kde-passwordManagerHint` type that KeePassXC and `wl-copy --sensitive` set) are never saved, nor is anything your `ignore_patterns` or the built-in secret patterns match. The daemon logs each capture by id and size only, never the copied text:

```
[watch] saved id:42 (118 bytes)
[watch] saved image id:43 (48211 bytes)
[watch] skipped sensitive
[watch] skipped: matches ignore_patterns[0]
```

It also applies retention and the size caps once a minute, so old entries are deleted even when nothing new is copied.

### The daemon socket

The daemon listens on `$XDG_RUNTIME_DIR/clipstack.sock` (usually `/run/user/<uid>/clipstack.sock`), next to its lock file `clipstack.lock`. Without `XDG_RUNTIME_DIR` both go in `/tmp/clipstack-<uid>/`, which ClipStack creates `0700` and refuses to use if another user owns it or it is accessible to others. The socket is `0600`, so only your user can connect. It serves [remote copy and paste](#remote-clipboard-ssh) (`rcopy`, `rpaste`) and `clipstack status`. Set `[socket] enabled = false` to turn it off.

A socket left behind by a daemon that crashed is replaced at the next start. A path that isn't a socket, or a socket another process is listening on, is left alone: the daemon then runs without a socket and logs `[socket] disabled: ...`.

### Manual Start

```bash
# Run in foreground (for testing)
clipstack daemon

# Run in background
nohup clipstack daemon > /tmp/clipstack.log 2>&1 &

# Stop the running daemon
clipstack kill
```

The daemon stops cleanly on SIGINT, SIGTERM and SIGHUP: Ctrl+C, `clipstack kill`, `kill <pid>` and `systemctl --user stop` all make it stop its `wl-paste --watch` child, remove its socket, print `clipstack daemon stopped` and exit with status 0. If the daemon is killed outright (SIGKILL), the kernel stops the watcher too (`PR_SET_PDEATHSIG`), and the next start replaces the stale socket.

### Systemd User Service

The unit in `systemd/clipstack.service` runs `~/.cargo/bin/clipstack daemon` with the graphical session:

```bash
# The store must exist: the sandbox only allows writing to it if it does
install -d -m 0700 ~/.local/share/clipd

# Install the service file
install -D -m 0644 systemd/clipstack.service ~/.config/systemd/user/clipstack.service
systemctl --user daemon-reload

# Enable and start
systemctl --user enable --now clipstack.service

# Check status
systemctl --user status clipstack.service
clipstack status

# View logs
journalctl --user -u clipstack.service -f
```

The daemon needs `WAYLAND_DISPLAY`, which the systemd user manager doesn't have unless the compositor exports it. In Hyprland, add to `~/.config/hypr/autostart.conf`:

```bash
exec-once = dbus-update-activation-environment --systemd WAYLAND_DISPLAY
```

(Sway: `exec dbus-update-activation-environment --systemd WAYLAND_DISPLAY`. Many distributions' session scripts already do this.)

The unit is sandboxed: the file system is read-only except `~/.local/share/clipd` and the runtime dir (`%t`, for the lock and the socket), `/tmp` is private, only Unix sockets are allowed, and files are created owner-only (`UMask=0077`). If you use `--storage-dir` or a different binary path, edit `ExecStart` and `ReadWritePaths` to match.

Some of these settings (`ProtectSystem`, `ProtectHome`, `PrivateTmp`, `MemoryDenyWriteExecute`) need unprivileged user namespaces in a user unit. Arch and most current distributions allow them. If `systemctl --user status clipstack.service` shows the service failing with a namespace error (`status=226/NAMESPACE`), delete those four lines from your copy of the unit, then run `systemctl --user daemon-reload` and `systemctl --user restart clipstack.service`, and check `clipstack status`.

**Upgrading from the old unit:** earlier versions shipped `systemd/clipd.service`, which couldn't create the lock file in the runtime dir. Copying the new unit over `~/.config/systemd/user/clipstack.service` with the commands above replaces it in place; run `systemctl --user daemon-reload` and `systemctl --user restart clipstack.service`. Stop a daemon of the old version first (`clipstack kill`): the new one refuses to start while the old one holds the lock, and `status` shows such a daemon as `capture=unknown`, because it has no socket.

### Hyprland Autostart

Without systemd, add to `~/.config/hypr/autostart.conf`:

```bash
exec-once = clipstack daemon
```

To bind the picker to a hotkey, add to `~/.config/hypr/bindings.conf`:

```bash
bind = $mainMod CTRL, B, exec, [float;size 800 600;center] alacritty --class clipstack-picker -e clipstack pick
# Or pick and paste straight into the window you were in (needs wtype)
bind = $mainMod CTRL, V, exec, [float;size 800 600;center] alacritty --class clipstack-picker -e clipstack pick --paste
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
# Or pick and paste straight into the window you were in (needs wtype)
bindsym $mod+Ctrl+v exec alacritty --class clipstack-picker -e clipstack pick --paste
for_window [app_id="clipstack-picker"] floating enable, resize set 800 600
```

### Paste after picking

`clipstack pick --paste` (or `paste_after_pick = true` under `[picker]` in the config file) copies the entry you choose, closes the picker, and 150 ms later types `paste_keys` with wtype, by default `shift+Insert`, which pastes the clipboard in terminals and most GUI apps. The delay lets the picker's window close and focus go back to the window you came from. If that window pastes something else on Shift+Insert (xterm pastes the primary selection), set `paste_keys = "ctrl+shift+v"` for terminals or `"ctrl+v"` for GUI apps. `paste_keys` is modifiers (`shift`, `ctrl`, `alt`, `altgr`, `super`) and one key (`Insert`, `Return`, or a letter or digit), joined with `+`. Without wtype the entry is still copied, and ClipStack says it didn't paste. Cancelling the picker pastes nothing.

## Omarchy Overlay

On [Omarchy](https://omarchy.org), ClipStack also comes as an omarchy-shell overlay plugin (`omarchy/clipstack/`): a Maccy-style panel with the history on the left and the full entry on the right. Open it, type to filter, press Enter, and the entry is pasted into the window you were in. Pinned entries come first, then Today, Yesterday and Earlier; while you search, one Matches group with the query marked in each row. Text previews show line numbers (the first 2000 lines); images show a thumbnail in the list and the whole image fitted to the preview.

The overlay never reads the clipboard or the history itself: every list, search, preview and action runs the `clipstack` CLI (`list --json`, `search --json`, `get`, `get --image-path`, `get --paste`, `get --copy`, `pin`, `unpin`, `alias`, `delete`, `edit`), so the daemon stays the only thing that captures copies. If a command fails, its error shows in the panel's footer. The terminal picker (`clipstack pick`) stays for other desktops and SSH sessions.

Install it from a checkout (it links the folder, so `git pull` updates the plugin):

```bash
scripts/install-omarchy-plugin.sh              # link to ~/.config/omarchy/plugins/solsystemlabs.clipstack and enable
scripts/install-omarchy-plugin.sh --uninstall  # disable and remove the link
```

The script refuses to replace anything at that path that isn't a symlink. To open the overlay from a key, bind `omarchy-shell shell toggle solsystemlabs.clipstack '{}'`.

| Key | Action |
|-----|--------|
| Type | Filter (the search field has focus when the overlay opens) |
| `↑` / `↓`, `Ctrl+K` / `Ctrl+J` | Move selection |
| `Page Up` / `Page Down`, `Home` / `End` | Jump a page, to the first or last entry |
| `Enter` | Paste into the previous window (the overlay closes first) |
| `Shift+Enter` | Copy only, and close |
| `Ctrl+P` | Pin or unpin (unpinning removes the alias) |
| `Ctrl+A` | Name the entry: an inline prompt checks the alias as you type (`a-z`, `0-9`, `_`, `-`, up to 32, starting with a letter or digit) and says when another entry has it; `Enter` saves, `Esc` cancels |
| `Ctrl+E` | Edit a text entry in your editor (`omarchy-launch-tui ... clipstack edit`; the overlay closes) |
| `Ctrl+Backspace` | Delete the entry |
| `Esc` | Clear the search; a second `Esc` closes |

Every action is also a button showing its key. Click selects a row, double-click pastes it.

## Remote Clipboard (SSH)

Two small POSIX `sh` scripts work from a remote shell through ssh:

- `rcopy` copies its stdin to your local clipboard. The text is also saved to the history, subject to the same ignore rules as any other copy.
- `rpaste` prints your local clipboard. It is off until you set `allow_pull = true` locally (see [Security](#security)).

ssh forwards the daemon's Unix socket to the remote host, and the scripts talk to the daemon over it. There is no TCP port.

### Setup

**On your local machine**, run the daemon (see above) and add to `~/.ssh/config`:

```
Host myserver
    HostName server.example.com
    User myuser
    RemoteForward /home/myuser/.clipstack.sock /run/user/1000/clipstack.sock
```

The first path is on the remote host (where the scripts look by default); it must be absolute, so write out the remote home dir. The second is your local socket (`clipstack status` prints it; `1000` is your local uid, from `id -u`).

To use `rpaste`, also allow pulls in your local `~/.config/clipstack/config.toml` and restart the daemon (`clipstack kill`, then start it again):

```toml
[socket]
allow_pull = true
```

**On the remote server**, let sshd replace a socket left by an earlier connection, in `/etc/ssh/sshd_config` (then reload sshd). Without it, every login after the first finds the old socket in the way and the forward fails:

```
StreamLocalBindUnlink yes
```

and install the scripts (from this repo's `scripts/`). They need `socat`, or `nc` with Unix socket support (`-U`, OpenBSD netcat), or `ncat`. BusyBox `nc` has no `-U`, so install `socat` there:

```bash
install -m 0755 scripts/rcopy scripts/rpaste ~/.local/bin/   # or /usr/local/bin

# Usage examples:
echo "text from remote" | rcopy
head -50 /var/log/syslog | rcopy
rcopy < /etc/hostname

rpaste                      # print the local clipboard
rpaste > notes.txt          # exactly as copied, no newline added
rpaste | sh                 # run what you copied locally (think first)
```

Both scripts read `CLIPSTACK_SOCK` for the socket path (default `$HOME/.clipstack.sock`), so a forward to a different remote path works with `export CLIPSTACK_SOCK=/path/to/socket`.

| Exit | `rcopy` | `rpaste` |
|------|---------|----------|
| 0 | Copied. If the history didn't keep it, stderr says why (`rcopy: copied, but not saved to history (ignored)`) | Printed the clipboard (nothing for an empty clipboard) |
| 1 | The daemon refused it (for example `not_utf8`) | The daemon refused: `forbidden pull disabled (set socket.allow_pull = true)`, or `sensitive` for a password manager's copy |
| 3 | No socket, no daemon behind it, or no `socat`/`nc`/`ncat` | Same |

**Migrating from `clipstack serve`:** the TCP server (port 7779, `CB_PORT`) is gone, and `clipstack serve` now only prints where the socket is and exits 2. Replace `RemoteForward 7779 localhost:7779` with the socket line above, and install the new `rcopy` on each remote host.

### How It Works

1. The daemon listens on `$XDG_RUNTIME_DIR/clipstack.sock` (`0600`, in a `0700` dir)
2. `RemoteForward` makes sshd on the remote host create `~/.clipstack.sock` and forward connections to it through your ssh connection to the local socket
3. `rcopy` sends `CLIPSTACK/1 PUSH <length>`, a newline and its input. The daemon saves the text (unless the ignore rules skip it), copies it with `wl-copy`, and replies `OK 0 stored=1 id=<id>`, `OK 0 stored=0 reason=<why>` or `ERR <code> <message>`
4. `rpaste` sends `CLIPSTACK/1 PULL 0`. The daemon reads the clipboard with `wl-paste` and replies `OK <length>`, a newline and the text, or `ERR forbidden ...` / `ERR sensitive ...`. A pull is never saved to the history

Each connection carries one request. The header is one line of at most 256 bytes; the body is exactly `<length>` bytes, at most `max_entry_bytes` for a push. `PING` reports the version, capture mode and whether pulls are allowed (`OK 0 version=0.1.0 capture=watch pull=0`). Requests are length-prefixed, so the server knows where one ends and clients never need to half-close (`nc -U` works without `-N`).

### Security

- Only your user can connect to the local socket (`0600` in a `0700` runtime dir). There is no TCP port.
- On the remote host, the forwarded socket is created by sshd as your remote user, `0600` by default (`StreamLocalBindMask`). Anyone who is root there can still use it to put text on your clipboard, so only forward it to hosts you trust.
- **`allow_pull = true` lets anything on the remote host that can reach the forwarded socket read your local clipboard** while you are connected: your remote user's processes and the remote root. That is why it is off by default. Copies a password manager marks as secret are refused even when it is on, but anything else you copy (a password typed into a terminal, a token from a web page) can be read. Turn it on only for hosts you trust as much as your local machine, or forward the socket only in the sessions where you need it (`ssh -o RemoteForward=... myserver`).
- A client gets `timeout_ms` (5 s) per read and twice that for the whole request, so a stuck connection can't block the socket for long. Oversized pushes are refused from the header alone; non-UTF-8 text is refused.

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

Deleting an entry (or `clear`, expiry, or pruning when the history passes its entry or size limit) overwrites its text inside the database (SQLite `secure_delete` and the FTS5 `secure-delete` option) and then truncates the WAL, so the deleted text doesn't linger in the files. If another process (an open picker) is reading at that moment, the truncation is recorded as pending and the daemon retries it every 60 seconds (as does the next delete) until it succeeds.

### Upgrading from the JSON store

Versions before the SQLite store kept `index.json` plus one `<id>.txt` file per entry. **Run `clipstack kill` before the first command of the new version**: an old daemon left running keeps writing `index.json`, and every later command would import those writes again and leave another backup directory.

The first command of the new version imports the old store automatically:

- It takes the old `index.lock`, so a still-running old daemon can't change the files mid-import, and imports every entry with its timestamp and pinned flag in one transaction. Identical contents are merged. The old version had no pin limit; if more than 25 entries were pinned, the 25 most recently used keep their pin and the rest are unpinned (the notice says how many). Each entry goes through the same filter as a new copy: empty entries, entries over `max_entry_bytes`, and entries matching your `ignore_patterns` or the built-in secret patterns are not imported (their files still go to the backup, below). Of the limits, only the entry limit applies during the import (the old version enforced it too); retention and the total-size cap apply from the next start.
- Only after the transaction commits are `index.json`, the `.txt` files and any `.tmp` leftovers moved into `legacy-backup-<ms>/` (`0700`). `index.lock` stays where it is. Nothing is deleted.
- It prints `Imported N entries from index.json; old files kept in .../legacy-backup-<ms>/ (delete after checking)` once, where N is the number of entries now in your history. If some entries were left out (over the entry limit, rejected by the filter, or not UTF-8 text), it says how many of each instead of "delete after checking", and that the backup is the only copy of those entries. If retention is on, it also says how many imported entries will expire at the next start.
- `status` and `check` keep listing the backup until you delete it. The backup holds your old history as plain text files, so delete it once `clipstack list` looks right. `clipstack clear` deletes it too (it prints `Removed N legacy backup dirs`): only the regular files inside each backup directory and then the directory are removed, and symlinks are never followed, so a backup directory holding anything else is emptied of its files, kept, and named in the output.

**Downgrading:** an older version doesn't read `clipstack.db`. It starts with an empty history and writes a new `index.json`; the next command of this version imports that as well. A database written by a newer version than the one running is refused with an "upgrade clipstack" error.

### Storage Limits

| Limit | Default | Notes |
|-------|---------|-------|
| Max entries | 100 | Unpinned entries; 1-10,000 via `--max-entries` or `CLIPSTACK_MAX_ENTRIES`. Oldest unpinned entries are deleted first. The limit is saved in the database |
| Max pinned | 25 | Pinned entries are exempt from every limit below |
| Retention | off | `retention_days` in the config file: unpinned entries last used longer ago are deleted at open, on each copy and once a minute in the daemon |
| Max entry size | 10 MiB | `max_entry_bytes` (up to 64 MiB). Larger copies still reach the clipboard but aren't saved |
| Max image size | 8 MiB | `max_image_bytes` (up to 64 MiB, and never more than `max_total_bytes`): larger PNG images aren't saved |
| Total size | 256 MiB | `max_total_bytes`: unpinned content above this total is deleted, oldest first |
| Search window | 256 K characters | Full-content search covers the first 262,144 characters of each entry |
| Preview | 100 characters | Shown in lists |

### Data Safety

- **Private permissions**: storage dir `0700`, database files `0600`; existing stores are tightened on open
- **Transactions**: every change (save, pin, delete, prune) is one SQLite transaction in WAL mode, so a crash or power loss leaves the history as it was before or after the change, never in between
- **Concurrent processes**: the daemon, its watch hook, the picker and the CLI each open the database; writers wait up to 5 seconds for each other and readers never block writers, so concurrent changes are never lost
- **Integrity check**: `clipstack check` runs SQLite's integrity check and the search index's consistency check, and checks that every image entry holds a PNG of its recorded size; `clipstack check --repair` rebuilds a damaged search index from the entries
- **Duplicate detection**: identical content is deduplicated by SHA256 hash
- **Secret filtering**: copies that match the built-in secret patterns or your `ignore_patterns` are never written to the database

### Custom Storage Location

```bash
clipstack --storage-dir /path/to/custom/dir daemon
clipstack --storage-dir /path/to/custom/dir list
```

The directory is treated as ClipStack's own: it is created if missing and chmod'ed to `0700`, and its files to `0600`, even if it already existed. Don't point `--storage-dir` at a shared directory.

### Inspecting Storage

`clipstack export` writes one JSON object per entry, oldest first, with the fields `id`, `created_at`, `last_used` (Unix ms), `size`, `pinned`, `alias`, `source`, `mime`, `kind` and `content`; image entries also have `width`, `height` and `image_base64` (see [Images](#images)):

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

# PNG images larger than this aren't saved (1 byte to 64 MiB; max_total_bytes caps it too).
max_image_bytes = 8388608    # 8 MiB

# Unpinned content above this total is deleted, oldest first (at least max_entry_bytes).
max_total_bytes = 268435456  # 256 MiB

# Copies matching any of these regexes (Rust regex syntax, up to 64) aren't saved.
# A pattern matches anywhere in the content unless anchored with ^/$ or \A/\z.
ignore_patterns = []

# Also skip private keys anywhere in a copy, and copies that are nothing but a GitHub,
# AWS access key, Slack, Stripe or sk- style API token (full list below).
builtin_secret_patterns = true

[capture]
# auto: wl-paste --watch, falling back to polling where the compositor can't watch;
# watch: never poll (exit with an error instead); poll: never watch.
mode = "auto"

# How often polling reads the clipboard (50 to 5000).
poll_interval_ms = 250

[socket]
# Serve $XDG_RUNTIME_DIR/clipstack.sock for rcopy, rpaste and `clipstack status`.
enabled = true

# Let socket clients read the local clipboard (PULL); rpaste needs this. Off by default:
# anyone who can reach a forwarded socket could read your clipboard.
allow_pull = false

# Per-read timeout for a socket client (100 to 60000); a whole request gets twice this.
timeout_ms = 5000

[picker]
# The picker searches once you stop typing for this long (0 to 1000; 0 = on every key).
search_debounce_ms = 50

# Paste into the previous window after picking, as with `clipstack pick --paste` (needs wtype).
paste_after_pick = false

# Keys wtype types to paste: modifiers (shift, ctrl, alt, altgr, super) and Insert, Return
# or one letter or digit, joined with +. Use "ctrl+shift+v" where Shift+Insert pastes the
# primary selection (xterm).
paste_keys = "shift+Insert"
```

**Built-in secret patterns** (Rust regex syntax). The private-key header is skipped wherever it appears in a copy. Every other pattern must match the whole copy, apart from surrounding whitespace: a copy that is nothing but a token is skipped, while prose or a script that contains one (for example a shell script with `export GITHUB_TOKEN=ghp_...` in it) is still saved. Use `ignore_patterns` if you want those skipped too.

| Secret | Pattern | Matches |
|--------|---------|---------|
| Private key (PEM, OpenSSH, PGP) | `-----BEGIN [A-Z0-9 ]*PRIVATE KEY( BLOCK)?-----` | anywhere |
| GitHub token | `\A\s*gh[pousr]_[A-Za-z0-9]{36,255}\s*\z` | whole copy |
| GitHub fine-grained token | `\A\s*github_pat_[A-Za-z0-9_]{80,}\s*\z` | whole copy |
| AWS access key ID (long-term and temporary) | `\A\s*(AKIA\|ASIA)[0-9A-Z]{16}\s*\z` | whole copy |
| Slack token | `\A\s*xox[abprs]-[A-Za-z0-9-]{10,}\s*\z` | whole copy |
| `sk-` style API key (OpenAI and others) | `\A\s*sk-[A-Za-z0-9_-]{20,}\s*\z` | whole copy |
| Stripe secret key | `\A\s*sk_(live\|test)_[A-Za-z0-9]{20,}\s*\z` | whole copy |

When a `clipstack copy` isn't saved because of these settings, the text is still copied to the clipboard and ClipStack says why (`Copied N bytes (not saved to history: matches ignore_patterns[0])`); the reason never includes the copied text. The entry limit is not in this file: it comes from `--max-entries`, `CLIPSTACK_MAX_ENTRIES` or the saved value.

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `CLIPSTACK_MAX_ENTRIES` | Maximum entries to store (1-10000); overridden by `--max-entries` | saved limit, else `100` |
| `CLIPSTACK_SOCK` | Socket `rcopy` and `rpaste` connect to (on the remote host) | `$HOME/.clipstack.sock` |

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
Daemon:  running (pid 4242)  (or: not running)
Socket:  /run/user/1000/clipstack.sock (reachable, capture=watch, pull=off)

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
- **Capture mode**: `capture=poll` means the compositor can't watch the clipboard (see [Capture modes](#capture-modes)); `not listening` means no daemon serves the socket; `not reachable; capture=unknown` means a daemon holds the lock but doesn't answer (an older version, or `[socket] enabled = false`)
- **Wayland not detected**: ClipStack requires a Wayland session
- **Storage issues**: Shows where data is stored and current usage, and lists any `legacy-backup-*` directory left by an upgrade
- **Unexpected limit**: Shows where the entry limit comes from: `cli` (`--max-entries`), `env` (`CLIPSTACK_MAX_ENTRIES`), `saved` (in the database), or `default`
- **Unexpected skips**: Shows which config file is in effect, the retention, the size caps and how many ignore patterns apply

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        ClipStack                                │
├─────────────────────────────────────────────────────────────────┤
│  CLI (main.rs → cli.rs: clap, REF, exit codes; commands.rs)     │
│  ├── copy/paste    → clipboard.rs (wl-copy/wl-paste wrapper)   │
│  ├── get/search    → storage/ (by position, id:ID, @alias; FTS)│
│  ├── alias         → Name (and pin) an entry                   │
│  ├── pin/unpin     → Pin state; delete or edit an entry        │
│  ├── get images    → PNG to stdout, --copy, --image-path files │
│  ├── pick          → picker/ (TUI with ratatui); --paste: wtype│
│  ├── daemon        → daemon/ (capture, socket, PID lock file)  │
│  ├── __watch-hook  → Run by wl-paste --watch per selection     │
│  ├── kill          → Signal the daemon PID from its lock file  │
│  ├── check/export  → Integrity check, JSONL export             │
│  ├── status        → Health check (daemon, socket, storage)    │
│  └── list/stats    → storage/ (SQLite queries)                 │
├─────────────────────────────────────────────────────────────────┤
│  Daemon (daemon/)                                               │
│  ├── mod.rs        → Main loop, runtime dir, lock, expiry      │
│  ├── capture.rs    → wl-paste --watch supervisor, poll fallback│
│  └── socket.rs     → clipstack.sock: PUSH/PULL/PING (0600)     │
│  protocol.rs       → CLIPSTACK/1 framing (rcopy, rpaste)       │
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
│  Picker (picker/)                                               │
│  ├── keys.rs       → map_key: one pure key → action table      │
│  ├── search.rs     → Fuzzy preview + FTS5 content, debounced   │
│  ├── state.rs      → Mode, focus, selection by id, prompt      │
│  ├── text.rs       → Wrap by display width, whitespace, ages   │
│  ├── render.rs     → List, preview, status line                │
│  └── actions.rs    → Edit in $EDITOR, wtype paste keys         │
├─────────────────────────────────────────────────────────────────┤
│  External                                                       │
│  ├── wl-copy       → Write to Wayland clipboard (text, PNG)    │
│  ├── wl-paste      → Read from Wayland clipboard (text, PNG)   │
│  └── wtype         → Optional: paste keys for pick/get --paste │
└─────────────────────────────────────────────────────────────────┘
```

### Key Design Decisions

1. **Metadata-First Reads**: Lists read only metadata and the 100-char preview; the content column is stored last, so listing never touches a large entry's text. Full content is loaded on demand.
2. **Indexed Search**: An FTS5 trigram index (external content, first 256 K characters per entry) answers full-content queries without scanning entries one by one.
3. **SQLite Transactions**: Every change is one transaction in WAL mode. Crashes never leave the history half-written.
4. **Events, Polling as Fallback**: `wl-paste --watch` runs a hidden `clipstack __watch-hook` per new selection, which saves straight to SQLite. Compositors without the data-control protocol fall back to 250 ms polling; a supervisor restarts a dying watcher with backoff.
5. **SHA256 Deduplication**: Hashes content to prevent duplicates; re-copying moves the existing entry to the top of the list and refreshes its timestamp.
6. **One Database File**: SQLite (compiled in) instead of a JSON index plus one file per entry. `clipstack export` and `sqlite3 -readonly` keep the data inspectable.
7. **Locking**: A daemon lock in the runtime dir (which also records the daemon PID) allows only one daemon at a time. SQLite's own locking serializes writes from every process.
8. **Unix Socket, Not TCP**: Remote copy goes through a `0600` socket in the runtime dir, forwarded by ssh, with a small length-prefixed protocol; there is no network port.
9. **Modal UI Pattern**: Vim-style normal/search/preview modes keep navigation keyboard-only and predictable.
10. **Pin Protection**: Pinned entries exempt from automatic pruning, with configurable limits to prevent abuse.

### Design Philosophy

ClipStack is built around several core principles:

**Simplicity Over Features**
- Single binary with zero runtime dependencies (besides wl-clipboard; wtype only for `pick --paste`)
- One SQLite file, inspectable with `clipstack export | jq` or `sqlite3 -readonly`
- No database server, no background services (except the daemon), an optional config file

**Performance by Design**
- Listing 100 entries: < 10ms, whatever their size
- Content load time: < 20ms even for 500KB entries
- Event-driven capture: nothing runs while the clipboard is idle (250 ms polling only where watching isn't supported)
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
- Letters are commands (`dd` delete, `u` undo, `p` pin, `P` name, `e` edit), so typing in normal mode never filters or deletes by accident; `/` starts a search
- `Tab` switches between list and preview focus

**Unix Philosophy**
- Does one thing well: manages clipboard history
- Composable with pipes: `cat file | clipstack copy`, `clipstack paste | grep pattern`
- Works with ssh socket forwarding for remote copy
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
- Binary content: PNG images are saved as images (see [Images](#images)); other binary offers are not saved
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
command -v wl-copy wl-paste

# Check you're in a Wayland session
echo $WAYLAND_DISPLAY
# Should output something like "wayland-0" or "wayland-1"
```

### Daemon not saving entries

```bash
# Check daemon is running
clipstack status

# Run in foreground to see output (stop a running daemon first: clipstack kill)
clipstack daemon
# Should print "[watch] saved id:N (B bytes)" (or "[poll] ...") on copy
```

Under systemd, check that the user manager has `WAYLAND_DISPLAY` (`systemctl --user show-environment | grep WAYLAND`); see [Systemd User Service](#systemd-user-service).

### Daemon falls back to polling

`clipstack status` shows `capture=poll`, and the log says `[capture] polling every 250 ms: wl-paste --watch exited at once (...)`. The reason is wl-paste's own error: usually the compositor doesn't support the data-control protocol (GNOME), which polling works around. Polling still saves every copy that stays on the clipboard for at least `poll_interval_ms`. If your compositor should support watching, run `wl-paste --watch cat` in a terminal to see the error, and check that wl-clipboard is 2.0 or newer (`wl-paste --version`).

### Socket unavailable

`clipstack status` shows `Socket: ... (not reachable; capture=unknown)` while the daemon runs. The daemon keeps capturing without a socket; its log says why:

- `[socket] disabled: ... exists and is not a socket` or `... is in use by another process`: something else has the path. The daemon never deletes it; move it away and restart the daemon.
- `[socket] disabled by config`: `[socket] enabled = false` in the config file.
- No `[socket]` line at all: an older daemon without a socket holds the lock. Run `clipstack kill` and start the daemon again.

`not listening` means no daemon is running: start one.

### Corrupted storage

```bash
# Check the database and the search index
clipstack check

# Search misses entries it should find: rebuild the search index
clipstack check --repair
```

If `check` reports database damage, it prints what to do: save what can still be read with `clipstack export -o backup.jsonl`, then rebuild the database with the `sqlite3` command it shows.

### Remote copy or paste not working

```bash
# On local: the daemon serves its socket
clipstack status          # Socket: /run/user/1000/clipstack.sock (reachable, ...)

# On remote: ssh created the forwarded socket
ls -l ~/.clipstack.sock   # srw------- ...

# On remote: ask the daemon directly
printf 'CLIPSTACK/1 PING 0\n' | socat - UNIX-CONNECT:$HOME/.clipstack.sock
# OK 0 version=0.1.0 capture=watch pull=0
```

`rpaste` exiting 1 with `forbidden` means the local daemon has `allow_pull = false` (the default): set it to `true` in the local config and restart the daemon (`PING` then shows `pull=1`).

`rcopy` or `rpaste` exiting 3 means it found no socket, no daemon behind it, or no `socat`/`nc -U`/`ncat`. A missing `~/.clipstack.sock` usually means the `RemoteForward` line is wrong, or a stale socket from an earlier login is in the way: set `StreamLocalBindUnlink yes` in the remote `sshd_config`, or delete the stale file. If ssh prints `remote port forwarding failed for listen path`, that is the same stale socket.

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

# Shell suites (need a release build and jq). e2e_daemon.sh also uses socat,
# nc -U and systemd-analyze, and e2e_remote.sh (rcopy/rpaste) socat and nc -U,
# when installed, and skip those checks otherwise; e2e_remote.sh needs one of the two.
cargo build --release
for t in tests/e2e_*.sh tests/integration/*.sh; do bash "$t" || break; done

# Omarchy overlay: Logic.js node tests, qmllint, omarchy plugin validate;
# --render also draws its four states headless (needs quickshell) into a temp dir
scripts/test-overlay.sh --render
```

The overlay checks run against `omarchy/clipstack/tests/fake-clipstack`, a stand-in CLI with canned entries (set `CLIPSTACK_BIN` to point the overlay at it), so they never touch the real history or the running shell.

Tests that run the `clipstack` binary are isolated from your real clipboard, history and daemon. Rust tests use `TestEnv` in `tests/common/mod.rs`; every shell suite sources `tests/lib.sh` and calls `isolate_env`. The shell suites build old-format stores and check the import through `clipstack export | jq`. `TestEnv` and `isolate_env` point `XDG_RUNTIME_DIR`, `XDG_DATA_HOME`, `XDG_CONFIG_HOME` and `HOME` at a temp directory, put the fake `wl-paste`/`wl-copy`/`wtype` in `tests/fakes/` first on `PATH`, unset `WAYLAND_DISPLAY`, and only ever kill processes they started themselves.

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
│   ├── main.rs          # Entry point: parse, open storage, dispatch, exit code
│   ├── cli.rs           # clap definitions, entry references (N, id:ID, @alias), exit codes
│   ├── commands.rs      # One function per subcommand
│   ├── clipboard.rs     # Wayland clipboard operations
│   ├── config.rs        # config.toml loading and validation
│   ├── daemon/          # Background daemon
│   │   ├── mod.rs       # Main loop, runtime dir, lock file, periodic expiry
│   │   ├── capture.rs   # wl-paste --watch supervisor, watch hook, polling fallback
│   │   └── socket.rs    # Unix socket server (PUSH/PULL/PING)
│   ├── image_cache.rs   # get --image-path files in $XDG_RUNTIME_DIR/clipstack-images
│   ├── perms.rs         # Private (0700/0600) storage permissions
│   ├── png.rs           # PNG signature and IHDR check (width, height)
│   ├── picker/          # TUI history picker
│   │   ├── mod.rs       # Picker: actions, event loop, terminal setup and suspend
│   │   ├── actions.rs   # Edit in $EDITOR, wtype paste keys
│   │   ├── data.rs      # Entries, content cache, undo
│   │   ├── state.rs     # View state: mode, focus, selection (by id), prompt, toggles
│   │   ├── search.rs    # Fuzzy preview match + full-content search, debounce
│   │   ├── keys.rs      # map_key: key -> action (pure)
│   │   ├── render.rs    # Drawing
│   │   └── text.rs      # Wrapping by display width, whitespace, line numbers, ages
│   ├── protocol.rs      # Socket protocol v1 framing
│   ├── storage/         # SQLite storage
│   │   ├── mod.rs       # Storage: open, pin/alias/delete/clear, check
│   │   ├── schema.rs    # Schema, PRAGMAs, migrations
│   │   ├── ingest.rs    # Saving, skip rules, limits and expiry
│   │   ├── query.rs     # list, search, counts, export
│   │   └── legacy.rs    # One-time import of the old JSON store
│   └── util.rs          # Formatting utilities (size, time)
├── tests/
│   ├── common/mod.rs    # TestEnv isolation harness for binary tests
│   ├── daemon_lifecycle.rs  # Daemon start/stop, shutdown, status and serve stub tests
│   ├── migration_cli.rs     # Import, config, export and check CLI tests
│   ├── cli_scripting.rs     # get, list/search --json, alias and exit code tests
│   ├── cli_entries.rs       # pin/unpin/delete/edit, get --paste and image get tests
│   ├── watch_capture.rs     # Watch hook, watch capture and polling fallback tests
│   ├── socket_protocol.rs   # Socket PUSH/PULL/PING, timeouts and stale-socket tests
│   ├── fakes/           # Fake wl-paste/wl-copy/wtype (never touch the real clipboard or type keys)
│   ├── lib.sh           # isolate_env/cleanup for shell suites
│   ├── e2e_*.sh         # Shell end-to-end suites (e2e_daemon.sh: capture, socket, unit; e2e_remote.sh: rcopy, rpaste)
│   └── integration/     # Shell integration suites
├── omarchy/clipstack/    # omarchy-shell overlay plugin (QML; Logic.js holds the pure logic)
│   └── tests/           # node tests for Logic.js, fake-clipstack, headless render harness
├── scripts/
│   ├── rcopy            # Remote copy over the forwarded daemon socket
│   ├── rpaste           # Remote paste (needs allow_pull = true)
│   ├── install-omarchy-plugin.sh  # Link and enable the overlay plugin (--uninstall)
│   └── test-overlay.sh  # Overlay checks: node tests, qmllint, plugin validate, --render
└── systemd/
    └── clipstack.service # Systemd user service file
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

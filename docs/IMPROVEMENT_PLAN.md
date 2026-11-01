# ClipStack Improvement Plan

> Status: **DRAFT (skeleton)**. Each step below is filled in with detailed
> design, file-level changes and test lists in later revisions.
>
> This plan runs **before** `docs/FEATURE_PLAN.md`. It replaces FEATURE_PLAN
> §0.0 (storage correctness contract) and most of §0.2–0.3 (async loading and
> content cache), and makes Feature 1's deep-search design simpler (FTS5
> replaces lazy file scanning).

## Goals

Ranked by: security, then functionality, then performance, then everything else.
The implementation order below is grouped by module instead, so that each area
is changed once and the foundations are built with later needs in mind.

| # | Improvement | Category | Step |
|---|-------------|----------|------|
| 1 | Storage permissions: `0700` dir, `0600` files, fix existing stores | Security | 0, 1 |
| 2 | Skip password-manager secrets (`x-kde-passwordManagerHint`) and configurable ignore patterns | Security | 1, 2 |
| 3 | Replace TCP `serve` with a Unix socket owned by the daemon; read timeout; per-connection handling | Security | 2 |
| 4 | Retention: expire unpinned entries after N days; per-entry and total size caps | Security | 1 |
| 5 | Clean shutdown on SIGTERM | Functionality | 0, 2 |
| 6 | Fix systemd unit (file name, Documentation URL, sandboxing vs lock/socket paths) | Functionality | 2 |
| 7 | Paste into the previous window after picking (`wtype`) | Functionality | 4 |
| 8 | Scriptable mode: `list --json`, `get <n\|id>`, `search <q> --json`, stable exit codes | Functionality | 3 |
| 9 | Named snippets: alias pinned entries, `get @alias` | Functionality | 1, 3, 4 |
| 10 | Edit entry in `$EDITOR` before copying | Functionality | 4 |
| 11 | `rpaste`: pull the local clipboard to a remote machine | Functionality | 2, 3 |
| 12 | Event-driven capture via `wl-paste --watch`, polling as fallback | Performance | 2 |
| 13 | SQLite (WAL) + FTS5 storage | Performance | 1 |
| 14 | ~~Daemon as single writer~~ **Dropped.** SQLite handles concurrent writers; only the daemon-owned socket (item 3) remains | — | — |
| 15 | Wrap preview to the actual pane width (not hard-coded 80) | Aesthetics | 4 |
| 16 | Fixed-width compact time column (`5s`, `12m`, `3d`) | Aesthetics | 4 |
| 17 | Visible-whitespace / line-number toggle in preview | Aesthetics | 4 |

## Key decisions

- **SQLite is the foundation.** Retention (4), aliases (9), scriptable search (8),
  permissions (1) and ingest-time secret filtering (2) are all built on it, so it
  lands first to avoid doing that work twice.
- **No single-writer daemon.** SQLite (WAL mode) handles concurrent processes.
  The daemon owns only the Unix socket used for remote copy/paste.
- **One ingest path.** Every save (daemon, `copy`, socket, picker undo) goes
  through `ingest(content, source, mime_types)`, which applies secret filtering
  and size caps.
- **Socket protocol supports push and pull from day one**, so `rpaste` needs no
  protocol change later.
- **Human-readable storage is traded for correctness and speed.** `clipstack
  export` keeps data inspectable.

## Steps

### Step 0: Quick fixes on the current code
Independent changes that survive the storage migration.
- [1] Permissions helper: create storage dir `0700`, files `0600`; tighten existing stores at startup.
- [5] Enable `ctrlc`'s `termination` feature so SIGTERM stops the daemon cleanly.

### Step 1: Storage core (`storage.rs` → SQLite module)
- [13] SQLite (WAL) + FTS5. Schema columns planned for later steps: `pinned`, `alias`, `source`, `mime`, `size`, `last_used`, `storage_version`.
- One-time migration from `index.json` + content files; `clipstack export`.
- Single `ingest()` path applying [2] secret filter (mime types + ignore patterns) and [4] size caps and expiry.
- [1] Permissions helper applied to the database file.
- Remove: `index.lock`, `attempt_recovery`/`recover` (or reimplement as DB integrity check), ID generation, content-search caps.

### Step 2: Daemon (`daemon.rs`, `clipboard.rs`, systemd)
- [12] `wl-paste --watch` with polling fallback; read `--list-types` per change and pass mime types to `ingest` ([2]).
- [3] Unix socket in `$XDG_RUNTIME_DIR`; framed `push`/`pull` protocol ([11] ready); read timeout; thread per connection. TCP `serve` removed or opt-in with token.
- [5] Verify clean shutdown with the watch child process.
- [6] Rewrite the systemd unit for the new socket and lock paths.

### Step 3: Command line (`main.rs`, `scripts/`)
- [8] `list --json`, `get <n|id>`, `search <q> --json` (FTS5), stable exit codes.
- [9] `get @alias`, `alias <id> <name>`.
- [11] `rcopy`/`rpaste` over SSH Unix-socket forwarding; README setup docs.

### Step 4: Picker (`picker.rs`)
- Restructure first (FEATURE_PLAN §0.1, §0.4): split view state from data; debounce search; search queries FTS5.
- [15] Wrap to pane width, [16] fixed-width time column, [17] whitespace toggle.
- [9] Name a pin with `P`; show aliases in the list.
- [10] `e` edits in `$EDITOR`.
- [7] Paste into previous window via `wtype` (last; external integration).

## Engineering rules (apply to every step)

- TDD: failing test first, then implementation. Full `cargo test` before every commit.
- Smallest coherent commits; one-to-two line messages ending with the
  `Co-Authored-By` / `Claude-Session` trailers.
- `cargo clippy --all-targets` must be clean. `ubs <changed files>` before each commit.
- Shell E2E suites (`tests/e2e_*.sh`, `tests/integration/*.sh`) must pass at the
  end of each step (they need `cargo build --release`).
- Update README and any other affected docs in the same step.
- **Never stage `.beads/`** (an intentional, uncommitted deletion is in the
  working tree). Stage files by explicit path; never `git add -A` / `git commit -a`.
- Never touch the user's real store (`~/.local/share/clipd`) or a running
  daemon; use `--storage-dir` with temp dirs.

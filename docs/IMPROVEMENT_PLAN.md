# ClipStack Improvement Plan

> Status: **IMPLEMENTED** (Steps 0-4, 2026-11-01); **Step 5 in progress**. Each step has an
> "Implementation status" line. Steps 0-4 below were fully specified and the open
> questions are decided. The user chose the recommended answers for Q2
> (retention off by default) and Q10 (`/` starts a search).
>
> This plan runs **before** `docs/FEATURE_PLAN.md`. It replaces FEATURE_PLAN
> §0.0 (storage correctness contract) and most of §0.2–0.3 (async loading and
> content cache), and makes Feature 1's deep-search design simpler (FTS5
> replaces lazy file scanning).

## Review notes

Verified against the code and tools on 2026-11-01: schema v1 (triggers, FTS5
external content over the virtual generated column, `secure-delete`, the
window-function prune, the alias CHECK) runs cleanly in sqlite3 3.54.0;
rusqlite 0.41.1 bundles SQLite 3.54.1 with MSRV 1.89; toml 1.1.8 has the
`std`/`parse`/`serde` features; ctrlc 3.5 has `termination`; rustix 1.1 exposes
`process::set_parent_process_death_signal(Option<Signal>)` with
`signal::Signal::TERM`; wl-clipboard 2.3.0 documents `CLIPBOARD_STATE`
`data|nil|clear|sensitive`; the local `nc` is OpenBSD netcat.

Substantive changes made in this review:

- **Retention is applied by `expire()` at `Storage::open` and by the daemon
  tick, not by a `?cutoff` filter on every read.** One code path instead of
  five, and `nth`/`counts`/`list` can't disagree. Reads are plain queries.
- **Socket server handles one connection at a time.** Thread-per-connection,
  `max_connections`, the `AtomicUsize` guard and `ERR busy` are gone. The
  client is one `ssh` forward; a 5 s timeout bounds a stuck client.
- **Entry-id reference is `id:42`, not `#42`.** `#` starts a shell comment at
  the start of a word, so `clipstack get #42` silently became `clipstack get`.
- **Step 1 module coexistence is specified.** `src/storage.rs` and
  `src/storage/mod.rs` can't both exist, so commit 1-1 moves the JSON code to
  `src/storage/json.rs` and callers switch to it until 1-12. Without this the
  first eleven commits don't compile.
- **`index.lock` is left in place by the legacy import.** Moving it would
  break mutual exclusion with a still-running old daemon (it keeps a lock on
  the renamed inode while new processes create a fresh file).
- **`fs2` is dropped entirely in 1-13.** std `File::lock`/`try_lock` (stable
  since 1.89, `flock`-based on Linux, so compatible with an old daemon's fs2
  lock) covers both the daemon lock and the legacy import lock.
- **The legacy upgrade note says to `clipstack kill` first.** Otherwise every
  new-version command creates another `legacy-backup-*` dir while the old
  daemon keeps writing `index.json`.
- **rcopy is rewritten in Step 2 (commit 2-13), with the TCP removal.** No
  window where remote copy is broken (Q3).
- **Picker spawns `current_exe()` in Step 0 (commit 0-8), not Step 2.** It is
  a one-line isolation fix like 0-7; a test that runs `pick` must never start
  the installed binary.
- **`get --copy` bumps the entry itself** via `ingest`, so it works without a
  running daemon and is deterministic in tests.
- **The wtype paste helper is detached** (`process_group(0)`, null stdio) so
  the terminal closing with the picker doesn't take it down.
- **Performance tests are `#[ignore]` with release-mode numbers.** The listed
  thresholds are unreachable in a debug build and would make `cargo test`
  flaky.
- **Dropped `foreign_keys = ON`** (no foreign keys), the `check --purge-legacy`
  and "check reports entries matching patterns" options (Q5/Q6), and the
  `rejects_fallback_owned_by_other_uid` test, which tested mode, not owner.
- **Commit 1-12 is split** into 1-12a (main, daemon) and 1-12b (picker); it was
  the largest commit in the plan.
- **Open questions resolved** (table below). Q2 and Q10 were decided by the user.

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
| 18 | Image history (FEATURE_PLAN Feature 14): PNG capture, preview, paste | Functionality | 5 |
| 19 | Maccy-style omarchy-shell overlay; `pin`/`unpin`/`delete`/`edit` commands | Functionality | 5 |

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

---

## Cross-step contracts

Later steps depend on these interfaces. Step 1 and Step 2 must ship them exactly as written here. A change to any of them means updating this section first.

### Paths

| Thing | Path | Mode | Introduced |
|-------|------|------|------------|
| Storage dir | `--storage-dir`, else `$XDG_DATA_HOME/clipd` (`~/.local/share/clipd`) | `0700` | 0 |
| Database | `<storage>/clipstack.db` (+ `-wal`, `-shm`) | `0600` | 1 |
| Legacy backup | `<storage>/legacy-backup-<unix_ms>/` | `0700` dir, `0600` files | 1 |
| Config | `--config PATH`, else `$XDG_CONFIG_HOME/clipstack/config.toml` | read-only to us | 1 |
| Runtime dir | `$XDG_RUNTIME_DIR`, else `/tmp/clipstack-<uid>/` (created `0700`, must be owned by us) | — | 2 |
| Daemon lock (PID inside) | `<runtime>/clipstack.lock` (path unchanged from today) | `0600` | 0 (mode), 2 (fallback) |
| Daemon socket | `<runtime>/clipstack.sock` | `0600` | 2 |

### Storage API (Step 1; `src/storage/mod.rs`)

```rust
pub type EntryId = i64; // SQLite rowid, AUTOINCREMENT, never reused

#[derive(Debug, Clone, Serialize)]
pub struct ClipEntry {
    pub id: EntryId,
    pub created_at: i64,       // unix ms, first time this content was seen
    pub last_used: i64,        // unix ms, last copy/re-copy; recency sort key (replaces `timestamp`)
    pub size: usize,           // bytes
    pub preview: String,       // first 100 chars, control chars -> ' '
    pub pinned: bool,
    pub alias: Option<String>, // only when pinned
    pub source: Source,
    pub mime: String,          // chosen text mime, e.g. "text/plain;charset=utf-8"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source { Watch, Poll, Copy, Remote, Picker, Edit, Import, Unknown }

pub enum IngestOutcome { Inserted(ClipEntry), Bumped(ClipEntry), Skipped(SkipReason) }
pub enum SkipReason { Empty, Sensitive, IgnoredPattern { index: usize }, TooLarge { size: usize, limit: usize } }

pub enum ListOrder { Recency, PinnedFirst }
pub struct Counts { pub total: usize, pub pinned: usize, pub total_bytes: u64, pub oldest: Option<i64>, pub newest: Option<i64> }

impl Storage {
    pub fn open(dir: PathBuf, max_entries: Option<usize>, policy: IngestPolicy) -> Result<Storage>;
    pub fn default_dir() -> PathBuf;
    pub fn base_dir(&self) -> &Path;
    pub fn db_path(&self) -> &Path;
    pub fn max_entries(&self) -> usize;
    pub fn limit_is_saved(&self) -> bool;                       // replaces index_exists()
    pub fn policy(&self) -> &IngestPolicy;

    pub fn ingest(&self, content: &str, source: Source, mime_types: &[String]) -> Result<IngestOutcome>;

    pub fn list(&self, order: ListOrder, limit: Option<usize>) -> Result<Vec<ClipEntry>>;
    pub fn counts(&self) -> Result<Counts>;
    pub fn get(&self, id: EntryId) -> Result<Option<ClipEntry>>;
    pub fn nth(&self, position: usize) -> Result<Option<ClipEntry>>; // 1-based, Recency order
    pub fn by_alias(&self, alias: &str) -> Result<Option<ClipEntry>>;
    pub fn load_content(&self, id: EntryId) -> Result<String>;
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<ClipEntry>>; // FTS5, PinnedFirst order

    pub fn delete_entry(&self, id: EntryId) -> Result<bool>;
    pub fn toggle_pin(&self, id: EntryId) -> Result<bool>;
    pub fn set_pinned(&self, id: EntryId, pinned: bool) -> Result<()>;      // unpin clears alias
    pub fn set_alias(&self, id: EntryId, alias: Option<&str>) -> Result<()>; // pins if needed
    pub fn clear(&self) -> Result<usize>;
    pub fn expire(&self) -> Result<usize>;                       // retention + caps, no insert; also run by open()
    pub fn retry_pending_scrub(&self) -> Result<Option<bool>>;  // daemon tick: rerun a WAL scrub a reader blocked
    pub fn check(&self, repair: bool) -> Result<CheckReport>;
    pub fn export_jsonl(&self, out: &mut dyn Write) -> Result<usize>;
    pub fn legacy_backup_dirs(&self) -> Vec<PathBuf>;
    pub fn remove_legacy_backups(&self) -> Result<(usize, Vec<PathBuf>)>; // `clear`: (removed, left because not only regular files)
}
```

`Storage` is `Send + Sync` (it holds a `Mutex<rusqlite::Connection>`). Each process opens its own `Storage`. Write transactions use `BEGIN IMMEDIATE`. `open()` runs `expire()` once (a `SELECT EXISTS` first, so a read-only command normally takes no write lock; `SQLITE_BUSY` here is ignored), so reads never need a retention filter.

### Config (Step 1 adds `[storage]`, Step 2 adds `[capture]` and `[socket]`, Step 4 adds `[picker]`; `src/config.rs`)

```rust
#[derive(Deserialize, Default)] #[serde(default, deny_unknown_fields)]
pub struct Config { pub storage: StorageConfig, pub capture: CaptureConfig, pub socket: SocketConfig, pub picker: PickerConfig }

pub struct StorageConfig {        // defaults in the TOML below
    pub retention_days: u32,      // 0 = never expire
    pub max_entry_bytes: u64,
    pub max_total_bytes: u64,
    pub ignore_patterns: Vec<String>,
    pub builtin_secret_patterns: bool,
}
pub struct CaptureConfig { pub mode: CaptureMode /* Auto|Watch|Poll */, pub poll_interval_ms: u64 }
pub struct SocketConfig  { pub enabled: bool, pub allow_pull: bool, pub timeout_ms: u64 }
pub struct PickerConfig  { pub search_debounce_ms: u64, pub paste_after_pick: bool, pub paste_keys: String } // Step 4

impl Config {
    pub fn load(explicit: Option<&Path>) -> Result<(Config, ConfigSource)>; // missing default file => defaults
    pub fn ingest_policy(&self) -> Result<IngestPolicy>;                   // compiles regexes
}
pub struct IngestPolicy { ignore: regex::RegexSet, max_entry_bytes: usize, max_total_bytes: u64, retention: Option<Duration> }
```

### Socket protocol v1 (Step 2; `src/protocol.rs`)

```
request  = "CLIPSTACK/1" SP verb SP length LF body
verb     = "PUSH" / "PULL" / "PING"
length   = 1*10DIGIT            ; PUSH: 0..=limit; PULL and PING: must be 0
response = "OK" SP length *(SP key "=" value) LF body
         / "ERR" SP code SP message LF
code     = "bad_request" / "too_large" / "not_utf8" / "forbidden" / "sensitive"
         / "timeout" / "unavailable" / "internal"
```

- One request per connection, handled sequentially by the accept thread. The server closes the connection after it responds. Because requests are length-prefixed, clients never need to half-close, so `nc -U` without `-N` works.
- The header line may be at most 256 bytes. Clients must ignore `key=value` pairs they don't recognise.
- Rust API: `Request { Push(Vec<u8>), Pull, Ping }`, `Response { Ok { body, meta: Vec<(String,String)> }, Err { code: ErrCode, message } }`, `read_request(&mut impl BufRead, max_body) -> Result<Request, ProtoError>`, `write_response`, `write_request`, `read_response`.

### CLI contracts (Step 3)

- `EntryRef` grammar: `N` = 1-based position in Recency order, `id:ID` = entry id, `@alias`. (`#ID` was rejected: `#` starts a shell comment at the start of a word.)
- Exit codes:

| Code | Meaning |
|------|---------|
| 0 | OK |
| 1 | Not found or no match (grep-like) |
| 2 | Usage error (clap default) |
| 3 | Daemon or socket unavailable |
| 4 | Storage error (locked, corrupt, newer schema) |
| 5 | Config error |
| 6 | Clipboard tool failure |

### Clipboard (Step 2; `src/clipboard.rs`)

`Clipboard { paste_bin: PathBuf, copy_bin: PathBuf }` with `Clipboard::system()` (`"wl-paste"`, `"wl-copy"`), plus `copy(&str)`, `paste_text() -> Result<String>` (`--no-newline --type text`), `list_types() -> Result<Vec<String>>`. Constant: `PASSWORD_HINT_MIME = "x-kde-passwordManagerHint"`.

---

## Steps

### Step 0: Quick fixes on the current code

> **Implementation status:** done (a65f3c8..b715b88): permissions helper, test harness, SIGTERM handling, `current_exe` autostart.

Independent changes that survive the storage migration.
- [1] Permissions helper: create storage dir `0700`, files `0600`; tighten existing stores at startup.
- [5] Enable `ctrlc`'s `termination` feature so SIGTERM stops the daemon cleanly.

#### 0.1 Design

**`src/perms.rs`** (new; uses only std `os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, MetadataExt}`):

```rust
pub const DIR_MODE: u32 = 0o700;
pub const FILE_MODE: u32 = 0o600;
/// create_dir_all; newly created leaf gets 0700; existing leaf is chmod'ed to 0700.
pub fn ensure_private_dir(path: &Path) -> io::Result<()>;
/// OpenOptions with .mode(0o600) (applied only on create).
pub fn private_file_options() -> fs::OpenOptions;
/// chmod dir 0700 and every regular file directly inside it 0600 when group/other bits
/// are set. Uses symlink_metadata (never follows symlinks), is not recursive, and skips
/// entries not owned by the dir's owner. Returns what changed so the first run can log it once.
pub fn tighten_dir(path: &Path) -> io::Result<TightenReport>;
pub struct TightenReport { pub dir_changed: bool, pub files_changed: usize, pub skipped: usize }
```

- `Storage::new`: replace `fs::create_dir_all` with `perms::ensure_private_dir`, then call `perms::tighten_dir` while holding `index.lock`. If any file changed, print `[storage] tightened permissions on N files` once to stderr (the dir itself was already fixed by `ensure_private_dir`).
- `atomic_write`: create the temp file with `private_file_options()`. The rename keeps the mode.
- `lock_index` and the daemon lock file: create with `.mode(0o600)`.
- `Cargo.toml`: `ctrlc = { version = "3.5", features = ["termination"] }`. That handles SIGINT, SIGTERM and SIGHUP. The daemon loop already polls `running` every 250 ms and returns `Ok(())`, so the process exits 0 and prints `clipstack daemon stopped`.

#### 0.2 Migration and compatibility

- Existing stores (the real one is currently a `0755` dir with `0644` files) are tightened on the next run of any command. Nothing else about the format changes.
- `recover`, `kill`, `serve`, `--max-entries`, `CB_PORT` and rcopy are unchanged in this step.

#### 0.3 File-by-file

| File | Change |
|------|--------|
| `src/perms.rs` | New (above) |
| `src/main.rs` | `mod perms;` |
| `src/storage.rs` | Use perms in `new`, `atomic_write`, `lock_index` |
| `src/daemon.rs` | Lock file `0600` |
| `Cargo.toml` | `ctrlc` `termination` feature |
| `tests/fakes/wl-paste` | New: minimal fake (`--no-newline` → exit 1 "No selection", `--version`; calls logged to `$FAKE_CLIP_DIR/wl-paste.calls`). Extended in Step 2 |
| `tests/fakes/wl-copy` | New: minimal fake (stdin → `$FAKE_CLIP_DIR/content`) so `copy`/picker tests can't reach the real clipboard. Extended in Step 2 |
| `tests/lib.sh` | New: `isolate_env`, `track_pid`, `cleanup` for shell suites (see 0.5) |
| `tests/common/mod.rs` | New: `TestEnv` harness (see 0.5) |
| `tests/daemon_lifecycle.rs` | New Rust integration test |
| `tests/integration/test_component_interaction.sh` | Remove `pkill -f "clipstack.*daemon"` (it kills the user's real daemon); isolate `XDG_RUNTIME_DIR` |
| `src/picker.rs` | `ensure_daemon_running` spawns `std::env::current_exe()` instead of `"clipstack"` from PATH |

#### 0.4 Commits (each test-first)

| # | Commit | Failing test(s) first |
|---|--------|-----------------------|
| 0-1 | Add perms helper for private dirs/files | `perms::tests::ensure_private_dir_creates_0700`, `ensure_private_dir_fixes_existing_0755`, `private_file_options_creates_0600`, `tighten_dir_fixes_world_readable_files`, `tighten_dir_does_not_follow_symlinks` |
| 0-2 | Create storage dir 0700 and content/index files 0600 | `storage::tests::test_new_storage_dir_is_0700`, `test_saved_content_and_index_are_0600` |
| 0-3 | Tighten existing world-readable stores on open | `storage::tests::test_open_tightens_existing_store` (pre-create `0755`/`0644`) |
| 0-4 | Create daemon lock file 0600 | `daemon::tests::test_lock_file_is_0600` |
| 0-5 | Add test harness with fake wl-paste and isolated runtime dir | `tests/daemon_lifecycle.rs::harness_isolates_runtime_dir` (asserts the lock appears under the temp `XDG_RUNTIME_DIR`, not the real one) |
| 0-6 | Stop the daemon cleanly on SIGTERM | `tests/daemon_lifecycle.rs::daemon_exits_zero_on_sigterm` (spawn `CARGO_BIN_EXE_clipstack daemon`, wait for the PID in the lock, `kill -TERM`, expect exit code 0 within 2 s and stderr containing `daemon stopped`; today it dies by signal, so `code()` is `None`) |
| 0-7 | Stop integration test killing every clipstack daemon | Shell change only. Verify by running the script while a sentinel `sleep` process named `clipstack-daemon-sentinel` exists and confirm it survives |
| 0-8 | Spawn the current executable when auto-starting the daemon | `picker::tests::daemon_spawn_program_is_current_exe` (the existing `daemon_spawn_args` test pattern, extended to the program path) |

#### 0.5 Test plan

- Unit tests as listed above; existing tests stay green.
- **Isolation harness** (`tests/common/mod.rs`), reused by every later step:
  - `TestEnv::new()` creates temp dirs for `XDG_RUNTIME_DIR`, `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `HOME` and storage, plus a `FAKE_CLIP_DIR` state dir.
  - `TestEnv::cmd()` returns a `std::process::Command` for `env!("CARGO_BIN_EXE_clipstack")` with those env vars set, `PATH=<repo>/tests/fakes:$PATH`, `WAYLAND_DISPLAY`, `WAYLAND_SOCKET` and `CLIPSTACK_MAX_ENTRIES` removed, and `--storage-dir` set.
  - `Drop` kills only the children it spawned.
  - A guard asserts the runtime dir is not `/run/user/<uid>`.
- Shell E2E: add `tests/lib.sh` with the same `isolate_env` (exports the XDG vars, `HOME` and PATH; keeps `CARGO_HOME`/`RUSTUP_HOME` pointing at the real toolchain) and a `cleanup` that kills only PIDs recorded with `track_pid`. Source it from `test_component_interaction.sh`.

#### 0.6 Docs

- README "Storage": note the `0700`/`0600` modes and that existing stores are tightened automatically.
- README "Running the Daemon": SIGTERM/`systemctl stop` exits cleanly.

#### 0.7 Risks and open questions

- `tighten_dir` on a user-supplied `--storage-dir` that is a shared directory will chmod it to `0700`. That's acceptable because it is ClipStack's own directory, but it should be documented.
- Commits 0-7 and 0-8 go beyond the two skeleton items. They are needed so that running the suites at the end of Step 0 can neither kill the user's daemon nor start the installed binary (Q8, approved).

---

### Step 1: Storage core (`storage.rs` → SQLite module)

> **Implementation status:** done (89eb9a9..a52ae06, 4290549): SQLite/FTS5 storage, migration from `index.json`, `export`.

- [13] SQLite (WAL) + FTS5. Schema columns planned for later steps: `pinned`, `alias`, `source`, `mime`, `size`, `last_used`, `storage_version`.
- One-time migration from `index.json` + content files; `clipstack export`.
- Single `ingest()` path applying [2] secret filter (mime types + ignore patterns) and [4] size caps and expiry.
- [1] Permissions helper applied to the database file.
- Remove: `index.lock`, `attempt_recovery`/`recover` (or reimplement as DB integrity check), ID generation, content-search caps.

#### 1.1 Design

**Crates** (checked on crates.io, 2026-11-01):

| Crate | Version / features | Why |
|-------|--------------------|-----|
| `rusqlite` | `0.41` (0.41.1), `features = ["bundled", "functions"]` (`functions` for the `fold()` case-folding SQL function) | Bundled SQLite 3.54.1 is compiled with `SQLITE_ENABLE_FTS5`. `bundled` already implies `modern_sqlite`, and the default `cache` feature gives `prepare_cached`. MSRV is 1.89 |
| `regex` | `1.13` | Ignore patterns. Linear-time matching, so a hostile pattern can't cause catastrophic backtracking |
| `toml` | `1` (1.1.8), `default-features = false, features = ["std", "parse", "serde"]` | Config parsing. Writing TOML isn't needed |
| `fs2` | removed in 1-13 | std `File::lock` / `try_lock` (stable since 1.89, `flock` on Linux, so it interoperates with an old daemon's fs2 lock) covers the daemon lock and the legacy import's `index.lock` |

Raising MSRV to 1.89 means `Cargo.toml` gets `rust-version = "1.89"` and the README changes from "Rust 1.85+".

**Module layout** (`src/storage.rs` becomes `src/storage/`):

```
src/storage/mod.rs     Storage, ClipEntry, EntryId, ListOrder, Counts, CheckReport; open(); mutations
src/storage/schema.rs  PRAGMAs, DDL, SCHEMA_VERSION, open_connection(), apply_migrations()
src/storage/ingest.rs  Source, IngestOutcome, SkipReason, IngestPolicy, classify() (pure),
                       make_preview(), insert_or_bump(), enforce_limits()
src/storage/query.rs   list/get/nth/by_alias/search/counts, fts_query() builder
src/storage/legacy.rs  detect/read index.json + {ms}[_nnn].txt, import(), move_to_backup()
src/config.rs          Config (see contracts), load(), builtin secret patterns
```

**Module coexistence during Step 1.** `src/storage.rs` and `src/storage/mod.rs` cannot both exist. Commit 1-1 does `git mv src/storage.rs src/storage/json.rs`, adds `pub mod json;` to the new `mod.rs`, and switches `main.rs`, `daemon.rs` and `picker.rs` to `use crate::storage::json::Storage` (one line each). The new `storage::Storage` is built and tested in 1-1..1-11 without any caller. 1-12a/b switch the callers back to `storage::Storage`; 1-12b deletes `json.rs` together with the last caller (with the module-wide `allow(dead_code)` gone, a dead `json.rs` fails `clippy -D warnings`, so it can't wait for 1-13). The storage API whose first caller is Step 3 (`get`, `nth`, `by_alias`) carries a per-item `#[cfg_attr(not(test), expect(dead_code, reason = ...))]`, which turns into an error once 3-4 calls them and so must be removed there. `legacy.rs` defines its own `LegacyIndex { max_entries, entries: Vec<LegacyEntry { id, timestamp, pinned }> }` serde structs (serde ignores `size`, `preview` and `hash`, which are recomputed from the content) so nothing depends on `json.rs`.

**Connection setup** (`schema::open_connection(path)`):

1. `perms::ensure_private_dir(dir)`. Pre-create `clipstack.db` with `private_file_options()` (`create(true)`, no truncate) so SQLite inherits `0600`. SQLite gives `-wal` and `-shm` the same mode as the db file. Then chmod any existing `-wal`/`-shm` to `0600` (they may be left over from an older umask).
2. `Connection::open(path)`, then `conn.busy_timeout(5s)`.
3. `auto_vacuum = INCREMENTAL` **first**: on a new file it only sticks if it is set before `journal_mode = WAL`, which writes the database header (checked in sqlite3 3.54: in the other order it reads back `0`). On an existing file it is a no-op.
4. Per-connection PRAGMAs: `journal_mode = WAL` (persistent, re-asserting is harmless; switching a new file needs an exclusive lock and SQLite returns `SQLITE_BUSY` at once, without the busy handler, while another process is opening it, so this one PRAGMA is retried until the 5 s busy timeout), `synchronous = NORMAL`, `secure_delete = ON`, `trusted_schema = OFF`, `temp_store = MEMORY` (no plaintext temp files during sorts). `application_id = 0x434C5053` ("CLPS") is set in the v1 migration transaction.
5. `apply_migrations()`: read `user_version`. If it is 0, run the v1 DDL in a transaction and set `user_version = 1`. If it is greater than `SCHEMA_VERSION`, fail with `StorageError::NewerSchema { found, supported }` (exit 4 in Step 3).
6. `Storage::open` then runs the legacy import (below) if a legacy store is detected, and `expire()`.

**Schema v1:**

```sql
CREATE TABLE entries (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,      -- stable, never reused
  hash        BLOB    NOT NULL UNIQUE CHECK (length(hash) = 32),  -- sha256(content bytes)
  size        INTEGER NOT NULL CHECK (size >= 0),
  preview     TEXT    NOT NULL,
  created_at  INTEGER NOT NULL,                       -- unix ms
  last_used   INTEGER NOT NULL,                       -- unix ms; recency key
  pinned      INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
  alias       TEXT    UNIQUE CHECK (alias IS NULL OR (pinned = 1 AND length(alias) BETWEEN 1 AND 32)),
  source      TEXT    NOT NULL DEFAULT 'unknown',
  mime        TEXT    NOT NULL DEFAULT 'text/plain;charset=utf-8',
  content     TEXT    NOT NULL,                       -- last stored column (see notes)
  search_text TEXT    GENERATED ALWAYS AS (substr(content, 1, 262144)) VIRTUAL
);
CREATE INDEX entries_by_recency      ON entries (last_used DESC, id DESC);
CREATE INDEX entries_unpinned_by_age ON entries (pinned, last_used);

CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
-- keys: max_entries, legacy_imported_at, legacy_imported_count, scrub_pending

CREATE VIRTUAL TABLE entries_fts USING fts5(
  search_text, content='entries', content_rowid='id', tokenize='trigram'
);
INSERT INTO entries_fts(entries_fts, rank) VALUES ('secure-delete', 1);

CREATE TRIGGER entries_ai AFTER INSERT ON entries BEGIN
  INSERT INTO entries_fts(rowid, search_text) VALUES (new.id, new.search_text);
END;
CREATE TRIGGER entries_ad AFTER DELETE ON entries BEGIN
  INSERT INTO entries_fts(entries_fts, rowid, search_text) VALUES ('delete', old.id, old.search_text);
END;
CREATE TRIGGER entries_au AFTER UPDATE OF content ON entries BEGIN
  INSERT INTO entries_fts(entries_fts, rowid, search_text) VALUES ('delete', old.id, old.search_text);
  INSERT INTO entries_fts(rowid, search_text) VALUES (new.id, new.search_text);
END;
PRAGMA user_version = 1;
```

Notes on the schema:

- **`content` is the last stored column.** SQLite stores a row's columns in order and spills a large row into overflow pages, so a column after `content` can only be read by walking those pages. With `content` last, the limit checks (`sum(size)` over unpinned rows on every ingest) and list queries read only each row's first page.
- **External-content FTS on a virtual generated column** means only the first 256 Ki characters of each entry are indexed. That keeps the trigram index (about 3-5x the indexed text) bounded, and `'rebuild'` and `'integrity-check'` still work. I checked this in an in-memory `sqlite3` 3.54 session: trigram matching is case-insensitive, `integrity-check` with rank 1 flags a damaged index, `rebuild` restores it, and queries shorter than 3 characters return nothing.
- **The `alias` CHECK** makes "alias only on pinned entries" a database invariant. Unpinning runs `UPDATE entries SET pinned = 0, alias = NULL`.
- **Schema version** is stored in `PRAGMA user_version`, not as a column (see Q1).

**Ingest** (`ingest.rs`):

```rust
pub fn classify(content: &str, mime_types: &[String], policy: &IngestPolicy) -> Option<SkipReason>; // pure
impl Storage {
    pub fn ingest(&self, content: &str, source: Source, mime_types: &[String]) -> Result<IngestOutcome> {
        self.ingest_at(content, source, mime_types, now_ms())
    }
    pub(crate) fn ingest_at(&self, ..., now_ms: i64) -> Result<IngestOutcome>; // tests inject time
}
```

1. `classify` checks, in this order:
   - `content.is_empty()` → `Empty`
   - any `mime_types` entry equals `x-kde-passwordManagerHint` → `Sensitive`
   - `content.len() > max_entry_bytes` → `TooLarge`
   - `ignore.matches(content)` → `IgnoredPattern { index }`
2. In a `BEGIN IMMEDIATE` transaction, compute `hash = sha256(content)`:
   - `UPDATE entries SET last_used = ?now WHERE hash = ?` returns the row → `Bumped`. Pin, alias, source and created_at are preserved.
   - Otherwise `INSERT` with `created_at = last_used = now`, preview from `make_preview` (same rules as today: 100 chars, control chars → space), `mime = mime_types.first_text_type().unwrap_or("text/plain;charset=utf-8")` → `Inserted`.
3. `enforce_limits(tx, now)` in the same transaction (pinned entries are never touched):
   - Max entries: `DELETE FROM entries WHERE id IN (SELECT id FROM entries WHERE pinned = 0 ORDER BY last_used DESC, id DESC LIMIT -1 OFFSET ?max_entries)`
   - Retention, if enabled: `DELETE FROM entries WHERE pinned = 0 AND last_used < ?cutoff`
   - Total bytes:
     ```sql
     DELETE FROM entries WHERE id IN (
       SELECT id FROM (SELECT id, SUM(size) OVER (ORDER BY last_used DESC, id DESC) AS running
                       FROM entries WHERE pinned = 0)
       WHERE running > ?max_total_bytes)
     ```
4. Commit. If step 3 deleted anything, do the best-effort checkpoint described under "Deleting secrets". Skips are logged by callers, never with the content, only `[source] skipped: <reason>`.

**`expire()`** runs steps 3-4 alone (no insert) and is called from `Storage::open` and the daemon tick, so expired entries are gone before any read in a fresh process and age out of a long-running daemon. Reads carry no retention filter. If `expire()` deleted anything it also does the best-effort checkpoint described under "Deleting secrets".

**Search** (`query.rs`):
- `fts_query(q)` splits on whitespace and keeps tokens of 3 or more characters. Each token is wrapped as an FTS5 phrase with internal `"` doubled (`"tok"`), and the phrases are joined with `AND`. If no token is long enough, it returns `None`.
- When there is an FTS query:
  ```sql
  SELECT e.* FROM entries_fts JOIN entries e ON e.id = entries_fts.rowid
  WHERE entries_fts MATCH ?1
  ORDER BY e.pinned DESC, e.last_used DESC, e.id DESC
  LIMIT ?2
  ```
  Short tokens, if any, are applied as an extra `AND instr(fold(e.search_text), ?) > 0` per token, with the token folded by the same function in Rust. `fold` is a deterministic scalar function registered on every connection (`schema::register_fold`, Rust `to_lowercase`), because SQLite's `lower()` folds only ASCII. Scanning `search_text` keeps short tokens to the same 256 K-character window as the index.
- When `fts_query` returns `None` (every token is shorter than 3 characters), use only the `instr` filter. Storage is capped (256 MiB default) and only the 256 K-character window is scanned, so the scan is bounded.
- Ranking beyond recency belongs to the picker (FEATURE_PLAN Feature 1).

**Config** (`src/config.rs`). Defaults, documented in README as a commented example:

```toml
# ~/.config/clipstack/config.toml   (all keys optional)
[storage]
retention_days = 0             # unpinned entries older than this are deleted; 0 = keep forever (see Q2)
max_entry_bytes = 10485760     # 10 MiB; larger copies are not saved
max_total_bytes = 268435456    # 256 MiB of unpinned content; oldest pruned first
builtin_secret_patterns = true # skip private keys and well-known API token formats
ignore_patterns = []           # extra regexes (Rust regex syntax); a match means "don't save"

[capture]                      # Step 2
mode = "auto"                  # auto | watch | poll
poll_interval_ms = 250

[socket]                       # Step 2
enabled = true
allow_pull = false             # rpaste needs this set to true
timeout_ms = 5000

[picker]                       # Step 4
search_debounce_ms = 50
paste_after_pick = false
paste_keys = "shift+Insert"
```

Validation (failure means config error, exit 5 in Step 3):
- `max_entry_bytes` in `1..=64 MiB`, and `max_total_bytes >= max_entry_bytes`.
- At most 64 user patterns, compiled with `RegexSetBuilder::size_limit(1 << 20)`.
- Error messages name the bad pattern and its index.
- `poll_interval_ms` in `50..=5000`, `[socket] timeout_ms` in `100..=60000`, `[picker] search_debounce_ms` in `0..=1000`, and `paste_keys` must parse (`paste_command`).

Built-in secret patterns, active when `builtin_secret_patterns = true`:
- `-----BEGIN [A-Z0-9 ]*PRIVATE KEY( BLOCK)?-----` (matches anywhere in the content; ` BLOCK` covers PGP private key blocks)
- The rest must match the whole content, trimmed, so prose or a script that merely contains a token is still saved:
  - `\A\s*gh[pousr]_[A-Za-z0-9]{36,255}\s*\z`
  - `\A\s*github_pat_[A-Za-z0-9_]{80,}\s*\z`
  - `\A\s*(AKIA|ASIA)[0-9A-Z]{16}\s*\z` (long-term and temporary AWS access key IDs)
  - `\A\s*xox[abprs]-[A-Za-z0-9-]{10,}\s*\z`
  - `\A\s*sk-[A-Za-z0-9_-]{20,}\s*\z`
  - `\A\s*sk_(live|test)_[A-Za-z0-9]{20,}\s*\z` (Stripe secret keys)

**`max_entries` stays in the database** (`settings.max_entries`), not in config. Resolution order is unchanged: CLI > `CLIPSTACK_MAX_ENTRIES` > stored > 100. `Some(n)` persists the value and prunes immediately. `limit_is_saved()` is true when the settings row exists. Status labels stay `cli`/`env`/`saved`/`default`. See Q7.

**Legacy import** (`legacy.rs`):
- **When it runs:** on every `Storage::open`, a legacy store is detected if `<dir>/index.json` exists or any file matches `^\d+(_\d+)?\.txt$`. Detection is two cheap stat/readdir calls.
- **Procedure** (idempotent and crash-safe):
  1. Take `<dir>/index.lock` with std `File::lock()` (same `flock` as the old code's fs2). This blocks a still-running old-version daemon's read-modify-write.
  2. Read `index.json`. If it can't be parsed, log it and continue with an empty list.
  3. Union the entries with orphan `.txt` files (a `scan(dir) -> LegacyScan` modelled on today's `attempt_recovery` scan; only regular files, never symlinks).
  4. Read each content file. Missing files are skipped. Non-UTF-8 files are counted, not imported, and moved to the backup dir with the rest (left in the root they would make every later open detect a legacy store and re-run the import).
  5. In one `BEGIN IMMEDIATE` transaction, insert oldest first with `source = 'import'`, `created_at` from the ID prefix (fallback: `timestamp`), and `last_used = timestamp`, using `INSERT ... ON CONFLICT(hash) DO UPDATE SET last_used = max(last_used, excluded.last_used), created_at = min(created_at, excluded.created_at), pinned = max(pinned, excluded.pinned)`. Index ids are only used if they name a content file found by the directory scan, so a crafted `index.json` can't point the import at another path. Each entry first goes through `classify` with the configured `IngestPolicy`, like any ingest: empty content, content over `max_entry_bytes`, and content matching an ignore or built-in secret pattern is not imported (counted as `skipped`; the file still moves to the backup in step 7, which is then its only copy). Pins are capped at `MAX_PINNED`: the most recently used pinned entries keep their pin, older ones are unpinned (reported as `unpinned`) before pruning. Of the caps, only max_entries pruning applies; retention and the total-size cap wait for the next open (see Q6).
  6. Set `settings.max_entries` from `index.json` only if it isn't already set. Set `legacy_imported_at` and `legacy_imported_count`. Commit.
  7. Only after the commit, `mkdir legacy-backup-<now_ms>` (`0700`) and `rename` each imported or kept file into it (`index.json`, the `*.txt` files, `*.tmp` files). `index.lock` stays where it is: it holds no data, and moving it would let an old daemon keep a lock on the renamed inode while new processes lock a fresh file. Detection ignores it.
- **Crash safety:** a crash before step 7 means the next open re-imports, and the upsert on hash prevents duplicates. A crash during step 7 leaves some files in the root, which are picked up and merged next time. Nothing is deleted.
- **Messages:** print once to stderr: `Imported N entries from index.json; old files kept in <dir>/legacy-backup-…/ (delete after checking)`, where N counts the distinct entries still in the history after max_entries pruning (also stored as `legacy_imported_count`). Pins dropped to stay within `MAX_PINNED` are reported on their own line, as are entries pruned by max_entries, skipped by the policy, or not UTF-8; when there are any, the `(delete after checking)` advice is dropped and a last line says the backup dir is the only copy of those entries. `status` keeps warning while a backup dir exists.
- **Old daemon still running:** if an old-version daemon keeps writing `index.json`, those writes are imported (merged) on the next open of any new-version command. Step 2's daemon refuses to start while the lock is held, so the user has to run `clipstack kill` first, which works because the PID file format is unchanged.

**`recover` → `check`:**
- `clipstack check [--repair]` runs `PRAGMA integrity_check` and FTS `integrity-check` (rank 1), and reports counts plus the legacy backups found.
- With `--repair`, it runs FTS `rebuild` if the FTS check failed (and re-checks; the report says `rebuilt`), then `PRAGMA wal_checkpoint(TRUNCATE)`. Core database corruption gets an actionable message: `clipstack export -o backup.jsonl`, then `sqlite3 clipstack.db .recover | sqlite3 recovered.db` (`.recover` writes SQL to stdout, so it has to be piped into a new database).
- `Storage::check` returns `CheckReport { integrity_errors, fts: FtsState::{Ok, Damaged, Rebuilt}, counts, legacy_backups }`. A failed check (core damage, or a damaged index without `--repair`) is `StorageError::Corrupt`, so Step 3-2 maps it to exit 4; until then it exits 1.
- `recover` stays as a hidden alias for `check --repair` and prints `Recovery complete. N entries.` so existing scripts that grep for "Recovery complete" keep working.

**`export`:**
- `clipstack export [-o FILE]` writes JSONL to stdout, oldest first (`last_used ASC, id ASC`, the reverse of `list`), one object per entry: `{"id","created_at","last_used","size","pinned","alias","source","mime","content"}`. One statement, so one consistent snapshot. A closed stdout (`export | head`) ends it quietly with status 0.
- `-o` creates FILE `0600` and refuses to overwrite unless `--force` is given; `--force` truncates and chmods the file `0600` before writing. It prints `Exported N entries to FILE` to stderr.
- No `import` command in this plan (see Q-misc).

**Deleting secrets:** `clear()`, `delete_entry`, a deleting `expire()` and an `ingest` whose `enforce_limits` pruned anything (it returns the delete count) run inside their transaction, then do a best-effort `PRAGMA wal_checkpoint(TRUNCATE)` followed by `PRAGMA incremental_vacuum`. With `secure_delete` and the FTS `secure-delete` option, deleted plaintext is overwritten in the db pages, and truncating the WAL removes the frames that carried the plaintext when it was inserted. The checkpoint cannot complete while another connection holds a read transaction (e.g. an open picker); it then returns busy, the scrub records `settings.scrub_pending` (visible to every process), and the daemon's 60 s tick (`Storage::retry_pending_scrub`) or the next deleting write retries and clears it. This is tested by grepping the db, `-wal` and `-shm` files in a single-process test.

#### 1.2 Migration and compatibility

| Item | After Step 1 |
|------|--------------|
| `~/.local/share/clipd` | Auto-imported on first open by any command. Originals moved to `legacy-backup-<ms>/` only after a successful commit. README upgrade note: run `clipstack kill` **before** the first new-version command; an old daemon left running keeps writing `index.json`, and every later new-version command would merge it and create another backup dir |
| `recover` | Hidden alias of `check --repair` |
| `kill` | Unchanged |
| `serve` | Unchanged except that it calls `ingest(Source::Remote)` |
| `--max-entries` / env | Unchanged semantics; persisted in `settings` |
| `CB_PORT`, rcopy | Unchanged (Step 2/3) |
| Entry IDs | Become integers. Legacy string IDs are not kept; nothing external referenced them |
| Empty content | No longer stored (`Skipped(Empty)`). Today `copy` with empty stdin stores an empty entry |
| Downgrade | An old binary sees no `index.json` and starts an empty history. Restore by moving the backup files back. Any `index.json` the old binary writes is merged by the next new-version run. Document this in README |
| Picker content search | Changes from fuzzy-on-content to FTS substring (3+ characters), and the 10-match/50-file caps are removed |

#### 1.3 File-by-file

| File | Change |
|------|--------|
| `Cargo.toml` | Add `rusqlite`, `regex`, `toml`; set `rust-version = "1.89"` |
| `src/storage.rs` | Moved to `src/storage/json.rs` in 1-1, deleted in 1-12b; replaced by `src/storage/{mod,schema,ingest,query,legacy}.rs` |
| `src/config.rs` | New |
| `src/main.rs` | Global `--config PATH`. Load config before storage. `list`/`stats`/`status` use `list()`/`counts()`. `copy` uses `ingest(Source::Copy)`, still copies to the clipboard when skipped, and reports why. `clear` returns a count and also removes the legacy backup dirs (`remove_legacy_backups`), printing how many. New `export` and `check`; `recover` becomes a hidden alias. `serve` uses `ingest(Source::Remote)`. `status` shows DB path, retention, caps, config path and legacy backups |
| `src/daemon.rs` | `save_entry` → `ingest(Source::Poll, &[])` (mime types arrive in Step 2) |
| `src/picker.rs` | `entries` from `list(ListOrder::PinnedFirst, None)`. `id: String` → `EntryId`. Phase-2 content search becomes `storage.search(query, 200)`, keeping `MatchLocation::Content` for hits whose preview didn't fuzzy-match. Remove `MAX_CONTENT_MATCHES`/`MAX_FILE_LOADS`. Undo uses `ingest(Source::Picker)` then `set_pinned`/`set_alias`. `entry.timestamp` → `entry.last_used` |
| `src/util.rs` | `compute_hash` reused. `compute_hash_string` removed once unused |
| `tests/migration_cli.rs` | New Rust integration tests (TestEnv) |
| `tests/e2e_*.sh`, `tests/integration/*.sh` | Fixtures still write `index.json` + `.txt` (now exercising the import path). Assertions that read `index.json` switch to `clipstack export \| jq -s`. `.tmp`-cleanup and recover tests are rewritten as import/backup/check tests |

#### 1.4 Commits

| # | Commit | Failing test(s) first |
|---|--------|-----------------------|
| 1-1 | Move JSON storage to storage/json.rs; add rusqlite and open a WAL database with schema v1 | `schema::tests::creates_entries_settings_and_fts`, `open_is_idempotent`, `rejects_newer_user_version`, `pragmas_wal_secure_delete_temp_store_memory` (the move itself is covered by the existing suite) |
| 1-2 | Create database and WAL/SHM files 0600 | `schema::tests::db_and_wal_files_are_0600`, `tightens_existing_0644_db` |
| 1-3 | Add config file with storage policy and built-in secret patterns | `config::tests::missing_file_gives_defaults`, `parses_storage_section`, `rejects_unknown_key`, `invalid_regex_error_names_pattern`, `rejects_total_cap_below_entry_cap`, `builtin_patterns_match_github_token_and_private_key`, `builtin_patterns_ignore_prose_mentioning_token` |
| 1-4 | Ingest inserts new entries and bumps duplicates | `ingest::tests::inserts_new_entry`, `duplicate_bumps_last_used_keeps_id`, `duplicate_preserves_pin_and_alias`, `preview_truncates_chars_and_sanitizes`, `unicode_content_roundtrips` |
| 1-5 | Skip empty, password-manager, oversize and ignored content | `ingest::tests::empty_is_skipped`, `password_hint_mime_is_skipped`, `oversize_is_skipped`, `ignore_pattern_match_is_skipped`, `classify_order_sensitive_before_size` |
| 1-6 | Enforce max entries, retention and total size on ingest and at open | `ingest::tests::prunes_unpinned_beyond_max_entries`, `pinned_survive_pruning`, `expires_unpinned_older_than_retention`, `retention_zero_disables_expiry`, `total_bytes_cap_prunes_oldest_unpinned`, `storage::tests::open_expires_old_unpinned_entries` (write with retention off, reopen with retention on, entry gone), `expire_returns_zero_without_write_when_nothing_expired` |
| 1-7 | Persist max_entries in settings with CLI/env/stored precedence | Port `test_open_without_limit_uses_stored_limit_and_does_not_prune`, `test_open_with_explicit_limit_persists_and_prunes`, `test_open_without_limit_on_fresh_dir_uses_default`, `test_max_entries_clamps_*`, `limit_is_saved_after_explicit_limit` |
| 1-8 | Pin, alias, delete and clear on SQLite (also adds `counts` and `by_alias`, which these tests read) | Port pin tests (`toggle_pin_respects_max_pinned` etc.), plus `set_alias_validates_name`, `alias_is_unique`, `set_alias_pins_entry`, `unpin_clears_alias`, `clear_removes_all`, `deleted_secret_not_in_db_files` (grep db/wal/shm bytes after `clear`) |
| 1-9 | Concurrent writers across processes lose nothing | Port `test_concurrent_writers_lose_no_updates` (8 threads × separate `Storage::open`), `test_concurrent_saves_dont_corrupt` (shared `Arc<Storage>`), `concurrent_first_open_creates_schema_once` (8 threads open a fresh dir at once; failed with `database is locked` from the WAL switch until it was retried) |
| 1-10 | Full-content search with FTS5 trigram index (also adds `list` and `nth`) | `query::tests::list_orders_by_recency_or_pinned_first`, `nth_is_one_based_in_recency_order`, `search_finds_term_beyond_preview`, `search_case_insensitive`, `search_escapes_fts_syntax` (query `"a" OR b*`), `short_tokens_use_substring_fallback`, `search_orders_pinned_then_recency`, `fts_index_limited_to_first_256k_chars`, `fts_rebuild_matches_trigger_index` |
| 1-11 | Import legacy index.json and content files into SQLite | `legacy::tests::imports_index_and_content`, `preserves_pin_recency_and_max_entries`, `includes_orphan_txt_files`, `handles_corrupt_index_json`, `dedupes_by_recomputed_hash`, `rerun_after_crash_before_move_is_idempotent`, `moves_files_to_backup_only_after_commit` (inject failure before commit; files must remain), `keeps_non_utf8_files_in_backup_without_importing`, `backup_dir_is_0700`, `reimports_index_written_by_old_daemon`, plus `detects_index_or_content_files_only`, `saved_limit_wins_over_index_json`, `import_prunes_to_max_entries_only`, `ignores_index_ids_that_are_not_content_file_names`, `no_import_when_no_legacy_files` |
| 1-12a | Switch CLI and daemon to the SQLite storage (`pick` keeps the JSON store until 1-12b; the `json.rs` methods only the CLI used are deleted; `Daemon::new(storage)` takes the opened `Storage` and runs `expire()` every 60 s) | `tests/migration_cli.rs::stats_after_import_matches_legacy_count`, `copy_skipped_by_policy_still_copies_and_reports`, `import_notice_warns_before_retention_expires`, `missing_explicit_config_is_an_error`, `bad_config_is_reported_before_opening_storage`, `status_shows_database_config_and_backups`, `clear_reports_count`, main `limit_label_saved_when_settings_row_exists`, daemon `saves_new_clipboard_content_once_with_poll_source` |
| 1-12b | Switch picker to the SQLite storage; delete storage/json.rs and `util::compute_hash_string` | picker `test_content_search_finds_non_preview_match` (unchanged test, now via FTS), `test_pinned_entries_shown_first_on_open` ported to integer IDs, `undo_restores_deleted_entry_with_pin_and_alias`, `undo_restores_unpinned_entry_unpinned`, `undo_after_window_does_not_restore` |
| 1-13 | Drop fs2; daemon lock via std File::try_lock | daemon `lock_is_held_while_a_daemon_runs_and_released_on_drop` (`is_running` delegates to `lock_is_held(path)` so the check is testable on a temp path); the existing daemon lock tests stay green |
| 1-14 | Add `export` (JSONL) | `query::tests::export_jsonl_oldest_first_with_all_fields`, `export_jsonl_empty_store_writes_nothing`, `tests/migration_cli.rs::export_jsonl_roundtrips_content`, `export_file_created_0600`, `export_refuses_overwrite_without_force`, `export_to_closed_pipe_exits_quietly` |
| 1-15 | Replace `recover` with `check [--repair]` (keep alias) | `storage::tests::check_ok_on_fresh_db`, `check_without_repair_reports_damaged_fts`, `check_repair_rebuilds_fts_after_delete_all`, `check_repair_on_healthy_db_changes_nothing`, `tests/migration_cli.rs::check_reports_ok_counts_and_backups`, `check_fails_on_damaged_index_until_repaired`, `recover_alias_prints_recovery_complete` (core-database damage can't be produced reliably in a test; that branch is reviewed, not tested) |
| 1-16 | Update E2E scripts for SQLite storage | Shell: scripts fail first against new behaviour, then are updated |
| 1-17 | Document SQLite storage, config file, export and check | — |

#### 1.5 Test plan

- **Unit tests:** as listed. Time-dependent tests use `ingest_at` with fixed `now_ms`. There are no sleeps.
- **Performance** (replaces `test_performance_large_entries`; `query::tests::perf_100_entries_of_500kb`), 100 × 500 KB entries, `#[ignore]` and run with `cargo test --release -- --ignored perf_` at the end of the step (debug builds of bundled SQLite and trigram indexing of 256 K characters per entry won't meet these). The `clipboard.rs` round-trip tests use the real `wl-copy`/`wl-paste`, so they skip themselves unless `CLIPSTACK_REAL_CLIPBOARD_TESTS=1` is set; a bare `--ignored` run is safe. The searched term sits in the first 256 K characters, since nothing past the FTS window is indexed. Measured in release: list 0.1 ms, load_content 0.3 ms, ingest 85 ms, search 0.5 ms:
  - `list(PinnedFirst, None)` < 10 ms
  - `load_content` < 20 ms
  - `ingest` of 500 KB < 100 ms
  - `search` < 20 ms
- **Rust integration** (`tests/migration_cli.rs`, TestEnv): build a legacy store fixture in a temp dir, run `stats`, `list`, `export`, `check` and `recover`, and assert backup contents and modes.
- **Shell E2E:**
  - `e2e_atomic_writes.sh` becomes the storage-integrity suite (import, backup, check, permissions); keep the file name so it's easy to find.
  - `e2e_search.sh`, `e2e_pinned.sh`, `e2e_preview.sh` and `integration/test_v11_features.sh` switch their assertions to `export | jq -s` (content compared exactly inside jq with `--arg`). Several of their old checks only read back the fixture files the script itself had written; those now go through the binary. `e2e_preview.sh` builds its fixture JSON with `jq` (it wrote invalid JSON for content with quotes or backslashes), and `e2e_pinned.sh` uses `grep -F` instead of the obsolescent `fgrep`.
  - `integration/test_component_interaction.sh`: 40 racing first opens must import once (one backup dir); the index.json-rewriting "atomic writes" test becomes 20 concurrent `copy` writers that must all be saved; the crash test checks the `.tmp` leftovers land in the backup and `recover` reports the count.
  - `integration/test_v11_features.sh` test 8 asserts that 30 imported pins are capped at `MAX_PINNED` (25), keeping the most recently used, with all 30 entries still in the history and the notice reporting 5 unpinned.
  - `e2e_max_entries.sh` is unchanged apart from fixture import.
  - All of them use `tests/lib.sh` isolation.
- **Real store:** never opened. One manual check, after the step merges and only with the user's go-ahead: `cp -a ~/.local/share/clipd /tmp/clipd-copy && clipstack --storage-dir /tmp/clipd-copy check`.

#### 1.6 Docs

README updates:
- **Storage:** new layout, `0600`, WAL files, legacy backup, downgrade note.
- Delete "Index Format" and "Inspecting Storage Manually". Replace them with `export | jq` and `sqlite3 -readonly` examples.
- Add a "Configuration file" section with the full TOML.
- **Storage Limits** table: retention, size caps, 256 K-character search window.
- **Search Features:** FTS replaces two-phase scanning, and the caps are gone.
- **Data Safety:** SQLite transactions replace atomic writes and `index.lock`.
- Commands table: `export`, `check`; drop `recover`.
- Architecture, Design Philosophy ("no database" and "human-readable JSON" claims) and Performance tables.
- Build: Rust 1.89+ and a C compiler (bundled SQLite).

FEATURE_PLAN updates:
- Banner on §0.0: superseded by IMPROVEMENT_PLAN Step 1.
- §0.2/§0.3: superseded, because SQLite reads are synchronous and fast and no worker or LRU is needed.
- Feature 1 "Deep Search Implementation Notes/Acceleration": replaced by `Storage::search`.
- Feature 13: `content_type` becomes a schema v2 `ALTER TABLE`.
- Feature 14: images need a `blobs` table in a future schema version.
- "Additional Recommendations → Config": done in Step 1.

Also add a "historical" banner to `docs/UPGRADE_PLAN.md` and `docs/IMPLEMENTATION_SPEC.md`, which describe the JSON store.

#### 1.7 Risks and open questions

- **FTS size:** trigram indexes run about 3-5x the indexed text. With the 256 K-character window and 256 MiB total cap, the worst case is about 1 GB, but typical use is a few MB. A lower window (64 K) can be decided after measuring.
- **Bundled SQLite build:** adds about 30-60 s to the first build and needs `cc`. That's acceptable.
- **Retention default**: any non-zero default deletes most of the existing 10-month history at the first `open` after import, because imported `last_used` is the legacy timestamp (Q2, decided by the user: ships `0`).
- **Import applies the ingest policy** (Q6). Secrets the patterns don't recognise stay until retention or a manual delete; the ones they do are left only in the plaintext backup dir.
- **SQLITE_BUSY:** a picker holding a read transaction doesn't block WAL writers. Writers wait up to 5 s. Long write transactions only happen during import.

---

### Step 2: Daemon (`daemon.rs`, `clipboard.rs`, systemd)

> **Implementation status:** done (9e736eb..4262c21, fixes through 43a1895): `wl-paste --watch`, Unix socket, systemd unit.

- [12] `wl-paste --watch` with polling fallback; read `--list-types` per change and pass mime types to `ingest` ([2]).
- [3] Unix socket in `$XDG_RUNTIME_DIR`; framed `push`/`pull` protocol ([11] ready); read timeout; one connection at a time. TCP `serve` removed; rcopy rewritten in the same commit.
- [5] Verify clean shutdown with the watch child process.
- [6] Rewrite the systemd unit for the new socket and lock paths.

#### 2.1 Design

**Module layout:**

```
src/daemon/mod.rs      Daemon (lock, lifecycle, tick loop, periodic expire), runtime_dir()
src/daemon/capture.rs  CaptureMode, Supervisor (pure state machine), WatchChild, Poller, run_hook()
src/daemon/socket.rs   SocketServer (accept loop, per-connection thread, limits)
src/protocol.rs        wire format (contracts section); shared by server, tests and the Step 3 client
src/clipboard.rs       Clipboard struct with bin paths; list_types(); paste_text()
```

**Capture with watch mode:**
- The daemon spawns `wl-paste --no-newline --type text --watch <current_exe> --storage-dir <abs> [--config <abs>] __watch-hook` (`--no-newline` so a trailing newline in the copied text is stored as copied; `--config` only when one was given on the command line). wl-paste runs the hook once per new selection, with the content on stdin and `CLIPBOARD_STATE` in the environment. wl-clipboard 2.3.0 sets `sensitive` when the offer contains `x-kde-passwordManagerHint`.
- The spawn (and every supervisor restart) happens from the main daemon thread (it lives as long as the daemon) with `pre_exec` setting `PR_SET_PDEATHSIG = SIGTERM`. That pre_exec block is the one `unsafe` block in the project and is documented. PDEATHSIG fires when the spawning thread exits, so it must be a long-lived thread.
- Crate: `rustix = { version = "1", features = ["process"] }`, already in the dependency tree through tempfile. Verified API: `rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::TERM))` (`rustix::signal` is private in 1.x; `Signal` is re-exported from `process`). After the prctl the child checks `getppid()` against the daemon's PID and fails the spawn if the daemon already died.
- The hook writes directly to SQLite. This is consistent with having no single writer. It inherits the daemon's environment, so `CLIPSTACK_MAX_ENTRIES` resolves the same way in both.

**Hidden `__watch-hook` subcommand** (`capture::run_hook`):
- Reads `CLIPBOARD_STATE`.
- `nil` or `clear`: drain stdin, exit 0. An unknown state is drained and skipped the same way.
- `sensitive`: drain stdin to a sink (so the source client doesn't get EPIPE), log `[watch] skipped sensitive`, exit 0.
- `data` or unset:
  1. Read stdin up to `max_entry_bytes + 1`, then drain the rest.
  2. Reject non-UTF-8.
  3. `types = clipboard.list_types()` with a 1 s timeout; on failure use `[]`.
  4. `ingest(content, Source::Watch, &types)`.
- Always exits 0 so wl-paste keeps running.

**Supervisor** (a pure state machine, unit-testable):

```rust
pub enum CaptureMode { Auto, Watch, Poll }
pub enum Event { Spawned(Instant), Exited { at: Instant, success: bool, stderr_tail: String }, Tick(Instant) }
pub enum Action { SpawnWatch, StartPolling { reason: String }, Wait, GiveUp(String) }
pub struct Supervisor { mode: CaptureMode, restarts: VecDeque<Instant>, state: SupState }
impl Supervisor { pub fn on(&mut self, ev: Event) -> Action; }
```

- `Auto`: if the first watch child exits within 2 s (or can't be spawned), switch to polling and log the reason (e.g. no data-control protocol). `Watch` gives up at once in that case.
- A later exit restarts with backoff (1 s, 2 s, 4 s ... max 30 s). After 5 restarts in 60 s, `Auto` falls back to polling and `Watch` gives up with an error exit.
- `Poll` never spawns.

**Poller:**
- Every `poll_interval_ms`, `paste_text()`. When the content hash changes, `list_types()` and `ingest(Source::Poll, &types)`.
- The hash is recorded only after `ingest` returns `Ok` (inserted, bumped or skipped, so a skipped secret isn't re-read on every tick). An ingest error leaves it unrecorded, so the next tick retries the same clip.
- Capture logs (`[watch]`, `[poll]`) give the entry id, byte size or skip reason, never a content preview.

**Periodic expire:** the daemon tick calls `storage.expire()` every 60 seconds, so secrets age out even when nothing new is copied, then `storage.retry_pending_scrub()` (see "Deleting secrets" in Step 1). `expire()` takes the write lock only when a cheap read finds something to delete, so the short interval costs one query a minute.

**Socket server:**
- After the lock is acquired, `remove_file(sock)` (a stale socket from a crash), bind, then `chmod 0600`. A path that isn't a socket, or a socket that still accepts connections (some other process), is left alone and the socket is disabled with a warning. The runtime dir is already `0700`, so the gap between bind and chmod doesn't matter.
- If the runtime dir is missing or not owned by us, the socket is disabled with a warning and capture continues.
- Accept loop on its own thread: listener is non-blocking and waited on with `poll(2)` (rustix `event` feature) for at most 100 ms, so it checks `running` every 100 ms without adding accept latency, and handles connections one at a time on that thread. There is no per-connection thread: the only client is the user's `ssh` forward, and the timeouts below bound how long a stuck client can hold the loop.
- Per connection:
  - `set_read_timeout` / `set_write_timeout(timeout_ms)` plus an overall deadline of `2 × timeout_ms`.
  - Reject headers over 256 bytes or that aren't ASCII.
  - Body limit is `min(max_entry_bytes, 16 MiB)`.
- `PUSH`:
  - Body not UTF-8 → `ERR not_utf8`.
  - Otherwise `ingest(Source::Remote, &[])`, then `wl-copy`, then reply `OK 0 stored=1 id=<id>` or `OK 0 stored=0 reason=<skip>`. Content that is skipped by policy is still copied to the clipboard, because the user sent it explicitly.
  - If wl-copy fails → `ERR unavailable …` (the entry stays stored).
- `PULL`:
  - `allow_pull = false` → `ERR forbidden pull disabled (set socket.allow_pull = true)`.
  - Clipboard types include the password hint → `ERR sensitive`.
  - Otherwise `OK <len>\n<content>`.
- `PING`: `OK 0 version=<pkg> capture=<watch|poll> pull=<0|1>`.
- No peer-credential check: the `0700` dir and `0600` socket restrict access to our uid, and `ssh -R` connections arrive as the local user anyway.

**Shutdown sequence** (SIGTERM, SIGINT, SIGHUP via ctrlc, or `stop()`):
1. `running = false`.
2. Wait up to 2 s for the accept thread (it may be inside a request; the timeouts bound that), then carry on without it.
3. `child.kill()` + `wait()` for the watch child.
4. Unlink the socket. A Drop guard does this only if the path is still the socket we bound (same device and inode).
5. Print `clipstack daemon stopped`.
6. Release the lock (when `main` drops the `Daemon`, right after `run` returns).

**Runtime dir:** `daemon::runtime_dir() -> Result<PathBuf>` returns `$XDG_RUNTIME_DIR`, or creates `/tmp/clipstack-<uid>` (`0700`) and verifies we own it. This replaces the shared `/tmp/clipstack.lock` fallback.

**Picker auto-start:** already spawns `std::env::current_exe()` since commit 0-8.

**TCP `serve` removal:** keep the subcommand hidden. It prints `serve was removed: the daemon listens on <runtime>/clipstack.sock; see README "Remote clipboard"` and exits 2. `CB_PORT` is dropped. The same commit (2-13) rewrites `scripts/rcopy` for the socket, exactly as specified in Step 3.1, so remote copy never has a broken window (Q3). Step 3 adds `rpaste`.

**systemd unit:** rename `systemd/clipd.service` to `systemd/clipstack.service`:

```ini
[Unit]
Description=ClipStack clipboard history daemon
Documentation=https://github.com/theirongolddev/clipstack#running-the-daemon
PartOf=graphical-session.target
After=graphical-session.target

[Service]
Type=simple
ExecStart=%h/.cargo/bin/clipstack daemon
Restart=on-failure
RestartSec=2
TimeoutStopSec=5
UMask=0077
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=-%h/.local/share/clipd %t
PrivateTmp=yes
RestrictAddressFamilies=AF_UNIX
LockPersonality=yes
MemoryDenyWriteExecute=yes
SystemCallArchitectures=native

[Install]
WantedBy=graphical-session.target
```

- `%t` is the user runtime dir. The current unit has `ProtectSystem=strict` without `%t`, so it can't create `clipstack.lock`, which is the bug behind goal 6.
- The `-` prefix tolerates a missing storage dir, so the README tells users to run `install -d -m 0700 ~/.local/share/clipd` before enabling.
- The README also explains that `WAYLAND_DISPLAY` must reach the user manager (`dbus-update-activation-environment --systemd WAYLAND_DISPLAY` in Hyprland autostart).

#### 2.2 Migration and compatibility

| Item | After Step 2 |
|------|--------------|
| `serve`, TCP 7779 | Removed (hidden stub, exit 2) |
| `CB_PORT` | Ignored. Removed from README |
| rcopy | Rewritten in the same commit for the Unix socket; users update `~/.ssh/config` (README gives before/after) |
| `kill` | Unchanged (PID in the same lock path); now also stops the watch child and removes the socket |
| Old daemon running at upgrade | The new daemon fails with "already running"; `clipstack kill` then start. Status shows `capture=unknown` when PING fails but the lock is held (an old daemon has no socket) |
| systemd users | Must re-copy the unit under its new name; README gives the commands. The old `clipstack.service` copy in `~/.config/systemd/user` is replaced in place, same name |

#### 2.3 File-by-file

| File | Change |
|------|--------|
| `src/daemon.rs` → `src/daemon/{mod,capture,socket}.rs` | As designed |
| `src/protocol.rs` | New |
| `src/clipboard.rs` | Struct with bin paths, `list_types`, `paste_text --type text`, `PASSWORD_HINT_MIME`. The `watch` polling fn is removed |
| `src/main.rs` | Hidden `__watch-hook`; `serve` stub; `status` adds socket path, PING result and capture mode; `check_dependencies` uses PATH lookup instead of `which` |
| `src/config.rs` | `[capture]`, `[socket]` |
| `scripts/rcopy` | Rewritten for the Unix socket (design in 3.1) |
| `Cargo.toml` | `rustix` |
| `systemd/clipd.service` → `systemd/clipstack.service` | Rewritten |
| `tests/fakes/wl-paste` | Supports `--list-types` (prints `$FAKE_CLIP_DIR/types`), `--no-newline [--type T]` (prints `$FAKE_CLIP_DIR/content` or exits 1 "No selection"), `--type text --watch CMD…`. Watch mode writes its PID to `$FAKE_CLIP_DIR/watch.pid`, then reads lines `data <file>` / `sensitive <file>` / `nil` from FIFO `$FAKE_CLIP_DIR/events` and runs CMD with `CLIPBOARD_STATE` and stdin set. `FAKE_WATCH_UNSUPPORTED=1` makes watch exit 1 with the data-control message |
| `tests/fakes/wl-copy` | stdin → `$FAKE_CLIP_DIR/content`; `--sensitive` also writes the hint to `types` |
| `tests/watch_capture.rs`, `tests/socket_protocol.rs`, `tests/daemon_lifecycle.rs` | Rust integration tests |
| `tests/e2e_daemon.sh` | New shell E2E (isolated), including the rcopy cases |

#### 2.4 Commits

| # | Commit | Failing test(s) first |
|---|--------|-----------------------|
| 2-1 | Extend fake wl-paste/wl-copy with types, watch FIFO and sensitive mode | `tests/watch_capture.rs::fake_clipboard_roundtrip` |
| 2-2 | Make Clipboard configurable and add list_types/paste_text | `clipboard::tests::list_types_parses_lines` (fake bins), `paste_text_no_selection_is_empty`, `copy_writes_content` |
| 2-3 | Resolve a private runtime dir with /tmp fallback | `daemon::tests::runtime_dir_prefers_xdg`, `fallback_dir_created_0700`, `rejects_fallback_with_wrong_mode_or_symlink` (ownership by another uid can't be simulated without root; the check is `symlink_metadata().uid() == getuid()` and is reviewed, not tested) |
| 2-4 | Add hidden watch hook that ingests stdin | `watch_capture::hook_ingests_stdin`, `hook_skips_sensitive_state_and_drains_stdin`, `hook_skips_when_types_include_password_hint`, `hook_rejects_non_utf8`, `hook_ignores_nil_state`, `hook_caps_read_at_max_entry_bytes` |
| 2-4b | Add `[capture]` and `[socket]` config sections with range checks | `config::tests::capture_and_socket_defaults`, `parses_capture_and_socket_sections`, `rejects_poll_interval_and_timeout_out_of_range` |
| 2-5 | Add capture supervisor state machine | `capture::tests::auto_falls_back_to_poll_on_fast_exit`, `restarts_with_backoff_after_late_exit`, `auto_polls_after_5_restarts_in_60s`, `watch_mode_gives_up_instead_of_polling`, `poll_mode_never_spawns_watch` |
| 2-6 | Run wl-paste --watch from the daemon with polling fallback | `watch_capture::daemon_watch_captures_copy`, `daemon_falls_back_to_poll_when_watch_unsupported`, `poll_skips_password_hint_types`, `poll_passes_mime_types_to_ingest` (assert `mime` column) |
| 2-7 | Expire old entries periodically in the daemon | `daemon::tests::tick_runs_expire_every_60_seconds` (injected clock) |
| 2-8 | Add socket protocol v1 framing | `protocol::tests::parses_push_header`, `rejects_header_over_256_bytes`, `rejects_len_over_limit`, `rejects_unknown_verb_and_version`, `rejects_nonzero_len_for_pull_and_ping`, `ok_response_roundtrip_with_meta`, `err_response_roundtrip` |
| 2-9 | Serve PUSH/PULL/PING on a 0600 Unix socket | `socket_protocol::socket_is_0600_in_runtime_dir`, `push_ingests_and_copies`, `push_ignored_pattern_copies_but_not_stored`, `pull_forbidden_by_default`, `pull_returns_clipboard_when_allowed`, `pull_refuses_sensitive_clipboard`, `ping_reports_capture_and_version` |
| 2-10 | Enforce socket timeouts and body limit | `socket_protocol::idle_client_gets_timeout` (`timeout_ms = 200`), `slow_client_hits_overall_deadline`, `second_client_served_after_first_times_out`, `oversize_push_rejected_without_reading_body` |
| 2-11 | Replace stale socket on start; never unlink a live one | `socket_protocol::stale_socket_replaced`, `second_daemon_leaves_live_socket`, `live_foreign_socket_is_left_alone` |
| 2-12 | Stop watch child and remove socket on shutdown | `daemon_lifecycle::sigterm_stops_watch_child_and_removes_socket` (PID from `watch.pid`), `sigkill_daemon_terminates_watch_child` (PDEATHSIG) |
| 2-13 | Remove TCP serve and rewrite rcopy for the socket | `daemon_lifecycle::serve_prints_migration_message_exits_2`; `e2e_daemon.sh`: rcopy via socat and via `nc -U` stores and copies, `OK 0 stored=0 reason=…` is reported on stderr with exit 0, unreachable socket exits 3 |
| 2-14 | Show socket and capture mode in status | `daemon_lifecycle::status_reports_socket_reachable_and_capture_mode` |
| 2-15 | Rewrite systemd unit for runtime-dir lock and socket | `tests/e2e_daemon.sh`: `systemd-analyze --user verify systemd/clipstack.service` (skip if unavailable) |
| 2-16 | Document daemon capture modes, socket, rcopy and systemd setup | — |

#### 2.5 Test plan

- **No real clipboard or daemon involved:**
  - Every daemon test goes through `TestEnv`: temp `XDG_RUNTIME_DIR`/`XDG_DATA_HOME`/`XDG_CONFIG_HOME`/`HOME`/`--storage-dir`, PATH with `tests/fakes` first, no `WAYLAND_DISPLAY`.
  - The real wl-copy/wl-paste are never reached because the fakes shadow them.
  - The lock and socket live in the temp runtime dir, so the user's running daemon (`/run/user/1000/clipstack.lock`) is never seen.
- **Socket tests** use a small `std::os::unix::net::UnixStream` client built on `protocol::write_request`/`read_response`.
- **Shell E2E** `tests/e2e_daemon.sh`, via `tests/lib.sh` isolation:
  - Start the daemon, `wl-copy` (fake) text, push a `data` event, assert it appears via `clipstack export`.
  - Send a `sensitive` event and assert nothing is stored.
  - Exercise the socket with `socat` and `nc -U`.
  - `kill`, then assert the socket is gone and the fake wl-paste PID is dead.
- **Manual, real compositor** (user-run, after merge): `clipstack kill && clipstack daemon` in a terminal; copy from a browser; copy a KeePassXC password and confirm it is not stored; `wl-copy --sensitive secret` is not stored.

#### 2.6 Docs

- README "Running the Daemon": capture modes, data-control requirement, fallback, new unit name and install steps, `WAYLAND_DISPLAY` import, sandbox notes.
- Remove the TCP "Remote Clipboard" text (replaced in Step 3) and `CB_PORT` from the env table.
- Architecture diagram: socket, hook, watch child.
- Troubleshooting: "daemon falls back to polling", "socket unavailable".
- FEATURE_PLAN "Additional Recommendations → Clipboard Backend Abstraction": partly addressed by the `Clipboard` struct.

#### 2.7 Risks and open questions

- **`wl-paste --type text --watch` on non-text offers** (images): verify whether wl-paste skips the command or runs it with empty stdin. The hook handles both, because empty input is skipped.
- **Compositor support:** Hyprland supports data-control; GNOME doesn't, which falls to polling. The `Auto` fallback covers this, and status shows the mode.
- **Race:** the hook's `list_types()` runs after wl-paste has read the offer. If the selection changed in between, the types could belong to the next offer. Low impact, because `CLIPBOARD_STATE=sensitive` comes from the same offer and is authoritative.
- **Self-capture:** PUSH → wl-copy → watch hook → `Bumped`. Harmless.
- **One process per copy:** about 5-10 ms of startup plus opening SQLite. Acceptable.
- **`unsafe` pre_exec** for PDEATHSIG. The alternative is no PDEATHSIG and accepting an orphaned wl-paste after `kill -9`.
- **systemd sandboxing in a user unit:** `ProtectSystem`, `ProtectHome`, `PrivateTmp` and `MemoryDenyWriteExecute` need unprivileged user namespaces for user services. Arch enables them; if `systemctl --user status` shows a namespace error, the README says which lines to drop. `systemd-analyze verify` does not catch this, so the manual check in 2.5 includes `systemctl --user restart clipstack && clipstack status`.

---

### Step 3: Command line (`main.rs`, `scripts/`)

> **Implementation status:** done (6ef5026..e31d08d): `cli.rs` split, exit codes, refs, `get`, `list`/`search --json`, `alias`, `rpaste`.

- [8] `list --json`, `get <n|id>`, `search <q> --json` (FTS5), stable exit codes.
- [9] `get @alias`, `alias <id> <name>`.
- [11] `rpaste` over SSH Unix-socket forwarding (rcopy landed in 2-13); README setup docs.

#### 3.1 Design

**Module layout:** `src/cli.rs` (clap types, `EntryRef`, `ExitCode`/`CliError`) and `src/commands.rs` (one fn per subcommand). `main.rs` shrinks to parse → load config → open storage → dispatch → map errors to exit codes.

```rust
pub enum EntryRef { Position(NonZeroUsize), Id(EntryId), Alias(String) }
impl FromStr for EntryRef { /* "3" | "id:42" | "@name" */ }
pub struct CliError { pub code: Exit, pub msg: String }
#[repr(u8)] pub enum Exit { NotFound = 1, Usage = 2, Storage = 4, Config = 5, Clipboard = 6 }
fn exit_code_for(err: &anyhow::Error) -> Exit; // first cause in the chain: CliError, ConfigError, ClipboardError, StorageError, rusqlite::Error
```

`Exit` has no `Ok` (success is `ExitCode::SUCCESS`) and no `Unavailable`: no clipstack subcommand needs the daemon (`get --copy` bumps the entry itself), so only `rcopy`/`rpaste` exit 3, and an unconstructed variant would be dead code. `StorageError::{NewerSchema, Corrupt}` and any `rusqlite::Error` (locked past the busy timeout, I/O) map to 4; `InvalidAlias` to 2; `NotFound`, `PinLimit` and `AliasTaken` to 1. An error with none of these types also exits 1, as every error did before Step 3. `ClipboardError` wraps the wl-clipboard error without changing its message, and a missing `wl-paste` is one too (6).

**Commands:**

| Command | Behaviour |
|---------|-----------|
| `list [-c N] [--json] [--pinned]` | Human output unchanged. `--json` prints a JSON array of `ClipEntry` + `"position"`. Empty history prints `[]` and exits 0. `--pinned` keeps each entry's position in the whole history, so `get N` still finds it |
| `get <REF> [--copy]` | Raw content to stdout, no added newline. `--copy` also runs wl-copy and bumps the entry itself via `ingest(Source::Copy)` (works without a daemon; the watch hook's later bump is a no-op). Missing entry → stderr `no entry for <REF>`, exit 1 |
| `search <QUERY>... [-c N] [--json]` | `Storage::search` over the words joined by spaces (so `search docker compose` needs no quotes). Human output in the same format as `list`; positions are those in the whole history. No match → exit 1 (and `[]` with `--json`) |
| `alias <REF> <NAME>` | `set_alias`, pinning if needed. Name rule `^[a-z0-9][a-z0-9_-]{0,31}$`. Conflict → exit 1 `alias @x already used by id:12`. Pin limit → exit 1 |
| `alias --remove <REF>` | Clears the alias and keeps the pin |

JSON timestamps are unix ms integers. No new dependency is needed for this.

**rcopy / rpaste** (POSIX sh, `scripts/`; rcopy is written in commit 2-13 to this spec, rpaste in 3-8):
- Socket path: `CLIPSTACK_SOCK`, default `$HOME/.clipstack.sock`.
- Transport preference: `socat - UNIX-CONNECT:$SOCK`, then `nc -U $SOCK` (OpenBSD), then `ncat -U $SOCK`; exit 3 if none is available.
- `rcopy`: `umask 077; tmp=$(mktemp)`, stdin → tmp, `n=$(wc -c <tmp)`, then `{ printf 'CLIPSTACK/1 PUSH %s\n' "$n"; cat "$tmp"; } | transport`. Parse the first response line; `OK` → exit 0 (print `reason=…` to stderr when `stored=0`), `ERR` → print the message, exit 1. Unreachable socket → exit 3. A trap removes tmp.
- `rpaste`: `[ -S "$SOCK" ]` or exit 3, then `printf 'CLIPSTACK/1 PULL 0\n' | transport | { IFS= read -r hdr; case $hdr in OK*) cat ;; ERR*) echo "rpaste: ${hdr#ERR }" >&2; exit 1 ;; "") exit 3 ;; *) exit 1 ;; esac; }`. An empty reply means no daemon behind the socket (a stale forward), so it exits 3 like rcopy. POSIX `read` doesn't over-read from a pipe.
- SSH setup (README):
  ```
  Host myserver
      RemoteForward /home/<remote-user>/.clipstack.sock /run/user/<local-uid>/clipstack.sock
  ```
  The remote sshd needs `StreamLocalBindUnlink yes` to replace stale sockets.

#### 3.2 Migration and compatibility

- rcopy already uses the Unix socket (Step 2). `rpaste` is new and needs `allow_pull = true` on the local side.
- `list` human output is unchanged.

#### 3.3 File-by-file

| File | Change |
|------|--------|
| `src/cli.rs`, `src/commands.rs` | New (moved from `main.rs`) |
| `src/main.rs` | Thin dispatcher + exit-code mapping |
| `scripts/rpaste` | New |
| `tests/cli_scripting.rs` | New |
| `tests/e2e_remote.sh` | New (isolated daemon; `CLIPSTACK_SOCK` pointed at the temp runtime socket, which stands in for the ssh forward). The rcopy cases move here from `e2e_daemon.sh` |

#### 3.4 Commits

| # | Commit | Failing test(s) first |
|---|--------|-----------------------|
| 3-1 | Move clap definitions and handlers out of main.rs | Existing suite (pure refactor) |
| 3-2 | Map errors to stable exit codes | `cli::tests::exit_code_for_storage_config_clipboard_errors`, `cli_scripting::newer_schema_exits_4`, `bad_config_exits_5` |
| 3-3 | Parse entry references (N, id:ID, @alias) | `cli::tests::parses_position_id_alias`, `rejects_zero_position`, `rejects_invalid_alias_chars` |
| 3-4 | Add `get` (remove the `expect(dead_code)` on `Storage::get`/`nth`/`by_alias`) | `cli_scripting::get_1_prints_newest_without_newline`, `get_by_id`, `get_by_alias`, `get_missing_exits_1`, `get_copy_puts_content_on_fake_clipboard` |
| 3-5 | Add `list --json` | `cli_scripting::list_json_fields_and_positions`, `list_json_respects_count`, `list_json_empty_array` |
| 3-6 | Add `search` with FTS5 and `--json` | `cli_scripting::search_finds_deep_match`, `search_json_shape`, `search_no_match_exits_1` |
| 3-7 | Add `alias` and `alias --remove` | `cli_scripting::alias_pins_and_names`, `alias_conflict_exits_1`, `alias_remove_keeps_pin`, `alias_at_pin_limit_exits_1` |
| 3-8 | Add rpaste and move the remote E2E cases into e2e_remote.sh | `e2e_remote.sh`: rpaste with `allow_pull=false` exits 1 with message; with `allow_pull=true` prints the fake clipboard content byte-for-byte (including no trailing newline); rcopy cases still pass |
| 3-9 | Document scripting mode, exit codes and SSH socket forwarding | — |

#### 3.5 Test plan

- **Unit tests:** parsing and exit-code mapping.
- **Rust integration:** all CLI behaviour through TestEnv (fake wl-copy for `--copy`).
- **Shell E2E:** `e2e_remote.sh` covers both transports available locally (socat, OpenBSD nc).
- **Real SSH** (manual, optional): `ssh -o RemoteForward=... localhost` only if the user runs sshd locally; otherwise against a real host.

#### 3.6 Docs

- README: commands table (`get`, `search`, `alias`, `list --json`), new "Scripting" section (JSON schema, exit-code table, examples like `clipstack get @sig | ...`).
- Rewrite "Remote Clipboard (SSH)": socket forwarding, rcopy/rpaste install, `allow_pull` security note, `StreamLocalBindUnlink`, troubleshooting.
- Environment variables: remove `CB_PORT`, add `CLIPSTACK_SOCK` (remote side).

#### 3.7 Risks and open questions

- Pull is a deliberate exfiltration path: anyone with remote root or the same remote uid can read the local clipboard while the forward is up. It defaults to off (Q4).
- The remote socket path must be absolute and known in advance. A stale socket blocks forwarding without `StreamLocalBindUnlink`.
- `nc` variants differ. The length-prefixed protocol avoids depending on `-N`/`-q`. BusyBox `nc` lacks `-U`, so socat is recommended.

---

### Step 4: Picker (`picker/`)

> **Implementation status:** done (5843009..def2d59, docs in the commit after): `picker/` modules, `map_key`, selection by id, debounced FTS search, wrapping, age column, `w`/`n`, `P` alias, `e` edit, `pick --paste`.

- Restructure first (FEATURE_PLAN §0.1, §0.4): split view state from data; debounce search; search queries FTS5.
- [15] Wrap to pane width, [16] fixed-width time column, [17] whitespace toggle.
- [9] Name a pin with `P`; show aliases in the list.
- [10] `e` edits in `$EDITOR`.
- [7] Paste into previous window via `wtype` (last; external integration).

#### 4.1 Design

**Module layout:**

```
src/picker/mod.rs     Picker::run, terminal setup/teardown, suspend/resume helpers, pick() (copying and pasting are in commands::pick)
src/picker/data.rs    PickerData { storage: Storage, entries: Vec<ClipEntry>, content_cache: Option<(EntryId, String)> }
src/picker/state.rs   ViewState { mode, focus, selection: Option<EntryId>, query, pending_query_at, scroll, preview_scroll,
                      show_whitespace, show_line_numbers, prompt: Option<Prompt>, status }
src/picker/search.rs  SearchState; run_search(&data, query) -> Vec<Hit> (preview fuzzy + Storage::search); debounce
src/picker/keys.rs    fn map_key(&ViewState, KeyEvent) -> Action   (pure)
src/picker/render.rs  all draw fns; takes &ViewState + &PickerData
src/picker/text.rs    wrap_to_width(), visualize_whitespace(), number_lines(), format_compact_age()
src/picker/actions.rs edit_in_editor(), paste_command()
```

**Selection and search:**
- Selection is stored by `EntryId` (FEATURE_PLAN Feature 4 invariant) and resolved to an index at render time.
- Search debounce: typing sets `pending_query_at = now`. The event loop polls with timeout `min(100 ms, remaining)`, and the search runs once 50 ms have passed since the last keystroke.
- FTS runs synchronously, because it takes under 20 ms on capped storage. No worker thread; this replaces FEATURE_PLAN §0.2.

**[15] Wrap to pane width:**
- `wrap_to_width(text, width) -> Vec<String>` uses `unicode-width = "0.2"` display widths (CJK = 2), the same version ratatui 0.29 uses.
- It replaces the hard-coded 80 and is cached by `(entry_id, width, show_ws)`, recomputed when the frame width changes. Tabs expand to stops of 4 columns; a character wider than the pane gets a row of its own.
- List focus shows the top of the same wrapped rows that preview focus scrolls, so `[+N lines]` in the title counts wrapped rows.

**[16] Compact time:** `format_compact_age(ts_ms, now_ms) -> String` produces exactly 4 columns, right-aligned: `  5s`, ` 12m`, `  3h`, ` 40d`, `999d`, capping at `999d`. The list uses `{:>4}`. `list` CLI output keeps the "5m ago" style.

**[17] Preview toggles:**
- Key bindings live in preview focus only, so `w` and `n` stay free in list focus: `w` toggles visible whitespace (`·` space, `→` tab, `␍` CR, `¶` end of line) and `n` toggles a line-number gutter (`{:>4} │`).
- The list-focus preview pane follows the same flags. `¶` marks only a newline that is there (not the end of content without one), and wrapped rows get a blank gutter (`     │ `).

**[9] Aliases:**
- `P` opens `Prompt::Alias` in the search-box area.
- Enter calls `set_alias`, which pins if needed. Validation and pin-limit errors appear in the status line. Esc cancels.
- The list shows `@name` in accent colour where the preview text starts (after the age and size columns, so they stay aligned).
- The prompt opens with the entry's current alias; an empty input removes it (the pin stays). A refused name keeps the prompt open.
- Unpinning with `p` warns `Unpinned (alias @x removed)`.

**[10] Edit in `$EDITOR`:**
- `actions::edit_in_editor(content, editor: &str, dir: &Path, id) -> Result<Option<String>>`:
  1. Write content to `<runtime>/clipstack-edit-<pid>-<id>.txt` (`0600`, not `/tmp`).
  2. Run `sh -c "$EDITOR \"$1\"" sh <file>` (`$VISUAL`, then `$EDITOR`, then `vi`).
  3. A non-zero exit returns `None`. Otherwise read the file back, remove it, and return `None` if the content is unchanged or empty.
- The picker suspends the TUI (leave alt screen, disable raw mode), runs the editor, then resumes.
- If there is new content: `ingest(Source::Edit, &[])` and return it as the selection (copy and exit, which is what "before copying" means). Text the ingest policy skips is still copied, with the reason on stderr.
- A final newline the editor adds to content that had none (vim's `fixeol`) is dropped, so opening and quitting does not count as a change. `edit_in_editor` takes the directory and entry id too, so tests use a temp dir; the picker passes `daemon::runtime_dir()`. The file is opened `create_new`, so a planted file is never reused.

**[7] Paste into the previous window:**
- `clipstack pick --paste`, or config `[picker] paste_after_pick = true` with `paste_keys = "shift+Insert"` (Shift+Insert pastes in terminals and most GUI apps).
- After `wl-copy` succeeds, spawn `sh -c 'sleep 0.15; exec wtype -M shift -k Insert -m shift'` detached: `process_group(0)` (std `CommandExt`), stdin/stdout/stderr null, never waited on. Without that, the terminal closing with the picker sends it SIGHUP before the sleep ends. `paste_command(keys) -> Vec<String>` (pure) builds the argv. The delay lets the picker window close and focus return.
- If wtype is missing, print a status message and still exit 0.
- As built: `paste_command(keys) -> Result<Vec<String>, String>` accepts modifiers `shift`, `ctrl`/`control`, `alt`, `altgr`, `super`/`logo`/`win` and a key `Insert`, `Return`/`Enter` or one ASCII letter or digit; anything else (and a repeated modifier) is a config error. The helper is `sh -c 'sleep 0.15; exec "$@"' sh <wtype path> <args>`, with wtype found on `PATH` by `util::find_in_path` (moved from commands.rs). `commands::pick` copies and pastes; `picker::pick` only returns the choice. Debug builds read `CLIPSTACK_TEST_PICK=REF` instead of running the TUI (no daemon autostart), which the `cli_scripting` pick tests use with a fake `tests/fakes/wtype` that only logs its argv.

**Config additions:** a `[picker]` section with `paste_after_pick` (default false), `paste_keys` (default "shift+Insert") and `search_debounce_ms` (default 50).

#### 4.2 Migration and compatibility

- No storage changes.
- New key bindings: `P` and `e` in list focus, `w` and `n` in preview focus.
- Per the Q10 default, `/` is required to enter search; bare letters no longer start a filter. README's picker tables change accordingly. If the user picks the alternative, only the `map_key` table and those README rows change.
- Delete becomes `dd` (vim): a single `d` only arms it, and any other key disarms it. With `d` alone still deleting, typing "docker" in normal mode would still delete on its first letter, which is what `typing_docker_in_normal_mode_deletes_nothing` rules out. `Ctrl+C` quits from every mode (before, it typed a `c`), and Ctrl chords never act as their bare letter (Ctrl+P no longer pins).

#### 4.3 File-by-file

| File | Change |
|------|--------|
| `src/picker.rs` → `src/picker/*.rs` | As designed |
| `Cargo.toml` | `unicode-width = "0.2"` |
| `src/config.rs` | `[picker]` |
| `src/main.rs`/`src/cli.rs` | `pick --paste` |

#### 4.4 Commits

| # | Commit | Failing test(s) first |
|---|--------|-----------------------|
| 4-1 | Split picker into data, view state, render and key modules | Existing picker tests (moved, unchanged assertions) |
| 4-2 | Map keys to actions as a pure function; `/` required to search (Q10) | `keys::tests::j_moves_down_in_list_focus`, `slash_enters_search_and_letters_are_commands`, `typing_docker_in_normal_mode_deletes_nothing`, `q_in_preview_returns_to_list` |
| 4-3 | Track selection by entry id across filter/pin changes | `state::tests::selection_survives_unpin` (FEATURE_PLAN Feature 4), `selection_none_when_filter_empty` |
| 4-4 | Debounce search and query FTS5 | `search::tests::rapid_typing_runs_one_search`, `search_runs_after_debounce_elapsed`, `deep_match_marked_content` |
| 4-5 | Wrap preview to actual pane width by display width | `text::tests::wraps_at_given_width`, `cjk_counts_double_width`, `render::tests::preview_rewraps_on_resize` (ratatui `TestBackend` 60 vs 120 cols) |
| 4-6 | Fixed-width compact time column | `text::tests::compact_age_units_and_width_4`, `caps_at_999d`, `future_clamps_to_0s`, `render::tests::list_time_column_aligned` |
| 4-7 | Toggle visible whitespace and line numbers in preview | `text::tests::visualize_whitespace_symbols`, `number_lines_gutter`, `keys::tests::w_and_n_only_bound_in_preview` |
| 4-8 | Name a pin with P and show aliases in list | `picker::tests::alias_prompt_sets_alias_and_pins`, `invalid_alias_shows_error`, `render::tests::list_shows_alias` |
| 4-9 | Edit entry in $EDITOR before copying | `actions::tests::edit_returns_modified_content` (EDITOR=`sh -c 'printf edited > "$1"' --`), `edit_unchanged_returns_none`, `editor_failure_returns_none`, `edit_temp_file_is_0600_in_runtime_dir_and_removed` |
| 4-10 | Paste into previous window via wtype | `actions::tests::paste_command_for_shift_insert`, `paste_command_rejects_unknown_key`, `cli_scripting::pick_paste_without_wtype_still_copies` (non-TTY path via test hook) |
| 4-11 | Document picker keys, aliases, edit and paste | — |

#### 4.5 Test plan

- Pure functions (`keys`, `text`, `search`, `actions`) are unit-tested.
- Rendering uses `ratatui::backend::TestBackend` buffers at fixed sizes, asserting cell contents.
- No real TTY, editor or wtype is used: the editor is a sh one-liner via `EDITOR`, and wtype is asserted at the argv level.
- E2E: the existing preview and search shell suites must still pass.

#### 4.6 Docs

- README Picker key tables (`P`, `e`, `w`, `n`), visual features (compact time, aliases, wrapping).
- Hyprland/Sway bindings with `pick --paste`, and `wtype` in requirements as optional.
- FEATURE_PLAN: mark §0.1 and §0.4 done (Step 4).
- Feature 2/3/4 notes: refer to the new module paths.

#### 4.7 Risks and open questions

- Paste timing and focus return depend on the compositor and window rules. 150 ms may need to be configurable.
- Shift+Insert pastes PRIMARY instead of CLIPBOARD in some apps (xterm). `paste_keys` makes this configurable.
- Key table: Q10 is decided by the user: `/` starts a search; bare letters are commands.

### Step 5: Images, entry commands and the omarchy-shell overlay

> **Implementation status:** in progress.

Goal: a Maccy-style clipboard overlay inside omarchy-shell (Quickshell), backed by
the `clipstack` CLI, with image history. Design: concept C "Reader" (list on the
left, full preview on the right), approved on the design canvas
(https://claude.ai/artifact/6ujZ8mxWRvaSXSLKuQH4B3, boards C1-C4). The terminal
picker stays as the fallback for non-omarchy desktops and SSH.

Flow (Maccy): Super+Ctrl+V opens the overlay with the search field focused; typing
filters at once; Up/Down (and Ctrl+J/K) move; Enter pastes into the previous
window; Shift+Enter copies only; Esc clears the search, a second Esc closes.
Pinned entries are listed first. Image entries are ordinary rows with a small
thumbnail; the preview shows the full image fitted to the pane.

#### 5.1 Storage schema v2 (images)

Migration v1 -> v2 (`PRAGMA user_version = 2`) adds these columns:

```sql
kind   TEXT    NOT NULL DEFAULT 'text' CHECK (kind IN ('text', 'image')),
width  INTEGER,   -- image only
height INTEGER,   -- image only
image  BLOB       -- PNG bytes; NULL for text
-- plus a CHECK that image/width/height are set exactly when kind = 'image'
```

*Amended during implementation:* the table is **rebuilt** (create `entries_v2`,
copy the rows with their ids, drop, rename, recreate the indexes and FTS
triggers, carry the AUTOINCREMENT counter over) instead of `ALTER TABLE ADD
COLUMN`. Added columns go after `content`, so every list read of `kind` would
walk the overflow pages of large text entries, which the v1 column order was
designed to avoid. v2 keeps the small columns first and ends with `content`,
then `image`. The FTS index is keyed by rowid and stays valid. A new store is
created as v1 and migrated, so the migration runs everywhere.

- An image entry has `content = ''` (nothing indexed by FTS), `mime = 'image/png'`,
  `preview = 'Image · WxH'`, `size` = PNG byte length.
- Hash: `sha256(b"image\0" || bytes)`, so an image never collides with a text entry.
  Re-copying the same image bumps it like text.
- Only `image/png` is stored. Width and height come from the PNG IHDR chunk; bytes
  without a valid PNG signature and IHDR are rejected (`SkipReason::NotPng`).
- Caps: new `[storage] max_image_bytes` (default 8 MiB, 1..=64 MiB). `max_total_bytes` and
  `max_entries` count image entries like text. Retention applies to them too.
  *Amended:* an image over the cap is `SkipReason::ImageTooLarge { size, limit }`
  (code `too_large`, message names `max_image_bytes`) rather than reusing
  `TooLarge`, whose message names `max_entry_bytes`. The effective image cap is
  `min(max_image_bytes, max_total_bytes)`, so an image is never saved only to be
  pruned by the total cap in the same transaction.
- `x-kde-passwordManagerHint` skips images as well. Text ignore patterns do not
  apply to images.
- `ClipEntry` gains `kind: Kind` (`Text | Image`, serialised lowercase),
  `width: Option<u32>`, `height: Option<u32>`.
- New API: `Storage::ingest_image(&self, png: &[u8], source, mime_types) -> Result<IngestOutcome>`
  and `Storage::load_image(&self, id) -> Result<Vec<u8>>`. `load_content` on an image
  entry is an error.
- `export` writes image rows with `"kind":"image"` and the bytes as standard base64 in
  `"image_base64"`. `check` verifies image rows (signature, dimensions, size match).
- A store opened by an older binary (user_version 2 > 1) already fails cleanly with
  the "newer schema" storage error (exit 4); no change needed.

#### 5.2 Capture and clipboard

- `Clipboard` gains `paste_bytes(mime) -> Result<Vec<u8>>` (`wl-paste --no-newline --type MIME`)
  and `copy_bytes(&[u8], mime)` (`wl-copy --type MIME`).
- Daemon (watch and poll): after `list_types()`, if no text type is offered
  (`text/plain*`, `UTF8_STRING`, `STRING`, `TEXT`) and `image/png` is, fetch the PNG
  and call `ingest_image`. Text keeps priority whenever both are offered.
- `tests/fakes` wl-paste/wl-copy learn `--type image/png` with binary payloads.

#### 5.3 CLI

| Command | Behaviour |
|---------|-----------|
| `pin <REF>` | Pin; no-op if already pinned. Exit 1 if not found. |
| `unpin <REF>` | Unpin; clears the alias (prints a note when one was removed). |
| `delete <REF>` | Delete one entry; also removes its cached image file (below). |
| `edit <REF>` | Open the text in `$VISUAL`/`$EDITOR` (same 0600 temp file as the picker's `e`); a changed result is saved and copied. Exit 1 for an image. |
| `get <REF> --paste` | Implies `--copy`, then runs the detached `wtype` paste exactly like `pick --paste` (`[picker] paste_keys`). |
| `get <REF>` on an image | Writes the PNG bytes to stdout, refusing a TTY (exit 2 with a hint). `--copy` uses `copy_bytes(.., "image/png")`. |
| `get <REF> --image-path` | Image only: makes sure `<runtime>/clipstack-images/<id>.png` exists (dir 0700, file 0600, written atomically) and prints its path. Used by the overlay for thumbnails and the preview. |
| `list --json`, `search --json` | Each object gains `kind`, `width`, `height` (null for text). |

`clear` and retention also remove cached image files whose entry is gone (the
runtime dir is tmpfs, so leftovers vanish at logout anyway).

#### 5.4 Overlay plugin (`omarchy/clipstack/`)

- Manifest: id `solsystemlabs.clipstack`, kind `overlay`, `keepLoaded: true`, entry
  point `Overlay.qml`. Validated with `omarchy plugin validate`.
- It never captures anything: all data comes from the CLI through
  `Quickshell.Io.Process`. `clipstack list --json` on open; `clipstack search <q> --json`
  debounced 60 ms; `clipstack get id:N` for the selected text entry (preview shows
  at most the first 2000 lines); `get id:N --image-path` for images.
- Actions: Enter `get id:N --paste` (overlay closes first); Shift+Enter
  `get id:N --copy`; Ctrl+P `pin`/`unpin`; Ctrl+A inline alias prompt (`alias`,
  validates `[a-z0-9_-]{1,32}` live); Ctrl+E `omarchy-launch-tui --app-id=org.omarchy.clipstack clipstack edit id:N`
  (overlay closes); Ctrl+Backspace `delete`. Every action is also a button with its
  shortcut shown; mouse click selects, double-click pastes.
- Groups: PINNED, TODAY, YESTERDAY, EARLIER; while searching, one MATCHES group
  with the query highlighted in each row. Footer: `N clips · P pinned`.
- Look: the C palette (accent `#A9B4FF`), Noto Sans / JetBrains Mono, panel
  1040x620 centred over a dimmed scrim. No `Commons.Color.menu` tokens.
- Pure logic (grouping, relative age, highlight spans, key map, alias validation)
  lives in `Logic.js` with node unit tests (`omarchy/clipstack/tests/*.test.mjs`,
  run by `scripts/test-overlay.sh`); QML is checked with `qmllint`.
- Install: `scripts/install-omarchy-plugin.sh` symlinks the folder into
  `~/.config/omarchy/plugins/solsystemlabs.clipstack` and runs
  `omarchy plugin enable`.

#### 5.5 Desktop integration (on this machine, after 5.1-5.4 land)

- Super+Ctrl+V: `omarchy-shell shell toggle solsystemlabs.clipstack '{}'`; the TUI
  window rule stays for `clipstack edit`.
- `omarchy plugin disable omarchy.clipboard`, so only ClipStack captures history.
  Its old `~/.local/state/omarchy/clipboard-history.json` (mode 644) is the user's to
  delete.

#### 5.6 Tests and docs

- TDD as in every step: migration v1->v2 on a populated v1 store, image ingest and
  bump, NotPng/too-large/sensitive skips, total cap with images, export/check of
  images, daemon image capture through the fakes, every new CLI command and its
  exit codes, `--image-path` modes and atomic write.
- README: images, the new commands, the overlay (install, keys), the omarchy
  binding. FEATURE_PLAN: note image support as done.

---

## Open questions (decided in review)

Implementation proceeds through Steps 0-3 on the decisions below. Q2 and Q10 were decided by the user.

| # | Question | Decision | Reason |
|---|----------|----------|--------|
| Q1 | `storage_version` as a schema column? | **`PRAGMA user_version` plus `application_id`.** | A per-row version adds nothing to a single-table schema; future formats use `ALTER TABLE` under a new `user_version` |
| Q2 | Retention default. The real store holds about 100 entries spanning about 10 months. | **Decided by the user: `retention_days = 0` (off) by default**; README recommends `30` or `90` under "Configuration file", and the one-time import notice adds `N entries are older than <retention> days and will expire at the next start` only when retention is enabled. | `max_entries` (100) already bounds the history, so retention is an opt-in privacy setting, not a size control. Any non-zero default deletes most of the user's history at the first `open` after import, and reads happen before the user has seen the notice |
| Q3 | TCP `serve`: remove or keep opt-in? | **Remove (hidden stub, exit 2) and rewrite rcopy in the same commit (2-13).** | A token-protected TCP listener is more code than the socket it replaces; rewriting rcopy with the removal leaves no broken window |
| Q4 | `allow_pull` default. | **`false`.** rpaste prints how to enable it. | Pull is an exfiltration path for anyone with the remote uid or root |
| Q5 | Legacy backup dir lifetime (plaintext). | **Keep it; `status` warns while it exists; README says to delete it after checking. No `--purge-legacy`. Amended after review: `clear` also removes the `legacy-backup-*` dirs (regular files inside, then the dir; symlinks never followed) and prints how many.** | Deleting user data is the user's call, and `clear` is that call: a "cleared" history must not survive as plaintext files next to the database |
| Q6 | Legacy import skips filters and caps. | **Reversed after review: the import applies the ingest policy** (`classify`: empty, oversize, ignore and built-in patterns) and reports a skipped count; of the caps only max_entries pruning applies at import. No "check reports matching entries". | Nothing is lost: every original, skipped or not, stays in the legacy backup dir. Importing secrets the user configured to never save would put them back into the live, searchable history |
| Q7 | `max_entries` in the DB, other limits in config. | **Keep.** | Preserves CLI/env/saved semantics and labels; the watch hook inherits the daemon's env so both agree. Revisit after Step 3 |
| Q8 | Step 0 additions beyond the skeleton. | **Approved: commits 0-7 (no `pkill`) and 0-8 (`current_exe()`).** | Both are isolation fixes needed before any suite is run against the new code |
| Q9 | Empty content no longer stored. | **Accept.** | An empty entry is never useful to pick |
| Q10 | Normal-mode letters start a search, but `d` deletes, `u` undoes, `p` pins; `P`/`e` make it worse. | **Decided by the user: `/` is required to search; bare letters are commands (vim-like, matching the existing `j`/`k`/`g`/`G`).** Step 4 implements `map_key` with this table; because it is one pure function, switching to the alternative (letters search, commands move to `D`/`U`/`Ctrl+P`-style chords) is a table change in commit 4-2. | Today typing "docker" from normal mode deletes an entry on the `d`; this is a latent data-loss bug, not a style choice |
| Q11 | Built-in secret patterns on by default. | **On; `builtin_secret_patterns = false` opts out; skips logged without content.** | Security goal; the patterns are whole-content matches so prose is unaffected |
| Q12 | CRLF→LF at ingest. | **Reject.** | The clipboard must round-trip exactly |
| Q13 | `clear` deletes pinned entries. | **Keep.** | Current behaviour; `clear --keep-pinned` can be added later without a format change |
| Q-misc | `clipstack import <jsonl>`; `copy --sensitive`. | **Out of scope.** | Neither is needed by Steps 0-4; `export` is for inspection and backup |

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
- Daemon, socket and CLI integration tests run only through `tests/common/mod.rs::TestEnv` (Rust) or `tests/lib.sh::isolate_env` (shell): temp `XDG_RUNTIME_DIR`, `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `HOME`, `--storage-dir`, and `tests/fakes` first on PATH. Never `pkill`; kill only recorded PIDs.

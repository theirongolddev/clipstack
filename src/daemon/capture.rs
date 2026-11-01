//! Clipboard capture. The daemon runs `wl-paste --watch <clipstack>
//! __watch-hook`, so wl-paste starts the hook once per new selection; the
//! `Supervisor` restarts the watcher or falls back to the `Poller`.

use crate::clipboard::{self, Clipboard};
use crate::storage::{IngestOutcome, Kind, Source, Storage};
use crate::util;
use anyhow::Result;
use serde::Deserialize;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// `[capture] mode`. `auto` watches and falls back to polling when the
/// compositor lacks the data-control protocol; `watch` never polls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureMode {
    #[default]
    Auto,
    Watch,
    Poll,
}

/// Handle one selection from `wl-paste --watch` (no `--type`): `state` is
/// `CLIPBOARD_STATE` and `input` the selection. wl-paste 2.3 passes a text
/// type whenever one is offered (the same choice as `--type text`), else the
/// first offered type; with `--type text` it would not run the hook at all
/// for an image-only offer. So when `clipboard::choose_offer` picks the
/// image (a PNG and no plain-text type), the PNG is fetched and saved as an
/// image whatever was on stdin; other binary offers are skipped. `nil` is an empty clipboard. Never fails on bad input
/// (it is logged, without content), so the caller can always exit 0 and
/// wl-paste keeps running; stdin is always read to the end, so the source
/// client never sees EPIPE.
pub fn run_hook(
    storage: &Storage,
    clipboard: &Clipboard,
    state: Option<&str>,
    input: &mut dyn Read,
) -> Result<()> {
    match state {
        None | Some("data") => {}
        Some("nil" | "clear") => {
            drain(input)?;
            return Ok(());
        }
        Some("sensitive") => {
            drain(input)?;
            eprintln!("[watch] skipped sensitive");
            return Ok(());
        }
        Some(other) => {
            drain(input)?;
            eprintln!("[watch] skipped: unknown CLIPBOARD_STATE {other:?}");
            return Ok(());
        }
    }

    // Read at most one byte past the cap, so a huge copy is never buffered
    let limit = storage.policy().max_entry_bytes();
    let mut buf = Vec::new();
    input.take(limit as u64 + 1).read_to_end(&mut buf)?;
    let too_large = buf.len() > limit;
    let rest = if too_large { drain(input)? } else { 0 };

    let types = clipboard.list_types().unwrap_or_else(|e| {
        eprintln!("[watch] could not list offer types: {e:#}");
        Vec::new()
    });
    let offer = clipboard::choose_offer(&types);
    if offer == clipboard::Offer::Image {
        // stdin held whichever type wl-paste picked; ask for the PNG itself
        let png = clipboard.paste_bytes(crate::png::MIME)?;
        if !png.is_empty() {
            log_outcome("watch", storage, &storage.ingest_image(&png, Source::Watch, &types)?);
        }
        return Ok(());
    }
    if too_large {
        let size = buf.len() as u64 + rest;
        eprintln!("[watch] skipped: too large ({size} bytes; max_entry_bytes is {limit})");
        return Ok(());
    }
    if !types.is_empty() && offer == clipboard::Offer::Neither {
        eprintln!("[watch] skipped: no text or PNG type offered");
        return Ok(());
    }
    let Ok(content) = String::from_utf8(buf) else {
        eprintln!("[watch] skipped: not UTF-8 text");
        return Ok(());
    };
    log_outcome("watch", storage, &storage.ingest(&content, Source::Watch, &types)?);
    Ok(())
}

/// One log line per ingest, never with the content.
pub fn log_outcome(tag: &str, storage: &Storage, outcome: &IngestOutcome) {
    match outcome {
        IngestOutcome::Inserted(entry) => {
            eprintln!("[{tag}] saved {}id:{} ({} bytes)", image_label(entry.kind), entry.id, entry.size);
        }
        IngestOutcome::Bumped(entry) => {
            eprintln!("[{tag}] bumped {}id:{} ({} bytes)", image_label(entry.kind), entry.id, entry.size);
        }
        IngestOutcome::Skipped(reason) => {
            eprintln!("[{tag}] skipped: {}", storage.policy().describe_skip(reason));
        }
    }
}

/// `"image "` for an image entry, so a log line says what was saved.
fn image_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Image => "image ",
        Kind::Text => "",
    }
}

/// Read and discard the rest of `input`; returns how many bytes that was.
fn drain(input: &mut dyn Read) -> io::Result<u64> {
    io::copy(input, &mut io::sink())
}

/// A first watch child that exits sooner than this means watching can't
/// work here (typically no data-control protocol).
const FAST_EXIT: Duration = Duration::from_secs(2);
/// Restarts are counted over this window...
const RESTART_WINDOW: Duration = Duration::from_secs(60);
/// ...and this many in it means the watcher keeps dying: stop restarting.
const MAX_RESTARTS: usize = 5;
/// A child that ran this long resets the backoff.
const STABLE_RUN: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// What the daemon tells the supervisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A watch child was started
    Spawned(Instant),
    /// The watch child exited (or could not be started)
    Exited { at: Instant, success: bool, stderr_tail: String },
    /// Time passed
    Tick(Instant),
}

/// What the daemon should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    SpawnWatch,
    /// Poll from now on (final)
    StartPolling { reason: String },
    Wait,
    /// Capture is impossible in `watch` mode: the daemon exits with this error
    GiveUp(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupState {
    Start,
    /// SpawnWatch was returned; waiting for Spawned or Exited
    Spawning,
    Running { since: Instant },
    Backoff { until: Instant },
    Polling,
    GaveUp,
}

/// Decides when to (re)start `wl-paste --watch` and when to fall back to
/// polling. Pure: the daemon feeds it events with timestamps.
///
/// - `Poll` never spawns.
/// - If the first watch child exits within 2 s, `Auto` polls and `Watch`
///   gives up.
/// - A later exit restarts after 1 s, 2 s, 4 s ... (max 30 s; reset by a
///   child that ran 60 s). After 5 restarts in 60 s, `Auto` polls and `Watch`
///   gives up.
#[derive(Debug, Clone)]
pub struct Supervisor {
    mode: CaptureMode,
    restarts: VecDeque<Instant>,
    state: SupState,
    spawns: u32,
    consecutive: u32,
}

impl Supervisor {
    pub fn new(mode: CaptureMode) -> Self {
        Self { mode, restarts: VecDeque::new(), state: SupState::Start, spawns: 0, consecutive: 0 }
    }

    pub fn on(&mut self, event: Event) -> Action {
        match (event, self.state) {
            (Event::Tick(_), SupState::Start) => {
                if self.mode == CaptureMode::Poll {
                    self.state = SupState::Polling;
                    Action::StartPolling { reason: "capture.mode = \"poll\"".to_string() }
                } else {
                    self.state = SupState::Spawning;
                    Action::SpawnWatch
                }
            }
            (Event::Tick(now), SupState::Backoff { until }) if now >= until => {
                self.state = SupState::Spawning;
                Action::SpawnWatch
            }
            (Event::Spawned(at), SupState::Spawning) => {
                self.spawns += 1;
                self.state = SupState::Running { since: at };
                Action::Wait
            }
            (Event::Exited { at, success, stderr_tail }, SupState::Spawning | SupState::Running { .. }) => {
                let since = match self.state {
                    SupState::Running { since } => since,
                    _ => at, // never started
                };
                self.on_exit(at, at.saturating_duration_since(since), success, &stderr_tail)
            }
            _ => Action::Wait,
        }
    }

    fn on_exit(&mut self, at: Instant, ran: Duration, success: bool, stderr_tail: &str) -> Action {
        let why = describe_exit(success, stderr_tail);
        if self.spawns <= 1 && ran < FAST_EXIT {
            return self.fall_back(format!("wl-paste --watch exited at once ({why})"));
        }
        if ran >= STABLE_RUN {
            self.consecutive = 0;
        }
        while self.restarts.front().is_some_and(|&t| at.saturating_duration_since(t) > RESTART_WINDOW) {
            self.restarts.pop_front();
        }
        if self.restarts.len() >= MAX_RESTARTS {
            return self.fall_back(format!(
                "wl-paste --watch was restarted {MAX_RESTARTS} times in {} s and exited again ({why})",
                RESTART_WINDOW.as_secs()
            ));
        }
        self.restarts.push_back(at);
        let delay = backoff(self.consecutive);
        self.consecutive += 1;
        eprintln!("[capture] wl-paste --watch exited ({why}); restarting in {} s", delay.as_secs());
        self.state = SupState::Backoff { until: at + delay };
        Action::Wait
    }

    fn fall_back(&mut self, reason: String) -> Action {
        if self.mode == CaptureMode::Watch {
            self.state = SupState::GaveUp;
            Action::GiveUp(format!("{reason}; capture.mode = \"watch\" does not fall back to polling"))
        } else {
            self.state = SupState::Polling;
            Action::StartPolling { reason }
        }
    }

    /// How the daemon currently captures, for `PING`: "watch", "poll" or "none".
    pub fn capturing(&self) -> &'static str {
        match self.state {
            _ if self.mode == CaptureMode::Poll => "poll",
            SupState::Polling => "poll",
            SupState::GaveUp => "none",
            _ => "watch",
        }
    }
}

/// Delay before restart number `n` (0-based): 1 s doubling, at most 30 s.
fn backoff(n: u32) -> Duration {
    Duration::from_secs(1u64.checked_shl(n).unwrap_or(u64::MAX)).min(MAX_BACKOFF)
}

/// The last line of the child's stderr, else its exit status.
fn describe_exit(success: bool, stderr_tail: &str) -> String {
    match stderr_tail.lines().rev().map(str::trim).find(|l| !l.is_empty()) {
        Some(line) => line.to_string(),
        None if success => "exit status 0".to_string(),
        None => "failed".to_string(),
    }
}

/// Lines of the watcher's stderr kept for the fallback reason.
const STDERR_TAIL_LINES: usize = 5;

/// A running `wl-paste --watch` child. Its stderr (which the hooks share) is
/// forwarded to ours line by line, and the last lines are kept so an early
/// exit can say why. Dropping it kills and reaps the child.
pub struct WatchChild {
    child: Child,
    tail: Arc<Mutex<VecDeque<String>>>,
    forwarder: Option<JoinHandle<()>>,
}

impl WatchChild {
    /// `<paste_bin> --no-newline --watch <hook...>`. No `--type text`:
    /// wl-paste would then never run the hook for an image-only offer.
    pub fn spawn(clipboard: &Clipboard, hook: &[OsString]) -> io::Result<WatchChild> {
        let mut cmd = Command::new(&clipboard.paste_bin);
        cmd.args(["--no-newline", "--watch"])
            .args(hook)
            .stdin(Stdio::null())
            // /dev/null: without --type, wl-paste infers a type from the name of
            // the file its stdout is, and "null" names none
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let daemon = rustix::process::getpid();
        // SAFETY: the closure runs in the forked child before exec, so it may
        // only make async-signal-safe calls. It makes two raw syscalls through
        // rustix (prctl, getppid), allocates nothing and touches no locks.
        // PR_SET_PDEATHSIG fires when the thread that forked us exits, which
        // is why the daemon spawns the watcher from its main thread. This is
        // the project's one `unsafe` block.
        unsafe {
            cmd.pre_exec(move || {
                rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::TERM))?;
                // The daemon died between fork and prctl: the signal will never come
                if rustix::process::getppid() != Some(daemon) {
                    return Err(rustix::io::Errno::SRCH.into());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn()?;
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let forwarder = child.stderr.take().map(|stderr| {
            let tail = Arc::clone(&tail);
            std::thread::spawn(move || forward_stderr(stderr, &tail))
        });
        Ok(WatchChild { child, tail, forwarder })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// `Some((success, stderr_tail))` once the child has exited.
    pub fn try_exit(&mut self) -> io::Result<Option<(bool, String)>> {
        let Some(status) = self.child.try_wait()? else {
            return Ok(None);
        };
        // Give the forwarder a moment to read the last lines
        let deadline = Instant::now() + Duration::from_millis(200);
        while self.forwarder.as_ref().is_some_and(|f| !f.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let tail = self.tail.lock().map(|t| Vec::from(t.clone()).join("\n")).unwrap_or_default();
        Ok(Some((status.success(), tail)))
    }
}

impl Drop for WatchChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Copy the watcher's stderr to ours, keeping the last few lines.
fn forward_stderr(stderr: impl Read, tail: &Mutex<VecDeque<String>>) {
    let mut reader = BufReader::new(stderr);
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
        let text = String::from_utf8_lossy(&line).trim_end().to_string();
        eprintln!("{text}");
        if let Ok(mut tail) = tail.lock() {
            if tail.len() == STDERR_TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(text);
        }
        line.clear();
    }
}

/// Polling capture: read the clipboard every `interval` and save it when it
/// changed.
pub struct Poller {
    interval: Duration,
    last_poll: Option<Instant>,
    last_hash: Option<Vec<u8>>,
}

impl Poller {
    pub fn new(interval: Duration) -> Self {
        Self { interval, last_poll: None, last_hash: None }
    }

    /// Read the clipboard if a poll is due at `now`.
    pub fn poll_if_due(&mut self, storage: &Storage, clipboard: &Clipboard, now: Instant) {
        if self.last_poll.is_some_and(|t| now.saturating_duration_since(t) < self.interval) {
            return;
        }
        self.last_poll = Some(now);
        // No selection, or none as text, reads as ""; errors are retried next tick
        match clipboard.paste_text() {
            Ok(content) if !content.is_empty() => {
                if self.last_hash.as_ref() == Some(&util::compute_hash(&content)) {
                    return;
                }
                // New text: its types decide whether the offer is an image
                let types = clipboard.list_types().unwrap_or_default();
                if clipboard::choose_offer(&types) == clipboard::Offer::Image {
                    self.poll_image(storage, clipboard, types);
                } else {
                    self.observe(storage, &content, || types);
                }
            }
            Ok(_) => {
                if let Ok(types) = clipboard.list_types() {
                    self.poll_image(storage, clipboard, types);
                }
            }
            Err(_) => {}
        }
    }

    /// Save the PNG when `choose_offer` picks it from `types`. The PNG is
    /// read on every such poll (there is no cheaper change signal), but
    /// saved only when its hash differs from the last poll's.
    fn poll_image(&mut self, storage: &Storage, clipboard: &Clipboard, types: Vec<String>) {
        if clipboard::choose_offer(&types) != clipboard::Offer::Image {
            return;
        }
        let Ok(png) = clipboard.paste_bytes(crate::png::MIME) else {
            return;
        };
        if png.is_empty() {
            return;
        }
        let hash = util::compute_image_hash(&png);
        if self.last_hash.as_ref() == Some(&hash) {
            return;
        }
        match storage.ingest_image(&png, Source::Poll, &types) {
            Ok(outcome) => {
                self.last_hash = Some(hash);
                log_outcome("poll", storage, &outcome);
            }
            Err(e) => eprintln!("[poll] error saving image (retrying next poll): {e:#}"),
        }
    }

    /// Save `content` if it differs from the last poll. The hash is recorded
    /// once ingest returns, even when it skips the content, so a secret isn't
    /// re-read on every tick; after an ingest error it isn't, so the next
    /// poll retries.
    pub fn observe(&mut self, storage: &Storage, content: &str, types: impl FnOnce() -> Vec<String>) {
        self.observe_with(storage, content, types, |c, t| storage.ingest(c, Source::Poll, t));
    }

    /// `observe` with the ingest call injected, so tests can make it fail.
    pub(crate) fn observe_with(
        &mut self,
        storage: &Storage,
        content: &str,
        types: impl FnOnce() -> Vec<String>,
        ingest: impl FnOnce(&str, &[String]) -> Result<IngestOutcome>,
    ) {
        if content.is_empty() {
            return;
        }
        let hash = util::compute_hash(content);
        if self.last_hash.as_ref() == Some(&hash) {
            return;
        }
        match ingest(content, &types()) {
            Ok(outcome) => {
                // Only now: a failed ingest is retried on the next tick
                self.last_hash = Some(hash);
                log_outcome("poll", storage, &outcome);
            }
            Err(e) => eprintln!("[poll] error saving entry (retrying next poll): {e:#}"),
        }
    }
}

/// The daemon's capture: supervisor, watch child and poller together.
pub struct Capture {
    supervisor: Supervisor,
    clipboard: Clipboard,
    hook: Vec<OsString>,
    child: Option<WatchChild>,
    poller: Poller,
    polling: bool,
}

impl Capture {
    /// `hook` is the command wl-paste runs per selection.
    pub fn new(mode: CaptureMode, poll_interval: Duration, clipboard: Clipboard, hook: Vec<OsString>) -> Self {
        Self {
            supervisor: Supervisor::new(mode),
            clipboard,
            hook,
            child: None,
            poller: Poller::new(poll_interval),
            polling: false,
        }
    }

    /// One daemon tick: reap a dead watcher, (re)spawn it, or poll. Fails
    /// only when `watch` mode gives up.
    pub fn step(&mut self, storage: &Storage, now: Instant) -> Result<()> {
        if let Some(child) = &mut self.child
            && let Some((success, stderr_tail)) = child.try_exit()?
        {
            self.child = None;
            let action = self.supervisor.on(Event::Exited { at: now, success, stderr_tail });
            self.apply(action, now)?;
        }
        let action = self.supervisor.on(Event::Tick(now));
        self.apply(action, now)?;
        if self.polling {
            self.poller.poll_if_due(storage, &self.clipboard, now);
        }
        Ok(())
    }

    fn apply(&mut self, action: Action, now: Instant) -> Result<()> {
        match action {
            Action::Wait => {}
            Action::SpawnWatch => match WatchChild::spawn(&self.clipboard, &self.hook) {
                Ok(child) => {
                    eprintln!("[capture] watching the clipboard (wl-paste pid {})", child.pid());
                    self.child = Some(child);
                    self.supervisor.on(Event::Spawned(now));
                }
                Err(e) => {
                    let stderr_tail = format!("cannot run {}: {e}", self.clipboard.paste_bin.display());
                    let action = self.supervisor.on(Event::Exited { at: now, success: false, stderr_tail });
                    self.apply(action, now)?;
                }
            },
            Action::StartPolling { reason } => {
                eprintln!("[capture] polling every {} ms: {reason}", self.poller.interval.as_millis());
                self.polling = true;
            }
            Action::GiveUp(reason) => anyhow::bail!("clipboard capture failed: {reason}"),
        }
        Ok(())
    }

    /// Whether the poller is active (the daemon then ticks at its interval).
    pub fn is_polling(&self) -> bool {
        self.polling
    }

    /// "watch", "poll" or "none", for `PING`.
    pub fn capturing(&self) -> &'static str {
        self.supervisor.capturing()
    }

    /// Kill and reap the watch child.
    pub fn stop(&mut self) {
        self.child = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn exited(at: Instant, tail: &str) -> Event {
        Event::Exited { at, success: false, stderr_tail: tail.to_string() }
    }

    /// Start a supervisor and its first watch child at `t0`.
    fn started(mode: CaptureMode, t0: Instant) -> Supervisor {
        let mut sup = Supervisor::new(mode);
        assert_eq!(sup.on(Event::Tick(t0)), Action::SpawnWatch);
        assert_eq!(sup.on(Event::Tick(t0)), Action::Wait, "only one spawn at a time");
        assert_eq!(sup.on(Event::Spawned(t0)), Action::Wait);
        sup
    }

    #[test]
    fn poller_saves_new_content_once_with_poll_source() {
        let (storage, _dir) = crate::storage::test_storage();
        let mut poller = Poller::new(Duration::from_millis(250));
        let mut type_reads = 0;
        for content in ["copied text", "copied text", ""] {
            poller.observe(&storage, content, || {
                type_reads += 1;
                vec!["text/html".to_string()]
            });
        }
        assert_eq!(type_reads, 1, "types are read only for new content");

        let entries = storage.list(crate::storage::ListOrder::Recency, None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, Source::Poll);
        assert_eq!(entries[0].mime, "text/html");
        assert_eq!(storage.load_content(entries[0].id).unwrap(), "copied text");
    }

    #[test]
    fn poller_retries_content_whose_ingest_failed() {
        let (storage, _dir) = crate::storage::test_storage();
        let mut poller = Poller::new(Duration::from_millis(250));
        let mut attempts = 0;
        let store = &storage;
        let ingest = |fail: bool| {
            move |content: &str, types: &[String]| -> Result<IngestOutcome> {
                if fail {
                    anyhow::bail!("database is locked");
                }
                store.ingest(content, Source::Poll, types)
            }
        };
        poller.observe_with(&storage, "copied text", || { attempts += 1; Vec::new() }, ingest(true));
        poller.observe_with(&storage, "copied text", || { attempts += 1; Vec::new() }, ingest(false));
        assert_eq!(attempts, 2, "a failed ingest is retried on the next tick");
        poller.observe_with(&storage, "copied text", || { attempts += 1; Vec::new() }, ingest(false));
        assert_eq!(attempts, 2, "a saved clip is not re-read");
        assert_eq!(storage.list(crate::storage::ListOrder::Recency, None).unwrap().len(), 1);
    }

    #[test]
    fn auto_falls_back_to_poll_on_fast_exit() {
        let t0 = Instant::now();
        let mut sup = started(CaptureMode::Auto, t0);
        assert_eq!(sup.capturing(), "watch");
        let tail = "Watch mode requires a compositor that supports the wlroots data-control protocol";
        match sup.on(exited(t0 + Duration::from_millis(300), &format!("{tail}\n"))) {
            Action::StartPolling { reason } => assert!(reason.contains(tail), "{reason}"),
            other => panic!("expected polling, got {other:?}"),
        }
        assert_eq!(sup.capturing(), "poll");
        // Polling is final: no more spawns
        assert_eq!(sup.on(Event::Tick(t0 + secs(100))), Action::Wait);

        // A watch child that can't even be spawned counts as a fast exit
        let mut sup = Supervisor::new(CaptureMode::Auto);
        assert_eq!(sup.on(Event::Tick(t0)), Action::SpawnWatch);
        assert!(matches!(sup.on(exited(t0, "No such file")), Action::StartPolling { .. }));
    }

    #[test]
    fn restarts_with_backoff_after_late_exit() {
        let t0 = Instant::now();
        let mut sup = started(CaptureMode::Auto, t0);

        // Ran 10 s, then died: restart after 1 s, then 2 s, then 4 s
        let mut at = t0 + secs(10);
        for backoff in [1, 2, 4] {
            assert_eq!(sup.on(exited(at, "lost connection")), Action::Wait);
            assert_eq!(
                sup.on(Event::Tick(at + secs(backoff) - Duration::from_millis(1))),
                Action::Wait
            );
            assert_eq!(sup.on(Event::Tick(at + secs(backoff))), Action::SpawnWatch);
            assert_eq!(sup.on(Event::Spawned(at + secs(backoff))), Action::Wait);
            assert_eq!(sup.capturing(), "watch");
            at += secs(backoff) + secs(20);
        }

        // A child that ran a long time resets the backoff to 1 s
        let at = at + secs(120);
        assert_eq!(sup.on(exited(at, "")), Action::Wait);
        assert_eq!(sup.on(Event::Tick(at + secs(1))), Action::SpawnWatch);
    }

    #[test]
    fn backoff_caps_at_30s() {
        assert_eq!(backoff(0), secs(1));
        assert_eq!(backoff(4), secs(16));
        assert_eq!(backoff(5), secs(30));
        assert_eq!(backoff(40), secs(30));
    }

    #[test]
    fn auto_polls_after_5_restarts_in_60s() {
        let t0 = Instant::now();
        let mut sup = started(CaptureMode::Auto, t0);
        let mut at = t0 + secs(3);
        for _ in 0..5 {
            assert_eq!(sup.on(exited(at, "crash")), Action::Wait);
            let respawn = at + secs(1);
            sup.on(Event::Tick(respawn + secs(30)));
            sup.on(Event::Spawned(respawn));
            at = respawn + Duration::from_millis(2500);
        }
        assert!(at - t0 < secs(60));
        match sup.on(exited(at, "crash again")) {
            Action::StartPolling { reason } => {
                assert!(reason.contains("5 times"), "{reason}");
                assert!(reason.contains("crash again"), "{reason}");
            }
            other => panic!("expected polling, got {other:?}"),
        }

        // Restarts spread over more than 60 s don't trigger the fallback
        let mut sup = started(CaptureMode::Auto, t0);
        let mut at = t0 + secs(3);
        for _ in 0..10 {
            assert_eq!(sup.on(exited(at, "crash")), Action::Wait);
            let respawn = at + secs(30);
            assert_eq!(sup.on(Event::Tick(respawn)), Action::SpawnWatch);
            sup.on(Event::Spawned(respawn));
            at = respawn + secs(5);
        }
    }

    #[test]
    fn watch_mode_gives_up_instead_of_polling() {
        let t0 = Instant::now();
        let mut sup = started(CaptureMode::Watch, t0);
        match sup.on(exited(t0 + Duration::from_millis(100), "no data-control")) {
            Action::GiveUp(reason) => assert!(reason.contains("no data-control"), "{reason}"),
            other => panic!("expected give-up, got {other:?}"),
        }

        let mut sup = started(CaptureMode::Watch, t0);
        let mut at = t0 + secs(3);
        for _ in 0..5 {
            assert_eq!(sup.on(exited(at, "crash")), Action::Wait);
            let respawn = at + secs(1);
            sup.on(Event::Tick(respawn + secs(30)));
            sup.on(Event::Spawned(respawn));
            at = respawn + secs(3);
        }
        assert!(matches!(sup.on(exited(at, "crash")), Action::GiveUp(_)));
    }

    #[test]
    fn poll_mode_never_spawns_watch() {
        let t0 = Instant::now();
        let mut sup = Supervisor::new(CaptureMode::Poll);
        assert_eq!(sup.capturing(), "poll", "before the first tick too");
        assert!(matches!(sup.on(Event::Tick(t0)), Action::StartPolling { .. }));
        assert_eq!(sup.capturing(), "poll");
        for n in 1..100 {
            assert_eq!(sup.on(Event::Tick(t0 + secs(n))), Action::Wait);
        }
    }
}

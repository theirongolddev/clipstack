//! Clipboard capture: the `__watch-hook` that `wl-paste --watch` runs once
//! per new selection.

// Removed in commit 2-6, which runs the supervisor from the daemon
#![cfg_attr(not(test), expect(dead_code, reason = "Supervisor has no caller until commit 2-6"))]

use crate::clipboard::Clipboard;
use crate::storage::{IngestOutcome, Source, Storage};
use anyhow::Result;
use serde::Deserialize;
use std::collections::VecDeque;
use std::io::{self, Read};
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

/// Handle one selection from `wl-paste --watch`: `state` is
/// `CLIPBOARD_STATE` and `input` the selection's text. Never fails on bad
/// input (it is logged, without content), so the caller can always exit 0
/// and wl-paste keeps running; stdin is always read to the end, so the
/// source client never sees EPIPE.
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
    if buf.len() > limit {
        let size = buf.len() as u64 + drain(input)?;
        eprintln!("[watch] skipped: too large ({size} bytes; max_entry_bytes is {limit})");
        return Ok(());
    }
    let Ok(content) = String::from_utf8(buf) else {
        eprintln!("[watch] skipped: not UTF-8 text");
        return Ok(());
    };

    let types = clipboard.list_types().unwrap_or_else(|e| {
        eprintln!("[watch] could not list offer types: {e:#}");
        Vec::new()
    });
    log_outcome("watch", storage, &storage.ingest(&content, Source::Watch, &types)?);
    Ok(())
}

/// One log line per ingest, never with the content.
pub fn log_outcome(tag: &str, storage: &Storage, outcome: &IngestOutcome) {
    match outcome {
        IngestOutcome::Inserted(entry) => eprintln!("[{tag}] saved id:{} ({} bytes)", entry.id, entry.size),
        IngestOutcome::Bumped(entry) => eprintln!("[{tag}] bumped id:{} ({} bytes)", entry.id, entry.size),
        IngestOutcome::Skipped(reason) => {
            eprintln!("[{tag}] skipped: {}", storage.policy().describe_skip(reason));
        }
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
        assert!(matches!(sup.on(Event::Tick(t0)), Action::StartPolling { .. }));
        assert_eq!(sup.capturing(), "poll");
        for n in 1..100 {
            assert_eq!(sup.on(Event::Tick(t0 + secs(n))), Action::Wait);
        }
    }
}

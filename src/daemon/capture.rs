//! Clipboard capture: the `__watch-hook` that `wl-paste --watch` runs once
//! per new selection.

use crate::clipboard::Clipboard;
use crate::storage::{IngestOutcome, Source, Storage};
use anyhow::Result;
use std::io::{self, Read};

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

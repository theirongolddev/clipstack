//! The daemon's Unix socket (`<runtime dir>/clipstack.sock`, `0600`):
//! protocol v1 PUSH/PULL/PING, one connection at a time on one thread.
//! The only expected client is the user's `ssh -R` forward running rcopy.

use crate::clipboard::{self, Clipboard};
use crate::config::SocketConfig;
use crate::protocol::{self, ErrCode, Request, Response};
use crate::storage::{IngestOutcome, Source, Storage};
use anyhow::{Context, Result};
use rustix::event::{PollFd, PollFlags, Timespec};
use std::io::{self, BufReader, ErrorKind, Read};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How often the accept loop checks `running` while idle.
const ACCEPT_POLL: Timespec = Timespec { tv_sec: 0, tv_nsec: 100_000_000 };

/// What the daemon currently captures with ("watch", "poll" or "none"),
/// updated by the main loop and reported by PING.
pub type CaptureStatus = Arc<Mutex<&'static str>>;

/// Answers requests; everything a connection needs, owned by the socket thread.
pub struct Handler {
    pub storage: Arc<Storage>,
    pub clipboard: Clipboard,
    pub config: SocketConfig,
    pub capture: CaptureStatus,
}

impl Handler {
    pub fn handle(&self, req: Request) -> Response {
        match req {
            Request::Push(body) => self.push(body),
            Request::Pull => self.pull(),
            Request::Ping => self.ping(),
        }
    }

    /// Save (by policy) and copy. Content the policy skips is still copied:
    /// the user sent it explicitly.
    fn push(&self, body: Vec<u8>) -> Response {
        let Ok(text) = String::from_utf8(body) else {
            eprintln!("[socket] PUSH refused: not UTF-8 text");
            return Response::err(ErrCode::NotUtf8, "body is not UTF-8 text");
        };
        let resp = match self.storage.ingest(&text, Source::Remote, &[]) {
            Ok(outcome) => {
                crate::daemon::capture::log_outcome("socket", &self.storage, &outcome);
                match outcome {
                    IngestOutcome::Inserted(entry) | IngestOutcome::Bumped(entry) => {
                        Response::ok(Vec::new()).with("stored", 1).with("id", entry.id)
                    }
                    IngestOutcome::Skipped(reason) => {
                        Response::ok(Vec::new()).with("stored", 0).with("reason", reason.code())
                    }
                }
            }
            Err(e) => {
                eprintln!("[socket] PUSH failed to save: {e:#}");
                return Response::err(ErrCode::Internal, format!("saving failed: {e:#}"));
            }
        };
        match self.clipboard.copy(&text) {
            Ok(()) => resp,
            Err(e) => {
                eprintln!("[socket] PUSH saved but copy failed: {e:#}");
                Response::err(ErrCode::Unavailable, format!("wl-copy failed: {e:#}"))
            }
        }
    }

    fn pull(&self) -> Response {
        if !self.config.allow_pull {
            return Response::err(ErrCode::Forbidden, "pull disabled (set socket.allow_pull = true)");
        }
        let types = match self.clipboard.list_types() {
            Ok(types) => types,
            Err(e) => return Response::err(ErrCode::Unavailable, format!("wl-paste failed: {e:#}")),
        };
        if clipboard::is_sensitive_offer(&types) {
            eprintln!("[socket] PULL refused: clipboard marked sensitive");
            return Response::err(ErrCode::Sensitive, "the clipboard is marked sensitive by a password manager");
        }
        match self.clipboard.paste_text() {
            Ok(text) => Response::ok(text.into_bytes()),
            Err(e) => Response::err(ErrCode::Unavailable, format!("wl-paste failed: {e:#}")),
        }
    }

    fn ping(&self) -> Response {
        let capture = *self.capture.lock().unwrap_or_else(|p| p.into_inner());
        Response::ok(Vec::new())
            .with("version", env!("CARGO_PKG_VERSION"))
            .with("capture", capture)
            .with("pull", u8::from(self.config.allow_pull))
    }
}

/// The listening socket and its accept thread.
pub struct SocketServer {
    thread: Option<JoinHandle<()>>,
}

impl SocketServer {
    /// Bind `path` (`0600`) and serve on a new thread until `running` is false.
    pub fn start(path: &Path, handler: Handler, running: Arc<AtomicBool>) -> Result<Self> {
        let listener = bind(path)?;
        listener.set_nonblocking(true)?;
        eprintln!("[socket] listening on {}", path.display());
        let thread = std::thread::Builder::new()
            .name("socket".into())
            .spawn(move || accept_loop(&listener, &handler, &running))
            .context("Cannot start the socket thread")?;
        Ok(Self { thread: Some(thread) })
    }

    /// Wait for the accept thread (call after clearing `running`).
    pub fn join(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn bind(path: &Path) -> Result<UnixListener> {
    remove_stale(path)?;
    let listener = UnixListener::bind(path).with_context(|| format!("Cannot bind {}", path.display()))?;
    // The runtime dir is 0700, so the window before chmod doesn't matter
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("Cannot chmod {}", path.display()))?;
    Ok(listener)
}

/// Remove a socket left by a daemon that crashed. We hold the daemon lock,
/// so no other clipstack daemon serves it; still, never unlink a socket that
/// accepts connections or a path that isn't a socket.
fn remove_stale(path: &Path) -> Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("Cannot inspect {}", path.display())),
    };
    if !meta.file_type().is_socket() {
        anyhow::bail!("{} exists and is not a socket; leaving it alone", path.display());
    }
    if UnixStream::connect(path).is_ok() {
        anyhow::bail!("{} is in use by another process; leaving it alone", path.display());
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Cannot remove stale socket {}", path.display())),
    }
}

fn accept_loop(listener: &UnixListener, handler: &Handler, running: &AtomicBool) {
    while running.load(Ordering::SeqCst) {
        let mut fds = [PollFd::new(listener, PollFlags::IN)];
        match rustix::event::poll(&mut fds, Some(&ACCEPT_POLL)) {
            Ok(0) => continue,
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => {
                eprintln!("[socket] poll failed, socket disabled: {e}");
                return;
            }
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Err(e) = serve_connection(stream, handler) {
                    eprintln!("[socket] connection error: {e:#}");
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                eprintln!("[socket] accept failed: {e}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// Reads from the client with `timeout` per read and an overall deadline, so
/// a client dripping one byte at a time can't hold the (only) socket thread.
struct DeadlineReader<'a> {
    stream: &'a UnixStream,
    timeout: Duration,
    deadline: Instant,
}

impl Read for DeadlineReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::from(ErrorKind::TimedOut));
        }
        self.stream.set_read_timeout(Some(left.min(self.timeout)))?;
        self.stream.read(buf)
    }
}

fn serve_connection(stream: UnixStream, handler: &Handler) -> Result<()> {
    stream.set_nonblocking(false)?;
    let timeout = Duration::from_millis(handler.config.timeout_ms);
    stream.set_write_timeout(Some(timeout))?;
    let mut reader = BufReader::new(DeadlineReader {
        stream: &stream,
        timeout,
        deadline: Instant::now() + 2 * timeout,
    });
    // Larger pushes could never be stored; refuse them from the header
    let max_body = handler.storage.policy().max_entry_bytes().min(protocol::MAX_BODY);
    let resp = match protocol::read_request(&mut reader, max_body) {
        Ok(req) => handler.handle(req),
        Err(e) => {
            eprintln!("[socket] bad request: {e}");
            Response::err(e.code(), e.to_string())
        }
    };
    protocol::write_response(&mut &stream, &resp).context("Cannot send the response")?;
    Ok(())
}

//! Socket protocol v1: one length-prefixed request and one response per
//! connection. Std-only, so integration tests and the Step 3 client can
//! include it with `#[path]`.
//!
//! ```text
//! request  = "CLIPSTACK/1" SP verb SP length LF body
//! response = "OK" SP length *(SP key "=" value) LF body
//!          / "ERR" SP code SP message LF
//! ```

use std::fmt;
use std::io::{self, BufRead, Read, Write};

pub const VERSION: &str = "CLIPSTACK/1";
/// Longest header line, including its newline.
pub const MAX_HEADER: usize = 256;
/// Upper bound on any body; the server also caps PUSH at `max_entry_bytes`.
pub const MAX_BODY: usize = 16 * 1024 * 1024;
/// `length = 1*10DIGIT`
const MAX_LEN_DIGITS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Push(Vec<u8>),
    Pull,
    Ping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrCode {
    BadRequest,
    TooLarge,
    NotUtf8,
    Forbidden,
    Sensitive,
    Timeout,
    Unavailable,
    Internal,
}

impl ErrCode {
    pub const ALL: [ErrCode; 8] = [
        ErrCode::BadRequest,
        ErrCode::TooLarge,
        ErrCode::NotUtf8,
        ErrCode::Forbidden,
        ErrCode::Sensitive,
        ErrCode::Timeout,
        ErrCode::Unavailable,
        ErrCode::Internal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ErrCode::BadRequest => "bad_request",
            ErrCode::TooLarge => "too_large",
            ErrCode::NotUtf8 => "not_utf8",
            ErrCode::Forbidden => "forbidden",
            ErrCode::Sensitive => "sensitive",
            ErrCode::Timeout => "timeout",
            ErrCode::Unavailable => "unavailable",
            ErrCode::Internal => "internal",
        }
    }

    pub fn parse(s: &str) -> Option<ErrCode> {
        ErrCode::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

impl fmt::Display for ErrCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Ok { body: Vec<u8>, meta: Vec<(String, String)> },
    Err { code: ErrCode, message: String },
}

impl Response {
    pub fn ok(body: Vec<u8>) -> Self {
        Response::Ok { body, meta: Vec::new() }
    }

    pub fn err(code: ErrCode, message: impl Into<String>) -> Self {
        Response::Err { code, message: message.into() }
    }

    /// Add a `key=value` pair to an `OK` response (no-op on `ERR`).
    pub fn with(mut self, key: &str, value: impl fmt::Display) -> Self {
        if let Response::Ok { meta, .. } = &mut self {
            meta.push((key.to_string(), value.to_string()));
        }
        self
    }

    /// The first value for `key` in an `OK` response.
    #[cfg_attr(not(test), expect(dead_code, reason = "client side, first used by `status`"))]
    pub fn meta(&self, key: &str) -> Option<&str> {
        match self {
            Response::Ok { meta, .. } => meta.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()),
            Response::Err { .. } => None,
        }
    }
}

/// Why a request or response couldn't be read. `code()` is what the server
/// answers with.
#[derive(Debug)]
pub enum ProtoError {
    Io(io::Error),
    BadRequest(String),
    TooLarge { len: u64, max: usize },
}

impl ProtoError {
    pub fn code(&self) -> ErrCode {
        match self {
            ProtoError::Io(e) if is_timeout(e) => ErrCode::Timeout,
            ProtoError::Io(_) | ProtoError::BadRequest(_) => ErrCode::BadRequest,
            ProtoError::TooLarge { .. } => ErrCode::TooLarge,
        }
    }
}

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtoError::Io(e) if is_timeout(e) => f.write_str("timed out"),
            ProtoError::Io(e) => write!(f, "{e}"),
            ProtoError::BadRequest(msg) => f.write_str(msg),
            ProtoError::TooLarge { len, max } => write!(f, "{len} bytes is over the {max}-byte limit"),
        }
    }
}

impl std::error::Error for ProtoError {}

impl From<io::Error> for ProtoError {
    fn from(e: io::Error) -> Self {
        ProtoError::Io(e)
    }
}

/// Socket read/write timeouts surface as `WouldBlock` (or `TimedOut`).
fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

fn bad(msg: impl Into<String>) -> ProtoError {
    ProtoError::BadRequest(msg.into())
}

/// Read one header line (at most `MAX_HEADER` bytes with its newline), ASCII,
/// without the newline.
fn read_header(r: &mut impl BufRead) -> Result<String, ProtoError> {
    let mut line = Vec::new();
    r.take(MAX_HEADER as u64 + 1).read_until(b'\n', &mut line)?;
    if line.last() != Some(&b'\n') {
        return Err(match line.len() {
            0 => bad("connection closed before the header"),
            n if n > MAX_HEADER => bad(format!("header over {MAX_HEADER} bytes")),
            _ => bad("header not terminated by a newline"),
        });
    }
    if line.len() > MAX_HEADER {
        return Err(bad(format!("header over {MAX_HEADER} bytes")));
    }
    line.pop();
    String::from_utf8(line)
        .ok()
        .filter(|h| h.is_ascii())
        .ok_or_else(|| bad("header is not ASCII"))
}

fn parse_len(s: &str) -> Result<u64, ProtoError> {
    if s.is_empty() || s.len() > MAX_LEN_DIGITS || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad(format!("bad length {s:?}: expected 1-{MAX_LEN_DIGITS} digits")));
    }
    s.parse().map_err(|_| bad(format!("bad length {s:?}")))
}

/// Read exactly `len` body bytes (`len` already checked against a limit).
fn read_body(r: &mut impl Read, len: u64) -> Result<Vec<u8>, ProtoError> {
    let mut body = Vec::with_capacity(len.min(MAX_BODY as u64) as usize);
    r.take(len).read_to_end(&mut body)?;
    if (body.len() as u64) < len {
        return Err(bad(format!("body ended after {} of {len} bytes", body.len())));
    }
    Ok(body)
}

/// Read one request. A PUSH longer than `max_body` is rejected from its
/// header, before any of the body is read.
pub fn read_request(r: &mut impl BufRead, max_body: usize) -> Result<Request, ProtoError> {
    let header = read_header(r)?;
    let parts: Vec<&str> = header.split(' ').collect();
    let [version, verb, len] = parts[..] else {
        return Err(bad(format!("expected \"{VERSION} VERB LENGTH\", got {header:?}")));
    };
    if version != VERSION {
        return Err(bad(format!("unsupported version {version:?} (this server speaks {VERSION})")));
    }
    let len = parse_len(len)?;
    match verb {
        "PUSH" => {
            let max = max_body.min(MAX_BODY);
            if len > max as u64 {
                return Err(ProtoError::TooLarge { len, max });
            }
            Ok(Request::Push(read_body(r, len)?))
        }
        "PULL" | "PING" if len != 0 => Err(bad(format!("{verb} takes no body (length must be 0)"))),
        "PULL" => Ok(Request::Pull),
        "PING" => Ok(Request::Ping),
        _ => Err(bad(format!("unknown verb {verb:?} (expected PUSH, PULL or PING)"))),
    }
}

#[cfg_attr(not(test), expect(dead_code, reason = "client side, first used by `status`"))]
pub fn write_request(w: &mut impl Write, req: &Request) -> io::Result<()> {
    match req {
        Request::Push(body) => {
            writeln!(w, "{VERSION} PUSH {}", body.len())?;
            w.write_all(body)?;
        }
        Request::Pull => writeln!(w, "{VERSION} PULL 0")?,
        Request::Ping => writeln!(w, "{VERSION} PING 0")?,
    }
    w.flush()
}

/// Header-safe text: whitespace (newlines included) becomes `sub`.
fn one_line(s: &str, sub: char) -> String {
    s.chars().map(|c| if c.is_whitespace() { sub } else { c }).collect()
}

pub fn write_response(w: &mut impl Write, resp: &Response) -> io::Result<()> {
    match resp {
        Response::Ok { body, meta } => {
            let mut header = format!("OK {}", body.len());
            for (k, v) in meta {
                header.push_str(&format!(" {}={}", one_line(k, '_'), one_line(v, '_')));
            }
            header.push('\n');
            w.write_all(header.as_bytes())?;
            w.write_all(body)?;
        }
        Response::Err { code, message } => writeln!(w, "ERR {code} {}", one_line(message, ' '))?,
    }
    w.flush()
}

/// Read one response; an `OK` body longer than `max_body` is an error.
#[cfg_attr(not(test), expect(dead_code, reason = "client side, first used by `status`"))]
pub fn read_response(r: &mut impl BufRead, max_body: usize) -> Result<Response, ProtoError> {
    let header = read_header(r)?;
    if let Some(rest) = header.strip_prefix("ERR ") {
        let (code, message) = rest.split_once(' ').unwrap_or((rest, ""));
        let code = ErrCode::parse(code).ok_or_else(|| bad(format!("unknown error code {code:?}")))?;
        return Ok(Response::err(code, message));
    }
    let mut parts = header.strip_prefix("OK ").ok_or_else(|| bad(format!("bad response {header:?}")))?.split(' ');
    let len = parse_len(parts.next().unwrap_or_default())?;
    let meta = parts
        .map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| bad(format!("bad key=value in {header:?}")))?;
    let max = max_body.min(MAX_BODY);
    if len > max as u64 {
        return Err(ProtoError::TooLarge { len, max });
    }
    Ok(Response::Ok { body: read_body(r, len)?, meta })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn read(bytes: &[u8], max_body: usize) -> Result<Request, ProtoError> {
        read_request(&mut Cursor::new(bytes), max_body)
    }

    fn bad_request(result: Result<Request, ProtoError>) -> String {
        match result {
            Err(e) => {
                assert_eq!(e.code(), ErrCode::BadRequest, "{e}");
                e.to_string()
            }
            Ok(r) => panic!("expected bad_request, got {r:?}"),
        }
    }

    #[test]
    fn parses_push_header() {
        assert_eq!(read(b"CLIPSTACK/1 PUSH 5\nhello", 100).unwrap(), Request::Push(b"hello".to_vec()));
        assert_eq!(read(b"CLIPSTACK/1 PUSH 0\n", 100).unwrap(), Request::Push(Vec::new()));
        assert_eq!(read(b"CLIPSTACK/1 PULL 0\n", 100).unwrap(), Request::Pull);
        assert_eq!(read(b"CLIPSTACK/1 PING 0\n", 100).unwrap(), Request::Ping);
        // The body is exactly LEN bytes, newlines included
        assert_eq!(read(b"CLIPSTACK/1 PUSH 3\na\nbcd", 100).unwrap(), Request::Push(b"a\nb".to_vec()));
        // A short body is an error, not a truncated push
        assert!(bad_request(read(b"CLIPSTACK/1 PUSH 9\nshort", 100)).contains("body"));
    }

    #[test]
    fn rejects_header_over_256_bytes() {
        let long = format!("CLIPSTACK/1 PUSH {}\n", "0".repeat(300));
        assert!(bad_request(read(long.as_bytes(), 100)).contains("256"));
        // 256 bytes including the newline is still fine
        let pad = 256 - "CLIPSTACK/1 PING \n".len();
        let max = format!("CLIPSTACK/1 PING {}\n", "0".repeat(pad));
        assert_eq!(max.len(), 256);
        assert!(bad_request(read(max.as_bytes(), 100)).contains("length"), "too many digits, not too long");
        bad_request(read(b"CLIPSTACK/1 PING 0", 100)); // no newline
        bad_request(read(b"", 100));
        bad_request(read("CLIPSTACK/1 PUSH 1\u{e9}\n".as_bytes(), 100)); // not ASCII
    }

    #[test]
    fn rejects_len_over_limit() {
        // Rejected from the header alone: the body is never read
        let err = read(b"CLIPSTACK/1 PUSH 101\n", 100).unwrap_err();
        assert_eq!(err.code(), ErrCode::TooLarge);
        assert!(err.to_string().contains("101") && err.to_string().contains("100"), "{err}");
        assert!(read(b"CLIPSTACK/1 PUSH 100\n", 100).is_err(), "100-byte limit, body missing");
        let body = vec![b'x'; 100];
        let mut req = b"CLIPSTACK/1 PUSH 100\n".to_vec();
        req.extend_from_slice(&body);
        assert_eq!(read(&req, 100).unwrap(), Request::Push(body));
    }

    #[test]
    fn rejects_unknown_verb_and_version() {
        assert!(bad_request(read(b"CLIPSTACK/2 PING 0\n", 100)).contains("version"));
        assert!(bad_request(read(b"HTTP/1.1 GET 0\n", 100)).contains("version"));
        assert!(bad_request(read(b"CLIPSTACK/1 POP 0\n", 100)).contains("verb"));
        assert!(bad_request(read(b"CLIPSTACK/1 push 0\n", 100)).contains("verb"));
        bad_request(read(b"CLIPSTACK/1 PING\n", 100));
        bad_request(read(b"CLIPSTACK/1 PING 0 extra\n", 100));
        bad_request(read(b"CLIPSTACK/1  PING 0\n", 100));
        bad_request(read(b"CLIPSTACK/1 PUSH -1\n", 100));
        bad_request(read(b"CLIPSTACK/1 PUSH +1\n", 100));
        bad_request(read(b"CLIPSTACK/1 PUSH 12345678901\n", 100)); // over 10 digits
    }

    #[test]
    fn rejects_nonzero_len_for_pull_and_ping() {
        assert!(bad_request(read(b"CLIPSTACK/1 PULL 1\nx", 100)).contains("PULL"));
        assert!(bad_request(read(b"CLIPSTACK/1 PING 3\nabc", 100)).contains("PING"));
    }

    #[test]
    fn request_roundtrip() {
        for req in [Request::Push(b"two\nlines".to_vec()), Request::Pull, Request::Ping] {
            let mut buf = Vec::new();
            write_request(&mut buf, &req).unwrap();
            assert_eq!(read(&buf, 100).unwrap(), req);
        }
    }

    #[test]
    fn ok_response_roundtrip_with_meta() {
        let resp = Response::ok(b"body\nwith newline".to_vec())
            .with("stored", "1")
            .with("id", "42");
        let mut buf = Vec::new();
        write_response(&mut buf, &resp).unwrap();
        assert!(buf.starts_with(b"OK 17 stored=1 id=42\nbody"), "{:?}", String::from_utf8_lossy(&buf));
        let back = read_response(&mut Cursor::new(&buf), 100).unwrap();
        assert_eq!(back, resp);
        assert_eq!(back.meta("id"), Some("42"));
        assert_eq!(back.meta("missing"), None);

        // Unknown keys are kept, so clients can ignore them
        let parsed = read_response(&mut Cursor::new(b"OK 0 future=x stored=0 reason=ignored\n"), 100).unwrap();
        assert_eq!(parsed.meta("reason"), Some("ignored"));
        // A meta value can't break the header: whitespace becomes '_'
        let mut buf = Vec::new();
        write_response(&mut buf, &Response::ok(Vec::new()).with("reason", "a b\nc")).unwrap();
        assert_eq!(buf, b"OK 0 reason=a_b_c\n");
    }

    #[test]
    fn err_response_roundtrip() {
        let resp = Response::err(ErrCode::Forbidden, "pull disabled (set socket.allow_pull = true)");
        let mut buf = Vec::new();
        write_response(&mut buf, &resp).unwrap();
        assert_eq!(buf, b"ERR forbidden pull disabled (set socket.allow_pull = true)\n");
        assert_eq!(read_response(&mut Cursor::new(&buf), 100).unwrap(), resp);

        // Messages stay on one line
        let mut buf = Vec::new();
        write_response(&mut buf, &Response::err(ErrCode::Internal, "line one\nline two")).unwrap();
        assert_eq!(buf, b"ERR internal line one line two\n");

        for code in ErrCode::ALL {
            assert_eq!(ErrCode::parse(code.as_str()), Some(code));
        }
        assert!(read_response(&mut Cursor::new(b"ERR nope message\n"), 100).is_err());
        assert!(read_response(&mut Cursor::new(b"WHAT 0\n"), 100).is_err());
    }

    #[test]
    fn timeouts_map_to_timeout_code() {
        let e = ProtoError::from(std::io::Error::from(std::io::ErrorKind::WouldBlock));
        assert_eq!(e.code(), ErrCode::Timeout);
        let e = ProtoError::from(std::io::Error::from(std::io::ErrorKind::TimedOut));
        assert_eq!(e.code(), ErrCode::Timeout);
    }
}

//! The subset of HTTP/1.1 that fennecdb needs.
//!
//! Why our own parser: the only requirement is "request line + headers +
//! `Content-Length` body", and that costs ~250 lines. An HTTP library would
//! bring a dependency tree, compile time and binary size -- the rest of the
//! project writes its own JSON and its own PostgreSQL wire for the same
//! reason.
//!
//! A body comes by its `Content-Length` or `Transfer-Encoding: chunked`,
//! decoded here into one buffer, so every route reads a body whole whichever
//! way it came ([`read_chunked`]). Rejected **explicitly, not silently**:
//! a body over the ceiling (413), a header block over the ceiling (431), and
//! whatever a server in front could frame otherwise than this one does
//! (400; a transfer coding but `chunked`, 501).

use std::io::{BufRead, IoSlice, Read, Write};

/// Ceiling of the header block. A large ceiling would let a single
/// connection grow memory without bound.
pub const MAX_HEADER_BYTES: usize = 64 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Patch,
    Put,
    Delete,
    Options,
    Head,
}

impl Method {
    fn parse(s: &str) -> Option<Method> {
        Some(match s {
            "GET" => Method::Get,
            "POST" => Method::Post,
            "PATCH" => Method::Patch,
            "PUT" => Method::Put,
            "DELETE" => Method::Delete,
            "OPTIONS" => Method::Options,
            "HEAD" => Method::Head,
            _ => return None,
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
            Method::Options => "OPTIONS",
            Method::Head => "HEAD",
        }
    }
}

pub struct Request {
    pub method: Method,
    /// The request target exactly as sent, still percent-encoded: a proxy
    /// forwards this, since re-encoding `path` and `query` would not
    /// reproduce every byte of the original.
    pub target: String,
    /// Percent-decoded path: `/articles/near`
    pub path: String,
    /// Query string pairs, **in order** and with repeats: two conditions on
    /// the same field (`?year=gte.2020&year=lt.2030`) would be lost in a map.
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub keep_alive: bool,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The non-empty `/`-separated segments of the path.
    pub fn segments(&self) -> Vec<&str> {
        self.path.split('/').filter(|s| !s.is_empty()).collect()
    }
}

/// A parse error: maps directly onto an HTTP status.
pub struct BadRequest(pub u16, pub String);

/// Reads a request. `Ok(None)` means the connection closed cleanly.
///
/// Generic over the reader for the tests' sake (a reader handing out a byte
/// a read); the server and the router read a `BufReader<TcpStream>`, the
/// one copy built.
pub fn read_request<R: BufRead>(
    reader: &mut R,
    max_body: usize,
) -> Result<Option<Request>, BadRequest> {
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let n = match read_line(reader, &mut line, MAX_HEADER_BYTES - head.len()) {
            Ok(n) => n,
            // Nothing of a request came before the silence (`--idle-timeout`)
            // or the reset: the connection is closed with nothing written.
            // A `400 read error` written here was read by a client that
            // reuses its connections (Python's `http.client`) as the answer
            // to the next request it sent, which never ran.
            Err(Line::Io(_)) if head.is_empty() && line.is_empty() => return Ok(None),
            Err(Line::Io(e)) => return Err(unread(&e)),
            Err(Line::Long) => return Err(BadRequest(431, "header line too long".into())),
        };
        if n == 0 {
            return if head.is_empty() {
                Ok(None) // the connection closed
            } else {
                Err(BadRequest(400, "truncated request header".into()))
            };
        }
        // Every line of the head ends in CRLF and holds no other CR (RFC
        // 9112 §2.2): a bare LF or CR that a server in front reads inside a
        // value, and this one read as a line's end, hides a
        // `Transfer-Encoding` or a `Content-Length` from that server, and
        // what it sent as the next request is read here as this one's body.
        let done = match line.strip_suffix(b"\r\n") {
            Some(content) if !content.contains(&b'\r') => content.is_empty(),
            Some(_) => return Err(BadRequest(400, "a bare CR in the request's head".into())),
            None if line.ends_with(b"\n") => {
                return Err(BadRequest(
                    400,
                    "a line of the head ends in a bare LF".into(),
                ));
            }
            None => return Err(BadRequest(400, "truncated request header".into())),
        };
        head.extend_from_slice(&line);
        if done {
            break;
        }
        if head.len() >= MAX_HEADER_BYTES {
            return Err(BadRequest(431, "header block too large".into()));
        }
    }

    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let start = lines.next().unwrap_or_default();
    let mut parts = start.split_whitespace();
    let (method, target, version) = match (parts.next(), parts.next(), parts.next()) {
        (Some(m), Some(t), Some(v)) => (m, t, v),
        _ => return Err(BadRequest(400, "malformed request line".into())),
    };
    let method = Method::parse(method)
        .ok_or_else(|| BadRequest(501, format!("`{method}` is not supported")))?;

    // The headers that frame the body and the connection are noted as they
    // are read: one pass, where three lookups after it each walked the list.
    let mut headers: Vec<(String, String)> = Vec::new();
    let (mut length, mut coded, mut connection) = (None, false, None);
    for line in lines {
        if line.is_empty() {
            continue;
        }
        // A line folded onto the one before (obs-fold) and a name with
        // whitespace before its colon are refused, as RFC 9112 §5.1 and
        // §5.2 have a server do: trimmed, each was read here as a header a
        // server in front may have read as none.
        if line.starts_with([' ', '\t']) {
            return Err(BadRequest(
                400,
                "a header line folded onto the one before".into(),
            ));
        }
        let Some((k, v)) = line.split_once(':') else {
            return Err(BadRequest(400, "malformed header line".into()));
        };
        // A name is a token and a value holds no control character (RFC
        // 9110 §5.1, §5.5), trimmed of SP and HTAB alone (OWS, §5.6.3):
        // `str::trim` took `\x0bchunked`, `chunked\x0c` and `chunked\u{a0}`
        // for `chunked`, and a name ending in `\x0b` was kept as no framing
        // header at all, each a body a server in front frames otherwise.
        if k.is_empty() || !k.bytes().all(tchar) {
            return Err(BadRequest(400, "malformed header line".into()));
        }
        let v = v.trim_matches([' ', '\t']);
        if v.bytes().any(ctl) {
            return Err(BadRequest(
                400,
                "a control character in a header's value".into(),
            ));
        }
        if k.eq_ignore_ascii_case("content-length") {
            // One length, of digits alone (RFC 9110 §8.6): `+5`, which
            // `usize`'s parse took, or a second length unlike the first, is
            // a body another server measures otherwise.
            if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                return Err(BadRequest(400, "invalid Content-Length".into()));
            }
            if length.is_some_and(|at: usize| headers[at].1 != v) {
                return Err(BadRequest(400, "two Content-Lengths that differ".into()));
            }
            length = Some(headers.len());
        } else if k.eq_ignore_ascii_case("transfer-encoding") {
            coded = true;
        } else if k.eq_ignore_ascii_case("connection") && connection.is_none() {
            // The first, as the lookup it replaced found it: a proxy that
            // puts its `close` ahead of a client's `keep-alive` is told so.
            connection = Some(headers.len());
        }
        headers.push((k.to_string(), v.to_string()));
    }

    let body = if coded {
        chunked_framing(&headers, length.is_some(), version)?;
        let mut body = Vec::new();
        read_chunked(reader, max_body, &mut body)?;
        body
    } else {
        let len: usize = match length {
            Some(at) => headers[at]
                .1
                .parse()
                .map_err(|_| BadRequest(400, "invalid Content-Length".into()))?,
            None => 0,
        };
        if len > max_body {
            return Err(BadRequest(
                413,
                format!("the body is {len} bytes, the ceiling is {max_body}"),
            ));
        }
        let mut body = vec![0u8; len];
        if len > 0 {
            reader.read_exact(&mut body).map_err(|e| unread(&e))?;
        }
        body
    };

    // HTTP/1.1 defaults to a persistent connection; 1.0 is the other way round.
    let keep_alive = match connection.map(|at| headers[at].1.as_str()) {
        Some(v) if v.eq_ignore_ascii_case("close") => false,
        Some(v) if v.eq_ignore_ascii_case("keep-alive") => true,
        _ => version.eq_ignore_ascii_case("HTTP/1.1"),
    };

    let (raw_path, raw_query) = target.split_once('?').unwrap_or((target, ""));
    Ok(Some(Request {
        method,
        target: target.to_string(),
        path: percent_decode(raw_path),
        query: parse_query(raw_query),
        headers,
        body,
        keep_alive,
    }))
}

/// The most hex digits a chunk's size is written in: 16 hold any `u64`,
/// and a size past the ceiling is refused all the same.
pub const MAX_SIZE_DIGITS: usize = 16;

/// The longest a chunk's size line is, its extensions and CRLF included.
pub const MAX_CHUNK_LINE: usize = 1024;

/// Whether a request's `Transfer-Encoding` is one this server reads: the
/// one coding `chunked`, in HTTP/1.1, with no `Content-Length` beside it
/// (RFC 9112 §6.1, §6.3). Every other framing is refused, and the
/// connection closed after the refusal as after every refusal of a request,
/// so no byte of a body framed otherwise is read as the next request: a
/// length beside a coding (400) is the shape of request smuggling, the two
/// headers read by two servers two ways; `chunked` not last or twice (400)
/// leaves a body with no end one can read; any other coding (`gzip,
/// chunked`, 501) would be a body handed on still coded; and HTTP/1.0 (400)
/// has no transfer coding at all.
fn chunked_framing(
    headers: &[(String, String)],
    has_length: bool,
    version: &str,
) -> Result<(), BadRequest> {
    if has_length {
        return Err(BadRequest(
            400,
            "a request with both Content-Length and Transfer-Encoding".into(),
        ));
    }
    if !version.eq_ignore_ascii_case("HTTP/1.1") {
        return Err(BadRequest(
            400,
            "Transfer-Encoding in an HTTP/1.0 request".into(),
        ));
    }
    // Every `Transfer-Encoding` line is one list, in order (RFC 9110
    // §5.3), its empty elements passed over (§5.6.1).
    let codings = headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("transfer-encoding"))
        .flat_map(|(_, v)| v.split(','))
        .map(|c| c.trim_matches([' ', '\t']))
        .filter(|c| !c.is_empty());
    let (mut last, mut chunked, mut other) = (None, 0, None);
    for c in codings {
        if c.eq_ignore_ascii_case("chunked") {
            chunked += 1;
        } else {
            other = other.or(Some(c));
        }
        last = Some(c);
    }
    match (last, other) {
        (None, _) => Err(BadRequest(400, "Transfer-Encoding names no coding".into())),
        _ if chunked > 1 => Err(BadRequest(400, "chunked twice in Transfer-Encoding".into())),
        (Some(c), _) if chunked == 1 && !c.eq_ignore_ascii_case("chunked") => Err(BadRequest(
            400,
            "chunked is not the last coding of Transfer-Encoding".into(),
        )),
        (_, Some(c)) => Err(BadRequest(
            501,
            format!("the transfer coding `{c}` is not supported: only chunked is"),
        )),
        (Some(_), None) => Ok(()),
    }
}

/// A chunked body (RFC 9112 §7.1), decoded into one buffer. Every size is
/// held to what is left under `max_body` before a byte of it is allocated,
/// and the buffer grows to `max_body` at most: a chunk past it is refused
/// (413), never cut. A size is 1 to [`MAX_SIZE_DIGITS`] hex digits on a
/// line of at most [`MAX_CHUNK_LINE`] bytes; extensions are read past and
/// trailers read and dropped, the two together at most [`MAX_HEADER_BYTES`]
/// (431) -- a body of one-byte chunks each carrying a kilobyte of extension
/// would have 64 MB of body read 64 GB off the wire (Go's CVE-2023-39326).
/// A chunk's data is read [`read_onto`] the body, which grows as its bytes
/// come, never by the size a line declares. Every line ends in CRLF, the
/// CRLF after each chunk's data is there, and nothing else is: a bare LF or
/// CR, a folded trailer or a size of anything but hex digits is 400. What
/// follows the trailers' empty line is the next request's, left in the
/// reader.
fn read_chunked<R: BufRead>(
    reader: &mut R,
    max_body: usize,
    body: &mut Vec<u8>,
) -> Result<(), BadRequest> {
    let mut spare = MAX_HEADER_BYTES;
    let mut line = Vec::new();
    loop {
        let n = chunk_line(reader, &mut line, MAX_CHUNK_LINE, size_line_too_long)?;
        let content = &line[..n];
        let digits = content.iter().take_while(|b| b.is_ascii_hexdigit()).count();
        if digits == 0 || digits > MAX_SIZE_DIGITS {
            return Err(BadRequest(400, "a malformed chunk size".into()));
        }
        // After the size, nothing, or extensions after whitespace and a
        // `;`: the size `5 ` or `5x` is no size.
        let rest = &content[digits..];
        let ext = &rest[rest
            .iter()
            .take_while(|&&b| b == b' ' || b == b'\t')
            .count()..];
        if !rest.is_empty() && (ext.first() != Some(&b';') || ext.iter().any(|&b| ctl(b))) {
            return Err(BadRequest(400, "a malformed chunk size".into()));
        }
        spare = spare
            .checked_sub(rest.len())
            .ok_or_else(framing_too_large)?;
        let size = content[..digits].iter().fold(0u64, |size, &d| {
            size << 4 | u64::from(hex(d).unwrap_or_default())
        });
        if size == 0 {
            break;
        }
        let at = body.len();
        if size > (max_body - at) as u64 {
            return Err(BadRequest(
                413,
                format!("the chunked body passes the ceiling of {max_body} bytes"),
            ));
        }
        read_onto(reader, body, size as usize, max_body).map_err(|e| unread(&e))?;
        let mut end = [0u8; 2];
        reader.read_exact(&mut end).map_err(|e| unread(&e))?;
        if &end != b"\r\n" {
            return Err(BadRequest(
                400,
                "a chunk's data is not followed by CRLF".into(),
            ));
        }
    }
    // The trailer section: fields, a line each, then an empty line. They
    // are dropped: a trailer is no header, and one naming a token or a
    // length after the body would be a request's head changed after it was
    // read.
    loop {
        let n = chunk_line(reader, &mut line, spare + 2, framing_too_large)?;
        if n == 0 {
            return Ok(());
        }
        spare = spare.checked_sub(n + 2).ok_or_else(framing_too_large)?;
        let content = &line[..n];
        if content[0] == b' ' || content[0] == b'\t' {
            return Err(BadRequest(
                400,
                "a trailer line folded onto the one before".into(),
            ));
        }
        match content.iter().position(|&b| b == b':') {
            Some(at) if at > 0 && content[at - 1] != b' ' && content[at - 1] != b'\t' => {}
            _ => return Err(BadRequest(400, "malformed trailer line".into())),
        }
    }
}

/// The most a body's buffer is grown, and zeroed, ahead of the bytes read
/// into it.
pub(crate) const BODY_STEP: usize = 64 << 10;

/// `n` bytes off `reader` onto the end of `body`, a [`BODY_STEP`] at a time:
/// the buffer grows as the bytes come, its room doubled as a `Vec`'s is but
/// never past `most`, so a 40 MB body under a ceiling of 64 does not reserve
/// 80. Grown by the size declared and zeroed whole before a byte of it came,
/// an 80-byte request ending in `3fffffff` had a node of a 1 GiB ceiling
/// take up a gibibyte a connection (241-371 MB of RSS in the review, from
/// 5), where a `Content-Length` of as much took up none: its
/// `vec![0; len]` is the system's zero pages, each taken up as a byte
/// reaches it. A step is one `read_exact` into the slice, which a
/// `BufReader` past its own buffer reads straight into.
pub(crate) fn read_onto<R: Read>(
    reader: &mut R,
    body: &mut Vec<u8>,
    mut n: usize,
    most: usize,
) -> std::io::Result<()> {
    while n > 0 {
        let step = n.min(BODY_STEP);
        let at = body.len();
        if at + step > body.capacity() {
            let room = (at + step)
                .max(body.capacity() * 2)
                .min(most.max(at + step));
            body.reserve_exact(room - at);
        }
        body.resize(at + step, 0);
        reader.read_exact(&mut body[at..])?;
        n -= step;
    }
    Ok(())
}

/// A token's character (RFC 9110 §5.6.2), what a field's name is made of.
fn tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'#'..=b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
        )
}

/// A line of a chunked body's framing, of at most `budget` bytes, read into
/// `line`: the length of its content before the CRLF it must end in.
fn chunk_line<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
    budget: usize,
    long: fn() -> BadRequest,
) -> Result<usize, BadRequest> {
    line.clear();
    match read_line(reader, line, budget) {
        Ok(_) if line.len() > budget => return Err(long()),
        Ok(_) => {}
        Err(Line::Io(e)) => return Err(unread(&e)),
        Err(Line::Long) => return Err(long()),
    }
    match line.strip_suffix(b"\r\n") {
        Some(content) if !content.contains(&b'\r') => Ok(content.len()),
        Some(_) => Err(BadRequest(400, "a bare CR in a chunked body".into())),
        None if line.ends_with(b"\n") => Err(BadRequest(
            400,
            "a line of a chunked body ends in a bare LF".into(),
        )),
        None => Err(BadRequest(
            400,
            "the chunked body ended before its last chunk".into(),
        )),
    }
}

fn size_line_too_long() -> BadRequest {
    BadRequest(
        400,
        format!("a chunk size line past {MAX_CHUNK_LINE} bytes"),
    )
}

fn framing_too_large() -> BadRequest {
    BadRequest(
        431,
        format!("a chunked body's extensions and trailers past {MAX_HEADER_BYTES} bytes"),
    )
}

/// A control byte a header's value or a chunk extension may not hold (RFC
/// 9110 §5.5, §5.6.2), a tab being whitespace.
fn ctl(b: u8) -> bool {
    (b < 0x20 && b != b'\t') || b == 0x7f
}

/// Why a line could not be read.
enum Line {
    Io(std::io::Error),
    Long,
}

/// The answer to a request cut off part way: 408 when the client went
/// silent, 400 otherwise.
fn unread(e: &std::io::Error) -> BadRequest {
    use std::io::ErrorKind::{TimedOut, WouldBlock};
    match e.kind() {
        WouldBlock | TimedOut => BadRequest(408, "the request was not sent in time".into()),
        _ => BadRequest(400, format!("read error: {e}")),
    }
}

fn read_line<R: BufRead>(reader: &mut R, out: &mut Vec<u8>, budget: usize) -> Result<usize, Line> {
    let mut taken = 0;
    loop {
        let available = reader.fill_buf().map_err(Line::Io)?;
        if available.is_empty() {
            return Ok(taken);
        }
        match available.iter().position(|b| *b == b'\n') {
            Some(i) => {
                out.extend_from_slice(&available[..=i]);
                reader.consume(i + 1);
                return Ok(taken + i + 1);
            }
            None => {
                let n = available.len();
                out.extend_from_slice(available);
                reader.consume(n);
                taken += n;
                if taken > budget {
                    return Err(Line::Long);
                }
            }
        }
    }
}

/// `a=1&b=2` -> `[("a","1"), ("b","2")]`. A valueless key becomes an empty
/// string (`?count` and `?count=` mean the same thing).
pub fn parse_query(raw: &str) -> Vec<(String, String)> {
    raw.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(p), String::new()),
        })
        .collect()
}

/// Decodes percent encoding and `+`. An invalid sequence is left as is.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => match hex(b[i + 1]).zip(hex(b[i + 2])) {
                Some((h, l)) => {
                    out.push(h << 4 | l);
                    i += 3;
                }
                None => {
                    out.push(b[i]);
                    i += 1;
                }
            },
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

// ----------------------------------------------------------------- response

/// The most of an answer's body a connection keeps for its next one.
const SPARE_MAX: usize = 1 << 20;

thread_local! {
    /// The last answer's body, emptied, for the next answer on the same
    /// connection (a connection is a thread). musl's allocator, the
    /// container's, gives a block of tens of KB back to the system when it
    /// is freed: a scan's 56 KB answer was its pages faulted in afresh
    /// each time, 34 page faults a scan of YCSB E in Docker.
    static SPARE: std::cell::Cell<Vec<u8>> = const { std::cell::Cell::new(Vec::new()) };
}

/// An empty buffer for an answer's body: the connection's spare, its room
/// kept.
pub fn spare_body() -> Vec<u8> {
    let mut v = SPARE.take();
    v.clear();
    v
}

/// An answer's body back to the connection once it is sent, unless it is
/// larger than a connection keeps.
pub fn give_back(body: Vec<u8>) {
    if body.capacity() <= SPARE_MAX {
        SPARE.set(body);
    }
}

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: &'static str,
    pub extra: Vec<(String, String)>,
    /// `Fennec-Seq`: the change a write left the database at. A field of its
    /// own rather than an `extra`, which cost every write two allocations
    /// and 2-3% of a put's rate over HTTP.
    pub seq: Option<u64>,
    /// `Fennec-History`: the history `seq` is a change of ([`Fork`]), where
    /// the database has a history of its own. A change number alone names no write once a failover
    /// forks the history: a copy left on the old branch holds a change 3
    /// of its own.
    pub history: Option<Fork>,
    /// `X-Fennecdb-Version`, which every statement's answer carries: a field
    /// as `seq` is, where an `extra` was two allocations an answer.
    pub version: Option<&'static str>,
}

impl Response {
    pub fn json(status: u16, body: impl Into<Vec<u8>>) -> Response {
        Response {
            status,
            body: body.into(),
            content_type: "application/json; charset=utf-8",
            extra: Vec::new(),
            seq: None,
            history: None,
            version: None,
        }
    }

    pub fn error(status: u16, message: &str) -> Response {
        let mut body = String::from("{\"error\":");
        fennec_core::json::escape_into(&mut body, message);
        body.push('}');
        Response::json(status, body)
    }

    /// A refusal to come back from in `secs` seconds: `Retry-After`, and
    /// `{"error", "retry_after"}` with the same seconds, as a quota's 429
    /// says it ([`crate::quota::refusal`]). The body too, since that is
    /// what a replica's sync reads -- a header would take a parameter more
    /// through the C ABI and every binding, and a page on another origin
    /// reads one only where CORS exposes it -- and without it a write sent
    /// during a move went again after the sync's own 250 ms, doubling,
    /// where the node had said a second.
    pub fn retry_after(status: u16, message: &str, secs: u64) -> Response {
        let mut body = String::from("{\"error\":");
        fennec_core::json::escape_into(&mut body, message);
        body.push_str(",\"retry_after\":");
        body.push_str(&secs.to_string());
        body.push('}');
        Response::json(status, body).header("Retry-After", &secs.to_string())
    }

    pub fn empty(status: u16) -> Response {
        Response {
            status,
            body: Vec::new(),
            content_type: "application/json; charset=utf-8",
            extra: Vec::new(),
            seq: None,
            history: None,
            version: None,
        }
    }

    /// With `X-Fennecdb-Version: <version>`.
    pub fn versioned(mut self, version: &'static str) -> Response {
        self.version = Some(version);
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Response {
        self.extra.push((name.to_string(), value.to_string()));
        self
    }

    /// The head and the body in one `writev`. Written apart on a socket
    /// that sends at once (`TCP_NODELAY`), the head went in a packet of its
    /// own and each answer cost two sends, which took the time of all the
    /// rest of a row by id; and `format!` built the head for another eighth.
    pub fn write(
        &self,
        out: &mut impl Write,
        keep_alive: bool,
        head_only: bool,
    ) -> std::io::Result<()> {
        // Room for the id a client may send: past 160 bytes, a head with
        // one made here was a second allocation an answer.
        let mut head = String::with_capacity(256);
        head.push_str("HTTP/1.1 ");
        push_number(&mut head, self.status as u64);
        head.push(' ');
        head.push_str(reason(self.status));
        head.push_str("\r\nContent-Type: ");
        head.push_str(self.content_type);
        head.push_str("\r\nContent-Length: ");
        push_number(&mut head, self.body.len() as u64);
        head.push_str("\r\nConnection: ");
        head.push_str(if keep_alive { "keep-alive" } else { "close" });
        head.push_str("\r\n");
        for (k, v) in &self.extra {
            head.push_str(k);
            head.push_str(": ");
            head.push_str(v);
            head.push_str("\r\n");
        }
        if let Some(v) = self.version {
            head.push_str("X-Fennecdb-Version: ");
            head.push_str(v);
            head.push_str("\r\n");
        }
        if let Some(seq) = self.seq {
            head.push_str("Fennec-Seq: ");
            push_number(&mut head, seq);
            head.push_str("\r\n");
        }
        if let Some(history) = self.history {
            head.push_str("Fennec-History: ");
            push_history(&mut head, history);
            head.push_str("\r\n");
        }
        crate::request_id::with(|id| {
            if !id.is_empty() {
                head.push_str("X-Request-Id: ");
                head.push_str(id);
                head.push_str("\r\n");
            }
        });
        crate::audit::push_verified(&mut head);
        head.push_str("\r\n");
        let body: &[u8] = if head_only { &[] } else { &self.body };
        write_parts(out, head.as_bytes(), body)
    }
}

/// `head` and `body` in one `writev`, as many as it takes, and flushed: the
/// two written apart on a socket that sends at once (`TCP_NODELAY`) went in
/// two packets, a send more an answer -- a node's, and a router's of a
/// node's answer.
pub fn write_parts(out: &mut impl Write, head: &[u8], body: &[u8]) -> std::io::Result<()> {
    let mut parts = [IoSlice::new(head), IoSlice::new(body)];
    let mut parts = &mut parts[..];
    while !parts.is_empty() {
        match out.write_vectored(parts) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(n) => IoSlice::advance_slices(&mut parts, n),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    out.flush()
}

/// A history as `Fennec-History` names it
/// ([`fennec_core::history::History`]): its id -- never the root's 0, the
/// history of a database that never had a replica, which every such
/// database shares and so names nothing -- and its term, the histories its
/// lineage holds, one more at each promotion: of two positions a client
/// was given, the one of the higher term is the later. A `NonZeroU64`, so
/// an `Option` of it costs a [`Response`] 16 bytes, not 24 -- past 128 a
/// `Result` carrying one is too large to return by value cheaply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fork {
    pub term: u32,
    pub id: std::num::NonZeroU64,
}

/// A history as `Fennec-History` writes it: `<term>-<id, 16 hex digits>`.
pub fn push_history(out: &mut String, f: Fork) {
    push_number(out, f.term as u64);
    out.push('-');
    let id = f.id.get();
    for k in (0..16).rev() {
        out.push(char::from(
            b"0123456789abcdef"[(id >> (4 * k)) as usize & 15],
        ));
    }
}

/// The history a `Fennec-History` names.
pub fn parse_history(text: &str) -> Option<Fork> {
    let (term, id) = text.trim().split_once('-')?;
    Some(Fork {
        term: term.parse().ok()?,
        id: std::num::NonZeroU64::new(u64::from_str_radix(id, 16).ok()?)?,
    })
}

/// The history `db` is at, as `Fennec-History` names it: none for the root.
pub fn history_of(db: &fennec_core::engine::Database) -> Option<Fork> {
    let h = db.history();
    let id = std::num::NonZeroU64::new(h.current())?;
    Some(Fork {
        term: h.lineage.len() as u32,
        id,
    })
}

/// `n` in decimal, onto `out`.
/// `n` in decimal onto `out`, with no formatting machinery.
pub fn push_number(out: &mut String, mut n: u64) {
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    loop {
        at -= 1;
        digits[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    // Digits alone: ASCII.
    out.push_str(std::str::from_utf8(&digits[at..]).unwrap_or_default());
}

pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        304 => "Not Modified",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        403 => "Forbidden",
        405 => "Method Not Allowed",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Read};

    #[test]
    fn a_history_reads_back_as_it_was_written() {
        let f = Fork {
            term: 3,
            id: std::num::NonZeroU64::new(0x00ab_0000_0000_00cd).unwrap(),
        };
        let mut out = String::new();
        push_history(&mut out, f);
        assert_eq!(out, "3-00ab0000000000cd");
        assert_eq!(parse_history(&out), Some(f));
        assert_eq!(parse_history(" 3-ab0000000000cd "), Some(f));
        // The root names nothing, and a text that is not one is no history.
        for bad in ["3-0000000000000000", "3", "x-ab", "3-xyz", ""] {
            assert_eq!(parse_history(bad), None, "{bad}");
        }
    }

    #[test]
    fn decodes_percent_and_plus() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("%C3%BCst"), "\u{fc}st");
        // An invalid sequence stays as is.
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn query_keeps_repeats_and_order() {
        let q = parse_query("year=gte.2020&year=lt.2030&count");
        assert_eq!(
            q,
            vec![
                ("year".to_string(), "gte.2020".to_string()),
                ("year".to_string(), "lt.2030".to_string()),
                ("count".to_string(), String::new()),
            ]
        );
    }

    /// A reader handing out one byte a read: every size line, chunk, CRLF
    /// and trailer of a body split across reads.
    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match (self.0.split_first(), buf.first_mut()) {
                (Some((&b, rest)), Some(slot)) => {
                    *slot = b;
                    self.0 = rest;
                    Ok(1)
                }
                _ => Ok(0),
            }
        }
    }

    /// What one request read of `bytes` is, and every request after it
    /// until the bytes end or one is refused -- read whole and a byte a
    /// read, which must read alike.
    fn read_all(bytes: &[u8], max_body: usize) -> Result<Vec<Request>, (u16, String)> {
        fn each<R: BufRead>(mut r: R, max_body: usize) -> Result<Vec<Request>, (u16, String)> {
            let mut all = Vec::new();
            loop {
                match read_request(&mut r, max_body) {
                    Ok(Some(req)) => all.push(req),
                    Ok(None) => return Ok(all),
                    Err(BadRequest(status, why)) => return Err((status, why)),
                }
            }
        }
        let whole = each(bytes, max_body);
        let trickled = each(BufReader::new(Trickle(bytes)), max_body);
        match (&whole, &trickled) {
            (Ok(a), Ok(b)) => {
                assert_eq!(a.len(), b.len());
                for (a, b) in a.iter().zip(b) {
                    assert_eq!(
                        (&a.target, &a.headers, &a.body),
                        (&b.target, &b.headers, &b.body)
                    );
                }
            }
            (Err(a), Err(b)) => assert_eq!(a.0, b.0, "{} / {}", a.1, b.1),
            _ => panic!(
                "read whole, {:?}; a byte a read, {:?}",
                whole.as_ref().err(),
                trickled.as_ref().err()
            ),
        }
        whole
    }

    /// `body` in chunks of `size`, then its last chunk and the empty line.
    fn chunked(body: &[u8], size: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for c in body.chunks(size) {
            out.extend_from_slice(format!("{:x}\r\n", c.len()).as_bytes());
            out.extend_from_slice(c);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n\r\n");
        out
    }

    const CHUNKED: &str = "POST /query HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: chunked\r\n\r\n";
    const NEXT: &[u8] = b"GET /next HTTP/1.1\r\nHost: t\r\n\r\n";

    /// A chunked request read whole: its body, its headers but the coding.
    fn one_chunked(body: &[u8], max_body: usize) -> Result<Request, (u16, String)> {
        let mut bytes = CHUNKED.as_bytes().to_vec();
        bytes.extend_from_slice(body);
        read_all(&bytes, max_body).map(|mut all| all.remove(0))
    }

    #[test]
    fn a_chunked_body_reads_as_its_length_would_and_the_next_request_after_it() {
        let data: Vec<u8> = (0..100_000u32).map(|i| (i * 7 % 251) as u8).collect();
        for len in [0, 1, 7, 8191, 8192, 8193, 100_000] {
            for size in [1, 3, 4096, 65_536] {
                if size == 1 && len > 10_000 {
                    continue;
                }
                let body = &data[..len];
                let mut bytes = CHUNKED.as_bytes().to_vec();
                bytes.extend_from_slice(&chunked(body, size));
                bytes.extend_from_slice(NEXT);
                let all = read_all(&bytes, 1 << 20).unwrap();
                assert_eq!(all.len(), 2, "{len} in chunks of {size}");
                assert_eq!(all[0].body, body, "{len} in chunks of {size}");
                assert!(all[0].keep_alive);
                assert_eq!(all[1].path, "/next");
                assert!(all[1].body.is_empty());

                let mut plain =
                    format!("POST /query HTTP/1.1\r\nHost: t\r\nContent-Length: {len}\r\n\r\n")
                        .into_bytes();
                plain.extend_from_slice(body);
                assert_eq!(read_all(&plain, 1 << 20).unwrap()[0].body, all[0].body);
            }
        }
    }

    #[test]
    fn extensions_and_trailers_are_read_past_and_dropped() {
        let mut bytes = CHUNKED.as_bytes().to_vec();
        bytes.extend_from_slice(
            b"5;name=value;q=\"a b\"\r\nhello\r\n1 \t; x\r\n!\r\n\
              0000000000000000;last\r\nChecksum: abc\r\nAuthorization: Bearer late\r\n\r\n",
        );
        bytes.extend_from_slice(NEXT);
        let all = read_all(&bytes, 1 << 20).unwrap();
        assert_eq!(all[0].body, b"hello!");
        assert_eq!(
            all[0].header("authorization"),
            None,
            "a trailer is no header"
        );
        assert_eq!(all[0].header("checksum"), None);
        assert_eq!(all[1].path, "/next");
    }

    #[test]
    fn a_chunk_past_the_ceiling_is_refused_before_it_is_allocated() {
        // The largest size there is, and no byte of it sent: allocated, it
        // aborts the process; read for, it is the body ending early (400).
        let r = one_chunked(b"ffffffffffffffff\r\n", 64 << 20);
        assert_eq!(r.err().unwrap().0, 413);
        // Chunks within the ceiling one by one, past it together.
        let r = one_chunked(b"5\r\nhello\r\n6\r\nworld!\r\n0\r\n\r\n", 10);
        assert_eq!(r.err().unwrap().0, 413);
        // At it, read whole, and the buffer never grown past it.
        let r = one_chunked(b"5\r\nhello\r\n5\r\nworld\r\n0\r\n\r\n", 10).unwrap();
        assert_eq!(r.body, b"helloworld");
        let body = vec![b'x'; 90];
        let r = one_chunked(&chunked(&body, 30), 100).unwrap();
        assert_eq!(r.body, body);
        assert!(r.body.capacity() <= 100, "{}", r.body.capacity());
    }

    #[test]
    fn a_declared_chunk_grows_the_body_only_as_its_bytes_come() {
        // The review's request: a gibibyte declared under a ceiling of one,
        // ten bytes of it sent, then the end. Zeroed whole as declared, the
        // buffer was the gibibyte; grown as the bytes come, it is a step.
        let mut body = Vec::new();
        let r = read_chunked(&mut &b"3fffffff\r\n0123456789"[..], 1 << 30, &mut body);
        assert_eq!(r.err().map(|e| e.0), Some(400));
        assert!(body.capacity() <= BODY_STEP, "{}", body.capacity());
        // Bytes that come grow it, twice them and a step at most.
        let sent = 3 * BODY_STEP + 5;
        let mut bytes = b"3fffffff\r\n".to_vec();
        bytes.resize(bytes.len() + sent, b'x');
        let mut body = Vec::new();
        let r = read_chunked(&mut &bytes[..], 1 << 30, &mut body);
        assert_eq!(r.err().map(|e| e.0), Some(400));
        assert!(
            body.capacity() <= 2 * sent + BODY_STEP,
            "{}",
            body.capacity()
        );
        // A chunk of several steps reads whole, a byte a read too.
        let data: Vec<u8> = (0..sent as u32).map(|i| (i * 7 % 251) as u8).collect();
        assert_eq!(
            one_chunked(&chunked(&data, sent), 1 << 20).unwrap().body,
            data
        );
    }

    #[test]
    fn a_header_disguised_past_sp_and_htab_is_refused() {
        // `str::trim` read every value here as the one it hides, and every
        // name here was kept as a header of no meaning: a framing a server
        // in front reads otherwise. Each request is refused before its body.
        let hello = "\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        let cases: &[(&str, u16)] = &[
            ("Transfer-Encoding: \x0bchunked", 400),
            ("Transfer-Encoding: chunked\x0c", 400),
            ("Transfer-Encoding: chunked\x00", 400),
            ("Transfer-Encoding: chunked\u{a0}", 501),
            ("Transfer-Encoding: \u{85}chunked", 501),
            ("Transfer-Encoding: chunked,\u{3000}", 400),
            ("Transfer-Encoding\x0b: chunked", 400),
            ("Transfer\x00-Encoding: chunked", 400),
            ("Content-Length: \x0b5", 400),
            ("Content-Length: 5\u{a0}", 400),
            ("Content-Length\x0c: 5", 400),
            ("X: a\x01b", 400),
            ("X: a\x7f", 400),
            ("X(: a", 400),
            ("X\u{e9}: a", 400),
        ];
        for (header, status) in cases {
            let bytes = format!("POST / HTTP/1.1\r\nHost: t\r\n{header}{hello}");
            let first = read_request(&mut bytes.as_bytes(), 1 << 20);
            assert_eq!(first.err().map(|e| e.0), Some(*status), "{header:?}");
        }
        // SP and HTAB are trimmed and kept inside a value; obs-text is a
        // value's to hold.
        let r = read_all(
            format!(
                "POST / HTTP/1.1\r\nX: \t a \t b\t \r\nY: caf\u{e9}\r\n\
                 Transfer-Encoding: \t chunked \t{hello}"
            )
            .as_bytes(),
            1 << 20,
        )
        .unwrap();
        assert_eq!(r[0].header("x"), Some("a \t b"));
        assert_eq!(r[0].header("y"), Some("caf\u{e9}"));
        assert_eq!(r[0].body, b"hello");
    }

    #[test]
    fn framings_another_server_would_read_otherwise_are_refused() {
        let hello = "\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        let cases: &[(&str, u16)] = &[
            (
                "POST / HTTP/1.1\r\nContent-Length: 5\r\nTransfer-Encoding: chunked",
                400,
            ),
            (
                "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\nContent-Length: 5",
                400,
            ),
            ("POST / HTTP/1.1\r\nTransfer-Encoding: gzip, chunked", 501),
            (
                "POST / HTTP/1.1\r\nTransfer-Encoding: gzip\r\nTransfer-Encoding: chunked",
                501,
            ),
            ("POST / HTTP/1.1\r\nTransfer-Encoding: chunked, gzip", 400),
            (
                "POST / HTTP/1.1\r\nTransfer-Encoding: chunked, chunked",
                400,
            ),
            (
                "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: chunked",
                400,
            ),
            ("POST / HTTP/1.1\r\nTransfer-Encoding: gzip", 501),
            ("POST / HTTP/1.1\r\nTransfer-Encoding: identity", 501),
            ("POST / HTTP/1.1\r\nTransfer-Encoding: chunked;q=1", 501),
            ("POST / HTTP/1.1\r\nTransfer-Encoding: ", 400),
            ("POST / HTTP/1.0\r\nTransfer-Encoding: chunked", 400),
            // A coding a server in front could miss: a bare LF or CR, a
            // folded line, whitespace before the colon.
            ("POST / HTTP/1.1\nTransfer-Encoding: chunked", 400),
            ("POST / HTTP/1.1\r\nX: a\nTransfer-Encoding: chunked", 400),
            ("POST / HTTP/1.1\r\nX: a\rTransfer-Encoding: chunked", 400),
            (
                "POST / HTTP/1.1\r\nX: a\r\n Transfer-Encoding: chunked",
                400,
            ),
            (
                "POST / HTTP/1.1\r\nX: a\r\n\tTransfer-Encoding: chunked",
                400,
            ),
            ("POST / HTTP/1.1\r\nTransfer-Encoding : chunked", 400),
            // A length another server measures otherwise.
            ("POST / HTTP/1.1\r\nContent-Length: +5", 400),
            (
                "POST / HTTP/1.1\r\nContent-Length: 5\r\nContent-Length: 6",
                400,
            ),
            ("POST / HTTP/1.1\r\nContent-Length: 5, 5", 400),
        ];
        for (head, status) in cases {
            let r = read_all(format!("{head}{hello}").as_bytes(), 1 << 20);
            assert_eq!(r.err().map(|e| e.0), Some(*status), "{head:?}");
        }
        // The end of the head in a bare LF, after a line that ended right.
        let r = read_all(b"GET / HTTP/1.1\r\nHost: t\r\n\n", 1 << 20);
        assert_eq!(r.err().map(|e| e.0), Some(400));
        // One length said twice alike is one length.
        let r = read_all(
            b"POST / HTTP/1.1\r\nContent-Length: 5\r\nContent-Length: 5\r\n\r\nhello",
            1 << 20,
        );
        assert_eq!(r.unwrap()[0].body, b"hello");
        // Of two `Connection`s, the first is read.
        let r = read_all(
            b"GET / HTTP/1.1\r\nConnection: close\r\nConnection: keep-alive\r\n\r\n",
            1 << 20,
        );
        assert!(!r.unwrap()[0].keep_alive);
        // Empty elements of the list are passed over.
        let r = read_all(
            format!("POST / HTTP/1.1\r\nTransfer-Encoding: , chunked{hello}").as_bytes(),
            1 << 20,
        );
        assert_eq!(r.unwrap()[0].body, b"hello");
    }

    #[test]
    fn a_malformed_chunked_body_is_refused() {
        let long_ext = format!("5;{}\r\nhello\r\n0\r\n\r\n", "e".repeat(MAX_CHUNK_LINE));
        let extensions = "1;".to_string() + &"e".repeat(1000) + "\r\nx\r\n";
        let extensions = extensions.repeat(70) + "0\r\n\r\n";
        let trailers =
            "0\r\n".to_string() + &format!("A: {}\r\n", "v".repeat(1000)).repeat(70) + "\r\n";
        let cases: &[(&str, u16)] = &[
            ("5\nhello\r\n0\r\n\r\n", 400),
            ("5\r\nhello\n0\r\n\r\n", 400),
            ("5\r\nhelloX\r\n0\r\n\r\n", 400),
            ("5\r\nhello\r\n0\r\n\n", 400),
            ("5\r;x\r\nhello\r\n0\r\n\r\n", 400),
            ("x\r\n", 400),
            ("\r\n", 400),
            ("-1\r\n", 400),
            ("0x5\r\nhello\r\n0\r\n\r\n", 400),
            ("5 \r\nhello\r\n0\r\n\r\n", 400),
            (" 5\r\nhello\r\n0\r\n\r\n", 400),
            ("5;a\x01\r\nhello\r\n0\r\n\r\n", 400),
            // Seventeen digits; sixteen are a size.
            ("00000000000000005\r\nhello\r\n0\r\n\r\n", 400),
            (&long_ext, 400),
            // Ended early: in a chunk, before the last, in the trailers.
            ("5\r\nhel", 400),
            ("5\r\nhello\r\n", 400),
            ("0\r\n", 400),
            ("0\r\nA: 1\r\n folded\r\n\r\n", 400),
            ("0\r\nno colon\r\n\r\n", 400),
            ("0\r\nA : 1\r\n\r\n", 400),
            ("0\r\nA: 1\n\r\n", 400),
            (&extensions, 431),
            (&trailers, 431),
        ];
        for (body, status) in cases {
            let r = one_chunked(body.as_bytes(), 1 << 20);
            assert_eq!(r.err().map(|e| e.0), Some(*status), "{body:?}");
        }
        let r = one_chunked(b"0000000000000005\r\nhello\r\n0\r\n\r\n", 1 << 20);
        assert_eq!(r.unwrap().body, b"hello");
    }
}

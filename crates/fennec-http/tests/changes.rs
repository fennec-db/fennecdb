//! The subscription endpoint (`GET /<name>/changes`) and the batch endpoint
//! (`POST /batch`).
//!
//! The client is raw TCP: the assertions are on the real SSE byte stream,
//! not on a library's leniency.

use fennec_core::prelude::*;
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

fn seeded() -> Database {
    let mut db = Database::new();
    for sql in [
        "create collection tasks (key text @hash, title text, status text @hash)",
        r#"put tasks [
             {key: "a", title: "one",   status: "open"},
             {key: "b", title: "two",   status: "open"},
             {key: "c", title: "three", status: "closed"}
           ]"#,
    ] {
        for stmt in fennec_ql::parse(sql).expect("parse") {
            db.execute(&stmt).expect("execute");
        }
    }
    db
}

struct Harness {
    port: u16,
    db: Arc<RwLock<Database>>,
    server: Arc<Server>,
}

fn start(cfg: Config) -> Harness {
    start_over(seeded(), cfg)
}

fn start_over(db: Database, mut cfg: Config) -> Harness {
    // We do not want to wait for a keep-alive line in the tests.
    cfg.stream_keepalive = Duration::from_millis(80);
    serve(db, cfg)
}

/// [`start_over`] keeping the keep-alive `cfg` names: a stream that looks
/// at a keep-alive every 80 ms would hide one that never wakes otherwise.
fn serve(db: Database, mut cfg: Config) -> Harness {
    cfg.addr = "127.0.0.1:0".into();
    let db = Arc::new(RwLock::new(db));
    let server = Arc::new(Server::new(Arc::clone(&db), cfg));
    let listener = server.bind().expect("bind");
    let port = listener.local_addr().unwrap().port();
    let serving = Arc::clone(&server);
    std::thread::spawn(move || {
        let _ = serving.serve_on(listener);
    });
    Harness { port, db, server }
}

// ------------------------------------------------------------------- SSE

struct Sub {
    sock: TcpStream,
    buf: String,
    status: u16,
    head: String,
}

impl Sub {
    fn open(port: u16, target: &str) -> Sub {
        Sub::open_with(port, target, &[])
    }

    fn open_with(port: u16, target: &str, headers: &[(&str, &str)]) -> Sub {
        let mut sock = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        sock.set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut req =
            format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n");
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        sock.write_all(req.as_bytes()).unwrap();

        let mut s = Sub {
            sock,
            buf: String::new(),
            status: 0,
            head: String::new(),
        };
        let head = s
            .read_until("\r\n\r\n", Duration::from_secs(3))
            .expect("header");
        s.status = head
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        s.head = head;
        s
    }

    fn head_line(&self) -> &str {
        &self.head
    }

    fn read_until(&mut self, sep: &str, budget: Duration) -> Option<String> {
        let deadline = Instant::now() + budget;
        loop {
            if let Some(i) = self.buf.find(sep) {
                let out = self.buf[..i].to_string();
                self.buf = self.buf[i + sep.len()..].to_string();
                return Some(out);
            }
            if Instant::now() > deadline {
                return None;
            }
            let mut chunk = [0u8; 4096];
            match self.sock.read(&mut chunk) {
                Ok(0) => return None,
                Ok(n) => self.buf.push_str(&String::from_utf8_lossy(&chunk[..n])),
                Err(_) => continue, // read timeout: keep trying until the budget runs out
            }
        }
    }

    /// The keep-alive comments heard within `budget`, `n` at most; events
    /// between them are passed over.
    fn keepalives(&mut self, n: usize, budget: Duration) -> usize {
        let deadline = Instant::now() + budget;
        let mut heard = 0;
        while heard < n {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(block) = self.read_until("\n\n", left) else {
                break;
            };
            if block.trim_start_matches(['\r', '\n']).starts_with(':') {
                heard += 1;
            }
        }
        heard
    }

    /// The next event; keep-alive comments are skipped.
    fn next(&mut self) -> Option<(String, String)> {
        self.next_within(Duration::from_secs(3))
    }

    fn next_within(&mut self, budget: Duration) -> Option<(String, String)> {
        let deadline = Instant::now() + budget;
        loop {
            let block =
                self.read_until("\n\n", deadline.saturating_duration_since(Instant::now()))?;
            let block = block.trim_start_matches(['\r', '\n']);
            if block.starts_with(':') {
                continue; // keep-alive
            }
            let mut name = String::new();
            let mut data = String::new();
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("event: ") {
                    name = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("data: ") {
                    data = v.trim().to_string();
                }
            }
            if !name.is_empty() {
                return Some((name, data));
            }
        }
    }
}

fn post(port: u16, target: &str, body: &str) -> (u16, String) {
    call(port, "POST", target, Some(body), &[])
}

fn call(
    port: u16,
    method: &str,
    target: &str,
    body: Option<&str>,
    headers: &[(&str, &str)],
) -> (u16, String) {
    let mut sock = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut req = format!("{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    match body {
        Some(b) => req.push_str(&format!("Content-Length: {}\r\n\r\n{b}", b.len())),
        None => req.push_str("\r\n"),
    }
    sock.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = sock.read_to_string(&mut out);
    let (head, tail) = out.split_once("\r\n\r\n").unwrap_or((out.as_str(), ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, tail.to_string())
}

/// Number of elements of a JSON array field inside `data`. It counts the
/// top-level commas; nested objects, arrays and strings are skipped.
fn count(data: &str, key: &str) -> usize {
    let pat = format!("\"{key}\":[");
    let at = data
        .find(&pat)
        .unwrap_or_else(|| panic!("`{key}` missing: {data}"));
    // *After* the opening bracket: is the array empty, and if not how many items.
    let body = &data[at + pat.len()..];
    let mut depth = 0i32;
    let mut commas = 0usize;
    let mut any = false;
    let mut in_str = false;
    let mut esc = false;
    for c in body.chars() {
        if esc {
            esc = false;
            continue;
        }
        if in_str {
            match c {
                '\\' => esc = true,
                '"' => in_str = false,
                _ => {}
            }
            any = true;
            continue;
        }
        match c {
            ']' | '}' if depth == 0 => break,
            '"' => {
                in_str = true;
                any = true;
            }
            '[' | '{' => {
                depth += 1;
                any = true;
            }
            ']' | '}' => depth -= 1,
            ',' if depth == 0 => commas += 1,
            c if !c.is_whitespace() => any = true,
            _ => {}
        }
    }
    if any {
        commas + 1
    } else {
        0
    }
}

// ------------------------------------------------------------------ tests

#[test]
fn seed_carries_the_whole_shape() {
    let h = start(Config::default());
    let mut s = Sub::open(h.port, "/tasks/changes");
    assert_eq!(s.status, 200);
    assert!(
        s.head_line().contains("text/event-stream"),
        "{}",
        s.head_line()
    );
    let (name, data) = s.next().expect("event");
    assert_eq!(name, "seed");
    assert_eq!(count(&data, "rows"), 3);
}

/// The head names where the database stood as the stream opened
/// (`Fennec-Seq`), and a database with no history of its own no history, as
/// every answer does; under `--http-cors` it carries every answer's CORS
/// headers, so a page on another origin reads them.
#[test]
fn the_head_names_where_the_stream_opened() {
    let h = start(Config {
        cors: Some("*".into()),
        ..Config::default()
    });
    let at = h.db.read().unwrap().change_seq();
    let s = Sub::open(h.port, "/tasks/changes");
    assert_eq!(s.status, 200);
    let head = s.head_line().to_ascii_lowercase();
    assert!(head.contains(&format!("fennec-seq: {at}\r\n")), "{head}");
    assert!(!head.contains("\r\nfennec-history:"), "{head}");
    assert!(
        head.contains("access-control-expose-headers: fennec-seq, fennec-history"),
        "{head}"
    );
}

#[test]
fn shape_filter_narrows_the_seed() {
    let h = start(Config::default());
    let mut s = Sub::open(h.port, "/tasks/changes?status=eq.open");
    let (name, data) = s.next().expect("event");
    assert_eq!(name, "seed");
    assert_eq!(count(&data, "rows"), 2);
}

#[test]
fn a_write_arrives_as_a_change() {
    let h = start(Config::default());
    let mut s = Sub::open(h.port, "/tasks/changes");
    assert_eq!(s.next().expect("seed").0, "seed");

    let (st, _) = post(
        h.port,
        "/tasks",
        r#"{"key":"d","title":"four","status":"open"}"#,
    );
    assert_eq!(st, 201);

    let (name, data) = s.next().expect("change");
    assert_eq!(name, "change");
    assert_eq!(count(&data, "puts"), 1);
    assert!(data.contains("\"dels\":[]"), "{data}");
    assert!(data.contains("four"), "{data}");
}

#[test]
fn leaving_the_shape_arrives_as_a_delete() {
    let h = start(Config::default());
    let mut s = Sub::open(h.port, "/tasks/changes?status=eq.open");
    assert_eq!(s.next().expect("seed").0, "seed");

    let (st, _) = call(
        h.port,
        "PATCH",
        "/tasks?key=eq.a",
        Some(r#"{"status":"closed"}"#),
        &[],
    );
    assert_eq!(st, 200);

    let (name, data) = s.next().expect("change");
    assert_eq!(name, "change");
    assert_eq!(count(&data, "puts"), 0, "{data}");
    assert!(
        !data.contains("\"dels\":[]"),
        "leaving the shape must be a delete: {data}"
    );
}

#[test]
fn a_write_outside_the_shape_is_not_pushed() {
    let h = start(Config::default());
    let mut s = Sub::open(h.port, "/tasks/changes?status=eq.closed");
    assert_eq!(s.next().expect("seed").0, "seed");

    // A write to another collection: the subscriber wakes but no empty batch is sent.
    let (st, _) = post(
        h.port,
        "/query",
        r#"{"query":"create collection notes (text text)"}"#,
    );
    assert_eq!(st, 200);
    // Only a schema event should arrive (another collection -> nothing).
    assert!(
        s.next_within(Duration::from_millis(500)).is_none(),
        "an unrelated write must not reach the subscriber"
    );
}

#[test]
fn resume_from_cursor_skips_the_seed() {
    let h = start(Config::default());
    let cursor = h.db.read().unwrap().change_seq();
    let mut s = Sub::open(h.port, &format!("/tasks/changes?since={cursor}"));
    assert_eq!(s.status, 200);

    post(
        h.port,
        "/tasks",
        r#"{"key":"d","title":"four","status":"open"}"#,
    );
    let (name, data) = s.next().expect("event");
    assert_eq!(name, "change", "a resuming subscriber must not get a seed");
    assert_eq!(count(&data, "puts"), 1);
}

#[test]
fn a_stale_cursor_is_reseeded() {
    let cfg = Config {
        change_capacity: 2,
        ..Config::default()
    };
    let h = start(cfg);

    // Overflow the ring.
    for k in ["x", "y", "z"] {
        post(
            h.port,
            "/tasks",
            &format!(r#"{{"key":"{k}","title":"{k}","status":"open"}}"#),
        );
    }
    let mut s = Sub::open(h.port, "/tasks/changes?since=1");
    let (name, data) = s.next().expect("event");
    assert_eq!(name, "seed", "a stale cursor must be reseeded");
    assert_eq!(count(&data, "rows"), 6);
}

#[test]
fn window_clauses_are_rejected() {
    let h = start(Config::default());
    for target in [
        "/tasks/changes?limit=10",
        "/tasks/changes?order=title.asc",
        "/tasks/changes?offset=1",
        "/tasks/changes?count",
    ] {
        let s = Sub::open(h.port, target);
        assert_eq!(s.status, 400, "{target}");
    }
}

#[test]
fn unknown_collection_is_404() {
    let h = start(Config::default());
    let s = Sub::open(h.port, "/nosuch/changes");
    assert_eq!(s.status, 404);
}

#[test]
fn projection_is_honoured_and_keeps_id() {
    let h = start(Config::default());
    let mut s = Sub::open(h.port, "/tasks/changes?select=title");
    let (_, data) = s.next().expect("seed");
    assert!(
        data.contains("\"id\""),
        "the id must always be carried: {data}"
    );
    assert!(!data.contains("\"status\""), "{data}");
}

#[test]
fn the_stream_needs_a_token_too() {
    let cfg = Config {
        token: Some("secret".into()),
        ..Config::default()
    };
    let h = start(cfg);

    assert_eq!(Sub::open(h.port, "/tasks/changes").status, 401);
    let s = Sub::open_with(
        h.port,
        "/tasks/changes",
        &[("Authorization", "Bearer secret")],
    );
    assert_eq!(s.status, 200);
}

#[test]
fn stream_cap_is_separate_from_connection_cap() {
    let cfg = Config {
        max_streams: 1,
        ..Config::default()
    };
    let h = start(cfg);

    let mut first = Sub::open(h.port, "/tasks/changes");
    assert_eq!(first.next().expect("seed").0, "seed");
    let mut second = Sub::open(h.port, "/tasks/changes");
    assert_eq!(second.status, 503);
    // Come back in a second, in the head and in the body a replica's sync
    // reads the wait from.
    assert!(
        second.head_line().contains("Retry-After: 1"),
        "{}",
        second.head
    );
    // Answered whole, as every route's refusal: its length said, every
    // header it carries written.
    assert!(
        second.head_line().contains("Content-Length: "),
        "{}",
        second.head
    );
    let said = second
        .read_until("}", Duration::from_secs(3))
        .unwrap_or_default();
    assert!(said.ends_with(r#""retry_after":1"#), "{said}");

    // Ordinary requests are unaffected.
    let (st, _) = call(h.port, "GET", "/tasks", None, &[]);
    assert_eq!(st, 200);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// The ids a change event names as deleted.
fn dels(data: &str) -> Vec<i64> {
    let at = data.find("\"dels\":[").expect("dels") + "\"dels\":[".len();
    let end = data[at..].find(']').unwrap() + at;
    data[at..end]
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.trim().parse().unwrap())
        .collect()
}

/// A row reaching its `@ttl` is told to a subscriber as a deletion at its
/// time, with no write to tell of it: left to the sweep, a minute and its
/// grace later, a courier whose phone went quiet stayed on a dispatcher's
/// map 73 s after it had gone from every read. Its time is asserted from
/// below; that the sweep had not deleted it, by the row still read as
/// expired once the deletion was heard. A shape that holds nothing --
/// what `FennecHttp.live` opens -- is told a change happened, for each row.
#[test]
fn a_row_reaching_its_ttl_is_told_as_a_deletion_at_its_time() {
    let mut db = Database::new();
    for stmt in
        fennec_ql::parse("create collection positions (courier text, seen timestamp @ttl(1000ms))")
            .unwrap()
    {
        db.execute(&stmt).unwrap();
    }
    let h = start_over(db, Config::default());
    let now = now_ms();
    // Gone in 300 ms, and in a second.
    let quiet_at = now - 700 + 1000;
    let busy_at = now + 1000;
    let (st, body) = post(
        h.port,
        "/query",
        &format!(
            r#"{{"query":"put positions [{{courier: \"quiet\", seen: $1}}, {{courier: \"busy\", seen: $2}}]","params":[{},{}]}}"#,
            now - 700,
            now
        ),
    );
    assert_eq!(st, 200, "{body}");
    let id_of = |courier: &str| {
        let guard = h.db.read().unwrap();
        let stmt = fennec_ql::parse_one(&format!(
            "get positions select id where courier = \"{courier}\""
        ))
        .unwrap();
        match guard.query(&stmt, &[]).unwrap() {
            Response::Rows(rs) => rs.rows[0].id as i64,
            other => panic!("{other:?}"),
        }
    };
    let (quiet, busy) = (id_of("quiet"), id_of("busy"));
    let mut s = Sub::open(h.port, "/positions/changes");
    let mut probe = Sub::open(h.port, "/positions/changes?select=id&where=false");
    let (name, data) = s.next().expect("seed");
    assert_eq!((name.as_str(), count(&data, "rows")), ("seed", 2), "{data}");
    assert_eq!(probe.next().expect("seed").0, "seed");

    for (id, at) in [(quiet, quiet_at), (busy, busy_at)] {
        let (name, data) = s.next_within(Duration::from_secs(10)).expect("a change");
        let heard = now_ms();
        assert_eq!(name, "change", "{data}");
        assert_eq!(dels(&data), vec![id], "{data}");
        assert_eq!(count(&data, "puts"), 0, "{data}");
        assert!(heard >= at, "told {} ms before its time", at - heard);
        eprintln!("expiry told {} ms after its time", heard - at);
        // Out of every read, and still in the file: the sweep did not
        // delete it.
        let guard = h.db.read().unwrap();
        let read = |q: &str| match guard.query(&fennec_ql::parse_one(q).unwrap(), &[]).unwrap() {
            Response::Rows(rs) => rs.rows.iter().map(|r| r.id as i64).collect::<Vec<_>>(),
            other => panic!("{other:?}"),
        };
        assert!(!read("get positions select id").contains(&id));
        assert!(read("get positions select id where expired()").contains(&id));
        drop(guard);
        let (name, data) = probe
            .next_within(Duration::from_secs(10))
            .expect("a change");
        assert_eq!(name, "change", "{data}");
    }
}

/// A stream's shape runs over no row past its time but those reaching it
/// since its last look, and its seed over none: `@ttl`'s window, and a
/// read's `alive`, are asked before the shape's terms. Asked after, a term
/// failing by a field -- `1 / n` over a row's `n = 0` -- ran over a row
/// long past its time and not yet swept, the stream's `error` telling what
/// it held, as a scoped token's term would without the `expired` grant.
/// The window holds most of the collection, so the look scans rather than
/// read its range of the index, and meets the old row first.
#[test]
fn a_stream_s_shape_never_runs_over_a_row_long_past_its_time() {
    let mut db = Database::new();
    let create = "create collection positions (n int, seen timestamp @ttl(1000ms))";
    db.execute(&fennec_ql::parse_one(create).unwrap()).unwrap();
    let h = start_over(db, Config::default());
    let now = now_ms();
    // Row 1 went 500 ms ago; rows 2 to 4 go in 600 ms.
    let (st, body) = post(
        h.port,
        "/query",
        &format!(
            r#"{{"query":"put positions [{{n: 0, seen: $1}}, {{n: 1, seen: $2}}, {{n: 2, seen: $2}}, {{n: 3, seen: $2}}]","params":[{},{}]}}"#,
            now - 1500,
            now - 400
        ),
    );
    assert_eq!(st, 200, "{body}");
    let mut s = Sub::open(h.port, "/positions/changes?where=1+%2F+n+%3E%3D+0");
    let (name, data) = s.next().expect("seed");
    assert_eq!((name.as_str(), count(&data, "rows")), ("seed", 3), "{data}");
    let mut told = Vec::new();
    while told.len() < 3 {
        let (name, data) = s.next_within(Duration::from_secs(10)).expect("a change");
        assert_eq!(name, "change", "{data}");
        told.extend(dels(&data));
    }
    told.sort_unstable();
    assert_eq!(told, [2, 3, 4]);
}

// ------------------------------------------------------------------- batch

#[test]
fn batch_runs_every_statement() {
    let h = start(Config::default());
    let body = concat!(
        r#"{"query":"put tasks {key: $1, title: $2, status: $3}","params":["d","four","open"]}"#,
        "\n",
        r#"{"query":"set tasks {title: $1} where key = $2","params":["ONE","a"]}"#,
    );
    let (st, out) = post(h.port, "/batch", body);
    assert_eq!(st, 200, "{out}");
    assert!(out.contains("\"ok\":2"), "{out}");

    let (_, rows) = call(h.port, "GET", "/tasks?key=eq.a", None, &[]);
    assert!(rows.contains("ONE"), "{rows}");
}

#[test]
fn a_batch_that_fails_lands_none_of_it() {
    let h = start(Config::default());
    let body = concat!(
        r#"{"query":"put tasks {key: $1, title: $2, status: $3}","params":["d","four","open"]}"#,
        "\n",
        r#"{"query":"put tasks {nofield: $1}","params":[1]}"#,
        "\n",
        r#"{"query":"put tasks {key: $1, title: $2, status: $3}","params":["e","five","open"]}"#,
    );
    let (st, out) = post(h.port, "/batch", body);
    assert_eq!(st, 404, "{out}");
    assert!(out.contains("\"completed\":0"), "{out}");

    // A batch is one block: the first is put back, and the third never ran.
    let (_, rows) = call(h.port, "GET", "/tasks?key=eq.d", None, &[]);
    assert_eq!(rows.trim(), "[]");
    let (_, rows) = call(h.port, "GET", "/tasks?key=eq.e", None, &[]);
    assert_eq!(rows.trim(), "[]");
}

/// A create, a drop or a create index is put back as a write is, so a
/// batch holding one is one block too: none of it lands. A batch holding a
/// compact runs each statement on its own, as it always did: what ran
/// before the error stays, and the answer says how much that was.
#[test]
fn a_batch_with_a_schema_change_lands_none_of_it() {
    let h = start(Config::default());
    let body = concat!(
        r#"{"query":"create collection notes (body text)"}"#,
        "\n",
        r#"{"query":"put notes {body: $1}","params":["gone"]}"#,
        "\n",
        r#"{"query":"create index on tasks (title) @hash"}"#,
        "\n",
        r#"{"query":"put tasks {nofield: $1}","params":[1]}"#,
        "\n",
        r#"{"query":"put notes {body: $1}","params":["never"]}"#,
    );
    let (st, out) = post(h.port, "/batch", body);
    assert_eq!(st, 404, "{out}");
    assert!(out.contains("\"completed\":0"), "{out}");
    let (st, rows) = call(h.port, "GET", "/notes", None, &[]);
    assert_eq!(st, 404, "the collection outlived its batch: {rows}");
    // The index went with it, and is built again.
    let (st, out) = post(
        h.port,
        "/batch",
        r#"{"query":"create index on tasks (title) @hash"}"#,
    );
    assert_eq!(st, 200, "{out}");

    let body = concat!(
        r#"{"query":"put tasks {key: $1, title: $2, status: $3}","params":["k","kept","open"]}"#,
        "\n",
        r#"{"query":"compact"}"#,
        "\n",
        r#"{"query":"put tasks {nofield: $1}","params":[1]}"#,
    );
    let (st, out) = post(h.port, "/batch", body);
    assert_eq!(st, 404, "{out}");
    assert!(out.contains("\"completed\":2"), "{out}");
    let (_, rows) = call(h.port, "GET", "/tasks?key=eq.k", None, &[]);
    assert!(rows.contains("kept"), "{rows}");
}

#[test]
fn batch_respects_read_only() {
    let cfg = Config {
        read_only: true,
        ..Config::default()
    };
    let h = start(cfg);
    let (st, _) = post(
        h.port,
        "/batch",
        r#"{"query":"put tasks {key: $1}","params":["z"]}"#,
    );
    assert_eq!(st, 403);
}

#[test]
fn empty_batch_is_rejected() {
    let h = start(Config::default());
    let (st, out) = post(h.port, "/batch", "\n\n");
    assert_eq!(st, 400, "{out}");
}

#[test]
fn batch_wakes_subscribers_once_per_statement() {
    let h = start(Config::default());
    let mut s = Sub::open(h.port, "/tasks/changes");
    assert_eq!(s.next().expect("seed").0, "seed");

    let body = concat!(
        r#"{"query":"put tasks {key: $1, title: $2, status: $3}","params":["d","four","open"]}"#,
        "\n",
        r#"{"query":"put tasks {key: $1, title: $2, status: $3}","params":["e","five","open"]}"#,
    );
    post(h.port, "/batch", body);

    // The two writes may arrive as a single batch or separately: seeing two
    // rows in total is enough.
    let mut seen = 0;
    while seen < 2 {
        let Some((name, data)) = s.next_within(Duration::from_secs(2)) else {
            break;
        };
        assert_eq!(name, "change");
        seen += count(&data, "puts");
    }
    assert_eq!(seen, 2);
}

#[test]
fn a_quiet_collection_keeps_its_cursor_fresh() {
    // The counter and the ring are shared across all collections. If the
    // subscriber of a quiet collection did not wake on other people's
    // writes, its cursor would stay put and it would be reseeded once the
    // ring overflowed. This test sets up exactly that scenario: a 4-entry
    // ring and 12 writes into a neighbouring collection.
    let cfg = Config {
        change_capacity: 4,
        ..Config::default()
    };
    let h = start(cfg);

    let mut s = Sub::open(h.port, "/tasks/changes");
    assert_eq!(s.next().expect("seed").0, "seed");

    post(
        h.port,
        "/query",
        r#"{"query":"create collection notes (text text)"}"#,
    );
    for i in 0..12 {
        post(h.port, "/notes", &format!(r#"{{"text":"n{i}"}}"#));
        // Let the subscriber wake and move its cursor.
        std::thread::sleep(Duration::from_millis(10));
    }

    post(
        h.port,
        "/tasks",
        r#"{"key":"z","title":"zed","status":"open"}"#,
    );
    let (name, data) = s.next().expect("change");
    assert_eq!(
        name, "change",
        "the cursor must stay current: there must be no reseed"
    );
    assert_eq!(count(&data, "puts"), 1);
    assert!(data.contains("zed"), "{data}");
}

/// A row past its time (`@ttl`) is out of the seed at once, and a
/// subscriber is told it is gone when the sweep deletes it -- one it was
/// sent and one it never was alike, as any delete of the shape.
#[test]
fn an_expired_row_leaves_the_subscription() {
    let h = start(Config::default());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    {
        let mut db = h.db.write().unwrap();
        for sql in [
            "create collection sessions (user text, seen timestamp @ttl(2s))".to_string(),
            format!("put sessions [{{user: \"live\", seen: {now}}}, {{user: \"old\", seen: 1}}]"),
        ] {
            db.execute(&fennec_ql::parse_one(&sql).unwrap()).unwrap();
        }
    }
    let mut s = Sub::open(h.port, "/sessions/changes");
    let (name, data) = s.next().expect("seed");
    assert_eq!(name, "seed");
    assert_eq!(count(&data, "rows"), 1, "{data}");
    assert!(data.contains("live") && !data.contains("old"), "{data}");

    // The sweep deletes the row the seed left out: a delete like any.
    assert_eq!(fennec_http::sweep::pass("test", &h.db), 1);
    let (name, data) = s.next().expect("change");
    assert_eq!(name, "change");
    assert!(data.contains("\"dels\":[2]"), "{data}");

    // The row it was sent is told deleted at its time, with no write, and
    // again as the sweep deletes it: a deletion of what is gone already.
    let (name, data) = s.next_within(Duration::from_secs(10)).expect("change");
    assert_eq!(name, "change");
    assert!(data.contains("\"dels\":[1]"), "{data}");
    assert!(data.contains("\"puts\":[]"), "{data}");
    assert_eq!(fennec_http::sweep::pass("test", &h.db), 1);
    let (name, data) = s.next().expect("change");
    assert_eq!(name, "change");
    assert!(data.contains("\"dels\":[1]"), "{data}");
}

/// A seed read pinned leaves out a row past its time as the one under the
/// lock does: `alive` is ANDed in on the pin as on the database.
#[test]
fn a_pinned_seed_leaves_out_an_expired_row() {
    let h = start(Config::default());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    {
        let mut db = h.db.write().unwrap();
        db.set_pin_at(0);
        for sql in [
            "create collection sessions (user text, seen timestamp @ttl(1h))".to_string(),
            format!("put sessions [{{user: \"live\", seen: {now}}}, {{user: \"old\", seen: 1}}]"),
        ] {
            db.execute(&fennec_ql::parse_one(&sql).unwrap()).unwrap();
        }
    }
    for target in ["/sessions/changes", "/sessions/changes?user=neq.nobody"] {
        let mut s = Sub::open(h.port, target);
        let (name, data) = s.next().expect("seed");
        assert_eq!(name, "seed");
        assert_eq!(count(&data, "rows"), 1, "{target}: {data}");
        assert!(data.contains("live") && !data.contains("old"), "{data}");
    }
}

/// A server over `seeded()` whose every read is pinned where the pin takes
/// it, and whose shape `gate(status) != "none"` holds its first read -- a
/// seed's -- where it began, until the test lets it go: the moment a long
/// seed is being read. The receiver hears the read begin; dropping the
/// sender lets it go.
fn gated(cfg: Config) -> (Harness, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (began, heard) = mpsc::channel();
    let (go, wait) = mpsc::channel::<()>();
    let first = Mutex::new(Some((began, wait)));
    let mut db = seeded();
    db.set_pin_at(0);
    let gate = move |args: &[Value]| {
        let held = first.lock().unwrap().take();
        if let Some((began, wait)) = held {
            let _ = began.send(());
            let _ = wait.recv();
        }
        Ok(args[0].clone())
    };
    db.registry_mut()
        .register_fn("gate", Arc::new(gate))
        .unwrap();
    (start_over(db, cfg), heard, go)
}

const GATED: &str = "/tasks/changes?where=gate(status)%20!%3D%20%22none%22";

/// The `seq` an event's data names.
fn seq_of(data: &str) -> u64 {
    let at = data.find("\"seq\":").expect("seq") + 6;
    let digits: String = data[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap()
}

/// The rows of the array `key` in an event's data, by id, each the text
/// after its id.
fn rows_of(data: &str, key: &str) -> Vec<(i64, String)> {
    let (_, tail) = data.split_once(&format!("\"{key}\":[")).expect(key);
    tail.split("{\"id\":")
        .skip(1)
        .map(|r| {
            let id: String = r.chars().take_while(char::is_ascii_digit).collect();
            (
                id.parse().unwrap(),
                r.split('}').next().unwrap().to_string(),
            )
        })
        .collect()
}

/// A seed is read pinned, with no lock held (`Database::pin`): a write
/// made while the seed is being read is answered then, not after it, and
/// the stream joins the two with no gap and no duplicate -- the seed at
/// the change it read at, without the writes, and the writes the changes
/// after it, each once. A seed taking the lock again holds the write for
/// as long as it reads, and the write here is never answered.
#[test]
fn a_write_made_while_a_seed_is_read_lands_and_follows_it() {
    let (h, began, go) = gated(Config::default());
    let before = h.db.read().unwrap().change_seq();
    let mut s = Sub::open(h.port, GATED);
    assert_eq!(s.status, 200);
    began
        .recv_timeout(Duration::from_secs(5))
        .expect("the seed read");

    // Answered while the seed is held: no lock is.
    let (st, _) = post(
        h.port,
        "/tasks",
        r#"{"key":"d","title":"four","status":"open"}"#,
    );
    assert_eq!(
        st, 201,
        "a write waited for the seed: the seed holds the lock"
    );
    let (st, body) = post(
        h.port,
        "/query",
        r#"{"query":"set tasks {title: \"ONE\"} where key = \"a\""}"#,
    );
    assert_eq!(st, 200, "{body}");
    let after = h.db.read().unwrap().change_seq();
    assert_eq!(after, before + 2);
    drop(go);

    let (name, seed) = s.next().expect("seed");
    assert_eq!(name, "seed");
    assert_eq!(seq_of(&seed), before, "the seed is at the change it read");
    assert_eq!(count(&seed, "rows"), 3, "{seed}");
    assert!(
        seed.contains("\"one\"") && !seed.contains("four") && !seed.contains("ONE"),
        "{seed}"
    );

    // The replica: the seed, then every change over it, by id.
    let mut replica: std::collections::BTreeMap<i64, String> =
        rows_of(&seed, "rows").into_iter().collect();
    let mut told = Vec::new();
    while replica.len() < 4 || !replica.values().any(|r| r.contains("ONE")) {
        let (name, data) = s.next().expect("the writes made beside the seed");
        assert_eq!(name, "change", "{data}");
        assert!(seq_of(&data) > before && seq_of(&data) <= after, "{data}");
        for (id, row) in rows_of(&data, "puts") {
            told.push(id);
            replica.insert(id, row);
        }
    }
    told.sort_unstable();
    assert_eq!(told, [1, 4], "each write told once, nothing the seed held");
    assert!(s.next_within(Duration::from_millis(300)).is_none());
    // What the server holds, row for row.
    let (_, all) = post(h.port, "/query", r#"{"query":"get tasks order id"}"#);
    let held: std::collections::BTreeMap<i64, String> =
        rows_of(&format!("{{\"rows\":{all}}}"), "rows")
            .into_iter()
            .collect();
    assert_eq!(replica, held);
}

/// `n` tasks put in one write while a seed is held, keyed from `from`.
fn put_tasks(port: u16, from: usize, n: usize) {
    let docs: Vec<String> = (from..from + n)
        .map(|k| format!(r#"{{"key":"k{k}","title":"t{k}","status":"open"}}"#))
        .collect();
    let body = format!(
        r#"{{"query":"put tasks $1","params":[[{}]]}}"#,
        docs.join(",")
    );
    let (st, out) = post(port, "/query", &body);
    assert_eq!(st, 200, "{out}");
}

/// Writes beside a seed outnumbering the change ring leave its change
/// servable: the seed holds the ring back to it (`Database::hold_changes`),
/// and the stream goes on from the seed with every one of them, rather
/// than finding its change overrun and seeding again -- pinned again, for
/// as long as the writes outran the scan.
#[test]
fn a_seed_holds_the_change_ring_back_to_its_change() {
    let (h, began, go) = gated(Config {
        change_capacity: 2,
        ..Config::default()
    });
    let before = h.db.read().unwrap().change_seq();
    let mut s = Sub::open(h.port, GATED);
    began
        .recv_timeout(Duration::from_secs(5))
        .expect("the seed read");
    put_tasks(h.port, 0, 5);
    drop(go);
    let (name, seed) = s.next().expect("seed");
    assert_eq!(name, "seed");
    assert_eq!(seq_of(&seed), before, "{seed}");
    assert_eq!(count(&seed, "rows"), 3, "{seed}");
    let (name, change) = s.next().expect("the writes beside the seed");
    assert_eq!(name, "change", "{change}");
    assert_eq!(count(&change, "puts"), 5, "{change}");
    // Let go, the ring is back to its capacity: a later write is told.
    put_tasks(h.port, 5, 1);
    let (name, change) = s.next().expect("a write after");
    assert_eq!((name.as_str(), count(&change, "puts")), ("change", 1));
}

/// A pinned seed the change ring no longer reaches back to by its end --
/// the writes beside it past what a hold keeps (`HELD_TIMES` the ring) --
/// is read again under the lock rather than sent: the stream would find its
/// change overrun and seed again, pinned again, for as long as writes
/// outran the scan. One seed goes out, at the change it read, holding
/// every write made beside the first read.
#[test]
fn a_seed_the_ring_overran_is_read_again_under_the_lock() {
    let (h, began, go) = gated(Config {
        change_capacity: 2,
        ..Config::default()
    });
    let mut s = Sub::open(h.port, GATED);
    began
        .recv_timeout(Duration::from_secs(5))
        .expect("the seed read");
    let beside = 2 * fennec_core::changes::HELD_TIMES + 1;
    put_tasks(h.port, 0, beside);
    let now = h.db.read().unwrap().change_seq();
    drop(go);
    let (name, seed) = s.next().expect("seed");
    assert_eq!(name, "seed");
    assert_eq!(seq_of(&seed), now, "{seed}");
    assert_eq!(count(&seed, "rows"), 3 + beside, "{seed}");
    assert!(s.next_within(Duration::from_millis(300)).is_none());
}

/// The rows the server holds, by id, as a stream's events write them.
fn server_rows(port: u16) -> std::collections::BTreeMap<i64, String> {
    let (_, all) = post(port, "/query", r#"{"query":"get tasks order id"}"#);
    rows_of(&format!("{{\"rows\":{all}}}"), "rows")
        .into_iter()
        .collect()
}

/// An image adopted into the server's database (`Database::adopt`, what a
/// replica does when its primary no longer holds the writes it missed),
/// holding other titles for the same ids at the very change the database
/// stands at: the ring and the counter show nothing.
fn adopt_at_the_same_change(h: &Harness) {
    let mut other = Database::new();
    for sql in [
        "create collection tasks (key text @hash, title text, status text @hash)",
        r#"put tasks [
             {key: "a", title: "uno",  status: "open"},
             {key: "b", title: "dos",  status: "open"},
             {key: "c", title: "tres", status: "closed"}
           ]"#,
    ] {
        other.execute(&fennec_ql::parse_one(sql).unwrap()).unwrap();
    }
    let image = other.snapshot();
    let mut fresh = Database::new();
    fresh.load(&image).unwrap();
    let mut db = h.db.write().unwrap();
    assert_eq!(fresh.change_seq(), db.change_seq());
    db.adopt(fresh, &image).unwrap();
}

/// An image adopted while a seed is read pinned replaced what the pin
/// reads, and can land on the pin's own change, where the ring and the
/// counter show nothing: the seed is read again rather than sent
/// (`Database::adoptions`), and the replica it builds is the server's.
#[test]
fn an_image_adopted_while_a_seed_is_read_is_read_again() {
    let (h, began, go) = gated(Config::default());
    let mut s = Sub::open(h.port, GATED);
    began
        .recv_timeout(Duration::from_secs(5))
        .expect("the seed read");
    adopt_at_the_same_change(&h);
    drop(go);
    let (name, seed) = s.next().expect("seed");
    assert_eq!(name, "seed");
    assert!(
        seed.contains("\"uno\"") && !seed.contains("\"one\""),
        "a seed of what the image replaced: {seed}"
    );
    let mut replica: std::collections::BTreeMap<i64, String> =
        rows_of(&seed, "rows").into_iter().collect();
    while let Some((name, data)) = s.next_within(Duration::from_millis(300)) {
        assert_eq!(name, "change", "{data}");
        for (id, row) in rows_of(&data, "puts") {
            replica.insert(id, row);
        }
    }
    assert_eq!(replica, server_rows(h.port));
}

/// An image adopted after a seed was sent, at the change the stream stands
/// at, reseeds it: its next look finds the adoptions moved, where the ring
/// shows nothing.
#[test]
fn an_image_adopted_at_a_streams_change_reseeds_it() {
    let h = start(Config::default());
    let mut s = Sub::open(h.port, "/tasks/changes");
    let (name, seed) = s.next().expect("seed");
    assert_eq!(name, "seed");
    assert!(seed.contains("\"one\""), "{seed}");
    adopt_at_the_same_change(&h);
    let (name, seed) = s.next().expect("seeded again");
    assert_eq!(name, "seed", "{seed}");
    let replica: std::collections::BTreeMap<i64, String> =
        rows_of(&seed, "rows").into_iter().collect();
    assert_eq!(replica, server_rows(h.port));
}

/// A seed's hold on the change ring goes at the stream's first look after
/// it, not with the stream: held for as long as an idle stream stays
/// open, the ring kept every write since its seed, `HELD_TIMES` its
/// capacity, for every stream open.
#[test]
fn a_stream_lets_the_ring_go_at_its_first_look() {
    let h = start(Config {
        change_capacity: 2,
        ..Config::default()
    });
    let mut s = Sub::open(h.port, "/tasks/changes");
    let (name, _) = s.next().expect("seed");
    assert_eq!(name, "seed");
    // Told a write: the stream has looked past its seed.
    put_tasks(h.port, 0, 1);
    let (name, change) = s.next().expect("a write after the seed");
    assert_eq!((name.as_str(), count(&change, "puts")), ("change", 1));
    // Writes a hold would keep, short of its bound: the ring keeps its
    // capacity alone, and the stream, its cursor overrun, is seeded again.
    put_tasks(h.port, 1, fennec_core::changes::HELD_TIMES / 2);
    {
        let db = h.db.read().unwrap();
        assert_eq!(
            db.change_horizon(),
            db.change_seq() - 2,
            "the ring is held past the stream's first look"
        );
    }
    let (name, _) = s.next().expect("the writes");
    assert_eq!(name, "seed");
}

// ------------------------------------------------------------ inner gets

/// A board whose messages a subscription follows by the titles of the
/// notes: `where=msg in (get notes select title)`.
fn board() -> Database {
    let mut db = Database::new();
    for sql in [
        "create collection notes (title text @hash)",
        "create collection board (msg text)",
        r#"put notes [{title: "a1"}, {title: "b1"}]"#,
        r#"put board [{msg: "a1"}, {msg: "b1"}, {msg: "c1"}]"#,
    ] {
        db.execute(&fennec_ql::parse_one(sql).unwrap()).unwrap();
    }
    db
}

const INNER: &str = "/board/changes?where=msg+in+(get+notes+select+title)";

/// The rows of the array `key` in an event's data, by id, each its `msg`.
fn msgs(data: &str, key: &str) -> Vec<(i64, String)> {
    let msg = |r: &str| {
        let m = r.split("\"msg\":\"").nth(1).unwrap_or("");
        m.split('"').next().unwrap_or("").to_string()
    };
    let mut rows: Vec<(i64, String)> = (rows_of(data, key).into_iter())
        .map(|(id, r)| (id, msg(&r)))
        .collect();
    rows.sort();
    rows
}

/// The ids a change event names as deleted, in order.
fn gone(data: &str) -> Vec<i64> {
    let mut ids = dels(data);
    ids.sort();
    ids
}

/// A shape holding an inner `get` is judged against the list as it is at
/// each look: the first write to `board` used to end the stream with
/// `event: error` (`eval` refuses an `in (get ...)`), its seed the only
/// thing it ever sent. Writes inside and outside the shape, one after
/// another, each told or not as a shape of a list written out would be.
#[test]
fn a_shapes_inner_get_is_answered_at_every_look() {
    let h = start_over(board(), Config::default());
    let mut s = Sub::open(h.port, INNER);
    assert_eq!(s.status, 200, "{}", s.head_line());
    let (name, data) = s.next().expect("seed");
    assert_eq!(name, "seed", "{data}");
    assert_eq!(
        msgs(&data, "rows"),
        [(1, "a1".into()), (2, "b1".into())],
        "{data}"
    );
    let mut id = 3;
    for (msg, told) in [("a1", true), ("zz", false), ("b1", true), ("c1", false)] {
        let (st, body) = post(h.port, "/board", &format!(r#"{{"msg":"{msg}"}}"#));
        assert_eq!(st, 201, "{body}");
        id += 1;
        // A write outside the shape is told nothing: the next event is the
        // next write's inside it.
        if told {
            let (name, data) = s.next().expect("a change");
            assert_eq!(name, "change", "{data}");
            assert_eq!(msgs(&data, "puts"), [(id, msg.into())], "{data}");
        }
    }
    // A message moved out of the list: deletions of the rows it was sent.
    let (st, body) = call(
        h.port,
        "PATCH",
        "/board?msg=eq.a1",
        Some(r#"{"msg":"a2"}"#),
        &[],
    );
    assert_eq!(st, 200, "{body}");
    let (name, data) = s.next().expect("a change");
    assert_eq!(name, "change", "{data}");
    assert_eq!(gone(&data), [1, 4], "{data}");
    assert_eq!(count(&data, "puts"), 0, "{data}");
}

/// A write to the inner `get`'s collection moves which rows the shape
/// holds with no write to them: a note titled `c1` lets the board's two
/// `c1`s in, and its deletion takes them out again, deletions of the rows
/// the stream was sent. A note naming no message, and a message outside
/// the list, are told nothing.
#[test]
fn a_write_to_the_inner_collection_moves_rows_in_and_out() {
    for (pinned, indexed) in [(false, true), (true, true), (true, false), (false, false)] {
        moves_rows_in_and_out(pinned, indexed);
    }
}

/// [`a_write_to_the_inner_collection_moves_rows_in_and_out`], every read
/// pinned or none (`set_pin_at`): the lists answered on a pin and the
/// changes judged there -- or under the lock where the inner `get` reaches
/// for `notes`' hash index, which a pin does not hold -- tell what the
/// lock tells.
fn moves_rows_in_and_out(pinned: bool, indexed: bool) {
    let mut db = match indexed {
        true => board(),
        false => {
            let mut db = Database::new();
            for sql in [
                "create collection notes (title text)",
                "create collection board (msg text)",
                r#"put notes [{title: "a1"}, {title: "b1"}]"#,
                r#"put board [{msg: "a1"}, {msg: "b1"}, {msg: "c1"}]"#,
            ] {
                db.execute(&fennec_ql::parse_one(sql).unwrap()).unwrap();
            }
            db
        }
    };
    if pinned {
        db.set_pin_at(0);
    }
    let h = start_over(db, Config::default());
    let mut s = Sub::open(h.port, INNER);
    assert_eq!(s.next().expect("seed").0, "seed");
    for (path, body) in [
        ("/board", r#"{"msg":"c1"}"#),
        ("/notes", r#"{"title":"nothing"}"#),
        ("/notes", r#"{"title":"c1"}"#),
    ] {
        let (st, answer) = post(h.port, path, body);
        assert_eq!(st, 201, "{answer}");
    }
    let (name, data) = s.next().expect("a change");
    assert_eq!(name, "change", "{data}");
    assert_eq!(
        msgs(&data, "puts"),
        [(3, "c1".into()), (4, "c1".into())],
        "{data}"
    );
    assert!(dels(&data).is_empty(), "{data}");

    let (st, body) = call(h.port, "DELETE", "/notes?title=eq.c1", None, &[]);
    assert_eq!(st, 200, "{body}");
    let (name, data) = s.next().expect("a change");
    assert_eq!(name, "change", "{data}");
    assert_eq!(gone(&data), [3, 4], "{data}");
    assert_eq!(count(&data, "puts"), 0, "{data}");
    // Gone, they are not told gone again at the next write.
    let (st, body) = post(h.port, "/board", r#"{"msg":"b1"}"#);
    assert_eq!(st, 201, "{body}");
    let (name, data) = s.next().expect("a change");
    assert_eq!(name, "change", "{data}");
    assert!(dels(&data).is_empty(), "{data}");
    assert_eq!(msgs(&data, "puts"), [(5, "b1".into())], "{data}");
}

/// Resumed with `since`, a stream whose shape reads another collection is
/// seeded all the same: neither what that collection held at its cursor
/// nor which rows it was sent is known here.
#[test]
fn a_resumed_stream_reading_another_collection_is_seeded() {
    let h = start_over(board(), Config::default());
    let cursor = h.db.read().unwrap().change_seq();
    let mut s = Sub::open(h.port, &format!("{INNER}&since={cursor}"));
    let (name, data) = s.next().expect("seed");
    assert_eq!(name, "seed", "{data}");
    assert_eq!(
        msgs(&data, "rows"),
        [(1, "a1".into()), (2, "b1".into())],
        "{data}"
    );
}

/// A poll of a read holding an inner `get` is tagged by the change
/// counter, as any read: 304 while neither collection was written, and
/// run again once the inner one was. It used to be tagged never, and ran
/// at every round however long nothing moved.
#[test]
fn a_poll_of_an_inner_get_is_answered_304_until_what_it_reads_is_written() {
    let h = start_over(board(), Config::default());
    let q = r#"{"query":"get board select msg where msg in (get notes select title) order msg"}"#;
    let poll = |tag: &str| {
        let mut sock = TcpStream::connect(("127.0.0.1", h.port)).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(
            sock,
            "POST /query HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nIf-None-Match: {tag}\r\n\
             Content-Length: {}\r\n\r\n{q}",
            q.len()
        )
        .unwrap();
        let mut out = String::new();
        sock.read_to_string(&mut out).unwrap();
        let status: u16 = out[9..12].parse().unwrap();
        let etag = out.lines().find_map(|l| l.strip_prefix("ETag: "));
        let body = out.split_once("\r\n\r\n").map_or("", |(_, b)| b);
        (status, etag.map(str::to_string), body.to_string())
    };
    let (status, tag, body) = poll("\"0\"");
    assert_eq!(status, 200, "{body}");
    let tag = tag.expect("a read holding an inner get is tagged");
    assert_eq!(poll(&tag).0, 304);
    // A write to neither collection is no change.
    let (st, body) = post(
        h.port,
        "/query",
        r#"{"query":"create collection other (n int)"}"#,
    );
    assert_eq!(st, 200, "{body}");
    assert_eq!(post(h.port, "/other", r#"{"n":1}"#).0, 201);
    assert_eq!(poll(&tag).0, 304);
    // The inner collection written: the answer moved, and is sent.
    assert_eq!(post(h.port, "/notes", r#"{"title":"c1"}"#).0, 201);
    let (status, _, body) = poll(&tag);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("c1"), "{body}");
}

/// A stream of the board whose notes expire (`@ttl`), one at a minute's
/// keep-alive: only the time can wake it.
fn expiring_board(ttl: &str, b1_at: i64) -> Harness {
    let mut db = Database::new();
    let far = now_ms() + 3_600_000;
    for sql in [
        format!("create collection notes (title text, at timestamp{ttl})"),
        "create collection board (msg text)".to_string(),
        format!(r#"put notes [{{title: "a1", at: {far}}}, {{title: "b1", at: {b1_at}}}]"#),
        r#"put board [{msg: "a1"}, {msg: "b1"}, {msg: "c1"}]"#.to_string(),
    ] {
        db.execute(&fennec_ql::parse_one(&sql).unwrap()).unwrap();
    }
    serve(
        db,
        Config {
            stream_keepalive: Duration::from_secs(60),
            ..Config::default()
        },
    )
}

/// A note reaching its time (`@ttl`) leaves the list with no write: the
/// stream wakes for it, as for a row of its own reaching its time
/// (`Reach::next`), and tells its rows gone then -- not at its next
/// keep-alive, a minute here, nor at the next write to anything.
#[test]
fn a_note_reaching_its_time_takes_its_rows_out_then() {
    // b1's note lives a second past two from now.
    let h = expiring_board(" @ttl(1s)", now_ms() + 2_000);
    let mut s = Sub::open(h.port, INNER);
    let (name, data) = s.next().expect("seed");
    assert_eq!(name, "seed", "{data}");
    assert_eq!(
        msgs(&data, "rows"),
        [(1, "a1".into()), (2, "b1".into())],
        "{data}"
    );
    let (name, data) = s
        .next_within(Duration::from_secs(15))
        .expect("b1's rows leave at its note's time");
    assert_eq!(name, "change", "{data}");
    assert_eq!(gone(&data), [2], "{data}");
    assert_eq!(count(&data, "puts"), 0, "{data}");
}

/// A `@ttl` given the inner collection after the stream opened is
/// followed: the change of schema is a write to what the stream reads,
/// which answers the lists again and asks when their rows reach their
/// time. Worked out once as the stream opened, from the collections that
/// expired then, it was never asked again.
#[test]
fn a_ttl_given_the_inner_collection_later_is_followed() {
    let h = expiring_board("", now_ms() + 1_000);
    let mut s = Sub::open(h.port, INNER);
    let (name, data) = s.next().expect("seed");
    assert_eq!(name, "seed", "{data}");
    assert_eq!(msgs(&data, "rows").len(), 2, "{data}");
    let (st, body) = post(
        h.port,
        "/query",
        r#"{"query":"create index on notes (at) @ttl(1s)"}"#,
    );
    assert_eq!(st, 200, "{body}");
    // b1's note goes a second after its `at`, a second from the stream's
    // start: told at its time, whatever the index's look saw.
    let mut told = Vec::new();
    while let Some((name, data)) = s.next_within(Duration::from_secs(15)) {
        assert_eq!(name, "change", "{data}");
        told.extend(gone(&data));
        if !told.is_empty() {
            break;
        }
    }
    assert_eq!(told, [2]);
}

// ------------------------------------------------------------ keep-alives

/// A stream woken more often than its keep-alive by something other than
/// a write it tells writes a keep-alive all the same once one passed with
/// nothing written, and a client gone frees its slot by the next ones.
/// Written only where a wait ran its whole length, such a stream wrote
/// none: a proxy cut it, and one whose client had gone held its
/// `--max-streams` slot till its next change, which may never come. Two
/// keep-alives are asked for within ten seconds, and the slot back within
/// ten more.
fn keeps_alive_and_lets_go(h: &Harness, target: &str) {
    let mut s = Sub::open(h.port, target);
    assert_eq!(s.status, 200, "{}", s.head_line());
    let (name, data) = s.next().expect("seed");
    assert_eq!(name, "seed", "{data}");
    assert_eq!(s.keepalives(2, Duration::from_secs(10)), 2, "{target}");
    assert_eq!(h.server.live_streams(), 1);
    drop(s);
    let by = Instant::now() + Duration::from_secs(10);
    while h.server.live_streams() > 0 {
        assert!(
            Instant::now() < by,
            "{target}: the stream outlived its client"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A stream whose inner `get` reads the time answers it again each
/// second, waking for it: a keep-alive longer than that, 1.2 s here, still
/// goes out.
#[test]
fn a_stream_reading_the_time_writes_its_keep_alives() {
    let mut db = Database::new();
    let far = now_ms() + 3_600_000;
    for sql in [
        "create collection notes (title text, at timestamp)".to_string(),
        "create collection board (msg text)".to_string(),
        format!(r#"put notes [{{title: "a1", at: {far}}}]"#),
        r#"put board [{msg: "a1"}, {msg: "c1"}]"#.to_string(),
    ] {
        db.execute(&fennec_ql::parse_one(&sql).unwrap()).unwrap();
    }
    let h = serve(
        db,
        Config {
            stream_keepalive: Duration::from_millis(1_200),
            ..Config::default()
        },
    );
    keeps_alive_and_lets_go(
        &h,
        "/board/changes?where=msg+in+(get+notes+select+title+where+at+%3E+now())",
    );
}

/// A stream whose own collection's rows reach their `@ttl` every 100 ms,
/// none of them in its shape: woken for each, it tells nothing, and
/// writes its keep-alives, 300 ms here, all the same.
#[test]
fn a_stream_woken_by_rows_reaching_their_time_writes_its_keep_alives() {
    let mut db = Database::new();
    let now = now_ms();
    // A row reaching its time every 100 ms for a minute, from 100 ms on.
    let rows: Vec<String> = (0..600)
        .map(|i| format!(r#"{{courier: "other", seen: {}}}"#, now - 900 + i * 100))
        .collect();
    for sql in [
        "create collection positions (courier text, seen timestamp @ttl(1000ms))".to_string(),
        format!("put positions [{}]", rows.join(", ")),
    ] {
        db.execute(&fennec_ql::parse_one(&sql).unwrap()).unwrap();
    }
    let h = serve(
        db,
        Config {
            stream_keepalive: Duration::from_millis(300),
            ..Config::default()
        },
    );
    keeps_alive_and_lets_go(&h, "/positions/changes?where=courier+%3D+%22mine%22");
}

/// Lists reading the time are answered again a second after the read
/// answering them ended. Counted from when it began -- the lock's time,
/// before the answer -- an inner `get` taking longer than a second was
/// due again as it ended, and an idle stream answered it back to back, its
/// thread never let go. Each answer is timed by the function its filter
/// calls (`slow`, over a collection of one row), and the next is asserted
/// to begin a second or more after the last one ended.
#[test]
fn lists_reading_the_time_are_due_a_second_after_their_answer_ended() {
    let answers: Arc<Mutex<Vec<(Instant, Instant)>>> = Default::default();
    let mut db = Database::new();
    let far = now_ms() + 3_600_000;
    for sql in [
        "create collection notes (title text, at timestamp)".to_string(),
        "create collection board (msg text)".to_string(),
        format!(r#"put notes [{{title: "a1", at: {far}}}]"#),
        r#"put board [{msg: "a1"}, {msg: "c1"}]"#.to_string(),
    ] {
        db.execute(&fennec_ql::parse_one(&sql).unwrap()).unwrap();
    }
    let timed = Arc::clone(&answers);
    let slow = move |args: &[Value]| {
        let began = Instant::now();
        std::thread::sleep(Duration::from_millis(1_200));
        timed.lock().unwrap().push((began, Instant::now()));
        Ok(args[0].clone())
    };
    db.registry_mut()
        .register_fn("slow", Arc::new(slow))
        .unwrap();
    let h = serve(
        db,
        Config {
            stream_keepalive: Duration::from_secs(60),
            ..Config::default()
        },
    );
    let mut s = Sub::open(
        h.port,
        "/board/changes?where=msg+in+(get+notes+select+title+where+slow(title)+%3D+title+and+at+%3E+now())",
    );
    let (name, data) = s.next_within(Duration::from_secs(10)).expect("seed");
    assert_eq!(name, "seed", "{data}");
    assert_eq!(msgs(&data, "rows"), [(1, "a1".into())], "{data}");
    // The seed's answer and two looks'.
    let by = Instant::now() + Duration::from_secs(30);
    while answers.lock().unwrap().len() < 3 {
        assert!(Instant::now() < by, "the lists were not answered again");
        std::thread::sleep(Duration::from_millis(50));
    }
    let answers = answers.lock().unwrap().clone();
    for w in answers.windows(2) {
        let gap = w[1].0.saturating_duration_since(w[0].1);
        // A millisecond's rounding of the server's clock spared.
        assert!(
            gap >= Duration::from_millis(990),
            "answered again {gap:?} after the last answer ended"
        );
    }
}

/// A poll's tag names the images the database adopted: one adopted at
/// the very change a tag names replaced what the read answered, with no
/// write the change ring shows, and the tag sent back is answered again,
/// not 304.
#[test]
fn a_poll_tag_names_the_images_adopted() {
    let h = start(Config::default());
    let poll = |tag: &str| {
        let mut sock = TcpStream::connect(("127.0.0.1", h.port)).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let q = r#"{"query":"get tasks order id"}"#;
        write!(
            sock,
            "POST /query HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nIf-None-Match: {tag}\r\n\
             Content-Length: {}\r\n\r\n{q}",
            q.len()
        )
        .unwrap();
        let mut out = String::new();
        sock.read_to_string(&mut out).unwrap();
        let status: u16 = out[9..12].parse().unwrap();
        let etag = out.lines().find_map(|l| l.strip_prefix("ETag: "));
        (status, etag.map(str::to_string))
    };
    let (status, tag) = poll("\"0\"");
    assert_eq!(status, 200);
    let tag = tag.expect("tagged");
    assert_eq!(poll(&tag).0, 304);
    adopt_at_the_same_change(&h);
    let (status, fresh) = poll(&tag);
    assert_eq!(status, 200, "an image adopted at the tag's change");
    let fresh = fresh.expect("tagged");
    assert_ne!(fresh, tag);
    assert_eq!(poll(&fresh).0, 304);
}

/// Every read pinned, an index's too (`set_pin_strays`): a pin reaching
/// for an index answers `None`, and what it read is read again under the
/// lock -- the seed by the lists its pin answered, kept where nothing they
/// read moved (`sse::Kept`); a look's shape, run on a pin by lists answered
/// apart, by those lists; a list whose inner `get` reaches for `notes`'
/// index, answered there -- each telling what the lock tells.
#[test]
fn a_pin_reaching_for_an_index_is_read_again_under_the_lock() {
    let mut db = Database::new();
    for sql in [
        "create collection notes (title text, k int @hash)",
        "create collection board (msg text @hash)",
        r#"put notes [{title: "a1", k: 1}, {title: "b1", k: 1}]"#,
        r#"put board [{msg: "a1"}, {msg: "b1"}, {msg: "c1"}]"#,
    ] {
        db.execute(&fennec_ql::parse_one(sql).unwrap()).unwrap();
    }
    db.set_pin_strays(true);
    let h = start_over(db, Config::default());
    let mut streams = Vec::new();
    for path in [
        INNER,
        "/board/changes?where=msg+in+(get+notes+select+title+where+k+=+1)",
    ] {
        let mut s = Sub::open(h.port, path);
        assert_eq!(s.status, 200, "{}", s.head_line());
        let (name, data) = s.next().expect("seed");
        assert_eq!(name, "seed", "{data}");
        assert_eq!(
            msgs(&data, "rows"),
            [(1, "a1".into()), (2, "b1".into())],
            "{path}: {data}"
        );
        streams.push(s);
    }
    let (st, body) = post(h.port, "/notes", r#"{"title":"c1","k":1}"#);
    assert_eq!(st, 201, "{body}");
    for s in &mut streams {
        let (name, data) = s.next().expect("a change");
        assert_eq!(name, "change", "{data}");
        assert_eq!(msgs(&data, "puts"), [(3, "c1".into())], "{data}");
        assert!(dels(&data).is_empty(), "{data}");
    }
    let (st, body) = call(h.port, "DELETE", "/notes?title=eq.a1", None, &[]);
    assert_eq!(st, 200, "{body}");
    for s in &mut streams {
        let (name, data) = s.next().expect("a change");
        assert_eq!(name, "change", "{data}");
        assert_eq!(gone(&data), [1], "{data}");
        assert_eq!(count(&data, "puts"), 0, "{data}");
    }
}

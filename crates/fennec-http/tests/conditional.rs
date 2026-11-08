//! Optimistic concurrency on `/<name>/<id>`: `GET` answers the row's tag in
//! `ETag`, and a `PATCH`, `PUT` or `DELETE` sent with `If-Match` or
//! `If-None-Match` writes only where the row, under the write lock that
//! writes it, meets them -- else 412 and nothing written.

use fennec_core::prelude::*;
use fennec_http::access::Access;
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Barrier, RwLock};
use std::time::Duration;

const SECRET: &[u8] = b"thirty-two bytes and a few more, for HS256";
const ROOT: &str = "root-token";

/// A reader sees a note's title and who wrote it, never its secret, and
/// may set the title alone.
const POLICY: &str = "\
notes  read(id, title, owner)
notes  update(title)
";

struct Node {
    port: u16,
    access: Arc<Access>,
}

const SETUP: &[&str] = &[
    "create collection notes (title text, owner text, secret text, n int)",
    r#"put notes [{id: 1, title: "a", owner: "k1", secret: "s1", n: 0},
                  {id: 2, title: "b", owner: "k2", secret: "s2", n: 0}]"#,
];

fn start() -> Node {
    start_on(Database::new(), SETUP)
}

fn start_on(db: Database, setup: &[&str]) -> Node {
    start_under(POLICY, db, setup)
}

fn start_under(policy: &str, mut db: Database, setup: &[&str]) -> Node {
    fennec_http::audit::set_delay(0);
    let access = Arc::new(Access::new(SECRET, policy).unwrap());
    for sql in setup {
        db.execute(&fennec_ql::parse_one(sql).unwrap()).unwrap();
    }
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some(ROOT.into()),
        access: Some(Arc::clone(&access)),
        ..Config::default()
    };
    let server = Server::new(Arc::new(RwLock::new(db)), cfg);
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    Node { port, access }
}

#[derive(Debug)]
struct Answer {
    status: u16,
    etag: Option<String>,
    /// `Cache-Control`, which an answer carrying a row's tag sends as
    /// `no-transform`.
    cache: Option<String>,
    body: String,
}

impl Node {
    fn reader(&self) -> String {
        self.access.mint(r#"{"sub":"k1"}"#).unwrap()
    }

    fn send(
        &self,
        token: &str,
        method: &str,
        target: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Answer {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut head = format!(
            "{method} {target} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\n\
             Content-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        s.write_all(head.as_bytes()).unwrap();
        s.write_all(body.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
        let header = |name: &str| {
            (head.lines())
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.trim().to_string())
        };
        Answer {
            status: head[9..12].parse().unwrap(),
            etag: header("etag"),
            cache: header("cache-control"),
            body: body.to_string(),
        }
    }

    /// `query` sent to `route` (`/query`, `/batch` or `/_call/<name>`)
    /// with `headers`, its body as that route reads one.
    fn statement(&self, route: &str, query: &str, headers: &[(&str, &str)]) -> Answer {
        let body = match route {
            "/batch" => format!("{{\"query\":{query:?}}}\n"),
            "/query" => format!("{{\"query\":{query:?}}}"),
            _ => String::new(),
        };
        self.root("POST", route, headers, &body)
    }

    fn root(&self, method: &str, target: &str, headers: &[(&str, &str)], body: &str) -> Answer {
        self.send(ROOT, method, target, headers, body)
    }

    /// The row and its tag, which must be there.
    fn get(&self, id: u64) -> (String, String) {
        let a = self.root("GET", &format!("/notes/{id}"), &[], "");
        assert_eq!(a.status, 200, "{a:?}");
        (a.body, a.etag.expect("a row's answer carries its tag"))
    }
}

#[test]
fn a_row_by_id_is_answered_with_its_tag() {
    let n = start();
    let (body, tag) = n.get(1);
    assert_eq!(
        body,
        r#"{"id":1,"title":"a","owner":"k1","secret":"s1","n":0}"#
    );
    // Strong, and the tag of those very bytes.
    assert!(tag.starts_with('"') && tag.len() == 34, "{tag}");
    assert_eq!(tag, fennec_http::conditional::tag(body.as_bytes()).as_str());
    // Kept strong past a proxy that compresses: `no-transform` beside it,
    // and on no answer carrying no row's tag.
    let a = n.root("GET", "/notes/1", &[], "");
    assert_eq!(a.cache.as_deref(), Some("no-transform"), "{a:?}");
    let filtered = n.root("GET", "/notes?id=eq.1", &[], "");
    assert_eq!((filtered.status, filtered.cache), (200, None));
    // HEAD tells the same tag with no body.
    let head = n.root("HEAD", "/notes/1", &[], "");
    assert_eq!(
        (head.status, head.etag.as_deref()),
        (200, Some(tag.as_str()))
    );
    assert_eq!(head.body, "");
    // Another row is another tag; a missing row is 404, with or without a
    // precondition, a missing collection as `/<name>` answers it.
    assert_ne!(n.get(2).1, tag);
    assert_eq!(n.root("GET", "/notes/99", &[], "").status, 404);
    let starred = n.root("GET", "/notes/99", &[("If-Match", "*")], "");
    assert_eq!(starred.status, 404);
    assert_eq!(n.root("GET", "/nosuch/1", &[], "").status, 404);
    // A second segment that is no id is no path, as before; a query string
    // is refused, never passed over: the tag is the whole row's.
    let word = n.root("GET", "/notes/abc", &[], "");
    assert_eq!(word.status, 404, "{word:?}");
    assert!(word.body.contains("path"), "{word:?}");
    assert_eq!(n.root("GET", "/notes/-1", &[], "").status, 404);
    let selected = n.root("GET", "/notes/1?select=title", &[], "");
    assert_eq!(selected.status, 400, "{selected:?}");
}

#[test]
fn a_tag_named_again_is_304_and_another_is_the_row() {
    let n = start();
    let (_, tag) = n.get(1);
    for asked in [
        tag.clone(),
        format!("W/{tag}"),
        format!("\"x\", {tag}"),
        "*".into(),
    ] {
        let a = n.root("GET", "/notes/1", &[("If-None-Match", &asked)], "");
        assert_eq!(a.status, 304, "{asked}: {a:?}");
        assert_eq!(a.etag.as_deref(), Some(tag.as_str()));
        assert_eq!(a.cache.as_deref(), Some("no-transform"));
        assert_eq!(a.body, "");
    }
    let other = n.root("GET", "/notes/1", &[("If-None-Match", "\"x\"")], "");
    assert_eq!(
        (other.status, other.etag.as_deref()),
        (200, Some(tag.as_str()))
    );
    // `If-Match` on a read: the row, or 412 where it is not the one named.
    assert_eq!(
        n.root("GET", "/notes/1", &[("If-Match", &tag)], "").status,
        200
    );
    assert_eq!(
        n.root("GET", "/notes/1", &[("If-Match", "\"x\"")], "")
            .status,
        412
    );
}

#[test]
fn a_write_under_the_rows_tag_is_made_and_answers_the_next() {
    let n = start();
    let (_, tag) = n.get(1);
    let a = n.root("PATCH", "/notes/1", &[("If-Match", &tag)], r#"{"n": 5}"#);
    assert_eq!(
        (a.status, a.body.as_str()),
        (200, r#"{"updated":1}"#),
        "{a:?}"
    );
    let (body, now) = n.get(1);
    assert!(body.contains(r#""n":5"#), "{body}");
    // The tag moved with the write, and the write's answer said where to,
    // `no-transform` beside it.
    assert_ne!(now, tag);
    assert_eq!(a.etag.as_deref(), Some(now.as_str()));
    assert_eq!(a.cache.as_deref(), Some("no-transform"));
    // So the next write goes on from it, a delete too, which answers none.
    let b = n.root("PATCH", "/notes/1", &[("If-Match", &now)], r#"{"n": 6}"#);
    assert_eq!(b.status, 200, "{b:?}");
    let (_, last) = n.get(1);
    let d = n.root("DELETE", "/notes/1", &[("If-Match", &last)], "");
    assert_eq!(
        (d.status, d.body.as_str(), d.etag),
        (200, r#"{"deleted":1}"#, None)
    );
    assert_eq!(n.root("GET", "/notes/1", &[], "").status, 404);
}

#[test]
fn a_stale_tag_is_412_and_nothing_is_written() {
    let n = start();
    let (_, stale) = n.get(1);
    // Another client writes first, without a precondition.
    let other = n.root("PATCH", "/notes/1", &[], r#"{"title": "theirs"}"#);
    assert_eq!((other.status, other.etag, other.cache), (200, None, None));
    let (before, now) = n.get(1);
    assert_ne!(now, stale);
    for (method, body) in [
        ("PATCH", r#"{"title": "mine"}"#),
        ("PUT", r#"{"title": "mine"}"#),
        ("DELETE", ""),
    ] {
        let a = n.root(method, "/notes/1", &[("If-Match", &stale)], body);
        assert_eq!(a.status, 412, "{method}: {a:?}");
        assert!(a.body.contains("If-Match"), "{a:?}");
        assert_eq!(n.get(1), (before.clone(), now.clone()), "{method} wrote");
    }
    // A weak tag never names a row for a write: compared strongly.
    let weak = n.root(
        "PATCH",
        "/notes/1",
        &[("If-Match", &format!("W/{now}"))],
        r#"{"n": 1}"#,
    );
    assert_eq!(weak.status, 412);
    // One of a list is enough.
    let listed = format!("{stale}, {now}");
    let a = n.root("PATCH", "/notes/1", &[("If-Match", &listed)], r#"{"n": 1}"#);
    assert_eq!(a.status, 200, "{a:?}");
}

#[test]
fn if_match_star_is_the_row_being_there() {
    let n = start();
    for (method, body) in [
        ("PATCH", r#"{"n": 1}"#),
        ("PUT", r#"{"title": "x"}"#),
        ("DELETE", ""),
    ] {
        let a = n.root(method, "/notes/99", &[("If-Match", "*")], body);
        assert_eq!(a.status, 412, "{method}: {a:?}");
    }
    // `PUT` under `If-Match: *` made nothing.
    assert_eq!(n.root("GET", "/notes/99", &[], "").status, 404);
    let a = n.root("PATCH", "/notes/2", &[("If-Match", "*")], r#"{"n": 1}"#);
    assert_eq!(a.status, 200, "{a:?}");
    // Without one, a write to a missing row says what it did, as the
    // filter's routes do.
    let plain = n.root("PATCH", "/notes/99", &[], r#"{"n": 1}"#);
    assert_eq!(
        (plain.status, plain.body.as_str()),
        (200, r#"{"updated":0}"#)
    );
    let plain = n.root("DELETE", "/notes/99", &[], "");
    assert_eq!(
        (plain.status, plain.body.as_str()),
        (200, r#"{"deleted":0}"#)
    );
}

#[test]
fn if_none_match_star_makes_the_row_once() {
    let n = start();
    let first = n.root(
        "PUT",
        "/notes/7",
        &[("If-None-Match", "*")],
        r#"{"title": "first"}"#,
    );
    assert_eq!(
        (first.status, first.body.as_str()),
        (201, r#"{"written":1}"#),
        "{first:?}"
    );
    let (body, tag) = n.get(7);
    assert_eq!(first.etag.as_deref(), Some(tag.as_str()));
    let again = n.root(
        "PUT",
        "/notes/7",
        &[("If-None-Match", "*")],
        r#"{"title": "second"}"#,
    );
    assert_eq!(again.status, 412, "{again:?}");
    assert_eq!(n.get(7).0, body);
    // `PUT` writes the row whole: 200 over a row, 201 where it made one;
    // the body's id, where it names one, is the path's.
    let over = n.root(
        "PUT",
        "/notes/7",
        &[("If-Match", &tag)],
        r#"{"id": 7, "n": 3}"#,
    );
    assert_eq!(over.status, 200, "{over:?}");
    assert_eq!(
        n.get(7).0,
        r#"{"id":7,"title":null,"owner":null,"secret":null,"n":3}"#
    );
    let made = n.root("PUT", "/notes/8", &[], r#"{"title": "plain"}"#);
    assert_eq!((made.status, made.etag), (201, None));
    assert_eq!(
        n.root("PUT", "/notes/8", &[], r#"{"title": "plain"}"#)
            .status,
        200
    );
    let other = n.root("PUT", "/notes/8", &[], r#"{"id": 9, "title": "x"}"#);
    assert_eq!(other.status, 400, "{other:?}");
}

#[test]
fn a_precondition_off_a_rows_path_is_refused() {
    let n = start();
    let (_, tag) = n.get(1);
    for (method, target, header, body) in [
        ("PATCH", "/notes?id=eq.1", "If-Match", r#"{"n": 1}"#),
        ("DELETE", "/notes?id=eq.1", "If-Match", ""),
        ("PATCH", "/notes/all", "If-Match", r#"{"n": 1}"#),
        (
            "POST",
            "/notes",
            "If-None-Match",
            r#"{"id": 1, "title": "x"}"#,
        ),
    ] {
        let value = if header == "If-Match" {
            tag.as_str()
        } else {
            "*"
        };
        let a = n.root(method, target, &[(header, value)], body);
        assert_eq!(a.status, 400, "{method} {target}: {a:?}");
    }
    assert_eq!(n.get(1).1, tag);
}

/// A read rule withholding `secret` and `n` shows the reader the row
/// without them, and the tag is of what it is shown: a write to those
/// alone moves the server's tag and leaves the reader's as it was -- no bit
/// of the secret in it -- and the reader's write under its tag goes on.
#[test]
fn a_scoped_tag_tells_nothing_of_a_withheld_field() {
    let n = start();
    let reader = n.reader();
    let read = |headers: &[(&str, &str)]| n.send(&reader, "GET", "/notes/1", headers, "");
    let seen = read(&[]);
    assert_eq!(seen.status, 200, "{seen:?}");
    assert_eq!(seen.body, r#"{"id":1,"title":"a","owner":"k1"}"#);
    let tag = seen.etag.clone().unwrap();
    assert_eq!(
        tag,
        fennec_http::conditional::tag(seen.body.as_bytes()).as_str()
    );
    assert_ne!(Some(tag.clone()), Some(n.get(1).1));
    let (_, full) = n.get(1);
    for secret in ["s1-changed", "another", ""] {
        let w = n.root(
            "PATCH",
            "/notes/1",
            &[],
            &format!(r#"{{"secret": "{secret}", "n": 9}}"#),
        );
        assert_eq!(w.status, 200);
        let again = read(&[]);
        assert_eq!(
            (again.body.as_str(), again.etag.as_deref()),
            (seen.body.as_str(), Some(tag.as_str()))
        );
        assert_eq!(read(&[("If-None-Match", &tag)]).status, 304);
    }
    assert_ne!(n.get(1).1, full);
    // Its write under the tag of its view is made; a write of the title
    // moves that tag, and the old one is then 412.
    let w = n.send(
        &reader,
        "PATCH",
        "/notes/1",
        &[("If-Match", &tag)],
        r#"{"title": "z"}"#,
    );
    assert_eq!(w.status, 200, "{w:?}");
    let moved = w.etag.clone().unwrap();
    assert_ne!(moved, tag);
    assert_eq!(read(&[]).etag.as_deref(), Some(moved.as_str()));
    let stale = n.send(
        &reader,
        "PATCH",
        "/notes/1",
        &[("If-Match", &tag)],
        r#"{"title": "y"}"#,
    );
    assert_eq!(stale.status, 412);
}

/// The tag is the row's content's: a write that leaves the row as it was
/// leaves its tag, and a write under that tag after it is made -- the row
/// it read is the row there, no write of another lost.
#[test]
fn a_write_leaving_the_row_as_it_was_leaves_its_tag() {
    let n = start();
    let (body, tag) = n.get(1);
    let same = n.root("PATCH", "/notes/1", &[("If-Match", &tag)], r#"{"n": 0}"#);
    assert_eq!(
        (same.status, same.etag.as_deref()),
        (200, Some(tag.as_str()))
    );
    assert_eq!(n.get(1), (body, tag.clone()));
    let next = n.root("PATCH", "/notes/1", &[("If-Match", &tag)], r#"{"n": 1}"#);
    assert_eq!(next.status, 200, "{next:?}");
}

/// Eight clients send one write each under the same tag at once: the write
/// lock that looks at the row writes it, so exactly one is made and every
/// other is 412, round after round. Each writes a value the row never held:
/// a write leaving the row as it was leaves its tag, and the next under it
/// is made -- it loses nothing -- which a client writing the `n` the row
/// started with did, one round in some, two made.
#[test]
fn of_writes_under_one_tag_at_once_one_is_made() {
    let n = Arc::new(start());
    const CLIENTS: usize = 8;
    for round in 0..20 {
        let (_, tag) = n.get(2);
        let gate = Arc::new(Barrier::new(CLIENTS));
        let sent: Vec<_> = (0..CLIENTS)
            .map(|c| {
                let (n, gate, tag) = (Arc::clone(&n), Arc::clone(&gate), tag.clone());
                std::thread::spawn(move || {
                    gate.wait();
                    let body = format!(r#"{{"n": {}}}"#, round * 100 + c + 1);
                    (c, n.root("PATCH", "/notes/2", &[("If-Match", &tag)], &body))
                })
            })
            .collect();
        let answers: Vec<_> = sent.into_iter().map(|t| t.join().unwrap()).collect();
        let made: Vec<_> = answers.iter().filter(|(_, a)| a.status == 200).collect();
        assert_eq!(made.len(), 1, "round {round}: {answers:?}");
        assert!(
            answers.iter().all(|(_, a)| matches!(a.status, 200 | 412)),
            "{answers:?}"
        );
        let winner = made[0].0;
        let (body, now) = n.get(2);
        assert!(
            body.contains(&format!(r#""n":{}"#, round * 100 + winner + 1)),
            "{body}"
        );
        assert_eq!(made[0].1.etag.as_deref(), Some(now.as_str()));
    }
}

/// `CLIENTS` requests sent at once, `send(node, client)` each, past one
/// gate: each client's answer.
fn at_once(
    n: &Arc<Node>,
    send: impl Fn(&Node, usize) -> Answer + Send + Sync + 'static,
) -> Vec<(usize, Answer)> {
    const CLIENTS: usize = 8;
    let gate = Arc::new(Barrier::new(CLIENTS));
    let send = Arc::new(send);
    let sent: Vec<_> = (0..CLIENTS)
        .map(|c| {
            let (n, gate, send) = (Arc::clone(n), Arc::clone(&gate), Arc::clone(&send));
            std::thread::spawn(move || {
                gate.wait();
                (c, send(&n, c))
            })
        })
        .collect();
    sent.into_iter().map(|t| t.join().unwrap()).collect()
}

/// Eight clients make one row under `If-None-Match: *` at once: the look
/// and the insert are under one write lock, so exactly one is made (201)
/// and every other is 412, never a 409 nor a write over it.
#[test]
fn of_puts_making_one_row_at_once_one_is_made() {
    let n = Arc::new(start());
    for round in 0..20 {
        let id = 100 + round;
        let answers = at_once(&n, move |n, c| {
            let body = format!(r#"{{"n": {c}}}"#);
            n.root(
                "PUT",
                &format!("/notes/{id}"),
                &[("If-None-Match", "*")],
                &body,
            )
        });
        let made: Vec<_> = answers.iter().filter(|(_, a)| a.status == 201).collect();
        assert_eq!(made.len(), 1, "round {round}: {answers:?}");
        assert!(
            answers.iter().all(|(_, a)| matches!(a.status, 201 | 412)),
            "{answers:?}"
        );
        let (body, tag) = n.get(id as u64);
        assert!(body.contains(&format!(r#""n":{}"#, made[0].0)), "{body}");
        assert_eq!(made[0].1.etag.as_deref(), Some(tag.as_str()));
    }
}

/// Eight clients delete one row under the tag they read, at once: exactly
/// one deletes it, every other is 412 -- the row it named is gone.
#[test]
fn of_deletes_under_one_tag_at_once_one_is_made() {
    let n = Arc::new(start());
    for round in 0..20 {
        let made = n.root("PUT", "/notes/2", &[], &format!(r#"{{"n": {round}}}"#));
        assert!(matches!(made.status, 200 | 201), "{made:?}");
        let (_, tag) = n.get(2);
        let answers = at_once(&n, move |n, _| {
            n.root("DELETE", "/notes/2", &[("If-Match", &tag)], "")
        });
        let deleted: Vec<_> = (answers.iter())
            .filter(|(_, a)| (a.status, a.body.as_str()) == (200, r#"{"deleted":1}"#))
            .collect();
        assert_eq!(deleted.len(), 1, "round {round}: {answers:?}");
        assert!(
            answers.iter().all(|(_, a)| matches!(a.status, 200 | 412)),
            "{answers:?}"
        );
        assert_eq!(n.root("GET", "/notes/2", &[], "").status, 404);
    }
}

/// A header sent on several lines is one list (RFC 9110, 5.3), whichever
/// line holds the row's tag: `If-Match` holds where any line names the row,
/// `If-None-Match` refuses where any line does. Read by its last line
/// alone, the first was 412 over the very row it named, and the second
/// wrote over the row it named not to.
#[test]
fn every_line_of_a_precondition_counts() {
    let n = start();
    for (round, first) in [true, false].into_iter().enumerate() {
        let (_, tag) = n.get(1);
        let lines = match first {
            true => [("If-Match", tag.as_str()), ("If-Match", "\"x\"")],
            false => [("If-Match", "\"x\""), ("If-Match", tag.as_str())],
        };
        assert_eq!(n.root("GET", "/notes/1", &lines, "").status, 200);
        let body = format!(r#"{{"n": {}}}"#, 10 + round);
        let a = n.root("PATCH", "/notes/1", &lines, &body);
        assert_eq!(a.status, 200, "{lines:?}: {a:?}");
    }
    let (body, tag) = n.get(1);
    let neither = [("If-Match", "\"x\""), ("If-Match", "\"y\"")];
    assert_eq!(
        n.root("PATCH", "/notes/1", &neither, r#"{"n": 1}"#).status,
        412
    );
    for first in [true, false] {
        let lines = match first {
            true => [("If-None-Match", tag.as_str()), ("If-None-Match", "\"x\"")],
            false => [("If-None-Match", "\"x\""), ("If-None-Match", tag.as_str())],
        };
        assert_eq!(
            n.root("GET", "/notes/1", &lines, "").status,
            304,
            "{lines:?}"
        );
        for (method, body) in [
            ("PATCH", r#"{"n": 1}"#),
            ("PUT", r#"{"n": 1}"#),
            ("DELETE", ""),
        ] {
            let a = n.root(method, "/notes/1", &lines, body);
            assert_eq!(a.status, 412, "{method} {lines:?}: {a:?}");
        }
        assert_eq!(n.get(1), (body.clone(), tag.clone()));
    }
    // `*` on any line is a create's: refused over the row, made where none is.
    let star = [("If-None-Match", "\"x\""), ("If-None-Match", "*")];
    assert_eq!(n.root("PUT", "/notes/1", &star, r#"{"n": 1}"#).status, 412);
    assert_eq!(n.root("PUT", "/notes/50", &star, r#"{"n": 1}"#).status, 201);
}

/// A scoped token's `PUT /<name>/<id>` is refused (403) whatever its
/// precondition, over its row and a missing one alike, nothing written: a
/// `PUT` names its row's id, which a scoped token never writes -- it would
/// write over a row the token cannot see, every field it leaves out gone.
/// Its conditional writes are `PATCH` and `DELETE` under `If-Match`.
#[test]
fn a_scoped_put_is_refused_whatever_its_precondition() {
    // The reader, granted no insert, and a token granted every write.
    for (policy, sub) in [(POLICY, "k1"), ("notes  read,write\n", "k1")] {
        let n = start_under(policy, Database::new(), SETUP);
        let token = n.access.mint(&format!(r#"{{"sub":"{sub}"}}"#)).unwrap();
        let seen = n.send(&token, "GET", "/notes/1", &[], "");
        let seen = seen.etag.expect("the token reads the row");
        let before = n.get(1);
        for target in ["/notes/1", "/notes/99"] {
            for headers in [
                &[][..],
                &[("If-Match", seen.as_str())][..],
                &[("If-Match", "\"nope\"")][..],
                &[("If-Match", "*")][..],
                &[("If-None-Match", "*")][..],
            ] {
                let a = n.send(&token, "PUT", target, headers, r#"{"title": "mine"}"#);
                assert_eq!(a.status, 403, "{policy} {target} {headers:?}: {a:?}");
                assert!(a.body.contains("PATCH /<name>/<id>"), "{a:?}");
            }
        }
        assert_eq!(n.get(1), before);
        assert_eq!(n.root("GET", "/notes/99", &[], "").status, 404);
        let w = n.send(
            &token,
            "PATCH",
            "/notes/1",
            &[("If-Match", &seen)],
            r#"{"title": "z"}"#,
        );
        assert_eq!(w.status, 200, "{w:?}");
    }
}

/// `If-Match` holds a write to a row by its path, and a statement names
/// none: `/query`, `/batch` and `/_call` refuse it (400) whatever they run,
/// nothing written -- a `set` sent under a tag it was never held to wrote
/// -- and `If-None-Match` on a write there, which on a read is the poll's.
/// `/_schema`'s writes refuse both.
#[test]
fn a_precondition_on_a_statement_is_refused() {
    let n = start();
    for def in [
        "create statement bump() public as set notes {n: 7} where id = 1",
        "create statement look() public as get notes select title where id = 1",
    ] {
        let a = n.statement("/query", def, &[]);
        assert_eq!(a.status, 200, "{a:?}");
    }
    let (before, tag) = n.get(1);
    let set = "set notes {n: 7} where id = 1";
    let writes = [("/query", set), ("/batch", set), ("/_call/bump", "")];
    let reads = [
        ("/query", "get notes where id = 1"),
        ("/batch", "get notes where id = 1"),
        ("/_call/look", ""),
    ];
    for (route, q) in writes.iter().chain(&reads) {
        for value in [tag.as_str(), "\"nope\"", "*"] {
            let a = n.statement(route, q, &[("If-Match", value)]);
            assert_eq!(a.status, 400, "{route} {q} If-Match {value}: {a:?}");
            assert!(
                a.body.contains("PATCH, PUT or DELETE /<name>/<id>"),
                "{a:?}"
            );
        }
    }
    for (route, q) in writes {
        for value in [tag.as_str(), "\"nope\"", "*"] {
            let a = n.statement(route, q, &[("If-None-Match", value)]);
            assert_eq!(a.status, 400, "{route} If-None-Match {value}: {a:?}");
        }
    }
    assert_eq!(n.get(1), (before, tag));
    // On a read, `If-None-Match` is the poll's, at `/query` and a call.
    let get = "get notes where id = 1";
    let first = n.statement("/query", get, &[("If-None-Match", "\"0\"")]);
    assert_eq!(first.status, 200, "{first:?}");
    let polled = first.etag.expect("a polled read's tag");
    let again = n.statement("/query", get, &[("If-None-Match", &polled)]);
    assert_eq!(again.status, 304, "{again:?}");
    assert_eq!(
        n.statement("/_call/look", "", &[("If-None-Match", "\"0\"")])
            .status,
        200
    );
    // `/_schema`'s plan and apply take neither.
    for header in ["If-Match", "If-None-Match"] {
        let a = n.root("POST", "/_schema/plan", &[(header, "*")], "{}");
        assert_eq!(a.status, 400, "{header}: {a:?}");
    }
    // Sent without them, the write is made.
    assert_eq!(n.statement("/query", set, &[]).status, 200);
    assert!(n.get(1).0.contains(r#""n":7"#));
}

/// A row a row-level rule hides from the token is answered as a row that
/// is not there, on every route and under every precondition: the same
/// status, the same words but for the id. The tag the server's token reads
/// of it names nothing to the scoped one.
#[test]
fn a_row_a_rule_hides_is_answered_as_a_missing_one() {
    let n = start_under(
        "notes  read,write  where owner = $jwt.sub\n",
        Database::new(),
        SETUP,
    );
    let k1 = n.access.mint(r#"{"sub":"k1"}"#).unwrap();
    assert_eq!(n.send(&k1, "GET", "/notes/1", &[], "").status, 200);
    // Row 2 is k2's; row 99 is no row.
    let (before, theirs) = n.get(2);
    let same = |method: &str, headers: &[(&str, &str)], body: &str| {
        let hidden = n.send(&k1, method, "/notes/2", headers, body);
        let missing = n.send(&k1, method, "/notes/99", headers, body);
        assert_eq!(
            (hidden.status, hidden.body.as_str(), &hidden.etag),
            (
                missing.status,
                missing.body.replace("99", "2").as_str(),
                &missing.etag
            ),
            "{method} {headers:?}"
        );
        hidden.status
    };
    for headers in [
        &[][..],
        &[("If-None-Match", "*")][..],
        &[("If-Match", "*")][..],
        &[("If-Match", theirs.as_str())][..],
    ] {
        assert_eq!(same("GET", headers, ""), 404, "{headers:?}");
    }
    for (method, body) in [("PATCH", r#"{"title": "mine"}"#), ("DELETE", "")] {
        assert_eq!(same(method, &[("If-Match", "*")], body), 412);
        assert_eq!(same(method, &[("If-Match", &theirs)], body), 412);
        assert_eq!(same(method, &[("If-None-Match", &theirs)], body), 200);
        assert_eq!(same(method, &[], body), 200);
    }
    assert_eq!(
        same("PUT", &[("If-None-Match", "*")], r#"{"title": "mine"}"#),
        403
    );
    assert_eq!(n.get(2), (before, theirs));
}

/// A row past its `@ttl` is no row, as every read has it: 404 by its path,
/// whatever its preconditions, and a write held to it is 412.
#[test]
fn a_row_past_its_ttl_is_no_row() {
    let n = start_on(
        Database::new(),
        &[
            "create collection holds (until timestamp @ttl(1ms), n int)",
            r#"put holds [{id: 1, until: 1, n: 0}, {id: 2, until: "2999-01-01", n: 0}]"#,
        ],
    );
    assert_eq!(n.root("GET", "/holds/2", &[], "").status, 200);
    for headers in [
        &[][..],
        &[("If-None-Match", "*")][..],
        &[("If-Match", "*")][..],
    ] {
        let gone = n.root("GET", "/holds/1", headers, "");
        let missing = n.root("GET", "/holds/99", headers, "");
        assert_eq!(gone.status, 404, "{headers:?}: {gone:?}");
        assert_eq!(gone.body, missing.body.replace("99", "1"));
    }
    for (method, body) in [
        ("PATCH", r#"{"n": 1}"#),
        ("PUT", r#"{"n": 1}"#),
        ("DELETE", ""),
    ] {
        let a = n.root(method, "/holds/1", &[("If-Match", "*")], body);
        assert_eq!(a.status, 412, "{method}: {a:?}");
    }
    let made = n.root("PUT", "/holds/1", &[("If-None-Match", "*")], r#"{"n": 1}"#);
    assert_eq!(made.status, 201, "{made:?}");
}

/// A write under `If-Match` sent again with its `Idempotency-Key` is
/// answered as the first time -- the row has moved past the tag since, by
/// that very write -- and made once.
#[test]
fn a_keyed_conditional_write_sent_again_is_its_first_answer() {
    let n = start();
    let (_, tag) = n.get(1);
    let headers = [("If-Match", tag.as_str()), ("Idempotency-Key", "k-1")];
    let first = n.root("PATCH", "/notes/1", &headers, r#"{"n": 1}"#);
    assert_eq!(first.status, 200, "{first:?}");
    let again = n.root("PATCH", "/notes/1", &headers, r#"{"n": 1}"#);
    assert_eq!((again.status, again.body), (200, first.body));
    // A refused one keeps no answer: sent again under its own tag it is made.
    let (_, now) = n.get(1);
    let stale = [("If-Match", tag.as_str()), ("Idempotency-Key", "k-2")];
    assert_eq!(
        n.root("PATCH", "/notes/1", &stale, r#"{"n": 2}"#).status,
        412
    );
    let fresh = [("If-Match", now.as_str()), ("Idempotency-Key", "k-2")];
    assert_eq!(
        n.root("PATCH", "/notes/1", &fresh, r#"{"n": 2}"#).status,
        200
    );
}

/// The tag is the row's, not the process's: a server opening the file again
/// tags each row as the one before did.
#[test]
fn a_rows_tag_outlives_its_server() {
    let dir = std::env::temp_dir().join(format!("fennec-conditional-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tags.fennec");
    let before = {
        let mut db = fennec_core::fs::open(&path).unwrap();
        for sql in SETUP
            .iter()
            .chain(&[r#"set notes {title: "kept"} where id = 2"#])
        {
            db.execute(&fennec_ql::parse_one(sql).unwrap()).unwrap();
        }
        db.sync().unwrap();
        let n = start_on(db, &[]);
        [n.get(1), n.get(2)]
    };
    // Its server's thread holds the database to the end of the test: the
    // file is read again as a second process would read it.
    let again = fennec_core::fs::open_read_only(&path).unwrap();
    let n = start_on(again, &[]);
    assert_eq!([n.get(1), n.get(2)], before);
    let _ = std::fs::remove_dir_all(&dir);
}

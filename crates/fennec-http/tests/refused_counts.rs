//! The connections a server answered as it took them, counted by why --
//! `fennec_connections_refused_total{reason}` -- and those it ended for a
//! client too slow, by the bound each ran past:
//! `fennec_client_timeouts_total{reason}`. A binary of its own: the counts
//! are the process's, and the other tests' servers refuse and cut too.
//! Neither test here makes what the other counts.

use fennec_core::prelude::*;
use fennec_http::{Config, Server};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// A keep-alive connection, the scrapes asked over it: past the ceiling,
/// a new connection would be refused, the scrape's own among them.
struct Kept {
    r: BufReader<TcpStream>,
    w: TcpStream,
}

impl Kept {
    fn open(port: u16) -> Kept {
        let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        Kept {
            r: BufReader::new(s.try_clone().unwrap()),
            w: s,
        }
    }

    /// `GET target`'s status and body.
    fn get(&mut self, target: &str) -> (u16, String) {
        write!(self.w, "GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        let mut line = String::new();
        self.r.read_line(&mut line).unwrap();
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut length = 0;
        loop {
            line.clear();
            self.r.read_line(&mut line).unwrap();
            let l = line.trim_end();
            if l.is_empty() {
                break;
            }
            if let Some((k, v)) = l.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    length = v.trim().parse().unwrap();
                }
            }
        }
        let mut body = vec![0u8; length];
        self.r.read_exact(&mut body).unwrap();
        (status, String::from_utf8(body).unwrap())
    }

    /// The count of connections refused for `reason`.
    fn refused(&mut self, reason: &str) -> u64 {
        self.count("fennec_connections_refused_total", reason)
    }

    /// The counts of connections ended for a slow client: `header`, `idle`
    /// and `write`.
    fn timed_out(&mut self) -> [u64; 3] {
        ["header", "idle", "write"].map(|r| self.count("fennec_client_timeouts_total", r))
    }

    /// `family`'s sample for `reason`.
    fn count(&mut self, family: &str, reason: &str) -> u64 {
        let (status, body) = self.get("/_metrics");
        assert_eq!(status, 200, "{body}");
        let line = format!("{family}{{reason=\"{reason}\"}} ");
        body.lines()
            .find_map(|l| l.strip_prefix(line.as_str()))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no {line}in {body}"))
    }
}

/// `db` served under `cfg` on a port of loopback's: the port.
fn serve(cfg: Config, db: Database) -> u16 {
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        ..cfg
    };
    let server = Server::new(Arc::new(RwLock::new(db)), cfg);
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    port
}

/// `head` sent, then `trickle` a byte every 50 ms, until the server answers
/// and ends the connection, 30 s at most: the answer.
fn sent_slowly(port: u16, head: &[u8], trickle: bool) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_nodelay(true).unwrap();
    s.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    s.write_all(head).unwrap();
    let (started, mut out, mut buf) = (Instant::now(), Vec::new(), [0u8; 512]);
    while started.elapsed() < Duration::from_secs(30) {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if trickle && out.is_empty() && s.write_all(b"a").is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A connection of its own, its request sent: the answer's status line.
fn refused_with(port: u16) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(b"GET /collections HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

/// A connection given no thread counts under `no_thread`, one past the
/// ceiling under `ceiling`, each by one and the other not at all.
#[test]
fn a_refused_connection_is_counted_by_why() {
    fennec_http::audit::set_delay(0);
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        max_connections: 2,
        ..Config::default()
    };
    let server = Server::new(Arc::new(RwLock::new(Database::new())), cfg);
    let fail = server.fail_spawns();
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    let mut scrape = Kept::open(port);
    let (ceiling, no_thread) = (scrape.refused("ceiling"), scrape.refused("no_thread"));

    // A second connection is within the ceiling of two: its thread is all
    // it lacks.
    fail.next(1);
    let out = refused_with(port);
    assert!(out.starts_with("HTTP/1.1 503 "), "{out}");
    assert!(out.contains("\r\nFennec-Unserved: no_thread\r\n"), "{out}");
    assert_eq!(scrape.refused("no_thread"), no_thread + 1);
    assert_eq!(scrape.refused("ceiling"), ceiling);

    // Two connections served, and the third past the ceiling.
    let mut second = Kept::open(port);
    assert_eq!(second.get("/collections").0, 200);
    let out = refused_with(port);
    assert!(out.starts_with("HTTP/1.1 503 "), "{out}");
    assert!(out.contains("\r\nFennec-Unserved: ceiling\r\n"), "{out}");
    assert!(out.contains("too many connections"), "{out}");
    assert_eq!(scrape.refused("ceiling"), ceiling + 1);
    assert_eq!(scrape.refused("no_thread"), no_thread + 1);
}

/// A request not whole in its time, one silent part way and an answer its
/// client takes nothing of are each counted once, under the bound it ran
/// past, before its 408 is read; nothing is refused, so the count of
/// refusals beside it holds.
#[test]
fn a_slow_client_is_counted_by_the_bound_it_ran_past() {
    fennec_http::audit::set_delay(0);
    let ms = Duration::from_millis;
    // A row of a mebibyte, its server's request and write timeouts 300 ms.
    let mut db = Database::new();
    for q in [
        "create collection t (s text)".to_string(),
        format!("put t {{id: 1, s: '{}'}}", "x".repeat(1 << 20)),
    ] {
        db.execute(&fennec_ql::parse_one(&q).unwrap()).unwrap();
    }
    let timed = Config {
        header_timeout: Some(ms(300)),
        write_timeout: Some(ms(300)),
        ..Config::default()
    };
    let port = serve(timed, db);
    // And one with no `--header-timeout`, its idle timeout 300 ms.
    let idle = Config {
        header_timeout: None,
        idle_timeout: Some(ms(300)),
        ..Config::default()
    };
    let quiet = serve(idle, Database::new());
    let mut scrape = Kept::open(port);
    let [header, idle, write] = scrape.timed_out();

    // A head trickled a byte every 50 ms: cut at its time.
    let out = sent_slowly(port, b"GET /collections HTTP/1.1\r\nX-Trickled: ", true);
    assert!(out.starts_with("HTTP/1.1 408 "), "{out}");
    assert_eq!(scrape.timed_out(), [header + 1, idle, write]);

    // Half a head, then nothing: the idle timeout's.
    let out = sent_slowly(quiet, b"GET /collections HTTP/1.1\r\n", false);
    assert!(out.starts_with("HTTP/1.1 408 "), "{out}");
    assert_eq!(scrape.timed_out(), [header + 1, idle + 1, write]);

    // Forty answers of a mebibyte asked and none read: the write that
    // took nothing for 300 ms ends the connection.
    let mut unread = TcpStream::connect(("127.0.0.1", port)).unwrap();
    unread
        .write_all(&b"GET /t/1 HTTP/1.1\r\nHost: localhost\r\n\r\n".repeat(40))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut now = scrape.timed_out();
    while now[2] == write {
        assert!(Instant::now() < deadline, "no write was cut: {now:?}");
        std::thread::sleep(ms(20));
        now = scrape.timed_out();
    }
    assert_eq!(now, [header + 1, idle + 1, write + 1]);
    drop(unread);
}

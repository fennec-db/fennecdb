//! The connections a router answered as it took them, counted by why --
//! `fennec_router_connections_refused_total{reason}` -- and those it ended
//! for a client too slow: `fennec_router_client_timeouts_total{reason}`. A
//! binary of its own: the counts are the process's -- the router's and any
//! node's in it alike -- and the other tests' servers refuse and cut too.
//! No node here, so the counts are the router's alone, and neither test
//! makes what the other counts.

use fennec_shard::directory::Directory;
use fennec_shard::{Config, Router};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// A keep-alive connection, the scrapes asked over it: past the ceiling,
/// a new connection would be refused, the scrape's own among them.
struct Kept {
    r: BufReader<TcpStream>,
    w: TcpStream,
}

impl Kept {
    fn open(addr: &str) -> Kept {
        let s = TcpStream::connect(addr).unwrap();
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
        self.count("fennec_router_connections_refused_total", reason)
    }

    /// The counts of connections ended for a slow client: `header`, `idle`
    /// and `write`.
    fn timed_out(&mut self) -> [u64; 3] {
        ["header", "idle", "write"].map(|r| self.count("fennec_router_client_timeouts_total", r))
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

/// A connection of its own, its request sent: the whole answer.
fn refused_with(addr: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(b"GET /t/acme/notes HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

/// A connection given no thread counts under `no_thread`, one past the
/// ceiling under `ceiling`, each by one and the other not at all, both
/// marked the router's and unserved.
#[test]
fn a_refused_connection_is_counted_by_why() {
    fennec_http::audit::set_delay(0);
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        max_connections: 2,
        ..Config::default()
    };
    let router = Router::new(Directory::in_memory(), cfg);
    let fail = router.fail_spawns();
    let listener = router.bind().unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let _ = router.serve_on(listener);
    });
    let mut scrape = Kept::open(&addr);
    let (ceiling, no_thread) = (scrape.refused("ceiling"), scrape.refused("no_thread"));

    // A second connection is within the ceiling of two: its thread is all
    // it lacks.
    fail.next(1);
    let out = refused_with(&addr);
    assert!(out.starts_with("HTTP/1.1 503 "), "{out}");
    assert!(out.contains("\r\nFennec-Shard: 1\r\n"), "{out}");
    assert!(out.contains("\r\nFennec-Unserved: no_thread\r\n"), "{out}");
    assert_eq!(scrape.refused("no_thread"), no_thread + 1);
    assert_eq!(scrape.refused("ceiling"), ceiling);

    // Two connections served, and the third past the ceiling.
    let mut second = Kept::open(&addr);
    assert_eq!(second.get("/_metrics").0, 200);
    let out = refused_with(&addr);
    assert!(out.starts_with("HTTP/1.1 503 "), "{out}");
    assert!(out.contains("\r\nFennec-Unserved: ceiling\r\n"), "{out}");
    assert!(out.contains("too many connections"), "{out}");
    assert_eq!(scrape.refused("ceiling"), ceiling + 1);
    assert_eq!(scrape.refused("no_thread"), no_thread + 1);
}

/// A head trickled a byte every 50 ms is cut at the router's
/// `--header-timeout` with its own 408, counted under `header` before the
/// answer is read, the other reasons not; nothing is refused, so the count
/// of refusals beside it holds.
#[test]
fn a_slow_client_is_counted_by_the_bound_it_ran_past() {
    fennec_http::audit::set_delay(0);
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        header_timeout: Some(Duration::from_millis(300)),
        ..Config::default()
    };
    let router = Router::new(Directory::in_memory(), cfg);
    let listener = router.bind().unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let _ = router.serve_on(listener);
    });
    let mut scrape = Kept::open(&addr);
    let [header, idle, write] = scrape.timed_out();

    let mut s = TcpStream::connect(&addr).unwrap();
    s.set_nodelay(true).unwrap();
    s.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    s.write_all(b"GET /t/acme/notes HTTP/1.1\r\nX-Trickled: ")
        .unwrap();
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
                if out.is_empty() && s.write_all(b"a").is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let out = String::from_utf8_lossy(&out);
    assert!(out.starts_with("HTTP/1.1 408 "), "{out}");
    assert_eq!(scrape.timed_out(), [header + 1, idle, write]);
}

//! What scoped callers' requests cost a server asked from several threads
//! at once, `--request-rate`'s table of callers among it: CLIENTS (8)
//! threads, each one keep-alive connection, ask `GET /t?id=eq.<id>` for
//! SECS (3) seconds against a server in this process, each as its share of
//! 1 000 subjects in turn (`scoped`) or under the server's token (`token`)
//! -- `make request-rate-bench MODE=... CLIENTS=... SECS=...`. It prints
//! the requests answered a second and the
//! p50 and p99 of one, and refuses to print a figure if any request was
//! answered other than 200: the default rate is never reached, each
//! subject asking a few hundred times a second at most. A connection a
//! client: the load is bounded, never a client a request.

use fennec_core::prelude::Database;
use fennec_http::access::Access;
use fennec_http::{Config, Server};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, RwLock};
use std::time::{Duration, Instant};

const ROWS: usize = 500;
const CALLERS: usize = 1000;
const ROOT: &str = "root-token";
const SECRET: &[u8] = b"thirty-two bytes and a few more, for HS256";

/// One keep-alive connection: a request, its status and its body read.
struct Conn {
    out: TcpStream,
    input: BufReader<TcpStream>,
}

impl Conn {
    fn open(port: u16) -> Conn {
        let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_nodelay(true).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        Conn {
            input: BufReader::new(s.try_clone().unwrap()),
            out: s,
        }
    }

    fn ask(&mut self, method: &str, target: &str, token: &str, body: &str) -> u16 {
        write!(
            self.out,
            "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut line = String::new();
        self.input.read_line(&mut line).unwrap();
        let status: u16 = line[9..12].parse().unwrap();
        let mut length = 0usize;
        loop {
            line.clear();
            self.input.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            let lower = line.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("content-length:") {
                length = v.trim().parse().unwrap();
            }
        }
        let mut body = vec![0u8; length];
        self.input.read_exact(&mut body).unwrap();
        status
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "scoped".into());
    let clients: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(8);
    let secs: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(3);
    let access = Arc::new(Access::new(SECRET, "t read\n").unwrap());
    let db = Arc::new(RwLock::new(Database::new()));
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some(ROOT.into()),
        access: Some(Arc::clone(&access)),
        ..Config::default()
    };
    let server = Server::new(Arc::clone(&db), cfg);
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    let mut admin = Conn::open(port);
    let q = |sql: String| format!(r#"{{"query":"{sql}"}}"#);
    let made = admin.ask(
        "POST",
        "/query",
        ROOT,
        &q("create collection t (n int, name text)".into()),
    );
    assert_eq!(made, 200);
    for id in 1..=ROWS {
        let put = q(format!(
            "put t {{id: {id}, n: 0, name: 'xxxxxxxxxxxxxxxxxxxx'}}"
        ));
        assert_eq!(admin.ask("POST", "/query", ROOT, &put), 200);
    }
    let tokens: Arc<Vec<String>> = Arc::new(match mode.as_str() {
        "scoped" => (0..CALLERS)
            .map(|i| access.mint(&format!(r#"{{"sub":"user-{i}"}}"#)).unwrap())
            .collect(),
        "token" => vec![ROOT.to_string()],
        other => panic!("no mode {other}: scoped or token"),
    });
    // Every token checked once, as a server's users' are by the time they
    // ask again: the run reads them from the verified tokens' cache.
    for t in tokens.iter() {
        assert_eq!(admin.ask("GET", "/t?id=eq.1", t, ""), 200);
    }
    let start = Arc::new(Barrier::new(clients + 1));
    let stop = Arc::new(AtomicBool::new(false));
    let threads: Vec<_> = (0..clients)
        .map(|c| {
            let (tokens, start, stop) =
                (Arc::clone(&tokens), Arc::clone(&start), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut conn = Conn::open(port);
                let mut took = Vec::with_capacity(1 << 20);
                let mut refused = 0u64;
                let mut i = c;
                start.wait();
                while !stop.load(Ordering::Relaxed) {
                    let token = &tokens[i % tokens.len()];
                    let target = format!("/t?id=eq.{}", i % ROWS + 1);
                    let t = Instant::now();
                    if conn.ask("GET", &target, token, "") != 200 {
                        refused += 1;
                    }
                    took.push(t.elapsed().as_nanos() as u32);
                    i += clients;
                }
                (took, refused)
            })
        })
        .collect();
    start.wait();
    let began = Instant::now();
    std::thread::sleep(Duration::from_secs(secs));
    stop.store(true, Ordering::Relaxed);
    let mut all = Vec::new();
    let mut refused = 0;
    for t in threads {
        let (took, r) = t.join().unwrap();
        all.extend(took);
        refused += r;
    }
    let elapsed = began.elapsed().as_secs_f64();
    assert_eq!(
        refused, 0,
        "requests answered other than 200: the figures would be theirs"
    );
    all.sort_unstable();
    let at = |q: f64| all[((all.len() as f64 * q) as usize).min(all.len() - 1)] as f64 / 1000.0;
    println!("mode {mode}, {clients} clients, {} requests", all.len());
    println!("requests a second {:.0}", all.len() as f64 / elapsed);
    println!("p50 {:.1} us, p99 {:.1} us", at(0.5), at(0.99));
}

//! What a request costs through the router, counted in instructions: `make
//! instr-compare PKG=fennec-shard EXAMPLE=routed MODES=read` (`cargo run
//! --release -p fennec-shard --example routed -- MODE [N]`), one mode a
//! process. `overhead.rs` times the router against the node it fronts;
//! this counts what a build changes on the way through, as fennec-http's
//! `served.rs` does for a node.
//!
//!   * `read`: `POST /t/acme/query` reading a row by id of 500 (`get t
//!     where id = $1`), on one kept-alive connection to the router, each
//!     answer read before the next request is sent: the router's read of
//!     the request, its forward on a pooled connection, the node's answer
//!     and its relay.
//!   * `pipelined`: that request, 16 written at once before their answers
//!     are read: each but the first in the router's buffer as its turn
//!     comes.
//!
//! The node, the router and the client are threads of this process, over
//! loopback. It prints ns a request, the median of 9 rounds of N / 9, and
//! on macOS the instructions and cycles the loop retired
//! (`proc_pid_rusage`), every thread's together: between two builds the
//! client's are the same code.

use fennec_http::tenants::Tenants;
use fennec_shard::directory::{Directory, Node, State};
use fennec_shard::{Config, Router};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Instant;

const ROUNDS: usize = 9;
const ROWS: usize = 500;
/// Requests written at once in `pipelined`.
const DEPTH: usize = 16;

fn request(path: &str, body: &str) -> Vec<u8> {
    format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// One kept-alive connection to the router.
struct Client {
    out: TcpStream,
    input: BufReader<TcpStream>,
    line: String,
    body: Vec<u8>,
}

impl Client {
    fn connect(port: u16) -> Client {
        let out = TcpStream::connect(("127.0.0.1", port)).unwrap();
        out.set_nodelay(true).unwrap();
        Client {
            input: BufReader::new(out.try_clone().unwrap()),
            out,
            line: String::new(),
            body: Vec::new(),
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.out.write_all(bytes).unwrap();
    }

    /// One answer, a success, read whole.
    fn answer(&mut self) {
        self.line.clear();
        self.input.read_line(&mut self.line).unwrap();
        let ok = self.line.get(9..10) == Some("2");
        let status = (!ok).then(|| self.line.clone());
        let mut length = 0usize;
        loop {
            self.line.clear();
            self.input.read_line(&mut self.line).unwrap();
            if self.line == "\r\n" {
                break;
            }
            if let Some((k, v)) = self.line.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    length = v.trim().parse().unwrap();
                }
            }
        }
        self.body.resize(length, 0);
        self.input.read_exact(&mut self.body).unwrap();
        if let Some(status) = status {
            panic!("{status}{}", String::from_utf8_lossy(&self.body));
        }
    }

    fn ask(&mut self, bytes: &[u8]) {
        self.send(bytes);
        self.answer();
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "read".into());
    let n: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(20_000);
    if !matches!(mode.as_str(), "read" | "pipelined") {
        panic!("no mode {mode}: read or pipelined");
    }
    let dir = std::env::temp_dir().join(format!("fennec-routed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let tenants = Arc::new(Tenants::new(&dir).unwrap());
    tenants.create("acme").unwrap();
    let node = fennec_http::Server::with_tenants(
        tenants,
        fennec_http::Config {
            addr: "127.0.0.1:0".into(),
            admin_token: Some("bench-admin-token".into()),
            ..fennec_http::Config::default()
        },
    );
    let listener = node.bind().unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let _ = node.serve_on(listener);
    });
    let mut d = Directory::in_memory();
    let placed = Node {
        addr,
        token: "bench-admin-token".into(),
        ..Node::default()
    };
    d.set_node("n1", placed).unwrap();
    d.place("acme", "n1", State::Active).unwrap();
    let router = Router::new(
        d,
        Config {
            addr: "127.0.0.1:0".into(),
            ..Config::default()
        },
    );
    let listener = router.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = router.serve_on(listener);
    });

    let mut c = Client::connect(port);
    let query = "/t/acme/query";
    c.ask(&request(
        query,
        r#"{"query":"create collection t (n int, s text)"}"#,
    ));
    let rows: Vec<String> = (1..=ROWS)
        .map(|id| format!("{{id: {id}, n: {id}, s: 'row {id}'}}"))
        .collect();
    c.ask(&request(
        query,
        &format!(r#"{{"query":"put t [{}]"}}"#, rows.join(", ")),
    ));
    let reads: Vec<Vec<u8>> = (1..=ROWS)
        .map(|id| {
            let body = format!(r#"{{"query":"get t where id = $1","params":[{id}]}}"#);
            request(query, &body)
        })
        .collect();
    let batch: Vec<u8> = reads[..DEPTH].concat();
    // Warm: the parse cache, the pool's connection, the rows read once.
    for i in 0..1_000 {
        c.ask(&reads[i % ROWS]);
    }

    let per = n / ROUNDS;
    let mut rounds = Vec::with_capacity(ROUNDS);
    let (mut i, mut total) = (0usize, 0usize);
    let before = retired();
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let mut sent = 0;
        while sent < per {
            i += 1;
            if mode == "read" {
                c.ask(&reads[i % ROWS]);
                sent += 1;
            } else {
                c.send(&batch);
                for _ in 0..DEPTH {
                    c.answer();
                }
                sent += DEPTH;
            }
        }
        total += sent;
        rounds.push(t.elapsed().as_nanos() as f64 / sent as f64);
    }
    let after = retired();
    rounds.sort_by(|a, b| a.total_cmp(b));
    println!("mode {mode}, {total} requests");
    println!("ns a request {:.0}", rounds[ROUNDS / 2]);
    if let (Some((i0, c0)), Some((i1, c1))) = (before, after) {
        println!("instructions in the loop {}", i1 - i0);
        println!("cycles in the loop {}", c1 - c0);
        println!(
            "a request: {:.1} instructions, {:.1} cycles",
            (i1 - i0) as f64 / total as f64,
            (c1 - c0) as f64 / total as f64
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The instructions and cycles this process retired so far, every thread's
/// (`proc_pid_rusage`, `RUSAGE_INFO_V4`), as fennec-http's `replays.rs`
/// reads them.
#[cfg(target_os = "macos")]
fn retired() -> Option<(u64, u64)> {
    extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u64) -> i32;
    }
    let mut info = [0u64; 37];
    let r = unsafe { proc_pid_rusage(std::process::id() as i32, 4, info.as_mut_ptr()) };
    (r == 0).then_some((info[31], info[32]))
}

#[cfg(not(target_os = "macos"))]
fn retired() -> Option<(u64, u64)> {
    None
}

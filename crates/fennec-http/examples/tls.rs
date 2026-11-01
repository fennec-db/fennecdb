//! What HTTPS served by fennec-server itself costs, and that plain HTTP
//! costs what it did: `make tls-bench` (`BASE=<another fennec-server>` to
//! hold plain HTTP to a build from before, in turns).
//!
//! Each turn starts a server in memory with 10 000 rows and asks it a row by
//! id (`POST /query`, the id a parameter) on connections kept alive: one
//! client at a time for the round trip's p50 and p99, and eight at once for
//! requests a second. Plain HTTP on the base binary and on this one, then
//! HTTPS on this one; and connections a second, a request each with
//! `Connection: close`, plain and with a full TLS handshake. The servers
//! share the machine with the clients, so a figure is the pair's.
//!
//! A round runs only once the machine's 1-minute load is under 2.5, and the
//! load each turn began at is printed beside it: a figure made beside
//! another's work is not one (a round's own turns raise it for the next).
//! The turns rotate round to round, each side first, middle and last in
//! turn, and each figure printed is its median over the rounds.
//!
//! Connections a turn are bounded (`CONNECTIONS`), each ended by the server
//! (`Connection: close`), so the `TIME_WAIT`s are the server's port's and
//! never use up the clients' ports.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const ROWS: usize = 10_000;
const ONE: usize = 20_000;
const CLIENTS: usize = 8;
const RUN: Duration = Duration::from_secs(3);
const CONNECTIONS: usize = 1_000;
const QUIET: f64 = 2.5;

extern "C" {
    fn getloadavg(loadavg: *mut f64, nelem: i32) -> i32;
}

fn load() -> f64 {
    let mut l = [0f64; 3];
    // SAFETY: three doubles, which outlive the call.
    if unsafe { getloadavg(l.as_mut_ptr(), 3) } < 1 {
        return f64::NAN;
    }
    l[0]
}

/// Waits for the 1-minute load to fall under [`QUIET`], saying so.
fn quiet() -> f64 {
    let mut said = false;
    loop {
        let l = load();
        if l < QUIET {
            return l;
        }
        if !said {
            eprintln!("waiting for the 1-minute load ({l:.2}) to fall under {QUIET}");
            said = true;
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}

fn certs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/certs")
}

struct Server {
    child: Child,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(bin: &str, tls: bool) -> Server {
    let mut cmd = Command::new(bin);
    cmd.args(["--http", "127.0.0.1:0", "--max-connections", "0"]);
    if tls {
        let (c, k) = (certs().join("server.pem"), certs().join("server.key"));
        cmd.arg("--tls-cert").arg(c).arg("--tls-key").arg(k);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{bin}: {e}"));
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    let port = loop {
        line.clear();
        assert!(
            err.read_line(&mut line).unwrap() > 0,
            "{bin} ended before it listened"
        );
        if let Some((_, rest)) = line.split_once("listening on: ") {
            let addr = rest.split_whitespace().next().unwrap();
            break addr.rsplit_once(':').unwrap().1.parse().unwrap();
        }
    };
    // Its log drained, so a full pipe never stops it.
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut err, &mut std::io::sink());
    });
    Server { child, port }
}

/// A connection, plain or TLS, asking one request at a time.
enum Stream {
    Plain(TcpStream),
    Tls(Box<fennec_tls::TlsStream>),
}

impl Read for Stream {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(b),
            Stream::Tls(s) => s.read(b),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(b),
            Stream::Tls(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

fn client() -> fennec_tls::Client {
    fennec_tls::Client::trusting_pem_file(&certs().join("ca.pem"))
        .unwrap()
        .alpn(&[b"http/1.1"])
}

fn dial(port: u16, tls: Option<&fennec_tls::Client>) -> BufReader<Stream> {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_nodelay(true).unwrap();
    let s = match tls {
        None => Stream::Plain(s),
        Some(c) => Stream::Tls(Box::new(c.dial(s, "localhost").unwrap())),
    };
    BufReader::with_capacity(1 << 16, s)
}

/// Writes `req` and reads its answer by its length: the status.
fn ask(c: &mut BufReader<Stream>, req: &[u8], body: &mut Vec<u8>) -> u16 {
    c.get_mut().write_all(req).unwrap();
    c.get_mut().flush().unwrap();
    let mut line = String::new();
    c.read_line(&mut line).unwrap();
    let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    let mut len = 0;
    loop {
        line.clear();
        c.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length: ") {
            len = v.trim().parse().unwrap();
        }
    }
    body.resize(len, 0);
    c.read_exact(body).unwrap();
    status
}

fn post(q: &str, params: &str, close: bool) -> Vec<u8> {
    let body = format!("{{\"query\":\"{q}\",\"params\":[{params}]}}");
    let close = if close { "Connection: close\r\n" } else { "" };
    let mut r = format!(
        "POST /query HTTP/1.1\r\nHost: b\r\n{close}Content-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    r.extend_from_slice(body.as_bytes());
    r
}

fn load_rows(port: u16, tls: Option<&fennec_tls::Client>) {
    let mut c = dial(port, tls);
    let mut body = Vec::new();
    assert_eq!(
        ask(
            &mut c,
            &post("create collection items (n int)", "", false),
            &mut body
        ),
        200
    );
    for chunk in (0..ROWS).collect::<Vec<_>>().chunks(1000) {
        let docs: Vec<String> = chunk
            .iter()
            .map(|i| format!("{{id: {}, n: {i}}}", i + 1))
            .collect();
        let q = format!("put items {}", docs.join(", "));
        let status = ask(&mut c, &post(&q, "", false), &mut body);
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    }
}

fn by_id(i: usize) -> Vec<u8> {
    post(
        "get items where id = $1",
        &(i % ROWS + 1).to_string(),
        false,
    )
}

/// One client: each round trip timed, its p50 and p99 in microseconds.
fn one(port: u16, tls: Option<&fennec_tls::Client>) -> (f64, f64) {
    let mut c = dial(port, tls);
    let reqs: Vec<Vec<u8>> = (0..1000).map(by_id).collect();
    let mut body = Vec::new();
    for r in reqs.iter().take(500) {
        ask(&mut c, r, &mut body);
    }
    let mut took = Vec::with_capacity(ONE);
    for i in 0..ONE {
        let t = Instant::now();
        assert_eq!(ask(&mut c, &reqs[i % reqs.len()], &mut body), 200);
        took.push(t.elapsed().as_nanos() as f64 / 1000.0);
    }
    took.sort_by(f64::total_cmp);
    (took[ONE / 2], took[ONE * 99 / 100])
}

/// [`CLIENTS`] clients at once for [`RUN`]: requests a second.
fn many(port: u16, tls: Option<&fennec_tls::Client>) -> f64 {
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicU64::new(0));
    let threads: Vec<_> = (0..CLIENTS)
        .map(|k| {
            let (stop, done) = (Arc::clone(&stop), Arc::clone(&done));
            let tls = tls.cloned();
            std::thread::spawn(move || {
                let mut c = dial(port, tls.as_ref());
                let reqs: Vec<Vec<u8>> = (0..1000).map(|i| by_id(i * 7 + k)).collect();
                let mut body = Vec::new();
                let mut n = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    assert_eq!(ask(&mut c, &reqs[n as usize % reqs.len()], &mut body), 200);
                    n += 1;
                }
                done.fetch_add(n, Ordering::Relaxed);
            })
        })
        .collect();
    let started = Instant::now();
    std::thread::sleep(RUN);
    stop.store(true, Ordering::Relaxed);
    for t in threads {
        t.join().unwrap();
    }
    done.load(Ordering::Relaxed) as f64 / started.elapsed().as_secs_f64()
}

/// [`CONNECTIONS`] connections over [`CLIENTS`] clients, each one request
/// with `Connection: close` and read to its end: connections a second.
fn connections(port: u16, tls: Option<&fennec_tls::Client>) -> f64 {
    let each = CONNECTIONS / CLIENTS;
    let started = Instant::now();
    let threads: Vec<_> = (0..CLIENTS)
        .map(|k| {
            let tls = tls.cloned();
            std::thread::spawn(move || {
                let mut body = Vec::new();
                for i in 0..each {
                    let mut c = dial(port, tls.as_ref());
                    let req = post(
                        "get items where id = $1",
                        &((i * 13 + k) % ROWS + 1).to_string(),
                        true,
                    );
                    assert_eq!(ask(&mut c, &req, &mut body), 200);
                    // The server's end first: its TIME_WAIT, not this port's.
                    let _ = c.read_to_end(&mut Vec::new());
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    (each * CLIENTS) as f64 / started.elapsed().as_secs_f64()
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

struct Figures {
    p50: Vec<f64>,
    p99: Vec<f64>,
    rps: Vec<f64>,
    conns: Vec<f64>,
}

impl Figures {
    fn new() -> Figures {
        Figures {
            p50: Vec::new(),
            p99: Vec::new(),
            rps: Vec::new(),
            conns: Vec::new(),
        }
    }
}

fn turn(bin: &str, tls: bool, into: &mut Figures, name: &str) {
    let server = start(bin, tls);
    let client = tls.then(client);
    load_rows(server.port, client.as_ref());
    let at = load();
    let (p50, p99) = one(server.port, client.as_ref());
    let rps = many(server.port, client.as_ref());
    let conns = connections(server.port, client.as_ref());
    println!(
        "  {name:<12} p50 {p50:>6.1} us  p99 {p99:>6.1} us  {rps:>8.0} req/s  {conns:>6.0} conn/s  (load {at:.2})"
    );
    into.p50.push(p50);
    into.p99.push(p99);
    into.rps.push(rps);
    into.conns.push(conns);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(bin) = args.first().cloned() else {
        eprintln!("usage: tls <fennec-server> [<base fennec-server> [rounds]]");
        std::process::exit(2);
    };
    let base = args.get(1).cloned();
    let rounds: usize = args.get(2).map_or(5, |r| r.parse().unwrap());
    let mut sides: Vec<(String, &str, bool, Figures)> = Vec::new();
    if let Some(b) = &base {
        sides.push((b.clone(), "base http", false, Figures::new()));
    }
    sides.push((bin.clone(), "http", false, Figures::new()));
    // `PLAIN_ONLY=1`: two builds without TLS, held to each other alone.
    if std::env::var_os("PLAIN_ONLY").is_none() {
        sides.push((bin.clone(), "https", true, Figures::new()));
    }
    println!(
        "a row by id over {ROWS} rows: one client ({ONE} round trips), {CLIENTS} at once for \
         {RUN:?}, {CONNECTIONS} connections of a request each; {rounds} rounds"
    );
    for round in 0..rounds {
        let l = quiet();
        println!("round {} (load {l:.2})", round + 1);
        // Each side first, middle and last in turn: a turn warms the
        // machine for the next, and reversed alone the middle stayed the
        // middle.
        let n = sides.len();
        for i in (0..n).map(|k| (k + round) % n) {
            let (bin, name, tls, figures) = &mut sides[i];
            turn(bin, *tls, figures, name);
        }
    }
    println!("medians over {rounds} rounds:");
    for (_, name, _, f) in &mut sides {
        println!(
            "  {name:<12} p50 {:>6.1} us  p99 {:>6.1} us  {:>8.0} req/s  {:>6.0} conn/s",
            median(&mut f.p50),
            median(&mut f.p99),
            median(&mut f.rps),
            median(&mut f.conns)
        );
    }
}

//! What the router dialing its nodes over TLS costs, and that dialing them
//! in plain HTTP costs what it did: `make tls-dial-bench`
//! (`BASE=<another fennec-shard>` to hold plain relays to a build from
//! before, in turns).
//!
//! Relays, between processes: a `--dir` node serving plain HTTP and one
//! serving HTTPS (`--tls-cert`), a tenant of 10 000 rows on each, and a
//! router in front -- BASE's over the plain node, this tree's over the
//! plain node, this tree's over the HTTPS one (`--tls-ca`) -- asked a row
//! by id on connections kept alive: one client for the round trip's p50
//! and p99, eight at once for requests a second. The client speaks plain
//! HTTP to the router; what differs is the router's dial to its node.
//!
//! Lease rounds, in this process: three leasing nodes serving plain HTTP
//! and three serving HTTPS, a router over each three, a tenant on each
//! node, and its `lease_round` timed, `LEASE_ROUNDS` of them over the
//! connections it keeps: p50 and p99. And a dial alone -- lookup, connect
//! and over TLS the handshake, what a grant to a node the pool holds no
//! connection to pays first -- to a node of each.
//!
//! A round of turns runs only once the machine's 1-minute load is under
//! 2.5, the load each turn began at printed beside it: a figure made
//! beside another's work is not one. The turns rotate round to round, and
//! each figure printed last is its median over the rounds.

use fennec_http::tenants::{Replicated, Tenants};
use fennec_shard::directory::Directory;
use fennec_shard::{Config, Router};
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
const QUIET: f64 = 2.5;
const LEASE_ROUNDS: usize = 200;
const DIALS: usize = 200;

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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fennec-http/tests/certs")
}

fn cert(name: &str) -> String {
    certs().join(name).to_string_lossy().into_owned()
}

struct Process {
    child: Child,
    port: u16,
}

impl Drop for Process {
    fn drop(&mut self) {
        // SIGTERM, its one shutdown, then waited for: a router lets go of
        // its directory's lock for the next turn's.
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `bin` with `args`, waited for until it says where it listens.
fn start(bin: &str, args: &[&str]) -> Process {
    let mut child = Command::new(bin)
        .args(args)
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
    Process { child, port }
}

fn dial(port: u16) -> BufReader<TcpStream> {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_nodelay(true).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    BufReader::with_capacity(1 << 16, s)
}

/// Writes `req` and reads its answer by its length: the status.
fn ask(c: &mut BufReader<TcpStream>, req: &[u8], body: &mut Vec<u8>) -> u16 {
    c.get_mut().write_all(req).unwrap();
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
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().unwrap();
            }
        }
    }
    body.resize(len, 0);
    c.read_exact(body).unwrap();
    status
}

fn request(method: &str, path: &str, body: &str) -> Vec<u8> {
    let mut r = format!(
        "{method} {path} HTTP/1.1\r\nHost: b\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    r.extend_from_slice(body.as_bytes());
    r
}

fn post(q: &str, params: &str) -> Vec<u8> {
    let body = format!("{{\"query\":\"{q}\",\"params\":[{params}]}}");
    request("POST", "/t/bench/query", &body)
}

fn by_id(i: usize) -> Vec<u8> {
    post("get items where id = $1", &(i % ROWS + 1).to_string())
}

/// A directory placing tenant `bench` on the node at `addr`, its rows
/// loaded through the router `bin` (this tree's, with `extra` flags).
fn directory(bin: &str, file: &Path, addr: &str, extra: &[&str]) {
    let mut args = vec![
        "--listen",
        "127.0.0.1:0",
        "--directory",
        file.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    let router = start(bin, &args);
    let mut c = dial(router.port);
    let mut body = Vec::new();
    let node = format!("{{\"addr\":\"{addr}\",\"token\":\"adm\"}}");
    let status = ask(
        &mut c,
        &request("PUT", "/_shard/nodes/n1", &node),
        &mut body,
    );
    assert!(status < 300, "{}", String::from_utf8_lossy(&body));
    let tenant = request("PUT", "/_shard/tenants/bench", r#"{"node":"n1"}"#);
    let status = ask(&mut c, &tenant, &mut body);
    assert!(status < 300, "{}", String::from_utf8_lossy(&body));
    let made = post("create collection items (n int)", "");
    assert_eq!(ask(&mut c, &made, &mut body), 200);
    for chunk in (0..ROWS).collect::<Vec<_>>().chunks(1000) {
        let docs: Vec<String> = chunk
            .iter()
            .map(|i| format!("{{id: {}, n: {i}}}", i + 1))
            .collect();
        let q = format!("put items {}", docs.join(", "));
        let status = ask(&mut c, &post(&q, ""), &mut body);
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    }
}

/// One client: each round trip timed, its p50 and p99 in microseconds.
fn one(port: u16) -> (f64, f64) {
    let mut c = dial(port);
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
fn many(port: u16) -> f64 {
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicU64::new(0));
    let threads: Vec<_> = (0..CLIENTS)
        .map(|k| {
            let (stop, done) = (Arc::clone(&stop), Arc::clone(&done));
            std::thread::spawn(move || {
                let mut c = dial(port);
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

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[derive(Default)]
struct Figures {
    p50: Vec<f64>,
    p99: Vec<f64>,
    rps: Vec<f64>,
}

struct Side {
    name: &'static str,
    bin: String,
    dir: PathBuf,
    extra: Vec<String>,
    figures: Figures,
}

/// A turn: the side's router started over its directory, one client's
/// round trips through it, then eight clients'.
fn turn(side: &mut Side) {
    let mut args = vec![
        "--listen".to_string(),
        "127.0.0.1:0".into(),
        "--directory".into(),
        side.dir.to_string_lossy().into_owned(),
    ];
    args.extend(side.extra.iter().cloned());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let router = start(&side.bin, &args);
    let at = load();
    let (p50, p99) = one(router.port);
    let rps = many(router.port);
    println!(
        "  {:<14} p50 {p50:>6.1} us  p99 {p99:>6.1} us  {rps:>8.0} req/s  (load {at:.2})",
        side.name
    );
    side.figures.p50.push(p50);
    side.figures.p99.push(p99);
    side.figures.rps.push(rps);
}

/// A leasing node in this process, plain or serving HTTPS: its address
/// as a router records it.
fn leasing(work: &Path, tag: &str, tls: bool) -> String {
    let dir = work.join(tag);
    let tenants = Tenants::new(&dir)
        .unwrap()
        .with_replication(Replicated {
            token: "repl".into(),
            buffer: 8 << 20,
            upstream: None,
            sync_on_write: true,
        })
        .with_lease();
    let cfg = fennec_http::Config {
        addr: "127.0.0.1:0".into(),
        admin_token: Some("adm".into()),
        sync_on_write: true,
        tls: tls.then(|| {
            let (pem, key) = (certs().join("server.pem"), certs().join("server.key"));
            Arc::new(fennec_http::conn::Tls::from_pem_files(&pem, &key).unwrap())
        }),
        ..fennec_http::Config::default()
    };
    let server = fennec_http::Server::with_tenants(Arc::new(tenants), cfg);
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    match tls {
        true => format!("https://localhost:{port}"),
        false => format!("127.0.0.1:{port}"),
    }
}

/// A router leasing three nodes, a tenant placed on each: the p50 and p99
/// of [`LEASE_ROUNDS`] rounds in microseconds.
fn lease_rounds(work: &Path, tag: &str, tls: bool) -> (f64, f64) {
    let router = Router::new(
        Directory::in_memory(),
        Config {
            addr: "127.0.0.1:0".into(),
            auto_failover: Some(Duration::from_secs(3)),
            ..Config::default()
        },
    );
    let listener = router.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    let serving = Arc::clone(&router);
    std::thread::spawn(move || {
        let _ = serving.serve_on(listener);
    });
    let mut c = dial(port);
    let mut body = Vec::new();
    for i in 1..=3 {
        let node = format!(
            "{{\"addr\":\"{}\",\"token\":\"adm\"}}",
            leasing(work, &format!("{tag}{i}"), tls)
        );
        let at = format!("/_shard/nodes/n{i}");
        assert!(ask(&mut c, &request("PUT", &at, &node), &mut body) < 300);
    }
    for i in 1..=3 {
        let at = format!("/_shard/tenants/{tag}t{i}");
        let t = request("PUT", &at, &format!("{{\"node\":\"n{i}\"}}"));
        let status = ask(&mut c, &t, &mut body);
        assert!(status < 300, "{}", String::from_utf8_lossy(&body));
    }
    router.lease_round();
    let mut took = Vec::with_capacity(LEASE_ROUNDS);
    for _ in 0..LEASE_ROUNDS {
        let t = Instant::now();
        router.lease_round();
        took.push(t.elapsed().as_nanos() as f64 / 1000.0);
    }
    took.sort_by(f64::total_cmp);
    (took[LEASE_ROUNDS / 2], took[LEASE_ROUNDS * 99 / 100])
}

/// A dial alone -- lookup, connect and, over TLS, the handshake -- to
/// `addr`, [`DIALS`] times: the p50 in microseconds.
fn dials(addr: &str) -> f64 {
    let mut took = Vec::with_capacity(DIALS);
    for _ in 0..DIALS {
        let t = Instant::now();
        let s = fennec_http::dial::connect(addr, Duration::from_secs(5)).unwrap();
        took.push(t.elapsed().as_nanos() as f64 / 1000.0);
        drop(s);
    }
    median(&mut took)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(shard), Some(server)) = (args.first().cloned(), args.get(1).cloned()) else {
        eprintln!(
            "usage: tls_dial <fennec-shard> <fennec-server> [<base fennec-shard>|- [rounds]]"
        );
        std::process::exit(2);
    };
    let base = args.get(2).filter(|b| b.as_str() != "-").cloned();
    let rounds: usize = args.get(3).map_or(5, |r| r.parse().unwrap());
    fennec_http::dial::trust_ca(&cert("ca.pem")).unwrap();

    let work = std::env::temp_dir().join(format!("fennec-tls-dial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let node_args = |dir: &str| {
        vec![
            "--dir".to_string(),
            work.join(dir).to_string_lossy().into_owned(),
            "--http".into(),
            "127.0.0.1:0".into(),
            "--admin-token".into(),
            "adm".into(),
            "--max-connections".into(),
            "0".into(),
        ]
    };
    let plain_args = node_args("plain-node");
    let plain = start(
        &server,
        &plain_args.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    let mut tls_args = node_args("tls-node");
    tls_args.extend([
        "--tls-cert".into(),
        cert("server.pem"),
        "--tls-key".into(),
        cert("server.key"),
    ]);
    let tls = start(
        &server,
        &tls_args.iter().map(String::as_str).collect::<Vec<_>>(),
    );

    let ca = cert("ca.pem");
    let (plain_dir, tls_dir) = (work.join("plain.fennec"), work.join("tls.fennec"));
    directory(
        &shard,
        &plain_dir,
        &format!("127.0.0.1:{}", plain.port),
        &[],
    );
    let tls_addr = format!("https://localhost:{}", tls.port);
    directory(&shard, &tls_dir, &tls_addr, &["--tls-ca", &ca]);

    let mut sides = Vec::new();
    if let Some(b) = &base {
        sides.push(Side {
            name: "base, http",
            bin: b.clone(),
            dir: plain_dir.clone(),
            extra: Vec::new(),
            figures: Figures::default(),
        });
    }
    sides.push(Side {
        name: "this, http",
        bin: shard.clone(),
        dir: plain_dir.clone(),
        extra: Vec::new(),
        figures: Figures::default(),
    });
    sides.push(Side {
        name: "this, https",
        bin: shard.clone(),
        dir: tls_dir.clone(),
        extra: vec!["--tls-ca".into(), ca.clone()],
        figures: Figures::default(),
    });

    println!(
        "a row by id over {ROWS} rows through the router: one client ({ONE} round trips), \
         {CLIENTS} at once for {RUN:?}; the router's dial to its node plain or over TLS"
    );
    let mut leases: [(Vec<f64>, Vec<f64>); 2] = Default::default();
    let mut dialed: [Vec<f64>; 2] = Default::default();
    for r in 0..rounds {
        let at = quiet();
        println!("round {} (load {at:.2})", r + 1);
        let n = sides.len();
        for k in 0..n {
            turn(&mut sides[(r + k) % n]);
        }
        for k in 0..2 {
            let tls = (r + k) % 2 == 1;
            let (p50, p99) = lease_rounds(&work, &format!("r{r}{k}"), tls);
            let i = tls as usize;
            leases[i].0.push(p50);
            leases[i].1.push(p99);
            let to = if tls {
                tls_addr.clone()
            } else {
                format!("127.0.0.1:{}", plain.port)
            };
            let d = dials(&to);
            dialed[i].push(d);
            println!(
                "  lease round {:<6} p50 {p50:>7.1} us  p99 {p99:>7.1} us  dial {d:>6.1} us",
                if tls { "https" } else { "http" }
            );
        }
    }
    println!("medians over {rounds} rounds:");
    for s in &mut sides {
        println!(
            "  {:<14} p50 {:>6.1} us  p99 {:>6.1} us  {:>8.0} req/s",
            s.name,
            median(&mut s.figures.p50),
            median(&mut s.figures.p99),
            median(&mut s.figures.rps)
        );
    }
    for (i, name) in ["http", "https"].iter().enumerate() {
        println!(
            "  lease round {name:<6} p50 {:>7.1} us  p99 {:>7.1} us  dial {:>6.1} us",
            median(&mut leases[i].0),
            median(&mut leases[i].1),
            median(&mut dialed[i])
        );
    }
    drop((plain, tls));
    let _ = std::fs::remove_dir_all(&work);
}

//! What one row costs over REST through `handle`, read and written by its
//! id, and what its tag costs: `make instr-compare EXAMPLE=rows
//! MODES=get,row,patch,patch-row,if-match` (`cargo run --release -p
//! fennec-http --example rows -- MODE [N] [BYTES]`), one mode a process.
//!
//!   * `get`: `GET /t?id=eq.<id>`, the read of a row by id a filter makes.
//!   * `row`: `GET /t/<id>`, the row and its `ETag`; on a build without
//!     the route, `get`'s request, so that a build from before reads a row
//!     by id as a client then did.
//!   * `patch`: `PATCH /t?id=eq.<id>` with `{"n": <i>}`, a plain row write.
//!   * `patch-row`: `PATCH /t/<id>`, the same write by the row's path; on a
//!     build without it, `patch`'s request.
//!   * `if-match`: `PATCH /t/<id>` under `If-Match`, the tag the row had,
//!     read from the answer before: the check and the new tag. On a build
//!     without the route, `patch`'s request.
//!   * `tag`: the tag of a row whose text is BYTES bytes alone, N times
//!     (`conditional::tag`); `sha`: its SHA-256, which a poll's tag takes.
//!
//! 500 rows of an int and a text of BYTES (40) bytes, written over in
//! turn, in a file as a server under `--sync <ms>` writes it. It prints ns
//! a request, the median of 9 rounds of N / 9, and on macOS the
//! instructions and cycles the loop retired (`proc_pid_rusage`, read
//! before and after it), as `replays.rs` does.

use fennec_core::prelude::Database;
use fennec_http::http::{Method, Request};
use fennec_http::Config;
use std::sync::{Arc, RwLock};
use std::time::Instant;

const ROUNDS: usize = 9;
const ROWS: usize = 500;

fn request(method: Method, target: &str, headers: &[(&str, &str)], body: &str) -> Request {
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    let query = (query.split('&'))
        .filter(|kv| !kv.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (kv.to_string(), String::new()),
        })
        .collect();
    Request {
        method,
        target: target.into(),
        path: path.into(),
        query,
        headers: (headers.iter())
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        body: body.as_bytes().to_vec(),
        keep_alive: true,
    }
}

/// The answer's status and its `ETag`.
fn send(db: &Arc<RwLock<Database>>, cfg: &Config, req: &Request) -> (u16, Option<String>) {
    // The thread's flags, as the connection resets them for each request.
    fennec_http::metrics::begin_request();
    let resp = fennec_http::handle(db, cfg, req);
    let tag = (resp.extra.iter())
        .find(|(k, _)| k.eq_ignore_ascii_case("etag"))
        .map(|(_, v)| v.clone());
    (resp.status, tag)
}

fn ok(db: &Arc<RwLock<Database>>, cfg: &Config, req: &Request) -> Option<String> {
    let (status, tag) = send(db, cfg, req);
    assert!(status < 300, "{} {}: {status}", req.method.name(), req.target);
    tag
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "get".into());
    let n: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(20_000);
    let bytes: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(40);
    let dir = std::env::temp_dir().join(format!("fennec-rows-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bench.fennec");
    let db = Arc::new(RwLock::new(fennec_core::fs::open(&path).unwrap()));
    let cfg = Config::default();
    let query = |sql: &str| {
        let body = format!(r#"{{"query":"{sql}"}}"#);
        ok(&db, &cfg, &request(Method::Post, "/query", &[], &body));
    };
    query("create collection t (n int, name text)");
    let name = "x".repeat(bytes);
    for id in 1..=ROWS {
        query(&format!("put t {{id: {id}, n: 0, name: '{name}'}}"));
    }
    // Whether this build answers a row by its path: one from before the
    // route reads `/t/1` as no path it takes.
    let routed = send(&db, &cfg, &request(Method::Get, "/t/1", &[], "")).0 == 200;
    let mut tags: Vec<Option<String>> = vec![None; ROWS + 1];
    if mode == "if-match" && routed {
        for (id, tag) in tags.iter_mut().enumerate().skip(1) {
            *tag = ok(&db, &cfg, &request(Method::Get, &format!("/t/{id}"), &[], ""));
        }
    }
    let row = format!(r#"{{"id":1,"n":12345,"name":"{name}"}}"#);
    let per = n / ROUNDS;
    let mut rounds = Vec::with_capacity(ROUNDS);
    let mut i = 0usize;
    let mut sink = 0u8;
    let before = retired();
    for _ in 0..ROUNDS {
        let t = Instant::now();
        for _ in 0..per {
            i += 1;
            let id = i % ROWS + 1;
            let set = || format!(r#"{{"n":{i}}}"#);
            match (mode.as_str(), routed) {
                ("get", _) | ("row", false) => {
                    ok(&db, &cfg, &request(Method::Get, &format!("/t?id=eq.{id}"), &[], ""));
                }
                ("row", true) => {
                    ok(&db, &cfg, &request(Method::Get, &format!("/t/{id}"), &[], ""));
                }
                ("patch", _) | ("patch-row" | "if-match", false) => {
                    let target = format!("/t?id=eq.{id}");
                    ok(&db, &cfg, &request(Method::Patch, &target, &[], &set()));
                }
                ("patch-row", true) => {
                    ok(&db, &cfg, &request(Method::Patch, &format!("/t/{id}"), &[], &set()));
                }
                ("if-match", true) => {
                    let tag = tags[id].take().expect("the row's tag");
                    let headers = [("If-Match", tag.as_str())];
                    let req = request(Method::Patch, &format!("/t/{id}"), &headers, &set());
                    tags[id] = ok(&db, &cfg, &req);
                }
                ("tag", _) => {
                    let t = fennec_http::conditional::tag(std::hint::black_box(row.as_bytes()));
                    sink ^= t.as_str().as_bytes()[1];
                }
                ("sha", _) => {
                    let d = fennec_http::crypto::sha256(std::hint::black_box(row.as_bytes()));
                    sink ^= d[0];
                }
                (other, _) => {
                    panic!("no mode {other}: get, row, patch, patch-row, if-match, tag or sha")
                }
            }
        }
        rounds.push(t.elapsed().as_nanos() as f64 / per as f64);
    }
    let after = retired();
    std::hint::black_box(sink);
    rounds.sort_by(|a, b| a.total_cmp(b));
    let how = match routed {
        true => "",
        false => " (no row route: by a filter)",
    };
    println!("mode {mode}{how}, {} requests", per * ROUNDS);
    println!("ns a request {:.0}", rounds[ROUNDS / 2]);
    if let (Some((i0, c0)), Some((i1, c1))) = (before, after) {
        let sent = (per * ROUNDS) as f64;
        println!("instructions in the loop {}", i1 - i0);
        println!("cycles in the loop {}", c1 - c0);
        println!(
            "a request: {:.1} instructions, {:.1} cycles",
            (i1 - i0) as f64 / sent,
            (c1 - c0) as f64 / sent
        );
    }
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The instructions and cycles this process retired so far, every thread's
/// (`proc_pid_rusage`, `RUSAGE_INFO_V4`): the loop runs on this one alone.
#[cfg(target_os = "macos")]
fn retired() -> Option<(u64, u64)> {
    extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u64) -> i32;
    }
    // `rusage_info_v4`: a 16-byte uuid, then 35 counters, `ri_instructions`
    // the 30th and `ri_cycles` the 31st.
    let mut info = [0u64; 37];
    let r = unsafe { proc_pid_rusage(std::process::id() as i32, 4, info.as_mut_ptr()) };
    (r == 0).then_some((info[31], info[32]))
}

#[cfg(not(target_os = "macos"))]
fn retired() -> Option<(u64, u64)> {
    None
}

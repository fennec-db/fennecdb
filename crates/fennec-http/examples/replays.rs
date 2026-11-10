//! What an `Idempotency-Key` costs a write through `handle`, and what a
//! replay's wait for the durability of its key's block costs: `make
//! replay-bench` (`cargo run --release -p fennec-http --example replays --
//! MODE [N]`), one mode a process, so that `/usr/bin/time -l` counts the
//! instructions of that mode alone beside the same setup.
//!
//!   * `put`: N writes by id through `/query` (`put t {id: $1, n: $2}`,
//!     500 ids written over in turn), no key, into a file as a server
//!     under `--sync <ms>` writes it (`flush_for`: the write behind the
//!     lock).
//!   * `keyed`: the same, each with a key of its own: the key's lookup,
//!     the block and the `_idempotency` row.
//!   * `batch`: N writes of 100 documents each (`put t [{id: 1, n: $1},
//!     ...]`, the same 100 ids written over), no key: a record of 100
//!     frames, where `put` writes one of one.
//!   * `replay`: one keyed write under `--sync always`, then that request
//!     N times, each a replay. Its block is on disk by then, so the
//!     durability a replay waits for (`Database::durability`) runs no
//!     fsync: what is measured is asking for it and finding it covered.
//!
//! It prints ns a request, the median of 9 rounds of N / 9, and on macOS
//! the instructions and cycles the loop retired (`proc_pid_rusage`, read
//! before and after it): between two builds those judge a cost, the
//! process's start, its setup and its end left out -- whole-process counts
//! (`/usr/bin/time -l`) moved by as much as 6 million over 20 000 requests
//! from one run to the next of one build. The nanoseconds are the
//! clock's, beside them.

use fennec_core::prelude::Database;
use fennec_http::http::{Method, Request};
use fennec_http::Config;
use std::sync::{Arc, RwLock};
use std::time::Instant;

const ROUNDS: usize = 9;

fn request(sql: &str, params: &str, key: Option<&str>) -> Request {
    Request {
        method: Method::Post,
        target: "/query".into(),
        path: "/query".into(),
        query: Vec::new(),
        headers: key
            .map(|k| ("Idempotency-Key".to_string(), k.to_string()))
            .into_iter()
            .collect(),
        body: format!(r#"{{"query":"{sql}","params":[{params}]}}"#).into_bytes(),
        keep_alive: true,
    }
}

fn send(db: &Arc<RwLock<Database>>, cfg: &Config, req: &Request) {
    // The thread's flags, as the connection resets them for each request.
    fennec_http::metrics::begin_request();
    let resp = fennec_http::handle(db, cfg, req);
    assert!(
        resp.status < 300,
        "{}: {}",
        resp.status,
        String::from_utf8_lossy(&resp.body)
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "put".into());
    let n: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(20_000);
    let dir = std::env::temp_dir().join(format!("fennec-replays-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bench.fennec");
    let db = Arc::new(RwLock::new(fennec_core::fs::open(&path).unwrap()));
    let cfg = Config {
        sync_on_write: mode == "replay",
        ..Config::default()
    };
    send(&db, &cfg, &request("create collection t (n int)", "", None));
    let put = "put t {id: $1, n: $2}";
    let replayed = request(put, "1, 1", Some("replayed"));
    let batch = format!(
        "put t [{}]",
        (1..=100)
            .map(|id| format!("{{id: {id}, n: $1}}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if mode == "replay" {
        send(&db, &cfg, &replayed);
    }
    let per = n / ROUNDS;
    let mut rounds = Vec::with_capacity(ROUNDS);
    let mut i = 0usize;
    let before = retired();
    for _ in 0..ROUNDS {
        let t = Instant::now();
        for _ in 0..per {
            i += 1;
            let params = || format!("{}, {i}", i % 500 + 1);
            match mode.as_str() {
                "put" => send(&db, &cfg, &request(put, &params(), None)),
                "keyed" => {
                    let key = format!("k-{i}");
                    send(&db, &cfg, &request(put, &params(), Some(&key)))
                }
                "replay" => send(&db, &cfg, &replayed),
                "batch" => send(&db, &cfg, &request(&batch, &i.to_string(), None)),
                other => panic!("no mode {other}: put, keyed, batch or replay"),
            }
        }
        rounds.push(t.elapsed().as_nanos() as f64 / per as f64);
    }
    let after = retired();
    rounds.sort_by(|a, b| a.total_cmp(b));
    println!("mode {mode}, {} requests", per * ROUNDS);
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

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
//!   * `replay`: one keyed write under `--sync always`, then that request
//!     N times, each a replay. Its block is on disk by then, so the
//!     durability a replay waits for (`Database::durability`) runs no
//!     fsync: what is measured is asking for it and finding it covered.
//!
//! It prints ns a request, the median of 9 rounds of N / 9. Between two
//! builds, the instructions retired of the whole process judge a cost; the
//! nanoseconds are the clock's, beside them.

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
    if mode == "replay" {
        send(&db, &cfg, &replayed);
    }
    let per = n / ROUNDS;
    let mut rounds = Vec::with_capacity(ROUNDS);
    let mut i = 0usize;
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
                other => panic!("no mode {other}: put, keyed or replay"),
            }
        }
        rounds.push(t.elapsed().as_nanos() as f64 / per as f64);
    }
    rounds.sort_by(|a, b| a.total_cmp(b));
    println!("mode {mode}, {} requests", per * ROUNDS);
    println!("ns a request {:.0}", rounds[ROUNDS / 2]);
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

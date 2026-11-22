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
//!   * `scoped`, `scoped-keyed`: a `set` by id (`set t {n: $2} where id =
//!     $1`, the 500 rows put first by the server's token) by a JSON Web
//!     Token (HS256, `t read,write`), verified once and found in the
//!     server's table after, without a key and with one of its own: what a
//!     key costs a scoped caller -- its key kept under its caller
//!     (`Scope::authority`), where the server's token keeps it under none.
//!     A scoped token puts no document by id. A third argument is the
//!     token's `sub`, `bench` unless given.
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
    scoped(sql, params, key, None)
}

/// [`request`], carrying `bearer` where it is given.
fn scoped(sql: &str, params: &str, key: Option<&str>, bearer: Option<&str>) -> Request {
    Request {
        method: Method::Post,
        target: "/query".into(),
        path: "/query".into(),
        query: Vec::new(),
        headers: key
            .map(|k| ("Idempotency-Key".to_string(), k.to_string()))
            .into_iter()
            .chain(bearer.map(|b| ("Authorization".to_string(), format!("Bearer {b}"))))
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
    // The scoped modes' `sub`, `bench` unless given: a key is kept under
    // its caller, whose length is the subject's and a separator before G14
    // and 17 bytes after (a byte saying whose, 16 of the caller's digest),
    // so a short `sub` reads the longer key's bytes as a cost -- an
    // identity provider's is 28 to 36 characters (`auth0|` and 24 hex
    // digits, a UUID).
    let sub = args.next().unwrap_or_else(|| "bench".into());
    let dir = std::env::temp_dir().join(format!("fennec-replays-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bench.fennec");
    let db = Arc::new(RwLock::new(fennec_core::fs::open(&path).unwrap()));
    let access = mode.starts_with("scoped").then(|| {
        let secret = b"a secret of more than thirty-two bytes, as HS256 wants";
        Arc::new(fennec_http::access::Access::new(secret, "t read,write\n").unwrap())
    });
    let jwt = access
        .as_ref()
        .map(|a| a.mint(&format!(r#"{{"sub":"{sub}"}}"#)).unwrap());
    let cfg = Config {
        sync_on_write: mode == "replay",
        // The collection is made by the server's own token, the writes by
        // the JWT.
        token: access.as_ref().map(|_| "bench-token".to_string()),
        access,
        // One caller writing as fast as it can: held to a thousand a second,
        // it would be refused.
        request_rate: 0,
        ..Config::default()
    };
    let root = cfg.token.as_deref();
    send(
        &db,
        &cfg,
        &scoped("create collection t (n int)", "", None, root),
    );
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
    let set = "set t {n: $2} where id = $1";
    if jwt.is_some() {
        let rows: Vec<String> = (1..=500).map(|id| format!("{{id: {id}, n: 0}}")).collect();
        let all = format!("put t [{}]", rows.join(", "));
        send(&db, &cfg, &scoped(&all, "", None, root));
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
                "scoped" => send(&db, &cfg, &scoped(set, &params(), None, jwt.as_deref())),
                "scoped-keyed" => {
                    let key = format!("k-{i}");
                    send(
                        &db,
                        &cfg,
                        &scoped(set, &params(), Some(&key), jwt.as_deref()),
                    )
                }
                other => {
                    panic!("no mode {other}: put, keyed, batch, replay, scoped or scoped-keyed")
                }
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
    // SAFETY: `info` is the 37 words of `rusage_info_v4` (above), all that
    // flavor 4 writes; the call keeps no pointer to it.
    let r = unsafe { proc_pid_rusage(std::process::id() as i32, 4, info.as_mut_ptr()) };
    (r == 0).then_some((info[31], info[32]))
}

#[cfg(not(target_os = "macos"))]
fn retired() -> Option<(u64, u64)> {
    None
}

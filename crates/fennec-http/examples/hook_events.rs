//! What a webhook event costs through `handle`, against the writes it is
//! compared with (`docs/design/named-statements.md`, *What a hook costs*):
//! `make hook-bench MODE=<mode> [N=<n>]` (`cargo run --release -p
//! fennec-http --example hook_events -- MODE [N]`), one mode a process, so
//! that each count is that mode's alone beside the same setup; `make
//! hook-bench` alone runs every mode in turns, and `replays.rs` against
//! main, each run once the load is low, the medians to a results file
//! (`hook_bench.py`).
//!
//!   * `event`: N Stripe events, each a new `id`, to `POST /_hooks/stripe`,
//!     each verified, taken -- a `set` of an order by its `@hash` key and
//!     the key's row in `_hook_events`, one block -- and fsynced before its
//!     answer, as every event is whatever `--sync` says.
//!   * `keyed`: the same `set` through `/query` under an `Idempotency-Key`
//!     and `--sync always`: the same block's shape (a write and a key's
//!     row), the same fsync, no signature.
//!   * `put`: a plain `set` through `/query` under `--sync always`: no key,
//!     no signature.
//!   * `buffered`: that `set` under `--sync 250`, what a server's writes
//!     are when no hook asks for the disk.
//!   * `duplicate`: one event taken, then sent N times: each found under
//!     the read lock and answered as the first, its block on disk already.
//!   * `forged`: N events signed under another secret: the one 401, both
//!     HMACs computed (the audit's wait set to none).
//!   * `verify4k`, `verify64k`: `Hooks::verify` alone over an event of 4
//!     and 64 KB, in process.
//!   * `beside0`, `beside10`, `beside100`: what an event's fsync costs the
//!     buffered writers beside it (*An fsync an event costs the buffered
//!     writers beside it*): `buffered`'s `set`s on this thread while
//!     another sends 0, 10 or 100 events a second, each fsynced; it prints
//!     the buffered writes' median and p99 besides, each timed alone. N is
//!     a million unless given: some seconds, so that events meet them.
//!
//! `forged` writes the line a refused event writes to stderr, as a server
//! does: send stderr to `/dev/null`.
//!
//! It prints ns a request and requests a second, the median of 9 rounds of
//! N / 9, and on macOS the instructions and cycles the loop retired
//! (`proc_pid_rusage`). The modes that fsync measure the disk as much as
//! the code: run them on an idle machine, each in turns with the one it is
//! compared with, and never under someone else's load (`uptime`). Not run
//! in CI.

use fennec_core::prelude::Database;
use fennec_http::crypto::hmac_sha256;
use fennec_http::hooks::Hooks;
use fennec_http::http::{Method, Request};
use fennec_http::Config;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ROUNDS: usize = 9;
const SECRET: &str = "whsec_hook_events_bench";
/// The orders the events pay, one a set over the run.
const ORDERS: usize = 500;

fn post(path: &str, headers: Vec<(String, String)>, body: String) -> Request {
    Request {
        method: Method::Post,
        target: path.into(),
        path: path.into(),
        query: Vec::new(),
        headers,
        body: body.into_bytes(),
        keep_alive: true,
    }
}

fn query(sql: &str, params: &str, key: Option<&str>) -> Request {
    post(
        "/query",
        key.map(|k| ("Idempotency-Key".to_string(), k.to_string()))
            .into_iter()
            .collect(),
        format!(r#"{{"query":"{sql}","params":[{params}]}}"#),
    )
}

/// Stripe's `payment_intent.succeeded` `id` for order `order`, padded to
/// `size` bytes in its metadata, signed now under `secret`.
fn event(id: &str, order: usize, size: usize, secret: &str) -> Request {
    let mut body = format!(
        r#"{{"id":"{id}","object":"event","type":"payment_intent.succeeded","livemode":true,"data":{{"object":{{"id":"pi_{id}","amount_received":2000,"metadata":{{"order":"o-{order}","note":""#
    );
    let tail = r#""}}}}"#;
    while body.len() + tail.len() < size {
        body.push('x');
    }
    body.push_str(tail);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mac = hmac_sha256(secret.as_bytes(), format!("{t}.{body}").as_bytes());
    let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
    post(
        "/_hooks/stripe",
        vec![
            ("Content-Type".into(), "application/json".into()),
            ("Stripe-Signature".into(), format!("t={t},v1={hex}")),
        ],
        body,
    )
}

fn send(db: &Arc<RwLock<Database>>, cfg: &Config, req: &Request, want: u16) {
    // The thread's flags, as the connection resets them for each request.
    fennec_http::metrics::begin_request();
    let resp = fennec_http::handle(db, cfg, req);
    assert_eq!(resp.status, want, "{}", String::from_utf8_lossy(&resp.body));
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "event".into());
    // Beside events, the writes run for seconds, so that tens and hundreds
    // of events meet them.
    let n: usize =
        args.next()
            .and_then(|a| a.parse().ok())
            .unwrap_or(if mode.starts_with("beside") {
                1_000_000
            } else {
                9_000
            });
    fennec_http::audit::set_delay(0);
    // The reader a server installs, which a hook's stored statement is
    // read by.
    fennec_ql::install();
    let dir = std::env::temp_dir().join(format!("fennec-hook-events-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bench.fennec");
    let mut db = fennec_core::fs::open(&path).unwrap();
    db.allow_hooks();
    let db = Arc::new(RwLock::new(db));
    let read = |_: &str| Ok(SECRET.as_bytes().to_vec());
    let hooks = Arc::new(Hooks::read("stripe stripe s max-body 128k\n", read, 64 << 20).unwrap());
    let beside: Option<u64> = mode.strip_prefix("beside").and_then(|r| r.parse().ok());
    let cfg = Config {
        sync_on_write: mode != "buffered" && beside.is_none(),
        hooks: Some(Arc::clone(&hooks)),
        ..Config::default()
    };
    let setup = |q: &str| send(&db, &cfg, &query(q, "", None), 200);
    setup("create collection orders (ref text @hash, status text, intent text)");
    for o in 0..ORDERS {
        setup(&format!("put orders {{ref: 'o-{o}', status: 'open'}}"));
    }
    setup(
        "create statement paid($intent text from event.data.object.id, $order text from \
         event.data.object.metadata.order) on hook stripe where event.type = \
         'payment_intent.succeeded' as set orders {status: 'paid', intent: $intent} where \
         ref = $order require 1",
    );
    let set = "set orders {status: 'paid', intent: $1} where ref = $2 require 1";
    let first = event("evt_first", 0, 0, SECRET);
    if mode == "duplicate" {
        send(&db, &cfg, &first, 200);
    }
    let size = match mode.as_str() {
        "verify64k" => 64 << 10,
        "verify4k" => 4 << 10,
        _ => 0,
    };
    let verified = event("evt_v", 1, size, SECRET);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    // The events sent beside the buffered writes, at their rate, from a
    // thread of their own as a sender's connection would be.
    let stop = Arc::new(AtomicBool::new(false));
    let taken = Arc::new(AtomicUsize::new(0));
    let sender = beside.filter(|r| *r > 0).map(|rate| {
        let (db, cfg) = (Arc::clone(&db), cfg.clone());
        let (stop, taken) = (Arc::clone(&stop), Arc::clone(&taken));
        std::thread::spawn(move || {
            let every = Duration::from_nanos(1_000_000_000 / rate);
            let mut next = Instant::now();
            let mut k = 0usize;
            while !stop.load(Ordering::Relaxed) {
                k += 1;
                send(
                    &db,
                    &cfg,
                    &event(&format!("evt_b{k}"), k % ORDERS, 0, SECRET),
                    200,
                );
                taken.fetch_add(1, Ordering::Relaxed);
                next += every;
                if let Some(wait) = next.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
            }
        })
    });

    let per = n / ROUNDS;
    let mut rounds = Vec::with_capacity(ROUNDS);
    let mut each: Vec<u64> = Vec::new();
    let mut i = 0usize;
    let before = retired();
    for _ in 0..ROUNDS {
        // Each request is made before the clock starts where making it is
        // not what is measured: a signature, a key, a body.
        let reqs: Vec<Request> = (0..per)
            .map(|_| {
                i += 1;
                let order = i % ORDERS;
                let params = format!("\"pi_{i}\", \"o-{order}\"");
                match mode.as_str() {
                    "event" => event(&format!("evt_{i}"), order, 0, SECRET),
                    "keyed" => query(set, &params, Some(&format!("k-{i}"))),
                    "put" | "buffered" => query(set, &params, None),
                    _ if beside.is_some() => query(set, &params, None),
                    "forged" => event(&format!("evt_{i}"), order, 0, "whsec_not_it"),
                    "duplicate" | "verify4k" | "verify64k" => post("/", Vec::new(), String::new()),
                    other => panic!(
                        "no mode {other}: event, keyed, put, buffered, duplicate, forged, \
                         verify4k, verify64k, beside0, beside10 or beside100"
                    ),
                }
            })
            .collect();
        let t = Instant::now();
        for req in &reqs {
            match mode.as_str() {
                "forged" => send(&db, &cfg, req, 401),
                "duplicate" => send(&db, &cfg, &first, 200),
                "verify4k" | "verify64k" => {
                    assert!(hooks.verify("stripe", &verified, now_ms).is_ok())
                }
                _ if beside.is_some() => {
                    let one = Instant::now();
                    send(&db, &cfg, req, 200);
                    each.push(one.elapsed().as_nanos() as u64);
                }
                _ => send(&db, &cfg, req, 200),
            }
        }
        rounds.push(t.elapsed().as_nanos() as f64 / per as f64);
    }
    let after = retired();
    stop.store(true, Ordering::Relaxed);
    if let Some(sender) = sender {
        sender.join().unwrap();
    }
    rounds.sort_by(|a, b| a.total_cmp(b));
    let ns = rounds[ROUNDS / 2];
    println!("mode {mode}, {} requests", per * ROUNDS);
    println!("ns a request {ns:.0}");
    println!("a second {:.0}", 1e9 / ns);
    if !each.is_empty() {
        each.sort_unstable();
        let at = |q: f64| each[((each.len() - 1) as f64 * q) as usize];
        println!(
            "buffered writes: median {} ns, p99 {} ns, max {} ns; {} events beside them",
            at(0.5),
            at(0.99),
            at(1.0),
            taken.load(Ordering::Relaxed)
        );
    }
    if let (Some((i0, c0)), Some((i1, c1))) = (before, after) {
        let sent = (per * ROUNDS) as f64;
        println!("instructions in the loop {}", i1 - i0);
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
/// (`proc_pid_rusage`, `RUSAGE_INFO_V4`), as `replays.rs` reads them.
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

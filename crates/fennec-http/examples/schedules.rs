//! What a tenant node's scheduler takes at the hour: `make schedule-bench`
//! (`cargo run --release -p fennec-http --example schedules -- [TENANTS]
//! [empty|writes|durable]`).
//!
//! TENANTS tenant files (10 000), each holding a statement due at one slot,
//! a node's scheduler over them (`schedule::Node`), and every slot run by
//! one look at the slot's instant (`Node::pass`), in process, the tenants
//! closed before it as a node's idle ones are:
//!
//!   * `empty`: a purge finding nothing, as most runs of a nightly purge
//!     are: a run landing no block, its mark pending, kept by the node as
//!     the tenant closes -- the bytes the files grew by, 0;
//!   * `writes`: an insert, a block a run written behind the lock, as a
//!     node under an interval `--sync` writes it;
//!   * `durable`: the insert under `--sync always`, fsynced before the next.
//!
//! For each: how late a run began behind its slot -- its start less the
//! look's, every slot due as the look begins -- the median, the p99 and
//! the last's; the tenants opened a second; and how long the node took to
//! learn which tenants hold one, opening each once, as a start with no
//! hint does. Past a minute late, the design's pool of schedulers is owed
//! (`docs/design/named-statements.md`, *Where a run happens, and once*).

use fennec_http::schedule::Node;
use fennec_http::tenants::Tenants;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

const HOUR: i64 = 3_600_000;

fn quantile(v: &[Duration], q: f64) -> Duration {
    v[((v.len() - 1) as f64 * q).round() as usize]
}

fn bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "fennec"))
        .map(|e| e.metadata().map_or(0, |m| m.len()))
        .sum()
}

fn measure(n: usize, mode: &str) {
    fennec_ql::install();
    let dir = std::env::temp_dir().join(format!("fennec-schedules-{mode}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let now = fennec_core::time::now_ms().unwrap();
    let slot = now - now.rem_euclid(HOUR) + HOUR;
    let body = match mode {
        "empty" => "create statement tidy every 1h scans as del ticks where n = -1",
        _ => "create statement tick every 1h as insert ticks {at: slot(), n: 1}",
    };
    {
        let mut db = fennec_core::fs::open(dir.join("t00000.fennec")).unwrap();
        db.allow_schedules();
        db.set_clock(Some(slot - HOUR / 2));
        for q in ["create collection ticks (at timestamp, n int)", body] {
            db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
        }
        // Each tenant holds some rows, as a live one does: its open reads
        // them.
        for i in 0..100 {
            let q = format!("put ticks {{at: {}, n: {i}}}", slot - HOUR);
            db.execute(&fennec_ql::parse_one(&q).unwrap()).unwrap();
        }
        if let Some(sync) = db.flush().unwrap() {
            sync().unwrap();
        }
    }
    for i in 1..n {
        std::fs::copy(
            dir.join("t00000.fennec"),
            dir.join(format!("t{i:05}.fennec")),
        )
        .unwrap();
    }
    let tenants = Arc::new(Tenants::new(&dir).unwrap());
    let node = Node::attach(&tenants, mode == "durable");
    let t = Instant::now();
    node.learn();
    let learned = t.elapsed();
    assert_eq!(node.named().len(), n);
    let before = bytes(&dir);
    let opens = node.opens();
    let t = Instant::now();
    let runs = node.pass(slot + 1);
    let took = t.elapsed();
    assert_eq!(runs.len(), n, "every tenant's slot ran");
    let mut late: Vec<Duration> = runs.iter().map(|r| r.1.started - t).collect();
    late.sort();
    let opened = node.opens() - opens;
    println!(
        "{mode:>8}: {n} tenants due at once, run in {:.2} s: late by {:.1} ms median, {:.1} ms \
         p99, {:.2} s the last; {:.0} tenants opened a second; the files grew {} bytes; \
         learned in {:.2} s",
        took.as_secs_f64(),
        quantile(&late, 0.5).as_secs_f64() * 1e3,
        quantile(&late, 0.99).as_secs_f64() * 1e3,
        late.last().unwrap().as_secs_f64(),
        opened as f64 / took.as_secs_f64(),
        bytes(&dir) - before,
        learned.as_secs_f64(),
    );
    drop(runs);
    drop(node);
    drop(tenants);
    let _ = std::fs::remove_dir_all(&dir);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let n: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(10_000);
    match args.next() {
        Some(mode) => measure(n, &mode),
        None => {
            for mode in ["empty", "writes", "durable"] {
                measure(n, mode);
            }
        }
    }
}

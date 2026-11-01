//! A list of ids asked for, as a cart's or a page's selection is: `in $1`,
//! one parameter holding the list, against the same list written out with
//! a parameter a value, `in [$1, $2, ...]`, as every client had to write
//! it before: `make lists-bench`.
//!
//! In process, each statement parsed once and run over `ROWS` rows -- the
//! engine's part, the list put in the filter's place against each
//! parameter read where it stands -- and then over fennec-server's HTTP
//! (`POST /query`, kept alive), the text parsed or found in the parse cache
//! and the parameters read from JSON: one client at a time (the round
//! trip's p50) and `CLIENTS` at once (requests a second). Each request asks
//! for `n` ids drawn at random, by `id` (the id index) and by a `@hash`
//! field, the two ways in turns, twice, so that neither has the machine to
//! itself.

#[path = "../http.rs"]
mod http;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fennec_core::prelude::*;

const ROWS: u64 = 100_000;
const SIZES: [usize; 2] = [10, 1_000];
const CLIENTS: usize = 8;
const RUN: Duration = Duration::from_secs(3);

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `n` ids of rows, each `* step` (the `@hash` field holds `id * 7`).
    fn list(&mut self, n: usize, step: u64) -> Vec<i64> {
        (0..n)
            .map(|_| ((self.next() % ROWS + 1) * step) as i64)
            .collect()
    }
}

/// The text asking for `n` values of `field`: one parameter, or `n`.
fn text(field: &str, n: usize, one: bool) -> String {
    let list = match one {
        true => "$1".to_string(),
        false => {
            let p: Vec<String> = (1..=n).map(|i| format!("${i}")).collect();
            format!("[{}]", p.join(", "))
        }
    };
    format!("get products select id where {field} in {list}")
}

/// The parameters of [`text`], a JSON array's insides.
fn params(list: &[i64], one: bool) -> String {
    let items: Vec<String> = list.iter().map(i64::to_string).collect();
    match one {
        true => format!("[{}]", items.join(",")),
        false => items.join(","),
    }
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p) as usize]
}

fn step(field: &str) -> u64 {
    if field == "id" {
        1
    } else {
        7
    }
}

/// Median us of a query in process, the statement parsed once.
fn in_process(db: &Database, field: &str, n: usize, one: bool) -> f64 {
    let stmt = fennec_ql::parse_one(&text(field, n, one)).unwrap();
    let mut rng = Rng(9);
    let reps = if n > 100 { 2_000 } else { 20_000 };
    let mut lat = Vec::with_capacity(reps);
    for i in 0..reps + reps / 10 {
        let list = rng.list(n, step(field));
        let values: Vec<Value> = match one {
            true => vec![Value::List(list.iter().map(|&v| Value::Int(v)).collect())],
            false => list.iter().map(|&v| Value::Int(v)).collect(),
        };
        let t = Instant::now();
        let r = db.query(&stmt, &values).unwrap();
        let took = t.elapsed().as_secs_f64() * 1e6;
        assert!(r.rows().is_some_and(|rs| !rs.rows.is_empty()));
        if i >= reps / 10 {
            lat.push(took);
        }
    }
    pct(&mut lat, 0.5)
}

fn ask(c: &mut http::Http, field: &str, n: usize, one: bool, rng: &mut Rng, text: &str) {
    let list = rng.list(n, step(field));
    let ids = http::ids(c.query(text, &params(&list, one)));
    assert!(!ids.is_empty() && ids.len() <= n);
}

/// p50 and p99 in ms, one request at a time.
fn one_at_a_time(addr: &str, field: &str, n: usize, one: bool) -> (f64, f64) {
    let mut c = http::Http::connect(addr);
    let mut rng = Rng(42);
    let t = text(field, n, one);
    let reps = if n > 100 { 1_000 } else { 5_000 };
    for _ in 0..reps / 10 {
        ask(&mut c, field, n, one, &mut rng, &t);
    }
    let mut lat = Vec::with_capacity(reps);
    for _ in 0..reps {
        let start = Instant::now();
        ask(&mut c, field, n, one, &mut rng, &t);
        lat.push(start.elapsed().as_secs_f64() * 1e3);
    }
    (pct(&mut lat, 0.5), pct(&mut lat, 0.99))
}

/// Requests a second from `CLIENTS` clients at once for `RUN`.
fn at_once(addr: &str, field: &str, n: usize, one: bool) -> f64 {
    let done = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        for c in 0..CLIENTS as u64 {
            let (done, stop) = (&done, &stop);
            s.spawn(move || {
                let mut conn = http::Http::connect(addr);
                let mut rng = Rng(1000 + c);
                let t = text(field, n, one);
                let mut k = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    ask(&mut conn, field, n, one, &mut rng, &t);
                    k += 1;
                }
                done.fetch_add(k, Ordering::Relaxed);
            });
        }
        std::thread::sleep(RUN);
        stop.store(true, Ordering::Relaxed);
    });
    done.load(Ordering::Relaxed) as f64 / RUN.as_secs_f64()
}

fn fill(db: &mut Database) {
    let run = |db: &mut Database, sql: &str, params: &[Value]| {
        db.execute_with(&fennec_ql::parse_one(sql).unwrap(), params)
            .unwrap();
    };
    run(
        db,
        "create collection products (sku int @hash, name text)",
        &[],
    );
    db.begin().unwrap();
    for i in 1..=ROWS as i64 {
        run(
            db,
            "put products {id: $1, sku: $2, name: $3}",
            &[
                Value::Int(i),
                Value::Int(i * 7),
                Value::Text(format!("p{i}")),
            ],
        );
    }
    db.commit().unwrap();
}

fn main() {
    let ways = [(true, "in $1"), (false, "written out")];
    let fields = ["id", "sku"];
    println!("{ROWS} rows; in process: median us a query, parsed once");
    let mut db = Database::new();
    fill(&mut db);
    for round in 0..2 {
        for field in fields {
            for n in SIZES {
                for (one, name) in ways {
                    let us = in_process(&db, field, n, one);
                    println!("round {round} {field:<3} {n:>5} ids  {name:<11} {us:>9.2} us");
                }
            }
        }
    }
    drop(db);

    let dir = std::env::temp_dir().join(format!("fennecbench-lists-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("lists.fennec");
    let _ = std::fs::remove_file(&file);
    let mut db = fennec_core::fs::open(&file).unwrap();
    fill(&mut db);
    db.checkpoint().unwrap();
    drop(db);
    let port = http::free_port();
    let _server = http::start_fennec(&file, port, "lists-bench", &[]);
    let addr = format!("127.0.0.1:{port}");
    println!(
        "over HTTP: one client, round trip p50 / p99 in ms; {CLIENTS} clients, requests a second"
    );
    for round in 0..2 {
        for field in fields {
            for n in SIZES {
                for (one, name) in ways {
                    let (p50, p99) = one_at_a_time(&addr, field, n, one);
                    let rate = at_once(&addr, field, n, one);
                    println!(
                        "round {round} {field:<3} {n:>5} ids  {name:<11} {p50:.3} / {p99:.3} ms  \
                         {rate:>8.0}/s"
                    );
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

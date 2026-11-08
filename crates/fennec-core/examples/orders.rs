//! What comparing values costs where it is hottest: `cargo run --release
//! -p fennec-core --example orders -- MODE [N]`, one mode a process, or two
//! builds held to each other by `make instr-compare PKG=fennec-core
//! EXAMPLE=orders MODES=order-int,order-float,scan-float,group-float`.
//!
//! 100 000 rows of a random int `n` and a random float `f`, neither
//! indexed, each request one statement over all of them:
//!
//!   * `order-int`: `get t select n order n`, every row sorted by its key
//!     compared as two ints (`Value::cmp_value`);
//!   * `order-float`: `get t select f order f`, as two floats;
//!   * `scan-float`: `get t where f > $1 count`, a float field's filter
//!     tested on every row against a float, no row passing;
//!   * `group-float`: `get t select f, count(*) as c group f limit 1`,
//!     every float filed by its key (`codec::encode_key`).
//!
//! It prints ns a request, the median of 9 rounds of N / 9 (N 18 unless
//! given), and on macOS the instructions and cycles the loop retired
//! (`proc_pid_rusage`, read before and after it), the process's start and
//! the rows' writing left out.

use fennec_core::prelude::*;
use std::time::Instant;

const ROUNDS: usize = 9;
const ROWS: usize = 100_000;

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "order-float".into());
    let n: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(18);
    let mut db = Database::new();
    db.execute(&fennec_ql::parse_one("create collection t (n int, f float)").unwrap())
        .unwrap();
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        // xorshift64*: the same values every run and every build.
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        seed.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    let mut docs = Vec::with_capacity(ROWS);
    for _ in 0..ROWS {
        let n = (next() >> 1) as i64 - (1 << 62);
        let f = (next() >> 11) as f64 / (1u64 << 53) as f64 * 2e6 - 1e6;
        // An object's members ascending by key.
        docs.push(Value::Object(vec![
            ("f".to_string(), Value::Float(f)),
            ("n".to_string(), Value::Int(n)),
        ]));
    }
    let put = fennec_ql::parse_one("put t $1").unwrap();
    db.execute_with(&put, &[Value::List(docs)]).unwrap();
    let (text, params): (&str, Vec<Value>) = match mode.as_str() {
        "order-int" => ("get t select n order n", vec![]),
        "order-float" => ("get t select f order f", vec![]),
        "scan-float" => ("get t where f > $1 count", vec![Value::Float(2e6)]),
        "group-float" => ("get t select f, count(*) as c group f limit 1", vec![]),
        other => panic!("no mode {other}: order-int, order-float, scan-float or group-float"),
    };
    let stmt = fennec_ql::parse_one(text).unwrap();
    // The first answer builds what a read builds once.
    db.query(&stmt, &params).unwrap();
    let per = n.div_ceil(ROUNDS);
    let mut rounds = Vec::with_capacity(ROUNDS);
    let before = retired();
    for _ in 0..ROUNDS {
        let t = Instant::now();
        for _ in 0..per {
            std::hint::black_box(db.query(&stmt, &params).unwrap());
        }
        rounds.push(t.elapsed().as_nanos() as f64 / per as f64);
    }
    let after = retired();
    rounds.sort_by(|a, b| a.total_cmp(b));
    println!("mode {mode}, {} requests over {ROWS} rows", per * ROUNDS);
    println!("ns a request {:.0}", rounds[ROUNDS / 2]);
    if let (Some((i0, c0)), Some((i1, c1))) = (before, after) {
        let sent = (per * ROUNDS) as f64;
        println!(
            "a request: {:.1} instructions, {:.1} cycles",
            (i1 - i0) as f64 / sent,
            (c1 - c0) as f64 / sent
        );
    }
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

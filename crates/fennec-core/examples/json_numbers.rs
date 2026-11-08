//! What writing a page of numbers as JSON costs: `cargo run --release -p
//! fennec-core --example json_numbers -- MODE [N]`, one mode a process, or
//! two builds held to each other by `make instr-compare PKG=fennec-core
//! EXAMPLE=json_numbers MODES=e15,wide,frac,small,ints`.
//!
//! Each request is `Database::query_json` of `get t`: 1 000 rows, the id
//! and four columns, written as a server writes a plain `get`'s answer
//! (read off the stored documents). The columns are floats -- half the
//! rows whole, half with a fraction -- the whole ones by MODE:
//!
//!   * `e15`: in [1e15, 1e16), where `json::num_into` wrote the shortest
//!     digits before every digit of the int (`int_into`);
//!   * `wide`: in [2^53, 9e18), past every int a double holds;
//!   * `small`: under a million, written through `num::f64_into` as ever;
//!   * `frac`: none whole, every float its shortest digits;
//!   * `ints`: the columns ints instead, of every size up to 2^62, either
//!     sign.
//!
//! It prints ns a request, the median of 9 rounds of N / 9 (N 900 unless
//! given), and on macOS the instructions and cycles the loop retired
//! (`proc_pid_rusage`, read before and after it), the process's start and
//! setup left out.

use fennec_core::prelude::*;
use std::time::Instant;

const ROUNDS: usize = 9;
const ROWS: u64 = 1_000;

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "e15".into());
    let n: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(900);
    let mut db = Database::new();
    let kind = if mode == "ints" { "int" } else { "float" };
    let create = format!("create collection t (a {kind}, b {kind}, c {kind}, d {kind})");
    db.execute(&fennec_ql::parse_one(&create).unwrap()).unwrap();
    let put = fennec_ql::parse_one("put t {a: $1, b: $2, c: $3, d: $4}").unwrap();
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        // xorshift64*: the same values every run and every build.
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        seed.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    for row in 0..ROWS {
        let params: Vec<Value> = (0..4)
            .map(|_| {
                let r = next();
                let whole = |lo: f64, hi: f64| (lo + (r % (hi - lo) as u64) as f64).floor();
                let fraction = (r % 100_000_000) as f64 / 7.0 + 0.5;
                if mode == "ints" {
                    let v = (r >> 2) as i64 >> (r % 60);
                    return Value::Int(if r & 1 == 0 { v } else { -v });
                }
                Value::Float(match (mode.as_str(), row % 2) {
                    ("frac", _) | (_, 1) => fraction,
                    ("e15", _) => whole(1e15, 1e16),
                    ("wide", _) => whole(9_007_199_254_740_992.0, 9e18),
                    ("small", _) => whole(0.0, 1e6),
                    (other, _) => panic!("no mode {other}: e15, wide, small, frac or ints"),
                })
            })
            .collect();
        db.execute_with(&put, &params).unwrap();
    }
    let get = fennec_ql::parse_one("get t").unwrap();
    let mut out = String::new();
    // The first answer builds what a read builds once.
    db.query_json(&get, &[], &mut out).unwrap();
    let per = n.div_ceil(ROUNDS);
    let mut rounds = Vec::with_capacity(ROUNDS);
    let before = retired();
    for _ in 0..ROUNDS {
        let t = Instant::now();
        for _ in 0..per {
            // A fresh `String` each time, as a server answers each request.
            let mut out = String::new();
            let rows = db.query_json(&get, &[], &mut out).unwrap();
            assert_eq!(rows, Some(ROWS as usize));
            std::hint::black_box(out);
        }
        rounds.push(t.elapsed().as_nanos() as f64 / per as f64);
    }
    let after = retired();
    rounds.sort_by(|a, b| a.total_cmp(b));
    println!(
        "mode {mode}, {} requests, {} bytes each",
        per * ROUNDS,
        out.len()
    );
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

//! What compressing an answer costs (`fennec_http::gzip`): `make gzip-bench`.
//!
//! Rows written as a `get` answers them, at an answer's sizes from one
//! packet to a 300 000-row seed, and noise: each compressed as an answer is
//! (`gzip::compress`), its throughput in MB of input a second and what it
//! shrank to, the best of `ROUNDS`; then CRC-32 alone. `--dump DIR` writes
//! the inputs for another compressor to be held to them
//! (docs/design/http.md#compressed-answers).

use fennec_http::gzip;
use std::time::Instant;

const ROUNDS: usize = 15;

/// `n` rows of a typical collection, as `/query` and a seed write them.
fn rows(n: usize) -> Vec<u8> {
    let mut s = String::from("{\"rows\":[");
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{\"_id\":{},\"name\":\"user {}\",\"email\":\"u{}@example.com\",\
             \"age\":{},\"score\":{:.3},\"active\":{},\"created\":\"2026-{:02}-{:02}T{:02}:{:02}:00Z\",\
             \"tags\":[\"t{}\",\"t{}\"]}}",
            i + 1,
            i * 7919 % 100_003,
            i * 31,
            18 + i % 60,
            (i as f64 * 0.618_034).fract() * 100.0,
            i % 3 == 0,
            1 + i % 12,
            1 + i % 28,
            i % 24,
            i % 60,
            i % 13,
            i % 7
        ));
    }
    s.push_str("],\"seq\":1}");
    s.into_bytes()
}

fn noise(n: usize) -> Vec<u8> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    (0..n)
        .map(|_| {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8
        })
        .collect()
}

fn best(mut f: impl FnMut()) -> f64 {
    (0..ROUNDS)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64()
        })
        .fold(f64::MAX, f64::min)
}

fn main() {
    let dump = std::env::args()
        .skip_while(|a| a != "--dump")
        .nth(1)
        .map(std::path::PathBuf::from);
    let cases: Vec<(String, Vec<u8>)> = vec![
        ("rows x 1".into(), rows(1)),
        ("rows x 3".into(), rows(3)),
        ("rows x 6".into(), rows(6)),
        ("rows x 10".into(), rows(10)),
        ("rows x 30".into(), rows(30)),
        ("rows x 1 000".into(), rows(1_000)),
        ("rows x 10 000".into(), rows(10_000)),
        ("rows x 100 000".into(), rows(100_000)),
        ("rows x 300 000".into(), rows(300_000)),
        ("noise 1 MB".into(), noise(1 << 20)),
    ];
    println!(
        "{:<16} {:>12} {:>12} {:>7} {:>9} {:>10}",
        "input", "bytes", "gzip", "ratio", "MB/s", "time"
    );
    for (name, input) in &cases {
        let mut out = Vec::new();
        let secs = best(|| {
            out.clear();
            gzip::compress(input, &mut out);
        });
        println!(
            "{:<16} {:>12} {:>12} {:>6.2}x {:>9.0} {:>8.0}us",
            name,
            input.len(),
            out.len(),
            input.len() as f64 / out.len() as f64,
            input.len() as f64 / secs / 1e6,
            secs * 1e6
        );
        if let Some(dir) = &dump {
            let file = name.replace([' ', 'x'], "");
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(format!("{file}.json")), input).unwrap();
        }
    }
    let big = &cases[8].1;
    let secs = best(|| {
        std::hint::black_box(gzip::crc32(0, big));
    });
    println!("crc32 alone: {:.0} MB/s", big.len() as f64 / secs / 1e6);
}

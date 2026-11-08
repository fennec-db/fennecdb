//! What a list of numbers given an `[int]` or a `[float]` costs a write
//! through `handle` (`POST /query`), its numbers read as written: `make
//! instr-compare PKG=fennec-http EXAMPLE=number_lists
//! MODES=docs,text,params,ints,embed,vectors,short` (`cargo run --release -p
//! fennec-http --example number_lists -- MODE [N]`), one mode a process, so
//! its instructions are that mode's alone.
//!
//!   * `docs`: a page of 1 000 documents as one parameter, `put t $1`, each
//!     an `[int]` and a `[float]` of 16 -- the REST body's shape, its lists
//!     read as written inside their objects.
//!   * `text`: the same page written in the statement.
//!   * `params`: the same page a parameter a value, `put t [{id: $1, xs: $2,
//!     fs: $3}, ...]`, as the builders write it.
//!   * `ints`: the page in the statement, its `[int]`s alone: whole numbers,
//!     whose `f32`s are the numbers written.
//!   * `embed`: one document a request, a 128-dim vector beside an `[int]`
//!     of 16, each a parameter.
//!   * `vectors`: 100 documents each a 128-dim vector written in the
//!     statement, no list of numbers among them.
//!   * `short`: one document a request written in a statement the parse
//!     cache keeps, a `[float]` of 16 among its fields.
//!   * `parse` and `parse-exact`: the `vectors` text parsed alone, the quick
//!     way and as written (`fennec_ql::parse_exact`), each list a list of
//!     numbers rather than a vector -- what reading every literal as written
//!     would cost a vector, its `f32`s made from the list where it binds.
//!
//! Each request writes the same ids over. It prints ns a request, the
//! median of 9 rounds, and on macOS the instructions the loop retired a
//! request (`proc_pid_rusage`).

use fennec_core::prelude::Database;
use fennec_http::http::{Method, Request};
use fennec_http::Config;
use std::fmt::Write;
use std::sync::{Arc, RwLock};
use std::time::Instant;

const ROUNDS: usize = 9;
const DOCS: usize = 1_000;
const LIST: usize = 16;

fn request(body: String) -> Request {
    Request {
        method: Method::Post,
        target: "/query".into(),
        path: "/query".into(),
        query: Vec::new(),
        headers: Vec::new(),
        body: body.into_bytes(),
        keep_alive: true,
    }
}

fn send(db: &Arc<RwLock<Database>>, cfg: &Config, req: &Request) {
    fennec_http::metrics::begin_request();
    let resp = fennec_http::handle(db, cfg, req);
    assert!(
        resp.status < 300,
        "{}: {}",
        resp.status,
        String::from_utf8_lossy(&resp.body)
    );
}

/// The `[int]` of document `d`: whole numbers short of 2^24, the ones an
/// `f32` holds.
fn ints(d: usize) -> String {
    let xs: Vec<String> = (0..LIST)
        .map(|k| ((d * LIST + k) as i64 * 7).to_string())
        .collect();
    format!("[{}]", xs.join(", "))
}

/// Document `d`'s 128-dim vector, as JavaScript writes an `f32`'s `f64`.
fn embedding(d: usize) -> String {
    let e: Vec<String> = (0..128)
        .map(|k| (((d * 128 + k) as f32 / 7.0 - 9.0) as f64).to_string())
        .collect();
    format!("[{}]", e.join(", "))
}

/// The `[float]` of document `d`: fractions an `f32` does not hold.
fn floats(d: usize) -> String {
    let fs: Vec<String> = (0..LIST)
        .map(|k| ((d * LIST + k) as f64 / 3.0).to_string())
        .collect();
    format!("[{}]", fs.join(", "))
}

fn body(mode: &str) -> String {
    let mut out = String::new();
    match mode {
        "docs" => {
            let docs: Vec<String> = (1..=DOCS)
                .map(|d| format!(r#"{{"id": {d}, "xs": {}, "fs": {}}}"#, ints(d), floats(d)))
                .collect();
            write!(
                out,
                r#"{{"query":"put t $1","params":[[{}]]}}"#,
                docs.join(", ")
            )
            .unwrap();
        }
        "text" | "ints" => {
            let docs: Vec<String> = (1..=DOCS)
                .map(|d| match mode {
                    "text" => format!("{{id: {d}, xs: {}, fs: {}}}", ints(d), floats(d)),
                    _ => format!("{{id: {d}, xs: {}}}", ints(d)),
                })
                .collect();
            write!(out, r#"{{"query":"put t [{}]"}}"#, docs.join(", ")).unwrap();
        }
        "params" => {
            let mut docs = Vec::new();
            let mut params = Vec::new();
            for d in 1..=DOCS {
                let at = 3 * (d - 1);
                docs.push(format!(
                    "{{id: ${}, xs: ${}, fs: ${}}}",
                    at + 1,
                    at + 2,
                    at + 3
                ));
                params.push(format!("{d}, {}, {}", ints(d), floats(d)));
            }
            write!(
                out,
                r#"{{"query":"put t [{}]","params":[{}]}}"#,
                docs.join(", "),
                params.join(", ")
            )
            .unwrap();
        }
        "vectors" => {
            let docs: Vec<String> = (1..=100)
                .map(|d| format!("{{id: {d}, e: {}}}", embedding(d)))
                .collect();
            write!(out, r#"{{"query":"put t [{}]"}}"#, docs.join(", ")).unwrap();
        }
        "short" => {
            write!(
                out,
                r#"{{"query":"put t {{id: 1, xs: {}, fs: {}}}"}}"#,
                ints(1),
                floats(1)
            )
            .unwrap();
        }
        "embed" => {
            let e: Vec<String> = (0..128)
                .map(|k| (k as f64 / 7.0 - 9.0).to_string())
                .collect();
            write!(
                out,
                r#"{{"query":"put t {{id: $1, e: $2, xs: $3}}","params":[1, [{}], {}]}}"#,
                e.join(", "),
                ints(1)
            )
            .unwrap();
        }
        other => panic!("no mode {other}: docs, text, params, ints, embed, vectors or short"),
    }
    out
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "docs".into());
    if let Some(exact) = match mode.as_str() {
        "parse" => Some(false),
        "parse-exact" => Some(true),
        _ => None,
    } {
        return parse_only(exact, args.next().and_then(|a| a.parse().ok()));
    }
    let per_doc = matches!(mode.as_str(), "embed" | "short");
    let n: usize = args
        .next()
        .and_then(|a| a.parse().ok())
        .unwrap_or(if per_doc { 18_000 } else { 27 });
    let db = Arc::new(RwLock::new(Database::new()));
    let cfg = Config::default();
    let create = r#"{"query":"create collection t (xs [int], fs [float], e vector<128>)"}"#;
    send(&db, &cfg, &request(create.into()));
    let req = request(body(&mode));
    // Once before the loop: the ids are there, each request writes them over.
    send(&db, &cfg, &req);
    let per = (n / ROUNDS).max(1);
    let mut rounds = Vec::with_capacity(ROUNDS);
    let before = retired();
    for _ in 0..ROUNDS {
        let t = Instant::now();
        for _ in 0..per {
            send(&db, &cfg, &req);
        }
        rounds.push(t.elapsed().as_nanos() as f64 / per as f64);
    }
    let after = retired();
    rounds.sort_by(|a, b| a.total_cmp(b));
    println!("mode {mode}, {} requests", per * ROUNDS);
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

/// The `vectors` text parsed alone `n` times, the quick way or as written.
fn parse_only(exact: bool, n: Option<usize>) {
    let docs: Vec<String> = (1..=100)
        .map(|d| format!("{{id: {d}, e: {}}}", embedding(d)))
        .collect();
    let text = format!("put t [{}]", docs.join(", "));
    let n = n.unwrap_or(270);
    let per = (n / ROUNDS).max(1);
    let before = retired();
    for _ in 0..per * ROUNDS {
        let s = match exact {
            true => fennec_ql::parse_exact(&text),
            false => fennec_ql::parse(&text),
        };
        std::hint::black_box(s.unwrap());
    }
    let after = retired();
    println!(
        "mode parse{}, {} requests",
        ["", "-exact"][exact as usize],
        per * ROUNDS
    );
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

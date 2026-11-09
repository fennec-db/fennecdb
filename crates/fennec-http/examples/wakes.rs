//! What a statement bound to writes costs a scoped token's write: `make
//! wake-bench` (`cargo run --release -p fennec-http --example wakes --
//! [OPS] [ROUNDS]`).
//!
//! Each write goes through `fennec_http::handle` in process -- the token
//! checked, the statement parsed (once, cached), held to the token and run
//! under the write lock with the `Check` hook installed, the answer written
//! as JSON -- so what a request pays beside the database is measured with
//! no socket in it. The sides, each OPS writes a round, in ROUNDS rounds
//! whose order turns every round, the median of the rounds printed, ns a
//! write:
//!
//!   * `scoped set` and `scoped insert`: a customer's `set` of its own row
//!     by id and `insert` of a row of its own, into a collection no
//!     statement is bound to, while `orders` beside it is bound: what every
//!     scoped write pays for slice 4's authority, which must be main's;
//!   * `server set`: the same `set` under the server's token;
//!   * `server wake` and `scoped wake`: `set orders {status: 'paid'}` of one
//!     row, which wakes `paid` -- an insert into `ledger` -- each time,
//!     under the server's token and under a customer granted `paid wake`
//!     (`--call-rate` set past what the run makes, so the charge is paid
//!     and never refuses): what a granted scoped write's runs cost against
//!     the server token's. A build that grants no `wake` refuses the
//!     scoped one (403), and the side is left out, so the same program
//!     measures main beside the branch.

use fennec_core::prelude::*;
use fennec_http::access::{Access, CheckPlugin};
use fennec_http::http::{Method, Request};
use fennec_http::Config;
use std::sync::{Arc, RwLock};
use std::time::Instant;

const SECRET: &[u8] = b"thirty-two bytes and a few more, for HS256";
const ROOT: &str = "root-token";
const RULES: &str = "plain  read,insert,update  where by = $jwt.sub  for customer
orders  read,update(status)  where by = $jwt.sub  for customer
ledger  read  for customer
";

fn request(token: &str, query: &str) -> Request {
    let mut body = String::from("{\"query\":");
    fennec_core::json::escape_into(&mut body, query);
    body.push('}');
    Request {
        method: Method::Post,
        target: "/query".into(),
        path: "/query".into(),
        query: Vec::new(),
        headers: vec![("Authorization".into(), format!("Bearer {token}"))],
        body: body.into_bytes(),
        keep_alive: true,
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

fn main() {
    let mut args = std::env::args().skip(1);
    let ops: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(20_000);
    let rounds: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(9);
    // As the server does: a definition's text read into the tree it runs.
    fennec_http::install_statement_reader();
    // A build reading no `wake` line refuses the policy holding one.
    let (access, granted) = match Access::new(SECRET, &format!("{RULES}paid  wake  for customer\n"))
    {
        Ok(a) => (a, true),
        Err(_) => (Access::new(SECRET, RULES).unwrap(), false),
    };
    let access = Arc::new(access);
    let customer = access.mint(r#"{"sub":"u0","role":"customer"}"#).unwrap();
    let mut db = Database::new();
    db.install_plugin(&CheckPlugin).unwrap();
    let db = Arc::new(RwLock::new(db));
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some(ROOT.into()),
        access: Some(Arc::clone(&access)),
        call_rate: u32::MAX,
        // One customer's writes in a loop, which the default bound holds
        // to a thousand a second: the bench measures the writes.
        request_rate: 0,
        ..Config::default()
    };
    let run = |token: &str, q: &str| {
        let resp = fennec_http::handle(&db, &cfg, &request(token, q));
        (
            resp.status,
            String::from_utf8_lossy(&resp.body).into_owned(),
        )
    };
    for q in [
        "create collection plain (n int, by text)",
        "create collection orders (status text, total int, by text)",
        "create collection ledger (o int, amount int)",
        "create statement paid after set orders where new.status = 'paid'
         as insert ledger {o: new.id, amount: new.total}",
        "insert plain {n: 0, by: 'u0'}",
        "insert orders {status: 'open', total: 10, by: 'u0'}",
    ] {
        let (status, body) = run(ROOT, q);
        assert_eq!(status, 200, "{q}: {body}");
    }
    let set = "set plain {n: n + 1} where id = 1";
    let insert = "insert plain {n: 1, by: 'u0'}";
    let wake = "set orders {status: 'paid'} where id = 1";
    let mut sides: Vec<(&str, &str, &str)> = vec![
        ("scoped set", customer.as_str(), set),
        ("scoped insert", customer.as_str(), insert),
        ("server set", ROOT, set),
        ("server wake", ROOT, wake),
    ];
    let (status, body) = run(&customer, wake);
    match (status, granted) {
        (200, _) => sides.push(("scoped wake", customer.as_str(), wake)),
        (403, false) => eprintln!("scoped wake: left out, this build grants no `wake` ({body})"),
        _ => panic!("scoped wake: {status} {body}"),
    }
    for &(name, token, q) in &sides {
        let (status, body) = run(token, q);
        assert_eq!(status, 200, "{name}: {body}");
    }
    let mut times: Vec<Vec<f64>> = vec![Vec::new(); sides.len()];
    for round in 0..rounds {
        for k in 0..sides.len() {
            let i = (k + round) % sides.len();
            let (_, token, q) = sides[i];
            let req = request(token, q);
            let t = Instant::now();
            for _ in 0..ops {
                let resp = fennec_http::handle(&db, &cfg, &req);
                debug_assert_eq!(resp.status, 200);
                std::hint::black_box(resp);
            }
            times[i].push(t.elapsed().as_nanos() as f64 / ops as f64);
        }
    }
    println!("wakes: {ops} writes a round, {rounds} rounds, ns a write (median of rounds)");
    for (i, (name, ..)) in sides.iter().enumerate() {
        let mut v = times[i].clone();
        v.sort_by(|a, b| a.total_cmp(b));
        println!(
            "  {name:<14} {:>8.0}   (rounds {:.0} .. {:.0})",
            median(times[i].clone()),
            v[0],
            v[v.len() - 1]
        );
    }
}

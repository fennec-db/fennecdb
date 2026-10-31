//! Named statements called over HTTP (`docs/design/named-statements.md`,
//! slice 3): a scoped token calls what a `call` line grants it -- an
//! ungranted name answered as a missing one -- as itself, its declared
//! claims read from the token and held to their types; the body runs with
//! the statement's own authority, outside the token's scope, its answer
//! unmasked and its refusals told; a scoped `/batch` holds one call alone;
//! `append-only` binds a call's body; `--call-rate` holds a caller; and a
//! call runs one version whole, under the write lock, at every site.

use fennec_core::prelude::*;
use fennec_http::access::Access;
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, RwLock};
use std::time::Duration;

const SECRET: &[u8] = b"thirty-two bytes and a few more, for HS256";
const ROOT: &str = "root-token";
const POLICY: &str = "accounts  read  where owner = $jwt.sub
entries  read(amount)  where by = $jwt.sub
journal  append-only
transfer, history, signup, note  call  for customer
purge  call  for admin
ghost  call
";

const SCHEMA: &str = "create collection accounts (owner text @hash, balance int);
create collection entries (src int, dst int, amount int, by text);
create collection journal (what text @hash);
create collection profiles (email text @unique, owner text)";

const STATEMENTS: [&str; 5] = [
    "create statement transfer($from int, $to int, $amount int)
       claims (sub text)
       check $amount > 0 and $from != $to
     as begin
       get accounts select id where id = $from and owner = $jwt.sub
         limit 1 require 1 else 'not your account';
       set accounts {balance: balance - $amount} where id = $from and balance >= $amount
         require 1 else 'insufficient funds';
       set accounts {balance: balance + $amount} where id = $to
         require 1 else 'no such account';
       insert entries {src: $from, dst: $to, amount: $amount, by: $jwt.sub}
         returning id, amount
     end",
    "create statement history() claims (sub text) as
       get entries select id, amount, by where by = $jwt.sub order id",
    "create statement signup($email text) claims (sub text) as
       insert profiles {email: $email, owner: $jwt.sub} returning id",
    "create statement purge($w text) public as del journal where what = $w",
    "create statement note($amount int) claims (sub text) as
       insert entries {amount: $amount, by: $jwt.sub} returning id, amount, by",
];

const ACCOUNTS: i64 = 8;
const START: i64 = 1000;

struct Node {
    port: u16,
    access: Arc<Access>,
}

fn start(tune: impl FnOnce(&mut Config)) -> Node {
    fennec_http::audit::set_delay(0);
    let access = Arc::new(Access::new(SECRET, POLICY).unwrap());
    let mut cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some(ROOT.into()),
        access: Some(Arc::clone(&access)),
        call_rate: 0,
        ..Config::default()
    };
    tune(&mut cfg);
    let server = Server::new(Arc::new(RwLock::new(Database::new())), cfg);
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    let node = Node { port, access };
    for s in SCHEMA.split(';').chain(STATEMENTS) {
        node.ok(ROOT, s);
    }
    for u in 0..ACCOUNTS {
        node.ok(
            ROOT,
            &format!("put accounts {{owner: 'u{u}', balance: {START}}}"),
        );
    }
    node
}

impl Node {
    fn send(&self, token: &str, target: &str, body: &str, extra: &str) -> (u16, String, String) {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let method = if target == "/_whoami" { "GET" } else { "POST" };
        write!(
            s,
            "{method} {target} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\n{extra}\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
        (
            out[9..12].parse().unwrap(),
            head.to_string(),
            body.to_string(),
        )
    }

    fn post(&self, token: &str, target: &str, body: &str) -> (u16, String) {
        let (status, _, body) = self.send(token, target, body, "");
        (status, body)
    }

    fn query(&self, token: &str, q: &str) -> (u16, String) {
        self.post(token, "/query", &line(q))
    }

    fn ok(&self, token: &str, q: &str) -> String {
        let (status, body) = self.query(token, q);
        assert_eq!(status, 200, "{q}: {body}");
        body
    }

    fn number(&self, q: &str) -> i64 {
        let body = self.ok(ROOT, q);
        let digits: String = body
            .chars()
            .skip_while(|c| !c.is_ascii_digit() && *c != '-')
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        digits.parse().unwrap_or_else(|_| panic!("{q}: {body}"))
    }

    fn token(&self, claims: &str) -> String {
        self.access.mint(claims).unwrap()
    }

    fn customer(&self, sub: &str) -> String {
        self.token(&format!(r#"{{"sub":"{sub}","role":"customer"}}"#))
    }

    fn balances(&self) -> String {
        self.ok(ROOT, "get accounts select balance order id")
    }
}

fn line(q: &str) -> String {
    let mut body = String::from("{\"query\":");
    fennec_core::json::escape_into(&mut body, q);
    body.push('}');
    body
}

/// The review's transfer: granted `transfer call` and no write on
/// `accounts`, a customer moves money it could not move with a `/batch`,
/// and eight customers at once keep the sum as `tests/require.rs`'s
/// `/batch`es do -- every landing an entry, no balance below zero.
#[test]
fn a_scoped_token_moves_money_through_its_grant_alone() {
    const EACH: u64 = 120;
    let node = start(|_| {});
    let u0 = node.customer("u0");
    let batch = [
        line("set accounts {balance: balance - 10} where id = 1"),
        line("set accounts {balance: balance + 10} where id = 2"),
    ]
    .join("\n");
    let (status, body) = node.post(&u0, "/batch", &batch);
    assert_eq!(status, 403, "{body}");
    let (status, body) = node.query(&u0, "call transfer {from: 1, to: 2, amount: 10}");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"amount\":10"), "{body}");
    let clients: Vec<_> = (0..ACCOUNTS as u64)
        .map(|t| {
            let token = node.customer(&format!("u{t}"));
            let port = node.port;
            let access = Arc::clone(&node.access);
            std::thread::spawn(move || {
                let node = Node { port, access };
                let mut seed = 0x2545_f491_4f6c_dd1du64 ^ (t + 1);
                let mut next = |n: u64| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed % n
                };
                let (mut landed, mut refused) = (0u64, 0u64);
                for _ in 0..EACH {
                    let to = match next(8) {
                        0 => 99,
                        _ => 1 + next(ACCOUNTS as u64),
                    };
                    let to = if to == t + 1 {
                        to % ACCOUNTS as u64 + 1
                    } else {
                        to
                    };
                    let amount = 1 + next(300);
                    let q = format!(
                        "call transfer {{from: {}, to: {to}, amount: {amount}}}",
                        t + 1
                    );
                    match node.query(&token, &q) {
                        (200, _) => landed += 1,
                        (412, _) => refused += 1,
                        (status, body) => panic!("{q}: {status} {body}"),
                    }
                }
                (landed, refused)
            })
        })
        .collect();
    let (mut landed, mut refused) = (1, 0);
    for h in clients {
        let (l, r) = h.join().unwrap();
        landed += l;
        refused += r;
    }
    assert!(
        landed > 1 && refused > 0,
        "{landed} landed, {refused} refused"
    );
    assert_eq!(
        node.number("get accounts select sum(balance)"),
        ACCOUNTS * START
    );
    assert_eq!(node.number("get accounts where balance < 0 count"), 0);
    assert_eq!(node.number("get entries count") as u64, landed);
}

/// A statement the token is not granted and one no database holds are the
/// same 404, in the same words; so is one granted that is not there.
#[test]
fn an_ungranted_name_and_a_missing_one_are_the_same_404() {
    let node = start(|_| {});
    let u0 = node.customer("u0");
    for name in ["purge", "nothing", "ghost"] {
        let (status, body) = node.query(&u0, &format!("call {name} {{w: 'x'}}"));
        assert_eq!(status, 404, "{name}: {body}");
        assert_eq!(
            body,
            format!("{{\"error\":\"statement `{name}`\"}}"),
            "{name}"
        );
    }
}

/// A claim the token carries as `null`, `""`, `[]`, another type, or not
/// at all, is one it lacks: 403, nothing run. `as` is the server's token's.
#[test]
fn a_claim_null_empty_or_mistyped_is_refused_and_nothing_runs() {
    let node = start(|_| {});
    let before = node.balances();
    for sub in ["null", "\"\"", "[]", "5", "[\"u0\"]", "{\"id\":\"u0\"}"] {
        let token = node.token(&format!(r#"{{"sub":{sub},"role":"customer"}}"#));
        let (status, body) = node.query(&token, "call transfer {from: 1, to: 2, amount: 5}");
        assert_eq!(status, 403, "{sub}: {body}");
    }
    let none = node.token(r#"{"role":"customer"}"#);
    let (status, body) = node.query(&none, "call transfer {from: 1, to: 2, amount: 5}");
    assert_eq!(status, 403, "{body}");
    let u0 = node.customer("u0");
    let (status, body) = node.query(
        &u0,
        "call transfer {from: 2, to: 1, amount: 5} as {sub: 'u1'}",
    );
    assert_eq!(status, 403, "{body}");
    assert_eq!(node.balances(), before);
    assert_eq!(node.number("get entries count"), 0);
}

/// A scoped `/batch` holds one call and nothing beside it: two calls, or a
/// call beside a `get`, are refused before anything runs.
#[test]
fn a_scoped_batch_holds_one_call_alone() {
    let node = start(|_| {});
    let u0 = node.customer("u0");
    let call = line("call transfer {from: 1, to: 2, amount: 5}");
    for batch in [
        [call.clone(), call.clone()].join("\n"),
        [call.clone(), line("get accounts select balance")].join("\n"),
    ] {
        let (status, body) = node.post(&u0, "/batch", &batch);
        assert_eq!(status, 400, "{batch}: {body}");
    }
    assert_eq!(node.number("get entries count"), 0);
    let (status, body) = node.post(&u0, "/batch", &call);
    assert_eq!(status, 200, "{body}");
    // The server's token runs any.
    let both = [call.clone(), call.clone()]
        .join("\n")
        .replace("amount: 5}", "amount: 5} as {sub: 'u0'}");
    let (status, body) = node.post(ROOT, "/batch", &both);
    assert_eq!(status, 200, "{body}");
    assert_eq!(node.number("get entries count"), 3);
}

/// The answer is the statement's: a field the token's read rule withholds
/// is masked in its own `get` and answered by a call naming it.
#[test]
fn field_lists_do_not_mask_a_calls_answer() {
    let node = start(|_| {});
    let u0 = node.customer("u0");
    node.ok(&u0, "call transfer {from: 1, to: 2, amount: 7}");
    let (status, own) = node.query(&u0, "get entries select id, amount, by");
    assert!(status != 200 || own.contains("\"by\":null"), "{own}");
    let called = node.ok(&u0, "call history");
    assert!(called.contains("\"by\":\"u0\""), "{called}");
    // A write's `returning` too, which a scoped write's is held to.
    let noted = node.ok(&u0, "call note {amount: 3}");
    assert!(noted.contains("\"by\":\"u0\""), "{noted}");
}

/// A scoped caller is told a refusal's kind and its reason alone: the
/// value a `@unique` clash met, and the collections a guard read, stay the
/// server's.
#[test]
fn a_scoped_callers_error_is_told() {
    let node = start(|_| {});
    let (u0, u1) = (node.customer("u0"), node.customer("u1"));
    node.ok(&u0, "call signup {email: 'bob@x.io'}");
    let (status, body) = node.query(&u1, "call signup {email: 'bob@x.io'}");
    assert_eq!(status, 409, "{body}");
    assert!(
        !body.contains("bob@x.io") && !body.contains("email"),
        "{body}"
    );
    let (status, body) = node.query(&u0, "call transfer {from: 1, to: 2, amount: 5000}");
    assert_eq!(status, 412, "{body}");
    assert!(body.contains("\"reason\":\"insufficient funds\""), "{body}");
    assert!(!body.contains("accounts"), "{body}");
    // The server's token is told what the engine said.
    let (status, body) = node.query(ROOT, "call signup {email: 'bob@x.io'} as {sub: 'u2'}");
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("bob@x.io"), "{body}");
}

/// `append-only` takes updates and deletes from every scoped token, a
/// call's body included; the server's token keeps corrections there.
#[test]
fn an_append_only_delete_is_refused_to_a_scoped_caller() {
    let node = start(|_| {});
    node.ok(ROOT, "put journal {what: 'x'}");
    let admin = node.token(r#"{"sub":"ops","role":"admin"}"#);
    let (status, body) = node.query(&admin, "call purge {w: 'x'}");
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("append-only"), "{body}");
    assert_eq!(node.number("get journal count"), 1);
    node.ok(ROOT, "call purge {w: 'x'}");
    assert_eq!(node.number("get journal count"), 0);
}

/// `/_whoami` names the statements a token may call; `statements` and
/// `describe statement` are the server's token's.
#[test]
fn a_token_is_told_what_it_may_call() {
    let node = start(|_| {});
    let u0 = node.customer("u0");
    let (status, _, body) = node.send(&u0, "/_whoami", "", "");
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains(r#""calls":["transfer","history","signup","note","ghost"]"#),
        "{body}"
    );
    let (status, _) = node.query(&u0, "statements");
    assert_eq!(status, 403);
    let (status, _) = node.query(&u0, "describe statement transfer");
    assert_eq!(status, 404);
    let list = node.ok(ROOT, "statements");
    for name in ["transfer", "history", "signup", "purge"] {
        assert!(list.contains(&format!("\"name\":\"{name}\"")), "{list}");
    }
    let one = node.ok(ROOT, "describe statement transfer");
    assert!(
        one.contains("\"text\":\"create statement transfer"),
        "{one}"
    );
}

/// A caller past `--call-rate` is answered 429 with when to come back,
/// while another is not; a call past `--call-max-rows` is put back.
#[test]
fn a_caller_past_the_rate_waits_and_a_call_past_its_rows_is_put_back() {
    let node = start(|cfg| {
        cfg.call_rate = 1;
        cfg.call_max_rows = 1;
    });
    let (u0, u1) = (node.customer("u0"), node.customer("u1"));
    let mut held = None;
    for _ in 0..50 {
        let (status, head, body) = node.send(&u0, "/query", &line("call history"), "");
        match status {
            200 => continue,
            429 => {
                held = Some((head, body));
                break;
            }
            s => panic!("{s}: {body}"),
        }
    }
    let (head, body) = held.expect("a caller past --call-rate is held");
    assert!(head.contains("Retry-After: "), "{head}");
    assert!(body.contains("\"retry_after\":"), "{body}");
    let (status, body) = node.query(&u1, "call history");
    assert_eq!(status, 200, "{body}");
    // Two entries `history` reads, past one row.
    node.ok(ROOT, "put entries {amount: 1, by: 'u2'}");
    node.ok(ROOT, "put entries {amount: 2, by: 'u2'}");
    let (status, body) = node.query(ROOT, "call history as {sub: 'u2'}");
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("--call-max-rows"), "{body}");
}

/// A definition replaced while calls run: each call runs one version whole
/// -- both its rows of one version -- at every site that asks whether a
/// statement writes before its lock: `/query`, keyed or not, and `/batch`.
#[test]
fn a_call_runs_one_version_whole_while_its_definition_is_replaced() {
    let node = start(|_| {});
    node.ok(ROOT, "create collection lefts (n int @hash, v int)");
    node.ok(ROOT, "create collection rights (n int @hash, v int)");
    let pair = |v: u8| {
        format!(
            "create or replace statement pair($n int) public as begin
               insert lefts {{n: $n, v: {v}}};
               insert rights {{n: $n, v: {v}}} returning n, v
             end"
        )
    };
    node.ok(ROOT, &pair(1));
    let port = node.port;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let replacer = {
        let (stop, access) = (Arc::clone(&stop), Arc::clone(&node.access));
        std::thread::spawn(move || {
            let node = Node { port, access };
            let mut v = 1;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                v = 3 - v;
                node.ok(ROOT, &pair(v));
            }
        })
    };
    let callers: Vec<_> = (0..3u32)
        .map(|site| {
            let access = Arc::clone(&node.access);
            std::thread::spawn(move || {
                let node = Node { port, access };
                for i in 0..60 {
                    let n = site * 1000 + i;
                    let q = line(&format!("call pair {{n: {n}}}"));
                    let (status, _, body) = match site {
                        0 => node.send(ROOT, "/query", &q, ""),
                        1 => node.send(ROOT, "/query", &q, &format!("Idempotency-Key: k{n}\r\n")),
                        _ => node.send(ROOT, "/batch", &q, ""),
                    };
                    assert_eq!(status, 200, "{site}: {body}");
                }
            })
        })
        .collect();
    for c in callers {
        c.join().unwrap();
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    replacer.join().unwrap();
    let lefts = node.ok(ROOT, "get lefts select n, v order n");
    let rights = node.ok(ROOT, "get rights select n, v order n");
    assert_eq!(lefts, rights);
    assert_eq!(node.number("get lefts count"), 180);
}

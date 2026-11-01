//! Named statements called over HTTP (`docs/design/named-statements.md`,
//! slice 3): a scoped token calls what a `call` line grants it -- an
//! ungranted name answered as a missing one -- as itself, its declared
//! claims read from the token and held to their types; the body runs with
//! the statement's own authority, outside the token's scope, its answer
//! unmasked and its refusals told; a scoped `/batch` holds one call alone;
//! `append-only` binds a call's body; `--call-rate` holds a caller; and a
//! call runs one version whole, under the write lock, at every site. And
//! slice 4's `POST /_call/<name>`: answered as `/query` answers the `call
//! <name> $1` it stands for, its path and body held to their form, judged
//! before a marked read's refusal, made once under a key and let in across
//! origins as `/query` is.

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

#[derive(Clone)]
struct Node {
    port: u16,
    access: Arc<Access>,
    db: Arc<RwLock<Database>>,
}

fn start(tune: impl FnOnce(&mut Config)) -> Node {
    fennec_http::audit::set_delay(0);
    let access = Arc::new(Access::new(SECRET, POLICY).unwrap());
    let db = Arc::new(RwLock::new(Database::new()));
    let port = serve(&db, &access, tune);
    let node = Node { port, access, db };
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

/// A server over `db` on a port of its own, its configuration `tune`d.
fn serve(db: &Arc<RwLock<Database>>, access: &Arc<Access>, tune: impl FnOnce(&mut Config)) -> u16 {
    let mut cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some(ROOT.into()),
        access: Some(Arc::clone(access)),
        call_rate: 0,
        ..Config::default()
    };
    tune(&mut cfg);
    let server = Server::new(Arc::clone(db), cfg);
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    port
}

impl Node {
    /// A read-only server (`--read-only`) over this node's database.
    fn read_only(&self) -> Node {
        Node {
            port: serve(&self.db, &self.access, |c| c.read_only = true),
            access: Arc::clone(&self.access),
            db: Arc::clone(&self.db),
        }
    }

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
            let node = node.clone();
            std::thread::spawn(move || {
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

/// `--call-rate` counts a caller in the database the call runs in: the same
/// subject held past the rate in one -- a tenant's -- calls another at once.
/// Counted by the caller alone, tenant A's `sub` spent tenant B's.
#[test]
fn a_caller_is_held_to_the_rate_in_each_database_apart() {
    let tune = |cfg: &mut Config| cfg.call_rate = 1;
    let (a, b) = (start(tune), start(tune));
    let me = a.customer("u3");
    let mut held = false;
    for _ in 0..50 {
        match a.query(&me, "call history") {
            (200, _) => continue,
            (429, _) => {
                held = true;
                break;
            }
            (s, body) => panic!("{s}: {body}"),
        }
    }
    assert!(held, "a caller past --call-rate is held");
    let (status, body) = b.query(&me, "call history");
    assert_eq!(status, 200, "{body}");
}

/// A definition replaced while calls run: each call runs one version whole
/// -- both its rows of one version -- at every site that asks whether a
/// statement writes before its lock: `/query`, keyed or not, `/batch` and
/// `POST /_call/<name>`.
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
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let replacer = {
        let (stop, node) = (Arc::clone(&stop), node.clone());
        std::thread::spawn(move || {
            let mut v = 1;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                v = 3 - v;
                node.ok(ROOT, &pair(v));
            }
        })
    };
    let callers: Vec<_> = (0..4u32)
        .map(|site| {
            let node = node.clone();
            std::thread::spawn(move || {
                for i in 0..60 {
                    let n = site * 1000 + i;
                    let q = line(&format!("call pair {{n: {n}}}"));
                    let (status, _, body) = match site {
                        0 => node.send(ROOT, "/query", &q, ""),
                        1 => node.send(ROOT, "/query", &q, &format!("Idempotency-Key: k{n}\r\n")),
                        2 => node.send(ROOT, "/batch", &q, ""),
                        _ => node.send(
                            ROOT,
                            "/_call/pair",
                            &format!(r#"{{"params":{{"n":{n}}}}}"#),
                            "",
                        ),
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
    assert_eq!(node.number("get lefts count"), 240);
}

impl Node {
    /// A request of any method, `token` or none, with its whole head.
    fn raw(
        &self,
        method: &str,
        target: &str,
        token: Option<&str>,
        body: &str,
        extra: &str,
    ) -> (u16, String, String) {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
        write!(
            s,
            "{method} {target} HTTP/1.1\r\nHost: x\r\n{auth}{extra}\
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

    /// `POST /_call/<name>` with `params`, an object's JSON.
    fn call(&self, token: &str, name: &str, params: &str) -> (u16, String) {
        self.post(
            token,
            &format!("/_call/{name}"),
            &format!(r#"{{"params":{params}}}"#),
        )
    }

    /// `call <name> $1` over `/query`, `params` the one parameter.
    fn call_query(&self, token: &str, name: &str, params: &str) -> (u16, String) {
        let q = format!(r#"{{"query":"call {name} $1","params":[{params}]}}"#);
        self.post(token, "/query", &q)
    }
}

/// `POST /_call/<name>` answers as `/query` answers the `call <name> $1`
/// it stands for -- the same status and, for a refusal, the same words --
/// in the order a call is judged: the grant and the statement (404), the
/// claims (403), the arguments and `check` (400), a guard (412) and a
/// unique value taken (409), for a scoped caller and the server's token.
#[test]
fn a_call_by_its_path_answers_as_query_answers_it() {
    let node = start(|_| {});
    let u0 = node.customer("u0");
    let nameless = node.token(r#"{"role":"customer"}"#);
    let (status, body) = node.call(&u0, "transfer", r#"{"from":1,"to":2,"amount":10}"#);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"amount\":10"), "{body}");
    let (status, body) = node.call(&u0, "signup", r#"{"email":"a@x.io"}"#);
    assert_eq!(status, 200, "{body}");
    let cases: [(&str, &str, &str, u16); 10] = [
        (&u0, "purge", r#"{"w":"a"}"#, 404),
        (&u0, "nothing", "{}", 404),
        (&nameless, "history", "{}", 403),
        (ROOT, "history", "{}", 403),
        (&u0, "transfer", r#"{"from":1,"to":2}"#, 400),
        (&u0, "transfer", r#"{"from":1,"to":2,"amount":"ten"}"#, 400),
        (
            &u0,
            "transfer",
            r#"{"from":1,"to":2,"amount":10,"by":3}"#,
            400,
        ),
        (&u0, "transfer", r#"{"from":1,"to":2,"amount":0}"#, 400),
        (&u0, "transfer", r#"{"from":2,"to":1,"amount":10}"#, 412),
        (&u0, "signup", r#"{"email":"a@x.io"}"#, 409),
    ];
    for (token, name, params, want) in cases {
        let by_path = node.call(token, name, params);
        let by_query = node.call_query(token, name, params);
        assert_eq!(by_path.0, want, "{name} {params}: {}", by_path.1);
        assert_eq!(by_path, by_query, "{name} {params}");
    }
    // An ungranted name is the 404 a missing one is.
    let (_, ungranted) = node.call(&u0, "purge", "{}");
    let (_, missing) = node.call(&u0, "nothing", "{}");
    assert_eq!(ungranted.replace("purge", "nothing"), missing);
    // The refused calls moved nothing: one transfer of 10 landed.
    let moved = node.number("get accounts where id = 1 select balance");
    assert_eq!(moved, START - 10);
}

/// A call's body holds `params` alone, an object of the arguments by
/// name, and a statement of none takes no body at all; the path takes a
/// POST alone (405, `Allow: POST`), and a name no statement may have is
/// the 404 a missing one is.
#[test]
fn a_calls_path_and_body_are_held_to_their_form() {
    let node = start(|_| {});
    let u0 = node.customer("u0");
    for body in ["", "{}", r#"{"params":{}}"#, r#"{"params":null}"#] {
        let (status, answer) = node.post(&u0, "/_call/history", body);
        assert_eq!(status, 200, "{body:?}: {answer}");
    }
    for (body, why) in [
        (r#"{"params":[1,2]}"#, "`params` is an object"),
        (
            r#"{"params":{},"as":{"sub":"u1"}}"#,
            "`params` alone, not `as`",
        ),
        ("[1]", "expected a JSON object"),
    ] {
        let (status, answer) = node.post(&u0, "/_call/history", body);
        assert_eq!(status, 400, "{body}: {answer}");
        assert!(answer.contains(why), "{body}: {answer}");
    }
    let (status, head, _) = node.raw("GET", "/_call/history", Some(&u0), "", "");
    assert_eq!(status, 405, "{head}");
    assert!(head.contains("Allow: POST"), "{head}");
    for target in ["/_call", "/_call/history/x"] {
        let (status, _, body) = node.raw("POST", target, Some(&u0), "", "");
        assert_eq!(status, 404, "{target}: {body}");
    }
    let (_, missing) = node.call(&u0, "nothing", "{}");
    for name in ["9lives", "a-b", "x%20y"] {
        let (status, body) = node.call(&u0, name, "{}");
        assert_eq!(status, 404, "{name}: {body}");
        let name = name.replace("%20", " ");
        assert_eq!(body.replace(&name, "nothing"), missing, "{name}");
    }
    // No token: 401, before anything of the call is asked.
    let (status, _, body) = node.raw("POST", "/_call/history", None, "", "");
    assert_eq!(status, 401, "{body}");
}

/// A call marked for a replica (`Fennec-Read: replica`) is judged first --
/// its grant 404, its arguments 400 -- and then refused as the write it
/// is, 403 in words that say so, before anything of it runs.
#[test]
fn a_marked_call_is_judged_before_it_is_refused() {
    let node = start(|_| {});
    let u0 = node.customer("u0");
    let marked = "Fennec-Read: replica\r\n";
    let send = |name: &str, params: &str| {
        let body = format!(r#"{{"params":{params}}}"#);
        node.send(&u0, &format!("/_call/{name}"), &body, marked)
    };
    let (status, _, body) = send("purge", r#"{"w":"a"}"#);
    assert_eq!(status, 404, "{body}");
    let (status, _, body) = send("transfer", r#"{"from":1,"to":2}"#);
    assert_eq!(status, 400, "{body}");
    let before = node.balances();
    let (status, _, body) = send("transfer", r#"{"from":1,"to":2,"amount":10}"#);
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("Fennec-Read: replica only reads"), "{body}");
    assert_eq!(node.number("get entries count"), 0);
    assert_eq!(before, node.balances());
}

/// A read-only server judges a call before its 403 whichever way it came
/// -- by its path, over `/query` and in a `/batch` -- the grant and the
/// statement (404), the claims (403), the arguments and `check` (400) and
/// `append-only` (403), the same status in the same words, a `/batch`'s
/// with where it stopped; a call that passes is refused as the write it
/// is, nothing of it run.
#[test]
fn a_read_only_server_judges_a_call_alike_at_every_way_in() {
    let node = start(|_| {}).read_only();
    let u0 = node.customer("u0");
    let nameless = node.token(r#"{"role":"customer"}"#);
    let admin = node.token(r#"{"sub":"ops","role":"admin"}"#);
    let cases: [(&str, &str, &str, u16, &str); 7] = [
        (&u0, "purge", r#"{"w":"a"}"#, 404, "purge"),
        (ROOT, "nothing", "{}", 404, "nothing"),
        (&nameless, "history", "{}", 403, "sub"),
        (&u0, "transfer", r#"{"from":1,"to":2}"#, 400, "amount"),
        (
            &u0,
            "transfer",
            r#"{"from":1,"to":2,"amount":0}"#,
            400,
            "check",
        ),
        (&admin, "purge", r#"{"w":"a"}"#, 403, "append-only"),
        (
            &u0,
            "transfer",
            r#"{"from":1,"to":2,"amount":10}"#,
            403,
            "read-only",
        ),
    ];
    for (token, name, params, want, says) in cases {
        let by_path = node.call(token, name, params);
        let by_query = node.call_query(token, name, params);
        let line = format!(r#"{{"query":"call {name} $1","params":[{params}]}}"#);
        let by_batch = node.post(token, "/batch", &line);
        assert_eq!(by_path.0, want, "{name} {params}: {}", by_path.1);
        assert!(by_path.1.contains(says), "{name} {params}: {}", by_path.1);
        assert_eq!(by_path, by_query, "{name} {params}");
        assert_eq!(by_batch.0, want, "{name} {params}: {}", by_batch.1);
        // A line's refusal is the same words, with its kind's first where
        // it stopped the batch, as every `/batch` line's is.
        let words = by_path.1.trim_start_matches(r#"{"error":""#);
        let words = words.trim_end_matches(r#""}"#);
        assert!(by_batch.1.contains(words), "{}: {}", by_path.1, by_batch.1);
    }
    assert_eq!(node.number("get entries count"), 0);
}

/// An `Idempotency-Key` makes a call once: sent again it is the answer
/// kept, replayed, and the money moved once; the key with another body is
/// 422, the request's hash being over its path and body.
#[test]
fn an_idempotency_key_makes_a_call_once() {
    let node = start(|_| {});
    let u0 = node.customer("u0");
    let body = r#"{"params":{"from":1,"to":2,"amount":10}}"#;
    let key = "Idempotency-Key: t-1\r\n";
    let (status, first_head, first) = node.send(&u0, "/_call/transfer", body, key);
    assert_eq!(status, 200, "{first}");
    assert!(!first_head.contains("Idempotent-Replayed"), "{first_head}");
    let (status, head, again) = node.send(&u0, "/_call/transfer", body, key);
    assert_eq!(status, 200, "{again}");
    assert!(head.contains("Idempotent-Replayed: true"), "{head}");
    assert_eq!(first, again);
    assert_eq!(node.number("get entries count"), 1);
    let other = r#"{"params":{"from":1,"to":2,"amount":20}}"#;
    let (status, _, body) = node.send(&u0, "/_call/transfer", other, key);
    assert_eq!(status, 422, "{body}");
    let balance = node.number("get accounts where id = 1 select balance");
    assert_eq!(balance, START - 10);
}

/// A page on another origin calls as it queries: the preflight of a call
/// is answered before any token, and the call's answer carries the
/// headers `/query`'s does.
#[test]
fn a_page_on_another_origin_calls_as_it_queries() {
    let node = start(|cfg| cfg.cors = Some("https://app.example".into()));
    let u0 = node.customer("u0");
    let preflight = "Origin: https://app.example\r\nAccess-Control-Request-Method: POST\r\n\
                     Access-Control-Request-Headers: authorization, idempotency-key\r\n";
    let (status, head, _) = node.raw("OPTIONS", "/_call/transfer", None, "", preflight);
    assert_eq!(status, 204, "{head}");
    let allowed = "Access-Control-Allow-Origin: https://app.example";
    assert!(head.contains(allowed), "{head}");
    assert!(head.contains("idempotency-key"), "{head}");
    let body = r#"{"params":{"from":1,"to":2,"amount":10}}"#;
    let origin = "Origin: https://app.example\r\n";
    let (status, head, body) = node.send(&u0, "/_call/transfer", body, origin);
    assert_eq!(status, 200, "{body}");
    assert!(head.contains(allowed), "{head}");
    assert!(head.contains("Fennec-Seq"), "{head}");
}

// ------------------------------------------------------- slice 5: reads

impl Node {
    /// Where the database stands: its change counter.
    fn seq(&self) -> u64 {
        self.db.read().unwrap().change_seq()
    }

    /// u0 moved 10 from 1 to 2, and 5 from 1 to 3: two entries of hers.
    fn with_entries(self) -> Node {
        let u0 = self.customer("u0");
        for (to, amount) in [(2, 10), (3, 5)] {
            let params = format!(r#"{{"from":1,"to":{to},"amount":{amount}}}"#);
            let (status, body) = self.call(&u0, "transfer", &params);
            assert_eq!(status, 200, "{body}");
        }
        self
    }
}

/// A call whose statement reads is a read: a read-only server and a read
/// marked for a replica answer it -- by its path, over `/query` and in a
/// `/batch`, the same rows the primary answers -- naming the change it read
/// at, keeping no key and moving no counter; one that writes is still
/// refused there.
#[test]
fn a_call_that_reads_is_answered_where_reads_are() {
    let node = start(|_| {}).with_entries();
    let ro = node.read_only();
    let u0 = node.customer("u0");
    let before = node.seq();
    let (status, home) = node.call(&u0, "history", "{}");
    assert_eq!(status, 200, "{home}");
    assert!(home.contains(r#""amount":10"#), "{home}");
    assert!(home.contains(r#""amount":5"#), "{home}");
    let marked = "Fennec-Read: replica\r\n";
    let key = "Idempotency-Key: read-1\r\n";
    for (server, extra) in [(&ro, ""), (&node, marked), (&ro, key), (&node, key)] {
        let (status, head, body) = server.send(&u0, "/_call/history", r#"{"params":{}}"#, extra);
        assert_eq!((status, &body), (200, &home), "{extra}");
        assert!(head.contains("Fennec-Seq"), "{head}");
        assert!(!head.contains("Idempotent-Replayed"), "{head}");
        let (status, body) = server.call_query(&u0, "history", "{}");
        assert_eq!((status, &body), (200, &home));
        let (status, batch) = server.post(&u0, "/batch", &line("call history"));
        assert_eq!(status, 200, "{batch}");
        assert!(batch.contains(r#""amount":10"#), "{batch}");
    }
    // The server's token reads with the claims it names, a batch of reads
    // and calls together.
    let lines = [
        line("call history as {sub: 'u0'}"),
        line("get accounts select balance where id = 1"),
    ];
    let (status, both) = ro.post(ROOT, "/batch", &lines.join("\n"));
    assert_eq!(status, 200, "{both}");
    assert!(both.contains(r#""amount":5"#), "{both}");
    assert!(both.contains(r#""balance":985"#), "{both}");
    let (status, body) = ro.call(&u0, "transfer", r#"{"from":1,"to":2,"amount":1}"#);
    assert_eq!(status, 403, "{body}");
    assert_eq!(node.seq(), before);
    let (status, _) = node.query(ROOT, "get _idempotency count");
    assert_eq!(status, 404, "a read kept a key");
}

/// A call that reads takes the read lock alone: it is answered while
/// another reader holds the database, whichever way it came, where one
/// under the write lock would wait for that reader.
#[test]
fn a_call_that_reads_takes_no_write_lock() {
    let node = start(|_| {}).with_entries();
    let u0 = node.customer("u0");
    let reader = node.db.read().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let asked = {
        let node = node.clone();
        std::thread::spawn(move || {
            let by_path = node.call(&u0, "history", "{}");
            let by_query = node.call_query(&u0, "history", "{}");
            let by_batch = node.post(&u0, "/batch", &line("call history"));
            let _ = tx.send([by_path, by_query, by_batch]);
        })
    };
    let answers = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("a call that reads waited for a lock another reader holds");
    drop(reader);
    asked.join().unwrap();
    for (status, body) in answers {
        assert_eq!(status, 200, "{body}");
    }
}

/// A server whose database follows a primary takes no write of its own,
/// and answers a call that reads; one that writes is refused as the
/// replica's every write is.
#[test]
fn a_replica_answers_a_call_that_reads() {
    let node = start(|_| {}).with_entries();
    node.db.write().unwrap().follow(vec![(5, 0)]).unwrap();
    let u0 = node.customer("u0");
    let (status, body) = node.call(&u0, "history", "{}");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""amount":5"#), "{body}");
    let (status, body) = node.post(&u0, "/batch", &line("call history"));
    assert_eq!(status, 200, "{body}");
    let (status, body) = node.call(&u0, "transfer", r#"{"from":1,"to":2,"amount":1}"#);
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("replica"), "{body}");
}

/// A long call is read on a pin, writes landing beside it; one whose body
/// an index answers -- declined by the pin, or refused on it -- is read
/// under the lock: the same rows either way.
#[test]
fn a_call_read_on_a_pin_answers_as_under_the_lock() {
    let node = start(|_| {}).with_entries();
    let mine = "create statement mine() claims (sub text) as
                  get accounts select id, balance where owner = $jwt.sub";
    node.ok(ROOT, mine);
    let history = node.ok(ROOT, "call history as {sub: 'u0'}");
    let owned = node.ok(ROOT, "call mine as {sub: 'u0'}");
    node.db.write().unwrap().set_pin_at(0);
    let writer = {
        let node = node.clone();
        std::thread::spawn(move || {
            for i in 0..50 {
                node.ok(ROOT, &format!("put journal {{what: 'w{i}'}}"));
            }
        })
    };
    let lines = [
        line("call history as {sub: 'u0'}"),
        line("call mine as {sub: 'u0'}"),
    ];
    for _ in 0..50 {
        assert_eq!(node.ok(ROOT, "call history as {sub: 'u0'}"), history);
        assert_eq!(node.ok(ROOT, "call mine as {sub: 'u0'}"), owned);
        let (status, both) = node.post(ROOT, "/batch", &lines.join("\n"));
        assert_eq!(status, 200, "{both}");
    }
    writer.join().unwrap();
}

/// A statement made again between reading and writing while it is called
/// at every site that asks whether a statement writes before its lock --
/// `/query`, keyed or not, `/batch` and `POST /_call/<name>`: each call
/// runs as the statement it finds under the lock it runs under, a read as a
/// read and a write as a write, never refused for the other; on a
/// read-only server it reads or is refused, and never writes.
#[test]
fn a_call_runs_as_the_statement_it_finds_under_its_lock() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
    let node = start(|_| {});
    let ro = node.read_only();
    node.ok(ROOT, "create collection marks (v int)");
    let reads = "create statement flip() public as get marks select id, v where v = 0";
    let writes = "create statement flip() public as insert marks {v: 1} returning id, v";
    node.ok(ROOT, reads);
    let stop = Arc::new(AtomicBool::new(false));
    let flipper = {
        let (stop, node) = (Arc::clone(&stop), node.clone());
        std::thread::spawn(move || {
            let mut i = 0;
            while !stop.load(Relaxed) {
                i += 1;
                let next = if i % 2 == 1 { writes } else { reads };
                let batch = [line("drop statement flip"), line(next)].join("\n");
                let (status, body) = node.post(ROOT, "/batch", &batch);
                assert_eq!(status, 200, "{body}");
            }
        })
    };
    let wrote = Arc::new(AtomicUsize::new(0));
    let callers: Vec<_> = (0..5u32)
        .map(|site| {
            let (node, ro, wrote) = (node.clone(), ro.clone(), Arc::clone(&wrote));
            std::thread::spawn(move || {
                for i in 0..60 {
                    let q = line("call flip");
                    let keyed = format!("Idempotency-Key: f{i}\r\n");
                    let (status, _, body) = match site {
                        0 => node.send(ROOT, "/query", &q, ""),
                        1 => node.send(ROOT, "/query", &q, &keyed),
                        2 => node.send(ROOT, "/batch", &q, ""),
                        3 => node.send(ROOT, "/_call/flip", "", ""),
                        _ => ro.send(ROOT, "/_call/flip", "", ""),
                    };
                    match status {
                        403 if site == 4 => assert!(body.contains("read-only"), "{body}"),
                        200 if body.contains(r#""v":1"#) => {
                            assert!(site != 4, "a read-only server wrote: {body}");
                            wrote.fetch_add(1, Relaxed);
                        }
                        200 => assert!(body.contains("[]"), "{site}: {body}"),
                        _ => panic!("{site}: {status} {body}"),
                    }
                }
            })
        })
        .collect();
    for c in callers {
        c.join().unwrap();
    }
    stop.store(true, Relaxed);
    flipper.join().unwrap();
    assert_eq!(node.number("get marks count"), wrote.load(Relaxed) as i64);
}

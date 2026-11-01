//! The read executor (`docs/design/named-statements.md`, slice 5): a call
//! whose body only reads runs over `&Database` -- beside other readers,
//! pinned where it is long, on a replica -- refuses a body that writes,
//! opens no block and moves no counter; its rows are bounded on its own
//! thread, so calls and plain reads beside it are charged nothing of each
//! other's.

use fennec_core::engine::CallRead;
use fennec_core::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

const SCHEMA: &str = "create collection accounts (owner text @hash, balance int);
create collection entries (src int, amount int, by text)";

const STATEMENTS: &str = "create statement mine() claims (sub text) as
  get accounts select id, balance where owner = $jwt.sub order id;
create statement rich($over int) public as
  get accounts select id, balance where balance > $over order id;
create statement both($over int) public as begin
  get accounts select id where balance > $over limit 1 require 1 else 'none that rich';
  get entries select id, amount where amount > $over order id
end;
create statement pay($amount int) claims (sub text) as
  insert entries {amount: $amount, by: $jwt.sub} returning id";

fn run(db: &mut Database, text: &str) -> Result<Response> {
    let mut last = Response::Affected(0);
    for s in fennec_ql::parse(text).expect("parse") {
        last = db.execute(&s)?;
    }
    Ok(last)
}

fn one(text: &str) -> Statement {
    fennec_ql::parse_one(text).expect("parse")
}

/// Accounts 1 to `n`, owned by `u0`..`u(n-1)`, account `i` holding
/// `i * 10`; an entry for each, of `i`.
fn bank(n: i64) -> Database {
    fennec_ql::install();
    let mut db = Database::new();
    run(&mut db, SCHEMA).unwrap();
    for i in 1..=n {
        run(
            &mut db,
            &format!(
                "put accounts {{owner: 'u{}', balance: {}}}; put entries {{src: {i}, amount: {i}}}",
                i - 1,
                i * 10
            ),
        )
        .unwrap();
    }
    run(&mut db, STATEMENTS).unwrap();
    db
}

fn ids(r: &Response) -> Vec<i64> {
    let rows = &r.rows().expect("rows").rows;
    rows.iter()
        .map(|r| match r.values[0] {
            Value::Int(i) => i,
            ref v => panic!("{v:?}"),
        })
        .collect()
}

/// A body of `get`s is read through `&Database` as a statement is, its
/// answer the last statement's and its guards' reasons its refusals, and
/// `execute` reads it too: no block opened, no counter moved.
#[test]
fn a_call_that_reads_is_answered_over_a_shared_database() {
    let mut db = bank(4);
    assert_eq!(reads(&db, "rich"), Some(true));
    assert_eq!(reads(&db, "pay"), Some(false));
    assert_eq!(reads(&db, "nothing"), None);
    let before = db.change_seq();
    let r = db.query(&one("call rich {over: 15}"), &[]).unwrap();
    assert_eq!(ids(&r), [2, 3, 4]);
    let r = db
        .query(&one("call mine as {sub: 'u2'}"), &[])
        .expect("claims bound");
    assert_eq!(ids(&r), [3]);
    // The answer is the last statement's; a guard before it says why.
    let r = db.query(&one("call both {over: 2}"), &[]).unwrap();
    assert_eq!(ids(&r), [3, 4]);
    let e = db.query(&one("call both {over: 99}"), &[]).unwrap_err();
    assert_eq!(e.reason(), Some("none that rich"), "{e}");
    // Judged as every call: the statement, the claims, the arguments.
    let e = db.query(&one("call nothing"), &[]).unwrap_err();
    assert!(matches!(e, Error::NotFound(_)), "{e}");
    let e = db.query(&one("call mine"), &[]).unwrap_err();
    assert!(matches!(e, Error::Denied(_)), "{e}");
    let e = db.query(&one("call rich {over: 'x'}"), &[]).unwrap_err();
    assert!(e.to_string().contains("over"), "{e}");
    // `execute` reads it as `query` does.
    let r = run(&mut db, "call rich {over: 35}").unwrap();
    assert_eq!(ids(&r), [4]);
    assert!(!db.in_block());
    assert_eq!(db.change_seq(), before);
}

/// [`Database::call_reads`] of `call <name>`.
fn reads(db: &Database, name: &str) -> Option<bool> {
    match one(&format!("call {name}")) {
        Statement::Call(c) => db.call_reads(&c),
        _ => unreachable!(),
    }
}

/// A call's last statement, a plain `get`, is written out as JSON from the
/// stored documents -- the bytes its rows render to, as `query_json` holds
/// a `get` to `query` -- after its guards; any other is answered as rows.
#[test]
fn a_call_that_reads_is_written_out_as_its_get_is() {
    let db = bank(4);
    for (text, rows) in [("call rich {over: 15}", 3), ("call both {over: 2}", 2)] {
        let Statement::Call(c) = one(text) else {
            unreachable!()
        };
        let callee = db.callee(&c, &[]).unwrap();
        let mut out = String::new();
        let CallRead::Json(n) = db.read_call_json(&callee, &mut out).unwrap() else {
            panic!("{text}: a plain get's rows are written out");
        };
        assert_eq!(n, rows);
        let mut rendered = String::new();
        let r = db.read_call(&callee).unwrap();
        fennec_core::json::rows_array_into(&mut rendered, r.rows().unwrap());
        assert_eq!(out, rendered, "{text}");
    }
    let Statement::Call(c) = one("call both {over: 99}") else {
        unreachable!()
    };
    let callee = db.callee(&c, &[]).unwrap();
    let e = db
        .read_call_json(&callee, &mut String::new())
        .err()
        .unwrap();
    assert_eq!(e.reason(), Some("none that rich"));
}

/// The read executor cannot write, by its type, and refuses a body that
/// writes rather than run it: nothing lands, and the counter stays.
#[test]
fn the_read_executor_refuses_a_body_that_writes() {
    let db = bank(2);
    let before = db.change_seq();
    let e = db
        .query(&one("call pay {amount: 5} as {sub: 'u0'}"), &[])
        .unwrap_err();
    assert!(matches!(e, Error::ReadOnly(_)), "{e}");
    assert!(e.to_string().contains("`pay` writes"), "{e}");
    // Judged before: a writing call's bad argument is its 400 still.
    let e = db
        .query(&one("call pay {amount: 'x'} as {sub: 'u0'}"), &[])
        .unwrap_err();
    assert!(!matches!(e, Error::ReadOnly(_)), "{e}");
    assert_eq!(db.change_seq(), before);
    assert!(db
        .pin(&[(&one("call pay {amount: 5} as {sub: 'u0'}"), &[])])
        .is_none());
}

/// A replica takes no write of its own, and answers a call that reads --
/// alone and in a block -- while one that writes is refused as every
/// write is there.
#[test]
fn a_replica_answers_a_call_that_reads() {
    let mut db = bank(3);
    db.follow(vec![(5, 0)]).unwrap();
    let before = db.change_seq();
    let r = run(&mut db, "call rich {over: 15}").unwrap();
    assert_eq!(ids(&r), [2, 3]);
    let reads = one("call rich {over: 25}");
    let get = one("get accounts select id order id");
    let r = db.execute_block(&[(&reads, &[]), (&get, &[])]).unwrap();
    assert_eq!(ids(&r[0]), [3]);
    let e = run(&mut db, "call pay {amount: 5} as {sub: 'u0'}").unwrap_err();
    assert!(matches!(e, Error::ReadOnly(_)), "{e}");
    assert!(e.to_string().contains("replica"), "{e}");
    assert_eq!(db.change_seq(), before);
}

/// A call's rows are bounded on its own thread: eight calls at once under
/// one shared database, each reaching the whole of its bound, and plain
/// scans beside them, none charged another's rows. Held in the database,
/// as the writer's bound was, the calls spent each other's and the scans
/// were refused as calls past their rows.
#[test]
fn a_reading_calls_rows_are_its_threads_alone() {
    let mut db = bank(50);
    // `rich {over: 0}` scans the 50 accounts: its bound exactly.
    db.set_call_max_rows(50);
    let e = {
        let mut tight = bank(50);
        tight.set_call_max_rows(49);
        tight.query(&one("call rich {over: 0}"), &[]).unwrap_err()
    };
    assert!(e.to_string().contains("--call-max-rows"), "{e}");
    let db = &db;
    let refused = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for t in 0..12 {
            let refused = &refused;
            s.spawn(move || {
                let call = one("call rich {over: 0}");
                let scan = one("get accounts select id where balance >= 0");
                for _ in 0..300 {
                    let r = match t % 3 {
                        0 => db.query(&scan, &[]),
                        _ => db.query(&call, &[]),
                    };
                    match r {
                        Ok(r) => assert_eq!(ids(&r).len(), 50),
                        Err(_) => {
                            refused.fetch_add(1, Relaxed);
                        }
                    }
                }
            });
        }
    });
    assert_eq!(refused.load(Relaxed), 0);
    // And none left behind: a plain scan once they are done.
    assert_eq!(
        ids(&db.query(&one("get accounts select id"), &[]).unwrap()).len(),
        50
    );
}

/// A long call is pinned as its body would be: read on the pin with no
/// lock held, as at the pin; one its index answers is declined, and one
/// let through by mistake meets the index's refusal, answered `None` for
/// the lock to run.
#[test]
fn a_long_call_is_pinned_as_its_body() {
    let mut db = bank(30);
    db.set_pin_at(0);
    let scan = one("call rich {over: 0}");
    let pin = db.pin(&[(&scan, &[])]).expect("a scan is pinned");
    // A write after the pin is not in what the pin reads.
    run(&mut db, "put accounts {owner: 'late', balance: 999}").unwrap();
    let r = pin.query(&scan, &[]).expect("held").unwrap();
    assert_eq!(ids(&r).len(), 30);
    assert_eq!(ids(&db.query(&scan, &[]).unwrap()).len(), 31);
    // Judged once, pinned and read by its callee.
    let c = match &scan {
        Statement::Call(c) => c,
        _ => unreachable!(),
    };
    let callee = db.callee(c, &[]).unwrap();
    let pin = db.pin_call(&callee).expect("pinned");
    assert_eq!(
        ids(&pin.read_call(&callee).expect("held").unwrap()).len(),
        31
    );
    // `owner` is hashed: its equality is the index's, which a pin holds
    // none of -- declined, and refused where pinned regardless.
    let mine = one("call mine as {sub: 'u3'}");
    assert!(db.pin(&[(&mine, &[])]).is_none());
    let every = db.pin_every_collection();
    assert!(every.query(&mine, &[]).is_none());
    assert_eq!(ids(&db.query(&mine, &[]).unwrap()), [4]);
}

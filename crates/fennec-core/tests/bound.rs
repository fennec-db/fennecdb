//! Write-bound statements run (`docs/design/named-statements.md`, F2's
//! slice 3): in the block of the write that woke them, once a row, after
//! the statement's own rows -- rows first, then names -- each condition
//! asked as its row is written; bounded by `MAX_WOKEN` and
//! `--call-max-rows`, refused never cut; depth one, a chain and a binding
//! of its own collection refused at definition; and every writer under
//! `Database::unbound` waking nothing.

use fennec_core::codec::{encode_str, put_uvarint};
use fennec_core::named::{Definition, GRAMMAR_EPOCH, MAX_WOKEN};
use fennec_core::prelude::*;

const SCHEMA: &str = "create collection orders (status text, total int, account int);
create collection accounts (owner text @hash, balance int);
create collection ledger (o int, account int, amount int, at timestamp);
create collection items (n int, k text);
create collection log (kind text, n int, was int, at timestamp);
create collection other (n int, k text @unique)";

const PAID: &str = "create statement paid_ledger
  after set orders where new.status = 'paid' and old.status = 'open'
  comment 'A ledger line and the debit for each order as it is paid'
as begin
  get accounts select id where id = new.account limit 1 require 1 else 'no such account';
  insert ledger {o: new.id, account: new.account, amount: new.total, at: now()};
  set accounts {balance: balance - new.total} where id = new.account
end";

fn database() -> Database {
    fennec_ql::install();
    let mut db = Database::new();
    run(&mut db, SCHEMA).unwrap();
    db
}

/// Each statement of `text` on its own, a block each.
fn run(db: &mut Database, text: &str) -> Result<Response> {
    let mut last = Response::Affected(0);
    for s in fennec_ql::parse(text).expect("parse") {
        last = db.execute(&s)?;
    }
    Ok(last)
}

/// The statements of `text` as one block.
fn block(db: &mut Database, text: &str) -> Result<Vec<Response>> {
    let stmts = fennec_ql::parse(text).expect("parse");
    let pairs: Vec<(&Statement, &[Value])> = stmts.iter().map(|s| (s, &[][..])).collect();
    db.execute_block(&pairs).map_err(|(_, e)| e)
}

/// The rows of a `get`, each its values.
fn rows(db: &mut Database, get: &str) -> Vec<Vec<Value>> {
    match run(db, get).unwrap() {
        Response::Rows(rs) => rs.rows.into_iter().map(|r| r.values).collect(),
        other => panic!("{other:?}"),
    }
}

fn int(n: i64) -> Value {
    Value::Int(n)
}

fn text(t: &str) -> Value {
    Value::Text(t.into())
}

/// `log`'s rows in the order written: kind, n, was.
fn logged(db: &mut Database) -> Vec<(String, Value, Value)> {
    rows(db, "get log select kind, n, was order id")
        .into_iter()
        .map(|r| {
            let Value::Text(k) = &r[0] else {
                panic!("{r:?}")
            };
            (k.clone(), r[1].clone(), r[2].clone())
        })
        .collect()
}

#[test]
fn the_ledger_line_and_the_debit_land_with_the_write_that_paid() {
    let mut db = database();
    run(
        &mut db,
        "put accounts {owner: 'ann', balance: 100};
         put orders [{status: 'open', total: 30, account: 1},
                     {status: 'open', total: 5, account: 99},
                     {status: 'new', total: 7, account: 1}]",
    )
    .unwrap();
    run(&mut db, PAID).unwrap();
    let before = db.change_seq();
    run(&mut db, "set orders {status: 'paid'} where id = 1").unwrap();
    // The order, the ledger line and the debit: one block, three writes.
    assert_eq!(db.change_seq(), before + 3);
    assert_eq!(
        rows(&mut db, "get ledger select o, account, amount"),
        [[int(1), int(1), int(30)]]
    );
    assert_eq!(rows(&mut db, "get accounts select balance"), [[int(70)]]);
    // A row the condition does not pass wakes nothing: `new` was not
    // `open`, and a `set` of another field leaves `status` as it was.
    run(&mut db, "set orders {status: 'paid'} where id = 3").unwrap();
    run(&mut db, "set orders {total: 31} where id = 1").unwrap();
    assert_eq!(rows(&mut db, "get ledger select o").len(), 1);
    assert_eq!(db.woken(), None);

    // A guard refusing: 412 with its reason, the block put back whole --
    // the writer's row and every write before it in the block.
    let seq = db.change_seq();
    let e = block(
        &mut db,
        "put accounts {owner: 'bob', balance: 1}; set orders {status: 'paid'} where id = 2",
    )
    .unwrap_err();
    assert!(matches!(&e, Error::Unmet(_)), "{e}");
    assert_eq!(e.reason(), Some("no such account"), "{e}");
    assert_eq!(db.woken(), Some("paid_ledger"), "the binding noted");
    assert_eq!(db.change_seq(), seq, "nothing landed");
    assert_eq!(
        rows(&mut db, "get orders select status where id = 2"),
        [[text("open")]]
    );
    assert_eq!(rows(&mut db, "get accounts select id").len(), 1);
    // The next statement begins with no note.
    run(&mut db, "put accounts {owner: 'cy', balance: 1}").unwrap();
    assert_eq!(db.woken(), None);
}

#[test]
fn each_event_reads_new_and_old_as_it_has_them() {
    let mut db = database();
    run(
        &mut db,
        "create statement made after insert items as insert log {kind: 'insert', n: new.n};
         create statement changed after set items as insert log {kind: 'set', n: new.n, was: old.n};
         create statement gone after del items as insert log {kind: 'del', was: old.n}",
    )
    .unwrap();
    assert_eq!(db.collection("items").unwrap().bound, 7);
    run(&mut db, "insert items {n: 1}").unwrap(); // id 1
    run(&mut db, "put items {n: 2}").unwrap(); // id 2, no id named
    run(&mut db, "put items {id: 1, n: 5}").unwrap(); // a live row written over
    run(&mut db, "put items {id: 9, n: 3}").unwrap(); // an id not held
                                                      // An upsert making a row and setting one in one statement.
    run(
        &mut db,
        "put items [{id: 1, n: 1}, {id: 10, n: 4}] if absent else set {n: n + new.n}",
    )
    .unwrap();
    run(&mut db, "set items {n: n * 10} where id = 2").unwrap();
    run(&mut db, "del items where id = 9").unwrap();
    let null = Value::Null;
    assert_eq!(
        logged(&mut db),
        [
            ("insert".into(), int(1), null.clone()),
            ("insert".into(), int(2), null.clone()),
            ("set".into(), int(5), int(1)),
            ("insert".into(), int(3), null.clone()),
            ("set".into(), int(6), int(5)),
            ("insert".into(), int(4), null.clone()),
            ("set".into(), int(20), int(2)),
            ("del".into(), null.clone(), int(3)),
        ]
    );
}

#[test]
fn rows_run_in_the_order_written_and_on_a_row_its_bindings_by_name() {
    let mut db = database();
    run(
        &mut db,
        "create statement zz after insert or set items as insert log {kind: 'zz', n: new.n};
         create statement aa after insert or set items as insert log {kind: 'aa', n: new.n}",
    )
    .unwrap();
    run(&mut db, "put items [{n: 1}, {n: 2}]").unwrap();
    let kinds = |db: &mut Database| -> Vec<(String, Value)> {
        logged(db).into_iter().map(|(k, n, _)| (k, n)).collect()
    };
    assert_eq!(
        kinds(&mut db),
        [
            ("aa".into(), int(1)),
            ("zz".into(), int(1)),
            ("aa".into(), int(2)),
            ("zz".into(), int(2)),
        ]
    );
    // A picked `set` wakes its rows in its pick's order.
    run(&mut db, "del log where n >= 0").unwrap();
    run(
        &mut db,
        "set items {n: n + 10} where n >= 0 order n desc limit 2",
    )
    .unwrap();
    assert_eq!(
        kinds(&mut db),
        [
            ("aa".into(), int(12)),
            ("zz".into(), int(12)),
            ("aa".into(), int(11)),
            ("zz".into(), int(11)),
        ]
    );
}

#[test]
fn a_put_over_an_expired_row_is_a_row_made() {
    let mut db = database();
    run(
        &mut db,
        "create collection sess (at timestamp @ttl(1m), n int);
         create statement opened after insert sess as insert log {kind: 'insert', n: new.n};
         create statement moved after set sess as insert log {kind: 'set', n: new.n, was: old.n}",
    )
    .unwrap();
    let t0 = 1_800_000_000_000;
    db.set_clock(Some(t0));
    run(&mut db, "put sess {id: 1, at: now(), n: 1}").unwrap();
    run(&mut db, "put sess {id: 1, at: now(), n: 2}").unwrap();
    // Two minutes on, the row is gone to every read, and a `put` naming
    // its id makes one: an `insert`, with no `old.`.
    db.set_clock(Some(t0 + 120_000));
    run(&mut db, "put sess {id: 1, at: now(), n: 3}").unwrap();
    assert_eq!(
        logged(&mut db),
        [
            ("insert".into(), int(1), Value::Null),
            ("set".into(), int(2), int(1)),
            ("insert".into(), int(3), Value::Null),
        ]
    );
}

#[cfg(feature = "nocase")]
#[test]
fn a_condition_over_a_nocase_field_wakes_where_a_get_finds() {
    let mut db = database();
    run(
        &mut db,
        "create collection users (email text nocase, n int);
         create statement signed after insert users where new.email = 'ANN@X.IO'
         as insert log {kind: 'signed', n: new.n}",
    )
    .unwrap();
    run(&mut db, "insert users {email: 'ann@x.io', n: 1}").unwrap();
    run(&mut db, "insert users {email: 'bob@x.io', n: 2}").unwrap();
    assert_eq!(
        rows(&mut db, "get users select n where email = 'ANN@X.IO'"),
        [[int(1)]]
    );
    assert_eq!(rows(&mut db, "get log select n"), [[int(1)]]);
}

#[test]
fn a_write_past_max_woken_rows_is_refused_whole() {
    let mut db = database();
    run(
        &mut db,
        "create statement counted after insert items where new.n > 0
         as insert log {kind: 'c', n: new.n}",
    )
    .unwrap();
    let docs = |n: usize, k: i64| -> String {
        let one = format!("{{n: {k}}}");
        format!("put items [{}]", vec![one; n].join(", "))
    };
    // A row passing no condition counts for nothing.
    run(&mut db, &docs(MAX_WOKEN + 5, 0)).unwrap();
    let seq = db.change_seq();
    let e = run(&mut db, &docs(MAX_WOKEN + 1, 1)).unwrap_err();
    assert!(e.to_string().contains("at most 10000 rows"), "{e}");
    assert_eq!(db.change_seq(), seq, "nothing landed");
    assert_eq!(rows(&mut db, "get log count").len(), 1);
    run(&mut db, &docs(MAX_WOKEN, 1)).unwrap();
    let n = rows(&mut db, "get log count");
    assert_eq!(n, [[int(MAX_WOKEN as i64)]]);
}

#[test]
fn the_runs_of_a_statement_share_one_row_budget_and_a_call_s_its_own() {
    let mut db = database();
    run(
        &mut db,
        "put accounts {owner: 'ann', balance: 0};
         create statement tally after insert items as begin
           get accounts select id where id = 1 limit 1 require 1;
           set accounts {balance: balance + 1} where id = 1
         end;
         create statement one() public as begin
           get accounts select id where id = 1 limit 1;
           insert items {n: 1}
         end",
    )
    .unwrap();
    // Each run examines two rows: a statement waking one fits two, and
    // one waking two is refused, put back whole.
    db.set_call_max_rows(2);
    run(&mut db, "insert items {n: 1}").unwrap();
    let seq = db.change_seq();
    let e = run(&mut db, "put items [{n: 1}, {n: 2}]").unwrap_err();
    assert!(e.to_string().contains("--call-max-rows"), "{e}");
    assert_eq!(db.change_seq(), seq);
    // Fresh for each statement a client sends, in one block too.
    block(&mut db, "insert items {n: 1}; insert items {n: 1}").unwrap();
    assert_eq!(rows(&mut db, "get accounts select balance"), [[int(3)]]);
    // Inside a call, what the call has left: its own read and the
    // binding's two are three.
    let e = run(&mut db, "call one").unwrap_err();
    assert!(e.to_string().contains("--call-max-rows"), "{e}");
    db.set_call_max_rows(3);
    run(&mut db, "call one").unwrap();
    assert_eq!(rows(&mut db, "get accounts select balance"), [[int(4)]]);
    // And a plain statement after, as ever.
    run(&mut db, "set items {n: 2} where n >= 0").unwrap();
}

#[test]
fn every_run_of_a_write_reads_one_now() {
    let mut db = database();
    // A clock that moves at every reading: the registry's `now` replaced.
    let tick = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(1_800_000_000_000));
    let t = std::sync::Arc::clone(&tick);
    let now = move |_: &[Value]| -> Result<Value> {
        Ok(Value::Timestamp(
            t.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        ))
    };
    db.registry_mut()
        .register_fn("Now", std::sync::Arc::new(now))
        .unwrap();
    run(
        &mut db,
        "insert log {kind: 'w', at: now()}; insert log {kind: 'w', at: now()}",
    )
    .unwrap();
    let at = rows(&mut db, "get log select at");
    assert_ne!(at[0], at[1], "the clock moves");
    run(
        &mut db,
        "del log where n >= 0 or n = null;
         create statement stamped after insert items where new.n >= 0
         as insert log {kind: 's', at: now()}",
    )
    .unwrap();
    run(&mut db, "put items [{n: 1}, {n: 2}, {n: 3}, {n: 4}]").unwrap();
    let at = rows(&mut db, "get log select at where kind = 's'");
    assert_eq!(at.len(), 4);
    assert!(at.iter().all(|r| r == &at[0]), "one time: {at:?}");
}

#[test]
fn a_binding_writes_nothing_it_is_bound_to_and_no_chain_is_made() {
    let mut db = database();
    let e = run(
        &mut db,
        "create statement own after insert items as set items {n: 1} where id = new.id",
    )
    .unwrap_err();
    assert!(
        e.to_string().contains("is bound to `items` and writes it"),
        "{e}"
    );
    // A binding writing a collection another is bound to, in both orders.
    run(
        &mut db,
        "create statement mk after insert items as insert log {kind: 'mk', n: new.n}",
    )
    .unwrap();
    let e = run(
        &mut db,
        "create statement next after insert log as insert ledger {o: new.n}",
    )
    .unwrap_err();
    assert!(e.to_string().contains("which `mk` writes"), "{e}");
    run(&mut db, "drop statement mk").unwrap();
    run(
        &mut db,
        "create statement next after insert log as insert ledger {o: new.n}",
    )
    .unwrap();
    let e = run(
        &mut db,
        "create statement mk after insert items as insert log {kind: 'mk', n: new.n}",
    )
    .unwrap_err();
    assert!(e.to_string().contains("which `next` is bound to"), "{e}");
    // A callable statement's writes wake bindings: it is no binding.
    run(
        &mut db,
        "create statement logs($n int) public as insert log {kind: 'call', n: $n}",
    )
    .unwrap();
    run(&mut db, "call logs(4)").unwrap();
    assert_eq!(rows(&mut db, "get ledger select o"), [[int(4)]]);
}

/// A kind-13 record of op 3, written by hand: a binding another build
/// made, which no definition here would.
fn bound_record(name: &str, text: &str, collection: &str, events: u8, change: u64) -> Vec<u8> {
    let mut body = vec![3u8];
    put_uvarint(&mut body, 0);
    put_uvarint(&mut body, change);
    put_uvarint(&mut body, GRAMMAR_EPOCH as u64);
    let digest = fennec_ql::definition(text).unwrap().digest().unwrap();
    body.extend_from_slice(&digest.to_le_bytes());
    encode_str(&mut body, name);
    put_uvarint(&mut body, 1);
    encode_str(&mut body, collection);
    encode_str(&mut body, collection);
    body.push(events);
    encode_str(&mut body, text);
    let mut record = vec![13u8];
    put_uvarint(&mut record, 0);
    put_uvarint(&mut record, body.len() as u64);
    record.extend_from_slice(&body);
    record
}

/// Depth one: a binding's writes wake nothing, a chain a file holds --
/// another build's -- included.
#[test]
fn a_binding_s_writes_wake_nothing() {
    let mut db = database();
    run(
        &mut db,
        "create statement mk after insert items as insert log {kind: 'mk', n: new.n}",
    )
    .unwrap();
    let mut file = db.snapshot();
    let next = "create statement next after insert log as insert ledger {o: new.n}";
    file.extend(bound_record("next", next, "log", 1, db.change_seq() + 1));
    let mut db = Database::new();
    db.load(&file).unwrap();
    let next = db.statements().get("next").cloned().expect("read back");
    assert!(next.tree.is_some() && next.broken.is_none());
    run(&mut db, "insert items {n: 7}").unwrap();
    assert_eq!(rows(&mut db, "get log select n"), [[int(7)]]);
    assert!(rows(&mut db, "get ledger select o").is_empty(), "depth one");
    // A write of its own wakes it.
    run(&mut db, "insert log {kind: 'own', n: 8}").unwrap();
    assert_eq!(rows(&mut db, "get ledger select o"), [[int(8)]]);
}

#[test]
fn an_unbound_writer_and_a_replica_wake_nothing() {
    let mut db = database();
    run(
        &mut db,
        "create statement mk after insert or set items as insert log {kind: 'mk', n: new.n}",
    )
    .unwrap();
    let image = db.snapshot();
    let from = db.change_seq();
    let records = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    struct Keep(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl Sink for Keep {
        fn append(&mut self, _: &[u8]) -> Result<()> {
            Ok(())
        }
        fn record(&mut self, _: u64, bytes: &[u8]) -> Result<()> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }
        fn rewrite(&mut self, _: &[u8]) -> Result<()> {
            Ok(())
        }
    }
    db.set_sink(Box::new(Keep(records.clone())));
    db.unbound(|db| run(db, "insert items {n: 1}; set items {n: 2} where id = 1"))
        .unwrap();
    db.internally(|db| run(db, "insert items {n: 3}")).unwrap();
    assert!(logged(&mut db).is_empty(), "unbound");
    // Given back as it ends: the next write wakes it.
    run(&mut db, "insert items {n: 4}").unwrap();
    assert_eq!(logged(&mut db).len(), 1);
    // Given back on a panic too.
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        db.unbound(|_| panic!("a writer that panics"))
    }));
    assert!(panicked.is_err());
    run(&mut db, "insert items {n: 5}").unwrap();
    assert_eq!(logged(&mut db).len(), 2);

    // A replica applies the records, derived rows and all, and runs
    // nothing: its log is its primary's.
    let mut replica = Database::new();
    replica.load(&image).unwrap();
    assert_eq!(replica.change_seq(), from);
    replica.apply(&records.lock().unwrap()).unwrap();
    assert_eq!(logged(&mut replica), logged(&mut db));
    assert_eq!(replica.change_seq(), db.change_seq());
}

/// A hook refusing a binding -- where a policy meets one -- refuses the
/// write before the binding runs, its block put back. A write whose rows
/// reach a binding by its event and pass none of its condition asks it
/// with no rows, so a policy may judge a writer whatever a condition
/// reading what the writer is not shown answered; one reaching none is
/// asked nothing.
#[test]
fn a_hook_refusing_a_wake_puts_the_write_back() {
    static UNPASSED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    struct NoWake;
    impl Hook for NoWake {
        fn name(&self) -> &str {
            "no-wake"
        }
        fn rewrites(&self) -> bool {
            false
        }
        fn waking(&self, binding: &Definition, rows: usize) -> Result<()> {
            if rows == 0 {
                UNPASSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return Ok(());
            }
            let (name, n) = (&binding.name, binding.body.len());
            Err(Error::Denied(format!("`{name}` of {n} for {rows} rows")))
        }
    }
    impl Plugin for NoWake {
        fn name(&self) -> &str {
            "no-wake"
        }
        fn init(&self, reg: &mut Registry) -> Result<()> {
            reg.register_hook(std::sync::Arc::new(NoWake));
            Ok(())
        }
    }
    let mut db = database();
    run(
        &mut db,
        "create statement mk after insert items where new.n > 0
         as insert log {kind: 'mk', n: new.n}",
    )
    .unwrap();
    db.install_plugin(&NoWake).unwrap();
    let seq = db.change_seq();
    let e = run(&mut db, "put items [{n: 1}, {n: 0}, {n: 2}]").unwrap_err();
    assert_eq!(e, Error::Denied("`mk` of 1 for 2 rows".into()));
    assert_eq!(db.woken(), Some("mk"));
    assert_eq!(db.change_seq(), seq);
    // A row reaching `mk` and passing no condition: asked with none.
    let unpassed = || UNPASSED.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(unpassed(), 0);
    run(&mut db, "put items {n: 0}").unwrap();
    assert_eq!(unpassed(), 1);
    // A write of another event reaches no binding, and is asked nothing.
    run(&mut db, "del items where n = 0").unwrap();
    assert_eq!(unpassed(), 1);
}

/// A hook judging its writer (`Hook::writer_only`, a scoped token's rules)
/// is passed over by a binding's writes -- its text's authority, never its
/// writer's -- and meets the writer's own, before the runs and after
/// them; every other hook meets both.
#[test]
fn a_hook_judging_its_writer_is_passed_over_by_a_binding_s_writes() {
    struct Refuse {
        writer_only: bool,
    }
    impl Hook for Refuse {
        fn name(&self) -> &str {
            match self.writer_only {
                true => "writer",
                false => "every",
            }
        }
        fn rewrites(&self) -> bool {
            false
        }
        fn writer_only(&self) -> bool {
            self.writer_only
        }
        fn before_write(&self, schema: &Schema, _op: WriteOp, _doc: &mut Document) -> Result<()> {
            match schema.name == "log" {
                true => Err(Error::Denied(format!("{} refuses log", Hook::name(self)))),
                false => Ok(()),
            }
        }
    }
    impl Plugin for Refuse {
        fn name(&self) -> &str {
            Hook::name(self)
        }
        fn init(&self, reg: &mut Registry) -> Result<()> {
            let writer_only = self.writer_only;
            reg.register_hook(std::sync::Arc::new(Refuse { writer_only }));
            Ok(())
        }
    }
    let mut db = database();
    run(
        &mut db,
        "create statement mk after insert items where new.n > 0
         as insert log {kind: 'mk', n: new.n}",
    )
    .unwrap();
    db.install_plugin(&Refuse { writer_only: true }).unwrap();
    // The writer's own write to `log` meets it.
    let e = run(&mut db, "insert log {kind: 'own', n: 1}").unwrap_err();
    assert_eq!(e, Error::Denied("writer refuses log".into()));
    // The binding's does not.
    run(&mut db, "put items [{n: 1}, {n: 2}]").unwrap();
    assert_eq!(rows(&mut db, "get log where kind = 'mk'").len(), 2);
    // And it is in place again after the runs.
    let e = run(&mut db, "insert log {kind: 'own', n: 3}").unwrap_err();
    assert_eq!(e, Error::Denied("writer refuses log".into()));
    // A hook judging what is written meets the binding's writes: the
    // write is put back whole.
    db.install_plugin(&Refuse { writer_only: false }).unwrap();
    let seq = db.change_seq();
    let e = run(&mut db, "put items {n: 4}").unwrap_err();
    assert_eq!(e, Error::Denied("every refuses log".into()));
    assert_eq!(db.woken(), Some("mk"));
    assert_eq!(db.change_seq(), seq);
}

/// What a collection no statement is bound to pays: no row asked, none
/// kept, over every write form, while another collection is bound.
#[test]
fn a_write_to_an_unbound_collection_keeps_no_row() {
    let mut db = database();
    run(&mut db, PAID).unwrap();
    run(
        &mut db,
        "create statement mk after insert or set or del items as insert log {kind: 'mk'}",
    )
    .unwrap();
    let before = Database::bound_counts();
    for w in [
        "insert other {n: 1, k: 'a'}",
        "put other {n: 2, k: 'b'}",
        "put other {id: 1, n: 3, k: 'a'}",
        "put other {n: 4, k: 'a'} if absent else set {n: n + new.n}",
        "insert other {n: 5, k: 'c'} if absent returning id",
        "set other {n: 6} where id = 2",
        "set other {n: n + 1} where n > 0 order n limit 1 returning id",
        "del other where id = 2",
        "del other where n > 0 order n limit 1 returning id",
        "put other [{n: 1, k: 'x'}, {n: 2, k: 'y'}]",
        "set other {n: 0} where n >= 0",
        "del other where n >= 0",
    ] {
        run(&mut db, w).unwrap_or_else(|e| panic!("{w}: {e}"));
    }
    assert_eq!(Database::bound_counts(), before, "nothing asked or kept");
    // And the bound collection's statements, against it.
    run(&mut db, "insert items {n: 1}").unwrap();
    assert_eq!(Database::bound_counts(), (before.0 + 1, before.1 + 1));
}

/// A `set` over many rows asks each its condition and keeps the few that
/// pass, never a pair a row.
#[test]
fn a_set_over_many_rows_keeps_the_few_that_pass() {
    let mut db = database();
    let docs = (0..20_000)
        .map(|i| format!("{{status: 'open', total: {i}, account: 1}}"))
        .collect::<Vec<_>>()
        .join(", ");
    run(&mut db, &format!("put orders [{docs}]")).unwrap();
    run(&mut db, "put accounts {owner: 'ann', balance: 0}").unwrap();
    run(&mut db, PAID).unwrap();
    let (asked, kept) = Database::bound_counts();
    run(
        &mut db,
        "set orders {status: case when total < 5 then 'paid' else 'open' end} where total >= 0",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let (asked2, kept2) = Database::bound_counts();
    assert_eq!((asked2 - asked, kept2 - kept), (20_000, 5));
    assert_eq!(rows(&mut db, "get ledger count"), [[int(5)]]);
    assert_eq!(rows(&mut db, "get accounts select balance"), [[int(-10)]]);
}

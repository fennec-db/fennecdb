//! Named statements called (`docs/design/named-statements.md`, slice 3):
//! a call is one block, judged statement, claims, arguments, `check` and
//! whether it can run, each before the next; its answer is the last
//! statement's; `--call-max-rows` puts back a call that reached for more;
//! and a definition is held to the fields it names and, where it writes,
//! to an index narrowing each statement or its `scans`.

use fennec_core::prelude::*;

const SCHEMA: &str = "create collection accounts (owner text @hash, balance int);
create collection entries (src int, dst int, amount int, by text)";

const TRANSFER: &str = "create statement transfer($from int, $to int, $amount int)
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
end";

fn run(db: &mut Database, text: &str) -> Result<Response> {
    let mut last = Response::Affected(0);
    for s in fennec_ql::parse(text).expect("parse") {
        last = db.execute(&s)?;
    }
    Ok(last)
}

/// A ledger: alice holds 1 with 100, bob 2 with 50, and `transfer`.
fn ledger() -> Database {
    fennec_ql::install();
    let mut db = Database::new();
    run(&mut db, SCHEMA).unwrap();
    run(
        &mut db,
        "put accounts {owner: 'alice', balance: 100}; put accounts {owner: 'bob', balance: 50}",
    )
    .unwrap();
    run(&mut db, TRANSFER).unwrap();
    db
}

fn balances(db: &Database) -> Vec<Value> {
    let s = fennec_ql::parse_one("get accounts select balance order id").unwrap();
    let r = db.query(&s, &[]).unwrap();
    r.rows()
        .unwrap()
        .rows
        .iter()
        .map(|r| r.values[0].clone())
        .collect()
}

fn entries(db: &Database) -> usize {
    let s = fennec_ql::parse_one("get entries select id").unwrap();
    db.query(&s, &[]).unwrap().rows().unwrap().rows.len()
}

#[test]
fn a_call_is_one_block_answering_its_last_statement() {
    let mut db = ledger();
    let before = db.change_seq();
    let r = run(
        &mut db,
        "call transfer {from: 1, to: 2, amount: 30} as {sub: 'alice'}",
    )
    .unwrap();
    let rows = r.rows().expect("the insert's returning");
    assert_eq!(rows.columns, ["id", "amount"]);
    assert_eq!(rows.rows[0].values[1], Value::Int(30));
    assert_eq!(balances(&db), [Value::Int(70), Value::Int(80)]);
    // Three writes, one block: the counter moves by its writes, at once.
    assert_eq!(db.change_seq(), before + 3);
    // SQL's `CALL` by place, and a parameter's object, are the same call.
    run(
        &mut db,
        "CALL transfer(1, 2, amount => 5) as {sub: 'alice'}",
    )
    .unwrap();
    let by_param = fennec_ql::parse_one("call transfer $1 as $2").unwrap();
    let args = fennec_core::json::parse_json(r#"{"from": 1, "to": 2, "amount": 5}"#).unwrap();
    let claims = fennec_core::json::parse_json(r#"{"sub": "alice"}"#).unwrap();
    db.execute_with(&by_param, &[args, claims]).unwrap();
    assert_eq!(balances(&db), [Value::Int(60), Value::Int(90)]);
    assert_eq!(entries(&db), 3);
}

#[test]
fn a_guard_puts_the_whole_call_back_with_its_reason() {
    let mut db = ledger();
    let before = db.change_seq();
    for (call, reason) in [
        (
            "call transfer {from: 1, to: 2, amount: 500} as {sub: 'alice'}",
            "insufficient funds",
        ),
        (
            "call transfer {from: 1, to: 9, amount: 5} as {sub: 'alice'}",
            "no such account",
        ),
        (
            "call transfer {from: 1, to: 2, amount: 5} as {sub: 'bob'}",
            "not your account",
        ),
    ] {
        let e = run(&mut db, call).unwrap_err();
        assert!(matches!(e, Error::Unmet(_)), "{call}: {e}");
        assert_eq!(e.reason(), Some(reason), "{call}");
    }
    assert_eq!(balances(&db), [Value::Int(100), Value::Int(50)]);
    assert_eq!((entries(&db), db.change_seq()), (0, before));
}

/// A claim missing, `null`, `""`, an empty list or of another type is one
/// the caller lacks: 403, nothing run.
#[test]
fn a_claim_null_empty_or_mistyped_is_missing_and_nothing_runs() {
    let mut db = ledger();
    let before = db.change_seq();
    for claims in [
        "",
        " as {}",
        " as {sub: null}",
        " as {sub: ''}",
        " as {sub: []}",
        " as {sub: 7}",
        " as {sub: ['alice']}",
        " as {sub: {name: 'alice'}}",
        " as {name: 'alice'}",
    ] {
        let text = format!("call transfer {{from: 1, to: 2, amount: 5}}{claims}");
        let e = run(&mut db, &text).unwrap_err();
        assert!(
            matches!(&e, Error::Denied(m) if m.contains("`sub`")),
            "{text}: {e}"
        );
    }
    assert_eq!(balances(&db), [Value::Int(100), Value::Int(50)]);
    assert_eq!(db.change_seq(), before);
}

/// Each argument is read by its parameter's type: one missing, extra,
/// given twice, null or of another type is a query error naming it.
#[test]
fn the_arguments_are_held_to_the_signature() {
    let mut db = ledger();
    let before = db.change_seq();
    for (args, names) in [
        ("{from: 1, to: 2}", "`amount`"),
        ("{from: 1, to: 2, amount: 5, memo: 'x'}", "`memo`"),
        ("{from: 1, to: 2, amount: null}", "`amount`"),
        ("{from: 1, to: 2, amount: 'five'}", "`amount`"),
        ("{from: 1, to: 2, amount: 5.5}", "`amount`"),
    ] {
        let text = format!("call transfer {args} as {{sub: 'alice'}}");
        let e = run(&mut db, &text).unwrap_err();
        assert!(
            matches!(&e, Error::Query(m) if m.contains(names)),
            "{text}: {e}"
        );
    }
    let e = run(&mut db, "CALL transfer(1, 2, 3, 4) as {sub: 'alice'}").unwrap_err();
    assert!(e.to_string().contains("takes 3 arguments"), "{e}");
    let e = run(&mut db, "CALL transfer(1, 2, from => 3) as {sub: 'alice'}").unwrap_err();
    assert!(e.to_string().contains("twice"), "{e}");
    // `check`, last, over values already typed.
    let e = run(
        &mut db,
        "call transfer {from: 1, to: 2, amount: 0} as {sub: 'alice'}",
    )
    .unwrap_err();
    assert!(
        matches!(&e, Error::Query(m) if m.contains("`check`")),
        "{e}"
    );
    // The claims before the arguments: a caller that may never run it is
    // told nothing of its signature.
    let e = run(&mut db, "call transfer {memo: 'x'}").unwrap_err();
    assert!(matches!(e, Error::Denied(_)), "{e}");
    assert_eq!(db.change_seq(), before);
    // A statement no database holds.
    assert_eq!(
        run(&mut db, "call nothing").unwrap_err(),
        Error::NotFound("statement `nothing`".into())
    );
}

/// A call examines at most `--call-max-rows` rows: past it the block is
/// put back and the call refused naming the bound, never answered from
/// the rows it reached.
#[test]
fn a_call_past_its_rows_is_put_back() {
    let mut db = ledger();
    run(
        &mut db,
        "create statement interest() public scans as
           set accounts {balance: balance + 1} where balance >= 0",
    )
    .unwrap();
    db.set_call_max_rows(1);
    let before = db.change_seq();
    let e = run(&mut db, "call interest").unwrap_err();
    assert!(e.to_string().contains("--call-max-rows"), "{e}");
    assert_eq!(balances(&db), [Value::Int(100), Value::Int(50)]);
    assert_eq!(db.change_seq(), before);
    // A transfer reads three rows, one by each of its guards and writes.
    let transfer = "call transfer {from: 1, to: 2, amount: 5} as {sub: 'alice'}";
    assert!(run(&mut db, transfer).is_err());
    db.set_call_max_rows(3);
    run(&mut db, transfer).unwrap();
    run(&mut db, "call interest").unwrap();
    assert_eq!(balances(&db), [Value::Int(96), Value::Int(56)]);
    // The bound is a call's alone: a statement sent alone scans as ever.
    run(
        &mut db,
        "set accounts {balance: balance - 1} where balance >= 0",
    )
    .unwrap();
}

/// A definition names fields its collections hold, and a writing body
/// reads each collection through an index -- or says `scans`.
#[test]
fn a_definition_is_held_to_its_fields_and_its_plan() {
    let mut db = ledger();
    let e = run(
        &mut db,
        "create statement s() public as set accounts {balanse: 0} where id = 1",
    )
    .unwrap_err();
    assert!(e.to_string().contains("holds no field `balanse`"), "{e}");
    let e = run(
        &mut db,
        "create statement s($b int) public as del accounts where balance = $b",
    )
    .unwrap_err();
    assert!(e.to_string().contains("`scans`"), "{e}");
    for ok in [
        "create statement s1($b int) public scans as del accounts where balance = $b",
        "create statement s2($o text) public as del accounts where owner = $o",
        "create statement s3($b int) as get accounts select id where balance = $b",
        "create statement s4($i int) public as del accounts where id = $i",
    ] {
        run(&mut db, ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
    let list = run(&mut db, "statements").unwrap();
    let names: Vec<_> = list
        .rows()
        .unwrap()
        .rows
        .iter()
        .map(|r| r.values[0].clone())
        .collect();
    assert_eq!(names.len(), 5);
    let d = run(&mut db, "describe statement transfer").unwrap();
    let rs = d.rows().unwrap();
    let at = |c: &str| rs.columns.iter().position(|x| x == c).unwrap();
    let row = &rs.rows[0].values;
    assert_eq!(row[at("writes")], Value::Bool(true));
    assert_eq!(
        row[at("signature")],
        Value::Text("($from int, $to int, $amount int) claims (sub text)".into())
    );
    assert_eq!(row[at("broken")], Value::Null);
}

/// `near`, `match` and a `lookup`'s children are charged to a call's rows
/// as a scan's are: none is a way around `--call-max-rows`, each past it
/// a query error naming the bound, never a cut answer.
#[test]
fn near_match_and_a_lookups_children_count_against_a_calls_rows() {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection docs (body text @text, v vector<2> @hnsw, owner int @hash);
         create collection owners (name text)",
    )
    .unwrap();
    run(&mut db, "put owners {name: 'a'}").unwrap();
    for i in 0..4 {
        run(
            &mut db,
            &format!("put docs {{body: 'word', v: [1.0, {i}.5], owner: 1}}"),
        )
        .unwrap();
    }
    run(
        &mut db,
        "create statement close() public as get docs select id near v [1.0, 0.0] limit 4;
         create statement worded() public as get docs select id match body 'word' limit 4;
         create statement theirs() public as
           get owners select name lookup docs on owner = id select id",
    )
    .unwrap();
    db.set_call_max_rows(2);
    for name in ["close", "worded", "theirs"] {
        let e = run(&mut db, &format!("call {name}")).unwrap_err();
        assert!(e.to_string().contains("--call-max-rows"), "{name}: {e}");
    }
    db.set_call_max_rows(5);
    for name in ["close", "worded", "theirs"] {
        run(&mut db, &format!("call {name}")).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

/// A list claim is held to the bound a policy holds one to: refused past
/// it, never run with the list cut.
#[test]
fn a_list_claim_past_its_bound_is_refused() {
    let mut db = ledger();
    run(
        &mut db,
        "create statement mine() claims (accounts [int]) as
           get accounts select id order id",
    )
    .unwrap();
    let list = |n: usize| {
        let ids: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
        format!("call mine as {{accounts: [{}]}}", ids.join(", "))
    };
    let e = run(&mut db, &list(fennec_core::named::MAX_CLAIM_VALUES + 1)).unwrap_err();
    assert!(
        matches!(&e, Error::Denied(m) if m.contains("at most 1000 values")),
        "{e}"
    );
    let r = run(&mut db, &list(fennec_core::named::MAX_CLAIM_VALUES)).unwrap();
    assert_eq!(r.rows().unwrap().rows.len(), 2);
}

/// Panics at a document whose `k` is `boom`.
struct Boom;
impl Hook for Boom {
    fn name(&self) -> &str {
        "boom"
    }
    fn rewrites(&self) -> bool {
        false
    }
    fn before_write(&self, _: &Schema, _: WriteOp, doc: &mut Document) -> Result<()> {
        if let Some(Value::Text(t)) = doc.get("k") {
            assert_ne!(t, "boom", "a hook that panics");
        }
        Ok(())
    }
}
impl Plugin for Boom {
    fn name(&self) -> &str {
        "boom"
    }
    fn init(&self, reg: &mut Registry) -> Result<()> {
        reg.register_hook(std::sync::Arc::new(Boom));
        Ok(())
    }
}

/// A call's row bound is the call's alone, a panic unwinding out of its
/// body included: the next plain statement scans as ever, as a server's
/// next request on another connection does.
#[test]
fn a_panic_in_a_call_leaves_no_bound_behind() {
    fennec_ql::install();
    let mut db = Database::new();
    run(&mut db, "create collection ev (k text)").unwrap();
    for _ in 0..3 {
        run(&mut db, "put ev {k: 'calm'}").unwrap();
    }
    run(
        &mut db,
        "create statement boom() public as insert ev {k: 'boom'}",
    )
    .unwrap();
    db.install_plugin(&Boom).unwrap();
    db.set_call_max_rows(1);
    let call = fennec_ql::parse_one("call boom").unwrap();
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = db.execute(&call);
    }));
    assert!(unwound.is_err());
    db.rollback();
    let all = fennec_ql::parse_one("get ev select k").unwrap();
    let rows = db.query(&all, &[]).expect("a plain get, no call's bound");
    assert_eq!(rows.rows().unwrap().rows.len(), 3);
    run(&mut db, "set ev {k: 'still'} where k = 'calm'").unwrap();
}

/// A statement whose text reads as another one here is judged as a call is
/// -- its claims, its arguments, `check` -- before it is refused as broken
/// (409): a caller lacking its claims is told nothing of it.
#[test]
fn a_misread_statement_is_refused_in_the_order_of_every_call() {
    use fennec_core::codec::{encode_str, put_uvarint};
    let db = ledger();
    let mut file = db.snapshot();
    let text = "create statement odd($n int) claims (sub text) check $n > 0 as
      get accounts select id where id = $n";
    let digest = fennec_ql::definition(text).unwrap().digest().unwrap();
    let mut body = vec![1u8];
    put_uvarint(&mut body, 0);
    put_uvarint(&mut body, db.change_seq() + 1);
    put_uvarint(&mut body, fennec_core::named::GRAMMAR_EPOCH as u64);
    body.extend_from_slice(&(digest ^ 1).to_le_bytes());
    encode_str(&mut body, "odd");
    put_uvarint(&mut body, 1);
    encode_str(&mut body, "accounts");
    encode_str(&mut body, text);
    file.push(13);
    put_uvarint(&mut file, 0);
    put_uvarint(&mut file, body.len() as u64);
    file.extend_from_slice(&body);
    let mut db = Database::new();
    db.load(&file).unwrap();
    let e = run(&mut db, "call odd {n: 1}").unwrap_err();
    assert!(matches!(&e, Error::Denied(m) if m.contains("`sub`")), "{e}");
    let e = run(&mut db, "call odd {n: 1, m: 2} as {sub: 'alice'}").unwrap_err();
    assert!(matches!(&e, Error::Query(m) if m.contains("`m`")), "{e}");
    let e = run(&mut db, "call odd {n: 0} as {sub: 'alice'}").unwrap_err();
    assert!(
        matches!(&e, Error::Query(m) if m.contains("`check`")),
        "{e}"
    );
    let e = run(&mut db, "call odd {n: 1} as {sub: 'alice'}").unwrap_err();
    assert!(
        matches!(&e, Error::Exists(m) if m.contains("reads as another statement")),
        "{e}"
    );
}

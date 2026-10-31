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

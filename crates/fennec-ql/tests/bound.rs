//! Write-bound statements' syntax (`docs/design/named-statements.md`,
//! *F2 in slices*, slice 1): `create statement <name> after <event> [or
//! <event> ...] <collection> [where ...] as <body>` read into its tree,
//! `new.` and `old.` held to the events and numbered as places, the
//! refusals the text alone answers, the binding in the canonical tree --
//! and every way of running one refusing it until the engine runs them, so
//! no write lands without the rows its binding would write.

use fennec_core::named::{Binding, Definition, Place, BOUND_NOT_BUILT};
use fennec_core::prelude::*;
use fennec_core::query::ArithOp;
use fennec_ql::{parse, parse_one};

/// The design's ledger line: a binding's every part.
const PAID: &str = "create statement paid_ledger
  after set orders where new.status = 'paid' and old.status = 'open'
  comment 'A ledger line and the debit for each order as it is paid'
as begin
  get accounts select id where id = new.account limit 1 require 1 else 'no such account';
  insert ledger {order: new.id, account: new.account, amount: new.total, at: now()};
  set accounts {balance: balance - new.total} where id = new.account require 1
end";

fn definition(text: &str) -> Definition {
    match parse_one(text) {
        Ok(Statement::CreateStatement(d)) => *d,
        other => panic!("{text}: {other:?}"),
    }
}

fn binding(text: &str) -> Binding {
    *definition(text).after.expect("a binding")
}

/// `text` refused in words holding `words`, placed at the first `at` in it,
/// in characters as the parser counts: the word refused is not consumed.
fn refused_at(text: &str, words: &str, at: &str) {
    let e = match parse(text) {
        Err(e) => e.to_string(),
        Ok(s) => panic!("{text} parsed: {s:?}"),
    };
    assert!(e.contains(words), "{text}: {e}");
    let place = text[..text.find(at).expect("the word is in the text")]
        .chars()
        .count();
    assert!(
        e.contains(&format!("position {place}:")),
        "{text}: {e}, not at {place}"
    );
}

fn place(old: bool, field: &str) -> Place {
    Place {
        old,
        field: field.into(),
    }
}

fn field(f: &str) -> Box<Expr> {
    Box::new(Expr::Field(f.into()))
}

fn text(t: &str) -> Box<Expr> {
    Box::new(Expr::Lit(Value::Text(t.into())))
}

#[test]
fn a_binding_reads_into_its_tree() {
    let d = definition(PAID);
    assert_eq!(d.name, "paid_ledger");
    assert!(d.params.is_empty() && d.claims.is_empty() && d.check.is_none() && !d.public);
    assert!(d.block && !d.scans && d.body.len() == 3);
    assert_eq!(
        d.comment.as_deref(),
        Some("A ledger line and the debit for each order as it is paid")
    );
    let b = d.after.as_deref().unwrap();
    assert_eq!((b.collection.as_str(), b.events), ("orders", Binding::SET));
    // The condition keeps the row's fields as written, for the engine to
    // bind to the collection as a read's filter is.
    assert_eq!(
        b.condition,
        Some(Expr::And(
            Box::new(Expr::Cmp(CmpOp::Eq, field("new.status"), text("paid"))),
            Box::new(Expr::Cmp(CmpOp::Eq, field("old.status"), text("open"))),
        ))
    );
    // The body's pair is places, in the order each first appears: the
    // statements every path of the engine takes.
    assert_eq!(
        b.places,
        [
            place(false, "account"),
            place(false, "id"),
            place(false, "total")
        ]
    );
    let Statement::Put { docs, .. } = &d.body[1] else {
        panic!("{:?}", d.body[1])
    };
    let doc: Vec<(&str, &Expr)> = docs[0].iter().map(|(k, e)| (k.as_str(), e)).collect();
    assert_eq!(doc[0], ("order", &Expr::Param(1)));
    assert_eq!(doc[1], ("account", &Expr::Param(0)));
    assert_eq!(doc[2], ("amount", &Expr::Param(2)));
    let Statement::Update { set, filter, .. } = &d.body[2] else {
        panic!("{:?}", d.body[2])
    };
    assert_eq!(
        set[0].1,
        Expr::Arith(ArithOp::Sub, field("balance"), Box::new(Expr::Param(2)))
    );
    assert_eq!(
        filter,
        &Some(Expr::Cmp(CmpOp::Eq, field("id"), Box::new(Expr::Param(0))))
    );
    // A guard's reason stays the text's.
    let Statement::Select(guard) = &d.body[0] else {
        panic!()
    };
    assert_eq!(&*guard.require.as_ref().unwrap().reason, "no such account");
}

/// Each event and each combination, in any order and SQL's spellings:
/// the bits a record will write, and `or` between them.
#[test]
fn each_event_and_combination_binds() {
    let (i, s, d) = (Binding::INSERT, Binding::SET, Binding::DEL);
    for (events, want) in [
        ("insert", i),
        ("set", s),
        ("del", d),
        ("update", s),
        ("delete", d),
        ("INSERT", i),
        ("Update", s),
        ("insert or set", i | s),
        ("set or insert", i | s),
        ("update or delete", s | d),
        ("insert or del", i | d),
        ("del or set or insert", i | s | d),
    ] {
        let text = format!("create statement c after {events} likes as insert log {{at: now()}}");
        let b = binding(&text);
        assert_eq!((b.events, b.collection.as_str()), (want, "likes"), "{text}");
        assert_eq!(b.condition, None, "{text}");
        assert!(b.places.is_empty(), "{text}");
    }
    assert_eq!(Binding::events_named(i | s | d), "insert or set or del");
    // A condition, `scans` and `comment` in either order, and `create or
    // replace`.
    let d = definition(
        "create or replace statement c after set likes where new.n > old.n scans comment \
         'counts' as set posts {likes: likes + 1} where id = new.post",
    );
    assert!(d.replace && d.scans && d.comment.as_deref() == Some("counts") && !d.block);
    assert_eq!(d.after.unwrap().places, [place(false, "post")]);
}

/// `new.` is the row as the write leaves it and `old.` as it stood: a
/// binding reads only what every one of its events has, in its condition
/// and its body, and is refused at the word otherwise -- never read as
/// `null` for the event that lacks it.
#[test]
fn new_and_old_are_held_to_every_event() {
    for (events, new, old) in [
        ("insert", true, false),
        ("set", true, true),
        ("del", false, true),
        ("insert or set", true, false),
        ("set or del", false, true),
        ("insert or del", false, false),
        ("insert or set or del", false, false),
    ] {
        for (side, reads) in [("new", new), ("old", old)] {
            let cond = format!(
                "create statement c after {events} t where {side}.a = 1 as del u where id = 1"
            );
            let body = format!("create statement c after {events} t as del u where id = {side}.a");
            let got = [parse(&cond).is_ok(), parse(&body).is_ok()];
            assert_eq!(got, [reads, reads], "{events} {side}");
            if !reads {
                let at = format!("{side}.a");
                let why = match side {
                    "new" => "a row taken out leaves none",
                    _ => "a row made has no row before it",
                };
                refused_at(&cond, why, &at);
                refused_at(&body, why, &at);
            }
        }
    }
    refused_at(
        "create statement c after insert or set t as insert u {a: new.a, b: old.b}",
        "this statement is bound after `insert or set` -- `old.` is read where every event is \
         `set` or `del`",
        "old.b",
    );
    // Each field once, wherever it is read again; `new.` and `old.` of one
    // field two places; a path into a json field a place of its own.
    let b = binding(
        "create statement c after set t as begin insert u {a: new.a, was: old.a}; set v {n: n \
         + new.a, m: new.meta.source} where id = old.id end",
    );
    assert_eq!(
        b.places,
        [
            place(false, "a"),
            place(true, "a"),
            place(false, "meta.source"),
            place(true, "id")
        ]
    );
}

/// What the text alone refuses of a binding, each at the word it is about.
#[test]
fn a_binding_s_refusals() {
    let body = "as insert log {at: now()}";
    // A bare name in the condition has no row to be read from.
    refused_at(
        &format!("create statement c after set t where status = 'paid' {body}"),
        "a bare name has no row to be read from",
        "status",
    );
    refused_at(
        &format!("create statement c after set t where new.a = 1 and b = 2 {body}"),
        "`b`: a bound statement's condition reads",
        "b = 2",
    );
    // Its condition reads the row alone: a read elsewhere is a guard's.
    for (cond, why, at) in [
        (
            "new.a in (get u select a)",
            "a read of another collection is its body's guard",
            "get u",
        ),
        ("count(new.a) > 1", "folds none", "count"),
        ("expired()", "no binding sees a row expire", "expired"),
    ] {
        refused_at(
            &format!("create statement c after set t where {cond} {body}"),
            why,
            at,
        );
    }
    // No parameters, no claims, no parentheses, no `check`, no `public`.
    let values = "a bound statement takes no parameters and reads no claim";
    refused_at(
        &format!("create statement c() after set t {body}"),
        values,
        "()",
    );
    refused_at(
        &format!("create statement c($a int) claims (sub text) after set t {body}"),
        values,
        "(",
    );
    for marker in ["claims (sub text)", "check 1 = 1", "public"] {
        refused_at(
            &format!("create statement c after set t {marker} {body}"),
            values,
            marker,
        );
    }
    for at in ["$1", "$a", "$jwt.sub"] {
        refused_at(
            &format!("create statement c after set t where new.a = {at} {body}"),
            values,
            at,
        );
        refused_at(
            &format!("create statement c after set t as del u where id = {at}"),
            values,
            at,
        );
    }
    // Its rows go nowhere: a `get` is a guard, and nothing answers.
    refused_at(
        "create statement c after set t as begin get u select id where id = new.u; del v where \
         id = new.id end",
        "its `get` is a guard",
        "get u",
    );
    refused_at(
        "create statement c after set t as insert u {a: new.a} returning id",
        "its writes say nothing with `returning`",
        "insert u",
    );
    refused_at(
        "create statement c after del t as del u where id = old.u returning *",
        "with `returning`",
        "del u",
    );
    // A pair's field is a value, never a name: a field it sets, an item it
    // answers under no name.
    refused_at(
        "create statement c after set t as update u set new.a = 1 where id = new.id",
        "names no field here",
        "new.a = 1",
    );
    refused_at(
        "create statement c after set t as begin get u select new.a where id = 1 require 1; del \
         v where id = 1 end",
        "answers under a name",
        "new.a where",
    );
    // A callable body's rules hold too.
    refused_at(
        "create statement c after set t as put u {id: new.id}",
        "a `put` naming an id",
        "put u",
    );
    refused_at(
        "create statement c after set t as del u",
        "say which rows they write",
        "del u",
    );
    refused_at(
        "create statement c after set t as insert _audit {a: new.a}",
        "is the database's own",
        "insert _audit",
    );
    // No collection of the database's own is bound.
    refused_at(
        &format!("create statement c after insert _idempotency {body}"),
        "no statement is bound to a collection whose name starts with `_`",
        "_idempotency",
    );
    // The events: each once, each a row's.
    refused_at(
        &format!("create statement c after set or update t {body}"),
        "`set` is named twice",
        "update",
    );
    refused_at(
        &format!("create statement c after truncate t {body}"),
        "`insert`, `set` or `del`",
        "truncate",
    );
    // SQL's trigger words, unconsumed, naming the form to write.
    refused_at(
        &format!("create statement c after update on orders {body}"),
        "with no `on`",
        "on orders",
    );
    refused_at(
        &format!("create statement c after set t when new.a = 1 {body}"),
        "its `where`",
        "when",
    );
    refused_at(
        &format!("create statement c before set t {body}"),
        "runs after the write that wakes it",
        "before",
    );
    refused_at(
        &format!("create statement c() before set t {body}"),
        "runs after the write that wakes it",
        "before",
    );
    refused_at(
        "CREATE TRIGGER paid AFTER UPDATE ON orders FOR EACH ROW EXECUTE FUNCTION f()",
        "`create statement <name> after insert|set|del <collection>",
        "TRIGGER",
    );
    refused_at(
        "drop trigger paid on orders",
        "`drop statement <name>`",
        "trigger",
    );
    // Outside a binding `new.` and `old.` are what they were: a path into
    // a field of that name, and an upsert's document.
    let d = definition("create statement s() public as insert t {a: new.a}");
    let Statement::Put { docs, .. } = &d.body[0] else {
        panic!()
    };
    assert_eq!(docs[0][0].1, Expr::Field("new.a".into()));
    assert!(d.after.is_none());
    assert!(parse("get t where old.a = 1").is_ok());
}

/// The binding is the definition's: the canonical tree writes it where
/// one is (tag 9), and only there, so no callable statement's digest
/// moves; every spelling of one binding is one digest, and every change of
/// its meaning another.
#[test]
fn a_binding_is_in_the_canonical_tree() {
    let digest = |t: &str| definition(t).digest().unwrap();
    let base = digest(PAID);
    let spelled = "CREATE OR REPLACE STATEMENT paid_ledger AFTER UPDATE orders -- paid
  WHERE (new.status = 'paid') AND old.status = 'open'
  COMMENT \"A ledger line and the debit for each order as it is paid\"
AS BEGIN
  SELECT id FROM accounts WHERE id = new.account LIMIT 1 REQUIRE 1 ELSE 'no such account';
  INSERT INTO ledger (order, account, amount, at) VALUES (new.id, new.account, new.total, now());;
  UPDATE accounts SET balance = balance - new.total WHERE id = new.account REQUIRE 1;
END";
    assert_eq!(digest(spelled), base);
    assert_eq!(
        digest("create statement c after insert or set t as insert u {a: new.a}"),
        digest("create statement c after set or insert t as begin insert u {a: new.a}; end")
    );
    let mut seen = vec![base];
    for (from, to) in [
        ("paid_ledger", "paid_lines"),
        ("set orders", "set order_lines"),
        ("new.status = 'paid' and ", ""),
        ("old.status = 'open'", "new.status = 'open'"),
        ("'open'", "'opened'"),
        ("debit for", "debits for"),
        ("amount: new.total", "amount: new.account"),
        (
            "{order: new.id, account: new.account",
            "{order: new.account, account: new.id",
        ),
        ("insert ledger", "insert ledgers"),
    ] {
        assert!(PAID.contains(from), "{from}");
        let d = digest(&PAID.replacen(from, to, 1));
        assert!(!seen.contains(&d), "{from} -> {to}");
        seen.push(d);
    }
    // Each set of events its own.
    for events in [
        "insert",
        "set",
        "del",
        "insert or set",
        "insert or del",
        "set or del",
    ] {
        let d = digest(&format!(
            "create statement paid_ledger after {events} orders as insert log {{at: now()}}"
        ));
        assert!(!seen.contains(&d), "{events}");
        seen.push(d);
    }
    // A text read back is its tree, as an open reads a stored one.
    let d = definition(PAID);
    assert_eq!(d.text, PAID);
    assert_eq!(fennec_ql::definition(&d.text).unwrap(), d);
    // Written as tag 9 after the body, and nowhere for a callable one:
    // `Debug`, which `make fennecql-corpus` compares, says it alike.
    let bytes = definition("create statement s after del t as del u where id = old.u")
        .canonical()
        .unwrap();
    // The binding: collection `t`, events 4 (`del`), no condition, one
    // place, `old.u`; then the definition's end.
    let tail = [9, 4, 1, 1, b't', 2, 4, 4, 1, 5, 1, 2, 1, b'u', 0, 0, 0];
    assert!(bytes.ends_with(&tail), "{bytes:?}");
    let callable = definition("create statement s($a int) public as del u where id = $a");
    assert!(!format!("{callable:?}").contains("after"));
    assert!(format!("{d:?}").contains("after: Binding"));
    // Pinned: a change to the encoding that moves this moves a stored
    // binding's digest, and raises fennec_ql::GRAMMAR_EPOCH with it.
    assert_eq!(base, 0x129d_557c_ceeb_0ded, "{base:#018x}");
}

/// Nothing runs one yet: every way of running a statement meets the
/// engine's definition, which refuses a binding in words of its own before
/// it asks anything else -- alone, in a block behind a write, which lands
/// nothing, and as a replacement of a statement held.
#[test]
fn a_binding_is_refused_where_statements_run() {
    fennec_ql::install();
    let mut db = Database::new();
    for s in parse(
        "create collection orders (status text, total int); create collection ledger (o int); \
         create statement s() as get ledger select o",
    )
    .unwrap()
    {
        db.execute(&s).unwrap();
    }
    let bound = parse_one(
        "create statement paid after set orders where new.status = 'paid' as insert ledger {o: \
         new.id}",
    )
    .unwrap();
    let not_built = Error::Query(BOUND_NOT_BUILT.into());
    assert_eq!(db.execute(&bound), Err(not_built.clone()));
    assert_eq!(db.execute_with(&bound, &[]), Err(not_built.clone()));
    let put = parse_one("put orders {status: 'open', total: 3}").unwrap();
    assert_eq!(
        db.execute_block(&[(&put, &[]), (&bound, &[])]),
        Err((1, not_built.clone()))
    );
    let replace =
        parse_one("create or replace statement s after insert orders as insert ledger {o: 1}")
            .unwrap();
    assert_eq!(db.execute(&replace), Err(not_built));
    let orders = db
        .execute(&parse_one("get orders select id").unwrap())
        .unwrap();
    assert_eq!(
        orders.rows().unwrap().rows.len(),
        0,
        "the block put back whole"
    );
    let listed = db.execute(&parse_one("statements").unwrap()).unwrap();
    assert_eq!(listed.rows().unwrap().rows.len(), 1, "`s` alone, as it was");
}

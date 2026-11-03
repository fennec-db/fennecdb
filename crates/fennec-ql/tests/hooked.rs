//! Webhook consumers' syntax (`docs/design/named-statements.md`, *F4 in
//! slices*, slice 1): `create statement <name> [($p <type> from <source>,
//! ...)] on hook <hook> [where <condition>] as <body>` read into its tree --
//! what each parameter reads of the event, the condition over the event's
//! paths -- the refusals the text alone answers, the hook in the canonical
//! tree, and every way of running one refusing it until a server takes
//! events, so no hook is defined that no sender reaches.

use fennec_core::named::{
    Definition, Source, Webhook, HOOKED_AND_OTHER, HOOKED_CONDITION_VALUES, HOOKED_NOT_BUILT,
    HOOKED_PARAMS, HOOKED_VALUES,
};
use fennec_core::prelude::*;
use fennec_ql::{parse, parse_one};

/// The design's payment: four parameters from Stripe's event, a condition
/// over its type and mode, and a guard holding the event to the data.
const STRIPE: &str = "create statement stripe_paid(
    $intent text from event.data.object.id,
    $order text from event.data.object.metadata.order,
    $amount int from event.data.object.amount_received,
    $currency text from event.data.object.currency)
  on hook stripe
  where event.type = 'payment_intent.succeeded' and event.livemode = true
  comment 'Marks the order an intent paid, for its whole total alone'
as set orders {status: 'paid', intent: $intent}
   where ref = $order and total = $amount and currency = $currency
     and (status = 'open' or intent = $intent)
   require 1 else 'no open order of that total'";

fn definition(text: &str) -> Definition {
    match parse_one(text) {
        Ok(Statement::CreateStatement(d)) => *d,
        other => panic!("{text}: {other:?}"),
    }
}

fn hook(text: &str) -> Webhook {
    *definition(text).hook.expect("a hook")
}

/// What `$p <type> from <from>` reads, in a statement that writes.
fn source(ty: &str, from: &str) -> Source {
    let mut h = hook(&format!(
        "create statement s($p {ty} from {from}) on hook h as insert t {{a: $p}}"
    ));
    h.sources.remove(0)
}

/// The condition `where <condition>` reads into.
fn condition(text: &str) -> Expr {
    hook(&format!(
        "create statement s on hook h where {text} as insert t {{a: 1}}"
    ))
    .condition
    .expect("a condition")
}

fn path(keys: &[&str]) -> Source {
    Source::Event(keys.iter().map(|k| k.to_string()).collect())
}

fn field(f: &str) -> Box<Expr> {
    Box::new(Expr::Field(f.into()))
}

fn eq(f: &str, v: Value) -> Expr {
    Expr::Cmp(CmpOp::Eq, field(f), Box::new(Expr::Lit(v)))
}

/// `text` refused in words holding `words`, placed at the first `at` in it,
/// in characters as the parser counts: the word refused is not consumed.
fn refused_at(text: &str, words: &str, at: &str) {
    refused_in(text, "", words, at);
}

/// `before` and `part` refused in words holding `words`, placed at the
/// first `at` in `part`.
fn refused_in(before: &str, part: &str, words: &str, at: &str) {
    let text = format!("{before}{part}");
    let e = match parse(&text) {
        Err(e) => e.to_string(),
        Ok(s) => panic!("{text} parsed: {s:?}"),
    };
    assert!(e.contains(words), "{text}: {e}");
    let (from, within) = match part.is_empty() {
        true => (0, before),
        false => (before.chars().count(), part),
    };
    let place = from
        + within[..within.find(at).expect("the word is in the text")]
            .chars()
            .count();
    assert!(
        e.contains(&format!("position {place}:")),
        "{text}: {e}, not at {place}"
    );
}

#[test]
fn a_hook_reads_into_its_tree() {
    let d = definition(STRIPE);
    assert_eq!(d.name, "stripe_paid");
    let names: Vec<(&str, &DataType)> = d.params.iter().map(|p| (&*p.name, &p.ty)).collect();
    assert_eq!(
        names,
        [
            ("intent", &DataType::Text),
            ("order", &DataType::Text),
            ("amount", &DataType::Int),
            ("currency", &DataType::Text)
        ]
    );
    assert!(d.params.iter().all(|p| p.default.is_none()));
    assert_eq!(
        d.hook.as_deref(),
        Some(&Webhook {
            name: "stripe".into(),
            condition: Some(Expr::And(
                Box::new(eq(
                    "event.type",
                    Value::Text("payment_intent.succeeded".into())
                )),
                Box::new(eq("event.livemode", Value::Bool(true))),
            )),
            sources: vec![
                path(&["data", "object", "id"]),
                path(&["data", "object", "metadata", "order"]),
                path(&["data", "object", "amount_received"]),
                path(&["data", "object", "currency"]),
            ],
        })
    );
    assert_eq!(
        d.comment.as_deref(),
        Some("Marks the order an intent paid, for its whole total alone")
    );
    assert!(d.after.is_none() && d.schedule.is_none());
    assert!(d.claims.is_empty() && !d.public && !d.scans && d.check.is_none());
    assert_eq!(d.body.len(), 1);
    assert_eq!(d.text, STRIPE);
    // A text read back is its tree, as an open reads a stored one.
    assert_eq!(fennec_ql::definition(&d.text).unwrap(), d);

    // No parameters, no condition: every event of its hook.
    let d = definition("create statement pushed on hook github as insert pushes {at: now()}");
    assert_eq!(
        d.hook.as_deref(),
        Some(&Webhook {
            name: "github".into(),
            condition: None,
            sources: Vec::new(),
        })
    );
    assert!(d.params.is_empty());

    // The whole body, a `json` parameter's; the hook's own three values;
    // `event` and `hook` in any case, the keys as written.
    assert_eq!(source("json", "event"), Source::Event(Vec::new()));
    assert_eq!(source("json", "EVENT"), Source::Event(Vec::new()));
    assert_eq!(source("text", "hook.id"), Source::HookId);
    assert_eq!(source("text", "Hook.ID"), Source::HookId);
    assert_eq!(source("timestamp", "hook.at"), Source::HookAt);
    assert_eq!(source("text", "hook.event"), Source::HookEvent);
    for from in ["hook.id", "hook.at", "hook.event"] {
        source("json", from);
    }
    assert_eq!(
        source("text", "Event.Data.Object"),
        path(&["Data", "Object"])
    );
    assert_eq!(source("[text]", "event.data.tags"), path(&["data", "tags"]));
    // A key FennecQL writes no name for, quoted: anywhere in the path.
    assert_eq!(
        source("text", "event.data.object.metadata.\"order-id\""),
        path(&["data", "object", "metadata", "order-id"])
    );
    assert_eq!(
        source("int", "event.\"0\".\"a b\".c"),
        path(&["0", "a b", "c"])
    );
    // A default after the path, and `check` over the parameters.
    let d = definition(
        "create statement s($order text from event.metadata.order = '', $n int from event.n) on \
         hook h check $n > 0 scans comment 'x' as del t where id = $order",
    );
    assert_eq!(d.params[0].default, Some(Value::Text("".into())));
    assert!(d.check.is_some() && d.scans && d.comment.is_some());

    // The condition: `event.` and `hook.event` in lower case, the keys as
    // written, any expression over them and literals.
    assert_eq!(
        condition("EVENT.Type = 'x'"),
        eq("event.Type", Value::Text("x".into()))
    );
    assert_eq!(
        condition("Hook.Event = 'push'"),
        eq("hook.event", Value::Text("push".into()))
    );
    assert_eq!(
        condition("event.metadata.\"order-id\" = 'x'"),
        eq("event.metadata.order-id", Value::Text("x".into()))
    );
    // A quoted key that reads as a name is that name's path.
    assert_eq!(
        condition("Event.\"type\" = 'x'"),
        condition("event.type = 'x'")
    );
    assert_eq!(
        condition("hook.\"event\" = 'x'"),
        condition("hook.event = 'x'")
    );
    assert_eq!(source("text", "hook.\"id\""), Source::HookId);
    let c = condition(
        "lower(event.type) in ['a', 'b'] and not (event.data.amount > 10) or hook.event is null",
    );
    let mut read = Vec::new();
    c.referenced_fields(&mut read);
    assert_eq!(read, ["event.type", "event.data.amount", "hook.event"]);

    // In the body `event.` is a field's path, as anywhere: a collection's
    // `json` field named `event`, held to its schema where it runs.
    let d = definition(
        "create statement s($id text from event.id) on hook h as begin get events select id \
         where event.id = $id limit 1 require 0 else 'taken'; insert events {key: $id, hook: \
         'h'}; end",
    );
    let Statement::Select(sel) = &d.body[0] else {
        panic!("a guard")
    };
    let mut read = Vec::new();
    sel.filter.as_ref().unwrap().referenced_fields(&mut read);
    assert_eq!(read, ["event.id"]);
    assert!(d.block && d.body.len() == 2);
}

#[test]
fn a_hook_s_refusals() {
    let writes = "as insert t {a: 1}";
    // Each parameter reads the event, and only a hook's does.
    refused_at(
        &format!("create statement s($a int, $b int from event.b) on hook h {writes}"),
        HOOKED_PARAMS,
        "$a",
    );
    refused_at(
        "create statement s($a text from event.a) public as set t {a: $a} where id = 1",
        "`from` reads a webhook's event, and the statement takes none",
        "from",
    );
    refused_at(
        "create statement s($a text from event.a) as set t {a: $a} where id = 1",
        "`from` reads a webhook's event",
        "from",
    );
    refused_at(
        &format!("create statement s($a text from event.a) comment 'x' on hook h {writes}"),
        "`on hook <name>` follows the statement's parameters",
        "on hook",
    );
    for text in [
        format!("create statement s on hooks h {writes}"),
        format!("create statement s($a text from event.a) on stripe {writes}"),
    ] {
        refused_at(
            &text,
            "`on` names the hook",
            if text.contains("hooks") {
                "hooks"
            } else {
                "stripe"
            },
        );
    }
    // What a parameter reads.
    for (from, at) in [
        ("evnt.a", "evnt.a"),
        ("'event.a'", "'event.a'"),
        ("$b", "$b"),
        ("data.object.id", "data.object.id"),
    ] {
        refused_at(
            &format!("create statement s($a text from {from}) on hook h {writes}"),
            "`from` names what of the event a parameter reads",
            at,
        );
    }
    refused_at(
        &format!("create statement s($a text from hook.key) on hook h {writes}"),
        "of the hook's own values a parameter reads",
        "hook.key",
    );
    refused_at(
        &format!("create statement s($a text from event) on hook h {writes}"),
        "the whole event is a `json` parameter's",
        "event)",
    );
    for (ty, from, words) in [
        ("int", "hook.id", "`hook.id` is a text"),
        ("timestamp", "hook.event", "`hook.event` is a text"),
        ("text", "hook.at", "`hook.at` is a timestamp"),
    ] {
        refused_at(
            &format!("create statement s($a {ty} from {from}) on hook h {writes}"),
            words,
            from,
        );
    }
    refused_at(
        &format!("create statement s($a text = 'x' from event.a) on hook h {writes}"),
        "goes before its default",
        "from",
    );
    let deep: String = (0..65).map(|k| format!(".k{k}")).collect();
    refused_at(
        &format!("create statement s($a text from event{deep}) on hook h {writes}"),
        "at most 64 keys past `event`",
        "event.",
    );
    refused_at(
        &format!("create statement s on hook h where event{deep} = 1 {writes}"),
        "at most 64 keys past `event`",
        "event.",
    );
    refused_at(
        &format!("create statement s($a text from event.\"a-b\"{deep}) on hook h {writes}"),
        "at most 64 keys past `event`",
        "event.",
    );
    // The hook's name: a word, the path its sender is told.
    refused_at(
        &format!("create statement s on hook \"stripe-live\" {writes}"),
        "is no hook's name",
        "\"stripe-live\"",
    );
    refused_at(
        &format!("create statement s on hook a.b {writes}"),
        "a name holds no dot",
        "a.b",
    );
    // Called by no token.
    for at in ["claims (sub text)", "public"] {
        refused_at(
            &format!("create statement s on hook h {at} {writes}"),
            HOOKED_VALUES,
            at,
        );
    }
    refused_at(
        "create statement s on hook h as insert t {by: $jwt.sub}",
        HOOKED_VALUES,
        "$jwt.sub",
    );
    refused_at(
        "create statement s($a int from event.a) on hook h as insert t {a: $2}",
        "declares 1 parameters, each read from the event",
        "$2",
    );
    // One statement, one way to run.
    for clause in ["every 1h", "at '0 3 * * *'", "after set t"] {
        refused_at(
            &format!("create statement s on hook h {clause} {writes}"),
            HOOKED_AND_OTHER,
            clause,
        );
    }
    refused_at(
        &format!("create statement s after set t on hook h {writes}"),
        HOOKED_AND_OTHER,
        "on hook",
    );
    refused_at(
        &format!("create statement s every 1h on hook h {writes}"),
        HOOKED_AND_OTHER,
        "on hook",
    );
    refused_at(
        &format!("create statement s on hook a on hook b {writes}"),
        "a statement takes one hook's events",
        "on hook b",
    );
    refused_at(
        &format!("create statement s on hook h comment 'x' where event.a = 1 {writes}"),
        "a hook's condition follows its name",
        "where",
    );
    refused_at(
        &format!("create statement s on hook h($a text from event.a) {writes}"),
        "parameters follow its name, before `on hook`",
        "($a",
    );
    // The condition reads the event and literals alone.
    for (cond, words, at) in [
        ("type = 'x'", "a bare name has no row", "type"),
        ("id = 1", "a bare name has no row", "id"),
        ("event = 'x'", "the whole event is a parameter's", "event"),
        (
            "hook.id = 'x'",
            "`hook.id` and `hook.at` are a parameter's",
            "hook.id",
        ),
        (
            "hook.at > 0",
            "`hook.id` and `hook.at` are a parameter's",
            "hook.at",
        ),
        ("event.a = $a", HOOKED_CONDITION_VALUES, "$a"),
        ("event.a = $1", HOOKED_CONDITION_VALUES, "$1"),
        ("event.a > now()", "and no clock", "event.a"),
        (
            "event.a in (get t select id)",
            "reads the event alone",
            "get",
        ),
        ("count(event.a) > 1", "folds none", "count"),
        ("new.a = 1", "no row is new or old", "new.a"),
        ("OLD.a = 1", "no row is new or old", "OLD.a"),
        ("slot() > 0", "read in its body alone", "slot()"),
        (
            "hook.\"id\" = 'x'",
            "`hook.id` and `hook.at` are a parameter's",
            "hook.",
        ),
        // SQL's quoted column is text here, as anywhere.
        ("\"type\" = 'x'", "double quotes make text here", "\"type\""),
    ] {
        refused_in(
            "create statement s($a int from event.a) on hook h where ",
            &format!("{cond} {writes}"),
            words,
            at,
        );
    }
    // The body: no row new or old, no slot, and nothing answered.
    for (body, words, at) in [
        ("insert t {a: new.a}", "no row is new or old", "new.a"),
        ("insert t {at: slot()}", "read in its body alone", "slot()"),
        (
            "set t {a: 1} where event.\"a-b\" = 1",
            "a quoted key reads a webhook's event",
            "event.",
        ),
        (
            "begin get t select a where a = 1; insert t {a: 1}; end",
            "its `get` is a guard",
            "get",
        ),
        (
            "set t {a: 1} where a = 2 returning a",
            "its writes say nothing with `returning`",
            "set",
        ),
        (
            "put t {id: 1, a: 1}",
            "a `put` naming an id writes over",
            "put",
        ),
    ] {
        refused_in("create statement s on hook h as ", body, words, at);
    }
    refused_at(
        "create statement guard on hook h as get t select a where a = 1 limit 1 require 1",
        "body of guards alone takes an event and changes nothing",
        "guard",
    );
    // A quoted key is a hook's alone; SQL's quoted column is a name here.
    for (text, at) in [
        ("get t where meta.\"a-b\" = 1", "meta."),
        ("select o.\"total\" from orders o", "o.\""),
        (
            "create statement s() public as set t {a: 1} where meta.\"a-b\" = 1",
            "meta.",
        ),
    ] {
        refused_at(text, "a quoted key reads a webhook's event", at);
    }
}

/// The hook is tag 11 of the canonical tree, written only where a
/// definition holds one, so no stored digest moves; every spelling of a
/// hook's meaning is one digest, and every change of it another.
#[test]
fn a_hook_is_in_the_canonical_tree() {
    let digest = |t: &str| definition(t).digest().unwrap();
    let base = digest(STRIPE);
    let spelled = "CREATE OR REPLACE STATEMENT stripe_paid($intent TEXT FROM EVENT.data.object.id,
    $order TEXT FROM Event.data.object.metadata.\"order\", -- quoted, a name all the same
    $amount INT FROM event.data.object.amount_received, $currency TEXT FROM event.data.object.currency)
  ON HOOK stripe WHERE (EVENT.type = 'payment_intent.succeeded') AND event.livemode = TRUE
  COMMENT \"Marks the order an intent paid, for its whole total alone\"
AS BEGIN UPDATE orders SET status = 'paid', intent = $1
   WHERE ref = $order AND total = $amount AND currency = $currency
     AND (status = 'open' OR intent = $intent)
   REQUIRE 1 ELSE 'no open order of that total'; END";
    assert_eq!(digest(spelled), base);

    let mut seen = vec![base];
    for (from, to) in [
        ("stripe_paid", "stripe_paye"),
        ("on hook stripe", "on hook stripe_test"),
        ("event.data.object.id", "event.data.object.payment_intent"),
        ("$amount int", "$amount float"),
        (
            "event.type = 'payment_intent.succeeded'",
            "event.type = 'charge.refunded'",
        ),
        ("event.livemode = true", "event.livemode = false"),
        (
            "  where event.type = 'payment_intent.succeeded' and event.livemode = true\n",
            "",
        ),
        ("its whole total alone", "its total alone"),
        ("require 1", "require 2"),
        (
            "$currency text from event.data.object.currency",
            "$currency text from hook.event",
        ),
    ] {
        assert!(STRIPE.contains(from), "{from}");
        let d = digest(&STRIPE.replacen(from, to, 1));
        assert!(!seen.contains(&d), "{from} -> {to}");
        seen.push(d);
    }
    // The hook's three values, and the whole body, apart.
    let read = |from: &str| {
        digest(&format!(
            "create statement s($p json from {from}) on hook h as insert t {{a: $p}}"
        ))
    };
    let all = [
        read("event"),
        read("event.id"),
        read("hook.id"),
        read("hook.at"),
        read("hook.event"),
    ];
    for (k, d) in all.iter().enumerate() {
        assert!(!all[..k].contains(d), "{k}");
    }
    assert_eq!(read("event.id"), read("EVENT.\"id\""));
    // A hook's statement is not the callable one of the same body.
    assert_ne!(
        digest("create statement s on hook h as del u where a = 1"),
        digest("create statement s() public as del u where a = 1")
    );

    // Written as tag 11 after the body, and nowhere for any other:
    // `Debug`, which `make fennecql-corpus` compares, says it alike.
    let bytes = |t: &str| definition(t).canonical().unwrap();
    // Tag 11, a node of kind 7: the name under 1; the node's end and the
    // definition's.
    let tail = [11, 7, 1, 1, b'h', 0, 0];
    let b = bytes("create statement s on hook h as del u where a = 1");
    assert!(b.ends_with(&tail), "{b:?}");
    // What each parameter reads under 3: the whole body a node of kind 8
    // alone, a path its keys under 2, `hook.id` 1 under 1.
    let b = bytes(
        "create statement s($e json from event, $p text from event.a.b, $k text from hook.id) on \
         hook h as insert u {a: $p}",
    );
    let sources = [
        &[11, 7, 1, 1, b'h', 3, 3][..],
        &[8, 0],
        &[8, 2, 2, 1, b'a', 1, b'b', 0],
        &[8, 1, 1, 0],
        &[0, 0],
    ]
    .concat();
    assert!(b.ends_with(&sources), "{b:?}");
    let callable = definition("create statement s($a int) public as del u where id = $a");
    assert!(!format!("{callable:?}").contains("hook"));
    assert!(format!("{:?}", definition(STRIPE)).contains("hook: Webhook"));
    // Pinned: a change to the encoding that moves this moves a stored
    // hook's digest, and raises fennec_ql::GRAMMAR_EPOCH with it.
    assert_eq!(base, 0xed42_cbd4_47b3_61f9, "{base:#018x}");
}

/// Every way of running a statement meets the engine's definition, which
/// refuses a hook in words of its own before it asks anything else --
/// alone, in a block behind a write, which lands nothing, and as a
/// replacement of a statement held. Never a hook no sender reaches.
#[test]
fn a_hook_is_refused_where_statements_run() {
    fennec_ql::install();
    let mut db = Database::new();
    for s in parse(
        "create collection orders (ref text, status text, intent text, total int, currency \
         text); create statement s() as get orders select status",
    )
    .unwrap()
    {
        db.execute(&s).unwrap();
    }
    let stripe = parse_one(STRIPE).unwrap();
    let not_built = Error::Query(HOOKED_NOT_BUILT.into());
    assert_eq!(db.execute(&stripe), Err(not_built.clone()));
    assert_eq!(db.execute_with(&stripe, &[]), Err(not_built.clone()));
    let put = parse_one("put orders {ref: 'a', total: 3}").unwrap();
    assert_eq!(
        db.execute_block(&[(&put, &[]), (&stripe, &[])]),
        Err((1, not_built.clone()))
    );
    let replace = parse_one(
        "create or replace statement s($o text from event.data.object.metadata.order) on hook \
         stripe as del orders where ref = $o",
    )
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

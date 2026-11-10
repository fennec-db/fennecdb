//! Webhook consumers as their database keeps them and takes their events
//! (`docs/design/named-statements.md`, F4's slice 2): a definition is
//! record kind 13's op 6, its hook's name after its text -- one write the
//! counter counts, put back with its block, kept by every open, image and
//! compact, applied by a replica and passed over by a reader of the
//! changes -- refused where a test does not let it be defined
//! (`Database::allow_hooks`); and `Database::take_event`, an event's
//! statements run in name order with its key's row in `_hook_events` as
//! one block, a held key answered as the first taking was.

use crate::stored::{body_at, body_end, kind_of, opened, refused, run, Tap};
use fennec_core::codec::{encode_str, put_uvarint};
use fennec_core::engine::{writes_in, ChangeKind, Event, HeldKey, Took, HOOK_EVENTS};
use fennec_core::named::{event_at, GRAMMAR_EPOCH, HOOKED_UNTAKEN, MAX_HOOKED};
use fennec_core::prelude::*;
use std::sync::Arc;

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;

const ORDERS: &str = "create collection orders (ref text @hash, total int, currency text, \
                      status text, intent text, at timestamp)";

/// The design's example: an order marked paid by the intent Stripe says
/// succeeded, for its whole total alone.
const STRIPE_PAID: &str = "create statement stripe_paid(
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

/// Saturday 28 March 2026, noon UTC: the clock every test here reads.
fn t0() -> i64 {
    (fennec_core::time::days_from_civil(2026, 3, 28) * 1440 + 12 * 60) * MINUTE
}

/// A database over `orders` holding order `o-1` of 2000 euros, its clock
/// at `t0`, letting a hook be defined.
fn orders(tap: &Tap) -> Database {
    let mut db = tap.database();
    db.set_clock(Some(t0()));
    db.allow_hooks();
    run(&mut db, ORDERS).unwrap();
    run(
        &mut db,
        "put orders {ref: 'o-1', total: 2000, currency: 'eur', status: 'open'}",
    )
    .unwrap();
    db
}

fn defined(tap: &Tap) -> Database {
    let mut db = orders(tap);
    run(&mut db, STRIPE_PAID).unwrap();
    db
}

/// Stripe's `payment_intent.succeeded` for `order`, as its body parses.
fn paid(id: &str, order: &str, amount: i64) -> Value {
    fennec_core::json::parse_json(&format!(
        r#"{{"id": "{id}", "type": "payment_intent.succeeded", "livemode": true,
            "data": {{"object": {{"id": "pi_{id}", "amount_received": {amount},
            "currency": "eur", "metadata": {{"order": "{order}"}}}}}}}}"#
    ))
    .unwrap()
}

/// An event of the hook `stripe` keyed by its `id`, kept for 30 days.
fn event<'a>(body: &'a Value, key: &'a str, digest: u64) -> Event<'a> {
    Event {
        hook: "stripe",
        key,
        body,
        digest,
        at: Some(t0() - MINUTE),
        event: None,
        keep: 30 * DAY,
    }
}

fn rows(db: &Database, text: &str) -> Vec<Vec<Value>> {
    let stmt = fennec_ql::parse_one(text).unwrap();
    match db.query(&stmt, &[]).unwrap() {
        Response::Rows(rs) => rs.rows.into_iter().map(|r| r.values).collect(),
        other => panic!("{other:?}"),
    }
}

fn status(db: &Database, reference: &str) -> Value {
    rows(
        db,
        &format!("get orders select status where ref = '{reference}'"),
    )
    .first()
    .map_or(Value::Null, |r| r[0].clone())
}

/// The keys `_hook_events` holds, as a read finds them.
fn keys(db: &Database) -> Vec<Value> {
    if db.collection(HOOK_EVENTS).is_err() {
        return Vec::new();
    }
    rows(db, "get _hook_events select key order key")
        .into_iter()
        .map(|r| r[0].clone())
        .collect()
}

/// The kinds of the records a write's record holds: its own, or a block's
/// (kind 9) records', each with a kind-13 record's op.
fn kinds_in(record: &[u8]) -> Vec<(u8, Option<u8>)> {
    let mut p = body_at(record);
    let kind = kind_of(record);
    if kind != 9 {
        return vec![(kind, (kind == 13).then(|| record[p]))];
    }
    let mut out = Vec::new();
    while p < body_end(record) {
        let kind = record[p];
        let mut q = p + 1;
        fennec_core::codec::get_uvarint(record, &mut q).unwrap();
        let len = fennec_core::codec::get_uvarint(record, &mut q).unwrap() as usize;
        out.push((kind, (kind == 13).then(|| record[q])));
        p = q + len;
    }
    out
}

// ------------------------------------------------------------ the record

#[test]
fn a_hook_is_one_record_of_op_6_refused_but_where_a_test_lets_it() {
    let tap = Tap::default();
    let mut db = tap.database();
    run(&mut db, ORDERS).unwrap();
    assert_eq!(
        refused(run(&mut db, STRIPE_PAID)),
        Error::Query(HOOKED_UNTAKEN.into()).to_string()
    );
    assert!(db.statements().is_empty());

    let tap = Tap::default();
    let mut db = orders(&tap);
    let before = db.change_seq();
    run(&mut db, STRIPE_PAID).unwrap();
    let (seq, record) = tap.last();
    let p = body_at(&record);
    assert_eq!((kind_of(&record), record[p]), (13, 6), "kind 13, op 6");
    assert_eq!(writes_in(&record).unwrap(), 1);
    assert_eq!(seq, before + 1);
    // The hook's name is the record's tail, after the text.
    let mut tail = Vec::new();
    encode_str(&mut tail, STRIPE_PAID);
    encode_str(&mut tail, "stripe");
    assert!(
        record[..body_end(&record)].ends_with(&tail),
        "the text, then the hook"
    );
    let s = db.statements().get("stripe_paid").unwrap().clone();
    assert_eq!(s.hook.as_deref(), Some("stripe"));
    assert!(s.tree.is_some() && s.broken.is_none(), "{:?}", s.broken);
    assert_eq!(s.kind(), fennec_core::named::Kind::Hooked);
    assert!(!s.called(), "no token's call is granted it");
    assert_eq!(
        s.depends,
        [
            "orders",
            "orders.status",
            "orders.intent",
            "orders.ref",
            "orders.total",
            "orders.currency"
        ]
    );
    // A call with no event runs nothing: its events run it, and the
    // server's token's call carries one.
    let e = refused(run(&mut db, "call stripe_paid"));
    assert!(e.contains("takes a hook's events"), "{e}");
    // What it reads, a schema change refused naming it.
    let e = refused(run(&mut db, "alter collection orders drop field total"));
    assert!(e.contains("statement `stripe_paid`"), "{e}");
}

/// A webhook consumer's literal lists run as written: `[0.25, 2]` handed
/// an `[float]` and a json field, where the quick read's `f32`s were
/// refused at every event (`fennec_ql::stored_definition`).
#[test]
fn a_hook_s_body_runs_its_lists_as_written() {
    let tap = Tap::default();
    let mut db = orders(&tap);
    run(
        &mut db,
        "create collection taken (k text, fs [float], j json)",
    )
    .unwrap();
    run(
        &mut db,
        "create statement took($k text from event.id) on hook stripe \
         as insert taken {k: $k, fs: [0.25, 2], j: [0.1]}",
    )
    .unwrap();
    let body = paid("evt_1", "o-1", 2000);
    assert_eq!(
        db.take_event(&event(&body, "evt_1", 7)).unwrap(),
        Took::Ran(1)
    );
    assert_eq!(
        rows(&db, "get taken select k, fs, j"),
        vec![vec![
            Value::Text("evt_1".into()),
            Value::List(vec![Value::Float(0.25), Value::Float(2.0)]),
            Value::List(vec![Value::Float(0.1)]),
        ]]
    );
}

#[test]
fn a_block_put_back_puts_back_a_hook_s_definition() {
    let tap = Tap::default();
    let mut db = orders(&tap);
    db.begin().unwrap();
    run(&mut db, STRIPE_PAID).unwrap();
    assert!(db.statements().get("stripe_paid").is_some());
    db.rollback();
    assert!(db.statements().get("stripe_paid").is_none());
    assert_eq!(
        refused(db.take_event(&event(&paid("evt_1", "o-1", 2000), "evt_1", 1))),
        "already exists: no statement takes hook `stripe`"
    );
}

#[test]
fn an_image_a_compact_a_replica_and_a_reader_of_the_changes_keep_a_hook() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    let body = paid("evt_1", "o-1", 2000);
    assert_eq!(
        db.take_event(&event(&body, "evt_1", 7)).unwrap(),
        Took::Ran(1)
    );
    let held = |db: &Database| -> Vec<_> {
        db.statements()
            .iter()
            .map(|s| (s.name.clone(), s.version, s.hook.clone(), s.text.clone()))
            .collect()
    };
    let want = held(&db);
    // The key's block is the last: an open and an image hold no mark of
    // it, their horizon standing there, and a replica the mark itself.
    let kept = Some(HeldKey {
        taken: 1,
        same: true,
        seq: db.change_seq(),
    });
    let asked = event(&body, "evt_1", 7);

    // An open of the file, and an image, each read at the clock the key
    // was kept by: past its thirty days, a key is as if never kept.
    let at_t0 = |bytes: &[u8]| {
        let mut db = opened(bytes);
        db.set_clock(Some(t0()));
        db
    };
    let again = at_t0(&tap.bytes());
    assert_eq!(held(&again), want);
    assert_eq!(again.event_kept(&asked).unwrap(), kept);
    let image = db.snapshot();
    let mut again = at_t0(&image);
    assert_eq!(held(&again), want);
    assert_eq!(
        again.statements().get("stripe_paid").unwrap().kind(),
        fennec_core::named::Kind::Hooked
    );
    assert_eq!(again.event_kept(&asked).unwrap(), kept);
    again.set_clock(Some(t0() + 30 * DAY + 1));
    assert_eq!(again.event_kept(&asked).unwrap(), None);

    // A replica, write for write, and a reader of the changes: the
    // definition passed over as a statement, the event's block its writes.
    let mut replica = Tap::default().database();
    let writes = tap.writes.lock().unwrap().clone();
    for (seq, record) in &writes {
        replica.apply(record).unwrap();
        assert_eq!(replica.change_seq(), *seq);
    }
    assert_eq!(held(&replica), want);
    replica.set_clock(Some(t0()));
    assert_eq!(replica.event_kept(&asked).unwrap(), kept);
    let all: Vec<u8> = writes.iter().flat_map(|(_, r)| r.clone()).collect();
    let (mut statements, mut collections) = (0, Vec::new());
    let last = db
        .changes_in(&all, 1, &mut |c| {
            match c.kind {
                ChangeKind::Statement => statements += 1,
                _ => collections.push(c.collection.unwrap_or_default()),
            }
            true
        })
        .unwrap();
    assert_eq!(last, db.change_seq());
    assert_eq!(statements, 1, "the definition, numbered");
    assert_eq!(
        collections,
        ["orders", "orders", "orders", HOOK_EVENTS, HOOK_EVENTS],
        "the collection and the order, then the event's block: its write, the keys' \
         collection made and its key"
    );

    // A compact keeps it, of a file read into memory and of a mapped one.
    let dir = std::env::temp_dir().join(format!("fennec-hooked-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for mapped in [false, true] {
        let path = dir.join(format!("compact-{mapped}.fennec"));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, &image).unwrap();
        fennec_ql::install();
        let mut db = match mapped {
            true => fennec_core::fs::open_mapped(&path).unwrap(),
            false => fennec_core::fs::open(&path).unwrap(),
        };
        run(&mut db, "compact").unwrap();
        drop(db);
        let mut again = opened(&std::fs::read(&path).unwrap());
        assert_eq!(held(&again), want, "mapped: {mapped}");
        again.set_clock(Some(t0()));
        assert_eq!(again.event_kept(&asked).unwrap(), kept, "mapped: {mapped}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_replacement_keeps_the_kind_and_changes_its_hook_freely() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    // Its hook, condition, parameters and body: no grant hangs on them.
    run(
        &mut db,
        "create or replace statement stripe_paid($order text from event.order) on hook \
         stripe_test as set orders {status: 'paid'} where ref = $order require 1",
    )
    .unwrap();
    let s = db.statements().get("stripe_paid").unwrap();
    assert_eq!(s.hook.as_deref(), Some("stripe_test"));
    run(
        &mut db,
        "create statement ask() as get orders select id where ref = 'x'",
    )
    .unwrap();
    db.allow_schedules();
    run(
        &mut db,
        "create statement tidy every 1d as del orders where ref = 'void'",
    )
    .unwrap();
    for (text, what) in [
        (
            "create or replace statement ask($o text from event.o) on hook stripe as set orders \
             {status: 'x'} where ref = $o",
            "takes a hook's events, where it was called",
        ),
        (
            "create or replace statement tidy($o text from event.o) on hook stripe as set orders \
             {status: 'x'} where ref = $o",
            "takes a hook's events, where it ran by the clock",
        ),
        (
            "create or replace statement stripe_paid() as get orders select id where ref = 'x'",
            "is called, where it took a hook's events",
        ),
        (
            "create or replace statement stripe_paid every 1h as del orders where ref = 'x'",
            "runs by the clock, where it took a hook's events",
        ),
        (
            "create or replace statement stripe_paid after insert orders as set orders {status: \
             'y'} where id = new.id",
            "is bound to writes, where it took a hook's events",
        ),
    ] {
        let e = refused(run(&mut db, text));
        assert!(e.contains(what), "{text}: {e}");
    }
}

#[test]
fn a_hook_s_events_are_taken_by_at_most_sixteen() {
    let tap = Tap::default();
    let mut db = orders(&tap);
    for i in 0..MAX_HOOKED {
        run(
            &mut db,
            &format!(
                "create statement s{i}($o text from event.o) on hook stripe as set orders \
                 {{status: 's{i}'}} where ref = $o"
            ),
        )
        .unwrap();
    }
    let e = refused(run(
        &mut db,
        "create statement one_more($o text from event.o) on hook stripe as set orders {status: \
         'x'} where ref = $o",
    ));
    assert!(e.contains("at most 16"), "{e}");
    // Another hook's are its own, and a replacement takes its own place.
    run(
        &mut db,
        "create statement one_more($o text from event.o) on hook github as set orders {status: \
         'x'} where ref = $o",
    )
    .unwrap();
    run(
        &mut db,
        "create or replace statement s0($o text from event.o) on hook stripe as set orders \
         {status: 'z'} where ref = $o",
    )
    .unwrap();
}

/// An op-6 record written by hand: what a later build, or another, may
/// have written.
fn hooked_record(name: &str, text: &str, hook: &[u8], change: u64) -> Vec<u8> {
    let mut body = vec![6u8];
    put_uvarint(&mut body, 0);
    put_uvarint(&mut body, change);
    put_uvarint(&mut body, GRAMMAR_EPOCH as u64);
    body.extend_from_slice(&crate::stored::digest(text).to_le_bytes());
    encode_str(&mut body, name);
    put_uvarint(&mut body, 1);
    encode_str(&mut body, "orders");
    encode_str(&mut body, text);
    body.extend_from_slice(hook);
    body
}

fn with_record(file: &[u8], body: &[u8]) -> Vec<u8> {
    let mut f = file.to_vec();
    // With its checksum, as this version writes a record.
    f.extend_from_slice(&fennec_core::engine::record(13, 0, body));
    f
}

/// A hook's name that is no word, a byte past it, a record cut short and
/// an op past 6 are refused as corrupt, never read as something else.
#[test]
fn a_hook_s_record_this_version_does_not_read_is_refused() {
    let tap = Tap::default();
    let db = orders(&tap);
    let file = db.snapshot();
    let text = "create statement late($o text from event.o) on hook stripe as set orders \
                {status: 'x'} where ref = $o";
    let hook = |h: &str| {
        let mut b = Vec::new();
        encode_str(&mut b, h);
        b
    };
    let mut past = hook("stripe");
    past.push(0);
    for (tail, reads) in [
        (hook("stripe"), true),
        (hook(""), false),
        (hook("1stripe"), false),
        (hook("str-ipe"), false),
        (past, false),
        (Vec::new(), false),
    ] {
        let body = hooked_record("late", text, &tail, 99);
        let mut db = Database::new();
        // A tail of nothing is a record cut short, its name's length never
        // read; the rest are a later version's, refused in words.
        match (db.load(&with_record(&file, &body)), reads) {
            (Ok(_), true) => {}
            (Err(Error::Corrupt(_)), false) if tail.is_empty() => {}
            (Err(e), false) => assert!(
                e.to_string()
                    .contains("a statement record this version does not read"),
                "{e}"
            ),
            (r, _) => panic!("{tail:?}: {:?}", r.err()),
        }
    }
    let mut db = Database::new();
    let e = db.load(&with_record(&file, &[7u8, 0])).unwrap_err();
    assert!(
        e.to_string()
            .contains("a statement record this version does not read"),
        "{e}"
    );
}

/// A hook's record whose text reads as another hook's, or as another kind,
/// its digest and all, is none this build writes: marked broken, never
/// taking events of a hook its record does not name.
#[test]
fn a_hook_s_record_reading_otherwise_is_marked_broken() {
    let tap = Tap::default();
    let db = orders(&tap);
    let file = db.snapshot();
    for (text, why) in [
        (
            "create statement late($o text from event.o) on hook github as set orders {status: \
             'x'} where ref = $o",
            "takes another hook's events than its record",
        ),
        (
            "create statement late($o text) public as set orders {status: 'x'} where ref = $o",
            "reads as a callable statement",
        ),
        (
            "create statement late every 1h as del orders where ref = 'x'",
            "reads as a scheduled statement",
        ),
    ] {
        let mut hook = Vec::new();
        encode_str(&mut hook, "stripe");
        let mut db = opened(&with_record(&file, &hooked_record("late", text, &hook, 99)));
        let s = db.statements().get("late").unwrap().clone();
        assert!(s.tree.is_none(), "{text}");
        assert_eq!(s.hook.as_deref(), Some("stripe"), "known from the record");
        let broken = s.broken.unwrap_or_default();
        assert!(broken.contains(why), "{text}: {broken}");
        // Unread here, so every event of its hook is refused.
        db.set_clock(Some(t0()));
        let body = paid("evt_1", "o-1", 2000);
        let e = refused(db.take_event(&event(&body, "evt_1", 1)));
        assert!(
            e.contains("statement `late`, which takes hook `stripe`, is held unread"),
            "{e}"
        );
        assert!(keys(&db).is_empty());
    }
}

// ------------------------------------------------------------ the taking

#[test]
fn an_event_is_taken_once_its_writes_and_its_key_one_block() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    let body = paid("evt_1", "o-1", 2000);
    let before = db.change_seq();
    assert_eq!(
        db.take_event(&event(&body, "evt_1", 7)).unwrap(),
        Took::Ran(1)
    );
    assert_eq!(status(&db, "o-1"), Value::Text("paid".into()));
    // One record: the order's write, then the keys' collection made, then
    // the key, last.
    let (seq, record) = tap.last();
    assert_eq!(
        kinds_in(&record),
        [(3, None), (1, None), (3, None)],
        "a block of the write, the collection made and the key"
    );
    assert_eq!(seq, before + 3);
    assert_eq!(keys(&db), [Value::Text("stripe\u{1f}evt_1".into())]);
    let until = rows(&db, "get _hook_events select until, taken, digest");
    assert_eq!(
        until,
        [vec![
            Value::Timestamp(t0() + 30 * DAY),
            Value::Int(1),
            Value::Int(7)
        ]]
    );

    // The same event again -- a retry, a duplicate -- is answered from its
    // key, nothing run, nothing written; a second body under it as well.
    // Its change number the key's, the block's last.
    let held = HeldKey {
        taken: 1,
        same: true,
        seq: before + 3,
    };
    let seq = db.change_seq();
    assert_eq!(
        db.take_event(&event(&body, "evt_1", 7)).unwrap(),
        Took::Held(held)
    );
    assert_eq!(
        db.event_kept(&event(&body, "evt_1", 7)).unwrap(),
        Some(held)
    );
    let other = paid("evt_1", "o-2", 5);
    assert_eq!(
        db.take_event(&event(&other, "evt_1", 8)).unwrap(),
        Took::Held(HeldKey {
            taken: 1,
            same: false,
            seq: before + 3,
        })
    );
    assert_eq!(db.change_seq(), seq, "nothing written");
    // The key is the hook's: the same key sent to another hook is its own.
    let mut test = event(&body, "evt_1", 7);
    test.hook = "stripe_test";
    assert_eq!(db.event_kept(&test).unwrap(), None);
}

#[test]
fn a_refused_event_keeps_no_key_and_is_taken_at_a_retry() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    // Out of order: the payment before its order's row. Its guard refuses
    // it (412), nothing lands, no key is kept.
    let body = paid("evt_2", "o-2", 500);
    let seq = db.change_seq();
    let e = db.take_event(&event(&body, "evt_2", 1)).unwrap_err();
    assert!(
        matches!(&e, Error::Unmet(m) if m.starts_with("no open order of that total")),
        "{e}"
    );
    assert_eq!(db.change_seq(), seq);
    assert!(db.collection(HOOK_EVENTS).is_err(), "no collection made");
    // The order lands; the sender's retry takes the event.
    run(
        &mut db,
        "put orders {ref: 'o-2', total: 500, currency: 'eur', status: 'open'}",
    )
    .unwrap();
    assert_eq!(
        db.take_event(&event(&body, "evt_2", 1)).unwrap(),
        Took::Ran(1)
    );
    assert_eq!(status(&db, "o-2"), Value::Text("paid".into()));
    // In a block its caller opened, it lands with the block or not at all.
    run(
        &mut db,
        "put orders {ref: 'o-3', total: 1, currency: 'eur', status: 'open'}",
    )
    .unwrap();
    let body = paid("evt_3", "o-3", 1);
    db.begin().unwrap();
    assert_eq!(
        db.take_event(&event(&body, "evt_3", 1)).unwrap(),
        Took::Ran(1)
    );
    db.rollback();
    assert_eq!(status(&db, "o-3"), Value::Text("open".into()));
    assert_eq!(db.event_kept(&event(&body, "evt_3", 1)).unwrap(), None);
}

#[test]
fn a_key_is_kept_until_its_own_until() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    run(
        &mut db,
        "create statement github_seen($o text from event.order) on hook github as insert orders \
         {ref: $o, status: 'seen'}",
    )
    .unwrap();
    let body = paid("evt_1", "o-1", 2000);
    let push: Value = fennec_core::json::parse_json(r#"{"order": "g-1"}"#).unwrap();
    db.take_event(&event(&body, "evt_1", 1)).unwrap();
    let mut github = event(&push, "abc", 2);
    github.hook = "github";
    github.keep = 7 * DAY;
    github.at = None;
    assert_eq!(db.take_event(&github).unwrap(), Took::Ran(1));
    // A week on, a millisecond past: the github key is let go of, each
    // row by its own `until`, the stripe key kept.
    db.set_clock(Some(t0() + 7 * DAY + 1));
    assert!(db.event_kept(&event(&body, "evt_1", 1)).unwrap().is_some());
    assert_eq!(db.event_kept(&github).unwrap(), None);
    assert_eq!(db.take_event(&github).unwrap(), Took::Ran(1), "taken again");
    assert_eq!(
        rows(&db, "get orders count where ref = 'g-1'"),
        [vec![Value::Int(2)]]
    );
    // And after thirty days the stripe key too: a resend after `keep` is
    // taken again, its guards deciding what it writes -- `intent =
    // $intent` writes what is there.
    db.set_clock(Some(t0() + 30 * DAY + 1));
    assert_eq!(db.event_kept(&event(&body, "evt_1", 1)).unwrap(), None);
    assert_eq!(
        db.take_event(&event(&body, "evt_1", 1)).unwrap(),
        Took::Ran(1)
    );
}

#[test]
fn an_event_no_condition_takes_writes_nothing() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    let test_mode = fennec_core::json::parse_json(
        r#"{"id": "evt_t", "type": "payment_intent.succeeded", "livemode": false}"#,
    )
    .unwrap();
    let refund =
        fennec_core::json::parse_json(r#"{"id": "evt_r", "type": "charge.refunded"}"#).unwrap();
    let seq = db.change_seq();
    for body in [&test_mode, &refund] {
        assert_eq!(
            db.take_event(&event(body, "evt_x", 1)).unwrap(),
            Took::Passed
        );
    }
    assert_eq!(db.change_seq(), seq);
    assert!(db.collection(HOOK_EVENTS).is_err());
    // A hook no statement takes is refused (409).
    let mut other = event(&refund, "evt_x", 1);
    other.hook = "github";
    let e = db.take_event(&other).unwrap_err();
    assert!(
        matches!(&e, Error::Exists(m) if m == "no statement takes hook `github`"),
        "{e}"
    );
}

#[test]
fn an_event_s_statements_run_in_name_order_in_one_block() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    run(
        &mut db,
        "create collection ledger (ref text @hash, amount int)",
    )
    .unwrap();
    // `a_note` runs first by its name and `z_check` last, reading what
    // `stripe_paid` wrote before it.
    for text in [
        "create statement a_note($order text from event.data.object.metadata.order) on hook \
         stripe as insert ledger {ref: $order, amount: 0}",
        "create statement z_check($order text from event.data.object.metadata.order) on hook \
         stripe where event.type = 'payment_intent.succeeded' as begin
           get orders select id where ref = $order and status = 'paid' limit 1 require 1 \
             else 'not paid';
           set ledger {amount: 1} where ref = $order require 1
         end",
    ] {
        run(&mut db, text).unwrap();
    }
    let body = paid("evt_1", "o-1", 2000);
    assert_eq!(
        db.take_event(&event(&body, "evt_1", 1)).unwrap(),
        Took::Ran(3)
    );
    assert_eq!(
        rows(&db, "get ledger select amount where ref = 'o-1'"),
        [vec![Value::Int(1)]]
    );
    let (_, record) = tap.last();
    assert_eq!(
        kinds_in(&record).len(),
        5,
        "the ledger's row, the order, its line set, the collection and the key: one record"
    );
    assert_eq!(
        rows(&db, "get _hook_events select taken"),
        [vec![Value::Int(3)]]
    );
    // The last refusing: nothing of any lands, no key.
    run(
        &mut db,
        "put orders {ref: 'o-4', total: 9, currency: 'eur', status: 'paid'}",
    )
    .unwrap();
    let seq = db.change_seq();
    let body = paid("evt_4", "o-4", 9);
    let e = db.take_event(&event(&body, "evt_4", 1)).unwrap_err();
    assert!(matches!(&e, Error::Unmet(_)), "{e}");
    assert_eq!(db.change_seq(), seq);
    assert_eq!(
        rows(&db, "get ledger count where ref = 'o-4'"),
        [vec![Value::Int(0)]]
    );
    assert_eq!(db.event_kept(&event(&body, "evt_4", 1)).unwrap(), None);
}

#[test]
fn a_parameter_is_read_by_its_type_and_its_path() {
    let tap = Tap::default();
    let mut db = orders(&tap);
    run(
        &mut db,
        "create collection seen (key text, at timestamp, kind text, n int, raw json, first text)",
    )
    .unwrap();
    run(
        &mut db,
        "create statement keep_it(
            $key text from hook.id,
            $at timestamp from hook.at,
            $kind text from hook.event = 'none',
            $n int from event.n,
            $raw json from event,
            $first text from event.items.\"0\".name = null)
          on hook github
        as insert seen {key: $key, at: $at, kind: $kind, n: $n, raw: $raw, first: $first}",
    )
    .unwrap();
    let take = |db: &mut Database, json: &str, key: &str, kind: Option<&str>| {
        let body = fennec_core::json::parse_json(json).unwrap();
        let e = Event {
            hook: "github",
            key,
            body: &body,
            digest: 1,
            at: Some(t0() - MINUTE),
            event: kind,
            keep: DAY,
        };
        db.take_event(&e)
    };
    // `hook.id`, `hook.at`, `hook.event`, a path, the body whole, and a
    // list's first element by its place.
    let json = r#"{"n": 3, "items": [{"name": "a"}, {"name": "b"}]}"#;
    assert_eq!(
        take(&mut db, json, "d-1", Some("push")).unwrap(),
        Took::Ran(1)
    );
    let row = rows(&db, "get seen select key, at, kind, n, first");
    assert_eq!(
        row,
        [vec![
            Value::Text("d-1".into()),
            Value::Timestamp(t0() - MINUTE),
            Value::Text("push".into()),
            Value::Int(3),
            Value::Text("a".into()),
        ]]
    );
    assert_eq!(
        rows(&db, "get seen select raw")[0][0],
        fennec_core::json::parse_json(json).unwrap(),
        "the body whole, as it came"
    );
    // A default stands in for a value missing; none, the event is refused.
    take(&mut db, r#"{"n": 4}"#, "d-2", None).unwrap();
    assert_eq!(
        rows(&db, "get seen select kind, first where key = 'd-2'"),
        [vec![Value::Text("none".into()), Value::Null]]
    );
    for (json, words) in [
        (
            r#"{"m": 1}"#,
            "`$n`, int, reads `event.n`, which the event does not hold",
        ),
        (
            r#"{"n": null}"#,
            "`$n`, int, reads `event.n`, which the event does not hold",
        ),
        // No cast: a text of digits is no `int`.
        (r#"{"n": "5"}"#, "`$n` is int, and `event.n` holds text"),
        (
            r#"{"n": 5, "items": {"0": {"name": 7}}}"#,
            "`$first` is text, and `event.items.0.name` holds int",
        ),
    ] {
        let e = take(&mut db, json, "d-3", None).unwrap_err();
        assert!(
            matches!(&e, Error::Query(m) if m.contains(words)),
            "{json}: {e}"
        );
    }
    assert_eq!(
        rows(&db, "get seen count"),
        [vec![Value::Int(2)]],
        "nothing of a refused event written"
    );
    // A key or a body no event has.
    for (json, key) in [
        (r#"{"n": 1}"#, ""),
        (r#"{"n": 1}"#, "a\u{1}b"),
        (r#"[1]"#, "k"),
    ] {
        let e = take(&mut db, json, key, None).unwrap_err();
        assert!(matches!(e, Error::Query(_)), "{json} {key:?}: {e}");
    }
    let long = "k".repeat(256);
    assert!(take(&mut db, r#"{"n": 1}"#, &long, None).is_err());
    assert_eq!(
        take(&mut db, r#"{"n": 1}"#, &long[..255], None).unwrap(),
        Took::Ran(1)
    );
}

/// The decision slice 1 left: a key of digits indexes a list, by RFC
/// 6901's places, and names an object's member of that name.
#[test]
fn a_key_of_digits_indexes_a_list_and_names_a_member() {
    let body = fennec_core::json::parse_json(
        r#"{"items": [{"id": "a"}, {"id": "b"}], "map": {"0": "zero", "01": "one"},
            "deep": [[1, 2], [3]]}"#,
    )
    .unwrap();
    for (keys, want) in [
        ("items.0.id", Value::Text("a".into())),
        ("items.1.id", Value::Text("b".into())),
        ("items.2.id", Value::Null),
        ("items.01.id", Value::Null),
        ("items.-1.id", Value::Null),
        ("items.99999999999999999999999.id", Value::Null),
        ("items.id", Value::Null),
        ("map.0", Value::Text("zero".into())),
        ("map.01", Value::Text("one".into())),
        ("deep.0.1", Value::Int(2)),
        ("deep.1.0", Value::Int(3)),
        ("deep.1.0.x", Value::Null),
    ] {
        assert_eq!(event_at(&body, keys), &want, "{keys}");
    }
}

#[test]
fn a_statement_broken_by_its_body_refuses_the_events_its_condition_takes() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    run(&mut db, "create collection notes (ref text @hash)").unwrap();
    run(
        &mut db,
        "create statement refunded($o text from event.data.object.metadata.order) on hook \
         stripe where event.type = 'charge.refunded' as insert notes {ref: $o}",
    )
    .unwrap();
    // An internal writer takes away what `stripe_paid` reads: marked
    // broken, never refused.
    db.internally(|db| run(db, "alter collection orders drop field currency"))
        .unwrap();
    let broken = db.statements().get("stripe_paid").unwrap();
    assert!(broken.broken.is_some() && broken.tree.is_some());
    let body = paid("evt_1", "o-1", 2000);
    let e = db.take_event(&event(&body, "evt_1", 1)).unwrap_err();
    assert!(
        matches!(&e, Error::Exists(m) if m.starts_with("statement `stripe_paid`, which takes \
            hook `stripe`, is broken: it reads `orders.currency`")),
        "{e}"
    );
    // An event its condition passes over is taken by the hook's others.
    let refund = fennec_core::json::parse_json(
        r#"{"id": "evt_r", "type": "charge.refunded",
            "data": {"object": {"metadata": {"order": "o-1"}}}}"#,
    )
    .unwrap();
    assert_eq!(
        db.take_event(&event(&refund, "evt_r", 1)).unwrap(),
        Took::Ran(1)
    );
    assert_eq!(rows(&db, "get notes count"), [vec![Value::Int(1)]]);
}

#[test]
fn check_holds_the_typed_values_and_now_is_the_taking() {
    let tap = Tap::default();
    let mut db = orders(&tap);
    run(&mut db, "create collection stamps (n int, at timestamp)").unwrap();
    for (name, n) in [("a_stamp", 1), ("b_stamp", 2)] {
        run(
            &mut db,
            &format!(
                "create statement {name}($n int from event.n) on hook clock check $n > 0 as \
                 insert stamps {{n: $n + {n}, at: now()}}"
            ),
        )
        .unwrap();
    }
    let body = fennec_core::json::parse_json(r#"{"n": 0}"#).unwrap();
    let mut e = event(&body, "k-0", 1);
    e.hook = "clock";
    let err = db.take_event(&e).unwrap_err();
    assert!(
        matches!(&err, Error::Query(m) if m == "statement `a_stamp`: its `check` does not hold \
            for this event"),
        "{err}"
    );
    let body = fennec_core::json::parse_json(r#"{"n": 1}"#).unwrap();
    let mut e = event(&body, "k-1", 1);
    e.hook = "clock";
    assert_eq!(db.take_event(&e).unwrap(), Took::Ran(2));
    let at = rows(&db, "get stamps select at");
    assert_eq!(
        at,
        [vec![Value::Timestamp(t0())], vec![Value::Timestamp(t0())]]
    );
}

/// One `--call-max-rows` for every statement an event wakes: each alone
/// within it, the two together past it.
#[test]
fn an_event_examines_at_most_call_max_rows_for_all_its_statements() {
    let tap = Tap::default();
    let mut db = orders(&tap);
    run(&mut db, "create collection marks (n int)").unwrap();
    for i in 0..3 {
        run(&mut db, &format!("put marks {{n: {i}}}")).unwrap();
    }
    for name in ["one", "two"] {
        run(
            &mut db,
            &format!(
                "create statement {name}($n int from event.n) on hook scan scans as set marks \
                 {{n: n + $n}} where n >= 0"
            ),
        )
        .unwrap();
    }
    db.set_call_max_rows(4);
    let body = fennec_core::json::parse_json(r#"{"n": 1}"#).unwrap();
    let mut e = event(&body, "k-1", 1);
    e.hook = "scan";
    let err = db.take_event(&e).unwrap_err();
    assert!(err.to_string().contains("--call-max-rows"), "{err}");
    db.set_call_max_rows(6);
    assert_eq!(db.take_event(&e).unwrap(), Took::Ran(2));
}

#[test]
fn an_event_s_write_wakes_a_binding_in_its_block() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    run(
        &mut db,
        "create collection ledger (ref text @hash, amount int)",
    )
    .unwrap();
    run(
        &mut db,
        "create statement paid_ledger after set orders where new.status = 'paid' and \
         old.status = 'open' as insert ledger {ref: new.ref, amount: new.total}",
    )
    .unwrap();
    let body = paid("evt_1", "o-1", 2000);
    db.take_event(&event(&body, "evt_1", 1)).unwrap();
    assert_eq!(
        rows(&db, "get ledger select amount where ref = 'o-1'"),
        [vec![Value::Int(2000)]]
    );
    let (_, record) = tap.last();
    assert_eq!(
        kinds_in(&record).len(),
        4,
        "the order, the ledger's line, the collection and the key: one record"
    );
}

/// A replica refuses writes, and a storage error stops them: an event
/// its statements would take is refused as any write is, before a body
/// runs -- nothing written, no key kept -- in the block a caller opened as
/// in one of its own.
#[test]
fn a_replica_and_a_failed_database_take_no_event() {
    let body = paid("evt_1", "o-1", 2000);
    let tap = Tap::default();
    let mut db = defined(&tap);
    db.follow(vec![(7, 0)]).unwrap();
    let seq = db.change_seq();
    let err = db.take_event(&event(&body, "evt_1", 1)).unwrap_err();
    assert!(matches!(err, Error::ReadOnly(_)), "{err}");
    assert_eq!(db.change_seq(), seq, "nothing written");
    assert_eq!(status(&db, "o-1"), Value::Text("open".into()));
    assert!(keys(&db).is_empty());

    let tap = Tap::default();
    let mut db = defined(&tap);
    db.fail(&Error::Io("the disk went away".into()));
    let seq = db.change_seq();
    let err = db.take_event(&event(&body, "evt_1", 1)).unwrap_err();
    assert!(matches!(err, Error::Io(_)), "{err}");
    db.begin().unwrap();
    let err = db.take_event(&event(&body, "evt_1", 1)).unwrap_err();
    assert!(matches!(err, Error::Io(_)), "{err}");
    assert_eq!(db.block_writes(), 0, "nothing run in the caller's block");
    db.rollback();
    assert_eq!(db.change_seq(), seq, "nothing written");
    assert_eq!(status(&db, "o-1"), Value::Text("open".into()));
    assert!(keys(&db).is_empty());
}

/// `now()` is the event's taking on the system's clock as on a pinned one:
/// one instant for every `check`, every body and the key's `until`,
/// however long the taking runs. `a_slow`'s parameter, a list long enough
/// that copying it as it is typed takes milliseconds, holds the taking
/// past the millisecond the event came in before `b_mark` is judged and
/// before any body runs; `b_mark`'s `check` holds only in that millisecond
/// (`now() < $cut`). A `check` reading the clock as it is asked refuses
/// every event, and a body reading it again writes a later time than the
/// key's `until` was kept by.
#[test]
fn now_is_one_instant_of_the_system_s_clock_for_check_and_bodies() {
    use fennec_core::time::now_ms;
    use std::time::{Duration, Instant};
    let tap = Tap::default();
    let mut db = orders(&tap);
    db.set_clock(None);
    run(&mut db, "create collection stamps (n int, at timestamp)").unwrap();
    for text in [
        "create statement a_slow($big json from event.big) on hook clock as insert stamps {n: 1, \
         at: now()}",
        "create statement b_mark($cut timestamp from event.cut) on hook clock check now() < $cut \
         as insert stamps {n: 2, at: now()}",
        "create statement c_mark on hook clock as insert stamps {n: 3, at: now()}",
    ] {
        run(&mut db, text).unwrap();
    }
    // A list whose copy takes 2 ms here, debug build or release.
    let mut n = 1i64 << 14;
    let big = loop {
        let big = Value::List((0..n).map(Value::Int).collect());
        let t = Instant::now();
        std::hint::black_box(big.clone());
        if t.elapsed() >= Duration::from_millis(2) || n >= 1 << 23 {
            break big;
        }
        n *= 2;
    };
    // Its members in order, as a parsed object holds them.
    let mut body = Value::Object(vec![("big".into(), big), ("cut".into(), Value::Int(0))]);
    for attempt in 0..20u64 {
        // A millisecond just begun: the taking reads it, and `check` holds,
        // unless this thread is put off between the two -- then nothing is
        // written, and the event goes again under another key.
        let start = now_ms().unwrap();
        let mut came = start;
        while came == start {
            came = now_ms().unwrap();
        }
        if let Value::Object(fields) = &mut body {
            fields[1].1 = Value::Int(came + 1);
        }
        let key = format!("k-{attempt}");
        let e = Event {
            hook: "clock",
            key: &key,
            body: &body,
            digest: attempt,
            at: None,
            event: None,
            keep: DAY,
        };
        match db.take_event(&e) {
            Ok(took) => assert_eq!(took, Took::Ran(3)),
            Err(Error::Query(m)) if m.contains("its `check` does not hold") => {
                assert_eq!(rows(&db, "get stamps count"), [vec![Value::Int(0)]]);
                continue;
            }
            Err(err) => panic!("{err}"),
        }
        let done = now_ms().unwrap();
        let until = rows(&db, "get _hook_events select until");
        let Value::Timestamp(until) = until[0][0] else {
            panic!("{until:?}")
        };
        let taken = until - DAY;
        assert!(came <= taken && taken <= done, "{came} {taken} {done}");
        assert_eq!(
            rows(&db, "get stamps select n, at order n"),
            (1..=3)
                .map(|n| vec![Value::Int(n), Value::Timestamp(taken)])
                .collect::<Vec<_>>(),
            "every `now()` the taking, the one `check` held at: before {}",
            came + 1
        );
        // The clock given back: a statement after it reads its own time.
        while now_ms().unwrap() <= taken {
            std::hint::spin_loop();
        }
        run(&mut db, "insert stamps {n: 4, at: now()}").unwrap();
        let after = rows(&db, "get stamps select at where n = 4");
        assert!(
            matches!(after[0][0], Value::Timestamp(t) if t > taken),
            "{after:?}"
        );
        return;
    }
    panic!(
        "every event refused by `b_mark`'s `check`: a taking never began in the millisecond its \
         event came in, or `check` read a later clock than the taking's"
    );
}

/// A statement that panics in a hook's code, run for an event in a block
/// its caller opened, unwinds out of `take_event` with the rows its write
/// woke kept -- no `whole` around it to let them go. The block put back,
/// the next event, writing nothing a statement is bound to, runs no
/// binding of those rows.
#[test]
fn rows_a_panic_left_woken_wake_nothing_in_the_next_event() {
    struct Boom;
    impl Hook for Boom {
        fn name(&self) -> &str {
            "boom"
        }
        fn rewrites(&self) -> bool {
            false
        }
        fn before_write(&self, _: &Schema, _: WriteOp, doc: &mut Document) -> Result<()> {
            assert_ne!(doc.get("n"), Some(&Value::Int(11)), "a hook that panics");
            Ok(())
        }
    }
    impl Plugin for Boom {
        fn name(&self) -> &str {
            "boom"
        }
        fn init(&self, reg: &mut Registry) -> Result<()> {
            reg.register_hook(Arc::new(Boom));
            Ok(())
        }
    }
    let tap = Tap::default();
    let mut db = orders(&tap);
    for text in [
        "create collection marks (n int)",
        "create collection notes (n int)",
        "create collection log (n int)",
        "put marks [{n: 0}, {n: 10}]",
        "create statement logged after set marks as insert log {n: new.n}",
        "create statement a_bump on hook mix where event.kind = 'bump' scans as set marks {n: n \
         + 1} where n >= 0",
        "create statement b_note on hook mix where event.kind = 'note' as insert notes {n: 1}",
    ] {
        run(&mut db, text).unwrap();
    }
    db.install_plugin(&Boom).unwrap();
    let bump = fennec_core::json::parse_json(r#"{"kind": "bump"}"#).unwrap();
    let mut e = event(&bump, "k-bump", 1);
    e.hook = "mix";
    db.begin().unwrap();
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = db.take_event(&e);
    }));
    assert!(unwound.is_err(), "the second row's hook panics");
    db.rollback();
    let note = fennec_core::json::parse_json(r#"{"kind": "note"}"#).unwrap();
    let mut e = event(&note, "k-note", 2);
    e.hook = "mix";
    assert_eq!(db.take_event(&e).unwrap(), Took::Ran(1));
    assert_eq!(rows(&db, "get notes count"), [vec![Value::Int(1)]]);
    assert_eq!(
        rows(&db, "get log count"),
        [vec![Value::Int(0)]],
        "no binding of the rows the panic left"
    );
    assert_eq!(
        rows(&db, "get marks select n order n"),
        [vec![Value::Int(0)], vec![Value::Int(10)]]
    );
}

// ------------------------------------------------------------ the fixture

/// `file` with `stripe_paid` after its records, once, as this build writes
/// it, over an order of its own, and an event it took, its write and its
/// key in one block: what `tests/statements.fennec` gained in F4's slice 2,
/// which the browser module opens and keeps through its images
/// (`web/fennec.test.js`).
pub(crate) fn with_hook(file: Vec<u8>) -> Vec<u8> {
    let tap = Tap {
        file: Arc::new(std::sync::Mutex::new(file.clone())),
        writes: Arc::default(),
    };
    let mut db = tap.database();
    db.load(&file).unwrap();
    if db.statements().get("stripe_paid").is_some() {
        return tap.bytes();
    }
    db.set_clock(Some(t0()));
    db.allow_hooks();
    run(&mut db, ORDERS).unwrap();
    run(
        &mut db,
        "put orders {ref: 'o-1', total: 2000, currency: 'eur', status: 'open'}",
    )
    .unwrap();
    run(&mut db, STRIPE_PAID).unwrap();
    let body = paid("evt_1", "o-1", 2000);
    assert_eq!(
        db.take_event(&event(&body, "evt_1", 7)).unwrap(),
        Took::Ran(1)
    );
    tap.bytes()
}

/// `tests/statements.fennec` holds a hook after the schedule, and an event
/// it took: every build reads them and keeps them through its images.
#[test]
fn the_fixture_s_hook_and_its_event_are_read_and_kept() {
    let file = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/statements.fennec"
    ))
    .expect("the fixture");
    let mut db = opened(&file);
    let s = db
        .statements()
        .get("stripe_paid")
        .expect("the hook")
        .clone();
    assert!(s.tree.is_some() && s.broken.is_none(), "{:?}", s.broken);
    assert_eq!(s.hook.as_deref(), Some("stripe"));
    assert_eq!(status(&db, "o-1"), Value::Text("paid".into()));
    db.set_clock(Some(t0()));
    let body = paid("evt_1", "o-1", 2000);
    assert_eq!(
        db.event_kept(&event(&body, "evt_1", 7)).unwrap(),
        Some(HeldKey {
            taken: 1,
            same: true,
            seq: db.change_seq(),
        })
    );
    let again = opened(&db.snapshot());
    assert_eq!(again.statements().get("stripe_paid"), Some(&s));
}

/// `statements` and `describe statement` as a row of `text` reads: each
/// column by its name.
fn listed(db: &mut Database, text: &str, name: &str) -> Vec<(String, Value)> {
    let r = run(db, text).unwrap();
    let rs = r.rows().unwrap();
    let row = rs
        .rows
        .iter()
        .find(|r| r.values[0] == Value::Text(name.into()))
        .unwrap_or_else(|| panic!("no {name} in {rs:?}"));
    rs.columns.iter().cloned().zip(row.values.clone()).collect()
}

fn column<'a>(row: &'a [(String, Value)], c: &str) -> &'a Value {
    &row.iter()
        .find(|(n, _)| n == c)
        .unwrap_or_else(|| panic!("no column {c}"))
        .1
}

/// The listings show a webhook consumer's hook, from its record, and
/// whether the server holds a secret for it -- `false` where no server
/// told the database, as the shell's and the native library's take no
/// event -- and `describe statement` its condition as written and what
/// each parameter reads (`docs/design/named-statements.md`, *Watching
/// hooks*). A statement taking no hook is listed with neither.
#[test]
fn the_listings_show_a_hook_and_whether_it_is_armed() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    run(
        &mut db,
        "create statement totals() as get orders select id where ref = 'o-1'",
    )
    .unwrap();
    let row = listed(&mut db, "statements", "stripe_paid");
    assert_eq!(column(&row, "hook"), &Value::Text("stripe".into()));
    assert_eq!(column(&row, "armed"), &Value::Bool(false));
    assert!(row.iter().all(|(c, _)| c != "condition"), "{row:?}");
    let other = listed(&mut db, "statements", "totals");
    assert_eq!(column(&other, "hook"), &Value::Null);
    assert_eq!(column(&other, "armed"), &Value::Null);

    // A server holding the hook's secret says so, asked as it lists.
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let held = Arc::clone(&armed);
    db.arm_hooks(Arc::new(move |h| {
        h == "stripe" && held.load(std::sync::atomic::Ordering::Relaxed)
    }));
    let row = listed(&mut db, "statements", "stripe_paid");
    assert_eq!(column(&row, "armed"), &Value::Bool(true));
    armed.store(false, std::sync::atomic::Ordering::Relaxed);
    let row = listed(&mut db, "describe statement stripe_paid", "stripe_paid");
    assert_eq!(column(&row, "armed"), &Value::Bool(false));
    assert_eq!(column(&row, "hook"), &Value::Text("stripe".into()));
    assert_eq!(
        column(&row, "condition"),
        &Value::Text("event.type = 'payment_intent.succeeded' and event.livemode = true".into())
    );
    assert_eq!(
        column(&row, "signature"),
        &Value::Text(
            "($intent text from event.data.object.id, $order text from \
             event.data.object.metadata.order, $amount int from \
             event.data.object.amount_received, $currency text from event.data.object.currency)"
                .into()
        )
    );
    // `hook.` and the whole body, and a key no name writes, as the event's
    // JSON names it; no condition, none shown.
    run(
        &mut db,
        "create statement kept($e json from event, $id text from hook.id, $at timestamp from \
         hook.at, $o text from event.metadata.\"order-id\") on hook stripe as insert orders \
         {ref: $o, intent: $id, at: $at}",
    )
    .unwrap();
    let row = listed(&mut db, "describe statement kept", "kept");
    assert_eq!(
        column(&row, "signature"),
        &Value::Text(
            "($e json from event, $id text from hook.id, $at timestamp from hook.at, $o text \
             from event.metadata.order-id)"
                .into()
        )
    );
    assert_eq!(column(&row, "condition"), &Value::Null);
    let other = listed(&mut db, "describe statement totals", "totals");
    assert_eq!(column(&other, "signature"), &Value::Text("()".into()));
    assert_eq!(column(&other, "condition"), &Value::Null);
}

/// What an event's taking ran is noted on the thread, statement by
/// statement -- its version, the rows its last statement answered, its
/// time -- and the one that refused it, for a server to count by the shape
/// `hook <name>`, write on the slow line and trace (`Database::take_runs`);
/// an event no condition takes, or one answered from its key, runs none and
/// notes none.
#[test]
fn an_event_s_taking_is_noted_statement_by_statement() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    run(
        &mut db,
        "create collection ledger (ref text @hash, amount int)",
    )
    .unwrap();
    run(
        &mut db,
        "create statement a_note($order text from event.data.object.metadata.order) on hook \
         stripe as insert ledger {ref: $order, amount: 0}",
    )
    .unwrap();
    drop(Database::take_runs());
    let body = paid("evt_1", "o-1", 2000);
    assert_eq!(
        db.take_event(&event(&body, "evt_1", 1)).unwrap(),
        Took::Ran(2)
    );
    assert!(Database::runs_noted());
    let t = Database::take_runs().unwrap().taking.expect("the taking");
    assert_eq!(t.hook, "stripe");
    let ran: Vec<_> = t.ran.iter().map(|r| (r.name.as_str(), r.rows)).collect();
    assert_eq!(ran, [("a_note", 1), ("stripe_paid", 1)]);
    for r in &t.ran {
        let listed = db.statements().get(&r.name).map(|s| s.version);
        assert_eq!(Some(r.version), listed);
    }
    assert!(t.refusal.is_none());
    assert!(!Database::runs_noted());

    // Answered from its key: nothing ran, nothing noted.
    assert!(matches!(
        db.take_event(&event(&body, "evt_1", 1)).unwrap(),
        Took::Held(_)
    ));
    assert!(Database::take_runs().is_none());

    // The second refusing: both ran, the second failed and noted so.
    run(
        &mut db,
        "put orders {ref: 'o-4', total: 9, currency: 'eur', status: 'paid'}",
    )
    .unwrap();
    let body = paid("evt_4", "o-4", 9);
    let e = db.take_event(&event(&body, "evt_4", 1)).unwrap_err();
    assert!(matches!(&e, Error::Unmet(_)), "{e}");
    let t = Database::take_runs().unwrap().taking.expect("the taking");
    let ran: Vec<_> = t.ran.iter().map(|r| (r.name.as_str(), r.rows)).collect();
    assert_eq!(ran, [("a_note", 1), ("stripe_paid", 0)]);
    // The first counted as it ran, its row though put back; the second
    // refused.
    assert!(t.refused(0).is_none());
    assert_eq!(
        t.refused(1).and_then(|e| e.reason()),
        Some("no open order of that total")
    );
    let (at, _) = t.refusal.expect("the refusal");
    assert_eq!(at, Some(1));

    // One no condition takes runs nothing.
    let test = fennec_core::json::parse_json(
        r#"{"id": "evt_5", "type": "payment_intent.succeeded", "livemode": false}"#,
    )
    .unwrap();
    run(&mut db, "drop statement a_note").unwrap();
    drop(Database::take_runs());
    assert_eq!(
        db.take_event(&event(&test, "evt_5", 1)).unwrap(),
        Took::Passed
    );
    assert!(Database::take_runs().is_none());
}

/// A sink whose writes fail once its flag is set: a block's commit refused
/// after every statement of an event ran.
#[derive(Clone, Default)]
struct Failing(Arc<std::sync::atomic::AtomicBool>);

impl Failing {
    fn check(&self) -> Result<()> {
        if self.0.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(Error::Io("the disk went away".into()));
        }
        Ok(())
    }
}

impl Sink for Failing {
    fn append(&mut self, _: &[u8]) -> Result<()> {
        self.check()
    }
    fn record(&mut self, _: u64, _: &[u8]) -> Result<()> {
        self.check()
    }
    fn rewrite(&mut self, _: &[u8]) -> Result<()> {
        self.check()
    }
}

/// An event whose statements all ran and that then failed to land -- its
/// key's row refused, its block's commit -- is noted refused by each of
/// them, their rows `0`: none landed, and a server counting them as taken
/// would count rows no file holds.
#[test]
fn an_event_failing_to_land_is_noted_refused_by_each_statement() {
    let each = |t: &fennec_core::engine::HookRuns| -> Vec<(String, u64, bool)> {
        (t.ran.iter().enumerate())
            .map(|(i, r)| (r.name.clone(), r.rows, t.refused(i).is_some()))
            .collect()
    };
    // Its key's row refused, after both statements ran: a `_hook_events`
    // whose `digest` is unique, another key holding the event's.
    let tap = Tap::default();
    let mut db = defined(&tap);
    run(&mut db, "create collection ledger (ref text, amount int)").unwrap();
    run(
        &mut db,
        "create statement a_note($order text from event.data.object.metadata.order) on hook \
         stripe as insert ledger {ref: $order, amount: 0}",
    )
    .unwrap();
    run(
        &mut db,
        "create collection _hook_events (key text @hash, digest int @unique, taken int, until \
         timestamp)",
    )
    .unwrap();
    run(
        &mut db,
        "put _hook_events {key: 'another', digest: 1, taken: 1}",
    )
    .unwrap();
    drop(Database::take_runs());
    let body = paid("evt_1", "o-1", 2000);
    let err = db.take_event(&event(&body, "evt_1", 1)).unwrap_err();
    let t = Database::take_runs().unwrap().taking.expect("the taking");
    assert_eq!(
        each(&t),
        [("a_note".into(), 0, true), ("stripe_paid".into(), 0, true)]
    );
    let (at, noted) = t.refusal.as_ref().expect("the refusal");
    assert_eq!((*at, noted.to_string()), (None, err.to_string()));
    let Response::Rows(ledger) = run(&mut db, "get ledger").unwrap() else {
        panic!("rows")
    };
    assert!(ledger.rows.is_empty(), "put back: {ledger:?}");

    // Its block's commit refused.
    let failing = Failing::default();
    fennec_ql::install();
    let mut db = Database::with_sink(Box::new(failing.clone()));
    db.set_clock(Some(t0()));
    db.allow_hooks();
    run(&mut db, ORDERS).unwrap();
    run(
        &mut db,
        "put orders {ref: 'o-1', total: 2000, currency: 'eur', status: 'open'}",
    )
    .unwrap();
    run(&mut db, STRIPE_PAID).unwrap();
    drop(Database::take_runs());
    failing.0.store(true, std::sync::atomic::Ordering::Relaxed);
    let err = db.take_event(&event(&body, "evt_1", 1)).unwrap_err();
    assert!(matches!(err, Error::Io(_)), "{err}");
    let t = Database::take_runs().unwrap().taking.expect("the taking");
    assert_eq!(each(&t), [("stripe_paid".into(), 0, true)]);
    assert!(matches!(t.refusal, Some((None, Error::Io(_)))));
}

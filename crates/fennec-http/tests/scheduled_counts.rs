//! What a process tells of its scheduled statements' runs
//! (`docs/design/named-statements.md`, *Watching schedules*): `/_metrics`'
//! four families -- none before a look, each run counted once by what came
//! of it, the slots passed over, the marks found ahead of the clock and
//! how late runs began -- no label naming a tenant, a statement or a
//! collection; and `/_stats/statements`' shape `schedule <name>`, a
//! tenant's apart and the admin's alone across tenants. Apart: the counts
//! are the process's own.

use fennec_core::engine::Landing;
use fennec_core::prelude::*;
use fennec_http::schedule::{self, Node, Outcome};
use fennec_http::tenants::Tenants;
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const ROOT: &str = "root-token";
const ADMIN: &str = "admin-token";

fn utc(y: i64, mo: u32, d: u32, h: i64, mi: i64) -> i64 {
    (fennec_core::time::days_from_civil(y, mo, d) * 1440 + h * 60 + mi) * MINUTE
}

fn run(db: &mut Database, sql: &str) {
    for s in fennec_ql::parse(sql).unwrap_or_else(|e| panic!("{sql}: {e}")) {
        db.execute(&s).unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
}

/// A database whose clock read `at` as `texts` defined it.
fn defined_at(at: i64, texts: &[&str]) -> Arc<RwLock<Database>> {
    fennec_ql::install();
    let mut db = Database::new();
    db.allow_schedules();
    db.set_clock(Some(at));
    for t in texts {
        run(&mut db, t);
    }
    db.set_clock(None);
    Arc::new(RwLock::new(db))
}

fn served(server: Server) -> u16 {
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    port
}

fn get(port: u16, token: Option<&str>, target: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    write!(
        s,
        "GET {target} HTTP/1.1\r\nHost: x\r\n{auth}Connection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let body = out.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    (out[9..12].parse().unwrap(), body.to_string())
}

/// A scrape's sample `line` -- its name and labels as written -- read.
fn sample(text: &str, line: &str) -> f64 {
    let head = format!("{line} ");
    let l = (text.lines().find(|l| l.starts_with(&head)))
        .unwrap_or_else(|| panic!("no `{line}` in the scrape:\n{text}"));
    l[head.len()..].parse().unwrap()
}

/// The shape `query`'s entry of a `/_stats/statements` answer.
fn shape(stats: &str, query: &str) -> Value {
    let doc = fennec_core::json::parse_json(stats).unwrap();
    let member = |o: &Value, k: &str| match o {
        Value::Object(m) => {
            (m.iter().find(|(n, _)| n == k)).map_or(Value::Null, |(_, v)| v.clone())
        }
        _ => Value::Null,
    };
    let Value::List(all) = member(&doc, "statements") else {
        panic!("{stats}")
    };
    (all.into_iter())
        .find(|e| member(e, "query") == Value::Text(query.into()))
        .unwrap_or_else(|| panic!("no `{query}` in {stats}"))
}

fn field(entry: &Value, k: &str) -> Value {
    match entry {
        Value::Object(m) => {
            (m.iter().find(|(n, _)| n == k)).map_or(Value::Null, |(_, v)| v.clone())
        }
        _ => Value::Null,
    }
}

#[test]
fn a_run_is_counted_by_outcome_and_shaped_by_name_a_tenant_s_apart() {
    fennec_http::audit::set_delay(0);
    // A file's server holding no schedule scrapes as it did: nothing of the
    // scheduler until a look is made.
    let idle = Arc::new(RwLock::new(Database::new()));
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some(ROOT.into()),
        ..Config::default()
    };
    let file = served(Server::new(Arc::clone(&idle), cfg));
    let (status, text) = get(file, Some(ROOT), "/_metrics");
    assert_eq!(status, 200, "{text}");
    assert!(!text.contains("fennec_scheduled_"), "{text}");

    // Each outcome once, every slot a minute and a half late, two passed
    // over before it: a slot that ran, one the statement refused, one of a
    // statement broken, one the quota held, one that landed as its lease
    // lapsed; and a definition ahead of the clock.
    let t0 = utc(2026, 5, 4, 12, 0);
    let at = t0 + 45 * MINUTE + 90_000;
    let ticks = "create collection ticks (slot timestamp, n int @hash)";
    let db = defined_at(
        t0,
        &[
            ticks,
            "create collection gone (n int)",
            "create statement tock every 15m as insert ticks {slot: slot(), n: 1}",
            "create statement gatekeep every 15m as begin get ticks where n = -1 require 1 else \
             'no such tick'; insert ticks {slot: slot(), n: 2} end",
            "create statement orphan every 15m as insert gone {n: 1}",
        ],
    );
    let gone = fennec_ql::parse_one("drop collection gone").unwrap();
    db.write()
        .unwrap()
        .internally(|d| d.execute(&gone))
        .unwrap();
    let grow = "create statement sprout every 15m as insert ticks {slot: slot(), n: 3}";
    let held = defined_at(t0, &[ticks, grow]);
    held.write()
        .unwrap()
        .set_limit(Some(Arc::new(|_: &Database, l: &Landing| match l.grows {
            true => Err("rows: at most 0".into()),
            false => Ok(()),
        })));
    let lapsing = defined_at(t0, &[ticks, grow]);
    let lapsed = Arc::new(AtomicBool::new(false));
    {
        let mut g = lapsing.write().unwrap();
        let fence = Arc::clone(&lapsed);
        g.set_fence(Some(Arc::new(move || match fence.load(Ordering::SeqCst) {
            true => Err("the lease lapsed".into()),
            false => Ok(()),
        })));
        // Asked after the fence as the block lands: the lease lapses with
        // the run landed.
        let lapse = Arc::clone(&lapsed);
        g.set_limit(Some(Arc::new(move |_: &Database, _: &Landing| {
            lapse.store(true, Ordering::SeqCst);
            Ok(())
        })));
    }
    let ahead = defined_at(at + 30 * MINUTE, &[ticks, grow]);
    let outcomes = |db: &Arc<RwLock<Database>>| -> Vec<(String, Outcome)> {
        let look = schedule::pass(db, at);
        let mut runs: Vec<_> = look.runs.into_iter().map(|r| (r.name, r.outcome)).collect();
        runs.sort_by(|a, b| a.0.cmp(&b.0));
        runs
    };
    let all = |names: &[(&str, Outcome)]| -> Vec<(String, Outcome)> {
        names.iter().map(|(n, o)| (n.to_string(), *o)).collect()
    };
    assert_eq!(
        outcomes(&db),
        all(&[
            ("gatekeep", Outcome::Refused(412)),
            ("orphan", Outcome::Broken),
            ("tock", Outcome::Ran)
        ])
    );
    assert_eq!(outcomes(&held), all(&[("sprout", Outcome::Held(507))]));
    assert_eq!(outcomes(&lapsing), all(&[("sprout", Outcome::Ran)]));
    assert!(
        lapsed.load(Ordering::SeqCst),
        "the lease lapsed as it landed"
    );
    assert!(outcomes(&ahead).is_empty());

    // A tenant node's run, its tenant's name in no label.
    let dir = std::env::temp_dir().join(format!(
        "fennec-scheduled-counts-{}-{:?}",
        std::process::id(),
        Instant::now()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let tenants = Arc::new(Tenants::new(&dir).unwrap());
    let node = Node::attach(&tenants, false);
    if node.learning() {
        node.learn();
    }
    let now = fennec_core::time::now_ms().unwrap();
    let hour = now - now.rem_euclid(HOUR);
    {
        let t = tenants.create("acme-north").unwrap();
        let mut g = t.db.write().unwrap();
        g.set_clock(Some(hour + 1));
        run(
            &mut g,
            "create collection ticks_north (slot timestamp); create statement tick_north every \
             1h as insert ticks_north {slot: slot()}",
        );
        g.set_clock(None);
    }
    let runs = node.pass(hour + HOUR + 90_000);
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].1.outcome, Outcome::Ran);
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some(ROOT.into()),
        admin_token: Some(ADMIN.into()),
        ..Config::default()
    };
    let tenant_node = served(Server::with_tenants(Arc::clone(&tenants), cfg));

    for (port, token) in [(file, ROOT), (tenant_node, ADMIN)] {
        let (status, text) = get(port, Some(token), "/_metrics");
        assert_eq!(status, 200, "{text}");
        for (outcome, n) in [
            ("ran", 2.0),
            ("refused", 1.0),
            ("broken", 1.0),
            ("held", 1.0),
            ("unknown", 1.0),
        ] {
            let line = format!("fennec_scheduled_runs_total{{outcome=\"{outcome}\"}}");
            assert_eq!(sample(&text, &line), n, "{outcome}");
        }
        assert_eq!(sample(&text, "fennec_scheduled_skipped_total"), 8.0);
        assert_eq!(sample(&text, "fennec_scheduled_mark_ahead_total"), 1.0);
        // Five runs that did not hold, each a minute and a half late.
        let late = "fennec_scheduled_lateness_seconds";
        assert_eq!(sample(&text, &format!("{late}_count")), 5.0);
        assert_eq!(sample(&text, &format!("{late}_sum")), 450.0);
        assert_eq!(sample(&text, &format!("{late}_bucket{{le=\"60\"}}")), 0.0);
        assert_eq!(sample(&text, &format!("{late}_bucket{{le=\"120\"}}")), 5.0);
        // No label names a tenant, a statement or a collection.
        let scheduled = text.lines().filter(|l| l.starts_with("fennec_scheduled_"));
        for l in scheduled {
            let labels = l.split_once('{').map_or("", |(_, rest)| rest);
            assert!(
                labels.is_empty() || labels.starts_with("outcome=") || labels.starts_with("le="),
                "{l}"
            );
        }
        for word in ["north", "tock", "gatekeep", "orphan", "sprout"] {
            assert!(!text.contains(word), "{word} in the scrape:\n{text}");
        }
    }

    // `/_stats/statements`: each run counted as its schedule's shape, its
    // rows what its last statement answered, a refusal an error.
    let (status, stats) = get(file, Some(ROOT), "/_stats/statements");
    assert_eq!(status, 200, "{stats}");
    let tick = shape(&stats, "schedule tock");
    assert_eq!(
        (
            field(&tick, "calls"),
            field(&tick, "rows"),
            field(&tick, "errors")
        ),
        (Value::Int(1), Value::Int(1), Value::Int(0))
    );
    for refused in ["schedule gatekeep", "schedule orphan"] {
        let e = shape(&stats, refused);
        assert_eq!(field(&e, "errors"), Value::Int(1), "{refused}");
    }
    let grow = shape(&stats, "schedule sprout");
    assert_eq!(
        (field(&grow, "calls"), field(&grow, "errors")),
        (Value::Int(2), Value::Int(1)),
        "a run held and one landed as its lease lapsed"
    );
    assert!(
        !stats.contains("north"),
        "a tenant's shape is its own: {stats}"
    );
    let (status, body) = get(file, None, "/_stats/statements");
    assert_eq!(status, 401, "{body}");
    // On the node: the tenant's under its prefix, and every tenant's,
    // named, the admin's alone.
    let (status, own) = get(tenant_node, Some(ROOT), "/t/acme-north/_stats/statements");
    assert_eq!(status, 200, "{own}");
    let e = shape(&own, "schedule tick_north");
    assert_eq!(
        (field(&e, "calls"), field(&e, "rows")),
        (Value::Int(1), Value::Int(1))
    );
    assert!(!own.contains("schedule tock"), "{own}");
    let (status, every) = get(tenant_node, Some(ADMIN), "/_stats/statements");
    assert_eq!(status, 200, "{every}");
    let e = shape(&every, "schedule tick_north");
    assert_eq!(field(&e, "tenant"), Value::Text("acme-north".into()));
    let (status, body) = get(tenant_node, Some(ROOT), "/_stats/statements");
    assert_eq!(status, 401, "{body}");
    drop(node);
    let _ = std::fs::remove_dir_all(&dir);
}

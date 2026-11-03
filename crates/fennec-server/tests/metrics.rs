//! `/_metrics` and `--slow-ms` on a running `fennec-server`: what clients
//! did, counted by whether it wrote; a histogram Prometheus can take apart;
//! the token the scrape needs; and a slow statement in the log with its
//! text, while a fast one stays out of it.

#[path = "support.rs"]
mod support;

use std::collections::HashMap;
use std::time::{Duration, Instant};
use support::{start, tmp, Http, Server};

const TOKEN: &str = "metrics-token";

struct Node {
    server: Server,
    metrics: u16,
}

fn node(name: &str, extra: &[&str]) -> Node {
    node_at(&tmp("metrics", name), extra)
}

/// [`node`] over a file made already.
fn node_at(path: &std::path::Path, extra: &[&str]) -> Node {
    let mut args = vec![
        "--metrics",
        "127.0.0.1:0",
        "--http-token",
        TOKEN,
        "--file",
        path.to_str().unwrap(),
    ];
    args.extend(extra);
    let server = start(&args);
    assert!(server.logged("metrics on: http://", Duration::from_secs(10)));
    let log = server.log.lock().unwrap().clone();
    let line = log
        .lines()
        .find(|l| l.contains("metrics on: http://"))
        .unwrap();
    let rest = line.split("127.0.0.1:").nth(1).unwrap();
    let metrics = rest
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap();
    Node { server, metrics }
}

impl Node {
    fn ask(
        &self,
        port: u16,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: &str,
    ) -> (u16, String) {
        let mut c = Http::open(port);
        c.token = token.map(String::from);
        let a = c.ask(method, path, body);
        (a.status, a.body)
    }

    /// Every sample of a scrape, by its name and labels as written.
    fn scrape(&self) -> HashMap<String, f64> {
        let (status, text) = self.ask(self.metrics, "GET", "/_metrics", Some(TOKEN), "");
        assert_eq!(status, 200, "{text}");
        text.lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .map(|l| {
                let (k, v) = l.rsplit_once(' ').expect(l);
                (k.to_string(), v.parse().expect(l))
            })
            .collect()
    }
}

fn value(m: &HashMap<String, f64>, key: &str) -> f64 {
    *m.get(key)
        .unwrap_or_else(|| panic!("no `{key}` in the scrape"))
}

#[test]
fn reads_and_writes_are_counted_apart() {
    let s = node("counted.fennec", &[]);
    let mut c = s.server.http().with_token(TOKEN);
    c.run("create collection t (name text, n int)");
    for i in 0..3 {
        c.run(&format!("put t {{name: \"n{i}\", n: {i}}}"));
    }
    c.run("get t order name");
    assert!(c.query("get t wher n = 1").is_err());
    assert_eq!(c.ask("POST", "/t", r#"{"name":"h","n":9}"#).status, 201);
    assert_eq!(c.ask("GET", "/t?n=eq.9", "").status, 200);
    assert_eq!(c.ask("GET", "/nosuch", "").status, 404);
    // A read over /query is a read, though it is a POST.
    c.run("get t count");

    let m = s.scrape();
    let n = |k: &str| value(&m, k);
    assert_eq!(n(r#"fennec_statements_total{kind="write"}"#), 5.0);
    // The statement with the typo is counted, as a read: it never got as
    // far as saying what it would do.
    assert_eq!(n(r#"fennec_statements_total{kind="read"}"#), 5.0);
    assert_eq!(n(r#"fennec_statement_errors_total{kind="read"}"#), 2.0);
    assert_eq!(n(r#"fennec_statement_errors_total{kind="write"}"#), 0.0);
    assert_eq!(n(r#"fennec_documents{collection="t"}"#), 4.0);
    assert_eq!(n("fennec_storage_failed"), 0.0);
    assert!(n("fennec_change_sequence") >= 5.0);
    assert!(n("fennec_memory_bytes") > 0.0);
    // The client is still connected; a scrape is nobody's connection.
    assert_eq!(n("fennec_connections"), 1.0);

    // What Prometheus needs of a histogram: buckets that only grow, the last
    // one the count, and a sum.
    for k in ["read", "write"] {
        let labels = format!(r#"kind="{k}""#);
        let mut buckets: Vec<(f64, f64)> = m
            .iter()
            .filter_map(|(key, v)| {
                let rest = key.strip_prefix(&format!(
                    "fennec_statement_duration_seconds_bucket{{{labels},le=\""
                ))?;
                let le = rest.strip_suffix("\"}")?;
                Some((
                    if le == "+Inf" {
                        f64::INFINITY
                    } else {
                        le.parse().unwrap()
                    },
                    *v,
                ))
            })
            .collect();
        buckets.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(buckets.len(), 17, "{labels}");
        assert!(
            buckets.windows(2).all(|w| w[0].1 <= w[1].1),
            "{labels}: {buckets:?}"
        );
        let count = n(&format!(
            "fennec_statement_duration_seconds_count{{{labels}}}"
        ));
        assert_eq!(buckets.last().unwrap().1, count, "{labels}");
        assert_eq!(n(&format!("fennec_statements_total{{{labels}}}")), count);
        let sum = n(&format!(
            "fennec_statement_duration_seconds_sum{{{labels}}}"
        ));
        assert_eq!(sum > 0.0, count > 0.0, "{labels}");
    }

    drop(c);
    let deadline = Instant::now() + Duration::from_secs(10);
    while value(&s.scrape(), "fennec_connections") != 0.0 {
        assert!(
            Instant::now() < deadline,
            "the closed connection is still counted"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_scrape_takes_the_servers_tokens() {
    let s = node("tokens.fennec", &["--admin-token", "admin-token"]);
    for port in [s.metrics, s.server.port] {
        assert_eq!(s.ask(port, "GET", "/_metrics", None, "").0, 401);
        assert_eq!(s.ask(port, "GET", "/_metrics", Some("wrong"), "").0, 401);
        for token in [TOKEN, "admin-token"] {
            let (status, body) = s.ask(port, "GET", "/_metrics", Some(token), "");
            assert_eq!(status, 200);
            assert!(
                body.contains("# TYPE fennec_statements_total counter"),
                "{body}"
            );
        }
    }
    // The metrics listener serves nothing else, token or not.
    assert_eq!(s.ask(s.metrics, "GET", "/t", Some(TOKEN), "").0, 404);
}

/// Which statements are slow is told by thresholds no statement can miss --
/// 0, every one, and a minute, none -- not by how long they take. At 20 ms
/// the test counted on a put of 20 000 rows taking longer and on the create
/// before it and a point read taking less, which a fast machine could undo
/// for the one and a loaded one did for the others: at utility QoS beside
/// eight busy loops the create took 47 ms and was the slow line found.
#[test]
fn a_slow_statement_is_logged_with_its_text() {
    let statements = |s: &Node| {
        let mut c = s.server.http().with_token(TOKEN);
        c.run("create collection t (name text, n int)");
        let docs: Vec<String> = (0..20_000)
            .map(|i| format!("{{name: \"row {i}\", n: {i}}}"))
            .collect();
        c.run(&format!("put t [{}]", docs.join(", ")));
        // A point read by id, with a mark to look for.
        c.run("get t where id = 987654");
    };

    // Every statement is logged, the put's megabyte cut short.
    let s = node("slow.fennec", &["--slow-ms", "0"]);
    statements(&s);
    let deadline = Instant::now() + Duration::from_secs(10);
    let log = loop {
        let log = s.server.log.lock().unwrap().clone();
        if log.contains("987654") {
            break log;
        }
        assert!(
            Instant::now() < deadline,
            "no slow line for the read in:\n{log}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let slow = |mark: &str| {
        log.lines()
            .find(|l| l.contains(r#""event":"slow""#) && l.contains(mark))
            .unwrap_or_else(|| panic!("no slow line holding {mark} in:\n{log}"))
    };
    // A JSON line; the request as it came: its line, then its body.
    let put = slow("put t [{name:");
    assert!(
        put.starts_with('{')
            && put.contains(r#""kind":"write""#)
            && put.contains(r#""statement":"POST /query "#),
        "{put}"
    );
    // Cut short: the statement is a megabyte, the line is not.
    assert!(put.len() < 1400 && put.ends_with(r#"..."}"#), "{put}");
    let read = slow("987654");
    assert!(read.contains(r#""kind":"read""#), "{read}");
    assert!(!read.ends_with(r#"..."}"#), "{read}");
    assert_eq!(value(&s.scrape(), "fennec_slow_statements_total"), 3.0);

    // Under a minute, the same statements stay out of the log: counted as
    // each is answered, before the scrape.
    let s = node("fast.fennec", &["--slow-ms", "60000"]);
    statements(&s);
    assert_eq!(value(&s.scrape(), "fennec_slow_statements_total"), 0.0);
    let log = s.server.log.lock().unwrap().clone();
    assert!(!log.contains(r#""event":"slow""#), "{log}");
}

/// The statements bound to writes: each run counted, a row a binding,
/// and each write one refused, with no label naming a binding or a
/// collection; and the slow line names the bindings a request woke, each
/// with its version and rows (`docs/design/named-statements.md`,
/// *Watching bindings*).
#[test]
fn a_binding_s_runs_and_refusals_are_counted_and_its_slow_line_names_it() {
    let s = node("bound.fennec", &["--slow-ms", "0"]);
    let mut c = s.server.http().with_token(TOKEN);
    c.run("create collection orders (status text, total int)");
    c.run("create collection ledger (o int @hash, amount int)");
    c.run(
        "create statement paid after set orders where new.status = 'paid' and old.status = 'open'
         as insert ledger {o: new.id, amount: new.total}",
    );
    c.run(
        "create statement voided after set orders where new.status = 'void' as begin
           get ledger select o where o = new.id limit 1 require 1 else 'no ledger line';
           insert ledger {o: new.id, amount: 0}
         end",
    );
    for t in [10, 20, 30] {
        c.run(&format!("put orders {{status: 'open', total: {t}}}"));
    }
    let before = s.scrape();
    assert_eq!(value(&before, "fennec_bound_runs_total"), 0.0);
    assert_eq!(value(&before, "fennec_bound_refused_total"), 0.0);
    c.run("set orders {status: 'paid'} where status = 'open'");
    c.run("put orders {status: 'open', total: 40}");
    let (status, body) = c
        .query("set orders {status: 'void'} where id = 4")
        .unwrap_err();
    assert_eq!(status, 412, "{body}");
    let after = s.scrape();
    // Three rows paid, and the one void whose guard refused it: a run
    // each, and one refusal.
    assert_eq!(value(&after, "fennec_bound_runs_total"), 4.0);
    assert_eq!(value(&after, "fennec_bound_refused_total"), 1.0);
    assert!(after
        .keys()
        .filter(|k| k.starts_with("fennec_bound_"))
        .all(|k| !k.contains('{')));

    let deadline = Instant::now() + Duration::from_secs(10);
    let log = loop {
        let log = s.server.log.lock().unwrap().clone();
        if log.contains(r#""bound":[["voided""#) {
            break log;
        }
        assert!(
            Instant::now() < deadline,
            "no slow line naming `voided` in:\n{log}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let paid = log
        .lines()
        .find(|l| l.contains(r#""event":"slow""#) && l.contains(r#""bound":[["paid",["#))
        .unwrap_or_else(|| panic!("no slow line naming `paid` in:\n{log}"));
    assert!(paid.contains(r#""kind":"write""#), "{paid}");
    assert!(
        paid.ends_with(",3]]}"),
        "its version and three rows: {paid}"
    );
    let plain = log
        .lines()
        .find(|l| l.contains(r#""event":"slow""#) && l.contains("total: 40"))
        .unwrap();
    assert!(
        !plain.contains(r#""bound""#),
        "a write waking nothing: {plain}"
    );
}

/// A scheduled statement's run past `--slow-ms` writes the slow line, its
/// `statement` the shape `/_stats/statements` counts it as, `schedule
/// <name>`, its `scheduled` the statement, the version that ran, the slot
/// and its rows, the bindings its writes woke beside it, and its
/// request id the run's own; `/_metrics` counts the runs by outcome, the
/// slots they passed over and how late they began
/// (`docs/design/named-statements.md`, *Watching schedules*).
#[test]
fn a_scheduled_run_is_counted_and_its_slow_line_names_it() {
    const HOUR: i64 = 3_600_000;
    // Every hour, defined a day and a half hour ago: the first look runs
    // each statement's latest slot, the 23 before it passed over.
    let path = tmp("metrics", "scheduled.fennec");
    fennec_ql::install();
    let since = fennec_core::time::now_ms().unwrap() - 24 * HOUR - HOUR / 2;
    {
        let mut db = fennec_core::fs::open(&path).unwrap();
        db.allow_schedules();
        db.set_clock(Some(since));
        for q in [
            "create collection ticks (slot timestamp, at timestamp)",
            "create collection logs (n int)",
            "create statement noted after insert ticks as insert logs {n: 1}",
            "create statement tick every 1h as insert ticks {slot: slot(), at: now()}",
            "create statement gatekeep every 1h scans as begin get ticks where slot = \
             timestamp(0) require 1 else 'no tick at the epoch'; insert ticks {slot: slot(), \
             at: now()} end",
        ] {
            db.execute(&fennec_ql::parse_one(q).unwrap())
                .unwrap_or_else(|e| panic!("{q}: {e}"));
        }
        if let Some(sync) = db.flush().unwrap() {
            sync().unwrap();
        }
    }
    let s = node_at(&path, &["--slow-ms", "0"]);
    let deadline = Instant::now() + Duration::from_secs(20);
    let log = loop {
        let log = s.server.log.lock().unwrap().clone();
        let lines = |n: &str| log.contains(&format!(r#""scheduled":["{n}",["#));
        if lines("tick") && lines("gatekeep") {
            break log;
        }
        assert!(Instant::now() < deadline, "no run's slow line in:\n{log}");
        std::thread::sleep(Duration::from_millis(20));
    };
    let line = |n: &str| {
        (log.lines())
            .find(|l| l.contains(&format!(r#""scheduled":["{n}",["#)))
            .unwrap()
            .to_string()
    };
    let tick = line("tick");
    for part in [
        r#""event":"slow""#,
        r#""kind":"write""#,
        r#""failed":false"#,
        r#""statement":"schedule tick""#,
        r#""request_id":""#,
        r#":00:00Z",1],"bound":[["noted",["#,
    ] {
        assert!(tick.contains(part), "{part} in {tick}");
    }
    assert!(tick.ends_with(",1]]}"), "a binding's run: {tick}");
    let gatekeep = line("gatekeep");
    for part in [
        r#""failed":true"#,
        r#""statement":"schedule gatekeep""#,
        r#":00:00Z",0]}"#,
    ] {
        assert!(gatekeep.contains(part), "{part} in {gatekeep}");
    }

    let m = s.scrape();
    for (outcome, n) in [
        ("ran", 1.0),
        ("refused", 1.0),
        ("broken", 0.0),
        ("held", 0.0),
        ("unknown", 0.0),
    ] {
        let key = format!("fennec_scheduled_runs_total{{outcome=\"{outcome}\"}}");
        assert_eq!(value(&m, &key), n, "{outcome}");
    }
    // Each passed over the slots from the first after its definition to
    // the one it ran, its slot in its slow line.
    let run = &tick[tick.find(r#""scheduled":["tick",["#).unwrap()..];
    let run = &run[run.find(r#"],""#).unwrap() + 3..];
    let slot = fennec_core::time::parse(&run[..run.find('"').unwrap()]).unwrap();
    let first = since - since.rem_euclid(HOUR) + HOUR;
    let skipped = 2 * (slot - first) / HOUR;
    assert!(skipped == 46 || skipped == 48, "{tick}");
    assert_eq!(value(&m, "fennec_scheduled_skipped_total"), skipped as f64);
    assert_eq!(value(&m, "fennec_scheduled_mark_ahead_total"), 0.0);
    assert_eq!(value(&m, "fennec_scheduled_lateness_seconds_count"), 2.0);
    // The two runs, slow at 0 ms, and the binding one woke.
    assert_eq!(value(&m, "fennec_slow_statements_total"), 2.0);
    assert_eq!(value(&m, "fennec_bound_runs_total"), 1.0);
    assert!(m
        .keys()
        .filter(|k| k.starts_with("fennec_scheduled_"))
        .all(|k| !k.contains("tick") && !k.contains("gatekeep")));
}

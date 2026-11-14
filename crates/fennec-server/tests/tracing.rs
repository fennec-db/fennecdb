//! OpenTelemetry tracing (`--otlp-endpoint`) from a running server: the
//! spans a collector is sent, with their parents and attributes; what a
//! ratio of 0 sends; and a collector that hangs or is down, which no
//! request may wait for.

use crate::otlp::{by_id, children, closed, silent, Got, Receiver};
use crate::support::{start_env, tmp, Http};
use std::time::{Duration, Instant};

const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const PARENT: &str = "00f067aa0ba902b7";

fn traceparent(trace: &str, flags: &str) -> String {
    format!("00-{trace}-{PARENT}-{flags}")
}

/// `name`'s value in a scrape, the labels as written.
fn scraped(http: &mut Http, name: &str) -> f64 {
    let a = http.ask("GET", "/_metrics", "");
    assert_eq!(a.status, 200);
    a.body
        .lines()
        .find_map(|l| l.strip_prefix(name)?.strip_prefix(' ')?.parse().ok())
        .unwrap_or_else(|| panic!("no {name} in\n{}", a.body))
}

/// Polls the scrape until `name` is past 0: an exporter's count moves on
/// its own thread, after the requests that made it.
fn until_counted(http: &mut Http, name: &str) -> f64 {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let n = scraped(http, name);
        if n > 0.0 {
            return n;
        }
        assert!(Instant::now() < deadline, "{name} stayed 0");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn within(child: &Got, parent: &Got) {
    assert_eq!(child.trace, parent.trace);
    assert!(
        child.start >= parent.start && child.end <= parent.end,
        "{} [{}, {}] outside {} [{}, {}]",
        child.name,
        child.start,
        child.end,
        parent.name,
        parent.start,
        parent.end
    );
}

#[test]
fn a_durable_write_is_traced_through_the_lock_the_disk_and_the_replicas() {
    let recv = Receiver::start();
    let path = tmp("trace", "spans.fennec");
    let url = recv.url();
    let server = start_env(
        &[
            "--file",
            path.to_str().unwrap(),
            "--sync",
            "always",
            "--replication-token",
            "r",
            "--otlp-endpoint",
            &url,
            "--otlp-header",
            "x-api-key=k1",
        ],
        &[("OTEL_SERVICE_NAME", "orders-db")],
    );
    let mut http = server.http();
    http.run("create collection notes (title text)");
    let tp = traceparent(TRACE, "01");
    let a = http.ask_with(
        "POST",
        "/query",
        r#"{"query": "put notes {title: 'secret'}"}"#,
        &[
            ("traceparent", &tp),
            ("tracestate", "vendor=x,other=y"),
            ("X-Request-Id", "req-7"),
        ],
    );
    assert_eq!(a.status, 200, "{}", a.body);

    // A trace goes out whole, so its server span brings the rest.
    let spans = recv.until("the put's server span", |s| {
        s.iter().any(|g| g.trace == TRACE && g.kind == 2)
    });
    let root = spans
        .iter()
        .find(|g| g.trace == TRACE && g.kind == 2)
        .unwrap();
    assert_eq!(root.parent.as_deref(), Some(PARENT), "the client's span");
    assert_eq!(root.state.as_deref(), Some("vendor=x,other=y"));
    assert_eq!(root.service, "orders-db");
    assert_eq!(root.name, "POST /query");
    for (k, v) in [
        ("http.request.method", "POST"),
        ("http.route", "/query"),
        ("http.response.status_code", "200"),
        ("db.system", "fennecdb"),
        ("db.operation.name", "put"),
        ("db.query.text", "put notes {title: $1}"),
        ("fennec.request_id", "req-7"),
        ("server.address", "127.0.0.1"),
        ("server.port", &server.port.to_string()),
    ] {
        assert_eq!(root.attr(k), Some(v), "{k} in {root:#?}");
    }
    assert!(!root.error);
    let trace: Vec<Got> = spans.iter().filter(|g| g.trace == TRACE).cloned().collect();
    assert!(
        trace
            .iter()
            .all(|g| !format!("{:?}", g.attrs).contains("secret")),
        "a literal reached a span: {trace:#?}"
    );
    let lock = children(&trace, root, "lock.wait");
    assert_eq!(lock.len(), 1, "{trace:#?}");
    assert_eq!(lock[0].attr("fennec.lock"), Some("write"));
    let [execute] = children(&trace, root, "execute")[..] else {
        panic!("{trace:#?}")
    };
    let [durability] = children(&trace, root, "durability")[..] else {
        panic!("{trace:#?}")
    };
    let [fsync] = children(&trace, durability, "fsync")[..] else {
        panic!("{trace:#?}")
    };
    let [sent] = children(&trace, durability, "replication.wait_sent")[..] else {
        panic!("{trace:#?}")
    };
    for child in [lock[0], execute, durability] {
        within(child, root);
        assert_eq!(child.kind, 1);
    }
    within(fsync, durability);
    within(sent, durability);
    assert!(fsync.end <= sent.start, "the disk, then the replicas");
    assert!(
        execute.end <= durability.start,
        "the fsync waits for no lock"
    );
    assert_eq!(
        by_id(&trace, &fsync.parent.clone().unwrap()).name,
        "durability"
    );

    // No traceparent: a trace of its own, sampled at the default of 1.
    let a = http.ask_with("GET", "/notes", "", &[("X-Request-Id", "req-8")]);
    assert_eq!(a.status, 200);
    let spans = recv.until("the read's server span", |s| {
        s.iter()
            .any(|g| g.attr("fennec.request_id") == Some("req-8"))
    });
    let read = spans
        .iter()
        .find(|g| g.attr("fennec.request_id") == Some("req-8"))
        .unwrap();
    assert_ne!(read.trace, TRACE);
    assert_eq!(read.parent, None);
    assert_eq!(read.name, "GET /{collection}");
    assert_eq!(read.attr("db.operation.name"), Some("get"));
    assert_eq!(
        read.attr("db.query.text"),
        None,
        "a REST target holds values"
    );
    let lock = children(&spans, read, "lock.wait");
    assert_eq!(lock[0].attr("fennec.lock"), Some("read"));
    assert_eq!(children(&spans, read, "execute").len(), 1);

    // A refusal is the client's, not an error of the server's span.
    let a = http.ask_with("GET", "/missing", "", &[("X-Request-Id", "req-9")]);
    assert_eq!(a.status, 404);
    let spans = recv.until("the 404's span", |s| {
        s.iter()
            .any(|g| g.attr("fennec.request_id") == Some("req-9"))
    });
    let missing = spans
        .iter()
        .find(|g| g.attr("fennec.request_id") == Some("req-9"))
        .unwrap();
    assert_eq!(missing.attr("http.response.status_code"), Some("404"));
    assert!(!missing.error);

    // A call, by its path and over `/query`: the statement's name is
    // OpenTelemetry's stored procedure, its text the call's shape, its
    // route never its name, and no argument in any span.
    http.run("create statement stamp($t text) public as insert notes {title: $t} returning id");
    let by_path = (
        "/_call/stamp",
        r#"{"params":{"t":"secret3"}}"#,
        "req-10",
        "POST /_call/{name}",
    );
    let by_query = (
        "/query",
        r#"{"query":"call stamp {t: 'secret4'}"}"#,
        "req-11",
        "POST /query",
    );
    for (target, body, id, name) in [by_path, by_query] {
        let a = http.ask_with("POST", target, body, &[("X-Request-Id", id)]);
        assert_eq!(a.status, 200, "{}", a.body);
        let spans = recv.until("the call's span", |s| {
            s.iter().any(|g| g.attr("fennec.request_id") == Some(id))
        });
        let call = spans
            .iter()
            .find(|g| g.attr("fennec.request_id") == Some(id))
            .unwrap();
        assert_eq!(call.name, name);
        for (k, v) in [
            ("db.operation.name", "call"),
            ("db.query.text", "call stamp $1"),
            ("db.stored_procedure.name", "stamp"),
        ] {
            assert_eq!(call.attr(k), Some(v), "{k} in {call:#?}");
        }
        let trace: Vec<&Got> = spans.iter().filter(|g| g.trace == call.trace).collect();
        assert!(
            trace
                .iter()
                .all(|g| !format!("{:?}", g.attrs).contains("secret")),
            "an argument reached a span: {trace:#?}"
        );
    }

    // A statement bound to the write: a child span of the request a
    // binding a statement, never a row -- its name and its rows, no value
    // -- and the one that refused the write failed, with the status and
    // its `else`'s reason alone: the statement's text, never the engine's
    // words.
    http.run("create collection marks (n int)");
    http.run(
        "create statement marked after insert notes scans as begin
           get marks select n limit 1 require 0 else 'marked once';
           insert marks {n: new.id}
         end",
    );
    let body = r#"{"query":"insert notes {title: 'secret5'}"}"#;
    let a = http.ask_with("POST", "/query", body, &[("X-Request-Id", "req-12")]);
    assert_eq!(a.status, 200, "{}", a.body);
    let a = http.ask_with("POST", "/query", body, &[("X-Request-Id", "req-13")]);
    assert_eq!(a.status, 412, "{}", a.body);
    let spans = recv.until("the bindings' spans", |s| {
        s.iter()
            .any(|g| g.attr("fennec.request_id") == Some("req-13"))
    });
    for (id, refused) in [("req-12", false), ("req-13", true)] {
        let root = spans
            .iter()
            .find(|g| g.attr("fennec.request_id") == Some(id))
            .unwrap();
        let bound = children(&spans, root, "bound marked");
        assert_eq!(bound.len(), 1, "{spans:#?}");
        for (k, v) in [
            ("db.operation.name", "bound"),
            ("db.stored_procedure.name", "marked"),
            ("fennec.rows_woken", "1"),
        ] {
            assert_eq!(bound[0].attr(k), Some(v), "{k} in {:#?}", bound[0]);
        }
        assert_eq!(bound[0].error, refused, "{:#?}", bound[0]);
        let message = refused.then_some("412 marked once");
        assert_eq!(bound[0].message.as_deref(), message, "{:#?}", bound[0]);
        assert_eq!(bound[0].attr("error.type"), refused.then_some("412"));
        assert!(!format!("{:?}", bound[0].attrs).contains("secret"));
    }

    // A binding whose body meets a unique value taken: the engine's words
    // name the value the row holds, and the span tells the status alone --
    // the value is nowhere in what was exported.
    http.run("create collection tags (label text @unique)");
    http.run("insert tags {label: 'clash-value-91'}");
    http.run("create collection pins (label text)");
    http.run(
        "create statement tagged after insert pins scans as begin
           insert tags {label: new.label}
         end",
    );
    let body = r#"{"query":"insert pins {label: $1}","params":["clash-value-91"]}"#;
    let a = http.ask_with("POST", "/query", body, &[("X-Request-Id", "req-14")]);
    assert_eq!(a.status, 409, "{}", a.body);
    assert!(a.body.contains("clash-value-91"), "{}", a.body);
    let spans = recv.until("the clash's spans", |s| {
        s.iter()
            .any(|g| g.attr("fennec.request_id") == Some("req-14"))
    });
    let root = spans
        .iter()
        .find(|g| g.attr("fennec.request_id") == Some("req-14"))
        .unwrap();
    let bound = children(&spans, root, "bound tagged");
    assert_eq!(bound.len(), 1, "{spans:#?}");
    assert!(bound[0].error, "{:#?}", bound[0]);
    assert_eq!(bound[0].message.as_deref(), Some("409"), "{:#?}", bound[0]);
    assert_eq!(bound[0].attr("error.type"), Some("409"), "{:#?}", bound[0]);
    for post in recv.posts() {
        assert!(!post.body.contains("clash-value-91"), "{}", post.body);
    }

    for post in recv.posts() {
        assert_eq!(post.path, "/v1/traces");
        let header = |k: &str| {
            post.headers
                .iter()
                .find(|(h, _)| h == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(header("content-type"), Some("application/json"));
        assert_eq!(header("x-api-key"), Some("k1"));
    }
    // Health checks and scrapes are no spans of their own.
    assert_eq!(http.ask("GET", "/_health", "").status, 200);
    until_counted(&mut http, "fennec_trace_spans_exported_total");
    assert!(recv
        .spans()
        .iter()
        .all(|g| !g.name.contains("_health") && !g.name.contains("_metrics")));
}

#[test]
fn sampling_at_zero_sends_nothing() {
    let recv = Receiver::start();
    let url = recv.url();
    let server = start_env(
        &["--otlp-endpoint", &url, "--trace-sample", "0"],
        &[("OTEL_BSP_SCHEDULE_DELAY", "50")],
    );
    let mut http = server.http();
    http.run("create collection notes (title text)");
    let unsampled = traceparent("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "00");
    for i in 0..30 {
        let body = format!(r#"{{"query": "put notes {{title: 't{i}'}}"}}"#);
        assert_eq!(http.ask("POST", "/query", &body).status, 200);
        let a = http.ask_with("GET", "/notes", "", &[("traceparent", &unsampled)]);
        assert_eq!(a.status, 200);
    }
    // A sampled parent is followed whatever the ratio, and its trace goes
    // out after everything before it: once it is in, nothing else came.
    let marker = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let tp = traceparent(marker, "01");
    assert_eq!(
        http.ask_with("GET", "/notes", "", &[("traceparent", &tp)])
            .status,
        200
    );
    let spans = recv.until("the sampled request", |s| {
        s.iter().any(|g| g.trace == marker)
    });
    assert!(
        spans.iter().all(|g| g.trace == marker),
        "a ratio of 0 sent {spans:#?}"
    );
    assert_eq!(
        scraped(
            &mut http,
            "fennec_trace_spans_dropped_total{reason=\"queue_full\"}"
        ),
        0.0
    );
}

#[test]
fn a_collector_that_hangs_or_is_down_costs_the_requests_nothing() {
    // It takes the connection and never answers: every post waits out its
    // 20 s, while the queue holds 16 spans.
    let (_held, url) = silent();
    let env = [
        ("OTEL_BSP_MAX_QUEUE_SIZE", "16"),
        ("OTEL_BSP_MAX_EXPORT_BATCH_SIZE", "4"),
        ("OTEL_EXPORTER_OTLP_TIMEOUT", "20000"),
    ];
    let server = start_env(&["--otlp-endpoint", &url], &env);
    let mut http = server.http();
    http.run("create collection notes (title text)");
    http.run("put notes {title: 'a'}");
    let began = Instant::now();
    for _ in 0..300 {
        assert_eq!(http.ask("GET", "/notes?id=eq.1", "").status, 200);
    }
    // Waiting on the post even once would have taken its 20 s.
    let took = began.elapsed();
    assert!(took < Duration::from_secs(10), "300 reads took {took:?}");
    let dropped = until_counted(
        &mut http,
        "fennec_trace_spans_dropped_total{reason=\"queue_full\"}",
    );
    assert!(dropped > 0.0);

    // Down: every post refused at once, its spans counted out.
    let url = closed();
    let server = start_env(&["--otlp-endpoint", &url], &env[..2]);
    let mut http = server.http();
    http.run("create collection notes (title text)");
    let began = Instant::now();
    for i in 0..100 {
        let body = format!(r#"{{"query": "put notes {{title: 't{i}'}}"}}"#);
        assert_eq!(http.ask("POST", "/query", &body).status, 200);
    }
    let took = began.elapsed();
    assert!(took < Duration::from_secs(10), "100 writes took {took:?}");
    until_counted(
        &mut http,
        "fennec_trace_spans_dropped_total{reason=\"export_failed\"}",
    );
    assert!(server.logged("tracing: could not post", Duration::from_secs(10)));
}

const HOUR: i64 = 3_600_000;

/// A file holding three scheduled statements, every hour, defined a day
/// and a half hour ago -- the first slot after that, what it answers --:
/// at a server's first look each runs its latest slot, the ones before it
/// passed over -- `tick` lands, `gatekeep`'s guard refuses with its reason,
/// and `clash` meets a unique value held, whose engine words name it.
fn three_scheduled(path: &std::path::Path) -> i64 {
    fennec_ql::install();
    let since = fennec_core::time::now_ms().unwrap() - 24 * HOUR - HOUR / 2;
    let mut db = fennec_core::fs::open(path).unwrap();
    db.allow_schedules();
    db.set_clock(Some(since));
    for q in [
        "create collection ticks (slot timestamp, at timestamp)",
        "create collection users (email text @unique)",
        "put users {email: 'kept@example.com'}",
        "create statement tick every 1h as insert ticks {slot: slot(), at: now()}",
        "create statement gatekeep every 1h scans as begin get ticks where slot = \
         timestamp(0) require 1 else 'no tick at the epoch'; insert ticks {slot: slot(), \
         at: now()} end",
        "create statement clash every 1h as insert users {email: 'kept@example.com'}",
    ] {
        db.execute(&fennec_ql::parse_one(q).unwrap())
            .unwrap_or_else(|e| panic!("{q}: {e}"));
    }
    if let Some(sync) = db.flush().unwrap() {
        sync().unwrap();
    }
    since - since.rem_euclid(HOUR) + HOUR
}

/// A scheduled statement's run is a trace of its own, begun and ended on
/// the scheduler's thread (`docs/design/named-statements.md`, *Watching
/// schedules*): its root span `schedule <name>`, the shape it is counted
/// as, its slot, how late it ran and what it passed over, the lock's wait,
/// the run and its sync beneath it as a request's are; a refused run's
/// span failed with its status and the reason its `else` wrote, never the
/// engine's words -- a unique value its body met is named in them, and
/// reaches no span.
#[test]
fn a_scheduled_run_is_a_trace_of_its_own() {
    let recv = Receiver::start();
    let path = tmp("trace", "scheduled.fennec");
    let first = three_scheduled(&path);
    let url = recv.url();
    let _server = start_env(
        &["--file", path.to_str().unwrap(), "--otlp-endpoint", &url],
        &[],
    );
    let names = ["schedule tick", "schedule gatekeep", "schedule clash"];
    let spans = recv.until("the three runs' spans", |s| {
        names.iter().all(|n| s.iter().any(|g| g.name == *n))
    });
    let root = |n: &str| spans.iter().find(|g| g.name == n).unwrap();
    let tick = root("schedule tick");
    assert_eq!((tick.kind, tick.parent.as_deref()), (1, None), "{tick:#?}");
    for (k, v) in [
        ("db.system", "fennecdb"),
        ("db.operation.name", "schedule"),
        ("db.stored_procedure.name", "tick"),
        ("db.query.text", "schedule tick"),
        ("fennec.outcome", "ran"),
    ] {
        assert_eq!(tick.attr(k), Some(v), "{k} in {tick:#?}");
    }
    assert!(!tick.error, "{tick:#?}");
    let slot = tick.attr("fennec.slot").unwrap();
    assert!(slot.ends_with(":00:00Z"), "an hour's edge: {slot}");
    // A day's slots passed over, the latest run: from the first after the
    // definition, 23 or 24 by where in its hour the clock stood.
    let ran = fennec_core::time::parse(slot).unwrap();
    let skipped = ((ran - first) / HOUR).to_string();
    assert_eq!(tick.attr("fennec.slots_skipped"), Some(&*skipped));
    assert!(skipped == "23" || skipped == "24", "{skipped}");
    let late: i64 = tick.attr("fennec.lateness_ms").unwrap().parse().unwrap();
    assert!((0..HOUR).contains(&late), "{late}");
    assert!(!tick.attr("fennec.request_id").unwrap_or("").is_empty());
    let trace: Vec<Got> = (spans.iter())
        .filter(|g| g.trace == tick.trace)
        .cloned()
        .collect();
    // The write lock the run holds to its landing, and the read the lease
    // is asked under once it is durable.
    let lock = children(&trace, tick, "lock.wait");
    let held: Vec<_> = lock.iter().map(|l| l.attr("fennec.lock")).collect();
    assert_eq!(held, [Some("write"), Some("read")], "{trace:#?}");
    for child in ["execute", "durability"] {
        let [c] = children(&trace, tick, child)[..] else {
            panic!("{child}: {trace:#?}")
        };
        within(c, tick);
    }
    // Refused: the status, and the reason the statement's text wrote.
    let gatekeep = root("schedule gatekeep");
    assert!(gatekeep.error, "{gatekeep:#?}");
    assert_eq!(
        gatekeep.message.as_deref(),
        Some("412 no tick at the epoch")
    );
    assert_eq!(gatekeep.attr("error.type"), Some("412"));
    assert_eq!(gatekeep.attr("fennec.outcome"), Some("refused"));
    assert!(
        children(&spans, gatekeep, "durability").is_empty(),
        "it landed nothing"
    );
    let clash = root("schedule clash");
    assert_eq!(clash.message.as_deref(), Some("409"), "{clash:#?}");
    assert!(
        spans
            .iter()
            .all(|g| !format!("{g:?}").contains("kept@example.com")),
        "a value reached a span: {spans:#?}"
    );
    // Each run a trace of its own.
    assert_ne!(tick.trace, gatekeep.trace);
    assert_ne!(gatekeep.trace, clash.trace);
}

/// An image's import refused from its head -- carrying no admin token, its
/// body never read -- is a request's trace as one refused once its body
/// was read was: its server span named by its route, continuing the
/// client's trace, its 401 the client's and no error of the server's, and
/// the wait a refused token waits inside it.
#[test]
fn an_import_refused_from_its_head_is_a_trace() {
    use std::io::{Read, Write};
    let recv = Receiver::start();
    let url = recv.url();
    let dir = tmp("trace", "node");
    std::fs::create_dir_all(&dir).unwrap();
    let server = start_env(
        &[
            "--dir",
            dir.to_str().unwrap(),
            "--admin-token",
            "adm",
            "--auth-delay",
            "100",
            "--otlp-endpoint",
            &url,
        ],
        &[],
    );
    let trace = "cccccccccccccccccccccccccccccccc";
    let mut s = std::net::TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    // A gibibyte declared and none of it sent: answered from its head, or
    // never.
    write!(
        s,
        "PUT /_admin/tenants/copy/file HTTP/1.1\r\nHost: localhost\r\n\
         Authorization: Bearer guessed\r\ntraceparent: {}\r\nX-Request-Id: req-import\r\n\
         Content-Length: {}\r\n\r\n",
        traceparent(trace, "01"),
        1u64 << 30
    )
    .unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    assert!(out.starts_with("HTTP/1.1 401"), "{out:?}");
    let spans = recv.until("the refused import's span", |s| {
        s.iter().any(|g| g.trace == trace && g.kind == 2)
    });
    let root = spans
        .iter()
        .find(|g| g.trace == trace && g.kind == 2)
        .unwrap();
    assert_eq!(root.name, "PUT /_admin/tenants/{name}/file");
    assert_eq!(root.parent.as_deref(), Some(PARENT), "the client's span");
    for (k, v) in [
        ("http.request.method", "PUT"),
        ("http.route", "/_admin/tenants/{name}/file"),
        ("http.response.status_code", "401"),
        ("fennec.request_id", "req-import"),
    ] {
        assert_eq!(root.attr(k), Some(v), "{k} in {root:#?}");
    }
    assert!(!root.error, "a refusal is the client's: {root:#?}");
    assert!(
        root.end - root.start >= 100_000_000,
        "the refusal's wait is outside its span: {root:#?}"
    );
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}

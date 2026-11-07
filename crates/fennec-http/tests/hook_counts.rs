//! What a process tells of the webhook events its tenants took
//! (`docs/design/named-statements.md`, *Watching hooks*): each statement an
//! event ran counted as the tenant's shape `hook <name>` -- an error where
//! it refused the event, read under the tenant's prefix and, named, by the
//! node's admin alone -- and a child span of the event's request, named by
//! the statement, the refusing one failed with its status and the reason
//! its `else` wrote; the request's span names the hook and what the event
//! came to, never its key or a value of its body. Apart: the counts, and
//! tracing, are the process's own.

#[path = "../../fennec-server/tests/otlp.rs"]
mod otlp;

use fennec_core::prelude::*;
use fennec_http::crypto::hmac_sha256;
use fennec_http::tenants::Tenants;
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

const ADMIN: &str = "adm";
const SECRET: &str = "whsec_acme";

fn call(
    port: u16,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut req = format!(
        "{method} {target} HTTP/1.1\r\nHost: t\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let body = out.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    (out[9..12].parse().unwrap(), body.to_string())
}

fn query(port: u16, tenant: &str, q: &str) {
    let mut body = String::from("{\"query\":");
    fennec_core::json::escape_into(&mut body, q);
    body.push('}');
    let (status, answer) = call(port, "POST", &format!("/t/{tenant}/query"), &[], &body);
    assert_eq!(status, 200, "{q}: {answer}");
}

/// Stripe's event `id` for order `order` of `total`, signed now under the
/// tenant's secret and sent to its hook.
fn event(port: u16, tenant: &str, id: &str, order: &str, total: i64) -> (u16, String) {
    let body = format!(r#"{{"id":"{id}","order":"{order}","total":{total}}}"#);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mac = hmac_sha256(SECRET.as_bytes(), format!("{t}.{body}").as_bytes());
    let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
    let sig = format!("t={t},v1={hex}");
    call(
        port,
        "POST",
        &format!("/t/{tenant}/_hooks/stripe"),
        &[
            ("Content-Type", "application/json"),
            ("Stripe-Signature", &sig),
        ],
        &body,
    )
}

/// Every entry of a `/_stats/statements` answer whose shape is `query`.
fn shapes(stats: &str, query: &str) -> Vec<Value> {
    let doc = fennec_core::json::parse_json(stats).unwrap();
    let Some(Value::List(all)) = doc.member("statements").cloned() else {
        panic!("{stats}")
    };
    (all.into_iter())
        .filter(|e| e.member("query") == Some(&Value::Text(query.into())))
        .collect()
}

fn n(entry: &Value, k: &str) -> i64 {
    match entry.member(k) {
        Some(Value::Int(n)) => *n,
        other => panic!("{k}: {other:?} in {entry:?}"),
    }
}

#[test]
fn an_events_statements_are_counted_and_traced_by_name_a_tenant_s_apart() {
    fennec_http::audit::set_delay(0);
    let recv = otlp::Receiver::start();
    let settings = fennec_http::trace::Options {
        endpoint: Some(recv.url()),
        ..Default::default()
    }
    .settings("fennec-http")
    .unwrap()
    .unwrap();
    fennec_http::trace::install(settings).unwrap();

    let dir = std::env::temp_dir().join(format!(
        "fennec-hook-counts-{}-{:?}",
        std::process::id(),
        Instant::now()
    ));
    let tenants = Arc::new(Tenants::new(&dir).unwrap());
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        admin_token: Some(ADMIN.into()),
        ..Config::default()
    };
    let server = Server::with_tenants(Arc::clone(&tenants), cfg);
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    let admin = [("Authorization", "Bearer adm")];
    for tenant in ["acme", "beta"] {
        let (status, body) = call(
            port,
            "PUT",
            &format!("/_admin/tenants/{tenant}"),
            &admin,
            "",
        );
        assert_eq!(status, 201, "{body}");
        for q in [
            "create collection orders (ref text @hash, total int, status text)",
            "create collection notes (ref text)",
            "put orders {ref: 'o-1', total: 2000, status: 'open'}",
            "create statement a_note($order text from event.order) on hook stripe as insert \
             notes {ref: $order}",
            "create statement paid($order text from event.order, $total int from event.total) \
             on hook stripe as set orders {status: 'paid'} where ref = $order and total = \
             $total and status = 'open' require 1 else 'no open order of that total'",
        ] {
            query(port, tenant, q);
        }
        let (status, body) = call(
            port,
            "PUT",
            &format!("/_admin/tenants/{tenant}/hooks/stripe"),
            &admin,
            &format!(r#"{{"scheme":"stripe","secrets":["{SECRET}"]}}"#),
        );
        assert_eq!(status, 200, "{body}");
    }

    // Taken by both statements; then one refused by `paid`'s guard, after
    // `a_note` ran, nothing of it landing.
    let (status, body) = event(port, "acme", "evt_taken", "o-1", 2000);
    assert_eq!(
        (status, body.as_str()),
        (200, r#"{"event":"evt_taken","taken":2}"#)
    );
    let (status, body) = event(port, "acme", "evt_refused", "o-1", 7);
    assert_eq!(status, 412, "{body}");
    let (status, _) = event(port, "beta", "evt_beta", "o-1", 2000);
    assert_eq!(status, 200);

    // The tenant's own shapes under its prefix: each statement a call an
    // event it ran, an error where it refused the event.
    let (status, stats) = call(port, "GET", "/t/acme/_stats/statements", &[], "");
    assert_eq!(status, 200, "{stats}");
    let [note] = &shapes(&stats, "hook a_note")[..] else {
        panic!("{stats}")
    };
    assert_eq!(
        (n(note, "calls"), n(note, "errors"), n(note, "rows")),
        (2, 0, 2)
    );
    let [paid] = &shapes(&stats, "hook paid")[..] else {
        panic!("{stats}")
    };
    assert_eq!(
        (n(paid, "calls"), n(paid, "errors"), n(paid, "rows")),
        (2, 1, 1)
    );
    assert_eq!(
        paid.member("tenant"),
        Some(&Value::Text("acme".into())),
        "{stats}"
    );
    assert!(!stats.contains("beta"), "another tenant's: {stats}");
    // Across tenants, each named, to the node's admin alone.
    let (status, all) = call(port, "GET", "/_stats/statements", &admin, "");
    assert_eq!(status, 200, "{all}");
    let tenants_of: Vec<_> = (shapes(&all, "hook paid").iter())
        .map(|e| e.member("tenant").cloned())
        .collect();
    assert_eq!(tenants_of.len(), 2, "{all}");
    for t in ["acme", "beta"] {
        assert!(tenants_of.contains(&Some(Value::Text(t.into()))), "{all}");
    }
    // A client's text shaped so is a shape of its own, never the runs'.
    query(port, "acme", "get notes where ref = 'hook paid' count");
    let (_, stats) = call(port, "GET", "/t/acme/_stats/statements", &[], "");
    assert_eq!(shapes(&stats, "hook paid").len(), 1, "{stats}");

    // The event's request is its trace: the hook and what it came to on
    // its span, each statement that ran a child of it.
    let roots = |spans: &[otlp::Got]| -> Vec<otlp::Got> {
        (spans.iter())
            .filter(|s| s.name == "POST /t/{tenant}/_hooks/{name}" && s.parent.is_none())
            .cloned()
            .collect()
    };
    let spans = recv.until("three events' spans and their children", |s| {
        roots(s).len() == 3 && s.iter().filter(|g| g.name.starts_with("hook ")).count() == 6
    });
    let root_of = |outcome: &str| -> otlp::Got {
        (roots(&spans).into_iter())
            .find(|r| {
                r.attr("fennec.outcome") == Some(outcome) && r.attr("fennec.tenant") == Some("acme")
            })
            .unwrap_or_else(|| panic!("no {outcome}: {spans:#?}"))
    };
    let taken = root_of("taken");
    for (k, v) in [
        ("db.operation.name", "hook"),
        ("fennec.hook", "stripe"),
        ("http.route", "/t/{tenant}/_hooks/{name}"),
    ] {
        assert_eq!(taken.attr(k), Some(v), "{k} in {taken:#?}");
    }
    let kids: Vec<_> = (spans.iter())
        .filter(|s| s.parent.as_deref() == Some(taken.id.as_str()) && s.name.starts_with("hook "))
        .collect();
    let names: Vec<_> = kids.iter().map(|k| k.name.as_str()).collect();
    assert_eq!(names, ["hook a_note", "hook paid"], "in name order");
    for k in &kids {
        assert_eq!(k.trace, taken.trace);
        assert!(
            k.start >= taken.start && k.end <= taken.end,
            "{k:#?} outside {taken:#?}"
        );
        assert_eq!(k.attr("db.operation.name"), Some("hook"));
        assert_eq!(k.attr("fennec.rows"), Some("1"));
        assert!(!k.error, "{k:#?}");
    }
    assert_eq!(kids[1].attr("db.stored_procedure.name"), Some("paid"));
    let refused = root_of("refused");
    let [failed] = &otlp::children(&spans, &refused, "hook paid")[..] else {
        panic!("{spans:#?}")
    };
    assert!(failed.error, "{failed:#?}");
    assert_eq!(
        failed.message.as_deref(),
        Some("412 no open order of that total")
    );
    assert_eq!(failed.attr("error.type"), Some("412"));
    assert_eq!(otlp::children(&spans, &refused, "hook a_note").len(), 1);
    // A span names the hook and its statements, never the event's key or a
    // value its sender chose.
    for s in &spans {
        let all = format!("{s:?}");
        assert!(!all.contains("evt_") && !all.contains("o-1"), "{s:#?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

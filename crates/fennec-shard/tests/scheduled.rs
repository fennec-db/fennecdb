//! Scheduled statements on leased nodes behind a router
//! (`docs/design/named-statements.md`, F3's slice 4, *Where a run happens,
//! and once*): a tenant's schedule declared in its schema, read by the
//! router with the grammar; a failover leaving the promoted copy the run
//! and its mark or neither, its `slot()` the same, the old node fenced; a
//! move's frozen source running nothing after its export and its target
//! the slot due, once; a move undone leaving the source to run it.
//!
//! Each node's scheduler is driven by the time the test names
//! (`schedule::Node::pass`); the leases, the streams and the router run on
//! the clock.

#[path = "lease_nodes.rs"]
pub(crate) mod nodes;

use fennec_http::schedule::{Outcome, Run};
use nodes::*;
use std::time::{Duration, Instant};

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;

const SCHEMA: &str = "create collection ticks (slot timestamp, at timestamp); create statement \
                      tick every 1h as insert ticks {slot: slot(), at: now()}";

fn now_ms() -> i64 {
    fennec_core::time::now_ms().unwrap()
}

/// Creates `t` on `n1` with [`SCHEMA`] through the router: its schedule
/// read by the router with the grammar, defined on the node. The slot due
/// first, read off the node's file, and the node holding its replica once
/// it took the primary's image.
fn created<'a>(nodes: &'a [Node; 3], port: u16, t: &str) -> (i64, &'a Node) {
    let mut body = String::from(r#"{"node":"n1","schema":"#);
    fennec_core::json::escape_into(&mut body, SCHEMA);
    body.push('}');
    let (status, answer) = call(port, "PUT", &format!("/_shard/tenants/{t}"), &body, None);
    assert_eq!(status, 201, "{answer}");
    let replica = by_name(nodes, &placed(port, t).1.expect("a replica"));
    until_copied(t, &nodes[0], replica);
    let tenant = nodes[0].tenants.get(t).unwrap();
    let due = tenant.db.read().unwrap().due(now_ms());
    assert_eq!(due.len(), 1, "{due:?}");
    (due[0].next.expect("a slot to come"), replica)
}

/// Waits until `nd`'s lease lets it write `t`.
fn writable(nd: &Node, t: &str) {
    let deadline = Instant::now() + WITHIN;
    let lease = nd.tenants.lease().expect("a leased node");
    while let Err(why) = lease.allows(t) {
        assert!(
            Instant::now() < deadline,
            "{} never took {t}: {why}",
            nd.tag
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The ticks `nd`'s file of `t` holds: each `[slot, at]`.
fn ticks(nd: &Node, t: &str) -> Vec<(i64, i64)> {
    let tenant = nd.tenants.get(t).unwrap();
    let g = tenant.db.read().unwrap();
    let q = fennec_ql::parse_one("get ticks select slot, at").unwrap();
    let r = g.query(&q, &[]).unwrap();
    let ms = |v: &fennec_core::prelude::Value| match v {
        fennec_core::prelude::Value::Timestamp(t) => *t,
        other => panic!("{other:?}"),
    };
    let mut out: Vec<(i64, i64)> = (r.rows().unwrap().rows.iter())
        .map(|row| (ms(&row.values[0]), ms(&row.values[1])))
        .collect();
    out.sort();
    out
}

fn one(runs: &[(String, Run)]) -> (i64, Outcome) {
    assert_eq!(runs.len(), 1, "{runs:?}");
    (runs[0].1.slot, runs[0].1.outcome)
}

/// Waits until the router has `t` on `node`.
fn until_placed(port: u16, t: &str, node: &str) {
    let deadline = Instant::now() + WITHIN;
    while placed(port, t).0 != node {
        assert!(
            Instant::now() < deadline,
            "{t} never went to {node}: {:?}",
            placed(port, t)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A lease node runs a slot and is cut off before its stream sends the
/// block: the copy promoted holds neither the run nor its mark, and runs
/// that slot once, its `slot()` the same; the old node, fenced past its
/// lease, lands no run; back, it follows the promoted copy, its own run on
/// a history leading nowhere.
#[test]
fn a_run_its_stream_never_sent_runs_once_on_the_promoted_copy() {
    let (nodes, port, _router) = three_leased("schedfo", true, TERM, upstream());
    let (slot, replica) = created(&nodes, port, "t");
    // The stream to the replica is cut with the router's renewals: the
    // run lands on n1 alone while its lease runs.
    nodes[0].proxy.cut();
    let at = slot + 1000;
    assert_eq!(one(&nodes[0].schedule.pass(at)), (slot, Outcome::Ran));
    assert_eq!(ticks(&nodes[0], "t"), [(slot, at)]);

    until_placed(port, "t", &replica.tag.replace("schedfo", "n"));
    writable(replica, "t");
    assert!(ticks(replica, "t").is_empty(), "the block never left n1");
    let again = slot + 2000;
    assert_eq!(one(&replica.schedule.pass(again)), (slot, Outcome::Ran));
    assert_eq!(ticks(replica, "t"), [(slot, again)], "slot() the same");
    assert!(replica.schedule.pass(again + MINUTE).is_empty(), "once");

    // The old node, its lease lapsed, lands no run.
    assert!(nodes[0].schedule.pass(slot + HOUR + 1000).is_empty());
    assert_eq!(ticks(&nodes[0], "t"), [(slot, at)]);

    // Back, it follows the promoted copy: its own run is gone with the
    // history it was on, the promoted copy's run in its place.
    nodes[0].proxy.heal();
    let deadline = Instant::now() + WITHIN;
    while ticks(&nodes[0], "t") != [(slot, again)] {
        assert!(
            Instant::now() < deadline,
            "n1 never followed: {:?}",
            ticks(&nodes[0], "t")
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The stream sent the run's block before the node was lost: the copy
/// promoted holds the run and its mark, and runs nothing for that slot --
/// the next it runs when it comes.
#[test]
fn a_run_its_stream_sent_is_not_run_again_on_the_promoted_copy() {
    let (nodes, port, _router) = three_leased("schedsent", true, TERM, upstream());
    let (slot, replica) = created(&nodes, port, "t");
    let at = slot + 1000;
    assert_eq!(one(&nodes[0].schedule.pass(at)), (slot, Outcome::Ran));
    let deadline = Instant::now() + WITHIN;
    while ticks(replica, "t") != [(slot, at)] {
        assert!(Instant::now() < deadline, "the replica never took the run");
        std::thread::sleep(Duration::from_millis(20));
    }
    nodes[0].proxy.cut();
    until_placed(port, "t", &replica.tag.replace("schedsent", "n"));
    writable(replica, "t");
    assert!(
        replica.schedule.pass(slot + 2000).is_empty(),
        "its mark came with it"
    );
    assert_eq!(ticks(replica, "t"), [(slot, at)]);
    let next = slot + HOUR + 1000;
    assert_eq!(
        one(&replica.schedule.pass(next)),
        (slot + HOUR, Outcome::Ran)
    );
}

/// A move: its source frozen runs nothing after its export -- here kept
/// there frozen, a request holding it past the move's delete -- and its
/// target runs the slot due, once.
#[test]
fn a_moved_tenant_runs_its_slot_on_its_target_once_and_its_frozen_source_none() {
    let (nodes, port, _router) = three_leased("schedmv", true, TERM, upstream());
    let (slot, replica) = created(&nodes, port, "t");
    let to = nodes[1..]
        .iter()
        .find(|n| n.tag != replica.tag)
        .expect("a node holding no copy");
    let to_name = to.tag.replace("schedmv", "n");
    // Held by a request in flight, the source's copy outlasts the move's
    // delete, frozen.
    let held = nodes[0].tenants.get("t").unwrap();
    let (status, body) = call(
        port,
        "POST",
        "/_shard/tenants/t/move",
        &format!(r#"{{"to":"{to_name}"}}"#),
        None,
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""source_removed":false"#), "{body}");
    assert!(held.is_frozen());
    assert!(nodes[0].schedule.pass(slot + 1000).is_empty(), "frozen");
    // Its delete refused while the request holds it, the source's hint
    // names it no more: a restart, its freeze gone with the process, runs
    // nothing into a file no router sends to.
    assert!(!nodes[0].schedule.named().iter().any(|n| n.0 == "t"));
    let hint = fennec_http::schedule::hint::read(nodes[0].tenants.dir())
        .unwrap()
        .expect("written");
    assert!(!hint.contains_key("t"), "{hint:?}");

    writable(to, "t");
    let at = slot + 2000;
    assert_eq!(one(&to.schedule.pass(at)), (slot, Outcome::Ran));
    assert_eq!(ticks(to, "t"), [(slot, at)]);
    assert!(to.schedule.pass(at + MINUTE).is_empty(), "once");
    assert!(
        nodes[0].schedule.pass(slot + 3000).is_empty(),
        "frozen still"
    );
    drop(held);
}

/// A move undone -- its target cut off, the install never made -- thaws
/// its source, which runs the slot due.
#[test]
fn a_move_undone_leaves_its_source_to_run_the_slot() {
    let (nodes, port, _router) = three_leased("schedun", true, TERM, upstream());
    let (slot, replica) = created(&nodes, port, "t");
    let to = nodes[1..]
        .iter()
        .find(|n| n.tag != replica.tag)
        .expect("a node holding no copy");
    to.proxy.cut();
    let (status, body) = call(
        port,
        "POST",
        "/_shard/tenants/t/move",
        &format!(r#"{{"to":"{}"}}"#, to.tag.replace("schedun", "n")),
        None,
    );
    assert_ne!(status, 200, "{body}");
    assert_eq!(placed(port, "t").0, "n1");
    let tenant = nodes[0].tenants.get("t").unwrap();
    assert!(!tenant.is_frozen(), "thawed");
    drop(tenant);
    writable(&nodes[0], "t");
    let at = slot + 1000;
    assert_eq!(one(&nodes[0].schedule.pass(at)), (slot, Outcome::Ran));
    assert_eq!(ticks(&nodes[0], "t"), [(slot, at)]);
}

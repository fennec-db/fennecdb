//! A router over nodes serving HTTPS alone (`--tls-cert`), each recorded
//! at an `https://` address: the router relays its clients' requests to
//! them, grants their leases and calls their `/_admin/` over TLS, each
//! node's copy follows its primary over TLS, and a node cut off is failed
//! over as over plain HTTP -- its tenant writing again on the copy the
//! router promoted. The test CA is the process's (`--tls-ca`).
#![cfg(feature = "tls")]

// The lease tests' nodes, proxies and calls: `leases.rs`' module, as one
// binary may hold the file once.
use crate::leases::nodes::*;
use std::time::Instant;

#[test]
fn a_router_relays_leases_and_fails_over_nodes_serving_https() {
    let (nodes, port, router) = three_leased_with("tls", true, TERM, upstream(), true);
    let (status, body) = call(
        port,
        "PUT",
        "/_shard/tenants/acme",
        r#"{"node":"n1"}"#,
        None,
    );
    assert_eq!(status, 201, "{body}");
    let path = "/t/acme/query";
    written(port, "acme", "create collection notes (title text)");
    written(port, "acme", r#"put notes {id: 1, title: "one"}"#);
    until(port, path, "get notes select title", "one");
    // A round of grants, each over TLS.
    round(&router);

    // The copy follows its primary over TLS, node to node.
    let copy = placed(port, "acme").1.expect("acme has a copy");
    let deadline = Instant::now() + WITHIN;
    while !by_name(&nodes, &copy)
        .query(path, "get notes select title")
        .1
        .contains("one")
    {
        assert!(Instant::now() < deadline, "the copy never took the write");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // n1 cut off: its lease lapses, and the copy is promoted and granted
    // its own, over TLS, taking writes through the router.
    nodes[0].proxy.cut();
    let deadline = Instant::now() + WITHIN;
    while placed(port, "acme").0 == "n1" {
        assert!(Instant::now() < deadline, "acme was never failed over");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(placed(port, "acme").0, copy);
    written(port, "acme", r#"put notes {id: 2, title: "two"}"#);
    until(port, path, "get notes select title", "two");
}

//! `integrations/api-golden/cli-fennec-shard.json` and
//! `metrics-fennec-shard.json` are what the router gives now: its flags,
//! asked of its parser, its exit codes (`fennec_shard::exit`), and the
//! metrics the binary's scrapes show -- a primary router leasing a node,
//! with `--replicas`, a node in a region and one it cannot reach, and its
//! standby following it (docs/design/conventions.md#api-baselines).
//! fennec-server's `tests/api_golden.rs` holds every `fennec_router_` name
//! the sources declare to this dump. `make api-golden` writes them again;
//! tools/api_compat.py holds them to main's.

use crate::api_dump::{self, Family};
use fennec_http::tenants::{Replicated, Tenants};
use fennec_shard::directory::{Directory, Node, State};
use fennec_shard::exit::Code;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const BIN: &str = env!("CARGO_BIN_EXE_fennec-shard");
const TOKEN: &str = "api-golden-router";
const REPL: &str = "api-golden-replication";

#[test]
fn the_flags_and_exit_codes_are_the_dump() {
    let codes: Vec<(i32, &str, String)> = Code::ALL
        .iter()
        .map(|c| (*c as i32, c.means(), format!("{c:?}")))
        .collect();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let (_, dump) = api_dump::cli(Path::new(BIN), "fennec-shard", &src, &codes);
    api_dump::hold("cli-fennec-shard.json", &dump);
}

/// A directory of this test's own, removed at its end.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An in-process `--dir` node taking leases, in region `eu`: what the
/// router's per-node families name, a lease's age among them.
fn leasing_node(dir: &Path) -> String {
    let tenants = Tenants::new(dir)
        .unwrap()
        .with_replication(Replicated {
            token: REPL.into(),
            buffer: 8 << 20,
            upstream: None,
            sync_on_write: true,
        })
        .with_lease();
    let cfg = fennec_http::Config {
        addr: "127.0.0.1:0".into(),
        admin_token: Some("api-golden-node".into()),
        ..fennec_http::Config::default()
    };
    fennec_ql::install();
    let server = fennec_http::Server::with_tenants(Arc::new(tenants), cfg);
    let listener = server.bind().unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    addr
}

fn scrape(router: &api_dump::Running) -> String {
    let (status, text) = api_dump::get(router.port, "/_metrics", TOKEN);
    assert_eq!(status, 200, "{text}");
    text
}

/// The configurations that between them show every family the router
/// writes: `--replicas`' (`tenants_unreplicated`), `--auto-failover`'s
/// (a lease's age, a node failed over), a node in a region and one in none,
/// a request a node did not answer, a primary's feed and a standby's
/// follower.
#[test]
fn the_metrics_are_the_dump() {
    let root = std::env::temp_dir().join(format!("fennec-shard-api-golden-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("n1")).unwrap();
    let scratch = Scratch(root);
    let n1 = leasing_node(&scratch.0.join("n1"));
    // A node nothing listens on: what a crashed one looks like from here.
    // Port 1 (`unsent::refusing`): a port found free and let go of is the
    // next listener's to take (docs/design/conventions.md#ports).
    let dead = crate::unsent::refusing();
    let primary_file = scratch.0.join("primary.fennec");
    {
        let mut dir = Directory::open(&primary_file).unwrap();
        let n1 = Node {
            addr: n1,
            token: "api-golden-node".into(),
            region: "eu".into(),
            zone: "a".into(),
        };
        dir.set_node("n1", n1).unwrap();
        let dead = Node {
            addr: dead,
            token: "api-golden-dead".into(),
            ..Node::default()
        };
        dir.set_node("dead", dead).unwrap();
        dir.place("lost", "dead", State::Active).unwrap();
        dir.db().write().unwrap().sync().unwrap();
    }
    let primary = api_dump::listening(
        Path::new(BIN),
        &[
            "--listen",
            "127.0.0.1:0",
            "--directory",
            primary_file.to_str().unwrap(),
            "--token",
            TOKEN,
            "--replication-token",
            REPL,
            "--replicas",
            // Long enough that nothing lapses while the test runs: the
            // node's first grant is what it waits for.
            "--auto-failover",
            "120",
            "--auth-delay",
            "0",
        ],
    );
    // Its node not reached: counted against the node.
    let (status, body) = api_dump::get(primary.port, "/t/lost/", TOKEN);
    assert_eq!(status, 503, "{body}");
    let mut seen = BTreeMap::<String, Family>::new();
    let text = api_dump::scrape_until(
        "the node's lease and the dead node's error",
        || scrape(&primary),
        |t| {
            t.contains("fennec_router_lease_age_seconds{node=\"n1\"")
                && t.contains("fennec_router_upstream_errors_total{node=\"dead\"}")
        },
    );
    api_dump::merge(&mut seen, &text);

    let standby_file = scratch.0.join("standby.fennec");
    let primary_url = format!("http://127.0.0.1:{}", primary.port);
    let standby = api_dump::listening(
        Path::new(BIN),
        &[
            "--listen",
            "127.0.0.1:0",
            "--directory",
            standby_file.to_str().unwrap(),
            "--token",
            TOKEN,
            "--replication-token",
            REPL,
            "--replica-of",
            &primary_url,
            "--auth-delay",
            "0",
        ],
    );
    let text = api_dump::scrape_until(
        "the standby's contact",
        || scrape(&standby),
        |t| t.contains("\nfennec_replica_last_contact_seconds "),
    );
    api_dump::merge(&mut seen, &text);
    drop(standby);
    drop(primary);
    api_dump::hold("metrics-fennec-shard.json", &api_dump::metrics_json(&seen));
}

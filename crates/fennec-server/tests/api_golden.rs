//! `integrations/api-golden/cli-fennec-server.json` and
//! `metrics-fennec-server.json` are what the binary gives now: its flags,
//! asked of its parser, its exit codes (`fennec_server::exit`), and the
//! metrics its scrapes show -- a file's primary with every optional family
//! on, its replica, and a tenant node -- with every metric name the code
//! declares in a dump (docs/design/conventions.md#api-baselines).
//! `make api-golden` writes them again; tools/api_compat.py holds them to
//! main's.

use crate::api_dump::{self, Family};
use crate::support::{start, tmp, Server};
use fennec_server::exit::Code;
use std::collections::BTreeMap;
use std::path::Path;

const BIN: &str = env!("CARGO_BIN_EXE_fennec-server");
const TOKEN: &str = "api-golden-token";
const ADMIN: &str = "api-golden-admin";
const REPL: &str = "api-golden-replication";

#[test]
fn the_flags_and_exit_codes_are_the_dump() {
    let codes: Vec<(i32, &str, String)> = Code::ALL
        .iter()
        .map(|c| (*c as i32, c.means(), format!("{c:?}")))
        .collect();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let (commands, dump) = api_dump::cli(Path::new(BIN), "fennec-server", &src, &codes);
    // `--follow-<option>` is made at run time from fennec-import's list
    // (`src/main.rs`), where no scan of the sources sees it: each option the
    // importer offers is a flag of the server's help and dump, taking a
    // value, or a new one would be a flag no dump holds.
    let flags = &commands["fennec-server"];
    let missing: Vec<String> = fennec_import::args::OPTIONS
        .iter()
        .map(|o| format!("--follow-{}", &o[2..]))
        .filter(|f| flags.get(f) != Some(&api_dump::Takes::Value))
        .collect();
    assert!(
        missing.is_empty(),
        "fennec-import's options the server takes as --follow-<option> that its --help does \
         not show, or its parser does not take with a value: name each in USAGE\n  {missing:?}"
    );
    api_dump::hold("cli-fennec-server.json", &dump);
}

fn scrape(server: &Server, token: &str) -> String {
    let (status, text) = api_dump::get(server.port, "/_metrics", token);
    assert_eq!(status, 200, "{text}");
    text
}

/// The configurations that between them show every family the server
/// writes: those of a file and of a node, the hooks' (a hooks file read),
/// MCP's (`--mcp`), the scheduler's (once it looked), a primary's feed and a
/// replica's follower, a node's streams. The proof that none is left out
/// is the scan after: every `fennec_` name the sources declare is in this
/// dump or the router's.
#[test]
fn the_metrics_are_the_dump_and_every_name_the_code_declares_is_in_one() {
    let secret = tmp("api-golden", "stripe.whsec");
    // The test's directory goes with it, a failed assertion's too: declared
    // first, it is removed after the servers below are stopped.
    let _scratch = api_dump::Scratch(secret.parent().unwrap().to_path_buf());
    std::fs::write(&secret, "whsec_api_golden").unwrap();
    let hooks = tmp("api-golden", "hooks");
    std::fs::write(&hooks, format!("orders stripe {}\n", secret.display())).unwrap();
    let primary_file = tmp("api-golden", "primary.fennec");
    let primary = start(&[
        "--file",
        primary_file.to_str().unwrap(),
        "--http-token",
        TOKEN,
        "--replication-token",
        REPL,
        "--hooks",
        hooks.to_str().unwrap(),
        "--mcp",
        "--auth-delay",
        "0",
    ]);
    let mut c = primary.http().with_token(TOKEN);
    c.run("create collection notes (n int)");
    c.run("put notes {n: 1}");
    // A schedule wakes the scheduler, whose families a scrape writes once
    // it looked (`schedule::LOOKED`).
    c.run("create statement tick every 1h as insert notes {n: 2}");
    let mut seen = BTreeMap::<String, Family>::new();
    let text = api_dump::scrape_until(
        "the scheduler's families",
        || scrape(&primary, TOKEN),
        |t| t.contains("# TYPE fennec_scheduled_runs_total "),
    );
    api_dump::merge(&mut seen, &text);

    let replica_file = tmp("api-golden", "replica.fennec");
    let primary_url = format!("http://127.0.0.1:{}", primary.port);
    let replica = start(&[
        "--file",
        replica_file.to_str().unwrap(),
        "--replica-of",
        &primary_url,
        "--replication-token",
        REPL,
        "--http-token",
        TOKEN,
        "--auth-delay",
        "0",
    ]);
    let text = api_dump::scrape_until(
        "the replica's contact",
        || scrape(&replica, TOKEN),
        |t| t.contains("\nfennec_replica_last_contact_seconds "),
    );
    api_dump::merge(&mut seen, &text);

    let node_dir = tmp("api-golden", "node");
    let node = start(&[
        "--dir",
        node_dir.to_str().unwrap(),
        "--admin-token",
        ADMIN,
        "--replication-token",
        REPL,
        "--auth-delay",
        "0",
    ]);
    api_dump::merge(&mut seen, &scrape(&node, ADMIN));
    let dump = api_dump::metrics_json(&seen);
    api_dump::hold("metrics-fennec-server.json", &dump);

    // Every name the code can write is in a dump: the router's own
    // (`fennec_router_`) in the router's, every other in this one. Read
    // from the sources, so a family only a configuration above does not
    // show -- or a new one -- fails here, never passes unseen.
    let crates = api_dump::crates();
    let declared = api_dump::declared_metrics(&[
        crates.join("fennec-http/src"),
        crates.join("fennec-server/src"),
        crates.join("fennec-shard/src"),
    ]);
    let router = api_dump::top_keys(&api_dump::current("metrics-fennec-shard.json"));
    let missing: Vec<&String> = declared
        .iter()
        .filter(|n| match n.starts_with("fennec_router_") {
            true => !router.contains(*n),
            false => !seen.contains_key(*n),
        })
        .collect();
    assert!(
        missing.is_empty(),
        "metrics the code declares that no dump holds (the server's here, the router's in \
         fennec-shard's tests/api_golden.rs): show each in a configuration\n  {missing:?}"
    );
    assert!(
        declared.len() > 60,
        "the scan found {} names",
        declared.len()
    );
}

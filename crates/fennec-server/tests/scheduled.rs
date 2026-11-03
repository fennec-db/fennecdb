//! Scheduled statements as `fennec-server` runs them
//! (`docs/design/named-statements.md`, F3's slice 3), against the binary:
//! its scheduler starts after both listeners and runs a missed slot at its
//! first look, once; the one shutdown lands a run whole and synced or not
//! at all; a server failing at its port writes no run; a server taking no
//! writes runs none, saying how many it holds unrun; and a tenant node
//! learns which of its tenants hold one and runs them.

use crate::support::{command, ended, start, tmp};
use std::path::Path;
use std::time::{Duration, Instant};

const HOUR: i64 = 3_600_000;

fn now_ms() -> i64 {
    fennec_core::time::now_ms().unwrap()
}

/// A file holding `tick`, every hour, defined a day ago: its latest slot is
/// due to whoever writes the file next, the 23 before it passed over.
fn ticking(tag: &str) -> std::path::PathBuf {
    fennec_ql::install();
    let path = tmp("scheduled", &format!("{tag}.fennec"));
    let mut db = fennec_core::fs::open(&path).unwrap();
    db.allow_schedules();
    db.set_clock(Some(now_ms() - 24 * HOUR - HOUR / 2));
    for q in [
        "create collection ticks (slot timestamp, at timestamp)",
        "create statement tick every 1h as insert ticks {slot: slot(), at: now()}",
    ] {
        db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
    }
    if let Some(sync) = db.flush().unwrap() {
        sync().unwrap();
    }
    path
}

/// The ticks the file holds, and the slot its mark names.
fn ran(path: &Path) -> (usize, Option<i64>) {
    let db = fennec_core::fs::open(path).unwrap();
    let ticks = (db.stats().iter())
        .find(|s| s.name == "ticks")
        .map_or(0, |s| s.documents);
    (ticks, db.last_mark("tick").map(|m| m.at))
}

/// The first look runs the latest missed slot, once: its tick and its mark
/// land together, and a SIGTERM's shutdown syncs them -- or, a signal
/// before the run, neither is there.
#[test]
fn a_missed_slot_runs_at_the_first_look_and_the_shutdown_lands_it_whole() {
    let path = ticking("missed");
    let server = start(&["--file", path.to_str().unwrap()]);
    let mut http = server.http();
    let t = Instant::now();
    while !http.run("get ticks count").contains("\"count\":1") {
        assert!(t.elapsed() < Duration::from_secs(20), "never ran");
        std::thread::sleep(Duration::from_millis(10));
    }
    let (code, log) = server.terminate();
    assert_eq!(code, 0, "{log}");
    let hour = now_ms() - now_ms().rem_euclid(HOUR);
    let (ticks, mark) = ran(&path);
    assert_eq!(ticks, 1, "{log}");
    assert!(mark == Some(hour) || mark == Some(hour - HOUR), "{mark:?}");

    // A signal as the server starts: the run whole, or none of it.
    let path = ticking("signalled");
    let server = start(&["--file", path.to_str().unwrap()]);
    let (code, log) = server.terminate();
    assert_eq!(code, 0, "{log}");
    match ran(&path) {
        (0, None) | (1, Some(_)) => {}
        other => panic!("a run cut in two: {other:?}\n{log}"),
    }
}

/// A server that cannot have its port exits before its scheduler starts:
/// the file as it was, the slot still due.
#[test]
fn a_server_failing_at_its_port_writes_no_run() {
    let path = ticking("unlistened");
    let before = std::fs::read(&path).unwrap();
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = held.local_addr().unwrap().to_string();
    let p = path.to_str().unwrap();
    let (code, log) = ended(
        command(&["--file", p, "--http", &addr]),
        Duration::from_secs(30),
    );
    assert_eq!(code, 1, "{log}");
    assert_eq!(std::fs::read(&path).unwrap(), before, "{log}");
    assert_eq!(ran(&path), (0, None));
}

/// `--http-read-only` runs no schedule: the server says how many it holds
/// unrun, and its file keeps their slots due.
#[test]
fn a_read_only_server_runs_none_and_says_so() {
    let path = ticking("read-only");
    let server = start(&["--file", path.to_str().unwrap(), "--http-read-only"]);
    assert!(
        server.logged(
            "1 scheduled statement(s) held unrun",
            Duration::from_secs(10)
        ),
        "{}",
        server.log.lock().unwrap()
    );
    assert!(server.http().run("get ticks count").contains("\"count\":0"));
    let (code, log) = server.terminate();
    assert_eq!(code, 0, "{log}");
    assert_eq!(ran(&path), (0, None));
}

/// A `--http-read-only` replica says so as its primary's first schedule
/// reaches it, as it would have at its start: never one held unrun without
/// a word.
#[test]
fn a_read_only_replica_says_so_as_a_schedule_reaches_it() {
    const TOKEN: &str = "repl-token";
    let (pf, rf) = (tmp("scheduled", "rp.fennec"), tmp("scheduled", "rr.fennec"));
    let (pf, rf) = (pf.to_str().unwrap(), rf.to_str().unwrap());
    let primary = start(&["--file", pf, "--replication-token", TOKEN]);
    let mut p = primary.http();
    p.run("create collection ticks (slot timestamp, at timestamp)");
    let upstream = format!("http://127.0.0.1:{}", primary.port);
    let replica = start(&[
        "--file",
        rf,
        "--replication-token",
        TOKEN,
        "--replica-of",
        &upstream,
        "--http-read-only",
    ]);
    let mut r = replica.http();
    let t = Instant::now();
    while r.query("get ticks count").is_err() {
        assert!(t.elapsed() < Duration::from_secs(20), "never followed");
        std::thread::sleep(Duration::from_millis(10));
    }
    p.run("create statement tick every 1d as insert ticks {slot: slot(), at: now()}");
    assert!(
        replica.logged(
            "1 scheduled statement(s) held unrun: --http-read-only runs none",
            Duration::from_secs(20)
        ),
        "{}",
        replica.log.lock().unwrap()
    );
    let (code, log) = replica.terminate();
    assert_eq!(code, 0, "{log}");
    let _ = primary.terminate();
}

/// A directory holding `acme.fennec`, `tick` every hour defined a day ago,
/// and `beta.fennec`, holding none, as `fennec restore` leaves a node's.
fn node_dir(tag: &str) -> std::path::PathBuf {
    let dir = tmp("scheduled", tag);
    std::fs::create_dir_all(&dir).unwrap();
    let acme = ticking(&format!("{tag}-acme"));
    std::fs::rename(&acme, dir.join("acme.fennec")).unwrap();
    let _ = std::fs::remove_file(acme.with_extension("fennec.lock"));
    drop(fennec_core::fs::open(dir.join("beta.fennec")).unwrap());
    dir
}

/// A tenant node runs its tenants' schedules (F3's slice 4): started with
/// no hint, it opens each tenant once in the background to learn which
/// hold one, writes the hint naming those alone, and runs the latest
/// missed slot of each once; the shutdown lands it whole.
#[test]
fn a_node_learns_its_hint_and_runs_a_tenant_s_missed_slot() {
    let dir = node_dir("learn");
    let server = start(&["--admin-token", "adm", "--dir", dir.to_str().unwrap()]);
    assert!(
        server.logged(
            "no schedule hint: opening each of its 2 tenant(s)",
            Duration::from_secs(10)
        ),
        "{}",
        server.log.lock().unwrap()
    );
    let mut http = server.http();
    let t = Instant::now();
    while !(http.query_at("/t/acme", "get ticks count")).is_ok_and(|a| a.contains("\"count\":1")) {
        assert!(t.elapsed() < Duration::from_secs(20), "never ran");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        server.logged(
            "1 of 2 tenant(s) hold a scheduled statement",
            Duration::from_secs(10)
        ),
        "{}",
        server.log.lock().unwrap()
    );
    let (code, log) = server.terminate();
    assert_eq!(code, 0, "{log}");
    let hint = std::fs::read_to_string(dir.join("schedules.hint")).unwrap();
    assert!(hint.lines().any(|l| l.starts_with("acme ")), "{hint}");
    assert!(!hint.contains("beta"), "{hint}");
    let hour = now_ms() - now_ms().rem_euclid(HOUR);
    let (ticks, mark) = ran(&dir.join("acme.fennec"));
    assert_eq!(ticks, 1, "{log}");
    assert!(mark == Some(hour) || mark == Some(hour - HOUR), "{mark:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A node taking no writes runs none: a tenant holding a schedule says
/// so as it opens, and its slots stay due.
#[test]
fn a_read_only_node_says_what_it_holds_unrun() {
    let dir = node_dir("read-only");
    let server = start(&[
        "--admin-token",
        "adm",
        "--dir",
        dir.to_str().unwrap(),
        "--http-read-only",
    ]);
    let answer = server.http().query_at("/t/acme", "get ticks count");
    assert!(answer.is_ok_and(|a| a.contains("\"count\":0")));
    assert!(
        server.logged(
            "tenant `acme`: 1 scheduled statement(s) held unrun: this node runs no scheduler",
            Duration::from_secs(10)
        ),
        "{}",
        server.log.lock().unwrap()
    );
    let (code, log) = server.terminate();
    assert_eq!(code, 0, "{log}");
    assert!(!dir.join("schedules.hint").exists());
    assert_eq!(ran(&dir.join("acme.fennec")), (0, None));
    let _ = std::fs::remove_dir_all(&dir);
}

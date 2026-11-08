//! Scheduled statements run on a tenant node
//! (`docs/design/named-statements.md`, F3's slice 4): one scheduler a
//! node, a queue of tenants by name, each opened when its slot comes and
//! closed after its run, the run no use of it; a hint naming the tenants
//! that hold a schedule and those alone, entered before a first schedule
//! lands and by every way a file comes on, holding a closed tenant's
//! pending marks, and learned again where it is gone; a run from each
//! tenant due before a second from any; a frozen tenant passed over; the
//! tenant's quota asked as a run lands, and no rate charged.
//!
//! The registry runs in process, its scheduler driven by the time each
//! test names (`schedule::Node::pass`), no thread beside it. Times are
//! read off the clock: a tenant's open asks its slots of the clock it
//! reads, and a test's look comes after.

use fennec_core::prelude::*;
use fennec_http::schedule::{hint, Node, Outcome, Run};
use fennec_http::tenants::{Tenant, Tenants};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;

fn now_ms() -> i64 {
    fennec_core::time::now_ms().unwrap()
}

/// The start of the clock's hour.
fn hour() -> i64 {
    let now = now_ms();
    now - now.rem_euclid(HOUR)
}

struct Nd {
    dir: PathBuf,
    tenants: Arc<Tenants>,
    node: Arc<Node>,
}

impl Drop for Nd {
    fn drop(&mut self) {
        // The last of the directory's users removes it: a restart's next
        // registry takes it over first.
        if !self.dir.join(".kept").exists() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fennec-node-scheduled-{tag}-{}-{}",
        std::process::id(),
        std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A node over `dir`, its scheduler attached and no thread running it:
/// its hint read, or, with none, learned at once.
fn over(dir: &PathBuf, build: impl FnOnce(Tenants) -> Tenants) -> Nd {
    fennec_ql::install();
    let tenants = Arc::new(build(Tenants::new(dir).unwrap()));
    let node = Node::attach(&tenants, false);
    if node.learning() {
        node.learn();
    }
    Nd {
        dir: dir.clone(),
        tenants,
        node,
    }
}

fn node(tag: &str) -> Nd {
    over(&scratch(tag), |t| t)
}

/// The node over the same directory again, as a restart has it: every
/// tenant of the one before let go first -- closed where `clean`, its
/// pending marks kept by the hint, or dropped as a crash drops them.
fn restart(nd: Nd, clean: bool) -> Nd {
    if clean {
        nd.tenants.close_idle(Duration::ZERO);
        nd.node.write_hint().unwrap();
    }
    let dir = nd.dir.clone();
    std::fs::write(dir.join(".kept"), "").unwrap();
    drop(nd);
    std::fs::remove_file(dir.join(".kept")).unwrap();
    over(&dir, |t| t)
}

fn exec(t: &Tenant, q: &str) -> Response {
    let mut g = t.db.write().unwrap();
    let mut last = Response::Affected(0);
    for s in fennec_ql::parse(q).unwrap_or_else(|e| panic!("{q}: {e}")) {
        last = g.execute(&s).unwrap_or_else(|e| panic!("{q}: {e}"));
    }
    last
}

/// A tenant made through the registry, `texts` defined by a clock reading
/// `at` -- the schedules' `since` -- and closed again.
fn tenant(nd: &Nd, name: &str, at: i64, texts: &[&str]) {
    let t = nd.tenants.create(name).unwrap();
    {
        let mut g = t.db.write().unwrap();
        g.set_clock(Some(at));
        for q in texts {
            for s in fennec_ql::parse(q).unwrap() {
                g.execute(&s).unwrap_or_else(|e| panic!("{q}: {e}"));
            }
        }
        g.set_clock(None);
    }
    drop(t);
    nd.tenants.close_idle(Duration::ZERO);
}

fn count(nd: &Nd, name: &str, collection: &str) -> i64 {
    let t = nd.tenants.get(name).unwrap();
    let g = t.db.read().unwrap();
    let q = fennec_ql::parse_one(&format!("get {collection} count")).unwrap();
    let r = g.query(&q, &[]).unwrap();
    match &r.rows().unwrap().rows[0].values[0] {
        Value::Int(n) => *n,
        other => panic!("{other:?}"),
    }
}

/// The hint's lines by tenant, its later line where named twice.
fn hinted(dir: &std::path::Path) -> std::collections::HashMap<String, hint::Line> {
    let mut out = std::collections::HashMap::new();
    let read = hint::read(dir, &mut |n, l| {
        out.insert(n.to_string(), l);
    });
    read.unwrap().expect("a hint");
    out
}

fn open(nd: &Nd) -> Vec<String> {
    nd.tenants.stats().open.into_iter().map(|o| o.0).collect()
}

fn names(runs: &[(String, Run)]) -> Vec<&str> {
    runs.iter().map(|(t, _)| t.as_str()).collect()
}

const TICKS: &str = "create collection ticks (slot timestamp, at timestamp)";
const TICK: &str = "create statement tick every 1h as insert ticks {slot: slot(), at: now()}";
/// Finds nothing: its runs land no block.
const TIDY: &str = "create statement tidy every 1h scans as del ticks where slot = 0";

/// A closed tenant is opened as its slot comes, runs it, and is closed
/// again: the run is no use of it. A tenant holding no schedule is never
/// opened for one, and the slot runs once.
#[test]
fn a_closed_tenant_opens_as_its_slot_comes_and_closes_after_its_run() {
    let nd = node("closed");
    let h = hour();
    tenant(&nd, "acme", h + 1, &[TICKS, TICK]);
    tenant(&nd, "beta", h + 1, &[TICKS]);
    assert!(open(&nd).is_empty());
    let named: Vec<String> = nd.node.named().into_iter().map(|n| n.0).collect();
    assert_eq!(named, ["acme"]);

    // Before its slot nothing opens.
    assert!(nd.node.pass(h + 30 * MINUTE).is_empty());
    assert_eq!(nd.node.opens(), 0);
    let runs = nd.node.pass(h + HOUR + MINUTE);
    assert_eq!(names(&runs), ["acme"]);
    assert_eq!(
        (runs[0].1.slot, runs[0].1.outcome),
        (h + HOUR, Outcome::Ran)
    );
    assert_eq!(nd.node.opens(), 1, "acme alone opened");
    assert!(
        open(&nd).is_empty(),
        "closed after its run: {:?}",
        open(&nd)
    );
    assert!(nd.node.pass(h + HOUR + 2 * MINUTE).is_empty(), "once");
    assert_eq!(count(&nd, "acme", "ticks"), 1);
}

/// A run's open evicts no tenant a client touched, under the memory
/// ceiling, and leaves an open tenant's `last_used` as it was: a tenant a
/// client used, idle past `--idle-close`, closes as if no run had come.
#[test]
fn a_run_is_no_use_of_a_tenant_and_evicts_no_hot_one() {
    let dir = scratch("hot");
    let nd = over(&dir, |t| t.with_max_memory(1));
    let h = hour();
    tenant(&nd, "acme", h + 1, &[TICKS, TICK]);
    // Rows of its own, so the ceiling is passed with it open.
    tenant(
        &nd,
        "hot",
        h + 1,
        &[TICKS, "put ticks {slot: 1}; put ticks {slot: 2}"],
    );
    drop(nd.tenants.get("hot").unwrap());
    assert_eq!(open(&nd), ["hot"]);
    let runs = nd.node.pass(h + HOUR + MINUTE);
    assert_eq!(names(&runs), ["acme"]);
    assert_eq!(open(&nd), ["hot"], "the hot tenant stays, the run's goes");

    // Open by a client, then idle: a run leaves `last_used` where the
    // client left it.
    drop(nd.tenants.get("acme").unwrap());
    std::thread::sleep(Duration::from_millis(300));
    let runs = nd.node.pass(h + 2 * HOUR + MINUTE);
    assert_eq!(names(&runs), ["acme"]);
    assert!(
        open(&nd).contains(&"acme".to_string()),
        "a client's open stays"
    );
    nd.tenants.close_idle(Duration::from_millis(200));
    assert!(
        open(&nd).is_empty(),
        "idle as the client left it: {:?}",
        open(&nd)
    );
}

/// A start reads the hint: it opens no tenant before its slot, and none
/// that holds no schedule. The hint removed, the node learns it again,
/// opening each tenant once.
#[test]
fn a_start_opens_no_tenant_before_its_slot_and_a_hint_removed_is_learned_again() {
    let nd = node("start");
    let h = hour();
    tenant(&nd, "acme", h + 1, &[TICKS, TICK]);
    tenant(&nd, "beta", h + 1, &[TICKS]);
    let nd = restart(nd, true);
    assert!(!nd.node.learning());
    assert_eq!(nd.node.named(), [("acme".to_string(), Some(h + HOUR))]);
    assert!(nd.node.pass(h + 30 * MINUTE).is_empty());
    assert_eq!(nd.node.opens(), 0, "nothing before its slot");
    assert_eq!(names(&nd.node.pass(h + HOUR + MINUTE)), ["acme"]);
    assert_eq!(nd.node.opens(), 1, "acme alone");

    std::fs::remove_file(nd.dir.join(hint::FILE)).unwrap();
    let nd = restart(nd, false);
    assert!(!nd.node.learning(), "learned at once by the test");
    assert_eq!(nd.node.opens(), 2, "each tenant once");
    let named: Vec<String> = nd.node.named().into_iter().map(|n| n.0).collect();
    assert_eq!(named, ["acme"]);
    assert!(open(&nd).is_empty());
    let read = hinted(&nd.dir);
    assert_eq!(read.keys().collect::<Vec<_>>(), ["acme"]);
}

/// A tenant enters the hint, synced, before the block bringing its first
/// schedule lands: a definition put back with its block leaves it named on
/// disk -- a crash past it would find it -- and a look then finds none
/// and the tenant leaves the hint as it closes.
#[test]
fn a_tenant_enters_the_hint_before_its_first_schedule_lands() {
    let nd = node("enter");
    let t = nd.tenants.create("acme").unwrap();
    exec(&t, TICKS);
    let mut g = t.db.write().unwrap();
    g.begin().unwrap();
    g.execute(&fennec_ql::parse_one(TICK).unwrap()).unwrap();
    g.rollback();
    assert!(!g.holds_schedules());
    drop(g);
    drop(t);
    let read = hinted(&nd.dir);
    assert!(
        read.contains_key("acme"),
        "named before it landed: {read:?}"
    );

    // As a crash leaves it: the next start looks at it at once, finds
    // nothing, and it leaves the hint as it closes.
    let nd = restart(nd, false);
    assert_eq!(nd.node.named(), [("acme".to_string(), None)]);
    assert!(nd.node.pass(now_ms()).is_empty());
    assert!(nd.node.named().is_empty(), "{:?}", nd.node.named());
    assert!(open(&nd).is_empty());
    assert!(!hinted(&nd.dir).contains_key("acme"));

    // Its last schedule dropped, it leaves as it closes.
    let h = hour();
    tenant(&nd, "beta", h + 1, &[TICKS, TICK]);
    assert!(hinted(&nd.dir).contains_key("beta"));
    let t = nd.tenants.get("beta").unwrap();
    exec(&t, "drop statement tick");
    drop(t);
    assert!(
        nd.node.named().iter().any(|n| n.0 == "beta"),
        "until it closes"
    );
    nd.tenants.close_idle(Duration::ZERO);
    nd.node.write_hint().unwrap();
    assert!(!hinted(&nd.dir).contains_key("beta"));
}

/// A definition the hint cannot name is refused, nothing of it written: a
/// schedule a start would not find is never held. The hint written again,
/// it is taken.
#[test]
fn a_definition_the_hint_cannot_name_is_refused() {
    let nd = node("unnamed");
    let t = nd.tenants.create("acme").unwrap();
    exec(&t, TICKS);
    // The hint's place taken by a directory: no line appends to it.
    let path = nd.dir.join(hint::FILE);
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let refused =
        t.db.write()
            .unwrap()
            .execute(&fennec_ql::parse_one(TICK).unwrap());
    assert!(
        matches!(&refused, Err(Error::Io(m)) if m.contains("schedule hint")),
        "{refused:?}"
    );
    assert!(!t.db.read().unwrap().holds_schedules());
    std::fs::remove_dir(&path).unwrap();
    exec(&t, TICK);
    assert!(hinted(&nd.dir).contains_key("acme"));
}

/// A database holding a schedule, as an install's image.
fn image(at: i64) -> Vec<u8> {
    fennec_ql::install();
    let mut db = Database::new();
    db.allow_schedules();
    db.set_clock(Some(at));
    for q in [TICKS, TICK] {
        db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
    }
    db.snapshot()
}

/// Every way a file comes onto the node enters its tenant: an install --
/// a move's target -- before its file is in place; a file put into the
/// directory behind the node, as `fennec restore` puts one, at its first
/// open. Each runs its slot.
#[test]
fn a_tenant_installed_or_restored_in_is_learned() {
    let nd = node("installed");
    let h = hour();
    nd.tenants.import_as("inst", &image(h + 1), None).unwrap();
    assert!(hinted(&nd.dir).contains_key("inst"));

    // Written into the directory behind the node: unknown until opened.
    {
        let mut db = fennec_core::fs::open(nd.dir.join("rest.fennec")).unwrap();
        db.allow_schedules();
        db.set_clock(Some(h + 1));
        for q in [TICKS, TICK] {
            db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
        }
        if let Some(sync) = db.flush().unwrap() {
            sync().unwrap();
        }
    }
    assert!(!nd.node.named().iter().any(|n| n.0 == "rest"));
    drop(nd.tenants.get("rest").unwrap());
    assert!(nd.node.named().iter().any(|n| n.0 == "rest"));
    nd.tenants.close_idle(Duration::ZERO);

    let mut runs = names(&nd.node.pass(h + HOUR + MINUTE))
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    runs.sort();
    assert_eq!(runs, ["inst", "rest"]);
}

/// A thousand tenants due at once, two statements each: one run from each
/// before a second from any, every slot run once.
#[test]
fn a_thousand_tenants_due_at_once_each_run_once_before_any_twice() {
    const N: usize = 1000;
    let dir = scratch("thousand");
    let h = hour();
    // One file copied a thousand times: each a tenant of its own, made
    // without a thousand fsyncs.
    {
        fennec_ql::install();
        let mut db = fennec_core::fs::open(dir.join("t0000.fennec")).unwrap();
        db.allow_schedules();
        db.set_clock(Some(h + 1));
        for q in [TICKS, TIDY, TIDY.replace("tidy", "sweep").as_str()] {
            db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
        }
        if let Some(sync) = db.flush().unwrap() {
            sync().unwrap();
        }
    }
    for i in 1..N {
        std::fs::copy(
            dir.join("t0000.fennec"),
            dir.join(format!("t{i:04}.fennec")),
        )
        .unwrap();
    }
    let nd = over(&dir, |t| t);
    assert_eq!(nd.node.named().len(), N);
    let runs = nd.node.pass(h + HOUR + MINUTE);
    assert_eq!(runs.len(), 2 * N);
    let (first, second) = runs.split_at(N);
    let distinct = |half: &[(String, Run)]| {
        let mut n: Vec<&str> = half.iter().map(|r| r.0.as_str()).collect();
        n.sort();
        n.dedup();
        n.len()
    };
    assert_eq!(distinct(first), N, "each once before any twice");
    assert_eq!(distinct(second), N);
    assert!(runs.iter().all(|r| r.1.outcome == Outcome::Ran));
    assert!(open(&nd).is_empty());
    assert!(
        nd.node.pass(h + HOUR + 2 * MINUTE).is_empty(),
        "each slot once"
    );
}

/// A run that wrote nothing marks its slot in memory; the tenant closing,
/// the node keeps the mark by name with where its file stood, the hint
/// across a restart, and hands it back as it opens -- the slot not run
/// again -- until its file changed: then the slot runs again over what is
/// there. A mark handed back lands with the next write.
#[test]
fn a_closed_tenant_s_pending_mark_is_kept_until_its_file_changes() {
    let nd = node("pending");
    let h = hour();
    tenant(&nd, "acme", h + 1, &[TICKS, TIDY]);
    let runs = nd.node.pass(h + HOUR + MINUTE);
    assert_eq!(names(&runs), ["acme"]);
    assert!(open(&nd).is_empty());
    let kept = hinted(&nd.dir)["acme"].kept.clone();
    let kept = kept.expect("the mark kept as it closed");
    assert_eq!(kept.marks[0].1.at, h + HOUR);

    // Across a restart, handed back as it opens: nothing due.
    let nd = restart(nd, false);
    let t = nd.tenants.get("acme").unwrap();
    assert!(t.db.read().unwrap().due(h + HOUR + 2 * MINUTE)[0]
        .run
        .is_none());
    drop(t);

    // Closed again, then its file written behind the node: the mark counts
    // for nothing, and the slot is due again.
    nd.tenants.close_idle(Duration::ZERO);
    {
        let mut db = fennec_core::fs::open(nd.dir.join("acme.fennec")).unwrap();
        db.execute(&fennec_ql::parse_one("put ticks {slot: 5}").unwrap())
            .unwrap();
        if let Some(sync) = db.flush().unwrap() {
            sync().unwrap();
        }
    }
    let t = nd.tenants.get("acme").unwrap();
    let due = t.db.read().unwrap().due(h + HOUR + 2 * MINUTE);
    assert_eq!(
        due[0].run,
        Some((h + HOUR, 0)),
        "run again over what is there"
    );
    drop(t);
    nd.tenants.close_idle(Duration::ZERO);
    assert_eq!(names(&nd.node.pass(h + HOUR + 3 * MINUTE)), ["acme"]);

    // Handed back, it lands with the next write.
    let t = nd.tenants.get("acme").unwrap();
    assert_eq!(t.db.read().unwrap().pending().len(), 1);
    exec(&t, "put ticks {slot: 1}");
    let g = t.db.read().unwrap();
    assert!(g.pending().is_empty());
    assert_eq!(g.last_mark("tidy").map(|m| m.at), Some(h + HOUR));
}

/// A frozen tenant -- a move's source -- runs nothing; thawed, a move
/// undone, it runs the slot it passed over, once.
#[test]
fn a_frozen_tenant_runs_nothing_and_thawed_runs_its_slot() {
    let nd = node("frozen");
    let h = hour();
    tenant(&nd, "acme", h + 1, &[TICKS, TICK]);
    nd.tenants.freeze("acme").unwrap();
    assert!(nd.node.pass(h + HOUR + MINUTE).is_empty());
    nd.tenants.thaw("acme").unwrap();
    let runs = nd.node.pass(h + HOUR + MINUTE);
    assert_eq!(names(&runs), ["acme"]);
    assert_eq!(runs[0].1.slot, h + HOUR);
    assert_eq!(count(&nd, "acme", "ticks"), 1);
}

/// The tenant's quota is asked of a run's block as it lands: a run that
/// grows the tenant at its rows' quota is held (507), its slot left due,
/// and runs once a client's delete made room; a purge at the quota is let
/// through, and so is a client's delete carrying a pending mark.
#[test]
fn a_run_at_its_tenant_s_quota_is_held_until_room_comes() {
    let nd = node("quota");
    let h = hour();
    tenant(
        &nd,
        "acme",
        h + 1,
        &[TICKS, TICK, "put ticks {slot: 1}; put ticks {slot: 2}"],
    );
    let set = fennec_http::quota::Set::parse(r#"{"rows": 2}"#).unwrap();
    nd.tenants.set_quota("acme", set).unwrap();
    let outcomes = |runs: &[(String, Run)]| runs.iter().map(|r| r.1.outcome).collect::<Vec<_>>();
    let held = nd.node.pass(h + HOUR + MINUTE);
    assert_eq!(outcomes(&held), [Outcome::Held(507)]);
    assert_eq!(count(&nd, "acme", "ticks"), 2);
    // Room comes with a client's delete, which moves the counter: tried
    // again a minute on, the slot it held.
    let t = nd.tenants.get("acme").unwrap();
    exec(&t, "del ticks where slot = 1");
    drop(t);
    assert!(
        nd.node.pass(h + HOUR + MINUTE + 30_000).is_empty(),
        "a minute on"
    );
    let runs = nd.node.pass(h + HOUR + 3 * MINUTE);
    assert_eq!(outcomes(&runs), [Outcome::Ran]);
    assert_eq!(runs[0].1.slot, h + HOUR);
    assert_eq!(count(&nd, "acme", "ticks"), 2);

    // At the quota again: the tick held, and a purge -- only taking away --
    // let through, run after it by name.
    let t = nd.tenants.get("acme").unwrap();
    exec(
        &t,
        "create statement vacuum every 1h scans as del ticks where slot < 1000",
    );
    drop(t);
    let runs = nd.node.pass(h + 2 * HOUR + MINUTE);
    assert_eq!(
        outcomes(&runs),
        [Outcome::Held(507), Outcome::Ran],
        "{runs:?}"
    );
    assert_eq!(
        count(&nd, "acme", "ticks"),
        1,
        "the purge took the client's row"
    );

    // The tick lands, the tenant at its quota once more; the purge finds
    // nothing, its mark pending; a client's purge at the quota lands it.
    let runs = nd.node.pass(h + 3 * HOUR + MINUTE);
    assert_eq!(outcomes(&runs), [Outcome::Ran, Outcome::Ran], "{runs:?}");
    let t = nd.tenants.get("acme").unwrap();
    assert_eq!(count(&nd, "acme", "ticks"), 2);
    assert_eq!(t.db.read().unwrap().pending().len(), 1);
    exec(&t, &format!("del ticks where slot = {}", h + HOUR));
    let g = t.db.read().unwrap();
    assert!(g.pending().is_empty(), "landed with the client's purge");
    assert_eq!(g.last_mark("vacuum").map(|m| m.at), Some(h + 3 * HOUR));
}

/// A run's block charges no rate of its tenant's: a tenant's clients are
/// not slowed by its schedule.
#[test]
fn a_run_charges_no_rate() {
    let nd = node("rate");
    let h = hour();
    tenant(&nd, "acme", h + 1, &[TICKS, TICK]);
    let set = fennec_http::quota::Set::parse(r#"{"write_bytes": 1}"#).unwrap();
    nd.tenants.set_quota("acme", set).unwrap();
    let cfg = fennec_http::Config {
        addr: "127.0.0.1:0".into(),
        admin_token: Some("adm".into()),
        ..fennec_http::Config::default()
    };
    let server = fennec_http::Server::with_tenants(Arc::clone(&nd.tenants), cfg);
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    // Open through the run, as a client's: its buckets are the open
    // tenant's.
    let _open = nd.tenants.get("acme").unwrap();
    assert_eq!(names(&nd.node.pass(h + HOUR + MINUTE)), ["acme"]);
    let (status, body) = query(port, "acme", "put ticks {slot: 1}");
    assert_eq!(status, 200, "{body}");
    // The client's own write is charged: the next waits its debt out.
    let (status, body) = query(port, "acme", "put ticks {slot: 2}");
    assert_eq!(status, 429, "{body}");
}

/// The node's one shutdown, for a scheduler thread a test starts: it ends
/// as its registry goes.
fn ignored(_: i32) {}

/// Waits for what must happen, bounded.
fn until(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !f() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A database holding `tick`, due now -- defined two hours ago -- and `pad`
/// rows of `bytes` each: an install's image.
fn due_image(pad: usize, bytes: usize) -> Vec<u8> {
    fennec_ql::install();
    let mut db = Database::new();
    db.allow_schedules();
    db.set_clock(Some(now_ms() - 2 * HOUR));
    for q in [TICKS, TICK, "create collection pad (body text)"] {
        db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
    }
    db.set_clock(None);
    let row = format!("insert pad {{body: '{}'}}", "x".repeat(bytes));
    let row = fennec_ql::parse_one(&row).unwrap();
    for _ in 0..pad {
        db.execute(&row).unwrap();
    }
    db.snapshot()
}

/// An install racing a scheduler that looks all the time -- its thread
/// running and a loop of looks beside it -- runs its slot and stays named:
/// its first look queued before the file was in place found none, and the
/// tenant left the queue and the hint for good, never run. Its look is
/// asked of the node, never by opening it: a client's open names it again.
#[test]
fn an_install_racing_a_looking_scheduler_runs_and_stays_named() {
    let nd = node("race");
    nd.node.set_arriving(0);
    nd.node.start(ignored).unwrap();
    let image = due_image(32, 1 << 20);
    let done = Arc::new(AtomicBool::new(false));
    let looking = {
        let (node, done) = (Arc::clone(&nd.node), Arc::clone(&done));
        std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                node.pass(now_ms());
            }
        })
    };
    nd.tenants.import("big", &image).unwrap();
    until("its look opened it", || nd.node.opens() >= 1);
    done.store(true, Ordering::SeqCst);
    looking.join().unwrap();
    assert_eq!(count(&nd, "big", "ticks"), 1);
    assert!(nd.node.named().iter().any(|n| n.0 == "big"));
    nd.node.write_hint().unwrap();
    assert!(hinted(&nd.dir).contains_key("big"));
}

/// An installed tenant -- a move's target -- runs nothing before it
/// arrived (`schedule::ARRIVING`), however it is opened meanwhile: a node
/// learns of no flip without a lease, and a move undone takes the file
/// back within the move's own call, gone by then -- its source runs the
/// slot alone. Arrived, it runs its slot.
#[test]
fn an_installed_tenant_runs_nothing_until_it_arrived_and_one_taken_back_none() {
    let nd = node("arrive");
    let now = now_ms();
    nd.tenants
        .import_as("moved", &due_image(0, 0), Some(0x5eed))
        .unwrap();
    // As the install's route opens it, to answer with its lineage.
    drop(nd.tenants.get("moved").unwrap());
    let arrived = nd.node.named()[0].1.expect("waits for its arrival");
    assert!(arrived >= now + MINUTE, "{arrived} against {now}");
    assert!(nd.node.pass(arrived - 1).is_empty());
    // Taken back, as a move undone takes it: nothing to run.
    assert_eq!(nd.tenants.reclaim("moved", &[0x5eed]).ok(), Some(true));
    assert!(nd.node.pass(arrived).is_empty());
    assert!(nd.node.named().is_empty(), "{:?}", nd.node.named());
    assert_eq!(nd.node.opens(), 0);

    nd.tenants
        .import_as("moved", &due_image(0, 0), Some(0x5eee))
        .unwrap();
    let arrived = nd.node.named()[0].1.expect("waits for its arrival");
    assert_eq!(names(&nd.node.pass(arrived)), ["moved"]);
}

/// A schedule replacing another on a tenant the hint names already is
/// named again, synced, before it lands: a crash before the hint's next
/// whole write finds the tenant to look at at once, never at the slot the
/// one replaced held -- a yearly one's, and the hourly runs till then
/// lost.
#[test]
fn a_replaced_schedule_is_looked_at_at_once_after_a_crash() {
    let nd = node("replaced");
    let now = now_ms();
    let yearly = "create statement tick at '0 0 1 1 *' as insert ticks {slot: slot(), at: now()}";
    tenant(&nd, "acme", now - 2 * HOUR, &[TICKS, yearly]);
    nd.node.write_hint().unwrap();
    let held = hinted(&nd.dir)["acme"].next.expect("its next slot");
    assert!(held > now + 24 * HOUR, "the new year's");
    let t = nd.tenants.get("acme").unwrap();
    {
        let mut g = t.db.write().unwrap();
        g.set_clock(Some(now - 2 * HOUR));
        let hourly = TICK.replace("create statement", "create or replace statement");
        g.execute(&fennec_ql::parse_one(&hourly).unwrap()).unwrap();
        g.set_clock(None);
    }
    drop(t);
    // A crash: the hint not written whole since.
    let nd = restart(nd, false);
    assert_eq!(nd.node.named(), [("acme".to_string(), None)], "at once");
    assert_eq!(names(&nd.node.pass(now_ms())), ["acme"]);
}

/// A closed tenant its node's lease does not let it write is not opened
/// for its slot -- a node whose lease lapsed opened each of its scheduled
/// tenants every minute to find so -- and runs it once the lease names it.
#[test]
fn a_node_without_its_lease_opens_no_tenant_for_its_slot() {
    let dir = scratch("unleased");
    let h = hour();
    {
        let nd = over(&dir, |t| t);
        tenant(&nd, "acme", h + 1, &[TICKS, TICK]);
        tenant(&nd, "beta", h + 1, &[TICKS, TICK]);
        nd.node.write_hint().unwrap();
        std::fs::write(dir.join(".kept"), "").unwrap();
    }
    std::fs::remove_file(dir.join(".kept")).unwrap();
    let nd = over(&dir, |t| t.with_lease());
    for m in 1..=5 {
        assert!(nd.node.pass(h + HOUR + m * MINUTE).is_empty());
    }
    assert_eq!(nd.node.opens(), 0, "none opened");
    let primaries = Some(vec!["acme".to_string(), "beta".to_string()]);
    let lease = nd.tenants.lease().expect("a leased node");
    assert!(lease.grant(60_000, "e1", primaries).is_ok());
    let mut ran = names(&nd.node.pass(h + HOUR + 6 * MINUTE))
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    ran.sort();
    assert_eq!(ran, ["acme", "beta"]);
    assert_eq!(nd.node.opens(), 2);
}

/// A closed tenant whose slot its quota holds waits the hold's backoff out
/// before it is opened again -- a minute, then two, then four -- nothing
/// moving a closed tenant's rows: opened each minute, a node of tenants at
/// their quotas opened every one of them every minute.
#[test]
fn a_closed_tenant_s_held_slot_waits_its_backoff_out() {
    let nd = node("backoff");
    let h = hour();
    tenant(
        &nd,
        "acme",
        h + 1,
        &[TICKS, TICK, "put ticks {slot: 1}; put ticks {slot: 2}"],
    );
    let set = fennec_http::quota::Set::parse(r#"{"rows": 2}"#).unwrap();
    nd.tenants.set_quota("acme", set).unwrap();
    nd.tenants.close_idle(Duration::ZERO);
    let outcomes = |runs: &[(String, Run)]| runs.iter().map(|r| r.1.outcome).collect::<Vec<_>>();
    let t0 = h + HOUR + MINUTE;
    assert_eq!(outcomes(&nd.node.pass(t0)), [Outcome::Held(507)]);
    assert!(open(&nd).is_empty());
    let opens = nd.node.opens();
    assert_eq!(outcomes(&nd.node.pass(t0 + MINUTE)), [Outcome::Held(507)]);
    assert!(nd.node.pass(t0 + 2 * MINUTE).is_empty());
    assert_eq!(
        outcomes(&nd.node.pass(t0 + 3 * MINUTE)),
        [Outcome::Held(507)]
    );
    for m in 4..7 {
        assert!(nd.node.pass(t0 + m * MINUTE).is_empty());
    }
    assert_eq!(nd.node.opens(), opens + 2, "at its backoff's ends alone");
}

/// A slot its quota holds is listed `held` once the look that held it
/// closed the tenant and a request opened it again: the scheduler's state,
/// handed back to the queue's entry before the close, is what the tenant's
/// database is told as it opens (`Node::opened`). Yearly, so the slot held
/// is the one due for as long as the test runs.
#[test]
fn a_held_slot_is_listed_once_a_request_opens_its_tenant_again() {
    let nd = node("heldshown");
    let since = now_ms() - 400 * 24 * HOUR;
    tenant(
        &nd,
        "acme",
        since,
        &[
            TICKS,
            "create statement yearly at '0 0 1 1 *' as insert ticks {slot: slot(), at: now()}",
            "put ticks {slot: 1}; put ticks {slot: 2}",
        ],
    );
    let set = fennec_http::quota::Set::parse(r#"{"rows": 2}"#).unwrap();
    nd.tenants.set_quota("acme", set).unwrap();
    nd.tenants.close_idle(Duration::ZERO);
    let at = now_ms();
    let runs = nd.node.pass(at);
    let outcomes: Vec<_> = runs.iter().map(|r| r.1.outcome).collect();
    assert_eq!(outcomes, [Outcome::Held(507)]);
    assert!(open(&nd).is_empty(), "closed after its look");
    let cfg = fennec_http::Config {
        addr: "127.0.0.1:0".into(),
        admin_token: Some("adm".into()),
        ..fennec_http::Config::default()
    };
    let server = fennec_http::Server::with_tenants(Arc::clone(&nd.tenants), cfg);
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    let (status, body) = query(port, "acme", "statements");
    assert_eq!(status, 200, "{body}");
    let held = format!(
        r#""held":{{"since":"{}","slot":"{}","status":507}}"#,
        fennec_core::time::format_iso(at),
        fennec_core::time::format_iso(runs[0].1.slot),
    );
    assert!(body.contains(&held), "{held} in {body}");
}

/// A frozen tenant -- a move's source -- whose delete fails once it was
/// closed, its freeze gone with it, leaves the queue and the hint: named,
/// a look or a restart ran its slots into a file no router sends to.
#[test]
fn a_frozen_tenant_whose_delete_fails_leaves_the_hint() {
    let nd = node("leftfail");
    let h = hour();
    tenant(&nd, "acme", h + 1, &[TICKS, TICK]);
    nd.tenants.freeze("acme").unwrap();
    // Its identity's sidecar a directory: the delete puts it away, and
    // cannot.
    let sidecar = nd.dir.join("acme.instance");
    let _ = std::fs::remove_file(&sidecar);
    std::fs::create_dir_all(sidecar.join("held")).unwrap();
    assert!(nd.tenants.delete("acme").is_err());
    assert!(nd.dir.join("acme.fennec").exists());
    assert!(nd.node.named().is_empty(), "{:?}", nd.node.named());
    assert!(!hinted(&nd.dir).contains_key("acme"));
    assert!(nd.node.pass(h + HOUR + MINUTE).is_empty(), "nothing runs");
    std::fs::remove_dir_all(&sidecar).unwrap();
}

fn query(port: u16, tenant: &str, q: &str) -> (u16, String) {
    use std::io::{Read, Write};
    let mut body = String::from("{\"query\":");
    fennec_core::json::escape_into(&mut body, q);
    body.push('}');
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    write!(
        s,
        "POST /t/{tenant}/query HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: \
         close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    let status = out.get(9..12).and_then(|s| s.parse().ok()).unwrap_or(0);
    let body = out.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    (status, body.to_string())
}

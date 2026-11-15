//! `--follow` against a live server: every kind of change arrives, a broken
//! stream loses nothing, a stopped follower resumes without a copy, and a
//! copy cut short is made again.
//!
//! Marked `#[ignore]`: it needs PostgreSQL with `wal_level=logical`, which
//! the Makefile's server has --
//!
//! ```text
//! make pgvector-up
//! cargo test -p fennec-import --test all follow:: -- --ignored
//! make pgvector-down
//! ```
//!
//! Each test has its own table, slot and publication, so they run in
//! parallel; each drops its slot at the end, or the server would keep its
//! log for a follower that never comes back.

use fennec_core::prelude::*;
use fennec_import::follow::{self, Event, Follow};
use fennec_import::pg::Url;
use fennec_import::Options;
use fennec_wire::client::Client;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const DEFAULT_URL: &str = "postgres://postgres:fennec@127.0.0.1:55432/fennecbench";

fn url() -> Url {
    let s = std::env::var("FENNEC_TEST_PG_URL").unwrap_or_else(|_| DEFAULT_URL.to_string());
    Url::parse(&s).expect("invalid FENNEC_TEST_PG_URL")
}

/// The test server's URL, in another of its databases.
fn url_in(database: &str) -> Url {
    let mut u = url();
    u.database = database.to_string();
    u
}

fn sql(q: &str) {
    sql_at(&url(), q);
}

fn sql_at(u: &Url, q: &str) {
    let mut c = Client::connect(u).expect("could not connect (make pgvector-up?)");
    c.query(q).unwrap_or_else(|e| panic!("{q}: {e}"));
}

/// Drops a slot left from an earlier run, once the server has let it go:
/// a run that ended in a panic left its follower streaming until its
/// process exited, and its walsender holds the slot until it reads that.
/// `DROP_REPLICATION_SLOT ... WAIT` is the server's own wait for the slot
/// to be released, bounded by the client's wait for an answer.
fn forget_slot(u: &Url, name: &str) {
    let mut c = Client::connect(u).unwrap();
    let left = c
        .query(&format!(
            "select 1 from pg_replication_slots where slot_name = '{name}'"
        ))
        .unwrap();
    if !left.rows.is_empty() {
        let q = format!("DROP_REPLICATION_SLOT {name} WAIT");
        let mut r = Client::connect_replication(u).unwrap();
        r.query(&q).unwrap_or_else(|e| panic!("{q}: {e}"));
    }
}

/// A table of `rows` rows with a key, a title, a number, tags and a vector
/// of 768 -- 3 KB, so PostgreSQL TOASTs it and an update of the title
/// arrives without it.
fn table(name: &str, rows: usize) {
    let _ = Client::connect(&url())
        .unwrap()
        .query("create extension if not exists vector");
    forget_slot(&url(), name);
    for q in [
        format!("drop publication if exists {name}"),
        format!("drop table if exists {name}"),
        format!(
            "create table {name} (id bigint primary key, title text, n int, tags text[], embed vector(768))"
        ),
        format!(
            "insert into {name} select g, 'row ' || g, g, array['a', 'b'], \
             (select array_agg(sin(g * 1000 + i))::vector(768) from generate_series(1, 768) i) \
             from generate_series(1, {rows}) g"
        ),
    ] {
        sql(&q);
    }
}

struct Follower {
    db: Arc<RwLock<Database>>,
    stop: Arc<AtomicBool>,
    events: Arc<Mutex<Vec<String>>>,
    thread: Option<JoinHandle<fennec_core::error::Result<()>>>,
}

impl Follower {
    fn start(name: &str, db: Arc<RwLock<Database>>) -> Follower {
        Follower::start_at(url(), name, db)
    }

    /// A follower of `name` in the database `u` names.
    fn start_at(u: Url, name: &str, db: Arc<RwLock<Database>>) -> Follower {
        let stop = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(Vec::new()));
        let (s, e, d, name) = (
            Arc::clone(&stop),
            Arc::clone(&events),
            Arc::clone(&db),
            name.to_string(),
        );
        let thread = std::thread::spawn(move || {
            let follow = Follow {
                slot: name.clone(),
                publication: name.clone(),
            };
            let opts = Options::new(name.clone());
            let mut report = |ev: Event| e.lock().unwrap().push(format!("{ev:?}"));
            follow::run(
                &u,
                &name,
                &d,
                &opts,
                &follow,
                &s,
                &AtomicBool::new(false),
                &mut report,
            )
        });
        Follower {
            db,
            stop,
            events,
            thread: Some(thread),
        }
    }

    fn saw(&self, what: &str) -> bool {
        self.events.lock().unwrap().iter().any(|e| e.contains(what))
    }

    fn stop(mut self) -> Vec<String> {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap().unwrap();
        let events = self.events.lock().unwrap().clone();
        events
    }
}

/// A test that panics stops its follower too, which then ends its stream
/// and lets its slot go, rather than streaming on beside the other tests
/// until the process exits.
impl Drop for Follower {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// The table as the server has it, by id: title, n, tags, and the vector.
type Rows = Vec<(
    i64,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<Vec<f32>>,
)>;

fn source_rows(name: &str) -> Rows {
    let mut c = Client::connect(&url()).unwrap();
    let r = c
        .query(&format!(
            "select id, title, n, array_to_string(tags, ','), embed::text from {name} order by id"
        ))
        .unwrap();
    r.rows
        .iter()
        .map(|row| {
            let embed = row[4].as_ref().map(|s| {
                s.trim_matches(|c| c == '[' || c == ']')
                    .split(',')
                    .map(|x| x.parse::<f32>().unwrap())
                    .collect()
            });
            (
                row[0].as_ref().unwrap().parse().unwrap(),
                row[1].clone(),
                row[2].as_ref().map(|s| s.parse().unwrap()),
                row[3].clone(),
                embed,
            )
        })
        .collect()
}

fn mirror_rows(db: &RwLock<Database>, name: &str) -> Rows {
    let g = db.read().unwrap();
    let Ok(c) = g.collection(name) else {
        return Vec::new();
    };
    let mut ids = c.store.ids();
    ids.sort_unstable();
    ids.iter()
        .map(|&id| {
            let d = c.store.read(&c.schema, id).unwrap().unwrap();
            let text = |f: &str| match d.get(f) {
                Some(Value::Text(s)) => Some(s.clone()),
                _ => None,
            };
            let tags = match d.get("tags") {
                Some(Value::List(items)) => Some(
                    items
                        .iter()
                        .map(|v| match v {
                            Value::Text(s) => s.clone(),
                            other => format!("{other:?}"),
                        })
                        .collect::<Vec<_>>()
                        .join(","),
                ),
                _ => None,
            };
            let n = match d.get("n") {
                Some(Value::Int(n)) => Some(*n),
                _ => None,
            };
            let embed = match d.get("embed") {
                Some(Value::Vector(v)) => Some(v.clone()),
                _ => None,
            };
            (id as i64, text("title"), n, tags, embed)
        })
        .collect()
}

/// Waits until the mirror holds exactly the server's rows.
fn converge(db: &RwLock<Database>, name: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (want, got) = (source_rows(name), mirror_rows(db, name));
        if want == got {
            return;
        }
        if Instant::now() > deadline {
            let first = want
                .iter()
                .zip(&got)
                .find(|(a, b)| a != b)
                .map(|(a, b)| format!("{a:?}\nvs\n{b:?}"));
            panic!(
                "{name}: {} rows on the server, {} in the mirror; first difference: {first:?}",
                want.len(),
                got.len()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Drops the slot just after its follower stopped, which is what a user is
/// told to do to stop following for good: stopping ends the stream and
/// returns once the server has let the slot go
/// (`a_stopped_follower_lets_its_slot_go`), so the drop is never refused
/// as "active for PID". When the follower only closed its connection, the
/// slot stayed taken until the walsender read the close, which under load
/// came after this drop.
fn drop_slot(name: &str) {
    drop_slot_at(&url(), name);
}

fn drop_slot_at(u: &Url, name: &str) {
    sql_at(
        u,
        &format!(
            "select pg_drop_replication_slot('{name}') from pg_replication_slots where slot_name = '{name}'"
        ),
    );
    sql_at(u, &format!("drop publication if exists {name}"));
}

#[test]
#[ignore]
fn every_kind_of_change_arrives() {
    let t = "fennec_follow_kinds";
    table(t, 200);
    let f = Follower::start(t, Arc::new(RwLock::new(Database::new())));
    converge(&f.db, t);
    for q in [
        format!("insert into {t} values (1001, 'new', 1, array['x'], null)"),
        // Only the title: the vector is TOASTed and does not come along.
        format!("update {t} set title = 'retitled' where id <= 20"),
        format!("update {t} set tags = null, n = n * 2 where id between 21 and 30"),
        // A key that changes: the document moves.
        format!("update {t} set id = 5000 where id = 40"),
        format!("delete from {t} where id between 100 and 150"),
        // Several changes to one row in one transaction.
        format!(
            "begin; insert into {t} values (2000, 'a', 0, null, null); \
             update {t} set title = 'b' where id = 2000; \
             update {t} set title = 'c', n = 3 where id = 2000; commit"
        ),
        // Inserted and deleted before the commit: never there at all.
        format!("begin; insert into {t} values (3000, 'gone', 0, null, null); delete from {t} where id = 3000; commit"),
    ] {
        sql(&q);
    }
    converge(&f.db, t);
    sql(&format!("truncate {t}"));
    sql(&format!(
        "insert into {t} values (1, 'after the truncate', 1, null, null)"
    ));
    converge(&f.db, t);
    f.stop();
    drop_slot(t);
}

#[test]
#[ignore]
fn a_broken_stream_loses_nothing() {
    let t = "fennec_follow_broken";
    table(t, 100);
    let f = Follower::start(t, Arc::new(RwLock::new(Database::new())));
    converge(&f.db, t);
    for round in 0..3 {
        for i in 0..20 {
            sql(&format!(
                "update {t} set n = {} where id = {}",
                round * 100 + i,
                1 + (round * 20 + i) % 100
            ));
        }
        // The server ends the follower's connection: this follower's alone,
        // by its slot's holder. By `application_name` it ended every
        // test's follower running beside it, at any step of theirs.
        sql(&format!(
            "select pg_terminate_backend(active_pid) from pg_replication_slots \
             where slot_name = '{t}' and active"
        ));
        sql(&format!(
            "insert into {t} values ({}, 'during', 0, null, null)",
            500 + round
        ));
    }
    converge(&f.db, t);
    assert!(f.saw("Reconnecting"), "{:?}", f.events.lock().unwrap());
    f.stop();
    drop_slot(t);
}

#[test]
#[ignore]
fn a_stopped_follower_resumes_without_a_copy() {
    let t = "fennec_follow_resume";
    table(t, 50);
    let db = Arc::new(RwLock::new(Database::new()));
    let f = Follower::start(t, Arc::clone(&db));
    converge(&db, t);
    let first = f.stop();
    assert!(first.iter().any(|e| e.contains("Copied")), "{first:?}");

    // Written while nothing follows: the slot holds it.
    sql(&format!(
        "update {t} set title = 'while stopped' where id <= 10"
    ));
    sql(&format!("delete from {t} where id > 40"));

    let f = Follower::start(t, Arc::clone(&db));
    converge(&db, t);
    let second = f.stop();
    assert!(!second.iter().any(|e| e.contains("Copying")), "{second:?}");
    drop_slot(t);
}

/// Whether the server still has the slot taken by a connection.
fn slot_active(c: &mut Client, name: &str) -> bool {
    let r = c
        .query(&format!(
            "select active from pg_replication_slots where slot_name = '{name}'"
        ))
        .unwrap();
    r.rows.first().and_then(|row| row[0].as_deref()) == Some("t")
}

/// Waits until `f` has `what` among its events.
fn sees(f: &Follower, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f.saw(what) {
        assert!(
            Instant::now() < deadline,
            "no {what}: {:?}",
            f.events.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Waits until the walsender holding `slot` has read the log up to `lsn`.
fn decoded_up_to(c: &mut Client, slot: &str, lsn: &str) {
    let q = format!(
        "select r.sent_lsn >= '{lsn}'::pg_lsn from pg_stat_replication r \
         join pg_replication_slots s on s.active_pid = r.pid where s.slot_name = '{slot}'"
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while first_cell(&c.query(&q).unwrap()).as_deref() != Some("t") {
        assert!(Instant::now() < deadline, "{slot} never read up to {lsn}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn first_cell(r: &fennec_wire::client::QueryResult) -> Option<String> {
    r.rows.first()?.first()?.clone()
}

/// A follower that stops has let its slot go when it returns, even while
/// the server is busy decoding a transaction -- here 200 000 rows of a
/// table it does not follow, which its slot decodes all the same, the stop
/// made once the walsender has read up to their commit. Closing the
/// connection left the slot taken until the walsender next read its
/// socket, after that transaction: a slot dropped then was refused as
/// "active for PID", and a follower started again met its own slot taken.
///
/// In a database of its own: every slot reads the whole server's log, but
/// one of another database passes over these rows at a glance (30 ms for
/// half a million), where one of the same database decodes them as this
/// one does (550 ms). A million rows beside the other tests held their
/// followers' convergence past its bound on a runner whose walsenders were
/// off the CPU most of the time. The rows go to disk as they are decoded
/// (`logical_decoding_work_mem` at its least, this database's alone),
/// which makes their commit long enough to decode to outlast the
/// follower's notice of its stop (`IDLE_WAIT`, 200 ms) with fewer of them:
/// held in memory, 300 000 were through before it and the test passed
/// without the fix 8 times in 8; spilled, 200 000 failed it 8 times in 8.
/// The database is kept between runs: dropping it forces a checkpoint the
/// tests beside it would wait on.
#[test]
#[ignore]
fn a_stopped_follower_lets_its_slot_go() {
    let t = "fennec_follow_release";
    let bulk = format!("{t}_bulk");
    let mut c = Client::connect(&url()).unwrap();
    let known = c
        .query(&format!("select 1 from pg_database where datname = '{t}'"))
        .unwrap();
    if known.rows.is_empty() {
        sql(&format!("create database {t}"));
    }
    sql(&format!(
        "alter database {t} set logical_decoding_work_mem = '64kB'"
    ));
    let at = url_in(t);
    forget_slot(&at, t);
    for q in [
        format!("drop publication if exists {t}"),
        format!("drop table if exists {t}, {bulk}"),
        format!("create table {t} (id bigint primary key, title text)"),
        format!("insert into {t} select g, 'row ' || g from generate_series(1, 10) g"),
        format!("create table {bulk} (id int)"),
    ] {
        sql_at(&at, &q);
    }
    let f = Follower::start_at(at.clone(), t, Arc::new(RwLock::new(Database::new())));
    sees(&f, "Streaming");
    let written = Client::connect(&at)
        .unwrap()
        .query(&format!(
            "begin; insert into {bulk} select g from generate_series(1, 200000) g; \
             select pg_current_wal_insert_lsn()::text; commit"
        ))
        .unwrap();
    // Asked over a connection already open, so the stop comes as early in
    // the commit's decoding as it can, and the slot is looked at as soon as
    // the follower returns.
    decoded_up_to(&mut c, t, &first_cell(&written).unwrap());
    f.stop();
    assert!(
        !slot_active(&mut c, t),
        "the stopped follower's slot is still taken"
    );
    sql_at(&at, &format!("drop table {t}, {bulk}"));
    drop_slot_at(&at, t);
}

#[test]
#[ignore]
fn a_copy_cut_short_is_made_again() {
    let t = "fennec_follow_cut";
    table(t, 30);
    let db = Arc::new(RwLock::new(Database::new()));
    let f = Follower::start(t, Arc::clone(&db));
    converge(&db, t);
    f.stop();

    // As if the process had died halfway through the copy: most of the
    // rows missing, and `_follow` saying the copy never finished.
    {
        let mut g = db.write().unwrap();
        g.execute(&fennec_ql::parse_one(&format!("del {t} where id > 5")).unwrap())
            .unwrap();
        g.execute(
            &fennec_ql::parse_one(&format!(
                "set _follow {{copied: false}} where collection = \"{t}\""
            ))
            .unwrap(),
        )
        .unwrap();
    }
    let f = Follower::start(t, Arc::clone(&db));
    converge(&db, t);
    let events = f.stop();
    assert!(
        events.iter().any(|e| e.contains("did not finish")),
        "{events:?}"
    );

    // A collection a follower did not copy is not the follower's to replace.
    let other = Arc::new(RwLock::new(Database::new()));
    other
        .write()
        .unwrap()
        .execute(&fennec_ql::parse_one(&format!("create collection {t} (x int)")).unwrap())
        .unwrap();
    let mut f = Follower::start(t, Arc::clone(&other));
    let result = f.thread.take().unwrap().join().unwrap();
    assert!(
        result
            .as_ref()
            .is_err_and(|e| e.to_string().contains("not copied by a follower")),
        "{result:?}"
    );
    drop_slot(t);
}

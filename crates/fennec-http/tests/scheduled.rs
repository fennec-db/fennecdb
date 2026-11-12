//! Scheduled statements run on a file's server
//! (`docs/design/named-statements.md`, F3's slice 3): a look runs each due
//! slot once, under the write lock, its writes and its mark one block --
//! the latest of the missed ones, `now()` its start and `slot()` its slot
//! -- and a run that wrote nothing lands nothing; a refusal the statement
//! makes marks its slot, one the state makes leaves it due, held and tried
//! again; a mark ahead of the clock waits an hour at most; nothing runs on
//! a following database, a failed one or past a lapsed lease; a crash
//! before the fsync runs the slot again once. Over HTTP: a definition
//! wherever a scheduler watches the database alone, a scoped token's call
//! the 404 a missing statement's is, the server's token's run by hand and
//! marking nothing, a held claim answered by a run's write and not woken
//! by a run that wrote nothing, and a server holding no schedule making no
//! look.

use fennec_core::engine::{Landing, Limit};
use fennec_core::error::Error;
use fennec_core::plugin::{Hook, Plugin, Registry, WriteOp};
use fennec_core::prelude::*;
use fennec_core::schema::Schema;
use fennec_http::access::Access;
use fennec_http::schedule::{self, Outcome};
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;

fn utc(y: i64, mo: u32, d: u32, h: i64, mi: i64) -> i64 {
    (fennec_core::time::days_from_civil(y, mo, d) * 1440 + h * 60 + mi) * MINUTE
}

/// Monday 4 May 2026, noon UTC: no clock changes near it.
fn t0() -> i64 {
    utc(2026, 5, 4, 12, 0)
}

const HOLDS: &str =
    "create collection holds (state text @hash, until timestamp @sorted, lapsed_at timestamp)";

/// Lapses each hold whose time is up, every quarter of an hour.
const LAPSE: &str = "create statement lapse every 15m as set holds {state: 'lapsed', lapsed_at: \
                     now()} where state = 'held' and until < now()";

fn run(db: &mut Database, sql: &str) -> Result<Response> {
    let mut last = Response::Affected(0);
    for s in fennec_ql::parse(sql).expect("parse") {
        last = db.execute(&s)?;
    }
    Ok(last)
}

fn number(db: &RwLock<Database>, q: &str) -> i64 {
    let g = db.read().unwrap();
    let r = g.query(&fennec_ql::parse_one(q).unwrap(), &[]).unwrap();
    match &r.rows().unwrap().rows[0].values[0] {
        Value::Int(n) => *n,
        other => panic!("{q}: {other:?}"),
    }
}

/// A database whose clock read `at` as `texts` defined it: their `since`.
fn defined_at(at: i64, texts: &[&str]) -> Arc<RwLock<Database>> {
    fennec_ql::install();
    let mut db = Database::new();
    db.allow_schedules();
    db.set_clock(Some(at));
    for t in texts {
        run(&mut db, t).unwrap_or_else(|e| panic!("{t}: {e}"));
    }
    db.set_clock(None);
    Arc::new(RwLock::new(db))
}

fn put(db: &RwLock<Database>, sql: &str) {
    run(&mut db.write().unwrap(), sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// The one run a look made: its slot, what it passed over and its outcome.
fn ran(look: schedule::Look) -> (i64, u64, Outcome) {
    assert_eq!(look.runs.len(), 1, "{look:?}");
    let r = &look.runs[0];
    assert!(!r.id.is_empty(), "a run has an id of its own");
    (r.slot, r.skipped, r.outcome)
}

fn mark(db: &RwLock<Database>, name: &str) -> Option<fennec_core::engine::SlotMark> {
    let g = db.read().unwrap();
    let pending = g
        .pending()
        .iter()
        .find(|(n, _)| n == name)
        .map(|p| p.1.clone());
    pending.or_else(|| g.last_mark(name).cloned())
}

/// A slot runs once, its writes and its mark one block: before its time
/// nothing, at a look after it the run -- `now()` the look's time -- and
/// at a second look at the same time nothing, the slot asked again under
/// the lock it runs under.
#[test]
fn a_run_and_its_mark_land_as_one_block_once() {
    let db = defined_at(t0(), &[HOLDS, LAPSE]);
    let until = t0() + 10 * MINUTE;
    put(&db, &format!("put holds {{state: 'held', until: {until}}}; put holds {{state: 'held', until: {until}}}"));
    assert_eq!(
        schedule::pass(&db, t0() + 14 * MINUTE),
        schedule::Look::default()
    );
    let before = db.read().unwrap().change_seq();
    let now = t0() + 20 * MINUTE;
    assert_eq!(
        ran(schedule::pass(&db, now)),
        (t0() + 15 * MINUTE, 0, Outcome::Ran)
    );
    // Two `set`s and the mark: one block, three writes.
    assert_eq!(db.read().unwrap().change_seq(), before + 3);
    let g = db.read().unwrap();
    let m = g
        .last_mark("lapse")
        .cloned()
        .expect("landed with the writes");
    assert!(g.pending().is_empty());
    drop(g);
    assert_eq!((m.at, m.skipped, m.refused), (t0() + 15 * MINUTE, 0, None));
    assert_eq!(number(&db, "get holds where state = 'lapsed' count"), 2);
    assert_eq!(
        number(&db, &format!("get holds where lapsed_at = {now} count")),
        2,
        "now() is the run's start"
    );
    assert!(schedule::pass(&db, now).runs.is_empty(), "once");
    assert!(schedule::pass(&db, now + 9 * MINUTE).runs.is_empty());
    // Three periods missed: one run, the latest, two passed over.
    let late = t0() + 15 * MINUTE + 3 * 15 * MINUTE + 5 * MINUTE;
    assert_eq!(
        ran(schedule::pass(&db, late)),
        (t0() + HOUR, 2, Outcome::Ran)
    );
}

/// A run that finds nothing lands nothing: no record, no counter moved;
/// its mark waits in memory and lands as the last record of the next
/// block that lands, a client's.
#[test]
fn a_run_that_wrote_nothing_lands_nothing() {
    let db = defined_at(t0(), &[HOLDS, LAPSE]);
    let before = db.read().unwrap().change_seq();
    let now = t0() + 20 * MINUTE;
    assert_eq!(
        ran(schedule::pass(&db, now)),
        (t0() + 15 * MINUTE, 0, Outcome::Ran)
    );
    let g = db.read().unwrap();
    assert_eq!(g.change_seq(), before, "nothing landed");
    assert!(g.last_mark("lapse").is_none());
    assert_eq!(g.pending().len(), 1);
    drop(g);
    assert!(
        schedule::pass(&db, now).runs.is_empty(),
        "the pending mark counts"
    );
    put(&db, "put holds {state: 'open'}");
    let g = db.read().unwrap();
    assert_eq!(
        g.change_seq(),
        before + 2,
        "the client's write and the mark"
    );
    assert!(g.pending().is_empty());
    assert_eq!(g.last_mark("lapse").map(|m| m.at), Some(t0() + 15 * MINUTE));
}

/// The scheduler runs nothing where the database cannot take a write of
/// its own: a following one, one whose storage failed, one whose lease
/// lapsed. Each slot is left unmarked and due, and a copy promoted runs it
/// at its first look as primary, once.
#[test]
fn nothing_runs_where_the_database_takes_no_write_of_its_own() {
    let db = defined_at(t0(), &[HOLDS, LAPSE]);
    let now = t0() + 20 * MINUTE;
    db.write().unwrap().follow(Vec::new()).unwrap();
    assert_eq!(schedule::pass(&db, now), schedule::Look::default());
    assert!(mark(&db, "lapse").is_none());
    // Promoted, it runs the slot due, once.
    db.write().unwrap().fork(7).unwrap();
    assert_eq!(
        ran(schedule::pass(&db, now)),
        (t0() + 15 * MINUTE, 0, Outcome::Ran)
    );
    assert!(schedule::pass(&db, now).runs.is_empty());

    let db = defined_at(t0(), &[HOLDS, LAPSE]);
    let lapsed = Arc::new(AtomicBool::new(true));
    let fence = Arc::clone(&lapsed);
    db.write()
        .unwrap()
        .set_fence(Some(Arc::new(move || match fence.load(Ordering::SeqCst) {
            true => Err("the lease lapsed".into()),
            false => Ok(()),
        })));
    assert!(schedule::pass(&db, now).runs.is_empty());
    assert!(mark(&db, "lapse").is_none(), "due to the next primary");
    lapsed.store(false, Ordering::SeqCst);
    assert_eq!(ran(schedule::pass(&db, now)).2, Outcome::Ran);

    let db = defined_at(t0(), &[HOLDS, LAPSE]);
    db.write()
        .unwrap()
        .fail(&Error::Io("the disk is gone".into()));
    assert!(schedule::pass(&db, now).runs.is_empty());
    assert!(mark(&db, "lapse").is_none());
}

/// A refusal the statement makes marks its slot -- its status and its
/// guard's reason in the mark -- and the next slot runs as due.
#[test]
fn a_refusal_the_statement_makes_marks_its_slot() {
    let guarded = "create statement close every 15m as begin
      get holds select id where state = 'open' limit 1 require 1 else 'nothing open';
      set holds {state: 'closed'} where state = 'open'
    end";
    let db = defined_at(t0(), &[HOLDS, guarded]);
    assert_eq!(
        ran(schedule::pass(&db, t0() + 20 * MINUTE)),
        (t0() + 15 * MINUTE, 0, Outcome::Refused(412))
    );
    let m = mark(&db, "close").unwrap();
    assert_eq!(m.refused, Some((412, "nothing open".to_string())));
    assert!(
        schedule::pass(&db, t0() + 25 * MINUTE).runs.is_empty(),
        "marked"
    );
    put(&db, "put holds {state: 'open'}");
    assert_eq!(
        ran(schedule::pass(&db, t0() + 35 * MINUTE)),
        (t0() + 30 * MINUTE, 0, Outcome::Ran)
    );
    assert_eq!(number(&db, "get holds where state = 'closed' count"), 1);
}

/// A broken statement's slots are marked as they pass, nothing of it run;
/// once it reads again, the next slot runs, never one it missed.
#[test]
fn a_broken_statement_s_slots_are_marked_as_they_pass() {
    let db = defined_at(t0(), &[HOLDS, LAPSE]);
    let drop = fennec_ql::parse_one("drop collection holds").unwrap();
    db.write()
        .unwrap()
        .internally(|db| db.execute(&drop))
        .unwrap();
    assert!(db
        .read()
        .unwrap()
        .statements()
        .get("lapse")
        .unwrap()
        .broken
        .is_some());
    assert_eq!(
        ran(schedule::pass(&db, t0() + 20 * MINUTE)),
        (t0() + 15 * MINUTE, 0, Outcome::Broken)
    );
    assert_eq!(mark(&db, "lapse").unwrap().refused.map(|r| r.0), Some(409));
    assert_eq!(
        ran(schedule::pass(&db, t0() + 35 * MINUTE)).2,
        Outcome::Broken
    );
    put(&db, HOLDS);
    assert!(db
        .read()
        .unwrap()
        .statements()
        .get("lapse")
        .unwrap()
        .broken
        .is_none());
    assert!(schedule::pass(&db, t0() + 40 * MINUTE).runs.is_empty());
    assert_eq!(
        ran(schedule::pass(&db, t0() + 50 * MINUTE)),
        (t0() + 45 * MINUTE, 0, Outcome::Ran)
    );
}

/// Panics as it writes `boom`, or refuses as a mirrored collection's guard
/// does, writes to `mirrored`; and lapses the lease as it writes `late`
/// while its trap is set, once.
struct Hooks {
    trap: Arc<AtomicBool>,
    lapsed: Arc<AtomicBool>,
}

impl Hook for Hooks {
    fn name(&self) -> &str {
        "tests"
    }
    fn rewrites(&self) -> bool {
        false
    }
    fn before_write(&self, schema: &Schema, _op: WriteOp, _doc: &mut Document) -> Result<()> {
        match schema.name.as_str() {
            "boom" => panic!("a hook's bug"),
            "mirrored" => Err(Error::ReadOnly(
                "`mirrored` mirrors a PostgreSQL table: its writes come from there".into(),
            )),
            "late" => {
                if self.trap.swap(false, Ordering::SeqCst) {
                    self.lapsed.store(true, Ordering::SeqCst);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

impl Plugin for Hooks {
    fn name(&self) -> &str {
        "tests"
    }
    fn init(&self, reg: &mut Registry) -> Result<()> {
        reg.register_hook(Arc::new(Hooks {
            trap: Arc::clone(&self.trap),
            lapsed: Arc::clone(&self.lapsed),
        }));
        Ok(())
    }
}

/// A panic in a run is caught, the block put back and the slot marked
/// refused (500), so the next look does not meet it again. A hook's
/// `Error::ReadOnly` -- a mirrored collection's guard -- is the
/// statement's, marked; the fence's, as a lease lapses mid-run, the
/// state's, the slot left due.
#[test]
fn a_panic_and_a_hook_s_refusal_are_marked_a_lapse_mid_run_is_not() {
    let schema = "create collection boom (n int); create collection mirrored (n int); \
                  create collection late (n int)";
    let db = defined_at(
        t0(),
        &[
            schema,
            "create statement b every 15m as insert boom {n: 1}",
            "create statement m every 15m as insert mirrored {n: 1}",
            "create statement z every 15m as insert late {n: 1}",
        ],
    );
    let lapsed = Arc::new(AtomicBool::new(false));
    let trap = Arc::new(AtomicBool::new(true));
    let fence = Arc::clone(&lapsed);
    {
        let mut g = db.write().unwrap();
        let hooks = Hooks {
            trap: Arc::clone(&trap),
            lapsed: Arc::clone(&lapsed),
        };
        g.install_plugin(&hooks).unwrap();
        g.set_fence(Some(Arc::new(move || match fence.load(Ordering::SeqCst) {
            true => Err("the lease lapsed".into()),
            false => Ok(()),
        })));
    }
    let now = t0() + 20 * MINUTE;
    let look = schedule::pass(&db, now);
    let outcome = |n: &str| look.runs.iter().find(|r| r.name == n).map(|r| r.outcome);
    assert_eq!(outcome("b"), Some(Outcome::Refused(500)));
    assert_eq!(outcome("m"), Some(Outcome::Refused(403)));
    assert_eq!(outcome("z"), Some(Outcome::Held(403)));
    assert_eq!(mark(&db, "b").unwrap().refused.map(|r| r.0), Some(500));
    assert_eq!(mark(&db, "m").unwrap().refused.map(|r| r.0), Some(403));
    assert!(mark(&db, "z").is_none(), "due to the next primary");
    assert_eq!(number(&db, "get late count"), 0);
    // The panic's block was put back, and writes go on.
    assert_eq!(number(&db, "get boom count"), 0);
    lapsed.store(false, Ordering::SeqCst);
    put(&db, "put late {n: 9}");
    // The marked are passed; the held slot is tried again, and lands.
    let look = schedule::pass(&db, now + MINUTE);
    assert_eq!(ran(look), (t0() + 15 * MINUTE, 0, Outcome::Ran));
    assert_eq!(number(&db, "get late count"), 2);
}

/// The quota's refusal leaves the slot due, held: tried again once the
/// counter or the limit moved, at most once a minute, else after a
/// backoff doubling from a minute; the next slot supersedes it, counting
/// it passed over. A purge, only taking away, lands at the quota.
#[test]
fn the_quota_holds_a_slot_and_the_next_supersedes_it() {
    let reset =
        "create statement reset every 15m as set holds {state: 'reset'} where state = 'held'";
    let purge = "create statement purge every 15m as del holds where state = 'gone'";
    let db = defined_at(t0(), &[HOLDS, reset, purge]);
    put(
        &db,
        "put holds {state: 'held'}; put holds {state: 'held'}; put holds {state: 'gone'}; \
         put holds {state: 'spare'}; put holds {state: 'spare'}",
    );
    // A tenant at its quota: whatever grows is refused.
    let quota = |full: bool| -> Limit {
        Arc::new(move |_: &Database, l: &Landing| match full && l.grows {
            true => Err("rows: at most 5".into()),
            false => Ok(()),
        })
    };
    db.write().unwrap().set_limit(Some(quota(true)));
    let t = t0() + 16 * MINUTE;
    let look = schedule::pass(&db, t);
    let outcome =
        |l: &schedule::Look, n: &str| l.runs.iter().find(|r| r.name == n).map(|r| r.outcome);
    assert_eq!(outcome(&look, "reset"), Some(Outcome::Held(507)));
    assert_eq!(outcome(&look, "purge"), Some(Outcome::Ran), "a purge lands");
    assert!(mark(&db, "reset").is_none(), "held, never marked");
    assert_eq!(number(&db, "get holds where state = 'gone' count"), 0);
    // Neither moved: the backoff, a minute, then two.
    assert!(schedule::pass(&db, t + 30_000).runs.is_empty());
    assert_eq!(ran(schedule::pass(&db, t + MINUTE)).2, Outcome::Held(507));
    assert!(schedule::pass(&db, t + 2 * MINUTE).runs.is_empty());
    assert_eq!(
        ran(schedule::pass(&db, t + 3 * MINUTE)).2,
        Outcome::Held(507)
    );
    // The counter moved, a delete landing at the quota: once a minute.
    put(&db, "del holds where state = 'spare' limit 1");
    assert!(schedule::pass(&db, t + 3 * MINUTE + 30_000).runs.is_empty());
    assert_eq!(
        ran(schedule::pass(&db, t + 4 * MINUTE)).2,
        Outcome::Held(507)
    );
    // The limit moved, and makes room: the slot held runs.
    db.write().unwrap().set_limit(Some(quota(false)));
    assert_eq!(
        ran(schedule::pass(&db, t + 5 * MINUTE)),
        (t0() + 15 * MINUTE, 0, Outcome::Ran)
    );
    assert_eq!(number(&db, "get holds where state = 'reset' count"), 2);
    // Held again, and superseded by the next slot, one passed over.
    put(&db, "put holds {state: 'held'}");
    db.write().unwrap().set_limit(Some(quota(true)));
    let look = schedule::pass(&db, t0() + 30 * MINUTE);
    assert_eq!(outcome(&look, "reset"), Some(Outcome::Held(507)));
    db.write().unwrap().set_limit(Some(quota(false)));
    let look = schedule::pass(&db, t0() + 46 * MINUTE);
    let r = look.runs.iter().find(|r| r.name == "reset").unwrap();
    assert_eq!(
        (r.slot, r.skipped, r.outcome),
        (t0() + 45 * MINUTE, 1, Outcome::Ran)
    );
    assert_eq!(number(&db, "get holds where state = 'held' count"), 0);
}

/// The clock moves the slots. Stepped back staying past the mark, nothing
/// runs again; behind the mark by under an hour, the slots wait for it,
/// told once; by more, the mark is the wrong reading and the schedule goes
/// on from the clock, the slot it ran early run again. A replacement moves
/// `since` and clears a mark a year ahead.
#[test]
fn a_mark_ahead_of_the_clock_waits_an_hour_at_most() {
    let at = |h, m| utc(2026, 5, 4, h, m);
    let hourly = "create statement h every 1h as del holds where state = 'x'";
    let db = defined_at(at(0, 30), &[HOLDS, hourly]);
    assert_eq!(
        ran(schedule::pass(&db, at(5, 10))),
        (at(5, 0), 4, Outcome::Ran)
    );
    assert_eq!(schedule::pass(&db, at(5, 5)), schedule::Look::default());
    // Behind the mark by under an hour: told once, nothing run.
    let look = schedule::pass(&db, at(4, 20));
    assert!(look.runs.is_empty());
    assert_eq!(look.ahead.len(), 1);
    assert_eq!((look.ahead[0].mark, look.ahead[0].waits), (at(5, 0), true));
    assert_eq!(
        schedule::pass(&db, at(4, 30)),
        schedule::Look::default(),
        "told once"
    );
    assert!(schedule::pass(&db, at(5, 10)).runs.is_empty(), "none twice");

    // By more: on from the clock, the slot run early run again.
    let db = defined_at(at(0, 30), &[HOLDS, hourly]);
    ran(schedule::pass(&db, at(5, 10)));
    let look = schedule::pass(&db, at(5, 0) - fennec_core::named::AHEAD_MAX - 1);
    assert_eq!(look.ahead.len(), 1);
    assert!(!look.ahead[0].waits);
    assert_eq!(ran(look), (at(3, 0), 2, Outcome::Ran));

    // A mark a year ahead, cleared by a replacement.
    let db = defined_at(at(0, 30), &[HOLDS, hourly]);
    ran(schedule::pass(&db, at(5, 10) + 365 * 24 * HOUR));
    let mut g = db.write().unwrap();
    g.set_clock(Some(at(6, 20)));
    run(&mut g, &hourly.replace("create", "create or replace")).unwrap();
    g.set_clock(None);
    drop(g);
    let look = schedule::pass(&db, at(7, 5));
    assert!(look.ahead.is_empty(), "cleared");
    assert_eq!(ran(look), (at(7, 0), 0, Outcome::Ran));
}

/// A `since` a wrong clock left a year ahead stops nothing: the latest
/// slot runs at the first look, and each slot after it as it comes.
#[test]
fn a_definition_a_year_ahead_runs_from_the_clock() {
    let insert = "create statement h every 1h as insert holds {state: 'tick'}";
    let db = defined_at(t0() + 365 * 24 * HOUR, &[HOLDS, insert]);
    for h in 0..4 {
        let look = schedule::pass(&db, t0() + h * HOUR + 5 * MINUTE);
        assert_eq!(ran(look), (t0() + h * HOUR, 0, Outcome::Ran), "{h}");
        assert!(schedule::pass(&db, t0() + h * HOUR + 30 * MINUTE)
            .runs
            .is_empty());
    }
    assert_eq!(number(&db, "get holds count"), 4);
}

/// A mark a wrong clock left ahead, landed, then runs that find nothing:
/// each one's mark is the latest by order, never outranked by the one
/// ahead, so no slot after it runs twice -- pending, or carried by a
/// client's write.
#[test]
fn a_mark_left_ahead_outranks_no_run_after_it() {
    let at = |h, m| utc(2026, 5, 4, h, m);
    let hourly = "create statement h every 1h as del holds where state = 'x'";
    let db = defined_at(at(0, 30), &[HOLDS, hourly]);
    put(&db, "put holds {state: 'x'}");
    // 05:00's run deletes a row: its mark lands.
    assert_eq!(
        ran(schedule::pass(&db, at(5, 10))),
        (at(5, 0), 4, Outcome::Ran)
    );
    assert_eq!(
        db.read().unwrap().last_mark("h").map(|m| m.at),
        Some(at(5, 0))
    );
    // The clock back to 02:10: 02:00 runs, finding nothing.
    assert_eq!(ran(schedule::pass(&db, at(2, 10))).0, at(2, 0));
    assert_eq!(db.read().unwrap().pending().len(), 1);
    assert!(schedule::pass(&db, at(2, 20)).runs.is_empty(), "pending");
    put(&db, "put holds {state: 'open'}");
    assert!(db.read().unwrap().pending().is_empty(), "carried");
    assert_eq!(
        db.read().unwrap().last_mark("h").map(|m| m.at),
        Some(at(2, 0))
    );
    assert!(schedule::pass(&db, at(2, 30)).runs.is_empty(), "landed");
    assert_eq!(
        ran(schedule::pass(&db, at(3, 5))),
        (at(3, 0), 0, Outcome::Ran)
    );
}

/// A copy promoted runs its due slot at once: the promotion wakes the
/// scheduler, which looked at it while it followed and would otherwise
/// look again within a minute.
#[test]
fn a_promoted_copy_runs_its_due_slot_at_once() {
    let now = fennec_core::time::now_ms().unwrap();
    let db = defined_at(
        now - 2 * HOUR,
        &[
            HOLDS,
            "create statement h every 1h as insert holds {state: 'tick'}",
        ],
    );
    db.write().unwrap().follow(Vec::new()).unwrap();
    schedule::watch("a promoted copy", &db, false, |_| {}).unwrap();
    let t = Instant::now();
    while schedule::looks(&db) == 0 {
        assert!(t.elapsed() < Duration::from_secs(20), "never looked");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(number(&db, "get holds count"), 0, "a follower runs none");
    db.write().unwrap().fork(9).unwrap();
    // Well inside the minute the look while following set.
    let t = Instant::now();
    while number(&db, "get holds count") == 0 {
        assert!(t.elapsed() < Duration::from_secs(30), "not run at once");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Berlin's clocks go forward at 02:00 on 29 March 2026 and back at 03:00
/// on 25 October: `at '30 2 * * *'` runs at the jump the first night and
/// once the second, at the first 02:30; `every 1h` twice in the hour shown
/// twice, at `bucket`'s edges.
#[test]
fn summer_time_through_the_scheduler() {
    let night = "create statement night at '30 2 * * *' in 'Europe/Berlin' as \
                 del holds where state = 'x'";
    let db = defined_at(utc(2026, 3, 28, 12, 0), &[HOLDS, night]);
    let jump = utc(2026, 3, 29, 1, 0);
    assert!(schedule::pass(&db, jump - 1).runs.is_empty());
    assert_eq!(ran(schedule::pass(&db, jump)), (jump, 0, Outcome::Ran));
    assert!(schedule::pass(&db, jump + 2 * HOUR).runs.is_empty());
    let db = defined_at(utc(2026, 10, 24, 12, 0), &[HOLDS, night]);
    let first = utc(2026, 10, 25, 0, 30);
    assert_eq!(ran(schedule::pass(&db, first)), (first, 0, Outcome::Ran));
    assert!(
        schedule::pass(&db, first + HOUR).runs.is_empty(),
        "the second 02:30"
    );
    let hourly = "create statement h every 1h in 'Europe/Berlin' as del holds where state = 'x'";
    let db = defined_at(utc(2026, 10, 24, 23, 30), &[HOLDS, hourly]);
    let twice = utc(2026, 10, 25, 0, 0);
    assert_eq!(
        ran(schedule::pass(&db, twice + 1)),
        (twice, 0, Outcome::Ran)
    );
    assert_eq!(
        ran(schedule::pass(&db, twice + HOUR + 1)),
        (twice + HOUR, 0, Outcome::Ran)
    );
}

/// A run's writes wake the statements bound to them, in its block: a
/// binding's refusal is the statement's, put back whole and marked. A run
/// late by forty minutes reads `@ttl`'s `alive` by its start.
#[test]
fn a_run_wakes_a_binding_in_its_block_and_reads_alive_by_its_start() {
    let db = defined_at(
        t0(),
        &[
            HOLDS,
            "create collection stock (n int)",
            LAPSE,
            "create statement give_back after set holds where new.state = 'lapsed' and \
             old.state = 'held' as set stock {n: n + 1} where id = 1 require 1 else 'no stock'",
        ],
    );
    let until = t0();
    put(&db, &format!("put holds {{state: 'held', until: {until}}}"));
    // No stock row: the binding refuses, the run is put back and marked.
    assert_eq!(
        ran(schedule::pass(&db, t0() + 20 * MINUTE)).2,
        Outcome::Refused(412)
    );
    assert_eq!(number(&db, "get holds where state = 'held' count"), 1);
    put(&db, "put stock {n: 0}");
    let before = db.read().unwrap().change_seq();
    assert_eq!(ran(schedule::pass(&db, t0() + 35 * MINUTE)).2, Outcome::Ran);
    assert_eq!(number(&db, "get stock select n"), 1);
    // The `set`, the binding's `set` and the mark: one block.
    assert_eq!(db.read().unwrap().change_seq(), before + 3);

    let db = defined_at(
        t0(),
        &[
            "create collection leases (state text @hash, until timestamp @ttl(1ms))",
            "create statement seen every 1h as set leases {state: 'seen'} where state = 'new'",
        ],
    );
    let slot = t0() + HOUR;
    let start = slot + 40 * MINUTE;
    put(
        &db,
        &format!(
            "put leases {{id: 1, state: 'new', until: {}}}; put leases {{id: 2, state: 'new', \
             until: {}}}",
            slot + 20 * MINUTE,
            start + 20 * MINUTE
        ),
    );
    assert_eq!(ran(schedule::pass(&db, start)).2, Outcome::Ran);
    db.write().unwrap().set_clock(Some(slot));
    assert_eq!(number(&db, "get leases where state = 'seen' count"), 1);
    assert_eq!(
        number(&db, "get leases where id = 1 and state = 'new' count"),
        1
    );
}

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fennecdb-http-scheduled-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{tag}.fennec"));
    let _ = std::fs::remove_file(&path);
    path
}

fn opened(path: &PathBuf) -> Arc<RwLock<Database>> {
    let mut db = fennec_core::fs::open(path).unwrap();
    db.allow_schedules();
    Arc::new(RwLock::new(db))
}

/// A crash between a run's landing and its fsync -- its record never on
/// disk, or torn -- runs the slot once on reopen; one after the fsync runs
/// nothing. A run that wrote nothing adds no byte to the file.
#[test]
fn a_crash_before_the_fsync_runs_the_slot_again_once() {
    fennec_ql::install();
    let path = tmp("crash");
    {
        let mut db = fennec_core::fs::open(&path).unwrap();
        db.allow_schedules();
        db.set_clock(Some(t0()));
        run(&mut db, HOLDS).unwrap();
        run(&mut db, LAPSE).unwrap();
        db.set_clock(None);
        run(
            &mut db,
            &format!("put holds {{state: 'held', until: {}}}", t0()),
        )
        .unwrap();
        if let Some(sync) = db.flush().unwrap() {
            sync().unwrap();
        }
    }
    let len = || std::fs::metadata(&path).unwrap().len();
    let before = len();
    let now = t0() + 20 * MINUTE;
    {
        let db = opened(&path);
        assert_eq!(ran(schedule::pass(&db, now)).2, Outcome::Ran);
    }
    let after = len();
    assert!(after > before);
    let bytes = std::fs::read(&path).unwrap();
    // The run's record, before the mark a close after an fsync appends.
    let run_end = match bytes.len() >= 8 && bytes[bytes.len() - 8] == 0x80 | 14 {
        true => bytes.len() - 8,
        false => bytes.len(),
    };
    assert!(run_end > before as usize);
    for cut in [before as usize, run_end - 3] {
        std::fs::write(&path, &bytes[..cut]).unwrap();
        let db = opened(&path);
        assert_eq!(
            number(&db, "get holds where state = 'held' count"),
            1,
            "{cut}"
        );
        assert_eq!(
            ran(schedule::pass(&db, now)),
            (t0() + 15 * MINUTE, 0, Outcome::Ran)
        );
        assert!(schedule::pass(&db, now).runs.is_empty());
    }
    std::fs::write(&path, &bytes).unwrap();
    let db = opened(&path);
    assert!(schedule::pass(&db, now).runs.is_empty(), "synced: run once");
    assert_eq!(number(&db, "get holds where state = 'lapsed' count"), 1);
    drop(db);
    // A slot finding nothing: no byte.
    let quiet = len();
    let db = opened(&path);
    assert_eq!(ran(schedule::pass(&db, now + 15 * MINUTE)).2, Outcome::Ran);
    drop(db);
    assert_eq!(len(), quiet);
}

// ------------------------------------------------------------ over HTTP

const SECRET: &[u8] = b"thirty-two bytes and a few more, for HS256";
const ROOT: &str = "root-token";
const POLICY: &str = "holds  read,write
jobs  read,write
lapse, ghost  call
lapse  wake
";

struct Node {
    port: u16,
    access: Arc<Access>,
    server: Arc<Server>,
}

/// A server over `db`, the policy's tokens beside the server's.
fn serve(db: &Arc<RwLock<Database>>) -> Node {
    fennec_http::audit::set_delay(0);
    let access = Arc::new(Access::new(SECRET, POLICY).unwrap());
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some(ROOT.into()),
        access: Some(Arc::clone(&access)),
        max_connections: 0,
        call_rate: 0,
        ..Config::default()
    };
    let server = Arc::new(Server::new(Arc::clone(db), cfg));
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    let s = Arc::clone(&server);
    std::thread::spawn(move || {
        let _ = s.serve_on(listener);
    });
    Node {
        port,
        access,
        server,
    }
}

fn send(port: u16, token: &str, target: &str, body: &str, extra: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let method = match body.is_empty() && target.starts_with("/_schema") {
        true => "GET",
        false => "POST",
    };
    write!(
        s,
        "{method} {target} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n{extra}\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let body = out
        .split_once("\r\n\r\n")
        .map_or("", |(_, b)| b)
        .to_string();
    (out[9..12].parse().unwrap(), body)
}

impl Node {
    fn send(&self, token: &str, target: &str, body: &str, extra: &str) -> (u16, String) {
        send(self.port, token, target, body, extra)
    }

    fn query(&self, token: &str, q: &str) -> (u16, String) {
        self.send(token, "/query", &line(q), "")
    }

    fn scoped(&self) -> String {
        self.access.mint(r#"{"sub":"u1"}"#).unwrap()
    }
}

fn line(q: &str) -> String {
    let mut body = String::from("{\"query\":");
    fennec_core::json::escape_into(&mut body, q);
    body.push('}');
    body
}

/// A file's server takes a scheduled statement where its scheduler
/// watches the database (`schedule::watch`), by the server's token alone;
/// one no scheduler watches refuses it -- alone, and in a `/batch` behind
/// a write, which lands nothing -- never a schedule no one runs.
#[test]
fn a_schedule_is_defined_where_a_scheduler_runs_it_by_the_server_s_token() {
    fennec_ql::install();
    let db = Arc::new(RwLock::new(Database::new()));
    let node = serve(&db);
    let (status, body) = node.query(ROOT, HOLDS);
    assert_eq!(status, 200, "{body}");
    let yearly = "create statement tidy at '0 3 1 1 *' in 'Europe/Berlin' as del holds where \
                  state = 'x'";
    let (status, body) = node.query(ROOT, yearly);
    assert_eq!(status, 400, "{body}");
    assert!(
        body.contains("no scheduler runs this database's statements"),
        "{body}"
    );
    let batch = [line("put holds {state: 'x'}"), line(yearly)].join("\n");
    let (status, body) = node.send(ROOT, "/batch", &batch, "");
    assert_eq!(status, 400, "{body}");
    assert_eq!(number(&db, "get holds count"), 0);
    schedule::watch("a test's database", &db, false, |_| {}).unwrap();
    let (status, body) = node.query(&node.scoped(), yearly);
    assert_eq!(status, 403, "{body}");
    let (status, body) = node.query(ROOT, yearly);
    assert_eq!(status, 200, "{body}");
    assert!(db.read().unwrap().statements().get("tidy").is_some());
}

/// To a token held to a policy a scheduled statement is the 404 a missing
/// one is -- at `/query`, in a `/batch` and at `POST /_call/<name>` --
/// whatever a `call` line says, and no listing or schema answer names it;
/// the server's token runs it by hand, its `slot()` the slot that has
/// come, marking none.
#[test]
fn a_scheduled_statement_is_the_server_token_s_to_call() {
    let db = defined_at(
        t0(),
        &[
            HOLDS,
            "create collection stamps (slot timestamp, n int)",
            "create statement lapse every 1h as insert stamps {slot: slot(), n: 1}",
        ],
    );
    let node = serve(&db);
    let scoped = node.scoped();
    let missing = node.query(&scoped, "call ghost");
    assert_eq!(missing.0, 404, "{missing:?}");
    let (status, body) = node.query(&scoped, "call lapse");
    assert_eq!(status, 404, "{body}");
    assert_eq!(body, missing.1.replace("ghost", "lapse"), "the same words");
    let (status, body) = node.send(&scoped, "/batch", &line("call lapse"), "");
    assert_eq!(status, 404, "{body}");
    let (status, body) = node.send(&scoped, "/_call/lapse", "", "");
    assert_eq!(status, 404, "{body}");
    for listing in ["statements", "describe statement lapse"] {
        let (status, body) = node.query(&scoped, listing);
        assert!(status == 403 || status == 404, "{listing}: {status} {body}");
        assert!(!body.contains("every"), "{body}");
    }
    let (status, body) = node.send(&scoped, "/_schema", "", "");
    assert_eq!(status, 200, "{body}");
    assert!(
        !body.contains("\"lapse\"") && !body.contains("every"),
        "{body}"
    );
    assert_eq!(number(&db, "get stamps count"), 0);
    // The server's token: run now, unmarked, its slot the one that came.
    let now = fennec_core::time::now_ms().unwrap();
    let (status, body) = node.query(ROOT, "call lapse");
    assert_eq!(status, 200, "{body}");
    let (status, body) = node.send(ROOT, "/_call/lapse", "", "");
    assert_eq!(status, 200, "{body}");
    assert_eq!(number(&db, "get stamps count"), 2);
    let hour = now - now.rem_euclid(HOUR);
    let at_hour = format!("get stamps where slot = {hour} count");
    let later = format!("get stamps where slot = {} count", hour + HOUR);
    assert!(number(&db, &at_hour) + number(&db, &later) == 2);
    assert!(mark(&db, "lapse").is_none(), "a run by hand marks no slot");
}

/// `statements`' row of each statement by name, as the engine answers it:
/// its `schedule`, `next`, `last` and `held`, as the database's clock
/// reads `at`.
fn shown(db: &RwLock<Database>, at: i64, q: &str) -> Vec<(String, Vec<Value>)> {
    let mut g = db.write().unwrap();
    g.set_clock(Some(at));
    let r = g.query(&fennec_ql::parse_one(q).unwrap(), &[]).unwrap();
    let rs = r.rows().unwrap();
    let at = |c: &str| rs.columns.iter().position(|x| x == c).unwrap();
    let cols = ["schedule", "next", "last", "held"].map(at);
    (rs.rows.iter())
        .map(|r| {
            let name = match &r.values[at("name")] {
                Value::Text(t) => t.clone(),
                other => panic!("{other:?}"),
            };
            (name, cols.iter().map(|&i| r.values[i].clone()).collect())
        })
        .collect()
}

fn object(pairs: &[(&str, Value)]) -> Value {
    Value::object(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    )
    .unwrap()
}

/// What the listings show of a scheduled statement, to the server's token:
/// `statements` and `describe statement` its schedule as its text writes
/// it, its next slot -- one held back shown due, overdue --, its last mark,
/// pending or landed, its outcome, status, reason and what it passed over,
/// and the slot its scheduler holds back, its status and since when; four
/// `null`s for a statement no clock runs. `GET /_schema` describes each
/// after the collections, in both its forms, each read back by the plan as
/// a plan of nothing. A token held to a policy is shown none of it.
#[test]
fn the_listings_show_a_schedule_its_next_slot_its_last_run_and_a_held_slot() {
    let guard = "create statement guard every 15m in 'Europe/Berlin' comment 'Looks first' \
                 scans as begin get holds where state = 'nothing' require 1 else 'no hold to \
                 look at'; del holds where state = 'x' end";
    let reset = "create statement reset every 15m as set holds {state: 'reset'} where state = \
                 'held'";
    let db = defined_at(
        t0(),
        &[
            HOLDS,
            LAPSE,
            guard,
            reset,
            "create statement count_holds() as get holds count",
        ],
    );
    put(&db, "put holds {state: 'held'}");
    let ts = Value::Timestamp;
    let text = |t: &str| Value::Text(t.into());
    let nulls = vec![Value::Null; 4];
    let before = shown(&db, t0() + 5 * MINUTE, "statements");
    assert_eq!(
        before,
        vec![
            ("count_holds".into(), nulls.clone()),
            (
                "guard".into(),
                vec![
                    text("every 15m in 'Europe/Berlin'"),
                    ts(t0() + 15 * MINUTE),
                    Value::Null,
                    Value::Null
                ]
            ),
            (
                "lapse".into(),
                vec![
                    text("every 15m"),
                    ts(t0() + 15 * MINUTE),
                    Value::Null,
                    Value::Null
                ]
            ),
            (
                "reset".into(),
                vec![
                    text("every 15m"),
                    ts(t0() + 15 * MINUTE),
                    Value::Null,
                    Value::Null
                ]
            ),
        ]
    );
    // A run that found nothing, a guard's refusal, and the quota holding
    // a run that grows.
    db.write()
        .unwrap()
        .set_limit(Some(Arc::new(|_: &Database, l: &Landing| match l.grows {
            true => Err("rows: at most 1".into()),
            false => Ok(()),
        })));
    let look = schedule::pass(&db, t0() + 16 * MINUTE);
    assert_eq!(look.runs.len(), 3, "{look:?}");
    let ran = |slot: i64| {
        object(&[
            ("outcome", text("ran")),
            ("reason", Value::Null),
            ("skipped", Value::Int(0)),
            ("slot", ts(slot)),
            ("status", Value::Null),
        ])
    };
    let refused = object(&[
        ("outcome", text("refused")),
        ("reason", text("no hold to look at")),
        ("skipped", Value::Int(0)),
        ("slot", ts(t0() + 15 * MINUTE)),
        ("status", Value::Int(412)),
    ]);
    let held = object(&[
        ("since", ts(t0() + 16 * MINUTE)),
        ("slot", ts(t0() + 15 * MINUTE)),
        ("status", Value::Int(507)),
    ]);
    let after = shown(&db, t0() + 17 * MINUTE, "statements");
    let next = ts(t0() + 30 * MINUTE);
    assert_eq!(
        after[1..],
        [
            (
                "guard".into(),
                vec![
                    text("every 15m in 'Europe/Berlin'"),
                    next.clone(),
                    refused.clone(),
                    Value::Null
                ]
            ),
            (
                "lapse".into(),
                vec![
                    text("every 15m"),
                    next.clone(),
                    ran(t0() + 15 * MINUTE),
                    Value::Null
                ]
            ),
            (
                "reset".into(),
                vec![
                    text("every 15m"),
                    ts(t0() + 15 * MINUTE),
                    Value::Null,
                    held.clone()
                ]
            ),
        ]
    );
    assert_eq!(after[0], ("count_holds".into(), nulls));
    let described = shown(&db, t0() + 17 * MINUTE, "describe statement reset");
    assert_eq!(described, after[3..]);

    // Over HTTP, to the server's token: the listing, and the schema in
    // both its forms, each read back as a plan of nothing.
    let node = serve(&db);
    let (status, body) = node.query(ROOT, "statements");
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains(r#""schedule":"every 15m","next":"2026-05-04T12:15:00Z","last":null,"held":{"since":"2026-05-04T12:16:00Z","slot":"2026-05-04T12:15:00Z","status":507}"#),
        "{body}"
    );
    let (status, described) = node.send(ROOT, "/_schema", "", "");
    assert_eq!(status, 200, "{described}");
    let doc = fennec_core::json::parse_json(&described).unwrap();
    let member = |o: &Value, k: &str| match o {
        Value::Object(m) => {
            (m.iter().find(|(n, _)| n == k)).map_or(Value::Null, |(_, v)| v.clone())
        }
        other => panic!("{other:?}"),
    };
    let schedules = member(&doc, "schedules");
    let Value::List(schedules) = &schedules else {
        panic!("{described}")
    };
    let names: Vec<_> = schedules.iter().map(|s| member(s, "name")).collect();
    assert_eq!(names, [text("guard"), text("lapse"), text("reset")]);
    let (g, r) = (&schedules[0], &schedules[2]);
    assert_eq!(member(g, "schedule"), text("every 15m in 'Europe/Berlin'"));
    assert_eq!(member(g, "text"), text(guard));
    assert_eq!(member(g, "next"), text("2026-05-04T12:30:00Z"));
    assert_eq!(
        member(&member(g, "last"), "reason"),
        text("no hold to look at")
    );
    assert_eq!(member(r, "last"), Value::Null);
    assert_eq!(
        member(r, "held"),
        object(&[
            ("since", text("2026-05-04T12:16:00Z")),
            ("slot", text("2026-05-04T12:15:00Z")),
            ("status", Value::Int(507)),
        ])
    );
    let (status, fennecql) = node.send(ROOT, "/_schema?as=fennecql", "", "");
    assert_eq!(status, 200, "{fennecql}");
    assert!(
        fennecql.contains("every 15m in 'Europe/Berlin'"),
        "{fennecql}"
    );
    for plan_of in [described.clone(), fennecql] {
        let (status, body) = node.send(ROOT, "/_schema/plan", &plan_of, "");
        assert_eq!(status, 200, "{body}");
        assert!(
            body.contains(r#""statements":[]"#) && body.contains(r#""refusals":[]"#),
            "{body}"
        );
    }
    // A description claiming another schedule than its text runs.
    let claimed = described.replacen(r#""schedule":"every 15m","#, r#""schedule":"every 1h","#, 1);
    let (status, body) = node.send(ROOT, "/_schema/plan", &claimed, "");
    assert_eq!(status, 400, "{body}");
    assert!(
        body.contains("listed with the schedule `every 1h`"),
        "{body}"
    );

    // A token held to a policy: no listing, and a schema naming none.
    let scoped = node.scoped();
    let (status, body) = node.query(&scoped, "statements");
    assert_eq!(status, 403, "{body}");
    let (status, body) = node.query(&scoped, "describe statement reset");
    assert_eq!(status, 404, "{body}");
    for target in ["/_schema", "/_schema?as=fennecql"] {
        let (status, body) = node.send(&scoped, target, "", "");
        assert_eq!(status, 200, "{body}");
        for word in ["schedules", "reset", "every 15m", "held", "next"] {
            assert!(!body.contains(word), "{target}: {word} in {body}");
        }
    }

    // The quota lifted: the slot held runs, and is held no more.
    db.write().unwrap().set_limit(None);
    let look = schedule::pass(&db, t0() + 18 * MINUTE);
    assert_eq!(look.runs.len(), 1, "{look:?}");
    let last = shown(&db, t0() + 18 * MINUTE, "describe statement reset");
    assert_eq!(
        last[0].1,
        vec![
            text("every 15m"),
            next,
            ran(t0() + 15 * MINUTE),
            Value::Null
        ]
    );
}

/// What the schema broke reads back through `/_schema/plan` as a plan of
/// nothing: `GET /_schema` lists a broken scheduled statement and a broken
/// binding with why (`broken`), which the plan holds to no dependency, and
/// `?as=fennecql` writes each as comments under its reason. A schedule is
/// listed up to its last word, a comment after it the text's alone, and
/// reads back as its text writes it.
#[test]
fn what_the_schema_broke_and_a_commented_schedule_read_back_as_a_plan_of_nothing() {
    let db = defined_at(
        t0(),
        &[
            HOLDS,
            "create collection gone (n int)",
            "create collection ledger (n int)",
            "create statement orphan every 15m as insert gone {n: 1}",
            "create statement noted after set holds as insert ledger {n: 1}",
            "create statement tidy every 1h\n-- the hour's holds\nas del holds where state = 'x'",
        ],
    );
    for dropped in ["drop collection gone", "drop collection ledger"] {
        let s = fennec_ql::parse_one(dropped).unwrap();
        db.write().unwrap().internally(|d| d.execute(&s)).unwrap();
    }
    let node = serve(&db);
    let (status, body) = node.query(ROOT, "statements");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""schedule":"every 1h","#), "{body}");
    let (status, described) = node.send(ROOT, "/_schema", "", "");
    assert_eq!(status, 200, "{described}");
    let doc = fennec_core::json::parse_json(&described).unwrap();
    let member = |o: &Value, k: &str| match o {
        Value::Object(m) => {
            (m.iter().find(|(n, _)| n == k)).map_or(Value::Null, |(_, v)| v.clone())
        }
        other => panic!("{other:?}"),
    };
    let named = |list: Value, name: &str| match list {
        Value::List(all) => (all.into_iter())
            .find(|e| member(e, "name") == Value::Text(name.into()))
            .unwrap_or_else(|| panic!("no {name} in {described}")),
        other => panic!("{other:?}"),
    };
    let broken = |e: &Value| matches!(member(e, "broken"), Value::Text(t) if !t.is_empty());
    let orphan = named(member(&doc, "schedules"), "orphan");
    assert!(broken(&orphan), "{described}");
    let tidy = named(member(&doc, "schedules"), "tidy");
    assert!(!broken(&tidy), "{described}");
    assert_eq!(member(&tidy, "schedule"), Value::Text("every 1h".into()));
    let holds = named(member(&doc, "collections"), "holds");
    assert!(
        broken(&named(member(&holds, "bindings"), "noted")),
        "{described}"
    );
    let (status, fennecql) = node.send(ROOT, "/_schema?as=fennecql", "", "");
    assert_eq!(status, 200, "{fennecql}");
    assert!(
        fennecql.contains("-- broken: ") && fennecql.contains("-- create statement orphan"),
        "{fennecql}"
    );
    for plan_of in [described, fennecql] {
        let (status, body) = node.send(ROOT, "/_schema/plan", &plan_of, "");
        assert_eq!(status, 200, "{body}");
        assert!(
            body.contains(r#""statements":[]"#) && body.contains(r#""refusals":[]"#),
            "{body}"
        );
    }
}

/// A held claim is answered by a run's write, its block naming `jobs`; a
/// run that wrote nothing lands nothing and wakes nothing, and a client's
/// write carrying its mark names its own collection alone -- never every
/// one, which would wake each group held.
#[test]
fn a_held_claim_is_answered_by_a_run_s_write_never_by_its_mark() {
    let now = fennec_core::time::now_ms().unwrap();
    let db = defined_at(
        now - 2 * HOUR,
        &[
            "create collection jobs (kind text @hash, run_at timestamp @sorted, owner text)",
            "create collection other (n int)",
            "create statement requeue every 1h as set jobs {owner: 'nobody', run_at: \
             timestamp(0)} where kind = 'stale'",
            "create statement idle every 1h as set jobs {owner: 'x'} where kind = 'none'",
        ],
    );
    let node = serve(&db);
    put(
        &db,
        &format!("put jobs {{kind: 'later', run_at: {}}}", now + 10 * HOUR),
    );
    let claim =
        "set jobs {owner: 'w1'} where run_at <= now() order run_at limit 1 returning id, kind";
    let port = node.port;
    let held = std::thread::spawn(move || {
        send(port, ROOT, "/query", &line(claim), "Fennec-Wait: 20000\r\n")
    });
    let t = Instant::now();
    while node.server.waits().held < 1 {
        assert!(t.elapsed() < Duration::from_secs(20), "never held");
        std::thread::sleep(Duration::from_millis(1));
    }
    let before = node.server.waits();
    // Each finds nothing: no landing, no wake.
    let look = schedule::pass(&db, now);
    assert_eq!(look.runs.len(), 2, "{look:?}");
    assert!(look.runs.iter().all(|r| r.outcome == Outcome::Ran));
    assert_eq!(db.read().unwrap().pending().len(), 2);
    let (status, body) = node.query(ROOT, "put other {n: 1}");
    assert_eq!(status, 200, "{body}");
    assert!(db.read().unwrap().pending().is_empty(), "carried");
    let after = node.server.waits();
    assert_eq!(
        (after.epoch, after.held),
        (before.epoch, 1),
        "nothing woken"
    );
    // A stale job: the next run puts it back, and the claim takes it.
    put(
        &db,
        &format!("put jobs {{kind: 'stale', run_at: {}}}", now + 20 * HOUR),
    );
    let look = schedule::pass(&db, now + HOUR);
    assert_eq!(look.runs.len(), 2, "{look:?}");
    let (status, body) = held.join().unwrap();
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"stale\""), "{body}");
}

/// A server holding no schedule makes no look: its scheduler sleeps with
/// no deadline over writes of every form -- documents, schema changes, a
/// callable and a bound statement, a compact -- and the first schedule
/// defined wakes it.
#[test]
fn a_server_holding_no_schedule_makes_no_look() {
    fennec_ql::install();
    let db = Arc::new(RwLock::new(Database::new()));
    schedule::watch("idle", &db, false, |_| {}).unwrap();
    let node = serve(&db);
    for q in [
        HOLDS,
        "create collection stock (n int)",
        "put holds {state: 'held'}",
        "insert holds {state: 'open'}",
        "set holds {state: 'x'} where id = 1",
        "del holds where id = 2",
        "put stock {n: 1}",
        "create statement count_holds() as get holds count",
        "create statement given after del holds as set stock {n: n + 1} where id = 1",
        "del holds where id = 1",
        "alter collection holds add note text",
        "compact",
        "drop statement given",
    ] {
        let (status, body) = node.query(ROOT, q);
        assert_eq!(status, 200, "{q}: {body}");
    }
    let (status, body) = node.send(
        ROOT,
        "/batch",
        &[line("put stock {n: 2}"), line("put stock {n: 3}")].join("\n"),
        "",
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(schedule::looks(&db), 0);
    let (status, body) = node.query(
        ROOT,
        "create statement tidy at '0 3 1 1 *' as del holds where state = 'x'",
    );
    assert_eq!(status, 200, "{body}");
    let t = Instant::now();
    while schedule::looks(&db) == 0 {
        assert!(t.elapsed() < Duration::from_secs(20), "never looked");
        std::thread::sleep(Duration::from_millis(1));
    }
}

//! Scheduled statements as their database keeps them
//! (`docs/design/named-statements.md`, F3's slice 2): a definition is
//! record kind 13's op 4, its schedule and `since` outside its text, and a
//! slot's mark op 5 -- each one write the counter counts, put back with
//! its block, kept by every open, image and compact, applied by a replica
//! and passed over by a reader of the changes; the mark of a run that wrote
//! nothing pending until the next block lands; and `Database::due`, what
//! the clock says. A definition is refused where no scheduler runs the
//! database's (`Database::allow_schedules`); a run's body is
//! `Database::run_slot`'s, and a call by hand marks nothing.

use crate::stored::{body_at, opened, refused, run, Tap};
use fennec_core::codec::{encode_str, put_uvarint};
use fennec_core::engine::{writes_in, ChangeKind, Due, Landing, SlotMark, Times};
use fennec_core::named::{AHEAD_MAX, GRAMMAR_EPOCH, MAX_SCHEDULED, SCHEDULED_UNRUN};
use fennec_core::prelude::*;
use std::sync::Arc;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;

const HOLDS: &str = "create collection holds (state text @hash, until timestamp)";

/// Lapses each hold whose time is up, every quarter of an hour.
const LAPSE: &str = "create statement lapse every 15m in 'Europe/Berlin'
  comment 'Lapses each hold whose time is up'
as set holds {state: 'lapsed'} where state = 'held' and until < now()";

/// What a run of `lapse` writes.
const BODY: &str = "set holds {state: 'lapsed'} where state = 'held' and until < now()";

fn utc(y: i64, mo: u32, d: u32, h: i64, mi: i64) -> i64 {
    (fennec_core::time::days_from_civil(y, mo, d) * 1440 + h * 60 + mi) * MINUTE
}

/// Saturday 28 March 2026, noon UTC: the night after, Berlin's clocks go
/// forward at 02:00.
fn t0() -> i64 {
    utc(2026, 3, 28, 12, 0)
}

/// A database over `holds`, its clock at `t0`, letting a schedule be
/// defined.
fn holds(tap: &Tap) -> Database {
    let mut db = tap.database();
    db.set_clock(Some(t0()));
    db.allow_schedules();
    run(&mut db, HOLDS).unwrap();
    db
}

fn defined(tap: &Tap) -> Database {
    let mut db = holds(tap);
    run(&mut db, LAPSE).unwrap();
    // An hour on: a hold due at `t0` is past its time.
    db.set_clock(Some(t0() + HOUR));
    db
}

fn version(db: &Database, name: &str) -> (u64, u64) {
    db.statements().get(name).unwrap().version
}

fn ran(db: &Database, name: &str, at: i64, skipped: u64) -> SlotMark {
    SlotMark {
        at,
        version: version(db, name),
        refused: None,
        skipped,
    }
}

/// What a run does: its body in a block, its mark the block's last record.
fn run_slot(db: &mut Database, body: &str, name: &str, at: i64, skipped: u64) -> Result<()> {
    let mark = ran(db, name, at, skipped);
    db.begin()?;
    if let Err(e) = run(db, body) {
        db.rollback();
        return Err(e);
    }
    db.mark_slot(name, mark)?;
    db.commit()
}

/// The ops of the kind-13 records a write's record holds: its own, or a
/// block's (kind 9) records'.
fn ops_in(record: &[u8]) -> Vec<u8> {
    let mut ops = Vec::new();
    let mut p = body_at(record);
    let end = record.len();
    match record[0] {
        13 => return vec![record[p]],
        9 => {}
        _ => return ops,
    }
    while p < end {
        let kind = record[p];
        let at = p;
        let mut q = p + 1;
        fennec_core::codec::get_uvarint(record, &mut q).unwrap();
        let len = fennec_core::codec::get_uvarint(record, &mut q).unwrap() as usize;
        if kind == 13 {
            ops.push(record[q]);
        }
        p = q + len;
        assert!(p > at);
    }
    ops
}

fn due(db: &Database, now: i64) -> Due {
    db.due(now).into_iter().next().expect("a schedule")
}

#[test]
fn a_schedule_is_one_record_of_op_4_refused_but_where_a_test_lets_it() {
    let tap = Tap::default();
    let mut db = tap.database();
    run(&mut db, HOLDS).unwrap();
    assert_eq!(
        refused(run(&mut db, LAPSE)),
        Error::Query(SCHEDULED_UNRUN.into()).to_string()
    );
    assert!(db.statements().is_empty());

    let tap = Tap::default();
    let mut db = holds(&tap);
    let before = db.change_seq();
    run(&mut db, LAPSE).unwrap();
    let (seq, record) = tap.last();
    assert_eq!(
        (record[0], record[body_at(&record)]),
        (13, 4),
        "kind 13, op 4"
    );
    assert_eq!(writes_in(&record).unwrap(), 1);
    assert_eq!(seq, before + 1);
    let s = db.statements().get("lapse").unwrap().clone();
    let t = s.schedule.as_ref().expect("its schedule, from the record");
    assert_eq!(t.times, Times::Step(15 * MINUTE, 0));
    assert_eq!((t.zone.as_str(), t.since), ("Europe/Berlin", t0()));
    assert!(s.tree.is_some() && s.broken.is_none() && db.last_mark("lapse").is_none());
    assert!(!s.called());
    assert!(s.depends.iter().any(|d| d == "holds.state"));
    // Called by hand -- the server's token's, the only caller one has --
    // it runs and marks no slot: the mark is the clock's.
    let seq = db.change_seq();
    run(&mut db, "call lapse").unwrap();
    assert!(db.last_mark("lapse").is_none() && db.pending().is_empty());
    assert_eq!(db.change_seq(), seq, "nothing to lapse, and no mark");
    // `statements` lists it.
    let listed = db
        .execute(&fennec_ql::parse_one("statements").unwrap())
        .unwrap();
    assert_eq!(listed.rows().unwrap().rows.len(), 1);
}

#[test]
fn a_mark_lands_as_the_last_record_of_its_run_s_block_or_goes_back_with_it() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    run(
        &mut db,
        &format!("put holds {{state: 'held', until: {}}}", t0()),
    )
    .unwrap();
    db.set_clock(Some(t0() + HOUR));
    let slot = due(&db, t0() + HOUR).run.expect("a slot due").0;
    let before = db.change_seq();
    let table = Arc::clone(db.statements());
    run_slot(&mut db, BODY, "lapse", slot, 3).unwrap();
    assert!(
        Arc::ptr_eq(&table, db.statements()),
        "a mark changes its statement's alone, never making a table"
    );
    let (seq, record) = tap.last();
    assert_eq!(ops_in(&record), vec![5], "the mark, after the run's write");
    assert_eq!(writes_in(&record).unwrap(), 2, "the write and its mark");
    assert_eq!(seq, before + 2);
    let last = db.last_mark("lapse").cloned().unwrap();
    assert_eq!((last.at, last.skipped, last.refused), (slot, 3, None));

    // Put back with its block: the mark too.
    run(
        &mut db,
        &format!("put holds {{state: 'held', until: {}}}", t0()),
    )
    .unwrap();
    db.begin().unwrap();
    run(&mut db, BODY).unwrap();
    db.mark_slot("lapse", ran(&db, "lapse", slot + 15 * MINUTE, 0))
        .unwrap();
    db.rollback();
    assert_eq!(db.last_mark("lapse").unwrap().at, slot);
    assert!(db.pending().is_empty());

    // A mark of another statement, or of a definition since replaced.
    let e = db
        .mark_slot("nothing", ran(&db, "lapse", slot, 0))
        .unwrap_err();
    assert!(matches!(e, Error::NotFound(_)), "{e}");
    let mut stale = ran(&db, "lapse", slot, 0);
    stale.version.1 += 1;
    assert!(db.mark_slot("lapse", stale).is_err());
}

#[test]
fn a_run_that_wrote_nothing_is_marked_pending_until_a_block_lands() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    run(&mut db, "create collection other (n int)").unwrap();
    let now = t0() + HOUR;
    db.set_clock(Some(now));
    let slot = due(&db, now).run.unwrap().0;
    let writes = tap.writes.lock().unwrap().len();
    // The body finds nothing: no record, no fsync, no send.
    run_slot(&mut db, BODY, "lapse", slot, 0).unwrap();
    assert_eq!(tap.writes.lock().unwrap().len(), writes, "nothing landed");
    assert_eq!(db.pending().len(), 1);
    assert!(db.last_mark("lapse").is_none());
    // Due again? No: the pending mark counts, landed or not.
    let d = due(&db, now);
    assert_eq!(d.run, None);
    assert_eq!(d.next, Some(slot + 15 * MINUTE));

    // A block put back keeps it pending.
    db.begin().unwrap();
    run(&mut db, "put other {n: 1}").unwrap();
    db.rollback();
    assert_eq!(db.pending().len(), 1);
    let e = refused(run(&mut db, "put other {n: 'x'}"));
    assert!(!e.is_empty());
    assert_eq!(db.pending().len(), 1);

    // An image holds it, and leaves it pending.
    let image = opened(&db.snapshot());
    assert_eq!(image.last_mark("lapse").map(|m| m.at), Some(slot));
    assert_eq!(db.pending().len(), 1);

    // The next block that lands carries it as its last record.
    let before = db.change_seq();
    run(&mut db, "put other {n: 2}").unwrap();
    let (seq, record) = tap.last();
    assert_eq!(ops_in(&record), vec![5]);
    assert_eq!((writes_in(&record).unwrap(), seq), (2, before + 2));
    assert!(db.pending().is_empty());
    let last = db.last_mark("lapse").cloned().unwrap();
    assert_eq!(last.at, slot);
    // The file read again holds it.
    let again = opened(&tap.bytes());
    assert_eq!(again.last_mark("lapse"), Some(&last));

    // One a later run's landed mark passed counts for nothing: the run
    // landing its writes and its mark leaves it behind.
    let next = slot + 15 * MINUTE;
    run_slot(&mut db, BODY, "lapse", next, 0).unwrap();
    assert_eq!(db.pending().len(), 1);
    let writes = format!("put holds {{state: 'held', until: {}}}; {BODY}", t0());
    run_slot(&mut db, &writes, "lapse", next + 15 * MINUTE, 1).unwrap();
    assert_eq!(ops_in(&tap.last().1), vec![5], "its own mark alone");
    assert!(db.pending().is_empty());
    let last = db.last_mark("lapse").cloned().unwrap();
    assert_eq!((last.at, last.skipped), (next + 15 * MINUTE, 1));
    run(&mut db, "put other {n: 3}").unwrap();
    assert_eq!(ops_in(&tap.last().1), Vec::<u8>::new());
}

#[test]
fn a_mark_takes_no_room_a_quota_counts_nor_a_write_a_rate_charges() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    run(
        &mut db,
        &format!("put holds {{state: 'held', until: {}}}", t0()),
    )
    .unwrap();
    let slot = due(&db, t0() + HOUR).run.unwrap().0;
    run_slot(&mut db, "del holds where state = 'none'", "lapse", slot, 0).unwrap();
    assert_eq!(db.pending().len(), 1);
    // At its quota: what grows is refused, a delete let through.
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let saw = seen.clone();
    db.set_limit(Some(Arc::new(move |_: &Database, l: &Landing| {
        saw.lock().unwrap().push(l.writes);
        match l.grows {
            true => Err("at its quota".into()),
            false => Ok(()),
        }
    })));
    assert!(run(&mut db, "put holds {state: 'held'}").is_err());
    assert_eq!(db.pending().len(), 1, "kept pending by the block put back");
    run(&mut db, "del holds where state = 'held'").unwrap();
    assert!(db.pending().is_empty(), "carried by the delete");
    assert_eq!(ops_in(&tap.last().1), vec![5]);
    assert_eq!(*seen.lock().unwrap().last().unwrap(), 1, "the delete alone");
}

#[test]
fn an_image_a_compact_a_replica_and_a_reader_of_the_changes_keep_the_marks() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    run(
        &mut db,
        "create statement nightly at '30 2 * * *' in 'Europe/Berlin' scans as \
         del holds where state = 'lapsed'",
    )
    .unwrap();
    for i in 0..3 {
        run(
            &mut db,
            &format!("put holds {{state: 'held', until: {}}}", t0()),
        )
        .unwrap();
        let slot = t0() + (i + 1) * 15 * MINUTE;
        run_slot(&mut db, BODY, "lapse", slot, i as u64).unwrap();
    }
    let refusal = SlotMark {
        at: utc(2026, 3, 29, 1, 0),
        version: version(&db, "nightly"),
        refused: Some((412, "nothing to purge".into())),
        skipped: 0,
    };
    db.begin().unwrap();
    run(&mut db, "put holds {state: 'x'}").unwrap();
    db.mark_slot("nightly", refusal.clone()).unwrap();
    db.commit().unwrap();
    let marks = |db: &Database| -> Vec<_> {
        db.statements()
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    s.schedule.clone(),
                    db.last_mark(&s.name).cloned(),
                )
            })
            .collect()
    };
    let want = marks(&db);
    assert_eq!(want[1].2.as_ref().unwrap().refused, refusal.refused);

    // An open of the file, and an image, which writes the last mark alone.
    assert_eq!(marks(&opened(&tap.bytes())), want);
    let image = db.snapshot();
    assert_eq!(marks(&opened(&image)), want);
    assert_eq!(count_marks(&image), 2, "a mark a schedule");

    // A replica, write for write, and a reader of the changes.
    let replica_tap = Tap::default();
    let mut replica = replica_tap.database();
    let writes = tap.writes.lock().unwrap().clone();
    for (seq, record) in &writes {
        replica.apply(record).unwrap();
        assert_eq!(replica.change_seq(), *seq);
    }
    assert_eq!(marks(&replica), want);
    let all: Vec<u8> = writes.iter().flat_map(|(_, r)| r.clone()).collect();
    let mut statements = 0;
    let last = db
        .changes_in(&all, 1, &mut |c| {
            statements += matches!(c.kind, ChangeKind::Statement) as u64;
            true
        })
        .unwrap();
    assert_eq!(last, db.change_seq());
    assert_eq!(
        statements,
        2 + 4,
        "two definitions and four marks, numbered"
    );

    // A compact keeps each schedule's last mark, of a file read into memory
    // and of a mapped one.
    let dir = std::env::temp_dir().join(format!("fennec-scheduled-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for mapped in [false, true] {
        let path = dir.join(format!("compact-{mapped}.fennec"));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, &image).unwrap();
        fennec_ql::install();
        let mut db = match mapped {
            true => fennec_core::fs::open_mapped(&path).unwrap(),
            false => fennec_core::fs::open(&path).unwrap(),
        };
        assert_eq!(marks(&db), want);
        run(&mut db, "compact").unwrap();
        drop(db);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(count_marks(&bytes), 2, "mapped: {mapped}");
        assert_eq!(marks(&opened(&bytes)), want, "mapped: {mapped}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The marks a file holds: each op-5 record's head and name, `lapse`'s or
/// `nightly`'s, short enough for a length of one byte.
fn count_marks(file: &[u8]) -> usize {
    let mut n = 0;
    for name in ["lapse", "nightly"] {
        let mut tail = vec![5u8, name.len() as u8];
        tail.extend_from_slice(name.as_bytes());
        n += file
            .windows(3 + tail.len())
            .filter(|w| w[0] == 13 && w[1] == 0 && w[3..] == tail[..])
            .count();
    }
    n
}

/// A compact beside the database, a run landing meanwhile: the swap writes
/// the mark as it stands then, and the compact is not refused for it.
#[test]
fn a_compact_beside_the_database_keeps_a_mark_landed_meanwhile() {
    let dir = std::env::temp_dir().join(format!("fennec-scheduled-beside-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("beside.fennec");
    let _ = std::fs::remove_file(&path);
    fennec_ql::install();
    let mut db = fennec_core::fs::open_mapped(&path).unwrap();
    db.set_clock(Some(t0()));
    db.allow_schedules();
    run(&mut db, HOLDS).unwrap();
    run(&mut db, LAPSE).unwrap();
    db.set_clock(Some(t0() + HOUR));
    for _ in 0..50 {
        run(
            &mut db,
            &format!("put holds {{state: 'held', until: {}}}", t0()),
        )
        .unwrap();
    }
    run(&mut db, "del holds where state = 'held'").unwrap();
    let db = std::sync::RwLock::new(db);
    let compact = fennec_ql::parse_one("compact").unwrap();
    let mut slot = t0();
    Database::maintain_with(&db, &compact, &mut || {
        let mut g = db.write().unwrap();
        slot += 15 * MINUTE;
        run(
            &mut g,
            &format!("put holds {{state: 'held', until: {}}}", t0()),
        )
        .unwrap();
        run_slot(&mut g, BODY, "lapse", slot, 0).unwrap();
    })
    .expect("a compact beside the database")
    .expect("not refused for a mark landed meanwhile");
    let db = db.into_inner().unwrap();
    let last = db.last_mark("lapse").cloned().unwrap();
    assert_eq!(last.at, slot);
    drop(db);
    let bytes = std::fs::read(&path).unwrap();
    let again = opened(&bytes);
    assert_eq!(again.last_mark("lapse"), Some(&last));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_replacement_keeps_the_kind_moves_since_and_clears_the_mark() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    run(
        &mut db,
        &format!("put holds {{state: 'held', until: {}}}", t0()),
    )
    .unwrap();
    // A mark a clock a year ahead left.
    let ahead = t0() + 365 * DAY;
    run_slot(&mut db, BODY, "lapse", ahead, 0).unwrap();
    // And one of a run that wrote nothing, pending.
    run_slot(&mut db, BODY, "lapse", ahead + MINUTE * 15, 0).unwrap();
    assert_eq!(db.pending().len(), 1);
    let later = t0() + 2 * HOUR + 20 * MINUTE;
    db.set_clock(Some(later));
    let daily = "create or replace statement lapse every 1d in 'Europe/Berlin'
  as set holds {state: 'lapsed'} where state = 'held' and until < now()";
    // A replacement put back with its block puts the mark it took back.
    db.begin().unwrap();
    run(&mut db, daily).unwrap();
    assert!(db.last_mark("lapse").is_none());
    db.rollback();
    assert_eq!(db.last_mark("lapse").map(|m| m.at), Some(ahead));
    run(&mut db, daily).unwrap();
    let s = db.statements().get("lapse").unwrap().clone();
    let t = s.schedule.unwrap();
    assert_eq!((t.times, t.since), (Times::Step(DAY, 0), later));
    assert!(db.last_mark("lapse").is_none(), "the mark cleared");
    // The pending mark names the definition replaced: it counts for
    // nothing, and the next block drops it.
    let d = due(&db, later + HOUR);
    assert_eq!((d.run, d.ahead), (None, None));
    assert_eq!(d.next, Some(utc(2026, 3, 28, 23, 0)), "Berlin's midnight");
    run(&mut db, "put holds {state: 'x'}").unwrap();
    assert!(db.pending().is_empty());
    assert_eq!(ops_in(&tap.last().1), Vec::<u8>::new());

    // The kind is kept: called, bound and scheduled each stay what they
    // are, a `drop` and a `create` making another.
    run(
        &mut db,
        "create statement ask() as get holds select id where state = 'x'",
    )
    .unwrap();
    for (text, what) in [
        (
            "create or replace statement ask every 1h as set holds {state: 'y'} where state = 'x'",
            "runs by the clock, where it was called",
        ),
        (
            "create or replace statement lapse() as get holds select id where state = 'x'",
            "is called, where it ran by the clock",
        ),
        (
            "create or replace statement lapse after insert holds as set holds {state: 'y'} \
             where id = new.id",
            "is bound to writes, where it ran by the clock",
        ),
    ] {
        let e = refused(run(&mut db, text));
        assert!(e.contains(what), "{text}: {e}");
    }
}

#[test]
fn a_database_schedules_at_most_sixteen_and_a_schema_change_meets_each() {
    let tap = Tap::default();
    let mut db = holds(&tap);
    for i in 0..MAX_SCHEDULED {
        run(
            &mut db,
            &format!("create statement s{i} every 1h as del holds where state = 's{i}'"),
        )
        .unwrap();
    }
    let e = refused(run(
        &mut db,
        "create statement one_more every 1h as del holds where state = 'x'",
    ));
    assert!(e.contains("at most 16"), "{e}");
    run(
        &mut db,
        "create or replace statement s0 every 2h as del holds where state = 'x'",
    )
    .unwrap();
    // What it reads, a schema change refused naming it.
    let e = refused(run(&mut db, "alter collection holds drop field state"));
    assert!(e.contains("statement `s0`"), "{e}");
}

/// A record whose op, schedule or outcome this version does not read is
/// refused as corrupt, never read as something else.
#[test]
fn a_schedule_or_a_mark_this_version_does_not_read_is_refused() {
    let tap = Tap::default();
    let db = defined(&tap);
    let file = tap.bytes();
    let def = |times: &[u8]| {
        let mut body = vec![4u8];
        put_uvarint(&mut body, 0);
        put_uvarint(&mut body, 99);
        put_uvarint(&mut body, GRAMMAR_EPOCH as u64);
        body.extend_from_slice(&1u64.to_le_bytes());
        encode_str(&mut body, "later");
        put_uvarint(&mut body, 0);
        encode_str(
            &mut body,
            "create statement later every 1m as del holds where id = 1",
        );
        body.extend_from_slice(times);
        encode_str(&mut body, "");
        put_uvarint(&mut body, 0);
        body
    };
    let mark = |outcome: u8, status: u64| {
        let mut body = vec![5u8];
        encode_str(&mut body, "lapse");
        put_uvarint(&mut body, 0);
        let (h, c) = version(&db, "lapse");
        put_uvarint(&mut body, h);
        put_uvarint(&mut body, c);
        body.push(outcome);
        put_uvarint(&mut body, status);
        put_uvarint(&mut body, 0);
        encode_str(&mut body, "");
        body
    };
    let mut minute = vec![1u8];
    put_uvarint(&mut minute, MINUTE as u64);
    put_uvarint(&mut minute, 0);
    let mut seconds = vec![1u8];
    put_uvarint(&mut seconds, 30_000);
    put_uvarint(&mut seconds, 0);
    let empty_cron = vec![3u8, 0, 1, 1, 1, 1];
    let mut past = def(&minute);
    past.push(0);
    for (body, reads) in [
        (def(&minute), true),
        (def(&seconds), false),
        (def(&empty_cron), false),
        (def(&[9, 1]), false),
        (past, false),
        (mark(1, 0), true),
        (mark(2, 409), true),
        (mark(3, 0), false),
        (mark(1, 200), false),
        (vec![6u8, 0], false),
    ] {
        let mut f = file.clone();
        f.push(13);
        put_uvarint(&mut f, 0);
        put_uvarint(&mut f, body.len() as u64);
        f.extend_from_slice(&body);
        let mut db = Database::new();
        match (db.load(&f), reads) {
            (Ok(_), true) => {}
            (Err(e), false) => assert!(
                e.to_string()
                    .contains("a statement record this version does not read"),
                "{e}"
            ),
            (r, _) => panic!("{body:?}: {:?}", r.err()),
        }
    }
}

// ------------------------------------------------------------- the clock

/// A database holding one schedule of `text`, defined at `since`.
fn scheduled(text: &str, since: i64) -> (Tap, Database) {
    let tap = Tap::default();
    let mut db = holds(&tap);
    db.set_clock(Some(since));
    run(&mut db, text).unwrap();
    (tap, db)
}

const HOURLY: &str = "create statement h every 1h as del holds where state = 'x'";

#[test]
fn a_definition_runs_no_slot_from_before_it() {
    let at = |h, m| utc(2026, 5, 4, h, m);
    let (_, db) = scheduled(HOURLY, at(2, 59));
    let d = due(&db, at(3, 0));
    assert_eq!(d.run, Some((at(3, 0), 0)));
    let (_, db) = scheduled(HOURLY, at(3, 1));
    let d = due(&db, at(3, 30));
    assert_eq!((d.run, d.next), (None, Some(at(4, 0))));
    let (_, db) = scheduled(HOURLY, at(3, 0));
    assert_eq!(due(&db, at(3, 0)).run, None, "none at its own instant");
}

#[test]
fn missed_slots_run_once_the_latest() {
    let at = |h, m| utc(2026, 5, 4, h, m);
    let (_, mut db) = scheduled(HOURLY, at(2, 59));
    db.mark_slot("h", ran(&db, "h", at(3, 0), 0)).unwrap();
    // Stepped forward three periods: one run, two passed over.
    let d = due(&db, at(6, 10));
    assert_eq!(d.run, Some((at(6, 0), 2)));
    assert_eq!(d.next, Some(at(7, 0)));
    // Stepped back, staying past the mark: nothing runs again.
    let d = due(&db, at(3, 30));
    assert_eq!((d.run, d.next, d.ahead), (None, Some(at(4, 0)), None));
}

#[test]
fn a_mark_ahead_of_the_clock_pauses_an_hour_at_most_then_goes_on_from_it() {
    let at = |h, m| utc(2026, 5, 4, h, m);
    let (_, mut db) = scheduled(HOURLY, at(0, 30));
    db.mark_slot("h", ran(&db, "h", at(5, 0), 0)).unwrap();
    // Behind the mark by under an hour: a pause, nothing run twice.
    let d = due(&db, at(4, 20));
    assert_eq!(d.ahead, Some(at(5, 0)));
    assert_eq!((d.run, d.next), (None, Some(at(6, 0))));
    // By more: the mark is the wrong reading, and the schedule goes on
    // from the clock, after `since`.
    let d = due(&db, at(5, 0) - AHEAD_MAX - 1);
    assert_eq!(d.ahead, Some(at(5, 0)));
    assert_eq!(d.run, Some((at(3, 0), 2)));
    assert_eq!(d.next, Some(at(4, 0)));
    // A `since` far ahead anchors nothing: the latest slot runs once, and
    // its mark anchors the slots after it, each of which runs.
    let (_, mut db) = scheduled(HOURLY, at(23, 30));
    let d = due(&db, at(10, 15));
    assert_eq!(d.ahead, Some(at(23, 30)));
    assert_eq!((d.run, d.next), (Some((at(10, 0), 0)), Some(at(11, 0))));
    db.mark_slot("h", ran(&db, "h", at(10, 0), 0)).unwrap();
    assert_eq!(due(&db, at(10, 50)).run, None, "once");
    assert_eq!(due(&db, at(11, 5)).run, Some((at(11, 0), 0)));
    db.mark_slot("h", ran(&db, "h", at(11, 0), 0)).unwrap();
    let d = due(&db, at(14, 5));
    assert_eq!((d.run, d.next), (Some((at(14, 0), 2)), Some(at(15, 0))));
    // A mark that far ahead too: the same, from the clock.
    let (_, mut db) = scheduled(HOURLY, at(23, 30));
    db.begin().unwrap();
    run(&mut db, "put holds {state: 'x'}").unwrap();
    db.mark_slot("h", ran(&db, "h", at(23, 0) + DAY, 0))
        .unwrap();
    db.commit().unwrap();
    let d = due(&db, at(10, 15));
    assert_eq!(d.run, Some((at(10, 0), 0)));
}

/// Berlin's clocks go forward at 02:00 on 29 March 2026 and back at 03:00
/// on 25 October.
#[test]
fn summer_time_through_due() {
    let text = "create statement night at '30 2 * * *' in 'Europe/Berlin' as \
                del holds where state = 'x'";
    let (_, db) = scheduled(text, t0());
    let jump = utc(2026, 3, 29, 1, 0);
    assert_eq!(due(&db, jump - 1).run, None);
    assert_eq!(due(&db, jump).run, Some((jump, 0)), "at the jump, once");
    assert_eq!(due(&db, jump).next, Some(utc(2026, 3, 30, 0, 30)));
    let (_, mut db) = scheduled(text, utc(2026, 10, 24, 12, 0));
    let first = utc(2026, 10, 25, 0, 30);
    assert_eq!(due(&db, first).run, Some((first, 0)));
    db.mark_slot("night", ran(&db, "night", first, 0)).unwrap();
    // The second 02:30, an hour on, runs nothing.
    let d = due(&db, first + HOUR + MINUTE);
    assert_eq!((d.run, d.next), (None, Some(utc(2026, 10, 26, 1, 30))));
    // `every 1h` there: twice in the hour shown twice.
    let (_, db) = scheduled(
        "create statement h every 1h in 'Europe/Berlin' as del holds where state = 'x'",
        utc(2026, 10, 24, 23, 30),
    );
    let d = due(&db, utc(2026, 10, 25, 2, 0));
    assert_eq!(d.run, Some((utc(2026, 10, 25, 2, 0), 2)));
}

// ------------------------------------------------------------ the fixture

/// `file` with `lapse` after its records, once, as this build writes it,
/// and a run of a slot marked in the block of its write: what
/// `tests/statements.fennec` gained in F3's slice 2, which the browser
/// module opens and keeps through its images (`web/fennec.test.js`).
pub(crate) fn with_schedule(file: Vec<u8>) -> Vec<u8> {
    let tap = Tap {
        file: Arc::new(std::sync::Mutex::new(file.clone())),
        writes: Arc::default(),
    };
    let mut db = tap.database();
    db.load(&file).unwrap();
    if db.statements().get("lapse").is_some() {
        return tap.bytes();
    }
    db.set_clock(Some(t0()));
    db.allow_schedules();
    run(&mut db, HOLDS).unwrap();
    run(&mut db, LAPSE).unwrap();
    run(
        &mut db,
        &format!("put holds {{state: 'held', until: {}}}", t0()),
    )
    .unwrap();
    let now = utc(2026, 3, 28, 13, 5);
    db.set_clock(Some(now));
    let (slot, skipped) = due(&db, now).run.unwrap();
    run_slot(&mut db, BODY, "lapse", slot, skipped).unwrap();
    tap.bytes()
}

/// `tests/statements.fennec` holds a schedule and its mark after the
/// binding: every build reads both and keeps them through its images.
#[test]
fn the_fixture_s_schedule_and_its_mark_are_read_and_kept() {
    let file = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/statements.fennec"
    ))
    .expect("the fixture");
    let db = opened(&file);
    let s = db.statements().get("lapse").expect("the schedule").clone();
    assert!(s.tree.is_some() && s.broken.is_none(), "{:?}", s.broken);
    let t = s.schedule.clone().unwrap();
    assert_eq!((t.times, t.since), (Times::Step(15 * MINUTE, 0), t0()));
    let last = db.last_mark("lapse").cloned().expect("its mark");
    assert_eq!((last.at, last.skipped), (utc(2026, 3, 28, 13, 0), 3));
    assert_eq!(last.version, s.version);
    assert_eq!(
        db.query(
            &fennec_ql::parse_one("get holds select state").unwrap(),
            &[]
        )
        .unwrap()
        .rows()
        .unwrap()
        .rows[0]
            .values,
        vec![Value::Text("lapsed".into())],
        "the run's write, landed with its mark"
    );
    let again = opened(&db.snapshot());
    assert_eq!(again.statements().get("lapse"), Some(&s));
    let d = due(&db, utc(2026, 3, 28, 13, 20));
    assert_eq!(d.run, Some((utc(2026, 3, 28, 13, 15), 0)));
}

// ------------------------------------------------------------ the runs

const STAMPS: &str = "create collection stamps (slot timestamp, at timestamp)";

/// Stamps the slot it ran for and the time it read, twice, in one run.
const STAMP: &str = "create statement stamp every 1h in 'Europe/Berlin' as begin
  insert stamps {slot: slot(), at: now()};
  insert stamps {slot: slot(), at: now()}
end";

fn stamped(db: &Database) -> Vec<(Value, Value)> {
    let stmt = fennec_ql::parse_one("get stamps select slot, at order id").unwrap();
    let rows = db.query(&stmt, &[]).unwrap();
    let rows = rows.rows().unwrap().rows.clone();
    rows.into_iter()
        .map(|r| (r.values[0].clone(), r.values[1].clone()))
        .collect()
}

/// A run's body reads `now()` as the run's start, once a run, and
/// `slot()` as its slot -- `Database::run_slot`, in the block its runner
/// opened, with its mark -- the database's own clock as it was after.
#[test]
fn a_run_reads_its_start_once_and_its_slot() {
    let tap = Tap::default();
    let mut db = holds(&tap);
    run(&mut db, STAMPS).unwrap();
    run(&mut db, STAMP).unwrap();
    let late = t0() + HOUR + 40 * MINUTE;
    let slot = due(&db, late).run.unwrap().0;
    assert_eq!(slot, t0() + HOUR);
    db.set_clock(Some(t0()));
    db.begin().unwrap();
    let version = db.run_slot("stamp", slot, late).unwrap();
    let mark = SlotMark {
        at: slot,
        version,
        refused: None,
        skipped: 0,
    };
    db.mark_slot("stamp", mark).unwrap();
    db.commit().unwrap();
    let ts = Value::Timestamp;
    assert_eq!(
        stamped(&db),
        vec![(ts(slot), ts(late)), (ts(slot), ts(late))]
    );
    assert_eq!(ops_in(&tap.last().1), vec![5], "one block, its mark last");
    // The database's clock is its own again.
    let stmt = fennec_ql::parse_one("get stamps select now() as t limit 1").unwrap();
    let now = db.query(&stmt, &[]).unwrap();
    assert_eq!(now.rows().unwrap().rows[0].values[0], ts(t0()));
    // Out of a run `slot()` is no function.
    let stmt = fennec_ql::parse_one("get stamps select slot() as s limit 1").unwrap();
    let e = refused(db.query(&stmt, &[]));
    assert!(e.contains("unknown function `slot`"), "{e}");
}

/// The server's token runs one by hand with `call`: its body runs, its
/// `slot()` the latest slot at or before the call's `now()` -- the slot
/// that has come, under its label -- and no slot is marked, landed or
/// pending; the slot still due runs at the scheduler's look.
#[test]
fn a_call_by_hand_runs_the_slot_that_has_come_and_marks_none() {
    let tap = Tap::default();
    let mut db = holds(&tap);
    run(&mut db, STAMPS).unwrap();
    run(&mut db, STAMP).unwrap();
    let at = t0() + HOUR + 30 * MINUTE;
    db.set_clock(Some(at));
    run(&mut db, "call stamp").unwrap();
    let ts = Value::Timestamp;
    assert_eq!(
        stamped(&db),
        vec![(ts(t0() + HOUR), ts(at)), (ts(t0() + HOUR), ts(at))]
    );
    assert!(db.last_mark("stamp").is_none() && db.pending().is_empty());
    assert_eq!(due(&db, at).run, Some((t0() + HOUR, 0)), "still due");
    assert_eq!(db.slot_at("stamp", at), Some(t0() + HOUR));
    assert_eq!(db.slot_at("lapse", at), None, "no such schedule");
}

/// `slot()` is a constant of a run: a filter on it reaches an index as a
/// parameter does -- at the definition, which takes it with no `scans`,
/// and in the run and the call by hand, each reading one row of ten where
/// `--call-max-rows` lets two be read.
#[test]
fn slot_reaches_an_index_as_a_parameter_does() {
    let tap = Tap::default();
    let mut db = holds(&tap);
    run(
        &mut db,
        "create collection logs (period timestamp @hash, n int)",
    )
    .unwrap();
    let text = "create statement b every 15m as set logs {n: 2} where period = slot()";
    run(&mut db, text).unwrap();
    let e = refused(run(
        &mut db,
        "create statement c every 15m as set logs {n: 3} where n = 1",
    ));
    assert!(e.contains("say `scans`"), "{e}");
    let slot = t0() + 15 * MINUTE;
    for i in 0..10 {
        run(
            &mut db,
            &format!("put logs {{period: {}, n: 1}}", slot - i * HOUR),
        )
        .unwrap();
    }
    db.set_call_max_rows(2);
    db.begin().unwrap();
    db.run_slot("b", slot, slot + MINUTE).unwrap();
    db.commit().unwrap();
    let stmt = fennec_ql::parse_one("get logs where n = 2 select period").unwrap();
    let rows = db.query(&stmt, &[]).unwrap().rows().unwrap().rows.clone();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Timestamp(slot));
    // By hand, the slot that has come.
    db.set_clock(Some(slot - HOUR + MINUTE));
    run(&mut db, "call b").unwrap();
    let stmt = fennec_ql::parse_one("get logs where n = 2 count").unwrap();
    let n = db.query(&stmt, &[]).unwrap().rows().unwrap().rows[0].values[0].clone();
    assert_eq!(n, Value::Int(2));
}

/// A slot's mark is a write that changes nothing read: a block carrying
/// one names the collections of its other writes alone -- none for a
/// mark alone -- where a definition names one no name stands for, so a
/// held write is woken by a run's writes, never by its mark.
#[test]
fn a_mark_names_no_collection_among_the_changes() {
    let tap = Tap::default();
    let mut db = defined(&tap);
    run(&mut db, "create collection other (n int)").unwrap();
    let now = t0() + HOUR;
    let slot = due(&db, now).run.unwrap().0;
    // A run that wrote nothing: its mark pending, then carried by a write.
    run_slot(&mut db, BODY, "lapse", slot, 0).unwrap();
    let before = db.change_seq();
    run(&mut db, "put other {n: 1}").unwrap();
    assert_eq!(ops_in(&tap.last().1), vec![5]);
    let other = Some(vec!["other".to_string()]);
    assert_eq!(db.changed_collections_since(before), other);
    // A run's writes and its mark: the run's collection.
    let before = db.change_seq();
    let writes = format!("put holds {{state: 'held', until: {}}}; {BODY}", t0());
    run_slot(&mut db, &writes, "lapse", slot + 15 * MINUTE, 0).unwrap();
    let holds = Some(vec!["holds".to_string()]);
    assert_eq!(db.changed_collections_since(before), holds);
    // A mark alone, landed with an image's worth of nothing: no name.
    let before = db.change_seq();
    run_slot(&mut db, BODY, "lapse", slot + 30 * MINUTE, 0).unwrap();
    db.begin().unwrap();
    db.commit().unwrap();
    assert_eq!(db.change_seq(), before, "an empty block lands nothing");
    // A definition: every collection.
    let before = db.change_seq();
    let hourly = "create statement h every 1h as del holds where state = 'x'";
    run(&mut db, hourly).unwrap();
    assert_eq!(db.changed_collections_since(before), None);
}

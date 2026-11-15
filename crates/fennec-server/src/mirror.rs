//! `fennec-server --follow`: a PostgreSQL table mirrored into the file this
//! server serves, over HTTP and its subscriptions at once.
//!
//! `fennec import --follow` is the single writer of the file it keeps, so
//! serving its mirror took a second process over a copy of it: two
//! processes over one file corrupt it. Here the importer's follower
//! (`fennec_import::follow`) runs on a thread of the server's, over the
//! database the server serves -- the copy when there is no whole one, then
//! the changes as they commit -- and its writes reach a subscriber as any
//! write does, a primary's replicas too.
//!
//! The mirrored collection is the follower's to write: a write from
//! anyone else is refused ([`Guard`]), since the next change from
//! PostgreSQL would overwrite it, or a copy made again forget it. What the
//! follower confirms to the server is on disk first, as ever, and it stops
//! with the server, before the checkpoint.

use fennec_core::error::{Error, Result};
use fennec_core::plugin::{Hook, Plugin, Registry, WriteOp};
use fennec_core::prelude::*;
use fennec_import::follow::{self, lsn_text, Event, Follow};
use fennec_import::pg::Url;
use fennec_import::Options;
use std::cell::Cell;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

thread_local! {
    /// Whether this thread is the follower's, the one that may write the
    /// mirrored collection. The follower does all of its writing on the
    /// thread [`start`] gives it.
    static FOLLOWER: Cell<bool> = const { Cell::new(false) };
}

/// How long the shutdown waits for the follower to put its last changes on
/// disk and confirm them, then end its stream, the slot let go for the
/// next start -- counted from the shutdown's start, as the follower's stop
/// is, since it looks at the same flag (within `IDLE_WAIT`, 0.2 s). A copy
/// still being made stops too, within `fennec_wire::client::LOOK` of the
/// flag, and is left unfinished for the next start to make again, as a copy
/// cut short always is: it holds no lock while it waits on PostgreSQL, so
/// the shutdown is never held up by it, whatever PostgreSQL does, nor by a
/// dial or a sign-in, each bounded by `connect_timeout`. Past the wait, a
/// stream the server has not ended yet is closed by the exit.
///
/// The follower's own wait for the stream's end (`follow::END_WAIT`, 3 s)
/// fits inside it with a second and more for the settle's fsync before, so
/// it ends by its own bound and says so rather than being cut off here: at
/// 10 s it never could. The shutdown's waits before the final sync run one
/// after another -- `HELD_GRACE` 2 s, `conn::END_GRACE` 0.5 s, the traces'
/// flush 2 s where tracing is on, then this one -- while the follower stops
/// beside them: counted from its own start this wait came after theirs, and
/// with `--follow` and tracing the waits came to 9.7 s of docker's 10 s
/// before its SIGKILL, the syncer's `LOOK` with them. Counted from the
/// shutdown's start, the final sync begins 5.2 s after the signal at most,
/// the rest of docker's 10 s left to it and the checkpoint; a checkpoint
/// cut off past it loses the graph's image, never a write
/// (`durability::shutdown`).
const STOP_WAIT: Duration = Duration::from_secs(5);
const _: () = assert!(follow::END_WAIT.as_millis() + 1000 < STOP_WAIT.as_millis());

/// What `--follow` was given.
pub struct Mirror {
    pub url: Url,
    pub table: String,
    pub opts: Options,
    pub follow: Follow,
}

/// Refuses a write to the mirrored collection from anything but the
/// follower's thread.
struct Guard {
    collection: String,
}

impl Guard {
    fn check(&self, collection: &str) -> Result<()> {
        if collection == self.collection && !FOLLOWER.with(Cell::get) {
            return Err(Error::ReadOnly(format!(
                "`{collection}` mirrors a PostgreSQL table: its writes come from there"
            )));
        }
        Ok(())
    }
}

impl Hook for Guard {
    fn name(&self) -> &str {
        "mirror"
    }
    /// It refuses a write and changes nothing of one.
    fn rewrites(&self) -> bool {
        false
    }
    fn before_write(&self, schema: &Schema, _op: WriteOp, _doc: &mut Document) -> Result<()> {
        self.check(&schema.name)
    }
    /// A delete reaches the hooks only here, before it is made.
    fn after_write(&self, collection: &str, op: WriteOp, _doc: &Document) -> Result<()> {
        match op {
            WriteOp::Delete => self.check(collection),
            _ => Ok(()),
        }
    }
}

/// Installs [`Guard`] over `collection`.
pub struct GuardPlugin(pub String);

impl Plugin for GuardPlugin {
    fn name(&self) -> &str {
        "mirror"
    }
    fn init(&self, reg: &mut Registry) -> Result<()> {
        reg.register_hook(Arc::new(Guard {
            collection: self.0.clone(),
        }));
        Ok(())
    }
}

/// Starts the follower on a thread of its own over `db`, stopping when the
/// server does. An error it cannot wait out stops the server, exit code 1,
/// and so does a panic of its thread (`Ending`): a server that went on
/// answering from a mirror that no longer moves would say nothing of it.
/// It stops it through the one shutdown, never by exiting here, which cut
/// short a shutdown already under way -- its sync among what was cut -- and
/// skipped the sync of the writes it had applied.
pub fn start(m: Mirror, db: Arc<RwLock<Database>>) -> std::io::Result<()> {
    // What the dials ask of TLS, and why, said once before the first.
    fennec_http::log!("follow: {}", m.url.tls_said());
    let wait = spawn(m.table.clone(), move |stop, cut| {
        FOLLOWER.with(|f| f.set(true));
        let mut report = reporter(&m);
        follow::run(
            &m.url,
            &m.table,
            &db,
            &m.opts,
            &m.follow,
            stop,
            cut,
            &mut report,
        )
    })?;
    crate::durability::before_shutdown(wait);
    Ok(())
}

/// Runs `follower` on a thread of its own, `fennec-follow`, given the
/// shutdown's flag to stop at and a cut nothing sets: the shutdown's wait
/// for it, which [`start`] registers, given the moment the shutdown began.
fn spawn(
    table: String,
    follower: impl FnOnce(&AtomicBool, &AtomicBool) -> Result<()> + Send + 'static,
) -> std::io::Result<impl FnOnce(Instant) + Send + 'static> {
    let (done, stopped) = mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("fennec-follow".into())
        .spawn(move || {
            // First, so that it is dropped last, the follower's own drops
            // -- an unwind's among them -- before it.
            let mut ending = Ending {
                table,
                done: Some(done),
                ended: None,
            };
            let stop = crate::durability::shutdown_flag();
            // Nothing cuts the follower's wait for its stream's end short
            // but its own bound: a second signal starts no second shutdown.
            let cut = AtomicBool::new(false);
            ending.ended = Some(follower(stop, &cut));
        })?;
    // Counted from the shutdown's start, as the follower's stop is -- it
    // looks at the same flag -- not from this wait's: the waits before it
    // ran while the follower stopped (`STOP_WAIT`).
    Ok(move |began: Instant| {
        let _ = stopped.recv_timeout(STOP_WAIT.saturating_sub(began.elapsed()));
    })
}

/// How the follower's thread ends, its follower returned or not: a guard
/// that runs on an unwind too. A follower that panics -- `fennec-server`'s
/// release profile unwinds -- stops the server as one that fails does,
/// exit code 1; the thread's end let the shutdown's wait go and nothing
/// more, and the server served on a mirror that no longer moved, and
/// exited 0 at its next shutdown, with a checkpoint.
struct Ending {
    table: String,
    /// The shutdown's wait for this thread, let go once the code is stored.
    done: Option<mpsc::Sender<()>>,
    /// What the follower returned; `None` while it runs, and after a panic.
    ended: Option<Result<()>>,
}

impl Drop for Ending {
    fn drop(&mut self) {
        let failed = !matches!(self.ended, Some(Ok(())));
        // The code first, before the line and before the wait is let go: a
        // shutdown already waiting on this thread exits as soon as it is,
        // and the follower returned an error before anything of it waited
        // (`follow::run`).
        if failed {
            crate::durability::exit_with(crate::exit::Code::Failed);
        }
        match &self.ended {
            Some(Ok(())) => {}
            Some(Err(e)) => {
                fennec_http::log!("follow: the mirror of {} stopped: {}", self.table, said(e))
            }
            None => fennec_http::log!(
                "follow: the mirror of {} stopped: its thread panicked",
                self.table
            ),
        }
        // Stopped either way: the shutdown's wait for this thread ends here
        // rather than at `STOP_WAIT`.
        if let Some(done) = self.done.take() {
            let _ = done.send(());
        }
        if failed {
            crate::durability::request_shutdown();
        }
    }
}

/// The follower's words for a log line: an error or a warning may carry
/// PostgreSQL's -- a column's name, a cell -- or whoever's stands in its
/// place: control characters escaped, the line cut at 4 KiB, as only the
/// follower's lines are. Every other log line is the server's own.
fn said(e: &impl std::fmt::Display) -> String {
    fennec_import::pg::peer_text(e.to_string().as_bytes(), 4096)
}

/// What the follower reports, as log lines: its progress now and then
/// rather than a line a transaction.
fn reporter(m: &Mirror) -> impl FnMut(Event) {
    let (table, slot, publication) = (
        m.table.clone(),
        m.follow.slot.clone(),
        m.follow.publication.clone(),
    );
    let mut last = Instant::now() - Duration::from_secs(60);
    move |e: Event| {
        match e {
        Event::Prepared {
            publication_created,
            slot_created,
        } => {
            let made = |yes: bool| if yes { " (created)" } else { "" };
            fennec_http::log!(
                "follow: {table}: slot {slot}{}, publication {publication}{}",
                made(slot_created),
                made(publication_created)
            );
            if slot_created {
                fennec_http::log!(
                    "follow: the slot keeps the server's WAL until this server confirms it; \
                     to stop following for good: select pg_drop_replication_slot('{slot}')"
                );
            }
        }
        Event::Copying(why) => fennec_http::log!("follow: copying {table}: {why}"),
        Event::CopyProgress(n) => {
            if last.elapsed() >= Duration::from_secs(10) {
                fennec_http::log!("follow: {n} rows of {table} copied");
                last = Instant::now();
            }
        }
        Event::Copied(s) => {
            fennec_http::log!(
                "follow: {table} copied, {} rows in {:.2?}",
                s.rows,
                s.elapsed
            );
            for w in &s.warnings {
                fennec_http::log!("follow: {}", said(w));
            }
        }
        Event::Streaming => fennec_http::log!("follow: {table}: streaming its changes"),
        Event::Confirmed {
            lsn,
            transactions,
            changes,
        } => {
            if last.elapsed() >= Duration::from_secs(60) {
                fennec_http::log!(
                    "follow: {table}: {transactions} transactions, {changes} changes, at {}",
                    lsn_text(lsn)
                );
                last = Instant::now();
            }
        }
        Event::Ignored(c) => fennec_http::log!(
            "follow: column `{c}` of {table} has no field in the collection; its values are not kept"
        ),
        Event::Reconnecting { error, wait } => fennec_http::log!(
            "follow: {table}: the stream broke ({}); again in {wait:.1?}",
            said(&error)
        ),
        Event::SlotHeld(why) => fennec_http::log!("follow: {table}: {}", said(&why)),
    }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(db: &mut Database, sql: &str) -> Result<Response> {
        db.execute(&fennec_ql::parse_one(sql).expect("parse"))
    }

    /// A follower that panics -- the release profile unwinds a panic --
    /// stops the server as one that fails does: exit code 1, stored before
    /// the shutdown's wait for the follower is let go, then the shutdown
    /// asked for. The thread's end used to drop that wait's sender alone:
    /// the server served on a mirror that no longer moved, and its next
    /// shutdown exited 0, with a checkpoint. The one test in this binary
    /// that stores an exit code or asks for a shutdown, both the process's.
    #[test]
    fn a_follower_that_panics_stops_the_server_with_its_code() {
        assert_eq!(crate::durability::exit_code(), crate::exit::Code::Ok);
        let wait = spawn("items".into(), |_, _| panic!("a follower's bug")).unwrap();
        let began = Instant::now();
        wait(began);
        // Ended by the thread's end, not by `STOP_WAIT`.
        assert!(began.elapsed() < STOP_WAIT, "{:?}", began.elapsed());
        assert_eq!(crate::durability::exit_code(), crate::exit::Code::Failed);
        let until = Instant::now() + Duration::from_secs(10);
        while !crate::durability::shutdown_requested() {
            assert!(Instant::now() < until, "no shutdown asked for");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The shutdown's wait for a follower that has not stopped counts from
    /// the shutdown's start, as the follower's stop does: the waits before
    /// it -- the held requests' answers, the TLS ends, the traces' flush,
    /// 4.5 s at most -- are not added to it, and the final sync starts
    /// within `STOP_WAIT` of the shutdown's start. Counted from its own
    /// start, it waited the whole 5 s after them, and with `--follow` and
    /// tracing the waits came to 9.7 s of docker's 10.
    #[test]
    fn the_wait_for_the_follower_counts_from_the_shutdowns_start() {
        use std::sync::atomic::Ordering;
        let release = Arc::new(AtomicBool::new(false));
        let held = Arc::clone(&release);
        let wait = spawn("items".into(), move |_, _| {
            while !held.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        })
        .unwrap();
        let before = Duration::from_millis(4500);
        let began = Instant::now().checked_sub(before).unwrap();
        let t = Instant::now();
        wait(began);
        let took = t.elapsed();
        release.store(true, Ordering::SeqCst);
        assert!(took >= STOP_WAIT - before, "{took:?}");
        assert!(took < Duration::from_secs(3), "{took:?}");
    }

    /// The mirrored collection takes the follower's writes and deletes and
    /// refuses anyone else's, as a replica refuses one; the rest of
    /// the file is written as ever.
    #[test]
    fn the_mirror_takes_the_followers_writes_alone() {
        let mut db = Database::new();
        db.install_plugin(&GuardPlugin("docs".into())).unwrap();
        run(&mut db, "create collection docs (n int)").unwrap();
        run(&mut db, "create collection notes (n int)").unwrap();
        let db = RwLock::new(db);

        let refused = |r: Result<Response>| match r {
            Err(Error::ReadOnly(m)) => assert!(m.contains("mirrors a PostgreSQL table"), "{m}"),
            other => panic!("not refused: {other:?}"),
        };
        refused(run(&mut db.write().unwrap(), "put docs {n: 1}"));

        std::thread::scope(|s| {
            s.spawn(|| {
                FOLLOWER.with(|f| f.set(true));
                let mut g = db.write().unwrap();
                run(&mut g, "put docs {n: 1}").unwrap();
                run(&mut g, "put docs {n: 2}").unwrap();
                run(&mut g, "set docs {n: 3} where n = 2").unwrap();
            })
            .join()
            .unwrap();
        });
        let mut g = db.write().unwrap();
        refused(run(&mut g, "set docs {n: 9} where n = 1"));
        refused(run(&mut g, "del docs where n = 1"));
        run(&mut g, "put notes {n: 1}").unwrap();
        run(&mut g, "del notes where n = 1").unwrap();
        let r = g
            .query(&fennec_ql::parse_one("get docs count").unwrap(), &[])
            .unwrap();
        assert_eq!(r.rows().unwrap().rows[0].values[0], Value::Int(2));
    }
}

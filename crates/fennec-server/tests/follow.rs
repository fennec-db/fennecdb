//! `fennec-server --follow` against a live PostgreSQL (`make import-test`,
//! which starts one in Docker): a table's copy and its changes served over
//! HTTP as they commit, a client's write to the mirror refused, a server
//! stopped -- by a signal, or killed outright -- started again over its
//! file without losing a row, and a follower that cannot go on stopping
//! the server through its shutdown.
//!
//!     cargo test -p fennec-server --test all follow:: -- --ignored
//!
//! And against a stand-in of its own, which every run has: the order a
//! follower's stop keeps -- a failure's exit code before anything waits,
//! a clean stop's slot let go before the exit.

use crate::support::{count, Server};
use fennec_wire::client::{Client, Url};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const URL: &str = "postgres://postgres:fennec@127.0.0.1:55432/fennecbench";

fn source_text() -> String {
    std::env::var("FENNEC_TEST_PG_URL").unwrap_or_else(|_| URL.into())
}

fn pg() -> Client {
    Client::connect(&Url::parse(&source_text()).unwrap())
        .expect("no PostgreSQL to follow (make pgvector-up)")
}

/// A table of `rows` rows, with no slot or publication left from before:
/// the follower names both `fennec_<collection>`. A slot left by a run that
/// ended in a panic -- its server killed, its walsender holding the slot
/// until it reads that -- is dropped once the server has let it go:
/// `DROP_REPLICATION_SLOT ... WAIT`, the server's own wait, bounded by the
/// client's wait for an answer.
fn table(c: &mut Client, t: &str, rows: u64) {
    let left = c
        .query(&format!(
            "select 1 from pg_replication_slots where slot_name = 'fennec_{t}'"
        ))
        .unwrap();
    if !left.rows.is_empty() {
        let q = format!("DROP_REPLICATION_SLOT fennec_{t} WAIT");
        let mut r = Client::connect_replication(&Url::parse(&source_text()).unwrap()).unwrap();
        r.query(&q).unwrap_or_else(|e| panic!("{q}: {e}"));
    }
    for q in [
        format!("drop publication if exists fennec_{t}"),
        format!("drop table if exists {t}"),
        format!("create table {t} (id bigint primary key, title text)"),
        format!("insert into {t} select g, 'row ' || g from generate_series(1, {rows}) g"),
    ] {
        c.query(&q).unwrap_or_else(|e| panic!("{q}: {e}"));
    }
}

/// The slot, publication and table gone, once the server that followed
/// them stopped by a signal or ended on its own: either way its follower
/// ended its stream and let the slot go, so the drop is never refused as
/// "active for PID". A server killed outright cannot; its walsender holds
/// the slot until it reads the connection closed.
fn forget(c: &mut Client, t: &str) {
    for q in [
        format!("select pg_drop_replication_slot('fennec_{t}') from pg_replication_slots where slot_name = 'fennec_{t}'"),
        format!("drop publication if exists fennec_{t}"),
        format!("drop table if exists {t}"),
    ] {
        c.query(&q).unwrap_or_else(|e| panic!("{q}: {e}"));
    }
}

/// Whether the server still has the slot taken by a connection.
fn slot_active(c: &mut Client, t: &str) -> bool {
    let r = c
        .query(&format!(
            "select active from pg_replication_slots where slot_name = 'fennec_{t}'"
        ))
        .unwrap();
    r.rows.first().and_then(|row| row[0].as_deref()) == Some("t")
}

fn rows(s: &Server, t: &str) -> u64 {
    count(&s.http().run(&format!("get {t} count"))) as u64
}

/// Waits until a row of the mirror has `title`.
fn shows(s: &Server, t: &str, title: &str) {
    let q = format!("get {t} where title = \"{title}\" count");
    let deadline = Instant::now() + Duration::from_secs(30);
    while count(&s.http().run(&q)) != 1 {
        assert!(
            Instant::now() < deadline,
            "no row of {t} has title `{title}`"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Waits until the mirror holds `n` rows.
fn holds(s: &Server, t: &str, n: u64) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while rows(s, t) != n {
        assert!(
            Instant::now() < deadline,
            "the mirror of {t} stayed at {}",
            rows(s, t)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn tmp(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("fennec-server-follow-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("mirror.fennec")
}

/// Starts the server over `file`, following `t`, and waits until it streams.
fn start(file: &Path, t: &str) -> Server {
    let source = source_text();
    let s = crate::support::start(&[
        "--file",
        file.to_str().unwrap(),
        "--follow",
        &source,
        "--follow-table",
        t,
    ]);
    assert!(
        s.logged("streaming its changes", Duration::from_secs(60)),
        "fennec-server never streamed:\n{}",
        s.log.lock().unwrap()
    );
    s
}

#[test]
#[ignore = "needs a PostgreSQL to follow: make import-test"]
fn a_followed_table_is_served_and_takes_no_other_write() {
    let t = "fennec_server_follow_served";
    let mut c = pg();
    table(&mut c, t, 50);
    let file = tmp("served");
    let s = start(&file, t);
    assert_eq!(rows(&s, t), 50);

    // A change committed there is served here: each in its own
    // transaction, and applied in their order.
    c.query(&format!("insert into {t} values (51, 'new')"))
        .unwrap();
    c.query(&format!("update {t} set title = 'changed' where id = 7"))
        .unwrap();
    c.query(&format!("delete from {t} where id = 8")).unwrap();
    c.query(&format!("insert into {t} values (52, 'last')"))
        .unwrap();
    shows(&s, t, "last");
    assert_eq!(rows(&s, t), 51);
    let body = s.http().run(&format!("get {t} where id = 7"));
    assert!(body.contains("changed"), "{body}");

    // The mirror is the follower's to write; the rest of the file is not.
    let (status, body) = s
        .http()
        .query(&format!("put {t} {{id: 99, title: \"mine\"}}"))
        .unwrap_err();
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("mirrors a PostgreSQL table"), "{body}");
    let (status, _) = s
        .http()
        .query(&format!("del {t} where id = 1"))
        .unwrap_err();
    assert_eq!(status, 403);
    s.http().run("create collection notes (n int)");
    s.http().run("put notes {n: 1}");
    assert_eq!(rows(&s, t), 51);

    let _ = s.terminate();
    forget(&mut c, t);
}

/// A server that cannot have its port exits before its mirror starts: no
/// slot, no publication, no copy -- each made at every restart its
/// supervisor tried, when the listener was taken after the mirror started.
#[test]
#[ignore = "needs a PostgreSQL to follow: make import-test"]
fn a_server_that_cannot_listen_starts_no_mirror() {
    let t = "fennec_server_follow_unlistened";
    let mut c = pg();
    table(&mut c, t, 10);
    let file = tmp("unlistened");
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = held.local_addr().unwrap().to_string();
    let source = source_text();
    let child = crate::support::command(&[
        "--file",
        file.to_str().unwrap(),
        "--http",
        &addr,
        "--follow",
        &source,
        "--follow-table",
        t,
    ]);
    let (code, log) = crate::support::ended(child, Duration::from_secs(30));
    assert_eq!(code, 1, "{log}");
    assert!(log.contains("could not open the HTTP endpoint"), "{log}");
    for (what, q) in [
        (
            "slot",
            format!("select 1 from pg_replication_slots where slot_name = 'fennec_{t}'"),
        ),
        (
            "publication",
            format!("select 1 from pg_publication where pubname = 'fennec_{t}'"),
        ),
    ] {
        assert!(
            c.query(&q).unwrap().rows.is_empty(),
            "a {what} was made\n{log}"
        );
    }
    forget(&mut c, t);
}

/// A follower that cannot go on stops the server through the one shutdown,
/// exit code 1, rather than exiting from its own thread: what it applied
/// is synced, not cut off with a shutdown it raced.
#[test]
#[ignore = "needs a PostgreSQL to follow: make import-test"]
fn a_follower_that_cannot_go_on_stops_the_server_through_the_shutdown() {
    let t = "fennec_server_follow_broken";
    let mut c = pg();
    table(&mut c, t, 10);
    let file = tmp("broken");
    let mut s = start(&file, t);
    holds(&s, t, 10);
    c.query(&format!("insert into {t} values (11, 'last')"))
        .unwrap();
    shows(&s, t, "last");
    // A column the collection holds, dropped from the table.
    c.query(&format!("alter table {t} drop column title"))
        .unwrap();
    c.query(&format!("insert into {t} values (12)")).unwrap();
    let (code, log) = s
        .exited(Duration::from_secs(60))
        .expect("the server went on serving a mirror that no longer moves");
    assert_eq!(code, 1, "{log}");
    assert!(log.contains("the mirror of"), "{log}");
    assert!(log.contains("shutting down: "), "{log}");
    let db = fennec_core::fs::open_read_only(&file).unwrap();
    let held = db.stats().iter().find(|x| x.name == t).map(|x| x.documents);
    assert_eq!(held, Some(11), "{log}");
    forget(&mut c, t);
}

#[test]
#[ignore = "needs a PostgreSQL to follow: make import-test"]
fn a_server_stopped_or_killed_goes_on_without_losing_a_row() {
    let t = "fennec_server_follow_restart";
    let mut c = pg();
    table(&mut c, t, 20);
    let file = tmp("restart");

    // Stopped by a signal: its last changes on disk and confirmed, its slot
    // let go -- a server started again at once streams from it, never
    // finding it taken -- and the changes committed while it was down taken
    // in when it is back.
    let s = start(&file, t);
    holds(&s, t, 20);
    let _ = s.terminate();
    // The slot is free once the server has exited -- read 100 ms after the
    // exit (`terminate`), from a walsender idle at the stop, which reads a
    // bare `Terminate` at once too: this fails only if the stop leaves the
    // stream open. That the server waits for the release, which a busy
    // walsender delays, is `a_stopped_server_lets_its_slot_go_before_it_
    // exits` (below, against a stand-in), and fennec-import's
    // `a_stopped_follower_lets_its_slot_go` against PostgreSQL.
    assert!(
        !slot_active(&mut c, t),
        "the stopped server's slot is still taken"
    );
    c.query(&format!(
        "insert into {t} select g, 'down ' || g from generate_series(21, 40) g"
    ))
    .unwrap();
    let s = start(&file, t);
    holds(&s, t, 40);

    // Killed outright while the table is written to: what it applied and
    // did not confirm comes again, and changes nothing.
    let written = Arc::new(AtomicU64::new(40));
    let done = Arc::new(AtomicBool::new(false));
    let writer = {
        let (written, done) = (Arc::clone(&written), Arc::clone(&done));
        std::thread::spawn(move || {
            let mut c = pg();
            while !done.load(Ordering::Relaxed) {
                let id = written.load(Ordering::Relaxed) + 1;
                c.query(&format!("insert into {t} values ({id}, 'burst')"))
                    .unwrap();
                written.store(id, Ordering::Relaxed);
            }
        })
    };
    std::thread::sleep(Duration::from_millis(300));
    drop(s);
    std::thread::sleep(Duration::from_millis(300));
    done.store(true, Ordering::Relaxed);
    writer.join().unwrap();
    let s = start(&file, t);
    holds(&s, t, written.load(Ordering::Relaxed));

    let _ = s.terminate();
    forget(&mut c, t);
}

// ------------------------------------------------ against a stand-in

/// What the stand-in PostgreSQL's walsender does once the follower streams.
#[derive(Clone, Copy, PartialEq)]
enum Walsends {
    /// A change the follower cannot read, then nothing: an answer to its
    /// `CopyDone`, had it sent one, would never come.
    Unreadable,
    /// Nothing, until the follower's `CopyDone`, which it answers as
    /// PostgreSQL does once the slot is let go (`released` set first).
    Releases,
}

/// Where the stand-in holds up the copy's connection, its second -- the
/// first is the catalog's -- and holds it for good.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Stall {
    /// It takes the connection and answers nothing: not the TLS request,
    /// not the startup.
    Dial,
    /// It answers the TLS request `S`, and then no byte of a handshake.
    Handshake,
    /// It starts the COPY and sends a byte of a row every half second,
    /// never a line's end: a table that never finishes coming.
    Copy,
}

/// A stand-in PostgreSQL on a port of its own: the catalog a follower asks
/// for, a copy of two rows of `items (id bigint, title text)`, and a
/// walsender doing as `Walsends` says.
struct StandIn {
    url: String,
    released: Arc<AtomicBool>,
    /// The first thing the follower did to end its stream: `c` its
    /// `CopyDone`, `X` its `Terminate`, `0` a close with neither.
    ending: mpsc::Receiver<u8>,
    /// The tags the follower sent once streaming, to its connection's end.
    sent: mpsc::Receiver<Vec<u8>>,
    /// Told once the copy's connection reached its `Stall`.
    stalled: mpsc::Receiver<()>,
}

fn framed(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    v.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    v.extend_from_slice(body);
    v
}

fn message(s: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    s.read_exact(&mut head).ok()?;
    let len = i32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize - 4;
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).ok()?;
    Some((head[0], body))
}

/// A query's answer: its columns, each a name and a type's oid, and rows.
fn answer(columns: &[(&str, i32)], rows: &[&[&str]]) -> Vec<u8> {
    let mut t = (columns.len() as i16).to_be_bytes().to_vec();
    for (name, oid) in columns {
        t.extend_from_slice(name.as_bytes());
        t.push(0);
        t.extend_from_slice(&0i32.to_be_bytes());
        t.extend_from_slice(&0i16.to_be_bytes());
        t.extend_from_slice(&oid.to_be_bytes());
        t.extend_from_slice(&(-1i16).to_be_bytes());
        t.extend_from_slice(&(-1i32).to_be_bytes());
        t.extend_from_slice(&0i16.to_be_bytes());
    }
    let mut out = framed(b'T', &t);
    for row in rows {
        let mut d = (row.len() as i16).to_be_bytes().to_vec();
        for cell in *row {
            d.extend_from_slice(&(cell.len() as i32).to_be_bytes());
            d.extend_from_slice(cell.as_bytes());
        }
        out.extend_from_slice(&framed(b'D', &d));
    }
    out.extend_from_slice(&framed(b'C', b"SELECT\0"));
    out.extend_from_slice(&framed(b'Z', b"I"));
    out
}

/// Reads the rest of a connection, answering nothing: a stall held until
/// the client goes.
fn hold(mut s: TcpStream) {
    let _ = s.read_to_end(&mut Vec::new());
}

/// One connection of the stand-in: a catalog query or the copy, each
/// answered by what it asks; or, in replication mode, the walsender. The
/// copy's connection, the second (`at` 1), stops at `stall` where one is
/// given.
#[allow(clippy::too_many_arguments)]
fn stand_in_connection(
    mut s: TcpStream,
    at: usize,
    walsends: Walsends,
    stall: Option<Stall>,
    released: &AtomicBool,
    ending: &mpsc::Sender<u8>,
    sent: &mpsc::Sender<Vec<u8>>,
    stalled: &mpsc::Sender<()>,
) {
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let stall = stall.filter(|_| at == 1);
    // An `SSLRequest` (8 bytes, its code) or the startup's first 8.
    let mut head = [0u8; 8];
    s.read_exact(&mut head).unwrap();
    let mut len = [head[0], head[1], head[2], head[3]];
    let mut startup = Vec::new();
    if head[4..] == 80_877_103i32.to_be_bytes() {
        match stall {
            Some(Stall::Dial) => {
                let _ = stalled.send(());
                return hold(s);
            }
            Some(Stall::Handshake) => {
                s.write_all(b"S").unwrap();
                let _ = stalled.send(());
                return hold(s);
            }
            _ => s.write_all(b"N").unwrap(),
        }
        s.read_exact(&mut len).unwrap();
    } else {
        startup.extend_from_slice(&head[4..]);
    }
    let mut rest = vec![0u8; i32::from_be_bytes(len) as usize - 4 - startup.len()];
    s.read_exact(&mut rest).unwrap();
    startup.extend_from_slice(&rest);
    if stall == Some(Stall::Dial) {
        let _ = stalled.send(());
        return hold(s);
    }
    let replication = startup.windows(21).any(|w| w == b"replication\0database\0");
    let mut out = framed(b'R', &0i32.to_be_bytes());
    out.extend_from_slice(&framed(b'S', b"server_version\x0017.0\x00"));
    out.extend_from_slice(&framed(b'Z', b"I"));
    s.write_all(&out).unwrap();
    while let Some((tag, body)) = message(&mut s) {
        if tag != b'Q' {
            return;
        }
        let q = String::from_utf8_lossy(&body).to_lowercase();
        let out = if q.starts_with("start_replication") {
            assert!(replication, "{q}");
            break;
        } else if q.starts_with("show wal_level") {
            answer(&[("wal_level", 25)], &[&["logical"]])
        } else if q.contains("::regclass") {
            answer(&[("nspname", 25), ("relname", 25)], &[&["public", "items"]])
        } else if q.contains("from pg_type") {
            answer(&[("typname", 25), ("oid", 26)], &[])
        } else if q.contains("limit 0") {
            answer(&[("id", 20), ("title", 25)], &[])
        } else if q.contains("from pg_publication_tables") {
            answer(&[("?column?", 23)], &[&["1"]])
        } else if q.contains("from pg_publication") {
            answer(
                &[
                    ("pubinsert", 16),
                    ("pubupdate", 16),
                    ("pubdelete", 16),
                    ("pubtruncate", 16),
                ],
                &[&["t", "t", "t", "t"]],
            )
        } else if q.contains("from pg_replication_slots") {
            answer(
                &[("plugin", 25), ("slot_type", 25), ("database", 25)],
                &[&["pgoutput", "logical", "d"]],
            )
        } else if q.starts_with("copy (") && stall == Some(Stall::Copy) {
            // A row begun and never ended: a byte every half second, each
            // its own CopyData, well within any one read's timeout.
            let mut out = framed(b'H', &[0, 0, 2, 0, 0, 0, 0]);
            out.extend_from_slice(&framed(b'd', b"1\tone\n"));
            s.write_all(&out).unwrap();
            let _ = stalled.send(());
            while s.write_all(&framed(b'd', b"x")).is_ok() {
                std::thread::sleep(Duration::from_millis(500));
            }
            return;
        } else if q.starts_with("copy (") {
            let mut out = framed(b'H', &[0, 0, 2, 0, 0, 0, 0]);
            out.extend_from_slice(&framed(b'd', b"1\tone\n2\ttwo\n"));
            out.extend_from_slice(&framed(b'c', b""));
            out.extend_from_slice(&framed(b'C', b"COPY 2\0"));
            out.extend_from_slice(&framed(b'Z', b"I"));
            out
        } else {
            answer(&[], &[])
        };
        s.write_all(&out).unwrap();
    }
    if !replication {
        return;
    }
    // The walsender: the stream started, then what it sends.
    let mut out = framed(b'W', &[0, 0, 0]);
    if walsends == Walsends::Unreadable {
        let mut w = vec![b'w'];
        w.extend_from_slice(&[0u8; 24]);
        w.push(b'Q');
        out.extend_from_slice(&framed(b'd', &w));
    }
    s.write_all(&out).unwrap();
    let mut tags = Vec::new();
    loop {
        let Some((tag, _)) = message(&mut s) else {
            if !tags.iter().any(|t| matches!(t, b'c' | b'X')) {
                let _ = ending.send(0);
            }
            break;
        };
        tags.push(tag);
        if matches!(tag, b'c' | b'X')
            && tags.iter().filter(|t| matches!(t, b'c' | b'X')).count() == 1
        {
            let _ = ending.send(tag);
        }
        match tag {
            b'c' if walsends == Walsends::Releases => {
                released.store(true, Ordering::SeqCst);
                let mut out = framed(b'c', b"");
                out.extend_from_slice(&framed(b'C', b"COPY 0\0"));
                out.extend_from_slice(&framed(b'Z', b"I"));
                s.write_all(&out).unwrap();
            }
            b'X' => break,
            _ => {}
        }
    }
    let _ = sent.send(tags);
}

fn stand_in(walsends: Walsends) -> StandIn {
    stalling(walsends, None, "sslmode=disable")
}

/// [`stand_in`], its copy's connection held up at `stall`; `query` the
/// URL's parameters.
fn stalling(walsends: Walsends, stall: Option<Stall>, query: &str) -> StandIn {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let released = Arc::new(AtomicBool::new(false));
    let (ending_tx, ending) = mpsc::channel();
    let (sent_tx, sent) = mpsc::channel();
    let (stalled_tx, stalled) = mpsc::channel();
    let slot = Arc::clone(&released);
    std::thread::spawn(move || {
        for (at, s) in listener.incoming().enumerate() {
            let Ok(s) = s else { return };
            let (slot, ending_tx, sent_tx, stalled_tx) = (
                Arc::clone(&slot),
                ending_tx.clone(),
                sent_tx.clone(),
                stalled_tx.clone(),
            );
            std::thread::spawn(move || {
                stand_in_connection(
                    s,
                    at,
                    walsends,
                    stall,
                    &slot,
                    &ending_tx,
                    &sent_tx,
                    &stalled_tx,
                )
            });
        }
    });
    StandIn {
        url: format!("postgres://u@127.0.0.1:{port}/d?{query}"),
        released,
        ending,
        sent,
        stalled,
    }
}

/// A server following `items` from `p`, into a file of its own.
fn follow_stand_in(p: &StandIn, tag: &str) -> Server {
    let file = tmp(tag);
    crate::support::start(&[
        "--file",
        file.to_str().unwrap(),
        "--follow",
        &p.url,
        "--follow-table",
        "items",
    ])
}

/// A follower that fails at what it read stores its exit code before
/// anything of its stop waits: it closes its stream, never asking the
/// server to end it, and a SIGTERM that comes as it does -- the shutdown
/// then waiting on the follower -- still exits 1, with no checkpoint. Its
/// stream once waited for the server's end of it, here never coming, past
/// the shutdown's wait, and the server exited 0 serving a mirror that had
/// stopped moving all the while.
#[test]
fn a_follower_that_fails_stops_the_server_with_its_code_before_it_waits() {
    let p = stand_in(Walsends::Unreadable);
    let s = follow_stand_in(&p, "standin-fails");
    let first = p
        .ending
        .recv_timeout(Duration::from_secs(60))
        .expect("the follower never ended its stream");
    let (code, log) = s.terminate();
    assert_eq!(code, 1, "{log}");
    assert!(log.contains("the mirror of items stopped"), "{log}");
    let sent = p.sent.recv_timeout(Duration::from_secs(60)).unwrap();
    assert_ne!(
        first, b'c',
        "the failed follower waited for the server: {sent:?}"
    );
    assert!(
        !sent.contains(&b'c'),
        "the failed follower waited for the server: {sent:?}"
    );
}

/// A server stopped by a signal ends its follower's stream as the protocol
/// does and exits only once the server answered, which PostgreSQL does
/// once the slot is let go: a server started again at once, or a drop of
/// the slot, finds it free. A `Terminate` alone left the slot held for as
/// long as the walsender did not read, which `a_server_stopped_or_killed_
/// goes_on_without_losing_a_row`, its walsender idle, cannot show.
#[test]
fn a_stopped_server_lets_its_slot_go_before_it_exits() {
    let p = stand_in(Walsends::Releases);
    let s = follow_stand_in(&p, "standin-stops");
    assert!(
        s.logged("streaming its changes", Duration::from_secs(60)),
        "{}",
        s.log.lock().unwrap()
    );
    let (code, log) = s.terminate();
    assert_eq!(code, 0, "{log}");
    assert!(
        p.released.load(Ordering::SeqCst),
        "the server exited before the slot was let go\n{log}"
    );
    assert_eq!(
        p.sent.recv_timeout(Duration::from_secs(60)).unwrap(),
        b"cX",
        "{log}"
    );
    // Read through the shutdown's own line, which comes after any of the
    // follower's: neither line saying the slot is or may be held was
    // written -- "still held at the stop", "may still be held at the stop".
    assert!(log.contains("shutting down: "), "{log}");
    assert!(!log.contains("held at the stop"), "{log}");
}

/// A first copy that PostgreSQL -- or whoever answers in its place -- holds
/// up holds up nothing of the server's: not a read of another collection,
/// nor a write to one, nor the shutdown, which ends within its wait and
/// leaves the copy unfinished for the next start. The copy's dial, its
/// sign-in and its rows each held the server's write lock: a request of
/// any collection waited on PostgreSQL, a stall sending a byte a little
/// under every 30 s held it for good, and a SIGTERM never ended the
/// server, its shutdown waiting for the same lock. The three places a
/// stand-in stalls: the dial, answered by nothing; the TLS handshake,
/// begun and never answered; the COPY, a byte every half second.
#[test]
fn a_copy_held_up_holds_up_no_request_nor_the_shutdown() {
    for (stall, query) in [
        (Stall::Dial, "sslmode=disable"),
        (Stall::Handshake, "sslmode=prefer"),
        (Stall::Copy, "sslmode=disable"),
    ] {
        let p = stalling(Walsends::Releases, Some(stall), query);
        let file = tmp(&format!("standin-stall-{stall:?}"));
        let s = crate::support::start(&[
            "--file",
            file.to_str().unwrap(),
            "--follow",
            &p.url,
            "--follow-table",
            "items",
        ]);
        p.stalled
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("{stall:?}: the copy never reached its stall"));
        // A request of another collection, a write and a read, each
        // answered while the copy waits: bounded by what it stands
        // against, a wait for good.
        let (tx, rx) = mpsc::channel();
        let port = s.port;
        std::thread::spawn(move || {
            let mut h = crate::support::Http::open(port);
            let w = h.query("create collection notes (title text)");
            let r = h.query(r#"put notes {title: "one"}"#);
            let g = h.query("get notes");
            let _ = tx.send((w, r, g));
        });
        let (w, r, g) = rx
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("{stall:?}: a request waited on the copy"));
        assert!(w.is_ok() && r.is_ok(), "{stall:?}: {w:?} {r:?}");
        assert!(g.unwrap().contains("\"one\""), "{stall:?}");
        // The shutdown ends within its wait, 5 s, the copy left unfinished.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(s.terminate());
        });
        let (code, log) = rx
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("{stall:?}: the shutdown waited on the copy"));
        assert_eq!(code, 0, "{stall:?}: {log}");
        assert!(
            !log.contains("the mirror of items stopped"),
            "{stall:?}: {log}"
        );
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }
}

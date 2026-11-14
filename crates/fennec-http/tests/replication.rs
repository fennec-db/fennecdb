//! Replication end to end: a primary and its replicas as servers in this
//! process, each over its own file, talking over real sockets.

use fennec_core::prelude::*;
use fennec_http::replication::{self, fresh_id, Follower, Replication};
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const TOKEN: &str = "replication-secret";

struct Node {
    port: u16,
    db: Arc<RwLock<Database>>,
    follower: Option<Arc<Follower>>,
    server: Arc<Server>,
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fennec-repl-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn serve(db: Arc<RwLock<Database>>, repl: Arc<Replication>) -> (u16, Arc<Server>) {
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        // Every write is fsynced before it is answered: what a replica is
        // sent is what an fsync covered.
        sync_on_write: true,
        ..Config::default()
    };
    let server = Arc::new(Server::new(db, cfg).with_replication(repl));
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    let s = Arc::clone(&server);
    std::thread::spawn(move || {
        let _ = s.serve_on(listener);
    });
    (port, server)
}

/// A primary over `file`, as `fennec-server --replication-token` starts one.
fn primary(file: &Path, buffer: usize) -> Node {
    let (mut db, feed) = replication::open(file.to_str().unwrap(), buffer).unwrap();
    if db.history().following {
        db.fork(fresh_id()).unwrap();
    }
    if db.history().lineage.is_empty() {
        db.fork(fresh_id()).unwrap();
    }
    let db = Arc::new(RwLock::new(db));
    let (port, server) = serve(
        Arc::clone(&db),
        Replication::new(Some(TOKEN.into()), Some(feed), None),
    );
    Node {
        port,
        db,
        follower: None,
        server,
    }
}

/// A replica over `file` following the server on `upstream`, and feeding
/// replicas of its own.
fn replica(file: &Path, upstream: u16) -> Node {
    let (mut db, feed) =
        replication::open(file.to_str().unwrap(), replication::DEFAULT_BUFFER).unwrap();
    // As `fennec-server --replica-of` does: no write of its own from the start,
    // before the primary has said a word.
    let lineage = db.history().lineage.clone();
    db.follow(lineage).unwrap();
    let db = Arc::new(RwLock::new(db));
    let follower = Follower::new(
        &format!("http://127.0.0.1:{upstream}"),
        TOKEN.into(),
        Arc::clone(&db),
        Some(Arc::clone(&feed)),
        true,
    )
    .unwrap();
    let f = Arc::clone(&follower);
    std::thread::spawn(move || f.run());
    let (port, server) = serve(
        Arc::clone(&db),
        Replication::new(Some(TOKEN.into()), Some(feed), Some(Arc::clone(&follower))),
    );
    Node {
        port,
        db,
        follower: Some(follower),
        server,
    }
}

fn http(port: u16, method: &str, path: &str, token: Option<&str>, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out[9..12].parse().unwrap();
    let body = out
        .split_once("\r\n\r\n")
        .map_or("", |(_, b)| b)
        .to_string();
    (status, body)
}

fn query(node: &Node, sql: &str) -> (u16, String) {
    let mut body = String::from("{\"query\":");
    fennec_core::json::escape_into(&mut body, sql);
    body.push('}');
    http(node.port, "POST", "/query", None, &body)
}

fn seq(node: &Node) -> u64 {
    node.db.read().unwrap().change_seq()
}

fn rows(node: &Node, sql: &str) -> Vec<(u64, Vec<Value>)> {
    let g = node.db.read().unwrap();
    let r = g.query(&fennec_ql::parse_one(sql).unwrap(), &[]).unwrap();
    r.rows()
        .unwrap()
        .rows
        .iter()
        .map(|r| (r.id, r.values.clone()))
        .collect()
}

fn caught_up(replica: &Node, primary: &Node) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while seq(replica) != seq(primary) {
        assert!(
            Instant::now() < deadline,
            "the replica stayed at {} while the primary is at {}",
            seq(replica),
            seq(primary)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// `f` halted, as `Follower::halt` halts it, the wait for its thread
/// bounded: a follower that read on past its halt's cut held the halt, and
/// Windows CI's job with it, for hours, where it now fails here.
fn halt(f: &Arc<Follower>) {
    let (halted, done) = std::sync::mpsc::channel();
    let f = Arc::clone(f);
    std::thread::spawn(move || {
        f.halt();
        let _ = halted.send(());
    });
    done.recv_timeout(Duration::from_secs(20))
        .expect("the halt waited 20 s for the follower's thread, which never ended");
}

fn write_some(node: &Node, from: usize, n: usize) {
    for i in from..from + n {
        let (status, body) = query(
            node,
            &format!("put items {{name: \"item {i}\", n: {i}, e: [{i}.0, 1.0, 0.5]}}"),
        );
        assert_eq!(status, 200, "{body}");
    }
}

const SCHEMA: &str =
    "create collection items (name text @text, n int @sorted, e vector<3> @hnsw(cosine))";

/// A primary that died in the middle of an append left its last record
/// cut short. It is cut off the file on the way back up -- no replica was
/// sent it, since no fsync covered it -- rather than read back later with
/// the first write after the restart as the rest of it.
#[test]
fn a_record_a_crash_cut_short_is_cut_off_before_the_next_write() {
    let d = dir("torn");
    let file = d.join("p.fennec");
    let path = file.to_str().unwrap();
    let run = |db: &mut Database, sql: &str| {
        db.execute(&fennec_ql::parse_one(sql).unwrap()).unwrap();
    };
    {
        let (mut db, _) = replication::open(path, replication::DEFAULT_BUFFER).unwrap();
        run(&mut db, "create collection t (x int)");
        run(&mut db, "put t {x: 1}");
        db.sync().unwrap();
    }
    let whole = std::fs::metadata(&file).unwrap().len();
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap();
    f.write_all(&[3, 1, 100, 0, 1, 2]).unwrap();
    drop(f);
    {
        let (mut db, _) = replication::open(path, replication::DEFAULT_BUFFER).unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().len(), whole);
        run(&mut db, "put t {x: 2}");
        db.sync().unwrap();
    }
    let (db, _) = replication::open(path, replication::DEFAULT_BUFFER).unwrap();
    let r = db
        .query(&fennec_ql::parse_one("get t count").unwrap(), &[])
        .unwrap();
    assert_eq!(r.rows().unwrap().rows[0].values[0], Value::Int(2));
}

/// A primary's writes made durable through the sync log (`fennec_core::fs`,
/// Linux's, kept here by the test thread) -- through its `Tee`, which sends
/// a write once its durability ran -- are its file's again after the
/// machine lost the file's tail, and a replica started after is sent every
/// one of them. Windows keeps no sync log (docs/design/windows.md, 3): a
/// durability flushes the file itself, so the machine loses none of what
/// one covered, and the primary comes back over the file as it was.
#[test]
fn a_primary_back_from_a_power_loss_holds_what_its_log_made_durable() {
    let d = dir("power");
    let file = d.join("p.fennec");
    let path = file.to_str().unwrap();
    #[cfg(unix)]
    fennec_core::fs::keep_sync_log(true);
    let (mut db, feed) = replication::open(path, replication::DEFAULT_BUFFER).unwrap();
    #[cfg(unix)]
    fennec_core::fs::keep_sync_log(false);
    db.fork(fresh_id()).unwrap();
    let durable = |db: &mut Database, sql: &str| {
        db.execute(&fennec_ql::parse_one(sql).unwrap()).unwrap();
        if let Some(d) = db.flush().unwrap() {
            d().unwrap();
        }
    };
    durable(&mut db, SCHEMA);
    for i in 0..30 {
        durable(
            &mut db,
            &format!("put items {{name: \"item {i}\", n: {i}, e: [{i}.0, 1.0, 0.5]}}"),
        );
    }
    let at_crash = db.change_seq();
    std::mem::forget(db);
    std::mem::forget(feed);
    #[cfg(unix)]
    {
        let log = std::fs::read(fennec_core::fs::beside(&file, "sync")).unwrap();
        assert_eq!(&log[..8], b"FENECSYN");
        let synced = u64::from_le_bytes(log[24..32].try_into().unwrap());
        let f = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        f.set_len(synced).unwrap();
    }

    let p = primary(&file, replication::DEFAULT_BUFFER);
    assert_eq!(seq(&p), at_crash);
    assert_eq!(rows(&p, "get items").len(), 30);
    let r = replica(&d.join("r.fennec"), p.port);
    caught_up(&r, &p);
    assert_eq!(rows(&r, "get items"), rows(&p, "get items"));
}

#[test]
fn a_replica_follows_and_a_restarted_one_goes_on_from_where_it_was() {
    let d = dir("follow");
    let p = primary(&d.join("p.fennec"), replication::DEFAULT_BUFFER);
    assert_eq!(query(&p, SCHEMA).0, 200);
    write_some(&p, 0, 20);

    let r = replica(&d.join("r.fennec"), p.port);
    caught_up(&r, &p);
    // It started empty, and an empty database continues anything.
    assert_eq!(r.follower.as_ref().unwrap().images(), 0);
    write_some(&p, 20, 30);
    assert_eq!(
        query(&p, "set items {name: \"renamed\"} where n < 5").0,
        200
    );
    assert_eq!(query(&p, "del items where n >= 45").0, 200);
    caught_up(&r, &p);
    for sql in [
        "get items",
        r#"get items select id match name "renamed""#,
        "get items select id, n where n > 10 order n desc limit 4",
        "get items select id near e [3.0, 1.0, 0.5] exact limit 3",
    ] {
        assert_eq!(rows(&r, sql), rows(&p, sql), "{sql}");
    }
    assert!(r.db.read().unwrap().history().following);
    assert_eq!(
        r.db.read().unwrap().history().lineage,
        p.db.read().unwrap().history().lineage
    );

    // A write sent to the replica is refused, as a standby refuses one.
    let (status, body) = query(&r, "put items {name: \"no\"}");
    assert_eq!(status, 403, "{body}");

    // Stopped and started again over its file, it asks for what it
    // missed rather than for an image. The file is looked at before the
    // follower starts: read after, the follower had often fetched the
    // writes it missed already.
    halt(r.follower.as_ref().unwrap());
    let at = seq(&r);
    write_some(&p, 50, 10);
    drop(r);
    let file = d.join("r.fennec");
    let (db, _) = replication::open(file.to_str().unwrap(), replication::DEFAULT_BUFFER).unwrap();
    assert_eq!(db.change_seq(), at);
    drop(db);
    let r = replica(&file, p.port);
    caught_up(&r, &p);
    assert_eq!(r.follower.as_ref().unwrap().images(), 0);
    assert_eq!(rows(&r, "get items"), rows(&p, "get items"));
}

/// A primary's `compact` writes its file beside the database, through the
/// feed as a server without replicas does, and the writes made while it
/// does reach the replica as any others: numbered on, and in the new file
/// when the primary opens it again.
#[test]
fn a_primary_compacts_beside_the_database_while_a_replica_follows() {
    let d = dir("compact");
    let file = d.join("p.fennec");
    let p = primary(&file, replication::DEFAULT_BUFFER);
    assert_eq!(query(&p, SCHEMA).0, 200);
    write_some(&p, 0, 40);
    assert_eq!(query(&p, "del items where n < 10").0, 200);
    let r = replica(&d.join("r.fennec"), p.port);
    caught_up(&r, &p);

    let compact = fennec_ql::parse_one("compact").unwrap();
    let mut calls = 0;
    Database::maintain_with(&p.db, &compact, &mut || {
        calls += 1;
        // The second copy is the file's: its side file is being written.
        assert_eq!(file.with_extension("fennec.beside").exists(), calls == 2);
        write_some(&p, 100 * calls, 5);
        assert_eq!(
            query(&p, &format!("del items where n = {}", 10 + calls)).0,
            200
        );
    })
    .unwrap()
    .unwrap();
    assert_eq!(calls, 2);
    assert!(!file.with_extension("fennec.beside").exists());
    write_some(&p, 300, 5);
    caught_up(&r, &p);
    assert_eq!(r.follower.as_ref().unwrap().images(), 0);
    for sql in [
        "get items",
        "get items select id near e [3.0, 1.0, 0.5] exact limit 3",
    ] {
        assert_eq!(rows(&r, sql), rows(&p, sql), "{sql}");
    }

    let (seq_was, rows_were) = (seq(&p), rows(&p, "get items"));
    halt(r.follower.as_ref().unwrap());
    drop(p);
    let back = fennec_core::fs::open(&file).unwrap();
    assert_eq!(back.change_seq(), seq_was);
    let g = back
        .query(&fennec_ql::parse_one("get items").unwrap(), &[])
        .unwrap();
    let got: Vec<(u64, Vec<Value>)> = g
        .rows()
        .unwrap()
        .rows
        .iter()
        .map(|r| (r.id, r.values.clone()))
        .collect();
    assert_eq!(got, rows_were);
}

#[test]
fn a_replica_behind_the_buffer_is_sent_an_image() {
    let d = dir("image");
    // A buffer this small keeps only the last few writes.
    let p = primary(&d.join("p.fennec"), 256);
    assert_eq!(query(&p, SCHEMA).0, 200);
    write_some(&p, 0, 40);

    let r = replica(&d.join("r.fennec"), p.port);
    caught_up(&r, &p);
    assert_eq!(r.follower.as_ref().unwrap().images(), 1);
    // After the image it follows write by write.
    write_some(&p, 40, 5);
    caught_up(&r, &p);
    assert_eq!(r.follower.as_ref().unwrap().images(), 1);
    assert_eq!(rows(&r, "get items"), rows(&p, "get items"));
    let near = "get items select id near e [7.0, 1.0, 0.5] exact limit 3";
    assert_eq!(rows(&r, near), rows(&p, near));
}

/// An image a copy took says on disk that the copy follows. The image's
/// own history is its primary's, taking writes, and the record making it a
/// follower's, appended after the rename, waited for a write behind that a
/// copy with nothing to apply never makes: a kill then, and the restart
/// opened the copy as a primary's file, following nothing.
#[test]
fn an_image_a_copy_took_says_on_disk_that_it_follows() {
    let d = dir("image-follows");
    let p = primary(&d.join("p.fennec"), 256);
    assert_eq!(query(&p, SCHEMA).0, 200);
    write_some(&p, 0, 40);
    let r = replica(&d.join("r.fennec"), p.port);
    caught_up(&r, &p);
    assert_eq!(r.follower.as_ref().unwrap().images(), 1);
    // What a kill leaves: the file as the system holds it, not what the
    // process kept to write.
    let killed = d.join("killed.fennec");
    std::fs::write(&killed, std::fs::read(d.join("r.fennec")).unwrap()).unwrap();
    let (db, _feed) =
        replication::open(killed.to_str().unwrap(), replication::DEFAULT_BUFFER).unwrap();
    assert!(
        db.history().following,
        "the image's own history, a primary's"
    );
    assert_eq!(db.history().lineage, r.db.read().unwrap().history().lineage);
    assert_eq!(db.change_seq(), seq(&p));
}

#[test]
fn a_replica_of_a_replica_is_fed_the_same_writes() {
    let d = dir("chain");
    let p = primary(&d.join("p.fennec"), replication::DEFAULT_BUFFER);
    assert_eq!(query(&p, SCHEMA).0, 200);
    let a = replica(&d.join("a.fennec"), p.port);
    let b = replica(&d.join("b.fennec"), a.port);
    write_some(&p, 0, 25);
    caught_up(&a, &p);
    caught_up(&b, &p);
    assert_eq!(rows(&b, "get items"), rows(&p, "get items"));
}

#[test]
fn a_promoted_replica_forks_and_the_old_primary_rejoins_under_it() {
    let d = dir("promote");
    let p = primary(&d.join("p.fennec"), replication::DEFAULT_BUFFER);
    assert_eq!(query(&p, SCHEMA).0, 200);
    write_some(&p, 0, 10);
    let r = replica(&d.join("r.fennec"), p.port);
    caught_up(&r, &p);

    // The token guards the replication endpoints.
    assert_eq!(
        http(r.port, "POST", "/_replication/promote", None, "").0,
        401
    );
    assert_eq!(
        http(r.port, "POST", "/_replication/promote", Some("wrong"), "").0,
        401
    );
    let (status, body) = http(r.port, "POST", "/_replication/promote", Some(TOKEN), "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"promoted\":true"), "{body}");
    let promoted_at = seq(&r);
    assert!(!r.db.read().unwrap().history().following);
    assert_eq!(query(&r, "put items {name: \"after\", n: 100}").0, 200);

    // The old primary wrote on after the replica left it: its history went
    // past the fork, so it cannot be continued and takes an image.
    write_some(&p, 10, 3);
    assert!(seq(&p) > promoted_at);
    let old = p.db.read().unwrap().history().clone();
    let at = seq(&p);
    drop(p);
    let back = replica(&d.join("p.fennec"), r.port);
    caught_up(&back, &r);
    assert_eq!(back.follower.as_ref().unwrap().images(), 1);
    assert_eq!(rows(&back, "get items"), rows(&r, "get items"));
    assert_ne!(back.db.read().unwrap().history().lineage, old.lineage);
    assert!(rows(&back, "get items where n >= 10 and n < 13").is_empty());
    let _ = at;

    // Its status names what it follows.
    let (status, body) = http(back.port, "GET", "/_replication/status", Some(TOKEN), "");
    assert_eq!(status, 200);
    assert!(body.contains("\"role\":\"replica\""), "{body}");
    assert!(body.contains("\"connected\":true"), "{body}");
    let (_, body) = http(r.port, "GET", "/_replication/status", Some(TOKEN), "");
    assert!(body.contains("\"role\":\"primary\""), "{body}");
}

#[test]
fn a_replica_on_the_forked_side_of_nothing_goes_on_without_an_image() {
    let d = dir("prefix");
    let p = primary(&d.join("p.fennec"), replication::DEFAULT_BUFFER);
    assert_eq!(query(&p, SCHEMA).0, 200);
    write_some(&p, 0, 10);
    let a = replica(&d.join("a.fennec"), p.port);
    let b = replica(&d.join("b.fennec"), p.port);
    caught_up(&a, &p);
    caught_up(&b, &p);

    // The primary goes; `a` is promoted and `b` pointed at it. `b` holds
    // nothing `a` does not, so it goes on from where it was.
    halt(b.follower.as_ref().unwrap());
    drop(b);
    drop(p);
    let (status, _) = http(a.port, "POST", "/_replication/promote", Some(TOKEN), "");
    assert_eq!(status, 200);
    write_some(&a, 10, 5);
    let b = replica(&d.join("b.fennec"), a.port);
    caught_up(&b, &a);
    assert_eq!(b.follower.as_ref().unwrap().images(), 0);
    assert_eq!(rows(&b, "get items"), rows(&a, "get items"));
    assert_eq!(
        b.db.read().unwrap().history().lineage,
        a.db.read().unwrap().history().lineage
    );
}

/// A request as written, and the answer's status, head and body.
fn raw(port: u16, method: &str, path: &str, headers: &str, body: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap();
    (
        head[9..12].parse().unwrap(),
        head.to_string(),
        body.to_string(),
    )
}

/// A client that wrote on the primary reads its write on a replica: the
/// write's answer names the change it made (`Fennec-Seq`), and a read sent
/// with `Fennec-After` waits until the replica holds it -- never answered
/// from before it.
#[test]
fn a_read_sent_after_a_write_waits_for_it_on_a_replica() {
    let d = dir("ryw");
    let p = primary(&d.join("p.fennec"), replication::DEFAULT_BUFFER);
    let r = replica(&d.join("r.fennec"), p.port);
    query(&p, SCHEMA);
    caught_up(&r, &p);
    for i in 0..50 {
        let put = format!(
            "{{\"query\":\"put items {{name: \\\"ryw {i}\\\", n: {i}, e: [1.0, 0.5, {i}.0]}}\"}}"
        );
        let (status, head, body) = raw(p.port, "POST", "/query", "", &put);
        assert_eq!(status, 200, "{body}");
        let seq: u64 = head
            .lines()
            .find_map(|l| l.strip_prefix("Fennec-Seq: "))
            .expect("a write names its change")
            .trim()
            .parse()
            .unwrap();
        let get = format!("{{\"query\":\"get items where n = {i}\"}}");
        let (status, _, body) = raw(
            r.port,
            "POST",
            "/query",
            &format!("Fennec-After: {seq}\r\n"),
            &get,
        );
        assert_eq!(status, 200, "{body}");
        assert!(
            body.contains(&format!("ryw {i}")),
            "read {i} on the replica missed its write: {body}"
        );
    }
    // A replica held back: without the header a read is answered from
    // where it stands, and with it waits until the write is there.
    let f = r.follower.as_ref().unwrap();
    halt(f);
    let put = "{\"query\":\"put items {name: \\\"held back\\\", n: 999, e: [1.0, 0.5, 2.0]}\"}";
    let (_, head, _) = raw(p.port, "POST", "/query", "", put);
    let written: u64 = head
        .lines()
        .find_map(|l| l.strip_prefix("Fennec-Seq: "))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let get = "{\"query\":\"get items where n = 999\"}";
    let (_, _, body) = raw(r.port, "POST", "/query", "", get);
    assert!(!body.contains("held back"), "{body}");
    let port = r.port;
    // The follower starts 300 ms after the reader says it is sending, not
    // after it was spawned: timed from its own start, a reader the system
    // started 50 ms late was answered sooner than the bound. The read waits
    // 15 s for the write, not the 5 a header alone gives: a replica starved
    // at background QoS beside a dozen other tests' loops applied it after
    // those, a 504 in 1 run of 100.
    let (sending, sent) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let started = Instant::now();
        sending.send(()).unwrap();
        let answer = raw(
            port,
            "POST",
            "/query",
            &format!("Fennec-After: {written}\r\nFennec-Wait: 15000\r\n"),
            get,
        );
        (started.elapsed(), answer)
    });
    sent.recv().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    f.start("replica".into()).unwrap();
    let (took, (status, _, body)) = reader.join().unwrap();
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("held back"), "{body}");
    assert!(took >= Duration::from_millis(300), "{took:?}");
    // A change that has not come: the wait, then 504 and where it stands.
    let started = Instant::now();
    let (status, _, body) = raw(
        r.port,
        "POST",
        "/query",
        &format!("Fennec-After: {}\r\nFennec-Wait: 300\r\n", seq(&p) + 1000),
        "{\"query\":\"get items count\"}",
    );
    assert_eq!(status, 504, "{body}");
    assert!(started.elapsed() >= Duration::from_millis(250));
    assert!(body.contains("\"seq\":"), "{body}");
    let (status, _, _) = raw(
        r.port,
        "POST",
        "/query",
        "Fennec-After: soon\r\n",
        "{\"query\":\"get items count\"}",
    );
    assert_eq!(status, 400);
}

/// A subscription's first event, read on a connection of its own -- or,
/// refused, its whole answer: the status and that text. Bounded as a whole,
/// not a read at a time: a stream sending its keep-alives and no event never
/// lets a read run out, and was read for as long as it stayed open.
fn first_event(port: u16, path: &str, headers: &str) -> (u16, String) {
    const WITHIN: Duration = Duration::from_secs(30);
    let deadline = Instant::now() + WITHIN;
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    // Closed after an answer that is not a stream, which a stream is anyway.
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\n{headers}Connection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some((head, body)) = out.split_once("\r\n\r\n") {
            let blocks: Vec<&str> = body.split("\n\n").collect();
            // Every block but the last is whole; a keep-alive is a comment.
            let whole = &blocks[..blocks.len() - 1];
            if let Some(event) = whole.iter().find(|b| b.trim_start().starts_with("event:")) {
                return (head[9..12].parse().unwrap(), event.to_string());
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "{path}: no event and no end within {WITHIN:?}, only {out:?}"
        );
        s.set_read_timeout(Some(left)).unwrap();
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => out.push_str(&String::from_utf8_lossy(&chunk[..n])),
        }
    }
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    let status = head.get(9..12).and_then(|s| s.parse().ok());
    let status =
        status.unwrap_or_else(|| panic!("{path}: no answer within {WITHIN:?}, only {out:?}"));
    (status, body.to_string())
}

/// A put of a row named `name` on `node`: the change it left it at.
fn put_named(node: &Node, name: &str) -> u64 {
    let mut put = String::from("{\"query\":");
    fennec_core::json::escape_into(
        &mut put,
        &format!("put items {{name: \"{name}\", n: 500, e: [1.0, 0.5, 2.0]}}"),
    );
    put.push('}');
    let (status, head, body) = raw(node.port, "POST", "/query", "", &put);
    assert_eq!(status, 200, "{body}");
    head.lines()
        .find_map(|l| l.strip_prefix("Fennec-Seq: "))
        .expect("a write names its change")
        .trim()
        .parse()
        .unwrap()
}

/// A subscription and `/_changes` sent after a write their client made on
/// the primary (`Fennec-After`) wait for it on a replica as a read does --
/// before the stream's head and before the page -- and are answered with it
/// once it lands, or 504 with where the replica stands at the end of the
/// wait. Served at once, a stream on a replica behind seeded from before
/// the client's write, a stream resumed with `since` was sent nothing of
/// it, and `/_changes` answered a page ending before it with a `Fennec-Seq`
/// behind it too. Each is seen waiting on the replica before the follower
/// runs again: one served at once never is, and the test fails there
/// rather than pass on a write that happened to land first.
#[test]
fn a_stream_and_changes_sent_after_a_write_wait_for_it_on_a_replica() {
    let d = dir("streams-after");
    let p = primary(&d.join("p.fennec"), replication::DEFAULT_BUFFER);
    let r = replica(&d.join("r.fennec"), p.port);
    query(&p, SCHEMA);
    write_some(&p, 0, 3);
    caught_up(&r, &p);
    let cursor = seq(&r);
    let f = r.follower.as_ref().unwrap();
    let cases = [
        (
            "a stream",
            "/items/changes?n=gte.500".to_string(),
            "event: seed",
        ),
        (
            "a resumed stream",
            format!("/items/changes?since={cursor}"),
            "event: change",
        ),
        (
            "/_changes",
            format!("/_changes?since={cursor}&wait=15000"),
            "\"op\":\"put\"",
        ),
    ];
    for (i, (what, target, kind)) in cases.into_iter().enumerate() {
        halt(f);
        let name = format!("held back {i}");
        let written = put_named(&p, &name);
        assert!(
            seq(&r) < written,
            "{what}: the replica holds the write already"
        );
        let port = r.port;
        let headers = format!("Fennec-After: {written}\r\nFennec-Wait: 20000\r\n");
        let changes = target.starts_with("/_changes");
        let asking = std::thread::spawn(move || match changes {
            // The page waits for the write to be here, and its `Fennec-Seq`
            // says it is: behind it, a consumer read the page as every
            // write there is. A write not on the replica's disk yet comes
            // on a page after, as any write does -- only what an fsync
            // covered is handed over.
            true => {
                let number = |head: &str, name: &str| {
                    head.lines()
                        .find_map(|l| l.strip_prefix(name))
                        .map(|v| v.trim().parse::<u64>().unwrap())
                };
                let (status, head, mut body) = raw(port, "GET", &target, &headers, "");
                if status != 200 {
                    return (status, body);
                }
                let stands = number(&head, "Fennec-Seq: ").unwrap();
                assert!(stands >= written, "a page at {stands}, after {written}");
                let mut next = number(&head, "Fennec-Next: ").unwrap();
                let deadline = Instant::now() + Duration::from_secs(20);
                while next < written {
                    assert!(Instant::now() < deadline, "the write never came: {body}");
                    let more = format!("/_changes?since={next}&wait=15000");
                    let (_, head, page) = raw(port, "GET", &more, "", "");
                    body.push_str(&page);
                    next = number(&head, "Fennec-Next: ").unwrap();
                }
                (status, body)
            }
            false => first_event(port, &target, &headers),
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        while r.server.after_waiting() == 0 {
            assert!(
                Instant::now() < deadline && !asking.is_finished(),
                "{what} never waited for the write on the replica"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        f.start("replica".into()).unwrap();
        let (status, body) = asking.join().unwrap();
        assert_eq!(status, 200, "{what}: {body}");
        assert!(body.contains(kind), "{what}: {body}");
        assert!(
            body.contains(&name),
            "{what} was served without its write: {body}"
        );
    }
    caught_up(&r, &p);

    // A change that has not come: the wait, then 504 and where the replica
    // stands, no head of a stream before it; a header it cannot read, 400.
    let far = seq(&p) + 1000;
    let stands = format!("\"seq\":{}", seq(&r));
    for target in ["/items/changes", "/_changes?since=1"] {
        let started = Instant::now();
        let (status, body) = first_event(
            r.port,
            target,
            &format!("Fennec-After: {far}\r\nFennec-Wait: 300\r\n"),
        );
        assert_eq!(status, 504, "{target}: {body}");
        assert!(started.elapsed() >= Duration::from_millis(250), "{target}");
        assert!(body.contains(&stands), "{target}: {body}");
        let (status, body) = first_event(r.port, target, "Fennec-After: soon\r\n");
        assert_eq!(status, 400, "{target}: {body}");
    }
    assert_eq!(r.server.after_waiting(), 0);
}

/// A subscription's head, read on a connection of its own.
fn stream_head(port: u16, path: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let mut out = String::new();
    let mut chunk = [0u8; 4096];
    while !out.contains("\r\n\r\n") {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => out.push_str(&String::from_utf8_lossy(&chunk[..n])),
        }
    }
    out.split("\r\n\r\n").next().unwrap().to_string()
}

/// A header of `head`, by its name.
fn header_of<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// A stream's head names the change the database stood at as it opened
/// (`Fennec-Seq`) and the history that change and every event's `seq` are
/// of (`Fennec-History`), as a write's answer does: a client running reads
/// after a change the stream names -- near, on a read replica, while the
/// stream stays home -- sends them after that change of that history,
/// which a number alone does not name across a failover. A replica's names
/// its primary's history.
#[test]
fn a_stream_s_head_names_the_history_of_its_changes() {
    let d = dir("stream-history");
    let p = primary(&d.join("p.fennec"), replication::DEFAULT_BUFFER);
    let r = replica(&d.join("r.fennec"), p.port);
    query(&p, SCHEMA);
    let put = "{\"query\":\"put items {name: \\\"x\\\", n: 1, e: [1.0, 0.5, 2.0]}\"}";
    let (status, written, body) = raw(p.port, "POST", "/query", "", put);
    assert_eq!(status, 200, "{body}");
    let history = header_of(&written, "Fennec-History").expect("a write names its history");
    let seq = header_of(&written, "Fennec-Seq").unwrap();
    caught_up(&r, &p);
    for node in [&p, &r] {
        let head = stream_head(node.port, "/items/changes");
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert_eq!(header_of(&head, "Fennec-History"), Some(history), "{head}");
        assert_eq!(header_of(&head, "Fennec-Seq"), Some(seq), "{head}");
    }
}

/// A read its client marked names where the copy that answered it stood
/// as it read (`Fennec-Seq`, `Fennec-History`), as a write's answer does: a
/// live query reading near sends its next run after it, so a copy behind
/// the rows it showed -- a run answered from home, past a change whose
/// event had not reached the client -- never answers that run. Its JSON
/// path and the rows' alike; a read nobody marked names nothing.
#[test]
fn a_marked_read_names_where_the_copy_that_answered_it_stood() {
    let d = dir("read-position");
    let p = primary(&d.join("p.fennec"), replication::DEFAULT_BUFFER);
    let r = replica(&d.join("r.fennec"), p.port);
    query(&p, SCHEMA);
    let put = "{\"query\":\"put items {name: \\\"x\\\", n: 1, e: [1.0, 0.5, 2.0]}\"}";
    let (status, written, body) = raw(p.port, "POST", "/query", "", put);
    assert_eq!(status, 200, "{body}");
    let history = header_of(&written, "Fennec-History").expect("a write names its history");
    let seq = header_of(&written, "Fennec-Seq").unwrap();
    caught_up(&r, &p);
    for node in [&p, &r] {
        for q in ["get items", "get items count"] {
            let body = format!("{{\"query\":\"{q}\"}}");
            let (status, head, rows) = raw(
                node.port,
                "POST",
                "/query",
                "Fennec-Read: replica\r\n",
                &body,
            );
            assert_eq!(status, 200, "{rows}");
            assert_eq!(header_of(&head, "Fennec-Seq"), Some(seq), "{q}: {head}");
            assert_eq!(
                header_of(&head, "Fennec-History"),
                Some(history),
                "{q}: {head}"
            );
            let (status, head, rows) = raw(node.port, "POST", "/query", "", &body);
            assert_eq!(status, 200, "{rows}");
            assert_eq!(header_of(&head, "Fennec-Seq"), None, "{q}: {head}");
        }
    }
}

/// The primary sweeps the rows past their time (`@ttl`) and its replica
/// applies the deletes as any it is sent; the replica itself never sweeps,
/// its writes being its primary's.
#[test]
fn a_sweep_on_the_primary_reaches_its_replica() {
    let d = dir("sweep");
    let primary = primary(&d.join("p.fennec"), replication::DEFAULT_BUFFER);
    let replica = replica(&d.join("r.fennec"), primary.port);
    for sql in [
        "create collection s (t text, seen timestamp @ttl(1h))",
        r#"put s [{t: "old", seen: 1}, {t: "live", seen: "2999-01-01"}, {t: "older", seen: 2}, {t: "none"}]"#,
    ] {
        let (status, body) = query(&primary, sql);
        assert_eq!(status, 200, "{body}");
    }
    caught_up(&replica, &primary);
    // Out of every read on both at once, before anything is deleted.
    let alive = |n: &Node| rows(n, "get s select t order id");
    let want = vec![
        (2, vec![Value::Text("live".into())]),
        (4, vec![Value::Text("none".into())]),
    ];
    assert_eq!(alive(&primary), want);
    assert_eq!(alive(&replica), want);
    // A replica sweeps nothing.
    assert_eq!(fennec_http::sweep::pass("replica", &replica.db), 0);
    let before = seq(&primary);
    assert_eq!(fennec_http::sweep::pass("primary", &primary.db), 2);
    assert_eq!(seq(&primary), before + 2, "a delete a row");
    caught_up(&replica, &primary);
    // Gone from both, at any time a read is answered at.
    for n in [&primary, &replica] {
        n.db.write().unwrap().set_clock(Some(0));
        assert_eq!(rows(n, "get s count")[0].1, vec![Value::Int(2)]);
        assert_eq!(alive(n), want);
    }
}

/// A consumer's place is a row of `_consumers`, replicated as any write
/// is: a replica reads from where the primary's consumer stands and makes,
/// moves or lets go of none -- its consumers are its primary's -- and once
/// promoted it goes on from that place, its relay making the consumer
/// again as it starts without moving it.
#[test]
fn a_consumer_goes_on_from_its_place_on_a_promoted_replica() {
    let d = dir("consumer-failover");
    let p = primary(&d.join("p.fennec"), replication::DEFAULT_BUFFER);
    let r = replica(&d.join("r.fennec"), p.port);
    assert_eq!(query(&p, SCHEMA).0, 200);
    let (status, body) = http(p.port, "POST", "/_changes/consumers/relay", None, "");
    assert_eq!(status, 201, "{body}");
    write_some(&p, 0, 2);
    let (status, head, page) = raw(p.port, "GET", "/_changes?consumer=relay", "", "");
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("item 1"), "{page}");
    let next = header_of(&head, "Fennec-Next").unwrap().to_string();
    let commit = format!("{{\"since\": {next}}}");
    let (status, head, body) = raw(p.port, "PATCH", "/_changes/consumers/relay", "", &commit);
    assert_eq!(status, 200, "{body}");
    // The commit's answer names its change, as a write's does: a read sent
    // after it to the replica waits for it there, and finds the consumer
    // where the commit left it.
    let committed = header_of(&head, "Fennec-Seq").expect("a commit names its change");
    let (status, _, listed) = raw(
        r.port,
        "GET",
        "/_changes/consumers",
        &format!("Fennec-After: {committed}\r\nFennec-Wait: 20000\r\n"),
        "",
    );
    assert_eq!(status, 200, "{listed}");
    assert!(listed.contains(&format!("\"since\":{next}")), "{listed}");
    // Written after the commit: what the consumer reads next, wherever.
    write_some(&p, 2, 2);
    caught_up(&r, &p);

    // Read from where the consumer stands, until the last write is on the
    // replica's disk: the stream hands over only what an fsync covered.
    let unread = |port: u16| {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let (status, page) = http(port, "GET", "/_changes?consumer=relay&wait=5000", None, "");
            assert_eq!(status, 200, "{page}");
            if page.contains("item 3") {
                return page;
            }
            assert!(
                Instant::now() < deadline,
                "the writes after the commit never came: {page}"
            );
        }
    };
    let page = unread(r.port);
    assert!(
        page.contains("item 2") && !page.contains("item 1"),
        "{page}"
    );
    for (method, body) in [("POST", ""), ("PATCH", commit.as_str()), ("DELETE", "")] {
        let (status, answer) = http(r.port, method, "/_changes/consumers/relay", None, body);
        assert_eq!(status, 403, "{method} on a replica: {answer}");
    }

    let (status, body) = http(r.port, "POST", "/_replication/promote", Some(TOKEN), "");
    assert_eq!(status, 200, "{body}");
    let (status, body) = http(r.port, "POST", "/_changes/consumers/relay", None, "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(&format!("\"since\":{next}")), "{body}");
    let page = unread(r.port);
    assert!(
        page.contains("item 2") && !page.contains("item 1"),
        "{page}"
    );
    let (_, head, _) = raw(r.port, "GET", "/_changes?consumer=relay", "", "");
    let end = header_of(&head, "Fennec-Next").unwrap().to_string();
    let (status, body) = http(
        r.port,
        "PATCH",
        "/_changes/consumers/relay",
        None,
        &format!("{{\"since\": {end}}}"),
    );
    assert_eq!(status, 200, "{body}");
    let (status, page) = http(r.port, "GET", "/_changes?consumer=relay", None, "");
    assert_eq!((status, page.as_str()), (200, ""));
}

/// A primary and its replica each compact their file on their own, again
/// and again, while the primary takes updates: the replica follows across
/// every one -- the writes made during a compact reach it numbered on --
/// holds what the primary holds, and opened again over its compacted file
/// it asks for what it missed rather than for an image.
#[test]
fn a_primary_and_its_replica_compact_on_their_own_while_writes_go_on() {
    use fennec_core::engine::{CompactPolicy, Compactor};
    let d = dir("autocompact");
    let p = primary(&d.join("p.fennec"), replication::DEFAULT_BUFFER);
    assert_eq!(query(&p, SCHEMA).0, 200);
    write_some(&p, 0, 40);
    let rfile = d.join("r.fennec");
    let r = replica(&rfile, p.port);
    caught_up(&r, &p);

    let small = CompactPolicy {
        ratio: 0.5,
        floor: 16 << 10,
        memory: false,
    };
    let every = Duration::from_millis(3);
    let compactors = [
        Compactor::start_every(&p.db, small, every).unwrap(),
        Compactor::start_every(&r.db, small, every).unwrap(),
    ];
    let pad = "y".repeat(300);
    for round in 0..40 {
        let (status, body) = query(
            &p,
            &format!("set items {{name: \"round {round} {pad}\", e: [{round}.0, 2.0, 1.0]}} where n < 30"),
        );
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            query(&p, &format!("del items where n = {}", 30 + round % 10)).0,
            200
        );
        write_some(&p, 100 + round * 2, 2);
    }
    drop(compactors);
    caught_up(&r, &p);
    let compacted = |n: &Node| n.db.read().unwrap().compactions();
    assert!(
        compacted(&p) >= 2,
        "the primary compacted {} times",
        compacted(&p)
    );
    assert!(
        compacted(&r) >= 2,
        "the replica compacted {} times",
        compacted(&r)
    );
    for sql in [
        "get items",
        "get items select id, n match name \"round\" limit 1000",
    ] {
        assert_eq!(rows(&r, sql), rows(&p, sql), "{sql}");
    }
    let near = |n: &Node| {
        let mut v = rows(
            n,
            "get items select id near e [3.0, 2.0, 1.0] exact limit 10000",
        );
        v.sort_by_key(|r| r.0);
        v
    };
    assert_eq!(near(&r), near(&p));

    // Opened again over the file it compacted, it goes on from where it was.
    let images = r.follower.as_ref().unwrap().images();
    halt(r.follower.as_ref().unwrap());
    let at = seq(&r);
    drop(r);
    write_some(&p, 500, 5);
    let (db, _) = replication::open(rfile.to_str().unwrap(), replication::DEFAULT_BUFFER).unwrap();
    assert_eq!(db.change_seq(), at);
    drop(db);
    let r = replica(&rfile, p.port);
    caught_up(&r, &p);
    assert_eq!(r.follower.as_ref().unwrap().images(), 0, "{images} before");
    assert_eq!(rows(&r, "get items"), rows(&p, "get items"));
}

/// The writes a raw replication stream has been sent: the last one, after
/// reading whatever the socket holds now and waiting for nothing more.
struct RawStream {
    s: TcpStream,
    buf: Vec<u8>,
    head: bool,
    last: u64,
}

impl RawStream {
    fn open(port: u16) -> RawStream {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            s,
            "GET /_replication?since=0 HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TOKEN}\r\n\r\n"
        )
        .unwrap();
        RawStream {
            s,
            buf: Vec::new(),
            head: false,
            last: 0,
        }
    }

    /// Reads what has arrived, blocking for none of it.
    fn drain(&mut self) {
        self.s.set_nonblocking(true).unwrap();
        let mut chunk = [0u8; 1 << 16];
        loop {
            match self.s.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("{e}"),
            }
        }
        self.s.set_nonblocking(false).unwrap();
        if !self.head {
            let Some(end) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                return;
            };
            assert!(self.buf.starts_with(b"HTTP/1.1 200"));
            self.buf.drain(..end + 4);
            self.head = true;
        }
        while self.buf.len() >= 9 {
            let len = u64::from_le_bytes(self.buf[1..9].try_into().unwrap()) as usize;
            if self.buf.len() < 9 + len {
                break;
            }
            let at = |i: usize| u64::from_le_bytes(self.buf[9 + i..17 + i].try_into().unwrap());
            match self.buf[0] {
                // [version][image][seq]...
                b'H' => self.last = self.last.max(at(2)),
                // [first][n]...
                b'W' => self.last = self.last.max(at(0) + at(8) - 1),
                _ => {}
            }
            self.buf.drain(..9 + len);
        }
    }
}

/// A durable write is answered once the stream to each replica has sent it:
/// sent after the answer, a write the client was told about was lost when
/// the primary died between the two, and its replica was promoted without
/// it (Trellis's node killed under eight writers, 3 runs in 60). Bytes in
/// the stream's socket reach the replica after the process is gone, so each
/// answered write must be in the stream already, with nothing waited for.
#[test]
fn a_durable_write_is_answered_once_its_replicas_streams_have_sent_it() {
    let d = dir("sent-first");
    let p = primary(&d.join("p.fennec"), replication::DEFAULT_BUFFER);
    assert_eq!(query(&p, SCHEMA).0, 200);
    let mut stream = RawStream::open(p.port);
    // Once the stream has its first write it is sending, and every write
    // after that waits for it.
    write_some(&p, 0, 1);
    let deadline = Instant::now() + Duration::from_secs(10);
    while stream.last < seq(&p) {
        assert!(
            Instant::now() < deadline,
            "the stream never sent the first write"
        );
        stream.drain();
        std::thread::sleep(Duration::from_millis(1));
    }
    for i in 1..1000 {
        write_some(&p, i, 1);
        let answered = seq(&p);
        stream.drain();
        assert!(
            stream.last >= answered,
            "write {answered} was answered before its stream sent it (sent {})",
            stream.last
        );
    }
}

/// A claim is a write, so a replica refuses it as it refuses any (403) --
/// at once, held or not: what a held claim waits for is the primary's.
#[test]
fn a_held_claim_on_a_replica_is_refused_at_once() {
    let d = dir("held");
    let p = primary(&d.join("p.fennec"), replication::DEFAULT_BUFFER);
    let r = replica(&d.join("r.fennec"), p.port);
    query(&p, SCHEMA);
    caught_up(&r, &p);
    let started = Instant::now();
    let (status, _, body) = raw(
        r.port,
        "POST",
        "/query",
        "Fennec-Wait: 5000\r\n",
        "{\"query\":\"set items {n: 1} where n = 999 order n limit 1 returning id\"}",
    );
    assert_eq!(status, 403, "{body}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// A replica's file following `upstream`, its follower started: what a
/// promotion is asked of.
fn following(tag: &str, upstream: &str) -> (Arc<RwLock<Database>>, Arc<Follower>) {
    let d = dir(tag);
    let (mut db, feed) = replication::open(
        d.join("r.fennec").to_str().unwrap(),
        replication::DEFAULT_BUFFER,
    )
    .unwrap();
    let lineage = db.history().lineage.clone();
    db.follow(lineage).unwrap();
    let db = Arc::new(RwLock::new(db));
    let follower =
        Follower::new(upstream, TOKEN.into(), Arc::clone(&db), Some(feed), true).unwrap();
    follower.start(tag.into()).unwrap();
    (db, follower)
}

/// A promotion halts the follower first, and the follower may be in the
/// middle of reconnecting to a primary that is gone: the halt waited for
/// that attempt to fail -- its connect's 5 s where the dead machine's
/// address swallows packets, the tail of a failover in V5's drill, and as
/// long as the system's resolver took where the primary's name was slow to
/// be refused. Here the primary's name is never answered until the test
/// ends: the promotion is done while its lookup still hangs, and well
/// inside the 5 s the dial is bounded by, which a halt waiting for the dial
/// would have waited out.
#[test]
fn a_promotion_does_not_wait_out_a_reconnect_in_flight() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Condvar, Mutex};
    // The lookup, asked and held until let go; let go at the latest 30 s
    // on, so a build whose halt waits for it ends, failing.
    let name = "gone.halt.replication.test:1";
    let gate = Arc::new((Mutex::new((false, false)), Condvar::new()));
    let g = Arc::clone(&gate);
    fennec_http::dial::stand_in(name, move || {
        let (state, moved) = &*g;
        let mut s = state.lock().unwrap();
        s.0 = true;
        moved.notify_all();
        while !s.1 {
            s = moved.wait(s).unwrap();
        }
        Err(std::io::Error::other("the resolver gave up"))
    });
    let released = Arc::new(AtomicBool::new(false));
    let (ended, ending) = mpsc::channel::<()>();
    let letting_go = {
        let (gate, released) = (Arc::clone(&gate), Arc::clone(&released));
        std::thread::spawn(move || {
            let _ = ending.recv_timeout(Duration::from_secs(30));
            released.store(true, Ordering::SeqCst);
            gate.0.lock().unwrap().1 = true;
            gate.1.notify_all();
        })
    };
    let (db, follower) = following("halt-lookup", &format!("http://{name}"));
    {
        let (state, moved) = &*gate;
        let mut s = state.lock().unwrap();
        while !s.0 {
            s = moved.wait(s).unwrap();
        }
    }
    let asked = Instant::now();
    let promoted = follower.promote(fresh_id());
    let took = asked.elapsed();
    let waited = released.load(Ordering::SeqCst);
    drop(ended);
    letting_go.join().unwrap();
    assert!(promoted.is_ok(), "{:?}", promoted.err());
    assert!(
        !waited && took < Duration::from_millis(2500),
        "the promotion waited {took:?} for the follower's dial of a primary that is gone"
    );
    assert!(!db.read().unwrap().history().following);
    assert!(!follower.running());
}

/// The connection to the primary is handed to the halt before the request
/// goes out: handed over once the primary's head had come, a primary that
/// took the connection and never answered -- stopped, or its machine
/// stalled -- held a promotion for the 10 s the follower waits for a word.
/// Here the primary takes the connection, reads the request and says
/// nothing.
#[test]
fn a_promotion_does_not_wait_for_a_primary_that_never_answers() {
    use std::sync::mpsc;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (read, asked) = mpsc::channel();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut buf = [0u8; 1024];
            if conn.read(&mut buf).unwrap_or(0) > 0 {
                let _ = read.send(());
            }
            held.push(conn);
        }
    });
    let (db, follower) = following("halt-silent", &format!("http://{addr}"));
    asked
        .recv_timeout(Duration::from_secs(20))
        .expect("the follower never asked its primary");
    let started = Instant::now();
    let promoted = follower.promote(fresh_id());
    let took = started.elapsed();
    assert!(promoted.is_ok(), "{:?}", promoted.err());
    assert!(
        took < Duration::from_secs(5),
        "the promotion waited {took:?} for a primary that never answers (the follower's \
         silence is 10 s)"
    );
    assert!(!db.read().unwrap().history().following);
}

/// A halt ends a follower that is between two reads as it comes, whatever
/// its socket does with a read begun after the cut. On Windows the cut -- a
/// `shutdown` through a duplicate of the socket -- ends a read already
/// waiting, not one begun after it, which read on: the primary's keep-alive
/// every 2 s kept it short of its 10 s of silence, the session never looked
/// at the halt again, and the halt waited for it for good. Five runs of
/// Windows CI hung so, each in a test halting a follower that had just
/// landed a write. Here the follower is held in its hello's callback, past
/// its read, until the cut has reached the primary's side of a relay
/// between them; let go, it reads no more.
#[test]
fn a_halt_ends_a_follower_held_between_two_reads() {
    use std::sync::mpsc;
    use std::sync::{Condvar, Mutex};
    let d = dir("halt-between-reads");
    let p = primary(&d.join("p.fennec"), replication::DEFAULT_BUFFER);
    // The follower's bytes to the primary and back, and word once the
    // follower's side has ended: the cut's FIN.
    let relay = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let via = relay.local_addr().unwrap();
    let (ended, cut) = mpsc::channel();
    let to = p.port;
    std::thread::spawn(move || {
        for conn in relay.incoming() {
            let Ok(follower) = conn else { continue };
            let primary = TcpStream::connect(("127.0.0.1", to)).unwrap();
            let mut up = (follower.try_clone().unwrap(), primary.try_clone().unwrap());
            let ended = ended.clone();
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut up.0, &mut up.1);
                let _ = ended.send(());
            });
            let mut down = (primary, follower);
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut down.0, &mut down.1);
            });
        }
    });
    let file = d.join("r.fennec");
    let (mut db, feed) =
        replication::open(file.to_str().unwrap(), replication::DEFAULT_BUFFER).unwrap();
    let lineage = db.history().lineage.clone();
    db.follow(lineage).unwrap();
    let db = Arc::new(RwLock::new(db));
    let follower = Follower::new(
        &format!("http://{via}"),
        TOKEN.into(),
        Arc::clone(&db),
        Some(feed),
        true,
    )
    .unwrap();
    // Held after the hello is read and taken, before the next read, until
    // the gate opens.
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (said, hello) = mpsc::channel();
    let held = Arc::clone(&gate);
    follower.on_hello(Box::new(move |_| {
        let _ = said.send(());
        let (open, opened) = &*held;
        let mut open = open.lock().unwrap();
        while !*open {
            open = opened.wait(open).unwrap();
        }
    }));
    follower.start("halt-between-reads".into()).unwrap();
    hello
        .recv_timeout(Duration::from_secs(20))
        .expect("the primary never said hello to the follower");
    let (halted, done) = mpsc::channel();
    let f = Arc::clone(&follower);
    std::thread::spawn(move || {
        f.halt();
        let _ = halted.send(());
    });
    cut.recv_timeout(Duration::from_secs(20))
        .expect("the halt never cut the follower's connection");
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    done.recv_timeout(Duration::from_secs(20))
        .expect("the follower read on past its halt's cut, and the halt never ended");
    assert!(!follower.running());
    let _ = std::fs::remove_dir_all(&d);
}

/// A copy's own sink, panicking as it is handed a write: a panic as the
/// copy applies its primary's records, past the point the engine puts them
/// back.
struct Panicking;

impl Sink for Panicking {
    fn append(&mut self, _: &[u8]) -> fennec_core::error::Result<()> {
        Ok(())
    }
    fn record(&mut self, _: u64, _: &[u8]) -> fennec_core::error::Result<()> {
        panic!("a test's panic as a write is handed to the sink")
    }
    fn rewrite(&mut self, _: &[u8]) -> fennec_core::error::Result<()> {
        Ok(())
    }
}

/// A panic applying a primary's records is a write the follower could not
/// apply: the engine put the records back whole, or -- here, from the sink
/// call on -- failed the copy, which then takes nothing more and says why.
/// Let through, it ended the follower's thread with the lock poisoned and
/// nothing in the log, and the halt a promotion starts with waited for that
/// thread for good.
#[test]
fn a_panic_applying_a_write_ends_the_follower_and_never_its_halt() {
    let d = dir("apply-panic");
    let p = primary(&d.join("p.fennec"), replication::DEFAULT_BUFFER);
    let (status, body) = query(&p, SCHEMA);
    assert_eq!(status, 200, "{body}");
    let mut db = Database::with_sink(Box::new(Panicking));
    let lineage = db.history().lineage.clone();
    db.follow(lineage).unwrap();
    let db = Arc::new(RwLock::new(db));
    let upstream = format!("http://127.0.0.1:{}", p.port);
    let follower = Follower::new(&upstream, TOKEN.into(), Arc::clone(&db), None, false).unwrap();
    follower.start("apply-panic".into()).unwrap();
    write_some(&p, 0, 3);
    let failed = || {
        db.read()
            .unwrap_or_else(|e| e.into_inner())
            .failure()
            .is_some()
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while !failed() {
        assert!(
            Instant::now() < deadline,
            "the copy took the panic as nothing"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let (halted, halt) = std::sync::mpsc::channel();
    let f = Arc::clone(&follower);
    std::thread::spawn(move || {
        f.halt();
        let _ = halted.send(());
    });
    halt.recv_timeout(Duration::from_secs(20))
        .expect("the halt waited for a follower's thread a panic ended");
    assert!(!follower.running());
    assert!(!db.is_poisoned(), "the panic let go of the lock unwinding");
    let _ = std::fs::remove_dir_all(&d);
}

/// A database's sink in memory, keeping the records it is handed: what a
/// primary's feed would send.
#[derive(Clone, Default)]
struct Tap(Arc<std::sync::Mutex<Vec<Vec<u8>>>>);

impl Sink for Tap {
    fn append(&mut self, _: &[u8]) -> fennec_core::error::Result<()> {
        Ok(())
    }
    fn record(&mut self, _: u64, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.0.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }
    fn rewrite(&mut self, _: &[u8]) -> fennec_core::error::Result<()> {
        Ok(())
    }
}

/// A copy that cannot apply a primary's record part way into what it was
/// sent -- an index made on a collection whose fields the copy holds
/// otherwise, after writes whose vectors wait to be linked with it -- puts
/// back what it had not landed, connects again for an image and ends the
/// primary's copy, taking writes on from there.
#[test]
fn a_copy_that_could_not_apply_a_write_takes_an_image_and_goes_on() {
    let d = dir("apply-refused");
    let p = primary(&d.join("p.fennec"), replication::DEFAULT_BUFFER);
    let (status, body) = query(&p, "create collection items2 (y int)");
    assert_eq!(status, 200, "{body}");
    // The copy's `items2`, made as the primary's first write: the same
    // collection to the counter and the history, with another field.
    let tap = Tap::default();
    let mut other = Database::with_sink(Box::new(tap.clone()));
    let made = fennec_ql::parse_one("create collection items2 (x text)").unwrap();
    other.execute(&made).unwrap();
    let mut db = Database::new();
    db.follow(p.db.read().unwrap().history().lineage.clone())
        .unwrap();
    db.apply(&tap.0.lock().unwrap().concat()).unwrap();
    assert_eq!(db.change_seq(), seq(&p));
    // Writes it takes, their vectors waiting together, and the record it
    // cannot, sent to it together.
    assert_eq!(query(&p, SCHEMA).0, 200);
    write_some(&p, 0, 5);
    let (status, body) = query(&p, "create index on items2 (y) @sorted");
    assert_eq!(status, 200, "{body}");
    let db = Arc::new(RwLock::new(db));
    let upstream = format!("http://127.0.0.1:{}", p.port);
    let follower = Follower::new(&upstream, TOKEN.into(), Arc::clone(&db), None, false).unwrap();
    follower.start("apply-refused".into()).unwrap();
    let at = || db.read().unwrap().change_seq();
    let deadline = Instant::now() + Duration::from_secs(20);
    while follower.images() == 0 || at() != seq(&p) {
        assert!(
            Instant::now() < deadline,
            "the copy is at {} after {} images, the primary at {}",
            at(),
            follower.images(),
            seq(&p)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    write_some(&p, 5, 3);
    let deadline = Instant::now() + Duration::from_secs(20);
    while at() != seq(&p) {
        assert!(Instant::now() < deadline, "the copy stayed at {}", at());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(follower.images(), 1);
    assert!(db.read().unwrap().failure().is_none());
    for sql in [
        "get items",
        "get items select id near e [3.0, 1.0, 0.5] exact limit 3",
        "get items2 select id where y > 0",
    ] {
        let copy = db.read().unwrap();
        let r = copy
            .query(&fennec_ql::parse_one(sql).unwrap(), &[])
            .unwrap();
        let got: Vec<(u64, Vec<Value>)> = (r.rows().unwrap().rows.iter())
            .map(|r| (r.id, r.values.clone()))
            .collect();
        assert_eq!(got, rows(&p, sql), "{sql}");
    }
    halt(&follower);
    let _ = std::fs::remove_dir_all(&d);
}

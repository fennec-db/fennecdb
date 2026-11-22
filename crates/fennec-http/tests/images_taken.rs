//! A replica's image is written into a file beside the copy's as it comes,
//! checked as it streams, and renamed into the copy's place once whole
//! (`replication::read_answer`, the follower's path, over a copy mapped as
//! a server's): an image torn or refused leaves the copy's file as it was
//! and no file beside it, and a crash at any point leaves the old file or
//! the new, which the next start reads as it is -- the image beside it
//! swept as the follower starts.

use fennec_core::engine::Database;
use fennec_core::history::History;
use fennec_http::replication::{self, Follower};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "fennec-images-taken-{tag}-{}-{:?}",
        std::process::id(),
        Instant::now()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn exec(db: &mut Database, q: &str) {
    db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
}

fn rows(db: &Database) -> Vec<String> {
    let q = fennec_ql::parse_one("get docs select t order id").unwrap();
    let r = db.query(&q, &[]).unwrap();
    r.rows()
        .unwrap()
        .rows
        .iter()
        .map(|r| format!("{:?}", r.values))
        .collect()
}

/// A primary's database of a few documents, as a stream's image is taken
/// of it.
struct Primary {
    image: Vec<u8>,
    /// Its change.
    seq: u64,
    lineage: Vec<(u64, u64)>,
    /// Its rows, as [`rows`] reads them.
    rows: Vec<String>,
}

fn primary() -> Primary {
    let mut db = Database::new();
    db.fork(replication::fresh_id()).unwrap();
    exec(&mut db, "create collection docs (t text, n int @hash)");
    for i in 0..300 {
        exec(&mut db, &format!("put docs {{t: \"primary {i}\", n: {i}}}"));
    }
    Primary {
        image: db.snapshot(),
        seq: db.change_seq(),
        lineage: db.history().lineage.clone(),
        rows: rows(&db),
    }
}

/// A copy's file, a database of its own, opened as a server opens one.
fn copy(at: &Path) -> (Database, Vec<String>) {
    let mut db = fennec_core::fs::open(at).unwrap();
    exec(&mut db, "create collection docs (t text, n int @hash)");
    exec(&mut db, "put docs {t: \"the copy's own\", n: 1}");
    db.sync().unwrap();
    let held = rows(&db);
    (db, held)
}

/// A primary's answer: its head, a hello promising an image of change
/// `seq` on `lineage`, and an image announced `image.len()` bytes long, the
/// first `sent` of them sent.
fn answer(image: &[u8], sent: usize, seq: u64, lineage: &[(u64, u64)]) -> Vec<u8> {
    let mut hello = vec![1u8, 1];
    for v in [seq, seq, 0] {
        hello.extend_from_slice(&v.to_le_bytes());
    }
    hello.extend_from_slice(
        &History {
            lineage: lineage.to_vec(),
            following: false,
        }
        .encode(),
    );
    let mut out =
        b"HTTP/1.1 200 OK\r\nContent-Type: application/x-fennec-replication\r\n\r\n".to_vec();
    out.push(b'H');
    out.extend_from_slice(&(hello.len() as u64).to_le_bytes());
    out.extend_from_slice(&hello);
    out.push(b'I');
    out.extend_from_slice(&(image.len() as u64).to_le_bytes());
    out.extend_from_slice(&image[..sent]);
    out
}

fn taking(file: &Path) -> PathBuf {
    fennec_core::fs::beside(file, "taking")
}

/// Taken whole, the image is the copy's file -- the bytes sent, to the
/// byte, and the record saying the copy follows after them: the primary's
/// rows, its change, a copy following the primary's lineage, read so again
/// once opened, and nothing beside it -- a copy mapped as a server's, and
/// one read into memory (`--no-mmap`), which loads the image so too.
#[test]
fn an_image_taken_whole_is_the_copys_file() {
    let d = dir("whole");
    let Primary {
        image,
        seq,
        lineage,
        rows: want,
    } = primary();
    let follows = History {
        lineage: lineage.clone(),
        following: true,
    }
    .record();
    for mapped in [true, false] {
        let file = d.join(format!("copy-{mapped}.fennec"));
        let (db, _) = copy(&file);
        let mut db = match mapped {
            true => db,
            false => {
                drop(db);
                fennec_core::fs::open_in_memory(&file).unwrap()
            }
        };
        assert_eq!(db.is_mapped(), mapped);
        let bytes = answer(&image, image.len(), seq, &lineage);
        let (n, end) = replication::read_answer(&mut &bytes[..], Some(&mut db));
        assert_eq!(n, 2, "{end}");
        assert_eq!(end.kind(), std::io::ErrorKind::UnexpectedEof, "{end}");
        assert_eq!(rows(&db), want, "mapped {mapped}");
        assert_eq!(db.change_seq(), seq);
        assert!(!taking(&file).exists(), "the image's file is the copy's");
        let landed = std::fs::read(&file).unwrap();
        assert!(
            landed == [&image[..], &follows].concat(),
            "the bytes sent, landed"
        );
        drop(db);
        let db = fennec_core::fs::open(&file).unwrap();
        assert_eq!((rows(&db), db.change_seq()), (want.clone(), seq));
        assert!(
            db.history().following,
            "a copy that follows, as its file says"
        );
        assert_eq!(db.history().lineage, lineage);
    }
    let _ = std::fs::remove_dir_all(d);
}

/// An image cut short on the way, and one a record of which fails its sum,
/// are refused as they stream: the copy holds what it held, its file is
/// the one it had, to the byte, and nothing is left beside it.
#[test]
fn an_image_torn_or_refused_leaves_the_copy_as_it_was() {
    let d = dir("refused");
    let Primary {
        image,
        seq,
        lineage,
        ..
    } = primary();
    let mut damaged = image.clone();
    // A byte of a document's text: its record's sum fails.
    let at = damaged
        .windows(9)
        .position(|w| w == b"primary 1")
        .expect("a document's text");
    damaged[at] ^= 0x20;
    for (how, bytes, said) in [
        (
            "torn",
            answer(&image, image.len() / 2, seq, &lineage),
            "cut short",
        ),
        (
            "refused",
            answer(&damaged, damaged.len(), seq, &lineage),
            "checksum",
        ),
    ] {
        let file = d.join(format!("{how}.fennec"));
        let (mut db, held) = copy(&file);
        let before = std::fs::read(&file).unwrap();
        let (n, end) = replication::read_answer(&mut &bytes[..], Some(&mut db));
        assert_eq!(n, 1, "{how}: the hello alone read whole");
        assert!(end.to_string().contains(said), "{how}: {end}");
        assert_eq!(rows(&db), held, "{how}: the copy holds what it held");
        assert!(!taking(&file).exists(), "{how}: nothing left beside it");
        assert_eq!(
            std::fs::read(&file).unwrap(),
            before,
            "{how}: its file as it was"
        );
        drop(db);
        assert_eq!(rows(&fennec_core::fs::open(&file).unwrap()), held, "{how}");
    }
    let _ = std::fs::remove_dir_all(d);
}

/// A crash before the rename -- the image written beside the copy whole,
/// its follow record after it, synced, or cut short anywhere -- leaves the
/// copy's file as it was, which the next start opens as the copy it was;
/// a follower started over it sweeps the file beside it. After the rename
/// the file is the image with the record saying the copy follows, which
/// the next start opens as a copy following the primary's lineage at the
/// image's change.
#[test]
fn a_crash_leaves_the_old_file_or_the_new() {
    let d = dir("crash");
    let Primary {
        image,
        seq,
        lineage,
        rows: want,
    } = primary();
    let follows = History {
        lineage: lineage.clone(),
        following: true,
    }
    .record();
    let whole = [&image[..], &follows].concat();

    // Before the rename: the file beside it whole, or a part of it.
    for (how, beside) in [
        ("whole", &whole[..]),
        ("cut short", &whole[..whole.len() / 3]),
    ] {
        let file = d.join(format!("before-{}.fennec", how.replace(' ', "-")));
        let (db, held) = copy(&file);
        drop(db);
        std::fs::write(taking(&file), beside).unwrap();
        let db = fennec_core::fs::open(&file).unwrap();
        assert_eq!(rows(&db), held, "{how}: the copy it was");
        assert!(!db.history().following, "{how}");
        // A follower started over it sweeps what was beside it, before it
        // connects anywhere -- here to no primary at all.
        let db = Arc::new(RwLock::new(db));
        let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", gone.local_addr().unwrap());
        drop(gone);
        let f = Follower::new(&url, "t".into(), Arc::clone(&db), None, false).unwrap();
        let run = {
            let f = Arc::clone(&f);
            std::thread::spawn(move || f.run())
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while taking(&file).exists() {
            assert!(
                Instant::now() < deadline,
                "{how}: the file beside it stayed"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        f.halt();
        run.join().unwrap();
        assert_eq!(rows(&db.read().unwrap()), held, "{how}");
    }

    // After the rename: the image and its follow record are the file.
    let file = d.join("after.fennec");
    std::fs::write(&file, &whole).unwrap();
    let db = fennec_core::fs::open(&file).unwrap();
    assert_eq!((rows(&db), db.change_seq()), (want, seq));
    assert!(db.history().following, "a copy that follows");
    assert_eq!(db.history().lineage, lineage);
    let _ = std::fs::remove_dir_all(d);
}

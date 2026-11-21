//! What a reader that takes no lock (`fs::open_read_only`) finds of a file
//! by reading it, never through a mapping (`engine::surveyed`): the survey
//! a load's walk of the mapping would make -- the last graph of each index
//! and the durable point -- and the point no open to write the file cuts it
//! before, which the reader maps up to and reads the rest of into memory.
//! Held to the walk and to the open's cut over a file torn at every point
//! of its tail, cut short there or zeros from there to its end, as a length
//! reaching the disk before its bytes leaves it.

#![cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]

use fennec_core::engine::reader_survey;
use fennec_core::prelude::*;

fn run(db: &mut Database, sql: &str, params: &[Value]) {
    db.execute_with(&fennec_ql::parse_one(sql).expect("parse"), params)
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// A file of every record a reader meets: an image holding a graph, writes
/// after it whose syncs claim points the records after them carry, records
/// of 200 KiB among small ones, graphs saved into the tail, and blocks that
/// spill, one landed and one put back -- past a mebibyte, so the reader
/// reads it in more than one window.
fn written() -> (Vec<u8>, usize) {
    let dir = std::env::temp_dir().join(format!("fennecdb-reader-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("reader.fennec");
    let mut biggest = 0;
    {
        let mut db = fennec_core::fs::open(&path).unwrap();
        run(
            &mut db,
            "create collection docs (title text, v vector<4> @hnsw(cosine, m=8))",
            &[],
        );
        let put = |db: &mut Database, i: usize, n: usize| {
            let v = Value::Vector(
                (0..4)
                    .map(|k| ((i * 7 + k * 3) % 11) as f32 + 0.5)
                    .collect(),
            );
            run(
                db,
                "put docs {title: $1, v: $2}",
                &[Value::Text("t".repeat(n)), v],
            );
        };
        for i in 0..40 {
            put(&mut db, i, 50);
        }
        db.checkpoint().unwrap();
        db.set_spill(2048);
        for i in 40..400usize {
            let n = match i % 131 {
                0 => 200 << 10,
                k => 10 + k * 23,
            };
            biggest = biggest.max(n + 128);
            put(&mut db, i, n);
            if i % 7 == 0 {
                db.sync().unwrap();
            }
            if i % 53 == 0 {
                let _ = db.save_graphs().unwrap();
            }
            if i % 97 == 0 {
                db.begin().unwrap();
                for k in 0..20 {
                    put(&mut db, 1000 + i + k, 300);
                }
                if i % 2 == 0 {
                    db.commit().unwrap();
                } else {
                    db.rollback();
                }
            }
        }
    }
    let file = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (file, biggest)
}

#[test]
fn a_reader_surveys_by_reading_as_the_walk_does_and_maps_short_of_every_cut() {
    let (file, biggest) = written();
    assert!(file.len() > 1 << 20, "{} bytes", file.len());
    let (mut opened, mut cut, mut graphs) = (0, 0, 0);
    for at in (MAGIC_LEN..=file.len())
        .step_by(4093)
        .chain([file.len() - 1])
    {
        for zeroed in [false, true] {
            let mut b = file.clone();
            match zeroed {
                false => b.truncate(at),
                true => b[at..].fill(0),
            }
            let (read, walked) = reader_survey(&b);
            let (found, uncut) = match (read, walked) {
                (Ok((found, uncut)), Ok(walked)) => {
                    assert_eq!(found, walked, "torn at {at}, zeroed {zeroed}");
                    (found, uncut)
                }
                (Err(_), Err(_)) => continue,
                (read, walked) => {
                    panic!("torn at {at}, zeroed {zeroed}: {read:?} against {walked:?}")
                }
            };
            graphs += usize::from(!found.0.is_empty());
            let Ok(whole) = Database::new().load(&b) else {
                continue;
            };
            opened += 1;
            cut += usize::from(whole < b.len());
            assert!(
                uncut <= whole,
                "torn at {at}, zeroed {zeroed}: {uncut} past the cut at {whole}"
            );
            // A mark's step, a record, and the records since a sync.
            assert!(
                whole - uncut <= (64 << 10) + 2 * biggest + (8 << 10) * 8,
                "torn at {at}, zeroed {zeroed}: {uncut} far short of the cut at {whole}"
            );
        }
    }
    assert!(
        opened > 400 && cut > 300 && graphs > 300,
        "{opened} opened, {cut} cut, {graphs} with graphs"
    );
}

/// A file with nothing past its image -- what a compact, a checkpoint,
/// `.save` and a backup write -- is durable to its end, and no open cuts
/// it: a reader maps it whole and reads none of it into memory of its own.
/// Until 2026-11-21 its survey's point fell on the start of its last
/// record: of a collection with no graph that is its data, about the whole
/// file, and the reader read it into memory.
#[test]
fn a_reader_maps_a_file_with_nothing_past_its_image_whole() {
    let dir = std::env::temp_dir().join(format!("fennecdb-reader-image-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let rows = |db: &mut Database| {
        run(db, "create collection t (body text)", &[]);
        for i in 0..64 {
            let body = format!("{i:02}{}", "a".repeat(8 << 10));
            run(db, "put t {body: $1}", &[Value::Text(body)]);
        }
    };
    // `.save`'s and a backup's: the image alone.
    let mut db = Database::new();
    rows(&mut db);
    let saved = db.snapshot();
    // A checkpoint's: the same rows appended to a file, which the
    // checkpoint writes anew, closed with no mark after it.
    let path = dir.join("checkpointed.fennec");
    {
        let mut db = fennec_core::fs::open(&path).unwrap();
        rows(&mut db);
        db.checkpoint().unwrap();
    }
    let checkpointed = std::fs::read(&path).unwrap();
    for (what, image) in [(".save", saved), ("a checkpoint", checkpointed)] {
        let (read, walked) = reader_survey(&image);
        let ((found, uncut), walked) = (read.unwrap(), walked.unwrap());
        assert_eq!(found, walked, "{what}");
        assert_eq!(found.1, image.len(), "{what}: durable short of its end");
        #[cfg(unix)]
        let copied = copied(&dir.join("read.fennec"), &image);
        #[cfg(not(unix))]
        let copied = 0;
        assert_eq!(
            (uncut, copied),
            (image.len(), 0),
            "{what}, {} bytes: mapped short of {uncut}, {copied} bytes of its rows read into memory",
            image.len()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The bytes of `image`'s rows a reader holds in memory of its own, as the
/// rows tell it: `image` written to `path` and opened read-only, then the
/// rows' text written over in place -- what no writer does, a file only
/// appended to or replaced by a rename -- and the rows read back: a page
/// the reader maps reads the new bytes, the page cache's, and one it read
/// into memory the old.
#[cfg(unix)]
fn copied(path: &std::path::Path, image: &[u8]) -> usize {
    use std::os::unix::fs::FileExt;
    std::fs::write(path, image).unwrap();
    let mut reader = fennec_core::fs::open_read_only(path).unwrap();
    // The rows' text alone: runs of a kilobyte of `a`, which nothing else
    // of the file holds.
    let mut over = image.to_vec();
    let mut i = 0;
    while i < over.len() {
        let run = over[i..].iter().take_while(|&&b| b == b'a').count();
        if run >= 1 << 10 {
            over[i..i + run].fill(b'b');
        }
        i += run.max(1);
    }
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.write_all_at(&over, 0).unwrap();
    drop(file);
    let get = fennec_ql::parse_one("get t").unwrap();
    let Response::Rows(rs) = reader.execute(&get).unwrap() else {
        panic!("expected rows");
    };
    assert_eq!(rs.rows.len(), 64);
    let (old, new) = rs
        .rows
        .iter()
        .flat_map(|r| &r.values)
        .map(|v| match v {
            Value::Text(t) => (
                t.bytes().filter(|&b| b == b'a').count(),
                t.bytes().filter(|&b| b == b'b').count(),
            ),
            _ => (0, 0),
        })
        .fold((0, 0), |(a, b), (x, y)| (a + x, b + y));
    // Every row's text read, whichever way.
    assert_eq!(old + new, 64 << 13, "{old} old bytes, {new} new");
    drop(reader);
    let _ = std::fs::remove_file(path);
    old
}

const MAGIC_LEN: usize = fennec_core::engine::MAGIC.len();

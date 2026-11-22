//! The file beside a database a replica takes its primary's image into
//! (`Sink::image_side`, `<file>.taking`): renamed into the database's place
//! only where it is still the file its follower wrote, and one a crash left
//! there removed by whoever opens the database to write it -- its owner,
//! under `<file>.lock` -- never by a tool that only looks.

#![cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]

use fennec_core::engine::ImageOut;
use fennec_core::fs::SideFile;
use fennec_core::prelude::*;
use std::path::{Path, PathBuf};

fn exec(db: &mut Database, sql: &str) {
    db.execute(&fennec_ql::parse_one(sql).expect("parse"))
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

fn rows(db: &Database) -> Vec<String> {
    let q = fennec_ql::parse_one("get c select x order id").unwrap();
    let r = db.query(&q, &[]).unwrap();
    r.rows()
        .unwrap()
        .rows
        .iter()
        .map(|r| format!("{:?}", r.values))
        .collect()
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fennecdb-side-files-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A primary's image: a collection of the copy's name, other rows.
fn image() -> Vec<u8> {
    let mut db = Database::new();
    exec(&mut db, "create collection c (x int)");
    for x in [100, 200, 300] {
        exec(&mut db, &format!("put c {{x: {x}}}"));
    }
    db.snapshot()
}

/// A file's inode, which a rename over it changes; on Windows none, and
/// its bytes alone are compared.
fn inode(path: &Path) -> u64 {
    #[cfg(unix)]
    return std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(path).unwrap());
    #[cfg(not(unix))]
    {
        let _ = path;
        0
    }
}

/// An image a replica took beside its file is renamed in only from the
/// file its follower wrote, the one `from_side`'s stores map: another put
/// at `.taking` since -- a file, whole and of an image too, or a link to
/// one -- is refused naming it, the copy's rows, its file and its inode as
/// they were, and the copy takes writes after, its disk not failed. Opened
/// again by its name, the file put there was renamed in -- the copy's file
/// another than the one its stores read -- and a link failed the copy for
/// good. A copy mapped and one read into memory alike. On Windows, a file
/// put there alone -- a link asks a privilege a test does not have -- held
/// off by its file index (`fs::still_named`): there the rename went by the
/// name, held by nothing, until the image review's second round.
#[test]
fn an_image_is_adopted_only_from_the_file_its_follower_wrote() {
    let d = dir("adopted");
    let image = image();
    for mapped in [true, false] {
        let path = d.join(format!("copy-{mapped}.fennec"));
        let mut db = match mapped {
            true => fennec_core::fs::open(&path).unwrap(),
            false => fennec_core::fs::open_in_memory(&path).unwrap(),
        };
        exec(&mut db, "create collection c (x int)");
        exec(&mut db, "put c {x: 1}");
        db.sync().unwrap();
        let planted_by: &[&str] = match cfg!(unix) {
            true => &["a file", "a link"],
            false => &["a file"],
        };
        for (n, &planted) in planted_by.iter().enumerate() {
            let what = format!("mapped {mapped}, {planted}");
            let held = rows(&db);
            let before = (std::fs::read(&path).unwrap(), inode(&path));
            let at = db
                .image_side()
                .expect("a file's database takes an image beside it");
            let mut side = SideFile::create(&at).unwrap();
            ImageOut::write(&mut side, &image).unwrap();
            side.sync().unwrap();
            let fresh = Database::from_side(&mut side, db.is_mapped()).unwrap();
            // Put at the name once the follower wrote and loaded it.
            let other = d.join(format!("other-{mapped}-{n}"));
            std::fs::write(&other, &image).unwrap();
            match planted {
                "a file" => std::fs::rename(&other, &at).unwrap(),
                _ => {
                    std::fs::remove_file(&at).unwrap();
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(&other, &at).unwrap();
                }
            }
            let e = db.adopt_side(fresh, side).expect_err(&what).to_string();
            assert!(e.contains("is no longer the file written"), "{what}: {e}");
            assert_eq!(rows(&db), held, "{what}: the copy holds what it held");
            assert!(
                (std::fs::read(&path).unwrap(), inode(&path)) == before,
                "{what}: the copy's file as it was"
            );
            db.refuse_if_failed()
                .unwrap_or_else(|e| panic!("{what}: the copy failed: {e}"));
            exec(&mut db, &format!("put c {{x: {}}}", 2 + n));
            db.sync().unwrap();
            let _ = std::fs::remove_file(&at);
            let _ = std::fs::remove_file(&other);
        }
        drop(db);
        let db = fennec_core::fs::open(&path).unwrap();
        let written: Vec<String> = (1..=1 + planted_by.len())
            .map(|x| format!("[Int({x})]"))
            .collect();
        assert_eq!(rows(&db), written, "mapped {mapped}");
    }
    let _ = std::fs::remove_dir_all(d);
}

/// A way a file is opened to be written.
type Open = fn(&Path) -> Result<Database>;

/// An image a crash left beside a database, whole or part taken, goes as
/// the database is opened to be written -- mapped, read into memory, a
/// server's open -- whatever opens it, a primary's or a copy's: up to the
/// image's review only a follower starting removed it, and a copy started
/// again as a primary kept a file as large as the database for good. A
/// tool that only looks (`open_read_only`) leaves it, as it leaves the
/// file.
#[test]
fn an_image_a_crash_left_goes_as_the_file_is_opened_to_be_written() {
    let d = dir("left");
    let image = image();
    let path = d.join("db.fennec");
    let mut db = fennec_core::fs::open(&path).unwrap();
    exec(&mut db, "create collection c (x int)");
    exec(&mut db, "put c {x: 1}");
    db.sync().unwrap();
    drop(db);
    let at = fennec_core::fs::beside(&path, "taking");
    let opens: [(&str, Open); 3] = [
        ("mapped", |p| fennec_core::fs::open(p)),
        ("in memory", |p| fennec_core::fs::open_in_memory(p)),
        ("a server's", |p| {
            fennec_core::fs::open_serving(p, true, Box::new(Ok))
        }),
    ];
    for (how, open) in opens {
        std::fs::write(&at, &image[..image.len() / 2]).unwrap();
        let looked = fennec_core::fs::open_read_only(&path).unwrap();
        assert_eq!(rows(&looked), ["[Int(1)]"], "{how}");
        drop(looked);
        assert!(at.exists(), "{how}: a tool that only looks removed it");
        let db = open(&path).unwrap();
        assert!(!at.exists(), "{how}: the open to write left it");
        assert_eq!(rows(&db), ["[Int(1)]"], "{how}: the database as it was");
    }
    let _ = std::fs::remove_dir_all(d);
}

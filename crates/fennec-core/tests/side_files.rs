//! The file beside a database a replica takes its primary's image into
//! (`Sink::image_side`, `<file>.taking`): renamed into the database's place
//! only where it is still the file its follower wrote, and one a crash left
//! there -- that one, a rewrite's `.compacting`, a compact's `.beside`, a
//! `.save`'s `.saving` (`fs::LEFT_BESIDE`) -- removed by whoever opens the
//! database to write it, its owner, under `<file>.lock`, never by a tool
//! that only looks.

#![cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]

use fennec_core::engine::ImageOut;
use fennec_core::fs::{beside, SideFile, LEFT_BESIDE};
use fennec_core::prelude::*;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::RwLock;

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

/// An image a crash left beside a database, whole or part written -- a
/// replica's `.taking`, a checkpoint's or a compact's `.compacting`, a
/// compact's or an index's `.beside`, the shell's `.saving` -- goes as the
/// database is opened to be written -- mapped, read into memory, a
/// server's open -- whatever
/// opens it, a primary's or a copy's: up to the image's review only a
/// follower starting removed `.taking`, and a copy started again as a
/// primary kept a file as large as the database for good; up to the P6
/// soak's second run a compact's stayed until the next compact. A tool
/// that only looks (`open_read_only`) leaves them, as it leaves the file.
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
    let opens: [(&str, Open); 3] = [
        ("mapped", |p| fennec_core::fs::open(p)),
        ("in memory", |p| fennec_core::fs::open_in_memory(p)),
        ("a server's", |p| {
            fennec_core::fs::open_serving(p, true, Box::new(Ok))
        }),
    ];
    for (how, open) in opens {
        for side in LEFT_BESIDE {
            std::fs::write(beside(&path, side), &image[..image.len() / 2]).unwrap();
        }
        let looked = fennec_core::fs::open_read_only(&path).unwrap();
        assert_eq!(rows(&looked), ["[Int(1)]"], "{how}");
        drop(looked);
        for side in LEFT_BESIDE {
            let at = beside(&path, side);
            assert!(at.exists(), "{how}: a tool that only looks removed .{side}");
        }
        let db = open(&path).unwrap();
        for side in LEFT_BESIDE {
            let at = beside(&path, side);
            assert!(!at.exists(), "{how}: the open to write left .{side}");
        }
        assert_eq!(rows(&db), ["[Int(1)]"], "{how}: the database as it was");
    }
    let _ = std::fs::remove_dir_all(d);
}

/// Where [`compacts_until_killed`] finds the file it compacts.
const COMPACT_IN: &str = "FENNEC_TEST_COMPACT_IN";

/// The code [`compacts_until_killed`] exits with, its compact stopped.
const KILLED: i32 = 86;

/// The other process [`a_compact_killed_mid_write_leaves_its_file_to_the_next_open`]
/// starts: a compact beside the database (`Database::maintain`, as a
/// server's `compact` and its auto-compact run it), stopped once its side
/// file is made and the copy it writes from taken. `process::exit` runs no
/// destructor, as a kill runs none: the side file stays as a crash leaves
/// it, where a compact that fails removes it as it is dropped. Nothing in a
/// test run of its own.
#[test]
fn compacts_until_killed() {
    let Some(path) = std::env::var_os(COMPACT_IN) else {
        return;
    };
    let db = RwLock::new(fennec_core::fs::open(&path).unwrap());
    let compact = fennec_ql::parse_one("compact").unwrap();
    let mut copies = 0;
    // The graphs' copy first, then the file's, taken once `.beside` is
    // made.
    let _ = Database::maintain_with(&db, &compact, &mut || {
        copies += 1;
        if copies == 2 {
            std::process::exit(KILLED);
        }
    });
    panic!("the compact ended before it copied its file");
}

/// A compact killed while it writes its file beside the database leaves
/// that file -- as large as the live data, 1.4 GB in the P6 soak's second
/// run -- and nothing removed it until the next compact, which the
/// auto-compact starts only once half the file is dead: the disk the
/// database takes, doubled meanwhile. The open that writes the file
/// removes it; a tool that only looks leaves it, and the rows are the
/// ones the compact found.
#[test]
fn a_compact_killed_mid_write_leaves_its_file_to_the_next_open() {
    let d = dir("killed");
    let path = d.join("db.fennec");
    let mut db = fennec_core::fs::open(&path).unwrap();
    exec(&mut db, "create collection c (x int)");
    for x in 1..=4 {
        exec(&mut db, &format!("put c {{x: {x}}}"));
    }
    exec(&mut db, "del c where x = 2");
    db.sync().unwrap();
    drop(db);
    let killed = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "side_files::compacts_until_killed"])
        .args(["--nocapture", "--test-threads=1"])
        .env(COMPACT_IN, &path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(killed.code(), Some(KILLED), "the compact was not stopped");
    let left = beside(&path, "beside");
    assert!(left.exists(), "the killed compact left no file beside");
    let held = ["[Int(1)]", "[Int(3)]", "[Int(4)]"];
    let looked = fennec_core::fs::open_read_only(&path).unwrap();
    assert_eq!(rows(&looked), held, "the database a tool looks at");
    drop(looked);
    assert!(left.exists(), "a tool that only looks removed it");
    let db = fennec_core::fs::open(&path).unwrap();
    assert!(!left.exists(), "the open to write left it");
    assert_eq!(rows(&db), held, "the database as the compact found it");
    drop(db);
    let _ = std::fs::remove_dir_all(d);
}

/// A link put at a side file's name goes as the file is opened to be
/// written, as a link: the file it names stays as it was (`unlink`
/// follows none). A directory there stays -- no file a process of ours
/// left -- and the database opens all the same.
#[cfg(unix)]
#[test]
fn a_link_at_a_side_files_name_goes_and_what_it_names_stays() {
    let d = dir("linked");
    let path = d.join("db.fennec");
    let mut db = fennec_core::fs::open(&path).unwrap();
    exec(&mut db, "create collection c (x int)");
    exec(&mut db, "put c {x: 1}");
    db.sync().unwrap();
    drop(db);
    let named = d.join("named");
    std::fs::write(&named, b"another file").unwrap();
    for side in LEFT_BESIDE {
        let at = beside(&path, side);
        std::os::unix::fs::symlink(&named, &at).unwrap();
        drop(fennec_core::fs::open(&path).unwrap());
        assert!(
            std::fs::symlink_metadata(&at).is_err(),
            "the link at .{side} stayed"
        );
        assert_eq!(
            std::fs::read(&named).unwrap(),
            b"another file",
            "the file the link at .{side} names"
        );
        std::fs::create_dir(&at).unwrap();
        let db = fennec_core::fs::open(&path).unwrap();
        assert!(at.is_dir(), "a directory at .{side} removed");
        assert_eq!(rows(&db), ["[Int(1)]"], "beside a directory at .{side}");
        drop(db);
        std::fs::remove_dir(&at).unwrap();
    }
    let _ = std::fs::remove_dir_all(d);
}

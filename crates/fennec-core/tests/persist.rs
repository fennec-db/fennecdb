//! Persistence: record framing and the id counter.
//!
//! The counter is derived from the records; since `compact` throws away the
//! tombstones the derivation falls short there, and the image carries the
//! counter explicitly (`REC_NEXTID`). The tests here hold both ends of that
//! gate: the highest deleted id must not come back, and collections must not
//! interfere with each other's counter.
//!
//! Framing as well: every record is laid out as
//! `[kind][collection-id][length][body]` and the read side *must* consume the
//! length. If it does not, the leftover byte is read as the next record kind
//! -- and the whole file becomes unopenable.

use fennec_core::prelude::*;

fn run(db: &mut Database, sql: &str) {
    for stmt in fennec_ql::parse(sql).expect("parse") {
        db.execute(&stmt).expect("execute");
    }
}

fn names(db: &mut Database) -> Vec<String> {
    let stmt = fennec_ql::parse_one("collections").expect("parse");
    let Response::Schemas(schemas) = db.execute(&stmt).expect("execute") else {
        panic!("expected a schema list");
    };
    schemas.iter().map(|s| s.name.clone()).collect()
}

fn ids(db: &mut Database, collection: &str) -> Vec<u64> {
    let stmt = fennec_ql::parse_one(&format!("get {collection}")).expect("parse");
    let Response::Rows(rs) = db.execute(&stmt).expect("execute") else {
        panic!("expected rows");
    };
    rs.rows.iter().map(|r| r.id).collect()
}

fn reload(db: &Database) -> Database {
    let mut fresh = Database::new();
    fresh.load(&db.snapshot()).expect("load");
    fresh
}

#[cfg(feature = "std-fs")]
fn tmp_path(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("fennecdb-persist-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("data.fennec");
    let _ = std::fs::remove_file(&path);
    (dir, path)
}

#[cfg(feature = "std-fs")]
fn cleanup(dir: std::path::PathBuf, path: std::path::PathBuf) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_dir(dir);
}

/// Once the document with the highest id is deleted and `compact` runs, that
/// id must never be handed out again -- not even after a restart.
#[test]
fn compaction_keeps_the_id_watermark() {
    let mut db = Database::new();
    run(&mut db, "create collection t (a text)");
    run(&mut db, r#"put t [{a: "one"}, {a: "two"}, {a: "three"}]"#);
    assert_eq!(ids(&mut db, "t"), vec![1, 2, 3]);

    run(&mut db, "del t where id = 3");
    run(&mut db, "compact");

    let mut db = reload(&db);
    run(&mut db, r#"put t {a: "four"}"#);
    assert_eq!(
        ids(&mut db, "t"),
        vec![1, 2, 4],
        "id 3 was handed out again"
    );
}

/// An externally supplied sparse id raises the watermark too, and survives
/// compaction.
#[test]
fn explicit_ids_raise_the_watermark_too() {
    let mut db = Database::new();
    run(&mut db, "create collection t (a text)");
    run(&mut db, r#"put t {id: 9000000000, a: "far"}"#);
    run(&mut db, "del t where id = 9000000000");
    run(&mut db, "compact");

    let mut db = reload(&db);
    run(&mut db, r#"put t {a: "new"}"#);
    assert_eq!(ids(&mut db, "t"), vec![9000000001]);
}

/// The counter is per collection: one must not shift another's ids.
#[test]
fn counters_are_per_collection() {
    let mut db = Database::new();
    run(&mut db, "create collection a (x text)");
    run(&mut db, "create collection b (x text)");
    run(&mut db, r#"put a [{x: "1"}, {x: "2"}, {x: "3"}]"#);
    run(&mut db, r#"put b {x: "1"}"#);
    run(&mut db, "del a where id = 3");
    run(&mut db, "compact");

    let mut db = reload(&db);
    run(&mut db, r#"put a {x: "new"}"#);
    run(&mut db, r#"put b {x: "new"}"#);
    assert_eq!(ids(&mut db, "a"), vec![1, 2, 4]);
    assert_eq!(ids(&mut db, "b"), vec![1, 2]);
}

/// A file with no counter record (WAL tail only, never compacted) must load
/// as before: there the counter is counted from the records.
#[cfg(feature = "std-fs")]
#[test]
fn images_without_the_counter_record_still_load() {
    let (dir, path) = tmp_path("wal");
    {
        let mut db = fennec_core::fs::open(&path).expect("open");
        run(&mut db, "create collection t (a text)");
        run(&mut db, r#"put t [{a: "one"}, {a: "two"}]"#);
        run(&mut db, "del t where id = 2");
        db.sync().expect("sync");
    }
    let mut db = fennec_core::fs::open(&path).expect("reopen");
    assert_eq!(ids(&mut db, "t"), vec![1]);
    run(&mut db, r#"put t {a: "three"}"#);
    assert_eq!(
        ids(&mut db, "t"),
        vec![1, 3],
        "the tombstone must carry the counter"
    );
    cleanup(dir, path);
}

/// The real restart path: compaction rewrites the file and later writes are
/// appended to the tail. Together they must still give the right counter.
#[cfg(feature = "std-fs")]
#[test]
fn watermark_survives_a_file_restart() {
    let (dir, path) = tmp_path("compact");
    {
        let mut db = fennec_core::fs::open(&path).expect("open");
        run(&mut db, "create collection t (a text)");
        run(&mut db, r#"put t [{a: "one"}, {a: "two"}, {a: "three"}]"#);
        run(&mut db, "del t where id = 3");
        run(&mut db, "compact");
        // The write that arrives *after* compaction lands in the tail.
        run(&mut db, r#"put t {a: "four"}"#);
        db.sync().expect("sync");
    }

    let mut db = fennec_core::fs::open(&path).expect("reopen");
    assert_eq!(ids(&mut db, "t"), vec![1, 2, 4]);
    run(&mut db, r#"put t {a: "five"}"#);
    assert_eq!(ids(&mut db, "t"), vec![1, 2, 4, 5]);

    cleanup(dir, path);
}

/// A crash in the middle of an append leaves the last record cut short, in
/// its body or its header. The open loads what came before it and cuts it
/// off the file: left there, it swallowed the first record appended after
/// it -- a write acknowledged, and gone on the open after -- and a header cut
/// short left the file unopenable.
#[cfg(feature = "std-fs")]
#[test]
fn a_record_cut_short_is_cut_off_before_the_next_write() {
    use std::io::Write;
    let torn: [(&str, &[u8]); 3] = [
        // A data record promising 100 bytes, 8 of them there.
        ("body", &[3, 1, 100, 0, 1, 2, 3, 4, 5, 6, 7, 8]),
        // Its kind and half its collection id.
        ("header", &[3, 0x81]),
        ("kind", &[3]),
    ];
    for (tag, bytes) in torn {
        let (dir, path) = tmp_path(&format!("torn-{tag}"));
        {
            let mut db = fennec_core::fs::open(&path).expect("open");
            run(&mut db, "create collection t (a text)");
            run(&mut db, r#"put t {a: "before"}"#);
            db.sync().expect("sync");
        }
        let whole = std::fs::metadata(&path).unwrap().len();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(bytes).unwrap();
        drop(f);
        {
            let mut db = fennec_core::fs::open(&path).expect("the open after the crash");
            assert_eq!(ids(&mut db, "t"), vec![1], "{tag}");
            assert_eq!(std::fs::metadata(&path).unwrap().len(), whole, "{tag}");
            run(&mut db, r#"put t {a: "after"}"#);
            db.sync().expect("sync");
        }
        let mut db = fennec_core::fs::open(&path).expect("the open after that");
        assert_eq!(ids(&mut db, "t"), vec![1, 2], "{tag}");
        cleanup(dir, path);
    }
}

/// Zeros from a record's head to the end are a torn tail too: an append
/// whose length reached the disk before its bytes, as Windows power-cut run
/// 42533808035 left one ("unknown record kind 0"). Cut as a torn tail is,
/// mapped or read into memory, after a tail or right after the image; a
/// read-only open reads up to them and cuts nothing.
#[cfg(feature = "std-fs")]
#[test]
fn zeros_to_the_end_are_cut_as_a_torn_tail() {
    use std::io::Write;
    for zeros in [1usize, 4096, 1 << 20] {
        for (way, tail) in [("mapped", true), ("in-memory", true), ("image", false)] {
            let tag = format!("zeros-{zeros}-{way}");
            let (dir, path) = tmp_path(&tag);
            let open = |p: &std::path::Path| match way {
                "in-memory" => fennec_core::fs::open_in_memory(p),
                _ => fennec_core::fs::open(p),
            };
            {
                let mut db = open(&path).expect("open");
                run(&mut db, "create collection t (a text)");
                run(&mut db, r#"put t {a: "one"}"#);
                db.checkpoint().expect("checkpoint");
                if tail {
                    run(&mut db, r#"put t {a: "two"}"#);
                }
                db.sync().expect("sync");
            }
            let rows: Vec<u64> = (1..=1 + tail as u64).collect();
            let whole = std::fs::metadata(&path).unwrap().len();
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(&vec![0; zeros]).unwrap();
            drop(f);
            let len = || std::fs::metadata(&path).unwrap().len();
            {
                let mut db = fennec_core::fs::open_read_only(&path).expect("read-only open");
                assert_eq!(ids(&mut db, "t"), rows, "{tag}");
            }
            assert_eq!(len(), whole + zeros as u64, "{tag}: the read-only open cut");
            {
                let mut db = open(&path).expect("the open after the cut");
                assert_eq!(ids(&mut db, "t"), rows, "{tag}");
                assert_eq!(len(), whole, "{tag}");
                run(&mut db, r#"put t {a: "after"}"#);
                db.sync().expect("sync");
            }
            let mut db = open(&path).expect("the open after that");
            let mut want = rows.clone();
            want.push(rows.len() as u64 + 1);
            assert_eq!(ids(&mut db, "t"), want, "{tag}");
            drop(db);
            cleanup(dir, path);
        }
    }
}

/// Zeros with anything after them are no torn tail, and zeros inside the
/// checkpoint image are damage, as a record cut short there is: each
/// refuses the open, naming the byte, and leaves the file as it was.
#[cfg(feature = "std-fs")]
#[test]
fn zeros_before_a_byte_or_inside_the_image_refuse_the_open() {
    let (dir, path) = tmp_path("zeros-refused");
    {
        let mut db = fennec_core::fs::open(&path).expect("open");
        run(&mut db, "create collection t (a text)");
        run(&mut db, r#"put t {a: "one"}"#);
        run(&mut db, r#"put t {a: "two"}"#);
        db.checkpoint().expect("checkpoint");
        run(&mut db, r#"put t {a: "three"}"#);
        db.sync().expect("sync");
    }
    let good = std::fs::read(&path).unwrap();
    let refused = |bad: &[u8], says: &str| {
        std::fs::write(&path, bad).unwrap();
        let err = fennec_core::fs::open(&path)
            .err()
            .unwrap_or_else(|| panic!("opened, where it {says}"));
        assert!(err.to_string().contains(says), "{err}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bad,
            "the open wrote to the file"
        );
    };
    for zeros in [1usize, 4096, 1 << 20] {
        let mut bad = good.clone();
        bad.resize(good.len() + zeros, 0);
        bad.push(3);
        refused(&bad, &format!("unknown record kind 0 at {}", good.len()));
    }
    // The image's data record, past the counter header: zeroed alone, the
    // tail's record after it; zeroed to the end, inside the image still --
    // or, after a zero byte, a head that is no kind.
    let mut pos = 8 + 17;
    let (data, end) = loop {
        let kind = good[pos] & 0x7f;
        let mut p = pos + 1;
        let _cid = varint(&good, &mut p);
        let len = varint(&good, &mut p) as usize;
        if kind == 3 {
            break (pos, p + len);
        }
        pos = p + len;
    };
    let mut bad = good.clone();
    bad[data..end].fill(0);
    refused(&bad, &format!("unknown record kind 0 at {data}"));
    let mut bad = good.clone();
    bad[data..].fill(0);
    let says = match good[data - 1] {
        0 => format!("unknown record kind 0 at {data}"),
        _ => format!("checkpoint image at {data}"),
    };
    refused(&bad, &says);

    std::fs::write(&path, &good).unwrap();
    let mut db = fennec_core::fs::open(&path).expect("the mended file");
    assert_eq!(ids(&mut db, "t"), vec![1, 2, 3]);
    drop(db);
    cleanup(dir, path);
}

/// Zeros from a 4 KiB boundary inside a record to the end, as a power cut
/// leaves a record whose head and first page reached the disk, in a file
/// written before records carried a checksum: the record's length still
/// fits, so it reads as whole, and the zero head after it follows a zero
/// byte -- the disk's, not a record's own. Cut there, the open went on and
/// read the big text back as NULs; it is refused, mapped and in memory, and
/// the file left as it was. A record with a checksum is cut instead
/// (`a_record_whose_end_the_disk_lost_is_cut`).
#[cfg(feature = "std-fs")]
#[test]
fn zeros_from_inside_a_record_without_a_checksum_refuse_the_open() {
    let (dir, path) = tmp_path("zeros-inside");
    {
        let mut db = fennec_core::fs::open(&path).expect("open");
        run(&mut db, "create collection t (a text)");
        run(&mut db, &format!(r#"put t {{a: "{}"}}"#, "x".repeat(8000)));
        run(&mut db, r#"put t {a: "small"}"#);
        db.sync().expect("sync");
    }
    let good = without_sums(&std::fs::read(&path).unwrap());
    let (big, next) = big_record(&good);
    let page = (big / 4096 + 1) * 4096;
    assert!(
        big + 16 < page && page < next && next < good.len(),
        "{big} {page} {next}"
    );
    let mut bad = good.clone();
    bad[page..].fill(0);
    for way in ["mapped", "in-memory"] {
        std::fs::write(&path, &bad).unwrap();
        let err = match way {
            "mapped" => fennec_core::fs::open(&path),
            _ => fennec_core::fs::open_in_memory(&path),
        }
        .err()
        .unwrap_or_else(|| panic!("{way}: opened a record with the disk's zeros in it"));
        let says = format!("unknown record kind 0 at {next}");
        assert!(err.to_string().contains(&says), "{way}: {err}");
        assert_eq!(std::fs::read(&path).unwrap(), bad, "{way}: the open wrote");
    }
    std::fs::write(&path, &good).unwrap();
    let mut db = fennec_core::fs::open(&path).expect("the mended file");
    assert_eq!(ids(&mut db, "t"), vec![1, 2]);
    drop(db);
    cleanup(dir, path);
}

/// Where the first record longer than 8 000 bytes starts in a file with no
/// image, and where it ends.
#[cfg(feature = "std-fs")]
fn big_record(file: &[u8]) -> (usize, usize) {
    let mut pos = 8;
    loop {
        let mut p = pos + 1;
        let _cid = varint(file, &mut p);
        let len = varint(file, &mut p) as usize;
        if len > 8000 {
            return (pos, p + len);
        }
        pos = p + len;
    }
}

/// A record whose length reached the disk and whose later bytes did not --
/// zeros from a page inside it, or stale bytes, to the end of the file, or
/// its middle lost and its end there -- fails its checksum, and with
/// nothing whole after it is a torn tail: cut, mapped and in memory, as a
/// record cut short is, and the next write lands where it began. Before
/// checksums it loaded as it read, or the open was refused.
#[cfg(feature = "std-fs")]
#[test]
fn a_record_whose_end_the_disk_lost_is_cut() {
    let (dir, path) = tmp_path("lost-end");
    {
        let mut db = fennec_core::fs::open(&path).expect("open");
        run(&mut db, "create collection t (a text)");
        run(&mut db, r#"put t {a: "first"}"#);
        run(&mut db, &format!(r#"put t {{a: "{}"}}"#, "x".repeat(12000)));
        run(&mut db, r#"put t {a: "small"}"#);
        db.sync().expect("sync");
    }
    let good = std::fs::read(&path).unwrap();
    let (big, next) = big_record(&good);
    let page = (big / 4096 + 1) * 4096;
    assert!(big + 16 < page && page + 4096 < next, "{big} {page} {next}");
    let lost: [(&str, Vec<u8>); 4] = [
        ("zeros to the end", {
            let mut b = good.clone();
            b[page..].fill(0);
            b
        }),
        ("stale bytes to the end", {
            let mut b = good.clone();
            b[page..].fill(0xee);
            b
        }),
        ("its middle, the last record", {
            let mut b = good[..next].to_vec();
            b[page..page + 4096].fill(0);
            b
        }),
        ("its last byte, the last record", {
            let mut b = good[..next].to_vec();
            b[next - 1] ^= 1;
            b
        }),
    ];
    for (what, bad) in lost {
        for way in ["mapped", "in-memory"] {
            std::fs::write(&path, &bad).unwrap();
            let open = |p: &std::path::Path| match way {
                "mapped" => fennec_core::fs::open(p),
                _ => fennec_core::fs::open_in_memory(p),
            };
            {
                let mut db = open(&path).unwrap_or_else(|e| panic!("{what}, {way}: {e}"));
                assert_eq!(ids(&mut db, "t"), vec![1], "{what}, {way}");
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    good[..big],
                    "{what}, {way}: cut where the record began"
                );
                run(&mut db, r#"put t {a: "after"}"#);
                db.sync().expect("sync");
            }
            let mut db = open(&path).expect("the open after that");
            assert_eq!(ids(&mut db, "t"), vec![1, 2], "{what}, {way}");
        }
    }
    cleanup(dir, path);
}

/// A record that fails its checksum with a whole record after it is no
/// torn tail -- a power cut's lost bytes have nothing whole after them --
/// but damage: the open is refused naming where it starts, mapped, in
/// memory and read-only, and the file left as it was. A graph a later one
/// replaces is never read, and the image's records are never checked by
/// an open: both open.
#[cfg(feature = "std-fs")]
#[test]
fn a_record_failing_its_checksum_before_a_whole_one_is_refused() {
    let (dir, path) = tmp_path("damaged");
    {
        let mut db = fennec_core::fs::open(&path).expect("open");
        run(&mut db, "create collection t (a text)");
        run(&mut db, r#"put t {a: "first"}"#);
        run(&mut db, &format!(r#"put t {{a: "{}"}}"#, "x".repeat(8000)));
        run(&mut db, r#"put t {a: "small"}"#);
        db.sync().expect("sync");
    }
    let good = std::fs::read(&path).unwrap();
    let (big, next) = big_record(&good);
    let mut bad = good.clone();
    bad[big + 4096] ^= 0x10;
    assert!(big + 4096 < next);
    let says = format!("the record at {big} fails its checksum, with whole records after it");
    for way in ["mapped", "in-memory", "read-only"] {
        std::fs::write(&path, &bad).unwrap();
        let err = match way {
            "mapped" => fennec_core::fs::open(&path),
            "in-memory" => fennec_core::fs::open_in_memory(&path),
            _ => fennec_core::fs::open_read_only(&path),
        }
        .err()
        .unwrap_or_else(|| panic!("{way}: opened a damaged record"));
        assert!(err.to_string().contains(&says), "{way}: {err}");
        assert_eq!(std::fs::read(&path).unwrap(), bad, "{way}: the open wrote");
    }
    // The image's data record, damaged the same way, is not read by an open.
    std::fs::write(&path, &good).unwrap();
    {
        let mut db = fennec_core::fs::open(&path).expect("open");
        db.checkpoint().expect("checkpoint");
    }
    let image = std::fs::read(&path).unwrap();
    let at = image
        .windows(8000)
        .position(|w| w.iter().all(|&b| b == b'x'))
        .unwrap();
    let mut bad = image.clone();
    bad[at + 100] = b'y';
    std::fs::write(&path, &bad).unwrap();
    let mut db = fennec_core::fs::open(&path).expect("an image is not checked by an open");
    assert_eq!(ids(&mut db, "t"), vec![1, 2, 3]);
    drop(db);
    let err = fennec_core::engine::check_records(&bad).expect_err("check_records reads it");
    assert!(err.to_string().contains("fails its checksum"), "{err}");
    fennec_core::engine::check_records(&image).expect("the image whole");
    cleanup(dir, path);
}

/// After a record whose checksum holds, zeros from the next head to the end
/// are a torn tail whatever that record's last byte: the record ends where
/// it says. Without a checksum, a zero last byte left the zeros refused
/// (`a_zero_head_after_a_zero_byte_without_a_checksum_is_cut_only_at_the_image_end`).
#[cfg(feature = "std-fs")]
#[test]
fn zeros_after_a_checked_record_are_cut_whatever_its_last_byte() {
    use std::io::Write;
    let (dir, path) = tmp_path("zero-sum");
    let mut db = fennec_core::fs::open(&path).expect("open");
    run(&mut db, "create collection t (n int)");
    // A record whose checksum's last byte is 0: one in 256.
    let mut n = 0;
    loop {
        run(&mut db, &format!("put t {{n: {n}}}"));
        // Into the file, with no fsync: the bytes are what is looked at.
        db.write_out().expect("write out");
        if std::fs::read(&path).unwrap().last() == Some(&0) {
            break;
        }
        n += 1;
        assert!(n < 5000, "no record ended in a zero byte");
    }
    drop(db);
    let whole = std::fs::read(&path).unwrap();
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    f.write_all(&[0; 4096]).unwrap();
    drop(f);
    let mut db = fennec_core::fs::open(&path).expect("zeros after a checked record");
    assert_eq!(ids(&mut db, "t").len(), n + 1);
    assert_eq!(std::fs::read(&path).unwrap(), whole, "cut");
    drop(db);
    cleanup(dir, path);
}

/// A file written before records carried checksums opens as it did, its
/// image and its tail, mapped and in memory, and takes writes, which carry
/// one: a file of both kinds opens.
#[cfg(feature = "std-fs")]
#[test]
fn a_file_without_checksums_opens_and_takes_checked_records() {
    let (dir, path) = tmp_path("before-sums");
    {
        let mut db = fennec_core::fs::open(&path).expect("open");
        run(&mut db, "create collection t (a text, n int @sorted)");
        run(&mut db, "create collection u (b text @hash)");
        run(&mut db, r#"put t [{a: "one", n: 1}, {a: "two", n: 2}]"#);
        db.checkpoint().expect("checkpoint");
        run(&mut db, r#"put t {a: "three", n: 3}"#);
        run(&mut db, "del t where n = 1");
        run(&mut db, r#"put u {b: "x"}"#);
        db.sync().expect("sync");
    }
    let checked = std::fs::read(&path).unwrap();
    let plain = without_sums(&checked);
    assert!(plain.len() < checked.len());
    for way in ["mapped", "in-memory"] {
        std::fs::write(&path, &plain).unwrap();
        let open = |p: &std::path::Path| match way {
            "mapped" => fennec_core::fs::open(p),
            _ => fennec_core::fs::open_in_memory(p),
        };
        {
            let mut db = open(&path).unwrap_or_else(|e| panic!("{way}: {e}"));
            assert_eq!(ids(&mut db, "t"), vec![2, 3], "{way}");
            assert_eq!(ids(&mut db, "u"), vec![1], "{way}");
            assert_eq!(std::fs::read(&path).unwrap(), plain, "{way}: not cut");
            run(&mut db, r#"put t {a: "four", n: 4}"#);
            db.sync().expect("sync");
        }
        let both = std::fs::read(&path).unwrap();
        assert_eq!(&both[..plain.len()], &plain[..]);
        assert_eq!(
            both[plain.len()] & 0x80,
            0x80,
            "{way}: the new record is checked"
        );
        let mut db = open(&path).expect("a file of both kinds");
        assert_eq!(ids(&mut db, "t"), vec![2, 3, 4], "{way}");
        run(&mut db, "compact");
        drop(db);
        fennec_core::engine::check_records(&std::fs::read(&path).unwrap())
            .expect("the compacted file is checked whole");
        let mut db = open(&path).expect("compacted");
        assert_eq!(ids(&mut db, "t"), vec![2, 3, 4], "{way}");
    }
    cleanup(dir, path);
}

/// `file` as a version before checksums wrote it: each record's checksum
/// and its kind's bit taken off, its length its body's, and the image's
/// length put right -- an old file, made of what this build wrote.
#[cfg(feature = "std-fs")]
fn without_sums(file: &[u8]) -> Vec<u8> {
    let mut out = file[..8].to_vec();
    let mut pos = 8;
    // Where the image's length is in `out`, and where the image ends in
    // `file`.
    let mut image: Option<(usize, usize)> = None;
    while pos < file.len() {
        if file[pos] == 6 {
            let len = u64::from_le_bytes(file[pos + 9..pos + 17].try_into().unwrap());
            out.extend_from_slice(&file[pos..pos + 17]);
            image = Some((out.len() - 8, pos + 17 + len as usize));
            pos += 17;
        } else {
            let kind = file[pos];
            let mut p = pos + 1;
            let cid = varint(file, &mut p);
            let len = varint(file, &mut p) as usize;
            let sum = if kind & 0x80 != 0 { 4 } else { 0 };
            let body = &file[p..p + len - sum];
            out.push(kind & 0x7f);
            fennec_core::codec::put_uvarint(&mut out, cid);
            fennec_core::codec::put_uvarint(&mut out, body.len() as u64);
            out.extend_from_slice(body);
            pos = p + len;
        }
        if let Some((at, end)) = image {
            if pos == end {
                let n = (out.len() - (at + 8)) as u64;
                out[at..at + 8].copy_from_slice(&n.to_le_bytes());
            }
        }
    }
    out
}

/// In a file written before checksums, a record may end in a zero byte of
/// its own -- here an int 0 -- and a zero head after one cannot be told
/// from the disk's zeros starting inside it: cut only at the image's end,
/// which a whole image is renamed into place to be whatever its last byte;
/// in the tail, refused.
#[cfg(feature = "std-fs")]
#[test]
fn a_zero_head_after_a_zero_byte_without_a_checksum_is_cut_only_at_the_image_end() {
    use std::io::Write;
    for way in ["mapped", "in-memory"] {
        let (dir, path) = tmp_path(&format!("zero-byte-{way}"));
        let open = |p: &std::path::Path| match way {
            "mapped" => fennec_core::fs::open(p),
            _ => fennec_core::fs::open_in_memory(p),
        };
        let zeros = |p: &std::path::Path| {
            let mut f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
            f.write_all(&[0; 4096]).unwrap();
        };
        let unsum = |p: &std::path::Path| {
            let plain = without_sums(&std::fs::read(p).unwrap());
            std::fs::write(p, plain).unwrap();
        };
        {
            let mut db = open(&path).expect("open");
            run(&mut db, "create collection t (n int)");
            run(&mut db, "put t {n: 0}");
            db.checkpoint().expect("checkpoint");
            db.sync().expect("sync");
        }
        unsum(&path);
        let image = std::fs::read(&path).unwrap();
        assert_eq!(image.last(), Some(&0), "{way}: the image's last byte");
        zeros(&path);
        {
            let mut db = open(&path).expect("zeros at the image's end");
            assert_eq!(ids(&mut db, "t"), vec![1], "{way}");
            assert_eq!(std::fs::read(&path).unwrap(), image, "{way}: not cut");
            run(&mut db, "put t {n: 0}");
            db.sync().expect("sync");
        }
        unsum(&path);
        let tail = std::fs::read(&path).unwrap();
        assert_eq!(tail.last(), Some(&0), "{way}: the tail's last byte");
        zeros(&path);
        let bad = std::fs::read(&path).unwrap();
        let err = open(&path).err().expect("a zero head after a zero byte");
        let says = format!("unknown record kind 0 at {}", tail.len());
        assert!(err.to_string().contains(&says), "{way}: {err}");
        assert_eq!(std::fs::read(&path).unwrap(), bad, "{way}: the open wrote");
        cleanup(dir, path);
    }
}

/// A record cut short inside the checkpoint image is not a crash's torn
/// tail -- an image is written beside the file and renamed over it whole --
/// but a damaged or truncated file. The open refuses it and leaves every
/// byte where it was: cut there as a torn tail is, one flipped bit in an
/// image's length deleted every record after it, intact ones included.
#[cfg(feature = "std-fs")]
#[test]
fn a_record_cut_short_inside_the_image_refuses_the_open_and_cuts_nothing() {
    let (dir, path) = tmp_path("torn-image");
    {
        let mut db = fennec_core::fs::open(&path).expect("open");
        run(&mut db, "create collection t (a text)");
        run(&mut db, r#"put t {a: "one"}"#);
        run(&mut db, r#"put t {a: "two"}"#);
        db.checkpoint().expect("checkpoint");
        run(&mut db, r#"put t {a: "three"}"#);
        db.sync().expect("sync");
    }
    let good = std::fs::read(&path).unwrap();
    // The counter header, then the image's records: find its data record
    // and have its length promise more than the file holds.
    let mut pos = 8 + 17;
    let data = loop {
        let kind = good[pos];
        let mut p = pos + 1;
        let _cid = varint(&good, &mut p);
        let len_at = p;
        let len = varint(&good, &mut p);
        if kind & 0x7f == 3 {
            break len_at;
        }
        pos = p + len as usize;
    };
    assert_eq!(good[data] & 0x80, 0, "a one-byte length");
    let mut bad = good.clone();
    bad[data] = 0x7f;
    std::fs::write(&path, &bad).unwrap();
    let err = fennec_core::fs::open(&path)
        .err()
        .expect("the damaged image opened");
    assert!(err.to_string().contains("checkpoint image"), "{err}");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bad,
        "the open wrote to the file"
    );

    // A file cut between two of the image's records: every record there is
    // whole, and only the image's stated length can tell.
    std::fs::write(&path, &good[..data - 2]).unwrap();
    let err = fennec_core::fs::open(&path)
        .err()
        .expect("the truncated image opened");
    assert!(err.to_string().contains("checkpoint image"), "{err}");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), (data - 2) as u64);

    // Mended, every row is there, the one after the image included.
    std::fs::write(&path, &good).unwrap();
    let mut db = fennec_core::fs::open(&path).expect("the mended file");
    assert_eq!(ids(&mut db, "t"), vec![1, 2, 3]);
    cleanup(dir, path);
}

#[cfg(feature = "std-fs")]
fn varint(b: &[u8], p: &mut usize) -> u64 {
    let (mut v, mut shift) = (0u64, 0);
    loop {
        let byte = b[*p];
        *p += 1;
        v |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return v;
        }
        shift += 7;
    }
}

/// A tool that only looks at a file -- `fennec types` -- opens it read-only:
/// a server may be in the middle of appending to it, and from outside that
/// looks like a torn last record. Nothing is cut, created or written, and a
/// write is refused.
#[cfg(feature = "std-fs")]
#[test]
fn a_read_only_open_leaves_the_file_as_it_is() {
    use std::io::Write;
    let (dir, path) = tmp_path("read-only");
    {
        let mut db = fennec_core::fs::open(&path).expect("open");
        run(&mut db, "create collection t (a text)");
        run(&mut db, r#"put t {a: "before"}"#);
        db.sync().expect("sync");
    }
    // A record the writer has only half appended.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    f.write_all(&[3, 1, 100, 0, 1, 2]).unwrap();
    drop(f);
    let before = std::fs::read(&path).unwrap();
    let mut db = fennec_core::fs::open_read_only(&path).expect("read-only open");
    assert_eq!(ids(&mut db, "t"), vec![1]);
    let err = db
        .execute(&fennec_ql::parse_one(r#"put t {a: "no"}"#).unwrap())
        .expect_err("a write went through");
    assert!(
        matches!(err, fennec_core::error::Error::ReadOnly(_)),
        "{err}"
    );
    drop(db);
    assert_eq!(std::fs::read(&path).unwrap(), before);

    let missing = dir.join("missing.fennec");
    assert!(fennec_core::fs::open_read_only(&missing).is_err());
    assert!(!missing.exists(), "the read-only open created the file");
    cleanup(dir, path);
}

/// Dropping a collection must not make the file unopenable.
///
/// The `drop` record carries a length field like every other one (even
/// though its body is empty). If the read side does not skip it, the
/// leftover `0` byte is read as a record kind and the *whole* database fails
/// to open with "unknown record kind 0". Since the snapshot path never
/// writes a `drop` record, it fixes itself after a `compact` or a
/// `checkpoint`; the bug only shows up in the raw tail.
#[cfg(feature = "std-fs")]
#[test]
fn dropping_a_collection_keeps_the_file_readable() {
    let (dir, path) = tmp_path("drop");
    {
        let mut db = fennec_core::fs::open(&path).expect("open");
        run(&mut db, "create collection a (x text)");
        run(&mut db, r#"put a {x: "one"}"#);
        run(&mut db, "drop collection a");
        run(&mut db, "create collection b (y text)");
        run(&mut db, r#"put b {y: "two"}"#);
        db.sync().expect("sync");
    }

    let mut db = fennec_core::fs::open(&path).expect("reopen");
    assert_eq!(names(&mut db), vec!["b".to_string()]);
    assert_eq!(ids(&mut db, "b"), vec![1]);
    cleanup(dir, path);
}

/// A database opened through a symbolic link -- one made before its file,
/// relative, as `ln -s` makes one -- is the file the link leads to
/// (`fs::resolved`): a compact and a checkpoint rename their image over
/// that file and leave the link a link. Renamed over the link, the image
/// was a file of its own, and a second opener through the link wrote
/// beside the first. On Windows a file's link needs
/// `SeCreateSymbolicLinkPrivilege`, which CI's account holds and a
/// developer's without Developer Mode may not: there the test says so.
#[cfg(all(any(unix, windows), feature = "std-fs"))]
#[test]
fn a_rewrite_through_a_link_renames_over_the_file_it_leads_to() {
    let (dir, real) = tmp_path("link");
    let link = dir.join("link.fennec");
    let _ = std::fs::remove_file(&link);
    #[cfg(unix)]
    std::os::unix::fs::symlink("data.fennec", &link).expect("link");
    #[cfg(windows)]
    if let Err(e) = std::os::windows::fs::symlink_file("data.fennec", &link) {
        // ERROR_PRIVILEGE_NOT_HELD
        assert_eq!(e.raw_os_error(), Some(1314), "{e}");
        println!("no symbolic link made here, the privilege not held: {e}");
        return;
    }
    assert_eq!(fennec_core::fs::resolved(&link), real);
    let is_link = || {
        std::fs::symlink_metadata(&link)
            .expect("link")
            .file_type()
            .is_symlink()
    };
    let mut db = fennec_core::fs::open(&link).expect("open");
    run(
        &mut db,
        "create collection n (i int); put n {i: 1}; put n {i: 2}; del n where i = 1; compact",
    );
    assert!(is_link(), "the compact renamed over the link");
    run(&mut db, "put n {i: 3}");
    db.checkpoint().expect("checkpoint");
    assert!(is_link(), "the checkpoint renamed over the link");
    run(&mut db, "put n {i: 4}");
    drop(db);
    let mut db = fennec_core::fs::open(&real).expect("open");
    assert_eq!(ids(&mut db, "n").len(), 3);
    drop(db);
    let _ = std::fs::remove_file(link);
    cleanup(dir, real);
}

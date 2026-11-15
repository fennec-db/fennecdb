//! Verifies the SQLite reader against real files.
//!
//! The fixtures are produced with the system `sqlite3` binary, so what we
//! read is guaranteed to be the format SQLite itself writes rather than our
//! own assumptions. When the binary is missing the tests are skipped --
//! fennec-import does not need `sqlite3` at runtime, only these tests do.

use fennec_core::prelude::*;
use fennec_core::query::Statement as Stmt;
use fennec_import::sqlite::Reader;
use fennec_import::{load, map, IdSource, Options, Source};
use std::path::{Path, PathBuf};
use std::process::Command;

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("fennecimport-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Produces a fixture. `None` means the system has no `sqlite3`.
fn make(name: &str, sql: &str) -> Option<PathBuf> {
    let path = dir().join(format!("{name}.sqlite"));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    match Command::new("sqlite3").arg(&path).arg(sql).output() {
        Err(_) => None,
        Ok(o) if !o.status.success() => {
            panic!("sqlite3 failed: {}", String::from_utf8_lossy(&o.stderr))
        }
        Ok(_) => Some(path),
    }
}

/// Sets up the fixture, or skips the test when `sqlite3` is missing (a panic under CI).
macro_rules! fixture {
    ($name:expr, $sql:expr) => {
        match make($name, $sql) {
            Some(p) => p,
            None => {
                // A runner that skipped these reported a green job that read
                // no SQLite file; GitHub's images ship sqlite3.
                assert!(
                    std::env::var_os("CI").is_none_or(|v| v.is_empty()),
                    "sqlite3 not found under CI: `{}` would test nothing",
                    $name
                );
                eprintln!("sqlite3 not found, `{}` skipped", $name);
                return;
            }
        }
    };
}

fn rows(path: &Path, table: &str) -> Vec<Vec<Value>> {
    let mut r = Reader::open(path, table).unwrap();
    let mut out = Vec::new();
    while let Some(row) = r.next_row().unwrap() {
        out.push(row);
    }
    out
}

#[test]
fn every_storage_class_round_trips() {
    let db = fixture!(
        "types",
        "create table t (
           a integer, b real, c text, d blob, e integer, f text
         );
         insert into t values (42, 2.5, 'hello world', x'00ff10', null, null);
         insert into t values (-1, -0.5, '', x'', 7, 'last');"
    );
    let got = rows(&db, "t");
    assert_eq!(got.len(), 2);
    assert_eq!(
        got[0],
        vec![
            Value::Int(42),
            Value::Float(2.5),
            Value::Text("hello world".into()),
            Value::Bytes(vec![0x00, 0xff, 0x10]),
            Value::Null,
            Value::Null,
        ]
    );
    assert_eq!(got[1][0], Value::Int(-1));
    assert_eq!(got[1][1], Value::Float(-0.5));
    assert_eq!(got[1][3], Value::Bytes(vec![]));
}

/// Integers are stored with 1, 2, 3, 4, 6 and 8 byte serial types, all of
/// them signed. Missing one of them silently produces a wrong number.
#[test]
fn integers_of_every_width() {
    let db = fixture!(
        "widths",
        "create table t (v integer);
         insert into t values (0), (1), (127), (-128), (32767), (-32768),
           (8388607), (-8388608), (2147483647), (-2147483648),
           (140737488355327), (-140737488355328),
           (9223372036854775807), (-9223372036854775808);"
    );
    let got: Vec<i64> = rows(&db, "t")
        .into_iter()
        .map(|r| match r[0] {
            Value::Int(n) => n,
            ref other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        got,
        vec![
            0,
            1,
            127,
            -128,
            32767,
            -32768,
            8388607,
            -8388608,
            2147483647,
            -2147483648,
            140737488355327,
            -140737488355328,
            i64::MAX,
            i64::MIN,
        ]
    );
}

/// `INTEGER PRIMARY KEY` is stored as NULL in the record; the real value is
/// the cell's rowid. Without substituting it, every row loses its id.
#[test]
fn rowid_alias_is_substituted() {
    let db = fixture!(
        "rowid",
        "create table t (id integer primary key, name text);
         insert into t values (10, 'ten'), (20, 'twenty');
         insert into t (name) values ('automatic');"
    );
    let got = rows(&db, "t");
    assert_eq!(got[0][0], Value::Int(10));
    assert_eq!(got[1][0], Value::Int(20));
    assert_eq!(got[2][0], Value::Int(21), "an automatic rowid follows 20");
}

/// A payload that does not fit the page spreads over the overflow chain. If
/// the chain is not walked correctly, text is cut off in the middle.
#[test]
fn overflow_chain_is_followed() {
    let db = fixture!(
        "overflow",
        "create table t (long text, huge blob);
         insert into t values (
           replace(hex(zeroblob(20000)), '0', 'x'),
           zeroblob(50000)
         );"
    );
    let got = rows(&db, "t");
    let Value::Text(s) = &got[0][0] else { panic!() };
    assert_eq!(s.len(), 40000, "the overflowing text was truncated");
    assert!(s.chars().all(|c| c == 'x'));
    let Value::Bytes(b) = &got[0][1] else {
        panic!()
    };
    assert_eq!(b.len(), 50000);
    assert!(b.iter().all(|&x| x == 0));
}

/// A table that does not fit in one page produces interior nodes; if the
/// walker does not descend through them, most rows stay invisible.
#[test]
fn interior_pages_are_descended() {
    let db = fixture!(
        "large",
        "create table t (id integer primary key, v text);
         insert into t (v) with recursive c(x) as (
           select 1 union all select x + 1 from c where x < 50000
         ) select 'row ' || x from c;"
    );
    let got = rows(&db, "t");
    assert_eq!(got.len(), 50_000);
    // The rowid order must be preserved.
    assert_eq!(got[0][0], Value::Int(1));
    assert_eq!(got[0][1], Value::Text("row 1".into()));
    assert_eq!(got[49_999][0], Value::Int(50_000));
    assert_eq!(got[49_999][1], Value::Text("row 50000".into()));
}

#[test]
fn empty_table_yields_no_rows() {
    let db = fixture!("empty", "create table t (a int, b text);");
    assert!(rows(&db, "t").is_empty());
    let mut r = Reader::open(&db, "t").unwrap();
    assert_eq!(r.columns().unwrap().len(), 2);
}

/// After an `ALTER TABLE ADD COLUMN` the new column is absent from older
/// records entirely; it has to be filled in as NULL.
#[test]
fn columns_added_later_read_as_null() {
    let db = fixture!(
        "alter",
        "create table t (a int);
         insert into t values (1);
         alter table t add column b text;
         insert into t values (2, 'there');"
    );
    let got = rows(&db, "t");
    assert_eq!(got[0], vec![Value::Int(1), Value::Null]);
    assert_eq!(got[1], vec![Value::Int(2), Value::Text("there".into())]);
}

/// Untyped columns are resolved from a value sample.
#[test]
fn untyped_columns_are_inferred_from_values() {
    let db = fixture!(
        "untyped",
        "create table t (a, b, c);
         insert into t values (1, 'text', 1.5);
         insert into t values (2, 'more', 2.5);"
    );
    let mut r = Reader::open(&db, "t").unwrap();
    let cols = r.columns().unwrap();
    assert_eq!(cols[0].ty, Some(DataType::Int));
    assert_eq!(cols[1].ty, Some(DataType::Text));
    assert_eq!(cols[2].ty, Some(DataType::Float));
    assert!(cols[0].note.as_ref().unwrap().contains("no declared type"));
}

#[test]
fn without_rowid_tables_are_rejected_clearly() {
    let db = fixture!(
        "wr",
        "create table t (a text primary key, b int) without rowid;
         insert into t values ('x', 1);"
    );
    // Refused as the table is opened, by its `CREATE` text, so `--dry-run`
    // says so too.
    let e = Reader::open(&db, "t").unwrap_err().to_string();
    assert!(e.contains("`t`") && e.contains("WITHOUT ROWID"), "{e}");
}

/// When an unprocessed WAL exists the main file shows stale data; stopping
/// beats silently returning it.
#[test]
fn pending_wal_is_refused() {
    let db = fixture!("wal", "create table t (a int); insert into t values (1);");
    let wal = format!("{}-wal", db.display());
    std::fs::write(&wal, vec![0u8; 512]).unwrap();
    let e = Reader::open(&db, "t").unwrap_err().to_string();
    assert!(e.contains("wal_checkpoint"), "{e}");
    let _ = std::fs::remove_file(&wal);
    // An empty WAL is not a blocker.
    std::fs::write(&wal, b"").unwrap();
    assert!(Reader::open(&db, "t").is_ok());
    let _ = std::fs::remove_file(&wal);
}

#[test]
fn missing_table_lists_what_exists() {
    let db = fixture!(
        "missing",
        "create table alpha (a int); create table beta (b int);"
    );
    let e = Reader::open(&db, "gamma").unwrap_err().to_string();
    assert!(e.contains("alpha, beta"), "{e}");
}

// ------------------------------------------------------- hostile files

/// SQLite's varint (big-endian, 7 bits a byte, the ninth byte all 8).
fn sqlite_varint(v: u64) -> Vec<u8> {
    if v >> 56 != 0 {
        let mut out: Vec<u8> = (0..8).map(|i| 0x80 | (v >> (57 - 7 * i)) as u8).collect();
        out.push(v as u8);
        return out;
    }
    let mut groups = vec![(v & 0x7f) as u8];
    let mut rest = v >> 7;
    while rest > 0 {
        groups.push(0x80 | (rest & 0x7f) as u8);
        rest >>= 7;
    }
    groups.reverse();
    groups
}

/// A file of 512-byte pages made by hand: page 1 `sqlite_master`, its one
/// row table `t` (`sql`) rooted at page 2, then `pages` from page 2 on.
/// `reserved` is the header's reserved bytes a page.
fn hand_made(sql: &str, reserved: u8, pages: &[Vec<u8>]) -> Vec<u8> {
    hand_made_of(512, sql, reserved, pages)
}

/// [`hand_made`], of pages of `size` bytes: 65 536 is the header's 1.
fn hand_made_of(size: usize, sql: &str, reserved: u8, pages: &[Vec<u8>]) -> Vec<u8> {
    let mut f = vec![0u8; size];
    f[..16].copy_from_slice(b"SQLite format 3\0");
    let field = if size == 65_536 { 1 } else { size as u16 };
    f[16..18].copy_from_slice(&field.to_be_bytes());
    f[18] = 1;
    f[19] = 1;
    f[20] = reserved;
    f[21..24].copy_from_slice(&[64, 32, 32]);
    f[28..32].copy_from_slice(&(1 + pages.len() as u32).to_be_bytes());
    f[56..60].copy_from_slice(&1u32.to_be_bytes());
    // The row (type, name, tbl_name, rootpage, sql): its header -- its own
    // length, then a serial type a column: text of 5, 1 and 1 bytes, an
    // 8-bit int, the text -- then the values.
    let mut record = vec![0, 23, 15, 15, 1];
    record.extend_from_slice(&sqlite_varint(13 + 2 * sql.len() as u64));
    record[0] = record.len() as u8;
    record.extend_from_slice(b"tablett");
    record.push(2);
    record.extend_from_slice(sql.as_bytes());
    let mut cell = sqlite_varint(record.len() as u64);
    cell.push(1);
    cell.extend_from_slice(&record);
    let at = size - cell.len();
    f[at..].copy_from_slice(&cell);
    f[100] = 0x0d;
    f[103..105].copy_from_slice(&1u16.to_be_bytes());
    f[105..107].copy_from_slice(&(at as u16).to_be_bytes());
    f[108..110].copy_from_slice(&(at as u16).to_be_bytes());
    for p in pages {
        assert_eq!(p.len(), size);
        f.extend_from_slice(p);
    }
    f
}

/// An interior table page with no cells, its right-most child `child`.
fn interior(child: u32) -> Vec<u8> {
    let mut p = vec![0u8; 512];
    p[0] = 0x05;
    p[5..7].copy_from_slice(&512u16.to_be_bytes());
    p[8..12].copy_from_slice(&child.to_be_bytes());
    p
}

fn written(name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir().join(format!("{name}.sqlite"));
    std::fs::write(&path, bytes).unwrap();
    path
}

fn refused(path: &Path, why: &str) {
    let mut r = Reader::open(path, "t").unwrap();
    let e = r.next_row().unwrap_err().to_string();
    assert!(e.contains(why), "{e}");
    let e = fennec_import::sqlite::count_rows(path, "t")
        .unwrap_err()
        .to_string();
    assert!(e.contains(why), "{e}");
}

/// An interior page naming itself, or a page above it, as a child is a
/// b-tree that loops: refused at the page reached twice, where the walk
/// went round for ever, its stack growing a page's cells each time.
#[test]
fn a_page_reached_twice_is_refused_not_walked_for_ever() {
    let sql = "CREATE TABLE t (a INTEGER)";
    let itself = written("loop-self", &hand_made(sql, 0, &[interior(2)]));
    refused(&itself, "page 2 is reached twice");
    let above = written(
        "loop-above",
        &hand_made(sql, 0, &[interior(3), interior(2)]),
    );
    refused(&above, "page 2 is reached twice");
    let past = written("past-end", &hand_made(sql, 0, &[interior(9)]));
    refused(&past, "page 9 is past the file's 2 pages");
}

/// A cell's payload length is a varint of the file's: one past the file's
/// length is refused before a byte is allocated for it, where
/// `Vec::with_capacity` of 2^62 bytes ended the import.
#[test]
fn a_payload_longer_than_the_file_is_refused_before_it_is_allocated() {
    for declared in [1u64 << 62, u64::MAX >> 1, 1 << 40, 4096] {
        let mut leaf = vec![0u8; 512];
        leaf[0] = 0x0d;
        leaf[3..5].copy_from_slice(&1u16.to_be_bytes());
        let mut cell = sqlite_varint(declared);
        cell.push(1);
        let at = 512 - cell.len() - 8;
        leaf[5..7].copy_from_slice(&(at as u16).to_be_bytes());
        leaf[8..10].copy_from_slice(&(at as u16).to_be_bytes());
        leaf[at..at + cell.len()].copy_from_slice(&cell);
        let path = written(
            "huge-payload",
            &hand_made("CREATE TABLE t (a INTEGER)", 0, &[leaf]),
        );
        refused(&path, "is longer than the file, 1024 bytes");
    }
}

/// SQLite refuses a page whose usable part is under 480 bytes, and so does
/// the reader: the overflow arithmetic stands on it.
#[test]
fn a_page_under_sqlites_usable_floor_is_refused() {
    let path = written(
        "reserved",
        &hand_made("CREATE TABLE t (a INTEGER)", 33, &[interior(2)]),
    );
    let e = Reader::open(&path, "t").unwrap_err().to_string();
    assert!(
        e.contains("usable part is 479 bytes, under SQLite's 480"),
        "{e}"
    );
}

/// A table leaf of 512 bytes holding `cells`, laid from the page's end
/// down, their pointers in order.
fn leaf(cells: &[Vec<u8>]) -> Vec<u8> {
    let mut at = 512;
    let mut placed = Vec::new();
    for c in cells {
        at -= c.len();
        placed.push(at);
    }
    raw_leaf(
        &cells
            .iter()
            .zip(&placed)
            .map(|(c, &at)| (at, c.as_slice()))
            .collect::<Vec<_>>(),
        &placed,
    )
}

/// A table leaf of 512 bytes with `bytes` laid where each says and
/// `pointers` as its cell pointers, whatever they point at.
fn raw_leaf(bytes: &[(usize, &[u8])], pointers: &[usize]) -> Vec<u8> {
    let mut p = vec![0u8; 512];
    p[0] = 0x0d;
    p[3..5].copy_from_slice(&(pointers.len() as u16).to_be_bytes());
    let first = bytes.iter().map(|(at, _)| *at).min().unwrap_or(512);
    p[5..7].copy_from_slice(&(first as u16).to_be_bytes());
    for (i, at) in pointers.iter().enumerate() {
        p[8 + 2 * i..10 + 2 * i].copy_from_slice(&(*at as u16).to_be_bytes());
    }
    for (at, b) in bytes {
        p[*at..*at + b.len()].copy_from_slice(b);
    }
    p
}

/// A table leaf cell: its payload's length, its rowid, the payload's bytes
/// the page holds and, where it spills, its first overflow page.
fn cell(total: usize, rowid: u64, local: &[u8], overflow: Option<u32>) -> Vec<u8> {
    let mut c = sqlite_varint(total as u64);
    c.extend_from_slice(&sqlite_varint(rowid));
    c.extend_from_slice(local);
    if let Some(page) = overflow {
        c.extend_from_slice(&page.to_be_bytes());
    }
    c
}

/// A record of `total` bytes holding one blob, the rest of them.
fn blob_record(total: usize) -> Vec<u8> {
    let serial = sqlite_varint(12 + 2 * (total as u64 - 3));
    assert_eq!(serial.len(), 2, "a header of three bytes");
    let mut r = vec![3];
    r.extend_from_slice(&serial);
    r.resize(total, 0x5a);
    r
}

/// An overflow page of 512 bytes naming `next` as the chain's next page.
fn overflow(next: u32) -> Vec<u8> {
    let mut p = vec![0u8; 512];
    p[..4].copy_from_slice(&next.to_be_bytes());
    p
}

/// An interior table page of 512 bytes: a cell a child of `children`, then
/// its right-most child.
fn interior_of(children: &[u32], rightmost: u32) -> Vec<u8> {
    let mut p = interior(rightmost);
    let mut at = 512;
    for (i, child) in children.iter().enumerate() {
        let mut c = child.to_be_bytes().to_vec();
        c.extend_from_slice(&sqlite_varint(i as u64 + 1));
        at -= c.len();
        p[at..at + c.len()].copy_from_slice(&c);
        p[12 + 2 * i..14 + 2 * i].copy_from_slice(&(at as u16).to_be_bytes());
    }
    p[3..5].copy_from_slice(&(children.len() as u16).to_be_bytes());
    p[5..7].copy_from_slice(&(at as u16).to_be_bytes());
    p
}

/// The review's file (G14, finding C3): pages of 64 KiB, one leaf whose
/// `pointers` cell pointers all name its one cell, whose payload is the
/// file's length, its overflow chain a page naming itself.
fn reviews_leaf(pointers: usize) -> Vec<u8> {
    const PAGE: usize = 65_536;
    let file = 3 * PAGE;
    // The least a cell keeps in a page of 65 536 usable bytes once it
    // spills: SQLite's M, (U - 12) * 32 / 255 - 23.
    let local = (PAGE - 12) * 32 / 255 - 23;
    let c = cell(file, 1, &vec![0; local], Some(3));
    let at = PAGE - c.len();
    assert!(8 + 2 * pointers <= at, "the pointers run into the cell");
    let mut leaf = vec![0u8; PAGE];
    leaf[0] = 0x0d;
    leaf[3..5].copy_from_slice(&(pointers as u16).to_be_bytes());
    leaf[5..7].copy_from_slice(&(at as u16).to_be_bytes());
    for i in 0..pointers {
        leaf[8 + 2 * i..10 + 2 * i].copy_from_slice(&(at as u16).to_be_bytes());
    }
    leaf[at..].copy_from_slice(&c);
    let mut chain = vec![0u8; PAGE];
    chain[..4].copy_from_slice(&3u32.to_be_bytes());
    hand_made_of(PAGE, "CREATE TABLE t (a INTEGER)", 0, &[leaf, chain])
}

/// The review's leaf: every pointer was read as a cell, and every cell of
/// a leaf before the first was yielded, so a 192 KiB file asked for 28 660
/// copies of itself, 5.2 GiB, and a 1 MiB one for 28 GiB -- `--count`
/// too. A pointer repeating or overlapping another is refused before a
/// cell is read, and a walk holds a page and one row at most: under four
/// of its pages here, both reading and counting.
#[test]
fn a_leaf_naming_one_cell_again_and_again_is_refused_holding_a_page() {
    for pointers in [1_000, 28_660] {
        let path = written(&format!("review-leaf-{pointers}"), &reviews_leaf(pointers));
        let (read, held) = crate::held::peak(|| {
            let mut r = Reader::open(&path, "t")?;
            r.next_row().map(|_| ())
        });
        assert!(
            held < 4 * 65_536,
            "{pointers} pointers: the walk held {held} bytes at once"
        );
        let e = read.unwrap_err().to_string();
        assert!(
            e.contains("page 2's cell 1 repeats or overlaps another of its cells"),
            "{e}"
        );
        let (count, held) =
            crate::held::peak(|| fennec_import::sqlite::count_rows(&path, "t").map(|_| ()));
        assert!(
            held < 4 * 65_536,
            "{pointers} pointers: counting held {held} bytes at once"
        );
        assert!(count.is_err());
    }
}

/// A cell overlapping another, or lying in the page's header or pointers,
/// is refused as its page is entered: read, it was a row of other cells'
/// bytes.
#[test]
fn a_cell_sharing_bytes_or_outside_the_cell_area_is_refused() {
    let sql = "CREATE TABLE t (a BLOB)";
    let row = cell(10, 1, &[0; 10], None);
    let overlapping = raw_leaf(&[(500, row.as_slice())], &[500, 505]);
    let path = written("overlap", &hand_made(sql, 0, &[overlapping]));
    refused(
        &path,
        "page 2's cell 1 repeats or overlaps another of its cells, at byte 505",
    );
    let in_pointers = raw_leaf(&[(500, row.as_slice())], &[500, 9]);
    let path = written("in-pointers", &hand_made(sql, 0, &[in_pointers]));
    refused(&path, "page 2's cell 1 lies outside the page's cell area");
}

/// An overflow chain naming a page it already holds went round, each time
/// round read as the payload's next bytes: a row of one page's bytes
/// again and again, and memory of the cells times the file's length.
#[test]
fn an_overflow_chain_that_cycles_is_refused() {
    // 2 000 bytes: 476 in the page, 1 524 in three overflow pages, which
    // the chain names as page 3 three times; two pages more make the file
    // longer than the payload.
    let record = blob_record(2_000);
    let row = leaf(&[cell(2_000, 1, &record[..476], Some(3))]);
    let path = written(
        "chain-cycle",
        &hand_made(
            "CREATE TABLE t (a BLOB)",
            0,
            &[row, overflow(3), overflow(0), overflow(0)],
        ),
    );
    refused(&path, "page 3 is reached twice");
}

/// A chain is as long as its payload needs: a page past the last was never
/// read, a file naming one is not SQLite's (`PRAGMA integrity_check`:
/// "overflow list length").
#[test]
fn an_overflow_chain_longer_than_its_payload_is_refused() {
    // 800 bytes: 292 in the page, 508 in one overflow page, which names a
    // next page all the same.
    let record = blob_record(800);
    let row = leaf(&[cell(800, 1, &record[..292], Some(3))]);
    let path = written(
        "chain-long",
        &hand_made(
            "CREATE TABLE t (a BLOB)",
            0,
            &[row, overflow(4), overflow(0)],
        ),
    );
    refused(&path, "goes on past its payload's 800 bytes, to page 4");
}

/// A page is one node of one b-tree or one page of one overflow chain: a
/// leaf read as a cell's overflow page was imported as the end of that row
/// and then walked as a leaf of its own.
#[test]
fn an_overflow_page_that_is_a_btree_page_is_refused() {
    let record = blob_record(800);
    let row = leaf(&[cell(800, 1, &record[..292], Some(4))]);
    let path = written(
        "chain-into-leaf",
        &hand_made(
            "CREATE TABLE t (a BLOB)",
            0,
            &[interior_of(&[3], 4), row, leaf(&[])],
        ),
    );
    refused(&path, "page 4 is reached twice");
}

/// Two interior pages naming one child are refused at the second, whose
/// rows would have been read twice.
#[test]
fn a_page_named_by_two_parents_is_refused() {
    let path = written(
        "two-parents",
        &hand_made(
            "CREATE TABLE t (a BLOB)",
            0,
            &[interior_of(&[3], 4), interior(5), interior(5), leaf(&[])],
        ),
    );
    refused(&path, "page 5 is reached twice");
}

/// A row lacking a column whose default the importer does not compute --
/// `CURRENT_TIMESTAMP`, an expression -- is refused naming the table, the
/// column and the default, where it was read as `NULL`. SQLite's own
/// `ADD COLUMN` takes constants alone, so only a file made otherwise has one.
#[test]
fn a_default_the_importer_does_not_compute_is_refused_where_a_row_needs_it() {
    let row = leaf(&[cell(3, 1, &[2, 1, 7], None)]);
    let path = written(
        "unread-default",
        &hand_made(
            "CREATE TABLE t (a INTEGER, b TEXT DEFAULT CURRENT_TIMESTAMP)",
            0,
            &[row],
        ),
    );
    let mut r = Reader::open(&path, "t").unwrap();
    let e = r.next_row().unwrap_err().to_string();
    assert!(
        e.contains("row 1 of `t`") && e.contains("`b`") && e.contains("CURRENT_TIMESTAMP"),
        "{e}"
    );
}

// ------------------------------------------ files SQLite wrote, kept here

/// A file SQLite wrote for these tests, read where it is
/// (`tests/sqlite-files/README.md` says how each was made): no test runs
/// `sqlite3`.
fn kept(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/sqlite-files")
        .join(name)
}

/// A table's column names and rows.
fn read_whole(path: &Path, table: &str) -> (Vec<String>, Vec<Vec<Value>>) {
    let mut r = Reader::open(path, table).unwrap_or_else(|e| panic!("{table}: {e}"));
    let names = r.columns().unwrap().into_iter().map(|c| c.name).collect();
    let mut out = Vec::new();
    while let Some(row) = r.next_row().unwrap_or_else(|e| panic!("{table}: {e}")) {
        out.push(row);
    }
    (names, out)
}

fn text(s: &str) -> Value {
    Value::Text(s.into())
}

/// SQLite stores no VIRTUAL generated column in a record -- it computes
/// one as a row is read -- and a STORED one where it is declared: a record
/// holds every other column, in order. Read as if every column were in it,
/// each column after `b` took the value of the one before it.
#[test]
fn a_virtual_generated_column_is_left_out_and_the_rest_keep_their_places() {
    let db = kept("generated.sqlite");
    let (names, rows) = read_whole(&db, "t");
    assert_eq!(names, ["a", "c", "d", "e"]);
    assert_eq!(
        rows,
        [
            vec![Value::Int(1), text("x"), Value::Int(2), text("y")],
            vec![Value::Int(2), text("p"), Value::Int(3), text("q")],
        ]
    );
}

/// What a VIRTUAL generated column was is named in the import's summary,
/// and a STORED one's values come across as a plain field's.
#[test]
fn a_generated_column_is_named_in_the_summary() {
    let db = kept("generated.sqlite");
    let mut src = Reader::open(&db, "t").unwrap();
    let cols = src.columns().unwrap();
    let stored = cols[2].note.as_deref().unwrap_or_default();
    assert!(stored.contains("STORED generated"), "{stored}");
    let mut target = Database::new();
    let s = load::run(&mut src, &mut target, &Options::new("g")).unwrap();
    assert_eq!(s.rows, 2);
    let virtuals: Vec<&String> = s
        .warnings
        .iter()
        .filter(|w| w.contains("VIRTUAL generated column"))
        .collect();
    assert_eq!(virtuals.len(), 2, "{:?}", s.warnings);
    assert!(virtuals[0].starts_with("`b`") && virtuals[1].starts_with("`f`"));
}

/// A column `ALTER TABLE ADD COLUMN` added is in no record written before
/// it: SQLite reads it there as its default made a value of the column's
/// affinity, `NULL` where it has none. Every one read as `NULL`, a `NOT
/// NULL DEFAULT 7` column's too. The text `RENAME COLUMN` rewrote names a
/// column as it is now, and a VIRTUAL column added among them moves none.
#[test]
fn a_column_added_later_reads_as_its_default_in_older_rows() {
    let (names, rows) = read_whole(&kept("defaults.sqlite"), "t");
    assert_eq!(
        names,
        ["a", "b two", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n"]
    );
    // SQLite's own reading, `SELECT quote(..)`:
    // 1|'one'|7|'5'|2.0|-3.5|X'00FF'|1|300000|12|NULL|'last'|'abc'|'-16'
    assert_eq!(
        rows[0],
        [
            Value::Int(1),
            text("one"),
            Value::Int(7),
            text("5"),
            Value::Float(2.0),
            Value::Float(-3.5),
            Value::Bytes(vec![0x00, 0xff]),
            Value::Int(1),
            Value::Int(300_000),
            Value::Int(12),
            Value::Null,
            text("last"),
            text("abc"),
            text("-16"),
        ]
    );
    assert_eq!(
        rows[1],
        [
            Value::Int(2),
            text("two"),
            Value::Int(8),
            text("w"),
            Value::Float(1.5),
            Value::Int(0),
            Value::Bytes(vec![0x01]),
            Value::Int(0),
            Value::Int(1),
            Value::Int(2),
            text("k"),
            text("l"),
            text("m"),
            text("n"),
        ]
    );
}

/// Names in each of SQLite's four quotes, a type holding a comma, table
/// constraints holding commas, and a table whose quoted name holds a
/// parenthesis -- where the body was looked for.
#[test]
fn quoted_names_and_constraints_keep_every_column_in_place() {
    let db = kept("names.sqlite");
    let (names, rows) = read_whole(&db, "quotes");
    assert_eq!(names, ["dq", "sq", "bt", "br", "x", "z"]);
    assert_eq!(
        rows,
        [vec![
            Value::Int(1),
            text("s"),
            Value::Int(2),
            text("b"),
            Value::Float(4.5),
            Value::Int(9),
        ]]
    );
    let (names, rows) = read_whole(&db, "my (table");
    assert_eq!(names, ["a", "b"]);
    assert_eq!(rows, [vec![Value::Int(1), text("b")]]);
}

/// A comment is a space to SQLite, which keeps it in the `CREATE` text: a
/// comma in one split a column in two, and a quote opened a string that
/// ran on over the columns after it.
#[test]
fn a_comment_in_the_create_text_moves_no_column() {
    let (names, rows) = read_whole(&kept("names.sqlite"), "comments");
    assert_eq!(names, ["a", "b", "c", "d"]);
    assert_eq!(
        rows,
        [vec![
            Value::Int(1),
            text("two"),
            text("not -- a comment, ("),
            Value::Int(4),
        ]]
    );
}

/// A column named `""` was dropped, and each after it took its left
/// neighbour's values. It keeps its place, as `column<N>` -- SQLite's own
/// name for a result column with none, N its place from 1.
#[test]
fn a_column_with_an_empty_name_keeps_its_place() {
    let (names, rows) = read_whole(&kept("names.sqlite"), "empty");
    assert_eq!(names, ["a", "column2", "c"]);
    assert_eq!(rows, [vec![Value::Int(1), Value::Int(2), text("three")]]);
}

/// SQLite's rowid alias is an `INTEGER` column that is its table's primary
/// key, named by a column's constraint or the table's `PRIMARY KEY(..)` --
/// in any case, with `ASC`, `DESC` or `COLLATE`, after a `CONSTRAINT`'s
/// name -- but for SQLite's own exception, a column declared `INTEGER
/// PRIMARY KEY DESC`, which keeps its value in the record. Each row here
/// holds 10, read as the rowid (1) or as `NULL` where the alias was
/// misread, and `PRIMARY KEY` inside a default's text made one.
#[test]
fn the_rowid_alias_is_the_column_sqlite_takes_for_it() {
    let db = kept("rowid.sqlite");
    for table in ["desc_pk", "table_desc", "named_pk", "cased", "in_text"] {
        let (_, rows) = read_whole(&db, table);
        assert_eq!(rows, [vec![Value::Int(10), text("a")]], "{table}");
    }
}

/// A WITHOUT ROWID table is refused as it is opened, by its name; a
/// STRICT table, whose records are laid out as any other's, is read.
#[test]
fn a_without_rowid_table_is_refused_by_name_and_a_strict_one_is_read() {
    let db = kept("without-rowid.sqlite");
    let e = Reader::open(&db, "Ünits").unwrap_err().to_string();
    assert!(e.contains("`Ünits`") && e.contains("WITHOUT ROWID"), "{e}");
    let e = fennec_import::sqlite::count_rows(&db, "Ünits")
        .unwrap_err()
        .to_string();
    assert!(e.contains("`Ünits`") && e.contains("WITHOUT ROWID"), "{e}");
    let (_, rows) = read_whole(&db, "plain");
    assert_eq!(rows, [vec![Value::Int(1), text("one")]]);
}

/// A rollback journal whose first byte is not zero is hot (SQLite's
/// `hasHotJournal`): a writer stopped inside a transaction, and the file
/// alone may hold part of it -- here SQLite was killed with its pages
/// spilled into the file. Read, the import took what was never committed.
/// A journal PERSIST mode left, its header zeroed, is not hot.
#[test]
fn a_hot_rollback_journal_is_refused() {
    let hot = kept("hot.sqlite");
    let e = match Reader::open(&hot, "t") {
        Ok(mut r) => panic!("the torn file was read, its first row {:?}", r.next_row()),
        Err(e) => e.to_string(),
    };
    assert!(
        e.contains("hot.sqlite-journal is a hot rollback journal"),
        "{e}"
    );
    let e = fennec_import::sqlite::count_rows(&hot, "t")
        .unwrap_err()
        .to_string();
    assert!(e.contains("hot rollback journal"), "{e}");
    let (_, rows) = read_whole(&kept("persist.sqlite"), "t");
    assert_eq!(rows, [vec![Value::Int(1), text("kept")]]);
}

/// A file of SQLite's largest pages, 65 536 bytes -- the header's 1 --
/// with a row spilling into an overflow page.
#[test]
fn a_file_of_the_largest_pages_is_read() {
    let (_, rows) = read_whole(&kept("large-pages.sqlite"), "t");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], [Value::Int(1), Value::Bytes(vec![0; 70_000])]);
    assert_eq!(rows[1], [Value::Int(2), Value::Bytes(vec![1, 2])]);
}

#[test]
fn not_a_sqlite_file_is_reported() {
    let path = dir().join("junk.sqlite");
    std::fs::write(&path, b"this is not a database").unwrap();
    let e = Reader::open(&path, "t").unwrap_err().to_string();
    assert!(e.contains("not a SQLite file"), "{e}");
}

// ------------------------------------------------------------ end to end

/// fennec-bench writes vectors into SQLite as f32 little-endian BLOBs
/// (`crates/fennec-bench/src/main.rs`, `blob()`). This checks we read the same
/// encoding and takes the load end to end.
#[test]
fn blob_vectors_load_and_index() {
    let db = fixture!(
        "vector",
        "create table docs (id integer primary key, title text, embed blob);
         insert into docs values
           (1, 'one',   x'0000803F0000000000000000'),
           (2, 'two',   x'000000000000803F00000000'),
           (3, 'three', x'00000000000000000000803F');"
    );

    let mut src = Reader::open(&db, "docs").unwrap();
    let mut target = Database::new();
    let mut opts = Options::new("articles");
    opts.vectors.push(("embed".into(), 3));
    opts.indexes.push((
        "embed".into(),
        IndexKind::Vector(VectorIndexSpec::default()),
    ));

    let summary = load::run(&mut src, &mut target, &opts).unwrap();
    assert_eq!(summary.rows, 3);

    let stats = target.stats();
    assert_eq!(stats[0].documents, 3);
    assert_eq!(stats[0].vector_indexes[0].field, "embed");
    assert_eq!(stats[0].vector_indexes[0].dim, 3);

    // The source ids must be preserved and the vectors decoded correctly.
    let Response::Rows(rs) = target
        .execute(&Statement::Select(Select {
            collection: "articles".into(),
            project: Some(vec!["id".into(), "title".into()]),
            near: Some(Near {
                field: "embed".into(),
                vector: Expr::Lit(Value::Vector(vec![0.0, 1.0, 0.0])),
                ef: None,
                exact: true,
                metric: None,
            }),
            limit: Some(Count::N(1)),
            ..Default::default()
        }))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(rs.rows[0].values[0], Value::Int(2));
    assert_eq!(rs.rows[0].values[1], Value::Text("two".into()));
}

/// A decimal column must not silently fall back to float; it has to ask for `--cast`.
#[test]
fn decimal_column_demands_cast() {
    let db = fixture!(
        "money",
        "create table t (id integer primary key, price decimal(10,2));
         insert into t values (1, 19.99);"
    );
    let mut src = Reader::open(&db, "t").unwrap();
    let cols = src.columns().unwrap();
    let e = map::plan(&cols, &Options::new("m"))
        .unwrap_err()
        .to_string();
    assert!(e.contains("--cast price="), "{e}");

    // Asked for as a float, it is parsed from the text value.
    let mut src = Reader::open(&db, "t").unwrap();
    let mut o = Options::new("m");
    o.casts.push(("price".into(), DataType::Float));
    let mut target = Database::new();
    load::run(&mut src, &mut target, &o).unwrap();
    assert_eq!(target.stats()[0].documents, 1);
}

/// A large table must load end to end; this also shows that the interior
/// node walk works together with the load pipeline.
#[test]
fn large_table_loads_end_to_end() {
    let db = fixture!(
        "many",
        "create table t (id integer primary key, category text, score int);
         insert into t (category, score) with recursive c(x) as (
           select 1 union all select x + 1 from c where x < 20000
         ) select 'k' || (x % 7), x from c;"
    );
    let mut src = Reader::open(&db, "t").unwrap();
    let mut target = Database::new();
    let mut o = Options::new("t");
    o.id = IdSource::Auto;
    o.indexes.push(("category".into(), IndexKind::HASH));
    let s = load::run(&mut src, &mut target, &o).unwrap();
    assert_eq!(s.rows, 20_000);
    assert_eq!(target.stats()[0].documents, 20_000);
}

/// `--where` works with SQLite too: the filter is FennecQL and is applied
/// before the row is written. Since there is no query engine on the source
/// side, it runs locally.
#[test]
fn where_filter_applies_to_sqlite() {
    let db = fixture!(
        "filter",
        "create table t (id integer primary key, category text, score int);
         insert into t (category, score) values
           ('book', 5), ('book', 50), ('article', 60), ('article', 5);"
    );
    let Stmt::Select(sel) =
        fennec_ql::parse_one(r#"get t where category = "book" and score >= 10"#).unwrap()
    else {
        panic!()
    };

    let mut src = Reader::open(&db, "t").unwrap();
    let mut target = Database::new();
    let mut o = Options::new("m");
    o.filter = sel.filter;
    let s = load::run(&mut src, &mut target, &o).unwrap();
    assert_eq!(s.rows, 1);
    assert_eq!(s.skipped, 3);

    let Response::Rows(rs) = target
        .execute(&Statement::Select(Select {
            collection: "m".into(),
            project: Some(vec!["score".into()]),
            ..Default::default()
        }))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(rs.rows[0].values[0], Value::Int(50));
}

/// Counting only counts the cells without decoding the record body; on a
/// multi-page table it has to descend through the interior nodes too.
#[test]
fn rows_can_be_counted_without_reading_them() {
    let db = fixture!(
        "count",
        "create table t (id integer primary key, v text);
         insert into t (v) with recursive c(x) as (
           select 1 union all select x + 1 from c where x < 30000
         ) select 'row ' || x from c;
         create table empty (a int);"
    );
    assert_eq!(fennec_import::sqlite::count_rows(&db, "t").unwrap(), 30_000);
    assert_eq!(fennec_import::sqlite::count_rows(&db, "empty").unwrap(), 0);
    assert!(fennec_import::sqlite::count_rows(&db, "nosuch").is_err());
}

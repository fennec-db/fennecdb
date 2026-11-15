//! A read-only scanner that reads the SQLite file format directly.
//!
//! Why it is hand-written: no fennecdb crate depends on anything external (see
//! `fennec_core` lib.rs, design decision 1). What it reads is the table b-tree;
//! indexes, triggers and views are out of scope -- the only thing an import
//! needs is the rows -- and a file whose WAL or hot rollback journal holds
//! what the file alone does not is refused ([`check_side_files`]).
//!
//! The file is the attacker's: its lengths are held to the file before
//! anything is allocated, its walk reaches a page once and holds a page and
//! a row at a time ([`Walk`]), and its `CREATE TABLE` text is read as SQLite
//! reads it, so no column takes another's values ([`parse_create`]).
//!
//! Reference: <https://www.sqlite.org/fileformat2.html>

use crate::{Column, Source};
use fennec_core::error::{Error, Result};
use fennec_core::value::{DataType, Value};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MAGIC: &[u8; 16] = b"SQLite format 3\0";

const LEAF_TABLE: u8 = 0x0d;
const INTERIOR_TABLE: u8 = 0x05;
const LEAF_INDEX: u8 = 0x0a;
const INTERIOR_INDEX: u8 = 0x02;

/// Rows to scan for columns that have no declared type.
pub const DEFAULT_SAMPLE: usize = 1_000;

/// `sqlite_master` is always rooted on page 1.
const MASTER_ROOT: u32 = 1;

fn corrupt(what: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!("sqlite: {what}"))
}

// ------------------------------------------------------------------ varint

/// SQLite varint: 1-9 bytes, big-endian. The top bit of the first eight
/// bytes is a continuation flag and they carry seven bits; the ninth byte
/// contributes all eight.
fn varint(b: &[u8], pos: &mut usize) -> Result<i64> {
    let mut v: u64 = 0;
    for i in 0..9 {
        let byte = *b.get(*pos).ok_or_else(|| corrupt("truncated varint"))?;
        *pos += 1;
        if i == 8 {
            return Ok(((v << 8) | byte as u64) as i64);
        }
        v = (v << 7) | (byte & 0x7f) as u64;
        if byte & 0x80 == 0 {
            return Ok(v as i64);
        }
    }
    unreachable!()
}

fn be16(b: &[u8], at: usize) -> Result<usize> {
    let s = b
        .get(at..at + 2)
        .ok_or_else(|| corrupt("the page is shorter than expected"))?;
    Ok(u16::from_be_bytes([s[0], s[1]]) as usize)
}

fn be32(b: &[u8], at: usize) -> Result<u32> {
    let s = b
        .get(at..at + 4)
        .ok_or_else(|| corrupt("the page is shorter than expected"))?;
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// An n-byte signed big-endian integer.
fn be_int(b: &[u8], pos: &mut usize, n: usize) -> Result<i64> {
    let s = b
        .get(*pos..*pos + n)
        .ok_or_else(|| corrupt("truncated record body"))?;
    *pos += n;
    let mut v: i64 = if s[0] & 0x80 != 0 { -1 } else { 0 };
    for &byte in s {
        v = (v << 8) | byte as i64;
    }
    Ok(v)
}

// ---------------------------------------------------------------- encoding

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

fn decode_text(b: &[u8], enc: Encoding) -> String {
    match enc {
        Encoding::Utf8 => String::from_utf8_lossy(b).into_owned(),
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let units = b.as_chunks::<2>().0.iter().map(|c| {
                if enc == Encoding::Utf16Le {
                    u16::from_le_bytes(*c)
                } else {
                    u16::from_be_bytes(*c)
                }
            });
            char::decode_utf16(units)
                .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect()
        }
    }
}

// ------------------------------------------------------------------ pager

/// The page reader. Pages are numbered from 1.
#[derive(Debug)]
struct Pager {
    file: File,
    /// The file's length: what every length the file declares is held to
    /// before anything is allocated for it.
    len: u64,
    page_size: usize,
    /// The usable part of a page once the reserved bytes are taken off.
    usable: usize,
    encoding: Encoding,
    /// The overflow page a payload's chain is read through, one page kept
    /// and reused rather than one allocated a page.
    spare: Vec<u8>,
}

/// SQLite's floor for a page's usable part (fileformat2.html, *Reserved
/// bytes per page*): what `local_len`'s arithmetic stands on, a file under
/// it refused as SQLite refuses one.
const MIN_USABLE: usize = 480;

impl Pager {
    fn open(path: &Path) -> Result<Pager> {
        let mut file =
            File::open(path).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
        let mut head = [0u8; 100];
        file.read_exact(&mut head)
            .map_err(|_| Error::Corrupt(format!("{}: not a SQLite file", path.display())))?;
        if &head[..16] != MAGIC {
            return Err(Error::Corrupt(format!(
                "{}: not a SQLite file",
                path.display()
            )));
        }
        // The value 1 means 65536 -- the field is 16 bits and the value does not fit.
        let page_size = match u16::from_be_bytes([head[16], head[17]]) {
            1 => 65_536,
            n => n as usize,
        };
        if page_size < 512 || !page_size.is_power_of_two() {
            return Err(corrupt(format!("invalid page size {page_size}")));
        }
        // A byte: at most 255 of a page of 512 at least.
        let reserved = head[20] as usize;
        if page_size - reserved < MIN_USABLE {
            return Err(corrupt(format!(
                "a page's usable part is {} bytes, under SQLite's {MIN_USABLE}",
                page_size - reserved
            )));
        }
        let encoding = match u32::from_be_bytes([head[56], head[57], head[58], head[59]]) {
            0 | 1 => Encoding::Utf8,
            2 => Encoding::Utf16Le,
            3 => Encoding::Utf16Be,
            n => return Err(corrupt(format!("unknown text encoding {n}"))),
        };
        let len = file
            .metadata()
            .map_err(|e| Error::Io(format!("{}: {e}", path.display())))?
            .len();
        Ok(Pager {
            file,
            len,
            page_size,
            usable: page_size - reserved,
            encoding,
            spare: Vec::new(),
        })
    }

    /// How many whole pages the file holds: no page past them is read.
    fn pages(&self) -> u64 {
        self.len / self.page_size as u64
    }

    /// Reads page `n` into `buf`, which is a page long after: the walk and
    /// the overflow chains each read into one buffer of their own, never a
    /// page allocated a read.
    fn read_into(&mut self, n: u32, buf: &mut Vec<u8>) -> Result<()> {
        if n == 0 {
            return Err(corrupt("there is no page 0"));
        }
        let at = (n as u64 - 1) * self.page_size as u64;
        self.file
            .seek(SeekFrom::Start(at))
            .map_err(|e| Error::Io(e.to_string()))?;
        buf.resize(self.page_size, 0);
        self.file
            .read_exact(buf)
            .map_err(|_| corrupt(format!("could not read page {n} (the file is short)")))
    }

    /// A cell's payload length, a varint of the file's, held to the file's
    /// length before a byte is allocated for it, as a wire message's is
    /// (`fennec_wire::proto::zeroed`): every byte of a payload is in the
    /// file, and `Vec::with_capacity` of a length declaring 2^62 bytes
    /// ended the import -- capacity overflow, or the allocator's abort.
    fn held_to_file(&self, total: u64) -> Result<()> {
        if total > self.len {
            return Err(corrupt(format!(
                "a cell's payload of {total} bytes is longer than the file, {} bytes",
                self.len
            )));
        }
        Ok(())
    }

    /// The payload length that fits inside the page on a table leaf.
    fn local_len(&self, p: usize) -> usize {
        let u = self.usable;
        let x = u - 35;
        if p <= x {
            return p;
        }
        let m = ((u - 12) * 32 / 255) - 23;
        let k = m + (p - m) % (u - 4);
        if k <= x {
            k
        } else {
            m
        }
    }

    /// Collects the cell payload; the part that does not fit in the page
    /// comes from the overflow chain, each of its pages reached once in the
    /// walk's `reached` and the chain as long as the payload needs: a chain
    /// naming a page it held went round for ever, and one naming a b-tree
    /// page read that page as this row's bytes ([`Walk`]).
    fn payload(
        &mut self,
        reached: &mut Reached,
        page: &[u8],
        at: usize,
        total: u64,
    ) -> Result<Vec<u8>> {
        self.held_to_file(total)?;
        let total = total as usize;
        let local = self.local_len(total);
        let head = page
            .get(at..at + local)
            .ok_or_else(|| corrupt("the cell payload does not fit the page"))?;
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(head);
        if local == total {
            return Ok(out);
        }
        let first = be32(page, at + local)?;
        let mut spare = std::mem::take(&mut self.spare);
        let chained = self.chain(reached, &mut spare, &mut out, first, total);
        self.spare = spare;
        chained.map(|()| out)
    }

    /// Reads the overflow chain from page `next` into `out` until it holds
    /// `total` bytes: the pages the bytes left need, and no more -- the
    /// last names no page after it, as SQLite writes it and its
    /// `integrity_check` holds it ("overflow list length").
    fn chain(
        &mut self,
        reached: &mut Reached,
        spare: &mut Vec<u8>,
        out: &mut Vec<u8>,
        mut next: u32,
        total: usize,
    ) -> Result<()> {
        let per_page = self.usable - 4;
        while out.len() < total {
            if next == 0 {
                return Err(corrupt("the overflow chain ended early"));
            }
            reached.reach(next, self.pages())?;
            self.read_into(next, spare)?;
            let take = (total - out.len()).min(per_page);
            out.extend_from_slice(&spare[4..4 + take]);
            next = be32(spare, 0)?;
        }
        if next != 0 {
            return Err(corrupt(format!(
                "the overflow chain goes on past its payload's {total} bytes, to page {next}"
            )));
        }
        Ok(())
    }
}

/// A single cell on a table leaf.
#[derive(Debug)]
struct Cell {
    rowid: i64,
    payload: Vec<u8>,
}

/// Where the b-tree header sits inside the page. On page 1 the 100-byte file
/// header comes first.
fn header_at(page_no: u32) -> usize {
    if page_no == MASTER_ROOT {
        100
    } else {
        0
    }
}

// ------------------------------------------------------------ b-tree walk

/// The pages a walk has reached, a bit each of the file's. A page of a
/// SQLite file is one node of one b-tree or one page of one overflow chain,
/// so a page reached twice, by any path, is refused naming it: an interior
/// page naming itself or a page above it walked on for ever, its stack
/// growing by the page's cells each time round; a chain naming a page it
/// held went round, reading one page as the payload's every next bytes,
/// and memory the cells times the file's length; a chain naming a b-tree
/// page read the page as a row's bytes, then walked it as a node.
#[derive(Debug, Default)]
struct Reached {
    bits: Vec<u64>,
}

impl Reached {
    /// Marks `page_no` reached, refusing page 0, a page past the file's
    /// end -- read, it would be refused as short -- or one reached before.
    fn reach(&mut self, page_no: u32, pages: u64) -> Result<()> {
        if page_no == 0 {
            return Err(corrupt("there is no page 0"));
        }
        if page_no as u64 > pages {
            return Err(corrupt(format!(
                "page {page_no} is past the file's {pages} pages"
            )));
        }
        if self.bits.is_empty() {
            self.bits = vec![0; (pages as usize + 1).div_ceil(64)];
        }
        let (word, bit) = (page_no as usize / 64, 1u64 << (page_no % 64));
        if self.bits[word] & bit != 0 {
            return Err(corrupt(format!(
                "page {page_no} is reached twice: a page is one node of one b-tree \
                 or one page of one overflow chain"
            )));
        }
        self.bits[word] |= bit;
        Ok(())
    }
}

/// A walker that starts at the root page and yields the leaves' cells in
/// rowid order, one at a time.
///
/// It holds a page and one row at a time: the page whose cells it yields
/// (`page`, read into again for each page), a bit a byte of it (`held`), a
/// bit a page of the file (`reached`), the pages named and not yet entered
/// (`stack`, 4 bytes each, a page of the file at most once, since a page is
/// reached as it is named) and the row it yields, whose payload is at most
/// the file's length and read from pages no other row reads. A cell's
/// payload is read as the cell is yielded: every cell of a leaf was read as
/// the leaf was entered, and a leaf of 64 KiB pages whose 28 660 pointers
/// named one cell, its payload the file's length, made a 192 KiB file ask
/// for 5.2 GiB (G14, the review's finding C3).
#[derive(Debug)]
struct Walk {
    root: u32,
    started: bool,
    /// Pages named by an interior page and not entered yet; `pop` gives
    /// the next one.
    stack: Vec<u32>,
    reached: Reached,
    /// The leaf whose cells are being yielded.
    page: Vec<u8>,
    page_no: u32,
    /// The next of its cells to yield, and how many it has.
    cell: usize,
    cells: usize,
    /// A bit a byte of a page: the bytes its cells hold, no byte two
    /// cells'.
    held: Vec<u64>,
}

impl Walk {
    fn new(root: u32) -> Walk {
        Walk {
            root,
            started: false,
            stack: Vec::new(),
            reached: Reached::default(),
            page: Vec::new(),
            page_no: 0,
            cell: 0,
            cells: 0,
            held: Vec::new(),
        }
    }

    fn next(&mut self, pager: &mut Pager) -> Result<Option<Cell>> {
        if !self.started {
            self.started = true;
            self.reached.reach(self.root, pager.pages())?;
            self.stack.push(self.root);
        }
        loop {
            if self.cell < self.cells {
                let i = self.cell;
                self.cell += 1;
                return self.leaf_cell(pager, i).map(Some);
            }
            let Some(page_no) = self.stack.pop() else {
                return Ok(None);
            };
            pager.read_into(page_no, &mut self.page)?;
            self.page_no = page_no;
            let h = header_at(page_no);
            let kind = *self.page.get(h).ok_or_else(|| corrupt("no page header"))?;
            let ncells = be16(&self.page, h + 3)?;
            match kind {
                LEAF_TABLE => {
                    self.cells_apart(pager, h + 8, ncells, true)?;
                    self.cell = 0;
                    self.cells = ncells;
                }
                INTERIOR_TABLE => {
                    let ptrs = h + 12;
                    self.cells_apart(pager, ptrs, ncells, false)?;
                    let pages = pager.pages();
                    let rightmost = be32(&self.page, h + 8)?;
                    self.reached.reach(rightmost, pages)?;
                    self.stack.push(rightmost);
                    // Pushed last to first, so `pop` yields rowid order.
                    for i in (0..ncells).rev() {
                        let at = be16(&self.page, ptrs + i * 2)?;
                        let child = be32(&self.page, at)?;
                        self.reached.reach(child, pages)?;
                        self.stack.push(child);
                    }
                }
                LEAF_INDEX | INTERIOR_INDEX => {
                    return Err(Error::Query(
                        "the table is declared `WITHOUT ROWID`; fennec-import \
                         only reads rowid tables"
                            .into(),
                    ))
                }
                other => return Err(corrupt(format!("unknown page type {other}"))),
            }
        }
    }

    /// Holds the cells of the page just read apart, before a byte of one is
    /// yielded: each begins past the page's header and cell pointers and
    /// ends inside its usable part, no byte is two cells', and a leaf's
    /// payload length is held to the file's. A cell pointer repeating
    /// another, or pointing into it, read the same bytes as rows again.
    fn cells_apart(&mut self, pager: &Pager, ptrs: usize, ncells: usize, leaf: bool) -> Result<()> {
        let p = self.page_no;
        let area = ptrs + 2 * ncells;
        let usable = pager.usable;
        if area > usable {
            return Err(corrupt(format!(
                "page {p}'s {ncells} cell pointers run past the page"
            )));
        }
        self.held.clear();
        self.held.resize(usable.div_ceil(64), 0);
        let outside = |i: usize| {
            corrupt(format!(
                "page {p}'s cell {i} lies outside the page's cell area, bytes {area} to {usable}"
            ))
        };
        for i in 0..ncells {
            let at = be16(&self.page, ptrs + 2 * i)?;
            if at < area || at >= usable {
                return Err(outside(i));
            }
            let mut end = at;
            if leaf {
                let total = varint(&self.page, &mut end)? as u64;
                pager.held_to_file(total)?;
                varint(&self.page, &mut end)?;
                let local = pager.local_len(total as usize);
                end += local + if (local as u64) < total { 4 } else { 0 };
            } else {
                end += 4;
                varint(&self.page, &mut end)?;
            }
            if end > usable {
                return Err(outside(i));
            }
            if !take(&mut self.held, at, end) {
                return Err(corrupt(format!(
                    "page {p}'s cell {i} repeats or overlaps another of its cells, at byte {at}"
                )));
            }
        }
        Ok(())
    }

    /// The leaf's cell `i`, its payload read now.
    fn leaf_cell(&mut self, pager: &mut Pager, i: usize) -> Result<Cell> {
        let at = be16(&self.page, header_at(self.page_no) + 8 + 2 * i)?;
        let mut pos = at;
        let total = varint(&self.page, &mut pos)? as u64;
        let rowid = varint(&self.page, &mut pos)?;
        let payload = pager.payload(&mut self.reached, &self.page, pos, total)?;
        Ok(Cell { rowid, payload })
    }
}

/// Marks bytes `from..to` taken in `bits`, a bit a byte, a word at a time:
/// false, and nothing promised of the bits, where one was taken already.
fn take(bits: &mut [u64], from: usize, to: usize) -> bool {
    let mut i = from;
    while i < to {
        let (word, bit) = (i / 64, i % 64);
        let n = (64 - bit).min(to - i);
        let mask = if n == 64 {
            !0
        } else {
            ((1u64 << n) - 1) << bit
        };
        if bits[word] & mask != 0 {
            return false;
        }
        bits[word] |= mask;
        i += n;
    }
    true
}

// ------------------------------------------------------------------ record

/// Parses the record body into the values of its first `ncols` columns, or
/// of fewer where it holds fewer: a row written before an `ALTER TABLE ADD
/// COLUMN` lacks the columns added after it ([`Table::row`] reads them as
/// their defaults). Columns past `ncols` are not read, as SQLite reads none.
fn record(payload: &[u8], enc: Encoding, ncols: usize) -> Result<Vec<Value>> {
    let mut pos = 0;
    let header_len = varint(payload, &mut pos)? as usize;
    if header_len > payload.len() {
        return Err(corrupt("the record header is longer than the body"));
    }
    let mut body = header_len;
    let mut out = Vec::with_capacity(ncols);
    while pos < header_len && out.len() < ncols {
        let serial = varint(payload, &mut pos)?;
        out.push(value(serial, payload, &mut body, enc)?);
    }
    Ok(out)
}

fn value(serial: i64, b: &[u8], pos: &mut usize, enc: Encoding) -> Result<Value> {
    Ok(match serial {
        0 => Value::Null,
        1 => Value::Int(be_int(b, pos, 1)?),
        2 => Value::Int(be_int(b, pos, 2)?),
        3 => Value::Int(be_int(b, pos, 3)?),
        4 => Value::Int(be_int(b, pos, 4)?),
        5 => Value::Int(be_int(b, pos, 6)?),
        6 => Value::Int(be_int(b, pos, 8)?),
        7 => {
            let s = b
                .get(*pos..*pos + 8)
                .ok_or_else(|| corrupt("the float body is short"))?;
            *pos += 8;
            Value::Float(f64::from_be_bytes([
                s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
            ]))
        }
        8 => Value::Int(0),
        9 => Value::Int(1),
        10 | 11 => return Err(corrupt(format!("reserved serial type {serial}"))),
        n if n >= 12 => {
            let len = ((n - 12) / 2) as usize;
            let s = b
                .get(*pos..*pos + len)
                .ok_or_else(|| corrupt("the value body is short"))?;
            *pos += len;
            if n % 2 == 0 {
                Value::Bytes(s.to_vec())
            } else {
                Value::Text(decode_text(s, enc))
            }
        }
        n => return Err(corrupt(format!("invalid serial type {n}"))),
    })
}

// --------------------------------------------------------- schema parsing

/// A single column declaration inside a `CREATE TABLE` body.
#[derive(Debug, Clone, PartialEq)]
struct Decl {
    name: String,
    /// The declared type, verbatim. SQLite allows an empty type.
    ty: String,
    /// `INTEGER PRIMARY KEY` -- the value is in the rowid, not in the record.
    rowid_alias: bool,
    /// `COLLATE NOCASE`: compared without ASCII's case.
    nocase: bool,
    /// `AS (...)`: a generated column, and whether SQLite stores it.
    generated: Option<Generated>,
    /// What a record lacking the column reads it as.
    absent: Absent,
    /// The name was `""`, and `name` is the one it was given.
    unnamed: bool,
}

/// A generated column (`GENERATED ALWAYS AS (...)`, or `AS (...)`).
/// SQLite's record holds every column of its table but a VIRTUAL generated
/// one, which it computes as the row is read (`sqlite3TableColumnToStorage`):
/// read as if each column were in the record, every column after one took
/// the value of the column before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Generated {
    /// In the record where it is declared: its values are imported.
    Stored,
    /// In no record: left out, and named in the import's summary. The
    /// importer computes no expression.
    Virtual,
}

/// What a record lacking a column reads it as: a row written before an
/// `ALTER TABLE ADD COLUMN` holds none of the columns added after it, and
/// SQLite reads each as its default, made a value of the column's affinity
/// (`sqlite3ColumnDefault`), `NULL` where it has none. Every one read as
/// `NULL`, a `NOT NULL DEFAULT 7` column's too.
#[derive(Debug, Clone, PartialEq)]
enum Absent {
    /// A constant, the column's affinity applied.
    Value(Value),
    /// A default the importer does not compute -- `CURRENT_TIMESTAMP`, an
    /// expression -- as written: a row needing it is refused. SQLite's
    /// `ADD COLUMN` takes constants alone, so only a file made otherwise
    /// has one where a row lacks it.
    Unread(String),
}

impl Absent {
    fn read(&self, table: &str, column: &str, rowid: i64) -> Result<Value> {
        match self {
            Absent::Value(v) => Ok(v.clone()),
            Absent::Unread(text) => Err(Error::Query(format!(
                "row {rowid} of `{table}` holds no value for `{column}`, added after it was \
                 written, and its default `{text}` is one fennec-import does not compute"
            ))),
        }
    }
}

/// Keywords that begin a column constraint; the type ends before them.
const COLUMN_CONSTRAINTS: &[&str] = &[
    "CONSTRAINT",
    "PRIMARY",
    "NOT",
    "NULL",
    "UNIQUE",
    "CHECK",
    "DEFAULT",
    "COLLATE",
    "REFERENCES",
    "GENERATED",
    "AS",
    "AUTOINCREMENT",
];

/// Keywords that begin a table-level constraint; these are not columns.
const TABLE_CONSTRAINTS: &[&str] = &["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"];

/// Parses the column list inside `CREATE TABLE ... ( ... )`, every column
/// in its place: a column keeps its position whatever its name, its
/// quotes or the comments around it, since a record's values are read by
/// position. The text is SQLite's as the table was made, comments and all,
/// and rewritten by `ALTER TABLE` (`RENAME COLUMN`, `ADD COLUMN`).
fn parse_create(sql: &str) -> Result<Vec<Decl>> {
    let sql = strip_comments(sql);
    let (inner, _) = create_body(&sql)?;

    let mut decls = Vec::new();
    // A table-level `PRIMARY KEY (x)`: if x is INTEGER it is a rowid alias too.
    let mut table_pk: Option<String> = None;

    for part in split_top(inner) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let upper_first = first_word(part).to_ascii_uppercase();
        if TABLE_CONSTRAINTS.contains(&upper_first.as_str()) {
            if let Some(pk) = table_primary_key(part) {
                table_pk = Some(pk);
            }
            continue;
        }
        let place = decls.len() + 1;
        let (name, quoted, rest) = take_name(part);
        if name.is_empty() && !quoted {
            return Err(corrupt(format!("column {place} has no name")));
        }
        let (ty, tail) = take_type(rest);
        let ty = ty.trim().to_string();
        let c = constraints(tail);
        decls.push(Decl {
            rowid_alias: ty.eq_ignore_ascii_case("INTEGER") && c.primary_key && !c.descending,
            nocase: c.nocase,
            generated: c.generated,
            absent: match c.default {
                Some(Literal::Unread(text)) => Absent::Unread(text),
                Some(lit) => Absent::Value(constant(lit, affinity(&ty))),
                None => Absent::Value(Value::Null),
            },
            unnamed: name.is_empty(),
            name,
            ty,
        });
    }

    if let Some(pk) = table_pk {
        // SQLite's names compare without ASCII's case.
        for d in &mut decls {
            if d.name.eq_ignore_ascii_case(&pk) && d.ty.eq_ignore_ascii_case("INTEGER") {
                d.rowid_alias = true;
            }
        }
    }
    if decls.is_empty() {
        return Err(corrupt("no column was found in the table"));
    }
    // A column named `""` keeps its place as `column<N>`, N its place from
    // 1: SQLite's own name for a result column that has none
    // (`sqlite3ColumnsFromExprList`). No field is named "", and dropped,
    // each column after it took its neighbour's values. The names are
    // looked up in a set, not each against all: a text of 256 KiB holds
    // 85 000 columns named `""`.
    if decls.iter().any(|d| d.unnamed) {
        let named: std::collections::HashSet<String> = decls
            .iter()
            .filter(|d| !d.unnamed)
            .map(|d| d.name.clone())
            .collect();
        for (i, d) in decls.iter_mut().enumerate() {
            if d.unnamed {
                let given = format!("column{}", i + 1);
                if named.contains(&given) {
                    return Err(corrupt(format!(
                        "column {} has an empty name, and `{given}`, the name it would be \
                         given, is another column's: rename one in SQLite first",
                        i + 1
                    )));
                }
                d.name = given;
            }
        }
    }
    Ok(decls)
}

/// The body of a `CREATE TABLE` text with no comments, and what follows
/// it: the table's options, `WITHOUT ROWID` and `STRICT`. The body opens
/// at the first parenthesis outside a quote, not one inside a quoted
/// table name.
fn create_body(sql: &str) -> Result<(&str, &str)> {
    let open = outside_quotes(sql, b'(').ok_or_else(|| corrupt("there is no CREATE TABLE body"))?;
    let inner = balanced_body(&sql[open..])
        .ok_or_else(|| corrupt("the CREATE TABLE parenthesis does not close"))?;
    Ok((inner, &sql[open + inner.len() + 2..]))
}

/// Whether a `CREATE TABLE` text declares the table `WITHOUT ROWID`: its
/// rows are in an index b-tree, which the reader does not read.
fn without_rowid(sql: &str) -> bool {
    let sql = strip_comments(sql);
    let Ok((_, options)) = create_body(&sql) else {
        return false;
    };
    let t = tokens(options);
    (0..t.len()).any(|k| word(&t, k, "WITHOUT") && word(&t, k + 1, "ROWID"))
}

/// The first byte `c` outside a quote.
fn outside_quotes(s: &str, c: u8) -> Option<usize> {
    let b = s.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &x) in b.iter().enumerate() {
        match quote {
            Some(q) => {
                if x == q {
                    quote = None;
                }
            }
            None if x == c => return Some(i),
            None => match x {
                b'\'' | b'"' | b'`' => quote = Some(x),
                b'[' => quote = Some(b']'),
                _ => {}
            },
        }
    }
    None
}

/// The text with each comment outside a quote made a space, as SQLite's
/// tokenizer reads one: `--` to the line's end, `/* */` to its close or
/// the text's end. SQLite keeps a `CREATE TABLE`'s comments in the text it
/// stores, and read as text, a comma in one split a column in two, a
/// parenthesis ended the body and a quote ran on over the columns after it.
fn strip_comments(sql: &str) -> std::borrow::Cow<'_, str> {
    if !sql.contains("--") && !sql.contains("/*") {
        return std::borrow::Cow::Borrowed(sql);
    }
    let b = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut quote: Option<u8> = None;
    let mut i = 0;
    let mut kept = 0;
    while i < b.len() {
        let x = b[i];
        match quote {
            Some(q) => {
                if x == q {
                    quote = None;
                }
                i += 1;
            }
            None => match x {
                b'\'' | b'"' | b'`' => {
                    quote = Some(x);
                    i += 1;
                }
                b'[' => {
                    quote = Some(b']');
                    i += 1;
                }
                b'-' if b.get(i + 1) == Some(&b'-') => {
                    out.push_str(&sql[kept..i]);
                    out.push(' ');
                    i = sql[i..].find('\n').map_or(b.len(), |n| i + n);
                    kept = i;
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    out.push_str(&sql[kept..i]);
                    out.push(' ');
                    i = sql[i + 2..].find("*/").map_or(b.len(), |n| i + 2 + n + 2);
                    kept = i;
                }
                _ => i += 1,
            },
        }
    }
    out.push_str(&sql[kept..]);
    std::borrow::Cow::Owned(out)
}

/// Given a string starting with `(`, returns the body up to the matching `)`.
fn balanced_body(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    if b.first() != Some(&b'(') {
        return None;
    }
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    for (i, &c) in b.iter().enumerate() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                b'\'' | b'"' | b'`' => quote = Some(c),
                b'[' => quote = Some(b']'),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&s[1..i]);
                    }
                }
                _ => {}
            },
        }
    }
    None
}

/// Splits on depth-0 commas.
fn split_top(s: &str) -> Vec<&str> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut quote: Option<u8> = None;
    for (i, &c) in b.iter().enumerate() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                b'\'' | b'"' | b'`' => quote = Some(c),
                b'[' => quote = Some(b']'),
                b'(' => depth += 1,
                b')' => depth = depth.saturating_sub(1),
                b',' if depth == 0 => {
                    out.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            },
        }
    }
    out.push(&s[start..]);
    out
}

fn first_word(s: &str) -> &str {
    s.trim()
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("")
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2 {
        let (open, close) = (b[0], b[b.len() - 1]);
        let pair = matches!(
            (open, close),
            (b'"', b'"') | (b'\'', b'\'') | (b'`', b'`') | (b'[', b']')
        );
        if pair {
            let inner = &s[1..s.len() - 1];
            // A quote is escaped by doubling it in SQL.
            return match open {
                b'"' => inner.replace("\"\"", "\""),
                b'\'' => inner.replace("''", "'"),
                b'`' => inner.replace("``", "`"),
                _ => inner.to_string(),
            };
        }
    }
    s.to_string()
}

/// Takes the name off the front of a column declaration, and whether it
/// was quoted: `""` is a name, an empty one.
fn take_name(s: &str) -> (String, bool, &str) {
    let s = s.trim_start();
    let b = s.as_bytes();
    if b.is_empty() {
        return (String::new(), false, "");
    }
    let close = match b[0] {
        b'"' => Some(b'"'),
        b'\'' => Some(b'\''),
        b'`' => Some(b'`'),
        b'[' => Some(b']'),
        _ => None,
    };
    match close {
        Some(q) => {
            let mut i = 1;
            while i < b.len() {
                if b[i] == q {
                    // A doubled quote: an escape, keep going.
                    if q != b']' && i + 1 < b.len() && b[i + 1] == q {
                        i += 2;
                        continue;
                    }
                    return (unquote(&s[..=i]), true, &s[i + 1..]);
                }
                i += 1;
            }
            (unquote(s), true, "")
        }
        None => {
            let end = s
                .find(|c: char| c.is_whitespace() || c == '(')
                .unwrap_or(s.len());
            (s[..end].to_string(), false, &s[end..])
        }
    }
}

/// Takes the type after the name; the type ends at the first constraint
/// keyword. `VARCHAR(255)` and `DECIMAL(10,2)` stay inside parentheses and
/// are not split.
fn take_type(s: &str) -> (String, &str) {
    let mut depth = 0usize;
    let mut end = s.len();
    let mut i = 0usize;
    // A character at a time, never a byte read as one: the lead byte of a
    // space of two bytes (U+00A0) read as `Â` began a word, which ended
    // where that space is -- at once -- and the loop went no further, an
    // import of such a schema never ending.
    while let Some(c) = s[i..].chars().next() {
        if c == '(' {
            depth += 1;
            i += 1;
            continue;
        }
        if c == ')' {
            depth = depth.saturating_sub(1);
            i += 1;
            continue;
        }
        if depth == 0 && !c.is_whitespace() {
            let word_end = s[i..]
                .find(|c: char| c.is_whitespace() || c == '(')
                .map(|k| i + k)
                .unwrap_or(s.len());
            let word = s[i..word_end].to_ascii_uppercase();
            if COLUMN_CONSTRAINTS.contains(&word.as_str()) {
                end = i;
                break;
            }
            i = word_end;
            continue;
        }
        i += c.len_utf8();
    }
    (s[..end].to_string(), &s[end..])
}

/// A token of a constraint's text, comments gone: what reading a
/// constraint word by word took apart -- `PRIMARY KEY` in a default's
/// string made a rowid alias, and a column's `COLLATE` inside a `CHECK`'s
/// parentheses was its own.
#[derive(Debug, Clone, PartialEq)]
enum Tok<'a> {
    /// A keyword, a name or a number.
    Word(&'a str),
    /// A quoted string or name, its quotes taken off.
    Quoted(String),
    /// `x'..'`: a blob's hex digits.
    Blob(&'a str),
    /// What a parenthesis holds, as written.
    Group(&'a str),
    /// Any other character: a sign, a comma.
    Mark(char),
}

/// The tokens of `s`, which holds no comment.
fn tokens(s: &str) -> Vec<Tok<'_>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(c) = s[i..].chars().next() {
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        match c {
            '\'' | '"' | '`' | '[' => {
                let end = quoted_end(b, i);
                out.push(Tok::Quoted(unquote(&s[i..end])));
                i = end;
            }
            'x' | 'X' if b.get(i + 1) == Some(&b'\'') => {
                let end = quoted_end(b, i + 1);
                // The digits end before the closing quote, or with the text
                // where none closes them -- never inside a character.
                let digits = if end > i + 2 && b[end - 1] == b'\'' {
                    end - 1
                } else {
                    end
                };
                out.push(Tok::Blob(&s[i + 2..digits]));
                i = end;
            }
            '(' => match balanced_body(&s[i..]) {
                Some(inner) => {
                    out.push(Tok::Group(inner));
                    i += inner.len() + 2;
                }
                None => {
                    out.push(Tok::Group(&s[i + 1..]));
                    i = s.len();
                }
            },
            '+' | '-' | ',' | ')' | ']' => {
                out.push(Tok::Mark(c));
                i += 1;
            }
            _ => {
                let end = s[i..]
                    .find(|c: char| c.is_whitespace() || "(),'\"`[]".contains(c))
                    .map_or(s.len(), |n| i + n);
                out.push(Tok::Word(&s[i..end]));
                i = end;
            }
        }
    }
    out
}

/// Where a quote opened at `at` closes, past it: a doubled quote inside is
/// one, as SQL writes it, but in `[...]`, which has no escape.
fn quoted_end(b: &[u8], at: usize) -> usize {
    let close = if b[at] == b'[' { b']' } else { b[at] };
    let mut i = at + 1;
    while i < b.len() {
        if b[i] == close {
            if close != b']' && b.get(i + 1) == Some(&close) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    b.len()
}

/// Whether token `k` is the keyword `w`.
fn word(t: &[Tok], k: usize, w: &str) -> bool {
    matches!(t.get(k), Some(Tok::Word(x)) if x.eq_ignore_ascii_case(w))
}

/// What a column's constraints say, read from their tokens.
#[derive(Debug, Default)]
struct Constraints {
    primary_key: bool,
    /// `PRIMARY KEY DESC`: SQLite's own exception, an `INTEGER` column so
    /// declared is no rowid alias and keeps its value in the record
    /// (lang_createtable.html, *ROWIDs and the INTEGER PRIMARY KEY*).
    descending: bool,
    nocase: bool,
    generated: Option<Generated>,
    default: Option<Literal>,
}

fn constraints(tail: &str) -> Constraints {
    let t = tokens(tail);
    let mut c = Constraints::default();
    for k in 0..t.len() {
        if word(&t, k, "PRIMARY") && word(&t, k + 1, "KEY") {
            c.primary_key = true;
            c.descending = word(&t, k + 2, "DESC");
        }
        if word(&t, k, "COLLATE") {
            // The last `COLLATE` is the one SQLite keeps.
            c.nocase = match t.get(k + 1) {
                Some(Tok::Word(n)) => n.eq_ignore_ascii_case("NOCASE"),
                Some(Tok::Quoted(n)) => n.eq_ignore_ascii_case("NOCASE"),
                _ => false,
            };
        }
        if word(&t, k, "AS") && matches!(t.get(k + 1), Some(Tok::Group(_))) {
            // `VIRTUAL` unless it says `STORED`, SQLite's default.
            c.generated = Some(if word(&t, k + 2, "STORED") {
                Generated::Stored
            } else {
                Generated::Virtual
            });
        }
        // Not a foreign key's `ON DELETE SET DEFAULT`.
        if word(&t, k, "DEFAULT") && !(k > 0 && word(&t, k - 1, "SET")) {
            c.default = Some(literal(&t[k + 1..]));
        }
    }
    c
}

/// The column a table-level `PRIMARY KEY (x)` names, where it names one:
/// its name, without the `ASC`, `DESC` or `COLLATE` after it, and after a
/// `CONSTRAINT`'s name or not.
fn table_primary_key(part: &str) -> Option<String> {
    let t = tokens(part);
    let k = (0..t.len()).find(|&k| word(&t, k, "PRIMARY") && word(&t, k + 1, "KEY"))?;
    let Some(Tok::Group(cols)) = t.get(k + 2) else {
        return None;
    };
    let cols = split_top(cols);
    if cols.len() != 1 {
        return None;
    }
    match tokens(cols[0]).into_iter().next()? {
        Tok::Word(name) => Some(name.to_string()),
        Tok::Quoted(name) => Some(name),
        _ => None,
    }
}

/// A default as SQLite's grammar reads it (`ccons ::= DEFAULT ...`), before
/// the column's affinity is applied.
#[derive(Debug, Clone, PartialEq)]
enum Literal {
    Null,
    /// A number as written, its sign before it, and its value where SQLite
    /// holds the token as one: digits, or `0x` and hex digits, that fit 32
    /// bits (`sqlite3GetInt32`).
    Number(String, Option<i64>),
    /// A string -- or a bare name, which SQLite reads as one.
    Text(String),
    Blob(Vec<u8>),
    Bool(bool),
    /// Anything else, as written.
    Unread(String),
}

/// How deep a default's parentheses are read: SQLite's constants are
/// rarely in more than one pair, and a file's `DEFAULT ((((...` read without
/// a bound recursed once a pair, and scanned the text once a pair.
const DEFAULT_DEPTH: usize = 8;

/// The default `t` begins with.
fn literal(t: &[Tok]) -> Literal {
    literal_within(t, DEFAULT_DEPTH)
}

fn literal_within(t: &[Tok], depth: usize) -> Literal {
    let unread = || {
        Literal::Unread(match t.first() {
            Some(Tok::Word(w)) => (*w).to_string(),
            Some(Tok::Group(g)) => format!("({g})"),
            _ => "an expression".to_string(),
        })
    };
    match t.first() {
        Some(Tok::Mark(sign @ ('-' | '+'))) => match t.get(1) {
            Some(Tok::Word(n)) if is_number(n) => {
                let n = n.replace('_', "");
                let negative = *sign == '-';
                let value = int32(&n).map(|v| if negative { -v } else { v });
                let text = if negative { format!("-{n}") } else { n };
                Literal::Number(text, value)
            }
            _ => unread(),
        },
        Some(Tok::Word(n)) if is_number(n) => {
            let n = n.replace('_', "");
            let value = int32(&n);
            Literal::Number(n, value)
        }
        Some(Tok::Word(w)) if w.eq_ignore_ascii_case("NULL") => Literal::Null,
        Some(Tok::Word(w)) if w.eq_ignore_ascii_case("TRUE") => Literal::Bool(true),
        Some(Tok::Word(w)) if w.eq_ignore_ascii_case("FALSE") => Literal::Bool(false),
        // `CURRENT_TIME`, `CURRENT_DATE`, `CURRENT_TIMESTAMP`: the clock's
        // when the row was read, which no import knows.
        Some(Tok::Word(w))
            if w.len() > 8
                && w.get(..8)
                    .is_some_and(|p| p.eq_ignore_ascii_case("CURRENT_")) =>
        {
            unread()
        }
        // A bare name is a string to SQLite (`DEFAULT scantok id`); a word
        // that is neither a number nor a name is not read.
        Some(Tok::Word(w)) if is_name(w) => Literal::Text((*w).to_string()),
        Some(Tok::Word(_)) => unread(),
        Some(Tok::Quoted(s)) => Literal::Text(s.clone()),
        Some(Tok::Blob(hex)) => match unhex(hex) {
            Some(b) => Literal::Blob(b),
            None => unread(),
        },
        // `DEFAULT (expr)`: a constant in parentheses is the constant; in
        // an expression a bare name is a column, not a string.
        Some(Tok::Group(g)) if depth > 0 => {
            let inside = tokens(g);
            match literal_within(&inside, depth - 1) {
                Literal::Text(_) if matches!(inside.first(), Some(Tok::Word(_))) => unread(),
                lit if inside.len() <= 2 => lit,
                _ => unread(),
            }
        }
        _ => unread(),
    }
}

/// Whether `w` is one of SQLite's number tokens: digits, a point and
/// digits, an exponent; or `0x` and hex digits. Digits may be parted by a
/// single `_`, as SQLite 3.46 reads them.
fn is_number(w: &str) -> bool {
    let b = w.as_bytes();
    if let [b'0', b'x' | b'X', rest @ ..] = b {
        return !rest.is_empty() && rest.iter().all(u8::is_ascii_hexdigit);
    }
    let mut i = 0;
    let digits = |i: &mut usize| {
        let from = *i;
        while *i < b.len()
            && (b[*i].is_ascii_digit()
                || (b[*i] == b'_' && *i > from && b.get(*i + 1).is_some_and(u8::is_ascii_digit)))
        {
            *i += 1;
        }
        *i - from
    };
    let mut n = digits(&mut i);
    if b.get(i) == Some(&b'.') {
        i += 1;
        n += digits(&mut i);
    }
    if n == 0 {
        return false;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == b.len()
}

/// Whether `w` is a name SQLite reads bare: letters, digits, `_`, `$` and
/// any character past ASCII, not beginning with a digit.
fn is_name(w: &str) -> bool {
    w.chars().next().is_some_and(|c| !c.is_ascii_digit())
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$' || !c.is_ascii())
}

/// A number token's value where SQLite holds it as one (`sqlite3GetInt32`):
/// decimal digits, or `0x` and up to eight hex digits, under 2^31.
fn int32(w: &str) -> Option<i64> {
    let v = match w.as_bytes() {
        [b'0', b'x' | b'X', ..] => i64::from_str_radix(w[2..].trim_start_matches('0'), 16)
            .ok()
            .or_else(|| w[2..].bytes().all(|c| c == b'0').then_some(0))?,
        b if b.iter().all(u8::is_ascii_digit) => w.parse().ok()?,
        _ => return None,
    };
    (v <= i32::MAX as i64).then_some(v)
}

fn unhex(hex: &str) -> Option<Vec<u8>> {
    let b = hex.as_bytes();
    // Hex digits alone: `from_str_radix` takes a sign too.
    if !b.len().is_multiple_of(2) || !b.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    b.chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
        .collect()
}

/// SQLite's affinity for a declared type, by its rules
/// (datatype3.html, *Determination Of Column Affinity*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Affinity {
    Integer,
    Text,
    Blob,
    Real,
    Numeric,
}

fn affinity(ty: &str) -> Affinity {
    let t = ty.to_ascii_uppercase();
    if t.contains("INT") {
        Affinity::Integer
    } else if t.contains("CHAR") || t.contains("CLOB") || t.contains("TEXT") {
        Affinity::Text
    } else if t.contains("BLOB") || t.trim().is_empty() {
        Affinity::Blob
    } else if t.contains("REAL") || t.contains("FLOA") || t.contains("DOUB") {
        Affinity::Real
    } else {
        Affinity::Numeric
    }
}

/// A constant default as SQLite reads it in a row lacking its column
/// (`valueFromExpr`): the column's affinity applied, and a number's,
/// under no affinity, made a number.
fn constant(lit: Literal, aff: Affinity) -> Value {
    match lit {
        Literal::Null | Literal::Unread(_) => Value::Null,
        Literal::Blob(b) => Value::Bytes(b),
        Literal::Bool(b) => apply(Value::Int(b as i64), aff),
        Literal::Text(s) => apply(Value::Text(s), aff),
        Literal::Number(text, value) => {
            let v = value.map_or(Value::Text(text), Value::Int);
            apply(
                v,
                if aff == Affinity::Blob {
                    Affinity::Numeric
                } else {
                    aff
                },
            )
        }
    }
}

/// `sqlite3ValueApplyAffinity` on what a default can be.
fn apply(v: Value, aff: Affinity) -> Value {
    match (aff, v) {
        (Affinity::Blob, v) => v,
        (Affinity::Text, Value::Int(n)) => Value::Text(n.to_string()),
        (Affinity::Text, v) => v,
        (Affinity::Integer | Affinity::Numeric, Value::Text(s)) => {
            numeric(&s).unwrap_or(Value::Text(s))
        }
        (Affinity::Real, Value::Int(n)) => Value::Float(n as f64),
        (Affinity::Real, Value::Text(s)) => match numeric(&s) {
            Some(Value::Int(n)) => Value::Float(n as f64),
            Some(v) => v,
            None => Value::Text(s),
        },
        (_, v) => v,
    }
}

/// A text that is a number to SQLite's numeric affinity: an integer where
/// it is one, or where a real is one exactly (`'3.0e+5'` is 300000), and a
/// real otherwise; `None` for any other text, which stays text.
fn numeric(s: &str) -> Option<Value> {
    let t = s.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\x0c' | '\r'));
    if t.is_empty()
        || !t
            .bytes()
            .all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-'))
    {
        return None;
    }
    if let Ok(n) = t.parse::<i64>() {
        return Some(Value::Int(n));
    }
    let f: f64 = t.parse().ok()?;
    // 2^63, `i64::MAX` as an f64, is past an i64; every f64 under it that
    // has no fraction is one.
    if f.fract() == 0.0 && f.abs() < i64::MAX as f64 {
        return Some(Value::Int(f as i64));
    }
    Some(Value::Float(f))
}

// ------------------------------------------------------------ type inference

/// The fennecdb type from the declared one. It follows SQLite's affinity rules,
/// but first checks the special cases that have a fennecdb counterpart.
fn map_decl(name: &str, decl: &str) -> Column {
    use fennec_ql::sqltypes::{sqlite, Mapped, Why};
    let src = if decl.trim().is_empty() {
        "(untyped)".to_string()
    } else {
        decl.trim().to_string()
    };
    // The rules SQL's `create table` reads a type by too
    // (`fennec_ql::sqltypes`), so the two cannot part.
    match sqlite(decl) {
        Mapped::Type(ty, _) => Column::new(name, ty, src),
        Mapped::Not(Why::Decimal) => Column::unsupported(
            name,
            src,
            "fennecdb has no decimal. SQLite stores this column as an int or a
             float too, so the exact value is already absent from the source;
             choose `--cast <field>=float` or `=int`",
        ),
        // An untyped column: resolved from a value sample, which the caller
        // fills in.
        Mapped::Not(_) => Column::unsupported(name, src, "no declared type"),
    }
}

/// The type inferred from sample values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Seen {
    #[default]
    Nothing,
    Int,
    Float,
    Text,
    Blob,
}

impl Seen {
    fn of(v: &Value) -> Seen {
        match v {
            Value::Null => Seen::Nothing,
            Value::Int(_) => Seen::Int,
            Value::Float(_) => Seen::Float,
            Value::Text(_) => Seen::Text,
            _ => Seen::Blob,
        }
    }
    /// The common supertype of two observations. Bytes is the widest:
    /// `adapt` can turn a number or text into bytes, the reverse is not always true.
    fn join(self, other: Seen) -> Seen {
        use Seen::*;
        match (self, other) {
            (Nothing, x) | (x, Nothing) => x,
            (a, b) if a == b => a,
            (Blob, _) | (_, Blob) => Blob,
            (Text, _) | (_, Text) => Text,
            (Int, Float) | (Float, Int) => Float,
            _ => Text,
        }
    }
    fn ty(self) -> DataType {
        match self {
            // No value was seen: text is the most harmless choice.
            Seen::Nothing | Seen::Text => DataType::Text,
            Seen::Int => DataType::Int,
            Seen::Float => DataType::Float,
            Seen::Blob => DataType::Bytes,
        }
    }
}

// ------------------------------------------------------------------ reader

/// A table as its records hold it.
#[derive(Debug)]
struct Table {
    name: String,
    /// The columns a record holds, in its order: every declared one but a
    /// VIRTUAL generated one ([`Generated`]).
    stored: Vec<Decl>,
    /// The VIRTUAL generated columns, in no record: left out.
    virtuals: Vec<String>,
    /// The rowid alias's place among `stored`.
    rowid_alias: Option<usize>,
}

impl Table {
    fn new(name: &str, sql: &str) -> Result<Table> {
        let decls = parse_create(sql).map_err(|e| match e {
            Error::Corrupt(m) => {
                Error::Corrupt(format!("{m}, in the CREATE TABLE text of `{name}`"))
            }
            other => other,
        })?;
        let (stored, virtuals): (Vec<Decl>, Vec<Decl>) = decls
            .into_iter()
            .partition(|d| d.generated != Some(Generated::Virtual));
        if stored.is_empty() {
            return Err(Error::Query(format!(
                "every column of `{name}` is a VIRTUAL generated one, which SQLite never stores"
            )));
        }
        let rowid_alias = stored.iter().position(|d| d.rowid_alias);
        Ok(Table {
            name: name.to_string(),
            stored,
            virtuals: virtuals.into_iter().map(|d| d.name).collect(),
            rowid_alias,
        })
    }

    /// A cell's row: a value a stored column, those the record lacks read
    /// as their defaults, the rowid alias's the cell's rowid.
    fn row(&self, cell: &Cell, enc: Encoding) -> Result<Vec<Value>> {
        let mut values = record(&cell.payload, enc, self.stored.len())?;
        for d in &self.stored[values.len()..] {
            values.push(d.absent.read(&self.name, &d.name, cell.rowid)?);
        }
        // The `INTEGER PRIMARY KEY` column is stored as NULL in the record;
        // the real value is the cell's rowid.
        if let Some(i) = self.rowid_alias {
            values[i] = Value::Int(cell.rowid);
        }
        Ok(values)
    }
}

/// A source that reads a SQLite table row by row.
#[derive(Debug)]
pub struct Reader {
    pager: Pager,
    walk: Walk,
    columns: Vec<Column>,
    table: Table,
}

impl Reader {
    /// Opens the file and finds the table.
    pub fn open(path: impl AsRef<Path>, table: &str) -> Result<Reader> {
        Reader::open_sampled(path, table, DEFAULT_SAMPLE)
    }

    /// [`Reader::open`], but with the number of rows to scan for columns
    /// that have no declared type.
    pub fn open_sampled(path: impl AsRef<Path>, table: &str, sample: usize) -> Result<Reader> {
        let path = path.as_ref();
        check_side_files(path)?;
        let mut pager = Pager::open(path)?;
        let (name, root, sql) = find_table(&mut pager, table)?;
        let table = Table::new(&name, &sql)?;
        let decls = &table.stored;

        let mut columns: Vec<Column> = decls
            .iter()
            .map(|d| {
                if d.rowid_alias {
                    // A rowid is always an i64, whatever the declared type says.
                    Column::new(&d.name, DataType::Int, "INTEGER PRIMARY KEY")
                } else if d.nocase {
                    // Broader than SQLite's, which folds ASCII alone: a
                    // `nocase` field folds every script.
                    map_decl(&d.name, &d.ty).nocase().note(
                        "COLLATE NOCASE taken as `nocase`, which folds every script's case \
                         where SQLite's folds ASCII's",
                    )
                } else {
                    map_decl(&d.name, &d.ty)
                }
            })
            .collect();

        // Columns with no declared type are resolved from a value sample.
        let untyped: Vec<usize> = columns
            .iter()
            .enumerate()
            .filter(|(i, c)| c.ty.is_none() && decls[*i].ty.trim().is_empty())
            .map(|(i, _)| i)
            .collect();
        if !untyped.is_empty() && sample > 0 {
            let seen = sample_types(&mut pager, root, &table, sample)?;
            for i in untyped {
                let ty = seen[i].ty();
                columns[i] = Column::new(&decls[i].name, ty.clone(), "(untyped)").note(format!(
                    "no declared type; {} was chosen from the first {sample} rows",
                    ty.name()
                ));
            }
        }

        // What the summary says of a column besides its type.
        for (c, d) in columns.iter_mut().zip(decls) {
            let mut said = Vec::new();
            if d.generated == Some(Generated::Stored) {
                said.push(
                    "a STORED generated column in SQLite: the values it stored are imported, \
                     a plain field here, its expression not",
                );
            }
            if d.unnamed {
                said.push("its name is empty (\"\") in SQLite, and it is imported under this one");
            }
            if !said.is_empty() {
                let mut note = c.note.take().map(|n| vec![n]).unwrap_or_default();
                note.extend(said.into_iter().map(String::from));
                c.note = Some(note.join("; "));
            }
        }

        Ok(Reader {
            pager,
            walk: Walk::new(root),
            columns,
            table,
        })
    }

    /// The number of columns a row holds: every column of the table but
    /// its VIRTUAL generated ones.
    pub fn width(&self) -> usize {
        self.table.stored.len()
    }
}

impl Source for Reader {
    fn columns(&mut self) -> Result<Vec<Column>> {
        Ok(self.columns.clone())
    }

    fn next_row(&mut self) -> Result<Option<Vec<Value>>> {
        let Some(cell) = self.walk.next(&mut self.pager)? else {
            return Ok(None);
        };
        self.table.row(&cell, self.pager.encoding).map(Some)
    }

    fn left_out(&self) -> Vec<String> {
        self.table
            .virtuals
            .iter()
            .map(|name| {
                format!(
                    "`{name}` is a VIRTUAL generated column, which SQLite computes as a row \
                     is read and never stores: left out"
                )
            })
            .collect()
    }
}

/// Counts the rows in the table.
///
/// It is not cheap: the whole table b-tree is walked. SQLite keeps the row
/// count nowhere, so counting means a full scan -- the call site has to ask
/// for it explicitly.
pub fn count_rows(path: impl AsRef<Path>, table: &str) -> Result<u64> {
    let path = path.as_ref();
    check_side_files(path)?;
    let mut pager = Pager::open(path)?;
    let (_, root, _) = find_table(&mut pager, table)?;
    let mut walk = Walk::new(root);
    let mut n = 0u64;
    // The record body is not decoded; only the cells are counted.
    while walk.next(&mut pager)?.is_some() {
        n += 1;
    }
    Ok(n)
}

/// Refuses a file whose side files hold what it does not: the two SQLite
/// keeps beside a database it writes (fileformat2.html, *The Rollback
/// Journal* and *The Write-Ahead Log*), each named by the file's name and
/// a suffix. Stopping beats silently returning stale or torn data.
///
/// - `<file>-wal` holding anything: changes not yet checkpointed into the
///   file, which shows what was before them.
/// - `<file>-journal` whose first byte is not zero: a hot journal
///   (SQLite's `hasHotJournal`), left by a writer stopped inside a
///   transaction, whose pages it may have spilled into the file -- read,
///   an import took what was never committed. SQLite rolls one back at
///   its first read; a journal whose header PERSIST mode zeroed, or one
///   whose header was never synced, is not hot.
fn check_side_files(path: &Path) -> Result<()> {
    let beside = |suffix: &str| {
        path.with_file_name(format!(
            "{}{suffix}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ))
    };
    let wal = beside("-wal");
    if let Ok(m) = std::fs::metadata(&wal) {
        if m.len() > 0 {
            return Err(Error::Query(format!(
                "{} holds unprocessed changes; first run \
                 `sqlite3 {} \"PRAGMA wal_checkpoint(TRUNCATE)\"`",
                wal.display(),
                path.display()
            )));
        }
    }
    let journal = beside("-journal");
    let mut first = [0u8; 1];
    let hot = File::open(&journal).is_ok_and(|mut f| f.read(&mut first).is_ok_and(|n| n == 1))
        && first[0] != 0;
    if hot {
        return Err(Error::Query(format!(
            "{} is a hot rollback journal: a writer stopped inside a transaction, and {} \
             alone may hold part of it; open it once with SQLite, which rolls the journal \
             back -- `sqlite3 {} \"PRAGMA user_version\"` -- then import again",
            journal.display(),
            path.display(),
            path.display()
        )));
    }
    Ok(())
}

/// Finds the table in `sqlite_master`: its name as the file has it, its
/// root page and its `CREATE TABLE` text. A WITHOUT ROWID table is refused
/// here, by its name, so `--dry-run` and `--count` say so too.
fn find_table(pager: &mut Pager, table: &str) -> Result<(String, u32, String)> {
    let mut walk = Walk::new(MASTER_ROOT);
    let mut tables = Vec::new();
    while let Some(cell) = walk.next(pager)? {
        // sqlite_master: (type, name, tbl_name, rootpage, sql)
        let mut r = record(&cell.payload, pager.encoding, 5)?;
        r.resize(5, Value::Null);
        let kind = r[0].as_text().unwrap_or_default();
        if kind != "table" {
            continue;
        }
        let name = r[1].as_text().unwrap_or_default().to_string();
        if name.eq_ignore_ascii_case(table) {
            let Value::Int(root) = r[3] else {
                return Err(corrupt("the root page is not a number"));
            };
            let sql = r[4].as_text().unwrap_or_default().to_string();
            if sql.is_empty() {
                return Err(Error::Query(format!(
                    "`{name}` is an internal table; it cannot be imported"
                )));
            }
            if without_rowid(&sql) {
                return Err(Error::Query(format!(
                    "`{name}` is declared `WITHOUT ROWID`; fennec-import only reads rowid tables"
                )));
            }
            return Ok((name, root as u32, sql));
        }
        tables.push(name);
    }
    tables.sort();
    Err(Error::NotFound(if tables.is_empty() {
        format!("there is no `{table}` table; no table was found in the file")
    } else {
        format!(
            "there is no `{table}` table; the tables in the file: {}",
            tables.join(", ")
        )
    }))
}

/// Infers the observed type per column by looking at the first `limit` rows.
fn sample_types(pager: &mut Pager, root: u32, table: &Table, limit: usize) -> Result<Vec<Seen>> {
    let mut seen = vec![Seen::Nothing; table.stored.len()];
    let mut walk = Walk::new(root);
    let mut n = 0;
    while n < limit {
        let Some(cell) = walk.next(pager)? else { break };
        let values = table.row(&cell, pager.encoding)?;
        for (s, v) in seen.iter_mut().zip(&values) {
            *s = s.join(Seen::of(v));
        }
        n += 1;
    }
    Ok(seen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_match_the_spec() {
        let case = |bytes: &[u8], want: i64| {
            let mut pos = 0;
            assert_eq!(varint(bytes, &mut pos).unwrap(), want, "{bytes:?}");
            assert_eq!(pos, bytes.len(), "{bytes:?}");
        };
        case(&[0x00], 0);
        case(&[0x7f], 127);
        case(&[0x81, 0x00], 128);
        case(&[0x82, 0x2f], 303);
        case(&[0xff, 0x7f], 16383);
        // The ninth byte contributes all eight bits.
        case(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff], -1);
    }

    #[test]
    fn signed_big_endian_sign_extends() {
        let mut p = 0;
        assert_eq!(be_int(&[0xff], &mut p, 1).unwrap(), -1);
        let mut p = 0;
        assert_eq!(be_int(&[0xff, 0xff, 0xff], &mut p, 3).unwrap(), -1);
        let mut p = 0;
        assert_eq!(be_int(&[0x00, 0x80], &mut p, 2).unwrap(), 128);
    }

    #[test]
    fn create_table_columns_are_parsed() {
        let d = parse_create(
            r#"CREATE TABLE docs (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 "head line" VARCHAR(255) NOT NULL DEFAULT 'x',
                 price DECIMAL(10,2),
                 embed BLOB,
                 untyped,
                 FOREIGN KEY (embed) REFERENCES t(id)
               )"#,
        )
        .unwrap();
        let names: Vec<&str> = d.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["id", "head line", "price", "embed", "untyped"]);
        assert_eq!(d[1].ty, "VARCHAR(255)");
        assert_eq!(
            d[2].ty, "DECIMAL(10,2)",
            "a parenthesised type must not be split"
        );
        assert_eq!(d[4].ty, "", "an untyped column carries an empty type");
        assert!(d[0].rowid_alias);
        assert!(!d[3].rowid_alias);
    }

    /// `COLLATE NOCASE` is a column compared without case, and so is the
    /// field it becomes; another collation is not.
    #[test]
    fn collate_nocase_is_a_nocase_field() {
        let d = parse_create(
            "CREATE TABLE u (email TEXT NOT NULL COLLATE NOCASE UNIQUE, name TEXT COLLATE \"nocase\", \
             code TEXT COLLATE BINARY, n INT)",
        )
        .unwrap();
        let nocase: Vec<bool> = d.iter().map(|x| x.nocase).collect();
        assert_eq!(nocase, [true, true, false, false]);
        assert_eq!(d[0].ty, "TEXT");
    }

    /// A table-level `PRIMARY KEY (x)` produces a rowid alias too.
    #[test]
    fn table_level_primary_key_is_rowid_alias() {
        let d = parse_create("CREATE TABLE t (a INTEGER, b TEXT, PRIMARY KEY (a))").unwrap();
        assert!(d[0].rowid_alias);
        assert!(!d[1].rowid_alias);
        // A multi-column primary key is not a rowid alias.
        let d = parse_create("CREATE TABLE t (a INTEGER, b INTEGER, PRIMARY KEY (a, b))").unwrap();
        assert!(!d[0].rowid_alias);
    }

    /// A space of more than one byte between a column's name and its type
    /// is a space: read a byte at a time, its lead byte began a word that
    /// ended at once, and the reading never moved on.
    #[test]
    fn a_space_of_several_bytes_ends_a_name_and_the_reading_goes_on() {
        let d =
            parse_create("CREATE TABLE t (a\u{a0}INTEGER, b\u{2003}TEXT\u{85}NOT NULL)").unwrap();
        let got: Vec<(&str, &str)> = d.iter().map(|x| (x.name.as_str(), x.ty.as_str())).collect();
        assert_eq!(got, [("a", "INTEGER"), ("b", "TEXT")]);
        assert_eq!(take_type("\u{a0}"), (String::from("\u{a0}"), ""));
    }

    #[test]
    fn quoted_identifiers_survive() {
        let d = parse_create(
            r#"CREATE TABLE "t" ([with space] TEXT, `back` INT, "double""quote" INT)"#,
        )
        .unwrap();
        let names: Vec<&str> = d.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["with space", "back", "double\"quote"]);
    }

    /// SQL's `create table` reads a declaration SQLite's way where it is
    /// none of PostgreSQL's names, as a SQLite column of it imports: the
    /// affinity rules are one, `fennec_ql::sqltypes`.
    #[test]
    fn create_table_reads_a_declaration_as_a_column_of_it_imports() {
        for decl in [
            "INTEGER",
            "TINYINT",
            "INT UNSIGNED",
            "MEDIUMTEXT",
            "NVARCHAR(30)",
            "CLOB",
            "BLOB",
            "DOUBLE",
            "FLOAT",
            "BOOLEAN",
            "DATETIME",
            "DATE",
        ] {
            let st = fennec_ql::parse_one(&format!("create table t (a {decl})"))
                .unwrap_or_else(|e| panic!("{decl}: {e}"));
            let fennec_core::query::Statement::CreateCollection { schema, .. } = st else {
                panic!("{decl}")
            };
            assert_eq!(
                Some(&schema.fields[0].ty),
                map_decl("a", decl).ty.as_ref(),
                "{decl}"
            );
        }
        for decl in ["DECIMAL(10,2)", "NUMERIC", "STRING"] {
            assert_eq!(map_decl("a", decl).ty, None, "{decl}");
            let sql = format!("create table t (a {decl})");
            assert!(fennec_ql::parse(&sql).is_err(), "{decl}");
        }
    }

    #[test]
    fn declared_types_map_to_fennecdb_types() {
        let t = |decl: &str| map_decl("x", decl).ty;
        assert_eq!(t("INTEGER"), Some(DataType::Int));
        assert_eq!(t("BIGINT"), Some(DataType::Int));
        assert_eq!(t("VARCHAR(80)"), Some(DataType::Text));
        assert_eq!(t("CLOB"), Some(DataType::Text));
        assert_eq!(t("BLOB"), Some(DataType::Bytes));
        assert_eq!(t("REAL"), Some(DataType::Float));
        assert_eq!(t("DOUBLE PRECISION"), Some(DataType::Float));
        assert_eq!(t("BOOLEAN"), Some(DataType::Bool));
        assert_eq!(t("DATETIME"), Some(DataType::Timestamp));
        assert_eq!(t("DATE"), Some(DataType::Timestamp));
        // The ones with no counterpart require `--cast`.
        assert_eq!(t("DECIMAL(10,2)"), None);
        assert_eq!(t("NUMERIC"), None);
        assert_eq!(t(""), None);
    }

    #[test]
    fn sampled_types_take_the_wider_side() {
        use Seen::*;
        assert_eq!(Nothing.join(Int), Int);
        assert_eq!(Int.join(Float), Float);
        assert_eq!(Int.join(Text), Text);
        assert_eq!(Text.join(Blob), Blob);
        assert_eq!(Nothing.ty(), DataType::Text);
        assert_eq!(Float.ty(), DataType::Float);
    }

    /// The overflow threshold must match the formula in the format document.
    #[test]
    fn overflow_threshold_matches_the_format() {
        let p = Pager {
            // The null device: `NUL` on Windows, read as the device alone.
            file: File::open(if cfg!(windows) { "NUL" } else { "/dev/null" }).unwrap(),
            len: 0,
            page_size: 4096,
            usable: 4096,
            encoding: Encoding::Utf8,
            spare: Vec::new(),
        };
        // X = U - 35: everything up to this size stays in the page.
        assert_eq!(p.local_len(10), 10);
        assert_eq!(p.local_len(4061), 4061);
        // Above it, it overflows and the local part does not exceed X.
        assert!(p.local_len(4062) < 4062);
        assert!(p.local_len(100_000) <= 4061);
    }

    /// A comment is a space, outside a quote; inside one it is text.
    #[test]
    fn comments_are_spaces_outside_quotes() {
        let s = strip_comments("a -- x, ( '\n b /* ) ' */ c 'not -- one' [d--] /* open");
        assert_eq!(s, "a  \n b   c 'not -- one' [d--]  ");
        assert!(matches!(
            strip_comments("no comment"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    /// A byte range is taken once: a repeat or an overlap is refused,
    /// across a word's edge too, and ranges side by side are not.
    #[test]
    fn a_byte_is_one_cells() {
        let mut bits = vec![0u64; 4];
        assert!(take(&mut bits, 10, 70));
        assert!(take(&mut bits, 70, 200));
        assert!(!take(&mut bits, 69, 71));
        assert!(!take(&mut bits, 199, 256));
        assert!(take(&mut bits, 200, 256));
        assert!(!take(&mut bits, 0, 11));
    }

    /// A default as SQLite reads it in a row lacking its column: each case
    /// as `SELECT quote(..)` read it from a file SQLite 3.51 wrote.
    #[test]
    fn defaults_read_as_sqlite_reads_them() {
        let read = |ty: &str, tail: &str| {
            let lit = constraints(tail).default.expect(tail);
            match lit {
                Literal::Unread(text) => Err(text),
                lit => Ok(constant(lit, affinity(ty))),
            }
        };
        let text = |s: &str| Ok(Value::Text(s.into()));
        assert_eq!(read("INTEGER", "NOT NULL DEFAULT 7"), Ok(Value::Int(7)));
        assert_eq!(read("TEXT", "DEFAULT 5"), text("5"));
        assert_eq!(read("REAL", "DEFAULT '2'"), Ok(Value::Float(2.0)));
        assert_eq!(read("", "DEFAULT -3.5"), Ok(Value::Float(-3.5)));
        assert_eq!(
            read("BLOB", "DEFAULT x'00ff'"),
            Ok(Value::Bytes(vec![0, 0xff]))
        );
        assert_eq!(read("", "DEFAULT TRUE"), Ok(Value::Int(1)));
        assert_eq!(read("NUMERIC", "DEFAULT '3.0e+5'"), Ok(Value::Int(300_000)));
        assert_eq!(read("INT", "DEFAULT (12)"), Ok(Value::Int(12)));
        assert_eq!(read("", "DEFAULT abc"), text("abc"));
        assert_eq!(read("TEXT", "DEFAULT -0x10"), text("-16"));
        assert_eq!(read("TEXT", "DEFAULT 1.50"), text("1.50"));
        assert_eq!(read("TEXT", "DEFAULT TRUE"), text("1"));
        assert_eq!(read("", "DEFAULT NULL"), Ok(Value::Null));
        assert_eq!(read("", "DEFAULT 1_000"), Ok(Value::Int(1000)));
        assert_eq!(read("", "DEFAULT \"quoted\""), text("quoted"));
        assert_eq!(
            read("", "DEFAULT 99999999999999999999"),
            Ok(Value::Float(1e20))
        );
        // What no import computes is named, never read as a value.
        assert_eq!(
            read("", "DEFAULT CURRENT_TIMESTAMP"),
            Err("CURRENT_TIMESTAMP".into())
        );
        assert_eq!(read("", "DEFAULT (a + 1)"), Err("(a + 1)".into()));
        assert_eq!(read("", "DEFAULT (abc)"), Err("(abc)".into()));
        // A foreign key's `SET DEFAULT` is no default.
        assert_eq!(
            constraints("REFERENCES p(id) ON DELETE SET DEFAULT").default,
            None
        );
    }

    /// A constraint's words are read as tokens: `PRIMARY KEY` in a string
    /// is no key, and `DESC` after a column's is SQLite's exception.
    #[test]
    fn constraints_are_read_as_tokens() {
        let c = constraints("DEFAULT ' PRIMARY KEY ' CHECK (x COLLATE NOCASE)");
        assert!(!c.primary_key && !c.nocase);
        let c = constraints("CONSTRAINT pk PRIMARY KEY DESC");
        assert!(c.primary_key && c.descending);
        let c = constraints("GENERATED ALWAYS AS (a + 1) STORED NOT NULL");
        assert_eq!(c.generated, Some(Generated::Stored));
        assert_eq!(constraints("AS (a)").generated, Some(Generated::Virtual));
        assert_eq!(
            table_primary_key("CONSTRAINT pk PRIMARY KEY (\"Id\" DESC)"),
            Some("Id".into())
        );
        assert_eq!(table_primary_key("PRIMARY KEY (a, b)"), None);
        assert!(without_rowid(
            "CREATE TABLE \"a(\" (x) /* ) */ WITHOUT ROWID"
        ));
        assert!(!without_rowid("CREATE TABLE t (x) STRICT"));
    }

    /// SQLite's numeric affinity on a text: an integer where it is one or
    /// where a real is one exactly, a real otherwise, and text that is no
    /// number stays text.
    #[test]
    fn numeric_affinity_reads_a_text_as_sqlite_does() {
        assert_eq!(numeric(" 42 "), Some(Value::Int(42)));
        assert_eq!(numeric("5."), Some(Value::Int(5)));
        assert_eq!(numeric("1e5"), Some(Value::Int(100_000)));
        assert_eq!(numeric(".5"), Some(Value::Float(0.5)));
        assert_eq!(
            numeric("9223372036854775808"),
            Some(Value::Float(2f64.powi(63)))
        );
        assert_eq!(numeric("0x10"), None);
        assert_eq!(numeric("1-2"), None);
        assert_eq!(numeric("inf"), None);
        assert_eq!(numeric(""), None);
    }
}

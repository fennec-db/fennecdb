//! The coverage walk over a release's fixture set, shared by fennec-core's
//! `compat::` (the database files, the sync logs, the page's IndexedDB
//! records) and fennec-http's (the sealed backup, the archives): every
//! byte of every file read by the format's own layout, written here from
//! the release's encoders (`Schema::encode`, `codec.rs`, `engine.rs`'s
//! record kinds, `engine/stored.rs`, `vector.rs`'s flat graph record,
//! `synclog.rs`, `seal.rs`, `archive.rs`), never through this build's
//! readers. It reads both ways: what the release writes and no file holds
//! fails ([`RELEASES`]), and so does what a file holds that the release's
//! encoders cannot write -- a record kind, a tag, an op, a flag byte -- or
//! a byte nobody read, named by its file and offset.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

/// What a release writes, by its own source, and a set of it holds: every
/// record kind, statement op, graph version, schema tag, value tag and
/// index kind, two ways -- each in a file, and no file holding another --
/// and every byte-level alternative its encoders emit ([`ALTERNATIVES`]):
/// in a file of its set (`shapes`), or named with why in `absent`, which
/// the set's README.md names too. A set without its line fails, and so
/// does a line without its set. A landed set never changes: `sums` pins
/// the SHA-256 of its `SHA256SUMS`, so a set written again whole fails. A
/// form found missing after a set landed is held by a further set of the
/// same tag (`<tag>+2`, ...), a line of its own written by binaries built
/// from the same commit, never by amending the first.
pub struct Writes {
    /// The set's directory: its tag, or `<tag>+<n>` for a further set.
    pub set: &'static str,
    /// The release whose binaries wrote it.
    pub tag: &'static str,
    /// The SHA-256 of its `SHA256SUMS`, in hex.
    pub sums: &'static str,
    pub kinds: &'static [u8],
    pub ops: &'static [u8],
    pub graphs: &'static [u8],
    pub schema_tags: &'static [u8],
    pub value_tags: &'static [u8],
    pub indexes: &'static [u8],
    pub shapes: &'static [&'static str],
    pub absent: &'static [(&'static str, &'static str)],
}

/// Every byte-level alternative the walk tells apart in a file a release
/// wrote, past the kinds, ops, versions, tags and index kinds counted
/// beside them: the forms of a record's head, each record kind's body, a
/// schema's field, a value, a graph record, the sync log, a seal, an
/// archive and the page's stores. The walk names none outside it, and the
/// newest release's line places each: in a file, or absent with why.
pub const ALTERNATIVES: &[&str] = &[
    // A file's front, and a record's head.
    "front: counter head",
    "front: records",
    "claim: none",
    "claim: 1 byte",
    "claim: 2 bytes",
    "claim: 3 bytes",
    "claim: 4 bytes",
    "head: collection id of 1 byte",
    "head: collection id of 2 bytes",
    "head: collection id of 3 bytes",
    "head: collection id of 4 bytes",
    "head: collection id of 5 bytes",
    "head: length of 1 byte",
    "head: length of 2 bytes",
    "head: length of 3 bytes",
    "head: length of 4 bytes",
    "head: length of 5 bytes",
    // What a crash leaves.
    "torn: kind 3 cut short",
    "torn: kind 14 failing its sum",
    "damage: kind 3 failing its sum, records after it",
    // A data record's frames and the documents in them.
    "frame: put",
    "frame: del",
    "document: every place",
    "document: fewer values than places",
    "value: bool false",
    "value: bool true",
    // Numbers below zero and past their type's range: a zigzag's sign, a
    // float's infinity, an f32's and an f16's.
    "value: int below zero",
    "value: timestamp before 1970",
    "value: float infinite",
    "value: vector component infinite",
    "value: f16 component past 65 504",
    // What a release wrote that this build refuses on purpose
    // (docs/design/crates.md, *How deep a value nests* and *Limits*).
    "value: nesting past 64 levels",
    "type: lists nested past 64",
    "type: a point under 64 lists",
    "type: a list of vectors past the dimension's range",
    // The id counter, and its index of the data record's frames; the
    // counter against the image's live ids.
    "next id: alone",
    "next id: index of one share",
    "next id: index of several shares",
    "next id: the live ids' next",
    "next id: past the live ids",
    // The collections the database's own code makes (`declared::OWN`).
    "own: _migrations",
    "own: _idempotency",
    "own: _consumers",
    "own: _follow",
    "own: _hook_events",
    "own: _sync_shapes",
    "own: _sync_queue",
    "own: _sync_temps",
    // A history: kind 8, and an archive's history file.
    "history: taking writes",
    "history: following",
    "history: forked none",
    "history: forked once",
    "history: forked twice",
    // A block, a spill and a land, and what each holds.
    "block: across collections",
    "block: holds kind 1",
    "block: holds kind 2",
    "block: holds kind 3",
    "block: holds kind 5",
    "block: holds kind 12",
    "block: holds kind 13",
    "spill: holds kind 1",
    "spill: holds kind 2",
    "spill: holds kind 3",
    "spill: holds kind 5",
    "spill: holds kind 12",
    "spill: holds kind 13",
    "land: holds kind 1",
    "land: holds kind 2",
    "land: holds kind 3",
    "land: holds kind 5",
    "land: holds kind 12",
    "land: holds kind 13",
    "spill: landed",
    "spill: dead",
    // A field record's change.
    "fields: 1 add",
    "fields: 2 drop",
    "fields: 3 rename",
    "fields: 4 ttl",
    // A statement record: a binding's events, a schedule, a slot's mark.
    "bound: insert",
    "bound: set",
    "bound: del",
    "bound: insert or set",
    "bound: insert or del",
    "bound: set or del",
    "bound: insert or set or del",
    "schedule: every",
    "schedule: months",
    "schedule: cron",
    "schedule: zone utc",
    "schedule: zone named",
    "schedule: zone offset",
    "schedule: every from the epoch",
    "schedule: every from an origin",
    "statement: a number past a float's range in its text",
    "mark: ran",
    "mark: refused",
    "mark: none passed over",
    "mark: slots passed over",
    // The statements a file holds at its end, and their last marks.
    "live schedule: every",
    "live schedule: months",
    "live schedule: cron",
    "live schedule: zone utc",
    "live schedule: zone named",
    "live schedule: zone offset",
    "live schedule: last slot ran",
    "live schedule: last slot refused",
    "live schedule: no slot run, its since alone",
    "synced mark",
    // A schema's fields.
    "field: dropped place",
    "field: required",
    "field: optional",
    "field tags: collated",
    "field tags: nocase",
    "field tags: beside",
    "field tags: collated nocase",
    "field tags: collated beside",
    "field tags: nocase beside",
    "field tags: collated nocase beside",
    "collation: tr",
    "collation: und",
    "beside: text",
    "beside: text chars",
    "beside: text accents",
    "beside: text chars accents",
    "type: list of bool",
    "type: list of int",
    "type: list of float",
    "type: list of text",
    "type: list of bytes",
    "type: list of vector",
    "type: list of list",
    "type: list of vector f16",
    "type: list of timestamp",
    "type: list of sparse",
    "type: list of geo",
    "type: list of json inside a list",
    "index: metric cosine",
    "index: metric l2",
    "index: metric dot",
    "index: quant int8",
    "index: quant bit",
    "index: text accents",
    "index: text chars accents",
    "index: text k1 not its default",
    "index: text b not its default",
    "index: text prefix not its default",
    "index: text prefix_min not its default",
    "index: hnsw m not its default",
    "index: hnsw ef_construction not its default",
    "index: hnsw ef_search not its default",
    "path: index kind 1",
    "path: index kind 4",
    "path: index kind 8",
    "composite: plain",
    "composite: unique",
    "composite: part ascending",
    "composite: part descending",
    // A flat graph record.
    "graph: cosine",
    "graph: l2",
    "graph: dot",
    "graph: arena f32",
    "graph: arena f16",
    "graph: arena int8",
    "graph: arena int8 of an f16 field",
    "graph: arena bit codes",
    "graph: arena bit codes of an f16 field",
    "graph: bit index holding its vectors whole",
    "graph: bit centres",
    "graph: tombstone in arena f32",
    "graph: tombstone in arena f16",
    "graph: tombstone in arena int8",
    "graph: tombstone in arena bit codes",
    "graph: node waiting",
    "graph: doc ids of 4 bytes",
    "graph: doc ids of 8 bytes",
    "graph: links of 2 bytes",
    "graph: links of 3 bytes",
    "graph: links of 4 bytes",
    "graph: list empty",
    "graph: list of one",
    "graph: steps of 1 byte",
    "graph: steps of 2 bytes",
    "graph: steps of 3 bytes",
    "graph: steps of 4 bytes",
    "graph: upper layers",
    "graph: aliases",
    "graph: halves",
    // The sync log beside a file.
    "sync log: entry",
    "sync log: zeros after the last entry",
    "sync log: a past pass's bytes after the last entry",
    "sync log: an entry torn",
    "sync log: header wiped, a spent generation after it",
    "sync log: all zeros, never begun",
    // A sealed file, an archive, the page's stores.
    "seal: whole, one frame",
    "seal: whole, several frames",
    "seal: appended frames",
    "archive: sealed",
    "archive: plain",
    "archive: image",
    "archive: segment",
    "archive: history file",
    "segment: write",
    "node archive: line followed",
    "node archive: line moved aside",
    "node archive: began",
    "node archive: presence gone",
    "node archive: presence back",
    "page: image",
    "page: chunk",
    "page: sealed image",
    "page: sealed chunk",
    "page: a rewrite's copy beside its file",
    "durable object: sql image",
    "durable object: sql writes",
    "durable object: values image",
    "durable object: values writes",
];

/// The sets in the tree, oldest first: a release's, then any further set
/// of the same tag.
pub const RELEASES: &[Writes] = &[Writes {
    set: "v0.2.0",
    tag: "v0.2.0",
    sums: "9a209946cb5a9df35bbc79e9101085011e7efa9a663eb7a41136ff1618484089",
    kinds: &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14],
    ops: &[1, 2, 3, 4, 5, 6],
    graphs: &[9, 10],
    schema_tags: &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 15, 16, 17, 18],
    value_tags: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 13, 15],
    indexes: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
    shapes: V0_2_0_SHAPES,
    absent: &[
        (
            "claim: 3 bytes",
            "a record more than 256 MiB past the file's last fsync: 256 MiB of working tree",
        ),
        (
            "claim: 4 bytes",
            "a record more than 64 GiB past the file's last fsync",
        ),
        (
            "head: length of 5 bytes",
            "a record of 256 MiB: one collection's data in an image that large, 256 MiB of working tree",
        ),
        (
            "graph: links of 4 bytes",
            "a graph past 16 777 216 nodes: some 300 MB of graph record",
        ),
        (
            "graph: steps of 4 bytes",
            "a step past 16 777 215 between a list's links: a graph past 16 777 216 nodes",
        ),
    ],
}];

const V0_2_0_SHAPES: &[&str] = &[
    "front: counter head",
    "front: records",
    "claim: none",
    "claim: 1 byte",
    "claim: 2 bytes",
    "head: collection id of 1 byte",
    "head: collection id of 2 bytes",
    "head: collection id of 3 bytes",
    "head: collection id of 4 bytes",
    "head: collection id of 5 bytes",
    "head: length of 1 byte",
    "head: length of 2 bytes",
    "head: length of 3 bytes",
    "head: length of 4 bytes",
    "torn: kind 3 cut short",
    "torn: kind 14 failing its sum",
    "damage: kind 3 failing its sum, records after it",
    "frame: put",
    "frame: del",
    "document: every place",
    "document: fewer values than places",
    "value: bool false",
    "value: bool true",
    "value: int below zero",
    "value: timestamp before 1970",
    "value: float infinite",
    "value: vector component infinite",
    "value: f16 component past 65 504",
    "value: nesting past 64 levels",
    "type: lists nested past 64",
    "type: a point under 64 lists",
    "type: a list of vectors past the dimension's range",
    "next id: alone",
    "next id: index of one share",
    "next id: index of several shares",
    "next id: the live ids' next",
    "next id: past the live ids",
    "own: _migrations",
    "own: _idempotency",
    "own: _consumers",
    "own: _follow",
    "own: _hook_events",
    "own: _sync_shapes",
    "own: _sync_queue",
    "own: _sync_temps",
    "history: taking writes",
    "history: following",
    "history: forked none",
    "history: forked once",
    "history: forked twice",
    "block: across collections",
    "block: holds kind 1",
    "block: holds kind 2",
    "block: holds kind 3",
    "block: holds kind 5",
    "block: holds kind 12",
    "block: holds kind 13",
    "spill: holds kind 1",
    "spill: holds kind 2",
    "spill: holds kind 3",
    "spill: holds kind 5",
    "spill: holds kind 12",
    "spill: holds kind 13",
    "land: holds kind 1",
    "land: holds kind 2",
    "land: holds kind 3",
    "land: holds kind 5",
    "land: holds kind 12",
    "land: holds kind 13",
    "spill: landed",
    "spill: dead",
    "fields: 1 add",
    "fields: 2 drop",
    "fields: 3 rename",
    "fields: 4 ttl",
    "bound: insert",
    "bound: set",
    "bound: del",
    "bound: insert or set",
    "bound: insert or del",
    "bound: set or del",
    "bound: insert or set or del",
    "schedule: every",
    "schedule: months",
    "schedule: cron",
    "schedule: zone utc",
    "schedule: zone named",
    "schedule: zone offset",
    "schedule: every from the epoch",
    "schedule: every from an origin",
    "statement: a number past a float's range in its text",
    "mark: ran",
    "mark: refused",
    "mark: none passed over",
    "mark: slots passed over",
    "live schedule: every",
    "live schedule: months",
    "live schedule: cron",
    "live schedule: zone utc",
    "live schedule: zone named",
    "live schedule: zone offset",
    "live schedule: last slot ran",
    "live schedule: last slot refused",
    "live schedule: no slot run, its since alone",
    "synced mark",
    "field: dropped place",
    "field: required",
    "field: optional",
    "field tags: collated",
    "field tags: nocase",
    "field tags: beside",
    "field tags: collated nocase",
    "field tags: collated beside",
    "field tags: nocase beside",
    "field tags: collated nocase beside",
    "collation: tr",
    "collation: und",
    "beside: text",
    "beside: text chars",
    "beside: text accents",
    "beside: text chars accents",
    "type: list of bool",
    "type: list of int",
    "type: list of float",
    "type: list of text",
    "type: list of bytes",
    "type: list of vector",
    "type: list of list",
    "type: list of vector f16",
    "type: list of timestamp",
    "type: list of sparse",
    "type: list of geo",
    "type: list of json inside a list",
    "index: metric cosine",
    "index: metric l2",
    "index: metric dot",
    "index: quant int8",
    "index: quant bit",
    "index: text accents",
    "index: text chars accents",
    "index: text k1 not its default",
    "index: text b not its default",
    "index: text prefix not its default",
    "index: text prefix_min not its default",
    "index: hnsw m not its default",
    "index: hnsw ef_construction not its default",
    "index: hnsw ef_search not its default",
    "path: index kind 1",
    "path: index kind 4",
    "path: index kind 8",
    "composite: plain",
    "composite: unique",
    "composite: part ascending",
    "composite: part descending",
    "graph: cosine",
    "graph: l2",
    "graph: dot",
    "graph: arena f32",
    "graph: arena f16",
    "graph: arena int8",
    "graph: arena int8 of an f16 field",
    "graph: arena bit codes",
    "graph: arena bit codes of an f16 field",
    "graph: bit index holding its vectors whole",
    "graph: bit centres",
    "graph: tombstone in arena f32",
    "graph: tombstone in arena f16",
    "graph: tombstone in arena int8",
    "graph: tombstone in arena bit codes",
    "graph: node waiting",
    "graph: doc ids of 4 bytes",
    "graph: doc ids of 8 bytes",
    "graph: links of 2 bytes",
    "graph: links of 3 bytes",
    "graph: list empty",
    "graph: list of one",
    "graph: steps of 1 byte",
    "graph: steps of 2 bytes",
    "graph: steps of 3 bytes",
    "graph: upper layers",
    "graph: aliases",
    "graph: halves",
    "sync log: entry",
    "sync log: zeros after the last entry",
    "sync log: a past pass's bytes after the last entry",
    "sync log: an entry torn",
    "sync log: header wiped, a spent generation after it",
    "sync log: all zeros, never begun",
    "seal: whole, one frame",
    "seal: whole, several frames",
    "seal: appended frames",
    "archive: sealed",
    "archive: plain",
    "archive: image",
    "archive: segment",
    "archive: history file",
    "segment: write",
    "node archive: line followed",
    "node archive: line moved aside",
    "node archive: began",
    "node archive: presence gone",
    "node archive: presence back",
    "page: image",
    "page: chunk",
    "page: sealed image",
    "page: sealed chunk",
    "page: a rewrite's copy beside its file",
    "durable object: sql image",
    "durable object: sql writes",
    "durable object: values image",
    "durable object: values writes",
];

/// The alternatives only fennec-http's walk meets: a seal is opened by
/// its `seal.rs`, which fennec-core does not have.
pub fn sealed_only(shape: &str) -> bool {
    ["seal:", "archive:", "segment:", "node archive:"]
        .iter()
        .any(|p| shape.starts_with(p))
}

/// The line of `set`, failing where there is none.
pub fn writes_of(set: &str) -> &'static Writes {
    RELEASES
        .iter()
        .find(|w| w.set == set)
        .unwrap_or_else(|| panic!("{set}: no line in RELEASES naming what its release writes"))
}

// ------------------------------------------------------------- reading

/// A cursor over a part of a file, failing where the bytes are not the
/// format's: by the file's name and the offset in it.
pub struct Cur<'a> {
    pub file: &'a str,
    pub b: &'a [u8],
    pub at: usize,
    /// Where `b` starts in the file.
    pub base: usize,
}

impl<'a> Cur<'a> {
    pub fn new(file: &'a str, b: &'a [u8], base: usize) -> Cur<'a> {
        Cur {
            file,
            b,
            at: 0,
            base,
        }
    }

    #[track_caller]
    pub fn fail(&self, what: &str) -> ! {
        panic!("{}: offset {}: {what}", self.file, self.base + self.at)
    }

    pub fn left(&self) -> usize {
        self.b.len() - self.at
    }

    pub fn done(&self) -> bool {
        self.at == self.b.len()
    }

    pub fn peek(&self) -> Option<u8> {
        self.b.get(self.at).copied()
    }

    #[track_caller]
    pub fn u8(&mut self) -> u8 {
        let Some(&v) = self.b.get(self.at) else {
            self.fail("ends where a byte is due")
        };
        self.at += 1;
        v
    }

    #[track_caller]
    pub fn take(&mut self, n: usize) -> &'a [u8] {
        if n > self.left() {
            self.fail(&format!("{n} bytes due, {} left", self.left()))
        }
        let s = &self.b[self.at..self.at + n];
        self.at += n;
        s
    }

    #[track_caller]
    pub fn le(&mut self, n: usize) -> u64 {
        let mut w = [0u8; 8];
        w[..n].copy_from_slice(self.take(n));
        u64::from_le_bytes(w)
    }

    /// A varint and how many bytes it took.
    #[track_caller]
    pub fn uvw(&mut self) -> (u64, usize) {
        let from = self.at;
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8();
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return (v, self.at - from);
            }
        }
        self.fail("a varint past 64 bits")
    }

    #[track_caller]
    pub fn uv(&mut self) -> u64 {
        self.uvw().0
    }

    #[track_caller]
    pub fn str(&mut self) -> &'a str {
        let n = self.uv() as usize;
        let raw = self.take(n);
        std::str::from_utf8(raw).unwrap_or_else(|_| self.fail("a text not UTF-8"))
    }

    /// The next `n` bytes as a cursor of their own.
    #[track_caller]
    pub fn sub(&mut self, n: usize) -> Cur<'a> {
        let base = self.base + self.at;
        Cur::new(self.file, self.take(n), base)
    }

    /// Every byte read: `what` ends here.
    #[track_caller]
    pub fn end(&self, what: &str) {
        if !self.done() {
            self.fail(&format!(
                "{} bytes after {what} that nobody read",
                self.left()
            ))
        }
    }
}

/// What the walk met, file by file or over a set.
#[derive(Default)]
pub struct Seen {
    pub kinds: BTreeSet<u8>,
    pub ops: BTreeSet<u8>,
    pub graphs: BTreeSet<u8>,
    pub schema_tags: BTreeSet<u8>,
    pub value_tags: BTreeSet<u8>,
    pub indexes: BTreeSet<u8>,
    pub shapes: BTreeSet<&'static str>,
    /// The collections by id, as the file's creates name them, and how
    /// many places each one's documents hold.
    pub names: BTreeMap<u64, String>,
    pub places: BTreeMap<u64, usize>,
    pub kept: BTreeMap<(u64, String), Kept>,
    /// Each spill by where its body is and its length: landed or not.
    pub spills: BTreeMap<(usize, usize), bool>,
    /// Each checked record whose sum holds and that claims a point: where
    /// it starts and the point, as the release's layout reads its claim.
    pub claims: Vec<(usize, usize)>,
    /// Each collection's schema as its last record writes it, by id.
    pub schemas: BTreeMap<u64, Facts>,
    /// Each collection's id counter as an image's kind 7 writes it, and the
    /// highest id a landed put frame wrote, by collection id: the next id
    /// is past both ([`Seen::next_ids`]).
    pub counters: BTreeMap<u64, u64>,
    pub puts: BTreeMap<u64, u64>,
    /// The put frames of each spill, by where its body is: counted once a
    /// land names it, never where it was put back.
    spill_puts: BTreeMap<(usize, usize), Vec<(u64, u64)>>,
    /// The spill whose records are being read, and whether the image's
    /// records are: its counters, and the ids its frames leave live.
    in_spill: Option<(usize, usize)>,
    in_image: bool,
    image_counters: Vec<(u64, u64)>,
    image_live: BTreeMap<u64, BTreeSet<u64>>,
    /// A record failing its sum or cut short was met: what an open keeps of
    /// the frames is its claims', which the walk does not judge.
    pub damaged: bool,
    /// The named statements defined at the file's end, by name, and the
    /// last mark of each name's slots.
    pub statements: BTreeMap<String, Def>,
    pub marks: BTreeMap<String, Mark>,
    /// The file's history as its last kind-8 record writes it: following,
    /// and each fork's id and change.
    pub history: Option<(bool, Vec<(u64, u64)>)>,
    /// A sync log whose header was wiped: where its first stale entry put
    /// its bytes in the file.
    pub spent_log: Option<u64>,
}

/// A named statement's definition as its record holds it: its version,
/// and a scheduled one's timing -- the times' tag (1 every, 2 months, 3
/// cron) and values, the zone and `since`.
#[derive(Debug, Clone, PartialEq)]
pub struct Def {
    pub version: (u64, u64),
    pub timing: Option<(u8, Vec<u64>, String, i64)>,
}

/// A slot's mark as its record (op 5) holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub at: i64,
    pub version: (u64, u64),
    /// Refused: its status and reason; `None` where it ran.
    pub refused: Option<(u16, String)>,
    pub skipped: u64,
}

/// Whether a statement's text holds a number no `f64` holds (`1e400`): a
/// run of digits, a point and an exponent, read as Rust reads a float.
fn past_range(text: &str) -> bool {
    text.split(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '+' | '-')))
        .map(|w| w.trim_start_matches(['-', '+']))
        .filter(|w| w.starts_with(|ch: char| ch.is_ascii_digit()) && w.contains(['e', 'E']))
        .any(|w| w.parse::<f64>().is_ok_and(f64::is_infinite))
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// What a schema's bytes say of a collection's fields, read by the walk:
/// what this build's reading of the same bytes is held to.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Facts {
    pub name: String,
    pub fields: Vec<FieldFacts>,
    /// Each index over several fields: unique, and its parts, descending
    /// or not.
    pub composites: Vec<(bool, Vec<(String, bool)>)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldFacts {
    pub name: String,
    pub required: bool,
    /// The collation's code.
    pub collate: Option<u8>,
    pub nocase: bool,
    /// Its own index's kind byte, and the numbers after it ([`index`]).
    pub index: u8,
    pub options: Vec<u64>,
    /// The `@text` beside its own: chars, accents, and its numbers.
    pub beside: Option<(bool, bool)>,
    pub beside_options: Vec<u64>,
}

/// A graph whose record a file keeps -- its collection's last -- and what
/// a later record did to its collection: what an open restores.
pub struct Kept {
    pub name: String,
    pub field: String,
    pub version: u8,
    pub body: Vec<u8>,
    /// A write to the collection after the record: the graph restored
    /// takes it, so it is no longer the record's bytes.
    pub touched: bool,
    /// A node waiting to be linked: an open links it.
    pub waiting: bool,
}

impl Seen {
    #[track_caller]
    pub fn shape(&mut self, s: &str) {
        let Some(&known) = ALTERNATIVES.iter().find(|&&a| a == s) else {
            panic!("the walk names `{s}`, which ALTERNATIVES lacks")
        };
        self.shapes.insert(known);
    }

    fn touch(&mut self, cid: u64) {
        for ((c, _), k) in self.kept.iter_mut() {
            if *c == cid {
                k.touched = true;
            }
        }
    }

    pub fn add(&mut self, other: &Seen) {
        self.kinds.extend(&other.kinds);
        self.ops.extend(&other.ops);
        self.graphs.extend(&other.graphs);
        self.schema_tags.extend(&other.schema_tags);
        self.value_tags.extend(&other.value_tags);
        self.indexes.extend(&other.indexes);
        self.shapes.extend(&other.shapes);
    }

    /// The id the next row put into each live collection takes, by name, as
    /// the file's records say it: past its image's counter and every id a
    /// landed put frame wrote, from 1.
    pub fn next_ids(&self) -> BTreeMap<String, u64> {
        self.names
            .iter()
            .map(|(cid, name)| {
                let counter = self.counters.get(cid).copied().unwrap_or(1);
                let put = self.puts.get(cid).map_or(1, |&id| id + 1);
                (name.clone(), counter.max(put).max(1))
            })
            .collect()
    }

    /// What a file says once every record of it was read: its spills
    /// landed or dead, and each scheduled statement alive at its end, by
    /// its timing and its last slot's mark.
    pub fn finish(&mut self) {
        self.close_spills();
        let live: Vec<(Def, Option<Mark>)> = self
            .statements
            .iter()
            .filter(|(_, d)| d.timing.is_some())
            .map(|(name, d)| {
                let mark = self.marks.get(name).filter(|m| m.version == d.version);
                (d.clone(), mark.cloned())
            })
            .collect();
        for (d, mark) in live {
            let (tag, _, zone, _) = d.timing.as_ref().unwrap();
            self.shape(match tag {
                1 => "live schedule: every",
                2 => "live schedule: months",
                _ => "live schedule: cron",
            });
            self.shape(match zone.as_bytes().first() {
                None => "live schedule: zone utc",
                Some(b'+' | b'-') => "live schedule: zone offset",
                Some(_) => "live schedule: zone named",
            });
            self.shape(match mark.map(|m| m.refused.is_some()) {
                Some(false) => "live schedule: last slot ran",
                Some(true) => "live schedule: last slot refused",
                None => "live schedule: no slot run, its since alone",
            });
        }
    }

    /// The landed and dead spills, once every record of a file was read.
    pub fn close_spills(&mut self) {
        let spills: Vec<bool> = self.spills.values().copied().collect();
        for landed in spills {
            self.shape(match landed {
                true => "spill: landed",
                false => "spill: dead",
            });
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "kinds {:?}, ops {:?}, graphs {:?}, schema tags {:?}, value tags {:?}, indexes {:?}\n    {:?}",
            self.kinds,
            self.ops,
            self.graphs,
            self.schema_tags,
            self.value_tags,
            self.indexes,
            self.shapes
        )
    }
}

fn bytes_word(n: usize) -> &'static str {
    match n {
        1 => "1 byte",
        2 => "2 bytes",
        3 => "3 bytes",
        4 => "4 bytes",
        5 => "5 bytes",
        _ => "more bytes",
    }
}

// ------------------------------------------------------------- values

/// A value, by the codec's layout: every tag a release writes into a
/// document and nothing else.
fn value(c: &mut Cur, seen: &mut Seen) {
    value_in(c, seen, 0)
}

/// A value inside `levels` lists and objects: past 64 in all, a value a
/// release wrote -- a 64-level json value in a `[[json]]` field -- and this
/// build refuses where it reads it ("a value nests more than 64 levels
/// deep").
fn value_in(c: &mut Cur, seen: &mut Seen, levels: usize) {
    let tag = c.u8();
    if matches!(tag, 7 | 13) && levels == 64 {
        seen.shape("value: nesting past 64 levels");
    }
    seen.value_tags.insert(tag);
    match tag {
        0 => {}
        1 => match c.u8() {
            0 => seen.shape("value: bool false"),
            1 => seen.shape("value: bool true"),
            b => c.fail(&format!("a bool of byte {b}")),
        },
        // A zigzag: its low bit is the sign.
        2 | 9 => {
            if c.uv() & 1 == 1 {
                seen.shape(match tag {
                    2 => "value: int below zero",
                    _ => "value: timestamp before 1970",
                });
            }
        }
        3 => {
            if f64::from_le_bytes(c.take(8).try_into().unwrap()).is_infinite() {
                seen.shape("value: float infinite");
            }
        }
        4 => {
            c.str();
        }
        5 => {
            let n = c.uv() as usize;
            c.take(n);
        }
        6 => {
            let n = c.uv() as usize;
            let past = c
                .take(4 * n)
                .as_chunks::<4>()
                .0
                .iter()
                .any(|b| f32::from_le_bytes(*b).is_infinite());
            if past {
                seen.shape("value: vector component infinite");
            }
        }
        // An f16 past 65 504 is its infinity: the exponent's bits all set,
        // no mantissa.
        8 => {
            let n = c.uv() as usize;
            let past = c
                .take(2 * n)
                .as_chunks::<2>()
                .0
                .iter()
                .any(|b| u16::from_le_bytes(*b) & 0x7fff == 0x7c00);
            if past {
                seen.shape("value: f16 component past 65 504");
            }
        }
        7 => {
            for _ in 0..c.uv() {
                value_in(c, seen, levels + 1);
            }
        }
        13 => {
            // Its members ascending by key, as the codec writes them and a
            // path's read relies on.
            let mut last: Option<&[u8]> = None;
            for _ in 0..c.uv() {
                let k = c.str().as_bytes();
                if last.is_some_and(|l| l >= k) {
                    c.fail("an object's keys out of order");
                }
                last = Some(k);
                value_in(c, seen, levels + 1);
            }
        }
        10 => {
            c.uv();
            let n = c.uv() as usize;
            for _ in 0..n {
                c.uv();
            }
            c.take(4 * n);
        }
        15 => {
            c.take(16);
        }
        t => {
            c.at -= 1;
            c.fail(&format!("a value tag the release writes none of: {t}"))
        }
    }
}

// ------------------------------------------------------------- schemas

/// A type as a schema writes it inside `lists` lists: its tag, and a
/// list's inner type. Past 64 lists, a point under 64 and a vector in a
/// list past a dimension's range are a release's that this build refuses
/// at the open, or will (docs/design/crates.md).
fn ty(c: &mut Cur, seen: &mut Seen, lists: usize) -> u8 {
    let in_list = lists > 0;
    let tag = c.u8();
    match tag {
        1..=5 | 9 | 14 => {}
        15 => {
            if lists >= 64 {
                seen.shape("type: a point under 64 lists");
            }
        }
        6 | 8 | 10 => {
            let dim = c.uv();
            if dim == 0 {
                c.fail("a dimension of 0");
            }
            if in_list && dim > 1_000_000_000 {
                seen.shape("type: a list of vectors past the dimension's range");
            }
        }
        7 => {
            if lists == 64 {
                seen.shape("type: lists nested past 64");
            }
            let inner = ty(c, seen, lists + 1);
            seen.shape(match inner {
                1 => "type: list of bool",
                2 => "type: list of int",
                3 => "type: list of float",
                4 => "type: list of text",
                5 => "type: list of bytes",
                6 => "type: list of vector",
                7 => "type: list of list",
                8 => "type: list of vector f16",
                9 => "type: list of timestamp",
                10 => "type: list of sparse",
                15 => "type: list of geo",
                // v0.2.0 refuses `[json]`, a field's own list of json, and
                // not one inside another list (`[[json]]`).
                14 if in_list => "type: list of json inside a list",
                t => c.fail(&format!("a list of type {t}, which the release refuses")),
            });
        }
        t => {
            c.at -= 1;
            c.fail(&format!(
                "a schema tag the release writes none of in a type{}: {t}",
                if in_list { " a list holds" } else { "" }
            ))
        }
    }
    seen.schema_tags.insert(tag);
    tag
}

/// An index's kind and its options, as a schema writes one: its kind,
/// and every number after it in order -- a graph's metric, `m`,
/// `ef_construction`, `ef_search` and quantization, a text index's flags,
/// k1 and b in hundredths, longest and shortest prefix, an expiry's
/// milliseconds -- which this build's reading is held to. Each option
/// away from the release's default is a form of its own (`Default for
/// VectorIndexSpec`, `TextIndexSpec`: m 16, ef_construction 200,
/// ef_search 100 or a bit index's 200; k1 0.9, b 0.4, prefix 0, prefix_min
/// 3).
fn index(c: &mut Cur, seen: &mut Seen) -> (u8, Vec<u64>) {
    let kind = c.u8();
    let mut opts = Vec::new();
    match kind {
        0 | 1 | 4 | 6 | 8 | 10 => {}
        2 | 5 => {
            let metric = c.u8();
            seen.shape(match metric {
                0 => "index: metric cosine",
                1 => "index: metric l2",
                2 => "index: metric dot",
                m => c.fail(&format!("a metric the release writes none of: {m}")),
            });
            let (m, construction, search) = (c.uv(), c.uv(), c.uv());
            opts.extend([metric as u64, m, construction, search]);
            let mut quant = 0;
            if kind == 5 {
                quant = c.u8();
                seen.shape(match quant {
                    1 => "index: quant int8",
                    2 => "index: quant bit",
                    q => c.fail(&format!("a quantization kind 5 never holds: {q}")),
                });
                opts.push(quant as u64);
            }
            if m != 16 {
                seen.shape("index: hnsw m not its default");
            }
            if construction != 200 {
                seen.shape("index: hnsw ef_construction not its default");
            }
            if search != if quant == 2 { 200 } else { 100 } {
                seen.shape("index: hnsw ef_search not its default");
            }
        }
        3 | 7 | 11 => {
            if kind == 11 {
                let flags = c.u8();
                seen.shape(match flags {
                    2 => "index: text accents",
                    3 => "index: text chars accents",
                    f => c.fail(&format!("text options the release writes none of: {f}")),
                });
                opts.push(flags as u64);
            }
            let (k1, b, longest, shortest) = (c.uv(), c.uv(), c.uv(), c.uv());
            opts.extend([k1, b, longest, shortest]);
            for (set, shape) in [
                (k1 != 90, "index: text k1 not its default"),
                (b != 40, "index: text b not its default"),
                (longest != 0, "index: text prefix not its default"),
                (shortest != 3, "index: text prefix_min not its default"),
            ] {
                if set {
                    seen.shape(shape);
                }
            }
        }
        9 => {
            opts.push(c.uv());
        }
        k => {
            c.at -= 1;
            c.fail(&format!("an index kind the release writes none of: {k}"))
        }
    }
    seen.indexes.insert(kind);
    (kind, opts)
}

/// A schema (`Schema::encode`), every byte: its places, each field's tags,
/// type, `required` byte, index and paths, the dropped places and the
/// indexes over several fields. How many places its documents hold.
pub fn schema(c: &mut Cur, seen: &mut Seen) -> (Facts, usize) {
    let name = c.str().to_string();
    let n = c.uv() as usize;
    let mut places = 0;
    let mut facts = Facts {
        name: name.clone(),
        ..Facts::default()
    };
    for at in 0..n {
        let fname = c.str();
        if c.peek() == Some(12) {
            c.u8();
            if !fname.is_empty() {
                c.fail("a dropped place with a name");
            }
            seen.schema_tags.insert(12);
            seen.shape("field: dropped place");
            places += 1;
            continue;
        }
        if fname.is_empty() && c.peek() == Some(16) {
            c.u8();
            if at + 1 != n {
                c.fail("indexes over several fields before the last place");
            }
            seen.schema_tags.insert(16);
            for _ in 0..c.uv() {
                let unique = c.u8();
                seen.shape(match unique {
                    0 => "composite: plain",
                    1 => "composite: unique",
                    b => c.fail(&format!("a unique byte of {b}")),
                });
                let parts = c.uv();
                if parts < 2 {
                    c.fail("an index over several fields of fewer than two");
                }
                let mut keys = Vec::new();
                for _ in 0..parts {
                    let key = c.str().to_string();
                    let desc = c.u8();
                    seen.shape(match desc {
                        0 => "composite: part ascending",
                        1 => "composite: part descending",
                        b => c.fail(&format!("a descending byte of {b}")),
                    });
                    keys.push((key, desc == 1));
                }
                facts.composites.push((unique == 1, keys));
            }
            continue;
        }
        if fname.is_empty() {
            c.fail("a field with no name");
        }
        places += 1;
        let mut f = FieldFacts {
            name: fname.to_string(),
            required: false,
            collate: None,
            nocase: false,
            index: 0,
            options: Vec::new(),
            beside: None,
            beside_options: Vec::new(),
        };
        let mut tags = Vec::new();
        if c.peek() == Some(11) {
            c.u8();
            seen.schema_tags.insert(11);
            let code = c.u8();
            seen.shape(match code {
                1 => "collation: tr",
                2 => "collation: und",
                k => c.fail(&format!("a collation the release writes none of: {k}")),
            });
            f.collate = Some(code);
            tags.push("collated");
        }
        if c.peek() == Some(17) {
            c.u8();
            seen.schema_tags.insert(17);
            f.nocase = true;
            tags.push("nocase");
        }
        if c.peek() == Some(18) {
            c.u8();
            seen.schema_tags.insert(18);
            let from = c.at;
            let (kind, opts) = index(c, seen);
            f.beside_options = opts;
            let (shape, spec) = match (kind, c.b[from + 1..].first()) {
                (3, _) => ("beside: text", (false, false)),
                (7, _) => ("beside: text chars", (true, false)),
                (11, Some(2)) => ("beside: text accents", (false, true)),
                (11, _) => ("beside: text chars accents", (true, true)),
                _ => {
                    c.at = from;
                    c.fail("an index beside a field's own that is no text index")
                }
            };
            seen.shape(shape);
            f.beside = Some(spec);
            tags.push("beside");
        }
        if !tags.is_empty() {
            seen.shape(match tags.join(" ").as_str() {
                "collated" => "field tags: collated",
                "nocase" => "field tags: nocase",
                "beside" => "field tags: beside",
                "collated nocase" => "field tags: collated nocase",
                "collated beside" => "field tags: collated beside",
                "nocase beside" => "field tags: nocase beside",
                _ => "field tags: collated nocase beside",
            });
        }
        let t = ty(c, seen, 0);
        let required = c.u8();
        seen.shape(match required {
            0 => "field: optional",
            1 => "field: required",
            b => c.fail(&format!("a required byte of {b}")),
        });
        f.required = required == 1;
        (f.index, f.options) = index(c, seen);
        facts.fields.push(f);
        if t == 14 {
            for _ in 0..c.uv() {
                c.str();
                let from = c.at;
                let (kind, _) = index(c, seen);
                seen.shape(match kind {
                    1 => "path: index kind 1",
                    4 => "path: index kind 4",
                    8 => "path: index kind 8",
                    _ => {
                        c.at = from;
                        c.fail("an index on a path that is neither a hash nor an ordered one")
                    }
                });
            }
        }
    }
    (facts, places)
}

// ------------------------------------------------------------- graphs

/// A flat graph record's body past its field's name (`vector.rs`'s
/// `serialize_into`, versions 9 and 10), every byte of it: its head --
/// dimension, metric, beams, precision, quantization and a bit index's
/// centres -- the nodes' documents, flags, levels and level-0 lengths,
/// every list, the tombstones' vectors, the aliases and the copies table's
/// halves. Whether a node waits.
fn graph(c: &mut Cur, seen: &mut Seen) -> bool {
    let version = c.u8();
    if !matches!(version, 9 | 10) {
        c.fail(&format!(
            "a graph record of version {version}, which the release writes no"
        ));
    }
    seen.graphs.insert(version);
    let dim = c.uv() as usize;
    seen.shape(match c.u8() {
        0 => "graph: cosine",
        1 => "graph: l2",
        2 => "graph: dot",
        m => c.fail(&format!("a metric the release writes none of: {m}")),
    });
    for _ in 0..3 {
        c.uv();
    }
    let half = match c.u8() {
        0 => false,
        1 => true,
        p => c.fail(&format!("a precision the release writes none of: {p}")),
    };
    let quant = c.u8();
    let mut centres = 0;
    if quant == 3 {
        centres = c.uv() as usize;
        c.take(centres * dim * 4);
        seen.shape(match centres {
            0 => "graph: bit index holding its vectors whole",
            _ => "graph: bit centres",
        });
    }
    let (arena, stored) = match (quant, centres > 0, half) {
        (1, _, false) => ("int8", dim + 4),
        (1, _, true) => ("int8 of an f16 field", dim + 4),
        (3, true, false) => ("bit codes", dim.div_ceil(64) * 8 + 9),
        (3, true, true) => ("bit codes of an f16 field", dim.div_ceil(64) * 8 + 9),
        (0 | 3, false, false) => ("f32", dim * 4),
        (0 | 3, false, true) => ("f16", dim * 2),
        (q, ..) => c.fail(&format!("a quantization the release writes none of: {q}")),
    };
    seen.shape(match arena {
        "int8" => "graph: arena int8",
        "int8 of an f16 field" => "graph: arena int8 of an f16 field",
        "bit codes" => "graph: arena bit codes",
        "bit codes of an f16 field" => "graph: arena bit codes of an f16 field",
        "f32" => "graph: arena f32",
        _ => "graph: arena f16",
    });
    let n = c.uv() as usize;
    if c.uv() as usize > n {
        c.fail("an entry past the nodes");
    }
    if c.uv() > 0 {
        seen.shape("graph: upper layers");
    }
    let (lw, dw) = (c.u8() as usize, c.u8() as usize);
    seen.shape(match lw {
        2 => "graph: links of 2 bytes",
        3 => "graph: links of 3 bytes",
        4 => "graph: links of 4 bytes",
        w => c.fail(&format!("links of {w} bytes")),
    });
    seen.shape(match dw {
        4 => "graph: doc ids of 4 bytes",
        8 => "graph: doc ids of 8 bytes",
        w => c.fail(&format!("document ids of {w} bytes")),
    });
    c.take(n * dw);
    let flags = c.take(n).to_vec();
    if let Some(f) = flags.iter().find(|&&f| f > 2) {
        c.fail(&format!("a node flag the release writes none of: {f}"));
    }
    let levels = c.take(n).to_vec();
    let l0: Vec<usize> = (0..n).map(|_| c.le(2) as usize).collect();
    let list = |c: &mut Cur, seen: &mut Seen, k: usize| match k {
        0 => seen.shape("graph: list empty"),
        1 => {
            c.take(lw);
            seen.shape("graph: list of one");
        }
        _ => {
            c.take(lw);
            let w = c.u8() as usize;
            seen.shape(match w {
                1 => "graph: steps of 1 byte",
                2 => "graph: steps of 2 bytes",
                3 => "graph: steps of 3 bytes",
                4 => "graph: steps of 4 bytes",
                _ => c.fail(&format!("steps of {w} bytes")),
            });
            c.take((k - 1) * w);
        }
    };
    for &k in &l0 {
        list(c, seen, k);
    }
    for &level in &levels {
        for _ in 0..level {
            let k = c.le(2) as usize;
            list(c, seen, k);
        }
    }
    let dead = flags.iter().filter(|&&f| f == 1).count();
    if dead > 0 {
        seen.shape(match arena {
            "int8" | "int8 of an f16 field" => "graph: tombstone in arena int8",
            "bit codes" | "bit codes of an f16 field" => "graph: tombstone in arena bit codes",
            "f32" => "graph: tombstone in arena f32",
            _ => "graph: tombstone in arena f16",
        });
    }
    c.take(dead * stored);
    let waiting = flags.contains(&2);
    if waiting {
        seen.shape("graph: node waiting");
    }
    let aliases = c.le(8) as usize;
    c.take(aliases * (lw + 8));
    if aliases > 0 {
        seen.shape("graph: aliases");
    }
    if n > 0 {
        if c.u8() != 1 {
            c.at -= 1;
            c.fail("a graph's halves behind a byte other than 1");
        }
        c.take(4 * n);
        seen.shape("graph: halves");
    }
    c.end("a graph record's halves");
    waiting
}

// ------------------------------------------------------------- records

/// A varint at `at` and how many bytes it took, where it is whole.
fn varint_at(b: &[u8], at: usize) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for (i, shift) in (0..64).step_by(7).enumerate() {
        let x = *b.get(at + i)?;
        v |= ((x & 0x7f) as u64) << shift;
        if x & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

/// A record's head and where its body is: plain, as inside a block, or
/// checked, its claim and sum held to its bytes.
struct Head {
    kind: u8,
    cid: u64,
    body: (usize, usize),
    claim: Option<u64>,
    torn: Option<&'static str>,
}

/// The records of `c` to its end, outer -- each with its claim and sum, a
/// torn last one named -- or inside a block, a spill or a land.
fn records(c: &mut Cur, seen: &mut Seen, holder: Option<&str>) {
    let mut cids = BTreeSet::new();
    while !c.done() {
        let start = c.at;
        let k = c.u8();
        let checked = k & 0x80 != 0;
        if holder.is_some() == checked || (!checked && k & 0x60 != 0) {
            c.at = start;
            c.fail(&match holder {
                Some(h) => format!("a record in a {h} with a sum or a claim: kind byte {k}"),
                None => {
                    format!("a record with no sum, which the release writes none of: kind byte {k}")
                }
            });
        }
        let kind = k & 0x1f;
        let cut = |c: &mut Cur, seen: &mut Seen| {
            seen.kinds.insert(kind);
            seen.damaged = true;
            seen.shape(match kind {
                3 => "torn: kind 3 cut short",
                _ => {
                    c.at = start;
                    c.fail(&format!("kind {kind} cut short"))
                }
            });
            c.at = c.b.len();
        };
        // A head a crash cut short ends the file.
        let head = varint_at(c.b, c.at)
            .and_then(|(cid, cw)| varint_at(c.b, c.at + cw).map(|(len, lw)| (cid, cw, len, lw)));
        let Some((cid, cw, len, lw)) = head else {
            if holder.is_some() {
                c.fail("a record's head cut short in a block");
            }
            cut(c, seen);
            break;
        };
        c.at += cw + lw;
        let len = len as usize;
        if len > c.left() {
            if holder.is_some() {
                c.fail("a record cut short in a block");
            }
            cut(c, seen);
            break;
        }
        let mut h = Head {
            kind,
            cid,
            body: (c.at, len),
            claim: None,
            torn: None,
        };
        if checked {
            seen.shape(match cw {
                1 => "head: collection id of 1 byte",
                2 => "head: collection id of 2 bytes",
                3 => "head: collection id of 3 bytes",
                4 => "head: collection id of 4 bytes",
                _ => "head: collection id of 5 bytes",
            });
            seen.shape(match lw {
                1 => "head: length of 1 byte",
                2 => "head: length of 2 bytes",
                3 => "head: length of 3 bytes",
                4 => "head: length of 4 bytes",
                _ => "head: length of 5 bytes",
            });
            let width = ((k & 0x60) >> 5) as usize + 1;
            if len < width + 4 {
                c.fail("a record shorter than its claim and sum");
            }
            let sum_at = c.at + len - 4;
            let want = u32::from_le_bytes(c.b[sum_at..sum_at + 4].try_into().unwrap());
            if fennec_core::crc::crc32c(0, &c.b[start..sum_at]) != want {
                h.torn = Some("failing its sum");
            }
            let mut w = [0u8; 8];
            w[..width].copy_from_slice(&c.b[sum_at - width..sum_at]);
            let claim = u64::from_le_bytes(w);
            h.claim = Some(claim);
            h.body.1 = len - width - 4;
            if h.torn.is_none() {
                seen.shape(match (claim, width) {
                    (0, _) => "claim: none",
                    (_, 1) => "claim: 1 byte",
                    (_, 2) => "claim: 2 bytes",
                    (_, 3) => "claim: 3 bytes",
                    _ => "claim: 4 bytes",
                });
                if claim > 0 {
                    let at = c.base + start;
                    let back = (claim as usize - 1) * 4096;
                    seen.claims.push((at, at.saturating_sub(back)));
                }
            }
        }
        let body_base = c.base + h.body.0;
        let mut body = Cur::new(c.file, &c.b[h.body.0..h.body.0 + h.body.1], body_base);
        c.at += len;
        seen.kinds.insert(kind);
        // A record failing its sum ends the file where a crash tore it, or
        // has records after it: a bit flipped where the file was synced,
        // which an open reads unchecked below the durable point a later
        // claim names. Its body is read as any other's.
        if let Some(why) = h.torn {
            seen.damaged = true;
            seen.shape(match (kind, c.done()) {
                (14, true) => "torn: kind 14 failing its sum",
                (3, false) => "damage: kind 3 failing its sum, records after it",
                _ => {
                    c.at = start;
                    c.fail(&format!("kind {kind} {why}"))
                }
            });
            if kind == 14 {
                break;
            }
        }
        if let Some(holder) = holder {
            match kind {
                1 | 2 | 3 | 5 | 12 | 13 => {}
                _ => {
                    c.at = start;
                    c.fail(&format!("kind {kind} in a {holder}, which never holds one"))
                }
            }
            seen.shape(&format!("{holder}: holds kind {kind}"));
            if kind == 3 {
                cids.insert(cid);
            }
        }
        record(&h, &mut body, seen, holder.is_none());
    }
    if holder == Some("block") && cids.len() > 1 {
        seen.shape("block: across collections");
    }
}

/// A record's body, read to its end by its kind's layout.
fn record(h: &Head, c: &mut Cur, seen: &mut Seen, outer: bool) {
    let cid = h.cid;
    match h.kind {
        1 | 5 => {
            let (facts, places) = schema(c, seen);
            c.end("a schema");
            if h.kind == 1 {
                seen.kept.retain(|(k, _), _| *k != cid);
                seen.names.insert(cid, facts.name.clone());
                // An image makes each collection again under its id: its
                // counter and frames follow. A collection made anew is a
                // new id.
                if !seen.in_image {
                    seen.counters.remove(&cid);
                    seen.puts.remove(&cid);
                }
                if facts.name.starts_with('_') {
                    seen.shape(&format!("own: {}", facts.name));
                }
            } else {
                seen.touch(cid);
            }
            seen.places.insert(cid, places);
            seen.schemas.insert(cid, facts);
        }
        2 => {
            c.end("a drop, which has no body");
            seen.counters.remove(&cid);
            seen.puts.remove(&cid);
            seen.kept.retain(|(k, _), _| *k != cid);
            seen.names.remove(&cid);
            seen.schemas.remove(&cid);
        }
        3 => {
            let places = *seen.places.get(&cid).unwrap_or_else(|| {
                c.fail(&format!("frames of collection {cid}, which no record made"))
            });
            while !c.done() {
                let op = c.u8();
                seen.shape(match op {
                    1 => "frame: put",
                    2 => "frame: del",
                    _ => {
                        c.at -= 1;
                        c.fail(&format!("a frame op the release writes none of: {op}"))
                    }
                });
                let id = c.uv();
                let n = c.uv() as usize;
                let mut doc = c.sub(n);
                if seen.in_image {
                    let live = seen.image_live.entry(cid).or_default();
                    match op {
                        1 => live.insert(id),
                        _ => live.remove(&id),
                    };
                }
                if op == 1 {
                    match seen.in_spill {
                        Some(spill) => seen.spill_puts.entry(spill).or_default().push((cid, id)),
                        None => {
                            let top = seen.puts.entry(cid).or_insert(0);
                            *top = (*top).max(id);
                        }
                    }
                }
                if op == 2 {
                    doc.end("a del frame's head, which holds no document");
                    continue;
                }
                let mut values = 0;
                while !doc.done() {
                    value(&mut doc, seen);
                    values += 1;
                }
                seen.shape(match values.cmp(&places) {
                    std::cmp::Ordering::Equal => "document: every place",
                    std::cmp::Ordering::Less => "document: fewer values than places",
                    std::cmp::Ordering::Greater => {
                        doc.fail(&format!("{values} values in a document of {places} places"))
                    }
                });
            }
            seen.touch(cid);
        }
        4 => {
            if !outer {
                c.fail("a graph record inside another");
            }
            let field = c.str().to_string();
            let from = c.at;
            let waiting = graph(c, seen);
            if let Some(name) = seen.names.get(&cid).cloned() {
                seen.kept.insert(
                    (cid, field.clone()),
                    Kept {
                        name,
                        field,
                        version: c.b[from],
                        body: c.b[from..].to_vec(),
                        touched: false,
                        waiting,
                    },
                );
            }
        }
        6 => c.fail("a counter head past the image's front"),
        7 => {
            let counter = c.uv();
            seen.counters.insert(cid, counter);
            if seen.in_image {
                seen.image_counters.push((cid, counter));
            }
            match c.peek() {
                None => seen.shape("next id: alone"),
                Some(2) => {
                    c.u8();
                    c.uv();
                    c.uv();
                    let count = c.uv() as usize;
                    for _ in 0..count {
                        c.uv();
                        c.uv();
                        c.uv();
                    }
                    // The shares' ends, 24 bytes each, and how many.
                    if c.left() < 4 || !(c.left() - 4).is_multiple_of(24) {
                        c.fail("the shares' ends are not 24 bytes each and their count");
                    }
                    let shares = (c.left() - 4) / 24;
                    for _ in 0..shares * 3 {
                        c.le(8);
                    }
                    if c.le(4) as usize != shares || shares != count.div_ceil(16_384).max(1) {
                        c.fail(&format!("{shares} shares' ends for {count} documents"));
                    }
                    seen.shape(match shares {
                        1 => "next id: index of one share",
                        _ => "next id: index of several shares",
                    });
                }
                Some(b) => c.fail(&format!(
                    "an index of the frames the release writes none of: {b}"
                )),
            }
            c.end("the id counter's index");
        }
        8 => {
            seen.history = Some(history(c, seen));
            c.end("a history");
        }
        9 => {
            if !outer {
                c.fail("a block inside another");
            }
            records(c, seen, Some("block"));
        }
        10 => {
            if !outer {
                c.fail("a spill inside another record");
            }
            seen.spills.entry((c.base, c.b.len())).or_insert(false);
            seen.in_spill = Some((c.base, c.b.len()));
            records(c, seen, Some("spill"));
            seen.in_spill = None;
        }
        11 => {
            if !outer {
                c.fail("a land inside another record");
            }
            for _ in 0..c.uv() {
                let place = c.uv() as usize;
                let len = c.uv() as usize;
                for (cid, id) in seen.spill_puts.remove(&(place, len)).unwrap_or_default() {
                    let top = seen.puts.entry(cid).or_insert(0);
                    *top = (*top).max(id);
                }
                match seen.spills.get_mut(&(place, len)) {
                    Some(landed) => *landed = true,
                    None => c.fail(&format!(
                        "a land naming a spill at {place} the file holds none at"
                    )),
                }
            }
            records(c, seen, Some("land"));
        }
        12 => {
            seen.shape(match c.u8() {
                1 => "fields: 1 add",
                2 => "fields: 2 drop",
                3 => "fields: 3 rename",
                4 => "fields: 4 ttl",
                k => {
                    c.at -= 1;
                    c.fail(&format!("a field change the release writes none of: {k}"))
                }
            });
            c.str();
            c.str();
            let (facts, places) = schema(c, seen);
            c.end("a field change's schema");
            seen.places.insert(cid, places);
            seen.schemas.insert(cid, facts);
            seen.touch(cid);
        }
        13 => {
            statement(c, seen);
            c.end("a statement record");
        }
        14 => {
            c.end("a close's mark, which has no body");
            if h.claim != Some(1) {
                c.fail("a close's mark claiming other than 1");
            }
            seen.shape("synced mark");
        }
        k => c.fail(&format!("a record kind the release writes none of: {k}")),
    }
}

/// A history (`History::encode`): kind 8's body, and an archive's
/// `history` file.
pub fn history(c: &mut Cur, seen: &mut Seen) -> (bool, Vec<(u64, u64)>) {
    let following = c.u8();
    seen.shape(match following {
        0 => "history: taking writes",
        1 => "history: following",
        b => c.fail(&format!("a following byte of {b}")),
    });
    let forks = c.uv();
    let mut lineage = Vec::new();
    for _ in 0..forks {
        let id = c.le(8);
        lineage.push((id, c.uv()));
    }
    seen.shape(match forks {
        0 => "history: forked none",
        1 => "history: forked once",
        _ => "history: forked twice",
    });
    (following == 1, lineage)
}

/// A named statement's record (`engine/stored.rs`), every op's fields.
fn statement(c: &mut Cur, seen: &mut Seen) {
    let op = c.u8();
    seen.ops.insert(op);
    if op == 5 {
        let name = c.str().to_string();
        let at = unzigzag(c.uv());
        let version = (c.uv(), c.uv());
        let outcome = c.u8();
        let status = c.uv();
        let skipped = c.uv();
        let reason = c.str();
        seen.shape(match (outcome, status) {
            (1, 0) if reason.is_empty() => "mark: ran",
            (2, 100..=599) => "mark: refused",
            _ => c.fail(&format!(
                "a slot's outcome the release writes none of: {outcome} {status}"
            )),
        });
        seen.shape(match skipped {
            0 => "mark: none passed over",
            _ => "mark: slots passed over",
        });
        let refused = (outcome == 2).then(|| (status as u16, reason.to_string()));
        seen.marks.insert(
            name,
            Mark {
                at,
                version,
                refused,
                skipped,
            },
        );
        return;
    }
    if !matches!(op, 1 | 2 | 3 | 4 | 6) {
        c.at -= 1;
        c.fail(&format!("a statement op the release writes none of: {op}"));
    }
    let version = (c.uv(), c.uv());
    c.uv();
    c.le(8);
    let name = c.str().to_string();
    for _ in 0..c.uv() {
        c.str();
    }
    if op == 3 {
        c.str();
        seen.shape(match c.u8() {
            1 => "bound: insert",
            2 => "bound: set",
            4 => "bound: del",
            3 => "bound: insert or set",
            5 => "bound: insert or del",
            6 => "bound: set or del",
            7 => "bound: insert or set or del",
            e => c.fail(&format!("events the release binds none of: {e}")),
        });
    }
    let text = c.str();
    if op == 2 && !text.is_empty() {
        c.fail("a drop's text");
    }
    if past_range(text) {
        seen.shape("statement: a number past a float's range in its text");
    }
    let mut timing = None;
    if op == 4 {
        let tag = c.u8();
        let n = match tag {
            1 => {
                seen.shape("schedule: every");
                2
            }
            2 => {
                seen.shape("schedule: months");
                1
            }
            3 => {
                seen.shape("schedule: cron");
                5
            }
            t => c.fail(&format!("a schedule the release writes none of: {t}")),
        };
        let values: Vec<u64> = (0..n).map(|_| c.uv()).collect();
        if tag == 1 {
            seen.shape(match values[1] {
                0 => "schedule: every from the epoch",
                _ => "schedule: every from an origin",
            });
        }
        let zone = c.str().to_string();
        seen.shape(match zone.as_bytes().first() {
            None => "schedule: zone utc",
            Some(b'+' | b'-') => "schedule: zone offset",
            Some(_) => "schedule: zone named",
        });
        let since = unzigzag(c.uv());
        timing = Some((tag, values, zone, since));
    }
    if op == 6 && c.str().is_empty() {
        c.fail("a hook with no name");
    }
    match op {
        2 => {
            seen.statements.remove(&name);
        }
        _ => {
            seen.statements.insert(name, Def { version, timing });
        }
    }
}

/// A database's bytes (`FENECDB\x01`, the counter's head where an image
/// begins it, the records): what every record holds.
pub fn database(bytes: &[u8], file: &str, seen: &mut Seen) {
    // What one file says of its collections, statements and history is its
    // own: a walk over several (fennec-http's) reads each afresh.
    seen.names.clear();
    seen.places.clear();
    seen.schemas.clear();
    seen.counters.clear();
    seen.puts.clear();
    seen.statements.clear();
    seen.marks.clear();
    seen.history = None;
    seen.spills.clear();
    seen.spill_puts.clear();
    let mut c = Cur::new(file, bytes, 0);
    if c.take(8) != b"FENECDB\x01" {
        c.fail("no fennecdb signature");
    }
    image_front(&mut c, seen);
    records(&mut c, seen, None);
}

/// The counter's fixed-width head where an image begins: its body's
/// length ends on a record's boundary, which the walk checks after.
fn image_front(c: &mut Cur, seen: &mut Seen) {
    if c.peek() == Some(6) {
        c.u8();
        c.le(8);
        let len = c.le(8) as usize;
        if len > c.left() {
            c.fail("an image's body past the file");
        }
        seen.shape("front: counter head");
        // `REC_SEQ`: the counter's head, kind 6, a plain record of its
        // own fixed layout, the image it heads its length.
        seen.kinds.insert(6);
        // The image's records end exactly where its head says.
        let mut image = c.sub(len);
        seen.in_image = true;
        records(&mut image, seen, None);
        seen.in_image = false;
        // Each collection's counter against the ids its image keeps live:
        // past them where a row was deleted before the image was taken.
        for (cid, counter) in std::mem::take(&mut seen.image_counters) {
            let next = seen
                .image_live
                .get(&cid)
                .and_then(|l| l.last())
                .map_or(1, |&top| top + 1);
            seen.shape(match counter.cmp(&next) {
                std::cmp::Ordering::Equal => "next id: the live ids' next",
                std::cmp::Ordering::Greater => "next id: past the live ids",
                std::cmp::Ordering::Less => image.fail(&format!(
                    "an id counter of {counter} below a live id's {next}"
                )),
            });
        }
        seen.image_live.clear();
    } else {
        seen.shape("front: records");
    }
}

/// A stream of outer records with no signature: a journal's chunk.
pub fn chunk(bytes: &[u8], file: &str, seen: &mut Seen) {
    records(&mut Cur::new(file, bytes, 0), seen, None);
}

/// An archive's segment, as it reads once its seal is off:
/// `FENECLG\x01`, then each write's time and record. Each write's time
/// and the changes its record holds, in order.
pub fn segment(bytes: &[u8], file: &str, seen: &mut Seen) -> Vec<(u64, u64)> {
    let mut c = Cur::new(file, bytes, 0);
    if c.take(8) != b"FENECLG\x01" {
        c.fail("no segment signature");
    }
    let mut writes = Vec::new();
    while !c.done() {
        let time = c.le(8);
        // The record's head says its length: one record a write.
        let start = c.at;
        let mut probe = Cur::new(file, &c.b[start..], c.base + start);
        probe.u8();
        probe.uv();
        let len = probe.uv() as usize;
        let mut one = c.sub(probe.at + len);
        writes.push((time, changes_in(&mut Cur::new(file, one.b, one.base))));
        records(&mut one, seen, None);
        seen.shape("segment: write");
    }
    writes
}

/// The changes a record moves the counter by, as the release counts them
/// (`engine::writes_in`): a data record's frames, a block's and a land's
/// records -- a data record's frames each, any other one -- a schema's,
/// a drop's, an alter's, a field change's and a statement's one, the rest
/// none.
fn changes_in(c: &mut Cur) -> u64 {
    let k = c.u8();
    let checked = k & 0x80 != 0;
    c.uv();
    let len = c.uv() as usize;
    let room = match checked {
        true => ((k & 0x60) >> 5) as usize + 1 + 4,
        false => 0,
    };
    let mut body = c.sub(len);
    body.b = &body.b[..len - room];
    let frames = |b: &mut Cur| {
        let mut n = 0;
        while !b.done() {
            b.u8();
            b.uv();
            let skip = b.uv() as usize;
            b.take(skip);
            n += 1;
        }
        n
    };
    let inner = |b: &mut Cur| {
        let mut n = 0;
        while !b.done() {
            let kind = b.u8() & 0x1f;
            b.uv();
            let len = b.uv() as usize;
            let mut r = b.sub(len);
            n += match kind {
                3 => frames(&mut r),
                _ => 1,
            };
        }
        n
    };
    match k & 0x1f {
        3 => frames(&mut body),
        9 => inner(&mut body),
        11 => {
            for _ in 0..body.uv() {
                body.uv();
                body.uv();
            }
            inner(&mut body)
        }
        1 | 2 | 5 | 12 | 13 => 1,
        _ => 0,
    }
}

/// A sealed file's frames (`seal.rs`): its magic, then each frame's length,
/// nonce, ciphertext and tag, to the file's end. Where each frame ends.
pub fn seal_frames(bytes: &[u8], file: &str) -> Vec<usize> {
    let mut c = Cur::new(file, bytes, 0);
    if c.take(8) != b"FENECSL\x01" {
        c.fail("no seal's signature");
    }
    let mut ends = Vec::new();
    while !c.done() {
        let n = c.le(4) as usize;
        c.take(12);
        c.take(n + 16);
        ends.push(c.at);
    }
    ends
}

// ------------------------------------------------------------- sync log

/// The sync log's own hash (`synclog.rs`), eight bytes a step: a part of
/// its format.
pub fn log_hash(seed: u64, bytes: &[u8]) -> u64 {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut h = seed ^ (bytes.len() as u64).wrapping_mul(0xA076_1D64_78BD_642F);
    let (words, rest) = bytes.as_chunks::<8>();
    for w in words {
        h = (h ^ u64::from_le_bytes(*w)).wrapping_mul(K).rotate_left(29);
    }
    let mut last = [0u8; 8];
    last[..rest.len()].copy_from_slice(rest);
    h = (h ^ u64::from_le_bytes(last)).wrapping_mul(K);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    h ^ (h >> 33)
}

const LOG_HEAD: usize = 4096;
const LOG_ROOM: usize = 256 << 10;
const ENTRY_MAGIC: u32 = 0x4E59_5346;

/// An entry's head and bytes at `pos` where they are one whole: its
/// generation, place in the file and bytes.
fn log_entry(log: &[u8], pos: usize) -> Option<(u64, u64, &[u8])> {
    let head = log.get(pos..pos + 32)?;
    let word = |i: usize| u64::from_le_bytes(head[i..i + 8].try_into().unwrap());
    if u32::from_le_bytes(head[..4].try_into().unwrap()) != ENTRY_MAGIC {
        return None;
    }
    let len = u32::from_le_bytes(head[4..8].try_into().unwrap()) as usize;
    let bytes = log.get(pos + 32..pos + 32 + len)?;
    let (generation, at) = (word(8), word(16));
    (word(24) == log_hash(generation ^ at.rotate_left(17), bytes))
        .then_some((generation, at, bytes))
}

/// The sync log beside `file` (`synclog.rs`), every byte: its header --
/// the generation, where the file stood synced, the fingerprint of the
/// bytes before it, the file's device and inode, its hash -- then this
/// generation's entries, each put back into the file's bytes as an open
/// puts it, then what follows the last: zeros, an earlier pass's bytes --
/// the tail of an entry written over, its own whole entries, zeros -- or an
/// entry of this generation torn. The file's bytes as an open reads them.
pub fn sync_log(file: &[u8], log: &[u8], name: &str, seen: &mut Seen) -> Vec<u8> {
    let c = Cur::new(name, log, 0);
    if log.len() != LOG_HEAD + LOG_ROOM {
        c.fail(&format!("a log of {} bytes, not its fixed size", log.len()));
    }
    let word = |i: usize| u64::from_le_bytes(log[i..i + 8].try_into().unwrap());
    // A header of zeros names no file: a log made and never begun, all
    // zeros, or one an open spent -- its entries put back, the file synced
    // and the header wiped, so that they are never put back again. The file
    // reads as it lies.
    if log[..LOG_HEAD].iter().all(|&b| b == 0) {
        if log.iter().all(|&b| b == 0) {
            seen.shape("sync log: all zeros, never begun");
            return file.to_vec();
        }
        let Some((g, first, _)) = log_entry(log, LOG_HEAD) else {
            Cur::new(name, log, LOG_HEAD).fail("a wiped header and no whole entry after it")
        };
        // Its entries, one generation's in a run, each the bytes the file
        // holds where it puts them: the open that wiped it put them there.
        let (mut pos, mut at) = (LOG_HEAD, first);
        while let Some((g2, a2, bytes)) = log_entry(log, pos) {
            if g2 != g || a2 != at {
                break;
            }
            if file.get(at as usize..at as usize + bytes.len()) != Some(bytes) {
                Cur::new(name, log, pos).fail("a spent entry the file does not hold");
            }
            pos += 32 + bytes.len();
            at += bytes.len() as u64;
        }
        if log[pos..].iter().any(|&b| b != 0) {
            Cur::new(name, log, pos)
                .fail("bytes after a spent generation's entries that are not zeros");
        }
        seen.shape("sync log: header wiped, a spent generation after it");
        seen.spent_log = Some(first);
        return file.to_vec();
    }
    if &log[..8] != b"FENECSYN" || log[8..12] != 2u32.to_le_bytes() || log[12..16] != [0; 4] {
        c.fail("no sync log's header of version 2");
    }
    if word(56) != log_hash(0, &log[..56]) {
        c.fail("a header failing its hash");
    }
    if log[64..LOG_HEAD].iter().any(|&b| b != 0) {
        Cur::new(name, log, 64).fail("bytes in the header's block past the header");
    }
    let (generation, base) = (word(16), word(24) as usize);
    if base > file.len() || base < 8 {
        c.fail(&format!("a base of {base} past a file of {}", file.len()));
    }
    let from = base.saturating_sub(4096);
    if word(32) != log_hash(base as u64, &file[from..base]) {
        c.fail("a fingerprint other than the file's bytes before its base");
    }
    let mut out = file.to_vec();
    let (mut pos, mut at) = (LOG_HEAD, base);
    while let Some((g, a, bytes)) = log_entry(log, pos) {
        if g != generation || a as usize != at {
            break;
        }
        if out.len() < at + bytes.len() {
            out.resize(at + bytes.len(), 0);
        }
        out[at..at + bytes.len()].copy_from_slice(bytes);
        seen.shape("sync log: entry");
        at += bytes.len();
        pos += 32 + bytes.len();
    }
    let rest = &log[pos..];
    let mut c = Cur::new(name, rest, pos);
    if rest.iter().all(|&b| b == 0) {
        seen.shape("sync log: zeros after the last entry");
        return out;
    }
    // This generation's head at the place an entry goes next, its bytes
    // not whole: one a crash cut short.
    let head_is_ours = rest.len() >= 32
        && u32::from_le_bytes(rest[..4].try_into().unwrap()) == ENTRY_MAGIC
        && u64::from_le_bytes(rest[8..16].try_into().unwrap()) == generation
        && u64::from_le_bytes(rest[16..24].try_into().unwrap()) == at as u64;
    let mut after = 0;
    if head_is_ours {
        seen.shape("sync log: an entry torn");
        let len = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
        after = (32 + len).min(rest.len());
    }
    // An earlier pass's bytes: the tail of an entry written over in part,
    // then that pass's whole entries one after another, then zeros.
    let tail = &rest[after..];
    if tail.iter().all(|&b| b == 0) {
        return out;
    }
    let found = (0..tail.len().min(32 + (64 << 10))).find_map(|skip| {
        let (g, a, bytes) = log_entry(log, pos + after + skip)?;
        (g != generation).then_some((skip, g, a, bytes.len()))
    });
    let Some((skip, g, mut a, mut len)) = found else {
        c.at = after;
        c.fail("bytes after the last entry that are neither zeros nor an earlier pass's")
    };
    let mut p = pos + after + skip;
    loop {
        p += 32 + len;
        a += len as u64;
        match log_entry(log, p) {
            Some((g2, a2, bytes)) if g2 == g && a2 == a => len = bytes.len(),
            _ => break,
        }
    }
    if log[p..].iter().any(|&b| b != 0) {
        Cur::new(name, log, p).fail("bytes after an earlier pass's entries that are not zeros");
    }
    seen.shape("sync log: a past pass's bytes after the last entry");
    out
}

// ------------------------------------------------------------- the page

/// Base64 as the page's records are written (`btoa`): the standard
/// alphabet, padded.
pub fn unbase64(s: &str) -> Vec<u8> {
    let val = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("a byte of no base64: {c}"),
        }
    };
    let s = s.as_bytes();
    assert!(
        s.len().is_multiple_of(4),
        "base64 of {} characters",
        s.len()
    );
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for q in s.chunks(4) {
        let pad = q.iter().filter(|&&c| c == b'=').count();
        let n = q[..4 - pad].iter().fold(0u32, |n, &c| (n << 6) | val(c)) << (6 * pad);
        out.extend_from_slice(&n.to_be_bytes()[1..4 - pad]);
    }
    out
}

/// The AES S-box (FIPS-197, figure 7).
const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

/// AES-256's round keys (FIPS-197, 5.2).
fn aes256_keys(key: &[u8; 32]) -> [[u8; 16]; 15] {
    let mut w = [[0u8; 4]; 60];
    for (i, word) in w.iter_mut().take(8).enumerate() {
        word.copy_from_slice(&key[4 * i..4 * i + 4]);
    }
    let mut rcon = 1u8;
    for i in 8..60 {
        let mut t = w[i - 1];
        if i % 8 == 0 {
            t = [
                SBOX[t[1] as usize] ^ rcon,
                SBOX[t[2] as usize],
                SBOX[t[3] as usize],
                SBOX[t[0] as usize],
            ];
            rcon = (rcon << 1) ^ if rcon & 0x80 != 0 { 0x1b } else { 0 };
        } else if i % 8 == 4 {
            t = t.map(|b| SBOX[b as usize]);
        }
        let before = w[i - 8];
        w[i] = std::array::from_fn(|j| before[j] ^ t[j]);
    }
    let mut keys = [[0u8; 16]; 15];
    for (r, k) in keys.iter_mut().enumerate() {
        for j in 0..4 {
            k[4 * j..4 * j + 4].copy_from_slice(&w[4 * r + j]);
        }
    }
    keys
}

/// One block through AES (FIPS-197, 5.1), the state column by column.
fn aes_block(keys: &[[u8; 16]; 15], block: &[u8; 16]) -> [u8; 16] {
    let xtime = |b: u8| (b << 1) ^ if b & 0x80 != 0 { 0x1b } else { 0 };
    let mut s = *block;
    for (i, b) in s.iter_mut().enumerate() {
        *b ^= keys[0][i];
    }
    for (round, key) in keys.iter().enumerate().skip(1) {
        let mut t = [0u8; 16];
        // SubBytes and ShiftRows: row r of column c from column c + r.
        for c in 0..4 {
            for r in 0..4 {
                t[4 * c + r] = SBOX[s[4 * ((c + r) % 4) + r] as usize];
            }
        }
        if round < 14 {
            for c in 0..4 {
                let a = [t[4 * c], t[4 * c + 1], t[4 * c + 2], t[4 * c + 3]];
                let all = a[0] ^ a[1] ^ a[2] ^ a[3];
                for r in 0..4 {
                    t[4 * c + r] = a[r] ^ all ^ xtime(a[r] ^ a[(r + 1) % 4]);
                }
            }
        }
        for (i, b) in t.iter_mut().enumerate() {
            *b ^= key[i];
        }
        s = t;
    }
    s
}

/// A record the page sealed with its `cryptoKey` (AES-256-GCM, a 12-byte
/// IV), its ciphertext turned back by GCM's counter: block `i` of the
/// keystream is the IV and the count `i + 2`. The tag, its last 16 bytes,
/// is the page's to check, and HEAD's restore checks it
/// (`web/fennec.compat.test.js`): a wrong key or counter here reads as no
/// record, which the walk refuses.
pub fn aes_gcm_plain(key: &[u8; 32], iv: &[u8], sealed: &[u8]) -> Vec<u8> {
    assert_eq!(iv.len(), 12, "an IV of {} bytes", iv.len());
    assert!(sealed.len() >= 16, "a sealed record shorter than its tag");
    let keys = aes256_keys(key);
    let data = &sealed[..sealed.len() - 16];
    let mut out = Vec::with_capacity(data.len());
    for (i, chunk) in data.chunks(16).enumerate() {
        let mut ctr = [0u8; 16];
        ctr[..12].copy_from_slice(iv);
        ctr[12..].copy_from_slice(&(i as u32 + 2).to_be_bytes());
        let ks = aes_block(&keys, &ctr);
        out.extend(chunk.iter().zip(ks).map(|(a, b)| a ^ b));
    }
    out
}

/// The key `tools/compat/web.mjs` seals a page's records with (`PAGE_KEY`):
/// the fixture's own.
pub fn page_key() -> [u8; 32] {
    std::array::from_fn(|i| ((i * 37 + 11) & 255) as u8)
}

/// A `page-idb*.json` file: the database, version and store the page
/// made, and its records -- the image under the page's name, then its
/// chunks in order -- each base64 bytes, or sealed (`gen` on the image,
/// `iv` and `data` on each). Every member read: the image walked as a
/// database, each chunk as the records it holds.
pub fn page_records(text: &str, file: &str, seen: &mut Seen) {
    use fennec_core::value::Value;
    let doc = fennec_core::json::parse_json(text).unwrap_or_else(|e| panic!("{file}: {e}"));
    let Value::Object(top) = doc else {
        panic!("{file}: not an object")
    };
    let members =
        |m: &[(String, Value)]| -> BTreeSet<String> { m.iter().map(|(k, _)| k.clone()).collect() };
    let set = |l: &[&str]| -> BTreeSet<String> { l.iter().map(|s| s.to_string()).collect() };
    assert_eq!(
        members(&top),
        set(&["database", "version", "store", "records"]),
        "{file}: its members"
    );
    let Some((_, Value::List(records))) = top.iter().find(|(k, _)| k == "records") else {
        panic!("{file}: records not a list")
    };
    let text_of = |v: &Value| match v {
        Value::Text(s) => s.clone(),
        v => panic!("{file}: {v:?} where a text is due"),
    };
    let mut name = None;
    for (i, r) in records.iter().enumerate() {
        let Value::Object(members_of) = r else {
            panic!("{file}: record {i} not an object")
        };
        let get = |k: &str| {
            members_of
                .iter()
                .find(|(m, _)| m == k)
                .map(|(_, v)| text_of(v))
        };
        let key = get("key").unwrap_or_else(|| panic!("{file}: record {i} has no key"));
        let names = members(members_of);
        let (bytes, sealed) = match get("bytes") {
            Some(b) => {
                assert_eq!(names, set(&["key", "bytes"]), "{file}: {key}'s members");
                (unbase64(&b), false)
            }
            None => {
                let want: &[&str] = match i {
                    0 => &["key", "gen", "iv", "data"],
                    _ => &["key", "iv", "data"],
                };
                assert_eq!(names, set(want), "{file}: {key}'s members");
                let iv = unbase64(&get("iv").unwrap());
                let data = unbase64(&get("data").unwrap());
                (aes_gcm_plain(&page_key(), &iv, &data), true)
            }
        };
        let label = format!("{file}: {key}");
        match i {
            0 => {
                name = Some(key.clone());
                database(&bytes, &label, seen);
                seen.shape(if sealed {
                    "page: sealed image"
                } else {
                    "page: image"
                });
            }
            _ => {
                let want = format!("{}#{i:09}", name.as_deref().unwrap());
                assert_eq!(key, want, "{file}: a chunk out of its place");
                chunk(&bytes, &label, seen);
                seen.shape(if sealed {
                    "page: sealed chunk"
                } else {
                    "page: chunk"
                });
            }
        }
    }
    assert!(records.len() > 1, "{file}: an image and no chunk");
}

/// A Durable Object's storage as `tools/compat/cloudflare.mjs` dumped what
/// a release's @fennecdb/cloudflare kept in it, every member read: a
/// SQLite-backed object's one table, `<key>:<gen>` -- the `CREATE TABLE`
/// the module ran, then its rows by id, the image's (`image` 1) before the
/// writes' (0) -- or a key-value backed object's values: the record
/// `<key>:meta` naming the generation and how many pieces and bytes its
/// image and its writes take, and those pieces, `<key>:<gen>:i:` and
/// `:l:` numbered from 0, none of another generation. Either way the
/// image's bytes walked as a database and the writes' as the records they
/// hold, each piece ending where it filled, inside a record or not.
pub fn durable_object(text: &str, file: &str, seen: &mut Seen) {
    use fennec_core::value::Value;
    let doc = fennec_core::json::parse_json(text).unwrap_or_else(|e| panic!("{file}: {e}"));
    let Value::Object(top) = doc else {
        panic!("{file}: not an object")
    };
    let get = |o: &[(String, Value)], k: &str| -> Value {
        o.iter()
            .find(|(m, _)| m == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("{file}: no `{k}`"))
    };
    // An object's members by name, sorted: the JSON reader keeps an
    // object's members in its own order, not the file's.
    let names = |o: &[(String, Value)]| -> Vec<String> {
        let mut v: Vec<String> = o.iter().map(|(k, _)| k.clone()).collect();
        v.sort();
        v
    };
    let text_of = |v: Value| match v {
        Value::Text(s) => s,
        v => panic!("{file}: {v:?} where a text is due"),
    };
    let int_of = |v: Value| match v {
        Value::Int(n) if n >= 0 => n as u64,
        v => panic!("{file}: {v:?} where a count is due"),
    };
    let list_of = |v: Value| match v {
        Value::List(l) => l,
        v => panic!("{file}: {v:?} where a list is due"),
    };
    let object_of = |v: Value| match v {
        Value::Object(o) => o,
        v => panic!("{file}: {v:?} where an object is due"),
    };
    let (mut image, mut writes) = (Vec::new(), Vec::new());
    let sql = match names(&top).as_slice() {
        [t] if t == "tables" => true,
        [v] if v == "values" => false,
        m => panic!("{file}: members {m:?}"),
    };
    if sql {
        let tables = list_of(get(&top, "tables"));
        assert_eq!(tables.len(), 1, "{file}: one generation's table kept");
        let table = object_of(tables[0].clone());
        assert_eq!(
            names(&table),
            ["name", "rows", "sql"],
            "{file}: a table's members"
        );
        let name = text_of(get(&table, "name"));
        let generation = name
            .strip_prefix("fennec:")
            .and_then(|g| g.parse::<u64>().ok())
            .filter(|&g| g > 0)
            .unwrap_or_else(|| panic!("{file}: a table named {name:?}"));
        assert_eq!(
            text_of(get(&table, "sql")),
            format!("CREATE TABLE \"fennec:{generation}\" (id INTEGER PRIMARY KEY, image INTEGER NOT NULL, data BLOB NOT NULL)"),
            "{file}: the table as the module makes it"
        );
        let (mut last, mut in_image) = (0, true);
        for r in list_of(get(&table, "rows")) {
            let r = object_of(r);
            assert_eq!(
                names(&r),
                ["data", "id", "image"],
                "{file}: a row's members"
            );
            let id = int_of(get(&r, "id"));
            assert!(id > last, "{file}: row {id} after {last}");
            last = id;
            let bytes = unbase64(&text_of(get(&r, "data")));
            assert!(!bytes.is_empty(), "{file}: row {id} empty");
            match int_of(get(&r, "image")) {
                1 if in_image => image.extend(bytes),
                0 => {
                    in_image = false;
                    writes.extend(bytes);
                }
                b => panic!("{file}: row {id}'s image is {b}, after the writes' rows"),
            }
        }
    } else {
        let values = list_of(get(&top, "values"));
        let mut record = None;
        let mut pieces: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for v in values {
            let v = object_of(v);
            let key = text_of(get(&v, "key"));
            match names(&v).as_slice() {
                [b, k] if k == "key" && b == "bytes" => {
                    pieces.insert(key, unbase64(&text_of(get(&v, "bytes"))));
                }
                [k, r] if k == "key" && r == "value" && key == "fennec:meta" => {
                    record = Some(object_of(get(&v, "value")));
                }
                m => panic!("{file}: {key}'s members {m:?}"),
            }
        }
        let record = record.unwrap_or_else(|| panic!("{file}: no fennec:meta"));
        assert_eq!(
            names(&record),
            ["gen", "image", "log", "v"],
            "{file}: the record's members"
        );
        assert_eq!(int_of(get(&record, "v")), 1, "{file}: the record's version");
        let generation = int_of(get(&record, "gen"));
        for (part, into, what) in [("i", &mut image, "image"), ("l", &mut writes, "log")] {
            let counts: Vec<u64> = list_of(get(&record, what))
                .into_iter()
                .map(int_of)
                .collect();
            let [count, bytes] = counts[..] else {
                panic!("{file}: the record's {what}: {counts:?}")
            };
            for n in 0..count {
                let key = format!("fennec:{generation}:{part}:{n:09}");
                into.extend(
                    pieces
                        .remove(&key)
                        .unwrap_or_else(|| panic!("{file}: no {key}")),
                );
            }
            assert_eq!(
                into.len() as u64,
                bytes,
                "{file}: the {what}'s pieces' bytes"
            );
        }
        assert!(
            pieces.is_empty(),
            "{file}: pieces no record names: {:?}",
            pieces.keys()
        );
    }
    let kind = if sql { "sql" } else { "values" };
    assert!(
        !image.is_empty() && !writes.is_empty(),
        "{file}: an image and writes after it"
    );
    database(&image, &format!("{file}: the image"), seen);
    seen.shape(match sql {
        true => "durable object: sql image",
        false => "durable object: values image",
    });
    chunk(&writes, &format!("{file}: the writes"), seen);
    seen.shape(match sql {
        true => "durable object: sql writes",
        false => "durable object: values writes",
    });
    eprintln!(
        "{file}: {kind}, an image of {} bytes and {} bytes of writes",
        image.len(),
        writes.len()
    );
}

/// SHA-256 (FIPS 180-4), the walk's own, as its AES is: what a set's
/// `SHA256SUMS` names each file by and its line in `RELEASES` pins the
/// sums by, read here with no crate.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes(word.try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 32];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The walk's own AES held to FIPS-197's AES-256 example (appendix C.3),
/// its counter to GCM's test case 15 (McGrew and Viega), and its SHA-256 to
/// FIPS 180-4's: a page record it misreads, or a set's sums, is caught here
/// first.
pub fn aes_holds_its_vectors() {
    let hex = |s: &str| -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    };
    let key: [u8; 32] = std::array::from_fn(|i| i as u8);
    let block: [u8; 16] = hex("00112233445566778899aabbccddeeff").try_into().unwrap();
    assert_eq!(
        aes_block(&aes256_keys(&key), &block).to_vec(),
        hex("8ea2b7ca516745bfeafc49904b496089"),
        "FIPS-197 C.3"
    );
    let key: [u8; 32] = hex("feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308")
        .try_into()
        .unwrap();
    let iv = hex("cafebabefacedbaddecaf888");
    let plain = hex(
        "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72\
         1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b391aafd255",
    );
    let mut sealed = hex(
        "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa\
         8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662898015ad",
    );
    sealed.extend_from_slice(&[0; 16]);
    assert_eq!(aes_gcm_plain(&key, &iv, &sealed), plain, "GCM test case 15");
    // FIPS 180-4's examples (NIST's SHA256.pdf), and a message of a whole
    // block plus one byte, whose padding takes a block of its own.
    for (msg, want) in [
        (
            &b""[..],
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            b"abc",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
    ] {
        assert_eq!(
            self::hex(&sha256(msg)),
            want,
            "SHA-256 of {} bytes",
            msg.len()
        );
    }
    assert_eq!(
        self::hex(&sha256(&[b'a'; 1_000_000])),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
        "SHA-256 of a million `a`s"
    );
}

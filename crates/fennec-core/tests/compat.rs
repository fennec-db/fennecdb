//! The fixture sets of `tests/compat/`, a directory a release: the files
//! that release's own binaries wrote -- its `fennec-server`, its `fennec`
//! shell, its web client and its engine (`tools/compat_fixtures.py`) --
//! and the answers it gave over them. 1.x opens every file written from
//! v0.2.0 on (docs/design/compatibility.md): this build holds every record
//! to its sum as `fennec verify` does, opens each file as a tool and as a
//! server would, answers every statement as the release did, writes values
//! of every type through every index into it, compacts, opens again and
//! answers alike; restores every graph the file keeps rather than building
//! it again; and every record kind, shape inside one, type tag and index
//! kind a release writes is in a file of its set. The sealed backup and
//! archive are fennec-http's `compat::`, `fennec verify` and `fennec
//! restore` fennec's, the server fennec-server's, the page's stores
//! `web/fennec.compat.test.js`'s.

mod answers;

use answers::{
    copied, differences, set_name, sets, torn, verified, written_and_compacted, Scratch,
};
use fennec_core::codec::{decode_str, get_uvarint};
use fennec_core::prelude::*;
use fennec_core::schema::{IndexKind, Quant, Schema};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::Ordering::Relaxed;

/// What each release's set must hold, by the release's own source: every
/// record kind (`REC_*`), statement record op (`engine/stored.rs`'s `OP_*`),
/// graph record version (`vector.rs`'s `GRAPH_VERSION_*` it writes), type
/// tag (`codec.rs`'s `TAG_*`) and index kind (`schema.rs`'s
/// `encode_index`) it can write, and the shapes a kind holds below it
/// ([`walk`] names each): every change a field record names, a frame's
/// ops, a history's forms, what a block holds, a spill landed and one left
/// dead, a close's mark, the damage a crash leaves, a record's claim, and
/// a graph's layout -- its metric, each arena its vectors and its
/// tombstones' are kept in, a bit index's centres, a node's flags, its
/// document ids' and links' widths, aliases and the copies table's halves.
/// A set without its line here fails, and so does a shape its line names
/// that no file holds: what a release writes and no file of its set holds
/// is named in the set's README.md with why, never left out silently.
struct Writes {
    set: &'static str,
    kinds: &'static [u8],
    ops: &'static [u8],
    graphs: &'static [u8],
    tags: &'static [u8],
    indexes: &'static [u8],
    shapes: &'static [&'static str],
}

const RELEASES: &[Writes] = &[Writes {
    set: "v0.2.0",
    kinds: &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14],
    ops: &[1, 2, 3, 4, 5, 6],
    graphs: &[9, 10],
    tags: &[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
    ],
    indexes: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
    shapes: &[
        "claim: none",
        "claim: 1 byte",
        "frame: put",
        "frame: del",
        "next id: alone",
        "next id: with its frames' index",
        "history: forked once",
        "history: forked twice",
        "history: following",
        "block: across collections",
        "block: holds kind 1",
        "block: holds kind 2",
        "block: holds kind 3",
        "block: holds kind 5",
        "block: holds kind 12",
        "block: holds kind 13",
        "spill: landed",
        "spill: dead",
        "fields: 1 add",
        "fields: 2 drop",
        "fields: 3 rename",
        "fields: 4 ttl",
        "synced mark",
        "torn: kind 3 cut short",
        "torn: kind 14 failing its sum",
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
        "graph: upper layers",
        "graph: aliases",
        "graph: halves",
    ],
}];

/// The set's database files: `<name>.fennec` beside `<name>.answers`.
fn files(set: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(set)
        .unwrap()
        .filter_map(|e| {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            let stem = name.strip_suffix(".answers")?.to_string();
            set.join(format!("{stem}.fennec")).is_file().then_some(stem)
        })
        .collect();
    v.sort();
    v
}

#[test]
fn every_file_of_every_release_opens_and_answers_as_its_release_did() {
    fennec_ql::install();
    let mut found = Vec::new();
    for set in sets() {
        let scratch = Scratch::new(&format!("files-{}", set_name(&set)));
        for name in files(&set) {
            // The sync log is a Unix file system's (`synclog.rs`): no
            // Windows build reads one, and the file cut back to its last
            // fsync answers there with what it holds alone.
            if cfg!(windows) && set.join(format!("{name}.fennec.sync")).exists() {
                eprintln!("{name}: passed over, no sync log on Windows");
                continue;
            }
            let dir = scratch.0.join(&name);
            std::fs::create_dir_all(&dir).unwrap();
            let path = copied(&set, &name, &dir);
            verified(&path, &name, torn(&name));
            // As a tool looks at it, writing nothing; then as a server or
            // the shell opens it to write.
            let looked = fennec_core::fs::open_read_only(&path)
                .unwrap_or_else(|e| panic!("{name}: a read-only open: {e}"));
            found.extend(differences(&looked, &set, &name, "read-only"));
            drop(looked);
            found.extend(written_and_compacted(&path, &set, &name));
        }
    }
    assert!(
        found.is_empty(),
        "{} answers differ:\n{}",
        found.len(),
        found.join("\n")
    );
}

// ------------------------------------------------------------- coverage

/// A record as the file holds it: its kind (under the checked bit and the
/// claim's width), its collection and its body, where the body starts,
/// past its claim and sum; the claim's width and value where it has one; and
/// the damage it shows where it is the torn end of a file.
struct Rec<'a> {
    kind: u8,
    cid: u32,
    body: &'a [u8],
    body_at: usize,
    claim: Option<(usize, u64)>,
    torn: Option<&'static str>,
}

/// The records of `bytes` from `at`, by their heads
/// (`[kind][collection][length][body]`, docs/design/storage.md): the
/// counter's fixed-width head first where `image` says one may be. An
/// outer record's sum is held to its bytes (CRC32C of its head, body and
/// claim); one cut short or failing its sum ends the walk, named as torn.
fn records(bytes: &[u8], mut at: usize, image: bool) -> Vec<Rec<'_>> {
    let mut out = Vec::new();
    let mut first = image;
    while at < bytes.len() {
        let start = at;
        let k = bytes[at];
        if first && k == 6 {
            out.push(Rec {
                kind: 6,
                cid: 0,
                body: &bytes[at + 1..at + 17],
                body_at: at + 1,
                claim: None,
                torn: None,
            });
            at += 17;
            first = false;
            continue;
        }
        first = false;
        at += 1;
        let head =
            get_uvarint(bytes, &mut at).and_then(|cid| Ok((cid, get_uvarint(bytes, &mut at)?)));
        let Ok((cid, n)) = head else {
            out.push(cut_short(bytes, start, k));
            break;
        };
        let n = n as usize;
        if at + n > bytes.len() {
            out.push(cut_short(bytes, start, k));
            break;
        }
        let mut body = &bytes[at..at + n];
        let mut claim = None;
        let mut torn = None;
        if k & 0x80 != 0 {
            let width = ((k & 0x60) >> 5) as usize + 1;
            let sum_at = at + n - 4;
            let want = u32::from_le_bytes(bytes[sum_at..sum_at + 4].try_into().unwrap());
            if fennec_core::crc::crc32c(0, &bytes[start..sum_at]) != want {
                torn = Some("failing its sum");
            }
            let mut c = [0u8; 8];
            c[..width].copy_from_slice(&bytes[sum_at - width..sum_at]);
            claim = Some((width, u64::from_le_bytes(c)));
            body = &body[..n - width - 4];
        }
        out.push(Rec {
            kind: k & 0x1f,
            cid: cid as u32,
            body,
            body_at: at,
            claim,
            torn,
        });
        at += n;
        if torn.is_some() {
            break;
        }
    }
    out
}

fn cut_short(bytes: &[u8], at: usize, k: u8) -> Rec<'_> {
    Rec {
        kind: k & 0x1f,
        cid: 0,
        body: &bytes[at..],
        body_at: at,
        claim: None,
        torn: Some("cut short"),
    }
}

/// A graph whose record a file keeps -- its collection's last -- and what
/// a later record did to its collection: what an open restores.
struct Kept {
    name: String,
    field: String,
    version: u8,
    body: Vec<u8>,
    /// A write to the collection after the record: the graph restored
    /// takes it, so it is no longer the record's bytes.
    touched: bool,
    /// A node waiting to be linked: an open links it.
    waiting: bool,
}

#[derive(Default)]
struct Seen {
    kinds: BTreeSet<u8>,
    ops: BTreeSet<u8>,
    graphs: BTreeSet<u8>,
    tags: BTreeSet<u8>,
    indexes: BTreeSet<u8>,
    shapes: BTreeSet<String>,
    /// The collections by id, as the file's creates name them.
    names: BTreeMap<u32, String>,
    kept: BTreeMap<(u32, String), Kept>,
    /// Each spill by where its body is and its length: landed or not.
    spills: BTreeMap<(usize, usize), bool>,
}

impl Seen {
    fn shape(&mut self, s: impl Into<String>) {
        self.shapes.insert(s.into());
    }

    fn touch(&mut self, cid: u32) {
        for ((c, _), k) in self.kept.iter_mut() {
            if *c == cid {
                k.touched = true;
            }
        }
    }
}

/// A value's tag and the tags inside it, by the codec's layout (`codec.rs`).
fn value(b: &[u8], at: &mut usize, tags: &mut BTreeSet<u8>) {
    let tag = b[*at];
    *at += 1;
    tags.insert(tag);
    let uv = |at: &mut usize| get_uvarint(b, at).unwrap() as usize;
    match tag {
        0 => {}
        1 => *at += 1,
        2 | 9 => {
            uv(at);
        }
        3 => *at += 8,
        4 | 5 => *at += uv(at),
        6 => *at += 4 * uv(at),
        8 => *at += 2 * uv(at),
        7 => {
            for _ in 0..uv(at) {
                value(b, at, tags);
            }
        }
        10 => {
            uv(at);
            let n = uv(at);
            for _ in 0..n {
                uv(at);
            }
            *at += 4 * n;
        }
        13 => {
            for _ in 0..uv(at) {
                *at += uv(at);
                value(b, at, tags);
            }
        }
        15 => *at += 16,
        t => panic!("a value tag no release writes: {t}"),
    }
}

fn index_kind(k: &IndexKind, seen: &mut Seen) {
    let code = match k {
        IndexKind::None => 0,
        IndexKind::Hash { unique: false } => 1,
        IndexKind::Hash { unique: true } => 8,
        IndexKind::Vector(spec) => match spec.quant {
            Quant::None => 2,
            _ => 5,
        },
        IndexKind::Text(spec) if spec.accents => 11,
        IndexKind::Text(spec) if spec.chars => 7,
        IndexKind::Text(_) => 3,
        IndexKind::Sorted { ttl: None } => 4,
        IndexKind::Sorted { ttl: Some(_) } => 9,
        IndexKind::Inverted => 6,
        IndexKind::Geo => 10,
    };
    seen.indexes.insert(code);
}

/// A schema's type tags (`Schema::encode`): each field's type, and those
/// a schema writes beside a field or past its last.
fn schema(s: &Schema, seen: &mut Seen) {
    fn ty(t: &DataType, tags: &mut BTreeSet<u8>) {
        let tag = match t {
            DataType::Bool => 1,
            DataType::Int => 2,
            DataType::Float => 3,
            DataType::Text => 4,
            DataType::Bytes => 5,
            DataType::Vector(_, VecPrec::F32) => 6,
            DataType::Vector(_, VecPrec::F16) => 8,
            DataType::List(inner) => {
                ty(inner, tags);
                7
            }
            DataType::Timestamp => 9,
            DataType::Sparse(_) => 10,
            DataType::Json => 14,
            DataType::Geo => 15,
        };
        tags.insert(tag);
    }
    for f in s.fields.iter().chain(&s.paths) {
        ty(&f.ty, &mut seen.tags);
        index_kind(&f.index, seen);
        if f.collate.is_some() {
            seen.tags.insert(11);
        }
        if f.nocase {
            seen.tags.insert(17);
        }
        if let Some(spec) = f.beside {
            seen.tags.insert(18);
            index_kind(&IndexKind::Text(spec), seen);
        }
    }
    if !s.dropped.is_empty() {
        seen.tags.insert(12);
    }
    if !s.composites.is_empty() {
        seen.tags.insert(16);
    }
}

/// A flat graph record's body past its field's name (`vector.rs`'s
/// `serialize_into`, versions 9 and 10), every byte of it accounted for:
/// its head -- dimension, metric, beams, precision, quantization and a bit
/// index's centres -- the nodes' documents, flags, levels and level-0
/// lengths, every list, the tombstones' vectors, the aliases and the
/// copies table's halves. Its shapes into `seen`; whether a node waits.
fn graph(b: &[u8], seen: &mut Seen) -> bool {
    let mut at = 0;
    let uv = |at: &mut usize| get_uvarint(b, at).unwrap() as usize;
    let version = b[at];
    at += 1;
    assert!(
        matches!(version, 9 | 10),
        "a flat graph record of version {version}"
    );
    let dim = uv(&mut at);
    seen.shape(match b[at] {
        0 => "graph: cosine",
        1 => "graph: l2",
        2 => "graph: dot",
        m => panic!("a metric no release writes: {m}"),
    });
    at += 1;
    for _ in 0..3 {
        uv(&mut at);
    }
    let half = match b[at] {
        0 => false,
        1 => true,
        p => panic!("a precision no release writes: {p}"),
    };
    at += 1;
    let quant = b[at];
    at += 1;
    let mut centres = 0;
    if quant == 3 {
        centres = uv(&mut at);
        at += centres * dim * 4;
        if centres > 0 {
            seen.shape("graph: bit centres");
        } else {
            seen.shape("graph: bit index holding its vectors whole");
        }
    }
    let of_half = if half { " of an f16 field" } else { "" };
    let (arena, stored) = match (quant, centres > 0, half) {
        (1, ..) => (format!("int8{of_half}"), dim + 4),
        (3, true, _) => (format!("bit codes{of_half}"), dim.div_ceil(64) * 8 + 9),
        (0 | 3, false, false) => ("f32".to_string(), dim * 4),
        (0 | 3, false, true) => ("f16".to_string(), dim * 2),
        (q, ..) => panic!("a quantization no release writes: {q}"),
    };
    seen.shape(format!("graph: arena {arena}"));
    let n = uv(&mut at);
    uv(&mut at);
    if uv(&mut at) > 0 {
        seen.shape("graph: upper layers");
    }
    let (lw, dw) = (b[at] as usize, b[at + 1] as usize);
    at += 2;
    seen.shape(format!("graph: links of {lw} bytes"));
    seen.shape(format!("graph: doc ids of {dw} bytes"));
    at += n * dw;
    let flags = &b[at..at + n];
    at += n;
    let levels = &b[at..at + n];
    at += n;
    let l0: Vec<usize> = b[at..at + 2 * n]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c) as usize)
        .collect();
    at += 2 * n;
    let links = |at: &mut usize, k: usize| {
        if k > 0 {
            *at += lw;
            if k > 1 {
                let w = b[*at] as usize;
                *at += 1 + (k - 1) * w;
            }
        }
    };
    for &k in &l0 {
        links(&mut at, k);
    }
    for &level in levels {
        for _ in 0..level {
            let k = u16::from_le_bytes([b[at], b[at + 1]]) as usize;
            at += 2;
            links(&mut at, k);
        }
    }
    let dead = flags.iter().filter(|&&f| f == 1).count();
    if dead > 0 {
        let arena = arena.trim_end_matches(of_half);
        seen.shape(format!("graph: tombstone in arena {arena}"));
    }
    at += dead * stored;
    let waiting = flags.contains(&2);
    if waiting {
        seen.shape("graph: node waiting");
    }
    assert!(
        flags.iter().all(|&f| f <= 2),
        "a node flag no release writes"
    );
    let aliases = u64::from_le_bytes(b[at..at + 8].try_into().unwrap()) as usize;
    at += 8 + aliases * (lw + 8);
    if aliases > 0 {
        seen.shape("graph: aliases");
    }
    match &b[at..] {
        [] => {}
        [1, halves @ ..] if halves.len() == 4 * n => {
            seen.shape("graph: halves");
            at = b.len();
        }
        rest => panic!("{} bytes after a graph's aliases", rest.len()),
    }
    assert_eq!(at, b.len(), "a graph record read short of its end");
    waiting
}

/// The inner records of a block, a spill or a land: data and schema
/// changes, plain.
fn inner(recs: Vec<Rec<'_>>, seen: &mut Seen, holder: &str) {
    let mut cids = BTreeSet::new();
    for r in &recs {
        seen.shape(format!("{holder}: holds kind {}", r.kind));
        if r.kind == 3 {
            cids.insert(r.cid);
        }
    }
    if cids.len() > 1 {
        seen.shape(format!("{holder}: across collections"));
    }
    walk(recs, seen, false);
}

fn walk(recs: Vec<Rec<'_>>, seen: &mut Seen, outer: bool) {
    for r in recs {
        seen.kinds.insert(r.kind);
        if let Some(why) = r.torn {
            seen.shape(format!("torn: kind {} {why}", r.kind));
            continue;
        }
        match r.claim {
            Some((_, 0)) => seen.shape("claim: none"),
            Some((w, _)) => seen.shape(format!("claim: {w} byte{}", if w > 1 { "s" } else { "" })),
            None => {}
        }
        match r.kind {
            1 | 5 => {
                let s = Schema::decode(r.body, &mut 0).unwrap();
                if r.kind == 1 {
                    seen.kept.retain(|(c, _), _| *c != r.cid);
                    seen.names.insert(r.cid, s.name.clone());
                } else {
                    seen.touch(r.cid);
                }
                schema(&s, seen);
            }
            2 => {
                seen.kept.retain(|(c, _), _| *c != r.cid);
                seen.names.remove(&r.cid);
            }
            12 => {
                let change = r.body[0];
                seen.shape(match change {
                    1 => "fields: 1 add",
                    2 => "fields: 2 drop",
                    3 => "fields: 3 rename",
                    4 => "fields: 4 ttl",
                    c => panic!("a field change no release writes: {c}"),
                });
                let mut at = 1;
                decode_str(r.body, &mut at).unwrap();
                decode_str(r.body, &mut at).unwrap();
                schema(&Schema::decode(r.body, &mut at).unwrap(), seen);
                seen.touch(r.cid);
            }
            3 => {
                // Frames: `[op][id][length][the document's values]`.
                let mut at = 0;
                while at < r.body.len() {
                    seen.shape(match r.body[at] {
                        1 => "frame: put",
                        2 => "frame: del",
                        op => panic!("a frame op no release writes: {op}"),
                    });
                    at += 1;
                    get_uvarint(r.body, &mut at).unwrap();
                    let n = get_uvarint(r.body, &mut at).unwrap() as usize;
                    let end = at + n;
                    while at < end {
                        value(r.body, &mut at, &mut seen.tags);
                    }
                }
                seen.touch(r.cid);
            }
            4 => {
                let mut at = 0;
                let field = decode_str(r.body, &mut at).unwrap();
                let body = &r.body[at..];
                seen.graphs.insert(body[0]);
                let waiting = graph(body, seen);
                if let Some(name) = seen.names.get(&r.cid).cloned() {
                    seen.kept.insert(
                        (r.cid, field.clone()),
                        Kept {
                            name,
                            field,
                            version: body[0],
                            body: body.to_vec(),
                            touched: false,
                            waiting,
                        },
                    );
                }
            }
            7 => {
                let mut at = 0;
                get_uvarint(r.body, &mut at).unwrap();
                seen.shape(match at < r.body.len() {
                    true => "next id: with its frames' index",
                    false => "next id: alone",
                });
            }
            8 => {
                let mut at = 1;
                let forks = get_uvarint(r.body, &mut at).unwrap();
                if r.body[0] == 1 {
                    seen.shape("history: following");
                }
                match forks {
                    0 => {}
                    1 => seen.shape("history: forked once"),
                    _ => seen.shape("history: forked twice"),
                }
            }
            9 => inner(records(r.body, 0, false), seen, "block"),
            10 => {
                assert!(outer, "a spill inside a record");
                seen.spills
                    .entry((r.body_at, r.body.len()))
                    .or_insert(false);
            }
            11 => {
                let mut at = 0;
                let n = get_uvarint(r.body, &mut at).unwrap();
                for _ in 0..n {
                    let place = get_uvarint(r.body, &mut at).unwrap() as usize;
                    let len = get_uvarint(r.body, &mut at).unwrap() as usize;
                    *seen
                        .spills
                        .get_mut(&(place, len))
                        .expect("a land names a spill the file holds") = true;
                }
                inner(records(&r.body[at..], 0, false), seen, "land");
            }
            13 => {
                seen.ops.insert(r.body[0]);
            }
            14 => seen.shape("synced mark"),
            _ => {}
        }
    }
}

/// A file's bytes as an open reads them, the sync log's writes put back
/// where a build reads one (Unix); elsewhere the file as it lies.
fn read(set: &Path, file: &str, scratch: &Path) -> Vec<u8> {
    let copy = copied(set, file, scratch);
    let bytes = fennec_core::fs::read_with_sync_log(&copy)
        .unwrap()
        .unwrap_or_else(|| std::fs::read(&copy).unwrap());
    assert_eq!(&bytes[..8], b"FENECDB\x01", "{file}");
    bytes
}

fn seen_in(bytes: &[u8]) -> Seen {
    let mut seen = Seen::default();
    walk(records(bytes, 8, true), &mut seen, true);
    let spills: Vec<bool> = seen.spills.values().copied().collect();
    for landed in spills {
        seen.shape(match landed {
            true => "spill: landed",
            false => "spill: dead",
        });
    }
    seen
}

/// Every record kind, shape inside one, statement op, graph version, type
/// tag and index kind a release writes is in a file of its set: what makes
/// the answers above a check of each.
#[test]
fn every_kind_and_shape_a_release_writes_is_in_its_set() {
    for set in sets() {
        let name = set_name(&set);
        let writes = RELEASES.iter().find(|w| w.set == name).unwrap_or_else(|| {
            panic!("{name}: no line in RELEASES naming what its release writes")
        });
        let mut all = Seen::default();
        let mut table = Vec::new();
        for file in files(&set) {
            let scratch = Scratch::new(&format!("kinds-{name}-{file}"));
            let mine = seen_in(&read(&set, &file, &scratch.0));
            table.push(format!(
                "{file}: kinds {:?}, ops {:?}, graphs {:?}, tags {:?}, indexes {:?}\n    {:?}",
                mine.kinds, mine.ops, mine.graphs, mine.tags, mine.indexes, mine.shapes
            ));
            all.kinds.extend(&mine.kinds);
            all.ops.extend(&mine.ops);
            all.graphs.extend(&mine.graphs);
            all.tags.extend(&mine.tags);
            all.indexes.extend(&mine.indexes);
            all.shapes.extend(mine.shapes);
        }
        eprintln!("{name}:\n  {}", table.join("\n  "));
        for (what, want, got) in [
            ("record kinds", writes.kinds, &all.kinds),
            ("statement ops", writes.ops, &all.ops),
            ("graph versions", writes.graphs, &all.graphs),
            ("type tags", writes.tags, &all.tags),
            ("index kinds", writes.indexes, &all.indexes),
        ] {
            let missing: Vec<&u8> = want.iter().filter(|k| !got.contains(k)).collect();
            assert!(
                missing.is_empty(),
                "{name}: {what} {missing:?} are in no file"
            );
        }
        let missing: Vec<&&str> = writes
            .shapes
            .iter()
            .filter(|s| !all.shapes.contains(**s))
            .collect();
        assert!(
            missing.is_empty(),
            "{name}: shapes {missing:?} are in no file"
        );
    }
}

/// Every graph a release's file keeps -- the last record of its index --
/// comes back from that record when this build opens the file, read-only
/// as a tool opens it and to write as a server or the shell does, never
/// built again from the vectors: the graph is derived data an open may
/// rebuild where it cannot validate a record, and a rebuild answers a
/// small `near` alike, so only the index itself can tell. A record
/// restored says so (`Persisted`: where it was read, and its bytes a
/// node). One no write touched after it, and holding no node an open
/// links, serializes again to the record's own bytes, where this build
/// writes that version: an arena given back to the bit.
#[test]
fn every_graph_a_release_kept_is_restored_not_rebuilt() {
    fennec_ql::install();
    let mut restored = BTreeMap::<u8, usize>::new();
    let mut same = 0;
    for set in sets() {
        let name = set_name(&set);
        for file in files(&set) {
            if cfg!(windows) && set.join(format!("{file}.fennec.sync")).exists() {
                continue;
            }
            let scratch = Scratch::new(&format!("graphs-{name}-{file}"));
            let seen = seen_in(&read(&set, &file, &scratch.0));
            if seen.kept.is_empty() {
                continue;
            }
            for how in ["read-only", "open"] {
                let dir = scratch.0.join(how);
                std::fs::create_dir_all(&dir).unwrap();
                let path = copied(&set, &file, &dir);
                let db = match how {
                    "read-only" => fennec_core::fs::open_read_only(&path),
                    _ => fennec_core::fs::open(&path),
                }
                .unwrap_or_else(|e| panic!("{file}: {e}"));
                for k in seen.kept.values() {
                    let c = db
                        .collection(&k.name)
                        .unwrap_or_else(|e| panic!("{file}: {}: {e}", k.name));
                    let ix = c
                        .vectors
                        .get(&k.field)
                        .unwrap_or_else(|| panic!("{file}: {}.{} has no graph", k.name, k.field));
                    assert!(
                        ix.persisted().node_bytes.load(Relaxed) > 0,
                        "{name}/{file} ({how}): {}.{}'s graph (version {}) was built again, \
                         not restored from its record",
                        k.name,
                        k.field,
                        k.version
                    );
                    *restored.entry(k.version).or_default() += 1;
                    if k.touched || k.waiting {
                        continue;
                    }
                    let mut again = Vec::new();
                    ix.serialize_graph_into(k.version == 10, &mut again);
                    if again.first() != Some(&k.version) {
                        eprintln!(
                            "{file}: {}.{}: this build writes version {:?}, the record {}: not compared",
                            k.name,
                            k.field,
                            again.first(),
                            k.version
                        );
                        continue;
                    }
                    assert!(
                        again == k.body,
                        "{name}/{file} ({how}): {}.{}'s graph restored serializes to {} bytes \
                         other than its record's {}",
                        k.name,
                        k.field,
                        again.len(),
                        k.body.len()
                    );
                    same += 1;
                }
            }
        }
    }
    eprintln!("graphs restored, by version: {restored:?}; {same} the record's bytes again");
    for v in [9, 10] {
        assert!(
            restored.contains_key(&v),
            "no graph of version {v} restored"
        );
    }
    assert!(same > 0, "no restored graph compared with its record");
}

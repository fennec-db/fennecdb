//! The fixture sets of `tests/compat/`, a directory a release: the files
//! that release's own binaries wrote -- its `fennec-server`, its `fennec`
//! shell, its web client and its engine (`tools/compat_fixtures.py`) --
//! and the answers it gave over them. 1.x opens every file written from
//! v0.2.0 on (docs/design/compatibility.md): this build holds every record
//! to its sum as `fennec verify` does, opens each file as a tool and as a
//! server would, answers every statement as the release did, then writes,
//! compacts, opens again and answers alike; and every record kind, type
//! tag and index kind a release writes is in a file of its set. The sealed
//! backup and archive are fennec-http's `compat::`, `fennec verify` and
//! `fennec restore` fennec's, the page's stores `web/fennec.compat.test.js`'s.

mod answers;

use answers::{copied, differences, set_name, sets, written_and_compacted, Scratch};
use fennec_core::codec::{decode_str, get_uvarint};
use fennec_core::prelude::*;
use fennec_core::schema::{IndexKind, Quant, Schema};
use std::collections::BTreeSet;
use std::path::Path;

/// What each release's set must hold, by the release's own source: every
/// record kind (`REC_*`), statement record op (`engine/stored.rs`'s `OP_*`),
/// graph record version (`vector.rs`'s `GRAPH_VERSION_*` it writes), type
/// tag (`codec.rs`'s `TAG_*`) and index kind (`schema.rs`'s
/// `encode_index`) it can write. A set without its line here fails: a
/// release's set names what its release writes.
struct Writes {
    set: &'static str,
    kinds: &'static [u8],
    ops: &'static [u8],
    graphs: &'static [u8],
    tags: &'static [u8],
    indexes: &'static [u8],
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

/// `fennec verify` of a database file: every record held to its sum, the
/// sync log's writes put back first as an open would.
fn verify(path: &Path) -> u64 {
    let checked = match fennec_core::fs::read_with_sync_log(path).unwrap() {
        Some(bytes) => fennec_core::engine::check_records(&bytes),
        None => fennec_core::engine::check_reader(&mut std::fs::File::open(path).unwrap()),
    };
    checked.unwrap_or_else(|e| panic!("{}: {e}", path.display()))
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
            assert!(verify(&path) > 0, "{name}: no record");
            // As a tool looks at it, writing nothing; then as a server or
            // the shell opens it to write.
            let looked = fennec_core::fs::open_read_only(&path)
                .unwrap_or_else(|e| panic!("{name}: a read-only open: {e}"));
            found.extend(differences(&looked, &set, &name, "read-only"));
            drop(looked);
            found.extend(written_and_compacted(&path, &set, &name));
            assert!(verify(&path) > 0, "{name}: no record after the compact");
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

/// A record as the file holds it: its kind, under the checked bit and the
/// claim's width, and its body, past its claim and sum.
struct Rec<'a> {
    kind: u8,
    body: &'a [u8],
}

/// The records of `bytes` from `at`, by their heads
/// (`[kind][collection][length][body]`, docs/design/storage.md): the
/// counter's fixed-width head first where `image` says one may be.
fn records(bytes: &[u8], mut at: usize, image: bool) -> Vec<Rec<'_>> {
    let mut out = Vec::new();
    let mut first = image;
    while at < bytes.len() {
        let k = bytes[at];
        if first && k == 6 {
            out.push(Rec {
                kind: 6,
                body: &bytes[at + 1..at + 17],
            });
            at += 17;
            first = false;
            continue;
        }
        first = false;
        at += 1;
        get_uvarint(bytes, &mut at).unwrap();
        let n = get_uvarint(bytes, &mut at).unwrap() as usize;
        let mut body = &bytes[at..at + n];
        if k & 0x80 != 0 {
            let width = ((k & 0x60) >> 5) as usize + 1;
            body = &body[..n - width - 4];
        }
        out.push(Rec {
            kind: k & 0x1f,
            body,
        });
        at += n;
    }
    out
}

#[derive(Default)]
struct Seen {
    kinds: BTreeSet<u8>,
    ops: BTreeSet<u8>,
    graphs: BTreeSet<u8>,
    tags: BTreeSet<u8>,
    indexes: BTreeSet<u8>,
    metrics: BTreeSet<String>,
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
        IndexKind::Vector(spec) => {
            seen.metrics
                .insert(format!("{:?} {:?}", spec.metric, spec.quant));
            match spec.quant {
                Quant::None => 2,
                _ => 5,
            }
        }
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

fn walk(recs: Vec<Rec<'_>>, seen: &mut Seen) {
    for r in recs {
        seen.kinds.insert(r.kind);
        match r.kind {
            1 | 5 => schema(&Schema::decode(r.body, &mut 0).unwrap(), seen),
            12 => {
                let mut at = 1;
                decode_str(r.body, &mut at).unwrap();
                decode_str(r.body, &mut at).unwrap();
                schema(&Schema::decode(r.body, &mut at).unwrap(), seen);
            }
            3 => {
                // Frames: `[op][id][length][the document's values]`.
                let mut at = 0;
                while at < r.body.len() {
                    at += 1;
                    get_uvarint(r.body, &mut at).unwrap();
                    let n = get_uvarint(r.body, &mut at).unwrap() as usize;
                    let end = at + n;
                    while at < end {
                        value(r.body, &mut at, &mut seen.tags);
                    }
                }
            }
            4 => {
                let mut at = 0;
                decode_str(r.body, &mut at).unwrap();
                seen.graphs.insert(r.body[at]);
            }
            9 | 10 => walk(records(r.body, 0, false), seen),
            11 => {
                let mut at = 0;
                for _ in 0..get_uvarint(r.body, &mut at).unwrap() * 2 {
                    get_uvarint(r.body, &mut at).unwrap();
                }
                walk(records(r.body, at, false), seen);
            }
            13 => {
                seen.ops.insert(r.body[0]);
            }
            _ => {}
        }
    }
}

/// Every record kind, statement op, graph version, type tag and index
/// kind a release writes is in a file of its set: what makes the answers
/// above a check of each.
#[test]
fn every_kind_and_tag_a_release_writes_is_in_its_set() {
    for set in sets() {
        let name = set_name(&set);
        let writes = RELEASES.iter().find(|w| w.set == name).unwrap_or_else(|| {
            panic!("{name}: no line in RELEASES naming what its release writes")
        });
        let mut seen = Seen::default();
        let mut table = Vec::new();
        for file in files(&set) {
            // The sync log's writes put back as its file's open puts them,
            // where a build reads one (Unix); elsewhere the file as it lies.
            let scratch = Scratch::new(&format!("kinds-{name}-{file}"));
            let copy = copied(&set, &file, &scratch.0);
            let bytes = fennec_core::fs::read_with_sync_log(&copy)
                .unwrap()
                .unwrap_or_else(|| std::fs::read(&copy).unwrap());
            assert_eq!(&bytes[..8], b"FENECDB\x01", "{file}");
            let mut mine = Seen::default();
            walk(records(&bytes, 8, true), &mut mine);
            table.push(format!(
                "{file}: kinds {:?}, ops {:?}, graphs {:?}, tags {:?}, indexes {:?}",
                mine.kinds, mine.ops, mine.graphs, mine.tags, mine.indexes
            ));
            seen.kinds.extend(&mine.kinds);
            seen.ops.extend(&mine.ops);
            seen.graphs.extend(&mine.graphs);
            seen.tags.extend(&mine.tags);
            seen.indexes.extend(&mine.indexes);
            seen.metrics.extend(mine.metrics);
        }
        eprintln!(
            "{name}:\n  {}\n  metrics {:?}",
            table.join("\n  "),
            seen.metrics
        );
        for (what, want, got) in [
            ("record kinds", writes.kinds, &seen.kinds),
            ("statement ops", writes.ops, &seen.ops),
            ("graph versions", writes.graphs, &seen.graphs),
            ("type tags", writes.tags, &seen.tags),
            ("index kinds", writes.indexes, &seen.indexes),
        ] {
            let missing: Vec<&u8> = want.iter().filter(|k| !got.contains(k)).collect();
            assert!(
                missing.is_empty(),
                "{name}: {what} {missing:?} are in no file"
            );
        }
        for m in [
            "Cosine None",
            "L2 None",
            "Dot None",
            "Cosine Int8",
            "Cosine Bit",
        ] {
            assert!(seen.metrics.contains(m), "{name}: no graph of {m}");
        }
    }
}

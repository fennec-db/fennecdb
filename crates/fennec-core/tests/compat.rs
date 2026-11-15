//! The fixture sets of `tests/compat/`, a directory a release: the files
//! that release's own binaries wrote -- its `fennec-server`, its `fennec`
//! shell, its web client and its engine (`tools/compat_fixtures.py`) --
//! and the answers it gave over them. 1.x opens every file written from
//! v0.2.0 on (docs/design/compatibility.md): this build holds every record
//! to its sum as `fennec verify` does, opens each file as a tool and as a
//! server would, answers every statement as the release did, writes values
//! of every type through every index into it, opens it again, compacts and
//! answers alike; restores every graph the file keeps rather than building
//! it again; and the walk (`compat/walk.rs`) reads every byte of every
//! file by the release's own layout, holding the set to what the release
//! writes both ways. The sealed backup and archives are fennec-http's
//! `compat::`, `fennec verify` and `fennec restore` fennec's, the server
//! fennec-server's, the page's stores `web/fennec.compat.test.js`'s.

mod answers;
mod walk;

use answers::{
    copied, differences, set_name, sets, torn, verified, written_and_compacted, Scratch, SETS,
};
use fennec_core::schema::{IndexKind, Quant};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::Ordering::Relaxed;
use walk::{sealed_only, writes_of, FieldFacts, Seen, ALTERNATIVES, RELEASES};

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
    assert!(
        v.len() >= 4,
        "{}: {} database files",
        set.display(),
        v.len()
    );
    v
}

/// Each of `names` run by `each` on a thread of its own -- a file's work is
/// its own, in its own scratch directory, most of it a close's or a
/// compact's fsync, `F_FULLFSYNC` on macOS -- what each found gathered in
/// order. A thread's panic is named in its own words above, then here.
fn each_file<T: Send>(names: &[String], each: impl Fn(&str) -> T + Sync) -> Vec<T> {
    std::thread::scope(|s| {
        let threads: Vec<_> = names
            .iter()
            .map(|name| (name, s.spawn(|| each(name))))
            .collect();
        threads
            .into_iter()
            .map(|(name, t)| t.join().unwrap_or_else(|_| panic!("{name}: failed, above")))
            .collect()
    })
}

#[test]
fn every_file_of_every_release_opens_and_answers_as_its_release_did() {
    fennec_ql::install();
    let mut found = Vec::new();
    for set in sets() {
        let scratch = Scratch::new(&format!("files-{}", set_name(&set)));
        let per_file = each_file(&files(&set), |name| {
            // The sync log is a Unix file system's (`synclog.rs`): no
            // Windows build reads one, and the file cut back to its last
            // fsync answers there with what it holds alone.
            if cfg!(windows) && set.join(format!("{name}.fennec.sync")).exists() {
                eprintln!("{name}: passed over, no sync log on Windows");
                return Vec::new();
            }
            let dir = scratch.0.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let path = copied(&set, name, &dir);
            verified(&path, name, torn(name));
            // As a tool looks at it, writing nothing; then as a server or
            // the shell opens it to write.
            let looked = fennec_core::fs::open_read_only(&path)
                .unwrap_or_else(|e| panic!("{name}: a read-only open: {e}"));
            let mut found = differences(&looked, &set, name, "read-only");
            drop(looked);
            found.extend(written_and_compacted(&path, &set, name));
            found
        });
        found.extend(per_file.into_iter().flatten());
    }
    assert!(
        found.is_empty(),
        "{} answers differ:\n{}",
        found.len(),
        found.join("\n")
    );
}

// ------------------------------------------------------------- coverage

/// What a set holds beside its database files and page stores, none of it
/// in the file format: the statements and answers, the README, the sums,
/// the seal's key, the server's flags and answers over HTTP, the page's
/// answers. The sealed backup and the archives are fennec-http's walk's.
fn not_a_format_file(name: &str) -> bool {
    [".fennecql", ".answers", ".web", ".http.json", ".sealed"]
        .iter()
        .any(|s| name.ends_with(s))
        || matches!(name, "README.md" | "SHA256SUMS" | "key")
}

/// A file's bytes as an open reads them: the sync log beside it put back
/// by the walk's own reading of the log -- its header, entries and what
/// follows them -- and held to this build's own reading where a build
/// reads one (Unix, the log stamped as its copy's); the file as it lies
/// where there is no log.
fn read(set: &Path, file: &str, scratch: &Path, seen: &mut Seen) -> Vec<u8> {
    let bytes = std::fs::read(set.join(format!("{file}.fennec"))).unwrap();
    let log = set.join(format!("{file}.fennec.sync"));
    if !log.is_file() {
        return bytes;
    }
    let applied = walk::sync_log(
        &bytes,
        &std::fs::read(&log).unwrap(),
        &format!("{file}.fennec.sync"),
        seen,
    );
    #[cfg(unix)]
    {
        let copy = copied(set, file, scratch);
        let theirs = fennec_core::fs::read_with_sync_log(&copy)
            .unwrap()
            .unwrap_or_else(|| std::fs::read(&copy).unwrap());
        assert!(
            theirs == applied,
            "{file}: this build reads the log into {} bytes, the walk {}",
            theirs.len(),
            applied.len()
        );
    }
    #[cfg(not(unix))]
    let _ = scratch;
    applied
}

/// Every format file of `set` walked, each by its kind; what each held.
fn walked(set: &Path) -> Vec<(String, Seen)> {
    let name = set_name(set);
    let mut out = Vec::new();
    let mut entries: Vec<String> = std::fs::read_dir(set)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    for entry in entries {
        let path = set.join(&entry);
        if path.is_dir() {
            assert!(
                entry.starts_with("archive"),
                "{name}/{entry}: a directory no walk reads"
            );
            continue;
        }
        let mut seen = Seen::default();
        if let Some(stem) = entry.strip_suffix(".fennec") {
            let scratch = Scratch::new(&format!("walk-{name}-{stem}"));
            let bytes = read(set, stem, &scratch.0, &mut seen);
            walk::database(&bytes, &format!("{name}/{entry}"), &mut seen);
            seen.close_spills();
        } else if entry.ends_with(".fennec.sync") {
            // Read with its file, above.
            continue;
        } else if entry.starts_with("page-idb") && entry.ends_with(".json") {
            let text = std::fs::read_to_string(&path).unwrap();
            walk::page_records(&text, &format!("{name}/{entry}"), &mut seen);
        } else if not_a_format_file(&entry) {
            continue;
        } else {
            panic!("{name}/{entry}: a file no walk reads");
        }
        out.push((entry, seen));
    }
    out
}

/// `want`, a line's list, and `got`, what the walk met: each in the other.
fn both_ways<T: Ord + std::fmt::Debug + Copy>(
    set: &str,
    what: &str,
    want: &[T],
    got: &BTreeSet<T>,
) {
    let want: BTreeSet<T> = want.iter().copied().collect();
    let missing: Vec<&T> = want.difference(got).collect();
    let extra: Vec<&T> = got.difference(&want).collect();
    assert!(
        missing.is_empty(),
        "{set}: {what} {missing:?} the release writes are in no file"
    );
    assert!(
        extra.is_empty(),
        "{set}: {what} {extra:?} are in a file and not in its RELEASES line"
    );
}

/// Every record kind, statement op, graph version, schema tag, value tag,
/// index kind and alternative inside them a release writes is in a file of
/// its set, and no file holds one its line does not name: what makes the
/// answers above a check of each. Every alternative the walk tells apart
/// is placed by the newest release's line: in a file, or absent with why,
/// which the set's README names.
#[test]
fn every_kind_and_shape_a_release_writes_is_in_its_set() {
    let newest = RELEASES.last().unwrap().set;
    for set in sets() {
        let name = set_name(&set);
        let writes = writes_of(&name);
        let mut all = Seen::default();
        let mut table = Vec::new();
        for (file, seen) in walked(&set) {
            table.push(format!("{file}: {}", seen.summary()));
            all.add(&seen);
        }
        eprintln!("{name}:\n  {}", table.join("\n  "));
        both_ways(&name, "record kinds", writes.kinds, &all.kinds);
        both_ways(&name, "statement ops", writes.ops, &all.ops);
        both_ways(&name, "graph versions", writes.graphs, &all.graphs);
        both_ways(&name, "schema tags", writes.schema_tags, &all.schema_tags);
        both_ways(&name, "value tags", writes.value_tags, &all.value_tags);
        both_ways(&name, "index kinds", writes.indexes, &all.indexes);
        // The sealed files' alternatives are fennec-http's walk's.
        let shapes: Vec<&str> = writes
            .shapes
            .iter()
            .copied()
            .filter(|s| !sealed_only(s))
            .collect();
        both_ways(&name, "shapes", &shapes, &all.shapes);

        let absent: BTreeSet<&str> = writes.absent.iter().map(|(s, _)| *s).collect();
        let placed: BTreeSet<&str> = writes.shapes.iter().copied().collect();
        assert!(
            absent.is_disjoint(&placed),
            "{name}: {:?} both in a file and absent",
            absent.intersection(&placed).collect::<Vec<_>>()
        );
        let every: BTreeSet<&str> = ALTERNATIVES.iter().copied().collect();
        let named: BTreeSet<&str> = absent.union(&placed).copied().collect();
        assert!(
            named.is_subset(&every),
            "{name}: {:?} are no alternative the walk knows",
            named.difference(&every).collect::<Vec<_>>()
        );
        if name == newest {
            assert!(
                named == every,
                "{name}: alternatives {:?} are neither in a file nor absent with why",
                every.difference(&named).collect::<Vec<_>>()
            );
        }
        let readme = std::fs::read_to_string(set.join("README.md")).unwrap();
        for (shape, why) in writes.absent {
            assert!(!why.is_empty(), "{name}: `{shape}` absent with no why");
            assert!(
                readme.contains(&format!("`{shape}`")),
                "{name}/README.md: `{shape}`, absent from every file, is not named there"
            );
        }
        eprintln!(
            "{name}: {} alternatives, {} in a file, {} absent with why; {} kinds, {} ops, {} graph \
             versions, {} schema tags, {} value tags, {} index kinds",
            every.len(),
            placed.len(),
            absent.len(),
            writes.kinds.len(),
            writes.ops.len(),
            writes.graphs.len(),
            writes.schema_tags.len(),
            writes.value_tags.len(),
            writes.indexes.len()
        );
    }
}

/// Every line of `RELEASES` has its set in the tree, and every set in the
/// tree its line: a set deleted, or never written for a release that
/// names one, fails here whichever sets `FENNEC_COMPAT_SETS` points the
/// other tests at.
#[test]
fn every_release_named_has_its_set_in_the_tree() {
    let tree: BTreeSet<String> = std::fs::read_dir(SETS)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .map(|p| set_name(&p))
        .collect();
    let named: BTreeSet<String> = RELEASES.iter().map(|w| w.set.to_string()).collect();
    assert!(
        named.contains("v0.2.0"),
        "RELEASES lost v0.2.0's line: 1.x opens every file written from v0.2.0 on"
    );
    assert_eq!(
        tree, named,
        "the sets in crates/fennec-core/tests/compat/ and RELEASES' lines"
    );
    let mut order: Vec<&str> = RELEASES.iter().map(|w| w.set).collect();
    order.sort_by_key(|s| answers::version_of(s));
    assert_eq!(
        order,
        RELEASES.iter().map(|w| w.set).collect::<Vec<_>>(),
        "RELEASES oldest first"
    );
}

/// Each database file of each set, walked: its bytes as an open reads
/// them, and what the walk met.
fn each_database(mut f: impl FnMut(&str, &str, &Path, &[u8], &Seen)) {
    for set in sets() {
        let name = set_name(&set);
        for file in files(&set) {
            let scratch = Scratch::new(&format!("each-{name}-{file}"));
            let mut seen = Seen::default();
            let bytes = read(&set, &file, &scratch.0, &mut seen);
            walk::database(&bytes, &file, &mut seen);
            f(
                name.as_str(),
                file.as_str(),
                set.as_path(),
                &bytes[..],
                &seen,
            );
        }
    }
}

/// Every claim a release's record makes reads in this build as its bytes
/// say (`engine::checked_claim`, what an archive's reader and a follower
/// read one with): the point every byte before which was durable, two
/// bytes wide where a record is more than 1 MiB past the last fsync. Where
/// an open's own reading of a claim decides what it reads, `torn-wide`'s
/// answers hold it.
#[test]
fn every_claim_a_release_wrote_reads_as_its_bytes() {
    let (mut held, mut wide) = (0, 0);
    each_database(|set, file, _, bytes, seen| {
        for &(at, point) in &seen.claims {
            let ours = fennec_core::engine::checked_claim(&bytes[at..], at);
            assert_eq!(
                ours,
                Some((true, point)),
                "{set}/{file}: the record at {at} claims {point}"
            );
            held += 1;
            wide += usize::from(at - point > 255 * 4096);
        }
    });
    eprintln!("{held} claims read alike, {wide} of them two bytes wide or more");
    assert!(wide > 0, "no claim of two bytes");
}

/// `HEAD`'s index kind of `k`, as a schema writes it.
fn index_code(k: &IndexKind) -> u8 {
    match k {
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
    }
}

/// Every collection's schema a release's file holds reads in this build as
/// its bytes say: each field's `required` byte, collation, `nocase`, index
/// and the `@text` beside it, and each index over several fields, unique
/// or not, its parts descending or not. A byte read otherwise changes no
/// answer where no statement asks the rule it carries, and every later
/// write it would have held.
#[test]
fn every_schema_a_release_wrote_reads_as_its_bytes() {
    fennec_ql::install();
    let mut fields = 0;
    each_database(|set, file, dir, _, seen| {
        if cfg!(windows) && dir.join(format!("{file}.fennec.sync")).exists() {
            return;
        }
        let scratch = Scratch::new(&format!("schemas-{set}-{file}"));
        let path = copied(dir, file, &scratch.0);
        let db =
            fennec_core::fs::open_read_only(&path).unwrap_or_else(|e| panic!("{set}/{file}: {e}"));
        for facts in seen.schemas.values() {
            let c = db
                .collection(&facts.name)
                .unwrap_or_else(|e| panic!("{set}/{file}: {}: {e}", facts.name));
            let ours: Vec<FieldFacts> = c
                .schema
                .fields
                .iter()
                .map(|f| FieldFacts {
                    name: f.name.clone(),
                    required: f.required,
                    collate: f.collate.map(|c| c.code()),
                    nocase: f.nocase,
                    index: index_code(&f.index),
                    beside: f.beside.map(|s| (s.chars, s.accents)),
                })
                .collect();
            assert_eq!(ours, facts.fields, "{set}/{file}: {}'s fields", facts.name);
            let composites: Vec<(bool, Vec<(String, bool)>)> = c
                .schema
                .composites
                .iter()
                .map(|k| (k.unique, k.keys.clone()))
                .collect();
            assert_eq!(
                composites, facts.composites,
                "{set}/{file}: {}'s indexes over several fields",
                facts.name
            );
            fields += ours.len();
        }
    });
    assert!(fields > 0, "no schema compared");
}

#[test]
fn the_walk_s_aes_holds_its_vectors() {
    walk::aes_holds_its_vectors();
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
            let mut seen = Seen::default();
            let bytes = read(&set, &file, &scratch.0, &mut seen);
            walk::database(&bytes, &file, &mut seen);
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

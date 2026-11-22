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
    at_clock, copied, differences, id_differences, on_purpose, release_ids, set_name, sets, tag_of,
    torn, verified, written_and_compacted, Otherwise, Scratch, ON_PURPOSE, SETS,
};
use fennec_core::schema::{IndexKind, Metric, Quant};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::Ordering::Relaxed;
use walk::{sealed_only, writes_of, FieldFacts, Seen, ALTERNATIVES, RELEASES};

/// The set's database files: `<name>.fennec` beside `<name>.answers`, but
/// those this build reads otherwise on purpose ([`ON_PURPOSE`]), which a
/// test of their own holds.
fn files(set: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(set)
        .unwrap()
        .filter_map(|e| {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            let stem = name.strip_suffix(".answers")?.to_string();
            (set.join(format!("{stem}.fennec")).is_file() && !on_purpose(&stem)).then_some(stem)
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
        let per_file = each_file(&files(&set), |name| as_released(&set, name, &scratch.0));
        found.extend(per_file.into_iter().flatten());
    }
    assert!(
        found.is_empty(),
        "{} answers differ:\n{}",
        found.len(),
        found.join("\n")
    );
}

/// `name` of `set`, on a copy in `scratch`: verified, opened as a tool
/// looks at it and as a server or the shell opens it to write, answering
/// as its release did, written into and compacted ([`written_and_compacted`]):
/// what differs.
fn as_released(set: &Path, name: &str, scratch: &Path) -> Vec<String> {
    // The sync log is a Unix file system's (`synclog.rs`): no Windows build
    // reads one, and the file cut back to its last fsync answers there with
    // what it holds alone.
    if cfg!(windows) && set.join(format!("{name}.fennec.sync")).exists() {
        eprintln!("{name}: passed over, no sync log on Windows");
        return Vec::new();
    }
    let dir = scratch.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let path = copied(set, name, &dir);
    verified(&path, name, torn(name));
    let looked = fennec_core::fs::open_read_only(&path)
        .unwrap_or_else(|e| panic!("{name}: a read-only open: {e}"));
    let looked = at_clock(looked, set);
    let mut found = differences(&looked, set, name, "read-only");
    found.extend(id_differences(&looked, set, name, "read-only"));
    drop(looked);
    found.extend(written_and_compacted(&path, set, name));
    found
}

/// A further set of a tag (`<tag>+<n>`, n from 2) is read beside its first,
/// after it, as a set of the same release: made in this test's own
/// directory from the first set's files, every Rust suite's `sets` finds it
/// in its place, its database file opens and answers as the release did,
/// and what `ACCEPTED_REQUESTS` names for the tag holds for it -- a name no
/// further set takes is no set. fennec-server's suite serves such a set's
/// server file, and `web/fennec.compat.test.js` restores its page.
#[test]
fn a_further_set_of_a_tag_is_read_as_that_release_s() {
    fennec_ql::install();
    let first = sets().remove(0);
    let tag = tag_of(&set_name(&first)).to_string();
    let scratch = Scratch::new("further");
    let names = [tag.clone(), format!("{tag}+2"), format!("{tag}+10")];
    for name in &names {
        let dir = scratch.0.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["README.md", "SHA256SUMS"] {
            std::fs::copy(first.join(f), dir.join(f)).unwrap();
        }
    }
    let found: Vec<String> = answers::sets_in(&scratch.0)
        .iter()
        .map(|p| set_name(p))
        .collect();
    assert_eq!(found, names, "the sets in order, oldest first");
    for name in &names {
        assert_eq!(tag_of(name), tag, "{name}: a set of {tag}");
    }
    for name in ["+1", "+02", "+0", "+", "+x"].map(|n| format!("{tag}{n}")) {
        assert_eq!(
            answers::version_of(&name),
            (0, 0, 0, 0),
            "{name}: no set's name"
        );
    }
    for x in answers::ACCEPTED_REQUESTS.iter().filter(|x| x.tag == tag) {
        let further = format!("{tag}+2");
        assert!(
            answers::accepted_request(&further, x.method, x.path, x.key).is_some(),
            "{further}: {} {} is not held to its tag's accepted answer",
            x.method,
            x.path
        );
    }
    // Database files of the further set, each held to its release's
    // answers as a first set's is.
    let further = scratch.0.join(&names[1]);
    let held = ["kept", "shell", "spill", "waiting"];
    for (file, ext) in held
        .iter()
        .flat_map(|f| ["fennec", "fennecql", "answers", "ids"].map(|e| (f, e)))
    {
        std::fs::copy(
            first.join(format!("{file}.{ext}")),
            further.join(format!("{file}.{ext}")),
        )
        .unwrap();
    }
    let names = files(&further);
    assert_eq!(names, held, "the further set's database files");
    let opened = scratch.0.join("opened");
    let differ: Vec<String> = each_file(&names, |name| as_released(&further, name, &opened))
        .into_iter()
        .flatten()
        .collect();
    assert!(differ.is_empty(), "{}", differ.join("\n"));
}

// ------------------------------------------------------------- coverage

/// What a set holds beside its database files and page stores, none of it
/// in the file format: the statements and answers, the README, the sums,
/// the seal's key, the server's flags and answers over HTTP, the page's
/// answers. The sealed backup and the archives are fennec-http's walk's.
fn not_a_format_file(name: &str) -> bool {
    [
        ".fennecql",
        ".answers",
        ".web",
        ".http.json",
        ".sealed",
        ".ids",
        ".verify",
        ".restores.json",
        "node-archive.json",
    ]
    .iter()
    .any(|s| name.ends_with(s))
        || matches!(
            name,
            "README.md" | "SHA256SUMS" | "MANIFEST.json" | "key" | "clock"
        )
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
                entry.starts_with("archive") || entry == "node-archive",
                "{name}/{entry}: a directory no walk reads"
            );
            continue;
        }
        let mut seen = Seen::default();
        if let Some(stem) = entry.strip_suffix(".fennec") {
            let scratch = Scratch::new(&format!("walk-{name}-{stem}"));
            let bytes = read(set, stem, &scratch.0, &mut seen);
            walk::database(&bytes, &format!("{name}/{entry}"), &mut seen);
            seen.finish();
        } else if entry.ends_with(".fennec~") {
            // The copy a page's rewrite flushed beside its file before it
            // stopped: the image its next open takes.
            let bytes = std::fs::read(&path).unwrap();
            walk::database(&bytes, &format!("{name}/{entry}"), &mut seen);
            seen.finish();
            seen.shape("page: a rewrite's copy beside its file");
        } else if entry.ends_with(".fennec.sync") {
            // Read with its file, above.
            continue;
        } else if entry.starts_with("page-idb") && entry.ends_with(".json") {
            let text = std::fs::read_to_string(&path).unwrap();
            walk::page_records(&text, &format!("{name}/{entry}"), &mut seen);
            seen.finish();
        } else if entry.starts_with("do-") && entry.ends_with(".json") {
            let text = std::fs::read_to_string(&path).unwrap();
            walk::durable_object(&text, &format!("{name}/{entry}"), &mut seen);
            seen.finish();
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
/// is placed by the newest release's sets: in a file of one, or absent from
/// it with why, which its README names -- a further set of the same tag
/// holding what the first was found to miss.
#[test]
fn every_kind_and_shape_a_release_writes_is_in_its_set() {
    let newest = RELEASES.last().unwrap().tag;
    let (mut placed_newest, mut absent_newest) = (BTreeSet::new(), BTreeSet::new());
    for set in sets() {
        let name = set_name(&set);
        let writes = writes_of(&name);
        let mut all = Seen::default();
        let mut table = Vec::new();
        for (file, seen) in walked(&set) {
            // The field the release dropped there it compacted away: no
            // dropped place left in the schema, nor the drop's record.
            if file == "compacted.fennec" {
                for gone in ["field: dropped place", "fields: 2 drop"] {
                    assert!(
                        !seen.shapes.contains(gone),
                        "{name}/{file}: `{gone}`, where the release's compact took the dropped field out"
                    );
                }
            }
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
        if writes.tag == newest {
            placed_newest.extend(placed.iter().copied());
            absent_newest.extend(absent.iter().copied());
        }
        let readme = std::fs::read_to_string(set.join("README.md")).unwrap();
        // The count the README states, derived here from the code: the forms
        // its line places in a file and absent, of every one the walk tells
        // apart.
        let numbers = writes.kinds.len()
            + writes.ops.len()
            + writes.graphs.len()
            + writes.schema_tags.len()
            + writes.value_tags.len()
            + writes.indexes.len();
        let said = format!(
            "Forms: {} held in a file of this set, {} named absent below, of the {} the walk tells \
             apart; and {numbers} numbers -- record kinds, statement ops, graph versions, schema tags, \
             value tags and index kinds -- each in a file.",
            placed.len(),
            absent.len(),
            placed.len() + absent.len()
        );
        assert!(
            readme.replace('\n', " ").contains(&said),
            "{name}/README.md: no `{said}`, the count RELEASES' line makes"
        );
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
    // A form a later set of the tag holds was absent from an earlier one:
    // it is named either way.
    let named: BTreeSet<&str> = placed_newest.union(&absent_newest).copied().collect();
    let every: BTreeSet<&str> = ALTERNATIVES.iter().copied().collect();
    assert!(
        named == every,
        "{newest}: alternatives {:?} are neither in a file of its sets nor absent with why",
        every.difference(&named).collect::<Vec<_>>()
    );
}

/// Every line of `RELEASES` has its set in the tree, and every set in the
/// tree its line: a set deleted, or never written for a release that
/// names one, fails here whichever sets `FENNEC_COMPAT_SETS` points the
/// other tests at. And no landed set changes: each holds every file its
/// `SHA256SUMS` names, by that sum, and nothing else, and the digest of the
/// `SHA256SUMS` is the one its line pins -- a set written again, its sums
/// with it, fails here. Its manifest names its tag, and its README the
/// commit its programs were built from.
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
    for w in RELEASES {
        let dir = std::path::PathBuf::from(SETS).join(w.set);
        assert_eq!(tag_of(w.set), w.tag, "{}: a set of another tag", w.set);
        assert!(
            RELEASES.iter().any(|f| f.set == w.tag),
            "{}: a further set of {}, whose first set has no line",
            w.set,
            w.tag
        );
        let sums = std::fs::read(dir.join("SHA256SUMS")).unwrap();
        assert_eq!(
            walk::hex(&walk::sha256(&sums)),
            w.sums,
            "{}/SHA256SUMS is not the one its RELEASES line pins: a landed set is never written again \
             (a form found missing goes into a further set of {}, `{}+2/`)",
            w.set,
            w.tag,
            w.tag
        );
        let mut listed = BTreeSet::new();
        for line in String::from_utf8(sums).unwrap().lines() {
            let (digest, file) = line
                .split_once("  ")
                .unwrap_or_else(|| panic!("{}/SHA256SUMS: {line:?}", w.set));
            let bytes = std::fs::read(dir.join(file))
                .unwrap_or_else(|e| panic!("{}/{file}, in its SHA256SUMS: {e}", w.set));
            assert_eq!(
                walk::hex(&walk::sha256(&bytes)),
                digest,
                "{}/{file}: not the bytes its release wrote",
                w.set
            );
            listed.insert(file.to_string());
        }
        let mut held = BTreeSet::new();
        let mut stack = vec![dir.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let path = e.unwrap().path();
                match path.is_dir() {
                    true => stack.push(path),
                    false => {
                        let rel = path.strip_prefix(&dir).unwrap();
                        let rel = rel.to_string_lossy().replace('\\', "/");
                        if rel != "SHA256SUMS" {
                            held.insert(rel);
                        }
                    }
                }
            }
        }
        assert_eq!(
            held, listed,
            "{}: its files and its SHA256SUMS' lines",
            w.set
        );
        let manifest = std::fs::read_to_string(dir.join("MANIFEST.json")).unwrap();
        let manifest = fennec_core::json::parse_json(&manifest).unwrap();
        let member = |k: &str| match &manifest {
            fennec_core::prelude::Value::Object(o) => o
                .iter()
                .find(|(m, _)| m == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("{}/MANIFEST.json: no `{k}`", w.set)),
            _ => panic!("{}/MANIFEST.json: not an object", w.set),
        };
        let text = |v: fennec_core::prelude::Value| match v {
            fennec_core::prelude::Value::Text(t) => t,
            v => panic!("{}/MANIFEST.json: {v:?}", w.set),
        };
        assert_eq!(text(member("tag")), w.tag, "{}/MANIFEST.json's tag", w.set);
        let commit = text(member("commit"));
        let readme = std::fs::read_to_string(dir.join("README.md")).unwrap();
        assert!(
            commit.len() == 40 && readme.contains(&commit),
            "{}/README.md does not name the commit its programs were built from, {commit}",
            w.set
        );
    }
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
            seen.finish();
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

/// `torn-wide` cut by an open to write it, then opened again with nothing
/// written between, every way a file opens: the second open cuts nothing
/// and answers as the release did, as the first did. The records claiming
/// the last fsync go with the torn tail, and the cut leaves a close's mark
/// after it, synced before the cut (`fs::FileSink::cut`). Without one, the second
/// open found the file durable only as far as the rows left claimed, held
/// the synced row the disk flipped a bit of to its sum and cut it with the
/// row after it -- 5 358 bytes of v0.2.0's file, silently on a Unix; on
/// Windows, whose open cuts and opens again at once, the open was refused
/// ("cut short again").
#[test]
fn torn_wide_cut_by_an_open_opens_again_as_that_open_read_it() {
    fennec_ql::install();
    let name = "torn-wide";
    let opened = |path: &Path, way: &str| {
        match way {
            "mapped" => fennec_core::fs::open(path),
            "in-memory" => fennec_core::fs::open_in_memory(path),
            "serving" => fennec_core::fs::open_serving(path, true, Box::new(Ok)),
            _ => fennec_core::fs::open_read_only(path),
        }
        .unwrap_or_else(|e| panic!("{name}, {way}: {e}"))
    };
    let (mut found, mut held) = (Vec::new(), 0);
    for set in sets() {
        if !set.join(format!("{name}.fennec")).is_file() {
            continue;
        }
        let scratch = Scratch::new(&format!("twice-{}", set_name(&set)));
        let writing = ["mapped", "in-memory", "serving"];
        for way in writing {
            let dir = scratch.0.join(way);
            std::fs::create_dir_all(&dir).unwrap();
            let path = copied(&set, name, &dir);
            let torn = std::fs::metadata(&path).unwrap().len();
            let db = at_clock(opened(&path, way), &set);
            let cut = db.cut_on_open().cloned().expect("the open cut nothing");
            found.extend(differences(&db, &set, name, way));
            drop(db);
            let len = std::fs::metadata(&path).unwrap().len();
            eprintln!(
                "{}/{name}, {way}: {} bytes ({} records) cut at {}, {torn} -> {len}",
                set_name(&set),
                cut.bytes,
                cut.records,
                cut.at
            );
            assert!(
                cut.at <= len && len < torn,
                "{way}: {cut:?}, {len} of {torn}"
            );
            for again in writing.iter().chain(&["read-only"]) {
                let db = at_clock(opened(&path, again), &set);
                let cut = db.cut_on_open().cloned();
                assert_eq!(cut, None, "{way}, then {again}: cut again");
                found.extend(differences(
                    &db,
                    &set,
                    name,
                    &format!("{way}, then {again}"),
                ));
                drop(db);
                let now = std::fs::metadata(&path).unwrap().len();
                assert_eq!(now, len, "{way}, then {again}: the open wrote");
                held += 1;
            }
            // The close's mark after the cut, which claims it.
            assert_eq!(len, cut.at + 8, "{way}: the cut unmarked");
        }
    }
    assert!(held > 0, "no set holds {name}");
    assert!(
        found.is_empty(),
        "{} answers differ:\n{}",
        found.len(),
        found.join("\n")
    );
}

/// `HEAD`'s reading of an index's options, laid out as the walk reads them
/// off the bytes (`walk::index`): a graph's metric, `m`, `ef_construction`,
/// `ef_search` and quantization; a text index's flags (kind 11), k1 and b in
/// hundredths, longest and shortest prefix; an expiry's milliseconds.
fn index_options(k: &IndexKind) -> Vec<u64> {
    match k {
        IndexKind::Vector(s) => {
            let metric = match s.metric {
                Metric::Cosine => 0,
                Metric::L2 => 1,
                Metric::Dot => 2,
            };
            let mut v = vec![
                metric,
                s.m as u64,
                s.ef_construction as u64,
                s.ef_search as u64,
            ];
            if s.quant != Quant::None {
                v.push(s.quant.code() as u64);
            }
            v
        }
        IndexKind::Text(s) => text_options(s),
        IndexKind::Sorted { ttl: Some(t) } => vec![*t],
        _ => Vec::new(),
    }
}

fn text_options(s: &fennec_core::schema::TextIndexSpec) -> Vec<u64> {
    let mut v = Vec::new();
    if s.accents {
        v.push(if s.chars { 3 } else { 2 });
    }
    v.extend([
        s.k1_pct as u64,
        s.b_pct as u64,
        s.prefix_max as u64,
        s.prefix_min as u64,
    ]);
    v
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
/// and every option of it -- a graph's metric, `m`, `ef_construction`,
/// `ef_search` and quantization, a text index's k1, b and prefixes, an
/// expiry -- and the `@text` beside it with its options, and each index
/// over several fields, unique or not, its parts descending or not. A byte
/// read otherwise changes no answer where no statement asks the rule it
/// carries, and every later write it would have held.
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
                    options: index_options(&f.index),
                    beside: f.beside.map(|s| (s.chars, s.accents)),
                    beside_options: f.beside.as_ref().map(text_options).unwrap_or_default(),
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

/// Every collection of a release's file hands out, as the next row's id,
/// the id its records say -- past its image's counter (kind 7) and every id
/// a landed put frame wrote -- and that is the id the release hands out
/// (`<name>.ids`, its own answer): a counter read as the live ids' next
/// would hand out an id the release retired. A file whose walk met a
/// record failing its sum keeps what its claims say of it, which the walk
/// does not judge: there the release's answer alone holds this build's
/// (`every_file_of_every_release_opens_and_answers_as_its_release_did`).
#[test]
fn every_collection_hands_out_the_next_id_its_release_would() {
    let (mut held, mut past) = (0, 0);
    each_database(|set, file, dir, _, seen| {
        let Some(theirs) = release_ids(dir, file) else {
            panic!("{set}/{file}: no {file}.ids, the release's next ids");
        };
        if seen.damaged {
            return;
        }
        let ours = seen.next_ids();
        assert_eq!(
            ours, theirs,
            "{set}/{file}: the next ids its records say, and the release's"
        );
        held += ours.len();
        past += seen
            .names
            .keys()
            .filter(|cid| {
                seen.counters
                    .get(cid)
                    .is_some_and(|&c| Some(&c) > seen.puts.get(cid).map(|p| p + 1).as_ref())
            })
            .count();
    });
    eprintln!("{held} collections' next ids held, {past} of them an image's counter past every id");
    assert!(past > 0, "no image's counter past the ids its frames hold");
}

/// `HEAD`'s reading of a schedule's times, as its record writes them.
fn times_of(t: &fennec_core::engine::Times) -> (u8, Vec<u64>) {
    use fennec_core::engine::Times;
    match *t {
        Times::Step(step, origin) => (1, vec![step as u64, origin as u64]),
        Times::Months(n) => (2, vec![n as u64]),
        Times::Cron(masks) => (3, masks.to_vec()),
    }
}

/// Every named statement a release's file holds at its end reads in this
/// build as its record's bytes say: its version, and a scheduled one's
/// times, zone and `since`; and each one's last slot as its mark says it --
/// when, by which version, ran or refused with its status and reason, and
/// the slots it passed over. The answers hold what they show of each at
/// the set's clock (`describe statement`'s `next` and `last`); this holds
/// the bytes directly, as the schema test does a schema's.
#[test]
fn every_statement_and_mark_a_release_wrote_reads_as_its_bytes() {
    fennec_ql::install();
    let (mut defs, mut timed, mut marked) = (0, 0, 0);
    each_database(|set, file, dir, _, seen| {
        if cfg!(windows) && dir.join(format!("{file}.fennec.sync")).exists() {
            return;
        }
        let scratch = Scratch::new(&format!("statements-{set}-{file}"));
        let path = copied(dir, file, &scratch.0);
        let db =
            fennec_core::fs::open_read_only(&path).unwrap_or_else(|e| panic!("{set}/{file}: {e}"));
        let table = db.statements();
        for (name, def) in &seen.statements {
            let s = table.get(name).unwrap_or_else(|| {
                panic!("{set}/{file}: statement `{name}` not in this build's table")
            });
            assert_eq!(s.version, def.version, "{set}/{file}: `{name}`'s version");
            let ours = s
                .schedule
                .as_ref()
                .map(|t| (times_of(&t.times), t.zone.clone(), t.since));
            let theirs = def
                .timing
                .as_ref()
                .map(|(tag, values, zone, since)| ((*tag, values.clone()), zone.clone(), *since));
            assert_eq!(ours, theirs, "{set}/{file}: `{name}`'s schedule");
            defs += 1;
            timed += usize::from(theirs.is_some());
            let Some(mark) = seen.marks.get(name).filter(|m| m.version == def.version) else {
                continue;
            };
            let ours = db.last_mark(name).unwrap_or_else(|| {
                panic!(
                    "{set}/{file}: `{name}`'s slot of {} is unmarked in this build",
                    mark.at
                )
            });
            assert_eq!(
                (ours.at, ours.version, ours.refused.clone(), ours.skipped),
                (mark.at, mark.version, mark.refused.clone(), mark.skipped),
                "{set}/{file}: `{name}`'s last mark"
            );
            marked += 1;
        }
        let names: BTreeSet<&str> = seen.statements.keys().map(String::as_str).collect();
        let held: BTreeSet<&str> = table.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            held, names,
            "{set}/{file}: the statements its records leave defined"
        );
    });
    eprintln!("{defs} statements read alike, {timed} scheduled, {marked} last marks");
    assert!(
        timed > 0 && marked > 0,
        "no scheduled statement or mark compared"
    );
}

/// Every file's history reads in this build as its last record of it
/// says: following or not, and each fork's id and the change it forked at
/// -- what a replica's sync and a promotion are told apart by.
#[test]
fn every_history_a_release_wrote_reads_as_its_bytes() {
    let mut forks = 0;
    each_database(|set, file, dir, _, seen| {
        if cfg!(windows) && dir.join(format!("{file}.fennec.sync")).exists() {
            return;
        }
        let scratch = Scratch::new(&format!("history-{set}-{file}"));
        let path = copied(dir, file, &scratch.0);
        let db =
            fennec_core::fs::open_read_only(&path).unwrap_or_else(|e| panic!("{set}/{file}: {e}"));
        let h = db.history();
        let theirs = seen.history.clone().unwrap_or((false, Vec::new()));
        assert_eq!(
            (h.following, h.lineage.clone()),
            theirs,
            "{set}/{file}: its history"
        );
        forks += theirs.1.len();
    });
    assert!(forks > 0, "no fork compared");
}

/// A sync log an open spent -- its entries put back, its header wiped --
/// is never put back again: not where the file was cut below its entries'
/// end after, as an older binary's cut of a torn record leaves it, which
/// the wipe exists for. The release's spent log, beside its file cut where
/// its first stale entry begins: read as the cut file, the log naming
/// nothing.
#[cfg(unix)]
#[test]
fn a_spent_sync_log_is_never_put_back_again() {
    let mut held = 0;
    each_database(|set, file, dir, _, seen| {
        let Some(first) = seen.spent_log else {
            return;
        };
        let scratch = Scratch::new(&format!("spent-{set}-{file}"));
        let path = copied(dir, file, &scratch.0);
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(first).unwrap();
        drop(f);
        let cut = std::fs::read(&path).unwrap();
        assert!(
            fennec_core::fs::read_with_sync_log(&path)
                .unwrap()
                .is_none(),
            "{set}/{file}: a spent log's entries put back over a file cut below them"
        );
        let db = fennec_core::fs::open_read_only(&path);
        drop(db);
        assert!(
            std::fs::read(&path).unwrap() == cut,
            "{set}/{file}: a read-only open changed the file"
        );
        held += 1;
    });
    assert!(held > 0, "no spent sync log");
}

/// A sync log is put back only over the file it was written beside, as
/// that file stood when its generation began: its header names the file by
/// device and inode and fingerprints the 4 096 bytes below `base`, where
/// the generation began (`synclog::fingerprint`). Each begun log of a
/// release's set, stamped as its copy's, is put back over the copy; with a
/// byte below `base` changed -- another file at that inode, or the file
/// rewritten -- it is not, and the copy reads as its own bytes.
#[cfg(unix)]
#[test]
fn a_sync_log_is_put_back_only_over_the_bytes_it_fingerprints() {
    let mut held = 0;
    for set in sets() {
        let name = set_name(&set);
        for file in files(&set) {
            let log = set.join(format!("{file}.fennec.sync"));
            if !log.is_file() || std::fs::read(&log).unwrap()[..8] != *b"FENECSYN" {
                continue;
            }
            let scratch = Scratch::new(&format!("print-{name}-{file}"));
            let path = copied(&set, &file, &scratch.0);
            assert!(
                fennec_core::fs::read_with_sync_log(&path)
                    .unwrap()
                    .is_some(),
                "{name}/{file}: its log, stamped as the copy's, not put back"
            );
            let header = std::fs::read(format!("{}.sync", path.display())).unwrap();
            let base = u64::from_le_bytes(header[24..32].try_into().unwrap()) as usize;
            let mut bytes = std::fs::read(&path).unwrap();
            assert!(
                base > 0 && base <= bytes.len(),
                "{name}/{file}: a base of {base}"
            );
            bytes[base - 1] ^= 0x01;
            std::fs::write(&path, &bytes).unwrap();
            assert!(
                fennec_core::fs::read_with_sync_log(&path)
                    .unwrap()
                    .is_none(),
                "{name}/{file}: its log put back over a file whose byte below its base changed"
            );
            held += 1;
        }
    }
    assert!(held > 0, "no begun sync log");
}

/// A statement over `db` as a write, its outcome: the words of a refusal.
fn tried(db: &mut fennec_core::prelude::Database, q: &str) -> Result<(), String> {
    let stmt = fennec_ql::parse_for(db, q)
        .map_err(|e| e.to_string())?
        .remove(0);
    db.execute_with(&stmt, &[])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Whether `answer` is a refusal in `words`.
fn refused_in(answer: &str, words: &str) -> bool {
    answer.starts_with("{\"kind\":\"error\"") && answer.contains(words)
}

/// The files a release wrote that this build reads otherwise on purpose
/// (docs/design/compatibility.md, *What 1.x does not read of 0.2.0's
/// files*; [`ON_PURPOSE`]): a schema past a limit refused at the open,
/// read-only and to write, in its words; a row nesting past 64 levels
/// refused where it is read -- exactly the statements named, each in its
/// words, and every other statement over the file answering as the release
/// did -- and, on copies of `deep.fennec`, each step that stands on 1.x
/// ([`deep_steps`]): the row's `del` refused and a compact taken, copying
/// the row as it is; its field dropped, the row read and a compact
/// taken, copying the row as it is; refused again, the row and
/// its `del`, once a field is declared after the dropped one; the
/// collection dropped and the rest of the file answering as the release
/// did.
#[test]
fn the_files_this_build_refuses_on_purpose_are_refused_in_its_words() {
    fennec_ql::install();
    let mut held = 0;
    for set in sets() {
        let name = set_name(&set);
        for (file, how) in ON_PURPOSE {
            let scratch = Scratch::new(&format!("on-purpose-{name}-{file}"));
            let path = copied(&set, file, &scratch.0);
            verified(&path, file, false);
            match how {
                Otherwise::OpenRefused(words) => {
                    for (when, opened) in [
                        ("read-only", fennec_core::fs::open_read_only(&path)),
                        ("to write", fennec_core::fs::open(&path)),
                    ] {
                        match opened {
                            Ok(_) => {
                                panic!("{name}/{file}: opened {when}, where this build refuses it")
                            }
                            Err(e) => assert!(
                                e.to_string().contains(words),
                                "{name}/{file}: refused {when} for {e}, not `{words}`"
                            ),
                        }
                    }
                }
                Otherwise::RowRefused { words, statements } => {
                    let texts = answers::statements(&set, file);
                    for s in *statements {
                        assert!(
                            texts.iter().any(|t| t == s),
                            "{name}/{file}: `{s}` is none of its statements"
                        );
                    }
                    let release =
                        std::fs::read_to_string(set.join(format!("{file}.answers"))).unwrap();
                    let release: Vec<String> = release.lines().map(str::to_string).collect();
                    // Every answer over a copy as it stands: the statements
                    // named refused in the words (where `refused` asks it),
                    // every other the release's.
                    let held_to = |path: &Path, when: &str, refused: bool| -> Vec<String> {
                        let db = at_clock(
                            fennec_core::fs::open_read_only(path)
                                .unwrap_or_else(|e| panic!("{name}/{file} ({when}): {e}")),
                            &set,
                        );
                        texts
                            .iter()
                            .zip(&release)
                            .filter_map(|(text, theirs)| {
                                let ours = answers::answer(&db, text);
                                match statements.contains(&text.as_str()) {
                                    true if refused && !refused_in(&ours, words) => Some(format!(
                                        "{name}/{file} ({when}): {text} answered {ours}, where this \
                                         build refuses it in `{words}`"
                                    )),
                                    false if ours != *theirs => Some(format!(
                                        "{name}/{file} ({when}): {text}\n  release: {theirs}\n  this build: {ours}"
                                    )),
                                    _ => None,
                                }
                            })
                            .collect()
                    };
                    let mut found = held_to(&path, "as the release wrote it", true);
                    if *file == "deep" {
                        found.extend(deep_steps(&set, &scratch.0, words, &held_to));
                    }
                    assert!(found.is_empty(), "{}", found.join("\n"));
                }
            }
            held += 1;
        }
    }
    assert!(held > 0, "no file read otherwise on purpose");
}

/// What stands on 1.x beside `deep.fennec`'s row -- its field `j`, a
/// `[[json]]`, the collection's last -- each step on a copy of its own, the
/// file opened anew after each write:
/// - as the release wrote it, the row's `del`, a `set` of it and a `put`
///   writing over it are refused in the words, the rest answering as the
///   release did; a `compact` is taken and copies the row as it is, every
///   answer of the file the release's once it opens again, the row refused;
/// - `j` dropped, the row reads; a `compact` is taken, copying the row as
///   it is with its dropped place, the row still read; once the row is
///   deleted, or written over, a `compact` is taken: dropping the field
///   never frees the file of the row by itself;
/// - the other layout, `j` dropped and a field added after it: the row and
///   its `del` refused again, the way out the collection dropped, the rest
///   of the file answering as the release did.
fn deep_steps(
    set: &Path,
    scratch: &Path,
    words: &str,
    held_to: &dyn Fn(&Path, &str, bool) -> Vec<String>,
) -> Vec<String> {
    let row = "get deep where n = 2";
    let mut found = Vec::new();
    let copy = |step: &str| {
        let dir = scratch.join(step);
        std::fs::create_dir_all(&dir).unwrap();
        copied(set, "deep", &dir)
    };
    // One statement as a write, the file opened for it alone.
    let write = |path: &Path, q: &str| match fennec_core::fs::open(path) {
        Ok(mut db) => tried(&mut db, q),
        Err(e) => Err(format!("opened again: {e}")),
    };
    let read = |path: &Path, q: &str| {
        let db = at_clock(fennec_core::fs::open_read_only(path).unwrap(), set);
        answers::answer(&db, q)
    };
    let answered = |a: String| match a.starts_with("{\"kind\":\"error\"") {
        true => Err(a),
        false => Ok(()),
    };
    let mut want = |what: &str, ok: bool, got: Result<(), String>| {
        let held = match (&got, ok) {
            (Ok(()), true) => true,
            (Err(e), false) => e.contains(words),
            _ => false,
        };
        eprintln!("deep: {what}: {got:?}");
        if !held {
            found.push(format!(
                "deep: {what}: {got:?}, where {} was held",
                match ok {
                    true => "Ok".to_string(),
                    false => format!("a refusal in `{words}`"),
                }
            ));
        }
    };
    let mut also = Vec::new();

    // As the release wrote it: every write that reads the row refused,
    // every answer as it was; a compact copies the row as it is.
    let path = copy("as-written");
    for (what, q) in [
        ("a del of the row", "del deep where n = 2"),
        ("a set of the row", "set deep {n: 5} where n = 2"),
        (
            "a put writing over the row",
            "put deep {id: 2, n: 2, j: [[1]]}",
        ),
    ] {
        want(what, false, write(&path, q));
    }
    also.extend(held_to(&path, "its del, set and put refused", true));
    want("a compact", true, write(&path, "compact"));
    also.extend(held_to(&path, "compacted", true));

    // Its field dropped, the collection's last: the row reads, and a
    // compact copies it as it is.
    let dropped = |step: &str| {
        let path = copy(step);
        let mut db = fennec_core::fs::open(&path).unwrap();
        let got = tried(&mut db, "alter collection deep drop field j");
        drop(db);
        (path, got)
    };
    let (path, got) = dropped("dropped");
    want("a drop of its field", true, got);
    want(
        "the row, its field dropped",
        true,
        answered(read(&path, row)),
    );
    for when in ["a compact, its field dropped", "a compact asked again"] {
        want(when, true, write(&path, "compact"));
        want(
            &format!("the row after {when}"),
            true,
            answered(read(&path, row)),
        );
    }
    // Deleted, or written over: the compact is taken.
    for (step, q, count) in [
        ("deleted", "del deep where n = 2", 1),
        ("written-over", "put deep {id: 2, n: 2}", 2),
    ] {
        let (path, got) = dropped(step);
        want("a drop of its field", true, got);
        want(&format!("`{q}`, its field dropped"), true, write(&path, q));
        want(
            &format!("a compact after `{q}`"),
            true,
            write(&path, "compact"),
        );
        let counted = read(&path, "get deep count");
        want(
            &format!("the collection's count after `{q}` and a compact"),
            true,
            match counted.contains(&format!("\"count\":{count}")) {
                true => Ok(()),
                false => Err(counted),
            },
        );
        for q in ["get deep where n = 1", "get plain order n"] {
            want(
                &format!("`{q}` after the compact"),
                true,
                answered(read(&path, q)),
            );
        }
    }

    // The other layout: a field added after the dropped one, whose place is
    // reached by skipping it -- the row refused again, and its `del`.
    let (path, got) = dropped("drop-and-add");
    want("a drop of its field", true, got);
    want(
        "a field added after it",
        true,
        write(&path, "alter collection deep add field k int"),
    );
    want(
        "the row, a field after the dropped one",
        false,
        answered(read(&path, row)),
    );
    want(
        "a del of the row, a field after the dropped one",
        false,
        write(&path, "del deep where n = 2"),
    );
    want(
        "the other row",
        true,
        answered(read(&path, "get deep where n = 1")),
    );
    // The way out: the collection dropped, the rest of the file as it was.
    want(
        "the collection dropped",
        true,
        write(&path, "drop collection deep"),
    );
    let untouched = copy("untouched");
    for q in ["get plain order n", "get plain where n = 2"] {
        let theirs = read(&untouched, q);
        want(
            &format!("`{q}` once the collection went"),
            true,
            match read(&path, q) == theirs {
                true => Ok(()),
                false => Err(read(&path, q)),
            },
        );
    }
    found.extend(also);
    found
}

/// What stands on 1.x beside `many.fennec`'s collection of 4 097 fields,
/// more places than 4 096, which the release made with no bound: on a copy,
/// the file opened anew for each, a field renamed and one dropped, taken,
/// one added, refused (`takes no field more`: its fields alone fill the
/// places a collection holds), then a compact, taken, each step's outcome
/// held, and after each every row read at its places as the release's file
/// reads them.
#[test]
fn a_collection_of_more_than_4_096_places_takes_a_rename_and_a_drop_and_refuses_a_field_more() {
    fennec_ql::install();
    let mut held = 0;
    for set in sets() {
        if !set.join("many.fennec").is_file() {
            continue;
        }
        let name = set_name(&set);
        let scratch = Scratch::new(&format!("many-{name}"));
        let copy = |step: &str| {
            let dir = scratch.0.join(step);
            std::fs::create_dir_all(&dir).unwrap();
            copied(&set, "many", &dir)
        };
        let (untouched, path) = (copy("untouched"), copy("altered"));
        let read = |path: &Path, q: &str| {
            let db = at_clock(fennec_core::fs::open_read_only(path).unwrap(), &set);
            answers::answer(&db, q)
        };
        // Its rows at places before the dropped one, past it and at the
        // last, read through the last field's index and in its order; `f0`
        // named `first` once renamed.
        let rows = |path: &Path, f0: &str| -> Vec<String> {
            [
                "get many where f4096 = 4096 select {f0}, f2, f2048, f4095, f4096",
                "get many where f4096 = -1 select {f0}, f2048, f4096",
                "get many order f4096 select {f0}, f2, f4096",
                "get many count",
            ]
            .iter()
            .map(|q| read(path, &q.replace("{f0}", f0)))
            .collect()
        };
        let theirs: Vec<String> = rows(&untouched, "f0")
            .iter()
            .map(|a| a.replace("\"f0\"", "\"first\""))
            .collect();
        assert!(
            theirs.iter().all(|a| !a.starts_with("{\"kind\":\"error\"")),
            "{name}/many: {theirs:?}"
        );
        let mut found = Vec::new();
        // Each step, and what it is held to: taken, or refused in words.
        let steps: [(&str, &str, Result<(), &str>); 4] = [
            (
                "a field renamed",
                "alter collection many rename field f0 to first",
                Ok(()),
            ),
            (
                "a field dropped",
                "alter collection many drop field f1",
                Ok(()),
            ),
            (
                "a field added",
                "alter collection many add field extra int",
                Err("takes no field more"),
            ),
            ("a compact", "compact", Ok(())),
        ];
        for (what, q, want) in steps {
            let got = match fennec_core::fs::open(&path) {
                Ok(mut db) => tried(&mut db, q),
                Err(e) => Err(format!("opened again: {e}")),
            };
            eprintln!("many: {what}: {got:?}");
            let as_held = match (&got, &want) {
                (Ok(()), Ok(())) => true,
                (Err(e), Err(words)) => e.contains(words),
                _ => false,
            };
            if !as_held {
                found.push(format!(
                    "{name}/many: {what}: {got:?}, where {want:?} was held"
                ));
            }
            let ours = rows(&path, "first");
            if ours != theirs {
                found.push(format!(
                    "{name}/many, after {what}:\n  the release's rows: {theirs:?}\n  this build: {ours:?}"
                ));
            }
        }
        assert!(found.is_empty(), "{}", found.join("\n"));
        held += 1;
    }
    assert!(held > 0, "no set holds many.fennec");
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

//! What every `compat::` test asks of a release's file, shared by
//! fennec-core's, fennec-http's, fennec's and fennec-server's (`#[path]`):
//! the sets, the statements, this build's answers beside the release's,
//! and the writes after them.
//!
//! An answer that differs is a finding, never a fixture to write again: a
//! set is its release's, written once. A difference a change accepted on
//! purpose explains is named in `ACCEPTED`, with the change and why.

#![allow(dead_code)]

use fennec_core::prelude::*;
use fennec_core::schema::{Field, IndexKind, Schema};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The sets, a directory a release, from any crate's tests.
pub const SETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../fennec-core/tests/compat");

/// Answers a later build gives otherwise, on purpose: (set, file,
/// statement, why). None so far.
pub const ACCEPTED: &[(&str, &str, &str, &str)] = &[];

/// A request to a release's server file (`server.http.json`) a later build
/// answers otherwise, on purpose. The release's answer stays in the set;
/// this build is held to the answer named here instead, so any other
/// answer is still a difference.
pub struct AcceptedRequest {
    pub set: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    /// The request's `Idempotency-Key`, where it sends one.
    pub key: Option<&'static str>,
    /// This build's answer.
    pub status: u16,
    pub body: &'static str,
    pub why: &'static str,
}

pub const ACCEPTED_REQUESTS: &[AcceptedRequest] = &[AcceptedRequest {
    set: "v0.2.0",
    method: "POST",
    path: "/query",
    key: Some("compat-idempotency-key"),
    status: 409,
    body: r#"{"error":"`orders.ref` is unique, and document 4 holds \"o-4\" already"}"#,
    why: "#677 (train-e): a key is kept as its caller's 16 bytes and the key's own, and \
          `_idempotency` as v0.2.0 kept it, its `key` text, is made again at the first keyed \
          write, so a keyed write sent before the upgrade and again after it runs again \
          (docs/design/http.md#idempotency-keys, compatibility.md's *Changed on purpose \
          before 1.0*, compatibility.html's upgrade from 0.2.0): v0.2.0 answered the kept \
          `{\"affected\":1}`, this build runs the put again, which clashes with the row it \
          wrote",
}];

/// What this build does with a file a release wrote that it reads
/// otherwise on purpose (docs/design/compatibility.md, *What 1.x does not
/// read of 0.2.0's files*).
pub enum Otherwise {
    /// The file opens; a row past a limit is refused where it is read: the
    /// statements named, each in these words, and every other answer is
    /// the release's.
    RowRefused {
        words: &'static str,
        statements: &'static [&'static str],
    },
    /// The open is refused, in these words.
    OpenRefused(&'static str),
}

/// A set's files this build reads otherwise on purpose, by name: each held
/// by `compat::the_files_this_build_refuses_on_purpose_are_refused_in_its_words`,
/// and passed over by the tests that open every other file and write into it.
pub const ON_PURPOSE: &[(&str, Otherwise)] = &[
    (
        "deep",
        // The one row nesting 66 levels; the other row, the collection's
        // count and the fields before the deep one answer.
        Otherwise::RowRefused {
            words: "a value nests more than 64 levels deep",
            statements: &["get deep where n = 2"],
        },
    ),
    (
        "refused-lists",
        // A file's schema is read by the value decoder's bound
        // (`codec::decode_type`), in its words.
        Otherwise::OpenRefused("a value nests more than 64 levels deep"),
    ),
    (
        "refused-dims",
        Otherwise::OpenRefused("a vector's dimension is 1 to 1000000000"),
    ),
    (
        "points-deep",
        // A point is a level of its type as it is of the value it reads as:
        // 64 lists around one are 65, refused where the schema is read
        // (`decode_type`), as `refused-lists` is.
        Otherwise::OpenRefused("a value nests more than 64 levels deep"),
    ),
];

/// Whether this build reads `name` otherwise on purpose ([`ON_PURPOSE`]).
pub fn on_purpose(name: &str) -> bool {
    ON_PURPOSE.iter().any(|(n, _)| *n == name)
}

/// The clock a set's answers were read at (its `clock` file, epoch
/// milliseconds): a scheduled statement's `next` is computed from it, and
/// this build reads its answers at the same.
pub fn clock(set: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(set.join("clock")).ok()?;
    Some(
        text.trim()
            .parse()
            .unwrap_or_else(|_| panic!("{}/clock: {text:?}", set.display())),
    )
}

/// `db` reading the set's clock, as the release's answers were read.
pub fn at_clock(mut db: Database, set: &Path) -> Database {
    db.set_clock(clock(set));
    db
}

/// The id the release hands the next row put into each collection of
/// `name` (`<name>.ids`, `<collection> <id>` a line), where the set has it.
pub fn release_ids(set: &Path, name: &str) -> Option<BTreeMap<String, u64>> {
    let text = std::fs::read_to_string(set.join(format!("{name}.ids"))).ok()?;
    Some(
        text.lines()
            .map(|l| {
                let (c, id) = l
                    .rsplit_once(' ')
                    .unwrap_or_else(|| panic!("{name}.ids: {l:?}"));
                (
                    c.to_string(),
                    id.parse().unwrap_or_else(|_| panic!("{name}.ids: {l:?}")),
                )
            })
            .collect(),
    )
}

/// Each collection whose next id in this build is not the one the release
/// hands out (`<name>.ids`): a counter read otherwise hands out an id the
/// release retired, or skips some.
pub fn id_differences(db: &Database, set: &Path, name: &str, when: &str) -> Vec<String> {
    let Some(theirs) = release_ids(set, name) else {
        return Vec::new();
    };
    let mut names = db.collection_names();
    names.sort();
    let ours: BTreeMap<String, u64> = names
        .into_iter()
        .map(|n| {
            let id = db.collection(&n).unwrap().store.next_id();
            (n, id)
        })
        .collect();
    match ours == theirs {
        true => Vec::new(),
        false => vec![format!(
            "{}/{name} ({when}): the next ids\n  release: {theirs:?}\n  this build: {ours:?}",
            set_name(set)
        )],
    }
}

/// Every set, oldest first: those under `FENNEC_COMPAT_SETS` where it names
/// a directory -- a set `tools/compat_fixtures.py write` has just written,
/// held to every test before it lands -- and the tree's otherwise. A
/// directory of sets holds sets alone, each `v<major>.<minor>.<patch>/`,
/// or `v<major>.<minor>.<patch>+<n>/` for a further set of the same tag,
/// with its README and sums: pointed at a set itself, or at a directory
/// holding none, every test fails rather than pass over nothing or read
/// `archive/` as a release's set.
pub fn sets() -> Vec<PathBuf> {
    let root =
        std::env::var_os("FENNEC_COMPAT_SETS").map_or_else(|| PathBuf::from(SETS), PathBuf::from);
    let mut v: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("{}: {e}", root.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort_by_key(|p| version_of(&set_name(p)));
    assert!(!v.is_empty(), "no fixture set under {}", root.display());
    for p in &v {
        let name = set_name(p);
        assert!(
            version_of(&name) != (0, 0, 0, 0),
            "{}: {name} is no set (`v<version>/`): FENNEC_COMPAT_SETS names the directory \
             holding the sets, never a set itself",
            root.display()
        );
        for must in ["README.md", "SHA256SUMS"] {
            assert!(
                p.join(must).is_file(),
                "{}: no {must}, which `tools/compat_fixtures.py write` writes",
                p.display()
            );
        }
    }
    v
}

/// A set's place among the sets: `v1.2.3`'s numbers and 1, `v1.2.3+2`'s
/// and 2 -- a further set of the same tag after the first -- or zeros
/// where the name is no set's.
pub fn version_of(name: &str) -> (u32, u32, u32, u32) {
    let (tag, nth) = match name.split_once('+') {
        Some((tag, n)) => match n.parse::<u32>() {
            Ok(k) if k >= 2 && n == k.to_string() => (tag, k),
            _ => return (0, 0, 0, 0),
        },
        None => (name, 1),
    };
    let n: Vec<u32> = tag
        .strip_prefix('v')
        .unwrap_or("")
        .split('.')
        .map_while(|x| x.parse().ok())
        .collect();
    match n[..] {
        [a, b, c] if tag == format!("v{a}.{b}.{c}") => (a, b, c, nth),
        _ => (0, 0, 0, 0),
    }
}

/// The tag a set's binaries were built from: its name, `+<n>` aside.
pub fn tag_of(name: &str) -> &str {
    name.split_once('+').map_or(name, |(tag, _)| tag)
}

pub fn set_name(set: &Path) -> String {
    set.file_name().unwrap().to_string_lossy().into_owned()
}

/// Whether the release wrote `name` torn (`torn-*.fennec`), as a crash
/// leaves a file, or a disk that flipped a bit where it was synced:
/// `fennec verify` refuses it as it lies, and an open cuts a torn end off,
/// reading below the durable point unchecked.
pub fn torn(name: &str) -> bool {
    name.starts_with("torn")
}

/// A directory of a test's own, gone with it.
pub struct Scratch(pub PathBuf);
impl Scratch {
    pub fn new(tag: &str) -> Scratch {
        // Numbered: tests running at once walk the same files under the
        // same tags, and one's drop would take another's directory.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d =
            std::env::temp_dir().join(format!("fennec-compat-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Scratch(d)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `fennec verify` of a database file: every record held to its sum, the
/// sync log's writes put back first as an open would. A file the release
/// wrote torn is refused naming its damage; every other verifies.
pub fn verified(path: &Path, name: &str, torn: bool) {
    let checked = match fennec_core::fs::read_with_sync_log(path).unwrap() {
        Some(bytes) => fennec_core::engine::check_records(&bytes),
        None => fennec_core::engine::check_reader(&mut std::fs::File::open(path).unwrap()),
    };
    match (checked, torn) {
        (Ok(n), false) => assert!(n > 0, "{name}: no record"),
        (Err(e), true) => {
            let e = e.to_string();
            assert!(
                e.contains("runs past the end") || e.contains("fails its checksum"),
                "{name}: refused for {e}, not its torn end"
            );
        }
        (Ok(_), true) => panic!("{name}: torn as the release wrote it, and verified"),
        (Err(e), false) => panic!("{}: {e}", path.display()),
    }
}

/// The fewest statements a file of a set is asked: a list cut short or
/// emptied would hold the file to nothing.
pub const MIN_STATEMENTS: usize = 4;

/// The statements of `<name>.fennecql`, a line each.
pub fn statements(set: &Path, name: &str) -> Vec<String> {
    let v: Vec<String> = std::fs::read_to_string(set.join(format!("{name}.fennecql")))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("--"))
        .map(str::to_string)
        .collect();
    assert!(
        v.len() >= MIN_STATEMENTS,
        "{}/{name}.fennecql: {} statements",
        set.display(),
        v.len()
    );
    v
}

/// This build's answer to `text`, as the writer wrote its release's:
/// `json::response_to_string`, or the error's.
pub fn answer(db: &Database, text: &str) -> String {
    match fennec_ql::parse_for(db, text) {
        Ok(stmts) => {
            assert_eq!(stmts.len(), 1, "{text}: one statement a line");
            match db.query(&stmts[0], &[]) {
                Ok(r) => fennec_core::json::response_to_string(&r),
                Err(e) => fennec_core::json::error_to_string(&e),
            }
        }
        Err(e) => fennec_core::json::error_to_string(&e),
    }
}

/// Each statement of `<name>` whose answer is not the release's, with
/// both, but those `ACCEPTED` names; `when` says which open it was.
pub fn differences(db: &Database, set: &Path, name: &str, when: &str) -> Vec<String> {
    let release = std::fs::read_to_string(set.join(format!("{name}.answers"))).unwrap();
    let release: Vec<&str> = release.lines().collect();
    let texts = statements(set, name);
    assert_eq!(texts.len(), release.len(), "{name}: a statement an answer");
    let set = set_name(set);
    texts
        .iter()
        .zip(release)
        .filter_map(|(text, theirs)| {
            let ours = answer(db, text);
            let accepted = ACCEPTED
                .iter()
                .any(|(s, f, t, _)| *s == set && *f == name && t == text);
            (ours != theirs && !accepted).then(|| {
                format!("{set}/{name} ({when}): {text}\n  release: {theirs}\n  this build: {ours}")
            })
        })
        .collect()
}

pub fn run(db: &mut Database, text: &str) -> Response {
    let stmts = fennec_ql::parse_for(db, text).unwrap_or_else(|e| panic!("{text}: {e}"));
    let mut last = Response::Affected(0);
    for s in stmts {
        last = db
            .execute_with(&s, &[])
            .unwrap_or_else(|e| panic!("{text}: {e}"));
    }
    last
}

fn ask(db: &Database, text: &str) -> std::result::Result<Response, String> {
    let stmt = fennec_ql::parse_for(db, text)
        .map_err(|e| e.to_string())?
        .remove(0);
    db.query(&stmt, &[]).map_err(|e| e.to_string())
}

fn ids(db: &Database, collection: &str) -> BTreeSet<i64> {
    let r = ask(db, &format!("get {collection} select id")).unwrap();
    r.rows()
        .unwrap()
        .rows
        .iter()
        .map(|row| row.id as i64)
        .collect()
}

/// The collections a statement may write: the database's own, `_`, are the
/// server's and the sync's, which their own paths write.
fn writable(db: &Database) -> Vec<String> {
    db.collection_names()
        .into_iter()
        .filter(|n| !n.starts_with('_'))
        .collect()
}

/// A value of `ty` as FennecQL writes it, the `k`th row's of the field at
/// `seed`: never one a release's row holds, nor another row's of this
/// build -- a vector of a direction of its own, its spike at a place of
/// its own, so it is its own nearest under every metric.
fn literal(ty: &DataType, k: usize, seed: usize) -> String {
    let u = k * 100 + seed;
    match ty {
        DataType::Bool => (k == 0).to_string(),
        DataType::Int => (7_000_000 + u).to_string(),
        DataType::Float => format!("{}.5", 7000 + u),
        DataType::Text => format!("\"quokka{u}x after\""),
        DataType::Bytes => format!("[1, 2, {}]", u % 256),
        DataType::Timestamp => format!("\"2098-0{}-{:02}T00:00:00Z\"", k + 1, 1 + seed % 28),
        DataType::Vector(dim, _) => {
            let v: Vec<String> = (0..*dim)
                .map(|i| {
                    let base = ((u * 7 + i * 3 + 1) % 15) as f64 / 8.0 - 0.875;
                    let spike = if i == (k + seed) % dim { 50.0 } else { 0.0 };
                    format!("{}", base + spike)
                })
                .collect();
            format!("[{}]", v.join(", "))
        }
        // Indices from 1; a release's rows hold the low ones.
        DataType::Sparse(n) => format!("\"{{{}:9.5}}/{n}\"", [*n, n - 1][k % 2]),
        DataType::List(inner) => format!(
            "[{}, {}]",
            literal(inner, k, seed),
            literal(inner, k, seed + 50)
        ),
        DataType::Json => {
            format!("{{kind: \"after{u}\", n: {u}, deep: {{x: [1, \"two\", null]}}}}")
        }
        DataType::Geo => format!(
            "[{}, -45.25]",
            -100.0 + 10.0 * k as f64 + seed as f64 * 0.01
        ),
    }
}

/// What finds the `k`th row through `f`'s index, by its own operator, and
/// whether it must be the first and only row.
fn probe(c: &str, f: &Field, lit: &str, k: usize, seed: usize) -> Vec<(String, bool)> {
    let u = k * 100 + seed;
    let mut out = Vec::new();
    let text = |out: &mut Vec<(String, bool)>| {
        out.push((
            format!("get {c} match {} \"quokka{u}x\" select id", f.name),
            false,
        ))
    };
    match &f.index {
        IndexKind::None => {}
        IndexKind::Hash { .. } | IndexKind::Sorted { .. } => {
            out.push((format!("get {c} where {} = {lit} select id", f.name), false))
        }
        IndexKind::Text(_) => text(&mut out),
        IndexKind::Vector(_) | IndexKind::Geo | IndexKind::Inverted => out.push((
            format!("get {c} near {} {lit} limit 1 select id", f.name),
            true,
        )),
    }
    if f.beside.is_some() {
        text(&mut out);
    }
    out
}

/// The rows this build wrote into a release's file, each by its
/// collection and id, read back as written; and what finds each through
/// each index, by its own operator.
pub struct Written {
    rows: BTreeMap<(String, i64), String>,
    probes: Vec<(String, i64, bool)>,
    before: BTreeMap<String, BTreeSet<i64>>,
    /// The id this build handed the first row it put into each collection
    /// no write had reached since the open: the release's next id.
    pub first: BTreeMap<String, u64>,
}

/// This build's writes into the release's file: into every collection of
/// the release's schemas, two rows holding a value in every field -- every
/// type, through every index, a path into a json field and an index over
/// several fields among them -- with whatever a statement bound to the
/// write wrote; and a collection of this build's own, made and dropped.
pub fn write_after(db: &mut Database) -> Written {
    let names = writable(db);
    let before: BTreeMap<String, BTreeSet<i64>> =
        names.iter().map(|n| (n.clone(), ids(db, n))).collect();
    run(
        db,
        "create collection compat_after (n int @hash, s text @text)",
    );
    run(
        db,
        r#"put compat_after [{n: 1, s: "one"}, {n: 2, s: "two"}]"#,
    );
    run(db, r#"set compat_after {s: "uno"} where n = 1"#);
    run(db, "del compat_after where n = 2");
    run(db, "drop collection compat_after");
    let mut probes = Vec::new();
    let mut first = BTreeMap::new();
    for c in &names {
        let schema = Schema::clone(&db.collection(c).unwrap().schema);
        for k in 0..2 {
            let mut doc = Vec::new();
            let mut lits = BTreeMap::new();
            for (seed, f) in schema.fields.iter().enumerate() {
                let lit = literal(&f.ty, k, seed);
                doc.push(format!("{}: {lit}", f.name));
                lits.insert(f.name.clone(), (lit, seed));
            }
            // Ids before the put: a statement bound to another
            // collection's write may have written here since.
            let was = ids(db, c);
            let untouched = k == 0 && was == before[c];
            run(db, &format!("put {c} {{{}}}", doc.join(", ")));
            let new: Vec<i64> = ids(db, c).difference(&was).copied().collect();
            assert_eq!(new.len(), 1, "{c}: one row put, {new:?} new");
            let id = new[0];
            if untouched {
                first.insert(c.clone(), id as u64);
            }
            for f in &schema.fields {
                let (lit, seed) = &lits[&f.name];
                for (q, first) in probe(c, f, lit, k, *seed) {
                    probes.push((q, id, first));
                }
            }
            // A path into a json field: its object's `kind` or `n`
            // (`literal`'s object).
            for p in &schema.paths {
                let Some((field, key)) = p.name.split_once('.') else {
                    continue;
                };
                let Some((_, seed)) = lits.get(field) else {
                    continue;
                };
                let u = k * 100 + seed;
                let value = match key {
                    "kind" => format!("\"after{u}\""),
                    "n" => u.to_string(),
                    _ => continue,
                };
                probes.push((
                    format!("get {c} where {} = {value} select id", p.name),
                    id,
                    false,
                ));
            }
            // An index over several fields: every part's value at once.
            for comp in &schema.composites {
                let terms: Option<Vec<String>> = comp
                    .keys
                    .iter()
                    .map(|(part, _)| lits.get(part).map(|(lit, _)| format!("{part} = {lit}")))
                    .collect();
                if let Some(terms) = terms {
                    probes.push((
                        format!("get {c} where {} select id", terms.join(" and ")),
                        id,
                        false,
                    ));
                }
            }
        }
    }
    // Every row the writes made, those a bound statement wrote among them,
    // read back as this build wrote them.
    let mut rows = BTreeMap::new();
    for c in &names {
        for id in ids(db, c).difference(&before[c]) {
            rows.insert((c.clone(), *id), row(db, c, *id));
        }
    }
    Written {
        rows,
        probes,
        before,
        first,
    }
}

fn row(db: &Database, c: &str, id: i64) -> String {
    match ask(db, &format!("get {c} where id = {id}")) {
        Ok(r) => fennec_core::json::response_to_string(&r),
        Err(e) => format!("{{\"error\": {e:?}}}"),
    }
}

/// Each row of `w` read back as it was written, and found through every
/// index by its own operator: what differs, `when` naming the open.
pub fn read_back(db: &Database, w: &Written, name: &str, when: &str) -> Vec<String> {
    let mut found = Vec::new();
    for ((c, id), was) in &w.rows {
        let now = row(db, c, *id);
        if &now != was {
            found.push(format!(
                "{name} ({when}): {c} row {id}\n  written: {was}\n  read: {now}"
            ));
        }
    }
    for (q, id, first) in &w.probes {
        let got: Vec<i64> = match ask(db, q) {
            Ok(r) => r.rows().unwrap().rows.iter().map(|r| r.id as i64).collect(),
            Err(e) => {
                found.push(format!("{name} ({when}): {q}: {e}"));
                continue;
            }
        };
        let ok = match first {
            true => got == [*id],
            false => got.contains(id),
        };
        if !ok {
            found.push(format!("{name} ({when}): {q} finds {got:?}, not row {id}"));
        }
    }
    found
}

/// The rows of `w` taken out again, so the release's answers stand.
pub fn take_out(db: &mut Database, w: &Written) {
    for (c, id) in w.rows.keys() {
        run(db, &format!("del {c} where id = {id}"));
    }
    for (c, before) in &w.before {
        assert_eq!(&ids(db, c), before, "{c}: the rows it held");
    }
}

/// The file at `path`, a release's `name`, held to its answers: opened to
/// write and answering as the release did; this build's rows of every type
/// written into it, read back and found through every index, and again
/// once opened anew -- the release's records and this build's after them,
/// as a server upgraded in place reads its file; then taken out, the
/// release's answers held again, compacted -- this build rewriting the
/// release's rows -- and opened once more, answering alike; the file
/// verified at the end. A replica's file takes no write of its own, and is
/// held to that. Three opens and one compact: each close and compact is an
/// fsync, `F_FULLFSYNC` on macOS, most of what a file costs here. The
/// differences.
pub fn written_and_compacted(path: &Path, set: &Path, name: &str) -> Vec<String> {
    let open = |when: &str| {
        at_clock(
            fennec_core::fs::open(path).unwrap_or_else(|e| panic!("{name}: an open {when}: {e}")),
            set,
        )
    };
    let mut found = Vec::new();
    let mut db = open("to write");
    found.extend(differences(&db, set, name, "open"));
    found.extend(id_differences(&db, set, name, "open"));
    if db.history().following {
        let c = writable(&db).remove(0);
        let stmt = fennec_ql::parse_for(&db, &format!("put {c} {{}}"))
            .unwrap()
            .remove(0);
        match db.execute_with(&stmt, &[]) {
            Err(Error::ReadOnly(_)) => {}
            r => found.push(format!("{name}: a replica's file took a write: {r:?}")),
        }
    } else {
        let w = write_after(&mut db);
        // The first row put into each collection takes the id the release
        // hands out next: past every id it retired, its image's counter
        // (kind 7) and its tail's frames.
        if let Some(theirs) = release_ids(set, name) {
            for (c, id) in &w.first {
                if theirs.get(c) != Some(id) {
                    found.push(format!(
                        "{}/{name}: the first row put into {c} took id {id}, the release's next is {:?}",
                        set_name(set),
                        theirs.get(c)
                    ));
                }
            }
        }
        eprintln!(
            "{name}: {} rows written, {} looks through an index",
            w.rows.len(),
            w.probes.len()
        );
        found.extend(read_back(&db, &w, name, "written"));
        drop(db);
        db = open("after the writes");
        found.extend(read_back(&db, &w, name, "written, opened again"));
        take_out(&mut db, &w);
        found.extend(differences(&db, set, name, "written and taken out"));
    }
    run(&mut db, "compact");
    drop(db);
    let db = open("after the compact");
    found.extend(differences(&db, set, name, "compacted, opened again"));
    drop(db);
    verified(path, name, false);
    found
}

/// The file at `path` with this build's rows of every type written into
/// it and compacted, its answers not asked: what a tool's own check of the
/// file after this build wrote it reads (`fennec verify` in fennec's
/// `compat::`), fennec-core's `compat::` holding the answers. A replica's
/// file only compacted.
pub fn written_then_compacted(path: &Path, name: &str) {
    let mut db = fennec_core::fs::open(path).unwrap_or_else(|e| panic!("{name}: {e}"));
    if !db.history().following {
        write_after(&mut db);
    }
    run(&mut db, "compact");
}

/// The sync log's header names the file it was written beside by its
/// device and inode (`synclog.rs`), so a copy's log is never applied to
/// the copy -- by design. Stamped with the copy's, the log is its file's
/// as it was on the machine that went down. The header's hash is the
/// log's own (`synclog::hash`), a part of its format: a build that read
/// another would refuse the stamped log, and the answers would show it.
#[cfg(unix)]
pub fn stamp_log(file: &Path) {
    use std::os::unix::fs::MetadataExt;
    let log = PathBuf::from(format!("{}.sync", file.display()));
    let m = std::fs::metadata(file).unwrap();
    let mut b = std::fs::read(&log).unwrap();
    assert_eq!(&b[..8], b"FENECSYN", "{}", log.display());
    b[40..48].copy_from_slice(&m.dev().to_le_bytes());
    b[48..56].copy_from_slice(&m.ino().to_le_bytes());
    let h = log_hash(0, &b[..56]);
    b[56..64].copy_from_slice(&h.to_le_bytes());
    std::fs::write(&log, b).unwrap();
}

#[cfg(unix)]
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

/// `name`'s file copied into `dir` as it lies, the sync log beside it
/// stamped as its file's: what an open there is handed.
pub fn copied(set: &Path, name: &str, dir: &Path) -> PathBuf {
    let to = dir.join(format!("{name}.fennec"));
    std::fs::copy(set.join(format!("{name}.fennec")), &to).unwrap();
    let log = set.join(format!("{name}.fennec.sync"));
    if log.is_file() {
        std::fs::copy(&log, dir.join(format!("{name}.fennec.sync"))).unwrap();
        // Unstamped, the log names the file it was written beside, not this
        // copy, and is not applied: the copy reads as its own bytes alone.
        assert!(
            fennec_core::fs::read_with_sync_log(&to).unwrap().is_none(),
            "{name}: a log applied to a file it does not name"
        );
        // A header wiped -- spent by an open, or never begun -- names no
        // file, the release's nor a copy's: there is nothing to stamp.
        #[cfg(unix)]
        if std::fs::read(&log).unwrap()[..8] == *b"FENECSYN" {
            stamp_log(&to);
        }
    }
    to
}

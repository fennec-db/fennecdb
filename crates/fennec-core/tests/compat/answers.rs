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

/// Every set, oldest first: those under `FENNEC_COMPAT_SETS` where it names
/// a directory -- a set `tools/compat_fixtures.py write` has just written,
/// held to every test before it lands -- and the tree's otherwise.
pub fn sets() -> Vec<PathBuf> {
    let root =
        std::env::var_os("FENNEC_COMPAT_SETS").map_or_else(|| PathBuf::from(SETS), PathBuf::from);
    let mut v: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("{}: {e}", root.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    assert!(!v.is_empty(), "no fixture set under {}", root.display());
    v
}

pub fn set_name(set: &Path) -> String {
    set.file_name().unwrap().to_string_lossy().into_owned()
}

/// Whether the release wrote `name` torn, as a crash leaves a file
/// (`torn-*.fennec`): `fennec verify` refuses it as it lies, and an open
/// cuts the damage off.
pub fn torn(name: &str) -> bool {
    name.starts_with("torn")
}

/// A directory of a test's own, gone with it.
pub struct Scratch(pub PathBuf);
impl Scratch {
    pub fn new(tag: &str) -> Scratch {
        let d = std::env::temp_dir().join(format!("fennec-compat-{tag}-{}", std::process::id()));
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

/// The statements of `<name>.fennecql`, a line each.
pub fn statements(set: &Path, name: &str) -> Vec<String> {
    std::fs::read_to_string(set.join(format!("{name}.fennecql")))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("--"))
        .map(str::to_string)
        .collect()
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
            run(db, &format!("put {c} {{{}}}", doc.join(", ")));
            let new: Vec<i64> = ids(db, c).difference(&was).copied().collect();
            assert_eq!(new.len(), 1, "{c}: one row put, {new:?} new");
            let id = new[0];
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
/// written into it, read back and found through every index, opened
/// again, compacted and opened again; then taken out, and the release's
/// answers held again, compacted and opened once more; the file verified
/// at the end. A replica's file takes no write of its own, and is held to
/// that. The differences.
pub fn written_and_compacted(path: &Path, set: &Path, name: &str) -> Vec<String> {
    let open = |when: &str| {
        fennec_core::fs::open(path).unwrap_or_else(|e| panic!("{name}: an open {when}: {e}"))
    };
    let mut found = Vec::new();
    let mut db = open("to write");
    found.extend(differences(&db, set, name, "open"));
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
        eprintln!(
            "{name}: {} rows written, {} looks through an index",
            w.rows.len(),
            w.probes.len()
        );
        found.extend(read_back(&db, &w, name, "written"));
        drop(db);
        db = open("after the writes");
        found.extend(read_back(&db, &w, name, "written, opened again"));
        run(&mut db, "compact");
        drop(db);
        db = open("after the compact");
        found.extend(read_back(&db, &w, name, "written, compacted, opened again"));
        take_out(&mut db, &w);
        found.extend(differences(&db, set, name, "written and taken out"));
    }
    run(&mut db, "compact");
    drop(db);
    let db = open("after the last compact");
    found.extend(differences(&db, set, name, "compacted, opened again"));
    drop(db);
    verified(path, name, false);
    found
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
        #[cfg(unix)]
        stamp_log(&to);
    }
    to
}

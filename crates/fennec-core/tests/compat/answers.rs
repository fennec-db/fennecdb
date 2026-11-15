//! What every `compat::` test asks of a release's file, shared by
//! fennec-core's, fennec-http's and fennec's (`#[path]`): the sets, the
//! statements, this build's answers beside the release's, and the writes
//! after them.
//!
//! An answer that differs is a finding, never a fixture to write again: a
//! set is its release's, written once. A difference a change accepted on
//! purpose explains is named in `ACCEPTED`, with the change and why.

#![allow(dead_code)]

use fennec_core::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The sets, a directory a release, from any crate's tests.
pub const SETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../fennec-core/tests/compat");

/// Answers a later build gives otherwise, on purpose: (set, file,
/// statement, why). None so far.
pub const ACCEPTED: &[(&str, &str, &str, &str)] = &[];

/// Every set, oldest first.
pub fn sets() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(SETS)
        .expect("tests/compat")
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    assert!(!v.is_empty(), "no fixture set under {SETS}");
    v
}

pub fn set_name(set: &Path) -> String {
    set.file_name().unwrap().to_string_lossy().into_owned()
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

fn ids(db: &Database, collection: &str) -> BTreeSet<i64> {
    let text = format!("get {collection} select id");
    let stmt = fennec_ql::parse_for(db, &text).unwrap().remove(0);
    let r = db.query(&stmt, &[]).unwrap();
    r.rows()
        .unwrap()
        .rows
        .iter()
        .map(|row| match &row.values[0] {
            Value::Int(n) => *n,
            v => panic!("{collection}: id {v:?}"),
        })
        .collect()
}

/// This build's writes into the release's file: a collection of its own
/// written, then dropped, and a row put into every collection and taken
/// out again -- with whatever a statement bound to the put wrote -- so the
/// release's answers stand after them.
pub fn write_after(db: &mut Database) {
    let names: Vec<String> = db
        .collection_names()
        .into_iter()
        .filter(|n| !n.starts_with('_'))
        .collect();
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
    for n in &names {
        run(db, &format!("put {n} {{id: 1000000}}"));
    }
    for n in &names {
        for id in ids(db, n).difference(&before[n]) {
            run(db, &format!("del {n} where id = {id}"));
        }
        assert_eq!(ids(db, n), before[n], "{n}: the rows it held");
    }
    run(db, "drop collection compat_after");
}

/// The file at `path`, a release's `name`, held to its answers: opened to
/// write, written after, compacted and opened again. The differences.
pub fn written_and_compacted(path: &Path, set: &Path, name: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut db = fennec_core::fs::open(path).unwrap_or_else(|e| panic!("{name}: an open: {e}"));
    found.extend(differences(&db, set, name, "open"));
    write_after(&mut db);
    run(&mut db, "compact");
    drop(db);
    let db = fennec_core::fs::open(path)
        .unwrap_or_else(|e| panic!("{name}: an open after the compact: {e}"));
    found.extend(differences(
        &db,
        set,
        name,
        "written, compacted, opened again",
    ));
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

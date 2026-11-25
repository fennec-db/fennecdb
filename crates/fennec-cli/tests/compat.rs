//! `fennec verify` and `fennec restore`, this build's, over a release's
//! fixture set (fennec-core's `tests/compat/`): every database file the
//! release wrote verifies, and again once this build wrote rows of every
//! type into it and compacted it; its archives verify and restore, the
//! sealed one with its key alone, its sealed backup restores with its key
//! alone, and each database the binary restored answers as the release's
//! own restore of it did -- then is written, opened again, compacted and
//! answers alike, and verifies; `fennec verify` says of each archive what
//! the release's said, line for line, and its `fennec restore --to` of each
//! archive to every moment the release's was run to says what the
//! release's said and exits as it did -- refused before the image, the
//! image alone before the first write, each write's change after; and of
//! the node's archive of every tenant too, whose `fennec restore
//! --tenants` to each moment the release's was run to says what it said
//! and answers as it did. A file
//! the release wrote torn, as a crash leaves one, is refused as it lies,
//! naming its damage. fennec-core's
//! `compat::` holds the files themselves to their answers, fennec-http's
//! the archive's library, fennec-server's the server's.

#[path = "../../fennec-core/tests/compat/answers.rs"]
mod answers;

use answers::{
    copied, on_purpose, set_name, sets, torn, written_and_compacted, written_then_compacted,
    Scratch,
};
use fennec_core::prelude::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_fennec");

fn fennec(args: &[&Path]) -> (i32, String) {
    let out = Command::new(BIN).args(args).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().unwrap_or(-1), text)
}

fn member<'a>(v: &'a Value, name: &str) -> &'a Value {
    match v {
        Value::Object(o) => o.iter().find(|(k, _)| k == name).map(|(_, v)| v).unwrap(),
        _ => panic!("`{name}`: not an object"),
    }
}

fn texts(v: &Value) -> Vec<String> {
    match v {
        Value::List(l) => l
            .iter()
            .map(|x| match x {
                Value::Text(t) => t.clone(),
                x => panic!("not a text: {x:?}"),
            })
            .collect(),
        v => panic!("not a list: {v:?}"),
    }
}

/// The node's archive of every tenant: `fennec verify --tenants` saying
/// what the release's said of each line, and `fennec restore --tenants`
/// to the end and to each moment the release's was run to saying what it
/// said -- the line each tenant came from, passed over gone or not yet
/// archived, refused where `--keep` let the moment go -- each tenant it
/// restored answering as the release's restore of it did.
fn node_archive(set: &Path, name: &str, scratch: &Path) -> Vec<String> {
    let root = scratch.join("node-archive");
    copy_dir(&set.join("node-archive"), &root);
    let recorded = std::fs::read_to_string(set.join("node-archive.json")).unwrap();
    let recorded = fennec_core::json::parse_json(&recorded).unwrap();
    let mut found = Vec::new();
    let (code, out) = fennec(&[Path::new("verify"), Path::new("--tenants"), &root]);
    let ours: Vec<String> = out
        .replace(root.to_str().unwrap(), "<root>")
        .lines()
        .map(String::from)
        .collect();
    let theirs = texts(member(&recorded, "verify"));
    if code != 0 || ours != theirs {
        found.push(format!("{name}/node-archive: verify --tenants\n  release: {theirs:?}\n  this build: {code} {ours:?}"));
    }
    let statements = answers::statements(set, "node");
    let Value::List(restores) = member(&recorded, "restores") else {
        panic!("restores")
    };
    for (i, r) in restores.iter().enumerate() {
        let into = scratch.join(format!("node-{i}"));
        let mut args: Vec<&Path> = vec![Path::new("restore"), Path::new("--tenants"), &root, &into];
        let to;
        if let Value::Text(t) = member(r, "to") {
            to = t.clone();
            args.extend([Path::new("--to"), Path::new(&to)]);
        }
        let (code, out) = fennec(&args);
        let ours: Vec<String> = out
            .replace(into.to_str().unwrap(), "<out>")
            .lines()
            .map(String::from)
            .collect();
        let theirs = texts(member(r, "said"));
        let status = match member(r, "status") {
            Value::Int(n) => *n as i32,
            v => panic!("status: {v:?}"),
        };
        if code != status || ours != theirs {
            found.push(format!(
                "{name}/node-archive: restore --tenants {:?}\n  release: {status} {theirs:?}\n  this build: {code} {ours:?}",
                member(r, "to")
            ));
        }
        let Value::Object(answered) = member(r, "answers") else {
            panic!("answers")
        };
        for (tenant, theirs) in answered {
            let file = into.join(format!("{tenant}.fennec"));
            let Ok(db) = fennec_core::fs::open_read_only(&file) else {
                found.push(format!(
                    "{name}/node-archive: {tenant} not restored to {:?}",
                    member(r, "to")
                ));
                continue;
            };
            for (q, theirs) in statements.iter().zip(texts(theirs)) {
                let ours = answers::answer(&db, q);
                if ours != theirs {
                    found.push(format!(
                        "{name}/node-archive: {tenant} restored to {:?}: {q}\n  release: {theirs}\n  this build: {ours}",
                        member(r, "to")
                    ));
                }
            }
        }
    }
    found
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let path = e.path();
        match path.is_dir() {
            true => copy_dir(&path, &to.join(e.file_name())),
            false => {
                std::fs::copy(&path, to.join(e.file_name())).unwrap();
            }
        }
    }
}

#[test]
fn fennec_verifies_and_restores_every_release_s_files() {
    fennec_ql::install();
    let mut found = Vec::new();
    for set in sets() {
        let name = set_name(&set);
        let scratch = Scratch::new(&format!("cli-{name}"));
        // The set's key as `fennec key` left it, its owner's alone: a
        // checkout keeps no mode but the executable bit, and a key file
        // others may read is warned of on every command that reads it,
        // which the release's lines never held.
        let key = scratch.0.join("key");
        std::fs::copy(set.join("key"), &key).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let key_flag = Path::new("--key-file");

        // Every database file, the sync log's writes put back first: a
        // file the release wrote torn, as a crash leaves one, refused naming
        // its damage. Then each as this build wrote into it and compacted
        // it, verified whole; its answers are fennec-core's `compat::`.
        let mut stems: Vec<String> = std::fs::read_dir(&set)
            .unwrap()
            .filter_map(|e| {
                let file = e.unwrap().file_name().to_string_lossy().into_owned();
                Some(file.strip_suffix(".fennec")?.to_string())
            })
            .filter(|stem| !(cfg!(windows) && set.join(format!("{stem}.fennec.sync")).exists()))
            .collect();
        stems.sort();
        assert!(!stems.is_empty(), "{name}: no database file");
        // Each file on a thread of its own: its work is its own, most of
        // it a compact's fsync and the binary's start.
        std::thread::scope(|s| {
            let threads: Vec<_> = stems
                .iter()
                .map(|stem| {
                    let (set, scratch, name) = (&set, &scratch, &name);
                    let t = s.spawn(move || {
                        let file = format!("{stem}.fennec");
                        let dir = scratch.0.join(stem);
                        std::fs::create_dir_all(&dir).unwrap();
                        let path = copied(set, stem, &dir);
                        let (code, out) = fennec(&[Path::new("verify"), &path]);
                        match torn(stem) {
                            false => {
                                assert_eq!(code, 0, "{name}/{file}: {out}");
                                assert!(
                                    out.contains("records, each whole"),
                                    "{name}/{file}: {out}"
                                );
                            }
                            true => {
                                assert_ne!(code, 0, "{name}/{file}, torn, verified: {out}");
                                assert!(
                                    out.contains("runs past the end")
                                        || out.contains("fails its checksum"),
                                    "{name}/{file}: {out}"
                                );
                            }
                        }
                        // A file this build reads otherwise on purpose is
                        // fennec-core's to hold (`ON_PURPOSE`).
                        if on_purpose(stem) {
                            return;
                        }
                        written_then_compacted(&path, stem);
                        let (code, out) = fennec(&[Path::new("verify"), &path]);
                        assert_eq!(code, 0, "{name}/{file} written and compacted: {out}");
                    });
                    (stem, t)
                })
                .collect();
            for (stem, t) in threads {
                t.join()
                    .unwrap_or_else(|_| panic!("{name}/{stem}.fennec: failed, above"));
            }
        });

        // Each archive, copied: verify and restore read it alone, the
        // sealed one with its key and not without.
        for dirname in ["archive", "archive-plain"] {
            let arch = scratch.0.join(dirname);
            copy_dir(&set.join(dirname), &arch);
            let restored = scratch.0.join(format!("{dirname}.fennec"));
            let keyed: Vec<&Path> = match dirname {
                "archive" => {
                    let (code, out) = fennec(&[Path::new("verify"), &arch]);
                    assert_ne!(code, 0, "{name}/{dirname} verified with no key: {out}");
                    vec![key_flag, key.as_path()]
                }
                _ => Vec::new(),
            };
            let with_key = |head: &[&Path]| -> Vec<PathBuf> {
                head.iter().chain(&keyed).map(|p| p.to_path_buf()).collect()
            };
            let run =
                |args: Vec<PathBuf>| fennec(&args.iter().map(PathBuf::as_path).collect::<Vec<_>>());
            let (code, out) = run(with_key(&[Path::new("verify"), &arch]));
            assert_eq!(code, 0, "{name}/{dirname}: {out}");
            // What the release said of it, whole: its images and segments,
            // the changes it restores and the times they span.
            let theirs = std::fs::read_to_string(set.join(format!("{dirname}.verify"))).unwrap();
            let ours = out.replace(arch.to_str().unwrap(), &format!("<{dirname}>"));
            if ours != theirs {
                found.push(format!(
                    "{name}/{dirname}: fennec verify\n  release: {theirs:?}\n  this build: {ours:?}"
                ));
            }
            // To each moment the release's own restore was asked
            // (`<archive>.restores.json`): what it said, and its exit.
            let recorded =
                std::fs::read_to_string(set.join(format!("{dirname}.restores.json"))).unwrap();
            let recorded = fennec_core::json::parse_json(&recorded).unwrap();
            let Value::List(moments) = member(&recorded, "moments") else {
                panic!("{dirname}.restores.json: moments")
            };
            assert!(
                moments.len() > 2,
                "{name}/{dirname}: {} moments",
                moments.len()
            );
            for (i, m) in moments.iter().enumerate() {
                let Value::Text(to) = member(m, "to") else {
                    panic!("{dirname}.restores.json: to")
                };
                let status = match member(m, "status") {
                    Value::Int(n) => *n as i32,
                    v => panic!("status: {v:?}"),
                };
                let into = scratch.0.join(format!("{dirname}-to-{i}.fennec"));
                let (code, out) = run(with_key(&[
                    Path::new("restore"),
                    &arch,
                    &into,
                    Path::new("--to"),
                    Path::new(to),
                ]));
                let ours: Vec<String> = out
                    .replace(into.to_str().unwrap(), "<out>")
                    .lines()
                    .map(String::from)
                    .collect();
                let theirs = texts(member(m, "said"));
                if code != status || ours != theirs {
                    found.push(format!(
                        "{name}/{dirname}: restore --to {to}\n  release: {status} {theirs:?}\n  this build: {code} {ours:?}"
                    ));
                }
                let _ = std::fs::remove_file(&into);
            }
            let (code, out) = run(with_key(&[Path::new("restore"), &arch, &restored]));
            assert_eq!(code, 0, "{name}/{dirname}: {out}");
            found.extend(written_and_compacted(&restored, &set, dirname));
            let (code, out) = fennec(&[Path::new("verify"), &restored]);
            assert_eq!(code, 0, "{name}/{dirname} written and compacted: {out}");
        }

        // Each sealed backup: no key, no database.
        for stem in ["backup", "backup-wide"] {
            let restored = scratch.0.join(format!("{stem}.fennec"));
            let sealed = set.join(format!("{stem}.sealed"));
            let (code, out) = fennec(&[Path::new("restore"), &sealed, &restored]);
            assert_ne!(code, 0, "{name}/{stem}.sealed restored with no key: {out}");
            let (code, out) = fennec(&[Path::new("restore"), &sealed, &restored, key_flag, &key]);
            assert_eq!(code, 0, "{name}/{stem}.sealed: {out}");
            found.extend(written_and_compacted(&restored, &set, stem));
            let (code, out) = fennec(&[Path::new("verify"), &restored]);
            assert_eq!(code, 0, "{name}/{stem} written and compacted: {out}");
        }
        found.extend(node_archive(&set, &name, &scratch.0));
    }
    assert!(
        found.is_empty(),
        "{} answers differ:\n{}",
        found.len(),
        found.join("\n")
    );
}

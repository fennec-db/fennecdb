//! `fennec verify` and `fennec restore`, this build's, over a release's
//! fixture set (fennec-core's `tests/compat/`): every database file the
//! release wrote verifies, and again once this build wrote rows of every
//! type into it and compacted it; its archives verify and restore, the
//! sealed one with its key alone, its sealed backup restores with its key
//! alone, and each database the binary restored answers as the release's
//! own restore of it did -- then is written, opened again, compacted and
//! answers alike, and verifies. A file the release wrote torn, as a crash
//! leaves one, is refused as it lies, naming its damage. fennec-core's
//! `compat::` holds the files themselves to their answers, fennec-http's
//! the archive's library, fennec-server's the server's.

#[path = "../../fennec-core/tests/compat/answers.rs"]
mod answers;

use answers::{
    copied, set_name, sets, torn, written_and_compacted, written_then_compacted, Scratch,
};
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
        let key = set.join("key");
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
            assert!(out.contains("restores changes"), "{name}/{dirname}: {out}");
            let (code, out) = run(with_key(&[Path::new("restore"), &arch, &restored]));
            assert_eq!(code, 0, "{name}/{dirname}: {out}");
            found.extend(written_and_compacted(&restored, &set, dirname));
            let (code, out) = fennec(&[Path::new("verify"), &restored]);
            assert_eq!(code, 0, "{name}/{dirname} written and compacted: {out}");
        }

        // The sealed backup: no key, no database.
        let restored = scratch.0.join("backup.fennec");
        let sealed = set.join("backup.sealed");
        let (code, out) = fennec(&[Path::new("restore"), &sealed, &restored]);
        assert_ne!(code, 0, "{name}/backup.sealed restored with no key: {out}");
        let (code, out) = fennec(&[Path::new("restore"), &sealed, &restored, key_flag, &key]);
        assert_eq!(code, 0, "{name}/backup.sealed: {out}");
        found.extend(written_and_compacted(&restored, &set, "backup"));
        let (code, out) = fennec(&[Path::new("verify"), &restored]);
        assert_eq!(code, 0, "{name}/backup written and compacted: {out}");
    }
    assert!(
        found.is_empty(),
        "{} answers differ:\n{}",
        found.len(),
        found.join("\n")
    );
}

//! `fennec verify` and `fennec restore`, this build's, over a release's
//! fixture set (fennec-core's `tests/compat/`): every database file the
//! release wrote verifies, its sealed archive verifies and restores with
//! its key, its sealed backup restores, and each restored database answers
//! as the release's own restore of it did -- then is written, compacted,
//! opened again and answers alike. fennec-core's `compat::` holds the files
//! themselves to their answers, fennec-http's the archive's library.

#[path = "../../fennec-core/tests/compat/answers.rs"]
mod answers;

use answers::{copied, set_name, sets, written_and_compacted, Scratch};
use std::path::Path;
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

        // Every database file, the sync log's writes put back first.
        let mut verified = 0;
        for e in std::fs::read_dir(&set).unwrap() {
            let file = e.unwrap().file_name().to_string_lossy().into_owned();
            let Some(stem) = file.strip_suffix(".fennec") else {
                continue;
            };
            let dir = scratch.0.join(stem);
            std::fs::create_dir_all(&dir).unwrap();
            let path = copied(&set, stem, &dir);
            let (code, out) = fennec(&[Path::new("verify"), &path]);
            assert_eq!(code, 0, "{name}/{file}: {out}");
            assert!(out.contains("records, each whole"), "{name}/{file}: {out}");
            verified += 1;
        }
        assert!(verified > 0, "{name}: no database file");

        // The sealed archive, copied: verify and restore read it alone.
        let arch = scratch.0.join("archive");
        copy_dir(&set.join("archive"), &arch);
        let (code, out) = fennec(&[Path::new("verify"), &arch, key_flag, &key]);
        assert_eq!(code, 0, "{name}/archive: {out}");
        assert!(out.contains("restores changes"), "{name}/archive: {out}");
        let restored = scratch.0.join("archive.fennec");
        let (code, out) = fennec(&[Path::new("restore"), &arch, &restored, key_flag, &key]);
        assert_eq!(code, 0, "{name}/archive: {out}");
        found.extend(written_and_compacted(&restored, &set, "archive"));

        // The sealed backup: no key, no database.
        let restored = scratch.0.join("backup.fennec");
        let sealed = set.join("backup.sealed");
        let (code, out) = fennec(&[Path::new("restore"), &sealed, &restored]);
        assert_ne!(code, 0, "{name}/backup.sealed restored with no key: {out}");
        let (code, out) = fennec(&[Path::new("restore"), &sealed, &restored, key_flag, &key]);
        assert_eq!(code, 0, "{name}/backup.sealed: {out}");
        found.extend(written_and_compacted(&restored, &set, "backup"));
    }
    assert!(
        found.is_empty(),
        "{} answers differ:\n{}",
        found.len(),
        found.join("\n")
    );
}

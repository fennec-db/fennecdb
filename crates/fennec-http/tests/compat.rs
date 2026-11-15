//! A release's sealed backup and sealed archive (fennec-core's
//! `tests/compat/`, written by that release's `fennec backup --key-file`
//! and `fennec archive --key-file`): each opens with its key in this
//! build, the archive verified as `fennec verify` reads it, and the
//! database each restores to answers as the release's own restore of it
//! did -- then is written into, compacted, opened again and answers
//! alike, every record held to its sum once more.
//! Seals by their own magic, never this build's constant: a seal of a
//! later version beside the release's changes nothing here.

#[path = "../../fennec-core/tests/compat/answers.rs"]
mod answers;

use answers::{set_name, sets, written_and_compacted, Scratch};
use fennec_http::archive::{self, Archive, Target};
use fennec_http::seal::Key;
use std::path::Path;

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
fn a_release_s_sealed_backup_and_archive_restore_and_answer_as_it_did() {
    fennec_ql::install();
    let mut found = Vec::new();
    for set in sets() {
        let name = set_name(&set);
        let scratch = Scratch::new(&format!("sealed-{name}"));
        let key = Key::from_file(&set.join("key")).unwrap();
        let other = scratch.0.join("other.key");
        std::fs::write(&other, format!("{}\n", "5a".repeat(32))).unwrap();
        let other = Key::from_file(&other).unwrap();

        // The backup: a sealed file, the database it holds.
        let sealed = set.join("backup.sealed");
        assert!(
            archive::unseal(&sealed, &other, &scratch.0.join("refused.fennec")).is_err(),
            "{name}: a sealed backup opened with another key"
        );
        let out = scratch.0.join("backup.fennec");
        let seq = archive::unseal(&sealed, &key, &out)
            .unwrap_or_else(|e| panic!("{name}/backup.sealed: {e}"));
        assert!(seq > 0, "{name}: the backup holds no change");
        found.extend(written_and_compacted(&out, &set, "backup"));

        // The archive, copied: a restore or a verify reads it alone.
        let dir = scratch.0.join("archive");
        copy_dir(&set.join("archive"), &dir);
        let arch = Archive::with_key(&dir, Some(key.clone())).unwrap();
        let v = arch
            .verify()
            .unwrap_or_else(|e| panic!("{name}/archive: {e}"));
        assert!(
            v.images >= 1 && v.segments >= 1,
            "{name}/archive: {} images, {} segments",
            v.images,
            v.segments
        );
        assert!(v.gaps.is_empty(), "{name}/archive: gaps {:?}", v.gaps);
        let out = scratch.0.join("archive.fennec");
        let r = arch
            .restore(&out, Target::End)
            .unwrap_or_else(|e| panic!("{name}/archive: a restore: {e}"));
        assert_eq!(r.seq, v.last, "{name}/archive: restored to its end");
        found.extend(written_and_compacted(&out, &set, "archive"));
    }
    assert!(
        found.is_empty(),
        "{} answers differ:\n{}",
        found.len(),
        found.join("\n")
    );
}

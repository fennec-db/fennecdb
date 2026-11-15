//! A release's sealed backup and its archives, sealed and plain
//! (fennec-core's `tests/compat/`, written by that release's `fennec
//! backup --key-file` and `fennec archive`): each opens in this build --
//! the sealed ones with their key, no other -- the archives verified as
//! `fennec verify` reads them, and the database each restores to answers
//! as the release's own restore of it did, then is written into, opened
//! again, compacted and answers alike, every record held to its sum once
//! more. And every byte of each is walked by the release's layout
//! (`compat/walk.rs`): a seal's frames, an archive's names, images,
//! segments and history, the database inside each, holding the set to the
//! seals' and archives' alternatives the release writes, both ways.
//! Seals by their own magic, never this build's constant: a seal of a
//! later version beside the release's changes nothing here.

#[path = "../../fennec-core/tests/compat/answers.rs"]
mod answers;
#[path = "../../fennec-core/tests/compat/walk.rs"]
mod walk;

use answers::{set_name, sets, written_and_compacted, Scratch};
use fennec_http::archive::{self, Archive, Target};
use fennec_http::seal::{self, Key};
use std::collections::BTreeSet;
use std::path::Path;
use walk::{sealed_only, writes_of, Seen};

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

/// A file sealed whole: its frames walked to its end, then opened by this
/// build's seal with the set's key. Its plaintext.
fn sealed_whole(bytes: &[u8], file: &str, key: &Key, seen: &mut Seen) -> Vec<u8> {
    let frames = walk::seal_frames(bytes, file);
    seen.shape(match frames {
        1 => "seal: whole, one frame",
        _ => "seal: whole, several frames",
    });
    seal::open_whole(key, bytes).unwrap_or_else(|e| panic!("{file}: {e}"))
}

/// Every file of an archive by its name -- `history`, each base image
/// `image-<change>-<time>.fennec`, each segment `writes-<first>.log`, no
/// other -- each sealed or each plain: the images walked as databases,
/// then the segments in order, each write's time and record, then the
/// history.
fn walk_archive(dir: &Path, label: &str, key: &Key, seen: &mut Seen) {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    let mut kinds = BTreeSet::new();
    let (mut images, mut segments, mut history) = (Vec::new(), Vec::new(), None);
    for name in &names {
        let bytes = std::fs::read(dir.join(name)).unwrap();
        kinds.insert(seal::is_sealed(&bytes));
        if let Some((change, time)) = name
            .strip_prefix("image-")
            .and_then(|r| r.strip_suffix(".fennec"))
            .and_then(|r| r.split_once('-'))
        {
            assert!(
                digits(change, 20) && digits(time, 13),
                "{label}/{name}: an image's name"
            );
            images.push((name.clone(), bytes));
        } else if let Some(first) = name
            .strip_prefix("writes-")
            .and_then(|r| r.strip_suffix(".log"))
        {
            assert!(digits(first, 20), "{label}/{name}: a segment's name");
            segments.push((name.clone(), bytes));
        } else if name == "history" {
            history = Some(bytes);
        } else {
            panic!("{label}/{name}: a file no archive holds");
        }
    }
    assert_eq!(kinds.len(), 1, "{label}: sealed and plain files mixed");
    let sealed = kinds.contains(&true);
    seen.shape(if sealed {
        "archive: sealed"
    } else {
        "archive: plain"
    });
    assert!(
        !images.is_empty() && !segments.is_empty(),
        "{label}: an image and a segment"
    );
    for (name, bytes) in &images {
        let file = format!("{label}/{name}");
        let plain = match sealed {
            true => sealed_whole(bytes, &file, key, seen),
            false => bytes.clone(),
        };
        walk::database(&plain, &file, seen);
        seen.shape("archive: image");
    }
    for (name, bytes) in &segments {
        let file = format!("{label}/{name}");
        let plain = match sealed {
            true => {
                walk::seal_frames(bytes, &file);
                let (plain, end, frames) =
                    seal::open_appended(key, bytes).unwrap_or_else(|e| panic!("{file}: {e}"));
                assert_eq!(end, bytes.len(), "{file}: a frame cut short");
                assert!(frames > 1, "{file}: {frames} frames");
                seen.shape("seal: appended frames");
                plain
            }
            false => bytes.clone(),
        };
        walk::segment(&plain, &file, seen);
        seen.shape("archive: segment");
    }
    let history = history.unwrap_or_else(|| panic!("{label}: no history"));
    let file = format!("{label}/history");
    let plain = match sealed {
        true => sealed_whole(&history, &file, key, seen),
        false => history,
    };
    let mut c = walk::Cur::new(&file, &plain, 0);
    walk::history(&mut c, seen);
    c.end("the archive's history");
    seen.shape("archive: history file");
}

#[test]
fn a_release_s_backup_and_archives_restore_and_answer_as_it_did() {
    fennec_ql::install();
    let mut found = Vec::new();
    for set in sets() {
        let name = set_name(&set);
        let scratch = Scratch::new(&format!("sealed-{name}"));
        let key = Key::from_file(&set.join("key")).unwrap();
        let other = scratch.0.join("other.key");
        std::fs::write(&other, format!("{}\n", "5a".repeat(32))).unwrap();
        let other = Key::from_file(&other).unwrap();
        let mut seen = Seen::default();

        // The backup: a sealed file, the database it holds.
        let sealed = set.join("backup.sealed");
        let plain = sealed_whole(
            &std::fs::read(&sealed).unwrap(),
            &format!("{name}/backup.sealed"),
            &key,
            &mut seen,
        );
        walk::database(&plain, &format!("{name}/backup.sealed"), &mut seen);
        assert!(
            archive::unseal(&sealed, &other, &scratch.0.join("refused.fennec")).is_err(),
            "{name}: a sealed backup opened with another key"
        );
        let out = scratch.0.join("backup.fennec");
        let seq = archive::unseal(&sealed, &key, &out)
            .unwrap_or_else(|e| panic!("{name}/backup.sealed: {e}"));
        assert!(seq > 0, "{name}: the backup holds no change");
        found.extend(written_and_compacted(&out, &set, "backup"));

        // Each archive, copied: a restore or a verify reads it alone.
        let mut archives = 0;
        for entry in std::fs::read_dir(&set).unwrap() {
            let dirname = entry.unwrap().file_name().to_string_lossy().into_owned();
            if !set.join(&dirname).is_dir() {
                continue;
            }
            archives += 1;
            walk_archive(
                &set.join(&dirname),
                &format!("{name}/{dirname}"),
                &key,
                &mut seen,
            );
            let dir = scratch.0.join(&dirname);
            copy_dir(&set.join(&dirname), &dir);
            let sealed = seal::is_sealed(&std::fs::read(dir.join("history")).unwrap());
            if sealed {
                let refused = Archive::with_key(&dir, Some(other.clone())).and_then(|a| a.verify());
                assert!(
                    refused.is_err(),
                    "{name}/{dirname}: verified with another key"
                );
            }
            let arch = Archive::with_key(&dir, sealed.then(|| key.clone())).unwrap();
            let v = arch
                .verify()
                .unwrap_or_else(|e| panic!("{name}/{dirname}: {e}"));
            assert!(
                v.images >= 1 && v.segments >= 1,
                "{name}/{dirname}: {} images, {} segments",
                v.images,
                v.segments
            );
            assert!(v.gaps.is_empty(), "{name}/{dirname}: gaps {:?}", v.gaps);
            let out = scratch.0.join(format!("{dirname}.fennec"));
            let r = arch
                .restore(&out, Target::End)
                .unwrap_or_else(|e| panic!("{name}/{dirname}: a restore: {e}"));
            assert_eq!(r.seq, v.last, "{name}/{dirname}: restored to its end");
            found.extend(written_and_compacted(&out, &set, &dirname));
        }
        assert!(
            archives >= 2,
            "{name}: {archives} archives, not a sealed one and a plain one"
        );

        // What only these files hold, both ways; the rest in fennec-core's
        // walk, and nothing here a release does not write.
        let writes = writes_of(&name);
        let want: BTreeSet<&str> = writes
            .shapes
            .iter()
            .copied()
            .filter(|s| sealed_only(s))
            .collect();
        let got: BTreeSet<&str> = seen
            .shapes
            .iter()
            .copied()
            .filter(|s| sealed_only(s))
            .collect();
        assert_eq!(got, want, "{name}: the seals' and archives' alternatives");
        let all: BTreeSet<&str> = writes.shapes.iter().copied().collect();
        let extra: Vec<&&str> = seen.shapes.iter().filter(|s| !all.contains(**s)).collect();
        assert!(
            extra.is_empty(),
            "{name}: {extra:?} in a file and not in its RELEASES line"
        );
        for (what, want, got) in [
            ("record kinds", writes.kinds, &seen.kinds),
            ("schema tags", writes.schema_tags, &seen.schema_tags),
            ("value tags", writes.value_tags, &seen.value_tags),
            ("index kinds", writes.indexes, &seen.indexes),
            ("statement ops", writes.ops, &seen.ops),
        ] {
            let extra: Vec<&u8> = got.iter().filter(|k| !want.contains(k)).collect();
            assert!(
                extra.is_empty(),
                "{name}: {what} {extra:?} not in its RELEASES line"
            );
        }
    }
    assert!(
        found.is_empty(),
        "{} answers differ:\n{}",
        found.len(),
        found.join("\n")
    );
}

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
//! seals' and archives' alternatives the release writes, both ways. A
//! restore to each moment the release's own `fennec restore --to` was asked
//! -- before the image, its time, the image alone before the first write,
//! each write's time and the moment before the next, which the walk's
//! times hold the record to covering -- stops where the release's did,
//! saying what it said, and answers as its database did; a file sealed
//! whole in several frames, cut at a frame's end, is refused; and the
//! node's archive -- a line a tenant, one moved aside, `began` and
//! `presence` -- restores its tenants as the release's restore did.
//! A file is told sealed by the release's own magic, `FENECSL\x01`,
//! never by this build's test of it (`seal::is_sealed`), which is only
//! held to agree: a seal of a later version beside the release's changes
//! nothing here.

#[path = "../../fennec-core/tests/compat/answers.rs"]
mod answers;
#[path = "../../fennec-core/tests/compat/walk.rs"]
mod walk;

use answers::{set_name, sets, written_and_compacted, Scratch};
use fennec_core::prelude::Value;
use fennec_http::archive::{self, Archive, Target};
use fennec_http::node_archive::{self, Restoring};
use fennec_http::seal::{self, Key};
use std::collections::BTreeSet;
use std::path::Path;
use walk::{sealed_only, writes_of, Seen};

/// Whether `bytes` are sealed by the release: version 1's magic, the one
/// v0.2.0 writes. A file a release seals by another is its set's to name.
fn release_sealed(bytes: &[u8]) -> bool {
    bytes.starts_with(b"FENECSL\x01")
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

/// A file sealed whole: its frames walked to its end, then opened by this
/// build's seal with the set's key. Its plaintext. Of several frames, the
/// file cut at the end of each but the last is refused: the last is
/// flagged as the last in what its tag covers, so a file cut where a frame
/// ends is told from a whole one.
fn sealed_whole(bytes: &[u8], file: &str, key: &Key, seen: &mut Seen) -> Vec<u8> {
    let ends = walk::seal_frames(bytes, file);
    seen.shape(match ends.len() {
        1 => "seal: whole, one frame",
        _ => "seal: whole, several frames",
    });
    for &end in &ends[..ends.len() - 1] {
        assert!(
            seal::open_whole(key, &bytes[..end]).is_err(),
            "{file}: opened cut at a frame's end, {end} of {} bytes",
            bytes.len()
        );
    }
    seal::open_whole(key, bytes).unwrap_or_else(|e| panic!("{file}: {e}"))
}

/// Every file of an archive by its name -- `history`, each base image
/// `image-<change>-<time>.fennec`, each segment `writes-<first>.log`, no
/// other -- each sealed or each plain: the images walked as databases,
/// then the segments in order, each write's time and record, then the
/// history.
fn walk_archive(dir: &Path, label: &str, key: &Key, seen: &mut Seen) -> Vec<(u64, u64, u64)> {
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
        // A tenant's line of a node's archive: when its first image was
        // taken, and the node's word that the tenant was gone or back,
        // plain whether the archive is sealed or not.
        if name == "began" {
            let text = String::from_utf8(bytes).unwrap();
            assert!(
                text.strip_suffix('\n')
                    .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())),
                "{label}/began: {text:?}"
            );
            seen.shape("node archive: began");
            continue;
        }
        if name == "presence" {
            let text = String::from_utf8(bytes).unwrap();
            for line in text.lines() {
                let (word, at) = line
                    .split_once(' ')
                    .unwrap_or_else(|| panic!("{label}/presence: {line:?}"));
                assert!(
                    !at.is_empty() && at.bytes().all(|b| b.is_ascii_digit()),
                    "{label}/presence: {line:?}"
                );
                seen.shape(match word {
                    "gone" => "node archive: presence gone",
                    "back" => "node archive: presence back",
                    _ => panic!("{label}/presence: {line:?}"),
                });
            }
            assert!(text.ends_with('\n'), "{label}/presence: {text:?}");
            continue;
        }
        assert_eq!(
            seal::is_sealed(&bytes),
            release_sealed(&bytes),
            "{label}/{name}: this build's test of a seal and the release's magic disagree"
        );
        kinds.insert(release_sealed(&bytes));
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
        seen.finish();
        seen.shape("archive: image");
    }
    // Each write's time and the change it ends at, segment by segment.
    let mut writes = Vec::new();
    for (name, bytes) in &segments {
        let file = format!("{label}/{name}");
        let first: u64 = name
            .strip_prefix("writes-")
            .and_then(|r| r.strip_suffix(".log"))
            .unwrap()
            .parse()
            .unwrap();
        let plain = match sealed {
            true => {
                walk::seal_frames(bytes, &file);
                let (plain, end, frames, _) =
                    seal::open_appended(key, bytes).unwrap_or_else(|e| panic!("{file}: {e}"));
                assert_eq!(end, bytes.len(), "{file}: a frame cut short");
                assert!(frames > 1, "{file}: {frames} frames");
                seen.shape("seal: appended frames");
                plain
            }
            false => bytes.clone(),
        };
        let mut seq = first - 1;
        for (time, n) in walk::segment(&plain, &file, seen) {
            seq += n;
            writes.push((time, seq, n));
        }
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
    writes
}

/// What a restore of every tenant said of each, as the release's `fennec
/// restore --tenants` prints it.
fn said(name: &str, r: &Restoring) -> String {
    let iso = |t: u64| fennec_core::time::format_iso(t as i64);
    match r {
        Restoring::Restored(seq, from) if from == name => format!("{name}: change {seq}"),
        Restoring::Restored(seq, from) => format!("{name}: change {seq}, from {from}"),
        Restoring::Gone(t) => format!("{name}: gone from the node {}, passed over", iso(*t)),
        Restoring::NotYet => format!("{name}: archived only from after then, passed over"),
        Restoring::LetGo(first) => format!(
            "{name}: that moment was let go of (--keep): its archive holds nothing from before {}",
            iso(*first)
        ),
        Restoring::Failed(e) => format!("{name}: {e}"),
    }
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

/// The release's restores of `<dirname>` to a moment, as the writer
/// recorded them, and each moment.
fn restores(set: &Path, dirname: &str) -> (Value, Vec<u64>) {
    let text = std::fs::read_to_string(set.join(format!("{dirname}.restores.json"))).unwrap();
    let record = fennec_core::json::parse_json(&text).unwrap();
    let moments = match member(&record, "moments") {
        Value::List(l) => l
            .iter()
            .map(|m| match member(m, "at") {
                Value::Int(at) => *at as u64,
                v => panic!("{dirname}.restores.json: at {v:?}"),
            })
            .collect(),
        v => panic!("{dirname}.restores.json: moments {v:?}"),
    };
    (record, moments)
}

/// What a restore of an archive to a moment says, as the release's `fennec
/// restore --to` prints it: the change, the image and the last write's
/// time.
fn restore_said(r: &archive::Restored) -> String {
    let when = r.time.map_or(String::new(), |t| {
        format!(
            ", the last written {}",
            fennec_core::time::format_iso(t as i64)
        )
    });
    format!(
        "<out>: change {} -- image {} and the writes after it{when}",
        r.seq, r.image
    )
}

/// The archive restored by this build to each moment the release's own
/// restore was asked: refused where the release's was, in its words,
/// otherwise stopping at its change and saying what it said, the database
/// answering `restore.fennecql` as the release's restore of that change
/// did. What differs.
fn restored_at_moments(
    arch: &Archive,
    set: &Path,
    dirname: &str,
    record: &Value,
    scratch: &Path,
) -> Vec<String> {
    let name = set_name(set);
    let statements = answers::statements(set, "restore");
    let mut found = Vec::new();
    let Value::List(moments) = member(record, "moments") else {
        panic!("moments")
    };
    for m in moments {
        let at = match member(m, "at") {
            Value::Int(at) => *at as u64,
            v => panic!("at {v:?}"),
        };
        let status = match member(m, "status") {
            Value::Int(s) => *s,
            v => panic!("status {v:?}"),
        };
        let said = texts(member(m, "said"));
        let out = scratch.join(format!("{dirname}-to-{at}.fennec"));
        match (arch.restore(&out, Target::Time(at)), status) {
            (Ok(r), 0) => {
                let ours = restore_said(&r);
                if said != [ours.clone()] {
                    found.push(format!(
                        "{name}/{dirname}: a restore to {at}\n  release: {said:?}\n  this build: {ours:?}"
                    ));
                    continue;
                }
                let theirs = texts(member(member(record, "answers"), &r.seq.to_string()));
                let db = fennec_core::fs::open_read_only(&out).unwrap();
                for (q, theirs) in statements.iter().zip(theirs) {
                    let ours = answers::answer(&db, q);
                    if ours != theirs {
                        found.push(format!(
                            "{name}/{dirname} restored to {at}: {q}\n  release: {theirs}\n  this build: {ours}"
                        ));
                    }
                }
            }
            (Err(e), s) if s != 0 && said.iter().any(|l| l.contains(&e.to_string())) => {}
            (got, _) => found.push(format!(
                "{name}/{dirname}: a restore to {at}\n  release: exit {status} {said:?}\n  this build: {:?}",
                got.map(|r| restore_said(&r)).map_err(|e| e.to_string())
            )),
        }
        let _ = std::fs::remove_file(&out);
    }
    found
}

/// The node's archive: every line walked as an archive, then every tenant
/// restored at each moment the release restored it to, each saying what
/// the release's restore said of it, and each restored answering as the
/// release's did.
fn node_archive(set: &Path, name: &str, scratch: &Path, key: &Key, seen: &mut Seen) -> Vec<String> {
    let root = set.join("node-archive");
    let mut lines = 0;
    for e in std::fs::read_dir(&root).unwrap() {
        let label = e.unwrap().file_name().to_string_lossy().into_owned();
        let moved = match label.split_once('.') {
            None => false,
            Some((_, hex)) => {
                assert!(
                    hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
                    "{name}/node-archive/{label}"
                );
                true
            }
        };
        seen.shape(if moved {
            "node archive: line moved aside"
        } else {
            "node archive: line followed"
        });
        walk_archive(
            &root.join(&label),
            &format!("{name}/node-archive/{label}"),
            key,
            seen,
        );
        lines += 1;
    }
    assert!(lines >= 3, "{name}/node-archive: {lines} lines");
    let recorded = std::fs::read_to_string(set.join("node-archive.json")).unwrap();
    let recorded = fennec_core::json::parse_json(&recorded).unwrap();
    let statements = answers::statements(set, "node");
    let mut found = Vec::new();
    let restores = match member(&recorded, "restores") {
        Value::List(l) => l.clone(),
        _ => panic!("restores"),
    };
    for (i, r) in restores.iter().enumerate() {
        let to = match member(r, "to") {
            Value::Text(t) => Target::Time(fennec_core::time::parse(t).unwrap() as u64),
            _ => Target::End,
        };
        let out = scratch.join(format!("node-{i}"));
        let done = node_archive::restore_all(&root, &out, to, None).unwrap();
        let ours: Vec<String> = done.iter().map(|(n, r)| said(n, r)).collect();
        let theirs: Vec<String> = texts(member(r, "said"))
            .into_iter()
            .filter(|l| !l.starts_with("<out>"))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let ours_sorted: Vec<String> = ours
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if ours_sorted != theirs {
            found.push(format!("{name}/node-archive to {to:?}\n  release: {theirs:?}\n  this build: {ours_sorted:?}"));
        }
        let answered = match member(r, "answers") {
            Value::Object(o) => o.clone(),
            _ => panic!("answers"),
        };
        for (tenant, theirs) in &answered {
            let file = out.join(format!("{tenant}.fennec"));
            let db =
                fennec_core::fs::open_read_only(&file).unwrap_or_else(|e| panic!("{tenant}: {e}"));
            for (q, theirs) in statements.iter().zip(texts(theirs)) {
                let ours = answers::answer(&db, q);
                if ours != theirs {
                    found.push(format!("{name}/node-archive {tenant} to {to:?}: {q}\n  release: {theirs}\n  this build: {ours}"));
                }
            }
        }
    }
    found
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

        // Each backup: a sealed file, the database it holds.
        let mut backups = 0;
        for entry in std::fs::read_dir(&set).unwrap() {
            let file = entry.unwrap().file_name().to_string_lossy().into_owned();
            let Some(stem) = file.strip_suffix(".sealed") else {
                continue;
            };
            let sealed = set.join(&file);
            let bytes = std::fs::read(&sealed).unwrap();
            assert!(
                release_sealed(&bytes),
                "{name}/{file}: not sealed by the release's version"
            );
            let plain = sealed_whole(&bytes, &format!("{name}/{file}"), &key, &mut seen);
            walk::database(&plain, &format!("{name}/{file}"), &mut seen);
            seen.finish();
            assert!(
                archive::unseal(&sealed, &other, &scratch.0.join("refused.fennec")).is_err(),
                "{name}/{file}: a sealed backup opened with another key"
            );
            let out = scratch.0.join(format!("{stem}.fennec"));
            let seq = archive::unseal(&sealed, &key, &out)
                .unwrap_or_else(|e| panic!("{name}/{file}: {e}"));
            assert!(seq > 0, "{name}/{file}: the backup holds no change");
            found.extend(written_and_compacted(&out, &set, stem));
            backups += 1;
        }
        assert!(backups >= 2, "{name}: {backups} sealed backups");

        // Each archive, copied: a restore or a verify reads it alone.
        let mut archives = 0;
        let mut timed = 0;
        for entry in std::fs::read_dir(&set).unwrap() {
            let dirname = entry.unwrap().file_name().to_string_lossy().into_owned();
            if !set.join(&dirname).is_dir() || dirname == "node-archive" {
                continue;
            }
            archives += 1;
            let writes = walk_archive(
                &set.join(&dirname),
                &format!("{name}/{dirname}"),
                &key,
                &mut seen,
            );
            let dir = scratch.0.join(&dirname);
            copy_dir(&set.join(&dirname), &dir);
            let sealed = release_sealed(&std::fs::read(dir.join("history")).unwrap());
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
            // A restore to each moment the release's own restore was
            // asked (`<archive>.restores.json`): where it stopped, what it
            // said and what the database it restored answered.
            let (record, moments) = restores(&set, &dirname);
            for (i, &(time, _, _)) in writes.iter().enumerate() {
                let next = writes.get(i + 1).map(|w| w.0);
                for at in [Some(time), next.filter(|&n| n > time + 1).map(|n| n - 1)]
                    .into_iter()
                    .flatten()
                {
                    assert!(
                        moments.contains(&at),
                        "{name}/{dirname}: no restore of the release's to {at}, a write's time or the \
                         moment before the next"
                    );
                }
            }
            let first = writes.first().map(|w| w.0).unwrap();
            assert!(
                moments.iter().any(|&at| at >= v.from && at < first),
                "{name}/{dirname}: no restore to its image alone, before its first write"
            );
            found.extend(restored_at_moments(
                &arch, &set, &dirname, &record, &scratch.0,
            ));
            timed += moments.len();
        }
        assert!(timed > 2, "{name}: {timed} restores to a time");
        found.extend(node_archive(&set, &name, &scratch.0, &key, &mut seen));
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

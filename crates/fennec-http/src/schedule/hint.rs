//! A tenant node's hint of the tenants holding a schedule
//! (`docs/design/named-statements.md`, *Where a run happens, and once*):
//! a file beside the tenants' -- `schedules.hint`, no tenant's, a tenant's
//! file being `<name>.fennec` -- naming each tenant that holds one, and those
//! alone, with its next slot and the marks of its runs that wrote nothing
//! as it closed, so a start opens no tenant before its slot and none that
//! holds no schedule, and a closed tenant's empty runs are not made again
//! at each open.
//!
//! **Derived, never the truth**, as the HNSW graph is: the slots are
//! computed again from a tenant's file as the scheduler opens it, so a
//! hint too early opens a tenant to find nothing due, and one naming a
//! tenant that holds none opens it once to learn so. What it must never do
//! is miss one that holds one: a tenant is entered, synced, before the
//! block bringing its first schedule lands -- a line appended and fsynced,
//! one line whatever the node holds -- while the whole is written again by
//! rename at most once a minute ([`write`]).
//!
//! One line a tenant, its fields apart by a space:
//!
//! ```text
//! fennec schedule hint 1
//! <tenant> <next | -> [<change>/<history>/<forks> <mark>...]
//! ```
//!
//! `next` in epoch milliseconds, `-` for a tenant to look at once -- one
//! entered and not looked at since, or one due; then, for a tenant closed
//! with marks pending, where its file stood -- its change, its history's
//! id in hex and how many forks it holds -- and each mark,
//! `<statement>,<slot>,<version's history>,<version's change>,<skipped>,
//! <status>,<reason in hex>`, a status of 0 a run that ran. A line is the
//! tenant's latest where it is named twice: an entry appended after the
//! whole was written. An appended line cut short by a crash -- no newline
//! -- was a definition that never landed, and is passed over; a file that
//! does not read otherwise is no hint, and the node learns one again.

use fennec_core::engine::SlotMark;
use fennec_core::prelude::*;
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The hint's file, in a node's directory.
pub const FILE: &str = "schedules.hint";
const HEAD: &str = "fennec schedule hint 1";

/// Where a tenant's file stood: its change, its history's id and how many
/// forks its lineage holds. A pending mark kept outside the database counts
/// only while its file stands there still, so it never stands for a run
/// over data that changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stood {
    pub change: u64,
    pub history: u64,
    pub forks: usize,
}

impl Stood {
    pub fn of(db: &Database) -> Stood {
        let h = db.history();
        Stood {
            change: db.change_seq(),
            history: h.current(),
            forks: h.lineage.len(),
        }
    }
}

/// A closed tenant's marks of runs that wrote nothing, and where its file
/// stood as they were taken.
#[derive(Debug, Clone, PartialEq)]
pub struct Kept {
    pub stood: Stood,
    pub marks: Vec<(String, SlotMark)>,
}

/// A tenant as the hint names it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Line {
    /// Its next slot; `None` to look at once.
    pub next: Option<i64>,
    pub kept: Option<Kept>,
}

fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

fn invalid(why: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

/// The hint in `dir`: `None` where there is none, an error where it does
/// not read -- each a node that learns one again.
pub fn read(dir: &Path) -> io::Result<Option<HashMap<String, Line>>> {
    let bytes = match std::fs::read(path(dir)) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let text = String::from_utf8(bytes).map_err(|_| invalid("not UTF-8".into()))?;
    // An append a crash cut short has no newline: a definition that never
    // landed, its tenant named by no line it needs.
    let whole = match text.rfind('\n') {
        Some(end) => &text[..end],
        None => return Err(invalid("no line whole".into())),
    };
    let mut lines = whole.split('\n');
    if lines.next() != Some(HEAD) {
        return Err(invalid(format!("its first line is not `{HEAD}`")));
    }
    let mut out = HashMap::new();
    for (i, l) in lines.enumerate() {
        let (name, line) =
            parse(l).ok_or_else(|| invalid(format!("line {} does not read: {l:?}", i + 2)))?;
        out.insert(name, line);
    }
    Ok(Some(out))
}

fn parse(l: &str) -> Option<(String, Line)> {
    let mut f = l.split(' ');
    let name = f.next()?;
    crate::tenants::check_name(name).ok()?;
    let next = match f.next()? {
        "-" => None,
        t => Some(t.parse().ok()?),
    };
    let kept = match f.next() {
        None => None,
        Some(stood) => {
            let mut s = stood.split('/');
            let stood = Stood {
                change: s.next()?.parse().ok()?,
                history: u64::from_str_radix(s.next()?, 16).ok()?,
                forks: s.next()?.parse().ok()?,
            };
            let marks = f.map(mark).collect::<Option<Vec<_>>>()?;
            Some(Kept { stood, marks })
        }
    };
    Some((name.to_string(), Line { next, kept }))
}

fn mark(m: &str) -> Option<(String, SlotMark)> {
    let mut f = m.split(',');
    let name = f.next()?;
    if !fennec_core::named::valid_name(name) {
        return None;
    }
    let at = f.next()?.parse().ok()?;
    let version = (
        u64::from_str_radix(f.next()?, 16).ok()?,
        f.next()?.parse().ok()?,
    );
    let skipped = f.next()?.parse().ok()?;
    let status: u16 = f.next()?.parse().ok()?;
    let reason = unhex(f.next()?)?;
    let refused = (status != 0).then_some((status, reason));
    if f.next().is_some() {
        return None;
    }
    let mark = SlotMark {
        at,
        version,
        refused,
        skipped,
    };
    Some((name.to_string(), mark))
}

fn unhex(h: &str) -> Option<String> {
    if !h.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(h.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    String::from_utf8(bytes).ok()
}

/// `name`'s line, its newline included.
fn line_into(out: &mut String, name: &str, line: &Line) {
    use std::fmt::Write as _;
    out.push_str(name);
    match line.next {
        Some(t) => {
            let _ = write!(out, " {t}");
        }
        None => out.push_str(" -"),
    }
    if let Some(k) = &line.kept {
        let s = k.stood;
        let _ = write!(out, " {}/{:x}/{}", s.change, s.history, s.forks);
        for (stmt, m) in &k.marks {
            let (status, reason) = m.refused.as_ref().map_or((0, ""), |r| (r.0, &*r.1));
            let _ = write!(
                out,
                " {stmt},{},{:x},{},{},{status},",
                m.at, m.version.0, m.version.1, m.skipped
            );
            for b in reason.bytes() {
                let _ = write!(out, "{b:02x}");
            }
        }
    }
    out.push('\n');
}

/// Writes the hint whole: beside, synced, renamed over the one before and
/// the rename made durable -- an entry appended after a rename the
/// directory had not kept went with it at a power loss. Its caller writes
/// it at most once a minute, where it changed.
pub fn write<'a>(dir: &Path, lines: impl Iterator<Item = (&'a str, Line)>) -> io::Result<()> {
    let mut text = String::from(HEAD);
    text.push('\n');
    for (name, line) in lines {
        line_into(&mut text, name, &line);
    }
    let to = path(dir);
    let tmp = dir.join(format!("{FILE}.writing"));
    let written = std::fs::File::create(&tmp)
        .and_then(|mut f| {
            f.write_all(text.as_bytes())?;
            f.sync_all()
        })
        .and_then(|_| std::fs::rename(&tmp, &to))
        .and_then(|_| fennec_core::fs::sync_dir(&to).map_err(|e| io::Error::other(e.to_string())));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Enters `name`, synced: a line appended to the hint, to look at at once.
/// `NotFound` where there is no hint to append to -- one an operator
/// removed -- for the caller to write it whole.
pub fn append(dir: &Path, name: &str) -> io::Result<()> {
    let mut line = String::new();
    line_into(&mut line, name, &Line::default());
    let mut f = std::fs::OpenOptions::new().append(true).open(path(dir))?;
    f.write_all(line.as_bytes())?;
    f.sync_data()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fennec-hint-{tag}-{}-{:?}",
            std::process::id(),
            std::time::Instant::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// What is written reads back as it was: a slot, a tenant to look at,
    /// and a closed tenant's marks -- a ran one, and a refused one whose
    /// reason holds a space, a comma and a line's end.
    #[test]
    fn a_hint_reads_back_what_was_written_and_its_appends() {
        let dir = scratch("back");
        let kept = Kept {
            stood: Stood {
                change: 42,
                history: 0xfeed,
                forks: 2,
            },
            marks: vec![
                (
                    "tidy".into(),
                    SlotMark {
                        at: 1_700_000_000_000,
                        version: (0xfeed, 7),
                        refused: None,
                        skipped: 3,
                    },
                ),
                (
                    "lapse".into(),
                    SlotMark {
                        at: 1_700_000_060_000,
                        version: (0, 9),
                        refused: Some((412, "none due, sir\nnot one".into())),
                        skipped: 0,
                    },
                ),
            ],
        };
        let lines = [
            (
                "acme",
                Line {
                    next: Some(1_700_000_120_000),
                    kept: Some(kept.clone()),
                },
            ),
            ("beta", Line::default()),
        ];
        write(&dir, lines.iter().map(|(n, l)| (*n, l.clone()))).unwrap();
        append(&dir, "gamma").unwrap();
        let read = read(&dir).unwrap().unwrap();
        assert_eq!(read.len(), 3);
        assert_eq!(read["acme"].kept.as_ref(), Some(&kept));
        assert_eq!(read["acme"].next, Some(1_700_000_120_000));
        assert_eq!(read["beta"], Line::default());
        assert_eq!(read["gamma"], Line::default());

        // An append a crash cut short is passed over; a line that does not
        // read makes the hint none.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(path(&dir))
            .unwrap();
        f.write_all(b"delta -").unwrap();
        assert!(!read_names(&dir).contains(&"delta".to_string()));
        std::fs::write(path(&dir), format!("{HEAD}\nacme soon\n")).unwrap();
        assert!(super::read(&dir).is_err());
        std::fs::write(path(&dir), "acme -\n").unwrap();
        assert!(super::read(&dir).is_err());
        std::fs::remove_file(path(&dir)).unwrap();
        assert!(super::read(&dir).unwrap().is_none());
        assert_eq!(
            append(&dir, "acme").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn read_names(dir: &Path) -> Vec<String> {
        read(dir).unwrap().unwrap().into_keys().collect()
    }
}

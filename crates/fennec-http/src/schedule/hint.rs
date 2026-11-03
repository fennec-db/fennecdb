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
//! is miss one that holds one, or hold a later slot than one defined: a
//! tenant is entered, synced, before every block bringing a schedule lands
//! -- a line appended and fsynced, one line whatever the node holds --
//! while the whole is written again by rename at most once a minute
//! ([`write`]).
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
//! -- was a definition that never landed, and is passed over, the next
//! append cutting it off first ([`append`]); a file that does not read
//! otherwise is no hint, and the node learns one again.

use fennec_core::engine::SlotMark;
use fennec_core::prelude::*;
use std::io::{self, Read, Seek, Write};
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

/// Reads the hint in `dir`, each tenant's line handed to `each` in the
/// file's order -- a tenant named twice, its later line last: `None` where
/// there is none, an error where it does not read -- each a node that
/// learns one again -- and otherwise whether a last line cut short was
/// passed over, for the caller to write the hint whole. Into the caller's
/// own table: a map built here only to be moved there was the server's
/// bytes for nothing.
pub fn read(dir: &Path, each: &mut dyn FnMut(&str, Line)) -> io::Result<Option<bool>> {
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
    for (i, l) in lines.enumerate() {
        let Some((name, line)) = parse(l) else {
            return Err(invalid(format!("line {} does not read: {l:?}", i + 2)));
        };
        each(name, line);
    }
    Ok(Some(whole.len() + 1 < text.len()))
}

fn parse(l: &str) -> Option<(&str, Line)> {
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
            let mut marks = Vec::new();
            for m in f {
                marks.push(mark(m)?);
            }
            Some(Kept { stood, marks })
        }
    };
    Some((name, Line { next, kept }))
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
    let mut bytes = Vec::with_capacity(h.len() / 2);
    let mut at = 0;
    while at < h.len() {
        bytes.push(u8::from_str_radix(h.get(at..at + 2)?, 16).ok()?);
        at += 2;
    }
    String::from_utf8(bytes).ok()
}

/// A whole hint's first line, for [`line_into`] to follow and [`write`]
/// to write.
pub(crate) fn begun() -> String {
    let mut out = String::from(HEAD);
    out.push('\n');
    out
}

/// `name`'s line, its newline included: its next slot -- `None` to look at
/// once -- and a closed tenant's marks kept.
pub(crate) fn line_into(out: &mut String, name: &str, next: Option<i64>, kept: Option<&Kept>) {
    use std::fmt::Write as _;
    out.push_str(name);
    match next {
        Some(t) => {
            let _ = write!(out, " {t}");
        }
        None => out.push_str(" -"),
    }
    if let Some(k) = kept {
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

/// Writes the hint whole, `text` from [`begun`] and [`line_into`]: beside,
/// synced, renamed over the one before and the rename made durable -- an
/// entry appended after a rename the directory had not kept went with it
/// at a power loss. Its caller writes it at most once a minute, where it
/// changed.
pub fn write(dir: &Path, text: &str) -> io::Result<()> {
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
/// A last line a crash cut short is cut off first: appended to as it was,
/// `acme` and `beta -` read as `acmebeta`, and `beta` was in no line; ended
/// with a newline, the piece left would be a line that does not read, and
/// the hint none. `NotFound` where there is no hint to append to -- one an
/// operator removed, or none whole -- for the caller to write it whole.
pub fn append(dir: &Path, name: &str) -> io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .open(path(dir))?;
    let mut last = [0u8];
    f.seek(io::SeekFrom::End(-1))
        .and_then(|_| f.read_exact(&mut last))
        .map_err(|_| io::Error::from(io::ErrorKind::NotFound))?;
    if last[0] != b'\n' {
        let mut all = Vec::new();
        f.seek(io::SeekFrom::Start(0))?;
        f.read_to_end(&mut all)?;
        let end = (all.iter().rposition(|&b| b == b'\n'))
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        f.set_len(end as u64 + 1)?;
    }
    let mut line = String::new();
    line_into(&mut line, name, None, None);
    f.write_all(line.as_bytes())?;
    f.sync_data()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fennec-hint-{tag}-{}-{:?}",
            std::process::id(),
            std::time::Instant::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The hint's lines by tenant, its later line where named twice, and
    /// whether a torn last line was passed over.
    fn lines(dir: &Path) -> io::Result<Option<(HashMap<String, Line>, bool)>> {
        let mut out = HashMap::new();
        let torn = read(dir, &mut |n, l| {
            out.insert(n.to_string(), l);
        })?;
        Ok(torn.map(|t| (out, t)))
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
        let mut text = begun();
        line_into(&mut text, "acme", Some(1_700_000_120_000), Some(&kept));
        line_into(&mut text, "beta", None, None);
        write(&dir, &text).unwrap();
        append(&dir, "gamma").unwrap();
        let (read, torn) = lines(&dir).unwrap().unwrap();
        assert!(!torn);
        assert_eq!(read.len(), 3);
        assert_eq!(read["acme"].kept.as_ref(), Some(&kept));
        assert_eq!(read["acme"].next, Some(1_700_000_120_000));
        assert_eq!(read["beta"], Line::default());
        assert_eq!(read["gamma"], Line::default());

        // An append a crash cut short is passed over, and said to be.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(path(&dir))
            .unwrap();
        f.write_all(b"delta -").unwrap();
        let (read, torn) = lines(&dir).unwrap().unwrap();
        assert!(torn && !read.contains_key("delta"));
        // A line appended after it cuts it off first: `omega` is named,
        // never `delta -omega`, and the piece left never reads as a line.
        append(&dir, "omega").unwrap();
        let (read, torn) = lines(&dir).unwrap().unwrap();
        assert!(!torn);
        assert!(read.contains_key("omega"), "{read:?}");
        assert!(!read.keys().any(|k| k.contains("delta")), "{read:?}");
        assert_eq!(read.len(), 4, "{read:?}");
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(path(&dir))
            .unwrap();
        f.write_all(b"del").unwrap();
        append(&dir, "psi").unwrap();
        let (read, _) = lines(&dir).unwrap().unwrap();
        assert!(read.contains_key("psi") && !read.contains_key("del"));

        // A line that does not read makes the hint none.
        std::fs::write(path(&dir), format!("{HEAD}\nacme soon\n")).unwrap();
        assert!(lines(&dir).is_err());
        std::fs::write(path(&dir), "acme -\n").unwrap();
        assert!(lines(&dir).is_err());
        std::fs::remove_file(path(&dir)).unwrap();
        assert!(lines(&dir).unwrap().is_none());
        assert_eq!(
            append(&dir, "acme").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

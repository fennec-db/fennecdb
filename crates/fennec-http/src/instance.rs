//! A tenant file's identity on its node: what a router names to take back
//! a file it left there, and the one thing the node takes one back by.
//!
//! **Why an identity and not the file's lineage and change.** A file the
//! router left -- a create taken back whose `DELETE` failed, a move's
//! source that would not go -- cannot be told from a later file at the same
//! place by what it holds: a lineage is its history's, and a tenant moved
//! back onto the node brings the same one; a change is the history's count,
//! and a tenant made again on a node without `--replication-token`, where
//! every lineage is none, stands at change 2 as the file left did. Both
//! took a live tenant's one file in #371's reviews. An identity is the
//! file's: 64 random bits minted as the file is made (a create, an
//! import) and as it is made to follow -- a router's follow keeping the one
//! its `PUT` gave only on the file carrying it as it begins, so one landing
//! late gives a later file none -- and gone as it is promoted, so a
//! later file at the place -- or the same one made something else --
//! carries another or none, and a router naming the one it learned takes
//! back only the file it left, unchanged since.
//!
//! **Kept beside the file, bound to it and to its role.** In
//! `<tenant>.instance`, as `.follows` and `.quota` are kept: a record in the
//! file would be a kind every binary before refuses and a replica is sent,
//! and the identity is this copy's alone. A file the node did not hold as it
//! changed -- made again by a binary from before this one, which leaves the
//! sidecar where it deletes the file; put in place by `fennec restore`, an
//! install's rename or a hand -- is another file under an old identity, so
//! the sidecar names the file it was written for: its device, inode and
//! birth time -- on a file system keeping no birth time no file is named,
//! and none carries an identity, as an inode alone a later file may take
//! over. An open notes the file's binding, one `stat` (a read of the
//! sidecar there added 15-20 us to an empty tenant's 67); a close forgets an
//! identity naming another file than the one opened, and binds it to what
//! this process's own renames made of that one (a `compact`, a checkpoint, a
//! follower taking its primary's image). And it names whether the file was
//! told to follow, its `.follows` there: an identity holds only while that
//! stays so. A promotion removes `.follows`, synced, so the identity of the
//! follow goes with it and the promotion writes nothing of its own -- put
//! away by an unlink before it alone, it took a pair's failover from 4.0
//! ms a tenant to 5.2 (`fennec-shard`'s `overhead`), and nobody would learn
//! a new one before a freeze, which mints one where there is none.
//! A follow puts the identity away before it writes `.follows`, its sync
//! covering the unlink, so no identity it had comes back with a later one.
//! A file whose binding or role does not hold carries no identity, and none
//! is taken back.
//!
//! The first line is `1 <id> <device> <inode> <birth ns> <n|f>`: a later
//! layout raises the number, which this one reads as no identity, never as
//! one.

use std::io;
use std::path::{Path, PathBuf};

/// The header a router proposes an identity in, for a file its call makes
/// or changes: what it notes where the call goes unanswered.
pub const HEADER: &str = "Fennec-Instance";

/// The sidecar's layout.
const LAYOUT: &str = "1";

/// An identity as it is written: 16 hex digits.
pub fn text(id: u64) -> String {
    format!("{id:016x}")
}

/// An identity read from its 16 hex digits: never 0, no file's.
pub fn parse(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.len() != 16 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(s, 16).ok().filter(|&id| id != 0)
}

/// An identity as a JSON value: its text, or `null`.
pub fn json(id: Option<u64>) -> String {
    id.map_or("null".into(), |id| format!("\"{}\"", text(id)))
}

pub(crate) fn path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.instance"))
}

/// What names the file a sidecar was written for: its device, inode and
/// birth time in nanoseconds.
pub(crate) type Binding = (u64, u64, u64);

/// A file's birth time in nanoseconds, where its file system keeps one: a
/// file system that keeps none binds no file, and its files carry no
/// identity -- a file put at the name without its sidecar put away, by a
/// hand's `rm` and a restore or by a binary from before, could take over
/// the inode, the device and inode alone naming it then, and inherit the
/// identity of the file a router left there.
fn birth(created: io::Result<std::time::SystemTime>) -> Option<u64> {
    let d = created.ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(d.as_nanos().min(u64::MAX as u128) as u64).filter(|&ns| ns != 0)
}

/// `file`'s binding as it stands: one `stat`, all an open pays. None
/// (`Unsupported`) where its file system keeps no birth time ([`birth`]).
pub(crate) fn binding(file: &Path) -> io::Result<Binding> {
    let m = std::fs::metadata(file)?;
    let born = birth(m.created()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "the file system keeps no birth time",
        )
    })?;
    #[cfg(unix)]
    let (dev, ino) = {
        use std::os::unix::fs::MetadataExt;
        (m.dev(), m.ino())
    };
    #[cfg(not(unix))]
    let (dev, ino) = (0, 0);
    Ok((dev, ino, born))
}

/// Whether the tenant was told to follow: its `.follows` is there.
fn follows(dir: &Path, name: &str) -> bool {
    dir.join(format!("{name}.follows")).exists()
}

/// The sidecar as written: its identity, the file it names and whether
/// that file was told to follow.
fn read(dir: &Path, name: &str) -> Option<(u64, Binding, bool)> {
    let text = std::fs::read_to_string(path(dir, name)).ok()?;
    let mut f = text.lines().next()?.split(' ');
    if f.next()? != LAYOUT {
        return None;
    }
    let id = parse(f.next()?)?;
    let mut n = || f.next()?.parse::<u64>().ok();
    // A binding of no birth time names no file alone ([`birth`]).
    let bound = (n()?, n()?, n().filter(|&ns| ns != 0)?);
    let following = match f.next()? {
        "f" => true,
        "n" => false,
        _ => return None,
    };
    Some((id, bound, following))
}

/// Writes the sidecar in place, syncing nothing: lost or cut short by a
/// power loss, the line does not read, or names the file or the role
/// before, and either is no identity -- a file kept, never one taken. Read
/// and written under the tenant's slot alone, so no reader meets it half
/// written; what must not come back is put away first ([`put_away`]).
/// Beside and renamed, a create took 150 us more in process, against 80 so.
fn write(dir: &Path, name: &str, id: u64, bound: Binding, following: bool) -> io::Result<()> {
    let line = format!(
        "{LAYOUT} {} {} {} {} {}\n",
        text(id),
        bound.0,
        bound.1,
        bound.2,
        if following { "f" } else { "n" }
    );
    std::fs::write(path(dir, name), line)
}

/// The identity `file` carries: the sidecar's, where it names this file in
/// the role it has.
pub(crate) fn carried(dir: &Path, name: &str, file: &Path) -> Option<u64> {
    let (id, bound, following) = read(dir, name)?;
    (binding(file).ok()? == bound && follows(dir, name) == following).then_some(id)
}

/// The identity of a file this process holds open: the sidecar's, where it
/// names the file as it was when this process took it (`held`, the binding
/// its open saw or its last stamp wrote), in the role it has. Its own
/// renames since are this process's, which the close binds the identity to
/// ([`closing`]).
pub(crate) fn held(dir: &Path, name: &str, held: Option<Binding>) -> Option<u64> {
    let (id, bound, following) = read(dir, name)?;
    (Some(bound) == held && follows(dir, name) == following).then_some(id)
}

/// Gives `file` the identity `id`, bound to it and its role as they stand:
/// the binding written.
pub(crate) fn stamp(dir: &Path, name: &str, file: &Path, id: u64) -> io::Result<Binding> {
    let bound = binding(file)?;
    write(dir, name, id, bound, follows(dir, name)).map(|_| bound)
}

/// Puts the identity away before the file is made to follow, syncing
/// nothing itself: the follow syncs the directory as it writes `.follows`
/// after it. Kept, an identity the follow's stamp did not replace -- lost to
/// a power loss -- would hold again once a promotion removed `.follows`.
pub(crate) fn put_away(dir: &Path, name: &str) -> io::Result<()> {
    match std::fs::remove_file(path(dir, name)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// At a close of a file held since its open: the identity bound to what
/// this process's own renames made of it (a `compact`, a checkpoint, a
/// follower taking its primary's image), in the role it was written in,
/// where the sidecar names the file as this process took it (`held`), and
/// forgotten where it names another -- one put in place before the open,
/// by a hand, a restore or a binary from before, under an identity it does
/// not carry. Not synced: lost, the binding names the file before, which
/// the file is not -- a file kept, never one taken.
pub(crate) fn closing(dir: &Path, name: &str, held: Option<Binding>, file: &Path) {
    let p = path(dir, name);
    let Some((id, bound, following)) = read(dir, name) else {
        // Cut short, or a layout this binary does not read: no identity,
        // said so by leaving none.
        if p.exists() {
            let _ = std::fs::remove_file(&p);
        }
        return;
    };
    if Some(bound) != held {
        if let Err(e) = std::fs::remove_file(&p) {
            crate::log!("tenant `{name}`'s identity names another file, and stays: {e}");
        }
        return;
    }
    match binding(file) {
        Ok(now) if now != bound => {
            if let Err(e) = write(dir, name, id, now, following) {
                crate::log!("tenant `{name}`'s identity was not bound to its file again: {e}");
            }
        }
        _ => {}
    }
}

/// The identities a router names, as a JSON array of their texts or one
/// text: what a reclaim is held to.
pub fn list_in(body: &str) -> Result<Vec<u64>, String> {
    let fields = fennec_core::json::parse_object(body)
        .map_err(|e| format!("the body is not a JSON object: {e}"))?;
    match fields.into_iter().find(|(k, _)| k == "instances") {
        Some((_, v)) => ids_of(v),
        None => Err("name the file's instances: {\"instances\": [\"<16 hex>\"]}".into()),
    }
}

/// The identities an `"instances"` value names, a list of texts or one:
/// what a reclaim, or a follow over a file found, is held to. None named
/// is refused, never read as any.
pub fn ids_of(v: fennec_core::prelude::Value) -> Result<Vec<u64>, String> {
    use fennec_core::prelude::Value;
    let items = match v {
        Value::List(items) => items,
        other => vec![other],
    };
    let ids = items.into_iter().map(|item| match item {
        Value::Text(t) => parse(&t).ok_or("an instance is 16 hex digits"),
        _ => Err("an instance is 16 hex digits"),
    });
    let ids = ids.collect::<Result<Vec<u64>, _>>()?;
    match ids.is_empty() {
        true => Err("name the file's instances: {\"instances\": [\"<16 hex>\"]}".into()),
        false => Ok(ids),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fennec-instance-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn an_identity_names_its_file_and_no_other_put_there() {
        let dir = scratch("names");
        let file = dir.join("acme.fennec");
        std::fs::write(&file, b"one").unwrap();
        stamp(&dir, "acme", &file, 0xabc).unwrap();
        assert_eq!(carried(&dir, "acme", &file), Some(0xabc));
        // Written in place: the same file.
        std::fs::write(&file, b"one two").unwrap();
        assert_eq!(carried(&dir, "acme", &file), Some(0xabc));
        // Another file renamed into the place, as a restore or a binary
        // from before making it again does: no identity.
        let other = dir.join("other");
        std::fs::write(&other, b"one").unwrap();
        std::fs::rename(&other, &file).unwrap();
        assert_eq!(carried(&dir, "acme", &file), None);
        // Opened so: held as the file it is, which the sidecar does not
        // name; the close forgets it, and nothing binds it to this file.
        let opened = binding(&file).ok();
        assert_eq!(held(&dir, "acme", opened), None);
        closing(&dir, "acme", opened, &file);
        assert!(!path(&dir, "acme").exists());
        assert_eq!(carried(&dir, "acme", &file), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_close_binds_the_identity_to_its_own_rewrite() {
        let dir = scratch("rebind");
        let file = dir.join("acme.fennec");
        std::fs::write(&file, b"one").unwrap();
        let stamped = stamp(&dir, "acme", &file, 7).unwrap();
        let opened = binding(&file).ok();
        assert_eq!(opened, Some(stamped));
        // A compact's rename while the file is held.
        let side = dir.join("acme.fennec.compacting");
        std::fs::write(&side, b"one").unwrap();
        std::fs::rename(&side, &file).unwrap();
        assert_eq!(held(&dir, "acme", opened), Some(7));
        assert_eq!(carried(&dir, "acme", &file), None);
        closing(&dir, "acme", opened, &file);
        assert_eq!(carried(&dir, "acme", &file), Some(7));
        // Put away: none, and nothing written back.
        put_away(&dir, "acme").unwrap();
        put_away(&dir, "acme").unwrap();
        assert_eq!(carried(&dir, "acme", &file), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An identity holds in the role it was given: one given as the file
    /// was told to follow goes as its `.follows` does -- a promotion -- and
    /// one given before goes as `.follows` comes.
    #[test]
    fn an_identity_holds_while_the_file_keeps_its_role() {
        let dir = scratch("role");
        let file = dir.join("acme.fennec");
        let follows = dir.join("acme.follows");
        std::fs::write(&file, b"one").unwrap();
        let made = stamp(&dir, "acme", &file, 3).unwrap();
        assert_eq!(carried(&dir, "acme", &file), Some(3));
        std::fs::write(&follows, b"http://n1").unwrap();
        assert_eq!(carried(&dir, "acme", &file), None);
        assert_eq!(held(&dir, "acme", Some(made)), None);
        stamp(&dir, "acme", &file, 4).unwrap();
        assert_eq!(carried(&dir, "acme", &file), Some(4));
        assert_eq!(held(&dir, "acme", Some(made)), Some(4));
        // Marked heard: the same role.
        std::fs::write(&follows, b"http://n1\nheard").unwrap();
        assert_eq!(carried(&dir, "acme", &file), Some(4));
        // Promoted while open: none, and the close keeps it none.
        std::fs::remove_file(&follows).unwrap();
        assert_eq!(held(&dir, "acme", Some(made)), None);
        closing(&dir, "acme", Some(made), &file);
        assert_eq!(carried(&dir, "acme", &file), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_layout_or_a_line_it_does_not_read_is_no_identity() {
        let dir = scratch("layout");
        let file = dir.join("acme.fennec");
        std::fs::write(&file, b"one").unwrap();
        stamp(&dir, "acme", &file, 9).unwrap();
        let line = std::fs::read_to_string(path(&dir, "acme")).unwrap();
        std::fs::write(path(&dir, "acme"), line.replacen("1 ", "2 ", 1)).unwrap();
        assert_eq!(carried(&dir, "acme", &file), None);
        std::fs::write(path(&dir, "acme"), line.replacen(" n", " x", 1)).unwrap();
        assert_eq!(carried(&dir, "acme", &file), None);
        std::fs::write(path(&dir, "acme"), line.replacen(" n", "", 1)).unwrap();
        assert_eq!(carried(&dir, "acme", &file), None);
        std::fs::write(path(&dir, "acme"), &line).unwrap();
        assert_eq!(carried(&dir, "acme", &file), Some(9));
        std::fs::write(path(&dir, "acme"), "1 0000").unwrap();
        assert_eq!(carried(&dir, "acme", &file), None);
        closing(&dir, "acme", binding(&file).ok(), &file);
        assert!(!path(&dir, "acme").exists());
        assert_eq!(parse("0000000000000000"), None);
        assert_eq!(parse("00000000000000ab"), Some(0xab));
        assert_eq!(parse("ab"), None);
        assert_eq!(
            list_in(r#"{"instances":["00000000000000ab","00000000000000cd"]}"#),
            Ok(vec![0xab, 0xcd])
        );
        assert!(list_in("{}").is_err());
        assert!(list_in(r#"{"instances":[1]}"#).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No birth time, no identity: a file system that keeps none binds no
    /// file, so no sidecar is written for it nor read as naming it.
    #[test]
    fn a_file_system_keeping_no_birth_time_binds_no_file() {
        use std::time::{Duration, UNIX_EPOCH};
        let unsupported = || Err(io::Error::from(io::ErrorKind::Unsupported));
        assert_eq!(birth(unsupported()), None);
        assert_eq!(birth(Ok(UNIX_EPOCH)), None);
        assert_eq!(birth(Ok(UNIX_EPOCH - Duration::from_secs(1))), None);
        assert_eq!(birth(Ok(UNIX_EPOCH + Duration::from_nanos(7))), Some(7));
        // A sidecar naming no birth time names no file, held or not.
        let dir = scratch("unborn");
        let file = dir.join("acme.fennec");
        std::fs::write(&file, b"one").unwrap();
        let (dev, ino, _) = binding(&file).unwrap();
        write(&dir, "acme", 5, (dev, ino, 0), false).unwrap();
        assert_eq!(carried(&dir, "acme", &file), None);
        assert_eq!(held(&dir, "acme", Some((dev, ino, 0))), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! `--user` and `FENNEC_USER`: a server started as root becomes an
//! unprivileged user before it opens a file or starts a thread, having
//! first handed that user the directory it writes in, where that is a
//! volume holding nothing but the database
//! (docs/design/integrations.md#the-images-user).
//!
//! The image starts as root for this alone. A volume a platform makes --
//! Fly's, Render's, Railway's, a bind mount, Docker's under
//! `volume-nocopy` -- is a file system whose root is root's, where a
//! process started as 65534 cannot make its file, and the image has no
//! shell to `chown` it with: so the binary does what the postgres and
//! redis images' entrypoints do in theirs, hand the data over, then drop
//! root for good. Started as anyone but root it does neither and runs as
//! itself, so `docker run --user`, a pod's `runAsUser` and systemd's
//! `User=` are as they were. On Windows only [`User`] and [`wanted`] are
//! built, for the flag's refusal; a service runs as the account it is
//! given.

#[cfg(unix)]
use std::path::{Component, Path, PathBuf};

/// A user and its group, by number: the image holds no `/etc/passwd` to
/// look a name up in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct User {
    pub uid: u32,
    pub gid: u32,
}

impl std::fmt::Display for User {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.uid, self.gid)
    }
}

impl User {
    /// `<uid>:<gid>`, each written in digits alone. Refused: a uid of 0,
    /// which would drop nothing, and 2^32 - 1, which `setuid` and `chown`
    /// read as "leave it as it is". `from` names where the text came from.
    pub fn parse(text: &str, from: &str) -> Result<User, String> {
        let number = |s: &str| {
            let digits = !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
            digits
                .then(|| s.parse::<u32>().ok())
                .flatten()
                .filter(|n| *n != u32::MAX)
        };
        let parsed = text
            .split_once(':')
            .and_then(|(u, g)| Some((number(u)?, number(g)?)));
        match parsed {
            None => Err(format!(
                "{from} expects <uid>:<gid>, two numbers such as 65534:65534, got `{text}`"
            )),
            Some((0, _)) => Err(format!(
                "{from} {text} names root, which is what a server started as root leaves: \
                 to run as root, set FENNEC_USER empty and give no --user"
            )),
            Some((uid, gid)) => Ok(User { uid, gid }),
        }
    }
}

/// Whom a server becomes, and what said so: `--user` over `FENNEC_USER`,
/// as `--http-token` over `FENNEC_HTTP_TOKEN`, a variable set empty
/// unset. Only root becomes anyone (`euid` 0). Started as another user,
/// the variable -- which the image sets for every container, `docker run
/// --user`'s too -- is passed over unread, and the flag is refused unless
/// it names who the process is already (`now`): a flag that cannot be done
/// is never let go by in silence.
pub fn wanted(
    flag: Option<&str>,
    env: Option<&str>,
    euid: u32,
    now: User,
) -> Result<Option<(User, &'static str)>, String> {
    if let Some(text) = flag {
        let user = User::parse(text, "--user")?;
        return match euid {
            0 => Ok(Some((user, "--user"))),
            _ if user == now => Ok(None),
            _ => Err(format!(
                "--user {user}: only a server started as root becomes another user, and this \
                 one runs as {now}"
            )),
        };
    }
    match env.filter(|t| !t.is_empty()) {
        Some(text) if euid == 0 => Ok(Some((User::parse(text, "FENNEC_USER")?, "FENNEC_USER"))),
        _ => Ok(None),
    }
}

/// Where a server writes, and which of the entries there are the
/// database's: what a start as root may hand over, and all it may.
#[cfg(unix)]
pub struct Writes {
    dir: PathBuf,
    ours: Ours,
    /// `--audit`'s file, where it is in `dir`: the server writes it too.
    audit: Option<String>,
}

#[cfg(unix)]
enum Ours {
    /// A file's server: the file and what it keeps beside it, each named
    /// `<file>.<suffix>` (`fennec_core::fs::beside`): `.lock`, `.sync`,
    /// a compaction's or an import's file before its rename.
    File(String),
    /// A node's: every file named after a tenant, `<tenant>.<anything>`
    /// (`fennec_http::tenants::shaped`) -- its file and those beside it,
    /// its quota, its hook secrets, what it follows, and their files
    /// before a rename -- and the scheduler's `schedules.hint`, which has
    /// that shape.
    Node,
    /// The directory a node's directory is made in, where it is not made
    /// yet: none of its entries is the database's.
    Nothing,
}

/// `path` from the root, its `.`s left out: what `canonicalize` gives
/// back, a link or a `..` on the way aside.
#[cfg(unix)]
fn absolute(path: &Path) -> Option<PathBuf> {
    let path = match path.is_absolute() {
        true => path.to_path_buf(),
        false => std::env::current_dir().ok()?.join(path),
    };
    Some(
        path.components()
            .filter(|c| !matches!(c, Component::CurDir))
            .collect(),
    )
}

#[cfg(unix)]
impl Writes {
    /// `--file`'s: the directory its file is in.
    pub fn file(path: &Path, audit: Option<&Path>) -> Option<Writes> {
        let path = absolute(path)?;
        let name = path.file_name()?.to_str()?.to_string();
        Some(Writes::new(
            path.parent()?.to_path_buf(),
            Ours::File(name),
            audit,
        ))
    }

    /// `--dir`'s: the directory, or where it is not made yet the one it
    /// is made in -- the node makes it as the user it became, and that
    /// directory has to let it.
    pub fn node(dir: &Path, audit: Option<&Path>) -> Option<Writes> {
        let dir = absolute(dir)?;
        match std::fs::symlink_metadata(&dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(Writes::new(
                dir.parent()?.to_path_buf(),
                Ours::Nothing,
                audit,
            )),
            _ => Some(Writes::new(dir, Ours::Node, audit)),
        }
    }

    fn new(dir: PathBuf, ours: Ours, audit: Option<&Path>) -> Writes {
        let audit = audit
            .and_then(absolute)
            .filter(|a| a.parent() == Some(dir.as_path()))
            .and_then(|a| Some(a.file_name()?.to_str()?.to_string()));
        Writes { dir, ours, audit }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether the entry named `name` is one the server writes.
    fn is_ours(&self, name: &str) -> bool {
        if self.audit.as_deref() == Some(name) {
            return true;
        }
        match &self.ours {
            Ours::File(file) => name
                .strip_prefix(file.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.')),
            Ours::Node => name
                .split_once('.')
                .is_some_and(|(tenant, _)| fennec_http::tenants::shaped(tenant)),
            Ours::Nothing => false,
        }
    }
}

/// One thing to hand over, as [`plan`] found it: [`hand_over`] changes
/// the owner of the inode it opens only if it is this one.
#[cfg(unix)]
#[derive(Debug)]
pub struct Entry {
    path: PathBuf,
    dev: u64,
    ino: u64,
    dir: bool,
}

/// What handing `w` to `user` would change, or why it may not be handed
/// over. Nothing where the directory and each entry of the database's are
/// the user's already -- every start after the first, and a volume Docker
/// filled from the image's `/data` -- whatever else the directory holds:
/// the rules are asked only of a hand-over. Otherwise the directory and
/// those of its entries that are the database's but not the user's, if
/// the directory is
///
/// - not `/`, and reached through no symbolic link: `canonicalize` gives
///   its path back as named;
/// - a volume of its own, not on the file system `/` is on (`system`, its
///   device): an image's directory, or the host's, keeps the owner it was
///   given;
/// - no one's to share: neither writable by anyone nor sticky, as `/tmp`
///   and `/dev/shm` are;
/// - holding nothing but the database's files -- each a regular file of
///   one name, never a link, a directory or a file with a name elsewhere
///   -- and a `lost+found` directory, which a fresh ext4 volume holds and
///   which stays as it is.
///
/// So a hand-over gives the user a directory and the database, and
/// nothing else of anyone's: a `--file` misdirected at `/etc`, a home or
/// `/tmp` is refused, as is a link planted among the files by a server
/// that ran as the user before.
#[cfg(unix)]
pub fn plan(w: &Writes, user: User, system: u64) -> Result<Vec<Entry>, String> {
    use std::os::unix::fs::MetadataExt;
    let top = match std::fs::metadata(&w.dir) {
        Ok(m) => m,
        // A directory not there: the open says so, as the user.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("it could not be read: {e}")),
    };
    if !top.is_dir() {
        return Err("it is no directory".into());
    }
    let mut wanted = Vec::new();
    let mut foreign: Option<String> = None;
    let entries = std::fs::read_dir(&w.dir).map_err(|e| format!("it could not be read: {e}"))?;
    for e in entries {
        let e = e.map_err(|e| format!("it could not be read: {e}"))?;
        let name = e.file_name();
        // `DirEntry::metadata` follows no link: what the entry is. One
        // that cannot be read, or named in UTF-8, is none of the
        // database's.
        let (Ok(m), Some(name)) = (e.metadata(), name.to_str()) else {
            foreign.get_or_insert_with(|| name.to_string_lossy().into_owned());
            continue;
        };
        if name == "lost+found" && m.is_dir() {
            continue;
        }
        if w.is_ours(name) && m.file_type().is_file() && m.nlink() == 1 {
            if m.uid() != user.uid {
                wanted.push(Entry {
                    path: e.path(),
                    dev: m.dev(),
                    ino: m.ino(),
                    dir: false,
                });
            }
        } else {
            foreign.get_or_insert_with(|| name.to_string());
        }
    }
    if top.uid() != user.uid {
        // The directory last: once it is the user's, a start that stopped
        // half-way is seen as one by its files, and done again.
        wanted.push(Entry {
            path: w.dir.clone(),
            dev: top.dev(),
            ino: top.ino(),
            dir: true,
        });
    }
    if wanted.is_empty() {
        return Ok(wanted);
    }
    if w.dir.parent().is_none() {
        return Err("it is `/`".into());
    }
    match std::fs::canonicalize(&w.dir) {
        Ok(real) if real == w.dir => {}
        Ok(real) => {
            return Err(format!(
                "it is reached through a symbolic link or a `..`, to {}",
                real.display()
            ))
        }
        Err(e) => return Err(format!("it could not be read: {e}")),
    }
    if top.dev() == system {
        return Err("it is on the file system `/` is on, not a volume of its own".into());
    }
    if top.mode() & 0o1002 != 0 {
        return Err(format!(
            "it is shared: anyone may write in it, or it is sticky (mode {:o})",
            top.mode() & 0o7777
        ));
    }
    if let Some(name) = foreign {
        return Err(format!(
            "it holds `{name}`, which is not the database's (a link, a directory or a file \
             with another name is none of its)"
        ));
    }
    Ok(wanted)
}

/// The device `/` is on, for [`plan`].
#[cfg(unix)]
pub fn system_device() -> std::io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(std::fs::metadata("/")?.dev())
}

/// Gives each of `entries` to `user`, through a descriptor of what was
/// opened: an entry whose inode is no longer the one [`plan`] found -- a
/// file swapped for a link, or for a name of another file, between the
/// look and the change -- is refused, never followed. Nothing is changed
/// but the owner and the group.
#[cfg(unix)]
pub fn hand_over(entries: &[Entry], user: User) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    for e in entries {
        let shown = e.path.display();
        // Read-only, so the open writes nothing; a directory opens so too.
        let f = std::fs::File::open(&e.path).map_err(|err| format!("{shown}: {err}"))?;
        let m = f.metadata().map_err(|err| format!("{shown}: {err}"))?;
        let same = (m.dev(), m.ino()) == (e.dev, e.ino)
            && match e.dir {
                true => m.is_dir(),
                false => m.file_type().is_file() && m.nlink() == 1,
            };
        if !same {
            return Err(format!("{shown} changed while it was handed over"));
        }
        std::os::unix::fs::fchown(&f, Some(user.uid), Some(user.gid))
            .map_err(|err| format!("{shown}: {err}"))?;
    }
    Ok(())
}

#[cfg(unix)]
mod sys {
    // libc's, declared here as `durability`'s `signal` is: std links libc
    // already, and the crates take no dependency. uid_t and gid_t are 32
    // bits on every Unix this builds for.
    extern "C" {
        pub fn geteuid() -> u32;
        pub fn getuid() -> u32;
        pub fn getegid() -> u32;
        pub fn getgid() -> u32;
        pub fn setuid(uid: u32) -> i32;
        pub fn setgid(gid: u32) -> i32;
        pub fn getgroups(size: i32, list: *mut u32) -> i32;
    }
    // Its count is a `size_t` on Linux's libcs and Android's, an `int` on
    // Apple's and the BSDs'.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    extern "C" {
        pub fn setgroups(size: usize, list: *const u32) -> i32;
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    extern "C" {
        pub fn setgroups(size: i32, list: *const u32) -> i32;
    }
}

/// The process's effective uid and its user and group, for [`wanted`].
#[cfg(unix)]
pub fn current() -> (u32, User) {
    // SAFETY: none of these takes an argument or can fail.
    unsafe {
        (
            sys::geteuid(),
            User {
                uid: sys::geteuid(),
                gid: sys::getegid(),
            },
        )
    }
}

/// Becomes `user` for good, before any thread is made: the kernel keeps
/// credentials a thread, and only libc's calls carry them to the others
/// there may be. The supplementary groups go first, root's among them,
/// then the group, then the user -- each step needing the root the last
/// one gives up -- and then it is checked that real, effective and saved
/// ids are the user's alone and that root cannot be had back: `setuid(0)`
/// must fail. A step that fails ends the start; nothing runs as root that
/// was told to run as `user`.
#[cfg(unix)]
pub fn become_user(user: User) -> Result<(), String> {
    let failed = |what: &str| {
        let e = std::io::Error::last_os_error();
        format!(
            "could not become {user} ({what}: {e}): a server started as root needs the \
             SETUID and SETGID capabilities to drop root, and CHOWN to hand its volume over"
        )
    };
    // SAFETY: an empty list, which the call reads nothing of; the ids are
    // plain numbers.
    unsafe {
        if sys::setgroups(0, std::ptr::null()) != 0 {
            return Err(failed("setgroups"));
        }
        if sys::setgid(user.gid) != 0 {
            return Err(failed("setgid"));
        }
        if sys::setuid(user.uid) != 0 {
            return Err(failed("setuid"));
        }
    }
    // SAFETY: as in `current`; `getgroups` with a size of 0 only counts.
    let (uid, euid, gid, egid, groups) = unsafe {
        (
            sys::getuid(),
            sys::geteuid(),
            sys::getgid(),
            sys::getegid(),
            sys::getgroups(0, std::ptr::null_mut()),
        )
    };
    if (uid, euid, gid, egid) != (user.uid, user.uid, user.gid, user.gid) {
        return Err(format!(
            "asked to become {user}, the process is {uid}:{gid} (effective {euid}:{egid})"
        ));
    }
    // Linux keeps no supplementary group after `setgroups(0)`; Apple's
    // libc answers the effective group as one, which is the user's.
    if cfg!(any(target_os = "linux", target_os = "android")) && groups != 0 {
        return Err(format!(
            "became {user}, and {groups} supplementary groups stayed"
        ));
    }
    // SAFETY: plain numbers. A call that succeeds is the failure checked
    // for: the process is root again, and the start ends.
    if unsafe { sys::setuid(0) } == 0 {
        return Err(format!("became {user}, and could become root again"));
    }
    if user.gid != 0 && unsafe { sys::setgid(0) } == 0 {
        return Err(format!("became {user}, and could take root's group again"));
    }
    Ok(())
}

/// A start as root told to become `user`: `writes` handed to it where the
/// rules let it be ([`plan`]), then `user` become. What a hand-over did,
/// or why it did nothing, is said; a hand-over that fails is said and
/// passed over, since the server may write there all the same and its
/// open says if it cannot. Only the drop's failure ends the start.
#[cfg(unix)]
pub fn start_as(user: User, from: &str, writes: Option<&Writes>) -> Result<(), String> {
    if let Some(w) = writes {
        let shown = w.dir().display();
        let done = system_device()
            .map_err(|e| format!("`/` could not be read: {e}"))
            .and_then(|system| plan(w, user, system))
            .and_then(|entries| hand_over(&entries, user).map(|()| said(w, &entries)));
        match done {
            Ok(said) if said.is_empty() => {}
            Ok(said) => fennec_http::log!("handed {said} to {user}"),
            Err(why) => fennec_http::log!(
                "warning: {shown} is not handed to {user}: {why}. If the server cannot write \
                 there, make it {user}'s, or start the server as the directory's owner"
            ),
        }
    }
    become_user(user)?;
    fennec_http::log!("running as {user}, started as root ({from})");
    Ok(())
}

/// What a hand-over of `entries` gave: "/data", "/data and 2 of its
/// files", "2 files in /data", or nothing.
#[cfg(unix)]
fn said(w: &Writes, entries: &[Entry]) -> String {
    let files = entries.iter().filter(|e| !e.dir).count();
    let s = if files == 1 { "" } else { "s" };
    let dir = w.dir().display();
    match (entries.iter().any(|e| e.dir), files) {
        (false, 0) => String::new(),
        (true, 0) => format!("{dir}"),
        (true, n) => format!("{dir} and {n} of its file{s}"),
        (false, n) => format!("{n} file{s} in {dir}"),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    /// A directory of this test's own, empty.
    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fennec-user-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // Canonical: macOS's temporary directory is reached through
        // `/var`, a link to `/private/var`.
        std::fs::canonicalize(&d).unwrap()
    }

    fn touch(d: &Path, name: &str) {
        std::fs::write(d.join(name), b"x").unwrap();
    }

    /// Someone the test process is not, so that every entry of its own is
    /// one to hand over.
    fn other() -> User {
        let (_, me) = current();
        User {
            uid: me.uid.wrapping_add(1),
            gid: me.gid,
        }
    }

    /// The device of `d`, which [`plan`] takes for the system's to find
    /// it on `/`'s file system, and one past it to find it a volume.
    fn devices(d: &Path) -> (u64, u64) {
        let dev = std::fs::metadata(d).unwrap().dev();
        (dev, dev.wrapping_add(1))
    }

    fn names(entries: &[Entry]) -> Vec<String> {
        let mut out: Vec<String> = entries
            .iter()
            .map(|e| e.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn a_user_is_two_numbers_and_never_root() {
        assert_eq!(
            User::parse("65534:65534", "--user"),
            Ok(User {
                uid: 65534,
                gid: 65534
            })
        );
        assert_eq!(
            User::parse("1000:0", "--user"),
            Ok(User { uid: 1000, gid: 0 })
        );
        for bad in [
            "",
            "65534",
            "65534:",
            ":65534",
            "a:b",
            "-1:5",
            "+5:5",
            " 5:5",
            "5:5:5",
            "4294967295:1",
            "1:4294967295",
            "99999999999:1",
        ] {
            let e = User::parse(bad, "--user").unwrap_err();
            assert!(e.contains("expects <uid>:<gid>"), "{bad}: {e}");
        }
        let e = User::parse("0:0", "FENNEC_USER").unwrap_err();
        assert!(e.starts_with("FENNEC_USER 0:0 names root"), "{e}");
    }

    #[test]
    fn only_root_becomes_anyone_and_the_flag_comes_first() {
        let me = User {
            uid: 1000,
            gid: 1000,
        };
        let nobody = User {
            uid: 65534,
            gid: 65534,
        };
        let root = User { uid: 0, gid: 0 };
        // The image's variable, as root: taken.
        assert_eq!(
            wanted(None, Some("65534:65534"), 0, root),
            Ok(Some((nobody, "FENNEC_USER")))
        );
        // The flag over the variable.
        assert_eq!(
            wanted(Some("1000:1000"), Some("65534:65534"), 0, root),
            Ok(Some((me, "--user")))
        );
        // Set empty: unset, and root stays root.
        assert_eq!(wanted(None, Some(""), 0, root), Ok(None));
        assert_eq!(wanted(None, None, 0, root), Ok(None));
        // `docker run --user 1000:1000` of the image: the variable passed
        // over, unread, whatever it says.
        assert_eq!(wanted(None, Some("65534:65534"), 1000, me), Ok(None));
        assert_eq!(wanted(None, Some("nonsense"), 1000, me), Ok(None));
        // As root it is read, and refused where it does not parse.
        assert!(wanted(None, Some("nonsense"), 0, root).is_err());
        // The flag as anyone else: what the process is already, or refused.
        assert_eq!(wanted(Some("1000:1000"), None, 1000, me), Ok(None));
        let e = wanted(Some("65534:65534"), None, 1000, me).unwrap_err();
        assert!(e.contains("only a server started as root"), "{e}");
        assert!(wanted(Some(""), None, 0, root).is_err());
    }

    #[test]
    fn a_files_volume_hands_over_the_file_and_what_it_keeps_beside() {
        let d = dir("file");
        for f in [
            "data.fennec",
            "data.fennec.lock",
            "data.fennec.sync",
            "data.fennec.compacting",
        ] {
            touch(&d, f);
        }
        std::fs::create_dir(d.join("lost+found")).unwrap();
        let w = Writes::file(&d.join("data.fennec"), None).unwrap();
        assert_eq!(w.dir(), d);
        let (_, volume) = devices(&d);
        let got = plan(&w, other(), volume).unwrap();
        // The directory last, and `lost+found` left as it is.
        assert!(got.last().unwrap().dir);
        assert_eq!(got.last().unwrap().path, d);
        let mut want = vec![
            d.file_name().unwrap().to_string_lossy().into_owned(),
            "data.fennec".into(),
            "data.fennec.compacting".into(),
            "data.fennec.lock".into(),
            "data.fennec.sync".into(),
        ];
        want.sort();
        assert_eq!(names(&got), want);

        // The user's already: nothing to do, and no rule asked -- the
        // directory on `/`'s file system and a stranger in it too.
        touch(&d, "notes.txt");
        let (_, me) = current();
        assert!(plan(&w, me, devices(&d).0).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_directory_holding_anything_else_is_not_handed_over() {
        let who = other();
        let refused = |tag: &str, set: &dyn Fn(&Path), want: &str| {
            let d = dir(tag);
            touch(&d, "data.fennec");
            set(&d);
            let w = Writes::file(&d.join("data.fennec"), None).unwrap();
            let e = plan(&w, who, devices(&d).1).unwrap_err();
            assert!(e.contains(want), "{tag}: {e}");
            let _ = std::fs::remove_dir_all(&d);
        };
        refused("stranger", &|d| touch(d, "notes.txt"), "holds `notes.txt`");
        // A name like the file's, but not beside it.
        refused(
            "like",
            &|d| touch(d, "data.fennecx"),
            "holds `data.fennecx`",
        );
        refused(
            "subdir",
            &|d| std::fs::create_dir(d.join("data.fennec.d")).unwrap(),
            "holds `data.fennec.d`",
        );
        // A link among the files, as a server that ran as the user could
        // plant: never followed, and the hand-over refused.
        refused(
            "link",
            &|d| std::os::unix::fs::symlink("/etc/passwd", d.join("data.fennec.lock")).unwrap(),
            "holds `data.fennec.lock`",
        );
        // A file with a name elsewhere: a hard link to another's file.
        let outside = dir("hardlink-outside");
        touch(&outside, "secret");
        refused(
            "hardlink",
            &|d| std::fs::hard_link(outside.join("secret"), d.join("data.fennec.lock")).unwrap(),
            "holds `data.fennec.lock`",
        );
        let _ = std::fs::remove_dir_all(&outside);
        refused(
            "shared",
            &|d| std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o777)).unwrap(),
            "anyone may write in it",
        );
        refused(
            "sticky",
            &|d| std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o1755)).unwrap(),
            "sticky",
        );
    }

    #[test]
    fn only_a_volume_of_its_own_is_handed_over() {
        let d = dir("system");
        touch(&d, "data.fennec");
        let w = Writes::file(&d.join("data.fennec"), None).unwrap();
        let e = plan(&w, other(), devices(&d).0).unwrap_err();
        assert!(e.contains("not a volume of its own"), "{e}");

        // Reached through a link: refused, whatever it leads to.
        let link = d.with_extension("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&d, &link).unwrap();
        let w = Writes::file(&link.join("data.fennec"), None).unwrap();
        let e = plan(&w, other(), devices(&d).1).unwrap_err();
        assert!(e.contains("symbolic link"), "{e}");
        let _ = std::fs::remove_file(&link);

        // `/`, never.
        let w = Writes::file(Path::new("/data.fennec"), None).unwrap();
        assert_eq!(w.dir(), Path::new("/"));
        let root = std::fs::metadata("/").unwrap().dev().wrapping_add(1);
        let e = plan(&w, other(), root).unwrap_err();
        assert_eq!(e, "it is `/`");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_nodes_directory_hands_over_its_tenants_files() {
        let d = dir("node");
        for f in [
            "acme.fennec",
            "acme.fennec.lock",
            "acme.fennec.sync",
            "acme.quota",
            "acme.follows",
            "b-2_c.hooks",
            "b-2_c.hooks.writing",
        ] {
            touch(&d, f);
        }
        let w = Writes::node(&d, None).unwrap();
        assert_eq!(w.dir(), d);
        assert_eq!(plan(&w, other(), devices(&d).1).unwrap().len(), 8);
        // Not a tenant's name: no file of the node's. (Another name than
        // a tenant's here: macOS's file system would take `Acme.fennec`
        // for `acme.fennec`.)
        touch(&d, "Other.fennec");
        let e = plan(&w, other(), devices(&d).1).unwrap_err();
        assert!(e.contains("holds `Other.fennec`"), "{e}");
        let _ = std::fs::remove_dir_all(&d);

        // A node's directory not made yet: the one it is made in, if that
        // is empty but for `lost+found`.
        let d = dir("node-parent");
        std::fs::create_dir(d.join("lost+found")).unwrap();
        let w = Writes::node(&d.join("n1"), None).unwrap();
        assert_eq!(w.dir(), d);
        let got = plan(&w, other(), devices(&d).1).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].dir);
        touch(&d, "acme.fennec");
        assert!(plan(&w, other(), devices(&d).1).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_audit_log_beside_the_file_is_the_servers() {
        let d = dir("audit");
        touch(&d, "data.fennec");
        touch(&d, "audit.log");
        let file = d.join("data.fennec");
        let w = Writes::file(&file, None).unwrap();
        assert!(plan(&w, other(), devices(&d).1).is_err());
        let w = Writes::file(&file, Some(&d.join("audit.log"))).unwrap();
        assert_eq!(plan(&w, other(), devices(&d).1).unwrap().len(), 3);
        // Elsewhere, it is not this directory's.
        let w = Writes::file(&file, Some(Path::new("/var/log/audit.log"))).unwrap();
        assert!(plan(&w, other(), devices(&d).1).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_hand_over_changes_only_the_inode_it_found() {
        let d = dir("swap");
        touch(&d, "data.fennec");
        touch(&d, "data.fennec.lock");
        let w = Writes::file(&d.join("data.fennec"), None).unwrap();
        let got = plan(&w, other(), devices(&d).1).unwrap();
        // To the process's own user and group, which needs no root: the
        // path through the descriptors is the one root takes.
        let (_, me) = current();
        hand_over(&got, me).unwrap();

        // The lock file swapped for another between the look and the
        // change: refused. The other is made before the swap, so that it
        // is never given the inode the swap frees.
        let got = plan(&w, other(), devices(&d).1).unwrap();
        touch(&d, "elsewhere");
        std::fs::rename(d.join("elsewhere"), d.join("data.fennec.lock")).unwrap();
        let e = hand_over(&got, me).unwrap_err();
        assert!(e.contains("changed while it was handed over"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }
}

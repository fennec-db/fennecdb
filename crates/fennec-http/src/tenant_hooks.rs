//! A tenant node's webhook secrets (`docs/design/named-statements.md`,
//! *Where the secret lives*): each tenant's hooks in `<tenant>.hooks`
//! beside its file, kept in the node's memory beside the registry -- never
//! in the tenant's database, an archive, a log, a trace or an answer but
//! `hook-secrets`' -- so an event to `/t/<tenant>/_hooks/<name>` is
//! verified before the tenant is looked up, and a forged one opens none.
//!
//! **Bound to its file, as `.instance` is.** The sidecar's first line names
//! the file it was written for -- its device, inode and birth time
//! ([`crate::instance::binding`]) -- and every way of fennec's that makes,
//! removes or replaces a tenant's file puts it away first, synced: a
//! create, an install, a follow, a delete, a reclaim, `fennec restore`,
//! `backup --force` and `unseal` ([`Store::put_away`], [`put_away_beside`]).
//! One naming another file is never adopted: read as the node starts, or
//! held in memory as the tenant opens -- a file a restore put in its place
//! under a running node, whose memory no `fennec restore` reaches -- it is
//! put away and logged ([`Store::opened`]), and an event verified before
//! that open against the set put away is the one 401
//! ([`Store::is_current`]). So a set a deleted tenant left -- a failed
//! remove, a crash between two removals, a binary from before -- never
//! arms a later tenant of its name. The node's own renames -- a `compact`,
//! a checkpoint, a follower taking its primary's image -- carry the binding
//! to the file they made, from the binding the open found alone: at the
//! tenant's close and the node's shutdown, as `.instance`'s, at a
//! follower's hello, which comes once its image is in place, and at the
//! tick after a compact ([`Store::rebind`]). A promotion keeps it; a
//! follow drops it, and the router sends the set again once the copy
//! follows.
//!
//! **Set by the node's admin token alone.** `PUT
//! /_admin/tenants/<t>/hooks/<name>` (`{scheme, secrets, keep, tolerance,
//! key, max_body}`) and `DELETE` change one hook, refused (400) as a hooks
//! file's line is at a server's start -- an empty or malformed secret above
//! all -- naming the field, never its value, and refused (409) while the
//! tenant is frozen, a move under way; `GET /_admin/tenants/<t>/hooks`
//! lists names, schemes, bounds and when each was set, never a secret nor
//! anything computed from one. `GET /_admin/tenants/<t>/hook-secrets`
//! answers the set whole with its version, the one answer holding a
//! secret, for the router to copy onto a copy lacking it, and `PUT` there
//! installs one whole. A set's version is minted at random at each change
//! and carried by a copy: two copies holding one version hold one set. A
//! tenant holds [`MOST_HOOKS`] hooks at most, its set written out
//! [`MOST_BYTES`] at most, so a set always fits the head a move across
//! regions carries it in.
//!
//! The sidecar: `1 <device> <inode> <birth ns>`, `version <16 hex>`, then
//! the set as `hook-secrets` answers it, which [`Store::install`]'s reader
//! reads. A later layout raises the number, which this one reads as no
//! set, put away.

use crate::crypto::HmacKey;
use crate::hooks::{self, Check, Hook, KeyBy, Scheme, CEILING};
use crate::http::Request;
use crate::instance::{self, Binding};
use crate::tenants::Refused;
use fennec_core::prelude::Value;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

/// The sidecar's layout.
const LAYOUT: &str = "1";

/// The hooks a tenant holds at most.
pub const MOST_HOOKS: usize = 64;

/// The bytes a tenant's set takes at most as `hook-secrets` answers it: in
/// base64 a third more, 43 KiB, under the 64 KiB head a move across
/// regions carries it in (`Fennec-Hook-Secrets`) beside its others. A
/// secret's length has no bound of its own -- GitHub's is its user's
/// choice -- so the set's is the one held.
pub const MOST_BYTES: usize = 32 << 10;

/// A JSON body's fields as a hooks file's options: the field, and the
/// option word it is.
const OPTIONS: [(&str, &str); 4] = [
    ("keep", "keep"),
    ("tolerance", "tolerance"),
    ("key", "key"),
    ("max_body", "max-body"),
];

/// A set holding no hook, as `hook-secrets` answers it.
const NO_SET: &str = "{\"version\":null,\"hooks\":{}}";

/// A node's tenants' hooks, kept beside its registry.
pub struct Store {
    dir: PathBuf,
    /// The process's own key for a secret that is not there: a tenant
    /// missing, a hook missing, a second secret ([`hooks::verified`]).
    stand_in: [HmacKey; 2],
    /// The widest ceiling of any hook this node held since it started,
    /// [`CEILING`] at least: every event's HMACs run over its body up to
    /// it, whatever tenant and hook its path names.
    widest: AtomicUsize,
    /// The node's `--max-body`, which a hook's own ceiling stays under.
    max_body: AtomicUsize,
    sets: RwLock<HashMap<String, Entry>>,
    /// Held across every change of a set -- its sidecar written or unlinked
    /// and synced, then its entry swapped -- so two never interleave
    /// ([`Store::changing`]). The map's write lock is taken for the swap
    /// alone, after the sync: a read of the sets -- an event's `verify`, a
    /// listing's `armed`, every tenant's -- never waits on one tenant's
    /// fsync, which it did while the map's lock was held across it.
    writing: Mutex<()>,
}

/// [`Store::writing`] held: what a change of a set takes before it reads
/// the entry it changes.
type Changing<'a> = MutexGuard<'a, ()>;

struct Entry {
    set: Arc<Set>,
    /// The file the sidecar names.
    bound: Binding,
    /// The open of the tenant's file that found it bound to that file, or
    /// that set it while open ([`Store::opened`]): only that open's own
    /// renames carry it ([`Store::rebind`]). 0 for none.
    held: u64,
}

/// A tenant's hooks, as one version.
pub struct Set {
    version: u64,
    hooks: Vec<Hook>,
    kept: Vec<Kept>,
}

/// A hook as its tenant's set keeps it: what its `PUT` gave, to write the
/// sidecar and the export from.
#[derive(Clone)]
struct Kept {
    /// When it was set, in milliseconds.
    set_ms: i64,
    /// Its secrets as its sender shows them.
    secrets: Vec<String>,
    /// Its options as given: the option word and its value, one word.
    opts: Vec<(&'static str, String)>,
}

/// An event that verified against a tenant's set: what `route_tenant`
/// hands the take, the set held whatever a `PUT` makes of it meanwhile.
pub struct Verified {
    set: Arc<Set>,
    at: usize,
    /// `hook.at`, in milliseconds.
    pub at_ms: Option<i64>,
}

impl Verified {
    pub fn hook(&self) -> &Hook {
        &self.set.hooks[self.at]
    }
}

impl Store {
    /// The node's store over `dir`, each `<tenant>.hooks` there read and
    /// held to the file it names: one naming another, or none, put away and
    /// logged, and a sidecar a crash left half written removed. Refused
    /// where the system's randomness cannot be read: a stand-in key is
    /// never the clock's.
    pub fn open(dir: &Path) -> io::Result<Store> {
        let key = hooks::stand_in().map_err(io::Error::other)?;
        let store = Store {
            dir: dir.to_path_buf(),
            stand_in: [key.clone(), key],
            widest: AtomicUsize::new(CEILING),
            max_body: AtomicUsize::new(usize::MAX),
            sets: RwLock::new(HashMap::new()),
            writing: Mutex::new(()),
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(store);
        };
        for e in entries.flatten() {
            let file = e.file_name();
            let Some(file) = file.to_str() else {
                continue;
            };
            // Cut short by a crash before its rename: secrets no set holds,
            // which nothing would take away after.
            if let Some(name) = file.strip_suffix(".hooks.writing") {
                if crate::tenants::check_name(name).is_ok() {
                    let _ = std::fs::remove_file(e.path());
                }
                continue;
            }
            let Some(name) = file.strip_suffix(".hooks") else {
                continue;
            };
            if crate::tenants::check_name(name).is_err() {
                continue;
            }
            match store.read(name) {
                Ok(entry) => {
                    store.widen(&entry.set);
                    store.write_map().insert(name.to_string(), entry);
                }
                Err(why) => store.forget(&store.changing(), name, why),
            }
        }
        if !store.read_map().is_empty() {
            hooks::serving();
        }
        Ok(store)
    }

    /// Holds a hook's own ceiling under the node's `--max-body`.
    pub fn set_max_body(&self, max_body: usize) {
        self.max_body.store(max_body, Ordering::Relaxed);
    }

    fn read_map(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, Entry>> {
        self.sets.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether any tenant holds a set.
    pub(crate) fn any(&self) -> bool {
        !self.read_map().is_empty()
    }

    /// Whether tenant `name` holds a set.
    pub(crate) fn holds(&self, name: &str) -> bool {
        self.read_map().contains_key(name)
    }

    /// For the swap of an entry alone, under [`Self::changing`], never
    /// across a write or a sync of a sidecar.
    fn write_map(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, Entry>> {
        self.sets.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Taken before a change of a set reads the entry it changes, and held
    /// through its sidecar's sync and its swap in memory.
    fn changing(&self) -> Changing<'_> {
        self.writing.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.hooks"))
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.fennec"))
    }

    fn widen(&self, set: &Set) {
        for h in &set.hooks {
            self.widest.fetch_max(h.ceiling, Ordering::Relaxed);
        }
    }

    /// The event `req` sent to `tenant`'s hook `hook`, verified at `now_ms`
    /// against the set in memory, or the first check it failed: the same
    /// work whatever is missing -- the tenant, its set, the hook -- and no
    /// tenant opened.
    pub fn verify(
        &self,
        tenant: &str,
        hook: &str,
        req: &Request,
        now_ms: i64,
    ) -> Result<Verified, Check> {
        let set = self.read_map().get(tenant).map(|e| Arc::clone(&e.set));
        let at = set
            .as_ref()
            .and_then(|s| s.hooks.iter().position(|h| h.name == hook));
        let found = set.as_ref().zip(at).map(|(s, i)| &s.hooks[i]);
        let widest = self.widest.load(Ordering::Relaxed);
        let at_ms = hooks::verified(found, &self.stand_in, widest, req, now_ms)?;
        match (set, at) {
            (Some(set), Some(at)) => Ok(Verified { set, at, at_ms }),
            _ => Err(Check::Hook),
        }
    }

    /// Whether `v`, verified before its tenant was looked up, verified
    /// against the set `tenant` holds now: not where the tenant's open put
    /// that set away -- one naming another file than the one opened -- nor
    /// where a change replaced it since, either of which is the one 401.
    pub fn is_current(&self, tenant: &str, v: &Verified) -> bool {
        (self.read_map().get(tenant)).is_some_and(|e| Arc::ptr_eq(&e.set, &v.set))
    }

    /// Whether `tenant` holds a secret for hook `hook`: what the log says
    /// as a tenant opens.
    pub fn armed(&self, tenant: &str, hook: &str) -> bool {
        self.read_map()
            .get(tenant)
            .is_some_and(|e| e.set.hooks.iter().any(|h| h.name == hook))
    }

    /// The sidecar read and held to the file it names, or why not.
    fn read(&self, name: &str) -> Result<Entry, &'static str> {
        let text = std::fs::read_to_string(self.path(name)).map_err(|_| "could not be read")?;
        let mut lines = text.splitn(3, '\n');
        let mut first = lines.next().unwrap_or("").split(' ');
        if first.next() != Some(LAYOUT) {
            return Err("are of a layout this node does not read");
        }
        let mut n = || first.next()?.parse::<u64>().ok();
        let (Some(dev), Some(ino), Some(born)) = (n(), n(), n()) else {
            return Err("name no file");
        };
        let bound = (dev, ino, born);
        match instance::binding(&self.file(name)) {
            Ok(now) if now == bound => {}
            Ok(_) => return Err("name another file than the tenant's"),
            Err(_) => return Err("lie beside no file of the tenant"),
        }
        // The version line is a reader's: the set holds its own.
        let set = lines.nth(1).unwrap_or("");
        match self.set_of(set.as_bytes(), usize::MAX) {
            Ok(Some(set)) => Ok(Entry {
                set: Arc::new(set),
                bound,
                held: 0,
            }),
            Ok(None) => Err("hold no hook"),
            Err(_) => Err("hold a set that does not read"),
        }
    }

    /// The sidecar's text for a set of `version`, `json` as it is
    /// exported, bound to `bound`.
    fn text(bound: Binding, version: u64, json: &str) -> String {
        format!(
            "{LAYOUT} {} {} {}\nversion {version:016x}\n{json}\n",
            bound.0, bound.1, bound.2
        )
    }

    /// Writes `name`'s sidecar beside and renames it into place, synced: a
    /// set a router was told is held must survive a power loss, and a
    /// binding carried past a rename of the node's own lost to one would
    /// name the file before, the set put away at the next start. Readable
    /// by the node's user alone.
    fn write(&self, name: &str, text: &str) -> io::Result<()> {
        let path = self.path(name);
        let tmp = self.writing(name);
        let written = (|| {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode_private()
                .open(&tmp)?;
            use std::io::Write;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, &path)?;
            fennec_core::fs::sync_dir(&path).map_err(|e| io::Error::other(e.to_string()))
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written
    }

    fn writing(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.hooks.writing"))
    }

    /// Unlinks `name`'s sidecar, the directory synced where there was one,
    /// and one a crash left half written.
    fn unlink(&self, name: &str) -> io::Result<()> {
        remove(&self.writing(name))?;
        remove(&self.path(name))
    }

    /// Puts `name`'s set away from disk, then memory, saying why: one that
    /// does not read, or names another file than the tenant's.
    fn forget(&self, _: &Changing<'_>, name: &str, why: &str) {
        crate::log!(
            "tenant `{name}`'s hook secrets {why}: put away, and its hooks answer 401 until \
             they are set again"
        );
        if let Err(e) = self.unlink(name) {
            crate::log!("tenant `{name}`'s hook secrets could not be put away: {e}");
        }
        self.write_map().remove(name);
    }

    /// Puts `name`'s set away, synced, before its file is made, removed or
    /// replaced -- a create, an install, a follow, a delete, a reclaim --
    /// and out of memory: a later file at the place is armed by no set
    /// another left. Asked under the tenant's slot. A node holding no set
    /// for it pays a lock, an unlink that finds nothing and a map's look.
    pub(crate) fn put_away(&self, name: &str) -> io::Result<()> {
        let _changing = self.changing();
        self.unlink(name)?;
        self.write_map().remove(name);
        Ok(())
    }

    /// Tenant `name`'s file opened, open `open` of it, found as `at` before
    /// anything here could rename it: the set is that open's where it names
    /// that file, and put away otherwise -- one a file put in its place
    /// left, `fennec restore`, `backup --force` or `unseal` under the
    /// running node, or a hand, whose memory no such tool reaches. Asked
    /// under the tenant's slot. A tenant holding no set pays a map's look.
    pub(crate) fn opened(&self, name: &str, at: Option<Binding>, open: u64) {
        if !self.holds(name) {
            return;
        }
        let changing = self.changing();
        {
            let mut map = self.write_map();
            let Some(e) = map.get_mut(name) else {
                return;
            };
            if Some(e.bound) == at {
                e.held = open;
                return;
            }
        }
        self.forget(&changing, name, "name another file than the one opened");
    }

    /// Before a change or a read of `name`'s set through the admin routes,
    /// under its slot, `open` the tenant's open (0 where it is closed): a
    /// set naming another file than the one there -- a restore over the
    /// closed tenant -- is put away first, never changed, exported or
    /// carried onto the file that took its place.
    pub(crate) fn settle(&self, name: &str, open: u64) {
        let Some((bound, held)) = self.read_map().get(name).map(|e| (e.bound, e.held)) else {
            return;
        };
        let mine = match open {
            0 => instance::binding(&self.file(name)).ok() == Some(bound),
            open => held == open,
        };
        if !mine {
            self.forget(
                &self.changing(),
                name,
                "name another file than the tenant's",
            );
        }
    }

    /// Carries `name`'s set to its file as it stands now, where the node's
    /// own rename -- a compact, a checkpoint, an image a follower took --
    /// made another of it: at a close, a shutdown, a follower's hello and
    /// the tick after a compact ([`crate::tenants::Tenants::rebind_hooks`]),
    /// each for `open`, the open of the file whose renames these are. A set
    /// that open did not find bound to its file, or set, is not carried; nor
    /// one replaced between the `stat` and the write -- a delete, a create,
    /// a `PUT` -- which its own writing bound. Synced: a binding lost to a
    /// power loss names the file before, which the next start puts away. A
    /// tenant holding no set pays a map's look.
    pub(crate) fn rebind(&self, name: &str, open: u64) {
        if !self.holds(name) {
            return;
        }
        // Every change takes it before it reads its entry: the entry read
        // here is the one swapped after the sync.
        let _changing = self.changing();
        let entry = (self.read_map().get(name))
            .filter(|e| e.held == open)
            .map(|e| (Arc::clone(&e.set), e.bound));
        let Some((set, bound)) = entry else {
            return;
        };
        let Ok(now) = instance::binding(&self.file(name)) else {
            return;
        };
        if bound == now {
            return;
        }
        let text = Self::text(now, set.version, &exported(&set));
        match self.write(name, &text) {
            Ok(()) => {
                if let Some(e) = self.write_map().get_mut(name) {
                    e.bound = now;
                }
            }
            Err(e) => {
                crate::log!("tenant `{name}`'s hook secrets were not bound to its file again: {e}")
            }
        }
    }

    /// Replaces `name`'s set with `set` -- none, every hook taken away --
    /// bound to its file, durably: the sidecar first, then memory. Asked
    /// under the tenant's slot, its file there, `open` its open (0, none).
    fn replace(&self, name: &str, set: Option<Set>, open: u64) -> Result<Option<u64>, Refused> {
        let failed = |e: io::Error| {
            Refused(
                500,
                format!("tenant `{name}`'s hook secrets could not be written: {e}"),
            )
        };
        let Some(set) = set else {
            let _changing = self.changing();
            self.unlink(name).map_err(failed)?;
            self.write_map().remove(name);
            return Ok(None);
        };
        if set.hooks.len() > MOST_HOOKS {
            return Err(Refused(
                400,
                format!("a tenant holds {MOST_HOOKS} hooks at most"),
            ));
        }
        let json = exported(&set);
        if json.len() > MOST_BYTES {
            return Err(Refused(
                400,
                format!(
                    "a tenant's hook secrets take {} KiB at most, written out as hook-secrets \
                     answers them",
                    MOST_BYTES >> 10
                ),
            ));
        }
        let bound = instance::binding(&self.file(name)).map_err(failed)?;
        let _changing = self.changing();
        self.write(name, &Self::text(bound, set.version, &json))
            .map_err(failed)?;
        self.widen(&set);
        hooks::serving();
        let version = set.version;
        self.write_map().insert(
            name.to_string(),
            Entry {
                set: Arc::new(set),
                bound,
                held: open,
            },
        );
        Ok(Some(version))
    }

    fn current(&self, name: &str) -> Option<Arc<Set>> {
        self.read_map().get(name).map(|e| Arc::clone(&e.set))
    }

    /// `name`'s set but its hook `hook`: the hooks and what each kept.
    fn others(&self, name: &str, hook: &str) -> (Vec<Hook>, Vec<Kept>) {
        let (mut hooks, mut kept) = (Vec::new(), Vec::new());
        if let Some(old) = self.current(name) {
            for (h, k) in old.hooks.iter().zip(&old.kept) {
                if h.name != hook {
                    hooks.push(h.clone());
                    kept.push(k.clone());
                }
            }
        }
        (hooks, kept)
    }

    /// `PUT /_admin/tenants/<t>/hooks/<hook>`: the hook set from `body` in
    /// place of the one of its name, the set's new version answered.
    pub(crate) fn put_hook(
        &self,
        name: &str,
        hook: &str,
        body: &[u8],
        open: u64,
    ) -> Result<u64, Refused> {
        let given = self.given(hook, &object(body)?, false)?;
        let (mut hooks, mut kept) = self.others(name, hook);
        let max_body = self.max_body.load(Ordering::Relaxed);
        let (h, k) = self.made(hook, given, &hooks, now_ms(), max_body)?;
        hooks.push(h);
        kept.push(k);
        let set = Set {
            version: fresh_version(),
            hooks,
            kept,
        };
        self.replace(name, Some(set), open).map(|v| v.unwrap_or(0))
    }

    /// `DELETE /_admin/tenants/<t>/hooks/<hook>`: the set's version after,
    /// `None` where no hook is left.
    pub(crate) fn drop_hook(
        &self,
        name: &str,
        hook: &str,
        open: u64,
    ) -> Result<Option<u64>, Refused> {
        if !self.armed(name, hook) {
            return Err(Refused(
                404,
                format!("tenant `{name}` holds no secret for hook `{hook}`"),
            ));
        }
        let (hooks, kept) = self.others(name, hook);
        let set = (!hooks.is_empty()).then(|| Set {
            version: fresh_version(),
            hooks,
            kept,
        });
        self.replace(name, set, open)
    }

    /// `PUT /_admin/tenants/<t>/hook-secrets`: a set installed whole, its
    /// version kept -- what the router copies onto a copy lacking it.
    pub(crate) fn install(
        &self,
        name: &str,
        body: &[u8],
        open: u64,
    ) -> Result<Option<u64>, Refused> {
        let set = self.set_of(body, self.max_body.load(Ordering::Relaxed))?;
        self.replace(name, set, open)
    }

    /// A set as `hook-secrets` answers it, each hook refused as a hook's
    /// `PUT` is, each hook's ceiling held under `max_body`: `None` where it
    /// holds no hook. The one reader of a set, a sidecar's too.
    fn set_of(&self, body: &[u8], max_body: usize) -> Result<Option<Set>, Refused> {
        let mut version = None;
        let mut listed = None;
        for (k, v) in object(body)? {
            match (k.as_str(), v) {
                ("version", Value::Null) => {}
                ("version", Value::Text(t)) => {
                    version = Some(
                        u64::from_str_radix(&t, 16)
                            .ok()
                            .filter(|v| *v != 0 && t.len() == 16)
                            .ok_or_else(|| Refused(400, "`version` is 16 hex digits".into()))?,
                    )
                }
                ("hooks", Value::Object(m)) => listed = Some(m),
                ("tenant", _) => {}
                (k, _) => {
                    return Err(Refused(
                        400,
                        format!("a set of hook secrets holds `version` and `hooks`, not `{k}`"),
                    ))
                }
            }
        }
        let mut hooks: Vec<Hook> = Vec::new();
        let mut kept = Vec::new();
        for (hook, v) in listed.unwrap_or_default() {
            let Value::Object(fields) = v else {
                return Err(Refused(400, format!("hook `{hook}`: an object")));
            };
            let given = self.given(&hook, &fields, true)?;
            let set_ms = given.set_ms.unwrap_or_else(now_ms);
            let (h, k) = self.made(&hook, given, &hooks, set_ms, max_body)?;
            hooks.push(h);
            kept.push(k);
        }
        if hooks.is_empty() {
            return Ok(None);
        }
        let version =
            version.ok_or_else(|| Refused(400, "a set of hooks names its `version`".into()))?;
        Ok(Some(Set {
            version,
            hooks,
            kept,
        }))
    }

    /// The hook `given` makes, beside `before`: refused as a hooks file's
    /// line is at a server's start, naming the hook and the field -- the
    /// line's own reader judging it, each secret named by its place.
    fn made(
        &self,
        hook: &str,
        given: Given,
        before: &[Hook],
        set_ms: i64,
        max_body: usize,
    ) -> Result<(Hook, Kept), Refused> {
        let mut words = vec![hook, given.scheme.name(), "0", "1"];
        words.truncate(2 + given.secrets.len());
        for (word, v) in &given.opts {
            words.extend([*word, v.as_str()]);
        }
        let secret = |w: &str| {
            (w.parse::<usize>().ok())
                .and_then(|i| given.secrets.get(i))
                .map(|s| s.as_bytes().to_vec())
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))
        };
        let h = hooks::hook_of(&words, before, &secret, max_body, &self.stand_in[0])
            .map_err(|why| Refused(400, format!("hook `{hook}`: {why}")))?;
        let k = Kept {
            set_ms,
            secrets: given.secrets,
            opts: given.opts,
        };
        Ok((h, k))
    }

    /// A hook's fields read: its scheme, its secrets, its options and, in a
    /// set installed whole, when it was set. Each value is the field's, of
    /// the field's kind, one word; anything else refused naming the field,
    /// never its value.
    fn given(&self, hook: &str, fields: &[(String, Value)], whole: bool) -> Result<Given, Refused> {
        let refused = |why: String| Refused(400, format!("hook `{hook}`: {why}"));
        let (mut scheme, mut secrets, mut opts, mut set_ms) = (None, None, Vec::new(), None);
        for (k, v) in fields {
            match (k.as_str(), v) {
                ("scheme", Value::Text(s)) => {
                    scheme = Some(Scheme::read(s).ok_or_else(|| {
                        refused("`scheme` is `stripe`, `github` or `standard`".into())
                    })?)
                }
                ("secrets", Value::List(items)) => {
                    let texts: Option<Vec<String>> = items
                        .iter()
                        .map(|i| match i {
                            Value::Text(t) => Some(t.to_string()),
                            _ => None,
                        })
                        .collect();
                    secrets = Some(texts.ok_or_else(|| {
                        refused("`secrets` is a list of texts, as their sender shows them".into())
                    })?);
                }
                ("set", Value::Int(ms)) if whole => set_ms = Some(*ms),
                (field, Value::Null) if OPTIONS.iter().any(|(f, _)| *f == field) => {}
                (field, Value::Text(t)) if OPTIONS.iter().any(|(f, _)| *f == field) => {
                    let word = OPTIONS.iter().find(|(f, _)| *f == field).expect("listed").1;
                    if t.is_empty() || t.contains(char::is_whitespace) {
                        return Err(refused(format!(
                            "`{field}` is one word: 30d, 5m, body, 64k"
                        )));
                    }
                    opts.push((word, t.to_string()));
                }
                (field, _) => {
                    return Err(refused(format!(
                        "a hook is `scheme`, `secrets`, `keep`, `tolerance`, `key` and \
                         `max_body`, each a text but `secrets`, a list: not `{field}` as sent"
                    )))
                }
            }
        }
        let scheme = scheme.ok_or_else(|| refused("name its `scheme`".into()))?;
        let secrets = secrets.unwrap_or_default();
        if !(1..=2).contains(&secrets.len()) {
            return Err(refused(
                "`secrets` holds one secret, or two while one rolls".into(),
            ));
        }
        Ok(Given {
            scheme,
            secrets,
            opts,
            set_ms,
        })
    }

    /// `GET /_admin/tenants/<t>/hooks`: every hook's name, scheme, bounds
    /// and when it was set -- never a secret, nor anything computed from
    /// one, a fingerprint of a secret its user chose being a test of every
    /// guess.
    pub(crate) fn listing(&self, name: &str) -> String {
        let set = self.current(name);
        let mut out = String::from("{\"tenant\":");
        fennec_core::json::escape_into(&mut out, name);
        out.push_str(",\"version\":");
        push_version(&mut out, set.as_ref().map(|s| s.version));
        out.push_str(",\"hooks\":[");
        if let Some(set) = &set {
            for (i, (h, k)) in set.hooks.iter().zip(&set.kept).enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str("{\"name\":");
                fennec_core::json::escape_into(&mut out, &h.name);
                out.push_str(&format!(
                    ",\"scheme\":\"{}\",\"secrets\":{},\"keep_ms\":{},\"tolerance_ms\":{},\
                     \"key\":{},\"max_body\":{},\"set\":{}}}",
                    h.scheme.name(),
                    h.secrets,
                    h.keep_ms,
                    h.tolerance_ms,
                    match (h.scheme, h.key) {
                        (Scheme::Github, KeyBy::Body) => "\"body\"",
                        (Scheme::Github, KeyBy::Delivery) => "\"delivery\"",
                        _ => "null",
                    },
                    h.ceiling,
                    k.set_ms
                ));
            }
        }
        out.push_str("]}");
        out
    }

    /// `GET /_admin/tenants/<t>/hook-secrets`: the set whole with its
    /// version, the one answer holding a secret.
    pub(crate) fn export(&self, name: &str) -> String {
        self.current(name)
            .map_or_else(|| NO_SET.to_string(), |s| exported(&s))
    }

    /// The version `name`'s set is at: `None` where it holds none.
    pub fn version(&self, name: &str) -> Option<u64> {
        self.read_map().get(name).map(|e| e.set.version)
    }

    /// Says, as tenant `name` opens, each hook its statements take that it
    /// holds no secret for: its events are answered 401 until one is set.
    /// A tenant whose statements take no hook pays a walk of them.
    pub(crate) fn log_unarmed(&self, name: &str, db: &fennec_core::prelude::Database) {
        let statements = db.statements();
        let mut taken: Vec<(&str, usize)> = Vec::new();
        for s in statements.iter() {
            let Some(h) = s.hook.as_deref() else {
                continue;
            };
            match taken.iter_mut().find(|(t, _)| *t == h) {
                Some((_, n)) => *n += 1,
                None => taken.push((h, 1)),
            }
        }
        for (hook, n) in taken {
            if !self.armed(name, hook) {
                crate::log!(
                    "tenant `{name}`'s hook `{hook}`: consumed by {n} statement{}, no secret -- \
                     its events are answered 401 until one is set (PUT \
                     /_shard/tenants/{name}/hooks/{hook})",
                    if n == 1 { "" } else { "s" }
                );
            }
        }
    }
}

/// A hook's fields as a `PUT` or an installed set gives them.
struct Given {
    scheme: Scheme,
    secrets: Vec<String>,
    opts: Vec<(&'static str, String)>,
    set_ms: Option<i64>,
}

fn object(body: &[u8]) -> Result<Vec<(String, Value)>, Refused> {
    let text =
        std::str::from_utf8(body).map_err(|_| Refused(400, "the body is not UTF-8".into()))?;
    match fennec_core::json::parse_json(text) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(Refused(400, "the body is a JSON object".into())),
    }
}

/// `set` as `hook-secrets` answers it, and its sidecar holds it.
fn exported(set: &Set) -> String {
    let mut out = String::from("{\"version\":");
    push_version(&mut out, Some(set.version));
    out.push_str(",\"hooks\":{");
    for (i, (h, k)) in set.hooks.iter().zip(&set.kept).enumerate() {
        if i > 0 {
            out.push(',');
        }
        fennec_core::json::escape_into(&mut out, &h.name);
        out.push_str(&format!(
            ":{{\"scheme\":\"{}\",\"secrets\":[",
            h.scheme.name()
        ));
        for (j, s) in k.secrets.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            fennec_core::json::escape_into(&mut out, s);
        }
        out.push(']');
        for (word, v) in &k.opts {
            let field = OPTIONS
                .iter()
                .find(|(_, w)| w == word)
                .map_or(*word, |(f, _)| f);
            out.push_str(&format!(",\"{field}\":"));
            fennec_core::json::escape_into(&mut out, v);
        }
        out.push_str(&format!(",\"set\":{}}}", k.set_ms));
    }
    out.push_str("}}");
    out
}

fn push_version(out: &mut String, v: Option<u64>) {
    match v {
        Some(v) => out.push_str(&format!("\"{v:016x}\"")),
        None => out.push_str("null"),
    }
}

/// A set's version as a router records it: 16 hex digits, or `None`.
pub fn version_text(v: Option<u64>) -> String {
    v.map_or_else(String::new, |v| format!("{v:016x}"))
}

/// A version minted at random, never 0: two sets the node made are two
/// versions, so a copy holding one holds that set. A version names a set
/// and guards nothing, so it is an id's draw (`id_u64`), unique without
/// the system's randomness too.
fn fresh_version() -> u64 {
    crate::crypto::id_u64().max(1)
}

fn now_ms() -> i64 {
    fennec_core::time::now_ms().unwrap_or(0)
}

/// Removes `path`, its directory synced where it was there.
fn remove(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => fennec_core::fs::sync_dir(path).map_err(|e| io::Error::other(e.to_string())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// A file opened readable by its owner alone, where the system has modes:
/// the sidecar holds secrets.
trait Private {
    fn mode_private(&mut self) -> &mut Self;
}

impl Private for std::fs::OpenOptions {
    #[cfg(unix)]
    fn mode_private(&mut self) -> &mut Self {
        use std::os::unix::fs::OpenOptionsExt;
        self.mode(0o600)
    }
    #[cfg(not(unix))]
    fn mode_private(&mut self) -> &mut Self {
        self
    }
}

/// Puts away the hook secrets beside a database file a way other than its
/// node renames over -- `fennec restore`, `fennec backup --force`, `fennec
/// unseal`, as [`instance::put_away_beside`] puts its identity away --
/// synced, one a crash left half written too: a restore brings a tenant's
/// statements back and no secret. A node running beside holds the set in
/// its memory still, which the tenant's next open puts away
/// ([`Store::opened`]).
pub fn put_away_beside(file: &Path) -> io::Result<()> {
    for f in [file.to_path_buf(), fennec_core::fs::resolved(file)] {
        let name = f.file_stem().and_then(|n| n.to_str());
        if let (Some(dir), Some(name), true) =
            (f.parent(), name, f.extension() == Some("fennec".as_ref()))
        {
            remove(&dir.join(format!("{name}.hooks.writing")))?;
            remove(&dir.join(format!("{name}.hooks")))?;
        }
    }
    Ok(())
}

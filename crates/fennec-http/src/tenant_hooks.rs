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
//! create, an install, a follow, a delete, a reclaim, `fennec restore` and
//! `backup --force` ([`Store::put_away`], [`put_away_beside`]). One naming
//! another file is never adopted: read as the node starts, it is put away
//! and logged, so a set a deleted tenant left -- a failed remove, a crash
//! between two removals, a binary from before -- never arms a later tenant
//! of its name. The node's own renames -- a `compact`, a checkpoint, a
//! follower taking its primary's image -- carry the binding to the file
//! they made: at the tenant's close and the node's shutdown, as
//! `.instance`'s, and at a follower's hello, which comes once its image is
//! in place ([`Store::rebind`]). A promotion keeps it; a follow drops it,
//! and the router sends the set again once the copy follows.
//!
//! **Set by the node's admin token alone.** `PUT
//! /_admin/tenants/<t>/hooks/<name>` (`{scheme, secrets, keep, tolerance,
//! key, max_body}`) and `DELETE` change one hook, refused (400) as a hooks
//! file's line is at a server's start -- an empty or malformed secret above
//! all -- naming the field, never its value; `GET /_admin/tenants/<t>/hooks`
//! lists names, schemes, bounds and when each was set, never a secret nor
//! anything computed from one. `GET /_admin/tenants/<t>/hook-secrets`
//! answers the set whole with its version, the one answer holding a
//! secret, for the router to copy onto a copy lacking it, and `PUT` there
//! installs one whole. A set's version is minted at random at each change
//! and carried by a copy: two copies holding one version hold one set.
//!
//! The sidecar: `1 <device> <inode> <birth ns>`, `version <16 hex>`, then a
//! line a hook -- when it was set, then a hooks file's line with each
//! secret's bytes in base64 (`b64:`) in its file's place. A later layout
//! raises the number, which this one reads as no set, put away.

use crate::crypto::{b64_decode, b64_encode, HmacKey};
use crate::hooks::{self, Check, Hook, KeyBy, Scheme, CEILING};
use crate::http::Request;
use crate::instance::{self, Binding};
use crate::tenants::Refused;
use fennec_core::prelude::Value;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

/// The sidecar's layout.
const LAYOUT: &str = "1";

/// A JSON body's fields as a hooks file's options: the field, and the
/// option word it is.
const OPTIONS: [(&str, &str); 4] = [
    ("keep", "keep"),
    ("tolerance", "tolerance"),
    ("key", "key"),
    ("max_body", "max-body"),
];

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
}

struct Entry {
    set: Arc<Set>,
    /// The file the sidecar names.
    bound: Binding,
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
    /// logged. Refused where the system's randomness cannot be read: a
    /// stand-in key is never the clock's.
    pub fn open(dir: &Path) -> io::Result<Store> {
        let key = hooks::stand_in().map_err(io::Error::other)?;
        let store = Store {
            dir: dir.to_path_buf(),
            stand_in: [key.clone(), key],
            widest: AtomicUsize::new(CEILING),
            max_body: AtomicUsize::new(usize::MAX),
            sets: RwLock::new(HashMap::new()),
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(store);
        };
        for e in entries.flatten() {
            let file = e.file_name();
            let Some(name) = file.to_str().and_then(|f| f.strip_suffix(".hooks")) else {
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
                Err(why) => {
                    crate::log!(
                        "tenant `{name}`'s hook secrets {why}: put away, and its hooks answer \
                         401 until they are set again"
                    );
                    if let Err(e) = store.unlink(name) {
                        crate::log!("tenant `{name}`'s hook secrets could not be put away: {e}");
                    }
                }
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

    fn write_map(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, Entry>> {
        self.sets.write().unwrap_or_else(|e| e.into_inner())
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

    /// Whether `tenant` holds a secret for hook `hook`: a statement's
    /// `armed`, and what the log says as a tenant opens.
    pub fn armed(&self, tenant: &str, hook: &str) -> bool {
        self.read_map()
            .get(tenant)
            .is_some_and(|e| e.set.hooks.iter().any(|h| h.name == hook))
    }

    /// The sidecar read and held to the file it names, or why not.
    fn read(&self, name: &str) -> Result<Entry, &'static str> {
        let text = std::fs::read_to_string(self.path(name)).map_err(|_| "could not be read")?;
        let mut lines = text.lines();
        let mut first = lines.next().unwrap_or("").split(' ');
        if first.next() != Some(LAYOUT) {
            return Err("are of a layout this node does not read");
        }
        let mut n = || first.next()?.parse::<u64>().ok();
        let bound = (n(), n(), n());
        let (Some(dev), Some(ino), Some(born)) = bound else {
            return Err("name no file");
        };
        let bound = (dev, ino, born);
        match instance::binding(&self.file(name)) {
            Ok(now) if now == bound => {}
            Ok(_) => return Err("name another file than the tenant's"),
            Err(_) => return Err("lie beside no file of the tenant"),
        }
        let version = (lines.next())
            .and_then(|l| l.strip_prefix("version "))
            .and_then(|v| u64::from_str_radix(v.trim(), 16).ok())
            .filter(|v| *v != 0)
            .ok_or("hold no version")?;
        let mut hooks: Vec<Hook> = Vec::new();
        let mut kept = Vec::new();
        for line in lines.filter(|l| !l.trim().is_empty()) {
            let words: Vec<&str> = line.split_ascii_whitespace().collect();
            let Some((set_ms, words)) = words.split_first() else {
                continue;
            };
            let set_ms = set_ms
                .parse::<i64>()
                .map_err(|_| "hold a line that does not read")?;
            let (hook, k) = self
                .line_of(words, &hooks, set_ms, usize::MAX)
                .map_err(|_| "hold a line that does not read")?;
            hooks.push(hook);
            kept.push(k);
        }
        if hooks.is_empty() {
            return Err("hold no hook");
        }
        Ok(Entry {
            set: Arc::new(Set {
                version,
                hooks,
                kept,
            }),
            bound,
        })
    }

    /// The hook a sidecar's line makes, beside `before`: a hooks file's
    /// line, each secret its base64.
    fn line_of(
        &self,
        words: &[&str],
        before: &[Hook],
        set_ms: i64,
        max_body: usize,
    ) -> Result<(Hook, Kept), hooks::Why> {
        let secret = |w: &str| {
            (w.strip_prefix("b64:").and_then(b64_decode))
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))
        };
        let hook = hooks::hook_of(words, before, &secret, max_body, &self.stand_in[0])?;
        let files = hook.secrets;
        let secrets = words[2..2 + files]
            .iter()
            .map(|w| secret(w).map(|b| String::from_utf8_lossy(&b).into_owned()))
            .collect::<io::Result<Vec<_>>>()
            .map_err(|_| "a secret does not read")?;
        let mut opts = Vec::new();
        let mut rest = words[2 + files..].iter();
        while let (Some(k), Some(v)) = (rest.next(), rest.next()) {
            if let Some((_, word)) = OPTIONS.iter().find(|(_, w)| w == k) {
                opts.push((*word, v.to_string()));
            }
        }
        Ok((
            hook,
            Kept {
                set_ms,
                secrets,
                opts,
            },
        ))
    }

    /// The sidecar's text for `set`, bound to `bound`.
    fn text(set: &Set, bound: Binding) -> String {
        let mut s = format!(
            "{LAYOUT} {} {} {}\nversion {:016x}\n",
            bound.0, bound.1, bound.2, set.version
        );
        for (h, k) in set.hooks.iter().zip(&set.kept) {
            s.push_str(&format!("{} {} {}", k.set_ms, h.name, h.scheme.name()));
            for secret in &k.secrets {
                s.push_str(" b64:");
                s.push_str(&b64_encode(secret.as_bytes()));
            }
            for (word, v) in &k.opts {
                s.push_str(&format!(" {word} {v}"));
            }
            s.push('\n');
        }
        s
    }

    /// Writes `name`'s sidecar beside and renames it into place, synced: a
    /// set a router was told is held must survive a power loss, and a
    /// binding carried past a rename of the node's own lost to one would
    /// name the file before, the set put away at the next start. Readable
    /// by the node's user alone.
    fn write(&self, name: &str, set: &Set, bound: Binding) -> io::Result<()> {
        let path = self.path(name);
        let tmp = self.dir.join(format!("{name}.hooks.writing"));
        let text = Self::text(set, bound);
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

    /// Unlinks `name`'s sidecar, the directory synced where there was one.
    fn unlink(&self, name: &str) -> io::Result<()> {
        let path = self.path(name);
        match std::fs::remove_file(&path) {
            Ok(()) => fennec_core::fs::sync_dir(&path).map_err(|e| io::Error::other(e.to_string())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Puts `name`'s set away, synced, before its file is made, removed or
    /// replaced -- a create, an install, a follow, a delete, a reclaim --
    /// and out of memory: a later file at the place is armed by no set
    /// another left. Asked under the tenant's slot. A node holding no set
    /// for it pays a map's look and an unlink that finds nothing.
    pub(crate) fn put_away(&self, name: &str) -> io::Result<()> {
        let mut map = self.write_map();
        self.unlink(name)?;
        map.remove(name);
        Ok(())
    }

    /// Carries `name`'s set to its file as it stands now, where the node's
    /// own rename -- a compact, a checkpoint, an image a follower took --
    /// made another of it: at a close, a shutdown, a follower's hello and
    /// the tick after a compact ([`crate::tenants::Tenants::rebind_hooks`]).
    /// Synced: a binding lost to a power loss names the file before, which
    /// the next start puts away. A tenant holding no set pays a map's look.
    pub(crate) fn rebind(&self, name: &str) {
        if !self.read_map().contains_key(name) {
            return;
        }
        let Ok(now) = instance::binding(&self.file(name)) else {
            return;
        };
        let mut map = self.write_map();
        let Some(entry) = map.get_mut(name) else {
            return;
        };
        if entry.bound == now {
            return;
        }
        match self.write(name, &entry.set, now) {
            Ok(()) => entry.bound = now,
            Err(e) => {
                crate::log!("tenant `{name}`'s hook secrets were not bound to its file again: {e}")
            }
        }
    }

    /// Replaces `name`'s set with `set` -- none, every hook taken away --
    /// bound to its file, durably: the sidecar first, then memory. Asked
    /// under the tenant's slot, its file there.
    fn replace(&self, name: &str, set: Option<Set>) -> Result<Option<u64>, Refused> {
        let failed = |e: io::Error| {
            Refused(
                500,
                format!("tenant `{name}`'s hook secrets could not be written: {e}"),
            )
        };
        let mut map = self.write_map();
        let Some(set) = set else {
            self.unlink(name).map_err(failed)?;
            map.remove(name);
            return Ok(None);
        };
        let bound = instance::binding(&self.file(name)).map_err(failed)?;
        self.write(name, &set, bound).map_err(failed)?;
        self.widen(&set);
        hooks::serving();
        let version = set.version;
        map.insert(
            name.to_string(),
            Entry {
                set: Arc::new(set),
                bound,
            },
        );
        Ok(Some(version))
    }

    fn current(&self, name: &str) -> Option<Arc<Set>> {
        self.read_map().get(name).map(|e| Arc::clone(&e.set))
    }

    /// `PUT /_admin/tenants/<t>/hooks/<hook>`: the hook set from `body` in
    /// place of the one of its name, the set's new version answered.
    pub(crate) fn put_hook(&self, name: &str, hook: &str, body: &[u8]) -> Result<u64, Refused> {
        let fields = object(body)?;
        let given = self.given(hook, &fields, false)?;
        let mut hooks: Vec<Hook> = Vec::new();
        let mut kept = Vec::new();
        if let Some(old) = self.current(name) {
            for (h, k) in old.hooks.iter().zip(&old.kept) {
                if h.name != hook {
                    hooks.push(self.again(h, k)?);
                    kept.push(k.clone());
                }
            }
        }
        let (h, k) = self.made(hook, &given, &hooks, now_ms())?;
        hooks.push(h);
        kept.push(k);
        let set = Set {
            version: fresh_version(),
            hooks,
            kept,
        };
        self.replace(name, Some(set)).map(|v| v.unwrap_or(0))
    }

    /// `DELETE /_admin/tenants/<t>/hooks/<hook>`: the set's version after,
    /// `None` where no hook is left.
    pub(crate) fn drop_hook(&self, name: &str, hook: &str) -> Result<Option<u64>, Refused> {
        let old = self.current(name);
        let held = old
            .as_ref()
            .is_some_and(|s| s.hooks.iter().any(|h| h.name == hook));
        if !held {
            return Err(Refused(
                404,
                format!("tenant `{name}` holds no secret for hook `{hook}`"),
            ));
        }
        let old = old.expect("held");
        let mut hooks: Vec<Hook> = Vec::new();
        let mut kept = Vec::new();
        for (h, k) in old.hooks.iter().zip(&old.kept) {
            if h.name != hook {
                hooks.push(self.again(h, k)?);
                kept.push(k.clone());
            }
        }
        let set = (!hooks.is_empty()).then(|| Set {
            version: fresh_version(),
            hooks,
            kept,
        });
        self.replace(name, set)
    }

    /// `PUT /_admin/tenants/<t>/hook-secrets`: a set installed whole, its
    /// version kept -- what the router copies onto a copy lacking it.
    pub(crate) fn install(&self, name: &str, body: &[u8]) -> Result<Option<u64>, Refused> {
        let fields = object(body)?;
        let mut version = None;
        let mut listed = None;
        for (k, v) in fields {
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
        let listed = listed.unwrap_or_default();
        let mut hooks: Vec<Hook> = Vec::new();
        let mut kept = Vec::new();
        for (hook, v) in listed {
            let Value::Object(fields) = v else {
                return Err(Refused(400, format!("hook `{hook}`: an object")));
            };
            let given = self.given(&hook, &fields, true)?;
            let set_ms = given.set_ms.unwrap_or_else(now_ms);
            let (h, k) = self.made(&hook, &given, &hooks, set_ms)?;
            hooks.push(h);
            kept.push(k);
        }
        if hooks.is_empty() {
            return self.replace(name, None);
        }
        let version =
            version.ok_or_else(|| Refused(400, "a set of hooks names its `version`".into()))?;
        self.replace(
            name,
            Some(Set {
                version,
                hooks,
                kept,
            }),
        )
    }

    /// A kept hook made again, beside others in a new set.
    fn again(&self, h: &Hook, k: &Kept) -> Result<Hook, Refused> {
        let words = Self::words(&h.name, h.scheme, k);
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        self.line_of(&words, &[], k.set_ms, usize::MAX)
            .map(|(h, _)| h)
            .map_err(|why| Refused(500, format!("hook `{}`: {why}", h.name)))
    }

    /// A hook's line, its secrets in base64.
    fn words(name: &str, scheme: Scheme, k: &Kept) -> Vec<String> {
        let mut w = vec![name.to_string(), scheme.name().to_string()];
        w.extend(
            k.secrets
                .iter()
                .map(|s| format!("b64:{}", b64_encode(s.as_bytes()))),
        );
        for (word, v) in &k.opts {
            w.push(word.to_string());
            w.push(v.clone());
        }
        w
    }

    /// The hook `given` makes, beside `before`: refused as a hooks file's
    /// line is at a server's start, naming the hook and the field.
    fn made(
        &self,
        hook: &str,
        given: &Given,
        before: &[Hook],
        set_ms: i64,
    ) -> Result<(Hook, Kept), Refused> {
        let refused = |why: &str| Refused(400, format!("hook `{hook}`: {why}"));
        if !fennec_core::named::valid_name(hook) {
            return Err(refused(
                "a hook's name is ASCII letters, digits and `_`, a letter first, 59 bytes at most",
            ));
        }
        for s in &given.secrets {
            hooks::secret_of(given.scheme, s.as_bytes())
                .map_err(|w| refused(&format!("`secrets`: {w}")))?;
        }
        let k = Kept {
            set_ms,
            secrets: given.secrets.clone(),
            opts: given.opts.clone(),
        };
        let words = Self::words(hook, given.scheme, &k);
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let max_body = self.max_body.load(Ordering::Relaxed);
        self.line_of(&words, before, set_ms, max_body)
            .map_err(refused)
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
        let set = self.current(name);
        let mut out = String::from("{\"version\":");
        push_version(&mut out, set.as_ref().map(|s| s.version));
        out.push_str(",\"hooks\":{");
        if let Some(set) = &set {
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
        }
        out.push_str("}}");
        out
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
/// versions, so a copy holding one holds that set.
fn fresh_version() -> u64 {
    let bytes = crate::crypto::random_bytes(8);
    let v = u64::from_le_bytes(bytes[..8].try_into().unwrap_or([1; 8]));
    v.max(1)
}

fn now_ms() -> i64 {
    fennec_core::time::now_ms().unwrap_or(0)
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
/// node renames over -- `fennec restore`, `fennec backup --force`, as
/// [`instance::put_away_beside`] puts its identity away -- synced: a
/// restore brings a tenant's statements back and no secret.
pub fn put_away_beside(file: &Path) -> io::Result<()> {
    for f in [file.to_path_buf(), fennec_core::fs::resolved(file)] {
        let name = f.file_stem().and_then(|n| n.to_str());
        if let (Some(dir), Some(name), true) =
            (f.parent(), name, f.extension() == Some("fennec".as_ref()))
        {
            let path = dir.join(format!("{name}.hooks"));
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    fennec_core::fs::sync_dir(&path).map_err(|e| io::Error::other(e.to_string()))?
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

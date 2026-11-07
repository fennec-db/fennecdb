//! Webhook consumers' events, taken at `POST /_hooks/<name>`
//! (`docs/design/named-statements.md`, *Webhook consumers (F4)*): another
//! service's event -- Stripe's, GitHub's, a sender signing as Standard
//! Webhooks does -- verified by the signature its sender makes before
//! anything of it is read, then taken once by its key by the statements
//! defined `on hook <name>` (`Database::take_event`), their writes and the
//! key's row one block, answered 2xx only once that block is on disk.
//!
//! **Where a hook is.** A file's server reads `--hooks <path>` once, at its
//! start: a line a hook, its name, its scheme, one or two files each
//! holding a secret, and options -- `keep`, `tolerance`, `key` and
//! `max-body` ([`Hooks::read`]). A secret is the deployment's, as a token
//! is: never the database's, an archive's, a log's, a trace's or an
//! answer's. A server with no hooks file has no such route: `/_hooks/`
//! asks for a token as every unknown path does.
//!
//! **Verified before it is read.** The route is judged before
//! `authenticate` asks for a token, the one route running a statement
//! without one, so every check before the signature is held to the end and
//! answered alike ([`Hooks::verify`]): the scheme read from the headers
//! present, each sent once; the hook looked up, the process's own key
//! standing in for a secret that is not there and never verifying; the
//! signed timestamp within the hook's tolerance; the body within its
//! ceiling; and two HMAC-SHA256s always, over the body up to the widest
//! ceiling of any hook, each compared in constant time with every `v1`
//! signature sent. Every failure is one 401, after the same work, its
//! check named only in the log line, the audit's `refused` line and
//! `fennec_hook_unverified_total{check}`; it waits by a count of the hooks'
//! own, by the sender's address and the hook's name the decoded path
//! names ([`named`]), which a verified event of that hook from that
//! address starts again and nothing else does -- never `audit::verified`,
//! a signature being no token.
//!
//! **Taken once.** Verified, the event's content type is asked (415), its
//! body parsed exactly as an object (400), its key read -- Stripe's `id`,
//! Standard Webhooks' `webhook-id`, GitHub's body digest or its
//! `X-GitHub-Delivery`, as the hook says -- and under a short read lock
//! its key looked up and its statements judged; under the write lock the
//! key looked up again and the event taken. A duplicate is answered as the
//! first taking was, once that block is as durable and sent as the first's
//! answer was to be; a second body under a key is answered so too, logged
//! and counted, never run.
//!
//! **A 2xx is durable.** Whatever `--sync` says, the block is fsynced
//! after the lock and sent to the streams keeping up, and where the server
//! feeds a replica a promotion could take the event from, one of them must
//! have sent it within `SENT_WAIT` or the answer is 503 with `Retry-After`:
//! a sender told 2xx never sends the event again, so a crash or a failover
//! after a buffered 2xx would lose a payment no one tells fennecdb of.
//! Every other answer is the refusal's status, the sender's retries being
//! the queue, told as a scoped caller is told -- the status, a guard's
//! reason and a bound, never the engine's words, which name values, nor a
//! statement's name -- and never a 410 or a 3xx.

use crate::crypto::{b64_decode, ct_eq, sha256, HmacKey};
use crate::http::{Method, Request, Response};
use crate::replication::{Feed, SENT_WAIT};
use crate::{held, Config};
use fennec_core::engine::{Event, Took};
use fennec_core::prelude::*;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A signed timestamp's tolerance unless a hook's line says otherwise,
/// either side of the server's clock: Stripe's libraries' default.
pub const TOLERANCE_MS: i64 = 5 * 60_000;
/// The longest tolerance a hook takes: past an hour a captured event
/// verifies for longer than any sender's clock needs.
pub const MAX_TOLERANCE_MS: i64 = 3_600_000;
/// The shortest: none is refused, as Stripe warns ("Don't use a tolerance
/// value of `0`"), which would turn the recency check off.
pub const MIN_TOLERANCE_MS: i64 = 1_000;
/// The clocks of the nodes a failover moves a hook between may differ by
/// this much: a key is kept past twice the tolerance and this.
pub const SKEW_MS: i64 = 60_000;
/// The shortest `keep`: a day, past every tolerance allowed's window.
pub const MIN_KEEP_MS: i64 = DAY;
/// A body's ceiling unless a hook's line says otherwise: Standard Webhooks
/// recommends events "usually smaller than 20kb".
pub const CEILING: usize = 1 << 20;
/// The `v1` signatures a header holds at most: a sender signs once a
/// secret, and two secrets roll at once.
pub const MAX_SIGNATURES: usize = 8;
const DAY: i64 = 86_400_000;

/// How a sender signs: the headers it sends and what its signature covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    /// `Stripe-Signature: t=<seconds>,v1=<hex>`, over `<t>.<body>`.
    Stripe,
    /// `X-Hub-Signature-256: sha256=<hex>`, over the body.
    Github,
    /// `webhook-signature: v1,<base64>`, over `<webhook-id>.<webhook-timestamp>.<body>`.
    Standard,
}

impl Scheme {
    pub(crate) fn read(word: &str) -> Option<Scheme> {
        match word {
            "stripe" => Some(Scheme::Stripe),
            "github" => Some(Scheme::Github),
            "standard" => Some(Scheme::Standard),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Scheme::Stripe => "stripe",
            Scheme::Github => "github",
            Scheme::Standard => "standard",
        }
    }

    /// Whether its signature covers a timestamp, held to the tolerance.
    fn timed(self) -> bool {
        self != Scheme::Github
    }
}

/// What a `github` hook keys its events by (`key body | delivery`): the
/// body's SHA-256, which its signature covers and a redelivery repeats, or
/// `X-GitHub-Delivery`, GitHub's own guidance, which its signature does
/// not cover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyBy {
    Body,
    Delivery,
}

/// The check an event failed before its signature verified: named in the
/// log line, the audit's `refused` line and
/// `fennec_hook_unverified_total{check}` alone, never in the answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// No scheme's headers, two schemes', one sent twice, or malformed.
    Header,
    /// Headers of another scheme than the hook's.
    Scheme,
    /// The signed timestamp outside the hook's tolerance.
    Stale,
    /// The body past the hook's ceiling.
    Size,
    /// No such hook.
    Hook,
    /// No signature sent matches.
    Signature,
}

impl Check {
    const ALL: [Check; 6] = [
        Check::Header,
        Check::Scheme,
        Check::Stale,
        Check::Size,
        Check::Hook,
        Check::Signature,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Check::Header => "header",
            Check::Scheme => "scheme",
            Check::Stale => "stale",
            Check::Size => "size",
            Check::Hook => "hook",
            Check::Signature => "signature",
        }
    }

    /// What the audit's `refused` line says of it.
    fn reason(self) -> &'static str {
        match self {
            Check::Header => "an event's signature headers are missing, malformed or sent twice",
            Check::Scheme => "an event signed as another scheme than its hook's",
            Check::Stale => "an event's signed timestamp is outside its hook's tolerance",
            Check::Size => "an event's body is past its hook's ceiling",
            Check::Hook => "an event for a hook this server does not hold",
            Check::Signature => "an event's signature does not verify",
        }
    }
}

/// A hook a hooks file's line makes: cloned as a tenant's set is made
/// again around a change of one of its hooks.
#[derive(Clone)]
pub struct Hook {
    pub name: String,
    pub scheme: Scheme,
    /// Its secrets' keys, the process's own standing in for a second it
    /// lacks: two HMACs always, whatever it holds.
    keys: [HmacKey; 2],
    /// How many secrets its line names: one or two. With one, the second
    /// slot's match is masked out ([`Hooks::verify`]).
    pub secrets: usize,
    pub tolerance_ms: i64,
    /// How long an event's key is kept past its taking.
    pub keep_ms: i64,
    pub key: KeyBy,
    /// The body's ceiling, `max-body`: a body past it fails `size`.
    pub ceiling: usize,
}

/// A server's hooks, read from its hooks file at its start.
pub struct Hooks {
    hooks: Vec<Hook>,
    /// A key of the process's own, made at its start from the system's
    /// randomness, for a secret that is not there -- a hook missing, a
    /// second secret: the work goes on all the same, and no signature
    /// anyone sent can match it, a second slot's match masked besides.
    stand_in: [HmacKey; 2],
    /// The widest ceiling of any hook, [`CEILING`] at least: every event's
    /// HMACs run over its body up to it, whatever hook it names, so the
    /// time they take tells no hook's ceiling from another's.
    widest: usize,
    /// The feed a file's server sends its replicas from, whose standbys a
    /// 2xx waits for ([`Feed::standby_sent`]).
    feed: OnceLock<Arc<Feed>>,
}

/// An event that verified: its hook, and the time its signature covers.
pub struct Verified<'h> {
    pub hook: &'h Hook,
    /// `hook.at`, in milliseconds.
    pub at: Option<i64>,
}

/// A hooks file's lines: `<name> <scheme> <secret file> [<secret file>]
/// [keep <duration>] [tolerance <duration>] [key body|delivery] [max-body
/// <size>]`, a `#` starting a comment.
const OPTIONS: [&str; 4] = ["keep", "tolerance", "key", "max-body"];

/// What a refused line says, past its number: the server's own words,
/// never a word of the line -- a secret pasted where its file's path goes
/// is no path to echo -- one format site for them all.
pub(crate) type Why = &'static str;

// The name's refusal says 59 bytes.
const _: () = assert!(fennec_core::named::MAX_NAME == 59);

impl Hooks {
    /// The hooks `text` -- a hooks file's -- names, each secret read by
    /// `secret` from the file its line names; refused, naming the line and
    /// what to write, for whatever the design refuses at the start: a hook
    /// named twice or by no word, a scheme unknown, a secret file empty,
    /// unread or holding a `standard` secret that is not `whsec_` and
    /// base64, a tolerance of none or past an hour, a `keep` shorter than
    /// a day or not past twice the tolerance and [`SKEW_MS`], a `key` on a
    /// hook not `github`'s, a `max-body` past `max_body`. Never a word of
    /// the line, so never a secret's value. Refused too where the system's
    /// randomness cannot be read: a stand-in key is never the clock's.
    pub fn read(
        text: &str,
        secret: impl Fn(&str) -> std::io::Result<Vec<u8>>,
        max_body: usize,
    ) -> std::result::Result<Hooks, String> {
        let stand_in = stand_in()?;
        let mut hooks: Vec<Hook> = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let words: Vec<&str> = line
                .split_ascii_whitespace()
                .take_while(|w| !w.starts_with('#'))
                .collect();
            if words.is_empty() {
                continue;
            }
            let hook = hook_of(&words, &hooks, &secret, max_body, &stand_in)
                .map_err(|why| format!("hooks file line {}: {why}", n + 1))?;
            hooks.push(hook);
        }
        SERVING.store(true, Ordering::Relaxed);
        let widest = hooks.iter().map(|h| h.ceiling).fold(CEILING, usize::max);
        Ok(Hooks {
            hooks,
            stand_in: [stand_in.clone(), stand_in],
            widest,
            feed: OnceLock::new(),
        })
    }

    pub fn get(&self, name: &str) -> Option<&Hook> {
        self.hooks.iter().find(|h| h.name == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Hook> {
        self.hooks.iter()
    }

    /// The feed whose standbys a 2xx waits for: a file's server's, which
    /// feeds its replicas from it.
    pub fn send_from(&self, feed: &Arc<Feed>) {
        let _ = self.feed.set(Arc::clone(feed));
    }

    /// The event `req` sent to hook `name`, verified at `now_ms`, or the
    /// first check it failed. Every check runs whatever fails before it --
    /// both HMACs over the body up to the widest ceiling of any hook, the
    /// same whatever `name` is -- so an event refused for its headers, its
    /// scheme, its time, its size, its hook or its signature costs one
    /// work and is answered one 401.
    pub fn verify(
        &self,
        name: &str,
        req: &Request,
        now_ms: i64,
    ) -> std::result::Result<Verified<'_>, Check> {
        let hook = self.get(name);
        let at = verified(hook, &self.stand_in, self.widest, req, now_ms)?;
        match hook {
            Some(hook) => Ok(Verified { hook, at }),
            None => Err(Check::Hook),
        }
    }
}

/// Whether `req` verifies as an event of `hook` -- `None`, no such hook,
/// the defaults and `stand_in` taking its place -- at `now_ms`, its HMACs
/// over the body up to `widest`: `hook.at` where it does, else the first
/// check it failed. Every check runs whatever fails before it, so an event
/// refused for its headers, its scheme, its time, its size, its hook or
/// its signature costs one work: a file's server's hooks and a tenant
/// node's alike ([`Hooks::verify`], `tenant_hooks`).
pub(crate) fn verified(
    hook: Option<&Hook>,
    stand_in: &[HmacKey; 2],
    widest: usize,
    req: &Request,
    now_ms: i64,
) -> std::result::Result<Option<i64>, Check> {
    let mut failed = None;
    let mut fail = |c: Check| {
        failed.get_or_insert(c);
    };
    let signed = signed(req);
    if signed.is_none() {
        fail(Check::Header);
    }
    // A second secret's match counts only where the hook names one: a
    // stand-in's slot never verifies, whatever key made it.
    let (keys, tolerance, ceiling, second) = match hook {
        Some(h) => (&h.keys, h.tolerance_ms, h.ceiling, h.secrets == 2),
        None => {
            fail(Check::Hook);
            (stand_in, TOLERANCE_MS, CEILING, false)
        }
    };
    let scheme = signed.as_ref().map(|s| s.scheme);
    if let (Some(h), Some(s)) = (hook, scheme) {
        if h.scheme != s {
            fail(Check::Scheme);
        }
    }
    let t = signed.as_ref().map_or(&b""[..], |s| s.t);
    let at = seconds(t).and_then(|s| s.checked_mul(1000));
    if scheme.is_some_and(Scheme::timed)
        && !at.is_some_and(|at| now_ms.abs_diff(at) <= tolerance as u64)
    {
        fail(Check::Stale);
    }
    // The hook's own ceiling decides the failure; the HMACs cover the
    // body up to the widest, so a hook whose ceiling was raised or
    // lowered takes the time any other name takes.
    if req.body.len() > ceiling {
        fail(Check::Size);
    }
    let body = &req.body[..req.body.len().min(widest)];
    let id = signed.as_ref().map_or(&b""[..], |s| s.id);
    let parts: [&[u8]; 5] = match scheme {
        Some(Scheme::Stripe) => [t, b".", body, b"", b""],
        Some(Scheme::Standard) => [id, b".", t, b".", body],
        Some(Scheme::Github) | None => [body, b"", b"", b"", b""],
    };
    let macs = [keys[0].mac(&parts), keys[1].mac(&parts)];
    #[cfg(test)]
    tests::counted(body.len());
    // Every signature against both, no `||` and no early way out: the
    // work is the same whichever matched, or none.
    let mut matched = false;
    if let Some(s) = &signed {
        for sig in &s.sigs[..s.n] {
            matched |= compared(&macs[0], sig) | (compared(&macs[1], sig) & second);
        }
    }
    if !matched {
        fail(Check::Signature);
    }
    match (failed, hook) {
        (None, Some(hook)) => Ok(at.filter(|_| hook.scheme.timed())),
        (Some(c), _) => Err(c),
        (None, None) => Err(Check::Hook),
    }
}

/// Waits for a standby to hold write `upto`, where the server feeds one a
/// promotion could take it from -- a file's server's replica, a tenant's
/// pair standby or `--replicas` copy, never an archiver nor a read replica,
/// which mark their streams -- `false` past `SENT_WAIT`.
fn standby_holds(feed: Option<&Feed>, upto: u64) -> bool {
    feed.is_none_or(|f| f.standby_sent(upto, SENT_WAIT))
}

/// A key of the process's own for a secret that is not there, made from
/// the system's randomness -- never the clock's, which `random_bytes` falls
/// back to: no signature anyone sent can match it. Refused where the
/// randomness cannot be read, and the server with it.
pub(crate) fn stand_in() -> std::result::Result<HmacKey, String> {
    let seed = crate::crypto::system_random(32).ok_or(
        "the hooks' stand-in key needs the system's randomness (/dev/urandom), which could not \
         be read",
    )?;
    Ok(HmacKey::new(&seed))
}

/// A MAC against a signature sent, in constant time: every comparison
/// `verify` makes goes through here, counted in a test build, so one made
/// another way -- a slice's `==`, which stops at the first byte that
/// differs -- or skipped shows in the count.
#[inline(always)]
fn compared(mac: &[u8; 32], sig: &[u8; 32]) -> bool {
    #[cfg(test)]
    tests::compared_one();
    ct_eq(mac, sig)
}

/// The hook a hooks file's line `words` makes, beside the `hooks` read
/// before it, or what is wrong with it.
pub(crate) fn hook_of(
    words: &[&str],
    hooks: &[Hook],
    secret: &impl Fn(&str) -> std::io::Result<Vec<u8>>,
    max_body: usize,
    stand_in: &HmacKey,
) -> std::result::Result<Hook, Why> {
    let [name, scheme, rest @ ..] = words else {
        return Err("a hook is `<name> <scheme> <secret file> [<secret file>] [options]`");
    };
    if !fennec_core::named::valid_name(name) {
        return Err(
            "a hook's name is ASCII letters, digits and `_`, a letter first, 59 bytes at most",
        );
    }
    if hooks.iter().any(|h| h.name == *name) {
        return Err("a hook is named twice");
    }
    let scheme = Scheme::read(scheme).ok_or("a scheme is `stripe`, `github` or `standard`")?;
    let files = rest.iter().take_while(|w| !OPTIONS.contains(w)).count();
    if !(1..=2).contains(&files) {
        return Err("a hook names one secret file, or two while a secret rolls");
    }
    let mut keys = [stand_in.clone(), stand_in.clone()];
    for (slot, f) in keys.iter_mut().zip(&rest[..files]) {
        let raw = secret(f).map_err(|_| "could not read a secret file it names")?;
        *slot = HmacKey::new(&secret_of(scheme, &raw)?);
    }
    let mut opts = rest[files..].iter();
    let (mut keep, mut tolerance, mut key, mut ceiling) = (None, None, None, None);
    while let Some(opt) = opts.next() {
        let value = *opts.next().ok_or("an option expects a value")?;
        let given = match *opt {
            "keep" => keep.replace(duration(value)?).is_some(),
            "tolerance" => tolerance.replace(duration(value)?).is_some(),
            "key" => {
                let by = match value {
                    "body" => KeyBy::Body,
                    "delivery" => KeyBy::Delivery,
                    _ => return Err("a `github` hook keys its events by `body` or `delivery`"),
                };
                key.replace(by).is_some()
            }
            "max-body" => {
                let size = (crate::quota::size(value).filter(|s| *s > 0))
                    .ok_or("`max-body` is a size: 64k, 4m")?;
                ceiling.replace(size).is_some()
            }
            _ => return Err("an option is `keep`, `tolerance`, `key` or `max-body`"),
        };
        if given {
            return Err("an option is given twice");
        }
    }
    if key.is_some() && scheme != Scheme::Github {
        return Err("`key` is a `github` hook's: the others' keys are signed");
    }
    let tolerance_ms = tolerance.unwrap_or(TOLERANCE_MS);
    if !(MIN_TOLERANCE_MS..=MAX_TOLERANCE_MS).contains(&tolerance_ms) {
        return Err("a tolerance is 1s to 1h");
    }
    let keep_ms = keep.unwrap_or(match scheme {
        Scheme::Stripe => 30 * DAY,
        _ => 7 * DAY,
    });
    if keep_ms < MIN_KEEP_MS || keep_ms <= 2 * tolerance_ms + SKEW_MS {
        return Err("a `keep` is 1d at least, and past twice the tolerance and 1m");
    }
    // Unless given, a mebibyte or the server's own ceiling where that is
    // lower: a default past `--max-body` refused a line that named none.
    let ceiling = ceiling.unwrap_or(CEILING.min(max_body) as u64);
    if ceiling > max_body as u64 {
        return Err("`max-body` is at most the server's --max-body");
    }
    Ok(Hook {
        name: name.to_string(),
        scheme,
        keys,
        secrets: files,
        tolerance_ms,
        keep_ms,
        key: key.unwrap_or(KeyBy::Body),
        ceiling: ceiling as usize,
    })
}

/// A secret file's bytes as its scheme reads them: Stripe's `whsec_` text
/// and GitHub's chosen secret as written, a `standard` one's base64 after
/// `whsec_` decoded. Its file's last line end is no part of it.
fn secret_of(scheme: Scheme, raw: &[u8]) -> std::result::Result<Vec<u8>, Why> {
    let mut s = raw;
    while let [rest @ .., b'\n' | b'\r'] = s {
        s = rest;
    }
    if s.is_empty() {
        return Err("a secret is empty: an empty secret verifies any signature");
    }
    if scheme != Scheme::Standard {
        return Ok(s.to_vec());
    }
    let key = (std::str::from_utf8(s).ok())
        .and_then(|t| t.strip_prefix("whsec_"))
        .and_then(b64_decode)
        .filter(|k| !k.is_empty());
    key.ok_or(
        "a secret is no Standard Webhooks secret: `whsec_` and base64, as its sender shows it",
    )
}

/// A duration of a hooks file: a number and `s`, `m`, `h` or `d`.
fn duration(text: &str) -> std::result::Result<i64, Why> {
    const REFUSED: Why = "a duration is a number and s, m, h or d: 30d, 5m";
    // Checked: a word ending in a letter of more than a byte (`5ü`) is no
    // duration, never a panic at the start.
    let (n, unit) = (text.split_at_checked(text.len().saturating_sub(1))).ok_or(REFUSED)?;
    let ms = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => DAY,
        _ => return Err(REFUSED),
    };
    let n: i64 = n.parse().map_err(|_| REFUSED)?;
    n.checked_mul(ms).filter(|v| *v > 0).ok_or(REFUSED)
}

/// Seconds written as digits alone, as a signed timestamp is.
fn seconds(t: &[u8]) -> Option<i64> {
    if t.is_empty() || t.len() > 15 || !t.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(t).ok()?.parse().ok()
}

/// What an event's headers say, read before anything of its hook: its
/// scheme, the timestamp and id its signature covers, and the `v1`
/// signatures sent, decoded.
struct Signed<'r> {
    scheme: Scheme,
    t: &'r [u8],
    id: &'r [u8],
    sigs: [[u8; 32]; MAX_SIGNATURES],
    n: usize,
}

/// The headers a scheme is told by, and GitHub's beside its signature.
/// Standard Webhooks' three come under two names: its own `webhook-*` and
/// Svix's `svix-*`, which Clerk, Resend and every service sending through
/// Svix send, and which Svix's libraries read as the same.
const HEADERS: [&str; 10] = [
    "stripe-signature",
    "x-hub-signature-256",
    "webhook-signature",
    "webhook-id",
    "webhook-timestamp",
    "svix-signature",
    "svix-id",
    "svix-timestamp",
    "x-github-event",
    "x-github-delivery",
];

/// The scheme `req`'s headers sign by, read from the headers present and
/// never from the hook: one scheme's headers, each sent once -- a reader
/// signing with one `t` and holding another to the tolerance would take an
/// old capture -- and Standard Webhooks' under one of its two names, never
/// both, so no reader takes the id of one and the signature of the other;
/// one `t` in `Stripe-Signature`, an id holding no `.` and a timestamp of
/// digits, so no two splits of what is signed sign alike, and one to
/// [`MAX_SIGNATURES`] `v1` signatures, each in its encoding; other
/// versions (`v0`, `v1a`) passed over, never read as `v1`. `None` for
/// anything else.
fn signed(req: &Request) -> Option<Signed<'_>> {
    let mut seen = [0u8; HEADERS.len()];
    let mut value = [""; HEADERS.len()];
    for (k, v) in &req.headers {
        if let Some(i) = HEADERS.iter().position(|h| k.eq_ignore_ascii_case(h)) {
            seen[i] = seen[i].saturating_add(1);
            value[i] = v.as_str();
        }
    }
    if seen.iter().any(|&n| n > 1) {
        return None;
    }
    let sent = |at: usize| seen[at..at + 3].iter().any(|&n| n > 0);
    let families = [seen[0] > 0, seen[1] > 0, sent(2), sent(5)];
    if families.iter().filter(|f| **f).count() != 1 {
        return None;
    }
    let mut s = Signed {
        scheme: Scheme::Github,
        t: b"",
        id: b"",
        sigs: [[0; 32]; MAX_SIGNATURES],
        n: 0,
    };
    let push = |s: &mut Signed, sig: Option<[u8; 32]>| -> Option<()> {
        if s.n == MAX_SIGNATURES {
            return None;
        }
        s.sigs[s.n] = sig?;
        s.n += 1;
        Some(())
    };
    if families[0] {
        s.scheme = Scheme::Stripe;
        let mut t = None;
        for el in value[0].split(',') {
            let (k, v) = el.trim().split_once('=')?;
            match k {
                "t" if t.is_none() => t = Some(v.as_bytes()),
                "t" => return None,
                "v1" => push(&mut s, hex32(v))?,
                _ => {}
            }
        }
        s.t = t.filter(|t| seconds(t).is_some())?;
    } else if families[1] {
        let hex = value[1].trim().strip_prefix("sha256=")?;
        push(&mut s, hex32(hex))?;
    } else {
        s.scheme = Scheme::Standard;
        let at = if families[2] { 2 } else { 5 };
        if seen[at..at + 3] != [1, 1, 1] {
            return None;
        }
        let (id, t) = (value[at + 1].trim(), value[at + 2].trim());
        if id.is_empty() || id.contains('.') || seconds(t.as_bytes()).is_none() {
            return None;
        }
        s.id = id.as_bytes();
        s.t = t.as_bytes();
        for sig in value[at].split_ascii_whitespace() {
            let (version, b64) = sig.split_once(',')?;
            if version == "v1" {
                let raw = b64_decode(b64).filter(|r| r.len() == 32)?;
                push(&mut s, raw.try_into().ok())?;
            }
        }
    }
    (s.n > 0).then_some(s)
}

/// A Standard Webhooks event's id, under whichever of its two names it
/// came: [`signed`] took one alone.
fn standard_id(req: &Request) -> &str {
    let id = |k: &str| k.eq_ignore_ascii_case("webhook-id") || k.eq_ignore_ascii_case("svix-id");
    (req.headers.iter())
        .find_map(|(k, v)| id(k).then_some(v.trim()))
        .unwrap_or_default()
}

/// 64 hex digits as 32 bytes, either case.
fn hex32(text: &str) -> Option<[u8; 32]> {
    let b = text.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (i, [hi, lo]) in b.as_chunks::<2>().0.iter().enumerate() {
        out[i] = (digit(*hi)? << 4) | digit(*lo)?;
    }
    Some(out)
}

fn hex(bytes: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(D[(b >> 4) as usize] as char);
        s.push(D[(b & 15) as usize] as char);
    }
    s
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Whether `req` is a hook's: every path under `/_hooks/`, answered alike
/// whatever it names.
pub(crate) fn is_hook(req: &Request) -> bool {
    req.path == "/_hooks" || req.path.starts_with("/_hooks/")
}

/// What one 401 says for every refusal before a verified signature.
pub const UNVERIFIED: &str = "the event does not verify";

/// `POST /_hooks/<name>`: verified, then taken once by its key and
/// answered once durable (the module's docs). Judged before `authenticate`:
/// nothing of the request is read before its signature checks out. Out of
/// line and cold: inlined, it made `fennec_http::handle`, which every
/// request runs, 4.6 KB larger -- the HMACs, SHA-256, the taking -- for a
/// route most servers never meet.
#[cold]
#[inline(never)]
pub(crate) fn handle(
    db: &Arc<RwLock<Database>>,
    cfg: &Config,
    hooks: &Hooks,
    req: &Request,
) -> Response {
    if req.method != Method::Post {
        return Response::error(405, "a hook takes a POST").header("Allow", "POST");
    }
    let name = req.path.strip_prefix("/_hooks/").unwrap_or_default();
    let v = match hooks.verify(name, req, now_ms()) {
        Ok(v) => v,
        Err(check) => return unverified(req, check),
    };
    let hook = v.hook;
    // A verified event of this hook from this address starts its count
    // again, and nothing else does: never a token's (`audit::verified`).
    cleared(crate::audit::client_ip(), None, &hook.name);
    let delivery = req.header("x-github-delivery");
    let taken = take(db, cfg, hooks.feed.get().map(|f| &**f), hook, v.at, req);
    noted(hook, delivery, taken)
}

/// A tenant's event, `POST /t/<tenant>/_hooks/<name>` on a node: verified
/// in `route_tenant` against the tenant's secrets before the tenant was
/// looked up ([`crate::tenant_hooks`]), and taken in its database as a
/// file's server takes one -- inside the tenant's gate, quota and lease,
/// as every request to it is -- its 2xx waiting for a standby of the
/// tenant's to hold it: its pair's or its `--replicas` copy, never a read
/// replica nor the node's archive, which mark their streams. Out of line
/// and cold, as [`handle`] is.
#[cold]
#[inline(never)]
pub(crate) fn handle_tenant(
    t: &crate::tenants::Tenant,
    cfg: &Config,
    v: &crate::tenant_hooks::Verified,
    req: &Request,
) -> Response {
    let hook = v.hook();
    cleared(crate::audit::client_ip(), Some(t.name()), &hook.name);
    let delivery = req.header("x-github-delivery");
    let feed = t.repl.as_ref().and_then(|r| r.feed());
    let mut taken = take(&t.db, cfg, feed.map(|f| &**f), hook, v.at_ms, req);
    // A frozen tenant's, or one whose lease is out, comes back: told 503
    // by the tenant's handling, a state's refusal and no event's.
    if cfg.come_back && taken.resp.status == 403 {
        taken.outcome = Outcome::Deferred;
    }
    // Its lease asked again as its answer is ready, as every write's is: a
    // block that landed past the lease's lapse may be lost to a failover
    // that took the tenant meanwhile, and is told so, `outcome: unknown`.
    let before = taken.resp.status;
    taken.resp = crate::answered_under_lease(t, req, taken.resp);
    if taken.resp.status == 500 && before != 500 {
        taken.outcome = Outcome::Unknown;
        taken.why = Some("landed, and the tenant's lease lapsed before its answer".into());
    }
    noted(hook, delivery, taken)
}

/// The hook's name `req`'s path names, decoded as the route looks a hook
/// up, where it is one -- a word of at most 59 bytes -- whether or not the
/// server holds that hook; `None` for any other path under `/_hooks/`. A
/// refusal is counted, logged and audited by it alone: a path is up to the
/// head's 64 KiB, and a forger's, kept or written, filled memory and the
/// log by the megabyte.
pub(crate) fn named(req: &Request) -> Option<Named<'_>> {
    if let Some(hook) = req.path.strip_prefix("/_hooks/") {
        return fennec_core::named::valid_name(hook).then_some(Named { tenant: None, hook });
    }
    // A tenant's, as `route_tenant` reads it before its prefix is cut --
    // and as a router reads the path it forwards.
    let (tenant, rest) = req.path.strip_prefix("/t/")?.split_once('/')?;
    let hook = rest.strip_prefix("_hooks/")?;
    let ok = crate::tenants::check_name(tenant).is_ok() && fennec_core::named::valid_name(hook);
    ok.then_some(Named {
        tenant: Some(tenant),
        hook,
    })
}

/// The hook a path names: its name, and on a tenant node its tenant's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Named<'r> {
    pub tenant: Option<&'r str>,
    pub hook: &'r str,
}

impl std::fmt::Display for Named<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.tenant {
            Some(t) => write!(f, "tenant `{t}`'s hook `{}`", self.hook),
            None => write!(f, "hook `{}`", self.hook),
        }
    }
}

/// Whether a request `target`, as sent -- percent-encoded, a query string
/// after it -- names a tenant's hook, `/t/<tenant>/_hooks/...`, decoded and
/// cut at its slashes as a node routes it: an event its node takes once by
/// its key, which a router and the global entry send again where its
/// connection ended before its answer (`fennec-shard`'s `replayable`).
pub fn keyed_path(target: &str) -> bool {
    let path = target.split('?').next().unwrap_or("");
    let path = crate::http::percent_decode(path);
    let mut segs = path.split('/').filter(|s| !s.is_empty());
    segs.next() == Some("t") && segs.next().is_some() && segs.next() == Some("_hooks")
}

/// Whether `req` is an event sent to a tenant's hook, `POST
/// /t/<tenant>/_hooks/...`, whatever it names: what a router forwarding a
/// node's 401 to it waits out by the hooks' count rather than the tokens'
/// (`audit::http`). Asked of a 401 alone.
pub(crate) fn to_tenant(req: &Request) -> bool {
    req.method == Method::Post && matches!(req.segments().as_slice(), ["t", _, "_hooks", ..])
}

/// The one 401, its check named where an operator reads it alone.
#[cold]
pub(crate) fn unverified(req: &Request, check: Check) -> Response {
    UNVERIFIED_COUNT[check as usize].fetch_add(1, Ordering::Relaxed);
    crate::audit::hook_refused(check.reason());
    crate::log!(
        "{}: an event from {} did not verify ({}): answered 401",
        named(req).map_or_else(|| "hook `?`".to_string(), |n| n.to_string()),
        crate::audit::client_ip().map_or_else(|| "?".to_string(), |ip| ip.to_string()),
        check.name()
    );
    Response::error(401, UNVERIFIED)
}

/// What a verified event came to: its answer, and what the log and the
/// counts say of it.
struct Taken {
    resp: Response,
    outcome: Outcome,
    /// The event's key, where one was read.
    key: Option<String>,
    /// The engine's words for a refusal: the log's, never the sender's.
    why: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Outcome {
    Taken,
    Ignored,
    Replayed,
    Conflict,
    Refused,
    Deferred,
    Unknown,
}

impl Outcome {
    const ALL: [Outcome; 7] = [
        Outcome::Taken,
        Outcome::Ignored,
        Outcome::Replayed,
        Outcome::Conflict,
        Outcome::Refused,
        Outcome::Deferred,
        Outcome::Unknown,
    ];

    fn name(self) -> &'static str {
        match self {
            Outcome::Taken => "taken",
            Outcome::Ignored => "ignored",
            Outcome::Replayed => "replayed",
            Outcome::Conflict => "conflict",
            Outcome::Refused => "refused",
            Outcome::Deferred => "deferred",
            Outcome::Unknown => "unknown",
        }
    }

    /// A refusal's outcome by its status: the event's own -- its typing,
    /// a statement's or a binding's -- or the state's, which waiting
    /// clears: a quota, a rate, the disk, a standby.
    fn of(status: u16) -> Outcome {
        match status {
            429 | 500 | 503 | 507 => Outcome::Deferred,
            _ => Outcome::Refused,
        }
    }
}

/// Counted, and logged where it was refused or came again with another
/// body: the hook, the key, the status and the engine's words, GitHub's
/// delivery beside the key -- what the sender was not told.
fn noted(hook: &Hook, delivery: Option<&str>, t: Taken) -> Response {
    EVENTS[t.outcome as usize].fetch_add(1, Ordering::Relaxed);
    if matches!(
        t.outcome,
        Outcome::Refused | Outcome::Deferred | Outcome::Unknown | Outcome::Conflict
    ) {
        let key = t.key.as_deref().unwrap_or("-");
        let delivery = delivery
            .map(|d| format!(" (delivery {d:?})"))
            .unwrap_or_default();
        let what = match t.outcome {
            Outcome::Conflict => {
                "came again with another body: answered as its first taking, nothing run"
                    .to_string()
            }
            _ => format!(
                "answered {}: {}",
                t.resp.status,
                t.why.as_deref().unwrap_or("refused")
            ),
        };
        crate::log!("hook `{}`: event {key:?}{delivery} {what}", hook.name);
    }
    t.resp
}

/// A verified event read, judged and taken.
fn take(
    db: &Arc<RwLock<Database>>,
    cfg: &Config,
    feed: Option<&Feed>,
    hook: &Hook,
    at: Option<i64>,
    req: &Request,
) -> Taken {
    let refused = |resp: Response, why: &str| Taken {
        outcome: Outcome::of(resp.status),
        resp,
        key: None,
        why: Some(why.to_string()),
    };
    // Only now is the content type asked: a forger is told nothing of it.
    let json = req.header("content-type").is_some_and(|t| {
        let media = t.split(';').next().unwrap_or("").trim();
        media.eq_ignore_ascii_case("application/json")
    });
    if !json {
        let form = req.header("content-type").is_some_and(|t| {
            t.trim_start()
                .to_ascii_lowercase()
                .starts_with("application/x-www-form-urlencoded")
        });
        let msg = match form {
            true => {
                "an event is JSON: this hook's sender sends a form -- set its content type \
                     to application/json (GitHub: the webhook's Content type)"
            }
            false => "an event is JSON: its sender sends Content-Type: application/json",
        };
        return refused(Response::error(415, msg), "not JSON's content type");
    }
    if req.header("fennec-wait").is_some() {
        return refused(
            Response::error(400, "an event is never held: Fennec-Wait is a write's"),
            "Fennec-Wait on an event",
        );
    }
    if cfg.read_only || cfg.marked {
        return refused(crate::read_only_refused(cfg), "a server taking no writes");
    }
    let body = match std::str::from_utf8(&req.body).map(fennec_core::json::parse_json) {
        Ok(Ok(v @ Value::Object(_))) => v,
        _ => {
            return refused(
                Response::error(400, "an event's body is a JSON object"),
                "a body no JSON object",
            )
        }
    };
    let key = match hook.scheme {
        Scheme::Stripe => match body.member("id") {
            Some(Value::Text(id)) => id.to_string(),
            _ => {
                return refused(
                    Response::error(400, "a Stripe event's body names its `id`"),
                    "no `id` in a Stripe event",
                )
            }
        },
        Scheme::Standard => standard_id(req).to_string(),
        Scheme::Github => match hook.key {
            KeyBy::Body => hex(&sha256(&req.body)),
            KeyBy::Delivery => match req.header("x-github-delivery") {
                Some(d) => d.trim().to_string(),
                None => {
                    return refused(
                        Response::error(
                            400,
                            "this hook keys its events by X-GitHub-Delivery, and none was sent",
                        ),
                        "no X-GitHub-Delivery",
                    )
                }
            },
        },
    };
    let e = Event {
        hook: &hook.name,
        key: &key,
        body: &body,
        digest: digest(&req.body),
        at,
        event: match hook.scheme {
            Scheme::Github => req.header("x-github-event").map(str::trim),
            _ => None,
        },
        keep: hook.keep_ms,
    };
    let mut t = taken(db, cfg, feed, &e);
    t.key = Some(key);
    t
}

/// The event `e`, its key looked up and its statements judged under a
/// short read lock, then taken under the write lock and answered once
/// durable -- or answered from its key once the first taking's block is.
fn taken(db: &Arc<RwLock<Database>>, cfg: &Config, feed: Option<&Feed>, e: &Event) -> Taken {
    let refusal = |e: &Error, hook: &str| Taken {
        resp: told(e, hook),
        outcome: Outcome::of(crate::api::status_of(e)),
        key: None,
        why: Some(e.to_string()),
    };
    crate::metrics::wrote();
    // A tenant's rates of writes, asked before the lock as a write's are.
    if let Err(resp) = crate::quota::writing() {
        return Taken {
            outcome: Outcome::Deferred,
            resp,
            key: None,
            why: Some("the rate of writes".into()),
        };
    }
    let judged = {
        let g = held::read(db);
        if g.history().following {
            return Taken {
                resp: Response::error(
                    403,
                    "this server is a replica: a hook's events go to its primary",
                ),
                outcome: Outcome::Refused,
                key: None,
                why: Some("a replica".into()),
            };
        }
        match g.event_kept(e) {
            Err(err) => return refusal(&err, e.hook),
            Ok(Some(k)) => return replayed(db, feed, g, e, k),
            Ok(None) => {}
        }
        match g.judge_event(e) {
            Err(err) => return refusal(&err, e.hook),
            Ok(j) if !j.takes() => {
                return Taken {
                    resp: Response::json(200, r#"{"taken":0}"#),
                    outcome: Outcome::Ignored,
                    key: None,
                    why: None,
                }
            }
            Ok(j) => j,
        }
    };
    let mut guard = held::write(db);
    // Held as a call of the bodies is (`--max-memory`): a statement that
    // inserts or sets above the ceiling, 507.
    if let Some(why) = judged
        .bodies()
        .find_map(|s| crate::over_ceiling(cfg.max_memory, &guard, s))
    {
        drop(guard);
        return Taken {
            resp: Response::error(507, &why),
            outcome: Outcome::Deferred,
            key: None,
            why: Some(why),
        };
    }
    let took = guard.take_judged(e, judged);
    let n = match took {
        Err(err) => {
            let resp = told(&err, e.hook);
            let read = crate::read_from_writes(&err);
            let outcome = Outcome::of(resp.status);
            let resp = crate::refused_under(db, cfg, guard, read, resp);
            return Taken {
                outcome: Outcome::of(resp.status).max_with(outcome),
                resp,
                key: None,
                why: Some(err.to_string()),
            };
        }
        Ok(Took::Passed) => {
            return Taken {
                resp: Response::json(200, r#"{"taken":0}"#),
                outcome: Outcome::Ignored,
                key: None,
                why: None,
            }
        }
        // A first try still running as this one came has landed: this is
        // its duplicate, answered once that block is durable.
        Ok(Took::Held(k)) => {
            let durability = guard.flush();
            let at = crate::At::of(&guard);
            drop(guard);
            return held_answer(db, feed, e, k, durability, at);
        }
        Ok(Took::Ran(n)) => n,
    };
    let at = crate::At::of(&guard);
    // Durable whatever `--sync` says: a sender told 2xx never sends the
    // event again, and a crash after a buffered 2xx would lose it.
    let durability = guard.flush();
    drop(guard);
    let resp = Response::json(200, answer(e.key, n));
    match durable(db, feed, durability, at.seq) {
        Err(resp) => Taken {
            outcome: Outcome::of(resp.status),
            resp,
            key: None,
            why: Some("taken, and not yet durable or held by a standby".into()),
        },
        Ok(()) => Taken {
            resp: crate::with_seq(resp, Some(at)),
            outcome: Outcome::Taken,
            key: None,
            why: None,
        },
    }
}

impl Outcome {
    /// The state's outcome over the event's own: a refusal whose fsync
    /// then failed is the disk's.
    fn max_with(self, other: Outcome) -> Outcome {
        match (self, other) {
            (Outcome::Deferred, _) | (_, Outcome::Deferred) => Outcome::Deferred,
            (o, _) => o,
        }
    }
}

/// The answer an event's taking is: its key and the statements that took
/// it.
fn answer(key: &str, taken: u32) -> String {
    let mut s = String::from("{\"event\":");
    fennec_core::json::escape_into(&mut s, key);
    s.push_str(",\"taken\":");
    s.push_str(&taken.to_string());
    s.push('}');
    s
}

/// A duplicate found under the read lock: answered as its first taking
/// was, once the database's durability -- the key's block among it -- ran
/// with no lock held and a standby holds it, as the first's answer waited
/// for. After a storage error, the refusal every write meets: whether the
/// disk kept the first taking, no one here knows.
fn replayed(
    db: &Arc<RwLock<Database>>,
    feed: Option<&Feed>,
    g: std::sync::RwLockReadGuard<'_, Database>,
    e: &Event,
    k: fennec_core::engine::HeldKey,
) -> Taken {
    if let Err(err) = g.refuse_if_failed() {
        return Taken {
            resp: crate::error_response(&err),
            outcome: Outcome::Deferred,
            key: None,
            why: Some(err.to_string()),
        };
    }
    let at = crate::At::of(&g);
    let durability = g.durability();
    drop(g);
    let durability = match durability {
        Ok(d) => Ok(d),
        Err(err) => {
            crate::storage_failed(db, &err);
            Err(err)
        }
    };
    held_answer(db, feed, e, k, durability, at)
}

/// A held key's answer -- `Idempotent-Replayed`, `Fennec-Seq` where the
/// database stands at `at` -- once `durability` ran and a standby holds
/// the block that took the key ([`HeldKey::seq`](fennec_core::engine::HeldKey)).
fn held_answer(
    db: &Arc<RwLock<Database>>,
    feed: Option<&Feed>,
    e: &Event,
    k: fennec_core::engine::HeldKey,
    durability: fennec_core::error::Result<Option<fennec_core::engine::Durability>>,
    at: crate::At,
) -> Taken {
    let outcome = match k.same {
        true => Outcome::Replayed,
        false => Outcome::Conflict,
    };
    // The block that took the key, never the database's latest: a standby
    // behind on later writes holds this event already.
    match durable(db, feed, durability, k.seq) {
        Err(resp) => Taken {
            outcome: Outcome::of(resp.status),
            resp,
            key: None,
            why: Some("taken before, and not yet durable or held by a standby".into()),
        },
        Ok(()) => Taken {
            resp: crate::with_seq(
                Response::json(200, answer(e.key, k.taken)).header("Idempotent-Replayed", "true"),
                Some(at),
            ),
            outcome,
            key: None,
            why: None,
        },
    }
}

/// `durability` run with no lock held -- the fsync, the streams keeping
/// up -- then a standby holding write `upto`: the refusal to answer in the
/// 2xx's place otherwise, a failed fsync's 500 or a standby's 503.
fn durable(
    db: &Arc<RwLock<Database>>,
    feed: Option<&Feed>,
    durability: fennec_core::error::Result<Option<fennec_core::engine::Durability>>,
    upto: u64,
) -> std::result::Result<(), Response> {
    let durability = durability.map_err(|e| crate::error_response(&e))?;
    crate::await_durable(db, durability).map_err(|e| crate::error_response(&e))?;
    if !standby_holds(feed, upto) {
        return Err(Response::retry_after(
            503,
            "the event is taken and on disk, and no standby holds it yet: send it again, and \
             it is answered from its key once one does",
            1,
        ));
    }
    Ok(())
}

/// What a sender is told of a refusal: what a scoped caller is told -- the
/// status, a guard's reason, a bound -- never the engine's words, which
/// name values, nor a statement's name. The log line holds those.
fn told(e: &Error, hook: &str) -> Response {
    let status = crate::api::status_of(e);
    match e {
        Error::Quota(m) => crate::quota::engine_refusal(m),
        Error::Unmet(_) => {
            let reason = e.reason();
            let mut body = String::from("{\"error\":");
            fennec_core::json::escape_into(
                &mut body,
                reason.unwrap_or("a guard of a statement taking the event was not met"),
            );
            crate::api::reason_into(&mut body, reason);
            body.push('}');
            Response::json(412, body)
        }
        Error::Exists(m) if m.starts_with("no statement takes hook") => Response::error(409, m),
        Error::Exists(m) if m.contains(", which takes hook `") => Response::error(
            409,
            &format!(
                "a statement taking hook `{hook}` cannot run: define it again or drop it -- \
                 this server's log names it"
            ),
        ),
        Error::Duplicate(_) => Response::error(
            409,
            "the event's write met an id or a unique value held already",
        ),
        Error::Query(m) | Error::Type(m) => {
            let own = m
                .strip_prefix("statement `")
                .and_then(|r| r.split_once("`: "))
                .map(|(_, r)| r)
                .filter(|r| r.starts_with("`$") || r.starts_with("its `check`"));
            let bound = [
                "a call examines at most",
                "a write wakes the statements",
                "an event's",
            ]
            .iter()
            .any(|p| m.starts_with(p));
            match (own, bound) {
                (Some(r), _) => Response::error(status, r),
                (None, true) => Response::error(status, m),
                (None, false) => Response::error(
                    status,
                    "the event could not be taken: this server's log says why",
                ),
            }
        }
        Error::ReadOnly(_) => Response::error(403, "this server takes no writes"),
        Error::Io(_) | Error::Corrupt(_) => {
            Response::error(500, "a storage error: the event was not taken")
        }
        _ => Response::error(
            status,
            "the event could not be taken: this server's log says why",
        ),
    }
}

/// A digest of an event's body's bytes as they came, by which a second
/// body under a held key is told from the first: FNV-1a.
fn digest(body: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in body {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    h
}

// ------------------------------------------------- the hooks' refusal count

/// A hook's refusals counted by the sender's address and the hook's name,
/// apart from the tokens' (`audit::failed`): a verified event from an
/// address many share -- a cloud's egress, a sender's ranges -- must not
/// start the token-guessing count again for every client there, nor a
/// hook's holder clear its own guesses at another's secret with signed
/// events. The wait is the token count's shape: `--auth-delay` after the
/// first within a minute, doubled for each more, 5 s at most -- under the
/// 10 s GitHub gives an answer.
///
/// Keyed by the address and a digest of the name the decoded path names
/// ([`named`]), whether or not the server holds that hook -- so
/// `/_hooks/%73tripe` counts as `/_hooks/stripe`, which takes its event,
/// and whether a hook is there tells nothing -- every path naming no name
/// one count of its own: an entry is a few dozen bytes whatever was sent.
/// The table is `audit`'s type, one copy of its code.
#[derive(Default)]
struct Fails {
    counts: crate::audit::Failures,
    /// No sweep before then: the last one found every count within its
    /// minute, the oldest's ending then.
    full_until: Option<Instant>,
}

impl Fails {
    /// `key`'s count raised at `now`, or `None` where [`KEPT`] counts are
    /// held, each within its minute, and `key` is not among them: past
    /// them a refusal waits the longest and is counted nowhere. One address
    /// filling the table forgives no other's count -- it is swept of the
    /// counts past their minute, never cleared.
    fn count(&mut self, key: (IpAddr, u64), now: Instant) -> Option<u32> {
        if self.counts.len() >= KEPT && !self.counts.contains_key(&key) {
            if self.full_until.is_some_and(|t| now < t) {
                return None;
            }
            self.counts
                .retain(|_, (_, at)| now.duration_since(*at) <= WINDOW);
            self.full_until = None;
            if self.counts.len() >= KEPT {
                self.full_until = self.counts.values().map(|(_, at)| *at + WINDOW).min();
                return None;
            }
        }
        let e = self.counts.entry(key).or_insert((0, now));
        if now.duration_since(e.1) > WINDOW {
            e.0 = 0;
        }
        e.0 = e.0.saturating_add(1);
        e.1 = now;
        Some(e.0)
    }
}

fn fails() -> &'static Mutex<Fails> {
    static F: OnceLock<Mutex<Fails>> = OnceLock::new();
    F.get_or_init(Default::default)
}

/// A count's key: the address, canonical as `audit`'s are, and the name's
/// digest -- odd, so no name's is the count of the paths naming none, 0.
fn keyed(ip: IpAddr, name: Option<Named>) -> (IpAddr, u64) {
    let digested = |n: Named| {
        let mut d = digest(n.tenant.unwrap_or("").as_bytes());
        // A unit separator between, which neither name holds: no tenant
        // and hook split elsewhere digest alike.
        for &b in [0x1f].iter().chain(n.hook.as_bytes()) {
            d = (d ^ b as u64).wrapping_mul(0x100000001b3);
        }
        d | 1
    };
    (ip.to_canonical(), name.map_or(0, digested))
}

/// Entries with a refusal counted: a verified event asks the table only
/// when there is one.
static FAILING: AtomicU64 = AtomicU64::new(0);
/// Counts kept at most, each a few dozen bytes: past them, those past their
/// minute are swept, and a refusal finding none to sweep waits the longest.
const KEPT: usize = 10_000;
const WINDOW: Duration = Duration::from_secs(60);

/// How long a refusal to `ip` of an event naming hook `name` ([`named`])
/// waits, counted.
pub(crate) fn failed(ip: Option<IpAddr>, name: Option<Named>) -> Duration {
    let base = crate::audit::delay_ms();
    let Some(ip) = ip.filter(|_| base > 0) else {
        return Duration::ZERO;
    };
    let mut f = fails().lock().unwrap_or_else(|e| e.into_inner());
    let n = f.count(keyed(ip, name), Instant::now());
    FAILING.store(f.counts.len() as u64, Ordering::Relaxed);
    crate::audit::wait_after(base, n.unwrap_or(u32::MAX))
}

/// An event to a tenant's hook a router forwarded from `peer`, its node's
/// answer a 2xx: verified there, so the router's count of that sender's
/// refusals by the hook -- by the client the entry names, else the peer,
/// as the refusals were counted -- starts again, as the node's does
/// ([`cleared`]). Asked of every 2xx the router forwards: one holding no
/// count pays a load.
#[inline]
pub(crate) fn taken_through(req: &Request, peer: Option<std::net::SocketAddr>) {
    if FAILING.load(Ordering::Relaxed) > 0 && req.method == Method::Post {
        let ip = crate::audit::named().or(peer.map(|p| p.ip()));
        if let Some(n) = named(req).filter(|n| n.tenant.is_some()) {
            cleared(ip, n.tenant, n.hook);
        }
    }
}

/// A verified event of hook `name` from `ip`: its count starts again.
fn cleared(ip: Option<IpAddr>, tenant: Option<&str>, hook: &str) {
    if let Some(ip) = ip.filter(|_| FAILING.load(Ordering::Relaxed) > 0) {
        let mut f = fails().lock().unwrap_or_else(|e| e.into_inner());
        f.counts.remove(&keyed(ip, Some(Named { tenant, hook })));
        FAILING.store(f.counts.len() as u64, Ordering::Relaxed);
    }
}

// ----------------------------------------------------------------- watching

/// A hooks file was read, or a node holds a tenant's set: `/_metrics`
/// writes the hooks' families. A server with none scrapes the bytes it did.
static SERVING: AtomicBool = AtomicBool::new(false);

/// A node's tenant holds a set: the hooks' families are written from now.
pub(crate) fn serving() {
    SERVING.store(true, Ordering::Relaxed);
}
static UNVERIFIED_COUNT: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
static EVENTS: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];

/// The hooks' families, once a hooks file was read: events by outcome and
/// the refusals before a verified signature by check. No label names a
/// tenant, a hook, a statement or a collection.
pub(crate) fn metrics(out: &mut crate::metrics::Text) {
    if !SERVING.load(Ordering::Relaxed) {
        return;
    }
    let name = "fennec_hook_events_total";
    out.family(name, "counter", "Verified webhook events, by outcome.");
    for o in Outcome::ALL {
        let n = EVENTS[o as usize].load(Ordering::Relaxed);
        out.sample(name, &[("outcome", o.name())], n);
    }
    let name = "fennec_hook_unverified_total";
    out.family(
        name,
        "counter",
        "Webhook events refused before a verified signature, by the check that failed.",
    );
    for c in Check::ALL {
        let n = UNVERIFIED_COUNT[c as usize].load(Ordering::Relaxed);
        out.sample(name, &[("check", c.name())], n);
    }
}

/// Logs each hook of the hooks file no statement takes the events of, and
/// each statement taking a hook the file does not name -- as the server
/// starts and after a request defined or dropped a statement: a hook
/// configured and its statement forgotten answers every event 409, and a
/// statement whose hook has no secret every event 401.
pub fn log_unheld(hooks: Option<&Hooks>, db: &Database) {
    let t = db.statements();
    if let Some(hooks) = hooks {
        for h in hooks.iter() {
            if !t.iter().any(|s| s.hook.as_deref() == Some(h.name.as_str())) {
                crate::log!(
                    "hook `{}` of the hooks file: no statement takes its events, which are \
                     answered 409 until one does (`create statement ... on hook {}`)",
                    h.name,
                    h.name
                );
            }
        }
    }
    for s in t.iter() {
        let Some(h) = s.hook.as_deref() else {
            continue;
        };
        if hooks.and_then(|x| x.get(h)).is_none() {
            let without = match hooks {
                Some(_) => "which the hooks file does not name",
                None => "and this server has no hooks file (--hooks)",
            };
            crate::log!(
                "statement `{}` takes hook `{h}`'s events, {without}: they are answered 401 \
                 until a line names it",
                s.name
            );
        }
    }
}

/// [`log_unheld`] after `stmts` ran, where one defined a webhook consumer
/// or dropped a statement.
pub fn after_change<'a>(
    hooks: Option<&Hooks>,
    db: &Database,
    mut stmts: impl Iterator<Item = &'a Statement>,
) {
    let changed = stmts.any(|s| match s {
        Statement::CreateStatement(def) => def.hook.is_some(),
        Statement::DropStatement { .. } => hooks.is_some(),
        _ => false,
    });
    if changed {
        log_unheld(hooks, db);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        /// The HMACs computed on this thread, and the bytes of body they
        /// covered: every refusal before a verified signature does the
        /// same work.
        static MACS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
        /// The comparisons of a MAC with a signature made on this thread
        /// ([`compared`]): two a signature sent, whatever matched -- a `||`
        /// or a loop leaving at its match makes fewer, a comparison made
        /// around `compared` none.
        static COMPARED: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn counted(body: usize) {
        MACS.with(|m| {
            let (n, b) = m.get();
            m.set((n + 2, b + 2 * body));
        });
    }

    pub(super) fn compared_one() {
        COMPARED.with(|c| c.set(c.get() + 1));
    }

    fn macs() -> (usize, usize) {
        MACS.with(|m| m.replace((0, 0)))
    }

    fn compares() -> usize {
        COMPARED.with(|c| c.replace(0))
    }

    /// A file's server's hook `hook`, as a path names it.
    fn hook_named(hook: &str) -> Option<Named<'_>> {
        Some(Named { tenant: None, hook })
    }

    const STRIPE_SECRET: &str = "whsec_test_secret";
    /// Stripe's libraries' test payload (stripe-python's
    /// `DUMMY_WEBHOOK_PAYLOAD`) and test secret, signed at the `t` of the
    /// example header its docs show. Stripe publishes no signed vector --
    /// its libraries' tests sign at run time -- so `STRIPE_V1` was computed
    /// here, from that payload and secret by the documented scheme:
    /// `HMAC-SHA256("whsec_test_secret", "1492774577." + payload)` in hex.
    const STRIPE_PAYLOAD: &str = "{\n  \"id\": \"evt_test_webhook\",\n  \"object\": \"event\",\n  \"data\": { \"object\": { \"id\": \"rdr_123\", \"object\": \"terminal.reader\" } }\n}";
    const STRIPE_T: i64 = 1_492_774_577;
    const STRIPE_V1: &str = "d21d91b425674689e03346536b8fec6384a8721d43ebe269a1689a3ad22cba9c";
    /// The same under a second secret, as Stripe signs once a secret while
    /// one rolls.
    const STRIPE_OLD_SECRET: &str = "whsec_test_secret_old";
    const STRIPE_OLD_V1: &str = "f51ee7056227bbf5eae0c29f01899e5e501bbd7be9d704846825d81adfc279d8";
    /// GitHub's documented test values ("Validating webhook deliveries").
    const GITHUB_SECRET: &str = "It's a Secret to Everybody";
    const GITHUB_PAYLOAD: &str = "Hello, World!";
    const GITHUB_SIG: &str =
        "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
    /// The Standard Webhooks reference library's `test_sign_function`.
    const STANDARD_SECRET: &str = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
    const STANDARD_ID: &str = "msg_p5jXN8AQM9LWM0D4loKWxJek";
    const STANDARD_T: i64 = 1_614_265_330;
    const STANDARD_PAYLOAD: &str = "{\"test\": 2432232314}";
    const STANDARD_SIG: &str = "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=";

    /// A secret file's contents, as a test names them: `file:<secret>`
    /// holds the secret written after it, and `/run/<n>` the `n`th of
    /// [`SECRETS`], which no path names.
    fn read_secret(f: &str) -> std::io::Result<Vec<u8>> {
        if let Some(s) = f.strip_prefix("file:") {
            return Ok(s.as_bytes().to_vec());
        }
        match f
            .strip_prefix("/run/")
            .and_then(|n| n.parse::<usize>().ok())
        {
            Some(n) => Ok(SECRETS[n].as_bytes().to_vec()),
            // No file: what a secret pasted where its path goes names.
            None => Err(std::io::ErrorKind::NotFound.into()),
        }
    }

    const SECRETS: [&str; 3] = ["", "whsec_!!!", GITHUB_SECRET];

    fn hooks(text: &str) -> Hooks {
        Hooks::read(text, read_secret, 64 << 20).unwrap()
    }

    fn refused_at(text: &str) -> String {
        match Hooks::read(text, read_secret, 1 << 20) {
            Ok(_) => panic!("{text}: taken"),
            Err(e) => e,
        }
    }

    fn all() -> Hooks {
        hooks(&format!(
            "stripe stripe file:{STRIPE_SECRET} tolerance 1h\n\
             rolled stripe file:{STRIPE_OLD_SECRET} file:{STRIPE_SECRET} tolerance 1h\n\
             github github /run/2\n\
             standard standard file:{STANDARD_SECRET} tolerance 1h\n"
        ))
    }

    fn req(headers: &[(&str, &str)], body: &str) -> Request {
        Request {
            method: Method::Post,
            target: "/_hooks/x".into(),
            path: "/_hooks/x".into(),
            query: Vec::new(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
            keep_alive: false,
        }
    }

    fn stripe_req(sig: &str) -> Request {
        req(&[("Stripe-Signature", sig)], STRIPE_PAYLOAD)
    }

    fn standard_req(sig: &str, id: &str, t: &str, body: &str) -> Request {
        req(
            &[
                ("webhook-id", id),
                ("webhook-timestamp", t),
                ("webhook-signature", sig),
            ],
            body,
        )
    }

    fn at(secs: i64) -> i64 {
        secs * 1000
    }

    /// Each scheme against its sender's own example: Stripe's documented
    /// scheme over its libraries' test payload and secret, the signature
    /// computed here from them ([`STRIPE_V1`]); GitHub's documented test
    /// values; and the Standard Webhooks reference library's signing test
    /// -- verified, and with one byte of the body, the timestamp or the
    /// signature changed, refused.
    #[test]
    fn each_scheme_verifies_its_senders_example() {
        let h = all();
        let sig = format!("t={STRIPE_T},v1={STRIPE_V1}");
        let v = h
            .verify("stripe", &stripe_req(&sig), at(STRIPE_T))
            .ok()
            .unwrap();
        assert_eq!(v.at, Some(at(STRIPE_T)));
        // Stripe's documented header's layout: `v0` passed over, spaces
        // after the commas read.
        let sig = format!("t={STRIPE_T}, v1={STRIPE_V1}, v0=6ffbb59b2300aae63f272406069a9788598b792a944a07aba816edb039989a39");
        assert!(h.verify("stripe", &stripe_req(&sig), at(STRIPE_T)).is_ok());
        let mut body = STRIPE_PAYLOAD.to_string();
        body.replace_range(10..11, "X");
        let changed = req(
            &[("Stripe-Signature", &format!("t={STRIPE_T},v1={STRIPE_V1}"))],
            &body,
        );
        assert_eq!(
            h.verify("stripe", &changed, at(STRIPE_T)).err(),
            Some(Check::Signature)
        );
        let sig = format!("t={},v1={STRIPE_V1}", STRIPE_T + 1);
        assert_eq!(
            h.verify("stripe", &stripe_req(&sig), at(STRIPE_T)).err(),
            Some(Check::Signature)
        );
        let mut flipped = STRIPE_V1.to_string();
        flipped.replace_range(0..1, "e");
        let sig = format!("t={STRIPE_T},v1={flipped}");
        assert_eq!(
            h.verify("stripe", &stripe_req(&sig), at(STRIPE_T)).err(),
            Some(Check::Signature)
        );

        let gh = req(&[("X-Hub-Signature-256", GITHUB_SIG)], GITHUB_PAYLOAD);
        let v = h.verify("github", &gh, 0).ok().unwrap();
        assert_eq!(v.at, None, "GitHub signs no time");
        let gh = req(&[("X-Hub-Signature-256", GITHUB_SIG)], "Hello, World?");
        assert_eq!(h.verify("github", &gh, 0).err(), Some(Check::Signature));

        let t = STANDARD_T.to_string();
        let sw = standard_req(STANDARD_SIG, STANDARD_ID, &t, STANDARD_PAYLOAD);
        let v = h.verify("standard", &sw, at(STANDARD_T)).ok().unwrap();
        assert_eq!(v.at, Some(at(STANDARD_T)));
        // Beside an Ed25519 one, which is passed over.
        let two = format!("v1a,hnO3f9T8Ytu9HwrXslvumlUpqtNVqkhqw/enGzPCXe5BdqzCInXqYXFymVJaA7AZdpXwVLPo3mNl8EM+m7TBAg== {STANDARD_SIG}");
        let sw = standard_req(&two, STANDARD_ID, &t, STANDARD_PAYLOAD);
        assert!(h.verify("standard", &sw, at(STANDARD_T)).is_ok());
        for (id, t, body) in [
            ("msg_p5jXN8AQM9LWM0D4loKWxJeK", t.as_str(), STANDARD_PAYLOAD),
            (STANDARD_ID, "1614265331", STANDARD_PAYLOAD),
            (STANDARD_ID, t.as_str(), "{\"test\": 2432232315}"),
        ] {
            let sw = standard_req(STANDARD_SIG, id, t, body);
            assert_eq!(
                h.verify("standard", &sw, at(STANDARD_T)).err(),
                Some(Check::Signature)
            );
        }
    }

    /// Svix's names for Standard Webhooks' three headers -- what Clerk and
    /// Resend send -- verify as its own do: the reference example under
    /// `svix-*`. Both names at once, or one header of each, are a header
    /// failure: no reader takes the id of one and the signature of the
    /// other.
    #[test]
    fn svix_names_are_standard_webhooks() {
        let h = all();
        let t = STANDARD_T.to_string();
        let svix = |names: [&str; 3]| {
            req(
                &[
                    (names[0], STANDARD_ID),
                    (names[1], t.as_str()),
                    (names[2], STANDARD_SIG),
                ],
                STANDARD_PAYLOAD,
            )
        };
        let sv = svix(["svix-id", "svix-timestamp", "svix-signature"]);
        let v = h.verify("standard", &sv, at(STANDARD_T)).ok().unwrap();
        assert_eq!(v.at, Some(at(STANDARD_T)));
        assert_eq!(standard_id(&sv), STANDARD_ID, "the key is the id sent");
        let changed = req(
            &[
                ("svix-id", STANDARD_ID),
                ("svix-timestamp", t.as_str()),
                ("svix-signature", STANDARD_SIG),
            ],
            "{\"test\": 2432232315}",
        );
        assert_eq!(
            h.verify("standard", &changed, at(STANDARD_T)).err(),
            Some(Check::Signature)
        );
        for names in [
            ["svix-id", "webhook-timestamp", "webhook-signature"],
            ["webhook-id", "svix-timestamp", "svix-signature"],
            ["svix-id", "svix-timestamp", "webhook-signature"],
        ] {
            assert_eq!(
                h.verify("standard", &svix(names), at(STANDARD_T)).err(),
                Some(Check::Header),
                "{names:?}"
            );
        }
        let mut both = standard_req(STANDARD_SIG, STANDARD_ID, &t, STANDARD_PAYLOAD);
        both.headers.extend(sv.headers.iter().cloned());
        assert_eq!(
            h.verify("standard", &both, at(STANDARD_T)).err(),
            Some(Check::Header)
        );
    }

    /// The process's stand-in key verifies nothing: an event MAC'd under it
    /// is refused by a hook of one secret, whose second slot it fills, and
    /// by a hook that is not there.
    #[test]
    fn a_stand_in_key_never_verifies() {
        let h = all();
        let sig = |parts: &[&[u8]]| h.stand_in[0].mac(parts);
        let gh = format!("sha256={}", hex(&sig(&[GITHUB_PAYLOAD.as_bytes()])));
        let gh = req(&[("X-Hub-Signature-256", &gh)], GITHUB_PAYLOAD);
        assert_eq!(h.verify("github", &gh, 0).err(), Some(Check::Signature));
        assert_eq!(h.verify("nosuch", &gh, 0).err(), Some(Check::Hook));
        let t = STRIPE_T.to_string();
        let mac = sig(&[t.as_bytes(), b".", STRIPE_PAYLOAD.as_bytes()]);
        let st = stripe_req(&format!("t={t},v1={}", hex(&mac)));
        assert_eq!(
            h.verify("stripe", &st, at(STRIPE_T)).err(),
            Some(Check::Signature)
        );
        assert_eq!(h.get("stripe").unwrap().secrets, 1);
    }

    /// A `v0` or a `v1a` signature alone is no `v1`; nine are refused.
    #[test]
    fn only_v1_is_read_and_eight_at_most() {
        let h = all();
        let sig = format!("t={STRIPE_T},v0={STRIPE_V1}");
        assert_eq!(
            h.verify("stripe", &stripe_req(&sig), at(STRIPE_T)).err(),
            Some(Check::Header)
        );
        let t = STANDARD_T.to_string();
        let v1a = STANDARD_SIG.replacen("v1,", "v1a,", 1);
        let sw = standard_req(&v1a, STANDARD_ID, &t, STANDARD_PAYLOAD);
        assert_eq!(
            h.verify("standard", &sw, at(STANDARD_T)).err(),
            Some(Check::Header)
        );
        let eight = format!("t={STRIPE_T}") + &format!(",v1={STRIPE_V1}").repeat(8);
        assert!(h
            .verify("stripe", &stripe_req(&eight), at(STRIPE_T))
            .is_ok());
        let nine = format!("t={STRIPE_T}") + &format!(",v1={STRIPE_V1}").repeat(9);
        assert_eq!(
            h.verify("stripe", &stripe_req(&nine), at(STRIPE_T)).err(),
            Some(Check::Header)
        );
    }

    /// Headers sent twice, two `t`s, a `webhook-id` holding a `.`, and two
    /// schemes' headers at once are each a header failure.
    #[test]
    fn a_header_sent_twice_or_malformed_is_refused() {
        let h = all();
        let sig = format!("t={STRIPE_T},v1={STRIPE_V1}");
        let twice = req(
            &[("Stripe-Signature", &sig), ("stripe-signature", &sig)],
            STRIPE_PAYLOAD,
        );
        assert_eq!(
            h.verify("stripe", &twice, at(STRIPE_T)).err(),
            Some(Check::Header)
        );
        let two_t = format!("t={STRIPE_T},t={},v1={STRIPE_V1}", STRIPE_T + 600);
        assert_eq!(
            h.verify("stripe", &stripe_req(&two_t), at(STRIPE_T)).err(),
            Some(Check::Header)
        );
        let gh = req(
            &[
                ("X-Hub-Signature-256", GITHUB_SIG),
                ("X-Hub-Signature-256", GITHUB_SIG),
            ],
            GITHUB_PAYLOAD,
        );
        assert_eq!(h.verify("github", &gh, 0).err(), Some(Check::Header));
        let gh = req(
            &[
                ("X-Hub-Signature-256", GITHUB_SIG),
                ("X-GitHub-Delivery", "a"),
                ("X-GitHub-Delivery", "b"),
            ],
            GITHUB_PAYLOAD,
        );
        assert_eq!(h.verify("github", &gh, 0).err(), Some(Check::Header));
        let t = STANDARD_T.to_string();
        let dotted = standard_req(STANDARD_SIG, "msg.1", &t, STANDARD_PAYLOAD);
        assert_eq!(
            h.verify("standard", &dotted, at(STANDARD_T)).err(),
            Some(Check::Header)
        );
        let mut sw = standard_req(STANDARD_SIG, STANDARD_ID, &t, STANDARD_PAYLOAD);
        sw.headers.push(("webhook-timestamp".into(), t.clone()));
        assert_eq!(
            h.verify("standard", &sw, at(STANDARD_T)).err(),
            Some(Check::Header)
        );
        let both = req(
            &[
                ("Stripe-Signature", &sig),
                ("X-Hub-Signature-256", GITHUB_SIG),
            ],
            STRIPE_PAYLOAD,
        );
        assert_eq!(
            h.verify("stripe", &both, at(STRIPE_T)).err(),
            Some(Check::Header)
        );
        let none = req(&[], STRIPE_PAYLOAD);
        assert_eq!(
            h.verify("stripe", &none, at(STRIPE_T)).err(),
            Some(Check::Header)
        );
    }

    /// A timestamp a second past the tolerance either way is stale;
    /// GitHub's, with none, is taken whenever it comes.
    #[test]
    fn a_timestamp_past_the_tolerance_is_stale() {
        let h = all();
        let sig = format!("t={STRIPE_T},v1={STRIPE_V1}");
        let hour = MAX_TOLERANCE_MS;
        for now in [at(STRIPE_T) + hour, at(STRIPE_T) - hour] {
            assert!(h.verify("stripe", &stripe_req(&sig), now).is_ok());
        }
        for now in [at(STRIPE_T) + hour + 1000, at(STRIPE_T) - hour - 1000] {
            assert_eq!(
                h.verify("stripe", &stripe_req(&sig), now).err(),
                Some(Check::Stale)
            );
        }
        let gh = req(&[("X-Hub-Signature-256", GITHUB_SIG)], GITHUB_PAYLOAD);
        assert!(h.verify("github", &gh, i64::MAX / 2).is_ok());
    }

    /// Two secrets: an event signed by either taken, by neither refused.
    #[test]
    fn either_of_two_secrets_verifies() {
        let h = all();
        for v1 in [STRIPE_V1, STRIPE_OLD_V1] {
            let sig = format!("t={STRIPE_T},v1={v1}");
            assert!(h.verify("rolled", &stripe_req(&sig), at(STRIPE_T)).is_ok());
        }
        let sig = format!("t={STRIPE_T},v1={STRIPE_OLD_V1}");
        assert_eq!(
            h.verify("stripe", &stripe_req(&sig), at(STRIPE_T)).err(),
            Some(Check::Signature),
            "the old secret is no hook `stripe`'s"
        );
    }

    /// Every refusal before a verified signature does one work: two HMACs
    /// over the body up to the widest ceiling, and two comparisons a
    /// signature sent -- a hook missing, of another scheme, stale, its
    /// headers malformed, its signature wrong, and a verified event alike
    /// -- each answered byte for byte as the others, its check told apart
    /// only by what `verify` hands the log. Counted, never timed: a `||`
    /// between the two comparisons, a loop leaving at its match, or a
    /// comparison made around [`compared`] changes the count.
    #[test]
    fn every_refusal_before_the_signature_does_the_same_work() {
        let h = hooks(&format!(
            "stripe stripe file:{STRIPE_SECRET}\n\
             rolled stripe file:{STRIPE_OLD_SECRET} file:{STRIPE_SECRET}\n\
             github github /run/2\n\
             short stripe file:{STRIPE_SECRET} tolerance 10s\n"
        ));
        let now = at(STRIPE_T) + 60_000;
        let good = format!("t={STRIPE_T},v1={STRIPE_V1}");
        let mut wrong = STRIPE_V1.to_string();
        wrong.replace_range(63..64, "0");
        let first = format!("t={STRIPE_T},v1={STRIPE_V1},v1={wrong}");
        let last = format!("t={STRIPE_T},v1={wrong},v1={STRIPE_V1}");
        let old = format!("t={STRIPE_T},v1={STRIPE_OLD_V1},v1={wrong}");
        let cases: Vec<(&str, Request, Option<Check>, usize)> = vec![
            ("stripe", stripe_req(&good), None, 1),
            ("stripe", stripe_req(&first), None, 2),
            ("stripe", stripe_req(&last), None, 2),
            ("rolled", stripe_req(&first), None, 2),
            ("rolled", stripe_req(&old), None, 2),
            (
                "stripe",
                stripe_req(&format!("t={STRIPE_T},v1={wrong}")),
                Some(Check::Signature),
                1,
            ),
            ("nosuch", stripe_req(&good), Some(Check::Hook), 1),
            ("github", stripe_req(&good), Some(Check::Scheme), 1),
            ("short", stripe_req(&good), Some(Check::Stale), 1),
            ("stripe", stripe_req("t=1,v1=zz"), Some(Check::Header), 0),
            ("stripe", req(&[], STRIPE_PAYLOAD), Some(Check::Header), 0),
        ];
        macs();
        compares();
        let mut answers = Vec::new();
        for (name, r, check, sigs) in &cases {
            assert_eq!(h.verify(name, r, now).err(), *check, "{name}");
            let what = format!("{name}, {check:?}, {sigs} signatures");
            assert_eq!(macs(), (2, 2 * STRIPE_PAYLOAD.len()), "{what}");
            assert_eq!(compares(), 2 * sigs, "{what}: compared otherwise");
            if let Some(check) = check {
                answers.push(unverified(r, *check).body);
            }
        }
        assert!(answers.windows(2).all(|w| w[0] == w[1]), "one answer");
        assert_eq!(answers[0], br#"{"error":"the event does not verify"}"#);
    }

    /// A body past its hook's ceiling fails `size`, and costs what it costs
    /// under any other name: the HMACs run over it up to the widest ceiling
    /// of any hook, so a hook whose `max-body` was lowered or raised takes
    /// the time a missing one does.
    #[test]
    fn a_body_past_its_hooks_ceiling_costs_what_any_other_name_does() {
        let h = hooks(
            "exact github /run/2 max-body 13\nsmall github /run/2 max-body 64k\nbig github \
             /run/2 max-body 4m\n",
        );
        assert_eq!(h.widest, 4 << 20);
        let gh = req(&[("X-Hub-Signature-256", GITHUB_SIG)], GITHUB_PAYLOAD);
        assert!(
            h.verify("exact", &gh, 0).is_ok(),
            "13 bytes, at the ceiling"
        );
        let gh = req(&[("X-Hub-Signature-256", GITHUB_SIG)], "Hello, World!!");
        assert_eq!(h.verify("exact", &gh, 0).err(), Some(Check::Size));
        let large = "x".repeat(2 << 20);
        let gh = req(&[("X-Hub-Signature-256", GITHUB_SIG)], &large);
        macs();
        for (name, check) in [
            ("exact", Check::Size),
            ("small", Check::Size),
            ("big", Check::Signature),
            ("nosuch", Check::Hook),
        ] {
            assert_eq!(h.verify(name, &gh, 0).err(), Some(check), "{name}");
            assert_eq!(macs(), (2, 2 * large.len()), "{name}: another work");
        }
        // Past the widest, every name's HMACs stop there alike.
        let huge = "x".repeat(5 << 20);
        let gh = req(&[("X-Hub-Signature-256", GITHUB_SIG)], &huge);
        for name in ["big", "nosuch"] {
            assert!(h.verify(name, &gh, 0).is_err());
            assert_eq!(macs(), (2, 2 * (4 << 20)), "{name}");
        }
        // With no hook raising it, the widest is a mebibyte.
        assert_eq!(hooks("a github /run/2 max-body 64k\n").widest, CEILING);
    }

    /// A refusal is counted by the address and the name the decoded path
    /// names: `/_hooks/%73tripe` is `/_hooks/stripe`'s count, whether or not
    /// the server holds the hook, and every path naming no name -- a
    /// forger's 64 KiB ones among them -- one count, the table never
    /// growing with them. The log and the audit line show no such path.
    #[test]
    fn a_refusal_is_counted_by_the_name_its_path_names() {
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        let at = |target: &str, path: &str| {
            let mut r = req(&[], "");
            r.target = target.into();
            r.path = path.into();
            r
        };
        let plain = at("/_hooks/stripe", "/_hooks/stripe");
        let encoded = at("/_hooks/%73tripe", "/_hooks/stripe");
        assert_eq!(named(&plain).map(|n| n.hook), Some("stripe"));
        assert_eq!(named(&encoded).map(|n| n.hook), Some("stripe"));
        let mut f = Fails::default();
        let now = Instant::now();
        assert_eq!(f.count(keyed(ip, named(&plain)), now), Some(1));
        assert_eq!(f.count(keyed(ip, named(&encoded)), now), Some(2));
        let mapped: IpAddr = "::ffff:192.0.2.1".parse().unwrap();
        assert_eq!(f.count(keyed(mapped, named(&plain)), now), Some(3));
        assert_ne!(
            keyed(ip, hook_named("nosuch")),
            keyed(ip, hook_named("stripe"))
        );
        assert_ne!(keyed(ip, hook_named("stripe")).1, keyed(ip, None).1);

        let long = "a".repeat(60_000);
        for i in 0..1_000 {
            let r = at(&format!("/_hooks/{long}{i}"), &format!("/_hooks/{long}{i}"));
            assert_eq!(named(&r), None);
            f.count(keyed(ip, named(&r)), now);
        }
        assert_eq!(f.counts.len(), 2, "one count for every path naming no name");
        assert_eq!(f.counts[&keyed(ip, None)].0, 1_000);
        for path in [
            "/_hooks/",
            "/_hooks",
            "/_hooks/a/b",
            "/_hooks/1a",
            "/_hooks/a.b",
        ] {
            assert_eq!(named(&at(path, path)), None, "{path}");
        }
    }

    /// A router's 2xx to an event to a tenant's hook, its node's answer to
    /// an event it verified, starts the router's count of that sender's
    /// refusals by the hook again, as a verified event starts the node's
    /// ([`taken_through`]): nothing else does -- a `GET`, another tenant's
    /// hook, a file's server's path. The address is this test's alone.
    #[test]
    fn a_router_forwarding_a_verified_event_starts_its_count_again() {
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        let at = |method: Method, path: &str| {
            let mut r = req(&[], "");
            r.method = method;
            r.target = path.into();
            r.path = path.into();
            r
        };
        let event = at(Method::Post, "/t/acme/_hooks/stripe");
        let key = keyed(ip, named(&event));
        {
            let mut f = fails().lock().unwrap();
            assert_eq!(f.count(key, Instant::now()), Some(1));
            FAILING.store(f.counts.len() as u64, Ordering::Relaxed);
        }
        let held = || fails().lock().unwrap().counts.contains_key(&key);
        let peer = Some(std::net::SocketAddr::new(ip, 443));
        for other in [
            at(Method::Get, "/t/acme/_hooks/stripe"),
            at(Method::Post, "/t/beta/_hooks/stripe"),
            at(Method::Post, "/_hooks/stripe"),
        ] {
            taken_through(&other, peer);
        }
        assert!(held(), "started again by its own event alone");
        taken_through(&event, peer);
        assert!(!held());
    }

    /// One address filling the table forgives no other's count: a name
    /// past [`KEPT`] live counts waits the longest, counted nowhere, and
    /// the counts past their minute are swept, never the table cleared.
    #[test]
    fn a_full_table_is_swept_of_old_counts_never_cleared() {
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        let other: IpAddr = "198.51.100.7".parse().unwrap();
        let mut f = Fails::default();
        let t0 = Instant::now();
        assert_eq!(f.count(keyed(other, hook_named("stripe")), t0), Some(1));
        let later = t0 + Duration::from_secs(30);
        for i in 0..KEPT - 1 {
            f.count(keyed(ip, hook_named(&format!("h{i}"))), later);
        }
        assert_eq!(f.counts.len(), KEPT);
        assert_eq!(f.count(keyed(ip, hook_named("more")), later), None);
        assert_eq!(f.counts.len(), KEPT, "nothing cleared");
        assert_eq!(f.count(keyed(other, hook_named("stripe")), later), Some(2));
        // A sweep finding every count within its minute makes no room; once
        // they are past it, they are swept and the name counted.
        let early = t0 + WINDOW + Duration::from_secs(10);
        assert_eq!(f.count(keyed(ip, hook_named("more")), early), None);
        assert_eq!(f.counts.len(), KEPT);
        let swept = later + WINDOW + Duration::from_secs(1);
        assert_eq!(f.count(keyed(ip, hook_named("more")), swept), Some(1));
        assert_eq!(f.counts.len(), 1);
    }

    /// What a hooks file refuses at the start, naming the line and never a
    /// word of it: a secret pasted where its file's path goes is not
    /// echoed.
    #[test]
    fn a_hooks_file_is_refused_at_the_start() {
        for (text, says) in [
            ("a stripe file:\n", "is empty"),
            ("a stripe file:\n\n", "is empty"),
            ("a carrier file:x\n", "a scheme is"),
            ("a stripe file:x\na github file:y\n", "named twice"),
            ("1a stripe file:x\n", "a hook's name is"),
            ("a stripe file:x tolerance 0s\n", "a duration is"),
            ("a stripe file:x tolerance 5ü\n", "a duration is"),
            ("a stripe file:x tolerance 2h\n", "a tolerance is 1s to 1h"),
            ("a stripe file:x keep 23h\n", "1d at least"),
            ("a stripe file:x tolerance 1h keep 2h\n", "1d at least"),
            ("a stripe file:x key delivery\n", "`github` hook's"),
            ("a github file:x key both\n", "`body` or `delivery`"),
            ("a standard file:secret\n", "Standard Webhooks secret"),
            ("a standard /run/1\n", "Standard Webhooks secret"),
            ("a stripe /run/0\n", "is empty"),
            ("a stripe file:x file:y file:z\n", "one secret file, or two"),
            ("a stripe file:x max-body 2M\n", "--max-body"),
            ("a stripe file:x max-body lots\n", "is a size"),
            ("a stripe file:x keep 30d keep 31d\n", "given twice"),
            ("a stripe file:x keep\n", "expects a value"),
            ("a stripe file:x keep 30d often 1\n", "an option is"),
            ("a stripe\n", "one secret file, or two"),
            ("a\n", "a hook is"),
            (
                "a stripe whsec_pasted_by_mistake\n",
                "could not read a secret file",
            ),
        ] {
            let e = refused_at(text);
            assert!(e.contains(says), "{text:?}: {e}");
            assert!(e.starts_with("hooks file line "), "{e}");
            for secret in ["!!!", "pasted", "carrier", "1a", "often", "lots"] {
                assert!(!e.contains(secret), "a word of the line in {e}");
            }
        }
        assert!(refused_at("\n\na stripe\n").starts_with("hooks file line 3: "));
        // `keep` against the tolerance: twice it and a minute is short by
        // nothing, which the floor of a day covers for every tolerance.
        let h = hooks("a stripe file:x\nb github file:y key delivery keep 1d\n# a comment\n\n");
        assert_eq!(h.get("a").unwrap().keep_ms, 30 * DAY);
        assert_eq!(h.get("a").unwrap().tolerance_ms, TOLERANCE_MS);
        assert_eq!(h.get("b").unwrap().keep_ms, DAY);
        assert_eq!(h.get("b").unwrap().key, KeyBy::Delivery);
        assert_eq!(h.get("b").unwrap().ceiling, CEILING);
        assert_eq!((h.get("a").unwrap().secrets, h.widest), (1, CEILING));
        // A server's lower `--max-body` is a hook's ceiling unless given.
        let low = Hooks::read("a github /run/2\n", read_secret, 512 << 10).unwrap();
        assert_eq!(low.get("a").unwrap().ceiling, 512 << 10);
    }

    #[test]
    fn durations_read_as_written() {
        assert_eq!(duration("5m"), Ok(300_000));
        assert_eq!(duration("30d"), Ok(30 * DAY));
        assert!(duration("5").is_err());
        assert!(duration("m").is_err());
        assert!(duration("-1h").is_err());
    }
}

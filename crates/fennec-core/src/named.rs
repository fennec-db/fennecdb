//! Named statements: a FennecQL statement, or a block of them, defined once
//! in a database and called by name with typed arguments
//! (`docs/design/named-statements.md`).
//!
//! What this holds is the tree and what the tree alone decides: a
//! definition's signature and body as the parser reads them, the refusals
//! a body meets at its definition that need nothing but its text, and the
//! canonical tree's digest. The engine keeps a definition as a record of
//! its database (`engine/stored.rs`) and runs a call under the write lock
//! (`engine/call.rs`); who may call one, with which claims, is the server's
//! (`fennec_http::access`).

use crate::codec;
use crate::error::{Error, Result};
use crate::query::{
    ArithOp, CmpOp, Column, Count, Expr, Facet, Flat, Fuse, Lookup, Mark, Match, Near, Pick,
    Require, Rerank, Select, Sort, Statement,
};
use crate::schema::Schema;
use crate::value::{DataType, Value};

/// What a call of a statement answers, as far as it is typed
/// ([`Definition::answer`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// Rows, each with these items, in the order the answer writes them.
    Rows(Vec<Item>),
    /// The number of rows its last statement wrote: a write with no
    /// `returning`, answered `{"affected": n}`.
    Wrote,
    /// Rows nothing types yet, and why.
    Untyped(String),
}

/// An item of an answer's row: its type, `json` where it is not typed, and
/// whether it is never null.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub name: String,
    pub ty: DataType,
    pub required: bool,
    pub what: What,
}

/// What an [`Item`] is, for a reader to say so beside its type.
#[derive(Debug, Clone, PartialEq)]
pub enum What {
    Id,
    Count,
    /// A field of the collection the statement reads or writes, as its
    /// schema holds it.
    Field(crate::schema::Field),
    /// Not typed, and why.
    Untyped(String),
}

/// The grammar's epoch: raised by every change that could read a text
/// stored before it as another tree, or write a tree's canonical bytes
/// otherwise. A named statement is stored as its text, with the epoch it
/// was parsed under and its tree's digest ([`Definition::digest`]); an open
/// parses it again and marks it broken where the digest is another, rather
/// than run a tree its author never wrote. The corpus rule keeps that from
/// happening (`docs/design/fennecql.md`): no text that parsed reads
/// otherwise, so the epoch moves only where a change means to, and says
/// so. Here, beside the encoder whose change raises it, so the engine
/// writes it into a record; `fennec_ql::GRAMMAR_EPOCH` is this one.
///
/// 2: SQL's `x = any($n)` and `x <> all($n)` read as `x in $n` and `not (x
/// in $n)`, where they were a call of a function no build has, refused as
/// it ran -- a definition holding one is marked broken, to be defined
/// again.
///
/// 3: a number a statement writes is read as written within its range or
/// refused naming it (`Parser::int_in`), where it was clamped or cut:
/// `fuse k` past 1 000 000 (`k 99999999999` was `u32::MAX`); `near`'s
/// `ef`, `fuse`'s and `rerank`'s `candidates` and a snippet's words under 1
/// (read as 1, or refused as they ran) or past their range (natively `ef
/// 5000000000` was read whole); a parameter's `vector<N>` past 10^9 -- a
/// definition holding one no longer reads, and is marked broken ("its
/// text no longer reads: ... under grammar epoch 3, where it was defined
/// under 2: define it again"): its calls are refused, and a bound one
/// refuses every write that would wake it, until it is defined again or
/// dropped (`tests/stored.rs`). `@hnsw(ef_construction=4)` to `7`, built
/// as 8 before, reads as written; no definition holds a `create`.
pub const GRAMMAR_EPOCH: u32 = 3;

/// What reads a named statement's stored text back into its tree: the
/// parser's (`fennec_ql::definition`), which this crate does not hold.
pub type Reader = fn(&str) -> Result<Definition>;

static READER: std::sync::OnceLock<Reader> = std::sync::OnceLock::new();

/// Installs the reader every open, adoption and replica's apply in this
/// process reads stored statements with (`fennec_ql::install`), once: a
/// process holds one grammar. Without one an open keeps each statement as
/// it came, written again in every image and its digest unchecked, and
/// reads none into a tree.
pub fn read_with(reader: Reader) {
    let _ = READER.set(reader);
}

pub(crate) fn reader() -> Option<Reader> {
    READER.get().copied()
}

/// The most statements a body holds.
pub const MAX_BODY: usize = 64;
/// What `--call-max-rows` is unless told: the rows a call's statements
/// may examine, counted where a scan, an index, an ordered walk, `near`,
/// `match` or a `lookup`'s probe hands them over
/// (`Database::set_call_max_rows`). With `scans` at definition, it and the
/// write lock are what one call can cost every other writer.
pub const CALL_MAX_ROWS: usize = 100_000;
/// The most named statements a database holds.
pub const MAX_STATEMENTS: usize = 1_000;
/// The most statements bound to one collection's writes, refused past it,
/// never cut: each is a run a row of every write it binds.
pub const MAX_BOUND: usize = 16;
/// The most values a list claim hands a call, as a policy's rule is handed
/// at most as many (`fennec_http::access::MAX_CLAIM_VALUES`, this one): the
/// body spreads it after `in` on every call. Past it the call is refused,
/// never run with the list cut.
pub const MAX_CLAIM_VALUES: usize = 1_000;
/// The longest definition, in bytes of its text.
pub const MAX_TEXT: usize = 64 << 10;
/// The most parameters a signature declares.
pub const MAX_PARAMS: usize = 32;
/// The most claims a signature declares.
pub const MAX_CLAIMS: usize = 16;
/// The longest name, in bytes: `call_<name>`, an MCP tool's, then fits the
/// 64 characters a model's API takes for a tool's name.
pub const MAX_NAME: usize = 59;

/// `create [or replace] statement <name>(<parameters>) [claims (...)]
/// [public] [scans] [comment '<text>'] [check <expr>] as <body>`, or one
/// bound to a collection's writes, `create [or replace] statement <name>
/// after <event> [or <event> ...] <collection> [where <condition>]
/// [comment '<text>'] [scans] as <body>` ([`Binding`]), or one run by the
/// clock, `create [or replace] statement <name> (every <interval> | at
/// '<cron>') [in '<zone>'] [comment '<text>'] [scans] as <body>`
/// ([`Schedule`]), or one taking another service's events, `create [or
/// replace] statement <name> [($p <type> from <source>, ...)] on hook
/// <hook> [where <condition>] [comment '<text>'] [check <expr>] [scans] as
/// <body>` ([`Webhook`]). Two are equal where
/// everything but their [`Definition::text`] is: the same statement
/// written over one line or several. Its `Debug` form is its tree's, the
/// text left out, which `make fennecql-corpus` compares.
#[derive(Clone)]
pub struct Definition {
    pub name: String,
    /// `create or replace`: what the definition does to a name held, which
    /// is no part of the statement defined -- its digest leaves it out.
    pub replace: bool,
    /// In the signature's order: `$1` in the body is the first.
    pub params: Vec<Param>,
    /// The token's claims the body reads, numbered after the parameters:
    /// with two parameters, `$jwt.sub` is `Expr::Param(2)` when `sub` is
    /// the first claim declared.
    pub claims: Vec<Claim>,
    /// `public`: a body that writes and reads no claim, which writes the
    /// same rows the same way for every token granted it, says so.
    pub public: bool,
    /// `scans`: a writing body whose plan walks a collection no index
    /// narrows, acknowledged (checked against the schema in a later slice).
    pub scans: bool,
    /// `comment '<text>'`: what `describe`, the MCP tool and generated types
    /// say of it.
    pub comment: Option<String>,
    /// `check <expr>`: a condition over the parameters, the claims and
    /// `now()` alone, asked before a call takes any lock.
    pub check: Option<Expr>,
    /// One statement, or the statements of `begin ... end` in order: the
    /// last is what a call answers.
    pub body: Vec<Statement>,
    /// Written `begin ... end`: the text closes it, so no line after it
    /// goes on with it, which the shell reads (fennec-cli's `kind`). How it
    /// was written, not what it means: the digest leaves it out.
    pub block: bool,
    /// The definition's own bytes, from `create` to its end, comments and
    /// all, as the parser found them in the text it read (`fennec_ql`'s
    /// `parse`): what its record stores and an open reads again. The
    /// digest leaves it out, as it leaves out how the tree was written.
    pub text: String,
    /// `after <event> <collection> [where ...]`: a statement bound to a
    /// collection's writes, never called, with no parameters, claims,
    /// `check` or `public` -- its values are the row's. Boxed: a callable
    /// definition carries a pointer for it.
    pub after: Option<Box<Binding>>,
    /// `every <interval>` or `at '<cron>'`, `in '<zone>'`: a statement run
    /// by the clock, never called, with no parameters, claims, `check` or
    /// `public` -- no one hands it a value -- whose body reads `slot()`, the
    /// instant of the slot it runs for. Boxed, as a binding is.
    pub schedule: Option<Box<Schedule>>,
    /// `on hook <hook> [where <condition>]`: a statement taking the events
    /// another service sends, called by no token, with no claims or
    /// `public` -- each parameter reads a part of the event. Boxed, as a
    /// binding is.
    pub hook: Option<Box<Webhook>>,
}

/// What a bound statement is bound to (`docs/design/named-statements.md`,
/// *Write-bound statements*): the writes of one collection, by what
/// happened to a row -- [`Binding::INSERT`], [`Binding::SET`] and
/// [`Binding::DEL`], the bits its record will write -- and a condition over
/// the row as written and as it stood. Its body reads the pair through
/// [`Binding::places`]: `new.total` is `Expr::Param(k)`, `k` its place, so
/// the body is the statements every path of the engine takes.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub collection: String,
    /// The events, a bit each: SQL's `update` and `delete` read as `set`
    /// and `del`.
    pub events: u8,
    /// `where <condition>`: `new.<field>`, `old.<field>` and `now()` alone,
    /// each field an `Expr::Field` named as written (`new.status`), which
    /// the engine binds to the collection as a read's filter is bound, its
    /// collation and case fold with it.
    pub condition: Option<Expr>,
    /// What the body's `Expr::Param(k)` reads: the `k`th of its
    /// `new.<field>`s and `old.<field>`s, in the order they first appear.
    pub places: Vec<Place>,
}

impl Binding {
    /// A row made: `insert`, a `put` of an id not held.
    pub const INSERT: u8 = 1;
    /// A row written over: `set`, a `put` of a live row's id.
    pub const SET: u8 = 2;
    /// A row taken out: `del`.
    pub const DEL: u8 = 4;

    /// Whether every event it is bound to leaves a row as written: none is
    /// a `del`.
    pub fn reads_new(&self) -> bool {
        self.events & Self::DEL == 0
    }

    /// Whether every event it is bound to had a row as it stood: none is an
    /// `insert`.
    pub fn reads_old(&self) -> bool {
        self.events & Self::INSERT == 0
    }

    /// Each field of the bound collection the condition and the body read,
    /// as `new.<field>` or `old.<field>`, a path into a `json` field by
    /// the field: what its `depends` names, so dropping one is refused
    /// while the binding stands.
    pub fn fields(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut read = Vec::new();
        if let Some(c) = &self.condition {
            c.referenced_fields(&mut read);
        }
        let pairs = read
            .iter()
            .filter_map(|n| n.strip_prefix("new.").or_else(|| n.strip_prefix("old.")));
        for f in pairs.chain(self.places.iter().map(|p| p.field.as_str())) {
            if !out.iter().any(|o| o == f) {
                out.push(f.to_string());
            }
        }
        out
    }

    /// The first field the pair reads that `schema` -- the bound
    /// collection's -- does not hold: a field, a path, or a path into a
    /// field it holds.
    pub fn unheld(&self, schema: &Schema) -> Option<String> {
        self.fields().into_iter().find(|f| {
            let root = crate::schema::split_path(f).map_or(f.as_str(), |(r, _)| r);
            f != "id" && schema.path(f).is_none() && schema.field(root).is_none()
        })
    }

    /// The events as FennecQL writes them, `insert or set`.
    pub fn events_named(events: u8) -> String {
        let mut out = String::new();
        for (bit, word) in [
            (Self::INSERT, "insert"),
            (Self::SET, "set"),
            (Self::DEL, "del"),
        ] {
            if events & bit != 0 {
                if !out.is_empty() {
                    out.push_str(" or ");
                }
                out.push_str(word);
            }
        }
        out
    }

    /// The events [`Self::events_named`] wrote, read back -- in any order
    /// and case, SQL's `update` and `delete` as the grammar reads them --
    /// or `None` for a text naming another word.
    pub fn events_read(text: &str) -> Option<u8> {
        let mut words = text.split_whitespace();
        let mut events = 0;
        loop {
            let word = words.next()?.to_ascii_lowercase();
            events |= match word.as_str() {
                "insert" => Self::INSERT,
                "set" | "update" => Self::SET,
                "del" | "delete" => Self::DEL,
                _ => return None,
            };
            match words.next() {
                None => return Some(events),
                Some(or) if or.eq_ignore_ascii_case("or") => {}
                Some(_) => return None,
            }
        }
    }
}

/// A place of a bound statement's body: `new.<field>` or `old.<field>`.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    /// `old.`, the row as it stood; else `new.`, the row as written.
    pub old: bool,
    /// The field, or a path into a `json` one (`meta.source`).
    pub field: String,
}

/// What a statement run by the clock is defined to run at
/// (`docs/design/named-statements.md`, *Scheduled statements (F3)*): the
/// edges `bucket` cuts, or the times a cron expression names, on a zone's
/// clock. A file's server runs one (`fennec_http::schedule`); a database
/// no scheduler runs refuses its definition ([`SCHEDULED_UNRUN`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Schedule {
    pub when: When,
    /// `in '<zone>'` as [`crate::tz::spelled`] writes the zone
    /// `crate::tz::zone_of` reads: one of IANA's zones the binary's table
    /// holds, by the table's spelling of its name, or a fixed offset as
    /// `+hh:mm`. `None` is UTC, however written.
    pub zone: Option<String>,
}

/// `every <interval>` or `at '<cron>'`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum When {
    /// `every <interval>`: the starts of the buckets `bucket(t, <interval>,
    /// <zone>)` gives, counted from the epoch -- a week from a Monday --
    /// never from the definition, so a slot is the same instant wherever
    /// it is computed. Whole minutes at least ([`MIN_PERIOD`]), or months.
    Every(crate::query::Interval),
    /// `at '<cron>'`: its five fields read into the sets they name.
    At(Cron),
}

/// A cron expression's five fields, each the set of values it names as a
/// mask: bit `m` of `minutes` (0-59), `h` of `hours` (0-23), `d - 1` of
/// `days` (1-31), `m - 1` of `months` (1-12) and `w` of `weekdays` (0-6,
/// Sunday 0 and 7). Read by [`Cron::read`], which refuses an expression
/// with no meaning or two, so no mask is empty, and never both `days` and
/// `weekdays` short of every value: the day matches where each holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cron {
    pub minutes: u64,
    pub hours: u32,
    pub days: u32,
    pub months: u16,
    pub weekdays: u8,
}

/// The finest period a schedule runs at, in milliseconds: a minute, cron's
/// finest step and the sweep's period (F3's decision 3). `every 30s` and
/// `every 90s` are refused at definition.
pub const MIN_PERIOD: i64 = 60_000;

/// The scheduled statements a database holds at most: sixteen runs a
/// minute at the floor. Refused past it, never cut.
pub const MAX_SCHEDULED: usize = 16;

/// How far ahead of the clock a schedule's last mark, or its `since`, may
/// stand and be waited for (`Database::due`): a clock stepped back by at
/// most this long is taken for the wrong reading, and the slots up to the
/// mark, which ran, wait for it -- a pause of an hour at most. A mark
/// further ahead is taken for the wrong one, left by a clock that was
/// ahead, and the schedule goes on from the clock.
pub const AHEAD_MAX: i64 = 3_600_000;

/// What the engine refuses a scheduled statement's definition with where
/// no scheduler runs the database's -- a tenant node's until F3's slice 4,
/// the shell, the native library, a server's database a scheduler does not
/// watch: a schedule defined where nothing runs it would be a slot no one
/// runs without a word. A file's server lets one be defined
/// (`Database::allow_schedules`, `fennec_http::schedule::watch`).
pub const SCHEDULED_UNRUN: &str = "no scheduler runs this database's statements, so none \
                                   runs by the clock here: a scheduled statement is defined \
                                   on a file's server (`fennec-server <file>`), which runs it";

/// What a parameter, a claim, `check`, `public` or parentheses in a
/// scheduled statement are refused with.
pub const SCHEDULED_VALUES: &str = "a scheduled statement is called by no one: it takes no \
                                    parameters, claims, `check` or `public`, and reads the data, \
                                    `now()` and `slot()`";

/// What a schedule beside `after` is refused with: a binding runs on its
/// writer's row, a schedule on none.
pub const SCHEDULED_AND_BOUND: &str =
    "a statement runs by the clock or after a write, not both: two statements, `create statement \
     <n> every 1h as ...` and `create statement <m> after set <collection> as ...`";

/// `every <n><unit>`'s interval, `bucket`'s ([`crate::query::Interval`]),
/// held to the floor: whole minutes, a minute at least, or calendar months.
/// The refusal names the form to write.
pub fn period(n: i64, unit: &str) -> std::result::Result<crate::query::Interval, String> {
    use crate::query::Interval;
    let written = format!("{n}{unit}");
    match Interval::of(&Value::Text(written.as_str().into())) {
        Ok(Interval::Ms(step, _)) if step < MIN_PERIOD || step % MIN_PERIOD != 0 => Err(format!(
            "`every {written}`: a schedule runs at most once a minute, its period a whole number \
             of minutes -- `every 1m`, `every 15m`, `every 1h`"
        )),
        Ok(iv) => Ok(iv),
        Err(_) => Err(format!(
            "`every {written}`: a schedule's interval is a whole number of minutes, hours, days, \
             weeks, months or years -- `every 15m`, `1h`, `1d`, `1w`, `1mo`, `1y`"
        )),
    }
}

/// Each cron field's name, its lowest and highest value, and the names it
/// takes for them.
const CRON_FIELDS: [(&str, u32, u32); 5] = [
    ("minute", 0, 59),
    ("hour", 0, 23),
    ("day of the month", 1, 31),
    ("month", 1, 12),
    ("day of the week", 0, 7),
];
const MONTH_NAMES: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DAY_NAMES: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
/// The most days each month holds, 29 February admitted.
const MONTH_DAYS: [u32; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

impl Cron {
    /// `at '<cron>'`: minute, hour, day of month, month and day of week,
    /// each `*`, a number, a range `a-b`, a list `a,b`, or a step `*/n` or
    /// `a-b/n`, Sunday 0 or 7, and the three-letter English names of
    /// months and days in any case -- what pg_cron, Cloud Scheduler and
    /// PocketBase take. Refused, naming the form to write: other than five
    /// fields, a value outside its field, a step of 0, a range running
    /// backwards, Quartz's `?`, `L`, `W` and `#`, pg_cron's `$`, a macro, a
    /// day of month and a day of week both written as other than `*`, and
    /// an expression that never fires.
    pub fn read(text: &str) -> std::result::Result<Cron, String> {
        let t = text.trim();
        if let Some(m) = t.strip_prefix('@') {
            return Err(cron_macro(m));
        }
        for (c, whose) in [
            ('?', "Quartz's `?`"),
            ('#', "Quartz's `#`"),
            ('$', "pg_cron's `$`"),
        ] {
            if t.contains(c) {
                return Err(format!(
                    "`{t}`: {whose} is not read here -- five fields of `*`, numbers, ranges `a-b`, \
                     lists `a,b` and steps `*/n`"
                ));
            }
        }
        let f: Vec<&str> = t.split_ascii_whitespace().collect();
        if f.len() == 6 {
            return Err(format!(
                "`{t}` has six fields, Quartz's with seconds first: a schedule runs at most once \
                 a minute -- five fields, `minute hour day-of-month month day-of-week`, or \
                 `every 1m`"
            ));
        }
        if f.len() != 5 {
            return Err(format!(
                "`{t}`: a cron expression is five fields, `minute hour day-of-month month \
                 day-of-week` -- `'0 3 * * *'` is 03:00 every day"
            ));
        }
        let mut masks = [0u64; 5];
        for (k, m) in masks.iter_mut().enumerate() {
            *m = cron_field(f[k], k)?;
        }
        // Sunday is 0 and 7: one bit.
        let weekdays = ((masks[4] | masks[4] >> 7) & 0x7f) as u8;
        let cron = Cron {
            minutes: masks[0],
            hours: masks[1] as u32,
            days: masks[2] as u32,
            months: masks[3] as u16,
            weekdays,
        };
        // By the text, never the masks: POSIX's crontab, Vixie cron and
        // pg_cron count a field not starting with `*` as restricted and run
        // on either day where both are, so `0 0 1-31 * mon` runs every day
        // there and `0 0 13 * 0-6` too, where the masks ANDed would run on
        // Mondays and on the 13th alone. Only `*` is open to every reader:
        // a step is no POSIX form, Vixie cron counts any field starting
        // with `*` open, `*/2` too, robfig's Go cron `*/1` and not `*/2`,
        // so `*/1` is held to the rule as well.
        if f[2] != "*" && f[4] != "*" {
            return Err(format!(
                "`{t}` restricts both the day of the month (`{}`) and the day of the week \
                 (`{}`) -- a field is open as `*` alone -- which cron runs on either and a \
                 reader takes for both: two statements, `at '{} {} {} {} *'` and `at '{} {} * {} \
                 {}'`, say either, and one of them with a guard in its body says both",
                f[2], f[4], f[0], f[1], f[2], f[3], f[0], f[1], f[3], f[4]
            ));
        }
        // The lowest day named, held to the longest month named.
        let first = cron.days.trailing_zeros() + 1;
        let fits = (0..12).any(|m| cron.months & 1 << m != 0 && first <= MONTH_DAYS[m]);
        if !fits {
            return Err(format!(
                "`{t}` never runs: no month it names has a day it names (29 February is one)"
            ));
        }
        Ok(cron)
    }
}

/// A cron macro's refusal: each named as the schedule it is.
#[cold]
fn cron_macro(m: &str) -> String {
    let lower = m.to_ascii_lowercase();
    let as_written = match lower.as_str() {
        "hourly" => "`every 1h`",
        "daily" | "midnight" => "`every 1d`",
        "weekly" => "`at '0 0 * * 0'`, Sunday's midnight (`every 1w` starts on a Monday)",
        "monthly" => "`every 1mo`",
        "yearly" | "annually" => "`every 1y`",
        "reboot" => {
            return "`@reboot` is no time: a schedule runs at the times its clock reads, never as \
                    a server starts"
                .into()
        }
        _ => "five fields, `minute hour day-of-month month day-of-week`",
    };
    format!("`@{m}` is written {as_written}")
}

/// One cron field read into the mask of the values it names, bit `v -
/// lowest` for each; the day of the week's 7 is bit 7, folded into
/// Sunday's by the caller.
fn cron_field(text: &str, k: usize) -> std::result::Result<u64, String> {
    let (what, lo, hi) = CRON_FIELDS[k];
    let mut mask = 0u64;
    for item in text.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((r, s)) => (r, Some(s)),
            None => (item, None),
        };
        let (a, b) = match range.split_once('-') {
            _ if range == "*" => (lo, hi),
            Some((a, b)) => (cron_value(a, k)?, cron_value(b, k)?),
            None if step.is_some() => {
                return Err(format!(
                    "`{item}`: a step follows `*` or a range -- `*/n`, or `{range}-{hi}/n` from \
                     {range}"
                ))
            }
            None => {
                let v = cron_value(range, k)?;
                (v, v)
            }
        };
        if b < a {
            let sunday = if k == 4 && b == 0 {
                ", Sunday 7 at its end"
            } else {
                ""
            };
            return Err(format!(
                "`{item}` runs backwards: a range runs from the lower {what} to the higher{sunday}"
            ));
        }
        let step = match step {
            None => 1,
            Some(s) => match small_number(s) {
                Some(n) if (1..=hi).contains(&n) => n,
                _ => {
                    return Err(format!(
                        "`{item}`: a step of the {what} field is 1 to {hi}{}",
                        if k < 2 {
                            ", and a period past it `every`'s"
                        } else {
                            ""
                        }
                    ))
                }
            },
        };
        let mut v = a;
        while v <= b {
            mask |= 1 << (v - lo);
            v += step;
        }
    }
    Ok(mask)
}

/// A number of one to three digits, all a cron field's value or step can
/// be: no sign, no space, nothing `str::parse` would take beside.
fn small_number(text: &str) -> Option<u32> {
    if text.is_empty() || text.len() > 3 {
        return None;
    }
    text.bytes().try_fold(0u32, |n, b| {
        b.is_ascii_digit().then(|| n * 10 + u32::from(b - b'0'))
    })
}

/// One value of cron field `k`: a number within it, or a month's or a
/// day's three-letter name.
fn cron_value(text: &str, k: usize) -> std::result::Result<u32, String> {
    let (what, lo, hi) = CRON_FIELDS[k];
    let names: &[&str] = match k {
        3 => &MONTH_NAMES,
        4 => &DAY_NAMES,
        _ => &[],
    };
    if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(text)) {
        return Ok(i as u32 + lo);
    }
    if let Some(v) = small_number(text).filter(|v| (lo..=hi).contains(v)) {
        return Ok(v);
    }
    let quartz = text.trim_start_matches(|c: char| c.is_ascii_digit());
    if !quartz.is_empty() && quartz.chars().all(|c| matches!(c, 'L' | 'W' | 'l' | 'w')) {
        return Err(format!(
            "`{text}`: Quartz's `L` and `W` are not read here -- a {what} is {lo} to {hi}"
        ));
    }
    let named = match k {
        3 => ", or `jan` to `dec`",
        4 => " (Sunday 0 or 7), or `sun` to `sat`",
        _ => "",
    };
    Err(format!(
        "`{text}` is no {what}: a {what} is {lo} to {hi}{named}"
    ))
}

/// What a webhook consumer takes (`docs/design/named-statements.md`,
/// *Webhook consumers (F4)*): the events another service sends to the hook
/// `name`, each taken where its `condition` passes it, its parameters read
/// from the event as its `sources` say, one a parameter in the signature's
/// order (`Database::take_event`). A file's server and a tenant node take
/// them (`fennec_http::hooks`); every other way of defining one refuses it
/// ([`HOOKED_UNTAKEN`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Webhook {
    /// The hook's name, a word as a statement's is ([`valid_name`]): the
    /// path segment its sender is told, `/_hooks/<name>`. As written: a
    /// path's segment is read in its case.
    pub name: String,
    /// `where <condition>`: `event.<path>`, `hook.event` and literals
    /// alone, each path an `Expr::Field` named as read, `event.` and
    /// `hook.` in lower case and every key as written -- read against the
    /// event's JSON as a `json` field's path is.
    pub condition: Option<Expr>,
    /// What each parameter reads, in the signature's order.
    pub sources: Vec<Source>,
}

/// What a webhook consumer's parameter reads: `$p <type> from <source>`.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// `event.<key>...`, a path into the event's body, its keys as the
    /// JSON names them, joined by dots, which no key holds -- as
    /// [`Value::at_path`] reads them -- or `event` alone, no key: the whole
    /// body, a `json` parameter's only. One `String`, not a list: a
    /// definition's reading and refusals took 1.2 KB of the server's code
    /// more with one.
    Event(String),
    /// `hook.id`: the event's key, a text.
    HookId,
    /// `hook.at`: the time its signature covers, a timestamp, `null` where
    /// its scheme signs none.
    HookAt,
    /// `hook.event`: GitHub's `X-GitHub-Event`, a text, `null` elsewhere.
    HookEvent,
}

/// What defining a webhook consumer is refused with where no server takes
/// the database's events -- the shell, the native library, a server's
/// database its route does not serve: one stored there would be a hook no
/// sender reaches, without a word. A file's server and a tenant node let
/// one be defined (`Database::allow_hooks`), their routes taking the
/// events (`fennec_http::hooks`, `fennec_http::tenant_hooks`).
pub const HOOKED_UNTAKEN: &str = "no server takes this database's webhook events, so no \
                                  webhook consumer is defined here: one is defined on a \
                                  file's server (`fennec-server --file <path>`), which takes \
                                  its events at `/_hooks/<hook>`, or in a tenant of a node, \
                                  at `/t/<tenant>/_hooks/<hook>`";

/// The statements one hook's events are taken by, at most: every one whose
/// condition passes an event takes it, in name order, in one block. Refused
/// past it, never cut -- sixteen, the event destinations a Stripe account
/// has, so an app reading a dozen of its event types through one endpoint
/// is not out of statements first.
pub const MAX_HOOKED: usize = 16;

/// The value at `keys` -- a path's keys past `event`, joined by dots --
/// inside an event's body, `null` where the path leads nowhere: what a
/// webhook consumer's parameter and condition read (`Source::Event`). A
/// key reads an object's member by its name, as [`Value::at_path`] does,
/// and a list's element by its place where it is one RFC 6901 (JSON
/// Pointer) writes -- `0`, or digits with no leading zero, from the first:
/// `event.data.object.lines.data."0".price.id` reads the first line's
/// price. A JSON value is an object or a list, never both, so a key of
/// digits names one thing wherever it stands: `"0"` the member `0` of an
/// object and the first element of a list. `01`, `-1` and a place past
/// the end read nothing, as a missing member does. A body is read exactly
/// (a list stays a list, never a vector's `f32`s), so only `List` is
/// indexed.
///
/// No `json` field's path reads a list: FennecQL writes no key of digits
/// but a hook's (the lexer refuses `.0`, and a quoted key is a hook's
/// alone), so a field's path never meets one, and the reader a field's
/// read goes through -- `Value::at_path`, and `codec::decode_path_into`
/// off the stored bytes, which the module holds -- is left as it is. A
/// field's key of digits, once written, would take this rule in both.
#[cold]
#[inline(never)]
pub fn event_at<'v>(body: &'v Value, keys: &str) -> &'v Value {
    let mut v = body;
    for k in keys.split('.') {
        let next = match v {
            Value::List(items) => place(k).and_then(|i| items.get(i)),
            _ => v.member(k),
        };
        match next {
            Some(x) => v = x,
            None => return &crate::value::NULL,
        }
    }
    v
}

/// A list's place as RFC 6901 writes one -- `0`, or digits with no leading
/// zero -- or `None`.
fn place(k: &str) -> Option<usize> {
    let b = k.as_bytes();
    let canonical =
        !b.is_empty() && b.iter().all(u8::is_ascii_digit) && (b.len() == 1 || b[0] != b'0');
    if !canonical {
        return None;
    }
    k.parse().ok()
}

/// What a claim, `$jwt` or `public` in a webhook consumer is refused with.
pub const HOOKED_VALUES: &str = "a webhook consumer is called by no token: it reads no claim and \
                                 is not `public` -- its values are the event's, each read by a \
                                 parameter, `$amount int from event.data.object.amount`";

/// What `on hook` beside `after` or a schedule is refused with: one
/// statement, one way to run.
pub const HOOKED_AND_OTHER: &str = "a statement takes a hook's events, runs by the clock or runs \
                                    after a write -- one of them: two statements, `create \
                                    statement <n>(...) on hook <hook> as ...` and `create \
                                    statement <m> every 1h as ...`";

/// What a parameter in a webhook consumer's condition is refused with.
pub const HOOKED_CONDITION_VALUES: &str = "a hook's condition reads the event and literals, and \
                                           no parameter: a statement whose parameters would \
                                           refuse an event never refuses one its condition \
                                           passes over -- a parameter's test is its `check`";

/// What a parameter of a webhook consumer reading no part of the event is
/// refused with.
pub const HOOKED_PARAMS: &str = "each parameter of a webhook consumer reads the event: `$amount \
                                 int from event.data.object.amount`, `$e json from event` for the \
                                 whole body, or `hook.id`, `hook.at`, `hook.event`";

/// Why `keys`, a path's keys past `event` joined by dots, names no part
/// of an event, if it names none: past `MAX_PATH_KEYS` keys, as a `json`
/// field's path, a key of no character, or `*` -- SQL's `o.*`, which the
/// lexer reads as a name's end: the whole body is `from event`.
#[cold]
fn keys_refused(keys: &str) -> Option<String> {
    use crate::value::MAX_PATH_KEYS;
    let mut n = 0;
    for k in keys.split('.') {
        if k.is_empty() || k == "*" {
            return Some(format!(
                "`event.{keys}` names no part of the event: a key holds a character at least, \
                 and `*` none -- the whole body is a `json` parameter's, `$e json from event`"
            ));
        }
        n += 1;
    }
    (n > MAX_PATH_KEYS).then(|| format!("a path names at most {MAX_PATH_KEYS} keys past `event`"))
}

/// Why a webhook consumer's parameter `p` cannot read `s`, if it cannot,
/// from their text alone: the whole body is a `json` parameter's, a path
/// names what [`keys_refused`] lets it, `hook.id` and `hook.event` are
/// texts and `hook.at` a timestamp -- a `json` parameter taking any of
/// them. Out of line: a definition's alone.
#[cold]
#[inline(never)]
pub fn source_refused(p: &Param, s: &Source) -> Option<String> {
    let held = |ty: DataType| p.ty == ty || p.ty == DataType::Json;
    let (read, is) = match s {
        Source::Event(keys) if keys.is_empty() && p.ty != DataType::Json => {
            return Some(format!(
                "`${}`: the whole event is a `json` parameter's, `${} json from event` -- a \
                 typed one reads a path into it, `from event.data.object.id`",
                p.name, p.name
            ))
        }
        Source::Event(keys) if !keys.is_empty() => {
            return keys_refused(keys).map(|why| format!("`${}`: {why}", p.name));
        }
        Source::HookId if !held(DataType::Text) => ("hook.id", "text"),
        Source::HookEvent if !held(DataType::Text) => ("hook.event", "text"),
        Source::HookAt if !held(DataType::Timestamp) => ("hook.at", "timestamp"),
        _ => return None,
    };
    Some(format!(
        "`${}`: `{read}` is a {is}, read by a `{is}` or a `json` parameter",
        p.name
    ))
}

/// Why a webhook consumer's condition is refused, if it is: it picks the
/// events its statement takes from the event alone -- its paths, `hook.event`
/// and literals -- so no parameter, no row and no other collection, no
/// `now()`, no `slot()`, no `expired()` and no fold, and a path names what
/// [`keys_refused`] lets it. Its names are the parser's to hold to
/// `event.` and `hook.event`; a tree made otherwise is held here too.
#[cold]
#[inline(never)]
pub fn hook_condition_refused(e: &Expr) -> Option<String> {
    let mut why = None;
    let mut walk = |e: &Expr| {
        if why.is_some() {
            return;
        }
        why = match e {
            Expr::Field(f) if f == "hook.event" => None,
            Expr::Field(f) if f.starts_with("event.") => keys_refused(&f["event.".len()..]),
            Expr::Field(f) => Some(format!(
                "`{f}`: a hook's condition reads the event, `event.<path>`, and `hook.event`"
            )),
            Expr::Param(_) | Expr::InParam(..) => Some(HOOKED_CONDITION_VALUES.into()),
            Expr::InSelect(..) => Some(
                "a hook's condition reads the event alone: a read of a collection is its body's \
                 guard, `get ... require 1 else '<why>'`"
                    .into(),
            ),
            e if e.aggregate_name().is_some() => {
                Some("a hook's condition reads one event, and folds none".into())
            }
            e if e.asks_expired() => Some("a hook's condition reads no row: no `expired()`".into()),
            Expr::Call(n, a)
                if a.is_empty()
                    && (n.eq_ignore_ascii_case("now") || n.eq_ignore_ascii_case("slot")) =>
            {
                Some(
                    "a hook's condition reads the event and literals, and no clock: `now()` is \
                     its body's, the time of the event's taking, and `slot()` a scheduled \
                     statement's"
                        .into(),
                )
            }
            _ => None,
        };
    };
    each_expr(e, &mut walk);
    why
}

/// The rows one statement may wake the statements bound to its writes for
/// (`docs/design/named-statements.md`, *When a binding runs*), a row
/// counted once whatever number of bindings it passes: the 10 001st
/// refuses the write as it is written, never cut -- the bound `near`,
/// `match` and a facet hold to.
pub const MAX_WOKEN: usize = 10_000;

/// `$name type [= literal]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: DataType,
    /// `= <literal>`: what it is when a call leaves it out; `Some(Null)`
    /// for `= null`. None: a call must give it, never null.
    pub default: Option<Value>,
}

/// `name type [from "<claim>"]`: a claim of the caller's token, read as
/// `$jwt.<name>`.
#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    pub name: String,
    pub ty: DataType,
    /// `from "cognito:groups"`: the token's claim, where FennecQL does not
    /// write its name as a name. None: the claim is `name` itself -- the
    /// parser writes `from "sub"` on a claim named `sub` as none.
    pub from: Option<String>,
}

impl Claim {
    /// The token's claim this reads.
    pub fn claim(&self) -> &str {
        self.from.as_deref().unwrap_or(&self.name)
    }
}

/// `call <name> [<arguments>] [as <claims>]`, and SQL's `CALL <name>(...)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub name: String,
    pub args: Args,
    /// `as {sub: 'alice'}` or `as $2`: the claims the server's token calls
    /// a statement reading claims with. `Named` or `Param` alone.
    pub claims: Option<Args>,
}

/// A call's arguments, matched to the signature by the engine, since the
/// parser reads no schema.
#[derive(Debug, Clone, PartialEq)]
pub enum Args {
    /// `call purge`, `CALL purge()`.
    None,
    /// `call transfer {from: 1, to: 2}`, and SQL's `CALL transfer(from =>
    /// 1, to => 2)`, which is the same call.
    Named(Vec<(String, Expr)>),
    /// `call transfer $1`: a parameter's object, its members by name.
    Param(usize),
    /// SQL's `CALL transfer(1, 2, 300)`: by place, then any by name
    /// (`CALL transfer(1, to => 2, amount => 300)`), as PostgreSQL's.
    Placed(Vec<Expr>, Vec<(String, Expr)>),
}

impl Args {
    /// Every expression it holds.
    pub fn exprs(&self) -> impl Iterator<Item = &Expr> {
        let (placed, named): (&[Expr], &[(String, Expr)]) = match self {
            Args::None | Args::Param(_) => (&[], &[]),
            Args::Named(n) => (&[], n),
            Args::Placed(p, n) => (p, n),
        };
        placed.iter().chain(named.iter().map(|(_, e)| e))
    }

    /// The highest `$n` it reads, one-based as [`Expr::max_param`]'s.
    pub fn max_param(&self) -> usize {
        match self {
            Args::Param(i) => i + 1,
            _ => self.exprs().map(Expr::max_param).max().unwrap_or(0),
        }
    }
}

/// Whether `name` is one a named statement may have: ASCII letters, digits
/// and `_`, starting with a letter, at most [`MAX_NAME`] bytes -- a path
/// segment with nothing to escape, and an identifier in every SDK.
pub fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_NAME
        && b[0].is_ascii_alphabetic()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// What a named statement's name is refused with.
pub fn name_refused(name: &str) -> String {
    format!(
        "`{name}` is no statement's name: ASCII letters, digits and `_`, starting with a \
         letter, at most {MAX_NAME} bytes"
    )
}

/// What a hook's name is refused with: a statement's rule, since it is the
/// path segment its sender is told.
#[cold]
pub fn hook_name_refused(name: &str) -> String {
    format!(
        "`{name}` is no hook's name: ASCII letters, digits and `_`, starting with a letter, at \
         most {MAX_NAME} bytes -- the path its sender is told, `/_hooks/<name>`"
    )
}

impl Definition {
    /// Whether the body writes: an `insert`, a `set` or a `del` among it.
    pub fn writes(&self) -> bool {
        self.body.iter().any(|s| !s.is_read_only())
    }

    /// Whether the body or the `check` reads a claim: a parameter numbered
    /// past the declared parameters.
    pub fn reads_claims(&self) -> bool {
        let n = self.params.len();
        self.body.iter().any(|s| s.max_param() > n)
            || self.check.as_ref().is_some_and(|c| c.max_param() > n)
    }

    /// What its tree alone refuses at its definition: the statement of the
    /// body it is about (`None` for the signature) and why. The checks
    /// against the schema -- names, indexes, the plan behind `scans` --
    /// are the engine's, at the definition and at each schema change's
    /// commit.
    pub fn refused(&self) -> Option<(Option<usize>, String)> {
        let at = |i: usize, why: String| Some((Some(i), why));
        if self.body.is_empty() {
            return Some((None, "a body holds at least one statement".into()));
        }
        if self.body.len() > MAX_BODY {
            return at(
                MAX_BODY,
                format!("a body holds at most {MAX_BODY} statements"),
            );
        }
        if let Some(why) = self.check.as_ref().and_then(check_refused) {
            return Some((None, why));
        }
        for (i, s) in self.body.iter().enumerate() {
            if let Some(why) = body_refused(s) {
                return at(i, why.into());
            }
            let mut own = None;
            each_collection(s, &mut |c| {
                if own.is_none() && crate::declared::own(c) {
                    own = Some(c.to_string());
                }
            });
            if let Some(c) = own {
                return at(
                    i,
                    format!(
                        "`{c}` is the database's own: a body names no collection whose name \
                         starts with `_`"
                    ),
                );
            }
        }
        // A webhook consumer's answer goes to its sender, which reads no
        // rows, and its values are the event's.
        if let Some(h) = &self.hook {
            return self.hooked_refused(h);
        }
        // A scheduled statement's answer goes nowhere either, and no one
        // hands it a value.
        if self.schedule.is_some() {
            return self.scheduled_refused();
        }
        // A bound statement's answer goes nowhere and it reads no claim:
        // its values are the row's, its writes on behalf of whoever wrote
        // it.
        if let Some(b) = &self.after {
            return self.bound_refused(b);
        }
        let last = self.body.len() - 1;
        if let Some(why) = unnamed_answer(&self.body[last]) {
            return at(last, why.into());
        }
        if self.public && !self.claims.is_empty() {
            return Some((
                None,
                "`public` says the body writes on behalf of nobody in particular, and the \
                 statement declares claims: one or the other"
                    .into(),
            ));
        }
        if self.writes() && !self.public && !self.reads_claims() {
            return Some((
                None,
                format!(
                    "statement `{}` writes and reads no claim, so every token granted it \
                     writes the same rows the same way: read the caller's claims \
                     (`claims (sub text)`, `$jwt.sub`), or say so with `public`",
                    self.name
                ),
            ));
        }
        None
    }

    /// What a bound statement's body refuses beyond a callable one's: its
    /// rows go nowhere, so a `get` is a guard, with `require`, and nothing
    /// answers `returning`. Out of line: only a binding's definition asks.
    #[cold]
    #[inline(never)]
    fn bound_refused(&self, _: &Binding) -> Option<(Option<usize>, String)> {
        for (i, s) in self.body.iter().enumerate() {
            let why = match s {
                Statement::Select(sel) if sel.require.is_none() => {
                    "a bound statement answers no one, so its `get` is a guard, which counts \
                     its rows: `get accounts select id where id = new.account require 1 else \
                     'no such account'`"
                }
                _ if s.pick().is_some_and(|p| p.returning.is_some()) => {
                    "a bound statement answers no one: its writes say nothing with `returning`"
                }
                _ => continue,
            };
            return Some((Some(i), why.into()));
        }
        None
    }

    /// What a scheduled statement refuses beyond a callable one's: no one
    /// calls it, so nothing hands it a value and its rows go nowhere -- a
    /// `get` is a guard, with `require`, nothing answers `returning`, and a
    /// body of guards alone is a run that changes nothing. Its parameters,
    /// claims, `check`, `public`, `new.` and `old.` the parser refuses at
    /// the word; a tree made otherwise is held here too.
    #[cold]
    #[inline(never)]
    fn scheduled_refused(&self) -> Option<(Option<usize>, String)> {
        if self.after.is_some() {
            return Some((None, SCHEDULED_AND_BOUND.into()));
        }
        if !self.params.is_empty() || !self.claims.is_empty() || self.check.is_some() || self.public
        {
            return Some((None, SCHEDULED_VALUES.into()));
        }
        for (i, s) in self.body.iter().enumerate() {
            let why = match s {
                Statement::Select(sel) if sel.require.is_none() => {
                    "a scheduled statement answers no one, so its `get` is a guard, which counts \
                     its rows: `get holds select id where state = 'held' limit 1 require 1 else \
                     'nothing held'`"
                }
                _ if s.pick().is_some_and(|p| p.returning.is_some()) => {
                    "a scheduled statement answers no one: its writes say nothing with `returning`"
                }
                _ => continue,
            };
            return Some((Some(i), why.into()));
        }
        if !self.writes() {
            return Some((
                None,
                "a scheduled statement's body of guards alone changes nothing a run could leave: \
                 it writes, `insert`, `set` or `del`"
                    .into(),
            ));
        }
        None
    }

    /// What a webhook consumer refuses beyond a callable one's: no token
    /// calls it, so it reads no claim and is not `public`; each parameter
    /// reads a part of the event its type can hold ([`source_refused`]);
    /// its condition reads the event alone ([`hook_condition_refused`]);
    /// and a sender reads no rows, so a `get` is a guard, with `require`,
    /// nothing answers `returning`, and a body of guards alone, which
    /// takes an event and changes nothing, is refused. The parser refuses
    /// each at its word; a tree made otherwise is held here too.
    #[cold]
    #[inline(never)]
    fn hooked_refused(&self, h: &Webhook) -> Option<(Option<usize>, String)> {
        if self.after.is_some() || self.schedule.is_some() {
            return Some((None, HOOKED_AND_OTHER.into()));
        }
        if !valid_name(&h.name) {
            return Some((None, hook_name_refused(&h.name)));
        }
        if !self.claims.is_empty() || self.public || self.reads_claims() {
            return Some((None, HOOKED_VALUES.into()));
        }
        if h.sources.len() != self.params.len() {
            return Some((None, HOOKED_PARAMS.into()));
        }
        if let Some(why) =
            (self.params.iter().zip(&h.sources)).find_map(|(p, s)| source_refused(p, s))
        {
            return Some((None, why));
        }
        if let Some(why) = h.condition.as_ref().and_then(hook_condition_refused) {
            return Some((None, why));
        }
        for (i, s) in self.body.iter().enumerate() {
            let why = match s {
                Statement::Select(sel) if sel.require.is_none() => {
                    "a webhook consumer answers its sender, which reads no rows, so its `get` is a \
                     guard, which counts them: `get orders select id where ref = $order limit 1 \
                     require 1 else 'no such order'`"
                }
                _ if s.pick().is_some_and(|p| p.returning.is_some()) => {
                    "a webhook consumer answers its sender, which reads no rows: its writes say \
                     nothing with `returning`"
                }
                _ => continue,
            };
            return Some((Some(i), why.into()));
        }
        if !self.writes() {
            return Some((
                None,
                "a webhook consumer's body of guards alone takes an event and changes nothing: it \
                 writes, `insert`, `set` or `del`"
                    .into(),
            ));
        }
        None
    }

    /// The canonical tree's digest: [`crate::hll::hash`] of
    /// [`Self::canonical`]. Equal for every text that parses to the same
    /// statement -- white space, comments, a keyword's case, SQL's spelling
    /// of a FennecQL form, `as <statement>` or `as begin <statement> end`,
    /// `create` or `create or replace` -- and another wherever the meaning
    /// differs, a literal's bits, a name, a type, a marker. A body statement
    /// that is no body's (a schema change, a `call`) has none.
    pub fn digest(&self) -> Result<u64> {
        Ok(crate::hll::hash(&self.canonical()?))
    }

    /// The canonical tree, as bytes. A node is its kind's byte, then each
    /// field that holds something -- not `None`, `false`, empty or `0`
    /// (an `offset` of 0, an ascending key) -- as its tag and its value, in
    /// the order of their tags, then a 0. A text is its length and its
    /// UTF-8 bytes, a count a varint, a literal and a type as the file
    /// writes them (`codec::encode_value`, `codec::encode_type`), an
    /// expression its variant's byte and its operands. So a field added to
    /// the tree that holds nothing in a statement leaves that statement's
    /// digest as it was: only a statement using it reads otherwise. A
    /// tag is never reused nor renumbered; a change that writes an old
    /// tree otherwise raises [`GRAMMAR_EPOCH`].
    pub fn canonical(&self) -> Result<Vec<u8>> {
        let Definition {
            name,
            replace: _,
            params,
            claims,
            public,
            scans,
            comment,
            check,
            body,
            block: _,
            text: _,
            after,
            schedule,
            hook,
        } = self;
        let mut c = Canon(Vec::new());
        c.node(1);
        c.text(1, name);
        c.list(2, params, |c, p| {
            c.node(2);
            c.text(1, &p.name);
            c.ty(2, &p.ty);
            if let Some(v) = &p.default {
                c.tag(3);
                c.value(v);
            }
            c.end();
        });
        c.list(3, claims, |c, k| {
            c.node(3);
            c.text(1, &k.name);
            c.ty(2, &k.ty);
            c.opt_text(3, k.from.as_deref());
            c.end();
        });
        c.flag(4, *public);
        c.flag(5, *scans);
        c.opt_text(6, comment.as_deref());
        c.opt_expr(7, check);
        if !body.is_empty() {
            c.tag(8);
            c.uint(body.len() as u64);
            for s in body {
                c.statement(s)?;
            }
        }
        // Tag 9 only where the statement is bound: every callable
        // statement's bytes, and so every stored digest, stay as they were.
        if let Some(b) = after {
            let Binding {
                collection,
                events,
                condition,
                places,
            } = &**b;
            c.tag(9);
            c.node(4);
            c.text(1, collection);
            c.num(2, u64::from(*events));
            c.opt_expr(3, condition);
            c.list(4, places, |c, p| {
                c.node(5);
                c.flag(1, p.old);
                c.text(2, &p.field);
                c.end();
            });
            c.end();
        }
        // Tag 10 only where the statement is scheduled, as tag 9: no other
        // definition's bytes move.
        if let Some(s) = schedule {
            c.tag(10);
            c.schedule(s);
        }
        // Tag 11 only where the statement takes a hook's events, as tags 9
        // and 10.
        if let Some(h) = hook {
            c.tag(11);
            c.webhook(h);
        }
        c.end();
        Ok(c.0)
    }
}

impl std::fmt::Debug for Definition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("Definition");
        d.field("name", &self.name)
            .field("replace", &self.replace)
            .field("params", &self.params)
            .field("claims", &self.claims)
            .field("public", &self.public)
            .field("scans", &self.scans)
            .field("comment", &self.comment)
            .field("check", &self.check)
            .field("body", &self.body)
            .field("block", &self.block);
        // Only where it holds one, as the canonical tree writes it: the
        // corpus compares every callable definition's form as it was.
        if let Some(b) = &self.after {
            d.field("after", b);
        }
        if let Some(s) = &self.schedule {
            d.field("schedule", s);
        }
        if let Some(h) = &self.hook {
            d.field("hook", h);
        }
        d.finish()
    }
}

impl PartialEq for Definition {
    fn eq(&self, o: &Definition) -> bool {
        let Definition {
            name,
            replace,
            params,
            claims,
            public,
            scans,
            comment,
            check,
            body,
            block,
            text: _,
            after,
            schedule,
            hook,
        } = self;
        (name, replace, params, claims, public, scans)
            == (
                &o.name, &o.replace, &o.params, &o.claims, &o.public, &o.scans,
            )
            && (comment, check, body, block) == (&o.comment, &o.check, &o.body, &o.block)
            && *after == o.after
            && *schedule == o.schedule
            && *hook == o.hook
    }
}

impl Definition {
    /// The fields the last statement answers, by name: what a call's
    /// caller reads, which a replacement keeps.
    pub fn answers(&self) -> Vec<String> {
        let mut out = Vec::new();
        match self.body.last() {
            Some(Statement::Select(sel)) => {
                out.extend(sel.project.iter().flatten().cloned());
                out.extend(
                    sel.computed
                        .iter()
                        .chain(&sel.aggregate)
                        .map(|c| c.name.clone()),
                );
                if sel.count {
                    out.push("count".into());
                }
                for l in sel.lookup.iter().flat_map(Lookup::chain) {
                    out.extend(l.project.iter().flatten().map(|f| cat(&l.collection, f)));
                }
            }
            Some(s) => {
                let returning = s.pick().and_then(|p| p.returning.as_ref());
                out.extend(returning.into_iter().flatten().flatten().cloned());
            }
            None => {}
        }
        out
    }

    /// What a call answers, typed as far as it is: a `get`'s list or a
    /// write's `returning` over one collection, each plain field as `of`
    /// (its collection's schema) types it, `id` and a count integers, and
    /// every other item -- computed, aggregated, renamed with `as`, a path
    /// into a json field, a `lookup`'s rows -- untyped, until the engine
    /// infers the types of expressions. One place for both readers of it:
    /// `fennec types`, which writes it in every language, and MCP's
    /// `call_<name>` tools, whose `outputSchema` it is.
    pub fn answer<'s>(&self, of: impl Fn(&str) -> Option<&'s Schema>) -> Answer {
        let untyped = |name: &str, why: String| Item {
            name: name.to_string(),
            ty: DataType::Json,
            required: false,
            what: What::Untyped(why),
        };
        let count = |name: &str| Item {
            name: name.to_string(),
            ty: DataType::Int,
            required: true,
            what: What::Count,
        };
        // A name a list or `returning` writes as it is: `id`, a field, or a
        // path into a json field.
        let named = |s: Option<&Schema>, name: &str| -> Item {
            if name == "id" {
                return Item {
                    name: "id".into(),
                    ty: DataType::Int,
                    required: true,
                    what: What::Id,
                };
            }
            match s.and_then(|s| s.field(name)) {
                Some(f) => Item {
                    name: name.to_string(),
                    ty: f.ty.clone(),
                    required: f.required,
                    what: What::Field(f.clone()),
                },
                None => match name.split_once('.') {
                    Some((f, _)) => untyped(name, format!("a path into `{f}`")),
                    None => untyped(name, "no field of its collection".into()),
                },
            }
        };
        // An item written as more than a field: a group's key that is a field
        // as that field, a count an integer, and the rest untyped.
        let column = |s: Option<&Schema>, c: &Column, aggregates: bool| -> Item {
            match &c.expr {
                Expr::Field(f) if *f == c.name => named(s, f),
                Expr::Field(f) => untyped(&c.name, format!("`{f}` given another name")),
                Expr::Call(f, _) if f == "count" => count(&c.name),
                _ if aggregates => untyped(&c.name, "an aggregate or a group's key".into()),
                _ => untyped(&c.name, "an expression".into()),
            }
        };
        match self.body.last() {
            Some(Statement::Select(sel)) => {
                if sel.flat.is_some() {
                    return Answer::Untyped("a join's rows".into());
                }
                let s = of(&sel.collection);
                if sel.count {
                    return Answer::Rows(vec![count("count")]);
                }
                let mut out = Vec::new();
                if !sel.aggregate.is_empty() {
                    out.extend(sel.aggregate.iter().map(|c| column(s, c, true)));
                } else if let Some(list) = &sel.project {
                    for n in list {
                        if sel.marks.iter().any(|m| m.label() == *n) {
                            out.push(untyped(n, "a highlight or a snippet".into()));
                        } else if let Some(c) = sel.computed.iter().find(|c| c.name == *n) {
                            out.push(column(s, c, false));
                        } else {
                            out.push(named(s, n));
                        }
                    }
                } else {
                    return Answer::Untyped("a list naming no fields".into());
                }
                // The rows a `lookup` attached go under its collection's name.
                if let Some(l) = &sel.lookup {
                    let why = format!("the rows of `{}` that `lookup` attached", l.collection);
                    out.push(untyped(&l.collection, why));
                }
                Answer::Rows(out)
            }
            Some(st) => {
                let collection = match st {
                    Statement::Update { collection, .. }
                    | Statement::Delete { collection, .. }
                    | Statement::Put { collection, .. } => Some(collection.as_str()),
                    _ => None,
                };
                match (collection, st.pick().and_then(|p| p.returning.as_ref())) {
                    (Some(c), Some(Some(list))) => {
                        Answer::Rows(list.iter().map(|n| named(of(c), n)).collect())
                    }
                    (Some(_), _) => Answer::Wrote,
                    (None, _) => Answer::Untyped("a statement answering no rows".into()),
                }
            }
            None => Answer::Untyped("an empty body".into()),
        }
    }

    /// Why a replacement of `old` by this one is refused, if it is: it keeps
    /// the statement's contract -- whether it writes, the claims it reads
    /// and their types, the fields it answers -- or it is a new statement,
    /// written as `drop` and `create`, so no grant starts meaning more with
    /// no line of a policy changed.
    pub fn breaks_contract(&self, old: &Definition) -> Option<String> {
        if let Some(why) = kind_kept(&self.name, self.kind(), old.kind()) {
            return Some(why);
        }
        // A scheduled statement is called by no one and woken by no write,
        // and neither is a hook's: no grant hangs on a schedule, a zone, a
        // hook, a condition, the parameters an event fills or a body.
        if self.schedule.is_some() || self.hook.is_some() {
            return None;
        }
        // A bound statement's contract is what a `wake` line grants: to
        // cause what it writes, each collection by the kind of write. A
        // condition, a value, a guard or a field a `set` writes is within
        // it, and so is writing less; writing a collection or a kind of
        // write it did not is a new grant's.
        if self.after.is_some() {
            let was = old.written();
            let (c, kind) = self.written().into_iter().find(|w| !was.contains(w))?;
            let how = match kind {
                Binding::INSERT => "inserts into",
                Binding::SET => "sets rows of",
                _ => "deletes from",
            };
            return Some(format!(
                "statement `{}` {how} `{c}`, which it did not: a bound statement's replacement \
                 writes no collection and no kind of write it did not -- write `drop statement \
                 {}` and `create statement`, which leaves every grant naming it naming nothing",
                self.name, self.name
            ));
        }
        let what = if self.writes() != old.writes() {
            match self.writes() {
                true => "writes, where it read",
                false => "reads, where it wrote",
            }
        } else if self.claims.len() != old.claims.len()
            || self
                .claims
                .iter()
                .zip(&old.claims)
                .any(|(a, b)| a.claim() != b.claim() || a.ty != b.ty)
        {
            "reads other claims"
        } else if self.answers() != old.answers() {
            "answers other fields"
        } else {
            return None;
        };
        Some(format!(
            "statement `{}` {what}: a replacement keeps whether it writes, the claims it reads \
             and the fields it answers -- write `drop statement {}` and `create statement`, \
             which leaves every grant naming it naming nothing",
            self.name, self.name
        ))
    }

    /// Its kind, as its record keeps it.
    pub fn kind(&self) -> Kind<'_> {
        match (self.bound_as(), &self.schedule) {
            (Some((c, e)), _) => Kind::Bound(c, e),
            (None, Some(_)) => Kind::Scheduled,
            (None, None) if self.hook.is_some() => Kind::Hooked,
            (None, None) => Kind::Called,
        }
    }

    /// What it binds, as its record keeps it: the collection and the
    /// events. `None` for a callable statement.
    pub fn bound_as(&self) -> Option<(&str, u8)> {
        self.after
            .as_ref()
            .map(|b| (b.collection.as_str(), b.events))
    }

    /// What the body writes, by collection and kind of write: each
    /// `(collection, Binding::INSERT | SET | DEL)` once. Counted as the
    /// write path counts what a statement may do to a row (`put_with`):
    /// `insert` and `put ... if absent` make one; `put`, which writes over
    /// a row its id names, and `if absent else set` make or set one; `set`
    /// sets and `del` takes out. A `put` counted as an `insert` alone let
    /// a replacement turning `insert` into `put` start setting rows no
    /// grant said it would.
    pub fn written(&self) -> Vec<(&str, u8)> {
        let mut out: Vec<(&str, u8)> = Vec::new();
        for s in &self.body {
            let (c, kinds) = match s {
                Statement::Put {
                    collection,
                    insert,
                    else_set,
                    ..
                } => (
                    collection,
                    match *insert && else_set.is_none() {
                        true => Binding::INSERT,
                        false => Binding::INSERT | Binding::SET,
                    },
                ),
                Statement::Update { collection, .. } => (collection, Binding::SET),
                Statement::Delete { collection, .. } => (collection, Binding::DEL),
                _ => continue,
            };
            for kind in [Binding::INSERT, Binding::SET, Binding::DEL] {
                let w = (c.as_str(), kind);
                if kinds & kind != 0 && !out.contains(&w) {
                    out.push(w);
                }
            }
        }
        out
    }

    /// What the body depends on among the collections `schema_of` holds
    /// (`Stored::depends`, in fennec-core's engine): each collection it
    /// names; each of their fields it reads or writes -- a name the
    /// collection does not hold, an alias or a join's, is the statement's
    /// own checks' to judge; the index each `lookup` probes; and the unique
    /// indexes of each collection it inserts into, whose refusal of a
    /// duplicate it relies on. A collection none holds, or a `lookup` no
    /// index answers, is refused, as running the statement would be.
    pub fn depends<'a>(
        &self,
        schema_of: &dyn Fn(&str) -> Option<&'a crate::schema::Schema>,
    ) -> Result<Vec<String>> {
        let mut d = Depends {
            schema_of,
            out: Vec::new(),
        };
        // A binding's collection first, and each field its pair reads:
        // dropping `orders`, or its `status`, is refused while it stands.
        if let Some(b) = &self.after {
            let s = d.collection(&b.collection)?;
            for f in b.fields() {
                d.field(s, &f);
            }
        }
        for s in &self.body {
            d.statement(s)?;
        }
        Ok(d.out)
    }
}

/// What kind of statement a definition is, as its record says it: called,
/// bound to a collection's writes (and which), run by the clock, or taking
/// a hook's events.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind<'a> {
    Called,
    Bound(&'a str, u8),
    Scheduled,
    /// Whichever hook: a replacement moves it freely.
    Hooked,
}

/// Why a replacement is refused for its kind, if it is: a callable
/// statement replaced by a bound, a scheduled or a hook's one, or the
/// other way, or a bound one by one bound to another collection or other
/// events. A `call` or `wake` line naming it would name something else, so
/// it is a new statement, written as `drop` and `create`. Asked from the
/// record where the old text reads no tree: its op and its fields say
/// both. A scheduled one changes its schedule, zone and body freely, and a
/// hook's its hook, condition, parameters and body: no grant hangs on
/// them.
pub fn kind_kept(name: &str, now: Kind, was: Kind) -> Option<String> {
    let what = match (now, was) {
        (a, b) if a == b => return None,
        (Kind::Bound(c, e), Kind::Bound(..)) => format!(
            "is bound after `{}` on `{c}`, where it was bound to other writes",
            Binding::events_named(e)
        ),
        (now, was) => {
            let is = match now {
                Kind::Called => "is called",
                Kind::Bound(..) => "is bound to writes",
                Kind::Scheduled => "runs by the clock",
                Kind::Hooked => "takes a hook's events",
            };
            let was = match was {
                Kind::Called => "it was called",
                Kind::Bound(..) => "it was bound to writes",
                Kind::Scheduled => "it ran by the clock",
                Kind::Hooked => "it took a hook's events",
            };
            format!("{is}, where {was}")
        }
    };
    Some(format!(
        "statement `{name}` {what}: a replacement keeps its kind, and a bound one the collection \
         and events it is bound to -- write `drop statement {name}` and `create statement`, \
         which leaves every grant naming it naming nothing"
    ))
}

/// `c.f`.
fn cat(c: &str, f: &str) -> String {
    let mut s = String::with_capacity(c.len() + f.len() + 1);
    s.push_str(c);
    s.push('.');
    s.push_str(f);
    s
}

/// [`Definition::depends`]' walk.
struct Depends<'s, 'a> {
    schema_of: &'s dyn Fn(&str) -> Option<&'a crate::schema::Schema>,
    out: Vec<String>,
}

impl<'a> Depends<'_, 'a> {
    fn add(&mut self, d: String) {
        if !self.out.contains(&d) {
            self.out.push(d);
        }
    }

    fn collection(&mut self, c: &str) -> Result<&'a crate::schema::Schema> {
        let s = (self.schema_of)(c).ok_or_else(|| Error::NotFound(format!("collection `{c}`")))?;
        self.add(c.to_string());
        Ok(s)
    }

    /// `name` of `s`, where `s` holds it: a field, a path, or a path's
    /// field.
    fn field(&mut self, s: &crate::schema::Schema, name: &str) {
        if name == "id" {
            return;
        }
        let held = match s.field(name).is_some() || s.path(name).is_some() {
            true => Some(name),
            false => crate::schema::split_path(name)
                .map(|(root, _)| root)
                .filter(|root| s.field(root).is_some()),
        };
        if let Some(f) = held {
            self.add(cat(&s.name, f));
        }
    }

    /// The fields `e` reads of `s`, and each inner `get` under it.
    fn expr(&mut self, s: &crate::schema::Schema, e: &Expr) -> Result<()> {
        let mut names = Vec::new();
        e.referenced_fields(&mut names);
        for n in &names {
            self.field(s, n);
        }
        let mut inner: Vec<&Select> = Vec::new();
        each_expr(e, &mut |x| {
            if let Expr::InSelect(_, sel) = x {
                inner.push(sel);
            }
        });
        for sel in inner {
            self.select(sel)?;
        }
        Ok(())
    }

    fn sorts(&mut self, s: &crate::schema::Schema, order: &[Sort]) -> Result<()> {
        for o in order {
            match &o.expr {
                Some(e) => self.expr(s, e)?,
                None => self.field(s, &o.field),
            }
        }
        Ok(())
    }

    fn names(&mut self, s: &crate::schema::Schema, names: &[String]) {
        for n in names {
            self.field(s, n);
        }
    }

    fn statement(&mut self, st: &Statement) -> Result<()> {
        match st {
            Statement::Select(sel) => self.select(sel),
            Statement::Put {
                collection, docs, ..
            } => {
                let s = self.collection(collection)?;
                for (k, e) in docs.iter().flatten() {
                    self.field(s, k);
                    self.expr(s, e)?;
                }
                // The duplicates an insert is refused: each unique index
                // of the collection, whose refusal the statement's guards
                // may be written around (`if absent`, `require`).
                for f in s.fields.iter().chain(&s.paths) {
                    if f.index.is_unique() {
                        self.add(cat(collection, &f.name) + "@unique");
                    }
                }
                for c in s.composites.iter().filter(|c| c.unique) {
                    self.add(composite(collection, c.keys.iter().map(|k| &*k.0)) + "@unique");
                }
                self.pick(s, st.pick())
            }
            Statement::Update {
                collection,
                set,
                filter,
                ..
            } => {
                let s = self.collection(collection)?;
                for (k, e) in set {
                    self.field(s, k);
                    self.expr(s, e)?;
                }
                if let Some(f) = filter {
                    self.expr(s, f)?;
                }
                self.pick(s, st.pick())
            }
            Statement::Delete {
                collection, filter, ..
            } => {
                let s = self.collection(collection)?;
                if let Some(f) = filter {
                    self.expr(s, f)?;
                }
                self.pick(s, st.pick())
            }
            _ => Ok(()),
        }
    }

    fn pick(&mut self, s: &crate::schema::Schema, p: Option<&Pick>) -> Result<()> {
        if let Some(p) = p {
            self.sorts(s, &p.order)?;
            self.names(
                s,
                p.returning
                    .as_ref()
                    .and_then(|r| r.as_deref())
                    .unwrap_or(&[]),
            );
            self.names(s, p.conflict.as_deref().unwrap_or(&[]));
        }
        Ok(())
    }

    fn select(&mut self, sel: &Select) -> Result<()> {
        let s = self.collection(&sel.collection)?;
        self.names(s, sel.project.as_deref().unwrap_or(&[]));
        let mut top: Vec<&Expr> = Vec::new();
        top.extend(&sel.filter);
        top.extend(&sel.having);
        top.extend(&sel.group);
        top.extend(sel.aggregate.iter().chain(&sel.computed).map(|c| &c.expr));
        top.extend(sel.near.as_ref().map(|n| &n.vector));
        top.extend(sel.matcher.as_ref().map(|m| &m.query));
        top.extend(sel.rerank.as_ref().map(|r| &r.vector));
        top.extend(sel.marks.iter().flat_map(|m| &m.args));
        for e in top {
            self.expr(s, e)?;
        }
        self.sorts(s, &sel.order)?;
        let fields = sel.near.iter().map(|n| &n.field);
        let fields = fields.chain(sel.matcher.iter().map(|m| &m.field));
        let fields = fields.chain(sel.rerank.iter().map(|r| &r.field));
        let fields = fields.chain(sel.marks.iter().map(|m| &m.field));
        let fields: Vec<String> = fields
            .chain(sel.facets.iter().map(|f| &f.field))
            .cloned()
            .collect();
        self.names(s, &fields);
        let mut parent = s;
        for l in sel.lookup.iter().flat_map(Lookup::chain) {
            let child = self.collection(&l.collection)?;
            self.names(parent, &l.parent);
            self.names(child, &l.on);
            self.probe(child, l)?;
            self.names(child, l.project.as_deref().unwrap_or(&[]));
            for e in l.filter.iter().chain(l.list_exprs()) {
                self.expr(child, e)?;
            }
            self.sorts(child, &l.order)?;
            parent = child;
        }
        Ok(())
    }

    /// The index a `lookup` level probes its child by: a hash index on its
    /// one key, or an index over several fields leading with its keys --
    /// refused where there is none, as the engine refuses the `lookup`.
    fn probe(&mut self, child: &crate::schema::Schema, l: &Lookup) -> Result<()> {
        let name = &l.collection;
        if let [key] = l.on.as_slice() {
            if key == "id" {
                return Ok(());
            }
            if child
                .field(key)
                .is_some_and(|f| matches!(f.index, crate::schema::IndexKind::Hash { .. }))
            {
                self.add(cat(name, key) + "@hash");
                return Ok(());
            }
        }
        let leads = |c: &&crate::schema::Composite| {
            c.keys.len() >= l.on.len() && c.keys.iter().zip(&l.on).all(|(k, n)| k.0 == *n)
        };
        if child.composites.iter().any(|c| leads(&c)) {
            self.add(composite(name, l.on.iter().map(|k| &**k)) + "@");
            return Ok(());
        }
        Err(Error::Query(format!(
            "`lookup` on `{name}` ({}) needs an index: a hash index on one key (@hash), or one \
             over several fields whose first fields are those (@sorted)",
            l.on.join(", ")
        )))
    }
}

/// `c(a,b)`.
fn composite<'k>(c: &str, keys: impl Iterator<Item = &'k str>) -> String {
    let mut s = String::from(c);
    s.push('(');
    for (i, k) in keys.enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(k);
    }
    s.push(')');
    s
}

/// Why `check` is refused, if it is: it reads the parameters, the claims
/// and `now()` alone, before a call reads any row.
pub fn check_refused(e: &Expr) -> Option<String> {
    let mut why = None;
    let mut walk = |e: &Expr| {
        if why.is_some() {
            return;
        }
        why = match e {
            Expr::Field(f) => Some(format!(
                "`check` reads the parameters, the claims and `now()`, and no field: `{f}`"
            )),
            Expr::InSelect(..) => Some("`check` reads no collection: no `in (get ...)`".into()),
            e if e.aggregate_name().is_some() => Some("`check` folds no rows".into()),
            e if e.asks_expired() => Some("`check` reads no row: no `expired()`".into()),
            _ => None,
        };
    };
    each_expr(e, &mut walk);
    why
}

/// A bound statement's condition as its text writes it -- what follows the
/// `where` after its collection, up to its `comment`, `scans` or `as` --
/// for `describe statement` to show beside the text: the tree holds the
/// condition as read, and no build writes an expression back as text. Read
/// from the stored text, so a binding this process holds no tree for shows
/// it too; strings, comments and brackets are stepped over, and a word with
/// a dot in it (`new.as`) is a field. `None` for a binding with no
/// condition, or a text that is not one.
#[cfg(feature = "statements")]
pub fn condition_text(text: &str) -> Option<&str> {
    // 0 before `after`, 1 between it and `where`, 2 in the condition.
    let (mut state, mut start) = (0u8, 0);
    for (w, from) in TopWords::of(text) {
        match state {
            0 if w.eq_ignore_ascii_case("after") => state = 1,
            1 if w.eq_ignore_ascii_case("where") => (state, start) = (2, from + w.len()),
            1 if clause_ends(w) => return None,
            2 if clause_ends(w) => return text.get(start..from).map(str::trim),
            _ => {}
        }
    }
    None
}

/// A scheduled statement's schedule as its text writes it -- `every
/// <interval>` or `at '<cron>'`, and `in '<zone>'` after it, up to its
/// `comment`, `scans` or `as` -- for the listings' `schedule` and the
/// audit's `schema` line: the record holds the schedule by its meaning,
/// and written back from it the schedule was 3 KB of the server
/// (`Schedule::written`, cut at slice 3's review). Read from the text as
/// [`condition_text`] reads a condition, so a statement this process holds
/// no tree for shows it too, and ending at its last word or string: a
/// comment after it is the text's, not the schedule's. `None` for any
/// other text.
#[cfg(feature = "statements")]
pub fn schedule_text(text: &str) -> Option<&str> {
    // 0 before `statement`, 1 at its name, 2 after it, 3 in the schedule.
    let (mut state, mut start) = (0u8, 0);
    let mut words = TopWords::of(text);
    while let Some((w, from)) = words.next() {
        match state {
            0 if w.eq_ignore_ascii_case("statement") => state = 1,
            1 => state = 2,
            2 if w.eq_ignore_ascii_case("every") || w.eq_ignore_ascii_case("at") => {
                (state, start) = (3, from)
            }
            2 => return None,
            3 if clause_ends(w) => return text.get(start..words.before),
            _ => {}
        }
    }
    // A text ending in its schedule, and not in a string left open.
    match state {
        3 if !words.open => text.get(start..words.last),
        _ => None,
    }
}

/// A word ending a definition's clause: the next clause's, or its body's.
#[cfg(feature = "statements")]
fn clause_ends(w: &str) -> bool {
    ["as", "comment", "scans"]
        .iter()
        .any(|e| w.eq_ignore_ascii_case(e))
}

/// The words of a definition's text outside its strings, comments and
/// brackets, each with where it starts: what [`condition_text`] and
/// [`schedule_text`] find a clause by. A word with a dot in it (`new.as`)
/// is one word; a string left open ends them.
#[cfg(feature = "statements")]
struct TopWords<'t> {
    text: &'t str,
    i: usize,
    depth: i32,
    /// They ended in a string left open.
    open: bool,
    /// Where the last word, string or sign outside a comment ended.
    last: usize,
    /// Where the last one before the word given last ended: a clause's end.
    before: usize,
}

#[cfg(feature = "statements")]
impl<'t> TopWords<'t> {
    fn of(text: &'t str) -> Self {
        TopWords {
            text,
            i: 0,
            depth: 0,
            open: false,
            last: 0,
            before: 0,
        }
    }
}

#[cfg(feature = "statements")]
impl<'t> Iterator for TopWords<'t> {
    type Item = (&'t str, usize);

    fn next(&mut self) -> Option<(&'t str, usize)> {
        let b = self.text.as_bytes();
        let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c >= 0x80;
        let line_end = |i: usize| {
            b[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(b.len(), |n| i + n)
        };
        while self.i < b.len() {
            let i = self.i;
            match b[i] {
                b'#' => self.i = line_end(i),
                b'-' if b.get(i + 1) == Some(&b'-') => self.i = line_end(i),
                q @ (b'\'' | b'"') => {
                    self.i += 1;
                    loop {
                        match b.get(self.i) {
                            None => {
                                self.open = true;
                                return None;
                            }
                            Some(b'\\') => self.i += 2,
                            Some(&c) if c == q && b.get(self.i + 1) == Some(&q) => self.i += 2,
                            Some(&c) if c == q => break self.i += 1,
                            Some(_) => self.i += 1,
                        }
                    }
                    self.last = self.i;
                }
                b'(' | b'[' | b'{' => {
                    (self.depth, self.i, self.last) = (self.depth + 1, i + 1, i + 1)
                }
                b')' | b']' | b'}' => {
                    (self.depth, self.i, self.last) = (self.depth - 1, i + 1, i + 1)
                }
                c if word(c) => {
                    while self.i < b.len() && word(b[self.i]) {
                        self.i += 1;
                    }
                    (self.before, self.last) = (self.last, self.i);
                    if self.depth == 0 {
                        return Some((self.text.get(i..self.i)?, i));
                    }
                }
                c => {
                    self.i += 1;
                    if !c.is_ascii_whitespace() {
                        self.last = self.i;
                    }
                }
            }
        }
        None
    }
}

/// Why a bound statement's condition is refused, if it is: it reads the
/// row the write wrote, as written and as it stood, and `now()` -- a read
/// of another collection is its body's guard, whose refusal can say why.
/// Its fields are the parser's to hold to `new.` and `old.`.
#[cold]
pub fn condition_refused(e: &Expr) -> Option<String> {
    let mut why = None;
    let mut walk = |e: &Expr| {
        if why.is_some() {
            return;
        }
        why = match e {
            Expr::InSelect(..) => Some(
                "a bound statement's condition reads the row alone: a read of another \
                 collection is its body's guard, `get ... require 1 else '<why>'`",
            ),
            e if e.aggregate_name().is_some() => {
                Some("a bound statement's condition reads one row, and folds none")
            }
            e if e.asks_expired() => Some(
                "a bound statement's condition reads no `expired()`: no binding sees a row \
                 expire",
            ),
            _ => None,
        };
    };
    each_expr(e, &mut walk);
    why.map(String::from)
}

/// Why a body refuses `s`, if it does: the statement it holds is a `get`,
/// an `insert` with its documents in the text, or a `set` or a `del` that
/// says which rows it writes.
fn body_refused(s: &Statement) -> Option<&'static str> {
    Some(match s {
        Statement::Select(_) => return None,
        Statement::Put {
            insert,
            docs_param,
            else_set,
            ..
        } => {
            if docs_param.is_some() {
                "`insert <collection> $n` writes the caller's documents -- fields, ids and \
                 values -- past every guard the text holds: a body writes its documents in \
                 the text, `insert entries {from: $from, by: $jwt.sub}`"
            } else if else_set.is_some() {
                "`if absent else set` finds its row by a key the caller's document names: a \
                 body writes `insert ... if absent`, or `set ... where` the row it means"
            } else if !insert {
                "a `put` naming an id writes over whatever row holds it, and an argument could \
                 name anyone's: a body writes `insert`, which never writes over a row, or `set \
                 ... where` the row it means"
            } else {
                return None;
            }
        }
        Statement::Update { filter, pick, .. } | Statement::Delete { filter, pick, .. } => {
            if filter.is_none() && !pick.as_ref().is_some_and(|p| p.picks()) {
                "a body's `set` and `del` say which rows they write: a `where`, or `order` and \
                 `limit` -- `where true` where every row is meant"
            } else {
                return None;
            }
        }
        Statement::CreateCollection { .. }
        | Statement::DropCollection { .. }
        | Statement::CreateIndex { .. }
        | Statement::CreateComposite { .. }
        | Statement::DropComposite { .. }
        | Statement::AlterCollection { .. }
        | Statement::CreateStatement(_)
        | Statement::DropStatement { .. } => {
            "a body changes no schema: that is the server token's, and a statement holding it \
             would hand it to every caller"
        }
        Statement::Compact(_) => "a body compacts nothing: `compact` is the server token's",
        Statement::Call(_) => {
            "a body calls no statement: it is read whole where it is defined, with no \
             recursion to bound"
        }
        Statement::Explain(_)
        | Statement::ListCollections
        | Statement::Describe(_)
        | Statement::ListStatements
        | Statement::DescribeStatement(_) => {
            "a body holds `get`, `insert`, `set ... where` and `del ... where`"
        }
    })
}

/// Why the last statement's answer is refused, if it is: it names every
/// field it answers, since a statement runs with an authority of its own,
/// and `*` would start answering a field added after it was defined.
fn unnamed_answer(s: &Statement) -> Option<&'static str> {
    const STAR: &str = "the last statement is what a call answers, and names every field it \
                        answers: `select a, b`, `returning a, b` -- `*`, or a `get` with no \
                        list, would answer a field added after it was defined";
    match s {
        Statement::Select(sel) => {
            let named = |project: &Option<Vec<String>>, folds: bool| project.is_some() || folds;
            let mut ok = named(&sel.project, !sel.aggregate.is_empty()) || sel.count;
            if let Some(f) = &sel.flat {
                ok &= !f.columns.iter().any(|c| c.column.is_empty());
            }
            let mut level = sel.lookup.as_ref();
            while let (true, Some(l)) = (ok, level) {
                ok = named(&l.project, !l.aggregate.is_empty());
                level = l.next.as_deref();
            }
            (!ok).then_some(STAR)
        }
        _ => match s.pick().and_then(|p| p.returning.as_ref()) {
            Some(None) => Some(STAR),
            _ => None,
        },
    }
}

/// Calls `f` on `e` and on every expression under it, an inner `get`'s
/// among them.
fn each_expr<'a>(e: &'a Expr, f: &mut dyn FnMut(&'a Expr)) {
    f(e);
    if let Expr::InSelect(_, sel) = e {
        select_exprs(sel, f);
    }
    e.each_child(&mut |c| each_expr(c, f));
}

/// Calls `f` on every expression of a `get` -- each clause's, each
/// `lookup` level's, each inner `get`'s -- and on what is under each.
fn select_exprs<'a>(s: &'a Select, f: &mut dyn FnMut(&'a Expr)) {
    let mut top: Vec<&'a Expr> = Vec::new();
    top.extend(&s.filter);
    top.extend(&s.having);
    top.extend(&s.group);
    top.extend(s.aggregate.iter().chain(&s.computed).map(|c| &c.expr));
    top.extend(s.order.iter().filter_map(|o| o.expr.as_ref()));
    top.extend(s.near.as_ref().map(|n| &n.vector));
    top.extend(s.matcher.as_ref().map(|m| &m.query));
    top.extend(s.rerank.as_ref().map(|r| &r.vector));
    top.extend(s.marks.iter().flat_map(|m| &m.args));
    for l in s.lookup.iter().flat_map(Lookup::chain) {
        top.extend(&l.filter);
        top.extend(l.list_exprs());
        top.extend(l.order.iter().filter_map(|o| o.expr.as_ref()));
    }
    for e in top {
        each_expr(e, f);
    }
}

/// Calls `f` on every collection `s` names: its own, a `lookup` level's,
/// an inner `get`'s.
fn each_collection<'a>(s: &'a Statement, f: &mut dyn FnMut(&str)) {
    let mut gets: Vec<&'a Select> = Vec::new();
    let mut exprs: Vec<&'a Expr> = Vec::new();
    match s {
        Statement::Select(sel) | Statement::Explain(sel) => gets.push(sel),
        Statement::Put {
            collection,
            docs,
            else_set,
            pick,
            ..
        } => {
            f(collection);
            exprs.extend(docs.iter().flatten().map(|(_, e)| e));
            exprs.extend(else_set.iter().flatten().map(|(_, e)| e));
            exprs.extend(
                pick.iter()
                    .flat_map(|p| &p.order)
                    .filter_map(|o| o.expr.as_ref()),
            );
        }
        Statement::Update {
            collection,
            set,
            filter,
            pick,
            ..
        } => {
            f(collection);
            exprs.extend(set.iter().map(|(_, e)| e));
            exprs.extend(filter);
            exprs.extend(
                pick.iter()
                    .flat_map(|p| &p.order)
                    .filter_map(|o| o.expr.as_ref()),
            );
        }
        Statement::Delete {
            collection,
            filter,
            pick,
            ..
        } => {
            f(collection);
            exprs.extend(filter);
            exprs.extend(
                pick.iter()
                    .flat_map(|p| &p.order)
                    .filter_map(|o| o.expr.as_ref()),
            );
        }
        _ => {}
    }
    // Every inner `get`, however deep: `each_expr` walks into each.
    let mut inner: Vec<&'a Select> = Vec::new();
    let mut found = |e: &'a Expr| {
        if let Expr::InSelect(_, sel) = e {
            inner.push(sel);
        }
    };
    for e in exprs {
        each_expr(e, &mut found);
    }
    for sel in &gets {
        select_exprs(sel, &mut found);
    }
    for sel in gets.iter().chain(&inner) {
        f(&sel.collection);
        for l in sel.lookup.iter().flat_map(Lookup::chain) {
            f(&l.collection);
        }
    }
}

/// An operator's byte in the canonical tree: fixed here, never the
/// variant's place in `CmpOp`, so reordering the enum or adding to it moves
/// no stored digest.
fn cmp_byte(op: CmpOp) -> u64 {
    match op {
        CmpOp::Eq => 0,
        CmpOp::Ne => 1,
        CmpOp::Lt => 2,
        CmpOp::Le => 3,
        CmpOp::Gt => 4,
        CmpOp::Ge => 5,
    }
}

/// As [`cmp_byte`], for `ArithOp`.
fn arith_byte(op: ArithOp) -> u64 {
    match op {
        ArithOp::Add => 0,
        ArithOp::Sub => 1,
        ArithOp::Mul => 2,
        ArithOp::Div => 3,
    }
}

/// The canonical tree's writer ([`Definition::canonical`]).
struct Canon(Vec<u8>);

impl Canon {
    /// A schedule, a node of kind 6: `every`'s interval as `bucket` holds
    /// it -- a step of milliseconds (1) from an origin (2, a week's Monday),
    /// or months (3) -- or cron's five masks (4 to 8), then the zone (9),
    /// none for UTC. Its meaning, not its spelling: `every 60m` is `every
    /// 1h`, `'*/15 * * * *'` is `'0,15,30,45 * * * *'`, and `in 'utc'` no
    /// zone, as the parser reads it (`crate::tz::spelled`). Out of line: no
    /// other definition reaches it.
    #[cold]
    #[inline(never)]
    fn schedule(&mut self, s: &Schedule) {
        use crate::query::Interval;
        self.node(6);
        match s.when {
            When::Every(Interval::Ms(step, origin)) => {
                self.num(1, step as u64);
                self.num(2, origin as u64);
            }
            When::Every(Interval::Months(n)) => self.num(3, n as u64),
            When::At(c) => {
                self.num(4, c.minutes);
                self.num(5, c.hours.into());
                self.num(6, c.days.into());
                self.num(7, c.months.into());
                self.num(8, c.weekdays.into());
            }
        }
        self.opt_text(9, s.zone.as_deref());
        self.end();
    }

    /// A hook, a node of kind 7: its name (1), its condition (2), and what
    /// each parameter reads (3), in the signature's order, each a node of
    /// kind 8 -- the body's path, its keys under 2 (none for the whole
    /// body), or `hook.id`, `hook.at` and `hook.event`, 1 to 3 under 1. The
    /// condition's paths are written as read, `event.` in lower case and
    /// each key as written. Out of line: no other definition reaches it.
    #[cold]
    #[inline(never)]
    fn webhook(&mut self, h: &Webhook) {
        self.node(7);
        self.text(1, &h.name);
        self.opt_expr(2, &h.condition);
        self.list(3, &h.sources, |c, s| {
            c.node(8);
            match s {
                // Each key under 2, as a list of them was written: a text
                // each, its length and its bytes, as `Canon::str` writes it.
                Source::Event(keys) if !keys.is_empty() => {
                    let keys = keys.as_bytes().split(|&b| b == b'.');
                    c.tag(2);
                    c.uint(keys.clone().count() as u64);
                    for k in keys {
                        c.uint(k.len() as u64);
                        c.0.extend_from_slice(k);
                    }
                }
                Source::Event(_) => {}
                Source::HookId => c.num(1, 1),
                Source::HookAt => c.num(1, 2),
                Source::HookEvent => c.num(1, 3),
            }
            c.end();
        });
        self.end();
    }

    fn tag(&mut self, t: u8) {
        self.0.push(t);
    }

    /// A node's kind.
    fn node(&mut self, kind: u8) {
        self.0.push(kind);
    }

    /// A node's end.
    fn end(&mut self) {
        self.0.push(0);
    }

    fn uint(&mut self, n: u64) {
        codec::put_uvarint(&mut self.0, n);
    }

    fn str(&mut self, s: &str) {
        codec::encode_str(&mut self.0, s);
    }

    fn value(&mut self, v: &Value) {
        codec::encode_value(&mut self.0, v);
    }

    fn text(&mut self, tag: u8, s: &str) {
        if !s.is_empty() {
            self.tag(tag);
            self.str(s);
        }
    }

    fn opt_text(&mut self, tag: u8, s: Option<&str>) {
        if let Some(s) = s {
            self.tag(tag);
            self.str(s);
        }
    }

    fn flag(&mut self, tag: u8, b: bool) {
        if b {
            self.tag(tag);
        }
    }

    fn num(&mut self, tag: u8, n: u64) {
        if n != 0 {
            self.tag(tag);
            self.uint(n);
        }
    }

    fn opt_num(&mut self, tag: u8, n: Option<u64>) {
        if let Some(n) = n {
            self.tag(tag);
            self.uint(n);
        }
    }

    /// `require <n> [else '<reason>']`: the count under `tag`, and the
    /// reason, a part of the API it answers, under `reason` where one is
    /// given -- a field of its own, so a guard without one writes what it
    /// wrote before reasons were read.
    fn require(&mut self, tag: u8, reason: u8, r: &Option<Require>) {
        if let Some(r) = r {
            self.tag(tag);
            self.uint(r.n);
            self.text(reason, &r.reason);
        }
    }

    fn ty(&mut self, tag: u8, t: &DataType) {
        self.tag(tag);
        codec::encode_type(&mut self.0, t);
    }

    fn names(&mut self, tag: u8, v: &[String]) {
        if !v.is_empty() {
            self.tag(tag);
            self.uint(v.len() as u64);
            for s in v {
                self.str(s);
            }
        }
    }

    /// `None` writes nothing; `Some` the list, empty or not, which differ:
    /// a `project` of none is every field, of an empty list none.
    fn opt_names(&mut self, tag: u8, v: &Option<Vec<String>>) {
        if let Some(v) = v {
            self.tag(tag);
            self.uint(v.len() as u64);
            for s in v {
                self.str(s);
            }
        }
    }

    fn list<T>(&mut self, tag: u8, v: &[T], mut each: impl FnMut(&mut Canon, &T)) {
        if !v.is_empty() {
            self.tag(tag);
            self.uint(v.len() as u64);
            for x in v {
                each(self, x);
            }
        }
    }

    fn count(&mut self, c: Count) {
        match c {
            Count::N(n) => {
                self.uint(0);
                self.uint(n as u64);
            }
            Count::Param(i) => {
                self.uint(1);
                self.uint(i as u64);
            }
        }
    }

    fn opt_count(&mut self, tag: u8, c: Option<Count>) {
        if let Some(c) = c {
            self.tag(tag);
            self.count(c);
        }
    }

    /// An `offset`: `0` written is none.
    fn offset(&mut self, tag: u8, c: Count) {
        if !c.is_zero() {
            self.tag(tag);
            self.count(c);
        }
    }

    fn opt_expr(&mut self, tag: u8, e: &Option<Expr>) {
        if let Some(e) = e {
            self.tag(tag);
            self.expr(e);
        }
    }

    fn exprs(&mut self, tag: u8, v: &[Expr]) {
        self.list(tag, v, |c, e| c.expr(e));
    }

    fn pairs(&mut self, tag: u8, v: &[(String, Expr)]) {
        self.list(tag, v, |c, (k, e)| {
            c.str(k);
            c.expr(e);
        });
    }

    fn columns(&mut self, tag: u8, v: &[Column]) {
        self.list(tag, v, |c, Column { expr, name }| {
            c.str(name);
            c.expr(expr);
        });
    }

    fn sorts(&mut self, tag: u8, v: &[Sort]) {
        self.list(tag, v, |c, s| {
            let Sort {
                field,
                asc,
                collate,
                expr,
            } = s;
            c.node(1);
            c.text(1, field);
            c.flag(2, !asc);
            c.opt_text(3, collate.map(|k| k.name()));
            c.opt_expr(4, expr);
            c.end();
        });
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Field(f) => {
                self.node(1);
                self.str(f);
            }
            Expr::Lit(v) => {
                self.node(2);
                self.value(v);
            }
            Expr::Param(i) => {
                self.node(3);
                self.uint(*i as u64);
            }
            Expr::And(a, b) => self.two(4, a, b),
            Expr::Or(a, b) => self.two(5, a, b),
            Expr::Not(a) => {
                self.node(6);
                self.expr(a);
            }
            Expr::Cmp(op, a, b) => {
                self.node(7);
                self.uint(cmp_byte(*op));
                self.expr(a);
                self.expr(b);
            }
            Expr::Like(a, b) => self.two(8, a, b),
            Expr::Has(a, b) => self.two(9, a, b),
            Expr::In(a, items) => {
                self.node(10);
                self.expr(a);
                self.uint(items.len() as u64);
                for i in items {
                    self.expr(i);
                }
            }
            Expr::InSelect(a, sel) => {
                self.node(11);
                self.expr(a);
                self.select(sel);
            }
            Expr::IsNull(a) => {
                self.node(12);
                self.expr(a);
            }
            Expr::Call(name, args) => {
                self.node(13);
                self.str(name);
                self.uint(args.len() as u64);
                for a in args {
                    self.expr(a);
                }
            }
            Expr::Arith(op, a, b) => {
                self.node(14);
                self.uint(arith_byte(*op));
                self.expr(a);
                self.expr(b);
            }
            // A node of its own: no text read before it was one.
            Expr::InParam(a, i) => {
                self.node(15);
                self.expr(a);
                self.uint(*i as u64);
            }
        }
    }

    fn two(&mut self, kind: u8, a: &Expr, b: &Expr) {
        self.node(kind);
        self.expr(a);
        self.expr(b);
    }

    fn select(&mut self, s: &Select) {
        let Select {
            collection,
            project,
            filter,
            near,
            matcher,
            rerank,
            order,
            limit,
            offset,
            count,
            lookup,
            aggregate,
            group,
            having,
            computed,
            fuse,
            marks,
            facets,
            require,
            flat,
        } = s;
        self.node(1);
        self.text(1, collection);
        self.opt_names(2, project);
        self.opt_expr(3, filter);
        if let Some(Near {
            field,
            vector,
            ef,
            exact,
            metric,
        }) = near
        {
            self.tag(4);
            self.node(1);
            self.text(1, field);
            self.tag(2);
            self.expr(vector);
            self.opt_num(3, ef.map(|n| n as u64));
            self.flag(4, *exact);
            self.opt_text(5, metric.map(|m| m.name()));
            self.end();
        }
        if let Some(Match {
            field,
            query,
            within,
            fuzzy,
            prefix,
        }) = matcher
        {
            self.tag(5);
            self.node(1);
            self.text(1, field);
            self.tag(2);
            self.expr(query);
            self.flag(3, *within);
            self.num(4, *fuzzy as u64);
            self.flag(5, *prefix);
            self.end();
        }
        if let Some(Rerank {
            field,
            vector,
            candidates,
        }) = rerank
        {
            self.tag(6);
            self.node(1);
            self.text(1, field);
            self.tag(2);
            self.expr(vector);
            self.opt_num(3, candidates.map(|n| n as u64));
            self.end();
        }
        self.sorts(7, order);
        self.opt_count(8, *limit);
        self.offset(9, *offset);
        self.flag(10, *count);
        if let Some(l) = lookup {
            self.tag(11);
            self.lookup(l);
        }
        self.columns(12, aggregate);
        self.exprs(13, group);
        self.opt_expr(14, having);
        self.columns(15, computed);
        if let Some(Fuse { k, candidates }) = fuse {
            self.tag(16);
            self.node(1);
            self.opt_num(1, k.map(|n| n as u64));
            self.opt_num(2, candidates.map(|n| n as u64));
            self.end();
        }
        self.list(17, marks, |c, m| {
            let Mark {
                field,
                snippet,
                args,
            } = m;
            c.node(1);
            c.text(1, field);
            c.opt_num(2, snippet.map(|n| n as u64));
            c.exprs(3, args);
            c.end();
        });
        self.list(18, facets, |c, f| {
            // `rest` is the filter split for a disjunctive facet, worked
            // out of the filter and the facets by the parser.
            let Facet {
                field,
                top,
                ranges,
                disjunctive,
                rest: _,
            } = f;
            c.node(1);
            c.text(1, field);
            c.opt_count(2, *top);
            if let Some(r) = ranges {
                c.tag(3);
                c.uint(r.len() as u64);
                for v in r {
                    c.value(v);
                }
            }
            c.flag(4, *disjunctive);
            c.end();
        });
        self.require(19, 21, require);
        if let Some(f) = flat {
            self.tag(20);
            self.flat(f);
        }
        self.end();
    }

    fn flat(&mut self, f: &Flat) {
        let Flat {
            columns,
            offset,
            limit,
            shown,
            names,
            probe,
        } = f;
        self.node(1);
        self.list(1, columns, |c, col| {
            c.uint(col.level as u64);
            c.str(&col.column);
            c.str(&col.name);
        });
        self.offset(2, *offset);
        self.opt_count(3, *limit);
        self.list(4, shown, |c, (level, fields)| {
            c.uint(*level as u64);
            c.uint(fields.len() as u64);
            for s in fields {
                c.str(s);
            }
        });
        if let Some(n) = names {
            self.tag(5);
            self.uint(n.tables.len() as u64);
            for (alias, collection) in &n.tables {
                self.str(alias);
                self.str(collection);
            }
            self.uint(n.names.len() as u64);
            for (table, name, bare) in &n.names {
                self.uint(*table as u64);
                self.str(name);
                self.uint(u64::from(*bare));
            }
        }
        self.flag(6, *probe);
        self.end();
    }

    fn lookup(&mut self, l: &Lookup) {
        let Lookup {
            collection,
            on,
            parent,
            project,
            aggregate,
            group,
            having,
            filter,
            order,
            limit,
            offset,
            required,
            next,
        } = l;
        self.node(1);
        self.text(1, collection);
        self.names(2, on);
        self.names(3, parent);
        self.opt_names(4, project);
        self.columns(5, aggregate);
        self.exprs(6, group);
        self.opt_expr(7, having);
        self.opt_expr(8, filter);
        self.sorts(9, order);
        self.opt_count(10, *limit);
        self.offset(11, *offset);
        self.flag(12, *required);
        if let Some(n) = next {
            self.tag(13);
            self.lookup(n);
        }
        self.end();
    }

    fn pick(&mut self, tag: u8, p: &Option<Box<Pick>>) {
        let Some(p) = p else { return };
        let Pick {
            order,
            limit,
            returning,
            conflict,
        } = &**p;
        self.tag(tag);
        self.node(1);
        self.sorts(1, order);
        self.opt_count(2, *limit);
        if let Some(r) = returning {
            self.tag(3);
            match r {
                None => self.uint(0),
                Some(names) => {
                    self.uint(1);
                    self.uint(names.len() as u64);
                    for s in names {
                        self.str(s);
                    }
                }
            }
        }
        self.opt_names(4, conflict);
        self.end();
    }

    /// A body's statement: a `get`, an `insert`, a `set` or a `del`.
    fn statement(&mut self, s: &Statement) -> Result<()> {
        match s {
            Statement::Select(sel) => {
                self.node(1);
                self.select(sel);
            }
            Statement::Put {
                collection,
                docs,
                docs_param,
                insert,
                if_absent,
                else_set,
                require,
                pick,
            } => {
                self.node(2);
                self.text(1, collection);
                self.list(2, docs, |c, d| {
                    c.uint(d.len() as u64);
                    for (k, e) in d {
                        c.str(k);
                        c.expr(e);
                    }
                });
                self.opt_num(3, docs_param.map(|i| i as u64));
                self.flag(4, *insert);
                self.flag(5, *if_absent);
                if let Some(set) = else_set {
                    self.tag(6);
                    self.uint(set.len() as u64);
                    for (k, e) in set {
                        self.str(k);
                        self.expr(e);
                    }
                }
                self.require(7, 9, require);
                self.pick(8, pick);
                self.end();
            }
            Statement::Update {
                collection,
                set,
                filter,
                require,
                pick,
            } => {
                self.node(3);
                self.text(1, collection);
                self.pairs(2, set);
                self.opt_expr(3, filter);
                self.require(4, 6, require);
                self.pick(5, pick);
                self.end();
            }
            Statement::Delete {
                collection,
                filter,
                require,
                pick,
            } => {
                self.node(4);
                self.text(1, collection);
                self.opt_expr(2, filter);
                self.require(3, 5, require);
                self.pick(4, pick);
                self.end();
            }
            _ => {
                return Err(Error::Query(
                    "a body holds `get`, `insert`, `set ... where` and `del ... where`".into(),
                ))
            }
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "statements"))]
mod tests {
    use super::{condition_text, schedule_text};

    /// A binding's condition as written, strings, comments and brackets
    /// stepped over, a dotted word a field: what `describe statement`
    /// shows.
    #[test]
    fn a_condition_is_read_from_its_text_as_written() {
        let read = |t| condition_text(t);
        assert_eq!(
            read(
                "create statement p after set orders where new.status = 'paid' as insert l {o: 1}"
            ),
            Some("new.status = 'paid'")
        );
        assert_eq!(
            read(
                "CREATE OR REPLACE STATEMENT p AFTER update OR delete orders\n  WHERE \
                 new.note = 'as comment' -- as scans\n  and (old.n in (1, 2) or new.as = 1)\n  \
                 COMMENT 'it' scans AS begin get x require 1; insert l {o: 1} end"
            ),
            Some("new.note = 'as comment' -- as scans\n  and (old.n in (1, 2) or new.as = 1)")
        );
        assert_eq!(
            read("create statement p after insert t as insert l {o: new.id}"),
            None
        );
        assert_eq!(
            read("create statement p after insert t comment 'where' as insert l {o: 1}"),
            None
        );
        assert_eq!(
            read("create statement p($a int) as get t where a = $a"),
            None
        );
        assert_eq!(read("create statement p after set t where 'open"), None);
    }

    /// The schedule as written, its zone with it, and nothing of the body
    /// or the clauses after it: the listings' `schedule` and the audit's.
    #[test]
    fn a_schedule_is_read_from_its_text_as_written() {
        for (text, clause) in [
            (
                "create statement a every 15m as del t where n = 1",
                Some("every 15m"),
            ),
            (
                "CREATE OR REPLACE STATEMENT a EVERY 1 h IN 'Europe/Berlin' comment 'x' as del t",
                Some("EVERY 1 h IN 'Europe/Berlin'"),
            ),
            (
                "create statement a every 1d scans as del t",
                Some("every 1d"),
            ),
            (
                "create statement a at '0 3 * * 1-5'\n  in '+03:00'\nas del t where s = 'in'",
                Some("at '0 3 * * 1-5'\n  in '+03:00'"),
            ),
            (
                "create statement at at '0 3 * * *' -- at 3\nas del t",
                Some("at '0 3 * * *'"),
            ),
            (
                "create statement a every 1h\n-- a comment\n# and another\nas del t",
                Some("every 1h"),
            ),
            (
                "create statement a every 1h -- to the end",
                Some("every 1h"),
            ),
            ("create statement a at 'it''s' as del t", Some("at 'it''s'")),
            ("create statement a every 1h", Some("every 1h")),
            ("create statement a() as get t count", None),
            ("create statement a after del t as del u where id = 1", None),
            ("create statement a at 'open", None),
            ("drop statement a", None),
        ] {
            assert_eq!(schedule_text(text), clause, "{text}");
        }
    }
}

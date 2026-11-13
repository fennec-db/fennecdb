//! The engine held to a model of itself. A seeded generator -- xorshift64*,
//! as `num.rs`'s tests draw, no dependency -- makes random runs of what a
//! database meets: `put`, `insert`, `if absent` and upserts, `set` and
//! `del` picking their rows with `order` and `limit`, each with `require`
//! and `returning`; blocks that land, blocks that fail part way and blocks
//! rolled back; `alter`; `create index` and `drop index`; `compact` and
//! checkpoints; `@ttl` rows reaching their time as `Database::set_clock`
//! moves, and the sweep; the file opened again, and opened again after a
//! crash -- cut at a record boundary, a record torn, zeros after it or over
//! its last records. Every step is held to a model kept in `BTreeMap`s:
//! what the statement answered, what every row holds, the record it
//! appended and the change counter, and which rows a read finds -- every
//! index, `@hash`, `@sorted`, `@unique`, an index over several fields,
//! `@ttl`, `@text`, the graph, `@inverted` and `@geo`, held to the plain
//! scan of a twin field holding the same values, as `tests/nan.rs` holds
//! them, and to the model.
//!
//! `make test` runs 200 seeds of 1 000 steps, in the full build and in the
//! one without the indexes (`tests/features.rs`), and
//! `.github/workflows/model.yml` 1 000 seeds of 100 000 in both each week,
//! seeds no run took before. A divergence is a bug, the engine's -- fixed
//! with a test of its own, beside the code's others -- or the model's: never
//! a step the generator stops drawing. A write form or an index kind the
//! engine gains is drawn here too. The environment asks for more, or for
//! one seed:
//!
//! ```text
//! FENNEC_MODEL_SEEDS=1000 FENNEC_MODEL_OPS=100000 cargo test -p fennec-core --release --test all model::
//! FENNEC_MODEL_SEED=17 FENNEC_MODEL_OPS=400 cargo test -p fennec-core --test all model::
//! ```
//!
//! `FENNEC_MODEL_FIRST` moves where the seeds start, `FENNEC_MODEL_THREADS`
//! says how many run at once, and `FENNEC_MODEL_PROGRESS=1` prints a line as
//! each tenth of the seeds is done. A divergence prints its seed and step,
//! and the steps shrunk to the fewest that still diverge the same way
//! (within `FENNEC_MODEL_SHRINK_SECS`, 120 by default), as FennecQL and the
//! operations around it.

use fennec_core::prelude::{Database, Error, Response, Sink, Statement, Value};
use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as Atomic};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ------------------------------------------------------------- the draw

/// xorshift64*, as `num.rs`'s tests draw. The seed goes through splitmix64
/// first, so seeds 0, 1, 2 start far apart and none is the zero state
/// xorshift never leaves.
struct Rng(u64);

impl Rng {
    fn seeded(seed: u64) -> Rng {
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Rng((z ^ (z >> 31)) | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn upto(&mut self, n: usize) -> usize {
        self.below(n as u64) as usize
    }

    fn one_in(&mut self, n: u64) -> bool {
        self.below(n) == 0
    }

    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo + 1) as u64) as i64
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.upto(xs.len())]
    }
}

// ------------------------------------------------------------ the model

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum C {
    A,
    B,
    T,
}

const COLLECTIONS: [C; 3] = [C::A, C::B, C::T];

impl C {
    fn name(self) -> &'static str {
        match self {
            C::A => "a",
            C::B => "b",
            C::T => "t",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ty {
    Int,
    Text,
    Time,
    Vector,
    Sparse,
    Point,
}

impl Ty {
    fn word(self) -> &'static str {
        match self {
            Ty::Int => "int",
            Ty::Text => "text",
            Ty::Time => "timestamp",
            Ty::Vector => "vector<3>",
            Ty::Sparse => "sparse<6>",
            Ty::Point => "geo",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ix {
    None,
    Hash,
    Sorted,
    Unique,
    Text,
    Graph,
    Inverted,
    Geo,
    /// `@ttl`: the collection's `ttl` says for how long.
    Ttl,
}

impl Ix {
    fn word(self) -> &'static str {
        match self {
            Ix::None | Ix::Ttl => "",
            Ix::Hash => " @hash",
            Ix::Sorted => " @sorted",
            Ix::Unique => " @unique",
            Ix::Text => " @text",
            Ix::Graph => " @hnsw(cosine)",
            Ix::Inverted => " @inverted",
            Ix::Geo => " @geo",
        }
    }

    /// Whether this build makes the index: one it is made without is
    /// declared and read all the same, and `create index` of it refused.
    fn built(self) -> bool {
        match self {
            Ix::Sorted => cfg!(feature = "sorted"),
            Ix::Text => cfg!(feature = "text"),
            Ix::Graph => cfg!(feature = "vector"),
            Ix::Inverted => cfg!(feature = "sparse"),
            Ix::Geo => cfg!(feature = "geo") && cfg!(feature = "sorted"),
            _ => true,
        }
    }
}

#[derive(Clone, Debug)]
struct Field {
    name: String,
    ty: Ty,
    ix: Ix,
}

#[derive(Clone, Debug, PartialEq)]
struct Composite {
    keys: Vec<String>,
    unique: bool,
}

/// A row's fields; a field it does not hold is `null`.
type Row = BTreeMap<String, Value>;

#[derive(Clone, Debug)]
struct Coll {
    name: &'static str,
    fields: Vec<Field>,
    composites: Vec<Composite>,
    /// Every row the store holds, those past their time among them.
    rows: BTreeMap<u64, Rc<Row>>,
    next_id: u64,
    /// How long after `at` a row of `t` is past its time, in milliseconds.
    ttl: Option<i64>,
}

/// The twins: fields written the same value each write, one answered by
/// the scan and the others through an index -- what holds every index to
/// the plain scan. The first of each is the scan's.
const N: [&str; 3] = ["n", "nh", "ns"];
const U: [&str; 2] = ["uu", "u"];
const P: [&str; 2] = ["p2", "p"];
const Q: [&str; 2] = ["q2", "q"];
const L: [&str; 2] = ["loc2", "loc"];
const AT: [&str; 2] = ["at2", "at"];
/// The names `alter` adds, renames and drops; the twins stay as made.
const EXTRA: [&str; 4] = ["x0", "x1", "x2", "x3"];
const TTL: i64 = 10_000;
/// Where the clock starts: 2023-11-14, in milliseconds.
const START: i64 = 1_700_000_000_000;

const WORDS: [&str; 5] = ["fox", "dog", "cat", "owl", "bee"];
const LETTERS: [&str; 4] = ["a", "b", "c", "d"];
/// Few, so that rows share them: a vector written again is one node of
/// the graph, the rest aliases.
const VECTORS: [[f32; 3]; 6] = [
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
    [1.0, 1.0, 0.0],
    [0.5, 0.5, 1.0],
    [-1.0, 0.0, 0.0],
];
const LONS: [f64; 5] = [10.0, 10.5, 11.0, 11.5, 12.0];
const LATS: [f64; 5] = [44.0, 44.5, 45.0, 45.5, 46.0];

fn field(name: &str, ty: Ty, ix: Ix) -> Field {
    Field {
        name: name.to_string(),
        ty,
        ix,
    }
}

impl Coll {
    fn new(name: &'static str, fields: Vec<Field>) -> Coll {
        Coll {
            name,
            fields,
            composites: Vec::new(),
            rows: BTreeMap::new(),
            next_id: 1,
            ttl: None,
        }
    }

    fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }

    fn has(&self, name: &str) -> bool {
        self.field(name).is_some()
    }

    fn columns(&self) -> Vec<String> {
        let mut c = vec!["id".to_string()];
        c.extend(self.fields.iter().map(|f| f.name.clone()));
        c
    }

    fn values(&self, id: u64, row: &Row, columns: &[String]) -> Vec<Value> {
        columns
            .iter()
            .map(|c| match c.as_str() {
                "id" => Value::Int(id as i64),
                f => row.get(f).cloned().unwrap_or(Value::Null),
            })
            .collect()
    }

    fn has_unique(&self) -> bool {
        self.fields.iter().any(|f| f.ix == Ix::Unique) || self.composites.iter().any(|c| c.unique)
    }

    /// The rows other than `me` holding one of `row`'s unique values: by
    /// field in the order they are declared, each ascending, then by index
    /// over several fields -- the order the engine finds them in, whose
    /// first an upsert sets.
    fn holders(&self, row: &Row, me: u64) -> Vec<u64> {
        let mut out = Vec::new();
        for f in self.fields.iter().filter(|f| f.ix == Ix::Unique) {
            let Some(v) = row.get(&f.name) else { continue };
            for (id, other) in &self.rows {
                if *id != me && other.get(&f.name).is_some_and(|w| same(v, w)) {
                    out.push(*id);
                }
            }
        }
        for c in self.composites.iter().filter(|c| c.unique) {
            let Some(vs) = c
                .keys
                .iter()
                .map(|k| row.get(k))
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            for (id, other) in &self.rows {
                let all = c
                    .keys
                    .iter()
                    .zip(&vs)
                    .all(|(k, v)| other.get(k).is_some_and(|w| same(v, w)));
                if *id != me && all {
                    out.push(*id);
                }
            }
        }
        out
    }

    /// Whether the row `id` is past its time at `cut`, now less the ttl.
    fn expired(&self, id: u64, cut: i64) -> bool {
        self.rows
            .get(&id)
            .is_some_and(|row| matches!(row.get("at"), Some(Value::Timestamp(t)) if *t <= cut))
    }

    fn alive(&self, row: &Row, cut: Option<i64>) -> bool {
        match (cut, row.get("at")) {
            (Some(cut), Some(Value::Timestamp(t))) => *t > cut,
            _ => true,
        }
    }

    /// The fields a filter, an order or a write of the generator's reads:
    /// those holding numbers, text and times.
    fn plain(&self) -> Vec<String> {
        self.fields
            .iter()
            .filter(|f| matches!(f.ty, Ty::Int | Ty::Text | Ty::Time))
            .map(|f| f.name.clone())
            .collect()
    }
}

/// Two values the engine finds equal: of one kind and the same, a time
/// and an int by their milliseconds.
fn same(a: &Value, b: &Value) -> bool {
    compare(a, b) == Some(Ordering::Equal)
}

fn number(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) | Value::Timestamp(i) => Some(*i),
        _ => None,
    }
}

/// Two values in their order, where the model knows it: numbers by value,
/// texts by their bytes.
fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Text(x), Value::Text(y)) => Some(x.as_bytes().cmp(y.as_bytes())),
        _ => Some(number(a)?.cmp(&number(b)?)),
    }
}

/// A key of `order`: `null` first, then the values.
fn order_key(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        _ => compare(a, b).expect("a field holds one kind"),
    }
}

/// `v` as a field of type `ty` takes it, or `None` where it is refused.
fn fits(ty: Ty, v: &Value) -> Option<Value> {
    match (ty, v) {
        (_, Value::Null)
        | (Ty::Int, Value::Int(_))
        | (Ty::Text, Value::Text(_))
        | (Ty::Time, Value::Timestamp(_))
        | (Ty::Vector, Value::Vector(_))
        | (Ty::Sparse, Value::Sparse(..))
        | (Ty::Point, Value::List(_)) => Some(v.clone()),
        (Ty::Time, Value::Int(t)) => Some(Value::Timestamp(*t)),
        _ => None,
    }
}

fn point(lon: f64, lat: f64) -> Value {
    Value::List(vec![Value::Float(lon), Value::Float(lat)])
}

fn lon_lat(v: &Value) -> Option<(f64, f64)> {
    match v {
        Value::List(xs) => match xs.as_slice() {
            [Value::Float(lon), Value::Float(lat)] => Some((*lon, *lat)),
            _ => None,
        },
        _ => None,
    }
}

/// Whether two sparse vectors share a place: a dot product above zero, as
/// every value drawn is.
fn overlaps(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Sparse(_, x), Value::Sparse(_, y)) => {
            x.iter().any(|(i, _)| y.iter().any(|(j, _)| i == j))
        }
        _ => false,
    }
}

/// What the engine answers a statement, as the model works it out.
#[derive(Debug, PartialEq)]
enum Want {
    Count(usize),
    Rows(Vec<String>, Vec<(u64, Vec<Value>)>),
    Done,
    Fail(Kind),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Duplicate,
    Unmet,
    Type,
    Query,
    NotFound,
    Exists,
    /// Refused, in whatever words: a build made without what it needs.
    Any,
}

impl Kind {
    fn of(e: &Error) -> Option<Kind> {
        Some(match e {
            Error::Duplicate(_) => Kind::Duplicate,
            Error::Unmet(_) => Kind::Unmet,
            Error::Type(_) => Kind::Type,
            Error::Query(_) => Kind::Query,
            Error::NotFound(_) => Kind::NotFound,
            Error::Exists(_) => Kind::Exists,
            _ => return None,
        })
    }

    fn takes(self, e: &Error) -> bool {
        self == Kind::Any || Kind::of(e) == Some(self)
    }
}

/// Why the model gave no answer: a refusal it foresees, or a statement it
/// cannot answer for -- which no run the generator made holds, and a
/// shrunk one may.
enum Out {
    Fail(Kind),
    Unknown(String),
}

fn unknown<T>(why: impl Into<String>) -> Result<T, Out> {
    Err(Out::Unknown(why.into()))
}

fn fail<T>(k: Kind) -> Result<T, Out> {
    Err(Out::Fail(k))
}

#[derive(Clone, Debug)]
struct Model {
    cs: [Coll; 3],
    clock: i64,
    /// The change counter: one a row written, set, deleted or let go of
    /// past its time, and one a schema change (`Database::change_seq`,
    /// what replication and `/_changes` count by).
    writes: u64,
}

impl Model {
    fn new() -> Model {
        let mut a = Coll::new(
            "a",
            vec![
                field("n", Ty::Int, Ix::None),
                field("nh", Ty::Int, Ix::Hash),
                field("ns", Ty::Int, Ix::Sorted),
                field("u", Ty::Int, Ix::Unique),
                field("uu", Ty::Int, Ix::None),
                field("p", Ty::Int, Ix::None),
                field("q", Ty::Text, Ix::None),
                field("p2", Ty::Int, Ix::None),
                field("q2", Ty::Text, Ix::None),
            ],
        );
        a.composites.push(Composite {
            keys: vec!["p".into(), "q".into()],
            unique: false,
        });
        let mut b = Coll::new(
            "b",
            vec![
                field("g", Ty::Int, Ix::None),
                field("body", Ty::Text, Ix::Text),
                field("v", Ty::Vector, Ix::Graph),
                field("sp", Ty::Sparse, Ix::Inverted),
            ],
        );
        if cfg!(feature = "geo") {
            b.fields.push(field("loc", Ty::Point, Ix::Geo));
            b.fields.push(field("loc2", Ty::Point, Ix::None));
        }
        let mut t = Coll::new(
            "t",
            vec![
                field("at", Ty::Time, Ix::Ttl),
                field("at2", Ty::Time, Ix::None),
                field("k", Ty::Int, Ix::Unique),
                field("m", Ty::Int, Ix::None),
            ],
        );
        t.ttl = Some(TTL);
        Model {
            cs: [a, b, t],
            clock: START,
            writes: 0,
        }
    }

    fn coll(&self, c: C) -> &Coll {
        &self.cs[c as usize]
    }

    fn coll_mut(&mut self, c: C) -> &mut Coll {
        &mut self.cs[c as usize]
    }

    /// Now less `c`'s ttl: a row whose `at` is at or before it is past its
    /// time.
    fn cut(&self, c: C) -> Option<i64> {
        self.coll(c).ttl.map(|ttl| self.clock.saturating_sub(ttl))
    }

    /// `st` run as a block of one: what it answers, the model moved on
    /// where it lands -- or why the model cannot say.
    fn run(&mut self, st: &St) -> Result<Want, String> {
        let mut next = self.clone();
        match next.apply(st) {
            Ok(w) => {
                *self = next;
                Ok(w)
            }
            Err(Out::Fail(k)) => Ok(Want::Fail(k)),
            Err(Out::Unknown(why)) => Err(why),
        }
    }

    fn apply(&mut self, st: &St) -> Result<Want, Out> {
        // A schema change is one write; a write counts its rows as it goes.
        if !matches!(
            st,
            St::Put { .. } | St::Set { .. } | St::Del { .. } | St::Get { .. }
        ) {
            self.writes += 1;
        }
        match st {
            St::Put {
                c,
                docs,
                how,
                require,
                ret,
            } => self.put(*c, docs, how, *require, ret),
            St::Set {
                c,
                set,
                filter,
                order,
                limit,
                require,
                ret,
            } => {
                let ids = self.pick(*c, filter.as_ref(), order, *limit, 0)?;
                let mut written = Vec::with_capacity(ids.len());
                for id in ids {
                    // A row an earlier one let go of, past its time, is not
                    // there to set, and is not counted.
                    if self.coll(*c).rows.contains_key(&id) {
                        self.set_row(*c, id, set, None)?;
                        written.push(id);
                    }
                }
                self.writes += written.len() as u64;
                self.answer(*c, ret, &written, *require)
            }
            St::Del {
                c,
                filter,
                order,
                limit,
                require,
                ret,
            } => {
                let ids = self.pick(*c, filter.as_ref(), order, *limit, 0)?;
                // Read before they go: what a pop hands back.
                let want = self.answer(*c, ret, &ids, *require)?;
                self.writes += ids.len() as u64;
                let coll = self.coll_mut(*c);
                for id in &ids {
                    coll.rows.remove(id);
                }
                Ok(want)
            }
            St::Get {
                c,
                filter,
                order,
                limit,
                offset,
                require,
            } => {
                let ids = self.pick(*c, filter.as_ref(), order, *limit, *offset)?;
                if require.is_some_and(|r| r != ids.len() as u64) {
                    return fail(Kind::Unmet);
                }
                let coll = self.coll(*c);
                let columns = coll.columns();
                let rows = ids
                    .iter()
                    .map(|id| (*id, coll.values(*id, &coll.rows[id], &columns)))
                    .collect();
                Ok(Want::Rows(columns, rows))
            }
            St::Add { c, name, ty, ix } => {
                let coll = self.coll_mut(*c);
                if coll.has(name) {
                    return fail(Kind::Exists);
                }
                if !ix.built() {
                    return fail(Kind::Any);
                }
                coll.fields.push(field(name, *ty, *ix));
                Ok(Want::Done)
            }
            St::Drop { c, name } => {
                let coll = self.coll_mut(*c);
                let Some(at) = coll.fields.iter().position(|f| &f.name == name) else {
                    return fail(Kind::NotFound);
                };
                coll.fields.remove(at);
                coll.composites.retain(|k| !k.keys.contains(name));
                for row in coll.rows.values_mut() {
                    if row.contains_key(name) {
                        Rc::make_mut(row).remove(name);
                    }
                }
                Ok(Want::Done)
            }
            St::Rename { c, from, to } => {
                let coll = self.coll_mut(*c);
                if !coll.has(from) {
                    return fail(Kind::NotFound);
                }
                if coll.has(to) {
                    return fail(Kind::Exists);
                }
                for f in &mut coll.fields {
                    if &f.name == from {
                        f.name.clone_from(to);
                    }
                }
                for k in &mut coll.composites {
                    for key in &mut k.keys {
                        if key == from {
                            key.clone_from(to);
                        }
                    }
                }
                for row in coll.rows.values_mut() {
                    if let Some(v) = row.get(from).cloned() {
                        let row = Rc::make_mut(row);
                        row.remove(from);
                        row.insert(to.clone(), v);
                    }
                }
                Ok(Want::Done)
            }
            St::Expiry { ttl } => {
                let coll = self.coll_mut(C::T);
                coll.ttl = *ttl;
                for f in &mut coll.fields {
                    if f.name == "at" {
                        f.ix = if ttl.is_some() { Ix::Ttl } else { Ix::Sorted };
                    }
                }
                Ok(Want::Done)
            }
            St::Index { c, name, ix } => {
                let coll = self.coll_mut(*c);
                let Some(f) = coll.field(name) else {
                    return fail(Kind::NotFound);
                };
                if f.ix != Ix::None {
                    return fail(Kind::Exists);
                }
                if !ix.built() {
                    return fail(Kind::Any);
                }
                if *ix == Ix::Unique {
                    let mut seen: Vec<&Value> = Vec::new();
                    for row in coll.rows.values() {
                        if let Some(v) = row.get(name) {
                            if seen.iter().any(|w| same(v, w)) {
                                return fail(Kind::Duplicate);
                            }
                            seen.push(v);
                        }
                    }
                }
                for f in &mut coll.fields {
                    if &f.name == name {
                        f.ix = *ix;
                    }
                }
                Ok(Want::Done)
            }
            St::Compose { c, keys, unique } => {
                let coll = self.coll_mut(*c);
                if keys.iter().any(|k| !coll.has(k)) {
                    return fail(Kind::NotFound);
                }
                if coll.composites.iter().any(|k| &k.keys == keys) {
                    return fail(Kind::Exists);
                }
                if !cfg!(feature = "composite") {
                    return fail(Kind::Any);
                }
                if *unique {
                    let mut seen: Vec<Vec<&Value>> = Vec::new();
                    for row in coll.rows.values() {
                        let Some(vs) = keys.iter().map(|k| row.get(k)).collect::<Option<Vec<_>>>()
                        else {
                            continue;
                        };
                        if seen
                            .iter()
                            .any(|w| w.iter().zip(&vs).all(|(a, b)| same(a, b)))
                        {
                            return fail(Kind::Duplicate);
                        }
                        seen.push(vs);
                    }
                }
                coll.composites.push(Composite {
                    keys: keys.clone(),
                    unique: *unique,
                });
                Ok(Want::Done)
            }
            St::Decompose { c, keys } => {
                let coll = self.coll_mut(*c);
                let Some(at) = coll.composites.iter().position(|k| &k.keys == keys) else {
                    return fail(Kind::NotFound);
                };
                coll.composites.remove(at);
                Ok(Want::Done)
            }
        }
    }

    fn put(
        &mut self,
        c: C,
        docs: &[Doc],
        how: &How,
        require: Option<u64>,
        ret: &Option<Ret>,
    ) -> Result<Want, Out> {
        let insert = !matches!(how, How::Put);
        let if_absent = matches!(how, How::Absent | How::Upsert(_));
        // Every document is built, each field checked, before any is
        // written.
        let mut built = Vec::with_capacity(docs.len());
        {
            let coll = self.coll(c);
            for d in docs {
                let mut row = Row::new();
                for (name, v) in &d.values {
                    let Some(f) = coll.field(name) else {
                        return unknown(format!("no field `{name}` to put"));
                    };
                    match fits(f.ty, v) {
                        None => return fail(Kind::Type),
                        Some(Value::Null) => {}
                        Some(v) => {
                            row.insert(name.clone(), v);
                        }
                    }
                }
                built.push((d.id, row));
            }
        }
        let unique = self.coll(c).has_unique();
        // A row past its time is out of a write's way: an id it holds is
        // free, and a unique value it holds is let go of with it.
        let cut = match self.cut(c) {
            Some(cut) if insert || unique => Some(cut),
            _ => None,
        };
        let mut written = Vec::new();
        for (named, row) in built {
            let coll = self.coll_mut(c);
            let mut gone = 0;
            if let (Some(cut), true) = (cut, unique) {
                for other in coll.holders(&row, named.unwrap_or(0)) {
                    if coll.expired(other, cut) {
                        coll.rows.remove(&other);
                        gone += 1;
                    }
                }
            }
            self.writes += gone;
            let coll = self.coll_mut(c);
            let mut held = None;
            let (id, allocated) = match named {
                None => {
                    coll.next_id += 1;
                    (coll.next_id - 1, true)
                }
                Some(id) => {
                    let gone = cut.is_some_and(|cut| coll.expired(id, cut));
                    if insert && !gone && coll.rows.contains_key(&id) {
                        if !if_absent {
                            return fail(Kind::Duplicate);
                        }
                        match how {
                            How::Upsert(_) => held = Some(id),
                            _ => continue,
                        }
                    }
                    (id, false)
                }
            };
            if held.is_none() {
                let clash = coll.holders(&row, id);
                if clash.is_empty() {
                    coll.rows.insert(id, Rc::new(row));
                    coll.next_id = coll.next_id.max(id + 1);
                    written.push(id);
                    continue;
                }
                if !if_absent {
                    return fail(Kind::Duplicate);
                }
                // Passed over: the id it was handed is handed out again.
                if allocated {
                    coll.next_id -= 1;
                }
                match how {
                    How::Upsert(_) => held = Some(clash[0]),
                    _ => continue,
                }
            }
            let (Some(id), How::Upsert(set)) = (held, how) else {
                unreachable!("only an upsert sets the row it found")
            };
            self.set_row(c, id, set, Some(&row))?;
            written.push(id);
        }
        self.writes += written.len() as u64;
        self.answer(c, ret, &written, require)
    }

    /// Row `id` set by `set`, each value worked out over the row as it was,
    /// `new` an upsert's document.
    fn set_row(
        &mut self,
        c: C,
        id: u64,
        set: &[(String, Ex)],
        new: Option<&Row>,
    ) -> Result<(), Out> {
        let cut = self.cut(c);
        let coll = self.coll_mut(c);
        let old = Rc::clone(&coll.rows[&id]);
        let mut row: Row = (*old).clone();
        for (name, ex) in set {
            let Some(f) = coll.field(name) else {
                return unknown(format!("no field `{name}` to set"));
            };
            let v = ex.eval(&old, new)?;
            match fits(f.ty, &v) {
                None => return fail(Kind::Type),
                Some(Value::Null) => {
                    row.remove(name);
                }
                Some(v) => {
                    row.insert(name.clone(), v);
                }
            }
        }
        // A unique value a row past its time holds is let go of with it, as
        // a `put`'s is: the row is out of every read.
        let mut gone = 0;
        if let (Some(cut), true) = (cut, coll.has_unique()) {
            for other in coll.holders(&row, id) {
                if coll.expired(other, cut) {
                    coll.rows.remove(&other);
                    gone += 1;
                }
            }
        }
        if !coll.holders(&row, id).is_empty() {
            return fail(Kind::Duplicate);
        }
        coll.rows.insert(id, Rc::new(row));
        self.writes += gone;
        Ok(())
    }

    /// What a write answers: its count, or with `returning` the rows it
    /// wrote, each as the statement left it -- a `del`'s as it found them
    /// -- held to `require` either way.
    fn answer(
        &self,
        c: C,
        ret: &Option<Ret>,
        ids: &[u64],
        require: Option<u64>,
    ) -> Result<Want, Out> {
        let coll = self.coll(c);
        let want = match ret {
            None => Want::Count(ids.len()),
            Some(ret) => {
                let columns = match ret {
                    Ret::All => coll.columns(),
                    Ret::These(names) => {
                        if let Some(name) = names.iter().find(|n| !coll.has(n)) {
                            return unknown(format!("no field `{name}` to return"));
                        }
                        let mut c = vec!["id".to_string()];
                        c.extend(names.iter().cloned());
                        c
                    }
                };
                // A row the write did not find is not answered.
                let rows = ids
                    .iter()
                    .filter_map(|id| {
                        coll.rows
                            .get(id)
                            .map(|row| (*id, coll.values(*id, row, &columns)))
                    })
                    .collect();
                Want::Rows(columns, rows)
            }
        };
        let n = match &want {
            Want::Rows(_, rows) => rows.len(),
            _ => ids.len(),
        };
        if require.is_some_and(|r| r != n as u64) {
            return fail(Kind::Unmet);
        }
        Ok(want)
    }

    /// The rows a statement reaches: the living -- or, asked for, those
    /// past their time -- its filter keeps, in its order, ties by id, from
    /// `offset`, at most `limit`.
    fn pick(
        &self,
        c: C,
        filter: Option<&F>,
        order: &[(String, bool)],
        limit: Option<u64>,
        offset: u64,
    ) -> Result<Vec<u64>, Out> {
        let coll = self.coll(c);
        let cut = self.cut(c);
        let expired = filter.is_some_and(F::asks_expired);
        if let Some(f) = filter {
            f.check(coll)?;
        }
        // `expired()` of a collection whose rows no longer expire is refused
        // at the first row it is asked of, and never over none (a filter
        // binds what it can, the rest an error of the first row).
        if expired && cut.is_none() {
            return match (coll.rows.is_empty(), filter) {
                (true, _) => Ok(Vec::new()),
                (false, Some(F::Expired)) => fail(Kind::Query),
                _ => {
                    unknown("`expired()` among other tests, of a collection that no longer expires")
                }
            };
        }
        for (key, _) in order {
            if key != "id" && !coll.has(key) {
                return unknown(format!("no field `{key}` to order by"));
            }
        }
        let mut ids = Vec::new();
        for (id, row) in &coll.rows {
            if !expired && !coll.alive(row, cut) {
                continue;
            }
            if filter.is_none_or(|f| f.eval(*id, row, cut)) {
                ids.push(*id);
            }
        }
        if !order.is_empty() {
            let get = |id: &u64, key: &str| match key {
                "id" => Value::Int(*id as i64),
                f => coll.rows[id].get(f).cloned().unwrap_or(Value::Null),
            };
            ids.sort_by(|a, b| {
                for (key, desc) in order {
                    let o = order_key(&get(a, key), &get(b, key));
                    let o = if *desc { o.reverse() } else { o };
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                a.cmp(b)
            });
        }
        let from = (offset as usize).min(ids.len());
        let to = match limit {
            Some(l) => (from + l as usize).min(ids.len()),
            None => ids.len(),
        };
        Ok(ids[from..to].to_vec())
    }

    /// The rows of `c` a read sees, ascending.
    fn visible(&self, c: C) -> Vec<u64> {
        let coll = self.coll(c);
        let cut = self.cut(c);
        coll.rows
            .iter()
            .filter(|(_, row)| coll.alive(row, cut))
            .map(|(id, _)| *id)
            .collect()
    }

    fn past_their_time(&self, c: C) -> Vec<u64> {
        let coll = self.coll(c);
        match self.cut(c) {
            Some(cut) => coll
                .rows
                .keys()
                .filter(|id| coll.expired(**id, cut))
                .copied()
                .collect(),
            None => Vec::new(),
        }
    }
}

// ------------------------------------------------------- the statements

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Op {
    fn word(self) -> &'static str {
        match self {
            Op::Eq => "=",
            Op::Ne => "!=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Gt => ">",
            Op::Ge => ">=",
        }
    }

    fn holds(self, o: Ordering) -> bool {
        match self {
            Op::Eq => o.is_eq(),
            Op::Ne => o.is_ne(),
            Op::Lt => o.is_lt(),
            Op::Le => o.is_le(),
            Op::Gt => o.is_gt(),
            Op::Ge => o.is_ge(),
        }
    }
}

const OPS: [Op; 6] = [Op::Eq, Op::Ne, Op::Lt, Op::Le, Op::Gt, Op::Ge];

/// A filter, as the model reads it: two-valued, a comparison with `null`
/// false but for `!=`.
#[derive(Clone, Debug)]
enum F {
    Cmp(String, Op, Value),
    In(String, Vec<Value>),
    Between(String, Value, Value),
    /// `is null`, or `is not null` when true.
    Null(String, bool),
    Id(Op, u64),
    Ids(Vec<u64>),
    And(Box<F>, Box<F>),
    Or(Box<F>, Box<F>),
    Not(Box<F>),
    /// `expired()`: the rows past their time, the living left out.
    Expired,
    Within(String, [f64; 4]),
}

impl F {
    fn asks_expired(&self) -> bool {
        match self {
            F::Expired => true,
            F::And(a, b) | F::Or(a, b) => a.asks_expired() || b.asks_expired(),
            F::Not(a) => a.asks_expired(),
            _ => false,
        }
    }

    /// Every field it reads is the collection's, each literal of the kind
    /// the field holds: what else the model cannot answer for.
    fn check(&self, coll: &Coll) -> Result<(), Out> {
        let kind = |name: &str, v: &Value| -> Result<(), Out> {
            let Some(f) = coll.field(name) else {
                return unknown(format!("no field `{name}` to filter by"));
            };
            let fine = matches!(
                (f.ty, v),
                (Ty::Int | Ty::Time, Value::Int(_) | Value::Timestamp(_))
                    | (Ty::Text, Value::Text(_))
            );
            if fine {
                Ok(())
            } else {
                unknown(format!("`{name}` compared with {v:?}"))
            }
        };
        match self {
            F::Cmp(f, _, v) => kind(f, v),
            F::In(f, vs) => vs.iter().try_for_each(|v| kind(f, v)),
            F::Between(f, a, b) => kind(f, a).and_then(|_| kind(f, b)),
            F::Null(f, _) | F::Within(f, _) => match coll.has(f) {
                true => Ok(()),
                false => unknown(format!("no field `{f}` to filter by")),
            },
            F::And(a, b) | F::Or(a, b) => a.check(coll).and_then(|_| b.check(coll)),
            F::Not(a) => a.check(coll),
            F::Id(..) | F::Ids(_) | F::Expired => Ok(()),
        }
    }

    fn eval(&self, id: u64, row: &Row, cut: Option<i64>) -> bool {
        let at = |f: &str| row.get(f);
        match self {
            F::Cmp(f, op, lit) => match at(f) {
                None => *op == Op::Ne,
                Some(v) => op.holds(compare(v, lit).expect("checked")),
            },
            F::In(f, lits) => at(f).is_some_and(|v| lits.iter().any(|l| same(v, l))),
            F::Between(f, lo, hi) => at(f).is_some_and(|v| {
                compare(v, lo).expect("checked").is_ge() && compare(v, hi).expect("checked").is_le()
            }),
            F::Null(f, not) => at(f).is_none() != *not,
            F::Id(op, n) => op.holds(id.cmp(n)),
            F::Ids(ids) => ids.contains(&id),
            F::And(a, b) => a.eval(id, row, cut) && b.eval(id, row, cut),
            F::Or(a, b) => a.eval(id, row, cut) || b.eval(id, row, cut),
            F::Not(a) => !a.eval(id, row, cut),
            F::Expired => match (cut, at("at")) {
                (Some(cut), Some(Value::Timestamp(t))) => *t <= cut,
                _ => false,
            },
            F::Within(f, [w, s, e, n]) => at(f)
                .and_then(lon_lat)
                .is_some_and(|(lon, lat)| *w <= lon && lon <= *e && *s <= lat && lat <= *n),
        }
    }

    fn renamed(&self, from: &str, to: &str) -> F {
        let name = |f: &String| if f == from { to.to_string() } else { f.clone() };
        match self {
            F::Cmp(f, op, v) => F::Cmp(name(f), *op, v.clone()),
            F::In(f, vs) => F::In(name(f), vs.clone()),
            F::Between(f, a, b) => F::Between(name(f), a.clone(), b.clone()),
            F::Null(f, not) => F::Null(name(f), *not),
            F::Within(f, b) => F::Within(name(f), *b),
            F::And(a, b) => F::And(Box::new(a.renamed(from, to)), Box::new(b.renamed(from, to))),
            F::Or(a, b) => F::Or(Box::new(a.renamed(from, to)), Box::new(b.renamed(from, to))),
            F::Not(a) => F::Not(Box::new(a.renamed(from, to))),
            other => other.clone(),
        }
    }

    fn text(&self) -> String {
        match self {
            F::Cmp(f, op, v) => format!("{f} {} {}", op.word(), lit(v)),
            F::In(f, vs) => format!("{f} in [{}]", list(vs)),
            F::Between(f, a, b) => format!("{f} between {} and {}", lit(a), lit(b)),
            F::Null(f, false) => format!("{f} is null"),
            F::Null(f, true) => format!("{f} is not null"),
            F::Id(op, n) => format!("id {} {n}", op.word()),
            F::Ids(ids) => format!(
                "id in [{}]",
                ids.iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            F::And(a, b) => format!("({} and {})", a.text(), b.text()),
            F::Or(a, b) => format!("({} or {})", a.text(), b.text()),
            F::Not(a) => format!("not ({})", a.text()),
            F::Expired => "expired()".to_string(),
            F::Within(f, [w, s, e, n]) => format!("within({f}, [{w}, {s}, {e}, {n}])"),
        }
    }
}

fn lit(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Int(i) | Value::Timestamp(i) => i.to_string(),
        Value::Text(s) => format!("\"{s}\""),
        Value::Vector(xs) => format!(
            "[{}]",
            xs.iter().map(f32::to_string).collect::<Vec<_>>().join(", ")
        ),
        // A sparse vector's text counts its places from 1, as pgvector's.
        Value::Sparse(dim, es) => format!(
            "\"{{{}}}/{dim}\"",
            es.iter()
                .map(|(i, x)| format!("{}:{x}", i + 1))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::List(_) => {
            let (lon, lat) = lon_lat(v).expect("a point");
            format!("[{lon}, {lat}]")
        }
        other => unreachable!("no literal for {other:?}"),
    }
}

fn list(vs: &[Value]) -> String {
    vs.iter().map(lit).collect::<Vec<_>>().join(", ")
}

/// A `set` value: worked out over the row as it was.
#[derive(Clone, Debug)]
enum Ex {
    Lit(Value),
    /// `f + k`
    Plus(String, i64),
    /// `coalesce(f, 0) + k`
    Coalesce(String, i64),
    /// `coalesce(f, 0) + new.g`, an upsert's.
    Merge(String, String),
}

impl Ex {
    fn eval(&self, old: &Row, new: Option<&Row>) -> Result<Value, Out> {
        let plus = |a: &Value, k: i64| match a {
            Value::Null => Ok(Value::Null),
            Value::Int(i) => i
                .checked_add(k)
                .map(Value::Int)
                .ok_or(Out::Fail(Kind::Query)),
            Value::Timestamp(t) => t
                .checked_add(k)
                .map(Value::Timestamp)
                .ok_or(Out::Fail(Kind::Query)),
            other => Err(Out::Unknown(format!("{other:?} + {k}"))),
        };
        match self {
            Ex::Lit(v) => Ok(v.clone()),
            Ex::Plus(f, k) => plus(old.get(f).unwrap_or(&Value::Null), *k),
            Ex::Coalesce(f, k) => plus(old.get(f).unwrap_or(&Value::Int(0)), *k),
            Ex::Merge(f, g) => {
                let Some(new) = new else {
                    return unknown("`new.` outside an upsert");
                };
                match new.get(g) {
                    None => Ok(Value::Null),
                    Some(Value::Int(k)) => plus(old.get(f).unwrap_or(&Value::Int(0)), *k),
                    Some(other) => unknown(format!("new.{g} is {other:?}")),
                }
            }
        }
    }

    fn text(&self) -> String {
        match self {
            Ex::Lit(v) => lit(v),
            Ex::Plus(f, k) => format!("{f} + {k}"),
            Ex::Coalesce(f, k) => format!("coalesce({f}, 0) + {k}"),
            Ex::Merge(f, g) => format!("coalesce({f}, 0) + new.{g}"),
        }
    }
}

#[derive(Clone, Debug)]
struct Doc {
    id: Option<u64>,
    values: Vec<(String, Value)>,
}

impl Doc {
    fn text(&self) -> String {
        let mut parts = Vec::new();
        if let Some(id) = self.id {
            parts.push(format!("id: {id}"));
        }
        for (f, v) in &self.values {
            parts.push(format!("{f}: {}", lit(v)));
        }
        format!("{{{}}}", parts.join(", "))
    }
}

#[derive(Clone, Debug)]
enum How {
    Put,
    Insert,
    /// `put ... if absent`
    Absent,
    /// `put ... if absent else set {...}`
    Upsert(Vec<(String, Ex)>),
}

#[derive(Clone, Debug)]
enum Ret {
    All,
    These(Vec<String>),
}

#[derive(Clone, Debug)]
enum St {
    Put {
        c: C,
        docs: Vec<Doc>,
        how: How,
        require: Option<u64>,
        ret: Option<Ret>,
    },
    Set {
        c: C,
        set: Vec<(String, Ex)>,
        filter: Option<F>,
        order: Vec<(String, bool)>,
        limit: Option<u64>,
        require: Option<u64>,
        ret: Option<Ret>,
    },
    Del {
        c: C,
        filter: Option<F>,
        order: Vec<(String, bool)>,
        limit: Option<u64>,
        require: Option<u64>,
        ret: Option<Ret>,
    },
    /// A read in a block, or one guarding it with `require`.
    Get {
        c: C,
        filter: Option<F>,
        order: Vec<(String, bool)>,
        limit: Option<u64>,
        offset: u64,
        require: Option<u64>,
    },
    Add {
        c: C,
        name: String,
        ty: Ty,
        ix: Ix,
    },
    Drop {
        c: C,
        name: String,
    },
    Rename {
        c: C,
        from: String,
        to: String,
    },
    /// `t`'s `at` made to expire after `ttl`, or no longer.
    Expiry {
        ttl: Option<i64>,
    },
    Index {
        c: C,
        name: String,
        ix: Ix,
    },
    Compose {
        c: C,
        keys: Vec<String>,
        unique: bool,
    },
    Decompose {
        c: C,
        keys: Vec<String>,
    },
}

fn assigns(set: &[(String, Ex)]) -> String {
    let pairs: Vec<String> = set
        .iter()
        .map(|(f, e)| format!("{f}: {}", e.text()))
        .collect();
    format!("{{{}}}", pairs.join(", "))
}

fn tail(
    filter: &Option<F>,
    order: &[(String, bool)],
    limit: Option<u64>,
    offset: u64,
    require: Option<u64>,
) -> String {
    let mut s = String::new();
    if let Some(f) = filter {
        let _ = write!(s, " where {}", f.text());
    }
    if !order.is_empty() {
        let keys: Vec<String> = order
            .iter()
            .map(|(k, desc)| {
                if *desc {
                    format!("{k} desc")
                } else {
                    k.clone()
                }
            })
            .collect();
        let _ = write!(s, " order {}", keys.join(", "));
    }
    if let Some(l) = limit {
        let _ = write!(s, " limit {l}");
    }
    if offset > 0 {
        let _ = write!(s, " offset {offset}");
    }
    if let Some(r) = require {
        let _ = write!(s, " require {r}");
    }
    s
}

fn returning(ret: &Option<Ret>) -> String {
    match ret {
        None => String::new(),
        Some(Ret::All) => " returning *".to_string(),
        Some(Ret::These(names)) => format!(" returning id, {}", names.join(", ")),
    }
}

impl St {
    fn text(&self) -> String {
        match self {
            St::Put {
                c,
                docs,
                how,
                require,
                ret,
            } => {
                let verb = if matches!(how, How::Insert) {
                    "insert"
                } else {
                    "put"
                };
                let docs = match docs.as_slice() {
                    [one] => one.text(),
                    many => format!(
                        "[{}]",
                        many.iter().map(Doc::text).collect::<Vec<_>>().join(", ")
                    ),
                };
                let how = match how {
                    How::Put | How::Insert => String::new(),
                    How::Absent => " if absent".to_string(),
                    How::Upsert(set) => format!(" if absent else set {}", assigns(set)),
                };
                format!(
                    "{verb} {} {docs}{how}{}{}",
                    c.name(),
                    tail(&None, &[], None, 0, *require),
                    returning(ret)
                )
            }
            St::Set {
                c,
                set,
                filter,
                order,
                limit,
                require,
                ret,
            } => format!(
                "set {} {}{}{}",
                c.name(),
                assigns(set),
                tail(filter, order, *limit, 0, *require),
                returning(ret)
            ),
            St::Del {
                c,
                filter,
                order,
                limit,
                require,
                ret,
            } => format!(
                "del {}{}{}",
                c.name(),
                tail(filter, order, *limit, 0, *require),
                returning(ret)
            ),
            St::Get {
                c,
                filter,
                order,
                limit,
                offset,
                require,
            } => format!(
                "get {}{}",
                c.name(),
                tail(filter, order, *limit, *offset, *require)
            ),
            St::Add { c, name, ty, ix } => format!(
                "alter collection {} add field {name} {}{}",
                c.name(),
                ty.word(),
                ix.word()
            ),
            St::Drop { c, name } => format!("alter collection {} drop field {name}", c.name()),
            St::Rename { c, from, to } => {
                format!("alter collection {} rename field {from} to {to}", c.name())
            }
            St::Expiry { ttl: Some(ms) } => {
                format!("alter collection t alter field at @ttl({}s)", ms / 1000)
            }
            St::Expiry { ttl: None } => "alter collection t alter field at @sorted".to_string(),
            St::Index { c, name, ix } => {
                format!("create index on {} ({name}){}", c.name(), ix.word())
            }
            St::Compose { c, keys, unique } => format!(
                "create index on {} ({}) @{}",
                c.name(),
                keys.join(", "),
                if *unique { "unique" } else { "sorted" }
            ),
            St::Decompose { c, keys } => {
                format!("drop index on {} ({})", c.name(), keys.join(", "))
            }
        }
    }

    fn coll(&self) -> C {
        match self {
            St::Put { c, .. }
            | St::Set { c, .. }
            | St::Del { c, .. }
            | St::Get { c, .. }
            | St::Add { c, .. }
            | St::Drop { c, .. }
            | St::Rename { c, .. }
            | St::Index { c, .. }
            | St::Compose { c, .. }
            | St::Decompose { c, .. } => *c,
            St::Expiry { .. } => C::T,
        }
    }

    fn require_mut(&mut self) -> Option<&mut Option<u64>> {
        match self {
            St::Put { require, .. } | St::Set { require, .. } | St::Del { require, .. } => {
                Some(require)
            }
            _ => None,
        }
    }
}

/// A read held to the model, and through each twin to the others.
#[derive(Clone, Debug)]
enum Read {
    /// `get <c> select id` with this filter and page over the scan's twin,
    /// then again over each other twin -- `with` renames it to them.
    Twins {
        c: C,
        filter: Option<F>,
        order: Vec<(String, bool)>,
        limit: Option<u64>,
        offset: u64,
        with: Vec<Vec<(&'static str, &'static str)>>,
    },
    Count {
        c: C,
        filter: Option<F>,
    },
    /// `match` beside `order`: the rows holding the word.
    Match(&'static str),
    /// `near v` over every row it reaches, through the graph and exactly.
    Near([f32; 3]),
    /// `near sp`, through `@inverted` and exactly.
    Sparse(Value, Option<u64>),
}

impl Read {
    fn text(&self) -> String {
        match self {
            Read::Twins {
                c,
                filter,
                order,
                limit,
                offset,
                with,
            } => {
                let twins: Vec<String> = with
                    .iter()
                    .map(|w| {
                        w.iter()
                            .map(|(a, b)| format!("{a} as {b}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .collect();
                format!(
                    "get {} select id{} [again with {}]",
                    c.name(),
                    tail(filter, order, *limit, *offset, None),
                    twins.join("; ")
                )
            }
            Read::Count { c, filter } => {
                format!("get {}{} count", c.name(), tail(filter, &[], None, 0, None))
            }
            Read::Match(w) => format!("get b select id match body \"{w}\" order id"),
            Read::Near(q) => format!(
                "get b select id near v {} [and exact]",
                lit(&Value::Vector(q.to_vec()))
            ),
            Read::Sparse(q, limit) => format!(
                "get b select id near sp {}{} [and exact]",
                lit(q),
                limit.map(|l| format!(" limit {l}")).unwrap_or_default()
            ),
        }
    }
}

/// One step of a run.
#[derive(Clone, Debug)]
enum Step {
    Run(St),
    /// `execute_block`: every statement lands, as one record, or none.
    Block(Vec<St>),
    /// `begin`, the statements, then `commit` -- or `rollback` when false,
    /// or at the first refused.
    Begin(Vec<St>, bool),
    Compact,
    Checkpoint,
    Clock(i64),
    Sweep,
    Reopen,
    /// A crash: the file cut back past at most this many of its last
    /// records, as `Cut` says, then opened again.
    Crash(usize, Cut),
    Check(Vec<Read>),
}

#[derive(Clone, Copy, Debug)]
enum Cut {
    /// At the record boundary.
    Clean,
    /// At the boundary, then this many zeros: a file the disk grew and
    /// never wrote.
    Zeros(usize),
    /// Inside the record after the boundary.
    Torn(usize),
    /// Zeros from inside the record after the boundary to the end, the
    /// file as long as it was: pages a power cut lost.
    Zeroed(usize),
}

impl Step {
    fn text(&self) -> String {
        let block = |sts: &[St]| sts.iter().map(St::text).collect::<Vec<_>>().join("; ");
        match self {
            Step::Run(st) => st.text(),
            Step::Block(sts) => format!("[block] {}", block(sts)),
            Step::Begin(sts, true) => format!("[begin] {} [commit]", block(sts)),
            Step::Begin(sts, false) => format!("[begin] {} [rollback]", block(sts)),
            Step::Compact => "compact".to_string(),
            Step::Checkpoint => "-- Database::checkpoint".to_string(),
            Step::Clock(t) => format!("-- Database::set_clock({t})"),
            Step::Sweep => "-- t swept (Database::expired, Database::sweep)".to_string(),
            Step::Reopen => "-- the file opened again".to_string(),
            Step::Crash(back, cut) => {
                format!("-- the file cut back {back} records ({cut:?}) and opened again")
            }
            Step::Check(reads) => format!(
                "-- check: {}",
                reads.iter().map(Read::text).collect::<Vec<_>>().join("; ")
            ),
        }
    }
}

// --------------------------------------------------------- the generator

/// A value of `name`'s, drawn from few so that rows share them: a bucket
/// of many, a range over equals, a unique value clashing.
fn drawn(r: &mut Rng, m: &Model, c: C, name: &str) -> Value {
    let text = |s: &str| Value::Text(s.to_string());
    match name {
        // Now and then at an end of an int, where `+ 1` overflows.
        "n" | "nh" | "ns" if r.one_in(30) => {
            Value::Int(*r.pick(&[i64::MAX, i64::MAX - 2, i64::MIN + 1, i64::MIN + 3]))
        }
        "n" | "nh" | "ns" => Value::Int(r.range(-3, 12)),
        "u" | "uu" => Value::Int(r.range(0, 40)),
        "p" | "p2" => Value::Int(r.range(0, 3)),
        "q" | "q2" => text(r.pick(&LETTERS)),
        "g" => Value::Int(r.range(0, 5)),
        "body" => {
            let words: Vec<&str> = (0..1 + r.upto(3)).map(|_| *r.pick(&WORDS)).collect();
            Value::Text(words.join(" "))
        }
        "v" => Value::Vector(r.pick(&VECTORS).to_vec()),
        "sp" => {
            let mut es = Vec::new();
            for i in 0..6u32 {
                if r.one_in(3) {
                    es.push((i, *r.pick(&[0.5f32, 1.0, 2.0])));
                }
            }
            if es.is_empty() {
                es.push((r.below(6) as u32, 1.0));
            }
            Value::Sparse(6, es)
        }
        "loc" | "loc2" => point(*r.pick(&LONS), *r.pick(&LATS)),
        "at" | "at2" => Value::Timestamp(m.clock + r.range(-15_000, 15_000)),
        "k" => Value::Int(r.range(0, 20)),
        "m" => Value::Int(r.range(0, 9)),
        extra => match m.coll(c).field(extra).map(|f| f.ty) {
            Some(Ty::Text) => text(r.pick(&LETTERS)),
            _ => Value::Int(r.range(0, 6)),
        },
    }
}

fn value(r: &mut Rng, m: &Model, c: C, name: &str) -> Value {
    if r.one_in(10) {
        Value::Null
    } else {
        drawn(r, m, c, name)
    }
}

/// The fields written together: a twin's every field, or one field alone.
fn groups(m: &Model, c: C) -> Vec<Vec<String>> {
    let names = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    match c {
        C::A => {
            let mut g = vec![names(&N), names(&U), names(&P), names(&Q)];
            for f in &m.coll(c).fields {
                if f.name.starts_with('x') {
                    g.push(vec![f.name.clone()]);
                }
            }
            g
        }
        C::B => {
            let mut g = vec![
                names(&["g"]),
                names(&["body"]),
                names(&["v"]),
                names(&["sp"]),
            ];
            if cfg!(feature = "geo") {
                g.push(names(&L));
            }
            g
        }
        C::T => vec![names(&AT), names(&["k"]), names(&["m"])],
    }
}

/// An id a write names: one a row holds, one past the last handed out, or
/// one below it that may be free.
fn named_id(r: &mut Rng, m: &Model, c: C) -> u64 {
    let coll = m.coll(c);
    let ids: Vec<u64> = coll.rows.keys().copied().collect();
    match r.below(10) {
        0..=5 if !ids.is_empty() => *r.pick(&ids),
        6 | 7 => coll.next_id + r.below(3),
        _ => 1 + r.below(coll.next_id),
    }
}

fn doc(r: &mut Rng, m: &Model, c: C) -> Doc {
    let id = r.one_in(4).then(|| named_id(r, m, c));
    let mut values = Vec::new();
    for g in groups(m, c) {
        if r.below(10) < 7 {
            let v = value(r, m, c, &g[0]);
            for f in g {
                values.push((f, v.clone()));
            }
        }
    }
    Doc { id, values }
}

fn id_filter(r: &mut Rng, m: &Model, c: C) -> F {
    match r.below(3) {
        0 => F::Id(Op::Eq, named_id(r, m, c)),
        1 => F::Id(*r.pick(&OPS), named_id(r, m, c)),
        _ => F::Ids((0..1 + r.upto(3)).map(|_| named_id(r, m, c)).collect()),
    }
}

/// A test of one field, `name`.
fn test_of(r: &mut Rng, m: &Model, c: C, name: &str) -> F {
    let name = name.to_string();
    match r.below(10) {
        0..=4 => F::Cmp(name.clone(), *r.pick(&OPS), drawn(r, m, c, &name)),
        5 | 6 => F::In(
            name.clone(),
            (0..1 + r.upto(3)).map(|_| drawn(r, m, c, &name)).collect(),
        ),
        7 => {
            let (a, b) = (drawn(r, m, c, &name), drawn(r, m, c, &name));
            match compare(&a, &b) {
                Some(Ordering::Greater) => F::Between(name, b, a),
                _ => F::Between(name, a, b),
            }
        }
        _ => F::Null(name, r.one_in(2)),
    }
}

fn bbox(r: &mut Rng) -> [f64; 4] {
    let (w, e) = (*r.pick(&LONS), *r.pick(&LONS));
    let (s, n) = (*r.pick(&LATS), *r.pick(&LATS));
    [w.min(e), s.min(n), w.max(e), s.max(n)]
}

fn filter(r: &mut Rng, m: &Model, c: C, depth: u32) -> F {
    if depth < 2 && r.one_in(4) {
        let a = Box::new(filter(r, m, c, depth + 1));
        return match r.below(3) {
            0 => F::And(a, Box::new(filter(r, m, c, depth + 1))),
            1 => F::Or(a, Box::new(filter(r, m, c, depth + 1))),
            _ => F::Not(a),
        };
    }
    if r.one_in(6) {
        return id_filter(r, m, c);
    }
    if c == C::T && m.coll(c).ttl.is_some() && r.one_in(20) {
        return F::Expired;
    }
    if c == C::B && cfg!(feature = "geo") && r.one_in(5) {
        return F::Within(r.pick(&L).to_string(), bbox(r));
    }
    let fields = m.coll(c).plain();
    let name = r.pick(&fields).clone();
    test_of(r, m, c, &name)
}

fn order(r: &mut Rng, m: &Model, c: C) -> Vec<(String, bool)> {
    let mut fields = m.coll(c).plain();
    fields.push("id".to_string());
    let first = r.pick(&fields).clone();
    let mut keys = vec![(first.clone(), r.one_in(2))];
    if r.one_in(3) {
        let second = r.pick(&fields).clone();
        if second != first {
            keys.push((second, r.one_in(2)));
        }
    }
    keys
}

fn sets(r: &mut Rng, m: &Model, c: C) -> Vec<(String, Ex)> {
    let gs = groups(m, c);
    let mut used = Vec::new();
    let mut out = Vec::new();
    for _ in 0..1 + r.upto(2) {
        let gi = r.upto(gs.len());
        if used.contains(&gi) {
            continue;
        }
        used.push(gi);
        let g = &gs[gi];
        let ty = m.coll(c).field(&g[0]).map(|f| f.ty);
        if matches!(ty, Some(Ty::Int | Ty::Time)) && r.one_in(2) {
            let time = ty == Some(Ty::Time);
            let k = if time {
                r.range(-5, 5) * 1000
            } else {
                r.range(-3, 3)
            };
            let coalesce = !time && r.one_in(3);
            for f in g {
                let ex = if coalesce {
                    Ex::Coalesce(f.clone(), k)
                } else {
                    Ex::Plus(f.clone(), k)
                };
                out.push((f.clone(), ex));
            }
        } else {
            let v = value(r, m, c, &g[0]);
            for f in g {
                out.push((f.clone(), Ex::Lit(v.clone())));
            }
        }
    }
    out
}

fn upsert_sets(r: &mut Rng, m: &Model, c: C) -> Vec<(String, Ex)> {
    match c {
        C::A if r.one_in(2) => N
            .iter()
            .map(|f| (f.to_string(), Ex::Merge(f.to_string(), f.to_string())))
            .collect(),
        C::T if r.one_in(2) => vec![("m".to_string(), Ex::Merge("m".into(), "m".into()))],
        _ => sets(r, m, c),
    }
}

fn ret(r: &mut Rng, m: &Model, c: C) -> Option<Ret> {
    match r.below(8) {
        0 => Some(Ret::All),
        1 => {
            let fields = m.coll(c).plain();
            let mut names = vec![r.pick(&fields).clone()];
            let second = r.pick(&fields).clone();
            if second != names[0] {
                names.push(second);
            }
            Some(Ret::These(names))
        }
        _ => None,
    }
}

fn page(r: &mut Rng, m: &Model, c: C, share: u64) -> (Vec<(String, bool)>, Option<u64>) {
    match r.below(100) < share {
        true => (order(r, m, c), Some(1 + r.below(4))),
        false => (Vec::new(), r.one_in(6).then(|| 1 + r.below(4))),
    }
}

fn put(r: &mut Rng, m: &Model, c: C) -> St {
    let n = if r.one_in(4) { 2 + r.upto(2) } else { 1 };
    let docs = (0..n).map(|_| doc(r, m, c)).collect();
    let how = match r.below(20) {
        0..=10 => How::Put,
        11..=14 => How::Insert,
        15 | 16 => How::Absent,
        _ => How::Upsert(upsert_sets(r, m, c)),
    };
    St::Put {
        c,
        docs,
        how,
        require: None,
        ret: ret(r, m, c),
    }
}

fn set(r: &mut Rng, m: &Model, c: C) -> St {
    let filter = (!r.one_in(10)).then(|| filter(r, m, c, 0));
    let (order, limit) = page(r, m, c, 25);
    St::Set {
        c,
        set: sets(r, m, c),
        filter,
        order,
        limit,
        require: None,
        ret: ret(r, m, c),
    }
}

fn del(r: &mut Rng, m: &Model, c: C) -> St {
    // Narrow mostly, so that a collection keeps rows to read.
    let filter = match r.below(10) {
        0..=3 => id_filter(r, m, c),
        4 | 5 => {
            let fields = m.coll(c).plain();
            let name = r.pick(&fields).clone();
            F::Cmp(name.clone(), Op::Eq, drawn(r, m, c, &name))
        }
        _ => filter(r, m, c, 0),
    };
    let (order, limit) = page(r, m, c, 30);
    St::Del {
        c,
        filter: Some(filter),
        order,
        limit,
        require: None,
        ret: ret(r, m, c),
    }
}

/// `st` made to require the count it writes -- or, a third of the time,
/// one more, so that it is refused.
fn with_require(r: &mut Rng, m: &Model, mut st: St) -> St {
    if !r.one_in(8) {
        return st;
    }
    let n = match m.clone().run(&st) {
        Ok(Want::Count(n)) => n,
        Ok(Want::Rows(_, rows)) => rows.len(),
        _ => return st,
    };
    let want = if r.one_in(3) { n + 1 } else { n };
    if let Some(require) = st.require_mut() {
        *require = Some(want as u64);
    }
    st
}

fn write(r: &mut Rng, m: &Model) -> St {
    let c = *r.pick(&[C::A, C::A, C::B, C::T]);
    let rows = m.visible(c).len();
    // Some 20 to 40 rows a collection: enough for an index's buckets and
    // pages to hold several, few enough to read whole each step.
    let (puts, dels) = match rows {
        0..=15 => (70, 10),
        16..=40 => (50, 15),
        _ => (25, 40),
    };
    let x = r.below(100);
    let st = if x < puts {
        put(r, m, c)
    } else if x < puts + dels {
        del(r, m, c)
    } else {
        set(r, m, c)
    };
    with_require(r, m, st)
}

/// A `get` guarding a block: it requires the rows it finds, or one more.
fn guard(r: &mut Rng, m: &Model, wrong: bool) -> St {
    let c = *r.pick(&COLLECTIONS);
    let filter = Some(filter(r, m, c, 0));
    let n = m
        .pick(c, filter.as_ref(), &[], None, 0)
        .map_or(0, |ids| ids.len()) as u64;
    St::Get {
        c,
        filter,
        order: Vec::new(),
        limit: None,
        offset: 0,
        require: Some(n + u64::from(wrong)),
    }
}

/// A statement that is refused: a taken id or unique value, a type, an
/// overflow, a count not met.
fn failing(r: &mut Rng, m: &Model) -> St {
    let alive = m.visible(C::A);
    let a = m.coll(C::A);
    let row = |id: &u64| &a.rows[id];
    match r.below(5) {
        0 if !alive.is_empty() => St::Put {
            c: C::A,
            docs: vec![Doc {
                id: Some(*r.pick(&alive)),
                values: Vec::new(),
            }],
            how: How::Insert,
            require: None,
            ret: None,
        },
        1 if !alive.is_empty() => St::Set {
            c: C::A,
            set: N
                .iter()
                .map(|f| (f.to_string(), Ex::Lit(Value::Text("x".into()))))
                .collect(),
            filter: Some(F::Id(Op::Eq, *r.pick(&alive))),
            order: Vec::new(),
            limit: None,
            require: None,
            ret: None,
        },
        2 => {
            let held: Vec<&Value> = alive.iter().filter_map(|id| row(id).get("u")).collect();
            if held.is_empty() {
                return guard(r, m, true);
            }
            let v = (*r.pick(&held)).clone();
            St::Put {
                c: C::A,
                docs: vec![Doc {
                    id: None,
                    values: U.iter().map(|f| (f.to_string(), v.clone())).collect(),
                }],
                how: How::Put,
                require: None,
                ret: None,
            }
        }
        3 => {
            let big: Vec<u64> = alive
                .iter()
                .copied()
                .filter(|id| matches!(row(id).get("n"), Some(Value::Int(n)) if *n > 0))
                .collect();
            if big.is_empty() {
                return guard(r, m, true);
            }
            St::Set {
                c: C::A,
                set: N
                    .iter()
                    .map(|f| (f.to_string(), Ex::Plus(f.to_string(), i64::MAX)))
                    .collect(),
                filter: Some(F::Id(Op::Eq, *r.pick(&big))),
                order: Vec::new(),
                limit: None,
                require: None,
                ret: None,
            }
        }
        _ => guard(r, m, true),
    }
}

/// A change of a collection: a field added, dropped or renamed, an index
/// made or taken off, `t`'s expiry -- or one refused.
fn schema(r: &mut Rng, m: &Model) -> St {
    let a = m.coll(C::A);
    let extras: Vec<&Field> = a
        .fields
        .iter()
        .filter(|f| f.name.starts_with('x'))
        .collect();
    let free: Vec<&str> = EXTRA.iter().copied().filter(|x| !a.has(x)).collect();
    let bare: Vec<&Field> = extras
        .iter()
        .copied()
        .filter(|f| f.ix == Ix::None)
        .collect();
    let pq = vec!["p".to_string(), "q".to_string()];
    // Each change the collection allows, as many times as it weighs: a
    // field added more often while few are, dropped while all are.
    let mut can: Vec<u8> = vec![7, 9, 10];
    if !free.is_empty() {
        can.extend_from_slice(if extras.len() < 2 { &[0; 6] } else { &[0; 2] });
    }
    if !extras.is_empty() {
        can.extend_from_slice(if free.is_empty() { &[3; 5] } else { &[3; 2] });
    }
    if !extras.is_empty() && !free.is_empty() {
        can.extend_from_slice(&[4; 2]);
    }
    if !bare.is_empty() {
        can.extend_from_slice(&[5; 3]);
    }
    if extras.len() >= 2 {
        can.extend_from_slice(&[8; 2]);
    }
    match *r.pick(&can) {
        0 => St::Add {
            c: C::A,
            name: r.pick(&free).to_string(),
            ty: *r.pick(&[Ty::Int, Ty::Text]),
            ix: *r.pick(&[Ix::None, Ix::None, Ix::Hash, Ix::Sorted, Ix::Unique]),
        },
        3 => St::Drop {
            c: C::A,
            name: r.pick(&extras).name.clone(),
        },
        4 => St::Rename {
            c: C::A,
            from: r.pick(&extras).name.clone(),
            to: r.pick(&free).to_string(),
        },
        5 => St::Index {
            c: C::A,
            name: r.pick(&bare).name.clone(),
            ix: *r.pick(&[Ix::Hash, Ix::Sorted, Ix::Unique]),
        },
        7 if a.composites.iter().any(|k| k.keys == pq) => St::Decompose { c: C::A, keys: pq },
        7 => St::Compose {
            c: C::A,
            keys: pq,
            unique: r.one_in(5),
        },
        8 => {
            let x = r.upto(extras.len());
            let y = (x + 1 + r.upto(extras.len() - 1)) % extras.len();
            let keys = vec![extras[x].name.clone(), extras[y].name.clone()];
            match a.composites.iter().any(|k| k.keys == keys) {
                true => St::Decompose { c: C::A, keys },
                false => St::Compose {
                    c: C::A,
                    keys,
                    unique: r.one_in(3),
                },
            }
        }
        9 => St::Expiry {
            ttl: *r.pick(&[Some(5_000), Some(TTL), Some(20_000), None]),
        },
        // Refused: a field taken, or one indexed already.
        _ => match r.one_in(2) {
            true => St::Add {
                c: C::A,
                name: "n".to_string(),
                ty: Ty::Int,
                ix: Ix::None,
            },
            false => St::Index {
                c: C::A,
                name: r.pick(&N[1..]).to_string(),
                ix: Ix::Hash,
            },
        },
    }
}

/// Two to five statements as one block, each drawn against the ones
/// before it -- a third of them with one refused among them.
fn block(r: &mut Rng, m: &Model) -> Vec<St> {
    let mut scratch = m.clone();
    let n = 2 + r.upto(4);
    let refused = r.one_in(3).then(|| r.upto(n));
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let st = if refused == Some(i) {
            failing(r, &scratch)
        } else if r.one_in(12) {
            schema(r, &scratch)
        } else if r.one_in(10) {
            guard(r, &scratch, false)
        } else {
            write(r, &scratch)
        };
        let _ = scratch.run(&st);
        out.push(st);
    }
    out
}

/// A read over one field of twins, its first the scan's.
fn twins(r: &mut Rng, m: &Model, c: C, twins: &[&'static str]) -> Read {
    let scan = twins[0];
    let filter = match r.below(4) {
        0 => None,
        1 => Some(F::And(
            Box::new(test_of(r, m, c, scan)),
            Box::new(test_of(r, m, c, scan)),
        )),
        _ => Some(test_of(r, m, c, scan)),
    };
    let (order, limit, offset) = match r.below(3) {
        0 => (Vec::new(), None, 0),
        _ => (
            vec![(scan.to_string(), r.one_in(2))],
            (!r.one_in(4)).then(|| 1 + r.below(6)),
            if r.one_in(3) { r.below(4) } else { 0 },
        ),
    };
    Read::Twins {
        c,
        filter,
        order,
        limit,
        offset,
        with: twins[1..].iter().map(|t| vec![(scan, *t)]).collect(),
    }
}

fn read(r: &mut Rng, m: &Model) -> Read {
    match r.below(12) {
        0..=2 => twins(r, m, C::A, &N),
        3 => twins(r, m, C::A, &U),
        4 if m.coll(C::A).composites.iter().any(|k| k.keys == ["p", "q"]) => {
            // An index over several fields: the first part equal, a range
            // or an order over the second.
            let p = drawn(r, m, C::A, "p2");
            let filter = match r.below(3) {
                0 => F::Cmp("p2".into(), Op::Eq, p),
                1 => F::And(
                    Box::new(F::Cmp("p2".into(), Op::Eq, p)),
                    Box::new(test_of(r, m, C::A, "q2")),
                ),
                _ => test_of(r, m, C::A, "p2"),
            };
            let desc = r.one_in(2);
            Read::Twins {
                c: C::A,
                filter: Some(filter),
                order: vec![("p2".into(), desc), ("q2".into(), desc)],
                limit: r.one_in(2).then(|| 1 + r.below(5)),
                offset: 0,
                with: vec![vec![("p2", "p"), ("q2", "q")]],
            }
        }
        5 | 6 => twins(r, m, C::T, &AT),
        7 => match r.below(4) {
            0 if cfg!(feature = "text") => Read::Match(r.pick(&WORDS)),
            1 => Read::Near(*r.pick(&VECTORS)),
            2 if cfg!(feature = "geo") => Read::Twins {
                c: C::B,
                filter: Some(F::Within("loc2".into(), bbox(r))),
                order: Vec::new(),
                limit: None,
                offset: 0,
                with: vec![vec![("loc2", "loc")]],
            },
            _ => Read::Sparse(
                drawn(r, m, C::B, "sp"),
                (!r.one_in(3)).then(|| 1 + r.below(4)),
            ),
        },
        8 => {
            let c = *r.pick(&COLLECTIONS);
            Read::Count {
                c,
                filter: r
                    .one_in(5)
                    .then(|| filter(r, m, c, 0))
                    .or_else(|| Some(filter(r, m, c, 0))),
            }
        }
        _ => {
            // Any field, the ones `alter` made among them, the model alone.
            let c = *r.pick(&[C::A, C::A, C::T]);
            let (order, limit) = page(r, m, c, 50);
            Read::Twins {
                c,
                filter: Some(filter(r, m, c, 0)),
                order,
                limit,
                offset: 0,
                with: Vec::new(),
            }
        }
    }
}

fn step(r: &mut Rng, m: &Model) -> Step {
    match r.below(1000) {
        0..=379 => Step::Run(write(r, m)),
        380..=409 => Step::Run(failing(r, m)),
        410..=499 => Step::Block(block(r, m)),
        500..=559 => Step::Begin(block(r, m), !r.one_in(3)),
        560..=589 => Step::Run(schema(r, m)),
        590..=604 => Step::Compact,
        605..=614 => Step::Checkpoint,
        615..=674 => {
            let to = match r.one_in(8) {
                true => m.clock - r.range(0, 5_000),
                false => m.clock + r.range(0, 8_000),
            };
            Step::Clock(to)
        }
        675..=684 => Step::Sweep,
        685..=709 => Step::Reopen,
        710..=744 => {
            let cut = match r.below(4) {
                0 => Cut::Clean,
                1 => Cut::Zeros(1 + r.upto(600)),
                2 => Cut::Torn(r.next() as usize),
                _ => Cut::Zeroed(r.next() as usize),
            };
            Step::Crash(r.upto(4), cut)
        }
        _ => Step::Check((0..2 + r.upto(3)).map(|_| read(r, m)).collect()),
    }
}

// ----------------------------------------------------------- the engine

/// A file in memory: appends go to its end, a rewrite replaces it.
#[derive(Clone)]
struct Tap {
    file: Arc<Mutex<Vec<u8>>>,
    rewrites: Arc<AtomicUsize>,
}

impl Sink for Tap {
    fn append(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.file.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }
    fn rewrite(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        *self.file.lock().unwrap() = bytes.to_vec();
        self.rewrites.fetch_add(1, Atomic::Relaxed);
        Ok(())
    }
}

/// How a step diverged from the model: `what` names the check, which a
/// shrunk run must fail the same way.
#[derive(Debug)]
struct Divergence {
    what: &'static str,
    detail: String,
}

enum Halt {
    Diverged(Divergence),
    /// The model cannot answer for a step -- a shrunk run's, which then
    /// does not count as diverging.
    Unknown(String),
}

fn diverged<T>(what: &'static str, detail: impl Into<String>) -> Result<T, Halt> {
    Err(Halt::Diverged(Divergence {
        what,
        detail: detail.into(),
    }))
}

fn uvarint(b: &[u8], p: &mut usize) -> Option<u64> {
    let (mut v, mut shift) = (0u64, 0);
    loop {
        let byte = *b.get(*p)?;
        *p += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

/// Where each whole record from `from` on ends: `[kind][collection][length]
/// [body]`, the length always read (docs/design/storage.md#file-format).
fn record_ends(b: &[u8], from: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut pos = from;
    while pos < b.len() {
        let mut p = pos + 1;
        let (Some(_), Some(len)) = (uvarint(b, &mut p), uvarint(b, &mut p)) else {
            break;
        };
        p += len as usize;
        if p > b.len() {
            break;
        }
        out.push(p);
        pos = p;
    }
    out
}

/// How many of the steps since the image `World::history` keeps: what a
/// cut back to one of their ends must leave.
const KEPT: usize = 8;

/// A database over a file in memory, and its model.
struct World {
    db: Database,
    tap: Tap,
    m: Model,
    /// The file's length at the image's end and at each step's end since,
    /// and what the model held then.
    history: Vec<(usize, Model)>,
    rewrites: usize,
}

impl World {
    fn new() -> World {
        let tap = Tap {
            file: Arc::new(Mutex::new(fennec_core::engine::MAGIC.to_vec())),
            rewrites: Arc::default(),
        };
        let mut db = Database::with_sink(Box::new(tap.clone()));
        let mut m = Model::new();
        db.set_clock(Some(m.clock));
        for coll in &m.cs {
            let text = create(coll);
            let stmt = fennec_ql::parse_one(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            db.execute(&stmt).unwrap_or_else(|e| panic!("{text}: {e}"));
        }
        m.writes = db.change_seq();
        // The collections are in the image, so that no crash goes back past
        // them.
        db.checkpoint().expect("checkpoint");
        let len = tap.file.lock().unwrap().len();
        World {
            db,
            rewrites: tap.rewrites.load(Atomic::Relaxed),
            tap,
            history: vec![(len, m.clone())],
            m,
        }
    }

    fn file(&self) -> Vec<u8> {
        self.tap.file.lock().unwrap().clone()
    }

    fn len(&self) -> usize {
        self.tap.file.lock().unwrap().len()
    }

    fn parse(&self, text: &str) -> Result<Statement, Halt> {
        fennec_ql::parse_one_for(&self.db, text)
            .or_else(|e| diverged("parse", format!("{text}: {e}")))
    }

    fn exec(&mut self, text: &str) -> Result<fennec_core::error::Result<Response>, Halt> {
        let stmt = self.parse(text)?;
        Ok(self.db.execute_with(&stmt, &[]))
    }

    fn query(&self, text: &str) -> Result<fennec_core::error::Result<Response>, Halt> {
        let stmt = self.parse(text)?;
        Ok(self.db.query(&stmt, &[]))
    }

    /// The ids, and the scores, a read answered.
    fn hits(&self, text: &str) -> Result<Vec<(u64, Option<f32>)>, Halt> {
        match self.query(text)? {
            Ok(Response::Rows(rs)) => Ok(rs.rows.iter().map(|r| (r.id, r.score)).collect()),
            other => diverged("read", format!("{text}: {other:?}")),
        }
    }

    fn step(&mut self, step: &Step) -> Result<(), Halt> {
        let before = self.len();
        match step {
            Step::Run(st) => {
                let want = self.m.run(st).map_err(Halt::Unknown)?;
                let text = st.text();
                let got = self.exec(&text)?;
                held(&text, &want, &got)?;
            }
            Step::Block(sts) => self.block(sts)?,
            Step::Begin(sts, commit) => self.begin(sts, *commit)?,
            Step::Compact => {
                let got = self.exec("compact")?;
                held("compact", &Want::Done, &got)?;
            }
            Step::Checkpoint => {
                if let Err(e) = self.db.checkpoint() {
                    return diverged("checkpoint", e.to_string());
                }
            }
            Step::Clock(t) => {
                self.db.set_clock(Some(*t));
                self.m.clock = *t;
            }
            Step::Sweep => self.sweep()?,
            Step::Reopen => {
                let bytes = self.file();
                self.reopen(&bytes, bytes.len())?;
            }
            Step::Crash(back, cut) => self.crash(*back, *cut)?,
            Step::Check(reads) => {
                for r in reads {
                    self.read(r)?;
                }
                return Ok(());
            }
        }
        if !matches!(step, Step::Reopen | Step::Crash(..) | Step::Clock(_)) {
            self.landed(before)?;
        }
        // The counter a replica and `/_changes` go by: moved by each write
        // that landed, as many as it wrote, and taken back by a crash and
        // an open to the writes the file holds -- never a change number
        // reused.
        if self.db.change_seq() != self.m.writes {
            return diverged(
                "counter",
                format!(
                    "the engine's change counter stands at {}, the model's at {}",
                    self.db.change_seq(),
                    self.m.writes
                ),
            );
        }
        self.state(step)
    }

    /// The history kept up after a step: a rewrite starts it again, and a
    /// step that wrote appended one record, whole.
    fn landed(&mut self, before: usize) -> Result<(), Halt> {
        let rewrites = self.tap.rewrites.load(Atomic::Relaxed);
        let len = self.len();
        if rewrites != self.rewrites {
            self.rewrites = rewrites;
            self.history = vec![(len, self.m.clone())];
        } else if len != before {
            let ends = record_ends(&self.file(), before);
            if ends != [len] {
                return diverged(
                    "one write, one record",
                    format!("the step appended {before}..{len}, records ending at {ends:?}"),
                );
            }
            self.history.push((len, self.m.clone()));
            if self.history.len() > KEPT {
                self.history.remove(1);
            }
        }
        Ok(())
    }

    fn block(&mut self, sts: &[St]) -> Result<(), Halt> {
        let texts: Vec<String> = sts.iter().map(St::text).collect();
        let parsed = texts
            .iter()
            .map(|t| self.parse(t))
            .collect::<Result<Vec<_>, _>>()?;
        let mut next = self.m.clone();
        let mut wants = Vec::new();
        let mut stopped = None;
        for (i, st) in sts.iter().enumerate() {
            match next.run(st).map_err(Halt::Unknown)? {
                Want::Fail(k) => {
                    stopped = Some((i, k));
                    break;
                }
                w => wants.push(w),
            }
        }
        let refs: Vec<(&Statement, &[Value])> = parsed.iter().map(|s| (s, &[][..])).collect();
        let got = self.db.execute_block(&refs);
        match (stopped, got) {
            (None, Ok(answers)) => {
                for ((text, want), got) in texts.iter().zip(&wants).zip(answers) {
                    held(text, want, &Ok(got))?;
                }
                self.m = next;
                Ok(())
            }
            (Some((i, k)), Err((j, e))) if i == j && k.takes(&e) => Ok(()),
            (stopped, got) => diverged(
                "block",
                format!(
                    "[block] {}\n  model: stops at {stopped:?}\n  engine: {got:?}",
                    texts.join("; ")
                ),
            ),
        }
    }

    fn begin(&mut self, sts: &[St], commit: bool) -> Result<(), Halt> {
        if let Err(e) = self.db.begin() {
            return diverged("begin", e.to_string());
        }
        let mut next = self.m.clone();
        let mut whole = true;
        for st in sts {
            let text = st.text();
            let outcome = next
                .run(st)
                .map_err(Halt::Unknown)
                .and_then(|want| Ok((self.exec(&text)?, want)))
                .and_then(|(got, want)| held(&text, &want, &got).map(|_| want));
            match outcome {
                Ok(Want::Fail(_)) => {
                    whole = false;
                    break;
                }
                Ok(_) => {}
                Err(h) => {
                    self.db.rollback();
                    return Err(h);
                }
            }
        }
        if commit && whole {
            if let Err(e) = self.db.commit() {
                return diverged("commit", e.to_string());
            }
            self.m = next;
        } else {
            self.db.rollback();
        }
        Ok(())
    }

    fn sweep(&mut self) -> Result<(), Halt> {
        let want = self.m.past_their_time(C::T);
        let now = self.m.clock;
        let ids = match self.db.expired("t", now, 100_000) {
            Ok(ids) => ids,
            Err(e) => return diverged("sweep", e.to_string()),
        };
        if ids != want {
            return diverged(
                "sweep",
                format!("past their time: model {want:?}, engine {ids:?}"),
            );
        }
        match self.db.sweep("t", now, &ids) {
            Ok(n) if n == want.len() => {}
            other => return diverged("sweep", format!("swept {other:?} of {want:?}")),
        }
        self.m.writes += want.len() as u64;
        let coll = self.m.coll_mut(C::T);
        for id in want {
            coll.rows.remove(&id);
        }
        Ok(())
    }

    /// The database opened again from `bytes`, which must be whole up to
    /// `whole`, and the file cut there, as `fs::open` cuts it.
    fn reopen(&mut self, bytes: &[u8], whole: usize) -> Result<(), Halt> {
        // The old one goes first, as a process that ended.
        self.db = Database::new();
        let mut db = Database::with_sink(Box::new(self.tap.clone()));
        let took = match db.load(bytes) {
            Ok(n) => n,
            Err(e) => return diverged("open", format!("{e}, the file {} bytes", bytes.len())),
        };
        if took != whole {
            return diverged(
                "open",
                format!(
                    "took {took} bytes of {}, where {whole} are whole",
                    bytes.len()
                ),
            );
        }
        {
            let mut file = self.tap.file.lock().unwrap();
            *file = bytes.to_vec();
            file.truncate(took);
        }
        db.set_clock(Some(self.m.clock));
        self.db = db;
        Ok(())
    }

    fn crash(&mut self, back: usize, cut: Cut) -> Result<(), Halt> {
        let mut bytes = self.file();
        let image = self.history[0].0;
        let ends = record_ends(&bytes, image);
        let last = ends.last().copied().unwrap_or(image);
        if last != bytes.len() || self.history.last().map(|h| h.0) != Some(bytes.len()) {
            return diverged(
                "records",
                format!(
                    "the file is {} bytes, its records end at {ends:?}, the steps at {:?}",
                    bytes.len(),
                    self.history.iter().map(|h| h.0).collect::<Vec<_>>()
                ),
            );
        }
        // The boundary a step ended at, `back` steps back, the image's at
        // most.
        let k = back.min(self.history.len() - 1);
        let (at, m) = {
            let (at, m) = &self.history[self.history.len() - 1 - k];
            (*at, m.clone())
        };
        let next = ends.iter().copied().find(|e| *e > at);
        match (cut, next) {
            (Cut::Clean, _) => bytes.truncate(at),
            (Cut::Zeros(z), _) => {
                bytes.truncate(at);
                bytes.resize(at + z, 0);
            }
            (Cut::Torn(keep), Some(end)) => bytes.truncate(at + 1 + keep % (end - at - 1)),
            (Cut::Zeroed(from), Some(end)) => {
                // From a byte of the record that is not zero already:
                // zeros over zeros to its end leave it whole, and an open
                // rightly keeps it (seed 1044 of 20 000 steps, its last
                // bytes 0).
                let last = (at..end).rev().find(|&i| bytes[i] != 0).unwrap_or(at);
                bytes[(at + from % (end - at)).min(last)..].fill(0);
            }
            // Nothing after the boundary to tear: zeros after it.
            (Cut::Torn(z) | Cut::Zeroed(z), None) => bytes.resize(at + 1 + z % 64, 0),
        }
        self.reopen(&bytes, at)?;
        self.history.retain(|(len, _)| *len <= at);
        // The rows go back with the file; the clock is the world's.
        let clock = self.m.clock;
        self.m = m;
        self.m.clock = clock;
        Ok(())
    }

    /// Every row each collection a step may have changed holds -- the
    /// living through a read, those past their time through `expired()` --
    /// as the model holds them.
    fn state(&self, step: &Step) -> Result<(), Halt> {
        let touched: Vec<C> = match step {
            Step::Run(st) => vec![st.coll()],
            Step::Block(sts) | Step::Begin(sts, _) => sts.iter().map(St::coll).collect(),
            Step::Sweep => vec![C::T],
            _ => COLLECTIONS.to_vec(),
        };
        for c in COLLECTIONS.into_iter().filter(|c| touched.contains(c)) {
            let coll = self.m.coll(c);
            let columns = coll.columns();
            let rows = |ids: Vec<u64>| {
                ids.into_iter()
                    .map(|id| (id, coll.values(id, &coll.rows[&id], &columns)))
                    .collect()
            };
            let text = format!("get {}", c.name());
            let want = Want::Rows(columns.clone(), rows(self.m.visible(c)));
            held_as("state", &text, &want, &self.query(&text)?)?;
            if c == C::T {
                let text = "get t where expired()";
                let want = match coll.ttl.is_none() && !coll.rows.is_empty() {
                    false => Want::Rows(columns.clone(), rows(self.m.past_their_time(c))),
                    true => Want::Fail(Kind::Query),
                };
                held_as("state", text, &want, &self.query(text)?)?;
            }
        }
        Ok(())
    }

    fn read(&self, r: &Read) -> Result<(), Halt> {
        let model = |ids: Result<Vec<u64>, Out>| match ids {
            Ok(ids) => Ok(ids),
            Err(Out::Unknown(why)) => Err(Halt::Unknown(why)),
            Err(Out::Fail(k)) => Err(Halt::Unknown(format!("a read refused: {k:?}"))),
        };
        match r {
            Read::Twins {
                c,
                filter,
                order,
                limit,
                offset,
                with,
            } => {
                let want = model(self.m.pick(*c, filter.as_ref(), order, *limit, *offset))?;
                let mut forms = vec![(filter.clone(), order.clone())];
                for renames in with {
                    let mut f = filter.clone();
                    let mut o = order.clone();
                    for (from, to) in renames {
                        f = f.map(|f| f.renamed(from, to));
                        for (k, _) in &mut o {
                            if k == from {
                                *k = to.to_string();
                            }
                        }
                    }
                    forms.push((f, o));
                }
                for (f, o) in forms {
                    let text = format!(
                        "get {} select id{}",
                        c.name(),
                        tail(&f, &o, *limit, *offset, None)
                    );
                    let got: Vec<u64> = self.hits(&text)?.into_iter().map(|h| h.0).collect();
                    if got != want {
                        return diverged(
                            "read",
                            format!("{text}\n  model: {want:?}\n  engine: {got:?}"),
                        );
                    }
                }
            }
            Read::Count { c, filter } => {
                let want = model(self.m.pick(*c, filter.as_ref(), &[], None, 0))?.len() as i64;
                let text = format!("get {}{} count", c.name(), tail(filter, &[], None, 0, None));
                match self.query(&text)? {
                    Ok(Response::Rows(rs))
                        if rs.rows.len() == 1 && rs.rows[0].values == [Value::Int(want)] => {}
                    got => {
                        return diverged(
                            "count",
                            format!("{text}\n  model: {want}\n  engine: {got:?}"),
                        )
                    }
                }
            }
            Read::Match(word) => {
                let b = self.m.coll(C::B);
                let want: Vec<u64> = self
                    .m
                    .visible(C::B)
                    .into_iter()
                    .filter(|id| match b.rows[id].get("body") {
                        Some(Value::Text(t)) => t.split(' ').any(|w| w == *word),
                        _ => false,
                    })
                    .collect();
                let text = format!("get b select id match body \"{word}\" order id");
                let got: Vec<u64> = self.hits(&text)?.into_iter().map(|h| h.0).collect();
                if got != want {
                    return diverged(
                        "match",
                        format!("{text}\n  model: {want:?}\n  engine: {got:?}"),
                    );
                }
            }
            Read::Near(qv) => {
                let b = self.m.coll(C::B);
                let want: Vec<u64> = self
                    .m
                    .visible(C::B)
                    .into_iter()
                    .filter(|id| b.rows[id].contains_key("v"))
                    .collect();
                let q = lit(&Value::Vector(qv.to_vec()));
                let mut graph = self.hits(&format!("get b select id near v {q}"))?;
                let mut exact = self.hits(&format!("get b select id near v {q} exact"))?;
                graph.sort_by_key(|h| h.0);
                exact.sort_by_key(|h| h.0);
                let ids: Vec<u64> = graph.iter().map(|h| h.0).collect();
                if ids != want || bits(&graph) != bits(&exact) {
                    return diverged(
                        "near",
                        format!(
                            "near v {q}\n  model: {want:?}\n  graph: {graph:?}\n  exact: {exact:?}"
                        ),
                    );
                }
                // Each score is the row's own vector's: a vector the graph
                // still holds for a row written since, which `exact` reads
                // too, answers the row with another's score.
                for (id, score) in &exact {
                    let Some(Value::Vector(v)) = b.rows[id].get("v") else {
                        continue;
                    };
                    let cos = cosine(qv, v);
                    if score.is_none_or(|s| (f64::from(s) - cos).abs() > 1e-5) {
                        return diverged(
                            "near",
                            format!(
                                "near v {q}: row {id} scored {score:?}, its vector {v:?} {cos}"
                            ),
                        );
                    }
                }
            }
            Read::Sparse(q, limit) => {
                let page = limit.map(|l| format!(" limit {l}")).unwrap_or_default();
                let text = format!("get b select id near sp {}{page}", lit(q));
                let index = self.hits(&text)?;
                let exact =
                    self.hits(&format!("get b select id near sp {} exact{page}", lit(q)))?;
                if bits(&index) != bits(&exact) {
                    return diverged(
                        "sparse",
                        format!("{text}\n  index: {index:?}\n  exact: {exact:?}"),
                    );
                }
                if limit.is_none() {
                    let b = self.m.coll(C::B);
                    let want: Vec<u64> = self
                        .m
                        .visible(C::B)
                        .into_iter()
                        .filter(|id| b.rows[id].get("sp").is_some_and(|s| overlaps(s, q)))
                        .collect();
                    let mut ids: Vec<u64> = index.iter().map(|h| h.0).collect();
                    ids.sort_unstable();
                    if ids != want {
                        return diverged(
                            "sparse",
                            format!("{text}\n  model: {want:?}\n  engine: {ids:?}"),
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

/// The cosine of two vectors, in `f64`: what `near` over a cosine graph
/// scores, within `f32`'s error.
fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum();
    let norm = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    dot / (norm(a) * norm(b))
}

fn bits(hits: &[(u64, Option<f32>)]) -> Vec<(u64, Option<u32>)> {
    hits.iter()
        .map(|(id, s)| (*id, s.map(f32::to_bits)))
        .collect()
}

fn create(coll: &Coll) -> String {
    let mut s = format!("create collection {} (", coll.name);
    for (i, f) in coll.fields.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        let _ = write!(s, "{} {}{}", f.name, f.ty.word(), f.ix.word());
        if f.ix == Ix::Ttl {
            let _ = write!(s, " @ttl({}s)", coll.ttl.expect("a ttl") / 1000);
        }
    }
    for k in &coll.composites {
        let kind = if k.unique { "unique" } else { "sorted" };
        let _ = write!(s, ", @{kind}({})", k.keys.join(", "));
    }
    s.push(')');
    s
}

fn held(text: &str, want: &Want, got: &fennec_core::error::Result<Response>) -> Result<(), Halt> {
    held_as("answer", text, want, got)
}

fn held_as(
    what: &'static str,
    text: &str,
    want: &Want,
    got: &fennec_core::error::Result<Response>,
) -> Result<(), Halt> {
    let same = match (want, got) {
        (Want::Count(n), Ok(Response::Affected(m))) => n == m,
        (Want::Rows(columns, rows), Ok(Response::Rows(rs))) => {
            *columns == rs.columns
                && rows.len() == rs.rows.len()
                && rows
                    .iter()
                    .zip(&rs.rows)
                    .all(|((id, values), r)| *id == r.id && *values == r.values)
        }
        (Want::Done, Ok(Response::Ok(_))) => true,
        (Want::Fail(k), Err(e)) => k.takes(e),
        _ => false,
    };
    if same {
        return Ok(());
    }
    let got = match got {
        Ok(Response::Rows(rs)) => format!(
            "{:?} {:?}",
            rs.columns,
            rs.rows
                .iter()
                .map(|r| (r.id, &r.values))
                .collect::<Vec<_>>()
        ),
        other => format!("{other:?}"),
    };
    diverged(what, format!("{text}\n  model: {want:?}\n  engine: {got}"))
}

// ------------------------------------------------------------ the runs

fn knob(name: &str, default: u64) -> u64 {
    match std::env::var(name) {
        Ok(v) => v
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{name}={v}: not a number")),
        Err(_) => default,
    }
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    match (p.downcast_ref::<&str>(), p.downcast_ref::<String>()) {
        (Some(s), _) => s.to_string(),
        (_, Some(s)) => s.clone(),
        _ => "a panic".to_string(),
    }
}

/// `steps` run from a database just made: where they diverged, a panic
/// among the ways.
fn replay(steps: &[Step]) -> Result<(), (usize, Halt)> {
    let at = Cell::new(0);
    let out = catch_unwind(AssertUnwindSafe(|| {
        let mut w = World::new();
        for (i, s) in steps.iter().enumerate() {
            at.set(i);
            w.step(s).map_err(|h| (i, h))?;
        }
        Ok(())
    }));
    out.unwrap_or_else(|p| {
        Err((
            at.get(),
            Halt::Diverged(Divergence {
                what: "panic",
                detail: panic_text(&*p),
            }),
        ))
    })
}

/// The fewest of `steps` that still diverge as `what` -- runs of them
/// taken out while it holds, then a block's statements one at a time --
/// as far as `budget` allows.
fn shrink(mut steps: Vec<Step>, what: &str, budget: Duration) -> Vec<Step> {
    let began = Instant::now();
    // Where a run diverges as `what`: everything after it goes too.
    let fails = |cand: &[Step]| match replay(cand) {
        Err((i, Halt::Diverged(d))) if d.what == what => Some(i),
        _ => None,
    };
    let mut chunk = (steps.len() / 2).max(1);
    loop {
        let mut i = 0;
        let mut shrank = false;
        while i < steps.len() {
            if began.elapsed() > budget {
                return steps;
            }
            let end = (i + chunk).min(steps.len());
            let mut cand: Vec<Step> = steps[..i].to_vec();
            cand.extend_from_slice(&steps[end..]);
            match fails(&cand) {
                Some(at) => {
                    cand.truncate(at + 1);
                    steps = cand;
                    shrank = true;
                }
                None => i = end,
            }
        }
        if chunk == 1 && !shrank {
            break;
        }
        if !shrank {
            chunk = (chunk / 2).max(1);
        }
    }
    let mut i = 0;
    while i < steps.len() {
        let len = match &steps[i] {
            Step::Block(sts) | Step::Begin(sts, _) => sts.len(),
            Step::Check(reads) => reads.len(),
            _ => 0,
        };
        let mut j = 0;
        let mut left = len;
        while j < left && left > 1 && began.elapsed() < budget {
            let mut cand = steps.clone();
            match &mut cand[i] {
                Step::Block(sts) | Step::Begin(sts, _) => {
                    sts.remove(j);
                }
                Step::Check(reads) => {
                    reads.remove(j);
                }
                _ => unreachable!(),
            }
            match fails(&cand) {
                Some(at) => {
                    cand.truncate(at + 1);
                    steps = cand;
                    left -= 1;
                }
                None => j += 1,
            }
        }
        i += 1;
    }
    steps
}

/// What a divergence prints: the seed and step that replay it whole, and
/// the steps shrunk.
fn report(seed: u64, steps: &[Step], d: &Divergence) -> String {
    let budget = Duration::from_secs(knob("FENNEC_MODEL_SHRINK_SECS", 120));
    let shrunk = shrink(steps.to_vec(), d.what, budget);
    let mut s = format!(
        "model: seed {seed} diverged at step {} ({}):\n  {}\n",
        steps.len() - 1,
        d.what,
        steps.last().map(Step::text).unwrap_or_default()
    );
    let _ = writeln!(s, "  {}", d.detail);
    // The build it ran in: every index, or none (`tests/features.rs`).
    let build = match cfg!(feature = "vector") {
        true => "--test all",
        false => "--no-default-features --features std-fs,language --test features",
    };
    let _ = writeln!(
        s,
        "  the whole run again: FENNEC_MODEL_SEED={seed} FENNEC_MODEL_OPS={} \
         cargo test -p fennec-core --release {build} model::",
        steps.len()
    );
    let _ = writeln!(
        s,
        "  shrunk to {} of its {} steps, after the collections are made:",
        shrunk.len(),
        steps.len()
    );
    for coll in &Model::new().cs {
        let _ = writeln!(s, "    {}", create(coll));
    }
    for st in &shrunk {
        let _ = writeln!(s, "    {}", st.text());
    }
    if let Err((i, Halt::Diverged(d))) = replay(&shrunk) {
        let _ = writeln!(s, "  where step {i} diverges: {}", d.detail);
    }
    s
}

/// One seed's run of `n` steps, each drawn against the model as it stands.
fn run_seed(seed: u64, n: usize) -> Result<(), String> {
    let mut r = Rng::seeded(seed);
    let mut steps: Vec<Step> = Vec::with_capacity(n);
    let at = Cell::new(0);
    let out = catch_unwind(AssertUnwindSafe(|| {
        let mut w = World::new();
        for i in 0..n {
            at.set(i);
            steps.push(step(&mut r, &w.m));
            w.step(&steps[i]).map_err(|h| (i, h))?;
        }
        Ok(())
    }));
    let (i, halt) = match out {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(e)) => e,
        Err(p) => (
            at.get(),
            Halt::Diverged(Divergence {
                what: "panic",
                detail: panic_text(&*p),
            }),
        ),
    };
    steps.truncate(i + 1);
    match halt {
        Halt::Unknown(why) => Err(format!(
            "model: seed {seed}, step {i}: the model cannot answer for a step the generator drew: \
             {why}\n  {}",
            steps.last().map(Step::text).unwrap_or_default()
        )),
        Halt::Diverged(d) => Err(report(seed, &steps, &d)),
    }
}

/// `FENNEC_MODEL_SEEDS` seeds from `FENNEC_MODEL_FIRST` -- or the one
/// `FENNEC_MODEL_SEED` names -- of `FENNEC_MODEL_OPS` steps each, run on
/// `FENNEC_MODEL_THREADS` threads, the first divergence shrunk and told.
#[test]
fn the_engine_answers_as_its_model() {
    fennec_ql::install();
    let ops = knob("FENNEC_MODEL_OPS", 1_000) as usize;
    let seeds: Vec<u64> = match std::env::var("FENNEC_MODEL_SEED") {
        Ok(s) => vec![s.trim().parse().expect("FENNEC_MODEL_SEED is a number")],
        Err(_) => {
            let first = knob("FENNEC_MODEL_FIRST", 0);
            (first..first + knob("FENNEC_MODEL_SEEDS", 200)).collect()
        }
    };
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as u64;
    let threads = knob("FENNEC_MODEL_THREADS", cores).clamp(1, seeds.len().max(1) as u64);
    let progress = knob("FENNEC_MODEL_PROGRESS", 0) == 1;
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let failed: Mutex<Option<String>> = Mutex::new(None);
    let began = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..threads {
            let work = || {
                while !stop.load(Atomic::Relaxed) {
                    let Some(&seed) = seeds.get(next.fetch_add(1, Atomic::Relaxed)) else {
                        break;
                    };
                    if let Err(report) = run_seed(seed, ops) {
                        stop.store(true, Atomic::Relaxed);
                        failed.lock().unwrap().get_or_insert(report);
                        break;
                    }
                    let n = done.fetch_add(1, Atomic::Relaxed) + 1;
                    if progress && n * 10 / seeds.len() != (n - 1) * 10 / seeds.len() {
                        eprintln!(
                            "model: {n} of {} seeds of {ops} steps, {:.0} s",
                            seeds.len(),
                            began.elapsed().as_secs_f64()
                        );
                    }
                }
            };
            std::thread::Builder::new()
                .stack_size(16 << 20)
                .spawn_scoped(s, work)
                .expect("a thread");
        }
    });
    if let Some(report) = failed.into_inner().unwrap() {
        panic!("{report}");
    }
}

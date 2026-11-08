use crate::error::{Error, Result};
use std::fmt;

/// Document id. Monotonically increasing, 64-bit.
pub type DocId = u64;

/// Storage precision of vector elements.
///
/// The runtime representation is `Value::Vector(Vec<f32>)` either way;
/// `F16` only halves the on-disk record and the HNSW arena. The values
/// embedding models produce fit comfortably into the f16 range
/// (+-65504, ~3 decimal digits), so the recall loss stays below the
/// measurable threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VecPrec {
    #[default]
    F32,
    F16,
}

/// fennecdb type system. Vectors are first-class citizens: `vector<768>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataType {
    Bool,
    Int,
    Float,
    Text,
    Bytes,
    /// UTC epoch milliseconds. Stored variable-length like `int`, but under
    /// a separate tag: display, the PostgreSQL type OID and `now()` all
    /// require that distinction -- as an alias over `int` every client
    /// would just see a raw number.
    Timestamp,
    /// Fixed-size vector. The dimension is known at schema time, which lets
    /// storage keep it inline without a length prefix (arena).
    Vector(usize, VecPrec),
    /// Sparse vector of dimension N: only the non-zero entries are kept,
    /// what a learned sparse model (SPLADE) gives a text. See
    /// [`crate::sparse`].
    Sparse(usize),
    /// Homogeneous list (of scalar types).
    List(Box<DataType>),
    /// Any value JSON can hold -- an object, a list, a number, text, a
    /// boolean or null -- untyped inside: `meta json`. A path reads into it
    /// (`meta.source.rank`), and `@hash` and `@sorted` take one.
    Json,
    /// A point on the earth, its longitude and latitude in degrees: `loc
    /// geo`. Written and read as `[lon, lat]`, GeoJSON's order and Redis's
    /// (`GEOADD key lon lat`); `distance`, `within`, `near` and `@geo` take
    /// one (see [`crate::geo`]).
    Geo,
}

impl DataType {
    pub fn name(&self) -> String {
        match self {
            DataType::Bool => "bool".into(),
            DataType::Int => "int".into(),
            DataType::Float => "float".into(),
            DataType::Text => "text".into(),
            DataType::Bytes => "bytes".into(),
            DataType::Timestamp => "timestamp".into(),
            DataType::Vector(d, VecPrec::F32) => format!("vector<{d}>"),
            DataType::Vector(d, VecPrec::F16) => format!("vector<{d}, f16>"),
            DataType::Sparse(d) => format!("sparse<{d}>"),
            DataType::List(inner) => format!("[{}]", inner.name()),
            DataType::Json => "json".into(),
            DataType::Geo => "geo".into(),
        }
    }

    /// Whether a field of the type keeps a list of numbers as written: a
    /// json field, and a point, whose degrees are `f64`s -- where a reader
    /// with no schema reads such a list into a vector's `f32`s, which would
    /// move a point by up to 1.7 m (`Database::exactly`).
    pub fn keeps_numbers(&self) -> bool {
        match self {
            DataType::Json | DataType::Geo => true,
            DataType::List(t) => t.keeps_numbers(),
            _ => false,
        }
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

/// Runtime value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    /// UTC epoch milliseconds.
    Timestamp(i64),
    Vector(Vec<f32>),
    List(Vec<Value>),
    /// A sparse vector: its dimension, and its non-zero entries ascending by
    /// index, the indices counted from 0. One `Vec` of pairs rather than two,
    /// so a `Value` stays the size it was.
    Sparse(u32, Vec<(u32, f32)>),
    /// A JSON object, in a `json` field: its members ascending by key, no
    /// key twice ([`Value::object`]) -- so a key is found by a binary
    /// search, two equal objects encode alike, and a hash index files them
    /// as one.
    Object(Vec<(String, Value)>),
}

// What a `Value` takes, held: 16 bytes in the browser module -- the tag in
// a niche of the sparse vector's `Vec` -- and 32 natively. A variant of two
// `f64`s for a point took the niche away there, 16 -> 24 bytes a value and
// 6 KB of the module, so a point is a list of two floats once read
// (`DataType::Geo`).
#[cfg(target_pointer_width = "32")]
const _: () = assert!(std::mem::size_of::<Value>() == 16);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Value>() == 32);

/// How deep a `json` value nests: an object or a list in one, 64 levels
/// down. A limit on the stack rather than on the data -- the codec, the
/// JSON reader and writer and the comparisons recurse a level at a time --
/// and past it a write is refused, never cut. MongoDB stops a document at
/// 100 levels; metadata is a handful.
pub const MAX_JSON_DEPTH: usize = 64;

/// How many keys a path may name past its field (`meta.a.b` names two):
/// no value nests deeper, so a longer one could only ever read `null`, and
/// it is refused rather than answered so.
pub const MAX_PATH_KEYS: usize = MAX_JSON_DEPTH;

/// `null`, to hand out by reference where a path leads nowhere.
pub static NULL: Value = Value::Null;

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Text(_) => "text",
            Value::Bytes(_) => "bytes",
            Value::Timestamp(_) => "timestamp",
            Value::Vector(_) => "vector",
            Value::List(_) => "list",
            Value::Sparse(..) => "sparse",
            Value::Object(_) => "object",
        }
    }

    /// An object from its members: sorted by key, a key given twice
    /// refused rather than one of them kept -- which one a reader keeps
    /// differs from reader to reader, and the difference would be silent.
    pub fn object(members: Vec<(String, Value)>) -> Result<Value> {
        let twice = |k: &str| Error::Type(format!("key `{k}` was given twice"));
        // Natively the standard library's sort. The browser module puts
        // each member where a binary search finds its place: a copy of the
        // sort for these pairs, or a merge of our own, was 4.1 and 1.2 KB of
        // it, and an object's members are a handful, mostly in order.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut members = members;
            members.sort_by(|a, b| a.0.cmp(&b.0));
            if let Some(w) = members.windows(2).find(|w| w[0].0 == w[1].0) {
                return Err(twice(&w[0].0));
            }
            Ok(Value::Object(members))
        }
        #[cfg(target_arch = "wasm32")]
        {
            let mut out: Vec<(String, Value)> = Vec::with_capacity(members.len());
            for m in members {
                match out.binary_search_by(|(k, _)| k.as_str().cmp(&m.0)) {
                    Ok(_) => return Err(twice(&m.0)),
                    Err(at) => out.insert(at, m),
                }
            }
            Ok(Value::Object(out))
        }
    }

    /// The member `key` of an object, by binary search; `None` for a key it
    /// does not hold and for anything that is not an object.
    pub fn member(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(m) => m
                .binary_search_by(|(k, _)| k.as_str().cmp(key))
                .ok()
                .map(|i| &m[i].1),
            _ => None,
        }
    }

    /// The value at `keys` (`source.rank`) inside this one, `null` where the
    /// path leads nowhere: a key missing, or a value on the way that is not
    /// an object.
    pub fn at_path(&self, keys: &str) -> &Value {
        let mut v = self;
        for k in keys.split('.') {
            match v.member(k) {
                Some(x) => v = x,
                None => return &NULL,
            }
        }
        v
    }

    /// Sets the value at `keys` inside this one -- the value of the field
    /// `field` -- an object made where the path finds `null` or nothing:
    /// `set docs {meta.lang: "en"}`. A value on the way that is not an
    /// object is refused rather than replaced.
    pub fn set_path(&mut self, field: &str, keys: &str, value: Value) -> Result<()> {
        let mut v = self;
        let mut walked = field.len();
        let path = format!("{field}.{keys}");
        for k in keys.split('.') {
            if v.is_null() {
                *v = Value::Object(Vec::new());
            }
            let Value::Object(m) = v else {
                return Err(Error::Type(format!(
                    "`{}` holds {}, not an object: `{path}` cannot be set in it",
                    path.get(..walked).unwrap_or_default(),
                    v.type_name()
                )));
            };
            walked += k.len() + 1;
            let at = match m.binary_search_by(|(x, _)| x.as_str().cmp(k)) {
                Ok(at) => at,
                Err(at) => {
                    m.insert(at, (k.to_string(), Value::Null));
                    at
                }
            };
            v = &mut m[at].1;
        }
        *v = value;
        Ok(())
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Timestamp(ms) => Some(*ms as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// A list of numbers as a vector's `f32`s, or the first value that is
    /// no number, for its caller to name in its own words. One loop for
    /// `near`'s query and the functions over vectors: each collected into a
    /// `Result`, the two were 1.1 KB more of the browser module.
    pub(crate) fn floats(items: &[Value]) -> std::result::Result<Vec<f32>, &Value> {
        let mut out = Vec::with_capacity(items.len());
        for v in items {
            out.push(v.as_f64().ok_or(v)? as f32);
        }
        Ok(out)
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_vector(&self) -> Option<&[f32]> {
        match self {
            Value::Vector(v) => Some(v),
            _ => None,
        }
    }

    /// Coerces the value to the target type (numeric widening and
    /// list->vector conversion included). Schema validation funnels
    /// through this single point.
    pub fn coerce(self, ty: &DataType) -> Result<Value> {
        self.coerce_as(ty, Fit::Write)
    }

    /// The value of a field of type `ty` that `=` finds equal to this one,
    /// as an index files it, or `None` where it is no value of that type
    /// -- for a key looked up by: an `=` through a `@hash`, a `lookup`'s
    /// probe. A number goes over only as the number it is, since
    /// `cmp_value` compares an int and a float by their true values: the
    /// float -2^63 finds the row holding `i64::MIN`, which it is, and 2^63
    /// nothing, which no int is; 2^53 + 1 finds no `float`, which none
    /// holds, where `as` would have made it 2^53. A `NaN` is a `float`'s
    /// key like any other, every `NaN` the same one ([`Value::cmp_value`];
    /// `hash_key` files them under one key whatever their bits), and no
    /// `int`'s.
    pub(crate) fn key_as(self, ty: &DataType) -> Option<Value> {
        self.coerce_as(ty, Fit::Key).ok()
    }

    /// The one body of [`Value::coerce`] and [`Value::key_as`], a number
    /// taken in as `fit` says: a function of its own for the ends, moving
    /// the value on into `coerce`, was 102 bytes more of the browser module.
    fn coerce_as(self, ty: &DataType, fit: Fit) -> Result<Value> {
        if self.is_null() {
            return Ok(Value::Null);
        }
        match (ty, self) {
            (DataType::Bool, Value::Bool(b)) => Ok(Value::Bool(b)),
            (DataType::Int, Value::Int(i)) => Ok(Value::Int(i)),
            // `as` saturates: `1e300` was filed as 9223372036854775807. A
            // whole float under 2^63 either side is the int it says, and
            // only a float takes this arm -- an integer written as one is
            // read as an `i64` and never meets the test.
            (DataType::Int, Value::Float(f)) if f.fract() == 0.0 => {
                let fits = match fit {
                    Fit::Write => f.abs() < I64_END,
                    Fit::Key => (-I64_END..I64_END).contains(&f),
                    Fit::F32 => f.abs() <= I64_END,
                };
                match fits {
                    true => Ok(Value::Int(f as i64)),
                    false => Err(past_int()),
                }
            }
            (DataType::Float, Value::Float(f)) => Ok(Value::Float(f)),
            // A key no float holds is none (`expected float` below, which
            // its callers drop): rounded, 2^53 + 1 found the rows holding
            // 2^53, which `=` does not.
            (DataType::Float, Value::Int(i)) if fit != Fit::Key || exact_float(i).is_some() => {
                Ok(Value::Float(i as f64))
            }
            (DataType::Text, Value::Text(s)) => Ok(Value::Text(s)),
            (DataType::Bytes, Value::Bytes(b)) => Ok(Value::Bytes(b)),
            (DataType::Bytes, Value::Text(s)) => Ok(Value::Bytes(s.into_bytes())),
            // As JSON writes bytes, a list of numbers from 0 to 255 -- read
            // as a vector's `f32`s or exactly, by where it stands -- so a
            // field read out goes back in: a sketch kept by day. Loops:
            // collected into a `Result`, the two were 1.2 KB more of the
            // browser module.
            (DataType::Bytes, Value::List(items)) => {
                let mut b = Vec::with_capacity(items.len());
                for v in &items {
                    match v {
                        Value::Int(n @ 0..=255) => b.push(*n as u8),
                        _ => return Err(not_bytes()),
                    }
                }
                Ok(Value::Bytes(b))
            }
            (DataType::Bytes, Value::Vector(v)) => {
                let mut b = Vec::with_capacity(v.len());
                for f in v {
                    if !(0.0..=255.0).contains(&f) || f.fract() != 0.0 {
                        return Err(not_bytes());
                    }
                    b.push(f as u8);
                }
                Ok(Value::Bytes(b))
            }
            (DataType::Timestamp, Value::Timestamp(ms)) => Ok(Value::Timestamp(ms)),
            // Epoch milliseconds directly; arithmetic results such as
            // `now() - 86400000` also pass through here.
            (DataType::Timestamp, Value::Int(ms)) => Ok(Value::Timestamp(ms)),
            (DataType::Timestamp, Value::Text(s)) => Ok(Value::Timestamp(crate::time::parse(&s)?)),
            (DataType::Int, Value::Timestamp(ms)) => Ok(Value::Int(ms)),
            (DataType::Text, Value::Timestamp(ms)) => Ok(Value::Text(crate::time::format_iso(ms))),
            (DataType::Vector(dim, _), Value::Vector(v)) => check_dim(v, *dim),
            // pgvector's text form is how a sparse vector arrives from JSON, a
            // FennecQL literal or an import; either way it is put in index
            // order, and its zeros dropped, before anything holds it.
            (DataType::Sparse(dim), Value::Sparse(d, e)) => {
                let (d, e) = crate::sparse::normalise(d, e).map_err(Error::Type)?;
                check_sparse_dim(d, e, *dim)
            }
            (DataType::Sparse(dim), Value::Text(s)) => {
                let (d, e) = crate::sparse::parse(&s)?;
                check_sparse_dim(d, e, *dim)
            }
            (DataType::Vector(dim, _), Value::List(items)) => {
                let out = Value::floats(&items).map_err(|it| {
                    Error::Type(format!(
                        "vector element must be numeric, found {}",
                        it.type_name()
                    ))
                })?;
                check_dim(out, *dim)
            }
            // On the JSON side an all-numeric array is parsed as a vector
            // (json.rs), because `near $1` is evaluated without schema
            // context. If the target field is a list it is converted back
            // here; without this arm `[int]`/`[float]` fields could not be
            // filled from the browser and fennec-server paths. The numbers
            // are `f32`s by then: a `[float]` keeps an `f32`'s digits, and an
            // `[int]` is exact only to 2^24 (vector-search.md#numbers-in).
            // Nor does an `f32` tell an int's ends from past them: 2^63 is
            // where every number from 2^63 - 2^38 to 2^63 + 2^39 lands,
            // i64::MAX among them, so an `[int]` takes ±2^63 as its ends, as
            // it always did ([`Fit::F32`]), and refuses an `f32` past them,
            // which only a number past 2^63 + 2^39 is.
            (DataType::List(inner), Value::Vector(v)) => {
                let mut out = Vec::with_capacity(v.len());
                for f in v {
                    out.push(Value::Float(f as f64).coerce_as(inner, Fit::F32)?);
                }
                Ok(Value::List(out))
            }
            (DataType::List(inner), Value::List(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    out.push(it.coerce_as(inner, fit)?);
                }
                Ok(Value::List(out))
            }
            (DataType::Json, v) => json_value(v, 0),
            // A point is `[lon, lat]` once read, a list of two floats: a
            // variant of its own grew every `Value` of the browser module.
            (DataType::Geo, v) => match crate::geo::point_of(&v)? {
                Some(p) => Ok(crate::codec::geo_value(p)),
                None => Ok(Value::Null),
            },
            (t, v) => Err(Error::Type(format!(
                "expected {}, found {}",
                t.name(),
                v.type_name()
            ))),
        }
    }

    /// Backstop of the total ordering for types that cannot be compared.
    /// Numeric types form a single group (Int <-> Float convert), and so do
    /// Text and Bytes -- since the schema coerces a `text` value into a
    /// `bytes` field, the two are compared within the same group.
    fn type_rank(&self) -> u8 {
        match self {
            Value::Null => 0,
            Value::Bool(_) => 1,
            Value::Int(_) | Value::Float(_) | Value::Timestamp(_) => 2,
            Value::Text(_) | Value::Bytes(_) => 3,
            Value::Vector(_) => 4,
            Value::List(_) => 5,
            Value::Sparse(..) => 6,
            Value::Object(_) => 7,
        }
    }

    /// Total ordering for sorting/comparison. Null counts as the smallest;
    /// a `NaN` equals a `NaN`, whatever its bits, and is greater than every
    /// other number, `+inf` included: PostgreSQL's order ([`floats`]).
    pub fn cmp_value(&self, other: &Value) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, _) => Ordering::Less,
            (_, Value::Null) => Ordering::Greater,
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            // Exact: through the f64 arm below two ints past 2^53 that differ
            // compared equal, and an ordered index, which keys ints exactly,
            // would then disagree with the scan about their order.
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            // An arm of its own, so two floats never ask whether a tie
            // needs a second look (below): through the last arm, as they
            // went before, an order by a float took 0.3% more instructions
            // for that; this way 1.5% fewer than before, and an order by an
            // int 0.6% fewer, the dispatch laid out anew.
            (Value::Float(a), Value::Float(b)) => floats(*a, *b),
            (Value::Text(a), Value::Text(b)) => a.cmp(b),
            (Value::Bytes(a), Value::Bytes(b)) => a.cmp(b),
            (Value::Timestamp(a), Value::Timestamp(b)) => a.cmp(b),
            // A timestamp must be comparable with a text literal: the write
            // path parses the text, so `where created_at >= "2026-01-01"`
            // must agree. Unparseable text falls back to type rank.
            (Value::Timestamp(a), Value::Text(b)) => match crate::time::parse(b) {
                Ok(ms) => a.cmp(&ms),
                Err(_) => Ordering::Less,
            },
            (Value::Text(a), Value::Timestamp(b)) => match crate::time::parse(a) {
                Ok(ms) => ms.cmp(b),
                Err(_) => Ordering::Greater,
            },
            // A `bytes` field must be comparable with a text literal: the
            // write path turns Text into Bytes, so the read path must agree.
            (Value::Text(a), Value::Bytes(b)) => a.as_bytes().cmp(&b[..]),
            (Value::Bytes(a), Value::Text(b)) => a[..].cmp(b.as_bytes()),
            // Arrays element by element: previously neither side was
            // numeric, so this returned Equal -- `tags = ["rust"]` matched
            // every row.
            (Value::List(a), Value::List(b)) => {
                for (x, y) in a.iter().zip(b.iter()) {
                    let o = x.cmp_value(y);
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                a.len().cmp(&b.len())
            }
            (Value::Sparse(da, a), Value::Sparse(db, b)) => {
                for (x, y) in a.iter().zip(b.iter()) {
                    let o = x.0.cmp(&y.0).then(floats(x.1 as f64, y.1 as f64));
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                a.len().cmp(&b.len()).then(da.cmp(db))
            }
            // Member by member, each its key and then its value, as a list
            // goes element by element.
            (Value::Object(a), Value::Object(b)) => {
                for (x, y) in a.iter().zip(b.iter()) {
                    let o = x.0.cmp(&y.0).then_with(|| x.1.cmp_value(&y.1));
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                a.len().cmp(&b.len())
            }
            // An `f32` as the `f64` it widens to, a `NaN` as a `NaN`: one
            // rule, one copy of it.
            (Value::Vector(a), Value::Vector(b)) => {
                for (x, y) in a.iter().zip(b.iter()) {
                    let o = floats(*x as f64, *y as f64);
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                a.len().cmp(&b.len())
            }
            // Numbers of two kinds -- an int, a float, a timestamp -- by their
            // true values, as two ints are: met as two f64s alone, 2^60 and
            // 2^60 + 1 each equalled the float 2^60 and not each other, so a
            // `@hash`, keying the int exactly, found one row where the scan
            // found every int rounding to 2^60, a sort had no order to keep,
            // and 2^63 equalled `i64::MAX`. The f64s still order them
            // wherever they differ ([`tie`] says why), and are equal only
            // where the numbers are, short of 2^53, where every int is a
            // float; a tie past it is looked at again. Arms of their own for
            // the mixed kinds made every comparison's dispatch longer: an
            // order by a float took 2% more instructions, one by an int 0.6%.
            _ => match (self.as_f64(), other.as_f64()) {
                (Some(a), Some(b)) => match a.partial_cmp(&b) {
                    Some(Ordering::Equal) if a.abs() >= EVERY_INT => tie(self, other),
                    Some(o) => o,
                    // A `NaN`, which only a float is: above the other.
                    None => a.is_nan().cmp(&b.is_nan()),
                },
                // Incomparable types fall back to type rank. This used to
                // return Equal: `year = "abc"` matched every row.
                _ => self.type_rank().cmp(&other.type_rank()),
            },
        }
    }
}

/// Two floats in PostgreSQL's order: by their values, `-0.0` equal to
/// `0.0`, and a `NaN` equal to a `NaN` and greater than every other float,
/// `+inf` included. A `NaN` equalled every value before, the one
/// comparison left that was no order -- `NaN = 3`, `NaN = 5` and `3 < 5`
/// at once -- so a sort over a float field holding one could panic in the
/// standard library ("does not correctly implement a total order"), and an
/// index, which keys a value in one place, could not give the scan's
/// answer. The `NaN` test is asked only where `partial_cmp` finds no
/// order, so two numbers pay nothing for it (docs/design/indexes.md#sorted).
#[inline]
fn floats(a: f64, b: f64) -> std::cmp::Ordering {
    match a.partial_cmp(&b) {
        Some(o) => o,
        None => a.is_nan().cmp(&b.is_nan()),
    }
}

/// `v` as a `json` field holds it: what JSON can say, nested no deeper
/// than [`MAX_JSON_DEPTH`]. A timestamp becomes its ISO text, as JSON
/// writes one. Bytes, a sparse vector, a number that is not finite and a
/// vector -- `f32`s, not the numbers they were read from -- are refused.
fn json_value(v: Value, depth: usize) -> Result<Value> {
    let deeper = || {
        Error::Type(format!(
            "a json value nests at most {MAX_JSON_DEPTH} levels deep"
        ))
    };
    Ok(match v {
        Value::Float(f) if !f.is_finite() => {
            return Err(Error::Type("a json number is finite".into()));
        }
        Value::Timestamp(ms) => Value::Text(crate::time::format_iso(ms)),
        // A vector holds the `f32`s a list of numbers was read into, not
        // the numbers written -- `0.1` is 0.10000000149011612 there, and
        // 12345678901 is 12345679000 -- so a json field, which keeps what
        // it is given, refuses one rather than keep another number. The
        // readers of text hand a json field the list as written instead
        // (`Database::exactly`).
        Value::Vector(_) => {
            return Err(Error::Type(
                "a json field keeps numbers as they were written, and a vector holds the \
                 f32s a list of them was read into: give the list as text, or as JSON"
                    .into(),
            ))
        }
        Value::List(items) => {
            if depth >= MAX_JSON_DEPTH {
                return Err(deeper());
            }
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(json_value(it, depth + 1)?);
            }
            Value::List(out)
        }
        Value::Object(members) => {
            if depth >= MAX_JSON_DEPTH {
                return Err(deeper());
            }
            let mut out = Vec::with_capacity(members.len());
            for (k, v) in members {
                out.push((k, json_value(v, depth + 1)?));
            }
            // Sorted, and a key twice refused, however it was made: one built
            // in Rust may be neither, and a member is found by binary search.
            Value::object(out)?
        }
        Value::Bytes(_) | Value::Sparse(..) => {
            return Err(Error::Type(format!(
                "json holds no {}: objects, lists, numbers, text, booleans and null",
                v.type_name()
            )))
        }
        v => v,
    })
}

fn check_sparse_dim(d: u32, e: Vec<(u32, f32)>, dim: usize) -> Result<Value> {
    if d as usize != dim {
        return Err(Error::Type(format!(
            "sparse vector dimension must be {dim}, got {d}"
        )));
    }
    Ok(Value::Sparse(d, e))
}

fn not_bytes() -> Error {
    Error::Type("bytes are numbers from 0 to 255".into())
}

/// 2^63: the first `f64` past `i64::MAX`, and `i64::MIN`'s magnitude.
/// -2^63 is `i64::MIN`, yet a float of it is refused too when written: a
/// JSON integer past `i64::MIN` is read as the float it rounds to
/// (`-9223372036854775809` is -2^63 exactly), and there the text that
/// said it is gone. `i64::MIN` written as an integer is an `i64` and taken,
/// and a key looked up by -2^63 is that int ([`Value::key_as`]).
const I64_END: f64 = 9_223_372_036_854_775_808.0;

/// How [`Value::coerce_as`] takes a number into an `int` or a `float`.
#[derive(Clone, Copy, PartialEq)]
enum Fit {
    /// As a write files it: a whole float short of 2^63 either side into
    /// an `int`, an int into a `float` as the nearest float.
    Write,
    /// As a key `=` finds rows by: only the number it is, -2^63 the int
    /// `i64::MIN` and 2^63 none, an int no float holds no `float`.
    Key,
    /// An `[int]`'s element read as an `f32`, where ±2^63 is what its ends
    /// were read as.
    F32,
}

/// 2^53: every int short of it either side is a float, so two numbers
/// whose f64s are equal and short of it are equal. 2^53 + 1 is not one,
/// and its f64 is 2^53.
const EVERY_INT: f64 = 9_007_199_254_740_992.0;

/// Two numbers of two kinds whose f64s are equal, at or past 2^53, by
/// their true values -- an int or a timestamp (its milliseconds' int)
/// against a float or against each other. Rounding to the
/// nearest float is monotonic, so where the f64s of two numbers differ so
/// do the numbers, the same way, and `cmp_value` takes the f64s' answer
/// there; where they are equal a float among them is a whole one from
/// -2^63 to 2^63, the nearest to the int, and the two are told apart as
/// ints: 2^63 is past every int, and anything short of it is exactly an
/// `i64`. Out of line: only a tie past 2^53 comes here.
#[inline(never)]
fn tie(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let whole = |i: i64, f: f64| match f < I64_END {
        true => i.cmp(&(f as i64)),
        false => Ordering::Less,
    };
    match (a, b) {
        (Value::Int(i) | Value::Timestamp(i), Value::Int(j) | Value::Timestamp(j)) => i.cmp(j),
        (Value::Int(i) | Value::Timestamp(i), Value::Float(f)) => whole(*i, *f),
        (Value::Float(f), Value::Int(i) | Value::Timestamp(i)) => whole(*i, *f).reverse(),
        _ => Ordering::Equal,
    }
}

/// The int `f` is, exactly: whole, and from -2^63 up to short of 2^63.
pub(crate) fn exact_int(f: f64) -> Option<i64> {
    (f.fract() == 0.0 && (-I64_END..I64_END).contains(&f)).then_some(f as i64)
}

/// The float `i` is, exactly: an int a float holds every bit of -- each
/// up to 2^53, and past it those whose bits below the float's 53 are
/// zero. `i64::MAX` rounds to 2^63, which `as` takes back to it, so
/// 2^63 is held out first.
pub(crate) fn exact_float(i: i64) -> Option<f64> {
    let f = i as f64;
    (f < I64_END && f as i64 == i).then_some(f)
}

/// The two floats `i` lies between, an int no float holds ([`exact_float`]
/// none), with no float between them: an ordered index keying floats says
/// `x > i` as `x > below` and `x >= i` as `x >= above`, where no key says
/// `i`. `i as f64` is the nearer one, either side; `i64::MAX`'s is 2^63.
/// The other is a step in its bits -- toward zero or away, the bits
/// holding the magnitude -- as every float past 2^53 is whole and
/// finite: `next_up` and `next_down`, which mind the rest, were 249
/// bytes of the browser module.
#[cfg(feature = "sorted")]
pub(crate) fn floats_beside(i: i64) -> (f64, f64) {
    let f = i as f64;
    let above = f >= I64_END || f as i64 > i;
    let bits = f.to_bits();
    let g = f64::from_bits(match above == (f > 0.0) {
        true => bits - 1,
        false => bits + 1,
    });
    match above {
        true => (g, f),
        false => (f, g),
    }
}

/// The refusal of a whole float at or past [`I64_END`] going into an `int`,
/// which a document's write names the field of (`engine::in_field`). Out of
/// line: the range is a limit, and a limit is an error, never the nearest
/// int it would have been cut to.
#[cold]
#[inline(never)]
fn past_int() -> Error {
    Error::Type("an int is from -9223372036854775808 to 9223372036854775807".into())
}

fn check_dim(v: Vec<f32>, dim: usize) -> Result<Value> {
    if v.len() != dim {
        return Err(Error::Type(format!(
            "vector dimension must be {}, got {}",
            dim,
            v.len()
        )));
    }
    Ok(Value::Vector(v))
}

/// Field name -> value. Field order comes from the schema, so a Vec suffices.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Document {
    pub id: DocId,
    pub fields: Vec<(String, Value)>,
}

impl Document {
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.fields.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    /// [`Self::get`], a path (`meta.source.rank`) read into its field:
    /// `null` where it leads nowhere, `None` only where the field is not
    /// the document's. What a path index keeps up as a write lands.
    pub fn at(&self, name: &str) -> Option<&Value> {
        match name.split_once('.') {
            None => self.get(name),
            Some((field, keys)) => self.get(field).map(|v| v.at_path(keys)),
        }
    }

    pub fn set(&mut self, name: &str, value: Value) {
        if let Some(slot) = self.fields.iter_mut().find(|(k, _)| k == name) {
            slot.1 = value;
        } else {
            self.fields.push((name.to_string(), value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::{self, *};

    /// An int against a float worked out in `i128`, which holds every int
    /// and the floor of every float short of 2^127 exactly.
    fn by_i128(i: i64, f: f64) -> Ordering {
        let far = 2f64.powi(127);
        if f.is_nan() {
            return Less;
        }
        if f >= far {
            return Less;
        }
        if f <= -far {
            return Greater;
        }
        let floor = f.floor();
        match (i as i128).cmp(&(floor as i128)) {
            Equal if f > floor => Less,
            o => o,
        }
    }

    /// Ints at the edges a float's precision changes at, and either side.
    fn ints() -> Vec<i64> {
        let mut out = vec![0, 1, -1, 3, i64::MAX, i64::MIN];
        for e in [24, 52, 53, 54, 60, 62, 63] {
            let p = 1i128 << e;
            for d in [
                -257, -256, -129, -128, -127, -2, -1, 0, 1, 2, 127, 128, 129, 255, 256, 257,
            ] {
                for x in [p + d, -p + d] {
                    if let Ok(x) = i64::try_from(x) {
                        out.push(x);
                    }
                }
            }
        }
        out
    }

    /// Each int's nearest float, the floats either side of it and halves.
    fn floats() -> Vec<f64> {
        let mut out = vec![
            0.0,
            -0.0,
            0.5,
            -0.5,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
            1e300,
            -1e300,
            f64::MIN_POSITIVE,
        ];
        for i in ints() {
            let f = i as f64;
            out.extend([f, f + 0.5, f - 0.5]);
            let bits = f.to_bits();
            out.extend([
                f64::from_bits(bits + 1),
                f64::from_bits(bits.saturating_sub(1)),
            ]);
        }
        out
    }

    #[test]
    fn an_int_and_a_float_compare_by_their_true_values() {
        for i in ints() {
            for f in floats() {
                let want = by_i128(i, f);
                let (int, float) = (Value::Int(i), Value::Float(f));
                assert_eq!(int.cmp_value(&float), want, "{i} {f:e}");
                assert_eq!(float.cmp_value(&int), want.reverse(), "{f:e} {i}");
                assert_eq!(Value::Timestamp(i).cmp_value(&float), want, "{i} {f:e}");
            }
            for j in ints() {
                assert_eq!(Value::Int(i).cmp_value(&Value::Timestamp(j)), i.cmp(&j));
                assert_eq!(Value::Timestamp(i).cmp_value(&Value::Int(j)), i.cmp(&j));
            }
        }
        let p60 = 1i64 << 60;
        let f60 = Value::Float(p60 as f64);
        assert_eq!(Value::Int(p60).cmp_value(&f60), Equal);
        assert_eq!(Value::Int(p60 + 1).cmp_value(&f60), Greater);
        assert_eq!(Value::Int(p60 - 1).cmp_value(&f60), Less);
        assert_eq!(Value::Int(i64::MAX).cmp_value(&Value::Float(I64_END)), Less);
        assert_eq!(
            Value::Int(i64::MIN).cmp_value(&Value::Float(-I64_END)),
            Equal
        );
    }

    /// A value of every kind at the edges `cmp_value` meets: `NaN`s of
    /// every sign and payload, the infinities, both zeros, ints and floats
    /// where a float's precision changes, the kinds that compare by rank,
    /// and the values holding numbers -- a list, an object, a vector of
    /// `f32`s and a sparse one.
    fn edges() -> Vec<Value> {
        let nans = [
            f64::NAN,
            -f64::NAN,
            f64::from_bits(0x7FF8_0000_0000_0001),
            f64::from_bits(0xFFF0_0000_0000_0001),
        ];
        let mut floats = vec![
            0.0,
            -0.0,
            2.5,
            -2.5,
            3.0,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MAX,
            I64_END,
            -I64_END,
        ];
        floats.extend(nans);
        for e in [53, 60] {
            let p = 2f64.powi(e);
            floats.extend([p, -p, f64::from_bits(p.to_bits() + 1)]);
        }
        let mut out = vec![
            Value::Null,
            Value::Bool(false),
            Value::Bool(true),
            Value::Text(String::new()),
            Value::Text("a".into()),
            Value::Bytes(vec![0xFF]),
        ];
        out.extend(floats.iter().map(|f| Value::Float(*f)));
        for i in [
            0,
            3,
            -1,
            1 << 53,
            (1 << 53) + 1,
            1 << 60,
            (1 << 60) + 1,
            i64::MAX,
            i64::MIN,
        ] {
            out.extend([Value::Int(i), Value::Timestamp(i)]);
        }
        let f32s = [0.0, -0.0, 1.0, f32::INFINITY, f32::NAN, -f32::NAN];
        for a in f32s {
            out.push(Value::Vector(vec![a, 1.0]));
            out.push(Value::Sparse(4, vec![(1, a)]));
        }
        for f in [nans[0], nans[1], 1.0, f64::INFINITY, -0.0] {
            out.push(Value::List(vec![Value::Float(f), Value::Int(1)]));
            out.push(Value::Object(vec![("k".into(), Value::Float(f))]));
        }
        out.push(Value::List(vec![Value::Int(1)]));
        out
    }

    /// PostgreSQL's order for `NaN`, and a total order over every kind:
    /// `NaN` equals `NaN` whatever its bits, and is greater than every
    /// other number, `+inf` included -- inside a list, an object or a
    /// vector too. It equalled every number, so `NaN = 3`, `NaN = 5` and
    /// `3 < 5` held at once, which a sort cannot keep.
    #[test]
    fn cmp_value_is_a_total_order_with_nan_above_every_number() {
        let vs = edges();
        for a in &vs {
            assert_eq!(a.cmp_value(a), Equal, "{a:?}");
            for b in &vs {
                assert_eq!(a.cmp_value(b), b.cmp_value(a).reverse(), "{a:?} {b:?}");
            }
        }
        for a in &vs {
            for b in &vs {
                let ab = a.cmp_value(b);
                if ab == Greater {
                    continue;
                }
                for c in &vs {
                    let bc = b.cmp_value(c);
                    if bc == Greater {
                        continue;
                    }
                    let want = if ab == Less || bc == Less {
                        Less
                    } else {
                        Equal
                    };
                    assert_eq!(a.cmp_value(c), want, "{a:?} {b:?} {c:?}");
                }
            }
        }
        let nan = Value::Float(f64::NAN);
        for v in &vs {
            let want = match v {
                Value::Float(f) if f.is_nan() => Equal,
                Value::Int(_) | Value::Float(_) | Value::Timestamp(_) => Greater,
                _ => continue,
            };
            assert_eq!(nan.cmp_value(v), want, "{v:?}");
        }
        let (zero, minus) = (Value::Float(0.0), Value::Float(-0.0));
        assert_eq!(zero.cmp_value(&minus), Equal);
        let v = |a: f32| Value::Vector(vec![a]);
        assert_eq!(v(f32::NAN).cmp_value(&v(-f32::NAN)), Equal);
        assert_eq!(v(f32::NAN).cmp_value(&v(f32::INFINITY)), Greater);
    }

    #[test]
    fn a_number_is_exactly_the_other_kind_or_none() {
        for i in ints() {
            match exact_float(i) {
                Some(f) => assert_eq!(by_i128(i, f), Equal, "{i}"),
                None => assert_ne!(by_i128(i, i as f64), Equal, "{i}"),
            }
        }
        for f in floats() {
            match exact_int(f) {
                Some(i) => assert_eq!(by_i128(i, f), Equal, "{f:e}"),
                None => {
                    let whole = f.fract() == 0.0 && (-I64_END..I64_END).contains(&f);
                    assert!(!whole, "{f:e}");
                }
            }
        }
        let (int, float) = (DataType::Int, DataType::Float);
        let key = |v: Value, ty: &DataType| v.key_as(ty);
        assert_eq!(
            key(Value::Float(-I64_END), &int),
            Some(Value::Int(i64::MIN))
        );
        assert_eq!(key(Value::Float(I64_END), &int), None);
        assert_eq!(key(Value::Float(2.5), &int), None);
        let nan = key(Value::Float(-f64::NAN), &float);
        assert!(
            matches!(nan, Some(Value::Float(f)) if f.is_nan()),
            "{nan:?}"
        );
        assert_eq!(key(Value::Float(f64::NAN), &int), None);
        assert_eq!(key(Value::Int((1 << 53) + 1), &float), None);
        let f60 = Value::Float((1u64 << 60) as f64);
        assert_eq!(key(Value::Int(1 << 60), &float), Some(f60));
        assert_eq!(key(Value::Int(i64::MAX), &float), None);
        // A write rounds an int into a float field, and refuses -2^63 into
        // an int one, whose text may have been past it.
        #[cfg(feature = "sorted")]
        for i in ints().into_iter().filter(|&i| exact_float(i).is_none()) {
            let (below, above) = floats_beside(i);
            assert_eq!(by_i128(i, below), Greater, "{i}");
            assert_eq!(by_i128(i, above), Less, "{i}");
            assert_eq!(below.next_up(), above, "{i}");
        }
        let rounded = Value::Int((1 << 53) + 1).coerce(&float).unwrap();
        assert_eq!(rounded, Value::Float(9_007_199_254_740_992.0));
        assert!(Value::Float(-I64_END).coerce(&int).is_err());
    }
}

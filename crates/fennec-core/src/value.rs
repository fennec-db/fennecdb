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
    /// json field, a point, whose degrees are `f64`s, and a list of numbers
    /// ([`Self::holds_numbers`]) -- where a reader with no schema reads such
    /// a list into a vector's `f32`s, which would move a point by up to 1.7
    /// m and keep 16777216 for 16777217 (`Database::exactly`).
    pub fn keeps_numbers(&self) -> bool {
        match self {
            DataType::Json | DataType::Geo => true,
            DataType::List(t) => t.is_number() || t.keeps_numbers(),
            _ => false,
        }
    }

    /// Whether the type is a list of numbers, `[int]`, `[float]` or
    /// `[timestamp]`: the one field keeping numbers that takes a vector,
    /// each `f32` an element, so a vector of whole `f32`s short of 2^24 --
    /// the integers written ([`whole_f32s`]) -- is handed it as it came
    /// and no other is.
    pub fn holds_numbers(&self) -> bool {
        matches!(self, DataType::List(t) if t.is_number())
    }

    fn is_number(&self) -> bool {
        matches!(self, DataType::Int | DataType::Float | DataType::Timestamp)
    }
}

/// Whether every `f32` of `v` is whole and short of 2^24 either side, where
/// an `f32` holds every integer: what a list of integers written alone is
/// read into, and the one vector a list of numbers takes as its numbers
/// written ([`DataType::holds_numbers`]). The quick read makes no such
/// vector of a list holding a number it rounds to one (`1.00000001` is 1 as
/// an `f32`), and reads that list as written ([`quick_vector`],
/// `num::hides_rounding`), so a vector of whole `f32`s is never another
/// list's. Through `as i32`, which the range test bounds: `trunc` is a call
/// of libm's on x86-64's baseline.
pub fn whole_f32s(v: &[f32]) -> bool {
    v.iter()
        .all(|&f| f.abs() < 16_777_216.0 && (f as i32) as f32 == f)
}

/// Numbers alone, read one at a time, as the vector of `f32`s the quick read
/// makes of them -- an embedding's -- or `None`: an empty list, one holding
/// no number, and one whose `f32`s would pass for the whole numbers written
/// ([`whole_f32s`]) where one was another number -- `[1.00000001]`, `[1e-50]`
/// -- which is read as written, a list, instead. The text's and the JSON's
/// readers a value at a time make their vectors here.
pub fn quick_vector(items: &[Value]) -> Option<Vec<f32>> {
    if items.is_empty() {
        return None;
    }
    let mut v = Vec::with_capacity(items.len());
    for x in items {
        v.push(match x {
            Value::Int(i) => *i as f32,
            Value::Float(f) => *f as f32,
            _ => return None,
        });
    }
    let hides = |(x, y): (&Value, &f32)| matches!(x, Value::Float(f) if *y as f64 != *f);
    match whole_f32s(&v) && items.iter().zip(&v).any(hides) {
        true => None,
        false => Some(v),
    }
}

/// Whether every component of `v` is a number a vector stored as `prec`
/// holds: no `NaN` and no infinity -- an `f32`'s, which every number from
/// 2^128 - 2^103 (3.4028235677973366e38) up rounds to, `1e39` among them --
/// nor, in an `f16` field, one rounding to an `f16`'s infinity: 65 504, the
/// largest `f16`, is odd, so 65 520, halfway to the next, goes up to it.
/// JSON writes an infinity and a `NaN` as `null`, and a distance over one is
/// an infinity or a `NaN`, so a write and `near`'s query refuse them
/// ([`unheld`]); what a file already holds is read as it is. A number too
/// small for an `f32` is the nearest one, 0 at or under half the smallest
/// subnormal, as pgvector's `strtof` reads it.
///
/// One pass, no branch a component: the bits under the sign order as the
/// magnitudes do, a `NaN`'s above an infinity's, so taken from the bound's
/// bits less one they borrow into the top bit exactly where they reach it,
/// and the pass ORs them. Natively LLVM makes the loop four lanes at a time:
/// 801 instructions a 768-dim vector on aarch64, by lldb's count, 2.6% of a
/// write of one handed over as `f32`s (docs/design/crates.md#limits). The
/// browser module, built for size, vectorises nothing on its own, and there
/// the lanes are written out, as `vector::simd` writes the distances.
#[inline]
pub fn held(v: &[f32], prec: VecPrec) -> bool {
    let below = match prec {
        VecPrec::F32 => f32::INFINITY.to_bits() - 1,
        VecPrec::F16 => 65_520f32.to_bits() - 1,
    };
    // The top bit set where `x`'s magnitude is past the bound.
    let over = |x: &f32| below.wrapping_sub(x.to_bits() & !(1 << 31));
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    {
        use core::arch::wasm32::*;
        let (fours, rest) = v.as_chunks::<4>();
        let (b, magnitude) = (u32x4_splat(below), u32x4_splat(!(1 << 31)));
        let mut lanes = u32x4_splat(0);
        for x in fours {
            // SAFETY: four `f32`s are the 16 bytes a `v128` reads, at any
            // alignment. Built from its elements, as `vector::simd` builds
            // its lanes, the loop took four loads a lane each for one.
            let x = unsafe { (x as *const [f32; 4] as *const v128).read_unaligned() };
            lanes = v128_or(lanes, i32x4_sub(b, v128_and(x, magnitude)));
        }
        i32x4_bitmask(lanes) == 0 && rest.iter().fold(0, |m, x| m | over(x)) >> 31 == 0
    }
    #[cfg(not(all(target_arch = "wasm32", target_feature = "simd128")))]
    {
        v.iter().fold(0, |m, x| m | over(x)) >> 31 == 0
    }
}

/// The refusal of a vector [`held`] finds a component of that its field's
/// `prec` does not hold, the first named, counted from 0, as a list's
/// element is: "element 3: a vector's component is a number at most
/// 3.4028235e38 either side of 0" -- the largest `f32`'s shortest digits --
/// a document's write naming the field before it (`engine::in_field`),
/// `near` its query vector. Out of line, on the refusal alone.
#[cold]
#[inline(never)]
pub fn unheld(v: &[f32], prec: VecPrec) -> Error {
    let i = v
        .iter()
        .position(|x| !held(std::slice::from_ref(x), prec))
        .unwrap_or(0);
    at_element(i, Error::Type(component(prec).into()))
}

/// What a vector stored as `prec` holds a component of, in a refusal's
/// words: [`unheld`]'s, and an import's naming the row
/// (`fennec_import::load`).
pub fn component(prec: VecPrec) -> &'static str {
    match prec {
        VecPrec::F32 => "a vector's component is a number at most 3.4028235e38 either side of 0",
        VecPrec::F16 => "an f16 vector's component is a number at most 65504 either side of 0",
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
    /// `int`'s. A time is no `text` key: `=` finds it equal to every text
    /// naming it, however spelled, which no one key files.
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
            // No key: `=` reads a text field's every spelling of the time
            // as the time ([`Value::cmp_read`]) -- `2026-01-01`, a space for
            // the `T` -- and the ISO text found that one alone through a
            // `@hash`, where the scan found them all.
            (DataType::Text, Value::Timestamp(ms)) if fit != Fit::Key => {
                Ok(Value::Text(crate::time::format_iso(ms)))
            }
            (DataType::Vector(dim, prec), Value::Vector(v)) => {
                check_dim(v, *dim, (fit == Fit::Write).then_some(*prec))
            }
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
            (DataType::Vector(dim, prec), Value::List(items)) => {
                let out = Value::floats(&items).map_err(|it| {
                    Error::Type(format!(
                        "vector element must be numeric, found {}",
                        it.type_name()
                    ))
                })?;
                check_dim(out, *dim, (fit == Fit::Write).then_some(*prec))
            }
            // A list of numbers alone is read into a vector's `f32`s where no
            // schema is at hand (`near $1`), and a list of numbers takes one
            // as its elements: every reader hands it the list as written
            // instead unless the `f32`s are the integers written, each whole
            // and short of 2^24 (`whole_f32s`, `Database::exactly`) -- a whole
            // one is the int it came from, as the exact read makes it, so a
            // `[timestamp]` takes it too. What else comes here is a vector
            // built in Rust, or one a binding marked exact
            // (`fennec_abi::EXACT`), its `f32`s the numbers it holds: 2^63 is
            // past every int, and refused as the float it is.
            (DataType::List(inner), Value::Vector(v)) => {
                // What an `[int]` and a `[float]` mostly take, made at once,
                // each element what the way below makes of it: a call of
                // this function an element was 5% of a `put` of rows each
                // holding an `[int]` of 16. Natively: the browser module
                // takes a page's lists as JSON, as written.
                #[cfg(not(target_arch = "wasm32"))]
                match inner.as_ref() {
                    DataType::Float => {
                        return Ok(Value::List(
                            v.into_iter().map(|f| Value::Float(f as f64)).collect(),
                        ))
                    }
                    DataType::Int if whole_f32s(&v) => {
                        return Ok(Value::List(
                            v.into_iter().map(|f| Value::Int(f as i64)).collect(),
                        ))
                    }
                    _ => {}
                }
                let mut out = Vec::with_capacity(v.len());
                for (i, f) in v.into_iter().enumerate() {
                    let x = match f.abs() < 16_777_216.0 && (f as i32) as f32 == f {
                        true => Value::Int(f as i64),
                        false => Value::Float(f as f64),
                    };
                    out.push(x.coerce_as(inner, fit).map_err(|e| at_element(i, e))?);
                }
                Ok(Value::List(out))
            }
            (DataType::List(inner), Value::List(items)) => {
                // Elements of the type already, as a list of numbers read as
                // written mostly holds, are what the way below would make
                // of each: the list is kept as it came, not built again.
                let kept = match inner.as_ref() {
                    DataType::Float => items.iter().all(|v| matches!(v, Value::Float(_))),
                    DataType::Int => items.iter().all(|v| matches!(v, Value::Int(_))),
                    _ => false,
                };
                if kept {
                    return Ok(Value::List(items));
                }
                let mut out = Vec::with_capacity(items.len());
                for (i, it) in items.into_iter().enumerate() {
                    out.push(it.coerce_as(inner, fit).map_err(|e| at_element(i, e))?);
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

    /// The order of the kinds, which values of two kinds take
    /// ([`Value::cmp_value`]). The numbers are one group, an int, a float
    /// and a timestamp (its milliseconds' int) compared by their true
    /// values, and so are Text and Bytes -- since the schema coerces a
    /// `text` value into a `bytes` field, the two are compared by their
    /// bytes. Any other two kinds order by this alone: a time is below
    /// every text.
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

    /// The total order `order`, `min`, `max` and every key take. Null
    /// counts as the smallest; a `NaN` equals a `NaN`, whatever its bits,
    /// and is greater than every other number, `+inf` included:
    /// PostgreSQL's order ([`floats`]). Values of two kinds order by kind
    /// ([`Value::type_rank`]), but for the numbers, and a text and bytes,
    /// each group compared as one: a time is below every text. A comparison
    /// -- `=`, `<`, `in`, `has` -- reads a text beside a time as the time it
    /// names ([`Value::cmp_read`]).
    #[inline]
    pub fn cmp_value(&self, other: &Value) -> std::cmp::Ordering {
        self.cmp_kinds::<false>(other)
    }

    /// Two values as a comparison takes them -- `=`, `<`, `in`, `has`, a
    /// filter's and an expression's alike: as [`Value::cmp_value`] orders
    /// them, but a text beside a time is read as the time it names, in a
    /// list or an object too, one naming none above every time. Whichever
    /// side it came from -- a literal, a parameter, a json field's ISO
    /// text, a text field's: `where j.at > now() - 1d` compares two times.
    /// And a list of numbers from 0 to 255 beside bytes is the bytes it
    /// writes ([`bytes_beside`]), in a stream's test and a `case` as in a
    /// query's filter. Never an order: a time equals the text naming it and
    /// the int 0 the time 0, while the text ranks above every int, so a
    /// sort by it could panic, and `order coalesce(t, j)` did
    /// (docs/design/indexes.md#sorted).
    #[inline]
    pub fn cmp_read(&self, other: &Value) -> std::cmp::Ordering {
        self.cmp_kinds::<true>(other)
    }

    /// [`Value::cmp_value`], and with `READ` [`Value::cmp_read`]: one body,
    /// two copies, 1.1 KB more of the browser module. A flag the copies
    /// shared, asked only where a time meets a text, was a register kept
    /// through the recursion and one more to pass at every call: an order
    /// of 100 000 rows by any field took 2.2% more instructions, a float
    /// field's scan 0.8%. A `cmp_read` of the reading arms alone, the rest
    /// sent on to `cmp_value` and `query::compare` sending it those kinds
    /// alone, was 682 bytes smaller, and took the float field's scan 1.9%
    /// more, the kinds asked before every comparison.
    fn cmp_kinds<const READ: bool>(&self, other: &Value) -> std::cmp::Ordering {
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
            // A time and a text order by kind, the time below, as any two
            // kinds do (`type_rank`); a comparison reads the text as the
            // time it names (`cmp_read`), as the write path reads one into
            // a `timestamp` field, so `where created_at >= "2026-01-01"`
            // agrees. Read in the order too, `Timestamp(0)` equalled
            // "1970-01-01T00:00:00Z", which the int 0, equal to the time,
            // ranked below: no order, and `order coalesce(t, j)` over a time
            // field and a json one holding ints and ISO texts panicked in
            // the standard library's sort in 3 of 117 runs.
            (Value::Timestamp(a), Value::Text(b)) => match READ {
                true => time_beside(*a, b),
                false => Ordering::Less,
            },
            (Value::Text(a), Value::Timestamp(b)) => match READ {
                true => time_beside(*b, a).reverse(),
                false => Ordering::Greater,
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
                    let o = x.cmp_kinds::<READ>(y);
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
                    let o = x.0.cmp(&y.0).then_with(|| x.1.cmp_kinds::<READ>(&y.1));
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
                    None => nan_order(a, b),
                },
                // Incomparable types fall back to type rank. This used to
                // return Equal: `year = "abc"` matched every row. A
                // comparison reads a list of byte numbers beside bytes
                // first ([`bytes_beside`]), here where no two kinds that
                // compare pay for the question.
                _ => match READ.then(|| bytes_beside(self, other)).flatten() {
                    Some(o) => o,
                    None => self.type_rank().cmp(&other.type_rank()),
                },
            },
        }
    }
}

/// Bytes beside a list of numbers from 0 to 255 -- a list, or a vector's
/// `f32`s, by where it was read -- as a comparison reads them: the list as
/// the bytes it writes ([`Value::key_as`]), as JSON writes bytes out and
/// the bound filter reads a value beside a `bytes` field once a query
/// (`engine::as_field`). Compared by kind, `b = [104, 105]` found the row
/// in a query and not in a stream's `eval`, which then told a subscriber
/// the row it had seeded was deleted. A list of other numbers, or of
/// other values, is none: `None`, and the two order by kind. Out of line:
/// no comparison of two other kinds pays for the conversion's code.
#[inline(never)]
fn bytes_beside(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    let (bytes, list, flip) = match (a, b) {
        (Value::Bytes(x), l @ (Value::List(_) | Value::Vector(_))) => (x, l, false),
        (l @ (Value::List(_) | Value::Vector(_)), Value::Bytes(y)) => (y, l, true),
        _ => return None,
    };
    let Some(Value::Bytes(read)) = list.clone().key_as(&DataType::Bytes) else {
        return None;
    };
    let o = bytes[..].cmp(&read[..]);
    Some(if flip { o.reverse() } else { o })
}

/// A time `t` beside a text, as a comparison reads them: the text as the
/// time it names, one naming none above every time, as
/// [`Value::cmp_value`] ranks it. Out of line: no comparison of two other
/// kinds pays for the parse's code.
#[inline(never)]
fn time_beside(t: i64, text: &str) -> std::cmp::Ordering {
    match crate::time::parse(text) {
        Ok(ms) => t.cmp(&ms),
        Err(_) => std::cmp::Ordering::Less,
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
        None => nan_order(a, b),
    }
}

/// [`floats`] where a `NaN` is met. Out of line and cold: inlined, its
/// test was folded into the `>` path, and an order of 100 000 rows by a
/// float took 1.8% more instructions.
#[cold]
#[inline(never)]
fn nan_order(a: f64, b: f64) -> std::cmp::Ordering {
    a.is_nan().cmp(&b.is_nan())
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

/// The vector a filter compares a vector field with where it is handed a
/// list of numbers alone: each number's `f32`, as the quick read makes of
/// it -- and `None` for any other value, compared as it is. A text read as
/// written for a json field or a list of numbers reads every list in it so,
/// and a parameter given one is read so; a vector is never equal to a
/// list, and `v = [0.5, 2]` beside them then found no row, where the quick
/// read's vector found it (`a_list_parameter_used_as_a_vector_answers_alike_hinted_or_not`).
/// Made once a query where the filter is bound (`Filter::bind`): asked in
/// `cmp_value`, even out of line, an order by an int took 1.15 to 3.4% more
/// instructions and one by a float 1.5 to 3.2% (`make instr-compare
/// PKG=fennec-core EXAMPLE=orders`), every comparison's dispatch laid out
/// anew.
pub fn compared_vector(v: &Value) -> Option<Vec<f32>> {
    let Value::List(items) = v else {
        return None;
    };
    if items.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(items.len());
    for x in items {
        out.push(match x {
            Value::Int(i) => *i as f32,
            Value::Float(f) => *f as f32,
            _ => return None,
        });
    }
    Some(out)
}

/// The vector a list of numbers alone read as written is read into the
/// quick way where its `f32`s are the integers written
/// ([`whole_f32s`], [`quick_vector`]), and `None` for any other value: what
/// the quick read hands a list of numbers as it came
/// (`Exactly::settle`), so a list read as written where it was asked for
/// at once (`fennec_http::api`'s hint) is the value it would have been
/// read again as -- the integers the vector is, a list a number past them.
pub fn whole_vector(v: &Value) -> Option<Vec<f32>> {
    // Each a whole number short of 2^24, an `f32` holding it and hiding no
    // rounding: asked before the vector is made, a list of decimals -- the
    // one a hint reads at once -- leaves at its first number, nothing set
    // aside.
    let whole = |x: &Value| match *x {
        Value::Int(i) => i.unsigned_abs() < 1 << 24,
        Value::Float(f) => f.abs() < 16_777_216.0 && (f as i32) as f64 == f,
        _ => false,
    };
    match v {
        Value::List(items) if !items.is_empty() && items.iter().all(whole) => quick_vector(items),
        _ => None,
    }
}

/// The int `f` is, exactly: whole, and from -2^63 up to short of 2^63.
/// `as` saturates and takes a `NaN` to 0, so the int is `f` again only
/// where `f` is it -- or 2^63, which `i64::MAX` goes back to, held out
/// after. Every float a group or a set files goes through this
/// (`codec::encode_key`): `fract` asked for a `trunc`, a call into libm on
/// x86-64 short of SSE4.1, and a range test both ways.
#[inline]
pub(crate) fn exact_int(f: f64) -> Option<i64> {
    let i = f as i64;
    (i as f64 == f && f < I64_END).then_some(i)
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

/// The refusal `e` of a list's element `i`, counted from 0, as a batch's
/// statements are: a document's write names the field before it
/// (`engine::in_field`), "field `xs`, element 2: an int is from ...". Out of
/// line, on the refusal alone.
#[cold]
#[inline(never)]
fn at_element(i: usize, e: Error) -> Error {
    match e {
        Error::Type(m) => Error::Type(format!("element {i}: {m}")),
        e => e,
    }
}

/// `v` as a field of `dim` dimensions holds it, and where it is `written`
/// into one stored as that precision, each component a number the field
/// holds ([`held`]) -- never where a key is looked up by it, which `=`
/// finds a row by as it is ([`Value::key_as`]): a file an earlier build
/// wrote may hold an infinity.
fn check_dim(v: Vec<f32>, dim: usize, written: Option<VecPrec>) -> Result<Value> {
    if v.len() != dim {
        return Err(Error::Type(format!(
            "vector dimension must be {}, got {}",
            dim,
            v.len()
        )));
    }
    if let Some(prec) = written {
        if !held(&v, prec) {
            return Err(unheld(&v, prec));
        }
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

    /// `whole_vector` is the quick read's vector where its `f32`s are the
    /// integers written, and nothing else: asked without making the vector
    /// first, it answers as `quick_vector` and `whole_f32s` together do.
    #[test]
    fn a_whole_vector_is_the_quick_read_s_where_it_is_whole() {
        let (i, f) = (Value::Int, Value::Float);
        let lists = [
            vec![i(1), i(-2)],
            vec![f(1.0), i(2)],
            vec![f(-0.0)],
            vec![f(0.5), i(2)],
            vec![f(1.00000001), i(2)],
            vec![i(16_777_215), i(-16_777_215)],
            vec![i(16_777_216)],
            vec![f(16_777_216.0)],
            vec![f(1e-50)],
            vec![i(1), Value::Text("a".into())],
            vec![],
        ];
        for items in lists {
            let want = quick_vector(&items).filter(|v| whole_f32s(v));
            let got = whole_vector(&Value::List(items.clone()));
            assert_eq!(
                got.as_ref()
                    .map(|v| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>()),
                want.as_ref()
                    .map(|v| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>()),
                "{items:?}"
            );
        }
        assert_eq!(whole_vector(&Value::Vector(vec![1.0])), None);
    }

    /// A list of numbers alone a filter compares a vector field with is the
    /// vector of its `f32`s; any other value is compared as it is.
    #[test]
    fn a_list_compared_with_a_vector_is_its_f32s() {
        let l = Value::List;
        assert_eq!(
            compared_vector(&l(vec![Value::Float(0.1), Value::Int(2)])),
            Some(vec![0.1, 2.0])
        );
        assert_eq!(
            compared_vector(&l(vec![Value::Float(1.00000001)])),
            Some(vec![1.0])
        );
        assert_eq!(
            compared_vector(&l(vec![Value::Int(1), Value::Text("a".into())])),
            None
        );
        assert_eq!(compared_vector(&l(vec![])), None);
        assert_eq!(compared_vector(&Value::Vector(vec![1.0])), None);
    }

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
        // A time's milliseconds about 2026-01-01T00:00:00Z as an int, a
        // time and a float, and the texts that read as times -- that one,
        // with its milliseconds and without, a day alone, the epoch --
        // beside texts that read as none: an int was equal to a time, which
        // was equal to a text reading as it, which ranked above the int.
        let ms: i64 = 1_767_225_600_000;
        for i in [ms - 1, ms, ms + 1] {
            out.extend([
                Value::Int(i),
                Value::Timestamp(i),
                Value::Float(i as f64),
                Value::Float(i as f64 + 0.5),
            ]);
        }
        let times = [
            "1970-01-01T00:00:00Z",
            "1970-01-01T00:00:00.003Z",
            "2026-01-01",
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00.001Z",
            "2025-12-31T23:59:59.999Z",
            "0000-01-01",
        ];
        for t in times.iter().chain(&["0", "3", "abc", "1767225600000"]) {
            out.push(Value::Text(t.to_string()));
        }
        out.extend([Value::Bytes(b"a".to_vec()), Value::Bytes(Vec::new())]);
        for v in [
            Value::Timestamp(0),
            Value::Int(0),
            Value::Text(times[0].into()),
        ] {
            out.push(Value::List(vec![v.clone(), Value::Int(1)]));
            out.push(Value::Object(vec![("k".into(), v)]));
        }
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
        // A time is a number, below every text, whatever the text reads as.
        let epoch = Value::Text("1970-01-01T00:00:00Z".into());
        assert_eq!(Value::Timestamp(0).cmp_value(&Value::Int(0)), Equal);
        assert_eq!(Value::Timestamp(0).cmp_value(&epoch), Less);
        assert_eq!(epoch.cmp_value(&Value::Timestamp(i64::MAX)), Greater);
    }

    /// A comparison reads a text beside a time as the time it names, either
    /// side, in a list and an object too, one naming none above every time,
    /// and a list of numbers from 0 to 255 beside bytes as the bytes it
    /// writes; any other two values it takes as the order does.
    #[test]
    fn a_comparison_reads_a_text_beside_a_time() {
        let text = |t: &str| Value::Text(t.into());
        let epoch = text("1970-01-01T00:00:00Z");
        let (t0, t1) = (Value::Timestamp(0), Value::Timestamp(1));
        assert_eq!(t0.cmp_read(&epoch), Equal);
        assert_eq!(epoch.cmp_read(&t0), Equal);
        assert_eq!(t1.cmp_read(&epoch), Greater);
        assert_eq!(epoch.cmp_read(&t1), Less);
        assert_eq!(t1.cmp_read(&text("abc")), Less);
        assert_eq!(text("").cmp_read(&t1), Greater);
        let list = |v: Value| Value::List(vec![Value::Int(1), v]);
        assert_eq!(list(t0.clone()).cmp_read(&list(epoch.clone())), Equal);
        let object = |v: Value| Value::Object(vec![("at".into(), v)]);
        assert_eq!(object(epoch.clone()).cmp_read(&object(t0.clone())), Equal);
        assert_eq!(list(t0.clone()).cmp_value(&list(epoch.clone())), Less);
        let hi = Value::Bytes(b"hi".to_vec());
        let ints = |v: &[i64]| Value::List(v.iter().map(|&i| Value::Int(i)).collect());
        assert_eq!(hi.cmp_read(&ints(&[104, 105])), Equal);
        assert_eq!(ints(&[104, 105]).cmp_read(&hi), Equal);
        assert_eq!(hi.cmp_read(&Value::Vector(vec![104.0, 105.0])), Equal);
        assert_eq!(hi.cmp_read(&ints(&[104, 106])), Less);
        assert_eq!(ints(&[104]).cmp_read(&hi), Less);
        assert_eq!(hi.cmp_read(&ints(&[])), Greater);
        assert_eq!(list(hi.clone()).cmp_read(&list(ints(&[104, 105]))), Equal);
        // Not bytes: the two order by kind, as the order takes them.
        for other in [ints(&[104, 256]), ints(&[-1]), Value::Vector(vec![104.5])] {
            assert_eq!(hi.cmp_read(&other), hi.cmp_value(&other), "{other:?}");
        }
        assert_eq!(hi.cmp_value(&ints(&[104, 105])), Less);
        for a in edges() {
            for b in edges() {
                let reads = matches!(
                    (&a, &b),
                    (Value::Timestamp(_), Value::Text(_))
                        | (Value::Text(_), Value::Timestamp(_))
                        | (Value::List(_), Value::List(_))
                        | (Value::Object(_), Value::Object(_))
                        | (Value::Bytes(_), Value::List(_) | Value::Vector(_))
                        | (Value::List(_) | Value::Vector(_), Value::Bytes(_))
                );
                if !reads {
                    assert_eq!(a.cmp_read(&b), a.cmp_value(&b), "{a:?} {b:?}");
                }
                assert_eq!(a.cmp_read(&b), b.cmp_read(&a).reverse(), "{a:?} {b:?}");
            }
        }
    }

    /// A group, a distinct count, a facet, a sketch and every index tell
    /// values apart by their keys (`codec::encode_key`): two keys are equal
    /// exactly where `cmp_value` finds the values equal -- a whole float
    /// and the int it is, a time and its milliseconds' int, bytes and the
    /// text of them, every `NaN`, both zeros, in a list or an object too.
    /// A json field's `3` and `3.0` were two groups `=` found one.
    #[test]
    fn two_keys_are_equal_where_the_values_are() {
        // A sparse vector holding a `NaN` or a zero, which no field holds
        // (`sparse::normalise`), is keyed as it is.
        let vs: Vec<Value> = edges()
            .into_iter()
            .filter(|v| !matches!(v, Value::Sparse(_, e) if e.iter().any(|w| w.1.is_nan() || w.1 == 0.0)))
            .collect();
        let key = |v: &Value| {
            let mut out = Vec::new();
            crate::codec::encode_key(&mut out, v);
            out
        };
        let keys: Vec<Vec<u8>> = vs.iter().map(key).collect();
        for (a, ka) in vs.iter().zip(&keys) {
            for (b, kb) in vs.iter().zip(&keys) {
                assert_eq!(ka == kb, a.cmp_value(b) == Equal, "{a:?} {b:?}");
            }
        }
        for i in ints() {
            for f in floats() {
                let same = Value::Int(i).cmp_value(&Value::Float(f)) == Equal;
                let (ki, kf) = (key(&Value::Int(i)), key(&Value::Float(f)));
                assert_eq!(ki == kf, same, "{i} {f:e}");
                assert_eq!(key(&Value::Timestamp(i)) == kf, same, "{i} {f:e}");
            }
        }
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

    /// `held` takes exactly the numbers a field holds: every `f32` but an
    /// infinity and a `NaN`, and in an `f16` field every one whose `f16` is
    /// a number (`codec::f16_from_f32`, which stores it), across every sign
    /// and exponent at each end of the significand; one component anywhere
    /// in a vector fails the whole, and `unheld` names the first.
    #[test]
    fn held_takes_the_numbers_a_field_holds() {
        use crate::codec::{f16_from_f32, f32_from_f16};
        let ends = [
            0, 1, 2, 0x3f_ffff, 0x40_0000, 0x7f_efff, 0x7f_f000, 0x7f_fffe, 0x7f_ffff,
        ];
        for top in 0..=0x1ffu32 {
            for low in ends {
                let x = f32::from_bits(top << 23 | low);
                assert_eq!(held(&[x], VecPrec::F32), x.is_finite(), "{x:e}");
                let half = f32_from_f16(f16_from_f32(x)).is_finite();
                assert_eq!(held(&[x], VecPrec::F16), half, "{x:e} as an f16");
            }
        }
        // The f32 just under 65 520 rounds to 65 504; 65 520 to the infinity.
        let under = f32::from_bits(65_520f32.to_bits() - 1);
        for (x, f32s, f16s) in [
            (f32::MAX, true, false),
            (f32::from_bits(1), true, true),
            (65_504.0, true, true),
            (under, true, true),
            (65_520.0, true, false),
            (f32::INFINITY, false, false),
            (f32::NAN, false, false),
        ] {
            for x in [x, -x] {
                assert_eq!(held(&[x], VecPrec::F32), f32s, "{x:e}");
                assert_eq!(held(&[x], VecPrec::F16), f16s, "{x:e} as an f16");
            }
        }
        assert_eq!(f32_from_f16(f16_from_f32(under)), 65_504.0);
        for n in 1..=13 {
            for at in 0..n {
                for bad in [f32::INFINITY, f32::NAN, -f32::INFINITY] {
                    let mut v = vec![0.5f32; n];
                    v[at] = bad;
                    // Another after it: the first is the one named.
                    if at + 1 < n {
                        v[n - 1] = f32::NAN;
                    }
                    assert!(!held(&v, VecPrec::F32), "{v:?}");
                    let said = unheld(&v, VecPrec::F32).to_string();
                    let at = format!("element {at}: a vector's component is a number at most");
                    assert!(said.contains(&at), "{said}");
                }
                assert!(held(&vec![-f32::MAX; n], VecPrec::F32));
                assert!(!held(&vec![-65_520.0; n], VecPrec::F16));
            }
        }
        assert!(held(&[], VecPrec::F32) && held(&[], VecPrec::F16));
    }
}

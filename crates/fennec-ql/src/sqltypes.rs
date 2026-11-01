//! The fennecdb type a SQL column's declared type is: PostgreSQL's types by
//! name, and any other declaration by SQLite's affinity rules. One table
//! for both readers of a SQL schema -- `fennec import`, which maps a
//! PostgreSQL column by its type's name and a SQLite column by its
//! declaration, and FennecQL's reading of SQL's `create table` -- so the
//! two cannot part.

use fennec_core::value::{DataType, VecPrec};

/// What a declared type is in fennecdb.
#[derive(Debug, Clone, PartialEq)]
pub enum Mapped {
    /// A field's type, and whether its text compares by its case fold, as
    /// `citext`'s does (a `nocase` field).
    Type(DataType, bool),
    /// No fennecdb type keeps the column's values, for this reason.
    Not(Why),
}

/// Why a declared type has no fennecdb type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// `numeric`, `decimal`, `money`: fennecdb has no decimal.
    Decimal,
    /// `vector`, `halfvec` or `sparsevec` with no dimension.
    NoDimension,
    /// No type, or one no rule reads: SQLite's `NUMERIC` affinity, which
    /// takes any value.
    Untyped,
}

/// PostgreSQL's type names, a group each of what [`postgres`] makes them,
/// in its order: `|` between two names, `;` between two groups. One text
/// read by a loop, where a `match` compared each name in code of its own
/// (0.6 KB more of the browser module). The last group is PostgreSQL's own types
/// that SQLite's rules would read otherwise -- `interval` holds `INT` --
/// and fennecdb has none of.
const POSTGRES: &str = "bool|boolean;\
    smallint|int2|integer|int|int4|bigint|int8;\
    real|float4|double precision|float8|float;\
    text|varchar|character varying|char|character|bpchar|name|uuid;\
    citext;\
    bytea;\
    date|timestamp|timestamptz|timestamp with time zone|timestamp without time zone;\
    json|jsonb;\
    point;\
    vector;\
    halfvec;\
    sparsevec;\
    numeric|decimal|money;\
    interval|time|timetz|time with time zone|time without time zone|inet|cidr|macaddr|\
    xml|tsvector|tsquery|bit|bit varying|varbit|box|line|lseg|path|polygon|circle|\
    int4range|int8range|numrange|tsrange|tstzrange|daterange|geometry|geography|oid";

/// PostgreSQL's type `name` -- lowercased, its words one space apart, its
/// modifiers apart in `args`: `varchar(80)` is `varchar` and 80, `vector(384)`
/// `vector` and 384, `timestamp(3) with time zone` `timestamp with time
/// zone` and 3 -- or `None` for a name that is none of PostgreSQL's. A
/// length is not held, as SQLite holds none, and a smaller integer or
/// float is the one fennecdb has.
pub fn postgres(name: &str, args: &[i64]) -> Option<Mapped> {
    use DataType::*;
    let group = POSTGRES
        .split(';')
        .position(|g| g.split('|').any(|n| n == name))?;
    let dim = args.first().copied().filter(|&d| d > 0).map(|d| d as usize);
    let ty = [Bool, Int, Float, Text, Text, Bytes, Timestamp, Json, Geo];
    Some(match (group, dim) {
        (0..=8, _) => Mapped::Type(ty[group].clone(), group == 4),
        (9, Some(d)) => Mapped::Type(Vector(d, VecPrec::F32), false),
        (10, Some(d)) => Mapped::Type(Vector(d, VecPrec::F16), false),
        (11, Some(d)) => Mapped::Type(Sparse(d), false),
        (9..=11, None) => Mapped::Not(Why::NoDimension),
        (12, _) => Mapped::Not(Why::Decimal),
        _ => Mapped::Not(Why::Untyped),
    })
}

/// A declared type as SQLite reads it -- its affinity, by the words in it
/// -- with the types fennecdb has besides: a boolean, and a time.
pub fn sqlite(decl: &str) -> Mapped {
    let d = decl.trim_ascii().to_ascii_uppercase();
    // A window of bytes compared: `str::contains` was the standard
    // library's two-way searcher and a slice's panic, 5.5 KB of the
    // browser module.
    let has = |k: &str| d.as_bytes().windows(k.len()).any(|w| w == k.as_bytes());
    let ty = |t: DataType| Mapped::Type(t, false);
    if has("BOOL") {
        return ty(DataType::Bool);
    }
    if has("DATETIME") || has("TIMESTAMP") || d == "DATE" {
        return ty(DataType::Timestamp);
    }
    if has("INT") {
        return ty(DataType::Int);
    }
    if has("CHAR") || has("CLOB") || has("TEXT") {
        return ty(DataType::Text);
    }
    if has("BLOB") {
        return ty(DataType::Bytes);
    }
    if has("REAL") || has("FLOA") || has("DOUB") {
        return ty(DataType::Float);
    }
    if has("DEC") || has("NUMERIC") || has("MONEY") {
        return Mapped::Not(Why::Decimal);
    }
    Mapped::Not(Why::Untyped)
}

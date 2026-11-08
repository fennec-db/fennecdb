//! A REST surface derived from the schema.
//!
//! Paths are collection names and the query string is the filter --
//! PostgREST's `?field=op.value` pattern. The translation always ends in a
//! [`Statement`]: the HTTP endpoint opens no separate execution path, it
//! arrives where FennecQL arrives.
//!
//! ```text
//! GET    /                       version + collection list
//! GET    /collections            schemas
//! GET    /<name>?select=a,b&year=gte.2024&order=year.desc&limit=10
//! GET    /<name>?...&count       number of matching rows
//! POST   /<name>                 body: {...} or [{...}]
//! PATCH  /<name>?<filter>        body: {...}
//! PATCH  /<name>/all             body: {...}   (unfiltered, deliberately)
//! DELETE /<name>?<filter>
//! DELETE /<name>/all
//! GET    /<name>/<id>            the row, its tag in `ETag`
//! PATCH  /<name>/<id>            body: {...}   (`If-Match`, `If-None-Match`)
//! PUT    /<name>/<id>            body: {...}   the row written whole
//! DELETE /<name>/<id>
//! POST   /<name>/near            body: {"field":..,"vector":[..],"limit":..}
//! POST   /query                  body: {"query":"<FennecQL>","params":[..]}
//! ```
//!
//! **Why is a vector a POST?** A 768-dimensional embedding does not fit in a
//! query string; squeezing it into a URL (base64, truncation) is both
//! unreadable and runs into URL ceilings in proxies and servers. Since
//! PostgREST's model does not cover fennecdb's main feature, `near` is its own
//! endpoint.

use crate::http::{Method, Request, Response};
use fennec_core::json;
use fennec_core::prelude::*;
use fennec_core::query::Pick;
use std::sync::Arc;

/// Query keys that are read as clauses rather than as filters. A field with
/// the same name cannot be filtered over HTTP (the FennecQL and `fennec-server` paths
/// are unaffected).
const RESERVED: [&str; 9] = [
    "select", "order", "limit", "offset", "count", "where", "lookup", "group", "facet",
];

/// On the subscription endpoint `since` is a clause as well. It is a
/// separate list so that a field named `since` stays filterable on the
/// **other** endpoints.
const RESERVED_STREAM: [&str; 8] = [
    "select", "order", "limit", "offset", "count", "where", "since", "lookup",
];

pub struct Routed {
    pub statement: Statement,
    /// The shape of the response body becomes clear during translation.
    pub shape: Shape,
    /// The id of the row `/<name>/<id>` names, whose tag its preconditions
    /// are held to (`crate::conditional`); `None` on every other route.
    pub row: Option<i64>,
}

pub enum Shape {
    /// Row array: `[{...}, {...}]`
    Rows,
    /// `{"count": N}`
    Count,
    /// `{"<name>": N}` -- write counter
    Affected(&'static str, u16),
    /// A write's rows (`?returning=`), under its status: `POST`'s 201.
    Written(u16),
    /// `{"fennecdb": "...", "collections": [...]}`
    Info,
    /// Schema list
    Schemas,
    /// One row, `GET /<name>/<id>`: the object alone, and its tag.
    Row,
}

/// Turns a request into a statement. It only consults the database for schemas.
pub fn route(db: &Database, req: &Request) -> Result<Routed> {
    let seg = req.segments();
    match (req.method, seg.as_slice()) {
        (Method::Get | Method::Head, []) => Ok(Routed {
            statement: Statement::ListCollections,
            shape: Shape::Info,
            row: None,
        }),
        (Method::Get | Method::Head, ["collections"]) => Ok(Routed {
            statement: Statement::ListCollections,
            shape: Shape::Schemas,
            row: None,
        }),
        (Method::Get | Method::Head, [name]) => {
            let schema = &db.collection(name)?.schema;
            let sel = select_from_query(db, schema, req)?;
            let shape = if sel.count { Shape::Count } else { Shape::Rows };
            Ok(Routed {
                statement: Statement::Select(sel),
                shape,
                row: None,
            })
        }
        (Method::Post, [name, "near"]) => {
            let schema = &db.collection(name)?.schema;
            Ok(Routed {
                statement: Statement::Select(near_from_body(schema, req)?),
                shape: Shape::Rows,
                row: None,
            })
        }
        (Method::Post, [name]) => route_put(&db.collection(name)?.schema, req),
        // `PATCH /<name>` and `/<name>/all`, the rows a filter or none
        // picks, and `PATCH` and `PUT /<name>/<id>`, one row by its id.
        (Method::Patch | Method::Put, [name]) | (Method::Patch | Method::Put, [name, _]) => {
            let row = match seg.as_slice() {
                [_, id] if *id != "all" => Some(row_id(req, id)?),
                _ => None,
            };
            let schema = &db.collection(name)?.schema;
            let filter = match row {
                Some(id) => Some(by_id(id)),
                None => require_filter(schema, req, seg.len() == 2, "an update")?,
            };
            // The body read in one place for the three: read in each arm,
            // `document` was no longer inlined here, and a write by a
            // filter ran 20 instructions more.
            let set = document(schema, req)?;
            if let (Method::Put, Some(id)) = (req.method, row) {
                return put_row(name, id, set, req);
            }
            if set.is_empty() {
                return Err(Error::Query("empty body: no field to update".into()));
            }
            Ok(Routed {
                statement: Statement::Update {
                    collection: name.to_string(),
                    set,
                    filter,
                    require: None,
                    pick: None,
                },
                shape: Shape::Affected("updated", 200),
                row,
            })
        }
        (Method::Delete, [name]) | (Method::Delete, [name, "all"]) => {
            let schema = &db.collection(name)?.schema;
            let all = seg.len() == 2;
            let filter = require_filter(schema, req, all, "a delete")?;
            Ok(Routed {
                statement: Statement::Delete {
                    collection: name.to_string(),
                    filter,
                    require: None,
                    pick: None,
                },
                shape: Shape::Affected("deleted", 200),
                row: None,
            })
        }
        // `/<name>/<id>`: the row by its id. A second segment that is no
        // id is no path, as it was before these routes: `all`, `near` and
        // `changes` are the routes above and the subscription's.
        (Method::Get | Method::Head, [name, id]) => {
            let id = row_id(req, id)?;
            db.collection(name)?;
            Ok(Routed {
                statement: row_select(name, id),
                shape: Shape::Row,
                row: Some(id),
            })
        }
        (Method::Delete, [name, id]) => {
            let id = row_id(req, id)?;
            db.collection(name)?;
            Ok(Routed {
                statement: Statement::Delete {
                    collection: name.to_string(),
                    filter: Some(by_id(id)),
                    require: None,
                    pick: None,
                },
                shape: Shape::Affected("deleted", 200),
                row: Some(id),
            })
        }
        _ => Err(unrouted(req)),
    }
}

/// `PUT /<name>/<id>`: the body, `doc`, written as the row of that id,
/// whole -- a `put`, which writes over a row -- the body's `id`, where it
/// names one, the path's. Under `If-None-Match: *` it is made only where
/// the id is free: an `insert`, which a row the writer is not shown
/// refuses as it refuses `POST`'s (409), never writing over it.
#[inline(never)]
fn put_row(name: &str, id: i64, mut doc: Vec<(String, Expr)>, req: &Request) -> Result<Routed> {
    match doc.iter().find(|(k, _)| k == "id") {
        Some((_, Expr::Lit(Value::Int(n)))) if *n == id => {}
        Some((_, v)) => {
            return Err(Error::Query(format!(
                "the body's `id` is {}, the path's {id}",
                match v {
                    Expr::Lit(v) => json::to_string(v),
                    _ => "not a number".into(),
                }
            )))
        }
        None => doc.push(("id".to_string(), Expr::Lit(Value::Int(id)))),
    }
    let insert = crate::conditional::Preconditions::of(req).only_if_absent();
    Ok(Routed {
        statement: Statement::Put {
            collection: name.to_string(),
            docs: vec![doc],
            docs_param: None,
            insert,
            if_absent: false,
            else_set: None,
            require: None,
            pick: None,
        },
        shape: Shape::Affected("written", 200),
        row: Some(id),
    })
}

/// [`route`]'s refusal of a path no route takes.
fn unrouted(req: &Request) -> Error {
    Error::NotFound(format!("path `{}`", req.path))
}

/// The id `/<name>/<id>` names: digits alone, as a row's id is written,
/// else no path a route takes. The row is the route's whole answer, so a
/// query string -- a `select`, a filter -- is refused rather than passed
/// over: its tag is the whole row's.
fn row_id(req: &Request, raw: &str) -> Result<i64> {
    // `num::parse_i64` reads the digits: `str::parse`'s was 300 bytes of
    // fennec-server more for this one call.
    let id = match raw.bytes().all(|b| b.is_ascii_digit()) {
        true => fennec_core::num::parse_i64(raw),
        false => None,
    };
    let Some(id) = id else {
        return Err(unrouted(req));
    };
    if !req.query.is_empty() {
        return Err(Error::Query(
            "`/<name>/<id>` is the row: it takes no query string".into(),
        ));
    }
    Ok(id)
}

/// The collection `/<name>/<id>` names, where the path is one: read
/// before the lock, for a request's preconditions, which no other route
/// takes ([`route`] then gives the id).
pub fn row_name(req: &Request) -> Option<&str> {
    let mut seg = req.path.split('/').filter(|s| !s.is_empty());
    let (name, id) = (seg.next()?, seg.next()?);
    let digits = id.bytes().all(|b| b.is_ascii_digit());
    (seg.next().is_none() && digits).then_some(name)
}

/// `id = <id>`.
fn by_id(id: i64) -> Expr {
    Expr::Cmp(
        CmpOp::Eq,
        Box::new(Expr::Field("id".to_string())),
        Box::new(Expr::Lit(Value::Int(id))),
    )
}

/// `get <name> where id = <id>`: the row `GET /<name>/<id>` answers, and
/// the one a write to that path holds its preconditions to.
pub fn row_select(name: &str, id: i64) -> Statement {
    Statement::Select(Select {
        collection: name.to_string(),
        filter: Some(by_id(id)),
        ..Default::default()
    })
}

/// Whether [`route`]'s refusal `e` of `req` was read from the database it
/// routed against -- a collection, a field or a type another write made,
/// dropped or changed -- rather than from the request alone: a path no
/// route takes, a body that is no UTF-8 or no JSON, which no write changes.
/// A write refused under the write lock by what another write left waits
/// for that write's fsync before it is answered, under `--sync always`
/// (`crate::refusal_durability`); one refused by its request alone tells
/// nothing a power loss could take back, and waited for an fsync it never
/// read -- `POST /<c>` with `{not json` stood behind another client's
/// write. A refusal this cannot place counts as read: a wait too many, never
/// one too few. Asked on the refusal's path alone, the body parsed again.
pub fn refusal_read(req: &Request, e: &Error) -> bool {
    if *e == unrouted(req) {
        return false;
    }
    let refused = match body_str(req) {
        Ok(body) => json::parse_documents_json(body, &[]).err(),
        Err(not_text) => Some(not_text),
    };
    refused.as_ref() != Some(e)
}

/// `POST /<name>`: the body's documents as the `insert` that makes them,
/// read against `schema` alone -- the collection's, which a server asks
/// for under the read lock and reads the body against with no lock held.
pub fn route_put(schema: &Schema, req: &Request) -> Result<Routed> {
    let mut statement = put_from_body(schema, req)?;
    // `?returning=id` or `?returning=*`: the documents made, ids and
    // all, rather than their count -- what `insert ... returning`
    // answers.
    let returning = req.query.iter().find(|(k, _)| k == "returning");
    let shape = match (returning, &mut statement) {
        (Some((_, v)), Statement::Put { pick, .. }) => {
            let list = match v.trim() {
                "*" => None,
                v => Some(projection(schema, v)?),
            };
            *pick = Some(Box::new(Pick {
                returning: Some(list),
                ..Pick::default()
            }));
            Shape::Written(201)
        }
        _ => Shape::Affected("inserted", 201),
    };
    Ok(Routed {
        statement,
        shape,
        row: None,
    })
}

/// An unfiltered `PATCH`/`DELETE` covers the whole collection. It is very
/// easy to do by accident and impossible to undo: we require an explicit
/// path (`/<name>/all`). It is a separate *path* because a key like
/// `?all=true` would clash with a field named `all`.
fn require_filter(schema: &Schema, req: &Request, all: bool, verb: &str) -> Result<Option<Expr>> {
    let filter = filter_from_query(schema, req)?;
    match (&filter, all) {
        (Some(_), true) => Err(Error::Query(format!(
            "`/all` takes no filter: either give a filter or use `/all` ({verb})"
        ))),
        (None, false) => Err(Error::Query(format!(
            "{verb} without a filter covers the whole collection; if deliberate, `/{}/all`",
            schema.name
        ))),
        _ => Ok(filter),
    }
}

// --------------------------------------------------------------- reading

fn select_from_query(db: &Database, schema: &Schema, req: &Request) -> Result<Select> {
    let lookup = lookup_from_query(db, schema, req)?;
    // Every level's prefix is reserved, not just the first: a chained key
    // read as a condition on the parent would be a silent wrong answer.
    let skip: Vec<String> = lookup
        .iter()
        .flat_map(|l| l.chain())
        .map(|s| format!("{}.", s.collection))
        .collect();
    let mut sel = Select {
        collection: schema.name.clone(),
        filter: filter_with(schema, req, &RESERVED, &skip)?,
        lookup,
        ..Default::default()
    };
    for (k, v) in &req.query {
        match k.as_str() {
            // `select=status,sum(total),count(*)`, `select=sym,px*qty as n`:
            // a list with a call or a name given in it is FennecQL's, read
            // by its parser -- one with an aggregate aggregates, as
            // FennecQL's does.
            "select" if v.contains('(') || v.contains(" as ") => {
                let l = fennec_ql::parse_select_list(v)
                    .map_err(|e| Error::Query(format!("`select`: {e}")))?;
                (sel.aggregate, sel.computed) = (l.aggregate, l.computed);
                if sel.aggregate.is_empty() {
                    sel.project = l.project;
                }
            }
            "select" => sel.project = Some(projection(schema, v)?),
            // `group=symbol,bucket(at, 1m)`: FennecQL's list, read by its
            // parser; an unknown field is the engine's to refuse.
            "group" => {
                sel.group = fennec_ql::parse_group_list(v)
                    .map_err(|e| Error::Query(format!("`group`: {e}")))?
            }
            "order" => sel.order = order(schema, v)?,
            "limit" => sel.limit = Some(Count::N(number(v, "limit")?)),
            "offset" => sel.offset = Count::N(number(v, "offset")?),
            "count" => sel.count = truthy(v),
            // `facet=brand top 5,color`: FennecQL's list, read by its parser.
            "facet" => {
                sel.facets = fennec_ql::parse_facet_list(v)
                    .map_err(|e| Error::Query(format!("`facet`: {e}")))?;
                for f in &sel.facets {
                    field(schema, &f.field)?;
                }
            }
            _ => {}
        }
    }
    sel.check()?;
    // Before `scoped()` ANDs a token's rules in, as the parser does.
    sel.split_facets()?;
    Ok(sel)
}

/// `?lookup=reviews&reviews.on=product_id&reviews.limit=3&reviews.stars=gte.4`
///
/// The collection is named once and everything prefixed with it configures
/// the clause -- `on`, `parent`, `select`, `order`, `limit`, `offset`,
/// `required`, `where`, and any field name as a condition. Prefixing keeps
/// the two sides apart in a query string the way the clause's position does
/// in FennecQL, and it is the shape PostgREST already uses for an embedded
/// resource's filters.
///
/// `lines.on=org,order&lines.parent=org,id` is a key over several fields,
/// FennecQL's `on (org, order) = (org, id)`: the two lists pair field by
/// field.
///
/// `?lookup=orders,lines&orders.on=shop_id&lines.on=order_id` chains: the
/// list is read left to right, each name binding to the one before it, and
/// each level keeps its own prefix. A comma-separated list is the same
/// ordering FennecQL gets from position, written where a query string has no
/// position to use.
fn lookup_from_query(db: &Database, parent: &Schema, req: &Request) -> Result<Option<Lookup>> {
    let Some((_, spec)) = req.query.iter().find(|(k, _)| k == "lookup") else {
        return Ok(None);
    };
    let names: Vec<&str> = spec
        .split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .collect();
    if names.is_empty() {
        return Err(Error::Query("`lookup` is empty".into()));
    }
    // Built innermost-last, then linked from the back, so each level owns the
    // one below it.
    let mut levels: Vec<Lookup> = Vec::with_capacity(names.len());
    let mut above = parent;
    for name in &names {
        let child = &db.collection(name)?.schema;
        // Off groups -- the query's, or a level's that folds -- a key
        // names a column of their list.
        let folded = match levels.last() {
            Some(l) => l.folds(),
            None => (req.query.iter()).any(|(k, v)| {
                k == "group"
                    || (k == "select"
                        && v.contains('(')
                        && fennec_ql::parse_select_list(v).is_ok_and(|l| !l.aggregate.is_empty()))
            }),
        };
        levels.push(lookup_level(name, above, child, folded, req)?);
        above = child;
    }
    let mut chain: Option<Box<Lookup>> = None;
    for mut l in levels.into_iter().rev() {
        l.next = chain;
        chain = Some(Box::new(l));
    }
    Ok(chain.map(|b| *b))
}

/// Whether the level `prefix` names groups its list: its `order` names
/// columns.
fn folds(req: &Request, prefix: &str) -> bool {
    req.query
        .iter()
        .any(|(k, _)| k.strip_prefix(prefix) == Some("group"))
}

/// A grouped level's `order`: each key a column of its list -- a name, or
/// an aggregate as written -- with `.asc` or `.desc`.
fn order_of_columns(raw: &str) -> Vec<Sort> {
    let mut out = Vec::new();
    for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (name, asc) = match part.rsplit_once('.') {
            Some((n, "desc")) => (n, false),
            Some((n, "asc")) => (n, true),
            _ => (part, true),
        };
        let name = match name.split_once('(') {
            Some((f, rest)) => match (f.to_ascii_lowercase().as_str(), rest) {
                ("count", "*)" | ")") => "count".to_string(),
                (f, rest) => format!("{f}({rest}"),
            },
            None => name.to_string(),
        };
        out.push(Sort::new(name, asc));
    }
    out
}

/// One level of the chain: every key prefixed with its collection's name.
/// `folded`: the level above answers groups, whose columns a key names.
fn lookup_level(
    name: &str,
    parent: &Schema,
    child: &Schema,
    folded: bool,
    req: &Request,
) -> Result<Lookup> {
    let prefix = format!("{name}.");
    let mut l = Lookup {
        collection: name.to_string(),
        ..Default::default()
    };
    let (mut on, mut above) = (Vec::new(), Vec::new());
    let mut parts: Vec<Expr> = Vec::new();
    for (key, raw) in &req.query {
        let Some(k) = key.strip_prefix(&prefix) else {
            continue;
        };
        match k {
            // `on=org,order&parent=org,id`: a key over several fields, as
            // FennecQL's `on (org, order) = (org, id)`.
            "on" => on = fields_of(child, raw)?,
            // A key off groups names a column of the list above, which
            // the engine checks against it.
            "parent" if folded => above = raw.split(',').map(|f| f.trim().to_string()).collect(),
            "parent" => above = fields_of(parent, raw)?,
            // `orders.select=count(*),sum(total)`: a list that aggregates
            // each parent's children, read by FennecQL's parser as the
            // query's own is.
            "select" if raw.contains('(') || raw.contains(" as ") => {
                let list = fennec_ql::parse_select_list(raw)
                    .map_err(|e| Error::Query(format!("`{name}.select`: {e}")))?;
                if !list.computed.is_empty() || !list.marks.is_empty() {
                    return Err(Error::Query(format!(
                        "`{name}.select` selects the children's fields, or aggregates of them"
                    )));
                }
                (l.project, l.aggregate) = (list.project, list.aggregate);
            }
            "select" => l.project = Some(projection(child, raw)?),
            "group" => {
                l.group = fennec_ql::parse_group_list(raw)
                    .map_err(|e| Error::Query(format!("`{name}.group`: {e}")))?
            }
            // Over groups a key names a column; the engine checks it.
            "order" if folds(req, &prefix) => l.order = order_of_columns(raw),
            "order" => l.order = order(child, raw)?,
            "limit" => l.limit = Some(Count::N(number(raw, "limit")?)),
            "offset" => l.offset = Count::N(number(raw, "offset")?),
            "required" => l.required = truthy(raw),
            "where" => parts.push(parse_expr(child, raw)?),
            other => parts.push(condition(child, other, raw)?),
        }
    }
    if on.is_empty() {
        return Err(Error::Query(format!(
            "`lookup={name}` needs `{name}.on=<field>`: the child field holding the key"
        )));
    }
    // As FennecQL's: the parent's `id` unless named, and `Select::check`
    // holds the two sides to as many.
    if above.is_empty() {
        above.push("id".to_string());
    }
    (l.on, l.parent) = (on, above);
    l.filter = parts
        .into_iter()
        .reduce(|a, b| Expr::And(Box::new(a), Box::new(b)));
    Ok(l)
}

/// A comma-separated list of [`field_of`]s.
fn fields_of(schema: &Schema, raw: &str) -> Result<Vec<String>> {
    raw.split(',').map(|f| field_of(schema, f)).collect()
}

/// A field name from the query string, checked against the schema so an
/// unknown one is an error here rather than at execution. `id` is not a
/// declared field but is a legal key on either side.
fn field_of(schema: &Schema, raw: &str) -> Result<String> {
    let name = raw.trim();
    if name == "id" || schema.field(name).is_some() {
        return Ok(name.to_string());
    }
    Err(Error::NotFound(format!("field `{}.{name}`", schema.name)))
}

fn projection(schema: &Schema, raw: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let name = part.trim();
        if name.is_empty() {
            continue;
        }
        field(schema, name)?;
        out.push(name.to_string());
    }
    if out.is_empty() {
        return Err(Error::Query("`select` is empty".into()));
    }
    Ok(out)
}

/// `order=year.desc,title` -> `order year desc, title`. A collation's name
/// among the modifiers puts a text field in its language's order:
/// `order=name.tr.desc` is `order name collate tr desc`. Over groups a key
/// may name an aggregate of the list, `order=sum(total).desc`; the engine
/// checks it against the list.
fn order(schema: &Schema, raw: &str) -> Result<Vec<Sort>> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // A field's name holds no dot, but a path into a json field does
        // (`meta.source.rank.desc`): the modifiers are the words after the
        // first that name a direction or a collation, read from the end, and
        // the name what is before them -- a field, which no dot follows
        // but a modifier's, or a path into a json field.
        let mut words: Vec<&str> = part.split('.').collect();
        let (mut asc, mut collate) = (true, None);
        while words.len() > 1 {
            let w = words[words.len() - 1];
            match w {
                "asc" => asc = true,
                "desc" => asc = false,
                w => match Collation::named(w) {
                    Some(c) => collate = Some(c),
                    None if schema.path_of(&words.join(".")).ok().flatten().is_some() => break,
                    None => {
                        return Err(Error::Query(format!(
                            "`order`: `{w}` in `{part}` is not asc, desc or a collation (tr)"
                        )))
                    }
                },
            }
            words.pop();
        }
        let joined = words.join(".");
        let name = joined.as_str();
        let name = if let Some((f, rest)) = name.split_once('(') {
            // The function's name folds as FennecQL folds it; the field's does not.
            match (f.to_ascii_lowercase().as_str(), rest) {
                ("count", "*)" | ")") => "count".to_string(),
                (f, rest) => format!("{f}({rest}"),
            }
        } else {
            field(schema, name)?;
            name.to_string()
        };
        out.push(Sort {
            field: name,
            asc,
            collate,
            expr: None,
        });
    }
    Ok(out)
}

fn number(raw: &str, what: &str) -> Result<usize> {
    raw.trim()
        .parse()
        .map_err(|_| Error::Query(format!("`{what}` expects a number, got `{raw}`")))
}

/// `?count`, `?count=true`, `?count=1` -- all of them mean on.
fn truthy(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "" | "true" | "1" | "yes" | "exact"
    )
}

// ---------------------------------------------------------------- filter

/// Joins every condition in the query string with `and`.
fn filter_from_query(schema: &Schema, req: &Request) -> Result<Option<Expr>> {
    filter_with(schema, req, &RESERVED, &[])
}

/// `skip` holds each `lookup` level's `<collection>.` prefix: those keys
/// configure a child and must not be read as conditions on the parent.
fn filter_with(
    schema: &Schema,
    req: &Request,
    reserved: &[&str],
    skip: &[String],
) -> Result<Option<Expr>> {
    let mut parts: Vec<Expr> = Vec::new();
    for (key, raw) in &req.query {
        if skip.iter().any(|p| key.starts_with(p)) {
            continue;
        }
        if reserved.contains(&key.as_str()) {
            if key == "where" {
                parts.push(parse_expr(schema, raw)?);
            }
            continue;
        }
        parts.push(condition(schema, key, raw)?);
    }
    Ok(parts
        .into_iter()
        .reduce(|a, b| Expr::And(Box::new(a), Box::new(b))))
}

/// `where=` is a free FennecQL expression, so that conditions which do not fit
/// the query-string pattern (function calls, `or` groups) can be expressed
/// too. Only the condition part is taken; no other clause is accepted.
/// A list of numbers a json field's path is compared with is read as
/// written (`exactly_for`), as a statement's text is.
fn parse_expr(schema: &Schema, raw: &str) -> Result<Expr> {
    let text = format!("get {} where {raw}", schema.name);
    let parsed = fennec_ql::parse_one(&text).and_then(|stmt| {
        match stmt.reads_vectors() && fennec_core::engine::exactly_for(schema, &stmt).text {
            true => fennec_ql::parse_exact(&text).map(|mut s| s.remove(0)),
            false => Ok(stmt),
        }
    });
    let stmt = parsed.map_err(|e| Error::Query(format!("`where` could not be parsed: {e}")))?;
    let Statement::Select(sel) = stmt else {
        return Err(Error::Query(
            "`where` must be a condition expression".into(),
        ));
    };
    if sel.project.is_some()
        || sel.near.is_some()
        || !sel.order.is_empty()
        || sel.limit.is_some()
        || !sel.offset.is_zero()
        || sel.count
    {
        return Err(Error::Query(
            "`where` takes only a condition, no clauses".into(),
        ));
    }
    sel.filter
        .ok_or_else(|| Error::Query("`where` is empty".into()))
}

/// `field=op.value` -- when the op is not recognised the whole value counts
/// as an equality. (Someone writing `?name=v1.2` is not looking for a `v1`
/// operator.)
fn condition(schema: &Schema, name: &str, raw: &str) -> Result<Expr> {
    let ty = field(schema, name)?;
    let (op, rest) = split_op(raw);
    if op == "not" {
        let inner = condition(schema, name, rest)?;
        return Ok(Expr::Not(Box::new(inner)));
    }
    let f = || Box::new(Expr::Field(name.to_string()));

    if op == "is" {
        return match rest.trim().to_ascii_lowercase().as_str() {
            "null" => Ok(Expr::IsNull(f())),
            other => Err(Error::Query(format!(
                "`is` only takes `null`, got `{other}`"
            ))),
        };
    }

    if matches!(ty, DataType::List(_)) && !matches!(op, "has" | "in") {
        return Err(Error::Query(format!(
            "`{name}` is a list: use `{name}=has.<value>` to look for an element"
        )));
    }

    Ok(match op {
        "eq" => Expr::Cmp(CmpOp::Eq, f(), boxed(lit(rest, ty, name)?)),
        "neq" | "ne" => Expr::Cmp(CmpOp::Ne, f(), boxed(lit(rest, ty, name)?)),
        "lt" => Expr::Cmp(CmpOp::Lt, f(), boxed(lit(rest, ty, name)?)),
        "lte" | "le" => Expr::Cmp(CmpOp::Le, f(), boxed(lit(rest, ty, name)?)),
        "gt" => Expr::Cmp(CmpOp::Gt, f(), boxed(lit(rest, ty, name)?)),
        "gte" | "ge" => Expr::Cmp(CmpOp::Ge, f(), boxed(lit(rest, ty, name)?)),
        "like" => Expr::Like(f(), Box::new(Expr::Lit(Value::Text(rest.to_string())))),
        "has" => Expr::Has(f(), boxed(lit(rest, ty, name)?)),
        "in" => {
            let items = rest.trim().trim_start_matches('(').trim_end_matches(')');
            let mut out = Vec::new();
            for item in items.split(',') {
                out.push(Expr::Lit(lit(item.trim(), ty, name)?));
            }
            if out.is_empty() {
                return Err(Error::Query(format!("`{name}=in.` empty list")));
            }
            Expr::In(f(), out)
        }
        other => {
            return Err(Error::Query(format!(
                "unknown operator `{other}` (field: {name})"
            )))
        }
    })
}

fn boxed(v: Value) -> Box<Expr> {
    Box::new(Expr::Lit(v))
}

const OPS: [&str; 13] = [
    "eq", "neq", "ne", "lt", "lte", "le", "gt", "gte", "ge", "like", "has", "in", "is",
];

/// If the part up to the first dot is a recognised operator, it is split
/// off; otherwise the operator is `eq` and the whole value is preserved.
fn split_op(raw: &str) -> (&str, &str) {
    match raw.split_once('.') {
        Some(("not", rest)) => ("not", rest),
        Some((head, rest)) if OPS.contains(&head) => (head, rest),
        _ => ("eq", raw),
    }
}

/// The query string is always text: the value is parsed according to the
/// field's type. `coerce` does not go from text to number (and must not --
/// we want no silent conversion on the write path), so the parse is
/// explicit here.
fn lit(raw: &str, ty: &DataType, name: &str) -> Result<Value> {
    let bad = |what: &str| Error::Query(format!("`{name}` expects {what}, got `{raw}`"));
    let v = match ty {
        DataType::Bool => match raw.to_ascii_lowercase().as_str() {
            "true" | "t" | "1" => Value::Bool(true),
            "false" | "f" | "0" => Value::Bool(false),
            _ => return Err(bad("a bool")),
        },
        DataType::Int => Value::Int(raw.parse().map_err(|_| bad("an integer"))?),
        // `num::parse_f64`, the same parser the JSON body and FennecQL use,
        // rather than `str::parse`. It is stricter by exactly one thing --
        // it has no spelling for infinity -- which makes the query string
        // agree with the JSON body, where `inf` was never a number either.
        // It also keeps `core`'s 12 KB table of powers of five out of every
        // binary that links this crate, `fennec-server` included.
        DataType::Float => {
            Value::Float(fennec_core::num::parse_f64(raw).ok_or_else(|| bad("a number"))?)
        }
        DataType::Text => Value::Text(raw.to_string()),
        DataType::Bytes => Value::Bytes(raw.as_bytes().to_vec()),
        // Both ISO-8601 and epoch milliseconds are accepted.
        DataType::Timestamp => match raw.parse::<i64>() {
            Ok(ms) => Value::Timestamp(ms),
            Err(_) => Value::Text(raw.to_string()).coerce(&DataType::Timestamp)?,
        },
        DataType::List(inner) => lit(raw, inner, name)?,
        DataType::Vector(..) | DataType::Sparse(_) => {
            return Err(Error::Query(format!(
                "`{name}` is a vector: it cannot be filtered in the query string, use `POST /<collection>/near`"
            )))
        }
        DataType::Geo => {
            return Err(Error::Query(format!(
                "`{name}` is a point: filter it with `where=distance({name}, [lon, lat]) <= metres` \
                 or `where=within({name}, [west, south, east, north])`"
            )))
        }
        // A json value has no type to read the text by: a number, `true`,
        // `false`, `null` or a quoted string as JSON reads it, and any other
        // text as itself -- `?meta.lang=tr`, `?meta.rank=gte.2`.
        DataType::Json => match fennec_core::json::parse(raw) {
            Ok(v @ (Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_))) => v,
            Ok(Value::Text(t)) if raw.starts_with('"') => Value::Text(t),
            _ => Value::Text(raw.to_string()),
        },
    };
    Ok(v)
}

/// Field type; `id` is not in the schema but is queryable.
fn field<'a>(schema: &'a Schema, name: &str) -> Result<&'a DataType> {
    if name == "id" {
        return Ok(&DataType::Int);
    }
    // A path into a json field: `?meta.lang=tr`.
    if let Ok(Some(_)) = schema.path_of(name) {
        return Ok(&DataType::Json);
    }
    schema
        .field(name)
        .map(|f| &f.ty)
        .ok_or_else(|| Error::NotFound(format!("field `{name}` in collection `{}`", schema.name)))
}

// --------------------------------------------------------------- writing

fn body_str(req: &Request) -> Result<&str> {
    std::str::from_utf8(&req.body).map_err(|_| Error::Query("the body is not UTF-8".into()))
}

/// The collection's json fields and points, whose members a body's reader
/// keeps every number of (`json::parse_documents_json`).
fn json_fields(schema: &Schema) -> Vec<&str> {
    (schema.fields.iter())
        .filter(|f| f.ty.keeps_numbers())
        .map(|f| f.name.as_str())
        .collect()
}

fn put_from_body(schema: &Schema, req: &Request) -> Result<Statement> {
    let docs = json::parse_documents_json(body_str(req)?, &json_fields(schema))?;
    if docs.is_empty() {
        return Err(Error::Query("empty body: no document to write".into()));
    }
    let mut out = Vec::with_capacity(docs.len());
    for doc in docs {
        out.push(check_fields(schema, doc)?);
    }
    // `POST` makes documents: one naming an id that is taken is refused
    // (409) rather than written over.
    Ok(Statement::Put {
        collection: schema.name.clone(),
        docs: out,
        insert: true,
        if_absent: false,
        docs_param: None,
        else_set: None,
        require: None,
        pick: None,
    })
}

fn document(schema: &Schema, req: &Request) -> Result<Vec<(String, Expr)>> {
    let mut docs = json::parse_documents_json(body_str(req)?, &json_fields(schema))?;
    if docs.len() != 1 {
        return Err(Error::Query("expected a JSON object".into()));
    }
    check_fields(schema, docs.remove(0))
}

/// Field names are validated against the schema: the engine validates them
/// too, but the error has to be a 400 here rather than a 404, and the
/// message has to carry the field name.
fn check_fields(schema: &Schema, doc: Vec<(String, Value)>) -> Result<Vec<(String, Expr)>> {
    let mut out = Vec::with_capacity(doc.len());
    for (k, v) in doc {
        // A path names a key inside a json field: `{"meta.lang": "en"}`.
        if k != "id" && schema.field(&k).is_none() && !matches!(schema.path_of(&k), Ok(Some(_))) {
            return Err(Error::Query(format!(
                "collection `{}` has no field `{k}`",
                schema.name
            )));
        }
        out.push((k, Expr::Lit(v)));
    }
    Ok(out)
}

// ------------------------------------------------------------------ near

fn near_from_body(schema: &Schema, req: &Request) -> Result<Select> {
    let quick = json::parse_object(body_str(req)?)?;
    let field_name = match quick.iter().find(|(k, _)| k == "field").map(|(_, v)| v) {
        Some(Value::Text(s)) => s.clone(),
        Some(_) => return Err(Error::Query("`field` must be text".into())),
        None => default_vector_field(schema)?,
    };
    let (sparse, point) = match field(schema, &field_name)? {
        DataType::Vector(..) => (false, false),
        DataType::Sparse(_) => (true, false),
        DataType::Geo => (false, true),
        other => {
            return Err(Error::Query(format!(
                "`{field_name}` is not a vector or a point ({})",
                other.name()
            )))
        }
    };
    // A point's degrees are read as written, not as a vector's `f32`s.
    let body = match point {
        true => json::parse_documents_json(body_str(req)?, &["vector"])?.remove(0),
        false => quick,
    };
    let get = |name: &str| body.iter().find(|(k, _)| k == name).map(|(_, v)| v);

    let vector = match get("vector") {
        Some(Value::Vector(v)) if !sparse => Value::Vector(v.clone()),
        Some(v @ Value::List(_)) if point => v.clone(),
        // A sparse field's is pgvector's text form: `{1:0.5,3:0.25}/30522`.
        Some(Value::Text(t)) if sparse => Value::Text(t.clone()),
        Some(Value::List(items)) if items.is_empty() => {
            return Err(Error::Query("`vector` is empty".into()))
        }
        Some(_) if sparse => {
            return Err(Error::Query(
                "`vector` must be a sparse vector as text: {index:value,...}/dimension".into(),
            ))
        }
        Some(_) => return Err(Error::Query("`vector` must be an array of numbers".into())),
        None => return Err(Error::Query("`vector` is required".into())),
    };

    let as_usize = |name: &str| -> Result<Option<usize>> {
        match get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Int(i)) if *i >= 0 => Ok(Some(*i as usize)),
            Some(_) => Err(Error::Query(format!(
                "`{name}` must be a non-negative integer"
            ))),
        }
    };

    let mut sel = Select {
        collection: schema.name.clone(),
        near: Some(Near {
            field: field_name,
            vector: Expr::Lit(vector),
            ef: as_usize("ef")?,
            exact: matches!(get("exact"), Some(Value::Bool(true))),
            metric: None,
        }),
        limit: as_usize("limit")?.map(Count::N),
        offset: Count::N(as_usize("offset")?.unwrap_or(0)),
        ..Default::default()
    };

    // `"match": "words"` makes it hybrid: BM25 over the text field ranks
    // too, and the two rankings are fused. Flat keys, as the rest of the
    // body's are.
    match get("match") {
        None | Some(Value::Null) => {}
        Some(Value::Text(q)) => {
            let text_field = match get("match_field") {
                Some(Value::Text(f)) => f.clone(),
                Some(_) => return Err(Error::Query("`match_field` must be text".into())),
                None => default_text_field(schema)?,
            };
            // `match_fuzzy`: true, or the most edits a word may take;
            // `match_prefix`: the last word a prefix -- `match`'s `fuzzy N`
            // and `prefix`.
            let fuzzy = match get("match_fuzzy") {
                None | Some(Value::Null) | Some(Value::Bool(false)) => 0,
                Some(Value::Bool(true)) => fennec_core::query::MAX_FUZZY,
                Some(Value::Int(n @ 0..=2)) => *n as u8,
                Some(_) => {
                    return Err(Error::Query(
                        "`match_fuzzy` must be true, false or 0 to 2 edits".into(),
                    ))
                }
            };
            let prefix = match get("match_prefix") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => return Err(Error::Query("`match_prefix` must be true or false".into())),
            };
            sel.matcher = Some(Match {
                field: text_field,
                query: Expr::Lit(Value::Text(q.clone())),
                within: false,
                fuzzy,
                prefix,
            });
            // As written: `Select::check` holds `k` to `MAX_FUSE_K`, as it
            // does every `Select`'s -- clamped to `u32::MAX` here, a side's
            // first ranks weighed alike.
            sel.fuse = Some(Fuse {
                k: as_usize("fuse_k")?,
                candidates: as_usize("candidates")?,
            });
        }
        Some(_) => return Err(Error::Query("`match` must be text".into())),
    }

    if let Some(v) = get("select") {
        let cols = match v {
            Value::List(items) => items
                .iter()
                .map(|i| match i {
                    Value::Text(s) => Ok(s.clone()),
                    _ => Err(Error::Query("`select` must be an array of text".into())),
                })
                .collect::<Result<Vec<_>>>()?,
            Value::Text(s) => vec![s.clone()],
            _ => return Err(Error::Query("`select` must be an array of text".into())),
        };
        for c in &cols {
            field(schema, c)?;
        }
        sel.project = Some(cols);
    }

    // The filter can come from two places: the `where` expression in the
    // body and the `?field=op.value` pairs in the query string. When both
    // are present they are `and`ed.
    let mut filter = filter_from_query(schema, req)?;
    if let Some(Value::Text(expr)) = get("where") {
        let parsed = parse_expr(schema, expr)?;
        filter = Some(match filter {
            Some(f) => Expr::And(Box::new(f), Box::new(parsed)),
            None => parsed,
        });
    }
    sel.filter = filter;
    Ok(sel)
}

/// When the collection has a single `@text` field there is no need to write
/// `match_field`.
fn default_text_field(schema: &Schema) -> Result<String> {
    let mut texts = schema.fields.iter().filter(|f| f.text_index().is_some());
    match (texts.next(), texts.next()) {
        (Some(f), None) => Ok(f.name.clone()),
        (None, _) => Err(Error::Query(format!(
            "`{}` has no @text field to match",
            schema.name
        ))),
        (Some(_), Some(_)) => Err(Error::Query(
            "the collection has several @text fields: name one with `match_field`".into(),
        )),
    }
}

/// When the collection has a single vector field there is no need to write `field`.
fn default_vector_field(schema: &Schema) -> Result<String> {
    let mut found = None;
    for f in &schema.fields {
        if matches!(f.ty, DataType::Vector(..)) {
            if found.is_some() {
                return Err(Error::Query(
                    "there is more than one vector field: specify `field`".into(),
                ));
            }
            found = Some(f.name.clone());
        }
    }
    found.ok_or_else(|| Error::Query(format!("collection `{}` has no vector field", schema.name)))
}

// ---------------------------------------------------------- subscription

/// Parsing of `GET /<name>/changes`.
///
/// The shape is a **set**, not a window: `order`, `limit`, `offset` and
/// `count` are deliberately rejected. A subscription like "the last 100
/// rows" looks right but is not -- when a new row arrives the oldest has to
/// drop out, and an incremental diff cannot say that. An explicit error
/// beats a silently wrong stream.
pub struct Subscription {
    pub collection: String,
    pub filter: Option<Expr>,
    pub project: Option<Vec<String>>,
    /// Absent: seed from scratch. Present: continue from this point.
    pub since: Option<u64>,
}

pub fn subscription(db: &Database, req: &Request) -> Result<Subscription> {
    let seg = req.segments();
    let [name, "changes"] = seg.as_slice() else {
        return Err(Error::NotFound(format!("path `{}`", req.path)));
    };
    let schema = &db.collection(name)?.schema;

    let mut since = None;
    let mut project = None;
    for (k, v) in &req.query {
        match k.as_str() {
            "since" => {
                since =
                    Some(v.trim().parse::<u64>().map_err(|_| {
                        Error::Query(format!("`since` expects a number, got `{v}`"))
                    })?)
            }
            "select" => {
                // The id is always carried: deletions are applied by id, and
                // the projection cannot leave it out.
                let mut cols = projection(schema, v)?;
                if !cols.iter().any(|c| c == "id") {
                    cols.insert(0, "id".to_string());
                }
                project = Some(cols);
            }
            "order" | "limit" | "offset" | "count" => {
                return Err(Error::Query(format!(
                    "`{k}` cannot be used on a subscription: the shape is a set, not a window"
                )))
            }
            _ => {}
        }
    }

    Ok(Subscription {
        collection: name.to_string(),
        filter: filter_with(schema, req, &RESERVED_STREAM, &[])?,
        project,
        since,
    })
}

/// The seed query: the **whole** shape, with the same filter and projection.
pub fn seed_select(sub: &Subscription) -> Select {
    Select {
        collection: sub.collection.clone(),
        filter: sub.filter.clone(),
        project: sub.project.clone(),
        ..Default::default()
    }
}

// ----------------------------------------------------------- raw query

/// A `POST /query` body read ([`parse_query`]): its statement and its
/// parameters, and what [`exactly`] reads them again as written by.
pub struct Query {
    pub stmt: Arc<Statement>,
    pub params: Vec<Value>,
    seen: Seen,
}

/// What a body's first read was told and found: its text, the places of its
/// parameters it read as written at once -- the hint the text's last request
/// left ([`Cached::hint`]) -- and, for a text too long to keep, where its
/// documents give a field a list of numbers ([`fennec_ql::Listing`]).
#[derive(Default)]
struct Seen {
    text: String,
    hash: Option<u64>,
    hint: Option<Arc<[usize]>>,
    /// The text read as written, where its shard kept it ([`Cached::exact`]).
    exact: Option<Arc<Statement>>,
    listing: Option<fennec_ql::Listing>,
}

/// The body of `POST /query`: `{"query": "...", "params": [...]}`.
///
/// The REST surface is derived from the schema and is deliberately narrow
/// (no DDL, no `or` groups). Raw input lifts that wall: so the query builder
/// in the browser can hand the same text to wasm and to this endpoint alike
/// -- one piece of query code, two transports.
///
/// The text's parsed statement is looked up once the parameters are
/// reached -- after the text, as clients write it -- and the places its last
/// request read again as written ([`exactly`]) are read so at once: a page
/// of 1 000 rows each a parameter a field, `[float]`s among them, was read
/// twice over, 20% more than the quick read alone. A body naming its
/// parameters first is read the quick way, as before.
pub fn parse_query(body: &str) -> Result<Query> {
    let mut found: Option<Found> = None;
    let mut obj = json::parse_object_listing_at(body, "params", &mut |before| {
        let at = text_at(before)?;
        let Value::Text(sql) = &before[at].1 else {
            return None;
        };
        let f = Found::of(at, sql);
        let hint = f.hint();
        found = Some(f);
        hint
    })?;
    let at = text_at(&obj);
    // Taken, not cloned: the text and the parameters are the body's alone.
    let sql = match at.map(|a| &mut obj[a].1) {
        Some(Value::Text(s)) => std::mem::take(s),
        Some(_) => return Err(Error::Query("`query` must be text".into())),
        None => return Err(Error::Query("`query` is required".into())),
    };
    let mut params = listed_params(&mut obj);
    crate::timing::lap(crate::timing::Phase::Json);
    // Found for the text the statement is: a body holding both `sql` and
    // `query`, the latter after its parameters, was told of the other --
    // and its parameters, read by the other's hint, are read again.
    if found
        .as_ref()
        .is_some_and(|f| Some(f.at) != at && f.hint().is_some())
    {
        params = listed_params(&mut json::parse_object_listing(body, "params")?);
    }
    let found = found.filter(|f| Some(f.at) == at);
    // Whether the shard was asked already, and had nothing.
    let looked = found.is_some();
    let mut seen = Seen {
        hash: found.as_ref().map(|f| f.hash),
        ..Seen::default()
    };
    let hit = found.and_then(|f| f.hit);
    let kept = match hit {
        Some(Hit {
            stmt: Some(stmt),
            exact,
            hint,
        }) => {
            seen.hint = hint;
            seen.exact = exact;
            Some(stmt)
        }
        // A long text's hint alone, or nothing.
        hit => {
            seen.hint = hit.and_then(|h| h.hint);
            None
        }
    };
    let stmt = match kept {
        Some(stmt) => Ok(stmt),
        None if sql.len() > PARSED_LONGEST => {
            crate::timing::parsed();
            fennec_ql::parse_one_listing(&sql)
                .map(|(stmt, listing)| {
                    seen.listing = Some(listing);
                    Arc::new(stmt)
                })
                .map_err(|e| Error::Query(e.to_string()))
        }
        // Not looked up yet -- a body of no parameters, or naming them
        // first -- or looked up and not found.
        None => {
            let hash = *seen.hash.get_or_insert_with(|| hash_of(&sql));
            match (!looked).then(|| cached(&sql, hash)).flatten() {
                Some(Hit {
                    stmt: Some(stmt),
                    exact,
                    ..
                }) => {
                    seen.exact = exact;
                    Ok(stmt)
                }
                _ => parsed_by(&sql, hash),
            }
        }
    };
    // What the request is counted as, rather than its JSON (`statements`):
    // a call by its name alone, `call transfer $1`, however its arguments
    // were written -- the call is the unit an operator tunes, and the
    // shape `POST /_call/<name>` is counted as.
    match stmt.as_deref() {
        Ok(Statement::Call(c)) => crate::statements::call(&c.name),
        _ => crate::statements::text(&sql),
    }
    let stmt = stmt?;
    crate::timing::lap(crate::timing::Phase::Cache);
    seen.text = sql;
    Ok(Query { stmt, params, seen })
}

/// The member holding a body's text: `query`, or else `sql`.
fn text_at(members: &[(String, Value)]) -> Option<usize> {
    let at = |name: &str| members.iter().position(|(k, _)| k == name);
    at("query").or_else(|| at("sql"))
}

/// A body's parameters, taken out of its members: a list whose elements
/// keep their types, as the browser module reads its parameters -- a
/// vector parameter is an array inside it, `[[..]]`.
fn listed_params(members: &mut [(String, Value)]) -> Vec<Value> {
    match members
        .iter_mut()
        .find(|(k, _)| k == "params")
        .map(|(_, v)| v)
    {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::List(items)) => std::mem::take(items),
        Some(other) => vec![std::mem::replace(other, Value::Null)],
    }
}

/// What the parse cache holds of a body's text, found as its parameters
/// are reached ([`parse_query`]): where the text stands among the members,
/// its hash, its statement where kept, and the hint its last request left.
struct Found {
    at: usize,
    hash: u64,
    hit: Option<Hit>,
}

impl Found {
    fn of(at: usize, text: &str) -> Found {
        let hash = hash_of(text);
        let hit = match text.len() > PARSED_LONGEST {
            // A long text's hint alone: its statement is not kept.
            true => long_hint(hash).map(|hint| Hit {
                stmt: None,
                exact: None,
                hint: Some(hint),
            }),
            false => cached(text, hash),
        };
        Found { at, hash, hit }
    }

    fn hint(&self) -> Option<Arc<[usize]>> {
        self.hit.as_ref().and_then(|h| h.hint.clone())
    }
}

/// What a shard keeps of a text, as a request takes it.
struct Hit {
    stmt: Option<Arc<Statement>>,
    exact: Option<Arc<Statement>>,
    hint: Option<Arc<[usize]>>,
}

/// The statement and parameters of a `POST /query` body as a json field
/// needs them: a list of numbers it is handed, or a path is compared with,
/// read again as written, from the statement's text or the parameters'
/// JSON -- read the quick way, into a vector's `f32`s, the field refuses it
/// (`Database::exactly`). A statement with no such list, and parameters
/// holding none, are handed back as they came, the database not looked at,
/// unless the first read took a hint, which is looked at each time.
pub fn exactly(
    db: &std::sync::RwLock<Database>,
    body: &str,
    q: Query,
) -> Result<(Arc<Statement>, Vec<Value>)> {
    let Query { stmt, params, seen } = q;
    if !vectored(&stmt, &params) && seen.hint.is_none() {
        return Ok((stmt, params));
    }
    let g = db.read().unwrap_or_else(|e| e.into_inner());
    exactly_in(&g, body, stmt, params, seen)
}

/// [`exactly`] for each line of a `POST /batch` body, the database looked
/// at once.
pub fn exactly_batch(
    db: &std::sync::RwLock<Database>,
    body: &str,
    stmts: Vec<Query>,
) -> Result<Vec<(Statement, Vec<Value>)>> {
    if !stmts
        .iter()
        .any(|q| vectored(&q.stmt, &q.params) || q.seen.hint.is_some())
    {
        return Ok(stmts
            .into_iter()
            .map(|q| (Arc::unwrap_or_clone(q.stmt), q.params))
            .collect());
    }
    let g = db.read().unwrap_or_else(|e| e.into_inner());
    let lines = body.lines().filter(|l| !l.trim().is_empty());
    let mut out = Vec::with_capacity(stmts.len());
    for (line, q) in lines.zip(stmts) {
        let (stmt, params) = exactly_in(&g, line, q.stmt, q.params, q.seen)?;
        out.push((Arc::unwrap_or_clone(stmt), params));
    }
    Ok(out)
}

/// The body of `POST /_call/<name>`: `{"params": {...}}`, or none for a
/// statement of no arguments, as the `call <name> $1` it stands for -- its
/// arguments the one parameter, an object of them by name. No FennecQL is
/// parsed: the name is the path's, the engine matches the arguments to the
/// signature, and the shape counted is the name's
/// ([`crate::statements::call`]). A name no statement may have
/// ([`fennec_core::named::valid_name`]) is the 404 a missing one is, in the
/// same words, and noted as nothing.
pub fn parse_call(name: &str, body: &str) -> Result<(Arc<Statement>, Vec<Value>)> {
    if !fennec_core::named::valid_name(name) {
        return Err(Error::NotFound(format!("statement `{name}`")));
    }
    crate::statements::call(name);
    let params = call_params(body, false)?;
    crate::timing::lap(crate::timing::Phase::Json);
    let args = match params.is_empty() {
        true => fennec_core::named::Args::None,
        false => fennec_core::named::Args::Param(0),
    };
    let call = fennec_core::named::Call {
        name: name.to_string(),
        args,
        claims: None,
    };
    Ok((Arc::new(Statement::Call(Box::new(call))), params))
}

/// A call's body read into its one parameter, `exact` reading a list of
/// numbers as written rather than as a vector's `f32`s ([`exactly_call`]).
/// `params` alone: anything beside it is refused, never ignored -- an `as`
/// a caller meant for its claims, run without them, would be a call it did
/// not make.
fn call_params(body: &str, exact: bool) -> Result<Vec<Value>> {
    if body.trim().is_empty() {
        return Ok(Vec::new());
    }
    let members = match exact {
        false => json::parse_object(body)?,
        true => json::parse_object_listing_exact(body, "params")?,
    };
    let mut params = Vec::new();
    for (k, v) in members {
        match (k.as_str(), v) {
            ("params", Value::Null) => {}
            ("params", v @ Value::Object(_)) => params.push(v),
            ("params", _) => {
                return Err(Error::Query(
                    "`params` is an object: each argument by its name".into(),
                ))
            }
            (other, _) => {
                return Err(Error::Query(format!(
                    "a call's body holds `params` alone, not `{other}`"
                )))
            }
        }
    }
    Ok(params)
}

/// [`exactly`] for a call's body: where an argument of a type keeping a
/// list of numbers as written is handed one, the body read again so.
pub fn exactly_call(
    db: &std::sync::RwLock<Database>,
    body: &str,
    stmt: Arc<Statement>,
    params: Vec<Value>,
) -> Result<(Arc<Statement>, Vec<Value>)> {
    if !vectored(&stmt, &params) {
        return Ok((stmt, params));
    }
    let need = db.read().unwrap_or_else(|e| e.into_inner()).exactly(&stmt);
    match need.params.is_empty() {
        true => Ok((stmt, params)),
        false => Ok((stmt, call_params(body, true)?)),
    }
}

/// Whether a list of numbers is among a statement's literals or its
/// parameters: where none is, no json field is handed a vector.
fn vectored(stmt: &Statement, params: &[Value]) -> bool {
    stmt.reads_vectors() || params.iter().any(fennec_core::query::holds_vector)
}

/// [`exactly`] over a statement and parameters as the first read made
/// them, `seen` what it was told and found.
///
/// The hint: each place the first read took as written must be one the
/// statement still names (`Database::exactly`, before `settle`), or the
/// parameters are read the quick way again and the hint dropped -- the
/// schema changed, or a long text's hash met another's. Every place it names
/// is read as written either way, at once or again ([`Exactly::settle`]
/// drops a value holding no vector, which is what a value read as written
/// is), so no answer depends on it. The places read as written are the
/// next request's hint, kept only where they changed: a statement whose
/// hint holds takes no lock for it.
fn exactly_in(
    db: &Database,
    body: &str,
    stmt: Arc<Statement>,
    mut params: Vec<Value>,
    mut seen: Seen,
) -> Result<(Arc<Statement>, Vec<Value>)> {
    let mut need = db.exactly(&stmt);
    let held = seen.hint.take();
    let mut hint = held.as_deref();
    if hint.is_some_and(|h| !h.iter().all(|i| need.params.binary_search(i).is_ok())) {
        params = listed_params(&mut json::parse_object_listing(body, "params")?);
        hint = None;
    }
    // A list of numbers alone given one read as written at once, its
    // `f32`s the integers written: the vector the quick read made of it,
    // as with no hint (`Exactly::settle` keeps that one), so the value is
    // the same either way, and every use of it -- `v = $1` beside `xs: $1`
    // -- answers alike. One holding another number is read as written
    // either way. The value asked first: a list of decimals, the one a hint
    // reads at once, leaves at its first number. Its place looked up first,
    // a page of 1 000 rows a `[float]` parameter each (`number_lists
    // params`) took 0.36% more instructions for it.
    for &i in hint.unwrap_or_default() {
        if let Some(v) = params.get(i).and_then(fennec_core::value::whole_vector) {
            if need.lists.binary_search(&i).is_ok() {
                params[i] = Value::Vector(v);
            }
        }
    }
    need.settle(&params);
    let stmt = match need.text {
        true => exact_text(stmt, &mut seen)?,
        false => stmt,
    };
    // Those parameters alone read again as written: the rest of the body
    // is stepped over, an embedding beside them unread.
    if !need.params.is_empty() {
        for (i, v) in json::exact_params(body, Some("params"), &need.params)? {
            if let Some(p) = params.get_mut(i) {
                *p = v;
            }
        }
    }
    // A place the hint read as written holds no vector, and is read again
    // never: the hint is the next request's as it stands unless one was
    // read again, or it was dropped.
    if !need.params.is_empty() || hint.is_none() != held.is_none() {
        remember(&seen, merged(hint.unwrap_or_default(), &need.params));
    }
    Ok((stmt, params))
}

/// The statement of `seen`'s text read as written ([`fennec_ql::parse_exact`]):
/// a text too long to keep has its documents' lists put in their places as
/// they were written, kept from its one read, where they are every list it
/// holds ([`fennec_ql::read_listed`]); a kept text's is kept beside it,
/// read once.
fn exact_text(stmt: Arc<Statement>, seen: &mut Seen) -> Result<Arc<Statement>> {
    let Some(mut listing) = seen.listing.take() else {
        return match seen.exact.take() {
            Some(exact) => Ok(exact),
            None => exact_kept(&seen.text, seen.hash),
        };
    };
    let mut stmt = stmt;
    // A long text's statement is no shard's: this request's alone.
    if let Some(s) = Arc::get_mut(&mut stmt) {
        if fennec_ql::read_listed(s, &mut listing) {
            return Ok(stmt);
        }
    }
    parse_exact_one(&seen.text).map(Arc::new)
}

fn parse_exact_one(text: &str) -> Result<Statement> {
    fennec_ql::parse_exact(text)
        .and_then(|mut s| match s.len() {
            1 => Ok(s.remove(0)),
            n => Err(Error::Query(format!(
                "expected a single statement, found {n}"
            ))),
        })
        .map_err(|e| Error::Query(e.to_string()))
}

/// Two lists of places ascending, as one; `None` where both are empty.
fn merged(a: &[usize], b: &[usize]) -> Option<Arc<[usize]>> {
    if b.is_empty() {
        return (!a.is_empty()).then(|| a.into());
    }
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        let x = match (a.get(i), b.get(j)) {
            (Some(&x), Some(&y)) if x == y => {
                j += 1;
                i += 1;
                x
            }
            (Some(&x), Some(&y)) if x < y => {
                i += 1;
                x
            }
            (Some(&x), None) => {
                i += 1;
                x
            }
            (_, Some(&y)) => {
                j += 1;
                y
            }
            (None, None) => break,
        };
        out.push(x);
    }
    Some(out.into())
}

/// Shards of the statements parsed, by their text's hash: a client sends
/// one text again and again with other parameters, and parsing it was half
/// of what `POST /query` cost for a row by id. A shard a mutex, so the
/// connections' threads do not queue on one; full, a shard forgets every
/// statement it held, and a text of literals, never sent twice, costs a
/// lookup and an insert.
const PARSED_SHARDS: usize = 16;
const PARSED_PER_SHARD: usize = 64;
/// A text longer than this is parsed each time: that long, it holds its
/// literals -- a vector's alone is kilobytes -- and would fill a shard with
/// statements no one asks for again.
const PARSED_LONGEST: usize = 1024;

/// A statement parsed, as its text's shard keeps it.
struct Cached {
    text: Box<str>,
    stmt: Arc<Statement>,
    /// The text read as written, once a request of it needed that
    /// (`Database::exactly`): what the text says alone, so the same tree
    /// each time, where it was parsed again each request.
    exact: Option<Arc<Statement>>,
    /// The places of its parameters its last request read as written,
    /// ascending: its next request reads them so at once ([`exactly_in`]).
    hint: Option<Arc<[usize]>>,
}

type Parsed = std::sync::Mutex<Option<std::collections::HashMap<u64, Cached>>>;
static PARSED: [Parsed; PARSED_SHARDS] = [const { std::sync::Mutex::new(None) }; PARSED_SHARDS];

/// The hints of texts too long to keep, by their hash alone, as [`PARSED`]
/// keeps its texts': a page of rows a parameter a field is one long text
/// sent again and again. A hint read for another text changes no answer
/// ([`exactly_in`]) and is put right by the request that read it.
type Hints = std::sync::Mutex<Option<Hinted>>;
static HINTS: [Hints; PARSED_SHARDS] = [const { std::sync::Mutex::new(None) }; PARSED_SHARDS];

/// The most places a hint holds: a text naming more for its lists keeps
/// none, and each request of it reads them a second time, as one with no
/// hint does. A place is 8 bytes and a long text's places are bounded by
/// its body alone, 64 MiB: `put t [{xs: $1}, ..., {xs: $3000000}]` would
/// have kept 24 MB a text, and a shard's 64 texts 1.5 GB. 4 096 places --
/// a page of 1 000 rows of four lists each -- are 32 KB.
const HINT_PLACES: usize = 4096;
/// The most places a shard of [`HINTS`] keeps, all its texts' together:
/// past it the shard forgets them, as it does past its 64 texts. 128 KB a
/// shard, 2 MB for the 16.
const HINT_BUDGET: usize = 16_384;

/// A shard of [`HINTS`]: its texts' hints by hash, and how many places
/// they hold together.
#[derive(Default)]
struct Hinted {
    map: std::collections::HashMap<u64, Arc<[usize]>>,
    places: usize,
}

impl Hinted {
    /// `next` as the hint of the text hashed `hash`, or none: the shard
    /// forgets every hint first where it holds 64 texts or `next` would
    /// take it past [`HINT_BUDGET`].
    fn keep(&mut self, hash: u64, next: Option<Arc<[usize]>>) {
        if let Some(old) = self.map.remove(&hash) {
            self.places -= old.len();
        }
        let Some(h) = next else {
            return;
        };
        if self.map.len() >= PARSED_PER_SHARD || self.places + h.len() > HINT_BUDGET {
            self.map.clear();
            self.places = 0;
        }
        self.places += h.len();
        self.map.insert(hash, h);
    }
}

fn hash_of(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

fn shard_of(hash: u64) -> usize {
    (hash % PARSED_SHARDS as u64) as usize
}

/// The statement kept for `text`, the text read as written where it is
/// kept too, and its hint.
fn cached(text: &str, hash: u64) -> Option<Hit> {
    PARSED[shard_of(hash)]
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&hash))
        .filter(|c| &*c.text == text)
        .map(|c| Hit {
            stmt: Some(Arc::clone(&c.stmt)),
            exact: c.exact.clone(),
            hint: c.hint.clone(),
        })
}

fn long_hint(hash: u64) -> Option<Arc<[usize]>> {
    HINTS[shard_of(hash)]
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.map.get(&hash).cloned())
}

/// `next` kept as the hint of `seen`'s text: on its shard's statement, or
/// for a long text by its hash. A hint of more than [`HINT_PLACES`] places
/// is none.
fn remember(seen: &Seen, next: Option<Arc<[usize]>>) {
    let next = next.filter(|h| h.len() <= HINT_PLACES);
    let hash = seen.hash.unwrap_or_else(|| hash_of(&seen.text));
    if seen.text.len() > PARSED_LONGEST {
        HINTS[shard_of(hash)]
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(Default::default)
            .keep(hash, next);
        return;
    }
    let mut shard = PARSED[shard_of(hash)]
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(c) = shard
        .as_mut()
        .and_then(|m| m.get_mut(&hash))
        .filter(|c| *c.text == *seen.text)
    {
        c.hint = next;
    }
}

/// The statement `text` reads as written: kept beside its quick one, read
/// once, where the text is kept.
fn exact_kept(text: &str, hash: Option<u64>) -> Result<Arc<Statement>> {
    if text.len() > PARSED_LONGEST {
        return parse_exact_one(text).map(Arc::new);
    }
    let hash = hash.unwrap_or_else(|| hash_of(text));
    let shard = &PARSED[shard_of(hash)];
    let kept = shard
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&hash))
        .filter(|c| &*c.text == text)
        .and_then(|c| c.exact.clone());
    if let Some(stmt) = kept {
        return Ok(stmt);
    }
    let stmt = Arc::new(parse_exact_one(text)?);
    if let Some(c) = shard
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|m| m.get_mut(&hash))
        .filter(|c| &*c.text == text)
    {
        c.exact = Some(Arc::clone(&stmt));
    }
    Ok(stmt)
}

/// `text` parsed as one statement, from the shards when it was before. A
/// parse reads the text alone, so the statement is the same one each time.
pub(crate) fn parsed(text: &str) -> Result<Arc<Statement>> {
    if text.len() > PARSED_LONGEST {
        return parse_text(text);
    }
    let hash = hash_of(text);
    match cached(text, hash).and_then(|hit| hit.stmt) {
        Some(stmt) => Ok(stmt),
        None => parsed_by(text, hash),
    }
}

fn parse_text(text: &str) -> Result<Arc<Statement>> {
    crate::timing::parsed();
    fennec_ql::parse_one(text)
        .map(Arc::new)
        .map_err(|e| Error::Query(e.to_string()))
}

/// [`parsed`], `text` -- of a kept length -- found in no shard: parsed, and
/// kept.
fn parsed_by(text: &str, hash: u64) -> Result<Arc<Statement>> {
    let stmt = parse_text(text)?;
    let mut shard = PARSED[shard_of(hash)]
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let map = shard.get_or_insert_with(Default::default);
    if map.len() >= PARSED_PER_SHARD {
        map.clear();
    }
    map.insert(
        hash,
        Cached {
            text: text.into(),
            stmt: Arc::clone(&stmt),
            exact: None,
            hint: None,
        },
    );
    Ok(stmt)
}

/// The body of `POST /batch`: **one per line**, each a `POST /query` body
/// (NDJSON).
///
/// ```text
/// {"query":"put tasks {title: $1}","params":["a"]}
/// {"query":"set tasks {status: $1} where id = $2","params":["open", 7]}
/// ```
///
/// Why NDJSON: fennecdb's JSON parser does not accept nested objects
/// (deliberately -- see Limits), so `{"statements":[{...}]}` could not be
/// parsed anyway. Rather than writing a second parser, the boundary is put
/// at the end of the line: every line is *exactly* the body of the single
/// query endpoint and goes down that same path. Escaping rules mean no JSON
/// encoder can write a bare `\n` into the body, so the split is unambiguous.
///
/// **It is one block.** The statements run in order, under a single write
/// lock, and their writes land as one record or not at all: the first error
/// puts back what the ones before it did, and the response says so
/// (`completed` is 0). One round trip instead of N, no other writer
/// slipping in between, and nothing half done. A schema change is put back
/// as a write is (`Statement::fits_block`); a `compact` cannot be, so a
/// batch holding one runs each statement on its own: it stops at the first
/// error, and `completed` says how many were applied.
pub fn parse_batch(body: &str) -> Result<Vec<Query>> {
    let mut out = Vec::new();
    for (i, line) in body.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let q = parse_query(line).map_err(|e| {
            // The parse's own words: `{e}` said their kind again,
            // `query error: query error: position 11: ...`.
            let why = match e {
                Error::Query(m) => m,
                e => e.to_string(),
            };
            Error::Query(format!("batch line {}: {why}", i + 1))
        })?;
        out.push(q);
    }
    if out.is_empty() {
        return Err(Error::Query("empty batch".into()));
    }
    Ok(out)
}

/// A single item of the batch response.
fn result_json(out: &mut String, resp: &Response2) {
    match resp {
        Response2::Rows(rs) => {
            out.push_str("{\"rows\":");
            json::rows_array_into(out, rs);
            json::facets_into(out, rs);
            out.push('}');
        }
        Response2::Affected(n) => out.push_str(&format!("{{\"affected\":{n}}}")),
        Response2::Ok(msg) => {
            out.push_str("{\"message\":");
            json::escape_into(out, msg);
            out.push('}');
        }
        Response2::Schemas(list) => {
            out.push_str("{\"collections\":");
            out.push_str(&schemas_json(list));
            out.push('}');
        }
    }
}

/// `{"ok":N,"results":[...]}`
pub fn render_batch(results: &[Response2], version: &'static str) -> Response {
    let mut out = format!("{{\"ok\":{},\"results\":[", results.len());
    for (i, r) in results.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        result_json(&mut out, r);
    }
    out.push_str("]}");
    Response::json(200, out).versioned(version)
}

/// A batch that stopped: we **have to say** what was applied -- nothing for
/// a block, the statements before the error otherwise. A silent error would
/// permanently separate the client's optimistic local state from the
/// server.
pub fn render_batch_error(e: &Error, completed: usize, version: &'static str) -> Response {
    render_batch_stop(stop_of(e), completed, completed, version)
}

/// Why a batch stopped: the status, the words, the reason a statement's
/// `require <n> else '<reason>'` gave where that refused it, and the
/// statement bound to the writes that refused it, named to the server's
/// token alone ([`bound_stop`]).
pub type Stop = (u16, String, Option<String>, Option<String>);

/// An engine error as the [`Stop`] it makes of a batch.
pub fn stop_of(e: &Error) -> Stop {
    (
        status_of(e),
        e.to_string(),
        e.reason().map(String::from),
        None,
    )
}

/// [`stop_of`], or where a statement bound to the write refused it --
/// `bound`, its name and the words its refusal is told after
/// (`Database::refused_by`), asked for the server's token alone -- the
/// engine's words after the binding's, and `binding` beside them, as
/// `/query` tells it (`docs/design/named-statements.md`, *When a binding
/// refuses*).
pub fn bound_stop(e: &Error, bound: Option<&(String, String)>) -> Stop {
    match bound {
        None => stop_of(e),
        Some((name, words)) => (
            status_of(e),
            format!("{words}{e}"),
            e.reason().map(String::from),
            Some(name.clone()),
        ),
    }
}

/// A batch stopped at statement `at` (from 0) with `status` and `why`: an
/// error, a write whose `require` was not met, or a write the data ceiling
/// refused. `completed` statements of it stay applied: none of a block.
/// `at` names the statement, since in a block the count is no longer where
/// it stopped; `reason`, beside it, what the statement said of its
/// `require`, which a caller tells refusals apart by where `at` moves with
/// every statement added in front.
pub fn render_batch_stop(
    (status, why, reason, binding): Stop,
    completed: usize,
    at: usize,
    version: &'static str,
) -> Response {
    let mut out = String::from("{\"error\":");
    json::escape_into(&mut out, &why);
    out.push_str(&format!(",\"completed\":{completed},\"at\":{at}"));
    reason_into(&mut out, reason.as_deref());
    if let Some(b) = binding {
        out.push_str(",\"binding\":");
        json::escape_into(&mut out, &b);
    }
    out.push('}');
    Response::json(status, out).versioned(version)
}

/// `,"reason":"..."` where a `require`'s `else` gave one: the statement's
/// own words, which a scoped token is told as any caller is -- it sent
/// them, and they name nothing of the rows.
pub fn reason_into(out: &mut String, reason: Option<&str>) {
    if let Some(r) = reason {
        out.push_str(",\"reason\":");
        json::escape_into(out, r);
    }
}

/// The JSON form of a raw query response: the shape follows the statement.
pub fn render_any(resp: &Response2, version: &'static str) -> Response {
    match resp {
        Response2::Rows(rs) => Response::json(200, rows_body(rs)),
        Response2::Affected(n) => Response::json(200, format!("{{\"affected\":{n}}}")),
        Response2::Ok(msg) => {
            let mut out = String::from("{\"message\":");
            json::escape_into(&mut out, msg);
            out.push('}');
            Response::json(200, out)
        }
        Response2::Schemas(list) => Response::json(200, schemas_json(list)),
    }
    .versioned(version)
}

// --------------------------------------------------------------- response

pub fn render(resp: &Response2, shape: &Shape, version: &'static str) -> Response {
    match (shape, resp) {
        (Shape::Rows, Response2::Rows(rs)) => Response::json(200, rows_body(rs)),
        (Shape::Count, Response2::Rows(rs)) => {
            let n = rs
                .rows
                .first()
                .and_then(|r| r.values.first())
                .map(json::to_string)
                .unwrap_or_else(|| "0".into());
            let mut out = format!("{{\"count\":{n}");
            json::facets_into(&mut out, rs);
            out.push('}');
            Response::json(200, out)
        }
        (Shape::Affected(name, status), Response2::Affected(n)) => {
            Response::json(*status, format!("{{\"{name}\":{n}}}"))
        }
        (Shape::Written(status), Response2::Rows(rs)) => Response::json(*status, rows_body(rs)),
        (Shape::Info, Response2::Schemas(list)) => {
            let mut out = String::from("{\"fennecdb\":");
            json::escape_into(&mut out, version);
            out.push_str(",\"collections\":[");
            for (i, s) in list.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json::escape_into(&mut out, &s.name);
            }
            out.push_str("]}");
            Response::json(200, out)
        }
        (Shape::Schemas, Response2::Schemas(list)) => Response::json(200, schemas_json(list)),
        // If the engine did not return what was expected, this is a programming bug.
        _ => Response::error(500, "unexpected response shape"),
    }
}

/// `fennec_core::query::Response` -- renamed so it does not clash with `http::Response`.
pub use fennec_core::query::Response as Response2;

/// The rows as a JSON array -- or, when the query asked for facets,
/// `{"rows": [...], "facets": {...}}`: the counts answer for the whole
/// set, so they go beside the rows rather than into one, and a query that
/// asks none keeps the bare array every client reads.
pub fn rows_json(rs: &ResultSet) -> String {
    rows_json_into(String::new(), rs)
}

/// [`rows_json`] as an answer's body, written into the connection's spare
/// buffer (`http::spare_body`), which goes back to it once the answer is
/// sent.
fn rows_body(rs: &ResultSet) -> Vec<u8> {
    let spare = String::from_utf8(crate::http::spare_body()).unwrap_or_default();
    rows_json_into(spare, rs).into_bytes()
}

fn rows_json_into(mut out: String, rs: &ResultSet) -> String {
    // Room for the rows as the first one goes: grown from nothing, the
    // answer to a read by id of a 1 KB record was copied eight times.
    let first: usize = rs.rows.first().map_or(0, |r| {
        r.values
            .iter()
            .zip(&rs.columns)
            .map(|(v, c)| {
                c.len()
                    + match v {
                        Value::Text(s) => s.len() + 8,
                        _ => 24,
                    }
            })
            .sum()
    });
    out.clear();
    out.reserve((first * rs.rows.len()).min(1 << 20) + 16);
    if rs.facets.is_empty() {
        json::rows_array_into(&mut out, rs);
        return out;
    }
    out.push_str("{\"rows\":");
    json::rows_array_into(&mut out, rs);
    json::facets_into(&mut out, rs);
    out.push('}');
    out
}

fn schemas_json(list: &[Schema]) -> String {
    let mut out = String::from("[");
    for (i, s) in list.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"name\":");
        json::escape_into(&mut out, &s.name);
        out.push_str(",\"fields\":[");
        for (j, f) in s.fields.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str("{\"name\":");
            json::escape_into(&mut out, &f.name);
            out.push_str(",\"type\":");
            json::escape_into(&mut out, &f.ty.name());
            out.push_str(",\"index\":");
            match &f.index {
                IndexKind::None => out.push_str("null"),
                IndexKind::Hash { unique: false } => json::escape_into(&mut out, "hash"),
                IndexKind::Hash { unique: true } => json::escape_into(&mut out, "unique"),
                IndexKind::Sorted { ttl: None } => json::escape_into(&mut out, "sorted"),
                IndexKind::Sorted { ttl: Some(ms) } => json::escape_into(
                    &mut out,
                    &format!("ttl({})", fennec_core::schema::ttl_text(*ms)),
                ),
                IndexKind::Vector(spec) => json::escape_into(
                    &mut out,
                    &format!(
                        "hnsw({}, m={}, ef_search={}{})",
                        spec.metric.name(),
                        spec.m,
                        spec.ef_search,
                        spec.quant_arg()
                    ),
                ),
                IndexKind::Text(spec) => {
                    json::escape_into(&mut out, &format!("text({})", spec.args()))
                }
                IndexKind::Inverted => json::escape_into(&mut out, "inverted"),
                IndexKind::Geo => json::escape_into(&mut out, "geo"),
            }
            // The `@text` it keeps beside its own index, only where there
            // is one: a client making a replica's collection keeps both.
            if let Some(spec) = f.beside {
                out.push_str(",\"also\":");
                json::escape_into(&mut out, &format!("text({})", spec.args()));
            }
            out.push_str(",\"required\":");
            out.push_str(if f.required { "true" } else { "false" });
            // Only where there is one, so a schema without stays as it was.
            if let Some(c) = f.collate {
                out.push_str(",\"collate\":");
                json::escape_into(&mut out, c.name());
            }
            // A replica made from this compares as the server does.
            if f.nocase {
                out.push_str(",\"nocase\":true");
            }
            out.push('}');
        }
        out.push(']');
        json::composites_into(&mut out, s);
        out.push('}');
    }
    out.push(']');
    out
}

/// Turns an engine error into an HTTP status.
pub fn status_of(e: &Error) -> u16 {
    match e {
        Error::NotFound(_) => 404,
        Error::Exists(_) | Error::Duplicate(_) => 409,
        // The write's `require` was not met: the state it was sent against
        // is not the one it found. Apart from 409 so a client tells a lost
        // race on a balance from a value taken.
        Error::Unmet(_) => 412,
        // A write's runs of the statements bound to it past its caller's
        // `--call-rate`, which waiting lets through (`calls::charge_runs`).
        Error::Query(m) if m.starts_with(crate::calls::RUNS_WAIT) => 429,
        Error::Type(_) | Error::Query(_) => 400,
        Error::Corrupt(_) | Error::Io(_) | Error::Plugin(_) => 500,
        // As `--http-read-only` answers: the write is not this server's to take.
        Error::ReadOnly(_) | Error::Denied(_) => 403,
        // Insufficient storage, as --max-memory answers: the write fits
        // once rows are deleted, not by sending it again.
        Error::Quota(_) => 507,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::RwLock;

    /// A statement's request reads at once, as written, the parameters its
    /// last request read again -- a short text's hint on its parse-cache
    /// entry, a long one's by its hash -- and the hint holds where it is
    /// right, no lock taken to keep it again.
    #[test]
    fn a_statement_s_next_request_reads_its_lists_at_once() {
        let db = RwLock::new(Database::new());
        let create = fennec_ql::parse_one("create collection hinted (fs [float], v vector<2>)");
        db.write().unwrap().execute(&create.unwrap()).unwrap();
        let short = r#"{"query":"put hinted {id: 1, fs: $1, v: $2}","params":[[0.1],[0.5, 0.25]]}"#;
        let docs: Vec<String> = (0..60)
            .map(|d| format!("{{id: {}, fs: ${}, v: ${}}}", d + 2, 2 * d + 1, 2 * d + 2))
            .collect();
        let params = vec!["[0.1], [0.5, 0.25]"; 60];
        let long = format!(
            r#"{{"query":"put hinted [{}]","params":[{}]}}"#,
            docs.join(", "),
            params.join(", ")
        );
        assert!(long.len() > 2 * PARSED_LONGEST);
        let floats = Value::List(vec![Value::Float(0.1)]);
        for (body, places) in [(short, vec![0]), (&*long, (0..60).map(|d| 2 * d).collect())] {
            let q = parse_query(body).unwrap();
            assert!(q.seen.hint.is_none());
            assert!(matches!(q.params[0], Value::Vector(_)));
            let (_, params) = exactly(&db, body, q).unwrap();
            assert_eq!(params[0], floats);
            for _ in 0..2 {
                let q = parse_query(body).unwrap();
                assert_eq!(q.seen.hint.as_deref(), Some(&places[..]));
                assert_eq!(q.params[0], floats);
                assert!(matches!(q.params[1], Value::Vector(_)));
                let (_, params) = exactly(&db, body, q).unwrap();
                assert_eq!(params[0], floats);
            }
        }
        // A body naming its text as `sql` before its parameters and as
        // `query` after them is the `query`'s: the `sql`'s hint read its
        // parameters, and they are read the quick way again.
        let both = r#"{"sql":"put hinted {id: 1, fs: $1, v: $2}","params":[[0.1],[0.5, 0.25]],"query":"get hinted where id = $3"}"#;
        let q = parse_query(both).unwrap();
        assert!(q.seen.hint.is_none());
        assert!(matches!(q.params[0], Value::Vector(_)));
    }

    /// A list read at once as the hint asks, its `f32`s the integers
    /// written, is the vector the quick read makes of it, as it is with no
    /// hint: the value is the same either way, so is every answer.
    #[test]
    fn a_whole_list_read_at_once_is_the_quick_read_s_vector() {
        let db = RwLock::new(Database::new());
        let create = fennec_ql::parse_one("create collection hinted_whole (fs [float])");
        db.write().unwrap().execute(&create.unwrap()).unwrap();
        let body = |p: &str| format!(r#"{{"query":"put hinted_whole {{fs: $1}}","params":[{p}]}}"#);
        let (fresh, tenth) = (body("[1, 2]"), body("[0.1, 2]"));
        let q = parse_query(&fresh).unwrap();
        let (_, quick) = exactly(&db, &fresh, q).unwrap();
        assert_eq!(quick[0], Value::Vector(vec![1.0, 2.0]));
        let q = parse_query(&tenth).unwrap();
        exactly(&db, &tenth, q).unwrap();
        let q = parse_query(&fresh).unwrap();
        assert_eq!(q.seen.hint.as_deref(), Some(&[0][..]));
        assert_eq!(q.params[0], Value::List(vec![Value::Int(1), Value::Int(2)]));
        let (_, hinted) = exactly(&db, &fresh, q).unwrap();
        assert_eq!(hinted, quick);
        // Past 2^24 a list keeps the integer written either way.
        let past = body("[16777217]");
        let q = parse_query(&past).unwrap();
        let (_, hinted) = exactly(&db, &past, q).unwrap();
        assert_eq!(hinted[0], Value::List(vec![Value::Int(16_777_217)]));
    }

    /// A hint keeps no more than [`HINT_PLACES`] places, and a shard of
    /// long texts' hints no more than [`HINT_BUDGET`] together: a text
    /// naming more is read twice each request, as with no hint.
    #[test]
    fn a_hint_is_bounded_in_places_and_a_shard_in_bytes() {
        let db = RwLock::new(Database::new());
        let create = fennec_ql::parse_one("create collection hinted_many (fs [float])");
        db.write().unwrap().execute(&create.unwrap()).unwrap();
        let body = |n: usize| {
            let docs: Vec<String> = (1..=n).map(|d| format!("{{fs: ${d}}}")).collect();
            format!(
                r#"{{"query":"put hinted_many [{}]","params":[{}]}}"#,
                docs.join(", "),
                vec!["[0.1]"; n].join(", ")
            )
        };
        for (n, kept) in [(HINT_PLACES, true), (HINT_PLACES + 1, false)] {
            let b = body(n);
            for _ in 0..2 {
                let q = parse_query(&b).unwrap();
                let (_, params) = exactly(&db, &b, q).unwrap();
                assert_eq!(params[n - 1], Value::List(vec![Value::Float(0.1)]));
            }
            let q = parse_query(&b).unwrap();
            assert_eq!(
                q.seen.hint.as_ref().map(|h| h.len()),
                kept.then_some(n),
                "{n}"
            );
        }
        let mut shard = Hinted::default();
        let hint: Arc<[usize]> = (0..HINT_PLACES).collect::<Vec<_>>().into();
        for hash in 0..64 {
            shard.keep(hash, Some(Arc::clone(&hint)));
            assert!(shard.places <= HINT_BUDGET);
            assert_eq!(shard.places, shard.map.len() * HINT_PLACES);
        }
        shard.keep(63, None);
        assert_eq!(shard.places, shard.map.len() * HINT_PLACES);
    }

    /// A text is parsed once whether its body names parameters or not,
    /// before its text or after: each request after the first is handed
    /// the statement kept.
    #[test]
    fn a_text_is_parsed_once_however_its_body_is_written() {
        for body in [
            r#"{"query":"get kept_once_a where n = 1"}"#,
            r#"{"query":"get kept_once_b where n = $1","params":[1]}"#,
            r#"{"params":[1],"query":"get kept_once_c where n = $1"}"#,
            r#"{"sql":"get kept_once_d where n = $1","params":[1]}"#,
        ] {
            let first = parse_query(body).unwrap().stmt;
            let again = parse_query(body).unwrap().stmt;
            assert!(Arc::ptr_eq(&first, &again), "{body}");
        }
    }

    #[test]
    fn places_merge_ascending_each_once() {
        assert_eq!(merged(&[], &[]), None);
        assert_eq!(merged(&[1, 4], &[]).as_deref(), Some(&[1, 4][..]));
        assert_eq!(merged(&[], &[2]).as_deref(), Some(&[2][..]));
        assert_eq!(
            merged(&[1, 3, 5], &[0, 3, 6, 7]).as_deref(),
            Some(&[0, 1, 3, 5, 6, 7][..])
        );
    }
}

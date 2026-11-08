//! The OpenAPI 3.1 document of the server's own REST API, at
//! `GET /_openapi.json`: part of the API, not an extra -- what Swagger UI,
//! Redoc or an SDK generator reads, and what a client checks itself
//! against.
//!
//! One table, [`OPS`], is the document: every route the server answers, a
//! row each, written out as JSON once a process, the first time it is
//! asked ([`document`]). The dispatcher does not read it -- a table match
//! on every request was a cost the hot path does not pay -- so the two are
//! held to each other by tests: `tests` below reads every route pattern
//! fennec-http's and fennec-shard's dispatchers match (a slice pattern
//! against a request's segments, in whatever form the code writes it, a
//! segment by its index, a path or its prefix) and finds each in the table
//! where its code serves it, and `fennec-server`'s and `fennec-shard`'s
//! `tests/openapi.rs` send a request to every operation of the document a
//! file's server, a tenant node and a router serve, answered neither 404
//! nor 405. A route added without its row fails the first; a row without
//! its route, the second.
//!
//! The document is the binary's, not the server's: the same for every
//! server of a kind, whatever its flags or its data, so it asks no token
//! (as `/_health` and the studio's files ask none) and says nothing a
//! caller could not read in the source. A route served only under a flag
//! names it (`x-fennec-flag`); a node's admin routes are described, marked
//! with the admin token they take, not hidden.

use crate::http::{Method, Request, Response};
use std::sync::OnceLock;

/// The path the document is served at, on a server, a node and a router.
pub const PATH: &str = "/_openapi.json";

/// Which server a document describes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// `fennec-server` over one file: every route at the root.
    File,
    /// `fennec-server --dir`: the node's own routes at the root, each
    /// tenant's below `/t/{tenant}`.
    Node,
    /// `fennec-shard`: each tenant's routes below `/t/{tenant}`, forwarded
    /// to its node -- what a tenant's client calls -- and the router's own
    /// at its root. Its `/_shard/` routes are its admin's, in the sharding
    /// reference.
    Router,
}

/// The document of `kind`, written once a process.
pub fn document(kind: Kind) -> &'static [u8] {
    static DOCS: [OnceLock<Vec<u8>>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    DOCS[kind as usize].get_or_init(|| build(kind).into_bytes())
}

/// Whether `req` asks for the document: its path alone, compared before
/// a segment is made -- every other request pays a length compare.
#[inline]
pub fn asked(req: &Request) -> bool {
    req.path == PATH
}

/// The answer to `GET /_openapi.json` on a server of `kind`.
pub fn answer(kind: Kind, req: &Request) -> Response {
    match req.method {
        Method::Get | Method::Head => Response::json(200, document(kind)),
        _ => Response::error(405, "the OpenAPI document is read with GET")
            .header("Allow", "GET, HEAD"),
    }
}

// ----------------------------------------------------------------- the table

/// A file's server, at its root.
const FILE: u8 = 1;
/// A tenant node, at its root.
const NODE: u8 = 2;
/// Each tenant, below `/t/{tenant}` on its node and through a router.
const TENANT: u8 = 4;
/// `fennec-shard`, at its root.
const ROUTER: u8 = 8;

/// What an operation takes for a token.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Auth {
    /// The server's token or a scoped JSON Web Token: the document's
    /// default.
    Token,
    /// The server's token alone: what reads every row unscoped.
    Full,
    /// A tenant node's `--admin-token`.
    Admin,
    /// `--replication-token`.
    Replication,
    /// The server's token or the admin token; none where the server holds
    /// neither (`metrics::allowed`).
    Scrape,
    /// The router's `--token`; none where it holds none (`Router::refused`).
    Router,
    /// No token: the event's signature, against the hook's secrets.
    Signed,
    /// No token at all.
    Open,
}

/// An operation's request body.
#[derive(Clone, Copy)]
enum Body {
    None,
    /// JSON, required: the schema.
    Json(&'static str),
    /// JSON, which may be left out.
    Optional(&'static str),
    /// One `{query, params}` a line (`/batch`).
    Lines,
    /// A database image.
    Image,
}

/// An operation's answer where it succeeds.
#[derive(Clone, Copy)]
enum Answer {
    /// JSON read from or written to the data: `Fennec-Seq` and
    /// `Fennec-History` beside it.
    Data(u16, &'static str, &'static str),
    /// JSON: status, schema, what it is.
    Json(u16, &'static str, &'static str),
    /// JSON with the row's `ETag` (`crate::conditional`): status, schema,
    /// what it is.
    Tagged(u16, &'static str, &'static str),
    /// No body: status, what it means.
    Empty(u16, &'static str),
    /// Server-sent events.
    Events,
    /// One JSON object a line (`/_changes`).
    Lines,
    /// A body with no end (a replica's stream).
    Stream,
    /// A database image.
    Image,
    /// Prometheus's text format.
    Metrics,
    /// A studio file.
    Page(&'static str),
    /// This document.
    Spec,
}

struct Op {
    method: &'static str,
    path: &'static str,
    at: u8,
    id: &'static str,
    tag: &'static str,
    summary: &'static str,
    /// The flags a server serves it under, `""` for none.
    flag: &'static str,
    /// Below a tenant, where they differ: a node's tenants keep their
    /// change feed with `--replication-token` and their hooks' secrets
    /// through `/_admin/`, `--cdc` and `--hooks` being a file's alone.
    tenant_flag: Option<&'static str>,
    /// A GET that answers HEAD too: listed as a `head` operation of its
    /// own. Not every GET does -- the stream, the change feed and the
    /// admin's reads answer GET alone.
    head: bool,
    auth: Auth,
    /// Query and header parameters, by their name in `components`.
    params: &'static [&'static str],
    body: Body,
    answer: Answer,
}

const DEFAULT: Op = Op {
    method: "get",
    path: "/",
    at: FILE | TENANT,
    id: "",
    tag: "Collections",
    summary: "",
    flag: "",
    tenant_flag: None,
    head: false,
    auth: Auth::Token,
    params: &[],
    body: Body::None,
    answer: Answer::Json(200, OBJECT, ""),
};

const OBJECT: &str = r#"{"type":"object"}"#;
const READ: &[&str] = &["FennecAfter", "FennecHistory", "FennecRead"];
const WRITE: &[&str] = &["IdempotencyKey"];
const HELD: &[&str] = &["IdempotencyKey", "FennecWait"];
const STATEMENT: &[&str] = &[
    "IdempotencyKey",
    "FennecWait",
    "FennecAfter",
    "FennecHistory",
    "FennecRead",
    "IfNoneMatch",
];
const ROWS: &[&str] = &[
    "Filter",
    "Where",
    "Select",
    "Order",
    "Limit",
    "Offset",
    "Count",
    "Group",
    "Facet",
    "Lookup",
    "FennecAfter",
    "FennecHistory",
    "FennecRead",
];

/// Every route a server answers, in the order the document lists them.
/// Paths below a tenant (`TENANT`) are written as a file's server has
/// them; a node and a router put `/t/{tenant}` in front.
const OPS: &[Op] = &[
    // The collections, over REST.
    Op {
        id: "info",
        head: true,
        summary: "The server's version and its collections' names",
        params: READ,
        answer: Answer::Json(200, r##"{"$ref":"#/components/schemas/Info"}"##, "Version and names"),
        ..DEFAULT
    },
    Op {
        path: "/collections",
        id: "listCollections",
        head: true,
        summary: "Every collection's schema",
        params: READ,
        answer: Answer::Json(200, r##"{"$ref":"#/components/schemas/Schemas"}"##, "The schemas"),
        ..DEFAULT
    },
    Op {
        path: "/{collection}",
        id: "readRows",
        head: true,
        summary: "The rows the query string's filter matches; ?count their number, \
                  select and group aggregates, facet each value's count",
        params: ROWS,
        answer: Answer::Data(200, r##"{"$ref":"#/components/schemas/Read"}"##, "The rows"),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/{collection}",
        id: "insertRows",
        summary: "Insert a document or a list of them; an id that is taken is 409",
        params: &["Returning", "IdempotencyKey"],
        body: Body::Json(r##"{"$ref":"#/components/schemas/Documents"}"##),
        answer: Answer::Data(
            201,
            r##"{"$ref":"#/components/schemas/Written"}"##,
            "How many were inserted, or with ?returning the rows",
        ),
        ..DEFAULT
    },
    Op {
        method: "patch",
        path: "/{collection}",
        id: "updateRows",
        summary: "Set the body's fields on the rows the filter matches (a filter is required)",
        params: &["Filter", "Where", "IdempotencyKey"],
        body: Body::Json(r##"{"$ref":"#/components/schemas/Document"}"##),
        answer: Answer::Data(200, r##"{"$ref":"#/components/schemas/Outcome"}"##, "How many were updated"),
        ..DEFAULT
    },
    Op {
        method: "put",
        path: "/{collection}",
        id: "updateRowsPut",
        summary: "PATCH /{collection}, under PUT",
        params: &["Filter", "Where", "IdempotencyKey"],
        body: Body::Json(r##"{"$ref":"#/components/schemas/Document"}"##),
        answer: Answer::Data(200, r##"{"$ref":"#/components/schemas/Outcome"}"##, "How many were updated"),
        ..DEFAULT
    },
    Op {
        method: "delete",
        path: "/{collection}",
        id: "deleteRows",
        summary: "Delete the rows the filter matches (a filter is required)",
        params: &["Filter", "Where", "IdempotencyKey"],
        answer: Answer::Data(200, r##"{"$ref":"#/components/schemas/Outcome"}"##, "How many were deleted"),
        ..DEFAULT
    },
    Op {
        method: "patch",
        path: "/{collection}/all",
        id: "updateAll",
        summary: "Set the body's fields on every row: no filter, deliberately explicit",
        params: WRITE,
        body: Body::Json(r##"{"$ref":"#/components/schemas/Document"}"##),
        answer: Answer::Data(200, r##"{"$ref":"#/components/schemas/Outcome"}"##, "How many were updated"),
        ..DEFAULT
    },
    Op {
        method: "put",
        path: "/{collection}/all",
        id: "updateAllPut",
        summary: "PATCH /{collection}/all, under PUT",
        params: WRITE,
        body: Body::Json(r##"{"$ref":"#/components/schemas/Document"}"##),
        answer: Answer::Data(200, r##"{"$ref":"#/components/schemas/Outcome"}"##, "How many were updated"),
        ..DEFAULT
    },
    Op {
        method: "delete",
        path: "/{collection}/all",
        id: "deleteAll",
        summary: "Delete every row",
        params: WRITE,
        answer: Answer::Data(200, r##"{"$ref":"#/components/schemas/Outcome"}"##, "How many were deleted"),
        ..DEFAULT
    },
    // A row by its id, and its tag: optimistic concurrency by HTTP's own
    // preconditions (`crate::conditional`).
    Op {
        path: "/{collection}/{id}",
        id: "readRow",
        head: true,
        summary: "The row of that id, its tag in ETag; 304 where If-None-Match names that tag, \
                  412 where If-Match names another, 404 where there is no such row",
        params: &["IfMatch", "IfNoneMatchRow", "FennecAfter", "FennecHistory", "FennecRead"],
        answer: Answer::Tagged(200, r##"{"$ref":"#/components/schemas/Document"}"##, "The row"),
        ..DEFAULT
    },
    Op {
        method: "put",
        path: "/{collection}/{id}",
        id: "putRow",
        summary: "Write the body as the row of that id, whole: 201 where it made it, 200 over \
                  it; under If-Match only over the row tagged, under If-None-Match: * only \
                  where there is none (412 else, nothing written)",
        params: &["IfMatch", "IfNoneMatchRow", "IdempotencyKey"],
        body: Body::Json(r##"{"$ref":"#/components/schemas/Document"}"##),
        answer: Answer::Tagged(
            200,
            r##"{"$ref":"#/components/schemas/Outcome"}"##,
            "Written over the row; a conditional write's ETag is the row's new tag",
        ),
        ..DEFAULT
    },
    Op {
        method: "patch",
        path: "/{collection}/{id}",
        id: "updateRow",
        summary: "Set the body's fields on the row of that id; under If-Match only where it is \
                  the row tagged (* any), else 412 and nothing written",
        params: &["IfMatch", "IfNoneMatchRow", "IdempotencyKey"],
        body: Body::Json(r##"{"$ref":"#/components/schemas/Document"}"##),
        answer: Answer::Tagged(
            200,
            r##"{"$ref":"#/components/schemas/Outcome"}"##,
            "How many were updated; a conditional write's ETag is the row's new tag",
        ),
        ..DEFAULT
    },
    Op {
        method: "delete",
        path: "/{collection}/{id}",
        id: "deleteRow",
        summary: "Delete the row of that id; under If-Match only where it is the row tagged \
                  (* any), else 412 and nothing deleted",
        params: &["IfMatch", "IfNoneMatchRow", "IdempotencyKey"],
        answer: Answer::Data(200, r##"{"$ref":"#/components/schemas/Outcome"}"##, "How many were deleted"),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/{collection}/near",
        id: "nearRows",
        summary: "The rows nearest a vector, nearest first; the query string's filter and the \
                  body's where ANDed, a match making it hybrid",
        params: &["Filter", "Where", "FennecAfter", "FennecHistory", "FennecRead"],
        body: Body::Json(r##"{"$ref":"#/components/schemas/Near"}"##),
        answer: Answer::Data(200, r##"{"$ref":"#/components/schemas/Rows"}"##, "The rows"),
        ..DEFAULT
    },
    Op {
        path: "/{collection}/changes",
        id: "subscribe",
        tag: "Subscriptions",
        summary: "A subscription: the rows the filter matches, then each change to them",
        params: &[
            "Filter",
            "Where",
            "Select",
            "Order",
            "Limit",
            "Since",
            "FennecAfter",
            "FennecHistory",
            "FennecRead",
        ],
        answer: Answer::Events,
        ..DEFAULT
    },
    // Statements.
    Op {
        method: "post",
        path: "/query",
        id: "query",
        tag: "Statements",
        summary: "Run one FennecQL statement (or SQL's spellings of one) with its parameters",
        params: STATEMENT,
        body: Body::Json(r##"{"$ref":"#/components/schemas/Query"}"##),
        answer: Answer::Data(200, r##"{"$ref":"#/components/schemas/Answer"}"##, "The statement's answer"),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/batch",
        id: "batch",
        tag: "Statements",
        summary: "Run several statements, one a line, landing whole or not at all",
        params: HELD,
        body: Body::Lines,
        answer: Answer::Data(200, r##"{"$ref":"#/components/schemas/Answer"}"##, "The batch's answer"),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/_call/{statement}",
        id: "call",
        tag: "Statements",
        summary: "Call a named statement with its arguments, answered as /query answers it",
        params: HELD,
        body: Body::Optional(r##"{"$ref":"#/components/schemas/Call"}"##),
        answer: Answer::Data(200, r##"{"$ref":"#/components/schemas/Answer"}"##, "The statement's answer"),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/_hooks/{hook}",
        id: "hook",
        tag: "Webhooks",
        summary: "Another service's event, verified by its signature in place of a token, \
                  against the --hooks file's secrets or, below a tenant, those the node's \
                  /_admin/ routes set; every refusal before it verifies is one 401",
        flag: "--hooks",
        tenant_flag: Some("--admin-token"),
        auth: Auth::Signed,
        params: &[
            "StripeSignature",
            "WebhookId",
            "WebhookTimestamp",
            "WebhookSignature",
            "HubSignature",
            "GitHubDelivery",
        ],
        body: Body::Json("{}"),
        answer: Answer::Json(200, OBJECT, "Taken, durable"),
        ..DEFAULT
    },
    // The schema.
    Op {
        path: "/_schema",
        id: "schema",
        head: true,
        tag: "Schema",
        summary: "The schema as a description, its bound, scheduled and webhook statements beside it",
        params: &["As"],
        answer: Answer::Json(200, r##"{"$ref":"#/components/schemas/Description"}"##, "The description"),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/_schema/plan",
        id: "schemaPlan",
        tag: "Schema",
        summary: "What applying a description would do, writing nothing; ?mode=follow what the server lacks",
        params: &["Mode"],
        body: Body::Json(r##"{"$ref":"#/components/schemas/Description"}"##),
        answer: Answer::Json(200, r##"{"$ref":"#/components/schemas/Plan"}"##, "The plan"),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/_schema/apply",
        id: "schemaApply",
        tag: "Schema",
        summary: "Run a description's migrations, then what only adds, as one block",
        auth: Auth::Full,
        body: Body::Json(r##"{"$ref":"#/components/schemas/Description"}"##),
        answer: Answer::Json(200, r##"{"$ref":"#/components/schemas/Plan"}"##, "What was applied"),
        ..DEFAULT
    },
    // Change data capture.
    Op {
        path: "/_changes",
        id: "changes",
        tag: "Change data capture",
        summary: "Every write on disk after `since` (or a consumer's place), a JSON line each",
        flag: "--cdc",
        tenant_flag: Some("--replication-token"),
        auth: Auth::Full,
        params: &[
            "Since",
            "Consumer",
            "Limit",
            "Wait",
            "FennecAfter",
            "FennecHistory",
            "FennecRead",
        ],
        answer: Answer::Lines,
        ..DEFAULT
    },
    Op {
        path: "/_changes/consumers",
        id: "listConsumers",
        tag: "Change data capture",
        summary: "Each consumer, where it is and how far behind",
        flag: "--cdc",
        tenant_flag: Some("--replication-token"),
        auth: Auth::Full,
        answer: Answer::Json(200, r#"{"type":"array","items":{"type":"object"}}"#, "The consumers"),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/_changes/consumers/{consumer}",
        id: "makeConsumer",
        tag: "Change data capture",
        summary: "Make a consumer at `since` or the last write (201); one already there stays put (200)",
        flag: "--cdc",
        tenant_flag: Some("--replication-token"),
        auth: Auth::Full,
        body: Body::Optional(r##"{"$ref":"#/components/schemas/Since"}"##),
        answer: Answer::Json(201, OBJECT, "Made"),
        ..DEFAULT
    },
    Op {
        method: "patch",
        path: "/_changes/consumers/{consumer}",
        id: "moveConsumer",
        tag: "Change data capture",
        summary: "Move a consumer: commit what it read, or go back",
        flag: "--cdc",
        tenant_flag: Some("--replication-token"),
        auth: Auth::Full,
        body: Body::Json(r##"{"$ref":"#/components/schemas/Since"}"##),
        answer: Answer::Json(200, OBJECT, "Moved"),
        ..DEFAULT
    },
    Op {
        method: "delete",
        path: "/_changes/consumers/{consumer}",
        id: "deleteConsumer",
        tag: "Change data capture",
        summary: "Forget a consumer",
        flag: "--cdc",
        tenant_flag: Some("--replication-token"),
        auth: Auth::Full,
        answer: Answer::Json(200, OBJECT, "Forgotten"),
        ..DEFAULT
    },
    // Replication.
    Op {
        path: "/_replication",
        at: FILE | TENANT | ROUTER,
        id: "replicationStream",
        tag: "Replication",
        summary: "The stream a replica is fed: a body with no end",
        flag: "--replication-token",
        auth: Auth::Replication,
        params: &["Since"],
        answer: Answer::Stream,
        ..DEFAULT
    },
    Op {
        path: "/_replication/status",
        at: FILE | TENANT | ROUTER,
        id: "replicationStatus",
        tag: "Replication",
        summary: "Where this server's replication stands",
        flag: "--replication-token",
        auth: Auth::Replication,
        answer: Answer::Json(200, OBJECT, "The status"),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/_replication/promote",
        at: FILE | TENANT | ROUTER,
        id: "promote",
        tag: "Replication",
        summary: "Promote this replica: it takes writes from now on",
        flag: "--replication-token",
        auth: Auth::Replication,
        answer: Answer::Json(200, OBJECT, "Promoted: {promoted, seq, history}"),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/_replication/tenants",
        at: NODE,
        id: "nodeStream",
        tag: "Replication",
        summary: "Every tenant of the node over one stream, from the places the body lists",
        flag: "--replication-token",
        auth: Auth::Replication,
        body: Body::Json(OBJECT),
        answer: Answer::Stream,
        ..DEFAULT
    },
    // The server.
    Op {
        path: "/_whoami",
        at: FILE | NODE | TENANT,
        id: "whoami",
        head: true,
        tag: "Server",
        summary: "Who the server takes the token for: its roles, its rules, the statements it may call",
        answer: Answer::Json(200, r##"{"$ref":"#/components/schemas/Whoami"}"##, "The token's reading"),
        ..DEFAULT
    },
    Op {
        path: "/_stats/statements",
        id: "statementStats",
        head: true,
        tag: "Server",
        summary: "Each statement's count and time, by its shape",
        auth: Auth::Full,
        answer: Answer::Json(200, OBJECT, "The counts"),
        ..DEFAULT
    },
    Op {
        method: "delete",
        path: "/_stats/statements",
        id: "resetStatementStats",
        tag: "Server",
        summary: "Start the counts again",
        auth: Auth::Full,
        answer: Answer::Empty(204, "Reset"),
        ..DEFAULT
    },
    Op {
        path: "/_stats/statements",
        at: NODE,
        id: "nodeStatementStats",
        head: true,
        tag: "Server",
        summary: "Every tenant's statements, by shape, each named",
        auth: Auth::Admin,
        answer: Answer::Json(200, OBJECT, "The counts"),
        ..DEFAULT
    },
    Op {
        method: "delete",
        path: "/_stats/statements",
        at: NODE,
        id: "nodeResetStatementStats",
        tag: "Server",
        summary: "Start every tenant's counts again",
        auth: Auth::Admin,
        answer: Answer::Empty(204, "Reset"),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/_mcp",
        id: "mcp",
        tag: "Agents",
        summary: "The MCP endpoint for agents: one JSON-RPC message a request",
        flag: "--mcp",
        body: Body::Json(r##"{"$ref":"#/components/schemas/JsonRpc"}"##),
        answer: Answer::Json(200, r##"{"$ref":"#/components/schemas/JsonRpc"}"##, "The JSON-RPC answer"),
        ..DEFAULT
    },
    Op {
        path: "/_metrics",
        at: FILE | NODE,
        id: "metrics",
        head: true,
        tag: "Server",
        summary: "Counters and gauges, Prometheus's text format",
        auth: Auth::Scrape,
        answer: Answer::Metrics,
        ..DEFAULT
    },
    Op {
        path: "/_metrics",
        at: ROUTER,
        id: "routerMetrics",
        head: true,
        tag: "Server",
        summary: "The router's counters and gauges and its directory's, Prometheus's text format",
        auth: Auth::Router,
        answer: Answer::Metrics,
        ..DEFAULT
    },
    Op {
        path: "/_health",
        at: FILE | NODE,
        id: "health",
        head: true,
        tag: "Server",
        summary: "Whether the server is up: no token, no lock",
        auth: Auth::Open,
        answer: Answer::Json(200, r#"{"type":"object","properties":{"ok":{"type":"boolean"}}}"#, "Up"),
        ..DEFAULT
    },
    Op {
        path: "/_studio/",
        at: FILE | NODE | ROUTER,
        id: "studio",
        head: true,
        tag: "Server",
        summary: "fennec studio's page: the token is the page's to send",
        flag: "--studio",
        auth: Auth::Open,
        answer: Answer::Page("text/html"),
        ..DEFAULT
    },
    Op {
        path: "/_studio/{file}",
        at: FILE | NODE | ROUTER,
        id: "studioFile",
        head: true,
        tag: "Server",
        summary: "One of the studio's files",
        flag: "--studio",
        auth: Auth::Open,
        answer: Answer::Page("*/*"),
        ..DEFAULT
    },
    Op {
        path: PATH,
        at: FILE | NODE | ROUTER,
        id: "openapi",
        head: true,
        tag: "Server",
        summary: "This document",
        auth: Auth::Open,
        answer: Answer::Spec,
        ..DEFAULT
    },
    // A tenant node's administration, the surface a router drives.
    Op {
        path: "/_admin/stats",
        at: NODE,
        id: "adminStats",
        tag: "Admin",
        summary: "The node's tenants, disk, memory and open tenants",
        flag: "--admin-token",
        auth: Auth::Admin,
        ..DEFAULT
    },
    Op {
        path: "/_admin/open",
        at: NODE,
        id: "adminOpen",
        tag: "Admin",
        summary: "The open tenants: name, memory, frozen",
        flag: "--admin-token",
        auth: Auth::Admin,
        answer: Answer::Json(200, r#"{"type":"array","items":{"type":"object"}}"#, "The open tenants"),
        ..DEFAULT
    },
    Op {
        path: "/_admin/tenants",
        at: NODE,
        id: "adminTenants",
        tag: "Admin",
        summary: "The tenants' names on disk",
        flag: "--admin-token",
        auth: Auth::Admin,
        answer: Answer::Json(200, r#"{"type":"array","items":{"type":"string"}}"#, "The names"),
        ..DEFAULT
    },
    Op {
        path: "/_admin/sizes",
        at: NODE,
        id: "adminSizes",
        tag: "Admin",
        summary: "Each tenant's bytes on disk",
        flag: "--admin-token",
        auth: Auth::Admin,
        ..DEFAULT
    },
    Op {
        path: "/_admin/quotas",
        at: NODE,
        id: "adminQuotas",
        tag: "Admin",
        summary: "Every tenant's quota and usage",
        flag: "--admin-token",
        auth: Auth::Admin,
        answer: Answer::Json(200, r#"{"type":"array","items":{"type":"object"}}"#, "The quotas"),
        ..DEFAULT
    },
    Op {
        method: "put",
        path: "/_admin/tenants/{tenant}",
        at: NODE,
        id: "adminCreate",
        tag: "Admin",
        summary: "Make an empty tenant (409 if there is one)",
        flag: "--admin-token",
        auth: Auth::Admin,
        answer: Answer::Json(201, OBJECT, "Made: {created, lineage, instance}"),
        ..DEFAULT
    },
    Op {
        method: "delete",
        path: "/_admin/tenants/{tenant}",
        at: NODE,
        id: "adminDelete",
        tag: "Admin",
        summary: "Close and remove a tenant",
        flag: "--admin-token",
        auth: Auth::Admin,
        answer: Answer::Empty(204, "Removed"),
        ..DEFAULT
    },
    Op {
        path: "/_admin/tenants/{tenant}/quota",
        at: NODE,
        id: "adminQuota",
        tag: "Admin",
        summary: "A tenant's quota, what is set and its usage",
        flag: "--admin-token",
        auth: Auth::Admin,
        ..DEFAULT
    },
    Op {
        method: "put",
        path: "/_admin/tenants/{tenant}/quota",
        at: NODE,
        id: "adminSetQuota",
        tag: "Admin",
        summary: "Set a tenant's quota over the node's, the whole set replaced",
        flag: "--admin-token",
        auth: Auth::Admin,
        body: Body::Json(r##"{"$ref":"#/components/schemas/Quota"}"##),
        ..DEFAULT
    },
    Op {
        path: "/_admin/tenants/{tenant}/hooks",
        at: NODE,
        id: "adminHooks",
        tag: "Admin",
        summary: "A tenant's webhook consumers, never a secret",
        flag: "--admin-token",
        auth: Auth::Admin,
        ..DEFAULT
    },
    Op {
        method: "put",
        path: "/_admin/tenants/{tenant}/hooks/{hook}",
        at: NODE,
        id: "adminSetHook",
        tag: "Admin",
        summary: "Set a hook's scheme and secrets, kept beside the tenant's file",
        flag: "--admin-token",
        auth: Auth::Admin,
        body: Body::Json(OBJECT),
        ..DEFAULT
    },
    Op {
        method: "delete",
        path: "/_admin/tenants/{tenant}/hooks/{hook}",
        at: NODE,
        id: "adminDeleteHook",
        tag: "Admin",
        summary: "Remove a hook's secrets",
        flag: "--admin-token",
        auth: Auth::Admin,
        ..DEFAULT
    },
    Op {
        path: "/_admin/tenants/{tenant}/hook-secrets",
        at: NODE,
        id: "adminHookSecrets",
        tag: "Admin",
        summary: "A tenant's hook secrets whole, for a router copying them onto a copy",
        flag: "--admin-token",
        auth: Auth::Admin,
        ..DEFAULT
    },
    Op {
        method: "put",
        path: "/_admin/tenants/{tenant}/hook-secrets",
        at: NODE,
        id: "adminSetHookSecrets",
        tag: "Admin",
        summary: "Install a tenant's hook secrets whole, their version kept",
        flag: "--admin-token",
        auth: Auth::Admin,
        body: Body::Json(OBJECT),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/_admin/tenants/{tenant}/schema",
        at: NODE,
        id: "adminSchema",
        tag: "Admin",
        summary: "Apply a description to a tenant, as /_schema/apply does",
        flag: "--admin-token",
        auth: Auth::Admin,
        body: Body::Json(r##"{"$ref":"#/components/schemas/Description"}"##),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/_admin/tenants/{tenant}/query",
        at: NODE,
        id: "adminQuery",
        tag: "Admin",
        summary: "A read of a tenant, answered as /query answers it; a write is refused",
        flag: "--admin-token",
        auth: Auth::Admin,
        body: Body::Json(r##"{"$ref":"#/components/schemas/Query"}"##),
        answer: Answer::Data(200, r##"{"$ref":"#/components/schemas/Answer"}"##, "The read's answer"),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/_admin/tenants/{tenant}/reclaim",
        at: NODE,
        id: "adminReclaim",
        tag: "Admin",
        summary: "Remove a tenant's file only where it carries one of the instances named",
        flag: "--admin-token",
        auth: Auth::Admin,
        body: Body::Json(OBJECT),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/_admin/tenants/{tenant}/freeze",
        at: NODE,
        id: "adminFreeze",
        tag: "Admin",
        summary: "Refuse a tenant's writes, once those in flight landed",
        flag: "--admin-token",
        auth: Auth::Admin,
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/_admin/tenants/{tenant}/thaw",
        at: NODE,
        id: "adminThaw",
        tag: "Admin",
        summary: "Take a tenant's writes again",
        flag: "--admin-token",
        auth: Auth::Admin,
        ..DEFAULT
    },
    Op {
        path: "/_admin/tenants/{tenant}/file",
        at: NODE,
        id: "adminExport",
        tag: "Admin",
        summary: "A tenant's whole image",
        flag: "--admin-token",
        auth: Auth::Admin,
        answer: Answer::Image,
        ..DEFAULT
    },
    Op {
        method: "put",
        path: "/_admin/tenants/{tenant}/file",
        at: NODE,
        id: "adminImport",
        tag: "Admin",
        summary: "Install an image as a new tenant",
        flag: "--admin-token",
        auth: Auth::Admin,
        body: Body::Image,
        answer: Answer::Json(201, OBJECT, "Installed: {imported, lineage, instance}"),
        ..DEFAULT
    },
    Op {
        path: "/_admin/tenants/{tenant}/lineage",
        at: NODE,
        id: "adminLineage",
        tag: "Admin",
        summary: "A tenant's lineage and history",
        flag: "--admin-token",
        auth: Auth::Admin,
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/_admin/tenants/{tenant}/promote",
        at: NODE,
        id: "adminPromote",
        tag: "Admin",
        summary: "This node's copy of a tenant takes its writes",
        flag: "--admin-token",
        auth: Auth::Admin,
        body: Body::Optional(OBJECT),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/_admin/tenants/{tenant}/follow",
        at: NODE,
        id: "adminFollow",
        tag: "Admin",
        summary: "Follow the tenant's primary at `from`, or this node's upstream",
        flag: "--admin-token",
        auth: Auth::Admin,
        body: Body::Optional(OBJECT),
        ..DEFAULT
    },
    Op {
        method: "post",
        path: "/_admin/lease",
        at: NODE,
        id: "adminGrantLease",
        tag: "Admin",
        summary: "A router's grant of the node's lease",
        flag: "--admin-token --lease",
        auth: Auth::Admin,
        body: Body::Json(OBJECT),
        ..DEFAULT
    },
    Op {
        path: "/_admin/lease",
        at: NODE,
        id: "adminLease",
        tag: "Admin",
        summary: "The node's lease as it runs",
        flag: "--admin-token --lease",
        auth: Auth::Admin,
        ..DEFAULT
    },
];

/// One operation of a document: its path there, its id there, the flags it
/// is served under there, and its row.
struct Listed {
    path: String,
    id: String,
    flag: &'static str,
    op: &'static Op,
}

/// Every operation of the document of `kind`.
fn operations(kind: Kind) -> Vec<Listed> {
    let mut out = Vec::new();
    // A node's and a router's own routes come first, then each tenant's
    // under its prefix.
    let (root, under) = match kind {
        Kind::File => (FILE, 0),
        Kind::Node => (NODE, TENANT),
        Kind::Router => (ROUTER, TENANT),
    };
    for op in OPS.iter().filter(|op| op.at & root != 0) {
        out.push(Listed {
            path: op.path.to_string(),
            id: op.id.to_string(),
            flag: op.flag,
            op,
        });
    }
    for op in OPS.iter().filter(|op| op.at & under != 0) {
        let path = match op.path {
            "/" => "/t/{tenant}".to_string(),
            p => format!("/t/{{tenant}}{p}"),
        };
        // An id is the document's: a node and a router hold `/_whoami` or
        // `/_replication` and each tenant's both, and a tenant's operation
        // has one id through either, so a client written from a node's
        // document calls a router.
        let mut id = String::from("tenant");
        let mut first = op.id.chars();
        id.extend(first.next().map(|c| c.to_ascii_uppercase()));
        id.push_str(first.as_str());
        let flag = op.tenant_flag.unwrap_or(op.flag);
        out.push(Listed { path, id, flag, op });
    }
    out
}

// ------------------------------------------------------------- the writing

fn build(kind: Kind) -> String {
    let mut o = String::with_capacity(64 * 1024);
    o.push_str(r#"{"openapi":"3.1.0","info":{"title":"#);
    str_into(
        &mut o,
        match kind {
            Kind::File => "fennecdb: fennec-server",
            Kind::Node => "fennecdb: a tenant node",
            Kind::Router => "fennecdb: fennec-shard",
        },
    );
    // The document's own version, not the binary's: a document asking no
    // token must not tell what a build is -- a version is what a scan
    // matches against advisories, and `GET /` names it only to a token.
    // Moved by hand where an operation's contract changes.
    o.push_str(r#","version":"#);
    str_into(&mut o, DOCUMENT_VERSION);
    o.push_str(r#","description":"#);
    let mut about = String::from(match kind {
        Kind::File => "fennec-server's REST API over one database file.",
        Kind::Node => {
            "A tenant node's REST API (fennec-server --dir): each tenant's routes below \
             /t/{tenant}, the node's own at the root, /_admin/ with --admin-token."
        }
        Kind::Router => {
            "fennec-shard's REST API: each tenant's routes below /t/{tenant}, forwarded to \
             its node, and the router's own at the root. Its /_shard/ routes are its \
             admin's, in the sharding reference."
        }
    });
    about.push_str(ABOUT);
    str_into(&mut o, &about);
    o.push_str(r#","license":{"name":"Apache-2.0","identifier":"Apache-2.0"}}"#);
    o.push_str(r#","externalDocs":{"url":"https://fennecdb.com/docs/http-reference"}"#);
    o.push_str(r#","servers":[{"url":"/"}],"security":[{"token":[]},{"jwt":[]}],"tags":["#);
    let ops = operations(kind);
    let mut tags: Vec<&str> = Vec::new();
    for l in &ops {
        if !tags.contains(&l.op.tag) {
            tags.push(l.op.tag);
        }
    }
    for (i, t) in tags.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        o.push_str(r#"{"name":"#);
        str_into(&mut o, t);
        let about = TAGS
            .iter()
            .find(|(name, _)| name == t)
            .map_or("", |(_, a)| a);
        o.push_str(r#","description":"#);
        str_into(&mut o, about);
        o.push('}');
    }
    o.push_str(r#"],"paths":{"#);
    // One path item a path, its operations in the table's order.
    let mut done: Vec<&str> = Vec::new();
    for Listed { path, .. } in &ops {
        if done.contains(&path.as_str()) {
            continue;
        }
        done.push(path);
        if done.len() > 1 {
            o.push(',');
        }
        str_into(&mut o, path);
        o.push_str(":{");
        let names = placeholders(path);
        if !names.is_empty() {
            o.push_str(r#""parameters":["#);
            for (i, n) in names.iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }
                ref_into(&mut o, "parameters", n);
            }
            o.push_str("],");
        }
        let mut first = true;
        for l in ops.iter().filter(|l| &l.path == path) {
            if !first {
                o.push(',');
            }
            first = false;
            operation_into(&mut o, l, false);
            if l.op.head {
                o.push(',');
                operation_into(&mut o, l, true);
            }
        }
        o.push('}');
    }
    o.push_str(r#"},"components":"#);
    o.push_str(COMPONENTS);
    o.push('}');
    o
}

/// `info.version`: the document's, not the binary's (see [`build`]).
const DOCUMENT_VERSION: &str = "1";

const ABOUT: &str = " A request takes `Authorization: Bearer` with the server's token \
(--http-token) or a scoped JSON Web Token (--jwt-secret, --jwt-keys), held to the --policy \
rules; a server started with neither takes requests with no token. Every answer carries \
X-Request-Id; a refusal is {\"error\": \"...\"} under a status that says what to do about \
it, and a 503 says when to come back in Retry-After and its body's retry_after. A route \
marked x-fennec-flag is served under those flags. A GET answering HEAD lists a head \
operation beside it, and every path answers a CORS preflight under --http-cors.";

/// Each tag an operation names, and what its routes are.
const TAGS: &[(&str, &str)] = &[
    (
        "Collections",
        "A collection's rows over REST: read by the query string, written by the body.",
    ),
    (
        "Subscriptions",
        "A read that follows its rows: the shape, then each change, as server-sent events.",
    ),
    (
        "Statements",
        "FennecQL as text with its parameters, a batch landing whole, a named statement called.",
    ),
    (
        "Webhooks",
        "Another service's events, taken by a named statement once verified.",
    ),
    (
        "Schema",
        "The schema as a description, and a description planned or applied.",
    ),
    (
        "Change data capture",
        "Every write on disk, a JSON line each, and the consumers reading them.",
    ),
    (
        "Replication",
        "What a replica, an archiver or a router reads, and a replica's promotion.",
    ),
    (
        "Server",
        "The server's own: who a token is, its counts, its health, the studio, this document.",
    ),
    (
        "Agents",
        "The MCP endpoint: an agent's tools, each a statement held to the token.",
    ),
    (
        "Admin",
        "A tenant node's administration, the surface a router drives.",
    ),
];

/// A row's operation, or with `head` the HEAD beside its GET: its status
/// and headers, no body, its id the GET's with `Head` after it.
fn operation_into(o: &mut String, l: &Listed, head: bool) {
    let op = l.op;
    str_into(o, if head { "head" } else { op.method });
    o.push_str(r#":{"operationId":"#);
    match head {
        true => str_into(o, &format!("{}Head", l.id)),
        false => str_into(o, &l.id),
    }
    o.push_str(r#","tags":["#);
    str_into(o, op.tag);
    o.push_str(r#"],"summary":"#);
    match head {
        true => str_into(o, &format!("The GET's head: {}", op.summary)),
        false => str_into(o, op.summary),
    }
    if !l.flag.is_empty() {
        o.push_str(r#","x-fennec-flag":"#);
        str_into(o, l.flag);
    }
    let security = match op.auth {
        Auth::Token => None,
        Auth::Full => Some(r#"[{"token":[]}]"#),
        Auth::Admin => Some(r#"[{"admin":[]}]"#),
        Auth::Replication => Some(r#"[{"replication":[]}]"#),
        Auth::Scrape => Some(r#"[{"token":[]},{"admin":[]},{}]"#),
        Auth::Router => Some(r#"[{"router":[]},{}]"#),
        Auth::Signed | Auth::Open => Some("[]"),
    };
    if let Some(s) = security {
        o.push_str(r#","security":"#);
        o.push_str(s);
    }
    if !op.params.is_empty() {
        o.push_str(r#","parameters":["#);
        for (i, p) in op.params.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            ref_into(o, "parameters", p);
        }
        o.push(']');
    }
    let body = match op.body {
        Body::None => None,
        Body::Json(s) => Some((true, "application/json", s)),
        Body::Optional(s) => Some((false, "application/json", s)),
        Body::Lines => Some((
            true,
            "application/x-ndjson",
            r#"{"type":"string","description":"One {\"query\", \"params\"} object a line"}"#,
        )),
        Body::Image => Some((
            true,
            "application/octet-stream",
            r#"{"type":"string","contentMediaType":"application/octet-stream"}"#,
        )),
    };
    if let Some((required, media, schema)) = body {
        o.push_str(r#","requestBody":{"required":"#);
        o.push_str(if required { "true" } else { "false" });
        o.push_str(r#","content":{"#);
        str_into(o, media);
        o.push_str(r#":{"schema":"#);
        o.push_str(schema);
        o.push_str("}}}");
    }
    o.push_str(r#","responses":{"#);
    answer_into(o, op.answer, head);
    if op.params.contains(&"IfNoneMatch") {
        // A read polled with If-None-Match is answered so while nothing
        // it reads was written.
        o.push_str(r##","304":{"$ref":"#/components/responses/NotModified"}"##);
    }
    // A row's preconditions (`crate::conditional`): a GET naming its tag
    // is 304, a write or a read they refuse 412, a PUT making the row 201.
    if op.params.contains(&"IfNoneMatchRow") && op.method == "get" {
        o.push_str(r##","304":{"$ref":"#/components/responses/RowNotModified"}"##);
    }
    if op.method == "put" && op.params.contains(&"IfNoneMatchRow") {
        o.push_str(r##","201":{"$ref":"#/components/responses/RowMade"}"##);
    }
    if op.params.contains(&"IfMatch") {
        o.push_str(r##","412":{"$ref":"#/components/responses/PreconditionFailed"}"##);
    }
    match op.auth {
        Auth::Open => {}
        // Its signature is its token: every refusal before it verifies is
        // this one answer, whatever failed.
        Auth::Signed => o.push_str(r##","401":{"$ref":"#/components/responses/Unverified"}"##),
        _ => o.push_str(r##","401":{"$ref":"#/components/responses/Unauthorized"}"##),
    }
    o.push_str(r##","503":{"$ref":"#/components/responses/Unavailable"}"##);
    o.push_str(r##","default":{"$ref":"#/components/responses/Refused"}}}"##);
}

fn answer_into(o: &mut String, answer: Answer, head: bool) {
    const ID: &str = r##""X-Request-Id":{"$ref":"#/components/headers/RequestId"}"##;
    const SEQ: &str = r##","Fennec-Seq":{"$ref":"#/components/headers/FennecSeq"},"Fennec-History":{"$ref":"#/components/headers/FennecHistory"}"##;
    let (status, what, media, schema, seq) = match answer {
        Answer::Data(s, schema, what) => (s, what, "application/json", schema, true),
        Answer::Json(s, schema, what) | Answer::Tagged(s, schema, what) => (
            s,
            if what.is_empty() { "Done" } else { what },
            "application/json",
            schema,
            false,
        ),
        Answer::Empty(s, what) => {
            o.push('"');
            o.push_str(&s.to_string());
            o.push_str(r#"":{"description":"#);
            str_into(o, what);
            o.push_str(r#","headers":{"#);
            o.push_str(ID);
            o.push_str("}}");
            return;
        }
        Answer::Events => (
            200,
            "A server-sent events stream: the shape, then each change as a diff",
            "text/event-stream",
            r#"{"type":"string"}"#,
            false,
        ),
        Answer::Lines => (
            200,
            "One JSON object a line, a write each; Fennec-Next is the next since",
            "application/x-ndjson",
            r#"{"type":"string"}"#,
            true,
        ),
        Answer::Stream => (
            200,
            "A body with no end, the stream a replica reads",
            "application/octet-stream",
            r#"{"type":"string","contentMediaType":"application/octet-stream"}"#,
            false,
        ),
        Answer::Image => (
            200,
            "The image",
            "application/octet-stream",
            r#"{"type":"string","contentMediaType":"application/octet-stream"}"#,
            false,
        ),
        Answer::Metrics => (
            200,
            "Prometheus's text format",
            "text/plain",
            r#"{"type":"string"}"#,
            false,
        ),
        Answer::Page(media) => (200, "The file", media, r#"{"type":"string"}"#, false),
        Answer::Spec => (
            200,
            "This OpenAPI 3.1 document",
            "application/json",
            r#"{"type":"object","description":"An OpenAPI 3.1 document"}"#,
            false,
        ),
    };
    o.push('"');
    o.push_str(&status.to_string());
    o.push_str(r#"":{"description":"#);
    str_into(o, what);
    o.push_str(r#","headers":{"#);
    o.push_str(ID);
    if matches!(answer, Answer::Tagged(..)) {
        o.push_str(r##","ETag":{"$ref":"#/components/headers/ETag"}"##);
    }
    if seq {
        o.push_str(SEQ);
        if matches!(answer, Answer::Lines) {
            o.push_str(r##","Fennec-Next":{"$ref":"#/components/headers/FennecNext"}"##);
        }
    }
    if head {
        o.push_str("}}");
        return;
    }
    o.push_str(r#"},"content":{"#);
    str_into(o, media);
    o.push_str(r#":{"schema":"#);
    o.push_str(schema);
    o.push_str("}}}");
}

/// The `{name}`s of a path template, in order.
fn placeholders(path: &str) -> Vec<&str> {
    path.split('/')
        .filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
        .collect()
}

fn ref_into(o: &mut String, kind: &str, name: &str) {
    o.push_str(r##"{"$ref":"#/components/"##);
    o.push_str(kind);
    o.push('/');
    o.push_str(name);
    o.push_str(r#""}"#);
}

fn str_into(o: &mut String, s: &str) {
    fennec_core::json::escape_into(o, s);
}

/// What every document shares: the schemes, the parameters, the headers,
/// the schemas and the refusals. JSON as written here, held to the
/// OpenAPI shape by the tests (every `$ref` resolves).
const COMPONENTS: &str = r##"{
"securitySchemes":{
"token":{"type":"http","scheme":"bearer","description":"The server's token (--http-token): every collection, unscoped."},
"jwt":{"type":"http","scheme":"bearer","bearerFormat":"JWT","description":"A scoped token: a JSON Web Token signed under --jwt-secret or --jwt-keys (HS256, RS256, ES256), held to the --policy rules at every level, on a node bound to the tenant it names."},
"admin":{"type":"http","scheme":"bearer","description":"A tenant node's --admin-token."},
"replication":{"type":"http","scheme":"bearer","description":"--replication-token: what a replica, an archiver or a router sends."},
"router":{"type":"http","scheme":"bearer","description":"fennec-shard's --token: its admin's."}},
"parameters":{
"collection":{"name":"collection","in":"path","required":true,"description":"A collection's name.","schema":{"type":"string"}},
"id":{"name":"id","in":"path","required":true,"description":"A row's id: digits alone.","schema":{"type":"integer","minimum":0}},
"tenant":{"name":"tenant","in":"path","required":true,"description":"A tenant's name: its file on its node.","schema":{"type":"string"}},
"statement":{"name":"statement","in":"path","required":true,"description":"A named statement (create statement).","schema":{"type":"string"}},
"hook":{"name":"hook","in":"path","required":true,"description":"A webhook consumer: a line of the hooks file, or a tenant's hook.","schema":{"type":"string"}},
"consumer":{"name":"consumer","in":"path","required":true,"description":"A change-stream consumer.","schema":{"type":"string"}},
"file":{"name":"file","in":"path","required":true,"description":"A file of the studio.","schema":{"type":"string"}},
"Filter":{"name":"filter","in":"query","style":"form","explode":true,"description":"field=operator.value, each an AND: eq neq lt lte gt gte like has in is, not. before one; a value alone is an equality (?year=2024).","schema":{"type":"object","additionalProperties":{"type":"string"}}},
"Where":{"name":"where","in":"query","description":"A FennecQL filter, for what the pattern cannot say: year >= 2024 and tags has \"rust\".","schema":{"type":"string"}},
"Select":{"name":"select","in":"query","description":"The fields, or aggregates: title,year or status,count(*),sum(total).","schema":{"type":"string"}},
"Order":{"name":"order","in":"query","description":"year.desc,title.asc; a collation among the modifiers: name.und.desc.","schema":{"type":"string"}},
"Limit":{"name":"limit","in":"query","schema":{"type":"integer","minimum":0}},
"Offset":{"name":"offset","in":"query","schema":{"type":"integer","minimum":0}},
"Count":{"name":"count","in":"query","allowEmptyValue":true,"description":"Present: the number of matching rows, not the rows.","schema":{"type":"string"}},
"Group":{"name":"group","in":"query","description":"Aggregates per group: status.","schema":{"type":"string"}},
"Facet":{"name":"facet","in":"query","description":"Each value's count beside the rows: brand top 5,color.","schema":{"type":"string"}},
"Lookup":{"name":"lookup","in":"query","description":"Another collection's matching documents attached to each row.","schema":{"type":"string"}},
"Returning":{"name":"returning","in":"query","description":"The rows a write made, not their count: id,title (* every field).","schema":{"type":"string"}},
"Since":{"name":"since","in":"query","description":"The change to start after.","schema":{"type":"integer","minimum":0}},
"Consumer":{"name":"consumer","in":"query","description":"Start from where this consumer committed.","schema":{"type":"string"}},
"Wait":{"name":"wait","in":"query","description":"Milliseconds to wait for a write when there is none yet.","schema":{"type":"integer","minimum":0}},
"As":{"name":"as","in":"query","description":"fennecql: the schema as its statements.","schema":{"type":"string","enum":["fennecql"]}},
"Mode":{"name":"mode","in":"query","description":"follow: only what the description declares and the server lacks.","schema":{"type":"string","enum":["follow"]}},
"IdempotencyKey":{"name":"Idempotency-Key","in":"header","description":"Makes a write once: a retry with the same key and request is answered as the first try was; the key with another request is 422.","schema":{"type":"string"}},
"FennecWait":{"name":"Fennec-Wait","in":"header","description":"Milliseconds to hold a set or a del that writes nothing, or a call whose body does, until it writes (30000 at most).","schema":{"type":"integer","minimum":0,"maximum":30000}},
"FennecAfter":{"name":"Fennec-After","in":"header","description":"Answer from nothing older than this change (a Fennec-Seq an answer named): a replica waits for it, 504 past the wait.","schema":{"type":"integer","minimum":0}},
"FennecHistory":{"name":"Fennec-History","in":"header","description":"The history Fennec-After's change is of, as an answer named it.","schema":{"type":"string"}},
"FennecRead":{"name":"Fennec-Read","in":"header","description":"replica: a read a router may answer from a read replica; every server runs it read-only.","schema":{"type":"string","enum":["replica"]}},
"IfNoneMatch":{"name":"If-None-Match","in":"header","description":"A read's last ETag: 304 while nothing it reads was written.","schema":{"type":"string"}},
"IfMatch":{"name":"If-Match","in":"header","description":"The row's ETag (a list of them), or *: answered or written only where the row is one tagged (* any row there), compared strongly, under the write lock that writes it; else 412 and nothing written.","schema":{"type":"string"}},
"IfNoneMatchRow":{"name":"If-None-Match","in":"header","description":"A read: the row's last ETag, 304 while it is that row. A write: *, made only where there is no such row (a PUT's create), or tags it must not be; else 412 and nothing written.","schema":{"type":"string"}},
"StripeSignature":{"name":"Stripe-Signature","in":"header","description":"A Stripe hook's signature.","schema":{"type":"string"}},
"WebhookId":{"name":"webhook-id","in":"header","description":"A Standard Webhooks event's id (or svix-id).","schema":{"type":"string"}},
"WebhookTimestamp":{"name":"webhook-timestamp","in":"header","description":"A Standard Webhooks event's time (or svix-timestamp).","schema":{"type":"string"}},
"WebhookSignature":{"name":"webhook-signature","in":"header","description":"A Standard Webhooks event's signature (or svix-signature).","schema":{"type":"string"}},
"HubSignature":{"name":"X-Hub-Signature-256","in":"header","description":"A GitHub hook's signature.","schema":{"type":"string"}},
"GitHubDelivery":{"name":"X-GitHub-Delivery","in":"header","description":"A GitHub event's key, where its hook says key delivery.","schema":{"type":"string"}}},
"headers":{
"RequestId":{"description":"The request's id, in every log line and span it made; one sent is kept.","schema":{"type":"string"}},
"FennecSeq":{"description":"The change the answer was read at, or the write landed as.","schema":{"type":"integer"}},
"FennecHistory":{"description":"The history Fennec-Seq is of.","schema":{"type":"string"}},
"FennecNext":{"description":"The since of the next page.","schema":{"type":"integer"}},
"RetryAfter":{"description":"Seconds to wait before coming back.","schema":{"type":"integer"}},
"ETag":{"description":"The row's tag: a hash of the row as the token is shown it, the same on every server for the same row. On a write, where it was sent with a precondition: the row's tag as the write left it.","schema":{"type":"string"}}},
"schemas":{
"Error":{"type":"object","required":["error"],"properties":{"error":{"type":"string"},"retry_after":{"type":"integer","description":"Seconds to wait, as Retry-After says."},"retry_after_ms":{"type":"integer","description":"A quota's wait, to the millisecond."},"quota":{"type":"string","description":"The quota refused."},"reason":{"type":"string","description":"A require's else words."},"at":{"type":"integer","description":"The statement of a batch that stopped."},"completed":{"type":"integer"},"binding":{"type":"string"},"outcome":{"type":"string","enum":["unknown"],"description":"A write whose outcome the server cannot know."},"seq":{"type":"integer"},"history":{"type":"string"}}},
"Document":{"type":"object","description":"A document: its fields, id among them.","additionalProperties":true},
"Documents":{"oneOf":[{"$ref":"#/components/schemas/Document"},{"type":"array","items":{"$ref":"#/components/schemas/Document"}}]},
"Rows":{"type":"array","items":{"$ref":"#/components/schemas/Document"}},
"Faceted":{"type":"object","properties":{"rows":{"$ref":"#/components/schemas/Rows"},"count":{"type":"integer"},"facets":{"type":"object","additionalProperties":{"type":"array","items":{"type":"object","properties":{"value":{},"count":{"type":"integer"}}}}}}},
"Read":{"oneOf":[{"$ref":"#/components/schemas/Rows"},{"$ref":"#/components/schemas/Faceted"}]},
"Outcome":{"type":"object","description":"What a write did.","properties":{"inserted":{"type":"integer"},"updated":{"type":"integer"},"deleted":{"type":"integer"},"message":{"type":"string"}},"additionalProperties":true},
"Written":{"oneOf":[{"$ref":"#/components/schemas/Outcome"},{"$ref":"#/components/schemas/Rows"}]},
"Answer":{"description":"A statement's answer: its rows, a count, or what a write did.","oneOf":[{"$ref":"#/components/schemas/Rows"},{"$ref":"#/components/schemas/Faceted"},{"$ref":"#/components/schemas/Outcome"}]},
"Query":{"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"A FennecQL statement, or SQL's spelling of one."},"params":{"description":"$1, $2 by position, or $name by name.","oneOf":[{"type":"array"},{"type":"object"}]}}},
"Call":{"type":"object","properties":{"params":{"type":"object","description":"The statement's arguments by name."}}},
"Near":{"type":"object","required":["vector"],"properties":{
"vector":{"description":"What to rank by: a vector field's numbers, a point's [longitude, latitude], or a sparse field's text form {index:value,...}/dimension, as pgvector's sparsevec writes it.","oneOf":[{"type":"array","items":{"type":"number"},"minItems":1},{"type":"string"}]},
"field":{"type":"string","description":"The vector, sparse or point field, where the collection has several."},
"limit":{"type":"integer","minimum":0},
"offset":{"type":"integer","minimum":0},
"ef":{"type":"integer","minimum":0,"description":"The graph search's beam: how many candidates it keeps."},
"exact":{"type":"boolean","description":"true: every row compared, no index."},
"where":{"type":"string","description":"A FennecQL filter, ANDed with the query string's."},
"select":{"description":"The fields to answer: a list, or one field's name.","oneOf":[{"type":"array","items":{"type":"string"}},{"type":"string"}]},
"match":{"type":"string","description":"Words BM25 ranks too over a @text field: a hybrid search, the two rankings fused by reciprocal rank."},
"match_field":{"type":"string","description":"The @text field match ranks, where the collection has several."},
"match_fuzzy":{"description":"true, or the most edits a word of match may take.","oneOf":[{"type":"boolean"},{"type":"integer","minimum":0,"maximum":2}]},
"match_prefix":{"type":"boolean","description":"The last word of match a prefix."},
"fuse_k":{"type":"integer","minimum":0,"maximum":1000000,"description":"The fusion's rank offset."},
"candidates":{"type":"integer","minimum":1,"maximum":10000,"description":"How many candidates each side ranks."}}},
"Info":{"type":"object","properties":{"fennecdb":{"type":"string"},"collections":{"type":"array","items":{"type":"string"}}}},
"Schemas":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"fields":{"type":"array","items":{"type":"object"}}}}},
"Description":{"type":"object","description":"A schema description, as Schema in code sends it.","properties":{"format":{"type":"integer"},"fennecql":{"type":"string"},"migrations":{"type":"array"}}},
"Plan":{"type":"object","properties":{"applied":{"type":"boolean"},"ran":{"type":"array"},"migrations":{"type":"array"},"statements":{"type":"array"},"refusals":{"type":"array"}}},
"Since":{"type":"object","properties":{"since":{"type":"integer","minimum":0}}},
"Whoami":{"type":"object","properties":{"node":{"type":"string"},"tenant":{"type":["string","null"]},"read_only":{"type":"boolean"},"kind":{"type":"string"}}},
"Quota":{"type":"object","properties":{"bytes":{"type":["integer","null"]},"rows":{"type":["integer","null"]},"streams":{"type":["integer","null"]},"requests":{"type":["integer","null"]},"writes":{"type":["integer","null"]},"write_bytes":{"type":["integer","null"]}}},
"JsonRpc":{"type":"object","description":"A JSON-RPC 2.0 message (MCP).","properties":{"jsonrpc":{"const":"2.0"},"id":{},"method":{"type":"string"},"params":{},"result":{},"error":{}}}},
"responses":{
"Refused":{"description":"A refusal: its status says what to do about it.","headers":{"X-Request-Id":{"$ref":"#/components/headers/RequestId"}},"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Error"}}}},
"Unverified":{"description":"The event did not verify: its headers, scheme, time, size, hook or signature, one answer for each.","headers":{"X-Request-Id":{"$ref":"#/components/headers/RequestId"}},"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Error"}}}},
"Unauthorized":{"description":"No token, or a wrong one.","headers":{"WWW-Authenticate":{"schema":{"type":"string"}},"X-Request-Id":{"$ref":"#/components/headers/RequestId"}},"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Error"}}}},
"Unavailable":{"description":"Come back after Retry-After seconds. Fennec-Unserved (as the connection was taken) or, from fennec-shard, Fennec-Unsent: 1 says the request never ran.","headers":{"Retry-After":{"$ref":"#/components/headers/RetryAfter"},"Fennec-Unserved":{"schema":{"type":"string","enum":["ceiling","no_thread"]}},"Fennec-Unsent":{"schema":{"type":"string"}},"X-Request-Id":{"$ref":"#/components/headers/RequestId"}},"content":{"application/json":{"schema":{"allOf":[{"$ref":"#/components/schemas/Error"},{"required":["retry_after"]}]}}}},
"NotModified":{"description":"Unchanged since the If-None-Match tag: no body, the read not run.","headers":{"ETag":{"schema":{"type":"string"}}}},
"RowNotModified":{"description":"The row is the one the If-None-Match tag names: no body.","headers":{"ETag":{"$ref":"#/components/headers/ETag"}}},
"RowMade":{"description":"The row was made: there was none of that id.","headers":{"X-Request-Id":{"$ref":"#/components/headers/RequestId"},"ETag":{"$ref":"#/components/headers/ETag"}},"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Outcome"}}}},
"PreconditionFailed":{"description":"If-Match or If-None-Match did not hold for the row as the write lock found it: nothing written.","headers":{"X-Request-Id":{"$ref":"#/components/headers/RequestId"}},"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Error"}}}}}}"##;

#[cfg(test)]
mod tests {
    use super::*;
    use fennec_core::value::Value;

    fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
        match v {
            Value::Object(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn text(v: Option<&Value>) -> &str {
        match v {
            Some(Value::Text(s)) => s,
            _ => "",
        }
    }

    fn members(v: Option<&Value>) -> &[(String, Value)] {
        match v {
            Some(Value::Object(m)) => m,
            _ => &[],
        }
    }

    /// The value a local `$ref` names, `#/a/b` with `~1` and `~0`.
    fn resolve<'a>(doc: &'a Value, pointer: &str) -> Option<&'a Value> {
        let rest = pointer.strip_prefix("#/")?;
        rest.split('/').try_fold(doc, |v, part| {
            get(v, &part.replace("~1", "/").replace("~0", "~"))
        })
    }

    /// Every `$ref` under `v`.
    fn refs<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
        match v {
            Value::Object(m) => {
                for (k, v) in m {
                    match (k.as_str(), v) {
                        ("$ref", Value::Text(r)) => out.push(r),
                        _ => refs(v, out),
                    }
                }
            }
            Value::List(items) => items.iter().for_each(|v| refs(v, out)),
            _ => {}
        }
    }

    const METHODS: &[&str] = &["get", "put", "post", "delete", "patch", "head", "options"];

    /// Each document reads as JSON and holds to OpenAPI 3.1's shape: every
    /// operation has an id of its own and responses, every placeholder of
    /// a path is a path parameter of that name, every `$ref` resolves.
    #[test]
    fn every_document_is_openapi_3_1() {
        for kind in [Kind::File, Kind::Node, Kind::Router] {
            let src = std::str::from_utf8(document(kind)).unwrap();
            let doc = fennec_core::json::parse_json(src)
                .unwrap_or_else(|e| panic!("{kind:?}'s document does not read: {e}"));
            assert_eq!(text(get(&doc, "openapi")), "3.1.0");
            let info = get(&doc, "info").expect("info");
            assert!(!text(get(info, "title")).is_empty());
            // The document's version, never the binary's: it asks no token.
            assert_eq!(text(get(info, "version")), DOCUMENT_VERSION);
            let paths = members(get(&doc, "paths"));
            assert!(!paths.is_empty());
            let mut ids = Vec::new();
            for (path, item) in paths {
                assert!(path.starts_with('/'), "{path}");
                let declared: Vec<&str> = match get(item, "parameters") {
                    Some(Value::List(ps)) => ps
                        .iter()
                        .map(|p| {
                            let p = match get(p, "$ref") {
                                Some(Value::Text(r)) => resolve(&doc, r).expect(r),
                                _ => p,
                            };
                            assert_eq!(text(get(p, "in")), "path", "{path}");
                            assert_eq!(get(p, "required"), Some(&Value::Bool(true)), "{path}");
                            text(get(p, "name"))
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                assert_eq!(declared, placeholders(path), "{path}");
                for (method, op) in members(Some(item)) {
                    if method == "parameters" {
                        continue;
                    }
                    assert!(METHODS.contains(&method.as_str()), "{path}: {method}");
                    let id = text(get(op, "operationId"));
                    assert!(!id.is_empty(), "{path} {method}");
                    assert!(!ids.contains(&id), "{kind:?}: operationId {id} twice");
                    ids.push(id);
                    let responses = members(get(op, "responses"));
                    assert!(!responses.is_empty(), "{path} {method}");
                    for (status, _) in responses {
                        let ok = status == "default"
                            || status.len() == 3 && status.bytes().all(|b| b.is_ascii_digit());
                        assert!(ok, "{path} {method}: {status}");
                    }
                }
            }
            let mut all = Vec::new();
            refs(&doc, &mut all);
            assert!(all.len() > 100, "{}", all.len());
            for r in all {
                assert!(
                    resolve(&doc, r).is_some(),
                    "{kind:?}: {r} resolves to nothing"
                );
            }
        }
    }

    /// The same method and path twice in one document would be one key
    /// twice, which a JSON reader keeps one of.
    #[test]
    fn no_route_is_listed_twice() {
        for kind in [Kind::File, Kind::Node, Kind::Router] {
            let ops = operations(kind);
            for (i, l) in ops.iter().enumerate() {
                let twice = ops[i + 1..]
                    .iter()
                    .any(|o| o.path == l.path && o.op.method == l.op.method);
                assert!(!twice, "{kind:?}: {} {} twice", l.op.method, l.path);
                assert!(
                    !l.op.head || l.op.method == "get",
                    "{}: HEAD beside a GET alone",
                    l.id
                );
            }
        }
    }

    /// Routes the server's code names that the document leaves out, each
    /// with its reason.
    const UNDOCUMENTED: &[(&str, &str)] = &[
        (
            "_timing",
            "the `timing` feature's, a measurement build's alone, never a release's",
        ),
        (
            "_shard",
            "the router's admin routes, in the sharding reference",
        ),
    ];

    /// Where the routes a function matches are served, so a pattern is
    /// looked for in the rows of every place its code serves it -- a file's
    /// root, a node's, a router's, below a tenant -- never in another's.
    /// The most specific region holding a pattern is its: a file, or a
    /// directory of them (`/` last), then a function, then a call in it
    /// from which on the function serves elsewhere. `below` is the
    /// segments the function's slices start after, as `admin::handle`
    /// matches `&segs[1..]` below `_admin`.
    struct Region {
        file: &'static str,
        func: &'static str,
        after: &'static str,
        at: u8,
        below: &'static [&'static str],
    }

    const REGIONS: &[Region] = &[
        // A database's routes: a file's server's, and each tenant's once
        // `route_tenant` cut its prefix.
        Region {
            file: "fennec-http/src/",
            func: "",
            after: "",
            at: FILE | TENANT,
            below: &[],
        },
        // Before a tenant is routed: a file's root and a node's.
        Region {
            file: "fennec-http/src/lib.rs",
            func: "serve_connection",
            after: "",
            at: FILE | NODE,
            below: &[],
        },
        Region {
            file: "fennec-http/src/lib.rs",
            func: "serve_connection",
            after: "route_tenant(",
            at: FILE | TENANT,
            below: &[],
        },
        Region {
            file: "fennec-http/src/lib.rs",
            func: "route_tenant",
            after: "",
            at: NODE,
            below: &[],
        },
        Region {
            file: "fennec-http/src/admin.rs",
            func: "",
            after: "",
            at: NODE,
            below: &["_admin"],
        },
        Region {
            file: "fennec-http/src/node_stream.rs",
            func: "",
            after: "",
            at: NODE,
            below: &[],
        },
        // The audit's names for a node's admin routes and a router's.
        Region {
            file: "fennec-http/src/audit.rs",
            func: "",
            after: "",
            at: NODE,
            below: &[],
        },
        // A replica's stream: a file's, a tenant's and a router's
        // directory's.
        Region {
            file: "fennec-http/src/replication.rs",
            func: "",
            after: "",
            at: FILE | TENANT | ROUTER,
            below: &[],
        },
        Region {
            file: "fennec-http/src/studio.rs",
            func: "",
            after: "",
            at: FILE | NODE | ROUTER,
            below: &[],
        },
        // The router: its own routes at its root, a tenant's forwarded.
        Region {
            file: "fennec-shard/src/",
            func: "",
            after: "",
            at: ROUTER,
            below: &[],
        },
        Region {
            file: "fennec-shard/src/lib.rs",
            func: "admin",
            after: "",
            at: ROUTER,
            below: &["_shard"],
        },
        Region {
            file: "fennec-shard/src/region.rs",
            func: "",
            after: "",
            at: ROUTER,
            below: &["_shard"],
        },
        Region {
            file: "fennec-shard/src/readers.rs",
            func: "reads_only",
            after: "",
            at: ROUTER,
            below: &["_shard"],
        },
    ];

    /// The region of a pattern in `file`, in function `func`, whose code
    /// has passed the calls `passed`.
    fn region(file: &str, func: &str, passed: &[&str]) -> &'static Region {
        REGIONS
            .iter()
            .filter(|r| match r.file.ends_with('/') {
                true => file.starts_with(r.file),
                false => file == r.file,
            })
            .filter(|r| r.func.is_empty() || r.func == func)
            .filter(|r| r.after.is_empty() || passed.contains(&r.after))
            .max_by_key(|r| {
                (
                    !r.file.ends_with('/'),
                    !r.func.is_empty(),
                    !r.after.is_empty(),
                )
            })
            .unwrap_or(&REGIONS[0])
    }

    /// A route pattern the dispatcher matches, read from its source.
    #[derive(Debug)]
    struct Pattern {
        file: String,
        func: String,
        /// Where its code serves it: `FILE`, `NODE`, `ROUTER`, `TENANT`.
        at: u8,
        method: Option<String>,
        /// Each segment, a literal or `None` for any.
        segments: Vec<Option<String>>,
        /// Whether more segments may follow.
        open: bool,
        /// Whether `segments` are the path's last, any before them.
        tail: bool,
    }

    /// Every route pattern in fennec-http's and fennec-shard's code.
    fn patterns() -> Vec<Pattern> {
        let mut out = Vec::new();
        for krate in ["fennec-http", "fennec-shard"] {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join(krate)
                .join("src");
            let mut dirs = vec![root.clone()];
            while let Some(d) = dirs.pop() {
                for e in std::fs::read_dir(&d).unwrap() {
                    let p = e.unwrap().path();
                    if p.is_dir() {
                        dirs.push(p);
                    } else if p.extension().is_some_and(|x| x == "rs") {
                        let rel = p.strip_prefix(&root).unwrap().to_string_lossy();
                        let name = format!("{krate}/src/{}", rel.replace('\\', "/"));
                        let src = std::fs::read_to_string(&p).unwrap();
                        scan(&name, &code_of(&src), &mut out);
                    }
                }
            }
        }
        out
    }

    /// The code of a source file as the scan reads it: its tests and its
    /// comment lines cut, its whitespace one space.
    fn code_of(src: &str) -> String {
        let src = src.split("#[cfg(test)]\nmod ").next().unwrap();
        let code: Vec<&str> = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect();
        code.join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Which bytes of `code` are inside a string or a char literal, so
    /// that a bracket or an anchor in one is not code.
    fn literals(code: &str) -> Vec<bool> {
        let b = code.as_bytes();
        let mut lit = vec![false; b.len()];
        let ident = |i: usize| b[i].is_ascii_alphanumeric() || b[i] == b'_';
        let mut i = 0;
        while i < b.len() {
            let start = i;
            let end = match b[i] {
                // `r"..."`, `r#"..."#`, `br#"..."#`: no escapes.
                b'r' if (i == 0
                    || !ident(i - 1)
                    || (b[i - 1] == b'b' && (i < 2 || !ident(i - 2))))
                    && b[i + 1..]
                        .iter()
                        .position(|&c| c != b'#')
                        .map(|n| b[i + 1 + n])
                        == Some(b'"') =>
                {
                    let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
                    let close: Vec<u8> = std::iter::once(b'"')
                        .chain(std::iter::repeat_n(b'#', hashes))
                        .collect();
                    let from = i + 2 + hashes;
                    b[from..]
                        .windows(close.len())
                        .position(|w| w == close.as_slice())
                        .map_or(b.len(), |n| from + n + close.len())
                }
                b'"' => {
                    let mut j = i + 1;
                    while j < b.len() && b[j] != b'"' {
                        j += if b[j] == b'\\' { 2 } else { 1 };
                    }
                    j + 1
                }
                // A char literal, not a lifetime: `'x'`, `'\''`, `'\u{..}'`.
                b'\'' if b.get(i + 1) == Some(&b'\\') => {
                    let j = (i + 3).min(b.len());
                    j + b[j..].iter().position(|&c| c == b'\'').unwrap_or(0) + 1
                }
                b'\'' => {
                    let ch = code[i + 1..].chars().next().map_or(1, char::len_utf8);
                    match b.get(i + 1 + ch) {
                        Some(b'\'') => i + 2 + ch,
                        _ => i + 1,
                    }
                }
                _ => i + 1,
            };
            let end = end.min(b.len());
            if end > start + 1 {
                lit[start..end].iter_mut().for_each(|l| *l = true);
            }
            i = end;
        }
        lit
    }

    /// The index of the bracket closing the one at `open`.
    fn closing(code: &str, lit: &[bool], open: usize) -> usize {
        let mut depth = 0;
        for (i, c) in code.bytes().enumerate().skip(open) {
            if lit[i] {
                continue;
            }
            match c {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return i;
                    }
                }
                _ => {}
            }
        }
        code.len()
    }

    /// The first of `stops` in `code[from..to]` at no depth below `from`,
    /// or where a bracket opened before it closes.
    fn until(code: &str, lit: &[bool], from: usize, to: usize, stops: &[&str]) -> usize {
        let mut depth = 0i32;
        let mut i = from;
        while i < to {
            if !lit[i] {
                let rest = &code[i..to];
                if depth == 0 && stops.iter().any(|s| rest.starts_with(s)) {
                    return i;
                }
                match code.as_bytes()[i] {
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => {
                        depth -= 1;
                        if depth < 0 {
                            return i;
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
        to
    }

    /// Whether an expression is a request's segments, or a slice of them.
    fn routeish(expr: &str, vars: &[String]) -> bool {
        expr.contains("segments()")
            || vars.iter().any(|v| {
                expr.match_indices(v.as_str()).any(|(i, _)| {
                    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                    !word(expr[..i].chars().next_back())
                        && !word(expr[i + v.len()..].chars().next())
                })
            })
    }

    /// Where a segment test reads: `.first()`, `.get(2)`, `.last()`.
    enum Index {
        At(usize),
        Last,
    }

    fn index_of(expr: &str) -> Index {
        let mut best = (0, Index::At(0));
        for (pat, ix) in [
            (".first()", Some(0)),
            (".peek()", Some(0)),
            (".last()", None),
        ] {
            if let Some(i) = expr.rfind(pat) {
                if i >= best.0 {
                    best = (i, ix.map_or(Index::Last, Index::At));
                }
            }
        }
        if let Some(i) = expr.rfind(".get(") {
            let n: String = expr[i + 5..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let (true, Ok(n)) = (i >= best.0, n.parse()) {
                best = (i, Index::At(n));
            }
        }
        best.1
    }

    /// A slice pattern's segments, `[` at `open`: each a literal, its
    /// alternatives (`"plan" | "apply"`) or any; `..` more of them.
    fn slice_at(code: &str, lit: &[bool], open: usize) -> (Vec<Vec<String>>, bool, bool) {
        let close = closing(code, lit, open);
        let (mut segments, mut open_end, mut tail) = (Vec::new(), false, false);
        let mut at = open + 1;
        while at < close {
            let end = until(code, lit, at, close, &[","]);
            let part = code[at..end].trim();
            at = end + 1;
            if part.is_empty() {
                continue;
            }
            if part == ".." || part.ends_with(" @ ..") {
                match segments.is_empty() {
                    true => tail = true,
                    false => open_end = true,
                }
                continue;
            }
            let words: Option<Vec<String>> = part
                .split('|')
                .map(|w| {
                    let w = w.trim().trim_start_matches('&');
                    w.strip_prefix('"')
                        .and_then(|w| w.strip_suffix('"'))
                        .map(str::to_string)
                })
                .collect();
            segments.push(words.unwrap_or_default());
        }
        (segments, open_end, tail)
    }

    /// Every slice pattern in `code[from..to]` outside another slice: a
    /// pattern's alternatives, a tuple's slice.
    fn slices_in(code: &str, lit: &[bool], from: usize, to: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut i = from;
        while i < to {
            if !lit[i] && code.as_bytes()[i] == b'[' {
                out.push(i);
                i = closing(code, lit, i);
            }
            i += 1;
        }
        out
    }

    /// Each `Some(&"word")` in `code[from..to]`: a segment test's word.
    fn words_in(code: &str, lit: &[bool], from: usize, to: usize) -> Vec<String> {
        code[from..to]
            .match_indices("Some(&\"")
            .filter(|(i, _)| !lit[from + i])
            .filter_map(|(i, m)| {
                let w = &code[from + i + m.len()..to];
                w.find('"').map(|e| w[..e].to_string())
            })
            .collect()
    }

    /// Every route pattern in `code`, the code of `file`, in each form the
    /// dispatchers match a request by: a slice pattern against its
    /// segments -- a `(Method::X | .., [..])` arm, the arms of a `match`
    /// on them, a `matches!`, a `let` or `if let`, an `== [..]` --
    /// alternatives and all; a segment by its index (`.first()`,
    /// `.get(2)`, `.last()` against `Some(&"x")`, compared or matched);
    /// and a path (`path == "/x"`, `starts_with`, `strip_prefix`,
    /// `path.contains`). A route written in another form is one this
    /// must learn.
    fn scan(file: &str, code: &str, out: &mut Vec<Pattern>) {
        let lit = literals(code);
        let b = code.as_bytes();
        let ident = |i: usize| b[i].is_ascii_alphanumeric() || b[i] == b'_';
        let mut vars: Vec<String> = Vec::new();
        let mut func = String::new();
        let mut passed: Vec<&str> = Vec::new();
        let mut i = 0;
        while i < code.len() {
            if lit[i] || (i > 0 && ident(i - 1) && ident(i)) {
                i += 1;
                continue;
            }
            let rest = &code[i..];
            if let Some(after) = rest.strip_prefix("fn ") {
                let name: String = after
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    func = name;
                    passed.clear();
                }
            }
            for call in REGIONS.iter().map(|r| r.after).filter(|a| !a.is_empty()) {
                if rest.starts_with(call) && !passed.contains(&call) {
                    passed.push(call);
                }
            }
            let r = region(file, &func, &passed);
            let mut push = |method: Option<&str>,
                            segs: Vec<Vec<String>>,
                            open: bool,
                            tail: bool,
                            slice: bool| {
                let below = r.below.iter().map(|w| vec![w.to_string()]);
                let segs: Vec<Vec<String>> = match slice {
                    true => below.chain(segs).collect(),
                    false => segs,
                };
                if segs.iter().all(Vec::is_empty) && !(slice && method.is_some()) {
                    return;
                }
                // One pattern for each choice of the alternatives: each
                // must be served, not one of them.
                let mut choices: Vec<Vec<Option<String>>> = vec![Vec::new()];
                for seg in &segs {
                    let opts: Vec<Option<String>> = match seg.is_empty() {
                        true => vec![None],
                        false => seg.iter().cloned().map(Some).collect(),
                    };
                    choices = choices
                        .iter()
                        .flat_map(|c| {
                            opts.iter()
                                .map(move |o| [c.clone(), vec![o.clone()]].concat())
                        })
                        .collect();
                }
                for segments in choices {
                    out.push(Pattern {
                        file: file.into(),
                        func: func.clone(),
                        at: r.at,
                        method: method.map(str::to_ascii_lowercase),
                        segments,
                        open,
                        tail,
                    });
                }
            };
            // A segment's word at its index, the leading ones any but
            // what the region says it is below.
            let indexed = |index: Index, word: String| -> (Vec<Vec<String>>, bool) {
                match index {
                    Index::Last => (vec![vec![word]], true),
                    Index::At(n) => {
                        let mut segs: Vec<Vec<String>> = (0..n)
                            .map(|k| {
                                r.below
                                    .get(k)
                                    .map(|w| vec![w.to_string()])
                                    .unwrap_or_default()
                            })
                            .collect();
                        segs.push(vec![word]);
                        (segs, false)
                    }
                }
            };
            // `(Method::A | Method::B, [..])`, and `req.method ==
            // Method::A && req.segments() == [..]`.
            if let Some(after) = rest.strip_prefix("Method::") {
                let mut methods = Vec::new();
                let mut s = after;
                loop {
                    let end = s
                        .find(|c: char| !c.is_ascii_alphanumeric())
                        .unwrap_or(s.len());
                    methods.push(&s[..end]);
                    s = &s[end..];
                    match s.strip_prefix(" | Method::") {
                        Some(more) => s = more,
                        None => break,
                    }
                }
                let at = code.len() - s.len();
                let slice = match s.strip_prefix(", [") {
                    Some(_) => Some(at + 2),
                    None => s
                        .strip_prefix(" && ")
                        .and_then(|t| {
                            t.find(" == [")
                                .filter(|&e| routeish(&t[..e], &vars) && !t[..e].contains("&&"))
                        })
                        .map(|e| at + 4 + e + 4),
                };
                if let Some(mut open) = slice {
                    loop {
                        let (segs, more, tail) = slice_at(code, &lit, open);
                        for m in &methods {
                            push(Some(*m), segs.clone(), more, tail, true);
                        }
                        let close = closing(code, &lit, open);
                        match code[close + 1..].starts_with(" | [") {
                            true => open = close + 4,
                            false => break,
                        }
                    }
                }
                // Past every method read: `Method::Head` after `Method::Get
                // |` is this arm's, not one of its own.
                i = at;
                continue;
            }
            // `match <segments> { arms }`.
            if rest.starts_with("match ") {
                let from = i + 6;
                let brace = until(code, &lit, from, code.len(), &["{"]);
                let scrutinee = &code[from..brace.min(code.len())];
                if brace < code.len() && routeish(scrutinee, &vars) {
                    let end = closing(code, &lit, brace);
                    let mut arm = brace + 1;
                    while arm < end {
                        let arrow = until(code, &lit, arm, end, &["=>"]);
                        if arrow >= end {
                            break;
                        }
                        for open in slices_in(code, &lit, arm, arrow) {
                            let (segs, more, tail) = slice_at(code, &lit, open);
                            push(None, segs, more, tail, true);
                        }
                        for w in words_in(code, &lit, arm, arrow) {
                            let (segs, tail) = indexed(index_of(scrutinee), w);
                            push(None, segs, true, tail, false);
                        }
                        // The arm's body: a block, or an expression to its
                        // comma.
                        let body = arrow + 2 + usize::from(code[arrow + 2..].starts_with(' '));
                        arm = match code.as_bytes().get(body) {
                            Some(b'{') => closing(code, &lit, body) + 1,
                            _ => until(code, &lit, body, end, &[","]),
                        };
                        arm += usize::from(code[arm..].starts_with(" ,"));
                        arm += usize::from(code[arm..].starts_with(','));
                    }
                }
            }
            // `matches!(<segments>, [..] | [..])`.
            if rest.starts_with("matches!(") {
                let open = i + 8;
                let close = closing(code, &lit, open);
                let comma = until(code, &lit, open + 1, close, &[","]);
                let expr = &code[open + 1..comma.min(close)];
                if comma < close && routeish(expr, &vars) {
                    for o in slices_in(code, &lit, comma, close) {
                        let (segs, more, tail) = slice_at(code, &lit, o);
                        push(None, segs, more, tail, true);
                    }
                    for w in words_in(code, &lit, comma, close) {
                        let (segs, tail) = indexed(index_of(expr), w);
                        push(None, segs, true, tail, false);
                    }
                }
            }
            // `let [..] = <segments> else`, `if let (_, [..]) = (..)`; and
            // a name given the segments, read as them from then on.
            if rest.starts_with("let ") {
                let from = i + 4;
                let eq = until(code, &lit, from, code.len(), &[" = ", ";"]);
                if code[eq..].starts_with(" = ") {
                    let pattern = &code[from..eq];
                    let to = until(code, &lit, eq + 3, code.len(), &[" else", " {", ";"]);
                    let expr = code[eq + 3..to].trim();
                    if routeish(expr, &vars) {
                        for o in slices_in(code, &lit, from, eq) {
                            let (segs, more, tail) = slice_at(code, &lit, o);
                            push(None, segs, more, tail, true);
                        }
                        for w in words_in(code, &lit, from, eq) {
                            let (segs, tail) = indexed(index_of(expr), w);
                            push(None, segs, true, tail, false);
                        }
                        let named = pattern.chars().all(|c| c.is_alphanumeric() || c == '_');
                        // `let segs = req.segments();`, `let rest =
                        // &segs[1..];` -- never `let name = match segs ..`.
                        let whole = !expr.contains(' ')
                            && (expr.ends_with("segments()")
                                || expr.ends_with(".as_slice()")
                                || (expr.starts_with('&') && expr.ends_with("..]")));
                        if named && whole && !vars.iter().any(|v| v == pattern) {
                            vars.push(pattern.to_string());
                        }
                    }
                }
            }
            // `<segments> == [..]`, `<segment> == Some(&"x")`.
            for (cmp, slice) in [
                (" == [", true),
                (" != [", true),
                (" == Some(&\"", false),
                (" != Some(&\"", false),
            ] {
                if !rest.starts_with(cmp) {
                    continue;
                }
                let back = &code[i.saturating_sub(80)..i];
                let left = ["&& ", "|| ", "if ", "{ ", "; ", "return ", "= ", "=> "]
                    .iter()
                    .filter_map(|d| back.rfind(d).map(|p| p + d.len()))
                    .max()
                    .map_or(back, |p| &back[p..]);
                if !routeish(left, &vars) {
                    continue;
                }
                match slice {
                    true => {
                        let (segs, more, tail) = slice_at(code, &lit, i + cmp.len() - 1);
                        push(None, segs, more, tail, true);
                    }
                    false => {
                        let w = &code[i + cmp.len()..];
                        let word = w[..w.find('"').unwrap_or(0)].to_string();
                        let (segs, tail) = indexed(index_of(left), word);
                        push(None, segs, true, tail, false);
                    }
                }
            }
            // A path, or its prefix.
            for anchor in [
                "path == \"/",
                "starts_with(\"/",
                "strip_prefix(\"/",
                "path.contains(\"",
            ] {
                if let Some(word) = rest.strip_prefix(anchor) {
                    let word = &word[..word.find('"').unwrap_or(0)];
                    let segs: Vec<Vec<String>> = word
                        .split('/')
                        .filter(|s| !s.is_empty())
                        .map(|s| vec![s.to_string()])
                        .collect();
                    push(None, segs, true, false, false);
                }
            }
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }

    /// Whether `want` reads `have`, a row's path: a literal its literal, a
    /// name (any) its placeholder -- never a word for a placeholder, so a
    /// new `/word` is no collection's path.
    fn fits(want: &[Option<String>], open: bool, tail: bool, have: &[&str]) -> bool {
        let one = |w: &Option<String>, h: &&str| match (w, h.starts_with('{')) {
            (None, true) => true,
            (Some(w), false) => w == h,
            _ => false,
        };
        match (tail, open) {
            (true, _) => {
                have.len() >= want.len()
                    && want
                        .iter()
                        .zip(&have[have.len() - want.len()..])
                        .all(|(w, h)| one(w, h))
            }
            (false, true) => {
                have.len() >= want.len() && want.iter().zip(have).all(|(w, h)| one(w, h))
            }
            (false, false) => {
                have.len() == want.len() && want.iter().zip(have).all(|(w, h)| one(w, h))
            }
        }
    }

    /// Whether every place `p`'s code serves it has its row.
    fn documented(p: &Pattern) -> bool {
        if let Some(Some(first)) = p.segments.first() {
            if UNDOCUMENTED.iter().any(|(u, _)| u == first) {
                return true;
            }
        }
        // A preflight is every path's; a HEAD is a GET's listed with it.
        let (method, head) = match p.method.as_deref() {
            Some("options") => return true,
            Some("head") => (Some("get"), true),
            m => (m, false),
        };
        // A path below `/t/<tenant>` is a tenant's, wherever its code is.
        let (at, segs, open) = match p.segments.split_first() {
            Some((Some(t), rest)) if t == "t" && !p.tail => (
                TENANT,
                rest.get(1..).unwrap_or(&[]),
                p.open || rest.is_empty(),
            ),
            _ => (p.at, &p.segments[..], p.open),
        };
        [FILE, NODE, ROUTER, TENANT]
            .into_iter()
            .filter(|bit| at & bit != 0)
            .all(|bit| {
                // A node's or a router's path whose first segment is any
                // is a tenant's: `.get(2)` of `/t/<tenant>/_hooks`.
                let (rows, segs) = match bit {
                    NODE | ROUTER if segs.len() >= 2 && segs[0].is_none() && !p.tail => {
                        (TENANT, &segs[2..])
                    }
                    _ => (bit, segs),
                };
                OPS.iter().any(|op| {
                    let have: Vec<&str> = op.path.split('/').filter(|s| !s.is_empty()).collect();
                    op.at & rows != 0
                        && method.is_none_or(|m| m == op.method)
                        && (!head || op.head)
                        && fits(segs, open, p.tail, &have)
                })
            })
    }

    /// Every route the dispatchers match is in the document: a route added
    /// without its row in [`OPS`] fails here.
    #[test]
    fn every_route_the_server_matches_is_documented() {
        let all = patterns();
        // The scan reads what it should: the forms above, in the files that
        // route.
        assert!(all.len() > 150, "{}", all.len());
        for at in [
            "fennec-http/src/lib.rs",
            "fennec-http/src/admin.rs",
            "fennec-http/src/cdc.rs",
            "fennec-http/src/api.rs",
            "fennec-http/src/hooks.rs",
            "fennec-http/src/replication.rs",
            "fennec-http/src/node_stream.rs",
            "fennec-http/src/studio.rs",
            "fennec-shard/src/lib.rs",
            "fennec-shard/src/readers.rs",
        ] {
            assert!(all.iter().any(|p| p.file == at), "nothing read in {at}");
        }
        let missing: Vec<String> = all
            .iter()
            .filter(|p| !documented(p))
            .map(|p| {
                format!(
                    "{} {} (served at {:#b}): {}",
                    p.file,
                    p.func,
                    p.at,
                    route(p)
                )
            })
            .collect();
        assert!(
            missing.is_empty(),
            "routes the server matches that the OpenAPI document (openapi.rs's OPS) \
             leaves out where their code serves them:\n{}",
            missing.join("\n")
        );
    }

    /// A pattern as a route: its method (`any`), its segments' words (`*`
    /// any).
    fn route(p: &Pattern) -> String {
        let segs: Vec<&str> = p
            .segments
            .iter()
            .map(|s| s.as_deref().unwrap_or("*"))
            .collect();
        format!(
            "{} /{}",
            p.method.as_deref().unwrap_or("any"),
            segs.join("/")
        )
    }

    /// The routes `code`, as `file`'s, that the table leaves out.
    fn left_out(file: &str, code: &str) -> Vec<String> {
        let mut out = Vec::new();
        scan(file, code, &mut out);
        assert!(!out.is_empty(), "nothing read in {code}");
        out.iter().filter(|p| !documented(p)).map(route).collect()
    }

    /// The scan reads each form a dispatcher matches a route by, and
    /// reports the route no row lists: one snippet a form, beside a
    /// route of the same form that the table holds.
    #[test]
    fn the_scan_finds_a_route_left_out_in_every_form() {
        const HTTP: &str = "fennec-http/src/x.rs";
        let cases: &[(&str, &str, &[&str])] = &[
            // A method's arm, its slice's alternatives each a route.
            (
                HTTP,
                r#"match (req.method, segs.as_slice()) { (Method::Get | Method::Head, ["_nosuch", name]) => 1, (Method::Post, ["_schema", "plan" | "_nosuch"]) => 2, (Method::Post, ["query"]) => 3 }"#,
                &["get /_nosuch/*", "head /_nosuch/*", "post /_schema/_nosuch"],
            ),
            // A HEAD the table does not list beside its GET.
            (
                HTTP,
                r#"match (req.method, seg) { (Method::Get | Method::Head, ["_changes"]) => 1 }"#,
                &["head /_changes"],
            ),
            // `== [..]`, the method before it read with it.
            (
                HTTP,
                r#"if req.method == Method::Post && req.segments() == ["_nosuch"] {} if req.segments() == ["batch"] {}"#,
                &["post /_nosuch", "any /_nosuch"],
            ),
            // `matches!` with alternatives past the first `]`.
            (
                HTTP,
                r#"let s = req.segments(); if matches!(s.as_slice(), ["query"] | ["_call", _] | ["_nosuch"]) {}"#,
                &["any /_nosuch"],
            ),
            // The arms of a `match` on the segments.
            (
                HTTP,
                r#"match req.segments().as_slice() { ["_call", name] => Some(name), ["_nosuch", ..] => None, _ => { let x = [1]; None } }"#,
                &["any /_nosuch"],
            ),
            // `let .. else` and `if let`.
            (
                HTTP,
                r#"let seg = req.segments(); let [name, "nosuch"] = seg.as_slice() else { return }; let [name, "changes"] = seg.as_slice() else { return }; if let (Some(t), ["_nosuch"]) = (&tenant, req.segments().as_slice()) {}"#,
                &["any /*/nosuch", "any /_nosuch"],
            ),
            // A path's prefix, cut or looked for.
            (
                HTTP,
                r#"if let Some(h) = req.path.strip_prefix("/_nosuch/") {} if req.path.contains("_nosuch2") {} if req.path.starts_with("/_nosuch3") || req.path == "/_mcp" {}"#,
                &["any /_nosuch", "any /_nosuch2", "any /_nosuch3"],
            ),
            // A segment by its index: a word is no collection's path.
            (
                HTTP,
                r#"if req.segments().first() == Some(&"nosuch") {} if req.segments().last() != Some(&"nosuch2") {} match req.segments().first() { Some(&"_nosuch") => 1, Some(&"_schema") => 2, _ => 0 }"#,
                &["any /nosuch", "any /nosuch2", "any /_nosuch"],
            ),
            // A node's root is not a file's: `/query` is a file's alone.
            (
                "fennec-http/src/lib.rs",
                r#"fn route_tenant(req: &Request) { let segs = req.segments(); if segs.as_slice() == ["query"] {} if segs.as_slice() == ["_whoami"] {} if segs.get(2) == Some(&"_nosuch") {} }"#,
                &["any /query", "any /*/*/_nosuch"],
            ),
            // A router's root, and a tenant's through it.
            (
                "fennec-shard/src/x.rs",
                r#"match req.segments().first() { Some(&"_health") => 1, Some(&"_metrics") => 2, Some(&"_shard") => 3, _ => 0 } match req.segments().get(2) { Some(&"_nosuch") => 1, _ => 0 }"#,
                &["any /_health", "any /*/*/_nosuch"],
            ),
        ];
        for (file, code, want) in cases {
            let got = left_out(file, code);
            assert_eq!(&got, want, "{code}");
        }
    }
}

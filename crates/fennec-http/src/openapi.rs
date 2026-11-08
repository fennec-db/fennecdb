//! The OpenAPI 3.1 document of the server's own REST API, at
//! `GET /_openapi.json`: part of the API, not an extra -- what Swagger UI,
//! Redoc or an SDK generator reads, and what a client checks itself
//! against.
//!
//! One table, [`OPS`], is the document: every route the server answers, a
//! row each, written out as JSON once a process, the first time it is
//! asked ([`document`]). The dispatcher does not read it -- a table match
//! on every request was a cost the hot path does not pay -- so the two are
//! held to each other by tests: `tests` below reads every route pattern the
//! dispatcher's code matches (a `(Method::X, [..])` arm, a `segments() ==
//! [..]`, a first segment or a path prefix of the server's own) and finds
//! each in the table, and `fennec-server`'s `tests/openapi.rs` sends a
//! request to every operation of the document a file's server and a
//! tenant node serve, answered neither 404 nor 405. A route added without
//! its row fails the first; a row without its route, the second.
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
    /// to its node -- what a tenant's client calls. The router's own
    /// `/_shard/` routes are its admin's, in the sharding reference.
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
    /// The flag a server serves it under, `""` for none.
    flag: &'static str,
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
        summary: "The server's version and its collections' names",
        params: READ,
        answer: Answer::Json(200, r##"{"$ref":"#/components/schemas/Info"}"##, "Version and names"),
        ..DEFAULT
    },
    Op {
        path: "/collections",
        id: "listCollections",
        summary: "Every collection's schema",
        params: READ,
        answer: Answer::Json(200, r##"{"$ref":"#/components/schemas/Schemas"}"##, "The schemas"),
        ..DEFAULT
    },
    Op {
        path: "/{collection}",
        id: "readRows",
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
    Op {
        method: "post",
        path: "/{collection}/near",
        id: "nearRows",
        summary: "The rows nearest a vector, nearest first",
        params: READ,
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
        summary: "Another service's event, verified by its signature in place of a token; \
                  every refusal before it verifies is one 401",
        flag: "--hooks",
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
        auth: Auth::Full,
        answer: Answer::Json(200, OBJECT, "Forgotten"),
        ..DEFAULT
    },
    // Replication.
    Op {
        path: "/_replication",
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
        tag: "Server",
        summary: "Who the server takes the token for: its roles, its rules, the statements it may call",
        answer: Answer::Json(200, r##"{"$ref":"#/components/schemas/Whoami"}"##, "The token's reading"),
        ..DEFAULT
    },
    Op {
        path: "/_stats/statements",
        id: "statementStats",
        tag: "Server",
        summary: "Each statement's count and time, by its shape",
        auth: Auth::Full,
        answer: Answer::Json(200, OBJECT, "The counts"),
        ..DEFAULT
    },
    Op {
        path: "/_stats/statements",
        at: NODE,
        id: "nodeStatementStats",
        tag: "Server",
        summary: "Every tenant's statements, by shape, each named",
        auth: Auth::Admin,
        answer: Answer::Json(200, OBJECT, "The counts"),
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
        tag: "Server",
        summary: "Counters and gauges, Prometheus's text format",
        auth: Auth::Scrape,
        answer: Answer::Metrics,
        ..DEFAULT
    },
    Op {
        path: "/_health",
        at: FILE | NODE,
        id: "health",
        tag: "Server",
        summary: "Whether the server is up: no token, no lock",
        auth: Auth::Open,
        answer: Answer::Json(200, r#"{"type":"object","properties":{"ok":{"type":"boolean"}}}"#, "Up"),
        ..DEFAULT
    },
    Op {
        path: "/_studio/",
        at: FILE | NODE,
        id: "studio",
        tag: "Server",
        summary: "fennec studio's page: the token is the page's to send",
        flag: "--studio",
        auth: Auth::Open,
        answer: Answer::Page("text/html"),
        ..DEFAULT
    },
    Op {
        path: "/_studio/{file}",
        at: FILE | NODE,
        id: "studioFile",
        tag: "Server",
        summary: "One of the studio's files",
        flag: "--studio",
        auth: Auth::Open,
        answer: Answer::Page("*/*"),
        ..DEFAULT
    },
    Op {
        path: PATH,
        at: FILE | NODE,
        id: "openapi",
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
        auth: Auth::Admin,
        ..DEFAULT
    },
    Op {
        path: "/_admin/open",
        at: NODE,
        id: "adminOpen",
        tag: "Admin",
        summary: "The open tenants: name, memory, frozen",
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
        auth: Auth::Admin,
        ..DEFAULT
    },
    Op {
        path: "/_admin/quotas",
        at: NODE,
        id: "adminQuotas",
        tag: "Admin",
        summary: "Every tenant's quota and usage",
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
        auth: Auth::Admin,
        ..DEFAULT
    },
    Op {
        path: "/_admin/tenants/{tenant}/hook-secrets",
        at: NODE,
        id: "adminHookSecrets",
        tag: "Admin",
        summary: "A tenant's hook secrets whole, for a router copying them onto a copy",
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
        auth: Auth::Admin,
        ..DEFAULT
    },
    Op {
        path: "/_admin/tenants/{tenant}/file",
        at: NODE,
        id: "adminExport",
        tag: "Admin",
        summary: "A tenant's whole image",
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
        flag: "--lease",
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
        flag: "--lease",
        auth: Auth::Admin,
        ..DEFAULT
    },
];

/// Every operation of the document of `kind`: its path there, its id there
/// and its row.
fn operations(kind: Kind) -> Vec<(String, String, &'static Op)> {
    let mut out = Vec::new();
    // A node's own routes come first, then each tenant's under its prefix.
    let (root, under) = match kind {
        Kind::File => (FILE, 0),
        Kind::Node => (NODE, TENANT),
        Kind::Router => (0, TENANT),
    };
    for op in OPS.iter().filter(|op| op.at & root != 0) {
        out.push((op.path.to_string(), op.id.to_string(), op));
    }
    for op in OPS.iter().filter(|op| op.at & under != 0) {
        let path = match op.path {
            "/" => "/t/{tenant}".to_string(),
            p => format!("/t/{{tenant}}{p}"),
        };
        // An id is the document's: a node holds `/_whoami` and each
        // tenant's both.
        let id = match kind {
            Kind::Node => {
                let mut id = String::from("tenant");
                let mut first = op.id.chars();
                id.extend(first.next().map(|c| c.to_ascii_uppercase()));
                id.push_str(first.as_str());
                id
            }
            _ => op.id.to_string(),
        };
        out.push((path, id, op));
    }
    if kind == Kind::Router {
        let spec = OPS.iter().find(|op| op.path == PATH).expect("listed");
        out.push((PATH.to_string(), spec.id.to_string(), spec));
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
    o.push_str(r#","version":"#);
    str_into(&mut o, env!("CARGO_PKG_VERSION"));
    o.push_str(r#","description":"#);
    let mut about = String::from(match kind {
        Kind::File => "fennec-server's REST API over one database file.",
        Kind::Node => {
            "A tenant node's REST API (fennec-server --dir): each tenant's routes below \
             /t/{tenant}, the node's own at the root, /_admin/ with --admin-token."
        }
        Kind::Router => {
            "fennec-shard's REST API: each tenant's routes below /t/{tenant}, forwarded to \
             its node. The router's own /_shard/ routes are its admin's, in the sharding \
             reference."
        }
    });
    about.push_str(ABOUT);
    str_into(&mut o, &about);
    o.push_str(r#","license":{"name":"Apache-2.0","identifier":"Apache-2.0"}}"#);
    o.push_str(r#","externalDocs":{"url":"https://fennecdb.com/docs/http-reference"}"#);
    o.push_str(r#","servers":[{"url":"/"}],"security":[{"token":[]},{"jwt":[]}],"tags":["#);
    let ops = operations(kind);
    let mut tags: Vec<&str> = Vec::new();
    for (_, _, op) in &ops {
        if !tags.contains(&op.tag) {
            tags.push(op.tag);
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
    for (path, _, _) in &ops {
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
        for (_, id, op) in ops.iter().filter(|(p, _, _)| p == path) {
            if !first {
                o.push(',');
            }
            first = false;
            operation_into(&mut o, id, op);
        }
        o.push('}');
    }
    o.push_str(r#"},"components":"#);
    o.push_str(COMPONENTS);
    o.push('}');
    o
}

const ABOUT: &str = " A request takes `Authorization: Bearer` with the server's token \
(--http-token) or a scoped JSON Web Token (--jwt-secret, --jwt-keys), held to the --policy \
rules; a server started with neither takes requests with no token. Every answer carries \
X-Request-Id; a refusal is {\"error\": \"...\"} under a status that says what to do about \
it, and a 503 says when to come back in Retry-After and its body's retry_after. A route \
marked x-fennec-flag is served under that flag. A GET answers HEAD too, and every path \
answers a CORS preflight under --http-cors.";

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

fn operation_into(o: &mut String, id: &str, op: &Op) {
    str_into(o, op.method);
    o.push_str(r#":{"operationId":"#);
    str_into(o, id);
    o.push_str(r#","tags":["#);
    str_into(o, op.tag);
    o.push_str(r#"],"summary":"#);
    str_into(o, op.summary);
    if !op.flag.is_empty() {
        o.push_str(r#","x-fennec-flag":"#);
        str_into(o, op.flag);
    }
    let security = match op.auth {
        Auth::Token => None,
        Auth::Full => Some(r#"[{"token":[]}]"#),
        Auth::Admin => Some(r#"[{"admin":[]}]"#),
        Auth::Replication => Some(r#"[{"replication":[]}]"#),
        Auth::Scrape => Some(r#"[{"token":[]},{"admin":[]},{}]"#),
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
    answer_into(o, op.answer);
    if op.params.contains(&"IfNoneMatch") {
        // A read polled with If-None-Match is answered so while nothing
        // it reads was written.
        o.push_str(r##","304":{"$ref":"#/components/responses/NotModified"}"##);
    }
    if !matches!(op.auth, Auth::Open | Auth::Signed) {
        o.push_str(r##","401":{"$ref":"#/components/responses/Unauthorized"}"##);
    }
    o.push_str(r##","503":{"$ref":"#/components/responses/Unavailable"}"##);
    o.push_str(r##","default":{"$ref":"#/components/responses/Refused"}}}"##);
}

fn answer_into(o: &mut String, answer: Answer) {
    const ID: &str = r##""X-Request-Id":{"$ref":"#/components/headers/RequestId"}"##;
    const SEQ: &str = r##","Fennec-Seq":{"$ref":"#/components/headers/FennecSeq"},"Fennec-History":{"$ref":"#/components/headers/FennecHistory"}"##;
    let (status, what, media, schema, seq) = match answer {
        Answer::Data(s, schema, what) => (s, what, "application/json", schema, true),
        Answer::Json(s, schema, what) => (
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
    if seq {
        o.push_str(SEQ);
        if matches!(answer, Answer::Lines) {
            o.push_str(r##","Fennec-Next":{"$ref":"#/components/headers/FennecNext"}"##);
        }
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
"replication":{"type":"http","scheme":"bearer","description":"--replication-token: what a replica, an archiver or a router sends."}},
"parameters":{
"collection":{"name":"collection","in":"path","required":true,"description":"A collection's name.","schema":{"type":"string"}},
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
"RetryAfter":{"description":"Seconds to wait before coming back.","schema":{"type":"integer"}}},
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
"Near":{"type":"object","required":["vector"],"properties":{"vector":{"type":"array","items":{"type":"number"}},"field":{"type":"string","description":"The vector field, where the collection has several."},"limit":{"type":"integer","minimum":1}}},
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
"Unauthorized":{"description":"No token, or a wrong one.","headers":{"WWW-Authenticate":{"schema":{"type":"string"}},"X-Request-Id":{"$ref":"#/components/headers/RequestId"}},"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Error"}}}},
"Unavailable":{"description":"Come back after Retry-After seconds. Fennec-Unserved (as the connection was taken) or, from fennec-shard, Fennec-Unsent: 1 says the request never ran.","headers":{"Retry-After":{"$ref":"#/components/headers/RetryAfter"},"Fennec-Unserved":{"schema":{"type":"string","enum":["ceiling","no_thread"]}},"Fennec-Unsent":{"schema":{"type":"string"}},"X-Request-Id":{"$ref":"#/components/headers/RequestId"}},"content":{"application/json":{"schema":{"allOf":[{"$ref":"#/components/schemas/Error"},{"required":["retry_after"]}]}}}},
"NotModified":{"description":"Unchanged since the If-None-Match tag: no body, the read not run.","headers":{"ETag":{"schema":{"type":"string"}}}}}}"##;

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
            assert_eq!(text(get(info, "version")), env!("CARGO_PKG_VERSION"));
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
            for (i, (path, _, op)) in ops.iter().enumerate() {
                let twice = ops[i + 1..]
                    .iter()
                    .any(|(p, _, o)| p == path && o.method == op.method);
                assert!(!twice, "{kind:?}: {} {path} twice", op.method);
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
            "the router's admin routes, named here only by the audit log",
        ),
    ];

    /// A route pattern the dispatcher matches, read from its source.
    #[derive(Debug)]
    struct Pattern {
        at: String,
        method: Option<String>,
        /// Each segment's literals, empty for any.
        segments: Vec<Vec<String>>,
        /// Whether more segments may follow.
        open: bool,
    }

    /// Every route pattern in the server's code, in the forms its
    /// dispatcher matches a request by: a `(Method::X | .., [..])` arm, a
    /// `segments() == [..]` or `.as_slice() == [..]`, a `matches!(..
    /// .as_slice(), [..])`, a first segment (`Some(&"_x")`) and a path or
    /// its prefix (`path == "/_x"`, `starts_with("/_x")`). A route written
    /// in another form is one this list must learn.
    fn patterns() -> Vec<Pattern> {
        let mut files = Vec::new();
        let mut dirs = vec![std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src"
        ))];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    dirs.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    files.push(p);
                }
            }
        }
        let mut out = Vec::new();
        for f in files {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            let src = std::fs::read_to_string(&f).unwrap();
            // The code, not its tests or its comments.
            let src = src.split("#[cfg(test)]\nmod ").next().unwrap();
            let code: String = src
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join(" ");
            let code = code.split_whitespace().collect::<Vec<_>>().join(" ");
            // `admin::handle` matches the segments after `_admin`.
            let prefix: &[&str] = if name == "admin.rs" { &["_admin"] } else { &[] };
            scan(&name, &code, prefix, &mut out);
        }
        out
    }

    fn scan(at: &str, code: &str, prefix: &[&str], out: &mut Vec<Pattern>) {
        let mut push = |method: Option<String>, segments: Vec<Vec<String>>, open: bool| {
            let segments = prefix
                .iter()
                .map(|p| vec![p.to_string()])
                .chain(segments)
                .collect();
            out.push(Pattern {
                at: at.into(),
                method,
                segments,
                open,
            });
        };
        let mut i = 0;
        while i < code.len() {
            let rest = &code[i..];
            // `(Method::A | Method::B, [..])`
            if let Some(after) = rest.strip_prefix("Method::") {
                let mut methods = Vec::new();
                let mut s = after;
                loop {
                    let end = s
                        .find(|c: char| !c.is_ascii_alphanumeric())
                        .unwrap_or(s.len());
                    methods.push(s[..end].to_ascii_lowercase());
                    s = &s[end..];
                    match s.strip_prefix(" | Method::") {
                        Some(more) => s = more,
                        None => break,
                    }
                }
                if let Some(pattern) = s.strip_prefix(", [") {
                    let (segments, open) = slice(pattern);
                    for m in methods {
                        push(Some(m), segments.clone(), open);
                    }
                }
                // Past every method read: `Method::Head` after `Method::Get |`
                // is this arm's, not one of its own.
                i = code.len() - s.len();
                continue;
            }
            for anchor in ["segments() == [", "as_slice() == [", "as_slice(), ["] {
                if let Some(pattern) = rest.strip_prefix(anchor) {
                    let (segments, open) = slice(pattern);
                    push(None, segments, open);
                }
            }
            for anchor in ["Some(&\"", "path == \"/", "starts_with(\"/"] {
                if let Some(word) = rest.strip_prefix(anchor) {
                    let word = &word[..word.find('"').unwrap_or(0)];
                    let segments: Vec<Vec<String>> = word
                        .split('/')
                        .filter(|s| !s.is_empty())
                        .map(|s| vec![s.to_string()])
                        .collect();
                    if !segments.is_empty() {
                        push(None, segments, true);
                    }
                }
            }
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }

    /// A slice pattern's segments, read up to its `]`: a literal or its
    /// alternatives (`"plan" | "apply"`), anything else any segment, `..`
    /// more of them.
    fn slice(pattern: &str) -> (Vec<Vec<String>>, bool) {
        let body = &pattern[..pattern.find(']').unwrap_or(0)];
        let mut segments = Vec::new();
        let mut open = false;
        for part in body.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            if part.ends_with("..") {
                open = true;
                continue;
            }
            let words: Vec<String> = part
                .split('|')
                .map(str::trim)
                .filter_map(|w| w.strip_prefix('"').and_then(|w| w.strip_suffix('"')))
                .map(str::to_string)
                .collect();
            segments.push(words);
        }
        (segments, open)
    }

    fn documented(p: &Pattern) -> bool {
        let first = p.segments.first().and_then(|s| s.first());
        if first.is_some_and(|w| UNDOCUMENTED.iter().any(|(u, _)| u == w)) {
            return true;
        }
        let method = match p.method.as_deref() {
            // A preflight is every path's; a HEAD is its GET's.
            Some("options") => return true,
            Some("head") => Some("get"),
            m => m,
        };
        // A path below a tenant is a tenant's route, as a file's server
        // has it.
        let (wanted, at) = match p.segments.split_first() {
            Some((t, [_, rest @ ..])) if t == &["t"] => (rest, TENANT),
            _ => (&p.segments[..], FILE | NODE),
        };
        OPS.iter().any(|op| {
            if op.at & at == 0 || method.is_some_and(|m| m != op.method) {
                return false;
            }
            let segs: Vec<&str> = op.path.split('/').filter(|s| !s.is_empty()).collect();
            let fits = match p.open {
                true => segs.len() >= wanted.len(),
                false => segs.len() == wanted.len(),
            };
            // A placeholder stands for a name, never one of the server's
            // own words: `/_x` is no collection's path.
            fits && wanted
                .iter()
                .zip(&segs)
                .all(|(want, have)| match have.starts_with('{') {
                    true => !want.iter().any(|w| w.starts_with('_')),
                    false => want.is_empty() || want.iter().any(|w| w == have),
                })
        })
    }

    /// Every route the dispatcher matches is in the document: a route
    /// added without its row in [`OPS`] fails here.
    #[test]
    fn every_route_the_server_matches_is_documented() {
        let all = patterns();
        // The scan reads what it should: the forms above, in the files that
        // route.
        assert!(all.len() > 60, "{}", all.len());
        for at in [
            "lib.rs",
            "admin.rs",
            "cdc.rs",
            "api.rs",
            "replication.rs",
            "node_stream.rs",
        ] {
            assert!(all.iter().any(|p| p.at == at), "nothing read in {at}");
        }
        let missing: Vec<&Pattern> = all.iter().filter(|p| !documented(p)).collect();
        assert!(
            missing.is_empty(),
            "routes the server matches that the OpenAPI document (openapi.rs's OPS) \
             leaves out: {missing:#?}"
        );
    }

    /// The scan finds what it should, and a route no row lists is one it
    /// refuses.
    #[test]
    fn the_scan_finds_a_route_left_out() {
        let mut out = Vec::new();
        let code = r#"match (req.method, segs) { (Method::Get | Method::Head, ["_nosuch", name]) => 1, (Method::Put, ["_schema", "plan"]) => 2 } if req.segments() == ["_whoami"] {} if p.starts_with("/_elsewhere/") {}"#;
        scan("x.rs", code, &[], &mut out);
        let found: Vec<bool> = out.iter().map(documented).collect();
        assert_eq!(found, [false, false, false, true, false], "{out:#?}");
    }
}

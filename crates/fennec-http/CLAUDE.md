# CLAUDE.md

fennec-http's invariants -- replication and archives, access, quotas, the HTTP server and held writes -- for code in `crates/fennec-http/`. The rules every change meets, and where every other area's are, are in the root's [CLAUDE.md](../../CLAUDE.md).

## Invariants

### Replication and archives

**[Replication ships only what is on disk, by the change counter.](../../docs/design/replication.md#replication)**
Every write goes through `wal`, its record moving the counter by its
writes (`writes_in`); the history and a tail's graphs, which move none, are
never sent. A durable write waits for every stream keeping up
(`Feed::wait_sent`, `SENT_WAIT` at most, a lagging one not waited for); a
promotion halts the follower before it forks, and a following database
refuses writes (`Error::ReadOnly`).

**[A node's tenants are archived over one stream](../../docs/design/replication.md#a-nodes-archive)**
(`fennec_http::node_stream`), a sender on each open tenant's feed:
`Tenants::close` refuses while it has a write left (`Feed::unsent`), and
whatever the stream does that can block sets `Sender::held` first, which
no durable write waits for; a sync waits for no stream
(`replication::unwaited`). Whatever makes, installs, deletes, promotes or
makes a tenant follow touches the stream, and an image on a history not
leading from an archive's end never lands among its changes
(`archive::Took::Diverged`).

**[`/_changes` reads the writes on disk, documents and all](../../docs/design/replication.md#change-data-capture)**
(`Database::changes_in`): only what an fsync covered, never with writes
missing (410); a scoped token is refused. A consumer is made by a `POST`,
which never moves one that is there (200 at its place, 409 for a `since`
ahead of it), and moved by a `PATCH` alone, which makes none
(`cdc::make`, `cdc::commit`); on a tenant each is a write held against a
move and the lease (`consumer_write`).

**[A read can wait for a write on a replica](../../docs/design/replication.md#fennec-after)**
(`Hub::reached`): served once the database holds that change or 504
after `Fennec-Wait` (30 s at most), never before. Every wait comes after
its token (`admitted`), a stream's before its head.

**[An archive and a backup can be sealed](../../docs/design/replication.md#sealed-archives)**
(`seal.rs`, `--key-file`), never the live file.

### Access

**[A scoped token is held to its rules at every level, twice for
writes.](../../docs/design/access.md#scoped-tokens)** A new path that runs a
statement must go through `scoped()` or it reads everything; a scoped write
outside `within()` goes unchecked, so only a named statement's call runs
one there (`run_as`), and a binding's writes, which the `Check` hook passes
over (`Hook::writer_only`) once `Hook::waking` judged it: a `wake` line
grants it (`* wake` refused), its body reaches no `append-only` collection
(403, the audit's `unwoken` line naming it), its runs charged to
`--call-rate` (`calls::charge_runs`), and `judge_call` holds a body to the
same. A write over a row meets `before_overwrite` with the
row as it was, judging `update(a, b)` and transitions (`new.`, granting
`update` alone); `append-only` binds every scoped token. Scoped, a stream
tells deletions only of ids it sent, a poll's tag digests its answer
(`content_tag`) and `match` scores its rows alone (`Match::within`); the
algorithm is the key's, never the token's. A claim held as `null`, `""` or
a value its rule cannot compare (`usable`) is one the token lacks: its rule
does not apply, binding no filter. A claim a rule orders by holds the other
side to its kind under any number of `not`s (`kind_held`), a row with no
value answering as the rule written out; an inner `get`'s rules are bound
for the `not`s it stands under (`hold`). Refused at startup: a claim in a
call or arithmetic (`like`'s pattern aside) or compared in order inside a
value (`claims_held`), and a policy writing its own `$<n>`. A tenant claim
naming no tenant reaches none; a `sub` of `""` is no subject.

**[A field a read rule withholds is held out of what a statement
does.](../../docs/design/access.md#field-lists)** `fields::hold` and
`fields::write` guard or refuse (403) each use of it before the rules are
ANDed in; it is `null` in a row (`Scope::shown`), absent from every schema
answer (`Scope::shown_schema`) and refused by name as a missing one, and a
stream sends no row a write left showing the same (`sse::Seen`).

**[A rule may read another collection, answered before each
statement.](../../docs/design/access.md#rules-reading-another-collection)**
Its `in` lists are answered under each lock (`Scope::bound`), the scope
`Check`, the mask and streams use; a list past `MAX_CLAIM_VALUES` is
refused, never cut, and rules reading each other's collections, a `*` rule
reading one, or `new.` or `expired()` in one are refused at startup
(`ring`).

**[A token is bound to the tenant it
names.](../../docs/design/access.md#tenant-binding)** `route_tenant` asks it,
and no route below `/t/<t>/` may skip it; every tenant's database installs
the `Check` hook (`Tenants::check_scoped_writes`).

**[A token is taken only from the issuers, for the audiences and clients,
a server was told.](../../docs/design/access.md#whom-a-token-is-for)**
`Demands::whom` asks `iss`, `aud` and `azp` (else `client_id`) in
`Access::check`, after the signature and before a token is kept, so every
route meets it through `authenticate`; a refusal is 401 naming the check,
never the token's value, as its audit line does (`audit::refusal`), and
clears no wait. A server told none takes every token its keys sign (said at
the start under `--jwt-keys`); a `--jwt-*` flag without keys is refused. A policy names top-level claims alone, quoted
where not a word (`claim_name`): a path into a nested object, or an
unquoted name running into `-` or `:`, is refused at startup, never read; a
role is the word after the last `for` or a quoted name (`role_of`).

**[An empty token is no token.](../../docs/design/access.md#empty-tokens)**
A variable set empty is unset (`env_secret`, fennec-shard's `env`); a token
flag, a JWT secret's file or a key file given empty is refused at the
start; a library constructor takes an empty token as none
(`Config::settled`).

### Quotas and retries

**[A tenant is held to its quota by its node, a block as it
lands.](../../docs/design/quotas.md#tenant-quotas)** A block or spill past
its bytes or rows is put back (507) as it lands (`Database::set_limit`),
one only taking away let through; a refusal names the bound alone, and a
rate charges a request's own thread alone (`writing_here`). The router
enforces nothing but sends the quota to the tenant's node, standby,
replica, a move's target before the flip and a promoted copy.

**[A quota is a refusal where waiting makes no room, a wait the server
names otherwise.](../../docs/design/quotas.md#quotas-in-the-sync)** A synced
507 naming a `quota` is put back; a retry waits `retry_after_ms` where
longer than its backoff.

**[Every 503 says when to come back, in its body
too.](../../docs/design/quotas.md#retry-after-in-bodies)** Write one with
`Response::retry_after`.

### The HTTP server

**[An agent's MCP tool is a statement, held to the token as `/query`
is.](../../docs/design/http.md#mcp)** A model's filter goes in parsed, never as
text, and a read past `--mcp-max-rows` or `--mcp-max-bytes` is refused,
never cut. The origin is asked before the token, and with no token only a
`Host` naming localhost or an address is answered; `write` never changes a
schema or compacts. A named statement the token may call is a `call_<name>`
tool, its answer held to the bounds before a write's block lands
(`mcp::land`).

**[An `Idempotency-Key` makes a write
once.](../../docs/design/http.md#idempotency-keys)** The write and its key's
`_idempotency` row are one block, the key the subject's for a scoped token
and the key with another request 422; a failed write or a `compact` keeps
none.

**[Refusals and schema changes are logged; a refusal
waits.](../../docs/design/http.md#audit)** Only `audit::verified`, where a token
checks out, clears the wait; a new check must call it, and nothing infers
it from an answer. A node believes `Fennec-Router` from the router alone.

**[Every request has an id, in its answer and every line it
wrote.](../../docs/design/http.md#request-ids)** `tests/datadog.rs` and
`tests/logs.rs` hold `monitoring/datadog/` to what is sent.

**[A request is a trace, sent over OTLP.](../../docs/design/http.md#traces)** A
thread-local from `begin` to `end`, `discard`ed before a stream takes the
connection over; a span names a statement's shape, never a value, and a
request never waits on the collector.

**[`/_metrics` counts at the edge, a shard per
thread.](../../docs/design/http.md#metrics)** Timed around `handle`, counted
before the answer is written; no label names a tenant or its collections,
and `--metrics` attaches no watcher.

**[`/_stats/statements` counts by shape, a tenant's
apart.](../../docs/design/http.md#statement-counts)** Literals are `$n`
(`statements::shape`). A shape names every collection, so they take what
reads the data unscoped or the admin token; on a tenant node a tenant
reads its own, and every tenant's, named, are the admin's alone.

**[A statement is parsed once, and an answer goes out in one
write.](../../docs/design/http.md#parse-cache)** A JWT's scope is ANDed into a
copy, never the cached parse; an answer is one `writev`
(`http::write_parts`), over TLS one plaintext whose records go in one.

**[A chunked body grows as it comes, held to the
ceiling](../../docs/design/http.md#chunked-bodies)** (`read_onto`); a framing
read two ways is refused (`chunked_framing`, `end_refused`).

**[A plain `get` is written as JSON from the stored
documents.](../../docs/design/http.md#json-from-stored-rows)**
`Database::query_json` must match `query` through `json::rows_array_into`
byte for byte (`tests/query_json.rs`).

**[A connection no thread can be made for is answered
503.](../../docs/design/http.md#connections-without-a-thread)** Both accept loops
spawn through `accept::spawn`, which hands back a refused thread's stream
for `accept::refuse` to answer on the accepting thread, non-blocking,
counted by `reason` and marked `Fennec-Unserved`: unrun to the entry and
relays, as a standby's 409 marked `Fennec-Follows` is. Its writing then
shut, the one lingering thread (`accept::linger`) reads it off till the
client ends it; only no such thread, no room (`most_held`) or an answer not
sent whole closes it at once. A new accept loop calls
`accept::ready()` and `accept::widen` before its first accept. A TLS
listener's is told `fennec_tls::REFUSAL`, an alert (`accept::refuse_tls`):
no handshake runs on the accepting thread.

**[`--tls-cert` and `--tls-key` make a listener HTTPS
alone](../../docs/design/http.md#tls)**, `--metrics` too, each handshake on its
connection's thread (`Tls::handshake`, `conn::Conn::open`), a plain request
a 400; without the flags the `&Conn` is the `TcpStream` it was, nothing
added. A failed handshake is counted by reason, logged a minute apart at
most; an end on purpose sends `close_notify` (`conn::end_all`), never after
a failed write or `Conn::cut`.

**[Every HTTP dial takes HTTPS by its URL.](../../docs/design/http.md#dialing-https)**
`https://host:port` -- a primary, a node, a region's router, the OTLP
collector, `fennec backup` and `mcp --url` -- is dialed over TLS
(`dial::tls_of`), its handshake inside the dial and bounded
(`dial_within`), so a refused certificate sends nothing; the CAs are
Mozilla's or `--tls-ca`'s, never both. A node's scheme is in its address,
never a field an older router would skip (RELEASING.md).

### Held writes

**[`Fennec-Wait` holds a `set` or a `del` that writes nothing until it
can](../../docs/design/waits.md#fennec-wait)** (`fennec_http::waits`, 30 s at
most), each run `handle` whole, token and all; a `put`, or the time read
but as a field against `now()` plus or minus integers or parameters, is
refused (`holdable`). A call is held by its body, an `insert` refused, and
whether it wrote is asked of the database (`waits::Mark`), never its last
statement's answer; a binding's unmet `require` (`Database::woken`) is
answered at once, never held. The builders refuse `wait` on a database in
the page (`LOCAL_WAIT`).

**[A write wakes the held writes reading what it wrote, a row going to the
group ranking it first](../../docs/design/waits.md#waking-held-writes)**
(`Waits::wrote`, `waits::settle`). A landing that cannot name its
collections wakes every group (`None`).

**[One held request looks for every group, and a group finding nothing
looks among the rows written since](../../docs/design/waits.md#the-leaders-looks)**
(`waits::look_all`): a look among the rows written must pick as the whole
pick does, which runs wherever that is not shown -- a load, a compact, a
maintenance or a schema mark moves `epoch` (`wrote(None)`).

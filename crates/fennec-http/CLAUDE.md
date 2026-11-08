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
refuses writes (`Error::ReadOnly`). A panic applying the primary's records
is a write the follower could not apply, caught with the lock held
(`Follower::session`): never a thread gone, which a halt waits for.

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
(`seal.rs`, `--key-file`), never the live file. A frame's nonce, as every
key and nonce, is the system's randomness (`crypto::system_random`, read
with no descriptor), the seal refused without it: never the clock, which
only an id may fall back to (`crypto::id_u64`).

### Access

**[A scoped token is held to its rules at every level, twice for
writes.](../../docs/design/access.md#scoped-tokens)** A new path that runs a
statement must go through `scoped()` or it reads everything; a scoped write
outside `within()` goes unchecked, so only a named statement's call runs
one there (`run_as`), and a binding's writes, which the `Check` hook passes
over (`Hook::writer_only`) once `Hook::waking` judged it -- a row passing
its condition or not where that reads a field the token is not shown
(`Scope::reads_unshown`): a `wake` line
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
(`ring`). A stream's filter reading another collection -- a rule's, or the
shape's own inner `get`, any token's -- is answered again at each look
after a write to what it reads, once a row of it reached its `@ttl`, or
a second after the read answering it ended where it reads the time -- the
stream wakes for both (`sse::Reach`), and writes its keep-alive once one
passed with nothing written, whatever woke it -- on a pin where a query of
it would be (`sse::look`); the shape runs again only where it moved, its
rows sent as a diff against those it was sent (`enter_and_leave`).

**[A collection whose name starts with `_` is the server's, out of every
rule's reach.](../../docs/design/access.md#the-servers-own-collections)**
`*` reaches every other (`access::covers`, `declared::own`); a rule naming
or reading one is refused at startup, and a scoped token is answered as
for a missing collection on every route -- as for any it is granted
nothing on, whatever a REST request's route read of the schemas its path
and each `lookup` level name (`path_refusal`), a subscription's inner
`get` held to the rules (`Scope::subscribed`). The server's token reads
them, and `scoped()` refuses its statement making, changing, compacting
or writing one (`access::own_refused`), as the schema routes a
description declaring one (`own_described`, off the body, before the
write rate) or a migration still to run reaching one (`own_migrations`,
under the lock; one recorded is not asked). The server
writes its own through the engine, never a statement; a new one is
`_`-named and listed in `declared::OWN`.

**[A token is bound to the tenant it
names.](../../docs/design/access.md#tenant-binding)** `route_tenant` asks it,
and no route below `/t/<t>/` may skip it -- a hook's event alone, whose
signature it asks in the token's place, against that tenant's secrets,
before the tenant is looked up; every tenant's database installs the
`Check` hook (`Tenants::check_scoped_writes`).

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
none. A replay answers only once the first try's record is as durable and
sent as its answer was to be: under `--sync always` every path that finds
a key runs `Database::durability` after its lock (`idempotent::replay`),
and a failed fsync is its answer, never the kept success; after a storage
error a replay is the refusal under either `--sync`, its outcome unknown.
A replay is a write's answer on every path, a call made to read's too
(`metrics::wrote`), asked of a tenant's lease (`answered_under_lease`).

**[Refusals and schema changes are logged; a refusal
waits.](../../docs/design/http.md#audit)** Only `audit::verified`, where a token
checks out, clears the wait; a new check must call it, and nothing infers
it from an answer. A node believes `Fennec-Router` from the router alone.

**[Every request has an id, in its answer and every line it
wrote.](../../docs/design/http.md#request-ids)** `tests/datadog.rs` and
`tests/logs.rs` hold `monitoring/datadog/` to what is sent.

**[A request is a trace, sent over OTLP.](../../docs/design/http.md#traces)** A
thread-local from `begin` to `end` -- a scheduled run's from `begin_run` to
`end_run`, one `start` and `finish` for both -- `discard`ed before a stream
takes the connection over; a span names a statement's shape, never a value
-- a binding's or an event's statement a child by its name (`ran_spans`),
never the event's key or body -- and a request never waits on the
collector.

**[`/_metrics` counts at the edge, a shard per
thread.](../../docs/design/http.md#metrics)** Timed around `handle`, counted
before the answer is written; no label names a tenant or its collections,
and `--metrics` attaches no watcher. The scheduler's families are written
only once a look was made (`schedule::LOOKED`), a server holding no
schedule scraping the bytes it did, and are one set of atomics: one thread
a process counts them; the hooks' once a hooks file was read
(`hooks::SERVING`).

**[`/_stats/statements` counts by shape, a tenant's
apart.](../../docs/design/http.md#statement-counts)** Literals are `$n`
(`statements::shape`), a quoted key kept as written; a shape the server
counts of its own -- `schedule <name>`, `after <name>`, `hook <name>` -- is
keyed where no client's text is (`statements::own_key`). A shape names every collection,
so they take what reads the data unscoped or the admin token; on a tenant
node a tenant reads its own, and every tenant's, named, are the admin's
alone.

**[A webhook is verified by its signature before anything of it is read,
and every refusal before is one 401.](../../docs/design/named-statements.md#verifying-an-event)**
`POST /_hooks/<name>`, on a file's server under `--hooks`, and a node's
`/t/<t>/_hooks/<name>`, asked in `route_tenant` before the tenant is looked
up against its secrets in the node's memory (`tenant_hooks::Store`) and held
after its open to the set it holds then (`Store::is_current`), is the
one route running a statement without a token, judged before
`authenticate`: `hooks::verified` reads the scheme from the headers
present (each once, one `t`, `webhook-*` or Svix's `svix-*`, never both)
and checks two HMAC-SHA256s always (`crypto::HmacKey`, `ct_eq` with
every signature through `compared`, `v1` alone, the process's random key
for a secret missing, never verifying) over the raw body, as far as the
widest hook's ceiling, and the timestamp it signs, within the hook's
tolerance and ceiling, before the body is parsed, a tenant opened or a
lock taken. Every failure before -- headers, scheme, tolerance, ceiling, no
such tenant or hook, the signature -- is one 401 after the same work, its
check named in the log, the audit's `refused` line and
`fennec_hook_unverified_total` alone; it waits by the hooks' own count, by
the address and the name the decoded path names (`hooks::failed`, `hooks::named`) -- a router forwarding the
node's 401 with no `WWW-Authenticate` too (`audit::hook_forwarded`) -- and
a verified event never calls `audit::verified`. A secret is a hooks file's
or a tenant's `<t>.hooks` (fennec-shard's rule), never the database's, an
archive's, a log's, a trace's or an answer's but `hook-secrets`'. An
event's key is a value its signature covers, or `X-GitHub-Delivery` where
its hook says `key delivery`, kept more than twice the tolerance and
`SKEW_MS`. A sender is told 2xx only once its event
is durable whatever `--sync` says (`hooks::taken` flushes as `--sync
always` does), and a standby holds it (`Feed::standby_sent`, an archiver's
or a read replica's stream none, each saying so on its request; else 503),
or where resending changes nothing, a replay no sooner than its first try,
a standby holding its key's block (`HeldKey::seq`); a tenant's asked of its
lease after (`answered_under_lease`, 500 `outcome: unknown`). Any other
answer is the refusal's status, told as a scoped caller is
(`hooks::told`), never a 410 or a 3xx.

**[A statement is parsed once, and an answer goes out in one
write.](../../docs/design/http.md#parse-cache)** A JWT's scope is ANDed into a
copy, never the cached parse; an answer is one `writev`
(`http::write_parts`), over TLS one plaintext whose records go in one.

**[A text's lists are read as written at once from its second
request.](../../docs/design/vector-search.md#numbers-in)** Its parse-cache
entry, or a long text's hash (`api::HINTS`), keeps the places its last
request read again (`Cached::hint`), held to `Database::exactly` each
request, a whole list read so given back as the quick read's vector
(`value::whole_vector`): no value depends on a hint, whatever text or
tenant left it. A hint keeps at most 4 096 places, a shard of long texts'
16 384 (`HINT_PLACES`, `HINT_BUDGET`); a page's documents' lists are read
again alone (`fennec_ql::read_listed`).

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
no handshake runs on the accepting thread. A connection's place under
`--max-connections` is an `accept::Slot`, given back by its drop before
its socket closes, on every way out, a panic's unwinding too -- never by a
line after the connection's work, which a panic skips; so is every count
taken around a connection or a held request (`waits::Answering`, a
`waits::Ticket` leaving its group).

**[`--tls-cert` and `--tls-key` make a listener HTTPS
alone](../../docs/design/http.md#tls)**, `--metrics` too, each handshake on its
connection's thread (`Tls::handshake`, `conn::Conn::open`), a plain request
a 400; without the flags the `&Conn` is the `TcpStream` it was, nothing
added. A failed handshake is counted by reason, logged a minute apart at
most; an end on purpose sends `close_notify` (`conn::end_all`), never after
a failed write or `Conn::cut`. A timeout set where the peer may have reset,
bytes of its still to read, goes through `conn::unless_gone` (fennec-tls's
own in a handshake): macOS's EINVAL there never fails a handshake done.

**[Every HTTP dial takes HTTPS by its URL.](../../docs/design/http.md#dialing-https)**
`https://host:port` -- a primary, a node, a region's router, the OTLP
collector, `fennec backup` and `mcp --url` -- is dialed over TLS
(`dial::tls_of`), its handshake inside the dial and bounded
(`dial_within`), so a refused certificate sends nothing; the CAs are
Mozilla's or `--tls-ca`'s, never both. A node's scheme is in its address,
never a field an older router would skip (RELEASING.md).

**[The OpenAPI document is held to the routes.](../../docs/design/http.md#openapi)**
`GET /_openapi.json` is written once a process from one table
(`openapi::OPS`), the binary's whatever the flags, so it asks no token
and names no build (`DOCUMENT_VERSION`). A route gets its row in the same
change: `every_route_the_server_matches_is_documented` reads every route
fennec-http's and fennec-shard's dispatchers match and finds each in the
table at every place its code serves it (`REGIONS`: a new routing function
gets its region), a route in a form the scan does not read teaching it that
form; fennec-server's and fennec-shard's `tests/openapi.rs` send every
operation a file's server, a tenant node and a router list, none answered
404 or 405, each whose `security` asks a token 401 without one, a HEAD
only where listed.

### Held writes

**[`Fennec-Wait` holds a `set` or a `del` that writes nothing until it
can](../../docs/design/waits.md#fennec-wait)** (`fennec_http::waits`, 30 s at
most), each run `handle` whole, token and all; a `put`, or the time read
but as a field against `now()` plus or minus integers or parameters, is
refused (`holdable`). A call is held by its body, an `insert` refused, and
whether it wrote is asked of the database (`waits::Mark`), never its last
statement's answer; a binding's unmet `require` (`Database::woken`) is
answered at once, never held. Its answer at its wait's end, or a
shutdown's, waits for the disk as a refusal does (`held_answered`), its
runs for none (`waits::plan`). The builders refuse `wait` on a database in
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

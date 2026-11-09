# Quotas and retries

The long form of the invariants [crates/fennec-http/CLAUDE.md](../../crates/fennec-http/CLAUDE.md) lists under *Quotas and retries*: a tenant's quotas, how the sync meets them, a scoped caller's request rate, and the wait every 503 names. Each section below is a paragraph moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists.

## Tenant quotas

**A tenant is held to its quota by its node, a block as it lands**
(`fennec_http::quota`, `fennec_core::engine::limit`). A node's tenants
share its disk, memory and threads, and only `--max-connections` and
`--max-memory` bounded them together. The node's flags
(`--tenant-max-bytes`, `-rows`, `-streams`, `-requests`,
`--tenant-writes-per-sec`, `--tenant-write-bytes-per-sec`) are every
tenant's, and a tenant's own set (`PUT /_admin/tenants/<t>/quota`, kept as
`<t>.quota` beside its file, written beside and renamed) goes over them, 0
for none. The file's bytes (`appended`, no system call) and the rows of
its collections (`Database::documents`, the `_`-named being the server's)
are asked as each block lands (`Database::set_limit`, a `Landing`): the
record's length is known to the byte there, so a block that would take the
file past its quota is put back as a failed one is, 507 -- asked before
the write ran, one `set` over every row would double a file at 99% of it
-- and so is each spill of a block past 16 MB, before it is written, or
every refused block left its spills dead in the file past the quota. A
block that only takes away (`grows` false: deletes, a drop, the server's
own rows, a keyed `del`'s key among them) always lands and a `compact`
always runs, and a file within a quarter of its quota is compacted by the
node once an eighth of the quota is dead (`link::quota_policy`), where the
policy every file has waits for 64 MB and a tenant at a 10 MB quota would
have deleted to no avail. Streams (subscriptions, `/_changes` waits,
`Fennec-After` waits -- once `after` has looked and must wait, its `slot`
taken then: taken for every request sent with the header, a tenant whose
live queries' subscriptions filled its quota, 25 of a default node's
connections, had every read after a write the node held refused 429, each
run of a live query reading near and every SDK's read-your-writes read
that reached home, `tests/quotas.rs` failing with the slot taken first;
a request without the header looks once as before: `make requests-bench`
in three ABBA quartets under the bench lock at loads of 3.4-4.9, a row by
id 0.022 ms p50 in every run either way, eight clients 143.6-147.2k a
second against main's 143.9-147.4k, a put 0.022-0.023 ms and 124.9-129.2k
against 0.022-0.023 and 127.5-128.6k, one branch run at a load of 4.7 left
out), held writes (`held`, `Fennec-Wait`'s: a plan sizes how
many workers wait apart from how many dashboards stream, and counted
together forty couriers waiting left a dispatcher's map no stream to open)
and requests in flight are counters a request takes in `serve_connection`,
`respond` and `handle_tenant`, only where the quota is set; rows and bytes
written a second are token buckets a second deep, a write let through
where it is known to be one (`quota::writing`, a thread-local as
`access::within` is) while a row's worth is left, and charged by the limit
as it lands -- the record's writes to the app's collections
(`Landing::writes`, `Database::app_writes`: an `Idempotency-Key`'s row is
the server's), on a request's own thread alone (`writing_here`: the sweep,
a compact, a replica's apply are the node's) -- so a write larger than a
second's worth goes through, the next waits its debt out, and a write that
wrote nothing costs nothing: taken as a write started, 45 claims held at
once by a fleet on a small plan were refused for `writes` before any for
being held. A held claim the rate refuses as it is to run stays held
(`Ticket::pause`, `rated_out`, `Held::write_wait`) and runs once a row's
worth is back, rather than come back at the end of every group. 429 with
`Retry-After` in whole seconds rounded up, as HTTP has it, and
`retry_after_ms` the wait itself (`quota::millis`): a bucket of 600 a
second refills a row in 1.7 ms, and fifty couriers told a second waited
fifty seconds between them. On a `--dir` node that sets neither
`--tenant-max-streams` nor `--tenant-max-held`, each is a quarter of
`--max-connections` (`quota::shared`, logged at the start): every stream
and held write is a connection and its thread, `--http-max-streams` counts
a database's own -- each tenant's on a node, not all of theirs together --
and nothing counted a tenant's held writes, so a hundred workers of one
tenant took every connection of a default node and its neighbours were
answered 503. Every refusal carries `Fennec-Quota`
and `"quota"` (with `"retry_after"`, a rate's `"retry_after_ms"` too, which
SDKs reading no header take),
and `quota::noted` counts it by quota (`fennec_quota_refused_total`, never
a tenant label) and logs it as an audit `quota` event; its message names
the bound, never what the tenant holds, so a scoped user is told the
tenant is full and not how many rows the others make. Left out: memory
(`memory_bytes` counts what has been built, which a read changes), a
statement's time (the engine stops no scan) and rows per collection. The
router enforces nothing -- it parses no query, and a tenant lives on one
node -- but keeps each tenant's quota in its directory's `quotas` and
sends it to the tenant's node, standby and replica, to a move's target
before the flip, and to a promoted copy again. The tests hold eight
writers to a file's quota and eight to a rows quota exactly, and a tenant
at its streams or requests quota beside another answered, by what each is
answered. A lone put held to bytes and rows took 593 ns against 566 in
process, and 618 with the bytes bucket's mutex and clock besides; over
HTTP a tenant with every quota answered as one with none, reads 20.9 us
either way and writes 23.7 against 23.6 (`make quota-bench`, in turns).
And a tenant with none costs nothing measurable: against main in ABBA
turns, `make requests-bench`'s one client stayed 0.021-0.022 ms a row by
id and 0.020-0.023 a put (main 0.022-0.029 and 0.022-0.029), eight
clients' 137-144k and 115-121k a second inside main's 122-143k and
89-153k; `make shard-bench`'s tenant request 19.5-21.7 us straight to the
node and 43.7-46.0 through the router, main 19.9-21.8 and 43.6-45.7, in
twelve runs before the machine grew busy. The limit is native only, as the
fence is: the browser module grew 63 bytes, the new error's words, and
brotli made it 49 smaller.

## Quotas in the sync

**A tenant's quota is a refusal where waiting makes no room, a wait the
server names otherwise.** A synced write a tenant node refused 507 for its
quota of `bytes` or `rows` was retried as a 5xx: room comes only from
deletes, and writes go in order, so every write after it -- the delete
that would make the room among them, which the engine lands on a tenant
past its quota (`engine/limit.rs`) -- waited behind it for good, and the
app was never told. A 507 whose body names a `quota` is a refusal now
(`Sync::on_answer`, `#pump`): put back, `refused` and `onRefused` told
with the quota, the queue going on; one naming none -- `--max-memory`'s,
which a handover or an idle tenant's close brings back under, or a
proxy's -- is waited out as a 5xx. A retried answer's `retry_after_ms`,
the wait itself where the body says it, or its `retry_after` otherwise, is
waited where it is longer than the backoff (a scenario holds both runners
to a 2 ms wait waited as the backoff's 250 and a 900 ms one as itself; every
SDK's error reads the milliseconds first, Go's and .NET's as a duration), never cut to the backoff's 15
s and an hour at most (`RETRY_AFTER_MAX_MS`: a browser's `setTimeout`
fires at once past 2^31 ms), the 30% jitter on top, since a bucket tells
every client it refused in one moment the same wait. A stream a 429 of
`streams` refuses waits the same way, its shape not `connected`: the
status reads `offline`, as for the network, its error naming the quota
(`error.quota`, JS `status()[i].quota`) rather than a state of its own --
why a replica is not live is the error's to say, as a 401's is. `online`
forgets the backoff but keeps a server's wait (each timer flagged, the
third member of `Sync::timers`, `#wait`'s `server`) and lets the streams
open past a write held so. Both runners read the body, not the headers:
`Sync::response` is handed a body, a header would be a parameter more
through the C ABI and three bindings, and a page on another origin reads
`Retry-After` only where CORS lists it -- so every 503 says it in its body
too (below). Five scenarios hold both runners to it, and the Swift,
Kotlin, Dart and browser tests a real tenant node at its rows quota: the
insert refused naming `rows`, the keyed delete behind it landing.
`fennec.js` grew 99.4 -> 102.4 KB, 27.0 -> 27.9 KB brotli, 0.26 KB of
that code (minified, 12.2 -> 12.5 KB); the native library's code 2.3 KB,
its file 16 bytes on aarch64-apple-darwin; the browser module not by a
byte, the sync being outside it.

## A caller's request rate

**A scoped caller is held to `--request-rate` before anything runs**
(`fennec_http::rate`). A tenant's quotas bound the tenant as a whole, and
`--call-rate` only a named statement's calls: one user of an app, holding
a scoped token, could ask a node for reads, writes, subscriptions and
MCP as fast as one connection goes -- some 45 000 a second for a row by
id -- and take the node from every neighbour. `--request-rate` (1 000 a
second unless told, 0 for none) is a token bucket per caller per
database: the caller a scoped token's issuer and subject
(`Scope::caller`), as `--call-rate`'s is -- a `sub` is unique only within
its issuer's, and names a person within its tenant alone -- and the
database the request runs in, a tenant's own on a node, its file's
`Database` by address. A token naming no `sub` is counted with every
other of its issuer's there.

*Where it is charged.* In `authenticate`, as a scoped token checks out
and only where the request's database is handed in: `handle` (every
route of a file's server and of a tenant -- REST, `/query`, `/batch`,
`/_call`, `/_schema`), MCP's endpoint (`mcp::admit`), a subscription as it
opens (once, however long it streams) and `/_whoami`. The checks asked
again with no database charge nothing -- a tenant's binding before its
lookup (`route_tenant`), a `Fennec-After` request's before its wait
(`admitted`), so a request waiting for a write is charged once, as
`handle` takes it after the wait -- and neither does a held write's run
after its first (`rate::again`, set by `respond` around it): a client's
one request held through ten wakes would otherwise spend eleven of its
second, and be answered 429 at the run past them, the rows it waited for
found. Not charged: `/_changes`, which refuses a scoped token (403) before
it reads anything; a node's root `/_whoami`, which reads no database; the
admin, replication and hook routes, which take no scoped token. The 429
goes back as `authenticate`'s refusal, before the request's body is
parsed, a statement planned or a lock taken: nothing ran, so the clients'
rule sends it again after the wait ([js-client.md](js-client.md#retries)),
and a replica's sync waits it out as any 429 (`Sync::on_answer`). Its body
says the wait to the millisecond (`retry_after_ms`), beside `Retry-After`
in whole seconds and `retry_after`: a bucket of 1 000 a second refills a
request in a millisecond, and a client told a second waits a thousand
times too long. Never `completed`: nothing landed. The token was
verified, so the refusal waits for nothing (`audit::verified`) and writes
no audit line -- a caller past its rate is the one client the log would
fill with lines -- but is counted (`fennec_request_rate_refused_total`)
for an operator to see a bound biting.

*How it composes.* A call is a request and a call: charged to
`--request-rate` as its token is verified, then to `--call-rate` once its
statement is parsed, two buckets, so a `--request-rate` below
`--call-rate` bounds calls at it, and a request its call's bucket refuses
has still spent its request. An MCP POST is a request, its `tools/call`
also charged to `--mcp-calls-per-sec` (`mcp::admitted`, by subject, the
server's token by address) -- a model's turn, rarer and costlier than a
request. A tenant's quotas (`requests` in flight, `writes` a second, ...)
bound the tenant whatever its callers do; this bounds each caller inside
it. Behind a router a tenant's requests are counted on the node that
answers them, the router enforcing nothing and passing the 429 and its
headers through as it passes every answer; a marked read a tenant's
read replica answers is counted there, in that copy's database.

*The default.* 1 000 a second is ten times `--call-rate`'s 100 (a call is
a write path, a request most often a read), and a bucket holds a second's
worth: a page opening with fifty requests, a search box asking at every
keystroke and a dozen live queries stay under a tenth of it, while a
caller in a loop gets a fiftieth of what one connection can ask. A service
asking more says so with `--request-rate 0`, or asks with the server's
token, which is never held.

*The table.* `--call-rate` kept its callers in one `Mutex`, asked only by
a call; asked by every scoped request, one lock would be a line every
core's requests pass through in turn. Both rates' tables are cut into
sixteen locks by the key (`rate::Table`), as the verified tokens' cache
is, a lock held for a lookup and a few floats. The key is built on the
stack -- the database's address, the issuer, a nul, the subject --
where `--call-rate`'s was a `String` formatted for each call. Each lock's
callers are one map, swept once a window of those who did not ask for
one, their buckets full again by then (`Held::take`'s `retain`, some tens
of microseconds over a few thousand entries, for the one request that
comes upon it). `--call-rate`'s were two generations a turn apart, every
caller's entry moved from the older map to the newer as it next called: a
hash, a free and the newer map grown again by doubling, for each caller
every second, which the first trace of a scoped request caught (+4 561
instructions in that one call, 2 915 of them the allocator's). The map
hashes with a 128-bit multiply fold under a seed `RandomState` draws for
each lock (`Seeded`): SipHash took some 290 instructions a 40-byte key,
more than the rest of the charge, and a key is a signed token's issuer and
subject, a lock holding a few thousand at most, so a seed no client sees is
defence enough against keys chosen to meet. A bucket's time is seconds
since its lock's map was made, an `f64`: one clock read and one
subtraction a charge, where `Instant`s took three, each a `timespec`'s
carry. `--request-rate`'s table holds 65 536 callers (some 100 bytes
each, 6.5 MB at the full), and past that lets a new caller through
unheld, where `--call-rate`'s 16 384 make a newcomer wait for the sweep:
a bound on by default must not turn a busy server's next user away for
the table's size, nor let a holder of many tokens fill it and refuse
everyone else, and a caller still counting is never forgotten, so none
resets its own bucket by flooding either.

*Out of scope.* Requests with no token, and those of the server's own
token, are not counted by address: there is no anonymous or per-IP limit.
A server that asks for no token answers whoever reaches it, and one that
asks has refused a client without one before any rate is asked (401, the
wait after a refused token slowing a guesser); limiting addresses is the
proxy's or the CDN's in front, which sees them before a connection is a
thread here. Only `/_mcp` holds the server's token's callers by address,
for its own tool calls.

*What it costs.* Instructions a request through `handle`, a row by id
(`make instr-compare EXAMPLE=rows MODES=scoped,token,get`, each build as
eight re-signed identities of seven runs, the mean of their medians): a
scoped token, one of 1 000 subjects in turn, 31 347 on main and 31 931
here, +584 (+1.9%, SE 73), where the first cut -- SipHash, `Instant`s and
two generations -- was +783; the server's token 26 247 and 26 292 (+44, SE
64) and a server asking none 24 664 and 24 607 (-57, SE 67), each inside
two standard errors: they pass `rate::admitted` by. The +584 is the
caller's key (some 110), the lock and its clock (some 90 and the
system's mutex), the hash and the probe, the bucket's floats. Under load,
eight keep-alive connections asking a server in the same process for
three seconds (`make request-rate-bench`, ABBA turns, each started under
a load of 3, none discarded), scoped requests went 57 544 a second at
the median of nine runs against main's 57 800 (-0.44%; head 56 743-57 948,
main 57 034-58 071, main's own runs 1.8% apart), p50 135.8 against 135.0
us and p99 253 against 249; the server's token 57 955 against 57 925 in
six runs each. Sixteen threads on eight cores there, every one busy: what
a request costs in CPU shows whole, and the half a percent is the +584
instructions, not threads waiting on one another at the locks.

## Retry-After in bodies

**Every 503 says when to come back, in its body too.** A move's or a
failover's 503 carried `Retry-After: 1` as a header alone, and the sync,
which reads bodies, came back after its own 250 and 500 ms where the node
had said a second. `Response::retry_after` writes `{"error",
"retry_after"}` beside the header, as `quota::refusal` writes a 429's: a
write to a frozen tenant or one the lease does not let this node take
(`handle_admitted`, the schema's apply), a held claim on a tenant closed
here, a standby router without its directory, the ceilings of connections
(node and router) and subscriptions (JSON now, plain text before), a node
going down (`Refused::response`, the 503 of a tenant opened during the
shutdown), `/_changes` over a database going, and a connection no thread
could be made for (node and router, [http.md](http.md#connections-without-a-thread)).
Both runners already waited
a 5xx's `retry_after`; two scenarios hold them to a write's 503s of a move
and a lapsed lease, waited a second and not cut short by `online`, and a
stream's 503 from a standby router. Every SDK's error carried a body's
`retry_after` for any status; the Go and .NET tests freeze a tenant on a
real node and read it. A page on another origin reads the router's own
answers -- a standby's 503, a 404, a 502 -- once the router has
`--http-cors` (below).

**Nothing that landed is told to come back.** A 503, and a 429 naming its
wait, say nothing ran: the JS and Python clients send such a request again
whatever it is ([js-client.md](js-client.md#retries)), as a proxy or a
retry policy may. A `/batch` holding a `compact` runs a statement at a
time, so its stop may come after statements landed, and two answers told
such a batch to come back: `answered_under_lease` turned a stop's 403 into
the move's 503 while the tenant was frozen -- `freeze_as` sets the flag
before it waits for the requests in flight, the batch among them -- and
`batch_stop` answered `--call-rate`'s 429 with its `Retry-After` beside
`completed`. Sent again whole, the first statements were made twice. A 403
is the 503 now only where nothing landed (`wrote`, the lapsed lease's
check), the stop answered as it stands otherwise: the freeze waits for the
request, and the move takes what it wrote. And `batch_stop` answers a 429
or a 503 stop after a statement was applied as a 409 naming no wait, its
words saying the statements from `at` on were not made and are to be sent
alone. A 409 rather than the 429 without its header, since a 429 is a
come-back to the retry policies that read one, a header or not (urllib3's
`status_forcelist`, `go-retryablehttp`'s default), where none resends a
409. `--call-rate` charges a scoped token's runs alone, and a scoped
token's batch is one block, put back whole: no stop of its has a landing
before it today, and the 409 is the rule's, for one that comes to. The
clients hold to the rule from their side too, never sending again a 429 or
a 503 whose body says `completed` is above 0. `lib.rs`'s
`a_frozen_tenant_tells_a_refusal_to_come_back_only_where_nothing_landed`
and `a_batch_stopped_after_a_landing_is_never_told_to_come_back` hold the
server to it.

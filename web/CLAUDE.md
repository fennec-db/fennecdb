# CLAUDE.md

The JS client's invariants -- the module and the client entry, Durable Objects and their alarms, live queries -- for code in `web/`. The rules every change meets, and where every other area's are, are in the root's [CLAUDE.md](../CLAUDE.md).

## Invariants

### The JS client and Cloudflare

**[One module and a client entry, by where the queries
run.](../docs/design/js-client.md#the-module-and-the-client)** `fennec-lite` and
`fennec-min` are test builds, never shipped; `web/client.js`
(`@fennecdb/web/client`) is `builder.js` and `http.js` alone
(`web/fennec.client.test.js`), and `FennecHttp` calls `fetch` on its own
(`#request`), never as its method. On a client reading near (`reads:
'nearest'`), `FennecHttp.live`'s streams and polls stay home and its runs
read near.

**[A Durable Object keeps a database as a file
would.](../docs/design/js-client.md#durable-objects)** `persist` writes an
image, then a row a flush (`storage.sql`), a new image and the old's drop
in one `transactionSync`; a failed flush stores nothing and rejects every
later `persist`. The example opens and restores inside
`blockConcurrencyWhile`.

**[An object's alarm keeps its
database.](../docs/design/js-client.md#alarms)** `schedule` sets one only when
something is due; `maintain` returns a failure as `error`, never a
rejection; every database let go goes through `#retire`, a released one
refusing `restore`.

**[A live query learns what changed from the change
ring.](../docs/design/js-client.md#live-queries)** `Query.reads` names each
collection read (`Lives`), `fennec_changes` those written.

**[A number from 2^53 on goes out as every
digit.](../docs/design/indexes.md#sorted)** Whatever sends values to a
server as JSON -- `FennecHttp`'s bodies, the sync layer's lines -- writes
them through `jsonOf`, never `JSON.stringify`, whose shortest digits the
server reads as another int: a request's body by `bodyOf`, which notes
them as it normalizes the parameters, and a sync line told by the
module's count of such ints read (`fennec_wide`) around the replica's run
of its write -- so a write marked `ran` sends no value that run did not
read -- the rest walked. The module's parameters go by `JSON.stringify`,
which it reads as JavaScript's doubles (`as_double`). `normalize` itself
notes nothing: the module's builder runs through it.

**[A client sends a request again only where it cannot have run or may
run twice, and bounds every request in time.](../docs/design/js-client.md#retries)**
`FennecHttp`'s requests go through `#ask`, a stream's head through
`#open`: a 503 or 429 naming its wait goes again after it, never sooner,
unless its body says statements landed (`completed`), a refused connection
too; only a read the builder made (`BUILT`), one sent near or a write under
a key goes again after a reset -- never the caller's `read: true` sent home,
a claim nothing checks. Nothing goes again after the client's own timeout,
the caller's abort or a refusal naming no wait. `timeout` bounds the wait
for the head and each between the body's pieces (`textOf`), a stream's its
head alone; the client makes no key. Python's `Client` and `AsyncClient`
keep the same rule (`_send`, `_post`, `_BUILT`).
Swift's, Kotlin's and Dart's `FennecRemote` keep it too (`ask`, `asking`,
`_ask`; the builder's reads a `built` copy, never `asRead()`'s), a task, a
coroutine or `withAbort` given up sending nothing again; Kotlin sends what
must go once with its length fixed, which the JVM's `HttpURLConnection`
never sends again on its own.

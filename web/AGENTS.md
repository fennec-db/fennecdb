# AGENTS.md

The JS client's invariants -- the module and the client entry, Durable Objects and their alarms, live queries -- for code in `web/`. The rules every change meets, and where every other area's are, are in the root's [AGENTS.md](../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

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

**[A seed in parts lands in one task.](../docs/design/js-client.md#seeds-in-parts)**
`FennecSync` holds a seed's parts as the text they came as (`#seedPart`),
never in the module, and lands them once the last came, a part at a time
with nothing awaited between the first write and the last (`#landParts`),
never in a block, whose writes the module's memory would keep -- but one
part into a collated collection (`#landSeed`), put back for the data it
lacks; several there fetch it all first. In a file (`openFile`), which
holds each statement as it lands, the shape's row says `LANDING` until the
cursor is written; an open finding it, or a write refused half way,
empties the shape before anything reads it (`#emptyShape`).
**[A page's IndexedDB calls share one connection, and let it go when
asked.](../docs/design/js-client.md#in-real-browsers)** `persist`,
`restore`, `putState` and `getState` reach the store through `inStore`, on
the connection `idb` keeps, never one of their own: left open, theirs
blocked a delete of the database, and closed after each call, the next
took 8 to 10 ms once the page sat idle. It closes on `versionchange`, and
the one a transaction failed on is let go (Firefox refuses its next
requests). A chunk follows its image only over the connection that stored
or read the image, open since (`stored`'s `db`, `inStore`'s `on`): past a
delete of the database or the site's data cleared, the next `persist`
stores an image, as it does after a `restore` that failed as it loaded,
which lets go of `stored` first: the database may be another, its journal
off or holding another's writes. A database's `persist`, `restore` and
`openFile` run one after another in call order (`inTurn`), each once the
one before has settled, refused or not: two at once read one chunk
number, and the second chunk was stored over the first; a retry inside
one runs in its turn (`persistNow`), never through `persist`, which would
wait for itself. A database `close`d is refused by each of them not yet
run, by the retry and by one that awaited before it touches the database
or while it loads or checks it (`alive`), naming the close: its image is
empty, and stored it replaced the record and its chunks; nor is an empty
image ever stored (`filled`). A `persist` or `openFile` made
while a `restore` was out -- till the promise it returned settles, let go
of beside its turn -- is refused when that restore was (`restoring`),
unless a later one before it loaded its record: the restore may have
emptied the database. `web/fennec.persist.test.js`,
`web/fennec.file.test.js` and `web/browser/` hold all seven. A change to
these calls or to `idb` runs `web/browser/` in Chrome, Firefox and Safari
before it lands -- every pull request's run has `browsers (safari)` --
and `make file-bench` in Chrome and Firefox against main's. After a trap
a close enters the module no more and throws nothing, its file let go of,
and `openFile` takes a name another database of this worker keeps from
one whose module stopped (`here`), refusing it from one that runs on
([js-client.md](../docs/design/js-client.md#persistence-in-the-page),
`web/fennec.file.test.js`).

**[A sealed chunk opens only after its own, and a replica keeps a store it
cannot restore.](../docs/design/js-client.md#sealed-chunks-in-a-chain)** A
sealed record's tag covers the name it is stored under, its image's
generation, its place and the tag of the record before it (`aad`, version
2, the image's `v`): a page going on from a store cut short never opens
with a chunk it cut, and what remains is a store cut at a record's edge. A
store sealed as version 1 (0.2.0's) restores as it was sealed and the next
`persist` stores an image over it; nothing is sealed so any more, and a
later version is refused by its number. A change to what a tag covers is a
new version beside the old, the old read as before: the page's stored data
is a frozen surface. A `FennecSync` seeds only a key holding nothing: a
store there that does not restore refuses its start, kept as it stands
with its unsent writes (`#loadPersist`), and a tab taking the lead from
one stores nothing there (`#unkept`, `status()`'s `unkept`), reading it
again at its next persist only where IndexedDB failed the read (`unread`);
one taking the lead from a store another tab wrote since it read it stores
an image first (`sameStore`). A replica stores its key only holding the
key's lock (`storeLock`), whatever its url, shapes or `leader`, claims the
store before it reads it (`claim`, a token under an array key) and takes
it over (`#takeStore`) before it leads. Each of its writes reads the token
in its own read/write transaction and stores nothing where it finds
another's or none (`taken`, `unkept`): IndexedDB runs a store's read/write
transactions one at a time, so this holds with no lock manager, where the
last replica started stores the key, and asks nothing of when a lock is
granted. `close` stores what was written before it, then lets go of the
lock, or after `LET_GO_MS`, past which no persist starts and `onError` is
told. A key's chunks are `key#` and nine digits (`isChunk`): nothing reads
or deletes another key's records. `unpersist` deletes one key's store,
under its lock (`locks`). `web/fennec.persist.test.js` and
`web/fennec.sync.test.js` hold each.

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
Go's `Client` (`ask`, `try`, `built`) and .NET's `FennecClient`
(`AskAsync`, `built`) keep it too, a GET -- the change stream's, a
subscription's head, the health check's -- among what may run twice, and
count a send their platform makes on its own as a try: Go's Transport
sends a request carrying an `Idempotency-Key` again itself, given its
body only through `try`'s `GetBody`, refused past the tries; .NET's
handler sends a GET again itself, three times at most, which `AskAsync`
then does not repeat (`ResponseEnded`).
Swift's, Kotlin's and Dart's `FennecRemote` keep it too (`ask`, `asking`,
`_ask`; the builder's reads a `built` copy, never `asRead()`'s), a task, a
coroutine or `withAbort` given up sending nothing again; Kotlin sends what
must go once with its length fixed, which the JVM's `HttpURLConnection`
never sends again on its own.

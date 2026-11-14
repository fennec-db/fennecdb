# The JS client and Cloudflare

The long form of the invariants [web/CLAUDE.md](../../web/CLAUDE.md) lists under *The JS client and Cloudflare*: the module and the client entry, live queries, Durable Objects and their alarms, and what the HTTP clients send again. Each section below is a paragraph moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists.

## The module and the client

**One module and a client entry, by where the queries run, and a test
build.** `@fennecdb/web` ships `fennec.wasm` (every index and the schema
check, 244.1 KB brotli) and `@fennecdb/web/client`, so a user has two
clear choices: queries on a server through the client entry and no
module, or a database in the page or a synced replica through the full
module. `fennec-lite.wasm` (no index, no schema check, 173 KB brotli; `make
wasm-lite`, as `make wasm FEATURES=language SCHEMA=0` now) was shipped
beside it and is a test build now -- built in CI, packed and released by
nothing -- because it holds the invariant above: a build without an index
opens a file that declares one, and `web/fennec.test.js` hands files
between it and the full module both ways. `fennec-min.wasm` (`make
wasm-min`, no feature at all) is the same kind of test build. The client entry is `web/client.js`: the builder (`builder.js`) and the HTTP client
(`http.js`) re-exported, the two modules `fennec.js` imports beside its
glue, persistence and sync. The entry's worth is that it cannot pull in
the engine, sync or storage, not the bytes: a `connect`-only app never
fetched the `.wasm` through `fennec.js` either (only `Fennec.open` does),
and bundling `connect` and a builder query's rows is 8.8 KB brotli
through the entry against 11.7 through `fennec.js` (5.7 against 8.2 when
the entry was made, 7.5 through `fennec.js` before `FennecHttp.live`), whose `Fennec` class a bundler cannot drop -- its static
block is a side effect. `web/fennec.client.test.js` walks the client's imports and fails
if they reach anything but those three files or hold the module's glue,
`FennecSync`, IndexedDB or files. `FennecHttp.live` is `Fennec.live`'s
contract over a server: a subscription to each collection the query reads
of a shape that holds nothing (`select=id&where=false`), every change a
write to it, the query run again on the server; the first rows wait for
every seed, a stream that ends is opened again and its seed runs the
query, and a text names what it reads or is refused -- so `useLiveQuery`
takes `connect()` as it takes `sync()`. A stream is a server thread
(45 to 57 KB resident) and a wake-up at every write -- a write's p50 went
0.26 -> 0.94 ms beside 60 streams and 3.1 beside 500 -- so `{ poll: ms }`
holds none: the query is sent with `If-None-Match` and the server answers
304 without running it while no collection it reads was written (`etag`,
`unchanged` in `fennec-http`, the change ring's
`changed_collections_since`, an inner `get`'s collections among those it
reads; a read of rows that expire or calling `now()` is never tagged), 54 us against 95 for a page
of 24 run. On a client reading near (`reads: 'nearest'`) the streams stay
home, unmarked, and every run -- the first rows too -- is a read near, sent
after the furthest of the client's position and the live query's own
(`AFTER`, kept apart: another client's change is no write of this one's,
which its other reads near would wait for): the furthest its streams named,
each event's `seq` of the history the head names (`Fennec-History`; the
seed's `seq` is at or past the head's `Fennec-Seq`), ordered as the client's
(`later`), so a stream opened again after a failover names the later term
and the old primary's moves nothing -- and the furthest its runs' answers
named, since a marked `/query` read's answer carries where the copy that
answered stood (`cfg.marked`, `At::of` under the read lock it reads under,
at the pin for a pinned read; every SDK takes a marked answer's position
for nothing of the client's): a run the router answered from home, past a
change whose event was still on its way, showed rows the next run sent
after the last event alone could have a replica behind answer without --
`[a,b,b2,c]` -> `[a,b,b2]` -> `[a,b,b2,c]` in review. A run waits a second
on the replica (`Fennec-Wait: 1000`), not the node's five: a replica has a
write once home's fsync covers it, 283 ms behind at most under `--sync 250`,
and a link later, and home answers in a region's round trip; a replica's 504
rests the copy at the router (above), so a replica cut off costs a live
query one second each `RESTING`, not five each change. The second assumes
a primary syncing at `--sync 250` or faster and links under about half a
second: at `--sync 1000` and up every near run misses it and is answered
home (`fallback` rising, the rows a round trip late), right but slower than
reading home, which `sharding-read-replicas.html` says; not an option of `live`, since a
deployment that slow reads home. Home's 504 -- the change lost in a
failover -- sets back whichever position the run was sent after and asks
near again after home's, home unmarked only where that fails too, since an
unmarked answer names nothing -- and so a run after that one may show rows
from before it, left so: it takes a failover that lost writes, whose lost
write the rows lose anyway. A head naming no history -- a server from
before heads named one, or a database never replicated -- sends the runs
home until every stream's head names one again (`bare`, by collection: set
for good, a stream reopened after an upgrade left the live query home):
term-0 positions never pass the client's. The rows are
never older than a change a stream told of or a run showed, and a stream
tells of every change home takes. The polls stay home: a tag is a change of
the copy that answered, and a replica holding that change of its history
(`Fennec-After`) judges it as home does up to where it stands -- behind,
`unchanged` reads a tag past its change as changed and runs the query, rows
older than those shown -- but nothing in a poll names a change home took
since, as a stream's events do, so a replica behind or cut off from home
would answer 304 to a write home holds. `web/fennec.replicas.test.js` holds
it to two routers, two nodes and a read replica that follows its primary
through a proxy parking what the primary sends (`parking`, which counts
what the routers ask the replica's node too), the tenant's `streams` quota
set so the replica's node counts a run held there (`usage.streams`): the
first rows and a change's run held on the replica, answered by it once
released (`fennec_router_replica_reads_total`'s `replica`), one never
released sent home after its second, and the changes after it home at once,
the replica asked once for three (`fallback`), the stream unmarked --
failing at once with the streams' position, its history or the mark taken
out, a stream marked, or the router's rest; runs through the home region's
router at a tenant whose one stream its live query holds answered, failing
with the slot taken first; and scripted tests: two streams' seeds and
changes, one behind another's, the client's own write further, a stream
reopened on the next term, the old primary's change ignored, home's 504
asked near again after home's position and home unmarked after a second, a
marked answer moving the live query's position and not the client's; a
replica behind home answering the run after a home answer -- failing with
answers' positions left out as the review's rows did; a head with no
history and a poll going home, unmarked. `useLiveQuery` passes the client
through and needs nothing. An app's bundle of `connect`, rows and `live`
went 29 684 -> 30 124 bytes, 9 728 -> 9 891 brotli; `http.js` 26 412 ->
29 298, 7 787 -> 8 586 brotli, which took the client the docs quote to 194
KB, 51 brotli, and the studio's first load to 66 KB gzipped.
`db.subscribe(collection, shape, onEvent, { onError, onState })`
keeps a shape over a server -- a seed, then each change -- opened again with
backoff and seeding again, stopped by a 401; `sseEvents` is exported with
it, the types beside (`ShapeFilter`, `ShapeEvent`, `SubscribeOptions`; a
sync shape was `Shape` already), and an app bundling `connect`, rows and
`live` went 7 905 -> 8 277 bytes brotli, `FennecHttp` one class a bundler
keeps whole. `db.batch([...], { idempotencyKey })` posts `/batch` -- a
builder query, a text or `[text, params]` a statement -- and a stopped one
throws `FennecError` with `at`, `status` and `completed`; `run` takes a key,
`withIdempotencyKey` makes a copy sharing `seq`, and every SDK has the same
(Go's `Error.At`, .NET's `FennecException.At`, Python's `idempotency_key=`,
the Swift, Kotlin and Dart remotes' `batch`). A replayed answer carries the
database's change as its `Fennec-Seq`, which holds the write. `Rows<P, {}>`
carries `facets?: Facets`: a `let` keeps its declared type, so a facet
asked after it was lost to the type. The client grew 6 -> 7.4 KB brotli in
an app's bundle. `FennecHttp` calls `fetch` on its
own (`#request`): as its method, a browser's `fetch` throws "Illegal
invocation", which Node's does not. `sync()` opens
the full `./fennec.wasm` unless given `wasm` or `local`. A replica's module
was shipped beside them for a while (text, sparse and sorted, no graph and
no schema check, 154.0 KB) and taken out: it left out too much for 27 KB
brotli -- no graph, so a `near` 2 to 40 times slower (0.27 -> 0.54 ms at
1 000 x 128, 0.68 -> 8.5 at 10 000 x 384, 0.98 -> 40 at 50 000 x 384), no
schema check, `create index @hnsw` refused. Without `vector`,
`near` over a field declaring `@hnsw` is `Database::near_stored`: every
vector read out of the store and measured as the full build's exact
search measures its arena (`vector::search_stored`: made a unit one as
`Arena::push` makes it, halved for f16, the same kernel, the page kept
through `best_first`, ties in id order -- the arena's when its nodes are
in id order), or over codes by `order_exactly` as the full build's exact
search over codes is, a bit index taken to hold its vectors whole while
fewer than `BIT_TRAIN` distinct ones are stored (`stored_hash`); its
tombstones, which count there, are not known here. Without `sparse`, a
sparse `near` takes the `exact` scan. `match` without `text` is refused:
BM25 needs the index's statistics. `web/fennec.test.js` holds the test
build's `near` to the full one's `near ... exact` over 9
declarations, filters and pages, row for row and score for score. The scan
costs the modules without the graph 2.2 KB brotli; in the test build at
10 000 x 128 it takes 2.4 ms against 0.22 through a graph and 0.49 for the
full module's exact scan, at 50 000 x 384 29, 0.75 and 3.9 (`make
wasm-exact-speed`): each
vector is read out of its document and made a unit one per query, four
lengths summed side by side (`flat_sqs4`; 4.3 -> 3.3 ms), where the arena
holds them made. An f16 field's vector goes into the arena as its record
holds it (`VectorIndex::as_held` in `place`): placed as written, cosine
made other unit vectors than an open did, and `near ... exact` scored
differently after a reopen.

## Persistence in the page

Not an invariant: the part of CLAUDE.md's *Architecture* on how the browser client keeps a database, moved here word for word, measurements included; CLAUDE.md keeps the journal's and the two-copy image's rules and says that the image and chunks can be sealed.

`persist`/`restore` keep a database in IndexedDB as a file would hold it: an
image, then the writes since as chunks, from the journal `fennec_journal` starts
and `fennec_drain` empties (off until asked for -- a page that never drains would
hold every write). One row persists in 0.18 ms over 32 MB in Chrome, against 92
ms for the image. `openFile` (a dedicated worker) keeps the same bytes as a file
of the origin private file system -- the file `fennec-server` keeps, so each opens
the other's -- and `run` appends each statement's writes and flushes before it
answers: 0.56 ms a row, statement included, and it opens in 20 ms against
IndexedDB's 47 (`make file-bench`; Safari 0.98 and 0.36 ms). A new image
(`compact`, or appended writes past half the image and 64 KB) goes into
`<name>~` and is flushed before the file is written over, so a crash leaves
one of the two whole; `openFile` takes the copy when its image is whole.
`persist(db, key, { cryptoKey })` seals the image and each chunk with
AES-GCM (WebCrypto), a tag over the key, the image's random generation and
the chunk's number, so one moved, dropped from the middle or kept from an
image before is refused; a row persists in 0.16 ms either way, the 32 MB
image in 129 ms against 76. The OPFS file stays plain: it is `fennec-server`'s.

### In real browsers

`web/fennec.persist.test.js` and `web/fennec.file.test.js` hold these calls
to in-memory stand-ins, Node having no IndexedDB and no origin private file
system. `web/browser/` (`make browser-test`, the `browsers` jobs in CI) runs
them against each engine's own -- Chrome and Firefox headless through
puppeteer-core, Safari through safaridriver -- in a page that runs its suite
itself (`persist.js`) and loads itself again where a test reloads it, its
state kept between loads by the server that opened it (`run.mjs`). It holds
what it races rather than timing a reload: a persist's transaction is held
open by a read after a read until the page is gone (`holdNextWrite`), and
the worker keeping a file (`file-worker.js`) counts every write, truncate
and flush `openFile` asks of the file system and stops at the one it is
armed for -- a write once half its bytes are in -- busy until the reload
ends it, as a worker the page left in the middle of a flush is. Stopped on
`Atomics.wait` instead, its file was still held 3 s after the reload in
Safari 27, which ends a running worker at once. A reload in the middle of a chunk's or a new image's
transaction loses no acknowledged write, and the image keeps its chunks or
the new one stands alone; a reload at each of a rewrite's eight steps, and
in the middle of an append, keeps every answered write and empties the
copy. Chrome is held to a quota over the DevTools protocol
(`Storage.overrideQuotaForOrigin`) and Firefox by a preference (a group of
origins gets a fifth of `dom.quotaManager.temporaryStorage.fixedLimit`, never
under 10 MiB); Safari has neither, and its two quota tests are skipped by
name. The page is told the quota, not left to read it: Chrome's
`navigator.storage.estimate()` names 10 GiB whatever it enforces.

What it found, fixed with it:

- A page reloaded while its worker was busy found its file held: Chrome
  stops a busy worker only 2 s after its page is gone, its sync access
  handles open till then, and `openFile` was refused as open elsewhere for
  1.98 to 2.01 s (Chrome 157). It now asks again for 3 s (`HELD_MS`) before
  it refuses, a file this worker itself holds refused at once (`holding`).
  Firefox ends the worker at once: the file opens 50 to 85 ms after the
  reload.
- Every `persist`, `restore`, `putState` and `getState` opened a connection
  and left it open until the collector came, and a delete of the database
  -- or another page's upgrade of it -- waited on all of them
  (`indexedDB.deleteDatabase` was blocked in Chrome). Each closes its own
  once its transaction is done.
- Past the quota, Chrome aborts the transaction at its commit, by `abort`
  alone: `putState`, waiting on `error` and `complete`, never answered.
  Firefox refuses the request, whose `error` reaches the transaction
  before the transaction holds it: `persist`'s refusal was `null`. All four
  read the request's error and the abort's (`finished`).
- A file past the quota: Chrome's file system throws QuotaExceededError,
  Firefox's writes nothing and says so by its count, which the file now
  names as the origin out of room. Either way the write is refused, every
  later one too, and the file opened again holds every answered write.

## Durable Objects

**A Durable Object keeps a database as a file would** (`integrations/cloudflare`,
`@fennecdb/cloudflare`). A Worker imports a `.wasm` compiled, and `Fennec.open`
takes the module so -- it read `.instance.exports` off what `instantiate`
gives a module, and the engine could not start in a Worker at all.
`persist` writes an image into `ctx.storage` and then only the writes since
(`journal`/`drain`), until they outgrow half the image and 64 KiB. An object
is billed by the row written ($1 a million), and a SQLite-backed one -- the
only kind a new account makes -- has `storage.sql`: there a generation is a
table, `<key>:<gen>` (`id INTEGER PRIMARY KEY, image, data`: an index is a
row written more), the image's rows under the 2 MB a row takes (`ROW`),
then a row a flush of the writes, and nothing beside them says where the
database stands, so a write is one row. A new image is written and the
tables before dropped in one `transactionSync`: workerd counts a table made
as two rows, one dropped as none and a `DELETE` a row each. The key-value
layout -- values under 128 KiB, a record naming the whole generation
written last -- is kept for an object without `sql`, and read through
it alone: a database an earlier version kept in values on a SQLite-backed
object is moved by restoring through the key-value methods and persisting
(README), not by code. Before, a single-row write was a value, the record
and the example's `setAlarm` (a row too), 3.4 rows; a checkpoint of
10 000 x 128 (5.7 MB) 101 rows and of 50 000 x 128 (27.6 MB) 479, its
128 KiB values and a delete each of the generation before. Now one row, 5
and 16 (`make cloudflare-bench`'s rows.mjs, every storage call counted
under `wrangler dev`). A flush goes out at once unless the one before is
not yet durable (`storage.sync()`); the writes that land meanwhile -- a
Durable Object takes the next request in while one waits -- go out after
it as one: 0.52 rows a write from 8 writers at once, 0.15 from 32, every
`persist` resolving once its own flush is durable and a read's once what
it may have read is. Waited out with a timer, a turn of the event loop for
requests to land, it cost a lone write 3.2 ms p50 against 1.4; this way
2.0-2.3 against the 2.4-3.1 before. A flush that fails leaves the storage
as it was and every `persist` on that database rejecting after: the
example lets it go and the next request restores, where it answered 400
and kept the write in memory for the next image to store. The example
opens and restores inside `blockConcurrencyWhile` (a cold object sent
requests at once could open an engine each, appending under the same
indices; `wrangler dev` never showed it in a burst of 128),
and keeps its database from its alarm (below); `checkpoint` writes nothing
until the writes since reach `{min}` (64 KiB), and then a new image: a restore takes the graphs in it, where the
vectors written after the last image are linked one after another in the
browser module's one thread -- which no longer costs a start much: the
first `near` over 10 000 x 128 after a restart 39 ms as written, 47 after
a checkpoint, 50 000 175 and 157, where it was 792 and 1 455 against 36
and 84 when first measured (`make cloudflare-bench`, under `wrangler dev`,
which runs from the example Worker's own `--config`: below the repository
it took the site's `wrangler.jsonc`). A restore copies each row or value
into the allocation the module keeps the image in as it reads it
(`Fennec.loadParts`, JS only): read whole into a `Map`, joined, and copied
in, a 27.6 MB image held 60 MB of JavaScript beside the module's 42 at
the peak, now 4 (memory.mjs, in Node), and the start of 50 000 rows went
186-196 -> 175 ms. A Worker's isolate has 128 MB with the module's memory,
which grows and never gives back. A checkpoint writes the image a mebibyte
at a time (`fennec_snapshot_chunks`, `snapshotChunks`), each into storage
before the next is made: built into one `Vec` the image doubled as it grew,
old and new side by side, and `boxed` copied it again -- 30 000 x 128 went
from 60 to 128 MB, and goes to 70. Made all at once, the chunks stood
beside the rows until the first was stored: 50 000 x 128 at 73 MB went to
109. So each is made as it is taken (`Chunks::take`), out of the stores'
own bytes held rather than copied (`ImageOut::write_kept`, `Kept`: the
image a load kept, and each segment -- an `Arc` in the module too now,
copied by a write while an image holds it, so the image is the database
as it stood when begun): 83 MB, and a checkpoint of 10 000 x 768 holds no
more than the rows, for 0.6 KB brotli; puts and the image as fast. About
60 000 rows of 128 dimensions or 12 000 of 768 fit, 97 and 102 MB at the
most; `wrangler dev` does not hold a Worker to it.

## Alarms

**An object's alarm keeps its database** (`maintain`, `schedule`, `due`
in `integrations/cloudflare`). A server sweeps `@ttl`, compacts and keeps
its graphs on threads; an object has one alarm, a row written each time
it is set and a wake-up -- a restore, if it was evicted -- each time it
rings. The example set one after every first write and checkpointed past
64 KiB of writes, and nothing deleted a row past its time: read past, kept
for good. `schedule`, after a write, sets the alarm only once something is
due -- a row that will expire, an image owed -- `quiet` (5 s) after the
write at the soonest, leaving one set no later than `every` (60 s) after
that, and asks nothing where one rings that soon (`due` is 8 us a write in
the module, 29 beside 100 000 rows that expire, a put 5); a write that
brings nothing due and every read set none, so an idle object costs
nothing. `maintain` deletes the rows `grace` (60 s, the server's `GRACE`,
so a reaper reading `expired()` as often sees each) past their time,
10 000 a call in statements of 1 000 -- `del c where expired() and f <= $1
order f limit $2`, which walks the `@ttl` index to the batch, so the module
took no export and not a byte (`Database::expired` and `sweep` are native
only): 0.46 ms a batch over 100 000 rows, an alarm's 10 000 holding the
object about 5 ms -- and sets the alarm again at once past the bound;
writes a new image once the writes since reach half the image and 64 KiB,
or half of what storage holds is dead and a mebibyte, which the writes'
size alone does not see (the module's dead record bytes less those it held
as the stored image was taken, `deadAtImage`: an image holds the live
records alone, and every write after lands in both), so an alarm woken to
sweep stores its deletes, one row, and no image -- under a mebibyte an
image of a churning 70 KB was three rows a sweep where the deletes were
one; and sets the alarm for the next row's time and grace, no sooner than
`every` after the sweep, or none, the alarm that rang forgotten either way
(kept, `schedule` took it for one still set and set none again). A failure
to set it -- `getAlarm`, read on every alarm that wakes an evicted object,
or `setAlarm` -- is `maintain`'s `error`, its work stored, never a
rejection: thrown, the example's handler let the database go as for a
storage failure (below). The names its statements write are the lexer's
(`\p{Alphabetic}`, then `\p{N}` too): ASCII alone, a `müşteri` was never
swept. A year past 9999 is written `10000-01-01T00:00:00Z` and one
before year 0 `-001-01-01T00:00:00Z`, four wide with its sign, which
`Date.parse` reads as `NaN`: the alarm was set at `NaN`, and a row of year
-1 past its time passed its whole collection over. Both are read in
ECMAScript's six-digit form; a time past any date JavaScript has is taken
as due now for a row past its time and passed over for a living one. Taken at
a quarter of the image, rewrites of 1 000 128-dim rows a request, a row
each, brought an 11 MB image, 8 rows, every 6; at half, where `persist` folds, the
images are as many as before, and a request folds only past the whole image
once `schedule` ran (`BACKSTOP`), so the alarm takes them and no request
waits. Under `wrangler dev` (rows.mjs, every storage call counted, in ABBA
turns against main): 30 bursts of 20 single-row writes over 10 000 x 128,
a second apart, 600 rows against 651-652; 12 bursts of 500 rows 19 against
100; 20 000 rows past their time deleted in two alarms, a row each. The
module never gives memory back and holds every record version written
since its open (20 000 x 128 restored held 18 MB, 74 after its titles were
set thrice and its vectors once): once its dead record bytes reach half
of what it holds, between 4 and 16 MiB, `maintain` makes sure storage holds
an image without them and answers `reload`, once a database, and the
example restores a new module -- 50 000 x 128, three in five expired, held
42.7 MB through two sweeps, 51.6 at the image the third owed and 19.2
restored (memory.mjs, in Node). Every database the example lets go -- a
reload, writes not stored, a `maintain` that failed -- goes one way
(`#retire`): calls arriving wait while those holding it finish, a write
on its way to storage among them, then it is `release`d -- nothing of it
stored after, resolved once its flush under way has settled -- closed, and
another restored. `release` remembers the database (a `WeakSet`), not only
its state: one never kept, or restored from empty storage, had none to
mark, and a `persist` after made one afresh and wrote an image over the
generation stored; `restore` into a released one is refused, where it
made a state anew. Let go of at once, as the alarm's handler first did when
`maintain` threw with storage sound, it lost answered writes: a request's
flush waited to be durable and another's waited behind it, the next
request restored a second database and wrote id 3, the first then stored
the waiting write as id 3 too, and the next restore read the first's. And
a refused `persist`, `checkpoint` or `maintain` on a failed or released
database answers only once no flush of it is under way, so a caller that
drops it and restores -- the README's alarm, a request's write not stored
-- reads all it stored and is alone: refused at once, it answered while a KV
flush still put its pieces, which would land after that restore. `example.test.js` drives the
example's class in Node over the storage stand-in (`module.registerHooks`
giving it `cloudflare:workers` and the compiled `.wasm`), holding a flush
at `sync` to put the alarm between two requests' writes; it fails on the
code before, and with the wait for the holders taken out. A `compact` in the module rebuilds every
graph holding tombstones and builds its image whole, and gives nothing back.
A restore reads its generation's sums to their end: left at the first row,
the statement held the table and a new image's `DROP TABLE` was refused
under node:sqlite. The first `near` after a restart, a lone write's p50
(2.00-2.07 ms against 1.96-2.35) and a restore's memory moved by noise
alone. `make cloudflare-test` holds the rules to the fakes -- storage
failing after every operation of a sweep's flush and of its image included
-- and the example under `wrangler dev`, on the port it says it bound
(`--port 0`, `Ready on`; dev.mjs, which rows.mjs and coldstart.mjs start
it through too): an idle object setting no alarm, 350 rows swept over four
alarms and kept across a restart, an image by the rule and none without
it, a reload. wrangler is started with no colour and its line read past
the escapes besides -- under FORCE_COLOR the address sat between them,
and the harness waited for good -- a start that ends before the line
fails, and an `after` hook ends any wrangler a test cut short by its
timeout left, which held the test process open.

## Live queries

**A live query learns what a write changed from the change ring.**
`Fennec.live` and `FennecSync.live` share one `Lives` (`web/fennec.js`): a
query's rows now, and again once a collection it reads is written --
`Query.reads` names them, each `lookup`'s and inner `in` query's too, `null`
for a `raw` holding a `get`, and a text names them with `{collections}` or
runs after every write. Which collections were written is
`fennec_changes(since)`, `Database::changed_collections_since`: a block's
writes reach the ring as it lands, so one put back names nothing, a schema
change names its collection, and a drop answers `None` -- everything --
since a dropped collection has no name left (a name kept for each would be
kept for good). A statement's answer does not carry it: asked once in a
microtask after the task that wrote (a replica: at the next frame, a seed
landing in chunks), it costs a write nothing -- a write no live query reads
about 2 us in Node, the module 33 bytes, `make wasm-speed`'s `put` as it
was. A `load` (`restore`, `openFile`) runs every live query, since the
image's counter may be the cursor's. The client grew 1.9 KB gzip.

## The server's rows

Not an invariant, but what a change to how a replica writes rows keeps: a
sync's replica writes the rows its streams hand it -- a seed's, a change's,
and its own rows again when the server's schema changed (`#putRows` in
`web/fennec.js`) -- as `put <c> $1`, a chunk of the rows the one parameter,
where it wrote the builder's `insert`, a text of `{f: $n, ...}` documents, a
parameter a value. Both are `put`, which writes over a held id -- the
builder's `.insert()` sends `put` -- so a row that stayed keeps its vector's
node, and what `#reconcile` and `#deleteLocal` do before the write, taking
out the temporary rows a key matched and the ids a seed lacks, is as it was;
a replica's collection declares a `@unique` field `@hash` either way
(`schemaDDL`). The module reads a parameter's documents as a body's
(`Database::document_of`): each member through `doc_field`, made its
field's type by `set_field`, as `build_docs` makes a literal document's, and
a list of numbers below the top of a parameter as written (`json::parse_value_at`),
so the rows and the refusals are the same. `web/fennec.test.js` writes the
rows a stream's text parses to both ways -- a whole number past 2^53, a json
field's numbers and a wide one in it, a time, a point, a sparse vector, an
`[int]` holding 16777217, a `[float]` holding 0.1, bytes, a vector holding
`-0` and a subnormal, nulls, a field left out, `@unique`, `@hnsw` -- holds
the two images to the byte, and the refusals of a held unique value, a
vector of another dimension, an int past 64 bits, a field not there and a
byte past 255 to their words.

A whole number past 2^53 is the double `JSON.parse` read from the stream's
line; `JSON.stringify` writes its shortest digits (2^60 as
1152921504606847000), and the module reads such an int, at any depth of a
parameter, as the double JavaScript held (`as_double` walks lists and
objects, [indexes.md](indexes.md#sorted)), so the replica holds 2^60
either way, as `web/fennec.sync.test.js` checks with every digit of it in a
filter, a seed's and a change's, beside a vector's `f32`s read back as the
server holds them.

A collection with a `vector<N>` or `bytes` field keeps the builder's text
(`APART`). The text hands a list of numbers that is a parameter of its own
to the module as `f32`s (`vectorsApart`); inside `$1` a vector is digits the
module reads a number at a time and rounds to an `f32` -- the same `f32`s,
as JavaScript's and Rust's roundings of a double agree -- at a cost: rows of
a 32-dim vector took 1.7x as long that way (a 300 000-row seed's 743 ms ->
1 296), 128-dim 2.4x and 512 bytes a row 1.6x, where 4 and 8 dims went
faster as a parameter and 16 as fast. `[int]` and `[float]` go by the parameter: the text
hands them apart too, but a list of numbers keeps them as written, so the
module makes a marked one the list of its numbers again and asks for an
unmarked one as JSON (`Refused::Exact`), and rows of a 16-element `[int]`
took 2.3x as long by the text, of a `[float]` 2.2x.

A chunk is 500 rows, as the builder's text's was. It bounds what a statement
holds at once, in the page's heap and in the module's memory, which never
shrinks. Of 300 000 rows of a text, a float and a json field, 1 000 a
statement was as fast as 500 and left the module at 38 MB against 29; 2 000
to 10 000 were up to 2.3% slower, and the page's peak 27 to 61 MB higher.

Measured in Node 26 on an M1 at a load of 2 of its 8 cores, a process
a run, the median of three: a seed handed to `sync()` as one event of a
stand-in `fetch`, from the event to `ready()`, `web/fennec.js` of `main`
against the parameter's:

| rows of | 10 000 | 100 000 | 300 000 | peak RSS at 300 000 | the module at 300 000 |
|---|---|---|---|---|---|
| a text, a float and a json field | 46 -> 29 ms | 295 -> 192 | 825 -> 550 | 373 -> 379 MB | 29 -> 29 MB |
| a text, a float, an int, a time, a bool | 59 -> 39 | 400 -> 265 | 1 135 -> 767 | 460 -> 465 | 46 -> 29 |
| a text and a point | 55 -> 27 | 357 -> 163 | 994 -> 468 | 368 -> 379 | 28 -> 29 |
| a text and a 32-dim vector (the text kept) | 58 -> 58 | 390 -> 386 | 1 133 -> 1 122 | 886 -> 886 | 78 -> 78 |

The peak RSS rose 5 to 11 MB at 300 000 rows -- the page's heap, which it
gives back; the module's memory, which it keeps, was within 1 MB of
`main`'s or 17 MB below it -- and went down below that, 217 -> 188 MB and
238 -> 217 at 100 000. The write alone, apart from the event's parse and
the rest of a seed, went 1.7x as fast for the first row type (748 -> 429
ms at 300 000) and 2.6x for points (917 -> 353).

## Retries

**A client sends a request again only where it cannot have run or may run
twice, and bounds every request in time.** `FennecHttp` (`web/http.js`,
`#ask`) and Python's `Client` and `AsyncClient`
(`integrations/python/fennecdb/__init__.py`, `_send` and `_post`) keep
the rule the router keeps toward its nodes
([router.md](router.md#pooled-connections)), which is Go `net/http`'s.
Swift's, Kotlin's and Dart's `FennecRemote` keep it too (`ask`, `asking`,
`_ask`), over HTTP stacks that send again on their own (below).

*Cannot have run:* an answer that says nothing ran -- a 503, every one of
which names its wait ([quotas.md](quotas.md#retry-after-in-bodies)), the
`Fennec-Unserved` one a server answers at its ceiling among them, and a
tenant's 429 where waiting makes room (`retry_after_ms`); and a connection
refused, which sent nothing (Node's `fetch` says so as `cause.code`
`ECONNREFUSED`; a browser's says nothing it can be told by, and its
refusals go as resets do; `asyncio.open_connection`, refused at each of a
host's addresses, joins their words in one `OSError` -- "Multiple
exceptions: ..." -- which `_refused` takes for a refusal where every error
in it is one). Python also takes a request it could not hand the system
whole as unsent, as the router judges a write by its `Stage`:
`conn.request` writes a bytes body with the head in one `sendall`, which
fails only before the last byte. Any such request goes again, after the
wait the server names -- `retry_after_ms` before `Retry-After`, whole
seconds, before the body's `retry_after`, which a page whose CORS exposes
no header still reads -- and never sooner. Never a 429 or a 503 whose body
says statements of a batch were made (`completed` above 0): a `/batch`
holding a `compact` runs a statement at a time, and sent again whole its
first statements were made twice. The server answers no stop after a
landing so ([quotas.md](quotas.md#retry-after-in-bodies)); the clients
hold to it whatever they are told.

*May run twice:* a read the client knows to be one, and a write under an
`Idempotency-Key`, which the server answers as the first try was, a repeat
while the first is under way waiting for it
([http.md](http.md#idempotency-keys)). A read the client knows: one the
builder made, marked by the builder itself apart from the caller's `read`
-- `BUILT` in JS's `READ` and in a batch of builder queries alone, Python's
`_BUILT`, the `read` its builder hands `Client` and `AsyncClient` -- one
sent near (`Fennec-Read: replica`), which every server runs read-only
([read-replicas.md](read-replicas.md#marked-reads)), and Python's `GET`s of
the change stream. The caller's `{ read: true }` (`read=True`) sent home is
none: a claim nothing checks, and `db.call("history", read=True)` stops
being a read once the statement is made to log a row, which sent again it
logs twice. It keeps what it meant -- a text a client reading near sends
near -- and grants no resend. These alone go again after a reset or a
dropped connection, which a write may have landed before; anything else's
error is the caller's. A refusal naming no wait -- a quota waiting makes no
room for, any 4xx, a 500, `outcome: "unknown"` among them -- never goes
again, nor a request the caller's `signal` gave up, nor one past the
client's own timeout (below).

**So the rule sends nothing again that the server could make twice.** A
request goes again only where the server said nothing ran, where nothing
reached it, or where running it again ends as the first run did: a read,
or a write whose key the server holds -- `--idempotency-ttl`, a day, after
which a key is gone and a write under it is made anew. On by default: the
project is alpha, and no caller relies on a 503 reaching it. `retry:
false` (`retry=False`) turns it off; the tests of the error's shape do
(`web/fennec.test.js`).

The bounds are three tries and 10 s of waits between them unless given
(`{ retry: { attempts, maxWait } }`, `Retry(attempts, max_wait)`): the
server's wait, or 250 ms doubling where it names none, 30% jitter on top
so a server back up is not met by every client at once -- the sync
layer's backoff ([the sync's quotas](quotas.md#quotas-in-the-sync)) --
the jitter cut to what is left of the ceiling. A wait past what is left is
not waited: the refusal is thrown at once with its `retryAfter`, the
caller's to schedule. Past either bound the last answer or error is
thrown as it came.

**The client makes no key.** A caller that wants a write sent again after
a reset passes its own (`idempotencyKey`, `idempotency_key`), one it can
send again after a restart too. The first version made one where asked
(`{ retry: { idempotencyKeys: true } }`, `Retry(idempotency_keys=True)`),
off by default since a key is not free -- a row of `_idempotency` written
in the write's block and kept a day, 16 200 single puts a second against
18 300 without. Dropped: a key the client makes dies with its process, so
the write a caller most needs made once -- the one it sends again after a
crash -- was never covered; it could not tell a text that writes from one
that reads without the parser, nor a `compact`, which the server refuses a
key (400), but by its first word; and it cost the client entry 112 bytes
brotli.

**The timeout bounds the wait for an answer's head, and each wait between
the pieces of its body** -- not the whole: 30 s unless given (`{ timeout
}` in ms, Python's `timeout` in seconds as before), a held write's
`Fennec-Wait` on top of the head's. Bounded whole, as the first version
had it, a read longer than the timeout failed however steadily it came,
and was run three times; a `compact` or a `create index` past it threw
`TimeoutError` while it ran on to its end. Nothing goes again after the
client's own timeout, a read neither: past it the server may still be
running the request, and three tries of a read it took 30 s over ran it
three times. A statement that runs long before it answers is sent through
a client given a longer `timeout`. A server sending a byte at a time holds
a request only while it keeps sending: one that stops is given up the
timeout after its last piece.

`fetch` bounds nothing of its own. `FennecHttp` aborts its fetch, the
body's read included, with a `DOMException` named `TimeoutError`
(`bounded`'s `late`), as `AbortSignal.timeout` does -- which cannot be
stopped at a stream's head, nor started again at a piece of a body:
`bounded`'s clock starts again (`tick`) as `textOf` reads each piece through
the body's reader, where `res.text()` hands nothing over until the last. A
stream's (`live`, `subscribe`, `#open`) is timed to its head alone, its
body running as long as it is read; `AbortSignal.any`, to join the
caller's signal, is Safari 17.4's, so `bounded` joins them by hand and
lets the caller's go at the end -- a live query opens one a stream, and
each reconnect would leave a listener behind. Python's `Client` gives the
connect and each wait for the head what is left of the head's deadline
(`_bound`), then the timeout to each wait of the body, which `read` meets
a piece at a time; a `settimeout`, a system call (0.3 us), is made only
where the one set is a millisecond off. `changes`, `consumers`,
`move_consumer` and `forget_consumer` went through `urlopen`, a connection
each; they go over the thread's kept connection now, bounded and retried
alike, a `PATCH` and a `DELETE` as writes. `AsyncClient` bounds the
connect, the head and a body the loop holds whole with it by one
`wait_for`, as it bounded its whole exchange before, and a body still to
come by a timer the loop holds (`_Idle`), started again at each piece,
which lets the connection go as it fires: `wait_for` made a task a wait
before 3.12.

`AsyncClient` used to send any request once more over a fresh connection
when its kept one failed, which made a write the server had read and
answered twice; it now lets go of a kept stream the loop found ended
(`_gone`) before a request goes, as `Client` asks its socket (`_closed`),
and sends again by the rule alone. Asking the socket too was 1 us a
request more, for a loop blocked past the server's close alone: the loop
reads a close as it comes whenever it waits.

Streams and polls keep their own loops and take the server's wait as a
floor: a stream opened again waits `max(backoff, retryAfter)`, and a
poll's round goes through `#ask`, sent again after a reset where its query
is a builder's.

**Go's and .NET's clients keep the rule** (`integrations/go/retry.go`,
`ask` and `try`; `integrations/dotnet/FennecDb/Retry.cs`, `AskAsync`), by
the same bounds: `WithRetry(fennecdb.Retry{Attempts, MaxWait})` and
`WithoutRetry()`, `FennecClientOptions.Retry` (`new Retry { Attempts,
MaxWait }`) and `Retry = null`. The builder marks its reads itself -- Go's
`built`, set by `sendRead`, .NET's `built` from `RunReadAsync` -- never by
`Read()` or `AsRead()`, the caller's claim. A GET -- `Changes`, a
subscription's head, `Health` -- is a read by its route, and may run twice
as Python's `changes` may. A refusal is told by the system's error: Go's
`syscall.ECONNREFUSED`, Windows' 10061 its own, and .NET's
`SocketError.ConnectionRefused` anywhere in the exception's chain. Each try
has a context or token of its own, made from the caller's: the head is
bounded by the timeout (a held wait on top), each read of the body by the
timeout again -- Go's `gaps`, its timer `Reset` at each read; .NET's
`BodyAsync`, `CancelAfter` at each read -- and a stream's (`Subscribe`,
`SubscribeAsync`) by its head alone, which had no bound but the caller's.
The client's own timeout is told from the caller's end by what ended the
try: Go's `errLate`, which `errors.Is` takes for `context.DeadlineExceeded`
as it took the context's before; .NET's a `TaskCanceledException` around a
`TimeoutException`, as `HttpClient.Timeout` throws it.

**A send the platform makes on its own is a try.** Go's `net/http`
Transport sends a request again itself on a pooled connection that ended
under it: any request where nothing was written, and, written whole, one it
takes for idempotent -- a GET, or one carrying an `Idempotency-Key` whose
body it can take again (`GetBody`, which `http.NewRequest` sets for a
`bytes.Reader`): the router's rule, which is Go's. Uncounted, and under
`WithoutRetry` too, a keyed write went out as often as its pooled
connections died under it, and with the client's loop on top up to three
times that. `try` gives the Transport the body again only through a
`GetBody` of its own, which counts the send as a try and refuses one past
the tries (`errSpent`); a body not yet read when the connection ended --
nothing written -- goes without it, sending nothing that ran. A GET has no
body to hold it by: the Transport may still send one once more for each
pooled connection that died under it, a read, which `WithoutRetry` does
not stop. .NET's `SocketsHttpHandler` sends a request without content again
itself, any method, where the server closed the connection before a byte
of the answer (`_canRetry`), three times at most
(`MaxConnectionFailureRetries`), a fresh connection or a pooled one; never
one with content, which every POST of the client has (`ByteArrayContent`).
A GET whose connection the server closed so has gone out four times when
`AskAsync` sees it (`HttpRequestError.ResponseEnded` before its head), and
goes no more: sent again by the loop, it went out up to twelve times. The
tests run the clients against a stub that answers its n-th request by a
script over real connections -- a reset with linger 0, a close, a refused
port, a head that never comes, a body in pieces -- each on port 0
(`retry_test.go`, `RetryTests.cs`): `TestTheTransportsOwnResendIsATry`
fails with `GetBody` left uncounted, the keyed write sent twice under
`WithoutRetry`, and `AGetTheHandlerSentAgainIsNotSentAgainHere` counts the
handler's four sends, a fifth without the `ResponseEnded` check.

Their cost, measured against a server answering at once in-process, five
rounds alternated, begun under a load of 1.8 on 8 cores (medians): Go's
builder read 41.56 -> 41.60 us, `Exec` 39.84 -> 39.89 us and a 256 KB
answer 149.8 -> 149.3 us, within the noise, with 3 allocations a request
more (the try's context with its cause, its timer, `GetBody`), about 50 B;
.NET's builder read 49.2 -> 49.9 us and `ExecAsync` 39.9 -> 40.4 us, 1%,
240 and 310 B a request more (`AskAsync`'s and `BodyAsync`'s state, the
try's token source, the request's maker), and a 256 KB answer 1 475 ->
1 323 us and 2.11 -> 1.84 MB: a body of a length the server gave is read
into one array of it, where `ReadAsByteArrayAsync` copied it out of a
buffer. A Go program reading through the builder grew 8 904 466 ->
8 941 986 bytes, `FennecDb.dll` 159 744 -> 167 936.

Cost, measured against a server answering at once, rounds alternated:
`FennecHttp.run` of a builder read 5.00 -> 5.83 us over a `fetch`
answering in-process as the first version had it (an `AbortController`, a
timer and an `async` frame a request), and 5.26 -> 5.27 us from there with
the body read a piece at a time: its clock moved at each piece, the timer
set again only as it fires -- set again at each piece, 5.65 us, 7% more.
Python's `Client.query` 60.6 -> 61.6 us and `AsyncClient.query` 58.6 ->
60.0 us over a socket to another process as the first version had it, and
70.7 -> 71.0 us and 58.1 -> 57.9 us from there against a Python server
answering at once, within the noise: `AsyncClient` reads a body the loop
holds whole already within the head's bound, as before (`_exchange`), and
leaves one still to come to `_body` and its timer -- every body read so
cost 3.5 us, 6%. `web/http.js` grew 9 894 -> 11 792 bytes minified,
3 642 -> 4 411 brotli; an app bundling `connect` and a query's rows
through the client entry 34 137 -> 36 032 bytes, 11 052 -> 11 794 brotli
-- the first version's 11 788, of which the keys gone, the shorter
messages and `#send`'s own parse gone took back 116 bytes and the clock
between a body's pieces cost 122. The modules as served, which carry their
comments, 57 -> 58 KB brotli.

**Swift, Kotlin and Dart keep the rule over HTTP stacks of their
platforms', each measured for what it sends again on its own** against a
stub that reads a request whole and resets the connection, fresh or kept:

- URLSession sends a POST once: a reset or a connection closed before the
  answer is `networkConnectionLost`, a refusal `cannotConnectToHost`, the
  client's own timeout `timedOut`. A GET it sends again on its own over a
  kept connection, which `FennecRemote` sends none of. `timeoutInterval`
  bounds each wait for a piece of the answer -- its head, then each gap of
  its body -- which is the timeout as `FennecHttp` has it; a held write's
  wait goes on top of each, not of the head alone. A cancelled task throws
  `CancellationError`, and `Task.sleep` ends a wait at the cancel.
- The JVM's `HttpURLConnection` sends a POST whose body it buffered again
  on its own once the answer's head fails -- a reset, a connection closed
  before it -- over a fresh connection or a kept one alike
  (`sun.net.http.retryPost`, read once for the process): a keyless write
  the server ran was made twice. Streamed with its length fixed
  (`setFixedLengthStreamingMode`) it never is, nor by Android's
  `HttpURLConnection` (OkHttp), which sends again only a body it holds.
  Streamed, though, the JVM asks a kept connection whether the server
  closed it before using it, by a read with a 1 ms timeout
  (`HttpClient.available()`): over a kept connection to a stub, in the
  gradle:8-jdk17 container, 63 us a POST buffered, 2.4 ms streamed; 186 us
  either way with keep-alive off. So only what must go once is streamed --
  a write without a key, a text sent home, a batch -- and a builder read,
  one sent near or a write under a key goes buffered, which the JVM may
  send once more after a reset: what the rule lets them do anyway, an
  uncounted try. Android's OkHttp makes the same 1 ms check before every
  POST over a kept connection, streamed or not (`isHealthy`, read in its
  source, not measured here), so there streaming costs nothing. A request
  that must go once costs the JVM that millisecond over a kept connection
  where it was made twice before at a reset. The JVM-only alternative,
  `java.net.http`, is not on Android, bounds no gap of a body, and would be
  a second transport. Measured on OpenJDK 21 (macOS), 2 000 keyless
  inserts over one kept connection to a release `fennec-server`: 1 537 us
  median streamed, 60 us buffered, 192 us through `java.net.http` set to
  HTTP/1.1, which sends a POST, PUT or DELETE once after a reset but a GET
  again, and left at its default HTTP/2 asks every plain-http request to
  upgrade (h2c). Not taken: Android compiles the same sources, so it would
  be a transport interface with two implementations, change stream and
  cancel included, for one platform's keyless writes, which a key makes
  buffered. `readTimeout` bounds each read, `connectTimeout` the
  connect; a coroutine cancelled closes the connection and throws
  `CancellationException`, never an `IOException`, and `delay` ends a wait
  at it; a blocking call's thread interrupted is given up alike
  (`InterruptedIOException`).
- `dart:io`'s `HttpClient` sends nothing again on its own, and bounds
  nothing but the connect (`connectionTimeout`, said as a
  `SocketException` with no OS error, taken for the client's own timeout):
  `close().timeout` bounds the head, aborting the request, and
  `Stream.timeout` each gap of the body. A `Future` cannot be cancelled, so
  a copy holds what gives its requests up: `withAbort(future)` aborts the
  one under way and ends a wait between tries. A refusal is the
  platform's ECONNREFUSED: 61 on Apple's, 111 on Linux and Android, 10061
  on Windows. The bounds are a `RetryPolicy`, not a `Retry`:
  `package:test` exports a `Retry` of its own, and a test importing both
  named neither.

The bounds are `FennecHttp`'s -- three tries, 10 s of waits, a 30 s
timeout -- Swift's in seconds (`FennecRemote.Retry(attempts:maxWait:)`),
Kotlin's in milliseconds (`FennecRemote.Retry(attempts, maxWait)`), Dart's
`Duration`s, given to `Fennec.connect` with `timeout`; `retry: nil`
(`null`) sends nothing again. Swift's and Kotlin's timeout was 60 s a
read before, Kotlin's connect 10 s, and Dart's requests had no bound but
the connect's. The builder's reads go through a copy of their own
(`built`), never `asRead()`'s, a claim nothing checks at home. Dart's two
timers cost nothing measured: a POST and its answer over a kept
connection to a server in the same process, rounds alternated, a median
of 63.1 us unbounded and 63.3-64.1 us bounded, within the noise. Swift's
loop adds no I/O, and URLSession bounded each wait before too, by 60 s.

Each SDK's tests (`RetryTests.swift`, `RetryTest.kt`, `retry_test.dart`)
run against a scripted server on loopback, port 0. A refusal is made of a
port held as the local end of a live connection: a connect to it is
refused, and once the client says it is waiting (`pausing`, the tests'
probe) a listener binds beside it (`SO_REUSEPORT`), the port never free
in between. A socket bound but not listening will not do: macOS answers a
connect to it with nothing, and the connect waits out its timeout. Dart's
first connect is sent there by `IOOverrides`. Taking streaming mode away
fails Kotlin's keyless write and its `asRead()` text: the JVM sent each
twice.

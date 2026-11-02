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

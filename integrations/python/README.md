# fennecdb for Python

A client for fennec-server's HTTP endpoint (`fennec-server --http <address>`), and
vector stores for LangChain and LlamaIndex on top of it.

```sh
pip install "fennecdb[langchain]"      # or "fennecdb[llama-index]"
```

```python
from fennecdb import Client

db = Client("http://127.0.0.1:8080", token="...")
db.query("get articles select title near embed $1 limit 5", [[0.1, 0.2, 0.3]])
```

Or the query builder writes the statement -- every value a parameter,
every name checked -- the same text the JavaScript, Go and .NET builders
make of the same chain:

```python
from fennecdb import or_

docs = db.collection("articles")
rows = (docs.select("title")
            .where("year", ">=", 2024)
            .where(or_({"lang": "tr"}, {"tags": {"has": "rust"}}))
            .near("embed", [0.1, 0.2, 0.3], ef=64)
            .limit(5)
            .rows())
docs.insert({"title": "Dunes", "year": 2021})
docs.where("year", "<", 2000).delete()        # no filter: refused unless all=True
docs.where("lang", "tr").count()
```

`insert` sends `put`, which writes over a row holding a document's id;
`insert_new` (`to_insert_new` for the text) sends FennecQL's `insert`,
refused whole (409, `duplicate`) where one does. `in` takes an inner query
of one column as well as a list, answered before the query around it, its
parameters numbered where it stands; `order` takes an expression or a
`distance` as well as a field:

```python
from fennecdb import distance, expr

docs.insert_new({"id": 7, "title": "Oasis"}, returning=["id"], require=1)
orders = db.collection("orders")
in_tr = db.collection("customers").select("id").where("country", "TR")
orders.where("customer", "in", in_tr).rows()
orders.where({"customer": {"in": in_tr}}).count()
orders.order(expr("n * 2"), "desc").order("item").limit(10).rows()
db.collection("places").order(distance("loc", [13.4, 52.5])).limit(5).rows()
```

Aggregates go in `select` as FennecQL spells them, and `group` takes one key
or more; `bucket`, `count_distinct`, `first`, `last` and `expr` make a
column, named with `.as_(name)` (`as` being Python's keyword):

```python
from fennecdb import bucket, expr, first, last

bars = (db.collection("ticks")
          .select(bucket("at", "1m").as_("minute"), first("px", "at").as_("open"),
                  "max(px)", "min(px)", last("px", "at").as_("close"),
                  expr("sum(px * qty) / sum(qty)").as_("vwap"))
          .where("sym", "S07")
          .group("minute")
          .order("minute", "desc")
          .rows())
```

`facet(field, ranges=[0, 25, 50])` counts by ranges of numbers, each
value `[from, to]`, and `disjunctive=True` counts past the filter's own
condition on the field, as a shop's filter list does.

Several statements are one `batch`, a block under one write lock: all
land or none, and a `FennecError` that stops it says which (`e.at`, from 0;
412 for a write whose `require` was not met). `reason=` beside `require=`
(`require 1 else 'insufficient funds'`, written into the text) comes back
as `e.reason`, which tells one guard's refusal from another's where `e.at`
moves with every statement added in front; a read's `.require(1,
reason="no such account")` says its own. On a tenant node a quota
names itself (`e.quota`): `"bytes"` and `"rows"` answer 507, and room is
made by deleting; `"streams"`, `"requests"`, `"writes"` and `"write_bytes"`
answer 429 with `e.retry_after`, the seconds the server asks to wait, as a
tenant being moved or failed over answers its writes 503. A retry after a timeout
cannot tell whether a write ran, so a write that must not run twice goes
under an idempotency key -- the second answer is the first one kept
(`db.replayed`), and the key with another request is refused (422):

```python
from fennecdb import inc

debit = docs.where("id", 1).to_update({"stock": inc(-1)}, require=1)
db.batch([debit, ("put orders {item: $1}", [1])], idempotency_key=order_id)
db.with_idempotency_key(order_id).collection("orders").insert({"item": 1})
db.seq                                        # the change the last write left the database at
```

One value in two places is one parameter with `param`, so the amount a
write takes and the amount its filter checks cannot drift apart: each use
of the same `param` writes the same `$n`, where two equal values are two:

```python
from fennecdb import expr, param

amount = param(300)
(db.collection("accounts").where({"name": "ada", "balance": {"gte": amount}})
   .update({"balance": expr("balance - ?", amount)}, require=1))
# set accounts {balance: balance - $1} where name = $2 and balance >= $1 require 1
```

A job queue's claim is one statement: `order` and `limit` before
`update` pick the rows it writes, and `returning=True` (or a list of
fields) answers them as written, where a count is answered otherwise:

```python
from fennecdb import expr, inc, raw

claimed = (
    db.collection("jobs").where(raw("run_at <= now()")).order("run_at").limit(10)
    .update({"owner": me, "run_at": expr("now() + ?", 30000), "attempts": inc(1)}, returning=True, wait=30)
)
```

With `wait` (seconds, at most 30) a claim that finds no job is held at
the server until one comes -- enqueued, a delayed one due, a lease
lapsed -- rather than sent again every few: answered with the jobs it
took, or with none at the end of the wait. `query` and `batch` take it
too, and `AsyncClient`'s give a held request up when cancelled.

A named statement (`create statement transfer(...) as ...`) is called by
name, its arguments a mapping of values by name -- one parameter, `call
transfer $1`, sent over `/query` as any statement is, so the errors, the
key and `wait` are a statement's own:

```python
from fennecdb import call

db.call("transfer", {"from": 1, "to": 2, "amount": 300}, idempotency_key=transfer_id)
db.call("stamp")                                          # no arguments: call stamp
db.call("mine", claims={"sub": "alice"})                  # call mine as $1
db.call("history", {"limit": 10}, read=True)              # goes near on reads="nearest"
await async_db.call("transfer", {"from": 1, "to": 2, "amount": 300})
text, params = call("transfer", {"from": 1, "to": 2, "amount": 300}).to_call()
db.batch([(text, params), debit])                         # a call inside a batch
```

`db.call(name, args=None, *, claims=None, idempotency_key=None, wait=None,
read=False)` answers as `query` does -- rows, or the affected count for a
body that answers none -- and `AsyncClient.call` is the same awaited. The
arguments and the claims are parameters, normalised as a document's values
are (a `datetime` its ISO text, an `array.array` its numbers); only the
name is written into the text, so it is checked: ASCII letters, digits and
`_`, a letter first, 59 bytes at most, else a `FennecError` of status 0
(`invalid statement name: ...`), as is arguments or claims that are not a
mapping (`a call's arguments are an object of values by name`). `claims`
is the `as` part, the server's token alone may say it (a scoped token's
are its own). `read=True` says the statement only reads: on a client with
`reads="nearest"` it is answered near the client, after its writes, and a
statement that writes is refused there (403), never landed; not said, a
call goes home. Errors are `FennecError`'s, by status: 404 no such
statement or none granted, 403 a claim missing, 400 an argument missing,
extra or of another type, 409 a unique value taken, 412 a `require` not met
with its `e.reason`.

To see the text and parameters a chain builds, for logging or a test,
`to_fennecql()` returns them and runs nothing:
`docs.select("title").limit(5).to_fennecql()` is
`("get articles select title limit 5", [])`. A query needs no call to it
before it runs.

In an event loop -- FastAPI, aiohttp, an agent -- `AsyncClient` makes the
same calls awaited, over one kept-alive connection, the standard library
alone:

```python
from fennecdb import AsyncClient

async with AsyncClient("http://127.0.0.1:8080", token="...") as db:
    rows = await db.query("get articles select title near embed $1 limit 5", [[0.1, 0.2, 0.3]])
    await db.batch([("put articles {title: $1}", ["a"]), ("put articles {title: $1}", ["b"])])
    rows = await db.collection("articles").where("year", ">=", 2024).rows()
```

Every write on the server's disk (`fennec-server --cdc`), for a consumer whose
cursor the server keeps -- each batch committed once the loop comes back
for the next, so each write comes at least once, and a loop started again
goes on from the last batch committed -- and the same handed on to another
program or a webhook. `db.consumer(name)` makes one where there is none and
leaves one that is there where it stands; `db.move_consumer(name, since)`
moves it:

```python
for batch in db.follow("search-index"):
    for change in batch:               # {"seq", "at", "collection", "op", "id", "doc"}
        index(change)
```

```sh
python -m fennecdb.relay http://127.0.0.1:8080 --consumer kafka | kcat -P -b broker:9092 -t fennec
```

```python
from fennecdb.langchain import FennecVectorStore

store = FennecVectorStore(embeddings, "docs", url="http://127.0.0.1:8080", token="...",
                         metadata_fields={"source": "text"})
store.add_documents(documents)
store.similarity_search("how do I compact", k=4, filter={"source": "handbook"})
```

```python
from fennecdb.llama_index import FennecVectorStore
from llama_index.core import StorageContext, VectorStoreIndex

store = FennecVectorStore("docs", url="http://127.0.0.1:8080", token="...")
index = VectorStoreIndex.from_documents(
    documents, storage_context=StorageContext.from_defaults(vector_store=store)
)
```

A document or node is a row -- its id, text, metadata as JSON and vector
under `@hnsw` -- in a collection created on the first write. Fields named in
`metadata_fields` get columns of their own under `@hash`, and filters over
them are answered by the index before the search runs.

With `full_text=True` the text is indexed for BM25 too, and both stores search
by the words alone or by the words and the vector fused -- LangChain's
`mode="text"` and `mode="hybrid"`, LlamaIndex's `TEXT_SEARCH` and `HYBRID`.
`quant="int8"` or `"bit"` keeps the graph over codes.

```python
store = FennecVectorStore(embeddings, "docs", url=..., token=..., full_text=True)
store.similarity_search("how do I compact", k=4, mode="hybrid")
```

`./run-tests.sh` runs LangChain's standard vector store suite and the tests
LlamaIndex's own integrations run against a fennec-server it builds and starts,
and the query builder against every case of `integrations/builder-golden.json`.
Full reference: https://fennecdb.com/docs/integrations

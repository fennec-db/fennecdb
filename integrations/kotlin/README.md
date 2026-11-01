# fennecdb for Kotlin and Java

fennecdb embedded in an Android or JVM app: the database is a file on the
device, the engine the native library (`crates/fennec-ffi`) reached through
JNI functions it carries itself -- on its own, or a replica a server keeps
in step.

```kotlin
// build.gradle.kts
implementation("com.fennecdb:fennecdb-android:0.1.7") // Android: the AAR, the library for each ABI inside
implementation("com.fennecdb:fennecdb:0.1.7")         // the JVM: bring libfennec_ffi for the platform
```

```kotlin
val db = Fennec.openAsync(File(context.filesDir, "app.fennec").path)
db.execute("create collection todos (title text, done bool @hash)")
db.from("todos").insert(mapOf("title" to "milk", "done" to false))
val open = db.from("todos").where("done", false).rows()
val near = db.query("get notes near embed \$1 limit 5", embedding)   // a FloatArray

@Composable
fun Todos(db: Fennec) {
    val todos by remember { db.live(db.from("todos").where("done", false)) }.collectAsState(emptyList())
    LazyColumn { items(todos) { Text(it.string("title") ?: "") } }
}
```

The suspending calls run on `Dispatchers.IO`; Java calls the `...Blocking`
ones (`queryBlocking`, `executeBlocking`, `rowsBlocking` ...) off its main
thread. A live query is a conflated `Flow<List<Row>>`, run again after a
write to a collection it reads, once a burst. To see the text and
parameters a chain builds, for logging or a test, `query.toFennecQL()`
returns them and runs nothing. Every write is fsynced before
it returns unless the file is opened with `Fennec.NO_SYNC`, which leaves the
writes for `sync()`; `flush()` hands them to the system, which outlives the
app being killed -- what `onStop` calls. An open leaves the hash, text,
ordered and sparse indexes for their first read; `warm(listOf("docs"))`
(`warmBlocking` from Java) builds them on `Dispatchers.IO` as the app
starts, so the first search does not (a `@text` index of 100 000 products:
141 ms), and answers how many it built. The builder's `facet(field, ranges
= listOf(0, 25, 50))` counts by ranges of numbers, each value `[from, to]`,
and `disjunctive = true` counts past the filter's own condition on the
field. The docs: `site/content/docs/mobile.html`.

Rows are `Row`s, a `Map<String, Any?>` in the answer's order with typed
getters (`string`, `long`, `double`, `bool`, `floats`, `row`, `list`); JSON is
read by a reader of the library's own, so it depends on kotlinx-coroutines
alone.

`insert` sends `put`, which writes over a row holding a document's id;
`insertNew` (`toInsertNew` for the text, `insertNewReturning` for the rows)
sends FennecQL's `insert`, refused whole (409, `Code.DUPLICATE`) where one
does. `in` takes an inner query of one column as well as a list, answered
before the query around it, its parameters numbered where it stands; `order`
takes an expression or a `distance` as well as a field; `match(..., fuzzy =
true)` is `fuzzy` alone, the default edits, and with `fuse` a `match` and a
`near` are written in the order they were called:

```kotlin
val orders = db.from("orders")
db.from("docs").insertNew(mapOf("id" to 7, "title" to "Oasis"), require = 1)
val turkish = db.from("customers").select("id").where("country", "TR")
orders.where("customer", "in", turkish).rows()
orders.where(mapOf("customer" to mapOf("in" to turkish))).count()
orders.order(Computed.expr("n * 2"), "desc").order("item").limit(10).rows()
db.from("places").order(Computed.distance("loc", listOf(13.4, 52.5))).limit(5).rows()
db.from("products").match("title", input, fuzzy = true, prefix = true).limit(10).rows()
```

One value in two places is one parameter with `param`, so the amount a write
takes and the amount its filter checks cannot drift apart: each use of the
same `param` writes the same `$n`, where two equal values are two. It goes
wherever a value does -- a condition (also as an operator map's value), an
`in` list's element, a document, `Computed.expr`'s and `Cond.raw`'s `?`,
`Computed.distance`'s point, `near`'s vector, `match`'s words:

```kotlin
val amount = param(300)
db.from("accounts").where(mapOf("name" to "ada", "balance" to mapOf("gte" to amount)))
    .update(mapOf("balance" to Computed.expr("balance - ?", amount)), require = 1)
// set accounts {balance: balance - $1} where name = $2 and balance >= $1 require 1
```

## Sync with a server

```kotlin
val db = Fennec.sync(
    url = "https://api.example.com", token = jwt,
    shapes = listOf(Shape("todos", where = mapOf("owner" to me), key = "key")),
    path = File(context.filesDir, "todos.fennec").path,
    tokenProvider = { refreshToken() },
)
db.replica!!.ready()
```

The same `Fennec`: reads and live queries are the file's, a write to a
shape's collection shows at once and is sent -- queued in the file while
the server cannot be reached, under an idempotency key so it lands once --
and one the server refuses is put back and comes on `replica.refusals`, a
tenant's quota of bytes or rows among them (its `quota` named) and a
`require` not met (its `reason` named), the writes behind it going on; a
quota's 429 is waited out for the seconds it names.
`replica.status` is a `StateFlow`: `ONLINE`, `OFFLINE` or `CATCHING_UP`,
the writes not yet answered and the last error, whose `quota` tells a
stream the quota of streams refused from the network gone; call `replica.resume()` in
`onStart`. The requests and the stream are `HttpURLConnection`'s, the JVM's
and Android's own, so a server is reached through TLS (Android refuses
cleartext by default). `Fennec.connect(url, token)` is a server with no
file: every query a request, the same builder. Its `batch(listOf(...),
idempotencyKey = id)` runs the builder's `toInsert`/`toUpdate`/`toDelete` as
one block, all or none, answering `results`, `seq` and `replayed`; a
`FennecException` that stops it carries `status` (412 and `UNMET` for a
`require` not met) and `at`, the statement from 0. A `require` says why
with `reason` -- `update(patch, require = 1, reason = "insufficient
funds")`, a read's `require(1, reason = "no such account")` -- written into
the statement as `require 1 else '...'`, and the exception's `reason`, the
library's and a server's alike, tells one refusal from another where `at`
moves with every statement put in front. On a tenant node a
quota names itself (`QUOTA`, and `quota`): `"bytes"` and `"rows"` answer
507, and room is made by deleting; `"streams"`, `"requests"`, `"writes"`
and `"write_bytes"` answer 429 with `retryAfter`, the seconds to wait, as a
tenant being moved or failed over answers its writes 503. `runList(text, params,
idempotencyKey = id)` and `withIdempotencyKey(key)`, a copy whose every
write carries the key, make a write once however often it is sent.
`Fennec.connect(url, token, reads = FennecRemote.Reads.NEAREST)` sends the
builder's reads, and an `asRead()` copy's, to the tenant's read replica near
the app through a fennec-shard router (`Fennec-Read: replica`), each after the
furthest position an answer from home named to the connection or any copy of
it (`Fennec-After`, `Fennec-History`); writes go home.

## Named statements

A statement defined once in the database (`create statement transfer(...)`)
is called by name with its arguments as one map, by name:

```kotlin
val moved = db.call("transfer", mapOf("from" to 1, "to" to 2, "amount" to 300))
val mine = db.call("history", read = true).rows
val st = call("mine", mapOf("limit" to 10), claims = mapOf("sub" to "alice")).toCall()
// st.text == "call mine $1 as $2", st.params == [{limit=10}, {sub=alice}]
```

`call` sends the builder's `call <name> $1` (`Call.of(name, args, claims)` or
the top-level `call(...)`, its `toCall()` the text and parameters like
`toInsert`'s) through `/query`, so the transport, the errors and the answer
are a text's: the rows of a statement ending in a `get` or a `returning`,
else how many documents it wrote. Only the name is written into the text, so
it is checked (ASCII letters, digits and `_`, a letter first, at most 59
bytes); the arguments and the claims are parameters, and the builder refuses
anything but a map (`a call's arguments are an object of values by name`).
No arguments is `call stamp`, an empty map is still one. On `Fennec.connect`'s
`FennecRemote`, `call(name, args, claims, idempotencyKey, wait, read)`:
`claims` is the `as` the server's token makes the call with (a scoped token's
own are its token's), `idempotencyKey` and `wait` (milliseconds, 30 000 at
most) are a text's, a cancelled coroutine gives a held call up, and `read =
true` marks a call known to only read, as `asRead()` does -- on a connection
reading `NEAREST` it is answered by the read replica near the app, and one
that writes is refused (403), never landed; without it a call goes home. A
refusal is a `FennecException`: `status` 404 (no such statement, or not
granted), 403 (a claim missing), 400 (an argument missing, extra or of
another type), 409 (a broken statement, a unique value taken) or 412 with
the `require`'s `reason`.

On the embedded `Fennec`, `call(name, args, claims)` runs it locally through
the library (`callBlocking` for Java): `claims` are the embedder's `as`, a
signed-in user's, held to the types the statement declares as a token's are.
There is no `wait` or key on a local database. A database made by
`Fennec.sync` is a replica of its shapes' rows, not of its server's
statements, so its `call` is refused with the engine's words (`a synced
database runs no named statement ...`) before anything runs or is queued:
call the server through `Fennec.connect`, and what the call wrote comes back
through the shapes' streams.

## Building and testing

`make kotlin-test` builds the native library for Linux with its JNI
functions and runs the JVM tests under Gradle -- the engine, the builder
over every case of `integrations/builder-golden.json`, live queries, a
replica against a `fennec-server` the tests start, kill and start again,
reads near a read replica behind `fennec-shard` routers --
in containers unless this is Linux with Gradle and Java.
`integrations/kotlin/build-aar.sh` builds the AAR with the NDK for
arm64-v8a, armeabi-v7a and x86_64 (`ANDROID_HOME`, `ANDROID_NDK_HOME`).

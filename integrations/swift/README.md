# FennecDB for Swift

fennecdb embedded in a macOS or iOS app: the database is a file on the
device, the engine the native library (`crates/fennec-ffi`) the server and
the browser module share their code and file format with -- on its own, or
a replica a server keeps in step.

```swift
.package(url: "https://github.com/fennec-db/fennecdb", from: "0.1.12")
// target: .product(name: "FennecDB", package: "fennecdb")
```

SwiftPM names a package after the last component of its URL, and the
repository was `fenecdb/fenec`, then `fenecdb/fenecdb`, before it became
`fennec-db/fennecdb`. An app that keeps an old URL (GitHub redirects it)
keeps that URL's package identity, `fenec` or `fenecdb`, in its target
dependencies (`package: "fenecdb"`); one that moves to
`https://github.com/fennec-db/fennecdb` changes them to
`package: "fennecdb"`, and its product to `FennecDB`, from the first release
after 0.1.12.

```swift
import FennecDB

let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
let db = try await Fennec.open(docs.appendingPathComponent("app.fennec"))
try await db.execute("create collection todos (title text, done bool @hash)")

struct Todo: Codable, Sendable, FennecValue { var title: String; var done: Bool }
try await db.from("todos").insert(Todo(title: "milk", done: false))

let open = try await db.from("todos").where("done", false).rows(as: Todo.self)
let near = try await db.query("get notes near embed $1 limit 5", embedding)   // [Float]
```

In SwiftUI, `LiveQuery` (an `ObservableObject`) or `LiveRows` (`@Observable`,
iOS 17 and macOS 14) holds a query's rows and runs it again after a write to
a collection it reads, once a burst, on the main actor:

```swift
@StateObject var todos = LiveQuery(db, try! db.from("todos").where("done", false), as: Todo.self)
```

`db.live(query)` is the same as an `AsyncThrowingStream`. To see the text
and parameters a chain builds, for logging or a test, `try
query.toFennecQL()` returns them and runs nothing.

Every write is fsynced before it returns unless the file is opened
`.noSync`, which leaves the writes for `sync()`; `flush()` hands them to the
system, which outlives the app being killed. `checkpoint()` writes the file
anew, graphs and all. An open leaves the hash, text, ordered and sparse
indexes for their first read; `warm(["docs"])` builds them off the main
thread as the app starts, so the first search does not (a `@text` index of
100 000 products: 141 ms), and answers how many it built. The builder's
`facet(field, ranges: [0, 25, 50])` counts by ranges of numbers, each value
`[from, to]`, and `disjunctive: true` counts past the filter's own
condition on the field. The docs: `site/content/docs/mobile.html`.

`insert` writes `put`, over a row holding a document's id; `insertNew`
(`toInsertNew` for the text, `insertNewReturning` for the rows) writes
FennecQL's `insert`, refused whole (409, `.duplicate`) where one does, and on
a sync replica refused at once, before anything is sent, where the replica
holds the id. `Value.param(300)` is one value bound once: each use writes
the same `$1`, where the same value given twice is two parameters, so the
amount a write takes and the amount its filter checks cannot drift apart. A
query is the list of an `in`, run before the one around it and numbered where
it stands; `order` takes an expression or a `distance` as well as a field;
`match(field, text, fuzzy: true)` writes `fuzzy` alone; with `fuse()`, `match`
and `near` are written in the order they were called:

```swift
let amount = Value.param(300)
try await db.from("accounts").where(["name": "ada", "balance": ["gte": amount]])
    .update(["balance": .expr("balance - ?", amount)], require: 1)
// set accounts {balance: balance - $1} where name = $2 and balance >= $1 require 1

let orders = try db.from("orders")
try await orders.where("customer", "in", db.from("customers").select("id").where("country", "TR")).rows()
try await orders.order(.expr("n * 2"), "desc").order("item").limit(10).rows()
try await db.from("journal").insertNew(["entry": "inv-7"] as Value)
```

## Sync with a server

```swift
let db = try await Fennec.sync(url: "https://api.example.com", token: jwt,
                              shapes: [Shape("todos", where: ["owner": me], key: "key")],
                              path: docs.appendingPathComponent("todos.fennec").path,
                              tokenProvider: { try await refreshToken() })
await db.replica!.ready()
```

The same `Fennec`: reads and live queries are the file's, a write to a
shape's collection shows at once and is sent -- queued in the file while
the server cannot be reached, under an idempotency key so it lands once --
and one the server refuses is put back and comes on
`db.replica!.refusals()`, a tenant's quota of bytes or rows among them
(its `quota` named) and a `require` not met (its `reason` named), the
writes behind it going on; a quota's 429 is
waited out for the seconds it names. `replica.status` says `online`,
`offline` or `catchingUp`, the writes not yet answered and the last error,
whose `quota` tells a stream the quota of streams refused from the network
gone; call
`replica.resume()` as the app comes back to the foreground. The requests and
the stream are `URLSession`'s, so a server is reached through TLS (App
Transport Security). `Fennec.connect(url:token:)` is a server with no file:
every query a request, the same builder. Its `batch([...], idempotencyKey:)`
runs the builder's `toInsert`/`toUpdate`/`toDelete` as one block, all or
none, answering `results`, `seq` and `replayed`; a `FennecError` that stops
it carries `status` (412 and `.unmet` for a `require` not met) and `at`,
the statement from 0. A `require` says why with `reason` --
`update(patch, require: 1, reason: "insufficient funds")`, a read's
`.require(1, reason: "no such account")` -- written into the statement as
`require 1 else '...'`, and the error's `reason`, the library's and a
server's alike, tells one refusal from another where `at` moves with every
statement put in front. On a tenant node a quota names itself (`.quota`, and
`quota`): `"bytes"` and `"rows"` answer 507, and room is made by deleting;
`"streams"`, `"requests"`, `"writes"` and `"write_bytes"` answer 429 with
`retryAfter`, the seconds to wait, as a tenant being moved or failed over
answers its writes 503. `run(text, params:, idempotencyKey:)` and
`withIdempotencyKey(key)`, a copy whose every write carries the key, make a
write once however often it is sent. `Fennec.connect(url:token:reads:
.nearest)` sends the builder's reads, and an `asRead()` copy's, to the
tenant's read replica near the app through a fennec-shard router
(`Fennec-Read: replica`), each after the furthest position an answer from
home named to the connection or any copy of it (`Fennec-After`,
`Fennec-History`); writes go home.

## Named statements

A statement a database holds under a name (`create statement transfer($from
int, $to int, $amount int) public as ...`) is called with `call`, on the
embedded database and on a server alike; the arguments are an object of
values by name -- a dictionary, a `Row`, a `Value` or a `Codable`
`FennecValue` -- and the answer is `run`'s: the rows of a `get` or a
`returning` body in `.rows`, the count of a write that answers none in
`.affected`.

```swift
struct Transfer: Codable, Sendable, FennecValue { var from: Int; var to: Int; var amount: Int }

// The embedded database: call <name> $1 through the library.
let moved = try await db.call("transfer", Transfer(from: 1, to: 2, amount: 300))
let stamped = try await db.call("stamp")                                  // a statement of none
let mine = try await db.call("mine", claims: ["sub": "alice"])            // a signed-in user's claims

// A server (Fennec.connect): the same call over /query.
let remote = Fennec.connect(url: "https://api.example.com", token: jwt)
let rows = try await remote.call("history", read: true)                   // near, on a connection reading .nearest
try await remote.call("transfer", Transfer(from: 1, to: 2, amount: 300), idempotencyKey: orderID)
```

`func call(_ name: String, _ args: (any FennecValue)? = nil, claims: (any
FennecValue)? = nil) async throws -> Answer` on `Fennec`, and on
`FennecRemote` with `idempotencyKey: String? = nil, wait: Double? = nil,
read: Bool = false` after them. `claims` is the call's `as`: the server
reads them from its own token alone (a scoped token's are its token's, and
it may not say them), the embedded database from the embedder, a signed-in
user's, held to the types the statement declares as a token's are.
`idempotencyKey` makes a write once however often it is sent; `wait` is
`withWait(seconds)`'s, at most 30; `read` is `asRead()`'s mark: on a
connection made with `reads: .nearest` a read replica answers the call, one
that writes is refused (403) and never lands, and without it a call goes
home, as every text not known to read does. The embedded database has no
`wait`, key or `read`. A call is refused as the text would be: a
`FennecError` with `status` on a server -- 404 no such statement or not
granted, 403 a claim missing, 400 an argument missing, extra or of another
type, 409 a broken statement or a value taken, 412 (`.unmet`) a `require`
not met, its `reason` the statement's -- and `.notFound`, `.denied`,
`.query` and `.unmet` from the library.

A synced database (`Fennec.sync`) holds its shapes' rows, not its server's
statements, and refuses a local `call` (`.query`: "a synced database runs
no named statement"); call the server through `Fennec.connect`, and what it
writes comes back through the shapes.

The call builder is `Call`, stood beside `Query.from`: `try Call.call("mine",
["limit": 10], claims: ["sub": "alice"]).toCall()` is `("call mine $1 as
$2", [{"limit": 10}, {"sub": "alice"}])`, the text and parameters for
`batch` or a log. No arguments is a statement of none (`call stamp`), an
empty object is still one argument (`call stamp $1`), and the name -- the
one thing written into the text -- is ASCII letters, digits and `_`, a
letter first, at most 59 bytes (`invalid statement name: "..."`); a list, a
text or a number as the arguments or the claims is refused, the builder's
words held to `integrations/builder-golden.json`.

## Building and testing

`make swift-test` builds the XCFramework's macOS slice for this machine
(`integrations/swift/build-xcframework.sh --macos`) and runs `swift test`
at the repository's root, where `Package.swift` is (the sync tests start a
`fennec-server` it builds, and the read-routing tests `fennec-shard` routers
in front of two, macOS alone) -- SwiftPM fetches a
package by its repository, so the manifest lives there. It takes the
XCFramework from `integrations/swift/build/` when it is there and from the
release's download otherwise. `build-xcframework.sh` alone builds every
slice: macOS (arm64, x86_64), iOS, and the simulator (arm64, x86_64).
It then runs the tests again with Swift's cooperative pool cut to one
thread (`test-strict.sh`, `LIBDISPATCH_COOPERATIVE_POOL_STRICT=1`), where
a call that blocks a thread of the pool hangs at once rather than now and
then on a device of few cores; a run that hangs is sampled and fails.

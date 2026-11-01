# FennecDb for .NET

A client for fennec-server's HTTP endpoint, `HttpClient` and `System.Text.Json` alone, for .NET 8 and newer.

```sh
dotnet add package FennecDb
```

```csharp
using FennecDb;

using var db = new FennecClient("http://127.0.0.1:8080", new() { Token = "secret" });
var w = await db.ExecAsync("put docs {title: $1, embed: $2}", ["Dunes", new[] { 0.9f, 0.1f, 0f }]);
var rows = await db.QueryAsync("get docs select title near embed $1 limit 5", [new[] { 0.1f, 0.2f, 0.3f }]);
var hits = await db.QueryAsync<Hit>("get docs select title near embed $1 limit 5", [vector]);
```

The query builder writes the statement -- every value a parameter, every name checked -- the same text the
Python, JavaScript and Go builders make of the same chain. A `Query` is immutable, each call a new one:

```csharp
var docs = db.From("articles");
var q = docs.Select("title")
    .Where("year", ">=", 2024)
    .Where(Cond.Or(Cond.Cmp("lang", "=", "tr"), Cond.Cmp("tags", "has", "rust")))
    .Near("embed", vector, ef: 64)
    .Limit(5);
var hits = await q.RowsAsync<Hit>();           // or RowsAsync(), FirstAsync(), CountAsync()
await docs.InsertAsync(new { title = "Dunes", year = 2021 });
await docs.Where("year", "<", 2000).DeleteAsync();   // no filter: refused unless all: true
await docs.Lookup("reviews", on: "article_id", limit: 3, order: [new("created", "desc")]).RowsAsync();
```

- `QueryAsync` gives rows as `JsonElement`s, `QueryAsync<T>` maps them to records or classes by property name, any case; `ExecAsync` a write's count and its `Seq`.
- `BatchAsync([new Statement(q, params), ...])` runs statements as one block: all land, or none.
- `WithIdempotencyKey(key)` makes a write once; `After(seq)` reads a write on a replica (`Fennec-After`);
  `WithWait(TimeSpan.FromSeconds(30))` holds a job queue's claim at the server until a job comes (`Fennec-Wait`),
  its cancellation token giving it up.
- `new FennecClientOptions { ReadsNearest = true }` sends the builder's reads, and an `AsRead()` copy's, to the
  tenant's read replica near the client through a fennec-shard router (`Fennec-Read: replica`), each after the
  furthest position an answer from home named (`Fennec-After`, `Fennec-History`); writes go home. An `AsRead()`
  copy's `SubscribeAsync` and `ChangesAsync` go so too, and the server holds the stream until a copy has the
  client's writes.
- `SubscribeAsync(collection, [new("year", "gte.2024")])` is an `IAsyncEnumerable<Event>` of `seed` and `change` events (SSE).
- `ChangesAsync(since, wait)` reads every write on disk (`/_changes`); `HealthAsync()` asks `/_health`.
- `FennecClientOptions` takes `Token`, `Tenant` (`/t/<tenant>/`), `Timeout` and an `HttpClient` of your own.
- A refusal is a `FennecException` with `Status`, `Code` and the server's message; a failed batch's also says which
  statement stopped it, `At` (from 0; `null` otherwise), and how many stayed applied, `Completed`.
  `reason: "insufficient funds"` beside `require: 1` (`require 1 else '...'`, written into the text) comes back as
  `Reason`, which tells one guard's refusal from another's where `At` moves with every statement added in front; a
  read's `Require(1, reason: "no such account")` says its own. A tenant's quota names itself, `Quota`: `"bytes"` and `"rows"` answer 507 (`storage_full`), and room is made by deleting;
  `"streams"`, `"requests"`, `"writes"` and `"write_bytes"` answer 429 (`too_many`) with `RetryAfter`, the wait the
  server asks for, as a tenant being moved or failed over answers its writes 503 (`unavailable`). Every call takes a
  `CancellationToken`.
- A `float[]` or `ReadOnlyMemory<float>` goes out as the decimals that read back as each float, so a vector round-trips to the bit.

- `Facet(field, ranges: [0, 25, 50])` counts by ranges of numbers, each value `[from, to]`; `disjunctive: true`
  counts past the filter's own condition on the field, as a shop's filter list does.
- A `Dictionary<string, object?>` is the builder's object condition, its operators a dictionary too; a refused
  step throws `FennecQueryException` with the JS builder's message.
- Aggregates go in `Select` as FennecQL spells them (`"count(*)"`, `"sum(total)"`), and `Group` takes one key or
  more; `Computed.Bucket(field, "1m")`, `CountDistinct`, `First(field, by)`, `Last` and `Expr` make a column,
  named with `.As(name)`: `Select(Computed.Bucket("at", "1h").As("hour"), "count(*)").Group("hour")`.
- `Order` takes an expression too, ties in id order: `Order(Computed.Expr("n * 2"), "desc")`,
  `Order(Computed.Distance("loc", new[] { 13.4, 52.5 }))`; it picks the rows an update or a delete writes as well.
- `new Param(300)` is one value bound once, each use writing the same `$1` -- a filter, a document,
  `Computed.Expr`, `Cond.Raw`, `Computed.Distance`, `Near`'s vector, `Match`'s text -- where the same value given
  twice is two parameters: with `var amount = new Param(300)`,
  `Where(new Dictionary<string, object?> { ["balance"] = new Dictionary<string, object?> { ["gte"] = amount } })
  .UpdateAsync(new { balance = Computed.Expr("balance - ?", amount) }, require: 1)` takes and checks one amount.
  A `Param` inside a list or a json value is refused. `Computed.Expr`'s and `Cond.Raw`'s text is trusted FennecQL,
  written into the statement as it is: the code's own, never a value from a user, which goes in as a `?`'s parameter.
- A query is the list of an `in`, run before the one around it: `Where("customer", "in",
  db.From("customers").Select("id").Where("country", "TR"))` is `customer in (get customers select id where country
  = $1)`, or `["in"] = query` in an object condition; it selects exactly one column.
- `InsertAsync` writes `put`, over a row holding the id; `InsertNewAsync(docs)` writes `insert`, refused whole (409,
  `duplicate`) where a row holds one -- `require`, `reason` and `returning` as `InsertAsync`'s, and `ToInsertNew`
  its text.
- `Match(field, text, fuzzy: true)` writes `fuzzy` alone, the parser's two edits; `fuzzy: 1` one at most. With
  `Near` and `Fuse()`, the two searches are written in the order they were first called.
- To see the text and parameters a chain builds, for logging or a test, `q.ToFennecQL()` returns them and runs
  nothing; a query needs no call to it before it runs.

### Named statements

A statement defined once in the database (`create statement transfer(...) as begin ... end`) is called by name with
typed arguments. `CallAsync` sends `call <name> $1` over `/query`, so the transport, the errors, the key and the wait
are a statement's:

```csharp
var moved = await db.CallAsync("transfer", new { from = 1, to = 2, amount = 300 }, idempotencyKey: "xfer-7");
var balance = moved.Rows![0].GetProperty("balance").GetInt64();   // the last statement of its body answers
```

`CallAsync(name, args, claims:, idempotencyKey:, wait:, read:, cancellationToken:)` returns an `ExecResult`: `Rows`
where the body answers rows, `Affected` where it does not, `Seq`, and `Replayed` for an answer kept for its key.

- `args` is an object of values by name -- a dictionary, an anonymous object, a record -- or null for a statement of
  none; an empty object is still one argument. `claims` is the same, the call's `as`: the claims it is made with, for a
  statement declaring them, which the server's token alone may say (a scoped token's are its own).
- `read: true` says the statement only reads: on a client with `ReadsNearest` it goes to the read replica near the
  client after the client's writes, and one that writes is refused there (403), never landed. Without it a call goes
  home. `idempotencyKey` makes the call once; `wait` holds one whose body can be held, as `WithWait` does; the
  `CancellationToken` and the client's `Timeout` bound it.
- A refusal is a `FennecException`: 404 for no such statement (or one the token is not granted), 403 for a claim the
  token lacks, 400 for an argument missing, extra or of another type, 412 for a `require` not met, its `Reason` the
  statement's own words, 409 for a broken statement or a unique value taken.
- `Query.Call(name, args, claims)` is the builder alone: `.ToCall()` returns the text and parameters, `call
  transfer $1`, and sends nothing. The values are parameters; the name, the one thing written into the text, must be
  ASCII letters, digits and `_`, a letter first, 59 bytes at most. A name that is not one, or arguments or claims
  that are not an object of values by name (a list, a text), throw `FennecQueryException` with the JS builder's
  message, before anything is sent.

`dotnet test FennecDb.Tests` runs against `fennec-server` processes it starts (`cargo build -p fennec-server`, or
`FENNEC_SERVER`), and the builder against every case of `integrations/builder-golden.json`.
Full reference: https://fennecdb.com/docs/languages

# fennecdb for Go

A client for fennec-server's HTTP endpoint, the standard library alone.

```sh
go get github.com/fennec-db/fennecdb/integrations/go
```

The module's path follows the repository's name. v0.1.11 and earlier were
published as `github.com/fenecdb/fenec/integrations/go`, before the
repository became `fenecdb`, and v0.1.12 as
`github.com/fenecdb/fenecdb/integrations/go`, before fenecdb became
fennecdb; each declares its path: an app on one of them keeps importing it,
and moves to the new path with the first release that declares it.

```go
import fennecdb "github.com/fennec-db/fennecdb/integrations/go"

db := fennecdb.New("http://127.0.0.1:8080", fennecdb.WithToken("secret"))
w, err := db.Exec(ctx, "put docs {title: $1, embed: $2}", "Dunes", []float32{0.9, 0.1, 0})
rows, err := db.Query(ctx, "get docs select title near embed $1 limit 5", []float32{0.1, 0.2, 0.3})
hits, err := fennecdb.QueryAs[Hit](ctx, db, "get docs select title near embed $1 limit 5", vec)
```

The query builder writes the statement -- every value a parameter, every name checked -- the same text the
Python, JavaScript and .NET builders make of the same chain. A `*Builder` is immutable, each step a copy, and the
first refused step comes back from whatever ends the chain:

```go
docs := db.From("articles")
q := docs.Select("title").
	Where("year", ">=", 2024).
	WhereCond(fennecdb.Or(fennecdb.Fields("lang", "tr"), fennecdb.Fields("tags", fennecdb.Ops("has", "rust")))).
	Near("embed", vec, fennecdb.Ef(64)).
	Limit(5)
rows, err := q.Rows(ctx)                       // or fennecdb.RowsAs[Hit](ctx, q), q.First(ctx), q.Count(ctx)
docs.Insert(ctx, fennecdb.D("title", "Dunes", "year", 2021))
docs.Where("year", "<", 2000).Delete(ctx)      // no filter: refused unless fennecdb.All()
```

- `Query` gives rows as `[]map[string]any`, `QueryAs[T]` decodes them by `json` tags; `Exec` a write's count and its `Seq`.
- `Batch(ctx, fennecdb.Stmt(q, params...), ...)` runs statements as one block: all land, or none.
- `IdempotencyKey(key)` makes a write once; `After(seq)` reads a write on a replica (`Fennec-After`), and
  `Wait(d)` beside it bounds how long the server holds the read for the change.
- `New(url, fennecdb.WithReadsNearest())` sends the builder's reads, and a `Read()` copy's, to the tenant's read
  replica near the client through a fennec-shard router (`Fennec-Read: replica`), each after the furthest position
  an answer from home named (`Fennec-After`, `Fennec-History`); writes go home. A `Read()` copy's `Subscribe`
  and `Changes` go so too, and the server holds the stream until a copy has the client's writes.
- `QueryRaw` and `BatchRaw` hand the answer back as the server wrote it, its columns in the select list's order
  (a map loses it); `IfNoneMatch(r.ETag).QueryRaw(...)` is answered `NotModified` while nothing the statement
  reads was written. The tag names a change, not the statement: send it only with the request it came from.
- `Subscribe(ctx, collection, url.Values{"year": {"gte.2024"}})` is a channel of `seed` and `change` events (SSE).
- `Changes(ctx, since, wait)` reads every write on disk (`/_changes`); `Health(ctx)` asks `/_health`.
- `WithTenant("acme")` sends everything under `/t/acme/`; `WithTimeout`, `WithHTTPClient` as they say.
- A refusal is an `*fennecdb.Error` with `Status`, `Code` and `Message`, for `errors.As`; a failed `Batch`'s also
  says which statement stopped it, `At` (from 0; -1 otherwise), and how many stayed applied, `Completed`.
  `Reason("insufficient funds")` beside `Require(1)` (`require 1 else '...'`, written into the text) comes back
  as `Reason`, which tells one guard's refusal from another's where `At` moves with every statement added in
  front; a read's `Require(1, Reason("no such account"))` says its own. A tenant's quota names itself, `Quota` (the `Quota*` constants): `QuotaBytes` and `QuotaRows` answer 507
  (`CodeFull`), and room is made by deleting; the rest answer 429 (`CodeTooMany`) with `RetryAfter`, the wait the
  server asks for, as a tenant being moved or failed over answers its writes 503 (`CodeUnavailable`).
- A `[]float32` goes out as the decimals that read back as each `f32`, so a vector round-trips to the bit.

- The builder's options are functional: `Ef`, `Exact`, `K`, `Candidates`, `Collate`, `All`, and `On`, `ParentKey`,
  `Select`, `Where`, `Required`, `Sort`, `Limit`, `Offset` for `Lookup` -- `On("org", "order"), ParentKey("org",
  "id")` a key over several fields. An object condition is `Fields` and `Ops`, names and values in turn, and a
  document a `D(...)` in its order, a map in its sorted keys, or a struct.
- `fennecdb.Param(v)` is one value in several places, bound once: `amount := fennecdb.Param(300)` in
  `WhereCond(fennecdb.Fields("name", "ada", "balance", fennecdb.Ops("gte", amount)))` and
  `Update(ctx, fennecdb.D("balance", fennecdb.Expr("balance - ?", amount)), fennecdb.Require(1))` is
  `set accounts {balance: balance - $1} where name = $2 and balance >= $1 require 1`. A Param goes wherever a value
  does -- a condition, an `in` list's element, a document, `Expr`'s and `Raw`'s `?`, `Distance`'s point, `Near`'s
  vector, `Match`'s words; the same value given twice is two parameters.
- `Insert` sends `put`, which writes over a row holding the id; `InsertNew` (and `ToInsertNew`) sends `insert`,
  refused whole (409, `CodeConflict`) where a document names an id a row holds. Both take `Returning`, `Require`
  and `Reason` among the documents; only `Insert` takes `IfAbsent`.
- `in` takes an inner query selecting one column, the list it answers: `Where("customer", "in",
  db.From("customers").Select("id").Where("country", "=", "TR"))`, or `fennecdb.Ops("in", ...)` in `Fields`, is
  `customer in (get customers select id where country = $1)`, in `Or`, `Not`, a lookup's `Where` and the filter
  of an `Update` or `Delete` too.
- `Order` takes an expression as a key: `Order(fennecdb.Expr("n * 2"), "desc")`, `Order(fennecdb.Distance("loc",
  []float64{13.4, 52.5}), "asc")`, its values bound where the order stands; one named with `.As` is refused.
- `Match(field, words, fennecdb.Fuzzy())` is `fuzzy` alone, the default edits (2 for a long word); `Fuzzy(1)`
  writes `fuzzy 1`. With `Fuse`, `Match` and `Near` are written in the order they were called.
- `Facet(field, Ranges(0, 25, 50))` counts by ranges of numbers, each value `[from, to]`; `Disjunctive()` counts past
  the filter's own condition on the field, as a shop's filter list does.
- Aggregates go in `Select` as FennecQL spells them (`"count(*)"`, `"sum(total)"`), and `Group` takes one key or
  more; `Bucket(field, "1m")`, `CountDistinct`, `First(field, by)`, `Last` and `Expr` make a column, named with
  `.As(name)`: `Select(fennecdb.Bucket("at", "1h").As("hour"), "count(*)").Group("hour")`.
- `Highlight(field, Tags(pre, post))` and `Snippet(field, words, Ellipsis("…"))` answer a `match`'s marks under
  `highlight(field)` and `snippet(field)`; `Facet(field, Top(n))` counts values over every matched row, which
  `q.Answer(ctx)` (or `db.QueryAnswer`, a `BatchItem`'s `Facets`) hands back beside the rows, `Facets.Of(field)`.
- `Order` and `Limit` before `Update` or `Delete` pick the rows it writes, and `Returning("*")` (or the fields)
  answers them in `Result.Rows`: a job queue's claim is
  `From("jobs").WhereCond(fennecdb.Raw("run_at <= now()")).Order("run_at", "asc").Limit(10).Update(ctx,
  fennecdb.D("owner", me, "run_at", fennecdb.Expr("now() + ?", 30000)), fennecdb.Returning("*"))`.
  Through `db.Wait(30 * time.Second)` a claim that finds no job is held at the server until one comes, or the
  wait ends with none (`Fennec-Wait`); the request's context gives it up.
- To see the text and parameters a chain builds, for logging or a test, `q.ToFennecQL()` returns them and runs
  nothing; a query needs no call to it before it runs.

A named statement (`create statement transfer(...)`) is called with `db.Call(ctx, name, args, opts...)`: it sends
`call <name> $1` over `/query` with the arguments as `$1`, so the transport, the errors, `IdempotencyKey`, `Wait`,
the context and the timeout are those of every statement. The arguments are an object of values by name -- a
`fennecdb.Params` map, a struct with `json` tags or a `D(...)` -- or `nil` for none; an empty map is one empty
argument. The answer is a `Result`, as `Exec`'s: the rows the body's last statement read or wrote with `returning`
in `Rows` (and a facet clause's counts in `Facets`), or what it wrote in `Affected`.

```go
r, err := db.IdempotencyKey("xfer-7").Call(ctx, "transfer", fennecdb.Params{"from": 1, "to": 2, "amount": 300})
var e *fennecdb.Error
if errors.As(err, &e) && e.Status == 412 { fmt.Println(e.Reason) } // "insufficient funds": the statement's own words
```

- A refusal is the usual `*fennecdb.Error`: 404 no such statement or not granted, 403 a claim the call lacks, 400 an
  argument missing, extra or of another type, 409 a broken statement or a unique value taken, 412 a `require` not
  met, its `Reason` the statement's.
- `fennecdb.Claims(fennecdb.Params{"sub": "alice"})` is `as {...}`, the claims a statement declares (`claims (sub
  text)`) that the server's own token makes the call with; a scoped token's claims are its own.
- `db.Read().Call(...)` marks a call that only reads: on a client made `WithReadsNearest` it goes to the read replica
  (`Fennec-Read: replica`) after the client's writes, and one that writes is refused there (403), never landed.
  Without `Read()` a call goes home, as every text not known to read does.
- `fennecdb.NewCall(name, args, opts...).ToCall()` returns the text and parameters and sends nothing, as
  `q.ToFennecQL()` does. Only the name goes into the text -- ASCII letters, digits and `_`, a letter first, at most 59
  bytes -- and an arguments value that is not an object is refused (`*fennecdb.QueryError`, nothing sent), both held
  to `integrations/builder-golden.json`.

`go test` runs against `fennec-server` processes it starts (`cargo build -p fennec-server`, or `FENNEC_SERVER`), and the
builder against every case of `integrations/builder-golden.json`.
Full reference: https://fennecdb.com/docs/languages

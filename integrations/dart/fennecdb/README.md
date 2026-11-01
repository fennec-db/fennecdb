# fennecdb for Dart

fennecdb embedded in a Dart or Flutter app: the database is a file on the
device, the engine the native library (`crates/fennec-ffi`) the server and the
browser module share their code and file format with -- on its own, or a
replica a server keeps in step.
In a Flutter app, depend on `fennecdb_flutter`, which bundles the library for
iOS, Android and macOS; elsewhere, point `Fennec.library` (or
`FENNEC_LIBRARY`) at `libfennec_ffi`.

```dart
import 'package:fennecdb/fennecdb.dart';

final db = await Fennec.open('${dir.path}/app.fennec');
await db.execute('create collection todos (title text, done bool @hash)');
await db.from('todos').insert({'title': 'milk', 'done': false});
final open = await db.from('todos').where('done', false).rows();
final near = await db.query(r'get notes near embed $1 limit 5', [embedding]); // a Float32List

db.live(db.from('todos').where('done', false)).listen((rows) => print(rows));
```

Every call runs on an isolate the database keeps for itself, since a call
may wait for the lock or an fsync and an FFI call blocks its isolate. A live
query is a Stream, run again after a write to a collection it reads, once a
burst. Every write is fsynced before it returns unless the file is opened
with `flags: Fennec.noSync`, which leaves the writes for `sync()`; `flush()`
hands them to the system, which outlives the app being killed. An open
leaves the hash, text, ordered and sparse indexes for their first read;
`warm(['docs'])` builds them on the database's isolate as the app starts,
so the first search does not (a `@text` index of 100 000 products: 141
ms), and answers how many it built. The builder's `facet(field, ranges:
[0, 25, 50])` counts by ranges of numbers, each value `[from, to]`, and
`disjunctive: true` counts past the filter's own condition on the field.

## Sync with a server

```dart
final db = await Fennec.openSynced(
  url: 'https://api.example.com', token: jwt,
  shapes: [Shape('todos', where: {'owner': me}, key: 'key')],
  path: '${dir.path}/todos.fennec',
  tokenProvider: refreshToken,
);
await db.replica!.ready();
```

The same `Fennec`: reads and live queries are the file's, a write to a
shape's collection shows at once and is sent -- queued in the file while
the server cannot be reached, under an idempotency key so it lands once --
and one the server refuses is put back and comes on `replica.refusals`, a
tenant's quota of bytes or rows among them (its `quota` named) and a
`require` not met (its `reason` named), the writes behind it going on; a
quota's 429 is waited out for the seconds it names.
`replica.status` says `online`, `offline` or `catchingUp`, the writes not
yet answered and the last error, whose `quota` tells a stream the quota of
streams refused from the network gone, and `statuses` streams it; call
`replica.resume()` on `AppLifecycleState.resumed`. `openSynced`, not
`sync`: Dart has no static and instance member of one name, and `sync()` is
the fsync. The requests and the stream are `dart:io`'s `HttpClient`'s, so
a server is reached through TLS. `Fennec.connect(url, token:)` is a server
with no file: every query a request, the same builder. Its `batch([...],
idempotencyKey: id)` runs the builder's `toInsert`/`toUpdate`/`toDelete` as
one block, all or none, answering a `BatchAnswer` (`results`, `seq`,
`replayed`); a `FennecException` that stops it carries `status` (412 and
`unmet` for a `require` not met) and `at`, the statement from 0. A
`require` says why with `reason` -- `update(patch, require: 1, reason:
'insufficient funds')`, a read's `require(1, reason: 'no such account')` --
written into the statement as `require 1 else '...'`, and the exception's
`reason`, the library's and a server's alike, tells one refusal from
another where `at` moves with every statement put in front. On a
tenant node a quota names itself (`FennecCode.quota`, and `quota`):
`"bytes"` and `"rows"` answer 507, and room is made by deleting;
`"streams"`, `"requests"`, `"writes"` and `"write_bytes"` answer 429 with
`retryAfter`, the wait the server asks for, as a tenant being moved or
failed over answers its writes 503.
`withIdempotencyKey(key)` is a copy whose every write -- `run`, the
builder's -- carries the key, so it lands once however often it is sent.
`Fennec.connect(url, token:, reads: Reads.nearest)` sends the builder's
reads, and an `asRead()` copy's, to the tenant's read replica near the app
through a fennec-shard router (`Fennec-Read: replica`), each after the
furthest position an answer from home named to the connection or any copy of
it (`Fennec-After`, `Fennec-History`); writes go home.

The query builder makes the text the JavaScript builder makes of the same
chain, to the byte: `integrations/builder-golden.json` holds the chains,
and `make dart-test` runs every one. To see the text and parameters a chain
builds, for logging or a test, `toFennecQL()` returns them and runs nothing.

## Named statements

A statement the server holds by name (`create statement transfer(...)`) is
called with `callStatement`, not `call`: a Dart method named `call` makes
its object callable as a function.

```dart
final db = Fennec.connect('https://api.example.com', token: serverToken);
final moved = await db.callStatement('transfer',
    args: {'from': 1, 'to': 2, 'amount': 300}, idempotencyKey: 'transfer-42');
final mine = await db.callStatement('history', claims: {'sub': 'alice'}, read: true);
```

It sends `call transfer $1` over `/query` -- the arguments one parameter, a
map of values by name, and only the name written into the text, checked
against letters, digits and `_` (a letter first, 59 bytes at most) -- and
answers as `run` does: the rows of a `get` or a `returning`, else `affected`
rows. `claims` is the `as`, the values a statement declaring `claims (sub
text)` reads as `$jwt.sub`, which the server's own token alone may name (a
scoped token's are its token's). `idempotencyKey` makes the call once and
`wait` is `withWait`'s, as a copy's would be (`db.withIdempotencyKey(k)
.callStatement(...)` too). `read: true` marks a call that only reads as
`asRead()` marks a text: on `Reads.nearest` it is answered by the read
replica near the app, after the connection's writes; without it a call goes
home, as every text not known to read does, and a call that writes sent
with `read` is refused (403), never landed. A refusal is a `FennecException`
by its `status`: 404 no such statement (or none this token may call), 403 a
claim missing, 400 an argument missing, extra or of another type, 409 a
broken statement or a unique value taken, 412 a `require` not met, with its
`reason`.

`Call('transfer', args: {...}, claims: {...}).toCall()` is the text and
parameters alone, offline -- a `Statement`, as the builder's `toInsert` is, so
a `batch` takes it -- held to the same `builder-golden.json`: `Call` takes
what a query's values take (a `DateTime` as its ISO text, a `Float32List` as
numbers), `args: {}` is still one argument (`call stamp $1`), `null` none, and
anything but a map, or a name that is not a name, is a `FennecException` of
code `builder`.

The embedded `Fennec` has `callStatement(name, args:, claims:)` too: the file's
own statements, called by the app with its signed-in user's `claims`, held to
the declared types as a token's are. It takes no `wait` and no key, those
being a server's, and a replica (`Fennec.openSynced`) refuses it -- it holds
its shapes and none of its server's statements -- as a `FennecException`
(`FennecCode.query`): call the server through `Fennec.connect`, and what the
call wrote reaches the replica through its shapes' streams.

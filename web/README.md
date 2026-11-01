# fennecdb for the browser

The engine as WebAssembly, and the client that drives it: `@fennecdb/web` on
npm, and the `fennec-web` bundle of each release.

| File | What it is |
| --- | --- |
| `fennec.js` | the client: the module's glue, persistence and the sync layer, a dependency-free ES module that imports and re-exports the two below |
| `builder.js`, `http.js` | the query builder and the HTTP client |
| `client.js` | the two of them, and nothing that could pull in the engine, sync or storage: `@fennecdb/web/client`, 10 KB brotli in an app's bundle |
| `fennec.d.ts`, `client.d.ts` | their types |
| `schema.js`, `schema.d.ts` | a schema declared in code, Drizzle's way: `@fennecdb/web/schema` |
| `fennec.wasm` | the engine with every index and the schema check: 275 KB brotli |
| `collate/` | the collation data the module fetches beside it, a chunk a group of scripts |

```js
import { Fennec } from './fennec.js';
const db = await Fennec.open('./fennec.wasm');
```

A page whose queries run on a server never fetches the module -- only
`Fennec.open` does -- and can import the client entry, which holds nothing of
the engine, the sync layer or storage (12 KB brotli bundled through
`@fennecdb/web`, 9 through it):

```js
import { connect } from '@fennecdb/web/client';  // or './client.js', beside builder.js and http.js
const db = connect('https://db.example.com', { token });
const rows = await db.from('docs').where('year', '>=', 2024).limit(10).rows();
const stop = db.live(db.from('docs').where('done', false), render);   // again after a write it may read
```

A named statement the server holds is called by name, its arguments one
parameter: `await db.call('transfer', { from: 1, to: 2, amount: 300 }, { idempotencyKey })`
sends `call transfer $1` and answers as `run` does; `{ read: true }` marks a
statement that only reads, which a client reading near has answered by a read
replica, `{ claims }` is `as {...}` for the server's token, and the builder's
`call(name, args).toCall()` is the text and parameters. A database in the page
refuses a call (a statement is a server's): call through `connect(url)`.

A refusal throws a `FennecError` with the server's `status`; a stopped
`batch` adds `at` and `completed`, and on a tenant node a quota names itself
(`quota`): `'bytes'` and `'rows'` answer 507, and room is made by deleting;
`'streams'`, `'requests'`, `'writes'` and `'write_bytes'` answer 429 with
`retryAfter`, the seconds the server asks to wait.

**From npm** the files are the same. A page serves the module and `collate/`
as static files and hands `Fennec.open` the module's URL -- with Vite, say:

```js
import { Fennec } from '@fennecdb/web';
import wasm from '@fennecdb/web/fennec.wasm?url';
const db = await Fennec.open(wasm, { collation: '/collate/' });   // collate/ copied into public/
```

Node reads both out of the package:

```js
import { readFile } from 'node:fs/promises';
import { Fennec } from '@fennecdb/web';
const file = (path) => readFile(new URL(import.meta.resolve(`@fennecdb/web/${path}`)));
const db = await Fennec.open(await file('fennec.wasm'), {
  collation: (name) => file(`collate/${name}.bin`),
});
```

**Which package.** A page whose queries run on a server imports
`@fennecdb/web/client` and loads no module; a database in the page, or a
synced replica, loads `fennec.wasm`, with every feature and the schema
check. A page that needs fewer can build its own module with them alone --
`make wasm FEATURES="text sorted"`, `FEATURES=none` for none (142 KB
brotli against 224), and `SCHEMA=0` leaves the schema check out (9.5 KB
brotli) -- and the file is the same either way: a store one build wrote
opens in the other. Without the graph a `near` measures every vector, as
`exact` does; without the text index a `match` throws a `FennecError`
naming it, and so does what needs any other feature a module lacks.

Serve `.wasm` as `application/wasm`, compressed once at build time.
Full reference: https://fennecdb.com/docs/javascript

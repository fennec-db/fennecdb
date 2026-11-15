// The page's part of a release's fixture set (tools/compat_fixtures.py):
// what that release's web client -- its web/fennec.js and the module its
// own `make wasm` built -- keeps in IndexedDB (`persist`: an image and the
// journal's chunks, in the clear and sealed with a `cryptoKey`) and in an
// OPFS file (`openFile`), a replica's sync state and a migration among it,
// and the answers its page gave to a list of statements over each, read
// back as a page opened again reads them.
//
//   node tools/compat/web.mjs <the release's web dir> <out dir> <statements> <sync url> <token>
//
// Node has neither store, so the stores below stand in for them: the part
// of each API the client uses, holding what it is given as it is given.
// web/fennec.compat.test.js hands the same records to HEAD's client
// through them, the IndexedDB stand-in holding only the database, version
// and store the release's page made: a client that names another is
// refused, as a browser's would come back empty.

import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

export class KeyRange {
  constructor(lo, hi) {
    this.lo = lo;
    this.hi = hi;
  }
  static bound(lo, hi) {
    return new KeyRange(lo, hi);
  }
  has(k) {
    return k >= this.lo && k <= this.hi;
  }
}

const refused = (name, message) => Object.assign(new Error(message), { name });

/**
 * IndexedDB's databases, each its version and stores, keys kept sorted as
 * the real one keeps them. Empty, it takes what a page makes -- a database
 * opened at a version, its stores made in `onupgradeneeded` -- as the
 * release's page wrote into it. Given `held`, the database the release's
 * page left (`{name, version, store, records}`), it holds that alone: a
 * database of another name or version, or a store of another name, is
 * refused, never made.
 */
export function fakeIndexedDB(held = null) {
  const dbs = new Map();
  if (held) dbs.set(held.name, { version: held.version, stores: new Map([[held.store, held.records]]) });
  const request = (fn) => {
    const req = {};
    queueMicrotask(() => {
      try {
        req.result = fn();
        req.onsuccess?.();
      } catch (e) {
        req.error = e;
        req.onerror?.();
      }
    });
    return req;
  };
  const storeOf = (records) => ({
    put: (value, key) => records.set(key, value),
    delete(range) {
      for (const k of [...records.keys()]) if (range.has(k)) records.delete(k);
    },
    get: (key) => request(() => records.get(key)),
    getAll: (range) =>
      request(() =>
        [...records.keys()]
          .filter((k) => range.has(k))
          .sort()
          .map((k) => records.get(k)),
      ),
  });
  const handle = (d, upgrading) => ({
    get objectStoreNames() {
      return { contains: (s) => d.stores.has(s) };
    },
    createObjectStore(s) {
      if (!upgrading) throw refused('InvalidStateError', `createObjectStore(${s}) outside an upgrade`);
      if (d.stores.has(s)) throw refused('ConstraintError', `the store ${s} exists`);
      d.stores.set(s, new Map());
    },
    transaction(names) {
      const scope = [names].flat();
      for (const s of scope) {
        if (!d.stores.has(s)) throw refused('NotFoundError', `no store ${s}: the release's page made ${[...d.stores.keys()]}`);
      }
      const tx = {
        objectStore(s) {
          if (!scope.includes(s)) throw refused('NotFoundError', `${s} is not in the transaction's scope`);
          return storeOf(d.stores.get(s));
        },
      };
      setImmediate(() => tx.oncomplete?.());
      return tx;
    },
    close() {},
  });
  return {
    dbs,
    open(name, version) {
      const req = {};
      queueMicrotask(() => {
        let d = dbs.get(name);
        if (held && (!d || version !== d.version)) {
          req.error = refused(
            'VersionError',
            d ? `${name} is at version ${d.version}, not ${version}` : `no database ${name}: the release's page made ${held.name}`,
          );
          req.onerror?.();
          return;
        }
        if (!d || version > d.version) {
          d ??= { version, stores: new Map() };
          d.version = version;
          dbs.set(name, d);
          req.result = handle(d, true);
          req.onupgradeneeded?.();
        }
        req.result = handle(d, false);
        req.onsuccess?.();
      });
      return req;
    },
  };
}

/** The origin private file system's directory: files by name, one handle each at a time. */
export function fakeDir(files = new Map()) {
  return {
    files,
    async getFileHandle(name, { create = false } = {}) {
      if (!files.has(name)) {
        if (!create) throw Object.assign(new Error(name), { name: 'NotFoundError' });
        files.set(name, { bytes: new Uint8Array(0), open: false });
      }
      const f = files.get(name);
      const resize = (n) => {
        const b = new Uint8Array(n);
        b.set(f.bytes.subarray(0, Math.min(n, f.bytes.length)));
        f.bytes = b;
      };
      return {
        async createSyncAccessHandle() {
          if (f.open) throw Object.assign(new Error('locked'), { name: 'NoModificationAllowedError' });
          f.open = true;
          return {
            getSize: () => f.bytes.length,
            read(buf, { at = 0 } = {}) {
              const n = Math.max(0, Math.min(buf.length, f.bytes.length - at));
              buf.set(f.bytes.subarray(at, at + n));
              return n;
            },
            write(buf, { at = 0 } = {}) {
              if (at + buf.length > f.bytes.length) resize(at + buf.length);
              f.bytes.set(buf, at);
              return buf.length;
            },
            truncate: resize,
            flush() {},
            close() {
              f.open = false;
            },
          };
        },
      };
    },
  };
}

const b64 = (bytes) => Buffer.from(bytes).toString('base64');
const unb64 = (s) => new Uint8Array(Buffer.from(s, 'base64'));

/** A store's records as JSON: bytes in base64, a sealed record's parts each. */
export function dumpRecords(records) {
  return [...records.keys()].sort().map((key) => {
    const v = records.get(key);
    if (v instanceof Uint8Array) return { key, bytes: b64(v) };
    return { key, ...(v.gen != null ? { gen: v.gen } : {}), iv: b64(v.iv), data: b64(v.data) };
  });
}

/** The records [`dumpRecords`] wrote, as the store held them. */
export function loadRecords(list) {
  return new Map(
    list.map((r) => [
      r.key,
      r.bytes != null ? unb64(r.bytes) : { ...(r.gen != null ? { gen: r.gen } : {}), iv: unb64(r.iv), data: unb64(r.data) },
    ]),
  );
}

/** A `page-idb*.json` file read: the database it names, as `fakeIndexedDB` holds one. */
export function heldDatabase(json) {
  return { name: json.database, version: json.version, store: json.store, records: loadRecords(json.records) };
}

/** The raw key a sealed page's records are sealed with: the fixture's own, never a secret. */
export const PAGE_KEY = new Uint8Array(32).map((_, i) => (i * 37 + 11) & 255);

export const sealingKey = () => crypto.subtle.importKey('raw', PAGE_KEY, 'AES-GCM', false, ['encrypt', 'decrypt']);

/** Each statement of `text`, a line each, `--` lines and blank ones passed over. */
export const statements = (text) => text.split('\n').filter((l) => l.trim() && !l.startsWith('--'));

/** The page's answers to `list`, a line each: what `run` returned, or the error's message. */
export function answers(db, list) {
  return list
    .map((q) => {
      try {
        return JSON.stringify(db.run(q));
      } catch (e) {
        return JSON.stringify({ error: e.message });
      }
    })
    .join('\n') + '\n';
}

/** A collection whose rows make the image large beside the parts after it. */
const FILLER = Array.from({ length: 30 }, (_, n) => `{n: ${n}, body: "${`filler row ${n} `.repeat(8)}"}`);

/**
 * What the page writes, in three parts: a persist after each. Each part
 * after the first is far smaller than the image, so `persist` keeps it as a
 * chunk rather than folding a new image (`FOLD`).
 */
const WRITES = [
  [
    'create collection filler (n int @hash, body text)',
    `put filler [${FILLER.join(', ')}]`,
    'create collection kinds (b bool, i int @hash, f float @sorted, t text @text, by bytes, ts timestamp @sorted, ' +
      'v vector<4> @hnsw(cosine, m=8), vh vector<4, f16>, l [int], sp sparse<8> @inverted, j json, g geo @geo, ' +
      'tr text collate tr @sorted, email text nocase @unique, tag text @hash @text, exp timestamp @ttl(1d), a int, z int, @sorted(a, z))',
    'create collection notes (n int @hash, body text @text(accents))',
    'put kinds {b: true, i: 1, f: 1.5, t: "the quick brown fox", by: [0, 159, 255], ts: "2026-01-01T00:00:00Z", ' +
      'v: [1, 0, 0, 0], vh: [0.5, 0.25, 0, 1], l: [3, 1, 2], sp: "{1:0.5,6:2}/8", j: {kind: "a", n: 1, deep: {x: [1, "two", null, true]}}, ' +
      'g: [13.4, 52.5], tr: "ılık", email: "Alice@Example.com", tag: "red fox", exp: "2099-01-01T00:00:00Z", a: 1, z: 10}',
    'put kinds {b: false, i: 2, f: -2.25, t: "lazy dogs sleep", by: "text as bytes", ts: 1767225600000, v: [0, 1, 0, 0], ' +
      'vh: [0, 1, 0, 0], l: [], sp: "{2:1}/8", j: [1, 2.5, "x"], g: [2.35, 48.86], tr: "Istanbul", email: "bob@example.com", ' +
      'tag: "blue whale", exp: "2000-01-01T00:00:00Z", a: 1, z: 20}',
    'put kinds {i: 3}',
    'put notes [{n: 1, body: "café crème"}, {n: 2, body: "cafe au lait"}]',
  ],
  [
    'put kinds {b: true, i: 9007199254740993, f: 0.1, t: "a fox again", v: [0, 0, 1, 0], j: {kind: "b", big: 9007199254740993}, a: 2, z: 1}',
    'set kinds {f: 9.5, tag: "green fox"} where i = 2',
    'del notes where n = 2',
    'create index on kinds (j.kind) @hash',
  ],
  [
    'alter collection kinds add field note text',
    'set kinds {note: "added after"} where i = 1',
    'put notes {n: 3, body: "naïve"}',
    'del kinds where i = 3',
  ],
];

async function page(web, wasm) {
  const { Fennec } = await import(pathToFileURL(join(web, 'fennec.js')).href);
  const collation = (name) => readFile(join(web, 'collate', `${name}.bin`));
  const db = await Fennec.open(wasm, { collation });
  await db.collation('all');
  return db;
}

/**
 * The page's database after its first part: a migration applied by the
 * declared schema (`_migrations`), and a replica of the server's `tasks`
 * seeded, then a write made offline -- queued, never sent, an optimistic
 * row under a temporary id (`_sync_shapes`, `_sync_queue`, `_sync_temps`).
 */
async function declaredAndSynced(web, db, url, token) {
  const { sync } = await import(pathToFileURL(join(web, 'fennec.js')).href);
  // The module reads a description as its FennecQL text.
  const description = db.describe('fennecql');
  const plan = db.checkSchema({ ...description, migrations: ['put notes {n: 9, body: "migrated"}'] }, 'apply');
  if (!plan.applied) throw new Error(`the migration was not applied: ${JSON.stringify(plan)}`);
  const replica = await sync({ url, token, local: db, shapes: [{ collection: 'tasks' }], leader: false });
  await replica.ready();
  replica.setOnline(false);
  replica.from('tasks').insert({ title: 'written offline', done: false }).catch(() => {});
  await new Promise((r) => setImmediate(r));
  if (!db.run('get _sync_queue').rows?.length) throw new Error('the offline write was not queued');
  replica.close();
}

async function main([web, out, statementsFile, url, token]) {
  const wasm = await readFile(join(web, 'fennec.wasm'));
  const reads = statements(await readFile(statementsFile, 'utf8'));
  const { persist, restore, openFile } = await import(pathToFileURL(join(web, 'fennec.js')).href);
  const idb = fakeIndexedDB();
  globalThis.indexedDB = idb;
  globalThis.IDBKeyRange = KeyRange;
  const key = await sealingKey();

  // IndexedDB: the image at the first persist, then a chunk a persist.
  for (const [name, opts] of [
    ['page-idb', {}],
    ['page-idb-sealed', { cryptoKey: key }],
  ]) {
    const db = await page(web, wasm);
    for (const [i, part] of WRITES.entries()) {
      for (const q of part) db.run(q);
      if (i === 1) await declaredAndSynced(web, db, url, token);
      await persist(db, name, opts);
    }
    db.close();
  }
  if (idb.dbs.size !== 1) throw new Error(`the page made ${idb.dbs.size} databases, not one`);
  const [[database, made]] = [...idb.dbs];
  if (made.stores.size !== 1) throw new Error(`the page made ${made.stores.size} stores, not one`);
  const [[store, records]] = [...made.stores];
  const dumped = dumpRecords(records);
  for (const name of ['page-idb', 'page-idb-sealed']) {
    const mine = dumped.filter((r) => r.key === name || r.key.startsWith(`${name}#`));
    if (mine.length < 3) throw new Error(`${name}: ${mine.length} records, not an image and its chunks`);
    const json = { database, version: made.version, store, records: mine };
    await writeFile(join(out, `${name}.json`), JSON.stringify(json, null, 1) + '\n');
    const back = await page(web, wasm);
    if (!(await restore(back, name, name.endsWith('sealed') ? { cryptoKey: key } : {}))) throw new Error(`${name}: nothing restored`);
    await writeFile(join(out, `${name}.web`), answers(back, reads));
    back.close();
  }

  // OPFS: every statement's writes flushed into the file as it answers.
  const dir = fakeDir();
  const db = await page(web, wasm);
  const file = await openFile(db, 'page.fennec', { dir });
  for (const [i, part] of WRITES.entries()) {
    for (const q of part) await db.query(q);
    if (i === 1) await declaredAndSynced(web, db, url, token);
  }
  file.close();
  db.close();
  const bytes = dir.files.get('page.fennec').bytes;
  await writeFile(join(out, 'page-opfs.fennec'), bytes);
  const again = await page(web, wasm);
  const reopened = await openFile(again, 'page.fennec', { dir });
  await writeFile(join(out, 'page-opfs.web'), answers(again, reads));
  reopened.close();
  again.close();
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main(process.argv.slice(2));
}

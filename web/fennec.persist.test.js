// Incremental persistence: `persist` stores the image once and then only the
// writes since, and `restore` reads the image and its chunks back as one
// file. Node has no IndexedDB, so a small in-memory one stands in for it --
// the part of the API `persist`, `restore` and the sync layer's cursors use,
// keys kept sorted as the real one keeps them.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { needs } from './ci-prereq.mjs';

const wasm = await readFile(new URL('./fennec.wasm', import.meta.url)).catch(() => null);
needs(['web/fennec.wasm', wasm, 'make wasm']);

class KeyRange {
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

/**
 * One store's records, every transaction that wrote to them, and the
 * connections left open. A transaction's writes land as it commits; with
 * `refuse` set to an error's name, the next one that writes is aborted
 * there instead, its writes gone, as a real one past its quota is: by
 * `abort` alone.
 */
function fakeIndexedDB() {
  const records = new Map();
  const log = [];
  let connections = 0;
  const request = (fn) => {
    const req = {};
    queueMicrotask(() => {
      req.result = fn();
      req.onsuccess?.();
    });
    return req;
  };
  const store = (writes) => ({
    put(value, key) {
      writes.push(['put', key, value.length ?? 0, () => records.set(key, value)]);
    },
    delete(range) {
      writes.push(['delete', `${range.lo}..`, 0, () => {
        for (const k of [...records.keys()]) if (range.has(k)) records.delete(k);
      }]);
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
  const idb = {
    records,
    log,
    refuse: null,
    /** Connections opened and not closed. */
    get connections() {
      return connections;
    },
    open() {
      const req = {};
      queueMicrotask(() => {
        connections++;
        req.result = db;
        req.onsuccess?.();
      });
      return req;
    },
  };
  const db = {
    transaction() {
      const writes = [];
      const tx = { objectStore: () => store(writes) };
      setTimeout(() => {
        if (writes.length && idb.refuse) {
          tx.error = Object.assign(new Error(`refused: ${idb.refuse}`), { name: idb.refuse });
          idb.refuse = null;
          return tx.onabort?.();
        }
        for (const w of writes) w.pop()();
        if (writes.length) log.push(writes);
        tx.oncomplete?.();
      });
      return tx;
    },
    close() {
      connections--;
    },
  };
  return idb;
}

test('persist stores the image once, then only what was written since', { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
  const idb = fakeIndexedDB();
  globalThis.indexedDB = idb;
  globalThis.IDBKeyRange = KeyRange;
  const { Fennec, persist, restore } = await import('./fennec.js');

  const db = await Fennec.open(wasm);
  db.run('create collection t (n int, body text)');
  const filler = 'x'.repeat(200);
  for (let i = 0; i < 500; i++) db.run('put t {n: $1, body: $2}', [i, filler]);

  const first = await persist(db, 'k');
  assert.ok(first > 100_000, `the image first: ${first} bytes`);
  assert.equal(idb.log.at(-1)[0][0], 'put');

  // One row: a chunk of that row's size, not the image again.
  db.run('put t {n: 9999, body: "one"}');
  const one = await persist(db, 'k');
  assert.ok(one > 0 && one < 200, `one row wrote ${one} bytes`);
  assert.deepEqual(idb.log.at(-1).map((w) => w[1]), ['k#000000001']);
  assert.equal(await persist(db, 'k'), 0, 'nothing written, nothing stored');

  db.run('set t {body: "changed"} where n = 3');
  db.run('del t where n = 4');
  await persist(db, 'k');
  assert.deepEqual(idb.log.at(-1).map((w) => w[1]), ['k#000000002']);

  // Image and chunks read back as one file give the same database.
  const back = await Fennec.open(wasm);
  assert.equal(await restore(back, 'k'), true);
  const rows = (d) => d.rows('get t select n, body order n');
  assert.deepEqual(rows(back), rows(db));

  // A restored database goes on adding chunks where the stored ones end.
  back.run('put t {n: 10000}');
  await persist(back, 'k');
  assert.deepEqual(idb.log.at(-1).map((w) => w[1]), ['k#000000003']);
  const again = await Fennec.open(wasm);
  await restore(again, 'k');
  assert.deepEqual(rows(again), rows(back));

  // `compact` rewrites the image: it replaces what was stored, chunks gone.
  back.run('compact');
  back.run('put t {n: 10001}');
  await persist(back, 'k');
  assert.deepEqual(idb.log.at(-1).map((w) => w[0]), ['put', 'delete']);
  assert.ok(![...idb.records.keys()].some((k) => k.startsWith('k#')));
  const third = await Fennec.open(wasm);
  await restore(third, 'k');
  assert.deepEqual(rows(third), rows(back));

  // Once the chunks outgrow half the image, a new image replaces them.
  for (let i = 0; i < 300; i++) {
    third.run('put t {n: $1, body: $2}', [20000 + i, filler]);
    await persist(third, 'k');
  }
  assert.ok(idb.log.some((w) => w[0][0] === 'put' && w[0][1] === 'k' && w[1]?.[0] === 'delete'));
  const fourth = await Fennec.open(wasm);
  await restore(fourth, 'k');
  assert.deepEqual(rows(fourth), rows(third));
});

// Left open, each call's connection stayed until the collector came for it,
// and a delete of the database -- or another page's upgrade of it -- waited
// on every one (web/browser/ found it in Chrome).
test('persist, restore and the state calls close the connections they open', { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
  const idb = fakeIndexedDB();
  globalThis.indexedDB = idb;
  globalThis.IDBKeyRange = KeyRange;
  const { Fennec, persist, restore, putState, getState } = await import('./fennec.js');
  const db = await Fennec.open(wasm);
  db.run('create collection t (n int)');
  await persist(db, 'c');
  db.run('put t {n: 1}');
  await persist(db, 'c');
  assert.equal(await persist(db, 'c'), 0, 'nothing written, nothing stored');
  assert.equal(await restore(await Fennec.open(wasm), 'c'), true);
  assert.equal(await restore(await Fennec.open(wasm), 'none'), false);
  await putState('c-state', { at: 1 });
  assert.deepEqual(await getState('c-state'), { at: 1 });
  assert.equal(idb.connections, 0);
});

// A transaction past its quota is refused at its commit, by `abort` alone:
// `putState`, waiting on `error` and `complete`, never answered (Chrome,
// web/browser/).
test('a write refused at its commit is refused to its caller, what was stored standing', { skip: wasm ? false : 'no web/fennec.wasm (make wasm)', timeout: 10_000 }, async () => {
  const idb = fakeIndexedDB();
  globalThis.indexedDB = idb;
  globalThis.IDBKeyRange = KeyRange;
  const { Fennec, persist, restore, putState, getState } = await import('./fennec.js');
  const db = await Fennec.open(wasm);
  db.run('create collection t (n int)');
  db.run('put t {n: 1}');
  await persist(db, 'q');
  idb.refuse = 'QuotaExceededError';
  await assert.rejects(putState('q-state', 'x'), { name: 'QuotaExceededError' });
  assert.equal(await getState('q-state'), undefined);
  db.run('put t {n: 2}');
  idb.refuse = 'QuotaExceededError';
  await assert.rejects(persist(db, 'q'), { name: 'QuotaExceededError' });
  const ns = (d) => d.rows('get t select n order n').map((r) => r.n);
  const back = await Fennec.open(wasm);
  await restore(back, 'q');
  assert.deepEqual(ns(back), [1], 'what was stored stands');
  // The refused chunk's writes are nowhere else: the next persist stores
  // the image whole.
  await persist(db, 'q');
  assert.deepEqual(idb.log.at(-1).map((w) => w[0]), ['put', 'delete']);
  const again = await Fennec.open(wasm);
  await restore(again, 'q');
  assert.deepEqual(ns(again), [1, 2]);
  assert.equal(idb.connections, 0);
});

test('drain hands over frames, or an image after a rewrite', { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
  const { Fennec } = await import('./fennec.js');
  const db = await Fennec.open(wasm);
  db.run('create collection t (n int)');
  // No journal started: nothing is kept.
  db.run('put t {n: 1}');
  assert.deepEqual(db.drain(), { replace: false, bytes: new Uint8Array(0) });

  db.journal();
  const base = db.snapshot();
  db.run('put t {n: 2}');
  const { replace, bytes } = db.drain();
  assert.equal(replace, false);
  const file = new Uint8Array(base.length + bytes.length);
  file.set(base);
  file.set(bytes, base.length);
  const copy = await Fennec.open(wasm);
  copy.load(file);
  assert.deepEqual(copy.rows('get t select n order n'), db.rows('get t select n order n'));
  assert.equal(db.drain().bytes.length, 0, 'drained once');

  db.run('compact');
  db.run('put t {n: 3}');
  const after = db.drain();
  assert.equal(after.replace, true);
  const fresh = await Fennec.open(wasm);
  fresh.load(after.bytes);
  assert.deepEqual(fresh.rows('get t select n order n'), db.rows('get t select n order n'));
});

test('a sealed database restores with its key and nothing else', { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
  const idb = fakeIndexedDB();
  globalThis.indexedDB = idb;
  globalThis.IDBKeyRange = KeyRange;
  const { Fennec, persist, restore } = await import('./fennec.js');
  const aes = () => crypto.subtle.generateKey({ name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
  const cryptoKey = await aes();

  const db = await Fennec.open(wasm);
  db.run('create collection t (n int, body text)');
  for (let i = 0; i < 200; i++) db.run('put t {n: $1, body: "secret-text"}', [i]);
  await persist(db, 's', { cryptoKey });
  db.run('put t {n: 1000, body: "secret-chunk-one"}');
  await persist(db, 's', { cryptoKey });
  db.run('put t {n: 1001, body: "secret-chunk-two"}');
  await persist(db, 's', { cryptoKey });

  // Nothing stored holds the text in the clear.
  const plainIn = (rec) => Buffer.from(rec.data).includes('secret');
  assert.equal([...idb.records.values()].some(plainIn), false);
  assert.deepEqual([...idb.records.keys()].sort(), ['s', 's#000000001', 's#000000002']);

  const rows = (d) => d.rows('get t select n, body order n');
  const back = await Fennec.open(wasm);
  assert.equal(await restore(back, 's', { cryptoKey }), true);
  assert.deepEqual(rows(back), rows(db));

  const refused = async (re) => assert.rejects(restore(await Fennec.open(wasm), 's', { cryptoKey }), re);
  await assert.rejects(restore(await Fennec.open(wasm), 's'), /sealed: restore needs its cryptoKey/);
  await assert.rejects(restore(await Fennec.open(wasm), 's', { cryptoKey: await aes() }), /does not open/);

  // A byte changed, two chunks swapped, a chunk from the image before.
  const saved = new Map(idb.records);
  const put = (k, v) => idb.records.set(k, v);
  const one = saved.get('s#000000001');
  const two = saved.get('s#000000002');
  const flipped = { ...two, data: two.data.slice() };
  flipped.data[3] ^= 1;
  put('s#000000002', flipped);
  await refused(/record 2 does not open/);
  put('s#000000001', two);
  put('s#000000002', one);
  await refused(/record 1 does not open/);
  idb.records.clear();
  for (const [k, v] of saved) put(k, v);

  // A new image gets a generation of its own, and the old chunk is refused.
  db.run('compact');
  await persist(db, 's', { cryptoKey });
  assert.equal(idb.records.has('s#000000001'), false);
  put('s#000000001', one);
  await refused(/record 1 does not open/);
  idb.records.delete('s#000000001');
  const again = await Fennec.open(wasm);
  await restore(again, 's', { cryptoKey });
  assert.deepEqual(rows(again), rows(db));

  // A record stored in the clear is not taken for a sealed one.
  await persist(db, 'clear');
  await assert.rejects(restore(await Fennec.open(wasm), 'clear', { cryptoKey }), /stored in the clear/);
});

// A page that keeps its state in the database asks its live queries before
// the restore answers: they are handed the restored rows once it lands.
test('restore runs the live queries asked before it', { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
  globalThis.indexedDB = fakeIndexedDB();
  globalThis.IDBKeyRange = KeyRange;
  const { Fennec, persist, restore } = await import('./fennec.js');
  const db = await Fennec.open(wasm);
  db.run('create collection todos (title text)');
  db.run('put todos {title: "kept"}');
  await persist(db, 'state');

  const page = await Fennec.open(wasm);
  const seen = [];
  page.live('get todos', (rows) => seen.push(rows.map((r) => r.title)), {
    collections: ['todos'],
    onError: (e) => seen.push(e.message.includes('todos') ? 'none yet' : e.message),
  });
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(await restore(page, 'state'), true);
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(seen, ['none yet', ['kept']]);
  // And on from there, kept as before.
  page.run('put todos {title: "new"}');
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(seen.at(-1), ['kept', 'new']);
  assert.ok((await persist(page, 'state')) > 0);
});

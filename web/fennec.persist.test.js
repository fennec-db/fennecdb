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
 * connections opened. A transaction's writes land as it commits; with
 * `refuse` set to an error's name, the next one that writes is aborted
 * there instead, its writes gone, as a real one past its quota is: by
 * `abort` alone. A delete asks every open connection to let go
 * (`versionchange`), and is `blocked` while one holds on.
 */
function fakeIndexedDB() {
  const records = new Map();
  const log = [];
  const open = new Set();
  let opened = 0;
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
    /** Connections opened, and those not closed. */
    get opened() {
      return opened;
    },
    get connections() {
      return open.size;
    },
    open() {
      const req = {};
      queueMicrotask(() => {
        const db = {
          transaction() {
            if (!open.has(db)) throw Object.assign(new Error('the connection is closed'), { name: 'InvalidStateError' });
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
            open.delete(db);
          },
        };
        opened++;
        open.add(db);
        req.result = db;
        req.onsuccess?.();
      });
      return req;
    },
    deleteDatabase() {
      const req = {};
      setTimeout(() => {
        for (const db of [...open]) db.onversionchange?.({});
        if (open.size) return req.onblocked?.();
        records.clear();
        req.onsuccess?.();
      });
      return req;
    },
    /** Every connection closed as Firefox closes one after a write past its quota: `close` told later. */
    invalidate() {
      const gone = [...open];
      open.clear();
      return () => {
        for (const db of gone) db.onclose?.({});
      };
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

// Each call opened a connection of its own, left open until the collector
// came for it, and a delete of the database -- or another page's upgrade of
// it -- waited on every one (web/browser/ found it in Chrome). Closed after
// each call instead, a call after the page sat idle 3 s took 8 to 10 ms
// reopening it (a `persist` of one row: 8.1 ms in Chrome, 10.2 in Firefox,
// medians; docs/design/js-client.md#in-real-browsers).
test('persist, restore and the state calls share one connection, which lets a delete of the database go ahead', { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
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
  assert.equal(idb.opened, 1, 'one connection for every call');

  const deleted = await new Promise((res) => {
    const req = idb.deleteDatabase('fennecdb');
    req.onsuccess = () => res('deleted');
    req.onblocked = () => res('blocked');
  });
  assert.equal(deleted, 'deleted');
  assert.equal(idb.connections, 0);
  // The next call opens another, and finds the store empty.
  assert.equal(await getState('c-state'), undefined);
  await putState('c-state', { at: 2 });
  assert.deepEqual(await getState('c-state'), { at: 2 });
  assert.equal(idb.opened, 2);
});

// The store deleted under a page that had persisted -- a script's
// `indexedDB.deleteDatabase('fennecdb')`, the browser clearing the site's
// data -- took the image, and `persist`, still believing it stored, went
// on storing chunks: `restore` found no image, and everything the page
// persisted after the delete was lost.
test('a store deleted under the page has its image stored again by the next persist', { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
  const idb = fakeIndexedDB();
  globalThis.indexedDB = idb;
  globalThis.IDBKeyRange = KeyRange;
  const { Fennec, persist, restore } = await import('./fennec.js');
  const ns = (d) => d.rows('get t select n order n').map((r) => r.n);
  const restored = async (key, opts) => {
    const back = await Fennec.open(wasm);
    assert.equal(await restore(back, key, opts), true, `${key} restores`);
    return ns(back);
  };
  const lastWrite = () => idb.log.at(-1).map((w) => [w[0], w[1]]);
  const deleteDatabase = () =>
    new Promise((res) => {
      const req = idb.deleteDatabase('fennecdb');
      req.onsuccess = () => res('deleted');
      req.onblocked = () => res('blocked');
    });
  const db = await Fennec.open(wasm);
  db.run('create collection t (n int)');
  db.run('put t {n: 1}');
  await persist(db, 'g');
  db.run('put t {n: 2}');
  await persist(db, 'g');
  assert.deepEqual([...idb.records.keys()].sort(), ['g', 'g#000000001']);

  // A delete asks the connection to let go (`versionchange`), then the
  // store is gone.
  assert.equal(await deleteDatabase(), 'deleted');
  db.run('put t {n: 3}');
  await persist(db, 'g');
  assert.deepEqual(lastWrite(), [['put', 'g'], ['delete', 'g#..']], 'the image, not a chunk');
  assert.deepEqual(await restored('g'), [1, 2, 3]);
  // Nothing written since the delete, the image is stored again all the same.
  assert.equal(await deleteDatabase(), 'deleted');
  assert.ok((await persist(db, 'g')) > 0);
  assert.deepEqual(await restored('g'), [1, 2, 3]);

  // The browser clearing the site's data closes the connection (a forced
  // close: its requests refused at once, `close` told after). A persist in
  // between is refused by it and goes to another: there, an image.
  db.run('put t {n: 4}');
  await persist(db, 'g');
  assert.deepEqual(lastWrite(), [['put', 'g#000000001']]);
  let told = idb.invalidate();
  idb.records.clear();
  db.run('put t {n: 5}');
  await persist(db, 'g');
  told();
  assert.deepEqual(lastWrite(), [['put', 'g'], ['delete', 'g#..']]);
  assert.deepEqual(await restored('g'), [1, 2, 3, 4, 5]);

  // The connection gone while a chunk is being sealed: the chunk is
  // stored over no other, the image is.
  const cryptoKey = await crypto.subtle.generateKey({ name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
  await persist(db, 's', { cryptoKey });
  db.run('put t {n: 6}');
  await persist(db, 's', { cryptoKey });
  assert.deepEqual(lastWrite(), [['put', 's#000000001']]);
  db.run('put t {n: 7}');
  const sealing = persist(db, 's', { cryptoKey });
  told = idb.invalidate();
  idb.records.clear();
  await sealing;
  told();
  assert.deepEqual(lastWrite(), [['put', 's'], ['delete', 's#..']]);
  assert.deepEqual(await restored('s', { cryptoKey }), [1, 2, 3, 4, 5, 6, 7]);
});

// A second persist called before the first had answered read the chunk
// number the first had not moved past yet, and stored its chunk over the
// first's: the writes the first drained were nowhere, and sealed, the chunk
// after them refused to open.
test("persists of one database run one after another, in call order, each where the one before left", { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
  const idb = fakeIndexedDB();
  globalThis.indexedDB = idb;
  globalThis.IDBKeyRange = KeyRange;
  const { Fennec, persist, restore } = await import('./fennec.js');
  const ns = (d) => d.rows('get t select n order n').map((r) => r.n);
  const restored = async (key, opts) => {
    const back = await Fennec.open(wasm);
    assert.equal(await restore(back, key, opts), true, `${key} restores`);
    return ns(back);
  };
  const cryptoKey = await crypto.subtle.generateKey({ name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
  for (const [key, opts] of [['plain', {}], ['sealed', { cryptoKey }]]) {
    const db = await Fennec.open(wasm);
    // An image large enough that no chunk here folds into a new one.
    db.run('create collection pad (body text)');
    db.run('put pad {body: $1}', ['x'.repeat(4096)]);
    db.run('create collection t (n int)');
    db.run('put t {n: 1}');
    await persist(db, key, opts);
    const from = idb.log.length;
    const written = () => idb.log.slice(from).map((tx) => tx.map((w) => w[1]));

    // The second called while the first is out, a write between.
    db.run('put t {n: 2}');
    const a = persist(db, key, opts);
    db.run('put t {n: 3}');
    const b = persist(db, key, opts);
    const [first, second] = await Promise.all([a, b]);
    assert.ok(first > 0 && second > 0, `${key}: each stores a chunk (${first}, ${second} bytes)`);
    assert.deepEqual(written(), [[`${key}#000000001`], [`${key}#000000002`]], `${key}: the chunks in call order`);
    assert.deepEqual(await restored(key, opts), [1, 2, 3]);

    // A third called while two are out. The second runs once the first
    // has answered, and stores both writes made by then; the third finds
    // nothing left.
    const calls = [];
    for (const n of [4, 5, 6]) {
      db.run('put t {n: $1}', [n]);
      calls.push(persist(db, key, opts));
    }
    const [c, d, e] = await Promise.all(calls);
    assert.ok(c > 0 && d > 0, `${key}: the first two store a chunk each (${c}, ${d} bytes)`);
    assert.equal(e, 0, `${key}: the one before stored the write made before it`);
    assert.deepEqual(written().slice(2), [[`${key}#000000003`], [`${key}#000000004`]], `${key}: two more, in call order`);
    assert.deepEqual(await restored(key, opts), [1, 2, 3, 4, 5, 6]);
  }
});

// A refused persist leaves no number taken and no turn held: the one called
// after it runs, stores what the refused one could not -- an image -- and
// answers for itself. Before, it stored a chunk over the image the refused
// writes were missing from.
test('a persist refused while another waits for it lets that one run, which stores the image', { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
  const idb = fakeIndexedDB();
  globalThis.indexedDB = idb;
  globalThis.IDBKeyRange = KeyRange;
  const { Fennec, persist, restore } = await import('./fennec.js');
  const ns = (d) => d.rows('get t select n order n').map((r) => r.n);
  const db = await Fennec.open(wasm);
  db.run('create collection t (n int)');
  db.run('put t {n: 1}');
  await persist(db, 'w');
  db.run('put t {n: 2}');
  idb.refuse = 'QuotaExceededError';
  const a = persist(db, 'w');
  db.run('put t {n: 3}');
  const b = persist(db, 'w');
  await assert.rejects(a, { name: 'QuotaExceededError' });
  assert.ok((await b) > 0, 'the second answers for itself');
  assert.deepEqual(idb.log.at(-1).map((w) => [w[0], w[1]]), [['put', 'w'], ['delete', 'w#..']], 'an image, holding what the refused one drained');
  const back = await Fennec.open(wasm);
  assert.equal(await restore(back, 'w'), true);
  assert.deepEqual(ns(back), [1, 2, 3]);
});

// A persist called while `restore` was still reading found no chunks to
// follow, and stored the empty database's image over the one being
// restored: the restore went on to load the old rows, and later chunks
// were stored after an image that no longer held them.
test('a persist called while restore is out runs after it, where it left the database', { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
  const idb = fakeIndexedDB();
  globalThis.indexedDB = idb;
  globalThis.IDBKeyRange = KeyRange;
  const { Fennec, persist, restore } = await import('./fennec.js');
  const ns = (d) => d.rows('get t select n order n').map((r) => r.n);
  const first = await Fennec.open(wasm);
  first.run('create collection t (n int)');
  first.run('put t {n: 1}');
  await persist(first, 'r');
  first.run('put t {n: 2}');
  await persist(first, 'r');

  const db = await Fennec.open(wasm);
  // A refused one holds up nothing after it.
  const cryptoKey = await crypto.subtle.generateKey({ name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
  const refused = restore(db, 'r', { cryptoKey });
  const r = restore(db, 'r');
  const p = persist(db, 'r');
  await assert.rejects(refused, /stored in the clear/);
  assert.equal(await r, true);
  assert.equal(await p, 0, 'nothing written since the restore, nothing stored');
  assert.deepEqual(ns(db), [1, 2]);
  db.run('put t {n: 3}');
  await persist(db, 'r');
  assert.deepEqual(idb.log.at(-1).map((w) => w[1]), ['r#000000002'], 'a chunk after those restored');
  const back = await Fennec.open(wasm);
  assert.equal(await restore(back, 'r'), true);
  assert.deepEqual(ns(back), [1, 2, 3]);
});

// A closed database gives an empty image. A persist that waited its turn,
// or retried once its connection had ended, ran after the page closed the
// database, stored that empty image over the record, deleted its chunks,
// and answered as if it had stored the database: restore then refused the
// record as corrupt.
test('a persist or restore that has not run by the close is refused, the record standing', { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async (t) => {
  const idb = fakeIndexedDB();
  globalThis.indexedDB = idb;
  globalThis.IDBKeyRange = KeyRange;
  const { Fennec, persist, restore } = await import('./fennec.js');
  const ns = (d) => d.rows('get t select n order n').map((r) => r.n);
  const restored = async (key) => {
    const back = await Fennec.open(wasm);
    assert.equal(await restore(back, key), true, `${key} restores`);
    return ns(back);
  };
  const stored = async (key) => {
    const db = await Fennec.open(wasm);
    db.run('create collection t (n int)');
    db.run('put t {n: 1}');
    await persist(db, key);
    db.run('put t {n: 2}');
    await persist(db, key);
    return db;
  };
  const closed = /the database was closed/;

  await t.test('one waiting behind a refused one, which owed an image', async () => {
    const db = await stored('q');
    db.run('put t {n: 3}');
    idb.refuse = 'QuotaExceededError';
    const refused = persist(db, 'q');
    const waiting = persist(db, 'q');
    db.close();
    await assert.rejects(refused, { name: 'QuotaExceededError' });
    await assert.rejects(waiting, closed);
    assert.deepEqual(await restored('q'), [1, 2]);
  });

  await t.test("one waiting behind another key's, which took its image at the call", async () => {
    const db = await stored('k2');
    const other = persist(db, 'k1');
    const back = persist(db, 'k2');
    db.close();
    assert.ok((await other) > 0, 'the one running at the close stores what it took');
    await assert.rejects(back, closed);
    assert.deepEqual(await restored('k2'), [1, 2]);
    assert.deepEqual(await restored('k1'), [1, 2]);
  });

  await t.test('the retry after an ended connection, no event said yet', async () => {
    const db = await stored('e');
    db.run('put t {n: 3}');
    const told = idb.invalidate();
    const ending = persist(db, 'e');
    db.close();
    await assert.rejects(ending, closed);
    told();
    assert.deepEqual(await restored('e'), [1, 2]);
  });

  // Each would have loaded into the closed database.
  await t.test('a restore reading at the close, one waiting its turn, and calls after', async () => {
    const db = await stored('r');
    const running = restore(db, 'r');
    const queued = restore(db, 'r');
    db.close();
    await assert.rejects(running, closed);
    await assert.rejects(queued, closed);
    await assert.rejects(persist(db, 'r'), closed);
    await assert.rejects(restore(db, 'r'), closed);
    assert.deepEqual(await restored('r'), [1, 2]);
  });

  await t.test('an empty image, however it came, is never stored', async () => {
    await stored('z');
    const db = await Fennec.open(wasm);
    db.snapshot = () => new Uint8Array(0);
    await assert.rejects(persist(db, 'z'), /empty image/);
    assert.deepEqual(await restored('z'), [1, 2]);
  });
});

// A restore that failed -- here the collation data its image needs could
// not be fetched -- may have emptied the database first (one holding only
// what its schema made is emptied for the image), and a persist waiting
// behind it stored that empty database over the record the restore was to
// load.
test('a persist waiting for a restore that fails is refused with it; one called after runs', { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
  globalThis.indexedDB = fakeIndexedDB();
  globalThis.IDBKeyRange = KeyRange;
  const { Fennec, persist, restore } = await import('./fennec.js');
  const collation = (name) => readFile(new URL(`./collate/${name}.bin`, import.meta.url));
  const schema = 'create collection words (w text collate und @sorted)';
  const words = (d) => d.rows('get words select w').map((r) => r.w);
  const writer = await Fennec.open(wasm, { schema, collation });
  await writer.from('words').insert([{ w: 'Ελλάδα' }]);
  assert.ok((await persist(writer, 'u')) > 0);

  const offline = () => {
    throw new Error('offline');
  };
  const page = await Fennec.open(wasm, { schema, collation: offline });
  const failing = restore(page, 'u');
  const waiting = persist(page, 'u');
  await assert.rejects(failing, /offline/);
  await assert.rejects(waiting, /the restore it waited for failed \(offline\)/);
  const back = await Fennec.open(wasm, { collation });
  assert.equal(await restore(back, 'u'), true);
  assert.deepEqual(words(back), ['Ελλάδα'], 'the record stands');
  // Called once the failure was answered, a persist runs as it always did.
  assert.ok((await persist(page, 'after')) > 0);

  // A restore that loaded its record, called after the failed one, is the
  // one a persist behind both waited for; one that found no record lets
  // the failed one before it count.
  const again = await Fennec.open(wasm, { schema, collation: offline });
  const first = restore(again, 'u');
  const none = restore(again, 'none');
  const behindNone = persist(again, 'u');
  await assert.rejects(first, /offline/);
  assert.equal(await none, false);
  await assert.rejects(behindNone, /the restore it waited for failed/);
  const fine = await Fennec.open(wasm, { schema, collation });
  const failed = restore(fine, 'u', { cryptoKey: await crypto.subtle.generateKey({ name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']) });
  const loaded = restore(fine, 'u');
  const behindLoaded = persist(fine, 'copy');
  await assert.rejects(failed, /stored in the clear/);
  assert.equal(await loaded, true);
  assert.ok((await behindLoaded) > 0, 'stored after the restore that loaded');
  const copy = await Fennec.open(wasm, { collation });
  assert.equal(await restore(copy, 'copy'), true);
  assert.deepEqual(words(copy), ['Ελλάδα']);
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
  // Firefox closes the connection after such a write, and says so only
  // later: a call in between goes to another. The refused chunk's writes
  // are nowhere else: the next persist stores the image whole.
  const told = idb.invalidate();
  await persist(db, 'q');
  assert.deepEqual(idb.log.at(-1).map((w) => w[0]), ['put', 'delete']);
  told();
  const again = await Fennec.open(wasm);
  await restore(again, 'q');
  assert.deepEqual(ns(again), [1, 2]);
  assert.equal(idb.connections, 1, 'those a refusal left closed, the one in use open');
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

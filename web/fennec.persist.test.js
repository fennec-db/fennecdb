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
    // Refused before it reads: read first, a key holding nothing answered
    // false, and a sealed record its key's absence.
    await assert.rejects(restore(db, 'nothing'), closed);
    const cryptoKey = await crypto.subtle.generateKey({ name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
    await assert.rejects(restore(db, 'r', { cryptoKey }), closed);
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

// A restore whose load failed left `persist` believing the database it had
// stored was still there. One holding only what its schema made had been
// given a new handle, no journal on it: every later persist drained
// nothing and answered 0, the page's writes stored nowhere. The module
// empties a database a load found collation data missing for, its journal
// kept: a persist appended the writes from before the restore to a record
// another had replaced, of a database that was gone.
test('a persist after a restore that failed as it loaded stores the database whole', { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
  const idb = fakeIndexedDB();
  globalThis.indexedDB = idb;
  globalThis.IDBKeyRange = KeyRange;
  const { Fennec, persist, restore } = await import('./fennec.js');
  const collation = (name) => readFile(new URL(`./collate/${name}.bin`, import.meta.url));
  const schema = 'create collection words (w text collate und @sorted)';
  const offline = () => {
    throw new Error('offline');
  };
  const ns = (d) => d.rows('get t select n order n').map((r) => r.n);
  const greek = async (key) => {
    const other = await Fennec.open(wasm, { schema, collation });
    await other.from('words').insert([{ w: 'Ελλάδα' }]);
    assert.ok((await persist(other, key)) > 0);
  };
  const restored = async (key) => {
    const back = await Fennec.open(wasm, { collation });
    assert.equal(await restore(back, key), true);
    return back;
  };
  const stores = async (db, key) => {
    assert.ok((await persist(db, key)) > 0, 'stored');
    assert.deepEqual(idb.log.at(-1).map((w) => w[1]), [key, `${key}#..`], 'an image');
  };

  // The review's sequence: emptied for the image, then refused.
  const page = await Fennec.open(wasm, { schema, collation: offline });
  assert.ok((await persist(page, 'e')) > 0);
  await greek('e');
  await assert.rejects(restore(page, 'e'), /offline/);
  page.run('create collection t (n int)');
  page.run('put t {n: 1}');
  await stores(page, 'e');
  page.run('put t {n: 2}');
  assert.ok((await persist(page, 'e')) > 0, 'a chunk after it');
  assert.deepEqual(ns(await restored('e')), [1, 2]);

  // Holding more, it is not emptied first: the module empties it. Its
  // image large enough that the next writes are a chunk, not folded.
  const more = await Fennec.open(wasm, { collation: offline });
  more.run('create collection t (n int)');
  for (let n = 10; n < 60; n++) more.run(`put t {n: ${n}}`);
  assert.ok((await persist(more, 'm')) > 0);
  more.run('put t {n: 2}');
  await greek('m');
  await assert.rejects(restore(more, 'm'), /offline/);
  assert.deepEqual(more.schemas(), [], 'emptied by the module');
  more.run('create collection t (n int)');
  more.run('put t {n: 3}');
  await stores(more, 'm');
  const back = await restored('m');
  assert.deepEqual(ns(back), [3]);
  assert.deepEqual(back.schemas().map((s) => s.name), ['t']);
});

// Closed while `restore` fetched the collation data its image needs, the
// load met a closed handle and refused the record as corrupt; closed while
// it checked the database against the code's schema, the check refused it
// as a handle not found.
test('a restore the database is closed under as it loads or checks is refused, naming the close', { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
  globalThis.indexedDB = fakeIndexedDB();
  globalThis.IDBKeyRange = KeyRange;
  const { Fennec, persist, restore } = await import('./fennec.js');
  const fetched = (name) => readFile(new URL(`./collate/${name}.bin`, import.meta.url));
  const schema = 'create collection words (w text collate und @sorted)';
  const words = (d) => d.rows('get words select w order w').map((r) => r.w);
  const writer = await Fennec.open(wasm, { schema, collation: fetched });
  await writer.from('words').insert([{ w: 'Ελλάδα' }, { w: 'b' }]);
  await persist(writer, 'g');
  const closed = /restore refused: the database was closed/;

  const fetching = await Fennec.open(wasm, {
    schema,
    collation: async (name) => {
      fetching.close();
      return fetched(name);
    },
  });
  await assert.rejects(restore(fetching, 'g'), closed);

  // The schema's check asks for nothing over the network: the close comes
  // as one would while it fetched collation data a migration needs, and
  // the check after meets the closed handle.
  const plain = await Fennec.open(wasm, { schema: 'create collection t (n int)' });
  plain.run('put t {n: 1}');
  await persist(plain, 'p');
  const checking = await Fennec.open(wasm, { schema: 'create collection t (n int)' });
  checking.checkSchema = function (...args) {
    this.close();
    return Fennec.prototype.checkSchema.apply(this, args);
  };
  await assert.rejects(restore(checking, 'p'), closed);

  const back = await Fennec.open(wasm, { collation: fetched });
  assert.equal(await restore(back, 'g'), true);
  assert.deepEqual(words(back), ['b', 'Ελλάδα'], 'the record stands');
});

// The restore was let go of as its work ended, a few jobs before its
// caller was answered: a persist made between counted as made after the
// failure, and stored the database the restore had emptied.
test("a restore is out until its caller is answered, and the caller's own handler may persist", { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
  const idb = fakeIndexedDB();
  globalThis.indexedDB = idb;
  globalThis.IDBKeyRange = KeyRange;
  const { Fennec, persist, restore } = await import('./fennec.js');
  const { inspect } = await import('node:util');
  const schema = 'create collection words (w text collate und @sorted)';
  const writer = await Fennec.open(wasm, { schema, collation: (name) => readFile(new URL(`./collate/${name}.bin`, import.meta.url)) });
  await writer.from('words').insert([{ w: 'Ελλάδα' }]);
  await persist(writer, 'u');

  // From the failed fetch on, a persist every job, each noting whether the
  // promise restore answered was still pending when it was made.
  const made = [];
  let restoring = null;
  let answered = false;
  // Capped: were the failure sent through a macrotask, an uncapped tick
  // would spin the microtask queue for ever (node:test has no timeout).
  const cap = 10_000;
  let ticks = 0;
  let stuck = null;
  const tick = () => {
    if (answered) return;
    if (++ticks > cap) {
      stuck = new Error(`the caller was never answered within ${cap} microtask ticks`);
      return;
    }
    made.push([inspect(restoring).includes('<pending>'), persist(page, `t${made.length}`)]);
    queueMicrotask(tick);
  };
  const page = await Fennec.open(wasm, {
    schema,
    collation: () => {
      queueMicrotask(tick);
      throw new Error('offline');
    },
  });
  restoring = restore(page, 'u');
  let own = null;
  await restoring.catch((e) => {
    answered = true;
    assert.match(e.message, /offline/);
    own = persist(page, 'own');
  });
  assert.equal(stuck, null, stuck?.message);
  const ended = await Promise.allSettled(made.map(([, p]) => p));
  assert.ok(made.some(([out]) => out) && made.some(([out]) => !out), 'calls made on both sides of the answer');
  made.forEach(([out], i) => {
    if (out) assert.match(ended[i].reason?.message ?? 'ran', /the restore it waited for failed \(offline\)/, `t${i}, made while it was out`);
    else assert.equal(ended[i].status, 'fulfilled', `t${i}, made once it was answered`);
  });
  assert.ok((await own) > 0, "its caller's own handler stores");
  assert.ok(idb.records.has('own'));
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

  // A whole store moved under another name, by the same key: its image's
  // tag covers the name it was sealed under.
  const other = await Fennec.open(wasm);
  other.run('create collection t (n int, body text)');
  other.run('put t {n: 1, body: "the other one"}');
  await persist(other, 'other', { cryptoKey });
  const kept = new Map(idb.records);
  for (const k of [...idb.records.keys()]) if (k === 'other' || k.startsWith('other#')) idb.records.delete(k);
  for (const [k, v] of kept) if (k === 's' || k.startsWith('s#')) put(k.replace(/^s/, 'other'), v);
  await assert.rejects(restore(await Fennec.open(wasm), 'other', { cryptoKey }), /other: record 0 does not open/);
});

// Each chunk's tag covers the tag of the record before it. Without that, a
// page that went on from a store cut short sealed its next chunks at the
// places of those cut, under the same generation, and a chunk it cut
// opened after one it sealed since, or one sealed since after one it cut
// (1.0's review): writes in the middle of the store dropped or doubled.
// What remains is a store cut short, restored as the page left it then.
test('a sealed store cut short opens as it stood or as the page went on, never spliced', { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
  const idb = fakeIndexedDB();
  globalThis.indexedDB = idb;
  globalThis.IDBKeyRange = KeyRange;
  const { Fennec, persist, restore } = await import('./fennec.js');
  const cryptoKey = await crypto.subtle.generateKey({ name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
  const chunk = (n) => `c#${String(n).padStart(9, '0')}`;
  const ns = (d) => d.rows('get t select n order n').map((r) => r.n);
  const opened = async () => {
    const d = await Fennec.open(wasm);
    assert.equal(await restore(d, 'c', { cryptoKey }), true);
    return ns(d);
  };

  const db = await Fennec.open(wasm);
  db.run('create collection t (n int, body text)');
  for (let i = 0; i < 100; i++) db.run('put t {n: $1, body: "a row of the image"}', [i]);
  await persist(db, 'c', { cryptoKey });
  for (const n of [1001, 1002, 1003, 1004]) {
    db.run('put t {n: $1}', [n]);
    assert.ok((await persist(db, 'c', { cryptoKey })) < 100, 'a chunk');
  }
  const base = ns(db).slice(0, 100);
  assert.deepEqual([...idb.records.keys()].sort(), ['c', chunk(1), chunk(2), chunk(3), chunk(4)]);
  const before = new Map(idb.records);

  // Cut at 2: the page opens what is left and goes on, sealing 3 and 4 again.
  idb.records.delete(chunk(3));
  idb.records.delete(chunk(4));
  const page = await Fennec.open(wasm);
  assert.equal(await restore(page, 'c', { cryptoKey }), true);
  assert.deepEqual(ns(page), [...base, 1001, 1002]);
  for (const n of [2003, 2004]) {
    page.run('put t {n: $1}', [n]);
    await persist(page, 'c', { cryptoKey });
  }
  assert.deepEqual([...idb.records.keys()].sort(), ['c', chunk(1), chunk(2), chunk(3), chunk(4)]);
  const after = new Map(idb.records);
  assert.deepEqual(await opened(), [...base, 1001, 1002, 2003, 2004]);

  // The chunk cut, then the one sealed in its place's next: refused.
  idb.records.set(chunk(3), before.get(chunk(3)));
  await assert.rejects(opened(), /c: record 4 does not open/);
  // And the one sealed since, then the one cut after it.
  idb.records.set(chunk(3), after.get(chunk(3)));
  idb.records.set(chunk(4), before.get(chunk(4)));
  await assert.rejects(opened(), /c: record 4 does not open/);
  // What remains: either run cut short, as it stood.
  idb.records.set(chunk(3), before.get(chunk(3)));
  idb.records.delete(chunk(4));
  assert.deepEqual(await opened(), [...base, 1001, 1002, 1003]);
  idb.records.set(chunk(3), after.get(chunk(3)));
  assert.deepEqual(await opened(), [...base, 1001, 1002, 2003]);
});

// A store sealed before chunks covered the tag before them -- 0.2.0's, up
// to this tree (version 1: its image has no `v`) -- restores as it was
// sealed, and the next persist stores an image sealed as version 2 in its
// place, which its chunks then follow. The page's stored data is a frozen
// surface (docs/design/compatibility.md); web/fennec.compat.test.js holds
// 0.2.0's own store, this one a store sealed here as it sealed.
test('a store sealed as version 1 restores, and its next persist stores an image of version 2', { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
  const idb = fakeIndexedDB();
  globalThis.indexedDB = idb;
  globalThis.IDBKeyRange = KeyRange;
  const { Fennec, persist, restore } = await import('./fennec.js');
  const cryptoKey = await crypto.subtle.generateKey({ name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
  // Version 1's seal, as 0.2.0's web/fennec.js wrote it.
  const sealedAsV1 = async (key, gen, n, bytes) => {
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const additionalData = new TextEncoder().encode(`fennecdb\0${key}\0${gen}\0${n}`);
    const data = new Uint8Array(await crypto.subtle.encrypt({ name: 'AES-GCM', iv, additionalData }, cryptoKey, bytes));
    return n === 0 ? { gen, iv, data } : { iv, data };
  };
  const ns = (d) => d.rows('get t select n order n').map((r) => r.n);

  const db = await Fennec.open(wasm);
  db.run('create collection t (n int, body text)');
  for (let i = 0; i < 100; i++) db.run('put t {n: $1, body: "a row of the image"}', [i]);
  db.journal();
  const gen = '0123456789abcdef';
  idb.records.set('old', await sealedAsV1('old', gen, 0, db.snapshot()));
  for (const [n, row] of [[1, 1001], [2, 1002]]) {
    db.run('put t {n: $1}', [row]);
    const { replace, bytes } = db.drain();
    assert.equal(replace, false);
    idb.records.set(`old#${String(n).padStart(9, '0')}`, await sealedAsV1('old', gen, n, bytes));
  }
  const want = ns(db);

  const page = await Fennec.open(wasm);
  assert.equal(await restore(page, 'old', { cryptoKey }), true);
  assert.deepEqual(ns(page), want);
  // Its `v` is its own: a version 1 image read as version 2 does not
  // open, and a later version is refused by its number.
  const image = idb.records.get('old');
  idb.records.set('old', { ...image, v: 2 });
  await assert.rejects(restore(await Fennec.open(wasm), 'old', { cryptoKey }), /old: record 0 does not open/);
  idb.records.set('old', { ...image, v: 3 });
  await assert.rejects(restore(await Fennec.open(wasm), 'old', { cryptoKey }), /old is sealed as version 3, which a later client wrote/);
  idb.records.set('old', image);

  // Written to, it stores an image sealed as version 2 in place of the
  // three records, which its chunks follow.
  page.run('put t {n: 2001}');
  assert.ok((await persist(page, 'old', { cryptoKey })) > 1000, 'an image');
  assert.deepEqual(idb.log.at(-1).map((w) => w[1]), ['old', 'old#..']);
  assert.equal(idb.records.get('old').v, 2);
  page.run('put t {n: 2002}');
  assert.ok((await persist(page, 'old', { cryptoKey })) < 100, 'then a chunk');
  assert.deepEqual([...idb.records.keys()].filter((k) => k.startsWith('old')).sort(), ['old', 'old#000000001']);
  const back = await Fennec.open(wasm);
  assert.equal(await restore(back, 'old', { cryptoKey }), true);
  assert.deepEqual(ns(back), [...want, 2001, 2002]);
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

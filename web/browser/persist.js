// fennecdb's persistence in a real browser (`make browser-test`): persist
// and restore against the engine's IndexedDB and WebCrypto, openFile
// against its origin private file system in a dedicated worker
// (file-worker.js). The page runs in phases; a phase that reloads the page
// leaves its state with the server first (run.mjs keeps it) and the next
// load goes on from there. Each test is reported to the server once it
// ends; the first failure ends the run.

import { Fennec, getState, openFile, persist, putState, restore } from '/web/fennec.js';
import { noise } from './noise.js';

const query = new URLSearchParams(location.search);
const RUN = query.get('run');
/**
 * The quota in bytes the driver held the origin to (run.mjs's `QUOTA`), 0
 * where it could set none. Told, not read: Chrome's
 * `navigator.storage.estimate()` names 10 GiB whatever quota it enforces.
 */
const QUOTA = Number(query.get('quota') ?? 0);

const TESTS = {
  chunks: 'persist stores the image, then each write since as a chunk',
  reload: 'a reload restores the image and its chunks as one file',
  inChunk: "a reload in the middle of a chunk's transaction loses no acknowledged write",
  inImage: "a reload in the middle of a new image's transaction keeps the old image and its chunks, or the new alone",
  fold: 'chunks outgrowing half the image are folded into a new one',
  sealed: 'a sealed image and its chunks hold no text in the clear and open with their key alone, after a reload too',
  file: 'openFile keeps each statement in the file before it answers, across a reload',
  torn: 'a reload in the middle of an append cuts the torn record off and loses no answered write',
  rewrite: 'a reload at any step of a rewrite loses no answered write and empties the copy beside the file',
  second: 'a second opener of a file is refused while the first holds it',
  page: 'openFile in the page itself is refused: it needs a dedicated worker',
  quota: 'persist and putState past the quota are refused, what was stored standing',
  fileQuota: 'a file past the quota refuses the write and keeps every write it answered',
  closed: 'persist, restore and the state calls leave no connection open to block a delete of the database',
};

/** The files the suite makes in the origin private file system, each with its copy. */
const FILES = ['keep', 'torn', 'swap', 'lock', 'page', 'quota'].flatMap((n) => [`${n}.fennec`, `${n}.fennec~`]);

const at = (path) => `/${path}?run=${encodeURIComponent(RUN)}`;
const send = (path, body = {}) => fetch(at(path), { method: 'POST', body: JSON.stringify(body) });
const out = document.getElementById('out');
const say = (text) => {
  out.textContent += `${text}\n`;
  return send('log', { text });
};

let state = await (await fetch(at('state'))).json();
let failed = false;
const fail = (e) => {
  if (failed) return;
  failed = true;
  say(`failed: ${e?.stack ?? e}`);
  send('fail', { error: `${state.phase}: ${e?.message ?? e}` });
};
addEventListener('error', (e) => fail(e.error ?? e.message));
addEventListener('unhandledrejection', (e) => fail(e.reason));

// ------------------------------------------------------------- assertions

function ok(cond, what) {
  if (!cond) throw new Error(what);
}

function same(got, due, what) {
  const a = JSON.stringify(got);
  const b = JSON.stringify(due);
  if (a !== b) throw new Error(`${what}: ${a.slice(0, 400)}, where ${b.slice(0, 400)} was due`);
}

/** The error `p` is refused with, which must match `re`. */
async function refuses(p, re, what) {
  try {
    await p;
  } catch (e) {
    if (re.test(`${e?.name}: ${e?.message}`)) return e;
    throw new Error(`${what}: refused with ${e?.name}: ${e?.message}, not ${re}`);
  }
  throw new Error(`${what}: not refused`);
}

/**
 * `p`, which must settle: a call that never answers is a failure of its
 * own, named, rather than the run's end at the driver's bound.
 */
function within(p, ms = 30_000) {
  let timer;
  const late = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`no answer in ${ms / 1000} s`)), ms);
  });
  return Promise.race([p, late]).finally(() => clearTimeout(timer));
}

const range = (from, to) => Array.from({ length: to - from }, (_, i) => from + i);

/** Runs a test and reports it; a failure is reported and ends the run. */
async function test(name, fn) {
  const t0 = performance.now();
  try {
    const r = await fn();
    await send('report', { test: name, ok: !r?.skip, skip: r?.skip, detail: r?.detail, ms: performance.now() - t0 });
  } catch (e) {
    await send('report', { test: name, ok: false, error: e?.message ?? String(e), ms: performance.now() - t0 });
    throw e;
  }
}

/**
 * The half of a test that runs before a reload: reported only when it
 * fails, the half after the reload reporting the test.
 */
async function part(name, fn) {
  const t0 = performance.now();
  try {
    return await fn();
  } catch (e) {
    await send('report', { test: name, ok: false, error: e?.message ?? String(e), ms: performance.now() - t0 });
    throw e;
  }
}

/** Workers a page leaves running as it reloads: kept from the collector till then. */
const left = [];

/** The page loaded again, the run going on at `phase`; never returns. */
async function reload(phase) {
  state.phase = phase;
  await fetch(at('state'), { method: 'PUT', body: JSON.stringify(state) });
  location.reload();
  await new Promise(() => {});
}

// ------------------------------------------------------------- databases

const MODULE = await WebAssembly.compileStreaming(fetch('/web/fennec.wasm'));
const opened = [];
/** A database of the page's, closed when the phase ends. */
async function open() {
  const db = await Fennec.open(MODULE);
  opened.push(db);
  return db;
}

const ns = (db, c = 't') => db.rows(`get ${c} select n order n`).map((r) => r.n);
const rows = (db) => db.rows('get t select n, body order n');

// ------------------------------------------------------------- IndexedDB

function done(req) {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

/** fennecdb's own database, as persist made it: never made here. */
async function fennecdb() {
  const req = indexedDB.open('fennecdb', 1);
  req.onupgradeneeded = () => req.transaction.abort();
  return done(req);
}

/** fennecdb's records under `key` -- the image's and its chunks' -- by key, as they are stored. */
async function records(key) {
  const db = await fennecdb();
  try {
    const os = db.transaction('images', 'readonly').objectStore('images');
    const keys = IDBKeyRange.bound(key, `${key}#￿`);
    const [names, values] = await Promise.all([done(os.getAllKeys(keys)), done(os.getAll(keys))]);
    return new Map(names.map((k, i) => [k, values[i]]));
  } finally {
    db.close();
  }
}

/** A record of fennecdb's written over, as something else than persist might. */
async function overwrite(entries) {
  const db = await fennecdb();
  try {
    const tx = db.transaction('images', 'readwrite');
    for (const [k, v] of entries) tx.objectStore('images').put(v, k);
    await new Promise((resolve, reject) => {
      tx.oncomplete = resolve;
      tx.onerror = tx.onabort = () => reject(tx.error);
    });
  } finally {
    db.close();
  }
}

/**
 * An AES-GCM key derived from `passphrase`, as the docs have a page make
 * it (site/content/docs/javascript.html#sealed): never stored, made again
 * after the reload. Fewer rounds than the docs' 600 000: what is tested is
 * the seal, not the derivation's cost.
 */
async function derived(passphrase) {
  const te = new TextEncoder();
  const base = await crypto.subtle.importKey('raw', te.encode(passphrase), 'PBKDF2', false, ['deriveKey']);
  return crypto.subtle.deriveKey(
    { name: 'PBKDF2', salt: te.encode('fennec-test'), iterations: 1000, hash: 'SHA-256' },
    base,
    { name: 'AES-GCM', length: 256 },
    false,
    ['encrypt', 'decrypt'],
  );
}

async function digest(bytes) {
  const d = new Uint8Array(await crypto.subtle.digest('SHA-256', bytes));
  return [...d].map((b) => b.toString(16).padStart(2, '0')).join('');
}

/** Whether `bytes` hold `text`'s ASCII anywhere. */
function holds(bytes, text) {
  const t = new TextEncoder().encode(text);
  outer: for (let i = 0; i + t.length <= bytes.length; i++) {
    for (let j = 0; j < t.length; j++) if (bytes[i + j] !== t[j]) continue outer;
    return true;
  }
  return false;
}

/**
 * The next transaction written in fennecdb's store, kept from committing
 * until the page is gone: once its writes are asked for -- the put, and
 * an image's delete of its chunks, asked in the same turn -- a read after
 * a read keeps it open. Resolves once the reads have followed the writes,
 * the transaction waiting on them alone.
 */
function holdNextWrite() {
  const put = IDBObjectStore.prototype.put;
  return new Promise((resolve) => {
    IDBObjectStore.prototype.put = function (...args) {
      IDBObjectStore.prototype.put = put;
      const asked = put.apply(this, args);
      let reads = 0;
      const again = () => {
        if (++reads === 2) resolve();
        this.get('held').onsuccess = again;
      };
      this.get('held').onsuccess = again;
      return asked;
    };
  });
}

/**
 * fennecdb's database and the suite's files gone. A connection left open
 * would block the delete -- told at once by `blocked` -- until the
 * collector came for it.
 */
async function wipe() {
  await new Promise((resolve, reject) => {
    const req = indexedDB.deleteDatabase('fennecdb');
    req.onsuccess = resolve;
    req.onerror = () => reject(req.error);
    req.onblocked = () => reject(new Error('a connection left open blocks the delete of fennecdb'));
  });
  const root = await navigator.storage.getDirectory();
  for (const name of FILES) {
    await root.removeEntry(name).catch((e) => {
      if (e.name !== 'NotFoundError') throw e;
    });
  }
}

// ------------------------------------------------------------- the worker

/** A dedicated worker keeping a file (file-worker.js), called by message. */
function worker() {
  const w = new Worker('./file-worker.js', { type: 'module' });
  const waiting = new Map();
  let id = 0;
  let stop;
  const stopped = new Promise((r) => (stop = r));
  w.onmessage = ({ data }) => {
    if (data.stopped) return stop(data.stopped);
    const p = waiting.get(data.id);
    waiting.delete(data.id);
    if (!data.error) return p.resolve(data.value);
    p.reject(Object.assign(new Error(data.error.message), { name: data.error.name, cause: data.error.cause }));
  };
  w.onerror = (e) => {
    e.preventDefault();
    for (const p of waiting.values()) p.reject(new Error(`the worker failed: ${e.message}`));
  };
  const call = (name, ...args) =>
    new Promise((resolve, reject) => {
      waiting.set(++id, { resolve, reject });
      w.postMessage({ id, call: name, args });
    });
  return { call, stopped, end: () => w.terminate() };
}

// ------------------------------------------------------------- phases

const PHASES = {
  async start() {
    ok(crossOriginIsolated, 'the page is cross-origin isolated (run.mjs serves it so)');
    await wipe();
    await send('plan', { tests: Object.values(TESTS) });
    await say(navigator.userAgent);
  },

  async persist() {
    await test(TESTS.chunks, async () => {
      const db = await open();
      db.run('create collection t (n int @hash, body text)');
      for (let i = 0; i < 200; i++) db.run('put t {n: $1, body: $2}', [i, `row ${i}`]);
      const image = await persist(db, 'plain');
      let stored = await records('plain');
      same([...stored.keys()], ['plain'], 'the first persist stores the image alone');
      ok(stored.get('plain') instanceof Uint8Array && stored.get('plain').length === image, 'the image is the bytes persist wrote');
      db.run('put t {n: 200, body: "one"}');
      const one = await persist(db, 'plain');
      ok(one > 0 && one < 200, `one row stores a chunk of its own size: ${one} bytes`);
      same(await persist(db, 'plain'), 0, 'nothing written, nothing stored');
      db.run('set t {body: "changed"} where n = 3');
      db.run('del t where n = 4');
      await persist(db, 'plain');
      stored = await records('plain');
      same([...stored.keys()], ['plain', 'plain#000000001', 'plain#000000002'], 'each persist after the image stores a chunk');
      state.plain = rows(db);
    });

    // Sealed with a key derived from a passphrase, made again after the
    // reload.
    await part(TESTS.sealed, async () => {
      const cryptoKey = await derived('a passphrase');
      const db = await open();
      db.run('create collection t (n int, body text)');
      for (let i = 0; i < 50; i++) db.run('put t {n: $1, body: $2}', [i, `secret-text-${i}`]);
      ok(holds(db.snapshot(), 'secret-text'), 'the image in the clear holds the text');
      await persist(db, 'sealed', { cryptoKey });
      db.run('put t {n: 100, body: "secret-chunk-one"}');
      await persist(db, 'sealed', { cryptoKey });
      db.run('put t {n: 101, body: "secret-chunk-two"}');
      await persist(db, 'sealed', { cryptoKey });
      const stored = await records('sealed');
      same([...stored.keys()], ['sealed', 'sealed#000000001', 'sealed#000000002'], 'the sealed image and its two chunks');
      for (const [k, rec] of stored) ok(rec.data instanceof Uint8Array && !holds(rec.data, 'secret'), `${k} holds no text in the clear`);
      state.sealed = rows(db);
    });

    // Every write acknowledged -- its persist resolved -- then one more,
    // whose chunk's transaction is held open as the page reloads.
    await part(TESTS.inChunk, async () => {
      const db = await open();
      db.run('create collection t (n int)');
      const acked = 12;
      for (let i = 0; i < acked; i++) {
        db.run('put t {n: $1}', [i]);
        await persist(db, 'held');
      }
      db.run('put t {n: $1}', [acked]);
      const held = holdNextWrite();
      persist(db, 'held').then(
        () => fail(new Error('a held transaction committed')),
        () => {},
      );
      await held;
      state.held = acked;
    });
    await reload('restored');
  },

  async restored() {
    await test(TESTS.reload, async () => {
      const back = await open();
      ok(await restore(back, 'plain'), 'restore found the record');
      same(rows(back), state.plain, 'the rows persisted before the reload');
      back.run('put t {n: 1000, body: "after"}');
      await persist(back, 'plain');
      same([...(await records('plain')).keys()], ['plain', 'plain#000000001', 'plain#000000002', 'plain#000000003'], 'a restored database adds chunks where the stored ones end');
      const again = await open();
      await restore(again, 'plain');
      same(rows(again), rows(back), 'restored again, with the chunk stored after the reload');
    });

    await test(TESTS.inChunk, async () => {
      const acked = state.held;
      const back = await open();
      ok(await restore(back, 'held'), 'restore found the record');
      const got = ns(back);
      const landed = got.length > acked;
      same(got, range(0, landed ? acked + 1 : acked), 'every acknowledged write, and the one in flight whole or not at all');
      back.run('put t {n: 999}');
      await persist(back, 'held');
      const again = await open();
      await restore(again, 'held');
      same(ns(again), ns(back), 'the next chunk stored where the restored ones end');
      return { detail: landed ? 'the write in flight landed' : 'the write in flight did not land' };
    });

    await test(TESTS.sealed, async () => {
      const cryptoKey = await derived('a passphrase');
      const back = await open();
      ok(await restore(back, 'sealed', { cryptoKey }), 'restore found the record');
      same(rows(back), state.sealed, 'the sealed rows, opened with their key');
      await refuses(restore(await open(), 'sealed'), /sealed: restore needs its cryptoKey/, 'restored without a key');
      const other = await derived('another passphrase');
      await refuses(restore(await open(), 'sealed', { cryptoKey: other }), /record 0 does not open/, 'restored with another key');
      const stored = await records('sealed');
      await overwrite([
        ['sealed#000000001', stored.get('sealed#000000002')],
        ['sealed#000000002', stored.get('sealed#000000001')],
      ]);
      await refuses(restore(await open(), 'sealed', { cryptoKey }), /record 1 does not open/, 'two chunks swapped');
      const flipped = { ...stored.get('sealed#000000002'), data: stored.get('sealed#000000002').data.slice() };
      flipped.data[3] ^= 1;
      await overwrite([...stored, ['sealed#000000002', flipped]]);
      await refuses(restore(await open(), 'sealed', { cryptoKey }), /record 2 does not open/, 'a byte changed');
      await overwrite([...stored]);
      await refuses(restore(await open(), 'plain', { cryptoKey }), /stored in the clear/, 'a record in the clear restored with a key');
      const again = await open();
      await restore(again, 'sealed', { cryptoKey });
      same(rows(again), state.sealed, 'put back as stored, the record opens again');
    });

    // An image and its chunks, then a new image -- a compact's -- whose
    // transaction, the image put and the chunks deleted, is held open as
    // the page reloads. The image is large enough for three chunks: past
    // half of it, they are folded into one.
    await part(TESTS.inImage, async () => {
      const db = await open();
      db.run('create collection t (n int)');
      db.run(`put t [${range(0, 100).map((n) => `{n: ${n}}`).join(', ')}]`);
      await persist(db, 'fold');
      for (let i = 100; i < 103; i++) {
        db.run('put t {n: $1}', [i]);
        await persist(db, 'fold');
      }
      const before = await records('fold');
      same([...before.keys()], ['fold', 'fold#000000001', 'fold#000000002', 'fold#000000003'], 'an image and three chunks');
      state.fold = await digest(before.get('fold'));
      db.run('put t {n: 103}');
      db.run('compact');
      const held = holdNextWrite();
      persist(db, 'fold').then(
        () => fail(new Error('a held transaction committed')),
        () => {},
      );
      await held;
    });
    await reload('folded');
  },

  async folded() {
    await test(TESTS.inImage, async () => {
      const after = await records('fold');
      const old = (await digest(after.get('fold'))) === state.fold;
      same([...after.keys()], old ? ['fold', 'fold#000000001', 'fold#000000002', 'fold#000000003'] : ['fold'], old ? 'the old image kept its chunks' : 'the new image stored alone');
      const back = await open();
      await restore(back, 'fold');
      same(ns(back), range(0, old ? 103 : 104), 'the rows of the image stored, and of its chunks');
      return { detail: old ? 'the old image stood' : 'the new image landed' };
    });

    await test(TESTS.fold, async () => {
      const db = await open();
      db.run('create collection t (n int, body text)');
      const pad = 'x'.repeat(200);
      for (let i = 0; i < 100; i++) db.run('put t {n: $1, body: $2}', [i, pad]);
      const image = await persist(db, 'grow');
      let folded = 0;
      for (let i = 100; i < 1000 && !folded; i++) {
        db.run('put t {n: $1, body: $2}', [i, pad]);
        if ((await persist(db, 'grow')) > image) folded = i;
      }
      ok(folded, 'the chunks were folded into an image');
      same([...(await records('grow')).keys()], ['grow'], 'the new image stored alone, its chunks deleted');
      const back = await open();
      await restore(back, 'grow');
      same(rows(back), rows(db), 'the folded image restores');
      return { detail: `folded after ${folded - 100} chunks` };
    });
  },

  async file() {
    await part(TESTS.file, async () => {
      const w = worker();
      left.push(w);
      let size = await w.call('open', 'keep.fennec');
      await w.call('run', 'create collection docs (n int, title text)');
      for (let i = 0; i < 20; i++) {
        const now = await w.call('run', 'put docs {n: $1, title: $2}', [i, `title ${i}`]);
        ok(now > size, 'each statement grows the file before it answers');
        same((await w.call('sizes', 'keep.fennec')).file, now, 'the file system holds every byte the file says it holds');
        size = now;
      }
      const due = await w.call('rows', 'get docs select n, title order n');
      same(await w.call('elsewhere', 'get docs select n, title order n'), due, 'its bytes open as a database of their own');
      // Left open: the page goes with the file in its worker's hands.
      state.keep = { rows: due, size };
    });

    // Ten writes answered, then one stopped halfway through its bytes.
    await part(TESTS.torn, async () => {
      const w = worker();
      left.push(w);
      await w.call('open', 'torn.fennec');
      await w.call('run', 'create collection docs (n int, title text)');
      for (let i = 0; i < 10; i++) await w.call('run', 'put docs {n: $1, title: "t"}', [i]);
      state.torn = (await w.call('sizes', 'torn.fennec')).file;
      await w.call('arm', 0);
      w.call('run', 'put docs {n: 10, title: "torn"}').then(
        () => fail(new Error('a stopped write answered')),
        () => {},
      );
      ok(/write of torn.fennec, half done/.test(await w.stopped), 'the write stopped halfway');
    });
    await reload('reopened');
  },

  async reopened() {
    await test(TESTS.file, async () => {
      const w = worker();
      same(await w.call('open', 'keep.fennec'), state.keep.size, 'opened again, the file is as long as it was');
      same(await w.call('rows', 'get docs select n, title order n'), state.keep.rows, 'and holds every statement');
      await w.call('close');
      w.end();
    });

    await test(TESTS.torn, async () => {
      const w = worker();
      same(await w.call('open', 'torn.fennec'), state.torn, 'cut back to the last whole record');
      same((await w.call('rows', 'get docs select n order n')).map((r) => r.n), range(0, 10), 'every answered write, the torn one not');
      await w.call('run', 'put docs {n: 11, title: "after"}');
      await w.call('close');
      await w.call('open', 'torn.fennec');
      same((await w.call('rows', 'get docs select n order n')).map((r) => r.n), [...range(0, 10), 11], 'the next write lands after the cut');
      await w.call('close');
      w.end();
    });
  },

  async rewrite() {
    // A rewrite's steps: the copy's truncate, write and flush, the file's,
    // and the copy's truncate and flush. The page is reloaded at each in
    // turn; then a compact runs through.
    const STEPS = 8;
    const check = async (where) => {
      const w = worker();
      await w.call('open', 'swap.fennec');
      same((await w.call('rows', 'get docs select n order n')).map((r) => r.n), range(20, 30), `stopped at ${where}: every row`);
      same((await w.call('sizes', 'swap.fennec')).aside, 0, `stopped at ${where}: the copy beside the file is emptied`);
      await w.call('run', 'put docs {n: 99, title: "after"}');
      const s = await w.call('sizes', 'swap.fennec');
      same(s.file, s.kept, `stopped at ${where}: the file takes the next write where it stands`);
      await w.call('close');
      w.end();
    };
    state.stops ??= [];
    const body = async () => {
      if (state.stops.length) await check(state.stops.at(-1));
      for (let k = state.stops.length; k <= STEPS; k++) {
        const w = worker();
        left.push(w);
        await w.call('remove', 'swap.fennec', 'swap.fennec~');
        await w.call('open', 'swap.fennec');
        await w.call('run', 'create collection docs (n int @hash, title text)');
        await w.call('run', `put docs [${range(0, 30).map((n) => `{n: ${n}, title: "t"}`).join(', ')}]`);
        await w.call('run', 'del docs where n < 20');
        await w.call('arm', k);
        const answered = w.call('run', 'compact');
        const where = await Promise.race([w.stopped, answered.then(() => null)]);
        if (where) {
          ok(k < STEPS, `a rewrite took more than ${STEPS} steps: stopped at ${where}`);
          state.stops.push(where);
          return true;
        }
        same(k, STEPS, 'the steps a rewrite took');
        await w.call('close');
        w.end();
        await check('none');
      }
      return false;
    };
    if (state.stops.length < STEPS) {
      if (await part(TESTS.rewrite, body)) await reload('rewrite');
    } else {
      await test(TESTS.rewrite, async () => {
        await body();
        return { detail: `reloaded at ${state.stops.join('; ')}` };
      });
    }
  },

  async refusals() {
    await test(TESTS.second, async () => {
      const a = worker();
      const b = worker();
      await a.call('open', 'lock.fennec');
      await refuses(b.call('open', 'lock.fennec'), /open elsewhere/, 'a second worker');
      await a.call('close');
      await b.call('open', 'lock.fennec');
      await b.call('close');
      a.end();
      b.end();
    });

    await test(TESTS.page, async () => {
      await refuses(openFile(await open(), 'page.fennec'), /dedicated worker/, 'openFile in the page');
    });
  },

  async quota() {
    const ROW = 100_000;
    await test(TESTS.quota, async () => {
      if (!QUOTA) return { skip: 'no quota a driver can set' };
      const db = await open();
      db.run('create collection big (n int, body text)');
      db.run('put big {n: 0, body: "small"}');
      await persist(db, 'quota');
      for (let i = 1; i * ROW < 1.5 * QUOTA; i++) db.run('put big {n: $1, body: $2}', [i, noise(ROW)]);
      await refuses(within(persist(db, 'quota')), /QuotaExceededError/, 'a persist past the quota');
      await refuses(within(putState('quota-state', noise(1.5 * QUOTA))), /QuotaExceededError/, 'putState past the quota');
      same(await getState('quota-state'), undefined, 'putState stored nothing');
      const back = await open();
      await restore(back, 'quota');
      same(ns(back, 'big'), [0], 'what was stored before stands');
      db.run('del big where n > 0');
      await persist(db, 'quota');
      const again = await open();
      await restore(again, 'quota');
      same(ns(again, 'big'), [0], 'with room again, the next persist stores the image whole');
      return { detail: `held to ${QUOTA} bytes` };
    });

    await test(TESTS.fileQuota, async () => {
      if (!QUOTA) return { skip: 'no quota a driver can set' };
      const w = worker();
      await w.call('open', 'quota.fennec');
      await w.call('run', 'create collection big (n int, body text)');
      const { answered, error } = await w.call('fill', ROW, Math.ceil((3 * QUOTA) / ROW));
      ok(error, `${answered} rows, three times the quota, and no write refused`);
      // Chrome's file system throws QuotaExceededError; Firefox's writes
      // nothing, and says so by the count it wrote.
      const full = error.cause?.name === 'QuotaExceededError' || /took none of a write/.test(error.message);
      ok(/refused a write/.test(error.message) && full, `the write past the quota is refused for room: ${error.name}: ${error.message} (${error.cause?.name})`);
      await refuses(w.call('run', 'put big {n: -1, body: "x"}'), /refused a write/, 'every write after it');
      ok((await w.call('rows', 'get big count'))[0].count > answered, 'reads go on');
      // Nothing left to write -- the refused writes went with their
      // refusals -- the file closes.
      await w.call('close');
      await w.call('open', 'quota.fennec');
      same((await w.call('rows', 'get big count'))[0].count, answered, 'opened again, the file holds every write it answered');
      same((await w.call('sizes', 'quota.fennec')).aside, 0, 'and the copy beside it is emptied');
      await w.call('close');
      w.end();
      return { detail: `${answered} rows of ${ROW} bytes answered, refused in ${error.where}` };
    });
  },

  async end() {
    // Every persist, restore and state call of this page has closed its
    // connection, or the delete is blocked.
    await test(TESTS.closed, wipe);
  },
};

const ORDER = Object.keys(PHASES);
try {
  for (let i = ORDER.indexOf(state.phase ?? 'start'); i < ORDER.length && !failed; i++) {
    state.phase = ORDER[i];
    await say(`phase ${state.phase}`);
    await PHASES[state.phase]();
    for (const db of opened.splice(0)) db.close();
  }
  if (!failed) {
    await say('done');
    await send('done');
  }
} catch (e) {
  fail(e);
}

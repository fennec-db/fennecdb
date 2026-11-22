// The part of IndexedDB `persist`, `restore` and the sync layer use, in
// memory, for the tests: Node has none. Keys are kept sorted as the real
// one keeps them; an array key (a replica's claim) is kept apart, in
// `arrays` by its JSON, as a real store sorts arrays after every string. A
// range is of strings. web/fennec.persist.test.js has its own, which logs
// writes.

export class KeyRange {
  constructor(lo, hi) {
    this.lo = lo;
    this.hi = hi;
  }
  static bound(lo, hi) {
    return new KeyRange(lo, hi);
  }
  has(k) {
    return typeof k === 'string' && k >= this.lo && k <= this.hi;
  }
}

export function fakeIndexedDB() {
  const records = new Map();
  const arrays = new Map();
  const at = (key) => (Array.isArray(key) ? [arrays, JSON.stringify(key)] : [records, key]);
  const request = (fn) => {
    const req = {};
    queueMicrotask(() => {
      req.result = fn();
      req.onsuccess?.();
    });
    return req;
  };
  const inRange = (range) => [...records.keys()].filter((k) => range.has(k)).sort();
  const store = {
    put(value, key) {
      const [m, k] = at(key);
      m.set(k, value);
    },
    delete(q) {
      if (!(q instanceof KeyRange)) {
        const [m, k] = at(q);
        return m.delete(k);
      }
      for (const k of inRange(q)) records.delete(k);
    },
    get(key) {
      const [m, k] = at(key);
      return request(() => m.get(k));
    },
    getAll: (range) => request(() => inRange(range).map((k) => records.get(k))),
    getAllKeys: (range) => request(() => inRange(range)),
  };
  const fake = { records, arrays };
  const db = {
    transaction(_name, mode) {
      // A test's IndexedDB failing a read once (`failRead`), as a real one
      // may: thrown where the transaction is made.
      if (mode === 'readonly' && fake.failRead) {
        fake.failRead = false;
        throw Object.assign(new Error('the read failed'), { name: 'UnknownError' });
      }
      const tx = { objectStore: () => store };
      setImmediate(() => tx.oncomplete?.());
      return tx;
    },
    close() {},
  };
  return Object.assign(fake, {
    open() {
      const req = {};
      queueMicrotask(() => {
        req.result = db;
        req.onsuccess?.();
      });
      return req;
    },
  });
}

/** Installs a fresh one as the global, with `IDBKeyRange`; returns it. */
export function installIndexedDB() {
  const idb = fakeIndexedDB();
  globalThis.indexedDB = idb;
  globalThis.IDBKeyRange = KeyRange;
  return idb;
}

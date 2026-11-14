// The dedicated worker a page keeps its file in (`openFile`), for
// persist.js: each message names a call on the database and its file, and
// is answered with what it gave or the error it threw.
//
// The file system `openFile` is handed is the origin's own, every write,
// truncate and flush counted on the way (`Steps`). Armed, the step it
// names stops -- a write once half its bytes are written, a truncate or a
// flush before it is made -- and the worker waits there until the page is
// loaded again, which ends it where it stands, its files still open: what
// a page closed in the middle of a flush leaves behind.

import { Fennec, openFile } from '/web/fennec.js';
import { noise } from './noise.js';

const MODULE = WebAssembly.compileStreaming(fetch('/web/fennec.wasm'));

class Steps {
  steps = 0;
  stopAt = -1;
  /** The last call the file system refused, by what and on which file. */
  refused = null;
  /** The handles `openFile` holds, by file name. */
  handles = new Map();

  constructor(root) {
    this.root = root;
  }

  /** The `k`th step from here stops (0 is the next). */
  arm(k) {
    this.stopAt = this.steps + k;
  }

  #stops() {
    return this.steps++ === this.stopAt;
  }

  #stop(what) {
    // Told first: the page reloads once it hears, and the message is on
    // its way before the wait begins.
    postMessage({ stopped: what });
    // Never woken: a SharedArrayBuffer of this worker's own, which needs
    // the page cross-origin isolated (run.mjs's headers).
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0);
  }

  async getFileHandle(name, opts) {
    const fh = await this.root.getFileHandle(name, opts);
    return { createSyncAccessHandle: async () => this.#counted(name, await fh.createSyncAccessHandle()) };
  }

  /** `call` on `name`, a refusal noted as `name`'s `what`. */
  #noted(name, what, call) {
    try {
      return call();
    } catch (e) {
      this.refused = `a ${what} of ${name}`;
      throw e;
    }
  }

  #counted(name, h) {
    const counted = {
      getSize: () => h.getSize(),
      read: (buf, opts) => h.read(buf, opts),
      write: (buf, opts) => {
        if (this.#stops()) {
          h.write(buf.subarray(0, buf.length >> 1), opts);
          this.#stop(`a write of ${name}, half done`);
        }
        const n = this.#noted(name, 'write', () => h.write(buf, opts));
        if (buf.length && !(n > 0)) this.refused = `a write of ${name}, none of it taken`;
        return n;
      },
      truncate: (n) => {
        if (this.#stops()) this.#stop(`a truncate of ${name}`);
        return this.#noted(name, 'truncate', () => h.truncate(n));
      },
      flush: () => {
        if (this.#stops()) this.#stop(`a flush of ${name}`);
        return this.#noted(name, 'flush', () => h.flush());
      },
      close: () => {
        this.handles.delete(name);
        return h.close();
      },
    };
    this.handles.set(name, counted);
    return counted;
  }
}

let steps;
let db;
let file;

const calls = {
  async open(name) {
    steps ??= new Steps(await navigator.storage.getDirectory());
    db = await Fennec.open(await MODULE);
    file = await openFile(db, name, { dir: steps });
    return file.size;
  },

  /** A statement, and the file's size once it answered. */
  run(text, params) {
    db.run(text, params);
    return file.size;
  },

  rows(text, params) {
    return db.rows(text, params);
  },

  arm(k) {
    steps.arm(k);
  },

  /** What the file and the copy beside it hold, as the file system has them. */
  sizes(name) {
    return { file: steps.handles.get(name).getSize(), aside: steps.handles.get(`${name}~`).getSize(), kept: file.size };
  },

  /** The file's bytes opened as a database of their own, read with `text`. */
  async elsewhere(text) {
    const other = await Fennec.open(await MODULE);
    try {
      other.load(file.bytes());
      return other.rows(text);
    } finally {
      other.close();
    }
  },

  close() {
    try {
      file.close();
    } finally {
      db.close();
    }
  },

  async remove(...names) {
    const root = await navigator.storage.getDirectory();
    for (const name of names) {
      await root.removeEntry(name).catch((e) => {
        if (e.name !== 'NotFoundError') throw e;
      });
    }
  },

  async estimate() {
    return navigator.storage.estimate();
  },

  /**
   * Rows of `size` random characters until a write is refused: the rows
   * answered, and the refusal.
   */
  fill(size) {
    for (let n = 0; ; n++) {
      try {
        db.run('put big {n: $1, body: $2}', [n, noise(size)]);
      } catch (e) {
        const cause = e.cause && { name: e.cause.name, message: e.cause.message };
        return { answered: n, error: { name: e.name, message: e.message, cause, where: steps.refused } };
      }
    }
  },
};

onmessage = async ({ data: { id, call, args } }) => {
  try {
    postMessage({ id, value: await calls[call](...args) });
  } catch (e) {
    postMessage({ id, error: { name: e?.name, message: e?.message ?? String(e), cause: e?.cause && { name: e.cause.name, message: e.cause.message } } });
  }
};

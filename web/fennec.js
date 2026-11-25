// fennecdb browser client.
//
// No dependencies, no build step, no npm. All it needs is `fennec.wasm`.
//
// Two layers, both taking the same path:
//
//   raw FennecQL -- synchronous, a single call
//     import { Fennec } from './fennec.js';
//     const db = await Fennec.open('./fennec.wasm');
//     db.run('create collection docs (title text, embed vector<3> @hnsw(cosine))');
//     const r = db.run('get docs near embed $1 limit 5', [vec]);
//
//   query builder -- dynamic filters, every value bound to a parameter
//     const rows = await db.from('docs')
//       .where('year', '>=', 2024)
//       .near('embed', vec, { ef: 128 })
//       .limit(10)
//       .rows();
//
// The builder hides nothing: `toFennecQL()` hands back the generated text
// and the parameters verbatim. Types: `fennec types data.fennec > fennec-schema.d.ts`.
//
// The builder (`builder.js`) and the HTTP client (`http.js`) are modules of
// their own, which this one re-exports: `@fennecdb/web/client` is the two
// without the engine, for a page whose queries run on a server.

import {
  FennecError,
  LOCAL_WAIT,
  Query,
  checked,
  collation,
  call,
  declared,
  from,
  ident,
  isSpec,
  jsonOf,
  nameOf,
  rowsOf,
  statementOf,
  whole,
} from './builder.js';
import { FennecHttp, connect, sseEvents } from './http.js';

export {
  FennecError, Query, Call, Param, from, call, or, and, not, raw, inc, expr, bucket, countDistinct, distance, first, last, param,
} from './builder.js';
export { FennecHttp, connect, sseEvents } from './http.js';

const enc = new TextEncoder();
const dec = new TextDecoder();

/**
 * Where a database opened with a schema stood once it was made: a load
 * (`restore`, `openFile`) adds the image's collections to what a database
 * holds, so one holding what its schema made and nothing since is emptied
 * first, and the one loaded checked instead.
 */
const untouched = new WeakMap();
const replaceable = (f) => untouched.get(f) === f.changeSeq;

/** The databases `close` let go of, which `persist`, `restore` and `openFile` refuse (`alive`). */
const closed = new WeakSet();

/** A `compact`, which `batch` cannot put back: a batch holding one runs each statement on its own. */
const COMPACT = /^(?:\s|--[^\n]*|#[^\n]*)*compact\b/i;

/** The module's word that bytes are no image of a database (`#take`). */
const NOT_AN_IMAGE = 'could not load image (corrupt or incompatible version)';

/** A second database over a database's module (`Fennec`'s static block). */
let sibling;
/** A database made empty again, its handle a new one (`Fennec`'s static block). */
let emptied;
/** The ints past 2^53 a database's module read (`Fennec`'s static block). */
let met;
/**
 * A block of a database's statements begun (0), landed (1) or put back (2)
 * (`Fennec`'s static block): what lands a sync's seed in parts as one.
 */
let block;
/** Whether a database's module stopped (`Fennec`'s static block). */
let stoppedOf;

// The module is built with WebAssembly SIMD (Chrome 91, Firefox 89, Safari
// 16.4). An engine without it fails to compile it with an opaque message; this
// probe -- one function returning a v128 -- turns that into one that says why.
const SIMD_PROBE = new Uint8Array([
  0, 97, 115, 109, 1, 0, 0, 0, 1, 5, 1, 96, 0, 1, 123, 3, 2, 1, 0, 10, 10, 1, 8, 0,
  65, 0, 253, 15, 253, 98, 11,
]);

function whyNoModule(err) {
  return WebAssembly.validate(SIMD_PROBE)
    ? err
    : new FennecError('fennec.wasm needs WebAssembly SIMD (Chrome 91+, Firefox 89+, Safari 16.4+)');
}

/**
 * The most bytes the module is handed at once. It reads a length as an
 * `i32`, which JavaScript makes of any number by ToInt32: 2^32 + 8 went
 * over as 8 and `NaN` or `undefined` as 0, an allocation smaller than what
 * was then written into it, over the module's heap. Past 2^31 - 1, its
 * `isize::MAX`, the module has no room for any.
 */
const MAX_LEN = 0x7fffffff;

/** `len`, refused unless it is a length the module can be handed. */
function lengthOf(len, what) {
  if (Number.isInteger(len) && len >= 0 && len <= MAX_LEN) return len;
  throw new FennecError(`${what} must be a whole number of bytes from 0 to ${MAX_LEN}, not ${String(len)}`);
}

/**
 * The module's exports, each call made through a guard. A trap -- an
 * abort where the module has no memory left to answer in, a bound it
 * checks -- leaves the instance as it stopped: its stack pointer where the
 * call left it, a statement landed and neither answered nor flushed to its
 * file. Run on, it answered from that state or trapped again; so every
 * later call, on every database of the module, throws the one error
 * saying to open the database again. A `TypeError` is a value no export
 * takes (a `BigInt`), refused before the module runs.
 *
 * Each export's guard takes the arguments it does, by name, and so has its
 * `length`, which `run` tells a module from before vectors went over apart
 * by: a guard of `(...args)` made an array of them on every call, and a
 * `run` makes five to eight. No export takes more than `GUARDS` has room
 * for, and one that did is refused here rather than handed part of what
 * it was given (`web/fennec.test.js` holds the module's to it).
 */
function guarded(exports) {
  let trap = null;
  const stopped = () =>
    new FennecError(`the module stopped (${trap?.message ?? trap}): nothing more runs on its databases; open the database again`, { cause: trap });
  // What a call that threw throws: a trap is kept, and stops every call after.
  const failed = (e) => {
    if (e instanceof TypeError) return e;
    trap = e;
    halted.add(out);
    return stopped();
  };
  const ok = () => {
    if (trap) throw stopped();
  };
  const out = {};
  for (const [name, f] of Object.entries(exports)) {
    if (typeof f !== 'function') {
      out[name] = f;
      continue;
    }
    if (f.length >= GUARDS.length) throw new FennecError(`${name} takes ${f.length} arguments, past the ${GUARDS.length - 1} its guard passes on`);
    out[name] = GUARDS[f.length](f, ok, failed);
  }
  return out;
}

/** The guarded exports whose module stopped (`guarded`): no close enters it. */
const halted = new WeakSet();

/** A guard (`guarded`) by its export's arity: `GUARDS[n](f, ok, failed)` passes `n` arguments on. */
const GUARDS = [
  (f, ok, failed) => () => {
    ok();
    try {
      return f();
    } catch (e) {
      throw failed(e);
    }
  },
  (f, ok, failed) => (a) => {
    ok();
    try {
      return f(a);
    } catch (e) {
      throw failed(e);
    }
  },
  (f, ok, failed) => (a, b) => {
    ok();
    try {
      return f(a, b);
    } catch (e) {
      throw failed(e);
    }
  },
  (f, ok, failed) => (a, b, c) => {
    ok();
    try {
      return f(a, b, c);
    } catch (e) {
      throw failed(e);
    }
  },
  (f, ok, failed) => (a, b, c, d) => {
    ok();
    try {
      return f(a, b, c, d);
    } catch (e) {
      throw failed(e);
    }
  },
  (f, ok, failed) => (a, b, c, d, e) => {
    ok();
    try {
      return f(a, b, c, d, e);
    } catch (x) {
      throw failed(x);
    }
  },
  (f, ok, failed) => (a, b, c, d, e, g) => {
    ok();
    try {
      return f(a, b, c, d, e, g);
    } catch (x) {
      throw failed(x);
    }
  },
  (f, ok, failed) => (a, b, c, d, e, g, h) => {
    ok();
    try {
      return f(a, b, c, d, e, g, h);
    } catch (x) {
      throw failed(x);
    }
  },
  (f, ok, failed) => (a, b, c, d, e, g, h, i) => {
    ok();
    try {
      return f(a, b, c, d, e, g, h, i);
    } catch (x) {
      throw failed(x);
    }
  },
];

export class Fennec {
  #wasm; #handle; #collation;
  /** The live queries (`live`), made with the first; and whether a run of them is due. */
  #lives = null;
  #due = false;
  /**
   * By text, the parameters the module asked to have as JSON (`exact`): an
   * `in $1`'s list of ids, a json field's numbers. Made at the first ask.
   */
  #asked = null;

  /**
   * The time, in milliseconds since the epoch, a statement is answered at:
   * a read of a collection whose rows expire (`@ttl`) leaves out those past
   * their time by it. The module has no clock; a test pins it.
   */
  now = Date.now;

  /**
   * Where a live query's error goes when it was given no `onError` of its
   * own (`live`); with neither, it is thrown, as `FennecSync` throws it.
   */
  onError = null;

  constructor(wasm, handle, collation = null) {
    this.#wasm = wasm;
    this.#handle = handle;
    this.#collation = collation;
  }

  static {
    // For `openFile` to try bytes in without touching the database it keeps.
    sibling = (f) => new Fennec(f.#wasm, f.#wasm.fennec_open(), f.#collation);
    emptied = (f) => {
      f.#wasm.fennec_close(f.#handle);
      f.#handle = f.#wasm.fennec_open();
    };
    met = (f) => f.#wasm.fennec_wide?.();
    stoppedOf = (f) => halted.has(f.#wasm);
    block = (f, op) => {
      const out = JSON.parse(f.#readString(f.#wasm.fennec_block(f.#handle, op)));
      if (out.kind === 'error') throw new FennecError(out.message);
    };
  }

  /**
   * Loads the WASM module and opens an empty database.
   * @param {string|BufferSource|WebAssembly.Module} src  A URL, the module
   *   bytes themselves, or the module compiled. The bytes are for Node:
   *   `fetch` cannot resolve a relative path there, so the file is read with
   *   `readFile` and handed over directly. The compiled module is for
   *   Cloudflare Workers, which import a `.wasm` file as one and refuse to
   *   compile bytes at run time.
   * @param {{collation?: string|URL|Function}} opts  Where the collation
   *   data the module does not carry comes from (`collation()`): the URL of
   *   the directory holding `<name>.bin`, or a function handed the name and
   *   returning its bytes or a `Response`. By default `collate/` beside the
   *   module, when `src` is a URL.
   */
  static async open(src = './fennec.wasm', opts = {}) {
    let mod;
    try {
      if (typeof src !== 'string') {
        mod = await WebAssembly.instantiate(src, {});
      } else {
        try {
          mod = await WebAssembly.instantiateStreaming(fetch(src), {});
        } catch {
          // Fallback for servers that return the wrong MIME type.
          mod = await WebAssembly.instantiate(await (await fetch(src)).arrayBuffer(), {});
        }
      }
    } catch (e) {
      throw whyNoModule(e);
    }
    // Instantiated from a compiled module, `instantiate` gives the instance
    // alone, and from bytes the module and the instance: read as the
    // latter, a Worker's module had no exports and `open` threw.
    const wasm = guarded((mod instanceof WebAssembly.Instance ? mod : mod.instance).exports);
    // With `schema`, what the code declares is checked and made: a load
    // (`restore`, `openFile`) checks the database it brings in again.
    const db = new Fennec(wasm, wasm.fennec_open(), opts.collation ?? beside(src));
    return checked(db, opts, 'apply').then(
      () => {
        if (opts.schema) untouched.set(db, db.changeSeq);
        return db;
      },
      (e) => {
        db.close();
        throw e;
      },
    );
  }

  /** Connects to a remote HTTP endpoint: `Fennec.connect('http://host:8080')`. */
  static connect(url, opts = {}) {
    return connect(url, opts);
  }

  get version() {
    return this.#readString(this.#wasm.fennec_version());
  }

  /** The length prefix of a returned buffer. */
  #len(ptr) {
    return new DataView(this.#wasm.memory.buffer).getUint32(ptr, true);
  }

  /**
   * Reads the returned buffer and frees it. An address comes back as the
   * `i32` its bits make, negative from 2 GiB on, where the module's memory,
   * which has no maximum, may reach: read as the unsigned one it is
   * (`>>> 0`), here, in `#readString` and where `fennec_alloc` answers.
   */
  #readBytes(at) {
    const ptr = at >>> 0;
    const len = this.#len(ptr);
    // Copies, and has to: the array outlives the buffer freed on the next
    // line, and a view into wasm memory would read whatever lands there next.
    const out = new Uint8Array(this.#wasm.memory.buffer, ptr + 4, len).slice();
    this.#wasm.fennec_free(ptr, 4 + len);
    return out;
  }

  // Decodes straight out of wasm memory. A string does not need the copy
  // `#readBytes` makes -- the decode is itself the copy -- and going through
  // it first meant every result was copied twice, which on a large result set
  // is megabytes of pure waste. The decode must finish before the free.
  #readString(at) {
    const ptr = at >>> 0;
    const len = this.#len(ptr);
    const out = dec.decode(new Uint8Array(this.#wasm.memory.buffer, ptr + 4, len));
    this.#wasm.fennec_free(ptr, 4 + len);
    return out;
  }

  /**
   * Copies a string, or bytes, into WASM memory. Never at 0, the null the
   * module answers when it has no room: written there, the bytes went over
   * the bottom of its stack.
   */
  #write(data) {
    const bytes = typeof data === 'string' ? enc.encode(data) : data;
    const len = lengthOf(bytes.length, 'a text, its parameters or collation data');
    const ptr = this.#wasm.fennec_alloc(len || 1) >>> 0;
    if (!ptr) throw new FennecError(`no room in the module for ${len} bytes`);
    this.#into(ptr, len, bytes);
    return [ptr, len];
  }

  /**
   * Writes `bytes` at `at` into the allocation of `len` at `ptr`, through a
   * view of that allocation alone: bytes past it are a `RangeError`, never
   * the module's memory beyond. A view each time: a call into the module
   * may have grown its memory, which detaches the buffer before.
   */
  #into(ptr, len, bytes, at = 0) {
    new Uint8Array(this.#wasm.memory.buffer, ptr, len).set(bytes, at);
  }

  /**
   * Runs FennecQL.
   * @param {string} sql
   * @param {Array} params  values for `$1`, `$2`...
   * @returns {{kind:string, ...}} `{columns, rows}` for row results
   */
  run(sql, params = []) {
    return this.#run(sql, params, null);
  }

  /**
   * `run`, the parameters at `asJson` sent as JSON rather than apart; a
   * live query's own run `quiet`, which has nothing to tell them.
   */
  #run(sql, params, asJson, quiet = false) {
    // A typed array sent as JSON goes as the numbers it holds.
    if (asJson) params = params.map((p, i) => (asJson.includes(i) && ArrayBuffer.isView(p) ? Array.from(p) : p));
    // A module from before vectors went over as f32s takes five arguments,
    // and would read the JSON's `null` where each vector goes.
    const [json, vectors] = this.#wasm.fennec_query.length > 5 ? vectorsApart(params, asJson) : [params, null];
    // A text whose lists the module asked for as JSON before sends them so
    // at once: sent apart, they were refused and sent again, the text and
    // its parameters read twice each run. Looked up only where a list of
    // numbers is among the parameters, so a run with none pays nothing.
    const known = vectors && !asJson ? this.#asked?.get(sql) : null;
    if (known) return this.#run(sql, params, known, quiet);
    const [sp, sl] = this.#write(sql);
    let pp = 0;
    let pl = 0;
    let vp = 0;
    let vl = 0;
    let out;
    try {
      // Not `jsonOf`: the module reads an int no double holds as the double
      // it rounds to (`as_double`), and a look here cost every `run`.
      [pp, pl] = this.#write(JSON.stringify(json));
      if (vectors) [vp, vl] = this.#write(vectors);
      // The time a read of a collection whose rows expire (`@ttl`) is
      // answered at: the module has no clock of its own.
      out = this.#readString(this.#wasm.fennec_query(this.#handle, sp, sl, pp, pl, vp, vl, this.now()));
    } finally {
      // Those written before one that could not be -- a parameter
      // `JSON.stringify` refuses, no room -- too.
      this.#wasm.fennec_free(sp, sl || 1);
      if (pp) this.#wasm.fennec_free(pp, pl || 1);
      if (vp) this.#wasm.fennec_free(vp, vl || 1);
    }
    // After an error too: the change ring says what landed, after the task.
    if (this.#lives?.size && !quiet) this.#touch();
    // Before the answer, and before an error too: a statement that failed
    // may follow ones in the same text that wrote.
    kept.get(this)?.flush();
    const res = JSON.parse(out);
    // A json field takes a list of numbers as written, not as the f32s it
    // went over apart as: the module names those, before running anything,
    // and they go again as JSON.
    // Never a place sent as JSON already: those it names are new, so this
    // ends.
    if (res.kind === 'error' && res.exact) {
      const places = (asJson ?? []).concat(res.exact);
      this.#asked ??= new Map();
      if (this.#asked.size >= 256) this.#asked.clear();
      this.#asked.set(sql, places);
      return this.#run(sql, params, places, quiet);
    }
    if (res.kind === 'error') {
      // On a sync's replica a named statement is the sync's refusal,
      // in the native core's words (`NAMED`).
      const e = new FennecError(replicas.has(this) && res.message.endsWith(UNBUILT) ? NAMED : res.message);
      // A text of several says which statement stopped it, from 0, as a
      // `/batch` does.
      if (typeof res.at === 'number') e.at = res.at;
      // What a `require n else '<reason>'` said, as a server's 412 says it.
      if (typeof res.reason === 'string') e.reason = res.reason;
      // Refused for collation data the module has not been handed: which,
      // and how many statements before this one ran (`query` runs it again
      // only when none did).
      if (res.chunks) Object.assign(e, { collation: res.chunks, ran: res.ran ?? 0 });
      throw e;
    }
    return res.kind === 'rows' ? res.result : res;
  }

  /**
   * `run`, fetching the collation data a statement is refused for and
   * running it again: the module carries `latin`, and a statement that
   * compares text of another script in a collation needs that script's
   * data first. Only a statement nothing ran before is run again -- the
   * refusal comes before it changes anything, but a statement ahead of it
   * in the same text may have.
   */
  async query(sql, params = []) {
    return this.#fetching(sql, params, false);
  }

  async #fetching(sql, params, quiet) {
    for (;;) {
      try {
        return this.#run(sql, params, null, quiet);
      } catch (e) {
        if (!e.collation || e.ran || !(await this.#fetched(e.collation))) throw e;
      }
    }
  }

  /**
   * Hands the module the collation data `names` names -- `'all'` for every
   * script's. It carries `latin`; `collate und` and `collate tr` over other
   * scripts need theirs: greek, cyrillic, middle-east, indic,
   * southeast-asia, east-asia, han, han-ext, symbols, other, smp. `query`
   * and the builder fetch what a statement needs by themselves; this is
   * for having it before the first one does. True when it fetched any.
   */
  async collation(...names) {
    return this.#fetched(names.includes('all') ? this.#collationState().chunks : names);
  }

  /** `{chunks, loaded}`: every chunk's name, and a bit each for those here. */
  #collationState() {
    return JSON.parse(this.#readString(this.#wasm.fennec_collation()));
  }

  /**
   * Fetches and hands over those of `names` the module has not got. False
   * when it had them all: what refused a statement was not their absence.
   */
  async #fetched(names) {
    const { chunks, loaded } = this.#collationState();
    const want = names.filter((name) => {
      const i = chunks.indexOf(name);
      if (i < 0) throw new FennecError(`no collation data named ${JSON.stringify(name)}; there are ${chunks.join(', ')}`);
      return !(loaded & (1 << i));
    });
    const got = await Promise.all(want.map((name) => this.#chunk(name)));
    got.forEach((bytes, k) => {
      const [ptr, len] = this.#write(bytes);
      let at;
      try {
        at = this.#wasm.fennec_add_chunk(ptr, len);
      } finally {
        this.#wasm.fennec_free(ptr, len || 1);
      }
      if (chunks[at] !== want[k]) {
        throw new FennecError(`${want[k]}.bin is not this module's collation data (another version's?)`);
      }
    });
    return want.length > 0;
  }

  async #chunk(name) {
    const from = this.#collation;
    if (!from) {
      throw new FennecError(
        `the collation data ${name} is needed: open the module with { collation } -- ` +
          `the URL of the directory holding ${name}.bin, or a function returning its bytes`,
      );
    }
    let got = typeof from === 'function' ? await from(name) : await fetch(new URL(`${name}.bin`, from));
    if (typeof got?.arrayBuffer === 'function') {
      if (got.ok === false) throw new FennecError(`${name}.bin: HTTP ${got.status}`);
      got = await got.arrayBuffer();
    }
    return got instanceof Uint8Array ? got : new Uint8Array(got.buffer ?? got);
  }

  /**
   * The database against a schema declared in code -- its description,
   * `{format, collections, migrations}` (`@fennecdb/web/schema`'s
   * `describe`) -- in the engine: `'plan'` says what an apply would do,
   * `'apply'` runs the migrations not yet recorded and makes what only
   * adds, one block, or nothing while anything is refused. Answers
   * `{applied, ran, migrations, statements, refusals}`; `Fennec.open`'s
   * `schema` is this, and throws on a refusal.
   */
  checkSchema(description, mode = 'plan') {
    const call = this.#wasm.fennec_schema;
    if (!call || mode === 'follow') {
      throw new FennecError(call ? 'a database in the page is its own: it plans or applies' : 'this fennec.wasm was built without the schema check');
    }
    const [p, l] = this.#write(typeof description === 'string' ? description : JSON.stringify(description));
    let out;
    try {
      out = JSON.parse(this.#readString(call(this.#handle, p, l, mode === 'apply' ? 1 : 0, this.now())));
    } finally {
      this.#wasm.fennec_free(p, l || 1);
    }
    if (out.kind === 'error') throw Object.assign(new FennecError(out.message), out.chunks ? { collation: out.chunks, ran: 0 } : {});
    if (out.applied) {
      if (this.#lives?.size) this.#touch();
      kept.get(this)?.flush();
    }
    return out;
  }

  /**
   * The database's schema as the engine describes a server's (`GET
   * /_schema`): a description, or with `'fennecql'` `{format, fennecql}`,
   * the statements that make it.
   */
  describe(as = 'json') {
    const out = JSON.parse(this.#readString(this.#wasm.fennec_schema(this.#handle, 0, 0, as === 'fennecql' ? 4 : 3, this.now())));
    if (out.kind === 'error') throw new FennecError(out.message);
    return out;
  }

  /**
   * `FennecHttp.batch` in the page: the same items and answers, one block,
   * each statement with its own parameters. A refusal names its statement
   * (`at`) and how many stayed applied (`completed`): none, but for a
   * batch holding a `compact`, whose statements run on their own.
   */
  async batch(items) {
    if (!Array.isArray(items) || items.length === 0) throw new FennecError('batch takes a list of statements');
    const list = items.map(statementOf);
    const whole = !list.some(([sql]) => COMPACT.test(sql));
    for (;;) {
      try {
        return this.#batch(list, whole);
      } catch (e) {
        if (!e.collation || e.completed || !(await this.#fetched(e.collation))) throw e;
      }
    }
  }

  /** The statements in this task -- nothing comes between them -- in a block (`fennec_block`) unless `whole` is false. */
  #batch(list, whole) {
    const block = (op) => {
      const out = JSON.parse(this.#readString(this.#wasm.fennec_block(this.#handle, op)));
      if (out.kind === 'error') throw new FennecError(out.message);
    };
    if (whole) block(0);
    const results = [];
    try {
      list.forEach(([sql, params], i) => {
        let r;
        try {
          r = this.#run(sql, params, null, true);
        } catch (e) {
          throw Object.assign(e, { at: i, completed: whole ? 0 : i });
        }
        const { kind, rows, facets, count, collections, message } = r;
        results.push(rows ? (facets ? { rows, facets } : { rows }) : kind === 'affected' ? { affected: count } : collections ? { collections } : { message });
      });
      if (whole) block(1);
    } catch (e) {
      if (whole) block(2);
      throw e;
    } finally {
      if (this.#lives?.size) this.#touch();
      kept.get(this)?.flush();
    }
    return { results, seq: this.changeSeq, replayed: false };
  }

  /**
   * Returns the query result as a plain array of objects, the counts a
   * `facet` asked for as its `facets`.
   */
  rows(sql, params = []) {
    return rowsOf(this.run(sql, params));
  }

  /**
   * Query builder. Validates the collection name and binds the query to
   * this connection.
   * @param {string} name
   * @returns {Query}
   */
  from(name) {
    return new Query({
      collection: ident(nameOf(name), 'collection'),
      exec: (sql, params, held) => {
        if (held?.wait !== undefined) throw new FennecError(LOCAL_WAIT);
        return this.query(sql, params);
      },
      // What `useLiveQuery` finds the query's database by.
      context: this,
      rel: declared.get(this)?.relations,
    });
  }

  /**
   * `call <name> $1`, as `FennecHttp.call` sends it. A named statement is a
   * server's: the module refuses it in words of its own ("named statements
   * are a server's; this module is built without them"), and a sync
   * replica's local database in the sync's (`NAMED`) -- so a call from a
   * page goes through the HTTP client, `connect(url).call(...)` or
   * `sync.remote.call(...)`. `{ wait }` is a server's too (`LOCAL_WAIT`).
   */
  async call(name, args, opts = {}) {
    if (opts?.wait !== undefined) throw new FennecError(LOCAL_WAIT);
    return this.query(...call(name, args, opts).toCall());
  }

  /**
   * Live query: `cb` is handed the rows now, and again after every write
   * to a collection the query reads -- a `run` of this page's, the
   * builder's writes, a `restore` or `openFile` loading the database.
   * `query` is a builder `Query`, a FennecQL text, or `[text, params]` (what
   * `toFennecQL` gives). The collections a builder query reads are known;
   * a text's are named with `{collections}`, and without them every write
   * runs it again. Returns the function that stops it.
   *
   * The writes of one task run each live query once, in a microtask after
   * it, from scratch: well under a millisecond in the page (`FennecSync.live`
   * says why nothing finer).
   *
   * @param {{onError?: Function, params?: Array, collections?: string[]}} opts
   */
  live(query, cb, opts = {}) {
    this.#lives ??= new Lives(this);
    return this.#lives.add(query, cb, opts, (sql, params) => this.#fetching(sql, params, true), opts.onError ?? this.onError);
  }

  /** The live queries looked at after the task that wrote: once, however many writes it made. */
  #touch() {
    if (this.#due) return;
    this.#due = true;
    queueMicrotask(() => {
      this.#due = false;
      this.#lives.tick();
    });
  }

  /**
   * What changed since `since`: `{seq, horizon, collections}`.
   *
   * When `collections === null` the cursor has fallen behind the change
   * ring and there is no telling which collection changed -- the caller
   * must treat everything as stale. Live queries are built on this.
   */
  changes(since = 0) {
    return JSON.parse(
      this.#readString(this.#wasm.fennec_changes(this.#handle, since)),
    );
  }

  /** Current value of the change counter. */
  get changeSeq() {
    // `since` is ahead of the counter: the Rust side never scans the ring.
    return this.changes(Number.MAX_SAFE_INTEGER).seq;
  }

  /**
   * Entry count of the change ring. Locally its only effect is how far
   * behind a live query may fall and still catch up; on overflow the
   * answer is simply "everything is stale" -- no data is lost.
   */
  setChangeCapacity(n) {
    // The module takes a `u32`: 2^32 went over as 0, a ring of one.
    if (whole(n, 'capacity') > 0xffffffff) throw new FennecError(`capacity must be at most ${0xffffffff}: ${n}`);
    this.#wasm.fennec_set_change_capacity(this.#handle, n);
  }

  /** Collection schemas (the `collections` output). */
  schemas() {
    return this.run('collections').collections ?? [];
  }

  /** Collection statistics. */
  stats() {
    return JSON.parse(this.#readString(this.#wasm.fennec_stats(this.#handle)));
  }

  /** Byte image of the whole database (to write into IndexedDB/OPFS). */
  snapshot() {
    const parts = [...this.snapshotChunks()];
    if (parts.length === 1) return parts[0];
    const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
    let at = 0;
    for (const p of parts) {
      out.set(p, at);
      at += p.length;
    }
    return out;
  }

  /**
   * The image a mebibyte at a time, each let go of in the module as it is
   * taken: the image is never in the module twice, nor whole here -- which
   * a Durable Object's 128 MB needs, its storage taking pieces anyway.
   */
  *snapshotChunks() {
    // A module from before chunks gives the image whole.
    if (!this.#wasm.fennec_snapshot_chunks) {
      yield this.#readBytes(this.#wasm.fennec_snapshot(this.#handle));
      return;
    }
    const n = this.#wasm.fennec_snapshot_chunks(this.#handle);
    for (let i = 0; i < n; i++) yield this.#readBytes(this.#wasm.fennec_snapshot_chunk(this.#handle));
  }

  /**
   * Starts keeping the writes for `drain()`, or with `false` stops;
   * `persist` and `openFile` start it themselves.
   */
  journal(on = true) {
    this.#wasm.fennec_journal(this.#handle, on ? 0 : 1);
  }

  /**
   * The writes since the last drain, `{replace, bytes}`: frames to append
   * to a stored image, or with `replace` an image to store instead.
   */
  drain() {
    const b = this.#readBytes(this.#wasm.fennec_drain(this.#handle));
    return { replace: b[0] === 1, bytes: b.subarray(1) };
  }

  /**
   * Restores from a byte image, and returns how many of its bytes that
   * took: all of them, or those before a last record a crash cut short. An
   * image whose collated text needs collation data the module has not been
   * handed is refused, as `run` refuses a statement (`e.collation`):
   * `restore` fetches it and loads the image again.
   */
  load(bytes) {
    if (!(bytes instanceof Uint8Array)) throw new FennecError('load takes the image as a Uint8Array');
    const len = bytes.length;
    const ptr = this.#room(len);
    this.#into(ptr, len, bytes);
    return this.#take(ptr, len);
  }

  /**
   * Room in the module for an image of `len`: never 0, which is the module's
   * own data, and never for a length that is not one (`lengthOf`).
   */
  #room(len) {
    if (kept.has(this)) throw new FennecError('the database is kept in a file (openFile): a load would leave the file behind');
    lengthOf(len, "an image's length");
    const ptr = this.#wasm.fennec_alloc(len) >>> 0;
    if (!ptr) throw new FennecError(`no room for an image of ${len} bytes`);
    return ptr;
  }

  /** Loads the image at `ptr`, an allocation of `len` the module takes. */
  #take(ptr, len) {
    // The module keeps the bytes it is handed and reads the documents out
    // of them, rather than copy each into memory of its own: 10 000 rows of
    // 768 dimensions restored held 97 MB, and hold 66. One from before
    // copies them, and the bytes are freed here.
    const owned = this.#wasm.fennec_load_owned;
    let r;
    try {
      r = (owned ?? this.#wasm.fennec_load)(this.#handle, ptr, len);
    } finally {
      if (!owned) this.#wasm.fennec_free(ptr, len);
    }
    if (r & 2) {
      const { chunks } = this.#collationState();
      const e = new FennecError('the image needs collation data the module has not been handed');
      throw Object.assign(e, { collation: chunks.filter((_, i) => (r >>> 2) & (1 << i)), ran: 0 });
    }
    if (r !== 0) throw new FennecError(NOT_AN_IMAGE);
    // Another database now, whose counter may stand where a live query's
    // cursor did: every live query runs again, whatever the ring says.
    if (this.#lives?.size) {
      this.#lives.stale();
      this.#touch();
    }
    return this.#wasm.fennec_loaded(this.#handle) >>> 0;
  }

  /** `load`, fetching the collation data the image needs and loading it again. */
  async loadAsync(bytes) {
    for (;;) {
      try {
        return this.load(bytes);
      } catch (e) {
        if (!e.collation || !(await this.#fetched(e.collation))) throw e;
      }
    }
  }

  /**
   * `loadAsync` over an image in parts, `length` bytes in all: `parts()`
   * gives them in order (`Uint8Array`s or `ArrayBuffer`s, iterable or async
   * iterable), each copied where it goes in the module's memory. Joined
   * first, a Durable Object's image stood in its 128 MB two and three
   * times over. Called again for collation data: a refused load lets the
   * bytes go.
   */
  async loadParts(length, parts) {
    for (;;) {
      try {
        return await this.#loadParts(length, parts());
      } catch (e) {
        if (!e.collation || !(await this.#fetched(e.collation))) throw e;
      }
    }
  }

  async #loadParts(length, parts) {
    const ptr = this.#room(length);
    let at = 0;
    const put = (p) => {
      const bytes = p instanceof ArrayBuffer ? new Uint8Array(p) : p;
      // Anything else's `length` is not its bytes': a `DataView` has none,
      // and the next part went at `ptr + NaN`, the module's address 0.
      if (!(bytes instanceof Uint8Array)) throw new FennecError('each part is a Uint8Array or an ArrayBuffer');
      if (at + bytes.length > length) throw new FennecError(`the parts hold more than the ${length} bytes said`);
      // A view each part: an await may let in a write that grows the memory.
      this.#into(ptr, length, bytes, at);
      at += bytes.length;
    };
    try {
      // A plain iterable with no await: nothing runs between its parts.
      if (parts[Symbol.asyncIterator]) for await (const p of parts) put(p);
      else for (const p of parts) put(p);
      if (at !== length) throw new FennecError(`the parts hold ${at} bytes, not the ${length} said`);
    } catch (e) {
      this.#wasm.fennec_free(ptr, length);
      throw e;
    }
    return this.#take(ptr, length);
  }

  /**
   * Lets the database go, and its file (`openFile`) as the file's `close`
   * does. After a trap it enters the module no more and throws nothing:
   * thrown, it kept the file's name from the open the error asks for.
   */
  close() {
    closed.add(this);
    this.#lives?.clear();
    try {
      kept.get(this)?.close();
    } finally {
      if (!halted.has(this.#wasm)) this.#wasm.fennec_close(this.#handle);
    }
  }
}

// ------------------------------------------------------------ live queries

/**
 * The live queries over one database: `Fennec.live`'s and `FennecSync.live`'s.
 * The change ring says which collections were written since a cursor
 * (`changes`), a block's once it lands whole, so the notice costs a write
 * nothing: it is asked for once after a burst of writes, and only by a
 * database holding a live query. Each holder says when (`tick`): a page's
 * own database after the task that wrote, a replica at the next frame. A
 * seed in parts lands in one block once its last part came, so no tick sees
 * it half way.
 */
class Lives {
  #db;
  #subs = [];
  #cursor;
  #all = false;

  constructor(db) {
    this.#db = db;
    this.#cursor = db.changeSeq;
  }

  get size() {
    return this.#subs.length;
  }

  /** A live query: `cb` handed its rows now, and again after a write it may read. */
  add(query, cb, opts, exec, onError) {
    if (typeof cb !== 'function') throw new FennecError('live(query, cb): cb must be a function');
    let rows;
    let reads;
    if (query instanceof Query) {
      const bound = query.plain().bind(exec);
      rows = () => bound.rows();
      reads = query.reads;
    } else {
      const [sql, params] = typeof query === 'string' ? [query, opts.params ?? []] : Array.isArray(query) ? query : [];
      if (typeof sql !== 'string') throw new FennecError('live: a Query, a FennecQL text, or [text, params]');
      rows = async () => rowsOf(await exec(sql, params ?? []));
      reads = null;
    }
    if (opts.collections) reads = opts.collections.map((c) => ident(c, 'collection'));
    // With none before it no tick has kept the cursor up: it starts here,
    // where the first run reads.
    if (this.#subs.length === 0) this.#cursor = this.#db.changeSeq;
    const entry = { rows, reads, cb, onError, on: true };
    this.#subs.push(entry);
    // The first rows right away: a subscriber should not start on a blank
    // screen. An error with no `onError` to go to is a rejection nothing
    // waits for, as it is at a tick.
    this.#run(entry);
    return () => {
      entry.on = false;
      const i = this.#subs.indexOf(entry);
      if (i >= 0) this.#subs.splice(i, 1);
    };
  }

  /** Every live query runs at the next tick: the database was replaced. */
  stale() {
    this.#all = true;
  }

  clear() {
    for (const e of this.#subs) e.on = false;
    this.#subs.length = 0;
  }

  /**
   * Runs again the live queries that read what was written since the last
   * tick -- collection granularity: anything finer (intersecting id sets)
   * would cost more than the local query itself.
   */
  async tick() {
    const info = this.#db.changes(this.#cursor);
    this.#cursor = info.seq;
    const dirty = this.#all || info.collections === null ? null : new Set(info.collections);
    this.#all = false;
    // Only reads since: nothing to run again, not even a query whose
    // collections are not known.
    if (dirty?.size === 0) return;
    let failed = null;
    for (const e of [...this.#subs]) {
      if (dirty && e.reads && !e.reads.some((c) => dirty.has(c))) continue;
      // One that throws does not keep the rest from their rows.
      await this.#run(e).catch((err) => {
        failed ??= { err };
      });
    }
    if (failed) throw failed.err;
  }

  async #run(e) {
    try {
      const rows = await e.rows();
      // Stopped while it ran: its rows go nowhere.
      if (e.on) e.cb(rows);
    } catch (err) {
      if (!e.on) return;
      if (e.onError) e.onError(err);
      else throw err;
    }
  }
}

/** `collate/` beside the module, where `make wasm` puts the collation data. */
function beside(src) {
  if (typeof src !== 'string' && !(src instanceof URL)) return null;
  try {
    return new URL('collate/', new URL(src, globalThis.location?.href));
  } catch {
    return null;
  }
}

// Whether this machine's typed arrays are little-endian, as the module
// reads the vectors `vectorsApart` hands it. Every browser's are.
const LITTLE = new Uint8Array(new Uint32Array([1]).buffer)[0] === 1;

/**
 * The parameters that are vectors, apart from the rest, as `f32`s: a
 * typed array, whatever numbers it holds, or an array of finite numbers,
 * which the module reads as a vector either way. Each goes over as its
 * place among the parameters and its length, then its values, and the JSON
 * holds `null` where it goes. Written out as text and read back, a page of
 * 200 768-dim vectors spent most of its time on the digits, both sides of
 * the call. A `-0` goes over as `0`, as JSON writes it, so either way
 * stores the same vector. Those at `asJson` stay in the JSON: a json field
 * keeps a list's numbers as written, which the module asks for by their
 * places. One each of whose numbers is a finite one an `f32` holds exactly
 * -- a `Float32Array`'s, an int short of 2^24, `0.5`, not `0.1` -- goes
 * marked exact, its place's top bit set, and an `[int]` or a `[float]`
 * takes it as it came (`fennec_abi::EXACT`).
 */
function vectorsApart(params, asJson = null) {
  const found = [];
  let size = 0;
  params.forEach((p, i) => {
    if (asJson?.includes(i)) return;
    const typed = !Array.isArray(p);
    const list = !typed || (ArrayBuffer.isView(p) && !(p instanceof DataView)) ? p : null;
    if (!LITTLE || !list || list.length === 0) return;
    // A typed array's numbers pass as they are, a NaN or an infinity among
    // them, as a native binding hands its `f32`s: the module refuses a
    // vector holding one where a vector field or `near` takes it, naming the
    // element -- in the JSON a NaN was `null` and the array an object, which
    // no vector field takes. An array's NaN or infinity keeps it in the
    // JSON, `null` there as a server is sent it. A number past an `f32`'s
    // range, `1e39`, goes over as the `f32`'s infinity, which the module
    // refuses so too, as a server reading its digits does; none of these
    // is exact, so a list of numbers asks for it as JSON, as written.
    let exact = true;
    for (let k = 0; k < list.length; k++) {
      const x = list[k];
      if (typeof x !== 'number' || (!typed && !Number.isFinite(x))) return;
      if (exact && !(Math.fround(x) === x && Number.isFinite(x))) exact = false;
    }
    found.push(exact ? -1 - i : i);
    size += 8 + 4 * list.length;
  });
  if (found.length === 0) return [params, null];
  const bytes = new Uint8Array(size);
  const words = new Uint32Array(bytes.buffer);
  const floats = new Float32Array(bytes.buffer);
  const json = params.slice();
  let at = 0;
  for (const f of found) {
    const i = f < 0 ? -1 - f : f;
    const n = params[i].length;
    words[at] = f < 0 ? (i | 0x80000000) >>> 0 : i;
    words[at + 1] = n;
    floats.set(params[i], at + 2);
    for (let k = at + 2; k < at + 2 + n; k++) if (floats[k] === 0) floats[k] = 0;
    json[i] = null;
    at += 2 + n;
  }
  return [json, bytes];
}

// ------------------------------------------------------------- persistence
// What is stored is what a file would hold: an image under the key, and the
// writes since, as chunks under `key#000000001`, `key#000000002`, ... A
// persist after the first stores only what was written since the last, so
// its cost follows the write rather than the database: before, every one
// wrote the whole image.

const DB_NAME = 'fennecdb';
const STORE = 'images';
/** Once the chunks outgrow this share of the image, a new image replaces them. */
const FOLD = 0.5;
/** Per database, where its chunks stand: `{key, next, image, chunks}`, and the tag the next one is sealed after (`tag`). */
const stored = new WeakMap();

const chunkKey = (key, n) => `${key}#${String(n).padStart(9, '0')}`;

// Sealed with a `CryptoKey` (AES-GCM), each record is `{v, gen, iv, data}`
// -- the image -- or `{iv, data}`, a chunk, its tag over the key it is
// stored under, the image's generation, its place and the tag of the record
// before it: a chunk moved, dropped from the middle, kept from an image
// before or put after another than the one it was sealed after is refused,
// as is one byte changed. A store cut short, which the page goes on from,
// opens as it stood or as the page went on, never with a chunk it cut
// (docs/design/js-client.md#sealed-chunks-in-a-chain). The generation is
// random and new with each image, since the chunks' numbers start again
// under it. Version 1 (no `v`, 0.2.0's) covers no tag before: it restores as
// it was sealed, and the next `persist` stores an image over it.
const te = new TextEncoder();
const CHAINED = 2;
/** The tag WebCrypto's AES-GCM appends to what it seals: 128 bits, its default. */
const TAG = 16;
const NONE = new Uint8Array(0);

/** What a record's tag covers beside its bytes: version 1's, or version 2's with the tag `before` it. */
function aad(v, key, gen, n, before) {
  if (v === 1) return te.encode(`fennecdb\0${key}\0${gen}\0${n}`);
  // Never a version 1 AAD: its prefix is another from its ninth byte on.
  const head = te.encode(`fennecdb/${v}\0${key}\0${gen}\0${n}\0`);
  const out = new Uint8Array(head.length + before.length);
  out.set(head);
  out.set(before, head.length);
  return out;
}

/** A sealed record's tag, as it is stored: the last bytes of its `data`. */
function tagOf(rec) {
  const d = ArrayBuffer.isView(rec.data) ? new Uint8Array(rec.data.buffer, rec.data.byteOffset, rec.data.byteLength) : new Uint8Array(rec.data);
  return d.slice(Math.max(0, d.length - TAG));
}

/** `bytes` sealed as version 2 at place `n` -- 0 the image -- after the record whose tag is `before`. */
async function seal(ck, key, gen, n, bytes, before) {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const data = new Uint8Array(await crypto.subtle.encrypt({ name: 'AES-GCM', iv, additionalData: aad(CHAINED, key, gen, n, before) }, ck, bytes));
  return n === 0 ? { v: CHAINED, gen, iv, data } : { iv, data };
}

async function unseal(ck, key, v, gen, n, rec, before) {
  try {
    return new Uint8Array(await crypto.subtle.decrypt({ name: 'AES-GCM', iv: rec.iv, additionalData: aad(v, key, gen, n, before) }, ck, rec.data));
  } catch {
    throw new FennecError(`${key}: record ${n} does not open: another key, the stored bytes changed or a record before it replaced`);
  }
}

const isSealed = (rec) => rec != null && rec.data != null && rec.iv != null;
/**
 * The range `key`'s chunks are in. Other keys can be too -- `notes#1` is
 * between `notes#000000000` and `notes#999999999` -- so what it holds is
 * read as `isChunk` says: the range alone, `key#` to `key#\uffff`, took
 * another key's records, `notes#archive` and its chunks among them. A key
 * named `key#` and nine digits is read as one of its chunks all the same:
 * the docs say to avoid such names, `persist` taking any.
 */
const chunkRange = (key) => IDBKeyRange.bound(chunkKey(key, 0), `${key}#999999999`);
const isChunk = (key, k) => /^\d{9}$/.test(k.slice(key.length + 1));

/** Deletes `key`'s chunks in `os`'s transaction, and no other key's. */
function dropChunks(os, key) {
  const range = chunkRange(key);
  const keys = os.getAllKeys(range);
  keys.onsuccess = () => {
    const mine = keys.result.filter((k) => isChunk(key, k));
    // One delete where the range holds this key's chunks alone, as it nearly always does.
    if (mine.length === keys.result.length) os.delete(range);
    else for (const k of mine) os.delete(k);
  };
}

/** Where a replica's token for `key` is kept: an array, never a string key `persist` or `putState` writes. */
const ownerKey = (key) => ['fennecdb/owner', key];

const randomHex = () => [...crypto.getRandomValues(new Uint8Array(8))].map((b) => b.toString(16).padStart(2, '0')).join('');

/**
 * The connection every call shares, opened by the first and kept: a call
 * opening its own and closing it took 8 to 10 ms once the page had sat
 * idle 3 s, in Chrome and Firefox alike, and Firefox 0.6 to 2.4 ms more
 * back to back (web/browser/). It lets go when a delete or an upgrade of
 * the database asks -- left open, connections held both up -- or the
 * browser closes it, and the next call opens another.
 */
let shared = null;
function idb() {
  const factory = indexedDB;
  if (shared?.factory !== factory) {
    const entry = { factory, db: null };
    const drop = () => {
      if (shared === entry) shared = null;
    };
    entry.opening = new Promise((res, rej) => {
      const req = factory.open(DB_NAME, 1);
      req.onupgradeneeded = () => req.result.createObjectStore(STORE);
      req.onsuccess = () => {
        const db = (entry.db = req.result);
        db.onversionchange = () => {
          db.close();
          drop();
        };
        db.onclose = drop;
        res(db);
      };
      req.onerror = () => {
        drop();
        rej(req.error);
      };
    });
    shared = entry;
  }
  return shared.opening;
}

/** Thrown by `inStore` asked to write over a connection that has ended. */
const ENDED = new FennecError('the connection the image was stored over has ended');

/**
 * `ask`'s requests of the store in a transaction of `mode`, done: then what
 * the function `ask` returned gives, or the transaction's error. `ask` is
 * handed the store and the connection. Past the quota Chrome aborts at the
 * commit (`abort` alone), and Firefox fails the request before the
 * transaction holds its error -- and then refused the connection's next
 * transactions and requests (InvalidStateError), no `close` said: the
 * connection a transaction failed on is let go, and a call a closed one
 * refuses is asked again of another (web/browser/). With `on`, over that
 * connection alone: once it has ended, nothing is asked and `ENDED` thrown.
 */
async function inStore(mode, ask, on = null) {
  for (let again = false; ; again = true) {
    const db = await idb();
    if (on && db !== on) throw ENDED;
    const forget = () => {
      if (shared?.db === db) shared = null;
      db.close();
    };
    let tx = null;
    let value;
    try {
      tx = db.transaction(STORE, mode);
      value = ask(tx.objectStore(STORE), db);
    } catch (e) {
      // Nothing of it lands: some of its requests may have been made.
      try {
        tx?.abort();
      } catch {}
      forget();
      if (again || e?.name !== 'InvalidStateError') throw e;
      continue;
    }
    return new Promise((res, rej) => {
      const fail = (e) => {
        forget();
        rej(e ?? new FennecError('IndexedDB aborted the transaction'));
      };
      tx.oncomplete = () => res(value?.());
      tx.onerror = (e) => fail(e?.target?.error ?? tx.error);
      tx.onabort = () => fail(tx.error);
    });
  }
}

/** The lock a replica storing under `key` holds, whatever its url, shapes or `leader` (`#lockStore`). */
const storeLock = (key) => `fennecdb/persist:${key}`;

/** How long a closed replica waits for its persist before it lets go of its key's lock (`#letGo`). */
const LET_GO_MS = 5000;

/** Per key, a replica of this page letting go of its lock: settled once the lock is free (`#letGo`). */
const letting = new Map();

/** IndexedDB's own failures reading a store or claiming it, not a store that does not open: tried again, it may (`#takeStore`). */
const unread = new WeakSet();

/** A replica's write refused: another replica claimed the store since, or it was deleted (`persistNow`'s `owner`). */
const taken = new WeakSet();

/**
 * A replica's claim on `key`'s store, made before it reads the store: a
 * token every later write of the key's finds in its own transaction, or
 * stores nothing. IndexedDB runs a store's read/write transactions one at a
 * time, each seeing the ones before: a write of the replica before lands
 * before this claim, and the read after it sees that write, or after, and
 * finds another's token. No lock manager is needed for it.
 */
async function claim(key) {
  const token = randomHex();
  try {
    await inStore('readwrite', (os) => {
      os.put(token, ownerKey(key));
    });
  } catch (e) {
    if (Object(e) === e) unread.add(e);
    throw e;
  }
  return token;
}

/**
 * Deletes what `persist` stored under `key`, its image, its chunks and a
 * replica's claim on it, and nothing else; refused while a replica keeps
 * the key in this browser (with `locks`, the one replicas were given). The
 * connection is let go of: every database's next `persist` in this page
 * stores an image.
 */
export async function unpersist(key, opts = {}) {
  const drop = () =>
    inStore('readwrite', (os, db) => {
      os.delete(key);
      dropChunks(os, key);
      os.delete(ownerKey(key));
      return () => {
        if (shared?.db === db) shared = null;
        db.close();
      };
    });
  const locks = opts.locks ?? globalThis.navigator?.locks;
  if (!locks) return drop();
  await letting.get(key);
  return locks.request(storeLock(key), { ifAvailable: true }, (lock) => {
    if (lock === null) throw new FennecError(`${key} is kept by a replica that runs in this browser (sync's persist): close it first`);
    return drop();
  });
}

/**
 * Per database, the last of its `persist`, `restore` and `openFile` calls.
 * Each reads where the one before left the database -- the next chunk's
 * number, the image's connection, the file it is kept in -- and moves it on
 * only once its own write has landed: two at once read the same number, and
 * the second chunk was stored over the first, its writes lost.
 */
const turns = new WeakMap();

/** Per database, the last `restore` called while it is out: `{prev, loaded, failed}`, `prev` the one out at its call. */
const restoring = new WeakMap();

/**
 * `run()` once every call before it on `fennec` has settled, refused or
 * not: at once when none is out. A `persist` or `openFile` (`call`) made
 * while a `restore` was out is refused when that restore was: the database
 * is not the one it waited for -- emptied, maybe, for an image that never
 * loaded -- and stored, it replaced the record the restore could not read.
 * A later restore that loaded its record answers for those before it; one
 * that found none leaves the database as they left it.
 */
function inTurn(fennec, run, call) {
  const behind = call && restoring.get(fennec);
  const go = () => {
    for (let r = behind; r && !r.loaded; r = r.prev) {
      if (r.failed) throw new FennecError(`${call} refused: the restore it waited for failed (${r.failed.message})`, { cause: r.failed });
    }
    return run();
  };
  const out = turns.get(fennec)?.then(go) ?? go();
  const done = out.then(free, free);
  function free() {
    if (turns.get(fennec) === done) turns.delete(fennec);
  }
  turns.set(fennec, done);
  return out;
}

/**
 * Writes the database into IndexedDB under `key`: the image the first time,
 * then only the writes since the last call, as a chunk. Returns the bytes
 * written. With `{cryptoKey}` (AES-GCM) each is sealed before it is stored.
 * Called while another is out, it stores what was written by its turn.
 */
export async function persist(fennec, key = 'default', opts = {}) {
  return inTurn(fennec, () => persistNow(fennec, key, opts), 'persist');
}

/**
 * Refuses a database `close` let go of, whose image is empty: stored, it
 * replaced the record and its chunks. A call that had not run by the
 * close meets it, a retry, and one that awaited before it touches the database.
 */
function alive(fennec, call) {
  if (closed.has(fennec)) throw new FennecError(`${call} refused: the database was closed`);
}

/** `image`, never an empty one -- a closed database's -- which stored would replace a record with nothing. */
function filled(image) {
  if (image?.length === 0) throw new FennecError('an empty image is never stored');
  return image;
}

/**
 * `persist`'s work, in its turn. With `owner`, a replica's token (`claim`),
 * the record is written only where the store still holds it, asked in the
 * write's own transaction; refused, the write stores nothing (`taken`).
 */
async function persistNow(fennec, key, opts, owner = null) {
  alive(fennec, 'persist');
  if (kept.has(fennec)) throw new FennecError('the database is kept in a file (openFile): persist would take the writes it appends');
  const ck = opts.cryptoKey ?? null;
  let s = stored.get(fennec);
  let image = null;
  let chunk = null;
  // A chunk follows its image only over the connection that stored the
  // image (`s.db`), open since: one that ended may have ended with the
  // store -- a delete of the database asks it to let go first
  // (`versionchange`), the browser clearing the site's data closes it
  // (`close`, or its requests refused) -- and chunks stored after would
  // restore nothing. Then an image, even with nothing written since, and
  // over a store restored as version 1 (no `tag`).
  if (s?.key !== key || s.ck !== ck || s.db !== shared?.db || (ck && !s.tag)) {
    fennec.journal();
    image = fennec.snapshot();
    s = { key, ck, db: null, gen: null, next: 1, image: 0, chunks: 0, tag: null };
    stored.set(fennec, s);
  } else {
    const { replace, bytes } = fennec.drain();
    if (replace) image = bytes;
    else if (bytes.length === 0) return 0;
    else if (s.chunks + bytes.length > s.image * FOLD) image = fennec.snapshot();
    else chunk = bytes;
  }
  const gen = image && ck ? randomHex() : s.gen;
  let on = null;
  let rec;
  let refused = false;
  try {
    filled(image);
    rec = !ck ? (image ?? chunk) : image ? await seal(ck, key, gen, 0, image, NONE) : await seal(ck, key, gen, s.next, chunk, s.tag);
    // The connection may end while the record is sealed or the call waits
    // its turn: a chunk is stored over the image's alone.
    const ask = (os, db) => {
      on = db;
      const write = () => {
        if (image) {
          os.put(rec, key);
          dropChunks(os, key);
        } else {
          os.put(rec, chunkKey(key, s.next));
        }
      };
      if (!owner) return write();
      const held = os.get(ownerKey(key));
      held.onsuccess = () => (held.result === owner ? write() : (refused = true));
    };
    await inStore('readwrite', ask, image ? null : s.db);
    if (refused) {
      const e = new FennecError(`${key}: what was written since the last persist is not stored: another replica claimed the store since, or it was deleted`);
      taken.add(e);
      throw e;
    }
  } catch (e) {
    // The drained writes are nowhere now; the next call stores an image --
    // this one, where the image's connection had ended.
    stored.delete(fennec);
    // In this call's turn still: `persist` would wait for this very call.
    // It stores an image, which a database closed meanwhile has not.
    if (e === ENDED) return persistNow(fennec, key, opts, owner);
    throw e;
  }
  // The tag the next chunk is sealed after: this record's, as stored.
  const tag = ck ? tagOf(rec) : null;
  if (image) Object.assign(s, { db: on, gen, next: 1, image: image.length, chunks: 0, tag });
  else Object.assign(s, { next: s.next + 1, chunks: s.chunks + chunk.length, tag });
  return (image ?? chunk).length;
}

/** Free-form state (cursors): next to the image, under a separate key. */
export async function putState(key, value) {
  await inStore('readwrite', (os) => {
    os.put(value, key);
  });
}

/** Reads back what `putState` wrote; `undefined` when absent. */
export async function getState(key) {
  return inStore('readonly', (os) => {
    const req = os.get(key);
    return () => req.result;
  });
}

/**
 * Restores from IndexedDB, the image and its chunks read as one file; later
 * `persist` calls go on adding chunks. Returns false when there is no record.
 * A sealed record needs the `{cryptoKey}` it was sealed with, and a key
 * refuses a record stored in the clear.
 */
export async function restore(fennec, key = 'default', opts = {}) {
  const r = { prev: restoring.get(fennec) };
  restoring.set(fennec, r);
  // Noted inside the turn: before the next call's begins.
  const out = inTurn(fennec, async () => {
    try {
      return (r.loaded = await restoreNow(fennec, key, opts));
    } catch (e) {
      throw (r.failed = e);
    }
  });
  // Out until the promise its caller holds settles: let go of beside the
  // turn, before the caller's own handlers, whose calls run. Let go of as
  // its work ended, jobs earlier, a call made between ran as one made once
  // the failure was seen.
  const answered = () => restoring.get(fennec) === r && restoring.delete(fennec);
  out.then(answered, answered);
  return out;
}

async function restoreNow(fennec, key, opts) {
  alive(fennec, 'restore');
  const ck = opts.cryptoKey ?? null;
  let on = null;
  let image, chunks;
  try {
    [image, chunks] = await inStore('readonly', (os, db) => {
      on = db;
      const img = os.get(key);
      // The values and their keys, in one order: a key's chunks alone kept.
      const ch = os.getAll(chunkRange(key));
      const ks = os.getAllKeys(chunkRange(key));
      return () => [img.result, ch.result.filter((_, i) => isChunk(key, ks.result[i]))];
    });
  } catch (e) {
    // IndexedDB's own failure, not the store's: read again, it may restore.
    if (Object(e) === e) unread.add(e);
    throw e;
  }
  if (!image) return false;
  if (isSealed(image) !== !!ck) {
    throw new FennecError(ck ? `${key} is stored in the clear, not sealed with a key` : `${key} is sealed: restore needs its cryptoKey`);
  }
  const gen = ck ? image.gen : null;
  let tag = null;
  if (ck) {
    const v = image.v ?? 1;
    if (v !== 1 && v !== CHAINED) {
      throw new FennecError(`${key} is sealed as version ${String(v)}, which a later client wrote and this one does not read`);
    }
    // Each record's tag is in the next one's AAD: read as stored, so they
    // open side by side.
    const tags = v === CHAINED ? [image, ...chunks].map(tagOf) : [];
    image = await unseal(ck, key, v, gen, 0, image, NONE);
    // Opened side by side, refused in order: the first record that does not
    // open is the one named, whichever settled first.
    const opened = await Promise.allSettled(chunks.map((c, i) => unseal(ck, key, v, gen, i + 1, c, tags[i])));
    const bad = opened.find((r) => r.status === 'rejected');
    if (bad) throw bad.reason;
    chunks = opened.map((r) => r.value);
    tag = tags.at(-1) ?? null;
  }
  let bytes = image;
  if (chunks.length) {
    bytes = new Uint8Array(chunks.reduce((n, c) => n + c.length, image.length));
    bytes.set(image, 0);
    let at = image.length;
    for (const c of chunks) {
      bytes.set(c, at);
      at += c.length;
    }
  }
  // Closed while the record was read: `emptied` would open it again.
  alive(fennec, 'restore');
  // A load that fails may leave another database -- `emptied`, no journal
  // on it, or emptied by the module for collation data it lacks, the
  // journal holding writes of the one gone -- and `persist` drained that
  // journal after the image of the one gone: nothing, or writes of a
  // database no longer there. The next stores an image.
  stored.delete(fennec);
  if (replaceable(fennec)) emptied(fennec);
  try {
    await fennec.loadAsync(bytes);
    fennec.journal();
    // Its chunks follow over the connection it was read over, as `persist`'s.
    stored.set(fennec, { key, ck, db: on, gen, next: chunks.length + 1, image: image.length, chunks: bytes.length - image.length, tag });
    // The database the code declared its schema for is this one now: checked
    // again, what it adds journaled with the rest.
    await checked(fennec, declared.get(fennec), 'apply');
  } finally {
    // Closed while collation data was fetched or the schema checked: the
    // load called the record corrupt, the check the handle not found.
    alive(fennec, 'restore');
  }
  return true;
}

// ------------------------------------------------------------- file (OPFS)
// A database kept in a file of the origin private file system holds what
// fennec-server holds on disk, byte for byte: an image, then every write since,
// appended as it is made. So a page's file opens with `fennec`, and a
// server's file loads in a page. `run` hands each statement's writes to the
// file and flushes it before it answers, where `persist` stores what was
// written when it is called.
//
// A new image -- a `compact`, or appended writes outgrowing half the image,
// folded as `persist` folds its chunks -- is written into a copy beside the
// file (`<name>~`) and flushed before the file is touched, then over the
// file, and the copy emptied. A crash at any point leaves one of the two
// whole: `openFile` takes the copy when its image is whole -- it holds every
// write the file does and more -- and the file otherwise.

/** Per database, the file it is kept in. */
const kept = new WeakMap();
/**
 * Nor before the writes appended since the image reach this: without it
 * every write to a small database folded, and a new image costs the file
 * three flushes where an append costs one -- 0.92 ms against 0.31 in
 * Chrome 153 (`make file-bench`).
 */
const FOLD_FLOOR = 64 * 1024;
const MAGIC = enc.encode('FENECDB\x01');
/** An image's head: the signature, then `[6][u64 counter][u64 body length]`. */
const HEAD = MAGIC.length + 17;

/** Where the image beginning with `head` ends, or -1 when it is not an image's head. */
function imageEnd(head) {
  if (head.length < HEAD || head[MAGIC.length] !== 6 || MAGIC.some((b, i) => head[i] !== b)) return -1;
  return HEAD + Number(new DataView(head.buffer, head.byteOffset).getBigUint64(MAGIC.length + 9, true));
}

function writeAt(h, bytes, at) {
  for (let done = 0; done < bytes.length; ) {
    const n = h.write(bytes.subarray(done), { at: at + done });
    // Firefox's answer past the quota, where Chrome throws (web/browser/).
    if (!(n > 0)) throw new FennecError('the file took none of a write: the origin is out of room, or its file system failed');
    done += n;
  }
}

function readAt(h, size) {
  const out = new Uint8Array(size);
  for (let done = 0; done < size; ) {
    const n = h.read(out.subarray(done), { at: done });
    if (!(n > 0)) throw new FennecError('the file ended before its size');
    done += n;
  }
  return out;
}

/** `h` holding `bytes` and nothing else, flushed. */
function overwrite(h, bytes) {
  h.truncate(0);
  writeAt(h, bytes, 0);
  h.flush();
}

function refused(name, e) {
  return new FennecError(`${name} refused a write (${e?.message ?? e}); it holds what was written before, and the database has to be opened from it again`, { cause: e });
}

/**
 * How long `openFile` waits for a file held elsewhere: a page loaded again
 * finds its file held until the page before's worker has ended, which
 * Chrome puts off 2 s for a busy one (web/browser/).
 */
const HELD_MS = 3000;
/** The files this worker keeps, by directory (`null`: the origin's) and name: the `FennecFile` keeping each. */
const holding = new Map();

/** A database kept in a file of the origin private file system (`openFile`). */
class FennecFile {
  #fennec; #main; #aside; #name; #size; #image; #where;
  #failed = null;
  #closed = false;

  constructor(fennec, name, main, aside, size, image, where) {
    this.#fennec = fennec;
    this.#name = name;
    this.#main = main;
    this.#aside = aside;
    this.#size = size;
    this.#image = image;
    this.#where = where;
    if (!holding.has(where)) holding.set(where, new Map());
    holding.get(where).set(name, this);
  }

  /**
   * `openFile`'s look at a name this worker keeps: refused while its
   * database runs; let go of once its module stopped, of which nothing runs
   * again: whether it was. A static method, never a static block assigning
   * it: a block runs as the module loads, a side effect a bundler keeps,
   * and with it this class and all it reaches in an app that opens no file
   * (esbuild, minified: 2.0 KB of the 51 KB an app of `connect` bundled to
   * through this module, 0.65 KB brotli).
   */
  static here(where, name) {
    const holder = holding.get(where)?.get(name);
    if (!holder) return false;
    if (!stoppedOf(holder.#fennec)) {
      throw new FennecError(`${name} is kept by another database of this worker: one database at a time writes a file; close that one, or its file, first`);
    }
    holder.close();
    return true;
  }

  /** The file's name in its directory. */
  get name() {
    return this.#name;
  }

  /** Bytes in the file. */
  get size() {
    return this.#size;
  }

  /**
   * Writes what the database wrote since the last call into the file and
   * flushes it; `run` calls it after every statement. Returns the bytes
   * written. Once the file has refused a write every later one is refused:
   * it holds what was written before, and the drained writes are nowhere.
   */
  flush() {
    const { replace, bytes } = this.#fennec.drain();
    if (!replace && bytes.length === 0) return 0;
    if (this.#failed) throw refused(this.#name, this.#failed);
    try {
      if (replace) return this.#rewrite(bytes);
      // Updates to the same rows would grow the file, and the replay of
      // the next open, without end.
      const appended = this.#size - this.#image + bytes.length;
      if (appended > Math.max(this.#image * FOLD, FOLD_FLOOR)) return this.#rewrite(this.#fennec.snapshot());
      writeAt(this.#main, bytes, this.#size);
      this.#main.flush();
      this.#size += bytes.length;
      return bytes.length;
    } catch (e) {
      this.#failed = e;
      throw refused(this.#name, e);
    }
  }

  /** The file's bytes, which `fennec` and a server open: to download them. */
  bytes() {
    this.flush();
    return readAt(this.#main, this.#size);
  }

  /**
   * Flushes and lets the file go, whatever throws: the database is kept in
   * it no longer; a second call does nothing. After a trap nothing enters
   * the module and nothing is thrown, the handles closed with nothing
   * written: left open, they refused the open the error asks for, as
   * "open elsewhere".
   */
  close() {
    if (this.#closed) return;
    this.#closed = true;
    try {
      if (!stoppedOf(this.#fennec)) this.flush();
    } finally {
      kept.delete(this.#fennec);
      const names = holding.get(this.#where);
      if (names?.get(this.#name) === this) names.delete(this.#name);
      if (names?.size === 0) holding.delete(this.#where);
      try {
        if (!stoppedOf(this.#fennec)) this.#fennec.journal(false);
      } finally {
        try {
          this.#main.close();
        } finally {
          this.#aside.close();
        }
      }
    }
  }

  /** The file holding `image` (and the writes after it) instead, beside it first. */
  #rewrite(image) {
    overwrite(this.#aside, image);
    overwrite(this.#main, image);
    this.#aside.truncate(0);
    this.#aside.flush();
    this.#size = image.length;
    this.#image = Math.max(imageEnd(image), 0);
    return image.length;
  }
}

/**
 * Synchronous access to `name` in `dir`, created if absent; held
 * elsewhere, asked for again for `HELD_MS`, and kept by a database of this
 * worker (`mine`), refused at once or taken.
 */
async function syncAccess(dir, name, mine) {
  const fh = await dir.getFileHandle(name, { create: true });
  if (typeof fh.createSyncAccessHandle !== 'function') {
    throw new FennecError('openFile needs a dedicated worker: the file system hands out synchronous access nowhere else');
  }
  const until = Date.now() + HELD_MS;
  for (;;) {
    try {
      return await fh.createSyncAccessHandle();
    } catch (e) {
      // Safari names a held file InvalidStateError, where the standard
      // (and Chrome and Firefox) say NoModificationAllowedError.
      if (e?.name !== 'NoModificationAllowedError' && e?.name !== 'InvalidStateError') throw e;
      // Kept by a database of this worker whose module stopped, its
      // handles were let go of just now: asked again at once.
      if (mine()) continue;
      if (Date.now() >= until) {
        throw new FennecError(`${name} is open elsewhere (another worker or tab): one database at a time writes a file`, { cause: e });
      }
      // Nothing tells when a handle is let go: asked again.
      await new Promise((r) => setTimeout(r, 50));
    }
  }
}

/** `f()` on the way out of an error, which what it throws never takes the place of. */
function quietly(f) {
  try {
    f();
  } catch {
    // The error on its way out says why.
  }
}

/**
 * Whether `bytes` load, tried in a database of their own. Only the module's
 * word that they are no database says no: anything else -- memory running
 * out, say -- is thrown, since the copy may be the one whole database left.
 */
function loads(fennec, bytes) {
  const trial = sibling(fennec);
  let took;
  try {
    trial.load(bytes);
    took = true;
  } catch (e) {
    // Refused for collation data: they loaded, and the text needs it.
    if (e.collation) took = true;
    // No room, a module that stopped: the copy is no less a database, and
    // a false here empties it.
    else if (e instanceof FennecError && e.message === NOT_AN_IMAGE) took = false;
    else {
      quietly(() => trial.close());
      throw e;
    }
  }
  trial.close();
  return took;
}

/**
 * `loads` of the copy a rewrite left beside `name`, which may be the one
 * whole database left: a copy that cannot be tried -- no room in the
 * module, a module that stopped -- is kept, and the error names it, every
 * `openFile` of `name` refused so until it is dealt with (the docs'
 * "Keeping a database" says how). Thrown as it came, it named neither.
 */
function copyLoads(fennec, copy, name) {
  try {
    return loads(fennec, copy);
  } catch (e) {
    throw new FennecError(
      `${name}~, the copy a rewrite of ${name} left beside it, could not be tried (${e?.message ?? e}): ` +
        `it is kept, since it may hold the database's last writes; save its bytes and open them where there is room, then remove it`,
      { cause: e },
    );
  }
}

/**
 * Keeps the database in a file of the origin private file system, the bytes
 * fennec-server keeps on disk. A file holding some is loaded into the database,
 * which must hold nothing yet; an empty one takes the database's image. From
 * then on `run` appends every statement's writes to it and flushes it before
 * it answers.
 *
 * Only in a dedicated worker, the one place the file system hands out the
 * synchronous access this needs; and one worker at a time, since a second
 * opener is refused -- as a second process over a server's file would
 * corrupt it. A file held elsewhere is waited for 3 s first; one another
 * database of this worker keeps is refused at once, or taken from it,
 * nothing written, once its module stopped (a trap).
 *
 * @param {Fennec} fennec
 * @param {string} name  the file's name in `opts.dir`
 * @param {{dir?: FileSystemDirectoryHandle}} opts  the directory, the root
 *   of the origin private file system unless given
 * @returns {Promise<FennecFile>}
 */
export async function openFile(fennec, name = 'default.fennec', opts = {}) {
  return inTurn(fennec, () => openFileNow(fennec, name, opts), 'openFile');
}

async function openFileNow(fennec, name, opts) {
  alive(fennec, 'openFile');
  if (kept.has(fennec)) throw new FennecError('the database is already kept in a file');
  if (stored.has(fennec)) throw new FennecError('persist keeps this database in IndexedDB: a file would take the writes it stores');
  const dir = opts.dir ?? (await globalThis.navigator?.storage?.getDirectory?.());
  if (!dir) throw new FennecError('openFile needs the origin private file system (navigator.storage.getDirectory)');
  const where = opts.dir ?? null;
  // Asked only once a handle is refused: a database of this worker that
  // keeps the file holds both handles until its file's close, and a handle
  // granted is no file anyone holds.
  const mine = () => FennecFile.here(where, name);
  const main = await syncAccess(dir, name, mine);
  let aside = null;
  // Closed while the file was asked for, its handles are let go of below,
  // and closed while collation data was fetched or the schema checked: the
  // load called the file corrupt, the check the handle not found.
  const live = () => alive(fennec, 'openFile');
  try {
    aside = await syncAccess(dir, `${name}~`, mine);
    live();
    const beside = aside.getSize();
    if (beside > 0) {
      // A rewrite stopped: its copy was flushed before the file was
      // touched when its image is whole, and the file was never touched
      // when it is not.
      const end = imageEnd(readAt(aside, Math.min(beside, HEAD)));
      const copy = end > 0 && end <= beside ? readAt(aside, beside) : null;
      if (copy && copyLoads(fennec, copy, name)) overwrite(main, copy);
      aside.truncate(0);
      aside.flush();
    }
    let size = main.getSize();
    let image;
    if (size === 0) {
      fennec.journal();
      const snap = filled(fennec.snapshot());
      overwrite(main, snap);
      size = snap.length;
      image = imageEnd(snap);
    } else {
      // What its own schema made, and nothing since, is nothing to lose.
      if (replaceable(fennec)) emptied(fennec);
      if (fennec.schemas().length) throw new FennecError(`${name} holds a database: open it into one that holds nothing`);
      const bytes = readAt(main, size);
      const took = await fennec.loadAsync(bytes).finally(live);
      // A last record a crash cut short: appended after, the next write
      // would be read back as the rest of it.
      if (took < size) {
        main.truncate(took);
        main.flush();
        size = took;
      }
      image = imageEnd(bytes);
      fennec.journal();
    }
    const file = new FennecFile(fennec, name, main, aside, size, Math.max(image, 0), where);
    kept.set(fennec, file);
    // Refused, or the database closed meanwhile, the file is let go of.
    try {
      await checked(fennec, opts.schema ? opts : declared.get(fennec), 'apply').finally(live);
    } catch (e) {
      quietly(() => file.close());
      throw e;
    }
    return file;
  } catch (e) {
    main.close();
    aside?.close();
    throw e;
  }
}

// -------------------------------------------------------------------- sync
//
// Local replica + server: reads come from the local side (no network),
// writes go to the server, the feedback arrives over the subscription.
// TanStack DB's model, with two differences:
//
// 1. **No incremental dataflow.** There a collection is a `Map`, and
//    re-running a filter over 50k rows on every keystroke is unacceptable,
//    which is why differential dataflow is needed. Here the local side is
//    an indexed database -- running the query from scratch is already
//    sub-millisecond. And incremental maintenance would not even be
//    *correct* for `near`: a single insert can reorder the whole top-k.
//
// 2. **Shapes are mandatory.** The whole database lives in memory and the
//    WASM32 address space is 4 GB; a client cannot pull an entire
//    collection. A subscription is a *subset*, filtered server side.
//
// One collection per shape: a seed has to be able to say "this is the whole
// collection", otherwise the cleanup step becomes ambiguous.

/// Identity base for optimistic rows. Server ids run consecutively from 1;
/// 2^52 is both far away from them and below `Number.MAX_SAFE_INTEGER`, so
/// it survives JSON intact. Ids in this range land in a sparse map in the
/// store -- the right trade for a handful of pending rows.
const TEMP_BASE = 2 ** 52;

// A read's `require n` as the builder writes it: after its clauses, before
// a `lookup`'s.
const GUARDED_READ = /^get .* require \d+( |$)/;
const GUARDED = "a `get ... require` counts the replica's rows and is not sent: a write guarded by a read goes to the server itself";
// The native core refuses the same in the same words.
const UPSERT_KEY = "an upsert into a synced collection names each document's key or id: the replica finds the row by it, and the server's copy is matched by it";
// A job queue's claim: the replica would pick its own rows and answer
// them, and the server its own -- two workers each told they hold a job.
const PICKED = "a synced write that picks its rows with `order` or `limit`, or answers them with `returning`, would pick and answer the replica's: it goes to the server itself";
// A named statement's call, definition or drop: a replica holds its shapes'
// rows, not its server's statements, so a call would run against its part
// of the rows and a definition be one the server never had. The module,
// built without them, refuses each as it reads the text, before any of it
// runs; a replica's database tells it in the native core's words
// (`Fennec#run`), the scenario file holding both runners to them.
const NAMED = "a synced database runs no named statement: `call`, `create statement` and `drop statement` go to the server through its HTTP client, and what a call writes comes back through the shapes' streams";
/** The module's words for them (fennec-ql's `UNBUILT`). */
const UNBUILT = "named statements are a server's; this module is built without them";
/** The databases a `FennecSync` keeps as its replica, while it does. */
const replicas = new WeakSet();

/** Operator spellings in a REST filter. */
const REST_OPS = {
  '=': 'eq', eq: 'eq',
  '!=': 'neq', ne: 'neq', neq: 'neq',
  '<': 'lt', lt: 'lt',
  '<=': 'lte', lte: 'lte', le: 'lte',
  '>': 'gt', gt: 'gt',
  '>=': 'gte', gte: 'gte', ge: 'gte',
  '~': 'like', like: 'like', contains: 'like',
  has: 'has',
  in: 'in',
};

/**
 * Turns a shape condition into REST query-string pairs.
 *
 * Why not the builder's condition tree: the builder emits `$1` parameters,
 * and a query string has no parameters. A free-form `?where=` would fall
 * back to string concatenation -- exactly what the builder avoids.
 * The `?field=op.value` form leaves escaping to the transport
 * (`URLSearchParams`) and type resolution to the server: no injection surface.
 *
 * The price: a shape has no `or` groups and no function calls. A shape is a
 * subset definition; once it gets complicated, a separate collection (or a
 * view) on the server is the right answer.
 */
function shapeParams(where) {
  const out = [];
  if (where == null) return out;
  if (!isSpec(where)) throw new FennecError('a shape condition must be an object');
  for (const [field, spec] of Object.entries(where)) {
    ident(field);
    if (spec === null) {
      out.push([field, 'is.null']);
      continue;
    }
    if (!isSpec(spec)) {
      out.push([field, `eq.${restValue(spec, field)}`]);
      continue;
    }
    for (const [k, v] of Object.entries(spec)) {
      if (k === 'not') {
        out.push([field, v === null ? 'not.is.null' : `not.${restOp(k2(v), field)}`]);
        continue;
      }
      out.push([field, restOp([k, v], field)]);
    }
  }
  return out;

  // `{not: {gte: 3}}` -> `['gte', 3]`
  function k2(v) {
    if (!isSpec(v)) return ['eq', v];
    const e = Object.entries(v);
    if (e.length !== 1) throw new FennecError('`not` takes a single condition');
    return e[0];
  }
  function restOp([k, v], field) {
    const op = REST_OPS[k];
    if (!op) throw new FennecError(`unknown operator \`${k}\` in shape (field: ${field})`);
    if (op === 'in') {
      // A subscription is told of its own collection's writes, and an
      // inner query's set would not follow those of the other.
      if (v instanceof Query) {
        throw new FennecError(`a shape's \`in\` takes a list, not a query (field: ${field})`);
      }
      if (!Array.isArray(v) || v.length === 0) {
        throw new FennecError(`\`in\` expects a non-empty array (field: ${field})`);
      }
      return `in.(${v.map((x) => restValue(x, field)).join(',')})`;
    }
    return `${op}.${restValue(v, field)}`;
  }
}

function restValue(v, field) {
  if (v === null || v === undefined) {
    throw new FennecError(`a shape value cannot be empty (field: ${field})`);
  }
  if (v instanceof Date) return v.toISOString();
  if (typeof v === 'object') {
    throw new FennecError(`a shape value must be a scalar (field: ${field})`);
  }
  return String(v);
}

/**
 * `create collection` text from a schema: field name, type, collation,
 * `nocase` and index as is, and the `@text` beside it (`also`). The collation too: without it the replica's
 * field compared its text by the bytes, and paged and ordered differently
 * from the server -- and without `nocase` its `=` missed what the server's
 * found.
 * `@unique` is a plain hash here, as on a replica: a batch lands rows'
 * last states one at a time, and a value moved between two collided -- and
 * a unique index over several fields an ordered one.
 */
function schemaDDL(schema) {
  const fields = schema.fields.map(
    (f) =>
      `${ident(f.name)} ${f.type}` +
      (f.collate ? ` collate ${collation(f.collate)}` : '') +
      (f.nocase ? ' nocase' : '') +
      (f.required ? ' required' : '') +
      (f.index ? ` @${f.index === 'unique' ? 'hash' : f.index}` : '') +
      (f.also ? ` @${f.also}` : ''),
  );
  for (const ix of schema.indexes ?? []) fields.push(`@${ix.replace(/^unique\(/, 'sorted(')}`);
  return `create collection if not exists ${ident(schema.name, 'collection')} (${fields.join(', ')})`;
}

function newKey() {
  if (globalThis.crypto?.randomUUID) return globalThis.crypto.randomUUID();
  return `k${Date.now().toString(36)}${Math.random().toString(36).slice(2, 10)}`;
}

/**
 * Fallback that runs the tick after at most this long when no frame
 * arrives. In a visible tab the frame lands in ~16 ms and cancels it; in a
 * hidden tab this bound is the ceiling on the delay.
 */
const TICK_FALLBACK_MS = 50;

/**
 * The sync's own collections, beside the replica's and as the native core
 * (`fennec_abi::sync`) keeps them: each shape's cursor, the writes the server
 * has not answered with what puts each back, and the rows an insert made
 * under a temporary id. In the replica, they go wherever it goes --
 * `persist`, an `openFile` -- so a page opened again sends what it had not.
 */
const SYNC_STATE = [
  'create collection if not exists _sync_shapes (collection text, shape text, cursor int, seeded bool)',
  'create collection if not exists _sync_queue (path text, body text, key text, undo text, label text)',
  'create collection if not exists _sync_temps (collection text, key text, op int, until int)',
];
const PUT_SHAPE = 'put _sync_shapes {id: $1, collection: $2, shape: $3, cursor: $4, seeded: $5}';
const PUT_OP = 'put _sync_queue {id: $1, path: $2, body: $3, key: $4, undo: $5, label: $6}';
const PUT_TEMP = 'put _sync_temps {id: $1, collection: $2, key: $3, op: $4, until: $5}';
/**
 * The server's rows a statement writes into the replica (`#putRows`), as
 * `put <c> $1`'s one parameter or as the builder's text. A chunk bounds
 * what a statement holds at once, in the page's heap and in the module's
 * memory, which never shrinks: a 300 000-row seed of 1 000 rows a
 * statement was as fast as of 500 but left the module at 38 MB against
 * 29, and 2 000 to 10 000 were up to 2.3% slower, the page's peak 27 to
 * 61 MB higher (docs/design/js-client.md#the-servers-rows).
 */
const PUT_ROWS = 500;
/**
 * A part of a seed sent in parts, its head read without its rows: the
 * server writes `{"seq":S,"part":N,"rows":[...],"more":true}`, `part` past
 * the first and `more` before the last, so a part held aside until the
 * last is parsed once, as the seed lands. Any other form is parsed whole.
 */
function seedHead(data) {
  const m = /^\{"seq":(\d+)(?:,"part":(\d+))?,"rows":\[/.exec(data);
  if (m) return { seq: Number(m[1]), part: Number(m[2] ?? 0), more: data.endsWith('],"more":true}') };
  const msg = data ? JSON.parse(data) : {};
  return { seq: msg.seq ?? 0, part: msg.part ?? 0, more: msg.more === true };
}
/** The rows of a part the leader hands a new tab of what it holds. */
const HELLO_ROWS = 2_000;
/**
 * A shape's cursor while a seed of several parts lands (`#landParts`): no
 * change of the server's, so a replica opened again holding it knows the
 * landing never ended.
 */
const LANDING = -1;
/**
 * A vector's or bytes' type: the builder's text hands its value to the
 * module as `f32`s (`vectorsApart`), where in a parameter it is digits the
 * module reads a number at a time -- a seed of 32-dim vectors' rows took
 * 1.7x as long so, 128-dim 2.4x, 512 bytes a row 1.6x.
 */
const APART = /^(vector<|bytes$)/;
/** Whether a field of `schema` is `APART`'s. */
const apartIn = (schema) => !!schema?.fields.some((f) => APART.test(f.type));
/** Whether a field of `schema` compares by a collation: the server's schema and `describe()` name it, `schemas()` does not. */
const collatedIn = (schema) => !!schema?.fields.some((f) => f.collate);
/** An answer with no `Fennec-Seq`: no stream is known to be past it. */
const NO_SEQ = Number.MAX_SAFE_INTEGER;
/** The longest a server's `retry_after` is waited: `setTimeout` fires at once past 2^31 ms. */
const RETRY_AFTER_MAX_MS = 3_600_000;
/**
 * The code fennec-server tells a token past its `exp` by, on a request's
 * 401 and on the `error` event ending a stream at the `exp`: an app
 * refreshes the token silently at it. A 401 naming no code may be cured by
 * a fresh token too -- a key rotation, a token not valid yet, an older
 * server's expiry, none sent -- so it never means "sign out" by itself.
 * The native core's `TOKEN_EXPIRED`.
 */
const TOKEN_EXPIRED = 'token_expired';

/**
 * What an answer says beyond its words -- the tenant's quota and its wait,
 * to the millisecond (`retry_after_ms`) or in the seconds `Retry-After`
 * gives -- read from the body fennec-server writes
 * beside its headers, as the native core reads it: a page on another
 * origin reads a header only where the server lists it for CORS.
 */
function said(body) {
  let b = null;
  try {
    b = JSON.parse(body);
  } catch {
    /* not JSON: nothing said */
  }
  const { quota, retry_after: s, retry_after_ms: ms, reason, code } = b ?? {};
  // The wait itself where the server says it, whole seconds otherwise.
  const wait = typeof ms === 'number' && ms > 0 ? ms : typeof s === 'number' && s > 0 ? s * 1000 : 0;
  return {
    quota: typeof quota === 'string' && quota ? quota : null,
    retryMs: Math.min(Math.floor(wait), RETRY_AFTER_MAX_MS),
    // What a `require n else '<reason>'` the server found unmet said.
    reason: typeof reason === 'string' && reason ? reason : null,
    // The code the server tells the refusal by, read from the body alone,
    // never its words, and passed on as it came: `TOKEN_EXPIRED`, or a
    // later server's.
    code: typeof code === 'string' && code ? code : null,
  };
}

/** A key's text, as the native core writes it: a text as it is, else its JSON. */
const keyText = (k) => (typeof k === 'string' ? k : JSON.stringify(k));

/**
 * Query builder that goes through the optimistic write path.
 *
 * Because `Query` clones through `this.constructor`, every step of the
 * chain stays this class: `db.from('x').where(...).delete()` still goes
 * local first, server second.
 */
class SyncQuery extends Query {
  insert(docs, opts = {}) {
    return this.context.write('insert', this, docs, opts);
  }
  // FennecQL's `insert`, applied as an insert is: a held id the replica
  // holds is refused here, before anything is sent, and one only the
  // server holds is its 409, the rows put back.
  insertNew(docs, opts = {}) {
    return this.context.write('insertNew', this, docs, opts);
  }
  update(patch, opts = {}) {
    if (opts?.wait !== undefined) throw new FennecError(LOCAL_WAIT);
    return this.context.write('update', this, patch, opts);
  }
  upsert(docs, patch, opts = {}) {
    return this.context.write('upsert', this, [docs, patch], opts);
  }
  delete(opts = {}) {
    if (opts?.wait !== undefined) throw new FennecError(LOCAL_WAIT);
    return this.context.write('delete', this, null, opts);
  }
}

/**
 * Local replica + server connection.
 *
 * ```js
 * const db = await sync({
 *   url: 'http://127.0.0.1:8080',
 *   shapes: [{ collection: 'tasks', where: { status: 'open' }, key: 'key' }],
 * });
 * await db.ready();
 *
 * const rows = await db.from('tasks').where('priority', '>=', 3).rows(); // local
 * const stop = db.live(db.from('tasks'), (rows) => render(rows));        // live
 * await db.from('tasks').insert({ title: 'new', status: 'open' });       // optimistic
 * ```
 *
 * It does what the native core does (`crates/fennec-abi/src/sync`), and the
 * two are held to one script, `integrations/sync-scenarios.json`: writes go
 * one at a time and in order, each under an `Idempotency-Key` it keeps until
 * the server answers; no answer, a 408, a 429 or a 5xx is tried again with
 * backoff -- or after the `retry_after` a quota's 429 or a move's 503 names,
 * where longer -- a 401 asks the `tokenProvider` for a token, handed a
 * `TokenRefusal` (`errorCode`, `expired`, `hadToken`, `attempt`), after a
 * wait of 1 s doubling to 60 s where the one handed over was refused too,
 * and a 507 naming a tenant's quota and any other 4xx are a refusal, put
 * back. Moving this layer onto that core instead was measured at +21 KB
 * brotli.
 */
export class FennecSync {
  #local;
  #remote;
  #url;
  #token;
  /**
   * Bumped at each other token handed over: what a request carried is this
   * number (`#carried`), so a 401 to one sent before is told from a refusal
   * of the token held now.
   */
  #tokenGen = 0;
  /** How many times in a row a token was asked for since the server last took the one held. */
  #tokenAsks = 0;
  /** The token last refused for its expiry (`#carried`): refused for its expiry again, it was handed back as it was. */
  #expiredGen = null;
  /**
   * The token the server last took (`#answered`), and the first one handed
   * over since, which it has not: an expiry of either starts the count over
   * (`#wantToken`).
   */
  #taken = null;
  #untaken = null;
  /**
   * The `TokenRefusal` the sync waits on a token for, while it does, and
   * where its ask stands: `delayed` (the wait before it runs), `asked` (the
   * provider asked, no answer yet) or `failed` (it threw, or handed over
   * none, `#tokenWhy` saying why), as the native core's `Stage` -- the
   * status's `token`.
   */
  #want = null;
  #stage = null;
  #tokenWhy = '';
  /** The provider is at work: one call is out at a time. */
  #providing = false;
  #fetch;
  #shapes = new Map();
  #lives;
  #scheduled = null;
  #flushers = [];
  #pendingTick = null;
  #closed = false;
  #abort = new AbortController();
  #ready;
  #resolveReady;
  #persistKey;
  #cryptoKey;
  #persisting = null;
  #persistAgain = false;
  #chan = null;
  #leader = true;
  #leaderMode;
  #locks;
  #release = null;
  #onError;
  #onRefused;
  #tokenProvider;
  /** A `batch` being gathered: what each write applied. */
  #batch = null;
  /** Writes the server has not answered, in order: `{n, path, body, key, undo, label, done?, count}`. */
  #queue = [];
  /** Rows of inserts under temporary ids: `{collection, key, temp, op, until}`. */
  #temps = [];
  #nextTemp = TEMP_BASE;
  #nextN = 1;
  #sending = false;
  #sendAttempt = 0;
  #sendWaiting = false;
  #schemaBusy = false;
  #schemaAttempt = 0;
  #schemaWaiting = false;
  /** Writes left from before go before the streams open, so what they bring holds them. */
  #flushing = false;
  #offline = false;
  /** Waiting for a token after a 401. */
  #paused = false;
  #fault = null;
  #waits = new Set();
  #pushed = [];
  #listeners = null;
  /** Why this tab stores nothing under `persist` (`status()`'s `unkept`), and whether to read the store again. */
  #unkept = null;
  #unkeptAgain = false;
  #taking = null;
  /** The key's lock held, or no lock manager to hold one with; its release, and its request. */
  #holding = false;
  #releaseStore = null;
  #storeAsk = null;
  /** The key's lock let go of, past the close's bound: no persist is started after the one out (`#letGo`). */
  #fenced = false;
  /** This replica's claim on the store (`claim`), which each of its writes finds there or stores nothing. */
  #owner = null;
  /** This tab holds the election: its followers are told why it stores nothing (`#unkeptBy`), and say it. */
  #elected = false;
  #theirs = null;

  constructor(local, opts) {
    this.#local = local;
    this.#url = String(opts.url).replace(/\/+$/, '');
    this.#token = opts.token ?? null;
    // The token started with is the first one handed over.
    this.#untaken = this.#token ? 0 : null;
    this.#fetch = opts.fetch ?? globalThis.fetch;
    if (typeof this.#fetch !== 'function') {
      throw new FennecError('fetch not found: pass one via opts.fetch');
    }
    this.#fetch = this.#fetch.bind(globalThis);
    this.#remote = new FennecHttp(this.#url, { token: this.#token, fetch: this.#fetch });
    this.#persistKey = opts.persist ?? null;
    this.#cryptoKey = opts.cryptoKey ?? null;
    this.#onError = opts.onError ?? null;
    this.#onRefused = opts.onRefused ?? null;
    this.#tokenProvider = opts.tokenProvider ?? null;
    this.#leaderMode = opts.leader ?? 'auto';
    // The lock manager can be supplied from outside: the default is the Web
    // Locks API, but making it injectable keeps this testable and leaves
    // room for another coordination mechanism.
    this.#locks = opts.locks ?? globalThis.navigator?.locks ?? null;
    this.#lives = new Lives(local);

    for (const raw of opts.shapes ?? []) {
      const shape = normalizeShape(raw, this.#url);
      if (this.#shapes.has(shape.collection)) {
        throw new FennecError(
          `two shapes for \`${shape.collection}\`: one collection per shape ` +
            '(a seed has to say "this is the whole collection")',
        );
      }
      this.#shapes.set(shape.collection, shape);
    }
    if (this.#shapes.size === 0) throw new FennecError('at least one shape is required');

    this.#ready = new Promise((res) => {
      this.#resolveReady = res;
    });
  }

  /**
   * The local database. For raw FennecQL: `db.local.run(...)`; a named
   * statement there is refused as the server's (`NAMED`).
   */
  get local() {
    return this.#local;
  }

  /** The remote endpoint (`FennecHttp`). For queries outside the shapes. */
  get remote() {
    return this.#remote;
  }

  /** Resolves once the first seed of every shape has landed. */
  ready() {
    return this.#ready;
  }

  /** Resolves once the server has answered every write made so far. */
  pushed() {
    return this.#queue.length ? new Promise((r) => this.#pushed.push(r)) : Promise.resolve();
  }

  /** Shape state: `{collection, cursor, seeded, connected, pending, error, quota, errorCode, token, leader, unkept}`. */
  status() {
    // While a token is wanted the error is the token's too, both said here
    // from where its ask stands and never kept as a fault, as the native
    // core's status says them: kept as one, a fault set after it -- a
    // stream's end, as streams end offline, where providers fail -- was
    // written over it, and an app handing its tokens over by the status
    // missed the ask while nothing was sent.
    const token = this.#paused && this.#want ? Object.freeze({ stage: this.#stage, refusal: this.#want }) : null;
    const fault = token ? { message: this.#tokenMessage(), quota: null, code: this.#want.errorCode } : this.#fault;
    return [...this.#shapes.values()].map((s) => ({
      collection: s.collection,
      cursor: s.cursor,
      seeded: s.seeded,
      connected: s.connected,
      pending: this.#queue.length,
      error: fault?.message ?? (s.error ? String(s.error.message ?? s.error) : null),
      // The tenant's quota the error names: a stream over the `streams`
      // quota is not connected, as one the network dropped is not.
      quota: fault?.quota ?? null,
      // The code a 401 the sync waits on for a token named: `token_expired`
      // once the token's `exp` passed, null for none.
      errorCode: fault?.code ?? null,
      // The token the sync waits on, null while it waits on none: where its
      // ask stands and the `TokenRefusal` it says or, delayed, will say.
      token,
      leader: this.#leader,
      // Why this tab stores nothing under `persist`, or a follower why the
      // tab elected to lead does not; null where it stores or the leader does.
      unkept: this.#unkept?.message ?? this.#theirs,
    }));
  }

  /** A new token: the requests from here on carry it, and what a 401 stopped goes on. */
  setToken(token) {
    // Another token than the one held: a 401 to what was sent with that one
    // is no longer a refusal of the token held. The same one handed over
    // again keeps its number, and is asked about again where refused again.
    const again = (token || null) === this.#token;
    if (!again) {
      this.#tokenGen++;
      this.#token = token || null;
      this.#remote = new FennecHttp(this.#url, { token: this.#token, fetch: this.#fetch });
      if (this.#untaken === null) this.#untaken = this.#carried();
    }
    // The token just refused, handed over again while the wait before the
    // next ask runs -- an app handing back its cached one at the status's
    // errorCode -- is the token the wait is for: it stands, or every wait
    // is skipped.
    if (this.#paused && !(again && this.#stage === 'delayed')) {
      this.#paused = false;
      this.#fault = null;
      // Another token handed over before the wait for a second ask ran
      // out: it is what the ask would have brought.
      this.#want = null;
      this.#stage = null;
      for (const w of this.#waits) {
        if (w.what !== 'token') continue;
        clearTimeout(w.t);
        this.#waits.delete(w);
      }
    }
    this.#kick();
  }

  /**
   * The network is gone (`false`): the streams end and writes wait. Back
   * (`true`): what waited goes at once, the backoff forgotten -- but not a
   * server's `retry_after`, since the network says nothing of a tenant's
   * quota, nor the wait before a token is asked for again -- and the
   * `tokenProvider` is asked again where it threw or handed over none, never
   * while it is still at work. A page has it from `online` and `offline` on
   * its own.
   */
  setOnline(on) {
    if (!on) {
      this.#offline = true;
      for (const s of this.#shapes.values()) this.#cancel(s);
      return;
    }
    this.#offline = false;
    this.#sendAttempt = this.#schemaAttempt = 0;
    for (const w of this.#waits) {
      if (w.server) continue;
      clearTimeout(w.t);
      this.#waits.delete(w);
    }
    const held = (what) => [...this.#waits].some((w) => w.what === what);
    for (const s of this.#shapes.values()) {
      s.attempt = 0;
      s.waiting = held(s);
    }
    this.#sendWaiting = held('send');
    this.#schemaWaiting = held('schema');
    // A write the server said to hold back lets the streams open, as its 429 did.
    this.#flushing = this.#queue.length > 0 && !this.#sendWaiting;
    // A provider that threw -- its issuer out of reach with the network --
    // handed over nothing, and the sync would wait for good. Not while it is
    // still at work, called a second time for one refusal.
    if (this.#paused && this.#stage === 'failed') this.#ask();
    this.#kick();
  }

  /**
   * Query builder. Reads are **local** (no network); a write goes local
   * first, then to the server.
   */
  from(name) {
    const collection = ident(nameOf(name), 'collection');
    if (!this.#shapes.has(collection)) {
      throw new FennecError(
        `no shape for \`${collection}\`: that collection is not in the local ` +
          'replica. To ask the server, use db.remote.from(...)',
      );
    }
    return new SyncQuery({
      collection,
      context: this,
      exec: (sql, params, held) => {
        if (held?.wait !== undefined) throw new FennecError(LOCAL_WAIT);
        // A read's `require` inside `batch()` would be the replica's count,
        // and the server the batch lands on never sees it: refused as the
        // native core refuses one beside a synced write.
        if (this.#batch && GUARDED_READ.test(sql)) throw new FennecError(GUARDED);
        return this.#local.query(sql, params);
      },
      rel: declared.get(this)?.relations,
    });
  }

  /**
   * Live query: re-run after every local change.
   *
   * **No** incremental maintenance, deliberately: the local query is
   * indexed and already returns in under a millisecond. An incremental
   * diff could not give the right answer for `near` anyway -- a single
   * insert can reorder the whole top-k.
   *
   * `Fennec.live`'s contract, over the replica: `query` a builder `Query`,
   * a FennecQL text or `[text, params]`. Unlike a page's own database, a
   * replica runs its live queries at the next frame (`#touch`).
   *
   * Returns: the function that ends the subscription.
   */
  live(query, cb, opts = {}) {
    return this.#lives.add(query, cb, opts, this.#localExec, opts.onError ?? this.#onError);
  }

  /**
   * Several writes as **one**: applied together, sent as one `/batch` under
   * one key, and landed by the server as one block or refused -- and put
   * back -- whole. Resolves with the number of statements once the server
   * has them.
   */
  async batch(fn) {
    if (this.#batch) throw new FennecError('nested batches are not supported');
    const b = [];
    this.#batch = b;
    try {
      await fn(this);
    } catch (e) {
      this.#batch = null;
      this.#undo(b.map((a) => a.undo));
      throw e;
    } finally {
      this.#batch = null;
    }
    if (b.length === 0) return 0;
    await this.#enqueue(b);
    return b.length;
  }

  /** Finishes any pending live-query runs (for tests). */
  async flush() {
    // When a tick is scheduled, wait for its *run* first: once the timer
    // fires `#pendingTick` is set, and that is what finishes the live queries.
    while (this.#scheduled || this.#pendingTick) {
      if (this.#pendingTick) {
        await this.#pendingTick;
        continue;
      }
      await new Promise((r) => this.#flushers.push(r));
    }
  }

  /**
   * Closes the subscriptions and gives up the lead, and the store once the
   * persist it has out settled (`#letGo`). The local database stays open.
   */
  close() {
    this.#closed = true;
    replicas.delete(this.#local);
    this.#abort.abort();
    for (const s of this.#shapes.values()) this.#cancel(s);
    for (const w of this.#waits) clearTimeout(w.t);
    this.#waits.clear();
    this.#chan?.close();
    this.#release?.();
    this.#letGo();
    this.#lives.clear();
    if (this.#listeners) {
      for (const [k, f] of this.#listeners) globalThis.removeEventListener?.(k, f);
    }
  }

  // ------------------------------------------------------------ writes

  /**
   * `SyncQuery`'s write hook: applied to the replica, what puts it back in
   * hand, and queued for the server with an idempotency key. Resolves with
   * the rows the write reached once the server has it, and rejects -- the
   * replica put back -- when the server refuses it; a network failure or a
   * server error is not a refusal, and the write waits and goes again.
   *
   * Everything up to the first `await` is **synchronous**, so the
   * optimistic row is in the local store before the caller even awaits the
   * promise. A single microtask in between would break the "visible
   * immediately" promise.
   */
  async write(verb, q, arg, opts) {
    let applied;
    const wait = this.#optimistic(() => {
      const was = met(this.#local);
      applied = this.#apply(verb, q, arg, opts);
      // `ran`: the replica's run read every value its lines send.
      if (applied?.ran && was !== undefined) applied.wide = met(this.#local) !== was;
    });
    if (wait) await wait;
    if (!applied) return 0;
    if (this.#batch) {
      this.#batch.push(applied);
      return applied.count;
    }
    return this.#enqueue([applied]);
  }

  /**
   * One statement applied to the replica: the lines the server is sent, the
   * undo -- `{c, del, put}`, the native core's -- and the temporary rows.
   *
   * - An insert into a shape with a `key` gets a key where it has none and
   *   a temporary id, and is matched with the server's copy by the key;
   *   one naming its id is applied under it. Without either it waits for
   *   the server: with nothing to match it by, the copy would leave two.
   *   An `insertNew` is sent and applied as `insert`, which refuses an id
   *   the replica holds as the server refuses one it holds.
   * - An update or a delete works by the filter, the rows it reaches read
   *   first: putting them back is the undo. A row of an insert the server
   *   has not answered has a temporary id the server never saw, so it is
   *   reached there by its key too, in a second line after the first.
   */
  #apply(verb, q, arg, opts) {
    const c = q.collection;
    const shape = this.#shapes.get(c);
    const base = q.plain();
    const key = shape.key;
    if (opts?.returning || ((verb === 'update' || verb === 'delete') && q.picks)) {
      throw new FennecError(PICKED);
    }
    if (verb === 'insert' || verb === 'insertNew') {
      const write = verb === 'insert' ? 'toInsert' : 'toInsertNew';
      const list = Array.isArray(arg) ? arg : [arg];
      if (list.length === 0) return null;
      const docs = list.map((d) => {
        const doc = { ...d };
        if (key && doc[key] == null) {
          delete doc[key];
          doc[key] = newKey();
        }
        return doc;
      });
      // `require` goes to the server with the write, and is held here too,
      // its reason with it.
      const required = { require: opts?.require, reason: opts?.reason };
      const line = base[write](docs, required);
      const named = docs.filter((d) => d.id != null).map((d) => d.id);
      if (!key && named.length < docs.length) {
        return { lines: [line], undo: { c, del: [], put: '[]' }, temps: [], count: docs.length };
      }
      const before = named.length ? this.#local.run(...from(c).where('id', 'in', named).toFennecQL()).rows ?? [] : [];
      const held = new Set(before.map((r) => r.id));
      const temps = [];
      const fresh = [];
      let next = this.#nextTemp;
      const local = docs.map((d) => {
        if (d.id != null) {
          if (!held.has(d.id)) fresh.push(d.id);
          return d;
        }
        const t = next++;
        fresh.push(t);
        if (key) temps.push([keyText(d[key]), t]);
        const { id: _, ...rest } = d;
        return { id: t, ...rest };
      });
      this.#local.run(...from(c)[write](local, required));
      this.#nextTemp = next;
      return { lines: [line], undo: { c, del: fresh, put: JSON.stringify(before) }, temps: temps.map(([k, t]) => [c, k, t]), count: docs.length, ran: true };
    }
    if (verb === 'upsert') return this.#applyUpsert(c, base, key, arg, opts);
    const stmt = verb === 'update' ? base.toUpdate(arg, opts) : base.toDelete(opts);
    // `select()` drops the projection: writing back needs every field.
    const before = this.#local.run(...base.select().toFennecQL()).rows ?? [];
    const ids = new Set(before.map((r) => r.id));
    const keys = key ? this.#temps.filter((t) => t.collection === c && ids.has(t.temp)).map((t) => t.key) : [];
    // `require` counts the rows the server's copy finds, and a row of an
    // insert it has not answered is reached there by a second line, its
    // key: the count would be split between two statements, the first
    // naming a temporary id the server never saw.
    if (keys.length && opts?.require != null) {
      throw new FennecError('a write with `require` cannot reach a row whose insert the server has not answered yet');
    }
    const count = this.#local.run(...stmt).count ?? 0;
    const lines = [stmt];
    if (keys.length) {
      const k = from(c).where(key, 'in', keys);
      lines.push(verb === 'update' ? k.toUpdate(arg) : k.toDelete());
    }
    return { lines, undo: { c, del: [], put: JSON.stringify(before) }, temps: [], count, ran: true };
  }

  /**
   * An upsert, sent as written: the server sets the row holding each
   * document's `@unique` value, its set worked out over that row again.
   * Here, where a replica's `@unique` is a plain hash, a document finds its
   * row by its id or the shape's key -- each names one, or there is nothing
   * to match the server's copy by -- and the replica runs the upsert by id:
   * a row it holds set, the rest made under temporary ids, the rows set
   * read first to put back.
   */
  #applyUpsert(c, base, key, [docs, patch], opts) {
    const list = Array.isArray(docs) ? docs : [docs];
    if (list.length === 0) return null;
    const required = { require: opts?.require, reason: opts?.reason };
    const line = base.toUpsert(list, patch, required);
    if (!list.every((d) => d.id != null || (key && d[key] != null))) {
      if (key) throw new FennecError(UPSERT_KEY);
      return { lines: [line], undo: { c, del: [], put: '[]' }, temps: [], count: list.length };
    }
    const one = (field, v) => this.#local.run(...from(c).select('id').where(field, v).limit(1).toFennecQL()).rows?.[0]?.id ?? null;
    const held = new Set();
    const fresh = [];
    const temps = [];
    // A key twice in the page: the second finds the row the first makes.
    const found = new Map();
    let next = this.#nextTemp;
    const local = list.map((d) => {
      if (d.id != null) {
        if (one('id', d.id) != null) held.add(d.id);
        else if (!fresh.includes(d.id)) fresh.push(d.id);
        return d;
      }
      const k = keyText(d[key]);
      let id = found.get(k);
      if (id === undefined) {
        id = one(key, d[key]);
        if (id != null) held.add(id);
        else {
          id = next++;
          fresh.push(id);
          temps.push([k, id]);
        }
        found.set(k, id);
      }
      const { id: _, ...rest } = d;
      return { id, ...rest };
    });
    const before = held.size ? this.#local.run(...from(c).where('id', 'in', [...held]).toFennecQL()).rows ?? [] : [];
    const count = this.#local.run(...from(c).toUpsert(local, patch, required)).count ?? 0;
    this.#nextTemp = next;
    return { lines: [line], undo: { c, del: fresh, put: JSON.stringify(before) }, temps: temps.map(([k, t]) => [c, k, t]), count, ran: true };
  }

  /**
   * What `#apply` made, queued: kept in the replica's `_sync_queue` and
   * `_sync_temps`, then sent -- by this tab when it leads, or by the one
   * that does. Resolves with the rows it reached once the server has it.
   */
  #enqueue(applied) {
    const lines = applied.flatMap((a) => a.lines);
    const n = this.#nextN++;
    const op = {
      n,
      path: lines.length === 1 ? '/query' : '/batch',
      body: applied.flatMap((a) => a.lines.map((l) => ndjson(l, a.wide))).join('\n'),
      key: `fennec-${newKey()}`,
      undo: JSON.stringify(applied.map((a) => a.undo)),
      label: lines[0][0].slice(0, 500),
      count: applied.reduce((s, a) => s + a.count, 0),
      done: null,
    };
    const p = new Promise((resolve, reject) => {
      op.done = { resolve, reject };
    });
    this.#keep(op);
    for (const a of applied) {
      for (const [collection, key, temp] of a.temps) {
        const t = { collection, key, temp, op: n, until: 0 };
        this.#temps.push(t);
        this.#saveTemp(t);
      }
    }
    this.#queue.push(op);
    this.#touch();
    this.#persistSoon();
    if (this.#leader) this.#kick();
    else this.#chan?.postMessage({ t: 'op', op: wire(op) });
    return p;
  }

  #keep(op) {
    this.#local.run(PUT_OP, [op.n, op.path, op.body, op.key, op.undo, op.label]);
  }

  #saveTemp(t) {
    this.#local.run(PUT_TEMP, [t.temp, t.collection, t.key, t.op, t.until]);
  }

  /**
   * Puts back what writes did, the last first: each step's ids deleted,
   * its rows written back -- but for the ids the server sent since
   * (`kept`, `#heard`), whose rows stay as the server sent them.
   */
  #undo(steps) {
    for (const s of steps.reverse()) {
      try {
        const kept = new Set(s.kept);
        this.#deleteLocal(s.c, s.del.filter((id) => !kept.has(id)));
        const rows = JSON.parse(s.put).filter((r) => !kept.has(r.id));
        if (rows.length) this.#local.run(...from(s.c).toInsert(rows));
      } catch (e) {
        this.#onError?.(e);
      }
    }
    this.#touch();
  }

  /**
   * Runs `fn`, the optimistic half of a write, at once, and again once the
   * collation data it was refused for is here -- refused before it changed
   * anything. Not `async`: when `fn` runs at once, as it does unless the
   * module lacks that data, nothing is awaited, and the change is visible
   * the moment `write` is called.
   */
  #optimistic(fn) {
    try {
      fn();
      return undefined;
    } catch (e) {
      if (!e.collation || e.ran) throw e;
      return this.#local.collation(...e.collation).then((fetched) => {
        if (!fetched) throw e;
        return this.#optimistic(fn);
      });
    }
  }

  get #localExec() {
    return (sql, params) => this.#local.query(sql, params);
  }

  #deleteLocal(collection, ids) {
    if (ids.length === 0) return 0;
    return this.#local.run(...from(collection).where('id', 'in', ids).toDelete()).count ?? 0;
  }

  // ----------------------------------------------------------- driving

  /** Asks for whatever is due: the schemas, the queue's next write, the streams. */
  #kick() {
    if (this.#closed || this.#offline) return;
    if ([...this.#shapes.values()].some((s) => s.rebuild || !s.made)) {
      this.#schemas().catch((e) => this.#onError?.(e));
      return;
    }
    if (!this.#leader) return;
    this.#pump().catch((e) => this.#onError?.(e));
    if (this.#paused || (this.#flushing && this.#queue.length)) return;
    for (const s of this.#shapes.values()) this.#connect(s);
  }

  /**
   * Waits out the backoff for `attempt` -- 250 ms doubling to 15 s, 30%
   * jitter on top, so a server back up is not met by every client at once
   * -- or the server's `retryMs` where that is longer, never cut to the
   * 15 s, the jitter on top all the same: the clients a quota refused in
   * one moment are told one `Retry-After`. Then runs `then`. `what` is the
   * send, the schema or a shape, which `setOnline` keeps waiting on a
   * server's word. The timer does not hold up a Node process's exit.
   */
  #wait(what, attempt, retryMs, then) {
    const base = Math.max(Math.min(250 * 2 ** Math.min(attempt, 10), 15000), retryMs);
    const w = {
      what,
      server: retryMs > 0,
      t: setTimeout(() => {
        this.#waits.delete(w);
        if (!this.#closed) then();
      }, base + Math.random() * base * 0.3),
    };
    w.t.unref?.();
    this.#waits.add(w);
  }

  #setFault(message, status, connection, quota = null) {
    this.#fault = { message, status, connection, quota };
  }

  #clearConnectionFault() {
    if (this.#fault?.connection) this.#fault = null;
  }

  /** The token a request sent now carries: the number of the one handed over last, null for none. */
  #carried() {
    return this.#token ? this.#tokenGen : null;
  }

  /**
   * The server answered a request, or a stream's head, that carried
   * `carried` with `status`. A 2xx to the token held is the server taking
   * it, and the next ask is a first one again; nothing else is -- a 408, a
   * 429 or a 5xx may come before a token is read (the connection ceiling's
   * 503, a router's while a tenant moves).
   */
  #answered(status, carried) {
    if (status >= 200 && status < 300 && carried === this.#carried()) {
      this.#tokenAsks = 0;
      this.#taken = carried;
      this.#untaken = null;
    }
  }

  /**
   * A 401 to a request, or a stream, that carried `carried`. Sent before a
   * token handed over since -- or none -- it goes again with that one, the
   * provider asked nothing: the refusal is of a token no longer held, and
   * counted, it would make the next ask wait for a refusal that crossed
   * the handover. Returns whether a token is wanted.
   */
  #tokenRefused(code, carried) {
    if (carried !== this.#carried()) return false;
    this.#wantToken(code, carried);
    return true;
  }

  /**
   * Nothing is sent until a token is handed over, and the `tokenProvider`
   * is asked for one (`#ask`) -- at once the first time in a row, after 1 s
   * doubling to 60 s where the token handed over at the last ask was
   * refused too: one minted again at once is refused for what minting does
   * not change, and a provider that hands one over at every ask would loop
   * at the network's speed. An expiry of a token that was good is no such
   * refusal, and starts the count again: of the token the server took
   * last, which ran out in use, and of the first handed over since, which
   * may have run out before it was sent -- a page offline. Any other token
   * refused for its expiry was minted expired, as every one is where the
   * server's clock runs ahead of the issuer's by a token's lifetime, and
   * asked for again at once each would cost an issuer's call a round trip:
   * it counts, as does the token already refused for its expiry, handed
   * back as it was. Asked already, a second 401 -- a stream's at the `exp`
   * beside a write's -- asks nothing more.
   */
  #wantToken(code, carried) {
    if (this.#paused) return;
    this.#paused = true;
    const expired = code === TOKEN_EXPIRED && carried !== null;
    if (expired && carried !== this.#expiredGen && (carried === this.#taken || carried === this.#untaken)) {
      this.#tokenAsks = 1;
    } else this.#tokenAsks++;
    if (expired) this.#expiredGen = carried;
    const attempt = this.#tokenAsks;
    this.#want = Object.freeze({ errorCode: code, expired: code === TOKEN_EXPIRED, hadToken: carried !== null, attempt });
    this.#stage = 'delayed';
    if (attempt === 1) return this.#ask();
    // A floor, as a server's word is: kept when the network comes back.
    this.#wait('token', 0, Math.min(1000 * 2 ** Math.min(attempt - 2, 6), 60000), () => this.#ask());
  }

  /**
   * Hands the `tokenProvider` the `TokenRefusal` wanted: the code the 401
   * named (`errorCode`, `token_expired` where the token's `exp` passed),
   * whether that is an expiry (`expired`), whether the refused request
   * carried a token (`hadToken`), and how many times in a row it is asked
   * since the server last took the token held (`attempt`). `status()`'s
   * `token` says the same, asked, for an app that hands its tokens over
   * with `setToken`.
   */
  #ask() {
    const refusal = this.#want;
    if (!refusal || this.#closed) return;
    this.#stage = 'asked';
    // One call out at a time: called again while at work -- the app's own
    // `setToken` went on and was refused meanwhile -- a provider would mint
    // twice for one refusal, and the second token, coming late, end the
    // wait the first one's refusal began. Its answer answers this ask too.
    // Nothing bounds the call, which may wait on a sign-in: one that never
    // returns is called no second time, and the app's own `setToken` gets
    // the sync going.
    if (!this.#tokenProvider || this.#providing) return;
    this.#providing = true;
    Promise.resolve()
      .then(() => this.#tokenProvider(refusal))
      .then(
        (t) => {
          this.#providing = false;
          if (this.#closed) return;
          if (t) this.setToken(t);
          else this.#tokenFailed('it handed over no token');
        },
        (e) => {
          this.#providing = false;
          // The status first: an `onError` that throws leaves it said.
          if (!this.#closed) this.#tokenFailed(String(e?.message ?? e));
          this.#onError?.(e);
        },
      );
  }

  /**
   * The provider threw, or handed over none: the status's `token` says so,
   * `failed` beside the refusal it was handed, its error why, and
   * `setOnline(true)` asks again, as the native core does at
   * `{"token_failed":..}`. One answering an ask the sync no longer waits on
   * says nothing of the token wanted now.
   */
  #tokenFailed(why) {
    if (!this.#paused || this.#stage !== 'asked') return;
    this.#stage = 'failed';
    this.#tokenWhy = why;
  }

  /** The status's error while a token is wanted, by where its ask stands (the core's `Want::message`). */
  #tokenMessage() {
    const r = this.#want;
    if (this.#stage === 'delayed') return 'the token handed over was refused too (401): another is asked for after a wait';
    if (this.#stage === 'failed') return this.#tokenWhy ? `the token provider failed: ${this.#tokenWhy}` : 'the token provider failed';
    if (r.expired) return 'the token expired (401): a fresh one is wanted';
    return r.hadToken ? 'the server refused the token (401): a fresh one is wanted' : 'the server wants a token (401): none was sent';
  }

  /**
   * The server's collections, made in the replica as it declares them --
   * field order is part of the record encoding, and rows cannot be decoded
   * if the two drift -- and made again when its schema changed.
   */
  async #schemas() {
    if (this.#schemaBusy || this.#schemaWaiting || this.#paused) return;
    this.#schemaBusy = true;
    const carried = this.#carried();
    const [status, , body] = await this.#call('GET', '/collections', null, {});
    this.#schemaBusy = false;
    if (this.#closed) return;
    this.#answered(status, carried);
    // Refused for a token since replaced, it is asked again with that one.
    if (status === 401) return this.#tokenRefused(said(body).code, carried) || this.#kick();
    try {
      if (status < 200 || status > 299) throw new FennecError(`could not read the server's collections: ${why(status, body)}`);
      await this.#make(JSON.parse(body));
      this.#schemaAttempt = 0;
    } catch (e) {
      const s = said(body);
      this.#setFault(String(e.message ?? e), status, true, s.quota);
      this.#onError?.(e);
      this.#schemaWaiting = true;
      this.#wait('schema', this.#schemaAttempt++, s.retryMs, () => {
        this.#schemaWaiting = false;
        this.#kick();
      });
      return;
    }
    this.#kick();
  }

  async #make(all) {
    for (const s of this.#shapes.values()) {
      if (s.made && !s.rebuild) continue;
      const schema = all.find((x) => x.name === s.collection);
      if (!schema) throw new FennecError(`the server has no \`${s.collection}\` collection`);
      const again = s.rebuild;
      await this.#remake(s, schema);
      if (again) this.#relay({ t: 'schema', c: s.collection, s: schema });
    }
  }

  /**
   * `shape`'s collection made as `schema` says: anew, or again over the one
   * the replica holds -- its rows kept in the fields that are left, so it
   * reads as it did until the seed writes the shape over it, and the rows of
   * writes not yet answered stay as a seed keeps them.
   */
  async #remake(s, schema) {
    const c = s.collection;
    const had = this.#local.schemas().some((x) => x.name === c);
    s.apart = apartIn(schema);
    s.collated = collatedIn(schema);
    if (had) {
      const fields = new Set(schema.fields.map((f) => f.name));
      const rows = (this.#local.run(`get ${c}`).rows ?? []).map((r) =>
        Object.fromEntries(Object.entries(r).filter(([k, v]) => (k === 'id' || fields.has(k)) && v !== null)),
      );
      this.#local.run(`drop collection ${c}`);
      this.#local.run(schemaDDL(schema));
      await this.#putRows(s, rows);
      // The code's schema checked again: the server's changed under it.
      const d = declared.get(this);
      if (d) checked(this.#remote, d, 'follow').catch((e) => this.#onError?.(e));
    } else {
      this.#local.run(schemaDDL(schema));
    }
    s.fields = new Set(schema.fields.map((f) => f.name));
    s.made = true;
    s.rebuild = false;
    this.#touch();
    this.#persistSoon();
  }

  /**
   * The server's schema is not the replica's: a change said so, or rows came
   * with a field the replica has not got. The stream ends, the collection is
   * made again from `GET /collections`, and the shape is sent whole -- a
   * rename or a drop is not told apart from the rows.
   */
  #refresh(s) {
    this.#cancel(s);
    s.rebuild = true;
    s.fresh = true;
    this.#kick();
  }

  /** Whether every field `rows` name is one the replica's collection has. */
  #fits(s, rows) {
    for (const r of rows) for (const k in r) if (k !== 'id' && !s.fields.has(k)) return false;
    return true;
  }

  /** Sends the queue's next write: one at a time and in order, so the server ends holding the last. */
  async #pump() {
    if (this.#sending || this.#sendWaiting || this.#paused || this.#offline || this.#closed) return;
    const op = this.#queue[0];
    if (!op) return;
    this.#sending = true;
    const type = op.path === '/batch' ? 'application/x-ndjson' : 'application/json';
    const carried = this.#carried();
    const [status, seq, body] = await this.#call('POST', op.path, op.body, { 'content-type': type, 'idempotency-key': op.key });
    this.#sending = false;
    if (this.#closed) return;
    this.#answered(status, carried);
    const s = status >= 200 && status < 300 ? null : said(body);
    if (!s) {
      this.#sendAttempt = 0;
      this.#clearConnectionFault();
      this.#landed(op, seq);
    } else if (status === 401) {
      // Kept either way; refused for a token since replaced, it goes again
      // with that one as the kick below pumps.
      this.#tokenRefused(s.code, carried);
    } else if (status === 507 && s.quota) {
      // A tenant past its quota of bytes or rows: waiting makes no room,
      // deleting does, and a write that only deletes always lands. Held,
      // this write would hold every write behind it -- the delete that
      // would make its room among them -- for good, so it is a refusal. A
      // 507 naming none (past `--max-memory`, or a proxy's) is waited out.
      this.#refused(op, status, why(status, body), s.quota);
    } else if (status === 0 || status === 408 || status === 429 || status >= 500) {
      // No answer, or one that says to come back: the write stays, and goes
      // again under its key, which the server answers as the first time if
      // the first reached it -- after the `retry_after` a quota's 429 names,
      // or a move's or a failover's 503, where that is longer than the
      // backoff.
      this.#setFault(why(status, body), status, true, s.quota);
      this.#flushing = false;
      this.#sendWaiting = true;
      this.#wait('send', this.#sendAttempt++, s.retryMs, () => {
        this.#sendWaiting = false;
        this.#kick();
      });
    } else {
      this.#refused(op, status, why(status, body), s.quota, s.reason);
    }
    this.#kick();
  }

  /** A request's `[status, Fennec-Seq, body]`; status 0 when none came. */
  async #call(method, path, body, headers) {
    if (this.#token) headers.authorization = `Bearer ${this.#token}`;
    try {
      const res = await this.#fetch(`${this.#url}${path}`, { method, headers, body, signal: this.#abort.signal });
      return [res.status, Number(res.headers?.get?.('fennec-seq')) || 0, await res.text()];
    } catch (e) {
      return [0, 0, String(e?.message ?? e)];
    }
  }

  /** The server took `op`: it leaves the queue, and its rows wait for the server's copies. */
  #landed(op, seq) {
    const at = this.#queue.indexOf(op);
    if (at < 0) return;
    this.#queue.splice(at, 1);
    this.#local.run('del _sync_queue where id = $1', [op.n]);
    for (const t of this.#temps) {
      if (t.op !== op.n) continue;
      t.op = 0;
      t.until = seq || NO_SEQ;
      this.#saveTemp(t);
    }
    // A stream already past it: the server's copy came, or the shape does
    // not hold the row.
    for (const s of this.#shapes.values()) this.#dropPassed(s.collection, s.cursor);
    this.#settled(op, { t: 'ans', key: op.key, status: 200, seq });
    op.done?.resolve(op.count);
  }

  /**
   * The server refused `op`: what it did is put back, the last statement's
   * first, and the app told -- with the tenant's quota where one refused
   * it, so a page says "storage full" rather than "refused", and the reason
   * a `require n else '<reason>'` gave, so it says "no seats left".
   */
  #refused(op, status, message, quota = null, reason = null) {
    const at = this.#queue.indexOf(op);
    if (at < 0) return;
    this.#queue.splice(at, 1);
    this.#undo(JSON.parse(op.undo));
    this.#rebase(op);
    this.#local.run('del _sync_queue where id = $1', [op.n]);
    const gone = this.#temps.filter((t) => t.op === op.n).map((t) => t.temp);
    this.#deleteLocal('_sync_temps', gone);
    this.#temps = this.#temps.filter((t) => t.op !== op.n);
    this.#setFault(message, status, false, quota);
    this.#settled(op, { t: 'ans', key: op.key, status, message, quota, reason });
    const e = Object.assign(new FennecError(message), { status, query: op.label }, quota && { quota }, reason && { reason });
    if (!op.foreign) this.#onRefused?.(e);
    if (op.done) op.done.reject(e);
  }

  #settled(op, ans) {
    if (!this.#queue.length) {
      this.#flushing = false;
      for (const r of this.#pushed.splice(0)) r();
    }
    this.#touch();
    this.#persistSoon();
    this.#relay(ans);
  }

  /**
   * The temporary rows of `collection` the server answered for at or before
   * `cursor`: the server's copy is here by now, or the shape does not hold it.
   */
  #dropPassed(collection, cursor) {
    const gone = this.#temps.filter((t) => t.collection === collection && t.op === 0 && t.until <= cursor);
    if (!gone.length) return;
    const ids = gone.map((t) => t.temp);
    this.#deleteLocal(collection, ids);
    this.#deleteLocal('_sync_temps', ids);
    this.#temps = this.#temps.filter((t) => !gone.includes(t));
  }

  // ---------------------------------------------------------- applying

  /**
   * A part of the seed: the whole shape, or a part of it where the server
   * sent it in parts. A part without `part` begins the seed, letting go of
   * the parts of one before it; each with `more` is held as the text it
   * came as, and the last lands them all (`#landSeed`). So every read and
   * live query before the last answers the replica as it was, and a stream
   * that ends before it leaves that as it was: its parts go, and the stream,
   * opened again, seeds again from its start. The page holds them, not the
   * module: the module's memory is the replica's and never shrinks, and a
   * part written into it would go through `persist`'s journal only to be
   * deleted. The native core's rule (`Sync::seed`), which keeps them in
   * the replica's file.
   */
  async #seedPart(s, data, relayed = false) {
    const head = seedHead(data);
    if (head.part === 0) s.parts = null;
    else if (s.parts?.seq !== head.seq || s.parts.texts.length !== head.part) {
      s.parts = null;
      // Relayed, the leader's stream ended mid-seed: the tab waits for the
      // seed begun again, as the leader does.
      if (relayed) return false;
      throw new FennecError(`part ${head.part} of a seed at ${head.seq} came out of order`);
    }
    if (head.more) {
      (s.parts ??= { seq: head.seq, texts: [] }).texts.push(data);
      return true;
    }
    const texts = s.parts?.texts ?? [];
    s.parts = null;
    texts.push(data);
    return this.#landSeed(s, head.seq, texts);
  }

  /**
   * The seed landed: its parts, written over the shape's collection, and
   * what the replica holds of it and the seed does not deleted -- written
   * over rather than cleared first, so a row that stayed keeps its vector's
   * node -- except the rows of writes the server has not answered yet, and
   * of those it answered after the seed was taken, whose copies come later.
   * It lands a part at a time in this task (`#landParts`): JavaScript runs
   * one task at a time, so no read, write or live query of the page comes
   * between its first write and its last. Not in a block: a block holds
   * every write it makes until it lands, and the module's memory, which
   * never shrinks, grew 68.7 -> 168.9 MB for a seed of 300 000 rows so, and
   * 3.8 -> 5.0 MB for one of 5 000 (docs/design/js-client.md#seeds-in-parts).
   * Nothing is awaited from the first write to the last, so a collated
   * field's data, which a statement half way may be refused for, is fetched
   * first for a seed of several parts; one of one part lands in a block,
   * put back whole where it is refused, the chunks it named fetched and the
   * seed landed again -- only those, as a write fetches them.
   */
  async #landSeed(s, seq, texts) {
    const whole = s.collated && texts.length === 1;
    if (s.collated && !whole) await this.#local.collation('all');
    for (;;) {
      // What a block changes in memory -- the temporary rows, the undo of
      // each write waiting (`#heard`) -- kept to put back with it: the
      // queue itself is not read again, which holds each write's promise.
      const temps = this.#temps;
      const undos = whole ? this.#queue.map((op) => [op, op.undo, op.touched && new Map([...op.touched].map(([c, ids]) => [c, new Set(ids)]))]) : [];
      const back = () => {
        this.#temps = temps;
        for (const [op, undo, touched] of undos) Object.assign(op, { undo, touched });
      };
      let landed;
      if (whole) block(this.#local, 0);
      try {
        landed = this.#landParts(s, seq, texts, whole);
        if (whole) block(this.#local, landed ? 1 : 2);
      } catch (e) {
        if (whole) {
          block(this.#local, 2);
          back();
        }
        // Without a block, a statement half way refused for collation data
        // left the shape emptied (`#landParts`), never half seeded: a module
        // built without the schema check cannot say which fields are
        // collated (`start`). The data fetched, the seed lands again.
        if (!e.collation || !(await this.#local.collation(...e.collation))) throw e;
        continue;
      }
      if (!landed) {
        back();
        return this.#refresh(s);
      }
      break;
    }
    s.cursor = seq;
    s.seeded = true;
    s.fresh = false;
    this.#afterApply(s, true);
    return true;
  }

  /**
   * A seed landed, a part at a time, each parsed as it lands and its rows
   * written in chunks (`PUT_ROWS`). Every part is the rows of one statement
   * of the server's, its columns the same, so the first holding rows is the
   * one that may name a field the replica has not got: false then, nothing
   * written. A replica in a file (`openFile`) holds each statement as it
   * lands, so there the shape's row says a landing is under way (`LANDING`)
   * from the first write to the last, which writes its cursor: a page that
   * ended between them finds it as it opens again and empties the shape
   * before anything reads it (`start`), the rows of writes not yet answered
   * kept, to be seeded again. Elsewhere the writes reach storage after the
   * task, `persist`'s drain one chunk, and the mark would cost a seed of
   * 5 000 rows a write the module had not compiled yet, 1.2 ms of 28. A
   * statement refused half way empties the shape at once, but `inBlock`,
   * which `#landSeed` puts back.
   */
  #landParts(s, seq, texts, inBlock) {
    const c = s.collection;
    const held = (this.#local.run(`get ${c} select id`).rows ?? []).map((r) => r.id);
    const came = new Set();
    const mark = !inBlock && kept.has(this.#local);
    let begun = false;
    try {
      for (const text of texts) {
        const rows = JSON.parse(text).rows ?? [];
        if (rows.length === 0) continue;
        if (!this.#fits(s, rows)) {
          if (!begun) return false;
          throw new FennecError(`a part of the seed of \`${c}\` names a field the parts before it did not`);
        }
        if (!begun && mark) this.#local.run(PUT_SHAPE, [s.row, c, s.fingerprint, LANDING, false]);
        begun = true;
        for (const r of rows) came.add(r.id);
        this.#reconcile(s, rows);
        this.#writeRows(s, rows);
      }
      const keep = new Set(this.#temps.filter((t) => t.collection === c && (t.op !== 0 || t.until > seq)).map((t) => t.temp));
      this.#deleteLocal(c, held.filter((id) => !came.has(id) && !keep.has(id)));
      const dropped = this.#temps.filter((t) => t.collection === c && t.op === 0 && t.until <= seq);
      this.#deleteLocal('_sync_temps', dropped.map((t) => t.temp));
      this.#temps = this.#temps.filter((t) => !dropped.includes(t));
      // Every id but the temporary rows kept: a row an unanswered write
      // deleted is not held, so a seed lacking it names it too.
      if (this.#queue.length) this.#heard(c, null, keep);
      this.#local.run(PUT_SHAPE, [s.row, c, s.fingerprint, seq, true]);
      return true;
    } catch (e) {
      if (begun && !inBlock) this.#emptyShape(s);
      throw e;
    }
  }

  /**
   * The replica's collections as `describe()` tells them, collations named;
   * none from a module built without the schema check, which has no
   * description (`make wasm SCHEMA=0`).
   */
  #described() {
    try {
      return this.#local.describe().collections ?? [];
    } catch {
      return [];
    }
  }

  /**
   * The shape's rows but those of writes not yet answered deleted, and the
   * shape no longer seeded: what a landing cut short leaves to read, rather
   * than half a seed.
   */
  #emptyShape(s) {
    this.#local.run(...from(s.collection).where('id', '<', TEMP_BASE).toDelete());
    Object.assign(s, { cursor: 0, seeded: false });
    this.#local.run(PUT_SHAPE, [s.row, s.collection, s.fingerprint, 0, false]);
  }

  /** A change: the rows that changed as they are now, and the ids that left the shape. */
  async #applyChange(s, msg) {
    const puts = msg.puts ?? [];
    if (msg.schema || !this.#fits(s, puts)) return this.#refresh(s);
    this.#reconcile(s, puts);
    this.#deleteLocal(s.collection, msg.dels ?? []);
    await this.#putRows(s, puts);
    if (this.#queue.length) this.#heard(s.collection, [...puts.map((r) => r.id), ...(msg.dels ?? [])]);
    const seq = msg.seq ?? s.cursor;
    this.#dropPassed(s.collection, seq);
    s.cursor = Math.max(seq, s.cursor);
    this.#afterApply(s);
    return true;
  }

  /**
   * The server sent ids of `c`, each as it is now or gone: a change's
   * `ids`, or, for a seed, every id but the temporary rows it `keep`s. What
   * a write it has not answered would put back of them stood before that:
   * refused, the write leaves them as the server sent them (its steps'
   * `kept`), else an insert refused for an id the server holds deleted the
   * server's row the stream had brought. The native core's rule
   * (`Sync::heard`). Each write's ids are read once (`touched`), so a
   * change reaching none of them rewrites nothing; it runs once the rows
   * are in, as the native core marks them in the change's own block.
   */
  #heard(c, ids, keep) {
    for (const op of this.#queue) {
      op.touched ??= touchedBy(op.undo);
      const mine = op.touched.get(c);
      if (!mine) continue;
      const hit = ids ? ids.filter((id) => mine.delete(id)) : [...mine].filter((id) => !keep.has(id));
      if (!ids) for (const id of hit) mine.delete(id);
      if (!hit.length) continue;
      const steps = JSON.parse(op.undo);
      for (const st of steps) if (st.c === c) st.kept = [...(st.kept ?? []), ...hit];
      op.undo = JSON.stringify(steps);
      this.#keep(op);
    }
  }

  /**
   * A refused write's rows are put back as the server holds them, but the
   * next waiting write over each of them read them as the refused one left
   * them, which the server never held: refused in turn, it would write that
   * back. So its undo gets a step of its own, run last, putting back each
   * such row as it is now, or deleting it where it is gone. Only the next
   * write over a row: the one after read that one's row, right if it lands.
   * The native core's rule (`Sync::rebase`).
   */
  #rebase(refused) {
    const left = touchedBy(refused.undo);
    for (const op of this.#queue) {
      op.touched ??= touchedBy(op.undo);
      const steps = [];
      for (const [c, ids] of left) {
        const mine = op.touched.get(c);
        if (!mine) continue;
        const both = [...ids].filter((id) => mine.has(id));
        if (!both.length) continue;
        for (const id of both) ids.delete(id);
        const rows = this.#local.run(...from(c).where('id', 'in', both).toFennecQL()).rows ?? [];
        const here = new Set(rows.map((r) => r.id));
        steps.push({ c, del: both.filter((id) => !here.has(id)), put: JSON.stringify(rows) });
      }
      if (!steps.length) continue;
      op.undo = JSON.stringify([...steps, ...JSON.parse(op.undo)]);
      this.#keep(op);
    }
  }

  /**
   * When a row from the server carries the key of a pending optimistic row,
   * the one with the temporary id is dropped. The counterpart of TanStack
   * DB's `txid`; here the handle is a **business key**, because the id space
   * belongs to the server and the client cannot know it in advance.
   */
  #reconcile(s, rows) {
    if (!s.key || rows.length === 0) return;
    const c = s.collection;
    if (!this.#temps.some((t) => t.collection === c)) return;
    const drop = [];
    for (const r of rows) {
      const k = r[s.key];
      if (k == null) continue;
      const t = this.#temps.find((x) => x.collection === c && x.key === keyText(k));
      if (t && t.temp !== r.id) drop.push(t);
    }
    if (!drop.length) return;
    const ids = drop.map((t) => t.temp);
    this.#deleteLocal(c, ids);
    this.#deleteLocal('_sync_temps', ids);
    this.#temps = this.#temps.filter((t) => !drop.includes(t));
  }

  /**
   * The server's rows of `s`'s collection written into the replica: `put`,
   * as the builder's `insert` sends, so a held id is written over -- a row
   * that stayed keeps its vector's node -- in chunks (`PUT_ROWS`). The rows
   * go as one parameter, which the module reads as a body's documents, each
   * value made its field's type as the builder's text has it made
   * (`Database::document_of`), so the same rows land: with no text built
   * and parsed again, a parameter a value, a seed of 100 000 rows of a
   * text, a float and a json field went from its event to `ready()` in
   * 192 ms against the text's 295, of points in 163 against 357. A
   * collection with a vector or bytes field keeps the builder's text,
   * which hands their lists apart as `f32`s (`APART`).
   */
  async #putRows(s, rows) {
    const c = ident(s.collection, 'collection');
    for (let i = 0; i < rows.length; i += PUT_ROWS) {
      const chunk = rows.slice(i, i + PUT_ROWS);
      if (s.apart) await from(c).bind(this.#localExec).insert(chunk);
      else await this.#local.query(`put ${c} $1`, [chunk]);
    }
  }

  /**
   * `#putRows` in this task, for a block: a statement refused for collation
   * data throws, for the block's owner to fetch it and run the block again.
   */
  #writeRows(s, rows) {
    const c = ident(s.collection, 'collection');
    for (let i = 0; i < rows.length; i += PUT_ROWS) {
      const chunk = rows.slice(i, i + PUT_ROWS);
      if (s.apart) this.#local.run(...from(c).toInsert(chunk));
      else this.#local.run(`put ${c} $1`, [chunk]);
    }
  }

  /** The cursor kept beside the rows it stands for: in their block where `saved`. */
  #afterApply(s, saved = false) {
    if (!saved) this.#local.run(PUT_SHAPE, [s.row, s.collection, s.fingerprint, s.cursor, s.seeded]);
    s.error = null;
    this.#touch();
    this.#persistSoon();
    if (this.#allSeeded) this.#resolveReady();
  }

  get #allSeeded() {
    return [...this.#shapes.values()].every((s) => s.seeded);
  }

  // -------------------------------------------------------------- live

  /**
   * Schedules the next tick; changes arriving back to back collapse into
   * a single run.
   *
   * Frame alignment is **not enough on its own**: `requestAnimationFrame`
   * never runs in a hidden tab. Tied to it alone, a backgrounded tab would
   * keep receiving data while its live queries silently stopped -- the
   * worst kind of silently wrong state. So the two **race**: in a visible
   * tab the frame arrives first and cancels the timer, in a hidden tab the
   * timer takes over.
   */
  #touch() {
    if (this.#scheduled) return;
    const raf = globalThis.requestAnimationFrame;
    const fire = () => {
      if (!this.#scheduled) return;
      clearTimeout(this.#scheduled.timer);
      if (this.#scheduled.frame !== undefined) {
        globalThis.cancelAnimationFrame?.(this.#scheduled.frame);
      }
      this.#scheduled = null;
      this.#pendingTick = this.#lives.tick().finally(() => {
        this.#pendingTick = null;
      });
      // Anyone waiting in `flush()`: the tick has *started*, so from here
      // on it can be awaited through `#pendingTick`.
      for (const f of this.#flushers.splice(0)) f();
    };
    this.#scheduled = {
      timer: setTimeout(fire, TICK_FALLBACK_MS),
      frame: raf ? raf.call(globalThis, fire) : undefined,
    };
    this.#scheduled.timer.unref?.();
  }

  // ------------------------------------------------------------ stream

  async start() {
    // From here its raw FennecQL (`local`) refuses a named statement in
    // the sync's words, as a native binding's `run` on a synced database
    // does; `close` gives the database back.
    replicas.add(this.#local);
    try {
      // A tab with no election takes its key's lock before it reads the
      // store, as it stands, and claims the store; one an election may make
      // leader does so then. With no lock manager each tab claims it, the
      // last one storing there.
      if (this.#stores()) {
        if (!this.#locks) this.#holding = true;
        else if (this.#single()) await this.#tryStore();
        if (this.#holding) await this.#claimFirst();
      }
      await this.#loadPersist();
    } catch (e) {
      this.close();
      throw e;
    }
    for (const text of SYNC_STATE) this.#local.run(text);
    this.#load();
    // A seed of several parts the page ended in the middle of landing: what
    // it wrote of the shape goes before anything reads it -- reads answer
    // none rather than half a seed -- the rows of unanswered writes kept,
    // and the shape is seeded again whole.
    for (const s of this.#shapes.values()) {
      if (!s.cut) continue;
      s.cut = false;
      this.#emptyShape(s);
    }
    const schemas = this.#local.schemas();
    // `schemas` names no collation; the description does.
    const described = schemas.length ? this.#described() : [];
    for (const s of this.#shapes.values()) {
      const schema = schemas.find((x) => x.name === s.collection);
      s.made = !!schema;
      s.fields = new Set(schema?.fields.map((f) => f.name));
      s.apart = apartIn(schema);
      s.collated = collatedIn(described.find((x) => x.name === s.collection));
    }
    this.#flushing = this.#queue.length > 0;
    if (this.#allSeeded) this.#resolveReady();
    // The replica as it was kept: a live query asked before it ran on the
    // empty one.
    this.#lives.stale();
    this.#touch();
    const on = globalThis.addEventListener;
    if (on) {
      this.#listeners = [['online', () => this.setOnline(true)], ['offline', () => this.setOnline(false)]];
      for (const [k, f] of this.#listeners) on.call(globalThis, k, f);
    }
    this.#elect();
    return this;
  }

  /** What the replica kept: each shape's cursor, the queue and the temporary rows. */
  #load() {
    let top = 0;
    for (const r of this.#local.run('get _sync_shapes').rows ?? []) {
      top = Math.max(top, r.id);
      const s = this.#shapes.get(r.collection);
      if (!s) continue;
      s.row = r.id;
      // Another server, filter or projection holds other rows: seeded again.
      if (r.shape === s.fingerprint) {
        s.cursor = r.cursor;
        s.seeded = r.seeded === true;
      }
      if (r.cursor === LANDING) s.cut = true;
    }
    for (const s of this.#shapes.values()) if (!s.row) s.row = ++top;
    this.#queue = (this.#local.run('get _sync_queue').rows ?? []).sort((a, b) => a.id - b.id).map((r) => ({
      n: r.id,
      path: r.path,
      body: r.body,
      key: r.key,
      undo: r.undo,
      label: r.label,
      count: 0,
      done: null,
    }));
    this.#nextN = (this.#queue.at(-1)?.n ?? 0) + 1;
    this.#temps = (this.#local.run('get _sync_temps').rows ?? []).map((r) => ({
      collection: r.collection,
      key: r.key,
      temp: r.id,
      op: r.op,
      until: r.until,
    }));
    this.#nextTemp = Math.max(TEMP_BASE, ...this.#temps.map((t) => t.temp + 1));
  }

  /**
   * Multiple tabs: each tab would have its own WASM instance and its own
   * subscription -- N copies, N connections. `navigator.locks` picks a
   * **single leader**; the others take the same batches over
   * `BroadcastChannel` and apply them to their own local copy.
   *
   * Only the leader sends writes. Every tab applies its own at once and
   * keeps it in its queue, as the leader does; a follower hands it to the
   * leader, which keeps it in its own queue -- persisted with its replica --
   * sends it under its key, and tells every tab the answer, which the tab
   * that made the write applies to its copy. A new leader is handed every
   * follower's queue again, and the queue its predecessor persisted; the
   * key makes a write sent twice land once.
   */
  #elect() {
    const name = `fennecdb:${this.#url}:${[...this.#shapes.keys()].join(',')}`;
    if (this.#single()) {
      // A single tab (or Node): no leader election needed. One that found
      // its key's lock held takes the store over once it is let go of.
      this.#kick();
      if (this.#stores() && !this.#holding) {
        this.#awaitStore()
          .then(() => this.#unkept || this.#persistSoon())
          .catch((e) => this.#onError?.(e));
      }
      return;
    }
    this.#leader = false;
    this.#chan = new BroadcastChannel(name);
    this.#chan.onmessage = (e) => {
      this.#onRelay(e.data).catch((err) => this.#onError?.(err));
    };
    // Ask an existing leader for a seed, so we can work without waiting
    // for our turn at the lock. **Repeated**, because the leader may be
    // downloading its own seed at that moment; it ignores the request then,
    // and a one-shot hello would go unanswered forever.
    this.#hello();
    this.#kick();
    this.#locks
      .request(name, { mode: 'exclusive' }, async () => {
        if (this.#closed) return;
        this.#elected = true;
        this.#theirs = null;
        // The store before the lead: the queue the tab before kept is taken
        // over first, its writes sent before this tab's, and one tab writes
        // the key at a time, whatever its shapes or url.
        if (this.#stores()) {
          const held = await this.#tryStore();
          if (this.#closed) return;
          if (held) await this.#takeStore();
          else await this.#awaitStore();
          if (this.#closed) return;
        }
        this.#lead();
        // The lock stays with us until the tab closes.
        return new Promise((r) => {
          this.#release = r;
        });
      })
      .catch((e) => this.#onError?.(e));
  }

  /** This tab leads now, the store taken over (`#takeStore`): the others are asked for their queues. */
  #lead() {
    this.#leader = true;
    this.#flushing = this.#queue.length > 0;
    this.#chan.postMessage({ t: 'leader' });
    this.#kick();
  }

  /** Whether the replica is kept in IndexedDB (`persist`), not in a file. */
  #stores() {
    return !!this.#persistKey && !!globalThis.indexedDB && !kept.has(this.#local);
  }

  /** No election: `leader: false`, or no `BroadcastChannel` or lock manager to hold one with. */
  #single() {
    return this.#leaderMode === false || !globalThis.BroadcastChannel || !this.#locks;
  }

  /** The key's lock, held till `#letGo`: as it stands or (`wait`) once free; false where held or closed. */
  #lockStore(wait) {
    return new Promise((got, failed) => {
      // A wait the close ends, holding no place in the lock's queue.
      const opts = wait ? { mode: 'exclusive', signal: this.#abort.signal } : { mode: 'exclusive', ifAvailable: true };
      const asked = this.#locks.request(storeLock(this.#persistKey), opts, (lock) => {
        if (lock === null || this.#closed) return got(false);
        this.#holding = true;
        got(true);
        return new Promise((r) => {
          this.#releaseStore = r;
        });
      });
      // Settled once the lock is free again: what `letting` waits on.
      this.#storeAsk = asked;
      asked.catch((e) => (this.#closed ? got(false) : failed(e)));
    });
  }

  /** The key's lock as it stands, once a replica of this page that closed has let go of it. */
  async #tryStore() {
    const free = letting.get(this.#persistKey);
    if (free) {
      this.#unkeptBy(this.#elsewhere());
      await free;
    }
    const held = await this.#lockStore(false);
    if (held) this.#unkeptBy(null);
    return held;
  }

  /** Waits for the key's lock held elsewhere, `unkept` saying so, then takes the store over. */
  async #awaitStore() {
    this.#unkeptBy(this.#elsewhere());
    if (await this.#lockStore(true)) await this.#takeStore();
  }

  /** Why this tab stores nothing, or null; told to the tabs it leads or will (`#theirs`). */
  #unkeptBy(e) {
    this.#unkept = e;
    // A closed tab's channel is closed: posted to, it throws.
    if (this.#elected && !this.#closed) this.#chan?.postMessage({ t: 'unkept', m: e?.message ?? null });
  }

  /** The store claimed before it is read (`claim`); refused, this tab stores nothing there, and tries again at its next persist. */
  async #claimFirst() {
    try {
      this.#owner = await claim(this.#persistKey);
    } catch (e) {
      this.#unkeptAgain = true;
      this.#unkeptBy(new FennecError(`${this.#persistKey} is kept as it is, and this tab stores nothing there: ${e.message}`, { cause: e }));
      this.#onError?.(this.#unkept);
    }
  }

  #elsewhere() {
    return new FennecError(
      `${this.#persistKey} is stored by another replica, in another tab or this one, and this one stores nothing there until it lets go`,
    );
  }

  /**
   * This tab writes the store now, the queue kept there taken over. A store
   * another tab wrote since this one read it gets an image first: a chunk
   * went on from where this tab read it. One that does not restore is kept,
   * this tab storing nothing there (`#unkept`), read again at the next
   * persist where IndexedDB failed the read (`#retake`).
   */
  async #takeStore() {
    const other = sibling(this.#local);
    try {
      // Claimed first: a write of the replica before lands before the read, or not at all.
      this.#owner = await claim(this.#persistKey);
      if (await restore(other, this.#persistKey, { cryptoKey: this.#cryptoKey })) {
        for (const r of other.run('get _sync_queue').rows ?? []) this.#adopt(r);
      }
      if (!sameStore(stored.get(other), stored.get(this.#local))) stored.delete(this.#local);
      this.#unkeptBy(null);
    } catch (e) {
      this.#unkeptAgain = unread.has(e);
      this.#unkeptBy(new FennecError(`${this.#persistKey} is kept as it is, and this tab stores nothing there: ${e.message}`, { cause: e }));
      this.#onError?.(this.#unkept);
    } finally {
      other.close();
    }
  }

  /** Another tab's write, kept and sent here: nothing of it to put back in this replica. */
  #adopt(o) {
    if (this.#queue.some((q) => q.key === o.key)) return;
    const op = { n: this.#nextN++, path: o.path, body: o.body, key: o.key, undo: '[]', label: o.label, count: 0, done: null, foreign: true };
    this.#keep(op);
    this.#queue.push(op);
    this.#persistSoon();
  }

  /** Repeats the hello until the leader is seeded. */
  #hello() {
    if (this.#closed || this.#leader || this.#allSeeded) return;
    this.#chan?.postMessage({ t: 'hello' });
    const t = setTimeout(() => this.#hello(), 400);
    t.unref?.();
  }

  /** Opens `s`'s stream, unless one is open or waits to be. */
  #connect(s) {
    if (s.stream || s.waiting || this.#paused || this.#offline || this.#closed) return;
    const ctl = new AbortController();
    s.stream = ctl;
    this.#stream(s, ctl).catch((e) => this.#onError?.(e));
  }

  /** Ends `s`'s stream from this side, and the parts of a seed it was sending: opened again, it seeds again. */
  #cancel(s) {
    s.stream?.abort();
    s.stream = null;
    s.connected = false;
    s.parts = null;
  }

  async #stream(s, ctl) {
    let reason = 'the server closed it';
    // A refusal to open: its status, and the tenant's quota and the wait it
    // names -- a 429 of the `streams` quota is waited out by its
    // `retry_after`, since asked again sooner it is only refused again.
    let refusal = { status: 0, quota: null, retryMs: 0 };
    const carried = this.#carried();
    try {
      const res = await this.#fetch(this.#streamUrl(s), { headers: this.#headers(), signal: ctl.signal });
      if (s.stream !== ctl) return;
      this.#answered(res.status, carried);
      if (res.status === 401) {
        // Read for its code -- a token past its `exp` is refused at the open
        // too -- before the stream is cancelled: the abort throws the body
        // away, and a fetch read after it is told nothing.
        const text = await res.text().catch(() => '');
        if (s.stream !== ctl) return;
        this.#cancel(s);
        // Opened with a token since replaced, it opens again with that one.
        return this.#tokenRefused(said(text).code, carried) || this.#kick();
      }
      if (!res.ok || !res.body) {
        const text = await res.text().catch(() => '');
        refusal = { status: res.status, ...said(text) };
        const e = new FennecError(`could not open the subscription to \`${s.collection}\`: ${why(res.status, text)}`);
        throw Object.assign(e, { status: refusal.status }, refusal.quota && { quota: refusal.quota });
      }
      s.connected = true;
      s.attempt = 0;
      this.#clearConnectionFault();
      for await (const ev of sseEvents(res)) {
        if (s.stream !== ctl) return;
        // The server ends a stream at its token's `exp` with a 401 and
        // `token_expired`: a fresh token is wanted before it opens again,
        // as for a request. One opened with a token since replaced opens
        // again with that one, the provider asked nothing.
        if (ev.name === 'error' && /"status":\s*401\b/.test(ev.data)) {
          this.#cancel(s);
          return this.#tokenRefused(said(ev.data).code, carried) || this.#kick();
        }
        await this.#onEvent(s, ev);
      }
    } catch (e) {
      if (s.stream !== ctl) return;
      reason = String(e?.message ?? e);
      s.error = e;
      this.#onError?.(e);
    }
    if (s.stream !== ctl) return;
    this.#cancel(s);
    this.#setFault(`the subscription to \`${s.collection}\` ended: ${reason}`, refusal.status, true, refusal.quota);
    // Waiting for a token, nothing is sent: the token opens it again, as it
    // does the stream a 401 ended, and a backoff begun now would hold it
    // back past the token (the native core's `retry_stream`).
    if (this.#paused) return;
    s.waiting = true;
    this.#wait(s, s.attempt++, refusal.retryMs, () => {
      s.waiting = false;
      this.#kick();
    });
  }

  async #onEvent(s, ev) {
    if (ev.name === 'seed') {
      if (await this.#seedPart(s, ev.data)) this.#relay({ t: 'seed', c: s.collection, data: ev.data });
      return;
    }
    const msg = ev.data ? JSON.parse(ev.data) : {};
    if (ev.name === 'change' && s.seeded) {
      if (await this.#applyChange(s, msg)) this.#relay({ t: 'change', c: s.collection, ...msg });
    } else if (ev.name === 'error') {
      throw new FennecError(msg.error ?? 'subscription error');
    }
  }

  #streamUrl(s) {
    const p = new URLSearchParams();
    for (const [k, v] of s.params) p.append(k, v);
    if (s.select) p.set('select', s.select.join(','));
    // Seeded once, it goes on from where it stopped: the server seeds it
    // again itself when the cursor is past its ring.
    if (s.seeded && !s.fresh) p.set('since', String(s.cursor));
    const qs = p.toString();
    return `${this.#url}/${encodeURIComponent(s.collection)}/changes${qs ? `?${qs}` : ''}`;
  }

  #headers() {
    const h = { accept: 'text/event-stream' };
    if (this.#token) h.authorization = `Bearer ${this.#token}`;
    return h;
  }

  // -------------------------------------------------------------- tabs

  #relay(msg) {
    if (this.#chan && this.#leader) this.#chan.postMessage(msg);
  }

  async #onRelay(msg) {
    // A tab that joined, or writes, learns why the one elected stores nothing, waiting to lead or leading.
    if ((msg.t === 'hello' || msg.t === 'op') && this.#elected && this.#unkept) this.#unkeptBy(this.#unkept);
    if (msg.t === 'unkept') {
      this.#theirs = msg.m && `the tab elected to lead stores nothing: ${msg.m}`;
      return;
    }
    if (this.#leader) {
      if (msg.t === 'op') {
        this.#adopt(msg.op);
        this.#kick();
      } else if (msg.t === 'hello') {
        // Hand the new tab what we have, its own rows not among it: a
        // temporary id is this replica's alone.
        // In parts as a stream's seed comes, read in this task so they are
        // one moment; then the parts of a seed it is being sent, which the
        // new tab takes the rest of as they come.
        for (const s of this.#shapes.values()) {
          if (!s.seeded) continue;
          const get = `get ${s.collection} where id > $1 and id < $2 limit $3`;
          for (let part = 0, after = -1; ; part++) {
            const rows = this.#local.run(get, [after, TEMP_BASE, HELLO_ROWS]).rows ?? [];
            const more = rows.length === HELLO_ROWS;
            const data = JSON.stringify({ seq: s.cursor, ...(part && { part }), rows, ...(more && { more }) });
            this.#chan.postMessage({ t: 'seed', c: s.collection, data });
            if (!more) break;
            after = rows.at(-1).id;
          }
          for (const data of s.parts?.texts ?? []) this.#chan.postMessage({ t: 'seed', c: s.collection, data });
        }
        this.#chan.postMessage({ t: 'leader' });
      }
      return;
    }
    if (msg.t === 'leader') {
      for (const op of this.#queue) this.#chan.postMessage({ t: 'op', op: wire(op) });
      return;
    }
    if (msg.t === 'ans') {
      const op = this.#queue.find((o) => o.key === msg.key);
      if (!op) return;
      if (msg.status < 300) this.#landed(op, msg.seq);
      else this.#refused(op, msg.status, msg.message, msg.quota, msg.reason);
      return;
    }
    const s = this.#shapes.get(msg.c);
    if (!s) return;
    if (msg.t === 'schema') await this.#remake(s, msg.s);
    else if (msg.t === 'seed') await this.#seedPart(s, msg.data, true);
    // An incremental diff cannot be applied to an unseeded copy: we would
    // be left with the changed rows only, the rest missing.
    else if (msg.t === 'change' && s.seeded) await this.#applyChange(s, msg);
  }

  // ------------------------------------------------------- persistence

  /**
   * Stores the replica now -- its writes since the last time, a chunk --
   * with the sync's own collections in it: a write is kept the moment it
   * is made. One store at a time, the next gathering what came meanwhile.
   * The leader's alone: the other tabs' copies are fed by it.
   */
  #persistSoon() {
    if (!this.#stores() || !this.#leader || !this.#holding || this.#closed) return;
    if (this.#unkept) {
      this.#retake();
      return;
    }
    if (!this.#owner) return;
    if (this.#persisting) {
      this.#persistAgain = true;
      return;
    }
    const local = this.#local;
    const key = this.#persistKey;
    const owner = this.#owner;
    const opts = { cryptoKey: this.#cryptoKey };
    // Set before the loop runs: one ending at once (`inTurn` throws before
    // it awaits where a restore failed) clears it after, never before.
    let ended;
    this.#persisting = new Promise((r) => (ended = r));
    (async () => {
      let failed = null;
      try {
        // A write made while one is out, the close's among them, is stored
        // after it: closed, until the key's lock is let go of (`#letGo`).
        do {
          this.#persistAgain = false;
          // `persist`, its write refused where another replica claimed the store.
          await inTurn(local, () => persistNow(local, key, opts, owner), 'persist');
        } while (this.#persistAgain && !this.#fenced);
        if (this.#persistAgain) {
          failed = new FennecError(
            `${key}: what was written since the last persist is not stored: ` +
              `the replica was closed, and let go of the store after ${LET_GO_MS / 1000} s waiting for it`,
          );
        }
      } catch (e) {
        failed = e;
      }
      // Over in the step that last looked for a write waiting: one made
      // after finds none out and starts the next. Cleared a few microtasks
      // on, in a `finally`, a write between was left to a loop that had
      // ended, and a close let go of the store without it, telling nothing
      // (the third review).
      this.#persisting = null;
      ended();
      if (!failed) return;
      // Claimed by another since, or deleted: this tab stores nothing there.
      if (taken.has(failed) && this.#owner === owner) {
        this.#owner = null;
        this.#unkeptAgain = false;
        this.#unkeptBy(failed);
      }
      this.#onError?.(failed);
    })();
  }

  /**
   * Lets go of the key's lock once the persist out has settled -- it stores
   * what was written before the close -- as one landing after the next tab
   * read the store replaced a record that tab's chunks were sealed after,
   * and it never restored again. Past `LET_GO_MS` it is let go of all the
   * same: that persist's write stores nothing once the next replica claimed
   * the store, and none is started after it.
   */
  #letGo() {
    const release = this.#releaseStore;
    if (!release) return;
    this.#releaseStore = null;
    const key = this.#persistKey;
    const free = this.#storeAsk.then(
      () => {},
      () => {},
    );
    letting.set(key, free);
    free.then(() => letting.get(key) === free && letting.delete(key));
    let t = null;
    const go = () => {
      if (this.#fenced) return;
      clearTimeout(t);
      this.#fenced = true;
      this.#holding = false;
      release();
    };
    const out = this.#persisting;
    if (!out) return go();
    t = setTimeout(go, LET_GO_MS);
    t.unref?.();
    out.then(go);
  }

  /** The store read again at a persist, where IndexedDB failed the take-over's read. */
  #retake() {
    if (!this.#unkeptAgain || this.#taking) return;
    this.#taking = this.#takeStore().then(() => {
      this.#taking = null;
      if (!this.#unkept) this.#persistSoon();
    });
  }

  /**
   * Restores the previous session's image: the replica, its cursors and the
   * writes it had not sent, which go first. Only a key holding nothing is
   * seeded: a store there that does not restore -- another key or none, a
   * later client's, refused by the module -- stops the start, kept as it
   * stands with its unsent writes, which a seed's first `persist` stored
   * over (js-client.md#sealed-chunks-in-a-chain).
   */
  async #loadPersist() {
    if (!this.#persistKey || !globalThis.indexedDB || kept.has(this.#local)) return;
    try {
      await restore(this.#local, this.#persistKey, { cryptoKey: this.#cryptoKey });
    } catch (e) {
      throw new FennecError(
        `the replica stored under ${this.#persistKey} does not restore (${e.message}): it is kept as it stands, ` +
          'with the writes it has not sent; start again with the cryptoKey it was sealed with, or delete it ' +
          `with unpersist(${JSON.stringify(this.#persistKey)}), those writes with it, to seed anew`,
        { cause: e },
      );
    }
  }
}

/** Whether two databases' `stored` read one store as it stood (`#takeStore`). */
function sameStore(a, b) {
  if (!a || !b) return false;
  if (a.key !== b.key || a.ck !== b.ck || a.gen !== b.gen || a.next !== b.next || a.image !== b.image || a.chunks !== b.chunks) return false;
  if (!a.tag || !b.tag) return a.tag === b.tag;
  return a.tag.length === b.tag.length && a.tag.every((x, i) => x === b.tag[i]);
}

/** A refusal's words: the server's `{"error": ..}`, or what the transport said. */
function why(status, body) {
  try {
    const e = JSON.parse(body)?.error;
    if (e) return e;
  } catch {
    /* not JSON */
  }
  if (status === 0) return body || 'the server could not be reached';
  return `HTTP ${status}: ${String(body).slice(0, 200)}`;
}

/** What another tab needs of a write to send it. */
/** The ids each collection's steps of an undo would delete or write back, by collection. */
function touchedBy(undo) {
  const by = new Map();
  for (const s of JSON.parse(undo)) {
    const kept = new Set(s.kept);
    let ids = by.get(s.c);
    if (!ids) by.set(s.c, (ids = new Set()));
    for (const id of s.del) if (!kept.has(id)) ids.add(id);
    for (const r of JSON.parse(s.put)) if (!kept.has(r.id)) ids.add(r.id);
  }
  return by;
}

function wire(op) {
  return { path: op.path, body: op.body, key: op.key, label: op.label };
}

/** A batch line: each line is exactly a `POST /query` body. */
function ndjson([sql, params], wide) {
  return jsonOf({ query: sql, params }, wide);
}

function normalizeShape(raw, url) {
  const spec = raw instanceof Query ? { collection: raw.collection } : raw;
  // A table declared in code (`fennecTable`) names its collection too, as
  // the docs write a shape: `{ collection: todos, key: 'key' }`.
  if (!isSpec(spec) || typeof nameOf(spec.collection) !== 'string') {
    throw new FennecError('a shape must be `{ collection, where?, select?, key? }`');
  }
  const collection = ident(nameOf(spec.collection), 'collection');
  if (collection.startsWith('_sync_')) throw new FennecError(`\`${collection}\` is the sync's own collection`);
  const select = spec.select ? [...new Set(['id', ...spec.select.map((c) => ident(c))])] : null;
  const params = shapeParams(spec.where);
  return {
    collection,
    key: spec.key ? ident(spec.key) : null,
    select,
    params,
    // The server, the filter and the projection, as the native core writes
    // them: a replica opened with another holds rows it should not.
    fingerprint: `${url}|${params.map(([k, v]) => `${k}=${v}&`).join('')}${select ? `|${select.join(',')}` : ''}`,
    row: 0,
    cursor: 0,
    seeded: false,
    connected: false,
    error: null,
    stream: null,
    waiting: false,
    attempt: 0,
    made: false,
    fields: new Set(),
    // Whether its rows go by the builder's text (`#putRows`).
    apart: false,
    // Whether a field compares by a collation, whose data the module may
    // lack (`#landSeed`).
    collated: false,
    rebuild: false,
    fresh: false,
    // A seed in parts under way: its change and its parts' texts (`#seedPart`).
    parts: null,
    // A landing of parts the page ended half way, found as it opened (`start`).
    cut: false,
  };
}

/**
 * Opens the local replica and starts the subscriptions.
 *
 * - `url`      server root (`fennec-server --http`)
 * - `shapes`   `[{ collection, where?, select?, key? }]`
 * - `local`    an existing `Fennec`; otherwise opened from `wasm`, its
 *              collation data from `collation` (`Fennec.open`). One kept
 *              in a file (`openFile`) keeps the replica, its cursors and
 *              its unsent writes there.
 * - `wasm`     the module the replica is opened with: a URL, its bytes or
 *              a compiled module, `./fennec.wasm` unless given.
 * - `token`    `Authorization: Bearer`; `tokenProvider` an async function
 *              asked for a new one when the server answers 401
 * - `persist`  IndexedDB key: the replica, its cursors and its unsent writes,
 *              one account's -- the key names it, and a replica is never
 *              opened for another. A store there that does not restore --
 *              another `cryptoKey`, or none -- refuses the sync, kept as it
 *              stands with its unsent writes; only none stored is seeded
 *              (`unpersist` deletes one). One replica stores a key at a
 *              time, holding its lock; another stores nothing there till
 *              it lets go (`status()`'s `unkept`)
 * - `cryptoKey` an AES-GCM `CryptoKey` the image and its chunks are sealed
 *              with (`persist`)
 * - `onRefused` told of each write the server refused, which was put back
 * - `leader`   `false` turns off multi-tab leader election: each tab sends
 *              its own writes, and one stores `persist`'s key at a time
 * - `locks`    lock manager (defaults to `navigator.locks`; `ifAvailable`
 *              among its options)
 */
export async function sync(opts = {}) {
  if (!opts.url) throw new FennecError('sync(): `url` is required');
  const local = opts.local ?? (await Fennec.open(opts.wasm ?? './fennec.wasm', { collation: opts.collation }));
  try {
    const replica = new FennecSync(local, opts);
    // The server owns the schema: the code's is compared with it, never applied.
    await checked(replica.remote, opts, 'follow');
    if (opts.schema) declared.set(replica, opts);
    return await replica.start();
  } catch (e) {
    // Opened here, the database is no one's once the sync is refused.
    if (!opts.local) local.close();
    throw e;
  }
}

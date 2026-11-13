// integrations/sync-scenarios.json run against FennecSync: every scenario's
// server events fed to it in order, through a transport the script plays --
// a fetch whose streams it writes to and whose requests it answers, a clock
// whose timers it runs out -- and what the replica holds, what was sent and
// what was told checked after every step. crates/fennec-abi/tests/scenarios.rs
// runs the same file against the native core, so the two are held to one
// behaviour; where they differ on purpose the file says so, and each side
// checks its own expectation.
//
// The replica is persisted (`persist`, over an in-memory IndexedDB), so a
// restart is the page opened again over what it stored. The names that
// passed are written to target/sync-scenarios/js.txt, which
// integrations/sync-scenarios-check.mjs holds to the file beside the core's.
// Skipped without web/fennec.wasm (make wasm), except under CI, where a skip
// would be a silent one.

import { test, after } from 'node:test';
import assert from 'node:assert/strict';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { Fennec, sync, inc, expr } from './fennec.js';
import { fakeIndexedDB, KeyRange } from './idb.fake.js';

const wasm = await readFile(new URL('./fennec.wasm', import.meta.url)).catch(() => null);
const file = JSON.parse(await readFile(new URL('../integrations/sync-scenarios.json', import.meta.url), 'utf8'));
const URL_ = 'http://server';

const skip = wasm ? false : process.env.CI ? false : 'no web/fennec.wasm (make wasm)';

// --------------------------------------------------------------- matching

/**
 * Expected against actual: an object's members a subset, a list whole, a
 * string `<name>` any value, the same each time the name comes again and
 * another than any other name's.
 */
function matches(exp, act, binds, at) {
  if (typeof exp === 'string' && /^<.+>$/.test(exp)) {
    const got = JSON.stringify(act ?? null);
    if (binds.has(exp)) {
      if (binds.get(exp) !== got) throw new Error(`${at}: ${exp} was ${binds.get(exp)}, now ${got}`);
      return;
    }
    for (const [k, v] of binds) if (v === got) throw new Error(`${at}: ${exp} is ${got}, which ${k} already is`);
    binds.set(exp, got);
    return;
  }
  if (Array.isArray(exp)) {
    if (!Array.isArray(act) || act.length !== exp.length) {
      throw new Error(`${at}: ${exp.length} items expected, ${JSON.stringify(act)} came`);
    }
    exp.forEach((x, i) => matches(x, act[i], binds, `${at}[${i}]`));
    return;
  }
  if (exp !== null && typeof exp === 'object') {
    if (act === null || typeof act !== 'object') throw new Error(`${at}: expected an object, got ${JSON.stringify(act)}`);
    for (const [k, v] of Object.entries(exp)) matches(v, act[k] ?? null, binds, `${at}.${k}`);
    return;
  }
  if (exp !== (act ?? null)) throw new Error(`${at}: expected ${JSON.stringify(exp)}, got ${JSON.stringify(act)}`);
}

/** A step's data with each fixture (`@name`) and each bound `<name>` put in. */
function resolve(v, binds) {
  if (typeof v === 'string' && v.startsWith('@')) return structuredClone(file.fixtures[v.slice(1)]);
  if (typeof v === 'string' && /^<.+>$/.test(v)) {
    if (!binds.has(v)) throw new Error(`${v} is used before it is bound`);
    return JSON.parse(binds.get(v));
  }
  if (Array.isArray(v)) return v.map((x) => resolve(x, binds));
  if (v !== null && typeof v === 'object') {
    return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, k.startsWith('expect') ? x : resolve(x, binds)]));
  }
  return v;
}

/** The fields of a write in alphabetical order, as the native runner's JSON reads them. */
function sorted(o, at) {
  const keys = Object.keys(o);
  assert.deepEqual(keys, [...keys].sort(), `${at}: fields in alphabetical order, or the platforms render them apart`);
}

/**
 * An answer as a real fetch gives it: its body thrown away once `signal`
 * aborts, a read begun after that rejecting. A stand-in whose body outlived
 * the abort hid a stream refused at its open whose body was read only after
 * the stream was cancelled -- told nothing, on Node's own fetch.
 */
function answered(text, status, signal, headers = {}) {
  const body = new ReadableStream({
    start(ctl) {
      const abort = () => {
        try {
          ctl.error(new DOMException('aborted', 'AbortError'));
        } catch {
          /* already ended */
        }
      };
      if (signal?.aborted) return abort();
      signal?.addEventListener('abort', abort, { once: true });
      // Queued and closed: an abort before the queue is read errors the
      // stream all the same, its chunks dropped, as a fetch drops a body.
      ctl.enqueue(new TextEncoder().encode(text));
      ctl.close();
    },
  });
  return new Response(body, { status, headers });
}

// ------------------------------------------------------------------- run

/** The app, its replica, and the network as the script plays it. */
class Run {
  constructor(sc) {
    this.shapes = sc.shapes ?? file.shapes;
    this.collections = JSON.stringify(sc.collections ?? file.collections);
    this.up = true;
    // The token the replica starts with: "t0" unless a start names one,
    // null for none.
    this.startToken = 't0';
    this.streamStatus = null;
    this.schemaStatus = null;
    // Answers held for the next `release`, in the order held.
    this.held = [];
    this.open = [];
    this.posts = [];
    this.timers = new Set();
    // The provider's call out, `{res, rej}`: one is out at a time, and a
    // second while it is fails the scenario (`askedTwice`).
    this.asking = null;
    this.askedTwice = false;
    this.closed = false;
    this.db = null;
    this.idb = fakeIndexedDB();
    this.clear();
  }

  clear() {
    this.requests = [];
    this.streams = [];
    this.refused = [];
    this.waits = [];
    this.tokenAsked = false;
    // What the provider was handed: `{code, had_token, attempt}`, as the
    // native core's token action says them.
    this.token = null;
    this.error = null;
    this.reason = null;
  }

  /**
   * An answer given only at the next `release`, the request out meanwhile:
   * an abort before then rejects it, as a real fetch's does, and it is
   * given to no one -- opened at the `release`, it was a stream no shape
   * held, which the next event went to.
   */
  hold(answer, signal) {
    return new Promise((res, rej) => {
      const give = () => res(answer());
      const abort = () => {
        this.held = this.held.filter((g) => g !== give);
        rej(new DOMException('aborted', 'AbortError'));
      };
      if (signal?.aborted) return abort();
      signal?.addEventListener('abort', abort, { once: true });
      this.held.push(give);
    });
  }

  fetch = (url, init = {}) => {
    const path = String(url).slice(URL_.length);
    const h = init.headers ?? {};
    if (h.accept === 'text/event-stream') {
      this.streams.push({ path, auth: h.authorization ?? null });
      if (!this.up) return Promise.reject(new TypeError('connection refused'));
      if (this.streamStatus) {
        const { status, body, held } = this.streamStatus;
        this.streamStatus = null;
        const answer = () => (status === 200 ? this.opened(path, init.signal) : answered(body, status, init.signal));
        return held ? this.hold(answer, init.signal) : Promise.resolve(answer());
      }
      return Promise.resolve(this.opened(path, init.signal));
    }
    const r = { method: init.method ?? 'GET', path, auth: h.authorization ?? null, key: h['idempotency-key'] ?? null };
    if (init.body != null) {
      r.body = String(init.body).split('\n').map((l) => JSON.parse(l));
      r.lines = r.body.length;
    }
    this.requests.push(r);
    if (r.method === 'GET') {
      if (!this.up) return Promise.reject(new TypeError('connection refused'));
      const { status, body, held } = this.schemaStatus ?? { status: 200, body: this.collections };
      this.schemaStatus = null;
      const answer = () => answered(body, status, init.signal);
      return held ? this.hold(answer, init.signal) : Promise.resolve(answer());
    }
    return new Promise((res, rej) => this.posts.push({ res, rej, signal: init.signal }));
  };

  /** A stream at `path` answered 200: open for the events the script writes to it. */
  opened(path, signal) {
    const c = path.slice(1).split('/')[0];
    let ctl;
    const body = new ReadableStream({ start: (c) => void (ctl = c) });
    const s = { c, ctl };
    this.open.push(s);
    signal?.addEventListener('abort', () => {
      this.open = this.open.filter((x) => x !== s);
      try {
        ctl.error(new DOMException('aborted', 'AbortError'));
      } catch {
        /* already ended */
      }
    });
    return new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } });
  }

  /** Timers the core would ask for: a backoff waits for `fire`, a frame's tick runs at once. */
  clock() {
    const real = { setTimeout: globalThis.setTimeout, clearTimeout: globalThis.clearTimeout };
    globalThis.setTimeout = (fn, ms = 0) => {
      if (ms <= 60) {
        const h = setImmediate(fn);
        return { unref() {}, h };
      }
      const t = { fn, ms, unref() {} };
      this.timers.add(t);
      this.waits.push(ms);
      return t;
    };
    globalThis.clearTimeout = (t) => {
      if (t?.h) clearImmediate(t.h);
      else if (t && this.timers.has(t)) this.timers.delete(t);
      else real.clearTimeout(t);
    };
    return () => Object.assign(globalThis, real);
  }

  async settle() {
    for (let i = 0; i < 60; i++) await new Promise((r) => setImmediate(r));
  }

  async start() {
    this.open = [];
    this.posts = [];
    this.held = [];
    this.timers.clear();
    this.asking = null;
    globalThis.indexedDB = this.idb;
    globalThis.IDBKeyRange = KeyRange;
    try {
      this.db = await sync({
        url: URL_,
        token: this.startToken ?? undefined,
        local: await Fennec.open(wasm),
        leader: false,
        persist: 'scenario',
        shapes: structuredClone(this.shapes),
        fetch: this.fetch,
        // Handed a `TokenRefusal`: its `expired` is its code's, always. Its
        // answer is the script's: a `token` step's, or a `token_failed`
        // step's error.
        tokenProvider: (r) => {
          this.askedTwice ||= this.asking !== null;
          this.tokenAsked = true;
          this.token = { code: r.errorCode, had_token: r.hadToken, attempt: r.attempt };
          if (r.expired !== (r.errorCode === 'token_expired')) this.token.expired = r.expired;
          return new Promise((res, rej) => (this.asking = { res, rej }));
        },
        onRefused: (e) => this.refused.push({ status: e.status, quota: e.quota ?? null, reason: e.reason ?? null }),
      });
    } catch (e) {
      this.db = null;
      this.error = String(e.message ?? e);
    }
  }

  event(n, name, data) {
    const c = this.shapes[n].collection;
    const s = this.open.find((x) => x.c === c);
    if (!s) throw new Error(`no stream open for \`${c}\``);
    const bytes = new TextEncoder().encode(`event: ${name}\ndata: ${JSON.stringify(data)}\n\n`);
    // Cut as a network cuts it.
    for (let i = 0; i < bytes.length; i += 7) s.ctl.enqueue(bytes.slice(i, i + 7));
  }

  write(w, on) {
    if (w.batch) {
      return this.db.batch(async (t) => {
        for (const x of w.batch) await this.write(x, t);
      });
    }
    // A FennecQL text as an app runs it on the replica itself: the page's
    // raw FennecQL is the replica's own database, `local`.
    if (w.text !== undefined) return this.db.local.query(w.text, w.params ?? []);
    const t = on ?? this.db;
    // `"require": n`, `"reason"` and `"returning"`: the builders'
    // `{ require: n }`, `{ reason }` and `{ returning }`.
    const opts = w.require === undefined ? {} : { require: w.require };
    if (w.reason !== undefined) opts.reason = w.reason;
    if (w.returning !== undefined) opts.returning = w.returning;
    if (w.insert) {
      w.docs.forEach((d, i) => sorted(d, `docs[${i}]`));
      return t.from(w.insert).insert(w.docs, opts);
    }
    // The builders' `insertNew`: FennecQL's `insert`.
    if (w.insert_new) {
      w.docs.forEach((d, i) => sorted(d, `docs[${i}]`));
      return t.from(w.insert_new).insertNew(w.docs, opts);
    }
    // `{"$inc": n}` is inc(n), `{"$expr": text}` expr(text), as the
    // builders' golden file writes them.
    const patch = (s) => {
      sorted(s, 'set');
      return Object.fromEntries(
        Object.entries(s).map(([k, v]) => [
          k,
          v !== null && typeof v === 'object' && '$inc' in v ? inc(v.$inc)
            : v !== null && typeof v === 'object' && '$expr' in v ? expr(v.$expr)
            : v,
        ]),
      );
    };
    if (w.upsert) {
      w.docs.forEach((d, i) => sorted(d, `docs[${i}]`));
      return t.from(w.upsert).upsert(w.docs, patch(w.set), opts);
    }
    const c = w.update ?? w.delete ?? w.get;
    let q = t.from(c);
    sorted(w.where ?? {}, 'where');
    for (const [k, v] of Object.entries(w.where ?? {})) q = q.where(k, v);
    // `"order": f` and `"limit": n`: the builders' `.order(f)` and `.limit(n)`.
    if (w.order) q = q.order(w.order);
    if (w.limit !== undefined) q = q.limit(w.limit);
    // A read in a batch: the builders' `.require(n, { reason })`.
    if (w.get) return (w.require === undefined ? q : q.require(w.require, w.reason === undefined ? undefined : { reason: w.reason })).rows();
    if (w.update) return q.update(patch(w.set), opts);
    return q.delete(opts);
  }

  async step(st) {
    this.clear();
    switch (st.do) {
      case 'start':
        if (st.shapes) this.shapes = st.shapes;
        this.startToken = 'token' in st ? st.token : 't0';
        await this.start();
        break;
      case 'restart':
        await this.settle();
        if (!this.closed) this.db.close();
        this.closed = false;
        if (st.shapes) this.shapes = st.shapes;
        this.startToken = 'token' in st ? st.token : 't0';
        await this.start();
        break;
      case 'seed':
      case 'change': {
        const data = { seq: st.seq ?? 0 };
        for (const k of ['rows', 'puts', 'dels', 'schema']) if (k in st) data[k] = st[k];
        this.event(st.shape ?? 0, st.do, data);
        break;
      }
      case 'error': {
        const data = { error: st.error };
        if ('status' in st) data.status = st.status;
        if ('code' in st) data.code = st.code;
        this.event(st.shape ?? 0, 'error', data);
        break;
      }
      case 'drop':
        for (const s of this.open.splice(0)) s.ctl.error(new TypeError('connection reset'));
        break;
      case 'fire': {
        const due = [...this.timers];
        this.timers.clear();
        for (const t of due) t.fn();
        break;
      }
      case 'answer': {
        const p = this.posts.shift();
        if (!p) throw new Error('no request waits for an answer');
        if (!st.status) p.rej(new TypeError('connection reset'));
        else {
          const body = st.body !== undefined ? JSON.stringify(st.body) : '{}';
          p.res(answered(body, st.status, p.signal, st.seq ? { 'fennec-seq': String(st.seq) } : {}));
        }
        break;
      }
      case 'write':
        // A refusal rejects the promise too, and is told to `onRefused`.
        this.write(st.write).catch((e) => {
          if (e.status === undefined) [this.error, this.reason] = [String(e.message ?? e), e.reason ?? null];
        });
        break;
      case 'net':
        this.up = st.up;
        break;
      case 'stream_status':
      case 'schema_status': {
        const answer = { status: st.status, body: st.body !== undefined ? JSON.stringify(st.body) : '{"error":"refused"}', held: st.held === true };
        if (st.do === 'stream_status') this.streamStatus = answer;
        else this.schemaStatus = answer;
        break;
      }
      // The answers held, given now: a request still out while a token was
      // handed over is answered for the one it carried.
      case 'release':
        for (const give of this.held.splice(0)) give();
        break;
      case 'online':
        this.db.setOnline(st.online);
        break;
      // The provider's answer to the ask out, or the app's own handover.
      case 'token':
        if (this.asking) {
          this.asking.res(st.token);
          this.asking = null;
        } else this.db.setToken(st.token);
        break;
      // The provider asked threw, or handed over none.
      case 'token_failed':
        if (!this.asking) throw new Error('a provider fails with no ask out');
        this.asking.rej(new Error(st.error));
        this.asking = null;
        break;
      case 'stop':
        this.db.close();
        this.closed = true;
        break;
      case 'schema':
        this.collections = JSON.stringify(st.collections);
        break;
      default:
        throw new Error(`a step of no kind: ${st.do}`);
    }
    await this.settle();
  }

  rows(c) {
    try {
      return [...(this.db.local.run(`get ${c}`).rows ?? [])].sort((a, b) => a.id - b.id);
    } catch {
      return [];
    }
  }

  check(e, binds) {
    if (e.requests) matches(e.requests, this.requests, binds, 'requests');
    if (e.streams) matches(e.streams, this.streams, binds, 'streams');
    for (const [c, rows] of Object.entries(e.rows ?? {})) matches(rows, this.rows(c), binds, `rows.${c}`);
    if ('pending' in e) matches(e.pending, this.db.status()[0].pending, binds, 'pending');
    // A status alone is a refusal told with no quota.
    if (e.refused) matches(e.refused.map((r) => (typeof r === 'number' ? { status: r, quota: null } : r)), this.refused, binds, 'refused');
    if ('quota' in e) matches(e.quota, this.db.status()[0].quota, binds, 'quota');
    if ('token_asked' in e) matches(e.token_asked, this.tokenAsked, binds, 'token_asked');
    if ('token' in e) matches(e.token, this.token, binds, 'token');
    if (this.askedTwice) throw new Error('a token asked for while the ask before was out');
    // A provider handed an `expired` its code does not say, whatever the step expects.
    if (this.token && 'expired' in this.token) throw new Error(`token: expired ${this.token.expired} for code ${this.token.code}`);
    // A text the status's last error says, null for none.
    if ('status_error' in e) {
      const said = this.db.status()[0].error;
      const ok = e.status_error === null ? said === null : typeof said === 'string' && said.includes(e.status_error);
      if (!ok) throw new Error(`status_error: expected ${JSON.stringify(e.status_error)}, got ${JSON.stringify(said)}`);
    }
    if ('code' in e) matches(e.code, this.db.status()[0].errorCode, binds, 'code');
    // What the status says the sync waits on a token for: the last ask's
    // three, null where it waits on none or waits before asking.
    if ('token_refusal' in e) {
      const r = this.db.status()[0].tokenRefusal;
      matches(e.token_refusal, r && { code: r.errorCode, had_token: r.hadToken, attempt: r.attempt }, binds, 'token_refusal');
    }
    if (e.waits) {
      assert.equal(this.waits.length, e.waits.length, `waits: ${e.waits.length} expected, ${JSON.stringify(this.waits)} asked`);
      e.waits.forEach(([lo, hi], i) => {
        const ms = this.waits[i];
        assert.ok(ms >= lo && ms <= hi, `waits: ${ms} ms is not within [${lo}, ${hi}]`);
      });
    }
    if ('ready' in e) matches(e.ready, this.db.status().every((s) => s.seeded), binds, 'ready');
    if (e.error) assert.ok(this.error?.includes(e.error), `error: expected one saying \`${e.error}\`, got ${this.error}`);
    else assert.equal(this.error, null, `an error no expectation names: ${this.error}`);
    if ('reason' in e) matches(e.reason, this.reason, binds, 'reason');
  }
}

const passed = [];

for (const sc of file.scenarios) {
  test(sc.name, { skip, concurrency: false }, async () => {
    assert.ok(wasm, 'web/fennec.wasm is missing: under CI every scenario runs (make wasm)');
    const run = new Run(sc);
    const restore = run.clock();
    const binds = new Map();
    try {
      for (const [i, raw] of sc.steps.entries()) {
        if ((raw.expect_js || raw.expect_native) && !sc.differs) throw new Error(`step ${i}: a platform's own expectation, and no \`differs\` saying why`);
        if (raw.only && raw.only !== 'js') continue;
        const st = resolve(raw, binds);
        await run.step(st);
        try {
          run.check({ ...st.expect, ...st.expect_js }, binds);
        } catch (e) {
          e.message = `step ${i} (${st.do}): ${e.message}`;
          throw e;
        }
      }
      passed.push(sc.name);
    } finally {
      if (!run.closed) run.db?.close();
      restore();
    }
  });
}

after(async () => {
  if (skip) return;
  const dir = new URL('../target/sync-scenarios/', import.meta.url);
  await mkdir(dir, { recursive: true });
  await writeFile(new URL('js.txt', dir), passed.map((n) => `${n}\n`).join(''));
});

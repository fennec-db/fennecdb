// `@fennecdb/web/client` (web/client.js): the HTTP client and the builder
// without the engine. A bundler's output for a page that imports it is the
// modules it reaches, so those are held to the three of the client --
// none of them the module's glue, persistence, files or the sync layer --
// and its live queries to a server's subscription stream, scripted here
// (web/fennec.sync.test.js runs them against a real fennec-server).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import * as client from './client.js';
import * as fennec from './fennec.js';

/** Every module `entry` reaches through its static imports, by file name. */
async function graph(entry) {
  const seen = new Map();
  const visit = async (name) => {
    if (seen.has(name)) return;
    const text = await readFile(new URL(`./${name}`, import.meta.url), 'utf8');
    seen.set(name, text);
    for (const m of text.matchAll(/^(?:import|export)\b[^;]*?\bfrom\s+'\.\/([^']+)'/gm)) await visit(m[1]);
    for (const m of text.matchAll(/\bimport\s*\(\s*['"]\.\/([^'"]+)['"]/g)) await visit(m[1]);
  };
  await visit(entry);
  return seen;
}

test('the client reaches the builder and the HTTP client, and nothing of the engine', async () => {
  const modules = await graph('client.js');
  assert.deepEqual([...modules.keys()].sort(), ['builder.js', 'client.js', 'http.js']);
  for (const [name, text] of modules) {
    // Comments may name them; the code may not.
    const code = text.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/[^\n]*/g, '');
    for (const word of ['WebAssembly', 'fennec_query', 'fennec_open', 'FennecSync', 'indexedDB', 'getDirectory', 'fennec.wasm', 'class Fennec ']) {
      assert.ok(!code.includes(word), `${name} holds ${word}`);
    }
  }
  // And fennec.js reaches the same two, so the classes are one.
  assert.deepEqual([...(await graph('fennec.js')).keys()].sort(), ['builder.js', 'fennec.js', 'http.js']);
});

test('the client exports what fennec.js does of it, the same objects', () => {
  assert.deepEqual(Object.keys(client).sort(), [
    'Call', 'FennecError', 'FennecHttp', 'Param', 'Query', 'and', 'bucket', 'call', 'connect', 'countDistinct', 'distance', 'expr', 'first', 'from', 'inc', 'last', 'not', 'or', 'param', 'raw', 'sseEvents',
  ]);
  for (const [name, value] of Object.entries(client)) assert.equal(value, fennec[name], name);
  assert.equal(client.from('docs').where('year', 2024).toFennecQL()[0], 'get docs where year = $1');
});

/**
 * A server's subscription streams, scripted: `fetch` answers `/query` from
 * `rows()`, and each `GET /<c>/changes` with a stream `emit` writes events
 * into. A `this` other than the global object is refused, as a browser's
 * own `fetch` refuses it.
 */
function scripted(rows) {
  const streams = [];
  const queries = [];
  function fetch(url, init = {}) {
    if (this !== undefined && this !== globalThis) throw new TypeError('Illegal invocation');
    const u = new URL(url);
    if (init.method === 'POST') {
      queries.push(JSON.parse(init.body));
      return Promise.resolve(new Response(JSON.stringify(rows()), { status: 200 }));
    }
    let push;
    const body = new ReadableStream({
      start(c) {
        push = c;
      },
    });
    const s = {
      path: u.pathname,
      search: u.search,
      auth: init.headers?.authorization,
      emit: (name, data) => push.enqueue(new TextEncoder().encode(`event: ${name}\ndata: ${JSON.stringify(data)}\n\n`)),
      end: () => push.close(),
    };
    init.signal?.addEventListener('abort', () => {
      s.aborted = true;
      try {
        push.error(new DOMException('aborted', 'AbortError'));
      } catch {
        /* closed */
      }
    });
    streams.push(s);
    return Promise.resolve(new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } }));
  }
  return { fetch, streams, queries };
}

const tick = () => new Promise((r) => setTimeout(r, 5));
const until = async (f, tries = 200) => {
  for (let i = 0; i < tries && !f(); i++) await tick();
  assert.ok(f(), 'timed out');
};

test('a live query over HTTP runs once every stream is seeded, and again on a write', async () => {
  let n = 1;
  const server = scripted(() => [{ id: 1, n }]);
  const db = client.connect('http://db.test/', { token: 't', fetch: server.fetch });
  const seen = [];
  const q = db.from('docs').where('n', '>', 0).lookup('notes', { on: 'doc' });
  const stop = db.live(q, (rows) => seen.push(rows[0].n));
  await until(() => server.streams.length === 2);
  // A stream a collection the query reads, of a shape that holds nothing.
  assert.deepEqual(server.streams.map((s) => s.path).sort(), ['/docs/changes', '/notes/changes']);
  for (const s of server.streams) {
    assert.equal(s.search, '?select=id&where=false');
    assert.equal(s.auth, 'Bearer t');
  }
  // The first rows wait for both seeds.
  server.streams[0].emit('seed', { seq: 1, rows: [] });
  await tick();
  assert.deepEqual(seen, []);
  server.streams[1].emit('seed', { seq: 1, rows: [] });
  await until(() => seen.length === 1);
  // A write to either runs it again; a burst while it runs, once more.
  n = 2;
  server.streams[1].emit('change', { seq: 2, puts: [], dels: [7], schema: false });
  await until(() => seen.length === 2);
  n = 3;
  server.streams[0].emit('change', { seq: 3, puts: [], dels: [1], schema: false });
  server.streams[0].emit('change', { seq: 4, puts: [], dels: [2], schema: false });
  server.streams[1].emit('change', { seq: 5, puts: [], dels: [3], schema: false });
  await until(() => seen.at(-1) === 3);
  await tick();
  assert.ok(seen.length <= 4, `a burst ran it ${seen.length - 2} times`);
  assert.equal(server.queries[0].query, q.toFennecQL()[0]);
  stop();
  await until(() => server.streams.every((s) => s.aborted));
  const before = seen.length;
  n = 4;
  server.queries.length = 0;
  await tick();
  assert.equal(seen.length, before);
});

test('a stream that ends is opened again, and its seed runs the query', async () => {
  let n = 1;
  const server = scripted(() => [{ id: 1, n }]);
  const db = client.connect('http://db.test', { fetch: server.fetch });
  const seen = [];
  const errors = [];
  const stop = db.live('get docs', (rows) => seen.push(rows[0].n), { collections: ['docs'], onError: (e) => errors.push(e) });
  await until(() => server.streams.length === 1);
  server.streams[0].emit('seed', { seq: 1, rows: [] });
  await until(() => seen.length === 1);
  // The server says why it ends a stream: that goes to onError, and the
  // stream is opened again, its seed running the query once more.
  server.streams[0].emit('error', { error: 'the tenant was closed on this node' });
  await until(() => errors.length === 1 && server.streams.length === 2);
  assert.match(errors[0].message, /tenant was closed/);
  n = 2;
  server.streams[1].emit('seed', { seq: 9, rows: [] });
  await until(() => seen.at(-1) === 2);
  stop();
});

// The server ends a stream at its token's `exp` with a 401 and its code,
// `token_expired`: the live query says so as an auth error and stops --
// opened again with the same token it was refused at every attempt, each
// refusal waiting longer -- for the app to open it again with a fresh one,
// told an expiry by the code alone (`errorCode`).
test("a stream ended at its token's exp is an auth error, and is not opened again", async () => {
  const server = scripted(() => [{ id: 1 }]);
  const db = client.connect('http://db.test', { token: 'old', fetch: server.fetch });
  const errors = [];
  db.live('get docs', () => {}, { collections: ['docs'], onError: (e) => errors.push(e) });
  await until(() => server.streams.length === 1);
  server.streams[0].emit('seed', { seq: 1, rows: [] });
  server.streams[0].emit('error', { error: 'the token has expired', status: 401, code: 'token_expired' });
  await until(() => errors.length === 1);
  assert.ok(errors[0] instanceof client.FennecError);
  assert.equal(errors[0].status, 401);
  assert.equal(errors[0].errorCode, 'token_expired');
  assert.match(errors[0].message, /expired/);
  await new Promise((r) => setTimeout(r, 400));
  assert.equal(server.streams.length, 1, 'opened again with the token refused');
});

// A shape's subscription through the client: Trellis and its tests each
// carried a reader of their own, the client keeping its own to itself.
test('subscribe hands a shape its seed and changes, opens it again, and stops at a 401', async () => {
  const server = scripted(() => []);
  const db = client.connect('http://db.test', { token: 't', fetch: server.fetch });
  const events = [];
  const errors = [];
  const states = [];
  const stop = db.subscribe('tasks', { team: 'eq.design', select: ['id', 'title'] }, (ev) => events.push(ev), {
    onError: (e) => errors.push(e),
    onState: (s) => states.push(s),
  });
  await until(() => server.streams.length === 1);
  assert.equal(server.streams[0].path, '/tasks/changes');
  assert.equal(server.streams[0].search, '?team=eq.design&select=id%2Ctitle');
  assert.equal(server.streams[0].auth, 'Bearer t');
  server.streams[0].emit('seed', { seq: 3, rows: [{ id: 1, title: 'a' }] });
  server.streams[0].emit('change', { seq: 4, puts: [{ id: 2, title: 'b' }], dels: [1], schema: false });
  await until(() => events.length === 2);
  assert.deepEqual(events, [
    { type: 'seed', seq: 3, rows: [{ id: 1, title: 'a' }] },
    { type: 'change', seq: 4, puts: [{ id: 2, title: 'b' }], dels: [1], schema: false },
  ]);
  // The server says why a stream ends: told, and the stream opened again.
  server.streams[0].emit('error', { error: 'the tenant was closed on this node' });
  await until(() => server.streams.length === 2);
  assert.match(errors[0].message, /tenant was closed/);
  server.streams[1].emit('seed', { seq: 9, rows: [] });
  await until(() => events.length === 3);
  assert.deepEqual(states, ['open', 'retry', 'open']);
  // Ended at its token's exp: an auth error, and not opened again.
  server.streams[1].emit('error', { error: 'the token has expired', status: 401, code: 'token_expired' });
  await until(() => errors.length === 2);
  assert.equal(errors[1].status, 401);
  assert.equal(errors[1].errorCode, 'token_expired');
  assert.equal(errors[0].errorCode, undefined, 'a stream ended for the tenant names no code');
  await new Promise((r) => setTimeout(r, 400));
  assert.equal(server.streams.length, 2);
  stop();
  assert.throws(() => db.subscribe('tasks', { since: '3' }, () => {}), /since/);
  assert.throws(() => db.subscribe('tasks', {}, null), /onEvent/);
});

test('sseEvents reads the events of a stream cut anywhere', async () => {
  const text = 'event: seed\r\ndata: {"seq":1}\r\n\r\n: keepalive\n\nevent: change\ndata: {"seq":2}\n\n';
  const bytes = new TextEncoder().encode(text);
  const body = new ReadableStream({
    start(c) {
      for (let i = 0; i < bytes.length; i += 5) c.enqueue(bytes.slice(i, i + 5));
      c.close();
    },
  });
  const got = [];
  for await (const ev of client.sseEvents(new Response(body))) got.push(ev);
  assert.deepEqual(got, [
    { name: 'seed', data: '{"seq":1}' },
    { name: 'change', data: '{"seq":2}' },
  ]);
});

test('sseEvents reads the same events at every cut: \\r\\n endings, \\r | \\n and \\n | \\n splits', async () => {
  const texts = [
    'event: seed\r\ndata: {"seq":1}\r\n\r\n: keepalive\r\n\r\nevent: change\r\ndata: a\r\ndata: b\r\n\r\n',
    'event: x\ndata: 1\n\n\n\nevent: y\ndata: 2\n\nevent: z\n\n',
    'event: x\r\ndata: 1\n\r\nevent: y\ndata: 2\n\n',
  ];
  const read = async (cuts, bytes) => {
    const body = new ReadableStream({
      start(c) {
        let at = 0;
        for (const end of [...cuts, bytes.length]) {
          if (end > at) c.enqueue(bytes.slice(at, end));
          at = Math.max(at, end);
        }
        c.close();
      },
    });
    const got = [];
    for await (const ev of client.sseEvents(new Response(body))) got.push(ev);
    return got;
  };
  for (const text of texts) {
    const bytes = new TextEncoder().encode(text);
    const whole = await read([], bytes);
    assert.ok(whole.length >= 2, text);
    for (let a = 1; a < bytes.length; a++) {
      assert.deepEqual(await read([a], bytes), whole, `cut at ${a} of ${JSON.stringify(text)}`);
      assert.deepEqual(await read([a, a + 1], bytes), whole, `cuts at ${a} and ${a + 1}`);
    }
    assert.deepEqual(await read([...bytes.keys()].slice(1), bytes), whole, 'a byte a piece');
  }
  assert.deepEqual(await read([], new TextEncoder().encode(texts[0])), [
    { name: 'seed', data: '{"seq":1}' },
    { name: 'change', data: 'ab' },
  ]);
  assert.deepEqual(await read([], new TextEncoder().encode(texts[1])), [
    { name: 'x', data: '1' },
    { name: 'y', data: '2' },
    { name: 'z', data: '' },
  ]);
  // A \r ending one piece and the \n starting the next are one line ending.
  const split = await read([20], new TextEncoder().encode('event: a\r\ndata: 1\r\n\r\n'));
  assert.deepEqual(split, [{ name: 'a', data: '1' }]);
});

test('a whole number from 2^53 to 2^63 goes out as every digit of it, anything else as JSON.stringify writes it', async () => {
  const { jsonOf, from, bodyOf } = await import('./builder.js');
  assert.equal(JSON.stringify(2 ** 60), '1152921504606847000');
  assert.equal(jsonOf(2 ** 60), '1152921504606846976');
  assert.equal(jsonOf(-(2 ** 60)), '-1152921504606846976');
  assert.equal(jsonOf(2 ** 53), '9007199254740992');
  assert.equal(jsonOf(2 ** 63 - 1024), '9223372036854774784');
  // From 2^63 on the server reads the digits as the float they say.
  for (const v of [2 ** 63, -(2 ** 63), 1e21, 1e300, 2 ** 53 - 1, 1.5, 0, -0, NaN, 'x', null, true, [], {}]) {
    assert.equal(jsonOf(v), JSON.stringify(v), String(v));
  }
  const v = { a: [1, 2 ** 60, { b: -(2 ** 54), c: 'text', d: new Date(0) }], e: 0.1, f: undefined };
  assert.equal(jsonOf(v), '{"a":[1,1152921504606846976,{"b":-18014398509481984,"c":"text","d":"1970-01-01T00:00:00.000Z"}],"e":0.1}');
  assert.deepEqual(JSON.parse(jsonOf(v)), JSON.parse(JSON.stringify(v)));
  // A text or a key that starts as the mark does leaves the text as
  // JSON.stringify writes it.
  for (const odd of [['\u0000fennec-int:5', 2 ** 60], { '\u0000fennec-int:5': 2 ** 60 }]) {
    assert.equal(jsonOf(odd), JSON.stringify(odd));
  }
  assert.throws(() => {
    const cycle = [2 ** 60];
    cycle.push(cycle);
    jsonOf(cycle);
  }, TypeError);
  // A request's parameters are looked through as they are made, and a
  // builder's text's walked: those holding no such number are written as
  // JSON.stringify writes them, and the others with every digit -- a typed
  // array's too.
  for (const [n, text] of [[7, '7'], [2 ** 60, '1152921504606846976']]) {
    const [, params] = from('t').toInsert({ m: { a: [n] } });
    assert.equal(jsonOf({ params }), `{"params":[{"a":[${text}]}]}`);
    assert.equal(bodyOf('q', [{ a: [n] }]), `{"query":"q","params":[{"a":[${text}]}]}`);
    assert.equal(bodyOf('q', [new Float64Array([n])]), `{"query":"q","params":[[${text}]]}`);
  }
});

test('a text says what it reads, or a live query over HTTP is refused', () => {
  const db = client.connect('http://db.test', { fetch: scripted(() => []).fetch });
  assert.throws(() => db.live('get docs', () => {}), /name the collections a text reads/);
  assert.throws(() => db.live(client.from('docs').where(client.raw('id in (get other select id)')), () => {}), /name the collections/);
  assert.throws(() => db.live(db.from('docs'), null), /cb must be a function/);
});

test('a query from an endpoint carries it, for useLiveQuery to find', () => {
  const db = client.connect('http://db.test', { fetch: scripted(() => []).fetch });
  assert.equal(db.from('docs').context, db);
});

// ------------------------------------------------------ retries and timeouts
//
// The client sends again only what cannot have run -- an answer naming a
// wait, a connection refused -- or may run twice -- a read the builder made
// or sent near, a write under a key -- and bounds every request in time
// (docs/design/js-client.md#retries).

/**
 * A server answering from `plan`, one item a request: a `Response`; 'reset',
 * a connection dropped after the request went; 'refused', one refused before
 * anything was sent; 'hang', no answer until the request's signal aborts it;
 * 'stall', a head and then a body that never ends; 'trickle', a head and
 * then `[{"id":1}]` a byte every 20 ms. Each request is noted with the time
 * it came, the key it carried and its signal.
 */
function planned(...plan) {
  const calls = [];
  const failed = (code) => new TypeError('fetch failed', { cause: Object.assign(new Error(code), { code }) });
  function fetch(url, init = {}) {
    if (this !== undefined && this !== globalThis) throw new TypeError('Illegal invocation');
    calls.push({ url, at: performance.now(), key: init.headers?.['idempotency-key'], signal: init.signal });
    const next = plan.shift();
    const aborted = (no) => init.signal?.addEventListener('abort', () => no(init.signal.reason));
    if (next === 'reset') return Promise.reject(failed('ECONNRESET'));
    if (next === 'refused') return Promise.reject(failed('ECONNREFUSED'));
    if (next === 'hang') return new Promise((_, no) => aborted(no));
    if (next === 'stall') {
      const body = new ReadableStream({ start: (c) => aborted((e) => c.error(e)) });
      return Promise.resolve(new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } }));
    }
    if (next === 'trickle') {
      const bytes = new TextEncoder().encode('[{"id":1}]');
      let i = 0;
      const body = new ReadableStream({
        start: (c) => aborted((e) => c.error(e)),
        pull: async (c) => {
          await new Promise((r) => setTimeout(r, 20));
          if (i < bytes.length) c.enqueue(bytes.subarray(i, ++i));
          else c.close();
        },
      });
      return Promise.resolve(new Response(body, { status: 200, headers: { 'content-type': 'application/json' } }));
    }
    return Promise.resolve(next);
  }
  return { fetch, calls };
}

const answer = (status, body, headers = {}) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json', ...headers } });

test('a 503 naming its wait is sent again, no sooner than the wait', async () => {
  const server = planned(
    answer(503, { error: 'the tenant is being moved; retry shortly', retry_after: 1 }, { 'retry-after': '1' }),
    answer(200, { affected: 1 }),
  );
  const db = client.connect('http://db.test', { fetch: server.fetch });
  // A write without a key: nothing ran, so it goes again all the same.
  assert.deepEqual(await db.run('put notes {n: 1}'), { kind: 'affected', count: 1, seq: null, replayed: false });
  assert.equal(server.calls.length, 2);
  // `Retry-After: 1`. Node's timers count from the loop's time, taken as
  // its turn began: one may fire a few ms before a second by the clock.
  const took = server.calls[1].at - server.calls[0].at;
  assert.ok(took >= 990, `${took} ms`);
  // A quota's 429 says it to the millisecond, before its whole seconds.
  const quota = planned(
    answer(429, { error: 'past its quota of 2 writes a second', quota: 'writes', retry_after: 1, retry_after_ms: 300 }, { 'retry-after': '1' }),
    answer(200, { affected: 1 }),
  );
  await client.connect('http://db.test', { fetch: quota.fetch }).run('put notes {n: 2}');
  assert.equal(quota.calls.length, 2);
  const ms = quota.calls[1].at - quota.calls[0].at;
  assert.ok(ms >= 290, `${ms} ms`);
});

test('a refusal naming no wait is never sent again', async () => {
  // A quota waiting makes no room for -- `held`'s 429, a 507 -- a 4xx, a
  // write's unknown outcome, and a 503 that names no wait.
  for (const refusal of [
    answer(429, { error: 'past its quota of 4 held requests', quota: 'held' }),
    answer(507, { error: 'past its quota of 5 rows', quota: 'rows' }),
    answer(409, { error: 'a value taken' }),
    answer(500, { error: 'the write landed', outcome: 'unknown', seq: 7 }),
    answer(503, { error: 'no wait named' }),
  ]) {
    const server = planned(refusal, answer(200, []));
    const err = await client.connect('http://db.test', { fetch: server.fetch }).from('docs').rows().catch((e) => e);
    assert.ok(err instanceof client.FennecError, `${refusal.status}`);
    assert.equal(err.status, refusal.status);
    assert.equal(server.calls.length, 1, `${refusal.status} sent again`);
  }
});

// A 401 for a token past its `exp` names its code, `errorCode` as every
// SDK names it; any other 401 names none, and a code the client does not
// know -- a later server's -- is passed on as it came.
test('a 401 for a token past its exp names its code, token_expired; any other none', async () => {
  for (const [body, code] of [
    [{ error: 'the token has expired', code: 'token_expired' }, 'token_expired'],
    [{ error: "the token's signature does not match" }, undefined],
    [{ error: 'refused', code: 'a_later_code' }, 'a_later_code'],
    [{ error: 'refused', code: 7 }, undefined],
  ]) {
    const server = planned(answer(401, body, { 'www-authenticate': 'Bearer' }), answer(200, []));
    const err = await client.connect('http://db.test', { token: 't', fetch: server.fetch }).from('docs').rows().catch((e) => e);
    assert.ok(err instanceof client.FennecError);
    assert.equal(err.status, 401);
    assert.equal(err.errorCode, code);
    assert.equal(server.calls.length, 1, 'a refused token is sent again');
  }
});

test('a write is sent again after a dropped connection only under a key; a read where the builder made it or it went near', async () => {
  const bare = planned('reset', answer(200, { affected: 1 }));
  const err = await client.connect('http://db.test', { fetch: bare.fetch }).run('put notes {n: 1}').catch((e) => e);
  assert.equal(err.cause.code, 'ECONNRESET', 'the network error, as fetch threw it');
  assert.equal(bare.calls.length, 1, 'a write that may have landed is never made twice');
  assert.equal(bare.calls[0].key, undefined, 'the client makes no key of its own');

  const keyed = planned('reset', answer(200, { affected: 1 }, { 'idempotent-replayed': 'true' }));
  const k = client.connect('http://db.test', { fetch: keyed.fetch });
  assert.equal((await k.run('put notes {n: 1}', [], { idempotencyKey: 'n-1' })).replayed, true);
  assert.deepEqual(keyed.calls.map((c) => c.key), ['n-1', 'n-1']);
  const copy = planned('reset', answer(200, { affected: 1 }));
  await client.connect('http://db.test', { fetch: copy.fetch }).withIdempotencyKey('n-2').from('notes').insert({ n: 2 });
  assert.deepEqual(copy.calls.map((c) => c.key), ['n-2', 'n-2']);

  // A read the builder made goes again, a batch of its queries alone too.
  // A text, said to read or not, is the caller's: said home, it is a claim
  // nothing checks -- a call to `history` stops being a read once the
  // statement is made to log a row -- and goes once.
  const reads = planned(
    'reset',
    answer(200, [{ id: 1 }]),
    'reset',
    answer(200, { results: [{ rows: [] }] }),
    'reset',
    'reset',
    'reset',
  );
  const r = client.connect('http://db.test', { fetch: reads.fetch });
  assert.deepEqual(await r.from('docs').rows(), [{ id: 1 }]);
  assert.equal((await r.batch([r.from('docs')])).results.length, 1);
  assert.equal(await r.rows('get docs').catch((e) => e.cause.code), 'ECONNRESET');
  assert.equal(await r.run('get docs', [], { read: true }).catch((e) => e.cause.code), 'ECONNRESET');
  assert.equal(await r.call('history', {}, { read: true }).catch((e) => e.cause.code), 'ECONNRESET');
  assert.equal(reads.calls.length, 7);

  // Said to read on a client reading near, a text goes marked
  // (`Fennec-Read: replica`), which every server runs read-only: it goes
  // again.
  const near = planned('reset', answer(200, [{ id: 2 }]));
  const n = client.connect('http://db.test', { fetch: near.fetch, reads: 'nearest' });
  assert.deepEqual((await n.run('get docs', [], { read: true })).rows, [{ id: 2 }]);
  assert.equal(near.calls.length, 2);

  // A connection refused sent nothing: any request goes again.
  const refused = planned('refused', answer(200, { affected: 1 }));
  assert.equal((await client.connect('http://db.test', { fetch: refused.fetch }).run('put notes {n: 3}')).count, 1);
  assert.equal(refused.calls.length, 2);
});

test('the tries are bounded: attempts, and their waits within maxWait', async () => {
  const busy = () => answer(503, { error: 'too many connections', retry_after: 1, retry_after_ms: 5 });
  const server = planned(busy(), busy(), busy(), busy(), answer(200, []));
  const err = await client.connect('http://db.test', { fetch: server.fetch }).from('docs').rows().catch((e) => e);
  assert.equal(server.calls.length, 3, 'three tries unless told');
  // The last answer, with what the server said.
  assert.equal(err.status, 503);
  assert.equal(err.retryAfter, 0.005);

  const four = planned(busy(), busy(), busy(), answer(200, [{ id: 1 }]));
  const db = client.connect('http://db.test', { fetch: four.fetch, retry: { attempts: 4 } });
  assert.deepEqual(await db.from('docs').rows(), [{ id: 1 }]);
  assert.equal(four.calls.length, 4);

  // A wait past what is left of maxWait is not waited: the refusal at once.
  const long = planned(answer(429, { error: 'past', quota: 'writes', retry_after_ms: 5000 }), answer(200, []));
  const late = await client
    .connect('http://db.test', { fetch: long.fetch, retry: { maxWait: 1000 } })
    .run('put notes {n: 1}')
    .catch((e) => e);
  assert.equal(late.retryAfter, 5);
  assert.equal(long.calls.length, 1);

  // Off: one try whatever the answer.
  const off = planned(busy(), answer(200, []));
  const once = await client.connect('http://db.test', { fetch: off.fetch, retry: false }).from('docs').rows().catch((e) => e);
  assert.equal(once.status, 503);
  assert.equal(off.calls.length, 1);
  assert.throws(() => client.connect('http://db.test', { fetch: off.fetch, retry: { attempts: 0 } }), /retry/);
  assert.throws(() => client.connect('http://db.test', { fetch: off.fetch, timeout: 0 }), /timeout/);
});

test('a request is given up at its timeout and never sent again; its body is timed between its pieces', async () => {
  const hung = planned('hang', answer(200, { affected: 1 }));
  const db = client.connect('http://db.test', { fetch: hung.fetch, timeout: 50 });
  const t0 = performance.now();
  const err = await db.run('put notes {n: 1}').catch((e) => e);
  assert.equal(err.name, 'TimeoutError');
  assert.match(err.message, /no answer within 50 ms/);
  assert.ok(performance.now() - t0 >= 40);
  assert.equal(hung.calls.length, 1, 'a write with no key that timed out may have landed');

  // Past the client's own timeout a request may still run -- a `compact`, a
  // `create index` -- and three tries of a long read ran it three times:
  // a read the builder made is not sent again either, nor a write under a
  // key.
  const read = planned('hang', answer(200, [{ id: 1 }]));
  const late = await client.connect('http://db.test', { fetch: read.fetch, timeout: 50 }).from('docs').rows().catch((e) => e);
  assert.equal(late.name, 'TimeoutError');
  assert.equal(read.calls.length, 1);
  const keyed = planned('hang', answer(200, { affected: 1 }));
  const k = client.connect('http://db.test', { fetch: keyed.fetch, timeout: 50 });
  assert.equal((await k.run('put notes {n: 1}', [], { idempotencyKey: 'n-1' }).catch((e) => e)).name, 'TimeoutError');
  assert.equal(keyed.calls.length, 1);

  // A head that came, and a body that stops: given up the timeout after its
  // last piece.
  const stalled = planned('stall', answer(200, [{ id: 1 }]));
  const r = client.connect('http://db.test', { fetch: stalled.fetch, timeout: 50 });
  assert.equal((await r.from('docs').rows().catch((e) => e)).name, 'TimeoutError');
  assert.equal(stalled.calls.length, 1);
  assert.ok(stalled.calls[0].signal.aborted);

  // A body that keeps coming is read for as long as it does: 10 pieces 20
  // ms apart, 200 ms whole against a timeout of 50.
  const slow = planned('trickle');
  const t1 = performance.now();
  const rows = await client.connect('http://db.test', { fetch: slow.fetch, timeout: 50 }).from('docs').rows();
  assert.deepEqual(rows, [{ id: 1 }]);
  assert.ok(performance.now() - t1 >= 150, 'read whole, past the timeout');
  assert.equal(slow.calls.length, 1);
});

test('a batch whose statements landed is never sent again whole, whatever its stop says', async () => {
  // A batch holding a `compact` runs a statement at a time; a stop after
  // one landed, sent again whole, made it twice. The server answers such a
  // stop with neither, and the client holds to it whatever it is told.
  for (const stop of [
    answer(503, { error: 'the tenant is being moved; retry shortly', completed: 1, at: 1, retry_after: 1 }, { 'retry-after': '1' }),
    answer(429, { error: 'too many runs', completed: 2, at: 2, retry_after: 1 }, { 'retry-after': '1' }),
  ]) {
    const server = planned(stop, answer(200, { results: [] }));
    const db = client.connect('http://db.test', { fetch: server.fetch });
    const err = await db.batch(['compact notes', 'put notes {n: 1}', 'put notes {n: 2}']).catch((e) => e);
    assert.equal(err.status, stop.status);
    assert.ok(err.completed > 0);
    assert.equal(server.calls.length, 1, `${stop.status} after a landing sent again`);
  }
});

test("the caller's signal gives a request up, never sent again", async () => {
  const hung = planned('hang', answer(200, []));
  const stop = new AbortController();
  setTimeout(() => stop.abort(), 20);
  const err = await client
    .connect('http://db.test', { fetch: hung.fetch })
    .run('get docs', [], { read: true, signal: stop.signal })
    .catch((e) => e);
  assert.equal(err.name, 'AbortError');
  assert.equal(hung.calls.length, 1, 'a read the caller gave up is not sent again');

  // Given up as it waits to go again.
  const busy = planned(answer(503, { error: 'moved', retry_after: 5 }), answer(200, []));
  const quit = new AbortController();
  setTimeout(() => quit.abort(), 20);
  const late = await client
    .connect('http://db.test', { fetch: busy.fetch })
    .run('get docs', [], { read: true, signal: quit.signal })
    .catch((e) => e);
  assert.equal(late.name, 'AbortError');
  assert.equal(busy.calls.length, 1);
});

test('a stream is timed to its head, and opened again no sooner than a 503 says', async () => {
  // 800 ms, past the 500 its second try's backoff would wait.
  const server = planned('hang', answer(503, { error: 'too many subscriptions', retry_after: 1, retry_after_ms: 800 }), 'stall');
  const db = client.connect('http://db.test', { fetch: server.fetch, timeout: 50 });
  const errors = [];
  const stop = db.subscribe('tasks', {}, () => {}, { onError: (e) => errors.push(e) });
  await until(() => server.calls.length === 3, 2000);
  assert.equal(errors[0].name, 'TimeoutError', 'no head within the timeout');
  assert.equal(errors[1].status, 503);
  const took = server.calls[2].at - server.calls[1].at;
  assert.ok(took >= 790, `${took} ms`);
  // Open, and quiet past the timeout: a stream's body is not timed.
  await new Promise((r) => setTimeout(r, 150));
  assert.equal(server.calls[2].signal.aborted, false);
  assert.equal(errors.length, 2);
  stop();
  await until(() => server.calls[2].signal.aborted);
});


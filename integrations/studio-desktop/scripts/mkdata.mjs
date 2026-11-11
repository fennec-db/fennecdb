// Makes a .fennec file to open the app on: the `orders` collection the studio's
// own end-to-end tests use (studio/test/harness.mjs), N rows, and `docs`.
//
//   bun scripts/mkdata.mjs out.fennec [rows=100000] [vector-dim=0]
//
// With a dimension, `orders` also gets `emb vector<dim> @hnsw`, which is what
// makes a big file slow to open the first time (the graph is built, then kept
// in the file at shutdown).
import { spawn } from 'node:child_process';
import { rmSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const [out, rowsArg = '100000', dimArg = '0'] = process.argv.slice(2);
if (!out) throw new Error('usage: mkdata.mjs out.fennec [rows] [dim]');
const n = Number(rowsArg);
const dim = Number(dimArg);
const WINDOWS = process.platform === 'win32';
const bin = process.env.FENNEC_SERVER_BIN ?? fileURLToPath(new URL(`../bin/fennec-server${WINDOWS ? '.exe' : ''}`, import.meta.url));
const TOKEN = 'mkdata';
for (const f of [out, `${out}.lock`, `${out}.sync`]) rmSync(f, { force: true });

// Stopped by its own shutdown, which writes the checkpoint: SIGTERM, and on Windows, where
// kill("SIGTERM") is TerminateProcess, the end of its stdin (--exit-on-stdin-close).
const child = spawn(bin, ['--file', out, '--http', '127.0.0.1:0', '--sync', 'off', ...(WINDOWS ? ['--exit-on-stdin-close'] : [])], {
  env: { ...process.env, FENNEC_HTTP_TOKEN: TOKEN },
  stdio: [WINDOWS ? 'pipe' : 'ignore', 'ignore', 'pipe'],
});
const url = await new Promise((resolve, reject) => {
  let text = '';
  child.stderr.on('data', (b) => {
    text += b;
    const m = /listening on: (http:\/\/\S+)/.exec(text);
    if (m) resolve(m[1]);
  });
  child.on('exit', (c) => reject(new Error(`server exited ${c}: ${text}`)));
});

async function query(text, params = []) {
  const r = await fetch(`${url}/query`, { method: 'POST', headers: { 'content-type': 'application/json', authorization: `Bearer ${TOKEN}` }, body: JSON.stringify({ query: text, params }) });
  const body = await r.json();
  if (!r.ok) throw new Error(`${text}: ${body.error}`);
  return body;
}

const emb = dim ? `, emb vector<${dim}> @hnsw` : '';
await query(`create collection orders (customer text @hash, status text @hash, total float, placed timestamp @sorted, owner text @hash, note text, meta json${emb})`);
const owners = ['alice', 'bob', 'carol', 'dave'];
const statuses = ['paid', 'open', 'refunded', 'shipped'];
const day = Date.UTC(2026, 0, 1);
const t0 = Date.now();
for (let from = 0; from < n; from += 5000) {
  const rows = [];
  for (let i = from; i < Math.min(n, from + 5000); i++) {
    const row = {
      customer: `customer-${String(i % 997).padStart(3, '0')}`,
      status: statuses[(i * 7) % 4],
      total: Math.round(((i * 37) % 100000) / 10) / 10,
      placed: new Date(day + i * 61_000).toISOString(),
      owner: owners[i % 4],
      note: i % 5 === 0 ? null : `order ${i}`,
      meta: { channel: i % 2 ? 'web' : 'store', items: [i % 3, i % 5] },
    };
    if (dim) row.emb = Array.from({ length: dim }, (_, k) => Math.round(Math.sin(i * 0.37 + k) * 1000) / 1000);
    rows.push(row);
  }
  const r = await fetch(`${url}/orders`, { method: 'POST', headers: { 'content-type': 'application/json', authorization: `Bearer ${TOKEN}` }, body: JSON.stringify(rows) });
  if (!r.ok) throw new Error(`seeding orders: ${await r.text()}`);
}
await query('create collection docs (title text @text, lang text @hash, code text @unique)');
for (let i = 0; i < 300; i++) await query('insert docs {title: $1, lang: $2, code: $3}', [`document ${i} about ${['deserts', 'foxes', 'vectors'][i % 3]}`, ['en', 'tr', 'de'][i % 3], `D-${i}`]);
console.error(`${n} rows${dim ? ` x ${dim}` : ''} in ${Date.now() - t0} ms`);

if (WINDOWS) child.stdin.end();
else child.kill('SIGTERM');
await new Promise((r) => child.on('exit', r));

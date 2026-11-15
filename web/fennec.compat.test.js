// A release's page, restored by this client: what that release's own web
// client kept in IndexedDB (`persist`, in the clear and sealed with a
// `cryptoKey`) and in an OPFS file (`openFile`) -- a replica's sync state
// and a migration among it -- each set in
// crates/fennec-core/tests/compat/<release>/ (tools/compat_fixtures.py,
// FENNEC_COMPAT_SETS naming another directory of sets). A page's stored
// data restores in every later 1.x (docs/design/compatibility.md): each
// store answers every statement as the release's page did; takes this
// client's rows of every type, read back and found through every index
// once persisted or flushed, restored and compacted; and answers alike
// once they are taken out. Node has neither store: tools/compat/web.mjs's
// stand in, the IndexedDB one holding only the database, version and store
// the release's page made, so a client naming another finds nothing.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { needs } from './ci-prereq.mjs';
import { KeyRange, answers, fakeDir, fakeIndexedDB, heldDatabase, sealingKey, statements } from '../tools/compat/web.mjs';

const wasm = await readFile(new URL('./fennec.wasm', import.meta.url)).catch(() => null);
needs(['web/fennec.wasm', wasm, 'make wasm']);
const skip = !wasm && 'no web/fennec.wasm (make wasm)';

const SETS = process.env.FENNEC_COMPAT_SETS
  ? pathToFileURL(`${process.env.FENNEC_COMPAT_SETS.replace(/\/*$/, '')}/`)
  : new URL('../crates/fennec-core/tests/compat/', import.meta.url);
const dirs = (await readdir(SETS, { withFileTypes: true })).filter((d) => d.isDirectory()).map((d) => d.name).sort();
/**
 * The sets, each `v<version>/` with its README and sums, at least one: as
 * the Rust tests' `sets`, an empty directory or one set itself (its
 * `archive/` no set) fails rather than pass over nothing.
 */
async function everySet() {
  assert.ok(dirs.length > 0, `no fixture set under ${SETS.pathname}`);
  for (const d of dirs) {
    assert.match(d, /^v\d+\.\d+\.\d+$/, `${SETS.pathname}: ${d} is no set: FENNEC_COMPAT_SETS names the directory holding the sets`);
    for (const must of ['README.md', 'SHA256SUMS']) await readFile(new URL(`${d}/${must}`, SETS));
  }
  return dirs;
}
const { Fennec, persist, restore, openFile } = await import('./fennec.js');

async function page() {
  const collation = (name) => readFile(new URL(`./collate/${name}.bin`, import.meta.url));
  const db = await Fennec.open(wasm, { collation });
  await db.collation('all');
  return db;
}

const read = (set, file) => readFile(new URL(`${set}/${file}`, SETS), 'utf8');

/** Each statement whose answer is not the release's page's, with both. */
async function differences(db, set, name, when) {
  const reads = statements(await read(set, `${name}.fennecql`));
  // As the Rust tests' `MIN_STATEMENTS`: a list cut short holds the store to nothing.
  assert.ok(reads.length >= 4, `${set}/${name}.fennecql: ${reads.length} statements`);
  const theirs = (await read(set, `${name}.web`)).split('\n').filter(Boolean);
  const ours = answers(db, reads).split('\n').filter(Boolean);
  assert.equal(ours.length, theirs.length, `${set}/${name}: a statement an answer`);
  return reads.flatMap((q, i) => (ours[i] === theirs[i] ? [] : [`${set}/${name} (${when}): ${q}\n  release: ${theirs[i]}\n  this client: ${ours[i]}`]));
}

const ids = (db, c) => db.run(`get ${c} select id order id`).rows.map((r) => r.id);
/** The collections a statement writes: `_` are the sync's and the server's own. */
const writable = (db) => db.run('collections').collections.map((c) => c.name).filter((n) => !n.startsWith('_'));

/**
 * A value of `type` (as `describe` names it) in FennecQL, the `k`th row's
 * of the field at `seed`: none a release's row holds, a vector of its own
 * direction with its spike at a place of its own, its own nearest under
 * every metric. As the Rust tests' `literal` (tests/compat/answers.rs).
 */
function literal(type, k, seed) {
  const u = k * 100 + seed;
  let m;
  if (type === 'bool') return String(k === 0);
  if (type === 'int') return String(7000000 + u);
  if (type === 'float') return `${7000 + u}.5`;
  if (type === 'text') return `"quokka${u}x after"`;
  if (type === 'bytes') return `[1, 2, ${u % 256}]`;
  if (type === 'timestamp') return `"2098-0${k + 1}-${String(1 + (seed % 28)).padStart(2, '0')}T00:00:00Z"`;
  if (type === 'json') return `{kind: "after${u}", n: ${u}, deep: {x: [1, "two", null]}}`;
  if (type === 'geo') return `[${-100 + 10 * k + seed * 0.01}, -45.25]`;
  if ((m = /^vector<(\d+)/.exec(type))) {
    const dim = Number(m[1]);
    return `[${Array.from({ length: dim }, (_, i) => ((u * 7 + i * 3 + 1) % 15) / 8 - 0.875 + (i === (k + seed) % dim ? 50 : 0)).join(', ')}]`;
  }
  if ((m = /^sparse<(\d+)>$/.exec(type))) {
    const n = Number(m[1]);
    return `"{${[n, n - 1][k % 2]}:9.5}/${n}"`;
  }
  if ((m = /^\[(.+)\]$/.exec(type))) return `[${literal(m[1], k, seed)}, ${literal(m[1], k, seed + 50)}]`;
  throw new Error(`a type this test writes no value of: ${type}`);
}

/** What finds the `k`th row through field `f`'s indexes, by their own operators, and whether it must come alone. */
function probes(c, f, lit, k, seed) {
  const u = k * 100 + seed;
  const out = [];
  const text = () => out.push([`get ${c} match ${f.name} "quokka${u}x" select id`, false]);
  const ix = f.index ?? 'none';
  if (/^(hash|unique|sorted|ttl)/.test(ix)) out.push([`get ${c} where ${f.name} = ${lit} select id`, false]);
  else if (/^text/.test(ix)) text();
  else if (/^(hnsw|geo|inverted)/.test(ix)) out.push([`get ${c} near ${f.name} ${lit} limit 1 select id`, true]);
  if (f.also) text();
  return out;
}

/**
 * This client's rows written into the release's database: two into every
 * collection of its schemas, a value in every field, and what finds each
 * through every index -- a path into a json field and an index over
 * several fields among them -- with the rows a statement bound to a write
 * wrote; read back as written.
 */
function writeAfter(db) {
  const names = writable(db);
  const before = new Map(names.map((n) => [n, ids(db, n)]));
  db.run('create collection compat_after (n int @hash, s text @text)');
  db.run('put compat_after [{n: 1, s: "one"}, {n: 2, s: "two"}]');
  db.run('drop collection compat_after');
  const probed = [];
  for (const c of names) {
    const [schema] = db.run(`describe ${c}`).collections;
    for (const k of [0, 1]) {
      const lits = new Map(schema.fields.map((f, seed) => [f.name, [literal(f.type, k, seed), seed]]));
      const was = ids(db, c);
      db.run(`put ${c} {${[...lits].map(([n, [lit]]) => `${n}: ${lit}`).join(', ')}}`);
      const made = ids(db, c).filter((id) => !was.includes(id));
      assert.equal(made.length, 1, `${c}: one row put, ${made} new`);
      for (const f of schema.fields) {
        const [lit, seed] = lits.get(f.name);
        for (const [q, alone] of probes(c, f, lit, k, seed)) probed.push([q, made[0], alone]);
      }
      for (const p of schema.paths ?? []) {
        const [field, key] = p.name.split('.');
        const seed = lits.get(field)?.[1];
        const value = { kind: `"after${k * 100 + seed}"`, n: String(k * 100 + seed) }[key];
        if (seed != null && value) probed.push([`get ${c} where ${p.name} = ${value} select id`, made[0], false]);
      }
      for (const ix of schema.indexes ?? []) {
        const parts = /^\w+\((.*)\)$/.exec(ix)?.[1].split(',').map((s) => s.trim().split(' ')[0]);
        if (parts?.every((p) => lits.has(p))) {
          probed.push([`get ${c} where ${parts.map((p) => `${p} = ${lits.get(p)[0]}`).join(' and ')} select id`, made[0], false]);
        }
      }
    }
  }
  const rows = new Map();
  for (const c of names) for (const id of ids(db, c)) if (!before.get(c).includes(id)) rows.set(`${c} ${id}`, JSON.stringify(db.run(`get ${c} where id = ${id}`)));
  return { rows, probed, before };
}

/** Each row of `w` as written, and found through every index: what differs. */
function readBack(db, w, name, when) {
  const found = [];
  for (const [key, was] of w.rows) {
    const [c, id] = key.split(' ');
    const now = JSON.stringify(db.run(`get ${c} where id = ${id}`));
    if (now !== was) found.push(`${name} (${when}): ${c} row ${id}\n  written: ${was}\n  read: ${now}`);
  }
  for (const [q, id, alone] of w.probed) {
    let got;
    try {
      got = db.run(q).rows.map((r) => r.id);
    } catch (e) {
      found.push(`${name} (${when}): ${q}: ${e.message}`);
      continue;
    }
    if (alone ? got.length !== 1 || got[0] !== id : !got.includes(id)) found.push(`${name} (${when}): ${q} finds ${JSON.stringify(got)}, not row ${id}`);
  }
  return found;
}

/** The rows of `w` taken out, so the release's answers stand. */
function takeOut(db, w) {
  for (const key of w.rows.keys()) {
    const [c, id] = key.split(' ');
    db.run(`del ${c} where id = ${id}`);
  }
  for (const [c, was] of w.before) assert.deepEqual(ids(db, c), was, `${c}: the rows it held`);
}

test('a release\'s IndexedDB image and chunks restore and answer as its page did', { skip }, async () => {
  const found = [];
  for (const set of await everySet()) {
    const stores = (await readdir(new URL(`${set}/`, SETS))).filter((f) => /^page-idb.*\.json$/.test(f));
    assert.ok(stores.length > 0, `${set}: no IndexedDB store`);
    for (const file of stores) {
      const name = file.replace(/\.json$/, '');
      const held = heldDatabase(JSON.parse(await read(set, file)));
      const records = held.records;
      assert.ok(records.has(name) && [...records.keys()].some((k) => k.startsWith(`${name}#`)), `${set}/${name}: an image and its chunks`);
      globalThis.indexedDB = fakeIndexedDB(held);
      globalThis.IDBKeyRange = KeyRange;
      const opts = name.endsWith('-sealed') ? { cryptoKey: await sealingKey() } : {};
      if (opts.cryptoKey) {
        await assert.rejects(restore(await page(), name), /sealed/, `${set}/${name}: restored with no key`);
      }
      const restored = async (when) => {
        const db = await page();
        assert.equal(await restore(db, name, opts), true, `${set}/${name}: restored (${when})`);
        return db;
      };
      const db = await restored('as the release left it');
      found.push(...(await differences(db, set, name, 'restored')));
      // Its writes go on as chunks after the release's, then an image.
      const w = writeAfter(db);
      found.push(...readBack(db, w, name, 'written'));
      await persist(db, name, opts);
      const again = await restored('written');
      found.push(...readBack(again, w, name, 'written, persisted, restored'));
      again.run('compact');
      await persist(again, name, opts);
      assert.ok(![...records.keys()].some((k) => k.startsWith(`${name}#`)), `${set}/${name}: the compact's image took the chunks' place`);
      const third = await restored('compacted');
      found.push(...readBack(third, w, name, 'compacted, persisted, restored'));
      takeOut(third, w);
      found.push(...(await differences(third, set, name, 'written and taken out')));
      third.run('compact');
      await persist(third, name, opts);
      const fourth = await restored('taken out');
      found.push(...(await differences(fourth, set, name, 'taken out, compacted, persisted, restored')));
      for (const d of [db, again, third, fourth]) d.close();
    }
  }
  assert.deepEqual(found, [], `${found.length} answers differ:\n${found.join('\n')}`);
});

test('a release\'s IndexedDB records are found only where its page put them', { skip }, async () => {
  for (const set of await everySet()) {
    const held = heldDatabase(JSON.parse(await read(set, 'page-idb.json')));
    // A client naming another database, version or store finds nothing
    // here, as a browser holding the release's records would give it none.
    const idb = fakeIndexedDB(held);
    const open = (name, version) =>
      new Promise((res, rej) => {
        const req = idb.open(name, version);
        req.onupgradeneeded = () => req.result.createObjectStore(held.store);
        req.onsuccess = () => res(req.result);
        req.onerror = () => rej(req.error);
      });
    await assert.rejects(open(`${held.name}-renamed`, held.version), /no database/);
    await assert.rejects(open(held.name, held.version + 1), /version/);
    const db = await open(held.name, held.version);
    assert.throws(() => db.transaction(`${held.store}-renamed`, 'readonly'), /no store/);
    assert.ok(db.objectStoreNames.contains(held.store), `${set}: the release's store`);
  }
});

test('a release\'s OPFS file opens and answers as its page did', { skip }, async () => {
  const found = [];
  for (const set of await everySet()) {
    const bytes = new Uint8Array(await readFile(new URL(`${set}/page-opfs.fennec`, SETS)));
    const dir = fakeDir(new Map([['page.fennec', { bytes, open: false }]]));
    const opened = async () => {
      const db = await page();
      return [db, await openFile(db, 'page.fennec', { dir })];
    };
    const close = ([db, file]) => {
      file.close();
      db.close();
    };
    let o = await opened();
    found.push(...(await differences(o[0], set, 'page-opfs', 'opened')));
    const w = writeAfter(o[0]);
    found.push(...readBack(o[0], w, 'page-opfs', 'written'));
    close(o);
    o = await opened();
    found.push(...readBack(o[0], w, 'page-opfs', 'written, opened again'));
    o[0].run('compact');
    close(o);
    o = await opened();
    found.push(...readBack(o[0], w, 'page-opfs', 'compacted, opened again'));
    takeOut(o[0], w);
    found.push(...(await differences(o[0], set, 'page-opfs', 'written and taken out')));
    o[0].run('compact');
    close(o);
    o = await opened();
    found.push(...(await differences(o[0], set, 'page-opfs', 'taken out, compacted, opened again')));
    close(o);
  }
  assert.deepEqual(found, [], `${found.length} answers differ:\n${found.join('\n')}`);
});

test('a release\'s OPFS file and the copy a stopped rewrite left beside it open as its page did', { skip }, async () => {
  const found = [];
  for (const set of await everySet()) {
    const file = new Uint8Array(await readFile(new URL(`${set}/page-opfs-stopped.fennec`, SETS)));
    const copy = new Uint8Array(await readFile(new URL(`${set}/page-opfs-stopped.fennec~`, SETS)));
    assert.ok(copy.length > 0, `${set}: an empty copy`);
    // The two as the release's page left them: its next open takes the
    // copy, the compact's image, over the file and empties it.
    const dir = fakeDir(new Map([
      ['page.fennec', { bytes: file.slice(), open: false }],
      ['page.fennec~', { bytes: copy.slice(), open: false }],
    ]));
    let db = await page();
    let kept = await openFile(db, 'page.fennec', { dir });
    assert.equal(dir.files.get('page.fennec~').bytes.length, 0, `${set}: the copy left beside the file`);
    assert.deepEqual(dir.files.get('page.fennec').bytes, copy, `${set}: the file is not the copy`);
    found.push(...(await differences(db, set, 'page-opfs-stopped', 'opened')));
    kept.close();
    db.close();
    db = await page();
    kept = await openFile(db, 'page.fennec', { dir });
    found.push(...(await differences(db, set, 'page-opfs-stopped', 'opened again')));
    kept.close();
    db.close();
  }
  assert.deepEqual(found, [], `${found.length} answers differ:\n${found.join('\n')}`);
});

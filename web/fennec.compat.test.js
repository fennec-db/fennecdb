// A release's page, restored by this client: what that release's own web
// client kept in IndexedDB (`persist`, in the clear and sealed with a
// `cryptoKey`) and in an OPFS file (`openFile`), each set in
// crates/fennec-core/tests/compat/<release>/ (tools/compat_fixtures.py). A
// page's stored data restores in every later 1.x
// (docs/design/compatibility.md): each store answers every statement as
// the release's page did, then takes this client's writes, persists or
// flushes them, compacts, and answers alike opened again. Node has neither
// store: tools/compat/web.mjs's stand in, handed the records as the release
// left them.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import { needs } from './ci-prereq.mjs';
import { KeyRange, answers, fakeDir, fakeIndexedDB, loadRecords, sealingKey, statements } from '../tools/compat/web.mjs';

const wasm = await readFile(new URL('./fennec.wasm', import.meta.url)).catch(() => null);
needs(['web/fennec.wasm', wasm, 'make wasm']);
const skip = !wasm && 'no web/fennec.wasm (make wasm)';

const SETS = new URL('../crates/fennec-core/tests/compat/', import.meta.url);
const sets = (await readdir(SETS, { withFileTypes: true })).filter((d) => d.isDirectory()).map((d) => d.name).sort();
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
  const theirs = (await read(set, `${name}.web`)).split('\n').filter(Boolean);
  const ours = answers(db, reads).split('\n').filter(Boolean);
  assert.equal(ours.length, theirs.length, `${set}/${name}: a statement an answer`);
  return reads.flatMap((q, i) => (ours[i] === theirs[i] ? [] : [`${set}/${name} (${when}): ${q}\n  release: ${theirs[i]}\n  this client: ${ours[i]}`]));
}

/**
 * This client's writes into the release's database: a collection of its own
 * written, then dropped, and a row put into every collection and taken out
 * again, so the release's answers stand after them.
 */
function writeAfter(db) {
  const names = db.run('collections').collections.map((c) => c.name).filter((n) => !n.startsWith('_'));
  const ids = (n) => db.run(`get ${n} select id order id`).rows.map((r) => r.id);
  const before = new Map(names.map((n) => [n, ids(n)]));
  db.run('create collection compat_after (n int @hash, s text @text)');
  db.run('put compat_after [{n: 1, s: "one"}, {n: 2, s: "two"}]');
  db.run('set compat_after {s: "uno"} where n = 1');
  db.run('del compat_after where n = 2');
  for (const n of names) db.run(`put ${n} {id: 1000000}`);
  for (const n of names) {
    for (const id of ids(n)) if (!before.get(n).includes(id)) db.run(`del ${n} where id = ${id}`);
    assert.deepEqual(ids(n), before.get(n), `${n}: the rows it held`);
  }
  db.run('drop collection compat_after');
}

test('a release\'s IndexedDB image and chunks restore and answer as its page did', { skip }, async () => {
  const found = [];
  for (const set of sets) {
    const stores = (await readdir(new URL(`${set}/`, SETS))).filter((f) => /^page-idb.*\.json$/.test(f));
    assert.ok(stores.length > 0, `${set}: no IndexedDB store`);
    for (const file of stores) {
      const name = file.replace(/\.json$/, '');
      const records = loadRecords(JSON.parse(await read(set, file)).records);
      assert.ok(records.has(name) && [...records.keys()].some((k) => k.startsWith(`${name}#`)), `${set}/${name}: an image and its chunks`);
      globalThis.indexedDB = fakeIndexedDB(records);
      globalThis.IDBKeyRange = KeyRange;
      const opts = name.endsWith('-sealed') ? { cryptoKey: await sealingKey() } : {};
      if (opts.cryptoKey) {
        await assert.rejects(restore(await page(), name), /sealed/, `${set}/${name}: restored with no key`);
      }
      const db = await page();
      assert.equal(await restore(db, name, opts), true, `${set}/${name}: restored`);
      found.push(...(await differences(db, set, name, 'restored')));
      // Its writes go on as chunks after the release's, then an image.
      writeAfter(db);
      await persist(db, name, opts);
      const again = await page();
      await restore(again, name, opts);
      found.push(...(await differences(again, set, name, 'written, persisted, restored')));
      again.run('compact');
      await persist(again, name, opts);
      assert.ok(![...records.keys()].some((k) => k.startsWith(`${name}#`)), `${set}/${name}: the compact's image took the chunks' place`);
      const third = await page();
      await restore(third, name, opts);
      found.push(...(await differences(third, set, name, 'compacted, persisted, restored')));
      for (const d of [db, again, third]) d.close();
    }
  }
  assert.deepEqual(found, [], `${found.length} answers differ:\n${found.join('\n')}`);
});

test('a release\'s OPFS file opens and answers as its page did', { skip }, async () => {
  const found = [];
  for (const set of sets) {
    const bytes = new Uint8Array(await readFile(new URL(`${set}/page-opfs.fennec`, SETS)));
    const dir = fakeDir(new Map([['page.fennec', { bytes, open: false }]]));
    const db = await page();
    const file = await openFile(db, 'page.fennec', { dir });
    found.push(...(await differences(db, set, 'page-opfs', 'opened')));
    writeAfter(db);
    db.run('compact');
    file.close();
    db.close();
    const again = await page();
    const reopened = await openFile(again, 'page.fennec', { dir });
    found.push(...(await differences(again, set, 'page-opfs', 'written, compacted, opened again')));
    reopened.close();
    again.close();
  }
  assert.deepEqual(found, [], `${found.length} answers differ:\n${found.join('\n')}`);
});

// @fennecdb/cloudflare's part of a release's fixture set
// (tools/compat_fixtures.py): what that release's module -- its
// integrations/cloudflare/index.js, over the release's web client and the
// module its own `make wasm` built -- keeps in a Durable Object's storage,
// a SQLite-backed object's table of rows (`<key>:<gen>`: the image's rows,
// then a row a flush of the writes after it) and a key-value backed
// object's values (the image's and the writes' pieces, and the record
// naming the generation), and the answers the release gave to a list of
// statements once it restored each.
//
//   node tools/compat/cloudflare.mjs <the release's web dir> <its cloudflare dir> <out dir> <statements>
//
// The storage is the release's own stand-ins for it (`fakes.js`, which its
// tests ran the module over): `Rows` over node:sqlite, `Values` a map.
// web/fennec.compat.test.js loads what is dumped here into this tree's
// stand-ins and restores it with this tree's module.

import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { WRITES, answers, page, statements } from './web.mjs';

/**
 * The most bytes a row or a value holds, lowered from the module's 2 MB and
 * 128 KiB through `persist`'s own option, so the image takes several rows
 * and values, as a database of megabytes does.
 */
export const PIECE = 2048;

const b64 = (bytes) => Buffer.from(bytes).toString('base64');

/** A SQLite-backed object's tables as JSON: each table's SQL and its rows, `data` in base64. */
export function dumpRows(storage) {
  return {
    tables: storage.tables().map((name) => ({
      name,
      sql: storage.db.prepare('SELECT sql FROM sqlite_master WHERE type = ? AND name = ?').get('table', name).sql,
      rows: storage.db
        .prepare(`SELECT id, image, data FROM "${name.replaceAll('"', '""')}" ORDER BY id`)
        .all()
        .map((r) => ({ id: r.id, image: r.image, data: b64(r.data) })),
    })),
  };
}

/** A key-value backed object's values as JSON: bytes in base64, the record as it is. */
export function dumpValues(storage) {
  return {
    values: [...storage.map.keys()].sort().map((key) => {
      const v = storage.map.get(key);
      return v instanceof Uint8Array ? { key, bytes: b64(v) } : { key, value: v };
    }),
  };
}

const unb64 = (s) => new Uint8Array(Buffer.from(s, 'base64'));

/** What [`dumpRows`] wrote, made again in `storage` (a `Rows`): each table by its own SQL, its rows as they were. */
export function loadRows(storage, json) {
  for (const t of json.tables) {
    storage.db.exec(t.sql);
    const put = storage.db.prepare(`INSERT INTO "${t.name.replaceAll('"', '""')}" (id, image, data) VALUES (?, ?, ?)`);
    for (const r of t.rows) put.run(r.id, r.image, unb64(r.data));
  }
  return storage;
}

/** What [`dumpValues`] wrote, put back into `storage` (a `Values`) as it held it. */
export function loadValues(storage, json) {
  for (const v of json.values) storage.map.set(v.key, v.bytes != null ? unb64(v.bytes) : structuredClone(v.value));
  return storage;
}

async function main([web, cloudflare, out, statementsFile]) {
  const wasm = await readFile(join(web, 'fennec.wasm'));
  const reads = statements(await readFile(statementsFile, 'utf8'));
  const { persist, restore } = await import(pathToFileURL(join(cloudflare, 'index.js')).href);
  const { Rows, Values } = await import(pathToFileURL(join(cloudflare, 'fakes.js')).href);
  for (const [name, storage, dump] of [
    ['do-sql', new Rows((await import('node:sqlite')).DatabaseSync), dumpRows],
    ['do-kv', new Values(), dumpValues],
  ]) {
    // The image at the first persist, then the writes since a flush each.
    const db = await page(web, wasm);
    for (const part of WRITES) {
      for (const q of part) db.run(q);
      await persist(db, storage, { piece: PIECE });
    }
    db.close();
    const dumped = dump(storage);
    await writeFile(join(out, `${name}.json`), JSON.stringify(dumped, null, 1) + '\n');
    const back = await page(web, wasm);
    if (!(await restore(back, storage))) throw new Error(`${name}: nothing restored`);
    await writeFile(join(out, `${name}.web`), answers(back, reads));
    back.close();
  }
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main(process.argv.slice(2));
}

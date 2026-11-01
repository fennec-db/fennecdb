// The store against a database in the page (a Fennec over web/fennec.wasm)
// and against fennec-server's HTTP endpoint (a FennecHttp): `npm test` here, after
// `npm ci`. What LangChain.js's own integrations are held to -- documents
// in and found again, their ids, scores, deletions, filters, a retriever --
// and BM25 and hybrid search. Needs web/fennec.wasm (make wasm); the HTTP
// half the fennec-server binary (cargo build -p fennec-server), and skips without it.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { access, mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Document } from '@langchain/core/documents';
import { SyntheticEmbeddings } from '@langchain/core/utils/testing';
import { Fennec, FennecHttp } from '../../web/fennec.js';
import { FennecVectorStore } from './index.js';

const root = new URL('../../', import.meta.url);
const wasm = await readFile(new URL('web/fennec.wasm', root)).catch(() => null);
const binary = new URL('target/debug/fennec-server', root).pathname;
const hasBinary = await access(binary).then(() => true, () => false);
const embeddings = new SyntheticEmbeddings({ vectorSize: 16 });

/**
 * Where a fennec-server started on port 0 listens, off its `listening on:
 * http://<address>` line, read to the space after it so a line that came
 * in two pieces is not read short. A port found free and handed over was
 * free only until someone else took it, and a server of someone else's
 * could answer in its place.
 */
const listening = (child) =>
  new Promise((res, rej) => {
    let log = '';
    child.stderr.on('data', (d) => {
      log += d;
      const at = /listening on: (http:\/\/\S+)\s/.exec(log);
      if (at) res(at[1]);
    });
    child.on('exit', () => rej(new Error(`fennec-server ended before it listened: ${log}`)));
  });

/** fennec-server over a file of its own, on a port the system picks; `stop()` ends it. */
async function server() {
  const dir = await mkdtemp(join(tmpdir(), 'fennecdb-langchain-'));
  const child = spawn(binary, ['--http', '127.0.0.1:0', '--file', join(dir, 'db.fennec')], {
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  const url = await listening(child);
  return {
    client: new FennecHttp(url),
    async stop() {
      // Waited out before the folder goes: the server writes into it as it
      // stops (see integrations/mcp/test.js).
      const exited = new Promise((res) =>
        child.exitCode !== null || child.signalCode !== null ? res() : child.once('exit', res),
      );
      child.kill();
      await exited;
      await rm(dir, { recursive: true, force: true });
    },
  };
}

const docs = [
  new Document({ pageContent: 'fennecdb compacts a file beside the database', metadata: { source: 'handbook', page: 1 } }),
  new Document({ pageContent: 'a replica applies what the primary sends', metadata: { source: 'handbook', page: 2 } }),
  new Document({ pageContent: 'the browser keeps a database in IndexedDB', metadata: { source: 'blog', page: 3 } }),
  new Document({ pageContent: 'HNSW links each vector to its nearest', metadata: { source: 'paper', page: 4 } }),
];

/** Each test, once over a database in the page and once over HTTP. */
function both(name, fn) {
  test(`${name} (in the page)`, { skip: wasm ? false : 'no web/fennec.wasm (make wasm)' }, async () => {
    const db = await Fennec.open(wasm);
    try {
      await fn(db);
    } finally {
      db.close();
    }
  });
  test(`${name} (over HTTP)`, { skip: wasm && hasBinary ? false : 'no fennec-server binary (cargo build -p fennec-server)' }, async () => {
    const s = await server();
    try {
      await fn(s.client);
    } finally {
      await s.stop();
    }
  });
}

both('documents go in and are found again, their ids and scores kept', async (client) => {
  const store = new FennecVectorStore(embeddings, { client, collection: 'docs' });
  assert.deepEqual(await store.similaritySearch('anything', 2), [], 'a store nothing was written to is empty');
  const ids = await store.addDocuments(docs, { ids: ['a', 'b', 'c', 'd'] });
  assert.deepEqual(ids, ['a', 'b', 'c', 'd']);
  const [[best, score]] = await store.similaritySearchWithScore(docs[1].pageContent, 1);
  assert.equal(best.id, 'b');
  assert.equal(best.pageContent, docs[1].pageContent);
  assert.deepEqual(best.metadata, { source: 'handbook', page: 2 });
  assert.ok(Math.abs(score - 1) < 1e-5, `the same text scores 1 under cosine: ${score}`);
  assert.equal((await store.similaritySearch('replica', 3)).length, 3);

  // Written again under the same id: replaced, not added.
  await store.addDocuments([new Document({ pageContent: 'a replica follows its primary', metadata: { page: 9 } })], { ids: ['b'] });
  const again = await store.getByIds(['b', 'nope', 'a']);
  assert.deepEqual(again.map((d) => [d.id, d.metadata.page]), [['b', 9], ['a', 1]]);
  assert.equal((await store.similaritySearch('x', 10)).length, 4);

  // Ids made up for documents that came without.
  const made = await store.addDocuments([new Document({ pageContent: 'no id here' })]);
  assert.match(made[0], /^[0-9a-f-]{36}$/);

  await store.delete({ ids: ['a', made[0]] });
  assert.deepEqual((await store.similaritySearch('x', 10)).map((d) => d.id).sort(), ['b', 'c', 'd']);
  await store.delete({ deleteAll: true });
  assert.deepEqual(await store.similaritySearch('x', 10), []);
});

both('a filter over the metadata fields is answered by their index', async (client) => {
  const store = await FennecVectorStore.fromDocuments(docs, embeddings, {
    client,
    collection: 'filtered',
    metadataFields: { source: 'text', page: 'int' },
  });
  const sources = async (filter) =>
    (await store.similaritySearch('database', 10, filter)).map((d) => d.metadata.source).sort();
  assert.deepEqual(await sources({ source: 'handbook' }), ['handbook', 'handbook']);
  assert.deepEqual(await sources({ source: { $in: ['blog', 'paper'] } }), ['blog', 'paper']);
  assert.deepEqual(await sources({ source: ['blog'], page: { $eq: 3 } }), ['blog']);
  await assert.rejects(store.similaritySearch('x', 1, { author: 'me' }), /not in metadataFields/);
  await assert.rejects(store.similaritySearch('x', 1, { page: { $gt: 1 } }), /only \$eq and \$in/);
});

both('fromTexts, and a retriever over the store', async (client) => {
  const store = await FennecVectorStore.fromTexts(
    docs.map((d) => d.pageContent),
    docs.map((d) => d.metadata),
    embeddings,
    { client, collection: 'texts' },
  );
  const found = await store.asRetriever({ k: 2 }).invoke(docs[3].pageContent);
  assert.equal(found.length, 2);
  assert.equal(found[0].pageContent, docs[3].pageContent);
});

both('with fullText, by the words and by both', async (client) => {
  const store = await FennecVectorStore.fromDocuments(docs, embeddings, { client, collection: 'words', fullText: true });
  const [[first]] = await store.search('replica primary', 1, { mode: 'text' });
  assert.equal(first.pageContent, docs[1].pageContent);
  const hybrid = await store.search('browser IndexedDB', 2, { mode: 'hybrid' });
  assert.equal(hybrid[0][0].pageContent, docs[2].pageContent);
  const plain = new FennecVectorStore(embeddings, { client, collection: 'words' });
  await assert.rejects(plain.search('x', 1, { mode: 'text' }), /fullText/);
});

test('names and options that are not a store refuse to make one', () => {
  const client = { run() {} };
  assert.throws(() => new FennecVectorStore(embeddings, { collection: 'x' }), /client/);
  assert.throws(() => new FennecVectorStore(embeddings, { client, collection: 'no-dash' }), /collection name/);
  assert.throws(() => new FennecVectorStore(embeddings, { client, metric: 'hamming' }), /metric/);
  assert.throws(() => new FennecVectorStore(embeddings, { client, quant: 'bit', metric: 'l2' }), /cosine/);
  assert.throws(() => new FennecVectorStore(embeddings, { client, metadataFields: { content: 'text' } }), /metadata field/);
  assert.throws(() => new FennecVectorStore(embeddings, { client, metadataFields: { page: 'vector' } }), /a field is one of/);
});

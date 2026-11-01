// `FennecHttp` against a `fennec-server` serving HTTPS itself
// (`--tls-cert`, `--tls-key`), as an SDK meets it: a Node process trusting
// the server's private CA through `NODE_EXTRA_CA_CERTS` -- read once, as
// Node starts, so the client runs in a process of its own -- reaches it,
// and one trusting only the system's roots is refused before it sends a
// request.
//
// Skipped when the `fennec-server` binary (cargo build) is missing.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, execFile } from 'node:child_process';
import { access } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

async function binary(name) {
  for (const p of [`../target/debug/${name}`, `../target/release/${name}`]) {
    const url = new URL(p, import.meta.url);
    try {
      await access(url);
      return url.pathname;
    } catch {
      /* not there */
    }
  }
  return null;
}
const bin = await binary('fennec-server');
const certs = fileURLToPath(new URL('../crates/fennec-http/tests/certs/', import.meta.url));
const client = fileURLToPath(new URL('./client.js', import.meta.url));

/** A server serving HTTPS, and the `https://` URL its line names. */
async function serving() {
  const proc = spawn(
    bin,
    ['--http', '127.0.0.1:0', '--tls-cert', `${certs}server.pem`, '--tls-key', `${certs}server.key`],
    { stdio: ['ignore', 'ignore', 'pipe'] },
  );
  const url = await new Promise((res, rej) => {
    let buf = '';
    // A bound only on a server that never starts, which is killed then.
    const timer = setTimeout(() => {
      proc.kill('SIGKILL');
      rej(new Error(`fennec-server did not start: ${buf}`));
    }, 30000);
    proc.stderr.on('data', (d) => {
      buf += d;
      const m = buf.match(/listening on: (https:\/\/\S+)/);
      if (m) {
        clearTimeout(timer);
        res(m[1]);
      }
    });
    proc.on('exit', (c) => {
      clearTimeout(timer);
      rej(new Error(`fennec-server exited with ${c}: ${buf}`));
    });
  });
  return { proc, url };
}

/** Node running `connect(url)` and three statements, with `env` added. */
function asked(url, env) {
  const script = `
    import { connect } from ${JSON.stringify(client)};
    const db = connect(${JSON.stringify(url)});
    try {
      await db.run('create collection notes (t text)');
      await db.run('put notes {id: 1, t: $1}', ['over tls']);
      const r = await db.run('get notes select t where id = 1');
      console.log(JSON.stringify({ rows: r.rows ?? r }));
    } catch (e) {
      console.log(JSON.stringify({ error: String(e?.cause?.code ?? e?.message ?? e) }));
    }
  `;
  // The caller's environment, less any CA it trusts already.
  const base = { ...process.env };
  delete base.NODE_EXTRA_CA_CERTS;
  return new Promise((res, rej) => {
    execFile(
      process.execPath,
      ['--input-type=module', '-e', script],
      { env: { ...base, ...env }, timeout: 60000 },
      (err, stdout, stderr) => (err ? rej(new Error(`${err}: ${stderr}`)) : res(JSON.parse(stdout))),
    );
  });
}

test('FennecHttp reaches an HTTPS server whose CA it is given', { skip: !bin && 'no fennec-server binary (cargo build)' }, async () => {
  const { proc, url } = await serving();
  try {
    assert.match(url, /^https:\/\/127\.0\.0\.1:\d+$/);
    const trusted = await asked(url, { NODE_EXTRA_CA_CERTS: `${certs}ca.pem` });
    assert.deepEqual(trusted, { rows: [{ t: 'over tls' }] });
    const untrusted = await asked(url, {});
    assert.ok(untrusted.error, JSON.stringify(untrusted));
    assert.match(untrusted.error, /CERT|certificate|SELF_SIGNED|UNABLE_TO/i);
  } finally {
    proc.kill('SIGKILL');
  }
});

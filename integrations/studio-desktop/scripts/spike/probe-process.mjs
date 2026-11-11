#!/usr/bin/env node
// SPIKE (desktop-spike.yml): how a fennec-server the app starts is stopped, on
// the host this runs on, under Node (the unit tests' runtime) and under the
// Bun the app bundles (its main process). Prints one JSON line a case and
// exits 0 whatever it finds: the findings are the output.
//
//   node scripts/spike/probe-process.mjs <fennec-server>
//   <bundled bun> scripts/spike/probe-process.mjs <fennec-server>
//
// Cases:
//   signal     server.ts's stop today: kill("SIGTERM"). On Windows libuv (and
//              Bun) emulate SIGTERM with TerminateProcess: no shutdown runs.
//   stdin-end  the pipe on its stdin ended by the app (`--exit-on-stdin-close`):
//              the one shutdown, on every system.
//   parent-killed  a process holding the pipe is killed hard (TerminateProcess
//              on Windows, SIGKILL elsewhere): the server must end by itself and
//              let the file go (a second server opens it).
//   env-allowlist  the server started with env.ts's allowlist as it reads the
//              environment here, and with Windows' SystemRoot added.
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { childEnvironment } from '../../src/bun/env.ts';

const runtime = globalThis.Bun ? `bun ${Bun.version}` : `node ${process.version}`;
const SERVER = process.argv[2];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** A server through the runtime's own spawn, as the app (Bun.spawn) or the unit tests (node:child_process) start one. */
function start(file, env = { ...process.env, FENNEC_HTTP_TOKEN: 'spike-token' }, extra = []) {
  const argv = [SERVER, '--file', file, '--http', '127.0.0.1:0', '--sync', 'always', '--exit-on-stdin-close', ...extra];
  let err = '';
  let p;
  let exited;
  if (globalThis.Bun) {
    p = Bun.spawn(argv, { env, stdin: 'pipe', stdout: 'ignore', stderr: 'pipe' });
    exited = p.exited.then((code) => ({ code, signal: p.signalCode ?? null }));
    (async () => {
      const d = new TextDecoder();
      for await (const c of p.stderr) err += d.decode(c, { stream: true });
    })();
  } else {
    p = spawn(argv[0], argv.slice(1), { env, stdio: ['pipe', 'ignore', 'pipe'] });
    p.stderr.on('data', (b) => (err += b));
    exited = new Promise((r) => p.on('exit', (code, signal) => r({ code, signal })));
  }
  const listening = Promise.race([
    (async () => {
      for (let i = 0; i < 300; i++) {
        const m = /listening on: (http:\/\/\S+)/.exec(err);
        if (m) return m[1];
        await sleep(50);
      }
      return null;
    })(),
    exited.then(() => null),
  ]);
  return {
    pid: p.pid,
    listening,
    exited,
    err: () => err,
    kill: (sig) => p.kill(sig),
    endStdin: () => (globalThis.Bun ? p.stdin.end() : p.stdin.end()),
  };
}

const said = (err) => ({
  stdinClosed: /stdin closed: shutting down/.test(err),
  shuttingDown: /shutting down/.test(err),
  checkpoint: /checkpoint written|checkpoint/.test(err),
  tail: err.trim().split('\n').slice(-4),
});
const out = (o) => console.log(JSON.stringify({ runtime, platform: `${process.platform}-${process.arch}`, ...o }));
const dir = mkdtempSync(join(tmpdir(), 'fennec-probe-'));

try {
  if (process.argv[3] === '--hold') {
    // The process that holds a server's stdin and is then killed.
    const s = start(process.argv[4]);
    await s.listening;
    console.log(`server-pid ${s.pid}`);
    await new Promise(() => {});
  }

  {
    // instance.ts as macOS has it: a socket file in the user's temporary folder.
    const { createServer } = await import('node:net');
    const at = join(tmpdir(), `spike-${process.pid}.instance`);
    const r = await new Promise((resolve) => {
      const srv = createServer();
      srv.once('error', (e) => resolve({ listened: false, code: e.code, message: e.message }));
      srv.listen(at, () => {
        srv.close();
        resolve({ listened: true });
      });
    });
    out({ case: 'instance-socket-file', path: at, ...r });
  }
  {
    const s = start(join(dir, 'signal.fennec'));
    const at = await s.listening;
    const t0 = Date.now();
    s.kill('SIGTERM');
    const end = await s.exited;
    out({ case: 'signal', listening: at, exit: end, ms: Date.now() - t0, ...said(s.err()) });
  }
  {
    const s = start(join(dir, 'stdin-end.fennec'));
    const at = await s.listening;
    const t0 = Date.now();
    s.endStdin();
    const end = await Promise.race([s.exited, sleep(20000).then(() => 'still running after 20 s')]);
    if (typeof end === 'string') s.kill('SIGKILL');
    out({ case: 'stdin-end', listening: at, exit: end, ms: Date.now() - t0, ...said(s.err()) });
  }
  {
    const file = join(dir, 'parent-killed.fennec');
    const holder = spawn(process.execPath, [process.argv[1], SERVER, '--hold', file], { stdio: ['ignore', 'pipe', 'inherit'] });
    let text = '';
    holder.stdout.on('data', (b) => (text += b));
    let pid = null;
    for (let i = 0; i < 300 && !pid; i++) {
      pid = Number(/server-pid (\d+)/.exec(text)?.[1]) || null;
      await sleep(50);
    }
    const t0 = Date.now();
    holder.kill('SIGKILL');
    let gone = false;
    for (let i = 0; i < 400 && !gone; i++) {
      try {
        process.kill(pid, 0);
        await sleep(50);
      } catch {
        gone = true;
      }
    }
    const again = start(file);
    const reopened = await again.listening;
    out({ case: 'parent-killed', serverPid: pid, serverEnded: gone, endedWithinMs: gone ? Date.now() - t0 : null, fileFreeAfter: Boolean(reopened), secondServer: reopened ? 'started' : again.err().trim().split('\n').slice(-2) });
    if (reopened) again.endStdin();
    await Promise.race([again.exited, sleep(10000)]);
    if (!gone && pid) try { process.kill(pid, 'SIGKILL'); } catch {}
  }
  {
    // The server started with env.ts's allowlist as macOS has it (exact names),
    // then with the spike's (env.ts: case-blind on Windows, SystemRoot and TEMP).
    const MAC = new Set(['HOME', 'PATH', 'TMPDIR', 'LANG', 'USER', 'LOGNAME', 'TZ']);
    const envs = {
      'env-allowlist-as-on-macos': {
        ...Object.fromEntries(Object.entries(process.env).filter(([k]) => MAC.has(k) || k.startsWith('LC_'))),
        FENNEC_HTTP_TOKEN: 'spike-token',
      },
      'env-allowlist-spike': childEnvironment(process.env, { FENNEC_HTTP_TOKEN: 'spike-token' }),
    };
    for (const [name, env] of Object.entries(envs)) {
      const s = start(join(dir, `${name}.fennec`), env);
      const at = await s.listening;
      out({ case: name, keys: Object.keys(env).filter((k) => k !== 'FENNEC_HTTP_TOKEN').sort(), listening: at, tail: at ? [] : s.err().trim().split('\n').slice(-3) });
      if (at) s.endStdin();
      await Promise.race([s.exited, sleep(10000)]);
    }
  }
} finally {
  if (process.argv[3] !== '--hold') rmSync(dir, { recursive: true, force: true });
}
process.exit(0);

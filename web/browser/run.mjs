#!/usr/bin/env node
// A page's database kept in real browsers: `make browser-test`.
//
//   node web/browser/run.mjs [chrome] [firefox] [safari]
//
// web/fennec.persist.test.js and web/fennec.file.test.js hold `persist`,
// `restore` and `openFile` to what they store over in-memory stand-ins,
// Node having no IndexedDB and no origin private file system. Here the
// same calls meet each engine's own: the page (persist.html, persist.js
// and file-worker.js) runs the suite itself, loads itself again where a
// test reloads the page -- in the middle of a persist's transaction, at
// each step of a file's flush -- and reports each test to this server,
// which keeps the page's state between its loads and prints what each
// browser said.
//
// Chrome and Firefox are driven by puppeteer-core (`npm ci` here: the
// one dependency, as the studio's tests have it), Safari by safaridriver
// over plain WebDriver, nothing installed (macOS, once `sudo safaridriver
// --enable` has allowed it). Chrome's quota for the page's origin is set
// over the DevTools protocol and Firefox's by a preference, so a write
// past it is refused as a full disk refuses it; Safari has neither, and
// its quota tests are skipped by name (`SKIPS`).
//
//   CHROME_PATH   Chrome, else the newest puppeteer keeps, else the system's
//   FIREFOX_PATH  Firefox, else `firefox` on PATH, else the system's

import { spawn } from 'node:child_process';
import { existsSync, readdirSync, realpathSync } from 'node:fs';
import { open, readFile, readdir } from 'node:fs/promises';
import { createServer } from 'node:http';
import { createServer as createNetServer } from 'node:net';
import { homedir } from 'node:os';
import { dirname, extname, join, normalize, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const WEB = join(ROOT, 'web');
const PAGE = '/web/browser/persist.html';

/**
 * The quota the page's origin is held to where a browser lets it be set:
 * the page writes past it on purpose. Chrome takes it as it is; Firefox
 * gives a group of origins a fifth of its whole limit but never under 10
 * MiB (`GetGroupLimitForLimit`), so its whole limit is five times this:
 * smaller, its eviction of the origins it holds would start too.
 */
const QUOTA = 10 * 1024 * 1024;

/** A whole run's bound: the suite takes seconds; this is for a hang. */
const RUN_LIMIT_MS = 180_000;

/** What an engine cannot run, by test name, and why. Any other skip fails. */
const SKIPS = {
  safari: {
    'persist and putState past the quota are refused, what was stored standing': 'Safari has no quota a driver can set',
    'a file past the quota refuses the write and keeps every write it answered': 'Safari has no quota a driver can set',
  },
};

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.wasm': 'application/wasm',
  '.bin': 'application/octet-stream',
};

/** A promise and what settles it. */
function deferred() {
  let resolve;
  const promise = new Promise((r) => (resolve = r));
  return { promise, resolve };
}

/**
 * The page's server: web/ as the page reads it, and where the page keeps
 * its state between loads and reports what it found. On port 0, its
 * address read from the listener: a port found free and handed over is
 * free only until someone else takes it.
 */
async function serve() {
  const runs = new Map();
  const run = (id) => {
    if (!runs.has(id)) runs.set(id, { state: {}, plan: null, results: [], done: deferred(), says: () => {} });
    return runs.get(id);
  };
  const server = createServer(async (req, res) => {
    const url = new URL(req.url, 'http://x');
    // A reload must fetch the page and its modules again, never keep the
    // first run's.
    const head = { 'cache-control': 'no-store' };
    let body = '';
    for await (const chunk of req) body += chunk;
    const id = url.searchParams.get('run');
    const reply = (status, text = '', type = 'text/plain; charset=utf-8') => {
      res.writeHead(status, { ...head, 'content-type': type });
      res.end(text);
    };
    if (id && url.pathname === '/state') {
      const r = run(id);
      if (req.method === 'PUT') {
        r.state = JSON.parse(body);
        return reply(204);
      }
      return reply(200, JSON.stringify(r.state), 'application/json');
    }
    if (id && req.method === 'POST' && ['/plan', '/report', '/done', '/fail', '/log'].includes(url.pathname)) {
      const r = run(id);
      const msg = body ? JSON.parse(body) : {};
      if (url.pathname === '/plan') r.plan ??= msg.tests;
      if (url.pathname === '/report') {
        r.results.push(msg);
        r.says(msg);
      }
      if (url.pathname === '/log') r.says({ log: msg.text });
      if (url.pathname === '/done') r.done.resolve({ ok: true });
      if (url.pathname === '/fail') r.done.resolve({ ok: false, error: msg.error });
      return reply(204);
    }
    // Asked for by every page Chrome opens: nothing, rather than a 404 in
    // the page's console.
    if (url.pathname === '/favicon.ico') return reply(204);
    if (req.method !== 'GET' || !url.pathname.startsWith('/web/')) return reply(404, 'not found');
    const file = normalize(join(ROOT, decodeURIComponent(url.pathname)));
    if (!file.startsWith(WEB + sep)) return reply(404, 'not found');
    try {
      reply(200, await readFile(file), TYPES[extname(file)] ?? 'application/octet-stream');
    } catch {
      reply(404, 'not found');
    }
  });
  const port = await new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server.address().port)));
  return {
    origin: `http://127.0.0.1:${port}`,
    run,
    close: () => new Promise((r) => server.close(r)),
  };
}

/** The newest of puppeteer's cached builds of `kind` holding one of `paths`. */
function cached(kind, paths) {
  const cache = join(homedir(), '.cache/puppeteer', kind);
  if (!existsSync(cache)) return null;
  for (const build of readdirSync(cache).sort().reverse()) {
    for (const p of paths) if (existsSync(join(cache, build, p))) return join(cache, build, p);
  }
  return null;
}

/**
 * Chrome: `CHROME_PATH` (CI's own), else the newest headless shell or
 * Chrome for Testing puppeteer keeps (studio/test/harness.mjs says why the
 * shell first), else the system's.
 */
function chromePath() {
  return (
    process.env.CHROME_PATH ||
    cached('chrome-headless-shell', ['chrome-headless-shell-mac-arm64/chrome-headless-shell', 'chrome-headless-shell-mac-x64/chrome-headless-shell', 'chrome-headless-shell-linux64/chrome-headless-shell']) ||
    cached('chrome', ['chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing', 'chrome-linux64/chrome']) ||
    ['/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', '/usr/bin/google-chrome'].find(existsSync) ||
    null
  );
}

/** Firefox: `FIREFOX_PATH`, else the one puppeteer keeps, else the system's. */
function firefoxPath() {
  return (
    process.env.FIREFOX_PATH ||
    cached('firefox', ['firefox/Firefox.app/Contents/MacOS/firefox', 'firefox/firefox']) ||
    ['/Applications/Firefox.app/Contents/MacOS/firefox', '/usr/bin/firefox'].find(existsSync) ||
    null
  );
}

/**
 * A browser's own files read once before it first starts, as the studio's
 * harness reads Chrome's (`warm`, docs/design/conventions.md#browsers):
 * started cold, a browser faults its binary in a page and a readahead at a
 * time, and on a fresh runner's disk its first launch has spent 5 to 20 s
 * of the 30 puppeteer waits for it. A directory that is not the browser's
 * own -- a wrapper script's, /usr/bin's -- holds none of `mark`'s files and
 * is passed over; an `.app` is read whole, its resources being in its
 * framework.
 */
async function warm(executable, mark) {
  const real = realpathSync(executable);
  const app = /^(.*?\.app)\//.exec(real);
  const root = app ? app[1] : dirname(real);
  if (!app && !readdirSync(root).some((n) => n.endsWith(mark))) return;
  const buf = Buffer.allocUnsafe(1 << 20);
  const walk = async (dir) => {
    for (const e of await readdir(dir, { withFileTypes: true })) {
      const path = join(dir, e.name);
      if (e.isDirectory()) await walk(path);
      if (!e.isFile()) continue;
      let f;
      try {
        f = await open(path);
      } catch {
        continue; // not ours to read, and so not the browser's either
      }
      try {
        while ((await f.read(buf, 0, buf.length, null)).bytesRead > 0);
      } finally {
        await f.close();
      }
    }
  };
  await walk(root);
}

/** Chrome and Firefox, puppeteer's way; what the page logs kept for a failure. */
async function puppeteerDriver(engine, url, origin, said) {
  const { default: puppeteer } = await import('puppeteer-core').catch(() => {
    throw new Error('no puppeteer-core: npm ci in web/browser');
  });
  let browser;
  if (engine === 'chrome') {
    const executablePath = chromePath();
    if (!executablePath) throw new Error('no Chrome: set CHROME_PATH');
    await warm(executablePath, '.pak');
    browser = await puppeteer.launch({
      executablePath,
      headless: executablePath.includes('headless-shell') ? 'shell' : true,
      args: ['--no-sandbox'],
    });
  } else {
    const executablePath = firefoxPath();
    if (!executablePath) throw new Error('no Firefox: set FIREFOX_PATH');
    await warm(executablePath, 'omni.ja');
    browser = await puppeteer.launch({
      browser: 'firefox',
      executablePath,
      headless: true,
      // In KiB: the whole limit, of which an origin's group gets a fifth.
      extraPrefsFirefox: { 'dom.quotaManager.temporaryStorage.fixedLimit': (5 * QUOTA) / 1024 },
    });
  }
  try {
    const page = await browser.newPage();
    page.on('console', (m) => said(`console.${m.type()}: ${m.text()}`));
    page.on('pageerror', (e) => said(`pageerror: ${e.message}`));
    if (engine === 'chrome') {
      const cdp = await page.createCDPSession();
      await cdp.send('Storage.overrideQuotaForOrigin', { origin, quotaSize: QUOTA });
    }
    // The page goes on, and reloads itself, after its first load answers.
    await page.goto(url, { waitUntil: 'domcontentloaded' });
    return { version: await browser.version(), close: () => browser.close() };
  } catch (e) {
    await browser.close();
    throw e;
  }
}

/** A port the system picked, let go of for a program that binds it itself. */
function systemPort() {
  return new Promise((resolve, reject) => {
    const s = createNetServer();
    s.on('error', reject);
    s.listen(0, '127.0.0.1', () => {
      const { port } = s.address();
      s.close(() => resolve(port));
    });
  });
}

/**
 * Safari, through safaridriver over plain WebDriver. safaridriver takes
 * its port on its command line and names none it chose (`-p 0` is a
 * default of its own), so it is the one program here given a port the
 * system picked rather than reading back its own
 * (docs/design/conventions.md#ports).
 */
async function safariDriver(url) {
  if (process.platform !== 'darwin') throw new Error('Safari runs on macOS alone');
  const port = await systemPort();
  const child = spawn('safaridriver', ['-p', String(port)], { stdio: ['ignore', 'ignore', 'pipe'] });
  let log = '';
  child.stderr.on('data', (d) => (log += d));
  const stop = () => {
    if (child.exitCode === null) child.kill('SIGTERM');
  };
  process.once('exit', stop);
  const base = `http://127.0.0.1:${port}`;
  const wd = async (method, path, body) => {
    const r = await fetch(base + path, {
      method,
      headers: { 'content-type': 'application/json' },
      body: body && JSON.stringify(body),
      signal: AbortSignal.timeout(60_000),
    });
    const json = await r.json();
    if (!r.ok) throw new Error(`safaridriver: ${json.value?.message ?? r.status} ${log}`);
    return json.value;
  };
  try {
    for (let i = 0; ; i++) {
      try {
        if ((await wd('GET', '/status')).ready) break;
      } catch (e) {
        if (child.exitCode !== null || i >= 200) throw new Error(`safaridriver did not start: ${e.message} ${log}`);
      }
      await new Promise((r) => setTimeout(r, 50));
    }
    const session = await wd('POST', '/session', { capabilities: { alwaysMatch: { browserName: 'safari' } } });
    const id = session.sessionId;
    await wd('POST', `/session/${id}/url`, { url });
    return {
      version: `Safari ${session.capabilities?.browserVersion ?? ''}`.trim(),
      close: async () => {
        await wd('DELETE', `/session/${id}`).catch(() => {});
        stop();
      },
    };
  } catch (e) {
    stop();
    throw e;
  }
}

/** One browser through the whole suite: whether it passed. */
async function runEngine(engine, server) {
  const id = `${engine}-${process.pid}-${Date.now()}`;
  const url = `${server.origin}${PAGE}?run=${encodeURIComponent(id)}&quota=${engine === 'safari' ? 0 : QUOTA}`;
  const run = server.run(id);
  const said = [];
  run.says = (msg) => {
    if (msg.log) return said.push(msg.log);
    const mark = msg.ok ? 'ok  ' : msg.skip ? 'skip' : 'FAIL';
    const why = msg.ok ? (msg.detail ? `: ${msg.detail}` : '') : `: ${msg.skip ?? msg.error}`;
    console.log(`  ${mark}  ${msg.test} (${Math.round(msg.ms)} ms)${why}`);
  };
  console.log(`${engine}:`);
  const t0 = performance.now();
  let driver;
  let outcome;
  try {
    driver =
      engine === 'safari' ? await safariDriver(url) : await puppeteerDriver(engine, url, server.origin, (line) => said.push(line));
    console.log(`  ${driver.version}`);
    let timer;
    outcome = await Promise.race([
      run.done.promise,
      new Promise((r) => (timer = setTimeout(() => r({ ok: false, error: `no end in ${RUN_LIMIT_MS / 1000} s` }), RUN_LIMIT_MS))),
    ]);
    clearTimeout(timer);
  } catch (e) {
    outcome = { ok: false, error: e.message };
  } finally {
    await driver?.close().catch(() => {});
  }
  const failures = [];
  if (!outcome.ok) failures.push(`the page: ${outcome.error}`);
  const skips = SKIPS[engine] ?? {};
  for (const test of run.plan ?? []) {
    const r = run.results.find((x) => x.test === test);
    if (!r) failures.push(`${test}: not run`);
    else if (r.skip && skips[test] === undefined) failures.push(`${test}: skipped (${r.skip}), which ${engine} must run`);
    else if (!r.ok && !r.skip) failures.push(`${test}: ${r.error}`);
  }
  for (const [test, why] of Object.entries(skips)) {
    const r = run.results.find((x) => x.test === test);
    if (r && !r.skip) failures.push(`${test}: ran, where it is skipped on ${engine} (${why}): drop it from SKIPS`);
  }
  if (!run.plan) failures.push('the page sent no plan');
  const secs = ((performance.now() - t0) / 1000).toFixed(1);
  if (failures.length) {
    console.log(`  ${engine} FAILED in ${secs} s:`);
    for (const f of failures) console.log(`    ${f}`);
    if (said.length) console.log(`  what the page said:\n${said.map((l) => `    ${l}`).join('\n')}`);
    return false;
  }
  console.log(`  ${engine}: ${run.results.filter((r) => r.ok).length} passed, ${run.results.filter((r) => r.skip).length} skipped, in ${secs} s`);
  return true;
}

const ENGINES = ['chrome', 'firefox', 'safari'];
const asked = process.argv.slice(2).flatMap((a) => a.split(/[\s,]+/)).filter(Boolean);
const engines = asked.length ? asked : ['chrome'];
for (const e of engines) if (!ENGINES.includes(e)) throw new Error(`no engine ${e}: ${ENGINES.join(', ')}`);
if (!existsSync(join(WEB, 'fennec.wasm'))) throw new Error('no web/fennec.wasm: make wasm');

const server = await serve();
let passed = true;
for (const engine of engines) passed = (await runEngine(engine, server)) && passed;
await server.close();
process.exit(passed ? 0 : 1);

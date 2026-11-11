// What every check shares: starting the test variant of the app, driving it
// through its control file and reading what it reports.
//
// The test variant (FENNEC_DESKTOP_TEST_BUILD=1, `make studio-desktop-test`)
// holds src/bun/testhooks.ts, which a shipped app does not. It reads
//   FENNEC_DESKTOP_REPORT  a file it appends one JSON fact a line to
//   FENNEC_DESKTOP_CONTROL a file of commands, one JSON object a line
//   FENNEC_DESKTOP_OPEN / _LAUNCHER  what to show at start
// and has its own identifier (dev.fennecdb.studio.test), so userData and the
// Keychain service are its own too.
//
// Environment:
//   FENNEC_DESKTOP_APP  the test variant's .app (default build-test/stable-macos-arm64/fennec studio test.app)
//   FENNEC_SERVER_BIN   a fennec-server to start by hand (default bin/fennec-server)
//   SD_DIR              where data, logs and reports go (default a fresh directory under the system's temp one)
import { execSync, spawn, spawnSync } from 'node:child_process';
import { appendFileSync, closeSync, existsSync, mkdirSync, openSync, readdirSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

// SPIKE (Windows and Linux): where the test variant is, its launcher, its
// userData and the processes on the machine differ by system. macOS is as it was.
const IDENTIFIER_ = 'dev.fennecdb.studio.test';
export const WIN = process.platform === 'win32';
export const MAC = process.platform === 'darwin';
export const HERE = fileURLToPath(new URL('../..', import.meta.url));
/**
 * What installs the test variant on Windows and Linux: on Linux the build's
 * build-test/stable-linux-<arch>/<name>/bin/launcher, a self-extractor; on
 * Windows build-test/stable-win-<arch>/<name>-Setup.exe beside its payload (the
 * folder's bin/launcher.exe is the wrapper the Setup carries, and says "Not a
 * valid self-extracting installer" when run).
 */
function findLauncher() {
  const want = (name) => (WIN ? /-Setup\.exe$/.test(name) : name === 'launcher');
  const walk = (dir, depth) => {
    if (!existsSync(dir)) return null;
    for (const name of readdirSync(dir).sort()) {
      const full = join(dir, name);
      const st = statSync(full);
      if (st.isFile() && want(name)) return full;
      if (st.isDirectory() && depth < 4) {
        const hit = walk(full, depth + 1);
        if (hit) return hit;
      }
    }
    return null;
  };
  return walk(join(HERE, 'build-test'), 0) ?? join(HERE, 'build-test', 'installer-not-found');
}
/**
 * On Windows and Linux the build's launcher is a self-extractor that installs
 * (into Electrobun's appData/<identifier>/stable/app, the folder its userData
 * also lives in) and starts the installed app: run again, it installs again
 * over the app it may have started. So `warmup` runs it once and every other
 * check runs the installed launcher.
 */
export const WRAPPER = MAC ? null : findLauncher();
const INSTALLED_ROOT = MAC
  ? null
  : join(WIN ? process.env.LOCALAPPDATA ?? join(homedir(), 'AppData', 'Local') : process.env.XDG_DATA_HOME || join(homedir(), '.local', 'share'), IDENTIFIER_, 'stable', 'app');
export const APP = process.env.FENNEC_DESKTOP_APP ?? (MAC ? `${HERE}build-test/stable-macos-arm64/fennec studio test.app` : INSTALLED_ROOT);
export const EXE = MAC ? `${APP}/Contents/MacOS/launcher` : join(APP, 'bin', WIN ? 'launcher.exe' : 'launcher');
/** The build's own folder of the test variant (Windows and Linux): its wrapper's processes are the app's too. */
const BUILT = MAC ? null : join(WRAPPER, '..', '..');
export const SERVER = process.env.FENNEC_SERVER_BIN ?? join(HERE, 'bin', `fennec-server${WIN ? '.exe' : ''}`);
export const IDENTIFIER = 'dev.fennecdb.studio.test';

export const D = process.env.SD_DIR ?? join(tmpdir(), 'fennec-desktop-checks');
mkdirSync(`${D}/data`, { recursive: true });
mkdirSync(`${D}/logs`, { recursive: true });
/** D as the app sees it (realpath: /tmp is /private/tmp on macOS), for the keys of its windows. */
export const R = realpathSync(D);
export const REPORT = `${D}/report.jsonl`;
export const CONTROL = `${D}/control.jsonl`;
/** Where the test variant keeps its state.json: Electrobun's Utils.paths.userData, by system. */
export const APP_DATA = MAC
  ? join(homedir(), 'Library', 'Application Support', IDENTIFIER)
  : WIN
    ? join(process.env.LOCALAPPDATA ?? join(homedir(), 'AppData', 'Local'), IDENTIFIER)
    : join(process.env.XDG_DATA_HOME || join(homedir(), '.local', 'share'), IDENTIFIER);
export const USER_DATA = join(APP_DATA, 'stable');

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export const now = () => Date.now();
export const log = (...a) => console.log(...a);
export const step = (title) => console.log(`\n-- ${title}`);

export function sh(cmd) {
  try {
    return execSync(cmd, { stdio: ['ignore', 'pipe', 'ignore'], maxBuffer: 1 << 26 }).toString().trim();
  } catch {
    return '';
  }
}

/** Every process on the machine: {pid, ppid, rss (KB), command}. */
export function procs() {
  if (WIN) {
    // SPIKE: no ps; the command lines from WMI (slow: about a second a call).
    const ps = "Get-CimInstance Win32_Process | ForEach-Object { '{0} {1} {2} {3}' -f $_.ProcessId, $_.ParentProcessId, [int]($_.WorkingSetSize/1024), $_.CommandLine }";
    return execSync(`powershell -NoProfile -NonInteractive -Command "${ps}"`, { maxBuffer: 1 << 26, timeout: 30000 })
      .toString()
      .split(/\r?\n/)
      .map((l) => /^\s*(\d+)\s+(\d+)\s+(\d+)\s+(.+)$/.exec(l))
      .filter(Boolean)
      .map((m) => ({ pid: +m[1], ppid: +m[2], rss: +m[3], command: m[4].replace(/^"([^"]*)"/, '$1') }));
  }
  // A bound: a `ps` that hangs would otherwise hang the check, and a hosted runner's own cleanup with it.
  return execSync(MAC ? 'ps -axww -o pid=,ppid=,rss=,command=' : 'ps axww -o pid=,ppid=,rss=,args=', { maxBuffer: 1 << 26, timeout: 30000 })
    .toString()
    .split('\n')
    .map((l) => /^\s*(\d+)\s+(\d+)\s+(\d+)\s+(.*)$/.exec(l))
    .filter(Boolean)
    .map((m) => ({ pid: +m[1], ppid: +m[2], rss: +m[3], command: m[4] }));
}
/** The test variant's processes (the launcher, Bun, the servers it bundles) and servers started by hand on this run's files. */
export const mine = () =>
  procs().filter((p) =>
    MAC
      ? p.command.startsWith(APP) || p.command.includes(`fennec-server --file ${R}/`) || p.command.includes(`fennec-server --file ${D}/`)
      : // SPIKE: Windows' and Linux's command lines: the app's folder (launcher, bun, the bundled servers) and servers on this run's files.
        p.command.includes(APP) || p.command.includes(BUILT) || (/fennec-server/.test(p.command) && (p.command.includes(`--file ${R}`) || p.command.includes(`--file ${D}`))),
  );
export const describe = (list) => list.map((p) => `${p.pid} (ppid ${p.ppid}) ${p.command.replace(APP, "<APP>").slice(0, 200)}`).join('\n');

export function lines() {
  if (!existsSync(REPORT)) return [];
  return readFileSync(REPORT, 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l));
}
/**
 * Sends the app a command. The lines one look of the app finds run in turn,
 * but the next look's lines start beside them (testhooks.ts's `boot`), so a
 * check that sends several waits for each one's report, never the last one's
 * alone.
 */
export function cmd(o) {
  appendFileSync(CONTROL, JSON.stringify(o) + '\n');
}

let current = '';
/** Names the log a failure prints the tail of, for a check that starts the app some other way than startApp. */
export const logName = (name) => void (current = name);
/** What a failure prints: the tail of the app's own log and of what it reported. */
function context() {
  const tail = (file, n) => (existsSync(file) ? readFileSync(file, 'utf8').split('\n').slice(-n).join('\n') : '(none)');
  return `\n--- app log (${current}) ---\n${tail(`${D}/logs/${current}.app.log`, 25)}\n--- last reports ---\n${lines().slice(-12).map((l) => JSON.stringify(l).slice(0, 220)).join('\n')}`;
}

/** The first report line `pred` accepts, waiting for it as long as `ms`: an event is waited for, never slept to. */
export async function waitFor(pred, ms = 30000, label = 'a report') {
  const t0 = now();
  for (;;) {
    const hit = lines().find(pred);
    if (hit) return hit;
    if (now() - t0 > ms) throw new Error(`timeout waiting for ${label}${context()}`);
    await sleep(40);
  }
}
/** Polls `fn` (which may be async) until it returns something truthy, which is returned. */
export async function until(fn, ms = 10000, label = 'a condition') {
  const t0 = now();
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (now() - t0 > ms) throw new Error(`timeout waiting for ${label}${context()}`);
    await sleep(80);
  }
}
/** Asserts nothing arrives within `ms`: the one place a check has to wait for a thing that must not happen. */
export async function waitNone(pred, ms, label) {
  const t0 = now();
  while (now() - t0 < ms) {
    const hit = lines().find(pred);
    if (hit) throw new Error(`${label}: ${JSON.stringify(hit)}${context()}`);
    await sleep(40);
  }
}

let evals = 0;
/**
 * Runs `js` (statements; `return` a value) in the window `key` -- a studio
 * window's key, `launcher`, `newdb` or `notice:<n>` -- through
 * executeJavascript, not the page's own eval(): the studio's CSP forbids that.
 * The result comes back through the event-only bridge.
 */
export async function evalIn(key, js, ms = 15000) {
  // A script reaches the page as one line (executeJavascript), where a `//`
  // comment would swallow what follows it: the eval would never answer.
  if (/(^|\n)\s*\/\//.test(js)) throw new Error('an eval script holds a // comment: use /* */ or none');
  const id = `e${++evals}`;
  const wrapped = `(async()=>{try{const v=await (async()=>{${js}})();window.__electrobunSendToHost({type:"eval",id:${JSON.stringify(id)},ok:true,value:v})}catch(e){window.__electrobunSendToHost({type:"eval",id:${JSON.stringify(id)},ok:false,value:String(e)})}})()`;
  cmd({ cmd: 'eval', key, js: wrapped });
  const hit = await waitFor((l) => l.detail?.type === 'eval' && l.detail.id === id, ms, `eval ${id} in ${key}`);
  return hit.detail;
}

/** A page's visible text, whitespace squeezed. */
export const textOf = async (key) => (await evalIn(key, `return document.body.innerText.replace(/\\s+/g,' ')`)).value;

/** The studio in window `key` showing a database, signed in, in a report after the `after`th. */
// SPIKE: each window's store is a WebView2 environment of its own on Windows (a
// browser process per window), slower to come up on a hosted runner.
export async function waitStudio(key, after = 0, ms = WIN ? 120000 : 30000) {
  return waitFor((l, i) => i >= after && l.detail?.type === 'state' && l.detail.loaded && l.key === key, ms, `the studio loaded in ${key}`);
}
/** The last state a studio window reported. */
export const lastState = (key) => lines().filter((l) => l.detail?.type === 'state' && l.key === key).pop()?.detail;

/** The notice window the app showed next (after `after` reports), as its report, once its page has drawn it. */
export async function waitNotice(title, ms = 15000, after = 0) {
  const shown = await waitFor((l, i) => i >= after && l.what === 'notice' && (title instanceof RegExp ? title.test(l.title) : l.title === title), ms, `a notice "${title}"`);
  await waitFor((l) => l.what === 'notice-ready' && l.key === shown.key, ms, `${shown.key} drawn`);
  return shown;
}
/**
 * Presses a notice's button, as a click would. Every button closes the notice,
 * and the main process closes it on the click's own message: what a script in
 * it would report after the click may not leave the window before it goes (a
 * `serve` run lost it), so the notice's closing is what is waited for.
 */
export async function pressNotice(notice, id) {
  const n = lines().length;
  cmd({ cmd: 'eval', key: notice.key, js: `document.getElementById(${JSON.stringify(`notice-${id}`)}).click()` });
  await waitFor((l, i) => i >= n && l.what === 'notice-closed' && l.key === notice.key, 15000, `${notice.key} closed by ${id}`);
}

/**
 * The test variant, started with `env` added to the hooks' own. Its output
 * goes to logs/<name>.app.log, which a failing run keeps.
 */
export function startApp(name, env = {}, exe = EXE) {
  current = name;
  rmSync(REPORT, { force: true });
  writeFileSync(CONTROL, '');
  if (!existsSync(exe)) throw new Error(`no app at ${exe}: build it with \`make studio-desktop-test\` (on Windows and Linux, the warmup check installs it)`);
  const fd = openSync(`${D}/logs/${name}.app.log`, 'w');
  const child = spawn(exe, [], {
    // SPIKE: a stable Windows launcher is a GUI program that drops the app's
    // output unless asked to keep a console (Electrobun's cross-platform guide).
    env: { ...process.env, ...(WIN ? { ELECTROBUN_CONSOLE: '1' } : {}), FENNEC_DESKTOP_REPORT: REPORT, FENNEC_DESKTOP_CONTROL: CONTROL, ...env },
    stdio: ['ignore', fd, fd],
  });
  closeSync(fd);
  const exited = new Promise((r) => child.on('exit', (code, signal) => r({ code, signal })));
  return { child, exited, name };
}

/** Asks the app to quit and waits until none of its processes is left. Throws if any is. */
export async function quit(app, ms = 40000) {
  cmd({ cmd: 'quit' });
  const t0 = now();
  while (mine().length && now() - t0 < ms) await sleep(100);
  const left = mine();
  if (left.length) {
    for (const p of left) {
      try {
        process.kill(p.pid, 'SIGKILL');
      } catch {
        // gone already
      }
    }
    throw new Error(`the app left processes behind after quit:\n${describe(left)}`);
  }
  await Promise.race([app.exited, sleep(5000)]);
}

/** A fennec-server started by hand, as another program holding a file or a remote server would be. */
export async function startServer(args, env = {}, bin = SERVER) {
  const child = spawn(bin, args, { env: { ...process.env, ...env }, stdio: ['ignore', 'ignore', 'pipe'] });
  let err = '';
  child.stderr.on('data', (b) => {
    err += b;
  });
  const exited = new Promise((r) => child.on('exit', (code) => r(code)));
  let poll;
  const origin = await Promise.race([
    new Promise((resolve) => {
      poll = setInterval(() => {
        const m = /listening on: (http:\/\/\S+)/.exec(err);
        if (m) resolve(m[1]);
      }, 10);
    }),
    exited.then((code) => {
      throw new Error(`the server exited ${code}: ${err}`);
    }),
  ]).finally(() => clearInterval(poll));
  return {
    child,
    origin,
    exited,
    err: () => err,
    async stop() {
      if (child.exitCode === null) child.kill('SIGTERM');
      await exited;
    },
  };
}

/** The data file `name` the checks open. */
export const file = (name) => (MAC ? `${R}/data/${name}.fennec` : join(R, 'data', `${name}.fennec`));

/** A fresh data file `name` of `rows` orders (the collections the studio's own tests use), replacing any. */
export function data(name, rows = 50) {
  const path = file(name);
  const r = spawnSync(process.execPath, [`${HERE}scripts/mkdata.mjs`, path, String(rows)], {
    env: { ...process.env, FENNEC_SERVER_BIN: SERVER },
    stdio: ['ignore', 'ignore', 'pipe'],
    encoding: 'utf8',
  });
  if (r.status !== 0) throw new Error(`could not make ${path}: ${r.stderr}`);
  return path;
}

/**
 * The Keychain items of the databases a check served (`serve:<id>`, under the
 * test variant's own service), deleted: a check that ends with one served
 * leaves its token behind otherwise, and the next run's ids are new ones.
 */
export function forgetServeItems() {
  let databases = [];
  try {
    databases = JSON.parse(readFileSync(`${USER_DATA}/state.json`, 'utf8')).databases ?? [];
  } catch {
    return;
  }
  for (const d of databases) spawnSync('security', ['delete-generic-password', '-s', IDENTIFIER, '-a', `serve:${d.id}`], { stdio: 'ignore' });
}

/** Every process of the app and of the servers on this run's files must be gone before a check starts. */
export function assertClean(when) {
  const left = mine();
  if (left.length) throw new Error(`${when}: processes left behind:\n${describe(left)}`);
}

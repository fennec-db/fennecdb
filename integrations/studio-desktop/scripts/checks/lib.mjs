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
// The same checks drive it on macOS, Linux and Windows: what differs by system
// is here -- where the app is and how it is installed, its folders, the
// processes on the machine, the credential store, a killed program's exit
// code, a path's separator and whether the file system tells case apart. Which
// checks a system does not run yet, and why, is run.mjs's list.
//
// Environment:
//   FENNEC_DESKTOP_APP  the test variant (default: macOS build-test/stable-macos-arm64/fennec studio test.app;
//                       Windows and Linux the copy `warmup` installs into its folder)
//   FENNEC_SERVER_BIN   a fennec-server to start by hand (default bin/fennec-server, .exe on Windows)
//   SD_DIR              where data, logs and reports go (default a fresh directory under the system's temp one)
import { execSync, spawn, spawnSync } from 'node:child_process';
import { appendFileSync, closeSync, copyFileSync, existsSync, mkdirSync, openSync, readdirSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { delimiter, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

export const MAC = process.platform === 'darwin';
export const WIN = process.platform === 'win32';
export const LINUX = process.platform === 'linux';

/** This directory (integrations/studio-desktop), with a separator at its end. A URL's pathname is /D:/a/... on Windows. */
export const HERE = fileURLToPath(new URL('../..', import.meta.url));
export const IDENTIFIER = 'dev.fennecdb.studio.test';
/** A list of paths in one variable (FENNEC_DESKTOP_OPEN), as PATH is one: ";" on Windows, where a path holds a ":". */
export const LIST = delimiter;

/**
 * The test variant's folder, Electrobun's `<appData>/<identifier>`: its userData
 * (`stable/`, state.json in it) on every system, and on Windows and Linux the
 * installed app (`stable/app`) and the pages' stores too.
 */
export const APP_DATA = MAC
  ? join(homedir(), 'Library', 'Application Support', IDENTIFIER)
  : WIN
    ? join(process.env.LOCALAPPDATA ?? join(homedir(), 'AppData', 'Local'), IDENTIFIER)
    : join(process.env.XDG_DATA_HOME || join(homedir(), '.local', 'share'), IDENTIFIER);
/** Where the test variant keeps its state.json. */
export const USER_DATA = join(APP_DATA, 'stable');

/** The test variant's builds: what runs from here is the test variant's, its installer on Windows and Linux. */
const BUILT = join(HERE, 'build-test');
/**
 * What installs the test variant on Windows and Linux, where a stable build is
 * an installer and not an app that runs in place: on Windows the build's
 * `<name>-Setup.exe` beside its payload (the build folder's own
 * `bin/launcher.exe` answers "Not a valid self-extracting installer"); on Linux
 * the build folder's `bin/launcher`, a self-extractor that installs the app into
 * USER_DATA/app and starts it -- and, run again, installs it again over the app
 * it started. So `warmup` runs it once, and every other check runs the
 * installed app. None on macOS, where the build unpacks itself in place.
 */
function installer() {
  const dir = join(BUILT, `stable-${WIN ? 'win' : 'linux'}-${process.arch}`);
  const names = existsSync(dir) ? readdirSync(dir).sort() : [];
  const found = WIN ? names.find((n) => /-Setup\.exe$/.test(n)) : names.find((n) => existsSync(join(dir, n, 'bin', 'launcher')));
  if (!found) return join(dir, 'no-installer-here-build-the-test-variant');
  return WIN ? join(dir, found) : join(dir, found, 'bin', 'launcher');
}
export const INSTALLER = MAC ? null : installer();
export const APP = process.env.FENNEC_DESKTOP_APP ?? (MAC ? `${HERE}build-test/stable-macos-arm64/fennec studio test.app` : join(USER_DATA, 'app'));
export const EXE = MAC ? `${APP}/Contents/MacOS/launcher` : join(APP, 'bin', WIN ? 'launcher.exe' : 'launcher');
export const SERVER = process.env.FENNEC_SERVER_BIN ?? join(HERE, 'bin', `fennec-server${WIN ? '.exe' : ''}`);

/**
 * Whether the exit code a server reported is that of a process killed from
 * outside (`kill -9`): no code and the signal on a Unix (137 where a shell
 * reports it), 1 on Windows, where Node's and Bun's SIGKILL is
 * TerminateProcess(handle, 1).
 */
export const killedCode = (code) => (WIN ? code === 1 : code === null || code === 137);
/** How the app says so in a notice. */
export const KILLED_SAID = WIN ? /code 1\b/ : /killed by a signal|code 137/;

// Resolved, so a separator SD_DIR was given with (Git's bash writes D:\a\_temp/x) is the system's.
export const D = resolve(process.env.SD_DIR ?? join(tmpdir(), 'fennec-desktop-checks'));
mkdirSync(join(D, 'data'), { recursive: true });
mkdirSync(join(D, 'logs'), { recursive: true });
/** D as the app sees it (realpath: /tmp is /private/tmp on macOS), for the keys of its windows. */
export const R = realpathSync(D);
export const REPORT = join(D, 'report.jsonl');
export const CONTROL = join(D, 'control.jsonl');

/**
 * Whether the file system the checks write on tells `a` from `A`: APFS by
 * default and NTFS do not, ext4 does. Asked of the folder itself, not assumed
 * by system (APFS can be made case-sensitive).
 */
export const CASE_BLIND = (() => {
  const probe = join(R, `case-probe-${process.pid}`);
  writeFileSync(probe, '');
  try {
    return existsSync(join(R, `CASE-PROBE-${process.pid}`));
  } finally {
    rmSync(probe, { force: true });
  }
})();

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

/**
 * The PowerShell that asks WMI on Windows: PowerShell 7 (`pwsh`) where there is
 * one, else Windows PowerShell. On windows-11-arm the latter runs emulated and
 * took 42 to 45 s a query (its CIM module loading), `pwsh` 0.6 s; on
 * windows-latest both 0.4 s.
 */
export const POWERSHELL = WIN ? ((process.env.PATH ?? '').split(delimiter).some((d) => d && existsSync(join(d, 'pwsh.exe'))) ? 'pwsh.exe' : 'powershell.exe') : null;

/** Every process on the machine: {pid, ppid, rss (KB), command}. */
export function procs() {
  if (WIN) {
    // No ps: the command lines are WMI's. A program's path comes quoted, and is taken out of its quotes.
    const ps = "Get-CimInstance Win32_Process | ForEach-Object { '{0} {1} {2} {3}' -f $_.ProcessId, $_.ParentProcessId, [int64]($_.WorkingSetSize / 1024), $_.CommandLine }";
    return execSync(`${POWERSHELL} -NoProfile -NonInteractive -Command "${ps}"`, { maxBuffer: 1 << 26, timeout: 30000 })
      .toString()
      .split(/\r?\n/)
      .map((l) => /^\s*(\d+)\s+(\d+)\s+(\d+)\s+(.+)$/.exec(l))
      .filter(Boolean)
      .map((m) => ({ pid: +m[1], ppid: +m[2], rss: +m[3], command: m[4].replace(/^"([^"]*)"/, '$1') }));
  }
  // A bound: a `ps` that hangs would otherwise hang the check, and a hosted runner's own cleanup with it.
  // procps' `args` is BSD's `command`, the whole command line.
  return execSync(MAC ? 'ps -axww -o pid=,ppid=,rss=,command=' : 'ps axww -o pid=,ppid=,rss=,args=', { maxBuffer: 1 << 26, timeout: 30000 })
    .toString()
    .split('\n')
    .map((l) => /^\s*(\d+)\s+(\d+)\s+(\d+)\s+(.*)$/.exec(l))
    .filter(Boolean)
    .map((m) => ({ pid: +m[1], ppid: +m[2], rss: +m[3], command: m[4] }));
}
/** Paths compare as the system does: Windows' are case-blind (a command line may spell the drive or a folder otherwise). */
const same = WIN ? (s) => s.toLowerCase() : (s) => s;
/** A server started by hand on one of this run's files (`fennec-server.exe --file D:\...` on Windows). */
const handServer = (command) =>
  MAC
    ? command.includes(`fennec-server --file ${R}/`) || command.includes(`fennec-server --file ${D}/`)
    : /fennec-server(\.exe)? --file /i.test(command) && [R, D].some((dir) => same(command).includes(same(`--file ${dir}${sep}`)));
/**
 * The test variant's processes (the launcher, Bun, the servers it bundles) and
 * servers started by hand on this run's files. On Windows and Linux also what
 * runs from its build (the installer) and, on Windows, from its folder:
 * WebView2's processes are told the store they use there (`--user-data-dir`),
 * and outlive the app by a moment.
 */
export const mine = () =>
  procs().filter((p) =>
    MAC
      ? p.command.startsWith(APP) || handServer(p.command)
      : same(p.command).includes(same(APP)) || same(p.command).includes(same(USER_DATA)) || same(p.command).includes(same(BUILT)) || handServer(p.command),
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
export async function waitStudio(key, after = 0, ms = 30000) {
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
export function startApp(name, env = {}) {
  current = name;
  rmSync(REPORT, { force: true });
  writeFileSync(CONTROL, '');
  if (!existsSync(EXE)) throw new Error(`no app at ${APP}: build it with \`make studio-desktop-test\`${MAC ? '' : ' (on Windows and Linux the warmup check installs it)'}`);
  const fd = openSync(`${D}/logs/${name}.app.log`, 'w');
  const child = spawn(EXE, [], {
    // A stable Windows launcher is a program with no console, which drops the
    // app's output unless told to keep one (Electrobun's ELECTROBUN_CONSOLE).
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
export const file = (name) => join(R, 'data', `${name}.fennec`);

/** A fresh data file `name` of `rows` orders (the collections the studio's own tests use), replacing any. */
export function data(name, rows = 50) {
  const path = file(name);
  const r = spawnSync(process.execPath, [join(HERE, 'scripts', 'mkdata.mjs'), path, String(rows)], {
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
  for (const d of databases) {
    if (MAC) spawnSync('security', ['delete-generic-password', '-s', IDENTIFIER, '-a', `serve:${d.id}`], { stdio: 'ignore' });
    else forgetCredentials(`serve:${d.id}`);
  }
}

/**
 * Where the pages' data stores are, a saved server's `persist:remote-<id>`
 * among them: WebKit's for the identifier on macOS; on Linux Electrobun's under
 * `$HOME/.local/share` and `$HOME/.cache` (it reads HOME, not XDG_DATA_HOME);
 * on Windows WebView2's user data folder in the test variant's own.
 */
export const STORES = MAC
  ? [join(homedir(), 'Library', 'WebKit', IDENTIFIER)]
  : LINUX
    ? [join(homedir(), '.local', 'share', IDENTIFIER, 'stable', 'WebKit'), join(homedir(), '.cache', IDENTIFIER, 'stable', 'WebKit')]
    : [join(USER_DATA, 'WebView2')];
/** Where WebKit keeps the test variant's data stores on macOS. */
export const WEBKIT = STORES[0];
/** Windows: the folder WebView2 keeps the persistent partition `persist:<name>` in (src/bun/stores.ts, Electrobun's own naming for the plain names the app gives). */
export const partitionDir = (partition) => join(USER_DATA, 'WebView2', 'Partitions', partition.replace(/^persist:/, ''));

/**
 * Windows: the top-level windows of the app's WebView2 processes (the app's own
 * windows are bun.exe's), which a popup WebView2 opened by itself would be.
 */
export function webviewWindows() {
  const dir = `'${USER_DATA.replace(/'/g, "''")}'`;
  const ps = `$ids = @(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains(${dir}) } | ForEach-Object { $_.ProcessId }); if ($ids.Count) { Get-Process -Id $ids -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | ForEach-Object { '{0} {1}' -f $_.Id, $_.MainWindowTitle } }`;
  const r = spawnSync(POWERSHELL, ['-NoProfile', '-NonInteractive', '-Command', ps], { encoding: 'utf8', timeout: 60000 });
  if (r.status !== 0) throw new Error(`the windows were not listed: ${r.stderr || r.error}`);
  return r.stdout.trim().split(/\r?\n/).filter(Boolean);
}

/** The Bun the test variant bundles: what a check runs a script of the app's own with (src/bun/win32.ts), or builds a stand-in program with. */
export const BUN = MAC ? `${APP}/Contents/MacOS/bun` : join(APP, 'bin', WIN ? 'bun.exe' : 'bun');

/**
 * Whether the system's credential store holds the test variant's item `name`,
 * asked of the store by name and never for the secret: `security` on macOS
 * (attributes, no `-w`), `secret-tool` on Linux (libsecret's attributes
 * `service` and `account`, which Bun.secrets keeps), `cmdkey` on Windows (the
 * generic credential `<service>/<name>`). On Windows also how it is kept:
 * `local`, kept on this machine and never roamed with a profile.
 */
export function credential(name) {
  if (MAC) return { there: spawnSync('security', ['find-generic-password', '-s', IDENTIFIER, '-a', name], { stdio: 'ignore', timeout: 30000 }).status === 0 };
  if (LINUX) {
    const r = spawnSync('secret-tool', ['lookup', 'service', IDENTIFIER, 'account', name], { stdio: ['ignore', 'ignore', 'pipe'], encoding: 'utf8', timeout: 30000 });
    if (r.error || (r.status !== 0 && r.status !== 1)) throw new Error(`secret-tool lookup: ${r.error?.message ?? `status ${r.status}: ${r.stderr}`}`);
    return { there: r.status === 0 };
  }
  const r = spawnSync('cmdkey', [`/list:${IDENTIFIER}/${name}`], { encoding: 'utf8', timeout: 30000 });
  if (r.error || r.status !== 0) throw new Error(`cmdkey /list: ${r.error?.message ?? `status ${r.status}`}`);
  const there = /Target:/.test(r.stdout);
  return { there, local: there && /Local machine persistence/i.test(r.stdout), said: r.stdout.replace(/\s+/g, ' ').trim() };
}

/**
 * The test variant's items in the system's credential store, every one or
 * `account`'s alone, gone; Linux and Windows (macOS's are `security`'s, in
 * freshState). Bun.secrets keeps an item by service and name: on Linux as
 * libsecret's attributes `service` and `account`, which `secret-tool clear`
 * matches exactly; on Windows as a generic credential named
 * `<service>/<name>`, which `cmdkey` lists as `LegacyGeneric:target=<that>`.
 * Either way a real install's service, `dev.fennecdb.studio`, is another.
 * One the store cannot answer stops the run rather than leave items unsaid.
 */
function forgetCredentials(account) {
  if (LINUX) {
    const r = spawnSync('secret-tool', ['clear', 'service', IDENTIFIER, ...(account ? ['account', account] : [])], { encoding: 'utf8', timeout: 30000 });
    // Status 1 and nothing said is no item matched (libsecret's clear returns FALSE
    // with no error); a store that cannot answer says why.
    const noneThere = r.status === 1 && (r.stderr ?? '').trim() === '';
    if (r.error || (r.status !== 0 && !noneThere)) {
      throw new Error(`secret-tool clear service ${IDENTIFIER}: ${r.error?.message ?? `status ${r.status}: ${r.stderr}`} (libsecret-tools has secret-tool, and a Secret Service must answer on the session bus)`);
    }
    return;
  }
  const listed = spawnSync('cmdkey', ['/list'], { encoding: 'utf8', timeout: 30000 });
  if (listed.error || listed.status !== 0) throw new Error(`cmdkey /list: ${listed.error?.message ?? `status ${listed.status}`}`);
  for (const m of listed.stdout.matchAll(/^\s*Target:\s*(\S.*?)\s*$/gm)) {
    const name = m[1].replace(/^LegacyGeneric:target=/, '');
    if (account ? name !== `${IDENTIFIER}/${account}` : !name.startsWith(`${IDENTIFIER}/`)) continue;
    const r = spawnSync('cmdkey', [`/delete:${m[1]}`], { encoding: 'utf8', timeout: 30000 });
    if (r.error || r.status !== 0) throw new Error(`cmdkey /delete:${m[1]}: ${r.error?.message ?? `status ${r.status}: ${r.stdout}`}`);
  }
}

/**
 * What the test variant keeps between runs, gone: its userData (state.json:
 * the recent files, the saved connections, the databases it served), every
 * item of its service in the credential store (a connection's token, a served
 * database's) and its pages' data stores. run.mjs does this before each check,
 * so one that failed half-way (a connection it never removed) is not in the
 * next one's state: `remote` once failed for a connection `storage` had left.
 * Only the test variant's own: its identifier is its own, and the store is
 * asked for its service exactly (`dev.fennecdb.studio`, a real install's, is
 * another).
 */
export function freshState() {
  // state.json alone: the rest of userData is Electrobun's (its self-extraction cache, its uninstall
  // notes, and on Windows and Linux the installed app), no check's state, and goes once a run,
  // before `warmup` (run.mjs).
  rmSync(join(USER_DATA, 'state.json'), { force: true });
  for (const store of STORES) rmSync(store, { recursive: true, force: true });
  if (!MAC) return forgetCredentials(null);
  // `security` deletes the first item it finds, so it is asked until none is left (44: none found);
  // bounded, as `ps` is, and one it could not answer stops the run rather than leave items unsaid.
  for (let i = 0; ; i++) {
    if (i === 1000) throw new Error(`more than 1000 Keychain items under ${IDENTIFIER}`);
    const r = spawnSync('security', ['delete-generic-password', '-s', IDENTIFIER], { stdio: 'ignore', timeout: 30000 });
    if (r.status === 0) continue;
    if (r.status === 44) break;
    throw new Error(`security delete-generic-password -s ${IDENTIFIER}: ${r.error ?? `status ${r.status}, signal ${r.signal}`}`);
  }
}

/**
 * What a failed check leaves beside its output, in logs/<name>.*: the unified
 * log of the app and of WebKit's processes since `since` (WebKit logs each
 * load's steps in the app's own `bun`: the page made, the web process
 * launched, the policy decided, the commit), a sample of the app while it
 * still runs, and the crash reports written since. A window whose page never
 * loads says nothing else: Electrobun 2.0.4 reports no failed navigation and
 * no web process that ended. run.mjs calls it before it kills what is left.
 *
 * Linux and Windows keep less: WebKitGTK writes its warnings to the app's own
 * output (the check's app log), and neither system has a log this reads yet;
 * what is kept is what was running at the failure, the app's processes and
 * the web engine's (WebKitGTK's helpers, WebView2's msedgewebview2).
 */
export function keepEvidence(name, since) {
  if (!MAC) {
    const engine = procs().filter((p) => /WebKit(Web|Network)Process|msedgewebview2/i.test(p.command));
    writeFileSync(`${D}/logs/${name}.processes.txt`, `the app's, ${new Date().toISOString()} (the check began ${new Date(since).toISOString()}):\n${describe(mine())}\n\nthe web engine's:\n${describe(engine)}\n`);
    return;
  }
  const local = (t) => {
    const d = new Date(t);
    const p = (n) => String(n).padStart(2, '0');
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
  };
  const out = `${D}/logs/${name}`;
  const predicate = 'process == "bun" OR process == "launcher" OR process BEGINSWITH "com.apple.WebKit" OR process == "fennec-server"';
  const shown = spawnSync('/usr/bin/log', ['show', '--style', 'compact', '--start', local(since - 1000), '--predicate', predicate], { encoding: 'utf8', maxBuffer: 1 << 28, timeout: 120000 });
  writeFileSync(`${out}.unified.log`, shown.stdout || `log show: ${shown.error ?? shown.stderr}`);
  for (const p of mine().filter((p) => /\/Contents\/MacOS\/bun /.test(p.command))) {
    spawnSync('/usr/bin/sample', [String(p.pid), '2', '-file', `${out}.sample-${p.pid}.txt`], { stdio: 'ignore', timeout: 60000 });
  }
  const reports = join(homedir(), 'Library', 'Logs', 'DiagnosticReports');
  let names = [];
  try {
    names = readdirSync(reports).filter((f) => /^(bun|launcher|fennec|com\.apple\.WebKit)/.test(f));
  } catch {
    // none
  }
  for (const f of names) {
    if (statSync(join(reports, f)).mtimeMs >= since) copyFileSync(join(reports, f), `${out}.crash-${f}`);
  }
}

/** Every process of the app and of the servers on this run's files must be gone before a check starts. */
export function assertClean(when) {
  const left = mine();
  if (left.length) throw new Error(`${when}: processes left behind:\n${describe(left)}`);
}

/**
 * Windows and Linux: what the test variant's registration of `.fennec` is
 * (src/bun/associate.ts writes it at each start), by the names the README's
 * "Uninstall" gives a user to remove it by.
 */
export const PROG_ID = `${IDENTIFIER}.fennec`;
export const MIME_TYPE = 'application/x-fennecdb';
const DATA_HOME = process.env.XDG_DATA_HOME && process.env.XDG_DATA_HOME.startsWith('/') ? process.env.XDG_DATA_HOME : join(homedir(), '.local', 'share');
export const DESKTOP_FILE = join(DATA_HOME, 'applications', `${IDENTIFIER}.desktop`);
export const MIME_FILE = join(DATA_HOME, 'mime', 'packages', `${IDENTIFIER}.xml`);

/** `reg` with an argument vector, its output and status: Windows' own registry tool, which the README's removal uses too. */
export function reg(args) {
  const r = spawnSync('reg', args, { encoding: 'utf8', timeout: 30000 });
  if (r.error) throw new Error(`reg ${args.join(' ')}: ${r.error.message}`);
  return { status: r.status, out: `${r.stdout ?? ''}${r.stderr ?? ''}` };
}

/** A REG_SZ of HKCU (`name` null: the key's default), or null where there is none. */
export function regValue(key, name) {
  const r = reg(['query', `HKCU\\${key}`, ...(name === null ? ['/ve'] : ['/v', name])]);
  if (r.status !== 0) return null;
  const m = /REG_SZ[ \t]*(.*?)\r?$/m.exec(r.out);
  return m ? m[1] : null;
}

/**
 * The test variant's registration, as the system holds it: on Windows the
 * values under HKCU\Software\Classes; on Linux the two files, and what GIO
 * makes of them (a `.fennec` file's type, the programs for that type).
 */
export function registration(sample) {
  if (WIN) {
    return {
      progId: regValue(`Software\\Classes\\${PROG_ID}`, null),
      icon: regValue(`Software\\Classes\\${PROG_ID}\\DefaultIcon`, null),
      command: regValue(`Software\\Classes\\${PROG_ID}\\shell\\open\\command`, null),
      listed: regValue(`Software\\Classes\\.fennec\\OpenWithProgids`, PROG_ID) !== null,
      default: regValue('Software\\Classes\\.fennec', null),
    };
  }
  const read = (p) => (existsSync(p) ? readFileSync(p, 'utf8') : null);
  const gio = (args) => {
    const r = spawnSync('gio', args, { encoding: 'utf8', timeout: 30000 });
    return r.error ? `gio: ${r.error.message}` : `${r.stdout ?? ''}${r.stderr ?? ''}`.trim();
  };
  return {
    desktop: read(DESKTOP_FILE),
    mime: read(MIME_FILE),
    type: sample ? gio(['info', '-a', 'standard::content-type', sample]).match(/standard::content-type: (\S+)/)?.[1] ?? null : null,
    programs: gio(['mime', MIME_TYPE]),
  };
}

/** Runs one of the system's tools on the registration and says how it ended; one not installed is said, not failed. */
function tool(argv) {
  const r = spawnSync(argv[0], argv.slice(1), { encoding: 'utf8', timeout: 60000 });
  if (r.error?.code === 'ENOENT') return `${argv[0]}: not installed`;
  if (r.error || r.status !== 0) throw new Error(`${argv.join(' ')}: ${r.error?.message ?? `status ${r.status}: ${r.stderr}`}`);
  return `${argv[0]}: done`;
}

/**
 * The test variant's registration gone, as the README tells a user to remove
 * the app's: on Windows its ProgID, its line in `.fennec`'s list, and
 * `.fennec`'s default where that is its own (`reg delete`); on Linux its two
 * files, and the system's caches made again without them. What another
 * program registered is left.
 */
export function forgetRegistration() {
  if (MAC) return [];
  if (WIN) {
    const done = [];
    if (reg(['delete', `HKCU\\Software\\Classes\\${PROG_ID}`, '/f']).status === 0) done.push(PROG_ID);
    if (reg(['delete', 'HKCU\\Software\\Classes\\.fennec\\OpenWithProgids', '/v', PROG_ID, '/f']).status === 0) done.push('.fennec\\OpenWithProgids');
    if (regValue('Software\\Classes\\.fennec', null) === PROG_ID && reg(['delete', 'HKCU\\Software\\Classes\\.fennec', '/ve', '/f']).status === 0) done.push('.fennec default');
    return done;
  }
  const done = [];
  for (const f of [DESKTOP_FILE, MIME_FILE]) {
    if (existsSync(f)) {
      rmSync(f);
      done.push(f);
    }
  }
  done.push(tool(['update-mime-database', join(DATA_HOME, 'mime')]));
  if (existsSync(join(DATA_HOME, 'applications'))) done.push(tool(['update-desktop-database', join(DATA_HOME, 'applications')]));
  return done;
}

/**
 * A double-click on `path`, as close as a runner allows: on Windows the
 * shell's own ShellExecuteEx on the file (.NET's Process.Start with
 * UseShellExecute, which Explorer's double-click is, with no dialog on a
 * failure), on Linux `gio open`, GNOME's (GDesktopAppInfo, the program the
 * file's type names). The hooks' variables go along in the environment, which
 * a program the system starts from here inherits. The path goes as data: in
 * the environment on Windows, an argument on Linux.
 */
export function doubleClick(path, env = {}) {
  const all = { ...process.env, FENNEC_DESKTOP_REPORT: REPORT, FENNEC_DESKTOP_CONTROL: CONTROL, ...env };
  if (WIN) {
    const ps = "$i = New-Object System.Diagnostics.ProcessStartInfo; $i.FileName = $env:FENNEC_CHECK_OPEN; $i.UseShellExecute = $true; [void][System.Diagnostics.Process]::Start($i)";
    const r = spawnSync(POWERSHELL, ['-NoProfile', '-NonInteractive', '-Command', ps], { env: { ...all, FENNEC_CHECK_OPEN: path }, encoding: 'utf8', timeout: 60000 });
    return { status: r.status, said: `${r.stdout ?? ''}${r.stderr ?? ''}${r.error ? r.error.message : ''}`.trim() };
  }
  const r = spawnSync('gio', ['open', path], { env: all, encoding: 'utf8', timeout: 60000 });
  return { status: r.status, said: `${r.stdout ?? ''}${r.stderr ?? ''}${r.error ? r.error.message : ''}`.trim() };
}

// The download on Windows and Linux, held as it ships: the zip or the tarball
// scripts/dist.sh made, whose app is the shipped app (no test hook), unpacked
// from the build's payload. checks/dist.mjs hands over to this there.
//
//   node scripts/checks/dist.mjs [the archive]
//     default: the one artifacts/dist/fennec-studio-<windows|linux>-<x64|arm64>.<zip|tar.gz>
//
// What it holds, on that very file:
//   - it is one folder, fennec-studio, the app unpacked: its launcher and its
//     main process, no payload left, and a launcher that is not Electrobun's
//     extractor (the build folder's own is, and must be found to be: the
//     control); it holds none of the test hooks (release.mjs);
//   - unpacked anywhere, as a browser's download is, it starts from there:
//     its processes up and a window of theirs on screen; it is then stopped,
//     leaves nothing running, and not a file of it changed;
//   - installed through what a user runs (install.ps1 -App on Windows, under
//     Windows PowerShell 5.1, which `irm | iex` runs; install.sh --app on
//     Linux) from a release laid out on this machine, it is where the
//     installer says, file for file the archive's, the Start-menu shortcut or
//     the menu entry it wrote pointing at it, and it starts from there the
//     same way, started by that shortcut's or entry's own target.
//
// Stopped: on Linux by SIGTERM to its main process, the app's own handler,
// every process of it gone and the launcher's exit 0, as on macOS. On Windows
// the process tree is ended (taskkill /T /F): a SIGTERM there is
// TerminateProcess, and a quit by the app's own hand is phase 2's (#578).
//
// The app runs with a home of the check's own -- HOME on Linux (the XDG
// folders unset, so they are under it), LOCALAPPDATA, APPDATA and the
// temporary folder on Windows -- so what it keeps is no real install's.
// Whatever it made under the real ones for the shipped identifier anyway is
// removed, where this run made it, and said.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { closeSync, copyFileSync, createReadStream, existsSync, mkdirSync, openSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { D, HERE, POWERSHELL, R, WIN, log, procs, sleep, step } from './lib.mjs';
import { treeDigest } from '../tree-digest.mjs';

const SYS = WIN ? 'windows' : 'linux';
const EXT = WIN ? 'zip' : 'tar.gz';
const IDENTIFIER = 'dev.fennecdb.studio';
const REPO = resolve(HERE, '..', '..');
const ROOT = join(R, 'dist');
const EXE = WIN ? '.exe' : '';
const { version: VERSION } = JSON.parse(readFileSync(join(HERE, 'package.json'), 'utf8'));

function archive() {
  if (process.argv[2]) return resolve(process.argv[2]);
  const dir = join(HERE, 'artifacts', 'dist');
  const found = existsSync(dir) ? readdirSync(dir).filter((n) => n.startsWith(`fennec-studio-${SYS}-`) && n.endsWith(`.${EXT}`)) : [];
  assert.equal(found.length, 1, `one archive in ${dir} (${found.join(', ') || 'none'}): make it with scripts/dist.sh`);
  return join(dir, found[0]);
}
const ARCHIVE = archive();
const ARCH = new RegExp(`^fennec-studio-${SYS}-(x64|arm64)\\.${EXT.replace('.', '\\.')}$`).exec(ARCHIVE.split(/[\\/]/).pop())?.[1];
assert.ok(ARCH, `${ARCHIVE} is named fennec-studio-${SYS}-<x64|arm64>.${EXT}`);

const sha256 = (file) => createHash('sha256').update(readFileSync(file)).digest('hex');
const same = WIN ? (s) => s.toLowerCase() : (s) => s;
const BSDTAR = WIN ? join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'tar.exe') : 'tar';

function said(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { encoding: 'utf8', timeout: 300000, maxBuffer: 1 << 26, ...opts });
  if (r.status !== 0) throw new Error(`${cmd} ${args.join(' ')} failed (${r.status ?? r.signal ?? r.error}):\n${r.stdout ?? ''}${r.stderr ?? ''}`);
  return `${r.stdout ?? ''}${r.stderr ?? ''}`;
}

async function until(fn, ms, label) {
  const t0 = Date.now();
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() - t0 > ms) throw new Error(`timeout (${ms / 1000} s) waiting for ${label}`);
    await sleep(250);
  }
}

// What the real home holds for the shipped identifier before anything runs:
// what a run adds there is this check's to remove, what was there is a real
// install's (none on a runner).
const REAL = WIN
  ? [join(process.env.LOCALAPPDATA ?? join(homedir(), 'AppData', 'Local'), IDENTIFIER), join(process.env.APPDATA ?? join(homedir(), 'AppData', 'Roaming'), 'Microsoft', 'Windows', 'Start Menu', 'Programs', 'fennec studio.lnk')]
  : [join(homedir(), '.local', 'share', IDENTIFIER), join(homedir(), '.local', 'share', 'applications', 'fennecstudio.desktop'), join(homedir(), '.config', IDENTIFIER), join(homedir(), '.cache', IDENTIFIER)];
const before = new Set(REAL.filter((p) => existsSync(p)));
if (before.size) log(`the real home holds ${[...before].join(', ')} (a real install): left as it is`);

/** Every process started from `pid` down, by parent, with those whose command names `app`. */
function family(roots, app) {
  const all = procs();
  const keep = new Map(all.filter((p) => app && same(p.command).includes(same(app))).map((p) => [p.pid, p]));
  for (let grew = true; grew; ) {
    grew = false;
    for (const p of all) {
      if (!keep.has(p.pid) && (roots.has(p.ppid) || keep.has(p.ppid))) {
        keep.set(p.pid, p);
        grew = true;
      }
    }
  }
  return [...keep.values()];
}

// Whatever happens, nothing this check started is left running, the release's
// server is closed, and what the runs added to the real home goes.
const started = new Map();
let server = null;
function endTree(pid) {
  if (WIN) spawnSync('taskkill', ['/PID', String(pid), '/T', '/F'], { stdio: 'ignore', timeout: 60000 });
  else {
    try {
      process.kill(pid, 'SIGKILL');
    } catch {
      // gone
    }
  }
}
function tidy() {
  for (const [pid, app] of started) {
    for (const p of family(new Set([pid]), app)) endTree(p.pid);
    endTree(pid);
  }
  server?.close();
  const made = REAL.filter((p) => existsSync(p) && !before.has(p));
  for (const p of made) rmSync(p, { recursive: true, force: true });
  if (made.length) log(`removed what the runs made in the real home for ${IDENTIFIER}: ${made.join(', ')}`);
}
process.on('exit', tidy);
for (const sig of ['SIGINT', 'SIGTERM']) process.on(sig, () => process.exit(1));

/** Which of `pids` has a window on screen: a visible top-level window of its own (Windows), one mapped and carrying its _NET_WM_PID (X11, xdotool). */
function windowed(pids) {
  if (!pids.length) return null;
  if (WIN) {
    const ps = `Get-Process -Id ${pids.join(',')} -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | ForEach-Object { '{0} {1}' -f $_.Id, $_.MainWindowTitle }`;
    const out = spawnSync(POWERSHELL, ['-NoProfile', '-NonInteractive', '-Command', ps], { encoding: 'utf8', timeout: 60000 }).stdout ?? '';
    const m = /^(\d+) ?(.*)$/m.exec(out.trim());
    return m ? { pid: +m[1], title: m[2] } : null;
  }
  for (const pid of pids) {
    const out = spawnSync('xdotool', ['search', '--onlyvisible', '--pid', String(pid)], { encoding: 'utf8', timeout: 20000 }).stdout ?? '';
    const id = out.trim().split('\n')[0];
    if (id) {
      const title = (spawnSync('xdotool', ['getwindowname', id], { encoding: 'utf8', timeout: 20000 }).stdout ?? '').trim();
      return { pid, title };
    }
  }
  return null;
}

/** Every file under `root` and its SHA-256, by its path in the tree. */
function listing(root) {
  const out = new Map();
  const walk = (dir) => {
    for (const n of readdirSync(dir, { withFileTypes: true })) {
      const full = join(dir, n.name);
      if (n.isDirectory()) walk(full);
      else out.set(full.slice(root.length + 1), n.isFile() ? sha256(full) : 'not a file');
    }
  };
  walk(root);
  return out;
}

/** That a run changed nothing under `root`, `was` its listing before: what it added, removed or changed is named. */
function unchanged(root, was, what) {
  const now = listing(root);
  const added = [...now.keys()].filter((p) => !was.has(p));
  const removed = [...was.keys()].filter((p) => !now.has(p));
  const changed = [...now.keys()].filter((p) => was.has(p) && was.get(p) !== now.get(p));
  assert.ok(!added.length && !removed.length && !changed.length, `${what}: added ${added.join(', ') || 'nothing'}; removed ${removed.join(', ') || 'nothing'}; changed ${changed.join(', ') || 'nothing'}`);
}

/** The environment the app runs in: the check's own home, and nothing of the test hooks. */
function homeOf(label) {
  const home = join(ROOT, `home-${label}`);
  const tmp = join(ROOT, `tmp-${label}`);
  mkdirSync(home, { recursive: true });
  mkdirSync(tmp, { recursive: true });
  const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('FENNEC_')));
  if (WIN) {
    const local = join(home, 'AppData', 'Local');
    const roaming = join(home, 'AppData', 'Roaming');
    mkdirSync(local, { recursive: true });
    mkdirSync(join(roaming, 'Microsoft', 'Windows', 'Start Menu', 'Programs'), { recursive: true });
    Object.assign(env, { LOCALAPPDATA: local, APPDATA: roaming, TEMP: tmp, TMP: tmp });
    return { home, env, data: join(local, IDENTIFIER), entry: join(roaming, 'Microsoft', 'Windows', 'Start Menu', 'Programs', 'fennec studio.lnk') };
  }
  for (const k of ['XDG_DATA_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME']) delete env[k];
  Object.assign(env, { HOME: home, TMPDIR: tmp });
  return { home, env, data: join(home, '.local', 'share', IDENTIFIER), entry: join(home, '.local', 'share', 'applications', 'fennecstudio.desktop') };
}

/**
 * Starts the app at `app` by `launcher` as a user's start would, waits for it
 * to be up -- its main process and a window of the app's on screen -- then
 * stops it and requires every process it started to be gone.
 */
async function launchAndStop(app, launcher, label, env) {
  assert.equal(family(new Set(), app).length, 0, `nothing of ${app} runs before it is started`);
  const fd = openSync(join(D, 'logs', `dist-${label}.app.log`), 'w');
  const child = spawn(launcher, [], { env, cwd: dirname(launcher), stdio: ['ignore', fd, fd], windowsHide: false });
  closeSync(fd);
  started.set(child.pid, app);
  const exited = new Promise((r) => child.on('exit', (code, signal) => r({ code, signal })));
  const t0 = Date.now();
  const main = await until(() => family(new Set([child.pid]), app).find((p) => /[\\/]bin[\\/]bun(\.exe)?\b/i.test(p.command)), 90000, `the app's main process (logs/dist-${label}.app.log)`);
  const shown = await until(() => windowed(family(new Set([child.pid]), app).map((p) => p.pid)), 120000, `a window of the app on screen (logs/dist-${label}.app.log)`);
  const seen = family(new Set([child.pid]), app);
  log(`up in ${((Date.now() - t0) / 1000).toFixed(1)} s: main process ${main.pid}, a window of ${shown.pid} on screen ("${shown.title}"), ${seen.length} processes`);

  if (WIN) {
    endTree(child.pid);
  } else {
    process.kill(main.pid, 'SIGTERM');
  }
  const ids = new Set(seen.map((p) => p.pid));
  await until(() => !procs().some((p) => ids.has(p.pid)) && family(new Set([child.pid]), app).length === 0, 60000, `every process of ${app} to end`);
  const { code, signal } = await Promise.race([exited, sleep(10000).then(() => ({ code: 'still running', signal: null }))]);
  if (!WIN) assert.equal(code, 0, `the launcher ended with ${code ?? signal}, not 0: its own SIGTERM handler (logs/dist-${label}.app.log)`);
  log(`stopped${WIN ? ' (its tree ended)' : ': exit 0'}, nothing of it left`);
  started.delete(child.pid);
}

/** Runs `cmd` to its end without blocking this process's own server; its output, its status. */
function run(cmd, args, env, out) {
  return new Promise((done) => {
    const fd = openSync(out, 'w');
    const p = spawn(cmd, args, { env, stdio: ['ignore', fd, fd] });
    closeSync(fd);
    const timer = setTimeout(() => endTree(p.pid), 600000);
    p.on('exit', (code) => {
      clearTimeout(timer);
      done({ code, text: readFileSync(out, 'utf8') });
    });
  });
}

rmSync(ROOT, { recursive: true, force: true });
mkdirSync(ROOT, { recursive: true });

step('the archive');
const unpacked = join(ROOT, 'unpacked');
mkdirSync(unpacked);
said(BSDTAR, ['-xf', ARCHIVE, '-C', unpacked]);
assert.deepEqual(readdirSync(unpacked), ['fennec-studio'], 'the archive holds the one folder fennec-studio');
const app = join(unpacked, 'fennec-studio');
const launcher = join(app, 'bin', `launcher${EXE}`);
assert.ok(existsSync(launcher), `its launcher, bin/launcher${EXE}`);
assert.ok(existsSync(join(app, 'bin', `bun${EXE}`)), `its main process's runtime, bin/bun${EXE}`);
assert.ok(existsSync(join(app, 'Resources', 'app', 'bun', 'index.js')), "the app's main process, unpacked");
for (const b of ['fennec-server', 'fennec']) assert.ok(existsSync(join(app, 'Resources', 'app', 'bin', `${b}${EXE}`)), `the bundled ${b}${EXE}`);
assert.deepEqual(readdirSync(join(app, 'Resources')).filter((f) => f.endsWith('.tar.zst')), [], 'no payload left to unpack');
if (!WIN) {
  for (const f of [launcher, join(app, 'bin', 'bun'), join(app, 'Resources', 'app', 'bin', 'fennec-server')]) {
    assert.ok(statSync(f).mode & 0o100, `${f} kept its executable bit`);
  }
}
// The extractor says what it is; the app's own launcher does not. The build
// folder's launcher is the extractor, and holding it to the same words is
// what shows they are the ones to look for.
const MARKERS = ['Self-extractor', 'self-extracting', 'Installation completed successfully'];
const holds = (file) => {
  const bytes = readFileSync(file);
  return MARKERS.filter((m) => bytes.includes(m));
};
const buildDir = join(HERE, 'build', `stable-${WIN ? 'win' : 'linux'}-${ARCH}`);
const extractor = readdirSync(buildDir).map((n) => join(buildDir, n, 'bin', `launcher${EXE}`)).find((f) => existsSync(f));
assert.ok(extractor, `the build's own launcher, the extractor, under ${buildDir}`);
assert.ok(holds(extractor).length, `the build's extractor (${extractor}) holds one of ${MARKERS.join(', ')}: what the check looks for is there to find`);
assert.deepEqual(holds(launcher), [], "the archive's launcher is the app's, not the extractor");
const sizeMB = (statSync(ARCHIVE).size / 1e6).toFixed(1);
const digest = treeDigest(app);
log(`${ARCHIVE}: ${sizeMB} MB, ${digest.files} files; the launcher is the app's (the build's extractor holds ${holds(extractor).join(', ')})`);
const hooks = spawnSync(process.execPath, [join(HERE, 'scripts', 'checks', 'release.mjs'), app], { stdio: 'inherit' });
assert.equal(hooks.status, 0, 'the app in the archive holds none of the test hooks');

step('started where it was unpacked, as a browser download is');
{
  const { env } = homeOf('unpacked');
  const was = listing(app);
  await launchAndStop(app, launcher, 'unpacked', env);
  unchanged(app, was, 'the run wrote into the app');
  log(`the folder: ${digest.files} files unchanged by the run`);
}

step(`installed by ${WIN ? 'install.ps1 -App, under Windows PowerShell 5.1' : 'install.sh --app'}, from a release laid out here`);
const name = `fennec-studio-v${VERSION}-${SYS}-${ARCH}.${EXT}`;
const release = join(ROOT, 'release');
const downloads = join(release, 'download', `v${VERSION}`);
mkdirSync(downloads, { recursive: true });
copyFileSync(ARCHIVE, join(downloads, name));
// The programs' line before the app's, as a release's SHA256SUMS has them.
const programs = WIN ? `fennec-v${VERSION}-${ARCH === 'x64' ? 'x86_64' : 'aarch64'}-pc-windows-msvc.zip` : `fennec-v${VERSION}-${ARCH === 'x64' ? 'x86_64' : 'aarch64'}-unknown-linux-musl.tar.gz`;
writeFileSync(join(downloads, 'SHA256SUMS'), `${'0'.repeat(64)}  ${programs}\n${sha256(ARCHIVE)}  ${name}\n`);
const inst = homeOf('installed');
const installLog = join(D, 'logs', 'dist-install.log');
let installed;
if (WIN) {
  const env = { ...inst.env, FENNEC_FROM: downloads, FENNEC_VERSION: VERSION };
  // Windows PowerShell builds its own module path where none is given; one
  // inherited from PowerShell 7 would leave it without Get-FileHash.
  delete env.PSModulePath;
  delete env.FENNEC_BASE_URL;
  installed = await run('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', join(REPO, 'site', 'install.ps1'), '-App'], env, installLog);
} else {
  server = createServer((req, res) => {
    const file = join(release, decodeURIComponent(new URL(req.url, 'http://x').pathname));
    if (!file.startsWith(release) || !existsSync(file) || !statSync(file).isFile()) {
      res.writeHead(404).end();
      return;
    }
    res.writeHead(200, { 'content-length': statSync(file).size });
    createReadStream(file).pipe(res);
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const env = { ...inst.env, FENNEC_BASE_URL: `http://127.0.0.1:${server.address().port}`, FENNEC_VERSION: VERSION };
  installed = await run('sh', [join(REPO, 'site', 'install.sh'), '--app'], env, installLog);
  server.close();
  server = null;
}
log(installed.text.trim());
assert.equal(installed.code, 0, `the installer ended with ${installed.code} (logs/dist-install.log)`);
assert.match(installed.text, new RegExp(`Checksum ok \\(${sha256(ARCHIVE)}\\)`), 'the installer checked the sum');
const dest = join(inst.data, 'stable', 'app');
assert.match(installed.text, new RegExp(`Installed fennec studio ${VERSION.replaceAll('.', '\\.')} \\(beta\\) in ${dest.replace(/[\\^$.*+?()[\]{}|]/g, '\\$&')}`), `the installer says it put the app in ${dest}`);
assert.deepEqual(treeDigest(dest), digest, "the installed app is the archive's, file for file");
assert.deepEqual(readdirSync(join(inst.data, 'stable')).sort(), ['app'], 'beside the app, nothing of the installer left');

// What a user starts it by: the Start-menu shortcut, or the menu entry.
let target;
if (WIN) {
  const ps = `(New-Object -ComObject WScript.Shell).CreateShortcut('${inst.entry.replaceAll("'", "''")}').TargetPath`;
  assert.ok(existsSync(inst.entry), `the Start-menu shortcut, ${inst.entry}`);
  target = said(POWERSHELL, ['-NoProfile', '-NonInteractive', '-Command', ps]).trim();
} else {
  assert.ok(existsSync(inst.entry), `the menu entry, ${inst.entry}`);
  const text = readFileSync(inst.entry, 'utf8');
  assert.match(text, /^\[Desktop Entry\]$/m, 'a desktop entry');
  assert.match(text, /^Type=Application$/m, 'of an application');
  assert.match(text, /^Name=fennec studio$/m, 'named fennec studio');
  const exec = /^Exec="([^"]*)"$/m.exec(text);
  assert.ok(exec, `its Exec is one quoted path: ${text}`);
  target = exec[1];
  const validate = spawnSync('desktop-file-validate', [inst.entry], { encoding: 'utf8' });
  if (validate.error) log('(desktop-file-validate is not here: the entry is held to its lines alone)');
  else assert.equal(validate.status, 0, `desktop-file-validate passes the entry: ${validate.stdout}${validate.stderr}`);
}
assert.equal(same(target), same(join(dest, 'bin', `launcher${EXE}`)), `the ${WIN ? 'shortcut' : 'menu entry'} starts the installed launcher`);
log(`${WIN ? 'the Start-menu shortcut' : 'the menu entry'} starts ${target}`);

step('started where the installer put it, by its shortcut\'s target');
const wasInstalled = listing(dest);
await launchAndStop(dest, target, 'installed', inst.env);
unchanged(dest, wasInstalled, 'the run wrote into the installed app');
log(`the installed app: ${digest.files} files unchanged by the run`);

rmSync(ROOT, { recursive: true, force: true });
log(`\nthe ${SYS} download holds: ${ARCHIVE.split(/[\\/]/).pop()}, ${sizeMB} MB`);

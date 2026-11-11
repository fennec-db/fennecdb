// The DMG a user downloads, held as it ships: the one scripts/dist.sh made,
// whose app is the shipped app (no test hook), unpacked and signed ad hoc.
//
//   node scripts/checks/dist.mjs [the .dmg]
//     default: artifacts/dist/fennec-studio-macos-arm64.dmg
//
// What it holds, on that very file:
//   - the image verifies, is compressed, and holds the app and a link to
//     /Applications, nothing else;
//   - the app's signature holds (`codesign --verify --deep --strict`), ad hoc,
//     for the app's identifier, with its resources sealed, and so does every
//     Mach-O file in it; it is unpacked (no payload, the launcher is not the
//     extractor) and holds none of the test hooks (release.mjs);
//   - it launches from the image's own read-only mount, as a quarantined copy
//     opened where Finder did not move it runs (translocated, read-only): a
//     self-extractor could not, since it writes into its bundle. Launched means
//     the app's own code ran (the instance socket it makes first) and a window
//     of its process is on screen; it quits on SIGTERM through its own handler,
//     exit 0, every process of it gone;
//   - copied out as Finder or install.sh does (ditto), the image then lets go,
//     and the copy launches and quits the same way, every file of it unchanged
//     by the run and its signature still holding.
//
// The app is started with a HOME and a TMPDIR of the check's own, so its
// instance socket (found there) and any state it keeps are not a real
// install's, and a real install running beside it is not asked to show
// itself instead. What WebKit and macOS keep under the real ~/Library for the
// app's identifier (~/Library/WebKit/dev.fennecdb.studio, and at times
// ~/Library/Preferences/dev.fennecdb.studio.plist: both take the home from the
// system, not from HOME) is removed afterwards where this run made it, never
// where it was there before (a real install on this Mac).
import assert from 'node:assert/strict';
import { execFileSync, spawn, spawnSync } from 'node:child_process';
import { closeSync, existsSync, lstatSync, mkdirSync, openSync, readdirSync, readFileSync, readlinkSync, rmSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { D, HERE, R, log, procs, sleep, step } from './lib.mjs';
import { treeDigest } from '../tree-digest.mjs';

const DMG = process.argv[2] ?? `${HERE}artifacts/dist/fennec-studio-macos-arm64.dmg`;
const NAME = 'fennec studio.app';
const IDENTIFIER = 'dev.fennecdb.studio';
const ROOT = `${R}/dist`;
const MNT = `${ROOT}/mnt`;
const LSREGISTER = '/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister';

assert.ok(existsSync(DMG), `no DMG at ${DMG}: make it with \`make studio-desktop-dist\``);
// An image a run that was killed outright left attached goes before its folder.
if (existsSync(MNT)) spawnSync('hdiutil', ['detach', '-force', MNT], { stdio: 'ignore', timeout: 60000 });
rmSync(ROOT, { recursive: true, force: true });
mkdirSync(MNT, { recursive: true });

const run = (cmd, args) => execFileSync(cmd, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], timeout: 120000 });
/** stdout and stderr of a command that must succeed: codesign says what it found on stderr. */
function said(cmd, args) {
  const r = spawnSync(cmd, args, { encoding: 'utf8', timeout: 120000 });
  if (r.status !== 0) throw new Error(`${cmd} ${args.join(' ')} failed (${r.status ?? r.signal}):\n${r.stdout}${r.stderr}`);
  return `${r.stdout}${r.stderr}`;
}

/** The processes of the app at `app`: the launcher, Bun, and what they start from the bundle. */
const of = (app) => procs().filter((p) => p.command.startsWith(`${app}/`));

// What the real ~/Library holds for the shipped identifier: what a launch adds
// there is the check's to remove, what was there is a real install's.
function libraryOfApp() {
  const found = new Set();
  const walk = (dir, depth) => {
    let names;
    try {
      names = readdirSync(dir);
    } catch {
      return;
    }
    for (const n of names) {
      const full = join(dir, n);
      if (n === IDENTIFIER || (n.startsWith(`${IDENTIFIER}.`) && !n.startsWith(`${IDENTIFIER}.test`))) found.add(full);
      else if (depth < 2) {
        try {
          if (lstatSync(full).isDirectory()) walk(full, depth + 1);
        } catch {
          // unreadable
        }
      }
    }
  };
  walk(join(homedir(), 'Library'), 0);
  return found;
}
const before = libraryOfApp();
if (before.size) log(`the real ~/Library holds ${[...before].join(', ')} (a real install): left as it is`);

// Whatever happens, nothing of the app is left running, the image is let go,
// and what the runs added to ~/Library goes.
let mounted = false;
const started = new Set();
function tidy() {
  for (const app of started) {
    for (const p of of(app)) {
      try {
        process.kill(p.pid, 'SIGKILL');
      } catch {
        // gone
      }
    }
  }
  if (mounted) spawnSync('hdiutil', ['detach', '-force', MNT], { stdio: 'ignore', timeout: 60000 });
  const made = [...libraryOfApp()].filter((p) => !before.has(p));
  // Preferences go through cfprefsd, which would write a plist removed under
  // it again from what it holds.
  if (made.some((p) => p.endsWith(`/Preferences/${IDENTIFIER}.plist`))) spawnSync('defaults', ['delete', IDENTIFIER], { stdio: 'ignore', timeout: 30000 });
  for (const p of made) rmSync(p, { recursive: true, force: true });
  if (made.length) log(`removed what the runs added to ~/Library: ${made.map((p) => p.replace(homedir(), '~')).join(', ')}`);
}
process.on('exit', tidy);
for (const sig of ['SIGINT', 'SIGTERM']) process.on(sig, () => process.exit(1));

/** The pids of windows on screen, read through CoreGraphics (a window's owner needs no permission; its title would). */
function windowOwners() {
  const js = [
    'ObjC.import("CoreGraphics");',
    'const a = ObjC.castRefToObject($.CGWindowListCopyWindowInfo(1, 0));',
    'const out = [];',
    'for (let i = 0; i < a.count; i++) {',
    '  const w = a.objectAtIndex(i);',
    '  if (ObjC.unwrap(w.objectForKey("kCGWindowLayer")) === 0) out.push(ObjC.unwrap(w.objectForKey("kCGWindowOwnerPID")));',
    '}',
    'out.join(" ")',
  ].join('\n');
  return new Set(run('osascript', ['-l', 'JavaScript', '-e', js]).trim().split(/\s+/).filter(Boolean).map(Number));
}

async function until(fn, ms, label) {
  const t0 = Date.now();
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() - t0 > ms) throw new Error(`timeout waiting for ${label}`);
    await sleep(100);
  }
}

/**
 * Launches the app at `app` as a user's launch would start it, its home and
 * temporary folder the check's, waits for it to be up, then quits it.
 */
async function launchAndQuit(app, label) {
  assert.equal(of(app).length, 0, `nothing of ${app} runs before it is launched`);
  const home = `${ROOT}/home-${label}`;
  const tmp = `${ROOT}/tmp-${label}`;
  mkdirSync(home, { recursive: true });
  mkdirSync(tmp, { recursive: true });
  const env = { PATH: '/usr/bin:/bin:/usr/sbin:/sbin', HOME: home, TMPDIR: `${tmp}/`, LANG: process.env.LANG ?? 'en_US.UTF-8', USER: process.env.USER ?? '', LOGNAME: process.env.LOGNAME ?? '' };
  const fd = openSync(`${D}/logs/dist-${label}.app.log`, 'w');
  started.add(app);
  const child = spawn(`${app}/Contents/MacOS/launcher`, [], { env, stdio: ['ignore', fd, fd] });
  closeSync(fd);
  const exited = new Promise((r) => child.on('exit', (code, signal) => r({ code, signal })));

  // The app's own code: its first act is the socket that keeps it one instance.
  const socket = join(tmp, `${IDENTIFIER}.instance`);
  await until(() => existsSync(socket), 90000, `the app's instance socket (${socket})`);
  const main = await until(() => of(app).find((p) => p.command.startsWith(`${app}/Contents/MacOS/bun`)), 30000, "the app's main process");
  const shown = await until(() => {
    const owners = windowOwners();
    return of(app).find((p) => owners.has(p.pid));
  }, 30000, 'a window of the app on screen');
  log(`launched: main process ${main.pid}, a window of ${shown.pid} on screen`);

  // Its own handler: every server stopped (there are none), then exit 0.
  process.kill(main.pid, 'SIGTERM');
  await until(() => of(app).length === 0, 30000, `every process of ${app} to end`);
  const { code, signal } = await Promise.race([exited, sleep(10000).then(() => ({ code: 'still running', signal: null }))]);
  assert.equal(code, 0, `the launcher ended with ${code ?? signal}, not 0 (logs/dist-${label}.app.log)`);
  log('quit: exit 0, nothing of it left');
  started.delete(app);
}

step('the image');
said('hdiutil', ['verify', '-quiet', DMG]);
const info = run('hdiutil', ['imageinfo', DMG]);
const format = /^Format: (\S+)/m.exec(info)?.[1];
assert.equal(format, 'ULMO', `the image is LZMA-compressed (ULMO), not ${format}`);
said('hdiutil', ['attach', '-quiet', '-readonly', '-nobrowse', '-noautoopen', '-mountpoint', MNT, DMG]);
mounted = true;
const shown = readdirSync(MNT).filter((n) => !n.startsWith('.')).sort();
assert.deepEqual(shown, ['Applications', NAME], `the volume holds the app and the Applications link alone: ${shown.join(', ')}`);
assert.ok(lstatSync(`${MNT}/Applications`).isSymbolicLink() && readlinkSync(`${MNT}/Applications`) === '/Applications', 'Applications is a link to /Applications');
log(`${DMG}: ${format}, ${(lstatSync(DMG).size / 1e6).toFixed(1)} MB, the app and a link to /Applications`);

step('the app, as it is in the image');
const app = `${MNT}/${NAME}`;
said('codesign', ['--verify', '--deep', '--strict', '--verbose=2', app]);
const sig = said('codesign', ['-dv', '--verbose=2', app]);
assert.match(sig, new RegExp(`^Identifier=${IDENTIFIER.replaceAll('.', '\\.')}$`, 'm'), 'signed for the app identifier');
assert.match(sig, /^Signature=adhoc$/m, 'signed ad hoc');
assert.match(sig, /^Sealed Resources version=2 /m, 'its resources sealed');
assert.doesNotMatch(sig, /linker-signed/, 'a signature of its own, not the linker');
const machO = run('find', [app, '-type', 'f'])
  .split('\n')
  .filter(Boolean)
  .filter((f) => /^Mach-O/.test(run('file', ['-b', f])));
for (const f of machO) said('codesign', ['--verify', '--strict', f]);
log(`signature holds: ad hoc, ${IDENTIFIER}, resources sealed; ${machO.length} Mach-O files each verify`);
assert.deepEqual(readdirSync(`${app}/Contents/Resources`).filter((f) => f.endsWith('.tar.zst')), [], 'no payload left to unpack');
assert.ok(existsSync(`${app}/Contents/Resources/app/bun/index.js`), "the app's main process is there, unpacked");
assert.ok(!readFileSync(`${app}/Contents/MacOS/launcher`).includes('Self-extractor'), 'the launcher is the app\'s, not the extractor');
const hooks = spawnSync(process.execPath, [`${HERE}scripts/checks/release.mjs`, app], { stdio: 'inherit' });
assert.equal(hooks.status, 0, 'the app in the image holds none of the test hooks');

step('launched from the image, read-only, as a translocated copy runs');
await launchAndQuit(app, 'image');

step('copied out, as Finder or install.sh does');
const copy = `${ROOT}/Applications/${NAME}`;
mkdirSync(`${ROOT}/Applications`, { recursive: true });
said('ditto', [app, copy]);
// LaunchServices noted the image's app as it ran; it is forgotten before the image goes.
spawnSync(LSREGISTER, ['-u', app], { stdio: 'ignore', timeout: 60000 });
said('hdiutil', ['detach', '-quiet', MNT]);
mounted = false;
log('the image let go');
const digest = treeDigest(copy);
said('codesign', ['--verify', '--deep', '--strict', copy]);
await launchAndQuit(copy, 'copy');
const after = treeDigest(copy);
assert.deepEqual(after, digest, 'the run wrote nothing into the app');
said('codesign', ['--verify', '--deep', '--strict', copy]);
log(`the copy: ${digest.files} files unchanged by the run, its signature holding`);

// LaunchServices noted the copy as it ran; it is forgotten before it goes.
spawnSync(LSREGISTER, ['-u', copy], { stdio: 'ignore', timeout: 60000 });
rmSync(ROOT, { recursive: true, force: true });
log('\nthe shipped DMG holds');

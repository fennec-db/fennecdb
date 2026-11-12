// A .fennec file opened from the desktop: double-clicked in Finder, Explorer
// or a file manager.
//
// macOS: LaunchServices starts the app and hands it the file, which arrives as
// an `open-url` event with a file: URL (src/bun/index.ts). `open -a <app>
// <file>` is that same path, and `--env` is how the hooks reach an app
// LaunchServices started. The file association itself (Info.plist's
// CFBundleDocumentTypes) is Electrobun's, from app.fileAssociations; this
// holds that what arrives is opened, spaces and non-ASCII letters in the path
// included.
//
// Windows and Linux: the app registers itself at each start
// (src/bun/associate.ts), and the system starts its launcher with the file,
// which the app reads back from the launcher (src/bun/handover.ts). Driven as
// close to a double-click as a runner allows: the shell's own ShellExecuteEx on
// the file on Windows (Explorer's double-click), `gio open` on Linux (GNOME's
// Files: GDesktopAppInfo and shared-mime-info). In order:
//   - the control: with the registration taken away as the README tells a user
//     to, the same double-click starts nothing (so what follows is the
//     registration's doing, not a default the system had);
//   - a start of the app writes it, as the system then reads it back;
//   - with the app not running, a double-click starts it with the file, which
//     opens (spaces and non-ASCII letters in its path), and no main window over
//     it, and what reached the main process is said (its own arguments are the
//     launcher's dropped ones; the file is in the launcher's);
//   - with the app running, a second file's double-click is handed by the
//     instance it starts to the one there, which opens it, and that instance
//     quits;
//   - a `.fennec` that is not a database is said in words and nothing is
//     written beside it;
//   - Linux: the app run from a folder whose name would run a command in a
//     shell registers itself there, and a double-click starts that copy with
//     the file, the name whole, and runs nothing; that copy removed, its entry
//     is ignored (TryExec) and the double-click starts nothing;
//   - the registration removed again, and the double-click starts nothing.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { closeSync, copyFileSync, existsSync, mkdirSync, openSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import {
  APP, CONTROL, D, EXE, LINUX, MAC, R, REPORT, WIN, assertClean, cmd, data, describe, doubleClick, forgetRegistration, lines, logName, mine, procs, quit, registration, startApp, step, until, waitFor, waitNotice, waitStudio, log,
} from './lib.mjs';

assertClean('before');
const original = data('finder', 20);
mkdirSync(`${R}/data/with space`, { recursive: true });
const FILE = join(R, 'data', 'with space', 'şirket 1.fennec');
copyFileSync(original, FILE);

/** Whether a launcher was shown over the file (none is left), after `after` reports. */
async function noLauncherOver(after = 0) {
  await until(async () => {
    const n = lines().length;
    cmd({ cmd: 'sessions' });
    const seen = await waitFor((l, i) => i >= n && l.what === 'sessions', 5000, 'sessions');
    return !seen.pages.includes('launcher');
  }, 15000, 'no launcher window over the file');
  return lines().some((l, i) => i >= after && l.what === 'window-created' && l.key === 'launcher');
}

if (MAC) {
  /**
   * `open -a <the app> <file>`, the app not running. With `hold`, the test
   * variant keeps the file back until the launcher is up, as a slow launch
   * delivers it after the 400 ms the app waits for it.
   */
  async function launch(hold) {
    logName('finder');
    rmSync(REPORT, { force: true });
    writeFileSync(CONTROL, '');
    const env = ['--env', `FENNEC_DESKTOP_REPORT=${REPORT}`, '--env', `FENNEC_DESKTOP_CONTROL=${CONTROL}`, ...(hold ? ['--env', 'FENNEC_DESKTOP_HOLD_OPEN_URL=launcher'] : [])];
    const r = spawnSync('open', ['-a', APP, ...env, FILE], { encoding: 'utf8' });
    assert.equal(r.status, 0, r.stderr);
    const url = await waitFor((l) => l.what === 'open-url', 30000, 'the open-url event');
    log('arrived as', url.url);
    assert.ok(url.url.startsWith('file://'));
    const started = await waitFor((l) => l.what === 'server-started', 30000, 'the server for it');
    assert.equal(started.path, FILE);
    await waitStudio(`file:${FILE}`);
    // With a file to show, no launcher over it: none was shown, or, if the file was
    // handed over after it had been (a slow launch), it is closed once it is.
    return noLauncherOver();
  }

  step('open -a <the app> <file>, the app not running');
  // Whether the launcher was shown first depends on how slow the launch was; that none is left over the file does not.
  await launch(false);
  await quit({ exited: Promise.resolve() });

  step('the same, the file handed over late: the launcher was shown first, and is closed when the file comes');
  assert.equal(await launch(true), true, 'the launcher was up before the file came (the test holds it back until it is)');
  step('quit');
  await quit({ exited: Promise.resolve() });
  log('clean');
  process.exit(0);
}

/** The app's main processes (its Bun running main.js) now. */
const mains = () => mine().filter((p) => /main\.js/.test(p.command));

/**
 * A double-click that must start nothing of the app. Linux's `gio open` says no
 * (no program for the type). Windows hands a file of no known type to "Open
 * with" (the Unknown class's openas verb: OpenWith.exe, the "How do you want to
 * open this file?" dialog), so there that is what must start: the event that
 * the shell found no handler, which is then closed.
 */
async function refused(path, why) {
  rmSync(REPORT, { force: true });
  const before = new Set(WIN ? procs().map((p) => p.pid) : []);
  const r = doubleClick(path);
  log(`the double-click: status ${r.status}${r.said ? `: ${r.said.split('\n').slice(-2).join(' ')}` : ''}`);
  if (WIN && r.status === 0) {
    const asked = await until(() => procs().find((p) => !before.has(p.pid) && /\\OpenWith\.exe/i.test(p.command)), 30000, `${why}: Windows asking what to open the file with`);
    log(`Windows asks what to open it with: ${asked.command}`);
    for (const p of procs().filter((q) => !before.has(q.pid) && /\\OpenWith\.exe/i.test(q.command))) {
      try {
        process.kill(p.pid, 'SIGKILL');
      } catch {
        // gone
      }
    }
  } else {
    assert.notEqual(r.status, 0, `${why}: the system refuses the double-click (${r.said})`);
  }
  // The system has said what it does with the file; the app would have said it was started first.
  assert.equal(lines().filter((l) => l.what === 'launch').length, 0, `${why}: the app was started`);
  assert.equal(mains().length, 0, `${why}: nothing of the app runs:\n${describe(mine())}`);
}

/** Starts the app once, from `exe`, until it has registered itself, and quits it; what it registered. */
async function registerFrom(name, exe = EXE) {
  const app = exe === EXE ? startApp(name) : startCopy(name, exe);
  const done = await waitFor((l) => l.what === 'registered', 60000, 'the registration');
  assert.equal(done.error, undefined, `the registration failed: ${done.error}`);
  await waitFor((l) => l.what === 'ready', 60000, 'the app up');
  await quit(app);
  return done;
}

/** A launcher other than the installed one, started as startApp starts that one. */
function startCopy(name, exe) {
  logName(name);
  rmSync(REPORT, { force: true });
  writeFileSync(CONTROL, '');
  const fd = openSync(`${D}/logs/${name}.app.log`, 'w');
  const child = spawn(exe, [], { env: { ...process.env, FENNEC_DESKTOP_REPORT: REPORT, FENNEC_DESKTOP_CONTROL: CONTROL }, stdio: ['ignore', fd, fd] });
  closeSync(fd);
  return { child, exited: new Promise((r) => child.on('exit', (code, signal) => r({ code, signal }))), name };
}

step('the control: no registration, and the double-click starts nothing');
log('taken away:', forgetRegistration().join(', ') || 'nothing was there');
await refused(FILE, 'with no registration');

step('a start of the app registers it');
const done = await registerFrom('finder-register');
log('wrote:', done.wrote.join(', '));
for (const r of done.ran ?? []) log(`ran ${r.argv.join(' ')}: ${r.result}`);
if (done.skipped) log('skipped:', done.skipped);
assert.ok(done.wrote.length > 0, 'a first start writes the registration');
const held = registration(FILE);
log('the system holds:', JSON.stringify(held, null, 1));
if (LINUX) {
  assert.match(held.desktop ?? '', /^MimeType=application\/x-fennecdb;$/m);
  assert.match(held.desktop ?? '', /^Exec=".*\/bin\/launcher" %f$/m);
  assert.match(held.desktop ?? '', /^TryExec=.*\/bin\/launcher$/m);
  assert.match(held.mime ?? '', /<glob pattern="\*\.fennec"\/>/);
  assert.equal(held.type, 'application/x-fennecdb', 'GIO reads a .fennec file as the type');
  for (const r of done.ran) assert.match(r.result, /^(exit 0|not found)$/, `${r.argv[0]}: ${r.result}`);
} else {
  assert.equal(held.command, `"${EXE}" "%1"`);
  assert.ok(held.listed, '.fennec lists the ProgID');
  assert.equal(held.default, `dev.fennecdb.studio.test.fennec`, '.fennec opens with it (nothing else had it here)');
  assert.ok(held.icon && existsSync(held.icon), `its icon is there: ${held.icon}`);
}
step('a second start writes nothing: the registration is what it would write');
assert.deepEqual((await registerFrom('finder-register-again')).wrote, []);

step('a double-click, the app not running: it starts with the file');
logName('finder-double-click');
rmSync(REPORT, { force: true });
writeFileSync(CONTROL, '');
const first = doubleClick(FILE);
assert.equal(first.status, 0, `the double-click: ${first.said}`);
const launch = await waitFor((l) => l.what === 'launch', 60000, 'the app, started by the double-click');
log('what reached the main process:', JSON.stringify({ argv: launch.argv, ppid: launch.ppid, launcherPid: launch.launcherPid, launched: launch.launched, handed: launch.handed }));
log('its environment holds:', launch.env.join(' '));
assert.equal(launch.argv.length, 2, `its own arguments are Bun's and main.js alone: the launcher drops the file (${launch.argv.join(' ')})`);
assert.ok(!launch.argv.some((a) => a.includes('şirket')), 'the file is not among its own arguments');
assert.deepEqual(launch.handed, [FILE], 'the file, read back from the launcher, whole');
assert.equal(String(launch.ppid), String(launch.launcherPid), 'its parent is the launcher that says it started it');
const started = await waitFor((l) => l.what === 'server-started', 60000, 'the server for it');
assert.equal(started.path, FILE);
await waitStudio(`file:${FILE}`);
assert.equal(await noLauncherOver(), false, 'no main window was shown for a launch that brought a file');
log('opened, and no main window over it');

step('a second file double-clicked while the app runs: handed to the one there, and the instance it started quits');
const OTHER = data('finder-other', 5);
const n = lines().length;
const second = doubleClick(OTHER);
assert.equal(second.status, 0, `the double-click: ${second.said}`);
const told = await waitFor((l, i) => i >= n && l.what === 'instance-asked', 60000, 'the running instance asked');
assert.deepEqual(told.ask, { open: [OTHER] });
await waitFor((l, i) => i >= n && l.what === 'second-instance', 30000, 'the second instance told to quit');
const opened = await waitFor((l, i) => i >= n && l.what === 'server-started', 60000, 'the second file opened');
assert.equal(opened.path, OTHER);
await waitStudio(`file:${OTHER}`, n);
await until(() => mains().length === 1, 30000, 'one main process: the second instance is gone');

step('a .fennec that is not a database: said in words, nothing written beside it');
const NOTES = join(R, 'data', 'notes.fennec');
writeFileSync(NOTES, 'not a database\n');
const before = readdirSync(join(R, 'data')).sort();
const k = lines().length;
assert.equal(doubleClick(NOTES).status, 0);
await waitNotice('That is not a fennecdb database', 60000, k);
assert.deepEqual(readdirSync(join(R, 'data')).sort(), before, 'nothing written beside it');
await quit({ exited: Promise.resolve() });

if (LINUX) {
  step('Linux: the app run from a folder whose name a shell would run: registered there, started there by a double-click, the name whole');
  // A hard-linked copy of the installed app (no second 100 MB): its launcher's own path is the
  // folder's, which the registration names. The marker is what `$(...)` would make in a shell.
  const marker = `fennec-finder-marker-${process.pid}`;
  // No backslash: Bun 1.4.0 reads one in its script's path as a separator, and cannot start from such a folder at all.
  const odd = join(R, `odd "q" 'a' \`id\` $(cd$IFS~;touch$IFS${marker}) 50%f`);
  rmSync(odd, { recursive: true, force: true });
  mkdirSync(odd, { recursive: true });
  const copied = spawnSync('cp', ['-al', APP, join(odd, 'app')], { encoding: 'utf8' });
  assert.equal(copied.status, 0, copied.stderr);
  const exe = join(odd, 'app', 'bin', 'launcher');
  try {
    const there = await registerFrom('finder-odd', exe);
    assert.ok(there.wrote.some((w) => w.endsWith('.desktop')), 'the entry is pointed at the copy');
    log('Exec:', /^Exec=.*$/m.exec(registration(null).desktop ?? '')?.[0]);
    rmSync(REPORT, { force: true });
    writeFileSync(CONTROL, '');
    assert.equal(doubleClick(FILE).status, 0);
    const l = await waitFor((x) => x.what === 'launch', 60000, 'the copy, started by the double-click');
    assert.equal(l.launched.argv?.[0], exe, 'the copy\'s launcher, by its whole name');
    assert.deepEqual(l.handed, [FILE]);
    await waitFor((x) => x.what === 'server-started' && x.path === FILE, 60000, 'the file opened');
    await quit({ exited: Promise.resolve() });
    assert.ok(!existsSync(join(homedir(), marker)), 'nothing ran the folder\'s name');

    step('Linux: that copy removed, its entry (TryExec) is ignored: the double-click starts nothing, rather than a launcher that is gone');
    rmSync(odd, { recursive: true, force: true });
    await refused(FILE, 'with the launcher the entry names gone');
  } finally {
    rmSync(join(homedir(), marker), { force: true });
    rmSync(odd, { recursive: true, force: true });
  }
  step('the installed app started again: the entry points back at it');
  assert.ok((await registerFrom('finder-back')).wrote.some((w) => w.endsWith('.desktop')));
}

step('the registration removed as the README says: the double-click starts nothing again');
log('taken away:', forgetRegistration().join(', '));
await refused(FILE, 'after the removal');
log('clean');

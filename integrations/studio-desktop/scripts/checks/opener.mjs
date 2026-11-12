// No link or folder the app hands the system goes through a shell (Linux; the
// spike's `linux-open-external` probe, inverted). Electrobun 2.0.4's Linux
// `openExternal` falls back to `system("xdg-open \"<url>\"")` and its
// `showItemInFolder` always runs `system("xdg-open \"<file URI>\"")`; a link
// holding `$(touch ...)` made the spike's marker through them. The app calls
// neither on Linux: it starts xdg-open itself, the address one argument, made
// inert besides (src/bun/opener.ts). Here a link and a folder whose names run
// `touch` in any shell they reach are opened:
//   - the control: the same link through a shell, as Electrobun puts it, makes
//     the marker, so the payload is live;
//   - with a stand-in xdg-open first on PATH, which records what it is handed:
//     one argument, the address as the app encodes it, and no marker -- a shell
//     between the app and the stand-in would have run the command before it;
//   - with the real xdg-open, the default browser and file manager programs
//     that cannot start (and $BROWSER /bin/false, so nothing opens on the
//     runner): xdg-open does what it does with it, and no marker.
// The test variant hands a link to the system only when FENNEC_DESKTOP_REAL_OPEN
// is set, and reports what the opener did (`external-done`, `revealed`).
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { D, assertClean, cmd, lines, quit, startApp, step, waitFor, log } from './lib.mjs';

assertClean('before');
const MARK = `fennec-opener-marker-${process.pid}`;
const marker = join(homedir(), MARK);
const made = () => existsSync(marker);
rmSync(marker, { force: true });
// A link names the marker by its path; a folder's name cannot hold a `/`, so its command goes home first.
const LINK = `https://example.invalid/?q=$(touch$IFS${marker})`;
const FOLDER = join(D, 'data', `x$(cd$IFS~;touch$IFS${MARK})`);
mkdirSync(FOLDER, { recursive: true });
const DB = join(FOLDER, 'db.fennec');
writeFileSync(DB, '');

// What the app hands over, worked out here apart from its code: the link with the
// characters a shell reads inside quotes percent-encoded, the folder as a file: URI
// with every byte but letters, digits, -._~ and / percent-encoded.
const LINK_HANDED = new URL(LINK).href.replace(/[$`\\"']/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`);
const pct = (s) => encodeURIComponent(s).replace(/[!*'()]/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`);
const FOLDER_HANDED = `file://${FOLDER.split('/').map(pct).join('/')}`;

// The stand-in xdg-open: each call's arguments, a line each, in a file of its own.
const stub = join(D, 'stub-bin');
const calls = join(D, 'stub-calls');
mkdirSync(stub, { recursive: true });
const fresh = () => {
  rmSync(calls, { recursive: true, force: true });
  mkdirSync(calls);
};
fresh();
writeFileSync(join(stub, 'xdg-open'), `#!/bin/sh\nprintf '%s\\n' "$@" > '${calls}/'$$\n`);
chmodSync(join(stub, 'xdg-open'), 0o755);
const handed = () => readdirSync(calls).map((f) => readFileSync(join(calls, f), 'utf8'));
const STUB_PATH = `${stub}:${process.env.PATH}`;

step('the control: the link through a shell, as Electrobun hands it to one, makes the marker');
spawnSync('sh', ['-c', `xdg-open "${LINK}"`], { env: { ...process.env, PATH: STUB_PATH }, stdio: 'ignore' });
assert.ok(made(), 'a shell runs what the link holds: the payload is live');
rmSync(marker);
fresh();

step('the app, with a stand-in xdg-open: one argument, the address as the app encodes it, and no marker');
let app = startApp('opener', { FENNEC_DESKTOP_REAL_OPEN: '1', PATH: STUB_PATH, FENNEC_DESKTOP_LAUNCHER: '1' });
await waitFor((l) => l.what === 'ready', 60000, 'ready');
let n = lines().length;
cmd({ cmd: 'open-link', url: LINK });
let done = await waitFor((l, i) => i >= n && l.what === 'external-done', 30000, 'the link handed over');
log('the link:', JSON.stringify(done.done));
assert.deepEqual(done.done.argv, ['xdg-open', LINK_HANDED], 'xdg-open, by name, the link one argument');
assert.deepEqual(handed(), [`${LINK_HANDED}\n`], 'what xdg-open was handed is that argument and nothing else');
assert.ok(!made(), 'no shell ran what the link holds');
fresh();
n = lines().length;
cmd({ cmd: 'reveal', path: DB });
done = await waitFor((l, i) => i >= n && l.what === 'revealed', 30000, 'the folder handed over');
log('the folder:', JSON.stringify(done.done));
assert.deepEqual(done.done.argv, ['xdg-open', FOLDER_HANDED]);
assert.deepEqual(handed(), [`${FOLDER_HANDED}\n`]);
assert.ok(!made(), 'no shell ran what the folder\'s name holds');
await quit(app);

step('the app, with the real xdg-open and nothing it can start: still no marker');
// A default browser and file manager that cannot start, and $BROWSER that fails at once: what xdg-open does then, with nothing opened on the runner.
const xdg = join(D, 'xdg');
mkdirSync(join(xdg, 'share', 'applications'), { recursive: true });
mkdirSync(join(xdg, 'config'), { recursive: true });
writeFileSync(join(xdg, 'share', 'applications', 'gone.desktop'), '[Desktop Entry]\nType=Application\nName=Gone\nExec=/nonexistent/opener %u\nMimeType=x-scheme-handler/http;x-scheme-handler/https;inode/directory;\n');
const defaults = '[Default Applications]\nx-scheme-handler/http=gone.desktop\nx-scheme-handler/https=gone.desktop\ninode/directory=gone.desktop\n';
writeFileSync(join(xdg, 'config', 'mimeapps.list'), defaults);
// xdg-utils 1.1.3 without a desktop takes a data folder's mimeinfo.cache over the mimeapps.list
// above, and the system's lists the runner's Firefox once desktop-file-utils is installed: the
// folder before it says the same (seen: Firefox started, and xdg-open waited on it).
writeFileSync(join(xdg, 'share', 'applications', 'defaults.list'), defaults);
writeFileSync(join(xdg, 'share', 'applications', 'mimeinfo.cache'), defaults.replace('[Default Applications]', '[MIME Cache]').replace(/\.desktop\n/g, '.desktop;\n'));
const realEnv = {
  BROWSER: '/bin/false',
  XDG_CURRENT_DESKTOP: '',
  XDG_DATA_DIRS: `${join(xdg, 'share')}:${process.env.XDG_DATA_DIRS || '/usr/local/share:/usr/share'}`,
  XDG_CONFIG_HOME: join(xdg, 'config'),
};
// What xdg-open will pick, asked first: a user's own cache (~/.local/share comes first) could name a browser.
const picked = spawnSync('xdg-mime', ['query', 'default', 'x-scheme-handler/https'], { encoding: 'utf8', env: { ...process.env, ...realEnv } });
assert.equal(picked.stdout.trim(), 'gone.desktop', `xdg-open would start what it picks for a link: ${picked.stdout.trim() || picked.stderr.trim()}`);
app = startApp('opener-real', { FENNEC_DESKTOP_REAL_OPEN: '1', FENNEC_DESKTOP_LAUNCHER: '1', ...realEnv });
await waitFor((l) => l.what === 'ready', 60000, 'ready');
n = lines().length;
cmd({ cmd: 'open-link', url: LINK });
done = await waitFor((l, i) => i >= n && l.what === 'external-done', 60000, 'the link handed over');
log('the link, the real opener:', JSON.stringify(done.done));
assert.ok(!made(), 'no shell ran what the link holds');
n = lines().length;
cmd({ cmd: 'reveal', path: DB });
done = await waitFor((l, i) => i >= n && l.what === 'revealed', 60000, 'the folder handed over');
log('the folder, the real opener:', JSON.stringify(done.done));
assert.ok(!made(), 'no shell ran what the folder\'s name holds');
await quit(app);
rmSync(marker, { force: true });
log('clean');

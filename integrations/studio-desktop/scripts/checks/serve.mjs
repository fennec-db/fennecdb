// Serving a database to the apps on this Mac: the serve sheet, a server on a
// port, its token in the Keychain and in no file, argument or URL, a window on
// that server rather than a second one, start and stop as the server's own
// shutdown, its output, what another program holding the file or the port
// does to it and how the Servers list keeps saying so (a callout, the
// server's own line), the notice's fold of what the server said, a server
// that dies, and what a relaunch starts: nothing.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { basename } from 'node:path';
import {
  assertClean, cmd, credential, data, evalIn, forgetServeItems, lines, mine, pressNotice, procs, quit, sleep, startApp, startServer, step, until, USER_DATA, waitFor, waitNone, waitNotice, waitStudio, log,
} from './lib.mjs';

assertClean('before');
const FILE = data('served', 40);
const OTHER = data('other', 5);
const HELD = data('held-by-another', 5);

const state = () => JSON.parse(readFileSync(`${USER_DATA}/state.json`, 'utf8'));
const entry = (path) => state().databases.find((d) => d.path === path);
/** The system's credential store holds item `name` of the test variant's (lib.mjs). */
const keychainHas = (name) => credential(name).there;
/** What the app answered to `c`, by the report it writes for it. */
async function ask(c, what, ms = 30000) {
  const n = lines().length;
  cmd(c);
  return waitFor((l, i) => i >= n && l.what === what && (l.path === undefined || c.path === undefined || l.path === c.path), ms, what);
}
const status = async (url, token) => (await fetch(url, { headers: token ? { authorization: `Bearer ${token}` } : {} })).status;
/** The fennec-server processes on `path` (`fennec-server.exe` on Windows): the app's (under its bundle) and any other. */
const serversOn = (path) => procs().filter((p) => /fennec-server(\.exe)? /.test(p.command) && p.command.includes(`--file ${path} `));
const freePort = () =>
  new Promise((resolve) => {
    const s = createServer().listen(0, '127.0.0.1', () => {
      const { port } = s.address();
      s.close(() => resolve(port));
    });
  });

let app = startApp('serve', { FENNEC_DESKTOP_LAUNCHER: '1' });
await waitFor((l) => l.what === 'ready', 60000, 'ready');
await waitFor((l) => l.what === 'page-text' && l.key === 'launcher' && /Databases/.test(l.text), 20000, 'the main window drawn');
const sections = async () => (await evalIn('launcher', `return [...document.querySelectorAll('.view')].filter((v) => getComputedStyle(v).display !== 'none').map((v) => v.id)`)).value;
assert.deepEqual(await sections(), ['databases'], 'one section drawn at a time: a hidden one is not shown by its own display rule');
// The file is in the list once it was opened: open it, close it.
let n = lines().length;
cmd({ cmd: 'open', path: FILE });
await waitStudio(`file:${FILE}`, n);
cmd({ cmd: 'close', key: `file:${FILE}` });
await waitFor((l, i) => i >= n && l.what === 'server-stopped' && l.key === `file:${FILE}`, 15000, 'its window\'s server stopped');

step('the serve sheet, from the Databases list: the command it shows holds no token, and Show shows the Keychain\'s');
// The row by its path compared as a string: in a selector a Windows path's `\f` is a form feed
// once in the script, which no CSS string holds, so querySelector threw, and the error's text was
// taken for the row found. Only `true` is the row.
const theRow = `[...document.querySelectorAll('tr[data-path]')].find((t) => t.dataset.path === ${JSON.stringify(FILE)})`;
await until(async () => (await evalIn('launcher', `return ${theRow} !== undefined`)).value === true, 10000, 'the row');
const clicked = await evalIn('launcher', `[...${theRow}.querySelectorAll('button')].find((b) => b.textContent === 'Serve…').click(); return true`);
assert.equal(clicked.value, true, `Serve… clicked: ${clicked.value}`);
const command = await until(async () => (await evalIn('launcher', `return document.getElementById('sheet').open && document.getElementById('command').textContent`)).value, 10000, 'the sheet open, with its command');
// No backups set up for it: no replication token, which would answer /_changes to the data token too.
assert.match(command, /^FENNEC_HTTP_TOKEN=•••• fennec-server --file /);
assert.match(command, /--http 127\.0\.0\.1:0/);
assert.match(command, /--exit-on-stdin-close$/);
await evalIn('launcher', `document.getElementById('tok-show').click()`);
const shown = await until(async () => {
  const t = (await evalIn('launcher', `return document.getElementById('tok-shown').textContent`)).value;
  return /^[A-Za-z0-9_-]{43}$/.test(t) && t;
}, 10000, 'the token shown');
const TOKEN = (await ask({ cmd: 'serve-token', path: FILE }, 'serve-token')).token;
assert.equal(shown, TOKEN, 'what Show shows is what the Keychain keeps');
const id = entry(FILE).id;
assert.ok(keychainHas(`serve:${id}`), 'kept in the Keychain under serve:<id>, before any server has it');
assert.ok(!readFileSync(`${USER_DATA}/state.json`, 'utf8').includes(TOKEN), 'state.json holds no token');

step('Start server: any free port, this Mac, the token from the Keychain, no second server');
n = lines().length;
await evalIn('launcher', `document.getElementById('sheet-start').click()`);
const started = await waitFor((l, i) => i >= n && l.what === 'served' && l.path === FILE, 30000, 'served');
assert.match(started.local, /^http:\/\/127\.0\.0\.1:\d+$/);
assert.equal(started.origin, started.local);
assert.equal(await status(`${started.local}/_whoami`, TOKEN), 200, 'an app with the token gets in');
assert.equal(await status(`${started.local}/_whoami`), 401, 'one without it does not');
assert.ok(!procs().some((p) => p.command.includes(TOKEN)), 'the token is in no process\'s arguments');
assert.equal(serversOn(FILE).length, 1, 'one server on the file');
assert.deepEqual(entry(FILE).serve, { port: null, reach: 'mac', studio: true, compact: true, mcp: false, metrics: false });
await waitFor((l, i) => i >= n && l.what === 'page-text' && l.key === 'launcher' && /Running for/.test(l.text), 15000, 'the Servers list shows it running');
assert.deepEqual(await sections(), ['servers'], 'the sheet hands over to the Servers list, alone');

step('Open: a window on the served server, not a second server; closing it leaves the server running');
n = lines().length;
cmd({ cmd: 'open', path: FILE });
const onServed = await waitFor((l, i) => i >= n && (l.what === 'window-on-served' || l.what === 'server-started') && l.path === FILE, 20000, 'the window');
assert.equal(onServed.what, 'window-on-served');
assert.equal(onServed.origin, started.local);
const loaded = await waitStudio(`file:${FILE}`, n);
assert.equal(loaded.detail.signin, false, 'signed in by the app, with the served token');
assert.match(loaded.detail.side, /orders 40/);
assert.equal(serversOn(FILE).length, 1, 'still one server on the file');
cmd({ cmd: 'close', key: `file:${FILE}` });
await waitFor((l, i) => i >= n && l.what === 'window-close' && l.key === `file:${FILE}`, 10000, 'the window closed');
await waitNone((l, i) => i >= n && (l.what === 'served-stopped' || l.what === 'server-stopped') && (l.path === FILE || l.key === `file:${FILE}`), 1500, 'the server stopped with the window');
assert.equal(await status(`${started.local}/_whoami`, TOKEN), 200, 'the apps still reach it');
assert.equal((await ask({ cmd: 'serve', path: FILE }, 'serve-result')).ok, false, 'served already: not twice');

step('its output, and its figures from its own metrics');
const out = await ask({ cmd: 'server-log', path: FILE }, 'server-log');
assert.ok(out.lines.some((l) => /^fennec-http \S+ listening on: http:\/\/127\.0\.0\.1:/.test(l)), out.lines.join('\n'));
assert.ok(!out.lines.join('\n').includes(TOKEN), 'no token in its output');
const fig = (await ask({ cmd: 'figures', path: FILE }, 'figures')).figures;
assert.equal(fig.live, true);
// mkdata.mjs makes the orders and the collections beside them.
assert.ok(fig.rows >= 40 && fig.collections >= 2, JSON.stringify(fig));
assert.ok(fig.fileBytes > 0);

step('Restart with other settings: a port asked for, the network; the token is the same, so the apps keep working');
const PORT = await freePort();
n = lines().length;
assert.equal((await ask({ cmd: 'restart-served', path: FILE, settings: { port: PORT, reach: 'lan', metrics: true } }, 'restart-served-result')).ok, true);
const again = await waitFor((l, i) => i >= n && l.what === 'served' && l.path === FILE, 30000, 'served again');
assert.equal(again.origin, `http://0.0.0.0:${PORT}`);
assert.equal(again.local, `http://127.0.0.1:${PORT}`);
assert.match(again.metrics, /^http:\/\/127\.0\.0\.1:\d+$/);
assert.ok(lines().slice(n).some((l) => l.what === 'served-stopped' && l.path === FILE && l.code === 0), 'the first stopped with exit 0');
assert.equal(await status(`${again.local}/_whoami`, TOKEN), 200, 'the same token');
assert.equal(await status(`${again.metrics}/_metrics`, TOKEN), 200, 'its metrics on a port of their own');
assert.ok(serversOn(FILE)[0].command.includes(`--http 0.0.0.0:${PORT}`));

step('New token: kept now, taken at the next start; the old one is refused after it');
assert.equal((await ask({ cmd: 'new-token', path: FILE }, 'new-token-result')).ok, true);
const NEW = (await ask({ cmd: 'serve-token', path: FILE }, 'serve-token')).token;
assert.notEqual(NEW, TOKEN);
assert.equal(await status(`${again.local}/_whoami`, TOKEN), 200, 'the running server has the old one until it starts again');
n = lines().length;
await ask({ cmd: 'restart-served', path: FILE }, 'restart-served-result');
const third = await waitFor((l, i) => i >= n && l.what === 'served' && l.path === FILE, 30000, 'served a third time');
assert.equal(third.port, PORT, 'with the settings it last ran with');
assert.equal(await status(`${third.local}/_whoami`, TOKEN), 401);
assert.equal(await status(`${third.local}/_whoami`, NEW), 200);

step('a port another program answers on is taken, said so, with the way to change it');
const blocker = createServer((c) => c.end());
await new Promise((r) => blocker.listen(0, '127.0.0.1', r));
const TAKEN = blocker.address().port;
n = lines().length;
cmd({ cmd: 'open', path: OTHER });
await waitStudio(`file:${OTHER}`, n);
cmd({ cmd: 'close', key: `file:${OTHER}` });
await waitFor((l, i) => i >= n && l.what === 'server-stopped' && l.key === `file:${OTHER}`, 15000, 'stopped');
n = lines().length;
cmd({ cmd: 'serve', path: OTHER, settings: { port: TAKEN } });
const taken = await waitNotice(`Port ${TAKEN} is taken`, 15000, n);
assert.deepEqual(taken.actions, ['settings']);
assert.equal(taken.detail, undefined, 'found before any server started: nothing is filed as what one said');
assert.equal(serversOn(OTHER).length, 0, 'nothing left running on it');

/** A served database's row in Servers, by its path, as a script in the page finds it. */
const pickOf = (path) => `[...document.querySelectorAll('#srv-list li')].find((li) => li.dataset.path === ${JSON.stringify(path)})`;
/** Servers, with `path`'s row picked, as a click picks it. */
async function showServerOf(path) {
  await evalIn('launcher', `[...document.querySelectorAll('.nav')].find((b) => b.dataset.section === 'servers').click(); return 1`);
  await until(async () => (await evalIn('launcher', `return ${pickOf(path)} !== undefined`)).value === true, 10000, `${basename(path)} in Servers`);
  await evalIn('launcher', `${pickOf(path)}.querySelector('button').click(); return 1`);
}
/** The callout on `path`'s card, once it shows: its role, its words, its row's and its chip's, and its buttons with their heights. */
const calloutOf = (path) =>
  until(async () => {
    const v = (
      await evalIn(
        'launcher',
        `const c = document.getElementById('srv-failed'); return document.getElementById('srv-name').textContent === ${JSON.stringify(basename(path))} && !c.hidden && JSON.stringify({ role: c.getAttribute('role'), text: c.innerText.replace(/\\s+/g, ' '), row: ${pickOf(path)}.innerText.replace(/\\s+/g, ' '), chip: document.getElementById('srv-chip').textContent, note: document.getElementById('running-count').textContent, start: document.getElementById('srv-start').hidden, buttons: [...c.querySelectorAll('button')].filter((b) => !b.hidden).map((b) => [b.textContent, b.getBoundingClientRect().height]) })`,
      )
    ).value;
    return v && JSON.parse(v);
  }, 10000, `the callout on ${basename(path)}`);

step('Servers keeps saying it: the row and the chip say Did not start, a callout says why, announced, with Change the port and Start again, 40 px tall or more');
await showServerOf(OTHER);
const portCallout = await calloutOf(OTHER);
assert.equal(portCallout.role, 'status', 'announced: a status, as the notice was the alert');
assert.match(portCallout.text, /It did not start\./);
assert.match(portCallout.text, new RegExp(`Port ${TAKEN} is in use: something else answers on it, so no server was started\\.`));
assert.match(portCallout.row, /Did not start/);
assert.equal(portCallout.chip, 'Did not start');
assert.equal(portCallout.note, '1 did not start', 'the sidebar says it first');
assert.equal(portCallout.start, true, 'Start again takes Start\'s place');
assert.deepEqual(portCallout.buttons.map(([label]) => label), ['Change the port', 'Start again']);
for (const [label, height] of portCallout.buttons) assert.ok(height >= 40, `${label}: ${height} px tall`);

step('Start again starts it with the same settings: the port still taken, said again');
n = lines().length;
await evalIn('launcher', `document.getElementById('srv-failed-start').click(); return 1`);
const takenAgain = await waitNotice(`Port ${TAKEN} is taken`, 15000, n);
await pressNotice(takenAgain, 'ok');
await pressNotice(taken, 'ok');

step('Change the port: the serve sheet on it, at the port that was taken; another port starts it, and the callout goes');
await evalIn('launcher', `document.getElementById('srv-failed-port').click(); return 1`);
const sheetOn = await until(async () => (await evalIn('launcher', `return document.getElementById('sheet').open && JSON.stringify([document.getElementById('sheet-title').textContent, document.getElementById('port').value])`)).value, 10000, 'the serve sheet');
assert.deepEqual(JSON.parse(sheetOn), [`Serve ${basename(OTHER)}`, String(TAKEN)]);
const MOVED = await freePort();
n = lines().length;
await evalIn('launcher', `const p = document.getElementById('port'); p.value = ${JSON.stringify(String(MOVED))}; p.dispatchEvent(new Event('input', { bubbles: true })); document.getElementById('sheet-start').click(); return 1`);
const moved = await waitFor((l, i) => i >= n && l.what === 'served' && l.path === OTHER, 30000, 'served on the port it was changed to');
assert.equal(moved.port, MOVED);
await until(async () => (await evalIn('launcher', `return document.getElementById('srv-failed').hidden && document.getElementById('srv-chip').textContent`)).value === 'Running', 10000, 'the callout gone once it runs');
assert.equal((await ask({ cmd: 'stop-served', path: OTHER }, 'stop-served-result')).ok, true);
blocker.close();

step('a file another program serves: said so, and no second writer');
const holder = await startServer(['--file', HELD, '--http', '127.0.0.1:0'], { FENNEC_HTTP_TOKEN: 'x' });
n = lines().length;
cmd({ cmd: 'serve', path: HELD });
const held = await waitNotice('This database is open in another program', 15000, n);
assert.equal(serversOn(HELD).length, 1, 'only the other program\'s');

step('the notice keeps what the server said in a fold, open as it comes, that its summary closes and opens again');
// Whether the server's words are drawn is asked of checkVisibility(): WebKit folds a closed
// details' content by content-visibility: hidden, which leaves it a box of its full size
// (on macOS a folded one measured taller than 0), where an engine before that left it no box.
const fold = async () =>
  JSON.parse(
    (
      await evalIn(
        held.key,
        `const d = document.getElementById('detail-box'); const s = d.querySelector('summary'); const pre = document.getElementById('detail'); return JSON.stringify({ tag: d.tagName, hidden: d.hidden, open: d.open, summary: s.textContent, summaryHeight: s.getBoundingClientRect().height, inside: d.contains(pre), detail: pre.textContent, drawn: pre.getClientRects().length > 0 && (typeof pre.checkVisibility !== 'function' || pre.checkVisibility()), buttons: [...document.querySelectorAll('#buttons button')].map((b) => [b.textContent, b.getBoundingClientRect().height]) })`,
      )
    ).value,
  );
const unfolded = await fold();
assert.equal(unfolded.tag, 'DETAILS');
assert.equal(unfolded.hidden, false);
assert.equal(unfolded.summary, 'What the server said');
assert.ok(unfolded.inside, 'the server\'s words are inside the fold');
assert.match(unfolded.detail, /it is open already/);
assert.ok(unfolded.open && unfolded.drawn, 'open as it comes');
assert.ok(unfolded.summaryHeight >= 40, `its summary is ${unfolded.summaryHeight} px tall`);
assert.deepEqual(unfolded.buttons.map(([label]) => label), ['Close', 'Try again']);
for (const [label, height] of unfolded.buttons) assert.ok(height >= 40, `${label}: ${height} px tall`);
// A click on the summary toggles the fold at once (its `toggle` event comes after): read straight after.
await evalIn(held.key, `document.querySelector('#detail-box summary').click(); return 1`);
const folded = await fold();
assert.ok(!folded.open && !folded.drawn, `folded: the server's words are not drawn: ${JSON.stringify(folded)}`);
await evalIn(held.key, `document.querySelector('#detail-box summary').click(); return 1`);
assert.ok((await fold()).open, 'and open again');

step('Servers says that too: why in words, no port to change, and the server\'s own line, its error, ending its output');
await showServerOf(HELD);
const heldCallout = await calloutOf(HELD);
assert.match(heldCallout.text, /held-by-another\.fennec is open in another program, which holds its lock\./);
assert.deepEqual(heldCallout.buttons.map(([label]) => label), ['Start again'], 'its port was not why');
const said = await until(async () => (await evalIn('launcher', `const s = document.querySelector('#srv-log .said'); return s !== null && s.textContent`)).value, 10000, 'the server\'s line in its output');
assert.match(said, /it is open already/);
await pressNotice(held, 'ok');
await holder.stop();

step('a served database\'s server that dies: said, with Start again, which starts it with the same settings');
n = lines().length;
process.kill(third.pid, 'SIGKILL');
const died = await waitNotice('A database server stopped', 15000, n);
assert.deepEqual(died.actions, ['start']);
assert.match(entry(FILE).lastRan.why, /exited/);
await pressNotice(died, 'start');
const back = await waitFor((l, i) => i >= n && l.what === 'served' && l.path === FILE, 30000, 'served again after Start again');
assert.equal(back.port, PORT);

step('Stop: the server\'s own shutdown, exit 0, and the file is free for another program');
n = lines().length;
assert.equal((await ask({ cmd: 'stop-served', path: FILE }, 'stop-served-result')).ok, true);
const stopped = await waitFor((l, i) => i >= n && l.what === 'served-stopped' && l.path === FILE, 15000, 'stopped');
assert.equal(stopped.code, 0);
assert.equal(serversOn(FILE).length, 0);
const free = await startServer(['--file', FILE, '--http', '127.0.0.1:0'], { FENNEC_HTTP_TOKEN: 'x' });
await free.stop();
assert.equal(entry(FILE).lastRan.why, 'stopped');

step('served again, and the app quits: the quit stops it, and nothing is left running');
n = lines().length;
await ask({ cmd: 'serve', path: FILE, settings: { port: PORT, reach: 'lan', metrics: true } }, 'serve-result');
await waitFor((l, i) => i >= n && l.what === 'served' && l.path === FILE, 30000, 'served');
await quit(app);
assert.equal(serversOn(FILE).length, 0, 'no server outlives the app');
assert.equal(entry(FILE).lastRan.why, 'the app quit');

step('a relaunch starts nothing: the server is listed stopped, with the settings it ran with');
app = startApp('serve-relaunch', { FENNEC_DESKTOP_LAUNCHER: '1' });
await waitFor((l) => l.what === 'ready', 60000, 'ready');
await waitFor((l) => l.what === 'page-text' && l.key === 'launcher' && /Databases/.test(l.text), 20000, 'the main window drawn');
await sleep(1500); // nothing is to happen: a server that started itself would be up by now
assert.equal(serversOn(FILE).length, 0, 'no server started by itself');
assert.equal((await ask({ cmd: 'served', path: FILE }, 'served-state')).running, false);
await evalIn('launcher', `[...document.querySelectorAll('.nav')].find((b) => b.dataset.section === 'servers').click()`);
const listed = await until(async () => {
  const t = (await evalIn('launcher', `return document.getElementById('servers').innerText`)).value;
  return /served\.fennec/.test(t) && /Stopped/.test(t) && t;
}, 10000, 'the Servers list');
assert.match(listed, new RegExp(`Port ${PORT} when started`));
// The failed start is kept with its last run (state.json), so it is said after a relaunch too.
const heldRow = await until(async () => (await evalIn('launcher', `const li = ${pickOf(HELD)}; return li !== undefined && li.innerText`)).value, 10000, 'the held file\'s row');
assert.match(heldRow, /Did not start/);

step('Remove from Servers: its settings and its Keychain item go');
await ask({ cmd: 'remove-served', path: FILE }, 'served-removed');
assert.ok(!keychainHas(`serve:${id}`), 'the Keychain item is gone');
assert.equal(entry(FILE).serve, undefined);
await quit(app);
assert.equal(mine().length, 0);
forgetServeItems();
log('clean');

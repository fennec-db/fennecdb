// The last window closed. On macOS the app stays, in the Dock, as a document
// app does, a served database's server still running until File, Quit. On
// Windows and Linux, which have no Dock, closing the last window quits the app
// as File, Quit does (src/bun/index.ts): every server stopped by its one
// shutdown first -- the end of its stdin on Windows, SIGTERM elsewhere, its
// checkpoint written -- a served database's too, then the app gone, nothing of
// it left. Before that, a window closed while another is open quits nothing,
// and neither does a notice whose Try again closes it, the last window, to
// open the file it was about: that open makes a window of its own.
import assert from 'node:assert/strict';
import {
  MAC, assertClean, cmd, data, describe, forgetServeItems, lines, mine, pressNotice, procs, quit, startApp, startServer, step, until, waitFor, waitNotice, waitStudio, log,
} from './lib.mjs';

assertClean('before');
const A = data('last-a', 10);
const SERVED = data('last-served', 10);
const HELD = data('last-held', 10);

const app = startApp('lastwindow', { FENNEC_DESKTOP_OPEN: A });
await waitFor((l) => l.what === 'server-started' && l.path === A, 60000, "the first file's server");
await waitStudio(`file:${A}`);
cmd({ cmd: 'serve', path: SERVED });
const served = await waitFor((l) => l.what === 'served' && l.path === SERVED, 60000, 'a served database, with no window');
const alive = (pid) => procs().some((p) => p.pid === pid);
/** The app answers a command: it is still there. */
async function answers() {
  const n = lines().length;
  cmd({ cmd: 'sessions' });
  return waitFor((l, i) => i >= n && l.what === 'sessions', 15000, 'the app answering');
}

step('a notice and a window: the window closed, the notice left, the app stays');
const holder = await startServer(['--file', HELD, '--http', '127.0.0.1:0'], { FENNEC_HTTP_TOKEN: 'x' });
let n = lines().length;
cmd({ cmd: 'open', path: HELD });
const notice = await waitNotice(/open in another program/, 15000, n);
assert.deepEqual(notice.actions, ['retry']);
n = lines().length;
cmd({ cmd: 'close', key: `file:${A}` });
const first = await waitFor((l, i) => i >= n && l.what === 'server-stopped' && l.key === `file:${A}`, 30000, "the first window's server stopped");
assert.equal(first.code, 0, 'exit 0: its checkpoint written');
assert.deepEqual((await answers()).keys, [], 'no studio window, and the app still there');
assert.ok(!lines().some((l) => l.what === 'last-window-closed'), 'nothing said the last window closed');

step("the notice, the last window, closed by its Try again: the file it opens makes a window, and the app stays");
await holder.stop();
n = lines().length;
await pressNotice(notice, 'retry');
await waitFor((l, i) => i >= n && l.what === 'server-started' && l.path === HELD, 30000, 'the held file opening');
await waitStudio(`file:${HELD}`, n);
assert.ok(!lines().some((l) => l.what === 'last-window-closed'), 'nothing said the last window closed');
assert.ok(alive(served.pid), 'the served database still runs');

n = lines().length;
cmd({ cmd: 'close', key: `file:${HELD}` });
if (MAC) {
  step('macOS: the last window closed, the app stays in the Dock, and so does the served database');
  const stopped = await waitFor((l, i) => i >= n && l.what === 'server-stopped' && l.key === `file:${HELD}`, 30000, "the window's server stopped");
  assert.equal(stopped.code, 0);
  assert.deepEqual((await answers()).keys, [], 'the app answers with no window open');
  assert.ok(alive(served.pid), 'the served database still runs');
  step('quit');
  await quit(app);
} else {
  step('Windows and Linux: the last window closed, the app quits: every server stopped by its own shutdown, the served one too, then nothing of the app left');
  const said = await waitFor((l, i) => i >= n && l.what === 'last-window-closed', 30000, 'the app quitting as its last window closed');
  assert.equal(said.busy, true, 'a quit with servers to stop');
  const window = await waitFor((l, i) => i >= n && l.what === 'server-stopped' && l.key === `file:${HELD}`, 30000, "the window's server stopped");
  assert.equal(window.code, 0, "the window's server: exit 0, its checkpoint written");
  const stopped = await waitFor((l, i) => i >= n && l.what === 'served-stopped' && l.path === SERVED, 40000, 'the served database stopped by the quit');
  assert.equal(stopped.why, 'the app quit');
  assert.equal(stopped.code, 0, 'the served one: exit 0, its checkpoint written, not killed');
  // The quit waits for its servers, and a hard kill comes 30 s after the shutdown was asked for.
  const gone = await until(() => mine().length === 0, 60000, 'nothing of the app left').then(() => true, () => false);
  assert.ok(gone, `the app went by itself; left:\n${describe(mine())}`);
  const ended = await app.exited;
  log('the app ended with', JSON.stringify(ended));
}
forgetServeItems();
log('clean');

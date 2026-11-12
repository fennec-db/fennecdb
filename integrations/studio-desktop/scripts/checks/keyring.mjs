// Linux with no credential store to keep a token in: the app says so in
// words, at once, keeps the token nowhere else, and never takes "no store" for
// "no token". Through libsecret alone a Secret Service that never answered held
// every call 25 s, and one with no default collection answered `get` with null,
// as if nothing were saved (#578); the app asks the service first, bounded
// (src/bun/secretservice.ts). Each case is a session bus of the check's own
// for the app alone (the run's own bus, with its unlocked keyring, stays the
// other checks'):
//   - a bus with no Secret Service started on it: gnome-keyring is installed,
//     so the bus would start it, and on the runners that start never answers;
//   - gnome-keyring running on it with no default collection (no login keyring):
//     a daemon of its own, with a runtime and a data folder of its own. Started
//     with the run's, `--start` found the run's daemon by its control socket in
//     $XDG_RUNTIME_DIR/keyring and registered nothing on this bus, which then
//     never answered: the first case again, not this one.
// In each, saving a connection with a token is refused within a bound well
// under the 25 s it stands against, and opening one saved before is refused in
// words, with no request sent to its server (a token not read is not "none").
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { join } from 'node:path';
import { D, USER_DATA, assertClean, cmd, lines, procs, quit, startApp, step, waitFor, log } from './lib.mjs';

assertClean('before');
const TOKEN = `keyring-check-${process.pid}-token`;
/** What the 25 s of libsecret's wait stands against, with seconds to spare: the refusal comes within this. */
const FAST_MS = 15000;

// A server a saved connection points at: it counts what reaches it, which must be nothing.
let requests = 0;
const target = createServer((req, res) => {
  requests++;
  res.end();
});
await new Promise((r) => target.listen(0, '127.0.0.1', r));
const URL_ = `http://127.0.0.1:${target.address().port}`;

/** A session bus of the check's own: its address and its daemon's pid. */
function bus() {
  const r = spawnSync('dbus-daemon', ['--session', '--fork', '--nopidfile', '--print-address=1', '--print-pid=1'], { encoding: 'utf8', timeout: 30000 });
  const [address, pid] = r.stdout.trim().split('\n');
  assert.ok(address?.startsWith('unix:') && Number(pid) > 0, `a session bus started: ${r.stdout}${r.stderr}`);
  return { address, pid: Number(pid) };
}
const keyrings = () => procs().filter((p) => /gnome-keyring-daemon/.test(p.command)).map((p) => p.pid);

/** The app on the session bus at `address`, where the store is refused for the reason `said` names. */
async function noStore(name, address, said) {
  // A connection saved before the store went: its token is somewhere the app cannot read now.
  const saved = { id: '11111111-2222-3333-4444-555555555555', name: 'saved before', url: URL_, tenant: null };
  writeFileSync(join(USER_DATA, 'state.json'), JSON.stringify({ recents: [], connections: [saved], databases: [] }));
  // NO_AT_BRIDGE: GTK's accessibility bus would be started on this bus too, and outlive it.
  const app = startApp(name, { DBUS_SESSION_BUS_ADDRESS: address, NO_AT_BRIDGE: '1', FENNEC_DESKTOP_LAUNCHER: '1' });
  await waitFor((l) => l.what === 'ready', 60000, 'ready');

  step(`${name}: a connection saved with a token is refused, in words, at once`);
  let n = lines().length;
  const t0 = Date.now();
  cmd({ cmd: 'addconn', name: 'new one', url: URL_, token: TOKEN, tenant: '' });
  const refused = await waitFor((l, i) => i >= n && (l.what === 'conn-refused' || l.what === 'conn-added'), FAST_MS + 15000, 'the save answered');
  const took = Date.now() - t0;
  log(`answered in ${took} ms:`, refused.message);
  assert.equal(refused.what, 'conn-refused', 'not saved');
  assert.match(refused.message, /credential store|keyring/i, 'said as the store it is');
  assert.match(refused.message, said, 'and why');
  assert.ok(took < FAST_MS, `within ${FAST_MS / 1000} s, not libsecret's 25: ${took} ms`);
  const state = readFileSync(join(USER_DATA, 'state.json'), 'utf8');
  assert.deepEqual(JSON.parse(state).connections.map((c) => c.name), ['saved before'], 'the list is as it was');
  for (const f of readdirSync(USER_DATA, { recursive: true })) {
    let text = '';
    try {
      text = readFileSync(join(USER_DATA, String(f)), 'latin1');
    } catch {
      continue; // a folder
    }
    assert.ok(!text.includes(TOKEN), `the token was kept nowhere else: ${f}`);
  }

  step(`${name}: a connection saved before is not opened as one with no token`);
  n = lines().length;
  cmd({ cmd: 'openconn', id: saved.id });
  const opened = await waitFor((l, i) => i >= n && l.what === 'openconn-result', FAST_MS + 15000, 'the open answered');
  assert.equal(opened.ok, false, 'not opened');
  // Its words are the notice's message; its title is the general "Something went wrong".
  const notice = await waitFor((l, i) => i >= n && l.what === 'notice' && /token saved for saved before could not be read/.test(l.message), 15000, 'a notice that the saved token could not be read');
  await waitFor((l) => l.what === 'notice-ready' && l.key === notice.key, 15000, `${notice.key} drawn`);
  log('said:', notice.title, '|', notice.message, '|', notice.detail ?? '');
  assert.match(`${notice.message} ${notice.detail ?? ''}`, /credential store|keyring/i);
  assert.equal(requests, 0, 'nothing was sent to its server, with a token or without');
  await quit(app);
}

const before = new Set(keyrings());
const started = [];
try {
  step('a session bus with no Secret Service started on it');
  const a = bus();
  started.push(a.pid);
  await noStore('keyring-none', a.address, /did not answer/);

  step('gnome-keyring on a session bus, with no default collection');
  const b = bus();
  started.push(b.pid);
  const own = join(D, 'keyring-own');
  // Empty each time: no keyring an earlier run made.
  rmSync(own, { recursive: true, force: true });
  mkdirSync(join(own, 'run'), { recursive: true, mode: 0o700 });
  mkdirSync(join(own, 'data'), { recursive: true });
  const k = spawnSync('gnome-keyring-daemon', ['--start', '--components=secrets'], {
    env: { ...process.env, DBUS_SESSION_BUS_ADDRESS: b.address, XDG_RUNTIME_DIR: join(own, 'run'), XDG_DATA_HOME: join(own, 'data') },
    encoding: 'utf8',
    timeout: 30000,
  });
  log('gnome-keyring-daemon --start:', k.status, k.stdout.trim().replace(/\n/g, ' '), k.stderr.trim().replace(/\n/g, ' '));
  await noStore('keyring-no-default', b.address, /no default keyring/);
} finally {
  // The buses this check started, and the keyring daemons on them.
  for (const pid of [...started, ...keyrings().filter((p) => !before.has(p))]) {
    try {
      process.kill(pid, 'SIGKILL');
    } catch {
      // gone
    }
  }
  target.close();
}
log('clean');

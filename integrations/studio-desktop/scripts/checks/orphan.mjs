// The app is killed (SIGKILL: Activity Monitor's Force Quit, a crash; on
// Windows TerminateProcess, Task Manager's End task). No system tells a child
// its parent died; but every one closes the handles a dead process held, and
// each server's stdin is a pipe the app held the write end of
// (`--exit-on-stdin-close`, src/bun/server.ts): the server sees it end and
// shuts down as on SIGTERM, its lock let go.
//
// Two files, opened one after the other: the second server is started by a
// process that already holds the first's pipe, and if that write end leaked
// into it (a descriptor not marked close-on-exec) the first would not see its
// stdin end for as long as the second ran. Both ending is no proof there was
// no leak (the second ends when the app's death closes its own stdin, and then
// lets the first's go), so the check looks at who holds what **while the app is
// alive**: the descriptor tables say which processes hold the write end of each
// server's stdin, and it is the app's alone -- lsof's on macOS, /proc's on
// Linux (lsof there names no pipe's other end). A control starts a process that
// does hold another's write end and requires the same reading to find it, so an
// empty answer is not a reading that sees nothing.
//
// Windows has no table a check can read without a handle walker of its own (a
// pipe's two ends are two handles of two processes, matched only by the
// kernel's object), so there the check is the kill, every server ending and
// every file free; that no server holds another's pipe is what it no longer
// proves there. libuv, which Bun spawns through, makes a child's stdio handles
// inheritable only for that child's own start.
//
// (An earlier version stopped the second server with SIGSTOP and required the
// first to end meanwhile. On a hosted macOS runner a stopped process made `ps`
// hang, and with it the runner's own cleanup: the job never finished, not even
// by its timeout. A process is not stopped here.)
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { readdirSync, readFileSync, readlinkSync } from 'node:fs';
import { LINUX, LIST, MAC, assertClean, cmd, data, describe, forgetServeItems, mine, procs, startApp, startServer, step, until, waitFor, waitStudio, log } from './lib.mjs';

/** macOS: the pipes and sockets each pid holds, from lsof: pid -> [{fd, type, own, peer}], `own` and `peer` the kernel's addresses of the two ends. */
function lsofEnds(pids) {
  const r = spawnSync('lsof', ['-nP', '-p', pids.join(','), '-F', 'pftdn'], { encoding: 'utf8', maxBuffer: 1 << 26 });
  const table = new Map();
  let pid;
  let fd;
  for (const line of r.stdout.split('\n')) {
    const k = line[0];
    const v = line.slice(1);
    if (k === 'p') {
      pid = +v;
      table.set(pid, []);
    } else if (k === 'f') {
      fd = { fd: v, type: '', own: '', peer: '' };
      table.get(pid).push(fd);
    } else if (k === 't') fd.type = v;
    else if (k === 'd') fd.own = v;
    else if (k === 'n' && v.startsWith('->')) fd.peer = v.slice(2);
  }
  for (const [p, list] of table) table.set(p, list.filter((e) => (e.type === 'PIPE' || e.type === 'unix') && e.own && e.peer));
  return table;
}

/**
 * Linux: what each of `pids` holds, from /proc: pid -> [{fd, link, write}].
 * A pipe's two ends are one `pipe:[inode]`, told apart by the descriptor's
 * access mode (fdinfo's flags); a socketpair's two are two `socket:[inode]`s,
 * paired by `ss` (libuv, Node's, makes a child's stdio so).
 */
function procEnds(pids) {
  const table = new Map();
  for (const pid of pids) {
    const list = [];
    let fds = [];
    try {
      fds = readdirSync(`/proc/${pid}/fd`);
    } catch {
      // gone
    }
    for (const fd of fds) {
      try {
        const link = readlinkSync(`/proc/${pid}/fd/${fd}`);
        if (!/^(pipe|socket):\[/.test(link)) continue;
        const flags = Number.parseInt(/flags:\s*([0-7]+)/.exec(readFileSync(`/proc/${pid}/fdinfo/${fd}`, 'utf8'))?.[1] ?? '0', 8);
        list.push({ fd, link, write: (flags & 3) !== 0 });
      } catch {
        // closed meanwhile
      }
    }
    table.set(pid, list);
  }
  return table;
}

/** Linux: the peer of each Unix socket, by inode, as `ss` pairs them. */
function socketPeers() {
  const r = spawnSync('ss', ['-xaH'], { encoding: 'utf8', maxBuffer: 1 << 26, timeout: 30000 });
  const peers = new Map();
  for (const line of r.stdout.split('\n')) {
    // Netid State Recv-Q Send-Q Local-Address:Port Peer-Address:Port: `* 12345 * 12346`, the inodes after each address.
    const m = /\s(\d+)\s+\S+\s+(\d+)\s*$/.exec(line.replace(/\s+users:.*$/, ''));
    if (m && m[2] !== '0') peers.set(m[1], m[2]);
  }
  return peers;
}

/** Who holds the other end of `pid`'s stdin, of the pids in the reading. */
function writersOfStdin(pid, pids) {
  if (MAC) {
    const table = lsofEnds(pids);
    const stdin = (table.get(pid) ?? []).find((e) => e.fd === '0');
    assert.ok(stdin, `${pid} has a pipe or socket for a stdin (lsof: ${JSON.stringify(table.get(pid))})`);
    return [...table].filter(([p, list]) => p !== pid && list.some((e) => e.own === stdin.peer)).map(([p]) => p);
  }
  const table = procEnds(pids);
  const stdin = (table.get(pid) ?? []).find((e) => e.fd === '0');
  assert.ok(stdin, `${pid} has a pipe or socket for a stdin (/proc: ${JSON.stringify(table.get(pid))})`);
  if (stdin.link.startsWith('pipe:')) {
    return [...table].filter(([p, list]) => p !== pid && list.some((e) => e.link === stdin.link && e.write)).map(([p]) => p);
  }
  const inode = /\[(\d+)\]/.exec(stdin.link)[1];
  const peer = socketPeers().get(inode);
  assert.ok(peer, `the socket on ${pid}'s stdin (${stdin.link}) has a peer ss names`);
  return [...table].filter(([p, list]) => p !== pid && list.some((e) => e.link === `socket:[${peer}]`)).map(([p]) => p);
}

assertClean('before');

if (MAC || LINUX) {
  step('the control: a process that holds another one\'s stdin write end is found');
  const reader = spawn('cat', [], { stdio: ['pipe', 'ignore', 'ignore'] });
  // Both started is an event each, listened for before either is awaited: Node emits
  // `spawn` on the next tick, so the reader's had gone by when the first version,
  // awaiting the leaker's first, began to listen for it, and the check hung on Linux.
  const readerUp = new Promise((r) => reader.once('spawn', r));
  // Handed the write end as a descriptor of its own, which is the leak.
  const leaker = spawn('sleep', ['30'], { stdio: ['ignore', 'ignore', 'ignore', reader.stdin] });
  await Promise.all([readerUp, new Promise((r) => leaker.once('spawn', r))]);
  const found = writersOfStdin(reader.pid, [reader.pid, leaker.pid, process.pid]);
  reader.kill();
  leaker.kill();
  assert.deepEqual(found.sort(), [leaker.pid, process.pid].sort(), 'the check sees this process and the one that was handed the descriptor');
  log('found:', found.length, 'holders, the leaker among them');
} else {
  log('Windows: no descriptor table is read (above); the kill, the servers ending and the files free are');
}

const A = data('small', 50);
const B = data('second', 20);
const C = data('served-one', 10);

const app = startApp('orphan', { FENNEC_DESKTOP_OPEN: `${A}${LIST}${B}` });
const a = await waitFor((l) => l.what === 'server-started' && l.path === A, 60000, 'the first server started');
const b = await waitFor((l) => l.what === 'server-started' && l.path === B, 60000, 'the second server started');
await waitStudio(`file:${A}`);
await waitStudio(`file:${B}`);
// A served database's server, started after both: the same pipe, the same end, whatever its window does.
cmd({ cmd: 'serve', path: C });
const c = await waitFor((l) => l.what === 'served' && l.path === C, 60000, 'the served one started');

const servers = [a.pid, b.pid, c.pid];
const appPids = mine()
  .filter((p) => !servers.includes(p.pid) && !/fennec-server/i.test(p.command))
  .map((p) => p.pid);
assert.ok(appPids.length > 0, 'the app\'s own processes');
if (MAC || LINUX) {
  step('the write end of each server\'s stdin is held by the app, and by no server');
  for (const [name, server] of [['A', a], ['B', b], ['C, served', c]]) {
    const writers = writersOfStdin(server.pid, [...servers, ...appPids]);
    log(`${name} (${server.pid}) is written to by`, writers.join(', '));
    assert.ok(writers.length > 0, `${name}'s stdin is written to by somebody (else the reading sees nothing)`);
    assert.deepEqual(writers.filter((p) => !appPids.includes(p)), [], `${name}'s stdin is held by the app alone, not by the other server`);
  }
}

step('kill the app (everything of it but its servers): every server ends by itself, the served one too');
for (const pid of appPids) {
  try {
    process.kill(pid, 'SIGKILL');
  } catch {
    // gone with its parent
  }
}
const gone = (pid) => !procs().some((p) => p.pid === pid);
await until(() => servers.every(gone), 30000, 'the servers ending');
log('all gone');

step('and each file is free: another program opens it');
for (const file of [A, B, C]) {
  const probe = await startServer(['--file', file, '--http', '127.0.0.1:0'], { FENNEC_HTTP_TOKEN: 'x' });
  await probe.stop();
}
await until(() => mine().length === 0, 30000, 'nothing of the app left');
assert.equal(mine().length, 0, `nothing left:\n${describe(mine())}`);
forgetServeItems();
void app;
log('clean');

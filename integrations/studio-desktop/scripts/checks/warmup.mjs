// The first launch of a build, which is not like the others.
//
// A stable Electrobun build is a self-extractor: the first time it runs it
// unpacks the app next to itself (about 2 s, with an installer window), starts
// the app as a process of its own and stays in its own event loop afterwards.
// Quitting the app then leaves that first process behind (Electrobun 2.0.4,
// seen unsigned, in place and from `open`; not verified for a signed and
// notarized build). So the checks start from a build that has been launched
// once, and this does it: start, wait for the app, quit it, end the helper.
//
// On Windows and Linux the build is an installer instead (lib.mjs's
// INSTALLER): it installs the app into the test variant's own folder and
// starts the installed copy, which on Windows is not handed the hooks'
// environment and so never says it is up. So there this installs, waits for
// the installed app to run, ends what the installer started, and then starts
// the installed app itself, as every other check does, and quits it.
import { spawn } from 'node:child_process';
import { closeSync, existsSync, openSync } from 'node:fs';
import { APP, D, INSTALLER, MAC, cmd, describe, logName, mine, now, quit, sleep, startApp, step, waitFor, log } from './lib.mjs';

if (MAC) {
  const app = startApp('warmup', { FENNEC_DESKTOP_LAUNCHER: '1' });
  await waitFor((l) => l.what === 'ready', 90000, 'the app, after unpacking itself');
  await waitFor((l) => l.what === 'page-text' && l.key === 'launcher' && /Open file/.test(l.text), 20000, 'the launcher drawn');
  log('the app is up');

  step('quit');
  cmd({ cmd: 'quit' });
  // The app itself ends within a moment; what may stay is the unpacker.
  const t0 = now();
  while (now() - t0 < 20000 && mine().some((p) => !p.command.endsWith('/Contents/MacOS/launcher'))) await sleep(100);
  const left = mine();
  const real = left.filter((p) => !p.command.endsWith('/Contents/MacOS/launcher'));
  if (real.length) throw new Error(`the app did not quit:\n${describe(real)}`);
  if (left.length) {
    log(`the unpacker is still running (${left.map((p) => p.pid).join(', ')}): Electrobun's first launch; ended`);
    for (const p of left) process.kill(p.pid, 'SIGTERM');
    await sleep(500);
  }
  await Promise.race([app.exited, sleep(2000)]);
  if (mine().length) throw new Error(`left behind:\n${describe(mine())}`);
  log('clean');
} else {
  step(`install: ${INSTALLER}`);
  if (!existsSync(INSTALLER)) throw new Error(`no installer at ${INSTALLER}: build the test variant (FENNEC_DESKTOP_TEST_BUILD=1 npm run build)`);
  logName('warmup');
  const fd = openSync(`${D}/logs/warmup.app.log`, 'w');
  // None of the hooks' environment: what the installer starts is the installed app, as a user's would be.
  const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('FENNEC_DESKTOP_')));
  const setup = spawn(INSTALLER, [], { env, stdio: ['ignore', fd, fd] });
  closeSync(fd);
  const t0 = now();
  // Installed once the installed copy runs: its Bun, which its launcher starts on its main.js.
  const running = () => mine().filter((p) => p.command.includes(APP) && /main\.js/.test(p.command));
  while (!running().length) {
    if (now() - t0 > 120000) throw new Error(`the installed app did not start in 120 s:\n${describe(mine())}`);
    if (setup.exitCode !== null && setup.exitCode !== 0) throw new Error(`the installer ended with ${setup.exitCode} (logs/warmup.app.log)`);
    await sleep(250);
  }
  log(`installed and started in ${((now() - t0) / 1000).toFixed(1)} s: ${APP}`);
  // What the installer started, and the installer, ended: they hold no file of a check's.
  for (const p of mine()) {
    try {
      process.kill(p.pid, 'SIGKILL');
    } catch {
      // gone
    }
  }
  const t1 = now();
  while (mine().length) {
    if (now() - t1 > 30000) throw new Error(`what the installer started is still running:\n${describe(mine())}`);
    await sleep(250);
  }

  step('the installed app: up, its launcher drawn, and a quit leaves nothing');
  const app = startApp('warmup-installed', { FENNEC_DESKTOP_LAUNCHER: '1' });
  await waitFor((l) => l.what === 'ready', 60000, 'the installed app');
  await waitFor((l) => l.what === 'page-text' && l.key === 'launcher' && /Open file/.test(l.text), 30000, 'the launcher drawn');
  await quit(app);
  log('clean');
}

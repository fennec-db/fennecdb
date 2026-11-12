// Runs the scripted checks one after another, each in a process of its own,
// and says which passed. `make studio-desktop-test` runs it after the unit
// tests; it needs a window server (a logged-in Mac, or a CI runner's; on Linux
// an X display, xvfb-run's on CI) and the test variant of the app built
// (FENNEC_DESKTOP_TEST_BUILD=1).
//
//   node scripts/checks/run.mjs            # all of them
//   ONLY=files,remote node scripts/checks/run.mjs
//
// Each check's output goes to $SD_DIR/logs/<check>.log and, as the check
// goes, the app's own to <check>.app.log; a CI run uploads that directory
// when it fails. A check that leaves a process behind is killed here, and
// fails for it; and each check starts with none of the test variant's state
// (`freshState`: its saved connections, credential store items and data
// stores), so a check that failed half-way fails alone, never the ones after it.
import { spawn, spawnSync } from 'node:child_process';
import { closeSync, mkdirSync, openSync, rmSync, writeSync } from 'node:fs';
import { join } from 'node:path';
import { APP_DATA, D, HERE, POWERSHELL, WIN, describe, freshState, keepEvidence, mine, now, procs, sleep } from './lib.mjs';

/**
 * Every check, in the order they run, and the systems one does not run on:
 * why, and, for one a later phase of the Windows and Linux plan brings, which
 * (#578: 3, desktop integration; 4, distribution). A check skipped is said as
 * SKIP with its reason; none is left out silently.
 */
const LINUX_ONLY = {
  darwin: 'Linux alone: macOS hands a link to NSWorkspace and a folder to Finder, and keeps tokens in the Keychain, none of it through a shell or a bus',
  win32: 'Linux alone: Windows hands a link and a folder to ShellExecute, and keeps tokens in Credential Manager, none of it through a shell or a bus',
};
const CHECKS = [
  { name: 'release' },
  { name: 'warmup' },
  { name: 'files' },
  { name: 'server-dies' },
  { name: 'orphan' },
  { name: 'sandbox' },
  { name: 'pages' },
  { name: 'storage' },
  { name: 'environment' },
  { name: 'remote' },
  { name: 'newdb' },
  { name: 'serve' },
  { name: 'backups' },
  { name: 'opener', skip: LINUX_ONLY },
  { name: 'keyring', skip: LINUX_ONLY },
  {
    name: 'finder',
    skip: {
      linux: "phase 3: a file handed to the app by the desktop; Electrobun 2.0.4's launcher drops its arguments (upstream #483, #554)",
      win32: "phase 3: a file handed to the app by Explorer; Electrobun 2.0.4's launcher drops its arguments (upstream #483, #554)",
    },
  },
];
const only = (process.env.ONLY ?? '').split(',').filter(Boolean);
const unknown = only.filter((n) => !CHECKS.some((c) => c.name === n));
if (unknown.length) throw new Error(`no check named ${unknown.join(', ')}`);
const chosen = only.length ? CHECKS.filter((c) => only.includes(c.name)) : CHECKS;
const LIMIT_MS = 240_000;

// The test variant's own folder, never a real install's (a different identifier): emptied once a
// run, so `warmup` is a first launch (and on Windows and Linux installs the app again into it);
// each check then starts with no state of it (`freshState`).
rmSync(APP_DATA, { recursive: true, force: true });
rmSync(join(D, 'data'), { recursive: true, force: true });
mkdirSync(join(D, 'data'), { recursive: true });
mkdirSync(join(D, 'logs'), { recursive: true });

/** When each process started, to tell one that outlived a check from one started after it ended. */
function startedAt(pids) {
  if (WIN) {
    const filter = pids.map((p) => `ProcessId=${Number(p)}`).join(' or ');
    const ps = `Get-CimInstance Win32_Process -Filter '${filter}' | ForEach-Object { '{0} {1}' -f $_.ProcessId, $_.CreationDate }`;
    return spawnSync(POWERSHELL, ['-NoProfile', '-NonInteractive', '-Command', ps], { encoding: 'utf8', timeout: 30000 }).stdout;
  }
  return spawnSync('ps', ['-o', 'pid=,lstart=', '-p', pids.join(',')], { encoding: 'utf8', timeout: 30000 }).stdout;
}

const system = process.platform;
// Every wait for the app's processes asks the system for all of them, and on Windows that is
// WMI through PowerShell, which a slow runner makes the bulk of a check's time: said up front.
{
  const t = now();
  procs();
  console.log(`the system lists its processes in ${now() - t} ms here`);
}
const results = [];
for (const { name, skip } of chosen) {
  const why = skip?.[system];
  if (why) {
    console.log(`\n===== ${name}: skipped on ${system}, ${why}`);
    results.push({ name, skipped: why });
    continue;
  }
  freshState();
  const t0 = Date.now();
  const log = openSync(join(D, 'logs', `${name}.log`), 'w');
  const child = spawn(process.execPath, [join(HERE, 'scripts', 'checks', `${name}.mjs`)], { stdio: ['ignore', 'pipe', 'pipe'] });
  const echo = (b) => {
    process.stdout.write(b);
    writeSync(log, b);
  };
  child.stdout.on('data', echo);
  child.stderr.on('data', echo);
  console.log(`\n===== ${name}`);
  const timer = setTimeout(() => {
    echo(`\n${name} did not finish in ${LIMIT_MS / 1000} s\n`);
    child.kill('SIGKILL');
  }, LIMIT_MS);
  const code = await new Promise((r) => child.on('exit', (c) => r(c)));
  clearTimeout(timer);
  closeSync(log);
  if (code !== 0) {
    // What it keeps is for reading after; a report that went or a full disk fails no check.
    try {
      keepEvidence(name, t0);
    } catch (e) {
      console.log(`${name}: the evidence was not all kept: ${e.message}`);
    }
  }

  let left = mine();
  if (left.length) {
    console.log(`${name} left processes behind; killing them:\n${describe(left)}`);
    console.log(`started:\n${startedAt(left.map((p) => p.pid))}`);
    for (const p of left) {
      try {
        process.kill(p.pid, 'SIGKILL');
      } catch {
        // gone
      }
    }
    // A kill is asked, not done, when it returns (TerminateProcess on Windows): the next check's
    // freshState removes folders these may still hold open, so they are waited for, bounded.
    const t = now();
    while (mine().length && now() - t < 30000) await sleep(200);
    if (mine().length) console.log(`still running 30 s after the kill:\n${describe(mine())}`);
  }
  results.push({ name, ok: code === 0 && left.length === 0, ms: Date.now() - t0 });
}

console.log('\n===== results');
for (const r of results) console.log(r.skipped ? `SKIP  ${r.name}  (${r.skipped})` : `${r.ok ? 'PASS' : 'FAIL'}  ${r.name}  (${(r.ms / 1000).toFixed(1)} s)`);
const ran = results.filter((r) => !r.skipped);
const failed = ran.filter((r) => !r.ok);
const skipped = results.length - ran.length;
if (failed.length) {
  console.log(`\n${failed.length} of ${ran.length} failed${skipped ? `, ${skipped} skipped on ${system}` : ''}; logs in ${join(D, 'logs')}`);
  for (const r of failed) console.log(`  ${join(D, 'logs', `${r.name}.log`)}, ${join(D, 'logs', `${r.name}.app.log`)}`);
  process.exit(1);
}
console.log(`\nall ${ran.length} checks passed${skipped ? `, ${skipped} skipped on ${system} (above, each with its reason)` : ''}`);

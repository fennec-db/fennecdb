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
 * Every check, in the order they run, and the systems one does not run on yet:
 * why, and the phase of the Windows and Linux plan that brings it (#578: 2, the
 * process and security layer; 3, desktop integration; 4, distribution). A
 * check skipped is said as SKIP with its reason; none is left out silently.
 */
const CHECKS = [
  { name: 'release' },
  { name: 'warmup' },
  {
    name: 'files',
    skip: { win32: "phase 2: a closed window's server is stopped by SIGTERM, TerminateProcess on Windows (exit 1, no checkpoint), until the app ends its stdin" },
  },
  { name: 'server-dies' },
  {
    name: 'orphan',
    skip: {
      linux: "phase 2: it reads the descriptor tables with lsof, which finds no pipe's other end on Linux: /proc/<pid>/fd then",
      win32: 'phase 2: no lsof, and the stop by stdin it needs; the app killed, its servers ending and the files free then',
    },
  },
  {
    name: 'sandbox',
    skip: {
      linux: "phase 2: a page's window.open of a non-web address raises no new-window-open on WebKitGTK, so the app is never asked; links handed to the system without a shell then",
      win32: "phase 2: a page's window.open of a non-web address raises no new-window-open on WebView2, so the app is never asked; links handed to the system then",
    },
  },
  {
    name: 'pages',
    skip: { win32: "phase 2: WebView2 runs the check's executeJavascript outside the page's CSP, so eval runs there; whether the page itself refuses it is not yet asked" },
  },
  {
    name: 'storage',
    skip: { win32: "phase 2: WebView2's clearStorageData empties the first view's profile, not the partition it is given, so a removed connection's store keeps its data until the app removes its folder" },
  },
  {
    name: 'environment',
    skip: { win32: "phase 2: its stand-in server is a shell script, which Windows runs as no program, and the allowlist must compare names case-blind (Path)" },
  },
  {
    name: 'remote',
    skip: {
      linux: "phase 2: it reads the Keychain with `security`; the credential store by name (libsecret's) then",
      win32: "phase 2: it reads the Keychain with `security`; the credential store by name (Credential Manager's) then",
    },
  },
  {
    name: 'newdb',
    skip: {
      win32:
        "phase 2: a new database's first collection is written by a server the app then stops by SIGTERM, TerminateProcess on Windows, which takes with it what the server still buffers (--sync 250): the window finds no collection, until the app ends its stdin",
    },
  },
  {
    name: 'serve',
    skip: {
      linux: "phase 2: it reads the Keychain with `security`; the credential store by name (libsecret's) then",
      win32: "phase 2: it reads the Keychain with `security`, and a served database's Stop must be exit 0: the stop by stdin then",
    },
  },
  {
    name: 'backups',
    skip: { win32: 'phase 2: the server started for a backup alone is stopped by SIGTERM, TerminateProcess on Windows (exit 1), until the app ends its stdin' },
  },
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

// What the server the app starts is given: the few variables a process needs
// and its token, whatever the app was started with. The server reads the
// environment for more than the token (OTEL_EXPORTER_OTLP_* turns trace export
// on, FENNEC_JWT_SECRET changes what it accepts), and the app is started from
// Finder, from a shell with anything exported in it, or by another program.
//
// The test variant lets FENNEC_SERVER_BIN name the server it starts; here that
// is a stand-in that writes its own environment down and runs the real one: a
// shell script on macOS and Linux, and on Windows, where a script is no program
// the app can start without a shell, a program built from a few lines by the
// Bun the app ships (`bun build --compile`), the real server its child with the
// stand-in's stdin, so the app's stop by its end reaches it. (A shipped app
// does not read FENNEC_SERVER_BIN: checks/release.mjs.) That Bun is the one
// Hutch fetched, not the app's copy: the build writes the app's icon into that
// copy, and Bun then refuses it as the base of a program ("Error adding Bun
// section to PE file: InvalidSecurityDirectory").
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { D, HERE, SERVER, WIN, assertClean, cmd, data, lines, quit, startApp, step, waitFor, waitStudio, log } from './lib.mjs';

assertClean('before');
const SMALL = data('small', 20);
mkdirSync(join(D, 'wrap'), { recursive: true });
const SEEN = join(D, 'wrap', 'server-env.txt');
const WRAP = join(D, 'wrap', `fennec-server${WIN ? '.exe' : ''}`);
if (WIN) {
  const source = join(D, 'wrap', 'stand-in.ts');
  writeFileSync(
    source,
    [
      `import { writeFileSync } from "node:fs";`,
      `import { spawn } from "node:child_process";`,
      // `--env-to <file>`: its environment written there, and nothing started (the control below).
      `const out = process.argv[2] === "--env-to" ? process.argv[3] : ${JSON.stringify(SEEN)};`,
      `writeFileSync(out, Object.entries(process.env).map(([k, v]) => k + "=" + v).join("\\n") + "\\n");`,
      `if (process.argv[2] === "--env-to") process.exit(0);`,
      `const child = spawn(${JSON.stringify(SERVER)}, process.argv.slice(2), { stdio: "inherit" });`,
      `child.on("exit", (code) => process.exit(code ?? 1));`,
    ].join('\n'),
  );
  const pin = JSON.parse(readFileSync(join(HERE, 'hutch-pin.json'), 'utf8'));
  const toolchain = Object.keys(pin[`windows-${process.arch}`]?.runtime ?? {}).find((k) => k.startsWith('toolchains/bun/'));
  assert.ok(toolchain, `hutch-pin.json pins a Bun for windows-${process.arch}`);
  const bun = join(HERE, '.hutch-home', toolchain, 'bun.exe');
  const built = spawnSync(bun, ['build', '--compile', source, '--outfile', WRAP], { encoding: 'utf8', timeout: 120000 });
  assert.equal(built.status, 0, `the stand-in built: ${built.stderr}${built.stdout}`);
} else {
  // `env -0` would be exact, and is not on every system; one a line is enough for names.
  writeFileSync(WRAP, `#!/bin/sh\nenv > '${SEEN}'\nexec '${SERVER}' "$@"\n`);
  chmodSync(WRAP, 0o755);
}

const LEAKS = {
  OTEL_EXPORTER_OTLP_ENDPOINT: 'http://127.0.0.1:9',
  OTEL_SERVICE_NAME: 'leak',
  FENNEC_JWT_SECRET: 'leak-jwt',
  FENNEC_HTTP_TOKEN: 'leak-old-token',
  FENNEC_REPLICATION_TOKEN: 'leak-repl',
  FENNEC_KEY_FILE: '/tmp/leak.key',
  AWS_SECRET_ACCESS_KEY: 'leak-aws',
  GITHUB_TOKEN: 'leak-gh',
};
const app = startApp('environment', { FENNEC_SERVER_BIN: WRAP, FENNEC_DESKTOP_OPEN: SMALL, ...LEAKS });
await waitFor((l) => l.what === 'server-started', 60000, 'the server started');
await waitStudio(`file:${SMALL}`);

step('the environment the server was started with');
// Windows' names are case-blind: one is the same however it is spelled.
const seen = (file = SEEN) =>
  Object.fromEntries(readFileSync(file, 'utf8').split(/\r?\n/).filter((l) => l.indexOf('=') > 0).map((l) => [WIN ? l.slice(0, l.indexOf('=')).toUpperCase() : l.slice(0, l.indexOf('=')), l.slice(l.indexOf('=') + 1)]));
const names = Object.keys(seen());
log('names:', names.join(' '));
for (const leak of ['OTEL_EXPORTER_OTLP_ENDPOINT', 'OTEL_SERVICE_NAME', 'FENNEC_JWT_SECRET', 'FENNEC_KEY_FILE', 'AWS_SECRET_ACCESS_KEY', 'GITHUB_TOKEN', 'FENNEC_DESKTOP_REPORT', 'FENNEC_DESKTOP_CONTROL', 'FENNEC_DESKTOP_OPEN', 'FENNEC_SERVER_BIN']) {
  assert.ok(!names.includes(leak), `${leak} must not reach the server`);
}
// On Windows the allowlist's own names for the system's, temporary and user's folders (src/bun/env.ts), which libuv hands a child whatever it is given.
const ALLOWED = WIN
  ? /^(HOME|PATH|TMPDIR|LANG|USER|LOGNAME|TZ|LC_[A-Z]+|HOMEDRIVE|HOMEPATH|LOGONSERVER|SYSTEMDRIVE|SYSTEMROOT|TEMP|TMP|USERDOMAIN|USERNAME|USERPROFILE|WINDIR|FENNEC_HTTP_TOKEN|FENNEC_REPLICATION_TOKEN)$/
  : /^(HOME|PATH|TMPDIR|LANG|USER|LOGNAME|TZ|LC_[A-Z]+|FENNEC_HTTP_TOKEN|FENNEC_REPLICATION_TOKEN|PWD|SHLVL|_|OLDPWD)$/;
// What Windows puts in a process's environment whatever it is handed is no leak of the app's: the
// control starts the stand-in from here with SystemRoot alone (libuv adds its required_vars, which
// the allowlist names) and reads what it got. On the arm64 runner that was PROCESSOR_ARCHITECTURE,
// the stand-in a native arm64 program. Allowed where the control shows it, and said.
let extra = [];
if (WIN) {
  const control = join(D, 'wrap', 'control-env.txt');
  const r = spawnSync(WRAP, ['--env-to', control], { env: { SystemRoot: process.env.SystemRoot }, encoding: 'utf8', timeout: 30000 });
  assert.equal(r.status, 0, `the control ran: ${r.stderr}${r.error ?? ''}`);
  extra = Object.keys(seen(control)).filter((n) => !ALLOWED.test(n));
  log(`what Windows gives a process handed SystemRoot alone, beyond the allowlist: ${extra.join(' ') || '(nothing)'}`);
}
assert.deepEqual(names.filter((n) => !ALLOWED.test(n) && !extra.includes(n)), [], 'only the allowlist (and what the wrapper script\'s own shell adds)');
// The case-blind compare is what lets Windows' `Path` through, where a name compared as macOS's would drop it.
if (WIN) assert.ok(names.includes('PATH') && names.includes('SYSTEMROOT'), 'Path and SystemRoot, however Windows spells them');
const env = seen();
assert.notEqual(env.FENNEC_HTTP_TOKEN, 'leak-old-token', 'the app\'s own token, not one that was in its environment');
assert.match(env.FENNEC_HTTP_TOKEN, /^[A-Za-z0-9_-]{43}$/, '32 random bytes, base64url');
// No backups set up: no replication token, which would turn /_changes on too; and not the app's own environment's.
assert.equal(env.FENNEC_REPLICATION_TOKEN, undefined, 'a file with no backups set up gets no replication token');

step('its backups set up, the server started next gets a replication token of the app\'s own');
let n = lines().length;
cmd({ cmd: 'set-archive', path: SMALL, dir: join(D, 'wrap', 'backups', 'small') });
const setUp = await waitFor((l, i) => i >= n && l.what === 'set-archive-result', 30000, 'set up');
assert.equal(setUp.ok, true, setUp.message);
assert.match(setUp.message, /next start/, 'the server running now is said to take them from its next start');
n = lines().length;
cmd({ cmd: 'close', key: `file:${SMALL}` });
await waitFor((l, i) => i >= n && l.what === 'server-stopped', 30000, 'the window\'s server stopped');
n = lines().length;
cmd({ cmd: 'open', path: SMALL });
await waitStudio(`file:${SMALL}`, n);
const backed = seen();
// The replication token a backup asks the server's image with: the app's own as well, and another.
assert.notEqual(backed.FENNEC_REPLICATION_TOKEN, 'leak-repl');
assert.match(backed.FENNEC_REPLICATION_TOKEN ?? '', /^[A-Za-z0-9_-]{43}$/);
assert.notEqual(backed.FENNEC_REPLICATION_TOKEN, backed.FENNEC_HTTP_TOKEN);

step('quit');
await quit(app);
log('clean');

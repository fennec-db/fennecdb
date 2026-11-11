// The shipped app holds none of what the checks drive the test variant with:
// no hook that reads a file of commands, none that reports, no environment
// variable that changes what it does. The test variant is the positive
// control: the same strings are in it, so a rename here cannot pass this
// check by looking for something that was never there. (Where there is no test
// variant -- the signing job builds only the app it ships -- the shipped bundle
// is held to the markers alone.)
//
// An environment variable the shipped app reads for itself is a hook, unless
// it is read behind `__TESTABLE__`: this list is every one there is.
//
//   node scripts/checks/release.mjs [release app] [test app]
//     macOS: the .app bundles, default build/ and build-test/stable-macos-arm64/
//     Windows and Linux: a build's folder (build/stable-<os>-<arch>/<name>, the
//     default, and build-test/'s), or an app unpacked (the download's fennec-studio)
//
// A stable build is a self-extracting wrapper: the app is a .tar.zst in the
// wrapper's Resources until it is unpacked (macOS: its first launch; Windows and
// Linux: the installer, or scripts/dist.sh), and this reads the main process's
// bundle out of it without running anything.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import * as zlib from 'node:zlib';
import { HERE, MAC, log } from './lib.mjs';

/** The one stable build's folder under `dir` (build/ or build-test/) on Windows and Linux: stable-<os>-<arch>/<name>. */
function built(dir) {
  const root = `${HERE}${dir}`;
  const stable = existsSync(root) ? readdirSync(root).filter((d) => /^stable-(win|linux)-/.test(d)) : [];
  const found = stable.flatMap((d) => readdirSync(join(root, d)).map((n) => join(root, d, n))).filter((p) => existsSync(join(p, 'Resources')));
  return found.length === 1 ? found[0] : join(root, 'no-single-stable-build-here');
}

const release = process.argv[2] ?? (MAC ? `${HERE}build/stable-macos-arm64/fennec studio.app` : built('build'));
const test = process.argv[3] ?? (MAC ? `${HERE}build-test/stable-macos-arm64/fennec studio test.app` : built('build-test'));

/**
 * The file `want` (a path ending, `/` separated) out of a tar, read in memory:
 * ustar's name and prefix, GNU's long name and pax's path are each enough to
 * find it, and no program is run for it (Windows has two tars that read paths
 * differently).
 */
function fromTar(tar, want) {
  let long = null;
  for (let at = 0; at + 512 <= tar.length; ) {
    const head = tar.subarray(at, at + 512);
    if (head.every((b) => b === 0)) break;
    const field = (from, len) => head.subarray(from, from + len).toString('utf8').replace(/\0.*$/s, '');
    const size = parseInt(field(124, 12).trim() || '0', 8);
    const type = String.fromCharCode(head[156] || 48);
    const body = tar.subarray(at + 512, at + 512 + size);
    at += 512 + Math.ceil(size / 512) * 512;
    if (type === 'L') {
      long = body.toString('utf8').replace(/\0.*$/s, '');
      continue;
    }
    if (type === 'x') {
      const m = /(?:^|\n)\d+ path=([^\n]*)\n/.exec(body.toString('utf8'));
      if (m) long = m[1];
      continue;
    }
    const prefix = field(345, 155);
    const name = long ?? (prefix ? `${prefix}/${field(0, 100)}` : field(0, 100));
    long = null;
    if ((type === '0' || type === '\0') && (name === want || name.endsWith(`/${want}`))) return body.toString('utf8');
  }
  throw new Error(`no ${want} in the payload`);
}

/** The main process's bundle, out of an app's payload, or of the app itself once it has been unpacked. */
function bundle(app) {
  const resources = MAC ? `${app}/Contents/Resources` : join(app, 'Resources');
  const payload = readdirSync(resources).find((f) => f.endsWith('.tar.zst'));
  if (!payload) return readFileSync(join(resources, 'app', 'bun', 'index.js'), 'utf8');
  if (MAC) return execFileSync('tar', ['-xOf', `${resources}/${payload}`, `${app.split('/').pop()}/Contents/Resources/app/bun/index.js`], { maxBuffer: 1 << 28 }).toString('utf8');
  // Node's zstd: 22.15 and later (CI's setup-node takes the newest 22).
  return fromTar(zlib.zstdDecompressSync(readFileSync(join(resources, payload))), 'Resources/app/bun/index.js');
}

const HOOKS = ['FENNEC_DESKTOP_REPORT', 'FENNEC_DESKTOP_CONTROL', 'FENNEC_DESKTOP_OPEN', 'FENNEC_DESKTOP_LAUNCHER', 'FENNEC_DESKTOP_HOLD_OPEN_URL', 'FENNEC_SERVER_BIN', 'FENNEC_CLI_BIN', '"close-direct"'];

const shipped = bundle(release);
const testing = existsSync(test) ? bundle(test) : null;
for (const marker of HOOKS) {
  if (testing) assert.ok(testing.includes(marker), `the test variant has ${marker} (so this check can see it)`);
  assert.ok(!shipped.includes(marker), `the shipped app must not hold ${marker}`);
}
// The constants the build sets: neither is left in the shipped code as a name to test.
assert.ok(!shipped.includes('__TESTABLE__'), '__TESTABLE__ was replaced by the bundler');
assert.ok(!shipped.includes('testhooks'), 'no module of hooks in the bundle');
log(`shipped bundle ${(shipped.length / 1024).toFixed(0)} KB holds none of ${HOOKS.length} hook markers` + (testing ? `; the test variant ${(testing.length / 1024).toFixed(0)} KB holds all` : ' (no test variant here to compare with)'));

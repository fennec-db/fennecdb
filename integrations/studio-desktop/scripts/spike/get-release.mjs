#!/usr/bin/env node
// SPIKE (desktop-spike.yml): scripts/get-server.sh's release path for every
// host the app may build on, Windows included, in Node so it runs the same on
// all three. Puts release <version>'s `fennec-server` and `fennec` for this
// machine in bin/ (`.exe` on Windows), after checking the asset's SHA-256
// against the release's own SHA256SUMS, and refuses a server without the two
// flags the app starts it with.
//
//   node scripts/spike/get-release.mjs 0.2.0
//
// The Windows asset is a zip (`fennec-v<V>-<triple>.zip`), unpacked with the
// system's own bsdtar (C:\Windows\System32\tar.exe), which reads zips; the
// others are the `.tar.gz` get-server.sh already takes.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = fileURLToPath(new URL('../..', import.meta.url));
const version = (process.argv[2] ?? '').replace(/^v/, '');
if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error(`"${version}" is not a release version`);

const triples = {
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
  'linux-x64': 'x86_64-unknown-linux-musl',
  'linux-arm64': 'aarch64-unknown-linux-musl',
  'win32-x64': 'x86_64-pc-windows-msvc',
  'win32-arm64': 'aarch64-pc-windows-msvc',
};
const triple = triples[`${process.platform}-${process.arch}`];
if (!triple) throw new Error(`no release asset for ${process.platform} ${process.arch}`);
const windows = process.platform === 'win32';
const exe = windows ? '.exe' : '';
const asset = `fennec-v${version}-${triple}${windows ? '.zip' : '.tar.gz'}`;
const base = `https://github.com/fennec-db/fennecdb/releases/download/v${version}`;

const get = async (url) => {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url}: HTTP ${r.status}`);
  return Buffer.from(await r.arrayBuffer());
};
const sums = (await get(`${base}/SHA256SUMS`)).toString('utf8');
const want = sums.split('\n').map((l) => l.trim().split(/\s+/)).find((f) => f[1] === asset)?.[0];
if (!want) throw new Error(`the release's SHA256SUMS does not list ${asset}`);
const bytes = await get(`${base}/${asset}`);
const got = createHash('sha256').update(bytes).digest('hex');
if (got !== want) throw new Error(`SHA-256 of ${asset} is ${got}, the release says ${want}: not using it`);
console.log(`get-release: ${asset} (${bytes.length} bytes), SHA-256 ${got} matches the release's SHA256SUMS`);

const tmp = mkdtempSync(join(tmpdir(), 'fennec-release-'));
try {
  writeFileSync(join(tmp, asset), bytes);
  const tar = windows ? join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'tar.exe') : 'tar';
  execFileSync(tar, [windows ? '-xf' : '-xzf', asset], { cwd: tmp, stdio: 'inherit' });
  const dir = join(tmp, `fennec-v${version}-${triple}`);
  mkdirSync(join(here, 'bin'), { recursive: true });
  for (const name of [`fennec-server${exe}`, `fennec${exe}`]) {
    copyFileSync(join(dir, name), join(here, 'bin', name));
    if (!windows) chmodSync(join(here, 'bin', name), 0o755);
  }
} finally {
  rmSync(tmp, { recursive: true, force: true });
}
const help = execFileSync(join(here, 'bin', `fennec-server${exe}`), ['--help'], { encoding: 'utf8' });
for (const flag of ['--studio', '--exit-on-stdin-close']) if (!help.includes(flag)) throw new Error(`this fennec-server has no ${flag}`);
console.log(`get-release: bin/fennec-server${exe} and bin/fennec${exe} in place`);

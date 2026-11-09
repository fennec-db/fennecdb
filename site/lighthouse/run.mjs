// Lighthouse CI against site/dist, served here on port 0 (serve.mjs):
// Chrome's files are read once first (warm-chrome.mjs, as the examples'
// jobs do), then `lhci autorun` runs lighthouserc.cjs's pages and budgets
// and summary.mjs prints each page's median run.
//
//   CHROME_PATH  Chrome or Chromium, when it is not where Lighthouse looks
//   LHCI_RUNS    runs a page (2)
import { spawn, spawnSync } from 'node:child_process';
import { rmSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { serve } from './serve.mjs';

const here = fileURLToPath(new URL('.', import.meta.url));
const server = serve(fileURLToPath(new URL('../dist', import.meta.url)));
await new Promise((ok) => server.listen(0, '127.0.0.1', ok));
const url = `http://127.0.0.1:${server.address().port}`;
console.error(`listening on: ${url}`);
rmSync(`${here}.lighthouseci`, { recursive: true, force: true });

let status = spawnSync('node', ['warm-chrome.mjs'], { cwd: here, stdio: 'inherit' }).status;
if (!status) {
  // Async: this process serves the pages, so its event loop must run.
  status = await new Promise((ok) =>
    spawn('npx', ['lhci', 'autorun'], { cwd: here, stdio: 'inherit', env: { ...process.env, SITE_URL: url } }).on('exit', (c) => ok(c ?? 1)),
  );
  spawnSync('node', ['summary.mjs'], { cwd: here, stdio: 'inherit' });
}
server.close();
process.exit(status);

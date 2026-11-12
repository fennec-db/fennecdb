// A page that loaded before its preload is loaded again (Windows).
//
// Electrobun 2.0.4 adds a view's preload with AddScriptToExecuteOnDocumentCreated,
// which completes later, and navigates in the same call: a first document can
// load with no event bridge, so with no session and no guard for new windows,
// and say nothing (seen once on the x64 runner). The app asks each page, once
// it navigated, whether the bridge is there and loads it again if not
// (src/bun/webview2.ts). When WebView2 lands the script is its own affair and
// cannot be had on demand, so the test variant makes what it leaves:
// FENNEC_DESKTOP_BRIDGELESS_FIRST=1 takes the bridge from the first document
// of each studio window (testhooks.ts). The control turns the guard off
// (FENNEC_DESKTOP_NO_PRELOAD_GUARD=1) and needs the same window to stay silent.
import assert from 'node:assert/strict';
import { assertClean, data, evalIn, lines, quit, startApp, step, waitFor, waitNone, waitStudio, log } from './lib.mjs';

assertClean('before');
const SMALL = data('small', 50);
const KEY = `file:${SMALL}`;
const loads = () => lines().filter((l) => l.what === 'did-navigate' && l.key === KEY).length;

step('a first document with no bridge is loaded again, and the studio is there with its bridge');
let app = startApp('latepreload', { FENNEC_DESKTOP_OPEN: SMALL, FENNEC_DESKTOP_BRIDGELESS_FIRST: '1' });
await waitStudio(KEY, 0, 60000);
const navs = lines().filter((l) => l.what === 'did-navigate' && l.key === KEY);
log('loads of the window:', navs.length, navs.length >= 2 ? `the second ${navs[1].at - navs[0].at} ms after the first` : '');
assert.ok(navs.length >= 2, 'the first document and the one loaded after it');
// window.name outlives the load: the first document set it and lost its bridge; this one kept its own.
const now = await evalIn(KEY, `return window.name + ' ' + typeof window.__electrobunSendToHost + ' ' + typeof window.fennecHost`);
assert.equal(now.value, 'fennec-check-bridgeless function object', 'the document after the bridgeless one, with its bridge and its session');
await quit(app);

step('the control: without the guard the same window keeps the document with no bridge, and says nothing');
app = startApp('latepreload-control', { FENNEC_DESKTOP_OPEN: SMALL, FENNEC_DESKTOP_BRIDGELESS_FIRST: '1', FENNEC_DESKTOP_NO_PRELOAD_GUARD: '1' });
await waitFor((l) => l.what === 'did-navigate' && l.key === KEY, 60000, 'the window loaded');
// The guard's load again came within a second of the first in every run above; five is the wait for what must not come.
await waitNone((l) => l.key === KEY && l.detail?.type === 'state', 5000, 'a document with no bridge reported');
assert.equal(loads(), 1, 'loaded once, and left so');
await quit(app);
log('clean');

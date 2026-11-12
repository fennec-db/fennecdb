// A studio window is a page a server serves, so it is held to what the
// studio needs: no bridge to the main process, no leaving its server's
// origin, links out go to the browser (here: reported, not opened).
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { MAC, WIN, assertClean, cmd, data, evalIn, lines, quit, startApp, step, until, waitFor, waitNone, waitStudio, webviewWindows, log } from './lib.mjs';

assertClean('before');
const SMALL = data('small', 50);
const KEY = `file:${SMALL}`;

const app = startApp('sandbox', { FENNEC_DESKTOP_OPEN: SMALL });
const started = await waitFor((l) => l.what === 'server-started', 60000, 'the server started');
await waitStudio(KEY);

step('no RPC bridge to the main process in the studio window');
const globals = await evalIn(KEY, `return Object.getOwnPropertyNames(window).filter(k => /electrobun/i.test(k)).sort()`);
log('globals with electrobun in the name:', JSON.stringify(globals.value));
// What a sandboxed view's preload (preload-sandboxed.js) defines, and no more:
// the page's way out is the event bridge, which carries events and host
// messages one way and calls nothing. The full preload adds the RPC bridge to
// Bun; if a later Electrobun puts anything else in a sandboxed page, this
// says so before a release does.
const SANDBOXED = ['__electrobunEventBridge', '__electrobunInternalBridge', '__electrobunPlatform', '__electrobunSendToHost', '__electrobunWebviewId', '__electrobunWindowId'];
assert.deepEqual(globals.value.filter((k) => !SANDBOXED.includes(k)), [], 'nothing beyond the sandboxed preload');
const rpc = await evalIn(KEY, `return [typeof window.__electrobunBunBridge, typeof window.electrobun, typeof window.__electrobunRpcSocketPort, typeof window.__electrobunSecretKey].join(',')`);
assert.equal(rpc.value, 'undefined,undefined,undefined,undefined');

step('the session is handed over as the studio asks: window.fennecHost, frozen, in the page\'s own world, and never stored');
const host = await evalIn(KEY, `
const d = Object.getOwnPropertyDescriptor(window, 'fennecHost');
const h = window.fennecHost;
const buttons = [...document.querySelectorAll('button')].map(b => b.textContent.trim());
return {
  defined: !!d,
  descriptor: d && { writable: d.writable, enumerable: d.enumerable, configurable: d.configurable },
  frozen: !!h && Object.isFrozen(h) && Object.isFrozen(h.session),
  server: h && h.session.server,
  origin: location.origin,
  tokenLength: h && h.session.token.length,
  tenant: h && h.session.tenant,
  sessionStorage: Object.keys(sessionStorage),
  localStorage: Object.keys(localStorage),
  signOut: buttons.includes('Sign out'),
  signIn: /Connect to a fennecdb server/.test(document.body.innerText),
};`);
assert.ok(host.ok, host.value);
assert.equal(host.value.defined, true, 'the preload ran in the same world as the page: the page sees it');
assert.deepEqual(host.value.descriptor, { writable: false, enumerable: false, configurable: false });
assert.equal(host.value.frozen, true);
assert.equal(host.value.server, host.value.origin, 'the origin that served the page');
assert.equal(host.value.tokenLength, 43, '32 random bytes, base64url');
assert.equal(host.value.tenant, null);
assert.deepEqual(host.value.sessionStorage, [], 'the token is not in session storage');
assert.deepEqual(host.value.localStorage.filter((k) => /token|session|fennec-studio/i.test(k)), [], 'nor local storage');
assert.equal(host.value.signOut, false, 'under an app the studio has no Sign out: the app holds the connection');
assert.equal(host.value.signIn, false, 'and no sign-in form');

step("navigating away from the server's origin is refused");
const m1 = lines().length;
await evalIn(KEY, `location.href = "https://example.com/"; return 1`);
const nav = await waitFor((l, i) => i >= m1 && l.what === 'will-navigate' && /example\.com/.test(l.data), 10000, 'the navigation being asked about');
assert.match(nav.data, /allowed[^,}]*false/, `refused: ${nav.data}`);
const where = await evalIn(KEY, `return location.href + ' | ' + document.title`);
log('after location.href = https://example.com:', where.value);
assert.ok(where.value.startsWith(started.origin), `still on ${started.origin}: ${where.value}`);

// WebKit on macOS hands a script's `window.open` to the app as `new-window-open`.
// On Windows the app's preload takes it first and sends it over the event bridge
// (`link-out`, src/bun/opener.ts): Electrobun handles no WebView2 NewWindowRequested,
// and WebView2 then opens a window of its own, which outlived the app on both
// runners. WebKitGTK raises nothing for a popup no user gesture asked for, so
// there the page-to-app hop is not shown, only that nothing was opened and the
// window stayed. The filter it reaches is the same code on every system:
// test/opener.test.ts holds it (web addresses alone, one in three seconds), and
// the `open-link` hook below drives it in the running app on Linux.
const RAISED = MAC || WIN;
const askedOf = (l) => (l.what === 'new-window-open' && /file:|fennec-check:/.test(l.data)) || (l.what === 'page' && l.key === KEY && l.detail?.type === 'link-out' && /^(file:|fennec-check:)/.test(l.detail.url));
// Another origin, as a page in the studio would reach it: one answer that
// allows any reader, one that allows none, and a page for a frame. On Windows
// Electrobun starts WebView2 with --disable-web-security, so the same-origin
// policy reads nothing for a page there (hostile.mjs); what holds the studio's
// reads is its own CSP (connect-src 'self', default-src 'self' for frames,
// crates/fennec-http/src/studio.rs), which refuses a request before it leaves.
const otherAsked = [];
const other = createServer((req, res) => {
  otherAsked.push(req.url);
  const open = req.url === '/shared' ? { 'access-control-allow-origin': '*' } : {};
  res.writeHead(200, { 'content-type': req.url === '/frame' ? 'text/html' : 'text/plain', ...open });
  res.end(req.url === '/frame' ? '<!doctype html><title>another origin</title>' : 'not for the studio');
});
const OTHER = await new Promise((r) => other.listen(0, '127.0.0.1', () => r(`http://127.0.0.1:${other.address().port}`)));
step("the studio reads no answer of another origin, one that allows any reader neither: its CSP refuses fetch, XHR and a frame before a request leaves, on every system");
const reads = await evalIn(
  KEY,
  `const out = {};
const get = async (u) => { try { const r = await fetch(u); return 'read ' + r.status; } catch (e) { return 'refused'; } };
out.own = await get(location.href);
out.shared = await get(${JSON.stringify(`${OTHER}/shared`)});
out.secret = await get(${JSON.stringify(`${OTHER}/secret`)});
try { const x = new XMLHttpRequest(); x.open('GET', ${JSON.stringify(`${OTHER}/shared`)}, false); x.send(); out.xhr = 'read ' + x.status; } catch (e) { out.xhr = 'refused'; }
const f = document.createElement('iframe'); f.src = ${JSON.stringify(`${OTHER}/frame`)};
f.addEventListener('load', () => { try { window.__frameRead = 'read ' + f.contentDocument.title; } catch (e) { window.__frameRead = 'refused'; } }, { once: true });
document.body.append(f);
return out;`,
  30000,
);
// A refused frame may still raise load, on an error page; one the window's rules refuse raises none.
reads.value.frame = await until(async () => (await evalIn(KEY, `return window.__frameRead || ''`)).value, 5000, 'the frame').catch(() => 'not loaded');
log('what the studio read:', JSON.stringify(reads.value), '| the other origin was asked:', JSON.stringify(otherAsked));
assert.equal(reads.value.own, 'read 200', "the control: the studio's own origin answers its fetch");
assert.equal(reads.value.shared, 'refused', 'fetch: an answer of another origin, though it allows any reader');
assert.equal(reads.value.secret, 'refused', 'fetch: one that allows none');
assert.equal(reads.value.xhr, 'refused', 'XHR: the same');
// WebKit may leave a refused frame an empty document of the studio's own: what must not be read is the other origin's.
assert.notEqual(reads.value.frame, 'read another origin', 'a frame of another origin: its document');
assert.deepEqual(otherAsked, [], 'no request reached the other origin: the CSP refused each before it left, not the same-origin policy after');
other.close();

step('an address that is not http(s) is not handed to the system, and the first one that is, is: the address was what held the others back');
// Before any web link, so that the rate limit below has nothing to do with it.
const mNon = lines().length;
await evalIn(KEY, `window.open("file:///etc/passwd"); window.open("javascript:alert(1)"); window.open("fennec-check://x/y"); return 1`);
await waitNone((l, i) => i >= mNon && l.what === 'external', 1500, 'a non-web link was handed to the system');
const asked = lines().slice(mNon).filter(askedOf);
log('asked of the app and not handed on:', asked.length, 'of the 3');
if (RAISED) {
  // The page did ask: the app was told of at least one of them, and dropped it.
  assert.ok(asked.length > 0, 'the app was asked to open a non-web address, so the filter is what refused it');
  // The control: a web link, the window's first, goes to the browser at once.
  const mWeb = lines().length;
  await evalIn(KEY, `window.open("https://example.com/"); return 1`);
  const ext = await waitFor((l, i) => i >= mWeb && l.what === 'external', 10000, 'the link out');
  assert.equal(ext.url, 'https://example.com/');
} else {
  log(`${process.platform}: a script's window.open raised no new-window-open (${asked.length} seen); the app's filter, asked directly:`);
  for (const [url, link] of [
    ['file:///etc/passwd', null],
    ['fennec-check://x/y', null],
    ['https://example.com/?q=$(id)', 'https://example.com/?q=%24(id)'],
  ]) {
    const m = lines().length;
    cmd({ cmd: 'open-link', url });
    const out = await waitFor((l, i) => i >= m && l.what === 'link-out' && l.url === url, 10000, `the filter's answer for ${url}`);
    assert.equal(out.link, link, url);
    if (link) assert.equal((await waitFor((l, i) => i >= m && l.what === 'external', 10000, 'the link out')).url, link);
  }
}
assert.ok((await evalIn(KEY, `return location.href`)).value.startsWith(started.origin), 'the window stayed');

step('a page cannot ask for links without end: a window is given one in three seconds');
const mRepeat = lines().length;
await evalIn(KEY, `window.open("https://example.com/a"); window.open("https://example.com/b"); return 1`);
await waitNone((l, i) => i >= mRepeat && l.what === 'external', 1500, 'a second link within the gap was handed to the browser');
if (WIN) {
  // Both were asked of the app (the gap held them back, not WebView2), and no window of WebView2's own was opened for any request above.
  assert.equal(lines().slice(mRepeat).filter((l) => l.what === 'page' && l.key === KEY && l.detail?.type === 'link-out').length, 2, 'both links reached the app');
  assert.deepEqual(webviewWindows(), [], 'no window WebView2 opened by itself');
}

step('quit');
await quit(app);
log('clean');

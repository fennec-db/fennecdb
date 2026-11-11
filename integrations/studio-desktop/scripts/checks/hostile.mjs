// A saved server's page is held as the studio is, whatever it does: it reads
// no response of another origin that did not allow it, and opens no window of
// its own, from its document or from any frame it makes.
//
// The page is a stand-in's, a server that answers the app's probe as
// fennec-server does and serves a page with no CSP: fennec-server's studio
// has one (connect-src 'self', no inline script), which would refuse what the
// page tries here before the window's own rules were asked. Every request for
// a new window names an address of the stand-in's own, /popup/<kind>, so a
// window that opened and loaded it is seen there whatever drew it; on Windows
// the visible windows of WebView2's processes are listed too.
//
// On Windows both rest on the arguments the app adds to WebView2's
// (src/bun/webview2.ts): Electrobun 2.0.4 starts it with --disable-web-security
// and leaves a request for a new window to it, which opens a window of its own.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { WIN, assertClean, cmd, evalIn, lines, quit, sleep, startApp, step, waitFor, webviewWindows, log } from './lib.mjs';

assertClean('before');

/** What the stand-ins were asked, in order. */
const asked = [];
const html = (res, body, headers = {}) => {
  res.writeHead(200, { 'content-type': 'text/html; charset=utf-8', ...headers });
  res.end(body);
};
// The saved server: its studio is the page under test, and it needs no token (its /_whoami answers 200).
const page = createServer((req, res) => {
  asked.push(req.url);
  if (req.url.startsWith('/_studio/')) return html(res, '<!doctype html><title>hostile</title><body><p>a page of a server the app does not know</p></body>');
  if (req.url === '/_whoami') {
    res.writeHead(200, { 'content-type': 'application/json' });
    return res.end('{"role":"admin"}');
  }
  if (req.url.startsWith('/popup/')) return html(res, `<!doctype html><title>popup ${req.url.slice(7)}</title>`);
  if (req.url === '/frame') return html(res, '<!doctype html><title>a frame of its own origin</title>');
  res.writeHead(404);
  res.end();
});
// Another origin: one answer that allows any reader (the control), one that allows none.
const other = createServer((req, res) => {
  const u = new URL(req.url, 'http://x');
  asked.push(`other${req.url}`);
  if (u.pathname === '/secret') {
    res.writeHead(200, { 'content-type': 'text/plain' });
    return res.end('not for that page');
  }
  if (u.pathname === '/shared') {
    res.writeHead(200, { 'content-type': 'text/plain', 'access-control-allow-origin': '*' });
    return res.end('for anyone');
  }
  if (u.pathname === '/frame') return html(res, '<!doctype html><title>another origin</title>');
  // A frame of another origin that asks for a window itself, and says what it was answered.
  if (u.pathname === '/opener') return html(res, opener(u.searchParams.get('k'), u.searchParams.get('u')));
  res.writeHead(404);
  res.end();
});
const listen = (s) => new Promise((r) => s.listen(0, '127.0.0.1', () => r(`http://127.0.0.1:${s.address().port}`)));
const PAGE = await listen(page);
const OTHER = await listen(other);
const popup = (k) => `${PAGE}/popup/${k}`;

/** A document's script that asks for `url` in a window and posts what it was answered, as kind `k`, to its parent. */
function opener(k, url) {
  return `<!doctype html><script>var r;try{r=open(${JSON.stringify(url)});r=r===null?"null":typeof r}catch(e){r="threw "+e}parent.postMessage({k:${JSON.stringify(k)},returned:r},"*")</script>`;
}

const app = startApp('hostile');
await waitFor((l) => l.what === 'ready', 60000, 'ready');
let n = lines().length;
cmd({ cmd: 'addconn', name: 'hostile', url: PAGE, token: '', tenant: '' });
const id = (await waitFor((l, i) => i >= n && l.what === 'conn-added' && l.name === 'hostile', 10000, 'saved')).id;
const KEY = `remote:${id}`;
n = lines().length;
cmd({ cmd: 'openconn', id });
assert.equal((await waitFor((l, i) => i >= n && l.what === 'openconn-result' && l.id === id, 20000, 'opened')).ok, true);
await waitFor((l, i) => i >= n && l.key === KEY && l.detail?.type === 'state' && l.detail.title === 'hostile', 30000, 'the page drawn');

step("another origin's answer is read only where it allows it: fetch, XHR and a frame's document");
const reads = await evalIn(
  KEY,
  `const out = {};
const get = async (u) => { try { const r = await fetch(u); return 'read ' + (await r.text()); } catch (e) { return 'refused'; } };
out.shared = await get(${JSON.stringify(`${OTHER}/shared`)});
out.secret = await get(${JSON.stringify(`${OTHER}/secret`)});
try { const x = new XMLHttpRequest(); x.open('GET', ${JSON.stringify(`${OTHER}/secret`)}, false); x.send(); out.xhr = 'read ' + x.responseText; } catch (e) { out.xhr = 'refused'; }
const f = document.createElement('iframe'); f.src = ${JSON.stringify(`${OTHER}/frame`)};
const loaded = new Promise((r) => { f.addEventListener('load', r, { once: true }); setTimeout(r, 5000); }); document.body.append(f); await loaded;
try { const at = f.contentWindow.location.href; out.frame = at === 'about:blank' ? 'not loaded' : 'read ' + f.contentDocument.title; } catch (e) { out.frame = 'refused'; }
f.remove();
return out;`,
  30000,
);
log('what the page read:', JSON.stringify(reads.value));
assert.equal(reads.value.shared, 'read for anyone', 'the control: an answer that allows any reader is read, so the page can fetch at all');
assert.equal(reads.value.secret, 'refused', "fetch: an answer of another origin that does not allow it");
assert.equal(reads.value.xhr, 'refused', 'XHR: the same');
// A frame of another origin is not even loaded on macOS, whose navigation rules Electrobun asks for a frame's too.
if (reads.value.frame === 'not loaded') assert.ok(!asked.includes('other/frame'), 'a frame that did not load was not asked for');
else assert.equal(reads.value.frame, 'refused', 'a frame of another origin: its document');
assert.ok(asked.includes('other/secret'), 'the request went out: what refused it was the page being held to its origin');

step('no window is opened for the page, from its document or from any frame it makes');
// Each kind: what it does, run in the page; it answers what it was told, where it can.
const FRAME = `const answer = (w) => (w === null ? 'null' : w === undefined ? 'undefined' : typeof w);
const loaded = (f) => new Promise((r) => { f.addEventListener('load', r, { once: true }); setTimeout(r, 5000); });
const posted = (k) => new Promise((r) => { const t = setTimeout(() => r('no answer'), 5000); addEventListener('message', (e) => { if (e.data && e.data.k === k) { clearTimeout(t); r(e.data.returned); } }); });`;
const KINDS = {
  'the document': `return answer(window.open(${JSON.stringify(popup('document'))}));`,
  'a link with a target': `const a = document.createElement('a'); a.href = ${JSON.stringify(popup('link'))}; a.target = '_blank'; document.body.append(a); a.click(); return 'clicked';`,
  "a link in a shadow root, a script's click": `const h = document.createElement('div'); document.body.append(h); const s = h.attachShadow({ mode: 'closed' }); const a = document.createElement('a'); a.href = ${JSON.stringify(popup('shadow'))}; a.target = '_blank'; s.append(a); a.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true })); return 'dispatched';`,
  'a link under <base target>': `const b = document.createElement('base'); b.target = '_blank'; document.head.append(b); const a = document.createElement('a'); a.href = ${JSON.stringify(popup('base'))}; document.body.append(a); a.click(); b.remove(); return 'clicked';`,
  'a form with a target, submitted by script': `const f = document.createElement('form'); f.method = 'get'; f.target = '_blank'; f.action = ${JSON.stringify(popup('form'))}; document.body.append(f); f.submit(); return 'submitted';`,
  'a new frame, at once': `const f = document.createElement('iframe'); document.body.append(f); return answer(f.contentWindow.open(${JSON.stringify(popup('frame-at-once'))}));`,
  'a new frame, by its index': `document.body.append(document.createElement('iframe')); return answer(window[window.length - 1].open(${JSON.stringify(popup('frame-index'))}));`,
  'a frame of about:blank, loaded': `const f = document.createElement('iframe'); f.src = 'about:blank'; const l = loaded(f); document.body.append(f); await l; return answer(f.contentWindow.open(${JSON.stringify(popup('frame-blank'))}));`,
  'a frame of srcdoc': `const f = document.createElement('iframe'); f.srcdoc = '<p>x</p>'; const l = loaded(f); document.body.append(f); await l; return answer(f.contentWindow.open(${JSON.stringify(popup('frame-srcdoc'))}));`,
  "a frame of srcdoc, its own script": `const p = posted('srcdoc-script'); const f = document.createElement('iframe'); f.srcdoc = ${JSON.stringify(opener('srcdoc-script', popup('srcdoc-script')))}; document.body.append(f); return await p;`,
  'a frame written by document.write': `const p = posted('written'); const f = document.createElement('iframe'); document.body.append(f); f.contentDocument.write(${JSON.stringify(opener('written', popup('written')))}); f.contentDocument.close(); return await p;`,
  // With three arguments document.open is window.open; with fewer it would answer its document.
  "a frame's document.open with three arguments": `const f = document.createElement('iframe'); document.body.append(f); const d = f.contentDocument; const w = d.open(${JSON.stringify(popup('document-open'))}, '', ''); return w === d ? 'its document' : answer(w);`,
  'a frame in a frame': `const f = document.createElement('iframe'); document.body.append(f); const g = f.contentDocument.createElement('iframe'); f.contentDocument.body.append(g); return answer(g.contentWindow.open(${JSON.stringify(popup('nested'))}));`,
  'a frame of its own origin, loaded': `const f = document.createElement('iframe'); f.src = '/frame'; const l = loaded(f); document.body.append(f); await l; return answer(f.contentWindow.open(${JSON.stringify(popup('same-origin'))}));`,
  'a frame of a data: address': `const p = posted('data'); const f = document.createElement('iframe'); f.src = 'data:text/html,' + encodeURIComponent(${JSON.stringify(opener('data', popup('data')))}); document.body.append(f); return await p;`,
  'a frame of a blob: address': `const p = posted('blob'); const f = document.createElement('iframe'); f.src = URL.createObjectURL(new Blob([${JSON.stringify(opener('blob', popup('blob')))}], { type: 'text/html' })); document.body.append(f); return await p;`,
  'a frame of another origin': `const p = posted('other-origin'); const f = document.createElement('iframe'); f.src = ${JSON.stringify(`${OTHER}/opener?k=other-origin&u=${encodeURIComponent(popup('other-origin'))}`)}; document.body.append(f); return await p;`,
};
const answers = {};
for (const [kind, js] of Object.entries(KINDS)) {
  const r = await evalIn(KEY, `${FRAME}\n${js}`, 20000);
  assert.ok(r.ok, `${kind}: ${r.value}`);
  answers[kind] = r.value;
}
log('what each was answered:', JSON.stringify(answers, null, 1));
// What must not happen: a window loading one of the addresses. A window opens and loads within a
// second of its request on every runner; the wait is for what must not come.
await sleep(3000);
const opened = asked.filter((u) => u.startsWith('/popup/'));
assert.deepEqual(opened, [], 'no window loaded any of the addresses asked for');
if (WIN) assert.deepEqual(webviewWindows(), [], 'and WebView2 drew no window of its own');
for (const [kind, said] of Object.entries(answers)) {
  if (/^(object|undefined)$/.test(said)) assert.fail(`${kind}: the page was handed a window (${said})`);
}

step('quit');
await quit(app);
page.close();
other.close();
log('clean');

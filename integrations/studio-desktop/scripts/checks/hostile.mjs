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
// Windows. Electrobun 2.0.4 leaves a request for a new window to WebView2,
// which opens one of its own, so the app's preload takes each way a page has
// of asking (src/bun/opener.ts, `newWindowPreload`), in every frame. And it
// starts WebView2 with --disable-web-security, which no variable takes away
// (WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS only adds to it): there the page reads
// another origin's answers, so the reads are not held on Windows while
// WebView2's browser has that switch, which is asserted -- an Electrobun that
// drops it fails this check until they are held there too.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { WIN, assertClean, cmd, evalIn, lines, quit, sleep, startApp, step, until, waitFor, webviewBrowserSwitches, webviewWindows, log } from './lib.mjs';

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

// Windows: WebView2 runs with the same-origin policy off; the reads below wait for an Electrobun that lets it on.
const unheld = WIN ? webviewBrowserSwitches() : [];
if (WIN) {
  log(`WebView2's browser processes' switches:\n${unheld.map((s) => `  ${s.filter((x) => !/user-data-dir|mojo|field-trial|metrics|variations|webview-exe/.test(x)).join(' ')}`).join('\n')}`);
  assert.ok(unheld.length > 0, 'a browser process of the app');
  assert.ok(
    unheld.every((switches) => switches.includes('--disable-web-security')),
    'WebView2 no longer runs with --disable-web-security: hold the reads below on Windows too, and say so in docs/design/integrations.md#studio-desktop',
  );
}

step("another origin's answer is read only where it allows it: fetch, XHR and a frame's document");
const reads = await evalIn(
  KEY,
  `const out = {};
const get = async (u) => { try { const r = await fetch(u); return 'read ' + (await r.text()); } catch (e) { return 'refused'; } };
out.shared = await get(${JSON.stringify(`${OTHER}/shared`)});
out.secret = await get(${JSON.stringify(`${OTHER}/secret`)});
try { const x = new XMLHttpRequest(); x.open('GET', ${JSON.stringify(`${OTHER}/secret`)}, false); x.send(); out.xhr = 'read ' + x.responseText; } catch (e) { out.xhr = 'refused'; }
const f = document.createElement('iframe'); f.src = ${JSON.stringify(`${OTHER}/frame`)};
f.addEventListener('load', () => { try { window.__frameRead = 'read ' + f.contentDocument.title; } catch (e) { window.__frameRead = 'refused'; } }, { once: true });
document.body.append(f);
return out;`,
  30000,
);
// The frame answers when it loads, if it does: macOS's navigation rules, which Electrobun asks for a frame's too, refuse it.
reads.value.frame = await until(async () => (await evalIn(KEY, `return window.__frameRead || ''`)).value, 5000, 'the frame').catch(() => 'not loaded');
log('what the page read:', JSON.stringify(reads.value));
assert.equal(reads.value.shared, 'read for anyone', 'the control: an answer that allows any reader is read, so the page can fetch at all');
assert.ok(asked.includes('other/secret'), 'the request went out: what refuses it is the page being held to its origin');
if (WIN) log('on Windows, where WebView2 runs with --disable-web-security, the reads are not held (above)');
else {
  assert.equal(reads.value.secret, 'refused', 'fetch: an answer of another origin that does not allow it');
  assert.equal(reads.value.xhr, 'refused', 'XHR: the same');
  if (reads.value.frame === 'not loaded') assert.ok(!asked.includes('other/frame'), 'a frame that did not load was not asked for');
  else assert.equal(reads.value.frame, 'refused', 'a frame of another origin: its document');
}

step('no window is opened for the page, from its document or from any frame it makes');
// Each kind: what it does, run in the page. What a frame's own script or a
// frame once loaded was answered is kept in window.__answers when it comes,
// read once all have run: no kind waits on a timer of the page's (WebKit
// throttles them in a window behind others, and a frame the window's rules
// refuse never loads).
const FRAME = `const answers = (window.__answers = window.__answers || {});
const answer = (w) => (w === null ? 'null' : w === undefined ? 'undefined' : typeof w);
const onLoad = (f, k, open) => f.addEventListener('load', () => { answers[k] = answer(open()); }, { once: true });
const posted = (k) => addEventListener('message', (e) => { if (e.data && e.data.k === k) answers[k] = e.data.returned; });`;
const KINDS = {
  'the document': `return answer(window.open(${JSON.stringify(popup('document'))}));`,
  'a link with a target': `const a = document.createElement('a'); a.href = ${JSON.stringify(popup('link'))}; a.target = '_blank'; document.body.append(a); a.click(); return 'clicked';`,
  'a link outside the document, clicked by script': `const a = document.createElement('a'); a.href = ${JSON.stringify(popup('detached'))}; a.target = '_blank'; a.click(); return 'clicked';`,
  "a link in a shadow root, a script's click": `const h = document.createElement('div'); document.body.append(h); const s = h.attachShadow({ mode: 'closed' }); const a = document.createElement('a'); a.href = ${JSON.stringify(popup('shadow'))}; a.target = '_blank'; s.append(a); return String(a.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: false })));`,
  "a link in a shadow root, a script's click at its text": `const h = document.createElement('div'); document.body.append(h); const s = h.attachShadow({ mode: 'closed' }); const a = document.createElement('a'); a.href = ${JSON.stringify(popup('shadow-text'))}; a.target = '_blank'; a.textContent = 'x'; s.append(a); return String(a.firstChild.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true })));`,
  'a link under <base target>': `const b = document.createElement('base'); b.target = '_blank'; document.head.append(b); const a = document.createElement('a'); a.href = ${JSON.stringify(popup('base'))}; document.body.append(a); a.click(); b.remove(); return 'clicked';`,
  "an image map's area": `const m = document.createElement('map'); m.name = 'm'; const a = document.createElement('area'); a.shape = 'default'; a.href = ${JSON.stringify(popup('area'))}; a.target = '_blank'; m.append(a); document.body.append(m); a.click(); return 'clicked';`,
  "SVG's link": `const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); const a = document.createElementNS('http://www.w3.org/2000/svg', 'a'); a.setAttribute('href', ${JSON.stringify(popup('svg'))}); a.setAttribute('target', '_blank'); const r = document.createElementNS('http://www.w3.org/2000/svg', 'rect'); r.setAttribute('width', '10'); r.setAttribute('height', '10'); a.append(r); svg.append(a); document.body.append(svg); return String(r.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true })));`,
  'a form with a target, submitted by script': `const f = document.createElement('form'); f.method = 'get'; f.target = '_blank'; f.action = ${JSON.stringify(popup('form'))}; document.body.append(f); f.submit(); return 'submitted';`,
  "a form's requestSubmit, by a button with a formtarget": `const f = document.createElement('form'); f.method = 'get'; f.action = ${JSON.stringify(popup('formtarget'))}; const b = document.createElement('button'); b.formTarget = '_blank'; f.append(b); document.body.append(f); f.requestSubmit(b); return 'submitted';`,
  "a form's button, clicked by script": `const f = document.createElement('form'); f.method = 'get'; f.target = '_blank'; f.action = ${JSON.stringify(popup('button'))}; const b = document.createElement('button'); f.append(b); document.body.append(f); b.click(); return 'clicked';`,
  'a form in a shadow root, submitted by request': `const h = document.createElement('div'); document.body.append(h); const s = h.attachShadow({ mode: 'closed' }); const f = document.createElement('form'); f.method = 'get'; f.target = '_blank'; f.action = ${JSON.stringify(popup('shadow-form'))}; s.append(f); f.requestSubmit(); return 'submitted';`,
  'a new frame, at once': `const f = document.createElement('iframe'); document.body.append(f); return answer(f.contentWindow.open(${JSON.stringify(popup('frame-at-once'))}));`,
  'a new frame, by its index': `document.body.append(document.createElement('iframe')); return answer(window[window.length - 1].open(${JSON.stringify(popup('frame-index'))}));`,
  'a frame of about:blank, loaded': `const f = document.createElement('iframe'); f.src = 'about:blank'; onLoad(f, 'frame-blank', () => f.contentWindow.open(${JSON.stringify(popup('frame-blank'))})); document.body.append(f); return 'made';`,
  'a frame of srcdoc': `const f = document.createElement('iframe'); f.srcdoc = '<p>x</p>'; onLoad(f, 'frame-srcdoc', () => f.contentWindow.open(${JSON.stringify(popup('frame-srcdoc'))})); document.body.append(f); return 'made';`,
  'a frame of srcdoc, its own script': `posted('srcdoc-script'); const f = document.createElement('iframe'); f.srcdoc = ${JSON.stringify(opener('srcdoc-script', popup('srcdoc-script')))}; document.body.append(f); return 'made';`,
  'a frame written by document.write': `posted('written'); const f = document.createElement('iframe'); document.body.append(f); f.contentDocument.write(${JSON.stringify(opener('written', popup('written')))}); f.contentDocument.close(); return 'written';`,
  // With three arguments document.open is window.open; with fewer it would answer its document.
  "a frame's document.open with three arguments": `const f = document.createElement('iframe'); document.body.append(f); const d = f.contentDocument; const w = d.open(${JSON.stringify(popup('frame-document-open'))}, '', ''); return w === d ? 'its document' : answer(w);`,
  "a frame's link outside its document, clicked": `const f = document.createElement('iframe'); document.body.append(f); const a = f.contentDocument.createElement('a'); a.href = ${JSON.stringify(popup('frame-detached'))}; a.target = '_blank'; a.click(); return 'clicked';`,
  "a frame's form, submitted by script": `const f = document.createElement('iframe'); document.body.append(f); const d = f.contentDocument; const fm = d.createElement('form'); fm.method = 'get'; fm.target = '_blank'; fm.action = ${JSON.stringify(popup('frame-form'))}; d.body.append(fm); fm.submit(); return 'submitted';`,
  'a frame in a frame': `const f = document.createElement('iframe'); document.body.append(f); const g = f.contentDocument.createElement('iframe'); f.contentDocument.body.append(g); return answer(g.contentWindow.open(${JSON.stringify(popup('nested'))}));`,
  'a frame of its own origin, loaded': `const f = document.createElement('iframe'); f.src = '/frame'; onLoad(f, 'same-origin', () => f.contentWindow.open(${JSON.stringify(popup('same-origin'))})); document.body.append(f); return 'made';`,
  'a frame of a data: address': `posted('data'); const f = document.createElement('iframe'); f.src = 'data:text/html,' + encodeURIComponent(${JSON.stringify(opener('data', popup('data')))}); document.body.append(f); return 'made';`,
  'a frame of a blob: address': `posted('blob'); const f = document.createElement('iframe'); f.src = URL.createObjectURL(new Blob([${JSON.stringify(opener('blob', popup('blob')))}], { type: 'text/html' })); document.body.append(f); return 'made';`,
  'a frame of another origin': `posted('other-origin'); const f = document.createElement('iframe'); f.src = ${JSON.stringify(`${OTHER}/opener?k=other-origin&u=${encodeURIComponent(popup('other-origin'))}`)}; document.body.append(f); return 'made';`,
  // Last: on a page whose document.open took fewer arguments it would be emptied for writing.
  "the document's document.open with three arguments": `const w = document.open(${JSON.stringify(popup('document-open'))}, '', ''); return w === document ? 'its document' : answer(w);`,
};
const said = {};
for (const [kind, js] of Object.entries(KINDS)) {
  const r = await evalIn(KEY, `${FRAME}\n${js}`, 20000);
  assert.ok(r.ok, `${kind}: ${r.value}`);
  said[kind] = r.value;
}
// What must not happen: a window loading one of the addresses. A window opened and loaded its
// address within a second of the request on both Windows runners (the spike before this check);
// the wait is for what must not come.
await sleep(3000);
const later = (await evalIn(KEY, `return window.__answers || {}`)).value;
log('what each was answered:', JSON.stringify({ ...said, 'later, from a frame': later }, null, 1));
const opened = asked.filter((u) => u.startsWith('/popup/'));
assert.deepEqual(opened, [], 'no window loaded any of the addresses asked for');
if (WIN) assert.deepEqual(webviewWindows(), [], 'and WebView2 drew no window of its own');
for (const [kind, answer] of Object.entries({ ...said, ...later })) {
  if (/^(object|undefined)$/.test(answer)) assert.fail(`${kind}: the page was handed a window (${answer})`);
}

step('quit');
await quit(app);
page.close();
other.close();
log('clean');

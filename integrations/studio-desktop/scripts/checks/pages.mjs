// The app's own pages are held to themselves. The launcher and the
// new-database window have the RPC bridge to the main process, and Electrobun
// adds that bridge to every document a window loads: a link or a file dropped
// on one is a navigation, and a page it loaded would be handed `openPath`,
// `openConnection` and the saved servers (and, with `openConnection` and
// `overview`, a way to a token). So each is held to its own views:// folder
// by navigation rules, and a navigation anywhere else, a web page, a file, or
// another of the app's pages, is refused. (sandbox.mjs holds a studio window.)
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { BUN, EXE, HERE, LINUX, REPORT, WIN, assertClean, cmd, evalIn, file, lines, quit, startApp, step, waitFor, waitNone, waitNotice, log } from './lib.mjs';

assertClean('before');

const app = startApp('pages', { FENNEC_DESKTOP_LAUNCHER: '1' });
await waitFor((l) => l.what === 'ready', 60000, 'ready');
await waitFor((l) => l.what === 'page-text' && l.key === 'launcher' && /Open file/.test(l.text), 20000, 'the launcher drawn');
cmd({ cmd: 'newdb' });
await waitFor((l) => l.what === 'page-text' && l.key === 'newdb' && /New database/.test(l.text), 20000, 'the new-database window drawn');
const n = lines().length;
cmd({ cmd: 'open', path: file('nothing-here') });
const notice = await waitNotice('That file is gone', 15000, n);

const where = (key) => evalIn(key, `return location.href`);

/**
 * `href` asked of window `key`: refused, and the window is where it was. A web
 * page, or another of the app's, reaches the navigation rules, which report
 * the question and the answer (`allowed: false`). A file: address does not:
 * WebKit will not take a page of another scheme there, and asks nothing; for it
 * the window must simply not have moved.
 */
async function refused(key, href, from) {
  const m = lines().length;
  await evalIn(key, `location.href = ${JSON.stringify(href)}; return 1`);
  if (href.startsWith('file:')) {
    await waitNone((l, i) => i >= m && l.key === key && ((l.what === 'did-navigate') || (l.what === 'will-navigate' && /allowed[^,}]*true/.test(l.data))), 1500, `${key} went to ${href}`);
  } else {
    const nav = await waitFor((l, i) => i >= m && l.what === 'will-navigate' && l.key === key && l.data.includes(href.split('?')[0]), 10000, `${key} being asked to go to ${href}`);
    assert.match(nav.data, /allowed[^,}]*false/, `${key} -> ${href} must be refused: ${nav.data}`);
  }
  const now = await where(key);
  assert.equal(now.value, from, `${key} stays where it was after asking for ${href}`);
}

step('the launcher cannot be taken anywhere else');
const launcherAt = (await where('launcher')).value;
assert.match(launcherAt, /^views:\/\/launcher\/index\.html/);
await refused('launcher', 'https://example.com/', launcherAt);
await refused('launcher', 'file:///etc/hosts', launcherAt);
// Another page of the app has an RPC bridge of its own: not a place for this one to go to.
await refused('launcher', 'views://newdb/index.html', launcherAt);
// And it still works: nothing it needs was taken away by being refused.
const ok = await evalIn('launcher', `return document.getElementById('open') !== null`);
assert.equal(ok.value, true);

step('the new-database window, the same');
const newdbAt = (await where('newdb')).value;
assert.match(newdbAt, /^views:\/\/newdb\/index\.html/);
await refused('newdb', 'https://example.com/', newdbAt);
await refused('newdb', 'views://launcher/index.html', newdbAt);

step('and a notice window');
const noticeAt = (await where(notice.key)).value;
assert.match(noticeAt, /^views:\/\/notice\/index\.html/);
await refused(notice.key, 'https://example.com/', noticeAt);
await refused(notice.key, 'views://launcher/index.html', noticeAt);

step('a link out of an app page is not handed to the browser: only a studio window does that');
const m = lines().length;
await evalIn('launcher', `window.open("https://example.com/"); return 1`);
await waitNone((l, i) => i >= m && l.what === 'external', 1200, 'a link from an app page was handed to the browser');

step('the app\'s own pages run their bundle alone: their CSP refuses eval and inline script');
// Eval is asked of the page itself: a string given to setTimeout is compiled by
// the page, in a task of its own, after the check's script has returned. The
// check's script is no page code: WebView2 runs it (ExecuteScript) as DevTools'
// Runtime.evaluate does, which lets the evaluated code eval past the page's CSP
// (its allowUnsafeEvalBlockedByCSP is on by default), so `new Function` there
// ran on Windows while the page's own would not. A function timer after it
// reports whether the string ran.
for (const key of ['launcher', 'newdb']) {
  const r = await evalIn(key, `return await new Promise((done) => { window.__evalRan = false; setTimeout("window.__evalRan = true", 0); setTimeout(() => done(window.__evalRan), 50); })`);
  assert.equal(r.value, false, `${key}: a string the page would eval, refused by its CSP`);
  const direct = await evalIn(key, `try { new Function('return 1')(); return 'ran'; } catch (e) { return 'refused'; }`);
  log(`${key}: new Function in the check's own script: ${direct.value}`);
  if (!WIN) assert.equal(direct.value, 'refused', `${key}: eval under the page's CSP`);
  const inline = await evalIn(key, `window.__inlineRan = false; const s = document.createElement('script'); s.textContent = 'window.__inlineRan = true'; document.head.append(s); return window.__inlineRan`);
  assert.equal(inline.value, false, `${key}: an inline script is not run`);
}

step('the one instance listens where only its user can: $TMPDIR, $XDG_RUNTIME_DIR, or a pipe of its user\'s SID that only that user may open');
const claim = await waitFor((l) => l.what === 'instance', 10000, 'the instance claimed');
log('the guard:', claim.path, claim.claim);
assert.equal(claim.claim, 'alone');
if (LINUX && process.env.XDG_RUNTIME_DIR) assert.equal(dirname(claim.path), process.env.XDG_RUNTIME_DIR, 'in the session\'s own folder, not /tmp');
if (WIN) {
  const sid = /\.instance\.(S-1-[0-9-]+)$/.exec(claim.path)?.[1];
  assert.ok(sid, `the pipe is named by its user's SID: ${claim.path}`);
  // The pipe's owner and DACL, read through the app's own call (win32.ts) by the app's own Bun.
  const script = `const w = await import(${JSON.stringify(pathToFileURL(join(HERE, 'src', 'bun', 'win32.ts')).href)}); console.log(w.pipeSddl(${JSON.stringify(claim.path)}));`;
  const read = spawnSync(BUN, ['-e', script], { encoding: 'utf8', timeout: 30000 });
  const sddl = read.stdout.trim();
  log('its security descriptor:', sddl, read.stderr.trim());
  assert.match(sddl, new RegExp(`D:P\\(A;;FA;;;${sid}\\)$`), 'protected, one entry: every right to its user, and none to anyone else');
}

step('a second instance asks this one to show itself and quits, opening nothing');
const k = lines().length;
const second = spawn(EXE, [], { env: { ...process.env, FENNEC_DESKTOP_REPORT: REPORT }, stdio: 'ignore' });
const secondExit = new Promise((r) => second.on('exit', r));
await waitFor((l, i) => i >= k && l.what === 'second-instance', 30000, 'the second instance found this one');
await waitFor((l, i) => i >= k && l.what === 'instance-asked', 30000, 'this one was asked to show itself');
await secondExit;
assert.ok(!lines().slice(k).some((l) => l.what === 'ready'), 'the second instance booted nothing');

step('quit');
await quit(app);
log('clean');

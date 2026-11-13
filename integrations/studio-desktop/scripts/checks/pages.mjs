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
import { BUN, EXE, HERE, LINUX, MAC, REPORT, WIN, assertClean, cmd, evalIn, file, lines, quit, startApp, step, until, waitFor, waitNone, waitNotice, webviewWindows, log } from './lib.mjs';

assertClean('before');

const app = startApp('pages', { FENNEC_DESKTOP_LAUNCHER: '1' });
await waitFor((l) => l.what === 'ready', 60000, 'ready');
await waitFor((l) => l.what === 'page-text' && l.key === 'launcher' && /Open file/.test(l.text), 20000, 'the launcher drawn');

step("the main window says what each system calls things: the computer, its file manager, its credential store");
{
  const words = MAC ? { machine: 'This Mac', reveal: 'Reveal in Finder' } : WIN ? { machine: 'This PC', reveal: 'Show in Explorer' } : { machine: 'This computer', reveal: 'Open its folder' };
  // Its words come with the first list it draws; the HTML's own are macOS's.
  await until(async () => (await evalIn('launcher', `return document.getElementById('db-reveal').textContent`)).value === words.reveal, 20000, `the file manager's button: ${words.reveal}`);
  // Every section's text, the hidden ones and the serve sheet's too.
  const text = (await evalIn('launcher', `return document.body.textContent.replace(/\\s+/g, ' ')`)).value;
  assert.ok(text.includes(words.machine) && text.includes(`${words.machine} only`), `the main window names the computer ${words.machine}`);
  if (!MAC) assert.doesNotMatch(text, /\bMac\b|Finder|Keychain/, 'no macOS word is left in the main window');
  log(`${words.machine}, ${words.reveal}`);
}
cmd({ cmd: 'newdb' });
await waitFor((l) => l.what === 'page-text' && l.key === 'newdb' && /New database/.test(l.text), 20000, 'the new-database window drawn');
const n = lines().length;
cmd({ cmd: 'open', path: file('nothing-here') });
const notice = await waitNotice('That file is gone', 15000, n);

const where = (key) => evalIn(key, `return location.href`);

step('every app page drew with its stylesheet: app.css sets --bg on :root, which no default style does');
// A page is views://<page>/index.html, where <page> is the URL's host, and `..` cannot climb above
// a host: a link to ../ui/app.css went to views://<page>/ui/app.css, a file the bundle never had,
// and the pages drew in browser defaults while every check passed (hiding by [hidden] is the
// browser's own rule, so a computed `display` says nothing about the stylesheet). --bg in effect
// is the proof the rules applied; the sheet's rules are not counted, since views://ui is another
// host than the page's and WebKitGTK keeps a sheet from another host unreadable (cssRules throws)
// while it applies it.
for (const key of ['launcher', 'newdb', notice.key]) {
  const r = await evalIn(key, `return JSON.stringify({ bg: getComputedStyle(document.documentElement).getPropertyValue('--bg').trim(), sheets: [...document.styleSheets].map((s) => s.href) })`);
  const { bg, sheets } = JSON.parse(r.value);
  assert.ok(bg !== '' && sheets.some((h) => h?.endsWith('/ui/app.css')), `${key}: its stylesheet did not load (--bg "${bg}", sheets ${JSON.stringify(sheets)})`);
}

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
// Nor opened by WebView2 in a window of its own, which it does for a request Electrobun leaves to it (src/bun/opener.ts).
if (WIN) assert.deepEqual(webviewWindows(), [], 'no window WebView2 opened by itself');

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
  // SDDL writes an account it has an alias for by the alias (the built-in Administrator, RID 500,
  // which the runners run as, is LA), and the DACL's flags as letters: P, protected, and AI, which
  // SetNamedSecurityInfo adds (read back on the runners: O:LAD:PAI(A;;FA;;;LA)).
  const who = sid.endsWith('-500') ? `(?:LA|${sid})` : sid;
  assert.match(sddl, new RegExp(`D:P(?:AI)?\\(A;;FA;;;${who}\\)$`), 'protected, one entry: every right to its user, and none to anyone else');
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

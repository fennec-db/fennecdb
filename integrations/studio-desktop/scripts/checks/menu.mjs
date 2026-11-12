// The File menu's items and keys, where there is no macOS menu bar.
//
// Windows: each window has a menu bar (Electrobun's), its accelerators
// CmdOrCtrl (src/bun/menu.ts), Ctrl there; whether a key reaches it while the
// page has the focus is the question, since the page is WebView2's, a window
// of another process. The main window takes the File menu's keys in its page
// too (src/launcher/index.ts), so either way Ctrl+N, Ctrl+O and Ctrl+Shift+O
// do there what the menu does, never twice; which one did is said. The keys
// are pressed as a person's are (testkeys.ts: each key through the system's
// input stream, a click as a mouse's), and nothing of the check's own brings a
// window forward or moves its focus. First the control, twice: text typed into
// the main window as it opened, with no click, arrives in its search box (the
// app put the keyboard focus in its page: windows.ts's `focusesPage`; on the
// arm64 runner it was on the bare window, and typed keys reached nothing), and
// a click into its page reaches the page, text typed after it too. Either
// failing fails the check, saying what the page saw and where the system's
// input went, so a chord that does nothing is the app's failing, not the keys'.
//
// Linux has no application menu (Electrobun 2.0.4: "Application menus are not
// supported on Linux"): the main window's own buttons do what macOS's File
// menu does, each pressed here as a click would: New Database…, Open…, Open
// Recent (the Databases list), Connect to a Server… (Remote connections); its
// keys, dispatched in the page (X has no input this check can press as a
// person's); and the main window itself shown again by starting the app a
// second time, the way back to it with no menu and no Dock while a file's
// window is open (with none, closing it quits the app: lastwindow.mjs).
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { CONTROL, EXE, REPORT, WIN, assertClean, cmd, data, evalIn, lines, log, quit, startApp, step, until, waitFor, waitStudio, webviewWindows } from './lib.mjs';

assertClean('before');
const FILE = data('menu', 5);

const app = startApp('menu', { FENNEC_DESKTOP_LAUNCHER: '1' });
await waitFor((l) => l.what === 'page-text' && l.key === 'launcher' && /Open file/.test(l.text), 60000, 'the main window drawn');
// Its script run, which takes the keys: the HTML's own text is there before (seen on Windows: a
// Ctrl+N pressed before the script loaded did nothing).
await until(async () => (await evalIn('launcher', `return document.getElementById('bundled').textContent.length > 0`)).value === true, 30000, "the main window's script run");

/** Whether the connections section is the one shown. */
const connectionsShown = async () => (await evalIn('launcher', `return !document.getElementById('connections').hidden`)).value === true;
/** Back to the Databases section, as its button does. */
const databasesShown = () => evalIn('launcher', `document.querySelector('.nav[data-section=databases]').click(); return true`);

/** Ctrl+N, Ctrl+O and Ctrl+Shift+O dispatched in the main window's page, each asserted by what it does. */
async function dispatchedKeys() {
  await databasesShown();
  const key = (k, shift = false) => evalIn('launcher', `document.dispatchEvent(new KeyboardEvent('keydown', {key: ${JSON.stringify(k)}, ctrlKey: true, shiftKey: ${shift}, bubbles: true, cancelable: true})); return true`);
  let n = lines().length;
  await key('n');
  await waitFor((l, i) => i >= n && l.what === 'window-created' && l.key === 'newdb', 30000, 'Ctrl+N: the new-database window');
  cmd({ cmd: 'close', key: 'newdb' });
  await waitFor((l, i) => i >= n && l.what === 'window-close' && l.key === 'newdb', 15000, 'the new-database window closed');
  n = lines().length;
  cmd({ cmd: 'answer', kind: 'file', path: FILE });
  await key('o');
  await waitFor((l, i) => i >= n && l.what === 'server-started' && l.path === FILE, 30000, 'Ctrl+O: the file opened');
  await waitStudio(`file:${FILE}`, n);
  cmd({ cmd: 'close', key: `file:${FILE}` });
  await waitFor((l, i) => i >= n && l.what === 'server-stopped', 30000, 'its window closed');
  await key('O', true);
  await until(connectionsShown, 10000, 'Ctrl+Shift+O: Remote connections shown');
}

if (WIN) {
  /** What the main window's page has seen of the system's input, and where its focus is. */
  const pageSaw = async () => (await evalIn('launcher', `return { seen: window.__seen, focused: document.hasFocus(), active: document.activeElement && document.activeElement.id, visible: document.visibilityState }`)).value;
  /** Where the system's input to the main window goes now, the windows within it too, and where the app found its focus: a failure's message. */
  async function whereInputGoes() {
    const k = lines().length;
    cmd({ cmd: 'last-input', key: 'launcher' });
    const l = await waitFor((x, i) => i >= k && x.what === 'last-input', 15000, 'where input goes');
    const looks = lines().filter((x) => x.what === 'page-focus' && x.key === 'launcher').map((x) => ({ since: x.since, focus: x.focus, moved: x.moved }));
    return `in front: ${JSON.stringify(l.front)}, this app ${l.self}; where input goes: ${JSON.stringify(l.targets)}; the windows in it: ${JSON.stringify(l.tree)}; the app's looks at its focus: ${JSON.stringify(looks)}`;
  }
  /** `fen` typed with the search box focused in the page (by the page, not the system); fails the check where it does not arrive. */
  async function typeFen(what) {
    await evalIn('launcher', `const f = document.getElementById('find'); f.value = ''; f.focus(); return true`);
    const before = await pageSaw();
    const n = lines().length;
    cmd({ cmd: 'keys', key: 'launcher', type: 'fen' });
    const r = await waitFor((l, i) => i >= n && l.what === 'keys-result', 15000, `${what}: typed`);
    const arrived = r.front && (await until(async () => (await evalIn('launcher', `return document.getElementById('find').value`)).value === 'fen', 10000, 'the search box holds what was typed').then(() => true, () => false));
    if (!arrived) assert.fail(`${what}: the page did not get the text typed (the keys: ${JSON.stringify(r)}; the page before ${JSON.stringify(before)}, after ${JSON.stringify(await pageSaw())}; ${await whereInputGoes()})`);
  }
  /** A click into the main window's page (near its top left corner), as a person's; fails the check where the page does not get it or the window is not then in front. */
  async function clickPage(what) {
    const before = (await pageSaw()).seen.mouse;
    const n = lines().length;
    cmd({ cmd: 'keys', key: 'launcher', click: true });
    const r = await waitFor((l, i) => i >= n && l.what === 'keys-result', 15000, `${what}: the click`);
    const got = r.front && (await until(async () => (await pageSaw()).seen.mouse > before, 10000, 'the page got the click').then(() => true, () => false));
    if (!got) assert.fail(`${what}: the page did not get the click, or the main window was not brought to the front by it (the click: ${JSON.stringify(r)}; the page: ${JSON.stringify(await pageSaw())}; ${await whereInputGoes()})`);
  }
  /** `chord` pressed with the main window's page clicked into; what came of it. */
  async function press(chord) {
    await databasesShown();
    await clickPage(chord);
    // What the page itself sees of the keys, recorded where the check can read it.
    await evalIn('launcher', `window.__keys = []; if (!window.__keysOn) { window.__keysOn = true; document.addEventListener('keydown', (e) => window.__keys.push((e.ctrlKey ? 'ctrl+' : '') + (e.shiftKey ? 'shift+' : '') + e.key.toLowerCase()), true); } return true`);
    const n = lines().length;
    cmd({ cmd: 'keys', key: 'launcher', chord });
    const r = await waitFor((l, i) => i >= n && l.what === 'keys-result', 15000, `${chord} pressed`);
    assert.equal(r.front, true, `the main window in front for ${chord} (in front: ${JSON.stringify(r.inFront)}, this app ${r.self})`);
    return n;
  }

  // What of the system's input reaches the page at all, for a failure's message.
  await evalIn('launcher', `window.__seen = { mouse: 0, keys: 0 }; document.addEventListener('mousedown', () => window.__seen.mouse++, true); document.addEventListener('keydown', () => window.__seen.keys++, true); return true`);
  step('the control: the main window as it opened takes text typed, with no click, into its search box');
  // The event the keys wait for: the page has the system's focus (the app moved it there once
  // the page loaded with the window in front). Nothing of the check's own moves it.
  const focused = await until(async () => (await pageSaw()).focused, 30000, "the main window's page focused").then(() => true, () => false);
  if (!focused) assert.fail(`the main window's page never had the focus, with nothing clicked (the page: ${JSON.stringify(await pageSaw())}; ${await whereInputGoes()})`);
  await typeFen('typed into the main window as it opened');
  const typed = await pageSaw();
  assert.ok(typed.seen.keys >= 3 && typed.seen.mouse === 0, `the page saw the three keys and no click (${JSON.stringify(typed)})`);
  log(`typed text reaches the page as the window opened: the page saw ${JSON.stringify(typed.seen)}`);

  step('the control: a click into the page reaches it, and text typed after it');
  await clickPage('a click into the page');
  await typeFen('typed after a click into the page');
  log(`a click and typed text reach the page: ${JSON.stringify((await pageSaw()).seen)}`);

  cmd({ cmd: 'answer', kind: 'file', path: FILE });
  const effects = {
    'ctrl+n': ['the new-database window', (i0) => lines().some((l, i) => i >= i0 && l.what === 'window-created' && l.key === 'newdb')],
    'ctrl+o': ['the file the dialog is answered with, opened', (i0) => lines().some((l, i) => i >= i0 && l.what === 'server-started' && l.path === FILE)],
    'ctrl+shift+o': ['Remote connections', async () => connectionsShown()],
  };
  for (const [chord, [what, done]] of Object.entries(effects)) {
    step(`${chord} with the page focused: ${what}`);
    const i0 = await press(chord);
    await until(async () => done(i0), 15000, `${chord}: ${what}`);
    const seen = (await evalIn('launcher', `return window.__keys || []`)).value;
    const menu = lines().some((l, i) => i >= i0 && l.what === 'menu');
    // The menu bar's accelerator takes a key before the page would see it: both is twice.
    assert.ok(!(menu && seen.includes(chord)), `${chord} taken once, by the menu bar or the page (the page saw ${JSON.stringify(seen)})`);
    log(`${chord} taken by ${menu ? 'the menu bar' : 'the page'}`);
    if (lines().some((l, i) => i >= i0 && l.what === 'window-created' && l.key === 'newdb')) {
      const k = lines().length;
      cmd({ cmd: 'close', key: 'newdb' });
      await waitFor((l, i) => i >= k && l.what === 'window-close' && l.key === 'newdb', 15000, 'the new-database window closed');
    }
    if (lines().some((l, i) => i >= i0 && l.what === 'server-started' && l.path === FILE)) {
      await waitStudio(`file:${FILE}`, i0);
      const k = lines().length;
      cmd({ cmd: 'close', key: `file:${FILE}` });
      await waitFor((l, i) => i >= k && l.what === 'server-stopped', 30000, 'its window closed');
    }
  }
  assert.deepEqual(webviewWindows(), [], 'no key opened a window of WebView2\'s own');
  await quit(app);
} else {
  step('New database: the main window\'s button, as File, New Database…');
  let n = lines().length;
  await evalIn('launcher', `document.getElementById('new').click(); return true`);
  await waitFor((l, i) => i >= n && l.what === 'window-created' && l.key === 'newdb', 30000, 'the new-database window');
  cmd({ cmd: 'close', key: 'newdb' });
  await waitFor((l, i) => i >= n && l.what === 'window-close' && l.key === 'newdb', 15000, 'the new-database window closed');

  step('Open file…: the main window\'s button, as File, Open… (the dialog answered by the check)');
  n = lines().length;
  cmd({ cmd: 'answer', kind: 'file', path: FILE });
  await evalIn('launcher', `document.getElementById('open').click(); return true`);
  const opened = await waitFor((l, i) => i >= n && l.what === 'server-started', 30000, 'the file opened');
  assert.equal(opened.path, FILE);
  await waitStudio(`file:${FILE}`, n);
  cmd({ cmd: 'close', key: `file:${FILE}` });
  await waitFor((l, i) => i >= n && l.what === 'server-stopped', 30000, 'its window closed');

  step('Open Recent: the Databases list, its row\'s Open');
  n = lines().length;
  const clicked = await evalIn('launcher', `const row = [...document.querySelectorAll('#db-rows tr')].find(r => r.title === ${JSON.stringify(FILE)}); if (!row) return 'no row'; [...row.querySelectorAll('button')].find(b => b.textContent.trim() === 'Open').click(); return 'clicked'`);
  assert.equal(clicked.value, 'clicked', 'the file is listed with an Open of its own');
  await waitFor((l, i) => i >= n && l.what === 'server-started' && l.path === FILE, 30000, 'the file opened again from the list');
  await waitStudio(`file:${FILE}`, n);
  cmd({ cmd: 'close', key: `file:${FILE}` });
  await waitFor((l, i) => i >= n && l.what === 'server-stopped', 30000, 'its window closed');

  step('Connect to a Server…: Remote connections, its form');
  const shown = await evalIn('launcher', `document.querySelector('.nav[data-section=connections]').click(); return !document.getElementById('connections').hidden && Boolean(document.getElementById('conn-add'))`);
  assert.equal(shown.value, true, 'the connections section and its Add are shown');

  step('the File menu\'s keys in the main window, dispatched in its page: Ctrl+N, Ctrl+O, Ctrl+Shift+O');
  await dispatchedKeys();

  step('the main window closed while a file\'s is open, then shown again by starting the app a second time');
  // The file's window is left, or closing the main window, the last, would quit the app (lastwindow.mjs).
  n = lines().length;
  cmd({ cmd: 'open', path: FILE });
  await waitStudio(`file:${FILE}`, n);
  n = lines().length;
  cmd({ cmd: 'close', key: 'launcher' });
  await waitFor((l, i) => i >= n && l.what === 'window-close' && l.key === 'launcher', 15000, 'the main window closed');
  const second = spawn(EXE, [], { env: { ...process.env, FENNEC_DESKTOP_REPORT: REPORT, FENNEC_DESKTOP_CONTROL: CONTROL }, stdio: 'ignore' });
  await waitFor((l, i) => i >= n && l.what === 'instance-asked' && l.ask?.show === true, 60000, 'the one there asked to show itself');
  await waitFor((l, i) => i >= n && l.what === 'window-created' && l.key === 'launcher', 30000, 'the main window shown again');
  await new Promise((r) => (second.exitCode !== null ? r() : second.on('exit', r)));
  await quit(app);
}
log('clean');

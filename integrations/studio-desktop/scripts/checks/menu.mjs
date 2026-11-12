// The File menu's items and keys, where there is no macOS menu bar.
//
// Windows: each window has a menu bar (Electrobun's), its accelerators
// CmdOrCtrl (src/bun/menu.ts), Ctrl there; whether a key reaches it while the
// page has the focus is the question, since the page is WebView2's, a window
// of another process. The main window takes the File menu's keys in its page
// too (src/launcher/index.ts), so either way Ctrl+N, Ctrl+O and Ctrl+Shift+O
// do there what the menu does, never twice; which one did is said. The keys
// are pressed as a person's are (testkeys.ts: the window brought to the front,
// a click into its page, each key through the system's input stream), and the
// control is text typed the same way arriving in the page's search box, so a
// chord that does nothing is the app's failing, not the keys'.
//
// Linux has no application menu (Electrobun 2.0.4: "Application menus are not
// supported on Linux"): the main window's own buttons do what macOS's File
// menu does, each pressed here as a click would: New Database…, Open…, Open
// Recent (the Databases list), Connect to a Server… (Remote connections); its
// keys, dispatched in the page (X has no input this check can press as a
// person's); and the main window itself shown again by starting the app a
// second time, the way back to it with no menu and no Dock.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { CONTROL, EXE, REPORT, WIN, assertClean, cmd, data, evalIn, lines, log, quit, startApp, step, until, waitFor, waitStudio, webviewWindows } from './lib.mjs';

assertClean('before');
const FILE = data('menu', 5);

const app = startApp('menu', { FENNEC_DESKTOP_LAUNCHER: '1' });
await waitFor((l) => l.what === 'page-text' && l.key === 'launcher' && /Open file/.test(l.text), 60000, 'the main window drawn');

/** Whether the connections section is the one shown. */
const connectionsShown = async () => (await evalIn('launcher', `return !document.getElementById('connections').hidden`)).value === true;
/** Back to the Databases section, as its button does. */
const databasesShown = () => evalIn('launcher', `document.querySelector('.nav[data-section=databases]').click(); return true`);

if (WIN) {
  /** A click into the main window's search box, where a person's click puts the focus in the page: its point in device pixels. */
  const findAt = async () => (await evalIn('launcher', `const f = document.getElementById('find'); f.value = ''; f.blur(); const r = f.getBoundingClientRect(); return [Math.round((r.left + r.width / 2) * devicePixelRatio), Math.round((r.top + r.height / 2) * devicePixelRatio)]`)).value;
  /** `chord` pressed with the main window's page focused, after the `after`th report; whether the menu (not the page) took it. */
  async function press(chord, menuAction) {
    const n = lines().length;
    cmd({ cmd: 'keys', key: 'launcher', click: await findAt(), chord });
    const pressed = await waitFor((l, i) => i >= n && l.what === 'keys-result', 15000, `${chord} pressed`);
    assert.equal(pressed.front, true, `the main window was brought to the front for ${chord}`);
    return { n, menu: () => lines().some((l, i) => i >= n && l.what === 'menu' && l.action === menuAction) };
  }

  step('the control: a click into the search box and text typed reach the page');
  const n = lines().length;
  cmd({ cmd: 'keys', key: 'launcher', click: await findAt(), type: 'fen' });
  const typed = await waitFor((l, i) => i >= n && l.what === 'keys-result', 15000, 'typed');
  assert.equal(typed.front, true, 'the main window was brought to the front');
  await until(async () => (await evalIn('launcher', `return document.getElementById('find').value`)).value === 'fen', 10000, 'the search box holds what was typed');
  log('typed text reaches the page: WebView2 has the focus');

  step('Ctrl+N with the page focused: the new-database window');
  const newdb = await press('ctrl+n', 'new');
  await waitFor((l, i) => i >= newdb.n && l.what === 'window-created' && l.key === 'newdb', 20000, 'the new-database window');
  log(`Ctrl+N taken by ${newdb.menu() ? 'the menu bar' : 'the page'}`);
  cmd({ cmd: 'close', key: 'newdb' });
  await waitFor((l, i) => i >= newdb.n && l.what === 'window-close' && l.key === 'newdb', 15000, 'the new-database window closed');

  step('Ctrl+O with the page focused: Open… (the dialog answered by the check)');
  cmd({ cmd: 'answer', kind: 'file', path: FILE });
  const open = await press('ctrl+o', 'open');
  const opened = await waitFor((l, i) => i >= open.n && l.what === 'server-started', 30000, 'the file opened');
  assert.equal(opened.path, FILE);
  log(`Ctrl+O taken by ${open.menu() ? 'the menu bar' : 'the page'}`);
  await waitStudio(`file:${FILE}`, open.n);
  cmd({ cmd: 'close', key: `file:${FILE}` });
  await waitFor((l, i) => i >= open.n && l.what === 'server-stopped', 30000, 'its window closed');

  step('Ctrl+Shift+O with the page focused: Connect to a Server…');
  const connect = await press('ctrl+shift+o', 'launcher');
  await until(async () => connect.menu() || (await connectionsShown()), 10000, 'Remote connections shown, or the menu asked');
  log(`Ctrl+Shift+O taken by ${connect.menu() ? 'the menu bar' : 'the page'}`);
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

  step('Connect to a Server…: Remote connections, its form');
  const shown = await evalIn('launcher', `document.querySelector('.nav[data-section=connections]').click(); return !document.getElementById('connections').hidden && Boolean(document.getElementById('conn-add'))`);
  assert.equal(shown.value, true, 'the connections section and its Add are shown');

  step('the File menu\'s keys in the main window: Ctrl+N, Ctrl+O, Ctrl+Shift+O');
  await databasesShown();
  const key = (k, shift = false) => evalIn('launcher', `document.dispatchEvent(new KeyboardEvent('keydown', {key: ${JSON.stringify(k)}, ctrlKey: true, shiftKey: ${shift}, bubbles: true, cancelable: true})); return true`);
  n = lines().length;
  await key('n');
  await waitFor((l, i) => i >= n && l.what === 'window-created' && l.key === 'newdb', 30000, 'Ctrl+N: the new-database window');
  cmd({ cmd: 'close', key: 'newdb' });
  await waitFor((l, i) => i >= n && l.what === 'window-close' && l.key === 'newdb', 15000, 'the new-database window closed');
  n = lines().length;
  cmd({ cmd: 'answer', kind: 'file', path: FILE });
  await key('o');
  await waitFor((l, i) => i >= n && l.what === 'server-started' && l.path === FILE, 30000, 'Ctrl+O: the file opened');
  await key('O', true);
  await until(connectionsShown, 10000, 'Ctrl+Shift+O: Remote connections shown');

  step('the main window closed, then shown again by starting the app a second time');
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

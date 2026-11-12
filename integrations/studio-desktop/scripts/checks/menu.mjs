// The File menu's items, where there is no macOS menu bar.
//
// Windows: each window has a menu bar (Electrobun's), its accelerators
// CmdOrCtrl (src/bun/menu.ts): Ctrl there. Whether a key reaches it is the
// question while a page has the focus, which WebView2, a window of another
// process inside the app's, takes from the app's own window. The keys are
// pressed as a person's are (testkeys.ts: the window brought to the front,
// each key through the system's input stream); the control is text typed the
// same way that does reach the page's search box, so a chord that reaches no
// menu is the menu's failing, not the keys'.
//
// Linux has no application menu (Electrobun 2.0.4: "Application menus are not
// supported on Linux"): the main window's own buttons do what macOS's File
// menu does, each pressed here as a click would: New Database…, Open…, Open
// Recent (the Databases list), Connect to a Server… (Remote connections), and
// the main window itself shown again by starting the app a second time, the
// way back to it with no menu and no Dock.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { CONTROL, EXE, REPORT, WIN, assertClean, cmd, data, evalIn, lines, log, quit, startApp, step, until, waitFor, waitStudio, webviewWindows } from './lib.mjs';

assertClean('before');
const FILE = data('menu', 5);

const app = startApp('menu', { FENNEC_DESKTOP_LAUNCHER: '1' });
await waitFor((l) => l.what === 'page-text' && l.key === 'launcher' && /Open file/.test(l.text), 60000, 'the main window drawn');

/** Presses `chord` into window `key` and says whether the app's menu then did `action`, within `ms`. */
async function chordReaches(key, chord, action, ms = 8000) {
  const n = lines().length;
  cmd({ cmd: 'keys', key, chord });
  const pressed = await waitFor((l, i) => i >= n && l.what === 'keys-result', 15000, `${chord} pressed`);
  assert.equal(pressed.front, true, `${key} was brought to the front for ${chord}`);
  try {
    await waitFor((l, i) => i >= n && l.what === 'menu' && l.action === action, ms, `the menu's ${action}`);
    return true;
  } catch {
    return false;
  }
}

if (WIN) {
  /** A click into the main window's search box, where a person's click puts the focus in the page: its point in device pixels. */
  const findAt = async () => (await evalIn('launcher', `const f = document.getElementById('find'); f.value = ''; f.blur(); const r = f.getBoundingClientRect(); return [Math.round((r.left + r.width / 2) * devicePixelRatio), Math.round((r.top + r.height / 2) * devicePixelRatio)]`)).value;

  step('the window in front, nothing in its page clicked: Ctrl+N');
  const windowOnly = await chordReaches('launcher', 'ctrl+n', 'new');
  log('Ctrl+N reached the menu with the window in front:', windowOnly);
  if (lines().some((l) => l.what === 'window-created' && l.key === 'newdb')) {
    const n = lines().length;
    cmd({ cmd: 'close', key: 'newdb' });
    await waitFor((l, i) => i >= n && l.what === 'window-close' && l.key === 'newdb', 15000, 'the new-database window closed');
  }

  step('the control: a click into the search box and text typed reach the page');
  let n = lines().length;
  cmd({ cmd: 'keys', key: 'launcher', click: await findAt(), type: 'fen' });
  const typed = await waitFor((l, i) => i >= n && l.what === 'keys-result', 15000, 'typed');
  log('clicked at', JSON.stringify(typed.clicked), 'in front:', typed.front);
  assert.equal(typed.front, true, 'the main window was brought to the front');
  await until(async () => (await evalIn('launcher', `return document.getElementById('find').value`)).value === 'fen', 10000, 'the search box holds what was typed');
  log('typed text reaches the page: WebView2 has the focus');

  step('Ctrl+N, Ctrl+O and Ctrl+Shift+O with the page focused');
  cmd({ cmd: 'answer', kind: 'file', path: null });
  const found = {};
  for (const [chord, action] of [['ctrl+n', 'new'], ['ctrl+o', 'open'], ['ctrl+shift+o', 'launcher']]) {
    n = lines().length;
    cmd({ cmd: 'keys', key: 'launcher', click: await findAt(), chord });
    const pressed = await waitFor((l, i) => i >= n && l.what === 'keys-result', 15000, `${chord} pressed`);
    assert.equal(pressed.front, true);
    try {
      await waitFor((l, i) => i >= n && l.what === 'menu' && l.action === action, 8000, `the menu's ${action}`);
      found[chord] = true;
    } catch {
      found[chord] = false;
    }
    log(chord, 'reached the menu with the page focused:', found[chord], 'the search box holds', JSON.stringify((await evalIn('launcher', `return document.getElementById('find').value`)).value));
  }
  log('WebView2 windows of its own:', JSON.stringify(webviewWindows()));
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

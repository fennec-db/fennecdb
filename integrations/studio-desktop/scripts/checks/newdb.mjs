// New database: a folder from the folder dialog (answered by this check, since
// a native dialog cannot be clicked from outside the process), a name, and
// the app makes the file by starting a server on it. An existing name is
// refused, whatever it holds is not touched.
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { assertClean, CASE_BLIND, cmd, D, evalIn, lines, procs, quit, R, startApp, step, textOf, until, waitFor, waitStudio, log } from './lib.mjs';

assertClean('before');
const FOLDER = join(R, 'new-here');
const FRESH = join(FOLDER, 'fresh.fennec');
rmSync(FOLDER, { recursive: true, force: true });
mkdirSync(FOLDER);

const app = startApp('newdb', { FENNEC_DESKTOP_LAUNCHER: '1' });
await waitFor((l) => l.what === 'ready', 60000, 'ready');
// Drawn with what the main process answered (the bundled server's version, which only its answer
// fills in): its page asks it through the RPC bridge, and on Windows a click before that bridge
// is up went nowhere, the new-database window never asked for.
await waitFor((l) => l.what === 'page-text' && l.key === 'launcher' && /New database/.test(l.text) && /Bundled server fennec-server/.test(l.text), 20000, 'the launcher drawn with the main process\'s answer');

step('the launcher\'s button opens the window');
let n = lines().length;
await evalIn('launcher', `document.getElementById('new').click()`);
await waitFor((l, i) => i >= n && l.what === 'page-text' && l.key === 'newdb' && /New database/.test(l.text), 15000, 'the new-database window drawn');
// Its folder is the main process's answer too (`defaults`), there once the window's bridge is.
const shown = await until(
  async () => {
    const v = (await evalIn('newdb', `return [document.getElementById('folder').value, document.getElementById('file').textContent].join('|')`)).value;
    return /^[^|]+\|/.test(v) && v;
  },
  15000,
  'the window showing the folder it starts in',
);
assert.match(shown, /Documents\|orders\.fennec$/, `starts in Documents, showing what the file will be called: ${shown}`);

step('Choose… the folder, name it: the file is made and opened');
cmd({ cmd: 'answer', kind: 'folder', path: FOLDER });
await evalIn('newdb', `document.getElementById('choose').click()`);
await until(async () => (await evalIn('newdb', `return document.getElementById('folder').value`)).value === FOLDER, 10000, 'the chosen folder shown');
await evalIn('newdb', `const i = document.getElementById('name'); i.value = 'fresh'; i.dispatchEvent(new Event('input', {bubbles: true}))`);
assert.equal((await evalIn('newdb', `return document.getElementById('file').textContent`)).value, 'fresh.fennec');
n = lines().length;
await evalIn('newdb', `document.getElementById('create').click()`);
const started = await waitFor((l, i) => i >= n && l.what === 'server-started', 20000, 'the new database opening');
assert.equal(started.path, FRESH);
assert.ok(existsSync(FRESH));
const KEY = `file:${FRESH}`;
await waitFor((l, i) => i >= n && l.what === 'page' && l.key === KEY && l.detail?.type === 'state', 30000, 'its studio');
await waitFor((l, i) => i >= n && l.what === 'window-close' && l.key === 'newdb', 10000, 'the new-database window closing');

step('close it: its server stops, and what is on disk is a database');
cmd({ cmd: 'close', key: KEY });
await waitFor((l) => l.what === 'server-stopped' && l.key === KEY, 15000, 'its server stopped');
assert.ok(statSync(FRESH).size >= 8, 'a database file, not empty');

step('the same name again: refused, and the file is as it was');
const before = readFileSync(FRESH);
n = lines().length;
cmd({ cmd: 'create', folder: FOLDER, name: 'fresh' });
const dup = await waitFor((l, i) => i >= n && l.what === 'create-result', 10000, 'create-result');
assert.equal(dup.ok, false);
assert.match(dup.message, /fresh\.fennec exists in that folder already/);
assert.deepEqual(readFileSync(FRESH), before);
// Spelled with the extension, or in another case where the file system ignores case (APFS and
// NTFS do, ext4 does not: FRESH there is another file, and another name to make), it is the same file.
for (const name of CASE_BLIND ? ['fresh.fennec', 'FRESH'] : ['fresh.fennec']) {
  n = lines().length;
  cmd({ cmd: 'create', folder: FOLDER, name });
  assert.equal((await waitFor((l, i) => i >= n && l.what === 'create-result', 10000, 'create-result')).ok, false, name);
}
// And a file that is not a database is as safe from it.
writeFileSync(join(FOLDER, 'notes.fennec'), 'mine');
n = lines().length;
cmd({ cmd: 'create', folder: FOLDER, name: 'notes' });
assert.equal((await waitFor((l, i) => i >= n && l.what === 'create-result', 10000, 'create-result')).ok, false);
assert.equal(readFileSync(join(FOLDER, 'notes.fennec'), 'utf8'), 'mine');

step('the window says so too, where the name is typed');
await evalIn('launcher', `document.getElementById('new').click()`);
await waitFor((l, i) => i >= n && l.what === 'page-text' && l.key === 'newdb', 15000, 'the window again');
cmd({ cmd: 'answer', kind: 'folder', path: FOLDER });
await evalIn('newdb', `document.getElementById('choose').click()`);
await until(async () => (await evalIn('newdb', `return document.getElementById('folder').value`)).value === FOLDER, 10000, 'the chosen folder shown');
await evalIn('newdb', `const i = document.getElementById('name'); i.value = 'fresh'; i.dispatchEvent(new Event('input', {bubbles: true})); document.getElementById('create').click()`);
const err = await until(async () => (await evalIn('newdb', `return document.getElementById('error').textContent`)).value, 10000, 'the error shown in the window');
assert.match(err, /fresh\.fennec exists in that folder already/);
assert.ok(!(await textOf('newdb')).includes('undefined'));

step('names that cannot be files, and a folder that is gone');
for (const bad of ['', 'a/b', '.hidden', 'x'.repeat(300)]) {
  n = lines().length;
  cmd({ cmd: 'create', folder: FOLDER, name: bad });
  assert.equal((await waitFor((l, i) => i >= n && l.what === 'create-result', 10000, 'create-result')).ok, false, JSON.stringify(bad.slice(0, 8)));
}
n = lines().length;
cmd({ cmd: 'create', folder: join(R, 'no-such-folder'), name: 'x' });
assert.match((await waitFor((l, i) => i >= n && l.what === 'create-result', 10000, 'create-result')).message, /does not exist or cannot be written/);
assert.ok(!existsSync(join(R, 'no-such-folder')));

step('a first collection, and when writes are safe: made with the database, its server started that way');
n = lines().length;
cmd({ cmd: 'create', folder: FOLDER, name: 'with-notes', sync: 'always', collection: { name: 'notes', fields: 'title text, done bool' } });
const withNotes = await waitFor((l, i) => i >= n && l.what === 'create-result', 20000, 'create-result');
assert.equal(withNotes.ok, true, withNotes.message);
const NOTES = join(FOLDER, 'with-notes.fennec');
const notesShown = await waitStudio(`file:${NOTES}`, n);
assert.match(notesShown.detail.side, /notes 0/, 'its first collection, empty');
assert.ok(lines().slice(n).some((l) => l.what === 'task-server-started' && l.path === NOTES), 'made by a server, not by the app');
assert.ok(procs().some((p) => p.command.includes(`--file ${NOTES} --http 127.0.0.1:0 --sync always`)), 'its window\'s server syncs every write');
cmd({ cmd: 'close', key: `file:${NOTES}` });
await waitFor((l, i) => i >= n && l.what === 'server-stopped' && l.key === `file:${NOTES}`, 15000, 'its server stopped');

step('a field list that could close itself and say something else is refused, and no file is made');
n = lines().length;
cmd({ cmd: 'create', folder: FOLDER, name: 'bad-notes', collection: { name: 'notes', fields: 'title text); drop collection orders; (' } });
const badNotes = await waitFor((l, i) => i >= n && l.what === 'create-result', 10000, 'create-result');
assert.equal(badNotes.ok, false);
assert.ok(!existsSync(join(FOLDER, 'bad-notes.fennec')));

step('Serve it once it is made: no studio window, the serve sheet in the main window instead');
n = lines().length;
cmd({ cmd: 'create', folder: FOLDER, name: 'to-serve', serve: true });
const toServe = await waitFor((l, i) => i >= n && l.what === 'create-result', 20000, 'create-result');
assert.equal(toServe.ok, true, toServe.message);
assert.equal(toServe.serve, true);
const sheetTitle = await until(async () => (await evalIn('launcher', `const d = document.getElementById('sheet'); return d.open && document.getElementById('sheet-title').textContent`)).value, 15000, 'the serve sheet on it');
assert.equal(sheetTitle, 'Serve to-serve.fennec');
assert.ok(!lines().slice(n).some((l) => l.what === 'window-opened' && l.key === `file:${join(FOLDER, 'to-serve.fennec')}`), 'no studio window');
assert.ok(statSync(join(FOLDER, 'to-serve.fennec')).size >= 8, 'a database already: a cancelled sheet leaves no empty file');
await evalIn('launcher', `document.getElementById('sheet-cancel').click()`);

step('Cancel closes the window');
n = lines().length;
await evalIn('newdb', `document.getElementById('cancel').click()`);
await waitFor((l, i) => i >= n && l.what === 'window-close' && l.key === 'newdb', 10000, 'the window closing');

step('quit');
await quit(app);
log('clean', D);

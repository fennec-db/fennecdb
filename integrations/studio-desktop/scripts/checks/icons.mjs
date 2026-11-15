// The icon of the Windows app's executables, held as it ships.
//
//   node scripts/checks/icons.mjs [the app's folder]
//     dist-archive.mjs passes the folder it unpacked from the zip (Windows only).
//
// bin/bun.exe owns every window of the app, so the taskbar button, Alt-Tab and
// the title bar take its icon; bin/launcher.exe is what the Start-menu
// shortcut and a pinned taskbar button start. Each is read as Windows reads it
// (PE resources, RT_GROUP_ICON naming RT_ICON images) and must hold:
//   - an image of 16, 32, 48 and 256 pixels (the sizes the shell asks for:
//     the title bar and a small taskbar, a 100-150% taskbar and Start, Alt-Tab
//     and Explorer's medium icons, the large ones; anything else is a smaller
//     or larger picture scaled);
//   - in every group, an entry whose image is the size and the byte count it
//     says (a group naming a 16-pixel picture as 256 draws it blurred);
//   - the app's own images (icon/icon.ico's), not the toolchain's: Bun's
//     dumpling, in a named group that Windows prefers to any numbered one, has
//     the sizes it has and none of these properties, so a build that leaves it
//     there fails on its group alone.
// The check carries its own control: the toolchain's Bun is what the build
// folder holds before scripts/set-exe-icon.mjs, and is held to the same
// words, which it must fail.
import assert from 'node:assert/strict';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { HERE, log, step } from './lib.mjs';
import { icoImages } from '../set-exe-icon.mjs';
import { iconGroups, iconProblems } from '../pe-icons.mjs';

const SIZES = [16, 32, 48, 256];
const EXES = ['bun.exe', 'launcher.exe'];

/** What each exe of `app` says wrong about its icon, by exe name. */
export function held(app, want) {
  const out = {};
  for (const name of EXES) {
    const file = join(app, 'bin', name);
    assert.ok(existsSync(file), `${file}: the app has no ${name}`);
    out[name] = iconProblems(iconGroups(readFileSync(file)), SIZES, want);
  }
  return out;
}

export const appIcon = () => new Map(icoImages(readFileSync(join(HERE, 'icon', 'icon.ico'))).map((i) => [i.width, i.data]));

const target = process.argv[2];
if (target) {
  step('the exes carry the app icon at every size');
  const app = resolve(target);
  const want = appIcon();
  for (const s of SIZES) assert.ok(want.has(s), `icon/icon.ico holds a ${s}x${s} image`);
  const problems = held(app, want);
  for (const [name, list] of Object.entries(problems)) {
    const groups = iconGroups(readFileSync(join(app, 'bin', name)));
    log(`${name}: ${groups.map((g) => `group ${g.name} [${g.entries.map((e) => `${e.width}x${e.height}`).join(' ')}]`).join('; ') || 'no icon group'}`);
    assert.deepEqual(list, [], `${name}'s icon:\n  ${list.join('\n  ')}`);
  }

  // The control: the build folder's own bun.exe, which nothing has written
  // the icon into, must fail the same words.
  const build = join(HERE, 'build');
  const stock = existsSync(build)
    ? readdirSync(build)
        .filter((d) => d.startsWith('stable-win-'))
        .flatMap((d) => readdirSync(join(build, d)).map((n) => join(build, d, n, 'bin', 'bun.exe')))
        .find((f) => existsSync(f))
    : null;
  if (stock) {
    const was = iconProblems(iconGroups(readFileSync(stock)), SIZES, want);
    assert.ok(was.length > 0, `${stock}, as Electrobun built it, is held to the same words and passes: the check cannot tell the app's icon from Bun's`);
    log(`control: the build folder's bun.exe fails them (${was[0]})`);
  } else {
    log('control: no build folder here to hold the stock bun.exe to the same words');
  }
}

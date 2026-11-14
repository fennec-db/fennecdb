// Writes an .ico into a Windows executable as its icon: every image of it as
// an RT_ICON and one RT_GROUP_ICON (id 1) naming them, replacing any icon the
// file has, the rest of its resources kept. scripts/dist.sh runs it on the
// app's bin/bun.exe and bin/launcher.exe before the zip is made.
//
//   node scripts/set-exe-icon.mjs <icon.ico> <exe> [...]
//
// Why here: the Windows app shipped with Bun's own dumpling in the taskbar.
// bun.exe, the process that owns every window, carried Bun's one 256-pixel
// icon (the named group IDI_MYICON, which Windows prefers to any numbered
// one) and launcher.exe, which the shortcuts start, no resources at all:
// Electrobun's `build.win.icon` reached neither file (checks/icons.mjs holds
// both to the app's icon; the design file has the measurements). Written in
// plain Node because a resource editor (rcedit, a Windows program) or a
// package for one would be a dependency and a Windows-only step for a table
// of a few hundred bytes; the file is rewritten in place where its resource
// section has room (bun.exe's: 272 KB for 270 KB of Bun's icon) and given a
// section where it has none (launcher.exe).
//
// Writing invalidates an Authenticode signature, so the certificate table is
// cut off and its directory zeroed rather than left naming bytes that are not
// a signature (Windows reads that as a broken file, not as an unsigned one).
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { RT_GROUP_ICON, RT_ICON, parsePe, readResources } from './pe-icons.mjs';

const align = (n, to) => Math.ceil(n / to) * to;

/** The images of an .ico: { width, height, planes, bits, data } each (a side of 0 is 256). */
export function icoImages(ico) {
  if (ico.readUInt16LE(0) !== 0 || ico.readUInt16LE(2) !== 1) throw new Error('not an .ico');
  const out = [];
  for (let i = 0; i < ico.readUInt16LE(4); i++) {
    const e = 6 + 16 * i;
    const size = ico.readUInt32LE(e + 8);
    const at = ico.readUInt32LE(e + 12);
    if (at + size > ico.length) throw new Error(`the .ico's entry ${i} runs past its end`);
    out.push({
      width: ico.readUInt8(e) || 256,
      height: ico.readUInt8(e + 1) || 256,
      colors: ico.readUInt8(e + 2),
      planes: ico.readUInt16LE(e + 4),
      bits: ico.readUInt16LE(e + 6),
      data: ico.subarray(at, at + size),
    });
  }
  if (out.length === 0) throw new Error('the .ico holds no image');
  return out;
}

/** A resource section's bytes, as it would sit at `va`: the tree of types, names and languages, then the data. Every type and name is a number: a named resource is not rewritten. */
function resourceSection(leaves, va) {
  const tree = new Map();
  for (const l of leaves) {
    if (typeof l.type !== 'number' || typeof l.name !== 'number') throw new Error(`a named resource (${l.type}/${l.name}) cannot be rewritten`);
    if (!tree.has(l.type)) tree.set(l.type, new Map());
    const names = tree.get(l.type);
    if (!names.has(l.name)) names.set(l.name, []);
    names.get(l.name).push(l);
  }
  const types = [...tree.keys()].sort((a, b) => a - b);
  let size = 16 + 8 * types.length;
  for (const t of types) {
    size += 16 + 8 * tree.get(t).size;
    for (const ls of tree.get(t).values()) size += 16 + 8 * ls.length + 16 * ls.length;
  }
  const dataAt = align(size, 8);
  const blobs = [];
  let end = dataAt;
  const out = Buffer.alloc(dataAt);
  const head = (at, n) => {
    out.writeUInt16LE(n, at + 14);
  };
  let next = 16 + 8 * types.length;
  head(0, types.length);
  let leafAt = 0;
  // Data entries sit after every directory.
  let entries = next;
  for (const t of types) {
    entries += 16 + 8 * tree.get(t).size;
    for (const ls of tree.get(t).values()) entries += 16 + 8 * ls.length;
  }
  types.forEach((t, i) => {
    out.writeUInt32LE(t, 16 + 8 * i);
    out.writeUInt32LE((0x80000000 | next) >>> 0, 16 + 8 * i + 4);
    const names = [...tree.get(t).keys()].sort((a, b) => a - b);
    const typeDir = next;
    head(typeDir, names.length);
    next += 16 + 8 * names.length;
    names.forEach((n, j) => {
      out.writeUInt32LE(n, typeDir + 16 + 8 * j);
      out.writeUInt32LE((0x80000000 | next) >>> 0, typeDir + 16 + 8 * j + 4);
      const ls = tree.get(t).get(n).sort((a, b) => a.lang - b.lang);
      const nameDir = next;
      head(nameDir, ls.length);
      next += 16 + 8 * ls.length;
      ls.forEach((l, k) => {
        out.writeUInt32LE(l.lang, nameDir + 16 + 8 * k);
        const entry = entries + 16 * leafAt++;
        out.writeUInt32LE(entry, nameDir + 16 + 8 * k + 4);
        out.writeUInt32LE(va + end, entry);
        out.writeUInt32LE(l.data.length, entry + 4);
        out.writeUInt32LE(l.codepage, entry + 8);
        blobs.push(l.data, Buffer.alloc(align(l.data.length, 8) - l.data.length));
        end += align(l.data.length, 8);
      });
    });
  });
  return Buffer.concat([out, ...blobs]);
}

/** `exe` (a Buffer) with `ico`'s images as its icon: a new Buffer. */
export function withIcon(exe, ico) {
  const p = parsePe(exe);
  const images = icoImages(ico);
  const old = readResources(exe, p);
  const lang = old.find((r) => r.type === RT_GROUP_ICON)?.lang ?? 1033;
  const kept = old.filter((r) => r.type !== RT_ICON && r.type !== RT_GROUP_ICON);
  const group = Buffer.alloc(6 + 14 * images.length);
  group.writeUInt16LE(1, 2);
  group.writeUInt16LE(images.length, 4);
  images.forEach((im, i) => {
    const e = 6 + 14 * i;
    group.writeUInt8(im.width === 256 ? 0 : im.width, e);
    group.writeUInt8(im.height === 256 ? 0 : im.height, e + 1);
    group.writeUInt8(im.colors, e + 2);
    group.writeUInt16LE(im.planes, e + 4);
    group.writeUInt16LE(im.bits, e + 6);
    group.writeUInt32LE(im.data.length, e + 8);
    group.writeUInt16LE(i + 1, e + 12);
  });
  const leaves = [
    ...kept,
    ...images.map((im, i) => ({ type: RT_ICON, name: i + 1, lang, codepage: 0, data: im.data })),
    { type: RT_GROUP_ICON, name: 1, lang, codepage: 0, data: group },
  ];

  let file = Buffer.from(exe);
  // The signature no longer covers the file.
  const security = p.dirs + 8 * 4;
  if (p.dirCount > 4 && file.readUInt32LE(security) !== 0) {
    const at = file.readUInt32LE(security);
    if (at + file.readUInt32LE(security + 4) === file.length) file = file.subarray(0, at);
    file.writeUInt32LE(0, security);
    file.writeUInt32LE(0, security + 4);
  }
  file.writeUInt32LE(0, p.optional + 64); // the checksum: unset, which only drivers must not have

  const rsrcDir = p.dirs + 8 * 2;
  const rva = file.readUInt32LE(rsrcDir);
  if (rva !== 0) {
    const s = p.sections.find((x) => x.va === rva);
    if (!s) throw new Error('the resource directory does not start a section: not rewritten');
    const blob = resourceSection(leaves, s.va);
    if (blob.length <= Math.min(s.rawsize, s.vsize)) {
      file.fill(0, s.raw, s.raw + s.rawsize);
      blob.copy(file, s.raw);
      file.writeUInt32LE(blob.length, rsrcDir + 4);
      return file;
    }
    // It does not fit (a build that wrote its own icon first leaves a section
    // sized to it, 8 bytes short here): the old one is emptied and renamed, the
    // resources go in a new one.
    file.fill(0, s.raw, s.raw + s.rawsize);
    file.fill(0, s.header, s.header + 8);
    file.write('.rsrc.o', s.header, 'latin1');
  }
  // No resources, or none that fit: a section of its own, after the last.
  const last = p.sections.reduce((a, b) => (b.va > a.va ? b : a));
  const va = align(last.va + Math.max(last.vsize, last.rawsize), p.sectionAlign);
  const blob = resourceSection(leaves, va);
  const headerAt = p.table + 40 * p.sections.length;
  const firstRaw = Math.min(...p.sections.map((x) => x.raw).filter(Boolean));
  if (headerAt + 40 > firstRaw) throw new Error('no room in the headers for another section');
  const raw = align(file.length, p.fileAlign);
  const padded = Buffer.alloc(align(blob.length, p.fileAlign));
  blob.copy(padded);
  file = Buffer.concat([file, Buffer.alloc(raw - file.length), padded]);
  file.fill(0, headerAt, headerAt + 40);
  file.write('.rsrc', headerAt, 'latin1');
  file.writeUInt32LE(blob.length, headerAt + 8);
  file.writeUInt32LE(va, headerAt + 12);
  file.writeUInt32LE(padded.length, headerAt + 16);
  file.writeUInt32LE(raw, headerAt + 20);
  file.writeUInt32LE(0x40000040, headerAt + 36); // initialized data, readable
  file.writeUInt16LE(p.sections.length + 1, p.pe + 6);
  file.writeUInt32LE(align(va + blob.length, p.sectionAlign), p.optional + 56);
  file.writeUInt32LE(va, rsrcDir);
  file.writeUInt32LE(blob.length, rsrcDir + 4);
  return file;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [ico, ...exes] = process.argv.slice(2);
  if (!ico || exes.length === 0) throw new Error('usage: set-exe-icon.mjs <icon.ico> <exe> [...]');
  const icon = readFileSync(ico);
  for (const exe of exes) {
    writeFileSync(exe, withIcon(readFileSync(exe), icon));
    console.log(`${exe}: ${icoImages(icon).length} icon images written`);
  }
}

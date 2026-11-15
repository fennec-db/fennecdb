// The resources and icon groups of a Windows executable, read from its PE
// file: RT_GROUP_ICON groups, each entry naming an RT_ICON image. Used by
// scripts/set-exe-icon.mjs (which writes the app's icon into the exes) and
// scripts/checks/icons.mjs (which holds the shipped exes to it). Plain Node on
// any system, no dependency: a resource tree is a few structs deep, and what
// the shell reads for a taskbar button is exactly these bytes.
//
//   node scripts/pe-icons.mjs <exe> [...]   prints each group's entries
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

export const RT_ICON = 3;
export const RT_GROUP_ICON = 14;

/** The headers of a PE file: where its optional header, data directories and section table are, and its sections. */
export function parsePe(exe) {
  if (exe.length < 0x40 || exe.readUInt16LE(0) !== 0x5a4d) throw new Error('not an executable (no MZ)');
  const pe = exe.readUInt32LE(0x3c);
  if (pe + 24 > exe.length || exe.readUInt32LE(pe) !== 0x4550) throw new Error('not a PE file');
  const optional = pe + 24;
  const magic = exe.readUInt16LE(optional);
  if (magic !== 0x10b && magic !== 0x20b) throw new Error(`unknown optional header ${magic.toString(16)}`);
  const dirs = optional + (magic === 0x20b ? 112 : 96);
  const count = exe.readUInt32LE(dirs - 4);
  const table = optional + exe.readUInt16LE(pe + 20);
  const sections = [];
  for (let i = 0; i < exe.readUInt16LE(pe + 6); i++) {
    const h = table + 40 * i;
    sections.push({
      header: h,
      va: exe.readUInt32LE(h + 12),
      vsize: exe.readUInt32LE(h + 8),
      raw: exe.readUInt32LE(h + 20),
      rawsize: exe.readUInt32LE(h + 16),
    });
  }
  return { pe, optional, magic, dirs, dirCount: count, table, sections, sectionAlign: exe.readUInt32LE(optional + 32), fileAlign: exe.readUInt32LE(optional + 36) };
}

/** The file offset of an RVA. */
function fileOffset(p, rva) {
  const s = p.sections.find((x) => rva >= x.va && rva < x.va + Math.max(x.vsize, x.rawsize));
  if (!s) throw new Error(`RVA ${rva.toString(16)} in no section`);
  return s.raw + (rva - s.va);
}

/** Every resource of the file: { type, name, lang, codepage, data }, a type or name a number, or a string where the file names it so. */
export function readResources(exe, p = parsePe(exe)) {
  const rsrcRva = p.dirCount > 2 ? exe.readUInt32LE(p.dirs + 16) : 0;
  if (rsrcRva === 0) return [];
  const base = fileOffset(p, rsrcRva);
  const key = (raw) => {
    if (!(raw & 0x80000000)) return raw;
    const at = base + (raw & 0x7fffffff);
    return exe.toString('utf16le', at + 2, at + 2 + 2 * exe.readUInt16LE(at));
  };
  const dir = (offset) => {
    const at = base + offset;
    const n = exe.readUInt16LE(at + 12) + exe.readUInt16LE(at + 14);
    const out = [];
    for (let i = 0; i < n; i++) {
      const e = at + 16 + 8 * i;
      const to = exe.readUInt32LE(e + 4);
      out.push({ key: key(exe.readUInt32LE(e)), sub: to & 0x80000000 ? to & 0x7fffffff : null, leaf: to & 0x80000000 ? null : to });
    }
    return out;
  };
  const found = [];
  for (const t of dir(0)) {
    if (t.sub === null) continue;
    for (const n of dir(t.sub)) {
      if (n.sub === null) continue;
      for (const l of dir(n.sub)) {
        if (l.leaf === null) continue;
        const d = base + l.leaf;
        const start = fileOffset(p, exe.readUInt32LE(d));
        found.push({ type: t.key, name: n.key, lang: l.key, codepage: exe.readUInt32LE(d + 8), data: exe.subarray(start, start + exe.readUInt32LE(d + 4)) });
      }
    }
  }
  return found;
}

/** The RT_GROUP_ICON groups of `exe` (a Buffer): [{ name, lang, entries: [{ width, height, bits, bytes, id, image }] }], `image` what the entry really points at ({ format, width, height, bits, size, data }) or null where no RT_ICON has that id. */
export function iconGroups(exe) {
  const all = readResources(exe);
  const images = new Map(all.filter((r) => r.type === RT_ICON).map((r) => [r.name, r.data]));
  return all
    .filter((r) => r.type === RT_GROUP_ICON)
    .map((g) => {
      const entries = [];
      for (let i = 0; i < g.data.readUInt16LE(4); i++) {
        const e = 6 + 14 * i;
        const side = (b) => (b === 0 ? 256 : b);
        const id = g.data.readUInt16LE(e + 12);
        entries.push({
          width: side(g.data.readUInt8(e)),
          height: side(g.data.readUInt8(e + 1)),
          bits: g.data.readUInt16LE(e + 6),
          bytes: g.data.readUInt32LE(e + 8),
          id,
          image: imageOf(images.get(id)),
        });
      }
      return { name: g.name, lang: g.lang, entries };
    });
}

/** What an RT_ICON image is: a PNG (width and height from its IHDR) or a DIB (a BITMAPINFOHEADER, its height counting the AND mask too, so twice the picture's). */
function imageOf(data) {
  if (!data) return null;
  if (data.length >= 26 && data.readUInt32BE(0) === 0x89504e47 && data.toString('latin1', 12, 16) === 'IHDR') {
    const channels = { 0: 1, 2: 3, 3: 1, 4: 2, 6: 4 }[data.readUInt8(25)] ?? 0;
    return { format: 'png', width: data.readUInt32BE(16), height: data.readUInt32BE(20), bits: data.readUInt8(24) * channels, size: data.length, data };
  }
  if (data.length >= 16 && data.readUInt32LE(0) >= 40) {
    return { format: 'dib', width: data.readInt32LE(4), height: Math.abs(data.readInt32LE(8)) / 2, bits: data.readUInt16LE(14), size: data.length, data };
  }
  return { format: 'unknown', width: 0, height: 0, bits: 0, size: data.length, data };
}

/**
 * The ways `groups` fail the icon: a group whose entry names no image, an
 * image that is not the size its entry says (Windows scales or refuses it), a
 * group lacking a size of `sizes`, or an image that is not the app's (`want`,
 * the .ico's images by size: the toolchain's own Bun icon has every property
 * but this one).
 */
export function iconProblems(groups, sizes, want = new Map()) {
  const problems = [];
  if (groups.length === 0) problems.push('no icon group at all');
  groups.forEach((g, n) => {
    const label = `group ${g.name ?? n}`;
    for (const e of g.entries) {
      if (!e.image) {
        problems.push(`${label}: the ${e.width}x${e.height} entry names image ${e.id}, which is in no RT_ICON`);
        continue;
      }
      if (e.image.width !== e.width || e.image.height !== e.height) {
        problems.push(`${label}: the ${e.width}x${e.height} entry points at a ${e.image.width}x${e.image.height} ${e.image.format} image`);
      }
      if (e.image.size !== e.bytes) problems.push(`${label}: the ${e.width}x${e.height} entry says ${e.bytes} bytes, its image is ${e.image.size}`);
    }
    for (const s of sizes) {
      const e = g.entries.find((x) => x.image && x.image.width === s && x.image.height === s);
      if (!e) problems.push(`${label}: no ${s}x${s} image (it holds ${g.entries.map((x) => (x.image ? x.image.width : `?${x.width}`)).join(', ') || 'none'})`);
      else if (want.has(s) && !want.get(s).equals(e.image.data)) problems.push(`${label}: its ${s}x${s} image is not the app's icon`);
    }
  });
  return problems;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  for (const f of process.argv.slice(2)) {
    console.log(f);
    for (const g of iconGroups(readFileSync(f))) {
      console.log(`  group ${g.name} (lang ${g.lang})`);
      for (const e of g.entries) {
        console.log(`    declared ${e.width}x${e.height} ${e.bits}bpp ${e.bytes} B -> icon ${e.id}: ${e.image ? `${e.image.format} ${e.image.width}x${e.image.height} ${e.image.bits}bpp ${e.image.size} B` : 'MISSING'}`);
      }
    }
  }
}

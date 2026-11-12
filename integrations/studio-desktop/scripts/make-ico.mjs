// PNG images -> one .ico, each image kept as the PNG it is (Windows Vista and
// later read a PNG entry at any size), and what icon/rasters.json says they
// were made from. scripts/make-icon.sh runs it; Windows and Linux have no SVG
// renderer of their own, so icon/icon.ico and icon/icon.png are made on a Mac
// from icon/icon.svg and kept in git, and test/icon.test.ts holds them to it.
//
//   node scripts/make-ico.mjs <out.ico> <a.png> [<b.png> ...]
//   node scripts/make-ico.mjs --record <dir with icon.svg, icon.png, icon.ico>
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

/** A PNG's width and height, from its IHDR, or an error for what is not a PNG. */
export function pngSize(png) {
  const signature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  if (png.length < 24 || !png.subarray(0, 8).equals(signature) || png.toString('latin1', 12, 16) !== 'IHDR') throw new Error('not a PNG');
  return { width: png.readUInt32BE(16), height: png.readUInt32BE(20) };
}

/** An ICONDIR, an ICONDIRENTRY an image (a side of 256 is written 0), then the images. */
export function ico(pngs) {
  const head = Buffer.alloc(6 + 16 * pngs.length);
  head.writeUInt16LE(0, 0);
  head.writeUInt16LE(1, 2);
  head.writeUInt16LE(pngs.length, 4);
  let offset = head.length;
  pngs.forEach((png, i) => {
    const { width, height } = pngSize(png);
    if (width > 256 || height > 256) throw new Error(`an icon image is 256 pixels at most, not ${width}x${height}`);
    const e = 6 + 16 * i;
    head.writeUInt8(width === 256 ? 0 : width, e);
    head.writeUInt8(height === 256 ? 0 : height, e + 1);
    head.writeUInt8(0, e + 2);
    head.writeUInt8(0, e + 3);
    head.writeUInt16LE(1, e + 4);
    head.writeUInt16LE(32, e + 6);
    head.writeUInt32LE(png.length, e + 8);
    head.writeUInt32LE(offset, e + 12);
    offset += png.length;
  });
  return Buffer.concat([head, ...pngs]);
}

/** The images of an .ico: each entry's stated size and its bytes. */
export function icoEntries(file) {
  if (file.readUInt16LE(0) !== 0 || file.readUInt16LE(2) !== 1) throw new Error('not an icon file');
  const out = [];
  for (let i = 0; i < file.readUInt16LE(4); i++) {
    const e = 6 + 16 * i;
    const side = (b) => (b === 0 ? 256 : b);
    const size = file.readUInt32LE(e + 8);
    const at = file.readUInt32LE(e + 12);
    out.push({ width: side(file.readUInt8(e)), height: side(file.readUInt8(e + 1)), data: file.subarray(at, at + size) });
  }
  return out;
}

export const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');

// Run, not imported (test/icon.test.ts imports it).
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [first, ...rest] = process.argv.slice(2);
  if (first === '--record') {
    const dir = rest[0];
    const record = {
      made: 'scripts/make-icon.sh rasters, on a Mac (sips draws the SVG)',
      'icon.svg': sha256(readFileSync(join(dir, 'icon.svg'))),
      'icon.png': sha256(readFileSync(join(dir, 'icon.png'))),
      'icon.ico': sha256(readFileSync(join(dir, 'icon.ico'))),
    };
    writeFileSync(join(dir, 'rasters.json'), `${JSON.stringify(record, null, 2)}\n`);
  } else {
    if (!first || rest.length === 0) throw new Error('usage: make-ico.mjs <out.ico> <a.png> [...]');
    writeFileSync(first, ico(rest.map((p) => readFileSync(p))));
  }
}

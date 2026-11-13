import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
// @ts-expect-error: scripts of plain JavaScript, with no types of their own
import { iconGroups, iconProblems, readResources } from "../scripts/pe-icons.mjs";
// @ts-expect-error: scripts of plain JavaScript, with no types of their own
import { icoImages, withIcon } from "../scripts/set-exe-icon.mjs";

// scripts/set-exe-icon.mjs writes the app's icon into the Windows exes and
// checks/icons.mjs reads them back with scripts/pe-icons.mjs; the shipped
// Windows app showed Bun's dumpling, from bun.exe's one 256-pixel group, and
// the launcher none. Held here on small PE files made in the test, of the two
// shapes the build has: one with no resource section and one whose section
// holds Bun's named group beside a version resource.

const ico = readFileSync(new URL("../icon/icon.ico", import.meta.url));
const SIZES = [16, 32, 48, 256];
const WANT = new Map((icoImages(ico) as { width: number; data: Buffer }[]).map((i) => [i.width, i.data]));

/** A PE32+ with a .text section and, if `rsrc` is given, a .rsrc section holding it (a resource tree already laid out). */
function pe(rsrc?: { va: number; bytes: Buffer }, room = 0x10000): Buffer {
	const headers = 0x200;
	const text = Buffer.alloc(0x200, 0x90);
	const sections = rsrc ? 2 : 1;
	const rsrcRaw = rsrc ? Buffer.concat([rsrc.bytes, Buffer.alloc(room - rsrc.bytes.length)]) : Buffer.alloc(0);
	const file = Buffer.alloc(headers + text.length + rsrcRaw.length);
	file.write("MZ", 0, "latin1");
	file.writeUInt32LE(0x80, 0x3c);
	file.writeUInt32LE(0x4550, 0x80);
	file.writeUInt16LE(0x8664, 0x84);
	file.writeUInt16LE(sections, 0x86);
	file.writeUInt16LE(240, 0x94);
	const opt = 0x98;
	file.writeUInt16LE(0x20b, opt);
	file.writeUInt32LE(0x1000, opt + 32);
	file.writeUInt32LE(0x200, opt + 36);
	file.writeUInt32LE(rsrc ? 0x4000 : 0x2000, opt + 56);
	file.writeUInt32LE(headers, opt + 60);
	file.writeUInt32LE(16, opt + 108);
	const section = (i: number, name: string, va: number, size: number, raw: number) => {
		const h = opt + 240 + 40 * i;
		file.write(name, h, "latin1");
		file.writeUInt32LE(size, h + 8);
		file.writeUInt32LE(va, h + 12);
		file.writeUInt32LE(size, h + 16);
		file.writeUInt32LE(raw, h + 20);
	};
	section(0, ".text", 0x1000, text.length, headers);
	text.copy(file, headers);
	if (rsrc) {
		section(1, ".rsrc", rsrc.va, rsrcRaw.length, headers + text.length);
		rsrcRaw.copy(file, headers + text.length);
		file.writeUInt32LE(rsrc.va, opt + 112 + 16);
		file.writeUInt32LE(rsrc.bytes.length, opt + 112 + 20);
	}
	return file;
}

/** Bun's shape: a named RT_GROUP_ICON naming one 256-pixel RT_ICON, and a version resource (16) beside them. */
function bunLike(room?: number): Buffer {
	const va = 0x2000;
	const b = Buffer.alloc(0x400);
	const u16 = (at: number, v: number) => b.writeUInt16LE(v, at);
	const u32 = (at: number, v: number) => b.writeUInt32LE(v >>> 0, at);
	// root: 3 types, 1 named (the group's type is 14; its name below is the string), then ids 3, 16
	u16(14, 3);
	let o = 16;
	const dirs = [0x40, 0x80, 0xc0];
	[3, 14, 16].forEach((t, i) => {
		u32(o, t);
		u32(o + 4, 0x80000000 | dirs[i]);
		o += 8;
	});
	// type dirs -> name dir -> lang dir -> data entry; names 1, "IDI" (a string at 0x300), 1
	const names: Array<number> = [1, 0x80000300, 1];
	const langDirs = [0x100, 0x120, 0x140];
	const entries = [0x160, 0x170, 0x180];
	const group = Buffer.from([0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 1, 0, 32, 0, 0, 0, 0, 0, 1, 0]);
	const blobs = [Buffer.alloc(40 + 4, 1), group, Buffer.from("version")];
	group.writeUInt32LE(blobs[0].length, 14);
	blobs[0].writeUInt32LE(40, 0);
	blobs[0].writeInt32LE(256, 4);
	blobs[0].writeInt32LE(512, 8);
	blobs[0].writeUInt16LE(32, 14);
	let data = 0x200;
	[0, 1, 2].forEach((i) => {
		u16(dirs[i] + 14, 1);
		u32(dirs[i] + 16, names[i]);
		u32(dirs[i] + 20, 0x80000000 | langDirs[i]);
		u16(langDirs[i] + 14, 1);
		u32(langDirs[i] + 16, 1033);
		u32(langDirs[i] + 20, entries[i]);
		u32(entries[i], va + data);
		u32(entries[i] + 4, blobs[i].length);
		blobs[i].copy(b, data);
		data += 0x40;
	});
	// the group's name: a length, then its UTF-16
	b.writeUInt16LE(3, 0x300);
	b.write("IDI", 0x302, "utf16le");
	return pe({ va, bytes: b }, room);
}

test("a file with no resources is given the app's icon at every size", () => {
	const bare = pe();
	assert.deepEqual(iconGroups(bare), []);
	assert.match(iconProblems(iconGroups(bare), SIZES, WANT)[0], /no icon group/);
	const done = withIcon(bare, ico);
	assert.deepEqual(iconProblems(iconGroups(done), SIZES, WANT), []);
	assert.deepEqual(
		iconGroups(done)[0].entries.map((e: { width: number }) => e.width),
		[16, 24, 32, 48, 64, 128, 256],
	);
});

test("a file with Bun's one 256-pixel icon fails, and has it replaced, its other resources kept", () => {
	const bun = bunLike();
	const before = iconGroups(bun);
	assert.equal(before.length, 1);
	assert.equal(before[0].name, "IDI");
	assert.deepEqual(
		before[0].entries.map((e: { width: number }) => e.width),
		[256],
	);
	const problems = iconProblems(before, SIZES, WANT) as string[];
	assert.ok(problems.some((p) => /no 16x16/.test(p)), problems.join("\n"));
	assert.ok(problems.some((p) => /not the app's icon/.test(p)) || problems.some((p) => /no 256x256|bytes/.test(p)), problems.join("\n"));
	const done = withIcon(bun, ico);
	assert.deepEqual(iconProblems(iconGroups(done), SIZES, WANT), []);
	assert.equal(iconGroups(done).length, 1, "Bun's named group is gone: it would be preferred to the app's");
	const rest = (readResources(done) as { type: number; data: Buffer }[]).filter((r) => r.type === 16);
	assert.deepEqual(
		rest.map((r) => r.data.toString()),
		["version"],
	);
});

test("a section too small for the icon is left empty and the resources go in a new one", () => {
	const done = withIcon(bunLike(0x400), ico);
	assert.deepEqual(iconProblems(iconGroups(done), SIZES, WANT), []);
	assert.deepEqual(
		(readResources(done) as { type: number; data: Buffer }[]).filter((r) => r.type === 16).map((r) => r.data.toString()),
		["version"],
	);
});

test("an entry that points at a picture of another size, or another length, is a problem", () => {
	const done = withIcon(pe(), ico);
	const groups = iconGroups(done);
	groups[0].entries[0].width = 32; // as if the group said 32 for the 16-pixel image
	assert.ok((iconProblems(groups, SIZES, WANT) as string[]).some((p) => /entry points at a 16x16/.test(p)));
	const groups2 = iconGroups(done);
	groups2[0].entries[1].bytes += 1;
	assert.ok((iconProblems(groups2, SIZES, WANT) as string[]).some((p) => /bytes, its image is/.test(p)));
});

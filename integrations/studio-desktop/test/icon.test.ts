import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
// @ts-expect-error: a script of plain JavaScript, with no types of its own
import { icoEntries, pngSize, sha256 } from "../scripts/make-ico.mjs";

// The icons Windows and Linux are given (build.win.icon, build.linux.icon, and
// what a .fennec file's registration shows), made on a Mac from icon/icon.svg
// by `scripts/make-icon.sh rasters` and kept in git, since neither system has
// an SVG renderer of its own: held to the SVG they were made from, so a change
// to it fails here until they are made again.

const icon = (name: string) => readFileSync(new URL(`../icon/${name}`, import.meta.url));
const record = JSON.parse(icon("rasters.json").toString("utf8")) as Record<string, string>;

test("the rasters were made from the icon as it is, and are what was made", () => {
	assert.equal(sha256(icon("icon.svg")), record["icon.svg"], "icon/icon.svg changed since the rasters were made: run `sh scripts/make-icon.sh rasters` on a Mac");
	assert.equal(sha256(icon("icon.png")), record["icon.png"], "icon/icon.png is not what make-icon.sh made");
	assert.equal(sha256(icon("icon.ico")), record["icon.ico"], "icon/icon.ico is not what make-icon.sh made");
});

test("the .ico holds a PNG for each size Explorer asks for, up to 256", () => {
	const entries = icoEntries(icon("icon.ico")) as { width: number; height: number; data: Buffer }[];
	assert.deepEqual(
		entries.map((e) => e.width),
		[16, 24, 32, 48, 64, 128, 256],
	);
	for (const e of entries) assert.deepEqual(pngSize(e.data), { width: e.width, height: e.height }, `the ${e.width} entry is a PNG of that size`);
});

test("the .png is 512 pixels square, what a desktop entry and a window take", () => {
	assert.deepEqual(pngSize(icon("icon.png")), { width: 512, height: 512 });
});

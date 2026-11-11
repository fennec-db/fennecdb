import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import {
	filePartition,
	goesWithWindow,
	inUse,
	partitionFolder,
	remotePartition,
	removeStoreFolder,
	storeClosed,
	storeOpened,
	sweepStores,
	webview2Name,
	webview2Root,
} from "../src/bun/stores.ts";

// The windows' data stores: which a window gets, and on Windows how one is
// taken away -- its WebView2 folder, never while a window is in it. The
// folders here are made by the test as WebView2 lays them out;
// `checks/storage.mjs` holds the app's own on Windows.

const work = mkdtempSync(join(tmpdir(), "fennec-desktop-stores-"));
after(() => rmSync(work, { recursive: true, force: true }));

test("a file window's store is its own and goes with it; on Windows a folder no other window is given", () => {
	const mac = filePartition("darwin");
	assert.match(mac, /^file-[0-9a-f-]{36}$/, "in memory: no persist:");
	assert.match(filePartition("linux"), /^file-[0-9a-f-]{36}$/);
	const win = filePartition("win32");
	assert.match(win, /^persist:file-[0-9a-f-]{36}$/, "a folder named once, where Ephemeral\\<view id> is named again next run");
	assert.notEqual(filePartition("win32"), win);
	assert.ok(goesWithWindow(mac) && goesWithWindow(win));
	assert.ok(!goesWithWindow(remotePartition("abc")), "a saved server's stays");
	assert.equal(remotePartition("abc"), "persist:remote-abc");
});

test("WebView2's folder for a partition is Electrobun's name for it, under Partitions", () => {
	const root = join("C:", "Users", "me", "AppData", "Local", "dev.fennecdb.studio", "stable", "WebView2");
	const id = "0b4c6a3e-8a7d-4d2a-9d8e-1f2a3b4c5d6e";
	assert.equal(partitionFolder(root, `persist:remote-${id}`), join(root, "Partitions", `remote-${id}`));
	assert.equal(partitionFolder(root, `file-${id}`), null, "not persist: Ephemeral\\<view id>, not this app's to name");
	// A name that is not plain is hashed as windows_profile_paths.h hashes it: its code points as UTF-32BE.
	const odd = "Remote/../x";
	const utf32 = Buffer.alloc(odd.length * 4);
	[...odd].forEach((c, i) => utf32.writeUInt32BE(c.codePointAt(0)!, i * 4));
	assert.equal(webview2Name(odd), `~h-${createHash("sha256").update(utf32).digest("hex")}`);
	assert.equal(webview2Name("con"), `~h-${createHash("sha256").update(Buffer.from([0, 0, 0, 0x63, 0, 0, 0, 0x6f, 0, 0, 0, 0x6e])).digest("hex")}`, "a device name");
	assert.equal(webview2Name("a".repeat(80)), "a".repeat(80));
	assert.match(webview2Name("a".repeat(81)), /^~h-/);
	// Nothing climbs out of Partitions, whatever the name.
	for (const name of ["..", "../..", "a\\..\\..", "/abs"]) assert.ok(partitionFolder(root, `persist:${name}`)!.startsWith(join(root, "Partitions", "~h-")), name);
});

test("the WebView2 folder is the identifier's and channel's under LOCALAPPDATA, as Electrobun's, and none without it", () => {
	assert.equal(webview2Root({ LOCALAPPDATA: "L" }, { identifier: "dev.fennecdb.studio.test", channel: "stable" }), join("L", "dev.fennecdb.studio.test", "stable", "WebView2"));
	assert.equal(webview2Root({}, { identifier: "x", channel: "stable" }), null);
	assert.equal(webview2Root({ LOCALAPPDATA: "L" }, {}), null);
});

/** A partition's folder as WebView2 leaves one, with a file in it. */
function made(root: string, partition: string) {
	const folder = partitionFolder(root, partition)!;
	mkdirSync(join(folder, "EBWebView", "Default", "Local Storage"), { recursive: true });
	writeFileSync(join(folder, "EBWebView", "Default", "Local Storage", "leveldb.log"), "history");
	return folder;
}

test("a store's folder is removed once no window is in it, and never while one is", async () => {
	const root = join(work, "remove");
	const p = remotePartition("11111111-2222-3333-4444-555555555555");
	const folder = made(root, p);
	storeOpened(p);
	storeOpened(p);
	assert.equal(await removeStoreFolder(root, p), false, "a window is in it");
	assert.ok(existsSync(folder));
	storeClosed(p);
	assert.ok(inUse(p), "one of two closed");
	assert.equal(await removeStoreFolder(root, p), false);
	storeClosed(p);
	assert.ok(!inUse(p));
	assert.equal(await removeStoreFolder(root, p), true);
	assert.ok(!existsSync(folder));
	assert.equal(await removeStoreFolder(root, p), true, "gone already is gone");
	assert.equal(await removeStoreFolder(root, "file-not-persistent"), true, "a store in memory has no folder");
});

test("a start sweeps the file windows' folders a run left, not one in use, not a saved server's", async () => {
	const root = join(work, "sweep");
	const left = made(root, `persist:file-${"a".repeat(8)}-${"b".repeat(4)}-${"c".repeat(4)}-${"d".repeat(4)}-${"e".repeat(12)}`);
	const live = filePartition("win32");
	const liveFolder = made(root, live);
	const remote = made(root, remotePartition("11111111-2222-3333-4444-555555555555"));
	mkdirSync(join(root, "Ephemeral", "3"), { recursive: true });
	storeOpened(live);
	try {
		await sweepStores(root);
	} finally {
		storeClosed(live);
	}
	assert.ok(!existsSync(left), "a file window's, left by a run that ended");
	assert.ok(existsSync(liveFolder), "one a window is in now");
	assert.ok(existsSync(remote), "a saved server's: kept between runs");
	assert.ok(!existsSync(join(root, "Ephemeral")), "Ephemeral\\<view id>: named again by the next run's views");
});

// The data stores the studio windows' pages keep (localStorage, IndexedDB:
// the studio's history, saved queries and theme): which one a window gets,
// which are in use, and on Windows how a store is taken away.
//
// A window's store is named by its partition: a saved server's is
// `persist:remote-<id>`, kept between runs and emptied when the connection is
// removed; a file's window gets one of its own that goes with it. On macOS and
// Linux that is a name without `persist:`, a store in memory (WebKit's
// non-persistent data store). Windows is otherwise:
//   - WebView2 keeps every partition in a folder, `<LOCALAPPDATA>\<identifier>\
//     <channel>\WebView2\Partitions\<name>` for `persist:<name>` and
//     `Ephemeral\<view id>` for any other, where the view id is a counter that
//     starts again each run (Electrobun 2.0.4, windows_profile_paths.h): a file
//     window in the next run with the same id would open on the earlier one's
//     store. So a file's window there gets `persist:file-<uuid>`, a folder
//     no later window is ever given, removed once the window has closed.
//   - Electrobun's `sessionClearStorageData` clears the first WebView2 view it
//     finds, whatever partition it is asked for (upstream #608): another
//     window's store, if any is open. It is never called there; a removed
//     connection's folder is removed instead.
// A folder is removed only while no window of the app uses its partition
// (`inUse`, asked again before each try, all on the main process's one
// thread), only under `Partitions\` by Electrobun's own naming, and the
// leftovers of a run that ended without removing them are swept at the next
// start by the one instance alone (instance.ts). Pure but for the files, so
// `node --test` holds it (test/stores.test.ts).

import { createHash, randomUUID } from "node:crypto";
import { readdirSync, rmSync } from "node:fs";
import { join } from "node:path";

/** The partitions of the windows open now, with how many windows each. */
const open = new Map<string, number>();

/** Called before a window's view is made in `partition`. */
export function storeOpened(partition: string) {
	open.set(partition, (open.get(partition) ?? 0) + 1);
}

/** Called once that window has closed. */
export function storeClosed(partition: string) {
	const n = (open.get(partition) ?? 0) - 1;
	if (n > 0) open.set(partition, n);
	else open.delete(partition);
}

/** Is a window of the app in `partition` now? */
export const inUse = (partition: string) => open.has(partition);

/** A saved server's windows' store: kept between runs, its own. */
export const remotePartition = (id: string) => `persist:remote-${id}`;

/** A new store for one file window, which goes with it: in memory, or on Windows a folder no other window is given. */
export const filePartition = (platform: NodeJS.Platform = process.platform) => (platform === "win32" ? `persist:file-${randomUUID()}` : `file-${randomUUID()}`);

/** Does a store in `partition` go when its window does? */
export const goesWithWindow = (partition: string) => /^(persist:)?file-/.test(partition);

/**
 * The folder name WebView2 is given for `persist:<name>` (Electrobun's
 * buildWindowsWebView2PartitionDirectoryName): the name itself when it is
 * 1 to 80 of `a-z0-9-_` and no device name, else `~h-` and the SHA-256 of its
 * code points as UTF-32BE.
 */
export function webview2Name(name: string): string {
	const plain = /^[a-z0-9_-]{1,80}$/.test(name) && !/^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/.test(name);
	if (plain) return name;
	const identity = Buffer.alloc([...name].length * 4);
	[...name].forEach((ch, i) => identity.writeUInt32BE(ch.codePointAt(0)!, i * 4));
	return `~h-${createHash("sha256").update(identity).digest("hex")}`;
}

/** The folder WebView2 keeps a persistent `partition` in under `root` (its WebView2 folder); null for any other partition. */
export function partitionFolder(root: string, partition: string): string | null {
	if (!partition.startsWith("persist:")) return null;
	return join(root, "Partitions", webview2Name(partition.slice("persist:".length)));
}

/** Windows: the WebView2 folder of the app's identifier and channel (`version.json`), where Electrobun puts it; null without LOCALAPPDATA, where it puts none. */
export function webview2Root(env: Record<string, string | undefined>, version: { identifier?: string; channel?: string }): string | null {
	const base = env["LOCALAPPDATA"];
	if (!base || !version.identifier || !version.channel) return null;
	return join(base, version.identifier, version.channel, "WebView2");
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/**
 * Removes the folder of `partition` under `root` once it is free, trying
 * again while WebView2's processes, which outlive their window by a moment,
 * still hold its files, for up to `ms`. Never while a window uses the
 * partition: asked before every try, and a try is one synchronous call, so no
 * window can be made in it meanwhile. True once it is gone (or never was).
 */
export async function removeStoreFolder(root: string, partition: string, ms = 20_000): Promise<boolean> {
	const folder = partitionFolder(root, partition);
	if (!folder) return true;
	const until = Date.now() + ms;
	for (;;) {
		if (inUse(partition)) return false;
		try {
			rmSync(folder, { recursive: true, force: true });
			return true;
		} catch (e) {
			const code = (e as NodeJS.ErrnoException).code;
			if (!["EBUSY", "EPERM", "EACCES", "ENOTEMPTY"].includes(code ?? "") || Date.now() > until) return false;
		}
		await sleep(200);
	}
}

/**
 * What a run that ended before its file windows' stores were removed left:
 * `Partitions\file-*` not in use, and `Ephemeral\` (no window of this app is
 * made in one now). Only by the one instance (instance.ts): another's windows
 * would be in them. Each is removed as `removeStoreFolder` removes one.
 */
export async function sweepStores(root: string): Promise<void> {
	let names: string[] = [];
	try {
		names = readdirSync(join(root, "Partitions")).filter((n) => /^file-[0-9a-f-]{36}$/.test(n));
	} catch {
		// none
	}
	await Promise.all(names.map((n) => removeStoreFolder(root, `persist:${n}`, 5000)));
	try {
		rmSync(join(root, "Ephemeral"), { recursive: true, force: true });
	} catch {
		// held by a process that has not ended yet: the next start takes it
	}
}

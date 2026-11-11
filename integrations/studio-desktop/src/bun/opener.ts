// What the app hands the system to open: a web link a studio page asked for
// (to the default browser) and a database's folder (to the file manager).
// **Neither goes through a shell.** macOS's Electrobun calls NSWorkspace and
// Windows' ShellExecute, which take the address as data. Linux's does not:
// Electrobun 2.0.4's `openExternal` falls back, when GIO cannot start the
// default browser, to `system("xdg-open \"<url>\"")`, and `showItemInFolder`
// always runs `system("xdg-open \"<file URI>\"")` -- a shell over text a page
// or a folder's name chose, inside double quotes where `$(...)` still runs. On
// the spike's runners `https://example.invalid/?q=$(touch$IFS<marker>)` made
// the marker (#578). So on Linux the app never calls either: it starts
// `xdg-open` itself with the address as one argument (`gio open` where there is
// no xdg-open), and `checks/opener.mjs` holds it to a link and a folder whose
// names would run a command in any shell they reached.
//
// The address handed over is made inert besides: a link is what the URL
// parser makes of it, with the characters a shell reads inside quotes
// (`$`, `` ` ``, `\`, `"`, `'`) percent-encoded, and a folder is a file: URI with
// every byte but letters, digits, `-._~` and `/` percent-encoded, so not even a
// program further on that did quote it badly (xdg-open is itself a script)
// would find one. Pure but for the spawn, so `node --test` holds the encoding and
// the filter (test/opener.test.ts).

import { spawn } from "node:child_process";
import { statSync } from "node:fs";
import { dirname } from "node:path";

/** A window is handed at most one link in this long: a page that asks over and over does not open tabs without end. */
export const EXTERNAL_GAP_MS = 3000;

/** A link as the system is handed it, or null: http(s) alone, as the URL parser reads it, with what a shell would read inside quotes encoded. */
export function webLink(url: string): string | null {
	let u: URL;
	try {
		u = new URL(url);
	} catch {
		return null;
	}
	if (u.protocol !== "http:" && u.protocol !== "https:") return null;
	return u.href.replace(/[$`\\"']/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase().padStart(2, "0")}`);
}

/**
 * Whether a studio window's request to open `url` goes to the system now, and
 * what it is handed: only a web address (never `file:` or a custom scheme),
 * and one in EXTERNAL_GAP_MS a window (`last` is that window's). Electrobun
 * 2.0.4 does not say whether a click asked for it: a script's `window.open` is
 * the same event.
 */
export function linkOut(url: string, last: { at: number }, now = Date.now()): string | null {
	const link = webLink(url);
	if (!link || now - last.at < EXTERNAL_GAP_MS) return null;
	last.at = now;
	return link;
}

/** A path as a file: URI with every byte outside `A-Za-z0-9-._~/` percent-encoded. */
export function fileUri(path: string): string {
	const bytes = Buffer.from(path, "utf8");
	let out = "";
	for (const b of bytes) {
		const c = String.fromCharCode(b);
		out += /[A-Za-z0-9\-._~/]/.test(c) ? c : `%${b.toString(16).toUpperCase().padStart(2, "0")}`;
	}
	return `file://${out}`;
}

/** The folder a path is shown in: the folder itself, or the one a file is in. */
export function folderOf(path: string): string {
	return statSync(path, { throwIfNoEntry: false })?.isDirectory() ? path : dirname(path);
}

/** The programs that open an address on Linux, each an argument vector, in the order tried. */
export const linuxOpeners = (target: string): string[][] => [
	["xdg-open", target],
	["gio", "open", target],
];

/**
 * Starts the first of `argvs` that can be started, with no shell, and
 * resolves with how it ended (its code, or the error that kept every one of
 * them from starting). The opener goes on alone: a browser it starts may
 * outlive the app.
 */
export function runFirst(argvs: string[][], start = spawn): Promise<{ argv: string[]; code: number | null } | { error: string }> {
	return new Promise((resolve) => {
		const next = (i: number, why: string) => {
			const argv = argvs[i];
			if (!argv) return resolve({ error: why });
			const child = start(argv[0]!, argv.slice(1), { stdio: "ignore", shell: false });
			child.unref();
			// A program that could not be started may still report an exit: only the first word of each counts.
			let said = false;
			child.once("error", (e: NodeJS.ErrnoException) => {
				if (said) return;
				said = true;
				if (e.code === "ENOENT") next(i + 1, `${why}${why ? "; " : ""}${argv[0]}: not found`);
				else resolve({ error: `${argv[0]}: ${e.message}` });
			});
			child.once("exit", (code) => {
				if (said) return;
				said = true;
				resolve({ argv, code });
			});
		};
		next(0, "");
	});
}

/** Electrobun's calls where they take no shell (macOS, Windows). Loaded only there, and never by a test. */
const utils = async () => (await import("electrobun/main")).Utils;

/** Opens a web link (already through `linkOut`) in the default browser. */
export async function openLink(link: string): Promise<unknown> {
	if (process.platform === "linux") return runFirst(linuxOpeners(link));
	return (await utils()).openExternal(link);
}

/** Shows `path` (a database file, or a folder) in the file manager. */
export async function reveal(path: string): Promise<unknown> {
	if (process.platform === "linux") return runFirst(linuxOpeners(fileUri(folderOf(path))));
	return (await utils()).showItemInFolder(path);
}

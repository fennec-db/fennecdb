// The files the system hands the app on Windows and Linux: a `.fennec`
// double-clicked in Explorer or a file manager.
//
// macOS hands a double-clicked file over as an `open-url` event (index.ts),
// from the document type in Info.plist. Windows and Linux start the program a
// registration names with the file as an argument (associate.ts registers the
// app's own launcher), and Electrobun 2.0.4's launcher starts the main process
// as `[bun, main.js]`, dropping its own arguments (upstream #483, #554;
// launcher/main.zig). It stays that process's parent for the app's life,
// waiting on it, so the arguments are still there as the launcher's own
// command line, and the app reads them back: on Linux `/proc/<parent>/cmdline`,
// each argument as the system handed it, NUL-separated; on Windows the one
// string CreateProcess was given (NtQueryInformationProcess, win32.ts), split
// by the rules the launcher's own runtime splits it by (Zig's, the C
// runtime's since 2008). The parent is taken for the launcher only when its
// program is the `launcher` beside this process's Bun: a terminal that ran
// the Bun, a harness or a reparenting init hands nothing.
//
// Each argument after the program is a path -- the registration's `%1` on
// Windows, `%f` on Linux -- data that is never run or read as a command:
// opened as Open… opens a file, so one that is not a database file is said in
// words (app.ts). Not paths: Linux's `--automation`, which the launcher reads
// itself (WebKitWebDriver's), and `--uninstall`, with which it never starts
// the app. A relative path is taken from the launcher's folder on Linux;
// Windows has no way to read another process's folder short of its memory,
// and Explorer and every registration hand a full path, so there one is
// taken as it is (and found nowhere, in words).
//
// Pure but for reading the parent, so `node --test` holds the parsing
// (test/handover.test.ts) and `checks/finder.mjs` the whole path.

import { readFileSync, readlinkSync, realpathSync } from "node:fs";
import { basename, posix, win32 } from "node:path";

/** Paths as `platform` reads them, whatever system this runs on (the unit tests run every system's). */
const pathsOn = (platform: NodeJS.Platform) => (platform === "win32" ? win32 : posix);

/**
 * A Windows command line as the launcher's runtime splits it: the program is
 * everything up to the next quote if it starts with one, else up to the first
 * blank, with no escape; then arguments split at blanks outside quotes, where
 * `2n` backslashes before a quote are `n` and the quote opens or closes a
 * quoted run, `2n+1` are `n` and a literal quote, other backslashes are
 * themselves, and `""` inside a quoted run is one quote.
 */
export function windowsArgv(line: string): string[] {
	const out: string[] = [];
	let i = 0;
	let program = "";
	if (line[0] === '"') {
		i = 1;
		while (i < line.length && line[i] !== '"') program += line[i++];
		if (i < line.length) i++;
	} else {
		while (i < line.length && line[i] !== " " && line[i] !== "\t") program += line[i++];
	}
	out.push(program);
	for (;;) {
		while (line[i] === " " || line[i] === "\t") i++;
		if (i >= line.length) break;
		let arg = "";
		let quoted = false;
		while (i < line.length) {
			const c = line[i]!;
			if ((c === " " || c === "\t") && !quoted) break;
			if (c === "\\") {
				let n = 0;
				while (line[i] === "\\") {
					n++;
					i++;
				}
				if (line[i] === '"') {
					arg += "\\".repeat(n >> 1);
					if (n % 2 === 1) {
						arg += '"';
						i++;
					}
				} else {
					arg += "\\".repeat(n);
				}
				continue;
			}
			if (c === '"') {
				if (quoted && line[i + 1] === '"') {
					arg += '"';
					i += 2;
					continue;
				}
				quoted = !quoted;
				i++;
				continue;
			}
			arg += c;
			i++;
		}
		out.push(arg);
	}
	return out;
}

/** Linux's `/proc/<pid>/cmdline`: each argument ended by a NUL. */
export function procArgv(cmdline: Buffer): string[] {
	const text = cmdline.toString("utf8");
	const parts = text.split("\0");
	if (parts[parts.length - 1] === "") parts.pop();
	return parts;
}

/** The launcher's file name on `platform`. */
export const launcherName = (platform: NodeJS.Platform) => (platform === "win32" ? "launcher.exe" : "launcher");

/** Whether `parent` is the launcher beside `bun`, compared as the system compares paths. */
export function isOurLauncher(parent: string, bun: string, platform: NodeJS.Platform): boolean {
	const p = pathsOn(platform);
	const want = p.join(p.dirname(bun), launcherName(platform));
	return platform === "win32" ? parent.toLowerCase() === want.toLowerCase() : parent === want;
}

/** The files among the launcher's arguments (`argv[0]` is the launcher), relative ones resolved against `cwd` where it is known. */
export function handedPaths(argv: string[], platform: NodeJS.Platform, cwd: string | null): string[] {
	const p = pathsOn(platform);
	const out: string[] = [];
	for (const arg of argv.slice(1)) {
		if (arg === "" || arg === "--uninstall" || (platform === "linux" && arg === "--automation")) continue;
		out.push(p.isAbsolute(arg) || !cwd ? arg : p.resolve(cwd, arg));
	}
	return out;
}

/** What the launcher was handed: its arguments and folder, or why none are read. */
export type Launched = { argv: string[]; cwd: string | null } | { none: string };

/** Linux: the launcher's arguments, read from its entry in /proc. */
function linuxLaunched(ppid: number): Launched {
	let parent: string;
	try {
		parent = readlinkSync(`/proc/${ppid}/exe`);
	} catch (e) {
		return { none: `the parent (${ppid}) is not readable: ${(e as Error).message}` };
	}
	const bun = realpathSync(process.execPath);
	if (!isOurLauncher(parent, bun, "linux")) return { none: `the parent is ${basename(parent)}, not this app's launcher` };
	let cwd: string | null = null;
	try {
		cwd = readlinkSync(`/proc/${ppid}/cwd`);
	} catch {
		// Relative paths are then taken as they are.
	}
	return { argv: procArgv(readFileSync(`/proc/${ppid}/cmdline`)), cwd };
}

/** Windows: the launcher's command line, read through the system (win32.ts). */
async function windowsLaunched(ppid: number): Promise<Launched> {
	const w = await import("./win32.ts");
	const { image, commandLine } = w.processCommandLine(ppid);
	if (!isOurLauncher(image, process.execPath, "win32")) return { none: `the parent is ${basename(image)}, not this app's launcher` };
	return { argv: windowsArgv(commandLine), cwd: null };
}

/** What the launcher that started this process was handed: nothing on macOS, where Finder's files come as `open-url`. */
export async function launched(): Promise<Launched> {
	const ppid = process.ppid;
	if (!ppid || ppid <= 1) return { none: `no parent (${ppid})` };
	try {
		if (process.platform === "linux") return linuxLaunched(ppid);
		if (process.platform === "win32") return await windowsLaunched(ppid);
	} catch (e) {
		return { none: `the launcher's arguments could not be read: ${(e as Error).message}` };
	}
	return { none: "macOS hands files over as open-url" };
}

/** The files the system handed the launcher, none where it handed none or none could be read. */
export const pathsOf = (l: Launched, platform: NodeJS.Platform = process.platform) => ("argv" in l ? handedPaths(l.argv, platform, l.cwd) : []);

// What the unit tests ask of the system they run on, the same question on
// macOS, Linux and Windows: a program's name, a process's arguments, whether a
// file is its owner's alone. A test a system cannot pass yet is skipped there
// with its reason and the phase of the Windows and Linux plan that brings it
// (#578; docs/design/integrations.md#studio-desktop), which `node --test`
// prints beside it; since phase 2 none is.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

export const WINDOWS = process.platform === "win32";
/** A program's file name ends so: the release's Windows zip holds `fennec-server.exe`. */
export const EXE = WINDOWS ? ".exe" : "";

// A path, not a URL's: a `file:` URL's pathname is /D:/a/... on Windows.
const APP = fileURLToPath(new URL("../", import.meta.url));

/**
 * Windows: the Bun the app is built with and runs on, where
 * scripts/electrobun.mjs fetched it (HUTCH_HOME, else .hutch-home/) and held
 * it to hutch-pin.json; `npm run typecheck` fetches it before the tests, as
 * CI runs them.
 */
function pinnedBun(): string {
	const home = process.env["HUTCH_HOME"] || join(APP, ".hutch-home");
	const pin = JSON.parse(readFileSync(join(APP, "hutch-pin.json"), "utf8")) as Record<string, { runtime?: Record<string, unknown> }>;
	const system = `windows-${process.arch}`;
	const dir = Object.keys(pin[system]?.runtime ?? {}).find((d) => d.startsWith("toolchains/bun/"));
	if (!dir) throw new Error(`hutch-pin.json pins no Bun for ${system}`);
	const bun = join(home, ...dir.split("/"), "bun.exe");
	if (!existsSync(bun)) throw new Error(`${bun} is not there: \`npm run typecheck\` fetches it`);
	return bun;
}

/**
 * Windows: `test/win32-read.ts` run by the app's Bun, which asks Windows
 * through the app's own calls (win32.ts). Not by PowerShell, as the tests
 * did: `pwsh` crashed on the arm64 runner once in some 600 to 1000 starts,
 * "Internal CLR error. (0x80131506)" and exit 0xC0000005, or a
 * SynchronizationLockException, the process asked about alive (jobs
 * 132046665510, 132452559288, 132452559492), with or without its startup
 * profile written, so a test failed with nothing of the app's wrong.
 */
function readWin32(what: "command-line" | "security", of: string): string {
	return execFileSync(pinnedBun(), [join(APP, "test", "win32-read.ts"), what, of]).toString();
}

/**
 * A process's command line, as the system shows it to every user (`ps`), or
 * on Windows to its own: by the app's own reader, as Task Manager reads one
 * (win32.ts's `processCommandLine`).
 */
export function argvOf(pid: number): string {
	if (!WINDOWS) return execFileSync("ps", ["-ww", "-o", "command=", "-p", String(pid)]).toString();
	return readWin32("command-line", String(Number(pid)));
}

/**
 * Whether only the file's owner can read it: mode 0600 on a Unix; on Windows,
 * where a mode is nothing and a file inherits its folder's ACL, a protected DACL
 * that grants this user full control and nobody anything else, as `fennec key`
 * makes it (fennec-http's `owner_only`). Says what it found otherwise.
 */
export function ownerAlone(path: string): true | string {
	if (!WINDOWS) {
		const mode = statSync(path).mode & 0o777;
		return mode === 0o600 || `mode ${mode.toString(8)}`;
	}
	// Its owner and DACL as SDDL, read by the app's own call (win32.ts's
	// `pipeSddl`), as scripts/checks/pages.mjs reads the pipe's. SDDL writes an
	// account it has an alias for by the alias (the built-in Administrator, RID
	// 500, which the runners run as, is LA), and the DACL's flags as letters: P,
	// protected, and AI where the call that set it adds it. One entry, its flags none:
	// every right (FA, FILE_ALL_ACCESS) to this user, and nothing to anyone else.
	const [me = "", sddl = ""] = readWin32("security", path).split("\n");
	if (!/^S-1-[0-9-]+$/.test(me)) return `no SID: ${me}`;
	const who = me.endsWith("-500") ? `(?:LA|${me})` : me;
	return new RegExp(`D:P(?:AI)?\\(A;;FA;;;${who}\\)$`).test(sddl) || `me ${me}: ${sddl}`;
}

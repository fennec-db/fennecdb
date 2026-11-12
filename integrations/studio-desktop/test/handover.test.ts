import assert from "node:assert/strict";
import { test } from "node:test";
import { handedPaths, isOurLauncher, procArgv, windowsArgv } from "../src/bun/handover.ts";

// The files a double-click hands the launcher, read back by the app from the
// launcher's command line (Electrobun 2.0.4's launcher drops its arguments):
// split as the launcher's own runtime splits it, and told from the launcher's
// own flags. `checks/finder.mjs` holds the read itself on Windows and Linux.

test("Windows: the registration's command line is the launcher and the file, whole", () => {
	assert.deepEqual(windowsArgv(String.raw`"C:\Users\a b\AppData\Local\dev.fennecdb.studio\stable\app\bin\launcher.exe" "C:\Users\a b\data\with space\şirket 1.fennec"`), [
		String.raw`C:\Users\a b\AppData\Local\dev.fennecdb.studio\stable\app\bin\launcher.exe`,
		String.raw`C:\Users\a b\data\with space\şirket 1.fennec`,
	]);
	assert.deepEqual(windowsArgv(String.raw`launcher.exe C:\x\y.fennec`), ["launcher.exe", String.raw`C:\x\y.fennec`]);
	assert.deepEqual(windowsArgv(String.raw`"C:\l.exe"`), [String.raw`C:\l.exe`], "no file: the program alone");
	assert.deepEqual(windowsArgv(String.raw`"C:\l.exe"   `), [String.raw`C:\l.exe`], "trailing blanks are no argument");
});

test("Windows: the C runtime's rules, which the launcher's runtime splits by", () => {
	// The program takes no escape: up to the next quote, a backslash before it its own.
	assert.deepEqual(windowsArgv('"C:\\dir\\" x'), ["C:\\dir\\", "x"]);
	// 2n backslashes and a quote: n of them, and the quote opens or closes a run.
	assert.deepEqual(windowsArgv('p "a\\\\" b'), ["p", "a\\", "b"]);
	// 2n+1: n and a literal quote.
	assert.deepEqual(windowsArgv('p a\\"b'), ["p", 'a"b']);
	assert.deepEqual(windowsArgv('p a\\\\\\"b'), ["p", 'a\\"b']);
	// Backslashes before anything else are themselves.
	assert.deepEqual(windowsArgv("p C:\\a\\\\b"), ["p", "C:\\a\\\\b"]);
	// "" inside a quoted run is one quote; a quoted run joins what touches it.
	assert.deepEqual(windowsArgv(`p "a""b" x"y z"w`), ["p", 'a"b', "xy zw"]);
	assert.deepEqual(windowsArgv(`p\t"" x`), ["p", "", "x"]);
});

test("Linux: /proc's command line is the arguments as the system handed them, NUL-ended", () => {
	const raw = Buffer.from(["/home/u/.local/share/dev.fennecdb.studio/stable/app/bin/launcher", "/home/u/with space/$(x) \"q\" şirket.fennec", ""].join("\0"));
	assert.deepEqual(procArgv(raw), ["/home/u/.local/share/dev.fennecdb.studio/stable/app/bin/launcher", "/home/u/with space/$(x) \"q\" şirket.fennec"]);
	assert.deepEqual(procArgv(Buffer.from("/l\0\0/f\0")), ["/l", "", "/f"], "an empty argument stays one");
});

test("the files are the arguments after the launcher, its own flags aside", () => {
	assert.deepEqual(handedPaths(["/l", "/a/b.fennec", "/c d/e.fennec"], "linux", "/home/u"), ["/a/b.fennec", "/c d/e.fennec"]);
	assert.deepEqual(handedPaths(["/l", "--automation", "/a.fennec"], "linux", null), ["/a.fennec"], "WebKitWebDriver's flag is the launcher's");
	assert.deepEqual(handedPaths(["/l", "--uninstall"], "linux", null), [], "the launcher never starts the app with it");
	assert.deepEqual(handedPaths(["/l", ""], "linux", null), []);
	assert.deepEqual(handedPaths(["C:\\l.exe", "--automation"], "win32", null), ["--automation"], "a Windows launcher reads no such flag: it is a name, said in words as not found");
});

test("a relative path is taken from the launcher's folder where that is known, else as it is", () => {
	assert.deepEqual(handedPaths(["/l", "x.fennec", "../y.fennec"], "linux", "/home/u/d"), ["/home/u/d/x.fennec", "/home/u/y.fennec"]);
	assert.deepEqual(handedPaths(["C:\\l.exe", "x.fennec"], "win32", null), ["x.fennec"]);
});

test("the parent is this app's launcher only when it is the one beside this Bun", () => {
	assert.equal(isOurLauncher("/app/bin/launcher", "/app/bin/bun", "linux"), true);
	assert.equal(isOurLauncher("/other/bin/launcher", "/app/bin/bun", "linux"), false, "another copy's");
	assert.equal(isOurLauncher("/usr/bin/bash", "/app/bin/bun", "linux"), false, "a shell that ran the Bun");
	assert.equal(isOurLauncher("/app/bin/Launcher", "/app/bin/bun", "linux"), false, "Linux tells case apart");
	assert.equal(isOurLauncher(String.raw`C:\App\Bin\LAUNCHER.EXE`, String.raw`C:\app\bin\bun.exe`, "win32"), true, "Windows does not");
	assert.equal(isOurLauncher(String.raw`C:\Windows\explorer.exe`, String.raw`C:\app\bin\bun.exe`, "win32"), false);
});

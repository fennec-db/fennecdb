import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import {
	dataHome,
	desktopEntry,
	execArg,
	linuxFiles,
	mimePackage,
	progId,
	registerLinux,
	registerWindows,
	takesDefault,
	windowsValues,
	type Registry,
} from "../src/bun/associate.ts";

// What the app registers so a `.fennec` opens in it on Windows and Linux,
// written where it differs from what is there. The contents and the Linux
// writes are held here on every system; the Windows registry's calls, and the
// system reading it all back, by `checks/finder.mjs`.

const work = mkdtempSync(join(tmpdir(), "fennec-desktop-associate-"));
after(() => rmSync(work, { recursive: true, force: true }));

const WIN_APP = { identifier: "dev.fennecdb.studio", name: "fennec studio", launcher: String.raw`C:\Users\a b\AppData\Local\dev.fennecdb.studio\stable\app\bin\launcher.exe`, icon: String.raw`C:\Users\a b\AppData\Local\dev.fennecdb.studio\stable\app\Resources\app\icon\icon.ico` };

test("Windows: a ProgID of the app's identifier, opening the launcher with the file as one quoted argument", () => {
	const values = windowsValues(WIN_APP);
	const at = (key: string, name: string | null = null) => values.find((v) => v.key === key && v.name === name)?.data;
	assert.equal(progId("dev.fennecdb.studio"), "dev.fennecdb.studio.fennec");
	assert.equal(progId("dev.fennecdb.studio.test"), "dev.fennecdb.studio.test.fennec", "the test variant's is its own");
	assert.equal(at("Software\\Classes\\dev.fennecdb.studio.fennec"), "fennecdb database");
	assert.equal(at("Software\\Classes\\dev.fennecdb.studio.fennec\\DefaultIcon"), WIN_APP.icon);
	assert.equal(at("Software\\Classes\\dev.fennecdb.studio.fennec\\shell\\open\\command"), `"${WIN_APP.launcher}" "%1"`);
	assert.equal(at("Software\\Classes\\.fennec\\OpenWithProgids", "dev.fennecdb.studio.fennec"), "");
	assert.equal(values.length, 4, "the default is not among them: it is taken only where it is the app's to take");
});

test("Windows: a path the shell would substitute in, or a quote would end, is not registered", () => {
	assert.throws(() => windowsValues({ ...WIN_APP, launcher: String.raw`C:\100%1\launcher.exe` }), /holds a character/);
	assert.throws(() => windowsValues({ ...WIN_APP, icon: 'C:\\a"b\\icon.ico' }), /holds a character/);
	assert.throws(() => windowsValues({ ...WIN_APP, launcher: "C:\\a\nb\\launcher.exe" }), /holds a character/);
});

test("Windows: .fennec's default is taken where there is none, it is ours, or its program is gone; another's is left", () => {
	const ours = "dev.fennecdb.studio.fennec";
	const commands: Record<string, string> = {
		"other.app": String.raw`"C:\Program Files\Other\other.exe" "%1"`,
		"gone.app": String.raw`"C:\Users\a\Old\launcher.exe" "%1"`,
		"loose.app": String.raw`C:\Program Files\Loose\loose.exe %1`,
	};
	const exists = (p: string) => p === String.raw`C:\Program Files\Other\other.exe`;
	const commandOf = (p: string) => commands[p] ?? null;
	assert.equal(takesDefault(null, ours, commandOf, exists), true, "none");
	assert.equal(takesDefault(ours, ours, commandOf, exists), true, "ours");
	assert.equal(takesDefault("gone.app", ours, commandOf, exists), true, "a quoted program that is gone: a copy removed or moved");
	assert.equal(takesDefault("other.app", ours, commandOf, exists), false, "a program that is there");
	assert.equal(takesDefault("loose.app", ours, commandOf, exists), false, "a command this cannot read for sure");
	assert.equal(takesDefault("unknown.app", ours, commandOf, exists), false, "a ProgID with no command this can read");
	assert.equal(takesDefault("broken.app", ours, () => { throw new Error("error 1630"); }, exists), false, "a value of another type");
});

/** A registry in memory, as associate.ts asks one: HKCU's values, and HKCR's as the user's over `machine`. */
function memory(machine: Record<string, string> = {}) {
	const user = new Map<string, string>();
	const id = (key: string, name: string | null) => `${key}|${name ?? ""}`;
	let changed = 0;
	const reg: Registry = {
		read: (key, name) => user.get(id(key, name)) ?? null,
		readClass: (key, name) => user.get(id(`Software\\Classes\\${key}`, name)) ?? machine[id(key, name)] ?? null,
		write: (key, name, data) => void user.set(id(key, name), data),
		changed: () => void changed++,
	};
	return { reg, user, changes: () => changed };
}

test("Windows: a first start writes it all and tells Explorer; a second writes nothing", () => {
	const m = memory();
	const first = registerWindows(WIN_APP, m.reg, () => false);
	assert.equal(first.wrote.length, 5);
	assert.equal(m.user.get("Software\\Classes\\.fennec|"), "dev.fennecdb.studio.fennec");
	assert.equal(m.changes(), 1);
	const again = registerWindows(WIN_APP, m.reg, () => false);
	assert.deepEqual(again.wrote, []);
	assert.equal(m.changes(), 1, "nothing changed, nothing said");
	const moved = registerWindows({ ...WIN_APP, launcher: String.raw`D:\elsewhere\bin\launcher.exe` }, m.reg, () => false);
	assert.deepEqual(moved.wrote, ["HKCU\\Software\\Classes\\dev.fennecdb.studio.fennec\\shell\\open\\command"], "a copy moved: its command alone");
});

test("Windows: a .fennec another program has, the machine's included, is left, and the app is listed beside it", () => {
	const m = memory({ ".fennec|": "other.app", "other.app\\shell\\open\\command|": String.raw`"C:\Other\other.exe" "%1"` });
	const done = registerWindows(WIN_APP, m.reg, (p) => p === String.raw`C:\Other\other.exe`);
	assert.match(done.skipped ?? "", /opens with other\.app/);
	assert.equal(m.user.get("Software\\Classes\\.fennec|"), undefined);
	assert.equal(m.user.get("Software\\Classes\\.fennec\\OpenWithProgids|dev.fennecdb.studio.fennec"), "");
});

/**
 * A desktop entry's Exec, read back as the specification reads it: the
 * string's escapes first, then its quoting. Written apart from execArg, from
 * the specification's words.
 */
function readExec(value: string): string[] {
	let unescaped = "";
	for (let i = 0; i < value.length; i++) {
		if (value[i] !== "\\") {
			unescaped += value[i];
			continue;
		}
		const next = value[++i];
		const known: Record<string, string> = { s: " ", n: "\n", t: "\t", r: "\r", "\\": "\\" };
		if (next === undefined || !(next in known)) throw new Error(`an escape a key file refuses: \\${next ?? ""}`);
		unescaped += known[next];
	}
	const args: string[] = [];
	let i = 0;
	while (i < unescaped.length) {
		while (unescaped[i] === " ") i++;
		if (i >= unescaped.length) break;
		let arg = "";
		if (unescaped[i] === '"') {
			i++;
			while (i < unescaped.length && unescaped[i] !== '"') {
				if (unescaped[i] === "\\") {
					const c = unescaped[++i]!;
					if (!'"`$\\'.includes(c)) throw new Error(`\\${c} inside quotes`);
					arg += c;
				} else arg += unescaped[i];
				i++;
			}
			if (unescaped[i] !== '"') throw new Error("an unclosed quote");
			i++;
		} else {
			while (i < unescaped.length && unescaped[i] !== " ") arg += unescaped[i++];
		}
		args.push(arg.replace(/%%/g, "%"));
	}
	return args;
}

const LINUX_APP = { identifier: "dev.fennecdb.studio", name: "fennec studio", launcher: "/home/u/.local/share/dev.fennecdb.studio/stable/app/bin/launcher", icon: "/home/u/.local/share/dev.fennecdb.studio/stable/app/Resources/app/icon/icon.png" };

test("Linux: the entry opens the launcher with the file, hidden from the menus, ignored once the launcher is gone", () => {
	const entry = desktopEntry(LINUX_APP);
	const key = (k: string) => new RegExp(`^${k}=(.*)$`, "m").exec(entry)?.[1];
	assert.equal(entry.split("\n")[0], "[Desktop Entry]");
	assert.deepEqual(readExec(key("Exec")!), [LINUX_APP.launcher, "%f"]);
	assert.equal(key("TryExec"), LINUX_APP.launcher);
	assert.equal(key("Icon"), LINUX_APP.icon);
	assert.equal(key("MimeType"), "application/x-fennecdb;");
	assert.equal(key("NoDisplay"), "true");
	assert.equal(key("Type"), "Application");
});

test("Linux: a launcher in a folder a shell would read is one argument, read back whole", () => {
	for (const odd of ['/h/odd "q" `id` $(cd$IFS~;touch$IFS/m) \\b/app/bin/launcher', "/h/a'b/launcher", "/h/ü ş/launcher", "/h/\\\\/launcher"]) {
		assert.deepEqual(readExec(`${execArg(odd)} %f`), [odd, "%f"], odd);
	}
	assert.throws(() => execArg("/h/a\nb/launcher"), /control/);
	// GIO looks for the program before it expands `%%`: it would look for a folder that is not there.
	assert.throws(() => execArg("/h/50%f/launcher"), /GIO/);
	assert.match(desktopEntry({ ...LINUX_APP, icon: "/h/a\\b/icon.png" }), /^Icon=\/h\/a\\\\b\/icon\.png$/m, "a backslash is a string's escape");
});

test("Linux: the type names *.fennec", () => {
	const xml = mimePackage();
	assert.match(xml, /<mime-type type="application\/x-fennecdb">/);
	assert.match(xml, /<glob pattern="\*\.fennec"\/>/);
	assert.match(xml, /^<\?xml version="1\.0" encoding="UTF-8"\?>/);
});

test("Linux: the data home is $XDG_DATA_HOME where it is absolute, else ~/.local/share", () => {
	assert.equal(dataHome({ XDG_DATA_HOME: "/x/data" }, "/home/u"), "/x/data");
	assert.equal(dataHome({ XDG_DATA_HOME: "relative" }, "/home/u"), "/home/u/.local/share");
	assert.equal(dataHome({}, "/home/u"), "/home/u/.local/share");
});

test("Linux: a first start writes both files and makes the caches again; a second writes and runs nothing", async () => {
	const data = join(work, "data");
	const ran: string[][] = [];
	const run = async (argv: string[]) => {
		ran.push(argv);
		return argv[0] === "update-desktop-database" ? "not found" : "exit 0";
	};
	const first = await registerLinux(LINUX_APP, data, run);
	const files = linuxFiles(data, "dev.fennecdb.studio");
	assert.deepEqual(first.wrote.sort(), [files.desktop, files.mime].sort());
	assert.deepEqual(ran, [
		["update-mime-database", join(data, "mime")],
		["update-desktop-database", join(data, "applications")],
	]);
	assert.deepEqual(first.ran.map((r) => r.result), ["exit 0", "not found"], "a tool not installed is said, and is no failure");
	assert.equal(readFileSync(files.desktop, "utf8"), desktopEntry(LINUX_APP));
	assert.equal(readFileSync(files.mime, "utf8"), mimePackage());
	ran.length = 0;
	const again = await registerLinux(LINUX_APP, data, run);
	assert.deepEqual(again.wrote, []);
	assert.deepEqual(ran, []);
	const moved = await registerLinux({ ...LINUX_APP, launcher: "/elsewhere/bin/launcher" }, data, run);
	assert.deepEqual(moved.wrote, [files.desktop], "a copy moved: its entry alone");
	assert.deepEqual(ran, [["update-desktop-database", join(data, "applications")]]);
});

// A `.fennec` file opens in the app on Windows and Linux: the registration
// that has Explorer or a file manager start the app's launcher with the file
// (handover.ts reads it back). macOS's is Electrobun's, Info.plist's document
// type from `app.fileAssociations`; Electrobun 2.0.4 writes none elsewhere.
//
// The app writes it itself, for its user alone, every time it starts, and only
// what differs from what is there: so it holds for a copy unpacked anywhere, a
// copy moved (its next start points the registration at where it now is) and
// Electrobun's installer alike, none of which runs anything of the app's when
// it places it. An installer of our own (install.ps1 -App, install.sh --app)
// would cover its own copies alone.
//
//   - Windows, under HKCU\Software\Classes: a ProgID `<identifier>.fennec`
//     (its name, its DefaultIcon, and `shell\open\command` = `"<launcher>"
//     "%1"`), listed in `.fennec\OpenWithProgids`, and made `.fennec`'s default
//     only where that is unset, already this app's, or a ProgID whose program
//     is gone. A program the user picked ("Always use this app") is Explorer's
//     own record, UserChoice, which wins over any default and is never
//     touched. A REG_SZ, so nothing in it is expanded but the shell's own `%1`:
//     a launcher path holding a `%` is not registered.
//   - Linux, under $XDG_DATA_HOME (~/.local/share): the shared-mime-info type
//     `application/x-fennecdb` for `*.fennec` (`mime/packages/<identifier>.xml`)
//     and a desktop entry for it (`applications/<identifier>.desktop`:
//     `MimeType=`, `Exec="<launcher>" %f`, `NoDisplay=true`, since Electrobun's
//     own entry is the one in the menus, and `TryExec=<launcher>`, so an entry
//     whose launcher was removed or moved is ignored, not left pointing nowhere),
//     then `update-mime-database` and `update-desktop-database` where there are
//     any, each started with an argument vector. Nothing is made the type's
//     default: a program the user picked (mimeapps.list) stays theirs.
//
// What is left when the app goes is said in the README ("Uninstall"):
// Electrobun's uninstaller knows nothing of it. Pure but for the writes, which
// are handed in, so `node --test` holds the contents and the Linux writes
// (test/associate.test.ts); the Windows writes are win32.ts's, held by
// `checks/finder.mjs`.

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, posix } from "node:path";
import { windowsArgv } from "./handover.ts";

/** The file type, as each system names it. */
export const EXTENSION = ".fennec";
export const MIME_TYPE = "application/x-fennecdb";
export const TYPE_NAME = "fennecdb database";

/** What a registration names: the app's identifier and name, its launcher and its icon, full paths. */
export type Registrant = { identifier: string; name: string; launcher: string; icon: string };

/** Windows' ProgID for the app's `.fennec` files: the test variant's is its own. */
export const progId = (identifier: string) => `${identifier}.fennec`;

const CLASSES = "Software\\Classes";
// A control character in a path is nothing a registration can carry as it is.
// eslint-disable-next-line no-control-regex
const CONTROL = /[\u0000-\u001f\u007f]/;

/** One REG_SZ under HKCU: its key, its name (null: the key's default value) and its data. */
export type RegValue = { key: string; name: string | null; data: string };

/** The values the app's ProgID is, under HKCU (`.fennec`'s default aside: `takesDefault`). Throws for a path Windows' `%1` substitution or a quote would misread. */
export function windowsValues(r: Registrant): RegValue[] {
	for (const p of [r.launcher, r.icon]) {
		if (/[%"]/.test(p) || CONTROL.test(p)) throw new Error(`not registered: ${p} holds a character the registration cannot carry (%, " or a control)`);
	}
	const prog = progId(r.identifier);
	return [
		{ key: `${CLASSES}\\${prog}`, name: null, data: TYPE_NAME },
		{ key: `${CLASSES}\\${prog}\\DefaultIcon`, name: null, data: r.icon },
		{ key: `${CLASSES}\\${prog}\\shell\\open\\command`, name: null, data: `"${r.launcher}" "%1"` },
		{ key: `${CLASSES}\\${EXTENSION}\\OpenWithProgids`, name: prog, data: "" },
	];
}

/** Where `.fennec`'s default ProgID is kept, for the user. */
export const DEFAULT_VALUE: RegValue = { key: `${CLASSES}\\${EXTENSION}`, name: null, data: "" };

/**
 * Whether `.fennec`'s default is the app's to take, read as Explorer reads it
 * (the user's classes over the machine's): there is none, it is the app's own
 * already, or it names a ProgID whose open command is a quoted program that is
 * gone (a copy removed, a folder moved: what the app itself writes). Any other
 * -- a program that is there, a command this cannot read -- is left.
 */
export function takesDefault(current: string | null, ours: string, commandOf: (prog: string) => string | null, exists: (path: string) => boolean): boolean {
	if (!current || current === ours) return true;
	let command: string | null = null;
	try {
		command = commandOf(current);
	} catch {
		return false;
	}
	if (!command || !command.startsWith('"')) return false;
	const program = windowsArgv(command)[0] ?? "";
	return program !== "" && !exists(program);
}

/** A desktop entry's Exec argument for `path`: quoted, with what its quoting and its string escapes read escaped. */
export function execArg(path: string): string {
	if (CONTROL.test(path)) throw new Error(`not registered: ${path} holds a control character`);
	// GIO checks the program before it expands field codes, so it looks for `%%` where the
	// file has `%` and ignores the entry: a launcher path with a `%` is not registered.
	if (path.includes("%")) throw new Error(`not registered: ${path} holds a %, which GIO would look for as %%`);
	// Inside quotes `"`, `` ` ``, `$` and `\` take a backslash; then the value is a
	// string, whose own escape doubles every backslash.
	const quoted = `"${path.replace(/["`$\\]/g, (c) => `\\${c}`)}"`;
	return quoted.replace(/\\/g, "\\\\");
}

/** A desktop entry's string value (Icon=, TryExec=): only its escapes. */
const stringValue = (s: string) => {
	if (CONTROL.test(s)) throw new Error(`not registered: ${s} holds a control character`);
	return s.replace(/\\/g, "\\\\");
};

/** The desktop entry the type opens with: hidden from the menus, ignored once its launcher is gone. */
export function desktopEntry(r: Registrant): string {
	return [
		"[Desktop Entry]",
		"Type=Application",
		`Name=${stringValue(r.name)}`,
		`Comment=Open a ${TYPE_NAME} in ${stringValue(r.name)}`,
		`Exec=${execArg(r.launcher)} %f`,
		`TryExec=${stringValue(r.launcher)}`,
		`Icon=${stringValue(r.icon)}`,
		`MimeType=${MIME_TYPE};`,
		"NoDisplay=true",
		"Terminal=false",
		"",
	].join("\n");
}

/** The shared-mime-info package that names `*.fennec`. */
export function mimePackage(): string {
	return [
		'<?xml version="1.0" encoding="UTF-8"?>',
		'<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">',
		`  <mime-type type="${MIME_TYPE}">`,
		`    <comment>${TYPE_NAME}</comment>`,
		'    <glob pattern="*.fennec"/>',
		"  </mime-type>",
		"</mime-info>",
		"",
	].join("\n");
}

/** Linux: where the data home is, by the XDG rule (an absolute $XDG_DATA_HOME, else ~/.local/share). */
export const dataHome = (env: Record<string, string | undefined>, home: string) => {
	const set = env["XDG_DATA_HOME"];
	return set && posix.isAbsolute(set) ? set : posix.join(home, ".local", "share");
};

/** Linux: the files the registration is, in `data`. */
export const linuxFiles = (data: string, identifier: string) => ({
	desktop: join(data, "applications", `${identifier}.desktop`),
	mime: join(data, "mime", "packages", `${identifier}.xml`),
});

/** What a registration did: the files or values it wrote, and the programs it ran with how each ended. */
export type Registered = { wrote: string[]; ran: { argv: string[]; result: string }[]; skipped?: string };

/** Starts `argv` with no shell and says how it ended: its code, or that it is not there. */
export function runTool(argv: string[], start = spawn): Promise<string> {
	return new Promise((resolve) => {
		const child = start(argv[0]!, argv.slice(1), { stdio: "ignore", shell: false });
		let said = false;
		child.once("error", (e: NodeJS.ErrnoException) => {
			if (said) return;
			said = true;
			resolve(e.code === "ENOENT" ? "not found" : `could not start: ${e.message}`);
		});
		child.once("exit", (code, signal) => {
			if (said) return;
			said = true;
			resolve(code === null ? `ended by ${signal}` : `exit ${code}`);
		});
	});
}

/** Writes `text` to `path` where it differs, by a rename, so a reader never sees half of it. Says whether it wrote. */
function writeIfChanged(path: string, text: string): boolean {
	try {
		if (readFileSync(path, "utf8") === text) return false;
	} catch {
		// Not there yet.
	}
	mkdirSync(dirname(path), { recursive: true });
	const temp = `${path}.${process.pid}.tmp`;
	writeFileSync(temp, text);
	renameSync(temp, path);
	return true;
}

/** Linux: the type and the entry written where they differ, and the system's caches made again for what was. */
export async function registerLinux(r: Registrant, data: string, run = runTool): Promise<Registered> {
	const files = linuxFiles(data, r.identifier);
	const entry = desktopEntry(r);
	const wrote: string[] = [];
	if (writeIfChanged(files.mime, mimePackage())) wrote.push(files.mime);
	if (writeIfChanged(files.desktop, entry)) wrote.push(files.desktop);
	const ran: Registered["ran"] = [];
	if (wrote.includes(files.mime)) {
		const argv = ["update-mime-database", join(data, "mime")];
		ran.push({ argv, result: await run(argv) });
	}
	if (wrote.includes(files.desktop)) {
		const argv = ["update-desktop-database", join(data, "applications")];
		ran.push({ argv, result: await run(argv) });
	}
	return { wrote, ran };
}

/** Windows' registry, as associate.ts asks it (win32.ts's calls, or a stand-in). */
export type Registry = {
	/** A REG_SZ of HKCU, or null. */
	read(key: string, name: string | null): string | null;
	/** A REG_SZ of HKCR, the classes Explorer reads (the user's over the machine's), or null. */
	readClass(key: string, name: string | null): string | null;
	write(key: string, name: string | null, data: string): void;
	changed(): void;
};

/** Windows: the ProgID and `.fennec`'s list written where they differ, `.fennec`'s default where it is the app's to take. */
export function registerWindows(r: Registrant, reg: Registry, exists: (path: string) => boolean): Registered {
	const prog = progId(r.identifier);
	const wrote: string[] = [];
	const put = (v: RegValue) => {
		if (reg.read(v.key, v.name) === v.data) return;
		reg.write(v.key, v.name, v.data);
		wrote.push(`HKCU\\${v.key}${v.name === null ? "" : `\\${v.name}`}`);
	};
	for (const v of windowsValues(r)) put(v);
	let skipped: string | undefined;
	let current: string | null = null;
	try {
		current = reg.readClass(EXTENSION, null);
	} catch (e) {
		skipped = `${EXTENSION}'s default could not be read (${(e as Error).message}), and is left as it is`;
	}
	if (!skipped) {
		if (takesDefault(current, prog, (p) => reg.readClass(`${p}\\shell\\open\\command`, null), exists)) put({ ...DEFAULT_VALUE, data: prog });
		else skipped = `${EXTENSION} opens with ${current}, which is left as it is`;
	}
	if (wrote.length) reg.changed();
	return { wrote, ran: [], ...(skipped ? { skipped } : {}) };
}

/**
 * The app's own registration, made where it differs (index.ts, at every start
 * of the one instance): its launcher beside this process's Bun, its icon
 * among its resources. Nothing on macOS, where Info.plist is it.
 */
export async function registerSelf(identifier: string, name: string): Promise<Registered> {
	if (process.platform !== "win32" && process.platform !== "linux") return { wrote: [], ran: [], skipped: "macOS takes the document type from Info.plist" };
	const { PATHS } = await import("electrobun/main");
	const bin = dirname(process.execPath);
	// The icon Hutch puts in the app from build.linux.icon or build.win.icon (electrobun.config.ts):
	// the one Electrobun's own menu entry and Start-menu shortcut show.
	if (process.platform === "linux") {
		return registerLinux({ identifier, name, launcher: join(bin, "launcher"), icon: join(PATHS.RESOURCES_FOLDER, "appIcon.png") }, dataHome(process.env, homedir()));
	}
	const w = await import("./win32.ts");
	const reg: Registry = {
		read: (key, value) => w.readString(key, value),
		readClass: (key, value) => w.readString(key, value, true),
		write: w.writeUserString,
		changed: w.associationsChanged,
	};
	return registerWindows({ identifier, name, launcher: join(bin, "launcher.exe"), icon: join(PATHS.RESOURCES_FOLDER, "app.ico") }, reg, existsSync);
}

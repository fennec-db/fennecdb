// Backups, through the bundled `fennec` shell and nothing of the app's own:
//
//   fennec backup <server> <archive-folder> [--key-file k]  an image of the database, taken while it runs
//   fennec verify <archive-folder> [--key-file k]            what the folder can restore, read as a restore would
//   fennec restore <archive-folder> <new.fennec> [--to-change n] [--key-file k]
//   fennec key <key-file>                                    a new key, readable by its owner alone
//
// A backup speaks the server's replication stream, so it is taken from a
// server: the one serving the file, the one behind its window, or one started
// for the backup alone (app.ts). Its replication token goes in the
// environment, as the server's own did (env.ts). The archive folder is
// `fennec archive`'s layout, so `fennec verify`, `restore` and `prune` read it as
// they read one that command keeps.
//
// **A restore never writes over a file.** `fennec restore` writes its output
// whether or not a file is there -- a database another program serves
// included -- so it writes into a folder of the app's own, made for that one
// restore beside the new file (`mkdtemp`), and the result is put in place with
// `link`, which fails if the name is taken: no check-then-write window. A
// folder and not a name of its own: the shell writes beside its output too
// (`<out>.restoring`, `<out>.compacting`, the lock), and a unique name it
// replaced the end of was shared by two restores to one name, which broke each
// other. Removing the folder takes all of it. Pure but for the file system and
// the runner it is handed (Bun's in the app, Node's in test/backups.test.ts,
// which drives the real binaries).

import { spawnSync } from "node:child_process";
import { closeSync, constants, copyFileSync, existsSync, fsyncSync, linkSync, mkdirSync, mkdtempSync, openSync, readdirSync, readFileSync, realpathSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, isAbsolute, join, relative } from "node:path";
import { childEnvironment } from "./env.ts";
import { kindOfFile } from "./dbfile.ts";

/** What a run of `fennec` said. `code` null: killed. */
export type Ran = { code: number | null; out: string; err: string };
/** Runs the bundled `fennec` with `args` in environment `env`. */
export type Run = (args: string[], env: Record<string, string>) => Promise<Ran>;

export type Archive = { dir: string; keyFile: string | null };

/** An image a backup put in an archive folder: `image-<change, 20 digits>-<ms>.fennec`. */
export type Image = { change: number; at: number; file: string };

const IMAGE = /^image-(\d{20})-(\d+)\.fennec$/;

/** The images among a folder's names, newest first. */
export function imagesIn(names: string[]): Image[] {
	return names
		.map((n) => IMAGE.exec(n))
		.filter((m): m is RegExpExecArray => m !== null)
		.map((m) => ({ change: Number(m[1]), at: Number(m[2]), file: m[0] }))
		.sort((a, b) => b.at - a.at || b.change - a.change);
}

/** The images in an archive folder, newest first; none if it is not there. */
export function listImages(dir: string): Image[] {
	try {
		return imagesIn(readdirSync(dir));
	} catch {
		return [];
	}
}

const keyArgs = (a: Archive) => (a.keyFile ? ["--key-file", a.keyFile] : []);
/** What `fennec` said last, for a message: its error lines, without the command's name before them. */
export const said = (r: Ran) => (r.err.trim() || r.out.trim()).split("\n").slice(-4).join("\n").replace(/^fennec \w+: /gm, "");

export type Taken = { ok: true; change: number } | { ok: false; message: string };

/**
 * An image of the database `server` serves, into `archive`. The folder is made
 * if it is not there: given a name that is not a folder, `fennec backup` writes a
 * backup *file* by that name instead.
 */
export async function takeBackup(run: Run, server: { local: string; replicationToken: string }, archive: Archive, env: Record<string, string | undefined> = process.env): Promise<Taken> {
	try {
		mkdirSync(archive.dir, { recursive: true });
	} catch (e) {
		return { ok: false, message: `The folder ${archive.dir} could not be made: ${(e as Error).message}` };
	}
	const r = await run(["backup", server.local, archive.dir, ...keyArgs(archive)], childEnvironment(env, { FENNEC_REPLICATION_TOKEN: server.replicationToken }));
	const m = /the database at change (\d+)/.exec(r.out);
	if (r.code !== 0 || !m) return { ok: false, message: said(r) || `fennec backup ended with ${r.code}` };
	return { ok: true, change: Number(m[1]) };
}

export type Verified = { ok: boolean; text: string };

/** `fennec verify`: what the folder can restore, or why it cannot. */
export async function verifyArchive(run: Run, archive: Archive, env: Record<string, string | undefined> = process.env): Promise<Verified> {
	const r = await run(["verify", archive.dir, ...keyArgs(archive)], childEnvironment(env, {}));
	return { ok: r.code === 0, text: r.code === 0 ? r.out.trim() : said(r) };
}

export type Restored = { ok: true; path: string; change: number } | { ok: false; message: string };

/**
 * The archive as it stood at image `change` (the newest when null), as a new
 * file at `dest`, which must not exist: if it does, nothing is written there.
 */
export async function restoreArchive(run: Run, archive: Archive, change: number | null, dest: string, env: Record<string, string | undefined> = process.env): Promise<Restored> {
	const name = basename(dest);
	const taken = (meanwhile: boolean): Restored => ({
		ok: false,
		message: `${name} ${meanwhile ? "appeared in that folder meanwhile" : "exists in that folder already"}. Choose another name: a restore never writes over a file.`,
	});
	if (existsSync(dest)) return taken(false);
	// A folder of this restore's own, beside the new file (so a link reaches it):
	// whatever the shell writes, at its output and beside it, is in there.
	let scratch: string;
	try {
		scratch = mkdtempSync(join(dirname(dest), `.${name}.restoring-`));
	} catch (e) {
		return { ok: false, message: (e as Error).message };
	}
	const out = join(scratch, "db.fennec");
	try {
		const to = change === null ? [] : ["--to-change", String(change)];
		const r = await run(["restore", archive.dir, out, ...to, ...keyArgs(archive)], childEnvironment(env, {}));
		const m = /change (\d+)/.exec(r.out);
		if (r.code !== 0 || !m) return { ok: false, message: said(r) || `fennec restore ended with ${r.code}` };
		if (kindOfFile(out) !== "database") return { ok: false, message: "The restore did not make a database file." };
		try {
			// Fails with EEXIST if the name was taken meanwhile: never over a file.
			linkSync(out, dest);
		} catch (e) {
			if ((e as NodeJS.ErrnoException).code === "EEXIST") return taken(true);
			// A disk without hard links (exFAT, FAT32): a copy, which fails as well
			// if the name is taken, and is on the disk before it is said to be done.
			try {
				copyFileSync(out, dest, constants.COPYFILE_EXCL);
			} catch (c) {
				if ((c as NodeJS.ErrnoException).code === "EEXIST") return taken(true);
				throw c;
			}
			const fd = openSync(dest, "r");
			try {
				fsyncSync(fd);
			} finally {
				closeSync(fd);
			}
		}
		return { ok: true, path: dest, change: Number(m[1]) };
	} catch (e) {
		return { ok: false, message: (e as Error).message };
	} finally {
		rmSync(scratch, { recursive: true, force: true });
	}
}

/** `path` resolved through the links of its longest part that exists: a folder not made yet is where it will be. */
function resolved(path: string): string {
	const rest: string[] = [];
	let at = path;
	for (;;) {
		try {
			return join(realpathSync(at), ...rest);
		} catch {
			const up = dirname(at);
			if (up === at) return path;
			rest.unshift(basename(at));
			at = up;
		}
	}
}

/** The device of `path`, or of the longest part of it that exists. */
function deviceOf(path: string): number {
	for (let at = path; ; at = dirname(at)) {
		const s = statSync(at, { throwIfNoEntry: false });
		if (s) return s.dev;
		if (dirname(at) === at) return -1;
	}
}

/** Is `path` `folder` or under it? By the system's own paths: `\` and case-blind on Windows. */
const within = (path: string, folder: string) => {
	const rel = relative(folder, path);
	return rel === "" || (!rel.startsWith("..") && !isAbsolute(rel));
};

/**
 * Who besides its owner may read the key at `path`, or null: what a key's
 * warning names. A Unix says it by the mode (`fennec key` writes 0600). A
 * Windows file's mode is nothing (every one reads 0666); who may read it is
 * its DACL, which `fennec key` makes its owner's alone, and a key copied in
 * inherits its folder's. That is read as the server reads a TLS key's
 * (fennec-http's `owner_only::open_to`): open where an entry lets one of the
 * groups that hold every user read it.
 */
export function othersMayRead(path: string, platform: NodeJS.Platform = process.platform): string | null {
	if (platform !== "win32") {
		const mode = statSync(path, { throwIfNoEntry: false })?.mode ?? 0;
		return mode & 0o077 ? `its permissions are ${(mode & 0o777).toString(8)}` : null;
	}
	const who = openTo(daclOf(path));
	return who ? `${who} may, by its permissions` : null;
}

/**
 * The file's DACL as SDDL (`D:PAI(A;;FA;;;S-1-5-...)...`), from `icacls
 * <file> /save`: no language's names in it, the SIDs as they are. Null where
 * it cannot be read, which no warning is said for.
 */
function daclOf(path: string): string | null {
	const dir = mkdtempSync(join(tmpdir(), "fennec-acl-"));
	try {
		const out = join(dir, "acl");
		const r = spawnSync("icacls", [path, "/save", out, "/Q"], { stdio: "ignore", timeout: 10_000, windowsHide: true });
		if (r.status !== 0) return null;
		// UTF-16LE, a line of the file's name and then its DACL.
		const text = readFileSync(out).toString("utf16le").replace(/^﻿/, "");
		return text.split(/\r?\n/).find((l) => l.startsWith("D:")) ?? null;
	} catch {
		return null;
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
}

/** The groups that hold every user of a machine, by SID and by SDDL's own name for each, widest first (owner_only.rs's OPEN). */
const OPEN: [string[], string][] = [
	[["S-1-1-0", "WD"], "every user (Everyone)"],
	[["S-1-5-11", "AU"], "every signed-in user (Authenticated Users)"],
	[["S-1-5-32-545", "BU"], "the machine's users (Users)"],
];
/** SDDL's names for access rights, as masks. */
const RIGHTS: Record<string, number> = {
	GA: 0x10000000, GR: 0x80000000, GW: 0x40000000, GX: 0x20000000,
	FA: 0x1f01ff, FR: 0x120089, FW: 0x120116, FX: 0x1200a0,
	RC: 0x20000, SD: 0x10000, WD: 0x40000, WO: 0x80000,
	CC: 0x1, DC: 0x2, LC: 0x4, SW: 0x8, RP: 0x10, WP: 0x20, DT: 0x40, LO: 0x80, CR: 0x100,
	KA: 0xf003f, KR: 0x20019, KW: 0x20006, KX: 0x20019,
};
/** What lets an entry's holder read a file's bytes: FILE_READ_DATA, and the generic rights that map onto it. */
const READS = 0x1 | 0x80000000 | 0x10000000;

/**
 * Which of the every-user groups `dacl` (SDDL) lets read the file, widest
 * first, or null: an allowing entry that grants a read, unless a denying one
 * before it denied it (Everyone's denial is every group's); an entry that only
 * passes to what a folder holds counts for nothing. No DACL at all
 * (`D:NO_ACCESS_CONTROL`) is every user's. Pure: test/backups.test.ts holds it.
 */
export function openTo(dacl: string | null): string | null {
	if (!dacl) return null;
	if (/^D:[A-Z_]*NO_ACCESS_CONTROL/.test(dacl)) return OPEN[0]![1];
	const denied = OPEN.map(() => false);
	const open = OPEN.map(() => false);
	for (const m of dacl.matchAll(/\(([^)]*)\)/g)) {
		const [kind = "", flags = "", rights = "", , , sid = ""] = m[1]!.split(";");
		if (flags.includes("IO")) continue;
		const mask = rights.startsWith("0x") ? Number.parseInt(rights, 16) : (rights.match(/../g) ?? []).reduce((a, r) => a | (RIGHTS[r] ?? 0), 0);
		if (!(mask & READS)) continue;
		const i = OPEN.findIndex(([names]) => names.includes(sid));
		if (i < 0) continue;
		if (kind === "D") {
			if (i === 0) denied.fill(true);
			else denied[i] = true;
		} else if (kind === "A" && !denied[i]) open[i] = true;
	}
	const at = open.indexOf(true);
	return at < 0 ? null : OPEN[at]![1];
}

/**
 * Whether `keyFile` is kept apart from the backups in `dir` of the database at
 * `database`: `refuse` says why it is not, `warn` what to look at.
 *
 * Refused **anywhere under the folder that was picked**: the backups' folder,
 * or, where it is named after the database as the app names it (`<picked>/<stem>`),
 * the folder above it, which is the one the user chose and may copy or sync
 * whole. Compared as real paths, through links. Refused as well **on the disk
 * the backups are on when that is not the database's**: backups on a drive of
 * their own are there to be carried off, and the key must not go with them;
 * beside the live database, a key on the same disk shows nothing the database
 * itself does not. A key others may read is a warning, not a refusal: the
 * shell's `fennec key` makes it its owner's alone, and one copied in may have
 * lost that (`othersMayRead`, by the system's own permissions).
 */
export function keyPlacement(
	dir: string,
	keyFile: string,
	database: string,
	dev: (path: string) => number = deviceOf,
	readers: (path: string) => string | null = othersMayRead,
): { refuse?: string; warn?: string } {
	const key = resolved(keyFile);
	const folder = resolved(dir);
	const stem = basename(database).replace(/\.fennec$/i, "");
	const picked = basename(folder) === stem ? dirname(folder) : folder;
	if (within(key, picked)) return { refuse: `The key is inside ${picked}, with the backups: anyone who has that folder could read them. Keep it in another folder.` };
	const backupsDisk = dev(folder);
	if (backupsDisk !== dev(database) && dev(key) === backupsDisk) {
		return { refuse: "The key is on the disk the backups go to, which is not the database's: whoever takes that disk has both. Keep the key on another disk." };
	}
	const others = readers(key);
	if (others) {
		const fix = process.platform === "win32" ? "make it yours alone, as fennec key makes one" : "chmod 600 it, as fennec key makes one";
		return { warn: `Others on this computer may read the key (${others}): ${fix}.` };
	}
	return {};
}

/** `fennec key`: a new key at `path`, which must not exist (the shell refuses to write over a key). */
export async function makeKey(run: Run, path: string, env: Record<string, string | undefined> = process.env): Promise<{ ok: boolean; message: string }> {
	const r = await run(["key", path], childEnvironment(env, {}));
	return { ok: r.code === 0, message: r.code === 0 ? `A new key is in ${path}. Keep it apart from the backups: without it nothing can be restored.` : said(r) };
}

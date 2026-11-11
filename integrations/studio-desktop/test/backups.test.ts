import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { imagesIn, keyPlacement, listImages, makeKey, openTo, othersMayRead, restoreArchive, takeBackup, verifyArchive } from "../src/bun/backups.ts";
import { randomToken, startServer, type FileServer } from "../src/bun/process.ts";
import { taskPlan, windowPlan } from "../src/bun/serving.ts";
import { missing, nodeRun, nodeSpawn, SERVER } from "./node-spawn.ts";
import { ownerAlone, WINDOWS } from "./platform.ts";

// Backups through the real binaries and the app's own code: `fennec backup`
// from a server started as the app starts one, into an archive folder;
// `fennec verify`; and `fennec restore` into a new file, which is never one that
// is there already -- a file a server serves least of all.

const skip = missing() || false;
const work = mkdtempSync(join(tmpdir(), "fennec-desktop-backups-"));
const running: FileServer[] = [];
after(async () => {
	await Promise.all(running.map((s) => s.stop()));
	rmSync(work, { recursive: true, force: true });
});

async function serve(name: string, rows: number): Promise<{ file: string; server: FileServer }> {
	const file = join(work, `${name}.fennec`);
	writeFileSync(file, "");
	// Its backups set up: a window's server serves the stream only then.
	const server = await startServer(nodeSpawn, SERVER, windowPlan(file, "250", true), randomToken());
	running.push(server);
	const q = async (query: string) => {
		const r = await fetch(`${server.local}/query`, { method: "POST", headers: { authorization: `Bearer ${server.token}` }, body: JSON.stringify({ query }) });
		assert.equal(r.status, 200, await r.clone().text());
	};
	await q("create collection notes (title text)");
	for (let i = 0; i < rows; i++) await q(`put notes {title: "n${i}"}`);
	return { file, server };
}

/** How many notes a database file holds, asked of a server started on it. */
async function count(file: string): Promise<number> {
	const s = await startServer(nodeSpawn, SERVER, taskPlan(file), randomToken());
	try {
		const r = await fetch(`${s.local}/query`, { method: "POST", headers: { authorization: `Bearer ${s.token}` }, body: JSON.stringify({ query: "get notes count" }) });
		const body = (await r.json()) as unknown;
		return Number(JSON.stringify(body).match(/\d+/)?.[0] ?? NaN);
	} finally {
		await s.stop();
	}
}

const noScratch = (dir: string) => assert.deepEqual(readdirSync(dir).filter((n) => n.includes(".restoring-")), [], "no scratch file left behind");

test("a backup of a running server, verified, restored into a new file that holds what it held", { skip }, async () => {
	const { server } = await serve("orders", 3);
	const archive = { dir: join(work, "backups", "orders"), keyFile: null };
	const taken = await takeBackup(nodeRun, { local: server.local, replicationToken: server.replicationToken! }, archive);
	assert.ok(taken.ok, JSON.stringify(taken));
	assert.ok(existsSync(archive.dir), "the folder is made");
	const r = await fetch(`${server.local}/query`, { method: "POST", headers: { authorization: `Bearer ${server.token}` }, body: JSON.stringify({ query: 'put notes {title: "later"}' }) });
	assert.equal(r.status, 200);
	const second = await takeBackup(nodeRun, { local: server.local, replicationToken: server.replicationToken! }, archive);
	assert.ok(second.ok && taken.ok && second.change > taken.change, "a later image, at a later change");
	const images = listImages(archive.dir);
	assert.equal(images.length, 2);
	assert.equal(images[0]!.change, second.ok ? second.change : -1, "newest first");

	const v = await verifyArchive(nodeRun, archive);
	assert.ok(v.ok, v.text);
	assert.match(v.text, /2 images/);

	const latest = join(work, "restored.fennec");
	const back = await restoreArchive(nodeRun, archive, null, latest);
	assert.ok(back.ok, JSON.stringify(back));
	assert.equal(await count(latest), 4);
	const earlier = join(work, "earlier.fennec");
	assert.ok((await restoreArchive(nodeRun, archive, taken.ok ? taken.change : 0, earlier)).ok);
	assert.equal(await count(earlier), 3, "as it stood at the first image");
	noScratch(work);
});

test("a restore never writes over a file: not one that is there, not one a server serves", { skip }, async () => {
	const { file, server } = await serve("live", 2);
	const archive = { dir: join(work, "backups", "live"), keyFile: null };
	assert.ok((await takeBackup(nodeRun, { local: server.local, replicationToken: server.replicationToken! }, archive)).ok);
	const before = readFileSync(file);
	const over = await restoreArchive(nodeRun, archive, null, file);
	assert.equal(over.ok, false);
	assert.match(over.ok ? "" : over.message, /exists in that folder already/);
	assert.deepEqual(readFileSync(file), before, "the served file is as it was");
	const mine = join(work, "notes.txt");
	writeFileSync(mine, "mine");
	assert.equal((await restoreArchive(nodeRun, archive, null, mine)).ok, false);
	assert.equal(readFileSync(mine, "utf8"), "mine");
	noScratch(work);
});

test("two restores to one name at once: one makes the file, the other is refused, and neither breaks it", { skip }, async () => {
	const { server } = await serve("twice", 5);
	const archive = { dir: join(work, "backups", "twice"), keyFile: null };
	assert.ok((await takeBackup(nodeRun, { local: server.local, replicationToken: server.replicationToken! }, archive)).ok);
	const dest = join(work, "twice-restored.fennec");
	// The shell writes beside its output (`<out>.restoring`, `<out>.compacting`, its lock): two restores
	// given one name's scratch broke each other; each now writes in a folder of its own.
	const both = await Promise.all([restoreArchive(nodeRun, archive, null, dest), restoreArchive(nodeRun, archive, null, dest), restoreArchive(nodeRun, archive, null, dest)]);
	assert.equal(both.filter((r) => r.ok).length, 1, JSON.stringify(both));
	for (const r of both.filter((r) => !r.ok)) assert.match(r.ok ? "" : r.message, /appeared in that folder meanwhile|exists in that folder already/);
	assert.equal(await count(dest), 5, "the one file made is whole");
	noScratch(work);
});

/**
 * Opens a file to every user of the machine: mode 0644 on a Unix; on Windows,
 * where a mode is nothing, an entry letting BUILTIN\Users read it, by its SID
 * (no language's group name).
 */
function openToOthers(p: string) {
	if (!WINDOWS) return chmodSync(p, 0o644);
	execFileSync("icacls", [p, "/grant", "*S-1-5-32-545:R"], { stdio: "ignore" });
}

test("a key is kept apart from the backups: not under the folder picked, not on the backups' own disk", () => {
	// Real paths (the temporary folder is under a link on macOS), as the app's are.
	const at = realpathSync(mkdtempSync(join(work, "keys-")));
	const picked = join(at, "Backups");
	const dir = join(picked, "orders");
	const db = join(at, "data", "orders.fennec");
	mkdirSync(dir, { recursive: true });
	mkdirSync(join(at, "data"));
	mkdirSync(join(at, "elsewhere"));
	// Its owner's alone: 0600, or on Windows what a file in the user's own temporary folder inherits.
	const key = (p: string, open = false) => {
		writeFileSync(p, "k");
		if (!WINDOWS) chmodSync(p, 0o600);
		if (open) openToOthers(p);
		return p;
	};
	const same = () => 1;
	// In the backups' folder, or anywhere under the folder that was picked: copied or synced with them.
	assert.match(keyPlacement(dir, key(join(dir, "in.key")), db, same).refuse ?? "", /inside/);
	assert.match(keyPlacement(dir, key(join(picked, "beside.key")), db, same).refuse ?? "", /inside/);
	mkdirSync(join(picked, "keys"));
	assert.match(keyPlacement(dir, key(join(picked, "keys", "deeper.key")), db, same).refuse ?? "", /inside/);
	// Through a link, it is where it really is.
	symlinkSync(picked, join(at, "link-to-backups"));
	assert.match(keyPlacement(dir, join(at, "link-to-backups", "beside.key"), db, same).refuse ?? "", /inside/);
	assert.match(keyPlacement(join(at, "link-to-backups", "orders"), join(picked, "beside.key"), db, same).refuse ?? "", /inside/);
	// A folder not named after the database is the one picked: the folder above it is not refused.
	assert.deepEqual(keyPlacement(join(picked, "nightly"), key(join(at, "sibling.key")), db, same), {});
	// A name that only begins like the folder's is not inside it.
	mkdirSync(join(at, "Backups-keys"));
	assert.deepEqual(keyPlacement(dir, key(join(at, "Backups-keys", "k.key")), db, same), {});
	// Elsewhere on this disk: kept.
	const apart = key(join(at, "elsewhere", "orders.key"));
	assert.deepEqual(keyPlacement(dir, apart, db, same), {});
	// Backups on a drive of their own: a key on that drive goes with them; beside the database it does not.
	const disks = (backups: number, database: number, k: number) => (p: string) => (p === dir ? backups : p === db ? database : p === apart ? k : assert.fail(`asked the disk of ${p}`));
	assert.match(keyPlacement(dir, apart, db, disks(2, 1, 2)).refuse ?? "", /disk/);
	assert.deepEqual(keyPlacement(dir, apart, db, disks(2, 1, 1)), {});
	// Others may read it: said, not refused, by what the system says of it.
	const open = key(join(at, "elsewhere", "open.key"), true);
	const r = keyPlacement(dir, open, db, same);
	assert.equal(r.refuse, undefined);
	assert.match(r.warn ?? "", WINDOWS ? /the machine's users \(Users\) may/ : /permissions are 644/);
	assert.equal(othersMayRead(apart), null, "the one kept apart is its owner's alone");
});

test("a Windows DACL is open where an entry lets every user, every signed-in user or the machine's users read, unless denied first", () => {
	const me = "S-1-5-21-1-2-3-1001";
	// What a file in the user's own folder inherits: SYSTEM, Administrators, the user.
	assert.equal(openTo(`D:AI(A;ID;FA;;;SY)(A;ID;FA;;;BA)(A;ID;FA;;;${me})`), null);
	// What `fennec key` makes: protected, the owner alone.
	assert.equal(openTo(`D:P(A;;FA;;;${me})`), null);
	// What C:\ProgramData or a shared folder gives: the machine's users read and run.
	assert.equal(openTo(`D:AI(A;ID;FA;;;SY)(A;ID;0x1200a9;;;BU)`), "the machine's users (Users)");
	assert.equal(openTo(`D:(A;;FR;;;S-1-5-11)`), "every signed-in user (Authenticated Users)", "by SID as well as by name");
	assert.equal(openTo(`D:(A;;GA;;;WD)(A;;FR;;;BU)`), "every user (Everyone)", "the widest is named");
	// Rights that read nothing: writing alone, or reading the permissions.
	assert.equal(openTo(`D:(A;;FW;;;BU)(A;;RC;;;WD)(A;;0x100000;;;AU)`), null);
	// An entry that only passes on to what a folder holds.
	assert.equal(openTo(`D:(A;OICIIO;FA;;;BU)`), null);
	// Denied before it is allowed; Everyone's denial is every group's; denied after is too late.
	assert.equal(openTo(`D:(D;;FR;;;BU)(A;;FR;;;BU)`), null);
	assert.equal(openTo(`D:(D;;FA;;;WD)(A;;FR;;;AU)(A;;FR;;;BU)`), null);
	assert.equal(openTo(`D:(A;;FR;;;BU)(D;;FR;;;BU)`), "the machine's users (Users)");
	// Rights spelled as SDDL's two-letter names, or by mask.
	assert.equal(openTo(`D:(A;;CCLCSWRPRC;;;BU)`), "the machine's users (Users)", "CC is FILE_READ_DATA");
	assert.equal(openTo(`D:(A;;0x80000000;;;BU)`), "the machine's users (Users)", "GENERIC_READ");
	// No DACL: anyone. None read: nothing said.
	assert.equal(openTo("D:NO_ACCESS_CONTROL"), "every user (Everyone)");
	assert.equal(openTo(null), null);
});

test("sealed with a key file: the key is made once, the backup reads only with it", { skip }, async () => {
	const { server } = await serve("sealed", 1);
	mkdirSync(join(work, "keys"), { recursive: true });
	const keyFile = join(work, "keys", "sealed.key");
	const made = await makeKey(nodeRun, keyFile);
	assert.ok(made.ok, made.message);
	assert.equal(ownerAlone(keyFile), true, "readable by its owner alone: mode 0600, a DACL of its owner alone on Windows");
	assert.equal((await makeKey(nodeRun, keyFile)).ok, false, "a key is never written over");
	const archive = { dir: join(work, "backups", "sealed"), keyFile };
	assert.ok((await takeBackup(nodeRun, { local: server.local, replicationToken: server.replicationToken! }, archive)).ok);
	const image = readFileSync(join(archive.dir, listImages(archive.dir)[0]!.file));
	assert.ok(!image.subarray(0, 7).equals(Buffer.from("FENECDB")), "the image is not a database in the clear");
	assert.equal((await verifyArchive(nodeRun, { ...archive, keyFile: null })).ok, false, "without the key it does not read");
	assert.ok((await verifyArchive(nodeRun, archive)).ok);
	const out = join(work, "unsealed.fennec");
	assert.equal((await restoreArchive(nodeRun, { ...archive, keyFile: null }, null, out)).ok, false);
	assert.ok(!existsSync(out), "a failed restore leaves nothing at the name");
	assert.ok((await restoreArchive(nodeRun, archive, null, out)).ok);
	assert.equal(await count(out), 1);
	noScratch(work);
});

test("a wrong replication token is a refusal in words", { skip }, async () => {
	const { server } = await serve("refused", 0);
	const r = await takeBackup(nodeRun, { local: server.local, replicationToken: "not-it" }, { dir: join(work, "backups", "refused"), keyFile: null });
	assert.equal(r.ok, false);
	assert.match(r.ok ? "" : r.message, /401|replication token/);
});

test("an archive folder's images, newest first; what is not one is passed over", () => {
	const names = ["image-00000000000000000003-1792968462639.fennec", "segment-1.fennec", "image-00000000000000000007-1792968500000.fennec", ".DS_Store", "image-7-1.fennec"];
	assert.deepEqual(imagesIn(names), [
		{ change: 7, at: 1792968500000, file: "image-00000000000000000007-1792968500000.fennec" },
		{ change: 3, at: 1792968462639, file: "image-00000000000000000003-1792968462639.fennec" },
	]);
	assert.deepEqual(listImages(join(work, "no-such-folder")), []);
});

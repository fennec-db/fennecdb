import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { chmodSync, lstatSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { connect, createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { claimInstance, instancePath, ownFolder, pipeName, runtimeFolder, type PipeSecurity } from "../src/bun/instance.ts";
import { WINDOWS } from "./platform.ts";

// One instance of the app at a time: the second asks the first to show itself
// and is told to quit, before it reads state.json or touches the credential
// store. Where the guard listens is a socket on macOS and Linux and a named
// pipe on Windows, which Node, running these, listens on as Bun does; the
// pipe's security calls are Bun's (win32.ts), so here they are stand-ins, and
// the real ones are held by `checks/pages.mjs` on Windows.

// A short folder: a socket's path is 104 bytes at most on macOS, and the
// test's temporary folder inside $TMPDIR is longer than the app's own name there.
// Windows has no /tmp (a folder of that name on the current drive, if any).
const work = mkdtempSync(WINDOWS ? join(tmpdir(), "fdi-") : "/tmp/fdi-");
after(() => rmSync(work, { recursive: true, force: true }));

/** Where an instance named `name` listens on this system: a socket in the test's folder, or a pipe of a made-up user's. */
const where = (name: string) => (WINDOWS ? pipeName(`fdi-${name}-${randomUUID()}`, "S-1-5-21-1-2-3-1001") : instancePath(name, work));

const asked = () => {
	let n = 0;
	let wake: () => void = () => {};
	return {
		count: () => n,
		onAsk: () => {
			n++;
			wake();
		},
		next: () => new Promise<void>((r) => (wake = r)),
	};
};

test("the first instance claims it; a second is told to quit and the first is asked to show itself", async () => {
	const path = where("one");
	const first = asked();
	assert.equal(await claimInstance(path, first.onAsk), "alone");
	const shown = first.next();
	assert.equal(await claimInstance(path, () => assert.fail("a second instance listens for nobody")), "another");
	await shown;
	assert.equal(first.count(), 1);
});

test("two started together: one claims it, the other quits", async () => {
	const path = where("two");
	const results = await Promise.all([claimInstance(path, () => {}), claimInstance(path, () => {})]);
	assert.deepEqual(results.sort(), ["alone", "another"]);
});

test("an instance that ended leaves nothing that holds the next one back", async () => {
	const path = where("stale");
	// A process that listens and is killed: on a Unix it leaves its socket's file
	// (a listener closed in Node unlinks it), which answers nothing; a pipe goes with it.
	spawnSync(process.execPath, ["-e", `require("node:net").createServer().listen(${JSON.stringify(path)}, () => process.kill(process.pid, "SIGKILL"))`]);
	if (!WINDOWS) assert.ok(lstatSync(path).isSocket(), "the dead instance's socket is there");
	const now = asked();
	assert.equal(await claimInstance(path, now.onAsk), "alone", "nothing answers: this one runs");
	const shown = now.next();
	assert.equal(await claimInstance(path, () => {}), "another");
	await shown;
});

test("a plain file where the socket goes is taken over as well", { skip: WINDOWS && "a pipe is no file: Windows has nothing in its place to take over" }, async () => {
	const path = instancePath("file", work);
	writeFileSync(path, "");
	assert.equal(await claimInstance(path, () => {}), "alone");
});

test("a guard that cannot be made does not keep the app from starting: it runs unguarded", async () => {
	if (WINDOWS) {
		// A pipe's name holds no folder: a name Windows refuses is one with a character a pipe may not have.
		assert.equal(await claimInstance("\\\\.\\pipe\\", () => {}), "unguarded");
		return;
	}
	assert.equal(await claimInstance(join(work, "no-such-folder", "x.instance"), () => {}), "unguarded");
	// macOS refuses a socket's path past its 104 bytes; Linux's libuv takes the first 108 of one.
	if (process.platform === "darwin") assert.equal(await claimInstance(join(work, "x".repeat(120)), () => {}), "unguarded", "a path past the 104 bytes a socket's may have");
});

/** The pipe's security calls as instance.ts asks them, answering as told and saying what was asked. */
function security(owner: (PipeSecurity["owner"] extends (n: string) => infer R ? R : never)[], secureFails = false) {
	const calls = { secured: [] as string[], asked: 0 };
	const pipe: PipeSecurity = {
		secure: (name) => {
			calls.secured.push(name);
			if (secureFails) throw new Error("SetNamedSecurityInfoW: error 5");
		},
		owner: () => owner[Math.min(calls.asked++, owner.length - 1)]!,
	};
	return { pipe, calls };
}

test("Windows' pipe: made its user's alone once it listens, and still the one instance if that fails", async () => {
	const path = where("secured");
	const { pipe, calls } = security(["ours"]);
	assert.equal(await claimInstance(path, () => {}, pipe), "alone");
	assert.deepEqual(calls.secured, [path], "secured once, by its name");
	const other = where("secured-fails");
	const failing = security(["ours"], true);
	assert.equal(await claimInstance(other, () => {}, failing.pipe), "alone", "the name is held: the DACL is the defence beside it");
});

test("Windows' pipe: one someone else made first is never asked, and the app runs unguarded rather than quit", async () => {
	const path = where("squatted");
	// The squatter: listens under the name, and records every connection and what it said.
	const told: string[] = [];
	let heard: () => void = () => {};
	const squatter = createServer((c) => {
		let said = "";
		c.on("data", (d) => (said += d));
		c.on("end", () => {
			told.push(said);
			heard();
		});
	});
	await new Promise<void>((r) => squatter.listen(path, r));
	try {
		const theirs = security(["theirs"]);
		assert.equal(await claimInstance(path, () => {}, theirs.pipe), "unguarded");
		assert.deepEqual(theirs.calls.secured, [], "nothing of its was changed");
		// Every instance busy, then ours: asked again, and then asked to show itself.
		const busy = security(["busy", "busy", "ours"]);
		const ended = new Promise<void>((r) => (heard = r));
		assert.equal(await claimInstance(path, () => {}, busy.pipe), "another");
		assert.equal(busy.calls.asked, 3);
		await ended;
		// A last connection, heard after the others: had the first claim asked too, there would be two "show"s before it.
		const control = new Promise<void>((r) => (heard = r));
		await new Promise<void>((r) => {
			const c = connect(path, () => c.end("control\n", r));
		});
		await control;
		assert.deepEqual(told, ["show\n", "control\n"], "the squatter was told nothing while it was not ours");
	} finally {
		squatter.close();
	}
});

test("Windows: the pipe is named by the user's SID, so another user's name is another pipe", () => {
	const a = pipeName("dev.fennecdb.studio", "S-1-5-21-1-2-3-1001");
	assert.equal(a, "\\\\.\\pipe\\dev.fennecdb.studio.instance.S-1-5-21-1-2-3-1001");
	assert.notEqual(a, pipeName("dev.fennecdb.studio", "S-1-5-21-1-2-3-1002"));
	assert.notEqual(a, pipeName("dev.fennecdb.studio.test", "S-1-5-21-1-2-3-1001"), "the test variant's is its own");
});

const unixFolders = WINDOWS && "a Unix folder's owner and mode: Windows' guard is a pipe";

test("Linux: the socket goes in $XDG_RUNTIME_DIR where that is the user's alone, else the app's own folder, else nowhere", { skip: unixFolders }, () => {
	const uid = process.getuid!();
	const runtime = join(work, "runtime");
	const data = join(work, "data");
	mkdirSync(runtime, { mode: 0o700 });
	mkdirSync(data, { mode: 0o755 });
	chmodSync(runtime, 0o700);
	chmodSync(data, 0o755);
	assert.equal(runtimeFolder({ XDG_RUNTIME_DIR: runtime }, uid, data), runtime);
	assert.ok(ownFolder(data, uid), "0755 is written by its owner alone");
	// Not the user's alone: /tmp's mode (others write to it), or another user's folder.
	chmodSync(runtime, 0o1777);
	assert.equal(runtimeFolder({ XDG_RUNTIME_DIR: runtime }, uid, data), data, "a folder others write to is passed over");
	chmodSync(runtime, 0o700);
	assert.equal(runtimeFolder({ XDG_RUNTIME_DIR: runtime }, uid + 1, data), null, "nor is one of another user's");
	assert.equal(runtimeFolder({}, uid, data), data, "no XDG_RUNTIME_DIR: the app's own folder");
	assert.equal(runtimeFolder({ XDG_RUNTIME_DIR: join(work, "missing") }, uid, join(work, "missing-too")), null, "none: no guard, rather than one in a place others share");
	// A link to the user's folder is not the folder: what it points at could change under the app.
	const link = join(work, "link-to-runtime");
	spawnSync("ln", ["-s", runtime, link]);
	assert.equal(runtimeFolder({ XDG_RUNTIME_DIR: link }, uid, join(work, "missing")), null);
});

test("the app's own socket fits where macOS puts the user's temporary folder", () => {
	// $TMPDIR is /var/folders/xx/<28 characters>/T/ for every user.
	const tmp = "/var/folders/8x/24jbdqnn3g3_r2knsn99ntw80000gn/T/";
	assert.ok(Buffer.byteLength(instancePath("dev.fennecdb.studio.test", tmp)) < 104);
});

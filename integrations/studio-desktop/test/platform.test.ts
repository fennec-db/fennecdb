import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { argvOf, ownerAlone, WINDOWS } from "./platform.ts";

// The questions test/platform.ts asks of the system, asked here where the
// answer is known, so a test that leans on one is not leaning on a guess.

const work = mkdtempSync(join(tmpdir(), "fennec-desktop-platform-"));
after(() => rmSync(work, { recursive: true, force: true }));

test("a process's arguments are read as the system shows them", async () => {
	const marker = `fennec-argv-${process.pid}`;
	const child = spawn(process.execPath, ["-e", "setTimeout(() => {}, 60000)", marker, "--sync off"], { stdio: "ignore" });
	try {
		await new Promise((r) => child.once("spawn", r));
		const argv = argvOf(child.pid!);
		assert.ok(argv.includes(marker), argv);
		assert.ok(argv.includes("--sync off"), `an argument with a space in it, whole: ${argv}`);
	} finally {
		child.kill();
	}
});

test("a file anyone may read is not its owner's alone", () => {
	const file = join(work, "open.key");
	writeFileSync(file, "k");
	// On a Unix a mode says so; on Windows the file inherits its folder's ACL (the user's
	// temporary folder grants SYSTEM and the administrators too), which is not the owner's alone.
	if (!WINDOWS) chmodSync(file, 0o644);
	assert.notEqual(ownerAlone(file), true);
	if (!WINDOWS) {
		chmodSync(file, 0o600);
		assert.equal(ownerAlone(file), true);
	}
});

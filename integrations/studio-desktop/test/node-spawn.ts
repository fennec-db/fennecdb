// Node's spawn and a runner of `fennec`, as process.ts and backups.ts take them,
// so `node --test` drives the real binaries through the app's own code (the
// app hands in Bun's: server.ts). Not a test file of its own.

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import type { Run } from "../src/bun/backups.ts";
import type { Child, Spawn } from "../src/bun/process.ts";

// SPIKE: fileURLToPath, not `.pathname` (`/D:/a/...` on Windows), and `.exe` there.
const BIN = fileURLToPath(new URL("../bin/", import.meta.url));
const EXE = process.platform === "win32" ? ".exe" : "";
/** The binaries `make studio-desktop` puts in bin/ (scripts/get-server.sh). */
export const SERVER = process.env["FENNEC_SERVER_BIN"] || `${BIN}fennec-server${EXE}`;
export const CLI = process.env["FENNEC_CLI_BIN"] || `${BIN}fennec${EXE}`;
/** Why a test that needs them cannot run, or "" when it can. */
export const missing = () => [SERVER, CLI].filter((b) => !existsSync(b)).map((b) => `${b} is not there: run scripts/get-server.sh`).join("; ");

export const nodeSpawn: Spawn = (argv, env): Child => {
	const p = spawn(argv[0]!, argv.slice(1), { env, stdio: ["pipe", "pipe", "pipe"] });
	const exited = new Promise<number | null>((resolve) => p.on("exit", (code) => resolve(code)));
	return {
		pid: p.pid!,
		stdin: p.stdin,
		stdout: p.stdout as unknown as AsyncIterable<Uint8Array>,
		stderr: p.stderr as unknown as AsyncIterable<Uint8Array>,
		exited,
		get exitCode() {
			return p.exitCode;
		},
		kill: (signal) => void p.kill(signal),
		endStdin: () => void p.stdin.end(),
	};
};

/** `fennec` with `args`, its output whole. */
export const nodeRun: Run = (args, env) =>
	new Promise((resolve) => {
		const p = spawn(CLI, args, { env, stdio: ["ignore", "pipe", "pipe"] });
		let out = "";
		let err = "";
		p.stdout.on("data", (b) => (out += b));
		p.stderr.on("data", (b) => (err += b));
		p.on("close", (code) => resolve({ code, out, err }));
	});

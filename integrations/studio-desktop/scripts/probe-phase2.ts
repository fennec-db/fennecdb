// TEMPORARY (phase 2, removed before the pull request is ready): what Bun and
// the system answer to the calls phase 2 makes, printed on each runner.
//   bun scripts/probe-phase2.ts            the system's own probe
//   bun scripts/probe-phase2.ts store      Linux: askSecretService on this bus, timed
import { spawnSync } from "node:child_process";
import { claimInstance, pipeName } from "../src/bun/instance.ts";
import { askSecretService } from "../src/bun/secretservice.ts";

const out = (what: string, v: unknown) => console.log(`PROBE ${what}: ${typeof v === "string" ? v : JSON.stringify(v)}`);
const timed = async <T>(f: () => Promise<T> | T) => {
	const t0 = performance.now();
	try {
		return { ok: true, value: await f(), ms: Math.round(performance.now() - t0) };
	} catch (e) {
		return { ok: false, error: String((e as Error).message ?? e), ms: Math.round(performance.now() - t0) };
	}
};

out("bun", `${Bun.version} ${process.platform}-${process.arch}`);
if (process.argv[2] === "store") {
	out("bus", process.env["DBUS_SESSION_BUS_ADDRESS"] ?? "(none)");
	out("askSecretService", await timed(() => askSecretService()));
	out("Bun.secrets.get", await timed(() => Bun.secrets.get({ service: "dev.fennecdb.studio.probe", name: "x" })));
	process.exit(0);
}

if (process.platform === "win32") {
	const w = await import("../src/bun/win32.ts");
	const user = w.userSid();
	out("userSid", user);
	out("ownerSid", w.ownerSid());
	out("whoami", spawnSync("whoami", ["/user", "/fo", "csv", "/nh"], { encoding: "utf8" }).stdout.trim());
	const name = pipeName(`dev.fennecdb.studio.probe-${process.pid}`, user);
	const ours = new Set([user, w.ownerSid(), "S-1-5-32-544"]);
	const pipe = {
		secure: (n: string) => w.restrictPipe(n, user),
		owner: (n: string) => {
			const o = w.pipeOwner(n);
			out("pipeOwner", o);
			if ("sid" in o) return ours.has(o.sid) ? ("ours" as const) : ("theirs" as const);
			return o.error === w.ERROR_PIPE_BUSY ? ("busy" as const) : ("theirs" as const);
		},
	};
	let asked = 0;
	out("first claim", await timed(() => claimInstance(name, () => asked++, pipe)));
	out("sddl", await timed(() => w.pipeSddl(name)));
	out("second claim", await timed(() => claimInstance(name, () => {}, pipe)));
	await Bun.sleep(300);
	out("first asked", asked);
	// A credential kept on this machine, read back through Bun, and as cmdkey lists it.
	const service = "dev.fennecdb.studio.probe";
	out("writeLocalCredential", await timed(() => w.writeLocalCredential(service, "probe:x", "the-value")));
	out("Bun.secrets.get", await timed(() => Bun.secrets.get({ service, name: "probe:x" })));
	out("cmdkey local", spawnSync("cmdkey", [`/list:${service}/probe:x`], { encoding: "utf8" }).stdout.replace(/\s+/g, " "));
	await Bun.secrets.set({ service, name: "probe:y", value: "v" });
	out("cmdkey bun", spawnSync("cmdkey", [`/list:${service}/probe:y`], { encoding: "utf8" }).stdout.replace(/\s+/g, " "));
	out("delete", await timed(async () => [await Bun.secrets.delete({ service, name: "probe:x" }), await Bun.secrets.delete({ service, name: "probe:y" })]));
	// A .cmd started without a shell, which the environment check's stand-in would be.
	out("spawn .cmd", await timed(() => Bun.spawnSync(["C:\\Windows\\System32\\cmd.exe", "/c", "exit 0"]).exitCode));
} else if (process.platform === "linux") {
	out("XDG_RUNTIME_DIR", process.env["XDG_RUNTIME_DIR"] ?? "(none)");
	out("bus", process.env["DBUS_SESSION_BUS_ADDRESS"] ?? "(none)");
	out("askSecretService (no keyring started)", await timed(() => askSecretService()));
}
process.exit(0);

import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, type Server, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { READ_ALIAS_TRIES, answerOf, askSecretService, externalAuth, methodCall, readMessages, sessionAddresses, socketPath, storeRefusal } from "../src/bun/secretservice.ts";
import { WINDOWS } from "./platform.ts";

// Linux's credential store, asked before Bun.secrets is: the bus's address,
// D-Bus's messages both ways, and the answer read, against a bus of the test's
// own that answers as each kind of desktop does. The real one, GNOME Keyring
// on a session bus, is `checks/keyring.mjs`'s.

const work = mkdtempSync(join(tmpdir(), "fdss-"));
after(() => rmSync(work, { recursive: true, force: true }));

test("the session bus's addresses, as DBUS_SESSION_BUS_ADDRESS gives them, else systemd's beside XDG_RUNTIME_DIR", () => {
	assert.deepEqual(sessionAddresses({ DBUS_SESSION_BUS_ADDRESS: "unix:path=/run/user/1000/bus" }), [{ transport: "unix", keys: { path: "/run/user/1000/bus" } }]);
	const two = sessionAddresses({ DBUS_SESSION_BUS_ADDRESS: "unix:abstract=/tmp/dbus-Ab%2c1,guid=0123;tcp:host=localhost,port=1" });
	assert.deepEqual(two[0], { transport: "unix", keys: { abstract: "/tmp/dbus-Ab,1", guid: "0123" } }, "a value's escapes undone");
	assert.equal(socketPath(two[0]!), "\0/tmp/dbus-Ab,1", "an abstract socket's name after a NUL");
	assert.equal(socketPath(two[1]!), null, "TCP is not a bus this app speaks to");
	assert.deepEqual(sessionAddresses({ XDG_RUNTIME_DIR: "/run/user/7" }), [{ transport: "unix", keys: { path: "/run/user/7/bus" } }]);
	assert.deepEqual(sessionAddresses({}), []);
});

test("SASL EXTERNAL sends the uid's decimal text as hex", () => {
	assert.equal(externalAuth(1000), "31303030");
	assert.equal(externalAuth(0), "30");
});

// ------------------------------------------------------------------ a bus of the test's own

/** A message a bus sends: little-endian, its header fields as D-Bus lays them out, a body of strings or object paths. */
function message(type: number, serial: number, fields: [number, string, string | number][], signature: string, body: string[]): Buffer {
	const out: number[] = [];
	const align = (n: number) => {
		while (out.length % n) out.push(0);
	};
	const u32 = (v: number) => {
		align(4);
		out.push(v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff);
	};
	const str = (s: string) => {
		u32(Buffer.byteLength(s));
		out.push(...Buffer.from(s), 0);
	};
	const sig = (s: string) => out.push(s.length, ...Buffer.from(s), 0);
	const bodyBytes: number[] = [];
	{
		// The body is aligned from its own start, which is 8-aligned in the message.
		for (const b of body) {
			while (bodyBytes.length % 4) bodyBytes.push(0);
			const n = Buffer.byteLength(b);
			bodyBytes.push(n & 0xff, (n >>> 8) & 0xff, (n >>> 16) & 0xff, (n >>> 24) & 0xff, ...Buffer.from(b), 0);
		}
	}
	out.push(0x6c, type, 0, 1);
	u32(bodyBytes.length);
	u32(serial);
	const at = out.length;
	u32(0);
	const start = out.length;
	const all: [number, string, string | number][] = signature ? [...fields, [8, "g", signature]] : fields;
	for (const [code, t, v] of all) {
		align(8);
		out.push(code);
		sig(t);
		if (t === "u") u32(v as number);
		else if (t === "g") sig(v as string);
		else str(v as string);
	}
	const len = out.length - start;
	out[at] = len & 0xff;
	out[at + 1] = (len >>> 8) & 0xff;
	align(8);
	return Buffer.from([...out, ...bodyBytes]);
}

/** A bus that takes any uid and answers ReadAlias as `answer` says, or never; `slash-first`, "/" to a connection's first ReadAlias alone, as gnome-keyring does to a connection it does not know yet. */
async function bus(answer: "collection" | "slash" | "slash-first" | "unknown" | "silent" | "refuse-auth"): Promise<{ path: string; server: Server; saw: string[] }> {
	const path = join(work, `bus-${answer}-${Math.random().toString(36).slice(2)}`);
	const saw: string[] = [];
	const server = createServer((c: Socket) => {
		let authed = false;
		let pending = Buffer.alloc(0);
		c.on("data", (chunk: Buffer) => {
			pending = Buffer.concat([pending, chunk]);
			if (!authed) {
				const eol = pending.indexOf("\r\n");
				if (eol < 0) return;
				const line = pending.toString("latin1", 0, eol);
				pending = pending.subarray(eol + 2);
				saw.push(line);
				if (answer === "refuse-auth") return void c.end("REJECTED EXTERNAL\r\n");
				c.write("OK 0123456789abcdef0123456789abcdef\r\n");
				authed = true;
			}
			if (pending.indexOf("BEGIN\r\n") === 0) pending = pending.subarray(7);
			const { messages, used } = readMessages(pending);
			pending = pending.subarray(used);
			// The member is a header field the reader does not keep: Hello is the client's first call, ReadAlias its second.
			for (const { serial: s, body } of messages) {
				saw.push(`call ${s}: ${body.join(",")}`);
				if (s === 1) c.write(message(2, 1, [[5, "u", 1], [6, "s", ":1.9"]], "s", [":1.9"]));
				// Signals a bus sends between replies are passed over.
				if (s === 1) c.write(message(4, 2, [[1, "o", "/org/freedesktop/DBus"], [2, "s", "org.freedesktop.DBus"], [3, "s", "NameAcquired"]], "s", [":1.9"]));
				// Every call after Hello is a ReadAlias, each answered to its own serial.
				const slash = answer === "slash" || (answer === "slash-first" && s === 2);
				if (s >= 2 && (answer === "collection" || answer === "slash-first") && !slash) c.write(message(2, s + 1, [[5, "u", s]], "o", ["/org/freedesktop/secrets/collection/login"]));
				if (s >= 2 && slash) c.write(message(2, s + 1, [[5, "u", s]], "o", ["/"]));
				if (s >= 2 && answer === "unknown") c.write(message(3, s + 1, [[4, "s", "org.freedesktop.DBus.Error.ServiceUnknown"], [5, "u", s]], "s", ["The name org.freedesktop.secrets was not provided by any .service files"]));
			}
		});
		c.on("error", () => {});
	});
	await new Promise<void>((r) => server.listen(path, r));
	return { path, server, saw };
}

const unixBus = WINDOWS && "D-Bus is Linux's; Windows' Node takes no socket path";

test("a store with a default collection: the store is there", { skip: unixBus }, async () => {
	const b = await bus("collection");
	try {
		const a = await askSecretService({ DBUS_SESSION_BUS_ADDRESS: `unix:path=${b.path}` }, 2000, 1000);
		assert.deepEqual(a, { ok: true, collection: "/org/freedesktop/secrets/collection/login" });
		assert.equal(b.saw[0], "\0AUTH EXTERNAL 31303030", "a NUL, then the uid");
		assert.deepEqual(b.saw.slice(1), ["call 1: ", "call 2: default"], "Hello, then ReadAlias(\"default\")");
	} finally {
		b.server.close();
	}
});

test("a store with no default collection is no store: never 'no token'", { skip: unixBus }, async () => {
	const b = await bus("slash");
	try {
		const a = await askSecretService({ DBUS_SESSION_BUS_ADDRESS: `unix:path=${b.path}` }, 2000, 1000);
		assert.equal(a.ok, false);
		assert.equal(!a.ok && a.why, "no-default");
		assert.match(storeRefusal(a as Extract<typeof a, { ok: false }>), /no default keyring/);
		assert.deepEqual(b.saw.slice(1), ["call 1: ", ...Array.from({ length: READ_ALIAS_TRIES }, (_, i) => `call ${i + 2}: default`)], "asked READ_ALIAS_TRIES times, each on the answer before it");
	} finally {
		b.server.close();
	}
});

test("a '/' from a store that did not know the connection yet is asked again, and its collection answers", { skip: unixBus }, async () => {
	const b = await bus("slash-first");
	try {
		const a = await askSecretService({ DBUS_SESSION_BUS_ADDRESS: `unix:path=${b.path}` }, 2000, 1000);
		assert.deepEqual(a, { ok: true, collection: "/org/freedesktop/secrets/collection/login" });
		assert.deepEqual(b.saw.slice(1), ["call 1: ", "call 2: default", "call 3: default"], "one connection: Hello, the first ReadAlias, and once more");
	} finally {
		b.server.close();
	}
});

test("no Secret Service on the bus: said at once, by the bus's own error", { skip: unixBus }, async () => {
	const b = await bus("unknown");
	try {
		const a = await askSecretService({ DBUS_SESSION_BUS_ADDRESS: `unix:path=${b.path}` }, 2000, 1000);
		assert.equal(!a.ok && a.why, "no-service");
		assert.match(!a.ok ? a.detail : "", /ServiceUnknown: The name org\.freedesktop\.secrets/);
	} finally {
		b.server.close();
	}
});

test("a store that never answers is refused at the bound, not after the bus's 25 s", { skip: unixBus }, async () => {
	const b = await bus("silent");
	try {
		const t0 = Date.now();
		const a = await askSecretService({ DBUS_SESSION_BUS_ADDRESS: `unix:path=${b.path}` }, 300, 1000);
		assert.ok(Date.now() - t0 >= 300, "the bound was waited for");
		assert.equal(!a.ok && a.why, "no-answer");
		assert.match(storeRefusal(a as Extract<typeof a, { ok: false }>), /did not answer/);
	} finally {
		b.server.close();
	}
});

test("no bus, or one that refuses this user: no store", { skip: unixBus }, async () => {
	const none = await askSecretService({ DBUS_SESSION_BUS_ADDRESS: `unix:path=${join(work, "nothing-here")}` }, 1000, 1000);
	assert.equal(!none.ok && none.why, "no-bus");
	assert.equal((await askSecretService({}, 1000, 1000)).ok, false);
	const b = await bus("refuse-auth");
	try {
		const a = await askSecretService({ DBUS_SESSION_BUS_ADDRESS: `unix:path=${b.path}` }, 2000, 1000);
		assert.equal(!a.ok && a.why, "no-bus");
		assert.match(!a.ok ? a.detail : "", /refused this user/);
	} finally {
		b.server.close();
	}
});

test("the call the app makes: ReadAlias(\"default\") on the Secret Service, read back by the same reader", () => {
	const m = methodCall(2, "org.freedesktop.secrets", "/org/freedesktop/secrets", "org.freedesktop.Secret.Service", "ReadAlias", ["default"]);
	assert.equal(m[0], 0x6c);
	assert.equal(m[1], 1, "a method call");
	assert.equal(m.readUInt32LE(8), 2, "its serial");
	const { messages, used } = readMessages(m);
	assert.equal(used, m.length);
	assert.equal(messages[0]!.signature, "s");
	assert.deepEqual(messages[0]!.body, ["default"]);
	for (const s of ["org.freedesktop.secrets", "/org/freedesktop/secrets", "org.freedesktop.Secret.Service", "ReadAlias"]) assert.ok(m.includes(Buffer.from(`${s}\0`)), s);
	// A message cut short is left for the next read.
	assert.deepEqual(readMessages(m.subarray(0, m.length - 1)), { messages: [], used: 0 });
	assert.deepEqual(answerOf({ type: 2, serial: 3, replySerial: 2, errorName: null, signature: "o", body: ["/org/freedesktop/secrets/aliases/default"] }), { ok: true, collection: "/org/freedesktop/secrets/aliases/default" });
});

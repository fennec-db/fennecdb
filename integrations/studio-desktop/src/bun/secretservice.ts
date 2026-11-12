// Whether Linux's credential store can keep a token, asked before Bun.secrets
// is: a Secret Service (GNOME Keyring, KWallet, KeePassXC) answering on the
// session bus, with a default collection (the "login" keyring) to keep it in.
//
// Bun.secrets goes through libsecret, which says neither in time. Where the
// service is installed but not running and its activation never answers,
// every call waited the bus's 25 s and then failed; where the service runs
// with no default collection, `set` failed and `get` answered null, as if no
// token had ever been saved (#578's runs). So the app asks the service itself,
// one D-Bus call, `org.freedesktop.Secret.Service.ReadAlias("default")`,
// bounded at a few seconds: an answer that is a collection means the store is
// there; "/" means there is none to keep anything in; no answer, or an error,
// means no store. The call opens no prompt (a locked keyring is unlocked by
// libsecret when an item is read, with the user, unbounded) and is spoken here
// over the bus's socket, so no program outside the app is needed.
//
// What D-Bus asks of a client is small for one call: a NUL byte and the SASL
// EXTERNAL handshake (the uid, which the bus checks against the socket's
// credentials), then `Hello` and the call, each a message of a fixed header,
// its fields and a body, little-endian, every value aligned to its size.
// Pure but for the socket, so `node --test` holds the encoding
// (test/secretservice.test.ts).

import { connect, type Socket } from "node:net";

export const SECRET_SERVICE = "org.freedesktop.secrets";
/** How long the store has to answer before the app says it is not there. */
export const STORE_ANSWER_MS = 3000;

export type StoreAnswer =
	| { ok: true; collection: string }
	| { ok: false; why: "no-bus" | "no-service" | "no-answer" | "no-default"; detail: string };

/** A bus address's transport and its keys (`unix:path=/run/user/1000/bus` -> unix, {path}). */
export type Address = { transport: string; keys: Record<string, string> };

/** DBUS_SESSION_BUS_ADDRESS's addresses, in order, the values unescaped; or the bus beside XDG_RUNTIME_DIR, systemd's, where it names none. */
export function sessionAddresses(env: Record<string, string | undefined>): Address[] {
	const given = env["DBUS_SESSION_BUS_ADDRESS"];
	if (!given) return env["XDG_RUNTIME_DIR"] ? [{ transport: "unix", keys: { path: `${env["XDG_RUNTIME_DIR"]}/bus` } }] : [];
	return given
		.split(";")
		.filter(Boolean)
		.map((a) => {
			const colon = a.indexOf(":");
			const keys: Record<string, string> = {};
			for (const kv of a.slice(colon + 1).split(",")) {
				const eq = kv.indexOf("=");
				if (eq > 0) keys[kv.slice(0, eq)] = decodeURIComponent(kv.slice(eq + 1));
			}
			return { transport: a.slice(0, colon), keys };
		});
}

/** The socket path Node and Bun connect to for a Unix address: an abstract one's name after a NUL. Null for any other transport. */
export function socketPath(a: Address): string | null {
	if (a.transport !== "unix") return null;
	if (a.keys["path"]) return a.keys["path"];
	if (a.keys["abstract"]) return `\0${a.keys["abstract"]}`;
	return null;
}

// ------------------------------------------------------------------ messages

const METHOD_CALL = 1;
const METHOD_RETURN = 2;
const ERROR = 3;
const FIELD = { PATH: 1, INTERFACE: 2, MEMBER: 3, ERROR_NAME: 4, REPLY_SERIAL: 5, DESTINATION: 6, SIGNATURE: 8 } as const;

/** A buffer written in D-Bus's little-endian marshalling, each value at its alignment from the message's start. */
class Writer {
	bytes: number[] = [];
	align(n: number) {
		while (this.bytes.length % n) this.bytes.push(0);
	}
	byte(b: number) {
		this.bytes.push(b & 0xff);
	}
	u32(v: number) {
		this.align(4);
		this.bytes.push(v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff);
	}
	/** A string or an object path: its UTF-8 length, the bytes, a NUL. */
	string(s: string) {
		const b = Buffer.from(s, "utf8");
		this.u32(b.length);
		this.bytes.push(...b, 0);
	}
	/** A signature: its length in a byte, the bytes, a NUL. */
	signature(s: string) {
		this.byte(s.length);
		this.bytes.push(...Buffer.from(s, "ascii"), 0);
	}
	at(offset: number, v: number) {
		this.bytes[offset] = v & 0xff;
		this.bytes[offset + 1] = (v >>> 8) & 0xff;
		this.bytes[offset + 2] = (v >>> 16) & 0xff;
		this.bytes[offset + 3] = (v >>> 24) & 0xff;
	}
}

/** A method call: `member` of `iface` on `path` at `destination`, with string arguments alone. */
export function methodCall(serial: number, destination: string, path: string, iface: string, member: string, args: string[] = []): Buffer {
	const body = new Writer();
	for (const a of args) body.string(a);
	const w = new Writer();
	w.byte(0x6c); // 'l': little-endian
	w.byte(METHOD_CALL);
	w.byte(0);
	w.byte(1);
	w.u32(body.bytes.length);
	w.u32(serial);
	const fieldsAt = w.bytes.length;
	w.u32(0);
	const fieldsStart = w.bytes.length;
	const field = (code: number, sig: string, write: () => void) => {
		w.align(8);
		w.byte(code);
		w.signature(sig);
		write();
	};
	field(FIELD.PATH, "o", () => w.string(path));
	field(FIELD.INTERFACE, "s", () => w.string(iface));
	field(FIELD.MEMBER, "s", () => w.string(member));
	field(FIELD.DESTINATION, "s", () => w.string(destination));
	if (args.length) field(FIELD.SIGNATURE, "g", () => w.signature("s".repeat(args.length)));
	w.at(fieldsAt, w.bytes.length - fieldsStart);
	w.align(8);
	return Buffer.from([...w.bytes, ...body.bytes]);
}

export type Reply = { type: number; serial: number; replySerial: number | null; errorName: string | null; signature: string; body: string[] };

/**
 * The whole messages at the start of `buf`, and how many bytes they took: a
 * reply's serial it answers, its error's name, and its body's strings and
 * object paths (all a reply here carries). A message not whole yet is left.
 */
export function readMessages(buf: Buffer): { messages: Reply[]; used: number } {
	const messages: Reply[] = [];
	let at = 0;
	while (buf.length - at >= 16) {
		const little = buf[at] === 0x6c;
		const u32 = (o: number) => (little ? buf.readUInt32LE(o) : buf.readUInt32BE(o));
		const bodyLen = u32(at + 4);
		const fieldsLen = u32(at + 12);
		const headerEnd = at + 16 + fieldsLen;
		const bodyStart = headerEnd + ((8 - ((headerEnd - at) % 8)) % 8);
		const end = bodyStart + bodyLen;
		if (buf.length < end) break;
		const m: Reply = { type: buf[at + 1]!, serial: u32(at + 8), replySerial: null, errorName: null, signature: "", body: [] };
		// Alignment is from the message's start.
		let o = at + 16;
		const align = (n: number) => (o += (n - ((o - at) % n)) % n);
		const str = () => {
			align(4);
			const n = u32(o);
			const s = buf.toString("utf8", o + 4, o + 4 + n);
			o += 4 + n + 1;
			return s;
		};
		const sig = () => {
			const n = buf[o]!;
			const s = buf.toString("ascii", o + 1, o + 1 + n);
			o += 1 + n + 1;
			return s;
		};
		while (o < headerEnd) {
			align(8);
			if (o >= headerEnd) break;
			const code = buf[o++]!;
			const type = sig();
			let value: string | number;
			if (type === "u") {
				align(4);
				value = u32(o);
				o += 4;
			} else if (type === "s" || type === "o") value = str();
			else if (type === "g") value = sig();
			else break; // no field a reply carries has another type
			if (code === FIELD.REPLY_SERIAL) m.replySerial = value as number;
			else if (code === FIELD.ERROR_NAME) m.errorName = value as string;
			else if (code === FIELD.SIGNATURE) m.signature = value as string;
		}
		o = bodyStart;
		for (const t of m.signature) {
			if (t !== "s" && t !== "o") break;
			m.body.push(str());
		}
		messages.push(m);
		at = end;
	}
	return { messages, used: at };
}

/** What the store's answer to ReadAlias("default") means. */
export function answerOf(reply: Reply): StoreAnswer {
	if (reply.type === ERROR) {
		const name = reply.errorName ?? "an error";
		const detail = `${name}${reply.body[0] ? `: ${reply.body[0]}` : ""}`;
		return { ok: false, why: name === "org.freedesktop.DBus.Error.ServiceUnknown" ? "no-service" : "no-answer", detail };
	}
	const path = reply.body[0] ?? "/";
	return path === "/" ? { ok: false, why: "no-default", detail: "ReadAlias(\"default\") answered \"/\"" } : { ok: true, collection: path };
}

/** The hex of the decimal text of `uid`: what SASL EXTERNAL sends. */
export const externalAuth = (uid: number) => Buffer.from(String(uid), "ascii").toString("hex");

/**
 * Asks the session bus's Secret Service for its default collection, within
 * `ms`. Never throws: a store that is not there is an answer.
 */
export async function askSecretService(env: Record<string, string | undefined> = process.env, ms = STORE_ANSWER_MS, uid = process.getuid?.() ?? 0): Promise<StoreAnswer> {
	const paths = sessionAddresses(env)
		.map(socketPath)
		.filter((p): p is string => p !== null);
	if (!paths.length) return { ok: false, why: "no-bus", detail: "no session bus: DBUS_SESSION_BUS_ADDRESS names none this app reaches, and XDG_RUNTIME_DIR none" };
	const deadline = Date.now() + ms;
	let last: StoreAnswer = { ok: false, why: "no-bus", detail: "no session bus answered" };
	for (const path of paths) {
		if (deadline - Date.now() <= 0) break;
		last = await askOne(path, deadline, ms, uid);
		// A bus that answered has said what there is; only a bus that could not be reached is passed over.
		if (last.ok || last.why !== "no-bus") return last;
	}
	return last;
}

/**
 * One bus asked, until `deadline` (Date.now()'s) has passed: `ms` is the whole
 * bound, for the words. The timer is set again for what is left where it fires
 * before the deadline by that clock: Node counts a timer by its monotonic
 * clock in whole milliseconds, and the time left was read in whole ones too,
 * so a 300 ms bound fired at 299 by Date.now() (the Linux arm64 runner).
 */
function askOne(path: string, deadline: number, ms: number, uid: number): Promise<StoreAnswer> {
	return new Promise((resolve) => {
		let socket: Socket | undefined;
		let done = false;
		let authed = false;
		let pending = Buffer.alloc(0);
		const finish = (a: StoreAnswer) => {
			if (done) return;
			done = true;
			clearTimeout(timer);
			socket?.destroy();
			resolve(a);
		};
		const late = () => {
			const left = deadline - Date.now();
			if (left > 0) {
				timer = setTimeout(late, left);
				return;
			}
			finish({ ok: false, why: authed ? "no-answer" : "no-bus", detail: `${authed ? `${SECRET_SERVICE} did not answer` : "the session bus did not answer"} within ${(ms / 1000).toFixed(1)} s` });
		};
		let timer = setTimeout(late, Math.max(0, deadline - Date.now()));
		const CALL = 2;
		try {
			socket = connect({ path });
		} catch (e) {
			finish({ ok: false, why: "no-bus", detail: `the session bus at ${path.replace("\0", "@")}: ${(e as Error).message}` });
			return;
		}
		const s = socket;
		s.on("error", (e) => finish({ ok: false, why: authed ? "no-answer" : "no-bus", detail: `the session bus at ${path.replace("\0", "@")}: ${e.message}` }));
		s.on("close", () => finish({ ok: false, why: authed ? "no-answer" : "no-bus", detail: "the session bus closed the connection" }));
		s.on("connect", () => s.write(Buffer.concat([Buffer.from([0]), Buffer.from(`AUTH EXTERNAL ${externalAuth(uid)}\r\n`, "ascii")])));
		s.on("data", (chunk: Buffer) => {
			pending = Buffer.concat([pending, chunk]);
			if (!authed) {
				const eol = pending.indexOf("\r\n");
				if (eol < 0) return;
				const line = pending.toString("ascii", 0, eol);
				pending = pending.subarray(eol + 2);
				if (!line.startsWith("OK ")) return finish({ ok: false, why: "no-bus", detail: `the session bus refused this user: ${line}` });
				authed = true;
				// Hello first, as a bus asks of every connection; the call after it.
				s.write(
					Buffer.concat([
						Buffer.from("BEGIN\r\n", "ascii"),
						methodCall(1, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "Hello"),
						methodCall(CALL, SECRET_SERVICE, "/org/freedesktop/secrets", "org.freedesktop.Secret.Service", "ReadAlias", ["default"]),
					]),
				);
			}
			const { messages, used } = readMessages(pending);
			pending = pending.subarray(used);
			for (const m of messages) {
				if ((m.type === METHOD_RETURN || m.type === ERROR) && m.replySerial === CALL) return finish(answerOf(m));
			}
		});
	});
}

/** The words a refusal says, by why the store could not be asked. */
export function storeRefusal(a: Extract<StoreAnswer, { ok: false }>): string {
	switch (a.why) {
		case "no-bus":
			return `There is no session bus to reach the system's credential store on (${a.detail}), so no token can be kept or read. Start the app from a desktop session.`;
		case "no-service":
			return `No credential store answers on the session bus (${a.detail}): the app keeps tokens in a Secret Service, GNOME Keyring or KWallet, and in no file. Install or start one, then try again.`;
		case "no-answer":
			return `The system's credential store did not answer (${a.detail}), so no token was kept or read. Start GNOME Keyring or KWallet in this session, then try again.`;
		case "no-default":
			return "The system's credential store has no default keyring (the login keyring) to keep tokens in, so no token was kept or read. Make one in Passwords and Keys, or log in again, then try again.";
	}
}

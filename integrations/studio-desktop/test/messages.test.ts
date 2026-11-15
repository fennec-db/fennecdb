import assert from "node:assert/strict";
import { test } from "node:test";
import {
	alreadyHereMessage,
	diedNotice,
	emptyFile,
	heldNotice,
	internalError,
	missingFile,
	notADatabase,
	outOfStepNotice,
	portTakenNotice,
	remoteNotice,
	startFailedNotice,
	startFailure,
} from "../src/bun/messages.ts";
import type { Probe } from "../src/bun/remote.ts";

const origin = "https://db.example.com";
const failures: Exclude<Probe, { kind: "ok" }>[] = [
	{ kind: "unreachable", reason: "nothing is listening at that address" },
	{ kind: "no-studio", status: 404 },
	{ kind: "token-refused", detail: "invalid token", hadToken: true },
	{ kind: "token-refused", detail: "invalid token", hadToken: false },
	{ kind: "redirected", status: 302, to: "https://login.example.com" },
];

test("a file another program holds says whose it is and what to do", () => {
	const n = heldNotice("orders.fennec", "could not open /x/orders.fennec: it is open already, in this process or another");
	assert.match(n.title, /open in another program/);
	assert.match(n.message, /orders\.fennec/);
	assert.match(n.hint ?? "", /corrupt/);
	assert.match(n.detail ?? "", /it is open already/);
	assert.deepEqual(n.actions?.map((a) => a.id), ["retry"]);
});

test("a server that died names its exit and keeps what it said", () => {
	assert.match(diedNotice("orders.fennec", 137, "boom").message, /code 137/);
	assert.match(diedNotice("orders.fennec", null, "").message, /signal/);
	assert.equal(diedNotice("orders.fennec", 1, "a\nb\nc\nd\ne\nf\ng\nh\ni\nj").detail, "c\nd\ne\nf\ng\nh\ni\nj");
	assert.deepEqual(diedNotice("orders.fennec", 1, "").actions?.map((a) => a.id), ["retry"]);
});

test("each way a saved server can fail has its own words and a way forward", () => {
	const titles = failures.map((p) => remoteNotice("prod", origin, p).title);
	assert.equal(new Set(titles).size, titles.length, titles.join(" | "));
	for (const p of failures) {
		const n = remoteNotice("prod", origin, p);
		assert.match(n.message, /prod/);
		assert.match(n.message, /db\.example\.com/);
		assert.ok(n.hint, `${p.kind} says what to do`);
	}
	assert.match(remoteNotice("prod", origin, failures[1]!).hint!, /--studio/);
	assert.match(remoteNotice("prod", origin, failures[0]!).message, /nothing is listening/);
});

test("a redirect names where it points and says the token is not sent there", () => {
	const n = remoteNotice("prod", origin, failures[4]!);
	assert.match(n.message, /redirect \(HTTP 302\) to https:\/\/login\.example\.com/);
	assert.match(n.hint ?? "", /only to the address it was saved for/);
	assert.deepEqual(n.actions?.map((a) => a.id), ["edit"]);
});

test("a file picked by mistake is told what it is, before any server is started on it", () => {
	assert.match(emptyFile("a.fennec").message, /nothing in it/);
	assert.match(emptyFile("a.fennec").hint ?? "", /New Database/);
	const other = notADatabase("notes.txt");
	assert.match(other.title, /not a fennecdb database/);
	assert.match(other.hint ?? "", /nothing was written beside it/i);
	assert.equal(other.actions, undefined);
});

test("a refused token and a missing one are not the same message, and both offer the form", () => {
	const had = remoteNotice("prod", origin, failures[2]!);
	const none = remoteNotice("prod", origin, failures[3]!);
	assert.match(had.message, /did not accept the token/);
	assert.match(none.message, /none is saved/);
	for (const n of [had, none]) assert.deepEqual(n.actions?.map((a) => a.id), ["edit"]);
});

test("a file that is some other kind of file says so, and that nothing was changed", () => {
	const n = startFailedNotice("notes.txt", "could not open /x/notes.txt: corrupt: invalid fennecdb signature");
	assert.match(n.title, /not a fennecdb database/);
	assert.match(n.hint ?? "", /Nothing was changed/);
	assert.equal(n.actions, undefined, "trying again would not help");
});

test("a server that refuses the app's flags says the app and its server are out of step, and what to do", () => {
	const n = outOfStepNotice("a.fennec", "unknown option: --exit-on-stdin-close\n\nusage: fennec-server ...");
	assert.match(n.title, /out of step/);
	assert.match(n.hint ?? "", /--exit-on-stdin-close/);
	assert.match(n.hint ?? "", /0\.1\.12/);
	assert.match(n.detail ?? "", /unknown option/);
	assert.equal(n.actions, undefined, "trying again would not help");
});

test("a port found taken before any server started has no server's words; one the server could not listen on has them", () => {
	const before = portTakenNotice("a.fennec", 8080, null);
	assert.equal(before.detail, undefined, "the app's own finding is not filed under what the server said");
	assert.deepEqual(before.actions?.map((a) => a.id), ["settings"]);
	const after = portTakenNotice("a.fennec", 8080, "opened: /x/a.fennec\ncould not open the HTTP endpoint: Address already in use (os error 48)");
	assert.match(after.detail ?? "", /Address already in use/);
});

test("a served database that did not start keeps why, in words, with the server's own last lines where one wrote them", () => {
	const early = startFailure("a.fennec", { kind: "port", port: 8080, said: null });
	assert.match(early.reason, /^Port 8080 is in use: something else answers on it, so no server was started\. /);
	assert.match(early.reason, /or free 8080\.$/);
	assert.equal(early.said, null, "no server ran, so nothing is the server's");
	assert.equal(early.port, 8080, "the port was the reason: changing it is the way on");

	const late = startFailure("a.fennec", { kind: "port", port: 8080, said: "opened: /x/a.fennec\ncould not open the HTTP endpoint: Address already in use (os error 48)" });
	assert.match(late.reason, /^Port 8080 is in use by another program, so its server could not listen there\./);
	assert.match(late.said ?? "", /Address already in use \(os error 48\)$/, "its error the last line");

	const held = startFailure("a.fennec", { kind: "held", said: "could not open /x/a.fennec: it is open already, in this process or another" });
	assert.match(held.reason, /a\.fennec is open in another program/);
	assert.equal(held.port, null, "a port would not help");
	assert.match(held.said ?? "", /it is open already/);

	assert.match(startFailure("a.fennec", { kind: "failed", exit: 2, said: "unknown option", message: "unknown option" }).reason, /built apart/);
	assert.match(startFailure("a.fennec", { kind: "failed", exit: 1, said: "bad magic", message: "bad magic" }).reason, /could not open a\.fennec \(exit 1\)/);
	assert.match(startFailure("a.fennec", { kind: "failed", exit: null, said: "opened: /x/a.fennec", message: "opened: /x/a.fennec" }).reason, /did not say where it listens in time/);
	const found = startFailure("a.fennec", { kind: "failed", exit: null, said: null, message: "fennec-server says it listens on http://10.0.0.1:1, not on 127.0.0.1 as it was asked to" });
	assert.equal(found.reason, "fennec-server says it listens on http://10.0.0.1:1, not on 127.0.0.1 as it was asked to.");
	assert.equal(found.said, null, "the app's finding, not the server's words");

	const long = startFailure("a.fennec", { kind: "held", said: "1\n2\n3\n4\n5\n6\n7\n8\n9\n10" });
	assert.equal(long.said, "3\n4\n5\n6\n7\n8\n9\n10", "its last eight lines, as a notice keeps");
});

test("an error of the app's own is not called what the server said", () => {
	const n = internalError("The window could not be opened.", new Error("boom"));
	assert.equal(n.detail, "boom");
	assert.match(n.detailTitle ?? "", /error/);
	assert.equal(heldNotice("a.fennec", "x").detailTitle, undefined, "the server's words keep the notice's own summary");
});

test("the rest", () => {
	assert.match(startFailedNotice("a.fennec", "bad magic").detail ?? "", /bad magic/);
	assert.match(missingFile("/gone.fennec").message, /gone\.fennec/);
	assert.match(alreadyHereMessage("a.fennec"), /already open in a window of this app/);
});

import assert from "node:assert/strict";
import { test } from "node:test";
import { LOAD_TRIES, loadAgainIfDropped } from "../src/bun/firstload.ts";

// A first load the engine dropped is loaded again (src/bun/firstload.ts). The
// running app's half is the Linux checks, where it happened; here the rule, on
// every system: what tells a dropped load from one that loaded, and how often.

/** A view as the guard sees it: its handlers by event, and the pages it was told to load. */
function fakeView() {
	const handlers: Record<string, (() => void)[]> = {};
	const loads: string[] = [];
	return {
		view: { on: (e: "did-commit-navigation" | "did-navigate", h: () => void) => void (handlers[e] ??= []).push(h), loadURL: (url: string) => void loads.push(url) },
		fire: (e: string) => (handlers[e] ?? []).forEach((h) => h()),
		handlers,
		loads,
	};
}

const URL_ = "http://127.0.0.1:41475/_studio/";

test("a load that ended with nothing committed is loaded again, the page the window was made for", () => {
	const v = fakeView();
	const said: number[] = [];
	loadAgainIfDropped(v.view, URL_, (n) => said.push(n), "linux");
	v.fire("did-navigate");
	assert.deepEqual(v.loads, [URL_]);
	assert.deepEqual(said, [1]);
	v.fire("did-commit-navigation");
	v.fire("did-navigate");
	assert.deepEqual(v.loads, [URL_], "the load again committed: nothing more");
});

test("a load that committed is left alone, and so is every load after it", () => {
	const v = fakeView();
	loadAgainIfDropped(v.view, URL_, () => {}, "linux");
	v.fire("did-commit-navigation");
	v.fire("did-navigate");
	v.fire("did-navigate");
	assert.deepEqual(v.loads, []);
});

test("a page whose loads are all dropped is loaded again LOAD_TRIES times, not for ever", () => {
	const v = fakeView();
	loadAgainIfDropped(v.view, URL_, () => {}, "linux");
	for (let i = 0; i < LOAD_TRIES + 3; i++) v.fire("did-navigate");
	assert.equal(v.loads.length, LOAD_TRIES);
});

test("a view whose window closed under its first load is not loaded again", () => {
	const v = fakeView();
	loadAgainIfDropped(v.view, URL_, () => {}, "linux");
	(v.view as { isRemoved?: boolean }).isRemoved = true;
	v.fire("did-navigate");
	assert.deepEqual(v.loads, []);
});

test("on macOS and Windows a did-navigate is a load that succeeded: nothing is watched", () => {
	for (const platform of ["darwin", "win32"] as const) {
		const v = fakeView();
		loadAgainIfDropped(v.view, URL_, () => {}, platform);
		assert.deepEqual(v.handlers, {}, platform);
	}
});

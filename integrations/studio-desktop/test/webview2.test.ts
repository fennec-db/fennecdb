import assert from "node:assert/strict";
import { test } from "node:test";
import vm from "node:vm";
import { guardPreload, keysToPage, movesFocus, PRELOAD_CHECK, PRELOAD_TRIES, webview2Variables, type KeyFocus } from "../src/bun/webview2.ts";

// What the app asks of WebView2 that Electrobun 2.0.4 does not. The running
// app's half is held on the Windows runners (checks/latepreload.mjs makes a
// page load without its bridge, as WebView2's race leaves one); here the
// guard's own rule, on every system.

/** A view as the guard sees it: its did-navigate handlers, and the scripts it was asked to run. */
function fakeView() {
	const handlers: (() => void)[] = [];
	const ran: string[] = [];
	return {
		view: { on: (_e: "did-navigate", h: () => void) => void handlers.push(h), executeJavascript: (js: string) => void ran.push(js) },
		navigate: () => handlers.forEach((h) => h()),
		ran,
		handlers,
	};
}

test("on Windows a page is asked for its preload after each of its first loads, and no more than PRELOAD_TRIES", () => {
	const v = fakeView();
	guardPreload(v.view, "win32");
	for (let i = 0; i < PRELOAD_TRIES + 3; i++) v.navigate();
	assert.equal(v.ran.length, PRELOAD_TRIES, "a page that never gets its preload is not loaded for ever");
	assert.ok(v.ran.every((js) => js === PRELOAD_CHECK));
});

test("elsewhere WebKit adds the script before the first load: nothing is asked", () => {
	for (const platform of ["darwin", "linux"] as const) {
		const v = fakeView();
		guardPreload(v.view, platform);
		assert.equal(v.handlers.length, 0, platform);
	}
});

test("the check loads a page again where Electrobun's bridge is missing, and leaves one that has it", () => {
	const run = (bridge: unknown) => {
		let reloads = 0;
		const window: Record<string, unknown> = { __electrobunSendToHost: bridge };
		vm.runInNewContext(PRELOAD_CHECK, { window, location: { reload: () => reloads++ } });
		return reloads;
	};
	assert.equal(run(undefined), 1, "no bridge: loaded again");
	assert.equal(run("not a function"), 1, "a page's own value is not the bridge");
	assert.equal(run(() => {}), 0, "the bridge: left alone");
});

test("every WEBVIEW2_ variable the app was started with is taken out, its name read case-blind", () => {
	const env = {
		Path: "C:\\Windows",
		WEBVIEW2_USER_DATA_FOLDER: "C:\\one-store-for-all",
		webview2_additional_browser_arguments: "--remote-debugging-port=9222",
		WebView2_Pipe_For_Script_Debugger: "x",
		NOT_WEBVIEW2_X: "stays",
	};
	assert.deepEqual(webview2Variables(env).sort(), ["WEBVIEW2_USER_DATA_FOLDER", "WebView2_Pipe_For_Script_Debugger", "webview2_additional_browser_arguments"].sort());
	assert.deepEqual(webview2Variables({ Path: "x" }), []);
});

/** A window and its page as `keysToPage` sees them: their handlers, and how often the window was activated. */
function fakeWindow() {
	const on: Record<string, (() => void)[]> = { focus: [], "did-navigate": [] };
	const w = {
		activated: 0,
		win: { on: (e: "focus", h: () => void) => void on[e]!.push(h), activate: () => void w.activated++ },
		view: { on: (e: "did-navigate", h: () => void) => void on[e]!.push(h) },
		fire: async (e: "focus" | "did-navigate") => {
			for (const h of on[e]!) h();
			// The handler asks for the focus asynchronously (win32.ts is loaded on first use).
			await new Promise((r) => setImmediate(r));
		},
		on,
	};
	return w;
}

const at = (f: Partial<KeyFocus>): KeyFocus => ({ front: true, menu: false, onWindow: true, ...f });

test("the focus is moved into the page only from a window in front, with no menu open, where no page has it", () => {
	assert.equal(movesFocus(at({})), true, "the bare window, in front: moved");
	assert.equal(movesFocus(at({ front: false })), false, "a window behind another program's is never brought forward");
	assert.equal(movesFocus(at({ menu: true })), false, "a menu that has the input is left to it");
	assert.equal(movesFocus(at({ onWindow: false })), false, "a page that has it keeps it");
	assert.equal(movesFocus(null), false, "a window that is gone");
});

test("on Windows a window's page is given the focus once it loads and at each activation, and only then", async () => {
	const w = fakeWindow();
	let focus = at({});
	const said: boolean[] = [];
	keysToPage(w.win, w.view, () => focus, (_f, moved) => said.push(moved), "win32");
	await w.fire("did-navigate");
	assert.equal(w.activated, 1, "the page loaded, the window in front, the focus on the bare window");
	focus = at({ onWindow: false });
	await w.fire("focus");
	assert.equal(w.activated, 1, "the page has the focus: nothing is done");
	focus = at({ front: false });
	await w.fire("focus");
	await w.fire("did-navigate");
	assert.equal(w.activated, 1, "a window behind: nothing is done, so nothing is brought forward");
	focus = at({});
	await w.fire("focus");
	assert.equal(w.activated, 2, "activated by the user, the focus on the bare window: moved into the page");
	assert.deepEqual(said, [true, false, false, false, true], "every look is said, moved or not");
});

test("a window closed before its look is no failure", async () => {
	const w = fakeWindow();
	w.win.activate = () => {
		throw new Error("Can't activate window. Window no longer exists");
	};
	const said: boolean[] = [];
	keysToPage(w.win, w.view, () => at({}), (_f, moved) => said.push(moved), "win32");
	await w.fire("focus");
	assert.deepEqual(said, [true]);
});

test("macOS and Linux leave a window's focus to their toolkit: nothing is listened for", () => {
	for (const platform of ["darwin", "linux"] as const) {
		const w = fakeWindow();
		keysToPage(w.win, w.view, () => at({}), undefined, platform);
		assert.equal(w.on["focus"]!.length + w.on["did-navigate"]!.length, 0, platform);
	}
});

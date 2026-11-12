import assert from "node:assert/strict";
import { test } from "node:test";
import vm from "node:vm";
import { guardPreload, PRELOAD_CHECK, PRELOAD_TRIES, webview2Variables } from "../src/bun/webview2.ts";

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

// What the app asks of WebView2 on Windows that Electrobun 2.0.4 does not:
// none of WebView2's variables from the environment the app was started from,
// and a page that loaded before its preload loaded again. Pure: `node --test`
// holds it on every system (test/webview2.test.ts); the running app's half is
// checks/latepreload.mjs on the Windows runners.

/**
 * The WEBVIEW2_* variables of `env` (names case-blind, as Windows reads them),
 * which the app takes out of its own environment before its first window:
 * what WebView2 is started with is Electrobun's and the app's, not the
 * environment the app was started from. WebView2 reads them from the app's
 * process's environment block when a view is made (where the app is not
 * elevated; an elevated one it ignores), which the app's `process.env` is not:
 * it runs in a Worker, whose copy is its own (win32.ts's `unsetEnvironment`).
 * WEBVIEW2_USER_DATA_FOLDER would put every window
 * in one store, the one thing the partitions are for (stores.ts);
 * WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS could add `--remote-debugging-port`,
 * handing every page, and the token a studio window holds, to any program of
 * the machine; WEBVIEW2_PIPE_FOR_SCRIPT_DEBUGGER hands a page to a debugger.
 */
export function webview2Variables(env: Record<string, string | undefined>): string[] {
	return Object.keys(env).filter((k) => /^webview2_/i.test(k));
}

/** How many times a page is asked for its preload: its first load and two loads again. */
export const PRELOAD_TRIES = 3;

/**
 * Run in a page once it navigated, through ExecuteScript (which needs no
 * preload): loads the page again if Electrobun's bridge, which its preload
 * defines, is not there.
 */
export const PRELOAD_CHECK = `if(typeof window.__electrobunSendToHost!=="function")location.reload();`;

/** The part of an Electrobun view the guard uses. */
export type GuardedView = { on(event: "did-navigate", handler: () => void): void; executeJavascript(js: string): void };

/**
 * Windows: a page that loaded without its preload is loaded again. Electrobun
 * 2.0.4 adds a view's preload (AddScriptToExecuteOnDocumentCreated, which
 * completes later) and navigates in the same call, without waiting for it
 * (win/nativeWrapper.cpp:7828 and 8077), so the first document can load
 * before the script is there: on the x64 runner a saved server's window,
 * opened again at once, loaded with no event bridge (and so no session and
 * no guard for new windows) and never reported. Once it has navigated the
 * page is asked through ExecuteScript whether Electrobun's bridge is there,
 * and loaded again if not, the preload being in place by then; at most
 * PRELOAD_TRIES times a view, so one that never gets it is not loaded for
 * ever. WebKit (macOS, Linux) takes a user script at once (its user content
 * controller), with nothing to complete later: nothing is asked there.
 */
export function guardPreload(view: GuardedView, platform: NodeJS.Platform = process.platform): void {
	if (platform !== "win32") return;
	let asked = 0;
	view.on("did-navigate", () => {
		if (++asked > PRELOAD_TRIES) return;
		view.executeJavascript(PRELOAD_CHECK);
	});
}

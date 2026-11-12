// What the app asks of WebView2 on Windows that Electrobun 2.0.4 does not:
// none of WebView2's variables from the environment the app was started from,
// a page that loaded before its preload loaded again, and the keyboard focus
// in a window's page. Pure: `node --test` holds it on every system
// (test/webview2.test.ts); the running app's half is checks/latepreload.mjs
// and checks/menu.mjs on the Windows runners.

/**
 * The WEBVIEW2_* variables of `env` (names case-blind, as Windows reads them),
 * which the app takes out of its own environment before its first window:
 * what WebView2 is started with is Electrobun's and the app's, not the
 * environment the app was started from. WebView2 reads them from the app's
 * process's environment block when a view is made (where the app is not
 * elevated; an elevated one it ignores), which a delete from the app's
 * `process.env` did not reach on the Windows runners (win32.ts's
 * `unsetEnvironment`).
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

/** Where a window's keyboard focus is, as Windows says (win32.ts's `keyFocus`). */
export type KeyFocus = {
	/** The window is the one in front. */
	front: boolean;
	/** A menu bar, a popup menu or the system menu has its thread's input. */
	menu: boolean;
	/** The focus is on a window of the app's own process, or on none: in no page. */
	onWindow: boolean;
};

/** Whether the focus is moved into the page: a window in front, no menu open, the focus in no page. */
export const movesFocus = (f: KeyFocus | null): boolean => f !== null && f.front && !f.menu && f.onWindow;

/** The part of an Electrobun window `keysToPage` uses. */
export type FocusedWindow = { on(event: "focus" | "close", handler: () => void): void; activate(): void };

/** Why the app looked at a window's focus: its page loaded, it was activated, or it came forward from behind. */
export type FocusLook = "loaded" | "activated" | "forward";

/** How often a window behind another program's is looked at, until it comes forward. */
export const BEHIND_LOOK_MS = 100;

/** The timer `keysToPage` watches a window behind with (setInterval's, or a test's). */
export type Every = { set(f: () => void, ms: number): unknown; clear(t: unknown): void };
const EVERY: Every = { set: (f, ms) => setInterval(f, ms), clear: (t) => clearInterval(t as ReturnType<typeof setInterval>) };

/**
 * Windows: the keyboard focus put in the page of `win` (its view `view`) once
 * the page has loaded, each time the window is activated, and when a window
 * that was behind another program's comes forward, where the window is in
 * front and the focus is in no page. Activating a window, Windows'
 * DefWindowProc puts the focus on the window itself (WM_ACTIVATE), and a
 * WebView2 page is a child window of another process, which takes it only
 * when the host moves it there (the controller's MoveFocus); Electrobun 2.0.4
 * does that only in `activate()` (activateWindow's FocusActiveView,
 * win/nativeWrapper.cpp:11496), not on WM_ACTIVATE (:6074) nor once a view is
 * made. On the Windows arm64 runner the main window, in front, took typed keys
 * into nothing: the system took them, the focus was on the bare window
 * (GetGUIThreadInfo), and the page saw none until `activate()`, after which it
 * saw all of them. `activate()` also brings the window forward, by
 * AttachThreadInput to the thread in front where SetForegroundWindow is
 * refused, so it is asked only of a window already in front and never brings
 * one forward: a window behind another program's stays there, and its page is
 * given the focus when the user brings it forward. That needs a look of its
 * own: a window Electrobun made the active one of its thread while another
 * program's was in front (its creation's SetActiveWindow, where the foreground
 * was refused) gets no WM_ACTIVATE when a click on it brings it forward, as on
 * the arm64 runner, where the Start menu was open when the main window opened:
 * clicked by its title bar, it came to the front with no "focus" event, its
 * focus on the bare window. So a window found behind is looked at every
 * BEHIND_LOOK_MS until it is in front or closed, and no longer once it is.
 * A menu that has the input is left to it. Not on macOS and Linux: there the
 * page is the window's own view, which AppKit makes its first responder and
 * GTK gives the window's focus, and `activate()` would bring the app forward
 * over another.
 */
export function keysToPage(
	win: FocusedWindow,
	view: { on(event: "did-navigate", handler: () => void): void },
	look: () => KeyFocus | null | Promise<KeyFocus | null>,
	said: (f: KeyFocus | null, moved: boolean, why: FocusLook) => void = () => {},
	platform: NodeJS.Platform = process.platform,
	every: Every = EVERY,
): void {
	if (platform !== "win32") return;
	let behind: unknown = null;
	let closed = false;
	const stopWatching = () => {
		if (behind !== null) every.clear(behind);
		behind = null;
	};
	const give = async (why: FocusLook) => {
		const f = await look();
		const moved = movesFocus(f);
		if (moved) {
			try {
				win.activate();
			} catch {
				// The window closed since: there is no page to give the focus to.
			}
		}
		said(f, moved, why);
		if (f === null || f.front || closed) stopWatching();
		else if (behind === null) {
			behind = every.set(() => {
				void (async () => {
					const now = await look();
					if (now === null) return stopWatching();
					if (now.front && behind !== null) {
						stopWatching();
						await give("forward");
					}
				})();
			}, BEHIND_LOOK_MS);
		}
	};
	view.on("did-navigate", () => void give("loaded"));
	win.on("focus", () => void give("activated"));
	win.on("close", () => {
		closed = true;
		stopWatching();
	});
}

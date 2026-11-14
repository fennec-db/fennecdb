// A studio window's first load that the engine dropped, loaded again.
//
// On Linux the first load of a studio window's page sometimes ends a few
// milliseconds after the window's rules allowed it, before anything of the
// page was committed: WebKitGTK cancels it ("Load request cancelled",
// WebKitNetworkError 302, as it started; caught once by a shim on the x64
// runner, a saved server's window opened again 150 ms after it closed), and
// Electrobun 2.0.4 hands the app nothing but the `did-navigate` its
// load-changed FINISHED sends after it (its load-failed event has no handler
// in the main process). WebKit draws no error page for a cancelled load, so
// the window stayed blank for good, and every check waiting for its studio
// timed out (seven times on the Linux runners in two days, each the same:
// did-navigate 5 to 22 ms after will-navigate, no did-commit-navigation
// between). A load that ended so is the event this waits for, never a time:
// the page is loaded again, at most LOAD_TRIES times a window, and only until
// a document of it has committed, after which a load is the page's own. Pure:
// `node --test` holds it on every system (test/firstload.test.ts);
// `checks/firstload.mjs` holds the app to it with a first load made to commit
// nothing.

/** How many times a window's first page is loaded again after a load that committed nothing. */
export const LOAD_TRIES = 3;

/** The part of an Electrobun view this uses. */
export type FirstLoadView = {
	on(event: "did-commit-navigation" | "did-navigate", handler: () => void): void;
	loadURL(url: string): void;
};

/**
 * Loads `url` in `view` again each time a load of it ends with nothing
 * committed, until one commits, at most LOAD_TRIES times; `said` hears each
 * load again (its number). Linux alone, the one system the drop was seen on,
 * and where Electrobun's did-navigate comes for a failed load too. On Windows
 * a failed navigation raises no event (Electrobun drops a NavigationCompleted
 * that did not succeed): nothing there to hear it by.
 */
export function loadAgainIfDropped(view: FirstLoadView, url: string, said: (tries: number) => void = () => {}, platform: NodeJS.Platform = process.platform): void {
	if (platform !== "linux") return;
	let committed = false;
	let tries = 0;
	view.on("did-commit-navigation", () => {
		committed = true;
	});
	view.on("did-navigate", () => {
		if (committed || tries >= LOAD_TRIES) return;
		said(++tries);
		view.loadURL(url);
	});
}

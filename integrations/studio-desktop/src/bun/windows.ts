// The windows a page of someone else's can be in, and the app's own notice.
//
// A studio window shows a page a *server* serves (the one this app started,
// or a saved remote one). The studio is "a page with no authority of its own":
// it holds a token and sends it. So its window:
//   - is `sandbox: true`: no Electrobun RPC to the main process at all;
//   - is held to the server's origin by navigation rules, and a link out
//     (`window.open`, target=_blank) goes to the default browser instead;
//   - learns its token from its preload, never from a URL (session.ts).

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { BrowserView, BrowserWindow, PATHS, Session } from "electrobun/main";
import type { Notice } from "../shared/rpc.ts";
import { LINK_OUT, linkOut, newWindowPreload, openLink } from "./opener.ts";
import { sessionPreload, type StudioSession } from "./session.ts";
import { goesWithWindow, inUse, removeStoreFolder, storeClosed, storeOpened, sweepStores, webview2Root } from "./stores.ts";
import { guardPreload, keysToPage } from "./webview2.ts";

let cascade = 0;

/** Every window of the app open now, by id: the main window, a new-database window, a notice, a studio window. */
const shown = new Set<number>();
let lastClosed: () => void = () => {};

/**
 * Counts `win` among the app's windows until it closes; the last one closing
 * calls what `onLastWindowClosed` was given. Every window the app makes is
 * counted (pages.ts, and here), or closing the others would quit the app
 * under it on Windows and Linux. Called on the next turn, after whatever
 * closed it has run on: a notice's Try again closes the notice, then opens the
 * file, which is in `starting` from then on.
 */
export function counted(win: BrowserWindow<any>): void {
	shown.add(win.id);
	win.on("close", () => {
		shown.delete(win.id);
		if (shown.size === 0) setImmediate(() => shown.size === 0 && lastClosed());
	});
}

/**
 * Windows: the keyboard focus put in `view`'s page, the page `win` shows, once
 * it has loaded, whenever the window is activated and when it comes forward
 * from behind another program's, where the window is in front and the focus
 * is in no page; never by bringing a window forward (webview2.ts's
 * `keysToPage`, `checks/menu.mjs`). Every window the app makes is given it, as
 * every one is `counted`: each takes keys in its page.
 */
export function focusesPage(win: BrowserWindow<any>, view: BrowserView): void {
	keysToPage(
		win,
		view as any,
		async () => {
			const ptr = win.ptr;
			return ptr ? (await import("./win32.ts")).keyFocus(BigInt(ptr as unknown as number)) : null;
		},
		__TESTABLE__ ? (f, moved, why) => void import("./testhooks.ts").then((h) => h.pageFocus(win, f, moved, why)) : undefined,
	);
}

/** What the app does once its last window has closed (index.ts: quit, on Windows and Linux). */
export function onLastWindowClosed(f: () => void): void {
	lastClosed = f;
}

/** How many windows the app has open. */
export const windowsShown = () => shown.size;

/**
 * A window held to one address: every navigation anywhere else is refused
 * (`^*` denies all, the second rule allows `pattern`, a glob). The whole of
 * the defence for a window that has the RPC bridge, the launcher and the
 * new-database window, which Electrobun's full preload adds to *every*
 * document the window loads: a link or a file dropped on it is a navigation,
 * and what it loaded would be handed `openPath`, `openConnection` and the
 * saved servers. So no window of the app is without these (`checks/pages.mjs`
 * holds each one).
 *
 * The constructor's `navigationRules` option did not take in Electrobun 2.0.4
 * (a navigation to https://example.com reported "allowed": true with it set);
 * the setter does.
 */
export function holdTo(view: BrowserView, pattern: string) {
	view.setNavigationRules(["^*", pattern]);
	// Windows: a page that loaded before its preload is loaded again (webview2.ts).
	// The test variant can be told not to, as the control of checks/latepreload.mjs.
	if (__TESTABLE__ && process.env["FENNEC_DESKTOP_NO_PRELOAD_GUARD"] === "1") return;
	guardPreload(view as any);
}

function frame(width: number, height: number) {
	const o = 40 * (cascade++ % 8);
	return { x: 140 + o, y: 90 + o, width, height };
}

/** What a webview event carries, whichever way this Electrobun wraps it. */
const detailOf = (e: any): any => e?.data?.detail ?? e?.data;

/**
 * A link out of a studio window goes to the default browser if `linkOut` lets
 * it (a web address, never a file: or a custom scheme; one in three seconds a
 * window), and never through a shell (opener.ts). The test variant reports it
 * instead, and hands it on as well only when a check asks it to
 * (`checks/opener.mjs`).
 */
async function openExternal(url: string, last: { at: number }) {
	const link = linkOut(url, last);
	if (!link) return;
	if (__TESTABLE__) return void (await import("./testhooks.ts")).external(link);
	await openLink(link);
}

/**
 * What a store holds that taking it away must take: Electrobun 2.0.4's native
 * `sessionClearStorageData` on macOS and Linux knows these six names and
 * nothing else, and the "all" its TypeScript sends (as `["all"]`) matches none
 * of them, so it does nothing. (The type has no `serviceWorkers` and a `webSQL`
 * the native code does not know, hence the cast.)
 */
const STORE_KINDS = ["cookies", "localStorage", "sessionStorage", "indexedDB", "cache", "serviceWorkers"] as never[];

/** Windows: where WebView2 keeps the app's stores (stores.ts), from the identifier and channel the build wrote. */
let webview2: string | null | undefined;
function webview2Folder(): string | null {
	if (webview2 === undefined) {
		let version: { identifier?: string; channel?: string } = {};
		try {
			version = JSON.parse(readFileSync(join(PATHS.RESOURCES_FOLDER, "version.json"), "utf8"));
		} catch {
			// no version.json: no folder known, and none is removed
		}
		webview2 = webview2Root(process.env, version);
	}
	return webview2;
}

/**
 * Takes away what a store holds once no window uses it: on Windows its folder
 * (stores.ts says why not Electrobun's call there), elsewhere a persistent
 * store's data by `STORE_KINDS` (one in memory goes with its window). True
 * once it is empty.
 */
export async function dropStore(partition: string): Promise<boolean> {
	let ok: boolean;
	if (process.platform === "win32") {
		const root = webview2Folder();
		ok = root ? await removeStoreFolder(root, partition) : false;
	} else {
		ok = !inUse(partition);
		if (ok && partition.startsWith("persist:")) Session.fromPartition(partition).clearStorageData(STORE_KINDS);
	}
	if (__TESTABLE__) (await import("./testhooks.ts")).report("store-dropped", { partition, ok });
	return ok;
}

/** Windows, by the one instance (index.ts): the stores a run that ended left (stores.ts). */
export function sweepLeftStores(): Promise<void> {
	const root = process.platform === "win32" ? webview2Folder() : null;
	return root ? sweepStores(root) : Promise.resolve();
}

/** A studio window: Electrobun's window, and the view that shows the server's page in it. */
export type StudioWindow = { win: BrowserWindow; view: BrowserView };

/**
 * A window on `<origin>/_studio/`, signed in as `session` (a server's token,
 * never a URL), in the data store `partition` names.
 *
 * Everything a page stores (localStorage, IndexedDB: the studio's query
 * history, its saved queries and its theme) lives in a WebKit data store, and
 * Electrobun's window makes its page's view in the one shared store, keyed by
 * origin alone: a file's server listens on a port the system picks, so a later
 * window on a port that was used before would show the earlier file's history.
 * So the window is made with no page, and the view is made in it with a store
 * of its own: `persist:remote-<id>` for a saved server (kept between runs, and
 * apart from every other saved server's), a name without `persist:` for a file
 * (a store that exists only for this window; on Windows a folder no other
 * window is given, stores.ts). The window's own view is removed on Windows
 * and Linux and kept on macOS (below). The partition is in use from before
 * the view is made until the window has closed, and no store is removed while
 * it is (stores.ts).
 */
export async function openStudioWindow(opts: { key: string; title: string; session: StudioSession; partition: string }): Promise<StudioWindow> {
	const { origin } = opts.session;
	// The origin is the pattern the window is held to, where `*` is a wildcard.
	if (/[*?]/.test(origin)) throw new Error(`not a server origin: ${origin}`);
	const hooks = __TESTABLE__ ? await import("./testhooks.ts") : null;
	// A hook that reports what the page shows is a preload too, and only the test variant has it.
	const extra = hooks?.pagePreload(origin) ?? "";
	const f = frame(1280, 820);
	const win = new BrowserWindow({ title: opts.title, url: null, html: null, frame: f, sandbox: true, allowedProtocols: { views: false, appData: false } });
	counted(win);
	// The window's own view, blank and in the shared store, under the page's.
	// Windows and Linux remove it: their Electrobun removes a view on its UI
	// thread, in order after making it. macOS keeps it. There Electrobun 2.0.4
	// makes a view's WKWebView in a block it queues on the main thread, and
	// `remove()` runs at once on this thread (Bun's): asked here, it mostly
	// removed nothing (the view, made after it, stayed: every first window of a
	// launch kept it so), and when it fell inside that block it took the view
	// from under it, a SIGSEGV in -[WKWebViewImpl initWithWebviewId:...]
	// _block_invoke (the finder check, once, on a hosted runner whose launcher
	// had warmed WebKit; a pause of a few ms before the removal does it here).
	// Removed once surely made (after the page's view, as the main thread runs
	// in order), it cost a warm open about 28 ms of 240 and freed no memory
	// (docs/design/integrations.md). Kept, it loads nothing, so the shared store
	// gets nothing from it, and it may navigate nowhere (`^*`, on the main
	// thread after it is made). `own-view` holds this (testhooks.ts, `waitStudio`).
	if (process.platform === "darwin") win.webview?.setNavigationRules(["^*"]);
	else win.webview?.remove();
	storeOpened(opts.partition);
	win.on("close", () => {
		storeClosed(opts.partition);
		// A file window's store goes with it: on Windows a folder, removed once WebView2 lets it go.
		if (goesWithWindow(opts.partition)) void dropStore(opts.partition);
	});
	const view = new BrowserView({
		url: `${origin}/_studio/`,
		preload: sessionPreload(opts.session) + newWindowPreload() + extra,
		sandbox: true,
		partition: opts.partition,
		windowId: win.id,
		frame: { x: 0, y: 0, width: f.width, height: f.height },
		autoResize: true,
		// A page a server serves is no reason to answer for views://, whose handler
		// sends `Access-Control-Allow-Origin: *`: it could read the app's own pages.
		allowedProtocols: { views: false, appData: false },
	});
	focusesPage(win, view);
	// Stay on the server's origin; everything else goes to the browser.
	holdTo(view, `${origin}/*`);
	const last = { at: 0 };
	(view as any).on("new-window-open", (e: any) => {
		const d = detailOf(e);
		const url = typeof d === "string" ? d : d?.url;
		if (typeof url === "string") void openExternal(url, last);
	});
	// Windows: the same request, taken from the page before WebView2 opens a window of its own (opener.ts).
	if (process.platform === "win32") {
		(view as any).on("host-message", (e: any) => {
			const d = detailOf(e);
			if (d?.type === LINK_OUT && typeof d.url === "string") void openExternal(d.url, last);
		});
	}
	const studio = { win, view };
	hooks?.ownView(win, opts.key);
	hooks?.watchStudio(studio, opts.key);
	return studio;
}

/** The notice window's preload: the notice, as data, for the page to draw with `textContent`. */
function noticePreload(n: Notice): string {
	return `window.__notice=${JSON.stringify(n)};`;
}

/**
 * A message in a window of its own. Not Utils.showMessageBox: that is a
 * synchronous call into the native layer (`returns: int`) inside an `async`
 * function, so the main process's JavaScript thread -- every server's
 * supervision, a SIGTERM handler, the launcher's RPC -- stands still until
 * someone clicks OK. A check that never clicked held the whole app for good.
 *
 * The page is bundled (views/notice) and sandboxed, so it has no RPC; its
 * buttons report themselves through the event-only bridge a sandboxed view
 * keeps. OK closes; any other button closes and calls `onAction`.
 */
export async function showNotice(n: Notice, onAction: (id: string) => void = () => {}): Promise<BrowserWindow> {
	const win = new BrowserWindow({
		title: n.title,
		url: "views://notice/index.html",
		preload: noticePreload(n) + newWindowPreload(),
		sandbox: true,
		frame: frame(540, 330),
	});
	counted(win);
	focusesPage(win, win.webview);
	holdTo(win.webview, "views://notice/*");
	(win.webview as any).on("host-message", (e: any) => {
		const d = detailOf(e);
		if (d?.type !== "notice") return;
		win.close();
		if (typeof d.id === "string" && d.id !== "ok") onAction(d.id);
	});
	if (__TESTABLE__) (await import("./testhooks.ts")).noticeShown(win, n);
	return win;
}

/** A window of the app's own, bundled, with an RPC bridge: the launcher and the new-database window. */
export function pageFrame(width: number, height: number) {
	return frame(width, height);
}

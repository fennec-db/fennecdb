// What the scripted checks (scripts/checks/) drive the app with. Bundled into
// the test variant only: every use is behind `__TESTABLE__`, which
// electrobun.config.ts sets true for FENNEC_DESKTOP_TEST_BUILD=1 and false in
// every other build, and the bundler drops what it guards. A shipped app
// reads none of these variables, runs no command from a file and reports
// nothing (scripts/checks/release.mjs holds the shipped bundle to it).
//
//   FENNEC_DESKTOP_REPORT  a file the app appends one JSON line a fact to
//   FENNEC_DESKTOP_CONTROL a file of JSON commands, one a line, read as it grows
//   FENNEC_DESKTOP_OPEN    files to open at start, joined by ":"
//   FENNEC_DESKTOP_LAUNCHER show the launcher at start
//
// A page reports through the event-only bridge a window keeps even when it is
// sandboxed (`__electrobunSendToHost`), which a server's page cannot reach
// the main process by.

import { appendFileSync, readFileSync } from "node:fs";
import { delimiter } from "node:path";
import { Utils, type BrowserWindow, type BrowserView } from "electrobun/main";
import type { Notice } from "../shared/rpc.ts";
import { secrets } from "./secrets.ts";
import { keychainName, sealToken } from "./token.ts";
import {
	backupNow,
	checkConnection,
	compactNow,
	createDatabase,
	figuresOf,
	forgetDatabase,
	forgetToken,
	makeKey,
	newServeToken,
	onFact,
	openConnection,
	openFile,
	remotePartition,
	removeConnection,
	removeServed,
	restartServed,
	restoreBackup,
	saveConnection,
	savedTokenOf,
	served,
	serveFile,
	serverLog,
	serveTokenOf,
	setArchive,
	stopServed,
	verifyBackups,
	windows,
} from "./app.ts";
import { DEFAULT_SETTINGS } from "./serving.ts";
import { revealPath, showLauncher, showNewDatabase } from "./pages.ts";
import { linkOut, openLink } from "./opener.ts";
import { filePartition } from "./stores.ts";
import { openStudioWindow, type StudioWindow } from "./windows.ts";

const T0 = Date.now();
const REPORT = process.env["FENNEC_DESKTOP_REPORT"] ?? "";
const CONTROL = process.env["FENNEC_DESKTOP_CONTROL"] ?? "";

/** One fact for the check that is watching. */
export function report(what: string, extra: Record<string, unknown> = {}) {
	if (!REPORT) return;
	appendFileSync(REPORT, JSON.stringify({ at: Date.now(), since: Date.now() - T0, what, ...extra }) + "\n");
}

const detailOf = (e: any): any => e?.data?.detail ?? e?.data;

/** Windows by the name a check addresses them: a studio window's key, `launcher`, `newdb`, `notice:<n>`. */
const pages = new Map<string, { win: BrowserWindow; view: BrowserView }>();
let notices = 0;

/**
 * A link out, which a check must not send the machine's browser to: reported,
 * and handed to the system's opener only where a check asks for that
 * (FENNEC_DESKTOP_REAL_OPEN=1, `checks/opener.mjs`, whose opener is a stand-in
 * or a browser that cannot start), with what the opener did.
 */
export async function external(url: string) {
	report("external", { url });
	if (process.env["FENNEC_DESKTOP_REAL_OPEN"] === "1") report("external-done", { url, done: await openLink(url) });
}

/**
 * What a page that loaded before its preload is left with, made on purpose
 * (FENNEC_DESKTOP_BRIDGELESS_FIRST=1, `checks/latepreload.mjs`): the first
 * document of each studio window loses Electrobun's bridge, as a document
 * WebView2 created before AddScriptToExecuteOnDocumentCreated landed has none.
 * `window.name` outlives a load of the same page, so the documents after it
 * keep theirs. WebView2's race itself cannot be had on demand.
 */
const BRIDGELESS_FIRST = `(function(){if(window.top!==window||window.name==="fennec-check-bridgeless")return;window.name="fennec-check-bridgeless";window.__electrobunSendToHost=undefined;})();`;

/** Reports what the studio in a window shows, until the check stops looking. */
export function pagePreload(origin: string): string {
	const o = JSON.stringify(origin);
	return [
		process.env["FENNEC_DESKTOP_BRIDGELESS_FIRST"] === "1" ? BRIDGELESS_FIRST : "",
		`(function(){if(location.origin!==${o})return;`,
		`var n=0,last="",t=setInterval(function(){n++;`,
		`var txt=document.body?document.body.innerText:"";`,
		`var signin=/Connect to a fennecdb server/.test(txt);`,
		// The collections and their counts come from the sidebar's own elements, not
		// the page's text: a narrow window slides the sidebar out of sight, and the
		// shell's own words ("Rows", "Schema") are there long before any data is.
		// "Loaded" is a collection with its count drawn.
		`var side=[].map.call(document.querySelectorAll("li.coll"),function(li){var n=li.querySelector(".coll-name"),c=li.querySelector(".coll-count");return (n?n.textContent:"")+" "+(c?c.textContent:"");}).join("|");`,
		`var loaded=!signin&&/\\d/.test(side);`,
		`var sig=signin+"/"+loaded+"/"+side+"/"+document.title;`,
		`if(sig!==last||n===1){last=sig;`,
		`window.__electrobunSendToHost&&window.__electrobunSendToHost({type:"state",n:n,at:Date.now(),signin:signin,loaded:loaded,side:side,title:document.title,text:txt.slice(0,300)});}`,
		`if(n>1200)clearInterval(t);},100);})();`,
	].join("");
}

/** Every navigation a window is asked to make, and what was decided: the rules a window is held to are checked by these. */
function watchNavigation(view: BrowserView, key: string) {
	// The commit too: a page that never loads is told apart from one that loaded and never drew.
	for (const name of ["will-navigate", "did-commit-navigation", "did-navigate", "new-window-open"]) {
		(view as any).on(name, (e: any) => report(name, { key, data: JSON.stringify(e?.data ?? e).slice(0, 200) }));
	}
}

export function watchStudio({ win, view }: StudioWindow, key: string) {
	pages.set(key, { win, view });
	(view as any).on("host-message", (e: any) => report("page", { key, detail: detailOf(e) }));
	watchNavigation(view, key);
	win.on("close", () => {
		report("window-close", { key });
		pages.delete(key);
	});
}

/**
 * Whether a studio window's own view (Electrobun's blank one, under the page's)
 * is still the window's once the window is made: kept on macOS, where removing
 * it raced the main thread making it and crashed the app; removed on Windows
 * and Linux (windows.ts).
 */
export function ownView(win: BrowserWindow, key: string) {
	report("own-view", { key, kept: Boolean(win.webview) });
}

export function noticeShown(win: BrowserWindow, n: Notice) {
	const key = `notice:${++notices}`;
	pages.set(key, { win, view: win.webview });
	report("notice", { key, title: n.title, message: n.message, hint: n.hint, detail: n.detail, actions: (n.actions ?? []).map((a) => a.id) });
	watchNavigation(win.webview, key);
	// A check presses the notice's buttons by running script in it, and hears back here.
	(win.webview as any).on("host-message", (e: any) => {
		const d = detailOf(e);
		if (d?.type === "eval") report("page", { key, detail: d });
		if (d?.type === "notice-ready") report("notice-ready", { key });
	});
	win.on("close", () => {
		report("notice-closed", { key });
		pages.delete(key);
	});
}

/** The text an app page shows, reported when it changes. */
export function pagePreloadFor(name: string): string {
	return `(function(){var last="";setInterval(function(){var t=(document.body?document.body.innerText:"").replace(/\\s+/g," ").slice(0,900);if(t!==last){last=t;window.__electrobunSendToHost&&window.__electrobunSendToHost({type:"page-text",name:${JSON.stringify(name)},text:t});}},300);})();`;
}

export function watchPage(win: BrowserWindow, name: string) {
	pages.set(name, { win, view: win.webview });
	report("window-created", { key: name });
	watchNavigation(win.webview, name);
	(win.webview as any).on("host-message", (e: any) => {
		const d = detailOf(e);
		report(d?.type === "page-text" ? "page-text" : "page", { key: name, detail: d, text: d?.text });
	});
	win.on("close", () => {
		report("window-close", { key: name });
		pages.delete(name);
	});
}

// What a native dialog would have answered, queued by a check: the open,
// folder and key-file dialogs cannot be clicked from outside the process.
const answers: { file: (string | null)[]; folder: (string | null)[]; key: (string | null)[] } = { file: [], folder: [], key: [] };

/** The queued answer for a dialog, or `undefined` for none (the dialog then opens). */
export function dialogAnswer(kind: "file" | "folder" | "key"): string | null | undefined {
	return answers[kind].length ? answers[kind].shift() : undefined;
}

async function run(c: any) {
	report("cmd", { cmd: c });
	const win = (key: string) => pages.get(key);
	switch (c.cmd) {
		case "open": report("open-result", { ...(await openFile(c.path)), path: c.path }); break;
		case "create": report("create-result", { ...(await createDatabase(c)), name: c.name }); break;
		case "close": win(c.key)?.win.requestClose(); break; // the path the red button takes
		case "close-direct": win(c.key)?.win.close(); break;
		// executeJavascript is not the page's own eval(): the studio's CSP
		// (script-src 'self') does not apply to it. The code reports its
		// result itself, through the event-only bridge.
		case "eval": win(c.key)?.view.executeJavascript(c.js); break;
		case "sessions": report("sessions", { keys: [...windows.keys()], pids: [...windows.values()].map((s) => s.server?.pid ?? s.on?.server.pid), pages: [...pages.keys()] }); break;
		case "answer": answers[c.kind as "file" | "folder" | "key"].push(c.path ?? null); break;
		// Serving: the settings are the sheet's defaults, but for what a check names.
		case "serve": report("serve-result", { ...(await serveFile(c.path, { ...DEFAULT_SETTINGS, ...(c.settings ?? {}) }, c.sync ?? "250")), path: c.path }); break;
		case "stop-served": report("stop-served-result", { ...(await stopServed(c.path)), path: c.path }); break;
		case "restart-served": report("restart-served-result", { ...(await restartServed(c.path, c.settings ? { ...DEFAULT_SETTINGS, ...c.settings } : undefined, c.sync)), path: c.path }); break;
		case "served": {
			const s = served.get(c.path);
			report("served-state", { path: c.path, running: Boolean(s), state: s?.state, origin: s?.server.origin, local: s?.server.local, port: s?.server.port, pid: s?.server.pid, metrics: s?.server.metrics });
			break;
		}
		// The served database's token, which a check sends as an app would: the one the Keychain keeps.
		case "serve-token": report("serve-token", { path: c.path, token: await serveTokenOf(c.path) }); break;
		case "new-token": report("new-token-result", { ...(await newServeToken(c.path)), path: c.path }); break;
		case "server-log": report("server-log", { path: c.path, lines: serverLog(c.path) }); break;
		case "figures": report("figures", { path: c.path, figures: await figuresOf(c.path) }); break;
		case "compact": report("compact-result", { ...(await compactNow(c.path)), path: c.path }); break;
		case "set-archive": report("set-archive-result", { ...setArchive(c.path, c.dir, c.keyFile ?? null), path: c.path }); break;
		case "backup": report("backup-result", { ...(await backupNow(c.path)), path: c.path }); break;
		case "verify": report("verify-result", { ...(await verifyBackups(c.path)), path: c.path }); break;
		case "restore": report("restore-result", { ...(await restoreBackup(c.path, c.change ?? null, c.folder, c.name)), from: c.path }); break;
		// Two restores into one name at once, as Restore in the main window and New database's "A backup" at the same moment.
		case "restore-twice": {
			const both = await Promise.all([restoreBackup(c.path, null, c.folder, c.name), restoreBackup(c.path, null, c.folder, c.name)]);
			report("restore-twice-result", { from: c.path, results: both });
			break;
		}
		case "check-conn": report("check-conn-result", { ...(await checkConnection(c.id)), id: c.id }); break;
		case "remove-served": await removeServed(c.path); report("served-removed", { path: c.path }); break;
		case "make-key": report("make-key-result", { ...(await makeKey(c.folder, c.name)) }); break;
		case "forget": report("forget-result", { ...(await forgetDatabase(c.path)), path: c.path }); break;
		case "launcher": await showLauncher(); break;
		case "newdb": await showNewDatabase(); break;
		case "addconn": {
			try {
				const saved = await saveConnection(c);
				report("conn-added", { id: saved.id, forgotToken: saved.forgotToken, name: c.name });
			} catch (e) {
				report("conn-refused", { message: (e as Error).message });
			}
			break;
		}
		case "openconn": report("openconn-result", { ...(await openConnection(c.id)), id: c.id }); break;
		// Two opens of one saved server at once, as two clicks while the first is still looking at it.
		case "openconn-twice": {
			const both = await Promise.all([openConnection(c.id), openConnection(c.id)]);
			report("openconn-twice-result", { id: c.id, results: both });
			break;
		}
		case "rmconn": await removeConnection(c.id); report("conn-removed", { id: c.id }); break;
		// The token saved for a connection, for the address it names now, and that address.
		case "secret": report("secret", { id: c.id, ...(await savedTokenOf(c.id)) }); break;
		// The Keychain item as it would be left by an edit of the address that was not
		// followed by a new token: the same token, sealed for another origin.
		case "seal": await secrets.set(keychainName(c.id), sealToken(c.origin, c.token)); report("sealed", { id: c.id }); break;
		case "forget-token": await forgetToken(c.id); report("token-forgotten", { id: c.id }); break;
		// A second window on a server's origin, in the data store named: a window of an
		// open file's (`of`: its key; its own store unless `partition` is given) or of a
		// saved server (`connection`: that connection's store, which a removal empties).
		// Reads what a store holds from outside the window that wrote it.
		case "store-window": {
			const of = c.of ? windows.get(c.of) : undefined;
			const origin = of?.server?.origin ?? c.origin;
			const token = of?.server?.token ?? c.token;
			const partition = c.connection ? remotePartition(c.connection) : (c.partition ?? filePartition());
			await openStudioWindow({ key: c.key, title: c.key, session: { origin, token, tenant: null }, partition });
			report("store-window-opened", { key: c.key, partition });
			break;
		}
		// A link as a studio window's page would ask for one, through the same filter
		// (a script's window.open raises no new-window-open on WebKitGTK or WebView2).
		case "open-link": {
			const link = linkOut(c.url, { at: 0 });
			report("link-out", { url: c.url, link });
			if (link) await external(link);
			break;
		}
		// The main window's "Show in folder".
		case "reveal": await revealPath(c.path); break;
		// Windows: keys pressed into a window, as a person's are (testkeys.ts): the
		// window brought to the front, a `click` into its page (near its top left
		// corner), then `type` typed or `chord` pressed.
		case "keys": {
			const page = win(c.key);
			if (!page || process.platform !== "win32") {
				report("keys-result", { key: c.key, error: page ? "Windows alone" : "no such window" });
				break;
			}
			const k = await import("./testkeys.ts");
			const hwnd = BigInt(page.win.ptr as unknown as number);
			const before = k.lastInput();
			let front = k.toFront(hwnd);
			// A point near the page's top left corner, inside it at any scale: the click is for the focus.
			const clicked = c.click ? k.click(hwnd, 8, 8) : null;
			// A click's activation is handled by the window's own thread, this one: it is waited for.
			for (let i = 0; !front && clicked && i < 50; i++) {
				await Bun.sleep(40);
				front = k.isFront(hwnd);
			}
			if (c.type) k.type(c.type);
			if (c.chord) k.chord(c.chord);
			// Whether the system took any of it: the raw input thread takes it after this returns.
			let taken = k.lastInput() !== before;
			for (let i = 0; !taken && i < 25; i++) {
				await Bun.sleep(20);
				taken = k.lastInput() !== before;
			}
			report("keys-result", { key: c.key, front, clicked, taken, type: c.type ?? null, chord: c.chord ?? null, ...(front ? {} : { inFront: k.foreground(), self: process.pid }) });
			break;
		}
		case "quit": Utils.quit(); break;
	}
}

/** Starts what the environment asks for. Returns whether it opened something the launcher need not cover. */
/** Names checks/hostile.mjs starts the app with on Windows: none may be left in its environment block (webview2.ts). */
const WEBVIEW2_NAMES = ["WEBVIEW2_USER_DATA_FOLDER", "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "webview2_pipe_for_script_debugger"];

export async function boot(): Promise<boolean> {
	onFact(report);
	if (process.platform === "win32") {
		const w = await import("./win32.ts");
		report("webview2-variables", { left: WEBVIEW2_NAMES.filter((name) => w.inEnvironment(name)) });
	}
	if (CONTROL) {
		let offset = 0;
		// A look's lines run in turn, but the next look does not wait for them:
		// commands read in two looks run side by side and report in either order.
		setInterval(async () => {
			let text = "";
			try {
				text = readFileSync(CONTROL, "utf8");
			} catch {
				return;
			}
			const fresh = text.slice(offset);
			const end = fresh.lastIndexOf("\n") + 1;
			offset += end;
			for (const line of fresh.slice(0, end).split("\n").filter(Boolean)) await run(JSON.parse(line));
		}, 150);
	}
	report("ready");
	const open = process.env["FENNEC_DESKTOP_OPEN"];
	// A list as PATH is one: ";" on Windows, where every path holds a ":" (C:\...).
	if (open) for (const p of open.split(delimiter)) await openFile(p);
	if (process.env["FENNEC_DESKTOP_LAUNCHER"]) await showLauncher();
	return Boolean(open) || Boolean(process.env["FENNEC_DESKTOP_LAUNCHER"]);
}

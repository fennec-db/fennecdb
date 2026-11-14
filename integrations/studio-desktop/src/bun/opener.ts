// What the app hands the system to open: a web link a studio page asked for
// (to the default browser) and a database's folder (to the file manager).
// **Neither goes through a shell.** macOS's Electrobun calls NSWorkspace and
// Windows' ShellExecute, which take the address as data. Linux's does not:
// Electrobun 2.0.4's `openExternal` falls back, when GIO cannot start the
// default browser, to `system("xdg-open \"<url>\"")`, and `showItemInFolder`
// always runs `system("xdg-open \"<file URI>\"")` -- a shell over text a page
// or a folder's name chose, inside double quotes where `$(...)` still runs. On
// the spike's runners `https://example.invalid/?q=$(touch$IFS<marker>)` made
// the marker (#578). So on Linux the app never calls either: it starts
// `xdg-open` itself with the address as one argument (`gio open` where there is
// no xdg-open), and `checks/opener.mjs` holds it to a link and a folder whose
// names would run a command in any shell they reached.
//
// The address handed over is made inert besides: a link is what the URL
// parser makes of it, with the characters a shell reads inside quotes
// (`$`, `` ` ``, `\`, `"`, `'`) percent-encoded, and a folder is a file: URI with
// every byte but letters, digits, `-._~` and `/` percent-encoded, so not even a
// program further on that did quote it badly (xdg-open is itself a script)
// would find one. Pure but for the spawn, so `node --test` holds the encoding and
// the filter (test/opener.test.ts).

import { spawn } from "node:child_process";
import { statSync } from "node:fs";
import { dirname } from "node:path";

/** A window is handed at most one link in this long: a page that asks over and over does not open tabs without end. */
export const EXTERNAL_GAP_MS = 3000;

/** A link as the system is handed it, or null: http(s) alone, as the URL parser reads it, with what a shell would read inside quotes encoded. */
export function webLink(url: string): string | null {
	let u: URL;
	try {
		u = new URL(url);
	} catch {
		return null;
	}
	if (u.protocol !== "http:" && u.protocol !== "https:") return null;
	return u.href.replace(/[$`\\"']/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase().padStart(2, "0")}`);
}

/**
 * Whether a studio window's request to open `url` goes to the system now, and
 * what it is handed: only a web address (never `file:` or a custom scheme),
 * and one in EXTERNAL_GAP_MS a window (`last` is that window's). Electrobun
 * 2.0.4 does not say whether a click asked for it: a script's `window.open` is
 * the same event.
 */
export function linkOut(url: string, last: { at: number }, now = Date.now()): string | null {
	const link = webLink(url);
	if (!link || now - last.at < EXTERNAL_GAP_MS) return null;
	last.at = now;
	return link;
}

/** What a page's request for a new window sends over the event bridge on Windows (`newWindowPreload`). */
export const LINK_OUT = "link-out";

/**
 * Windows: a preload for every window of the app that takes a page's request
 * for a new window before WebView2 does; "" elsewhere. WebKit hands the
 * request to Electrobun (macOS's createWebView, WebKitGTK's create signal),
 * which opens nothing; Electrobun 2.0.4 handles no WebView2
 * NewWindowRequested, so WebView2 opens a popup window of its own: held to no
 * origin, in the page's store, and alive after the app quit. Nothing native
 * refuses it -- `--block-new-web-contents=true`, which Microsoft lists, reached
 * WebView2 156's browser process and blocked nothing -- so the preload does,
 * and WebView2 runs it in every document of the window, every frame's
 * included (the initial about:blank, srcdoc, data:, blob:, another origin's,
 * one written by document.write: `checks/hostile.mjs` asks each).
 *
 * In each document it runs in, every way a page names a new window is taken:
 *   - `window.open`, and `document.open` with three arguments, which is
 *     window.open: the address goes over the event bridge as LINK_OUT and
 *     nothing opens (a studio window hands it to `linkOut`, the app's own pages
 *     drop it);
 *   - a link (`a`, `area`, SVG's `a`) followed to a target other than the
 *     page's own -- its own or the document's `<base target>` -- or with a
 *     modifier key or the middle button: sent likewise, by a click on it
 *     (taken at the window, before the page's listeners), `click()` (a link
 *     outside the document is clicked where no window listener hears it) and
 *     `dispatchEvent` (a script's click in a shadow root never reaches the
 *     window);
 *   - a form submitted to such a target (the submitter's `formtarget`, the
 *     form's, the document's): `submit()` (which fires no event),
 *     `requestSubmit()`, a click on its button and Enter in its field (a form in
 *     a shadow root raises a submit event the window never hears) do nothing.
 * A target naming a frame is taken as a new window too: neither the studio nor
 * the app's pages has a named frame. Every replaced function is put out of a
 * page's reach (not writable, not configurable), and every document of the
 * window has its own, so no realm keeps WebView2's. What a person must click
 * or allow first and the preload cannot see is not covered: a link inside a
 * PDF in WebView2's viewer, and a notification's `clients.openWindow`, which
 * Electrobun asks the user to allow.
 */
export function newWindowPreload(platform: NodeJS.Platform = process.platform): string {
	if (platform !== "win32") return "";
	return `(function(){
var send=function(u,base){try{var s=window.__electrobunSendToHost;if(typeof s==="function")s({type:${JSON.stringify(LINK_OUT)},url:new URL(String(u),base||document.baseURI).href});}catch(e){}};
var lock=function(o,k,v){try{Object.defineProperty(o,k,{value:v,writable:false,configurable:false});}catch(e){}};
var named=function(u){return u!==undefined&&u!==null&&String(u)!=="";};
lock(window,"open",function(u){if(named(u))send(u);return null;});
var docOpen=Document.prototype.open;
lock(Document.prototype,"open",function(u){if(arguments.length>2){if(named(u))send(u);return null;}return docOpen.apply(this,arguments);});
var elsewhere=function(t){t=(t||"").toLowerCase();return t!==""&&t!=="_self"&&t!=="_top"&&t!=="_parent";};
var targetOf=function(el,own){var t=own!==undefined?own:el.getAttribute("target");if(t!==null)return t;var b=(el.ownerDocument||document).querySelector("base[target]");return b?b.getAttribute("target"):"";};
var XLINK="http://www.w3.org/1999/xlink";
var hrefOf=function(n){return n.getAttribute("href")!==null?n.getAttribute("href"):n.getAttributeNS?n.getAttributeNS(XLINK,"href"):null;};
var kind=function(n){if(!n||n.nodeType!==1)return null;var l=String(n.localName).toLowerCase();
if((l==="a"||l==="area")&&hrefOf(n)!==null)return "link";
if(l==="button"&&String(n.getAttribute("type")||"submit").toLowerCase()==="submit"&&n.form)return "submit";
if(l==="input"&&/^(submit|image)$/i.test(String(n.getAttribute("type")||""))&&n.form)return "submit";
return null;};
var formLeaves=function(f,by){return elsewhere(targetOf(f,by&&by.hasAttribute&&by.hasAttribute("formtarget")?by.getAttribute("formtarget"):undefined));};
var asks=function(e){return !!(e&&(e.ctrlKey||e.shiftKey||e.metaKey||e.button===1));};
var away=function(path,e){for(var i=0;i<path.length;i++){var n=path[i],k=kind(n);if(!k)continue;
if(k==="link"){if(!elsewhere(targetOf(n))&&!asks(e))return null;send(hrefOf(n),n.baseURI);return n;}
return formLeaves(n.form,n)||asks(e)?n:null;}return null;};
var up=function(n){var p=[];while(n){p.push(n);n=n.parentNode||(n.nodeType===11&&n.host)||null;}return p;};
var clicks=function(e){if(away(e.composedPath?e.composedPath():[e.target],e))e.preventDefault();};
window.addEventListener("click",clicks,true);window.addEventListener("auxclick",clicks,true);
var click=HTMLElement.prototype.click;
lock(HTMLElement.prototype,"click",function(){if(away(up(this),null))return;return click.apply(this,arguments);});
var dispatch=EventTarget.prototype.dispatchEvent;
lock(EventTarget.prototype,"dispatchEvent",function(e){if(e&&/^(click|auxclick|DOMActivate)$/.test(e.type)&&this&&this.nodeType&&away(up(this),e))return false;return dispatch.apply(this,arguments);});
var submit=HTMLFormElement.prototype.submit,requestSubmit=HTMLFormElement.prototype.requestSubmit;
lock(HTMLFormElement.prototype,"submit",function(){if(formLeaves(this,null))return;return submit.apply(this,arguments);});
lock(HTMLFormElement.prototype,"requestSubmit",function(by){if(formLeaves(this,by||null))return;return requestSubmit.apply(this,arguments);});
window.addEventListener("submit",function(e){var f=e.target;if(f&&f.getAttribute&&formLeaves(f,e.submitter||null))e.preventDefault();},true);
window.addEventListener("keydown",function(e){if(e.key!=="Enter")return;var p=e.composedPath?e.composedPath():[e.target],n=p[0];if(n&&String(n.localName).toLowerCase()==="input"&&n.form&&formLeaves(n.form,null))e.preventDefault();},true);
})();`;
}

/** A path as a file: URI with every byte outside `A-Za-z0-9-._~/` percent-encoded. */
export function fileUri(path: string): string {
	const bytes = Buffer.from(path, "utf8");
	let out = "";
	for (const b of bytes) {
		const c = String.fromCharCode(b);
		out += /[A-Za-z0-9\-._~/]/.test(c) ? c : `%${b.toString(16).toUpperCase().padStart(2, "0")}`;
	}
	return `file://${out}`;
}

/** The folder a path is shown in: the folder itself, or the one a file is in. */
export function folderOf(path: string): string {
	return statSync(path, { throwIfNoEntry: false })?.isDirectory() ? path : dirname(path);
}

/** The programs that open an address on Linux, each an argument vector, in the order tried. */
export const linuxOpeners = (target: string): string[][] => [
	["xdg-open", target],
	["gio", "open", target],
];

/**
 * Starts the first of `argvs` that can be started, with no shell, and
 * resolves with how it ended (its code, or the error that kept every one of
 * them from starting). It holds nothing of the app's up: xdg-open may wait
 * for the browser it starts, which may outlive the app.
 */
export function runFirst(argvs: string[][], start = spawn): Promise<{ argv: string[]; code: number | null } | { error: string }> {
	return new Promise((resolve) => {
		const next = (i: number, why: string) => {
			const argv = argvs[i];
			if (!argv) return resolve({ error: why });
			const child = start(argv[0]!, argv.slice(1), { stdio: "ignore", shell: false });
			child.unref();
			// A program that could not be started may still report an exit: only the first word of each counts.
			let said = false;
			child.once("error", (e: NodeJS.ErrnoException) => {
				if (said) return;
				said = true;
				if (e.code === "ENOENT") next(i + 1, `${why}${why ? "; " : ""}${argv[0]}: not found`);
				else resolve({ error: `${argv[0]}: ${e.message}` });
			});
			child.once("exit", (code) => {
				if (said) return;
				said = true;
				resolve({ argv, code });
			});
		};
		next(0, "");
	});
}

/** Electrobun's calls where they take no shell (macOS, Windows). Loaded only there, and never by a test. */
const utils = async () => (await import("electrobun/main")).Utils;

/** Opens a web link (already through `linkOut`) in the default browser. */
export async function openLink(link: string): Promise<unknown> {
	if (process.platform === "linux") return runFirst(linuxOpeners(link));
	return (await utils()).openExternal(link);
}

/** What the button that shows a file in the file manager says, in each system's words. */
export function revealLabel(platform: string = process.platform): string {
	return platform === "darwin" ? "Reveal in Finder" : platform === "win32" ? "Show in Explorer" : "Open its folder";
}

/** Where a `.fennec` is double-clicked, as the main window's empty Databases names it: Linux's desktops each have their own. */
export function fileManager(platform: string = process.platform): string {
	return platform === "darwin" ? "Finder" : platform === "win32" ? "Explorer" : "your file manager";
}

/** Shows `path` (a database file, or a folder) in the file manager. */
export async function reveal(path: string): Promise<unknown> {
	if (process.platform === "linux") return runFirst(linuxOpeners(fileUri(folderOf(path))));
	return (await utils()).showItemInFolder(path);
}

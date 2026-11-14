import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import vm from "node:vm";
import { EXTERNAL_GAP_MS, fileManager, fileUri, folderOf, LINK_OUT, linkOut, linuxOpeners, newWindowPreload, revealLabel, runFirst, webLink } from "../src/bun/opener.ts";

// What the app hands the system to open, and the filter a studio window's
// links go through. The page-to-app hop is a WebKit event a script cannot
// raise on WebKitGTK, and on Windows the app's own preload, so the filter and
// the preload are held here on every system, and `checks/sandbox.mjs` holds
// the hop where it is raised (macOS, Windows).

const work = mkdtempSync(join(tmpdir(), "fennec-desktop-opener-"));
after(() => rmSync(work, { recursive: true, force: true }));

test("a link out is a web address alone, as the URL parser reads it", () => {
	assert.equal(webLink("https://example.com/a?b=c#d"), "https://example.com/a?b=c#d");
	assert.equal(webLink("HTTP://Example.COM"), "http://example.com/");
	for (const no of ["file:///etc/passwd", "javascript:alert(1)", "fennec-check://x/y", "data:text/html,x", "views://launcher/index.html", "mailto:a@b", "not a url", ""]) {
		assert.equal(webLink(no), null, no);
	}
});

test("what a shell reads inside quotes is percent-encoded: a link names no command in any shell it might reach", () => {
	const link = webLink("https://example.invalid/?q=$(touch$IFS/tmp/m)&r=`id`&s='x'&t=\"y\"&u=\\z")!;
	assert.equal(link, "https://example.invalid/?q=%24(touch%24IFS/tmp/m)&r=%60id%60&s=%27x%27&t=%22y%22&u=%5Cz");
	assert.doesNotMatch(link, /[$`'"\\]/);
	// The path and the fragment too.
	assert.doesNotMatch(webLink("https://h/$(id)/`id`#$(id)")!, /[$`]/);
});

test("a window is given one link in three seconds, and a refused one does not count", () => {
	const last = { at: 0 };
	assert.equal(linkOut("https://example.com/", last, 10_000), "https://example.com/");
	assert.equal(linkOut("https://example.com/b", last, 10_000 + EXTERNAL_GAP_MS - 1), null, "within the gap");
	assert.equal(linkOut("file:///etc/passwd", last, 10_000 + EXTERNAL_GAP_MS), null);
	assert.equal(linkOut("https://example.com/c", last, 10_000 + EXTERNAL_GAP_MS), "https://example.com/c", "the file: one took no turn");
});

test("a folder is a file: URI with every byte but letters, digits, -._~ and / percent-encoded", () => {
	assert.equal(fileUri("/home/me/My Data/x$(touch m);`id`"), "file:///home/me/My%20Data/x%24%28touch%20m%29%3B%60id%60");
	assert.equal(fileUri("/home/me/ünï"), "file:///home/me/%C3%BCn%C3%AF", "UTF-8's bytes");
	assert.equal(fileUri("/a-b_c.d~e/f"), "file:///a-b_c.d~e/f");
});

test("a database is shown in its folder, and a folder in itself", () => {
	const dir = join(work, "x$(touch m)");
	mkdirSync(dir);
	const db = join(dir, "orders.fennec");
	writeFileSync(db, "");
	assert.equal(folderOf(db), dir);
	assert.equal(folderOf(dir), dir);
	assert.equal(folderOf(join(dir, "gone.fennec")), dir, "a file gone since: its folder");
});

test("the file manager is named in each system's words: no Finder off a Mac (checks/pages.mjs reads the page for it)", () => {
	assert.equal(revealLabel("darwin"), "Reveal in Finder");
	assert.equal(fileManager("darwin"), "Finder");
	assert.equal(revealLabel("win32"), "Show in Explorer");
	assert.equal(fileManager("win32"), "Explorer");
	assert.equal(revealLabel("linux"), "Open its folder");
	assert.equal(fileManager("linux"), "your file manager");
});

test("Linux's openers are argument vectors, the address one argument of each", () => {
	const target = "file:///x%24%28id%29";
	assert.deepEqual(linuxOpeners(target), [
		["xdg-open", target],
		["gio", "open", target],
	]);
});

test("the first opener there is is started, with no shell; one that is not there is passed over", async (t) => {
	// The opener holds no event loop up (the app's windows do), so the test holds this one until it is done.
	const held = setInterval(() => {}, 1000);
	t.after(() => clearInterval(held));
	// A program that is there, run with a shell's word as an argument: it is handed the word, not what a shell would make of it.
	const node = process.execPath;
	const marker = join(work, "marker");
	const script = `require("node:fs").writeFileSync(${JSON.stringify(join(work, "argv.json"))}, JSON.stringify(process.argv.slice(1)))`;
	const word = `$(node -e "require('fs').writeFileSync('${marker.replaceAll("\\", "/")}','')")`;
	const done = await runFirst([
		["no-such-program-fennec-test", "x"],
		[node, "-e", script, word],
	]);
	assert.ok("argv" in done && done.code === 0, JSON.stringify(done));
	assert.deepEqual(JSON.parse((await import("node:fs")).readFileSync(join(work, "argv.json"), "utf8")), [word]);
	assert.equal((await import("node:fs")).existsSync(marker), false, "no shell ran the word");
	const none = await runFirst([["no-such-program-fennec-test"], ["no-such-program-either"]]);
	assert.ok("error" in none && /not found.*not found/.test(none.error), JSON.stringify(none));
});

/**
 * Windows' preload run as a page at `base` runs it: the bridge, the window's
 * listeners and the prototypes it replaces stood in for. What WebView2 itself
 * would have done -- its own `window.open`, a native `click()`,
 * `dispatchEvent`, `submit()`, `requestSubmit()`, `document.open` -- is
 * recorded in `reached`, and `window.open` fails the test if reached at all.
 */
function windowsPage(base = "http://127.0.0.1:4000/_studio/") {
	const sent: { type: string; url: string }[] = [];
	const reached: string[] = [];
	const listeners: Record<string, ((e: unknown) => void)[]> = {};
	class EventTarget {
		dispatchEvent(e: { type: string }) {
			reached.push(`dispatchEvent ${e.type}`);
			return true;
		}
	}
	class HTMLElement extends EventTarget {
		localName = "";
		click() {
			reached.push(`click ${this.localName}`);
		}
	}
	class HTMLFormElement extends HTMLElement {
		submit() {
			reached.push("submit");
		}
		requestSubmit() {
			reached.push("requestSubmit");
		}
	}
	class Document {
		open(...args: unknown[]) {
			reached.push(`document.open with ${args.length}`);
			return "the document";
		}
	}
	let baseTarget: string | null = null;
	const document = Object.assign(new Document(), {
		baseURI: base,
		querySelector: (sel: string) => (sel === "base[target]" && baseTarget !== null ? { getAttribute: () => baseTarget } : null),
	});
	const page: Record<string, unknown> = {
		URL,
		EventTarget,
		HTMLElement,
		HTMLFormElement,
		Document,
		document,
		// A copy, as the bridge serializes it: an object of the page's realm is no plain object of this one.
		__electrobunSendToHost: (m: unknown) => sent.push(JSON.parse(JSON.stringify(m))),
		addEventListener: (type: string, f: (e: unknown) => void, capture: boolean) => {
			assert.equal(capture, true, "taken before the page's own listeners");
			(listeners[type] ??= []).push(f);
		},
		open: () => assert.fail("WebView2's own window.open was reached"),
	};
	page["window"] = page;
	vm.createContext(page);
	vm.runInContext(newWindowPreload("win32"), page);
	const run = (js: string) => vm.runInContext(js, page);
	/** An event of `type` that went through `path` (the innermost first), as the window's listeners would see it. */
	const dispatch = (type: string, path: unknown[], init: Record<string, unknown> = {}) => {
		const e = { type, target: path[0], composedPath: () => path, button: 0, ctrlKey: false, shiftKey: false, metaKey: false, defaultPrevented: false, preventDefault() { this.defaultPrevented = true; }, ...init };
		for (const f of listeners[type] ?? []) f(e);
		return e.defaultPrevented;
	};
	/** An element of the page, with `attrs`, inside `parent` (an element or a shadow root), in a form where it is a form's control. */
	const element = (localName: string, attrs: Record<string, string>, parent: unknown = null, form: unknown = null): any => {
		const proto = localName === "form" ? HTMLFormElement.prototype : HTMLElement.prototype;
		return Object.assign(Object.create(proto), {
			localName,
			nodeType: 1,
			parentNode: parent,
			form,
			baseURI: base,
			ownerDocument: document,
			getAttribute: (n: string) => attrs[n] ?? null,
			hasAttribute: (n: string) => n in attrs,
			getAttributeNS: (_ns: string, n: string) => attrs[`xlink:${n}`] ?? null,
		});
	};
	const shadowRoot = (host: unknown) => ({ nodeType: 11, parentNode: null, host });
	const setBaseTarget = (t: string | null) => void (baseTarget = t);
	return { sent, reached, run, dispatch, element, shadowRoot, setBaseTarget, document };
}

test("off Windows there is no new-window preload: WebKit hands the request to Electrobun", () => {
	assert.equal(newWindowPreload("darwin"), "");
	assert.equal(newWindowPreload("linux"), "");
});

test("Windows: window.open sends the address to the app and opens nothing, and cannot be put back", () => {
	const { sent, run } = windowsPage();
	assert.equal(run(`window.open("https://example.com/a")`), null);
	assert.equal(run(`window.open("../docs#x")`), null, "a relative address");
	assert.equal(run(`window.open("file:///etc/passwd")`), null, "the filter is the app's: every address goes to it");
	assert.equal(run(`window.open()`), null, "nothing named: nothing sent");
	assert.deepEqual(sent, [
		{ type: LINK_OUT, url: "https://example.com/a" },
		{ type: LINK_OUT, url: "http://127.0.0.1:4000/docs#x" },
		{ type: LINK_OUT, url: "file:///etc/passwd" },
	]);
	run(`window.open = function () { return "replaced"; }`);
	assert.equal(run(`window.open("https://example.com/b")`), null, "an assignment does not replace it");
	assert.throws(() => run(`Object.defineProperty(window, "open", { value: () => 1 })`), "nor a definition");
});

test("Windows: document.open with three arguments is window.open, and with fewer the page's own", () => {
	const { sent, reached, run } = windowsPage();
	assert.equal(run(`document.open("https://example.com/w", "", "")`), null);
	assert.deepEqual(sent.map((m) => m.url), ["https://example.com/w"]);
	assert.equal(run(`document.open()`), "the document", "the document opened for writing, as before");
	assert.deepEqual(reached, ["document.open with 0"]);
	assert.throws(() => run(`Object.defineProperty(Document.prototype, "open", { value: () => 1 })`), "not to be put back");
});

test("Windows: a link that would open a window is sent to the app instead, and one that stays is left alone", () => {
	const { sent, dispatch, element, setBaseTarget } = windowsPage();
	const blank = element("a", { href: "https://example.com/blank", target: "_blank" });
	const inner = element("span", {}, blank);
	assert.equal(dispatch("click", [inner, blank]), true, "target=_blank, clicked on what it holds");
	const named = element("a", { href: "/x", target: "results" });
	assert.equal(dispatch("click", [named]), true, "a target naming a frame: none is there");
	const plain = element("a", { href: "#c=orders" });
	assert.equal(dispatch("click", [plain]), false, "a link in the page stays the page's");
	assert.equal(dispatch("click", [element("a", { href: "/y", target: "_self" })]), false);
	assert.equal(dispatch("click", [element("a", { href: "/z", target: "_top" })]), false);
	assert.equal(dispatch("click", [plain], { ctrlKey: true }), true, "Ctrl+click asks for a new tab");
	assert.equal(dispatch("click", [plain], { shiftKey: true }), true, "Shift+click for a new window");
	assert.equal(dispatch("auxclick", [plain], { button: 1 }), true, "the middle button");
	assert.equal(dispatch("click", [element("a", {})]), false, "an anchor with no href is no link");
	assert.equal(dispatch("click", [element("area", { href: "/map", target: "_blank" })]), true, "an image map's area is a link");
	assert.equal(dispatch("click", [element("a", { "xlink:href": "/svg", target: "_blank" })]), true, "and SVG's a, by xlink:href");
	setBaseTarget("_blank");
	assert.equal(dispatch("click", [plain]), true, "a link with no target of its own, under <base target=_blank>");
	assert.equal(dispatch("click", [element("a", { href: "/own", target: "" })]), false, "its own target, empty, is the page");
	setBaseTarget(null);
	assert.deepEqual(sent.map((m) => m.url), [
		"https://example.com/blank",
		"http://127.0.0.1:4000/x",
		"http://127.0.0.1:4000/_studio/#c=orders",
		"http://127.0.0.1:4000/_studio/#c=orders",
		"http://127.0.0.1:4000/_studio/#c=orders",
		"http://127.0.0.1:4000/map",
		"http://127.0.0.1:4000/svg",
		"http://127.0.0.1:4000/_studio/#c=orders",
	]);
});

test("Windows: a script's click on a link, by click() or dispatchEvent, out of the window's hearing, is taken as a click is", () => {
	const { sent, reached, element, shadowRoot } = windowsPage();
	// Outside the document: its click event never reaches the window.
	const detached = element("a", { href: "https://example.com/detached", target: "_blank" });
	detached.click();
	// In a closed shadow root, a script's event that is not composed: it never leaves the root.
	const host = element("div", {});
	const shadowed = element("a", { href: "https://example.com/shadow", target: "_blank" }, shadowRoot(host));
	assert.equal(shadowed.dispatchEvent({ type: "click", cancelable: false }), false, "not dispatched, cancelable or not");
	// At a text node inside the link: its activation is the link's.
	const text = { nodeType: 3, parentNode: shadowed };
	assert.equal(Reflect.apply(Object.getPrototypeOf(Object.getPrototypeOf(shadowed)).dispatchEvent, text, [{ type: "click" }]), false);
	assert.deepEqual(sent.map((m) => m.url), ["https://example.com/detached", "https://example.com/shadow", "https://example.com/shadow"]);
	assert.deepEqual(reached, [], "WebView2 followed none of them");
	// What stays is the page's own: a link in place, any other event.
	element("a", { href: "#here" }).click();
	shadowed.dispatchEvent({ type: "focus" });
	assert.deepEqual(reached, ["click a", "dispatchEvent focus"]);
});

test("Windows: a form submitted to another window is not submitted, by any of the ways a page has", () => {
	const { reached, dispatch, element, setBaseTarget } = windowsPage();
	const leaving = element("form", { target: "_blank", action: "https://example.com/f" });
	const staying = element("form", { action: "/save" });
	leaving.submit();
	leaving.requestSubmit();
	staying.submit();
	staying.requestSubmit();
	assert.deepEqual(reached, ["submit", "requestSubmit"], "submit() fires no event, so it is taken itself; the form that stays is submitted");
	const by = element("button", { formtarget: "_blank" }, staying, staying);
	staying.requestSubmit(by);
	assert.deepEqual(reached.length, 2, "a submitter's formtarget is the form's target");
	setBaseTarget("_blank");
	staying.submit();
	assert.deepEqual(reached.length, 2, "and so is the document's <base target>");
	setBaseTarget(null);
	// A click on its button and Enter in its field: a form in a shadow root raises a submit event the window never hears.
	const button = element("button", {}, leaving, leaving);
	assert.equal(dispatch("click", [button]), true, "its submit button");
	assert.equal(dispatch("click", [element("input", { type: "image" }, leaving, leaving)]), true, "an image input submits too");
	assert.equal(dispatch("click", [element("button", { type: "button" }, leaving, leaving)]), false, "a button that submits nothing");
	assert.equal(dispatch("click", [element("button", {}, staying, staying)]), false, "the button of a form that stays");
	const field = element("input", { type: "text" }, leaving, leaving);
	assert.equal(dispatch("keydown", [field], { key: "Enter" }), true, "Enter in its field");
	assert.equal(dispatch("keydown", [field], { key: "a" }), false, "any other key");
	assert.equal(dispatch("keydown", [element("input", {}, staying, staying)], { key: "Enter" }), false);
	assert.equal(dispatch("submit", [leaving], { submitter: null }), true, "its submit event, where the window hears it");
	assert.equal(dispatch("submit", [staying], { submitter: null }), false);
});

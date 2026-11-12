import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import vm from "node:vm";
import { EXTERNAL_GAP_MS, fileUri, folderOf, LINK_OUT, linkOut, linuxOpeners, newWindowPreload, runFirst, webLink } from "../src/bun/opener.ts";

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
 * Windows' preload run as a page at `base` runs it: the bridge and the window's
 * listeners stood in for, and WebView2's own `window.open` one that fails the
 * test if it is ever reached.
 */
function windowsPage(base = "http://127.0.0.1:4000/_studio/") {
	const sent: { type: string; url: string }[] = [];
	const listeners: Record<string, ((e: unknown) => void)[]> = {};
	const page: Record<string, unknown> = {
		URL,
		document: { baseURI: base },
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
	/** An event of `type` that went through `path` (the innermost first), as the page's listeners would see it. */
	const dispatch = (type: string, path: unknown[], init: Record<string, unknown> = {}) => {
		const e = { target: path[0], composedPath: () => path, button: 0, ctrlKey: false, shiftKey: false, metaKey: false, defaultPrevented: false, preventDefault() { this.defaultPrevented = true; }, ...init };
		for (const f of listeners[type] ?? []) f(e);
		return e.defaultPrevented;
	};
	return { sent, run, dispatch };
}
const element = (localName: string, attrs: Record<string, string>) => ({ localName, getAttribute: (n: string) => attrs[n] ?? null, hasAttribute: (n: string) => n in attrs });

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

test("Windows: a link that would open a window is sent to the app instead, and one that stays is left alone", () => {
	const { sent, dispatch } = windowsPage();
	const inner = { localName: "span" };
	const blank = element("a", { href: "https://example.com/blank", target: "_blank" });
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
	assert.equal(dispatch("click", [element("button", { href: "/n", target: "_blank" })]), false, "only an anchor");
	assert.deepEqual(
		sent.map((m) => m.url),
		["https://example.com/blank", "http://127.0.0.1:4000/x", "http://127.0.0.1:4000/_studio/#c=orders", "http://127.0.0.1:4000/_studio/#c=orders", "http://127.0.0.1:4000/_studio/#c=orders"],
	);
	assert.equal(dispatch("submit", [element("form", { target: "_blank" })]), true, "a form posted to a new window is not");
	assert.equal(dispatch("submit", [element("form", {})]), false, "one posted in place is the page's");
	assert.equal(sent.length, 5, "a form sends nothing");
});

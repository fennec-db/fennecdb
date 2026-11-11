import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { EXTERNAL_GAP_MS, fileUri, folderOf, linkOut, linuxOpeners, runFirst, webLink } from "../src/bun/opener.ts";

// What the app hands the system to open, and the filter a studio window's
// links go through. The page-to-app hop is a WebKit event a script cannot
// raise on WebKitGTK or WebView2, so the filter is held here on every system,
// and `checks/sandbox.mjs` holds the event where it is raised (macOS).

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

test("the first opener there is is started, with no shell; one that is not there is passed over", async () => {
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

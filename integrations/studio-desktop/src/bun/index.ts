// fennec studio, desktop. The main process (Bun).
//
// A window per open thing:
//   - a file: a fennec-server this process started for it, and a window on
//     that server's own /_studio/ (so the studio is the one the server was built with);
//   - a remote server: a window on that server's /_studio/, same origin, so no CORS;
// plus a launcher (recent files, saved servers) and a new-database window.
//
// The pieces: app.ts what the app does, server.ts the one place a server is
// started, session.ts the one place a token reaches a page, windows.ts what a
// studio window is held to, pages.ts the app's own pages, messages.ts every
// failure in words. README.md has the picture.

import Electrobun, { Utils } from "electrobun/main";
import { anythingOpen, isQuitting, openFile, serversBusy, stopAll } from "./app.ts";
import { registerSelf } from "./associate.ts";
import { sweepSecrets } from "./clipboard.ts";
import { launched, pathsOf } from "./handover.ts";
import { claimInstance, instancePath, pipeName, runtimeFolder, type Ask, type PipeSecurity } from "./instance.ts";
import { startMenu } from "./menu.ts";
import { closeLauncher, launcherOpen, showLauncher } from "./pages.ts";
import { sweepLeftStores } from "./windows.ts";

/**
 * Where this user's one instance listens (instance.ts), and on Windows the
 * security calls that make the pipe the user's (win32.ts); null where nothing
 * is the user's alone, and the app runs unguarded.
 */
async function guard(): Promise<{ path: string; pipe?: PipeSecurity } | null> {
	if (process.platform === "darwin") return { path: instancePath(__KEYCHAIN_SERVICE__) };
	if (process.platform === "linux") {
		const folder = runtimeFolder(process.env, process.getuid?.() ?? -1, Utils.paths.userData);
		return folder ? { path: instancePath(__KEYCHAIN_SERVICE__, folder) } : null;
	}
	try {
		const w = await import("./win32.ts");
		const user = w.userSid();
		// What our own instance's pipe is owned by: this user, or Administrators where it ran
		// elevated (only an administrator makes that owner, and one is the machine's anyway).
		const ours = new Set([user, w.ownerSid(), "S-1-5-32-544"]);
		return {
			path: pipeName(__KEYCHAIN_SERVICE__, user),
			pipe: {
				secure: (name) => w.restrictPipe(name, user),
				owner: (name) => {
					const o = w.pipeOwner(name);
					if ("sid" in o) return ours.has(o.sid) ? "ours" : "theirs";
					return o.error === w.ERROR_PIPE_BUSY ? "busy" : "theirs";
				},
			},
		};
	} catch (e) {
		console.error(`no single-instance guard: ${(e as Error).message}`);
		return null;
	}
}

// Windows and Linux: the files the system handed the launcher, a `.fennec`
// double-clicked (handover.ts; macOS's come as `open-url`, below).
const launch = await launched();
const handed = pathsOf(launch);
if (__TESTABLE__) {
	// What reached this process, said as it is: the check holds the hand-over to it.
	(await import("./testhooks.ts")).report("launch", {
		argv: process.argv,
		ppid: process.ppid,
		launcherPid: process.env["ELECTROBUN_LAUNCHER_PID"] ?? null,
		env: Object.keys(process.env).sort(),
		launched: launch,
		handed,
	});
}

/** What another instance asked: to show the main window, or to open the files it was handed. */
function asked(ask: Ask) {
	if (__TESTABLE__) void import("./testhooks.ts").then((h) => h.report("instance-asked", { ask }));
	if ("open" in ask) for (const path of ask.open) void openFile(path);
	else void showLauncher();
}

// One instance at a time (instance.ts): a second asks the first to show its
// main window, or to open what it was handed, and quits before it opens
// anything or reads its lists.
const where = await guard();
const claim = where ? await claimInstance(where.path, asked, where.pipe, handed.length ? { open: handed } : { show: true }) : "unguarded";
if (__TESTABLE__) (await import("./testhooks.ts")).report("instance", { path: where?.path ?? null, claim });
if (claim === "another") {
	if (__TESTABLE__) (await import("./testhooks.ts")).report("second-instance");
	Utils.quit();
	// Nothing after this runs in a second instance.
	await new Promise(() => {});
}
// Windows: the file windows' stores a run that ended left behind (stores.ts), by the one instance alone.
if (claim === "alone") void sweepLeftStores();
// Windows and Linux: a `.fennec` opens in this app (associate.ts), pointed at
// where it runs from now; written only where it differs, by the one instance.
if (claim !== "another") {
	void registerSelf(__KEYCHAIN_SERVICE__, __APP_NAME__).then(
		(done) => {
			if (__TESTABLE__) void import("./testhooks.ts").then((h) => h.report("registered", done));
		},
		(e) => {
			console.error(`.fennec files were not registered to open here: ${(e as Error).message}`);
			if (__TESTABLE__) void import("./testhooks.ts").then((h) => h.report("registered", { error: (e as Error).message }));
		},
	);
}

const started = Date.now();
/** The launcher was shown because nothing else was opening, not because someone asked for it. */
let autoLauncher = false;
/** A file was handed over by the system (Finder), and when. */
let handedOver = false;
const AUTO_LAUNCHER_GRACE_MS = 5000;

// Finder: a .fennec file double-clicked (macOS; Info.plist from app.fileAssociations).
Electrobun.events.on("open-url", (event: any) => {
	const url = String(event?.data?.url ?? "");
	if (__TESTABLE__) void import("./testhooks.ts").then((h) => h.report("open-url", { url }));
	if (url.startsWith("file:")) {
		void (async () => {
			// The test variant can hold the event back until the launcher is up, as a
			// slow launch delivers it after: the order a timer could only hope for.
			if (__TESTABLE__ && process.env["FENNEC_DESKTOP_HOLD_OPEN_URL"] === "launcher") {
				for (let i = 0; i < 200 && !launcherOpen(); i++) await Bun.sleep(50);
			}
			// A file handed over after the launcher was shown on its own (the launch
			// was slow, and the 400 ms below was not enough) is what the launch was
			// for: the launcher, which nobody has had time to use, is closed.
			handedOver = true;
			if (autoLauncher && Date.now() - started < AUTO_LAUNCHER_GRACE_MS) closeLauncher();
			void openFile(decodeURIComponent(new URL(url).pathname));
		})();
	}
});

// The Dock icon clicked with no window open.
Electrobun.events.on("reopen", () => {
	if (!anythingOpen()) void showLauncher();
});

// Stop every server before the process goes: the quit is held, the servers
// are asked to checkpoint, then the quit is asked for again. A token copied
// less than a minute ago goes from the clipboard first (clipboard.ts).
Electrobun.events.on("before-quit", (event: any) => {
	sweepSecrets();
	if (isQuitting() || !serversBusy()) return;
	event.response = { allow: false };
	void stopAll().finally(() => Utils.quit());
});

for (const sig of ["SIGTERM", "SIGINT", "SIGHUP"] as const) {
	process.on(sig, () => {
		sweepSecrets();
		void stopAll().finally(() => process.exit(0));
	});
}

// A server this app started does not outlive it: its stdin is a pipe this
// process holds, and the server stops when it ends (server.ts), so there is
// nothing from an earlier run to stop before a file is opened.
startMenu();

let opened = false;
if (__TESTABLE__) {
	const hooks = await import("./testhooks.ts");
	opened = await hooks.boot();
}
// The files a double-click handed this, the one instance, as Finder's arrive on macOS.
if (handed.length) {
	opened = true;
	for (const path of handed) void openFile(path);
}
// A launch from Finder with a file arrives as `open-url` a moment after the
// process starts: wait for it before showing the launcher over the file's window.
if (!opened) await Bun.sleep(400);
if (!opened && !anythingOpen()) {
	autoLauncher = true;
	await showLauncher();
	// The file may have come while the launcher was being made.
	if (handedOver) closeLauncher();
}

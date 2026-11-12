// One instance of the app at a time. Two would each keep state.json and the
// credential store's items as if they were alone: one's Remove would delete
// the token under a server the other runs, and each would write the other's
// lists over. macOS opens a running app rather than a second one from Finder,
// the Dock and `open`, but not from `open -n` or the binary run by hand;
// Electrobun 2.0.4 has no single-instance switch. So the first instance
// listens somewhere that is its user's alone, and a later one that reaches it
// asks it to show itself and quits. What "its user's alone" is depends on the
// system:
//   - macOS: a socket in $TMPDIR, the user's own folder (0700);
//   - Linux: a socket in $XDG_RUNTIME_DIR (0700, the user's, made by the login
//     session), not /tmp, which every user writes to; where it is not such a
//     folder, the app's own data folder if that is; else no guard;
//   - Windows: a named pipe, `\\.\pipe\<identifier>.instance.<the user's SID>`.
//     A pipe's name is one for the whole machine, so the SID makes it the
//     user's, and the listen makes it ours: libuv makes the first instance with
//     FILE_FLAG_FIRST_PIPE_INSTANCE, which fails where the name exists. Once
//     it listens, its DACL is made the user's alone (win32.ts). A user who
//     made a pipe of that name first is not obeyed: a pipe whose owner is not
//     this user (or one this user cannot even open) is never asked, and the
//     app runs unguarded rather than quit for someone else.
// A socket left by an instance that ended answers nothing, and is taken over;
// a pipe goes with its last holder. Pure but for the socket, so `node --test`
// holds it (test/instance.test.ts), Windows' pipe with stand-ins for its
// security calls (the real ones are Bun's: index.ts).

import { lstatSync, unlinkSync } from "node:fs";
import { connect, createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

/** The socket of the app `identifier` (the test variant has its own) in `folder`. */
export const instancePath = (identifier: string, folder = tmpdir()) => join(folder, `${identifier}.instance`);

/** Windows: the app's pipe for the user whose SID is `sid`. */
export const pipeName = (identifier: string, sid: string) => `\\\\.\\pipe\\${identifier}.instance.${sid}`;

const isPipe = (path: string) => path.startsWith("\\\\.\\pipe\\");

/** Whether `dir` is a folder of `uid`'s that no one else may write to: a socket there is that user's alone. */
export function ownFolder(dir: string, uid: number): boolean {
	const s = lstatSync(dir, { throwIfNoEntry: false });
	return Boolean(s?.isDirectory() && s.uid === uid && (s.mode & 0o022) === 0);
}

/** Linux: the first of $XDG_RUNTIME_DIR and `fallback` that is `uid`'s alone, or null. */
export function runtimeFolder(env: Record<string, string | undefined>, uid: number, fallback: string): string | null {
	for (const dir of [env["XDG_RUNTIME_DIR"], fallback]) if (dir && ownFolder(dir, uid)) return dir;
	return null;
}

/** Does an instance answer at `path`? If one does, it is asked to show itself. */
function askOther(path: string): Promise<boolean> {
	return new Promise((resolve) => {
		const socket = connect(path);
		socket.once("connect", () => socket.end("show\n", () => resolve(true)));
		socket.once("error", () => resolve(false));
	});
}

type Listened = "listening" | "taken" | "failed";

/** Listens at `path` for later instances; "taken": something is there already. */
function listen(path: string, onAsk: () => void): Promise<Listened> {
	return new Promise((resolve) => {
		const server = createServer((c) => {
			c.on("error", () => {});
			c.once("data", () => onAsk());
			c.resume();
		});
		server.once("error", (e: NodeJS.ErrnoException) => resolve(e.code === "EADDRINUSE" ? "taken" : "failed"));
		server.listen(path, () => {
			// The socket keeps no event loop alive: the app's windows do.
			server.unref();
			resolve("listening");
		});
	});
}

/** Windows: what the guard asks of a pipe, through the system's security calls. */
export type PipeSecurity = {
	/** Makes the pipe this app listens on its user's alone. */
	secure(name: string): void;
	/** Who made the pipe of that name: this user, someone else (or nobody this user may ask), or no instance free to ask yet. */
	owner(name: string): "ours" | "theirs" | "busy";
};

/**
 * "alone": this is the one instance, and `onAsk` is called whenever another
 * starts. "another": another runs, and has been asked to show itself; this
 * one is to quit before it opens anything. "unguarded": no guard could be
 * kept (no folder of the user's alone, a socket that cannot be made, a pipe
 * someone else made): the app runs, as it would with none.
 *
 * The bind is the claim, made first, so two instances started together cannot
 * both have it: the second's bind fails, and it asks the first. A socket an
 * instance that ended left answers nothing and is taken over; only two
 * instances started together beside such a socket could both take it, in the
 * moment between one's look and its unlink.
 */
export async function claimInstance(path: string, onAsk: () => void, pipe?: PipeSecurity): Promise<"alone" | "another" | "unguarded"> {
	for (let attempt = 0; attempt < 3; attempt++) {
		const listened = await listen(path, onAsk);
		if (listened === "failed") return "unguarded";
		if (listened === "listening") {
			if (pipe) {
				try {
					pipe.secure(path);
				} catch (e) {
					// Still the one instance, by the name it holds: the DACL keeps other users out of it, which a failure here is said for.
					console.error(`the instance pipe could not be made this user's alone: ${(e as Error).message}`);
				}
			}
			return "alone";
		}
		if (pipe) {
			let owner = pipe.owner(path);
			// Every instance of our own pipe busy (others starting at this moment): asked again, a few times.
			for (let i = 0; owner === "busy" && i < 20; i++) {
				await new Promise((r) => setTimeout(r, 50));
				owner = pipe.owner(path);
			}
			// Not this user's: never asked, never obeyed.
			if (owner !== "ours") return "unguarded";
		}
		if (await askOther(path)) return "another";
		// Nothing answered: what is there was left by an instance that ended. A pipe has no file to leave.
		if (!isPipe(path)) {
			try {
				unlinkSync(path);
			} catch {
				// Taken away meanwhile by another instance starting.
			}
		}
	}
	// Three binds lost to what answers nothing: run unguarded rather than not at all.
	return "unguarded";
}

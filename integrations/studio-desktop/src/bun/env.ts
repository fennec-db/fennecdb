// What the programs the app starts (fennec-server, and the `fennec` shell that
// takes and restores backups) are given of the app's own environment: an
// allowlist, not "everything but ours". The server reads the environment for
// more than its token -- `OTEL_EXPORTER_OTLP_*` turns trace export on, a
// `FENNEC_JWT_SECRET` changes what it accepts -- and `fennec` reads
// `FENNEC_KEY_FILE` and `FENNEC_REPLICATION_TOKEN`; a database file opened from
// a Finder double-click should do what it does for everyone, whatever the
// shell that launched the app had exported. The secrets the app hands over go
// here, never on a command line, which `ps` shows every user. Pure, so
// `node --test` holds it, each system's rule on every system (test/env.test.ts).
//
// Windows reads a variable's name case-blind (`Path`, not `PATH`, is how it is
// set there), so its names are compared so too, and it has names of its own
// for the same things: the system's folder (`SystemRoot`, which Winsock wants),
// the temporary one (`TEMP`, `TMP`) and the user's (`USERPROFILE`). libuv, which
// Bun and Node start a program through there, hands a child its `required_vars`
// from the parent whatever environment it is given; they are named here so the
// list says what the server gets.

const KEEP = new Set(["HOME", "PATH", "TMPDIR", "LANG", "USER", "LOGNAME", "TZ"]);
/** libuv's required_vars (src/win/process.c), and TMP. */
const KEEP_WINDOWS = new Set(["HOMEDRIVE", "HOMEPATH", "LOGONSERVER", "SYSTEMDRIVE", "SYSTEMROOT", "TEMP", "TMP", "USERDOMAIN", "USERNAME", "USERPROFILE", "WINDIR"]);

/** Is `name` passed on to a program the app starts, on `platform`? */
export function kept(name: string, platform: NodeJS.Platform = process.platform): boolean {
	if (platform !== "win32") return KEEP.has(name) || name.startsWith("LC_");
	const n = name.toUpperCase();
	return KEEP.has(n) || KEEP_WINDOWS.has(n) || n.startsWith("LC_");
}

/** The secrets a program of the app's may be given, by the variable it reads them from. */
export type Secrets = {
	/** The data token (`fennec-server`). */
	FENNEC_HTTP_TOKEN?: string;
	/** The replication token: `fennec-server` serves its stream with it, `fennec backup` asks with it. */
	FENNEC_REPLICATION_TOKEN?: string;
};

export function childEnvironment(env: Record<string, string | undefined>, secrets: Secrets, platform: NodeJS.Platform = process.platform): Record<string, string> {
	const out: Record<string, string> = {};
	for (const [k, v] of Object.entries(env)) {
		if (v !== undefined && kept(k, platform)) out[k] = v;
	}
	for (const [k, v] of Object.entries(secrets)) {
		// An empty one is none: the server and the shell take a variable set empty as unset.
		if (v) out[k] = v;
	}
	return out;
}

/** A server's environment: the allowlist, its data token and, unless none, its replication token. */
export function serverEnvironment(
	env: Record<string, string | undefined>,
	token: string,
	replicationToken: string | null = null,
	platform: NodeJS.Platform = process.platform,
): Record<string, string> {
	return childEnvironment(env, { FENNEC_HTTP_TOKEN: token, ...(replicationToken ? { FENNEC_REPLICATION_TOKEN: replicationToken } : {}) }, platform);
}

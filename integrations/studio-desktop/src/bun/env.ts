// What the programs the app starts (fennec-server, and the `fennec` shell that
// takes and restores backups) are given of the app's own environment: an
// allowlist, not "everything but ours". The server reads the environment for
// more than its token -- `OTEL_EXPORTER_OTLP_*` turns trace export on, a
// `FENNEC_JWT_SECRET` changes what it accepts -- and `fennec` reads
// `FENNEC_KEY_FILE` and `FENNEC_REPLICATION_TOKEN`; a database file opened from
// a Finder double-click should do what it does for everyone, whatever the
// shell that launched the app had exported. The secrets the app hands over go
// here, never on a command line, which `ps` shows every user. Pure, so
// `node --test` holds it (test/env.test.ts).

const KEEP = new Set(["HOME", "PATH", "TMPDIR", "LANG", "USER", "LOGNAME", "TZ"]);
// SPIKE (Windows): a variable's name is case-blind there (`Path`, not `PATH`),
// and a program started without `SystemRoot` may fail to start Winsock
// (WSAEPROVIDERFAILEDINIT); the temporary folder is `TEMP`/`TMP` and the
// profile `USERPROFILE`/`LOCALAPPDATA`. desktop-spike.yml's process probe
// starts the server with the allowlist above alone to see which matter.
const KEEP_WINDOWS = new Set(["SYSTEMROOT", "WINDIR", "PATH", "TEMP", "TMP", "USERPROFILE", "LOCALAPPDATA", "APPDATA", "USERNAME", "TZ"]);
const keep = (k: string) => (process.platform === "win32" ? KEEP_WINDOWS.has(k.toUpperCase()) : KEEP.has(k) || k.startsWith("LC_"));

/** The secrets a program of the app's may be given, by the variable it reads them from. */
export type Secrets = {
	/** The data token (`fennec-server`). */
	FENNEC_HTTP_TOKEN?: string;
	/** The replication token: `fennec-server` serves its stream with it, `fennec backup` asks with it. */
	FENNEC_REPLICATION_TOKEN?: string;
};

export function childEnvironment(env: Record<string, string | undefined>, secrets: Secrets): Record<string, string> {
	const out: Record<string, string> = {};
	for (const [k, v] of Object.entries(env)) {
		if (v !== undefined && keep(k)) out[k] = v;
	}
	for (const [k, v] of Object.entries(secrets)) {
		// An empty one is none: the server and the shell take a variable set empty as unset.
		if (v) out[k] = v;
	}
	return out;
}

/** A server's environment: the allowlist, its data token and, unless none, its replication token. */
export function serverEnvironment(env: Record<string, string | undefined>, token: string, replicationToken: string | null = null): Record<string, string> {
	return childEnvironment(env, { FENNEC_HTTP_TOKEN: token, ...(replicationToken ? { FENNEC_REPLICATION_TOKEN: replicationToken } : {}) });
}

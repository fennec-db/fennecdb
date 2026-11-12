import assert from "node:assert/strict";
import { test } from "node:test";
import { childEnvironment, kept, serverEnvironment } from "../src/bun/env.ts";

// Each system's rule, asked on every system: the function is pure, and the
// platform it answers for is a parameter (the app passes its own).

/** What a shell that launched the app may have exported, and what the server reads. */
const LEAKS = {
	OTEL_EXPORTER_OTLP_ENDPOINT: "http://collector:4318",
	OTEL_SERVICE_NAME: "x",
	FENNEC_JWT_SECRET: "leak",
	FENNEC_HTTP_TOKEN: "an old one",
	AWS_SECRET_ACCESS_KEY: "leak",
	ELECTROBUN_APPLEAPIKEYPATH: "/tmp/key.p8",
	GITHUB_TOKEN: "leak",
	DYLD_INSERT_LIBRARIES: "/tmp/evil.dylib",
	LD_PRELOAD: "/tmp/evil.so",
};

test("macOS and Linux: the few variables a process needs and the token, and nothing else", () => {
	for (const platform of ["darwin", "linux"] as const) {
		const given = serverEnvironment(
			{
				HOME: "/Users/me",
				PATH: "/usr/bin",
				TMPDIR: "/var/folders/x/",
				LANG: "en_US.UTF-8",
				LC_ALL: "en_US.UTF-8",
				LC_CTYPE: "UTF-8",
				USER: "me",
				LOGNAME: "me",
				TZ: "Europe/Istanbul",
				// A Unix's names are its names: another case is another variable.
				Path: "/elsewhere",
				SystemRoot: "C:\\Windows",
				...LEAKS,
			},
			"the token",
			null,
			platform,
		);
		assert.deepEqual(Object.keys(given).sort(), ["FENNEC_HTTP_TOKEN", "HOME", "LANG", "LC_ALL", "LC_CTYPE", "LOGNAME", "PATH", "TMPDIR", "TZ", "USER"], platform);
		assert.equal(given["FENNEC_HTTP_TOKEN"], "the token", "the app's token, not one that was in the environment");
	}
});

test("Windows: the names compared case-blind, its own for the system's, temporary and user's folders, and nothing else", () => {
	const given = serverEnvironment(
		{
			// As Windows spells them.
			Path: "C:\\Windows\\system32;C:\\Windows",
			SystemRoot: "C:\\Windows",
			windir: "C:\\Windows",
			SystemDrive: "C:",
			TEMP: "C:\\Users\\me\\AppData\\Local\\Temp",
			TMP: "C:\\Users\\me\\AppData\\Local\\Temp",
			USERPROFILE: "C:\\Users\\me",
			USERNAME: "me",
			USERDOMAIN: "PC",
			HOMEDRIVE: "C:",
			HOMEPATH: "\\Users\\me",
			LOGONSERVER: "\\\\PC",
			TZ: "Europe/Istanbul",
			// Every name a Windows machine has that the server needs nothing of.
			APPDATA: "C:\\Users\\me\\AppData\\Roaming",
			LOCALAPPDATA: "C:\\Users\\me\\AppData\\Local",
			COMPUTERNAME: "PC",
			PSModulePath: "C:\\x",
			// The server's own names, in any case: still the app's to give.
			fennec_jwt_secret: "leak",
			Fennec_Http_Token: "an old one",
			...LEAKS,
		},
		"the token",
		null,
		"win32",
	);
	assert.deepEqual(Object.keys(given).sort(), [
		"FENNEC_HTTP_TOKEN",
		"HOMEDRIVE",
		"HOMEPATH",
		"LOGONSERVER",
		"Path",
		"SystemDrive",
		"SystemRoot",
		"TEMP",
		"TMP",
		"TZ",
		"USERDOMAIN",
		"USERNAME",
		"USERPROFILE",
		"windir",
	]);
	assert.equal(given["FENNEC_HTTP_TOKEN"], "the token");
	assert.equal(given["Path"], "C:\\Windows\\system32;C:\\Windows", "Path as it was set, under the name it was set by");
	assert.ok(kept("path", "win32") && kept("PATH", "win32") && kept("Lc_All", "win32"));
	assert.ok(!kept("path", "linux") && !kept("Path", "darwin"));
});

test("a variable that is not set is not passed as the text 'undefined'", () => {
	for (const platform of ["darwin", "linux", "win32"] as const) {
		assert.deepEqual(serverEnvironment({ HOME: undefined, PATH: "/bin" }, "t", null, platform), { PATH: "/bin", FENNEC_HTTP_TOKEN: "t" }, platform);
	}
});

test("the replication token a backup asks with is the app's, and none is none", () => {
	const env = { PATH: "/bin", FENNEC_REPLICATION_TOKEN: "an old one", FENNEC_KEY_FILE: "/tmp/leak.key" };
	for (const platform of ["darwin", "linux", "win32"] as const) {
		assert.deepEqual(serverEnvironment(env, "t", "r", platform), { PATH: "/bin", FENNEC_HTTP_TOKEN: "t", FENNEC_REPLICATION_TOKEN: "r" });
		// Under --sync off the server refuses one, so it is given none, not the shell's.
		assert.deepEqual(serverEnvironment(env, "t", null, platform), { PATH: "/bin", FENNEC_HTTP_TOKEN: "t" });
		// The shell is given the allowlist and what is named, and never a key file of the app's environment.
		assert.deepEqual(childEnvironment(env, { FENNEC_REPLICATION_TOKEN: "r" }, platform), { PATH: "/bin", FENNEC_REPLICATION_TOKEN: "r" });
		assert.deepEqual(childEnvironment(env, {}, platform), { PATH: "/bin" });
		assert.deepEqual(childEnvironment(env, { FENNEC_REPLICATION_TOKEN: "" }, platform), { PATH: "/bin" }, "an empty secret is none, as the shell reads it");
	}
});

test("this system's rule is the one the app uses", () => {
	const env = { PATH: "/bin", Path: "x", path: "y" };
	assert.deepEqual(serverEnvironment(env, "t"), serverEnvironment(env, "t", null, process.platform));
});

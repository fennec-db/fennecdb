// What the unit tests ask of the system they run on, the same question on
// macOS, Linux and Windows: a program's name, a process's arguments, whether a
// file is its owner's alone. And the tests a system cannot pass yet, each with
// its reason and the phase of the Windows and Linux plan that brings it (#578;
// docs/design/integrations.md#studio-desktop): `node --test` prints the reason
// beside the test it skipped.

import { execFileSync } from "node:child_process";
import { statSync } from "node:fs";

export const WINDOWS = process.platform === "win32";
/** A program's file name ends so: the release's Windows zip holds `fennec-server.exe`. */
export const EXE = WINDOWS ? ".exe" : "";

/**
 * Phase 2. A stop is `kill("SIGTERM")`, which on Windows libuv and Bun make
 * TerminateProcess: the server ends in a few ms with no checkpoint and exit 1,
 * not its shutdown and exit 0. The end of its stdin is that shutdown there
 * (`--exit-on-stdin-close`), which the app does not use to stop it yet.
 */
export const WINDOWS_STOP = WINDOWS && "phase 2: a stop on Windows is TerminateProcess (exit 1, no checkpoint) until the app ends the server's stdin";

/** A process's command line, as the system shows it to every user (`ps`), or to its own on Windows (WMI). */
export function argvOf(pid: number): string {
	if (!WINDOWS) return execFileSync("ps", ["-ww", "-o", "command=", "-p", String(pid)]).toString();
	const ps = `(Get-CimInstance Win32_Process -Filter "ProcessId=${Number(pid)}").CommandLine`;
	return execFileSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", ps]).toString();
}

/**
 * Whether only the file's owner can read it: mode 0600 on a Unix; on Windows,
 * where a mode is nothing and a file inherits its folder's ACL, a protected DACL
 * that grants this user full control and nobody anything else, as `fennec key`
 * makes it (fennec-http's `owner_only`). Says what it found otherwise.
 */
export function ownerAlone(path: string): true | string {
	if (!WINDOWS) {
		const mode = statSync(path).mode & 0o777;
		return mode === 0o600 || `mode ${mode.toString(8)}`;
	}
	// The path goes in by the environment, not into the script's text.
	const ps =
		"$acl = Get-Acl -LiteralPath $env:FENNEC_TEST_PATH; " +
		"$me = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value; " +
		"$rules = @($acl.Access); " +
		"$said = ($rules | ForEach-Object { $_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value + ' ' + $_.AccessControlType + ' ' + $_.FileSystemRights + ' inherited ' + $_.IsInherited }) -join '; '; " +
		"if ($acl.AreAccessRulesProtected -and $rules.Count -eq 1 -and $rules[0].AccessControlType -eq 'Allow' -and $rules[0].FileSystemRights -eq 'FullControl' -and $rules[0].IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value -eq $me) { 'owner alone' } " +
		"else { 'protected ' + $acl.AreAccessRulesProtected + ', me ' + $me + ': ' + $said }";
	const said = execFileSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", ps], { env: { ...process.env, FENNEC_TEST_PATH: path } })
		.toString()
		.trim();
	return said === "owner alone" || said;
}

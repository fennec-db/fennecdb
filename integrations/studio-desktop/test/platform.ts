// What the unit tests ask of the system they run on, the same question on
// macOS, Linux and Windows: a program's name, a process's arguments, whether a
// file is its owner's alone. A test a system cannot pass yet is skipped there
// with its reason and the phase of the Windows and Linux plan that brings it
// (#578; docs/design/integrations.md#studio-desktop), which `node --test`
// prints beside it; since phase 2 none is.

import { execFileSync } from "node:child_process";
import { existsSync, statSync } from "node:fs";
import { delimiter, join } from "node:path";

export const WINDOWS = process.platform === "win32";
/** A program's file name ends so: the release's Windows zip holds `fennec-server.exe`. */
export const EXE = WINDOWS ? ".exe" : "";

/**
 * PowerShell 7 (`pwsh`) where there is one, else Windows PowerShell: on
 * windows-11-arm the latter runs emulated and took over 40 s to ask WMI, `pwsh`
 * under a second (scripts/checks/lib.mjs's POWERSHELL says the same).
 */
const POWERSHELL = (process.env["PATH"] ?? "").split(delimiter).some((d) => d !== "" && existsSync(join(d, "pwsh.exe"))) ? "pwsh.exe" : "powershell.exe";

/** A process's command line, as the system shows it to every user (`ps`), or to its own on Windows (WMI). */
export function argvOf(pid: number): string {
	if (!WINDOWS) return execFileSync("ps", ["-ww", "-o", "command=", "-p", String(pid)]).toString();
	const ps = `(Get-CimInstance Win32_Process -Filter "ProcessId=${Number(pid)}").CommandLine`;
	return execFileSync(POWERSHELL, ["-NoProfile", "-NonInteractive", "-Command", ps]).toString();
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
	const said = execFileSync(POWERSHELL, ["-NoProfile", "-NonInteractive", "-Command", ps], { env: { ...process.env, FENNEC_TEST_PATH: path } })
		.toString()
		.trim();
	return said === "owner alone" || said;
}

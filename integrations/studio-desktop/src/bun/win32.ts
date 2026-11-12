// Windows' own calls, made through bun:ffi (Electrobun's main process reaches
// its native layer the same way), for what neither Bun nor Electrobun 2.0.4
// offers there:
//   - the user's SID, which names the single-instance pipe per user, and the
//     owner this process gives what it creates, which tells our pipe from one
//     another user made under that name first (instance.ts);
//   - that pipe's DACL, made its user's alone once it listens;
//   - a credential kept on this machine: Bun.secrets writes every item with
//     CRED_PERSIST_ENTERPRISE, which roams with a domain profile to every
//     machine the user signs in to (secrets.ts);
//   - the launcher's command line, which holds the file Explorer handed it
//     (handover.ts), and the registration that has Explorer hand it one
//     (associate.ts);
//   - a variable taken out of the process's environment block, which WebView2
//     reads and a delete from the app's `process.env` did not reach
//     (webview2.ts).
// Only the main process on Windows loads it; nothing here runs elsewhere, and
// `node --test` never imports it (bun:ffi is Bun's).
//
// Every call is one whose answer is its return value (a BOOL or a Win32 error
// code), never GetLastError read by a second call: between two calls through
// FFI the runtime may make calls of its own that set the thread's last error.

import { dlopen, FFIType, ptr, toArrayBuffer } from "bun:ffi";

const TOKEN_QUERY = 0x0008;
/** `TOKEN_INFORMATION_CLASS`: the user, and the owner it gives what it creates. */
const TOKEN_USER = 1;
const TOKEN_OWNER = 4;
/**
 * `SE_OBJECT_TYPE::SE_FILE_OBJECT`: a named pipe is a file object to the named
 * security calls, which open it by its `\\.\pipe\` path as CreateFile does.
 * The kernel object type, which the first version passed, takes by name only a
 * semaphore, an event, a mutex, a waitable timer or a file mapping: on the
 * runners every call answered 161 (ERROR_BAD_PATHNAME), so no pipe was made
 * its user's, and a second instance took ours for someone else's and ran
 * unguarded.
 */
const SE_FILE_OBJECT = 1;
const OWNER_SECURITY_INFORMATION = 0x1;
const DACL_SECURITY_INFORMATION = 0x4;
/** The DACL inherits nothing. */
const PROTECTED_DACL_SECURITY_INFORMATION = 0x80000000;
const SDDL_REVISION_1 = 1;
const CRED_TYPE_GENERIC = 1;
/** Kept on this machine for this user, never roamed with a profile. */
const CRED_PERSIST_LOCAL_MACHINE = 2;
/** winerror.h: every instance of the pipe is busy. */
export const ERROR_PIPE_BUSY = 231;
export const ERROR_ACCESS_DENIED = 5;

/** Enough to read another process's name and command line, which its user's processes are given. */
const PROCESS_QUERY_LIMITED_INFORMATION = 0x1000;
/** `PROCESSINFOCLASS::ProcessCommandLineInformation` (Windows 8.1 and later): the command line, as a UNICODE_STRING. */
const PROCESS_COMMAND_LINE_INFORMATION = 60;
/** The current user's hive: the predefined handle `(HKEY)(LONG)0x80000001`, sign-extended to 64 bits. */
const HKEY_CURRENT_USER = 0xffffffff80000001n;
/** The classes as Explorer reads them: the user's over the machine's (`(HKEY)(LONG)0x80000000`). */
const HKEY_CLASSES_ROOT = 0xffffffff80000000n;
const KEY_WRITE = 0x20006;
const REG_SZ = 1;
/** `RRF_RT_REG_SZ`: RegGetValueW answers a REG_SZ alone. */
const RRF_RT_REG_SZ = 0x2;
/** winerror.h: no such key or value. */
export const ERROR_FILE_NOT_FOUND = 2;
/** shlobj.h: a file type's association changed, so Explorer reads its icons again. */
const SHCNE_ASSOCCHANGED = 0x08000000;

let libs: ReturnType<typeof load> | null = null;
function load() {
	const kernel32 = dlopen("kernel32.dll", {
		GetCurrentProcess: { args: [], returns: FFIType.u64 },
		CloseHandle: { args: [FFIType.u64], returns: FFIType.i32 },
		LocalFree: { args: [FFIType.u64], returns: FFIType.u64 },
		lstrlenW: { args: [FFIType.u64], returns: FFIType.i32 },
		OpenProcess: { args: [FFIType.u32, FFIType.i32, FFIType.u32], returns: FFIType.u64 },
		QueryFullProcessImageNameW: { args: [FFIType.u64, FFIType.u32, FFIType.ptr, FFIType.ptr], returns: FFIType.i32 },
		SetEnvironmentVariableW: { args: [FFIType.ptr, FFIType.ptr], returns: FFIType.i32 },
		GetEnvironmentVariableW: { args: [FFIType.ptr, FFIType.ptr, FFIType.u32], returns: FFIType.u32 },
	});
	const ntdll = dlopen("ntdll.dll", {
		NtQueryInformationProcess: { args: [FFIType.u64, FFIType.u32, FFIType.ptr, FFIType.u32, FFIType.ptr], returns: FFIType.i32 },
	});
	const shell32 = dlopen("shell32.dll", {
		SHChangeNotify: { args: [FFIType.i32, FFIType.u32, FFIType.ptr, FFIType.ptr], returns: FFIType.void },
	});
	const advapi32 = dlopen("advapi32.dll", {
		OpenProcessToken: { args: [FFIType.u64, FFIType.u32, FFIType.ptr], returns: FFIType.i32 },
		GetTokenInformation: { args: [FFIType.u64, FFIType.u32, FFIType.ptr, FFIType.u32, FFIType.ptr], returns: FFIType.i32 },
		ConvertSidToStringSidW: { args: [FFIType.u64, FFIType.ptr], returns: FFIType.i32 },
		ConvertStringSecurityDescriptorToSecurityDescriptorW: { args: [FFIType.ptr, FFIType.u32, FFIType.ptr, FFIType.ptr], returns: FFIType.i32 },
		GetSecurityDescriptorDacl: { args: [FFIType.u64, FFIType.ptr, FFIType.ptr, FFIType.ptr], returns: FFIType.i32 },
		SetNamedSecurityInfoW: { args: [FFIType.ptr, FFIType.u32, FFIType.u32, FFIType.u64, FFIType.u64, FFIType.u64, FFIType.u64], returns: FFIType.u32 },
		GetNamedSecurityInfoW: { args: [FFIType.ptr, FFIType.u32, FFIType.u32, FFIType.ptr, FFIType.ptr, FFIType.ptr, FFIType.ptr, FFIType.ptr], returns: FFIType.u32 },
		CredWriteW: { args: [FFIType.ptr, FFIType.u32], returns: FFIType.i32 },
		RegCreateKeyExW: { args: [FFIType.u64, FFIType.ptr, FFIType.u32, FFIType.ptr, FFIType.u32, FFIType.u32, FFIType.ptr, FFIType.ptr, FFIType.ptr], returns: FFIType.i32 },
		RegSetValueExW: { args: [FFIType.u64, FFIType.ptr, FFIType.u32, FFIType.u32, FFIType.ptr, FFIType.u32], returns: FFIType.i32 },
		RegCloseKey: { args: [FFIType.u64], returns: FFIType.i32 },
		RegGetValueW: { args: [FFIType.u64, FFIType.ptr, FFIType.ptr, FFIType.u32, FFIType.ptr, FFIType.ptr, FFIType.ptr], returns: FFIType.i32 },
		RegDeleteTreeW: { args: [FFIType.u64, FFIType.ptr], returns: FFIType.i32 },
		RegDeleteKeyValueW: { args: [FFIType.u64, FFIType.ptr, FFIType.ptr], returns: FFIType.i32 },
	});
	return { k: kernel32.symbols, a: advapi32.symbols, n: ntdll.symbols, s: shell32.symbols };
}
const win = () => (libs ??= load());

/** A string as Windows takes one: UTF-16LE, NUL-ended. */
const wide = (s: string) => Buffer.from(`${s}\0`, "utf16le");

/** The UTF-16 string at `address`, which the caller frees. */
function readWide(address: bigint): string {
	const n = win().k.lstrlenW(address);
	if (n <= 0) return "";
	return Buffer.from(toArrayBuffer(Number(address), 0, n * 2)).toString("utf16le");
}

/** The SID at `sid` as text (`S-1-5-21-...`). */
function sidText(sid: bigint): string {
	const out = new BigUint64Array(1);
	if (!win().a.ConvertSidToStringSidW(sid, out)) throw new Error("ConvertSidToStringSidW failed");
	try {
		return readWide(out[0]!);
	} finally {
		win().k.LocalFree(out[0]!);
	}
}

/** One SID of this process's token: its user's, or the owner it gives what it makes. */
function tokenSid(kind: typeof TOKEN_USER | typeof TOKEN_OWNER): string {
	const { k, a } = win();
	const handle = new BigUint64Array(1);
	if (!a.OpenProcessToken(k.GetCurrentProcess(), TOKEN_QUERY, handle)) throw new Error("OpenProcessToken failed");
	try {
		// TOKEN_USER is a SID_AND_ATTRIBUTES, TOKEN_OWNER a PSID: either way the
		// answer begins with a pointer to the SID, which is inside the answer.
		const buf = new BigUint64Array(128);
		const size = new Uint32Array(1);
		if (!a.GetTokenInformation(handle[0]!, kind, buf, buf.byteLength, size)) throw new Error("GetTokenInformation failed");
		return sidText(buf[0]!);
	} finally {
		k.CloseHandle(handle[0]!);
	}
}

/**
 * Takes `name` out of this process's own environment block, the one Windows'
 * calls read and WebView2 reads when Electrobun makes a view. Not `delete
 * process.env[name]` alone: on both Windows runners the app, started
 * de-elevated with WEBVIEW2_USER_DATA_FOLDER and deleting it so, still had
 * WebView2 make that folder, though a Worker of the same Bun outside the app
 * took a name out of the block by the same delete; why is not known
 * (docs/design/integrations.md#studio-desktop).
 */
export function unsetEnvironment(name: string): void {
	win().k.SetEnvironmentVariableW(wide(name), null);
	// Judged by what the block holds after, not by the call's BOOL: a name the
	// block never held fails the call, and is what was asked.
	if (inEnvironment(name)) throw new Error(`${name} is still in the environment`);
}

/** Whether this process's environment block holds `name` (asked for a size of 0, a held one, empty too, answers at least 1). */
export function inEnvironment(name: string): boolean {
	return win().k.GetEnvironmentVariableW(wide(name), null, 0) !== 0;
}

/** This user's SID, which no other user of the machine has. */
export const userSid = () => tokenSid(TOKEN_USER);
/** The owner this process gives what it creates: its user, or Administrators when it runs elevated. */
export const ownerSid = () => tokenSid(TOKEN_OWNER);

/**
 * Makes the named pipe `name` its user's alone: a protected DACL of one entry,
 * every right to `sid`. A pipe made with no security attributes (libuv's,
 * Bun's) lets Everyone open it to read; with this no other user opens it at
 * all, and none may make an instance of it.
 */
export function restrictPipe(name: string, sid: string): void {
	const { k, a } = win();
	if (!/^S-1-[0-9-]+$/.test(sid)) throw new Error(`not a SID: ${sid}`);
	const sd = new BigUint64Array(1);
	if (!a.ConvertStringSecurityDescriptorToSecurityDescriptorW(wide(`D:P(A;;FA;;;${sid})`), SDDL_REVISION_1, sd, null)) {
		throw new Error("ConvertStringSecurityDescriptorToSecurityDescriptorW failed");
	}
	try {
		const present = new Int32Array(1);
		const dacl = new BigUint64Array(1);
		const defaulted = new Int32Array(1);
		if (!a.GetSecurityDescriptorDacl(sd[0]!, present, dacl, defaulted) || !present[0]) throw new Error("the security descriptor has no DACL");
		const code = a.SetNamedSecurityInfoW(wide(name), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION, 0n, 0n, dacl[0]!, 0n);
		if (code !== 0) throw new Error(`SetNamedSecurityInfoW: error ${code}`);
	} finally {
		k.LocalFree(sd[0]!);
	}
}

/**
 * The owner of the named pipe `name` (a SID), or the Win32 error that kept it
 * from being read: ERROR_ACCESS_DENIED where its DACL lets this user in not at
 * all, ERROR_PIPE_BUSY where every instance is in use. Asking opens the pipe
 * as a client would, so its server sees a connection that ends at once.
 */
export function pipeOwner(name: string): { sid: string } | { error: number } {
	const { k, a } = win();
	const owner = new BigUint64Array(1);
	const sd = new BigUint64Array(1);
	const code = a.GetNamedSecurityInfoW(wide(name), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, owner, null, null, null, sd);
	if (code !== 0) return { error: code };
	try {
		return { sid: sidText(owner[0]!) };
	} finally {
		k.LocalFree(sd[0]!);
	}
}

/**
 * The text form of the named pipe `name`'s security descriptor: its owner and
 * DACL. What a check reads back after the app made the pipe its user's alone.
 */
export function pipeSddl(name: string): string {
	const { k, a } = win();
	const owner = new BigUint64Array(1);
	const dacl = new BigUint64Array(1);
	const sd = new BigUint64Array(1);
	const code = a.GetNamedSecurityInfoW(wide(name), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION, owner, null, dacl, null, sd);
	if (code !== 0) throw new Error(`GetNamedSecurityInfoW: error ${code}`);
	try {
		const conv = dlopen("advapi32.dll", {
			ConvertSecurityDescriptorToStringSecurityDescriptorW: { args: [FFIType.u64, FFIType.u32, FFIType.u32, FFIType.ptr, FFIType.ptr], returns: FFIType.i32 },
		}).symbols;
		const text = new BigUint64Array(1);
		if (!conv.ConvertSecurityDescriptorToStringSecurityDescriptorW(sd[0]!, SDDL_REVISION_1, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION, text, null)) {
			throw new Error("ConvertSecurityDescriptorToStringSecurityDescriptorW failed");
		}
		try {
			return readWide(text[0]!);
		} finally {
			k.LocalFree(text[0]!);
		}
	} finally {
		k.LocalFree(sd[0]!);
	}
}

/**
 * Writes the generic credential `<service>/<name>` -- the item Bun.secrets
 * reads and deletes by that name, its value the same UTF-8 bytes -- kept on
 * this machine (CRED_PERSIST_LOCAL_MACHINE), not with a roaming profile.
 * CredWriteW replaces an item of that name whatever it was kept as.
 */
export function writeLocalCredential(service: string, name: string, value: string): void {
	const target = wide(`${service}/${name}`);
	const user = wide(name);
	const blob = Buffer.from(value, "utf8");
	// CREDENTIALW on a 64-bit Windows: Flags 0, Type 4, TargetName 8, Comment 16,
	// LastWritten 24 (a FILETIME, 8 bytes), CredentialBlobSize 32, CredentialBlob 40,
	// Persist 48, AttributeCount 52, Attributes 56, TargetAlias 64, UserName 72; 80 in all.
	const cred = new ArrayBuffer(80);
	const v = new DataView(cred);
	v.setUint32(4, CRED_TYPE_GENERIC, true);
	v.setBigUint64(8, BigInt(ptr(target)), true);
	v.setUint32(32, blob.length, true);
	v.setBigUint64(40, BigInt(ptr(blob)), true);
	v.setUint32(48, CRED_PERSIST_LOCAL_MACHINE, true);
	v.setBigUint64(72, BigInt(ptr(user)), true);
	const ok = win().a.CredWriteW(new Uint8Array(cred), 0);
	// The value's bytes, scrubbed as Bun scrubs its own copy.
	blob.fill(0);
	// Held to here: the struct points into them.
	void target;
	void user;
	if (!ok) throw new Error("Windows' Credential Manager did not keep the token (CredWriteW failed)");
}

/**
 * The program and the command line of process `pid` (one of this user's): the
 * launcher's, which holds the file Explorer handed it (handover.ts). Read with
 * the least access there is, as Task Manager reads a command line.
 */
export function processCommandLine(pid: number): { image: string; commandLine: string } {
	const { k, n } = win();
	const handle = k.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
	if (!handle) throw new Error(`OpenProcess(${pid}) failed`);
	try {
		const name = new Uint16Array(32768);
		const size = new Uint32Array([name.length]);
		if (!k.QueryFullProcessImageNameW(handle, 0, name, size)) throw new Error("QueryFullProcessImageNameW failed");
		const image = Buffer.from(name.buffer, 0, size[0]! * 2).toString("utf16le");
		// A UNICODE_STRING (Length, MaximumLength, 4 bytes of padding, Buffer) and then
		// the string it points at, in the same buffer: 32 767 characters at most.
		const answer = new BigUint64Array(8200);
		const at = ptr(answer);
		const returned = new Uint32Array(1);
		const status = n.NtQueryInformationProcess(handle, PROCESS_COMMAND_LINE_INFORMATION, at, answer.byteLength, returned);
		if (status !== 0) throw new Error(`NtQueryInformationProcess: status 0x${(status >>> 0).toString(16)}`);
		const v = new DataView(answer.buffer);
		const length = v.getUint16(0, true);
		const offset = Number(v.getBigUint64(8, true) - BigInt(at));
		if (offset < 16 || offset + length > answer.byteLength) throw new Error("the command line is not where its answer says");
		return { image, commandLine: Buffer.from(answer.buffer, offset, length).toString("utf16le") };
	} finally {
		k.CloseHandle(handle);
	}
}

/**
 * The REG_SZ `name` (null: the key's default value) of `HKCU\<key>`, or with
 * `classes` of `HKCR\<key>` (the user's classes over the machine's), or null
 * where there is none.
 */
export function readString(key: string, name: string | null, classes = false): string | null {
	const data = new Uint16Array(32768);
	const size = new Uint32Array([data.byteLength]);
	const root = classes ? HKEY_CLASSES_ROOT : HKEY_CURRENT_USER;
	const code = win().a.RegGetValueW(root, wide(key), name === null ? null : wide(name), RRF_RT_REG_SZ, null, data, size);
	if (code === ERROR_FILE_NOT_FOUND) return null;
	if (code !== 0) throw new Error(`RegGetValueW(${classes ? "HKCR" : "HKCU"}\\${key}): error ${code}`);
	// The size counts the closing NUL.
	return Buffer.from(data.buffer, 0, Math.max(0, size[0]! - 2)).toString("utf16le");
}

/** Writes the REG_SZ `name` (null: the key's default value) of `HKCU\<key>`, making the key where it is missing. */
export function writeUserString(key: string, name: string | null, value: string): void {
	const { a } = win();
	const opened = new BigUint64Array(1);
	const made = a.RegCreateKeyExW(HKEY_CURRENT_USER, wide(key), 0, null, 0, KEY_WRITE, null, opened, null);
	if (made !== 0) throw new Error(`RegCreateKeyExW(HKCU\\${key}): error ${made}`);
	try {
		const data = wide(value);
		const code = a.RegSetValueExW(opened[0]!, name === null ? null : wide(name), 0, REG_SZ, data, data.byteLength);
		if (code !== 0) throw new Error(`RegSetValueExW(HKCU\\${key}): error ${code}`);
	} finally {
		a.RegCloseKey(opened[0]!);
	}
}

/** Deletes `HKCU\<key>` and everything under it; none there is no failure. */
export function deleteUserKey(key: string): void {
	const code = win().a.RegDeleteTreeW(HKEY_CURRENT_USER, wide(key));
	if (code !== 0 && code !== ERROR_FILE_NOT_FOUND) throw new Error(`RegDeleteTreeW(HKCU\\${key}): error ${code}`);
}

/** Deletes the value `name` (null: the key's default) of `HKCU\<key>`; none there is no failure. */
export function deleteUserValue(key: string, name: string | null): void {
	const code = win().a.RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(key), name === null ? null : wide(name));
	if (code !== 0 && code !== ERROR_FILE_NOT_FOUND) throw new Error(`RegDeleteKeyValueW(HKCU\\${key}): error ${code}`);
}

/** Tells Explorer a file type's association changed, so it shows the type's new icon and handler. */
export function associationsChanged(): void {
	win().s.SHChangeNotify(SHCNE_ASSOCCHANGED, 0, null, null);
}

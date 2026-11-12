// Windows: keys pressed into a window of the app as a person's would be, for
// `checks/menu.mjs`: a click where a person would click into its page, then
// each key down and up through the system's input stream (keybd_event,
// mouse_event), which delivers it to whatever has the focus, WebView2's page
// included. Nothing here brings a window forward but the click: the Alt pressed
// and let go before a SetForegroundWindow, as the first version did, can put
// the menu bar of a window that is in front by then in menu mode, where the
// keys after it go. Test variant only: testhooks.ts imports it, and nothing else
// does.

import { dlopen, FFIType } from "bun:ffi";
import { processCommandLine } from "./win32.ts";

const KEYEVENTF_KEYUP = 0x2;
const MOUSEEVENTF_MOVE = 0x1;
const MOUSEEVENTF_LEFTDOWN = 0x2;
const MOUSEEVENTF_LEFTUP = 0x4;
const MOUSEEVENTF_ABSOLUTE = 0x8000;
const GW_HWNDNEXT = 2;
const GW_CHILD = 5;
const GWL_EXSTYLE = -20;
const SM_CYCAPTION = 4;
const SM_CYMENU = 15;
const VK: Record<string, number> = { ctrl: 0x11, shift: 0x10, alt: 0x12 };

let user32: ReturnType<typeof load> | null = null;
function load() {
	return dlopen("user32.dll", {
		keybd_event: { args: [FFIType.u8, FFIType.u8, FFIType.u32, FFIType.u64], returns: FFIType.void },
		GetForegroundWindow: { args: [], returns: FFIType.u64 },
		GetLastInputInfo: { args: [FFIType.ptr], returns: FFIType.i32 },
		ClientToScreen: { args: [FFIType.u64, FFIType.ptr], returns: FFIType.i32 },
		SetCursorPos: { args: [FFIType.i32, FFIType.i32], returns: FFIType.i32 },
		mouse_event: { args: [FFIType.u32, FFIType.u32, FFIType.u32, FFIType.u32, FFIType.u64], returns: FFIType.void },
		GetWindowThreadProcessId: { args: [FFIType.u64, FFIType.ptr], returns: FFIType.u32 },
		GetWindowTextW: { args: [FFIType.u64, FFIType.ptr, FFIType.i32], returns: FFIType.i32 },
		WindowFromPoint: { args: [FFIType.u64], returns: FFIType.u64 },
		GetClassNameW: { args: [FFIType.u64, FFIType.ptr, FFIType.i32], returns: FFIType.i32 },
		GetGUIThreadInfo: { args: [FFIType.u32, FFIType.ptr], returns: FFIType.i32 },
		GetWindowRect: { args: [FFIType.u64, FFIType.ptr], returns: FFIType.i32 },
		GetSystemMetrics: { args: [FFIType.i32], returns: FFIType.i32 },
		GetDpiForWindow: { args: [FFIType.u64], returns: FFIType.u32 },
		IsChild: { args: [FFIType.u64, FFIType.u64], returns: FFIType.i32 },
		GetCursorPos: { args: [FFIType.ptr], returns: FFIType.i32 },
		GetWindow: { args: [FFIType.u64, FFIType.u32], returns: FFIType.u64 },
		IsWindowVisible: { args: [FFIType.u64], returns: FFIType.i32 },
		GetWindowLongPtrW: { args: [FFIType.u64, FFIType.i32], returns: FFIType.i64 },
	}).symbols;
}
const u = () => (user32 ??= load());

const key = (vk: number, up: boolean) => u().keybd_event(vk, 0, up ? KEYEVENTF_KEYUP : 0, 0n);

/** The virtual key of a letter or digit, as `"n"` or `"7"`. */
function vkOf(k: string): number {
	const c = k.toUpperCase();
	if (!/^[A-Z0-9]$/.test(c)) throw new Error(`no key named ${k}`);
	return c.charCodeAt(0);
}

/** Whether `hwnd` is the window in front. */
export const isFront = (hwnd: bigint) => u().GetForegroundWindow() === hwnd;

/** A chord (`"ctrl+shift+o"`): its modifiers held while its key goes down and up. */
export function chord(spec: string): void {
	const parts = spec.toLowerCase().split("+");
	const last = vkOf(parts.pop()!);
	const mods = parts.map((m) => {
		const vk = VK[m];
		if (vk === undefined) throw new Error(`no modifier named ${m}`);
		return vk;
	});
	for (const m of mods) key(m, false);
	key(last, false);
	key(last, true);
	for (const m of mods.reverse()) key(m, true);
}

/** Letters and digits typed one after another. */
export function type(text: string): void {
	for (const c of text) {
		key(vkOf(c), false);
		key(vkOf(c), true);
	}
}

/**
 * A left click at `x`, `y` of `hwnd`'s client area (device pixels): where it
 * lands takes the focus, and its window comes to the front, as a person's
 * click brings it. Each half carries the point itself (absolute, normalized to
 * the primary screen), not the cursor's place a SetCursorPos left. Says where
 * on the screen, and where the cursor is after.
 */
export function click(hwnd: bigint, x: number, y: number): { x: number; y: number; cursor: number[] } {
	const point = new Int32Array([x, y]);
	if (!u().ClientToScreen(hwnd, point)) throw new Error("ClientToScreen failed");
	if (!u().SetCursorPos(point[0]!, point[1]!)) throw new Error("SetCursorPos failed");
	const nx = Math.round((point[0]! * 65535) / Math.max(1, u().GetSystemMetrics(0) - 1));
	const ny = Math.round((point[1]! * 65535) / Math.max(1, u().GetSystemMetrics(1) - 1));
	u().mouse_event(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_LEFTDOWN, nx, ny, 0, 0n);
	u().mouse_event(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_LEFTUP, nx, ny, 0, 0n);
	const cursor = new Int32Array(2);
	u().GetCursorPos(cursor);
	return { x: point[0]!, y: point[1]!, cursor: [...cursor] };
}

/** The window in front, by its process, its title and, another program's, its program: where a window's keys went when it was not. */
export function foreground(): { pid: number; title: string; image?: string } {
	const hwnd = u().GetForegroundWindow();
	const pid = new Uint32Array(1);
	u().GetWindowThreadProcessId(hwnd, pid);
	const title = new Uint16Array(256);
	const n = u().GetWindowTextW(hwnd, title, title.length);
	const other = pid[0]! !== 0 && pid[0]! !== process.pid;
	return { pid: pid[0]!, title: Buffer.from(title.buffer, 0, Math.max(0, n) * 2).toString("utf16le"), ...(other ? { image: imageOf(pid[0]!) } : {}) };
}

/**
 * A left click on `hwnd`'s title bar, at the first point along it, from its
 * left, where the window under the point is `hwnd` itself (no other window
 * lies over it): the middle of the caption, above the menu bar and the client
 * area (ClientToScreen of the client's top, less SM_CYMENU and half of
 * SM_CYCAPTION). As a person brings a window forward without touching its
 * page. Says where, and the window it found there.
 */
export function clickTitle(hwnd: bigint): { x: number; y: number; cursor: number[]; hit: unknown } {
	const top = new Int32Array([0, 0]);
	if (!u().ClientToScreen(hwnd, top)) throw new Error("ClientToScreen failed");
	const rect = new Int32Array(4);
	u().GetWindowRect(hwnd, rect);
	const y = top[1]! - u().GetSystemMetrics(SM_CYMENU) - Math.round(u().GetSystemMetrics(SM_CYCAPTION) / 2);
	const at = (x: number) => BigInt.asUintN(64, (BigInt(y) << 32n) | BigInt.asUintN(32, BigInt(x)));
	const screenRight = u().GetSystemMetrics(0);
	for (let x = rect[0]! + 60; x < Math.min(rect[2]!, screenRight) - 160; x += 40) {
		if (u().WindowFromPoint(at(x)) !== hwnd) continue;
		const done = click(hwnd, x - top[0]!, y - top[1]!);
		return { ...done, hit: describeWindow(hwnd, hwnd) };
	}
	throw new Error(`no point of the title bar at y ${y} is the window's own: another window lies over all of it`);
}

/**
 * The tick of the last input the session took (GetLastInputInfo), injected
 * input included: unchanged after keys pressed, the system took none of them
 * (seen on the windows-11-arm runner, where typed text never reached a page).
 */
export function lastInput(): number {
	const info = new Uint32Array([8, 0]);
	return u().GetLastInputInfo(info) ? info[1]! : -1;
}

/** A window as a check can name it: its process, its class and whether it is `top` or in it; one of another program's, its program too. */
function describeWindow(hwnd: bigint, top: bigint) {
	if (!hwnd) return null;
	const pid = new Uint32Array(1);
	u().GetWindowThreadProcessId(hwnd, pid);
	const name = new Uint16Array(256);
	const n = u().GetClassNameW(hwnd, name, name.length);
	const ours = hwnd === top || u().IsChild(top, hwnd) !== 0;
	return { pid: pid[0]!, class: Buffer.from(name.buffer, 0, Math.max(0, n) * 2).toString("utf16le"), ours, ...(ours ? {} : { image: imageOf(pid[0]!) }) };
}

/** A process's program, or why it is not known. */
function imageOf(pid: number): string {
	try {
		return processCommandLine(pid).image;
	} catch (e) {
		return `(${(e as Error).message})`;
	}
}

/**
 * Where input to `hwnd` goes, for a check whose keys never reached a page:
 * the window under the point a click lands on (`x`, `y` on the screen), the
 * window that has the keyboard focus on the window's own thread and the
 * thread's flags (GetGUIThreadInfo: 0x4 a menu bar's menu mode), the window's
 * rectangle, the screen's size and the window's DPI. Test variant only.
 */
export function inputTargets(hwnd: bigint, at: { x: number; y: number } | null) {
	const thread = u().GetWindowThreadProcessId(hwnd, null);
	// GUITHREADINFO: cbSize, flags, then hwndActive, hwndFocus, ... (pointer-sized, 8-aligned on 64-bit).
	const info = new BigUint64Array(9);
	const head = new Uint32Array(info.buffer, 0, 2);
	head[0] = 72;
	const got = u().GetGUIThreadInfo(thread, info) !== 0;
	const rect = new Int32Array(4);
	u().GetWindowRect(hwnd, rect);
	const point = at ? BigInt.asUintN(64, (BigInt(at.y) << 32n) | BigInt.asUintN(32, BigInt(at.x))) : null;
	return {
		underClick: point === null ? null : describeWindow(u().WindowFromPoint(point), hwnd),
		focus: got ? describeWindow(info[2]!, hwnd) : null,
		active: got ? describeWindow(info[1]!, hwnd) : null,
		flags: got ? head[1]! : null,
		rect: [...rect],
		screen: [u().GetSystemMetrics(0), u().GetSystemMetrics(1)],
		dpi: u().GetDpiForWindow(hwnd),
		arch: process.arch,
	};
}

/**
 * The windows within `hwnd`, each with its class, process, rectangle, whether
 * it is visible and its extended style (WS_EX_TRANSPARENT, 0x20, lets a click
 * through), to a few levels: where a click into the page can land.
 */
export function windowTree(hwnd: bigint, depth = 4): unknown[] {
	const out: unknown[] = [];
	let child = depth > 0 ? u().GetWindow(hwnd, GW_CHILD) : 0n;
	for (let n = 0; child && n < 32; n++) {
		const rect = new Int32Array(4);
		u().GetWindowRect(child, rect);
		out.push({
			...describeWindow(child, hwnd),
			rect: [...rect],
			visible: u().IsWindowVisible(child) !== 0,
			exStyle: Number(u().GetWindowLongPtrW(child, GWL_EXSTYLE)),
			children: windowTree(child, depth - 1),
		});
		child = u().GetWindow(child, GW_HWNDNEXT);
	}
	return out;
}

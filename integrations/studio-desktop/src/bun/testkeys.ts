// Windows: keys pressed into a window of the app as a person's would be, for
// `checks/menu.mjs`: the window brought to the front, a click where a person
// would click to put the focus in its page, then each key down and up through
// the system's input stream (keybd_event, mouse_event), which delivers it to
// whatever has the focus in that window, WebView2's page included. Test
// variant only: testhooks.ts imports it, and nothing else does.

import { dlopen, FFIType } from "bun:ffi";

const KEYEVENTF_KEYUP = 0x2;
const MOUSEEVENTF_LEFTDOWN = 0x2;
const MOUSEEVENTF_LEFTUP = 0x4;
const VK: Record<string, number> = { ctrl: 0x11, shift: 0x10, alt: 0x12 };

let user32: ReturnType<typeof load> | null = null;
function load() {
	return dlopen("user32.dll", {
		keybd_event: { args: [FFIType.u8, FFIType.u8, FFIType.u32, FFIType.u64], returns: FFIType.void },
		SetForegroundWindow: { args: [FFIType.u64], returns: FFIType.i32 },
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

/**
 * Brings `hwnd` to the front: an Alt pressed and let go first, so this process
 * received the last input event, which Windows asks of a process that sets
 * the foreground window. Says whether it is in front.
 */
export function toFront(hwnd: bigint): boolean {
	// Not again for a window in front: an Alt there would put its menu bar in menu mode, where typed keys go.
	if (isFront(hwnd)) return true;
	key(VK["alt"]!, false);
	key(VK["alt"]!, true);
	u().SetForegroundWindow(hwnd);
	return isFront(hwnd);
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
 * click brings it (SetForegroundWindow alone was refused once on the arm64
 * runner, the foreground lock's doing). Says where on the screen.
 */
export function click(hwnd: bigint, x: number, y: number): { x: number; y: number } {
	const point = new Int32Array([x, y]);
	if (!u().ClientToScreen(hwnd, point)) throw new Error("ClientToScreen failed");
	if (!u().SetCursorPos(point[0]!, point[1]!)) throw new Error("SetCursorPos failed");
	u().mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0n);
	u().mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0n);
	return { x: point[0]!, y: point[1]! };
}

/** The window in front, by its process and title: what a refused SetForegroundWindow left there. */
export function foreground(): { pid: number; title: string } {
	const hwnd = u().GetForegroundWindow();
	const pid = new Uint32Array(1);
	u().GetWindowThreadProcessId(hwnd, pid);
	const title = new Uint16Array(256);
	const n = u().GetWindowTextW(hwnd, title, title.length);
	return { pid: pid[0]!, title: Buffer.from(title.buffer, 0, Math.max(0, n) * 2).toString("utf16le") };
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

/** A window as a check can name it: its process, its class and whether it is `top` or in it. */
function describeWindow(hwnd: bigint, top: bigint) {
	if (!hwnd) return null;
	const pid = new Uint32Array(1);
	u().GetWindowThreadProcessId(hwnd, pid);
	const name = new Uint16Array(256);
	const n = u().GetClassNameW(hwnd, name, name.length);
	return { pid: pid[0]!, class: Buffer.from(name.buffer, 0, Math.max(0, n) * 2).toString("utf16le"), ours: hwnd === top || u().IsChild(top, hwnd) !== 0 };
}

/**
 * Where input to `hwnd` goes, for a check whose keys never reached a page:
 * the window under the point a click lands on (`x`, `y` on the screen), the
 * window that has the keyboard focus on the thread in front
 * (GetGUIThreadInfo), the window's rectangle, the screen's size and the
 * window's DPI. Test variant only.
 */
export function inputTargets(hwnd: bigint, at: { x: number; y: number } | null) {
	const front = u().GetForegroundWindow();
	const thread = u().GetWindowThreadProcessId(front, null);
	// GUITHREADINFO: cbSize, flags, then hwndActive, hwndFocus, ... (pointer-sized, 8-aligned on 64-bit).
	const info = new BigUint64Array(9);
	new Uint32Array(info.buffer)[0] = 72;
	const got = u().GetGUIThreadInfo(thread, info) !== 0;
	const rect = new Int32Array(4);
	u().GetWindowRect(hwnd, rect);
	const point = at ? BigInt.asUintN(64, (BigInt(at.y) << 32n) | BigInt.asUintN(32, BigInt(at.x))) : null;
	return {
		underClick: point === null ? null : describeWindow(u().WindowFromPoint(point), hwnd),
		focus: got ? describeWindow(info[2]!, hwnd) : null,
		active: got ? describeWindow(info[1]!, hwnd) : null,
		rect: [...rect],
		screen: [u().GetSystemMetrics(0), u().GetSystemMetrics(1)],
		dpi: u().GetDpiForWindow(hwnd),
		arch: process.arch,
	};
}

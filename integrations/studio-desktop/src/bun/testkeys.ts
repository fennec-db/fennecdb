// Windows: keys pressed into a window of the app as a person's would be, for
// `checks/menu.mjs`: the window brought to the front, then each key down and
// up through the system's input stream (keybd_event), which delivers it to
// whatever has the focus in that window, WebView2's page included. Test
// variant only: testhooks.ts imports it, and nothing else does.

import { dlopen, FFIType } from "bun:ffi";

const KEYEVENTF_KEYUP = 0x2;
const VK: Record<string, number> = { ctrl: 0x11, shift: 0x10, alt: 0x12 };

let user32: ReturnType<typeof load> | null = null;
function load() {
	return dlopen("user32.dll", {
		keybd_event: { args: [FFIType.u8, FFIType.u8, FFIType.u32, FFIType.u64], returns: FFIType.void },
		SetForegroundWindow: { args: [FFIType.u64], returns: FFIType.i32 },
		GetForegroundWindow: { args: [], returns: FFIType.u64 },
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
	key(VK["alt"]!, false);
	key(VK["alt"]!, true);
	u().SetForegroundWindow(hwnd);
	return u().GetForegroundWindow() === hwnd;
}

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

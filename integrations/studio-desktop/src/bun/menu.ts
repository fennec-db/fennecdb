// The application menu: macOS's menu bar, and on Windows a menu bar in each
// window (Electrobun 2.0.4 has none on Linux, where the main window's buttons
// do what its File menu does). Accelerators are CmdOrCtrl: Command on macOS,
// Ctrl on Windows. A bare key was Command on macOS alone, and "shift+o" was
// Shift and O with no Command anywhere.

import { basename } from "node:path";
import { ApplicationMenu } from "electrobun/main";
import { onChange, openFile } from "./app.ts";
import { chooseFile, showLauncher, showNewDatabase } from "./pages.ts";
import { store } from "./store.ts";

export function refreshMenu() {
	const recents = store.recents().slice(0, 10);
	ApplicationMenu.setApplicationMenu([
		{
			label: "fennec studio",
			submenu: [{ label: "About fennec studio", role: "about" } as any, { type: "separator" }, { role: "hide" } as any, { role: "quit" }],
		},
		{
			label: "File",
			submenu: [
				{ label: "New Database…", action: "new", accelerator: "CmdOrCtrl+N" } as any,
				{ label: "Open…", action: "open", accelerator: "CmdOrCtrl+O" },
				{
					label: "Open Recent",
					submenu: recents.length
						? recents.map((r) => ({ label: basename(r.path), action: "open-recent", data: { path: r.path } }))
						: [{ label: "No recent files", enabled: false }],
				},
				{ label: "Connect to a Server…", action: "launcher", accelerator: "CmdOrCtrl+Shift+O" } as any,
				{ type: "separator" },
				{ role: "close" },
			],
		},
		{
			label: "Edit",
			submenu: [
				{ role: "undo" },
				{ role: "redo" },
				{ type: "separator" },
				{ role: "cut" },
				{ role: "copy" },
				{ role: "paste" },
				{ role: "selectAll" },
			],
		},
	] as any);
}

export function startMenu() {
	onChange(refreshMenu);
	refreshMenu();
	ApplicationMenu.on("application-menu-clicked", (event: any) => {
		const d = event?.data ?? event;
		const action = d?.action;
		const data = d?.data;
		if (__TESTABLE__) void import("./testhooks.ts").then((h) => h.report("menu", { action, data }));
		if (action === "new") void showNewDatabase();
		else if (action === "open") void chooseFile();
		else if (action === "open-recent" && data?.path) void openFile(data.path);
		else if (action === "launcher") void showLauncher();
	});
}

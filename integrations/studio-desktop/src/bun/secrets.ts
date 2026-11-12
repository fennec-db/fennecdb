// Connection tokens live in the system's credential store, never in
// state.json, and never in a file of the app's own: where there is no store,
// a token is refused in words, not kept elsewhere.
//
// Electrobun 2.0.4 has no credential API of its own. The main process is Bun,
// and Bun.secrets (Bun >= 1.2.21; the app bundles 1.4) is one: the macOS
// Keychain (Security.framework), Windows' Credential Manager, a Secret Service
// through libsecret on Linux. Each system takes something more:
//   - Windows: Bun writes every item with CRED_PERSIST_ENTERPRISE, which roams
//     with a domain profile to every machine the user signs in to. The app
//     writes the same item (`<service>/<name>`, the same bytes) itself, kept on
//     this machine (win32.ts), and reads and deletes it through Bun.
//   - Linux: libsecret says neither "no store" nor "no keyring" in time: a
//     service that never answers held every call 25 s, and with no default
//     collection `get` answered null, as if nothing were saved. The store is
//     asked first, bounded (secretservice.ts), and a store that is not there
//     is an error in words, which every caller takes as one -- never as "no
//     token".
// There is no fallback to a command-line tool on purpose: macOS's `security
// add-generic-password -w <secret>` puts the secret in argv, where `ps` shows
// it to every user, which is the one place this app never puts a token.

import { askSecretService, storeRefusal } from "./secretservice.ts";

// The app's identifier (the test variant has its own), so the checks never read
// or write a real install's items.
const SERVICE = __KEYCHAIN_SERVICE__;

type Secrets = {
	get(o: { service: string; name: string }): Promise<string | null>;
	set(o: { service: string; name: string; value: string }): Promise<void>;
	delete(o: { service: string; name: string }): Promise<boolean>;
};

const bunStore = (): Secrets => {
	const s = (Bun as unknown as { secrets?: Secrets }).secrets;
	if (typeof s?.get !== "function") throw new Error("this runtime has no credential store (Bun.secrets)");
	return s;
};

/** Linux: throws, in words, unless a Secret Service with a default collection answers. Elsewhere nothing to ask. */
async function storeThere(): Promise<void> {
	if (process.platform !== "linux") return;
	const answer = await askSecretService();
	if (!answer.ok) throw new Error(storeRefusal(answer));
}

export const secrets = {
	kind: process.platform === "darwin" ? "the macOS Keychain" : process.platform === "win32" ? "Windows' Credential Manager" : "the Secret Service (GNOME Keyring or KWallet)",
	async get(name: string): Promise<string | null> {
		await storeThere();
		return (await bunStore().get({ service: SERVICE, name })) ?? null;
	},
	async set(name: string, value: string): Promise<void> {
		await storeThere();
		if (process.platform === "win32") return (await import("./win32.ts")).writeLocalCredential(SERVICE, name, value);
		await bunStore().set({ service: SERVICE, name, value });
	},
	async delete(name: string): Promise<void> {
		await storeThere();
		await bunStore().delete({ service: SERVICE, name });
	},
};

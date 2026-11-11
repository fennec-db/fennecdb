#!/usr/bin/env bun
// SPIKE (desktop-spike.yml): Bun.secrets, which secrets.ts keeps every token
// in, on the host this runs on: the Keychain on macOS, the Credential Manager
// on Windows, libsecret (a Secret Service on D-Bus) on Linux. Run with the Bun
// the app bundles. Prints one JSON line and exits 0 whatever it finds.
const service = 'dev.fennecdb.studio.spike';
const name = `probe:${crypto.randomUUID()}`;
const value = `${'x'.repeat(40)}-${Date.now()}`;
const step = async (what, f) => {
  try {
    return { what, ok: true, value: await f() };
  } catch (e) {
    return { what, ok: false, error: String(e?.message ?? e), code: e?.code ?? null };
  }
};
const steps = [];
steps.push(await step('set', () => Bun.secrets.set({ service, name, value })));
steps.push(await step('get', async () => ((await Bun.secrets.get({ service, name })) === value ? 'same value' : 'DIFFERENT')));
steps.push(await step('delete', () => Bun.secrets.delete({ service, name })));
steps.push(await step('get-after-delete', () => Bun.secrets.get({ service, name })));
console.log(JSON.stringify({ bun: Bun.version, platform: `${process.platform}-${process.arch}`, dbus: process.env.DBUS_SESSION_BUS_ADDRESS ? 'session bus' : 'no session bus', steps }));

// What a suite needs from a build (web/fennec.wasm, the binaries cargo
// builds) and does not have is a skip on a developer's machine, where
// nothing was built, and a failure under CI: a runner that skipped them
// reported a green job that had tested nothing (tools/skips_check.py holds
// the count of what is still skipped). Not shipped: package.json's `files`
// leaves it out.

/**
 * Throws under CI when a prerequisite is missing; returns otherwise, the
 * suite's own `skip` then standing in for a machine with no build.
 * Each requirement is `[what, present, how to make it]`.
 */
export function needs(...requirements) {
  if (!process.env.CI) return;
  const missing = requirements.filter(([, present]) => !present).map(([what, , how]) => `${what} (${how})`);
  if (missing.length) {
    throw new Error(`CI needs ${missing.join(', ')}: a suite that skips itself there tests nothing`);
  }
}

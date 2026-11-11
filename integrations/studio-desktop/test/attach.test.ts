import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import { WINDOWS } from "./platform.ts";

// scripts/attach.sh, which desktop.yml's attach-unsigned runs with
// contents: write on a published release, against a stand-in `gh` that keeps
// each release in a folder ($FAKE_RELEASES/<tag>/): no release is written to,
// and the script is the one the job runs. What it holds: the downloads are
// attached under the release's version and each one's line is appended to
// SHA256SUMS, every byte before kept; a re-run changes nothing; a file of an
// asset's name that is not this one, or a line with another sum, fails the
// run before anything is written; SHA256SUMS must read back as written.
// Windows skips it: the job runs on Linux, and Git's bash would run the
// stand-in `gh` only by its `#!`.

const script = fileURLToPath(new URL("../scripts/attach.sh", import.meta.url));
const work = mkdtempSync(join(tmpdir(), "fennec-attach-"));
after(() => rmSync(work, { recursive: true, force: true }));
const skip = WINDOWS && "the attach job runs on Linux; on Windows Git's bash would run the stand-in gh by its #! alone";

const sha = (bytes: string | Buffer) => createHash("sha256").update(bytes).digest("hex");

// gh release view|download|upload, against a folder per release; every call
// logged, an upload that would replace an asset refused unless --clobber, as
// gh refuses it. FAKE_GH_SKEW makes an uploaded SHA256SUMS differ from what
// was sent, as a release changed under the job would.
const bin = join(work, "bin");
mkdirSync(bin);
writeFileSync(
	join(bin, "gh"),
	`#!/bin/sh
[ "$1" = release ] || exit 2
sub=$2
tag=$3
shift 3
dir=$FAKE_RELEASES/$tag
echo "$sub $tag $*" >>"$FAKE_RELEASES/gh.log"
[ -d "$dir" ] || { echo "release not found" >&2; exit 1; }
case $sub in
    view) ls -A "$dir" ;;
    download)
        pat= out=.
        while [ $# -gt 0 ]; do
            case $1 in -p) pat=$2; shift 2 ;; -D) out=$2; shift 2 ;; -R) shift 2 ;; *) shift ;; esac
        done
        [ -f "$dir/$pat" ] || { echo "no asset $pat" >&2; exit 1; }
        [ ! -e "$out/$pat" ] || { echo "$out/$pat exists" >&2; exit 1; }
        mkdir -p "$out" && cp "$dir/$pat" "$out/$pat"
        ;;
    upload)
        clobber= files=
        while [ $# -gt 0 ]; do
            case $1 in --clobber) clobber=1; shift ;; -R) shift 2 ;; *) files="$files $1"; shift ;; esac
        done
        for f in $files; do
            n=$(basename "$f")
            if [ -e "$dir/$n" ] && [ -z "$clobber" ]; then echo "asset $n already exists" >&2; exit 1; fi
            cp "$f" "$dir/$n"
            if [ -n "\${FAKE_GH_SKEW:-}" ] && [ "$n" = SHA256SUMS ]; then printf 'x' >>"$dir/$n"; fi
        done
        ;;
esac
`,
);
chmodSync(join(bin, "gh"), 0o755);

const DOWNLOADS = ["fennec-studio-linux-arm64.tar.gz", "fennec-studio-linux-x64.tar.gz", "fennec-studio-macos-arm64.dmg", "fennec-studio-windows-arm64.zip", "fennec-studio-windows-x64.zip"];
const OLD_SUMS = `${"1".repeat(64)}  fennec-v0.9.0-x86_64-unknown-linux-musl.tar.gz\n${"2".repeat(64)}  fennec-v0.9.0-x86_64-pc-windows-msvc.zip\n`;

let n = 0;
/** A release v0.9.0 holding SHA256SUMS (`sums`) and `assets`, and the build jobs' downloads, each its own bytes. */
function setup(sums: string | null = OLD_SUMS, assets: Record<string, string> = {}) {
	const at = join(work, `case-${++n}`);
	const releases = join(at, "releases");
	const rel = join(releases, "v0.9.0");
	const dist = join(at, "dist");
	mkdirSync(rel, { recursive: true });
	mkdirSync(dist);
	if (sums !== null) writeFileSync(join(rel, "SHA256SUMS"), sums);
	for (const [name, body] of Object.entries(assets)) writeFileSync(join(rel, name), body);
	for (const d of DOWNLOADS) writeFileSync(join(dist, d), `the ${d} of case ${n}`);
	return { releases, rel, dist, files: DOWNLOADS.map((d) => join(dist, d)) };
}

function attach(releases: string, args: string[], env: Record<string, string> = {}) {
	return spawnSync("sh", [script, ...args], {
		encoding: "utf8",
		env: { ...process.env, ...env, PATH: `${bin}:${process.env["PATH"]}`, FAKE_RELEASES: releases, GITHUB_REPOSITORY: "fennec-db/fennecdb", GITHUB_STEP_SUMMARY: "" },
	});
}
const log = (releases: string) => (existsSync(join(releases, "gh.log")) ? readFileSync(join(releases, "gh.log"), "utf8") : "");
const versioned = (d: string) => d.replace("fennec-studio-", "fennec-studio-v0.9.0-");

test("each download is attached under the release's version, its line appended and every byte before kept", { skip }, () => {
	const { releases, rel, files } = setup();
	const r = attach(releases, ["v0.9.0", ...files]);
	assert.equal(r.status, 0, r.stdout + r.stderr);
	const sums = readFileSync(join(rel, "SHA256SUMS"), "utf8");
	assert.ok(sums.startsWith(OLD_SUMS), "the old lines, byte for byte, first");
	const added = sums.slice(OLD_SUMS.length).split("\n").filter(Boolean);
	assert.deepEqual(
		added,
		files.map((f, i) => `${sha(readFileSync(f))}  ${versioned(DOWNLOADS[i]!)}`),
		"one line a download, in the order given",
	);
	for (const d of DOWNLOADS) assert.equal(readFileSync(join(rel, versioned(d)), "utf8"), readFileSync(files[DOWNLOADS.indexOf(d)]!, "utf8"), `${versioned(d)} is the download`);
	assert.equal(log(releases).split("\n").filter((l) => /^upload .*SHA256SUMS --clobber/.test(l)).length, 1, "SHA256SUMS written once for all of them");
});

test("a re-run attaches nothing and writes nothing", { skip }, () => {
	const { releases, rel, files } = setup();
	assert.equal(attach(releases, ["0.9.0", ...files]).status, 0);
	const sums = readFileSync(join(rel, "SHA256SUMS"), "utf8");
	writeFileSync(join(releases, "gh.log"), "");
	const r = attach(releases, ["0.9.0", ...files]);
	assert.equal(r.status, 0, r.stdout + r.stderr);
	assert.match(r.stdout, /lists every one already/);
	assert.doesNotMatch(log(releases), /^upload/m, "no upload");
	assert.equal(readFileSync(join(rel, "SHA256SUMS"), "utf8"), sums);
});

test("a run cut off after the uploads lists them the next time", { skip }, () => {
	const { releases, rel, files } = setup();
	assert.equal(attach(releases, ["v0.9.0", ...files]).status, 0);
	writeFileSync(join(rel, "SHA256SUMS"), OLD_SUMS);
	writeFileSync(join(releases, "gh.log"), "");
	const r = attach(releases, ["v0.9.0", ...files]);
	assert.equal(r.status, 0, r.stdout + r.stderr);
	assert.equal(readFileSync(join(rel, "SHA256SUMS"), "utf8").split("\n").filter(Boolean).length, 2 + DOWNLOADS.length);
	assert.doesNotMatch(log(releases), /^upload .*fennec-studio/m, "the downloads not uploaded again");
});

test("an asset of a download's name that is another file fails the run before anything is written", { skip }, () => {
	const taken = versioned("fennec-studio-windows-x64.zip");
	const { releases, rel, files } = setup(OLD_SUMS, { [taken]: "another build's zip" });
	const r = attach(releases, ["v0.9.0", ...files]);
	assert.notEqual(r.status, 0);
	assert.match(r.stderr, /has a fennec-studio-v0\.9\.0-windows-x64\.zip already, not this one/);
	assert.doesNotMatch(log(releases), /^upload/m, "nothing uploaded, the downloads before it in the list included");
	assert.equal(readFileSync(join(rel, "SHA256SUMS"), "utf8"), OLD_SUMS);
	assert.equal(readFileSync(join(rel, taken), "utf8"), "another build's zip", "the asset left as it was");
});

test("a line for a download with another sum fails the run before anything is written", { skip }, () => {
	const sums = `${OLD_SUMS}${"3".repeat(64)}  ${versioned("fennec-studio-linux-x64.tar.gz")}\n`;
	const { releases, rel, files } = setup(sums);
	const r = attach(releases, ["v0.9.0", ...files]);
	assert.notEqual(r.status, 0);
	assert.match(r.stderr, /SHA256SUMS lists fennec-studio-v0\.9\.0-linux-x64\.tar\.gz with 3{64}/);
	assert.doesNotMatch(log(releases), /^upload/m);
	assert.equal(readFileSync(join(rel, "SHA256SUMS"), "utf8"), sums);
});

test("a SHA256SUMS with no newline at its end gets one before the new lines", { skip }, () => {
	const { releases, rel, files } = setup(OLD_SUMS.trimEnd());
	assert.equal(attach(releases, ["v0.9.0", files[0]!]).status, 0);
	assert.equal(readFileSync(join(rel, "SHA256SUMS"), "utf8"), `${OLD_SUMS}${sha(readFileSync(files[0]!))}  ${versioned(DOWNLOADS[0]!)}\n`);
});

test("a SHA256SUMS that does not read back as written fails the run", { skip }, () => {
	const { releases, files } = setup();
	const r = attach(releases, ["v0.9.0", files[1]!], { FAKE_GH_SKEW: "1" });
	assert.notEqual(r.status, 0);
	assert.match(r.stderr, /does not read back as written/);
});

test("what is not a release, a download or a file is refused, and nothing is uploaded", { skip }, () => {
	const { releases, dist, files } = setup();
	const other = join(dist, "fennec-studio-macos-x64.dmg");
	writeFileSync(other, "an Intel Mac's app, which nothing builds");
	for (const [args, said] of [
		[["0.9", files[0]!], /release is a tag such as v0\.2\.0/],
		[["v0.9.0;x", files[0]!], /release is a tag/],
		[["v0.9.0"], /nothing to attach/],
	] as const) {
		const r = attach(releases, [...args]);
		assert.notEqual(r.status, 0, args.join(" "));
		assert.match(r.stderr, said);
	}
	assert.equal(log(releases), "", "gh was never asked");
	for (const [args, said] of [
		[["v0.9.0", other], /fennec-studio-macos-x64\.dmg is not one of the app's downloads/],
		[["v0.9.0", files[0]!, files[0]!], /named twice/],
		[["v0.9.0", join(dist, "missing", "fennec-studio-linux-x64.tar.gz")], /no .*fennec-studio-linux-x64\.tar\.gz/],
	] as const) {
		const r = attach(releases, [...args]);
		assert.notEqual(r.status, 0, args.join(" "));
		assert.match(r.stderr, said);
	}
	assert.doesNotMatch(log(releases), /^upload/m, "nothing uploaded");
});

test("a release with no SHA256SUMS is refused", { skip }, () => {
	const { releases, rel, files } = setup(null);
	const r = attach(releases, ["v0.9.0", ...files]);
	assert.notEqual(r.status, 0);
	assert.match(r.stderr, /has no SHA256SUMS/);
	assert.deepEqual(readdirSync(rel), [], "nothing attached");
});

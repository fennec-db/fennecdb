#!/usr/bin/env python3
"""tools/release-verify.sh, RELEASING.md's step 6, against stand-ins for
`gh`, `npm` and `curl` that keep a release, its attestations, the
registry's digests and the packages' provenance in a folder, in the
pattern of test_release_latest.py: a release, a candidate and a draft that
verify, and each thing missing or wrong failing it, at the first, naming
it. Then its list of assets, held to what release.yml, swift-binary.yml and
desktop.yml attach and the scripts that name the files. Every case runs
the script under /bin/dash.

    python3 tools/test_release_verify.py
"""

import base64
import hashlib
import importlib.util
import json
import os
import pathlib
import re
import shutil
import subprocess
import tempfile
import textwrap
import unittest

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent
SCRIPT = HERE / "release-verify.sh"
DASH = "/bin/dash"
IMAGE = "ghcr.io/o/fennec-server"
SLSA = "https://slsa.dev/provenance/v1"
NPM = ("@fennecdb/web", "@fennecdb/react", "@fennecdb/cloudflare", "@fennecdb/langchain")

spec = importlib.util.spec_from_file_location("workflow_check", HERE / "workflow_check.py")
wc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wc)


def key(text):
    """A URL or a package's spec as a file name, as the stand-ins spell it."""
    return re.sub(r"[^A-Za-z0-9]", "_", text)


# gh as the owner's login meets it. `gh release view TAG -R o/r --json
# isDraft,isPrerelease,assets` answers $STATE/release.json; `gh release
# download TAG -R o/r -D DIR` copies $STATE/files/*; `gh api
# repos/o/r/releases/latest --jq .tag_name` answers $STATE/latest, a 404
# where there is none. `gh attestation verify SUBJECT -R o/r
# --cert-identity https://github.com/o/r/.github/workflows/W@REF --source-ref REF
# [--deny-self-hosted-runners] [--format json --jq Q]` finds the subject's
# digest -- a file's SHA-256, or for oci://IMAGE:REF what $STATE/gh-sees/REF
# names, else $STATE/registry/REF -- among $STATE/attested's lines
# ("digest workflow ref runner"), and fails as gh does where no line has
# that workflow and ref and, with --deny-self-hosted-runners, a GitHub-hosted
# runner. Anything else is not stood in for.
GH = r"""#!/bin/sh
set -eu
case "$1 $2" in
"release view")
    [ "$4 $5 $6 $7" = "-R o/r --json isDraft,isPrerelease,assets" ] || { echo "gh $*: not stood in for" >&2; exit 1; }
    [ -f "$STATE/release.json" ] || { echo "release not found" >&2; exit 1; }
    cat "$STATE/release.json"
    ;;
"release download")
    [ "$4 $5 $6" = "-R o/r -D" ] && [ $# -eq 7 ] || { echo "gh $*: not stood in for" >&2; exit 1; }
    cp "$STATE/files/"* "$7/"
    ;;
"api repos/o/r/releases/latest")
    [ "$3 $4" = "--jq .tag_name" ] || { echo "gh $*: not stood in for" >&2; exit 1; }
    t=$(cat "$STATE/latest" 2>/dev/null || true)
    if [ -z "$t" ]; then
        echo '{"message":"Not Found","status":"404"}'
        echo 'gh: Not Found (HTTP 404)' >&2
        exit 1
    fi
    echo "$t"
    ;;
"attestation verify")
    subject=$3
    shift 3
    repo= id= ref= deny= format= q=
    while [ $# -gt 0 ]; do
        case $1 in
        -R) repo=$2; shift 2 ;;
        --cert-identity) id=$2; shift 2 ;;
        --source-ref) ref=$2; shift 2 ;;
        --deny-self-hosted-runners) deny=1; shift ;;
        --format) format=$2; shift 2 ;;
        --jq | -q) q=$2; shift 2 ;;
        *) echo "gh attestation verify $1: not stood in for" >&2; exit 1 ;;
        esac
    done
    [ "$repo" = o/r ] || { echo "gh attestation verify -R $repo: not the repository" >&2; exit 1; }
    # The identity as a whole, never a prefix: https://github.com/o/r/.github/workflows/W@REF
    case $id in
    https://github.com/o/r/.github/workflows/*@*) ;;
    *) echo "gh attestation verify --cert-identity $id: not stood in for" >&2; exit 1 ;;
    esac
    wf=${id#https://github.com/o/r/.github/workflows/}
    wf=${wf%@*}
    [ "${id##*@}" = "$ref" ] || { echo "gh attestation verify: the identity's ref is not $ref" >&2; exit 1; }
    case $wf in
    *[!A-Za-z0-9.-]*) echo "gh attestation verify: no such workflow $wf" >&2; exit 1 ;;
    esac
    case $subject in
    oci://*)
        r=${subject##*:}
        d=$(cat "$STATE/gh-sees/$r" 2>/dev/null || cat "$STATE/registry/$r")
        d=${d#sha256:}
        ;;
    *)
        if command -v sha256sum >/dev/null 2>&1; then d=$(sha256sum "$subject" | cut -d' ' -f1)
        else d=$(shasum -a 256 "$subject" | cut -d' ' -f1); fi
        ;;
    esac
    touch "$STATE/attested"
    if ! awk -v d="$d" '$1 == d { found = 1 } END { exit !found }' "$STATE/attested"; then
        echo "Error: HTTP 404: Not Found (https://api.github.com/repos/o/r/attestations/sha256:$d?per_page=30&predicate_type=https%3A%2F%2Fslsa.dev%2Fprovenance%2Fv1)" >&2
        exit 1
    fi
    if ! awk -v d="$d" -v w="${wf##*/}" -v r="$ref" -v deny="$deny" \
        '$1 == d && $2 == w && $3 == r && (deny == "" || $4 == "github-hosted") { found = 1 } END { exit !found }' "$STATE/attested"; then
        echo "Error: verifying with issuer \"sigstore.dev\": failed to verify certificate identity: no matching CertificateIdentity found, last error: expected SAN value to match \"$id\", or source ref $ref" >&2
        exit 1
    fi
    if [ "$format" = json ]; then
        printf '[{"verificationResult":{"statement":{"subject":[{"digest":{"sha256":"%s"}}]}}}]\n' "$d" | jq -r "$q"
    else
        echo "Loaded digest sha256:$d"
        echo "The following policy criteria will be enforced: ..."
        echo "✓ Verification succeeded!"
    fi
    ;;
*)
    echo "gh $*: not stood in for" >&2
    exit 1
    ;;
esac
"""

# npm: `npm view PKG@V dist.attestations --json` answers $STATE/npm/<key>,
# nothing where there is no such file (a field npm does not have), a 404
# where $STATE/npm/<key>.missing is; `npm init -y` and `npm install` write
# nothing of note; `npm audit signatures` fails where
# $STATE/npm-audit-fails is.
NPM_STANDIN = r"""#!/bin/sh
set -eu
echo "npm $*" >>"$STATE/npm-calls"
case $1 in
view)
    [ "$3 $4" = "dist.attestations --json" ] || { echo "npm $*: not stood in for" >&2; exit 1; }
    k=$(printf '%s' "$2" | tr -c 'A-Za-z0-9' '_')
    if [ -f "$STATE/npm/$k.missing" ]; then echo "npm error 404 Not Found - GET https://registry.npmjs.org/$2" >&2; exit 1; fi
    if [ -f "$STATE/npm/$k" ]; then cat "$STATE/npm/$k"; fi
    ;;
init) echo '{}' >package.json ;;
install) ;;
audit)
    [ "$2" = signatures ] || { echo "npm $*: not stood in for" >&2; exit 1; }
    if [ -f "$STATE/npm-audit-fails" ]; then
        echo "audited 4 packages"
        echo "1 package has an invalid attestation: @fennecdb/web"
        exit 1
    fi
    echo "audited 4 packages: 4 verified registry signatures, 4 verified attestations"
    ;;
*) echo "npm $*: not stood in for" >&2; exit 1 ;;
esac
"""

# curl: the URL (its last argument) answers $STATE/http/<key>, or with -I
# $STATE/http/<key>.head, and a 404 as `curl -f` fails where there is none.
CURL = r"""#!/bin/sh
set -eu
head= url=
while [ $# -gt 0 ]; do
    case $1 in
    -H) shift 2 ;;
    --*) shift ;;
    -*I*) head=.head; shift ;;
    -*) shift ;;
    *) url=$1; shift ;;
    esac
done
k=$(printf '%s' "$url" | tr -c 'A-Za-z0-9' '_')
if [ ! -f "$STATE/http/$k$head" ]; then
    echo "curl: (22) The requested URL returned error: 404" >&2
    exit 22
fi
cat "$STATE/http/$k$head"
"""


def digest(n):
    return "sha256:" + format(n, "064x")


def swift_ref(version):
    """The one ref swift-binary.yml's run may be on: main for an X.Y.0 and
    its candidates, release/X.Y for a patch and its."""
    major, minor, patch = version.split("-")[0].split(".")
    return "refs/heads/main" if int(patch) == 0 else f"refs/heads/release/{major}.{minor}"


def own_part(sums):
    """SHA256SUMS's lines before the app's, the file release.yml attests."""
    lines = sums.splitlines(True)
    n = next((i for i, line in enumerate(lines) if line.split()[1].lstrip("*").startswith("fennec-studio-")),
             len(lines))
    return "".join(lines[:n])


class ReleaseVerify(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not os.access(DASH, os.X_OK):
            raise AssertionError(f"{DASH}, the runners' sh, is what the script is held to, and is not here")

    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.dir.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name, text in (("gh", GH), ("npm", NPM_STANDIN), ("curl", CURL)):
            (self.bin / name).write_text(text)
            (self.bin / name).chmod(0o755)
        self.state = self.root / "state"

    def tearDown(self):
        self.dir.cleanup()

    def run_it(self, *args, **env):
        e = {k: v for k, v in os.environ.items() if k not in ("GH_TOKEN", "REPO", "IMAGE")}
        e.update(PATH=f"{self.bin}{os.pathsep}{os.environ['PATH']}", STATE=str(self.state), REPO="o/r",
                 IMAGE=IMAGE, TMPDIR=self.dir.name)
        e.update(env)
        return subprocess.run([DASH, str(SCRIPT), *args], env=e, capture_output=True, text=True, timeout=120)

    def listed(self, tag):
        got = self.run_it("--assets", tag)
        self.assertEqual(got.returncode, 0, got.stderr)
        return [tuple(line.split(" ", 1)) for line in got.stdout.splitlines()]

    def http(self, url, body, head=False):
        (self.state / "http").mkdir(exist_ok=True)
        (self.state / "http" / (key(url) + (".head" if head else ""))).write_text(body)

    def attest(self, data, workflow, ref, runner="github-hosted"):
        d = hashlib.sha256(data).hexdigest() if isinstance(data, bytes) else data.removeprefix("sha256:")
        with open(self.state / "attested", "a") as f:
            f.write(f"{d} {workflow} {ref} {runner}\n")

    def given(self, tag, draft=False, prerelease=None, latest="v0.2.0", skip=()):
        """A release that verifies: every asset there, attested by its
        workflow at its ref, SHA256SUMS's lines before the app's attested by
        release.yml at the tag, the image's three tags on the registry and
        attested, npm's four packages and PyPI's files attested by
        packages.yml. `skip` leaves out what a case breaks."""
        if self.state.exists():
            shutil.rmtree(self.state)
        self.state.mkdir()
        version = tag[1:]
        candidate = "-" in version
        app = not draft and not candidate
        assets = [(kind, name) for kind, name in self.listed(tag) if app or kind != "desktop"]
        files = self.state / "files"
        files.mkdir()
        sums = []
        for kind, name in assets:
            if name == "SHA256SUMS":
                continue
            data = f"{name} as {kind} built it\n".encode()
            if name.endswith(".checksum"):
                data = (hashlib.sha256((files / name.removesuffix(".checksum")).read_bytes()).hexdigest() + "\n").encode()
            (files / name).write_bytes(data)
            if not name.endswith(".checksum"):
                sums.append(f"{hashlib.sha256(data).hexdigest()}  {name}\n")
            if ("attest", name) in skip:
                continue
            ref = {"release": f"refs/tags/{tag}", "swift": swift_ref(version), "desktop": "refs/heads/main"}[kind]
            self.attest(data, {"release": "release.yml", "swift": "swift-binary.yml",
                               "desktop": "desktop.yml"}[kind], ref)
        self.tag = tag
        self.sums("".join(sums), attest=("attest", "SHA256SUMS") not in skip)
        self.release = {"isDraft": draft, "isPrerelease": candidate if prerelease is None else prerelease,
                        "assets": [{"name": n} for _, n in assets]}
        self.write_release()
        if latest:
            (self.state / "latest").write_text(latest + "\n")
        # The image: the registry's digest for each tag, and attested.
        (self.state / "registry").mkdir()
        self.http("https://ghcr.io/token?scope=repository:o/fennec-server:pull", '{"token":"t"}')
        for n, ref in enumerate((version, f"{version}-amd64", f"{version}-arm64"), 1):
            (self.state / "registry" / ref).write_text(digest(n))
            self.http(f"https://ghcr.io/v2/o/fennec-server/manifests/{ref}",
                      f"HTTP/2 200\r\ncontent-type: x\r\ndocker-content-digest: {digest(n)}\r\n\r\n", head=True)
            if ("image", ref) not in skip:
                self.attest(digest(n), "release.yml", f"refs/tags/{tag}")
        # npm and PyPI.
        (self.state / "npm").mkdir()
        for pkg in NPM:
            self.npm(pkg, version)
        pyversion = re.sub(r"-rc\.(\d+)$", r"rc\1", version)
        names = [f"fennecdb-{pyversion}-py3-none-any.whl", f"fennecdb-{pyversion}.tar.gz"]
        self.http(f"https://pypi.org/pypi/fennecdb/{pyversion}/json",
                  json.dumps({"urls": [{"filename": f} for f in names]}))
        for f in names:
            self.pypi(pyversion, f)

    def write_release(self):
        (self.state / "release.json").write_text(json.dumps(self.release))

    def sums(self, text, attest=True):
        """SHA256SUMS as `text`, its lines before the app's attested by
        release.yml at the tag where `attest`: what the tag's own run wrote,
        right or wrong."""
        (self.state / "files" / "SHA256SUMS").write_text(text)
        if attest:
            self.attest(own_part(text).encode(), "release.yml", f"refs/tags/{self.tag}")

    def not_checked(self, got):
        """The checks a failure names as not made, in order."""
        return [line.removeprefix("not checked: ") for line in got.stderr.splitlines()
                if line.startswith("not checked: ")]

    def npm(self, pkg, version, workflow=".github/workflows/packages.yml", repository="https://github.com/o/r",
            provenance=True):
        spec_ = f"{pkg}@{version}"
        url = f"https://registry.npmjs.org/-/npm/v1/attestations/{pkg.replace('/', '%2f')}@{version}"
        if provenance:
            (self.state / "npm" / key(spec_)).write_text(json.dumps(
                {"url": url, "provenance": {"predicateType": SLSA}}))
        statement = {"predicate": {"buildDefinition": {"externalParameters": {"workflow": {
            "ref": "refs/tags/v" + version, "repository": repository, "path": workflow}}}}}
        payload = base64.b64encode(json.dumps(statement).encode()).decode()
        self.http(url, json.dumps({"attestations": [
            {"predicateType": "https://github.com/npm/attestation/tree/main/specs/publish/v0.1", "bundle": {}},
            {"predicateType": SLSA, "bundle": {"dsseEnvelope": {"payload": payload}}}]}))

    def pypi(self, pyversion, f, workflow="packages.yml", repository="o/r"):
        self.http(f"https://pypi.org/integrity/fennecdb/{pyversion}/{f}/provenance", json.dumps(
            {"version": 1, "attestation_bundles": [{"publisher": {
                "kind": "GitHub", "repository": repository, "workflow": workflow, "environment": "pypi"},
                "attestations": [{}]}]}))

    def assertVerifies(self, got, assets):
        self.assertEqual(got.returncode, 0, got.stdout + got.stderr)
        self.assertNotIn("FAIL", got.stdout + got.stderr)
        self.assertIn(f"holds its {assets} assets, no more", got.stdout)
        return got.stdout

    def assertFails(self, got, *why):
        self.assertEqual(got.returncode, 1, got.stdout + got.stderr)
        fail = [line for line in got.stderr.splitlines() if line.startswith("FAIL ")]
        self.assertEqual(len(fail), 1, got.stderr)
        for w in why:
            self.assertIn(w, fail[0])
        return fail[0]

    # What verifies.

    def test_a_release_verifies(self):
        self.given("v1.0.0")
        out = self.assertVerifies(self.run_it("v1.0.0"), 23)
        for line in ("ok   fennec-v1.0.0-x86_64-unknown-linux-musl.tar.gz: release.yml at refs/tags/v1.0.0",
                     "ok   FennecFFI.xcframework.zip: swift-binary.yml at refs/heads/main",
                     "ok   fennec-studio-v1.0.0-macos-arm64.dmg: desktop.yml at refs/heads/main",
                     f"ok   {IMAGE}:1.0.0 {digest(1)}: release.yml at refs/tags/v1.0.0",
                     f"ok   {IMAGE}:1.0.0-arm64 {digest(3)}: release.yml at refs/tags/v1.0.0",
                     "ok   SHA256SUMS: release.yml at refs/tags/v1.0.0, its 16 lines before the app's",
                     "ok   SHA256SUMS lists 21 assets, each by its sum",
                     "ok   npm @fennecdb/langchain@1.0.0: SLSA provenance, packages.yml at refs/tags/v1.0.0",
                     "ok   npm audit signatures",
                     "ok   PyPI fennecdb-1.0.0.tar.gz: attested, packages.yml of o/r",
                     "ok   v1.0.0 verified"):
            self.assertIn(line, out)
        # One line an asset, SHA256SUMS's its attestation's.
        self.assertEqual(sum(1 for line in out.splitlines() if re.match(r"ok   \S+: \S+\.yml at ", line)), 23)

    def test_a_candidate_has_no_app_and_verifies(self):
        self.given("v1.0.0-rc.1")
        out = self.assertVerifies(self.run_it("v1.0.0-rc.1"), 18)
        self.assertIn("a candidate has no app files", out)
        self.assertIn("ok   PyPI fennecdb-1.0.0rc1-py3-none-any.whl", out)
        self.assertNotIn("fennec-studio", out)

    def test_a_draft_verifies_as_far_as_a_draft_goes(self):
        self.given("v1.0.0", draft=True, latest="")
        out = self.assertVerifies(self.run_it("v1.0.0"), 18)
        self.assertIn("verified as far as a draft goes", out)
        self.assertFalse((self.state / "npm-calls").exists())

    def test_xcframework_at_the_one_ref_its_version_takes(self):
        # main for an X.Y.0 and its candidates, release/X.Y for a patch and
        # its, never the other: a writer may push `release/1.0` with an
        # edited swift-binary.yml, whose run attests whatever it is given.
        cases = (
            ("v1.0.0", "refs/heads/main", True), ("v1.0.0", "refs/heads/release/1.0", False),
            ("v1.4.0", "refs/heads/release/1.4", False),
            ("v1.4.2", "refs/heads/release/1.4", True), ("v1.4.2", "refs/heads/main", False),
            ("v1.4.2", "refs/heads/release/1.3", False),
            ("v1.0.0-rc.1", "refs/heads/main", True), ("v1.0.0-rc.1", "refs/heads/release/1.0", False),
            ("v1.0.1-rc.2", "refs/heads/release/1.0", True), ("v1.0.1-rc.2", "refs/heads/main", False),
        )
        for tag, ref, passes in cases:
            with self.subTest(tag=tag, ref=ref):
                self.given(tag)
                lines = (self.state / "attested").read_text().replace(
                    f"swift-binary.yml {swift_ref(tag[1:])}", f"swift-binary.yml {ref}")
                (self.state / "attested").write_text(lines)
                got = self.run_it(tag)
                if passes:
                    out = self.assertVerifies(got, 18 if "-" in tag else 23)
                    self.assertIn(f"ok   FennecFFIDynamic.xcframework.zip: swift-binary.yml at {ref}", out)
                else:
                    self.assertFails(got, "FennecFFI.xcframework.zip: no attestation by swift-binary.yml at"
                                     f" {swift_ref(tag[1:])}:")

    # An asset missing or wrong.

    def test_a_missing_attestation(self):
        self.given("v1.0.0", skip=[("attest", "fennecdb_1.0.0-1_arm64.deb")])
        self.assertFails(self.run_it("v1.0.0"), "fennecdb_1.0.0-1_arm64.deb: no attestation by release.yml at"
                         " refs/tags/v1.0.0", "HTTP 404")

    def test_a_wrong_signer(self):
        # A server tarball vouched for by desktop.yml's `attach`, or by a
        # workflow of a branch: each run of the repository can attest.
        for workflow, ref in (("desktop.yml", "refs/heads/main"), ("release.yml", "refs/heads/main"),
                              ("release.yml", "refs/heads/evil"), ("release.yml", "refs/tags/v0.9.9")):
            with self.subTest(workflow=workflow, ref=ref):
                name = "fennec-v1.0.0-x86_64-unknown-linux-musl.tar.gz"
                self.given("v1.0.0", skip=[("attest", name)])
                self.attest((self.state / "files" / name).read_bytes(), workflow, ref)
                self.assertFails(self.run_it("v1.0.0"), f"{name}: no attestation by release.yml at refs/tags/v1.0.0",
                                 "no matching CertificateIdentity")
        # The app attested by release.yml, and the XCFramework by a branch.
        name = "fennec-studio-v1.0.0-linux-x64.tar.xz"
        self.given("v1.0.0", skip=[("attest", name)])
        self.attest((self.state / "files" / name).read_bytes(), "release.yml", "refs/tags/v1.0.0")
        self.assertFails(self.run_it("v1.0.0"), f"{name}: no attestation by desktop.yml at refs/heads/main")
        self.given("v1.0.0", skip=[("attest", "FennecFFI.xcframework.zip")])
        self.attest((self.state / "files" / "FennecFFI.xcframework.zip").read_bytes(), "swift-binary.yml",
                    "refs/heads/topic")
        self.assertFails(self.run_it("v1.0.0"), "FennecFFI.xcframework.zip: no attestation by swift-binary.yml at"
                         " refs/heads/main:")

    def test_a_checksum_naming_another_digest(self):
        name = "FennecFFI.xcframework.zip.checksum"
        self.given("v1.0.0")
        (self.state / "files" / name).write_text("0" * 64 + "\n")
        self.assertFails(self.run_it("v1.0.0"), f"{name} holds [{'0' * 64}], and FennecFFI.xcframework.zip is")

    def test_a_signer_workflow_of_a_longer_name(self):
        # `release.yml-x.yml` at the right ref satisfied a prefix match.
        name = "fennec-v1.0.0-x86_64-unknown-linux-musl.tar.gz"
        self.given("v1.0.0", skip=[("attest", name)])
        self.attest((self.state / "files" / name).read_bytes(), "release.yml-x.yml", "refs/tags/v1.0.0")
        self.assertFails(self.run_it("v1.0.0"), f"{name}: no attestation by release.yml at refs/tags/v1.0.0")

    def test_a_self_hosted_runners_attestation(self):
        name = "fennec-web-v1.0.0.tar.gz"
        self.given("v1.0.0", skip=[("attest", name)])
        self.attest((self.state / "files" / name).read_bytes(), "release.yml", "refs/tags/v1.0.0", "self-hosted")
        self.assertFails(self.run_it("v1.0.0"), f"{name}: no attestation by release.yml")

    def test_an_attestation_of_other_bytes(self):
        # A rebuild attested; the release holds the first build's bytes.
        name = "fennecdb-android-1.0.0.aar"
        self.given("v1.0.0", skip=[("attest", name)])
        self.attest(b"a rebuild", "release.yml", "refs/tags/v1.0.0")
        self.assertFails(self.run_it("v1.0.0"), f"{name}: no attestation by release.yml")

    def test_a_missing_asset(self):
        for name in ("fennec-v1.0.0-aarch64-pc-windows-msvc.zip", "FennecFFI.xcframework.zip.checksum",
                     "fennec-studio-v1.0.0-windows-arm64.zip", "SHA256SUMS"):
            with self.subTest(name=name):
                self.given("v1.0.0")
                self.release["assets"].remove({"name": name})
                self.write_release()
                self.assertFails(self.run_it("v1.0.0"), f"v1.0.0 lacks {name}")

    def test_an_extra_asset(self):
        self.given("v1.0.0")
        self.release["assets"].append({"name": "fennec-v1.0.0-riscv64-unknown-linux-musl.tar.gz"})
        self.write_release()
        self.assertFails(self.run_it("v1.0.0"), "holds fennec-v1.0.0-riscv64-unknown-linux-musl.tar.gz, which no"
                         " workflow attaches")
        # The app's downloads on a candidate, which desktop.yml never makes.
        self.given("v1.0.0-rc.1")
        self.release["assets"].append({"name": "fennec-studio-v1.0.0-rc.1-macos-arm64.dmg"})
        self.write_release()
        self.assertFails(self.run_it("v1.0.0-rc.1"), "holds fennec-studio-v1.0.0-rc.1-macos-arm64.dmg")

    def test_a_candidate_that_is_no_pre_release_and_the_reverse(self):
        self.given("v1.0.0-rc.1", prerelease=False)
        self.assertFails(self.run_it("v1.0.0-rc.1"), "v1.0.0-rc.1 is a candidate and its release is no"
                         " pre-release")
        self.given("v1.0.0", prerelease=True)
        self.assertFails(self.run_it("v1.0.0"), "v1.0.0 is a release and is marked a pre-release")

    def test_latest_on_a_candidate(self):
        self.given("v1.0.0-rc.1", latest="v1.0.0-rc.1")
        self.assertFails(self.run_it("v1.0.0-rc.1"), "GitHub's Latest is v1.0.0-rc.1, no X.Y.Z release")

    def test_sha256sums_held_to_the_assets(self):
        # Each written and attested by the tag's run, wrong all the same.
        name = "fennec-ffi-v1.0.0-x86_64-unknown-linux-gnu.tar.gz"
        sums = self.state / "files" / "SHA256SUMS"

        def own(change):
            """`change` to the lines before the app's alone."""
            def apply(t):
                head = own_part(t)
                return change(head) + t[len(head):]
            return apply

        cases = (
            ("a wrong sum", own(lambda t: re.sub(r"^\w+(  " + re.escape(name) + ")", "0" * 64 + r"\1", t,
                                                 flags=re.M)),
             f"SHA256SUMS lists {name} with {'0' * 64}"),
            ("a line missing", own(lambda t: "".join(l for l in t.splitlines(True) if not l.endswith(name + "\n"))),
             f"SHA256SUMS lists no {name}"),
            ("a line twice", own(lambda t: t + next(l for l in t.splitlines(True) if l.endswith(name + "\n"))),
             f"SHA256SUMS lists {name} more than once"),
            ("a line for no asset", own(lambda t: t + "0" * 64 + "  other.tar.gz\n"),
             "SHA256SUMS has 22 lines for 21"),
        )
        for what, change, why in cases:
            with self.subTest(what=what):
                self.given("v1.0.0")
                self.sums(change(sums.read_text()))
                self.assertFails(self.run_it("v1.0.0"), why)

    def test_sha256sums_attested_before_a_sum_is_read(self):
        # Unattested: a release made by a branch's dispatch, or SHA256SUMS
        # replaced by hand. Nothing after it is checked, and it says so.
        self.given("v1.0.0", skip=[("attest", "SHA256SUMS")])
        got = self.run_it("v1.0.0")
        self.assertFails(got, "SHA256SUMS: its 16 lines before the app's are no file release.yml attested at"
                         " refs/tags/v1.0.0, so no sum in it is read", "HTTP 404")
        self.assertEqual(self.not_checked(got), [
            "SHA256SUMS's sums", "each asset's attestation", "the image's three tags", "npm's provenance",
            "npm audit signatures", "PyPI's attestations"])
        # Attested by another workflow, or at another ref.
        for workflow, ref in (("desktop.yml", "refs/heads/main"), ("release.yml", "refs/heads/main")):
            with self.subTest(workflow=workflow, ref=ref):
                self.given("v1.0.0", skip=[("attest", "SHA256SUMS")])
                text = (self.state / "files" / "SHA256SUMS").read_text()
                self.attest(own_part(text).encode(), workflow, ref)
                self.assertFails(self.run_it("v1.0.0"), "SHA256SUMS: its 16 lines before the app's are no file"
                                 " release.yml attested", "no matching CertificateIdentity")

    def test_two_files_names_swapped(self):
        # Each file is attested by the tag's run, and an attestation names
        # bytes, not a name: swapped, with SHA256SUMS written to match, each
        # verifies alone. The tag's own SHA256SUMS is what refuses it.
        self.given("v1.0.0")
        a, b = (self.state / "files" / f"fennec-v1.0.0-{t}-unknown-linux-musl.tar.gz" for t in ("x86_64", "aarch64"))
        da, db = a.read_bytes(), b.read_bytes()
        a.write_bytes(db)
        b.write_bytes(da)
        sums = self.state / "files" / "SHA256SUMS"
        ha, hb = hashlib.sha256(da).hexdigest(), hashlib.sha256(db).hexdigest()
        self.sums(sums.read_text().replace(ha, "@").replace(hb, ha).replace("@", hb), attest=False)
        self.assertFails(self.run_it("v1.0.0"), "SHA256SUMS: its 16 lines before the app's are no file release.yml"
                         " attested at refs/tags/v1.0.0")

    def test_the_apps_lines_are_held_by_their_files(self):
        # Appended by desktop.yml after the publish and attested by no run:
        # each held to its file, which its own attestation holds.
        name = "fennec-studio-v1.0.0-linux-arm64.tar.xz"
        self.given("v1.0.0")
        sums = self.state / "files" / "SHA256SUMS"
        self.sums(re.sub(r"^\w+(  " + re.escape(name) + ")", "1" * 64 + r"\1", sums.read_text(), flags=re.M),
                  attest=False)
        self.assertFails(self.run_it("v1.0.0"), f"SHA256SUMS lists {name} with {'1' * 64}")
        # One of release.yml's lines after the app's: no run wrote it there.
        self.given("v1.0.0")
        lines = sums.read_text().splitlines(True)
        moved = next(l for l in lines if "fennec-web-v1.0.0.tar.gz" in l)
        self.sums("".join(l for l in lines if l != moved) + moved, attest=False)
        self.assertFails(self.run_it("v1.0.0"), "SHA256SUMS lists fennec-web-v1.0.0.tar.gz after the app's lines")

    def test_a_failure_names_every_check_it_did_not_make(self):
        # Stopped in the attestations: the assets after the one that failed,
        # then every check after.
        self.given("v1.0.0", skip=[("attest", "fennecdb-1.0.0-1.aarch64.rpm")])
        got = self.run_it("v1.0.0")
        self.assertFails(got, "fennecdb-1.0.0-1.aarch64.rpm: no attestation by release.yml")
        self.assertEqual(self.not_checked(got), [
            "the attestations of FennecFFI.xcframework.zip, FennecFFI.xcframework.zip.checksum,"
            " FennecFFIDynamic.xcframework.zip, fennec-studio-v1.0.0-macos-arm64.dmg,"
            " fennec-studio-v1.0.0-windows-x64.zip, fennec-studio-v1.0.0-windows-arm64.zip,"
            " fennec-studio-v1.0.0-linux-x64.tar.xz, fennec-studio-v1.0.0-linux-arm64.tar.xz",
            "the image's three tags", "npm's provenance", "npm audit signatures", "PyPI's attestations"])
        self.assertTrue(got.stderr.rstrip().endswith("not checked: PyPI's attestations"))
        # The image's second tag; npm's second package; the first check.
        self.given("v1.0.0", skip=[("image", "1.0.0-amd64")])
        got = self.run_it("v1.0.0")
        self.assertEqual(self.not_checked(got)[:2], [f"{IMAGE}:1.0.0-arm64", "npm's provenance"])
        self.given("v1.0.0")
        (self.state / "npm" / key("@fennecdb/react@1.0.0")).unlink()
        got = self.run_it("v1.0.0")
        self.assertEqual(self.not_checked(got), [
            "npm's provenance of @fennecdb/cloudflare, @fennecdb/langchain", "npm audit signatures",
            "PyPI's attestations"])
        got = self.run_it("vfoo")
        self.assertEqual(len(self.not_checked(got)), 11)
        # The last check's last file leaves nothing.
        self.given("v1.0.0")
        (self.state / "http" / key("https://pypi.org/integrity/fennecdb/1.0.0/fennecdb-1.0.0.tar.gz/provenance")
         ).unlink()
        got = self.run_it("v1.0.0")
        self.assertFails(got, "PyPI's fennecdb-1.0.0.tar.gz has no attestation")
        self.assertEqual(self.not_checked(got), [])

    # The image.

    def test_the_image_unattested(self):
        for ref in ("1.0.0", "1.0.0-amd64", "1.0.0-arm64"):
            with self.subTest(ref=ref):
                self.given("v1.0.0", skip=[("image", ref)])
                self.assertFails(self.run_it("v1.0.0"), f"{IMAGE}:{ref} (sha256:", "no attestation by release.yml"
                                 " at refs/tags/v1.0.0")

    def test_the_image_not_on_the_registry(self):
        self.given("v1.0.0")
        (self.state / "http" / (key("https://ghcr.io/v2/o/fennec-server/manifests/1.0.0-amd64") + ".head")).unlink()
        self.assertFails(self.run_it("v1.0.0"), f"{IMAGE}:1.0.0-amd64 is not on the registry")

    def test_the_attested_digest_is_not_the_one_served(self):
        # gh found an attested list; the registry serves another for the tag.
        self.given("v1.0.0")
        (self.state / "gh-sees").mkdir()
        (self.state / "gh-sees" / "1.0.0").write_text(digest(9))
        self.attest(digest(9), "release.yml", "refs/tags/v1.0.0")
        self.assertFails(self.run_it("v1.0.0"), f"{IMAGE}:1.0.0: the attestation names [{digest(9)[7:]}], not"
                         f" {digest(1)}")

    # npm and PyPI.

    def test_an_npm_package_without_provenance(self):
        self.given("v1.0.0")
        (self.state / "npm" / key("@fennecdb/cloudflare@1.0.0")).unlink()
        self.assertFails(self.run_it("v1.0.0"), "npm @fennecdb/cloudflare@1.0.0 carries no SLSA provenance")

    def test_an_npm_package_not_published(self):
        self.given("v1.0.0")
        (self.state / "npm" / (key("@fennecdb/react@1.0.0") + ".missing")).write_text("")
        self.assertFails(self.run_it("v1.0.0"), "npm @fennecdb/react@1.0.0 could not be read", "404")

    def test_an_npm_package_from_another_workflow(self):
        for workflow, repository in ((".github/workflows/release.yml", "https://github.com/o/r"),
                                     (".github/workflows/packages.yml", "https://github.com/someone/r")):
            with self.subTest(workflow=workflow, repository=repository):
                self.given("v1.0.0")
                self.npm("@fennecdb/web", "1.0.0", workflow=workflow, repository=repository)
                self.assertFails(self.run_it("v1.0.0"), "npm @fennecdb/web@1.0.0: its provenance names"
                                 f" [{repository} {workflow} refs/tags/v1.0.0]")

    def test_npm_audit_signatures_failing(self):
        self.given("v1.0.0")
        (self.state / "npm-audit-fails").write_text("")
        self.assertFails(self.run_it("v1.0.0"), "npm audit signatures over the four packages",
                         "invalid attestation")

    def test_a_pypi_file_without_attestations(self):
        self.given("v1.0.0")
        (self.state / "http" / key("https://pypi.org/integrity/fennecdb/1.0.0/fennecdb-1.0.0.tar.gz/provenance")
         ).unlink()
        self.assertFails(self.run_it("v1.0.0"), "PyPI's fennecdb-1.0.0.tar.gz has no attestation")

    def test_a_pypi_file_attested_by_another_publisher(self):
        self.given("v1.0.0")
        self.pypi("1.0.0", "fennecdb-1.0.0-py3-none-any.whl", workflow="other.yml")
        self.assertFails(self.run_it("v1.0.0"), "PyPI's fennecdb-1.0.0-py3-none-any.whl: its attestations name no"
                         " publisher packages.yml of o/r")

    def test_no_pypi_release(self):
        self.given("v1.0.0")
        (self.state / "http" / key("https://pypi.org/pypi/fennecdb/1.0.0/json")).unlink()
        self.assertFails(self.run_it("v1.0.0"), "PyPI holds no files of fennecdb 1.0.0")

    def test_the_usage_and_a_tag_that_is_no_release(self):
        for args in ([], ["--assets"], ["a", "b"]):
            got = subprocess.run([DASH, str(SCRIPT), *args], capture_output=True, text=True, timeout=60)
            self.assertEqual(got.returncode, 2)
            self.assertIn("tools/release-verify.sh TAG", got.stderr)
        for tag in ("1.0.0", "v1.0", "vfoo", "v1.0.0;id", "v1.0.0-1", "v1.0.0-beta.1", "v1.0.0-rc.0"):
            with self.subTest(tag=tag):
                self.assertFails(self.run_it(tag), f"{tag} is no release's tag: vX.Y.Z, or vX.Y.Z-rc.N for a"
                                 " candidate, N from 1")


class Assets(unittest.TestCase):
    """The script's list, held to what the workflows attach: release.yml's
    builds' matrices and the names their steps and scripts give the files,
    the XCFramework's three as swift-binary.yml keeps and attests them,
    SHA256SUMS, and the app's five as desktop.yml's attach.sh names them.
    Every job handing `publish` a file (a `dist-*` artifact) is one whose
    names `built` derives: a new one fails here by its name, not as an
    extra asset at the release."""

    # release.yml's jobs whose files' names `built` derives.
    DERIVED = {"binaries", "windows", "linux-ffi", "wasm", "apple", "android", "linux-packages"}

    def uploaders(self, jobs):
        """The jobs uploading an artifact `publish` downloads (`dist-*`)."""
        return sorted(name for name, job in jobs.items()
                      if any(str(s.get("uses", "")).startswith("actions/upload-artifact")
                             and str((s.get("with") or {}).get("name", "")).startswith("dist-")
                             for s in wc.steps(job)))

    def underived(self, jobs):
        return [name for name in self.uploaders(jobs) if name not in self.DERIVED]

    def listed(self, tag):
        got = subprocess.run([DASH, str(SCRIPT), "--assets", tag], capture_output=True, text=True, timeout=60)
        self.assertEqual(got.returncode, 0, got.stderr)
        return {name: kind for kind, name in (line.split(" ", 1) for line in got.stdout.splitlines())}

    def workflow(self, name):
        return wc.read((ROOT / ".github" / "workflows" / name).read_text(encoding="utf-8"))

    def runs(self, job):
        return "\n".join(str(s["run"]) for s in wc.steps(job) if "run" in s)

    def matrix(self, job, key_):
        return [str(e[key_]) for e in job["strategy"]["matrix"]["include"]]

    def text(self, path):
        return (ROOT / path).read_text(encoding="utf-8")

    def built(self, version):
        """Every file release.yml's publish attaches, from the workflows and
        the scripts that name them, each with the workflow that attests it."""
        jobs = self.workflow("release.yml")["jobs"]
        download = [s for s in wc.steps(jobs["publish"]) if str(s.get("uses", "")).startswith("actions/download-artifact")]
        self.assertEqual([str(s["with"]["pattern"]) for s in download], ["dist-*"])
        underived = self.underived(jobs)
        self.assertEqual(underived, [], f"release.yml's {', '.join(underived)} upload a dist-* artifact, which"
                         " `publish` attaches, and no line of this test derives the files' names: derive them"
                         " here and add them to tools/release-verify.sh's list")
        self.assertEqual(set(self.uploaders(jobs)), self.DERIVED)
        want = {}
        binaries = self.runs(jobs["binaries"])
        self.assertIn('stage="fennec-v${version}-${t}"', binaries)
        self.assertIn('tar -czf "dist/${stage}.tar.gz"', binaries)
        for targets in self.matrix(jobs["binaries"], "targets"):
            for t in targets.split():
                want[f"fennec-v{version}-{t}.tar.gz"] = "release"
        self.assertIn('tools/windows-zip.sh "${TAG#v}" ${{ matrix.target }} dist', self.runs(jobs["windows"]))
        self.assertIn("stage=fennec-v$version-$target", self.text("tools/windows-zip.sh"))
        self.assertIn('"$stage.zip"', self.text("tools/windows-zip.sh"))
        for t in self.matrix(jobs["windows"], "target"):
            want[f"fennec-v{version}-{t}.zip"] = "release"
        self.assertIn('tools/linux-ffi.sh "${TAG#v}" ${{ matrix.target }} dist', self.runs(jobs["linux-ffi"]))
        self.assertIn("stage=fennec-ffi-v$version-$target", self.text("tools/linux-ffi.sh"))
        self.assertIn('"$dist/$stage.tar.gz"', self.text("tools/linux-ffi.sh"))
        for t in self.matrix(jobs["linux-ffi"], "target"):
            want[f"fennec-ffi-v{version}-{t}.tar.gz"] = "release"
        self.assertIn('tar -czf "dist/fennec-web-v${version}.tar.gz"', self.runs(jobs["wasm"]))
        want[f"fennec-web-v{version}.tar.gz"] = "release"
        self.assertIn('"$OUT/fennecdb-android-$version.aar"', self.text("integrations/kotlin/build-aar.sh"))
        want[f"fennecdb-android-{version}.aar"] = "release"
        packages = self.text("tools/linux-packages.sh")
        self.assertIn("deb) file=fennecdb_${version}-1_$arch.deb ;;", packages)
        self.assertIn("rpm) file=fennecdb-$version-1.$triple.rpm ;;", packages)
        triples = dict(re.findall(r"^\s*(amd64|arm64)\) triple=(\w+) ;;", packages, re.M))
        for arch in self.matrix(jobs["linux-packages"], "arch"):
            want[f"fennecdb_{version}-1_{arch}.deb"] = "release"
            want[f"fennecdb-{version}-1.{triples[arch]}.rpm"] = "release"
        # The XCFramework: what `apple` hands on is what swift-binary.yml
        # kept and attests; `publish` sets the three aside while it attests.
        apple = [s for s in wc.steps(jobs["apple"]) if str(s.get("uses", "")).startswith("actions/upload-artifact")]
        names = [pathlib.PurePath(p).name for p in str(apple[0]["with"]["path"]).split()]
        swift = self.workflow("swift-binary.yml")["jobs"]
        kept = [s for s in wc.steps(swift["build"]) if str(s.get("uses", "")).startswith("actions/upload-artifact")]
        self.assertEqual(names, [pathlib.PurePath(p).name for p in str(kept[0]["with"]["path"]).split()])
        self.assertTrue(any(str(s.get("uses", "")).startswith("actions/attest-build-provenance")
                            for s in wc.steps(swift["pull-request"])))
        publish = self.runs(jobs["publish"])
        self.assertIn("mv dist/" + " dist/".join(names) + " swift/", publish)
        for n in names:
            want[n] = "swift"
        self.assertIn("sha256sum *.tar.gz *.zip *.aar *.deb *.rpm > SHA256SUMS", publish)
        # SHA256SUMS attested once written, in the tag's own run alone.
        names = [str(s.get("name", "")) for s in wc.steps(jobs["publish"])]
        attest = next(s for s in wc.steps(jobs["publish"]) if str((s.get("with") or {}).get("subject-path", ""))
                      == "dist/SHA256SUMS")
        self.assertTrue(str(attest["uses"]).startswith("actions/attest-build-provenance@"))
        self.assertEqual(str(attest["if"]), "steps.own.outputs.attest == 'true'")
        self.assertGreater(names.index(str(attest["name"])), names.index("Checksums"))
        self.assertLess(names.index(str(attest["name"])), names.index("Publish"))
        want["SHA256SUMS"] = "sums"
        return want

    def test_a_new_job_handing_publish_a_file_fails_by_name(self):
        jobs = dict(self.workflow("release.yml")["jobs"])
        jobs.update(wc.read(textwrap.dedent("""
            riscv:
              runs-on: ubuntu-latest
              steps:
                - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7.0.2
                  with:
                    name: dist-riscv
                    path: dist/*
            notes:
              runs-on: ubuntu-latest
              steps:
                - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7.0.2
                  with:
                    name: notes
                    path: notes.md
        """)))
        self.assertEqual(self.underived(jobs), ["riscv"])

    def app(self, version):
        attach = self.text("integrations/studio-desktop/scripts/attach.sh")
        bases = re.search(r"^\s*case \$base in\n\s*(.+?)\) ;;", attach, re.M).group(1).split(" | ")
        self.assertIn("name=fennec-studio-v$version-${base#fennec-studio-}", attach)
        desktop = (ROOT / ".github" / "workflows" / "desktop.yml").read_text(encoding="utf-8")
        self.assertIn("sh integrations/studio-desktop/scripts/attach.sh", desktop)
        return {f"fennec-studio-v{version}-{b.removeprefix('fennec-studio-')}": "desktop" for b in bases}

    def test_a_release_is_what_the_workflows_attach(self):
        want = {**self.built("1.4.2"), **self.app("1.4.2")}
        self.assertEqual(self.listed("v1.4.2"), want)
        self.assertEqual(len(want), 23)

    def test_a_candidate_has_no_app(self):
        # desktop.yml and attach.sh take X.Y.Z alone.
        for path, form in ((".github/workflows/desktop.yml", "'^v?[0-9]+\\.[0-9]+\\.[0-9]+$'"),
                           ("integrations/studio-desktop/scripts/attach.sh", "'v?[0-9]+\\.[0-9]+\\.[0-9]+'")):
            self.assertIn(form, self.text(path))
        self.assertEqual(self.listed("v1.0.0-rc.1"), self.built("1.0.0-rc.1"))
        self.assertEqual(len(self.listed("v1.0.0-rc.1")), 18)


# The one form a release's tag takes: vX.Y.Z, or vX.Y.Z-rc.N for a
# candidate, N from 1. release.yml's jobs and swift-binary.yml's (the
# version, without its `v`) hold it in their first step, before any
# checkout, where no file of the tree can be read: so each copy of it is
# held here to this one, and run.
TAG_FORM = r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-rc\.[1-9][0-9]*)?$"
TAKEN = ("v0.2.0", "v1.0.0", "v1.4.2", "v1.0.0-rc.1", "v1.0.0-rc.10", "v12.34.56-rc.7")
REFUSED = ("v01.0.0", "v1.00.0", "v1.0.00", "v1.0.0-1", "v1.0.0-rc.0", "v1.0.0-rc.01", "v1.0.0-beta.1", "v1.0.0-rc", "v1.0.0-rc.1.2", "v1.0.0-RC.1",
           "v1.0", "1.0.0", "v1.0.0;id", "v1.0.0\nv1.0.0", "v1.0.0-rc.1$(id)", "")
# What a value that is more than one line, or holds a control character, or
# nothing, must meet: grep matches by line and `$` in a shell's pattern is
# the string's end, so each copy is run with these as well.
HOLES = ("{ok}\nversion=$(id)", "version=$(id)\n{ok}", "{ok}\n", "{ok}\r", "\n{ok}", "{ok}\x01", "")
WORDS = "vX.Y.Z, or vX.Y.Z-rc.N for a candidate, N from 1"


class TagForm(unittest.TestCase):
    def workflow(self, name):
        return wc.read((ROOT / ".github" / "workflows" / name).read_text(encoding="utf-8"))

    def bash(self, script, **env):
        e = {k: v for k, v in os.environ.items() if not k.startswith("GITHUB_")}
        e.update(env)
        return subprocess.run(["bash", "--noprofile", "--norc", "-eo", "pipefail", "-c", script], env=e,
                              capture_output=True, text=True, timeout=60)

    def test_the_form(self):
        for tag in TAKEN:
            self.assertTrue(re.fullmatch(TAG_FORM[1:-1], tag), tag)
        for tag in REFUSED:
            self.assertFalse(re.fullmatch(TAG_FORM[1:-1], tag), tag)

    def test_every_release_job_holds_it_first(self):
        jobs = self.workflow("release.yml")["jobs"]
        self.assertGreater(len(jobs), 10)
        for name, job in jobs.items():
            with self.subTest(job=name):
                first = wc.steps(job)[0]
                self.assertEqual(str(first.get("name")), "The tag is a release's")
                run = str(first["run"])
                self.assertIn(f"re='{TAG_FORM}'", run)
                self.assertIn(WORDS, run)
                for tag in TAKEN:
                    got = self.bash(run, TAG=tag)
                    self.assertEqual(got.returncode, 0, (tag, got.stdout, got.stderr))
                for tag in REFUSED:
                    got = self.bash(run, TAG=tag)
                    self.assertEqual(got.returncode, 1, tag)
                    self.assertIn(f"::error::the tag asked for is no release's: {WORDS}", got.stdout)

    def test_swift_binarys_jobs_hold_it_and_the_branch_first(self):
        # The version without its `v`, on the one branch release-verify.sh
        # takes the XCFramework's attestation from.
        jobs = self.workflow("swift-binary.yml")["jobs"]
        for name, branch_env in (("build", ("GITHUB_REF_TYPE", "GITHUB_REF_NAME")),
                                 ("pull-request", ("REF_TYPE", "BASE"))):
            run = str(wc.steps(jobs[name])[0]["run"])
            self.assertIn(f"re='^{TAG_FORM[2:]}'", run)
            self.assertIn(WORDS.replace("v", ""), run)

            def on(version, kind, branch):
                return self.bash(run, VERSION=version, **{branch_env[0]: kind, branch_env[1]: branch})

            for tag in TAKEN:
                with self.subTest(job=name, tag=tag):
                    want = swift_ref(tag[1:]).removeprefix("refs/heads/")
                    self.assertEqual(on(tag[1:], "branch", want).returncode, 0, tag)
                    for other in {"main", "release/1.0", "release/1.4", "topic"} - {want}:
                        got = on(tag[1:], "branch", other)
                        self.assertEqual(got.returncode, 1, (tag, other))
                        self.assertIn(f"XCFramework is built on {want}, the one branch", got.stdout)
                    self.assertEqual(on(tag[1:], "tag", want).returncode, 1, tag)
            # A tag's form without its `v`, and a version with one.
            for version in [r[1:] for r in REFUSED if r.startswith("v")] + ["v1.0.0"]:
                with self.subTest(job=name, version=version):
                    got = on(version, "branch", "main")
                    self.assertEqual(got.returncode, 1, version)
                    self.assertIn("no release's: X.Y.Z, or X.Y.Z-rc.N for a candidate, N from 1", got.stdout)

    def holes(self, ok):
        return [h.replace("{ok}", ok) for h in HOLES]

    def step(self, workflow, job, needle):
        steps = [str(s["run"]) for s in wc.steps(self.workflow(workflow)["jobs"][job]) if needle in str(s.get("run", ""))]
        self.assertEqual(len(steps), 1, (workflow, job, needle))
        return steps[0]

    def cut(self, run, marker):
        """The step up to the line holding marker, where its check ends."""
        lines = run.splitlines()
        at = next(i for i, line in enumerate(lines) if marker in line)
        return "\n".join(lines[:at] + ["echo PASSED"])

    def test_desktops_releases_are_judged_whole(self):
        # An X.Y.Z with or without its `v`, as before; the dispatch's
        # input is a make variable's value and a release's name.
        # `empty_ok`: the two steps where an empty input means this
        # checkout's server and shell.
        copies = (
            ("app", "The server to bundle", "RELEASE", self.cut(
                self.step("desktop.yml", "app", 'echo "version=${RELEASE#v}"'), 'if [ -n "$RELEASE" ]; then echo'), True),
            ("other-systems", "The release asked for", "STUDIO_SERVER_RELEASE",
             self.step("desktop.yml", "other-systems", "$STUDIO_SERVER_RELEASE =~") + "\necho PASSED", False),
            ("sign", "The release asked for", "RELEASE",
             self.step("desktop.yml", "sign", 'echo "version=${RELEASE#v}"') + "\necho PASSED", True),
            ("attach", "Attach", "RELEASE", self.cut(
                self.step("desktop.yml", "attach", "mv \"$dmg\""), 'version="${RELEASE#v}"'), False),
        )
        for job, name, var, run, empty_ok in copies:
            for ok in ("0.2.0", "v0.2.0", "10.20.30", "v1.4.2"):
                with self.subTest(copy=name, value=ok):
                    out = tempfile.NamedTemporaryFile("r")
                    got = self.bash(run, **{var: ok, "GITHUB_OUTPUT": out.name})
                    self.assertEqual(got.returncode, 0, (got.stdout, got.stderr))
                    self.assertIn("PASSED", got.stdout)
            bad = [v for ok in ("0.2.0", "v0.2.0") for v in self.holes(ok)[:-1]] + ["", "v", "1.0", "1.0.0-rc.1", "1.0.0;id"]
            for value in bad:
                with self.subTest(copy=name, value=value):
                    out = tempfile.NamedTemporaryFile("r")
                    got = self.bash(run, **{var: value, "GITHUB_OUTPUT": out.name})
                    if value == "" and empty_ok:
                        self.assertEqual(got.returncode, 0, (got.stdout, got.stderr))
                        continue
                    self.assertEqual(got.returncode, 1, (value, got.stdout, got.stderr))
                    self.assertNotIn("PASSED", got.stdout)
                    self.assertIn("::error::release is a tag such as", got.stdout)
                    self.assertEqual(out.read(), "", "nothing reached $GITHUB_OUTPUT")

    def test_attach_sh_judges_its_release_whole(self):
        script = HERE.parent / "integrations" / "studio-desktop" / "scripts" / "attach.sh"
        for ok in ("0.2.0", "v0.2.0", "v10.20.30"):
            got = subprocess.run([DASH, str(script), ok], capture_output=True, text=True, timeout=60)
            self.assertIn("nothing to attach", got.stderr, (ok, got.stderr))
        for value in [v for ok in ("0.2.0", "v0.2.0") for v in self.holes(ok)] + ["v", "1.0", "1.0.0-rc.1"]:
            with self.subTest(value=value):
                got = subprocess.run([DASH, str(script), value], capture_output=True, text=True, timeout=60)
                self.assertEqual(got.returncode, 1)
                self.assertIn("release is a tag such as", got.stderr)

    def test_packages_homebrew_judges_its_version_whole(self):
        run = self.cut(self.step("packages.yml", "homebrew", "is not a release version"), "The release says what it is")
        for ok in ("0.2.0", "v0.2.0", "v10.20.30"):
            got = self.bash(run, RELEASE_TAG=ok, HOMEBREW_TAP_TOKEN="t")
            self.assertEqual((got.returncode, "PASSED" in got.stdout), (0, True), (ok, got.stdout, got.stderr))
        for value in [v for ok in ("0.2.0", "v0.2.0") for v in self.holes(ok)] + ["v1.0.0-rc.1", "v", "1.0"]:
            with self.subTest(value=value):
                out = tempfile.NamedTemporaryFile("r")
                got = self.bash(run, RELEASE_TAG=value, HOMEBREW_TAP_TOKEN="t", GITHUB_OUTPUT=out.name)
                self.assertNotIn("PASSED", got.stdout)
                self.assertEqual(out.read(), "set=false\n")

    def test_image_latests_version_is_judged_whole(self):
        text = (HERE / "image-latest.sh").read_text(encoding="utf-8")
        block = re.search(r"^nl='\n'\nif ! \{ case \$version in\n(?:.*\n)*?^fi\n", text, re.M)
        self.assertTrue(block)
        for value, ok in [("1.4.2", True), ("0.2.0", True), ("1.0.0-rc.1", False), ("v1.0.0", False), ("", False)] + \
                [(v, False) for v in self.holes("1.4.2")]:
            with self.subTest(value=value):
                script = f'set -eu\nsays() {{ echo "SAYS $*"; }}\nversion=$1\nset -- x "$1"\n{block.group(0)}echo PASSED\n'
                got = subprocess.run([DASH, "-c", script, "sh", value], capture_output=True, text=True, timeout=60)
                self.assertEqual("PASSED" in got.stdout, ok, (value, got.stdout, got.stderr))

    def test_every_digest_and_latest_is_judged_whole(self):
        loop = re.search(r"re='\^sha256[^\n]*\n\s*for d in \"\$list\" \"\$amd\" \"\$arm\"; do\n(?:.*\n)*?\s*done\n",
                         self.step("release.yml", "manifest", "a digest of"))
        self.assertTrue(loop)
        ok = digest(7)
        for position in range(3):
            for value, good in [(ok, True)] + [(h, False) for h in self.holes(ok)] + [("sha256:abc", False)]:
                with self.subTest(position=position, value=value):
                    vals = [ok, ok, ok]
                    vals[position] = value
                    got = self.bash(f'list=$LIST amd=$AMD arm=$ARM IMAGE=i version=v\n{loop.group(0)}echo PASSED',
                                    LIST=vals[0], AMD=vals[1], ARM=vals[2])
                    self.assertEqual(got.returncode == 0, good, (value, got.stdout))

    def test_release_verifys_whole_matches(self):
        text = SCRIPT.read_text(encoding="utf-8")
        helper = re.search(r"^whole\(\) \{\n(?:.*\n)*?^\}\n", text, re.M)
        self.assertTrue(helper)
        uses = re.findall(r"whole '([^']+)' \"\$(\w+)\"", text)
        self.assertEqual(sorted(v for _, v in uses), ["digest", "latest", "tag"])
        valid = {"tag": ("v0.2.0", "v1.0.0-rc.3"), "latest": ("v1.4.2",), "digest": (digest(3),)}
        nl = "nl='\n'\n"
        for pattern, var in uses:
            for ok in valid[var]:
                for value, good in [(ok, True)] + [(h, False) for h in self.holes(ok)]:
                    with self.subTest(var=var, value=value):
                        got = subprocess.run([DASH, "-c", f"{nl}{helper.group(0)}whole '{pattern}' \"$1\"", "sh", value],
                                             capture_output=True, text=True, timeout=60)
                        self.assertEqual(got.returncode == 0, good, (value, got.stderr))

    def test_the_scripts_take_it(self):
        bin_ = tempfile.TemporaryDirectory()
        self.addCleanup(bin_.cleanup)
        gh = pathlib.Path(bin_.name) / "gh"
        gh.write_text("#!/bin/sh\necho 'gh: not stood in for' >&2\nexit 1\n")
        gh.chmod(0o755)
        env = {**os.environ, "PATH": f"{bin_.name}{os.pathsep}{os.environ['PATH']}", "GITHUB_REPOSITORY": "o/r",
               "API_LAG_TRIES": "1", "API_LAG_WAIT": "0"}
        for tag in TAKEN + REFUSED:
            with self.subTest(tag=tag):
                verify = subprocess.run([DASH, str(SCRIPT), "--assets", tag], env=env, capture_output=True,
                                        text=True, timeout=60)
                latest = subprocess.run([DASH, str(HERE / "release-latest.sh"), "npm-tag", tag], env=env,
                                        capture_output=True, text=True, timeout=60)
                if tag in TAKEN:
                    self.assertEqual(verify.returncode, 0, verify.stderr)
                    self.assertNotIn("is no release's tag", latest.stderr)
                elif tag:
                    self.assertIn(f"{tag} is no release's tag: {WORDS}", verify.stderr)
                    self.assertIn(f"{tag} is no release's tag: {WORDS}", latest.stderr)
                    self.assertEqual(latest.returncode, 1)


if __name__ == "__main__":
    unittest.main()

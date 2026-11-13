#!/usr/bin/env python3
"""tools/image-latest.sh, the move of the image's `latest` that
packages.yml's and release.yml's `image-latest` run, against stand-ins for
`gh` and `docker` keeping the releases and the registry in a folder: where
it moves `latest`, where it leaves it and says why, and where it fails.

    python3 tools/test_image_latest.py
"""

import os
import pathlib
import subprocess
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
SCRIPT = HERE / "image-latest.sh"
IMAGE = "ghcr.io/o/fennec-server"

# `gh release view TAG ...` answers "isDraft isPrerelease" from
# $STATE/releases (a line "tag draft prerelease" a release); `gh release
# list ...` the published releases' tags but those $STATE/unlisted names (a
# list a moment behind the publish), and then runs $STATE/after-list-<n>
# after its n-th answer: a release published meanwhile.
GH = r"""#!/bin/sh
set -eu
case "$1 $2" in
"release view")
    line=$(grep "^$3 " "$STATE/releases") || { echo "release not found" >&2; exit 1; }
    set -- $line
    echo "$2 $3"
    ;;
"release list")
    n=$(($(cat "$STATE/lists" 2>/dev/null || echo 0) + 1))
    echo "$n" >"$STATE/lists"
    touch "$STATE/unlisted"
    awk '$2 == "false" && $3 == "false" { print $1 }' "$STATE/releases" | grep -vxF -f "$STATE/unlisted" || true
    if [ -f "$STATE/after-list-$n" ]; then sh "$STATE/after-list-$n"; fi
    ;;
*)
    echo "gh $*: not stood in for" >&2
    exit 1
    ;;
esac
"""

# `docker buildx imagetools inspect --format '{{json .Manifest}}' REF`
# answers the list $STATE/registry/<tag> names, and `... create -t REF SRC`
# copies SRC's to REF's, noting it in $STATE/creates; with
# $STATE/broken-create it notes it and writes nothing.
DOCKER = r"""#!/bin/sh
set -eu
[ "$1 $2" = "buildx imagetools" ] || { echo "docker $*: not stood in for" >&2; exit 1; }
case $3 in
inspect)
    [ -f "$STATE/registry/${6##*:}" ] || { echo "ERROR: $6: not found" >&2; exit 1; }
    printf '{"mediaType":"application/vnd.docker.distribution.manifest.list.v2+json","digest":"%s","size":683}\n' \
        "$(cat "$STATE/registry/${6##*:}")"
    ;;
create)
    echo "latest from ${6##*:}" >>"$STATE/creates"
    [ -f "$STATE/broken-create" ] || cp "$STATE/registry/${6##*:}" "$STATE/registry/${5##*:}"
    ;;
*)
    exit 1
    ;;
esac
"""


def digest(n):
    return "sha256:" + str(n) * 64


class ImageLatest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        root = pathlib.Path(self.dir.name)
        self.state = root / "state"
        (self.state / "registry").mkdir(parents=True)
        self.bin = root / "bin"
        self.bin.mkdir()
        for name, text in (("gh", GH), ("docker", DOCKER)):
            (self.bin / name).write_text(text)
            (self.bin / name).chmod(0o755)
        self.summary = root / "summary"
        self.summary.write_text("")

    def tearDown(self):
        self.dir.cleanup()

    def given(self, releases, registry):
        """Releases ("v0.3.0", or "v0.3.0 true false" for a draft) and the
        registry's lists, by tag."""
        lines = [r if " " in r else f"{r} false false" for r in releases]
        (self.state / "releases").write_text("".join(f"{line}\n" for line in lines))
        for tag, d in registry.items():
            (self.state / "registry" / tag).write_text(d)

    def run_it(self, tag, **env):
        e = {k: v for k, v in os.environ.items() if k not in ("EVENT_PRERELEASE", "GH_TOKEN")}
        e.update(PATH=f"{self.bin}{os.pathsep}{os.environ['PATH']}", STATE=str(self.state),
                 GITHUB_REPOSITORY="o/r", GITHUB_STEP_SUMMARY=str(self.summary), RUNNER_TEMP=self.dir.name)
        e.update(env)
        return subprocess.run(["sh", str(SCRIPT), IMAGE, tag], env=e, capture_output=True, text=True, timeout=60)

    def latest(self):
        p = self.state / "registry" / "latest"
        return p.read_text() if p.exists() else None

    def creates(self):
        p = self.state / "creates"
        return p.read_text().splitlines() if p.exists() else []

    def assertLeft(self, got, why, latest):
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertIn(why, got.stdout + got.stderr)
        self.assertEqual(self.creates(), [])
        self.assertEqual(self.latest(), latest)

    def test_the_newest_published_release_moves_it(self):
        self.given(["v0.2.0", "v0.3.0"], {"0.2.0": digest(2), "0.3.0": digest(3), "latest": digest(2)})
        got = self.run_it("v0.3.0")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(self.creates(), ["latest from 0.3.0"])
        self.assertEqual(self.latest(), digest(3))
        self.assertIn(f"{IMAGE}:latest is 0.3.0, {digest(3)}", self.summary.read_text())

    def test_the_first_release_makes_it(self):
        self.given(["v0.3.0"], {"0.3.0": digest(3)})
        got = self.run_it("0.3.0")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(self.latest(), digest(3))

    def test_where_it_is_there_already_nothing_is_written(self):
        self.given(["v0.3.0"], {"0.3.0": digest(3), "latest": digest(3)})
        self.assertLeft(self.run_it("v0.3.0"), "latest is 0.3.0 already", digest(3))

    def test_a_list_a_moment_behind_the_publish_moves_it_all_the_same(self):
        # The release says it is published; the list read just after does
        # not hold it yet, and its newest is the release before.
        self.given(["v0.2.0", "v0.3.0"], {"0.2.0": digest(2), "0.3.0": digest(3), "latest": digest(2)})
        (self.state / "unlisted").write_text("v0.3.0\n")
        got = self.run_it("v0.3.0")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(self.latest(), digest(3))

    def test_an_older_release_leaves_it(self):
        self.given(["v0.2.0", "v0.10.0"], {"0.2.0": digest(2), "0.10.0": digest(1), "latest": digest(1)})
        got = self.run_it("v0.2.0")
        self.assertLeft(got, "latest is left on the newest published release, 0.10.0", digest(1))
        self.assertIn("::warning::", got.stdout)

    def test_a_draft_a_pre_release_and_no_version_leave_it(self):
        self.given(["v0.2.0", "v0.3.0 true false", "v0.4.0 false true", "v1.0.0-rc.1 false true"],
                   {"0.2.0": digest(2), "0.3.0": digest(3), "0.4.0": digest(4), "latest": digest(2)})
        self.assertLeft(self.run_it("v0.3.0"), "v0.3.0 is a draft: latest moves once it is published", digest(2))
        self.assertLeft(self.run_it("v0.4.0"), "v0.4.0 is a pre-release", digest(2))
        self.assertLeft(self.run_it("v1.0.0-rc.1"), "v1.0.0-rc.1 is no X.Y.Z", digest(2))

    def test_the_release_events_pre_release_flag_leaves_it(self):
        self.given(["v0.2.0", "v0.3.0"], {"0.2.0": digest(2), "0.3.0": digest(3), "latest": digest(2)})
        self.assertLeft(self.run_it("v0.3.0", EVENT_PRERELEASE="true"), "v0.3.0 is a pre-release", digest(2))
        self.assertEqual(self.run_it("v0.3.0", EVENT_PRERELEASE="false").returncode, 0)
        self.assertEqual(self.latest(), digest(3))

    def test_no_list_yet_fails_saying_so(self):
        # Published before release.yml's manifest made 0.3.0: never green.
        self.given(["v0.2.0", "v0.3.0"], {"0.2.0": digest(2), "latest": digest(2)})
        got = self.run_it("v0.3.0")
        self.assertEqual(got.returncode, 1)
        self.assertIn(f"::error::{IMAGE}:0.3.0, the list latest copies, is not on the registry yet", got.stderr)
        self.assertIn("not found", got.stderr)
        self.assertIn("run release.yml again for v0.3.0", got.stderr)
        self.assertEqual(self.creates(), [])
        self.assertEqual(self.latest(), digest(2))

    def test_a_move_that_does_not_hold_fails(self):
        self.given(["v0.3.0"], {"0.3.0": digest(3), "latest": digest(2)})
        (self.state / "broken-create").write_text("")
        got = self.run_it("v0.3.0")
        self.assertEqual(got.returncode, 1)
        self.assertIn(f"latest is [{digest(2)}] after the move, not 0.3.0's list {digest(3)}", got.stderr)

    def test_a_newer_release_published_meanwhile_gets_it_back(self):
        # 0.4.0 is published after this run found 0.3.0 the newest, and its
        # own run moves latest before this one's move: latest goes on to it.
        self.given(["v0.2.0", "v0.3.0"], {"0.2.0": digest(2), "0.3.0": digest(3), "latest": digest(2)})
        (self.state / "after-list-1").write_text(
            f'echo "v0.4.0 false false" >>"$STATE/releases"\n'
            f'printf %s {digest(4)} >"$STATE/registry/0.4.0"\n'
            f'printf %s {digest(4)} >"$STATE/registry/latest"\n')
        got = self.run_it("v0.3.0")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(self.creates(), ["latest from 0.3.0", "latest from 0.4.0"])
        self.assertEqual(self.latest(), digest(4))

    def test_a_newer_release_without_its_list_leaves_it_on_this_one(self):
        self.given(["v0.2.0", "v0.3.0"], {"0.2.0": digest(2), "0.3.0": digest(3), "latest": digest(2)})
        (self.state / "after-list-1").write_text('echo "v0.4.0 false false" >>"$STATE/releases"\n')
        got = self.run_it("v0.3.0")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertIn("v0.4.0, published since, has no list on the registry yet", got.stdout)
        self.assertEqual(self.latest(), digest(3))

    def test_the_usage(self):
        got = subprocess.run(["sh", str(SCRIPT)], capture_output=True, text=True, timeout=60)
        self.assertEqual(got.returncode, 2)
        self.assertIn("tools/image-latest.sh IMAGE TAG", got.stderr)


if __name__ == "__main__":
    unittest.main()

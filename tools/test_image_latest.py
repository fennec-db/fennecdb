#!/usr/bin/env python3
"""tools/image-latest.sh, the move of the image's `latest` that
packages.yml's and release.yml's `image-latest` run, against stand-ins for
`gh` and `docker` that keep the releases and the registry in a folder and
answer as those do: where it moves `latest`, where it leaves it and says
why, and where it fails. Every case runs the script under /bin/dash, the
`sh` of the ubuntu-24.04 runners both jobs run it on: a bash, as macOS's
`sh` is, lets by what dash refuses.

    python3 tools/test_image_latest.py
"""

import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
SCRIPT = HERE / "image-latest.sh"
IMAGE = "ghcr.io/o/fennec-server"
DASH = "/bin/dash"

# gh as both jobs' tokens, which read alone, meet it. `gh api
# repos/o/r/releases/tags/TAG` answers as the API does: a published release
# alone, from $STATE/releases (a line "tag draft prerelease" a release), put
# through --jq; a draft, or no release, is a 404, its body on stdout and
# gh's line on stderr, and $STATE/api-error holds another error's line (a
# rate limit, a 5xx) to answer instead. `gh release list` honours the flags
# it is given -- a draft or a pre-release listed unless excluded, newest
# first, -L, --json tagName put through -q -- leaving out $STATE/unlisted's
# names (a list a moment behind the publish), then runs
# $STATE/after-list-<n> after its n-th answer: a release published
# meanwhile. From the call $STATE/gh-down-from names on, every call fails as
# one that cannot reach the API does. Anything else, `gh release view`
# among it, is not stood in for.
GH = r"""#!/bin/sh
set -eu
calls=$(($(cat "$STATE/gh-calls" 2>/dev/null || echo 0) + 1))
echo "$calls" >"$STATE/gh-calls"
if [ -f "$STATE/gh-down-from" ] && [ "$calls" -ge "$(cat "$STATE/gh-down-from")" ]; then
    case $1 in
    api) echo "Get \"https://api.github.com/$2\": dial tcp: lookup api.github.com: no such host" >&2 ;;
    *) echo 'Post "https://api.github.com/graphql": dial tcp: lookup api.github.com: no such host' >&2 ;;
    esac
    exit 1
fi
case $1 in
api)
    path=$2
    shift 2
    filter=.
    while [ $# -gt 0 ]; do
        case $1 in
        --jq | -q) filter=$2; shift 2 ;;
        *) echo "gh api: $1: not stood in for" >&2; exit 1 ;;
        esac
    done
    tag=${path#repos/o/r/releases/tags/}
    [ "$tag" != "$path" ] || { echo "gh api $path: not stood in for" >&2; exit 1; }
    if [ -f "$STATE/api-error" ]; then
        echo '{"message":"stood in"}'
        cat "$STATE/api-error" >&2
        exit 1
    fi
    line=$(awk -v t="$tag" '$1 == t && $2 == "false"' "$STATE/releases")
    if [ -z "$line" ]; then
        echo '{"message":"Not Found","documentation_url":"https://docs.github.com/rest/releases/releases#get-a-release-by-tag-name","status":"404"}'
        echo 'gh: Not Found (HTTP 404)' >&2
        exit 1
    fi
    set -- $line
    printf '{"tag_name":"%s","draft":%s,"prerelease":%s}\n' "$1" "$2" "$3" | jq -r "$filter"
    ;;
release)
    [ "$2" = list ] || { echo "gh $*: not stood in for" >&2; exit 1; }
    shift 2
    drafts=true prereleases=true limit=30 fields= filter=. repo=
    while [ $# -gt 0 ]; do
        case $1 in
        --exclude-drafts) drafts=false; shift ;;
        --exclude-pre-releases) prereleases=false; shift ;;
        -L | --limit) limit=$2; shift 2 ;;
        --json) fields=$2; shift 2 ;;
        -q | --jq) filter=$2; shift 2 ;;
        -R | --repo) repo=$2; shift 2 ;;
        *) echo "gh release list: $1: not stood in for" >&2; exit 1 ;;
        esac
    done
    [ "$repo" = o/r ] || { echo "gh release list -R $repo: not the repository" >&2; exit 1; }
    [ "$fields" = tagName ] || { echo "gh release list --json $fields: not stood in for" >&2; exit 1; }
    n=$(($(cat "$STATE/lists" 2>/dev/null || echo 0) + 1))
    echo "$n" >"$STATE/lists"
    touch "$STATE/unlisted"
    awk -v d="$drafts" -v p="$prereleases" '(d == "true" || $2 == "false") && (p == "true" || $3 == "false") { print $1 }' "$STATE/releases" |
        grep -vxF -f "$STATE/unlisted" |
        awk '{ l[NR] = $0 } END { for (i = NR; i > 0; i--) print l[i] }' |
        head -n "$limit" |
        jq -R . | jq -rs "map({tagName: .}) | $filter"
    if [ -f "$STATE/after-list-$n" ]; then sh "$STATE/after-list-$n"; fi
    ;;
*)
    echo "gh $*: not stood in for" >&2
    exit 1
    ;;
esac
"""

# `docker buildx imagetools inspect --format '{{json .Manifest}}' REF`
# answers the list $STATE/registry/<tag> names, and a tag it has not as
# buildx does, `ERROR: REF: not found`; $STATE/inspect-error-<tag> holds
# another error to answer for that tag instead. `... create -t REF SRC`
# copies SRC's list to REF's, noting it in $STATE/creates (with
# $STATE/broken-create it notes it and writes nothing), and then runs
# $STATE/after-create-<n> after its n-th: another run's move landing before
# this one reads `latest` back.
DOCKER = r"""#!/bin/sh
set -eu
[ "$1 $2" = "buildx imagetools" ] || { echo "docker $*: not stood in for" >&2; exit 1; }
case $3 in
inspect)
    [ "$4 $5" = "--format {{json .Manifest}}" ] || { echo "docker $*: not stood in for" >&2; exit 1; }
    tag=${6##*:}
    if [ -f "$STATE/inspect-error-$tag" ]; then cat "$STATE/inspect-error-$tag" >&2; exit 1; fi
    [ -f "$STATE/registry/$tag" ] || { echo "ERROR: $6: not found" >&2; exit 1; }
    printf '{"mediaType":"application/vnd.docker.distribution.manifest.list.v2+json","digest":"%s","size":683}\n' \
        "$(cat "$STATE/registry/$tag")"
    ;;
create)
    n=$(($(cat "$STATE/created" 2>/dev/null || echo 0) + 1))
    echo "$n" >"$STATE/created"
    echo "latest from ${6##*:}" >>"$STATE/creates"
    [ -f "$STATE/broken-create" ] || cp "$STATE/registry/${6##*:}" "$STATE/registry/${5##*:}"
    if [ -f "$STATE/after-create-$n" ]; then sh "$STATE/after-create-$n"; fi
    ;;
*)
    exit 1
    ;;
esac
"""

NO_HOST = "dial tcp: lookup api.github.com: no such host"


def digest(n):
    return "sha256:" + str(n) * 64


class ImageLatest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not os.access(DASH, os.X_OK):
            raise AssertionError(f"{DASH}, the runners' sh, is what the script is held to, and is not here")

    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.dir.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name, text in (("gh", GH), ("docker", DOCKER)):
            (self.bin / name).write_text(text)
            (self.bin / name).chmod(0o755)
        self.state = self.root / "state"
        self.summary = self.root / "summary"
        self.fresh()

    def tearDown(self):
        self.dir.cleanup()

    def fresh(self):
        """No release, an empty registry and no summary: a subtest's start."""
        if self.state.exists():
            shutil.rmtree(self.state)
        (self.state / "registry").mkdir(parents=True)
        self.summary.write_text("")

    def given(self, releases, registry):
        """Releases ("v0.3.0", or "v0.3.0 true false" for a draft) and the
        registry's lists, by tag."""
        lines = [r if " " in r else f"{r} false false" for r in releases]
        (self.state / "releases").write_text("".join(f"{line}\n" for line in lines))
        for tag, d in registry.items():
            (self.state / "registry" / tag).write_text(d)

    def meanwhile(self, hook, *lines):
        """What another run, or the owner, does at a point of this run:
        after the n-th release list (`after-list-<n>`) or create
        (`after-create-<n>`)."""
        (self.state / hook).write_text("".join(f"{line}\n" for line in lines))

    def run_it(self, tag, **env):
        e = {k: v for k, v in os.environ.items() if k not in ("EVENT_PRERELEASE", "GH_TOKEN")}
        e.update(PATH=f"{self.bin}{os.pathsep}{os.environ['PATH']}", STATE=str(self.state),
                 GITHUB_REPOSITORY="o/r", GITHUB_STEP_SUMMARY=str(self.summary), RUNNER_TEMP=self.dir.name)
        e.update(env)
        return subprocess.run([DASH, str(SCRIPT), IMAGE, tag], env=e, capture_output=True, text=True, timeout=60)

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

    def assertFails(self, got, why, latest, creates=()):
        self.assertEqual(got.returncode, 1, got.stdout + got.stderr)
        self.assertIn(f"::error::{why}", got.stderr)
        self.assertEqual(self.creates(), list(creates))
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

    def test_a_newer_draft_or_pre_release_holds_nothing_back(self):
        # The list is read for published releases alone: a newer draft or
        # pre-release, each with its list made, is no newer release.
        self.given(["v0.2.0", "v0.3.0", "v0.4.0 true false", "v0.5.0 false true"],
                   {"0.2.0": digest(2), "0.3.0": digest(3), "0.4.0": digest(4), "0.5.0": digest(5),
                    "latest": digest(2)})
        got = self.run_it("v0.3.0")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(self.creates(), ["latest from 0.3.0"])
        self.assertEqual(self.latest(), digest(3))

    def test_a_draft_seen_from_either_caller_leaves_it(self):
        # At the tag's push release.yml's run finds the release a draft,
        # with `manifest`'s list made, and a dispatch of packages.yml for a
        # draft's tag (or the tag of no release) finds the same: the API
        # answers a 404 by the tag, whatever the token, and both end green.
        callers = (("release.yml", {}), ("packages.yml, dispatched", {"EVENT_PRERELEASE": ""}),
                   ("packages.yml, by a release event", {"EVENT_PRERELEASE": "false"}))
        for caller, env in callers:
            for tag in ("v0.3.0", "v0.9.0"):
                with self.subTest(caller=caller, tag=tag):
                    self.fresh()
                    self.given(["v0.2.0", "v0.3.0 true false"],
                               {"0.2.0": digest(2), "0.3.0": digest(3), "latest": digest(2)})
                    got = self.run_it(tag, **env)
                    self.assertLeft(got, f"{tag} is not published yet: packages.yml moves latest when it is",
                                    digest(2))
                    self.assertNotIn("::error::", got.stderr)

    def test_a_pre_release_and_no_version_leave_it(self):
        self.given(["v0.2.0", "v0.4.0 false true", "v1.0.0-rc.1 false true"],
                   {"0.2.0": digest(2), "0.4.0": digest(4), "latest": digest(2)})
        self.assertLeft(self.run_it("v0.4.0"), "v0.4.0 is a pre-release", digest(2))
        self.assertLeft(self.run_it("v1.0.0-rc.1"), "v1.0.0-rc.1 is no X.Y.Z", digest(2))

    def test_a_release_saying_neither_true_nor_false_fails(self):
        self.given(["v0.2.0", "v0.3.0 false null"], {"0.2.0": digest(2), "0.3.0": digest(3), "latest": digest(2)})
        self.assertFails(self.run_it("v0.3.0"), "v0.3.0's release says [null] for prerelease, neither true nor"
                         " false", digest(2))

    def test_the_release_events_pre_release_flag_leaves_it(self):
        self.given(["v0.2.0", "v0.3.0"], {"0.2.0": digest(2), "0.3.0": digest(3), "latest": digest(2)})
        self.assertLeft(self.run_it("v0.3.0", EVENT_PRERELEASE="true"), "v0.3.0 is a pre-release", digest(2))
        self.assertEqual(self.run_it("v0.3.0", EVENT_PRERELEASE="false").returncode, 0)
        self.assertEqual(self.latest(), digest(3))

    def test_gh_failing_fails(self):
        # Only the API's 404 means "not published yet": gh out of reach of
        # the API, or any other answer, fails -- never green with latest
        # left behind.
        def given():
            self.fresh()
            self.given(["v0.2.0", "v0.3.0"], {"0.2.0": digest(2), "0.3.0": digest(3), "latest": digest(2)})

        cases = (
            # The lookup by tag, the first call, cannot reach the API.
            ("1", None, "v0.3.0's release could not be read", NO_HOST, digest(2), ()),
            # The release list, before the move.
            ("2", None, "the release list could not be read", NO_HOST, digest(2), ()),
            # The release list read again after the move: latest moved, and
            # the run fails, not having looked for a newer release.
            ("3", None, "the release list could not be read", NO_HOST, digest(3), ["latest from 0.3.0"]),
            # The API answering another error than a 404.
            (None, "gh: API rate limit exceeded for installation ID 1. (HTTP 403)",
             "v0.3.0's release could not be read", "(HTTP 403)", digest(2), ()),
            (None, "gh: HTTP 502", "v0.3.0's release could not be read", "HTTP 502", digest(2), ()))
        for down_from, api_error, why, said, latest, creates in cases:
            with self.subTest(down_from=down_from, api_error=api_error):
                given()
                if down_from:
                    (self.state / "gh-down-from").write_text(down_from)
                if api_error:
                    (self.state / "api-error").write_text(api_error + "\n")
                got = self.run_it("v0.3.0")
                self.assertFails(got, why, latest, creates)
                self.assertIn(said, got.stderr)
                self.assertNotIn("not published yet", got.stdout + got.stderr)

    def test_an_inspect_error_other_than_not_found_fails(self):
        # Only the registry's `<ref>: not found` means no list: a registry
        # out of reach or a token refused fails, never read as a list not
        # made yet.
        refused = ("ERROR: failed to authorize: failed to fetch anonymous token: unexpected status from GET"
                   " request to https://ghcr.io/token?scope=repository%3Ao%2Ffennec-server%3Apull: 403 Forbidden")
        unreachable = ('ERROR: failed to do request: Head "https://ghcr.io/v2/o/fennec-server/manifests/{tag}":'
                       " dial tcp: lookup ghcr.io: no such host")
        for tag, error, latest, creates in (("0.3.0", refused, digest(2), ()),
                                            ("latest", unreachable.format(tag="latest"), digest(2), ()),
                                            ("0.4.0", unreachable.format(tag="0.4.0"), digest(3),
                                             ["latest from 0.3.0"])):
            with self.subTest(tag=tag):
                self.fresh()
                self.given(["v0.2.0", "v0.3.0"], {"0.2.0": digest(2), "0.3.0": digest(3), "latest": digest(2)})
                (self.state / f"inspect-error-{tag}").write_text(error + "\n")
                if tag == "0.4.0":
                    # Published while this run moved latest: the loop's
                    # look at its list is the one refused.
                    self.meanwhile("after-list-1", 'echo "v0.4.0 false false" >>"$STATE/releases"')
                got = self.run_it("v0.3.0")
                self.assertFails(got, f"{IMAGE}:{tag} could not be read from the registry: {error}",
                                 latest, creates)
                self.assertNotIn("is not on the registry yet", got.stderr)
                self.assertNotIn("has no list on the registry yet", got.stdout)

    def test_a_list_answered_without_its_digest_fails(self):
        # Read as no list, an answer naming no digest would leave latest
        # behind a newer release, green.
        self.given(["v0.2.0", "v0.3.0"], {"0.2.0": digest(2), "0.3.0": digest(3), "latest": digest(2)})
        self.meanwhile("after-list-1", 'echo "v0.4.0 false false" >>"$STATE/releases"',
                       'printf "" >"$STATE/registry/0.4.0"')
        got = self.run_it("v0.3.0")
        self.assertFails(got, f"{IMAGE}:0.4.0's list, as the registry gave it, names no digest", digest(3),
                         ["latest from 0.3.0"])
        self.assertNotIn("has no list on the registry yet", got.stdout)

    def test_no_list_yet_fails_saying_so(self):
        # Published before release.yml's manifest made 0.3.0: never green.
        self.given(["v0.2.0", "v0.3.0"], {"0.2.0": digest(2), "latest": digest(2)})
        got = self.run_it("v0.3.0")
        self.assertFails(got, f"{IMAGE}:0.3.0, the list latest copies, is not on the registry yet", digest(2))
        self.assertIn(f"ERROR: {IMAGE}:0.3.0: not found", got.stderr)
        self.assertIn("published before image (manifest) finished, nothing needs re-running", got.stderr)
        self.assertIn("re-run that run's failed jobs (RELEASING.md, step 4), not the whole workflow", got.stderr)

    def test_a_move_that_does_not_hold_fails(self):
        self.given(["v0.3.0"], {"0.3.0": digest(3), "latest": digest(2)})
        (self.state / "broken-create").write_text("")
        got = self.run_it("v0.3.0")
        self.assertFails(got, f"{IMAGE}:latest is [{digest(2)}] after the move, neither 0.3.0's list {digest(3)}"
                         " nor a newer published release's", digest(2), ["latest from 0.3.0"])

    def test_a_newer_release_published_meanwhile_gets_it_back(self):
        # 0.4.0 is published after this run found 0.3.0 the newest, and its
        # own run moves latest before this one's move: latest goes on to it.
        self.given(["v0.2.0", "v0.3.0"], {"0.2.0": digest(2), "0.3.0": digest(3), "latest": digest(2)})
        self.meanwhile("after-list-1", 'echo "v0.4.0 false false" >>"$STATE/releases"',
                       f'printf %s {digest(4)} >"$STATE/registry/0.4.0"',
                       f'printf %s {digest(4)} >"$STATE/registry/latest"')
        got = self.run_it("v0.3.0")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(self.creates(), ["latest from 0.3.0", "latest from 0.4.0"])
        self.assertEqual(self.latest(), digest(4))

    def test_a_newer_release_without_its_list_leaves_it_on_this_one(self):
        self.given(["v0.2.0", "v0.3.0"], {"0.2.0": digest(2), "0.3.0": digest(3), "latest": digest(2)})
        self.meanwhile("after-list-1", 'echo "v0.4.0 false false" >>"$STATE/releases"')
        got = self.run_it("v0.3.0")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertIn("v0.4.0, published since, has no list on the registry yet", got.stdout)
        self.assertEqual(self.latest(), digest(3))

    def test_a_newer_releases_move_between_the_create_and_the_read_back_stands(self):
        # 0.4.0's own run, running at once, moves latest after this run's
        # create and before its read-back: latest names a newer published
        # release's list, which is where it should be.
        self.given(["v0.2.0", "v0.3.0"], {"0.2.0": digest(2), "0.3.0": digest(3), "latest": digest(2)})
        self.meanwhile("after-create-1", 'echo "v0.4.0 false false" >>"$STATE/releases"',
                       f'printf %s {digest(4)} >"$STATE/registry/0.4.0"',
                       f'printf %s {digest(4)} >"$STATE/registry/latest"')
        got = self.run_it("v0.3.0")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertIn(f"{IMAGE}:latest is 0.4.0, {digest(4)}: v0.4.0's own run moved it there after this one's"
                      " move to 0.3.0", got.stdout)
        self.assertIn(f"{IMAGE}:latest is 0.4.0 already", got.stdout)
        self.assertEqual(self.creates(), ["latest from 0.3.0"])
        self.assertEqual(self.latest(), digest(4))

    def test_any_other_list_between_the_create_and_the_read_back_fails(self):
        # An older release's list, a newer draft's (release.yml's manifest
        # made it at the tag; no run moves latest to a draft), and a list no
        # release names, a newer release published beside it: none of them
        # is where latest should be.
        cases = (("an older release's", digest(2), ()),
                 ("a newer draft's", digest(4), ('echo "v0.4.0 true false" >>"$STATE/releases"',
                                                 f'printf %s {digest(4)} >"$STATE/registry/0.4.0"')),
                 ("no release's", digest(9), ('echo "v0.4.0 false false" >>"$STATE/releases"',
                                              f'printf %s {digest(4)} >"$STATE/registry/0.4.0"')))
        for whose, moved_to, also in cases:
            with self.subTest(whose=whose):
                self.fresh()
                self.given(["v0.2.0", "v0.3.0"], {"0.2.0": digest(2), "0.3.0": digest(3), "latest": digest(2)})
                self.meanwhile("after-create-1", *also, f'printf %s {moved_to} >"$STATE/registry/latest"')
                got = self.run_it("v0.3.0")
                self.assertFails(got, f"{IMAGE}:latest is [{moved_to}] after the move, neither 0.3.0's list"
                                 f" {digest(3)} nor a newer published release's", moved_to, ["latest from 0.3.0"])

    def test_the_usage(self):
        got = subprocess.run([DASH, str(SCRIPT)], capture_output=True, text=True, timeout=60)
        self.assertEqual(got.returncode, 2)
        self.assertIn("tools/image-latest.sh IMAGE TAG", got.stderr)


if __name__ == "__main__":
    unittest.main()

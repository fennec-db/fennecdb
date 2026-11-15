#!/usr/bin/env python3
"""tools/release-latest.sh, which keeps GitHub's Latest and npm's `latest`
and `next` on the newest release and pre-release whatever goes out (a patch
to the previous minor, a release candidate), against a stand-in for `gh`
that keeps the releases in a folder and answers as gh does, in the pattern
of test_image_latest.py: where it moves Latest, which dist-tag it names,
where it leaves things and says why, and where it fails. Every case runs
the script under /bin/dash, the `sh` of the runners packages.yml runs it
on.

    python3 tools/test_release_latest.py
"""

import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
SCRIPT = HERE / "release-latest.sh"
DASH = "/bin/dash"

# gh as packages.yml's tokens meet it. `gh api repos/o/r/releases/tags/TAG`
# answers a published release alone, from $STATE/releases (a line "tag
# draft prerelease" a release), through --jq; a draft, or no release, is a
# 404, its body on stdout and gh's line on stderr, and so is a published
# one for the first $STATE/tag-lag reads (the API a moment behind its
# publish). `gh api
# repos/o/r/releases/latest` answers the tag $STATE/latest names, a 404
# where it is empty or missing. $STATE/api-error holds another error's line
# (a rate limit, a 5xx) for every `gh api` to answer instead. `gh release
# list` honours the flags it is given -- a draft or a pre-release listed
# unless excluded, newest first, -L, --json tagName or
# tagName,isPrerelease put through -q --
# leaving out $STATE/unlisted's names (a list a moment behind the publish),
# then runs $STATE/after-list-<n> after its n-th answer. `gh release edit
# TAG -R o/r --latest` makes a published, non-pre-release TAG Latest, as
# the API does (a draft or a pre-release is refused), noting it in
# $STATE/edits, and then runs $STATE/after-edit-<n> after its n-th: another
# run's move landing before this one reads Latest back; $STATE/broken-edit
# notes it and moves nothing. From the call $STATE/gh-down-from names on,
# every call fails as one that cannot reach the API does. Anything else is
# not stood in for.
GH = r"""#!/bin/sh
set -eu
calls=$(($(cat "$STATE/gh-calls" 2>/dev/null || echo 0) + 1))
echo "$calls" >"$STATE/gh-calls"
if [ -f "$STATE/gh-down-from" ] && [ "$calls" -ge "$(cat "$STATE/gh-down-from")" ]; then
    echo 'Get "https://api.github.com/graphql": dial tcp: lookup api.github.com: no such host' >&2
    exit 1
fi
notfound() {
    echo '{"message":"Not Found","documentation_url":"https://docs.github.com/rest/releases/releases","status":"404"}'
    echo 'gh: Not Found (HTTP 404)' >&2
    exit 1
}
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
    if [ -f "$STATE/api-error" ]; then
        echo '{"message":"stood in"}'
        cat "$STATE/api-error" >&2
        exit 1
    fi
    if [ "$path" = repos/o/r/releases/latest ]; then
        t=$(cat "$STATE/latest" 2>/dev/null || true)
        [ -n "$t" ] || notfound
        printf '{"tag_name":"%s"}\n' "$t" | jq -r "$filter"
        exit 0
    fi
    tag=${path#repos/o/r/releases/tags/}
    [ "$tag" != "$path" ] || { echo "gh api $path: not stood in for" >&2; exit 1; }
    lag=$(cat "$STATE/tag-lag" 2>/dev/null || echo 0)
    if [ "$lag" -gt 0 ]; then
        echo $((lag - 1)) >"$STATE/tag-lag"
        notfound
    fi
    line=$(awk -v t="$tag" '$1 == t && $2 == "false"' "$STATE/releases")
    [ -n "$line" ] || notfound
    set -- $line
    printf '{"tag_name":"%s","draft":%s,"prerelease":%s}\n' "$1" "$2" "$3" | jq -r "$filter"
    ;;
release)
    case $2 in
    list)
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
        case $fields in
        tagName) object='{tagName: .[0]}' ;;
        tagName,isPrerelease) object='{tagName: .[0], isPrerelease: (.[1] == "true")}' ;;
        *) echo "gh release list --json $fields: not stood in for" >&2; exit 1 ;;
        esac
        n=$(($(cat "$STATE/lists" 2>/dev/null || echo 0) + 1))
        echo "$n" >"$STATE/lists"
        touch "$STATE/unlisted"
        awk -v d="$drafts" -v p="$prereleases" -v u="$STATE/unlisted" '
            BEGIN { while ((getline l < u) > 0) gone[l] = 1 }
            (d == "true" || $2 == "false") && (p == "true" || $3 == "false") && !($1 in gone) { print $1, $3 }' "$STATE/releases" |
            awk '{ l[NR] = $0 } END { for (i = NR; i > 0; i--) print l[i] }' |
            head -n "$limit" |
            jq -R 'split(" ")' | jq -rs "map($object) | $filter"
        if [ -f "$STATE/after-list-$n" ]; then sh "$STATE/after-list-$n"; fi
        ;;
    edit)
        [ "$#" -eq 6 ] && [ "$4 $5 $6" = "-R o/r --latest" ] ||
            { echo "gh $*: not stood in for" >&2; exit 1; }
        tag=$3
        line=$(awk -v t="$tag" '$1 == t' "$STATE/releases")
        [ -n "$line" ] || notfound
        set -- $line
        if [ "$2" != false ] || [ "$3" != false ]; then
            echo 'gh: Validation Failed (HTTP 422)' >&2
            exit 1
        fi
        n=$(($(cat "$STATE/edited" 2>/dev/null || echo 0) + 1))
        echo "$n" >"$STATE/edited"
        echo "latest to $1" >>"$STATE/edits"
        [ -f "$STATE/broken-edit" ] || echo "$1" >"$STATE/latest"
        if [ -f "$STATE/after-edit-$n" ]; then sh "$STATE/after-edit-$n"; fi
        echo "https://github.com/o/r/releases/tag/$1"
        ;;
    *)
        echo "gh $*: not stood in for" >&2
        exit 1
        ;;
    esac
    ;;
*)
    echo "gh $*: not stood in for" >&2
    exit 1
    ;;
esac
"""

NO_HOST = "dial tcp: lookup api.github.com: no such host"


class ReleaseLatest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not os.access(DASH, os.X_OK):
            raise AssertionError(f"{DASH}, the runners' sh, is what the script is held to, and is not here")

    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.dir.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.bin / "gh").write_text(GH)
        (self.bin / "gh").chmod(0o755)
        self.state = self.root / "state"
        self.summary = self.root / "summary"
        self.fresh()

    def tearDown(self):
        self.dir.cleanup()

    def fresh(self):
        """No release, no Latest and no summary: a subtest's start."""
        if self.state.exists():
            shutil.rmtree(self.state)
        self.state.mkdir()
        self.summary.write_text("")

    def given(self, releases, latest=None):
        """Releases ("v0.3.0", or "v0.3.0 true false" for a draft) and the
        one GitHub names Latest."""
        lines = [r if " " in r else f"{r} false false" for r in releases]
        (self.state / "releases").write_text("".join(f"{line}\n" for line in lines))
        if latest:
            (self.state / "latest").write_text(latest + "\n")

    def meanwhile(self, hook, *lines):
        (self.state / hook).write_text("".join(f"{line}\n" for line in lines))

    def run_it(self, mode, tag, **env):
        e = {k: v for k, v in os.environ.items()
             if k not in ("EVENT_PRERELEASE", "GH_TOKEN", "GITHUB_EVENT_NAME", "API_LAG_TRIES", "API_LAG_WAIT")}
        e.update(PATH=f"{self.bin}{os.pathsep}{os.environ['PATH']}", STATE=str(self.state),
                 GITHUB_REPOSITORY="o/r", GITHUB_STEP_SUMMARY=str(self.summary), RUNNER_TEMP=self.dir.name,
                 API_LAG_WAIT="0")
        e.update(env)
        return subprocess.run([DASH, str(SCRIPT), mode, tag], env=e, capture_output=True, text=True, timeout=60)

    def latest(self):
        p = self.state / "latest"
        return p.read_text().strip() if p.exists() else None

    def edits(self):
        p = self.state / "edits"
        return p.read_text().splitlines() if p.exists() else []

    def assertLeft(self, got, why, latest):
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertIn(why, got.stdout + got.stderr)
        self.assertEqual(self.edits(), [])
        self.assertEqual(self.latest(), latest)

    def assertFails(self, got, why, latest, edits=()):
        self.assertEqual(got.returncode, 1, got.stdout + got.stderr)
        self.assertIn(f"::error::{why}", got.stderr)
        self.assertEqual(self.edits(), list(edits))
        self.assertEqual(self.latest(), latest)

    # GitHub's Latest.

    def test_the_newest_release_published_leaves_it_where_github_put_it(self):
        self.given(["v1.0.3", "v1.1.0"], latest="v1.1.0")
        self.assertLeft(self.run_it("github", "v1.1.0"), "GitHub's Latest is v1.1.0 already", "v1.1.0")

    def test_a_patch_to_an_older_minor_puts_it_back(self):
        # Published without --latest=false, the patch took Latest.
        self.given(["v1.0.3", "v1.1.0", "v1.0.4"], latest="v1.0.4")
        got = self.run_it("github", "v1.0.4")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(self.edits(), ["latest to v1.1.0"])
        self.assertEqual(self.latest(), "v1.1.0")
        self.assertIn("::warning::v1.0.4 is older than the newest published release, v1.1.0", got.stdout)
        self.assertIn("GitHub's Latest is v1.1.0, moved from v1.0.4", self.summary.read_text())

    def test_a_patch_published_with_latest_false_moves_nothing(self):
        self.given(["v1.0.3", "v1.1.0", "v1.0.4"], latest="v1.1.0")
        self.assertLeft(self.run_it("github", "v1.0.4"), "GitHub's Latest is v1.1.0 already", "v1.1.0")

    def test_versions_compare_as_versions(self):
        # 1.10.0 is newer than 1.9.2, which a sort of the text would not say.
        self.given(["v1.9.1", "v1.10.0", "v1.9.2"], latest="v1.9.2")
        got = self.run_it("github", "v1.9.2")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(self.latest(), "v1.10.0")

    def test_no_latest_at_all_makes_this_one(self):
        self.given(["v1.0.0"])
        got = self.run_it("github", "v1.0.0")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(self.edits(), ["latest to v1.0.0"])
        self.assertIn("GitHub's Latest is v1.0.0, moved from none", got.stdout)

    def test_a_list_a_moment_behind_the_publish_moves_nothing_back(self):
        # The newest release, just published, is not in the list yet: read
        # without it, v1.0.3 would look the newest and take Latest from it.
        self.given(["v1.0.3", "v1.1.0"], latest="v1.1.0")
        (self.state / "unlisted").write_text("v1.1.0\n")
        self.assertLeft(self.run_it("github", "v1.1.0"), "GitHub's Latest is v1.1.0 already", "v1.1.0")

    def test_a_newer_draft_or_pre_release_holds_nothing_back(self):
        self.given(["v1.0.3", "v1.1.0", "v1.2.0 true false", "v1.3.0 false true"], latest="v1.0.3")
        got = self.run_it("github", "v1.1.0")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(self.latest(), "v1.1.0")

    def test_a_draft_or_a_pre_release_is_not_among_the_newest(self):
        # Latest is held on the newest published X.Y.Z all the same: here
        # it is there already.
        self.given(["v1.1.0", "v1.2.0 true false", "v1.2.0-rc.1 false true", "v1.3.0 false true"], latest="v1.1.0")
        self.assertLeft(self.run_it("github", "v1.2.0"), "v1.2.0 is not published yet", "v1.1.0")
        self.assertLeft(self.run_it("github", "v1.9.0"), "v1.9.0 is not published yet", "v1.1.0")
        self.assertLeft(self.run_it("github", "v1.3.0"), "v1.3.0 is a pre-release", "v1.1.0")
        self.assertLeft(self.run_it("github", "v1.2.0-rc.1"), "v1.2.0-rc.1 is a pre-release", "v1.1.0")
        self.assertLeft(self.run_it("github", "v1.1.0", EVENT_PRERELEASE="true"), "v1.1.0 is a pre-release",
                        "v1.1.0")

    def test_a_draft_or_a_pre_release_holding_latest_gives_it_back(self):
        # A dispatch for a draft's tag, or a pre-release's, finds Latest
        # where a publish took it, and puts it back all the same.
        for tag in ("v1.2.0", "v1.3.0", "v1.2.0-rc.1"):
            with self.subTest(tag=tag):
                self.fresh()
                self.given(["v1.1.0", "v1.2.0 true false", "v1.2.0-rc.1 false false", "v1.3.0 false true"],
                           latest="v1.2.0-rc.1")
                got = self.run_it("github", tag)
                self.assertEqual(got.returncode, 0, got.stderr)
                self.assertEqual(self.edits(), ["latest to v1.1.0"])
                self.assertEqual(self.latest(), "v1.1.0")

    # Release candidates: GitHub makes a candidate Latest when it is
    # published without `--prerelease`, and install.sh would take it.

    def test_a_candidate_before_its_release(self):
        # Published as a pre-release, it took nothing.
        self.given(["v0.1.12", "v0.2.0", "v1.0.0-rc.1 false true"], latest="v0.2.0")
        self.assertLeft(self.run_it("github", "v1.0.0-rc.1"), "GitHub's Latest is v0.2.0 already", "v0.2.0")
        # Published as a release, it took Latest, which goes back to 0.2.0.
        self.fresh()
        self.given(["v0.1.12", "v0.2.0", "v1.0.0-rc.1"], latest="v1.0.0-rc.1")
        got = self.run_it("github", "v1.0.0-rc.1")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(self.edits(), ["latest to v0.2.0"])
        self.assertEqual(self.latest(), "v0.2.0")
        self.assertIn("GitHub's Latest is v0.2.0, moved from v1.0.0-rc.1", got.stdout)

    def test_a_candidate_of_a_later_minor_while_an_earlier_one_is_newest(self):
        self.given(["v1.0.3", "v1.1.0-rc.1", "v1.1.0-rc.2 false true"], latest="v1.1.0-rc.1")
        got = self.run_it("github", "v1.1.0-rc.2")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(self.edits(), ["latest to v1.0.3"])
        self.assertEqual(self.latest(), "v1.0.3")

    def test_a_release_after_its_candidates(self):
        self.given(["v0.2.0", "v1.0.0-rc.1 false true", "v1.0.0-rc.2 false true", "v1.0.0"], latest="v1.0.0")
        self.assertLeft(self.run_it("github", "v1.0.0"), "GitHub's Latest is v1.0.0 already", "v1.0.0")
        # A list a moment behind the publish, and Latest left on 0.2.0 (a
        # publish with --latest=false by mistake): it goes to 1.0.0.
        self.fresh()
        self.given(["v0.2.0", "v1.0.0-rc.1 false true", "v1.0.0-rc.2 false true", "v1.0.0"], latest="v0.2.0")
        (self.state / "unlisted").write_text("v1.0.0\n")
        got = self.run_it("github", "v1.0.0")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(self.edits(), ["latest to v1.0.0"])

    def test_a_candidate_with_no_release_to_hold_latest_on(self):
        self.given(["v1.0.0-rc.1 false true"])
        self.assertLeft(self.run_it("github", "v1.0.0-rc.1"), "no X.Y.Z release is published, and none is Latest",
                        None)
        self.fresh()
        self.given(["v1.0.0-rc.1"], latest="v1.0.0-rc.1")
        self.assertFails(self.run_it("github", "v1.0.0-rc.1"), "GitHub's Latest is v1.0.0-rc.1, and no X.Y.Z"
                         " release is published to hold it on", "v1.0.0-rc.1")

    def test_a_tag_that_is_no_release_fails(self):
        self.given(["v1.0.0"], latest="v1.0.0")
        for tag in ("vfoo", "v1.0", "1.0.0", "v1.0.0-", "v1.0.0+build"):
            with self.subTest(tag=tag):
                self.assertFails(self.run_it("github", tag), f"{tag} is no release's tag", "v1.0.0")
                got = self.run_it("npm-tag", tag)
                self.assertEqual((got.returncode, got.stdout), (1, ""))

    # A run its publish started: the release is there, and a 404 is the API
    # behind it.

    def test_a_404_in_the_publishs_own_run_is_read_again(self):
        self.given(["v1.0.3", "v1.1.0", "v1.0.4"], latest="v1.0.4")
        (self.state / "tag-lag").write_text("2")
        got = self.run_it("github", "v1.0.4", GITHUB_EVENT_NAME="release")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertIn("read again in 0 s (2 of 6)", got.stderr)
        self.assertEqual(self.edits(), ["latest to v1.1.0"])
        self.fresh()
        self.given(["v1.0.3", "v1.1.0"])
        (self.state / "tag-lag").write_text("1")
        self.assertEqual(self.npm_tag("v1.1.0", GITHUB_EVENT_NAME="release"), "latest\n")

    def test_a_404_that_stays_in_the_publishs_own_run_fails(self):
        self.given(["v1.0.3", "v1.1.0"], latest="v1.1.0")
        (self.state / "tag-lag").write_text("9")
        self.assertFails(self.run_it("github", "v1.1.0", GITHUB_EVENT_NAME="release", API_LAG_TRIES="3"),
                         "v1.1.0's release is published -- this run is its publish's -- and the API answered its"
                         " tag with a 404 3 times, 0 s apart", "v1.1.0")
        (self.state / "tag-lag").write_text("9")
        got = self.run_it("npm-tag", "v1.1.0", GITHUB_EVENT_NAME="release", API_LAG_TRIES="3")
        self.assertEqual((got.returncode, got.stdout), (1, ""))
        # Started by a dispatch, the 404 is a draft's.
        (self.state / "tag-lag").write_text("9")
        self.assertLeft(self.run_it("github", "v1.1.0", GITHUB_EVENT_NAME="workflow_dispatch"),
                        "v1.1.0 is not published yet", "v1.1.0")

    def test_a_release_saying_neither_true_nor_false_fails(self):
        self.given(["v1.0.0", "v1.1.0 false null"], latest="v1.0.0")
        self.assertFails(self.run_it("github", "v1.1.0"), "v1.1.0's release says [null] for prerelease,"
                         " neither true nor false", "v1.0.0")

    def test_gh_failing_fails(self):
        # Only the API's 404 is an answer: gh out of reach, or any other
        # error, fails -- never green with Latest on an older release.
        def given():
            self.fresh()
            self.given(["v1.0.3", "v1.1.0", "v1.0.4"], latest="v1.0.4")

        cases = (
            ("1", None, "v1.0.4's release could not be read", NO_HOST, "v1.0.4", ()),
            ("2", None, "the release list could not be read", NO_HOST, "v1.0.4", ()),
            ("3", None, "GitHub's Latest could not be read", NO_HOST, "v1.0.4", ()),
            ("4", None, "v1.1.0 could not be made Latest", NO_HOST, "v1.0.4", ()),
            # The read-back, after the move.
            ("5", None, "GitHub's Latest could not be read", NO_HOST, "v1.1.0", ["latest to v1.1.0"]),
            # The list read again after the move.
            ("6", None, "the release list could not be read", NO_HOST, "v1.1.0", ["latest to v1.1.0"]),
            (None, "gh: API rate limit exceeded for installation ID 1. (HTTP 403)",
             "v1.0.4's release could not be read", "(HTTP 403)", "v1.0.4", ()),
            (None, "gh: HTTP 502", "v1.0.4's release could not be read", "HTTP 502", "v1.0.4", ()))
        for down_from, api_error, why, said, latest, edits in cases:
            with self.subTest(down_from=down_from, api_error=api_error):
                given()
                if down_from:
                    (self.state / "gh-down-from").write_text(down_from)
                if api_error:
                    (self.state / "api-error").write_text(api_error + "\n")
                got = self.run_it("github", "v1.0.4")
                self.assertFails(got, why, latest, edits)
                self.assertIn(said, got.stderr)
                self.assertNotIn("not published yet", got.stdout + got.stderr)

    def test_a_move_that_does_not_hold_fails(self):
        self.given(["v1.0.3", "v1.1.0", "v1.0.4"], latest="v1.0.4")
        (self.state / "broken-edit").write_text("")
        self.assertFails(self.run_it("github", "v1.0.4"), "GitHub's Latest is [v1.0.4] after the move, neither"
                         " v1.1.0 nor a newer published release", "v1.0.4", ["latest to v1.1.0"])

    def test_a_newer_release_published_meanwhile_gets_it_back(self):
        # v1.2.0 is published, and its own run done, after this run found
        # v1.1.0 the newest and before it moved Latest there.
        self.given(["v1.0.3", "v1.1.0", "v1.0.4"], latest="v1.0.4")
        self.meanwhile("after-list-1", 'echo "v1.2.0 false false" >>"$STATE/releases"',
                       'echo v1.2.0 >"$STATE/latest"')
        got = self.run_it("github", "v1.0.4")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(self.edits(), ["latest to v1.1.0", "latest to v1.2.0"])
        self.assertEqual(self.latest(), "v1.2.0")

    def test_a_newer_releases_move_between_the_edit_and_the_read_back_stands(self):
        self.given(["v1.0.3", "v1.1.0", "v1.0.4"], latest="v1.0.4")
        self.meanwhile("after-edit-1", 'echo "v1.2.0 false false" >>"$STATE/releases"',
                       'echo v1.2.0 >"$STATE/latest"')
        got = self.run_it("github", "v1.0.4")
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertIn("GitHub's Latest is v1.2.0: v1.2.0's own run moved it there after this one's move to"
                      " v1.1.0", got.stdout)
        self.assertIn("GitHub's Latest is v1.2.0 already", got.stdout)
        self.assertEqual(self.edits(), ["latest to v1.1.0"])

    def test_any_other_release_between_the_edit_and_the_read_back_fails(self):
        for whose, moved_to, also in (("an older release", "v1.0.3", ()),
                                      ("a release no list names", "v9.9.9", ())):
            with self.subTest(whose=whose):
                self.fresh()
                self.given(["v1.0.3", "v1.1.0", "v1.0.4"], latest="v1.0.4")
                self.meanwhile("after-edit-1", *also, f'echo {moved_to} >"$STATE/latest"')
                self.assertFails(self.run_it("github", "v1.0.4"), f"GitHub's Latest is [{moved_to}] after the"
                                 " move, neither v1.1.0 nor a newer published release", moved_to,
                                 ["latest to v1.1.0"])

    # npm's dist-tag.

    def npm_tag(self, tag, **env):
        got = self.run_it("npm-tag", tag, **env)
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(self.edits(), [])
        return got.stdout

    def test_the_newest_release_goes_out_under_latest(self):
        self.given(["v1.0.3", "v1.1.0"])
        self.assertEqual(self.npm_tag("v1.1.0"), "latest\n")

    def test_an_older_minors_patch_goes_out_under_its_line(self):
        self.given(["v1.0.3", "v1.1.0", "v1.0.4"])
        self.assertEqual(self.npm_tag("v1.0.4"), "release-1.0\n")
        self.given(["v1.9.1", "v1.10.0", "v1.9.2"])
        self.assertEqual(self.npm_tag("v1.9.2"), "release-1.9\n")
        # A dispatch for an old release of the newest minor's line.
        self.given(["v1.1.0", "v1.1.1"])
        self.assertEqual(self.npm_tag("v1.1.0"), "release-1.1\n")

    def test_a_pre_release_goes_out_under_next(self):
        self.given(["v1.0.3", "v1.1.0-rc.1 false true"])
        self.assertEqual(self.npm_tag("v1.1.0-rc.1"), "next\n")
        # The next one, which a list read just after its publish lacks.
        self.assertEqual(self.npm_tag("v1.1.0-rc.2"), "next\n")
        self.given(["v1.0.3", "v1.1.0 false true"])
        self.assertEqual(self.npm_tag("v1.1.0"), "next\n")
        self.given(["v1.0.3", "v1.1.0"])
        self.assertEqual(self.npm_tag("v1.1.0", EVENT_PRERELEASE="true"), "next\n")

    def test_next_follows_version_order_among_pre_releases(self):
        # 1.1.0-rc.1 after 2.0.0-rc.1 would move `next` back.
        self.given(["v1.0.3", "v2.0.0-rc.1 false true", "v1.1.0-rc.1 false true"])
        self.assertEqual(self.npm_tag("v1.1.0-rc.1"), "next-1.1\n")
        self.assertEqual(self.npm_tag("v2.0.0-rc.1"), "next\n")
        # rc.10 is newer than rc.2, as a sort of the text would not say.
        self.given(["v1.0.0-rc.2 false true", "v1.0.0-rc.10 false true"])
        self.assertEqual(self.npm_tag("v1.0.0-rc.10"), "next\n")
        self.assertEqual(self.npm_tag("v1.0.0-rc.2"), "next-1.0\n")
        # X.Y.Z published as a pre-release is newer than its own candidates,
        # as semver says and sort -V does not.
        self.given(["v1.0.0-rc.2 false true", "v1.0.0 false true"])
        self.assertEqual(self.npm_tag("v1.0.0"), "next\n")
        self.assertEqual(self.npm_tag("v1.0.0-rc.2"), "next-1.0\n")
        # A draft or a release is no pre-release `next` is held to.
        self.given(["v1.0.0-rc.1 false true", "v1.0.0-rc.3 true true", "v1.0.0"])
        self.assertEqual(self.npm_tag("v1.0.0-rc.2"), "next\n")

    def test_a_list_a_moment_behind_and_a_newer_draft_or_pre_release_leave_latest(self):
        self.given(["v1.0.3", "v1.1.0", "v1.2.0 true false", "v1.3.0 false true"])
        (self.state / "unlisted").write_text("v1.1.0\n")
        self.assertEqual(self.npm_tag("v1.1.0"), "latest\n")

    def test_a_drafts_tag_is_judged_by_its_version(self):
        self.given(["v1.0.3", "v1.1.0 true false"])
        self.assertEqual(self.npm_tag("v1.1.0"), "latest\n")
        self.given(["v1.1.0", "v1.0.4 true false"])
        self.assertEqual(self.npm_tag("v1.0.4"), "release-1.0\n")

    def test_npm_tag_failing_prints_no_tag(self):
        for down_from in ("1", "2"):
            with self.subTest(down_from=down_from):
                self.fresh()
                self.given(["v1.0.3", "v1.1.0"])
                (self.state / "gh-down-from").write_text(down_from)
                got = self.run_it("npm-tag", "v1.1.0")
                self.assertEqual(got.returncode, 1, got.stderr)
                self.assertEqual(got.stdout, "")
                self.assertIn(NO_HOST, got.stderr)

    def test_the_usage(self):
        for args in ([], ["github"], ["latest", "v1.0.0"]):
            got = subprocess.run([DASH, str(SCRIPT), *args], capture_output=True, text=True, timeout=60)
            self.assertEqual(got.returncode, 2)
            self.assertIn("tools/release-latest.sh github TAG", got.stderr)


if __name__ == "__main__":
    unittest.main()

#!/bin/sh
# The pointers that mean "the newest release", kept there when a release
# that is not the newest goes out -- a patch to the previous minor:
#
#   tools/release-latest.sh github TAG
#   tools/release-latest.sh npm-tag TAG
#
# TAG is the release's (vX.Y.Z). `github` holds GitHub's Latest, which
# site/install.sh and site/install.ps1 read (`releases/latest/download/`),
# on the newest published release: GitHub makes every release it publishes
# Latest, whatever its version, unless it is published with
# `--latest=false`, so packages.yml's `github-latest` runs this once a
# release is published and puts Latest back where a patch to an older minor
# took it. Nothing is moved, each saying why and ending well, for a TAG that
# is no X.Y.Z, a release not published yet (the API answers a draft by its
# tag with a 404 whatever the token), or a pre-release, which GitHub makes
# no Latest. A move is read back and held, or to a newer published
# release, which that release's own run moved it to between the move and
# the read-back; then the release list is read again, and a newer release
# published meanwhile, whose own run may have run before this one's move,
# gets it back.
#
# `npm-tag` prints the dist-tag packages.yml's `npm` publishes TAG's
# packages under: `latest` for the newest published release, `next` for a
# pre-release (as its version, the release or EVENT_PRERELEASE says), and
# `release-X.Y` for an older X.Y.Z, so that npm's `latest`, which `npm
# install` takes, never moves back. Not `vX.Y`: npm refuses a dist-tag that
# reads as a semver range, and `v1.4` reads as >=1.4.0 <1.5.0. A dispatch
# for a draft's tag is judged by its version.
#
# "The newest" as tools/image-latest.sh decides it: `sort -V` over the
# published releases' X.Y.Z, drafts and pre-releases left out, and TAG's
# own, which a list read just after the publish may not hold yet. Any error
# of gh but the API's 404 fails, never taken for an answer.
#
# GH_TOKEN and GITHUB_REPOSITORY as a workflow sets them; `github` writes
# with `gh release edit --latest`, which wants `contents: write`. POSIX sh
# has no pipefail: a command whose failure matters runs in a substitution of
# its own, never at a pipeline's head.

set -eu

usage() {
    sed -n '2,/^$/s/^# \{0,1\}//p' "$0" >&2
    exit 2
}
[ $# -eq 2 ] || usage
case $1 in
    github | npm-tag) ;;
    *) usage ;;
esac
mode=$1
tag=$2
version=${2#v}
repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY names the repository}

err=$(mktemp "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/release-latest.XXXXXX")
trap 'rm -f "$err"' EXIT

# What this says goes to stderr in npm-tag, whose stdout is the dist-tag.
says() {
    if [ "$mode" = npm-tag ]; then echo "$*" >&2; else echo "$*"; fi
    if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then echo "$*" >>"$GITHUB_STEP_SUMMARY"; fi
}

fails() {
    echo "::error::$*" >&2
    if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then echo "$*" >>"$GITHUB_STEP_SUMMARY"; fi
    exit 1
}

# What the last command to fail said, on one line.
said() {
    tr '\n' ' ' <"$err"
}

# The published releases' versions, X.Y.Z alone, ascending, into
# `published`, and the newest of them into `newest`, version $1's among
# them.
releases() {
    if ! tags=$(gh release list -R "$repo" --exclude-drafts --exclude-pre-releases -L 1000 --json tagName -q '.[].tagName' 2>"$err"); then
        fails "the release list could not be read: $(said)"
    fi
    published=$(printf '%s\nv%s\n' "$tags" "$1" | sed -n 's/^v\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)$/\1/p' | sort -V)
    newest=$(printf '%s\n' "$published" | tail -n 1)
}

# Whether the release of TAG is a pre-release, into `prerelease`: true or
# false as the release says, `draft` where the API answers its tag with a
# 404 -- a draft, or no release -- and true where EVENT_PRERELEASE says so.
# gh writes the API's error body to stdout and its status to stderr.
release_kind() {
    if ! prerelease=$(gh api "repos/$repo/releases/tags/$tag" --jq .prerelease 2>"$err"); then
        grep -q 'HTTP 404' "$err" || fails "$tag's release could not be read: $(said)"
        prerelease=draft
    fi
    [ "${EVENT_PRERELEASE:-}" != true ] || prerelease=true
    case $prerelease in
        true | false | draft) ;;
        *) fails "$tag's release says [$prerelease] for prerelease, neither true nor false" ;;
    esac
}

# GitHub's Latest, its tag into `latest`: empty where there is none (the
# API's 404).
latest_tag() {
    if ! latest=$(gh api "repos/$repo/releases/latest" --jq .tag_name 2>"$err"); then
        grep -q 'HTTP 404' "$err" || fails "GitHub's Latest could not be read: $(said)"
        latest=
    fi
}

# Latest on v$1, read back and held there, or on a newer published
# release, whose own run moved it there between this move and the read-back.
hold() {
    latest_tag
    if [ "$latest" = "v$1" ]; then
        says "GitHub's Latest is v$1 already"
        return 0
    fi
    was=${latest:-none}
    gh release edit "v$1" -R "$repo" --latest >/dev/null 2>"$err" || fails "v$1 could not be made Latest: $(said)"
    latest_tag
    if [ "$latest" = "v$1" ]; then
        says "GitHub's Latest is v$1, moved from $was"
        return 0
    fi
    releases "$1"
    newer=$(printf '%s\n' "$published" | awk -v v="$1" '$0 == v { seen = 1; next } seen')
    for v in $newer; do
        if [ "$latest" = "v$v" ]; then
            says "GitHub's Latest is v$v: v$v's own run moved it there after this one's move to v$1"
            return 0
        fi
    done
    fails "GitHub's Latest is [$latest] after the move, neither v$1 nor a newer published release"
}

if [ "$mode" = npm-tag ]; then
    if ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
        says "$tag is no X.Y.Z: its packages go out under next, and npm's latest stays"
        echo next
        exit 0
    fi
    release_kind
    if [ "$prerelease" = true ]; then
        says "$tag is a pre-release: its packages go out under next, and npm's latest stays"
        echo next
        exit 0
    fi
    releases "$version"
    if [ "$newest" = "$version" ]; then
        says "$tag is the newest release: its packages go out under latest"
        echo latest
    else
        says "$tag is older than the newest published release, $newest: its packages go out under release-${version%.*}, and npm's latest stays"
        echo "release-${version%.*}"
    fi
    exit 0
fi

if ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    says "$tag is no X.Y.Z, and GitHub makes no pre-release Latest: nothing to hold"
    exit 0
fi
release_kind
case $prerelease in
    draft)
        says "$tag is not published yet: GitHub's Latest is held once it is"
        exit 0
        ;;
    true)
        says "$tag is a pre-release, which GitHub makes no Latest: nothing to hold"
        exit 0
        ;;
esac

releases "$version"
if [ "$newest" != "$version" ]; then
    echo "::warning::$tag is older than the newest published release, v$newest: GitHub's Latest is held on v$newest (publish such a release with --latest=false, and Latest does not move at all)"
fi
# `hold` reads the release list itself, so the version it is handed is kept
# apart.
held=$newest
hold "$held"

# A newer release published while this run held Latest, whose own run may
# have run before this one's move: Latest goes on to it.
while :; do
    releases "$held"
    [ "$newest" != "$held" ] || break
    next=$newest
    hold "$next"
    held=$next
done

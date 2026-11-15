#!/bin/sh
# The pointers that mean "the newest release", kept there whatever goes out
# -- a patch to the previous minor, a release candidate:
#
#   tools/release-latest.sh github TAG
#   tools/release-latest.sh npm-tag TAG
#
# TAG is the release's: vX.Y.Z, or vX.Y.Z-rc.N for a candidate, N from 1,
# the one form release.yml's jobs take (tools/test_release_verify.py).
# `github` holds GitHub's Latest, which site/install.sh and site/install.ps1
# read (`releases/latest/download/`), on the newest published X.Y.Z release,
# whatever TAG it was run for: GitHub makes every release it publishes
# Latest, whatever its version, unless it is published with
# `--latest=false` or as a pre-release, so packages.yml's `github-latest`
# runs this once a release is published and puts Latest back where a patch
# to an older minor, or a candidate published without `--prerelease`, took
# it. TAG's own version is among the published ones where its release is
# published and no pre-release; a draft (the API answers a draft by its tag
# with a 404 whatever the token), a pre-release or a TAG that is no X.Y.Z
# is not, and Latest is held on the newest of the others all the same. A
# move is read back and held, or to a newer published release, which that
# release's own run moved it to between the move and the read-back; then
# the release list is read again, and a newer release published meanwhile,
# whose own run may have run before this one's move, gets it back.
#
# `npm-tag` prints the dist-tag packages.yml's `npm` publishes TAG's
# packages under: `latest` for the newest published release, `release-X.Y`
# for an older X.Y.Z, so that npm's `latest`, which `npm install` takes,
# never moves back; `next` for the newest pre-release (as its version, the
# release or EVENT_PRERELEASE says), by version among the published
# pre-releases and TAG's own, and `next-X.Y` for an older one, so that
# `next` never moves back either (1.1.0-rc.1 after 2.0.0-rc.1). Not `vX.Y`:
# npm refuses a dist-tag that reads as a semver range, and `v1.4` reads as
# >=1.4.0 <1.5.0. A dispatch for a draft's tag is judged by its version. A
# package's first publish ever is `latest` whatever this names: the
# registry gives a package with no `latest` that version.
#
# "The newest" as tools/image-latest.sh decides it: `sort -V` over the
# published releases' X.Y.Z, drafts and pre-releases left out, and TAG's
# own, which a list read just after the publish may not hold yet; among
# pre-releases, the same with X.Y.Z after its own candidates, as semver
# orders them. Any error of gh but the API's 404 fails, never taken for an
# answer, and so does that 404 where GITHUB_EVENT_NAME is `release` -- this
# run is TAG's publish's own, so the release is there, and the 404 is the
# API a moment behind -- once API_LAG_TRIES reads API_LAG_WAIT seconds
# apart (6 and 10) have all had it.
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
tries=${API_LAG_TRIES:-6}
wait=${API_LAG_WAIT:-10}

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

# Whether $1 is X.Y.Z alone, or X.Y.Z and a candidate's -rc.N. grep
# matches line by line: a value of two lines, each a version, is none.
nl='
'
release_version() {
    case $1 in *"$nl"*) return 1 ;; esac
    printf '%s\n' "$1" | grep -Eqx '[0-9]+\.[0-9]+\.[0-9]+'
}
pre_version() {
    case $1 in *"$nl"*) return 1 ;; esac
    printf '%s\n' "$1" | grep -Eqx '[0-9]+\.[0-9]+\.[0-9]+-rc\.[1-9][0-9]*'
}

# The published releases' versions, X.Y.Z alone, ascending, into
# `published`, and the newest of them into `newest`, version $1's among
# them where it is one (an empty $1 adds none).
releases() {
    if ! tags=$(gh release list -R "$repo" --exclude-drafts --exclude-pre-releases -L 1000 --json tagName -q '.[].tagName' 2>"$err"); then
        fails "the release list could not be read: $(said)"
    fi
    published=$(printf '%s\nv%s\n' "$tags" "$1" | sed -n 's/^v\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)$/\1/p' | sort -V)
    newest=$(printf '%s\n' "$published" | tail -n 1)
}

# The newest of the published pre-releases' versions and $1, into
# `newest_pre`. sort -V puts X.Y.Z before X.Y.Z-rc.1, which semver puts
# after: each is sorted by a key that puts a version with no pre-release
# after its own candidates (X.Y.Z.1 against X.Y.Z.0.rc.1), rc.2 before
# rc.10 still.
prereleases() {
    if ! tags=$(gh release list -R "$repo" --exclude-drafts -L 1000 --json tagName,isPrerelease -q '.[] | select(.isPrerelease) | .tagName' 2>"$err"); then
        fails "the release list could not be read: $(said)"
    fi
    newest_pre=$(printf '%s\nv%s\n' "$tags" "$1" |
        sed -n 's/^v\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\(-rc\.[1-9][0-9]*\)\{0,1\}\)$/\1/p' |
        awk '{ i = index($0, "-"); print (i ? substr($0, 1, i - 1) ".0." substr($0, i + 1) : $0 ".1") " " $0 }' |
        sort -V | tail -n 1 | cut -d' ' -f2)
}

# Whether the release of TAG is a pre-release, into `prerelease`: true or
# false as the release says, `draft` where the API answers its tag with a
# 404 -- a draft, or no release -- and true where EVENT_PRERELEASE says so.
# In a run its publish started (GITHUB_EVENT_NAME `release`) the release is
# published, and a 404 is the API behind it: read again, a bounded number of
# times, then failed. gh writes the API's error body to stdout and its
# status to stderr.
release_kind() {
    n=1
    while ! prerelease=$(gh api "repos/$repo/releases/tags/$tag" --jq .prerelease 2>"$err"); do
        grep -q 'HTTP 404' "$err" || fails "$tag's release could not be read: $(said)"
        if [ "${GITHUB_EVENT_NAME:-}" != release ]; then
            prerelease=draft
            break
        fi
        [ "$n" -lt "$tries" ] ||
            fails "$tag's release is published -- this run is its publish's -- and the API answered its tag with a 404 $tries times, $wait s apart"
        echo "$tag's release is published, and the API answers its tag with a 404 still: read again in $wait s ($n of $tries)" >&2
        sleep "$wait"
        n=$((n + 1))
    done
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

if [ "$tag" = "$version" ] || { ! release_version "$version" && ! pre_version "$version"; }; then
    fails "$tag is no release's tag: vX.Y.Z, or vX.Y.Z-rc.N for a candidate, N from 1"
fi

if [ "$mode" = npm-tag ]; then
    if release_version "$version"; then
        release_kind
        if [ "$prerelease" != true ]; then
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
    fi
    prereleases "$version"
    core=${version%%-*}
    if [ "$newest_pre" = "$version" ]; then
        says "$tag is the newest pre-release: its packages go out under next, and npm's latest stays"
        echo next
    else
        says "$tag is older than the newest pre-release, $newest_pre: its packages go out under next-${core%.*}, and npm's next and latest stay"
        echo "next-${core%.*}"
    fi
    exit 0
fi

# GitHub's Latest, on the newest published X.Y.Z whatever TAG is. TAG's own
# version joins the list, which a list read just after the publish may not
# hold yet, where its release is published and no pre-release.
own=
if release_version "$version"; then
    release_kind
    case $prerelease in
        false) own=$version ;;
        draft) says "$tag is not published yet: GitHub's Latest is held on the newest published release, which it is not among" ;;
        true) says "$tag is a pre-release, which GitHub makes no Latest: it is held on the newest published release" ;;
    esac
else
    says "$tag is a pre-release, which GitHub makes no Latest unless it is published as a release: it is held on the newest published X.Y.Z release"
fi

releases "$own"
if [ -z "$newest" ]; then
    latest_tag
    [ -n "$latest" ] || { says "no X.Y.Z release is published, and none is Latest: nothing to hold"; exit 0; }
    fails "GitHub's Latest is $latest, and no X.Y.Z release is published to hold it on"
fi
if [ -n "$own" ] && [ "$newest" != "$own" ]; then
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

#!/bin/sh
# `latest` on the registry, moved to a release's image where that release is
# the newest published one:
#
#   tools/image-latest.sh IMAGE TAG
#
# IMAGE is ghcr.io/<owner>/fennec-server and TAG the release's (vX.Y.Z).
# Two jobs run this, and whichever runs second moves it: packages.yml's
# `image-latest`, once the release is published, and release.yml's
# `image-latest`, once its `manifest` has made `<version>`, the list
# `latest` copies. A release published before that -- its image jobs
# failed -- finds no `<version>`: this fails saying so, and the failed jobs
# of release.yml's run for TAG, re-run, move `latest` once they have made
# the list.
#
# The release is read by its tag through the API, which answers a published
# release alone, and a draft with a 404 whatever the token. Both jobs' tokens
# read alone, and see no draft: `gh release view` looks a draft up in the
# release list, where they find none, and said "release not found" at every
# tag's push, failing release.yml's job. Nothing is moved, each saying why
# and ending well, for a TAG that is no X.Y.Z, a release not published yet
# (that 404: release.yml's run at the tag, whose release is `publish`'s
# draft), a pre-release (as the release says, or EVENT_PRERELEASE where a
# release event started the run), or a release older than the newest
# published one: `sort -V` over the release list and TAG itself, which a
# list read just after the publish may not hold yet. Any other error fails:
# the API's but that 404, the registry's but a tag's not found.
#
# A move copies the list (`imagetools create` from one list writes its
# bytes again) and holds `latest` to its digest, or to a newer published
# release's list, which that release's own run moved it to between this
# create and its read-back. Then it reads the release list again: a newer
# release published meanwhile, whose own run may have moved `latest` before
# this one's move, gets it back, so two versions' runs, which may run at
# once, leave it on the newest.
#
# GH_TOKEN and GITHUB_REPOSITORY as a workflow sets them, and docker logged
# in to IMAGE's registry. POSIX sh has no pipefail: a command whose failure
# matters runs in a substitution of its own, never at a pipeline's head.

set -eu

[ $# -eq 2 ] || { sed -n '2,/^$/s/^# \{0,1\}//p' "$0" >&2; exit 2; }
image=$1
version=${2#v}
repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY names the repository}

err=$(mktemp "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/image-latest.XXXXXX")
trap 'rm -f "$err"' EXIT

says() {
    echo "$*"
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
# `published`, and the newest of them into `newest`. TAG's among them: a
# list read just after the publish may not hold it yet, and would leave
# `latest` behind, green.
releases() {
    if ! tags=$(gh release list -R "$repo" --exclude-drafts --exclude-pre-releases -L 1000 --json tagName -q '.[].tagName' 2>"$err"); then
        fails "the release list could not be read: $(said)"
    fi
    published=$(printf '%s\nv%s\n' "$tags" "$1" | sed -n 's/^v\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)$/\1/p' | sort -V)
    newest=$(printf '%s\n' "$published" | tail -n 1)
}

# A tag's list as the registry holds it: its digest into `digest`, empty
# where the registry says it has no such tag (`<ref>: not found`, in $err).
# Any other error -- the registry out of reach, a token refused -- says
# nothing of the tag, and fails: taken for no list, it ended a run green
# with `latest` left behind.
digest_of() {
    digest=
    if manifest=$(docker buildx imagetools inspect --format '{{json .Manifest}}' "$1" 2>"$err"); then
        digest=$(printf '%s' "$manifest" | jq -r '.digest // empty')
        [ -n "$digest" ] || fails "$1's list, as the registry gave it, names no digest"
    elif ! grep -qF "$1: not found" "$err"; then
        fails "$1 could not be read from the registry: $(said)"
    fi
}

# `latest` moved to version $1, whose list's digest is $2, and held to it,
# or to a newer published release's list: that release's own run, which
# may run at once, moved it there between this create and the read-back,
# and its list is the one `latest` should name. Any other list fails.
move() {
    digest_of "$image:latest"
    if [ "$digest" = "$2" ]; then
        says "$image:latest is $1 already, $2"
        return 0
    fi
    docker buildx imagetools create -t "$image:latest" "$image:$1"
    digest_of "$image:latest"
    if [ "$digest" = "$2" ]; then
        says "$image:latest is $1, $2"
        return 0
    fi
    held=$digest
    if [ -n "$held" ]; then
        releases "$1"
        newer=$(printf '%s\n' "$published" | awk -v v="$1" '$0 == v { seen = 1; next } seen')
        for v in $newer; do
            digest_of "$image:$v"
            if [ "$digest" = "$held" ]; then
                says "$image:latest is $v, $held: v$v's own run moved it there after this one's move to $1"
                return 0
            fi
        done
    fi
    fails "$image:latest is [$held] after the move, neither $1's list $2 nor a newer published release's"
}

if ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    says "latest names releases alone, and $2 is no X.Y.Z: it is not moved"
    exit 0
fi

# The release by its tag: a published one alone, a draft a 404. gh writes
# the API's error body to stdout and its status to stderr.
if ! prerelease=$(gh api "repos/$repo/releases/tags/v$version" --jq .prerelease 2>"$err"); then
    if grep -q 'HTTP 404' "$err"; then
        says "v$version is not published yet: packages.yml moves latest when it is"
        exit 0
    fi
    fails "v$version's release could not be read: $(said)"
fi
[ "${EVENT_PRERELEASE:-}" != true ] || prerelease=true
case $prerelease in
    false) ;;
    true)
        says "v$version is a pre-release: latest names published releases alone, and is not moved"
        exit 0
        ;;
    *)
        fails "v$version's release says [$prerelease] for prerelease, neither true nor false"
        ;;
esac

releases "$version"
if [ "$newest" != "$version" ]; then
    echo "::warning::v$version is older than the newest published release, $newest: latest is not moved back"
    says "latest is left on the newest published release, $newest, not moved back to $version"
    exit 0
fi

digest_of "$image:$version"
if [ -z "$digest" ]; then
    fails "$image:$version, the list latest copies, is not on the registry yet ($(said)): release.yml's image (manifest) makes it, and its image (latest) then moves latest. Where the release was published before image (manifest) finished, nothing needs re-running; where release.yml's image jobs for v$version failed, re-run that run's failed jobs (RELEASING.md, step 4), not the whole workflow, which would build the arch tags again and push them under new digests"
fi
move "$version" "$digest"

# A newer release published while this run moved `latest`, whose own run
# may have moved it before this one's move: it goes on to that one. `move`
# reads the release list itself, so the version it is handed is kept apart.
moved=$version
while :; do
    releases "$moved"
    [ "$newest" != "$moved" ] || break
    next=$newest
    digest_of "$image:$next"
    if [ -z "$digest" ]; then
        says "v$next, published since, has no list on the registry yet: latest stays on $moved until release.yml's image (latest) for v$next moves it"
        break
    fi
    move "$next" "$digest"
    moved=$next
done

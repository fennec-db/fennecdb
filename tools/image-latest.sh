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
# failed -- finds no `<version>`: this fails saying so, and release.yml run
# again moves `latest` once it has made the list.
#
# Nothing is moved, each saying why and ending well, for a TAG that is no
# X.Y.Z, a draft (release.yml's run at the tag), a pre-release (as the
# release says, or EVENT_PRERELEASE where a release event started the
# run), or a release older than the newest published one: `sort -V` over
# the release list and TAG itself, which a list read just after the publish
# may not hold yet. A move copies the list (`imagetools create` from one
# list writes its bytes again), holds `latest` to its digest, and reads the
# release list again: a newer release published meanwhile, whose own run
# may have moved `latest` before this one's move, gets it back, so two
# versions' runs, which may run at once, leave it on the newest.
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

# The newest published release's version, TAG's among them: a list read
# just after the publish may not hold it yet, and would leave `latest`
# behind, green. Into `newest`.
newest() {
    tags=$(gh release list -R "$repo" --exclude-drafts --exclude-pre-releases -L 1000 --json tagName -q '.[].tagName')
    newest=$(printf '%s\nv%s\n' "$tags" "$1" | sed -n 's/^v\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)$/\1/p' | sort -V | tail -n 1)
}

# A tag's list as the registry holds it: its digest into `digest`, empty
# where the registry has none, its answer in $err.
digest_of() {
    digest=
    if manifest=$(docker buildx imagetools inspect --format '{{json .Manifest}}' "$1" 2>"$err"); then
        digest=$(printf '%s' "$manifest" | jq -r '.digest // empty')
    fi
}

# `latest` moved to version $1, whose list's digest is $2, and held to it.
move() {
    digest_of "$image:latest"
    if [ "$digest" = "$2" ]; then
        says "$image:latest is $1 already, $2"
        return 0
    fi
    docker buildx imagetools create -t "$image:latest" "$image:$1"
    digest_of "$image:latest"
    [ "$digest" = "$2" ] || fails "$image:latest is [$digest] after the move, not $1's list $2"
    says "$image:latest is $1, $2"
}

if ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    says "latest names releases alone, and $2 is no X.Y.Z: it is not moved"
    exit 0
fi

state=$(gh release view "v$version" -R "$repo" --json isDraft,isPrerelease -q '"\(.isDraft) \(.isPrerelease)"')
[ "${EVENT_PRERELEASE:-}" != true ] || state="${state% *} true"
case $state in
    "false false") ;;
    "true "*)
        says "v$version is a draft: latest moves once it is published (packages.yml's image-latest)"
        exit 0
        ;;
    *)
        says "v$version is a pre-release: latest names published releases alone, and is not moved"
        exit 0
        ;;
esac

newest "$version"
if [ "$newest" != "$version" ]; then
    echo "::warning::v$version is older than the newest published release, $newest: latest is not moved back"
    says "latest is left on the newest published release, $newest, not moved back to $version"
    exit 0
fi

digest_of "$image:$version"
if [ -z "$digest" ]; then
    fails "$image:$version, the list latest copies, is not on the registry yet ($(tr '\n' ' ' <"$err")): release.yml's image (manifest) makes it, and its image (latest) then moves latest; run release.yml again for v$version once its image jobs pass (RELEASING.md, step 4)"
fi
move "$version" "$digest"

# A newer release published while this run moved `latest`, whose own run
# may have moved it before this one's move: it goes on to that one.
moved=$version
while :; do
    newest "$moved"
    [ "$newest" != "$moved" ] || break
    digest_of "$image:$newest"
    if [ -z "$digest" ]; then
        says "v$newest, published since, has no list on the registry yet: latest stays on $moved until release.yml's image (latest) for v$newest moves it"
        break
    fi
    move "$newest" "$digest"
    moved=$newest
done

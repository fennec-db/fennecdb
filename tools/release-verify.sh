#!/bin/sh
# A release held to what its workflows publish and attest, RELEASING.md's
# step 6, once the app's downloads are attached:
#
#   tools/release-verify.sh TAG
#   tools/release-verify.sh --assets TAG
#
# TAG is vX.Y.Z, or vX.Y.Z-rc.N for a candidate, N from 1: the one form
# release.yml's and swift-binary.yml's jobs take in their first step,
# tools/test_release_verify.py holding them to it. The release's
# assets are exactly those `release.yml` and `desktop.yml` attach (`--assets`
# prints them, the workflow that attested each first): `release.yml`'s 17 --
# its own 14 and swift-binary.yml's three XCFramework files -- and
# SHA256SUMS, and for an X.Y.Z the app's 5, which desktop.yml attaches once
# the release is published. A candidate has no app files: desktop.yml takes
# X.Y.Z alone (`release is a tag such as v0.2.0`), so its 18 are all. A
# candidate is a pre-release, which GitHub makes no Latest, and an X.Y.Z is
# none; Latest is an X.Y.Z. SHA256SUMS is what binds a file to its name in
# this release, which a file's attestation does not (it names the bytes):
# release.yml's `publish` writes it and attests it in the tag's own run,
# and desktop.yml's attach jobs append the app's lines after the publish,
# every line before them kept byte for byte. So its lines before the app's
# must verify as release.yml's at refs/tags/TAG before a sum is read from
# them; then it lists every asset but itself and the XCFramework's
# `.checksum`, each with its sum, the app's by lines no run attested, each
# app file held by its own attestation. Each asset verifies with `gh
# attestation verify` as built by the workflow that made it, at the one ref
# its run may have been on, on GitHub's runners:
#
#   release.yml        refs/tags/TAG       the tag's own run (its push, or a
#                                          dispatch at the tag)
#   swift-binary.yml   refs/heads/main for an X.Y.0 and its candidates, and
#                      refs/heads/release/X.Y for a patch (Z above 0) and
#                      its: built before the tag. Never both: a `release/*`
#                      branch is pushed by a writer, and its run of an
#                      edited swift-binary.yml attests what it is given.
#   desktop.yml        refs/heads/main     dispatched from main alone
#
# and so does the image, each of `<version>` (the list), `<version>-amd64`
# and `<version>-arm64`, by `oci://` and the digest ghcr.io serves for it,
# which the verified statement must name. Once published: npm's four
# packages each carry SLSA provenance (`dist.attestations`) naming
# packages.yml of this repository, and `npm audit signatures` passes over
# them -- it checks the registry's signatures and the attestations a package
# has, and passes one that has none, so the first check is what finds a
# package published without provenance -- and each of PyPI's files has PEP
# 740 attestations whose publisher is packages.yml of this repository.
#
# One line an asset, and the first thing missing or wrong ends it, non-zero,
# its last lines naming every check it did not make. gh logged in, npm, curl and jq; REPO and IMAGE name another repository and
# image (tests). POSIX sh, under dash as the other release scripts: a
# command whose failure matters runs in a substitution of its own, never at
# a pipeline's head.

set -eu

usage() {
    sed -n '2,/^$/s/^# \{0,1\}//p' "$0" >&2
    exit 2
}
list_only=
if [ $# -eq 2 ] && [ "$1" = --assets ]; then
    list_only=1
    shift
fi
[ $# -eq 1 ] || usage
case $1 in -*) usage ;; esac
tag=$1
version=${tag#v}
repo=${REPO:-fennec-db/fennecdb}
image=${IMAGE:-ghcr.io/fennec-db/fennec-server}

# Every check, in the order they are made: each, as it begins, takes
# itself and those before it off `left`, and a failure names what is left,
# then `pending`, what the check under way had not reached.
left="the release's state
GitHub's Latest
the asset list
SHA256SUMS's attestation
SHA256SUMS's sums
each asset's attestation
the image's three tags
npm's provenance
npm audit signatures
PyPI's attestations"
pending=
# The words after $1 among the rest, comma-separated: what a loop had not
# reached.
after() {
    w=$1
    shift
    printf '%s\n' "$@" | awk -v w="$w" 'f { printf "%s%s", s, $0; s = ", " } $0 == w { f = 1 }'
}
doing() {
    left=$(printf '%s\n' "$left" | awk -v c="$1" 'f; $0 == c { f = 1 }')
    pending=
}

fails() {
    echo "FAIL $*" >&2
    {
        [ -z "$pending" ] || echo "not checked: $pending"
        [ -z "$left" ] || printf '%s\n' "$left" | sed 's/^/not checked: /'
    } >&2
    exit 1
}

ok() {
    echo "ok   $*"
}

# Whether $2 is wholly the extended regular expression $1. grep matches
# line by line: a value of two lines, one of them right, is none.
nl='
'
whole() {
    case $2 in *"$nl"*) return 1 ;; esac
    printf '%s\n' "$2" | grep -Eqx "$1"
}
whole 'v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-rc\.[1-9][0-9]*)?' "$tag" || fails "$tag is no release's tag: vX.Y.Z, or vX.Y.Z-rc.N for a candidate, N from 1"
case $version in
    *-*) candidate=yes ;;
    *) candidate=no ;;
esac
core=${version%%-*}
# The one ref swift-binary.yml's run may be on: main for an X.Y.0 and its
# candidates, release/X.Y for a patch and its.
if [ "${core##*.}" -eq 0 ]; then
    swift_ref=refs/heads/main
else
    swift_ref=refs/heads/release/${core%.*}
fi

# The assets of TAG, each after the workflow whose run attests it:
# `release`, `swift` (swift-binary.yml's, before the tag), `desktop`, or
# `sums` for SHA256SUMS, whose lines before the app's release.yml attests. $1 is `yes` where the app's are
# among them. tools/test_release_verify.py holds this list to the
# workflows and to the scripts that name the files.
assets() {
    for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl aarch64-apple-darwin x86_64-apple-darwin; do
        echo "release fennec-v$version-$t.tar.gz"
    done
    for t in x86_64-pc-windows-msvc aarch64-pc-windows-msvc; do
        echo "release fennec-v$version-$t.zip"
    done
    for t in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
        echo "release fennec-ffi-v$version-$t.tar.gz"
    done
    echo "release fennec-web-v$version.tar.gz"
    echo "release fennecdb-android-$version.aar"
    echo "release fennecdb_$version-1_amd64.deb"
    echo "release fennecdb_$version-1_arm64.deb"
    echo "release fennecdb-$version-1.x86_64.rpm"
    echo "release fennecdb-$version-1.aarch64.rpm"
    echo "swift FennecFFI.xcframework.zip"
    echo "swift FennecFFI.xcframework.zip.checksum"
    echo "swift FennecFFIDynamic.xcframework.zip"
    echo "sums SHA256SUMS"
    if [ "$1" = yes ]; then
        for a in macos-arm64.dmg windows-x64.zip windows-arm64.zip linux-x64.tar.xz linux-arm64.tar.xz; do
            echo "desktop fennec-studio-v$version-$a"
        done
    fi
}

if [ -n "$list_only" ]; then
    app=yes
    [ "$candidate" = no ] || app=no
    assets "$app"
    exit 0
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/release-verify.XXXXXX")
trap 'rm -rf "$work"' EXIT
err=$work/err

# What the last command to fail said, on one line.
said() {
    tr '\n' ' ' <"$err"
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# The release: published or a draft, a pre-release or not, its assets.
doing "the release's state"
release=$(gh release view "$tag" -R "$repo" --json isDraft,isPrerelease,assets 2>"$err") ||
    fails "$tag: the release could not be read: $(said)"
draft=$(printf '%s' "$release" | jq -r .isDraft)
prerelease=$(printf '%s' "$release" | jq -r .isPrerelease)
if [ "$candidate" = yes ] && [ "$prerelease" != true ]; then
    fails "$tag is a candidate and its release is no pre-release: GitHub makes it Latest at its publish, and install.sh installs it (gh release edit $tag --prerelease)"
fi
if [ "$candidate" = no ] && [ "$prerelease" != false ]; then
    fails "$tag is a release and is marked a pre-release: GitHub makes it no Latest, and install.sh never installs it"
fi
app=no
if [ "$draft" = true ]; then
    ok "$tag is a draft: release.yml's files and the image are verified; the app's downloads and the registries' packages follow its publish"
elif [ "$candidate" = yes ]; then
    ok "$tag is a published pre-release: a candidate has no app files (desktop.yml takes X.Y.Z alone)"
else
    app=yes
    ok "$tag is a published release"
fi

doing "GitHub's Latest"
if [ "$draft" = false ]; then
    if ! latest=$(gh api "repos/$repo/releases/latest" --jq .tag_name 2>"$err"); then
        fails "GitHub's Latest could not be read: $(said)"
    fi
    whole 'v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)' "$latest" ||
        fails "GitHub's Latest is $latest, no X.Y.Z release: install.sh and install.ps1 install it (tools/release-latest.sh github $tag puts it back)"
    ok "GitHub's Latest is $latest, a release"
fi

# Exactly the assets the workflows attach: none missing, none beside them.
doing "the asset list"
assets "$app" >"$work/want"
cut -d' ' -f2 "$work/want" | sort >"$work/want-names"
printf '%s' "$release" | jq -r '.assets[].name' | sort >"$work/have-names"
missing=$(comm -23 "$work/want-names" "$work/have-names")
[ -z "$missing" ] || fails "$tag lacks $(echo $missing | sed 's/ /, /g'), which $( [ "$app" = yes ] && echo "release.yml and desktop.yml attach" || echo "release.yml attaches")"
extra=$(comm -13 "$work/want-names" "$work/have-names")
[ -z "$extra" ] || fails "$tag holds $(echo $extra | sed 's/ /, /g'), which no workflow attaches: tools/release-verify.sh's list and the workflows part"
ok "$tag holds its $(wc -l <"$work/want-names" | tr -d ' ') assets, no more"

mkdir "$work/assets"
gh release download "$tag" -R "$repo" -D "$work/assets" >/dev/null 2>"$err" ||
    fails "$tag's assets could not be downloaded: $(said)"
while read -r kind name; do
    [ -f "$work/assets/$name" ] || fails "$name: not downloaded"
done <"$work/want"

# gh attestation verify of $1 as built by workflow $2 at ref $3.
verified() {
    gh attestation verify "$1" -R "$repo" --cert-identity "https://github.com/$repo/.github/workflows/$2@$3" --source-ref "$3" \
        --deny-self-hosted-runners >/dev/null 2>"$err"
}

# SHA256SUMS's lines before the app's: the file release.yml's `publish`
# wrote and attested in the tag's own run, byte for byte (head -n keeps
# them as they are), before a sum is read from it. The app's lines come
# after all of them, as desktop.yml appends them.
doing "SHA256SUMS's attestation"
sums=$work/assets/SHA256SUMS
late=$(awk '{ f = $2; sub(/^\*/, "", f) } f ~ /^fennec-studio-/ { app = 1; next } app { print f }' "$sums")
[ -z "$late" ] || fails "SHA256SUMS lists $(echo $late | sed 's/ /, /g') after the app's lines, which desktop.yml appends to release.yml's: no run wrote it there"
own=$(awk '{ f = $2; sub(/^\*/, "", f) } f ~ /^fennec-studio-/ { exit } { n++ } END { print n + 0 }' "$sums")
head -n "$own" "$sums" >"$work/sums-release"
verified "$work/sums-release" release.yml "refs/tags/$tag" ||
    fails "SHA256SUMS: its $own lines before the app's are no file release.yml attested at refs/tags/$tag, so no sum in it is read: $(said)"
ok "SHA256SUMS: release.yml at refs/tags/$tag, its $own lines before the app's"

# SHA256SUMS: every asset but itself and the .checksum, once, by its sum.
doing "SHA256SUMS's sums"
listed=0
while read -r kind name; do
    case $name in SHA256SUMS | *.checksum) continue ;; esac
    want=$(awk -v n="$name" '{ f = $2; sub(/^\*/, "", f); if (f == n) print $1 }' "$sums")
    [ -n "$want" ] || fails "SHA256SUMS lists no $name"
    [ "$(printf '%s\n' "$want" | wc -l | tr -d ' ')" -eq 1 ] || fails "SHA256SUMS lists $name more than once"
    [ "$want" = "$(sha256 "$work/assets/$name")" ] || fails "SHA256SUMS lists $name with $want, and the asset is $(sha256 "$work/assets/$name")"
    listed=$((listed + 1))
done <"$work/want"
[ "$(grep -c . "$sums")" -eq "$listed" ] || fails "SHA256SUMS has $(grep -c . "$sums") lines for $listed assets"
ok "SHA256SUMS lists $listed assets, each by its sum"

# A .checksum file is no line of SHA256SUMS: the digest of the zip beside
# it, which Package.swift's binaryTarget names. Held to that zip here.
doing "each .checksum"
while read -r kind name; do
    case $name in *.checksum) ;; *) continue ;; esac
    zip=${name%.checksum}
    [ -f "$work/assets/$zip" ] || fails "$name: $zip, the zip it stands beside, is not an asset"
    held=$(cat "$work/assets/$name")
    [ "$held" = "$(sha256 "$work/assets/$zip")" ] || fails "$name holds [$held], and $zip is $(sha256 "$work/assets/$zip")"
    ok "$name is $zip's SHA-256"
done <"$work/want"

doing "each asset's attestation"
while read -r kind name; do
    file=$work/assets/$name
    case $kind in
        sums) continue ;;
        release) workflow=release.yml ref="refs/tags/$tag" ;;
        swift) workflow=swift-binary.yml ref=$swift_ref ;;
        desktop) workflow=desktop.yml ref=refs/heads/main ;;
    esac
    if ! verified "$file" "$workflow" "$ref"; then
        pending="the attestations of $(awk -v n="$name" 'f && $1 != "sums" { printf "%s%s", s, $2; s = ", " } $2 == n { f = 1 }' "$work/want")"
        [ "$pending" != "the attestations of " ] || pending=
        fails "$name: no attestation by $workflow at $ref: $(said)"
    fi
    ok "$name: $workflow at $ref"
done <"$work/want"

# The image, by the digest ghcr.io serves for each tag, which the verified
# statement must name.
doing "the image's three tags"
path=${image#*/}
host=${image%%/"$path"}
token=$(curl -fsS "https://$host/token?scope=repository:$path:pull" 2>"$err" | jq -r '.token // empty') || true
[ -n "$token" ] || fails "$host gave no token to read $path: $(said)"
for ref in "$version" "$version-amd64" "$version-arm64"; do
    case $ref in
        *-amd64) pending="$image:$version-arm64" ;;
        *-arm64) pending= ;;
        *) pending="$image:$version-amd64, $image:$version-arm64" ;;
    esac
    headers=$(curl -fsSI -H "Authorization: Bearer $token" \
        -H 'Accept: application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json' \
        "https://$host/v2/$path/manifests/$ref" 2>"$err") || fails "$image:$ref is not on the registry: $(said)"
    digest=$(printf '%s\n' "$headers" | tr -d '\r' | awk 'tolower($0) ~ /^docker-content-digest:/ { sub(/^[^:]*: */, ""); print }')
    whole 'sha256:[0-9a-f]{64}' "$digest" || fails "$image:$ref: the registry names no digest ([$digest])"
    subjects=$(gh attestation verify "oci://$image:$ref" -R "$repo" --cert-identity "https://github.com/$repo/.github/workflows/release.yml@refs/tags/$tag" \
        --source-ref "refs/tags/$tag" --deny-self-hosted-runners --format json \
        --jq '.[].verificationResult.statement.subject[].digest.sha256' 2>"$err") ||
        fails "$image:$ref ($digest): no attestation by release.yml at refs/tags/$tag: $(said)"
    printf '%s\n' "$subjects" | grep -qxF "${digest#sha256:}" ||
        fails "$image:$ref: the attestation names [$(echo $subjects)], not $digest, which the registry serves"
    ok "$image:$ref $digest: release.yml at refs/tags/$tag"
done

pending=
if [ "$draft" = true ]; then
    ok "$tag, a draft: verified as far as a draft goes; run this again once it is published and the app attached"
    exit 0
fi

# npm: each package's SLSA provenance, naming packages.yml of this
# repository; then the registry's signatures and the attestations, which
# `npm audit signatures` checks where a package has them.
doing "npm's provenance"
pkgs="@fennecdb/web @fennecdb/react @fennecdb/cloudflare @fennecdb/langchain"
for pkg in $pkgs; do
    pending=$(after "$pkg" $pkgs)
    [ -z "$pending" ] || pending="npm's provenance of $pending"
    att=$(npm view "$pkg@$version" dist.attestations --json 2>"$err") || fails "npm $pkg@$version could not be read: $(said)"
    url=$(printf '%s' "${att:-null}" | jq -r 'if type == "object" and .provenance.predicateType == "https://slsa.dev/provenance/v1" then .url // empty else empty end')
    [ -n "$url" ] || fails "npm $pkg@$version carries no SLSA provenance (dist.attestations is [$(echo $att)]): published without --provenance, or not by packages.yml"
    bundle=$(curl -fsS "$url" 2>"$err") || fails "npm $pkg@$version: its attestations ($url) could not be read: $(said)"
    from=$(printf '%s' "$bundle" | jq -r '[.attestations[] | select(.predicateType == "https://slsa.dev/provenance/v1")
        | .bundle.dsseEnvelope.payload | @base64d | fromjson | .predicate.buildDefinition.externalParameters.workflow
        | "\(.repository) \(.path) \(.ref)"] | first // empty')
    case $from in
        "https://github.com/$repo .github/workflows/packages.yml "*) ;;
        *) fails "npm $pkg@$version: its provenance names [$from], not packages.yml of $repo" ;;
    esac
    ok "npm $pkg@$version: SLSA provenance, packages.yml at ${from##* }"
done
pending=
doing "npm audit signatures"
mkdir "$work/npm"
(
    cd "$work/npm"
    npm init -y >/dev/null 2>"$err"
    npm install --ignore-scripts --no-audit --no-fund --legacy-peer-deps "@fennecdb/web@$version" \
        "@fennecdb/react@$version" "@fennecdb/cloudflare@$version" "@fennecdb/langchain@$version" >/dev/null 2>"$err"
    npm audit signatures >"$work/npm-audit" 2>"$err"
) || fails "npm audit signatures over the four packages: $(said) $(tr '\n' ' ' <"$work/npm-audit" 2>/dev/null || true)"
ok "npm audit signatures: the four packages' registry signatures and attestations"

# PyPI: PEP 440 spells a candidate X.Y.ZrcN.
doing "PyPI's attestations"
case $version in
    *-rc.*) pyversion=${core}rc${version#*-rc.} ;;
    *) pyversion=$version ;;
esac
files=$(curl -fsS "https://pypi.org/pypi/fennecdb/$pyversion/json" 2>"$err" | jq -r '.urls[].filename') || true
[ -n "$files" ] || fails "PyPI holds no files of fennecdb $pyversion: $(said)"
for f in $files; do
    pending=$(after "$f" $files)
    [ -z "$pending" ] || pending="PyPI's attestations of $pending"
    prov=$(curl -fsS "https://pypi.org/integrity/fennecdb/$pyversion/$f/provenance" 2>"$err") ||
        fails "PyPI's $f has no attestation (uploaded with PYPI_API_TOKEN, which makes none?): $(said)"
    printf '%s' "$prov" | jq -e --arg repo "$repo" '[.attestation_bundles[].publisher
        | select(.kind == "GitHub" and .repository == $repo and .workflow == "packages.yml")] | length > 0' >/dev/null ||
        fails "PyPI's $f: its attestations name no publisher packages.yml of $repo: $(printf '%s' "$prov" | jq -c '[.attestation_bundles[].publisher]')"
    ok "PyPI $f: attested, packages.yml of $repo"
done

ok "$tag verified: its assets, the image, npm's four packages and PyPI's files"

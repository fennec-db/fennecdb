#!/bin/sh
# Flutter's SDK from Flutter's own archive, into DIR/flutter:
#
#   tools/install-flutter.sh VERSION RELEASES SHA256 DIR
#
# RELEASES is Flutter's list of its releases for this system
# (https://storage.googleapis.com/flutter_infra_release/releases/releases_<linux|macos>.json)
# and SHA256 the sum that list gives VERSION's stable archive for this
# machine's processor. The workflow that calls this names all three, so a
# newer Flutter is a change of the version and its sum, made on purpose. The
# archive the list names is held to the list's sum and to SHA256 before it
# is unpacked: a list or a download that is not what was looked at is
# refused. An SDK already in DIR (the workflow's cache) is taken as it is,
# and either is then held to VERSION as Flutter itself reports it.
# ci.yml's mobile jobs and packages.yml's `pub` come through here, in place
# of subosito/flutter-action, whose own steps run actions/cache by its tag
# (docs/design/integrations.md#workflows).

set -eu

[ $# -eq 4 ] || { sed -n '2,/^$/s/^# \{0,1\}//p' "$0" >&2; exit 2; }
version=$1
releases=$2
want=$3
dir=$4

command -v jq >/dev/null 2>&1 || { echo "jq reads Flutter's lists and its version, and there is none" >&2; exit 1; }

if [ -x "$dir/flutter/bin/flutter" ]; then
    echo "Flutter in $dir, from the cache"
else
    case "$(uname -m)" in
        x86_64 | amd64) arch=x64 ;;
        arm64 | aarch64) arch=arm64 ;;
        *)
            echo "no Flutter archive for $(uname -m)" >&2
            exit 1
            ;;
    esac
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/install-flutter.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT
    curl -fsSL --retry 3 -o "$tmp/releases.json" "$releases"
    # The one stable release of that version for this processor: its
    # archive's URL and the sum the list gives it.
    entry=$(jq -r --arg v "$version" --arg a "$arch" '
        .base_url as $base
        | [.releases[] | select(.channel == "stable" and .version == $v and (.dart_sdk_arch // "x64") == $a)]
        | if length == 1 then "\($base)/\(.[0].archive) \(.[0].sha256)" else "none none" end' "$tmp/releases.json")
    url=${entry% *}
    listed=${entry##* }
    if [ "$listed" != "$want" ]; then
        echo "$releases gives Flutter $version for $arch as [$entry], not the $want asked for" >&2
        exit 1
    fi
    curl -fsSL --retry 3 -o "$tmp/archive" "$url"
    if command -v sha256sum >/dev/null 2>&1; then
        got=$(sha256sum "$tmp/archive" | awk '{ print $1 }')
    else
        got=$(shasum -a 256 "$tmp/archive" | awk '{ print $1 }')
    fi
    if [ "$got" != "$want" ]; then
        echo "$url is $got, not the $want $releases gives it" >&2
        exit 1
    fi
    # Unpacked beside its place and moved in whole: an unpacking cut short
    # leaves no SDK that a later run would take as one.
    rm -rf "$dir/.unpacking"
    mkdir -p "$dir/.unpacking"
    case $url in
        *.zip) unzip -q "$tmp/archive" -d "$dir/.unpacking" ;;
        *) tar -xf "$tmp/archive" -C "$dir/.unpacking" ;;
    esac
    mv "$dir/.unpacking/flutter" "$dir/flutter"
    rmdir "$dir/.unpacking"
    echo "Flutter $version in $dir, from $url ($got)"
fi

# The SDK is VERSION's, whether this run unpacked it or the cache held it:
# the cache's key is a name the workflow writes, and a key and a version
# that part would run another Flutter, green. Flutter's own answer
# (`--machine`, a JSON object, which a first run's notices may come before
# on its output) and not a file of its tree.
machine=$("$dir/flutter/bin/flutter" --version --machine) || {
    echo "$dir/flutter/bin/flutter --version --machine failed: [$machine]" >&2
    exit 1
}
found=$(printf '%s\n' "$machine" | sed -n '/^{/,/^}/p' | jq -r '.frameworkVersion // empty' 2>/dev/null) || found=
if [ "$found" != "$version" ]; then
    echo "$dir/flutter is Flutter [$found], not the $version asked for: $machine" >&2
    exit 1
fi
echo "Flutter $found in $dir/flutter"

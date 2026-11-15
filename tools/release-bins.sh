#!/usr/bin/env bash
# A published release's `fennec-server`, `fennec` and `fennec-shard`, the old
# side of tools/compat-matrix.sh (docs/design/compatibility.md#the-compatibility-matrix):
#
#   tools/release-bins.sh TAG OUT_DIR [TARGET]
#
# TAG is a release's tag (v0.2.0), or `latest`: the published release, not a
# draft nor a pre-release, highest in version order up to this tree's
# version, and not one tagged at this commit, so a release's own run is held
# to the one before it (asked of GitHub with `gh`; a `gh` that fails stops
# the script, never read as "no release").
#
# `fennec-server` and `fennec` come from the release's own tarball,
# `fennec-<TAG>-<TARGET>.tar.gz`, held to its line of the release's
# `SHA256SUMS` by site/install.sh -- the installer a user runs, which checks the
# sum before it unpacks anything (tools/CLAUDE.md) -- served to it from this
# machine, which is the one place besides https it fetches from. So the check
# is the installer's own, not a third one written here. The two files are
# taken from OLD_ASSETS_DIR where it is set -- a directory already holding
# them, copied out of it, nothing run there -- or else from the release
# through `gh release download` (GH_TOKEN; GITHUB_REPOSITORY, default
# fennec-db/fennecdb), as ci.yml's latest-release job takes its tarball.
#
# The release ships no `fennec-shard`: release.yml packs `fennec` and
# `fennec-server` alone. So the router is built from the tag's own sources
# (`git archive`, fetched where this clone lacks the tag and no
# OLD_ASSETS_DIR is set) with the tag's Cargo.lock (`--locked`) and this
# tree's toolchain, a debug build into SHARD_TARGET_DIR (default
# target/compat-shard, kept: compat.yml's cargo cache holds target/, so the
# router's dependencies are not built cold each run) -- the code the release
# was cut from, not a binary it published.
#
# Nothing reaches OUT_DIR until all three binaries are in hand and each
# one's `--version` is the tag's: a tarball whose sum differs, a tag with no
# sources, a router that does not build, each leaves OUT_DIR as it was
# (tools/test_release_bins.sh holds the first, a byte of a copy flipped).
#
# TARGET defaults to this machine's, the one install.sh installs; another is
# refused, since the matrix runs these binaries here.

set -euo pipefail

TAG=${1:?usage: tools/release-bins.sh TAG OUT_DIR [TARGET]}
OUT=${2:?usage: tools/release-bins.sh TAG OUT_DIR [TARGET]}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
REPO=${GITHUB_REPOSITORY:-fennec-db/fennecdb}
WORK=$(mktemp -d "${TMPDIR:-/tmp}/fennec-release-bins.XXXXXX")
SERVER_PID=

cleanup() {
    if [ -n "$SERVER_PID" ]; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK"
}
# EXIT cleans up; a signal ends the script, whose EXIT trap then runs
# (cleaning up on the signal and going on would build a router after a ^C).
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

die() {
    echo "release-bins: $*" >&2
    exit 1
}

# The target as release.yml names its tarballs, as install.sh's
# detect_target works it out (Linux's builds are musl's).
here=
case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) here=aarch64-apple-darwin ;;
    Darwin-x86_64)
        if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
            here=aarch64-apple-darwin
        else
            here=x86_64-apple-darwin
        fi
        ;;
    Linux-x86_64 | Linux-amd64) here=x86_64-unknown-linux-musl ;;
    Linux-aarch64 | Linux-arm64) here=aarch64-unknown-linux-musl ;;
    *) die "no release tarball runs on $(uname -s) $(uname -m)" ;;
esac
TARGET=${3:-$here}
[ "$TARGET" = "$here" ] ||
    die "$TARGET's binaries do not run here ($here): install.sh installs this machine's"

if [ "$TAG" = latest ]; then
    [ -z "${OLD_ASSETS_DIR:-}" ] ||
        die "OLD_ASSETS_DIR holds one release's files: name its tag in place of \`latest\`"
    # sed quits at the first line, no pipe left to close early under pipefail.
    tree=$(sed -n 's/^version = "\(.*\)"/\1/p;/^version = /q' "$ROOT/Cargo.toml")
    # Asked once, its failure the script's: inside `for t in $(gh ...)` a
    # failing gh was an empty list, read as "no published release".
    listed=$(gh release list -R "$REPO" --exclude-drafts --exclude-pre-releases \
        --limit 200 --json tagName,isDraft,isPrerelease) ||
        die "gh could not list $REPO's releases"
    # A release cut at this very commit is this tree: the run on its tag is
    # held to the release before it.
    here_tags=$(git -C "$ROOT" tag --points-at HEAD)
    # Version order, not the list's (creation dates: a patch of an older
    # minor made after a newer minor would come first): X.Y.Z compared as
    # numbers, a pre-release below its release, and never above this tree.
    TAG=$(printf '%s' "$listed" | python3 -I -c '
import json, re, sys

tree, here = sys.argv[1], set(sys.argv[2].split())


def key(v):
    m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?", v)
    if not m:
        return None
    pre = m.group(4)
    return (int(m.group(1)), int(m.group(2)), int(m.group(3)), pre is None, pre or "")


top = key(tree)
if top is None:
    sys.exit("this tree'"'"'s version, %r, is no X.Y.Z" % tree)
best = None
for r in json.load(sys.stdin):
    t = r.get("tagName", "")
    if r.get("isDraft") or r.get("isPrerelease") or t in here or not t.startswith("v"):
        continue
    k = key(t[1:])
    if k is not None and k <= top and (best is None or k > best[0]):
        best = (k, t)
print(best[1] if best else "")
' "$tree" "$here_tags") || die "could not read $REPO's releases"
    [ -n "$TAG" ] || die "no published release up to this tree's version, $tree"
    echo "release-bins: the newest published release up to $tree is $TAG"
fi
case $TAG in
    v[0-9]*) ;;
    *) die "\"$TAG\" is not a release's tag (v0.2.0)" ;;
esac
VERSION=${TAG#v}
case $VERSION in
    *[!0-9A-Za-z.+-]*) die "\"$TAG\" is not a release's tag (v0.2.0)" ;;
esac
NAME=fennec-$TAG-$TARGET.tar.gz

# The release's files, in a directory of their own laid out as GitHub serves
# them: <base>/download/<tag>/<file>.
SITE=$WORK/site
mkdir -p "$SITE/download/$TAG"
if [ -n "${OLD_ASSETS_DIR:-}" ]; then
    for f in "$NAME" SHA256SUMS; do
        [ -f "$OLD_ASSETS_DIR/$f" ] || die "$OLD_ASSETS_DIR holds no $f"
        cp "$OLD_ASSETS_DIR/$f" "$SITE/download/$TAG/$f"
    done
else
    gh release download "$TAG" -R "$REPO" -p "$NAME" -p SHA256SUMS -D "$SITE/download/$TAG" ||
        die "gh could not download $NAME and SHA256SUMS of $TAG"
fi

# A server on a port the system picks, found by the file it writes, as
# site/test_install.sh serves install.sh its releases. Isolated (-I): the
# directory it serves is the release's files, not code.
python3 -I - "$SITE" "$WORK/port" <<'EOF' &
import functools, http.server, sys


class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


server = http.server.ThreadingHTTPServer(
    ("127.0.0.1", 0), functools.partial(Quiet, directory=sys.argv[1])
)
open(sys.argv[2], "w").write(str(server.server_address[1]))
server.serve_forever()
EOF
SERVER_PID=$!
for _ in $(seq 1 200); do
    [ -s "$WORK/port" ] && break
    kill -0 "$SERVER_PID" 2>/dev/null || die "the file server for install.sh did not start"
    sleep 0.05
done
[ -s "$WORK/port" ] || die "the file server for install.sh named no port"

# install.sh checks the tarball against SHA256SUMS before it unpacks it, and
# installs nothing where they differ. It installs into this run's own
# directory; OUT_DIR is written only once all three binaries are here.
if ! FENNEC_BASE_URL="http://127.0.0.1:$(cat "$WORK/port")" FENNEC_VERSION="$VERSION" \
    FENNEC_INSTALL="$WORK/installed" HOME="$WORK" sh "$ROOT/site/install.sh" >"$WORK/install.log" 2>&1; then
    cat "$WORK/install.log" >&2
    die "install.sh did not install $NAME: nothing installed in $OUT"
fi
grep -q '^Checksum ok' "$WORK/install.log" || {
    cat "$WORK/install.log" >&2
    die "install.sh said nothing of $NAME's checksum: nothing installed in $OUT"
}
echo "release-bins: $NAME, $(grep '^Checksum ok' "$WORK/install.log") against the release's SHA256SUMS"
kill "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=

# The router, from the tag's sources.
if ! git -C "$ROOT" rev-parse -q --verify "refs/tags/$TAG^{commit}" >/dev/null; then
    [ -z "${OLD_ASSETS_DIR:-}" ] || die "this clone has no tag $TAG to build its fennec-shard from: nothing installed in $OUT"
    git -C "$ROOT" fetch --quiet --no-tags origin "refs/tags/$TAG:refs/tags/$TAG" ||
        die "could not fetch the tag $TAG: nothing installed in $OUT"
fi
mkdir "$WORK/src"
git -C "$ROOT" archive "$TAG" | tar -x -C "$WORK/src"
SHARD_TARGET=${SHARD_TARGET_DIR:-$ROOT/target/compat-shard}
echo "release-bins: building fennec-shard from $TAG's sources ($(git -C "$ROOT" rev-parse --short "$TAG^{commit}")) into $SHARD_TARGET ..."
# Built from here, not from the sources' directory, so the toolchain is this
# tree's (rustup reads rust-toolchain.toml from the working directory): the
# tag's may not be installed. --locked: the tag's own Cargo.lock.
(cd "$ROOT" && CARGO_TARGET_DIR="$SHARD_TARGET" "${CARGO:-cargo}" build --quiet \
    --locked --manifest-path "$WORK/src/Cargo.toml" -p fennec-shard) ||
    die "fennec-shard did not build from $TAG's sources: nothing installed in $OUT"
cp "$SHARD_TARGET/debug/fennec-shard" "$WORK/installed/bin/fennec-shard"

for b in fennec-server fennec fennec-shard; do
    said=$("$WORK/installed/bin/$b" --version 2>&1) || true
    said=${said%%$'\n'*}
    [ "$said" = "$b $VERSION" ] || die "$b of $TAG says \"$said\", not $b $VERSION: nothing installed in $OUT"
done
# Copied into a directory beside OUT first, then renamed in: a copy failing
# part way (a full disk) leaves OUT as it was, not holding some of the three.
mkdir -p "$OUT"
STAGE=$(mktemp -d "$OUT.stage.XXXXXX")
STAGED=$STAGE
trap 'rm -rf "$STAGED"; cleanup' EXIT
for b in fennec-server fennec fennec-shard; do
    cp "$WORK/installed/bin/$b" "$STAGE/$b"
done
for b in fennec-server fennec fennec-shard; do
    mv -f "$STAGE/$b" "$OUT/$b"
done
rm -rf "$STAGE"
echo "release-bins: $TAG's fennec-server, fennec and fennec-shard in $OUT"

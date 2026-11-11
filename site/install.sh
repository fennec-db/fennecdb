#!/bin/sh
# Install fennecdb: `fennec`, the shell, and `fennec-server`, from a release.
#
#   curl -fsSL https://fennecdb.com/install.sh | sh
#
# Or, on a Mac with Apple silicon, the fennec studio app (beta) instead:
#
#   curl -fsSL https://fennecdb.com/install.sh | sh -s -- --app
#
# Read it first with `curl -fsSL https://fennecdb.com/install.sh | less`. It
#
#   - works out the platform: macOS or Linux, arm64 or x86_64;
#   - downloads the release's tarball and its SHA256SUMS from GitHub, over
#     https, and checks the tarball against the sum before it unpacks it;
#   - puts `fennec` and `fennec-server` in a bin/ directory of your own: no sudo,
#     and no shell startup file is touched. It says how to put the directory
#     on your PATH, and that is the only thing left to you.
#
# With --app it downloads the release's DMG of the app instead, checks it
# against SHA256SUMS before it attaches it (read-only), and copies the app
# into ~/Applications: no sudo, and nothing else is touched. A file curl
# downloads carries no quarantine, so macOS opens the app without asking.
#
# Settings, all optional:
#
#   FENNEC_VERSION  X.Y.Z (or vX.Y.Z); the latest release when unset
#   FENNEC_INSTALL  where it goes: $FENNEC_INSTALL/bin; default ~/.fennecdb
#   FENNEC_BASE_URL where releases are fetched from, for a mirror or a test:
#                   https://github.com/fennec-db/fennecdb/releases by default,
#                   and it must be https (http only for localhost)
#
# The checksums come from the same release as the tarball: they catch a
# truncated or damaged download, not a release someone replaced.
#
# The whole script is one function, called on the last line: a download cut
# short stops before that line and runs nothing.

set -eu

say() {
    printf '%s\n' "$*"
}

err() {
    printf 'fennecdb install: %s\n' "$*" >&2
}

die() {
    err "$*"
    exit 1
}

usage() {
    cat <<'EOF'
Install fennecdb (fennec, the shell, and fennec-server) from a release.

    curl -fsSL https://fennecdb.com/install.sh | sh
    curl -fsSL https://fennecdb.com/install.sh | sh -s -- --help
    curl -fsSL https://fennecdb.com/install.sh | sh -s -- --app

  FENNEC_VERSION  X.Y.Z; the latest release when unset
  FENNEC_INSTALL  installs into $FENNEC_INSTALL/bin; default ~/.fennecdb
  --app           the fennec studio app (beta) into ~/Applications instead,
                  on macOS with Apple silicon

macOS and Linux on arm64 and x86_64. On Windows, PowerShell installs the
zip: irm https://fennecdb.com/install.ps1 | iex (or run this inside WSL).
EOF
}

# The platform as the release names it: <arch>-<vendor>-<os>.
detect_target() {
    os=$(uname -s)
    cpu=$(uname -m)
    case $cpu in
        arm64 | aarch64) arch=aarch64 ;;
        x86_64 | amd64) arch=x86_64 ;;
        *) die "no fennecdb release for the processor '$cpu' (arm64 and x86_64 only). The Docker image, ghcr.io/fennec-db/fennec-server, is another way." ;;
    esac
    case $os in
        Darwin)
            # A shell started under Rosetta says x86_64 on an Apple silicon
            # Mac: the native build is the one to install.
            if [ "$arch" = x86_64 ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
                arch=aarch64
            fi
            target="$arch-apple-darwin"
            ;;
        Linux)
            # The Linux builds are static (musl): glibc or musl, nothing to match.
            target="$arch-unknown-linux-musl"
            ;;
        MINGW* | MSYS* | CYGWIN* | Windows_NT)
            # Git Bash, MSYS and Cygwin are not where the Windows zip goes:
            # PowerShell, which every Windows has, installs it.
            die "this script installs on macOS and Linux. On Windows, in PowerShell: irm https://fennecdb.com/install.ps1 | iex (or run this script inside WSL)."
            ;;
        *) die "no fennecdb release for '$os' (macOS and Linux only). The Docker image, ghcr.io/fennec-db/fennec-server, runs anywhere Docker does." ;;
    esac
}

# The app's platform: macOS on Apple silicon, a Rosetta shell's included
# (it says x86_64 there). Its build tool makes no other build of it.
app_platform() {
    os=$(uname -s)
    cpu=$(uname -m)
    if [ "$os" = Darwin ]; then
        case $cpu in
            arm64) return 0 ;;
            x86_64) [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" != 1 ] || return 0 ;;
        esac
    fi
    die "the fennec studio app is for macOS on Apple silicon (arm64), and this is $os on $cpu: nothing was installed. The studio itself runs in a browser wherever fennec-server does: install it without --app and start it with --studio."
}

# fetch URL FILE. Over https each tool is held to https and TLS 1.2 or later,
# and a certificate it cannot check is a failure, never a download.
fetch() {
    case $downloader in
        curl)
            case $1 in
                https://*) curl -fsSL --retry 3 --proto '=https' --tlsv1.2 -o "$2" "$1" ;;
                *) curl -fsSL --retry 3 -o "$2" "$1" ;;
            esac
            ;;
        gnu-wget)
            case $1 in
                https://*) wget -q --https-only --secure-protocol=TLSv1_2 -O "$2" "$1" ;;
                *) wget -q -O "$2" "$1" ;;
            esac
            ;;
        busybox-wget)
            # Its own stderr is read: a build that does not check the server's
            # certificate says so on every connection, and what it fetched is
            # then not to be trusted with a tarball.
            fetched=$(wget -q -O "$2" "$1" 2>&1) || {
                [ -z "$fetched" ] || printf '%s\n' "$fetched" >&2
                return 1
            }
            case $fetched in
                *"certificate validation not implemented"*)
                    rm -f "$2"
                    err "this wget does not check TLS certificates: install curl and run this again"
                    return 1
                    ;;
            esac
            ;;
    esac
}

# Which downloader, before anything is downloaded: curl, else a wget that
# checks certificates. BusyBox's does only where it hands the connection to
# ssl_client (Alpine's does); upstream's own TLS checks nothing, and a
# tarball and the sum it is held to would both come over a connection anyone
# on the path could answer.
choose_downloader() {
    if command -v curl >/dev/null 2>&1; then
        downloader=curl
    elif command -v wget >/dev/null 2>&1; then
        case $(wget --help 2>&1 | head -n 1) in
            *BusyBox*)
                command -v ssl_client >/dev/null 2>&1 ||
                    die "the wget here is BusyBox's, which does not check TLS certificates: install curl and run this again"
                downloader=busybox-wget
                ;;
            *)
                case $(wget --version 2>&1 | head -n 1) in
                    "GNU Wget"*) downloader=gnu-wget ;;
                    *) die "the wget here is not one this knows to check TLS certificates: install curl and run this again" ;;
                esac
                ;;
        esac
    else
        die "needs curl or wget to download the release"
    fi
}

sha256_of() {
    case $hasher in
        sha256sum) sha256sum "$1" | awk '{ print $1 }' ;;
        shasum) shasum -a 256 "$1" | awk '{ print $1 }' ;;
    esac
}

# Lets the app's DMG go, forced if something (Spotlight) holds it a moment.
detach_dmg() {
    hdiutil detach -quiet "$mnt" 2>/dev/null || hdiutil detach -quiet -force "$mnt" 2>/dev/null || return 1
    mounted=
}

cleanup() {
    if [ -n "${mounted:-}" ] && ! detach_dmg; then
        # $tmp holds the mount point: removing it under a mount would reach
        # into the image.
        err "could not detach $mnt: hdiutil detach -force \"$mnt\" lets it go"
        return
    fi
    if [ -n "${stage:-}" ]; then
        rm -rf "$stage"
    fi
    if [ -n "${tmp:-}" ]; then
        rm -rf "$tmp"
    fi
}

# The app, from the release's DMG: checked against SHA256SUMS before it is
# attached, attached read-only where nothing else sees it, the app copied out
# and the DMG let go, the copy's signature checked (ad hoc: no Developer ID
# signs it, but it must hold), and only then put in ~/Applications, over the
# one there by rename, so a failure leaves that one as it was.
install_app() {
    for tool in hdiutil ditto codesign ps; do
        command -v "$tool" >/dev/null 2>&1 || die "needs '$tool'"
    done
    [ -n "${HOME:-}" ] || die "HOME is not set: the app goes into ~/Applications"
    appdir=$HOME/Applications
    bundle="fennec studio.app"
    dest=$appdir/$bundle

    name=fennec-studio-v$version-macos-arm64.dmg
    want=$(awk -v n="$name" '{ f = $2; sub(/^\*/, "", f); if (f == n) { print $1; exit } }' "$sums")
    [ -n "$want" ] || die "release v$version lists no $name: it has no Mac app yet"

    # A running app's files are not replaced under it: a process whose
    # command starts with the app's path. Not pgrep, whose -f takes a regular
    # expression, nor a grep, whose own command holds the path.
    if ps -axww -o command= 2>/dev/null |
        running="$dest/Contents/MacOS/" awk 'index($0, ENVIRON["running"]) == 1 { found = 1 } END { exit !found }'; then
        die "fennec studio is running from $dest: quit it, then run this again"
    fi

    say "Downloading $name"
    fetch "$base/download/v$version/$name" "$tmp/$name" ||
        die "could not download $base/download/v$version/$name"
    got=$(sha256_of "$tmp/$name")
    if [ "$got" != "$want" ]; then
        die "the checksum of $name is $got, SHA256SUMS says $want: nothing was installed"
    fi
    say "Checksum ok ($want)"

    mnt=$tmp/mnt
    mkdir "$mnt"
    hdiutil attach -quiet -readonly -nobrowse -noautoopen -mountpoint "$mnt" "$tmp/$name" ||
        die "could not attach $name"
    mounted=1
    [ -d "$mnt/$bundle" ] || die "$name holds no $bundle"

    mkdir -p "$appdir" || die "could not make $appdir"
    stage=$appdir/.fennec-studio-install.$$
    rm -rf "$stage"
    mkdir "$stage" || die "could not write in $appdir"
    ditto "$mnt/$bundle" "$stage/$bundle" || die "could not copy the app out of $name"
    detach_dmg || err "could not detach $mnt"
    codesign --verify --deep --strict "$stage/$bundle" 2>/dev/null ||
        die "the copied app's signature does not hold: nothing was installed"

    if [ -e "$dest" ]; then
        mv "$dest" "$stage/previous.app" || die "could not move the app at $dest aside: nothing was installed"
    fi
    if ! mv "$stage/$bundle" "$dest"; then
        if [ -e "$stage/previous.app" ] && ! mv "$stage/previous.app" "$dest"; then
            # Kept, not removed with the rest: it is the only copy left.
            kept=$stage/previous.app
            stage=
            die "could not write $dest, nor put the one that was there back: it is $kept"
        fi
        die "could not write $dest"
    fi
    rm -rf "$stage"
    stage=

    say ""
    say "Installed fennec studio $version (beta) in $dest."
    say "Open it from Launchpad or Spotlight, or:"
    say ""
    say "  open \"$dest\""
    if [ -e "/Applications/$bundle" ]; then
        say ""
        say "Note: /Applications/$bundle is another copy of the app."
    fi
    say ""
    say "The app: https://fennecdb.com/docs/studio#desktop"
}

main() {
    app=
    for arg in "$@"; do
        case $arg in
            -h | --help)
                usage
                return 0
                ;;
            --app) app=1 ;;
            "") ;;
            *)
                usage >&2
                die "unknown argument '$arg'"
                ;;
        esac
    done

    base=${FENNEC_BASE_URL:-https://github.com/fennec-db/fennecdb/releases}
    base=${base%/}
    # https, or http to this machine for a test: the host is read up to its
    # port, and a user or password (an `@`) is refused, since
    # http://localhost:x@example.com is a request to example.com.
    case $base in
        *@* | *[[:space:]]*) die "FENNEC_BASE_URL holds a user, a password or a space: $base" ;;
        https://?*) ;;
        *)
            printf '%s\n' "$base" | grep -Eq '^http://(localhost|127\.0\.0\.1)(:[0-9]+)?(/|$)' ||
                die "FENNEC_BASE_URL must be https (http only for localhost): $base"
            ;;
    esac

    if [ -n "${FENNEC_INSTALL:-}" ]; then
        home=$FENNEC_INSTALL
    elif [ -n "${HOME:-}" ]; then
        home=$HOME/.fennecdb
    else
        die "HOME is not set: say where to install with FENNEC_INSTALL"
    fi
    # The directory is told back as a PATH line, which a relative one would
    # not be good for from any other directory.
    case $home in
        /*) ;;
        *) home=$(pwd)/${home#./} ;;
    esac
    bindir=${home%/}/bin

    # What it will need, before it downloads anything.
    choose_downloader
    if command -v sha256sum >/dev/null 2>&1; then
        hasher=sha256sum
    elif command -v shasum >/dev/null 2>&1; then
        hasher=shasum
    else
        die "needs sha256sum or shasum to check the download"
    fi
    for tool in tar gzip awk sed grep head mktemp uname; do
        command -v "$tool" >/dev/null 2>&1 || die "needs '$tool'"
    done

    if [ -n "$app" ]; then
        app_platform
        # The latest release is found by its macOS tarball, as for the shell.
        target=aarch64-apple-darwin
    else
        detect_target
    fi

    tmp=$(mktemp -d "${TMPDIR:-/tmp}/fennecdb-install.XXXXXX")
    trap cleanup EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM

    # The version: the one asked for, else the latest release's, which the
    # release's own SHA256SUMS names in the tarballs it lists.
    sums=$tmp/SHA256SUMS
    if [ -n "${FENNEC_VERSION:-}" ]; then
        version=${FENNEC_VERSION#v}
        case $version in
            *[!0-9A-Za-z.+-]* | "") die "FENNEC_VERSION '$FENNEC_VERSION' is not a version like 0.1.11" ;;
        esac
        say "Looking for fennecdb $version"
        fetch "$base/download/v$version/SHA256SUMS" "$sums" ||
            die "could not download $base/download/v$version/SHA256SUMS: is there a release v$version?"
    else
        say "Looking for the latest fennecdb release"
        fetch "$base/latest/download/SHA256SUMS" "$sums" ||
            die "could not download $base/latest/download/SHA256SUMS"
        version=$(sed -n "s/^[0-9a-f]\{64\}  *\**fennec-v\([^ ]*\)-$target\.tar\.gz\$/\1/p" "$sums" | head -n 1)
        case $version in
            *[!0-9A-Za-z.+-]* | "") die "the latest release lists no build for $target" ;;
        esac
    fi

    if [ -n "$app" ]; then
        install_app
        return 0
    fi

    name=fennec-v$version-$target.tar.gz
    want=$(awk -v n="$name" '{ f = $2; sub(/^\*/, "", f); if (f == n) { print $1; exit } }' "$sums")
    [ -n "$want" ] || die "release v$version lists no $name: it has no build for this platform"

    say "Downloading $name"
    fetch "$base/download/v$version/$name" "$tmp/$name" ||
        die "could not download $base/download/v$version/$name"

    got=$(sha256_of "$tmp/$name")
    if [ "$got" != "$want" ]; then
        die "the checksum of $name is $got, SHA256SUMS says $want: nothing was installed"
    fi
    say "Checksum ok ($want)"

    tar -xzf "$tmp/$name" -C "$tmp" || die "could not unpack $name"
    src=$tmp/fennec-v$version-$target
    for b in fennec fennec-server; do
        [ -f "$src/$b" ] || die "$name holds no $b"
    done

    mkdir -p "$bindir" || die "could not make $bindir: say where to install with FENNEC_INSTALL"
    for b in fennec fennec-server; do
        # Written beside and renamed over: a running server's file is not
        # overwritten in place ("Text file busy"), and a failure part of the
        # way leaves the old one whole.
        staged=$bindir/.$b.new.$$
        if ! { cp "$src/$b" "$staged" && chmod 755 "$staged" && mv -f "$staged" "$bindir/$b"; }; then
            rm -f "$staged"
            die "could not write $bindir/$b"
        fi
    done

    say ""
    say "Installed fennecdb $version ($target) in $bindir:"
    say "  fennec         the shell:  fennec scratch.fennec"
    say "  fennec-server  the server: fennec-server --file notes.fennec"

    # Does it start? Its version is the first line of --help.
    if line=$("$bindir/fennec-server" --help 2>&1 | head -n 1) && [ -n "$line" ]; then
        say "  ($line)"
    else
        err "installed, but $bindir/fennec-server did not start on this machine"
    fi

    case ":${PATH:-}:" in
        *":$bindir:"*) ;;
        *)
            shown=$bindir
            case $bindir in
                "${HOME:-/nonexistent}"/*) shown="\$HOME/${bindir#"$HOME"/}" ;;
            esac
            say ""
            say "$bindir is not on your PATH. Add it for this shell:"
            say ""
            say "  export PATH=\"$shown:\$PATH\""
            say ""
            say "and put the same line in your shell's startup file (~/.zshrc, ~/.bashrc"
            say "or ~/.profile) to keep it. For fish: fish_add_path \"$shown\"."
            ;;
    esac
    other=$(command -v fennec 2>/dev/null || true)
    if [ -n "$other" ] && [ "$other" != "$bindir/fennec" ]; then
        say ""
        say "Note: 'fennec' on your PATH is $other, not this one."
    fi

    say ""
    say "A first query: https://fennecdb.com/docs/quickstart"
}

main "$@"

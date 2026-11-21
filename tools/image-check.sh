#!/bin/sh
# Run the server's image before it is pushed: release.yml's `image` job
# runs it on the runner that built it, of the image's own architecture,
# before `docker save` hands it to the job that pushes.
#
#   tools/image-check.sh IMAGE VERSION
#
# IMAGE is the reference the build tagged, VERSION the release's (a `v`
# before it is dropped). The image is asked `--version` through its own
# entry point, held to `fennec-server VERSION` on stdout; then started as a
# user starts it -- its own CMD, which listens beyond loopback and so takes
# a token, given in FENNEC_HTTP_TOKEN -- and held to turn healthy by its own
# HEALTHCHECK, the server's `--ping` (GET /_health on 127.0.0.1:8080). An
# image without a HEALTHCHECK fails, as does a container that stops or turns
# unhealthy, at once and with its log; one still starting fails once the
# HEALTHCHECK's start period and its three retries have passed. The
# container and its volume are removed whatever happens.

set -eu

[ $# -eq 2 ] || { sed -n '2,/^$/s/^# \{0,1\}//p' "$0" >&2; exit 2; }
image=$1
version=${2#v}

fails() { echo "FAIL: $*" >&2; exit 1; }

# stdout alone: what a script reads from it.
first=$(docker run --rm "$image" --version) || fails "--version exited with an error, printing [$first]"
[ "$first" = "fennec-server $version" ] || fails "--version: found [$first], wanted [fennec-server $version]"
echo "ran: $first"

# A token of 32 random bytes, in the environment rather than on a command
# line, and thrown away with the container.
FENNEC_HTTP_TOKEN=$(od -An -tx1 -N32 /dev/urandom | tr -d ' \n')
export FENNEC_HTTP_TOKEN
cid=$(docker run -d -e FENNEC_HTTP_TOKEN "$image")
trap 'docker rm -f -v "$cid" >/dev/null 2>&1 || true' EXIT
trap 'exit 1' INT TERM

# Docker runs the HEALTHCHECK every 10 s from the start: healthy at the
# first probe the server answers. The bound is its start period (120 s) and
# three failed probes after it, with room to spare.
start=$(date +%s)
while :; do
    state=$(docker inspect -f '{{.State.Status}} {{if .State.Health}}{{.State.Health.Status}}{{else}}no-healthcheck{{end}}' "$cid")
    waited=$(($(date +%s) - start))
    case $state in
        "running healthy") break ;;
        "running starting") ;;
        *)
            docker logs "$cid" >&2 || true
            fails "the container is [$state] after $waited s, wanted [running healthy]"
            ;;
    esac
    if [ "$waited" -ge 180 ]; then
        docker logs "$cid" >&2 || true
        fails "not healthy after $waited s: [$state]"
    fi
    sleep 1
done
echo "healthy after $waited s: $image"

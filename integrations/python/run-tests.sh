#!/bin/sh
# The Python integrations against a real fennec-server: built here, started on
# this machine, and tested from a python:3.13 container, which gets the same
# interpreter everywhere -- a Homebrew Python whose pyexpat cannot load is
# how this came to use one.
#
#   integrations/python/run-tests.sh [pytest arguments]
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
cargo=${CARGO:-cargo}
[ -x "$HOME/.cargo/bin/cargo" ] && cargo="$HOME/.cargo/bin/cargo"
"$cargo" build -q -p fennec-server --manifest-path "$root/Cargo.toml"

dir=$(mktemp -d)
token=python-tests
# Outside loopback, so the container reaches it: a token is required there.
# On a port the system picks, read off the server's `listening on:` line: a
# run never meets another, or any other server, on one.
"$root/target/debug/fennec-server" --file "$dir/python.fennec" \
    --http 0.0.0.0:0 --http-token "$token" --cdc 2>"$dir/server.log" &
server=$!
# The schema test's own: the migrations a schema ran are recorded in the
# server's own `_migrations`, which no statement changes, so a list starts
# over in a new file -- that test's, its record empty whatever ran before.
"$root/target/debug/fennec-server" --file "$dir/schema.fennec" \
    --http 0.0.0.0:0 --http-token "$token" 2>"$dir/schema.log" &
schema=$!
trap 'kill $server $schema 2>/dev/null; rm -rf "$dir"' EXIT INT TERM
# The port a server's log says it listens on, once it does.
port_of() {
    i=0
    until grep -q "listening on: http://.* " "$1"; do
        i=$((i + 1))
        [ $i -gt 100 ] && { cat "$1" >&2; return 1; }
        sleep 0.1
    done
    sed -n 's/.*listening on: http:[^ ]*:\([0-9]*\) .*/\1/p' "$1" | head -1
}
port=$(port_of "$dir/server.log")
schema_port=$(port_of "$dir/schema.log")

# The source is copied inside, so the build and pytest's caches stay there.
# host.docker.internal is Docker Desktop's name for the machine; a Linux
# daemon, a CI runner's, only knows it when told. The builder's golden file
# lies outside the package, and goes in beside it.
docker run --rm -v "$here:/src:ro" --add-host=host.docker.internal:host-gateway \
    -v "$root/integrations/builder-golden.json:/golden.json:ro" -e FENNEC_GOLDEN=/golden.json \
    -e FENNEC_URL="http://host.docker.internal:$port" -e FENNEC_TOKEN="$token" \
    -e FENNEC_SCHEMA_URL="http://host.docker.internal:$schema_port" \
    python:3.13-slim sh -c \
    "cp -r /src /work && cd /work &&
     pip install -q --root-user-action=ignore --disable-pip-version-check '.[test]' &&
     python -m pytest -q -p no:cacheprovider $*"

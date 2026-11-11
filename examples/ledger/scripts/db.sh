#!/bin/sh
# fennec-server for the ledger: a tenant node (a file per tenant in
# data/tenants), the operator's token, the node's admin token, the
# console's and customers' tokens checked against policy.txt, every write
# on disk before it is answered, the change stream kept for the sink, and
# an audit log of refused tokens and schema changes.
#
#   FENNEC_SERVER  the binary (default: this repository's target/release, then target/debug)
#   FENNEC_PORT    8080; 0 takes a port the system picks, which the server's
#                  `listening on: http://<address>` line names
#   FENNEC_DIR     data/tenants
#   FENNEC_SYNC    always
#   FENNEC_REQUEST_RATE  unset: the server's own bound on a scoped caller,
#                  1 000 requests a second (--request-rate); 0 for none
#   FENNEC_TOKEN, FENNEC_ADMIN_TOKEN, FENNEC_JWT_SECRET   as src/config.ts and src/tokens.ts read them
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$here/../.." && pwd)
server=${FENNEC_SERVER:-}
if [ -z "$server" ]; then
    for b in "$root/target/release/fennec-server" "$root/target/debug/fennec-server"; do
        if [ -x "$b" ]; then server=$b; break; fi
    done
fi
[ -n "$server" ] || { echo "no fennec-server: cargo build --release -p fennec-server, or set FENNEC_SERVER" >&2; exit 1; }
dir=${FENNEC_DIR:-$here/data/tenants}
mkdir -p "$dir"
umask 077
secret="$dir/../jwt.secret"
printf '%s' "${FENNEC_JWT_SECRET:-ledger-dev-jwt-secret-of-at-least-32-bytes}" >"$secret"
# --replication-token keeps each tenant's change stream (/t/<t>/_changes);
# no replica need follow it.
# --request-rate only where asked: unset, a scoped caller -- the app's one
# backend token among them -- meets the server's default.
set --
[ -n "${FENNEC_REQUEST_RATE:-}" ] && set -- --request-rate "$FENNEC_REQUEST_RATE"
FENNEC_HTTP_TOKEN=${FENNEC_TOKEN:-ledger-dev-operator} exec "$server" \
    --dir "$dir" \
    --http "127.0.0.1:${FENNEC_PORT:-8080}" \
    --admin-token "${FENNEC_ADMIN_TOKEN:-ledger-dev-node-admin}" \
    --replication-token "${FENNEC_REPLICATION_TOKEN:-ledger-dev-replication}" \
    --jwt-secret-file "$secret" \
    --policy "$here/policy.txt" \
    --audit "${FENNEC_AUDIT:-$dir/../audit.log}" \
    --sync "${FENNEC_SYNC:-always}" \
    "$@"

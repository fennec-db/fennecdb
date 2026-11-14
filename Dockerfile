# fennecdb -- the server (`fennec-server`), over HTTP
#
#   docker build -t fennecdb .
#   docker run -d --name fennecdb -p 127.0.0.1:8080:8080 -v fennecdata:/data \
#     -e FENNEC_HTTP_TOKEN=secret --memory 1g fennecdb
#   curl -H 'Authorization: Bearer secret' -d '{"query": "collections"}' \
#     http://127.0.0.1:8080/query
#
# Two stages: build statically with musl, then put the result into an *empty*
# image. fennec-server has no dependencies and runs its own health probe (`--ping`),
# so the runtime image holds nothing but the binary: no shell, no package
# manager, no libc.

# ----------------------------------------------------------------- build
# The release rust-toolchain.toml names, by digest: the compiler CI tests
# with is the one the image ships. tools/toolchain_check.py fails CI when
# the two name different releases, or a FROM here names no digest.
FROM rust:1.99.1-alpine@sha256:44819b27be4c97b0e4d7203bd2e02c4820f97700211b6a37fb4c246a2de59383 AS build

RUN apk add --no-cache musl-dev rsync

WORKDIR /src
# rust-toolchain.toml governs this build as it does every other: it names
# this image's own release, and the wasm32 target it asks for -- which no
# binary here needs, 99 MB of the build stage alone -- is added in a layer
# of its own, kept until the file changes, rather than by rustup at every
# build's first cargo call.
COPY rust-toolchain.toml ./
RUN rustup toolchain install --no-self-update
COPY . .

# On Alpine the host target is already *-linux-musl: a plain `--release`
# gives a static binary, and since we name no target, arm64/amd64 both come
# out of the same Dockerfile. fennec-server is deliberately on the `release` profile
# (unwind): a panicking connection thread takes down only its own session.
# FEATURES: fennec-server's features, e.g. `timing` for `make roundtrip-bench`.
# tools/image-cargo.sh builds in a tree it keeps in the cache mount, by
# content, and holds the binary to this build's sources before it copies it
# out: with target/ alone in the cache, a tree whose files were older than
# the last build's compiled nothing and the image kept the binary before.
# It lands in /built, a path no build before the script copied from: BuildKit
# keyed those COPYs by binaries a stale target/ had made, and could hand a
# build of the same sources another tree's layer (the script says how).
ARG FEATURES=""
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,id=fennec-image-cargo,target=/work,sharing=locked \
    tools/image-cargo.sh /built --release -p fennec-server ${FEATURES:+--features $FEATURES}

# The empty image has no shell to run `mkdir`: the data directory is prepared
# here and copied over together with its ownership.
RUN mkdir -p /data

# --------------------------------------------------------------- runtime
FROM scratch

COPY --from=build /built/fennec-server /usr/local/bin/fennec-server
# 65534's already: a named volume Docker fills from it, and a pod that mounts
# nothing at /data, need no hand-over (below).
COPY --from=build --chown=65534:65534 /data /data

# No USER: the image starts as root, and fennec-server becomes 65534:65534
# itself before it opens a file or starts a thread -- dropping its
# supplementary groups, then its group, then root, and checking root cannot
# be had back. Root is kept that long for one thing: a volume a platform
# mounts (Fly's, Render's, Railway's, a bind mount, Docker's with
# volume-nocopy) is a file system whose root is root's, where a process
# started as 65534 cannot make its file, and this image has no shell to
# chown it with. So the server first hands /data over -- the directory and
# the database's files in it, nothing else, and only where /data is a
# volume of its own holding nothing but the database and a lost+found
# (docs/design/integrations.md#the-images-user) -- as the postgres and redis
# images' entrypoints do in theirs.
#
# An environment variable, not a flag, so it holds when CMD is overridden.
# `--ping` and anything else run in the container (`docker exec`) drop root
# the same way. Started as another user -- `docker run --user 1000:1000`,
# Kubernetes' runAsUser -- the server passes the variable over and runs as
# that user, with the volume that user's already. FENNEC_USER= (empty) keeps
# root, which nothing here needs.
ENV FENNEC_USER=65534:65534

VOLUME /data
EXPOSE 8080

# The binary runs the probe itself: `GET /_health` on 127.0.0.1:8080, which
# answers with no token and takes no lock -- a probe waiting on the lock
# during a long `compact` would report a healthy server as dead. If you
# change the port in CMD you have to add `--http` here as well.
#
# start-period is long: the documents are read before the listener opens
# (vectors the graph lacks are linked beside the queries after it).
HEALTHCHECK --interval=10s --timeout=3s --start-period=120s --retries=3 \
  CMD ["/usr/local/bin/fennec-server", "--ping"]

ENTRYPOINT ["/usr/local/bin/fennec-server"]

# 0.0.0.0 is outside loopback: fennec-server refuses to listen on that address
# without a token. Supply FENNEC_HTTP_TOKEN (or --jwt-secret and --policy),
# or add --insecure deliberately. Overriding CMD drops all of these defaults.
CMD ["--http", "0.0.0.0:8080", "--file", "/data/data.fennec", "--sync", "250"]

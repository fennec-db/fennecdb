# Grafana with fennecdb-datasource built from this checkout: the frontend with
# webpack, the backend with mage for the image's architecture. Built from the
# repository's root, since the backend takes the Go SDK beside it
# (integrations/go):
#
#   docker build -f integrations/grafana/Dockerfile -t fennecdb-grafana .
#
# monitoring/docker-compose.yml builds it as its Grafana. The plugin is not
# signed: GF_PLUGINS_ALLOW_LOADING_UNSIGNED_PLUGINS=fennecdb-datasource lets
# this Grafana load it (integrations/grafana/README.md says how to sign it).

# ----------------------------------------------------------------- frontend
FROM node:24.21.0-alpine AS frontend
WORKDIR /src/integrations/grafana
COPY integrations/grafana/package.json integrations/grafana/package-lock.json integrations/grafana/.npmrc ./
RUN --mount=type=cache,target=/root/.npm npm ci --no-audit --no-fund --loglevel=error
COPY integrations/grafana/ ./
RUN npm run build

# ------------------------------------------------------------------ backend
FROM golang:1.26.5-alpine AS backend
ENV GOTOOLCHAIN=local CGO_ENABLED=0
# proxy.golang.org drops a stream now and then -- "stream error: stream ID 741;
# INTERNAL_ERROR; received from peer" failed CI's monitoring job, a required
# check, on 2026-11-24 -- and neither `go install` nor `go mod download` tries
# again. What a pass fetched stays in the module cache, so the next takes only
# the rest; three passes, 10 and 20 s apart.
RUN --mount=type=cache,target=/root/go/pkg/mod \
    for i in 1 2 3; do go install github.com/magefile/mage@v1.17.2 && exit 0; sleep $((i * 10)); done; exit 1
WORKDIR /src/integrations/grafana
COPY integrations/go/ /src/integrations/go/
COPY integrations/grafana/go.mod integrations/grafana/go.sum ./
RUN --mount=type=cache,target=/root/go/pkg/mod \
    for i in 1 2 3; do go mod download && exit 0; sleep $((i * 10)); done; exit 1
COPY integrations/grafana/Magefile.go integrations/grafana/package.json ./
COPY integrations/grafana/src/plugin.json ./src/
COPY integrations/grafana/pkg/ ./pkg/
ARG TARGETARCH
RUN --mount=type=cache,target=/root/go/pkg/mod --mount=type=cache,target=/root/.cache/go-build \
    case "$TARGETARCH" in arm64) target=build:linuxARM64 ;; *) target=build:linux ;; esac && \
    mage -v "$target"

# ------------------------------------------------------------------ Grafana
FROM grafana/grafana:13.2.2
COPY --from=frontend /src/integrations/grafana/dist/ /var/lib/grafana/plugins/fennecdb-datasource/
COPY --from=backend /src/integrations/grafana/dist/ /var/lib/grafana/plugins/fennecdb-datasource/
ENV GF_PLUGINS_ALLOW_LOADING_UNSIGNED_PLUGINS=fennecdb-datasource

# syntax=docker/dockerfile:1.7

# ─── Stage 1: build UI ────────────────────────────────────────────────────────
FROM node:22-alpine AS ui-builder
WORKDIR /ui

COPY ui/package.json ui/package-lock.json* ./
RUN npm ci

COPY ui/ ./
RUN npm run build

# ─── Stage 2: build daemon ────────────────────────────────────────────────────
FROM rust:1.94-slim-bookworm AS daemon-builder
WORKDIR /build

RUN apt-get update && apt-get install -y --no-install-recommends pkg-config && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock* ./
COPY crates/ ./crates/

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --bin lumen \
    && cp /build/target/release/lumen /lumen

# ─── Stage 3: runtime ─────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 1000 lumen \
    && useradd --system --uid 1000 --gid lumen --home-dir /var/lib/lumen --create-home lumen

COPY --from=daemon-builder /lumen /usr/local/bin/lumen
COPY --from=ui-builder /ui/dist /var/lib/lumen/ui

ENV LUMEN_HTTP_LISTEN=0.0.0.0:3000 \
    LUMEN_UI_ASSETS_DIR=/var/lib/lumen/ui \
    LUMEN_LOG_FORMAT=json \
    RUST_LOG=info,lumen_daemon=info

USER lumen
EXPOSE 3000
WORKDIR /var/lib/lumen

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget --quiet --tries=1 --spider http://127.0.0.1:3000/healthz || exit 1

ENTRYPOINT ["/usr/local/bin/lumen"]

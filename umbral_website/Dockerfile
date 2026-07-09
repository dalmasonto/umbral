# syntax=docker/dockerfile:1
#
# The Umbral website (crates/umbral from crates.io, not from the local workspace).
#
# Build context is `umbral_website/` and nothing outside it — that is why
# Cargo.toml pins `umbral* = "0.0.6"` from the registry instead of path-depping
# `../crates`. A path dep would reach outside the context and the build would fail.
#
# BuildKit cache mounts persist the cargo registry + target/ between builds, so a
# plain `docker compose build` only recompiles what actually changed.
#
# WORKDIR IS LOAD-BEARING AND MUST MATCH ACROSS BOTH STAGES.
# Every website plugin resolves its templates with
#     PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")
# and `env!` bakes the builder's ABSOLUTE path into the binary at compile time.
# Build in /app and run in /app, and `/app/plugins/<name>/templates` is still
# valid at runtime. Build in /build and run in /app, and the image builds fine
# but every plugin-rendered page 500s on the first request. Do not "tidy" these.

FROM rust:1-bookworm AS builder

RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY . .

# build.rs shells out to Tailwind only when styles/node_modules exists. The image
# never ships node, so it takes the lenient branch and uses the committed
# static/css/umbral.css. That is intentional: CSS is a build-time artefact of the
# repo, not of the image.
#
# target/ and the cargo caches are mounts, not layers — copy the binary OUT
# before the mount detaches at the end of this RUN.
RUN --mount=type=cache,target=/app/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --locked \
 && cp target/release/umbral_website /usr/local/bin/umbral_website

# --- slim runtime ---------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# ca-certificates: outbound TLS to the OAuth providers (Google, GitHub).
# libssl3: the OpenSSL runtime the binary links against.
# curl: the compose healthcheck.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl libssl3 \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /usr/local/bin/umbral_website /usr/local/bin/umbral_website

# Site-level templates, resolved RELATIVE to WORKDIR.
COPY templates ./templates

# umbral.toml is deliberately NOT shipped. Settings merge as
# `Toml::file("umbral.toml")` THEN `Env::prefixed("UMBRAL_")`, so env wins — but
# the file declares `environment = "Dev"`, which would override a release
# binary's secure `Prod` default. Baking it in means one forgotten env var
# silently disables Host-header validation and re-permits the dev secret key.
# In the image, /app/.env is the single source of truth.

# Per-plugin templates, resolved via the ABSOLUTE baked CARGO_MANIFEST_DIR.
# This must land at /app/plugins/<name>/templates — see the note at the top.
COPY plugins ./plugins

# `migrate` reads ./migrations/<plugin>/ relative to WORKDIR.
COPY migrations ./migrations

# StoragePlugin::static_files("/static", "./static").
COPY static ./static

# StoragePlugin::media("/media", "./media") — user uploads. Created empty here and
# mounted over by the `media` named volume in compose, so uploads survive a
# rebuild. Without the volume, every deploy would silently drop uploaded images.
RUN mkdir -p ./media

# Bake the environment: umbral auto-loads /app/.env. ENV_FILE is the decrypted
# production env (sops -> .prod.env on deploy). It must set
# UMBRAL_BIND_ADDR=0.0.0.0:9100 — binding 127.0.0.1 inside a container makes the
# published port unreachable from the host.
ARG ENV_FILE=.prod.env
COPY ${ENV_FILE} ./.env

EXPOSE 9100
ENTRYPOINT ["umbral_website"]
CMD ["serve"]

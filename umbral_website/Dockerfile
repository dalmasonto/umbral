# syntax=docker/dockerfile:1
#
# Runtime image for the Umbral website. It COMPILES NOTHING.
#
# CI builds the release binary (see .github/workflows/deploy-website.yml) and
# ships it as `dist/umbral_website`. The server then assembles this image in
# seconds -- a handful of COPY layers -- instead of compiling 468 registry
# crates, which took ~28 minutes on a cold VPS.
#
# The binary MUST be built inside `rust:1-bookworm` with the source at /app:
#
#   * SAME DISTRO. rust:1-bookworm and debian:bookworm-slim are both Debian 12:
#     glibc 2.36, OpenSSL 3. Building on a plain ubuntu-22.04 runner would link
#     glibc 2.35 and merely *happen* to run here. That is luck, not a contract.
#
#   * SAME PATH. Every website plugin resolves its templates with
#         PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")
#     and `env!` freezes the BUILDER's absolute path into the binary's .rodata at
#     compile time. Build at /__w/umbral/umbral/... (a GitHub runner's checkout)
#     and the binary looks for its templates there forever. `--remap-path-prefix`
#     does not help: it rewrites debug info and file!(), not a cargo env var.
#
# To build the binary locally with the same guarantees: bash scripts/build_binary.sh

FROM debian:bookworm-slim

# ca-certificates: outbound TLS to the OAuth providers (Google, GitHub).
# libssl3: the OpenSSL runtime the binary links against.
# curl: the compose healthcheck.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl libssl3 \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY dist/umbral_website /usr/local/bin/umbral_website

# Fail the BUILD, not the first request. If the binary was compiled anywhere
# other than /app, its baked CARGO_MANIFEST_DIR paths point at the build machine
# and every plugin-rendered page returns 500 while the image builds perfectly.
# The literal lives in .rodata and survives `strip`.
RUN chmod +x /usr/local/bin/umbral_website; \
    if ! grep -qa '/app/plugins/public' /usr/local/bin/umbral_website; then \
      echo "FATAL: dist/umbral_website was not compiled with its source at /app."; \
      echo "       Its templates would resolve to the build machine's paths."; \
      echo "       Rebuild with scripts/build_binary.sh (or the CI build job)."; \
      exit 1; \
    fi

# Site-level templates, resolved RELATIVE to WORKDIR.
COPY templates ./templates

# Per-plugin templates, resolved via the ABSOLUTE baked CARGO_MANIFEST_DIR.
# These must land at /app/plugins/<name>/templates -- see the note at the top.
COPY plugins ./plugins

# `migrate` reads ./migrations/<plugin>/ relative to WORKDIR.
COPY migrations ./migrations

# StoragePlugin::static_files("/static", "./static").
COPY static ./static

# StoragePlugin::media("/media", "./media") -- user uploads. Created empty and
# mounted over by the `media` named volume, so uploads survive a rebuild.
RUN mkdir -p ./media

# umbral.toml is deliberately NOT shipped: it declares `environment = "Dev"`,
# which would override a release binary's secure Prod default. In the image,
# /app/.env is the single source of truth.
#
# .prod.env never leaves the server -- CI builds only the binary, so production
# secrets are baked here, on the box, exactly as before.
ARG ENV_FILE=.prod.env
COPY ${ENV_FILE} ./.env

EXPOSE 9100
ENTRYPOINT ["umbral_website"]
CMD ["serve"]

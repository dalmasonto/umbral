#!/usr/bin/bash

# Build dist/umbral_website the same way CI does, so `docker compose build` works
# locally and produces a byte-for-byte-equivalent runtime image.
#
#   bash scripts/build_binary.sh
#
# Why this exists instead of a plain `cargo build --release`:
#
#   * The source must be compiled AT /app. Every website plugin resolves its
#     templates with `PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")`,
#     and `env!` freezes the builder's absolute path into the binary. A binary
#     built in your checkout looks for templates in your checkout -- forever.
#     The runtime image runs at /app, so the binary must have been built at /app.
#
#   * It must be compiled in rust:1-bookworm. The runtime image is
#     debian:bookworm-slim; both are Debian 12 (glibc 2.36, OpenSSL 3). A binary
#     built against another distro's glibc may load here by accident, or not.
#
# The Dockerfile asserts the first of these with a `grep` on the binary, so a
# wrongly-built binary fails the image build rather than 500ing at request time.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR/.."

cd "$PROJECT_ROOT"
mkdir -p dist

echo "Building umbral_website in rust:1-bookworm at /app ..."

# The source is copied to /app inside the container rather than bind-mounted at
# /app, because cargo would otherwise write target/ straight into your checkout
# and the host/container UIDs differ. The cargo caches ARE bind-mounted (named
# volumes) so repeat builds are incremental.
docker run --rm \
  -v "$PROJECT_ROOT":/src:ro \
  -v umbral_website_cargo_registry:/usr/local/cargo/registry \
  -v umbral_website_build_target:/app/target \
  -v "$PROJECT_ROOT/dist":/out \
  rust:1-bookworm \
  bash -euo pipefail -c '
    apt-get update -qq && apt-get install -y -qq --no-install-recommends pkg-config libssl-dev >/dev/null
    mkdir -p /app
    # -a preserves times so cargo fingerprints stay valid across runs.
    # target/ is a mount; exclude it from the copy. styles/node_modules is dead
    # weight (build.rs skips Tailwind when it is absent and uses the committed CSS).
    cp -a /src/. /app/ 2>/dev/null || true
    rm -rf /app/styles/node_modules
    cd /app
    cargo build --release --locked
    strip target/release/umbral_website
    cp target/release/umbral_website /out/umbral_website
  '

echo
echo "Verifying the binary baked /app paths (not a host checkout) ..."
found=$(grep -a -o -E "/app/plugins/[a-z_]+" dist/umbral_website | sort -u | wc -l)
if [ "$found" -lt 10 ]; then
    echo "FATAL: only $found /app/plugins/* paths baked in; expected 10."
    echo "       The binary was not compiled at /app. Refusing to ship it."
    exit 1
fi
echo "  OK: $found plugin manifest dirs resolve under /app"
echo
ls -lh dist/umbral_website
echo
echo "Now: docker compose build && docker compose up -d"

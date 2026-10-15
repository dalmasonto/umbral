#!/usr/bin/env bash
# Resolve and run k6 for the bench harness — no global install required.
#
# Resolution order:
#   1. `k6` on PATH
#   2. ~/.local/bin/k6            (user-local install; see README)
#   3. docker run grafana/k6      (fallback when docker is present)
#
# Usage (same args as k6, defaults point at the harness backend):
#   harness/k6/run.sh run harness/k6/journeys.js
#   ONLY=profile VUS=300 harness/k6/run.sh run harness/k6/single-endpoints.js
set -euo pipefail

if command -v k6 >/dev/null 2>&1; then
  K6=(k6)
elif [ -x "$HOME/.local/bin/k6" ]; then
  K6=("$HOME/.local/bin/k6")
elif command -v docker >/dev/null 2>&1; then
  # --network host so BASE=http://127.0.0.1:8113 reaches the host's backend.
  exec docker run --rm -i --network host \
    -e BASE -e USERS -e POSTS -e VUS -e ONLY -e DURATION -e READERS -e WRITERS -e LURKERS \
    -v "$PWD:/scripts:ro" -w /scripts \
    grafana/k6 "$@"
else
  echo "k6 not found. Install it (see harness/k6/README.md):" >&2
  echo "  curl -sL https://github.com/grafana/k6/releases/latest/download/\\" >&2
  echo "    k6-\$(curl -s https://api.github.com/repos/grafana/k6/releases/latest | grep -oP 'v[0-9.]+' | head -1)-linux-amd64.tar.gz | tar xz" >&2
  echo "  install -m755 k6-*-linux-amd64/k6 ~/.local/bin/k6" >&2
  exit 1
fi

exec "${K6[@]}" "$@"

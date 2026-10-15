#!/usr/bin/env bash
# Capture golden responses from a running harness binary.
#
# Usage: capture.sh <port> <outdir>
#
# Records, per route: HTTP status, the Server-Timing metric NAMES (not the
# durations — those are nondeterministic), and a sha256 of the response body.
# Volatile fields (ids, timestamps) are normalised out before hashing so the
# capture is stable across runs.
set -euo pipefail

PORT="${1:?usage: capture.sh <port> <outdir>}"
OUT="${2:?usage: capture.sh <port> <outdir>}"
BASE="http://127.0.0.1:${PORT}"
mkdir -p "$OUT"

# Route + the x-bench-user id it needs (empty = none).
ROUTES=(
  "/api/ping|"
  "/api/feed|"
  "/api/feed?limit=5|"
  "/api/feed-naive?limit=5|"
  "/api/users/1/profile|"
  "/|"
  "/u/1|"
  "/posts/1|"
)

for entry in "${ROUTES[@]}"; do
  route="${entry%%|*}"
  user="${entry##*|}"
  name="$(echo "$route" | tr '/?=&' '____' | sed 's/^_*//')"
  [ -z "$name" ] && name="root"

  hdr=$(mktemp); body=$(mktemp)
  if [ -n "$user" ]; then
    code=$(curl -s -o "$body" -D "$hdr" -w '%{http_code}' \
      -H "x-bench-user: $user" "${BASE}${route}")
  else
    code=$(curl -s -o "$body" -D "$hdr" -w '%{http_code}' "${BASE}${route}")
  fi

  # Server-Timing metric NAMES only, sorted — durations vary run to run.
  # Quoted parameter values are dropped BEFORE the comma split: the db entry's
  # desc is `desc="orm wall, statements global"`, whose comma would otherwise
  # invent a bogus name and leave a stray quote that breaks the JSON below.
  timing=$(grep -i '^server-timing:' "$hdr" \
    | sed 's/^[Ss]erver-[Tt]iming: *//' \
    | sed 's/"[^"]*"//g' \
    | tr ',' '\n' | sed 's/;.*//' | tr -d ' \r' | sort | paste -sd, -)

  # Normalise volatile values before hashing.
  norm=$(sed -E \
    -e 's/"id":[0-9]+/"id":0/g' \
    -e 's/"created_at":"[^"]*"/"created_at":"X"/g' \
    -e 's/[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z/X/g' \
    "$body")

  sha=$(printf '%s' "$norm" | sha256sum | cut -d' ' -f1)
  printf '{"status":%s,"timing_keys":"%s","body_sha256":"%s"}\n' \
    "$code" "$timing" "$sha" > "$OUT/${name}.json"
  echo "  ${route} -> status=${code} sha=${sha:0:12}"
  rm -f "$hdr" "$body"
done
echo "captured to $OUT"

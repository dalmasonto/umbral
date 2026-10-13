#!/usr/bin/env bash
# Build bench/session_pool.json by logging the pool users in for real.
#
# Each entry is the value of the `umbral_session` cookie the login returned.
# k6 reads this via SharedArray, exactly as the twelve-dollar challenge reads
# its seed/tokens.json — real sessions, built once, verified per request.
#
# The pool USERS are not created here; the binary owns that, because only it
# has the password-hashing API. The one rule is that they must exist in the
# database the SERVED binary is using: sessions are rows in that database's
# `session` table (the real DbStore), so a pool built against one database
# authenticates nothing against another. Both halves therefore run where the
# served database lives. With the silo (port 8114, /data/bench-run.db):
#
#   harness/bench/run.sh serve
#   docker exec umbral-bench /app/umbral-bench-prod seed-sessions
#   harness/bench/seed-sessions.sh 8114 200
#
# Do NOT run the seeder on the host with no database override: it then defaults
# to harness/backend/bench.db — the golden gate's fixture — and the logins
# below would be minted against a different file from the one being served.
# Override UMBRAL_DATABASE_URL to the served database if you must run it
# host-side.
#
# Usage: seed-sessions.sh [port] [pool_size]
#
# Exits non-zero WITHOUT writing the pool unless every single login returned
# 200 with a session cookie. A partial pool is worse than no pool: it looks
# like success and then quietly stops authenticating part of the load, and the
# resulting `http_req_failed` reads as a server problem. A 401 here means the
# user is missing or its password differs (re-run `seed-sessions`); a 429 means
# the login throttle tripped (5 failures per IP+username per 5 minutes) —
# which, on a first pass over N distinct users, means the seeding is wrong,
# not that the server is busy.
set -euo pipefail

PORT="${1:-8114}"
N="${2:-200}"
BASE="http://127.0.0.1:${PORT}"
OUT="$(cd "$(dirname "$0")" && pwd)/session_pool.json"
PASSWORD="${BENCH_PASSWORD:-bench-password-123}"

# Validate the size before anything else touches the filesystem. `seq 1 0`
# yields nothing and `seq 1 abc` errors, so an unchecked size would let the
# loop run zero times and be read as a clean run — writing `[]` over a good
# pool and calling it success.
if ! [[ "$N" =~ ^[0-9]+$ ]] || [ "$N" -lt 1 ]; then
  echo "ERROR: pool size must be a positive integer (got '$N')." >&2
  exit 2
fi

echo "logging in $N pooled users against $BASE ..."
tmp=$(mktemp)
hdr=$(mktemp)
body=$(mktemp)
trap 'rm -f "$tmp" "$hdr" "$body"' EXIT

printf '[' > "$tmp"
ok=0
failed=0
for i in $(seq 1 "$N"); do
  # `|| status=000`: an unreachable server must land in the same failure path
  # as a rejected login, not abort the loop with curl's exit code and no
  # explanation. `--max-time` bounds a hung server for the same reason.
  status=$(curl -s --max-time 10 -o "$body" -D "$hdr" -w '%{http_code}' \
    -X POST "$BASE/api/auth/login" \
    -H 'content-type: application/json' \
    -d "{\"username\":\"bench_$i\",\"password\":\"$PASSWORD\"}") || status="000"
  # Read the cookie from the header block, never from the whole response: a
  # body that happened to quote a Set-Cookie line must not be mistaken for one.
  # `|| true`: a response with no Set-Cookie makes grep exit 1, and pipefail
  # would otherwise abort the whole run here instead of counting the failure.
  cookie=$(grep -i '^set-cookie: umbral_session=' "$hdr" \
    | sed -E 's/^[^:]*:[[:space:]]*umbral_session=([^;]+).*/\1/' | tr -d '\r' || true)
  # The 200 is load-bearing. A failed login can still carry a Set-Cookie — a
  # session layer is free to hand out an anonymous session on any response —
  # and accepting that would put a cookie in the pool that never
  # authenticated anything.
  if [ "$status" != "200" ] || [ -z "$cookie" ]; then
    echo "  user bench_$i: FAILED (status=$status, cookie=$([ -n "$cookie" ] && echo present || echo absent))" >&2
    failed=$((failed + 1))
    continue
  fi
  [ "$ok" -gt 0 ] && printf ',' >> "$tmp"
  printf '"%s"' "$cookie" >> "$tmp"
  ok=$((ok + 1))
done
printf ']' >> "$tmp"

if [ "$failed" -ne 0 ]; then
  echo "ERROR: $failed of $N logins failed — no pool written." >&2
  echo "       Re-run 'seed-sessions' against this database before retrying." >&2
  exit 1
fi

# Second gate, independent of the one above: whatever the counter arithmetic
# does, an empty pool is never written. `[]` would sail through k6's
# SharedArray and fail every authenticated request at run time, and it would
# have replaced a good pool to get there.
if [ "$ok" -eq 0 ]; then
  echo "ERROR: no sessions obtained — no pool written." >&2
  echo "       Is the pool seeded in the database this server is using, and is" >&2
  echo "       the login path right? See the header for the seeder command." >&2
  exit 1
fi

mv "$tmp" "$OUT"
echo "wrote $ok sessions to $OUT"

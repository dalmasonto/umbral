#!/usr/bin/env bash
# Build bench/session_pool.json by logging the pooled users in for real.
#
# Each entry is the value of the `umbral_session` cookie the login returned.
# k6 reads this via SharedArray, exactly as the twelve-dollar challenge reads
# its seed/tokens.json — real sessions, built once, verified per request.
#
# The pool holds one session per DATASET USER (20,000), SHUFFLED: k6 pins every
# VU to one pooled session (one VU = one client, bench/workload.md), so a pool
# smaller than the VU count would hand several concurrent VUs the same identity
# — identity sharing that `k6/lib.js` refuses to start a run with — and the
# positional mapping (VU i -> sessions[i-1]) means the pool's ORDER decides
# which identities a run activates. Minting in login order would make the
# active set the lowest user ids, the same contiguous slice in every run;
# shuffling makes it a random sample of the user base, so the session working
# set and last-seen write set spread across the dataset the way real traffic
# spreads them. The shuffled file IS the assignment record, and it is kept
# fixed across runs so Phase 3 comparisons stay comparable.
#
# The pool USERS are the dataset's own rows (`bench_user_1..bench_user_N`), not
# a parallel namespace: the seeder inside the container gives the first N
# dataset users the pool password, and this script logs them in. Nothing is
# created — `auth_user` keeps its row count and id range. The one rule is that
# the users must exist in the database the SERVED binary is using: sessions are
# rows in that database's `session` table (the real DbStore), so a pool built
# against one database authenticates nothing against another. Both halves
# therefore run where the served database lives. With the silo (port 8114,
# /data/bench-run.db):
#
#   harness/bench/run.sh serve
#   docker exec umbral-bench /app/umbral-bench-prod seed-sessions
#   harness/bench/seed-sessions.sh 8114 20000
#
# The seeder's size and this script's N are two halves of ONE pool and must be
# the same number: the seeder gives users 1..BENCH_SESSION_POOL_SIZE their
# password, this script logs users 1..N in. When BENCH_SESSION_POOL_SIZE is
# set in this script's environment and disagrees with the argument, the script
# refuses (exit 2) rather than build half a pool.
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
# user's stored hash is not the pool password (re-run `seed-sessions` with
# BENCH_SESSION_POOL_SIZE covering this script's N — a user past the seeder's
# size still holds the dataset's placeholder hash); a 429 means the login
# throttle tripped (5 failures per IP+username per 5 minutes) — which, on a
# first pass over N distinct users, means the seeding is wrong, not that the
# server is busy.
set -euo pipefail

PORT="${1:-8114}"
N="${2:-${BENCH_SESSION_POOL_SIZE:-20000}}"
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

# The two halves of the pool must agree. BENCH_SESSION_POOL_SIZE is what the
# seeder inside the container reads (`docker exec -e
# BENCH_SESSION_POOL_SIZE=N umbral-bench ... seed-sessions`); this script's N is
# how many of those users get logged in. Both numbers visible and different is
# a misconfiguration caught here, for free, before either half runs.
if [ -n "${BENCH_SESSION_POOL_SIZE:-}" ] && [ "$N" != "$BENCH_SESSION_POOL_SIZE" ]; then
  echo "ERROR: pool size disagreement — BENCH_SESSION_POOL_SIZE=$BENCH_SESSION_POOL_SIZE," >&2
  echo "       but this script was asked for $N. Seeder and script build ONE pool:" >&2
  echo "       the seeder gives users 1..BENCH_SESSION_POOL_SIZE their password, this" >&2
  echo "       script logs users 1..N in. If N is larger, the logins past the seeder's" >&2
  echo "       size 401 (those users still hold the dataset's placeholder hash); if N" >&2
  echo "       is smaller, the difference is seeded but never used. Pass the same" >&2
  echo "       number to both:" >&2
  echo "         docker exec -e BENCH_SESSION_POOL_SIZE=$N umbral-bench /app/umbral-bench-prod seed-sessions" >&2
  echo "         BENCH_SESSION_POOL_SIZE=$N $0 $PORT $N" >&2
  echo "       (or unset BENCH_SESSION_POOL_SIZE and let the argument stand alone)." >&2
  exit 2
fi

echo "logging in $N pooled users (bench_user_1..bench_user_$N) against $BASE ..."
tmp=$(mktemp)
hdr=$(mktemp)
body=$(mktemp)
shuffled="$(mktemp)"
trap 'rm -f "$tmp" "$hdr" "$body" "$shuffled"' EXIT

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
    -d "{\"username\":\"bench_user_$i\",\"password\":\"$PASSWORD\"}") || status="000"
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
    echo "  user bench_user_$i: FAILED (status=$status, cookie=$([ -n "$cookie" ] && echo present || echo absent))" >&2
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
  echo "       Re-run 'seed-sessions' against this database, sized to cover all $N" >&2
  echo "       users (BENCH_SESSION_POOL_SIZE=$N), then retry." >&2
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

# The pool's ORDER is the assignment: k6 maps VU i to sessions[i-1], so the
# first `vus` entries are the identities a run activates. Login order would
# make that the lowest user ids in every run — a contiguous slice of the user
# table, always the same one; shuffling makes it a random sample of the user
# base. `shuf` works on lines, so the array is flattened and rebuilt; every
# element is a session UUID (no newlines, no whitespace, no JSON metacharacters).
jq -r '.[]' "$tmp" | shuf | jq -R -s -c 'split("\n") | map(select(length > 0))' > "$shuffled"

# Prove the shuffle wrote what the counters say: a shuffle that silently
# dropped or duplicated entries would be a pool nothing downstream could catch.
shuffled_n="$(jq 'length' "$shuffled")"
if [ "$shuffled_n" -ne "$ok" ]; then
  echo "ERROR: the shuffle produced $shuffled_n entries for $ok sessions — no pool written." >&2
  exit 1
fi

mv "$shuffled" "$OUT"
echo "wrote $ok sessions to $OUT (shuffled)"

#!/usr/bin/env bash
# compare.sh — diff two result files, or refuse to.
#
# Usage: compare.sh <before.json> <after.json>
#
# The refusals are the point. Two numbers measured against different datasets,
# inside different container envelopes, on different DATABASE BACKENDS, or
# under a different workload mix are not two samples of the same thing —
# subtracting them produces a delta that does not mean what it looks like, and
# a delta that does not mean anything is worse than no comparison. So this
# script refuses (exit 3) rather than comparing, and it also refuses when
# either file is not a load result at all (a k6 preflight abort, exit 107,
# measures nothing about the server).
#
# It fails CLOSED: anything it cannot read, parse, or establish comparability
# for is a non-zero exit, never a silent pass.
#
# Exit codes:
#   0  comparable, every thresholded metric within its limit
#   1  comparable, but a metric moved beyond its limit (see the verdict lines)
#   2  usage: cannot read or parse one of the files
#   3  refused: these results are not comparable
#
# Thresholds, from the plan:
#   rps     moves more than 10% in either direction   -> flagged
#   p99_ms  moves more than 20% in either direction   -> flagged
# "Either direction" is deliberate: a run that got 30% faster is as much a
# change as one that got 30% slower, and this script exists to make changes
# visible, not to bless improvements. The verdict line says which it was.
set -euo pipefail

usage() {
  echo "usage: compare.sh <before.json> <after.json>" >&2
}

die() { # die <exit-code> <message...>
  local code="$1"; shift
  echo "ERROR: $*" >&2
  exit "$code"
}

# Which DATABASE BACKEND a result was measured against, from the file alone.
#
# The fact record.sh carries is the URL the container was started with, read
# out of the container's own environment (`docker inspect`, so it is what the
# container is actually serving, not what someone meant to pass), recorded as
# `wal.db_url`. There is no separate `backend` key; the scheme IS the backend.
#
# Deriving it from the SCHEME and not the whole URL is deliberate: two runs of
# the same backend legitimately differ in path, database name and port, and
# refusing those would make this comparator useless. Only the scheme names the
# architecture — SQLite is a function call inside the measured process,
# Postgres is a TCP round trip to another container — so only the scheme is
# compared. `postgres` and `postgresql` are one server under two spellings and
# canonicalise to one name; an unknown scheme is echoed as-written, so two
# different unknowns still refuse rather than silently matching.
#
# Prints the empty string when the file names no database URL at all, which
# the caller treats as "backend unknown" and refuses on: a result whose
# backend cannot be established is comparable to nothing.
db_backend() { # db_backend <result.json>
  local url scheme
  url="$(jq -r '.wal.db_url // ""' "$1" 2>/dev/null || echo '')"
  case "$url" in
    *://*) ;;
    *) echo ""; return ;;
  esac
  scheme="${url%%://*}"
  scheme="$(printf '%s' "$scheme" | tr '[:upper:]' '[:lower:]')"
  case "$scheme" in
    sqlite) echo "sqlite" ;;
    postgres|postgresql) echo "postgres" ;;
    *) echo "$scheme" ;;
  esac
}

case "${1:-}" in -h|--help) usage; exit 0;; esac
[ "$#" -eq 2 ] || { usage; exit 2; }

A="$1"
B="$2"

command -v jq >/dev/null 2>&1 || die 2 "jq is required (apt install jq)."

for f in "$A" "$B"; do
  [ -r "$f" ] || die 2 "cannot read '$f'."
  jq -e . "$f" >/dev/null 2>&1 || die 2 "'$f' is not valid JSON."
  # A file with no `schema` was not written by record.sh; guessing its shape is
  # how a comparator blesses garbage.
  jq -e 'has("schema")' "$f" >/dev/null 2>&1 \
    || die 2 "'$f' is not a result file written by record.sh (no \"schema\" key)."
  jq -e '.schema == 1' "$f" >/dev/null 2>&1 \
    || die 2 "'$f' has schema $(jq -c '.schema' "$f"), which this comparator does not know."
done

# ---------------------------------------------------------------------------
# Refusals — before any number is compared
# ---------------------------------------------------------------------------
for f in "$A" "$B"; do
  class="$(jq -r '.exit.class // "missing"' "$f")"
  case "$class" in
    passed|threshold_failed) ;;
    preflight_abort)
      die 3 "'$f' is a preflight abort (k6 exit 107), not a load result: the run never issued
       its load, so there is nothing here to compare. It is a rig-configuration
       failure — fix the rig (id ranges, session pool) and re-run." ;;
    *)
      die 3 "'$f' has exit class '$class', which is not a load result (only 'passed' and
       'threshold_failed' are). An unclassified failure is not a comparable sample." ;;
  esac
  for key in rps p99_ms dataset_scale mix; do
    jq -e --arg k "$key" 'has($k) and .[$k] != null' "$f" >/dev/null 2>&1 \
      || die 3 "'$f' has no $key (or it is null): the comparison facts are incomplete."
  done
  jq -e '.rps | type == "number"' "$f" >/dev/null 2>&1 || die 3 "'$f': rps is not a number."
  jq -e '.p99_ms | type == "number"' "$f" >/dev/null 2>&1 || die 3 "'$f': p99_ms is not a number."
  jq -e '.envelope.cpuset? != null' "$f" >/dev/null 2>&1 \
    || die 3 "'$f' records no envelope.cpuset: an unverified envelope is not comparable."
  # The backend is the outermost fact of all — it decides whether the database
  # is in the measured process or across a network hop — so a file that does
  # not name one is not comparable to anything, not even to another such file.
  [ -n "$(db_backend "$f")" ] \
    || die 3 "'$f' names no database URL (wal.db_url is absent, empty, or has no scheme),
       so the backend it was measured against cannot be established. An unknown
       backend is not comparable to anything. Re-record it with record.sh."
done

backend_a="$(db_backend "$A")"
backend_b="$(db_backend "$B")"
if [ "$backend_a" != "$backend_b" ]; then
  die 3 "the DATABASE BACKEND differs: $A is '$backend_a', $B is '$backend_b' (from each
       result's recorded database URL). SQLite's database is a function call in the
       measured process; Postgres's is a TCP round trip to a sibling container. Neither
       is better — they are different architectures with different envelopes, and a
       delta between them is the architecture, not the code. The only meaningful
       comparisons are sqlite-vs-sqlite and postgres-vs-postgres across code changes:
       compare each result with one from its own backend."
fi

if ! jq -en --slurpfile a "$A" --slurpfile b "$B" '$a[0].dataset_scale == $b[0].dataset_scale' >/dev/null; then
  die 3 "dataset_scale differs: $A is $(jq -r '.dataset_scale' "$A"), $B is $(jq -r '.dataset_scale' "$B").
       A result is about a dataset at a scale; the same code against a bigger
       dataset is a different measurement. Re-measure both at the same scale."
fi
if ! jq -en --slurpfile a "$A" --slurpfile b "$B" '$a[0].envelope.cpuset == $b[0].envelope.cpuset' >/dev/null; then
  die 3 "the container cpuset differs: $(jq -r '.envelope.cpuset' "$A") vs $(jq -r '.envelope.cpuset' "$B").
       Different CPU sets are different machines."
fi
if ! jq -en --slurpfile a "$A" --slurpfile b "$B" '$a[0].mix == $b[0].mix' >/dev/null; then
  die 3 "the workload mix differs:
       $A: $(jq -c '.mix' "$A")
       $B: $(jq -c '.mix' "$B")
       The mix (including think time) is what 'fast' means for this rig; a
       comparison across a mix change is not a comparison."
fi
if ! jq -en --slurpfile a "$A" --slurpfile b "$B" '$a[0].rps > 0' >/dev/null; then
  die 3 "$A has rps = $(jq -r '.rps' "$A"): a relative change from zero is undefined."
fi

# ---------------------------------------------------------------------------
# The comparison
# ---------------------------------------------------------------------------
# One jq program emits the rows so the arithmetic and the verdicts live in one
# place; bash only formats. Layout: metric \t a \t b \t change \t verdict.
report="$(jq -rn --slurpfile a "$A" --slurpfile b "$B" '
  ($a[0]) as $x | ($b[0]) as $y |
  def fmt3: if . == null then "n/a" else ((. * 1000 | round) / 1000 | tostring) end;
  # null-safe: p95/error_rate are informational, and an older or hand-made file
  # may not carry them — a missing value must print as n/a, not crash the diff.
  def pct($p; $q):
    if $p == null or $q == null then null
    elif $p == 0 then null
    else ((($q - $p) / $p) * 100) end;
  def pctstr($p): if $p == null then "n/a"
                  else (if $p >= 0 then "+" else "" end)
                       + ((($p * 100 | round) / 100) | tostring) + "%" end;
  # `bad` is the change expressed so that positive is worse: rps falling is bad,
  # latency rising is bad. The limit is in the same signed units.
  def verdict($bad; $limit):
        if $bad > $limit then "REGRESSION (limit \($limit)%)"
        elif $bad < (0 - $limit) then "IMPROVEMENT (limit \($limit)%)"
        else "ok (limit \($limit)%)" end;

  (pct($x.rps; $y.rps)) as $rps_pct |
  (pct($x.p99_ms; $y.p99_ms)) as $p99_pct |
  ([
     ["rps",        ($x.rps | fmt3),               ($y.rps | fmt3),               (pctstr($rps_pct)), (if $rps_pct == null then "n/a" else verdict((0 - $rps_pct); 10) end)],
     ["p95_ms",     ($x.p95_ms | fmt3),            ($y.p95_ms | fmt3),            (pctstr(pct($x.p95_ms; $y.p95_ms))), "ok (no limit)"],
     ["p99_ms",     ($x.p99_ms | fmt3),            ($y.p99_ms | fmt3),            (pctstr($p99_pct)),  (if $p99_pct == null then "n/a" else verdict($p99_pct; 20) end)],
     ["error_rate", (($x.error_rate // null) | fmt3), (($y.error_rate // null) | fmt3), (pctstr(pct($x.error_rate; $y.error_rate))), "ok (no limit)"]
   ] | .[] | @tsv),
  # A single boolean for the exit code: any thresholded metric beyond its limit.
  (if ($rps_pct != null and ($rps_pct > 10 or $rps_pct < -10)) or
      ($p99_pct != null and ($p99_pct > 20 or $p99_pct < -20))
   then "BREACH" else "CLEAN" end)
')"

echo "compare  $A  ->  $B"
printf '  a  %s  %s   vus %s   scale %s\n' \
  "$(jq -r '.git_sha_short' "$A")" "$(jq -r '.verdict' "$A")" "$(jq -r '.vus' "$A")" "$(jq -r '.dataset_scale' "$A")"
printf '  b  %s  %s   vus %s   scale %s\n' \
  "$(jq -r '.git_sha_short' "$B")" "$(jq -r '.verdict' "$B")" "$(jq -r '.vus' "$B")" "$(jq -r '.dataset_scale' "$B")"
echo

while IFS=$'\t' read -r metric va vb change verdict; do
  case "$metric" in ''|BREACH|CLEAN) continue;; esac
  printf '  %-12s %14s -> %-14s %9s   %s\n' "$metric" "$va" "$vb" "$change" "$verdict"
done <<<"$report"
echo

# ---------------------------------------------------------------------------
# Context: what could explain a movement that is not the code
# ---------------------------------------------------------------------------
ctx() { printf '  %-12s %s\n' "$1" "$2"; }
echo "context"

# A failed check means the run measured a path it did not intend to (a 404
# where a page should be), so a failing run's check names are printed, not just
# a count — the numbers beside it are still compared, but the reader is told.
check_summary() {
  jq -r '
    if .checks.held == null then "n/a"
    else
      (if .checks.held then "held" else "FAILED" end)
      + " (\(.checks.totals.passes // 0)/\((.checks.totals.passes // 0) + (.checks.totals.fails // 0)))"
      + ((.checks.per_check // []) | map(select(.fails > 0) | "\(.name) (\(.fails) fails)") | if length == 0 then "" else ": " + join(", ") end)
    end' "$1"
}
ctx "checks" "a $(check_summary "$A")   b $(check_summary "$B")"

jq -en --slurpfile a "$A" --slurpfile b "$B" '$a[0].host.hostname == $b[0].host.hostname' >/dev/null || ctx "host" "$(jq -r '.host.hostname' "$A") vs $(jq -r '.host.hostname' "$B") — different machines"

# The provenance is part of the number: an in-run peak and a post-run sample
# are different kinds of fact, and a stale sample is not a fact about its run at
# all. Printing one blanket caption over both would be the same mistake the
# store exists to prevent, one level up.
thermal_summary() {
  jq -r '
    if .thermal == null then "n/a"
    elif .thermal.source == "sampler" then "IN-RUN peak \(.thermal.peak_c) C (\(.thermal.window_s)s window)"
    elif .thermal.stale == true then "post-run sample \(.thermal.peak_c) C, +\(.thermal.sampled_after_run_s)s (STALE: not evidence about the run)"
    elif (.thermal.sampled_after_run_s // null) == null then "peak \(.thermal.peak_c) C (provenance not recorded)"
    else "post-run sample \(.thermal.peak_c) C, +\(.thermal.sampled_after_run_s)s (cooling curve, below the in-run peak)"
    end' "$1"
}
ctx "thermal" "a $(thermal_summary "$A")   b $(thermal_summary "$B")"

if jq -en --slurpfile a "$A" --slurpfile b "$B" '$a[0].host.hostname == $b[0].host.hostname' >/dev/null; then
  ctx "throttling" "$(jq -n --slurpfile a "$A" --slurpfile b "$B" -r '
    ($a[0].throttling // {}) as $t1 | ($b[0].throttling // {}) as $t2 |
    if ($t1.core_throttle_count == null or $t2.core_throttle_count == null) then "n/a"
    else "cumulative core events \($t1.core_throttle_count) -> \($t2.core_throttle_count) (\($t2.core_throttle_count - $t1.core_throttle_count) since a), package \($t1.package_throttle_count) -> \($t2.package_throttle_count) (\($t2.package_throttle_count - $t1.package_throttle_count) since a); a nonzero delta means the CPU throttled between the two records" end')"
fi

if ! jq -en --slurpfile a "$A" --slurpfile b "$B" '$a[0].vus == $b[0].vus' >/dev/null; then
  ctx "vus" "DIFFERENT ($(jq -r '.vus' "$A") vs $(jq -r '.vus' "$B")) — the offered load differs, so an rps change is not a like-for-like movement"
fi

wa="$(jq -r '(.warnings // []) | length' "$A")"; wb="$(jq -r '(.warnings // []) | length' "$B")"
if [ "$wa" -gt 0 ]; then ctx "warnings" "a has $wa recorded (jq .warnings $A)"; fi
if [ "$wb" -gt 0 ]; then ctx "warnings" "b has $wb recorded (jq .warnings $B)"; fi

echo
case "$report" in
  *BREACH*)
    echo "verdict  BREACH: at least one metric moved beyond its limit (see the verdict lines above)."
    exit 1
    ;;
esac
echo "verdict  no thresholded metric moved beyond its limit"
exit 0

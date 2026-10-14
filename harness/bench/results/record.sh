#!/usr/bin/env bash
# record.sh — turn one k6 run into a result a stranger can judge six weeks later.
#
# Every number this rig quotes is supposed to have come through here. A result
# is not the number; it is the number PLUS every fact needed to decide whether
# the number means anything: which commit and which binary, what container
# envelope, what dataset scale, what workload mix, whether the machine was
# throttling, whether the container was OOM-killed, whether the check results
# actually held. `compare.sh` is the other half: it refuses to diff two results
# whose facts disagree.
#
# Usage:
#   record.sh <summary-export.json> <vus> <dataset_scale> <k6-exit-code>
#   record.sh --thermal-sample [file]        # start before a hold; stop with SIGTERM
#
# Example (the Task 9 baseline flow, with the in-run thermal window):
#   harness/bench/results/record.sh --thermal-sample &
#   SAMPLER=$!
#   taskset -c 8,9,10,11 ~/.local/bin/k6 run -e VUS=2500 --summary-export=/tmp/hold-1.json \
#     harness/bench/k6/hold.js; echo "exit=$?"
#   kill "$SAMPLER"; wait "$SAMPLER"
#   harness/bench/results/record.sh /tmp/hold-1.json 2500 20000 $?
#
# `--thermal-sample` writes a ~1 Hz thermal/load trace until it is stopped. The
# normal invocation then reads the run's PEAK out of that file when the file
# covers the run. Without it there is no in-run temperature at all, and the
# recorder says so in the result: this host keeps no thermal history, and a
# window sampled after the fact describes whatever the machine was doing at
# record time — which may be more or less than what the run did. The
# post-run window survives only as a clearly-labelled, lag-guarded fallback.
#
# It writes results/<git-sha>-<utc>.json and prints the path. It REFUSES to
# write anything when the envelope cannot be verified (no container, or a
# cpuset different from the one run.sh asserts) — a result from an unverified
# envelope is not a result. It also refuses when its arguments are unusable or
# the summary export is missing, because none of those produce a recordable
# measurement.
#
# Exit codes of THIS script (not of k6):
#   0  result written
#   2  bad invocation: unusable arguments, no/unreadable summary export, no jq
#   3  refused: the container envelope cannot be verified (no docker daemon, no
#      container named umbral-bench, or a cpuset that does not match)
#
# The k6 exit code is a PARAMETER, recorded as a classified fact rather than
# folded into a boolean:
#   0    thresholds held                     -> passed
#   99   a threshold failed (latency/errors) -> threshold_failed, a REAL load result
#   107  aborted in setup()/preflight        -> not a load result at all
# Anything else is unclassified and recorded as such. Filing a 107 as a load
# result would blame umbral for a typo'd `-e USERS=`, and the two are read
# weeks apart by someone who was not there when the run happened.
#
# Two facts the brief asks for are only readable from DURING the run, so they
# come from the sampler when one was run, and are recorded as explicitly
# labelled substitutes when it was not:
#   * "peak thermal-zone temperature DURING the run" — from the sampler file's
#     peak when it covers the run. The fallback is a ~10 s post-run window, and
#     the result says which it is (`thermal.source`), how far after the run it
#     was taken (`thermal.sampled_after_run_s`), and — past a few tens of
#     seconds, where the reading stops being evidence about the run at all —
#     that it must not be used to explain a run-to-run difference
#     (`thermal.stale: true`, plus a warning).
#   * "host load average at start" — from the sampler's FIRST reading, which is
#     taken just before k6 is launched. Without a sampler the record-time
#     average is kept, with a note that says whether it is merely run-tainted
#     (a minute or two after the run) or a description of the machine now.
# The thermal-throttle counters are cumulative since BOOT, so "throttling
# during this run" is not directly observable in one file either: it is the
# DIFF of those counters between two results, which compare.sh prints. A
# counter that cannot be read is recorded as null (never as 0), because 0 is
# the reassuring value and unknown is the true one.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
BENCH="$(cd "$HERE/.." && pwd)"          # harness/bench
DATA="$BENCH/data"                       # the bind mount run.sh serve mounts as /data

# The container `run.sh serve` starts, and the one envelope every number on
# this rig must come from. EXPECTED_CPUSET is the value run.sh's `verify`
# asserts; a mismatch here means the container was not started by run.sh (or
# was started with a typo'd --cpuset-cpus), and a number from it describes a
# different machine. Deliberately not overridable.
NAME=umbral-bench
EXPECTED_CPUSET="0,2,4,6"

# The result store. BENCH_RESULTS_DIR exists so the recorder can be exercised
# against synthetic summaries without dropping files that look like real
# results into the store; the default is this directory.
OUTDIR="${BENCH_RESULTS_DIR:-$HERE}"

# Post-run thermal fallback window. Short by design: it runs in the seconds
# right after the run ends, and it is only a substitute for the sampler.
THERMAL_SAMPLES=21   # ~10 s at 0.5 s
THERMAL_INTERVAL=0.5

# Where `--thermal-sample` writes and the normal invocation reads. One file is
# one run's window: the sampler truncates it at start. Deliberately a FIXED
# path rather than one derived from the result store, so that redirecting the
# store (BENCH_RESULTS_DIR) cannot silently unpair the sampler from the
# recorder; BENCH_THERMAL_SAMPLE points both at somewhere else.
sample_file_default() { echo "${BENCH_THERMAL_SAMPLE:-$HERE/.thermal-sample.tsv}"; }
SAMPLE_INTERVAL="${BENCH_SAMPLE_INTERVAL:-1}"     # seconds between samples
SAMPLE_MAX_S="${BENCH_SAMPLE_MAX_S:-3600}"        # a safety cap, not a plan

# How close to the run's end a sample window must reach to count as covering
# it. The sampler is stopped right after k6 exits, so in the documented flow
# `last >= run_end - 30`; a file left over from an earlier run is minutes
# behind the next run's end and is rejected. The plan's cooldown is 2-3
# minutes, which is what makes 30 s a separator rather than a guess.
SAMPLER_COVER_S=30
# A window that starts this soon before the run ended may have missed the
# steady-state peak (the ramp alone is 60 s), so it is accepted but warned about.
SAMPLER_MIN_COVER_S=60
# Past this lag, a post-run temperature is not evidence about the run.
THERMAL_FRESH_S=60
# Past this lag, the record-time load average no longer carries even the run's
# tail: the 1-minute average has fully decayed and only the 15-minute one is
# left, describing whatever the machine has been doing since.
LOAD_FRESH_S=120

iso() { date -u -d "@$1" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo null; }

# The session settings the measured binary runs with, declared rather than
# probed: `harness/backend/src/bin/bench-prod.rs` mounts
# `SessionsPlugin::default()`, which is the DbStore backend with
# `umbral_sessions::DEFAULT_TTL_SECONDS` (14 days) as the cookie TTL and no
# absolute max age. There is no endpoint that reports this, and minting a
# session to read its Set-Cookie would write a row into the measured database.
SESSION_TTL_SECONDS=1209600        # 14 days, plugins/umbral-sessions/src/lib.rs
SESSION_SOURCE="declared (bench-prod mounts SessionsPlugin::default(): cookie TTL 14d, no absolute max age); not probed at record time"

die() { # die <exit-code> <message...>
  local code="$1"; shift
  echo "ERROR: $*" >&2
  exit "$code"
}

# ---------------------------------------------------------------------------
# 0a. The sampler — the run's own thermal window
# ---------------------------------------------------------------------------
# Started before k6, stopped after it, so the recorded peak is the RUN's, not a
# reading of whatever the machine happens to be doing when someone gets round
# to filing the result. It runs on the HOST, never in the container: the
# container's cores are the measurement. Its cost is a handful of sysfs reads a
# second (microseconds), which is why it is not pinned either.
#
# File format, tab-separated, one line per reading:
#   L <epoch> <load1> <load5> <load15>
#   T <epoch> <zone type> <millidegrees C>
run_sampler() {
  local file="$1" stopped=0 start now ts z zone_type zone_temp
  mkdir -p "$(dirname "$file")"
  # TRUNCATE: one sample file is one run's window. A leftover file from an
  # earlier run is rejected by the recorder's coverage check, but overwriting
  # here means the common mistake (forgetting the sampler for run 2) cannot
  # silently pair run 2 with run 1's window either.
  : > "$file"
  echo "sampling thermals + load to $file every ${SAMPLE_INTERVAL}s (cap ${SAMPLE_MAX_S}s); stop with: kill $BASHPID" >&2
  trap 'stopped=1' TERM INT
  start="$(date +%s)"
  while :; do
    now="$(date +%s)"
    # shellcheck disable=SC2034  # the last two fields are unused here
    read -r l1 l5 l15 _ < /proc/loadavg
    printf 'L\t%s\t%s\t%s\t%s\n' "$now" "$l1" "$l5" "$l15" >> "$file"
    for z in /sys/class/thermal/thermal_zone*; do
      [ -r "$z/temp" ] || continue
      zone_type="$(cat "$z/type" 2>/dev/null || true)"
      zone_temp="$(cat "$z/temp" 2>/dev/null || true)"
      [ -n "$zone_type" ] && [ -n "$zone_temp" ] || continue
      printf 'T\t%s\t%s\t%s\n' "$now" "$zone_type" "$zone_temp" >> "$file"
    done
    if [ "$stopped" = 1 ]; then break; fi
    if [ $(( now - start )) -ge "$SAMPLE_MAX_S" ]; then
      echo "sampler hit its ${SAMPLE_MAX_S}s cap and stopped; if the run is still going, the window is truncated" >&2
      break
    fi
    # `sleep` as a background job + `wait`: a plain sleep would keep the trap
    # from running until the tick ends, and the stop is meant to be prompt.
    sleep "$SAMPLE_INTERVAL" &
    wait $! || true
  done
  # Ticks, not readings: one tick writes one row per thermal zone, so a count
  # of rows would overstate the sampling rate by the number of zones.
  ts="$(awk -F'\t' '
        $1=="T" { readings++; ticks[$2]=1; if (n++ == 0) first=$2; last=$2
                  if ($4+0 > peak) { peak=$4+0; who=$3 } }
        END { if (readings == 0) { printf "no thermal readings"; exit }
              c=0; for (t in ticks) c++
              printf "%d ticks over %ds (%d readings), peak %.1f C (%s)", c, last-first+1, readings, peak/1000, who }
      ' "$file")"
  echo "sampler stopped: $ts -> $file" >&2
}

# ---------------------------------------------------------------------------
# 0b. Arguments
# ---------------------------------------------------------------------------
usage() {
  cat >&2 <<'EOF'
usage: record.sh <summary-export.json> <vus> <dataset_scale> <k6-exit-code>
       record.sh --thermal-sample [file]   # run around a hold; stop with SIGTERM
EOF
}

if [ "${1:-}" = "--thermal-sample" ]; then
  shift
  [ "$#" -le 1 ] || { usage; die 2 "--thermal-sample takes at most one file argument."; }
  SAMPLER_FILE="${1:-$(sample_file_default)}"
  run_sampler "$SAMPLER_FILE"
  exit 0
fi
case "${1:-}" in -h|--help) usage; exit 0;; esac

[ "$#" -eq 4 ] || { usage; die 2 "expected 4 arguments, got $#."; }

SUMMARY="$1"
VUS="$2"
SCALE="$3"
EXIT_CODE="$4"

[ -r "$SUMMARY" ] || die 2 "the k6 summary export '$SUMMARY' does not exist or is not readable.
       Every k6 run started with --summary-export writes one, including a run
       that aborts in setup()/preflight (exit 107); without it there is no
       record of what ran, and this script will not invent one."

case "$VUS" in ''|*[!0-9]*) die 2 "VU count must be a positive integer (got '$VUS').";; esac
[ "$VUS" -ge 1 ] || die 2 "VU count must be at least 1 (got '$VUS')."
case "$SCALE" in ''|*[!0-9]*) die 2 "dataset scale must be a positive integer (got '$SCALE').";; esac
[ "$SCALE" -ge 1 ] || die 2 "dataset scale must be at least 1 (got '$SCALE')."
case "$EXIT_CODE" in ''|*[!0-9]*) die 2 "k6 exit code must be an integer (got '$EXIT_CODE').";; esac

command -v jq >/dev/null 2>&1 || die 2 "jq is required (apt install jq)."
command -v docker >/dev/null 2>&1 || die 3 "docker is required to verify the envelope."
# The summary export is the run's own record; reading it with jq as we go means
# an unparseable file would turn every field into null and still be filed as a
# result. Fail closed instead.
jq -e . "$SUMMARY" >/dev/null 2>&1 \
  || die 2 "'$SUMMARY' is not valid JSON: the summary export is corrupt or truncated.
       Nothing was recorded — a result built from an unreadable run would be
       a pile of nulls wearing a measurement's name."

# ---------------------------------------------------------------------------
# 1. The thermal window: the sampler's, or the post-run fallback
# ---------------------------------------------------------------------------
# The sampler is checked first and, when it covers this run, the post-run
# window is skipped entirely — which is also why the time-sensitive read below
# only happens in the fallback case (a sha256 of the container's binary, a
# `docker inspect` or a `git status` would each steal a second of the cooling
# curve).
now_epoch() { date +%s; }
SAMPLE_START_EPOCH="$(now_epoch)"
SUMMARY_MTIME="$(stat -c %Y "$SUMMARY")"
SAMPLED_AFTER_RUN_S=$(( SAMPLE_START_EPOCH - SUMMARY_MTIME ))

SAMPLE_FILE="$(sample_file_default)"
THERMAL_SOURCE="post-run-window"
THERMAL_STALE="false"
SAMPLER_FIRST=""; SAMPLER_LAST=""; SAMPLER_TICKS=""
sampler_exists="false"
if [ -f "$SAMPLE_FILE" ]; then
  sampler_exists="true"
  # The interval is measured from the file (mean spacing between ticks), not
  # taken from this shell's BENCH_SAMPLE_INTERVAL: the sampler may have been
  # started from a different shell, and a configured value asserted over a
  # measured one is the kind of substitution this store exists to avoid.
  read -r SAMPLER_FIRST SAMPLER_LAST SAMPLER_TICKS SAMPLER_INTERVAL <<<"$(awk -F'\t' '
    $1=="T" || $1=="L" { ts=$2+0; n++; if (n==1 || ts<first) first=ts; if (ts>last) last=ts; ticks[ts]=1 }
    END { if (n==0) { printf "\n"; exit }
          c=0; for (t in ticks) c++
          iv = (c > 1) ? (last - first) / (c - 1) : 0
          printf "%d %d %d %.3g\n", first, last, c, iv }
  ' "$SAMPLE_FILE")" || true
fi

# Does the file describe THIS run? It must reach back before the run ended and
# still have been alive as the run ended — a file from an earlier run is
# minutes behind the next run's end and is rejected here.
SAMPLER_COVERS="false"
if [ -n "${SAMPLER_LAST:-}" ] && [ "$SAMPLER_FIRST" -le "$SUMMARY_MTIME" ] \
   && [ "$SAMPLER_LAST" -ge $(( SUMMARY_MTIME - SAMPLER_COVER_S )) ]; then
  SAMPLER_COVERS="true"
fi

THERMAL_RAW="$(mktemp)"
THROTTLE_RAW="$(mktemp)"
trap 'rm -f "$THERMAL_RAW" "$THROTTLE_RAW"' EXIT

if [ "$SAMPLER_COVERS" = "true" ]; then
  THERMAL_SOURCE="sampler"
  THERMAL_WINDOW_FROM="$(iso "$SAMPLER_FIRST")"
  THERMAL_WINDOW_TO="$(iso "$SAMPLER_LAST")"
  THERMAL_WINDOW_S=$(( SAMPLER_LAST - SAMPLER_FIRST ))
  THERMAL_SAMPLES_N="$SAMPLER_TICKS"
  THERMAL_INTERVAL_USED="${SAMPLER_INTERVAL:-$SAMPLE_INTERVAL}"
  THERMAL_SAMPLED_AFTER="null"
  THERMAL_ZONES="$(jq -Rn '
    [inputs | split("\t") | select(.[0] == "T")
     | { i: (.[1] | tonumber), z: .[2], t: (.[3] | tonumber / 1000) }]
    | sort_by(.i)
    | group_by(.z)
    | map({ key: .[0].z,
            value: { first_c: .[0].t, peak_c: (map(.t) | max), last_c: .[-1].t } })
    | from_entries
  ' < "$SAMPLE_FILE")"
else
  i=0
  while [ "$i" -lt "$THERMAL_SAMPLES" ]; do
    for z in /sys/class/thermal/thermal_zone*; do
      [ -r "$z/temp" ] || continue
      zone_type="$(cat "$z/type" 2>/dev/null || true)"
      zone_temp="$(cat "$z/temp" 2>/dev/null || true)"
      [ -n "$zone_type" ] && [ -n "$zone_temp" ] || continue
      # Tab-separated: zone names contain spaces ("INT3400 Thermal"), values are
      # millidegrees Celsius.
      printf '%s\t%s\t%s\n' "$i" "$zone_type" "$zone_temp" >> "$THERMAL_RAW"
    done
    i=$(( i + 1 ))
    [ "$i" -lt "$THERMAL_SAMPLES" ] && sleep "$THERMAL_INTERVAL"
  done

  THERMAL_WINDOW_FROM="$(iso "$SAMPLE_START_EPOCH")"
  THERMAL_WINDOW_TO="$(iso "$(awk -v s="$SAMPLE_START_EPOCH" -v n="$THERMAL_SAMPLES" -v i="$THERMAL_INTERVAL" 'BEGIN { printf "%d", s + (n - 1) * i }')")"
  THERMAL_WINDOW_S="$(awk -v s="$THERMAL_SAMPLES" -v i="$THERMAL_INTERVAL" 'BEGIN { printf "%.1f", (s - 1) * i }')"
  THERMAL_SAMPLES_N="$THERMAL_SAMPLES"
  THERMAL_INTERVAL_USED="$THERMAL_INTERVAL"
  THERMAL_SAMPLED_AFTER="$SAMPLED_AFTER_RUN_S"
  if [ "$SAMPLED_AFTER_RUN_S" -gt "$THERMAL_FRESH_S" ]; then
    THERMAL_STALE="true"
  fi

  THERMAL_ZONES="$(jq -Rn '
    [inputs | split("\t") | { i: (.[0] | tonumber), z: .[1], t: (.[2] | tonumber / 1000) }]
    | sort_by(.i)
    | group_by(.z)
    | map({ key: .[0].z,
            value: { first_c: .[0].t, peak_c: (map(.t) | max), last_c: .[-1].t } })
    | from_entries
  ' < "$THERMAL_RAW")"
fi
THERMAL_PEAK="$(printf '%s' "$THERMAL_ZONES" | jq -r '[.[].peak_c] | if length == 0 then null else max end')"

# The load average the run started with, when the sampler caught it: its FIRST
# reading is taken a moment before k6 is launched. Only usable when the file
# covers THIS run — a leftover file's first reading is from some earlier
# period, which is exactly the mistake the coverage check exists to catch.
LOAD_BEFORE_RUN="null"
if [ "$SAMPLER_COVERS" = "true" ]; then
  LOAD_BEFORE_RUN="$(jq -Rn '
    [inputs | split("\t") | select(.[0] == "L")
     | { m1: (.[2] | tonumber), m5: (.[3] | tonumber), m15: (.[4] | tonumber) }]
    | if length == 0 then null else .[0] end
  ' < "$SAMPLE_FILE")"
fi

# Thermal throttling is a hardware counter, not an event log. The counters are
# per-CPU and cumulative since boot; the ones that matter are those of the CPUs
# the container is pinned to, because those are the only cores the measured
# process ever runs on. `cpu0` etc. also carry everyone else's work, so these
# numbers include other activity on those cores — diff them across results.
#
# An unreadable counter is recorded as NULL, never as 0. 0 is the reassuring
# value ("no throttling happened") and would make compare.sh print a `0 since a`
# delta that means nothing; unknown is the true value. If any of a CPU's four
# counters is missing, that CPU's row is null; if any row is null, the aggregate
# is null too — a partial sum presented as a total is the same lie one level up.
IFS=',' read -r -a CPUSET_CPUS <<< "$EXPECTED_CPUSET"
throttle_rows=0
throttle_unreadable=0
for cpu in "${CPUSET_CPUS[@]}"; do
  tdir="/sys/devices/system/cpu/cpu${cpu}/thermal_throttle"
  core=""; core_ms=""; pkg=""; pkg_ms=""
  if [ -d "$tdir" ]; then
    core="$(cat "$tdir/core_throttle_count" 2>/dev/null || true)"
    core_ms="$(cat "$tdir/core_throttle_total_time_ms" 2>/dev/null || true)"
    pkg="$(cat "$tdir/package_throttle_count" 2>/dev/null || true)"
    pkg_ms="$(cat "$tdir/package_throttle_total_time_ms" 2>/dev/null || true)"
  fi
  printf '%s\t%s\t%s\t%s\t%s\n' "$cpu" "${core:-null}" "${core_ms:-null}" "${pkg:-null}" "${pkg_ms:-null}" >> "$THROTTLE_RAW"
  throttle_rows=$(( throttle_rows + 1 ))
  case "${core}${core_ms}${pkg}${pkg_ms}" in
    ''|*[!0-9]*) throttle_unreadable=$(( throttle_unreadable + 1 )) ;;
  esac
done

cpu_max_khz="$(cat /sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq 2>/dev/null || true)"
cpu_max_mhz="null"; [ -n "$cpu_max_khz" ] && cpu_max_mhz="$(awk -v k="$cpu_max_khz" 'BEGIN {printf "%d", k/1000}')"
cpu_now_mhz="$(cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_cur_freq 2>/dev/null \
  | awk '{v=$1/1000; if (n==0 || v<min) min=v; if (v>max) max=v; n++} END {if (n==0) print "null"; else printf "[%d,%d]", min, max}')"

# ---------------------------------------------------------------------------
# 2. The envelope, from `docker inspect` — what took effect, not what was asked
# ---------------------------------------------------------------------------
# Same field template as `run.sh verify`, for the same reason: `--cpus=4` is
# stored as either NanoCpus or a quota/period pair depending on the Docker
# version, and a typo'd cpuset produces a perfectly healthy-looking container
# measuring a different machine. `docker container inspect`, not plain
# `docker inspect`: the bare form resolves across object types.
docker container inspect "$NAME" >/dev/null 2>&1 \
  || die 3 "no container named '$NAME'. Every result must come from the silo
       'run.sh serve' starts; run that first, then 'run.sh verify'."

read -r env_cpuset nano quota period memory swap running oom started_at image_name image_id container_path container_args \
  <<<"$(docker inspect -f \
        '{{.HostConfig.CpusetCpus}} {{.HostConfig.NanoCpus}} {{.HostConfig.CpuQuota}} {{.HostConfig.CpuPeriod}} {{.HostConfig.Memory}} {{.HostConfig.MemorySwap}} {{.State.Running}} {{.State.OOMKilled}} {{.State.StartedAt}} {{.Config.Image}} {{.Image}} {{.Path}} {{.Args}}' \
        "$NAME")"
nano=${nano:-0}; quota=${quota:-0}; period=${period:-0}

# THE REFUSAL. Everything in this file is a claim about a number measured inside
# one specific envelope; if the envelope is not the one run.sh asserts, the
# number is not comparable to anything and must not enter the store.
if [ "$env_cpuset" != "$EXPECTED_CPUSET" ]; then
  die 3 "envelope mismatch: the container's cpuset is '${env_cpuset:-<unset>}', expected '$EXPECTED_CPUSET'.
       The silo was not started by run.sh serve, or was started with a different
       --cpuset-cpus. A number from this container describes a different machine,
       so nothing was recorded. Fix the envelope, re-run, then record."
fi

if [ "$nano" -gt 0 ]; then
  cpu_cap_cpus=$(( nano / 1000000000 ))
  cpu_cap_form="$nano nano-cpus"
elif [ "$quota" -gt 0 ] && [ "$period" -gt 0 ]; then
  cpu_cap_cpus=$(( quota / period ))
  cpu_cap_form="$quota/$period quota/period"
else
  cpu_cap_cpus=0
  cpu_cap_form="unset"
fi

# In-container facts need a running container. A container that is not running
# is still recorded — an OOM-killed or crashed silo is exactly the kind of
# result that must reach the store rather than being lost to a refusal — but
# the in-container half is then null and says so.
nproc_in="null"; mem_max_in="null"; swap_max_in="null"; nofile_in="null"; binary_sha="null"
if [ "$running" = "true" ]; then
  nproc_in="$(docker exec "$NAME" nproc 2>/dev/null || echo null)"
  mem_max_in="$(docker exec "$NAME" cat /sys/fs/cgroup/memory.max 2>/dev/null || echo null)"
  swap_max_in="$(docker exec "$NAME" cat /sys/fs/cgroup/memory.swap.max 2>/dev/null || echo null)"
  # pid 1's limit, not a fresh exec shell's: this is the limit that decides
  # whether a high-VU hold can keep its connections (run.sh verify reads the
  # same file for the same reason).
  nofile_in="$(docker exec "$NAME" awk '/Max open files/{print $4}' /proc/1/limits 2>/dev/null || echo null)"
  # Which binary ran, not just which name it had: a commit can be rebuilt, and
  # the binary is what actually served the load.
  binary_sha="$(docker exec "$NAME" sha256sum "$container_path" 2>/dev/null | awk '{print $1}' || true)"
  [ -n "$binary_sha" ] || binary_sha="null"
fi

# The database the container is actually serving, from its own environment —
# `run.sh serve` accepts an override, so the URL is a fact about this run, not
# a constant. The WAL facts are only readable when the database lives on the
# bind mount (/data -> harness/bench/data).
db_url="$(docker inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$NAME" \
  | awk '/^UMBRAL_DATABASE_URL=/{sub(/^UMBRAL_DATABASE_URL=/,""); print; exit}')"
# A WAL file that is ABSENT at record time is not an error: SQLite deletes
# -wal/-shm when the last connection closes cleanly, so a database at rest
# after its writer went idle looks exactly like this. The size is recorded when
# the file is there, and the absence is recorded as null rather than guessed at.
db_host_path="null"; db_bytes="null"; wal_bytes="null"; shm_bytes="null"
case "$db_url" in
  sqlite:///data/*)
    rel="${db_url#sqlite:///data/}"; rel="${rel%%\?*}"
    db_host_path="$DATA/$rel"
    [ -f "$db_host_path" ] && db_bytes="$(stat -c %s "$db_host_path" 2>/dev/null || echo null)"
    [ -f "$db_host_path-wal" ] && wal_bytes="$(stat -c %s "$db_host_path-wal" 2>/dev/null || echo null)"
    [ -f "$db_host_path-shm" ] && shm_bytes="$(stat -c %s "$db_host_path-shm" 2>/dev/null || echo null)"
    ;;
esac

# The container's own log. Counts are CUMULATIVE since the container started; a
# SQLite busy/locked error is the observable trace of a writer that had to wait
# (or gave up) on the WAL, and its delta between two results is what says
# whether it happened in between. Post-hoc, because `bench-prod` carries no
# instrumentation by design, so there is no per-request telemetry to read.
#
# A failing `docker logs` is recorded as NULL, not as 0: "0 busy lines" would be
# a false negative on exactly the fact the WAL-stall rule leans on. (A grep that
# runs and finds nothing is a real 0 and stays one.)
if container_log="$(docker logs "$NAME" 2>&1)"; then
  busy_lines="$(printf '%s' "$container_log" | grep -ciE 'busy|database is locked|checkpoint' || true)"
  error_lines="$(printf '%s' "$container_log" | grep -cE 'level=error|ERROR' || true)"
else
  busy_lines="null"
  error_lines="null"
fi
unset container_log 2>/dev/null || true

# ---------------------------------------------------------------------------
# 3. The k6 summary
# ---------------------------------------------------------------------------
# `// null` everywhere: a preflight abort (exit 107) still writes a summary
# export, but it contains only an empty root_group and data_sent/data_received
# — no metrics, no checks, no setup_data. Reading it must not crash, and the
# absence must be visible in the result.
jc() { jq -c "$1" "$SUMMARY" 2>/dev/null || echo null; }

rps="$(jc '(.metrics.http_reqs.rate // null) | if . == null then null else ((. * 1000 | round) / 1000) end')"
requests="$(jc '.metrics.http_reqs.count // null')"
iterations="$(jc '.metrics.iterations.count // null')"
error_rate="$(jc '(.metrics.http_req_failed.value // null) | if . == null then null else ((. * 1000000 | round) / 1000000) end')"
vus_max="$(jc '.metrics.vus_max.value // null')"
# k6's summary export carries `http_req_duration` in MILLISECONDS already (its
# metric unit) — verified against the same run's text summary: export
# `p(95): 4.4108278` is the printed `p(95)=4.41ms`. The only work here is
# rounding to millisecond precision, so nobody re-derives a different unit.
ms_def='def ms: if . == null then null else ((. * 1000 | round) / 1000) end; '
p50_ms="$(jc "${ms_def}(.metrics.http_req_duration[\"med\"] // null) | ms")"
p95_ms="$(jc "${ms_def}(.metrics.http_req_duration[\"p(95)\"] // null) | ms")"
p99_ms="$(jc "${ms_def}(.metrics.http_req_duration[\"p(99)\"] // null) | ms")"
max_ms="$(jc "${ms_def}(.metrics.http_req_duration[\"max\"] // null) | ms")"
thresholds="$(jc '((.metrics.http_req_duration.thresholds // {}) + (.metrics.http_req_failed.thresholds // {}))')"
checks_total="$(jc '.metrics.checks // null')"
per_check="$(jc '(.root_group.checks // {}) | to_entries | map({name: .key, passes: .value.passes, fails: .value.fails})')"
setup_data="$(jc '.setup_data // null')"
mix="$(jc 'if (.setup_data.mix // null) == null then null else {LIKE_P: .setup_data.mix.LIKE_P, POST_P: .setup_data.mix.POST_P, PAGE_P: .setup_data.mix.PAGE_P, think_ms: (.setup_data.think_ms // null)} end')"
workload_range="$(jc 'if (.setup_data.users // null) == null then null else {users: .setup_data.users, posts: (.setup_data.posts // null), preflight: (.setup_data.preflight // null)} end')"

# ---------------------------------------------------------------------------
# 4. Classify the exit code, and cross-check what the summary says
# ---------------------------------------------------------------------------
case "$EXIT_CODE" in
  0)   exit_class="passed";           verdict="pass" ;;
  99)  exit_class="threshold_failed"; verdict="fail" ;;
  107) exit_class="preflight_abort";  verdict="not_a_load_result" ;;
  *)   exit_class="unclassified";     verdict="unknown" ;;
esac

warnings=()
warn() { warnings+=("$1"); }

git_dirty="false"
git_sha=""; git_sha_short=""
if git -C "$BENCH" rev-parse --git-dir >/dev/null 2>&1; then
  git_sha="$(git -C "$BENCH" rev-parse HEAD 2>/dev/null || true)"
  git_sha_short="$(git -C "$BENCH" rev-parse --short HEAD 2>/dev/null || true)"
  # An unattributed number is a weaker number: a dirty tree means the binary
  # may not correspond to the commit at all.
  if [ -n "$(git -C "$BENCH" status --porcelain 2>/dev/null | head -n 1)" ]; then
    git_dirty="true"
    warn "the working tree is dirty: the recorded commit does not fully describe the source that was built."
  fi
else
  warn "not inside a git repository: no commit can be recorded for this result."
fi
[ -n "$git_sha" ] || { git_sha="null"; git_sha_short="nogit"; }

[ "$exit_class" = "preflight_abort" ] && warn "k6 exit 107: the run aborted in setup()/preflight. This is a RIG-CONFIGURATION failure (id ranges, or a session pool that does not match the served database), not a load result — it measures nothing about umbral."
[ "$exit_class" = "unclassified" ] && warn "k6 exit $EXIT_CODE is unclassified: not a pass, not a threshold failure, not a preflight abort. Recorded as-is rather than folded into either bucket."

if [ "$exit_class" = "passed" ]; then
  if [ "$thresholds" = "null" ] || printf '%s' "$thresholds" | jq -e 'length == 0' >/dev/null; then
    warn "exit 0 but the summary carries no thresholds: this run was not judged against anything (an unscored warm-up would look like this)."
  elif printf '%s' "$thresholds" | jq -e 'to_entries | any(.value == true)' >/dev/null; then
    warn "exit 0 but the summary marks a threshold violated: the exit code and the thresholds disagree; check how the run was invoked."
  fi
fi
if [ "$exit_class" = "threshold_failed" ] && [ "$thresholds" != "null" ] \
   && printf '%s' "$thresholds" | jq -e 'length > 0 and (to_entries | all(.value == false))' >/dev/null; then
  warn "exit 99 but no threshold is marked violated: the exit code and the thresholds disagree; check how the run was invoked."
fi

# The checks are not thresholds: k6 exits 0 on latency alone even if every
# `page 200` failed. A failed check means the run measured the miss path (or a
# broken endpoint), so it is recorded loudly rather than left to the exit code.
failed_checks="$(printf '%s' "$per_check" | jq -r '[.[] | select(.fails > 0)] | map("\(.name) (\(.fails) fails)") | join(", ")')"
[ -n "$failed_checks" ] && warn "checks failed: $failed_checks — the thresholds cover latency and error rate, not the checks."

if [ "$vus_max" = "null" ]; then
  [ "$exit_class" != "preflight_abort" ] && warn "the summary reports no vus_max: the run may not have started."
elif ! jq -en --argjson a "$vus_max" --argjson b "$VUS" '$a == $b' >/dev/null; then
  warn "the VU count passed to record.sh ($VUS) differs from the summary's vus_max ($vus_max)."
fi

if [ "$workload_range" != "null" ]; then
  range_users="$(printf '%s' "$workload_range" | jq -r '.users')"
  if ! jq -en --argjson a "$range_users" --argjson b "$SCALE" '$a == $b' >/dev/null; then
    warn "dataset_scale ($SCALE, the argument) differs from the id range the run actually used (setup_data.users=$range_users, which the preflight verified against the served rows). compare.sh gates on dataset_scale — settle which one is right before comparing."
  fi
elif [ "$exit_class" != "preflight_abort" ]; then
  warn "the summary carries no setup_data: the mix and the id ranges the run used are unknown (was the script run with PREFLIGHT=0, or is this not a hold.js summary?)."
fi

[ "$mix" = "null" ] && [ "$exit_class" != "preflight_abort" ] && warn "no mix in the summary: mix is null and compare.sh will refuse to diff this result."

# Envelope facts that run.sh asserts but that this script only warns about: the
# brief's refusal is the cpuset, and a future rig may legitimately change these
# — but a container with swap or without the memory cap is not the silo the
# numbers are quoted from, and that must not pass silently.
if [ "$memory" != "4294967296" ] || [ "$swap" != "4294967296" ]; then
  warn "memory=$memory / memory-swap=$swap is not the 4 GiB, no-swap envelope run.sh serve requests (4294967296/4294967296)."
fi
case "$nofile_in" in
  null|''|*[!0-9]*) ;;
  *) [ "$nofile_in" -ge 20000 ] || warn "the container's nofile limit is $nofile_in: a hold above ~1,000 VUs needs more, and the failures it causes look like the server rejecting load." ;;
esac
[ "$oom" = "true" ] && warn "the container was OOM-killed: this run hit the memory ceiling and was killed, not slowed."
[ "$running" = "true" ] || warn "the container is not running at record time: in-container envelope facts are null (a crash or OOM kill is itself the finding)."
[ "$db_host_path" = "null" ] && warn "the served database is not a SQLite file under /data (db_url='$db_url'), so the WAL and file-size facts could not be read from the host."

# A WAL checkpoint under a write burst can block a reader for hundreds of
# milliseconds while the percentile p99 stays in the tens. That is a storage
# stall, not umbral, and a single blocked request lands in `max` only. The rule
# below is explicit so a reader can re-judge it from the raw numbers.
wal_stall_suspected="false"
if [ "$max_ms" != "null" ] && [ "$p99_ms" != "null" ] \
   && jq -en --argjson max "$max_ms" --argjson p99 "$p99_ms" '$max > 200 and $max > (10 * $p99)' >/dev/null; then
  wal_stall_suspected="true"
  warn "max request latency ($max_ms ms) is >10x p99 ($p99_ms ms) and over 200 ms: a WAL checkpoint stall is a plausible cause — treat this before blaming throughput."
fi

# The provenance of the thermal and load readings, for a run that actually
# measured something. A post-run sample is a legitimate record and a worthless
# explanation of a run-to-run spread, and the difference has to be stated in
# the file rather than assumed by whoever reads it weeks later.
load_stale="false"
if [ "$LOAD_BEFORE_RUN" = "null" ] && [ "$SAMPLED_AFTER_RUN_S" -gt "$LOAD_FRESH_S" ]; then
  load_stale="true"
fi
case "$exit_class" in
  passed|threshold_failed)
    if [ "$THERMAL_SOURCE" = "sampler" ]; then
      sampler_cover=$(( SUMMARY_MTIME - SAMPLER_FIRST ))
      if [ "$sampler_cover" -lt "$SAMPLER_MIN_COVER_S" ]; then
        warn "the thermal sampler covered only the last ${sampler_cover}s before the run ended (a hold ramps for 60s before its steady state): the peak may be earlier in the run than the window reaches."
      fi
      # A window opened on a hot machine measures the heat THAT machine had,
      # which is the same misattribution the fallback window suffers from, in
      # the opposite direction. The per-zone first_c values are the evidence.
      sampler_first_max="$(printf '%s' "$THERMAL_ZONES" | jq -r '[.[].first_c] | if length == 0 then null else max end')"
      if [ "$sampler_first_max" != "null" ] \
         && jq -en --argjson f "$sampler_first_max" '$f >= 75' >/dev/null; then
        warn "the sampler's window opened at ${sampler_first_max} C: the machine was already hot when sampling began, so peak_c may include heat this run did not generate (a previous run, or other work). Start the sampler after the cooldown and stop it when k6 exits."
      fi
    elif [ "$sampler_exists" = "true" ]; then
      warn "the sampler file $SAMPLE_FILE does not cover this run (its last sample is $(iso "${SAMPLER_LAST:-0}"), the run ended $(iso "$SUMMARY_MTIME")): falling back to the post-run window. Was the sampler started for THIS run?"
    else
      warn "no sampler at $SAMPLE_FILE: the thermal and load readings are post-hoc substitutes (temperature sampled ${SAMPLED_AFTER_RUN_S}s after the run ended, load average read at record time), not in-run readings. Start 'record.sh --thermal-sample' before the run and stop it after."
    fi
    [ "$THERMAL_STALE" = "true" ] && warn "the thermal reading was taken ${SAMPLED_AFTER_RUN_S}s after the run ended — that is NOT evidence about the run (nothing on this host keeps thermal history). Do not attribute a run-to-run spread to heat-soak from it."
    [ "$load_stale" = "true" ] && warn "the load average was recorded ${SAMPLED_AFTER_RUN_S}s after the run ended: it describes the machine at record time, not what was running during the hold."
    if [ "$throttle_rows" -eq 0 ]; then
      warn "no thermal-throttle counters are exposed for the cpuset CPUs ($EXPECTED_CPUSET): throttling is UNKNOWN (recorded as null), not absent."
    elif [ "$throttle_unreadable" -gt 0 ]; then
      warn "$throttle_unreadable of $throttle_rows cpuset CPUs have unreadable thermal-throttle counters: the aggregates are recorded as null (unknown), not as zero."
    fi
    [ "$busy_lines" = "null" ] && warn "docker logs failed: the container's busy/locked/error line counts are UNKNOWN (null), so a WAL stall cannot be ruled out from the log."
    ;;
esac

# ---------------------------------------------------------------------------
# 5. Assemble
# ---------------------------------------------------------------------------
hostname_s="$(hostname 2>/dev/null || echo unknown)"
kernel_s="$(uname -r 2>/dev/null || echo unknown)"
cpu_s="$(awk -F': ' '/^model name/{print $2; exit}' /proc/cpuinfo 2>/dev/null || echo unknown)"
read -r load1 load5 load15 _ < /proc/loadavg
nofile_host="$(ulimit -n 2>/dev/null || true)"
case "$nofile_host" in ''|*[!0-9]*) nofile_host="null" ;; esac   # 'unlimited' is not a number
recorded_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
summary_at="$(date -u -d "@$SUMMARY_MTIME" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo null)"

# jq scalar arguments that are strings-or-null need the null to stay a JSON
# null, not the string "null".
n_or_q() { [ "$1" = "null" ] && echo null || printf '"%s"' "$1"; }

if [ "${#warnings[@]}" -gt 0 ]; then
  warnings_json="$(printf '%s\n' "${warnings[@]}" | jq -Rn '[inputs | select(length > 0)]')"
else
  warnings_json="[]"
fi

envelope_json="$(jq -n \
  --arg container "$NAME" \
  --arg image "$image_name" --arg image_id "$image_id" \
  --arg path "$container_path" \
  --argjson args_json "$(jq -cn --arg a "$container_args" '$a | ltrimstr("[") | rtrimstr("]") | split(" ") | map(select(length > 0))')" \
  --argjson binary_sha "$(n_or_q "$binary_sha")" \
  --arg cpuset "$env_cpuset" --arg cap_form "$cpu_cap_form" --argjson cap_cpus "$cpu_cap_cpus" \
  --argjson nano "$nano" --argjson quota "$quota" --argjson period "$period" \
  --argjson memory "$memory" --argjson swap "$swap" \
  --argjson nproc "$nproc_in" --argjson mem_max "$mem_max_in" --argjson swap_max "$swap_max_in" \
  --argjson nofile "$nofile_in" \
  --argjson running "$running" --argjson oom "$oom" \
  --arg started_at "$started_at" \
  '{
     container: $container,
     image: $image,
     image_id: $image_id,
     binary: { path: $path, args: $args_json, sha256: $binary_sha },
     cpuset: $cpuset,
     cpu_cap: { cpus: $cap_cpus, form: $cap_form, nano_cpus: $nano, quota: $quota, period: $period },
     memory_bytes: $memory,
     memory_swap_bytes: $swap,
     container_running: $running,
     oom_killed: $oom,
     container_started_at: $started_at,
     nproc_in_container: $nproc,
     memory_max_in_container: $mem_max,
     memory_swap_max_in_container: $swap_max,
     nofile_soft_in_container: $nofile,
     verified: true
   }')"

# What the thermal number IS, in the file, derived from where it came from —
# never asserted. A reading taken after the run is a measurement of whatever
# the machine was doing at that moment; the note says so, and says it louder
# the further from the run it was taken.
if [ "$THERMAL_SOURCE" = "sampler" ]; then
  sampler_gap=$(( SAMPLER_LAST - SUMMARY_MTIME ))
  if [ "$sampler_gap" -ge 0 ]; then
    sampler_gap_desc="ending ${sampler_gap}s after the run ended"
  else
    sampler_gap_desc="ending $(( -sampler_gap ))s before the run ended"
  fi
  THERMAL_NOTE="the run's own window: $THERMAL_SAMPLES_N ticks at ~${THERMAL_INTERVAL_USED}s taken on the HOST by 'record.sh --thermal-sample', spanning ${THERMAL_WINDOW_S}s and $sampler_gap_desc. peak_c is the highest reading in that window — the run's own peak when the sampler was bracketed around the run (started after the cooldown, stopped when k6 exited); the per-zone first_c values show whether the window opened on a cold machine."
  THERMAL_SHORT="sampler, ${THERMAL_WINDOW_S}s window, ${sampler_gap_desc}"
elif [ "$THERMAL_STALE" = "true" ]; then
  THERMAL_NOTE="SAMPLED ${SAMPLED_AFTER_RUN_S}s AFTER the run ended — this is NOT evidence about the run. Nothing on this host keeps thermal history and no sampler covered the run, so there is no in-run reading; these numbers describe the machine at RECORD time. Do not attribute a run-to-run spread to heat-soak from this field."
  THERMAL_SHORT="post-run window, +${SAMPLED_AFTER_RUN_S}s (STALE: not evidence about the run)"
else
  THERMAL_NOTE="peak of $THERMAL_SAMPLES_N samples taken ${SAMPLED_AFTER_RUN_S}s after the run ended (post-run window; no sampler covered the run). This is the top of the cooling curve — close to, but below, the in-run peak. Start 'record.sh --thermal-sample' before the next hold to get the run's own window."
  THERMAL_SHORT="post-run window, +${SAMPLED_AFTER_RUN_S}s"
fi

thermal_json="$(jq -n \
  --arg source "$THERMAL_SOURCE" \
  --argjson stale "$THERMAL_STALE" \
  --argjson zones "$THERMAL_ZONES" \
  --argjson peak "${THERMAL_PEAK:-null}" \
  --argjson samples "$THERMAL_SAMPLES_N" \
  --argjson interval "$THERMAL_INTERVAL_USED" \
  --argjson window "$THERMAL_WINDOW_S" \
  --arg from "$THERMAL_WINDOW_FROM" --arg to "$THERMAL_WINDOW_TO" \
  --argjson after "$THERMAL_SAMPLED_AFTER" \
  --arg note "$THERMAL_NOTE" \
  '{
     source: $source,
     stale: $stale,
     window_s: $window,
     window_from: $from,
     window_to: $to,
     interval_s: $interval,
     samples: $samples,
     sampled_after_run_s: $after,
     peak_c: $peak,
     zones: $zones,
     note: $note
   }')"

# An unreadable counter arrives from the reader above as the string "null".
# The aggregate is null if ANY component is unknown: a partial sum presented as
# a total is a reassuring value standing in for an unknown one, which is the
# bug this whole section exists to avoid.
throttling_json="$(jq -Rn \
  --arg scope "cumulative since boot, restricted to the container's cpuset CPUs ($EXPECTED_CPUSET); those cores also carry other processes' work. Diff two results to see throttling between them." \
  --argjson max_mhz "$cpu_max_mhz" \
  --argjson now_mhz "$cpu_now_mhz" \
  'def agg($rows; f):
     if ($rows | length) == 0 then null
     elif ($rows | map(f) | any(. == null)) then null
     else ($rows | map(f) | add) end;
   [inputs | split("\t") | select(length == 5)
    | { cpu: (.[0] | tonumber),
        core:      (if .[1] == "null" then null else (.[1] | tonumber) end),
        core_ms:   (if .[2] == "null" then null else (.[2] | tonumber) end),
        package:   (if .[3] == "null" then null else (.[3] | tonumber) end),
        package_ms:(if .[4] == "null" then null else (.[4] | tonumber) end) }] as $rows
  | {
      counter_scope: $scope,
      core_throttle_count: (agg($rows; .core)),
      core_throttle_total_ms: (agg($rows; .core_ms)),
      package_throttle_count: (agg($rows; .package)),
      package_throttle_total_ms: (agg($rows; .package_ms)),
      by_cpu: ($rows | map({ key: (.cpu | tostring), value: { core: .core, package: .package } }) | from_entries),
      cpu_current_mhz: { min: (if $now_mhz == null then null else $now_mhz[0] end),
                         max: (if $now_mhz == null then null else $now_mhz[1] end) },
      cpu_max_mhz: $max_mhz
    }' < "$THROTTLE_RAW")"

wal_json="$(jq -n \
  --arg db_url "$db_url" --argjson host_path "$(n_or_q "$db_host_path")" \
  --argjson db "$db_bytes" \
  --argjson wal "$wal_bytes" --argjson shm "$shm_bytes" \
  --argjson busy "$busy_lines" --argjson errors "$error_lines" \
  --argjson max_ms "$max_ms" --argjson p99 "$p99_ms" \
  --argjson suspected "$wal_stall_suspected" \
  --arg basis "checkpoint_stall_suspected = max_ms > 200 AND max_ms > 10 x p99_ms. Rationale: a checkpoint blocks a reader for hundreds of ms while p99 stays in the tens, and the summary export only carries max/p99 — bench-prod has no per-request instrumentation by design. The log counters are cumulative since container start; diff them across results." \
  '{
     db_url: $db_url,
     host_path: $host_path,
     db_bytes: $db,
     wal_bytes: $wal,
     shm_bytes: $shm,
     max_request_ms: $max_ms,
     p99_ms: $p99,
     container_log: { busy_or_locked_or_checkpoint_lines: $busy, error_lines: $errors, scope: "cumulative since container start" },
     checkpoint_stall_suspected: $suspected,
     basis: $basis
   }')"

session_json="$(jq -n \
  --argjson ttl "$SESSION_TTL_SECONDS" --arg source "$SESSION_SOURCE" \
  '{ store: "DbStore", cookie: "umbral_session", ttl_seconds: $ttl, absolute_max_age_seconds: null, source: $source }')"

# The load average is the other fact the brief asks for "at start", and it gets
# the same treatment: the sampler's first reading when there is one, otherwise
# the record-time value with a note that says whether it is merely run-tainted
# or a description of a machine that has long since moved on.
if [ "$LOAD_BEFORE_RUN" != "null" ]; then
  LOAD_NOTE="loadavg_before_run is the sampler's FIRST reading, taken a moment before k6 was launched — the closest available reading of the host at the run's start. loadavg_at_record is the machine as this result was written."
elif [ "$SAMPLED_AFTER_RUN_S" -le "$LOAD_FRESH_S" ]; then
  LOAD_NOTE="no sampler ran, so there is no pre-run reading. loadavg_at_record was taken ${SAMPLED_AFTER_RUN_S}s after the run ended: the 1- and 5-minute averages still carry the run's tail, so it is run-tainted, not a pre-run baseline."
else
  LOAD_NOTE="no sampler ran, so there is no pre-run reading. loadavg_at_record was taken ${SAMPLED_AFTER_RUN_S}s after the run ended — that describes the machine NOW, not at the run, and says nothing about what else was running while the hold was measured. Start 'record.sh --thermal-sample' before the next hold."
fi

host_json="$(jq -n \
  --arg hostname "$hostname_s" --arg kernel "$kernel_s" --arg cpu "$cpu_s" \
  --argjson l1 "$load1" --argjson l5 "$load5" --argjson l15 "$load15" \
  --argjson before "$LOAD_BEFORE_RUN" \
  --arg note "$LOAD_NOTE" \
  --argjson nofile "$nofile_host" \
  '{
     hostname: $hostname, kernel: $kernel, cpu: $cpu,
     loadavg_before_run: $before,
     loadavg_at_record: { m1: $l1, m5: $l5, m15: $l15 },
     note: $note,
     nofile_soft: $nofile,
     nofile_note: "ulimit -n of the shell that invoked record.sh — in the documented flow, the same shell that ran k6."
   }')"

result_json="$(jq -n \
  --argjson schema 1 \
  --arg git_sha "$git_sha" --arg git_sha_short "$git_sha_short" --argjson git_dirty "$git_dirty" \
  --arg recorded_at "$recorded_at" --arg summary_path "$SUMMARY" --arg summary_at "$summary_at" \
  --argjson recorded_after_run_s "$SAMPLED_AFTER_RUN_S" \
  --argjson vus "$VUS" --argjson dataset_scale "$SCALE" \
  --argjson exit_code "$EXIT_CODE" --arg exit_class "$exit_class" --arg verdict "$verdict" \
  --argjson rps "$rps" --argjson requests "$requests" --argjson iterations "$iterations" \
  --argjson p50 "$p50_ms" --argjson p95 "$p95_ms" --argjson p99 "$p99_ms" --argjson max "$max_ms" \
  --argjson error_rate "$error_rate" --argjson vus_max "$vus_max" \
  --argjson thresholds "$thresholds" --argjson checks "$checks_total" --argjson per_check "$per_check" \
  --argjson mix "$mix" --argjson workload_range "$workload_range" --argjson setup_data "$setup_data" \
  --argjson envelope "$envelope_json" \
  --argjson thermal "$thermal_json" \
  --argjson throttling "$throttling_json" \
  --argjson wal "$wal_json" \
  --argjson session "$session_json" \
  --argjson host "$host_json" \
  --argjson warnings "$warnings_json" \
  '{
     schema: $schema,
     git_sha: $git_sha,
     git_sha_short: $git_sha_short,
     git_dirty: $git_dirty,
     recorded_at: $recorded_at,
     summary: { path: $summary_path, written_at: $summary_at, recorded_after_run_s: $recorded_after_run_s },
     vus: $vus,
     dataset_scale: $dataset_scale,
     mix: $mix,
     rps: $rps,
     p50_ms: $p50, p95_ms: $p95, p99_ms: $p99, max_ms: $max,
     error_rate: $error_rate,
     requests: $requests,
     iterations: $iterations,
     vus_max: $vus_max,
     checks: { totals: $checks, per_check: $per_check, held: (($per_check // []) | if length == 0 then null else all(.fails == 0) end) },
     thresholds: $thresholds,
     exit: { code: $exit_code, class: $exit_class },
     verdict: $verdict,
     workload_range: $workload_range,
     k6_setup_data: $setup_data,
     envelope: $envelope,
     thermal: $thermal,
     throttling: $throttling,
     wal: $wal,
     session: $session,
     host: $host,
     warnings: $warnings
   }')"

# ---------------------------------------------------------------------------
# 6. Write, atomically, under a name that cannot collide
# ---------------------------------------------------------------------------
mkdir -p "$OUTDIR"
base="${git_sha_short}-${stamp}"
out="$OUTDIR/$base.json"
n=2
while [ -e "$out" ]; do
  out="$OUTDIR/$base-$n.json"
  n=$(( n + 1 ))
done
tmp="$(mktemp "$OUTDIR/.record.XXXXXX")"
chmod 0644 "$tmp"          # mktemp is 0600; a result is meant to be read
printf '%s\n' "$result_json" > "$tmp"
mv "$tmp" "$out"

# ---------------------------------------------------------------------------
# 7. Say what was recorded
# ---------------------------------------------------------------------------
fmt() { [ "$1" = "null" ] && echo "n/a" || echo "$1"; }
echo "recorded $out"
printf '  verdict   %s (exit %s, class %s)\n' "$verdict" "$EXIT_CODE" "$exit_class"
printf '  rps       %s   p95 %s ms   p99 %s ms   errors %s\n' \
  "$(fmt "$rps")" "$(fmt "$p95_ms")" "$(fmt "$p99_ms")" "$(fmt "$error_rate")"
printf '  checks    %s\n' "$(printf '%s' "$checks_total" | jq -r 'if . == null then "n/a" else "\(.passes)/\(.passes + .fails) held" end')"
printf '  envelope  cpuset %s, %s cpus, memory %s, nofile %s\n' "$env_cpuset" "$cpu_cap_cpus" "$memory" "$nofile_in"
printf '  thermal   peak %s C (%s)\n' "$(fmt "${THERMAL_PEAK:-null}")" "$THERMAL_SHORT"
printf '  warnings  %s\n' "$(printf '%s' "$warnings_json" | jq -r 'length')"
if [ "${#warnings[@]}" -gt 0 ]; then
  for w in "${warnings[@]}"; do printf '    - %s\n' "$w"; done
fi

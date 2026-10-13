#!/usr/bin/env bash
# Launch the measured application inside the fixed-resource silo.
#
# The envelope is the whole point: every number ever quoted must come from a
# container started by this script, so the CPU and memory available are
# identical between the baseline and every later comparison.
#
# `--cpuset-cpus=0,2,4,6` pins four DISTINCT PHYSICAL P-cores. On this i7-13700H
# the P-cores are hyperthreaded pairs (physical core 0 = cpus {0,1}, core 1 =
# {2,3}, core 2 = {4,5}, core 3 = {6,7}), so `--cpuset-cpus=0-3` would grant
# two physical cores, not four. Siblings 1,3,5,7 are left idle deliberately —
# k6 must never share a physical core with the server.
#
# `--cpus=4` is a redundant CFS cap; the cpuset is authoritative.
# `--memory-swap=4g` equals `--memory=4g`, which means NO SWAP: matching the
# challenge's stated rule, and making an over-budget run a hard OOM rather than
# a slow, unreproducible degradation.
#
# `--network host` means k6 reaches loopback directly. A published port would
# route through docker-proxy — the reverse-proxy hop the challenge explicitly
# rewards deleting, and measuring Docker rather than umbral.
set -euo pipefail

NAME=umbral-bench
HERE="$(cd "$(dirname "$0")" && pwd)"
BACKEND="$(cd "$HERE/../backend" && pwd)"
DATA="$HERE/data"
IMAGE="$NAME:latest"

mkdir -p "$DATA"

# Everything needed to diagnose a container that is not serving. A dead
# container and a slow one look identical from the outside, so print the state
# (status / exit code / OOMKilled) and then the log, which is where a failed
# cold start says what it was doing — migration, seeding, or a boot check.
describe_failure() { # describe_failure <headline>
  echo "ERROR: $1" >&2
  docker inspect -f 'container: status={{.State.Status}} exit={{.State.ExitCode}} oom={{.State.OOMKilled}}' "$NAME" >&2 || true
  echo "--- last 40 log lines ---" >&2
  docker logs --tail 40 "$NAME" >&2 || true
}

case "${1:-serve}" in
  build)
    echo "building release binary on the host ..."
    (cd "$BACKEND" && cargo build --release --bin umbral-bench-prod)
    echo "building image ..."
    docker build -t "$IMAGE" "$BACKEND"
    ;;
  serve)
    # Optional database override, passed through to `docker run -e`, so it has
    # to be a whole ASSIGNMENT:
    #   UMBRAL_DATABASE_URL='UMBRAL_DATABASE_URL=sqlite:///data/other.db' run.sh serve
    # The natural-looking bare form is not a working no-op: docker accepts an
    # `-e` value with no `=` as a variable NAME, sets nothing, and the
    # container quietly serves the default database. A silent wrong-database
    # run is exactly the failure this rig cannot afford, so refuse it loudly.
    DB_ENV="${UMBRAL_DATABASE_URL:-UMBRAL_DATABASE_URL=sqlite:///data/bench-run.db?mode=rwc}"
    case "$DB_ENV" in
      UMBRAL_DATABASE_URL=*) ;;
      *)
        echo "ERROR: UMBRAL_DATABASE_URL must be a whole assignment, e.g." >&2
        echo "       UMBRAL_DATABASE_URL='UMBRAL_DATABASE_URL=sqlite:///data/other.db' run.sh serve" >&2
        echo "       (a bare URL is passed to docker as a variable NAME, which docker" >&2
        echo "       accepts while setting nothing). Got: $DB_ENV" >&2
        exit 2
        ;;
    esac
    # A missing database is NOT a neutral condition. `?mode=rwc` makes the app
    # silently CREATE one, seed it at the default 2000-user scale, and serve
    # that — so a hold would measure the wrong dataset while looking perfectly
    # healthy. The first boot of a fresh silo is legitimately cold (Task 6's
    # Step 5 is exactly that), so it is allowed — but only when asked for, so
    # that an unintentional cold start cannot happen quietly. This runs before
    # the container is touched, so a refusal leaves a running silo alone.
    db_url="${DB_ENV#*=}"
    db_file=""
    case "$db_url" in
      sqlite:///data/*)
        rel="${db_url#sqlite:///data/}"
        rel="${rel%%\?*}"
        db_file="$DATA/$rel"
        ;;
    esac
    if [ -n "$db_file" ] && [ ! -f "$db_file" ]; then
      if [ "${BENCH_ALLOW_COLD_START:-0}" != "1" ]; then
        echo "ERROR: no database at $db_file" >&2
        echo "       The app would create an EMPTY one (the URL carries ?mode=rwc)," >&2
        echo "       seed it at the default scale, and serve that — a hold against it" >&2
        echo "       would look healthy and measure the wrong dataset." >&2
        echo "       Intentional first boot, or a deliberate reset?" >&2
        echo "         BENCH_ALLOW_COLD_START=1 $0 serve" >&2
        exit 2
      fi
      echo "WARNING: cold start — no $db_file; it will be created and seeded at" >&2
      echo "         the default scale. No number from this container is the baseline." >&2
    fi
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    docker run -d --name "$NAME" \
      --cpuset-cpus=0,2,4,6 \
      --cpus=4 \
      --memory=4g --memory-swap=4g \
      --network host \
      -v "$DATA:/data" \
      -e "$DB_ENV" \
      "$IMAGE" serve
    echo "started $NAME (pid $(docker inspect -f '{{.State.Pid}}' "$NAME"))"
    echo "waiting for readiness ..."
    for _ in $(seq 1 60); do
      if curl -sf "http://127.0.0.1:8114/api/ping" >/dev/null 2>&1; then
        echo "ready on 8114"
        exit 0
      fi
      # A container that has already EXITED will never become ready, and a
      # cold start that fails (a bad URL, a boot check, a missing COPY) fails
      # in the first second. Waiting the full 60 s buys nothing but a minute of
      # silence, so report as soon as the process is gone.
      if [ "$(docker inspect -f '{{.State.Running}}' "$NAME" 2>/dev/null)" != "true" ]; then
        describe_failure "container $NAME exited before becoming ready"
        exit 1
      fi
      sleep 1
    done
    describe_failure "not ready after 60s"
    exit 1
    ;;
  stop)
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    echo "stopped"
    ;;
  shell)
    docker exec -it "$NAME" /bin/bash
    ;;
  verify)
    # Assert the container actually got the envelope we asked for, and fail
    # non-zero if it did not. Printing the values is not enough: a typo in the
    # cpuset produces a perfectly healthy-looking container measuring two
    # physical cores instead of four, and nothing in its output would look
    # wrong. Run this before trusting any number.
    # `docker container inspect`, not plain `docker inspect`: the bare form
    # resolves across object types, so `umbral-bench` matches the IMAGE
    # `umbral-bench:latest` when no container exists — the guard would pass and
    # the field template would then fail on an object with no HostConfig.
    if ! docker container inspect "$NAME" >/dev/null 2>&1; then
      echo "ERROR: no container named $NAME — run 'run.sh serve' first." >&2
      exit 1
    fi

    fail=0
    check() { # check <label> <expected> <actual>
      if [ "$2" = "$3" ]; then
        printf 'ok    %s == %s\n' "$1" "$3"
      else
        printf 'FAIL  %s: expected %s, got %s\n' "$1" "$2" "$3" >&2
        fail=1
      fi
    }

    read -r cpuset nano quota period memory swap running oom <<<"$(docker inspect -f \
      '{{.HostConfig.CpusetCpus}} {{.HostConfig.NanoCpus}} {{.HostConfig.CpuQuota}} {{.HostConfig.CpuPeriod}} {{.HostConfig.Memory}} {{.HostConfig.MemorySwap}} {{.State.Running}} {{.State.OOMKilled}}' \
      "$NAME")"
    nano=${nano:-0}
    quota=${quota:-0}
    period=${period:-0}

    # `--cpus=4` is recorded as nano-cpus or as quota/period depending on the
    # Docker version — both express the same 4-CPU CFS cap, so assert the
    # number of CPUs it comes to and print which form was stored.
    if [ "$nano" -gt 0 ]; then
      cpus=$((nano / 1000000000))
      cap_desc="$nano nano-cpus"
    elif [ "$quota" -gt 0 ] && [ "$period" -gt 0 ]; then
      cpus=$((quota / period))
      cap_desc="$quota/$period quota/period"
    else
      cpus=0
      cap_desc="unset"
    fi

    if [ "$running" = "true" ]; then
      nproc_in="$(docker exec "$NAME" nproc)"
      mem_max="$(docker exec "$NAME" cat /sys/fs/cgroup/memory.max)"
      swap_max="$(docker exec "$NAME" cat /sys/fs/cgroup/memory.swap.max)"
      meminfo="$(docker exec "$NAME" grep MemTotal /proc/meminfo)"
    else
      nproc_in="<not running>"
      mem_max="<not running>"
      swap_max="<not running>"
      meminfo="<not running>"
    fi

    echo "=== requested by run.sh (docker inspect) ==="
    echo "cpuset-cpus        $cpuset"
    echo "cpu cap            $cap_desc  ->  $cpus CPUs"
    echo "memory             $memory"
    echo "memory-swap        $swap"
    echo "=== as the container sees itself ==="
    echo "nproc              $nproc_in"
    echo "memory.max         $mem_max"
    echo "memory.swap.max    $swap_max"
    # /proc/meminfo is NOT namespaced by cgroup: it reports the HOST's RAM on
    # any runtime without lxcfs, so it is informational only and the cgroup
    # file above is the value that binds.
    echo "MemTotal           $meminfo   (host RAM, not the limit)"
    echo "OOMKilled          $oom"
    echo "=== assertions ==="
    check "cpuset-cpus" "0,2,4,6" "$cpuset"
    check "cpu cap (CPUs)" "4" "$cpus"
    check "memory (bytes)" "4294967296" "$memory"
    check "memory-swap == memory (no swap)" "$memory" "$swap"
    check "nproc inside the container" "4" "$nproc_in"
    check "cgroup memory.max inside the container" "4294967296" "$mem_max"
    # 0 inside the cgroup is the mechanism behind `--memory-swap=4g`: with no
    # swap to fall back on, an over-budget run is killed rather than degraded.
    check "cgroup memory.swap.max inside the container" "0" "$swap_max"
    check "OOMKilled" "false" "$oom"
    check "container running" "true" "$running"

    if [ "$fail" -ne 0 ]; then
      echo "ENVELOPE MISMATCH: every number from this container is invalid. Fix the envelope before running anything." >&2
      exit 1
    fi
    echo "envelope verified."
    ;;
  *)
    echo "usage: run.sh [build|serve|stop|shell|verify]" >&2
    exit 2
    ;;
esac

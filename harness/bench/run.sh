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
    # File descriptors. Docker's default is 1024, and the workload needs far
    # more: every keep-alive connection is one descriptor, the reference
    # benchmark runs on the order of 15,000 concurrent connections, and the
    # plan's limit search doubles from 2,500 toward 40,000. At the default the
    # container held ~1,000 and the server logged `accept error: Too many open
    # files` — with p95/p99 still clean, so it did not even look like
    # saturation. Every hold above ~1,000 VUs would have measured Docker's
    # default and reported it as umbral's ceiling.
    #
    # 524288:524288 is the largest this host can grant, measured rather than
    # guessed: the invoking shell's limit is 1048576, but the container's rlimit
    # is set by the daemon, and dockerd's own hard limit is 524288
    # (`grep 'Max open files' /proc/$(pgrep -x dockerd)/limits`) — asking for
    # more makes `docker run` fail outright. At 524288 the silo's descriptor
    # limit cannot be what stops a run: 13x the 40,000-VU cap the search
    # targets, 35x the reference workload's connection count. Do not tidy this
    # back to the default.
    docker run -d --name "$NAME" \
      --cpuset-cpus=0,2,4,6 \
      --cpus=4 \
      --memory=4g --memory-swap=4g \
      --ulimit nofile=524288:524288 \
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
      # The app's OWN limit (pid 1's), not an exec shell's — this is the number
      # that decides whether a high-VU hold can keep its connections. At
      # Docker's default (1024) the server logs "Too many open files" while
      # p95/p99 stay clean, which reads as a mystery rather than a ceiling.
      nofile_in="$(docker exec "$NAME" awk '/Max open files/{print $4}' /proc/1/limits)"
      meminfo="$(docker exec "$NAME" grep MemTotal /proc/meminfo)"
    else
      nproc_in="<not running>"
      mem_max="<not running>"
      swap_max="<not running>"
      nofile_in="<not running>"
      meminfo="<not running>"
    fi

    # The client side has a limit of its own, and a client-side EMFILE is
    # indistinguishable in a k6 summary from the server rejecting load — which
    # is why the plan records `ulimit -n` with every result. A k6 started from
    # this shell inherits this number.
    host_nofile="$(ulimit -n)"

    echo "=== requested by run.sh (docker inspect) ==="
    echo "cpuset-cpus        $cpuset"
    echo "cpu cap            $cap_desc  ->  $cpus CPUs"
    echo "memory             $memory"
    echo "memory-swap        $swap"
    echo "=== as the container sees itself ==="
    echo "nproc              $nproc_in"
    echo "memory.max         $mem_max"
    echo "memory.swap.max    $swap_max"
    echo "nofile (soft)      $nofile_in"
    # /proc/meminfo is NOT namespaced by cgroup: it reports the HOST's RAM on
    # any runtime without lxcfs, so it is informational only and the cgroup
    # file above is the value that binds.
    echo "MemTotal           $meminfo   (host RAM, not the limit)"
    echo "OOMKilled          $oom"
    echo "=== on the host (inherited by k6) ==="
    echo "ulimit -n          $host_nofile"
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
    # Must match the --ulimit on `docker run`: a hold above ~1,000 VUs needs
    # more descriptors than Docker's 1024 default, and the failure it causes
    # does not look like saturation.
    check "container nofile limit (soft)" "524288" "$nofile_in"
    check "OOMKilled" "false" "$oom"
    check "container running" "true" "$running"

    case "$host_nofile" in
      *[!0-9]*) ;; # `unlimited` and friends: never the binding constraint
      *)
        if [ "$host_nofile" -lt 20000 ]; then
          echo "WARNING: the host nofile limit ($host_nofile) is below the ~20000 a" >&2
          echo "         high-VU hold needs. k6 would fail client-side with EMFILE, which" >&2
          echo "         a k6 summary reports the same way as the server rejecting load." >&2
          echo "         Raise it in the shell you run k6 from: ulimit -n 65535" >&2
        fi
        ;;
    esac

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

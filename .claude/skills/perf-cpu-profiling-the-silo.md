---
name: perf-cpu-profiling-the-silo
description: Use when you need a CPU profile (perf) of the bench silo or any process on this host — covers the paranoid=4 workaround, correct symbolization, and the symbol traps that produce plausible-looking lies
---

# CPU-profiling the bench silo with perf

## Context

The bench host forbids unprivileged profiling: `/proc/sys/kernel/perf_event_paranoid=4` (Ubuntu hardening) and no passwordless sudo. `perf record` from the host fails; the harvester needs a way in that changes **no host setting**. Also: the measured app runs in a Docker container whose glibc differs from every other container on the box, and perf's default symbolization will silently resolve against the *wrong file*, producing hot-function names that look real and are not.

## Approach

1. **Privileged sidecar, pinned off the measured cores.**

   ```bash
   docker run -d --name umbral-prof --privileged --pid=host --cpuset-cpus=12 \
     ubuntu:24.04 sleep infinity
   docker exec umbral-prof bash -c 'apt-get update -qq && apt-get install -y -qq linux-tools-common linux-tools-generic'
   ```

   `--privileged` grants CAP_PERFMON (bypasses `perf_event_paranoid`) and CAP_SYS_PTRACE (bypasses yama `ptrace_scope=1`). `--pid=host` lets perf see and attach to the containerized app by its *host* pid (`docker inspect -f '{{.State.Pid}}' umbral-bench`). `--cpuset-cpus=12` puts the profiler on an E-core outside the silo, same rule as the thermal sampler.

2. **The Ubuntu version-wrapper lies.** On 24.04, `/usr/bin/perf` is a wrapper that looks for tools matching the *running* kernel exactly (`6.8.0-136`) while the archive ships `6.8.0-147`; it exits 2 with "perf not found for kernel". The real binary is at `/usr/lib/linux-tools/6.8.0-147-generic/perf` and works fine against 6.8.0-136 for user-space sampling. Call that path directly:

   ```bash
   PERF=/usr/lib/linux-tools/6.8.0-147-generic/perf   # run inside umbral-prof
   ```

3. **Capture.** One long hold at 15,000 VUs; sample only in steady state (start after the 60 s ramp, ~100 s in). All four silo cores at once because PG shares the app's cpuset, so one capture holds both sides:

   ```bash
   docker exec umbral-prof $PERF record -F 999 -C 0,2,4,6 -o /tmp/prof.data -- sleep 180
   ```

   `-C` records every process scheduled on those CPUs; filter by `comm` in the report. Expect small extras (chrome, swapper) — the cpuset does not *reserve* the cores.

4. **Symbolize against the TARGET container's dso's.** Copy them out and register them by build-id — this is the only method that cannot mis-resolve:

   ```bash
   docker cp umbral-bench:/usr/lib/x86_64-linux-gnu/libc.so.6 /tmp/symfs-libc.so.6   # NOTE the path!
   docker cp umbral-bench:/app/umbral-bench-prod /tmp/symfs-app
   docker cp /tmp/symfs-libc.so.6 umbral-prof:/symfs/usr/lib/x86_64-linux-gnu/libc.so.6
   docker cp /tmp/symfs-app      umbral-prof:/symfs/app/umbral-bench-prod
   docker exec umbral-prof $PERF buildid-cache --add /symfs/usr/lib/x86_64-linux-gnu/libc.so.6 \
                                                --add /symfs/app/umbral-bench-prod
   ```

   Then report **without** `--symfs` (build-id matching wins over anything path-based):

   ```bash
   docker exec umbral-prof $PERF report -i /tmp/prof.data --stdio --no-children \
     -s comm,dso,symbol --percent-limit 0.1
   ```

## Pitfalls

- **glibc is stripped.** Samples inside unexported malloc internals (`_int_malloc`, `_int_free`, `tcache_*`) are labeled with the nearest *preceding exported* symbol — which on glibc 2.35 means rows like `pthread_setcancelstate 2.2%`, `sem_open 1.2%`, `pthread_rwlock_rdlock 2.3%` in a tokio app that contains no such calls. **Do not report those names.** The addresses tell the truth: `malloc` sits at `0xa5060–0xa53a0`, `free` at `0xa53a0–0xa5700`, and the internals block is roughly `0xa1000–0xa5000`; bucket by address range to size the allocator's real share.
- **The recorded dso path is `/usr/lib/x86_64-linux-gnu/libc.so.6`**, not `/lib/...` — even though `/lib` symlinks there, `/proc/<pid>/maps` (and `perf buildid-list`) records the `/usr/lib` spelling. A `--symfs` tree built under `/lib/...` silently misses, and perf falls back to the *sidecar's own* libc (a different glibc version!) → every symbol becomes a raw address or a lie. When in doubt: `perf buildid-list -i data | grep -v '\['` shows the exact paths perf will look for.
- **Rust release builds are symbol-rich but DWARF-free.** `debug = 0` keeps the symtab (function names resolve), but there are no source lines and no frame pointers. Flat `--no-children` self-time is trustworthy and is what you want; `-g`/`--call-graph fp` yields garbage. If callers are genuinely needed, rebuild a *diagnostic* binary with `RUSTFLAGS="-C force-frame-pointers=yes"` — never measure with it.
- **LBR (`-b`) is a trap on this rig.** It inflates the data ~10× (574 MB for 180 s), makes `perf report` take 10+ minutes, and the branch-mode output does not resolve into the flat view you actually read. Skip it; use flat profiles plus targeted experiments.
- **`perf report` is single-threaded but unconfined.** Do not run it *during* a measurement hold — it competes for the silo's P-cores. Generate reports between arms.
- **A capture and its report must name which binary they describe.** The `umbral-bench:latest` image is whatever the last sweep arm left there — check `docker exec umbral-bench sha256sum /app/umbral-bench-prod` against the arms' recorded `envelope.binary.sha256` before interpreting anything.

## See also

- `harness/bench/results/reports/2026-10-14-db-bucket-attribution.md` — the client/server split that motivated profiling.
- Ledger entries for 2026-10-14 (SDD ledger) — the first two profiles and what they found.
- `harness/bench/run.sh` — the silo definition the profiler must not disturb.

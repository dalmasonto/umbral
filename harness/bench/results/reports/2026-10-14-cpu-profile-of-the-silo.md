# Bench rig v2 — the first CPU profile: where the four cores actually go

**Date:** 2026-10-14 · **Branch:** `bench-rig-v2` · **Diagnosis, not measurement** — proportions from a 180-second sample window, not quotable as latency. **Profiled binary:** the Cycle-A join build (`245ac28e`) — the sweep had left `umbral-bench:latest` pointing at arm B2's image; the *shape* below (diffuse app CPU + one concentrated allocator pool) is not query-shape dependent, but the baseline binary is being re-profiled for the record.

## Method

`perf record -F 999 -C 0,2,4,6`, 180 s of steady state inside a 15,000-VU hold, capturing every process on the silo's four cores at once — the app and Postgres share the cpuset, so one capture holds both sides. 715,287 samples. The profiler runs in a privileged sidecar on an E-core; no host setting was changed (`perf_event_paranoid=4` is bypassed by CAP_PERFMON, not disabled). Symbolization pitfalls — glibc's stripped allocator internals getting labeled with the nearest exported symbol, and the recorded dso path being `/usr/lib/...` rather than `/lib/...` — are documented in `.claude/skills/perf-cpu-profiling-the-silo.md`; the allocator bucket below is bucketed by address range for exactly that reason.

## The split (share of all four cores)

| owner | share | of which |
|---|---|---|
| **app** (`tokio-rt-worker`) | **≈67%** | 30.0% app code · 25.3% libc · 11.7% kernel |
| **Postgres** | **≈27%** | 16.5% code · 8.7% kernel · 1.5% libc |
| idle / desktop | ≈4% | swapper 1.4%, chrome 0.8% (the cpuset does not *reserve* the cores; other host processes are visible and small) |

Two facts stand out against the earlier attribution (2,500 VUs), where server-side execution was ~106 µs/request ≈ 9.5% of the budget:

1. **At saturation Postgres is a first-class part of the four-core budget (~27%), not a footnote.** PG is pinned to the same cpuset, so its parser, planner, executor and `palloc` compete with the app directly. Server-side query-cheapening *is* capacity work.
2. **The app's own CPU is diffuse.** Its 30% splits into thousands of sub-0.1% symbols — ~58% of app-code samples sit below a 0.1% per-symbol cutoff. There is no single framework function to optimize; the wins must come from removing whole passes, not tuning leaves.

## The one concentrated pool: glibc's allocator

Bucketing the app's libc rows by address (the exported-symbol labels in that region are junk — a tokio app does not spend 2% of the box in `sem_open`): the sampled addresses sit inside `malloc`/`free` and the unexported `_int_malloc`/`_int_free`/`tcache_*` block.

- **≥17.3% of all four cores** is in glibc allocator internals (a lower bound — it counts only per-symbol rows ≥0.1%, and the allocator's mass concentrates in a few addresses, so unlike the app's diffuse code most of it survives the cutoff). That is **roughly a quarter of the app's entire CPU budget**, spent mediating small-object churn: JSON payload values, decoded rows, response headers, error paths.
- Consistent with it: app-code rows `Vec::clone` (~1.7%) and `String::clone` (~1.0%) are among the largest *named* app symbols — every one of those is a malloc + memcpy + free.

## What it implies for the workstream

1. **The allocator is the one lever with a measured, concentrated cost.** Cycle B tests it directly: the same binary plus a mimalloc global allocator — application-level configuration any umbral user can apply, and (more durably) the measurement of how big the prize is for allocation-*count* reduction inside the ORM and framework glue.
2. **Individual-function optimization is bounded.** With the app's mass spread under 0.1% per symbol, no leaf-tuning session can plausibly move RPS; the plausible framework wins are pass-eliminations (e.g. the select_related prefetch's row → `serde_json::Value` → typed `Deserialize` double materialization, 2–4 prefetches per request path).
3. **Postgres's 27% is real but equally diffuse** (hash lookups, btree compares, `palloc`/`AllocSetAlloc`, thousands of executor symbols). It will not yield to one change; it yields to issuing fewer/cheaper statements.

## Caveats

- One binary, one host, one 180 s window; ratios, not absolute times.
- The profiled binary is the join build (see header); its *mix* of work differs from the baseline's in exactly the four related-load sites — the baseline re-profile is owed and will be attached to the ledger when it lands.
- Kernel time (≈20% across app+PG) is real but largely irreducible here: loopback TCP, syscalls, scheduling. It moves only with fewer/smaller messages.

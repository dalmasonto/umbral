# Bench rig v2 — the VU ramp on the 20k shuffled pool

**Date:** 2026-10-14 · **Branch:** `bench-rig-v2` · **Backend:** Postgres 18.6 (sibling container, port 55432) · **Runs:** one per level (2,500 / 5,000 / 7,500), by operator directive ("only run a single test, not 3") · **Result files:** `4aea727a-20261014T125142Z` / `T130501Z` / `T131820Z`

## Read this first

**One run per level** — no spread per point. The host's run-to-run p99 noise is ~20–27%, so a single point's tail can move by ~1.3× without anything having changed. The *shape* of the curve is the product; no single point is a precise number.

**This series ran on a changed pool, deliberately:** one session per dataset user (**20,000**, minted shuffled), so each level's active identities are a random sample of the user base rather than the same lowest user ids. That makes it **incomparable to every earlier result** — including this morning's 862.6 RPS baseline (10,000-entry pool in login order); `compare.sh` refuses the diff by construction (pool size is a gated fact).

**RPS is the signal here, uniquely.** At 2,500 VUs the offered rate sat below capacity, so RPS measured demand. Each level above raises the offered rate; the ramp exists to find where the server stops keeping up.

## The numbers

| VUs | offered (run-average) req/s | delivered RPS | delivered/offered | p50 | p95 | p99 | max | errors | checks |
|---|---|---|---|---|---|---|---|---|---|
| 2,500 | ≈866 | **866.222** | 100.0% | 0.989 ms | 2.349 ms | 3.588 ms | 17.3 ms | 0 / 343,230 | 100% |
| 5,000 | ≈1,732 | **1,725.472** | 99.6% | 1.001 ms | 1.963 ms | 3.530 ms | 29.8 ms | 0 / 685,871 | 100% |
| 7,500 | ≈2,598 | **2,592.503** | 99.8% | 1.021 ms | 2.141 ms | 3.975 ms | 16.3 ms | 0 / 1,029,017 | 100% |

## What the ramp shows

**RPS tracks the offered rate to within 0.4% at every level — and the server's latency does not move.**

- **p50 is flat at ~1.0 ms across a 3× load increase** (0.989 / 1.001 / 1.021 ms). A server approaching its limit queues; this one does not, even slightly.
- **The tails are flat**: p95 1.96–2.35 ms, p99 3.5–4.0 ms — and the *ordering is non-monotonic* (p95 is best at 5,000 VUs, p99 best at 5,000 too), which is the signature of host noise, not load.
- **Zero failed requests and 100% checks across 1.03 million requests** at the top level, including the write path (~80k likes/posts over the 7,500-VU run).

**Consequence: there is no saturation through 7,500 VUs, and the earlier estimate that saturation sat near ~7,350 VUs is falsified.** That estimate came from a CPU-bound model (≈1.35 CPU-seconds per second at 865 RPS → four cores ≈ 2,600 RPS). The workload is dominated by *waiting* on the database, not by CPU, so the async runtime sustains more concurrent requests than the CPU model predicted — and Postgres, on the same four cores, evidently keeps up at ~13,000 statements/s. **Where the ceiling actually is remains unknown; it is higher than 2,592 RPS.** The next levels (10,000+) or the zero-think-time saturation exercise will find it.

**For the mission, this is the useful half of the news:** the framework delivered 3× the baseline's throughput at *identical* per-request latency, and the improvement work now has a measurement where a faster server shows up directly — anything that raises capacity or cuts client-side latency at 2,500 VUs.

## Provenance

- **Binary** `8222bfa8…d68a60`, built from `4aea727a` (the 20k shuffled pool + the `UMBRAL_DB_LOG_STATEMENTS` knob), `git_dirty: false` on all three records.
- **Envelope** verified before the series (`run.sh verify`, exit 0): cpuset 0,2,4,6, 4 GB, no swap, nofile 524288, `--network host`; k6 on cores 8–11.
- **Pool** 20,000 (recorded in every result); preflight passed at every level; warm-up (1,000 VUs, 2 min) preceded each hold; 180 s cooldown between levels.
- **Thermal/load**: sampler-covered windows, peaks ~100 °C (the machine ran hot from the day's seeding/build work); `loadavg_before_run` 5.97 / 1.59 / 2.44 — the 2,500-VU run ran *hottest and busiest* and still posted p95 2.35 ms, one more datum against a clean load/heat explanation of tail movement (see the Postgres baseline report's §8.3).
- Throttle counters recorded per result for diffing.

## What this does and does not license

- **Licenses**: the saturation *shape* (rising, not yet bent); a capacity floor of **≥2,592 RPS at 7,500 VUs** under this workload and envelope; and the 2,500-VU point as the fixed comparison level for Phase 3 cycles (same pool, same mix, same binary family).
- **Does not license**: point-precise numbers (one run each); any comparison to the pool-10k series or any SQLite number; any claim about where saturation *is*; a "25,000 RPS" projection from this curve.

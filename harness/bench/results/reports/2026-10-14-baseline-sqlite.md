# Bench rig v2 — SQLite baseline

**Date:** 2026-10-14 · **Branch:** `bench-rig-v2` · **Commit:** `92f278df` · **Result files:** `harness/bench/results/1819ddbc-20261014T01{3509,4450}Z.json`, `52e800a9-20261014T012528Z.json`

---

## The number

| Run | RPS | p95 | p99 | Errors | Verdict |
|---|---|---|---|---|---|
| 1 | 865.38 | 3.284 ms | 4.749 ms | 0 | passed |
| 2 | 865.09 | 3.318 ms | 4.747 ms | 0 | passed |
| 3 | 864.54 | 3.389 ms | 4.815 ms | 0 | passed |

**Spread: 0.098% on RPS** (p95 3.2%, p99 1.4%). Thresholds were `p95<500 ms`, `p99<1000 ms`, `failed<1%` — all three cleared with roughly two orders of magnitude of margin.

## ⚠️ What this number is, and what it is not

**865 RPS is what 2,500 virtual users asked for. It is not what umbral can do.**

The workload offers `2.17 requests/iteration ÷ 5.5 s think` = **0.3945 req/s per VU**, i.e. 986 RPS flat at 2,500 VUs. Averaged over the run, offered is **≈866 RPS** and delivered is **865.4 — 99.9%**, i.e. **100% within ±0.1%**.

(An earlier draft of this report said ≈872 offered and "99% delivered". That used a trapezoidal VU trajectory, which is ~0.8% high: k6's own counters give a mean of **2,195.0** concurrent VUs, not 2,211.5, because its elapsed window is 396.2 s rather than 390 s and ramp-down VUs finish their last iteration. The run's own iteration rate — 398.94/s delivered against 399.09/s offered — settles it, as does the in-flight concurrency figure of **1.34 requests**.)

Corroborated inside the run's own numbers: mean latency × throughput ≈ 1.34 CPU-seconds per second, **at most a third of the silo's four CPUs.**

So this is a **demand** figure, not a capacity figure. umbral kept up with everything offered and never approached its limit. Two consequences worth stating plainly:

1. **RPS is a one-sided regression detector at this operating point, not a signal.** Throughput is pinned by think time — a *twofold* server speedup would move RPS by less than 0.1%. Optimization work will show up in **p95/p99**, and any "umbral got faster" claim argued from RPS alone is structurally incapable of registering its own win.
2. **No earlier number in this project is comparable to this one.** The old harness ran its load generator inside the server process, bypassed authentication with a header, and kept telemetry middleware in the measured path. Its best reading was 2,550 RPS on a 20-thread host; that number measured the laptop, not the framework.

## Why this is trustworthy

- **Heat-soak is ruled out by evidence, not assumption.** All three runs carry `thermal.source: sampler` — a real in-run window (398/399/399 s) opened cold at 55/57/55 °C — and the CPU throttle counters are **byte-identical across all three records** (43441 / 186551, by-CPU included). That covers runs 2–3 and both cooldowns; run 1 is covered by the cold-start and direction argument (it had the highest peak *and* was the fastest). This was the single biggest threat to reproducing anything on a laptop.
- **The envelope is asserted, not assumed.** `run.sh verify` checks the cpuset against the literal `0,2,4,6` — four *distinct physical* P-cores on this chip, where the obvious `0-3` would have granted two.
- **The measured artifact is pinned.** `git_sha` differs between run 1 and runs 2–3 (a docs commit landed mid-sequence), but the **binary sha256 `bcc4479a…2418eb` is identical across all three** — re-hashed live inside the container during review. The binary is what was measured.
- **The comparator is not a rubber stamp.** Review fed `compare.sh` in-memory mutations: it returns `1` on a 30% RPS regression, `3` on a mix or scale mismatch, `2` on a non-result file.
- **The gate's fixture survived.** `harness/backend/bench.db` is sha256-unchanged at 50 users / 150 posts, so all eight earlier tasks' verification still holds.

## Configuration

| | |
|---|---|
| Backend | SQLite (in-process, WAL, `synchronous=NORMAL`) |
| Envelope | `--cpuset-cpus=0,2,4,6`, 4 GB, **no swap**, `--network host` |
| VUs | 2,500 |
| Dataset | 20,000 users / ~210,000 posts / ~156,000 likes / ~160,000 follows |
| Workload | 2.17 req/iter: feed read every iteration, one deeper read, 15% like, 2% create, one 3–8 s think |
| Auth | **real** session auth — `DbStore`, so a session-store read is inside every authenticated request |
| k6 | v2.3.0, pinned to cores 8–11 |

**Session auth is in the number.** All previous benchmarks bypassed it with an `x-bench-user` header.

## Honest caveats

- **Scale is 20,000, not the 50,000** the spec reserves for figures quoted outside the repo. A confirmation run at 50k is deferred; note it will not change the offered rate (only VUs or think time does), so it is about data shape, not headroom.
- **`SecurityPlugin` runs without HSTS or CSP** — two warnings per boot. The framework is telling the truth; the full production posture was not measured.
- **The database grows during a run** (~20 / ~150 rows per hold by design), and the write path is real contention.
- **A `404` is not a failure any threshold catches.** k6's `USERS`/`POSTS` must match the served dataset or a fraction of reads silently return nothing.

## Reproduction

Full sequence in `harness/bench/README.md`. In brief: seed a disposable copy to 20k, hand it to the silo, create the pool users *inside* the container and mint the session pool from the served file, wait for `[bench-seed] done:`, then three 5-minute holds with 2–3 minute cooldowns, each bracketed by the thermal sampler pinned to core 12.

## What's next

**The Postgres baseline**, same workload and VUs, database in a sibling container pinned to the *same* cpuset on port 55432. It is **not comparable** to this number — different architecture, each a fixed point only for itself. Then Phase 3 optimization cycles, reporting latency percentiles as the primary signal.

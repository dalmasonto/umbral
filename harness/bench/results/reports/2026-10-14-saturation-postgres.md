# Bench rig v2 — the saturation curve: where RPS stops rising

**Date:** 2026-10-14 · **Branch:** `bench-rig-v2` · **Backend:** Postgres 18.6 · **Runs:** one per level (2,500 / 5,000 / 7,500 / 10,000 / 15,000 / 20,000 VUs) · **Pool:** 20,000 sessions (the whole dataset; all active at 20k VUs)

## Read this first

**This is the measurement the 2,500-VU baseline could not be.** Below the knee, RPS measures what the load *asked for* (think-time-pinned demand); past it, RPS **is the server's capacity**. The knee is between 7,500 and 10,000 VUs; the ceiling is **≈3,580 RPS at 15,000 VUs**.

**One run per level** (operator directive), so no per-point spread. The host ran **throttled to ~2.0 GHz** (`cpu_current_mhz` 1,924–2,100 during the saturated levels), so the ceiling is a **floor for this host**, not the machine's best. Zero failed requests at every level, 1.3–1.4M requests per saturated run, 100% checks.

## The curve

| VUs | offered (run-avg) req/s | delivered RPS | delivered/offered | p50 | p95 | p99 | exit |
|---|---|---|---|---|---|---|---|
| 2,500 | ≈866 | 866.2 | 100.0% | 0.99 ms | 2.35 ms | 3.59 ms | passed |
| 5,000 | ≈1,732 | 1,725.5 | 99.6% | 1.00 | 1.96 | 3.53 | passed |
| 7,500 | ≈2,598 | 2,592.5 | 99.8% | 1.02 | 2.14 | 3.98 | passed |
| **10,000** | ≈3,464 | **3,437.8** | 99.2% | 2.27 | **58.1** | 93.5 | passed |
| **15,000** | ≈5,196 | **3,579.1** | **68.9%** | **1,311.8** | 1,628.6 | 1,806.2 | threshold_failed |
| **20,000** | ≈6,928 | **3,463.7** | **50.0%** | **2,848.7** | 3,439.4 | 3,687.1 | threshold_failed |

*(The two `threshold_failed` exits are real load results — p95 crossed its 500 ms limit because the server is saturated. That is the finding, correctly classified; no rig configuration failed.)*

## The shape

- **Up to 7,500 VUs: pure demand-tracking.** Delivered = 99.6–100% of offer at every level, p50 flat at ~1.0 ms. The server is not the constraint.
- **The knee (7,500 → 10,000): queueing onset.** RPS still tracks the offer (99.2%) but **p95 jumps 2 ms → 58 ms** — the first level where the server's own capacity bites.
- **Saturation (10,000 → 15,000): delivery decouples from demand.** +50% VUs buys **+4.1% RPS**; p50 goes to **1.3 seconds**; delivery falls to 68.9% of offer.
- **Super-saturation (15,000 → 20,000): throughput declines.** RPS *falls* to 3,463.7 (−3.2%) as the closed loop thins: 20,000 VUs × (5.5 s think + ~2.8 s latency) submits less work per second than 20,000 × (5.5 s + 1.3 s).

**Ceiling ≈ 3,580 RPS** — 4.2× the 862 RPS baseline — under this envelope and workload.

## Why this matters for the 25k mission

**The ceiling is CPU-bound**: 4 cores ÷ 3,579 RPS ≈ **1.12 core-seconds of CPU per request** at saturation. That matches the attribution report's decomposition: the ORM's *client-side* per-request cost (≈1,181 µs database bucket + serialization etc.) is what consumes the cores, and the database's own execution is ~106 µs of it. **A change that cuts the framework's CPU per request raises this ceiling proportionally** — a −20% CPU per request is a +25% ceiling. The 25k target is ≈7× above today's ceiling — the same ~6–7× the attribution's arithmetic pointed at.

**The demonstration level for optimizations changes with this report.** At 2,500 VUs a 5× faster server moves RPS by under 0.1% (demand-bound). **At 15,000 VUs, RPS is capacity** — a change either moves it or it doesn't. Cycles are re-based there.

## Provenance and caveats

- **Every level ran the same measured binary**, `umbral-bench-prod` sha256 `8222bfa8…d68a60` (verified in each result's `envelope.binary.sha256`); `git_dirty: false` throughout; envelope verified before the series.
- Pool 20,000 recorded in every result (at 20k VUs every pooled session is active — the whole user base).
- Thermal/clock recorded per result; the saturated levels ran at ~1.9–2.1 GHz → **the ceiling is a floor**; a cool-host rerun would read higher.
- A day-long latency drift exists at *fixed* RPS (documented in the ledger: +0.1 ms/h at the p50 across the afternoon, mechanism unidentified). It does not affect the *shape* conclusions here (the levels are 3–50× apart in effect) but bounds precision.
- **Not comparable to**: the pool-10k series (`compare.sh` refuses), any SQLite result, or any earlier harness.

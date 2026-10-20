# Bench rig v2 — the post-wins ramp: the old knee is now a free point

**Date:** 2026-10-15 · **Branch:** `bench-rig-v2` · **Binary:** `92b7f6b5` (all three wins: allocator +20.5%, post-page statement cut +13.8%, session cache +8.3%) · **Design:** one hold per level, the standard recipe (cooldown, warm-up, 5-minute hold, recorded with full facts). A ramp maps the curve; it does not compare arms.

## The measurement, against the pre-wins curve

| concurrent active users | pre-wins RPS | pre-wins p95 | **post-wins RPS** | **post-wins p50** | **post-wins p95** | errors |
|---|---|---|---|---|---|---|
| 7,500 | 2,592.5 | 2.14 ms | 2,590.4 | 0.77 ms | 2.23 ms | 0 |
| 10,000 | 3,437.8 | **58.1 ms** ← the old knee | **3,454.7** | **0.79 ms** | **2.48 ms** ← now free | 0 |
| 12,500 | — (not run; knee established at 10k) | — | **4,285.8** | 1.02 ms | **104.3 ms** ← the new knee | 0 |
| 15,000 | 3,580 (ceiling, throttled) | ~2.1 s | ~4,600 (the Cycle-F arms) | ~0.5 s | ~1.0 s | 0 |

**Two facts carry the answer:**

1. **The old knee point became a free point.** 10,000 concurrent active users — which pre-wins cost 58 ms at p95 (the first place the server's capacity bit) — now delivers at **p95 2.5 ms, p50 0.8 ms, zero errors, 100% checks**: a ~23× better tail at the same load. The safe region's edge moved with it.
2. **The knee moved to between 10,000 and 12,500.** At 12,500 the tail jumps to 104 ms (queueing onset, still zero errors and full delivery); at 15,000 the system is saturated by construction.

## The safe-capacity answer (measured, not projected)

**Safely ≈ 10,000–11,000 concurrently active users on these four throttled P-cores — at single-digit-millisecond tails** (that is ~4,000 requests/s at this workload's 0.3945 req/s per active user). The pre-wins system's comparable point was ~7,500; the wins bought roughly **+33–40% in safe concurrent users**, on top of the +50% in raw capacity.

Converting to registered users needs the app's duty cycle and is arithmetic on traffic shape, not a measurement: a consumer site with 5–10% of registered users active in the peak hour and a third of those concurrent at once puts **~10,000 concurrent ≈ 300k–600k registered users with a normal daily rhythm**. For a business-hours-heavy internal tool the multiple is far smaller.

**Hardware caveat:** every number here is four P-cores of a laptop sharing a desktop session, running throttled for most of the run. A dedicated server CPU would do materially better per core; the *curve shape* (free → knee → saturation) is what transfers, not the absolute RPS.

## How to read the level of the numbers

The 7,500 and 12,500 holds pass the workload's own latency thresholds; the arms' RPS figures sit ~15–20% below the quiet-host runs of the previous day because the host was noisy (desktop session). The p50/p95 at 7,500–10,000 (0.77–0.8 ms / 2.2–2.5 ms) are, however, essentially host-noise-independent — they are indistinguishable from the light-load baseline — which is exactly what "safe" should look like.

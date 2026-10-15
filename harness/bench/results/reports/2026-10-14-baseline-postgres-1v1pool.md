# Bench rig v2 — the Postgres baseline, re-taken on a 1:1 session pool

**Date:** 2026-10-14 · **Branch:** `bench-rig-v2` · **Result:** `harness/bench/results/77e45a9d-20261014T120420Z.json` · **Backend:** Postgres 18.6 (sibling container, port 55432, stock settings) · **VUs:** 2,500 · **Holds:** 1

## Read this first — what the number can and cannot answer

**862.6 RPS at 2,500 VUs is a demand figure, not a capacity figure.** The ratified mix offers roughly **866 requests/s averaged over the run's trajectory** (60 s ramp + 5-minute hold + 30 s ramp-down; steady-state at 2,500 VUs it is ≈986/s). The server delivered **99.9% of what the load asked for** — 343,110 requests against an expected ≈342,900 for the iterations actually completed, zero dropped iterations. **A twofold server speedup would move this RPS by under 0.1% at this operating point.** The metric that can see a server-side change is p95/p99; RPS is a regression guard. (The VU ramp — 2,500 → 5,000 → 10,000, one run per level — is the measurement that finds where the offered rate finally exceeds capacity; it follows this baseline.)

**This is ONE hold, by operator directive** ("only run a single test, not 3" for the fix-and-ramp window). It therefore has **no spread of its own**. The host's observed run-to-run p99 spread is ~20–27% (two independent sightings: the SQLite baseline set and the six-run Postgres series), so no p99 claim tighter than that is supportable from a single record.

**Not comparable to any SQLite number** (in-process database vs a TCP round trip — different architectures; `compare.sh` refuses across backends), and **not comparable to the earlier ≈863 RPS Postgres baseline**. That one ran the pre-fix **200-entry** session pool, where `(VU-1) % 200` handed each session identity to **12–13 concurrent VUs** while the rig's own docs claim one coherent user per VU; this one runs a **10,000-entry pool, 1:1 with the ramp's peak**. The rig now refuses that diff by construction: the old records carry no `session.pool_size`, and a missing pool size must never read as "matches" (`compare.sh` exit 3). Both numbers sit in the same band — which is expected, because RPS is think-pinned — but **no delta is derived from them here**, per the rig's own rule.

## What changed since the earlier Postgres baseline — one variable

The **session pool**, 200 → 10,000 entries. The session read lives inside the `database` layer bucket (83.26% of measured server time in the first under-load ledger), so the old pool's identity sharing shrank the session working set and the last-seen write set inside that bucket — in the direction that flatters `DbStore`, the arm the upcoming session-store comparison exists to evaluate.

The pool is drawn from the dataset's own rows, not a parallel namespace: `bench_user_1..bench_user_10,000` were given a real argon2 password (`seed-sessions`: **10,000 set in 178.8 s**, 18 ms/user), then logged in for real (`seed-sessions.sh 8114 10000`: **10,000 sessions written, zero failed logins**). **`auth_user` held 20,200 rows before and after** — 20,000 dataset users plus the 200 legacy `bench_1..bench_200` users from the pre-fix design (pre-existing state; the fix adds and removes no rows).

## Method and provenance

- **Envelope** (asserted by `run.sh verify`, exit 0): `--cpuset-cpus=0,2,4,6` (four distinct physical P-cores), 4 GB memory, **no swap** (`memory.swap.max` = 0), `nofile 524288:524288`, `--network host`. k6 on `taskset -c 8,9,10,11`.
- **Served database, proven by more than its URL**: the container's environment reads `postgres://…@127.0.0.1:55432/umbral_bench_pg`; live `ESTABLISHED` sockets from the app to 55432 were observed; and the pool's session rows are **rows in Postgres** (the `session` table grew 200 → 10,200), which is what makes the authenticated half of the workload execute at all — a pool built against another database authenticates nothing, and the preflight aborts (exit 107) rather than measure it. The warm-up ran first with `PREFLIGHT` on and passed, which is itself the end-to-end proof that the minted pool authenticates against Postgres.
- **The measured binary**: `umbral-bench-prod` sha256 `e1887011…6ac0009`, built from commit `77e45a9d` (the session-pool fix, verified the same day — the refusal fires at exit 107 and is not skippable by `PREFLIGHT=0`; `record.sh` writes the pool size; `compare.sh` refuses a missing or differing one). The result carries `git_dirty: true` from **two comment/doc edits made after the binary was built** (an argon2 cost comment in `seed.rs`, corrected from the measured 17–18 ms/user, and the SDD ledger) — code-identical to the built binary.
- **Load shape**: `hold.js` — 60 s ramp to 2,500 VUs, 5-minute hold, 30 s ramp-down (≈6.5 min wall clock); the reported percentiles span all three stages, as in every number this rig has produced. A 1,000-VU, 2-minute unscored warm-up ran first.
- **Thermal/load provenance**: the sampler ran on **core 12** (outside the silo's cpuset), started before the warm-up; `thermal.source: "sampler"`, window 618 s covering the run, peak **81 °C**, not stale. `host.loadavg_before_run` = **1.94** (1-min; 2.20/2.62 over 5/15 min). Thermal-throttle counters (cumulative since boot, cpuset cores): core **122,397**, package **505,315** — recorded to be diffed against the next result, not as an absolute.
- **Zero failed requests and 100% checks**: 343,106/343,106 checks held — feed 200 ×158,002 · post 200 ×142,268 · like 2xx ×23,937 · create 2xx ×3,165 · page 200 ×15,734. The write path was genuinely exercised (≈3.2k creates, ≈24k likes).

## The result

| Metric | Value |
|---|---|
| RPS | **862.585** |
| p50 / p95 / p99 / max | 1.585 / **3.197** / **4.171** / 9.457 ms |
| Errors | 0 / 343,110 |
| Checks | 343,106 / 343,106 held |
| Requests · iterations | 343,110 · 158,002 |
| VUs (max) | 2,500 |
| Dataset scale | 20,000 users / 200,000 posts |
| Session pool | **10,000** (1:1 with peak VUs), `DbStore` |
| Exit | `passed` (thresholds p95<500 ms, p99<1000 ms — cleared by ~100×) |

Context rows (not a licensed delta; see "Read this first"):

| | this run (pool 10,000, 1 hold) | earlier Postgres baseline (pool 200, 6 holds) |
|---|---|---|
| RPS | 862.585 | 861.7 – 865.3 (mean 863.73) |
| p95 | 3.197 ms | 3.0 – 3.5 ms |
| p99 | 4.171 ms | 4.07 – 6.74 ms |
| max | 9.457 ms | 10.4 – 32.8 ms |
| load before run | 1.94 | 0.67 – 4.12 |

## What this does and does not license

- **It licenses**: a fixed point for Postgres-vs-Postgres comparisons at 2,500 VUs with pool 10,000 (Phase 3 cycles), and the starting point for the VU ramp.
- **It does not license**: any SQLite delta; any delta against the pre-fix ≈863; any "capacity" reading of the RPS figure; any p99 claim tighter than the host's ~20–27% run-to-run noise.

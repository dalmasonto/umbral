# Bench rig v2 — Phase 2 closeout, and the first under-load layer ledger

**Date:** 2026-10-14 · **Branch:** `bench-rig-v2` · **Backend:** SQLite · **Envelope:** `--cpuset-cpus=0,2,4,6`, 4 GB, no swap, `--network host`

This file covers three things: the Cycle 0 null check on SQLite, the first layer ledger ever captured under load, and a rig defect that invalidates part of the session cost we are about to measure. The headline is the ledger — **83% of the server's time is in the database bucket**, and that is now measured rather than assumed.

---

## 1. Cycle 0 null check — the RPS null holds, the tail tells us something else

Cycle 0 changed `crates/` (commits `1a73bcbe`, `4cd5af96`: index-column validation in `read_migration_file`, and an index-group filter on `AlterColumn` / rebuild-`DropColumn`). That changed the measured binary, so the baseline had to be re-taken. A validation fix on the boot path should be a **null result**, and that null result is what proves the fix was behaviour-preserving.

| Run | RPS | p95 | p99 | load m1 | Exit |
|---|---|---|---|---|---|
| new 1 `…T102253Z` | 864.692 | 1.842 ms | 3.034 ms | 1.89 | 0 |
| new 2 `…T103232Z` | 863.722 | 3.126 ms | 4.200 ms | 1.02 | 0 |
| new 3 `…T104339Z` | 865.995 | 3.176 ms | 4.217 ms | 1.31 | 0 |
| Task 9 baseline | 865.381 / 865.086 / 864.536 | 3.284 / 3.318 / 3.389 | 4.749 / 4.747 / 4.815 | 2.25 / 2.68 / 2.06 | 0 |

**RPS: null confirmed.** Means 864.803 against 865.001 — **−0.023%**. Zero failed requests, checks 100%, no OOM, envelope verified. The new binary sha256 is `00a65b456681e30e5dac23dd176c10ef8fe6dd8c1512259ede5a70633891ad88`, differing from Task 9's `bcc4479a03f7…` as required — that difference is the point of the check.

### The p99 movement is NOT the fix, and must not be reported as one

All three new holds sit below all three baseline holds on p99 (max 4.217 < min 4.747), which reads as a ~20% tail improvement. **The Cycle 0 change cannot cause it.** It runs at migration-read time and in the migration planner; it touches no request hot path. Three pieces of evidence say the tail is host-state dominated instead:

1. **Throttle counters are byte-identical within each set but moved between them** — core 43441 → 117696, package 186551 → 485123. The two sets were not taken in the same thermal regime.
2. **Load is confounded, not causal.** Across sets, low load tracks low p99. *Within* the new set the ordering inverts: load 1.89 gave the best p99 (3.034) and load 1.02 the worst (4.200). A variable that reverses sign inside the set it is supposed to explain is not the driver.
3. **New hold 1 carries the recorder's own warning** — its sampler window opened at 77 °C, so the machine was already hot — and it posted the best p99 of the three. Heat does not explain it either.

**This is the second independent observation of ~20% set-to-set tail drift on this host.** The first was the Postgres six-run series. The consequence is a rig fact, not a result about umbral:

- **`compare.sh`'s p99 limit of 20% sits exactly on this drift.** Two of Cycle 0's own holds — identical binary, identical envelope — diff as `+38.43% REGRESSION`, exit 1. Holds 2 and 3 of the same set are concordant to 0.40%; hold 1 is uniformly fast at every percentile. The comparator's verdict lines are a change detector, not evidence of a change.
- Cross-set comparisons must be taken within one thermal regime, or repeated until concordant.

`compare.sh` was deliberately **not** changed in this task. That decision belongs to the operator.

---

## 2. The first under-load layer ledger — where the time actually goes

Diagnosis, not measurement: captured from `umbral-bench-instrumented` in the same silo under the same workload. Proportions are the product; its latency is not quotable, because the telemetry is in the path by construction.

**342,925 requests** at 2,500 VUs, all routes, zero errors.

### The bucket split, per request

| Layer | avg µs | Share of in-server |
|---|---|---|
| **database** | **905.8** | **83.26%** |
| middleware (all) | 68.0 | 6.25% |
| tail | 43.0 | 3.95% |
| serialize | 39.8 | 3.66% |
| render | 20.2 | 1.85% |
| routing | 7.0 | 0.64% |
| handler_logic | 4.2 | 0.38% |
| query_build | 0.0 | 0.00% |
| **in-server total** | **1087.8** | 100% |

k6 reports 1415 µs end-to-end, so roughly **327 µs is loopback and the load generator**, not the server.

### Middleware, ranked

| Middleware | avg µs | Present in prod? |
|---|---|---|
| **security (headers + csrf)** | **40.64** | yes |
| sessions | 16.14 | yes — but see §4 |
| bench probe (wrap) | 10.49 | **diagnostic only** |
| response stamp | 0.39 | — |
| dev auth (header) | 0.19 | **diagnostic only** |
| request log | 0.09 | — |
| api rate limit | 0.01 | yes |
| bench lab (injectable) | 0.01 | **diagnostic only** |

Real prod middleware is therefore about **57 µs**, of which `security` is 40.6 µs — 71%.

### Per route

| Route | Requests | avg | Errors |
|---|---|---|---|
| `GET /api/feed` | 158,088 | 1427 µs | 0 |
| `GET /posts/{id}` | 141,931 | 770 µs | 0 |
| `POST /api/posts/{id}/like` | 23,699 | 849 µs | 0 |
| `GET /u/{id}` | 16,157 | 1020 µs | 0 |
| `POST /api/posts` | 3,050 | 534 µs | 0 |

### What this settles

**Middleware is not where the work is, and cannot close the gap to 25,000 RPS.** Deleting the entire middleware chain — security headers, CSRF, sessions, rate limiting — buys about 57 µs of 1088, roughly **5%**. The target needs roughly a 6× reduction. Every optimization cycle must therefore target the database bucket.

**The v1 rig's hypothesis is confirmed**: the database bucket dominates by orders of magnitude while middleware is nearly free. It was right about the shape and wrong about the numbers — that run was untrustworthy for other reasons — and it is now confirmed on a rig that holds up.

---

## 3. The database bucket is opaque, and that is the next thing to fix

Two limitations bound what this ledger can say:

- **`query_build` is 0 µs and the `sql` block came back empty.** sqlx gives no per-statement attribution on SQLite, so the ledger cannot split the 905 µs into the feed query, the session lookup, and the follow-ups. This split is the prerequisite for naming a Cycle 1 target.
- **"Database" here is ORM wall time, not time inside SQLite.** Every record carries `db_source: orm_wall`, so the 905 µs is the ORM's own wall clock around the call — which includes query building, row mapping and hydration, not only SQLite's execution. That is a useful distinction rather than a defect: it means a meaningful part of the 905 µs is **framework code that can be optimized**, as opposed to a C library we cannot touch.

Per-route p50/p95/p99 in the captures are ring-based and run-end only (feed's avg 1427 µs sits below its p50 1725 µs), so they are not run percentiles and should not be quoted as such.

---

## 4. A rig defect found while reading the ledger: the session pool is 12× too small, and it biases the measurement we are about to take

**`harness/bench/k6/lib.js:107`** selects a session with:

```js
const session = sessions[(__VU - 1) % sessions.length];
```

Deterministic modulo. With a **200-entry pool** and **2,500 VUs**, each session identity is shared by **12–13 concurrent clients**.

The rig's own documentation states the opposite intent. `harness/bench/workload.md:33`:

> *"VUs are pinned to one pooled session each, so a VU's traffic is one coherent user rather than a new identity per call."*

That is correct modelling — one VU should be one client. `BENCH_SESSION_POOL_SIZE` defaults to 200 (`bench-prod.rs:39`) and was never revisited when the VU count went to 2,500. `harness/bench/README.md:113` bakes the 200 into the documented recipe. **No rationale for 200 is recorded anywhere; it is a default, not a decision.**

### Why it matters

Real users do not share sessions. The effect on read latency is modest — an indexed session lookup is log₂(200)≈7.6 against log₂(2,500)≈11.3 page descents, both warm — but the working set is not: 200 resident session rows instead of ~2,500, on a page cache shared with the whole feed workload, plus session last-seen writes touching 200 rows instead of 2,500.

It lands in the worst possible place. **The session read lives inside the 905 µs database bucket**, so this contaminates 83% of the measured time, and it does so in the direction that **flatters `DbStore`** — which understates what moving sessions to a faster store would buy. The session-store comparison would be rigged toward the status quo without anyone intending it.

There is a second, related hole: **the pool size is not recorded in the result file at all.** The `session` block carries `store`, `cookie`, `ttl_seconds`, `absolute_max_age_seconds` — no pool size. And `compare.sh` refuses across `mix`, `dataset_scale` and `cpuset`, but not across a pool-size change. So a rebuilt pool would diff happily against a 200-pool baseline. This is the same class of silent-incomparability hole the rig already closed for the mix.

### A caveat on the ledger itself, stated loudly

The instrumented binary mounts `CookieStore` while `prod` mounts `DbStore` (ruling 15), and in this tree the instrumented binary's cookie store **cannot authenticate the pooled cookies at all** — the pooled cookie is a raw rotated UUID while the store is AEAD-backed. So the ledger's `sessions` line (16.14 µs) is not prod's session read in any form: **prod's real `DbStore` read does not appear in this ledger at all.** It is buried in the database bucket, and the 905 µs is therefore a **floor** for prod, not an estimate of it.

---

## 5. What happens next

1. **Fix the rig.** Raise the session pool to 1:1 with peak VUs so one VU is one client, record the pool size in every result, and add it to `compare.sh`'s refusal list alongside `mix`.
2. **Re-baseline on Postgres** with the corrected rig, as a **single** hold — not three.
3. **Raise the VU count** to find where RPS actually stops rising, again as a **single** run — not three. This answers "can it do 25,000 RPS" as a capacity question, which the current 2,500-VU operating point cannot: there, throughput is pinned by the workload's 3–8 s think time, and a twofold server speedup would move RPS by under 0.1%.
4. **Then** split the database bucket by query, and begin optimization cycles against it.

The baselines remain the fixed points: SQLite numbers compare to SQLite, Postgres to Postgres, never across.

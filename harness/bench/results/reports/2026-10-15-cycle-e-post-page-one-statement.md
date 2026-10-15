# Bench rig v2 — Cycle E: the post page in one statement, ~+12% capacity

**Date:** 2026-10-15 · **Branch:** `bench-rig-v2` · **Design:** ABAB at 15,000 VUs, one hold per arm, arm **means** compared; both arms carry mimalloc, so the cycle contains one variable. **Verdict: a WIN — +13.8% capacity (means: control 3,498.9 vs one-statement 3,981.0), tails down ~38%.**

## What "statement-level work" means here

Yesterday's diagnosis (sent ahead of any change): the DB is not slow — it is *busy*. Single-statement round trips measure 16–75 µs; the workload pays **~4.7 statements per request ≈ 20,000 statements/second**, and the cost is the aggregate fixed overhead (PG's per-statement machinery ≈ 33% of the four cores, both kernels ≈ 28% — loopback TCP + syscalls), not row work. So the lever is fewer round trips per request.

## The change — two parts, one hypothesis

1. **Framework (`fix(orm)`): `fetch_annotated` now composes with `select_related`.** The annotated-fetch terminal decoded rows and read annotations but never ran the prefetch — combining the two silently returned UNHYDRATED rows (related slots empty, raw FK ids). Same silent-composition class as the 2026-10-14 hydration bug. Regression test added with a red→green control (the test fails against the pre-fix file with the exact "select_related must hydrate alongside annotations" panic).
2. **App: `GET /posts/{id}` is now ONE annotated statement** — the post, its author (prefetched), and both engagement counts as correlated scalar subqueries on the same row — where it used to run three (post, then two separate `COUNT(*)`s). The counts do the same index work; what disappears is the round trips.

**Why this route, and the arithmetic:** `/posts/{id}` is the second read on **90% of the workload's iterations** (read from `lib.js`; the profile page is the 10% branch). Per iteration the workload therefore drops from ~10 statements (feed's ~5 + post page's ~5) to ~8 — **−20% of all statements** — which predicts ≈ +11% capacity if the DB side is roughly half the four-core budget. It also sets up the follow-ups (the session read and the write path's exists+insert are the other statement-dense spots).

## Correctness gate, before any measurement

Byte-identical `/posts/{id}` pages from the two binaries (`01a8a20f` vs `4634d9a9`), including a post with **35 real likes** — so the annotated counts are proven to return live values, not matching zeros. Workspace: **646 suites green** (which also surfaced and fixed two unrelated test-isolation flakes, `1caf46d4`).

## The measurement

| arm | recorded (UTC) | RPS | p50 ms | p95 ms | cpu band (MHz) | peak °C | load |
|---|---|---|---|---|---|---|---|
| A1 current | 22:33:03 | 3,471.7 | 1,067.7 | 2,751.0 | 400–3,700 | 100 | 1.8 |
| B1 one-statement | 22:46:29 | **3,926.0** | 805.4 | 1,712.2 | 400–2,274 | 99 | 2.12 |
| A2 current | 22:59:55 | 3,526.0 | 1,289.7 | 1,976.0 | 400–2,789 | 99 | 2.11 |
| B2 one-statement | 23:13:20 | **4,036.0** | 718.8 | 1,401.0 | 400–1,946 | 100 | 3.09 |

**Arm means: control 3,498.9 vs one-statement 3,981.0 → Δ = +13.8%.** Pair spreads: control 1.6%, treatment 2.8% — both ~5–9× below the effect; `compare.sh` across A1→B1 independently reads **+13.09% IMPROVEMENT, p95 −37.8%, p99 −36.0%**. The treatment ran at the sweep's LOWEST max clock band (1,946 MHz on B2, 2,274 on B1, against control's 2,789–3,700) — the thermal confound runs against the finding for the third cycle in a row. Control pair spread 1.6% (3,471.7 → 3,526.0); the host ran under a heavy desktop session (chrome 60–70% throughout) and the dataset has grown to **3.13M likes / 606k posts** across the day's runs — the level is lower than yesterday's 4,3xx runs, and the ABAB means are exactly what carry the comparison across that.

## Mechanism notes

- The tails moved with the rate (p95 2,751 → 1,712 ms on B1): fewer round trips per request is *directly* less queueing at the same VU count.
- The removed statements were counted per route from the code, not estimated: post page 5 → 3 statements (session + annotated post + author prefetch). No new statements were added; the correlated subqueries execute inside the same statement as the post read.

## What this opens (honest sizes, from the same measurement base)

- **Profile page** — `post_count` folds into the user read the same way (−1 statement on 10% of iterations, ≈ +0.3–0.5%).
- **Write path** — `like`/`follow` run `exists()` + `insert`; an atomic upsert terminal (framework feature; `get_or_create` is deliberately SELECT-then-INSERT and is the same shape as today) would halve the write path's statements (≈ +1%).
- **Session cache** — the last big single-statement cut (1 of every request's statements, ≈ +2–3%), pending the operator's call on revocation semantics.

## Caveats

- One hold per arm; the control pair's 1.6% is the effective precision, and the effect is ~7× that.
- Host under a heavy desktop session throughout (verified during the sweep: chrome 60–70%, a usage tool at ~60%, system monitor); both arms share it, and the ABAB means cancel the drift.
- Release builds are not bit-reproducible (rig fact); the mechanism (statement count per route) is code-counted, not inferred from the delta.

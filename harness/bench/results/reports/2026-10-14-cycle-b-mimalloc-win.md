# Bench rig v2 — Cycle B: the allocator was worth ~20% of the server

**Date:** 2026-10-14 · **Branch:** `bench-rig-v2` · **Design:** ABAB at 15,000 VUs (where RPS *is* capacity), one hold per arm, arm **means** compared so the day's drift cancels; both arms built from the same tree, so the cycle contains one variable. **Verdict: a WIN — +20.5% capacity, landing as application-level configuration.**

## The change

Two lines, no framework code: `mimalloc` as the global allocator — a dependency plus one `#[global_allocator]` in the harness library (so both bench binaries share it). The rationale came straight from the CPU profile (`2026-10-14-cpu-profile-of-the-silo.md`): glibc's allocator internals held **≥17% of all four silo cores** (lower bound) — about a quarter of the app's entire CPU budget — mediating small-object churn, while the app's own CPU was diffuse (under 0.1% per symbol; no single function had ever mattered). The allocator was the one *concentrated* pool a change could aim at.

Both arms carry the hydration fix (`11740d35`); this cycle does not re-measure that change. The complete arm diff is 39 inserted lines across `harness/backend/{Cargo.toml,Cargo.lock,src/lib.rs}` — nothing else.

| arm | binary sha256 (short) | allocator |
|---|---|---|
| A-control ×2 | `202c9410503034c3` | glibc |
| B-mimalloc ×2 | `01a8a20f1372deff` | mimalloc |

## The measurement

| arm | recorded (UTC) | RPS | p50 ms | p95 ms | cpu band (MHz) | peak °C | load |
|---|---|---|---|---|---|---|---|
| A1-control | 18:57:55 | 3,504.4 | 1,272.5 | 2,058.4 | 399–2,001 | 99 | 1.54 |
| B1-mimalloc | 19:11:20 | **4,261.5** | 551.8 | 1,116.4 | 400–1,803 | 97 | 0.64 |
| A2-control | 19:24:46 | 3,605.7 | 1,246.2 | 1,760.3 | 400–2,047 | 101 | 0.70 |
| B2-mimalloc | 19:38:12 | **4,302.8** | 541.6 | 920.7 | 400–2,015 | 97 | 0.82 |

**Arm means: control 3,555.0 vs mimalloc 4,282.2 → Δ = +20.5%**, zero errors throughout, checks fully held (1.39M / 1.69M assertions).

- **The intra-pair spreads (control +2.9%, mimalloc +1.0%) are ~7× smaller than the effect**, and the control's drift runs *in the direction of its own clock* (2,001 → 2,047 MHz) — exactly what the ABAB design folds out.
- **The treatment's slower arm ran at the sweep's LOWEST clock band (1,803 MHz) and still beat every control hold by ≥18%.** The thermal confound runs against the finding, not for it.
- `compare.sh` across A1→B1 (independent of the means): rps **+21.61% IMPROVEMENT**, p95 **−45.8%**, p99 **−35.4%**, all facts aligned (same mix, scale, pool, cpuset, backend).
- **Latency says the same thing the rate does:** p50 ≈1,250–1,270 ms under control vs ≈542–552 ms under mimalloc, at the same 15,000 VUs. At saturation, a halved p50 *is* the queueing collapse of a faster server — not a separate phenomenon.

## Why, and the confirmation

At saturation the server is CPU-bound, and ~a quarter of the app's CPU was allocator brokerage. Removing most of it predicts roughly +15–25% capacity; the measured +20.5% sits exactly in that band, and the four-core core-seconds-per-request falls from ~1.12 µs-scale budget to ~0.93 (4 cores ÷ 4,282 RPS). Beyond raw CPU: mimalloc's per-thread free lists remove the lock contention glibc's shared arenas impose when four workers allocate continuously — contention costs *queueing*, which is why the p50 collapses further than the allocator's own CPU share alone would suggest.

**Direct mechanism check (same evening, diagnostic — not an arm):** the identical 15,000-VU steady state was re-profiled on the mimalloc binary with the same 180-second `perf` window. Since both runs saturate the same four cores, the profiles' totals are both ≈4 core-seconds per second, so their shares are directly comparable:

| bucket | baseline (glibc) | mimalloc build |
|---|---|---|
| app code (`umbral-bench-prod`, **includes mimalloc itself**) | 30.0% | 44.0% |
| app libc | **25.3%** | **2.6%** |
| app kernel | 11.7% | 14.9% |
| postgres (code + kernel + libc) | 26.7% | 32.9% |
| idle / desktop | ~4% | ~5% |

Read it with the knowledge that mimalloc is statically linked — its own cost moved *inside* the app binary's row, where its symbols (`mi_free`, `_mi_page_malloc_zero`, `mi_theap_*`, …) total **9.65%** of the budget. So:

- **The glibc allocator's dedicated CPU path is gone** (app libc 25.3% → 2.6%): its internals, its tcache bookkeeping, and the memcpy/string traffic that lived inside chunk handling.
- **Allocator-side cost roughly halved: ≥17.3% of all four cores → 9.65%.**
- **The freed CPU was reinvested in throughput, not idled**: Postgres's share *rose* (26.7% → 32.9%) because it serves ~20% more requests per second, and the app's own code outside mimalloc rose 30.0% → 34.4% for the same reason. This is the whole story of the +20.5% in one table: nothing got cheaper except the broker, and the broker's fee went back to the work.

## Scope — what this is and is not

- **This is application-level configuration, not a framework fix.** Any umbral app gets it by adding the dependency and the `#[global_allocator]` line. The harness now carries it, so it is part of the rig's configuration from this cycle onward.
- **The framework-side prize is now sized:** the profile plus this result say that allocation *count* is where umbral's remaining CPU work is. The top candidate is the prefetch hydration's double materialization — every prefetched related row is built as a `serde_json::Value` tree and then re-parsed into the typed model (see the optimization queue in the SDD ledger).
- No ORM/middleware/session behaviour changed between arms.

## Caveats

- One hold per arm; the pairs' internal agreement is the effective precision, and the effect is 7× the largest observed drift.
- Release builds are not bit-reproducible across rebuilds (rig fact, 2026-10-14); the arms differ by the allocator change *and* ordinary codegen nondeterminism. A layout accident cannot remove malloc's CPU — which is why the mechanism check above, not the delta alone, is the proof.
- The host was throttled throughout (1.8–2.0 GHz bands, 97–101 °C): the ceiling measured here is a floor.
- All four arms record `threshold_failed` (exit 99) by construction — the hold's latency thresholds are 2,500-VU-tuned and cannot hold at saturation; the class is uniform across arms, so the means comparison stands.
- The desktop (chrome et al.) is visible on the silo cores in every profile of the day (~0.8% of samples); constant across arms.

## Rig follow-ups

1. ~~Record the allocator as a first-class fact~~ — **landed `db66d012`, same evening**: `run.sh serve` passes `BENCH_ALLOCATOR` (warns when unset; no guessed default), `record.sh` files the declaration under `envelope.allocator` with its provenance, `compare.sh` refuses a declared-vs-declared mismatch (exit 3) and prints a named context line when one side predates the field. Tested behaviourally (null path on the live container; three comparator cases with fixtures).
2. The mechanism comparison above is the confirmation artifact; its raw `perf` reports live in the day's job scratch, and the method (including the symbolization traps) is the committed skill `.claude/skills/perf-cpu-profiling-the-silo.md`.

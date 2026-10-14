# Bench rig v2 — Cycle D: the allocation-count probe measures null (and that is the result)

**Date:** 2026-10-14 · **Branch:** `bench-rig-v2` · **Design:** ABAB at 15,000 VUs, one hold per arm, arm **means** compared; both arms carry mimalloc, so the cycle contains one variable. **Verdict: −1.4% means (control 4,343.5 vs change 4,281.6) — no win, a hint of a small loss. Not landed; the change is reverted. The instrument and the finding are kept.**

## Why this change existed

Cycle B removed the allocator's own overhead (+20.5% capacity). The next question was the *count* of allocations the framework asks for. To answer it, this cycle built the instrument the profiles could not provide — a **dhat heap profile of the ORM read paths** (feature-gated `dhat-alloc` that swaps the global allocator; an ignored test; read-only against `bench-run.db`):

- **1,939,496 allocation blocks per 400 iterations** across feed/engage/pages (~970 per route call).
- The single largest avoidable block: **a deep clone of every prefetched related row** — `Some((pk_json_key(pk_val), obj.clone()))`, five sites (single-hop, nested ×2, dynamic ×2), where the key borrows `obj` while the tuple construction needs it moved. **14.8% of all blocks.**

The fix (one `bucket_by_pk` helper — owned key first, then move; `mem::take` for the nested levels) demonstrably worked on its own terms: **1,939,496 → 1,626,216 blocks (−16.2%)**, bytes −8.8%, all 646 workspace suites green.

## What the rig said

| arm | RPS | p95 ms |
|---|---|---|
| A1 current (mimalloc) | 4,355.1 | 871 |
| B1 + clone fix | 4,297.5 | 966 |
| A2 current | 4,331.9 | 860 |
| B2 + clone fix | 4,265.7 | 919 |

**Means: 4,343.5 → 4,281.6 = −1.4%.** Both pairs agreed tightly (control 0.54%, treatment 0.74%) on the quietest, fastest host window of the day — so the −1.4% is larger than either pair's internal spread, and there is *no* evidence of a win to weigh against it. The change was reverted; the tree returns to the Cycle-B configuration that measured 4,343.

## What this establishes

1. **After the allocator is fixed, allocation-*count* reduction is low-yield on this workload.** Cutting 16% of allocation blocks bought nothing measurable: mimalloc's per-call cost is small, the removed work was small-string memcpy, and these sites are not the saturation bottleneck. This closes the "chase allocations" line with data rather than opinion — and it also downgrades the typed-hydration project, whose remaining prize was exactly this class (the JSON value-tree's name/RFC3339/key allocations), to something that must justify itself by more than block counts.
2. **Where the budget actually is (mimalloc profile):** Postgres ≈33% (its code + kernel + libc), the two kernels ≈28% combined (loopback + syscalls), and the app's own code diffuse below 0.1% per symbol. The remaining levers are **statement-level** (fewer statements per request; the session read is one of ~4.7) and **message-level** (fewer/smaller bytes across the wire) — not allocation-level.
3. **A null with a working mechanism is still a result.** The dhat instrument exists now (feature + ignored test, committed), the deep-clone waste is documented where it lived, and the next candidate's expected value is known to be small *before* anyone spends a night on it.

## Caveats

- One hold per arm, as every cycle here; the pair spreads bound the precision (and the −1.4% sits inside the day's cross-cycle drift class, though not inside either pair's).
- The arms differ by the source change *plus* ordinary build nondeterminism (release builds are not bit-reproducible — rig fact of 2026-10-14).
- dhat's own bookkeeping inflates absolute allocation times; the *counts* and their deltas are the artifact, transformed into a capacity claim only by the ABAB.

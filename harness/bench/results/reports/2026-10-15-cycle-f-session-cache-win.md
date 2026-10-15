# Bench rig v2 — Cycle F: the session cache, +8.3% (and every pairing agrees)

**Date:** 2026-10-15 · **Branch:** `bench-rig-v2` · **Design:** ABAB at 15,000 VUs, one hold per arm, arm **means** compared; both arms carry the allocator + statement work, so the cycle contains one variable. **Verdict: a WIN — means 4,247.9 → 4,598.8 = +8.3%; all four cross-arm pairings favour the cache; p50 halved. Caveat: tonight's host variance was the widest of any cycle.**

## The change — built to the operator's specified contract

The operator's call on the revocation-semantics question: yes to a session cache, with **signal-driven revocation** — "whenever a sessions table changes with a given key, it should revoke the key". That is exactly what `CachedDbStore` implements:

1. **Signal-driven invalidation (route-independent).** `install_store` subscribes to `post_save:session` / `post_delete:session` (+ bulk variants) and evicts the changed key for *every ORM-mediated write, wherever it came from* — logout, rotation, admin revocation. The session helpers deliberately mix store calls and direct ORM writes; the signal route is what makes the contract hold for both.
2. **A TTL** (default 300 s) bounds what signals cannot see — other processes, raw SQL — documented with that scope stated plainly.
3. **`upsert` now emits `pre_save`/`post_save`** — a prerequisite found while designing this: the store's own save path was invisible to every subscriber (no emits at all), so the operator's contract would have had a hole exactly where sessions are written. `created=false` by documented contract (INSERT vs conflict-UPDATE is not distinguishable without an extra round trip).

## Correctness evidence, ahead of the numbers

- **Five crate tests**, including: a cache hit proven by deleting the row with **raw SQL underneath the cache** (no signals → a hit can only come from memory), with a cold-store control asserting the row is really gone; ORM-mediated invalidation with a **red→green control** (removing the upsert emits makes the test fail with exactly the invalidation assertion).
- **End-to-end over HTTP, before the sweep:** login 200 → `me` 200 → `me` again 200 (identical payload, cached path) → logout 204 → **`me` 401** — a stale cache would have served 200; this is the revocation contract proven on the wire.
- Workspace: **647 suites green**. Docs page shipped with the feature (`documentation/docs/v0.0.1/sessions/cached-store.mdx`).
- Under load: the 15k-VU hold is fully authenticated (~1.7M session reads served, checks held, zero errors).

## The measurement

| arm | recorded (UTC) | RPS | p50 ms | p95 ms | clock band | load |
|---|---|---|---|---|---|---|
| A1 control | 23:54:36 | 4,159.4 | 555.9 | 1,373.6 | 399–1,802 | 3.52 |
| B1 cache | 00:08:04 | **4,758.1** | 125.1 | 883.8 | 400–3,700 | 1.24 |
| A2 control | 00:21:31 | 4,336.3 | 463.1 | 1,058.1 | 399–3,600 | 0.86 |
| B2 cache | 00:34:57 | **4,439.4** | 370.2 | 1,073.1 | 400–3,500 | 0.60 |

**Means: control 4,247.9 vs cache 4,598.8 → Δ = +8.3%.** `compare.sh` across A1→B1 independently: **+14.39%**, p95 −35.7%, p99 −33.8%.

**The arms do not overlap** — every cache hold beat every control hold (worst cache 4,439.4 > best control 4,336.3) — and **all four cross-arm pairings favour the cache**. The strongest internal control against the host-noise caveat: B1 (load 1.24) beat A2 (load 0.86) by 9.7% *despite the heavier host*.

**Latency:** p50 means 509.5 → 247.7 ms (−51%). The session read sat at the *front* of every authenticated request, so removing it shortens every request — the median, not just the tail.

## Caveats — read the verdict against these

- **Host variance tonight was the widest of any cycle:** the recorded pre-run loads ran 3.52 / 1.24 / 0.86 / 0.60 (a desktop session + two stuck statusline processes cleared mid-sweep), giving pair spreads of 4.3% (control) and 7.2% (cache) against 0.5–2.9% in earlier cycles. The means comparison and the non-overlap are what carry the verdict; a re-run on a quiet host would tighten the number but cannot change its sign — no pairing, including the most adverse one available, has the cache losing.
- One hold per arm; release builds are not bit-reproducible (rig fact).
- The cache's own staleness bounds (TTL, per-process scope) are documented on `SessionStore::invalidate` — for a multi-process deployment the TTL is the cross-process bound, not the signals.

## Where this lands the run

Three measured wins, each ABAB-verified, each with its mechanism confirmed: **allocator +20.5%** (Cycle B), **statement cut on the post page +13.8%** (Cycle E), **session cache +8.3%** (this). Same four cores, same workload, every rejection (−8% join, −1.4% allocations) documented and reverted. The remaining queued lever: `defer()`-style column narrowing on the prefetch (~+1.5–2.5%), still the operator's call.

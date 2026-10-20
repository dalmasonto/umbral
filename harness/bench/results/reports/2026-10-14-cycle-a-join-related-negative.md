# Bench rig v2 — Cycle A: the round-trip-frugal shape is capacity-negative

**Date:** 2026-10-14 · **Branch:** `bench-rig-v2` · **Design:** ABAB at 15,000 VUs (where RPS *is* capacity), one hold per arm, arm **means** compared so the day's linear drift cancels · **Verdict: −8% capacity. Not landed; the change is reverted.**

## The change

The app's four related-loads (`recent_posts`, the feed page, the profile page, the post page) switched from `select_related("author")` — the ORM's batched **1+N prefetch**, one extra statement plus a 25-row `SELECT *` per request, measured at **32% of server-side DB time** — to `join_related("author")`, which the ORM's own docs describe as "one round-trip total … better when round-trip count dominates". The attribution said round trips dominate; the arithmetic predicted ≈+7% capacity.

## The measurement

| arm | binary | RPS | `cpu_current_mhz` at record | p50 |
|---|---|---|---|---|
| A1 old (`select_related`) | `c17f7bdf`-family | **3,412.5**¹ | — | — |
| B1 join | `245ac28e` | **3,106.6** | 2,094 | 1,865 ms |
| A2 old | `c17f7bdf` | **3,328.4** | 1,901 | 1,619 ms |
| B2 join | `245ac28e` | **3,092.2** | 3,247 | 1,901 ms |

¹ A1's recorder run hit a transient `record.sh` bug (an empty container/cgroup read fed to `--argjson`); its **summary export survives** and its number is read from that. The A1→A2 gap (28 min) is the day's thermal drift line (−2.5%); B1 and B2 each sit **6–8% below** that line.

**Arm means: old 3,370.5 vs join 3,099.4 → Δ = −8.0%.** The B arms reproduce each other to **0.5%** — even though B2 ran at **3,247 MHz** against B1's 2,094. The clock does not explain the loss; the difference is the binary's.

## Why — and the model this sharpens

**At saturation the only scarce resource is CPU, and a round trip you `await` is free.** The prefetch's extra statement is almost entirely an *await* (the pool hands the request a connection, the query blocks, the data arrives) — it costs latency at demand-bound levels but almost no CPU. The JOIN moves the same 25 author rows *into* the main query: a nested loop the planner runs per row, wider rows to materialise and decode, more columns through the protocol — **CPU work**, on the same four cores that are the ceiling. Removing an await bought nothing and paid in CPU.

This retro-explains the two knob nulls from earlier today: `test_before_acquire=false` removes an await (**null**), and pool 10→50 changes queueing dynamics, not CPU (**null-to-negative**).

**Consequences for the optimization queue:**
- **Capacity-raising changes must cut CPU per request** — row→model mapping/decoding, per-statement protocol CPU, columns fetched — not round trips.
- **Round-trip cuts remain latency changes**, to be measured at a demand-bound level (7,500 VUs, p95 ≈2 ms), not at saturation. Cycle A's latency effect there is **untested** and is the honest open question about this shape.
- A narrower variant — joining only the *single-row* sites (the post page's 1-post+1-author) where the prefetch fetches one row for one statement — is untested and may have a different sign.

## Side finding: the change fixed a latent hydration bug

Payload verification against a **frozen copy** of the database (PG `umbral_verify`, captured before the change) showed that on the old path the author was **never resolved** on the feed and post pages: the payloads served fallback names (`user_4538`) where the naive path served the real `bench_user_4538`. The join shape resolved them. **That is an ORM hydration gap on the `select_related` path, not a reason to switch shapes** — filed for a framework fix, not landed as an app workaround here.

## Caveats

- One hold per arm; the arm pairs' internal agreement (0.5% for B, 2.5% for A) is the effective precision.
- A1's number is summary-only (recorded facts lost to the recorder bug, which is filed for hardening).
- The host was throttled (2.0–3.2 GHz wandering) throughout; the ABAB mean comparison is designed to survive that, and the B arms' agreement across a 1.2 GHz clock spread is direct evidence it did.

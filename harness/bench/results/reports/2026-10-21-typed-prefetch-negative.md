# Typed prefetch — negative result (reverted)

**Date:** 2026-10-21 · **Branch:** `revert/typed-prefetch` (reverts `5f9cc00c`) · **Experiment parked:** local `perf/typed-prefetch-check`

## What was tried

`select_related`'s single-hop forward/nullable FK prefetch fetched `SELECT *`, decoded each related row to a `serde_json::Value` through the column metadata, then deserialized that JSON into the model — two decodes per related row. The typed path (`HydrateRelated::prefetch_typed` + `__fetch_typed_by_pk`) ran the same batched `IN (...)` query with `query_as::<T>`, decoding through the target's own `sqlx::FromRow`; a related row shared by many parents was decoded once and cloned per parent. M2M, reverse-FK, one-to-one, nested `__` paths and hand-written models kept the JSON fallback.

## Measurements

Diagnostic A/B on the v1 instrumented harness (`feed`, fresh SQLite database per arm, 20 VUs, 20 s). The `database` bucket is the handler's ORM wall (SQLite attribution, not per-statement).

| run | order | JSON (µs) | typed (µs) | delta |
|---|---|---|---|---|
| two binaries (831b5558 vs 07fa32ac) | OLD, NEW | 2 294 / 2 339 | 2 557 / 2 958 | +19% |
| same binary (`UMBRAL_TYPED_PREFETCH=0` vs unset) | JSON, TYPED | 2 372 / 2 602 | 2 977 / 2 920 | +18.5% |
| same binary, order alternated | TYPED first | — | — | whole arm degraded (7 299; controls also up) |
| same binary, order alternated | JSON first | 2 412 | 2 879 | +19.4% |

The `security` and `sessions` rows were flat in every comparable pair, so the delta is not host drift. The typed arm was slower in every non-degraded comparison, in both orders.

## Verdict

Reverted. The expected decode saving did not materialize, and the typed path adds macro surface plus a `sqlx::FromRow` + `Clone` requirement on FK targets. A change whose only purpose is speed does not ship without a win.

## What is parked

The local `perf/typed-prefetch-check` branch holds the typed implementation plus two check-only variants: an `UMBRAL_TYPED_PREFETCH=0` runtime switch and a cached-registry-metadata version. A future investigation should isolate the per-row decode cost (for example a same-process microbenchmark on `AuthUser`) before retrying.

## Caveats

v1 harness: in-process load generator, `DevAuth`, 20-second runs — diagnostic only, no capacity claim. The `database` bucket on SQLite is the ORM wall, including task scheduling.

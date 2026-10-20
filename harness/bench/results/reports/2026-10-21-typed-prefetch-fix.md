# Typed prefetch — root cause and re-land

**Date:** 2026-10-21 · **Branch:** `perf/typed-prefetch-v2` · **Supersedes the revert:** `b0ae1115`

## The regression

The first typed prefetch (`5f9cc00c`) removed the row→JSON→serde double decode by fetching related rows with `sqlx::query_as_with::<_, T, _>` (the `QueryAs` path). It passed the full suite but measured **~19% slower** in the ORM-wall bucket (JSON 2,371–2,602 µs vs typed 2,879–2,977 µs in the same-binary, order-alternated A/B), so it was reverted in `b0ae1115`.

## Root cause

A same-process benchmark on the real `AuthUser` rows (`harness/backend/tests/typed_prefetch_bench.rs`, release build, 25-row batches, alternating blocks) separated the steps:

| step | cost |
|---|---|
| in-memory `AuthUser::from_row` decode | **8.6 µs** / 25 rows |
| in-memory metadata row→JSON decode | **28.3 µs** / 25 rows |
| sharing one decoded row: `Clone` | **3.4 µs** / 25 parents |
| sharing one JSON row: borrowed serde | **9.2 µs** / 25 parents |
| `query_with` + manual `from_row` | **99–107 µs** / 25 rows |
| `query_with` + metadata JSON | **129–131 µs** / 25 rows |
| `query_as_with` (typed helper, before the fix) | **160–199 µs** / 25 rows |

The decode and the sharing step both favoured the typed path. The regression was **`sqlx::query_as_with` itself**: its `QueryAs` fetch path cost ~50 µs per 25-row batch more than the same query with `query_with` + a manual `T::from_row` loop. The typed helper was paying that plumbing cost on every prefetch.

## The fix

`__fetch_typed_by_pk` now runs `query_with` and decodes each row with `<T as FromRow<_>>::from_row(row)` manually (both backends). It also reads its PK column and soft-delete flag from the cached registry metadata instead of building `ModelMeta::for_` per call. An internal `UMBRAL_TYPED_PREFETCH=0` switch forces the JSON path for diagnostics.

After the fix, in the same benchmark:

| helper | cost |
|---|---|
| typed (`query_with` + manual `from_row`) | **103–109 µs** / 25 rows |
| JSON (`fetch_related_as_json_by_pk`) | **126–131 µs** / 25 rows |

The typed helper is now ~19% **faster** than the JSON helper for the same rows.

## Verification

- SQLite: `select_related` suite 11/11.
- Postgres: `pk_uuid_postgres` and `select_related_datetime_postgres` both pass (the manual `from_row` arm on `PgRow`).
- Full workspace gate on the re-landed tree: see the commit / CI run.
- End-to-end A/B caveat: the fixed-binary harness A/B ran while the host was loaded (an unrelated process at ~58% CPU, package at 85 °C) and produced whole-arm degradation, so it is **not** quoted. The same-process benchmark above is the evidence; a quiet-host/silo ABAB is the next measurement.

## Result

The typed prefetch is re-landed on `perf/typed-prefetch-v2` with the fixed fetch path, the cached metadata lookup, and the escape switch.

# Raw SQL Escape Hatch Lacks Bind And Write Routing

Category: Security, Correctness
Severity: Medium

## Finding

`QuerySet::raw` is a public escape hatch that sends the SQL string verbatim. It has no bound-parameter companion and resolves the ambient read pool by default.

The rustdoc warns callers to sanitize input and to pin the write pool for mutating raw SQL, which is useful. The safer API surface is still missing, so application code that needs a raw CTE or vendor-specific expression is nudged toward string interpolation.

## Evidence

- `crates/umbral-core/src/orm/queryset/mod.rs:4792-4804` documents `raw` as a hand-written SQL escape hatch and says the string is sent verbatim.
- `crates/umbral-core/src/orm/queryset/mod.rs:4809-4814` routes `raw` to `RouteOp::Read` by default even though arbitrary SQL can mutate data.
- `crates/umbral-core/src/orm/queryset/mod.rs:4816-4825` calls `sqlx::query_as::<_, T>(sql)` with no bound argument support.
- `crates/umbral-core/tests/bulk_update_raw.rs:146-160` covers constant raw SQL examples, not user-input binding.

## Risk

The main ORM path is strongly parameterized through SeaQuery and sqlx values. `raw` bypasses that protection and makes the unsafe pattern the shortest path for custom SQL. It can also run writes on a read replica or read-only route unless callers remember to select a write pool explicitly.

## Recommendation

Add a safer raw API before this grows in user code:

- `raw_with` or `raw_query` that accepts sqlx arguments or a small framework-owned bind builder.
- `raw_read` and `raw_write`, or an explicit `RouteOp` parameter, so routing intent is visible at the call site.
- Optional guardrails that reject obvious non-SELECT statements from the read-routed variant.

Keep the current `raw` only as a documented low-level escape hatch.

## Suggested Tests

- A parameterized raw query binds user input instead of interpolating it.
- A write-shaped raw statement through the read-routed helper is rejected or documented as unsupported.
- A write-routed raw helper uses `RouteOp::Write`.
- Existing constant raw SQL tests continue to pass.

# Core ORM Clean Areas

Category: Positive Findings, Security, Correctness, Simplicity
Severity: Informational

## Finding

The core ORM under `crates/umbral-core/src/orm` is not broadly unsafe or careless. Several important areas are clean, centralized, and worth preserving while fixing the sharper items above.

## Clean Areas Observed

- Main query construction uses SeaQuery and bound values rather than string concatenation. The typed and dynamic query paths build SQL through structured expressions in the normal case.
- `crates/umbral-core/src/orm/mod.rs:74-96` escapes LIKE literals, including wildcard characters and the escape character itself.
- `crates/umbral-core/src/orm/dynamic.rs:184-295` coerces dynamic string inputs by declared SQL type for JSON/equality/comparison predicates.
- `crates/umbral-core/src/orm/dynamic.rs:201-207` provides a `never_matches` predicate, and `crates/umbral-core/src/orm/dynamic.rs:817-838` uses it for invalid single-value filters.
- `crates/umbral-core/src/orm/dynamic.rs:363-397` centralizes read-side field policy in `may_serialize` and `visible_select_cols`.
- `crates/umbral-core/src/orm/dynamic.rs:1586-1758` applies that policy to normal dynamic string and JSON reads before selecting from the database.
- `crates/umbral-core/src/orm/queryset/mod.rs:601-627` centralizes soft-delete implicit predicates for typed querysets.
- `crates/umbral-core/src/orm/queryset/mod.rs:465-499` refuses `.only()` with typed terminals and points callers to `values`, avoiding partial-row hydration traps.
- `crates/umbral-core/src/orm/queryset/write_helpers.rs:78-163` centralizes typed insert normalization, cleaners, auto timestamps, auto users, and FK placeholder checks.
- `crates/umbral-core/src/orm/validation.rs` gives the write path structured validation and database error classification instead of surfacing only raw driver errors.
- `crates/umbral-core/src/orm/dynamic.rs:1841-2009` and `crates/umbral-core/src/orm/dynamic.rs:2034-2140` make dynamic JSON inserts transactional around parent insert, M2M junction writes, and M2M response hydration.

## Why It Matters

Most of the high-risk issues are drift issues: one branch or helper bypasses a centralized guard that already exists elsewhere. That is a good sign for remediation. The fix should usually be to reuse the existing guard more consistently, not to redesign the ORM.

## Recommendation

When addressing ORM findings, preserve these centers of gravity:

- Keep type coercion and fail-closed predicate logic in shared helpers.
- Keep read-side field visibility in one helper and route every JSON/readback terminal through it.
- Keep dynamic write planning shared between pool and transaction paths.
- Keep typed and dynamic terminals aligned through internal helpers where possible.

## Suggested Tests

- Add regression tests around the clean contracts, not only the defects: LIKE escaping, invalid equality filters matching no rows, hidden fields absent from normal dynamic reads, typed insert validators, and dynamic insert transaction rollback.

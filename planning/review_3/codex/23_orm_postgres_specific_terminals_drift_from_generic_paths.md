# Postgres-Specific ORM Terminals Drift From Generic Paths

Category: Correctness, Simplicity
Severity: Medium

## Finding

The core typed ORM has generic terminals that centralize related-object hydration, M2M parent ID seeding, `.only()` validation, validation, signals, audit, structured write errors, and atomic create behavior. The Postgres-only terminals are implemented as slimmer explicit-pool paths and do not inherit much of that behavior.

These helpers exist for models with Postgres-only field types, so the constraint is understandable. The problem is that they look like regular ORM terminals with a `_pg` suffix, but behave like lower-level escape hatches.

## Evidence

- `crates/umbral-core/src/orm/queryset/mod.rs:1680-1793` generic `fetch` rejects `.only()`, validates `join_related`, applies joins, seeds M2M parent IDs, and hydrates `select_related` and `prefetch_related`.
- `crates/umbral-core/src/orm/queryset/mod.rs:3627-3636` `fetch_pg` only builds and executes the base query.
- `crates/umbral-core/src/orm/queryset/mod.rs:3638-3687` `first_pg`, `exists_pg`, and `get_pg` are layered on that slimmer fetch path.
- `crates/umbral-core/src/orm/queryset/mod.rs:4031-4197` generic `Manager::create` validates typed input, classifies SQL errors, seeds and writes pending M2M, emits post-save signals, and records audit entries.
- `crates/umbral-core/src/orm/queryset/mod.rs:4654-4668` `create_pg` serializes, inserts, and returns the row without the generic create lifecycle.
- `crates/umbral-core/tests/array_field.rs:258-261`, `crates/umbral-core/tests/json_field.rs:203`, `crates/umbral-core/tests/fulltext_field.rs:271-329`, and similar tests use `fetch_pg` for Postgres-only field coverage.

## Risk

Postgres-only models can get different ORM semantics than portable models. Examples:

- `select_related`, `prefetch_related`, and `join_related` chains can be silently ignored by `fetch_pg`.
- M2M handles returned from `fetch_pg` may not have parent IDs seeded for later `.add` or `.remove` operations.
- `create_pg` can miss validation, signal, audit, M2M pending-write, and structured error behavior users expect from `create`.

## Recommendation

Either make these terminals visibly low-level or route them through shared internal helpers.

Preferred direction: extract backend-specific fetch/create execution helpers that generic and `_pg` paths both call, with bounds only where row hydration actually requires them. If that is too large, document `_pg` methods as low-level Postgres escape hatches and add runtime guards for unsupported chained features such as `select_related`, `prefetch_related`, `join_related`, `.only()`, and pending M2M writes.

## Suggested Tests

- A Postgres-only model with an M2M field created through `create_pg` records junction rows or fails with an explicit unsupported-feature error.
- `fetch_pg().select_related(...)` either hydrates relations or returns a clear error.
- `fetch_pg().only(&["id"])` returns the same clear partial-row error as `fetch`.
- `create_pg` fires or explicitly documents not firing post-save signals and audit entries.

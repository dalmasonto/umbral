# DatabaseRouter foundation — design

> Status: design / pending implementation plan. Date: 2026-06-16. Gap: gaps2 #69 (absorbs #22 done, #23 read/write split). Charter: `planning/hardening/00-charter.md`.

## Goal

Extract umbral's database-routing decisions out of inline ORM logic into a single swappable `DatabaseRouter` trait, and add the one primitive the framework lacks: a **request-scoped routing context**. This is the keystone that later unblocks read/write replica split (#23), database-per-tenant, schema-per-tenant multitenancy, and alternate backends — each as a custom router, with no further ORM surgery.

This spec is the **foundation only**, but a *complete* one: every trait method ships with a working consumer (no defined-but-dead surface). What it deliberately defers is the multitenancy *management ergonomics*, not the routing *mechanism* (see Non-goals).

## Non-goals (explicitly Phase 2+)

The foundation delivers a complete, working routing+schema *mechanism*. The following management/ergonomic layers are separate follow-up specs that build on it:

- A `Tenant` model and tenant provisioning.
- `migrate_schemas` — create + migrate every tenant schema (Django `django-tenants`' `migrate_schemas` equivalent).
- The tenant-vs-shared model classification (`SHARED_APPS`/`TENANT_APPS`): which models live in `public` vs each tenant schema. In the foundation, when a schema is active, *all* model tables are qualified with it.
- A built-in HTTP tenant-resolver (subdomain/header → tenant). The foundation ships the seam (`App::builder().route_context(...)`); the app supplies the resolver closure.
- Row-level (`tenant_id` column) tenancy and RLS activation (`SET LOCAL app.user_id`, ties gaps2 #79) — a different strategy, separate spec.
- User-facing multitenancy docs/guide.

## Current state (what already exists)

- Pools: `static POOLS: OnceLock<HashMap<String, DbPool>>` (`crates/umbral-core/src/db.rs`), keyed by alias, published once in `App::build`. `DbPool` is `Sqlite(SqlitePool) | Postgres(PgPool)`.
- Routing today: `resolve_pool<T>(explicit) -> DbPool` (`crates/umbral-core/src/orm/queryset/mod.rs:1116`), called by ~15 read/write terminals. Precedence: `.on(&pool)` → `Model::DATABASE` (`#[umbral(database="…")]`) → `Plugin::database()` → `"default"`. Keyed by **model**, resolved at **build time**. No request/tenant awareness, no swappable seam.
- Cross-DB FK guard (#22, done): boot-time check in `App::build` (`crates/umbral-core/src/app.rs`, Phase 2.5b) → `BuildError::CrossDatabaseForeignKey`, opt-out `#[umbral(db_constraint=false)]`.
- Per-DB migrations: the migrate engine walks operations per `table_alias`, one `umbral_migrations` table per database.
- Middleware contract (#68, shipped): `middleware::Middleware` async trait (`before_request`/`after_response`), `AppBuilder::middleware` / `Plugin::middleware`. This is the seam the request-context layer plugs into.

## Design

### 1. The `DatabaseRouter` trait

Lives in `crates/umbral-core/src/db/router.rs`; re-exported as `umbral::db::DatabaseRouter` and in the prelude.

```rust
pub trait DatabaseRouter: Send + Sync {
    /// Alias of the database to read this model from. Default: today's static precedence.
    fn db_for_read(&self, model: &ModelMeta, ctx: &RouteContext) -> Alias;

    /// Alias of the database to write this model to. Default: same as read.
    fn db_for_write(&self, model: &ModelMeta, ctx: &RouteContext) -> Alias;

    /// May a relation (FK) span these two models? Default: same alias only
    /// (the #22 cross-DB FK guard).
    fn allow_relation(&self, a: &ModelMeta, b: &ModelMeta) -> bool;

    /// Should this model be migrated on this database alias? Default: migrate
    /// each model on its assigned alias.
    fn allow_migrate(&self, alias: &str, model: &ModelMeta) -> bool;

    /// The Postgres schema to scope this request's queries to. Default: None
    /// (no schema qualification — today's behavior). Returning Some(schema)
    /// makes the SQL builder qualify table references with it.
    fn schema_for(&self, ctx: &RouteContext) -> Option<Schema> { None }
}
```

`Alias` is a small newtype over `String`/`&'static str` (the existing alias strings). `Schema` is a validated Postgres identifier newtype (constructed via a checked constructor — `^[a-zA-Z_][a-zA-Z0-9_]*$`, length-bounded — so a schema name can never be an injection vector; it is emitted as a quoted identifier regardless).

### 2. The default router — zero behavior change

`DefaultRouter` (used when the app installs no custom router) reproduces today's behavior exactly:

- `db_for_read` and `db_for_write` both return today's static precedence result (`Model::DATABASE` → `Plugin::database()` → `"default"`), ignoring `ctx`.
- `allow_relation` returns `a.alias == b.alias` (the #22 rule).
- `allow_migrate` returns `true` for a model on its own assigned alias.
- `schema_for` returns `None`.

The non-regression bar: the entire existing test suite (935 umbral-core tests + plugin suites) passes unchanged with `DefaultRouter` active.

### 3. `RouteContext` and propagation

`RouteContext` is the request-scoped value a router reads to make per-request decisions:

```rust
pub struct RouteContext {
    tenant: Option<TenantKey>,   // first-class common case
    extensions: Extensions,      // anymap-style typed store for app/plugin-specific keys
}
```

A router reads `ctx.tenant()` or downcasts a typed extension. The default router ignores it. This keeps the trait generic across strategies: read-replica routers key on nothing, db/schema-per-tenant routers key on `tenant`, bespoke routers on whatever they stashed.

Propagation uses `tokio::task_local!` — the per-request twin of umbral's ambient-`OnceLock` pool pattern:

- `crates/umbral-core/src/db/route_context.rs` owns the task-local plus `current() -> RouteContext` (returns the default context when unset) and `scope(ctx, fut)` (runs a future inside a context).
- `RouteContextLayer` (a `Middleware` from the #68 contract) builds a `RouteContext` from the request via an app-supplied resolver and runs the downstream request inside `scope(...)`. Installed via `App::builder().route_context(|req| RouteContext { … })`.
- The router reads `route_context::current()` ambiently in `resolve_pool` — no threading through call signatures.

### 4. Spawned-task safety (hard rule)

`route_context::current()` returns the **default** context whenever no task-local is set: background `umbral-tasks` jobs, boot, CLI, and tests. The router then falls back to the default database and `public` schema. It **never** silently inherits or guesses a tenant. A background job that must run as a tenant opts in explicitly via `route_context::scope(ctx, fut)`. This prevents the classic multitenancy data-leak (a pooled worker running tenant A's job against tenant B's context).

### 5. The resolve seam (read/write split — folds in #23)

`resolve_pool<T>` is split by operation:

- Read terminals (`fetch`, `first`, `count`, `exists`, `get`, `aggregate`, `values`) call `router.db_for_read(meta, &current())`.
- Write terminals (`create`, `bulk_create`, `update_values`, `update_expr`, `delete`, `get_or_create`, `update_or_create`) call `router.db_for_write(meta, &current())`.
- Both pass the resolved alias to the existing `db::pool_for_dispatched(alias)`.
- `.on(&pool)` remains a hard override: an explicit pool short-circuits the router entirely (preserving today's escape hatch and test ergonomics).

The dynamic path (`DynQuerySet`, admin/REST) routes the same way — it already carries `&ModelMeta`.

### 6. `schema_for` consumed via SQL-level schema qualification (option C — the performant path)

When the active router returns `schema_for(ctx) = Some(schema)`, the **SQL builder qualifies table references** with it: `"tenant_7"."post"` rather than `"post"`. Chosen over `SET search_path` (per-request or per-acquire) and connection-pinning because it costs **zero extra round-trips** and keeps the normal acquire-per-query pool model (no pinning, no per-connection mutable schema state, no leak/reset hygiene, no pool-concurrency tail under load). See Performance rationale.

Mechanics:

- A single schema-aware "table ref" helper is introduced; every place the ORM emits a table identifier routes through it: the typed `build_query_for` (`T::TABLE`), joins, M2M junction tables, subqueries, aggregates, and the dynamic path (`meta.table`). When `current()`'s active router yields a schema, the helper emits a sea-query schema-qualified `TableRef`; otherwise it emits the bare table (today's output, byte-identical).
- SQLite has no schemas → `schema_for` is ignored with a one-time `tracing::warn!` (mirrors how `umbral-rls` already skips SQLite).
- Foundation scope: when a schema is active, **all** model tables are qualified with it. The tenant-vs-shared (`public`) split is the Phase 2 refinement. **Known foundation limitation, stated plainly:** while a schema is active, the foundation cannot mix shared-`public` tables into the same request — every table is qualified with the tenant schema. The common pattern still works (the tenant registry is resolved *before* a schema is active, so that lookup is unqualified `public`), but a tenant request that needs to also read a genuinely shared/global table is exactly what the Phase 2 `SHARED_APPS` per-model classification handles. The foundation does not pretend to solve it.
- The schema is always emitted as a **quoted identifier** built from the validated `Schema` newtype, so it is never an injection vector.

### 7. `allow_relation` / `allow_migrate` — refactor existing behavior through the trait

- `allow_relation` *becomes* the #22 cross-DB FK boot guard: `App::build`'s Phase 2.5b calls `router.allow_relation(a, b)` instead of the inline `alias == alias` check. `BuildError::CrossDatabaseForeignKey` and the `db_constraint=false` opt-out are unchanged.
- `allow_migrate` gates the migrate engine's per-alias model walk: a model is migrated on an alias only when `router.allow_migrate(alias, model)`. Default `true` reproduces today.

Both get a test proving a custom router can veto (reject a relation / skip a migration).

### 8. Installation API

```rust
App::builder()
    .database("default", default_pool)
    .router(MyTenantRouter::new())          // optional; DefaultRouter if omitted
    .route_context(|req| RouteContext { … }) // optional; default context if omitted
    .build()?
```

The router is stored in a `OnceLock<Arc<dyn DatabaseRouter>>` published in `App::build` (same lifecycle/pattern as `POOLS`).

## Performance rationale (why option C)

| Approach | Per-request cost | Under load |
|---|---|---|
| Pin connection + `SET search_path` | +1 round-trip (the `SET`) | Worst: a request holds a pool connection for its whole duration; concurrent requests > pool size queue for a connection (hundreds-of-ms tail). |
| `SET` on every acquire | +1 round-trip × every query | Bad: linear in query count. |
| **Schema-qualified SQL (chosen)** | **0 round-trips** | Best: normal acquire-per-query pooling, full concurrency, no pinning, no per-connection state. |

A `SET search_path` is one network round-trip (sub-ms localhost, 1–5ms cross-AZ); the per-acquire variant multiplies it by query count, and connection-pinning adds a pool-contention tail far larger than 20ms under load. Option C makes the cost literally zero — the schema only changes how the query builder emits an identifier it already emits. Minor, accepted: the Postgres prepared-statement cache grows per-(schema, query), bounded by sqlx's LRU.

## Testing strategy

- Default-router non-regression: the full existing suite stays green (proves zero behavior change).
- Read/write split: a custom router routing reads → replica alias, writes → primary alias; assert each terminal hits the right pool (proves #23).
- Context seam: a db-per-tenant router keyed on `ctx.tenant`; set the tenant via `scope`, assert routing follows.
- Schema qualification (Postgres-gated, `#[ignore]` + `UMBRAL_TEST_POSTGRES_URL`, like `pk_uuid_postgres.rs`): two schemas, same model, rows isolated; assert generated SQL is schema-qualified and a query in schema A never sees schema B's rows.
- Spawned-task safety: a task with no context resolves to the default DB/`public` (proves the hard rule).
- `allow_relation` / `allow_migrate`: a vetoing router rejects a cross-DB relation / skips a migration.

## Risks & mitigations

- **Broad SQL-builder change (option C):** every table-ref emission routes through one helper. Mitigation: a single chokepoint helper + the non-regression suite asserting byte-identical SQL when no schema is active.
- **Ambient task-local correctness:** the spawned-task default-fallback rule + an explicit test prevent cross-tenant leakage.
- **Schema identifier injection:** `Schema` is a validated newtype, always emitted quoted.
- **Scope creep into Phase 2:** the Non-goals section is the contract; the foundation qualifies all tables uniformly and ships no tenant-management tooling.

## File-level change map (indicative)

- New: `crates/umbral-core/src/db/router.rs` (trait + `DefaultRouter` + `Alias`/`Schema`), `crates/umbral-core/src/db/route_context.rs` (`RouteContext` + task-local + `current`/`scope`), `RouteContextLayer` middleware.
- Modified: `orm/queryset/mod.rs` (`resolve_pool` → read/write split + router call; table-ref helper), the dynamic path (`orm/dynamic.rs`), `app.rs` (router/route_context builder methods + publish; Phase 2.5b → `allow_relation`), the migrate engine (per-alias walk → `allow_migrate`), facade re-exports + prelude.
- The schema-aware table-ref helper is the central new chokepoint both the typed and dynamic builders call.

## Follow-ups from the final review (2026-06-16)

These were surfaced by the post-implementation code review. None block the foundation (all are invisible under `DefaultRouter`); they are the items the Phase 2 / replica work must address.

- **FIXED — `get_or_create` / `update_or_create` read-your-writes.** Their existence probe (and `update_or_create`'s post-update re-fetch) now pin the write pool via `pin_to_pool` (`orm/queryset/mod.rs`), so a read/write-split router can't miss a just-written row on a lagging replica and insert a duplicate. Regression test: `tests/router_upsert_readwrite.rs`. (The separate no-transaction gap, gaps2 #71, is still open.)
- **Minor — M2M junction pool selection bypasses the router.** `orm/m2m.rs` `set_junction_dynamic` / `load_junction_selection` (and `_in_tx` variants) pick the pool via raw `pool_dispatched()` (default alias). Their *table refs* are schema-qualified (so schema-per-tenant is fine), but a db-per-tenant / replica router would land junction writes on the default pool. Junctions carry no `ModelMeta`, so closing this needs a junction-aware routing key.
- **ADDRESSED (doc) — `raw()` is classified `RouteOp::Read`.** A `raw("UPDATE …")` would hit the read replica under a split router. Documented in the method: most `raw` is SELECT, and a raw write must pin the write pool via `.on()`. A typed write-variant remains a possible future addition.
- **Minor — an unregistered alias from a custom router panics** (`pool_for_dispatched` → caught by the panic layer → 500). Consider a typed error for misconfigured tenant routers.
- **Minor — SQLite + `schema_for == Some` is unguarded.** `schema_qualified_table` is backend-agnostic; a router wrongly returning `Some` on SQLite emits a schema-qualified ref SQLite rejects at execution. The spec's promised one-time warn-and-skip needs backend awareness at the helper — a Phase-2 follow-up.

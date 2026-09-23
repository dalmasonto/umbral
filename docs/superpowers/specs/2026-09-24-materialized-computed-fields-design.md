# Materialized (computed) fields — auto-refresh design

**Status:** approved (2026-09-24). Closes the design half of `planning/gaps6.md` #7.
**Scope:** v1 — single-model computed fields, eager after-commit, framework-owned write-back. Cached-aggregate integration, deferred (task-queue) refresh, partial recompute, and cross-field cycle detection are explicitly out of scope (see §9).

## 1. Problem

A denormalized or computed value — a projection over another table, a cached aggregate — goes stale silently whenever a related row is inserted, updated, or deleted. Today the framework offers no way to keep it current except asking the developer to *remember* to recompute it at every write site. That "remember to refresh everywhere" burden is the classic cache-invalidation trap: a single missed call site produces wrong data with no error. The cited real consumer is the web3clubs backend's `payment` field, a projection over `RSVP`.

The framework should own the refresh so **the declaration site is the only place the dependency is expressed**, wired through the existing signals layer with dependency inversion — the source table never names the derived one.

## 2. Goals / non-goals

**Goals**
- Declare, in one place, that a column on model `M` is derived from one or more *source* tables, plus a closure that recomputes its value.
- The framework keeps that column fresh automatically on every source insert / update / delete, through the ORM, on both backends, with no per-write-site code.
- Zero cost for models that declare no materialized field; cost is paid only by a declared target's source tables.

**Non-goals (v1)**
- Postgres `MATERIALIZED VIEW` refresh (that is the existing `#[umbral(materialized_view = …)]`, a different, PG-only, read-only feature).
- Cached aggregates tied into the heavy-relations epic's JOIN engine.
- Deferred (background-task) refresh.
- Partial / incremental recompute over large source sets.
- Cross-materialized-field dependency graphs and full cycle detection.

## 3. Decisions (locked during brainstorming)

| Decision | Choice | Why |
|---|---|---|
| v1 shape | Single-model computed field; value stored in `M`'s own column | Covers the web3clubs case; simplest shippable unit; extensible. |
| Refresh timing | Eager, **after-commit** | Reuses gaps6 #15's after-commit signal delivery; source rows are committed and visible before recompute reads them; no `umbral-tasks` dependency. Slightly eventually-consistent within the triggering request, which the consumer accepted. |
| Declaration surface | Builder registration: `App::builder().materialize(Materialized::<M>::field(col).from::<S>(key_fn).recompute(f))` | The recompute is a closure and cannot live in a `const` derive attribute. No derive-macro change; the column stays an ordinary typed field. |
| Write-back | Framework owns it | Closure stays pure (returns the value); single choke point through the ORM; framework can apply the re-entrancy guard. |
| Location | `umbral-core`, exposed via the `umbral` facade | The builder is `AppBuilder` (core); the wiring uses core signals + the ambient ORM pool. Not a separate plugin in v1. |

## 4. Public API

```rust
use umbral::prelude::*;

App::builder()
    .database("default", pool)
    .model::<Booking>()
    .model::<Rsvp>()
    .materialize(
        Materialized::<Booking>::field(booking::PAYMENT_TOTAL)   // target model + column
            .from::<Rsvp>(|r: &Rsvp| Some(r.booking_id))         // source → affected target pk
            .recompute(|booking_id: i64| async move {            // fresh value for that target row
                let agg = Rsvp::objects()
                    .filter(rsvp::BOOKING_ID.eq(booking_id))
                    .filter(rsvp::PAID.eq(true))
                    .aggregate(&[("total", Aggregate::sum("amount"))])
                    .await
                    .unwrap_or_default();
                agg["total"].as_i64().unwrap_or(0)
            }),
    )
    .build()?;
```

### 4.1 Types

- `Materialized<M: Model>` — the spec builder.
  - `Materialized::<M>::field(col)` — `col` is any column token implementing the `ColName` surface (`.name() -> &'static str`, gaps6 #1). Fixes the target model `M`, the target column name, and (via `M`) the target pk column + type. Returns a builder awaiting `.from(...)` and `.recompute(...)`.
  - `.from::<S: Model>(key_fn)` where `key_fn: Fn(&S) -> Option<Pk> + Send + Sync + 'static`. `Pk` is the target pk value type (`i64` / `String` / `Uuid`). Returning `None` skips this source event (e.g. a null FK). May be called more than once for multiple source models; every source shares the one `recompute`.
  - `.recompute(f)` where `f: Fn(Pk) -> Fut + Send + Sync + 'static`, `Fut: Future<Output = V> + Send`, `V: Serialize + Send`. Terminal-ish: yields the finished `MaterializedSpec` (type-erased) that `AppBuilder::materialize` accepts.
- `AppBuilder::materialize(spec: impl Into<MaterializedSpec>) -> Self` — collects the spec; the actual signal subscriptions happen in `build()`.

### 4.2 Type erasure

`Materialized<M>` is generic; the registry stores an erased `MaterializedSpec` so `AppBuilder` can hold a `Vec<MaterializedSpec>` regardless of `M` / `S` / `V`. Each `.from::<S>` closes over `S` by producing an erased source registration: `{ source_table: &'static str, subscribe: Box<dyn Fn() + Send + Sync> }` (or an equivalent boxed async handler). `.recompute` and the write-back are captured behind `Arc<dyn Fn(Pk) -> BoxFuture<Value>>` and the target `ModelMeta` + column name, all monomorphized at the builder call site and boxed. `Pk` and `V` are erased to `serde_json::Value` at the boundary (the key_fn's `Pk` is serialized to a JSON scalar to filter by pk; `V` is serialized to a JSON value to write back), so the internal handler is non-generic.

## 5. Boot wiring

`AppBuilder::build()` (after the DB pool and settings are installed, before/at the existing plugin `on_ready` phase) iterates the collected `MaterializedSpec`s and, for each source registration, subscribes an **async** handler (`signals::subscribe_async`) to both `post_save:<S::TABLE>` and `post_delete:<S::TABLE>`. Subscription is ambient (the signals registry is process-global), so no handle threading is needed — same mechanism umbral-signals' `on_model::<S>()` uses.

A boot-time **validation check** (system check) verifies, for every spec: the target column exists on `M`'s `ModelMeta`, `M` has a single-column primary key, and the target column is not itself the pk. A failure aborts boot with a clear message (secure-by-default posture: a misdeclared materialization is a bug, not a silent no-op).

## 6. The recompute handler (per source event)

For a `post_save`/`post_delete` on a registered source `S`:

1. Decode `payload["instance"]` into `S` (`serde_json::from_value`). On decode failure, log and skip (never panic a signal handler). Delete payloads carry the full row when subscribed (gaps6 #14/#15), so `key_fn` sees the pre-delete field values.
2. `let affected = key_fn(&s);` — `None` → skip.
3. `let value = recompute(affected_pk).await;` — the closure reads the (committed) source rows through the ORM and returns the fresh value.
4. Write-back through the late-bound ORM path:
   `DynQuerySet::for_meta(&m_meta).filter_pk_eq(affected_pk).update_json({ target_col: value }).await`.
   This works for any pk type and both backends (the ORM is the single DB interface — no raw SQL in the feature). A new `DynQuerySet::filter_pk_eq(&self, pk: &serde_json::Value)` convenience (typed over the meta's pk column) is added if not already expressible; the update is the existing `update_json`.

Errors in steps 3–4 are logged loudly and do **not** propagate (an async signal handler cannot fail the already-committed source write). Losing one refresh is bad; the TTL-free nature means the next source write re-refreshes, and the boot check plus tests guard the happy path.

## 7. Loop guard

The write-back is an UPDATE on `M`, which fires `post_save:<M>` / `bulk_post_save:<M>`. To prevent a self-cascade (e.g. `M` also registered as a source whose recompute updates `M`), the handler brackets its write-back with a **task-local re-entrancy set** keyed by `(target_table, target_column)`:

- Before the write-back, if `(table, col)` is already in the active set, log a warning (`"materialized field <table>.<col> recompute re-entered; skipping to break a cycle"`) and skip.
- Otherwise insert, perform the write-back, remove.

This breaks direct self-loops and caps depth at one for a given target field within a single async task. Multi-field cycles (A→B→A) are out of v1 scope and are the reason cycle detection is listed as deferred; the guard degrades safely (it stops, it does not hang).

## 8. Testing (behavioral — real rows, the real public path)

`crates/umbral-core/tests/materialized_fields.rs`, SQLite in-memory/file pool:

1. **Surface (create):** register `Booking.payment_total` materialized over `Rsvp`; insert real `Rsvp` rows through `Rsvp::objects().create(...)`; read `Booking` back via `Booking::objects().get(...)` and assert `payment_total` equals the summed amount — with **zero manual recompute calls**.
2. **Decrement (delete):** delete an `Rsvp`; assert `payment_total` drops.
3. **Change (update):** update an `Rsvp`'s amount / paid flag; assert `payment_total` tracks it.
4. **After-commit / rollback:** a source write inside `db::transaction_sqlite` that rolls back fires no recompute (the target column is unchanged); a committed one does (reuses #15's after-commit delivery).
5. **Null-key skip:** a source row whose `key_fn` returns `None` triggers no write-back.
6. **Multiple sources:** two `.from::<S1>` / `.from::<S2>` both refresh the same target.
7. **Loop guard:** a pathological self-referential registration logs the warning and terminates (no hang), asserted via a bounded timeout.
8. **Zero-cost:** a model with no materialized field, and a source table for none, subscribe nothing (assert via `signals::has_subscribers`).

## 9. Deferred (later gaps entries)

- Cached-aggregate / `annotate_*` integration with the heavy-relations epic (`docs/specs/orm-heavy-relations-epic.md`) — a cached `annotate_count` is one instance of this same problem.
- Deferred refresh via `umbral-tasks::enqueue` (per-field eager|deferred choice).
- Partial / incremental recompute for aggregates over large source sets.
- Cross-materialized-field dependency graph + full cycle detection.
- Optional derive-attribute marker on the target column (read-only hint / documentation), if ergonomics warrant it.

## 10. Files touched (indicative)

- `crates/umbral-core/src/orm/materialized.rs` (new) — `Materialized<M>`, `MaterializedSpec`, the erased source registration, the recompute handler, the re-entrancy guard.
- `crates/umbral-core/src/app.rs` — `AppBuilder::materialize(...)`, spec storage, build-time subscription + system check.
- `crates/umbral-core/src/orm/dynamic.rs` — `DynQuerySet::filter_pk_eq` if needed for the generic-pk write-back.
- `crates/umbral-core/src/lib.rs` + `crates/umbral/src/...` — facade re-exports (`Materialized`, in the prelude).
- `crates/umbral-core/tests/materialized_fields.rs` (new) — the behavioral suite in §8.
- `documentation/docs/v0.0.1/orm/materialized-fields.mdx` (new) — purpose + one example + link to this spec.

## 11. See also

- Signals: `crates/umbral-core/src/signals.rs`, `plugins/umbral-signals/src/lib.rs` (`on_model`).
- After-commit signal delivery: gaps6 #15 (`crates/umbral-core/src/orm/queryset/tx.rs`, `db.rs`).
- Column `.name()` surface: gaps6 #1 (`crates/umbral-core/src/orm/column.rs`).
- Aggregate/annotate terminals a recompute closure can use: `crates/umbral-core/src/orm/aggregate.rs`, `queryset/aggregate_path.rs`.

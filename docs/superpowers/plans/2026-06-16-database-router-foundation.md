# DatabaseRouter Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace umbral's hardcoded database-routing logic with a swappable `DatabaseRouter` trait plus a request-scoped routing context, with a default router that reproduces today's behavior byte-for-byte and a zero-round-trip schema-qualification path for schema-per-tenant.

**Architecture:** A `DatabaseRouter` trait (`db_for_read`/`db_for_write`/`allow_relation`/`allow_migrate`/`schema_for`) is stored in a process `OnceLock`, installed via `App::builder().router(...)`, defaulting to `DefaultRouter`. A `RouteContext` rides a `tokio::task_local!`, populated by a `RouteContextLayer` middleware, read ambiently by the router. The typed and dynamic query builders route every read/write through the router and qualify table names with `schema_for`'s result directly in the SQL.

**Tech Stack:** Rust, sea-query 0.32 (SQL builder), sqlx (pools), axum/tower (middleware), tokio (`task_local!`). Workspace lives at `crates/Cargo.toml`; run all `cargo` from inside `crates/`.

**Source of truth:** `docs/superpowers/specs/2026-06-16-database-router-foundation-design.md`. Read it before starting.

**Global conventions for every task:**
- All `cargo` commands run from `crates/`.
- Pre-commit (per `CLAUDE.md`): `cargo fmt`, `cargo clippy --all-targets`, `cargo build`, then the task's tests. The disk on this machine is tight — if a build fails with `No space left on device`, run `rm -rf crates/target/debug/incremental` and retry; do NOT touch `examples/*/target`.
- Prose in docs is not hard-wrapped (one line per paragraph).
- The non-regression bar for the whole plan: `cargo test -p umbral-core` stays green (935 tests at plan start) with `DefaultRouter` active.

---

## File Structure

`db.rs` stays a file; Rust lets a `db/` directory hold its submodules as long as `db.rs` declares them (`mod router;` resolves to `src/db/router.rs`). No move of `db.rs` is needed.

- **Create** `crates/umbral-core/src/db/router.rs` — `Alias`, `Schema`, the `DatabaseRouter` trait, `DefaultRouter`, the `ROUTER` `OnceLock`, `router()`, `install_router()`.
- **Create** `crates/umbral-core/src/db/route_context.rs` — `TenantKey`, `RouteContext`, the `tokio::task_local!`, `current()`, `scope()`, and the `RouteContextLayer` middleware.
- **Modify** `crates/umbral-core/src/db.rs` — add `pub mod router;` and `pub mod route_context;`; re-export the public items.
- **Modify** `crates/umbral-core/src/orm/queryset/mod.rs` — `resolve_pool` → router-backed read/write split; route the typed FROM/table clauses through `schema_qualified_table`.
- **Modify** `crates/umbral-core/src/orm/dynamic.rs` — route `DynQuerySet` terminals through the router; qualify its FROM clauses.
- **Modify** `crates/umbral-core/src/app.rs` — `.router()` / `.route_context()` builder methods, publish the router in `build()`, route the cross-DB FK guard through `allow_relation`, install the `RouteContextLayer`.
- **Modify** `crates/umbral-core/src/migrate.rs` — gate the per-alias op walk through `allow_migrate`; add a cached `model_meta_ref` accessor.
- **Modify** `crates/umbral/src/lib.rs` — facade re-exports + prelude additions.
- **Create** `documentation/docs/v0.0.1/orm/database-router.mdx` — the user-facing page.
- **Tests:** new integration tests under `crates/umbral-core/tests/` (`router_default.rs`, `router_read_write_split.rs`, `route_context.rs`, `router_dynamic.rs`, `router_allow.rs`, `router_schema_qualified.rs`, `router_schema_postgres.rs`) and unit tests inside the new modules.

---

### Task 1: Module scaffolding + `Alias` and `Schema` newtypes

**Files:**
- Create: `crates/umbral-core/src/db/router.rs`
- Modify: `crates/umbral-core/src/db.rs` (add module declarations)

- [ ] **Step 1: Write the failing test** — append to `crates/umbral-core/src/db/router.rs` (create the file with this content):

```rust
//! The swappable `DatabaseRouter` trait and its default implementation.
//! See `docs/superpowers/specs/2026-06-16-database-router-foundation-design.md`.

/// A database alias — the key under which a pool is registered
/// (`App::builder().database(alias, pool)`), e.g. `"default"`, `"replica"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Alias(String);

impl Alias {
    pub fn new(s: impl Into<String>) -> Self {
        Alias(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// The conventional default alias.
    pub fn default_alias() -> Self {
        Alias("default".to_string())
    }
}

impl From<&str> for Alias {
    fn from(s: &str) -> Self {
        Alias(s.to_string())
    }
}
impl From<String> for Alias {
    fn from(s: String) -> Self {
        Alias(s)
    }
}

/// A validated Postgres schema identifier. Constructed only through
/// [`Schema::new`], which rejects anything that isn't a safe identifier,
/// so a schema name can never be a SQL-injection vector — it is always
/// emitted as a quoted identifier regardless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema(String);

impl Schema {
    /// Validate and wrap a schema name: `^[A-Za-z_][A-Za-z0-9_]*$`, 1..=63 chars
    /// (Postgres identifier limit). Returns `None` for anything else.
    pub fn new(s: impl Into<String>) -> Option<Self> {
        let s = s.into();
        let ok = (1..=63).contains(&s.len())
            && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        ok.then_some(Schema(s))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_accepts_valid_identifiers_and_rejects_the_rest() {
        assert!(Schema::new("tenant_7").is_some());
        assert!(Schema::new("_private").is_some());
        assert!(Schema::new("public").is_some());
        // rejects injection / malformed
        assert!(Schema::new("").is_none());
        assert!(Schema::new("1tenant").is_none());
        assert!(Schema::new("a b").is_none());
        assert!(Schema::new("drop\";--").is_none());
        assert!(Schema::new("a".repeat(64)).is_none());
    }

    #[test]
    fn alias_roundtrips() {
        assert_eq!(Alias::from("replica").as_str(), "replica");
        assert_eq!(Alias::default_alias().as_str(), "default");
    }
}
```

- [ ] **Step 2: Wire the module** — in `crates/umbral-core/src/db.rs`, add near the top (after the existing `use` block, before the `DbPool` enum at line 58):

```rust
pub mod route_context;
pub mod router;
```

- [ ] **Step 3: Run the test to verify it compiles and passes**

Run: `cd crates && cargo test -p umbral-core --lib db::router::tests`
Expected: 2 tests pass. (Note: `route_context` is declared but its file doesn't exist yet — create an empty placeholder `crates/umbral-core/src/db/route_context.rs` containing only `//! placeholder` so the crate compiles; Task 2 fills it in.)

- [ ] **Step 4: Commit**

```bash
cd .
git add crates/umbral-core/src/db/router.rs crates/umbral-core/src/db/route_context.rs crates/umbral-core/src/db.rs
git commit -m "feat(db): Alias + validated Schema newtypes for routing"
```

---

### Task 2: `RouteContext` + task-local (`current`/`scope`) + spawned-task fallback

**Files:**
- Modify: `crates/umbral-core/src/db/route_context.rs` (replace the placeholder)

- [ ] **Step 1: Write the full module with failing tests** — replace the contents of `crates/umbral-core/src/db/route_context.rs`:

```rust
//! The request-scoped routing context: a `tokio::task_local!` value the
//! `DatabaseRouter` reads to make per-request (per-tenant) decisions. The
//! per-request twin of umbral's ambient-`OnceLock` pool pattern.

use std::future::Future;
use std::sync::Arc;

/// An opaque tenant identifier. Apps that don't do multitenancy never set it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TenantKey(String);

impl TenantKey {
    pub fn new(s: impl Into<String>) -> Self {
        TenantKey(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The request-scoped routing context. Carries the common-case tenant plus
/// an extensible typed store so any app/plugin can stash its own routing key.
#[derive(Clone, Default)]
pub struct RouteContext {
    tenant: Option<TenantKey>,
    extensions: http::Extensions,
}

impl RouteContext {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_tenant(mut self, tenant: TenantKey) -> Self {
        self.tenant = Some(tenant);
        self
    }
    pub fn tenant(&self) -> Option<&TenantKey> {
        self.tenant.as_ref()
    }
    /// Stash a typed routing value for a custom router to read back.
    pub fn insert<T: Clone + Send + Sync + 'static>(&mut self, value: T) {
        self.extensions.insert(value);
    }
    /// Read a typed routing value previously stashed via [`Self::insert`].
    pub fn get<T: Clone + Send + Sync + 'static>(&self) -> Option<&T> {
        self.extensions.get::<T>()
    }
}

tokio::task_local! {
    static ROUTE_CONTEXT: Arc<RouteContext>;
}

/// The current request's routing context. Returns a **default** context when
/// none is set — background `umbral-tasks` jobs, boot, CLI, and tests. The
/// router then falls back to the default DB / `public` schema; it never
/// silently inherits or guesses a tenant.
pub fn current() -> Arc<RouteContext> {
    ROUTE_CONTEXT
        .try_with(|c| c.clone())
        .unwrap_or_else(|_| Arc::new(RouteContext::default()))
}

/// Run `fut` with `ctx` as the ambient routing context. The explicit opt-in a
/// background job uses to run as a tenant.
pub async fn scope<F: Future>(ctx: RouteContext, fut: F) -> F::Output {
    ROUTE_CONTEXT.scope(Arc::new(ctx), fut).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn current_is_default_when_unset() {
        // No scope established: spawned-task / background fallback.
        assert!(current().tenant().is_none());
    }

    #[tokio::test]
    async fn scope_sets_and_restores_context() {
        let ctx = RouteContext::new().with_tenant(TenantKey::new("acme"));
        scope(ctx, async {
            assert_eq!(current().tenant().unwrap().as_str(), "acme");
        })
        .await;
        // Outside the scope, back to default.
        assert!(current().tenant().is_none());
    }

    #[tokio::test]
    async fn spawned_task_does_not_inherit_context() {
        let ctx = RouteContext::new().with_tenant(TenantKey::new("acme"));
        scope(ctx, async {
            // A freshly spawned task has NO ambient context (task-locals
            // don't cross spawn). This is the hard safety rule: no silent
            // tenant inheritance into background work.
            let handle = tokio::spawn(async { current().tenant().cloned() });
            assert!(handle.await.unwrap().is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn extensions_store_typed_values() {
        #[derive(Clone, PartialEq, Debug)]
        struct Region(&'static str);
        let mut ctx = RouteContext::new();
        ctx.insert(Region("eu"));
        scope(ctx, async {
            assert_eq!(current().get::<Region>(), Some(&Region("eu")));
        })
        .await;
    }
}
```

- [ ] **Step 2: Confirm `http` is a dependency** — `http::Extensions` is used. Check `crates/umbral-core/Cargo.toml` for `http`. It is already a transitive/direct dep (used by `hosts.rs`, `errors.rs`). If `cargo build` complains it's not a direct dependency, add `http = "1"` under `[dependencies]` in `crates/umbral-core/Cargo.toml`.

- [ ] **Step 3: Run tests to verify they pass**

Run: `cd crates && cargo test -p umbral-core --lib db::route_context::tests`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
cd .
git add crates/umbral-core/src/db/route_context.rs crates/umbral-core/Cargo.toml
git commit -m "feat(db): request-scoped RouteContext on a task-local with spawn-safe fallback"
```

---

### Task 3: `DatabaseRouter` trait + `DefaultRouter` + ambient `router()`

**Files:**
- Modify: `crates/umbral-core/src/db/router.rs` (add the trait, default impl, installer)

- [ ] **Step 1: Append the trait, default router, and installer to `db/router.rs`** (before the existing `#[cfg(test)] mod tests`):

```rust
use std::sync::{Arc, OnceLock};

use crate::db::route_context::RouteContext;
use crate::migrate::ModelMeta;

/// The operation a route is being resolved for. The query terminal knows
/// whether it is reading or writing; this is passed to the seam, not stored
/// in the context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteOp {
    Read,
    Write,
}

/// Swappable routing policy. Every decision umbral makes about *which*
/// database/relation/migration target, plus the optional per-request schema,
/// flows through this trait. The default methods reproduce today's behavior;
/// install a custom impl via `App::builder().router(MyRouter)`.
pub trait DatabaseRouter: Send + Sync {
    /// Alias of the database to read `model` from for this request.
    fn db_for_read(&self, model: &ModelMeta, ctx: &RouteContext) -> Alias {
        let _ = ctx;
        default_alias_for(model)
    }

    /// Alias of the database to write `model` to for this request.
    fn db_for_write(&self, model: &ModelMeta, ctx: &RouteContext) -> Alias {
        let _ = ctx;
        default_alias_for(model)
    }

    /// May a relation (FK) span these two models? Default: same alias only
    /// (the #22 cross-DB FK guard).
    fn allow_relation(&self, a: &ModelMeta, b: &ModelMeta) -> bool {
        default_alias_for(a) == default_alias_for(b)
    }

    /// Should `model` be migrated on database `alias`? Default: yes when
    /// `alias` is the model's assigned alias.
    fn allow_migrate(&self, alias: &str, model: &ModelMeta) -> bool {
        default_alias_for(model).as_str() == alias
    }

    /// The Postgres schema to scope this request's queries to. Default: None
    /// (no qualification — today's behavior). `Some(schema)` makes the SQL
    /// builder schema-qualify table references.
    fn schema_for(&self, ctx: &RouteContext) -> Option<Schema> {
        let _ = ctx;
        None
    }
}

/// Today's static precedence, resolved by name: per-model `Model::DATABASE`
/// then per-plugin `Plugin::database()` (both folded into `MODEL_ALIASES` at
/// build) then `"default"`. This is exactly what the old `resolve_pool` did.
fn default_alias_for(model: &ModelMeta) -> Alias {
    match crate::migrate::model_alias(&model.name) {
        Some(a) => Alias::new(a),
        None => Alias::default_alias(),
    }
}

/// The zero-override router. Every method is the trait default.
#[derive(Debug, Default)]
pub struct DefaultRouter;

impl DatabaseRouter for DefaultRouter {}

static ROUTER: OnceLock<Arc<dyn DatabaseRouter>> = OnceLock::new();

/// Install the app's router. Called once during `App::build`. Idempotent
/// no-op on a second call (mirrors `db::init`'s set-once discipline but
/// without the panic, so tests that build twice don't blow up).
pub(crate) fn install_router(router: Arc<dyn DatabaseRouter>) {
    let _ = ROUTER.set(router);
}

/// The ambient router: the installed one, or `DefaultRouter` before/without
/// `App::build` (boot, CLI, low-level tests).
pub fn router() -> Arc<dyn DatabaseRouter> {
    ROUTER
        .get()
        .cloned()
        .unwrap_or_else(|| DEFAULT.clone())
}

static DEFAULT: OnceLock<Arc<dyn DatabaseRouter>> = OnceLock::new();
fn default_router_arc() -> Arc<dyn DatabaseRouter> {
    DEFAULT
        .get_or_init(|| Arc::new(DefaultRouter))
        .clone()
}
```

Note: replace the `router()` body's `DEFAULT.clone()` with `default_router_arc()` (the `DEFAULT` `OnceLock` is initialized lazily by `default_router_arc`). Final `router()`:

```rust
pub fn router() -> Arc<dyn DatabaseRouter> {
    ROUTER.get().cloned().unwrap_or_else(default_router_arc)
}
```

- [ ] **Step 2: Verify `ModelMeta` and `model_alias` are reachable.** `crate::migrate::ModelMeta` and `crate::migrate::model_alias` are both `pub` (confirmed: `migrate.rs:289` and the `ModelMeta` struct). `cargo build -p umbral-core` should compile.

- [ ] **Step 3: Run build + existing router tests**

Run: `cd crates && cargo test -p umbral-core --lib db::router`
Expected: PASS (the Task 1 newtype tests still pass; no new behavior to unit-test here — `DefaultRouter`'s behavior depends on the global `MODEL_ALIASES`, so it's covered by integration in Task 5).

- [ ] **Step 4: Commit**

```bash
cd .
git add crates/umbral-core/src/db/router.rs
git commit -m "feat(db): DatabaseRouter trait + DefaultRouter + ambient router()"
```

---

### Task 4: Facade re-exports + `App::builder().router()` + publish in `build()`

**Files:**
- Modify: `crates/umbral-core/src/db.rs` (re-export router/context items)
- Modify: `crates/umbral-core/src/app.rs` (builder field + method + publish)
- Modify: `crates/umbral/src/lib.rs` (facade + prelude)

- [ ] **Step 1: Re-export from `db.rs`** — add after the `pub mod` lines from Task 1:

```rust
pub use route_context::{current as route_context, RouteContext, TenantKey};
pub use router::{router, Alias, DatabaseRouter, DefaultRouter, RouteOp, Schema};
```

(Keep `scope` reachable as `crate::db::route_context::scope` — it is `pub` in the module.)

- [ ] **Step 2: Add the builder field + method** — in `crates/umbral-core/src/app.rs`:

In the `AppBuilder` struct (near the `middleware` field at line 134), add:
```rust
    router: Option<std::sync::Arc<dyn crate::db::DatabaseRouter>>,
```
In the builder's `Default`/`new` init (near line 157), add:
```rust
    router: None,
```
Add the method next to `database()` (after line 185):
```rust
    /// Install a custom [`crate::db::DatabaseRouter`]. Omit to use
    /// `DefaultRouter` (today's static per-model routing).
    pub fn router<R: crate::db::DatabaseRouter + 'static>(mut self, router: R) -> Self {
        self.router = Some(std::sync::Arc::new(router));
        self
    }
```

- [ ] **Step 3: Publish the router in `build()`** — in `app.rs`, immediately after the `db::init(self.databases);` line (line 720):

```rust
        if let Some(router) = self.router.take() {
            crate::db::router::install_router(router);
        }
```

(`self` is `mut` in `build`; `self.router.take()` requires `router: Option<...>`. If `build(self)` is by-value not `&mut`, use `self.router` directly: `if let Some(router) = self.router { crate::db::router::install_router(router); }` — match the existing `build` signature; `db::init(self.databases)` already moves a field, so by-value moves are fine.)

- [ ] **Step 4: Facade re-exports** — in `crates/umbral/src/lib.rs`, extend the `pub mod db` re-export block (line 180):

```rust
    pub use umbral_core::db::{
        Alias, DatabaseRouter, DbPool, DefaultRouter, RouteContext, RouteOp, Schema, TenantKey,
        Transaction, TxFuture, begin, begin_pg, begin_sqlite, connect, connect_sqlite, pool,
        pool_dispatched, pool_for, pool_for_dispatched, registered_aliases, route_context, router,
        transaction, transaction_pg, transaction_sqlite,
    };
    pub use umbral_core::db::route_context::scope as route_context_scope;
```

In the prelude (`pub mod prelude`, near the `Middleware` re-export at line 25), add:
```rust
    pub use crate::db::{DatabaseRouter, RouteContext, TenantKey};
```

- [ ] **Step 5: Build the whole workspace**

Run: `cd crates && cargo build`
Expected: clean build (facade + core compile).

- [ ] **Step 6: Commit**

```bash
cd .
git add crates/umbral-core/src/db.rs crates/umbral-core/src/app.rs crates/umbral/src/lib.rs
git commit -m "feat(app): App::builder().router(...) install + facade re-exports"
```

---

### Task 5: Typed read/write routing — refactor `resolve_pool` through the router

**Files:**
- Modify: `crates/umbral-core/src/orm/queryset/mod.rs` (`resolve_pool` + call sites)
- Modify: `crates/umbral-core/src/migrate.rs` (add a cached `model_meta_ref`)
- Test: `crates/umbral-core/tests/router_read_write_split.rs`

The seam needs `T`'s `ModelMeta` to call the router. Add a cached by-name accessor that returns a reference (no per-call clone), and have the seam fall back to today's behavior when the registry isn't initialised (so the 935 registry-less tests stay green).

- [ ] **Step 1: Add `model_meta_ref` to `migrate.rs`** — near `model_alias` (line 289). First check whether the perf work already added a cached `model_meta_by_table`/`model_meta_for_table` map; if a `OnceLock<HashMap<String, ModelMeta>>` cache keyed by table exists, add a by-name sibling reading the same registry. Otherwise add:

```rust
static MODEL_META_BY_NAME: OnceLock<std::collections::HashMap<String, ModelMeta>> = OnceLock::new();

/// Cached `&ModelMeta` lookup by model name. Returns `None` before
/// `App::build` populates the registry (low-level tests), which the routing
/// seam treats as "fall back to legacy static routing".
pub fn model_meta_ref(name: &str) -> Option<&'static ModelMeta> {
    if !is_initialised() {
        return None;
    }
    MODEL_META_BY_NAME
        .get_or_init(|| {
            registered_models()
                .into_iter()
                .map(|m| (m.name.clone(), m))
                .collect()
        })
        .get(name)
}
```

- [ ] **Step 2: Write the failing integration test** — create `crates/umbral-core/tests/router_read_write_split.rs`:

```rust
//! A custom router that splits reads → "replica", writes → "default" proves
//! the read/write seam (#23) and that ctx flows into the router.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use umbral::db::{Alias, DatabaseRouter, RouteContext};
use umbral::migrate::ModelMeta;

#[derive(
    Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model,
)]
#[umbral(table = "rw_widget")]
struct Widget {
    id: i64,
    name: String,
}

static READS: AtomicUsize = AtomicUsize::new(0);
static WRITES: AtomicUsize = AtomicUsize::new(0);

struct SplitRouter;
impl DatabaseRouter for SplitRouter {
    fn db_for_read(&self, _m: &ModelMeta, _c: &RouteContext) -> Alias {
        READS.fetch_add(1, Ordering::SeqCst);
        Alias::new("replica")
    }
    fn db_for_write(&self, _m: &ModelMeta, _c: &RouteContext) -> Alias {
        WRITES.fetch_add(1, Ordering::SeqCst);
        Alias::new("default")
    }
}

async fn make_pool(path: &str) -> sqlx::SqlitePool {
    let pool = umbral_core::db::connect_sqlite(path).await.unwrap();
    sqlx::query("CREATE TABLE rw_widget (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    pool
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_hit_replica_writes_hit_default() {
    let default = make_pool("sqlite::memory:").await;
    let replica = make_pool("sqlite::memory:").await;

    umbral::App::builder()
        .settings(umbral::settings::Settings::default())
        .database("default", default)
        .database("replica", replica)
        .router(SplitRouter)
        .model::<Widget>()
        .build()
        .await
        .unwrap();

    // A write goes to "default".
    Widget::objects()
        .create(Widget { id: 0, name: "a".into() })
        .await
        .unwrap();
    assert_eq!(WRITES.load(Ordering::SeqCst), 1);

    // A read goes to "replica" — which is a *separate* empty pool, so the
    // write above is invisible to the read. That divergence proves the split.
    let rows = Widget::objects().fetch().await.unwrap();
    assert!(READS.load(Ordering::SeqCst) >= 1);
    assert_eq!(rows.len(), 0, "read routed to the empty replica, not default");
}
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cd crates && cargo test -p umbral-core --test router_read_write_split`
Expected: FAIL — today `resolve_pool` ignores the router, so the read hits `default` and returns 1 row (or `WRITES`/`READS` stay 0).

- [ ] **Step 4: Refactor `resolve_pool`** — replace the function at `mod.rs:1116-1124`:

```rust
fn resolve_pool<T: Model>(explicit: Option<DbPool>, op: crate::db::RouteOp) -> DbPool {
    if let Some(pool) = explicit {
        return pool;
    }
    // Route through the swappable router when the registry is up.
    if let Some(meta) = crate::migrate::model_meta_ref(T::NAME) {
        let ctx = crate::db::route_context::current();
        let r = crate::db::router::router();
        let alias = match op {
            crate::db::RouteOp::Read => r.db_for_read(meta, &ctx),
            crate::db::RouteOp::Write => r.db_for_write(meta, &ctx),
        };
        return crate::db::pool_for_dispatched(alias.as_str()).clone();
    }
    // Registry-less fallback (low-level tests): today's static behavior.
    if let Some(alias) = crate::migrate::model_alias(T::NAME) {
        return crate::db::pool_for_dispatched(&alias).clone();
    }
    crate::db::pool_dispatched().clone()
}
```

- [ ] **Step 5: Thread `RouteOp` through every call site.** Every `resolve_pool::<T>(self.explicit_pool.clone())` and `resolve_pool::<T>(None)` now takes a second arg. Read terminals pass `crate::db::RouteOp::Read`; write terminals pass `RouteOp::Write`. The sites (from the agent's audit):
  - **Reads** (`RouteOp::Read`): `fetch` (1530, and its two hydration re-resolves at 1595/1599), plus `first`, `count`, `exists`, `get`, `aggregate`, `values`, `in_bulk`, `explain`, and the `*_pg` mirrors — every terminal that issues a SELECT. Search the file for `resolve_pool::<T>(` and classify by whether the terminal reads or writes.
  - **Writes** (`RouteOp::Write`): `delete` (2815), `update_values`/`update_expr`, `create` (3671), `bulk_create` (3789), `get_or_create` (4028), `update_or_create` (4199/4238), `upsert`, `bulk_update`, and the `Manager`-direct sites (4449, 4608). These are the `resolve_pool::<T>(None)` calls — change to `resolve_pool::<T>(None, RouteOp::Write)`.

  Add `use crate::db::RouteOp;` to the file's imports to shorten the calls. Mechanical; the compiler enumerates every missed site (each becomes a "this function takes 2 arguments" error until updated).

- [ ] **Step 6: Run the new test + the full non-regression suite**

Run: `cd crates && cargo test -p umbral-core --test router_read_write_split`
Expected: PASS.
Run: `cd crates && cargo test -p umbral-core`
Expected: 935+ pass, 0 fail (DefaultRouter reproduces old behavior; `model_meta_ref` is `None` in registry-less unit tests → legacy fallback).

- [ ] **Step 7: Commit**

```bash
cd .
git add crates/umbral-core/src/orm/queryset/mod.rs crates/umbral-core/src/migrate.rs crates/umbral-core/tests/router_read_write_split.rs
git commit -m "feat(orm): route typed reads/writes through DatabaseRouter (folds in #23)"
```

---

### Task 6: `RouteContextLayer` middleware + `App::builder().route_context()`

**Files:**
- Modify: `crates/umbral-core/src/db/route_context.rs` (add the layer)
- Modify: `crates/umbral-core/src/app.rs` (builder method + install in the middleware stack)
- Test: `crates/umbral-core/tests/route_context.rs`

- [ ] **Step 1: Add `RouteContextLayer` to `route_context.rs`** — append:

```rust
use crate::middleware::Middleware;
use crate::web::{Request, Response};

type Resolver = Arc<dyn Fn(&Request) -> RouteContext + Send + Sync>;

/// Middleware that builds a [`RouteContext`] from each request (via an
/// app-supplied resolver) and runs the downstream request inside
/// [`scope`]. Installed by `App::builder().route_context(...)`.
pub struct RouteContextLayer {
    resolver: Resolver,
}

impl RouteContextLayer {
    pub fn new<F>(resolver: F) -> Self
    where
        F: Fn(&Request) -> RouteContext + Send + Sync + 'static,
    {
        Self { resolver: Arc::new(resolver) }
    }
}

#[crate::async_trait]
impl Middleware for RouteContextLayer {
    fn name(&self) -> &'static str {
        "RouteContextLayer"
    }
    async fn before_request(&self, req: Request) -> Result<Request, Response> {
        // The context is established for the request future via the stack
        // driver's call into the handler. Because `Middleware` hands the
        // request back rather than wrapping the downstream future, store the
        // resolved context on the request's extensions and let the seam read
        // it. See note below.
        let ctx = (self.resolver)(&req);
        let mut req = req;
        req.extensions_mut().insert(Arc::new(ctx));
        Ok(req)
    }
}
```

**Design note for the implementer:** the `Middleware` contract (`before_request(req) -> req`) does not wrap the downstream future, so it cannot itself `scope(...)` the handler. Two correct options — pick the one that fits the #68 stack driver (`middleware.rs:128-174`):
  1. **Preferred:** add a `scope`-aware path in the stack driver `run_stack` (`middleware.rs`) — after `before_request`s run, if the request extensions carry an `Arc<RouteContext>`, run `next.run(req)` inside `route_context::scope(ctx, ...)`. This keeps the task-local active for the whole handler + its `.await`s.
  2. If modifying the driver is undesirable, expose `RouteContextLayer` as a dedicated `tower::Layer` (not the `Middleware` trait) that wraps the inner service future in `scope(...)`, and install it directly in `assemble_router` rather than via the middleware stack.

The plan assumes option 1. Add to `run_stack` (after the `before_request` loop, around `next.run`):
```rust
    // Establish the request-scoped routing context for the handler + its awaits.
    if let Some(ctx) = req.extensions().get::<std::sync::Arc<crate::db::RouteContext>>().cloned() {
        let res = crate::db::route_context::scope((*ctx).clone(), next.run(req)).await;
        // ... existing after_response unwind over `res` ...
        return /* unwound */ res;
    }
```
Integrate with the existing after-response unwind rather than duplicating it; the key change is wrapping `next.run(req)` in `scope`.

- [ ] **Step 2: Builder method** — in `app.rs`, add a field `route_context_resolver: Option<crate::db::route_context::Resolver>` (or store the boxed closure), an init `None`, and:
```rust
    pub fn route_context<F>(mut self, resolver: F) -> Self
    where
        F: Fn(&crate::web::Request) -> crate::db::RouteContext + Send + Sync + 'static,
    {
        self.route_context_resolver = Some(std::sync::Arc::new(resolver));
        self
    }
```
In `build()`, when a resolver is present, push a `RouteContextLayer::new(resolver)` onto the middleware stack **first** (so the context is established before any other middleware/handler runs): near where the middleware stack is assembled, prepend it.

- [ ] **Step 3: Write the failing test** — `crates/umbral-core/tests/route_context.rs`: a router that records the `ctx.tenant()` it sees; a `route_context` resolver that reads a `X-Tenant` header; a request through the assembled router asserts the router saw the tenant. (Use the existing test harness pattern from `crates/umbral-core/tests/` that drives a built `App`'s router with `tower::ServiceExt::oneshot`; mirror an existing middleware test such as `middleware_pipeline.rs` for the request-dispatch scaffolding.)

- [ ] **Step 4: Run → fail → implement → pass.**
Run: `cd crates && cargo test -p umbral-core --test route_context`

- [ ] **Step 5: Commit**

```bash
cd .
git add crates/umbral-core/src/db/route_context.rs crates/umbral-core/src/middleware.rs crates/umbral-core/src/app.rs crates/umbral-core/tests/route_context.rs
git commit -m "feat(app): RouteContextLayer middleware + App::builder().route_context()"
```

---

### Task 7: Dynamic-path routing (`DynQuerySet`)

**Files:**
- Modify: `crates/umbral-core/src/orm/dynamic.rs`
- Test: `crates/umbral-core/tests/router_dynamic.rs`

The dynamic path calls bare `pool_dispatched()` at ~12 terminals and is pinned to `"default"`. Route it through the router keyed on `self.meta`.

- [ ] **Step 1: Add a dynamic resolver** near the top of `dynamic.rs` (after the imports):

```rust
/// Resolve the pool for a dynamic (late-bound) query on `meta`, routing
/// through the `DatabaseRouter` exactly like the typed path.
fn resolve_pool_dyn(meta: &crate::migrate::ModelMeta, op: crate::db::RouteOp) -> crate::db::DbPool {
    let ctx = crate::db::route_context::current();
    let r = crate::db::router::router();
    let alias = match op {
        crate::db::RouteOp::Read => r.db_for_read(meta, &ctx),
        crate::db::RouteOp::Write => r.db_for_write(meta, &ctx),
    };
    crate::db::pool_for_dispatched(alias.as_str()).clone()
}
```

(The dynamic path always runs post-`App::build` with a live registry, so no registry-less fallback is needed — but `DefaultRouter` still yields the model's static alias, fixing the pre-existing bug where `self.meta.database` was ignored.)

- [ ] **Step 2: Write the failing test** — `crates/umbral-core/tests/router_dynamic.rs`: register a model with `#[umbral(database = "analytics")]`, build with `default` + `analytics` pools, insert a row into the `analytics` pool directly, then read it via `DynQuerySet::for_meta(&meta).count()` — assert it sees the row (today it reads `default` and sees 0). Use `umbral::migrate::registered_models()` to get the `&ModelMeta` after build.

- [ ] **Step 3: Run → fail.** `cd crates && cargo test -p umbral-core --test router_dynamic` → FAIL (counts 0 against `default`).

- [ ] **Step 4: Replace `pool_dispatched()` at every `DynQuerySet` terminal** with `resolve_pool_dyn(self.meta, RouteOp::Read|Write)`. Read terminals (`count` 569, `fetch_distinct_strings` 603, `fetch_as_strings` 952, `fetch_as_json` 1038, `first_as_json`, the `_json` readers) → `RouteOp::Read`. Write terminals (`delete` 654, soft-delete 694, `update_*` 738/813, `insert*` 893/1159, m2m 2517) → `RouteOp::Write`. For the `*_in_tx` variants that already take an explicit pool/tx, leave them as-is (the caller chose the connection). Add `use crate::db::RouteOp;`.

- [ ] **Step 5: Run → pass + non-regression.**
Run: `cd crates && cargo test -p umbral-core --test router_dynamic` → PASS.
Run: `cd crates && cargo test -p umbral-core` → green.

- [ ] **Step 6: Commit**

```bash
cd .
git add crates/umbral-core/src/orm/dynamic.rs crates/umbral-core/tests/router_dynamic.rs
git commit -m "fix(orm): route DynQuerySet through DatabaseRouter (admin/REST no longer pinned to default)"
```

---

### Task 8: `allow_relation` — route the cross-DB FK guard through the router

**Files:**
- Modify: `crates/umbral-core/src/app.rs` (Phase 2.5b, lines 687-704)
- Test: `crates/umbral-core/tests/router_allow.rs` (the relation half)

- [ ] **Step 1: Write the failing test** — `router_allow.rs`: a router whose `allow_relation` returns `false` for a specific (model, target) pair, asserting `App::build` rejects an otherwise-legal same-DB FK with `BuildError::CrossDatabaseForeignKey` (or a new `BuildError::RelationNotAllowed` — see step 3). Also assert the default still allows same-DB relations (non-regression).

- [ ] **Step 2: Run → fail** (today the guard is a hardcoded `target_db != model_db`).

- [ ] **Step 3: Route the guard through the router** — in `app.rs` Phase 2.5b, the FK loop currently compares aliases directly (lines 687-704). The router is not yet installed at Phase 2.5b (install happens at Phase 3). Two options:
  1. Build the candidate router early: resolve `self.router` (or `DefaultRouter`) into a local `let router = self.router.clone().unwrap_or_else(|| Arc::new(DefaultRouter));` at the top of Phase 2.5b and call `router.allow_relation(model_meta, target_meta)` instead of the inline `target_db != model_db`. This requires `&ModelMeta` for both sides — already available in the walk (`model` and the target's meta via `all_models`/`table_alias`). The default `allow_relation` reproduces the alias-equality check, so behavior is unchanged. Keep `BuildError::CrossDatabaseForeignKey` for the default-router case; a custom veto can surface the same error (the message is about relations being disallowed).

  Replace the `if target_db != model_db { return Err(... CrossDatabaseForeignKey ...) }` body with: look up `target_meta` (`ModelMeta` for `target_table`), and `if field.db_constraint && !router.allow_relation(&model, &target_meta) { return Err(...) }`.

- [ ] **Step 4: Run → pass + non-regression** (`cross_db_fk.rs` and `multi_database.rs` still green — the default router preserves #22).

- [ ] **Step 5: Commit**

```bash
cd .
git add crates/umbral-core/src/app.rs crates/umbral-core/tests/router_allow.rs
git commit -m "refactor(app): cross-DB FK guard (#22) routes through DatabaseRouter::allow_relation"
```

---

### Task 9: `allow_migrate` — gate the migrate per-alias walk

**Files:**
- Modify: `crates/umbral-core/src/migrate.rs` (`op_targets_alias`, lines 1341-1343)
- Test: `crates/umbral-core/tests/router_allow.rs` (the migrate half)

- [ ] **Step 1: Write the failing test** — extend `router_allow.rs`: a router whose `allow_migrate(alias, model)` returns `false` for a given model on `"default"`; build + `migrate::make`/`run`; assert that model's table is **not** created on `default`. Default-router non-regression: a normal model still migrates.

- [ ] **Step 2: Run → fail.**

- [ ] **Step 3: Gate the walk** — `op_targets_alias` (migrate.rs:1341) currently returns `table_alias(op.table_name()) == alias`. Extend it to also consult the router:

```rust
fn op_targets_alias(op: &Operation, alias: &str) -> bool {
    if table_alias(op.table_name()) != alias {
        return false;
    }
    // Let the router veto migrating this table on this alias.
    match model_meta_for_table(op.table_name()) {
        Some(meta) => crate::db::router::router().allow_migrate(alias, &meta),
        None => true, // junction / unowned table → migrate on its alias
    }
}
```

Use the cached `model_meta_for_table` (or `model_meta_ref` keyed by table — add a by-table variant mirroring Task 5 if one doesn't exist). The default `allow_migrate` returns `true` for the model's own alias, so behavior is unchanged.

- [ ] **Step 4: Run → pass + non-regression** (`migration_safety.rs`, `migrate_drift.rs`, `multi_database.rs` green).

- [ ] **Step 5: Commit**

```bash
cd .
git add crates/umbral-core/src/migrate.rs crates/umbral-core/tests/router_allow.rs
git commit -m "feat(migrate): per-alias op walk routes through DatabaseRouter::allow_migrate"
```

---

### Task 10: `schema_for` consumer — schema-qualified table references (option C)

**Files:**
- Modify: `crates/umbral-core/src/orm/queryset/mod.rs` (typed table refs)
- Modify: `crates/umbral-core/src/orm/dynamic.rs` (dynamic table refs)
- Create helper: in `crates/umbral-core/src/db/router.rs`
- Test: `crates/umbral-core/tests/router_schema_qualified.rs` (SQL text), `crates/umbral-core/tests/router_schema_postgres.rs` (live isolation, `#[ignore]`)

- [ ] **Step 1: Add the schema-aware table-ref helper** to `db/router.rs`:

```rust
/// Build a sea-query table reference, schema-qualified when the active
/// router yields a schema for the current request. SQLite has no schemas, so
/// the caller only invokes the qualified form on Postgres builds; on SQLite,
/// or when `schema_for` is None, this returns the bare table.
pub fn schema_qualified_table(table: &str) -> sea_query::TableRef {
    use sea_query::{Alias as SqAlias, IntoTableRef};
    let ctx = crate::db::route_context::current();
    match router().schema_for(&ctx) {
        Some(schema) => (SqAlias::new(schema.as_str()), SqAlias::new(table)).into_table_ref(),
        None => SqAlias::new(table).into_table_ref(),
    }
}
```

(Verify the exact sea-query 0.32 API for a schema-qualified `TableRef`: the `(schema, table)` tuple implements `IntoTableRef` as `TableRef::SchemaTable`. If the tuple form isn't available in 0.32, construct `TableRef::SchemaTable(SeaRc::new(SqAlias::new(schema.as_str())), SeaRc::new(SqAlias::new(table)))` directly.)

- [ ] **Step 2: Write the failing SQL-text test** — `router_schema_qualified.rs`: install a router whose `schema_for` returns `Schema::new("tenant_7")`; build a typed query's SQL (use the existing `to_sql`/`explain`-style accessor, or `Manager::queryset().filter(...).to_sql("postgres")` if exposed) and assert the SQL contains `"tenant_7"."rw_widget"`. Add a SQLite assertion that the bare table is used (no schema). If no public `to_sql` accessor exists for assertion, route the test through `DynQuerySet` + a SQL-capturing path, or expose a `pub(crate)` test helper.

- [ ] **Step 3: Run → fail** (table is unqualified today).

- [ ] **Step 4: Route table positions through the helper.** Replace `Alias::new(<table>)` with `crate::db::router::schema_qualified_table(<table>)` at every **table-position** (not column) site:
  - Typed FROM: `Manager::queryset()` `.from(Alias::new(T::TABLE))` (mod.rs:3326).
  - Typed writes: `.table(Alias::new(T::TABLE))` (2935/3124/3219/4165/4478/4592) and `.from_table(Alias::new(T::TABLE))` (3104/4603).
  - Annotation/correlated subqueries in `build_query_for` that do `.from(Alias::new(child_table))`.
  - Dynamic: `q.from(Alias::new(&self.meta.table))` at every `DynQuerySet` terminal (dynamic.rs:1000, 561, etc.), plus M2M junction `from`s if they reference a model table.
  Leave **column** `Alias::new(col)` untouched. Guard cost: `schema_qualified_table` calls `current()`+`router().schema_for()`; for the common `None` case it returns the bare table — same SQL as today (the non-regression suite asserts byte-identical output).

- [ ] **Step 5: Run → pass + non-regression.**
Run: `cd crates && cargo test -p umbral-core --test router_schema_qualified` → PASS.
Run: `cd crates && cargo test -p umbral-core` → green (DefaultRouter `schema_for` is None → bare tables → identical SQL).

- [ ] **Step 6: Write the Postgres isolation test** — `router_schema_postgres.rs`, `#[ignore]` + `UMBRAL_TEST_POSTGRES_URL` (mirror `pk_uuid_postgres.rs`): create schemas `t_a`, `t_b`, the same table in each; a router whose `schema_for` reads `ctx.tenant()` → `Schema`; under `route_context::scope(tenant=a)` insert+read sees only `t_a`'s rows; under `scope(tenant=b)` only `t_b`'s. Assert cross-tenant isolation.

- [ ] **Step 7: Run the Postgres test if a DB is available**

Run: `cd crates && UMBRAL_TEST_POSTGRES_URL=postgres://… cargo test -p umbral-core --test router_schema_postgres -- --ignored`
Expected: PASS (skip if no DB; note in the commit that it's gated).

- [ ] **Step 8: Commit**

```bash
cd .
git add crates/umbral-core/src/db/router.rs crates/umbral-core/src/orm/queryset/mod.rs crates/umbral-core/src/orm/dynamic.rs crates/umbral-core/tests/router_schema_qualified.rs crates/umbral-core/tests/router_schema_postgres.rs
git commit -m "feat(orm): schema-qualified SQL consumes DatabaseRouter::schema_for (option C)"
```

---

### Task 11: User docs page + final verification

**Files:**
- Create: `documentation/docs/v0.0.1/orm/database-router.mdx`
- Verify: whole workspace

- [ ] **Step 1: Write the doc page** (per `CLAUDE.md` "ship a feature, ship its doc page" — purpose, one example, link to spec):

```mdx
---
title: Database router
description: Swap how umbral chooses which database, schema, and read/write target a query uses.
sidebar_position: 6
---

# Database router

By default umbral routes each model to its assigned database (`#[umbral(database = "…")]`, `Plugin::database()`, else `"default"`). Install a `DatabaseRouter` to take over those decisions per request — read/write replica split, database-per-tenant, or schema-per-tenant.

<Callout type="info">
The default behavior is unchanged: with no router installed you get today's static per-model routing.
</Callout>

## Example: read/write split

```rust
use umbral::db::{Alias, DatabaseRouter, RouteContext};
use umbral::migrate::ModelMeta;

struct ReplicaRouter;
impl DatabaseRouter for ReplicaRouter {
    fn db_for_read(&self, _m: &ModelMeta, _c: &RouteContext) -> Alias { Alias::new("replica") }
    fn db_for_write(&self, _m: &ModelMeta, _c: &RouteContext) -> Alias { Alias::new("default") }
}

App::builder()
    .database("default", primary_pool)
    .database("replica", replica_pool)
    .router(ReplicaRouter)
    .build().await?;
```

For per-request (per-tenant) routing, populate the request context with `App::builder().route_context(|req| …)` and read `ctx.tenant()` in your router. `schema_for` returns a Postgres schema to scope a request's queries to (schema-per-tenant), applied with zero extra round-trips via SQL-level qualification.

See the design spec: `docs/superpowers/specs/2026-06-16-database-router-foundation-design.md`.
```

- [ ] **Step 2: Full workspace verification**

Run, from `crates/`:
```bash
cargo fmt
cargo clippy --all-targets
cargo build
cargo test -p umbral-core
cargo test -p umbral-rest -p umbral-admin   # ORM-dependent plugins
```
Expected: clean fmt, no new clippy warnings on the changed files, green build, green tests. (If the disk fills mid-build: `rm -rf crates/target/debug/incremental` and retry per-crate.)

- [ ] **Step 3: Commit**

```bash
cd .
git add documentation/docs/v0.0.1/orm/database-router.mdx
git commit -m "docs(orm): database-router page (gaps2 #69 foundation)"
```

- [ ] **Step 4: Close the gap** — append to `planning/gaps2.md` under #69 a one-line note that the foundation shipped (trait + context + read/write split + schema qualification), with Phase 2 (tenant management) still open. Do NOT renumber any entries. Commit separately: `docs(planning): #69 foundation shipped; Phase 2 (tenant mgmt) open`.

---

## Self-Review

**Spec coverage:** trait (Task 3) ✓; default router byte-for-byte (Tasks 3+5, non-regression bar) ✓; `RouteContext` + task-local + spawn-safe fallback (Task 2) ✓; `RouteContextLayer` + `.route_context()` (Task 6) ✓; read/write split #23 (Task 5) ✓; `.on()` hard override preserved (Task 5, `explicit` short-circuit kept) ✓; schema_for via option C SQL qualification (Task 10) ✓; SQLite no-op + quoted identifier (Task 10) ✓; `allow_relation` = #22 refactor (Task 8) ✓; `allow_migrate` gates per-alias walk (Task 9) ✓; dynamic-path routing (Task 7, the agent's finding #1) ✓; facade/prelude (Task 4) ✓; docs page (Task 11) ✓. Non-goals (Tenant model, migrate_schemas, SHARED_APPS) are correctly absent.

**Placeholder scan:** the two implementer-decision points (Task 6 middleware-vs-Layer for `scope`-wrapping the handler; Task 10 exact sea-query `TableRef` API) are called out with concrete code for both branches and a recommended choice — not "TODO"s. Task 5/7/10's mechanical multi-site edits list the exact sites and show the one transformation pattern.

**Type consistency:** `Alias`/`Schema`/`RouteOp`/`RouteContext`/`TenantKey`/`DatabaseRouter`/`DefaultRouter` names are used identically across tasks; `resolve_pool` gains a `RouteOp` arg consistently in Tasks 5 and referenced in 7/10; `model_meta_ref` (by-name, Task 5) and `model_meta_for_table` (by-table, Task 9) are both flagged as "add if the perf cache doesn't already provide it."

**Known risks carried into execution:** the disk-space fragility (mitigation noted in every build step); the Task 6 `scope`-wrapping seam is the subtlest piece (it must keep the task-local alive across the handler's `.await`s — verify with the Task 6 test that a deep ORM call inside a handler sees the tenant).

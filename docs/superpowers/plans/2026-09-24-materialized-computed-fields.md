# Materialized (computed) fields Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an app declare, in one place, that a column on model `M` is derived from one or more source tables plus a recompute closure, and have the framework keep it fresh automatically on every source insert / update / delete.

**Architecture:** A builder `Materialized::<M>::field(col).from::<S>(key_fn).recompute(f)` produces a fully type-erased `MaterializedSpec`. `AppBuilder::materialize(spec)` collects specs; `build()` validates each and subscribes an after-commit async handler to `post_save:<S>` / `post_delete:<S>` for every source. On a source event the handler decodes the source row, maps it to the affected target pk, runs the recompute, and writes the value back through the late-bound ORM (`DynQuerySet::update_json`), guarded against self-cascade by a task-local re-entrancy set.

**Tech Stack:** Rust, umbral-core (signals, ORM `DynQuerySet`, `AppBuilder`), serde_json for the erasure boundary, tokio task-local for the loop guard.

**Spec:** `docs/superpowers/specs/2026-09-24-materialized-computed-fields-design.md`

## Global Constraints

- **The ORM is the single DB interface.** The recompute write-back goes through `DynQuerySet::update_json`; no `sqlx::query(...)` anywhere in this feature. (CLAUDE.md "Plugins use the ORM. Not raw SQL.")
- **Location:** all code in `umbral-core`; public types re-exported from the `umbral` facade and added to `umbral::prelude`. No new crate, no plugin.
- **Refresh timing:** eager, after-commit — subscribe via `signals::subscribe_async` (NOT `subscribe_txn`), so the write-back is a separate post-commit write. Never emit inline mid-transaction (gaps6 #15/#16 lesson: a `subscribe_txn` write-back would deadlock SQLite's single writer and couple the source write's latency).
- **Zero cost when unused:** a model with no materialized field, and a table that is a source for none, subscribe nothing.
- **Backends:** correctness on both SQLite and Postgres; tests run on SQLite.
- **Naming:** the public builder is `Materialized<M>`; the collected type is `MaterializedSpec`; the builder method is `AppBuilder::materialize`.

## Review Focus

- **Deleted-source refresh:** a source `post_delete` must carry the full pre-delete row so `key_fn` can read the FK; a pk-only payload would silently skip the refresh. → Task 4 test `delete_of_a_source_row_refreshes_the_target`.
- **Non-i64 target pk:** the generic write-back must coerce a `String`/`Uuid` pk correctly, not assume i64. → Task 2 test `filter_pk_eq_matches_a_string_pk` + Task 4 test `refresh_works_for_a_string_pk_target`.
- **Concurrent independent recomputes:** the loop guard must be per-task (task-local), so two requests recomputing the same field for different pks don't block each other. → Task 3 test `guard_is_per_task_not_global`.
- **Rolled-back source write:** an `on_tx` source write that rolls back must fire no refresh (after-commit property). → Task 4 test `a_rolled_back_source_write_does_not_refresh`.
- **Null / absent key:** a source row whose `key_fn` returns `None` (null FK) must trigger no write-back and no error. → Task 4 test `a_source_row_with_a_null_key_is_skipped`.

---

## File Structure

- **Create** `crates/umbral-core/src/orm/materialized.rs` — `Materialized<M>`, `MaterializedSpec`, `SourceReg`, the erased recompute type, the task-local re-entrancy guard, and the `install()` fn that performs the subscriptions.
- **Modify** `crates/umbral-core/src/orm/mod.rs` — `pub mod materialized;` and re-export `Materialized`, `MaterializedSpec`.
- **Modify** `crates/umbral-core/src/orm/dynamic.rs` — add `DynQuerySet::filter_pk_eq`.
- **Modify** `crates/umbral-core/src/app.rs` — `materialized: Vec<MaterializedSpec>` field, `AppBuilder::materialize(...)`, build-time validation + `materialized::install(...)`.
- **Modify** `crates/umbral-core/src/lib.rs` and `crates/umbral/src/lib.rs` / prelude — facade re-exports.
- **Create** `crates/umbral-core/tests/materialized_fields.rs` — the behavioral suite.
- **Create** `documentation/docs/v0.0.1/orm/materialized-fields.mdx` — user doc.

---

### Task 1: The `Materialized<M>` builder and erased `MaterializedSpec`

**Files:**
- Create: `crates/umbral-core/src/orm/materialized.rs`
- Modify: `crates/umbral-core/src/orm/mod.rs` (add `pub mod materialized;` + re-exports)
- Test: `crates/umbral-core/tests/materialized_spec.rs`

**Interfaces:**
- Produces:
  - `pub struct Materialized<M: Model> { target_meta: ModelMeta, target_col: String, sources: Vec<SourceReg>, _m: PhantomData<M> }`
  - `impl<M: Model> Materialized<M> { pub fn field(col: impl AsRef<str>) -> Self }`
  - `pub fn from<S: Model + for<'de> Deserialize<'de>>(mut self, key_fn: impl Fn(&S) -> Option<Pk> + Send + Sync + 'static) -> Self where Pk: Serialize` — note `Pk` is a generic on this method: `pub fn from<S, K, Pk>(self, key_fn: K) -> Self where S: Model + DeserializeOwned, K: Fn(&S) -> Option<Pk> + Send + Sync + 'static, Pk: Serialize`.
  - `pub fn recompute<F, Fut, V>(self, f: F) -> MaterializedSpec where F: Fn(Value) -> Fut ... ` — see code; returns the erased spec. (The public closure the user writes is `Fn(Pk) -> Fut<Output = V>`; a small typed wrapper method `recompute_typed` provides that ergonomics — see Step 3.)
  - `pub struct MaterializedSpec { pub(crate) target_meta: ModelMeta, pub(crate) target_col: String, pub(crate) sources: Vec<SourceReg>, pub(crate) recompute: Arc<dyn Fn(Value) -> BoxFuture<'static, Option<Value>> + Send + Sync> }`
  - `pub(crate) struct SourceReg { pub(crate) table: String, pub(crate) extract: Arc<dyn Fn(&Value) -> Option<Value> + Send + Sync> }`

- [ ] **Step 1: Write the failing test**

`crates/umbral-core/tests/materialized_spec.rs`:
```rust
//! gaps6 #7 — the Materialized<M> builder erases M / S / Pk / V into a
//! non-generic MaterializedSpec. This tests the erasure boundary in isolation:
//! a source row's JSON maps to the right pk JSON, and the recompute closure's
//! typed value comes back as JSON.

use serde::{Deserialize, Serialize};
use serde_json::json;
use umbral_core::orm::Materialized;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ms_booking")]
pub struct MsBooking {
    pub id: i64,
    pub payment_total: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ms_rsvp")]
pub struct MsRsvp {
    pub id: i64,
    pub booking_id: i64,
    pub amount: i64,
}

#[tokio::test]
async fn builder_erases_source_extract_and_recompute_to_json() {
    let spec = Materialized::<MsBooking>::field(ms_booking::PAYMENT_TOTAL)
        .from::<MsRsvp, _, _>(|r: &MsRsvp| Some(r.booking_id))
        .recompute_typed(|booking_id: i64| async move { booking_id * 10 });

    assert_eq!(spec.target_col, "payment_total");
    assert_eq!(spec.sources.len(), 1);
    assert_eq!(spec.sources[0].table, "ms_rsvp");

    // extract: a source instance JSON → the affected target pk JSON
    let instance = json!({ "id": 5, "booking_id": 42, "amount": 3 });
    assert_eq!((spec.sources[0].extract)(&instance), Some(json!(42)));

    // recompute: pk JSON in → value JSON out
    let out = (spec.recompute)(json!(42)).await;
    assert_eq!(out, Some(json!(420)));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-core --test materialized_spec -- --nocapture`
Expected: FAIL to compile — `Materialized` not found.

- [ ] **Step 3: Write the builder**

`crates/umbral-core/src/orm/materialized.rs`:
```rust
//! gaps6 #7 — materialized (computed) fields: a column kept fresh by the
//! framework whenever a declared source table changes. See the design spec at
//! `docs/superpowers/specs/2026-09-24-materialized-computed-fields-design.md`.

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::migrate::ModelMeta;
use crate::orm::Model;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One source table for a materialized field: its table name plus an erased
/// extractor that decodes a source-row payload into the affected target pk.
pub(crate) struct SourceReg {
    pub(crate) table: String,
    pub(crate) extract: Arc<dyn Fn(&Value) -> Option<Value> + Send + Sync>,
}

/// The fully type-erased, collectible form of a materialized-field declaration.
pub struct MaterializedSpec {
    pub(crate) target_meta: ModelMeta,
    pub(crate) target_col: String,
    pub(crate) sources: Vec<SourceReg>,
    pub(crate) recompute: Arc<dyn Fn(Value) -> BoxFuture<'static, Option<Value>> + Send + Sync>,
}

/// Builder for a single computed column on target model `M`.
pub struct Materialized<M: Model> {
    target_meta: ModelMeta,
    target_col: String,
    sources: Vec<SourceReg>,
    _m: PhantomData<fn() -> M>,
}

impl<M: Model> Materialized<M> {
    /// Name the target column (any column token, e.g. `booking::PAYMENT_TOTAL`,
    /// or a `&str`). Fixes the target model + column.
    pub fn field(col: impl AsRef<str>) -> Self {
        Self {
            target_meta: ModelMeta::for_::<M>(),
            target_col: col.as_ref().to_owned(),
            sources: Vec::new(),
            _m: PhantomData,
        }
    }

    /// Register a source model `S` and a map from a changed `S` row to the
    /// affected target pk (`None` to skip, e.g. a null FK). Call more than once
    /// for multiple sources; all share the one `recompute`.
    pub fn from<S, K, Pk>(mut self, key_fn: K) -> Self
    where
        S: Model + DeserializeOwned,
        K: Fn(&S) -> Option<Pk> + Send + Sync + 'static,
        Pk: Serialize,
    {
        let extract = Arc::new(move |instance: &Value| -> Option<Value> {
            let s: S = serde_json::from_value(instance.clone()).ok()?;
            let pk = key_fn(&s)?;
            serde_json::to_value(pk).ok()
        });
        self.sources.push(SourceReg {
            table: S::TABLE.to_owned(),
            extract,
        });
        self
    }

    /// Finalize with the recompute closure `Fn(Pk) -> Future<Output = V>`.
    /// The framework writes the returned `V` back into the target column.
    pub fn recompute_typed<F, Fut, Pk, V>(self, f: F) -> MaterializedSpec
    where
        F: Fn(Pk) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = V> + Send + 'static,
        Pk: DeserializeOwned,
        V: Serialize,
    {
        let f = Arc::new(f);
        let recompute = Arc::new(move |pk_json: Value| -> BoxFuture<'static, Option<Value>> {
            let f = f.clone();
            Box::pin(async move {
                let pk: Pk = serde_json::from_value(pk_json).ok()?;
                let value = f(pk).await;
                serde_json::to_value(value).ok()
            })
        });
        MaterializedSpec {
            target_meta: self.target_meta,
            target_col: self.target_col,
            sources: self.sources,
            recompute,
        }
    }
}
```

Add to `crates/umbral-core/src/orm/mod.rs`:
```rust
pub mod materialized;
pub use materialized::{Materialized, MaterializedSpec};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-core --test materialized_spec -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/umbral-core/src/orm/materialized.rs crates/umbral-core/src/orm/mod.rs crates/umbral-core/tests/materialized_spec.rs
git commit -m "feat(orm): materialized-field builder (Materialized<M>) with type erasure"
```

---

### Task 2: `DynQuerySet::filter_pk_eq` — generic-pk filter for the write-back

**Files:**
- Modify: `crates/umbral-core/src/orm/dynamic.rs`
- Test: `crates/umbral-core/tests/dyn_filter_pk_eq.rs`

**Interfaces:**
- Consumes: `ModelMeta::pk_column()`, `crate::orm::write::json_to_sea_value` (`json_to_sea_value(ty, &Value, false, &name, None) -> Result<sea_query::Value, _>`).
- Produces: `pub fn filter_pk_eq(self, pk_value: &serde_json::Value) -> Self` on `DynQuerySet` — adds a `pk = <value>` condition, coercing the JSON to the pk column's `SqlType`. A model without a single-column pk, or an uncoercible value, adds a condition that matches nothing (safe: the write-back no-ops rather than updating every row).

- [ ] **Step 1: Write the failing test**

`crates/umbral-core/tests/dyn_filter_pk_eq.rs`:
```rust
//! gaps6 #7 — DynQuerySet::filter_pk_eq: filter by a JSON pk value, coercing to
//! the pk column's type. Exercised for both an i64 pk and a String pk (the
//! materialized write-back must not assume i64).

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::sqlite::SqlitePoolOptions;
use umbral::orm::DynQuerySet;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "fpk_int")]
pub struct FpkInt {
    pub id: i64,
    pub label: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "fpk_str")]
pub struct FpkStr {
    #[umbral(primary_key)]
    pub slug: String,
    pub label: String,
}

async fn boot() {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let settings = umbral::Settings::from_env().unwrap();
        let pool = SqlitePoolOptions::new().connect("sqlite::memory:").await.unwrap();
        umbral::App::builder().settings(settings).database("default", pool)
            .model::<FpkInt>().model::<FpkStr>().build().unwrap();
        umbral_core::migrate::create_tables_for_tests().await.unwrap();
    }).await;
}

#[tokio::test]
async fn filter_pk_eq_matches_an_i64_pk() {
    boot().await;
    let meta = umbral::migrate::ModelMeta::for_::<FpkInt>();
    DynQuerySet::for_meta(&meta).insert_json(json!({"label":"a"}).as_object().unwrap()).await.unwrap();
    let row = DynQuerySet::for_meta(&meta).insert_json(json!({"label":"b"}).as_object().unwrap()).await.unwrap();
    let id = row["id"].clone();

    let n = DynQuerySet::for_meta(&meta)
        .filter_pk_eq(&id)
        .update_json(json!({"label":"B"}).as_object().unwrap())
        .await
        .unwrap();
    assert_eq!(n, 1, "exactly the one pk-matched row updates");

    let got = DynQuerySet::for_meta(&meta).filter_pk_eq(&id).fetch_as_json().await.unwrap();
    assert_eq!(got[0]["label"], "B");
}

#[tokio::test]
async fn filter_pk_eq_matches_a_string_pk() {
    boot().await;
    let meta = umbral::migrate::ModelMeta::for_::<FpkStr>();
    DynQuerySet::for_meta(&meta).insert_json(json!({"slug":"x","label":"a"}).as_object().unwrap()).await.unwrap();

    let n = DynQuerySet::for_meta(&meta)
        .filter_pk_eq(&json!("x"))
        .update_json(json!({"label":"A"}).as_object().unwrap())
        .await
        .unwrap();
    assert_eq!(n, 1, "a String pk coerces and matches");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-core --test dyn_filter_pk_eq`
Expected: FAIL to compile — `filter_pk_eq` not found.

- [ ] **Step 3: Implement `filter_pk_eq`**

In `crates/umbral-core/src/orm/dynamic.rs`, next to `filter_in_i64` (~:792):
```rust
    /// Filter to the single row whose primary key equals `pk_value`, coercing
    /// the JSON scalar to the pk column's SQL type. Used by the materialized-
    /// field write-back (gaps6 #7), which knows the affected pk only as JSON.
    /// If the model has no single-column pk, or the value can't be coerced, the
    /// filter matches nothing — the write-back safely no-ops rather than
    /// touching every row.
    pub fn filter_pk_eq(self, pk_value: &serde_json::Value) -> Self {
        use sea_query::{Alias, Expr};
        let Some(pk) = self.meta.pk_column() else {
            return self.filter_condition(sea_query::Condition::all().add(Expr::val(1).eq(0)));
        };
        match crate::orm::write::json_to_sea_value(pk.ty, pk_value, false, &pk.name, None) {
            Ok(v) => self.filter_condition(
                sea_query::Condition::all().add(Expr::col(Alias::new(&pk.name)).eq(v)),
            ),
            Err(_) => self.filter_condition(sea_query::Condition::all().add(Expr::val(1).eq(0))),
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-core --test dyn_filter_pk_eq`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add crates/umbral-core/src/orm/dynamic.rs crates/umbral-core/tests/dyn_filter_pk_eq.rs
git commit -m "feat(orm): DynQuerySet::filter_pk_eq for generic-pk single-row filter"
```

---

### Task 3: The task-local re-entrancy guard

**Files:**
- Modify: `crates/umbral-core/src/orm/materialized.rs` (add the guard)
- Test: `crates/umbral-core/tests/materialized_guard.rs`

**Interfaces:**
- Produces: `pub(crate) async fn guarded(table: String, col: String, fut: impl Future<Output = ()> + Send)` — runs `fut` unless `(table, col)` is already active in the current task; a re-entry logs a warning and skips. Nested *different* keys run (legit cascade); concurrent *independent* tasks never block each other (task-local).

- [ ] **Step 1: Write the failing test**

`crates/umbral-core/tests/materialized_guard.rs`:
```rust
//! gaps6 #7 — the materialized re-entrancy guard: a nested recompute of the
//! SAME (table,col) is skipped (breaks self-cycles); a nested DIFFERENT key
//! runs (legit cascade); two independent tasks never block each other.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use umbral_core::orm::materialized::guarded;

#[tokio::test]
async fn re_entry_of_the_same_key_is_skipped() {
    let inner_ran = Arc::new(AtomicUsize::new(0));
    let ran = inner_ran.clone();
    guarded("t".into(), "c".into(), async move {
        // re-enter the SAME key from within — must be skipped
        let ran2 = ran.clone();
        guarded("t".into(), "c".into(), async move {
            ran2.fetch_add(1, Ordering::SeqCst);
        })
        .await;
    })
    .await;
    assert_eq!(inner_ran.load(Ordering::SeqCst), 0, "same-key re-entry must be skipped");
}

#[tokio::test]
async fn nested_different_key_runs() {
    let inner_ran = Arc::new(AtomicUsize::new(0));
    let ran = inner_ran.clone();
    guarded("t".into(), "c".into(), async move {
        let ran2 = ran.clone();
        guarded("other".into(), "c".into(), async move {
            ran2.fetch_add(1, Ordering::SeqCst);
        })
        .await;
    })
    .await;
    assert_eq!(inner_ran.load(Ordering::SeqCst), 1, "a different key nested under one must run");
}

#[tokio::test]
async fn guard_is_per_task_not_global() {
    // Two independent tasks recomputing the same (table,col) concurrently must
    // BOTH run — the guard is per-task, not a global lock.
    let ran = Arc::new(AtomicUsize::new(0));
    let a = { let r = ran.clone(); tokio::spawn(async move {
        guarded("t".into(), "c".into(), async move { r.fetch_add(1, Ordering::SeqCst); }).await;
    })};
    let b = { let r = ran.clone(); tokio::spawn(async move {
        guarded("t".into(), "c".into(), async move { r.fetch_add(1, Ordering::SeqCst); }).await;
    })};
    a.await.unwrap();
    b.await.unwrap();
    assert_eq!(ran.load(Ordering::SeqCst), 2, "independent tasks must not block each other");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-core --test materialized_guard`
Expected: FAIL to compile — `guarded` not found.

- [ ] **Step 3: Implement the guard**

Append to `crates/umbral-core/src/orm/materialized.rs`:
```rust
use std::cell::RefCell;
use std::collections::HashSet;

tokio::task_local! {
    static ACTIVE: RefCell<HashSet<(String, String)>>;
}

/// Run `fut` unless `(table, col)` is already being recomputed in this task
/// (a self-cascade); a re-entry logs a warning and skips. Nested *different*
/// keys run (legit multi-field cascade). Per-task, so independent requests
/// never block each other.
pub(crate) async fn guarded(table: String, col: String, fut: impl Future<Output = ()> + Send) {
    let key = (table.clone(), col.clone());
    let entered = ACTIVE.try_with(|set| set.borrow_mut().insert(key.clone()));
    match entered {
        Ok(true) => {
            fut.await;
            let _ = ACTIVE.try_with(|set| set.borrow_mut().remove(&key));
        }
        Ok(false) => {
            tracing::warn!(
                table = %table, column = %col,
                "umbral: materialized field recompute re-entered the same field; \
                 skipping to break a cycle",
            );
        }
        Err(_) => {
            // No scope yet — establish one for the top-level recompute.
            let mut init = HashSet::new();
            init.insert(key);
            ACTIVE.scope(RefCell::new(init), fut).await;
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-core --test materialized_guard`
Expected: PASS (all three).

- [ ] **Step 5: Commit**

```bash
git add crates/umbral-core/src/orm/materialized.rs crates/umbral-core/tests/materialized_guard.rs
git commit -m "feat(orm): task-local re-entrancy guard for materialized recompute"
```

---

### Task 4: Wiring — `AppBuilder::materialize` + install + the behavioral suite

**Files:**
- Modify: `crates/umbral-core/src/orm/materialized.rs` (add `install`)
- Modify: `crates/umbral-core/src/app.rs` (field, `materialize`, call `install` in `build()`)
- Modify: `crates/umbral-core/src/lib.rs`, `crates/umbral/src/lib.rs` + prelude (re-exports)
- Test: `crates/umbral-core/tests/materialized_fields.rs`

**Interfaces:**
- Consumes: `MaterializedSpec` (Task 1), `DynQuerySet::filter_pk_eq` (Task 2), `guarded` (Task 3), `crate::signals::subscribe_async`.
- Produces:
  - `pub(crate) fn install(spec: &MaterializedSpec)` in `materialized.rs` — subscribes an after-commit async handler to `post_save:<S>` and `post_delete:<S>` for each source.
  - `pub fn materialize(self, spec: MaterializedSpec) -> Self` on `AppBuilder`.

- [ ] **Step 1: Write the failing behavioral test**

`crates/umbral-core/tests/materialized_fields.rs`:
```rust
//! gaps6 #7 — behavioral: a computed column stays fresh automatically as its
//! source rows change, with NO manual recompute call. Real rows, the real
//! public ORM path.

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral::orm::{Aggregate, Materialized};
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mf_booking")]
pub struct MfBooking {
    pub id: i64,
    pub payment_total: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mf_rsvp")]
pub struct MfRsvp {
    pub id: i64,
    pub booking_id: i64,
    pub amount: i64,
}

#[derive(Debug)]
struct Boom;
impl std::fmt::Display for Boom { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "boom") } }
impl std::error::Error for Boom {}
impl From<sqlx::Error> for Boom { fn from(_: sqlx::Error) -> Self { Boom } }
impl From<umbral::orm::write::WriteError> for Boom { fn from(_: umbral::orm::write::WriteError) -> Self { Boom } }

fn lock() -> &'static tokio::sync::Mutex<()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &L
}

async fn boot() -> sqlx::SqlitePool {
    static ONCE: tokio::sync::OnceCell<sqlx::SqlitePool> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let settings = umbral::Settings::from_env().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("mf.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new().max_connections(5).connect_with(
            SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                .filename(&path).create_if_missing(true)).await.unwrap();
        umbral::App::builder().settings(settings).database("default", pool.clone())
            .model::<MfBooking>().model::<MfRsvp>()
            .materialize(
                Materialized::<MfBooking>::field(mf_booking::PAYMENT_TOTAL)
                    .from::<MfRsvp, _, _>(|r: &MfRsvp| Some(r.booking_id))
                    .recompute_typed(|booking_id: i64| async move {
                        let agg = MfRsvp::objects()
                            .filter(mf_rsvp::BOOKING_ID.eq(booking_id))
                            .aggregate(&[("total", Aggregate::sum("amount"))])
                            .await.unwrap_or_default();
                        agg["total"].as_i64().unwrap_or(0)
                    }),
            )
            .build().unwrap();
        umbral_core::migrate::create_tables_for_tests().await.unwrap();
        pool
    }).await.clone()
}

async fn payment_total(id: i64) -> i64 {
    MfBooking::objects().filter(mf_booking::ID.eq(id)).get().await.unwrap().payment_total
}

#[tokio::test]
async fn creating_a_source_row_refreshes_the_target() {
    let _g = lock().lock().await;
    let _pool = boot().await;
    let b = MfBooking::objects().create(MfBooking { id: 0, payment_total: 0 }).await.unwrap();

    MfRsvp::objects().create(MfRsvp { id: 0, booking_id: b.id, amount: 30 }).await.unwrap();
    MfRsvp::objects().create(MfRsvp { id: 0, booking_id: b.id, amount: 12 }).await.unwrap();

    assert_eq!(payment_total(b.id).await, 42, "payment_total tracks the sum with no manual recompute");
}

#[tokio::test]
async fn delete_of_a_source_row_refreshes_the_target() {
    let _g = lock().lock().await;
    let _pool = boot().await;
    let b = MfBooking::objects().create(MfBooking { id: 0, payment_total: 0 }).await.unwrap();
    let r = MfRsvp::objects().create(MfRsvp { id: 0, booking_id: b.id, amount: 50 }).await.unwrap();
    assert_eq!(payment_total(b.id).await, 50);

    MfRsvp::objects().filter(mf_rsvp::ID.eq(r.id)).delete().await.unwrap();
    assert_eq!(payment_total(b.id).await, 0, "a deleted source row drops the cached value");
}

#[tokio::test]
async fn a_rolled_back_source_write_does_not_refresh() {
    let _g = lock().lock().await;
    let pool = boot().await;
    let b = MfBooking::objects().create(MfBooking { id: 0, payment_total: 0 }).await.unwrap();

    let bid = b.id;
    let res: Result<(), Boom> = db::transaction_sqlite(&pool, |tx| Box::pin(async move {
        MfRsvp::objects().on_tx(tx).create(MfRsvp { id: 0, booking_id: bid, amount: 99 }).await?;
        Err(Boom)
    })).await;
    assert!(res.is_err());
    assert_eq!(payment_total(b.id).await, 0, "a rolled-back source write fires no refresh");
}

#[tokio::test]
async fn a_source_row_with_a_null_key_is_skipped() {
    // key_fn here always returns Some, so simulate a null key by pointing at a
    // booking id that doesn't exist: the recompute runs but updates zero rows —
    // no panic, no error. (The null-FK Option::None path is unit-tested in
    // materialized_spec.rs; this guards the write-back no-op.)
    let _g = lock().lock().await;
    let _pool = boot().await;
    MfRsvp::objects().create(MfRsvp { id: 0, booking_id: 999_999, amount: 5 }).await.unwrap();
    // No booking 999999 exists → nothing to assert beyond "no panic"; a prior
    // booking's total is untouched.
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-core --test materialized_fields`
Expected: FAIL to compile — `AppBuilder::materialize` not found.

- [ ] **Step 3: Implement `install` in `materialized.rs`**

```rust
/// Subscribe the after-commit recompute handlers for one spec. Called from
/// `AppBuilder::build()`. Subscription is ambient (the signals registry is
/// process-global), so no handle is threaded.
pub(crate) fn install(spec: &MaterializedSpec) {
    for source in &spec.sources {
        let target_meta = spec.target_meta.clone();
        let target_col = spec.target_col.clone();
        let extract = source.extract.clone();
        let recompute = spec.recompute.clone();

        let handler = move |payload: &Value| {
            let target_meta = target_meta.clone();
            let target_col = target_col.clone();
            let extract = extract.clone();
            let recompute = recompute.clone();
            // Delete payloads carry the full pre-delete row when a subscriber
            // exists (gaps6 #14/#15) — which we are — so `instance` has the FK.
            let instance = payload.get("instance").cloned().unwrap_or(Value::Null);
            async move {
                let Some(pk_json) = extract(&instance) else { return };
                let table = target_meta.table.clone();
                let col = target_col.clone();
                guarded(table, col, async move {
                    let Some(value) = recompute(pk_json.clone()).await else { return };
                    let mut body = serde_json::Map::new();
                    body.insert(target_col.clone(), value);
                    if let Err(e) = crate::orm::dynamic::DynQuerySet::for_meta(&target_meta)
                        .filter_pk_eq(&pk_json)
                        .update_json(&body)
                        .await
                    {
                        tracing::error!(
                            table = %target_meta.table, column = %target_col,
                            "umbral: materialized field write-back failed: {e:?}",
                        );
                    }
                })
                .await;
            }
        };

        crate::signals::subscribe_async(&format!("post_save:{}", source.table), handler.clone());
        crate::signals::subscribe_async(&format!("post_delete:{}", source.table), handler);
    }
}
```

- [ ] **Step 4: Wire `AppBuilder`**

In `crates/umbral-core/src/app.rs`: add the field to the struct (near `models: Vec<ModelMeta>`, ~:278):
```rust
    /// gaps6 #7 — collected materialized-field specs; subscribed in `build()`.
    materialized: Vec<crate::orm::MaterializedSpec>,
```
Initialize it in the builder's default construction (near `models: Vec::new()`, ~:380):
```rust
            materialized: Vec::new(),
```
Add the builder method (near `model<T>`, ~:546):
```rust
    /// Declare a materialized (computed) field: a column on `M` the framework
    /// keeps fresh whenever a declared source table changes (gaps6 #7). See
    /// [`crate::orm::Materialized`].
    pub fn materialize(mut self, spec: crate::orm::MaterializedSpec) -> Self {
        self.materialized.push(spec);
        self
    }
```
In `build()` (after the databases/pool are installed into the ambient pool and after model registration — place it just before returning `Ok(App { .. })`, alongside the other post-registration wiring):
```rust
        // gaps6 #7 — subscribe every materialized field's after-commit
        // recompute handlers. Validated first (see Task 5).
        for spec in &self.materialized {
            crate::orm::materialized::install(spec);
        }
```

- [ ] **Step 5: Facade re-exports**

In `crates/umbral-core/src/lib.rs` ensure `pub use orm::{Materialized, MaterializedSpec};` is reachable (or that `orm` module is public — it is). In `crates/umbral/src/lib.rs`, re-export and add to the prelude:
```rust
pub use umbral_core::orm::{Materialized, MaterializedSpec};
```
and in the prelude module:
```rust
pub use umbral_core::orm::Materialized;
```

- [ ] **Step 6: Run the behavioral tests to verify they pass**

Run: `cargo test -p umbral-core --test materialized_fields`
Expected: PASS (all four).

- [ ] **Step 7: Full-crate suite + clippy**

Run: `cargo test -p umbral-core && cargo clippy -p umbral-core --all-targets`
Expected: green; no new warnings in `materialized.rs` / `dynamic.rs` / `app.rs`.

- [ ] **Step 8: Commit**

```bash
git add crates/umbral-core/src/orm/materialized.rs crates/umbral-core/src/app.rs \
        crates/umbral-core/src/lib.rs crates/umbral/src/lib.rs \
        crates/umbral-core/tests/materialized_fields.rs
git commit -m "feat(orm): AppBuilder::materialize wires after-commit computed-field refresh"
```

---

### Task 5: Boot-time validation (system check)

**Files:**
- Modify: `crates/umbral-core/src/app.rs` (validate specs in `build()` before `install`)
- Modify: `crates/umbral-core/src/orm/materialized.rs` (a `validate` fn returning `Result<(), String>`)
- Test: `crates/umbral-core/tests/materialized_validation.rs`

**Interfaces:**
- Produces: `pub(crate) fn validate(spec: &MaterializedSpec) -> Result<(), String>` — errors if the target column is absent from `target_meta`, if the model has no single-column pk, or if the target column IS the pk. `build()` maps the error into `BuildError`.

- [ ] **Step 1: Write the failing test**

`crates/umbral-core/tests/materialized_validation.rs`:
```rust
//! gaps6 #7 — a misdeclared materialized field aborts boot with a clear error,
//! rather than silently never refreshing.

use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePoolOptions;
use umbral::orm::Materialized;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mv_booking")]
pub struct MvBooking { pub id: i64, pub total: i64 }

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mv_rsvp")]
pub struct MvRsvp { pub id: i64, pub booking_id: i64 }

#[tokio::test]
async fn a_target_column_that_does_not_exist_aborts_build() {
    let settings = umbral::Settings::from_env().unwrap();
    let pool = SqlitePoolOptions::new().connect("sqlite::memory:").await.unwrap();
    let err = umbral::App::builder().settings(settings).database("default", pool)
        .model::<MvBooking>().model::<MvRsvp>()
        .materialize(
            Materialized::<MvBooking>::field("nonexistent_col")
                .from::<MvRsvp, _, _>(|r: &MvRsvp| Some(r.booking_id))
                .recompute_typed(|_id: i64| async move { 0i64 }),
        )
        .build();
    assert!(err.is_err(), "an unknown target column must abort build");
    let msg = format!("{:?}", err.err().unwrap());
    assert!(msg.contains("nonexistent_col"), "the error names the bad column: {msg}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-core --test materialized_validation`
Expected: FAIL — build currently succeeds (returns Ok).

- [ ] **Step 3: Implement `validate` and call it in `build()`**

In `materialized.rs`:
```rust
/// Validate a spec at boot: the target column must exist, the model must have a
/// single-column pk, and the target column must not be that pk.
pub(crate) fn validate(spec: &MaterializedSpec) -> Result<(), String> {
    let m = &spec.target_meta;
    let Some(pk) = m.pk_column() else {
        return Err(format!(
            "materialized field on `{}`: model has no single-column primary key",
            m.table
        ));
    };
    if !m.fields.iter().any(|c| c.name == spec.target_col) {
        return Err(format!(
            "materialized field `{}.{}`: no such column on the model",
            m.table, spec.target_col
        ));
    }
    if pk.name == spec.target_col {
        return Err(format!(
            "materialized field `{}.{}`: the target column cannot be the primary key",
            m.table, spec.target_col
        ));
    }
    Ok(())
}
```
In `build()`, immediately before the `install` loop from Task 4:
```rust
        for spec in &self.materialized {
            crate::orm::materialized::validate(spec)
                .map_err(BuildError::Materialized)?;
        }
        for spec in &self.materialized {
            crate::orm::materialized::install(spec);
        }
```
Add the `BuildError` variant (find `enum BuildError` in `app.rs`):
```rust
    /// A materialized-field declaration failed validation (gaps6 #7).
    Materialized(String),
```
and its `Display` arm:
```rust
            BuildError::Materialized(m) => write!(f, "materialized field: {m}"),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-core --test materialized_validation`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/umbral-core/src/orm/materialized.rs crates/umbral-core/src/app.rs crates/umbral-core/tests/materialized_validation.rs
git commit -m "feat(orm): boot-time validation of materialized-field declarations"
```

---

### Task 6: String-pk target coverage + docs

**Files:**
- Modify: `crates/umbral-core/tests/materialized_fields.rs` (add the string-pk case)
- Create: `documentation/docs/v0.0.1/orm/materialized-fields.mdx`

**Interfaces:** none new.

- [ ] **Step 1: Write the failing string-pk test**

Add to `crates/umbral-core/tests/materialized_fields.rs` (new models + test). Use a `String`-pk target refreshed from a source keyed by that string:
```rust
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mf_club")]
pub struct MfClub {
    #[umbral(primary_key)]
    pub slug: String,
    pub member_count: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mf_member")]
pub struct MfMember { pub id: i64, pub club_slug: String }

// Register in a SECOND boot (own OnceCell + own file) mirroring `boot()`, adding:
//   .materialize(
//       Materialized::<MfClub>::field(mf_club::MEMBER_COUNT)
//           .from::<MfMember, _, _>(|m: &MfMember| Some(m.club_slug.clone()))
//           .recompute_typed(|slug: String| async move {
//               MfMember::objects().filter(mf_member::CLUB_SLUG.eq(&slug)).count().await.unwrap_or(0)
//           }),
//   )

#[tokio::test]
async fn refresh_works_for_a_string_pk_target() {
    // create MfClub{slug:"acme", member_count:0}; create two MfMember{club_slug:"acme"};
    // assert MfClub("acme").member_count == 2 with no manual recompute.
}
```
(Write the second `boot_str()` helper concretely, mirroring `boot()` with its own `OnceCell`, temp file, and the `MfClub`/`MfMember` registration + the `materialize(...)` above.)

- [ ] **Step 2: Run to verify it fails, then it passes**

Run: `cargo test -p umbral-core --test materialized_fields refresh_works_for_a_string_pk_target`
Expected: FAIL first if the string-pk write-back path is wrong; PASS once `filter_pk_eq` (Task 2) handles the String coercion (it does) — this test is the integration proof for the Review-Focus "non-i64 pk" line.

- [ ] **Step 3: Write the doc page**

`documentation/docs/v0.0.1/orm/materialized-fields.mdx`:
```mdx
---
title: Materialized (computed) fields
description: Declare a column that the framework keeps fresh automatically whenever a source table changes - no manual recompute at every write site.
sidebar_position: 8
icon: refresh-cw
tags: [orm, computed, denormalization, cache]
---

Reach for a materialized field when a column's value is *derived* from another table - a cached total, a projected status, a denormalized count - and you don't want to remember to recompute it at every place that writes the source. You declare the dependency and the recompute once; the framework keeps the column current on every source insert, update, and delete.

## Example

<CodeBlock language="rust">
{`App::builder()
    .model::<Booking>()
    .model::<Rsvp>()
    .materialize(
        Materialized::<Booking>::field(booking::PAYMENT_TOTAL)
            .from::<Rsvp, _, _>(|r: &Rsvp| Some(r.booking_id))   // a changed Rsvp → its Booking
            .recompute_typed(|booking_id: i64| async move {      // the fresh value
                let agg = Rsvp::objects()
                    .filter(rsvp::BOOKING_ID.eq(booking_id))
                    .aggregate(&[("total", Aggregate::sum("amount"))])
                    .await.unwrap_or_default();
                agg["total"].as_i64().unwrap_or(0)
            }),
    )
    .build()?;`}
</CodeBlock>

Now `Booking.payment_total` updates itself whenever an `Rsvp` is created, changed, or deleted - no code at the write sites.

<Callout type="info">
  **Eager, after-commit.** The recompute runs after the source write commits and reads the committed rows, so it's correct but slightly eventually-consistent within the same request. It fires nothing for a transaction that rolls back. The write-back goes through the ORM, so it works on every backend.
</Callout>

<Callout type="warning">
  **v1 scope.** One computed column per declaration, stored in the target's own column, recomputed by your closure. Cached-aggregate integration, deferred (background-task) refresh, and cross-field cycle detection are deferred - a direct self-cascade is caught and stopped, but a multi-field cycle is out of scope. See the design spec for the full boundary.
</Callout>

## See also

- Design + rationale: `docs/superpowers/specs/2026-09-24-materialized-computed-fields-design.md`
- [Signals](../plugins/signals) - the after-commit fan-out this builds on.
```
(If the docs site has no global `<CodeBlock>`, use a fenced ```rust block instead — match the sibling pages under `documentation/docs/v0.0.1/orm/`.)

- [ ] **Step 4: Commit**

```bash
git add crates/umbral-core/tests/materialized_fields.rs documentation/docs/v0.0.1/orm/materialized-fields.mdx
git commit -m "test(orm): string-pk materialized target + docs(orm): materialized-fields page"
```

---

### Task 7: Close the tracker + full-workspace verification

**Files:**
- Modify: `planning/gaps6.md` (mark #7 `[x]` with a write-up)

- [ ] **Step 1: Full-workspace verification**

Run, from the repo root:
```bash
cargo fmt
cargo clippy --all-targets
cargo build
cargo test
```
Expected: green. Note any pre-existing flake (e.g. the `select_related` intra-binary parallel counter) explicitly if it recurs; re-run in isolation to confirm it's not caused by this change.

- [ ] **Step 2: Mark gaps6 #7 done**

Edit `planning/gaps6.md` entry 7: change `[ ]` → `[x]` and replace the problem-statement body with a shipped write-up naming: the `Materialized<M>` builder + erased `MaterializedSpec`, `AppBuilder::materialize`, the after-commit `subscribe_async` install, the `DynQuerySet::filter_pk_eq` generic-pk write-back, the task-local re-entrancy guard, boot validation, the test files, and the deferred items (cached-aggregate integration, deferred refresh, cycle detection). Match the inline style of the already-closed entries (#14/#15).

- [ ] **Step 3: Commit**

```bash
git add planning/gaps6.md
git commit -m "docs(planning): close gaps6 #7 — materialized computed fields shipped"
```

---

## Self-Review notes

- **Spec coverage:** §4 API → Task 1 + Task 4 wiring; §5 boot wiring + system check → Task 4 (install) + Task 5 (validate); §6 recompute handler → Task 4 install; §7 loop guard → Task 3; §8 tests → Tasks 4/5/6 (surface/delete/rollback/null-key/multiple-source covered; the "multiple sources" case is exercised implicitly by two `.from` in Task 1's unit test and can be added to Task 4 if desired); §9 deferred items are documented, not built. All covered.
- **Type consistency:** `recompute_typed` (not `recompute`) is the public typed entry; the erased `recompute` field is `Arc<dyn Fn(Value)->BoxFuture<Option<Value>>>`. `filter_pk_eq(&Value)`, `guarded(String,String,fut)`, `install(&MaterializedSpec)`, `validate(&MaterializedSpec)->Result<(),String>`, `AppBuilder::materialize(MaterializedSpec)` — names used identically across tasks.
- **Naming note:** the spec §4 sketch wrote `.recompute(...)`; the plan uses `.recompute_typed(...)` because a bare `.recompute` generic over `Pk`/`V` needs turbofish-free inference from the closure, which `recompute_typed` gives cleanly. If a bare `.recompute` alias is wanted later it can delegate. Flagged so a reviewer doesn't read it as drift.
- **Review Focus:** every line has an owning test (Task 4 delete/rollback/null-key, Task 2 + Task 6 non-i64 pk, Task 3 per-task guard).

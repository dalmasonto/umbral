//! In-process signal registry.
//!
//! Provides the bare name-keyed pub/sub that the ORM write paths call
//! directly and that `umbra-signals` builds its typed per-model API on
//! top of. Moving the registry here — inside `umbra-core` — breaks the
//! dependency cycle: the signals plugin depends on the `umbra` facade
//! which depends on `umbra-core`, so `umbra-core` can call
//! `crate::signals::emit(...)` without pulling in the plugin crate.
//!
//! ## Surface
//!
//! - [`subscribe`] — register a sync handler by signal name.
//! - [`subscribe_async`] — register an async handler by signal name.
//! - [`emit`] — fire all handlers for a name; returns the handler count.
//! - [`clear_for_tests`] — reset the registry; only for use in tests.
//!
//! ## Signal name conventions
//!
//! ORM signals use `<event>:<table>` names so the set of built-in
//! signals is namespaced away from application-defined ones.
//!
//! Per-row (fired by `Manager::save` / `Manager::delete_instance`):
//!
//! - `pre_save:<table>` — fired before INSERT or UPDATE.
//! - `post_save:<table>` — fired after INSERT or UPDATE.
//! - `pre_delete:<table>` — fired before a single-row DELETE.
//! - `post_delete:<table>` — fired after a single-row DELETE.
//!
//! Bulk (fired once per statement by `bulk_create`, `update_values`,
//! `QuerySet::delete`):
//!
//! - `bulk_post_save:<table>` — payload `{ ids, created, actor }`.
//! - `bulk_post_delete:<table>` — payload `{ ids, actor }`.
//!
//! Many-to-many (fired by `M2M::add` / `remove` / `set` / `clear`):
//!
//! - `m2m_changed:<junction_table>` — payload
//!   `{ action, parent_id, added, removed, actor }`.
//!
//! Application-level signals keep their own name space (`post_user_signup`,
//! `before_email_send`, etc.). The ORM prefix ensures no collision.
//!
//! ## Actor envelope
//!
//! Every signal payload is enriched with an `"actor"` key whose value
//! comes from the nearest enclosing [`with_actor`] scope (or `Null` if
//! none). The merge happens in [`emit`] itself, so user-defined signals
//! pick up the actor too without per-call ceremony.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::{Map, Value};

// =============================================================================
// Actor task-local
//
// Every ORM signal payload carries an `"actor"` key whose value is the
// JSON the nearest enclosing `with_actor(...)` scope set. When no scope
// is active the value is `Null`. Storing the actor as `serde_json::Value`
// keeps `umbra-core` decoupled from any specific identity shape — auth
// plugins, API-key middleware, system-task wrappers all populate it with
// whatever JSON suits the consumer.
//
// The same task-local is the natural carrier for tracing context
// (`trace_id`, `span_id`) once gap #48 lands; both pieces of cross-cutting
// state share the same scoping rules.
// =============================================================================

tokio::task_local! {
    /// The actor (user / identity / system task) responsible for the
    /// async work currently executing. Set via [`with_actor`]; read via
    /// [`current_actor`]. Outside any scope, [`current_actor`] returns
    /// `Value::Null`.
    static ACTOR: Value;
}

/// Run `fut` with `actor` published as the current task-local actor.
///
/// Inside `fut` (and anything `fut` awaits), [`current_actor`] returns
/// `actor`. After `fut` completes — Ok or Err — the previous actor (or
/// `Null`) is restored. Nested calls shadow then restore the outer
/// scope's value.
///
/// Typical use lives in an auth middleware: it resolves the request's
/// session into a JSON identity, wraps the downstream handler in
/// `with_actor(identity, ...).await`, and every ORM write the handler
/// triggers carries that identity in its signal payload.
pub async fn with_actor<F, T>(actor: Value, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    ACTOR.scope(actor, fut).await
}

/// Return the current actor, or `Value::Null` if no [`with_actor`] scope
/// is active on the calling task. Cheap (one task-local read + a clone).
pub fn current_actor() -> Value {
    ACTOR.try_with(|a| a.clone()).unwrap_or(Value::Null)
}

/// Merge the current actor into `payload` under the `"actor"` key. If
/// `payload` already carries an explicit `"actor"`, the caller's value
/// wins — useful for system tasks that want to identify themselves
/// independently of whatever scope they're running under. If `payload`
/// is not an object, it's wrapped as `{ "data": payload, "actor": ... }`.
fn with_payload_actor(mut payload: Value) -> Value {
    match payload.as_object_mut() {
        Some(map) => {
            map.entry("actor").or_insert_with(current_actor);
            payload
        }
        None => {
            let mut map = Map::new();
            map.insert("data".to_string(), payload);
            map.insert("actor".to_string(), current_actor());
            Value::Object(map)
        }
    }
}

pub(crate) type SyncHandler = Box<dyn Fn(&Value) + Send + Sync + 'static>;
pub(crate) type AsyncHandler = Box<
    dyn Fn(&Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync
        + 'static,
>;

struct Registry {
    sync: HashMap<String, Vec<SyncHandler>>,
    r#async: HashMap<String, Vec<AsyncHandler>>,
}

impl Registry {
    fn new() -> Self {
        Self {
            sync: HashMap::new(),
            r#async: HashMap::new(),
        }
    }
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::new()))
}

/// Lock the registry, recovering from a poisoned mutex.
///
/// A sync signal handler that panics while we hold this lock would
/// otherwise poison it, and every later `.expect(...)` would then panic
/// — bricking *every* ORM write that emits a signal, permanently, for
/// the life of the process. The registry is just a handler map: a
/// panicking handler can't leave it half-mutated (it only ever reads the
/// map while dispatching), so recovering the guard via `into_inner` is
/// safe. Pairs with the `catch_unwind` around each sync handler in
/// [`emit`], which stops a bad handler from poisoning the lock at all.
fn lock_registry() -> std::sync::MutexGuard<'static, Registry> {
    registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Register a sync handler for `name`. Multiple handlers per name
/// stack in registration order.
pub fn subscribe<F>(name: &str, handler: F)
where
    F: Fn(&Value) + Send + Sync + 'static,
{
    let mut reg = lock_registry();
    reg.sync
        .entry(name.to_string())
        .or_default()
        .push(Box::new(handler));
}

/// Register an async handler for `name`. The emitter awaits each
/// handler's future in series before returning.
pub fn subscribe_async<F, Fut>(name: &str, handler: F)
where
    F: Fn(&Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let mut reg = lock_registry();
    let wrapped: AsyncHandler = Box::new(move |payload| {
        let fut = handler(payload);
        Box::pin(fut)
    });
    reg.r#async
        .entry(name.to_string())
        .or_default()
        .push(wrapped);
}

/// Emit a named signal. Runs every sync handler then awaits every
/// async handler in series. Returns the total subscriber count that
/// received the event.
///
/// ## Locking discipline
///
/// The lock is held only to collect the async futures, then dropped
/// before any `.await`. Holding the lock across `.await` would block
/// other emitters/subscribes for the duration of every future.
pub async fn emit(name: &str, payload: Value) -> usize {
    // Merge the current actor task-local into the payload before
    // dispatch. Object payloads get `"actor"` slotted in (unless the
    // caller already supplied one); non-object payloads are wrapped as
    // `{ "data": <payload>, "actor": ... }`. With no enclosing
    // `with_actor(...)` scope the actor is `Value::Null` — the key is
    // always present, which keeps subscriber payload-shape stable.
    let payload = with_payload_actor(payload);
    let (futures, total) = {
        let reg = lock_registry();
        let mut count = 0;
        if let Some(handlers) = reg.sync.get(name) {
            for h in handlers {
                // Isolate each handler: a panic here would otherwise
                // poison the registry mutex (bricking every later signal
                // emit, hence every ORM write) and abort this emit before
                // the remaining handlers run. Catch it, log it, carry on.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h(&payload)));
                if result.is_err() {
                    tracing::error!(signal = %name, "sync signal handler panicked; skipping it");
                }
                count += 1;
            }
        }
        let mut futs: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>> =
            Vec::new();
        if let Some(handlers) = reg.r#async.get(name) {
            for h in handlers {
                futs.push(h(&payload));
                count += 1;
            }
        }
        (futs, count)
    };
    for fut in futures {
        // Mirror the sync path's catch_unwind isolation: a panicking async
        // subscriber must not unwind through emit() into the ORM write that
        // fired the signal. Wrap each future with AssertUnwindSafe (the
        // handler already opted in to `Send + Sync + 'static`) and use
        // FutureExt::catch_unwind; on Err log and continue to the next
        // subscriber, exactly as the sync branch does above.
        use futures_util::future::FutureExt as _;
        let result = std::panic::AssertUnwindSafe(fut).catch_unwind().await;
        if result.is_err() {
            tracing::error!(signal = %name, "async signal handler panicked; skipping it");
        }
    }
    total
}

/// Fire the ORM `pre_save` signal for model `M`.
///
/// Payload: `{ "instance": <M as JSON>, "created": bool, "actor": ... }`.
/// Signal name: `pre_save:<M::TABLE>`.
///
/// Called by `Manager::save` before INSERT (created=true) or UPDATE
/// (created=false). Per-row bulk paths fire [`emit_bulk_post_save`] instead.
pub async fn emit_pre_save<M>(instance: &M, created: bool)
where
    M: crate::orm::Model + serde::Serialize,
{
    let Ok(instance_json) = serde_json::to_value(instance) else {
        return;
    };
    emit_pre_save_by_table(M::TABLE, instance_json, created).await;
}

/// Fire the ORM `post_save` signal for model `M`.
///
/// Payload: `{ "instance": <M as JSON>, "created": bool, "actor": ... }`.
/// Signal name: `post_save:<M::TABLE>`.
///
/// Called by `Manager::save` after INSERT (created=true) or UPDATE
/// (created=false). Per-row bulk paths fire [`emit_bulk_post_save`] instead.
pub async fn emit_post_save<M>(instance: &M, created: bool)
where
    M: crate::orm::Model + serde::Serialize,
{
    let Ok(instance_json) = serde_json::to_value(instance) else {
        return;
    };
    emit_post_save_by_table(M::TABLE, instance_json, created).await;
}

/// Fire the ORM `pre_delete` signal for model `M`.
///
/// Payload: `{ "instance": <M as JSON>, "actor": ... }`.
/// Signal name: `pre_delete:<M::TABLE>`.
///
/// Called by `Manager::delete_instance` before the single-row DELETE.
/// Bulk paths fire [`emit_bulk_post_delete`] instead.
pub async fn emit_pre_delete<M>(instance: &M)
where
    M: crate::orm::Model + serde::Serialize,
{
    let Ok(instance_json) = serde_json::to_value(instance) else {
        return;
    };
    emit_pre_delete_by_table(M::TABLE, instance_json).await;
}

/// Fire the ORM `post_delete` signal for model `M`.
///
/// Payload: `{ "instance": <M as JSON>, "actor": ... }`.
/// Signal name: `post_delete:<M::TABLE>`.
///
/// Called by `Manager::delete_instance` after the single-row DELETE.
/// Bulk paths fire [`emit_bulk_post_delete`] instead.
pub async fn emit_post_delete<M>(instance: &M)
where
    M: crate::orm::Model + serde::Serialize,
{
    let Ok(instance_json) = serde_json::to_value(instance) else {
        return;
    };
    emit_post_delete_by_table(M::TABLE, instance_json).await;
}

// =============================================================================
// gaps #77 — `DynQuerySet` write paths fire the same signals as the typed
// `Manager` / `QuerySet` paths. REST endpoints and admin form submits both
// go through `DynQuerySet::insert_json` / `update_json` / `delete`; without
// these emits, every write through those surfaces was invisible to audit
// log / cache-invalidation / search-index subscribers.
//
// The typed M-generic functions above now delegate to these
// `*_by_table` variants. Same signal name format (`pre_save:<table>`),
// same payload shape (`{ "instance": <row>, "created": bool }`), same
// subscribers — the typed and dynamic surfaces are observationally
// identical from a handler's perspective.
//
// PK shape: the `instance` JSON carries the row's PK in whatever shape
// the model declares (i64, String, UUID). Subscribers that index on PK
// should treat the value as `serde_json::Value` rather than assuming
// i64 — that keeps the contract forward-compatible with the planned
// `PrimaryKey` refactor that lifts the framework off i64 hardcoding.
// =============================================================================

/// Table-keyed `pre_save` emit. Called by [`emit_pre_save`] (typed
/// path) and by `DynQuerySet::insert_json` (dynamic path) directly
/// with the row's serialized JSON form.
pub async fn emit_pre_save_by_table(table: &str, instance: serde_json::Value, created: bool) {
    let payload = serde_json::json!({ "instance": instance, "created": created });
    emit(&format!("pre_save:{table}"), payload).await;
}

/// Table-keyed `post_save` emit. See [`emit_pre_save_by_table`].
pub async fn emit_post_save_by_table(table: &str, instance: serde_json::Value, created: bool) {
    let payload = serde_json::json!({ "instance": instance, "created": created });
    emit(&format!("post_save:{table}"), payload).await;
}

/// Table-keyed `pre_delete` emit. See [`emit_pre_save_by_table`].
pub async fn emit_pre_delete_by_table(table: &str, instance: serde_json::Value) {
    let payload = serde_json::json!({ "instance": instance });
    emit(&format!("pre_delete:{table}"), payload).await;
}

/// Table-keyed `post_delete` emit. See [`emit_pre_save_by_table`].
pub async fn emit_post_delete_by_table(table: &str, instance: serde_json::Value) {
    let payload = serde_json::json!({ "instance": instance });
    emit(&format!("post_delete:{table}"), payload).await;
}

/// Table-keyed bulk-save emit for the dynamic-dispatch UPDATE path.
/// Signal name: `bulk_post_save:<table>`. Payload:
/// `{ "ids": [...], "created": bool }` — `created=false` for UPDATE
/// terminals (the only consumer today; `bulk_create_dyn` would pass
/// `created=true` if it ever lands).
pub async fn emit_bulk_post_save_by_table(table: &str, ids: Vec<Value>, created: bool) {
    let payload = serde_json::json!({ "ids": ids, "created": created });
    emit(&format!("bulk_post_save:{table}"), payload).await;
}

/// Table-keyed bulk-delete emit. Signal name:
/// `bulk_post_delete:<table>`. Payload: `{ "ids": [...] }`.
pub async fn emit_bulk_post_delete_by_table(table: &str, ids: Vec<Value>) {
    let payload = serde_json::json!({ "ids": ids });
    emit(&format!("bulk_post_delete:{table}"), payload).await;
}

// =============================================================================
// Bulk-write signals — gap #38 follow-up
//
// `Manager::bulk_create`, `QuerySet::update_values`, and
// `QuerySet::delete` operate on N rows in a single SQL statement. Per-row
// `post_save` / `post_delete` would O(N) the handler fan-out on a tight
// loop — the deliberate trade-off we made (option C in the design pass)
// is one bulk event per call with the affected PKs in the payload.
//
// Names:
//   - `bulk_post_save:<table>`   for INSERT (bulk_create)   and UPDATE
//     (update_values). `created: true` on insert; `false` on update.
//   - `bulk_post_delete:<table>` for DELETE (QuerySet::delete).
//
// Payload shape:
//   bulk_post_save:   `{ "ids": [...], "created": bool, "actor": ... }`
//   bulk_post_delete: `{ "ids": [...], "actor": ... }`
//
// Empty-id arrays still fire the event — the audit consumer learns "an
// UPDATE matched zero rows", which is a distinct signal from "no
// UPDATE happened". Subscribers that want to skip empty events filter
// in their handler.
// =============================================================================

/// Fire the ORM `bulk_post_save` signal for model `M`.
///
/// `created = true` for INSERT terminals (`bulk_create`);
/// `created = false` for UPDATE terminals (`update_values`,
/// `update_expr`). `ids` is the list of primary keys that the statement
/// affected — for inserts that's the autoincrement-assigned PKs, for
/// updates it's the rows the WHERE clause matched.
pub async fn emit_bulk_post_save<M>(ids: Vec<Value>, created: bool)
where
    M: crate::orm::Model,
{
    emit_bulk_post_save_by_table(M::TABLE, ids, created).await;
}

/// Fire the ORM `bulk_post_delete` signal for model `M`. `ids` is the
/// list of primary keys the DELETE removed.
pub async fn emit_bulk_post_delete<M>(ids: Vec<Value>)
where
    M: crate::orm::Model,
{
    emit_bulk_post_delete_by_table(M::TABLE, ids).await;
}

/// Fire the ORM `m2m_changed` signal for a junction-table mutation.
///
/// Signal name: `m2m_changed:<junction_table>`. Payload:
/// `{ "action": "add"|"remove"|"set"|"clear", "parent_id": <PK as JSON>,
///    "added": [...], "removed": [...], "actor": ... }`.
///
/// Conventions per action:
///
/// - `"add"`   — fired by [`crate::orm::m2m::M2M::add`]; `added` has one
///   child PK, `removed` is empty. Fires even when the row already
///   existed (the `ON CONFLICT DO NOTHING` made it a no-op) so audit
///   consumers see the user intent.
/// - `"remove"` — fired by [`crate::orm::m2m::M2M::remove`]; `removed`
///   has one child PK, `added` is empty.
/// - `"set"`    — fired by [`crate::orm::m2m::M2M::set`]; `added` lists
///   the supplied children, `removed` lists the prior children that were
///   cleared (best-effort: empty when the prior set isn't materialised).
/// - `"clear"`  — fired by [`crate::orm::m2m::M2M::clear`]; `removed`
///   lists the prior children (best-effort), `added` is empty.
pub async fn emit_m2m_changed(
    junction_table: &str,
    action: &str,
    parent_id: Value,
    added: Vec<Value>,
    removed: Vec<Value>,
) {
    let payload = serde_json::json!({
        "action": action,
        "parent_id": parent_id,
        "added": added,
        "removed": removed,
    });
    emit(&format!("m2m_changed:{junction_table}"), payload).await;
}

/// Test-only helper: drop every registered handler.
///
/// The signals registry is process-wide; a `#[tokio::test]` that
/// registers handlers can interfere with sibling tests in the same
/// binary. Call `clear_for_tests()` at the top of each test to isolate
/// them.
#[doc(hidden)]
pub fn clear_for_tests() {
    let mut reg = lock_registry();
    reg.sync.clear();
    reg.r#async.clear();
}

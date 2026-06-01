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
//! signals is namespaced away from application-defined ones:
//!
//! - `pre_save:post` — fired before INSERT or UPDATE on `post`.
//! - `post_save:post` — fired after INSERT or UPDATE on `post`.
//! - `pre_delete:post` — fired before DELETE of one row from `post`.
//! - `post_delete:post` — fired after DELETE of one row from `post`.
//!
//! Application-level signals keep their own name space (`post_user_signup`,
//! `before_email_send`, etc.). The ORM prefix ensures no collision.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

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

/// Register a sync handler for `name`. Multiple handlers per name
/// stack in registration order.
pub fn subscribe<F>(name: &str, handler: F)
where
    F: Fn(&Value) + Send + Sync + 'static,
{
    let mut reg = registry().lock().expect("signals registry poisoned");
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
    let mut reg = registry().lock().expect("signals registry poisoned");
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
    let (futures, total) = {
        let reg = registry().lock().expect("signals registry poisoned");
        let mut count = 0;
        if let Some(handlers) = reg.sync.get(name) {
            for h in handlers {
                h(&payload);
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
        fut.await;
    }
    total
}

/// Fire the ORM `pre_save` signal for model `M`.
///
/// Payload: `{ "instance": <M as JSON>, "created": bool }`.
/// Signal name: `pre_save:<M::TABLE>`.
///
/// Called by `Manager::save` before INSERT (created=true) or UPDATE
/// (created=false). Never called by bulk write methods.
pub async fn emit_pre_save<M>(instance: &M, created: bool)
where
    M: crate::orm::Model + serde::Serialize,
{
    let Ok(instance_json) = serde_json::to_value(instance) else {
        return;
    };
    let payload = serde_json::json!({ "instance": instance_json, "created": created });
    emit(&format!("pre_save:{}", M::TABLE), payload).await;
}

/// Fire the ORM `post_save` signal for model `M`.
///
/// Payload: `{ "instance": <M as JSON>, "created": bool }`.
/// Signal name: `post_save:<M::TABLE>`.
///
/// Called by `Manager::save` after INSERT (created=true) or UPDATE
/// (created=false). Never called by bulk write methods.
pub async fn emit_post_save<M>(instance: &M, created: bool)
where
    M: crate::orm::Model + serde::Serialize,
{
    let Ok(instance_json) = serde_json::to_value(instance) else {
        return;
    };
    let payload = serde_json::json!({ "instance": instance_json, "created": created });
    emit(&format!("post_save:{}", M::TABLE), payload).await;
}

/// Fire the ORM `pre_delete` signal for model `M`.
///
/// Payload: `{ "instance": <M as JSON> }`.
/// Signal name: `pre_delete:<M::TABLE>`.
///
/// Called by `Manager::delete_instance` before the single-row DELETE.
/// Never called by bulk `QuerySet::delete()`.
pub async fn emit_pre_delete<M>(instance: &M)
where
    M: crate::orm::Model + serde::Serialize,
{
    let Ok(instance_json) = serde_json::to_value(instance) else {
        return;
    };
    let payload = serde_json::json!({ "instance": instance_json });
    emit(&format!("pre_delete:{}", M::TABLE), payload).await;
}

/// Fire the ORM `post_delete` signal for model `M`.
///
/// Payload: `{ "instance": <M as JSON> }`.
/// Signal name: `post_delete:<M::TABLE>`.
///
/// Called by `Manager::delete_instance` after the single-row DELETE.
/// Never called by bulk `QuerySet::delete()`.
pub async fn emit_post_delete<M>(instance: &M)
where
    M: crate::orm::Model + serde::Serialize,
{
    let Ok(instance_json) = serde_json::to_value(instance) else {
        return;
    };
    let payload = serde_json::json!({ "instance": instance_json });
    emit(&format!("post_delete:{}", M::TABLE), payload).await;
}

/// Test-only helper: drop every registered handler.
///
/// The signals registry is process-wide; a `#[tokio::test]` that
/// registers handlers can interfere with sibling tests in the same
/// binary. Call `clear_for_tests()` at the top of each test to isolate
/// them.
#[doc(hidden)]
pub fn clear_for_tests() {
    let mut reg = registry().lock().expect("signals registry poisoned");
    reg.sync.clear();
    reg.r#async.clear();
}

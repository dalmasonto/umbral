//! umbral-signals — Django-style per-model lifecycle signals + generic
//! name-keyed pub/sub.
//!
//! ## Quick start — model signals
//!
//! Subscribe to lifecycle hooks for a specific model type using the
//! typed `on_model::<M>()` API:
//!
//! ```rust,ignore
//! use umbral_signals::on_model;
//! use my_app::models::Post;
//!
//! on_model::<Post>().post_save(|post, created| async move {
//!     if created {
//!         tracing::info!(id = post.id, "new post created");
//!     }
//! });
//! ```
//!
//! The ORM fires the signals automatically when you call
//! `Post::objects().save(instance).await` or
//! `Post::objects().delete_instance(&instance).await`.
//!
//! ## Signal lifecycle
//!
//! | Method | Fires | Triggered by |
//! |--------|-------|--------------|
//! | `pre_save`    | before INSERT or UPDATE   | `Manager::save` |
//! | `post_save`   | after INSERT or UPDATE    | `Manager::save` |
//! | `pre_update`  | before UPDATE only        | `Manager::save` |
//! | `post_update` | after UPDATE only         | `Manager::save` |
//! | `pre_delete`  | before per-row DELETE     | `Manager::delete_instance` |
//! | `post_delete` | after per-row DELETE      | `Manager::delete_instance` |
//!
//! `pre_update` / `post_update` carry BOTH the old (`previous`) and new
//! (`instance`) row, and fire only on UPDATE. The ORM reads the old-row
//! snapshot only when an `*_update` subscriber exists, so they cost
//! nothing when nobody listens.
//!
//! **Bulk methods do NOT fire signals.** `Manager::create`,
//! `Manager::bulk_create`, `QuerySet::update_values`, and
//! `QuerySet::delete` are signal-free for performance reasons,
//! matching Django's own behaviour. See the doc callout in the
//! user-facing docs at `documentation/docs/v0.0.1/plugins/signals.mdx`.
//!
//! ## Signal name format
//!
//! Internally the typed API subscribes under `<event>:<table>` names:
//! `pre_save:post`, `post_save:auth_user`, etc. The generic
//! `subscribe` / `emit` functions operate on the same namespace, so
//! app-defined signals should NOT use the `<event>:<table>` format to
//! avoid accidental collisions with ORM signals.
//!
//! ## Generic pub/sub
//!
//! The lower-level `subscribe` / `subscribe_async` / `emit` functions
//! are still available for application-defined signals that aren't
//! tied to a model lifecycle:
//!
//! ```rust,ignore
//! use umbral_signals::{emit, subscribe_async};
//!
//! subscribe_async("order_placed", |payload| async move {
//!     // payload is serde_json::Value
//!     let order_id = payload["id"].as_i64().unwrap_or(0);
//!     // ...
//! });
//!
//! emit("order_placed", serde_json::json!({ "id": 42 })).await;
//! ```
//!
//! ## In-process only at v1
//!
//! Signals are strictly in-process. For work that must survive a
//! process crash, pair signals with `umbral-tasks`: the signal handler
//! enqueues a task; the worker runs it durably.
//!
//! ## Deferred past v1
//!
//! - Typed event enums with compile-time emitter/subscriber agreement.
//! - Cross-process broadcast (Redis / NATS adapter).
//! - Signal `disconnect` / per-call `disable` for testing.
//! - `m2m_changed` signals for many-to-many relationships.

use std::future::Future;
use std::marker::PhantomData;

use serde::Serialize;
use serde::de::DeserializeOwned;
use umbral::prelude::*;

// Re-export the bare registry from umbral-core so user code that
// imports from umbral-signals gets everything in one place.
pub use umbral::signals::{clear_for_tests, emit, subscribe, subscribe_async};

/// Entry point for typed per-model signals.
///
/// Returns a [`ModelSignals<M>`] builder on which you attach handlers
/// for `pre_save`, `post_save`, `pre_delete`, and `post_delete`.
///
/// ```rust,ignore
/// use umbral_signals::on_model;
/// use my_app::models::AuthUser;
///
/// on_model::<AuthUser>().post_save(|user, created| async move {
///     if created {
///         // Enqueue a welcome-email task.
///         umbral_tasks::enqueue(
///             "send_welcome_email",
///             &WelcomeEmailPayload { user_id: user.id },
///         ).await.ok();
///     }
/// });
/// ```
pub fn on_model<M: Model>() -> ModelSignals<M> {
    ModelSignals { _m: PhantomData }
}

/// Typed handler builder for a single model type `M`.
///
/// Obtained via [`on_model::<M>()`]. Each method registers one handler
/// under the corresponding ORM signal name (`pre_save:<table>`, etc.).
/// The handler receives a deserialised `&M` (not a raw `serde_json::Value`),
/// so the caller never writes manual JSON field access.
///
/// All four methods take an async closure that returns a `Future<Output = ()>`
/// and is `Send + Sync + 'static`. Handlers are awaited in series by the
/// emitter (same semantics as Django's `Signal.send`). Spawn a
/// `tokio::task::spawn` inside the handler for fire-and-forget work.
pub struct ModelSignals<M: Model> {
    _m: PhantomData<M>,
}

/// Decode the `"instance"` key of a signal payload into the model `M`.
///
/// Returns `None` (and logs a warning) when the JSON is present but
/// doesn't deserialize into `M` — previously this swallowed the error
/// with `.ok()` (gaps: BROKEN-5), so a payload/schema drift made typed
/// handlers silently stop firing with nothing in the logs to explain it.
/// A genuinely-absent instance (non-object) stays a quiet `None`.
fn decode_instance<M: DeserializeOwned>(payload: &serde_json::Value) -> Option<M> {
    decode_key::<M>(payload, "instance")
}

/// Decode the named key (`"instance"` or `"previous"`) of a signal
/// payload into the model `M`. Shared by [`decode_instance`] and the
/// `pre_update` / `post_update` helpers (gaps2 #92), which need to decode
/// BOTH the old (`"previous"`) and new (`"instance"`) rows.
fn decode_key<M: DeserializeOwned>(payload: &serde_json::Value, key: &str) -> Option<M> {
    let raw = &payload[key];
    if !raw.is_object() {
        return None;
    }
    match serde_json::from_value::<M>(raw.clone()) {
        Ok(inst) => Some(inst),
        Err(e) => {
            tracing::warn!(
                model = std::any::type_name::<M>(),
                key = %key,
                error = %e,
                "signal payload could not be decoded into model; typed handler skipped"
            );
            None
        }
    }
}

impl<M> ModelSignals<M>
where
    M: Model + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    /// Register a handler called **before** INSERT or UPDATE for this model.
    ///
    /// `handler(instance, created)` receives a reference to the
    /// instance that is about to be written and `created = true` if
    /// this is an INSERT or `false` if it is an UPDATE.
    ///
    /// Signal name: `pre_save:<M::TABLE>`.
    pub fn pre_save<F, Fut>(&self, handler: F)
    where
        F: Fn(&M, bool) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let name = format!("pre_save:{}", M::TABLE);
        subscribe_async(&name, move |payload| {
            let instance: Option<M> = decode_instance::<M>(payload);
            let created = payload["created"].as_bool().unwrap_or(false);
            let fut = instance.map(|inst| handler(&inst, created));
            async move {
                if let Some(f) = fut {
                    f.await;
                }
            }
        });
    }

    /// Register a handler called **after** INSERT or UPDATE for this model.
    ///
    /// `handler(instance, created)` receives a reference to the
    /// row as it now exists in the database and `created = true` if the
    /// write was an INSERT or `false` if it was an UPDATE.
    ///
    /// Signal name: `post_save:<M::TABLE>`.
    pub fn post_save<F, Fut>(&self, handler: F)
    where
        F: Fn(&M, bool) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let name = format!("post_save:{}", M::TABLE);
        subscribe_async(&name, move |payload| {
            let instance: Option<M> = decode_instance::<M>(payload);
            let created = payload["created"].as_bool().unwrap_or(false);
            let fut = instance.map(|inst| handler(&inst, created));
            async move {
                if let Some(f) = fut {
                    f.await;
                }
            }
        });
    }

    /// Register a handler called **before** an UPDATE for this model
    /// (gaps2 #92). Fires ONLY on UPDATE — never INSERT.
    ///
    /// `handler(previous, instance)` receives the row as it existed in
    /// the DB immediately before the UPDATE (`previous`) and the value
    /// about to be written (`instance`). Use it for audit diffs / change
    /// tracking that need the old value.
    ///
    /// **Note:** the old-row snapshot the ORM reads to feed `previous` is
    /// only taken when a `pre_update` / `post_update` subscriber exists,
    /// so registering this handler turns that read on. Fires only for
    /// `Manager::save` (the typed per-row UPDATE path).
    ///
    /// Signal name: `pre_update:<M::TABLE>`.
    pub fn pre_update<F, Fut>(&self, handler: F)
    where
        F: Fn(M, M) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let name = format!("pre_update:{}", M::TABLE);
        subscribe_async(&name, move |payload| {
            let previous: Option<M> = decode_key::<M>(payload, "previous");
            let instance: Option<M> = decode_key::<M>(payload, "instance");
            let fut = match (previous, instance) {
                (Some(p), Some(i)) => Some(handler(p, i)),
                _ => None,
            };
            async move {
                if let Some(f) = fut {
                    f.await;
                }
            }
        });
    }

    /// Register a handler called **after** an UPDATE for this model
    /// (gaps2 #92). Fires ONLY on UPDATE — never INSERT.
    ///
    /// `handler(previous, instance)` receives the pre-UPDATE row
    /// (`previous`) and the row as it now exists after the UPDATE
    /// (`instance`). umbral-storage's replace-cleanup subscribes here to
    /// delete the OLD blob when a file field is changed to a new key.
    ///
    /// Signal name: `post_update:<M::TABLE>`.
    pub fn post_update<F, Fut>(&self, handler: F)
    where
        F: Fn(M, M) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let name = format!("post_update:{}", M::TABLE);
        subscribe_async(&name, move |payload| {
            let previous: Option<M> = decode_key::<M>(payload, "previous");
            let instance: Option<M> = decode_key::<M>(payload, "instance");
            let fut = match (previous, instance) {
                (Some(p), Some(i)) => Some(handler(p, i)),
                _ => None,
            };
            async move {
                if let Some(f) = fut {
                    f.await;
                }
            }
        });
    }

    /// Register a handler called **before** a per-row DELETE for this model.
    ///
    /// `handler(instance)` receives a reference to the instance that is
    /// about to be deleted.
    ///
    /// **Note:** only fires for `Manager::delete_instance`. Bulk
    /// `QuerySet::delete()` calls do NOT fire this signal.
    ///
    /// Signal name: `pre_delete:<M::TABLE>`.
    pub fn pre_delete<F, Fut>(&self, handler: F)
    where
        F: Fn(&M) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let name = format!("pre_delete:{}", M::TABLE);
        subscribe_async(&name, move |payload| {
            let instance: Option<M> = decode_instance::<M>(payload);
            let fut = instance.map(|inst| handler(&inst));
            async move {
                if let Some(f) = fut {
                    f.await;
                }
            }
        });
    }

    /// Register a handler called **after** a per-row DELETE for this model.
    ///
    /// `handler(instance)` receives a reference to the instance that
    /// was just deleted (as it was at call time — not a DB read-back).
    ///
    /// **Note:** only fires for `Manager::delete_instance`. Bulk
    /// `QuerySet::delete()` calls do NOT fire this signal.
    ///
    /// Signal name: `post_delete:<M::TABLE>`.
    pub fn post_delete<F, Fut>(&self, handler: F)
    where
        F: Fn(&M) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let name = format!("post_delete:{}", M::TABLE);
        subscribe_async(&name, move |payload| {
            let instance: Option<M> = decode_instance::<M>(payload);
            let fut = instance.map(|inst| handler(&inst));
            async move {
                if let Some(f) = fut {
                    f.await;
                }
            }
        });
    }
}

/// The plugin marker. Carries no models, no routes, no system checks.
///
/// Register it so other plugins can declare `"signals"` as a dependency
/// and be confident the registry is initialised before their `on_ready`
/// fires.
///
/// ```rust,ignore
/// App::builder()
///     .plugin(SignalsPlugin)
///     .plugin(AuthPlugin::default())
///     .build()?;
/// ```
#[derive(Debug, Default)]
pub struct SignalsPlugin;

impl Plugin for SignalsPlugin {
    fn name(&self) -> &'static str {
        "signals"
    }
}

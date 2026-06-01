//! umbra-signals — Django-style per-model lifecycle signals + generic
//! name-keyed pub/sub.
//!
//! ## Quick start — model signals
//!
//! Subscribe to lifecycle hooks for a specific model type using the
//! typed `on_model::<M>()` API:
//!
//! ```rust,ignore
//! use umbra_signals::on_model;
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
//! | `pre_delete`  | before per-row DELETE     | `Manager::delete_instance` |
//! | `post_delete` | after per-row DELETE      | `Manager::delete_instance` |
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
//! use umbra_signals::{emit, subscribe_async};
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
//! process crash, pair signals with `umbra-tasks`: the signal handler
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
use umbra::prelude::*;

// Re-export the bare registry from umbra-core so user code that
// imports from umbra-signals gets everything in one place.
pub use umbra::signals::{clear_for_tests, emit, subscribe, subscribe_async};

/// Entry point for typed per-model signals.
///
/// Returns a [`ModelSignals<M>`] builder on which you attach handlers
/// for `pre_save`, `post_save`, `pre_delete`, and `post_delete`.
///
/// ```rust,ignore
/// use umbra_signals::on_model;
/// use my_app::models::AuthUser;
///
/// on_model::<AuthUser>().post_save(|user, created| async move {
///     if created {
///         // Enqueue a welcome-email task.
///         umbra_tasks::enqueue(
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
            let instance: Option<M> = payload["instance"]
                .as_object()
                .and_then(|_| serde_json::from_value(payload["instance"].clone()).ok());
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
            let instance: Option<M> = payload["instance"]
                .as_object()
                .and_then(|_| serde_json::from_value(payload["instance"].clone()).ok());
            let created = payload["created"].as_bool().unwrap_or(false);
            let fut = instance.map(|inst| handler(&inst, created));
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
            let instance: Option<M> = payload["instance"]
                .as_object()
                .and_then(|_| serde_json::from_value(payload["instance"].clone()).ok());
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
            let instance: Option<M> = payload["instance"]
                .as_object()
                .and_then(|_| serde_json::from_value(payload["instance"].clone()).ok());
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

//! umbra-realtime — real-time push over SSE + WebSocket.
//!
//! Push data to **one user** or to a **named group/room** without
//! hand-rolling connection bookkeeping:
//!
//! ```ignore
//! use umbra_realtime::Realtime;
//!
//! Realtime::to_user(42).send("notification", &payload).await;
//! Realtime::to_group("chat:123").send("message", &msg).await;
//! Realtime::broadcast().send("ping", &json!({})).await;
//! ```
//!
//! Shipped: the connection registry + broker seam (`InProcessBroker` for
//! single-instance; a `RedisBroker` is the documented multi-instance
//! swap), the ambient [`Realtime`] handle, and both transports — SSE
//! (`GET /realtime/sse`, push-only) and WebSocket (`GET /realtime/ws`,
//! bidirectional with a [`MessageHandler`] for inbound frames). The
//! signals bridge (`on_model`) is the remaining phase (see
//! `docs/superpowers/specs/2026-06-13-umbra-realtime-design.md`).
//!
//! ## Why a broker now
//!
//! `to_user(42)` only reaches user 42 if the message lands on the process
//! that owns their socket. Single-process is fine for v1, but routing
//! through a [`Broker`] means multi-instance becomes a drop-in
//! (`RedisBroker`) later instead of an API break.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use serde::Serialize;
use tokio::sync::{RwLock, mpsc};
use umbra::plugin::{AppContext, Plugin, PluginError};

/// Re-export so a `MessageHandler` impl can name the attribute
/// (`#[umbra_realtime::async_trait]`) without a direct `async-trait` dep.
pub use async_trait::async_trait;

mod sse;
mod ws;

/// A unique id per open connection (one socket).
pub type ConnId = u64;

/// Default per-connection outbound buffer. A connection that can't keep
/// up (its buffer fills) drops events rather than blocking the sender —
/// best-effort delivery, never back-pressure onto a request handler.
pub const DEFAULT_BUFFER: usize = 64;

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

/// One server→client event: a named event plus a JSON payload. The SSE
/// transport renders this as `event: <event>\ndata: <json>`; the WS
/// transport sends it as a JSON text frame.
#[derive(Clone, Debug)]
pub struct Event {
    /// The event name (`"message"`, `"notification"`, …).
    pub event: String,
    /// The JSON payload.
    pub data: serde_json::Value,
}

/// Who an [`Event`] is addressed to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetKind {
    /// Every live connection authenticated as this user id.
    User(i64),
    /// Every connection that has joined this group/room.
    Group(String),
    /// Every connection.
    Broadcast,
}

/// A message published to the [`Broker`]: a target + the event to deliver.
#[derive(Clone, Debug)]
pub struct Envelope {
    pub target: TargetKind,
    pub event: String,
    pub data: serde_json::Value,
}

// =========================================================================
// Connection registry.
// =========================================================================

struct ConnEntry {
    tx: mpsc::Sender<Event>,
    user_id: Option<i64>,
    groups: HashSet<String>,
}

#[derive(Default)]
struct RegistryInner {
    conns: HashMap<ConnId, ConnEntry>,
    by_user: HashMap<i64, HashSet<ConnId>>,
    by_group: HashMap<String, HashSet<ConnId>>,
}

/// Tracks every live connection and the user / groups it belongs to, so a
/// [`TargetKind`] resolves to a set of connections in O(1). Shared behind
/// an `Arc`; the transports register/deregister, the broker dispatches.
#[derive(Default)]
pub struct Registry {
    inner: RwLock<RegistryInner>,
}

impl Registry {
    /// Register a new connection. Returns its [`ConnId`] and the receiving
    /// half of its outbound channel (the transport turns this into the
    /// SSE/WS stream). `user_id` is the authenticated identity (or `None`
    /// for anonymous); `groups` are the rooms it joined at handshake.
    pub async fn register(
        &self,
        user_id: Option<i64>,
        groups: HashSet<String>,
        buffer: usize,
    ) -> (ConnId, mpsc::Receiver<Event>) {
        let id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(buffer.max(1));
        let mut inner = self.inner.write().await;
        if let Some(uid) = user_id {
            inner.by_user.entry(uid).or_default().insert(id);
        }
        for g in &groups {
            inner.by_group.entry(g.clone()).or_default().insert(id);
        }
        inner.conns.insert(
            id,
            ConnEntry {
                tx,
                user_id,
                groups,
            },
        );
        (id, rx)
    }

    /// Remove a connection from the registry and every index it appears
    /// in. Called when its socket closes (a `ConnGuard` in the transport
    /// ensures this runs even on a dropped stream).
    pub async fn deregister(&self, id: ConnId) {
        let mut inner = self.inner.write().await;
        let Some(entry) = inner.conns.remove(&id) else {
            return;
        };
        if let Some(uid) = entry.user_id
            && let Some(set) = inner.by_user.get_mut(&uid)
        {
            set.remove(&id);
            if set.is_empty() {
                inner.by_user.remove(&uid);
            }
        }
        for g in &entry.groups {
            if let Some(set) = inner.by_group.get_mut(g) {
                set.remove(&id);
                if set.is_empty() {
                    inner.by_group.remove(g);
                }
            }
        }
    }

    /// Add a live connection to a group (server-driven membership).
    pub async fn join(&self, id: ConnId, group: impl Into<String>) {
        let group = group.into();
        let mut inner = self.inner.write().await;
        if let Some(entry) = inner.conns.get_mut(&id) {
            entry.groups.insert(group.clone());
            inner.by_group.entry(group).or_default().insert(id);
        }
    }

    /// Remove a live connection from a group.
    pub async fn leave(&self, id: ConnId, group: &str) {
        let mut inner = self.inner.write().await;
        if let Some(entry) = inner.conns.get_mut(&id) {
            entry.groups.remove(group);
        }
        if let Some(set) = inner.by_group.get_mut(group) {
            set.remove(&id);
            if set.is_empty() {
                inner.by_group.remove(group);
            }
        }
    }

    /// Deliver `event` to every connection matching `target`. Best-effort:
    /// a connection whose buffer is full or closed is skipped. Returns the
    /// number of connections the event was queued to.
    pub async fn dispatch(&self, target: &TargetKind, event: Event) -> usize {
        let inner = self.inner.read().await;
        let ids: Vec<ConnId> = match target {
            TargetKind::User(uid) => inner
                .by_user
                .get(uid)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default(),
            TargetKind::Group(g) => inner
                .by_group
                .get(g)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default(),
            TargetKind::Broadcast => inner.conns.keys().copied().collect(),
        };
        let mut delivered = 0;
        for id in ids {
            if let Some(entry) = inner.conns.get(&id)
                && entry.tx.try_send(event.clone()).is_ok()
            {
                delivered += 1;
            }
        }
        delivered
    }

    /// Current live connection count (diagnostics / tests).
    pub async fn connection_count(&self) -> usize {
        self.inner.read().await.conns.len()
    }
}

// =========================================================================
// Broker — the multi-instance seam.
// =========================================================================

/// How a published [`Envelope`] reaches the connections it targets.
///
/// [`InProcessBroker`] dispatches straight to the local [`Registry`]
/// (single instance). A future `RedisBroker` would `PUBLISH` to Redis and
/// run a task that `SUBSCRIBE`s and dispatches to the local registry, so
/// `to_user(42)` reaches whichever instance holds that socket — no change
/// to the [`Realtime`] API.
#[async_trait::async_trait]
pub trait Broker: Send + Sync {
    async fn publish(&self, env: Envelope);
}

/// Single-instance broker: `publish` dispatches directly to the local
/// registry. Correct and dependency-free for one process.
pub struct InProcessBroker {
    registry: Arc<Registry>,
}

impl InProcessBroker {
    pub fn new(registry: Arc<Registry>) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl Broker for InProcessBroker {
    async fn publish(&self, env: Envelope) {
        self.registry
            .dispatch(
                &env.target,
                Event {
                    event: env.event,
                    data: env.data,
                },
            )
            .await;
    }
}

// =========================================================================
// Group policy — the auth seam.
// =========================================================================

/// Decides whether a connection may join a group. The default denies any
/// non-`public:` group, so a client can't subscribe to `tenant:99` or
/// `chat:123` it has no claim to — override to grant access from the
/// authenticated identity (membership tables, tenant id, role).
pub trait GroupPolicy: Send + Sync {
    /// `user_id` is the authenticated user (or `None` for anonymous).
    /// Return `true` to allow the join. Default: only `public:*` groups.
    fn can_join(&self, user_id: Option<i64>, group: &str) -> bool {
        let _ = user_id;
        group.starts_with("public:")
    }
}

/// The permissive-for-public default policy.
pub struct PublicGroupsOnly;
impl GroupPolicy for PublicGroupsOnly {}

// =========================================================================
// Inbound WebSocket messages.
// =========================================================================

/// Who sent an inbound WebSocket message — the connection id + its user.
/// A handler uses this to authorize and route (e.g. join a room, or
/// broadcast back to the sender's group via [`Realtime::to_group`]).
pub struct MessageContext {
    pub conn_id: ConnId,
    pub user_id: Option<i64>,
}

/// Handles text frames a WebSocket client sends to the server. SSE is
/// push-only, so this only matters for the WS transport. The default
/// ([`NoopMessageHandler`]) ignores inbound frames; a chat app implements
/// this to broadcast a received message to its room:
///
/// ```ignore
/// async fn on_message(&self, ctx: &MessageContext, text: String) {
///     let msg: ChatMsg = serde_json::from_str(&text).unwrap();
///     Realtime::to_group(&msg.room).send("message", &msg).await;
/// }
/// ```
#[async_trait::async_trait]
pub trait MessageHandler: Send + Sync {
    async fn on_message(&self, ctx: &MessageContext, text: String);
}

/// Ignores inbound frames — for push-only apps.
pub struct NoopMessageHandler;

#[async_trait::async_trait]
impl MessageHandler for NoopMessageHandler {
    async fn on_message(&self, _ctx: &MessageContext, _text: String) {}
}

// =========================================================================
// Signals bridge — model changes fan out with zero polling.
// =========================================================================

/// What happened to a model row (decoded from the ORM signal payload).
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelAction {
    Created,
    Updated,
    Deleted,
}

/// A model-change event handed to an [`on_model`](RealtimePlugin::on_model)
/// handler: the table, what happened, the row as JSON, and the actor that
/// triggered it (from the signals task-local).
#[derive(Clone, Debug, Serialize)]
pub struct ModelEvent {
    pub table: String,
    pub action: ModelAction,
    /// The row, serialized to JSON (the signal's `instance` payload).
    pub instance: serde_json::Value,
    /// Who triggered the change (`Null` outside a `with_actor` scope).
    pub actor: serde_json::Value,
}

impl ModelEvent {
    /// The row's `id`, if present and integer — the common case for the
    /// default i64 PK. Returns `None` for a non-`id` PK or a missing id.
    pub fn pk(&self) -> Option<i64> {
        self.instance.get("id").and_then(|v| v.as_i64())
    }

    fn from_payload(table: &str, payload: &serde_json::Value, is_delete: bool) -> ModelEvent {
        let instance = payload
            .get("instance")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let actor = payload
            .get("actor")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let action = if is_delete {
            ModelAction::Deleted
        } else if payload
            .get("created")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            ModelAction::Created
        } else {
            ModelAction::Updated
        };
        ModelEvent {
            table: table.to_string(),
            action,
            instance,
            actor,
        }
    }
}

// =========================================================================
// Ambient handle.
// =========================================================================

static REALTIME: OnceLock<Realtime> = OnceLock::new();

/// The ambient real-time handle, set by [`RealtimePlugin`] at boot. Build
/// targets with [`Realtime::to_user`] / [`to_group`](Realtime::to_group) /
/// [`broadcast`](Realtime::broadcast) and call [`Target::send`].
#[derive(Clone)]
pub struct Realtime {
    broker: Arc<dyn Broker>,
    registry: Arc<Registry>,
    policy: Arc<dyn GroupPolicy>,
    handler: Arc<dyn MessageHandler>,
}

impl Realtime {
    fn get() -> &'static Realtime {
        REALTIME
            .get()
            .expect("umbra-realtime: RealtimePlugin is not installed")
    }

    /// Whether `RealtimePlugin` has been installed (the ambient handle is
    /// set). `send` no-ops when this is `false`.
    pub fn is_installed() -> bool {
        REALTIME.get().is_some()
    }

    /// The shared registry (the transports register connections here).
    pub fn registry() -> Arc<Registry> {
        Self::get().registry.clone()
    }

    /// The configured group-join policy (the transports check it at
    /// handshake before joining a connection to a group).
    pub fn policy() -> Arc<dyn GroupPolicy> {
        Self::get().policy.clone()
    }

    /// The configured inbound-message handler (WS transport).
    pub fn message_handler() -> Arc<dyn MessageHandler> {
        Self::get().handler.clone()
    }

    /// Target a single user's every live connection.
    pub fn to_user(user_id: i64) -> Target {
        Target {
            target: TargetKind::User(user_id),
        }
    }

    /// Target every connection in a group/room.
    pub fn to_group(group: impl Into<String>) -> Target {
        Target {
            target: TargetKind::Group(group.into()),
        }
    }

    /// Target every connection.
    pub fn broadcast() -> Target {
        Target {
            target: TargetKind::Broadcast,
        }
    }
}

/// A pending send to a [`TargetKind`]. Call [`send`](Target::send).
pub struct Target {
    target: TargetKind,
}

impl Target {
    /// Serialize `data` to JSON and push a named event to every matching
    /// connection. Fire-and-forget: it publishes to the broker and returns
    /// — it never blocks on socket I/O, so it's safe in a request handler.
    ///
    /// **No-op when `RealtimePlugin` isn't installed** — so a handler can
    /// call `Realtime::to_group(...).send(...)` unconditionally and an app
    /// (or a test) that doesn't wire realtime simply ignores it, rather
    /// than panicking. Check [`Realtime::is_installed`] if you need to know.
    pub async fn send<T: Serialize>(self, event: &str, data: &T) {
        let Some(rt) = REALTIME.get() else {
            return;
        };
        let data = serde_json::to_value(data).unwrap_or(serde_json::Value::Null);
        rt.broker
            .publish(Envelope {
                target: self.target,
                event: event.to_string(),
                data,
            })
            .await;
    }
}

// =========================================================================
// Plugin.
// =========================================================================

/// Installs real-time push: sets the ambient [`Realtime`] handle at boot
/// and mounts `GET /realtime/sse`.
pub struct RealtimePlugin {
    policy: Arc<dyn GroupPolicy>,
    handler: Arc<dyn MessageHandler>,
    /// Deferred signal-subscription registrations (one per `on_table` /
    /// `on_model` call). Run once at `on_ready` so they only fire when the
    /// plugin is actually installed.
    subscriptions: Vec<Box<dyn Fn() + Send + Sync>>,
}

impl Default for RealtimePlugin {
    fn default() -> Self {
        Self {
            policy: Arc::new(PublicGroupsOnly),
            handler: Arc::new(NoopMessageHandler),
            subscriptions: Vec::new(),
        }
    }
}

impl RealtimePlugin {
    /// Override the group-join policy. The default ([`PublicGroupsOnly`])
    /// allows only `public:*` groups; supply your own to grant access to
    /// private rooms from the authenticated identity.
    pub fn group_policy<P: GroupPolicy + 'static>(mut self, policy: P) -> Self {
        self.policy = Arc::new(policy);
        self
    }

    /// Set the inbound-message handler for the WebSocket transport. The
    /// default ([`NoopMessageHandler`]) ignores client frames (push-only).
    pub fn message_handler<H: MessageHandler + 'static>(mut self, handler: H) -> Self {
        self.handler = Arc::new(handler);
        self
    }

    /// Fan out a table's create/update/delete to real-time clients with
    /// zero polling. Subscribes to the ORM's `post_save:<table>` /
    /// `post_delete:<table>` signals (gap #38); each fire decodes a
    /// [`ModelEvent`] and runs `handler` (which typically pushes via
    /// [`Realtime::to_group`] / [`to_user`](Realtime::to_user)).
    ///
    /// ```ignore
    /// RealtimePlugin::default().on_table("post", |ev| async move {
    ///     Realtime::to_group(format!("post:{}", ev.pk().unwrap_or(0)))
    ///         .send("changed", &ev).await;
    /// })
    /// ```
    pub fn on_table<F, Fut>(mut self, table: impl Into<String>, handler: F) -> Self
    where
        F: Fn(ModelEvent) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let table = table.into();
        let handler = Arc::new(handler);
        self.subscriptions.push(Box::new(move || {
            for (signal, is_delete) in [("post_save", false), ("post_delete", true)] {
                let h = handler.clone();
                let t = table.clone();
                umbra::signals::subscribe_async(
                    &format!("{signal}:{table}"),
                    move |payload: &serde_json::Value| {
                        let ev = ModelEvent::from_payload(&t, payload, is_delete);
                        let h = h.clone();
                        async move {
                            h(ev).await;
                        }
                    },
                );
            }
        }));
        self
    }

    /// Typed sugar over [`on_table`](Self::on_table): `on_model::<Post>(...)`
    /// uses `Post`'s table name.
    pub fn on_model<T, F, Fut>(self, handler: F) -> Self
    where
        T: umbra::orm::Model,
        F: Fn(ModelEvent) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.on_table(T::TABLE, handler)
    }
}

impl Plugin for RealtimePlugin {
    fn name(&self) -> &'static str {
        "realtime"
    }

    fn routes(&self) -> umbra::web::Router {
        umbra::web::Router::new()
            .route("/realtime/sse", umbra::web::get(sse::sse_handler))
            .route("/realtime/ws", umbra::web::get(ws::ws_handler))
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        let registry = Arc::new(Registry::default());
        let broker: Arc<dyn Broker> = Arc::new(InProcessBroker::new(registry.clone()));
        let _ = REALTIME.set(Realtime {
            broker,
            registry,
            policy: self.policy.clone(),
            handler: self.handler.clone(),
        });
        // Register the model-change subscriptions now that the ambient
        // handle exists (a fired handler calls Realtime::to_group, etc.).
        for register in &self.subscriptions {
            register();
        }
        tracing::info!(
            "realtime: in-process broker ready; SSE at /realtime/sse, WS at /realtime/ws"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(gs: &[&str]) -> HashSet<String> {
        gs.iter().map(|s| s.to_string()).collect()
    }

    async fn recv(rx: &mut mpsc::Receiver<Event>) -> Option<Event> {
        // Events dispatch synchronously into the channel, so a try_recv is
        // enough once dispatch has returned.
        rx.try_recv().ok()
    }

    #[tokio::test]
    async fn to_user_reaches_every_connection_of_that_user() {
        let reg = Registry::default();
        let (_a, mut rx_a) = reg.register(Some(7), groups(&[]), DEFAULT_BUFFER).await;
        let (_b, mut rx_b) = reg.register(Some(7), groups(&[]), DEFAULT_BUFFER).await;
        let (_c, mut rx_c) = reg.register(Some(9), groups(&[]), DEFAULT_BUFFER).await;

        let n = reg
            .dispatch(
                &TargetKind::User(7),
                Event {
                    event: "ping".into(),
                    data: serde_json::json!({"x": 1}),
                },
            )
            .await;

        assert_eq!(n, 2, "both of user 7's connections received it");
        assert!(recv(&mut rx_a).await.is_some());
        assert!(recv(&mut rx_b).await.is_some());
        assert!(recv(&mut rx_c).await.is_none(), "user 9 did not");
    }

    #[tokio::test]
    async fn to_group_targets_only_joined_connections() {
        let reg = Registry::default();
        let (_a, mut rx_a) = reg
            .register(Some(1), groups(&["chat:1"]), DEFAULT_BUFFER)
            .await;
        let (_b, mut rx_b) = reg
            .register(Some(2), groups(&["chat:2"]), DEFAULT_BUFFER)
            .await;

        let n = reg
            .dispatch(
                &TargetKind::Group("chat:1".into()),
                Event {
                    event: "message".into(),
                    data: serde_json::json!("hi"),
                },
            )
            .await;
        assert_eq!(n, 1);
        assert!(recv(&mut rx_a).await.is_some());
        assert!(recv(&mut rx_b).await.is_none());
    }

    #[tokio::test]
    async fn broadcast_reaches_all_and_deregister_cleans_indexes() {
        let reg = Registry::default();
        let (a, _rx_a) = reg.register(Some(1), groups(&["g"]), DEFAULT_BUFFER).await;
        let (_b, _rx_b) = reg.register(None, groups(&["g"]), DEFAULT_BUFFER).await;

        assert_eq!(reg.connection_count().await, 2);
        let n = reg
            .dispatch(
                &TargetKind::Broadcast,
                Event {
                    event: "e".into(),
                    data: serde_json::Value::Null,
                },
            )
            .await;
        assert_eq!(n, 2, "broadcast hit both");

        reg.deregister(a).await;
        assert_eq!(reg.connection_count().await, 1);
        // User index for the gone connection is cleaned: to_user(1) → 0.
        let n = reg
            .dispatch(
                &TargetKind::User(1),
                Event {
                    event: "e".into(),
                    data: serde_json::Value::Null,
                },
            )
            .await;
        assert_eq!(n, 0, "deregister removed user 1 from the index");
        // The group still has the anonymous connection.
        let n = reg
            .dispatch(
                &TargetKind::Group("g".into()),
                Event {
                    event: "e".into(),
                    data: serde_json::Value::Null,
                },
            )
            .await;
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn join_and_leave_update_group_membership() {
        let reg = Registry::default();
        let (a, _rx) = reg.register(Some(1), groups(&[]), DEFAULT_BUFFER).await;

        reg.join(a, "room:5").await;
        let n = reg
            .dispatch(&TargetKind::Group("room:5".into()), evt())
            .await;
        assert_eq!(n, 1, "joined the room");

        reg.leave(a, "room:5").await;
        let n = reg
            .dispatch(&TargetKind::Group("room:5".into()), evt())
            .await;
        assert_eq!(n, 0, "left the room");
    }

    #[tokio::test]
    async fn in_process_broker_publishes_to_the_registry() {
        let registry = Arc::new(Registry::default());
        let (_id, mut rx) = registry
            .register(Some(3), groups(&[]), DEFAULT_BUFFER)
            .await;
        let broker = InProcessBroker::new(registry.clone());

        broker
            .publish(Envelope {
                target: TargetKind::User(3),
                event: "hello".into(),
                data: serde_json::json!({"ok": true}),
            })
            .await;

        let got = recv(&mut rx).await.expect("event delivered via broker");
        assert_eq!(got.event, "hello");
    }

    #[test]
    fn default_group_policy_allows_only_public() {
        let p = PublicGroupsOnly;
        assert!(p.can_join(Some(1), "public:lobby"));
        assert!(!p.can_join(Some(1), "tenant:99"));
        assert!(!p.can_join(None, "chat:1"));
    }

    fn evt() -> Event {
        Event {
            event: "e".into(),
            data: serde_json::Value::Null,
        }
    }
}

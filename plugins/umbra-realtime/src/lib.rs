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
//! This file is **phase 1**: the connection registry, the broker seam
//! (`InProcessBroker` for single-instance; a `RedisBroker` is the
//! documented multi-instance swap), and the ambient [`Realtime`] handle.
//! The SSE / WebSocket transports + the signals bridge land in phases 2–4
//! (see `docs/superpowers/specs/2026-06-13-umbra-realtime-design.md`).
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

mod sse;

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
}

impl Realtime {
    fn get() -> &'static Realtime {
        REALTIME
            .get()
            .expect("umbra-realtime: RealtimePlugin is not installed")
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
    pub async fn send<T: Serialize>(self, event: &str, data: &T) {
        let data = serde_json::to_value(data).unwrap_or(serde_json::Value::Null);
        Realtime::get()
            .broker
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
}

impl Default for RealtimePlugin {
    fn default() -> Self {
        Self {
            policy: Arc::new(PublicGroupsOnly),
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
}

impl Plugin for RealtimePlugin {
    fn name(&self) -> &'static str {
        "realtime"
    }

    fn routes(&self) -> umbra::web::Router {
        umbra::web::Router::new().route("/realtime/sse", umbra::web::get(sse::sse_handler))
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        let registry = Arc::new(Registry::default());
        let broker: Arc<dyn Broker> = Arc::new(InProcessBroker::new(registry.clone()));
        let _ = REALTIME.set(Realtime {
            broker,
            registry,
            policy: self.policy.clone(),
        });
        tracing::info!("realtime: in-process broker ready; SSE at /realtime/sse");
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

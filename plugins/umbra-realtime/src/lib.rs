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
//! Shipped: the connection registry + broker seam ([`InProcessBroker`]
//! for single-instance, [`RedisBroker`] for multi-instance pub/sub), the
//! ambient [`Realtime`] handle, both transports — SSE (`GET /realtime/sse`,
//! push-only) and WebSocket (`GET /realtime/ws`, bidirectional with a
//! [`MessageHandler`] for inbound frames) — and the signals bridge
//! ([`on_model`](RealtimePlugin::on_model), zero-poll model-change fan-out).
//! Full design: `docs/superpowers/specs/2026-06-13-umbra-realtime-design.md`.
//!
//! ## Why a broker
//!
//! `to_user(42)` only reaches user 42 if the message lands on the process
//! that owns their socket. One process needs nothing; to scale out, point
//! [`RealtimePlugin::redis`] at a shared Redis and every targeted send
//! relays to whichever instance holds the socket — the [`Realtime`] API is
//! identical, only the [`Broker`] swaps. Requires the `redis` feature.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use http::HeaderMap;
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, mpsc};
use umbra::plugin::{AppContext, Plugin, PluginError};

/// Resolves the authenticated user's `i64` id from request headers. Returns
/// `None` for anonymous / unauthenticated requests.
///
/// The default resolver (set by [`RealtimePlugin::default`]) always returns
/// `None` — every connection is anonymous — so a push-only feed compiles with
/// no auth dependency. Wire a real resolver via
/// [`RealtimePlugin::identity_resolver`], or use the convenience method
/// [`RealtimePlugin::with_auth_sessions`] (requires the `auth` feature) to
/// plug in `umbra-auth`'s session-cookie lookup.
pub type IdentityResolver =
    Arc<dyn Fn(HeaderMap) -> Pin<Box<dyn Future<Output = Option<i64>> + Send>> + Send + Sync>;

/// Re-export so a `MessageHandler` impl can name the attribute
/// (`#[umbra_realtime::async_trait]`) without a direct `async-trait` dep.
pub use async_trait::async_trait;

mod assets;
mod sse;
mod ws;

/// A unique id per open connection (one socket).
pub type ConnId = u64;

/// Default per-connection outbound buffer. A connection that can't keep
/// up (its buffer fills) drops events rather than blocking the sender —
/// best-effort delivery, never back-pressure onto a request handler.
pub const DEFAULT_BUFFER: usize = 64;

/// Default replay-buffer capacity: the number of recent delivered events
/// the [`Registry`] keeps so a briefly-disconnected SSE client can catch
/// up on reconnect via `Last-Event-ID`. Override with
/// [`RealtimePlugin::replay_buffer`].
pub const DEFAULT_REPLAY_BUFFER: usize = 1024;

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
    /// The channel this event was addressed to, stamped by
    /// [`Registry::dispatch`] from the [`TargetKind`]: a group is its own name
    /// (`"chat:1"`), a user is `"@user:{id}"`, and a broadcast is
    /// `"@broadcast"`. The SSE transport carries this in the enveloped frame so
    /// the single shared `EventSource` can route each event to the right tabs.
    /// Empty until `dispatch` stamps it.
    pub channel: String,
    /// Process-global monotonic sequence id, assigned by
    /// [`Registry::dispatch`] when the event is delivered. The SSE transport
    /// renders this as the event's `id:` line so a browser's `EventSource`
    /// echoes it back as `Last-Event-ID` on reconnect, driving the replay
    /// buffer. Events constructed by hand (tests, the broker before
    /// dispatch) carry `0` until `dispatch` stamps the real id.
    pub seq: u64,
}

/// Who an [`Event`] is addressed to.
///
/// `Serialize`/`Deserialize` so an [`Envelope`] can cross a process
/// boundary — the multi-instance [`RedisBroker`] ships it as JSON over
/// pub/sub.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetKind {
    /// Every live connection authenticated as this user id.
    User(i64),
    /// Every connection that has joined this group/room.
    Group(String),
    /// Every connection.
    Broadcast,
}

impl TargetKind {
    /// The channel string stamped on every [`Event`] this target dispatches,
    /// so the single shared SSE `EventSource` can route the enveloped frame to
    /// the interested tabs: a group is its own name, a user is `"@user:{id}"`,
    /// and a broadcast is `"@broadcast"`.
    pub fn channel(&self) -> String {
        match self {
            TargetKind::Group(g) => g.clone(),
            TargetKind::User(uid) => format!("@user:{uid}"),
            TargetKind::Broadcast => "@broadcast".to_string(),
        }
    }
}

/// A message published to the [`Broker`]: a target + the event to deliver.
///
/// `Serialize`/`Deserialize` is the wire format the [`RedisBroker`] uses
/// to relay a send to every instance.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
///
/// Also owns the **monotonic event sequence** and the **replay buffer** (a
/// bounded ring of the most recent `(seq, Envelope)`), so a reconnecting
/// SSE client can replay the events it missed during a brief drop, and an
/// optional **aggregate connection cap**.
pub struct Registry {
    inner: RwLock<RegistryInner>,
    /// Process-global monotonic event counter. Each [`dispatch`](Self::dispatch)
    /// claims the next id and stamps it on every delivered [`Event`].
    seq: AtomicU64,
    /// Recent `(seq, Envelope)`, oldest→newest, capped at `replay_cap`. A
    /// plain `Mutex` (never held across `.await`): pushes/reads are O(1) /
    /// O(buffer) and synchronous.
    replay: Mutex<VecDeque<(u64, Envelope)>>,
    /// Replay-buffer capacity (events retained). `0` disables replay.
    replay_cap: usize,
    /// Aggregate live-connection cap. `None` = unlimited; when set,
    /// [`register`](Self::register) refuses once `connection_count >= max`.
    max_connections: Option<usize>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new(DEFAULT_REPLAY_BUFFER, None)
    }
}

impl Registry {
    /// Build a registry with an explicit replay-buffer capacity and an
    /// optional aggregate connection cap. [`Registry::default`] uses
    /// [`DEFAULT_REPLAY_BUFFER`] and no cap.
    pub fn new(replay_cap: usize, max_connections: Option<usize>) -> Self {
        Self {
            inner: RwLock::new(RegistryInner::default()),
            seq: AtomicU64::new(0),
            replay: Mutex::new(VecDeque::new()),
            replay_cap,
            max_connections,
        }
    }

    /// Register a new connection. Returns its [`ConnId`] and the receiving
    /// half of its outbound channel (the transport turns this into the
    /// SSE/WS stream), or `None` when the aggregate connection cap
    /// ([`max_connections`](RealtimePlugin::max_connections)) is reached —
    /// the transports turn `None` into a `503 Service Unavailable`. `user_id`
    /// is the authenticated identity (or `None` for anonymous); `groups` are
    /// the rooms it joined at handshake.
    pub async fn register(
        &self,
        user_id: Option<i64>,
        groups: HashSet<String>,
        buffer: usize,
    ) -> Option<(ConnId, mpsc::Receiver<Event>)> {
        let mut inner = self.inner.write().await;
        if let Some(max) = self.max_connections
            && inner.conns.len() >= max
        {
            return None;
        }
        let id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(buffer.max(1));
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
        Some((id, rx))
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
    ///
    /// Assigns the event a process-global monotonic [`seq`](Event::seq) and
    /// records `(seq, Envelope)` in the bounded replay buffer so a
    /// reconnecting SSE client can catch up on what it missed.
    pub async fn dispatch(&self, target: &TargetKind, mut event: Event) -> usize {
        // Claim the next monotonic id and stamp it on the event so every
        // matching connection (and the replay buffer) sees the same seq.
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        event.seq = seq;
        // Tag the event with its channel so the single shared SSE EventSource
        // can route the enveloped frame to the interested tabs. The same event
        // goes to one target, so stamp once.
        event.channel = target.channel();

        // Record in the replay buffer (synchronous; the lock is never held
        // across an `.await`). Cap 0 disables replay entirely.
        if self.replay_cap > 0 {
            let env = Envelope {
                target: target.clone(),
                event: event.event.clone(),
                data: event.data.clone(),
            };
            let mut buf = self.replay.lock().expect("replay mutex poisoned");
            buf.push_back((seq, env));
            while buf.len() > self.replay_cap {
                buf.pop_front();
            }
        }

        // Snapshot-then-send: resolve the target to a list of cloned senders
        // under the read lock, then DROP the lock before any send. Holding the
        // registry read lock across the `try_send` loop would make every
        // register/unregister (which take the write lock) wait behind a
        // broadcast; cloning the `mpsc::Sender`s (cheap — an Arc bump each) and
        // releasing the guard keeps the registry available throughout the send.
        let senders: Vec<mpsc::Sender<Event>> = {
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
            ids.into_iter()
                .filter_map(|id| inner.conns.get(&id).map(|entry| entry.tx.clone()))
                .collect()
        };

        // Lock is dropped. `try_send` stays non-blocking: a connection whose
        // bounded channel is full drops this one message (correct per-conn
        // backpressure) and never stalls the broadcaster.
        let mut delivered = 0;
        for tx in senders {
            if tx.try_send(event.clone()).is_ok() {
                delivered += 1;
            }
        }
        delivered
    }

    /// Replay buffered events with `seq > last_event_id` that a connection
    /// with this `user_id` / `groups` *would have received*, oldest→newest.
    /// Called by the SSE transport on a reconnect carrying `Last-Event-ID`,
    /// before attaching the live receiver, so the client fills the gap with
    /// no missed events — bounded by the replay buffer (events evicted from
    /// it are unrecoverable; see the bounded-buffer caveat in the docs).
    ///
    /// Each returned [`Event`] carries its **original** `seq` so the stream
    /// re-stamps the same `id:` line.
    pub fn replay_since(
        &self,
        last_event_id: u64,
        user_id: Option<i64>,
        groups: &HashSet<String>,
    ) -> Vec<Event> {
        let buf = self.replay.lock().expect("replay mutex poisoned");
        buf.iter()
            .filter(|(seq, _)| *seq > last_event_id)
            .filter(|(_, env)| target_matches(&env.target, user_id, groups))
            .map(|(seq, env)| Event {
                event: env.event.clone(),
                data: env.data.clone(),
                channel: env.target.channel(),
                seq: *seq,
            })
            .collect()
    }

    /// Current live connection count (diagnostics / tests).
    pub async fn connection_count(&self) -> usize {
        self.inner.read().await.conns.len()
    }
}

/// Whether an [`Envelope`]'s target would deliver to a connection with this
/// `user_id` / `groups` — the replay-filter predicate, mirroring the
/// [`Registry::dispatch`] index lookup.
fn target_matches(target: &TargetKind, user_id: Option<i64>, groups: &HashSet<String>) -> bool {
    match target {
        TargetKind::User(uid) => user_id == Some(*uid),
        TargetKind::Group(g) => groups.contains(g),
        TargetKind::Broadcast => true,
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
                    channel: String::new(),
                    seq: 0,
                },
            )
            .await;
    }
}

/// Multi-instance broker over Redis pub/sub (P6 phase 5). Requires the
/// `redis` cargo feature.
///
/// Every instance runs one background pump that does two things on a
/// shared channel (`umbra:realtime:events`):
///   * PUBLISHes the envelopes this instance's handlers produce, and
///   * SUBSCRIBEs and dispatches every envelope (including its own) to the
///     LOCAL [`Registry`].
///
/// So `Realtime::to_user(42).send(...)` reaches user 42's socket no matter
/// which instance holds it — the [`Realtime`] API is unchanged; only the
/// broker swaps. The pump reconnects with a fixed backoff if Redis drops.
///
/// Note the originating instance also delivers via its own subscription
/// (not a direct local dispatch), so a connection is never double-served.
#[cfg(feature = "redis")]
pub struct RedisBroker {
    tx: tokio::sync::mpsc::UnboundedSender<Envelope>,
}

#[cfg(feature = "redis")]
impl RedisBroker {
    /// The pub/sub channel every umbra instance shares.
    const CHANNEL: &'static str = "umbra:realtime:events";

    /// Connect to `url` and spawn the background pump (publish + subscribe).
    /// Spawned rather than awaited so it slots into the synchronous
    /// `Plugin::on_ready`; the pump owns the connections.
    pub fn start(url: String, registry: Arc<Registry>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(Self::pump(url, registry, rx));
        Self { tx }
    }

    /// Reconnect loop around [`run_once`](Self::run_once). Returns only when
    /// the outbound channel closes (the plugin/process is shutting down).
    async fn pump(
        url: String,
        registry: Arc<Registry>,
        mut rx: tokio::sync::mpsc::UnboundedReceiver<Envelope>,
    ) {
        loop {
            match Self::run_once(&url, &registry, &mut rx).await {
                Ok(()) => return,
                Err(err) => {
                    tracing::warn!("realtime redis broker: {err}; reconnecting in 1s");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    }

    /// One connection's lifetime: PUBLISH outbound envelopes and dispatch
    /// inbound ones until the channel closes (`Ok`) or Redis errors (`Err`,
    /// triggering a reconnect).
    async fn run_once(
        url: &str,
        registry: &Arc<Registry>,
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<Envelope>,
    ) -> Result<(), redis::RedisError> {
        use futures_util::StreamExt;

        let client = redis::Client::open(url)?;
        let mut publisher = redis::aio::ConnectionManager::new(client.clone()).await?;
        let mut pubsub = client.get_async_pubsub().await?;
        pubsub.subscribe(Self::CHANNEL).await?;
        let mut messages = pubsub.on_message();

        loop {
            tokio::select! {
                outbound = rx.recv() => {
                    let Some(env) = outbound else {
                        return Ok(()); // channel closed → shut the pump down
                    };
                    if let Ok(json) = serde_json::to_string(&env) {
                        // A publish failure surfaces as a stream error / the
                        // next command erroring; ConnectionManager also
                        // reconnects under us. Swallow the per-publish result.
                        let _ = redis::cmd("PUBLISH")
                            .arg(Self::CHANNEL)
                            .arg(json)
                            .query_async::<i64>(&mut publisher)
                            .await;
                    }
                }
                inbound = messages.next() => {
                    let Some(msg) = inbound else {
                        // Subscription stream ended → reconnect.
                        return Err(redis::RedisError::from((
                            redis::ErrorKind::IoError,
                            "realtime pub/sub stream closed",
                        )));
                    };
                    let payload: String = msg.get_payload().unwrap_or_default();
                    if let Ok(env) = serde_json::from_str::<Envelope>(&payload) {
                        registry
                            .dispatch(
                                &env.target,
                                Event { event: env.event, data: env.data, channel: String::new(), seq: 0 },
                            )
                            .await;
                    }
                }
            }
        }
    }
}

#[cfg(feature = "redis")]
#[async_trait::async_trait]
impl Broker for RedisBroker {
    async fn publish(&self, env: Envelope) {
        // Hand off to the pump without blocking the request handler. If the
        // pump is gone (process shutting down) the send is silently dropped.
        let _ = self.tx.send(env);
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
    resolver: IdentityResolver,
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

    /// The identity resolver: maps request headers to an authenticated user's
    /// `i64` id (or `None` for anonymous). Used by both transports at handshake
    /// to populate the registry entry and pass the user id to the group policy.
    pub fn resolver() -> IdentityResolver {
        Self::get().resolver.clone()
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
    /// How to resolve the authenticated user id from request headers at the
    /// SSE/WS handshake. Defaults to always-`None` (anonymous). Override via
    /// [`identity_resolver`](Self::identity_resolver) or the convenience
    /// [`with_auth_sessions`](Self::with_auth_sessions) (requires feature `auth`).
    resolver: IdentityResolver,
    /// Deferred signal-subscription registrations (one per `on_table` /
    /// `on_model` call). Run once at `on_ready` so they only fire when the
    /// plugin is actually installed.
    subscriptions: Vec<Box<dyn Fn() + Send + Sync>>,
    /// When set (and the `redis` feature is on), boot a [`RedisBroker`]
    /// instead of the single-instance [`InProcessBroker`]. Configured via
    /// [`redis`](Self::redis).
    redis_url: Option<String>,
    /// Replay-buffer capacity (recent events retained for `Last-Event-ID`
    /// reconnect resume). Defaults to [`DEFAULT_REPLAY_BUFFER`]; set via
    /// [`replay_buffer`](Self::replay_buffer).
    replay_cap: usize,
    /// Aggregate live-connection cap across all transports. `None` =
    /// unlimited (the default); set via
    /// [`max_connections`](Self::max_connections).
    max_connections: Option<usize>,
}

/// The no-op identity resolver: every connection is anonymous (`None`).
/// This is the default so anonymous / push-only feeds require no auth dep.
fn anonymous_resolver() -> IdentityResolver {
    Arc::new(|_headers: HeaderMap| Box::pin(async { None }))
}

impl Default for RealtimePlugin {
    fn default() -> Self {
        Self {
            policy: Arc::new(PublicGroupsOnly),
            handler: Arc::new(NoopMessageHandler),
            resolver: anonymous_resolver(),
            subscriptions: Vec::new(),
            redis_url: None,
            replay_cap: DEFAULT_REPLAY_BUFFER,
            max_connections: None,
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

    /// Supply a custom identity resolver: an async function that maps the
    /// request headers to the authenticated user's `i64` id (or `None` for
    /// anonymous). This is the extension point for custom auth schemes (JWT,
    /// API keys, etc.) without pulling in `umbra-auth`.
    ///
    /// For session-cookie auth backed by `umbra-auth`, use the convenience
    /// method [`with_auth_sessions`](Self::with_auth_sessions) (requires the
    /// `auth` cargo feature).
    pub fn identity_resolver<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(HeaderMap) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<i64>> + Send + 'static,
    {
        self.resolver = Arc::new(move |h| Box::pin(f(h)));
        self
    }

    /// Use `umbra-auth`'s session-cookie resolver to identify the current
    /// user at the SSE/WS handshake. This is the standard wiring when
    /// `umbra-auth` is installed.
    ///
    /// Requires the `auth` cargo feature (`--features auth`).
    #[cfg(feature = "auth")]
    pub fn with_auth_sessions(self) -> Self {
        self.identity_resolver(|headers| async move {
            umbra_auth::current_session_user_id(&headers).await
        })
    }

    /// Set the inbound-message handler for the WebSocket transport. The
    /// default ([`NoopMessageHandler`]) ignores client frames (push-only).
    pub fn message_handler<H: MessageHandler + 'static>(mut self, handler: H) -> Self {
        self.handler = Arc::new(handler);
        self
    }

    /// Size the **replay buffer** — how many recent delivered events the
    /// registry retains so a reconnecting SSE client can resume from its
    /// `Last-Event-ID` with no gap. Defaults to [`DEFAULT_REPLAY_BUFFER`]
    /// (1024). Set `0` to disable replay (live-only).
    ///
    /// The buffer is bounded: an event evicted before a client reconnects
    /// is unrecoverable, so the client resumes from the oldest *retained*
    /// event and silently misses anything older. Size it to cover the
    /// longest drop you want to bridge times your peak event rate.
    pub fn replay_buffer(mut self, n: usize) -> Self {
        self.replay_cap = n;
        self
    }

    /// Cap the **aggregate** number of live connections across SSE *and*
    /// WebSocket. Once reached, a new handshake is refused with
    /// `503 Service Unavailable` instead of opening the stream. `None` (the
    /// default) is unlimited. A freed slot (a disconnect) immediately admits
    /// the next connection.
    pub fn max_connections(mut self, n: usize) -> Self {
        self.max_connections = Some(n);
        self
    }

    /// Scale horizontally: relay targeted sends through a Redis pub/sub
    /// backplane so `to_user` / `to_group` / `broadcast` reach sockets held
    /// by *other* instances (P6 phase 5). Without this, each instance only
    /// serves the connections it holds — correct for a single process.
    ///
    /// Requires the `redis` cargo feature (`--features redis`). The same
    /// `url` must point every instance at one Redis. The [`Realtime`] API is
    /// unchanged; only the broker swaps.
    #[cfg(feature = "redis")]
    pub fn redis(mut self, url: impl Into<String>) -> Self {
        self.redis_url = Some(url.into());
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

    /// Pick the broker at boot: a [`RedisBroker`] when a URL is configured
    /// and the `redis` feature is on, else the single-instance
    /// [`InProcessBroker`]. A URL set without the feature warns and falls
    /// back rather than silently scaling to one instance.
    fn build_broker(&self, registry: Arc<Registry>) -> Arc<dyn Broker> {
        #[cfg(feature = "redis")]
        if let Some(url) = self.redis_url.clone() {
            tracing::info!("realtime: redis broker backplane → {url}");
            return Arc::new(RedisBroker::start(url, registry));
        }
        #[cfg(not(feature = "redis"))]
        if self.redis_url.is_some() {
            tracing::warn!(
                "realtime: a redis url is set but the `redis` feature is off; \
                 using the single-instance in-process broker"
            );
        }
        Arc::new(InProcessBroker::new(registry))
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
            .route(
                "/realtime/worker.js",
                umbra::web::get(assets::worker_js_handler),
            )
            .route(
                "/realtime/client.js",
                umbra::web::get(assets::client_js_handler),
            )
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        let registry = Arc::new(Registry::new(self.replay_cap, self.max_connections));
        let broker: Arc<dyn Broker> = self.build_broker(registry.clone());
        let _ = REALTIME.set(Realtime {
            broker,
            registry,
            policy: self.policy.clone(),
            handler: self.handler.clone(),
            resolver: self.resolver.clone(),
        });
        // Register the model-change subscriptions now that the ambient
        // handle exists (a fired handler calls Realtime::to_group, etc.).
        for register in &self.subscriptions {
            register();
        }
        tracing::info!("realtime: SSE at /realtime/sse, WS at /realtime/ws");
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

    fn reg_event(event: &str, data: serde_json::Value) -> Event {
        Event {
            event: event.into(),
            data,
            channel: String::new(),
            seq: 0,
        }
    }

    #[tokio::test]
    async fn to_user_reaches_every_connection_of_that_user() {
        let reg = Registry::default();
        let (_a, mut rx_a) = reg
            .register(Some(7), groups(&[]), DEFAULT_BUFFER)
            .await
            .unwrap();
        let (_b, mut rx_b) = reg
            .register(Some(7), groups(&[]), DEFAULT_BUFFER)
            .await
            .unwrap();
        let (_c, mut rx_c) = reg
            .register(Some(9), groups(&[]), DEFAULT_BUFFER)
            .await
            .unwrap();

        let n = reg
            .dispatch(&TargetKind::User(7), reg_event("ping", serde_json::json!({"x": 1})))
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
            .await
            .unwrap();
        let (_b, mut rx_b) = reg
            .register(Some(2), groups(&["chat:2"]), DEFAULT_BUFFER)
            .await
            .unwrap();

        let n = reg
            .dispatch(
                &TargetKind::Group("chat:1".into()),
                reg_event("message", serde_json::json!("hi")),
            )
            .await;
        assert_eq!(n, 1);
        assert!(recv(&mut rx_a).await.is_some());
        assert!(recv(&mut rx_b).await.is_none());
    }

    #[tokio::test]
    async fn broadcast_reaches_all_and_deregister_cleans_indexes() {
        let reg = Registry::default();
        let (a, _rx_a) = reg
            .register(Some(1), groups(&["g"]), DEFAULT_BUFFER)
            .await
            .unwrap();
        let (_b, _rx_b) = reg
            .register(None, groups(&["g"]), DEFAULT_BUFFER)
            .await
            .unwrap();

        assert_eq!(reg.connection_count().await, 2);
        let n = reg
            .dispatch(&TargetKind::Broadcast, evt())
            .await;
        assert_eq!(n, 2, "broadcast hit both");

        reg.deregister(a).await;
        assert_eq!(reg.connection_count().await, 1);
        // User index for the gone connection is cleaned: to_user(1) → 0.
        let n = reg
            .dispatch(&TargetKind::User(1), evt())
            .await;
        assert_eq!(n, 0, "deregister removed user 1 from the index");
        // The group still has the anonymous connection.
        let n = reg
            .dispatch(&TargetKind::Group("g".into()), evt())
            .await;
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn broadcast_delivers_on_every_receiver_after_snapshot_then_send() {
        // Snapshot-then-send must still reach every registered connection:
        // register 3, broadcast once, and read the event off each receiver.
        let reg = Registry::default();
        let (_a, mut rx_a) = reg.register(Some(1), groups(&[]), DEFAULT_BUFFER).await.unwrap();
        let (_b, mut rx_b) = reg.register(Some(2), groups(&[]), DEFAULT_BUFFER).await.unwrap();
        let (_c, mut rx_c) = reg.register(None, groups(&["g"]), DEFAULT_BUFFER).await.unwrap();

        let n = reg
            .dispatch(&TargetKind::Broadcast, reg_event("hi", serde_json::json!({"x": 1})))
            .await;
        assert_eq!(n, 3, "broadcast queued to all three");
        assert!(recv(&mut rx_a).await.is_some(), "conn a received");
        assert!(recv(&mut rx_b).await.is_some(), "conn b received");
        assert!(recv(&mut rx_c).await.is_some(), "conn c received");
    }

    #[tokio::test]
    async fn join_and_leave_update_group_membership() {
        let reg = Registry::default();
        let (a, _rx) = reg
            .register(Some(1), groups(&[]), DEFAULT_BUFFER)
            .await
            .unwrap();

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
            .await
            .unwrap();
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
    fn target_channel_stamps_group_user_broadcast() {
        assert_eq!(TargetKind::Group("chat:1".into()).channel(), "chat:1");
        assert_eq!(TargetKind::User(42).channel(), "@user:42");
        assert_eq!(TargetKind::Broadcast.channel(), "@broadcast");
    }

    #[tokio::test]
    async fn dispatch_stamps_the_channel_on_delivered_events() {
        let reg = Registry::default();
        let (_g, mut rx_g) = reg
            .register(Some(5), groups(&["chat:1"]), DEFAULT_BUFFER)
            .await
            .unwrap();
        reg.dispatch(
            &TargetKind::Group("chat:1".into()),
            reg_event("message", serde_json::json!("hi")),
        )
        .await;
        assert_eq!(recv(&mut rx_g).await.unwrap().channel, "chat:1");

        let (_u, mut rx_u) = reg
            .register(Some(7), groups(&[]), DEFAULT_BUFFER)
            .await
            .unwrap();
        reg.dispatch(&TargetKind::User(7), reg_event("ping", serde_json::json!({})))
            .await;
        assert_eq!(recv(&mut rx_u).await.unwrap().channel, "@user:7");

        let (_b, mut rx_b) = reg
            .register(None, groups(&[]), DEFAULT_BUFFER)
            .await
            .unwrap();
        reg.dispatch(&TargetKind::Broadcast, reg_event("all", serde_json::json!({})))
            .await;
        // Drain other receivers' broadcast copies are irrelevant; check b.
        assert_eq!(recv(&mut rx_b).await.unwrap().channel, "@broadcast");
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
            channel: String::new(),
            seq: 0,
        }
    }
}

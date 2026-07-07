//! umbral-realtime — real-time push over SSE + WebSocket.
//!
//! Push data to **one user** or to a **named group/room** without
//! hand-rolling connection bookkeeping:
//!
//! ```ignore
//! use umbral_realtime::Realtime;
//!
//! Realtime::to_user("42").send("notification", &payload).await;
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
//! Full design: `docs/superpowers/specs/2026-06-13-umbral-realtime-design.md`.
//!
//! ## Why a broker
//!
//! `to_user("42")` only reaches user 42 if the message lands on the process
//! that owns their socket. One process needs nothing; to scale out, point
//! [`RealtimePlugin::redis`] at a shared Redis and every targeted send
//! relays to whichever instance holds the socket — the [`Realtime`] API is
//!
//! Connection **identity is an opaque string** — the user's primary key
//! rendered to its canonical [`Display`](std::fmt::Display) form, so it works
//! for every PK type (`i64` → `"42"`, `uuid::Uuid` → `"…"`, `String` → itself).
//! [`to_user`](Realtime::to_user) takes that string; [`with_auth_sessions`] resolves
//! the session user's PK to the *same* string, so targeting lines up.
//! identical, only the [`Broker`] swaps. Requires the `redis` feature.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use http::HeaderMap;
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, mpsc};
use umbral::plugin::{AppContext, Plugin, PluginError};

/// Resolves the authenticated user's identity string from request headers.
/// Returns `None` for anonymous / unauthenticated requests.
///
/// The identity is the user's **primary key rendered to its canonical
/// [`Display`](std::fmt::Display) string** — PK-type-agnostic, so an `i64`,
/// `String`, or `uuid::Uuid` PK all resolve to the same opaque key the dev
/// passes to [`Realtime::to_user`].
///
/// The default resolver (set by [`RealtimePlugin::default`]) always returns
/// `None` — every connection is anonymous — so a push-only feed compiles with
/// no auth dependency. Wire a real resolver via
/// [`RealtimePlugin::identity_resolver`], or use the convenience method
/// [`RealtimePlugin::with_auth_sessions`] (requires the `auth` feature) to
/// plug in `umbral-auth`'s session-cookie lookup.
pub type IdentityResolver =
    Arc<dyn Fn(HeaderMap) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>;

/// Re-export so a `MessageHandler` impl can name the attribute
/// (`#[umbral_realtime::async_trait]`) without a direct `async-trait` dep.
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

/// Default cap on an inbound WebSocket message (and frame), in bytes: 1 MiB.
/// Without an explicit cap the WS stack accepts messages up to 64 MiB, which
/// lets a single client force large allocations (memory DoS). 1 MiB is far
/// above any sane chat/command payload while bounding the per-frame
/// allocation; raise it per app via
/// [`RealtimePlugin::ws_max_message_bytes`].
pub const DEFAULT_WS_MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// Default aggregate live-connection cap across SSE + WebSocket (audit_2
/// realtime #4). Previously unlimited, which let a handful of clients exhaust
/// file descriptors / memory. 10k concurrent connections is generous for a
/// single node yet bounds the blast radius; raise it with
/// [`RealtimePlugin::max_connections`] or opt back into unlimited with
/// [`RealtimePlugin::unlimited_connections`].
pub const DEFAULT_MAX_CONNECTIONS: usize = 10_000;

/// Default per-connection inbound-message rate cap, messages per second
/// (audit_2 realtime #4). A client that sustains more than this floods the
/// connection; the socket is closed. Generous for chat/command traffic; tune
/// with [`RealtimePlugin::ws_max_messages_per_sec`].
pub const DEFAULT_WS_MAX_MESSAGES_PER_SEC: u32 = 100;

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
    /// Every live connection authenticated as this user id (the PK string).
    User(String),
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
    user_id: Option<String>,
    groups: HashSet<String>,
}

#[derive(Default)]
struct RegistryInner {
    conns: HashMap<ConnId, ConnEntry>,
    by_user: HashMap<String, HashSet<ConnId>>,
    by_group: HashMap<String, HashSet<ConnId>>,
}

/// The per-group presence transitions a single connection's register /
/// deregister produced, computed atomically under the registry lock so the
/// caller can dispatch them after dropping the lock.
///
/// Dedup is by `user_id`: `joined` lists `(group, user_id)` where this conn was
/// the user's **first** in that group; `left` lists groups where it was their
/// **last**; `sync` carries, for each newly-entered presence group, the full
/// set of present (deduped) user ids — delivered to the joining conn so it sees
/// who's already there. Anonymous connections produce no transitions.
#[derive(Default, Debug)]
pub struct PresenceTransitions {
    /// `(group, user_id)` for each group this user FIRST entered via this conn.
    pub joined: Vec<(String, String)>,
    /// `(group, user_id)` for each group this user FULLY LEFT via this conn.
    pub left: Vec<(String, String)>,
    /// `(group, present_user_ids)` snapshot per newly-entered group, for sync.
    pub sync: Vec<(String, Vec<String>)>,
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
        user_id: Option<String>,
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
        if let Some(uid) = &user_id {
            inner.by_user.entry(uid.clone()).or_default().insert(id);
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

    /// Like [`register`](Self::register), but also returns the per-group
    /// **presence transitions** this connection caused, computed atomically
    /// under the same write lock. A `(group, user_id)` lands in
    /// [`joined`](PresenceTransitions::joined) only when this conn is the user's
    /// FIRST in `group` (dedup by user); each newly-entered group also yields a
    /// `sync` snapshot of the present (deduped) user ids in that group.
    ///
    /// Anonymous connections (`user_id == None`) produce no transitions — they
    /// never appear in presence. The caller dispatches the returned transitions
    /// after this returns (the lock is dropped), keeping sends off the lock.
    pub async fn register_with_presence(
        &self,
        user_id: Option<String>,
        groups: HashSet<String>,
        buffer: usize,
    ) -> Option<(ConnId, mpsc::Receiver<Event>, PresenceTransitions)> {
        let mut inner = self.inner.write().await;
        if let Some(max) = self.max_connections
            && inner.conns.len() >= max
        {
            return None;
        }
        let id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(buffer.max(1));

        // First-join per group is computed BEFORE this conn is indexed: the
        // user just entered `group` iff they had no OTHER live conn in it.
        let mut transitions = PresenceTransitions::default();
        if let Some(uid) = &user_id {
            for g in &groups {
                if !user_in_group(&inner, uid, g) {
                    transitions.joined.push((g.clone(), uid.clone()));
                }
            }
        }

        if let Some(uid) = &user_id {
            inner.by_user.entry(uid.clone()).or_default().insert(id);
        }
        for g in &groups {
            inner.by_group.entry(g.clone()).or_default().insert(id);
        }
        inner.conns.insert(
            id,
            ConnEntry {
                tx,
                user_id,
                groups: groups.clone(),
            },
        );

        // Sync snapshot per newly-entered group: the deduped present user ids,
        // computed AFTER this conn is indexed so the joining user is included.
        for (g, _uid) in &transitions.joined {
            transitions
                .sync
                .push((g.clone(), present_user_ids(&inner, g)));
        }

        Some((id, rx, transitions))
    }

    /// Like [`deregister`](Self::deregister), but also returns the per-group
    /// presence transitions this disconnect caused: a `(group, user_id)` in
    /// [`left`](PresenceTransitions::left) for each group this conn was the
    /// user's LAST in (dedup by user). Anonymous conns produce none. The caller
    /// dispatches after the lock is dropped.
    pub async fn deregister_with_presence(&self, id: ConnId) -> PresenceTransitions {
        let mut inner = self.inner.write().await;
        let mut transitions = PresenceTransitions::default();
        let Some(entry) = inner.conns.remove(&id) else {
            return transitions;
        };
        let user_id = entry.user_id;
        if let Some(uid) = &user_id
            && let Some(set) = inner.by_user.get_mut(uid)
        {
            set.remove(&id);
            if set.is_empty() {
                inner.by_user.remove(uid);
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
        // Last-leave per group: now that this conn is removed, the user fully
        // left `group` iff they have no remaining conn in it.
        if let Some(uid) = &user_id {
            for g in &entry.groups {
                if !user_in_group(&inner, uid, g) {
                    transitions.left.push((g.clone(), uid.clone()));
                }
            }
        }
        transitions
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
        user_id: Option<&str>,
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

/// Whether `user_id` has at least one live connection currently in `group`.
/// Drives presence dedup: a user is "present" in a group iff this is `true`.
fn user_in_group(inner: &RegistryInner, user_id: &str, group: &str) -> bool {
    let Some(group_conns) = inner.by_group.get(group) else {
        return false;
    };
    group_conns.iter().any(|cid| {
        inner
            .conns
            .get(cid)
            .is_some_and(|e| e.user_id.as_deref() == Some(user_id))
    })
}

/// The deduped set of authenticated user ids currently present in `group`
/// (anonymous conns excluded), ascending. The presence sync snapshot.
fn present_user_ids(inner: &RegistryInner, group: &str) -> Vec<String> {
    let Some(group_conns) = inner.by_group.get(group) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = group_conns
        .iter()
        .filter_map(|cid| inner.conns.get(cid).and_then(|e| e.user_id.clone()))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Whether an [`Envelope`]'s target would deliver to a connection with this
/// `user_id` / `groups` — the replay-filter predicate, mirroring the
/// [`Registry::dispatch`] index lookup.
fn target_matches(target: &TargetKind, user_id: Option<&str>, groups: &HashSet<String>) -> bool {
    match target {
        TargetKind::User(uid) => user_id == Some(uid.as_str()),
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
/// shared channel (`umbral:realtime:events`):
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
    /// The pub/sub channel every umbral instance shares.
    const CHANNEL: &'static str = "umbral:realtime:events";

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
    /// `user_id` is the authenticated user's PK string (or `None` for
    /// anonymous) — PK-type-agnostic, so `i64`/`String`/`uuid` PKs all
    /// arrive as the same canonical string. Return `true` to allow the
    /// join. Default: only `public:*` groups.
    fn can_join(&self, user_id: Option<&str>, group: &str) -> bool {
        let _ = user_id;
        group.starts_with("public:")
    }

    /// Decides whether a connection may **send** (broadcast) to a group
    /// (audit_2 realtime #2). A [`MessageHandler`] that forwards to a
    /// client-supplied room MUST authorize it — via [`Realtime::can_send`] —
    /// before broadcasting, or a client joined to one room can inject messages
    /// into ANY group (IDOR). The default mirrors [`Self::can_join`] ("if you
    /// can't join it, you can't send to it"), the safe default; override for
    /// asymmetric rooms (e.g. a read-only broadcast channel, or a room you may
    /// post to but not subscribe).
    fn can_send(&self, user_id: Option<&str>, group: &str) -> bool {
        self.can_join(user_id, group)
    }
}

/// The permissive-for-public default policy.
pub struct PublicGroupsOnly;
impl GroupPolicy for PublicGroupsOnly {}

/// A [`GroupPolicy`] built from a closure — the ergonomic way to gate rooms
/// without declaring a named type. Construct it directly, or use
/// [`RealtimePlugin::group_policy_fn`]. The closure returns `true` to allow
/// the join.
///
/// ```ignore
/// use umbral_realtime::RealtimePlugin;
///
/// RealtimePlugin::new()
///     .with_auth_sessions()                       // so `user_id` is the logged-in user's PK string
///     .group_policy_fn(|user_id, group| {
///         if group.starts_with("public:") { return true; }   // public rooms: anyone
///         match user_id {
///             Some(uid) => group == format!("user:{uid}")     // your own private room
///                 || is_member(uid, group),                   // or your DB membership check
///             None => false,                                  // anonymous: public only
///         }
///     });
/// ```
pub struct FnGroupPolicy<F>(pub F);

impl<F> GroupPolicy for FnGroupPolicy<F>
where
    F: Fn(Option<&str>, &str) -> bool + Send + Sync,
{
    fn can_join(&self, user_id: Option<&str>, group: &str) -> bool {
        (self.0)(user_id, group)
    }
}

// =========================================================================
// Inbound WebSocket messages.
// =========================================================================

/// Who sent an inbound WebSocket message — the connection id + its user.
/// A handler uses this to authorize and route (e.g. join a room, or
/// broadcast back to the sender's group via [`Realtime::to_group`]).
pub struct MessageContext {
    pub conn_id: ConnId,
    /// The authenticated user's PK string (or `None` for anonymous).
    pub user_id: Option<String>,
}

impl MessageContext {
    /// Whether this connection's identity may broadcast to `group`, per the
    /// installed [`GroupPolicy::can_send`]. Sugar for
    /// [`Realtime::can_send`]`(self.user_id.as_deref(), group)`.
    pub fn can_send(&self, group: &str) -> bool {
        Realtime::can_send(self.user_id.as_deref(), group)
    }

    /// **Authorized publish** — the safe-by-default way to broadcast from an
    /// inbound WS handler (audit_2 realtime #2). Broadcasts `data` under
    /// `event` to `group` **only if** this connection passes
    /// [`GroupPolicy::can_send`]; otherwise it drops the frame and returns
    /// `false`. Prefer this over `Realtime::to_group(group).send(...)` for any
    /// **client-supplied** room: the raw `to_group` does no sender check, so a
    /// client joined to one room could otherwise inject into any group (IDOR).
    /// Returns `true` if the message was published.
    ///
    /// ```ignore
    /// async fn on_message(&self, ctx: &MessageContext, text: String) {
    ///     let Ok(msg) = serde_json::from_str::<ChatMsg>(&text) else { return };
    ///     // authorized in one call — no way to forget the send check
    ///     ctx.publish(&msg.room, "message", &msg).await;
    /// }
    /// ```
    pub async fn publish<T: Serialize>(&self, group: &str, event: &str, data: &T) -> bool {
        if !self.can_send(group) {
            return false;
        }
        Realtime::to_group(group).send(event, data).await;
        true
    }
}

/// Handles text frames a WebSocket client sends to the server. SSE is
/// push-only, so this only matters for the WS transport. The default
/// ([`NoopMessageHandler`]) ignores inbound frames; a chat app implements
/// this to broadcast a received message to its room.
///
/// **Inbound frames carry no automatic send-authorization.** The
/// [`GroupPolicy`] gate runs at the *handshake* and decides which groups a
/// connection may *join*; it does **not** run on inbound messages, and the raw
/// [`Realtime::to_group`] delivers to whoever is in the target group regardless
/// of who sent the frame. Treat `text` as untrusted input and publish through
/// [`MessageContext::publish`], which authorizes the sender before it
/// broadcasts — the safe default for a client-supplied room:
///
/// ```ignore
/// async fn on_message(&self, ctx: &MessageContext, text: String) {
///     // Parse defensively — never unwrap attacker-controlled input.
///     let Ok(msg) = serde_json::from_str::<ChatMsg>(&text) else { return };
///     // `publish` runs GroupPolicy::can_send first and drops the frame if the
///     // sender may not post to `msg.room` — no way to forget the check
///     // (audit_2 realtime #2). For the raw broadcast, gate it yourself with
///     // `ctx.can_send(&msg.room)` before `Realtime::to_group(...).send(...)`.
///     ctx.publish(&msg.room, "message", &msg).await;
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

/// Render a JSON primary-key value to its canonical [`Display`] string, the
/// same form [`Realtime::to_user`] and the identity resolver use. A JSON
/// number renders as its digits (`42` → `"42"`), a JSON string yields its
/// inner text *unquoted* (`"a-uuid"` → `"a-uuid"`); `null` / a JSON object or
/// array (never a PK) yields `None`. This keeps a model row's PK and a
/// `to_user(pk.to_string())` target keyed identically across PK types.
fn json_pk_to_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Null => None,
        // bool is not a PK but render it deterministically rather than drop it.
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

impl ModelEvent {
    /// The row's `id`, if present and integer — the convenience accessor for
    /// the default i64 PK. Returns `None` for a non-integer / non-`id` PK or a
    /// missing id. For a PK-type-agnostic accessor (`i64`/`String`/`uuid`), use
    /// [`pk_str`](Self::pk_str).
    pub fn pk(&self) -> Option<i64> {
        self.instance.get("id").and_then(|v| v.as_i64())
    }

    /// The row's primary key as a **canonical string**, for any PK type
    /// (`i64` → `"42"`, `String` → itself, `uuid::Uuid` → `"…"`). Pulls the
    /// `id` field out of `instance` and renders it the same way
    /// [`Realtime::to_user`] expects, so a per-row group like
    /// `format!("post:{}", ev.pk_str().unwrap_or_default())` lines up with the
    /// PK whatever its type. Returns `None` for a missing / null `id`.
    pub fn pk_str(&self) -> Option<String> {
        self.instance.get("id").and_then(json_pk_to_string)
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

    /// Action name as the wire event the client subscribes on:
    /// `"created" | "updated" | "deleted"`.
    fn action_name(&self) -> &'static str {
        match self.action {
            ModelAction::Created => "created",
            ModelAction::Updated => "updated",
            ModelAction::Deleted => "deleted",
        }
    }
}

// =========================================================================
// Safe model exposure — opt-in, field-whitelisted broadcast.
// =========================================================================

/// How an [`Expose`] picks the group for a given [`ModelEvent`].
enum GroupRoute {
    /// One static group every matching event lands in.
    Static(String),
    /// A per-row group computed from the event (e.g. `format!("post:{}", ev.pk_str()…)`),
    /// so a client can watch a single row.
    Dynamic(Arc<dyn Fn(&ModelEvent) -> String + Send + Sync>),
}

impl GroupRoute {
    fn group_for(&self, ev: &ModelEvent) -> String {
        match self {
            GroupRoute::Static(g) => g.clone(),
            GroupRoute::Dynamic(f) => f(ev),
        }
    }
}

/// Which fields of the row reach the wire.
enum Projection {
    /// Only the primary key (`id`) — the safe default. The payload says
    /// "row N changed; refetch it through your normal authorized endpoint".
    IdOnly,
    /// Exactly these columns, projected out of `instance`. Every other key
    /// (including secrets the dev never listed) is dropped.
    Fields(Vec<String>),
    /// The entire row — the explicit, conspicuous opt-in via
    /// [`Expose::all_fields`]. The dev knowingly accepts broadcasting every column.
    AllFields,
}

/// A safe, opt-in model-change broadcast spec. Build one with
/// [`Expose::to_group`] (or [`to_group_with`](Expose::to_group_with) for a
/// per-row group) and hand it to [`RealtimePlugin::expose`].
///
/// **Nothing is broadcast unless you `expose` it, and only the fields you
/// list.** Safety is the default in three layers:
///
/// 1. **Default-deny model** — a model with no `expose`/`on_model` never fans out.
/// 2. **Default id-only projection** — without [`fields`](Self::fields), the
///    payload carries the PK alone. The client treats it as "something changed,
///    refetch through your authorized endpoint". [`fields`](Self::fields)
///    whitelists exactly the columns to include; [`all_fields`](Self::all_fields)
///    is the explicit, conspicuous opt-in to broadcast the whole row.
/// 3. **Group + policy** — the group you name is governed by
///    [`GroupPolicy::can_join`] at the SSE/WS handshake; a private (non-`public:`)
///    group is unjoinable under the default policy.
///
/// ```ignore
/// RealtimePlugin::new().expose::<Post>(
///     Expose::to_group("public:posts").fields(&["id", "title", "slug"]),
/// );
/// ```
pub struct Expose {
    route: GroupRoute,
    projection: Projection,
    actions: Vec<ModelAction>,
}

impl Expose {
    /// Broadcast matching events to one static `group`. **Required** — the dev
    /// consciously picks the group whose visibility [`GroupPolicy::can_join`]
    /// then governs. Defaults to id-only projection and all three actions.
    pub fn to_group(group: impl Into<String>) -> Self {
        Self {
            route: GroupRoute::Static(group.into()),
            projection: Projection::IdOnly,
            actions: vec![
                ModelAction::Created,
                ModelAction::Updated,
                ModelAction::Deleted,
            ],
        }
    }

    /// Route each event to a per-row group computed from it, so a client can
    /// watch a single row: `Expose::to_group_with(|ev| format!("post:{}", ev.pk_str().unwrap_or_default()))`.
    /// The computed group is still governed by [`GroupPolicy::can_join`].
    pub fn to_group_with<F>(f: F) -> Self
    where
        F: Fn(&ModelEvent) -> String + Send + Sync + 'static,
    {
        Self {
            route: GroupRoute::Dynamic(Arc::new(f)),
            projection: Projection::IdOnly,
            actions: vec![
                ModelAction::Created,
                ModelAction::Updated,
                ModelAction::Deleted,
            ],
        }
    }

    /// Whitelist the columns to include in the broadcast payload. Every other
    /// key in the row — including secrets you never list — is dropped. This is
    /// the core safety control: the wire carries *only* what you name here.
    pub fn fields(mut self, fields: &[&str]) -> Self {
        self.projection = Projection::Fields(fields.iter().map(|s| s.to_string()).collect());
        self
    }

    /// **Explicit opt-in to broadcast the ENTIRE row.** Conspicuously named so
    /// it can't happen by accident: by calling this you knowingly accept that
    /// every column (including any you'd otherwise want to hide) reaches every
    /// subscriber of the group. Prefer [`fields`](Self::fields) (or the id-only
    /// default) unless you genuinely need the whole row on the wire.
    pub fn all_fields(mut self) -> Self {
        self.projection = Projection::AllFields;
        self
    }

    /// Restrict which actions fan out. Default: all three
    /// ([`Created`](ModelAction::Created), [`Updated`](ModelAction::Updated),
    /// [`Deleted`](ModelAction::Deleted)).
    pub fn actions(mut self, actions: &[ModelAction]) -> Self {
        self.actions = actions.to_vec();
        self
    }

    /// Project `instance` down to the whitelist (or id-only / all), per the
    /// projection mode. The returned JSON is exactly what reaches the wire.
    fn project(&self, instance: &serde_json::Value) -> serde_json::Value {
        match &self.projection {
            Projection::AllFields => instance.clone(),
            Projection::IdOnly => {
                let mut out = serde_json::Map::new();
                if let Some(id) = instance.get("id") {
                    out.insert("id".to_string(), id.clone());
                }
                serde_json::Value::Object(out)
            }
            Projection::Fields(fields) => {
                let mut out = serde_json::Map::new();
                if let Some(obj) = instance.as_object() {
                    for f in fields {
                        if let Some(v) = obj.get(f) {
                            out.insert(f.clone(), v.clone());
                        }
                    }
                }
                serde_json::Value::Object(out)
            }
        }
    }
}

// =========================================================================
// Presence — gated "who's online in a group".
// =========================================================================

/// Decides which groups have **presence** ("who's online") enabled, and how a
/// present user's identity is projected onto the wire.
///
/// Presence is **off for every group by default** — a [`RealtimePlugin`] with
/// no [`with_presence`](RealtimePlugin::with_presence) emits no `presence:*`
/// events at all. Enabling it is a conscious, per-group opt-in: a group passes
/// [`enabled`](Self::enabled) iff the dev's predicate (or prefix set) matches
/// it. A group that isn't presence-enabled fans out nothing on connect/disconnect.
///
/// Because presence exposes *user identity*, the projection is locked down:
///
/// - **Authenticated only.** Anonymous connections (`user_id == None`) are
///   excluded entirely — there's nothing to dedupe on, and "an anonymous person
///   is here" is itself a leak. Only signed-in users appear.
/// - **Id-only by default.** Without a [`resolver`](Self::resolver), a present
///   user is broadcast as `{ "id": "<user_id>" }` and nothing else — never the
///   raw user row. A resolver (`Fn(&str) -> serde_json::Value`) is the dev's
///   explicit choice of what's safe to broadcast (e.g. `{id, name, avatar}`).
/// - **Policy-gated visibility.** Presence events go through the normal group
///   dispatch, so [`GroupPolicy::can_join`] already governs *who can see* a
///   group's presence: a client that can't join `room:42` never receives its
///   `presence:*` events.
pub struct PresenceSpec {
    /// Returns `true` for a group that has presence enabled. Default (no
    /// [`with_presence`]) is the all-`false` predicate.
    enabled: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// Projects a present user's PK string to the JSON broadcast for them.
    /// Default: `{ "id": "<user_id>" }` only.
    resolver: Arc<dyn Fn(&str) -> serde_json::Value + Send + Sync>,
}

impl PresenceSpec {
    /// Enable presence for any group matching `predicate`. The default
    /// projection is **id-only** (`{ "id": <user_id> }`); chain
    /// [`resolver`](Self::resolver) to broadcast more.
    pub fn matching<F>(predicate: F) -> Self
    where
        F: Fn(&str) -> bool + Send + Sync + 'static,
    {
        Self {
            enabled: Arc::new(predicate),
            resolver: Arc::new(default_presence_projection),
        }
    }

    /// Enable presence for any group whose name starts with one of `prefixes`
    /// (e.g. `["room:", "public:lobby"]`). Sugar over [`matching`](Self::matching).
    pub fn prefixes<I, S>(prefixes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let prefixes: Vec<String> = prefixes.into_iter().map(Into::into).collect();
        Self::matching(move |group| prefixes.iter().any(|p| group.starts_with(p.as_str())))
    }

    /// Supply the identity projection: map a present user's PK string to the
    /// JSON broadcast for them. **This is the dev's explicit choice of what's
    /// safe to expose** — the default (no resolver) is id-only.
    ///
    /// ```ignore
    /// PresenceSpec::prefixes(["room:"]).resolver(|uid| serde_json::json!({
    ///     "id": uid,
    ///     "name": lookup_name(uid),
    /// }))
    /// ```
    pub fn resolver<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) -> serde_json::Value + Send + Sync + 'static,
    {
        self.resolver = Arc::new(f);
        self
    }

    /// Whether `group` has presence enabled.
    pub fn enabled(&self, group: &str) -> bool {
        (self.enabled)(group)
    }

    /// The projected presence info for a present `user_id` (the PK string).
    pub fn project(&self, user_id: &str) -> serde_json::Value {
        (self.resolver)(user_id)
    }
}

impl Default for PresenceSpec {
    /// Presence **off** for every group, id-only projection.
    fn default() -> Self {
        Self {
            enabled: Arc::new(|_| false),
            resolver: Arc::new(default_presence_projection),
        }
    }
}

/// The default presence projection: `{ "id": "<user_id>" }` and nothing else.
/// Never the raw user — the dev opts into more via [`PresenceSpec::resolver`].
fn default_presence_projection(user_id: &str) -> serde_json::Value {
    serde_json::json!({ "id": user_id })
}

/// Presence event names dispatched to a group (subscribers route on these):
/// the full member list on join, a single user joining, a single user leaving.
pub const PRESENCE_SYNC: &str = "presence:sync";
pub const PRESENCE_JOIN: &str = "presence:join";
pub const PRESENCE_LEAVE: &str = "presence:leave";

/// Dispatch the presence transitions a connection's register/deregister caused.
/// Best-effort and policy-gated by the normal group dispatch: a `presence:join`
/// / `presence:leave` delta is sent to the whole group only when this conn was
/// the user's FIRST / LAST in it. The `sync` snapshot (the full member roster)
/// is delivered ONLY to the joining user's connection(s) — existing members
/// track the roster from the join/leave deltas, so the roster is not
/// re-broadcast to the whole group on every join (audit_2 realtime #5: that was
/// O(N²) fan-out for no new information).
///
/// Called from the SSE connect path / `ConnGuard` with the transitions the
/// registry computed under its lock (the lock is already dropped here, so the
/// async sends never hold it). No-op when presence isn't installed/enabled.
///
/// Public so a transport (or a test) holding [`PresenceTransitions`] from
/// [`Registry::register_with_presence`] / [`deregister_with_presence`] can drive
/// the same gated, projected dispatch the built-in SSE/WS paths use.
pub async fn dispatch_presence(transitions: PresenceTransitions) {
    let Some(rt) = REALTIME.get() else {
        return;
    };
    let spec = &rt.presence;
    // First-join deltas: tell the WHOLE group a new member entered. This is the
    // O(N)-per-join broadcast every existing member needs to update its roster.
    for (group, user_id) in &transitions.joined {
        if !spec.enabled(group) {
            continue;
        }
        let member = spec.project(user_id);
        Realtime::to_group(group.clone())
            .send(PRESENCE_JOIN, &member)
            .await;
    }
    // Sync snapshot: the full member roster of each newly-entered group. Deliver
    // it ONLY to the joining user's connection(s) — the ones that actually need
    // the initial roster. Every existing member already learned about the
    // newcomer from the `presence:join` delta above and maintains its roster
    // incrementally, so re-broadcasting the whole roster to the ENTIRE group on
    // every single join (the old behavior) was O(N²) fan-out under a join storm
    // for zero new information (audit_2 realtime #5). The wire messages are
    // unchanged — only the recipient set of `presence:sync` narrows from the
    // group to the joiner, which the bundled client (and any client that tracks
    // join/leave deltas) handles transparently.
    for (group, members) in &transitions.sync {
        if !spec.enabled(group) {
            continue;
        }
        // The user who newly entered this group. `sync` is built 1:1 from
        // `joined`, so a match always exists; match by group rather than trust
        // ordering. All entries share this register's single conn/user.
        let Some((_, joiner)) = transitions.joined.iter().find(|(g, _)| g == group) else {
            continue;
        };
        let projected: Vec<serde_json::Value> =
            members.iter().map(|uid| spec.project(uid)).collect();
        Realtime::to_user(joiner.clone())
            .send(PRESENCE_SYNC, &projected)
            .await;
    }
    // Last-leave deltas: tell the whole group a member fully left.
    for (group, user_id) in &transitions.left {
        if !spec.enabled(group) {
            continue;
        }
        let member = spec.project(user_id);
        Realtime::to_group(group.clone())
            .send(PRESENCE_LEAVE, &member)
            .await;
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
    presence: Arc<PresenceSpec>,
    /// Explicit allowlist of permitted WebSocket `Origin` values (the CSWSH
    /// guard's prod cross-origin allowlist). Empty by default — same-origin
    /// is always permitted regardless.
    allowed_origins: Arc<[String]>,
    /// The configured mount base (default `/realtime`). The served JS and the
    /// startup log are templated off this.
    base_path: Arc<str>,
    /// Cap on an inbound WebSocket message/frame, in bytes. Default
    /// [`DEFAULT_WS_MAX_MESSAGE_BYTES`].
    ws_max_message_bytes: usize,
    /// Per-connection inbound message-rate cap, messages/sec (`0` = disabled).
    /// Default [`DEFAULT_WS_MAX_MESSAGES_PER_SEC`].
    ws_max_messages_per_sec: u32,
}

impl Realtime {
    fn get() -> &'static Realtime {
        REALTIME
            .get()
            .expect("umbral-realtime: RealtimePlugin is not installed")
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

    /// Whether `user_id` may **send** to `group` under the configured policy
    /// (audit_2 realtime #2). A [`MessageHandler`] that broadcasts to a
    /// client-supplied room MUST call this first — a joined client can otherwise
    /// inject into any group (IDOR). Delegates to [`GroupPolicy::can_send`],
    /// which defaults to the same rule as join.
    pub fn can_send(user_id: Option<&str>, group: &str) -> bool {
        Self::get().policy.can_send(user_id, group)
    }

    /// The configured inbound-message handler (WS transport).
    pub fn message_handler() -> Arc<dyn MessageHandler> {
        Self::get().handler.clone()
    }

    /// The identity resolver: maps request headers to an authenticated user's
    /// PK string (or `None` for anonymous). Used by both transports at handshake
    /// to populate the registry entry and pass the user id to the group policy.
    pub fn resolver() -> IdentityResolver {
        Self::get().resolver.clone()
    }

    /// The configured presence spec (which groups have presence enabled + the
    /// identity projection). Defaults to presence-off for every group.
    pub fn presence() -> Arc<PresenceSpec> {
        Self::get().presence.clone()
    }

    /// The explicit allowlist of permitted WebSocket `Origin` values (the
    /// CSWSH guard's prod cross-origin allowlist). Empty by default; set via
    /// [`RealtimePlugin::allowed_origins`]. Same-origin requests are always
    /// permitted regardless of this list.
    pub fn allowed_origins() -> Arc<[String]> {
        Self::get().allowed_origins.clone()
    }

    /// The configured mount base for the realtime endpoints (default
    /// `/realtime`). The served `worker.js` / `client.js` template their
    /// `/realtime/...` URLs off this. Set via [`RealtimePlugin::at`].
    pub fn base_path() -> Arc<str> {
        Self::get().base_path.clone()
    }

    /// The cap on an inbound WebSocket message/frame in bytes (default
    /// [`DEFAULT_WS_MAX_MESSAGE_BYTES`]). The WS transport applies it to the
    /// upgrade so an oversized client frame is refused instead of allocated.
    /// Set via [`RealtimePlugin::ws_max_message_bytes`].
    pub fn ws_max_message_bytes() -> usize {
        Self::get().ws_max_message_bytes
    }

    /// Per-connection inbound message-rate cap, messages/sec (`0` = disabled).
    /// Set via [`RealtimePlugin::ws_max_messages_per_sec`] (audit_2 realtime #4).
    pub fn ws_max_messages_per_sec() -> u32 {
        Self::get().ws_max_messages_per_sec
    }

    /// Target a single user's every live connection. `user_id` is the user's
    /// **primary key as a string** (`user.id().to_string()`, `uuid.to_string()`,
    /// or a literal like `"42"`) — PK-type-agnostic, and the same canonical
    /// string the [`IdentityResolver`] / [`with_auth_sessions`](RealtimePlugin::with_auth_sessions)
    /// produces, so the target lines up with the registered connection.
    pub fn to_user(user_id: impl Into<String>) -> Target {
        Target {
            target: TargetKind::User(user_id.into()),
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
    /// Which groups have presence ("who's online") enabled + the identity
    /// projection. Defaults to presence-off for every group; opt in via
    /// [`with_presence`](Self::with_presence).
    presence: Arc<PresenceSpec>,
    /// Explicit allowlist of permitted WebSocket `Origin` values (the CSWSH
    /// guard). Empty by default; set via [`allowed_origins`](Self::allowed_origins).
    allowed_origins: Vec<String>,
    /// Mount base for the realtime endpoints. Default `/realtime`; set via
    /// [`at`](Self::at). The served JS templates its URLs off this.
    base_path: String,
    /// Cap on an inbound WebSocket message/frame, in bytes. Defaults to
    /// [`DEFAULT_WS_MAX_MESSAGE_BYTES`]; set via
    /// [`ws_max_message_bytes`](Self::ws_max_message_bytes).
    ws_max_message_bytes: usize,
    /// Per-connection inbound message-rate cap, messages per second (audit_2
    /// realtime #4). Defaults to [`DEFAULT_WS_MAX_MESSAGES_PER_SEC`]; `0`
    /// disables the cap. Set via [`ws_max_messages_per_sec`](Self::ws_max_messages_per_sec).
    ws_max_messages_per_sec: u32,
}

/// The no-op identity resolver: every connection is anonymous (`None`).
/// This is the default so anonymous / push-only feeds require no auth dep.
fn anonymous_resolver() -> IdentityResolver {
    Arc::new(|_headers: HeaderMap| Box::pin(async { None }))
}

/// Strip the userinfo (`user:password@`) from a connection URL so it is safe
/// to log. `redis://:s3cret@host:6379/0` → `redis://host:6379/0`. Only the
/// authority section (between the scheme and the first `/`, `?` or `#`) is
/// inspected, so an `@` in a path or query parameter is left alone. URLs
/// without userinfo pass through unchanged.
// The only production call site is the `redis`-gated broker boot log; unit
// tests exercise it under every feature set.
#[cfg_attr(not(feature = "redis"), allow(dead_code))]
fn redact_url(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (Some(s), r),
        None => (None, url),
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let host = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };
    match scheme {
        Some(s) => format!("{s}://{host}{tail}"),
        None => format!("{host}{tail}"),
    }
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
            max_connections: Some(DEFAULT_MAX_CONNECTIONS),
            presence: Arc::new(PresenceSpec::default()),
            allowed_origins: Vec::new(),
            base_path: "/realtime".to_string(),
            ws_max_message_bytes: DEFAULT_WS_MAX_MESSAGE_BYTES,
            ws_max_messages_per_sec: DEFAULT_WS_MAX_MESSAGES_PER_SEC,
        }
    }
}

impl RealtimePlugin {
    /// A fresh plugin with the safe defaults (alias for
    /// [`default`](Default::default)): anonymous identity, `public:*`-only
    /// group policy, no model exposed. Chain [`expose`](Self::expose) to
    /// opt a model into live broadcast.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the group-join policy. The default ([`PublicGroupsOnly`])
    /// allows only `public:*` groups; supply your own to grant access to
    /// private rooms from the authenticated identity.
    pub fn group_policy<P: GroupPolicy + 'static>(mut self, policy: P) -> Self {
        self.policy = Arc::new(policy);
        self
    }

    /// Gate room access with a closure — the ergonomic alternative to a named
    /// [`GroupPolicy`] type. `|user_id, group| -> bool` returns `true` to allow
    /// the join (`user_id: Option<&str>` is the authenticated user's PK string,
    /// or `None` for anonymous — PK-type-agnostic). Wire
    /// [`with_auth_sessions`](Self::with_auth_sessions) (or a custom
    /// [`identity_resolver`](Self::identity_resolver)) first so `user_id` is
    /// populated. See [`FnGroupPolicy`].
    ///
    /// ```ignore
    /// RealtimePlugin::new()
    ///     .with_auth_sessions()
    ///     .group_policy_fn(|user_id, group| {
    ///         group.starts_with("public:")
    ///             || matches!(user_id, Some(uid) if group == format!("user:{uid}"))
    ///     });
    /// ```
    pub fn group_policy_fn<F>(self, f: F) -> Self
    where
        F: Fn(Option<&str>, &str) -> bool + Send + Sync + 'static,
    {
        self.group_policy(FnGroupPolicy(f))
    }

    /// Supply a custom identity resolver: an async function that maps the
    /// request headers to the authenticated user's PK string (or `None` for
    /// anonymous). This is the extension point for custom auth schemes (JWT,
    /// API keys, etc.) without pulling in `umbral-auth`.
    ///
    /// For session-cookie auth backed by `umbral-auth`, use the convenience
    /// method [`with_auth_sessions`](Self::with_auth_sessions) (requires the
    /// `auth` cargo feature).
    pub fn identity_resolver<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(HeaderMap) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<String>> + Send + 'static,
    {
        self.resolver = Arc::new(move |h| Box::pin(f(h)));
        self
    }

    /// Use `umbral-auth`'s session-cookie resolver to identify the current
    /// user at the SSE/WS handshake. This is the standard wiring when
    /// `umbral-auth` is installed.
    ///
    /// The session user's primary key is rendered to its canonical
    /// [`Display`](std::fmt::Display) string (via
    /// [`current_session_user_pk`](umbral_auth::current_session_user_pk) over the
    /// active [`AuthUser`](umbral_auth::AuthUser)) — `i64`/`String`/`uuid` PKs all
    /// produce the **same** string the dev passes to [`Realtime::to_user`], so
    /// targeting and per-user gating line up regardless of PK type.
    ///
    /// Requires the `auth` cargo feature (`--features auth`).
    #[cfg(feature = "auth")]
    pub fn with_auth_sessions(self) -> Self {
        self.identity_resolver(|headers| async move {
            umbral_auth::current_session_user_pk::<umbral_auth::AuthUser>(&headers)
                .await
                .map(|pk| pk.to_string())
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

    /// Remove the aggregate connection cap entirely (audit_2 realtime #4 ships a
    /// sane [`DEFAULT_MAX_CONNECTIONS`] default; this is the explicit opt-out for
    /// a deployment that fronts its own connection limiting, e.g. at the load
    /// balancer).
    pub fn unlimited_connections(mut self) -> Self {
        self.max_connections = None;
        self
    }

    /// Cap each connection's inbound message rate at `n` messages per second
    /// (audit_2 realtime #4). A client that sustains a higher rate is flooding;
    /// its socket is closed. `0` disables the cap. Defaults to
    /// [`DEFAULT_WS_MAX_MESSAGES_PER_SEC`].
    pub fn ws_max_messages_per_sec(mut self, n: u32) -> Self {
        self.ws_max_messages_per_sec = n;
        self
    }

    /// Cap an **inbound WebSocket message** (and frame) at `n` bytes.
    /// A client frame above the cap is refused by the socket layer — it is
    /// never buffered in full and never reaches the [`MessageHandler`]; the
    /// offending connection is closed. Defaults to
    /// [`DEFAULT_WS_MAX_MESSAGE_BYTES`] (1 MiB) so a single client can't
    /// force multi-megabyte allocations. Outbound (server→client) pushes are
    /// unaffected.
    pub fn ws_max_message_bytes(mut self, n: usize) -> Self {
        self.ws_max_message_bytes = n;
        self
    }

    /// Allowlist of permitted **WebSocket** `Origin` values for the CSWSH
    /// (cross-site WebSocket hijacking) guard. Each entry is a full origin
    /// string, e.g. `"https://app.example.com"`.
    ///
    /// CORS does **not** cover the WebSocket handshake, so without this guard a
    /// cross-origin page could open `wss://your-host/realtime/ws` carrying the
    /// victim's session cookie and receive their gated realtime data. The guard
    /// is safe-by-default:
    ///
    /// - Non-browser clients (no `Origin` header) are allowed (not a CSWSH vector).
    /// - In `Environment::Dev`, cross-origin is allowed (local Vite-style frontends).
    /// - In prod, an Origin is allowed iff it is **same-origin** as the request's
    ///   `Host`, or it appears in this allowlist. Anything else is rejected with
    ///   `403 Forbidden` before the upgrade.
    ///
    /// SSE is deliberately not gated here — the browser's CORS enforcement
    /// already protects a cross-origin `EventSource`. This guard is WS-specific.
    pub fn allowed_origins<I, S>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_origins = origins.into_iter().map(Into::into).collect();
        self
    }

    /// Remount the realtime endpoints under a different base path (default
    /// `/realtime`). `at("/rt")` exposes `/rt/sse`, `/rt/ws`, `/rt/worker.js`,
    /// `/rt/client.js`. The served `worker.js` / `client.js` follow the base
    /// automatically (their `EventSource` / `SharedWorker` URLs are templated),
    /// so an app only needs to point its `<script src>` at `{base}/client.js`.
    ///
    /// The path is normalised exactly like [`OpenApiPlugin::at`]: one leading
    /// slash is ensured, the trailing slash is stripped, and an empty path
    /// falls back to the default `/realtime`.
    pub fn at(mut self, path: &str) -> Self {
        let trimmed = path.trim().trim_end_matches('/');
        self.base_path = if trimmed.is_empty() {
            "/realtime".to_string()
        } else if let Some(stripped) = trimmed.strip_prefix('/') {
            format!("/{stripped}")
        } else {
            format!("/{trimmed}")
        };
        self
    }

    /// Enable **presence** ("who's online in a group"), opt-in and gated. By
    /// default presence is OFF for every group and no `presence:*` event is ever
    /// emitted; this turns it on only for the groups the [`PresenceSpec`]
    /// matches, and controls how a present user's identity is projected.
    ///
    /// Safety is the default:
    /// - **Off unless enabled.** A group not matched by the spec emits nothing.
    /// - **Authenticated only.** Anonymous connections never appear in presence.
    /// - **Id-only projection.** Without a [`resolver`](PresenceSpec::resolver),
    ///   a present user is broadcast as `{ "id": "<user_id>" }` — never the raw row.
    /// - **Policy-gated.** Presence rides the normal group dispatch, so
    ///   [`GroupPolicy::can_join`] governs who can *see* a group's presence.
    ///
    /// Dedup is by user: a user with three tabs in a group is "present" once;
    /// `presence:join` fires on their FIRST connection into the group and
    /// `presence:leave` only when their LAST one leaves.
    ///
    /// ```ignore
    /// // Id-only, for any `room:*` group:
    /// RealtimePlugin::new().with_presence(PresenceSpec::prefixes(["room:"]));
    ///
    /// // Custom projection (the dev's choice of what's safe to broadcast):
    /// RealtimePlugin::new().with_presence(
    ///     PresenceSpec::prefixes(["room:"]).resolver(|uid| serde_json::json!({
    ///         "id": uid, "name": name_of(uid),
    ///     })),
    /// );
    /// ```
    pub fn with_presence(mut self, spec: PresenceSpec) -> Self {
        self.presence = Arc::new(spec);
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
    ///     Realtime::to_group(format!("post:{}", ev.pk_str().unwrap_or_default()))
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
                umbral::signals::subscribe_async(
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
        T: umbral::orm::Model,
        F: Fn(ModelEvent) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.on_table(T::TABLE, handler)
    }

    /// Safe, opt-in model-change broadcast — the Supabase-style "subscribe to a
    /// model's live changes", but secure by construction. Wraps
    /// [`on_model`](Self::on_model) with an [`Expose`] spec that adds field
    /// projection and group routing on top of the raw signal bridge.
    ///
    /// **Nothing is exposed unless you call this, and only the fields you list.**
    /// The defaults are the safe path:
    /// - a model with no `expose`/`on_model` is never broadcast (default-deny);
    /// - without [`Expose::fields`], the payload is **id-only** — "row N changed,
    ///   refetch through your authorized endpoint";
    /// - the group you name is governed by [`GroupPolicy::can_join`], so a
    ///   private group is unjoinable under the default policy;
    /// - [`Expose::all_fields`] is the explicit opt-in to broadcast the whole row.
    ///
    /// Per matching event the bridge: skips actions not in the spec, computes
    /// the (static or per-row) group, projects `instance` to the whitelist
    /// (or id-only / all), and sends event-name `"created" | "updated" |
    /// "deleted"` to that group.
    ///
    /// ```ignore
    /// RealtimePlugin::new().expose::<Post>(
    ///     Expose::to_group("public:posts").fields(&["id", "title", "slug"]),
    /// );
    /// ```
    pub fn expose<T>(self, spec: Expose) -> Self
    where
        T: umbral::orm::Model,
    {
        let spec = Arc::new(spec);
        self.on_model::<T, _, _>(move |ev| {
            let spec = spec.clone();
            async move {
                // Default-deny per action: skip anything not in the whitelist.
                if !spec.actions.contains(&ev.action) {
                    return;
                }
                let group = spec.route.group_for(&ev);
                let projected = spec.project(&ev.instance);
                let action_name = ev.action_name();
                Realtime::to_group(group)
                    .send(action_name, &projected)
                    .await;
            }
        })
    }

    /// Pick the broker at boot: a [`RedisBroker`] when a URL is configured
    /// and the `redis` feature is on, else the single-instance
    /// [`InProcessBroker`]. A URL set without the feature warns and falls
    /// back rather than silently scaling to one instance.
    fn build_broker(&self, registry: Arc<Registry>) -> Arc<dyn Broker> {
        #[cfg(feature = "redis")]
        if let Some(url) = self.redis_url.clone() {
            // Log the redacted form only: the configured URL commonly embeds
            // a password (`redis://:password@host`), which must never reach
            // a log sink.
            tracing::info!("realtime: redis broker backplane → {}", redact_url(&url));
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

    fn routes(&self) -> umbral::web::Router {
        let base = &self.base_path;
        umbral::web::Router::new()
            .route(&format!("{base}/sse"), umbral::web::get(sse::sse_handler))
            .route(&format!("{base}/ws"), umbral::web::get(ws::ws_handler))
            .route(
                &format!("{base}/worker.js"),
                umbral::web::get(assets::worker_js_handler),
            )
            .route(
                &format!("{base}/client.js"),
                umbral::web::get(assets::client_js_handler),
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
            presence: self.presence.clone(),
            allowed_origins: self.allowed_origins.clone().into(),
            base_path: Arc::from(self.base_path.as_str()),
            ws_max_message_bytes: self.ws_max_message_bytes,
            ws_max_messages_per_sec: self.ws_max_messages_per_sec,
        });
        // Register the model-change subscriptions now that the ambient
        // handle exists (a fired handler calls Realtime::to_group, etc.).
        for register in &self.subscriptions {
            register();
        }
        let base = &self.base_path;
        tracing::info!("realtime: SSE at {base}/sse, WS at {base}/ws");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(gs: &[&str]) -> HashSet<String> {
        gs.iter().map(|s| s.to_string()).collect()
    }

    /// Terse user-id helper: identity is now a PK string, so a test that used
    /// `Some(7)` becomes `u(7)` → `Some("7".to_string())`.
    fn u(id: i64) -> Option<String> {
        Some(id.to_string())
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
            .register(u(7), groups(&[]), DEFAULT_BUFFER)
            .await
            .unwrap();
        let (_b, mut rx_b) = reg
            .register(u(7), groups(&[]), DEFAULT_BUFFER)
            .await
            .unwrap();
        let (_c, mut rx_c) = reg
            .register(u(9), groups(&[]), DEFAULT_BUFFER)
            .await
            .unwrap();

        let n = reg
            .dispatch(
                &TargetKind::User("7".into()),
                reg_event("ping", serde_json::json!({"x": 1})),
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
            .register(u(1), groups(&["chat:1"]), DEFAULT_BUFFER)
            .await
            .unwrap();
        let (_b, mut rx_b) = reg
            .register(u(2), groups(&["chat:2"]), DEFAULT_BUFFER)
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
            .register(u(1), groups(&["g"]), DEFAULT_BUFFER)
            .await
            .unwrap();
        let (_b, _rx_b) = reg
            .register(None, groups(&["g"]), DEFAULT_BUFFER)
            .await
            .unwrap();

        assert_eq!(reg.connection_count().await, 2);
        let n = reg.dispatch(&TargetKind::Broadcast, evt()).await;
        assert_eq!(n, 2, "broadcast hit both");

        reg.deregister(a).await;
        assert_eq!(reg.connection_count().await, 1);
        // User index for the gone connection is cleaned: to_user(1) → 0.
        let n = reg.dispatch(&TargetKind::User("1".into()), evt()).await;
        assert_eq!(n, 0, "deregister removed user 1 from the index");
        // The group still has the anonymous connection.
        let n = reg.dispatch(&TargetKind::Group("g".into()), evt()).await;
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn broadcast_delivers_on_every_receiver_after_snapshot_then_send() {
        // Snapshot-then-send must still reach every registered connection:
        // register 3, broadcast once, and read the event off each receiver.
        let reg = Registry::default();
        let (_a, mut rx_a) = reg
            .register(u(1), groups(&[]), DEFAULT_BUFFER)
            .await
            .unwrap();
        let (_b, mut rx_b) = reg
            .register(u(2), groups(&[]), DEFAULT_BUFFER)
            .await
            .unwrap();
        let (_c, mut rx_c) = reg
            .register(None, groups(&["g"]), DEFAULT_BUFFER)
            .await
            .unwrap();

        let n = reg
            .dispatch(
                &TargetKind::Broadcast,
                reg_event("hi", serde_json::json!({"x": 1})),
            )
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
            .register(u(1), groups(&[]), DEFAULT_BUFFER)
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
            .register(u(3), groups(&[]), DEFAULT_BUFFER)
            .await
            .unwrap();
        let broker = InProcessBroker::new(registry.clone());

        broker
            .publish(Envelope {
                target: TargetKind::User("3".into()),
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
        assert_eq!(TargetKind::User("42".into()).channel(), "@user:42");
        assert_eq!(TargetKind::Broadcast.channel(), "@broadcast");
    }

    #[tokio::test]
    async fn dispatch_stamps_the_channel_on_delivered_events() {
        let reg = Registry::default();
        let (_g, mut rx_g) = reg
            .register(u(5), groups(&["chat:1"]), DEFAULT_BUFFER)
            .await
            .unwrap();
        reg.dispatch(
            &TargetKind::Group("chat:1".into()),
            reg_event("message", serde_json::json!("hi")),
        )
        .await;
        assert_eq!(recv(&mut rx_g).await.unwrap().channel, "chat:1");

        let (_u, mut rx_u) = reg
            .register(u(7), groups(&[]), DEFAULT_BUFFER)
            .await
            .unwrap();
        reg.dispatch(
            &TargetKind::User("7".into()),
            reg_event("ping", serde_json::json!({})),
        )
        .await;
        assert_eq!(recv(&mut rx_u).await.unwrap().channel, "@user:7");

        let (_b, mut rx_b) = reg
            .register(None, groups(&[]), DEFAULT_BUFFER)
            .await
            .unwrap();
        reg.dispatch(
            &TargetKind::Broadcast,
            reg_event("all", serde_json::json!({})),
        )
        .await;
        // Drain other receivers' broadcast copies are irrelevant; check b.
        assert_eq!(recv(&mut rx_b).await.unwrap().channel, "@broadcast");
    }

    #[test]
    fn default_group_policy_allows_only_public() {
        let p = PublicGroupsOnly;
        assert!(p.can_join(Some("1"), "public:lobby"));
        assert!(!p.can_join(Some("1"), "tenant:99"));
        assert!(!p.can_join(None, "chat:1"));
    }

    #[test]
    fn fn_group_policy_gates_rooms_via_closure() {
        // The ergonomic gate: public rooms for anyone, a private room only for
        // its owning user, everything else denied.
        let p = FnGroupPolicy(|user_id: Option<&str>, group: &str| {
            group.starts_with("public:")
                || matches!(user_id, Some(uid) if group == format!("user:{uid}"))
        });
        assert!(p.can_join(None, "public:lobby"), "public open to anyone");
        assert!(p.can_join(Some("7"), "user:7"), "owner can join their room");
        assert!(!p.can_join(Some("8"), "user:7"), "non-owner denied");
        assert!(!p.can_join(None, "user:7"), "anonymous denied private");
    }

    #[test]
    fn group_policy_is_pk_type_agnostic() {
        // Identity is an opaque PK STRING, so a numeric-PK user and a UUID-PK
        // user route through the very same closure with no i64 assumption: each
        // owns its own `user:<pk>` room, keyed on whatever its PK renders to.
        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        let p = FnGroupPolicy(
            |user_id: Option<&str>, group: &str| matches!(user_id, Some(uid) if group == format!("user:{uid}")),
        );
        // Numeric string PK.
        assert!(
            p.can_join(Some("42"), "user:42"),
            "numeric pk owns its room"
        );
        assert!(
            !p.can_join(Some("42"), &format!("user:{uuid}")),
            "not the uuid's room"
        );
        // UUID string PK.
        assert!(
            p.can_join(Some(uuid), &format!("user:{uuid}")),
            "uuid pk owns its room"
        );
        assert!(!p.can_join(Some(uuid), "user:42"), "uuid is not user 42");
    }

    #[tokio::test]
    async fn to_user_targets_the_matching_string_identity() {
        // to_user(<pk string>) reaches the connection registered under exactly
        // that string, and no other — proven for both a UUID and a numeric pk.
        let reg = Registry::default();
        let uuid = "550e8400-e29b-41d4-a716-446655440000".to_string();
        let (_a, mut rx_a) = reg
            .register(Some(uuid.clone()), groups(&[]), DEFAULT_BUFFER)
            .await
            .unwrap();
        let (_b, mut rx_b) = reg
            .register(Some("42".to_string()), groups(&[]), DEFAULT_BUFFER)
            .await
            .unwrap();

        // Target the UUID identity: only conn A receives it.
        let n = reg
            .dispatch(
                &TargetKind::User(uuid.clone()),
                reg_event("ping", serde_json::json!({})),
            )
            .await;
        assert_eq!(n, 1, "only the uuid-identity connection matched");
        assert!(recv(&mut rx_a).await.is_some(), "uuid conn received");
        assert!(recv(&mut rx_b).await.is_none(), "numeric conn did NOT");
        // A different string (the same digits, but not the uuid) reaches nobody.
        let n = reg
            .dispatch(
                &TargetKind::User("999".into()),
                reg_event("ping", serde_json::json!({})),
            )
            .await;
        assert_eq!(n, 0, "an unknown identity string reaches no one");

        // The channel stamp carries the opaque string verbatim.
        assert_eq!(
            TargetKind::User(uuid).channel(),
            "@user:550e8400-e29b-41d4-a716-446655440000"
        );
    }

    #[tokio::test]
    async fn presence_dedup_and_projection_key_on_string_ids() {
        // Presence dedup is keyed on the PK STRING: a user with two conns in a
        // group counts once, and the default projection is `{"id":"<string>"}`.
        let reg = Registry::default();
        let uuid = "abc-123".to_string();
        // First conn for the uuid user → first-join.
        let (_a, _rx_a, t1) = reg
            .register_with_presence(Some(uuid.clone()), groups(&["room:1"]), DEFAULT_BUFFER)
            .await
            .unwrap();
        assert_eq!(t1.joined, vec![("room:1".to_string(), uuid.clone())]);
        assert_eq!(
            t1.sync,
            vec![("room:1".to_string(), vec![uuid.clone()])],
            "sync snapshot carries the string id"
        );
        // Second conn for the SAME uuid user → no new first-join (deduped).
        let (b, _rx_b, t2) = reg
            .register_with_presence(Some(uuid.clone()), groups(&["room:1"]), DEFAULT_BUFFER)
            .await
            .unwrap();
        assert!(
            t2.joined.is_empty(),
            "second conn of same user does not re-join"
        );

        // Default projection renders the id as a STRING, not a number.
        assert_eq!(
            default_presence_projection(&uuid),
            serde_json::json!({ "id": "abc-123" })
        );

        // Dropping the second conn is NOT a last-leave (the first still holds).
        let t3 = reg.deregister_with_presence(b).await;
        assert!(t3.left.is_empty(), "user still present via the first conn");
    }

    fn evt() -> Event {
        Event {
            event: "e".into(),
            data: serde_json::Value::Null,
            channel: String::new(),
            seq: 0,
        }
    }

    #[test]
    fn at_normalizes_the_base_path() {
        // Default is /realtime.
        assert_eq!(RealtimePlugin::new().base_path, "/realtime");
        // A bare segment gains a leading slash.
        assert_eq!(RealtimePlugin::new().at("rt").base_path, "/rt");
        // A leading slash is kept (not doubled).
        assert_eq!(RealtimePlugin::new().at("/rt").base_path, "/rt");
        // A trailing slash is stripped.
        assert_eq!(RealtimePlugin::new().at("/rt/").base_path, "/rt");
        // Nested paths work.
        assert_eq!(RealtimePlugin::new().at("/api/rt").base_path, "/api/rt");
        // Empty / whitespace falls back to the default.
        assert_eq!(RealtimePlugin::new().at("").base_path, "/realtime");
        assert_eq!(RealtimePlugin::new().at("   ").base_path, "/realtime");
    }

    #[test]
    fn allowed_origins_collects_the_allowlist() {
        let p = RealtimePlugin::new().allowed_origins(["https://app.example.com", "https://b.com"]);
        assert_eq!(
            p.allowed_origins,
            vec![
                "https://app.example.com".to_string(),
                "https://b.com".to_string()
            ]
        );
        // Empty by default — same-origin still works without any config.
        assert!(RealtimePlugin::new().allowed_origins.is_empty());
    }

    #[test]
    fn redact_url_strips_password_userinfo() {
        // The canonical Redis credential form: `redis://:password@host`.
        let out = redact_url("redis://:s3cr3t@cache.internal:6379/0");
        assert_eq!(out, "redis://cache.internal:6379/0");
        assert!(!out.contains("s3cr3t"));
    }

    #[test]
    fn redact_url_strips_user_and_password() {
        let out = redact_url("redis://admin:hunter2@host:6380");
        assert_eq!(out, "redis://host:6380");
        assert!(!out.contains("hunter2"));
        assert!(!out.contains("admin"));
    }

    #[test]
    fn redact_url_passes_through_credential_free_urls() {
        assert_eq!(
            redact_url("redis://cache.internal:6379/0"),
            "redis://cache.internal:6379/0"
        );
        assert_eq!(redact_url("rediss://host"), "rediss://host");
    }

    #[test]
    fn redact_url_only_inspects_the_authority() {
        // An `@` after the path/query is NOT userinfo — leave it alone.
        assert_eq!(
            redact_url("redis://host:6379/0?note=a@b"),
            "redis://host:6379/0?note=a@b"
        );
        // ...but real userinfo is still stripped when a query also has an `@`.
        let out = redact_url("redis://:pw@host:6379/0?note=a@b");
        assert_eq!(out, "redis://host:6379/0?note=a@b");
        assert!(!out.contains("pw@"));
    }

    #[test]
    fn redact_url_handles_schemeless_urls() {
        assert_eq!(redact_url("user:pw@host:6379"), "host:6379");
        assert_eq!(redact_url("host:6379"), "host:6379");
    }
}

#[cfg(test)]
mod audit_realtime2_tests {
    use super::*;

    // audit_2 realtime #2: can_send defaults to can_join (safe default), and a
    // policy can override it independently.
    #[test]
    fn can_send_defaults_to_can_join() {
        let p = PublicGroupsOnly;
        assert!(p.can_send(Some("1"), "public:lobby"));
        assert!(!p.can_send(Some("1"), "chat:secret")); // same rule as join
    }

    #[test]
    fn can_send_is_overridable_independently_of_join() {
        // A room you may POST to but not SUBSCRIBE to.
        struct PostOnly;
        impl GroupPolicy for PostOnly {
            fn can_join(&self, _u: Option<&str>, group: &str) -> bool {
                group.starts_with("public:")
            }
            fn can_send(&self, _u: Option<&str>, group: &str) -> bool {
                group == "feedback" || group.starts_with("public:")
            }
        }
        let p = PostOnly;
        assert!(!p.can_join(None, "feedback"));
        assert!(p.can_send(None, "feedback"));
    }
}

#[cfg(test)]
mod audit_realtime4_tests {
    use super::*;

    // audit_2 realtime #4: sane connection cap + message-rate cap by default,
    // with explicit opt-outs.
    #[test]
    fn defaults_ship_a_connection_cap_and_rate_cap() {
        let p = RealtimePlugin::default();
        assert_eq!(
            p.max_connections,
            Some(DEFAULT_MAX_CONNECTIONS),
            "default must cap connections, not be unlimited"
        );
        assert_eq!(p.ws_max_messages_per_sec, DEFAULT_WS_MAX_MESSAGES_PER_SEC);
    }

    #[test]
    fn unlimited_connections_opts_out() {
        let p = RealtimePlugin::default().unlimited_connections();
        assert_eq!(p.max_connections, None);
    }

    #[test]
    fn builders_override_the_caps() {
        let p = RealtimePlugin::default()
            .max_connections(5)
            .ws_max_messages_per_sec(7);
        assert_eq!(p.max_connections, Some(5));
        assert_eq!(p.ws_max_messages_per_sec, 7);
    }
}

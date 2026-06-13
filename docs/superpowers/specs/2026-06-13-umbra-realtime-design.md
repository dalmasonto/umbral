# umbra-realtime — SSE + WebSocket real-time push (features.md #45)

Date: 2026-06-13
Status: design approved (autonomous), scaffolding next

## Goal

A developer can push data to **a specific user** ("notify user 42") and to **a named group/room** ("everyone in `chat:123`", "all staff", "tenant:7") without hand-rolling connection bookkeeping. Two transports — Server-Sent Events (default, simplest) and WebSockets (bidirectional) — over one broker. Model changes can fan out automatically via the signals system (#38).

This is the plugin that unblocks the SSE notification bell (#2), the playground "Realtime" tab (#10), and the alerts `SseChannel` (#77).

## The shape a developer sees

```rust
use umbra_realtime::Realtime;

// From anywhere with the ambient handle (a handler, a signal, a task):
Realtime::to_user(42).send("notification", &json!({ "title": "Build passed" })).await;
Realtime::to_group("chat:123").send("message", &msg).await;
Realtime::to_group("staff").send("plugin_submitted", &payload).await;
Realtime::broadcast().send("ping", &json!({})).await;
```

- `to_user(uid)` targets **every** live connection that authenticated as that user (laptop + phone).
- `to_group(name)` targets every connection that has joined that group.
- `broadcast()` targets all connections.
- `send(event, data)` serializes `data` to JSON and pushes a named SSE/WS event.

Wiring:

```rust
App::builder()
    .plugin(AuthPlugin::<AuthUser>::default())   // identity at handshake
    .plugin(SessionsPlugin::default())
    .plugin(
        RealtimePlugin::default()
            .group_policy(MyPolicy)              // who may join which group
            .on_model::<Post>(|ev| {             // signals bridge
                Realtime::to_group(format!("post:{}", ev.pk)).send("updated", &ev)
            }),
    )
```

## Architecture

Three layers: a **registry** (who's connected), a **broker** (how a message reaches the connection's process), and **transports** (SSE / WS) that own the socket.

### 1. Connection registry (in-process)

- `ConnId` — a unique id per open socket (monotonic counter).
- `tx: HashMap<ConnId, mpsc::Sender<Event>>` — the fan-out sink: pushing an `Event` to a conn's sender makes it appear on that socket's stream.
- Two index maps for O(1) targeting:
  - `by_user: HashMap<i64, HashSet<ConnId>>`
  - `by_group: HashMap<String, HashSet<ConnId>>`
- On connect: allocate `ConnId`, insert into `tx` + `by_user[uid]` (if authenticated) + `by_group[g]` for each joined group.
- On disconnect (stream dropped / WS closed): remove the `ConnId` from `tx` and **every** index it appears in. A `ConnGuard` (Drop) does this so a panic or dropped stream can't leak.
- The whole registry sits behind a `RwLock` (or sharded locks if contention shows up — not v1).

### 2. Broker trait (the multi-instance seam, designed now, one impl in v1)

```rust
#[async_trait]
pub trait Broker: Send + Sync {
    /// Publish an envelope to all instances (including this one).
    async fn publish(&self, env: Envelope);
    /// Subscribe to envelopes published by any instance.
    fn subscribe(&self) -> BoxStream<'static, Envelope>;
}

pub struct Envelope {
    pub target: TargetKind,   // User(i64) | Group(String) | Broadcast
    pub event: String,
    pub data: serde_json::Value,
}
```

- `Realtime::to_*().send()` builds an `Envelope` and calls `broker.publish(env)`.
- A per-process task drains `broker.subscribe()` and applies each envelope to the **local** registry (looks up the target's `ConnId`s, pushes to their `tx`).
- **v1 ships `InProcessBroker`** — `publish` just feeds the local subscribe stream (a `tokio::sync::broadcast` loop), so a single instance works with zero external deps.
- **v2 `RedisBroker`** (documented, not built) — `publish` → Redis `PUBLISH`, `subscribe` → Redis `SUBSCRIBE`. So `to_user(42)` reaches the instance holding that socket. No API change; swap the broker in the builder. Same pattern as the cache/alerts backplane.

Why a broker now: targeting `to_user(42)` only works if the message reaches the process that owns user 42's socket. Single-process is fine for v1; the trait makes multi-instance a drop-in later instead of an API break.

### 3. Transports

- **SSE** — `GET /realtime/sse?groups=chat:123,presence`. Returns axum `Sse<impl Stream>`; the conn's `mpsc::Receiver` becomes the event stream. A keep-alive comment every ~15s holds the connection through proxies. Unidirectional server→client; the default and simplest. This is where `futures-util` lands (also unblocks the QuerySet `iterator()` Stream, features.md #29 phase 2).
- **WebSocket** — `GET /realtime/ws?groups=...`. `WebSocketUpgrade`; bidirectional. Outbound shares the same per-conn sink; inbound client frames dispatch to an app `MessageHandler` (chat send, presence ping, typed commands). Built on axum's WS (tokio-tungstenite under the hood).

Both transports resolve the connection's identity at handshake from the session/bearer (reuse `umbra_auth::resolve_identity`) and validate requested groups before joining.

## Auth + group membership (the security seam)

- A connection's `user_id` comes from the authenticated identity at handshake. Anonymous connections may still join **public** groups but `to_user` never targets them.
- Group joins are validated by an app-provided policy — **default-deny for non-public groups**:

```rust
pub trait GroupPolicy: Send + Sync {
    /// May this identity join this group? Default impl: allow groups
    /// prefixed "public:", deny everything else.
    fn can_join(&self, identity: Option<&Identity>, group: &str) -> bool;
}
```

So a client opening `?groups=tenant:99` it doesn't belong to is rejected at handshake — a user can't subscribe to another tenant's stream. Explicit `Realtime::join(conn, group)` / `leave(conn, group)` also exist for server-driven membership (e.g. on a "joined room" action).

## Signals bridge

`RealtimePlugin::on_model::<T>(f)` subscribes to `post_save:<table>` / `post_delete:<table>` (#38) and runs `f(event)` — the "live dashboard / notifications with zero polling" story. The admin SSE bell (#2) is just `to_group("staff")` on chosen signals. Because signals already carry the actor (task-local), an event can include who triggered it.

## Data flow (end to end)

1. Client opens `GET /realtime/sse?groups=chat:123`. Transport resolves identity, runs `GroupPolicy::can_join` per group, registers a `ConnId` in `by_user[uid]` + `by_group["chat:123"]`, returns the SSE stream.
2. Server code calls `Realtime::to_group("chat:123").send("message", &msg)`.
3. That builds an `Envelope { Group("chat:123"), "message", json }` → `broker.publish`.
4. The per-process drain task receives the envelope, resolves `by_group["chat:123"]` → conn ids, pushes the `Event` to each conn's `tx`.
5. Each SSE stream yields the event; the browser's `EventSource` `onmessage` fires.
6. Client navigates away → stream drops → `ConnGuard` removes the conn from every index.

## Error handling

- A full/closed conn channel (slow client) → drop that conn (back-pressure policy: bounded `mpsc`, drop-oldest or disconnect; v1 disconnects a conn that can't keep up rather than buffering unboundedly).
- `send()` is fire-and-forget from the caller's view (it publishes to the broker; delivery is best-effort) — never blocks a request handler on socket I/O.
- A `GroupPolicy` rejection at handshake → 403; the SSE/WS upgrade never completes.

## Testing

- Registry unit tests: connect/disconnect maintains the indexes; `to_user`/`to_group`/`broadcast` resolve the right conn set; disconnect cleans every index (no leak).
- `InProcessBroker`: `publish` reaches a local subscriber.
- Transport integration (via `umbra-testing`): open an SSE stream against a test app, `Realtime::to_group(...).send(...)`, assert the event arrives; assert a `GroupPolicy`-denied group yields 403.
- Signals bridge: a `post_save` on a registered model fans out to the bound group.

## Implementation plan (phases — "start executing" = phase 1+2)

1. **Crate skeleton** — `plugins/umbra-realtime/`: `RealtimePlugin` (Plugin impl, mounts routes), `Realtime` ambient handle (OnceLock), `Target`/`TargetKind`, `Envelope`, `Broker` trait + `InProcessBroker`, the registry. Compiles + registry unit tests. **← start here.**
2. **SSE transport** — `GET /realtime/sse`, keep-alive, identity + `GroupPolicy` at handshake, `ConnGuard` cleanup. Integration test via umbra-testing. Pulls in `futures-util`.
3. **WebSocket transport** — `GET /realtime/ws`, `MessageHandler` for inbound.
4. **Signals bridge** — `on_model::<T>()`.
5. **Demo + docs** — a live "plugin submitted → staff" feed on umbra_website (or a presence counter), playground Realtime tab (#10), `documentation/docs/v0.0.1/realtime/*.mdx`. Closes #45; unblocks #2, #10, #77.

## Dependencies

- axum 0.8 (SSE + WS built in), `tokio` (mpsc/broadcast/RwLock), `futures-util` (Stream), `async-trait`, `serde`/`serde_json`. `tokio-tungstenite` comes via axum's `ws` feature. No external services for v1 (InProcessBroker). Redis is the documented v2 backplane only.

## Crate-boundary check

`umbra-realtime` depends only on the `umbra` facade (Plugin, web, auth identity, signals) — never on a concrete consumer. The signals bridge uses the existing `subscribe`/`emit` surface. No core changes required beyond what's already public (`resolve_identity`, signals, the Plugin trait).

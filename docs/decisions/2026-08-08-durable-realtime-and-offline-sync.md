# Durable realtime delivery and offline sync

Status: draft for ratification (proposes the shape of gaps5 #43 and gaps5 #44)
Date: 2026-08-08
Relates: planning/gaps5.md #43 (tf#256, durable realtime delivery), #44 (tf#257, offline sync), #37 (tf#251, unified security rules), #31 (tf#244, CDC / transactional outbox), #52 (tf#265, after-commit), #36 (client SDK generation)
Builds on: docs/decisions/2026-08-08-cdc-outbox-and-read-replicas.md (the `umbral-outbox` plugin, its `outbox_event` table, the at-least-once relay), docs/decisions/2026-08-08-product-north-star.md (Stage 2 self-hosted platform posture)

## Scope

Two realtime items, one doc, because they are the two halves of "the live feed keeps working when the network does not":

- **#43** is server-to-client durability: an optional durable channel backend layered BEHIND the existing best-effort `Realtime` API, so a subscriber that was offline (or a slow consumer whose events were dropped) can catch up on exactly what it missed, with per-subscriber acknowledgements, cursors, and retention. Best-effort stays the default; durability is opt-in per channel.
- **#44** is client-side offline sync for selected models: a durable change-feed (riding the #31 outbox and the #43 durable channel), a client-side cache, conflict strategies (last-write-wins plus a merge hook), and a cursor-based pull-plus-push sync protocol with client SDK support. This is a large surface and the design phases it honestly.

Both build on real seams that already ship. Read the two anchors first:

- `plugins/umbral-realtime/src/lib.rs` is the realtime plugin as it exists today.
- `docs/decisions/2026-08-08-cdc-outbox-and-read-replicas.md` designs `umbral-outbox`, the durable, ordered, replayable change stream this doc consumes.

---

## What umbral-realtime actually is today (accurate baseline)

The framing for both items has to be exact, because #43 and #44 layer on top of this and must not misrepresent it.

The delivery model is **best-effort, bounded, in-memory, per-process**:

- **The ambient handle.** `RealtimePlugin` sets a process-global `Realtime` at `on_ready`. A handler builds a target and sends:

  ```rust,ignore
  use umbral_realtime::Realtime;

  Realtime::to_user("42").send("notification", &payload).await;   // one user's every live connection
  Realtime::to_group("chat:123").send("message", &msg).await;     // every connection in a room
  Realtime::broadcast().send("ping", &json!({})).await;           // every connection
  ```

  `to_user` takes the user's primary key rendered to its canonical `Display` string (PK-type-agnostic: `i64` -> `"42"`, `Uuid` -> its text, `String` -> itself). `Target::send<T: Serialize>(event, data)` serializes `data` to JSON and calls `Broker::publish(Envelope { target, event, data })`. It is fire-and-forget and **no-ops when the plugin is not installed**, so a handler can call it unconditionally.

- **The broker seam.** `trait Broker { async fn publish(&self, env: Envelope); }`. `InProcessBroker` dispatches straight to the local `Registry`. `RedisBroker` (feature `redis`, wired via `RealtimePlugin::redis(url)`) PUBLISHes the `Envelope` as JSON on the shared `umbral:realtime:events` channel and a per-instance pump SUBSCRIBEs and dispatches to the local registry, so `to_user(42)` reaches whichever instance holds that socket. The Redis handoff queue is bounded (`QUEUE_CAP = 4096`) and **drops the newest envelope with a warning** when full: "a missed live update, not lost data".

- **The registry and dispatch.** `Registry::dispatch` assigns a **process-global monotonic `seq`**, stamps the target `channel` on the event, records `(seq, Envelope)` in a **bounded replay buffer**, then snapshot-clones the matching connections' senders and `try_send`s one shared `Arc<Delivery>` to each. Each connection has a **bounded outbound channel** (`DEFAULT_BUFFER = 64`); `try_send` to a full channel **drops that message for that connection** rather than blocking the sender. This is the by-design slow-consumer drop.

- **The replay buffer.** A bounded ring of the most recent `(seq, Envelope)` (`DEFAULT_REPLAY_BUFFER = 1024`, `RealtimePlugin::replay_buffer(n)`, `0` disables). On SSE reconnect the client sends `Last-Event-ID`; `Registry::replay_since(last_event_id, user_id, groups)` returns the buffered events with `seq > last_event_id` that this subscriber would have received, oldest to newest, before the live receiver attaches. It is explicitly bounded: "an event evicted from it are unrecoverable". This is best-effort catch-up over a brief drop, not durable delivery. It is also **process-local**: the buffer lives in one process's `Registry`, so a reconnect that lands on a different instance sees that instance's buffer, not the one that dropped the events.

- **Transports.** SSE (`GET /realtime/sse`, push-only) and WebSocket (`GET /realtime/ws`, bidirectional via a `MessageHandler`). Group joins are gated at the handshake by `GroupPolicy::can_join`; inbound WS publishes go through `MessageContext::publish`, which runs `GroupPolicy::can_send` first.

- **The signals bridge.** `RealtimePlugin::on_table` / `on_model::<T>` subscribe to the ORM's `post_save:<table>` / `post_delete:<table>` signals and fan a `ModelEvent` out to clients with zero polling. `expose::<T>(Expose::to_group(...).fields(&[...]))` is the safe, opt-in, field-whitelisted version.

The one-line honest summary, straight from the module docs (`lib.rs:75-84`): **bounded buffers, best-effort delivery, slow consumers drop events by design.** Every value #43 and #44 add has to be layered so that this default is unchanged for the apps that want exactly this.

---

## Part 1 (#43): optional durable channels

### The problem, stated against what exists

The replay buffer bridges a *brief* SSE drop and only within one process. Three gaps remain for anyone who needs the live feed to be a source of truth rather than a convenience:

1. **Eviction loses events.** The ring holds the last 1024 (default) events across all subscribers. Under any real event rate a client offline for more than seconds reconnects past the oldest retained `seq` and silently misses everything older. `replay_since` returns what survives; the rest is gone.
2. **It is process-local.** The buffer lives in the instance's `Registry`. On a multi-instance deployment (the `RedisBroker` case) a reconnect that lands on instance B cannot replay events instance A dropped. Redis pub/sub is fire-and-forget: an instance that was down when an envelope was PUBLISHed never sees it, and there is no per-subscriber cursor.
3. **No acknowledgement.** `try_send` returning `Ok` means "queued into the bounded channel", not "the client received it". There is no delivery record and no way for a subscriber to say "I have consumed through `seq` N".

Best-effort is the correct default for presence pings, typing indicators, and "row changed, refetch it". It is the wrong default for "an order was paid", "a document was edited", or any feed a client reconciles state from. #43 adds durability as an opt-in tier without moving the default.

### The design: durable channels behind the same `Realtime` API

The key constraint from the north star and the plugin contract: **the `Realtime::to_user / to_group / broadcast` surface does not change.** A durable channel is a policy on a channel name, resolved at publish time, exactly like the broker swap is invisible to the send site.

#### A durable-channel registry

`RealtimePlugin` gains an opt-in, per-channel durability declaration, matched by prefix or predicate the same way `PresenceSpec` and `GroupPolicy` already match group names:

```rust,ignore
RealtimePlugin::new()
    .with_auth_sessions()
    .durable(DurableChannels::prefixes(["orders:", "doc:"])   // which channels are durable
        .retention(Duration::from_days(7))                    // how long the durable log keeps events
        .backend(DurableBackend::Outbox));                    // where the durable log lives
```

A channel not matched by a `DurableChannels` spec stays exactly as it is today: best-effort, bounded, replay-buffer-only. So durability is default-off, opt-in per channel, the same safety posture as `expose` and `with_presence`.

#### The durable log: reuse the outbox, do not invent a second one

The #31 CDC doc already designs `umbral-outbox`: an `outbox_event` table (monotonic `id`, `aggregate`, `aggregate_id`, `payload` JSON, `destinations`, `available_at`, `published_at`, `attempts`), an at-least-once relay on `umbral-tasks` with exponential backoff and a per-attempt delivery log, and a `realtime` destination that "hands the event to `umbral-realtime` to fan out to subscribed clients". That doc already names this exact upgrade: the `realtime` destination "upgrades realtime from best-effort (gaps5 #43) to durable-at-the-source ... the event is retained and replayable from `outbox_event`."

#43 is the *consumer* side of that promise. A durable channel is one whose events are persisted to a durable log with a monotonic cursor and served to subscribers from that log, not only from the in-memory ring. Two backends, in phase order:

- **`DurableBackend::Outbox` (Postgres, phase 1).** The durable log IS the outbox. A publish to a durable channel writes an `outbox_event` (with `aggregate = channel`, `aggregate_id` = the row/entity key, `payload` = the event JSON) in addition to the best-effort in-memory dispatch. The event `id` is the durable cursor. The best-effort path still fires immediately for live subscribers (low latency); the durable log is the fallback a reconnecting or lagging subscriber reads from. When the event originated from a model change captured by the #31 outbox in the business transaction (`outbox::publish_on(tx, ...)`), the realtime durable channel and the outbox row are the SAME row, committed with the data, so there is no separate write and no dual-write window. This is the reason to build on the outbox and not a parallel table.
- **`DurableBackend::RedisStreams` / `DurableBackend::Nats` (phase 3, opt-in).** For deployments that already run Redis or NATS and want the durable log off the primary database, a Redis Streams / NATS JetStream backend implements the same durable-log trait (append, read-from-cursor, trim-by-retention). Redis Streams gives an ordered, ID-cursored, retention-trimmed log with consumer groups and `XACK` acknowledgements out of the box; JetStream gives the same with stronger retention. This is the "Redis Streams/NATS" the gap names. It is a later phase because the Outbox backend already delivers the guarantee on the database every umbral app already has.

The durable-log operations are a small trait so the three backends are interchangeable and an app can supply its own (Kafka, etc.), the same extension shape as `Broker` and `Destination`:

```rust,ignore
#[async_trait]
pub trait DurableLog: Send + Sync {
    /// Append an event to `channel`'s durable log; returns its monotonic cursor.
    async fn append(&self, channel: &str, event: &Envelope) -> Result<Cursor, DurableError>;
    /// Read events on `channel` with cursor > `after`, oldest first, up to `limit`.
    async fn read_since(&self, channel: &str, after: Cursor, limit: usize) -> Result<Vec<(Cursor, Envelope)>, DurableError>;
    /// Trim events older than the retention horizon (periodic, off the hot path).
    async fn trim(&self, channel: &str, older_than: SystemTime) -> Result<(), DurableError>;
}
```

#### Per-subscriber cursors and acknowledgements

Best-effort realtime has no per-subscriber state. Durable channels need it: to serve "what did THIS subscriber miss", the server must know each subscriber's last acknowledged cursor per channel.

- **Cursor.** For a durable channel the SSE `Last-Event-ID` / a WS `ack` frame carries the durable-log cursor (the outbox `id` or Redis Stream id), not only the in-process `seq`. On (re)connect the transport reads the subscriber's persisted cursor, calls `DurableLog::read_since(channel, cursor, limit)` to replay the true gap from the durable log (page it, so a long-offline client catches up in bounded batches), then attaches the live receiver. This is the durable analogue of `replay_since`, and it is cross-process: any instance can serve the catch-up because the log is shared, not in one instance's `Registry`.
- **Acknowledgement.** A durable subscriber periodically acks the highest cursor it has durably applied (an explicit WS `ack` frame; for SSE, a lightweight `POST {base}/ack` since SSE is push-only). The server persists `(subscriber, channel, cursor)` in a small `realtime_cursor` model (written through the ORM, per the plugin-uses-the-ORM rule). Retention then means "keep events until every live subscriber has acked past them, bounded by a max horizon so a permanently-gone subscriber cannot pin the log forever". This is the acknowledgement + retention the gap asks for.
- **Subscriber identity.** A durable subscriber must be identifiable across reconnects. The `IdentityResolver` already yields a stable per-user PK string; a durable channel keys cursors on `(user_id, channel)` plus an optional client-supplied device id (so a user's phone and laptop track independent cursors). Anonymous connections cannot use durable channels (there is no stable key to persist a cursor against); they fall back to best-effort, which is correct.

#### Retention and pruning

Retention reuses the outbox's existing discipline: a periodic `#[task]` on the `umbral-tasks` beat trims durable-log entries older than the channel's retention window AND already acked by every tracked subscriber, whichever is more conservative, capped by a hard max horizon. Published/acked rows are not deleted inline (that would turn every publish into a write-write, the exact anti-pattern the outbox doc calls out). Redis Streams / JetStream backends use their native `MAXLEN` / retention policy for the same effect.

### Guarantee, stated honestly

Durable channels are **at-least-once** with a per-subscriber cursor, matching the outbox relay's own guarantee ("at-least-once + idempotency key, not exactly-once"). A subscriber can receive an event twice (a crash after apply but before ack re-delivers on reconnect); the event carries its durable cursor as the idempotency key, and clients dedupe on it. This is the honest ceiling of a DB/stream-backed log without two-phase commit into the client, and it is exactly what #44's client cache is built to tolerate. Best-effort channels keep their current guarantee (may drop, replay buffer bridges brief drops); nothing about the default weakens.

### Why this shape

- **The default does not move.** Durability is a per-channel opt-in resolved at publish time; an app that wires no `DurableChannels` gets byte-for-byte today's best-effort behavior. This is the same default-deny posture as `expose` / `with_presence` / `GroupPolicy`.
- **It reuses the outbox, not a parallel durable store.** The #31 `outbox_event` table, its relay, its backoff, its delivery log, and its `realtime` destination already exist in that design; #43 is the subscriber-facing cursor/ack/catch-up layer over the same log. One durable mechanism, several consumers (webhook, analytics, realtime), as the CDC doc intends.
- **It is cross-process by construction.** Because the durable log is the shared outbox (or a shared stream), catch-up works no matter which instance a reconnect lands on, fixing the process-local limitation of the in-memory replay buffer without touching the `Realtime` send API.
- **`umbral-core` stays plugin-free.** All of this lives in `umbral-realtime` (+ `umbral-outbox`); core never learns durable channels exist. Arrows point inward.

### Deferred / out of scope for #43

- Exactly-once server-to-client delivery (needs client-side dedupe infra; we ship at-least-once + cursor idempotency key, and #44's cache IS that client-side dedupe).
- Ordering stronger than per-channel FIFO (the outbox already promises per-`aggregate_id` FIFO, not global total order across channels).
- A channel-rules DSL tying durable channels to RLS/permissions (gaps5 #45, tf#258) is its own item; #43 keeps `GroupPolicy::can_join` as the visibility gate.

---

## Part 2 (#44): offline sync for selected models

### The problem, stated against what exists

umbral has server push (best-effort today, durable per #43) but nothing on the client: no offline cache, no conflict resolution, no sync protocol. Firebase's Firestore and Supabase's client libraries win adoption largely on offline-first client sync: the app reads and writes a local cache, works offline, and reconciles with the server when connectivity returns. This is a genuinely large surface (a client cache, a merge engine, a wire protocol, and an SDK per language), and the design is explicit that it is phased, not one drop.

### The design: a cursor-based pull-plus-push sync layer over the durable change-feed

Offline sync is, mechanically, three things: a **durable server-side change-feed** the client can page through from a cursor, a **client-side cache** that mirrors selected models, and a **reconciliation protocol** (pull server changes, push local changes, resolve conflicts). #43 supplies the first; #44 adds the second and third.

#### Server side: syncable models and the change-feed

An app opts a model into sync the same declarative way it opts into `expose`:

```rust,ignore
SyncPlugin::new()
    .syncable::<Task>(Sync::for_user(|task| task.owner_id.to_string())   // row -> owning subscriber scope
        .fields(&["id", "title", "done", "updated_at", "version"])       // projected columns on the wire
        .conflict(ConflictStrategy::LastWriteWins));                      // default resolution
```

- **The feed is the durable change-feed from #43/#31.** A syncable model's create/update/delete already lands in the outbox (via `OutboxPlugin::capture::<Task>()` or an in-transaction `outbox::publish_on`). The sync feed for a subscriber is "the durable-log events on the channels that subscriber may see, since their cursor". No new change-capture mechanism: sync is a *reader* of the outbox/durable log, keyed by the per-subscriber cursor #43 already tracks. This is why #44 is drawn on top of #43 and #31 rather than beside them.
- **Scoping.** `Sync::for_user` (or `for_group`/`for_tenant`) maps a row to the sync scope that may see it, and the projection whitelists columns exactly like `Expose::fields` (secrets never reach a client cache by default). Visibility is still gated by `GroupPolicy` / permissions, so a client only pulls rows it is authorized for. This ties into the unified channel-rules item (gaps5 #45) rather than inventing a second authorization model.
- **A version column.** Conflict resolution needs a per-row version. A syncable model carries a monotonically-updated `version` (or reuses `updated_at` with a tiebreaker); the ORM stamps it on write. Last-write-wins compares versions; a merge hook receives both sides.

#### The sync protocol: pull, then push, cursor-based

Two endpoints, both cursor-based, mounted by `SyncPlugin` under `{base}/sync`:

1. **Pull.** `GET {base}/sync/pull?models=task,note&cursor=<opaque>` returns the batch of changes on the caller's syncable models with durable cursor greater than `cursor`, oldest first, projected and scope-filtered, plus the new high-water cursor and a `has_more` flag for paging. The cursor is the durable-log cursor from #43, so pull and the live durable channel share one cursor space: a client can pull to catch up, then switch to the live SSE/WS durable channel from the same cursor with no gap and no double-apply.
2. **Push.** `POST {base}/sync/push` sends the client's queued local mutations, each tagged with the base `version` the client last saw for that row and a client-generated mutation id (the idempotency key, so a retried push does not double-apply). The server, per mutation:
   - applies it through the ORM inside a transaction if the row's current `version` matches the client's base version (fast path, no conflict);
   - on a version mismatch, runs the model's `ConflictStrategy` (below) and returns the resolved row plus its new version so the client can reconcile its cache;
   - writes the resulting change to the outbox as usual, so every OTHER subscriber sees it through the same durable feed. Push and pull close the loop through one durable log.

Both endpoints are ordinary umbral routes contributed by the plugin, gated by the same auth/permission stack as any handler, and they read/write exclusively through the ORM.

#### Conflict resolution

Two strategies, weakest ceremony first, chosen per model:

- **`ConflictStrategy::LastWriteWins` (default).** The higher `version` (server-authoritative tiebreak on equal versions) wins; the loser is discarded and the client is told the winning row so it overwrites its cache. Simple, predictable, correct for the "single logical owner edits from multiple devices" case (a to-do app, settings sync).
- **`ConflictStrategy::Merge(hook)`.** A `Fn(base, local, server) -> resolved` merge hook the app supplies, for models where field-level or CRDT-style merge matters (a collaboratively-edited document, a counter). The hook receives the common base version, the client's proposed value, and the current server value, and returns the merged row; the server applies it under the same version-guarded transaction (re-running the hook if the version moved again, bounded retries). This is the escape hatch for the cases LWW is wrong for, without the framework committing to a full CRDT runtime it would have to own forever.

The framework ships LWW and the merge-hook seam; it does NOT ship a built-in CRDT library in phase 1 (naming CRDTs as a possible future `ConflictStrategy` variant, not a phase-1 promise). This keeps the surface honest: umbral gives you the sync loop and the resolution seam, not a bundled OT/CRDT engine.

#### Client side: the cache and the SDK (ties #37 / #36)

The client half is an SDK, not Rust in this repo, and it is the largest part of #44. It ties directly to the SDK-generation item (gaps5 #37/#36): the same OpenAPI/typegen pipeline that emits typed REST clients emits the sync client. The client SDK provides:

- a **local cache** (IndexedDB in the browser, SQLite on native) mirroring the subscriber's syncable models;
- an **outbound mutation queue** persisted in that cache, so writes made offline survive a reload and flush on reconnect via `sync/push` with their idempotency ids;
- a **cursor store** and the pull/live-channel switch described above (pull to catch up, then the #43 durable channel for live);
- **optimistic local apply** with rollback/reconcile when the server's conflict resolution returns a different winner.

The `updated_at` / `version` contract, the projected field set, and the scope function are all declared once on the server (`SyncPlugin::syncable`) and flow into the generated client types, so the SDK is generated from the same source of truth as the REST client (`umbral typegen` already emits TS from `ModelMeta`; the sync client extends that).

### This is large. The phases.

The design is explicit that #44 is a multi-release effort and sequences it so each phase is independently useful:

- **Phase A (server feed + protocol, no SDK).** `SyncPlugin`, `Sync::syncable`, the `version` column contract, and the `sync/pull` + `sync/push` endpoints over the #43/#31 durable feed, with `LastWriteWins`. A hand-written client (or curl) can already pull-and-push. Depends on #43 phase 1 (Outbox durable backend).
- **Phase B (conflict + scoping hardening).** The `ConflictStrategy::Merge` hook, tenant/group scoping, permission-gated projections, and the pull/live-channel cursor unification so a client can go offline, pull, and resume the live durable channel with no gap or double-apply.
- **Phase C (the official client SDK).** The generated JS/TS offline-sync client (local cache, mutation queue, optimistic apply, reconnect flush), emitted by the #37/#36 SDK pipeline. Native (SQLite-backed) clients follow if BaaS adoption warrants (Stage 3 territory in the north star).
- **Phase D (collaboration primitives, optional).** CRDT-backed `ConflictStrategy`, presence-integrated cursors, and the collaboration primitives gaps5 #46 (tf#259) asks for, built on the sync loop rather than beside it.

Nothing past Phase A is promised for a specific release; the phases exist so the honest answer to "does umbral do offline sync" moves from "no" to "the server feed and protocol, SDK in progress" rather than staying binary.

### Why this shape

- **It is a reader of the durable feed, not a new capture mechanism.** Sync pulls the same outbox/durable-log events #43 serves live, keyed by the same per-subscriber cursor. One change stream feeds webhooks, analytics, live realtime, and offline sync. No second source of truth for "what changed".
- **The server declaration is one line per model, matching `expose`.** `SyncPlugin::syncable::<T>(...)` is the same declarative, default-deny, field-whitelisted shape as `RealtimePlugin::expose::<T>(...)`. A model is not syncable until you say so, and only the fields you list cross to a client cache.
- **Conflict resolution is a seam, not a bundled engine.** LWW ships; the merge hook is the escape hatch; CRDTs are a named future variant, not a phase-1 commitment. umbral owns the sync loop and the resolution point, not a forever-maintained OT/CRDT runtime.
- **The SDK rides the existing typegen.** Offline sync's client is generated from `ModelMeta` by the same pipeline as the REST client (#37/#36), so it is not a hand-maintained parallel artifact.
- **Honest about size.** #44 is phased, each phase independently shippable, and the client SDK (the biggest part) is explicitly its own phase gated on demand, consistent with the north star's Stage 2/Stage 3 sequencing.

### Deferred / out of scope for #44

- A bundled CRDT/OT runtime (Phase D, and only as an optional `ConflictStrategy`, never the default).
- Native mobile SDKs (Kotlin/Swift) beyond JS/TS (Stage 3, gated on BaaS demand per the north star).
- Peer-to-peer / serverless sync (umbral's model is client-to-server-to-durable-log; no P2P mesh).
- Schema-migration coordination on the client cache (an evolving model's cache invalidation) is noted as a hard sub-problem for Phase C, not designed here.

---

## Summary of the contract

- **#43:** durable channels are an opt-in, per-channel policy (`RealtimePlugin::durable(DurableChannels::prefixes([...]).retention(...).backend(...))`) layered BEHIND the unchanged `Realtime::to_user / to_group / broadcast` API. A durable channel persists its events to a durable log (phase 1: the #31 `outbox_event` table; phase 3: Redis Streams / NATS JetStream via the `DurableLog` trait), tracks a per-subscriber `(user_id, channel, device)` cursor in a `realtime_cursor` model, serves cross-process catch-up via `read_since(cursor)` on reconnect, takes acknowledgements to advance the cursor, and prunes by retention on the `umbral-tasks` beat. The guarantee is at-least-once with the durable cursor as the idempotency key; best-effort stays the default and does not change.
- **#44:** offline sync is a cursor-based pull-plus-push protocol (`SyncPlugin`, `Sync::syncable::<T>`, `GET {base}/sync/pull` + `POST {base}/sync/push`) that READS the #43/#31 durable change-feed, scoped and field-projected per model, with `ConflictStrategy::LastWriteWins` plus a `Merge(hook)` seam and a per-row `version`. The client half is an offline cache + mutation queue + optimistic apply SDK generated by the #37/#36 typegen pipeline. It is deliberately phased (server feed + protocol, then conflict/scoping, then the official SDK, then optional CRDT/collaboration), each phase independently useful, the SDK gated on demand.
- Both live entirely in `umbral-realtime` / a new `umbral-sync` plugin plus `umbral-outbox`; `umbral-core` never learns either exists. Arrows point inward, the plugin contract is the boundary, and the best-effort realtime default is preserved untouched.

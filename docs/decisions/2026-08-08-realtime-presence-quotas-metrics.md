# Realtime: presence and collaboration, quotas and abuse, operational metrics

Status: draft for ratification (proposes the design for gaps5 #46, #47, #48; the final call is the maintainer's)
Date: 2026-08-08
Closes: planning/gaps5.md #46 (tf #259), #47 (tf #260), #48 (tf #261)

## Context: what umbral-realtime already is

`umbral-realtime` (see `plugins/umbral-realtime/src/lib.rs`) is a thin, safe-by-default push layer over SSE (`GET /realtime/sse`, push-only) and WebSocket (`GET /realtime/ws`, bidirectional). The pieces the three items below build on already exist and are load-bearing:

- **A connection `Registry`** keyed three ways (`conns`, `by_user`, `by_group`), owning a process-global monotonic `seq`, a bounded replay buffer, and an optional aggregate connection cap. `dispatch(target, event)` is snapshot-then-send and returns the number of connections the event was queued to.
- **The `Broker` seam**: `InProcessBroker` for one process, `RedisBroker` (feature `redis`) for multi-instance pub/sub over `umbral:realtime:events`, with a bounded handoff queue (`QUEUE_CAP = 4096`) that drops the newest with a warning rather than growing without bound.
- **Identity**: `IdentityResolver` maps request headers to the authenticated user's PK string (PK-type-agnostic), `with_auth_sessions()` wires `umbral-auth`'s session cookie. Anonymous by default.
- **`GroupPolicy`** (`can_join` / `can_send`, with `PublicGroupsOnly`, `FnGroupPolicy`, `AsyncFnGroupPolicy`): the room-authorization seam. Default denies any non-`public:` group.
- **Presence already exists in primitive form**: `PresenceSpec`, `PresenceTransitions`, `Registry::register_with_presence` / `deregister_with_presence`, `dispatch_presence`, and the `presence:sync` / `presence:join` / `presence:leave` wire events. Presence is off per group by default, authenticated-only, deduped by user, id-only projection, and policy-gated. The `sync` roster is delivered only to the joining connection (audit_2 realtime #5), join/leave deltas go to the whole group.
- **The caps** (audit_2 realtime #4): `max_connections` (default 10k), `ws_max_messages_per_sec` (default 100), `ws_max_message_bytes` (default 1 MiB), plus the replay buffer size.
- **The signals bridge**: `on_table` / `on_model` / `expose` fan a model's create/update/delete to a group with a whitelisted projection (default id-only).

The gap these three items name is not primitives, it is **productization**: the primitives are there, but every app has to reinvent rooms, read receipts, typing, collaborative editing, per-tenant budgets, and instrumentation on top of them. The design principle throughout is the same as the rest of umbral: **stay a thin plugin, add opt-in primitives with safe defaults, do not turn the push layer into a stateful application**.

These three land as one feature area but ship independently. #48 (metrics) is the smallest and unblocks the dashboards #46 and #47 both want, so it lands first.

---

## #46 (tf #259): Presence and collaboration primitives

### Problem

`umbral-realtime` has live presence transitions but no *productized* collaboration surface. There is no durable presence model (last-seen survives no reconnect and no restart), no first-class rooms API (a room is just an ad-hoc group string plus a `GroupPolicy` predicate), and no read receipts, typing indicators, or collaborative-document primitives. Every chat/collab app rebuilds these by hand on top of `Realtime::to_group`.

### What exists vs. what is missing

| Capability | Today | This item adds |
|---|---|---|
| Live "who's online" | `PresenceSpec` + transitions, in-registry, transient | Keep as-is; add optional durable `Presence` model + `Rooms::roster()` query |
| Rooms | ad-hoc group strings + `GroupPolicy` | A `Room` convenience API over the same groups (join/leave/roster/broadcast), no new transport |
| Typing indicators | none | Ephemeral `typing:start` / `typing:stop` helpers with server-side TTL |
| Read receipts | none | Opt-in `ReadReceipt` model + `mark_read` / `receipts_for` helpers |
| Collaborative docs | none | A `DocumentSync` trait (CRDT/OT hook) with a reference last-writer-wins impl |
| Operator view | none | An `AdminView` presence/rooms dashboard |

### Design

**A. Optional durable presence (`Presence` model).** The live registry stays the source of truth for "connected right now". For "last seen" that must survive a reconnect or a process restart, add an opt-in model owned by the plugin's migrations:

```rust
// installed only when RealtimePlugin::with_durable_presence() is called
#[derive(Model)]
struct Presence {
    id: i64,
    user_id: String,        // the PK string, matching the identity resolver
    room: String,           // the group name
    tenant: Option<String>, // set from tenants::current_tenant() when present
    last_seen: DateTime<Utc>,
    status: PresenceStatus, // Online | Away | Offline
}
```

`dispatch_presence` already runs on every first-join / last-leave; the durable layer hooks the same transitions and upserts through the ORM (never raw SQL, per the plugin rule). `Presence` rows let a page render "last seen 4m ago" without a live socket, and let the operator dashboard show presence history. Off by default (no model, no migration) so a push-only app pays nothing.

**B. Rooms API.** A thin `Room` handle that names a group and routes every operation through the existing `GroupPolicy` and `Registry`, adding no new transport or index:

```rust
let room = Room::new("chat:42");
room.roster().await;                       // deduped present user ids (from present_user_ids)
room.broadcast("message", &msg).await;     // == Realtime::to_group("chat:42").send(...)
room.join(conn_id).await;                  // == Registry::join
room.members_durable().await;              // Presence rows, when durable presence is on
```

This is convenience over the primitives, not a parallel system: `Room` is sugar so app code stops re-deriving group-name conventions. `GroupPolicy::can_join` / `can_send` still governs every access.

**C. Typing indicators.** Typing is pure ephemeral signalling; it must never touch the DB or the replay buffer. Ship it as two helpers plus a server-side debounce so a client that forgets to send `typing:stop` auto-expires:

```rust
ctx.typing_start(&room).await;  // broadcasts typing:start {user} to the room, gated by can_send
ctx.typing_stop(&room).await;   // broadcasts typing:stop  {user}
```

The plugin holds a small per-(room, user) TTL map (default 8s); a missing `typing:stop` fires a synthetic `typing:stop` when the TTL lapses. Bounded, in-memory, dropped on restart by design.

**D. Read receipts.** Receipts are durable state, so they are an opt-in model plus helpers:

```rust
#[derive(Model)]
struct ReadReceipt { id: i64, room: String, user_id: String, up_to_seq: i64, read_at: DateTime<Utc> }

ctx.mark_read(&room, seq).await;           // upsert up_to_seq = max(current, seq)
Receipts::for_room(&room).await;           // { user_id -> up_to_seq } for rendering ticks
```

`up_to_seq` reuses the registry's existing monotonic `Event::seq`, so a receipt is "I have seen everything through event N in this room" with no new counter. `mark_read` also broadcasts a `receipt` event so other members' ticks update live.

**E. Collaborative-document hook.** Full CRDT/OT is out of scope to *implement*, but the seam is in scope so an app can plug `yrs` (Yjs), `automerge`, or a custom OT engine without forking the plugin:

```rust
#[async_trait]
trait DocumentSync: Send + Sync {
    // merge an inbound client update into doc state, return the update to fan out
    async fn apply(&self, doc: &str, update: &[u8], actor: Option<&str>) -> Result<Vec<u8>, SyncError>;
    // the full state a late-joiner needs to catch up
    async fn snapshot(&self, doc: &str) -> Result<Vec<u8>, SyncError>;
}
```

`RealtimePlugin::with_document_sync(impl DocumentSync)` wires a `MessageHandler` that, per inbound `doc:update` frame, runs `can_send` on the doc's room, calls `apply`, and broadcasts the merged update to `doc:<id>`. We ship one reference impl, `LastWriterWins` (coarse, whole-document, no true merge), documented as a starting point; a real app supplies a CRDT. This keeps umbral from reimplementing a merge engine (arch.md "do not reimplement primitives") while making the integration a one-liner.

**F. Operator dashboard.** An `AdminView` (the custom-view surface already shipped: `plugins/umbral-admin/src/lib.rs`, `AdminPlugin::view(AdminView)`) rendering live rooms, per-room roster size, present users, and (when durable presence is on) last-seen history. Its data endpoint reads the registry via the metrics surface from #48, so it needs no privileged registry access beyond the existing `Realtime::registry()`.

### Safety defaults

Everything here is off unless opted in. Durable presence and read receipts add no table until their builder is called. Typing is bounded in-memory. Every broadcast path (rooms, typing, receipts, doc updates) runs through `GroupPolicy::can_send`, so a client cannot inject into a room it may not post to (the audit_2 realtime #2 IDOR guard is preserved). Presence projection stays id-only unless the dev supplies a `resolver`.

### Deferred

True server-side CRDT/OT merge (we ship the hook and a trivial reference, not a merge engine), presence heartbeat tuning knobs beyond a single TTL, and cross-room "global presence" rollups.

---

## #47 (tf #260): Realtime quotas and abuse controls

### Problem

The caps today are **global and per-connection**: `max_connections` (whole node), `ws_max_messages_per_sec` (one socket), `ws_max_message_bytes` (one frame). There is no notion of a **tenant** or **channel budget**, so one noisy tenant can consume the entire node's 10k connection budget and starve everyone else, and there are no counters feeding the metering subsystem (#84). This item adds per-tenant and per-channel quotas, usage counters, and noisy-neighbor isolation, all built on the existing caps rather than replacing them.

### Design

**A. A quota spec keyed by tenant and channel.** Reuse `tenants::current_tenant()` (already ambient: `plugins/umbral-tenants/src/lib.rs:709`) to attribute every connection and message to a tenant. Add an opt-in `RealtimeQuota`:

```rust
RealtimePlugin::new()
    .with_quota(RealtimeQuota::default()
        .max_connections_per_tenant(500)
        .max_messages_per_sec_per_tenant(2_000)
        .max_connections_per_channel(200)
        .max_subscriptions_per_connection(50));
```

The registry gains per-tenant and per-channel counters alongside its existing indexes (it already tracks `by_user` and `by_group`, so `by_tenant` is the same shape). `register` / `register_with_presence` already return `None` when the aggregate cap is hit and the transports turn that into `503`; the tenant/channel check slots into the same admission point and returns the same `503`, so no new transport code path. A per-tenant message-rate check joins the existing per-connection `ws_max_messages_per_sec` gate in the WS read loop.

**B. Noisy-neighbor isolation.** Two mechanisms, both cheap:

1. **Fair-share admission.** When the node is near `max_connections`, admission is denied for any tenant already over its `max_connections_per_tenant` share *before* it is denied for a tenant under its share. This keeps one tenant's connection storm from consuming a departing tenant's freed slots.
2. **Per-tenant send budget.** Outbound fan-out already drops on a full per-connection buffer (best-effort `try_send`). Add a per-tenant token bucket on the *publish* side (`Target::send` / the broker hand-in) so a tenant that floods `broadcast` cannot monopolize the broker's bounded Redis queue (`QUEUE_CAP`). Over-budget publishes are dropped with a warning and counted, exactly as the existing Redis-queue-full path does, so the failure mode is already-understood: a missed live update, never lost durable data or a blocked request handler.

**C. Billing-meter counters (feeds #84).** Every admission and every delivered/dropped message increments a counter tagged by tenant and channel. These are the same counters #48 exposes to Prometheus, but here they are also drained into the metering subsystem's per-tenant usage rows (#84) so realtime becomes a metered resource (connections opened, connection-seconds, messages in/out). The counter surface is defined once (in #48) and consumed by both the exporter and the meter, so there is one source of truth.

```rust
// the shared counter surface (see #48)
metrics.record_connection_opened(tenant, channel);
metrics.record_message(tenant, channel, MessageOutcome::Delivered | Dropped);
```

**D. Quota exhaustion is observable, not silent.** A tenant hitting its connection quota gets a `503` with a `Retry-After`; a tenant over its message budget gets its socket closed with a close-frame reason (`quota_exceeded`), the same shape the flood cap already uses. Both increment a `realtime_quota_rejections_total{tenant, reason}` counter so the operator dashboard (#46 F) and alerts can see abuse building.

### Safety defaults

Quotas are opt-in (`with_quota`); an app that does not install tenants or call `with_quota` behaves exactly as today, governed only by the global caps. Fail-safe direction: when tenant attribution is unavailable (`current_tenant()` is `None`), a connection is charged to a synthetic `"_untenanted"` bucket rather than being exempt, so a misconfiguration cannot become an unlimited-usage bypass.

### Ties to other items

- **#84 (metering)**: the per-tenant counters here are the realtime rows of the metering subsystem. This item defines *what* realtime meters; #84 owns aggregation, plan limits, and billing hooks.
- **#19 (abuse controls)**: the abuse plugin composes throttles and lockout policies; a repeated `quota_exceeded` close is exactly the event-based lockout signal #19 wants. We emit the signal; #19 decides the lockout policy.
- **#67 (distributed rate limiting)**: single-node quotas here are exact; multi-node per-tenant limits need the shared Redis limiter #67 introduces. Until then, per-tenant caps are per-replica (documented, same caveat as the existing throttles), and the Redis broker's shared counters give an approximate global view.

### Deferred

Exact cross-replica per-tenant enforcement (waits on #67's Redis limiter), plan-tier quota presets (belongs to #84), and CAPTCHA/challenge escalation (belongs to #19).

---

## #48 (tf #261): Realtime operational metrics

### Problem

There is no way to see a realtime deployment's health: open connections, dropped messages, buffer pressure, reconnects, broker lag, and channel fan-out are all invisible. The observability plugin (`umbral-logs`) exports OTLP traces but there is no Prometheus `/metrics` exporter anywhere (gaps5 #64). This item defines the realtime metric surface and emits it so the #64 exporter can scrape it, and so the #46 and #47 dashboards have data.

### Design

**A. A metrics surface owned by the registry/broker.** Add a `RealtimeMetrics` handle (an `Arc` of atomics plus a small histogram) that the hot paths already touch increment. It is populated whether or not an exporter is installed; the exporter (from #64) reads it, and so does the operator dashboard (#46 F). The metrics are derived from data the code already computes:

| Metric | Type | Source (already computed today) |
|---|---|---|
| `realtime_connections_open` | gauge | `Registry::connection_count()` (exists), tagged by transport/tenant/channel |
| `realtime_connections_total` | counter | `register` admission point |
| `realtime_messages_dropped_total` | counter | `dispatch`: `senders.len() - delivered` (the try_send failures) |
| `realtime_messages_delivered_total` | counter | `dispatch` returns `delivered` |
| `realtime_buffer_pressure` | histogram | per-connection channel `capacity()` vs `DEFAULT_BUFFER` at send time |
| `realtime_reconnects_total` | counter | SSE `Last-Event-ID` replays (`replay_since` call count) |
| `realtime_replay_events_total` | counter | events returned by `replay_since` |
| `realtime_broker_queue_depth` | gauge | `RedisBroker` handoff channel len vs `QUEUE_CAP` (the lag signal) |
| `realtime_broker_dropped_total` | counter | the existing `TrySendError::Full` warn path |
| `realtime_channel_fanout` | histogram | `senders.len()` per `dispatch` (recipients per event) |

Crucially, **almost every metric is a number `dispatch` / the broker / the transports already produce** (delivered count, drops, fan-out size, queue fullness); this item just records them instead of discarding them. The one genuinely new bit of bookkeeping is the open-connections gauge tagged by tenant/channel, which reuses the #47 `by_tenant` index.

**B. Emission is decoupled from export.** `RealtimeMetrics` is a plain counter/gauge holder in `umbral-realtime` with no Prometheus dependency; the `/metrics` HTTP exporter lives in the metrics plugin from #64 and pulls from a registered provider. This keeps the plugin dependency-light (a push-only app that never scrapes metrics pulls in no exporter crate) and matches the dependency-inversion rule: realtime emits, the exporter consumes, realtime never depends on the exporter.

```rust
// #64 provides the exporter and a registration seam; realtime registers its provider:
metrics_registry.register(RealtimeMetrics::provider());
// GET /metrics (owned by the #64 plugin) then renders realtime gauges/counters alongside HTTP/DB/tasks.
```

**C. Cardinality guard.** Per-channel labels are unbounded (a chat app has millions of rooms), which would explode Prometheus cardinality. Default emission is tagged by **transport and tenant only**; per-channel breakdowns are available on the operator dashboard (which queries the live registry directly, no time-series retention) and behind an explicit `RealtimeMetrics::with_per_channel_labels()` opt-in for apps with bounded channel counts. This is the standard cardinality-vs-detail tradeoff, decided in favor of a safe default.

**D. Dashboard panels.** The #46 operator `AdminView` renders these same metrics as live panels (open connections, drop rate, fan-out distribution, broker queue depth). Per the dogfooding note, charts route through ApexCharts and never hand-rolled SVG.

### Safety defaults

Metrics collection is always on (the atomics are cheap and the data is already computed), but the `/metrics` endpoint is owned by the #64 plugin and is not mounted unless that plugin is installed and its route is authorized. No metric carries user PII: identity appears only as opaque PK strings inside the dashboard's live roster, never in a scraped label.

### Ties to other items

- **#64 (metrics)**: this item is the realtime slice of the framework-wide Prometheus exporter. It defines the realtime metric names and emission; #64 owns the `/metrics` route, the exporter format, and the HTTP/DB/tasks/cache/storage slices.
- **#47**: the per-tenant counters here and the quota counters there are the *same* surface; defining them once avoids double-counting.
- **#66 (DB/task spans)**: broker-lag and fan-out here complement the per-operation spans #66 adds, giving both a time-series (metrics) and a trace (spans) view of a slow dispatch.

### Deferred

The `/metrics` HTTP exporter and format (belongs to #64), trace-context propagation into realtime frames (belongs to #65), and long-term per-channel time-series retention (the dashboard reads live registry state instead).

---

## Summary

All three items are **productization on top of primitives that already exist**, not new subsystems. #46 turns the existing presence transitions and group dispatch into a rooms/typing/receipts/collab surface with an operator dashboard, keeping full CRDT merge as a pluggable hook rather than an implementation. #47 adds per-tenant/per-channel quotas and noisy-neighbor isolation by extending the existing caps and admission point, and defines the realtime counters that feed metering (#84). #48 records the delivered/dropped/fan-out/queue-depth numbers the hot paths already compute and exposes them to the #64 Prometheus exporter, with a cardinality guard defaulting to tenant-level labels. Each ships independently, each is opt-in, and each preserves the existing safe-by-default posture (default-deny groups, id-only projection, authorized publish, bounded buffers).

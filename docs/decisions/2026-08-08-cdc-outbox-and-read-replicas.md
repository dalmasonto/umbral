# CDC / transactional outbox and read-replica / failover operations

Status: draft for ratification (proposes the shape of gaps5 #31 and gaps5 #32)
Date: 2026-08-08
Relates: planning/gaps5.md #31 (tf#244), #32 (tf#245), #42 (tf#255, webhook endpoint management), #52 (tf#265, transactional outbox / after-commit)

## Scope

Two data-streaming items, one doc, because they are the two halves of "your data leaves the database safely and comes back correctly":

- **#31** is the outbound stream: a durable, transactional record of every committed change, published exactly once after commit to pluggable destinations (webhook, realtime, analytics), with retries and delivery logs.
- **#32** is the inbound-consistency story: formalizing read-replica routing so reads go to replicas, writes go to the primary, a caller gets read-your-writes when it needs it, and the app degrades safely when a replica lags or fails.

Both build on real seams that already exist. #31 sits on the ORM's after-commit signals (`umbral-signals`) and the durable retry loop (`umbral-tasks`). #32 builds on the `DatabaseRouter` trait in `crates/umbral-core/src/db/router.rs` and the `examples/read-replica` demo.

This doc defers webhook-endpoint management (registration, secrets, replay, per-tenant quotas, admin UI) to #42 and treats #31 as producing the *event*; #42 owns *where the event is delivered*. It also closes the design of #52 by naming the outbox + after-commit path as the one blessed way to do "write a row, then do something exactly after it commits".

---

## Part 1 (#31): CDC / transactional outbox and the after-commit publisher

### The problem, stated against what exists

Today umbral has two ways a committed change can trigger downstream work, and neither is durable across a process crash:

1. **`umbral-signals`.** The ORM fires `post_save:<table>` / `post_delete:<table>` / `bulk_post_save:<table>` / `m2m_changed:<junction>` after a write. These already fire *after commit* - the ORM comments are explicit that `bulk_post_save` and per-row `post_save` fire only once the row is durable (e.g. `crates/umbral-core/src/orm/queryset/mod.rs` around the `emit_bulk_post_save` / `emit_post_save` calls, and `crates/umbral-core/src/orm/dynamic.rs` "bulk_post_save fires only after commit"). But signals are **strictly in-process** (`plugins/umbral-signals/src/lib.rs`, "In-process only at v1"). A handler that enqueues an email or POSTs a webhook runs in the same process; if that process dies between the commit and the handler completing, the side effect is lost. Signals have no persistence, no retry, no delivery record.

2. **`umbral-tasks`.** The DB-backed queue *is* durable - `enqueue` writes a `pending` row to the app's own pool, the worker claims it, retries with exponential backoff, and records terminal state (`plugins/umbral-tasks/src/lib.rs`). But `enqueue` is a bare INSERT the caller makes by hand. If the caller enqueues **before** its own transaction commits, a rollback leaves an orphan task pointing at a row that never existed. If it enqueues **after** commit (in a `post_save` signal handler), the process can die in the window between commit and enqueue, and the task is silently never created. This is the dual-write problem: two writes (business row + task row) that must both happen or neither, but which today are not atomic.

The transactional outbox pattern solves exactly this: write the *intent to publish* into an outbox table **inside the same transaction as the business change**, so it commits atomically with the data. A separate relay reads the outbox after commit and publishes. Because the outbox row and the business row share one commit, there is no window where one exists without the other.

### The design: `umbral-outbox` plugin

A new built-in plugin, `plugins/umbral-outbox`, depending only on the `umbral` facade plus `umbral-tasks` (for the durable relay) and `umbral-signals` (to observe model changes). It contributes one core model and one relay.

#### The outbox table

An `OutboxEvent` model (table `outbox_event`), owned by the plugin, migrated the normal way (`plugin.migrations()`), one row per change to publish:

| Column | Type | Meaning |
|---|---|---|
| `id` | PK | monotonic event id (also the ordering key within an aggregate) |
| `event_type` | String | `"created" \| "updated" \| "deleted"`, or an app-defined name for a domain event |
| `aggregate` | String | the source table (`ModelMeta::table`) or a logical stream name |
| `aggregate_id` | String | the changed row's PK, PK-shape independent (i64/String/Uuid all serialize to String, matching `RouteContext::user`'s existing shape) |
| `payload` | Json | the serialized row (or a change delta) - exactly what a typed `post_save` handler already receives |
| `destinations` | Json | the set of destination names this event fans out to (`["webhook","realtime","analytics"]`) |
| `created_at` | DateTime | commit-time-adjacent enqueue instant |
| `published_at` | Option<DateTime> | NULL until the relay has delivered to every destination |
| `attempts` | i32 | delivery attempts so far (the relay is idempotent on this) |
| `available_at` | DateTime | next eligible delivery instant; backoff pushes it forward |

A partial index on `(published_at) WHERE published_at IS NULL` (Postgres) keeps the relay's "unpublished, due" scan cheap as the table grows; SQLite gets the equivalent non-partial index. This is DDL owned by the migration engine - the one allowed raw-SQL exception.

**Retention.** Published rows are not deleted inline (that would turn every publish into a write-write). A periodic `#[task]` prunes `outbox_event WHERE published_at < now() - retention` on a schedule the operator sets (default 7 days), so the outbox stays bounded without the relay paying for deletes on the hot path. This reuses `umbral-tasks`' `periodic` beat rather than inventing a second scheduler.

#### Writing to the outbox: two paths, both atomic-with-the-change

1. **Automatic model-change capture (CDC-style).** The plugin registers, in `on_ready`, a set of model signal subscribers via `umbral-signals`' `on_model::<M>()` for every model the operator opts in (`OutboxPlugin::capture::<Post>()`). When `post_save` / `post_delete` fires, the subscriber writes an `OutboxEvent` through the ORM (`OutboxEvent::objects().create(...)`, never raw SQL - the plugin-uses-the-ORM rule). Because ORM signals already fire **after commit**, this path is "capture the change I can already see is durable". It does *not* get the same-transaction atomicity of the outbox proper - it inherits the exact crash window signals have today (process dies between the business commit and the outbox INSERT). It is the low-ceremony option and is honest about that ceiling.

2. **In-transaction enqueue (the true transactional outbox, the #52 blessing).** For the strong guarantee, the outbox INSERT must ride the *same* transaction as the business write. umbral already has the seam: `umbral::db::transaction(|tx| ...)` and `Model::objects().on_tx(&mut tx)`. The plugin offers `outbox::publish_on(tx, event).await?` that does `OutboxEvent::objects().on_tx(tx).create(event)` - so the outbox row and the business rows commit together or roll back together:

 ```rust,ignore
 umbral::db::transaction(|tx| Box::pin(async move {
 let order = Order::objects().on_tx(tx).create(new_order).await?;
 outbox::publish_on(tx, Event::created("order", &order)).await?;
 Ok::<_, MyError>(order)
 })).await?;
 ```

 This is the path email (#53), webhooks (#42), analytics, and realtime all route through when they need "exactly after commit, never lost". It is the answer to #52: `after_commit` is not a new ORM hook we bolt on; it is *"write an outbox row in your transaction and let the relay fire after commit"*. The relay's read of a committed outbox row IS the after-commit point, and it is durable by construction.

 We deliberately do **not** add a general `Transaction::on_commit(closure)` callback. An in-process closure is exactly the thing that dies with the process; the whole point of the outbox is to move the after-commit intent into durable storage. Naming the outbox as the after-commit primitive keeps one mechanism instead of two.

#### The relay: publishing after commit, durably, on umbral-tasks

The relay is a periodic task (`#[task] outbox_relay`) scheduled via `TasksPlugin::periodic`, plus an optional low-latency `NOTIFY`-driven wake on Postgres. Each run:

1. Claims a batch of due, unpublished events with the same optimistic conditional-`UPDATE` claim pattern `umbral-tasks`' `claim_one` already uses (order by `id`, `available_at <= now()`, `published_at IS NULL`; a conditional update stamps a claim so two relay instances can't double-send). This reuses the queue's existing at-least-once claim discipline rather than inventing locking.
2. For each event, dispatches to each named destination (below). Success on all destinations stamps `published_at`. A partial or total failure increments `attempts` and pushes `available_at` forward by the same exponential backoff `umbral-tasks` uses (`retry_backoff_base * 2^(attempts-1)`, capped at `retry_backoff_max`), then abandons to a dead-letter state after a max-attempts ceiling - mirroring the queue's retriable-failure semantics so operators reason about one backoff model, not two.
3. Records a **delivery log** row per (event, destination, attempt): destination, status code / error, latency, attempt number, timestamp. This is the audit trail #31 and #42 both need; it is a second small model (`outbox_delivery`) written through the ORM.

Delivery is **at-least-once**: a crash after the destination received the event but before `published_at` was stamped re-delivers. Destinations are therefore expected to be idempotent, and the event `id` is the idempotency key we hand every destination (a webhook gets it as a header, realtime as the event id, analytics as the dedupe key). This is the honest guarantee a DB-backed outbox can make without two-phase commit; we document it as at-least-once, not exactly-once.

#### Pluggable destinations

A `Destination` trait - structurally a mini-plugin, in keeping with "the framework dogfoods its own plugin system":

```rust,ignore
#[async_trait]
pub trait Destination: Send + Sync {
 fn name(&self) -> &'static str;
 async fn deliver(&self, event: &OutboxEvent) -> Result<(), DeliveryError>;
}
```

Three built-ins ship:

- **`webhook`** - POSTs the event payload to a URL. #31 produces the event and does the durable-send-with-retry; **#42 owns endpoint management** (which URLs, per-tenant, secrets/HMAC signing, replay, quotas, admin UI). The webhook `Destination` here calls into #42's endpoint registry to resolve targets and sign requests. Until #42 lands, the webhook destination takes a static URL + shared secret from settings.
- **`realtime`** - hands the event to `umbral-realtime` to fan out to subscribed clients. This upgrades realtime from best-effort (gaps5 #43) to durable-at-the-source for the subset of events that flow through the outbox: the client may still miss a live frame, but the event is retained and replayable from `outbox_event`.
- **`analytics`** - appends to an analytics sink (the analytics plugin, or an external warehouse via a thin adapter). Analytics is the canonical CDC consumer: a durable, ordered, replayable change stream is exactly a warehouse-ingestion feed.

Because destinations are a trait, an app adds its own (`OutboxPlugin::destination(MyKafkaSink)`) with no framework change - the same extension shape as a third-party plugin.

### Why this shape

- **It stands on real seams, not new infrastructure.** After-commit ordering already exists in the ORM's signal emission; durable retry + backoff + claim already exist in `umbral-tasks`; periodic relay scheduling already exists in the beat. #31 is composition of three shipped mechanisms plus two models, not a new subsystem.
- **It resolves the dual-write problem honestly.** The in-transaction `publish_on(tx, ...)` path gives true atomicity (outbox row commits with the business row); the automatic-capture path is offered for convenience with its crash window named, not hidden.
- **It gives #52 a concrete answer.** "The blessed after-commit path" is the outbox, and it is the same path for email, tasks, webhooks, analytics, and realtime - one mechanism, five consumers.
- **`umbral-core` stays plugin-free.** The outbox is a plugin; core never learns it exists. The relay reaches `umbral-tasks` and destinations reach `umbral-realtime` / analytics through the facade, arrows pointing inward.

### Deferred / out of scope for #31

- Webhook endpoint registry, secrets, replay, quotas, admin UI → **#42**.
- Exactly-once delivery (needs consumer-side dedupe infra we don't own) - we ship at-least-once + idempotency key.
- Logical-replication CDC (reading the Postgres WAL directly, à la Debezium) - the outbox is application-level CDC; WAL-level CDC is a separate, heavier future item and is noted, not designed here.
- Ordering guarantees stronger than per-`aggregate_id` FIFO (the `id`/`available_at` ordering gives per-stream order; global total order across aggregates is not promised).

---

## Part 2 (#32): read-replica routing and failover operations

### The real seam that exists today

Read-replica routing is **already wired**, not aspirational. The mechanism:

- `crates/umbral-core/src/db/router.rs` defines `trait DatabaseRouter` with `db_for_read(model, ctx) -> Alias` and `db_for_write(model, ctx) -> Alias`. Every ORM terminal consults it: a read terminal resolves `db_for_read`, a write terminal resolves `db_for_write`, and the resolved `Alias` selects a pool registered under that name.
- `RouteOp::{Read, Write}` is the read/write discriminator the terminal passes in.
- Pools are registered by alias at build (`App::builder().database("default", primary).database("replica", replica)`) into the `POOLS` map in `crates/umbral-core/src/db.rs`, and resolved by `pool_for_dispatched(alias)`.
- `examples/read-replica/src/main.rs` is a working demo: a `ReplicaRouter` returns `Alias::new("replica")` from `db_for_read` and `Alias::new("default")` from `db_for_write`, wired via `.router(ReplicaRouter)`. Its README documents the escape hatch: `Note::objects().on(&primary).fetch()` pins a pool and bypasses the router for read-your-writes.
- `RouteContext` (`crates/umbral-core/src/db/route_context.rs`) is the per-request task-local the router reads; it already carries tenant, user, and session vars, and is extensible via a typed `extensions` store - so a router can stash and read a per-request "must-read-primary" flag with no signature change.

So the routing decision point, the pool registry, the read/write discriminator, and the per-request context are all real. What #32 adds is **policy, safety, and operations** on top of that seam: it does not need new plumbing, it needs a productized `ReplicaRouter`, lag awareness, a read-your-writes strategy, and a runbook.

### What #32 adds

#### 1. A blessed `ReplicaRouter` policy (not just an example)

Promote the `examples/read-replica` router into a configurable, shipped policy - either in a small `umbral-replicas` plugin or as a constructor in core's router module:

```rust,ignore
App::builder()
 .database("default", primary)
 .database("replica_a", replica_a)
 .database("replica_b", replica_b)
 .router(ReplicaRouter::new()
 .replicas(["replica_a", "replica_b"]) // round-robin / random reads
 .writes_to("default"))
```

`db_for_write` always returns the primary alias. `db_for_read` picks among healthy replicas (round-robin or random). Per-model overrides still fall through to `Model::DATABASE` / `Plugin::database()` via the existing `default_alias_for` precedence, so a model pinned to its own DB is unaffected. This is a thin, config-driven impl of the trait that already exists - no ORM change.

#### 2. Read-your-writes

Three layers, weakest ceremony first:

- **Explicit pin (works today):** `Model::objects().on(&primary).fetch()` bypasses the router for one query. Documented as the surgical tool.
- **Request-scoped stickiness (new):** after any write in a request, the router marks the request "read primary until end of request" by setting a flag in `RouteContext`'s typed `extensions` (the store already exists). `db_for_read` checks the flag and returns the primary alias for the rest of that request. This gives read-your-writes for the common "POST then GET on the same request / same logical action" case without the caller thinking about it. The flag is set by the write terminal path when the active router opts in (`ReplicaRouter::read_your_writes(true)`).
- **Timestamp/LSN gating (advanced, Postgres):** for read-your-writes *across* requests, record the primary's commit LSN at write time and only route a subsequent read to a replica whose applied LSN has caught up (`pg_last_wal_replay_lsn()` vs the recorded LSN); otherwise fall back to primary. This is opt-in and Postgres-only; SQLite has no replicas.

The example README already notes that `get_or_create` / `update_or_create` read-your-writes by probing the primary, so those compound terminals are safe against replica lag by construction - #32 keeps that and generalizes it.

#### 3. Replica lag checks and readiness gating

A lag probe per replica pool:

- **Postgres:** `SELECT EXTRACT(EPOCH FROM (now() - pg_last_xact_replay_timestamp()))` on the replica gives seconds-behind-primary; on the primary side `pg_wal_lsn_diff` gives byte lag. The probe runs on a schedule (a `#[task] periodic`) and stores per-replica lag.
- **Routing decision:** `ReplicaRouter::max_lag(Duration)` - a replica whose measured lag exceeds the threshold is dropped from the read set and reads fail over to another healthy replica or the primary. This is the "degrade safely" behavior: a lagging replica stops receiving reads instead of serving stale data.
- **Readiness gating:** the health/readiness endpoint (the `umbral::db::ping` liveness check already exists in `db.rs`) is extended so a replica-backed deployment reports **not ready** when *all* replicas exceed the lag threshold or are unreachable - so a load balancer / orchestrator stops routing traffic to a node that can only serve dangerously-stale reads. Liveness (`SELECT 1`) stays separate from readiness (lag-aware), matching the standard k8s split.

#### 4. Failover

- **Read failover (automatic):** already covered by the lag/health-aware read set - an unreachable or lagging replica is removed and reads go to a healthy replica or the primary. Because the router is consulted per terminal, failover is transparent to models and handlers.
- **Write failover / primary promotion (operational runbook, not automatic magic):** promoting a replica to primary is a database-cluster operation (managed Postgres, Patroni, etc.), not something the framework should silently do - an app that auto-promotes on a transient blip risks split-brain. #32 ships a **runbook** in the operations reference (ties gaps5 #4): detect primary loss (writes to `default` fail health checks), gate writes (return 503 / enqueue to the durable outbox so writes are not lost while the primary is down - the #31 outbox doubles as a write buffer here), promote a replica out-of-band, re-point the `default` alias's pool URL, and restart / hot-swap. The `connect_lazy` pools in `db.rs` connect on first use, which eases re-pointing.

#### 5. Observability

Per-replica lag, read/write routing counts, failover events, and outbox relay lag are surfaced as metrics (ties gaps5 #64, metrics) and are the raw material for the "replica dashboard" the gap asks for. The dashboard itself is an admin custom view (the `AdminPlugin::view` seam already exists).

### Why this shape

- **It builds on the real seam.** The `DatabaseRouter` trait, `RouteOp`, the alias-keyed pool registry, and `RouteContext`'s extensible store are all shipped. #32 is a productized router policy plus lag/health/readiness logic plus a runbook - it adds zero new plumbing to the ORM's routing path.
- **It keeps writes safe.** Automatic *read* failover is safe and transparent; automatic *write* failover (promotion) is deliberately a documented human/orchestrator operation, with the outbox as a write buffer so a primary outage doesn't drop writes.
- **Readiness vs liveness stays honest.** A lagging node reports not-ready rather than quietly serving stale reads, which is the operable behavior at more than one replica (the Stage 2 self-hosted-platform posture from the product north star).

### Deferred / out of scope for #32

- Automatic primary promotion / consensus (Patroni's job, not the framework's).
- Multi-region / residency routing (gaps5 #85, Stage 3).
- A hosted replica dashboard beyond the admin custom view.

---

## Summary of the contract

- **#31:** a `umbral-outbox` plugin adds an `outbox_event` table written either automatically from ORM after-commit signals or, for true atomicity, inside the business transaction via `outbox::publish_on(tx, event)`. A periodic relay on `umbral-tasks` publishes each event at-least-once to pluggable `Destination`s (webhook / realtime / analytics), with exponential backoff, a dead-letter ceiling, and a per-attempt delivery log. This is the blessed after-commit path (#52); webhook endpoint management is deferred to #42.
- **#32:** promote the `examples/read-replica` `DatabaseRouter` into a configurable `ReplicaRouter` (reads to replicas, writes to primary), add request-scoped read-your-writes via `RouteContext` extensions plus the existing `.on(&primary)` pin, add per-replica lag probes that drop lagging replicas from the read set and gate readiness, and ship a write-failover runbook (with the #31 outbox as a write buffer during a primary outage). All of it rides the existing `DatabaseRouter` / `RouteOp` / alias-pool seam with no new ORM plumbing.

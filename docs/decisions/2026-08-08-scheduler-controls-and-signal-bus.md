# Scheduler operator controls and an optional distributed signal bus

Status: draft for ratification (proposes the shape of gaps5 #56 and gaps5 #57)
Date: 2026-08-08
Relates: planning/gaps5.md #56 (tf#269, scheduler controls), #57 (tf#270, distributed signals)
Reads on: `plugins/umbral-tasks/src/lib.rs` (`run_beat`, `fire_due_periodic`, `PeriodicTask`, `Schedule`), `plugins/umbral-signals/src/lib.rs` (`on_model`, `emit`/`subscribe`), `plugins/umbral-realtime/src/lib.rs` (the `Broker` / `RedisBroker` seam), `docs/decisions/2026-08-08-cdc-outbox-and-read-replicas.md` (the durable outbox)

## Scope

Two backlog items, one doc, because they share a theme: taking a v1 primitive that is correct-but-minimal and giving it the operator-grade and multi-node surface a self-hosted platform (the Stage 2 posture in `2026-08-08-product-north-star.md`) needs.

- **#56** turns `PeriodicTask` from "a schedule row a beat process advances" into an operator scheduler: pause/resume with intent, per-schedule locks against overlap, an explicit missed-run policy, timezone-aware schedules with a calendar preview, and visibility into which node is actually beating.
- **#57** adds an optional cross-process transport behind the existing `emit`/`subscribe` signal API so a signal emitted on one instance reaches subscribers on another, while keeping in-process the default and making the local-only semantics explicit. It deliberately does NOT try to be durable; durability is the outbox's job (`umbral-outbox`, gaps5 #31), and this doc draws that line.

Both build on seams that already exist. #56 extends the `PeriodicTask` model and `fire_due_periodic`. #57 mirrors, almost verbatim, the `Broker` / `InProcessBroker` / `RedisBroker` pattern `umbral-realtime` already ships.

---

## Part 1 (#56): the scheduler, from primitive to operator surface

### What exists today (accurately)

`umbral-tasks` ships a working periodic scheduler ("beat"):

- **The model.** `PeriodicTask` carries `name` (unique, the stable key), `task` (the handler name to enqueue), `payload` (JSON args), `schedule` (a serialized `Schedule`), `next_run`, `last_run: Option`, `enabled: bool`, and `created_at` / `updated_at`. One row per schedule name.
- **The schedule type.** `Schedule` is `Cron(String)` (a standard 5-field expression, normalized to the `cron` crate's 6-field `sec min hour dom mon dow` by prepending a `0 ` seconds field) or `Every(Duration)`. It serializes to a single string column (`"cron:0 0 * * *"` / `"every:3600"`). `Schedule::next_after(after)` computes the next fire instant. **Everything is UTC**: the `cron` crate evaluates against UTC and every timestamp column is `DateTime<Utc>`.
- **The beat loop.** `run_beat` syncs the builder-registered `PeriodicSpec`s to rows once on startup (`sync_registered_periodic`), then each tick (default 5s) calls `fire_due_periodic`, which selects every `enabled` row whose `next_run <= now`, and for each one does an **optimistic conditional UPDATE**: `... WHERE id = ? AND next_run = <the value we read>` advancing `next_run` to `next_after(now)` and stamping `last_run`. Only if that UPDATE affected exactly one row does it `enqueue_periodic` the underlying task. A second beat instance that read the same row loses the race (its `next_run` guard no longer matches, zero rows affected, it enqueues nothing).

### The three things that are true about that design

1. **The "double-fire guard" is leaderless, not leader-election.** There is no leader, no lock table, no node identity persisted anywhere. Correctness comes from the per-row conditional UPDATE: N beat instances can all run and the row-level guard guarantees each fire is enqueued once. This is a good property (no split-brain failure mode, no lease to lose), but it means the audit's phrase "which node holds the beat lock" describes something that does not exist yet, because there is no lock.
2. **The guard prevents double-*enqueue*, not overlapping *runs*.** Beat only inserts a `TaskRow`. If a schedule fires hourly but the underlying task takes 90 minutes, beat happily enqueues a second copy while the first is still `running` on a worker. Nothing today serializes the runs of one schedule.
3. **Missed runs coalesce to one, silently.** If beat is down for six hours across an hourly schedule, `next_run` sits six hours in the past. When beat returns, `fire_due_periodic` fires the row **once** and advances `next_run` to `next_after(now)`, so the five intervening fires are dropped with no record and no choice. That is a reasonable default (it is Celery beat's default too) but it is currently implicit and unconfigurable.

Partial surface that already exists: `periodic_admin_model()` gives an admin list of schedules with an "Enable / disable selected" bulk action editing `enabled`, and `admin_model()` gives the queue view. So pause/resume exists in skeleton (`enabled`); the gap is intent, safety, and the four capabilities below.

### The design: additive columns plus a beat-lease, all migrated the normal way

Every addition is an additive, nullable-or-defaulted column on `PeriodicTask` (the same additive-migration discipline the model's doc comment already calls out) plus one new small model for the optional lease. No raw SQL; all reads/writes go through the ORM.

#### 1. Pause / resume with intent

`enabled` stays the on/off switch, but pausing becomes a first-class action rather than a bare boolean flip:

| New column | Type | Meaning |
|---|---|---|
| `paused_at` | `Option<DateTime<Utc>>` | when it was paused (NULL when running) |
| `paused_reason` | `Option<String>` | free text an operator leaves ("incident 412, muting the digest") |
| `resume_policy` | `String` | what to do with the time spent paused when resumed: `"skip"` (default) or `"catch_up"` (see missed-run policy below) |

Admin gains three actions on the schedules view, beyond the existing toggle:

- **Pause** (bulk): sets `enabled = false`, stamps `paused_at`, prompts for `paused_reason`.
- **Resume** (bulk): sets `enabled = true`, clears `paused_at`, and applies `resume_policy` to the gap (skip forward to `next_after(now)`, or enqueue catch-up fires up to the bounded cap).
- **Run now** (single): enqueue the underlying task immediately without disturbing `next_run`. This is the "I do not want to wait until 3am to test the nightly job" action, and it is the operator action `admin_model`'s queue view cannot express today. It writes a `TaskRow` via the normal `enqueue` path and records nothing on the schedule other than an optional `last_manual_run` timestamp.

`paused_reason` and `paused_at` show read-only in the admin so an on-call engineer at 3am sees *why* a schedule is off and who muted it, instead of an unexplained `enabled = false`.

#### 2. Per-schedule locks (overlap policy)

A new column controls whether a schedule's runs may overlap:

| New column | Type | Meaning |
|---|---|---|
| `overlap` | `String` | `"allow"` (default, today's behavior), `"skip"` (do not fire while the previous run is unfinished), or `"queue_one"` (at most one queued-or-running at a time) |
| `last_task_id` | `Option<i64>` | the `TaskRow.id` of the most recent fire, so beat can check its state |

When `overlap != "allow"`, `fire_due_periodic`, after winning the `next_run` conditional UPDATE, checks `last_task_id`'s `TaskRow`: if it is still `pending` or `running`, beat records the fire as **skipped** (advances `next_run`, does not enqueue) for `"skip"`, or leaves `next_run` unadvanced-past-due for `"queue_one"` so it retries next tick. This is a per-schedule mutex expressed through the queue's own state, no new lock primitive: the "lock" is "the prior task row is not terminal yet." A `skipped_runs` counter column makes the skip visible to the operator rather than silent.

This is distinct from the leaderless enqueue guard (which stops two beat *instances* double-enqueuing one fire) and from the lease below (which is about beat-process ownership). Three different concerns; this one is "do not let a slow job pile up on itself."

#### 3. Missed-run policy (catch-up vs skip)

Make today's implicit coalesce-to-one an explicit per-schedule choice:

| New column | Type | Meaning |
|---|---|---|
| `missed_policy` | `String` | `"skip"` (default; coalesce all missed fires to a single advance to `next_after(now)`, today's behavior) or `"catch_up"` (enqueue one task per missed interval) |
| `catch_up_max` | `Option<i32>` | ceiling on catch-up fires per tick so a schedule that missed a week does not enqueue thousands of tasks at once; default 100 |

For `"catch_up"`, when `now` is many intervals past `next_run`, beat walks `next_after` forward from `next_run`, enqueuing one task per boundary until it reaches `now` or hits `catch_up_max`, then sets `next_run` to the next future boundary. `"skip"` keeps the cheap single-advance. Cron and interval schedules both walk via `Schedule::next_after`, so no new schedule math is needed. Backfill catch-up defaults OFF because most periodic work (cleanups, digests) wants "run once now," not "run the five reports I missed"; the operator opts in per schedule where replay is the right semantics.

#### 4. Timezone-aware schedules and a calendar preview

Add a timezone to the schedule so "midnight daily" means midnight in the business's zone across DST, not midnight UTC:

| New column | Type | Meaning |
|---|---|---|
| `timezone` | `Option<String>` | an IANA zone name (`"America/New_York"`); NULL means UTC (today's behavior, fully backward compatible) |

`Schedule::next_after` grows a tz-aware variant: for a cron schedule with a timezone, evaluate the cron fields in that zone (via `chrono-tz`), then convert the resulting local instant back to UTC for storage in `next_run`. This reuses the DST-ambiguous-datetime rejection already built for gaps3 #42: a wall-clock time that does not exist (spring-forward gap) or is ambiguous (fall-back overlap) is resolved by the same documented rule rather than silently picking a branch. `Every(Duration)` is timezone-independent (a fixed duration is a fixed duration) and ignores the column.

The admin schedule detail gains a **calendar preview**: the next N (default 10) fire instants computed by walking `next_after`, rendered in both the schedule's timezone and UTC. This is the "what does this cron actually do" answer an operator otherwise gets only by waiting or by pasting the expression into a third-party cron site. It is pure computation over the stored `Schedule` + `timezone`, no new persistence.

#### 5. Beat ownership visibility (the honest version of "leader election")

The correctness model stays leaderless: the per-row conditional UPDATE remains the always-on guarantee, and nothing below is required for correctness. What operators actually asked for is **observability** ("which process is beating, and is it alive?") and, optionally, **single-active beat** to cut the wasted contention of many instances all scanning every tick. Both come from one small lease model:

`BeatLease` (table `beat_lease`), a singleton-row lease the beat loop renews:

| Column | Type | Meaning |
|---|---|---|
| `id` | PK | always the same well-known row (a fixed key, one lease) |
| `holder` | `String` | an instance identity (hostname + pid + a random nonce) |
| `acquired_at` | `DateTime<Utc>` | when the current holder took the lease |
| `heartbeat_at` | `DateTime<Utc>` | last renewal; a holder renews every tick |
| `expires_at` | `DateTime<Utc>` | `heartbeat_at + lease_ttl`; a lease past this is stealable |

`run_beat` gains an optional mode, `BeatOptions::single_active` (default off, preserving today's leaderless multi-beat behavior). When on, each tick a beat instance tries to acquire-or-renew the lease with an optimistic conditional UPDATE (`... WHERE expires_at < now OR holder = self`) - the same optimistic-claim technique `claim_one` and `fire_due_periodic` already use, so no new locking primitive and no dependency on Postgres advisory locks (it works identically on SQLite). Only the lease holder runs `fire_due_periodic`; non-holders idle and stand by to take over within `lease_ttl` if the holder dies. Because the per-row guard is still there, even a brief two-holders-at-once window during a handover cannot double-fire.

Visibility rides on the lease regardless of mode:

- `umbral tasks-beat status` (a new subcommand) prints the current `holder`, `heartbeat_at`, `expires_at`, and whether the lease is live or expired.
- The admin schedules view gains a header panel showing the same, so "is beat even running, and where?" is answerable from the dashboard, which today it is not.

In leaderless mode the lease is written as a pure heartbeat (every instance stamps its own `holder`/`heartbeat_at` on a per-instance lease row rather than contending for the singleton), so the panel can still list every live beat instance. The difference between the two modes is honestly labeled in the UI ("single-active: one holder runs beat" vs "leaderless: all instances beat, row guard dedupes"), so nobody mistakes leaderless for broken.

### Why this shape

- **Additive and backward compatible.** Every new column is nullable or defaulted to today's behavior (`overlap = "allow"`, `missed_policy = "skip"`, `timezone = NULL`, `single_active = false`). An existing deployment migrates cleanly and behaves identically until an operator opts into a control.
- **No new primitives.** Locks, leases, and catch-up all reuse the optimistic-conditional-UPDATE technique already proven in `claim_one` and `fire_due_periodic`, and the queue's own task-state as the overlap signal. Nothing here needs Postgres-only advisory locks, so it works on SQLite in tests exactly as in production.
- **Honest about leaderless.** Rather than bolt on a leader election the design does not need for correctness, it surfaces ownership as observability and offers single-active as an efficiency opt-in, with the per-row guard as the load-bearing guarantee in both modes.

### Out of scope for #56

- Per-schedule task priority (an `enqueue_periodic` priority), already deferred to features #82.
- Sub-second schedule resolution (beat's tick is the resolution floor).
- A visual cron builder UI; the calendar preview answers "what will this do" without one.

---

## Part 2 (#57): an optional distributed signal bus

### What exists today (accurately)

`umbral-signals` is strictly in-process:

- **Typed model signals.** `on_model::<M>()` attaches `pre_save` / `post_save` / `pre_update` / `post_update` / `pre_delete` / `post_delete` handlers, keyed internally under `<event>:<table>` names. The ORM fires them; `post_save` / `post_delete` fire **after commit**.
- **Generic pub/sub.** `emit(name, payload)` / `subscribe(name, ...)` / `subscribe_async(name, ...)`, re-exported from the `umbral::signals` core registry. Handlers are awaited **in series** by the emitter (fire-and-collect); fire-and-forget is a `tokio::spawn` inside the handler.
- **No persistence, no cross-process, no replay.** The module doc says so plainly, and lists cross-process broadcast (Redis / NATS) as explicitly deferred. It also already gives the durable-work recipe: pair a signal handler with `umbral-tasks` (the handler enqueues a durable task).

### The design: a `SignalBus` transport behind the existing API

This mirrors `umbral-realtime`'s `Broker` seam almost exactly, because the shape is identical: local dispatch by default, an optional pub/sub transport that fans a message out to other instances, swappable behind an unchanged public API.

A `SignalBus` trait carried on the plugin:

```rust,ignore
#[async_trait::async_trait]
pub trait SignalBus: Send + Sync {
    /// Publish a signal to OTHER instances. The local registry has already
    /// dispatched to local subscribers by the time this is called.
    async fn publish(&self, msg: BusMessage);
}
```

- **`InProcessBus` (default).** A no-op `publish`. Local `emit` behaves exactly as today: dispatch to local subscribers in series, done. Zero new dependencies, zero behavior change; this is what every app gets unless it opts in.
- **`RedisBus` / `NatsBus` (feature-gated).** Behind a `redis` / `nats` cargo feature (matching realtime's `redis` feature). Each instance runs one background pump that PUBLISHes the local emissions it is asked to bridge and SUBSCRIBEs a shared channel (`umbral:signals:events`), re-dispatching received messages to its **local** registry. So `emit("cache_invalidate", {...})` on instance A reaches the `subscribe("cache_invalidate", ...)` handler on instance B. The pump uses the same bounded-queue-drops-newest backpressure realtime's `RedisBroker` uses (a dropped cross-process signal is a missed notification, never lost data - see the durability boundary below).

Wiring: `emit` gains an internal "after local dispatch, if a bus is installed and this signal is bridged, hand the message to the bus." The public `emit` / `subscribe` / `on_model` signatures do not change. An app opts in with `SignalsPlugin::redis(url)` (mirroring `RealtimePlugin::redis`).

#### Loopback: do not double-fire local handlers

`umbral-realtime` delivers only via its subscription (never a direct local dispatch), so the originating instance is served once. Signals cannot copy that, because local subscribers already ran synchronously inside `emit` before the bus ever saw the message. So the bus must **not** redeliver a message to the instance that originated it. Each `BusMessage` carries an `origin` instance id; on receipt the pump skips messages where `origin == self`. Local handlers fire once (directly, in `emit`); remote instances fire once (via their subscription); the originator never double-fires.

#### Selective bridging: opt-in per signal name

The ORM fires a signal on every row write. Bridging all of them across the wire would flood the bus with high-frequency `post_save:<table>` traffic that most apps do not need cross-process. So bridging is **opt-in by name**:

```rust,ignore
SignalsPlugin::default()
    .redis("redis://…")
    .distribute(&["cache_invalidate", "config_reloaded", "tenant_settings_changed"])
```

Only listed names (exact or a `prefix:*` glob) cross the wire; everything else stays in-process exactly as today. The default distribute set is empty: turning on the bus changes nothing until you name the signals worth broadcasting. This keeps cross-process traffic to the handful of genuinely cluster-wide events (cache invalidation, config/feature-flag reloads, live coordination) it is meant for.

### The durability boundary: bus is ephemeral, outbox is durable

This is the load-bearing distinction and it must be documented at the top of the signals docs, because "distributed signals" invites the assumption of durability that this bus deliberately does not provide.

- **The signal bus is best-effort, at-most-once, ephemeral, unordered.** Exactly like realtime events. If an instance is down when a signal is published, it misses it. If the bus drops a message under backpressure, it is gone. There is no persistence and no replay. It is for *live coordination* (invalidate a cache, reload config, notify a live dashboard), where a missed message is self-correcting on the next event, not for *side effects that must happen*.
- **Durable, exactly-once-after-commit delivery is the outbox's job, not the bus's.** `docs/decisions/2026-08-08-cdc-outbox-and-read-replicas.md` designs `umbral-outbox`: an `outbox_event` row written in the same transaction as the business change, a relay on `umbral-tasks` that delivers to pluggable destinations with retries, backoff, dead-lettering, a per-attempt delivery log, and at-least-once with the event id as idempotency key. **If a cross-process signal must survive a crash, it is an outbox event, not a bus message.** The two compose cleanly: a durable destination (webhook, analytics, search reindex) rides the outbox; an ephemeral fan-out (cache bust) rides the bus.

Concretely, the docs will carry this decision table:

| You want | Use | Why |
|---|---|---|
| React to a change in the same process | `on_model` / `subscribe` (in-process) | zero infra, synchronous, after-commit |
| Tell every instance to invalidate a cache / reload config | signal bus (`RedisBus`) | ephemeral fan-out, a miss is self-correcting |
| A side effect that must happen even across a crash | `umbral-outbox` destination | durable, retried, at-least-once, logged |
| A durable job triggered by a change | signal handler enqueues an `umbral-tasks` job | the existing v1 recipe, still the answer |

### Make the local-only semantics explicit (the other half of #57)

Independent of the bus, the audit asks for the in-process semantics to be stated plainly. The signals docs (`documentation/docs/v0.0.1/plugins/signals.mdx`) get an explicit "Delivery semantics" section covering, for the default in-process registry: handlers run **in the emitting process only**; they run **in series, awaited by the emitter** (a slow handler slows the write path - spawn for fire-and-forget); `post_save` / `post_delete` fire **after commit** while `pre_*` fire before; **bulk writes fire bulk signals, not per-row** (already documented, reinforced here); and there is **no delivery guarantee, no retry, no replay** in-process either (a panicking handler is caught but its work is lost). This is true today and is worth stating so the bus section can then say "and here is what changes, and what does not, when you turn on cross-process."

### Why this shape

- **It reuses a proven seam.** The `Broker`/`InProcessBroker`/`RedisBroker` pattern in `umbral-realtime` already solved "swap a local dispatcher for a Redis pump behind an unchanged API, with bounded backpressure." Signals get the same trait shape, the same feature gate, the same drop-newest policy, so there is one mental model for both and one place to learn the failure modes.
- **Local stays the default and the API does not move.** `InProcessBus` is a no-op; existing apps and existing `emit`/`subscribe`/`on_model` code are untouched. Opting in is one builder call plus a `distribute` allowlist.
- **It refuses to reinvent durability.** The single most important design decision is what this bus is NOT: it is not a durable event log, because that already exists as the outbox. Keeping the bus honestly ephemeral avoids a half-durable middle thing that would compete with the outbox and mislead users into trusting it for side effects it cannot guarantee.

### Out of scope for #57

- Durable / replayable cross-process events: that is `umbral-outbox` (gaps5 #31), referenced above, not this bus.
- Typed event enums with compile-time emitter/subscriber agreement (still deferred, orthogonal to transport).
- Signal `disconnect` / per-call `disable` for testing (still deferred).
- Exactly-once or ordered cross-process delivery: the bus promises neither; reach for the outbox when you need them.

---

## Summary

- **#56** keeps beat's leaderless per-row-guard correctness and layers operator controls on top, all as additive `PeriodicTask` columns plus one optimistic-lease model: pause/resume with reason and a run-now action, a per-schedule overlap lock via the queue's own task state, an explicit skip-vs-catch-up missed-run policy with a bounded ceiling, IANA-timezone cron with a DST-safe `next_after` and an admin calendar preview, and a `BeatLease` that surfaces which node is beating (with an optional single-active mode) via `umbral tasks-beat status` and an admin panel.
- **#57** adds an optional `SignalBus` transport behind the unchanged `emit`/`subscribe`/`on_model` API, mirroring realtime's `Broker`/`RedisBroker` (feature-gated, bounded, origin-skip to avoid double-fire, opt-in per-signal bridging), documents the in-process semantics explicitly, and draws a hard line: the bus is ephemeral live-coordination, and anything that must survive a crash belongs to the durable `umbral-outbox` relay, not the bus.

# Task queue: pluggable broker, dead-letter queues and routing, a Horizon-like dashboard

Status: draft for ratification (proposes the answers to gaps5 #49, #50, #51; the final call is the maintainer's)
Date: 2026-08-08
Decision coverage: planning/gaps5.md #49 (tf #262), #50 (tf #263), #51 (tf #264). This closes the design question, not the broker/DLQ/dashboard implementation.

## Where the task queue actually is today (evidence)

`umbral-tasks` is a DB-backed queue built on the application's own SQLite/Postgres pool as the broker. The moving parts, by their real names in `plugins/umbral-tasks/src/lib.rs`:

- `TaskRow` is the queue: one row per enqueued job, columns `id, name, payload, status, attempts, max_attempts, scheduled_for, run_at, started_at, completed_at, error, result, priority, created_at`. `status` is one of the string constants `STATUS_PENDING`, `STATUS_RUNNING`, `STATUS_SUCCEEDED`, `STATUS_FAILED`. The claim query is indexed with `#[umbral(indexes = [["status", "run_at"]])]`.
- `enqueue` / `enqueue_task<T: Task>` insert a `pending` row. `EnqueueOptions` carries `max_attempts, scheduled_for, eta, delay, timeout, priority` (`timeout` is accepted but not yet persisted per task).
- `claim_one` runs inside `umbral::transaction`, filters `status = pending AND scheduled_for <= now AND (run_at IS NULL OR run_at <= now)`, orders by `priority DESC, scheduled_for ASC, id ASC`, takes `for_update_skip_locked().limit(1)`, then does a conditional `UPDATE ... WHERE id = ? AND status = 'pending'` and counts affected rows as the race guard.
- `process_one(row, policy)` dispatches the handler (looked up by `name` in a process-wide `HANDLERS` registry), wraps it in `tokio::task::spawn` + `tokio::time::timeout`, and writes back the terminal state. On failure the decision is: `exhausted (attempts >= max_attempts) || non_retriable (HandlerNotFound)` marks `STATUS_FAILED`; otherwise it resets to `pending` and pushes `run_at` forward by the exponential backoff in `RetryPolicy::next_run_at`.
- `reclaim_orphaned_tasks_with(visibility_timeout, policy)` moves `running` rows whose `started_at` is older than the visibility timeout back to `pending` (or `failed` if exhausted). This is the at-least-once guarantee.
- `run_worker(WorkerOptions)` is the polling loop: reclaim, claim one, dispatch, write back, sleep. `run_beat(BeatOptions)` is the separate periodic scheduler over the `PeriodicTask` model; `fire_due_periodic` claims due schedules with an optimistic conditional `UPDATE ... WHERE next_run = <read value>` and enqueues the underlying task.
- `retry_task(id)` re-queues a `failed` row (resets to `pending`, `attempts = 0`, clears `error`/timestamps). `admin_model()` and `periodic_admin_model()` return read-only `umbral_admin::AdminModel`s with a "Retry selected" / "Enable / disable" bulk action.
- Handlers self-register at link time via the `#[umbral::task]` attribute + `inventory` (`TaskRegistration`, `register_discovered`), installed from `TasksPlugin::on_ready` and at the top of the `tasks-worker` / `tasks-beat` commands.

What is missing, and what each of the three items adds:

- There is exactly one broker (the DB) and it is hardcoded into every helper. Swapping in Redis or SQS means rewriting `enqueue` / `claim_one` / `process_one`. (#49)
- There is no `queue` column: every task lands in one undifferentiated stream. There is no dead-letter path (an exhausted or poison task just becomes `STATUS_FAILED` in place), no per-queue concurrency or rate limit, and no operator requeue distinct from `retry_task`. (#50)
- The admin surface is a read-only table plus a retry action. There are no worker heartbeats, no per-queue throughput/wait/depth metrics, and no alerts. (#51)

All three must hold the framework's rules: the ORM is the only row-level database interface (no `sqlx::query` in the plugin), dependencies point inward through the facade, and the DB queue stays the zero-config default so a fresh project gets background work for one `.plugin(TasksPlugin)` line.

## #49 (tf #262): a pluggable external broker

### The idea

Introduce a `Broker` trait that abstracts the three queue operations the worker actually performs against storage: enqueue, claim (lease a due row), and ack (write back the outcome), plus reclaim of expired leases. The DB queue becomes the default `DbBroker` implementation wrapping today's code unchanged. Redis and SQS ship as opt-in adapter crates implementing the same trait. Task semantics (at-least-once, `attempts`/`max_attempts`, exponential backoff, priority, visibility timeout) stay identical across backends because the semantic decisions live in the worker loop, not in the broker.

### The seam

The key design move: keep the retry/backoff decision in `process_one` (the worker), and let the broker only persist claim and ack. The broker never decides whether a failure retries; it is told the outcome and records it. This is what keeps "same task semantics" true regardless of backend.

```rust
/// The storage-and-transport seam under the worker. The DB queue is the
/// default impl; Redis/SQS are opt-in adapters. Object-safe so the
/// ambient broker is a `Box<dyn Broker>` set at App::build (mirroring the
/// ambient DbPool OnceLock).
#[async_trait]
pub trait Broker: Send + Sync {
    /// Insert a task and return its id. Backs `enqueue` / `enqueue_task`.
    async fn enqueue(&self, task: NewTask) -> Result<TaskId, TaskError>;

    /// Lease one due task from any of `queues`, honouring priority and
    /// `run_at`/eta. Returns the claimed task plus an opaque lease token.
    /// The DbBroker impl is today's `claim_one` (FOR UPDATE SKIP LOCKED +
    /// conditional UPDATE); SQS returns a receipt handle; Redis a stream id.
    async fn claim(&self, queues: &[String], opts: ClaimOptions)
        -> Result<Option<Claimed>, TaskError>;

    /// Record the terminal decision the worker computed. `Outcome` is
    /// Success{result} | Retry{run_at} | Fail{error} | Dead{error}. The
    /// broker persists it; it does not decide it.
    async fn ack(&self, lease: Lease, outcome: Outcome) -> Result<(), TaskError>;

    /// Return leases whose visibility timeout elapsed (crashed worker) to
    /// the ready set. DbBroker = `reclaim_orphaned_tasks_with`; SQS is a
    /// native no-op (visibility timeout re-delivers automatically).
    async fn reclaim(&self, visibility_timeout: Duration) -> Result<u64, TaskError>;

    /// Point-in-time counts per queue for the dashboard (#51) and metrics
    /// (#64): depth (ready), in-flight, oldest-ready age. DbBroker runs ORM
    /// aggregates; Redis LLEN/XLEN; SQS ApproximateNumberOfMessages.
    async fn stats(&self, queues: &[String]) -> Result<Vec<QueueStat>, TaskError>;
}
```

`run_worker` is rewritten to be broker-agnostic: `broker.reclaim(...)`, `broker.claim(queues, ...)`, dispatch the handler, compute the `RetryPolicy` decision exactly as `process_one` does today, then `broker.ack(lease, outcome)`. The result backend (`TaskRow::result`, `task_status`, `await_result`) reads through `Broker::status(id)` so a non-DB broker can back it with its own status store or a small DB-side status table.

### Wiring and dependency direction

- `Broker` and `DbBroker` live in `umbral-tasks`. The ambient broker is a `OnceLock<Box<dyn Broker>>` set at `App::build`, defaulting to `DbBroker` over the ambient `DbPool`. Selectable via `TasksPlugin::broker(...)`.
- `umbral-tasks-redis` and `umbral-tasks-sqs` are new opt-in crates that depend on the `umbral` facade + `umbral-tasks` and implement `Broker`. Structurally they are plugins-of-a-plugin: same inward dependency arrow, no change to `umbral-core`. This is the same inversion the whole framework runs on.

### What each adapter can and cannot honour (documented honestly)

The trait is uniform but the backends are not, so the design names the divergences rather than hiding them:

- Priority: DbBroker orders by the `priority` column; Redis needs one list/ZSET per priority band or a scored ZSET; SQS has no priority at all, so priority maps to separate SQS queues or is documented as unsupported on SQS.
- `eta`/`delay`: DbBroker uses `run_at`; Redis a ZSET scored by fire time; SQS `DelaySeconds` (capped at 15 minutes) with anything longer staying DB-scheduled.
- SKIP LOCKED concurrency: Postgres-native today; Redis uses consumer groups (XREADGROUP) or BRPOPLPUSH; SQS uses its native visibility timeout.

Redis first (streams + consumer groups), SQS second, per the gaps5 recommendation. The DB queue remains the default and the only one a fresh project needs.

## #50 (tf #263): dead-letter queues and queue routing

### queue column + routing

Add a `queue: Option<String>` column to `TaskRow` (additive nullable, same migration lesson as `run_at`/`priority`/`result`: an `ADD COLUMN queue TEXT` with no `NOT NULL DEFAULT` applies cleanly to a populated table; a `NULL` queue reads as `"default"`; `enqueue` always writes `Some`). `EnqueueOptions` grows a `queue: Option<String>` field (and `enqueue_task<T>` a `Task::QUEUE` associated const so routing is type-checked, matching the `Task::NAME` pattern).

`claim_one` gains a queue filter: a worker serves a configured set of queues (`WorkerOptions.queues`, default `["default"]`), and the claim predicate adds `queue IN (...)`. The claim's ordering (`priority DESC, scheduled_for ASC, id ASC`) is preserved within the served set. The composite index becomes `["queue", "status", "run_at"]` so the per-queue claim stays index-only.

Per-queue configuration is registered on the plugin:

```rust
TasksPlugin::default()
    .queue(QueueConfig::new("emails").concurrency(4).rate_limit(10, Duration::from_secs(1)))
    .queue(QueueConfig::new("exports").concurrency(1))
```

- concurrency: the worker runs a small bounded pool of claim/dispatch slots per served queue (today it processes strictly one at a time; the loop is extended to `concurrency` in-flight tasks per queue, each an independent claim + `process_one`). The cap is enforced worker-side by the slot count, not by a DB lock, so it composes with `for_update_skip_locked` giving each slot a distinct row.
- rate limit: a per-queue token bucket gates `claim` calls, so a queue drains at most N tasks per interval regardless of how many slots are free. This is the umbral-tasks-local control; distributed rate limiting across replicas is deferred to the shared-throttle work (gaps5 #67).

### Dead-letter queue + poison-message isolation

A task reaches the DLQ when it is out of runway rather than transiently failing:

1. It exhausts `max_attempts` (today: `STATUS_FAILED` in place).
2. It is non-retriable: `HandlerNotFound`, an un-deserializable payload, or a handler that panics/times out on every attempt (poison message).

The design adds a terminal `STATUS_DEAD` state and a `dead_lettered_at: Option<DateTime<Utc>>` column, and `process_one`'s failure arm routes the "exhausted or non-retriable" case to `Dead` instead of `Failed`. Keeping the dead row in `task_row` (rather than a separate table) means `admin_model()` already lists it, `list_filter(&["status"])` already filters it, and the full `payload`/`error`/`attempts` audit trail stays attached. The alternative, a dedicated `dead_letter_task` table with its own retention, is noted for high-volume deployments where DLQ rows should not bloat the hot claim table; the `STATUS_DEAD`-in-place form is the recommended default because it reuses the existing admin surface.

Poison-message isolation falls out of this: a message that fails every attempt (including after `reclaim`) lands in `STATUS_DEAD` and is no longer claimable, so it stops churning a worker slot. Making `HandlerNotFound` and deserialize errors go to the DLQ (operator-visible, requeuable) rather than a silent `STATUS_FAILED` is itself the fix for the class of poison that used to just accumulate as failures.

### Operator controls

- `requeue_dead(id)` (and a "Requeue from DLQ" bulk admin action on a DLQ-filtered `admin_model`): reset a `STATUS_DEAD` row to `pending` on its original `queue` with a fresh attempt budget, mirroring `retry_task`'s existing shape but gated on `status = 'dead'`.
- Purge and bulk-requeue-by-queue actions for draining a DLQ after a fix ships.

With #49 in place, queue names map to broker-native queues (one SQS queue or Redis stream per name); the DbBroker keeps them as one table filtered by the `queue` column, and DLQ maps to each backend's native dead-letter facility (SQS redrive policy, a Redis dead-letter stream) where available.

## #51 (tf #264): a Horizon-like task dashboard

### An AdminView over heartbeats + broker stats

The dashboard is an `umbral_admin::AdminView` (the existing custom-view surface: a page at `{admin_base}/tasks/` rendering `WidgetSection`s of the existing `WidgetKind`s - `Kpi`, `Card`, `Line`, `Bar`, `Donut`, `Radial`, `Heatmap`, `Progress`, `Table`, `Feed`). It is registered with `AdminPlugin::view(umbral_tasks::dashboard_view())`, and its permission is gated the same way every admin view already is (`AdminView::permission`).

Two data sources feed it, both read through the ORM / the `Broker::stats` seam so no raw SQL enters the plugin:

1. Worker heartbeats. A new `WorkerHeartbeat` model (`worker_id, hostname, pid, queues, started_at, last_beat, in_flight, processed, failed`) that `run_worker` upserts once per loop iteration (keyed on a stable per-process `worker_id`). The dashboard reads these rows and marks a worker "down" when `last_beat` is older than a staleness threshold (a small multiple of the poll interval). This is what turns "is my worker alive" from an SSH-and-ps question into a panel. Heartbeats stay DB-side regardless of the active broker, because they are a small control-plane table, not queue traffic.

2. Per-queue metrics. Point-in-time figures come from `Broker::stats` (queue depth = ready count, in-flight, oldest-ready age) plus ORM aggregates over `task_row` grouped by `queue`/`status`: throughput (rows that reached `succeeded`/`dead` with `completed_at` inside the window), wait time (`started_at - created_at` for recently-started rows), failure count (`dead` + retrying in the window). For the throughput and wait-time sparklines, a lightweight `TaskMetricSample` rollup row is written once a minute by the worker (or beat) so the charts have history without scanning the whole table on every dashboard load.

### Widget layout

- A KPI row (`WidgetKind::Kpi`): total throughput/min, median wait time, failed + dead count, workers up / configured.
- `Line` charts: throughput over time and wait time over time, from `TaskMetricSample`.
- A `Table` of per-queue health: queue name, depth, oldest-ready age, in-flight vs concurrency cap, drain rate, DLQ size.
- A `Radial` or `Progress` widget per busy queue: in-flight against its concurrency cap (queue saturation).
- A `Feed` of recent failures / newly dead-lettered tasks, linking into the existing `admin_model()` detail rows.

### Feeding #64 (metrics) and #70 (alerts)

The same counters the dashboard computes are the export surface for the Prometheus `/metrics` exporter (gaps5 #64): queue depth, oldest-ready age, throughput, wait-time histogram, in-flight, failure/dead counters, worker-up gauge, all labelled by queue. The dashboard's data functions and the metrics exporter read one shared aggregation layer so the numbers cannot diverge. Alerts (gaps5 #70) are threshold rules over those same series (queue depth above N, oldest-ready age above a duration, zero workers up for a queue with a non-empty backlog, DLQ growth rate), evaluated on a beat tick and emitted through the framework's alert seam. The dashboard is the human view of the metrics; #64 is the machine view; #70 is the automated view. One measurement layer, three consumers.

## What this commits us to

- The plugin contract stays the boundary: the broker adapters (Redis, SQS) are opt-in crates depending on the facade, not changes to `umbral-core`; the dashboard is an `AdminView`, not special-cased admin code. Every capability is still a plugin.
- The DB queue stays the zero-config default. A fresh project still gets background work, DLQ, and a dashboard for one `.plugin(TasksPlugin)` line, with no Redis/SQS in the dependency graph unless the app opts in.
- The ORM stays the only row-level database interface. `queue`, `dead_lettered_at`, `WorkerHeartbeat`, and `TaskMetricSample` are ORM models with autodetected additive migrations; the DbBroker is today's ORM code behind a trait; the dashboard reads via ORM aggregates and `Broker::stats`. No new `sqlx::query` enters the plugin.
- Task semantics are defined once, in the worker loop, and preserved across every broker. Where a backend cannot honour a semantic (SQS priority, SQS delay ceiling), it is documented as a named limitation, never silently diverged.

## What is explicitly out (for now)

Cross-replica distributed rate limiting (deferred to the shared-throttle work, gaps5 #67), Kafka/NATS/AMQP adapters beyond the Redis-then-SQS first cut, automatic queue balancing (Horizon's "balance" strategy: shifting worker capacity between queues by pressure), and the managed-cloud per-tenant queue quotas (Stage 3 in the product north star). The seams (the `Broker` trait, per-queue `QueueConfig`, the shared metrics layer) are designed so these are additive.

## Open decision for the maintainer

Three calls to ratify:

1. DLQ storage: `STATUS_DEAD` in-place in `task_row` (recommended, reuses the admin surface) versus a dedicated `dead_letter_task` table (better retention/volume isolation).
2. First external broker: Redis first then SQS (recommended, matches the gaps5 note), or SQS first if the target deployments are AWS-native.
3. Whether per-queue concurrency ships in the first cut (it changes `run_worker` from single-in-flight to a bounded per-queue pool) or lands as a follow-up after the `queue` column + DLQ + dashboard, which are independently shippable on the existing single-slot worker.

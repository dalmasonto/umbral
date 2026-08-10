# Metrics exporter, W3C traceparent propagation, and DB/task spans

Status: draft for ratification (proposes the design for gaps5 #64, #65, #66; the final call is the maintainer's)
Date: 2026-08-08
Decision coverage: planning/gaps5.md #64 (tf #277), #65 (tf #278), #66 (tf #279). This is a pre-implementation contract, not evidence that metrics/tracing are already shipped.

## Context: what observability already is

`umbral-logs` (see `plugins/umbral-logs/src/observability.rs`) is the framework's single observability entry point. `umbral_logs::observability::init(ObservabilityConfig::from_env())` installs the global `tracing` subscriber (human-readable or, under `UMBRAL_LOG_FORMAT=json`, JSON) and, when the `otel` cargo feature is on, adds a `tracing-opentelemetry` layer whose `SdkTracerProvider` ships spans over OTLP gRPC (tonic) to `OTEL_EXPORTER_OTLP_ENDPOINT`. The returned `ObservabilityGuard` flushes and shuts the exporter down on drop. `init` is set-once via a `OnceLock`; a malformed endpoint degrades to logs-only rather than failing the boot.

The pieces the three items below build on already exist and are load-bearing:

- **One HTTP request span.** `AppBuilder::build` mounts a `tower_http::trace::TraceLayer` outermost on the router (`crates/umbral-core/src/app.rs:1892`), opening an `http.request` span per request carrying `http.method`, `http.route`, and `http.status_code`. Under the `otel` feature that span is exported; without it, it is a cheap `tracing` span. This is the ONLY span the framework emits today.
- **A single ORM pool seam.** Every ORM terminal resolves the ambient pool through `crate::db::pool_dispatched()` / `resolve_pool()` and matches on the `DbPool::Sqlite | DbPool::Postgres` variant (`crates/umbral-core/src/db.rs`). For example `QuerySet::fetch` (`crates/umbral-core/src/orm/queryset/mod.rs:1729`) builds per-backend SQL and calls `sqlx::query_as_with(...).fetch_all(&pool)` inside that match. The write terminals (`create`, `bulk_create`, `update_values`, `update_expr`, `delete`) and the count/exists/first terminals follow the same shape, as do the transaction-bound `on_tx` variants in `queryset/tx.rs`.
- **A DB-backed task queue that only speaks ORM.** `umbral-tasks` (`plugins/umbral-tasks/src/lib.rs`) inserts via `enqueue` (`:601`, a `TaskRow::objects().create(...)`), claims via `claim_one` (`:1042`, a `SELECT ... FOR UPDATE SKIP LOCKED` + conditional `UPDATE` inside `umbral::transaction`), runs via `process_one` (`:1130`, a `tokio::task::spawn` under an optional `tokio::time::timeout`), and drives the loop from `run_worker` / `run_worker_once` and the periodic scheduler `run_beat`.
- **Process-wide outbound HTTP clients.** Several plugins already hold a shared `reqwest::Client`: `umbral-oauth` (`plugins/umbral-oauth/src/http.rs`, `http_client()`), `umbral-analytics` (`plugins/umbral-analytics/src/lib.rs:110`), and `umbral-email` under the `api` feature (`plugins/umbral-email/src/lib.rs:997`). These are the outbound edges a trace has to cross.
- **A proposed realtime metric surface.** `docs/decisions/2026-08-08-realtime-presence-quotas-metrics.md` #48 defines `RealtimeMetrics` (an `Arc` of atomics plus a small histogram) recording connections, delivered/dropped messages, buffer pressure, reconnects, broker queue depth, and channel fan-out from numbers the hot paths already compute, and explicitly defers the `/metrics` HTTP exporter to #64.

The gap the three items name is not primitives, it is reach: one request span, no metrics surface, no cross-service trace continuity, and no visibility inside the ORM, the queue, email, cache, or storage. All three extend the EXISTING `tracing` / OTLP setup rather than introducing a second telemetry stack. They ship independently; #66 (spans) is pure additive instrumentation on paths that already exist, #65 (propagation) needs #66's spans to have something to propagate, and #64 (metrics) is the one new subsystem.

---

## #64 (tf #277): A Prometheus `/metrics` exporter

### Problem

Observability today is traces and logs only. There is no counter or histogram surface and no `/metrics` endpoint, so an operator cannot see request rates, error rates, latency percentiles, DB pool saturation, queue depth, cache hit ratio, or realtime fan-out on a dashboard or alert on them. Traces answer "why was THIS request slow"; they do not answer "what is the p99 across all requests right now". That is the metrics question, and it is currently unanswerable.

### Design

**A. Emit through the `metrics` facade, export through a plugin.** Instrumentation code calls the `metrics` crate's macros (`metrics::counter!`, `metrics::histogram!`, `metrics::gauge!`), which record into whatever recorder is installed process-wide, or into a no-op recorder when none is. A new optional plugin, `umbral-metrics`, owns `metrics-exporter-prometheus`: on install it builds a `PrometheusBuilder`, installs its recorder as the global one, holds the `PrometheusHandle`, and mounts `GET /metrics` to render `handle.render()` in the Prometheus text exposition format.

This is the same dependency inversion the rest of the framework uses and the exact split #48 already committed to: emitters depend only on the lightweight `metrics` facade (no Prometheus types leak into `umbral-core` or into `umbral-realtime`), and only the app that installs `MetricsPlugin` pulls in the exporter crate. A metrics-free app compiles and runs with the macros compiled to no-ops. Realtime never depends on the exporter; it emits, the plugin consumes.

```rust
// user binary
App::builder()
    .plugin(MetricsPlugin::new())               // installs the recorder + GET /metrics
    .plugin(TasksPlugin::new())
    .build()?;
```

Why the `metrics` facade rather than the `prometheus` crate directly: `prometheus` requires every emission site to hold a handle to a registered collector, which forces threading a registry through `umbral-core` and every plugin (the same cascade `db.rs` rejected for `sqlx::AnyPool`). The `metrics` facade decouples emission from the recorder exactly like `tracing` decouples spans from the subscriber, so an ORM query can record a histogram with no argument plumbing, and `umbral-core` never names a metrics backend.

**B. The metric set.** Owned by the layer that already sees the event, named with the standard `_total` / `_seconds` / `_bytes` suffixes:

| Area | Metric | Type | Recorded at |
|---|---|---|---|
| HTTP | `umbral_http_requests_total{method,route,status}` | counter | the `TraceLayer` `on_response` hook (`app.rs:1892`) |
| HTTP | `umbral_http_request_duration_seconds{method,route}` | histogram | same hook, from the span's elapsed time |
| HTTP | `umbral_http_requests_in_flight` | gauge | `on_request` inc / `on_response` dec |
| DB | `umbral_db_queries_total{op,backend}` | counter | the ORM query funnel (see #66) |
| DB | `umbral_db_query_duration_seconds{op,backend}` | histogram | same funnel |
| DB | `umbral_db_pool_connections{alias,state}` | gauge | sampled from `sqlx::Pool::size()` / `num_idle()` |
| Cache | `umbral_cache_requests_total{backend,result}` (hit/miss) | counter | `umbral-cache` get/set |
| Tasks | `umbral_tasks_enqueued_total{task}` | counter | `enqueue` (`tasks:601`) |
| Tasks | `umbral_tasks_processed_total{task,outcome}` (succeeded/failed/retried) | counter | `process_one` terminal-state write (`tasks:1130`) |
| Tasks | `umbral_tasks_duration_seconds{task}` | histogram | `process_one`, around the handler `spawn` |
| Tasks | `umbral_tasks_queue_depth{status}` | gauge | sampled `TaskRow::objects().filter(status).count()` |
| Tasks | `umbral_tasks_queue_latency_seconds{task}` | histogram | `process_one`: `started_at - scheduled_for` |
| Storage | `umbral_storage_operations_total{op,backend}` + `_bytes` | counter | `umbral-storage` put/get/delete |
| Realtime | the #48 `RealtimeMetrics` surface, bridged here | mixed | `RealtimeMetrics::provider()` |
| Auth | `umbral_auth_logins_total{result}`, `umbral_auth_sessions_active` | counter/gauge | `umbral-auth` / `umbral-sessions` |

**C. Realtime and task feeds.** `RealtimeMetrics` (#48) stays a plain atomics holder in `umbral-realtime` with no Prometheus dependency; `MetricsPlugin` registers it as a provider and, on each `/metrics` scrape, reads its gauges/counters and renders them alongside the rest (`metrics_registry.register(RealtimeMetrics::provider())`, exactly as #48 sketched). The task queue-depth and queue-latency gauges (#51's stats) are sampled the same pull way, from `TaskRow` counts through the ORM, never raw SQL. This keeps the counter surface defined once and consumed by both the exporter (here) and the per-tenant meter (#47 / #84).

**D. Cardinality guard.** Per-route HTTP labels use the matched route template (`/users/{id}`), never the raw path, so an id in the URL does not mint a new series per request. Realtime defaults to transport plus tenant labels only, per the #48 guard; per-channel breakdown stays opt-in. No metric label ever carries a user identifier, an email, or a payload value; identity appears only as opaque PK strings inside live dashboards, never in a scraped series.

**E. Endpoint safety.** `/metrics` is mounted only when `MetricsPlugin` is installed. It is gated behind the plugin's `require_permission` option (default: bind to an internal listener or require a scrape token) so a public deployment does not expose its operational internals unauthenticated. The default posture matches the framework's secure-by-default stance: not mounted unless asked for, and authorized when it is.

### Safety defaults

Collection is cheap and always on once the plugin is installed (the atomics and histogram updates are lock-free); the endpoint is the only new attack surface and it is authorized by default. Without the plugin, every `metrics::` macro in the framework is a no-op, so a base build pays nothing and pulls in no exporter crate.

### Deferred

OTLP metrics export (the OpenTelemetry metrics pipeline, distinct from the Prometheus pull model) waits until there is demand for a push path; the `metrics` facade lets us add an OTLP recorder later without touching a single emission site. Exemplars (linking a histogram bucket to a trace id) wait on the propagation work in #65 stabilizing. StatsD / other exporters are a plugin swap, not a core change.

---

## #65 (tf #278): W3C `traceparent` propagation

### Problem

The `http.request` span is created locally (`app.rs:1892`): it ignores any inbound `traceparent` header and sets no outbound one. A request that arrives from an upstream service starts a brand-new trace instead of continuing the caller's, and a call this service makes to another (an OAuth token exchange, an email-provider POST, an analytics beacon) carries no context, so the downstream span is orphaned. Distributed traces break at every umbral boundary. The framework already has the OTel context machinery (`tracing-opentelemetry` bridges every `tracing` span to an OTel span); nothing wires the W3C `traceparent` / `tracestate` headers into and out of it.

### Design

**A. Inbound extraction on the HTTP edge.** Replace the bare `make_span_with` closure at `app.rs:1892` so that, before creating `http.request`, it reads the request headers and runs the OpenTelemetry `TraceContextPropagator` (W3C `traceparent` + `tracestate`) to build a parent `opentelemetry::Context`, then sets that as the new span's parent via `tracing_opentelemetry::OpenTelemetrySpanExt::set_parent`. When no `traceparent` is present, or the `otel` feature is off, behaviour is unchanged (a fresh local root span), so this is strictly additive. This lives behind the `otel` feature in `umbral-logs` and is exposed to `umbral-core`'s router builder through a small hook (a boxed `fn(&Request, &Span)`) so `umbral-core` does not gain an OpenTelemetry dependency: the propagator lives in `umbral-logs`, `umbral-core` only calls the hook if one is installed, matching the dependency-inversion rule that keeps otel out of core.

**B. Outbound injection on every client edge.** Each shared `reqwest::Client` gets its outbound requests stamped with the current span's context. The clean way, given the clients are already centralized, is a shared helper in `umbral-logs` (feature-gated) that injects the current `opentelemetry::Context` into a `reqwest::header::HeaderMap` via the same `TraceContextPropagator`. The three existing clients call it:

- `umbral-oauth` `http_client()` requests (token / userinfo exchanges),
- `umbral-analytics` `http_client()` beacons,
- `umbral-email` `api`-feature `reqwest` POST (`lib.rs:997`).

Injection reads the ambient context from the current span, so any code running inside an `http.request` span (or a task/email span from #66) propagates automatically. With the `otel` feature off the helper is a no-op that returns the headers unchanged.

**C. Into task payloads.** A queued task runs later, on a worker, outside the enqueuing request's span. To keep the trace continuous across the queue, `enqueue` captures the current `traceparent` (the serialized W3C string, not a live context) and stores it on the row, and `process_one` restores it as the parent of the task's execution span (#66). The row already persists a `payload` and metadata columns; the trace context rides as one additional nullable `traceparent` field on `TaskRow`, written through the ORM like every other column (never raw SQL). A row enqueued outside any span, or before this ships, has a NULL `traceparent` and simply starts a fresh trace, so the migration is a plain additive nullable column with no backfill.

**D. Into email and webhooks.** The email API path (C above) already injects on its outbound POST. For SMTP delivery there is no header channel to a third party, so the trace ends at the send span (#66); that is correct, the trace cannot follow a message into an SMTP relay. Outbound webhooks (any plugin POSTing to a subscriber URL) inject `traceparent` the same way as the HTTP clients, so a subscriber that also runs umbral (or any W3C-aware stack) continues the trace.

**E. Into realtime.** Per #48's deferral note, propagating context into realtime FRAMES (so a browser client's action continues server-side) is a larger design (the wire protocol would need a context field and the browser would need to originate one); this item wires the SERVER-side continuity that matters for operators: a `dispatch` triggered inside a request span, and the `RedisBroker` hand-off across instances, carry the context so a fan-out's downstream work stays in the originating trace. Client-originated frame context stays deferred to a realtime-protocol revision.

### Safety defaults

Propagation is entirely gated on the `otel` feature and on a `traceparent` actually being present; nothing changes for an app that does not export traces. Inbound `traceparent` is trusted only to the extent OpenTelemetry's propagator validates it (a malformed header is ignored, yielding a fresh root, never an error). `tracestate` is passed through but the framework writes no vendor entry of its own. No PII ever rides in trace headers; `traceparent` is opaque ids only.

### Deferred

Client-originated realtime frame context (needs a protocol field, per E), `baggage` propagation (out of scope until there is a use for cross-service key/values), and trust policy for inbound `traceparent` from untrusted callers (an operator behind a hostile edge may want to drop inbound context; a follow-up config knob, defaulting to "trust", since the common deployment is behind a trusted gateway).

---

## #66 (tf #279): DB and task spans

### Problem

Below the one `http.request` span there is no instrumentation: a slow request is a single opaque span with no child telling you it spent 400ms in one query, 200ms waiting on a task enqueue, and 150ms in an email send. The observability doc defers exactly this (per-DB-query and per-task spans). Without child spans, the OTLP traces the framework already exports are shallow, and the #64 DB/task histograms have no obvious place to be recorded. Spans and the metric funnel are the same instrumentation point, so they are designed together.

### Design

**A. The ORM query funnel.** Today each terminal dispatches on the `DbPool` variant and calls `sqlx` inline (e.g. `fetch` at `queryset/mod.rs:1729`), so there is no single choke point to instrument; there are roughly a dozen call sites across `queryset/mod.rs`, `queryset/tx.rs`, `write.rs`, `m2m.rs`, and `aggregate.rs`. Rather than sprinkle `info_span!` at every one, introduce two private helpers in `umbral-core` that wrap the actual `sqlx` execute/fetch call:

```rust
// crates/umbral-core/src/db.rs (sketch)
pub(crate) async fn traced_fetch<'q, O, A>(op: &'static str, table: &str, backend: &'static str, q: ...) -> Result<Vec<O>, sqlx::Error>;
pub(crate) async fn traced_exec (op: &'static str, table: &str, backend: &'static str, q: ...) -> Result<u64, sqlx::Error>;
```

Each opens a `tracing::info_span!("db.query", db.operation = op, db.table = table, db.system = backend)` around the sqlx call, records `db.rows_affected` / `db.rows_returned` on completion, and records the #64 `umbral_db_queries_total` counter and `umbral_db_query_duration_seconds` histogram from the same measured interval. Every terminal routes its `.fetch_all` / `.execute` through the matching helper. `op` is the terminal name (`select`, `insert`, `update`, `delete`, `count`), `table` is `T::TABLE`, `backend` is `pool.backend_name()`. This makes the funnel real without changing any terminal's public behaviour, and it is the single edit that gives BOTH the DB spans and the DB metrics. The SQL text is NOT put on the span by default (it can carry literal values and blow up cardinality); an opt-in `UMBRAL_TRACE_SQL` records the parameterized statement for debugging only.

**B. Migration spans.** The migration engine wraps each applied migration in a `migrate.step{migration = name, direction = up|down}` span and the whole run in `migrate.run`. These are low-frequency and high-value (a slow production migration is exactly what you want to see in a trace), and they reuse the same `traced_exec` helper for the DDL statements they run.

**C. Task spans.** `umbral-tasks` gets three spans on the existing paths:

- `enqueue` (`tasks:601`) opens `task.enqueue{task = name}`; this is where the #65 `traceparent` is captured onto the row and the #64 `umbral_tasks_enqueued_total` counter fires.
- `claim_one` (`tasks:1042`) runs inside a `task.claim` span so the `FOR UPDATE SKIP LOCKED` + conditional `UPDATE` contention is visible.
- `process_one` (`tasks:1130`) opens `task.run{task = name, attempt = n}` as the ROOT of the task's trace, parented to the row's stored `traceparent` (#65) so the run continues the trace that enqueued it. The handler `spawn` runs inside this span; the terminal-state write records `umbral_tasks_processed_total{outcome}` and the duration/queue-latency histograms (#64).

**D. Email, cache, storage spans.** Each plugin's outbound operation gets one span at its public boundary, reusing the same pattern:

- `umbral-email`: `email.send{transport, to_count}` around the SMTP / API-POST delivery (`lib.rs:997` for the API path), so a slow provider is visible and the #65 outbound injection has a span to read context from.
- `umbral-cache`: `cache.op{op, backend}` around get/set/delete, recording hit/miss for the #64 counter.
- `umbral-storage`: `storage.op{op, backend}` around put/get/delete, recording the bytes counter.

**E. Feature gating and cost.** The spans are plain `tracing` spans, so with no subscriber attached they are near-free and with the fmt subscriber they cost only when `RUST_LOG` enables their target. Only under the `otel` feature (and a reachable collector) do they become exported OTLP child spans. The metric recording in the same helpers is a no-op without `MetricsPlugin` (#64). So a base build with neither feature pays effectively nothing, matching the framework's "you pay for what you install" posture.

### Safety defaults

No span carries SQL literals, payload contents, PII, or secrets by default: DB spans carry operation + table + backend + row counts; task spans carry the task NAME and attempt, never the payload (payloads are plaintext and may hold sensitive data, per the `enqueue` docstring warning); email spans carry a recipient COUNT, not addresses. The `UMBRAL_TRACE_SQL` escape hatch for parameterized SQL is opt-in and documented as debug-only.

### Ties to other items

- **#64**: the `traced_fetch` / `traced_exec` funnel and the task/email/cache/storage span boundaries are the exact points that record the #64 DB/task/cache/storage metrics. Spans and metrics are one instrumentation pass, not two.
- **#65**: the task `traceparent` column (#65 C) is what parents the `task.run` span here to its enqueuing trace; the email/webhook spans here are where #65's outbound injection reads the current context.
- **#48**: broker-lag and fan-out metrics (#48) plus the per-`dispatch` span here give both a time-series and a trace view of a slow realtime fan-out.

### Deferred

Per-row-decode spans (too fine-grained, would dwarf the query span), connection-acquire spans (sqlx does not expose a hook cleanly; the pool gauges in #64 cover saturation instead), and template-render spans (a separate follow-up if template rendering becomes a latency source).

---

## Summary

All three extend the existing `tracing` / OTLP setup rather than adding a parallel telemetry stack. #64 adds the one genuinely new subsystem, a Prometheus `/metrics` exporter, built on the `metrics` facade so emitters never depend on the exporter (an optional `umbral-metrics` plugin owns the recorder and the authorized `/metrics` route), fed by the same HTTP / DB / cache / tasks / storage / realtime / auth counters, with `RealtimeMetrics` (#48) and task stats (#51) registered as pull providers and a cardinality guard defaulting to route-template and tenant labels. #65 wires W3C `traceparent` in and out at every umbral boundary, inbound on the HTTP edge (`app.rs:1892`), outbound through the three shared `reqwest` clients, across the task queue via a stored `traceparent` column, and into email-API / webhook POSTs, all gated on the `otel` feature and strictly additive. #66 replaces the shallow single-span trace with child spans on the paths that already exist, a two-helper ORM query funnel (`traced_fetch` / `traced_exec`) that is simultaneously the #64 DB-metric recording point, plus migration, task enqueue/claim/run, email, cache, and storage spans, none carrying SQL literals or PII by default. Each ships independently, each is off until its feature or plugin is installed, and each preserves the framework's secure-by-default, pay-for-what-you-install posture.

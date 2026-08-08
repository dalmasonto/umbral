# Distributed throttling, log drains/sinks, and pluggable analytics destinations

Status: draft for ratification (proposes the shape of gaps5 #67, #68, #69)
Date: 2026-08-08
Relates: planning/gaps5.md #67 (tf#280), #68 (tf#281), #69 (tf#282); #3 (tf#216, EnterprisePreset), #47 (tf#260, realtime quotas), and docs/decisions/2026-08-08-cdc-outbox-and-read-replicas.md (the transactional outbox this doc reuses for retry)

## Scope

Three observability/infra items, one doc, because they are the same move made three times: each takes an umbral subsystem that today lives entirely inside one process (an in-memory rate counter, a DB-local request-log insert, a single hard-coded PostHog send) and puts a **backend trait** behind it so the default keeps working unchanged while an operator can point the subsystem at real infrastructure (Redis, a log warehouse, a data warehouse) when they outgrow the single-process default.

None of the three needs new plumbing in `umbral-core`. Each rides a seam that already ships:

- **#67** rides `umbral::ratelimit::RateLimiter` (`crates/umbral-core/src/ratelimit.rs`) and the `Throttle` trait in `plugins/umbral-rest/src/throttle.rs`. The distributed limiter is a second `RateLimiter`-shaped backend, keyed off the same `umbral::settings::client_ip`-derived key the throttles already build.
- **#68** rides the fire-and-forget capture path in `plugins/umbral-logs/src/lib.rs` (the `capture_layer` -> `tokio::spawn` -> `RequestLog::objects().create` flow) and its existing `LogsConfig::should_capture` filter/sampler. A `LogSink` trait replaces the hard-wired ORM insert; the DB insert becomes the default sink.
- **#69** rides `plugins/umbral-analytics/src/lib.rs` (the `AnalyticsClient::capture_fire_and_forget` semaphore-bounded send). An `AnalyticsSink` trait replaces the hard-wired PostHog POST; PostHog becomes the default sink. Critical events route through the `umbral-outbox` `Destination` from the CDC doc instead of fire-and-forget.

The through-line: fire-and-forget is the right default for best-effort telemetry and the wrong default for anything an operator bills, audits, or must not lose. Each item below keeps the cheap best-effort path AND adds a durable path for the subset that needs it.

---

## Part 1 (#67): distributed rate limiting

### The real seam that exists today

Rate limiting is two shipped layers:

1. **The limiter primitive.** `crates/umbral-core/src/ratelimit.rs` defines `RateLimiter`: an in-memory sliding-window counter behind a `Mutex<Buckets>`, keyed by an arbitrary string, with `check(key) -> RateDecision`. `RateDecision` carries `{ allowed, retry_after, limit, remaining }`, which is exactly the material a `429` response and its `Retry-After` / `X-RateLimit-*` headers need. `Rate::parse("100/hour")` turns the rate string into `{ num, period }`. Memory is bounded by a periodic `sweep_map` every `SWEEP_EVERY` checks plus `clear(key)` ("success forgives", used by umbral-auth's login lockout). It is re-exported from the facade at `crates/umbral/src/lib.rs:654` as `umbral::ratelimit::{Rate, RateDecision, RateLimiter}`.

2. **The REST throttles.** `plugins/umbral-rest/src/throttle.rs` defines `trait Throttle { fn check(&self, ctx: &ThrottleContext) -> Result<(), ThrottleDenied>; }`, synchronous, and three built-ins that each own a `RateLimiter`: `AnonRateThrottle` (keyed by `client_ip`), `UserRateThrottle` (keyed by `user:{id}`), `ScopedRateThrottle` (keyed by `scope:who`). `ThrottleContext` carries `{ identity, client_ip, scope }`. The dispatch (`plugins/umbral-rest/src/lib.rs` around line 815) builds the context, resolves the IP via `throttle_client_ip(headers)` which calls `umbral::settings::client_ip(headers)` (trusted-proxy-hop aware), and the first throttle to `Err(ThrottleDenied { retry_after })` short-circuits to a 429.

The gap (tf#280): the counter lives in `Buckets` in one process. Behind N replicas, a `"100/hour"` limit is really `100*N/hour`, and a restart wipes the window. `documentation/docs/v0.0.1/rest/throttling.mdx:11-15` says this out loud.

### The design: a `RateLimiterBackend` trait with an in-memory default and a Redis adapter

The fix is to introduce the backend seam **below** the `Throttle` trait, not beside it. `Throttle` already computes the right key (`ip`, `user:{id}`, `scope:who`) and already maps a decision to `ThrottleDenied`. What varies between single-process and distributed is only *where the sliding-window count lives*. So we extract that:

```rust,ignore
// umbral-core, re-exported from umbral::ratelimit
pub trait RateLimiterBackend: Send + Sync {
    /// Check-and-record one hit for `key` against `rate`, returning the same
    /// verdict shape the in-memory limiter returns today.
    fn check(&self, key: &str, rate: Rate) -> RateDecision;
    /// Forget a key's window ("success forgives"), mirroring RateLimiter::clear.
    fn clear(&self, key: &str);
}
```

`RateLimiter` itself becomes the built-in `InMemoryBackend` (it already has this exact method set: `check`, `clear`, `sweep`). The `Rate` moves into the `check` argument so one backend instance can serve throttles of different rates, but the default keeps a per-`RateLimiter` rate for source compatibility. `RateDecision` is unchanged, so nothing downstream of a throttle changes.

The Redis adapter (`RedisBackend`, in a small `umbral-ratelimit-redis` plugin crate so the base build never pulls a Redis client) implements the same trait against a shared store:

- **Sliding-window log** or **token bucket**, both expressible as one atomic Redis operation. The sliding-window-log variant is the faithful port of the in-memory `VecDeque<Instant>`: a per-key sorted set of hit timestamps, and one Lua script does `ZREMRANGEBYSCORE` (prune older than `now - period`), `ZCARD` (count), conditional `ZADD` + `PEXPIRE`, and returns `(allowed, count)`. The token-bucket variant (fewer keys, coarser fairness) is offered as `RedisBackend::token_bucket()` for very high-cardinality key spaces where a sorted set per key is too much. The script computes `retry_after` from the oldest in-window entry exactly as `check_at` does today, so `RateDecision.retry_after` stays meaningful and the `Retry-After` header is still correct.
- **Keyed off the same string the throttle already builds.** No new key scheme: `AnonRateThrottle` still passes the `client_ip` (settings-resolved, trusted-proxy-hop aware), `UserRateThrottle` still passes `user:{id}`. The backend just counts them in Redis instead of a local map. A namespace prefix (`umbral:rl:`) keeps umbral's keys distinct in a shared Redis.
- **Fail-open vs fail-closed is a policy knob.** When Redis is unreachable, `RedisBackend::on_error(FailOpen | FailClosed)` decides whether the check returns `allowed: true` (availability over strictness, the sane default for a rate limit that must not take the site down when the limiter's own store blips) or `allowed: false`. This is logged at `warn` so an operator sees the limiter degraded rather than discovering it silently.

Wiring is a one-liner on the throttle constructors: today `AnonRateThrottle::new("100/hour")` builds an in-memory `RateLimiter`; we add `AnonRateThrottle::new("100/hour").backend(redis)` (and the same on the other two) that swaps the backend the throttle counts against. Everything else about the throttle, its keying, its no-op-pass rules, its `ThrottleDenied` mapping, is unchanged.

### Async note

`Throttle::check` is synchronous today because every built-in only touches an in-memory counter. A Redis round-trip is async. Rather than make the whole `Throttle` trait async (a wide ripple through the dispatch), the `RedisBackend` uses a **connection-pool with a blocking-safe check inside the request task**: the dispatch already runs on a tokio worker, and the backend call is a short single-RTT command. We expose it as `fn check(...)` that internally drives the redis command to completion on the current runtime (the same pattern `umbral-storage`'s S3 presign uses after gaps4 #59 moved it onto the reactor). If measurement shows the sync bridge is a bottleneck, an `async fn check_async` variant on the backend and an async `Throttle` path is the follow-up, but we do not pay that complexity up front.

### The production default (the EnterprisePreset tie-in)

tf#280 asks for "a production scaffold default", and gaps5 #3 (tf#216, `EnterprisePreset`) is where it belongs: when an app opts into the enterprise/production preset AND a Redis URL is configured (`UMBRAL_REDIS_URL`), the preset installs the `RedisBackend` as the default throttle backend so multi-replica limits are correct out of the box. Without a Redis URL the preset keeps the in-memory backend and emits a boot `warn` that per-replica limits multiply, so the operator makes an informed choice rather than silently shipping `100*N/hour`. Single-process dev and test keep the in-memory default with zero config, so nothing regresses for the common case.

The realtime-quota work (gaps5 #47, tf#260) shares this backend: per-tenant / per-channel connection and message budgets are the same "count events per key per window, across replicas" problem, so `umbral-realtime`'s quota enforcement counts against the same `RateLimiterBackend` (keyed `tenant:{id}` / `channel:{id}`) rather than inventing a second distributed counter.

### Why this shape

- **The distributed limiter is a backend, not a rewrite.** `Throttle` already isolates keying and decision-mapping; only the counting store varies, so that is the only thing the trait extracts. The three built-in throttles, the dispatch, and the 429 path are untouched.
- **`RateDecision` is the stable contract.** Because the Redis script returns the same `{ allowed, retry_after, remaining }`, `Retry-After` and `X-RateLimit-*` headers keep working with no per-backend special-casing.
- **`umbral-core` stays store-free.** The trait and the in-memory default live in core; the Redis client lives in a plugin crate, arrows pointing inward. A Redis-free app pulls no Redis dependency.

### Deferred / out of scope for #67

- Distributed-limiter fairness under clock skew across replicas (the sorted-set-by-timestamp approach tolerates modest skew; NTP is assumed, not enforced).
- A generic pluggable store beyond Redis (Memcached, a SQL-backed limiter). The trait admits them; only Redis ships.
- Cost-based / weighted limits (a request consuming N tokens). The `Rate` is per-hit today; weighted consumption is a later `check_n(key, n, rate)` addition.

---

## Part 2 (#68): log drains / sinks

### The real seam that exists today

`plugins/umbral-logs/src/lib.rs` captures one `RequestLog` row per request, the umbral way:

- `capture_layer` (an axum `from_fn`) stamps an `Instant`, runs the handler, then applies `LogsConfig::should_capture(path, status, seq)` (built-in + operator exclusion prefixes, a `min_status` floor, and a deterministic `sampled(seq, rate)` sampler driven by a `SAMPLE_COUNTER`).
- If captured, it `tokio::spawn`s a fire-and-forget task that does `RequestLog::objects().create(row)` (the ORM, per the plugins-use-the-ORM rule), logs a DB failure at `warn`, and never blocks the response. `track_handle` / `flush` make the async insert testable.
- The IP comes from `umbral::settings::client_ip` (trusted-proxy-hop aware), the user id from the trusted `LoggedUserId` request extension (never a client header). `admin_model()` gives a read-only admin view.
- Separately, `plugins/umbral-logs/src/observability.rs` (`init`, `ObservabilityConfig`, `ObservabilityGuard`) already exports **traces** to an OTLP collector under the `otel` feature (`OTEL_EXPORTER_OTLP_ENDPOINT`). So the plugin already knows about OTLP for spans; it does not yet ship request *logs* anywhere but the app DB.

The gap (tf#281): every captured request is a row in the app's own database. At volume that is write load on the primary, storage growth, and a retention problem, competing with the application's real tables. `plugins/umbral-logs/src/lib.rs:1-7` states the DB-insert design plainly.

### The design: a `LogSink` trait behind the existing async path

The capture decision (`should_capture`) and the capture layer stay exactly as they are; what changes is the single line that today is `RequestLog::objects().create(row)`. That becomes a dispatch to one or more configured sinks:

```rust,ignore
#[async_trait]
pub trait LogSink: Send + Sync {
    fn name(&self) -> &'static str;
    /// Emit one captured request. Called from the same spawned, fire-and-forget
    /// task the DB insert runs in today; an error is logged at warn and swallowed.
    async fn emit(&self, log: &RequestLog) -> Result<(), LogSinkError>;
    /// Optional batched variant; the default calls `emit` per row. Network sinks
    /// override it to ship a batch per flush.
    async fn emit_batch(&self, logs: &[RequestLog]) -> Result<(), LogSinkError> { /* default: loop emit */ }
}
```

- **`DbLogSink` is the default and is the current behaviour verbatim.** It calls `RequestLog::objects().create(row)`. An app that adds no sink keeps writing to `logs_requestlog`, so nothing regresses and the admin view keeps working.
- **Network sinks batch.** OTLP-logs, Kafka, S3, ClickHouse, and Datadog all prefer batches over one-row-per-request. So `LogsPlugin` grows a bounded, time-and-size-flushed buffer (a small channel + a flusher task, sized like the analytics semaphore so a burst cannot fan out unbounded): the capture layer pushes the `RequestLog` into the buffer instead of spawning a per-row insert, and the flusher calls `sink.emit_batch(&drained)` on the interval or when the buffer fills. The `DbLogSink` ignores batching (loops `create`); the network sinks use it. The buffer is bounded and drops-with-warn at capacity, matching the analytics plugin's best-effort posture (a request log is telemetry, not billing).

Built-in sinks (each feature-gated so the base build stays lean, mirroring how `otel` is already gated in `observability.rs`):

- **`OtlpLogSink`** - emits the request as an OTLP log record, reusing the OTLP endpoint config already in `ObservabilityConfig`. This is the natural pairing with the trace export the plugin already does: one collector, spans and request logs together, `trace_id` correlation for free.
- **`KafkaLogSink`** - produces to a topic, key = tenant or path, for a streaming pipeline.
- **`S3LogSink`** - batches to newline-delimited JSON (or Parquet) objects in a bucket, the cheap archival tier; reuses `umbral-storage`'s S3 client rather than a second AWS dependency.
- **`ClickHouseLogSink`** - batch-inserts to a ClickHouse table, the query-it-later analytics tier.
- **`DatadogLogSink`** - POSTs batches to the Datadog logs intake, the hosted-SaaS tier.

Multiple sinks **stack**: `LogsPlugin::default().sink(OtlpLogSink::from_env()).sink(S3LogSink::new(bucket))` fans each captured request to both, exactly as multiple throttles stack. A sink failure is isolated (logged at `warn`, other sinks still run), so one flaky drain never drops logs for the others and never touches the request path.

### Retention jobs

Retention is not the capture layer's job; it is a scheduled sweep, and umbral already has the scheduler. `LogsPlugin` registers a periodic task via `TasksPlugin::periodic` (the `periodic(name, schedule, task, payload)` seam in `plugins/umbral-tasks/src/lib.rs:341`) that prunes `RequestLog::objects().filter(created_at.lt(now - retention)).delete()` (through the ORM, batched to avoid a single giant delete) on the operator's schedule (default 30 days). This only applies to the `DbLogSink`; network sinks own their own retention (S3 lifecycle rules, ClickHouse TTL, Datadog plan). Naming the periodic task the retention seam reuses the beat rather than inventing a second scheduler, the same reasoning the outbox doc uses for its own pruner.

### Per-tenant sampling

`LogsConfig` today has a single global `sample_rate` fed to the deterministic `sampled(seq, rate)`. Per-tenant sampling generalizes it to a resolver: `LogsPlugin::sample_by(|log| -> f64)` returns the rate for a given request (read the tenant from the trusted request context / `LoggedUserId`, or a tenant extension), so a noisy free-tier tenant can be sampled at 1% while a paying tenant is logged in full. The deterministic-cadence sampler is unchanged; only the rate it is handed becomes per-request instead of process-global. The default resolver returns the existing global `sample_rate`, so nothing changes for apps that do not opt in.

### Why this shape

- **Capture and routing are separated.** `should_capture` (what to log) is untouched; only *where the captured row goes* becomes pluggable. The security-sensitive parts (trusted-proxy IP, unforgeable user id from the extension, exclusion of static/health traffic) all live in capture and are unaffected.
- **The default is the current behaviour, byte for byte.** `DbLogSink` is `RequestLog::objects().create`, so the admin view, the model, and the migration are all as they are. A drain is purely additive.
- **Batching is where the network sinks need it and nowhere else.** The DB sink keeps its simple per-row insert; only network sinks pay the buffer/flush machinery, and that machinery is bounded so a burst degrades to drop-with-warn rather than unbounded memory, matching the plugin's existing fire-and-forget contract.

### Deferred / out of scope for #68

- A generic structured-application-log drain (beyond the per-request `RequestLog`). This item is request logs; shipping arbitrary `tracing` events to the same sinks is a natural follow-up but is not designed here (the OTLP trace export in `observability.rs` already covers spans).
- Exactly-once delivery to a sink. Request logs are best-effort telemetry; a crash mid-flush may drop a batch, matching today's fire-and-forget guarantee. Anything needing not-lost delivery uses the outbox (Part 3's pattern), not a log sink.
- A hosted log-search UI beyond the read-only admin view.

---

## Part 3 (#69): pluggable analytics destinations

### The real seam that exists today

`plugins/umbral-analytics/src/lib.rs` is a single-destination, fire-and-forget PostHog client:

- `AnalyticsClient` owns `{ api_key, host, exclude_prefixes }`. `capture_fire_and_forget(distinct_id, event, properties)` acquires a permit from a `Semaphore` (`MAX_CONCURRENT_ANALYTICS_SENDS = 64`, drop-with-`debug` when exhausted so a burst cannot fan out unbounded outbound tasks, audit_2 obs #5), builds the PostHog `/capture/` payload via `build_payload`, and `tokio::spawn`s the HTTPS POST; errors are logged at `warn`/`debug` and never propagated.
- The free functions `capture` / `identify` dispatch to an ambient `AnalyticsClient` installed once in `on_ready` (an `AMBIENT_CLIENT: OnceLock`); with no API key they are clean no-ops. `pageview_middleware` fires a `$pageview` per request, gated by `should_capture_path` (exclusion prefixes) and `scrub_path` / `scrub_segment` (gaps4 #22: `:id` / `:uuid` / `:email` / `:token` placeholders so secrets and PII never leave the trust boundary).

The gaps (tf#282): the destination is hard-coded to PostHog; there are no consent hooks (every `capture` ships regardless of a user's tracking preference); there is no path to a warehouse; there is no event schema; and every send is fire-and-forget, so a *critical* product event (a completed purchase a revenue report depends on) is dropped as silently as a pageview when the semaphore is full or the network blips.

### The design: an `AnalyticsSink` trait, consent hooks, a schema registry, and outbox-backed critical events

Four additions, each independent, each defaulting to today's behaviour.

#### 1. `AnalyticsSink` trait (PostHog is the default sink)

```rust,ignore
#[async_trait]
pub trait AnalyticsSink: Send + Sync {
    fn name(&self) -> &'static str;
    /// Best-effort send of one event. Called inside the same semaphore-bounded,
    /// spawned task PostHog uses today; errors are logged and swallowed.
    async fn send(&self, event: &AnalyticsEvent) -> Result<(), AnalyticsSinkError>;
}
```

`AnalyticsEvent` is the already-assembled `{ distinct_id, event, properties, timestamp }` (what `build_payload` composes). `PostHogSink` wraps the current `AnalyticsClient` send verbatim and is the default, so an app that only sets `UMBRAL_POSTHOG_API_KEY` is unchanged. Additional built-in sinks: `SegmentSink`, `AmplitudeSink`, `MixpanelSink`, and a generic `WebhookSink` (POST the event JSON to an operator URL) for anything else. Sinks **stack** (`AnalyticsPlugin::new(key).sink(SegmentSink::from_env())` fans out to both); the existing semaphore bounds the *total* outbound concurrency across all sinks so the fan-out cannot amplify a burst.

#### 2. Consent hooks

Fire-and-forget telemetry that ignores a user's tracking preference is a compliance bug. `AnalyticsPlugin::consent(|ctx| -> bool)` installs a predicate consulted before **every** `capture` / `identify` / auto-`$pageview`: it reads the consent signal (a cookie, a session flag, a per-user column, whatever the app models) and, when it returns `false`, the event is dropped at the source before any sink sees it. The default predicate returns `true` (today's behaviour, opt-out semantics for apps that do not wire consent), and the hook is the one place to make it opt-in. This composes with the existing `should_capture_path` / `scrub_path` privacy layers rather than replacing them: consent decides *whether* an event is sent, scrubbing decides *what* is in it.

#### 3. Event schema registry

Free-form `properties: Value` means a typo (`amount_cent` vs `amount_cents`) silently produces a broken warehouse column no one notices until a report is wrong. An optional registry lets an app declare its events:

```rust,ignore
AnalyticsPlugin::new(key)
    .register_event(EventSchema::new("purchase")
        .required("amount_cents", FieldType::Int)
        .required("currency", FieldType::String)
        .optional("coupon", FieldType::String))
```

When a schema is registered for an event name, `capture` validates the `properties` against it before dispatch: a missing required field or a type mismatch is rejected (logged at `warn`, and in a `strict()` mode returns an error to the caller) so a malformed event is caught at the call site, not in the warehouse. Events with no registered schema pass through unchanged, so the registry is purely additive and an app adopts it event by event. The registry is also the source for a generated events catalog (a `umbral analytics schema` command, later) so the analytics contract is documented from the same declarations.

#### 4. Outbox-backed retry for critical events (the CDC-doc tie-in)

Fire-and-forget is correct for a pageview and wrong for a purchase. Rather than build a second durable-retry mechanism, critical events route through the **transactional outbox** already designed in `docs/decisions/2026-08-08-cdc-outbox-and-read-replicas.md`. That doc's Part 1 ships an `analytics` `Destination` explicitly ("Analytics is the canonical CDC consumer"). So:

- `capture` stays fire-and-forget (best-effort, the right default for high-volume behavioural events).
- A new `capture_critical` writes the event to the `outbox_event` table **inside the caller's transaction** via `outbox::publish_on(tx, event)`, so the analytics event commits atomically with the business row that caused it (the purchase row and its `purchase` event either both commit or neither do, closing the dual-write window). The outbox relay then delivers it to the analytics `Destination` with the outbox's existing exponential backoff, dead-letter ceiling, and per-attempt delivery log, at-least-once with the event `id` as the idempotency/dedupe key.
- The analytics `Destination` in the outbox delivers to the **same `AnalyticsSink` set** configured here, so critical and best-effort events land in the same PostHog/Segment/warehouse, differing only in the durability of the path they took to get there.

This means umbral has one durable-retry mechanism (the outbox), reused by webhooks, realtime, email, and now critical analytics, not a bespoke retry queue inside the analytics plugin.

#### Warehouse export

Warehouse export is not a fifth mechanism; it is a sink plus a drain. Two supported shapes:

- **Streaming:** a `WarehouseSink` (BigQuery / Snowflake / Redshift streaming insert, or via the `KafkaLogSink`-style producer) as another `AnalyticsSink`, for near-real-time rows.
- **Batch (the canonical CDC feed):** the outbox's ordered, replayable `outbox_event` stream IS a warehouse-ingestion feed (as the CDC doc notes). An operator points their existing ELT (Fivetran/Airbyte/dbt) at `outbox_event`, or umbral ships an `S3` batch exporter analogous to `S3LogSink`. Either way the durable change stream the outbox already persists is the export source, so no new export subsystem is invented.

### Why this shape

- **Every addition defaults to today's behaviour.** No sink configured -> PostHog only. No consent hook -> always-send. No schema -> free-form properties. No `capture_critical` -> fire-and-forget. An existing analytics app is unchanged; each capability is opt-in.
- **It reuses the outbox instead of building a retry queue.** "Critical event must not be lost" is the exact problem the transactional outbox solves, and the CDC doc already ships an `analytics` destination. Adding a second durable mechanism inside the analytics plugin would duplicate backoff/dead-letter/delivery-log logic that already exists.
- **The privacy layers compose.** Consent (whether), `should_capture_path` (which routes), and `scrub_path` (what PII) stack; none replaces another, and the semaphore still bounds total outbound concurrency across every sink.

### Deferred / out of scope for #69

- Server-side identity resolution for `$pageview` (`distinct_id` is still `"anonymous"` for auto-pageviews; wiring the session identity is a separate item noted in the code).
- A full consent-management platform (banner UI, preference center). The hook consumes a consent signal; producing/storing it is the app's or a dedicated plugin's job.
- Exactly-once analytics delivery (the outbox is at-least-once + idempotency key, same honest guarantee as every other outbox consumer).
- Client-side (browser SDK) analytics; this is the server-side capture path only.

---

## Summary of the contract

- **#67:** extract a `RateLimiterBackend` trait under the existing `Throttle` seam; `RateLimiter` (`crates/umbral-core/src/ratelimit.rs`) is the in-memory default, a `RedisBackend` (sliding-window log or token bucket, fail-open/closed policy) is the distributed adapter in a plugin crate. Throttles keep their keying (`client_ip` via `umbral::settings::client_ip`, `user:{id}`, `scope`) and their `RateDecision` -> `ThrottleDenied` mapping unchanged. The `EnterprisePreset` (#3) installs the Redis backend as the production default when a Redis URL is set; realtime quotas (#47) share the same backend.
- **#68:** put a `LogSink` trait behind the `capture_layer` fire-and-forget path in `plugins/umbral-logs/src/lib.rs`; `DbLogSink` (`RequestLog::objects().create`) is the unchanged default, with batched `OtlpLogSink` / `KafkaLogSink` / `S3LogSink` / `ClickHouseLogSink` / `DatadogLogSink` adapters that stack. Retention is a `TasksPlugin::periodic` prune of the DB sink; per-tenant sampling generalizes the existing deterministic `sampled(seq, rate)` to a per-request rate resolver. `should_capture` and the security-sensitive capture layer are untouched.
- **#69:** put an `AnalyticsSink` trait behind `AnalyticsClient::capture_fire_and_forget` in `plugins/umbral-analytics/src/lib.rs`; `PostHogSink` is the unchanged default, with `Segment` / `Amplitude` / `Mixpanel` / warehouse / webhook sinks that stack under the existing send semaphore. A consent predicate gates every event, an optional event schema registry validates `properties`, and critical events route through the `umbral-outbox` transactional outbox (`outbox::publish_on(tx, event)` -> the CDC doc's `analytics` `Destination`) for durable at-least-once delivery, so there is one retry mechanism, not a bespoke analytics queue. Warehouse export is a `WarehouseSink` or an ELT feed off the durable `outbox_event` stream.

All three follow one pattern: a backend/sink trait, the current single-process behaviour as the zero-config default, and durable infrastructure reachable through the facade with arrows pointing inward, so `umbral-core` never learns that Redis, Kafka, or a warehouse exist.

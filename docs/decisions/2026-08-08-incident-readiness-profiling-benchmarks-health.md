# Incident readiness, runtime profiling, public benchmarks, and dependency health

Date: 2026-08-08
Status: draft (design only, nothing shipped)
Scope: gaps5 #70 (tf#283), #71 (tf#284), #72 (tf#285), #73 (tf#286)
Related: gaps5 #64 (tf#277) metrics, gaps5 #65/#66 (tf#278/#279) trace + span instrumentation

## Why these four together

They are the operational maturity cluster of the gaps5 sweep. An organization evaluating umbral for production asks four questions in sequence: can I see it (metrics, #64), can I run an incident against it (#70), can I find the slow path before it pages me (#71), how fast is it against the alternatives (#72), and will readiness tell the truth about every dependency it leans on (#73). #64 (a Prometheus `/metrics` exporter with HTTP/DB/cache/task/queue counters and histograms) is the substrate the other three build on; this doc assumes it lands first and designs #70, #71, #73 to consume it and #72 to measure it. Where #64 is not yet shipped, each section below names exactly which metric it needs so the dependency is explicit rather than hand-waved.

Ground truth this design is written against (real code, read on 2026-08-08):

- `plugins/umbral-health/src/lib.rs` is the entire health plugin today: a `HealthCheck` trait (`name()` + `async check() -> Result<(), HealthError>`), `HealthPlugin::default().check(...)` registration, `/healthz` liveness (unconditional 200), `/ready` + `/readyz` readiness (DB `SELECT 1` via `umbral::db::ping()`, each registered check, plus an opt-in `require_migrations()` gate over `umbral::migrate::drift_report()`), a per-check `check_timeout` (default 5s), and a shutdown-drain short-circuit via `umbral::shutdown::is_draining()`. Checks run sequentially, each bounded by the timeout, and the unauthenticated body carries only generic reasons (never the raw DB error / DSN).
- `crates/umbral-core/tests/query_counts.rs` is the query-count proof harness. It installs a `tracing_subscriber` `Layer` (`CountLayer`) that counts every event at target `sqlx::query`, reading the `summary` / `db.statement` fields and skipping connection-setup `PRAGMA`s. This is the exact runtime seam #71 reuses.
- `crates/umbral-core/src/db.rs:717-727` (`sqlite_options`) sets `.log_statements(Off)` for runtime performance but deliberately leaves sqlx's built-in `slow statement` WARN (default 1s threshold) on, "since it goes via a separate log target." So a primitive slow-query WARN already exists; #71 makes it first-class, configurable, structured, and backend-uniform rather than inventing it.
- `crates/umbral-core/src/db.rs`: `pool_dispatched()`, `pool_for(alias)`, `ping()`, `DbPool::{Sqlite,Postgres}`, `RouteOp::{Read,Write}` are the dispatch points every ORM terminal already routes through.
- No `benches/` directory and no `criterion` dev-dependency exists anywhere in the workspace today. #72 is greenfield.

Non-negotiable house rules honored below: plugins go through the ORM, never raw `sqlx::query` (CLAUDE.md); the health endpoints stay unauthenticated and leak no DSN; nothing here wipes a DB or a migration file.

---

## #70 (tf#283): Incident readiness packaged as artifacts

### Problem

Health endpoints exist, but there is no packaged answer to "it is 3am and the service is degraded." An org needs SLO definitions, alert rules that fire before users notice, dashboards that show saturation, runbooks that say what to do, and a documented model of how the app behaves when a dependency degrades rather than fully fails. None of that ships today.

### Design

Incident readiness is not code, it is a set of versioned, copy-pasteable artifacts built on the #64 metric names and the #73 health registry. Ship them under a new tree so they version with the framework:

```
ops/
  slo/
    slo-templates.yaml          # SLI/SLO definitions keyed to #64 metric names
  prometheus/
    umbral-recording-rules.yaml # burn-rate + saturation recording rules
    umbral-alert-rules.yaml     # alerts (page vs ticket severity)
  grafana/
    umbral-overview.json        # RED (rate/errors/duration) + saturation
    umbral-database.json        # pool saturation, slow-query rate (feeds on #71)
    umbral-tasks.json           # queue depth, wait time, worker heartbeat (#64 task metrics)
    umbral-realtime.json        # connections, dropped messages, buffer pressure
  runbooks/
    database-down.md
    migrations-pending.md
    queue-backlog.md
    dependency-degraded.md
    high-error-rate.md
  degradation-modes.md          # per-dependency: fail-closed vs fail-open behavior
```

SLO templates (the four golden signals mapped to real umbral surfaces):

- Availability SLO: `1 - (rate(http_requests_total{status=~"5.."}[5m]) / rate(http_requests_total[5m]))`, target 99.9%. Source metric: #64 `http_requests_total`.
- Latency SLO: p99 of `http_request_duration_seconds` under a per-route budget. Source: #64 histogram.
- Readiness SLO: fraction of `/readyz` probes returning 200, sourced from the #73 registry (below), so a degraded-but-serving dependency shows as a distinct band from hard-down.
- Freshness/queue SLO: `umbral_task_queue_wait_seconds` p95 under budget (depends on the #64 task-queue metrics, gaps5 #52/tf#265).

Alert rules use multi-window multi-burn-rate (fast-burn 1h/5m pages, slow-burn 6h/30m tickets) so a brief blip does not page but a sustained regression does. Saturation alerts wrap the pool: DB connection pool utilization from #64, tasks queue depth, realtime buffer pressure. Every alert annotation links to its runbook file by name.

Runbooks are markdown with a fixed shape: symptom, the exact `/readyz` JSON that confirms it, first diagnostic command (e.g. `umbral migrate --check` for `migrations-pending`), remediation, and rollback. They cite the health-registry check name (`database`, `migrations`, `redis`, ...) so the runbook and the probe body use one vocabulary.

`degradation-modes.md` documents, per dependency, whether umbral fails closed or open when it degrades: e.g. cache miss falls through to DB (fail-open), session store down means auth fails closed, realtime broker down drops best-effort messages (already documented behavior). This is the "dependency degradation modes" gaps5 explicitly asks for. It pairs with the #73 `critical` flag so a non-critical dependency degrading marks the pod degraded, not down.

### Honesty

These artifacts are reviewable and lintable (promtool for rules, jsonnet/dashboard schema for Grafana) but only meaningfully testable against a live Prometheus/Grafana + a running app emitting #64 metrics, which is not available in this environment. CI validates syntax (`promtool check rules`) and that every metric referenced in a rule exists in the #64 exporter's registry; end-to-end firing is a manual/staging step. We ship the artifacts and the linter, and say plainly that the metrics exporter (#64) is a hard prerequisite for any of them to light up.

---

## #71 (tf#284): Runtime slow-query / N+1 detection and a dev profiler

### Problem

Query-count discipline lives only in `tests/query_counts.rs`. At runtime an operator has sqlx's primitive 1s `slow statement` WARN and nothing else: no configurable threshold surfaced through umbral settings, no `EXPLAIN` helper, no per-request N+1 signal, no dev toolbar. A developer who writes an accidental N+1 finds out in production.

### Design

Four pieces, all hanging off the one seam that already works in the test harness: a `tracing` layer over the `sqlx::query` target. sqlx 0.8 emits one event per executed statement carrying `summary`, `db.statement`, `rows_affected`, `rows_returned`, and `elapsed`. The test harness already proves this is a reliable per-statement signal. We productionize it.

1. Slow-query threshold logging at the ORM dispatch point.

Add a `QueryProfileLayer` in `umbral-core` (installed by `App::build()` when logging is initialized, same set-once pattern as the OTLP subscriber). It subscribes to `sqlx::query` events and, when `elapsed` exceeds a configured threshold, emits a structured umbral WARN (`umbral::db::slow_query`) with the statement summary, elapsed, rows, and the active DB alias / `RouteOp`. Configuration lands in `Settings`:

```
[db.profiling]
slow_query_threshold_ms = 200   # 0 disables; default off in prod, 200 in dev scaffold
explain_slow_queries    = false # dev-only: attach EXPLAIN to each slow-query log
```

This is a first-class umbral surface, not sqlx's raw log: it is backend-uniform (routes through the `sqlx::query` target regardless of `DbPool` variant), honors the umbral settings schema, and carries umbral's alias/route context. It coexists with the existing `.log_statements(Off)` choice in `sqlx_options` (that only disables the pre-execution INFO log; the tracing event we count is separate and stays on). The #64 metrics side of this is a `umbral_db_query_duration_seconds` histogram fed from the same layer, so the slow-query dashboard panel in #70 has data.

Because the seam is a `tracing` event, this never adds a per-query allocation on the hot path when the threshold is 0 (the layer early-returns before formatting), matching the performance rationale that put `.log_statements(Off)` there in the first place.

2. `EXPLAIN` helper.

A QuerySet terminal that returns the plan instead of rows, backend-dispatched:

```rust
let plan = Comment::objects()
    .select_related("plugin__author")
    .explain()          // SQLite: EXPLAIN QUERY PLAN; Postgres: EXPLAIN (FORMAT JSON)
    .await?;
```

`explain()` builds the same SQL the terminal would run (reusing `build_query_for`) and prefixes it per backend via `pool_dispatched()`. This is the one place raw-ish SQL is legitimate because `EXPLAIN` is a backend feature the ORM cannot model at the row level (the gated exception #2 in CLAUDE.md), and it is gated on the backend match, never a silent SQLite fallback. It powers `explain_slow_queries` above and is directly callable in tests and the shell.

3. N+1 heuristic reusing the query-count infra.

Promote the `tests/query_counts.rs` counting layer into a reusable `umbral::profiling::QueryRecorder` (the test file becomes a thin consumer of it, so the harness and the runtime detector share one implementation and can never drift). At runtime, scope a recorder per HTTP request via a `tower` middleware (dev/profiling builds only). The heuristic: within one request, if the recorder sees N (default >= 10) statements whose normalized `summary` is identical except for bound parameters, that is a probable N+1; log a WARN naming the repeated statement, the count, and the request route. Normalization strips literals/placeholders so `SELECT ... WHERE id = ?` repeated 500 times collapses to one fingerprint with count 500. This is exactly the invariant the scale proofs assert (constant count vs row count); the runtime detector is the same measurement pointed at live traffic.

4. Dev toolbar / profiler.

A `DevToolbarPlugin` (dev-only, refuses to install when `settings.debug == false`, same posture as other dev-only surfaces) that mounts a `/__umbral/profiler` endpoint and injects a bottom-of-page toolbar into HTML responses. Per request it shows: total time, query count, the slowest queries with their `EXPLAIN` on click, any N+1 fingerprints flagged, template render time, and the resolved settings. Data comes from the per-request `QueryRecorder` plus the request span. This is the Django Debug Toolbar analogue; it is strictly a plugin (dogfooding the contract) and never ships enabled in production.

### Honesty

Pieces 1-3 are testable here (the harness already exercises the counting seam; the EXPLAIN helper is unit-testable against in-memory SQLite; the N+1 fingerprinting is pure logic over captured statements). Piece 4's HTML injection is testable for the endpoint and data, less so for the visual toolbar without a browser. Threshold logging's real value shows only under production load, which we cannot generate here.

---

## #72 (tf#285): Public performance benchmarks

### Problem

There are zero benchmarks. An org comparing umbral to Axum-bare, Loco, Django, or Laravel has no reproducible numbers. Absence of benchmarks reads as "either slow or untested."

### Design

Two layers, because micro and macro answer different questions, plus a CI job and a published results page. Honest framing up front: I cannot run any of these in this environment (no criterion in the tree, benchmarking needs dedicated isolated hardware and a live DB to mean anything). This section designs the harness and the honesty guardrails, not numbers.

1. Micro benchmarks (criterion).

Add `benches/` with `criterion` as a dev-dependency in `umbral-core`. Target the hot paths that are pure-CPU and DB-free so they are stable and reproducible: QuerySet -> SQL rendering (`build_query_for`), migration autodetection diff, settings/figment load, route matching, model hydration from a fixed row. These catch per-release regressions in the framework's own overhead independent of DB latency. They run in CI on every PR and gate on a regression threshold (criterion's `--baseline`).

2. App-realistic macro benchmarks (oha / k6).

A dedicated `benchmarks/` example app (a standalone Cargo project, like other examples, not a workspace member) exposing the TechEmpower-style canonical endpoints plus umbral-realistic ones:

- plaintext and JSON serialization (TechEmpower comparability),
- single-row fetch by id, multi-row fetch, `select_related` join (exercises the anti-N+1 path),
- a create with validation + signals,
- an authenticated request through sessions + permissions (the realistic middleware stack).

Load is driven by `oha` (simple, Rust, reproducible) with a `k6` script as the scripted-scenario alternative for ramp/soak profiles. Each scenario pins: umbral version, backend (Postgres primary, SQLite for the DB-free cases), pool size, hardware class, and concurrency. The app-realistic set is what distinguishes umbral from a bare-framework microbench: it measures the batteries (ORM, auth, sessions, validation) that are the actual product.

3. CI job.

A separate, non-blocking `bench` workflow (labeled, not on the critical PR path since numbers need isolated runners to be trustworthy) that: runs the criterion micros with a committed baseline and comments regressions; optionally, on a self-hosted/isolated runner with Postgres, runs the oha macro suite and uploads results as artifacts. The workflow is explicit that shared CI runners produce indicative, not authoritative, macro numbers.

4. Results page.

`documentation/docs/v0.0.1/performance/benchmarks.mdx` publishing methodology, the exact commands to reproduce, hardware disclosure, and results with a prominent honesty banner: version, date, hardware, and the caveat that macro numbers are environment-bound. Comparisons to Axum-bare (the floor: shows umbral's overhead over raw Axum), and to Django/Laravel/Loco framed as "same app, different framework" with each competitor's setup fully disclosed so the comparison is reproducible and not cherry-picked.

### Honesty

This is the item most easily faked and I am not going to. No numbers are produced by this design; it ships the harness, the scenarios, the CI wiring, and the reproducibility contract. Any published figure must come from a disclosed, isolated run, and the results page leads with that caveat. The criterion micros are the part that gives honest, reproducible, hardware-relative regression signal in ordinary CI; the macro suite needs infra we do not have here and the doc says so.

---

## #73 (tf#286): Dependency health registry with readiness/liveness profiles

### Problem

`HealthCheck` today is a bare `name()` + `check() -> Result<(), HealthError>`. Every dependency check is hand-written by the app author. There is no standard library of checks for the dependencies umbral's own plugins introduce (DB is built in; Redis, S3, email, OAuth providers, task workers, realtime broker, disk are not), and no notion of check profiles (liveness vs readiness) or of degraded-but-serving vs hard-down.

### Design

Evolve the health plugin into a dependency registry without breaking the existing `HealthCheck` trait (apps using `.check(MyCheck)` keep working). Add a richer `DependencyCheck` and a registry that classifies checks by profile and criticality.

```rust
pub enum Probe { Liveness, Readiness }   // which endpoint runs it
pub enum Criticality { Critical, Degraded } // fail -> 503 vs report degraded but 200

#[async_trait]
pub trait DependencyCheck: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn probe(&self) -> Probe { Probe::Readiness } // default: readiness only
    fn criticality(&self) -> Criticality { Criticality::Critical }
    async fn check(&self) -> Result<CheckDetail, HealthError>; // detail carries latency + optional info
}
```

Semantics wired into the existing `readiness()` handler in `plugins/umbral-health/src/lib.rs`:

- Liveness (`/healthz`) stays unconditional 200 by default but may run `Probe::Liveness` checks that assert only "this process is not deadlocked" (never a network call, keeping the no-flapping-pods contract from the current doc comment).
- Readiness (`/readyz`) runs `Probe::Readiness` checks. A `Critical` failure -> overall 503 (today's behavior). A `Degraded` failure -> pod stays 200/ready but the JSON body reports that check as `"status":"degraded"`, so a non-critical dependency (e.g. analytics sink) does not pull the pod from rotation. This is the readiness/liveness profile split the gap asks for and the substrate for #70's degradation-modes doc and the readiness SLO band.
- The existing `require_migrations()` gate becomes one registered `Critical` readiness check named `migrations` (built on `drift_report()` exactly as today), so it stops being a special case.
- Reasons stay generic in the unauthenticated body (existing audit_2 rule), with the real error logged server-side. `CheckDetail` latency is safe to expose and helps operators.

Standardized built-in checks, each contributed by the plugin that owns the dependency (respecting the plugin architecture: the health registry defines the trait, each plugin ships its own check via `Plugin::health_checks()`, a new optional trait hook, rather than umbral-health depending on every plugin):

- `database` (built in today; move into the registry as a `Critical` readiness check),
- `migrations` (from `require_migrations`),
- `redis` (from the cache/sessions plugin when a Redis backend is configured; PING),
- `s3` (from `umbral-storage` when an S3 backend is configured; HEAD bucket),
- `email` (from `umbral-email`; SMTP connect / provider reachability, `Degraded` by default since a queued mailer can tolerate a brief outage),
- `oauth_providers` (from `umbral-oauth`; discovery/JWKS endpoint reachability, `Degraded`),
- `task_workers` (from `umbral-tasks`; last worker heartbeat within a window, via the ORM over the tasks table, no raw SQL),
- `realtime_broker` (from `umbral-realtime`; broker reachability, `Degraded` given best-effort delivery),
- `disk` (core; free-space threshold on the media/temp dir, `Critical` when below a floor).

Each built-in registers only when its plugin and backend are actually configured, so a Redis-free app never reports a `redis` check. The `Plugin::health_checks()` hook is how a plugin contributes without umbral-health knowing about it, keeping the dependency arrow pointing inward (health defines the trait; plugins implement it), which is the same inversion the whole framework rests on.

Readiness/liveness profiles as a named preset:

```rust
HealthPlugin::default()
    .profile(HealthProfile::Kubernetes)   // liveness = process-only; readiness = all Critical + migrations
    .register_plugin_checks()             // pull each installed plugin's health_checks()
```

`HealthProfile::{Minimal, Kubernetes, Strict}` are opinionated presets: `Minimal` = DB only (today's default), `Kubernetes` = DB + migrations + all critical plugin checks with the liveness/readiness split, `Strict` = also fail readiness on any `Degraded` check (for environments that want zero-degradation gating).

### Honesty

The trait, registry, profile split, and the `database`/`migrations`/`disk` checks are fully implementable and testable here (the existing tests over `evaluate_migrations` show the pattern: pure verdict functions unit-tested without a live dependency). The network checks (Redis, S3, email, OAuth, realtime) are implementable but only meaningfully testable against those live services, which this environment lacks; they get pure unit tests over their verdict mapping plus integration tests gated behind the same `UMBRAL_TEST_*` env vars the Postgres tests already use.

---

## Sequencing and dependencies

1. #64 metrics exporter is the hard prerequisite for #70's dashboards/alerts and for #71's query-duration histogram. Land it first.
2. #73's registry is a prerequisite for #70's readiness SLO band and degradation-modes doc, and it is independently valuable, so it can land in parallel with #64.
3. #71's counting-layer promotion (piece 3) should reuse `tests/query_counts.rs` so the harness and runtime detector never diverge; do that refactor first, then build slow-query logging and the toolbar on it.
4. #72 is independent and can land anytime; its criterion micros are the honest CI-runnable part, the macro suite is documented-but-infra-gated.

## What is explicitly NOT decided here

- Exact #64 metric names (owned by the #64 design). This doc references them by intent; the two must reconcile names before #70's rules are final.
- Whether the dev toolbar ships in `umbral-admin` or a standalone `umbral-devtools` plugin.
- The specific competitor versions/configs in the #72 comparison (must be pinned and disclosed at publish time, not guessed now).

# Framework Scaling By Default

## Problem

Umbral already has many of the primitives needed for production scaling, but the developer has to assemble them manually: open database pools, choose shared-state backends, run workers and beat processes, wire Redis for realtime, think about read replicas, add health checks, and avoid known unbounded query surfaces.

The framework should make scaling the default production posture. Developers should declare the app; Umbral should select the correct scalable infrastructure wiring from settings and fail loudly when a production deployment is unsafe.

## Current Building Blocks

- `AppBuilder` centralizes settings, databases, middleware, shutdown drain, system checks, and app boot.
- `Settings` and `db::PoolConfig` already expose database pool sizing and timeout knobs.
- `DatabaseRouter` can route reads and writes and is the natural extension point for read replicas.
- `umbral-tasks` has a DB-backed queue and safe horizontal task claiming.
- `umbral-realtime` can use a Redis broker for cross-process fanout.
- `umbral-cache` supports memory, SQLite, and Redis.
- `umbral-sessions` has pluggable session stores, with DB/Redis-compatible production shapes.
- `umbral-health` already exposes readiness/liveness checks and shutdown draining.

The gap is not raw capability. The gap is productizing those pieces so production apps get the safe topology by default.

## Desired Shape

Add a first-class runtime profile and preset layer:

```rust
let app = App::from_env()
    .preset(PlatformPreset::production())
    .model::<Post>()
    .plugin(RestPlugin::default())
    .build_runtime()
    .await?;
```

Profiles should separate development convenience from production safety:

- `dev`: SQLite and in-process defaults are acceptable.
- `single_node`: production-like settings, but no expectation of multiple replicas.
- `production`: shared state and explicit topology are required for unsafe features.
- `platform`: production plus stricter multi-tenant, observability, and dependency checks.

## Proposal

1. **Async boot owns infrastructure wiring**

   Add an async builder path such as `App::from_env()` or `AppBuilder::build_runtime().await` that opens the default database, opens configured database aliases, applies pool config, installs the database router, and keeps explicit `.database(...)` calls as an override.

2. **Redis becomes the shared-state switch**

   When `UMBRAL_REDIS_URL` is present under a production/platform profile, the preset should automatically wire Redis-backed cache, sessions, realtime broker, distributed throttles, and any task/result coordination that needs shared state.

   In a multi-replica production profile, missing Redis or another configured shared-state backend should be a system-check error for features that would otherwise fall back to process-local state.

3. **Process topology is generated, not remembered**

   Production deployments should have explicit `web`, `worker`, and `beat` roles. The framework should not silently hide production workers inside the web process, but the CLI/scaffold/deployment output should make the correct topology the default.

   Add task worker concurrency settings, queue-specific caps, worker heartbeats, and readiness checks for required workers.

4. **Read replicas are a first-class router**

   Ship a `ReplicaRouter::from_env()` built on `DatabaseRouter`:

   - writes route to primary
   - reads route to replicas
   - requests can pin reads to primary after writes
   - replica lag is exposed through readiness checks
   - unhealthy replicas fall back according to profile policy

5. **Durable side effects use an outbox**

   Production presets should prefer a durable outbox for emails, webhooks, analytics, realtime fanout, and other side effects that must be committed consistently with database writes.

6. **Scaling-safe data defaults**

   The framework should remove common cliffs from generated/default surfaces:

   - auto-index foreign keys
   - auto-index soft-delete predicates such as `deleted_at`
   - paginate admin relation pickers
   - cap or stream exports
   - cache generated OpenAPI specs
   - batch permission checks where possible

7. **Health and observability are part of the preset**

   Production/platform presets should install dependency health checks, migration readiness, task queue depth, worker heartbeat, Redis/realtime status, metrics, request spans, database timing, and query-profile warnings.

## System Checks

Production/platform profiles should fail or warn on:

- SQLite as the production database
- default secret key
- missing allowed hosts
- proxy settings that do not match deployment
- process-local throttles in a multi-replica deployment
- process-local realtime broker in a multi-replica deployment
- process-local cache/session state where shared state is required
- tasks plugin installed with no worker role configured
- scheduled tasks configured with no beat role
- outbox-enabled features with no relay
- read replicas configured with no lag/readiness policy
- known unbounded admin/API/export surfaces

## Acceptance Criteria

- A new production app does not need to manually open the default database pool in `main.rs`.
- A production app can run more than one web replica without changing application code.
- Unsafe process-local defaults produce system-check findings under production/platform profiles.
- Generated deployment artifacts include web, worker, and beat roles when the installed plugins require them.
- Redis-backed cache, sessions, realtime, and throttling can be enabled from settings alone.
- Read-replica routing can be enabled from settings alone.
- Health/readiness reports cover database, Redis, tasks, realtime, migrations, and configured dependencies.

## First Implementation Slice

1. Add `RuntimeProfile` and `PlatformPreset`.
2. Add async app boot that opens configured databases automatically.
3. Add production/profile-aware system checks for process-local state.
4. Auto-wire Redis-backed cache, sessions, realtime, and distributed throttles from settings.
5. Add generated deployment roles and worker concurrency defaults for tasks.
6. Ship `ReplicaRouter::from_env()` with lag/readiness checks.
7. Add an outbox plugin or preset integration for durable side effects.

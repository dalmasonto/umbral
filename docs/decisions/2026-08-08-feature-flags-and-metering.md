# Feature flags / remote config, and usage metering with quota enforcement

Status: draft for ratification (proposes the design for gaps5 #83, #84; the final call is the maintainer's)
Date: 2026-08-08
Closes: planning/gaps5.md #83 (tf #296), #84 (tf #297)

## Context: what these two build on

Neither of these is a new subsystem from scratch. Both are compositions over primitives that already ship, and both keep the framework's posture: a thin opt-in plugin, safe defaults, ORM-only data access, and pay for what you install.

The load-bearing pieces they stand on:

- **The cache seam.** `umbral-cache` (see `plugins/umbral-cache/src/lib.rs`) is a `Cache` handle over a `CacheBackend` trait with in-memory, SQLite, and Redis backends, plus a process-wide ambient handle (`umbral_cache::ambient()`) set during `App::build()`. A flag lookup on the request hot path cannot hit the DB every time; it reads the ambient cache, which is exactly the shape the flag evaluator needs.
- **Tenant attribution.** `umbral-tenants` exposes `current_tenant() -> Option<TenantKey>` (`plugins/umbral-tenants/src/lib.rs:709`), already ambient per request. Both flag targeting (roll out to tenant X) and metering (charge usage to tenant X) key off the same call. When tenants is not installed, `current_tenant()` is `None` and both features degrade to a single implicit tenant.
- **The realtime counters (#47).** `docs/decisions/2026-08-08-realtime-presence-quotas-metrics.md` #47 already defines per-tenant, per-channel counters (connections opened, connection-seconds, messages in and out, quota rejections) and states in plain terms that "this item defines what realtime meters; #84 owns aggregation, plan limits, and billing hooks." Metering is the consumer those counters were built to feed.
- **The metrics counters (#64).** `docs/decisions/2026-08-08-metrics-traceparent-spans.md` #64 defines the framework-wide counter surface (HTTP requests, DB queries, cache hits/misses, tasks enqueued/processed, storage bytes) emitted through the lightweight `metrics` facade, and #64 C explicitly commits to "the counter surface defined once and consumed by both the exporter and the per-tenant meter (#47 / #84)." Metering reads the same emission points; it does not re-instrument anything.
- **The product north star (Stage split).** `docs/decisions/2026-08-08-product-north-star.md` places billing/metering in Stage 3 (managed control plane) but says to "design the seams now so Stage 3 is additive, not a rewrite." The concrete reading for this doc: ship the metering and quota-enforcement infrastructure as Stage 2 self-hosted plumbing (an operator can meter and cap tenants on their own deployment today), and keep the full billing UI, invoice rendering, and dunning as Stage 3. We build the meter and the admission gate now; we ship a billing-hook seam (with a reference Stripe adapter) but not a billing product.
- **The admin custom-view surface.** `AdminView` (`plugins/umbral-admin/src/views.rs:22`, wired via `AdminPlugin::view(AdminView)`) gives both features their operator dashboard with no new UI framework.
- **Typegen.** `umbral typegen` (`crates/umbral-cli/src/lib.rs:489`) already emits TypeScript types from the model registry (`ModelMeta`); the flag SDK surface reuses it so a frontend gets typed flag names for free.
- **The audit trail.** `umbral-core` has an audit-trail seam (`crates/umbral-core/tests/audit_trail.rs`) that logs create/update/delete on the typed and dynamic ORM paths; the flag audit log rides this rather than inventing a second audit mechanism.

Both features ship independently. #83 (flags) is smaller and self-contained; #84 (metering) is larger and leans on the #47 and #64 counter work, so #83 can land first while #64's exporter stabilizes.

---

## #83 (tf #296): Feature flags and remote config

### Problem

There is no way to turn a code path on or off without a redeploy, no way to roll a change out to 5 percent of users and watch it, no way to target a flag at one tenant or one user, and no kill switch to disable a misbehaving feature in production without shipping a revert. Firebase Remote Config and every mature platform ships this; umbral has nothing. Every app that wants a gradual rollout hand-rolls an env var plus a redeploy, which is neither gradual nor observable.

### Design

A new optional plugin, `umbral-flags`, owning one small model and a cache-backed evaluator.

**A. The Flag model.** One table, owned by the plugin's migrations, holding the flag definition and its targeting rules:

```rust
#[derive(Model)]
struct Flag {
    id: i64,
    key: String,              // unique; the stable name code and SDK reference
    description: String,
    enabled: bool,            // the master kill switch: false forces every eval to the off variant
    default_variant: String,  // the value returned when no targeting rule matches ("on" / "off" / a JSON string)
    rollout_percent: i16,     // 0..=100; sticky-hashed rollout, 0 disables, 100 forces on
    targeting: Json<Targeting>, // user and tenant include/exclude lists, described below
    updated_at: DateTime<Utc>,
    updated_by: Option<String>, // the actor PK string, for the audit trail
}
```

`Targeting` is a typed struct serialized into the `Json` column (never raw SQL), holding `users_include`, `users_exclude`, `tenants_include`, `tenants_exclude`, and an optional list of typed attribute predicates (`attribute == value`) for the ABAC-lite case. Keeping targeting as one typed JSON column rather than a satellite table keeps a single-row read sufficient to evaluate a flag, which is what makes the cache layer trivial.

**B. Evaluation order.** `flags::eval(key, ctx)` resolves a flag against an evaluation context (the current user PK, the current tenant from `current_tenant()`, and a bag of attributes the caller supplies). The order is fixed and short-circuiting, so the outcome is always explainable:

1. Flag missing or `enabled == false` (kill switch): return the off variant. The kill switch wins over everything, by design, so an operator can always force a feature dark.
2. `users_exclude` / `tenants_exclude` match: return the off variant.
3. `users_include` / `tenants_include` match: return `default_variant` (the on side).
4. Attribute predicates all match: return `default_variant`.
5. `rollout_percent` bucket: hash `(flag.key, stable_id)` to a stable 0..=99 bucket where `stable_id` is the user PK (or the tenant key, or a supplied anonymous id); return the on variant when `bucket < rollout_percent`. Same id plus same flag always lands in the same bucket, so a user does not flap between variants across requests, and raising the percentage only ever adds users.
6. Otherwise: return the off variant.

The result is a small `FlagDecision { variant, reason }` where `reason` names which rule fired, so the admin UI and the SDK can both explain "why am I in this variant". `flags::is_enabled(key, ctx)` is sugar over `eval` returning a bool for the common on/off flag.

**C. Cache-backed lookup.** Evaluating a flag on every request must not be a DB round-trip. On boot and on every write, the plugin loads all flag rows into the ambient cache under a single versioned key (`umbral:flags:snapshot`), and `eval` reads that snapshot. Writes (through the admin UI or the management helper) bump the snapshot: they write the row through the ORM, then refresh the cache entry, and, when the Redis cache backend is in use, publish an invalidation so every replica reloads. With the in-memory backend (single process) the refresh is local. This gives sub-microsecond evaluation with a bounded staleness window (immediate on the writing node, one cache-propagation hop on peers) rather than per-eval DB load. A cache miss (cold start, evicted key) falls back to a single `Flag::objects()` load that repopulates the snapshot, so correctness never depends on the cache being warm.

**D. Kill switches.** The kill switch is not a separate concept; it is `enabled = false` on the Flag row, checked first in evaluation (step 1). An operator flips one boolean in the admin and every replica serves the off variant within the cache-propagation window, no redeploy. This is deliberately the cheapest possible operation because it is the one an operator reaches for during an incident.

**E. Audit trail.** Every write to a Flag row (create, toggle, rollout change, targeting edit) is recorded through the existing `umbral-core` audit-trail seam, capturing the actor (`updated_by`), the before/after of the changed fields, and a timestamp. Flags are exactly the kind of production control where "who turned this on, and when" is a question an operator will ask during a postmortem, so the audit log is not optional; it is on whenever the plugin is installed.

**F. Admin UI.** An `AdminView` (`AdminPlugin::view(...)`) rendering the flag list, each flag's current state, its rollout percentage, its targeting rules, and its recent audit history, with toggle and rollout-slider controls that go through the same write-plus-invalidate path as the programmatic API. Per the dogfooding rule, any rollout charts route through ApexCharts, never hand-rolled SVG. Write access is gated behind a permission (`flags.change`) so flag control is a privileged operation.

**G. SDK exposure and typegen.** Two exposure paths, both opt-in:

- **Server-side:** `flags::is_enabled("new_checkout", &ctx).await` and `flags::eval(...)` are the in-process API for handler and template code.
- **Client-side:** an optional `GET /flags/evaluate` endpoint (gated by the same identity resolution the rest of the app uses) returns the evaluated variant map for the current user/tenant context, so a browser or mobile client gets its flag decisions from the server rather than re-implementing the bucketing (which would need the raw rules and leak targeting). `umbral typegen` extends to emit a typed `FlagKey` union and a `Flags` interface from the registered flag keys, so a TypeScript frontend references `flags.newCheckout` with compile-time key checking rather than stringly-typed lookups. This reuses the existing `ModelMeta` to TypeScript pipeline; flags register their keys the same way models register their fields.

### Safety defaults

The plugin adds no table until installed. A missing flag evaluates to off (fail-closed: an un-declared or deleted flag never accidentally enables a code path). The kill switch always wins. The client `/flags/evaluate` endpoint returns only evaluated variants for the caller's own context, never the raw targeting rules (so a client cannot enumerate which users or tenants a flag targets). Flag writes require the `flags.change` permission and are audited.

### Deferred

Scheduled flag changes (turn on at a timestamp), multivariate experiment analytics with statistical significance (a flag returns a variant; measuring conversion against it is an analytics job, not the flag engine's), a streaming SDK that pushes flag changes to clients over the realtime channel (the pull endpoint plus cache invalidation covers the common case; realtime push is a follow-up), and per-environment flag values (belongs with the Stage 3 environment-promotion model, gaps5 #92).

---

## #84 (tf #297): Usage metering and quota enforcement

### Problem

If umbral is to serve the Supabase/Firebase-shaped use case, an operator needs to answer "how much is each tenant using" and "cap a tenant at its plan limit", for API calls, storage bytes, realtime connections and messages, task executions, DB rows, and seats. Today none of that is measured against a tenant, no quota is enforced, and there is no seam a billing system could read. The realtime work (#47) and metrics work (#64) already produce the raw counters; nothing aggregates them per tenant, compares them to a plan, or stops a tenant that is over its limit.

Per the north star, the full billing product (invoices, dunning, a customer billing portal) is Stage 3. This item builds the Stage 2 infrastructure underneath it: the meter, the per-tenant aggregation, the plan-limit model, admission-time enforcement, and a billing-hook seam with a reference Stripe adapter. It stops short of rendering an invoice.

### Design

`umbral-metering`, an optional plugin owning two models, an ingest path fed by the existing counters, an admission gate, and a billing-hook trait.

**A. The metered resources.** One enum names every resource the framework can meter, so a plan limit and a usage row always speak the same vocabulary:

```rust
enum Resource {
    ApiCalls,            // per-request, from the #64 HTTP counter
    StorageBytes,        // gauge, from umbral-storage
    RealtimeConnections, // from the #47 connection counter
    RealtimeMessages,    // from the #47 message counter
    TaskRuns,            // from the #64 tasks-processed counter
    DbRows,              // sampled row counts per tenant-owned table
    Seats,               // active users in the tenant, from auth
}
```

**B. UsageEvent and UsageCounter.** Two models, split by write frequency:

```rust
// append-only, high-volume; one row per metered occurrence or per flush batch
#[derive(Model)]
struct UsageEvent {
    id: i64,
    tenant: String,        // current_tenant(), or "_untenanted" (never exempt; matches the #47 fail-safe)
    resource: Resource,
    quantity: i64,         // API calls in the batch, bytes delta, message count, ...
    occurred_at: DateTime<Utc>,
    idempotency_key: Option<String>, // dedupes a retried flush
}

// the rolled-up current window, one row per (tenant, resource, period); the admission gate reads THIS
#[derive(Model)]
struct UsageCounter {
    id: i64,
    tenant: String,
    resource: Resource,
    period: String,        // "2026-08" for a monthly meter, or a rolling-window id
    used: i64,             // running total for the period
    updated_at: DateTime<Utc>,
}
```

`UsageEvent` is the audit-grade ledger (every occurrence, for reconciliation and billing export); `UsageCounter` is the cheap read the admission gate consults. Both go through the ORM (`UsageEvent::objects().bulk_create(...)`, `UsageCounter::objects().filter(...).update_expr(...)` for the atomic increment), never raw SQL, so the whole thing works on SQLite and Postgres identically.

**C. Ingest: fed by the existing counters, not a second instrumentation pass.** This is the central design point. The framework already increments per-tenant counters at every metered edge (#47 for realtime, #64 for HTTP/DB/tasks/storage/cache). Metering does not re-instrument those paths; it registers a **meter sink** that those same emission points feed. Concretely, the counter surface #64 committed to being "consumed by both the exporter and the per-tenant meter" gains one consumer: a bounded in-memory per-(tenant, resource) accumulator that a background task flushes to `UsageEvent` + `UsageCounter` on an interval (default 10s) or when a batch fills. Hot-path cost is one atomic add per metered event, exactly what the exporter already pays; the DB write is amortized across the flush window. A crash loses at most one flush window of un-persisted counts (acceptable for metering; the ledger is eventually-consistent by design, and the idempotency key makes a retried flush safe). Storage bytes and seats are gauges, not events, so they are sampled on the flush tick (`filter(tenant).count()` through the ORM) rather than accumulated.

```rust
// the shared counter surface (defined in #64, consumed here)
meter.record(tenant, Resource::ApiCalls, 1);          // called from the same spot #64's counter fires
meter.record(tenant, Resource::RealtimeMessages, n);  // from #47's record_message
// gauges sampled on the flush tick, not recorded per event:
meter.sample(tenant, Resource::StorageBytes, bytes_for_tenant);
```

**D. Plan limits and quota enforcement at admission.** A `Plan` associates a tenant with per-resource limits:

```rust
#[derive(Model)]
struct Plan { id: i64, name: String, limits: Json<HashMap<Resource, i64>> }
#[derive(Model)]
struct TenantPlan { id: i64, tenant: String, plan_id: i64 }  // which plan a tenant is on
```

Enforcement is an **admission check**, run at the point a resource is about to be consumed, that compares the tenant's `UsageCounter.used` for the period against its plan limit for that resource:

- **API calls:** a middleware (opt-in, `MeteringPlugin::enforce_api_calls()`) that rejects with `429 Too Many Requests` plus a `Retry-After` when the tenant is over its monthly API-call limit. This composes with, and is distinct from, rate limiting: throttling is short-window burst control, quota is a plan-period cap.
- **Realtime connections/messages:** reuses the exact admission point #47 already defined. #47 rejects at the node cap and per-tenant realtime quota; the plan limit is one more per-tenant ceiling checked at the same `register` admission point, returning the same `503`. Metering owns the plan-limit number; #47 owns the mechanism.
- **Storage bytes:** checked at the upload-admission boundary (`umbral-storage`), rejecting an upload that would push the tenant over its byte quota.
- **Task runs, DB rows, seats:** these are checked at their natural admission edge (enqueue, row insert on a tenant-owned table, user activation) when the corresponding `enforce_*` builder is enabled; each is opt-in because the right failure mode is app-specific (reject vs. soft-cap-and-alert).

The enforcement read is a single indexed `UsageCounter` lookup, cache-warmed the same way flags are (the current period's counters for the active tenant live in the ambient cache, refreshed on flush), so admission adds a cache read, not a DB round-trip. Fail direction is deliberate and matches #47: when tenant attribution is unavailable, usage is charged to a synthetic `"_untenanted"` bucket rather than exempted, so a misconfiguration cannot become an unmetered bypass. When a plan or limit is missing, the default is **allow and meter** (measure first, do not accidentally lock out a tenant with no plan row), which an operator can flip to **deny** with `MeteringPlugin::default_deny()` once every tenant is provisioned.

**E. Billing hooks and the reference Stripe adapter.** The billing seam is a trait, so the meter never depends on a payment provider (dependency inversion, same as every other plugin edge):

```rust
#[async_trait]
trait BillingAdapter: Send + Sync {
    // report a period's usage to the billing provider (metered/usage-based billing)
    async fn report_usage(&self, tenant: &str, resource: Resource, quantity: i64, period: &str) -> Result<(), BillingError>;
    // called when a tenant crosses a limit, for overage handling or notification
    async fn on_quota_exceeded(&self, tenant: &str, resource: Resource) -> Result<(), BillingError>;
}
```

We ship one reference impl, `StripeAdapter`, that maps each `Resource` to a Stripe metered-billing subscription item and reports usage on the flush tick (or a coarser billing tick). It is a thin `reqwest` client against the Stripe usage-records API, feature-gated (`feature = "stripe"`) so a metering-only deployment that never bills pulls in no HTTP client. The adapter is a starting point, documented as such; an operator with a different billing provider implements the same trait. Crucially, reporting usage to Stripe reads the same `UsageEvent` ledger, so billing and internal metering can never diverge: there is one source of truth.

**F. Operator dashboard.** An `AdminView` rendering per-tenant usage against plan limits (a usage bar per resource), the tenants nearest their caps, and a usage-over-time chart per resource (ApexCharts, per the dogfooding rule). Its data endpoint reads `UsageCounter` and `UsageEvent` through the ORM, so it needs no privileged access.

### Safety defaults

The plugin adds no tables until installed and meters nothing until a resource's recording is wired (though the counter surface it reads is the same one the exporter already populates, so enabling metering is cheap). Enforcement is off until an `enforce_*` builder is called, so installing the meter does not silently start rejecting traffic. The missing-plan default is allow-and-meter, not deny, so measuring a tenant never locks it out by accident. Untenanted usage is charged, never exempted. The Stripe adapter is feature-gated and off by default, so metering never implies an outbound billing call unless the operator asks for one. No usage row carries PII: a tenant appears as its opaque `TenantKey` string, a seat count is a number, never a user list.

### Ties to other items

- **#47 (realtime quotas):** #47 defines and emits the realtime counters and owns the realtime admission mechanism; #84 aggregates those counts per tenant, holds the plan limit, and reports them to billing. The realtime per-tenant quota (#47) and the realtime plan limit (#84) are two ceilings checked at the same admission point; #47 owns the noisy-neighbor fair-share number, #84 owns the plan-period cap.
- **#64 (metrics):** the HTTP/DB/tasks/storage/cache counters #64 defines are the same emission points the meter sinks from. #64 C already committed the counter surface to being consumed by both the Prometheus exporter and this meter; this item is that second consumer. Defining the counters once avoids double-counting.
- **North star Stage 3:** the `BillingAdapter` trait, the `Plan`/`TenantPlan` models, and the per-tenant `UsageCounter` are exactly the seams the Stage 3 managed control plane (gaps5 #5) and project/team RBAC (gaps5 #88) would build on. Building them now as Stage 2 self-hosted plumbing makes Stage 3 additive.

### Deferred (Stage 3)

Invoice rendering and a customer-facing billing portal, dunning and payment-retry workflows, proration and mid-period plan changes, credit/discount/coupon logic, tax computation, multi-currency, and a billing-events reconciliation UI. All of these are the billing product, not the meter; this item ships the meter and the admission gate and stops at the `BillingAdapter` seam. Also deferred: cross-replica exact metering (the flush-window model is eventually-consistent; exact real-time global counts would need the shared Redis limiter from gaps5 #67), and usage-based alerting thresholds (an alert when a tenant hits 80 percent of a limit belongs with the incident-readiness work, gaps5 #70).

---

## Summary

Both items are compositions over primitives that already ship, not new subsystems. #83 adds a `umbral-flags` plugin: one `Flag` model with typed targeting, a fixed short-circuiting evaluation order (kill switch first, then exclude, include, attributes, sticky-hashed percentage rollout), cache-backed lookup through the existing ambient `umbral-cache` so evaluation is not a DB round-trip, an audit trail on every write via the `umbral-core` audit seam, an `AdminView` operator console, and SDK exposure through a context-scoped `/flags/evaluate` endpoint plus a `typegen`-emitted typed flag surface. It fails closed (a missing flag is off) and gates writes behind a permission. #84 adds a `umbral-metering` plugin fed by the #47 realtime counters and #64 metric counters (one more consumer of a surface already committed to be shared, not a second instrumentation pass): a high-volume `UsageEvent` ledger plus a rolled-up `UsageCounter` the admission gate reads, per-tenant plan limits enforced at each resource's admission edge (429 for API calls, the #47 503 for realtime, upload rejection for storage), and a `BillingAdapter` seam with a feature-gated reference Stripe adapter that reports from the same ledger so billing and metering never diverge. Per the north star, #84 ships the Stage 2 meter and quota gate now and keeps the full billing product (invoices, dunning, portal) as Stage 3, building the exact seams that later stage would compose. Each is opt-in, each adds no table until installed, each attributes untenanted usage rather than exempting it, and each preserves the framework's secure-by-default, pay-for-what-you-install posture.

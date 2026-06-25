# Review: umbral-health

Read-only audit, 2026-06-16. Scope: `plugins/umbral-health/src/lib.rs` and `tests/integration.rs`. Cross-referenced against `planning/hardening/backlog.md`, `reviews/security.md`, and `reviews/performance-scalability.md`.

NET-NEW items only. No prior entry specifically covers `umbral-health`.

---

## Verdict

**Complete and correctly designed.** `/healthz` (liveness, unconditional 200) and `/ready` (readiness, DB probe + registered checks, 503 on any failure) are both real implementations, not stubs. The liveness/readiness split is correctly motivated and matches Kubernetes conventions. The JSON format, the `HealthError::reason` surfacing, and the `route_paths()` announcement are all present.

**Worst finding:** `probe_database` uses raw `sqlx::query("SELECT 1")` directly against the pool — a CLAUDE.md-flagged ORM bypass (though `SELECT 1` is one of the two explicitly permitted DDL-level exceptions). Additionally, the readiness checks run sequentially with no per-check timeout, so a single blocked `HealthCheck` implementation can hang `/ready` indefinitely. Neither is critical, but the timeout gap is the most impactful finding.

---

## Completeness

| Area | Status |
|---|---|
| Liveness endpoint (`GET /healthz`) | Complete. Always 200 + `{"status":"ok"}`. |
| Readiness endpoint (`GET /ready`) | Complete. DB probe + registered checks, 200 or 503. |
| Check registry | Complete. `HealthPlugin::check(C: HealthCheck)` builder. |
| DB connectivity check | Complete. `SELECT 1` on the ambient pool, dispatched per `DbPool` variant. |
| JSON format | Complete. `{"status":"ok|fail","checks":{"name":{"status":"ok|fail","reason":"..."}}}`. |
| `route_paths()` | Complete. Announces both endpoints. |
| `HealthCheck` trait | Complete. `name()` + `async fn check()` returning `Result<(), HealthError>`. |
| `HealthError` | Complete. `reason: String`; `Display` + `Error` impl. |
| Stubs / todo | None found. |

---

## Findings

### HE-1 — `probe_database` uses raw `sqlx::query` (ORM bypass) (NEW)

**Severity: Important**

`lib.rs:240-253`: `probe_database` calls `sqlx::query("SELECT 1").execute(&*p).await` directly against the SQLite and Postgres pool handles. This is a CLAUDE.md-flagged ORM bypass — every plugin should route DB access through the ORM, not raw sqlx.

The mitigation: `SELECT 1` is a connectivity probe, not a row-level operation. It does not read, write, or mutate application data. The CLAUDE.md explicitly lists "Schema DDL ... Owned by the migration engine" as a permitted raw-SQL exception, and while `SELECT 1` is not DDL, it falls into the category of "backend-specific operation the ORM can't model" — there is no ORM equivalent of a raw connectivity ping.

However, the correct approach per CLAUDE.md is: if the ORM can't express it, the right fix is to add the operation to the ORM (e.g. `umbral::db::ping()` → `SELECT 1` dispatched internally). Health checks exist in every production app; the framework should expose a typed `db::ping()` rather than requiring every plugin that needs to probe connectivity to write its own raw sqlx.

**Fix:** Add `pub async fn ping() -> Result<(), sqlx::Error>` to `umbral::db` (in `umbral-core`, re-exported from the facade) that issues `SELECT 1` against the ambient pool. `probe_database` then calls `umbral::db::ping().await.map_err(|e| e.to_string())`. This closes the raw-SQL exception for the health plugin and makes the connectivity probe reusable.

**Gap:** NEW — add `umbral::db::ping()` to the ORM surface. Until it exists, the current `sqlx::query("SELECT 1")` is the narrowest acceptable workaround; add a comment citing this gap.

---

### HE-2 — No per-check timeout; a slow/blocked check hangs `/ready` indefinitely (NEW)

**Severity: Important**

`lib.rs:208-223`: `HealthCheck` impls are awaited sequentially with no timeout wrapping. A `HealthCheck` that hangs (e.g. a Redis ping to a host that accepts the TCP connection but never responds, or a third-party API with a 60s default timeout) will block the `/ready` handler for the full duration.

Kubernetes readiness probes have their own timeout (`timeoutSeconds`, default 1s), but a slow check still holds an async worker and delays the 503 the load balancer needs to pull the pod from the rotation. The doc comment (`lib.rs:87-89`) advises "keep your check fast (under a few hundred ms)", but does not enforce it.

**Fix:** Wrap each `check.check().await` in `tokio::time::timeout(Duration::from_secs(CHECK_TIMEOUT_SECS), ...)`. On timeout, treat the check as failed with `HealthError::new("timed out after {N}s")`. Make the default 5s (conservative but bounded) and expose a `timeout()` builder on `HealthPlugin` for apps that need different values.

**Gap:** NEW.

---

### HE-3 — `/healthz` and `/ready` are unconditionally public — no mention in docs (NEW)

**Severity: FYI**

`lib.rs:111-115` (doc comment): "Gate them off your reverse proxy or auth middleware if you don't want them publicly reachable. They never carry authentication — by design". This is correct reasoning, but:
- The `/ready` response surfaces the names of registered checks and their failure reasons (e.g. `{"checks":{"redis":{"status":"fail","reason":"connection refused at redis://internal.corp"}}}`) — leaking internal service topology to any caller who can reach the pod.
- In a public-internet deployment where liveness/readiness routes are accidentally exposed (common behind a misconfigured ALB), this is an information-leak.

There is no mechanism to restrict access beyond "use your reverse proxy". An `Environment::Prod` boot warning when `HealthPlugin` is installed without a listed IP allowlist would help operators catch the mistake.

**Fix (Optional):** Add a builder method `HealthPlugin::default().allowed_ips(vec!["127.0.0.1/32", "10.0.0.0/8"])` that gates both endpoints to the listed CIDRs and returns 404 to others. Default: open (to not break zero-config k8s). Add a boot `check.rs` warning in `Prod` when no IP restriction is set and the bind address is non-loopback.

**Gap:** NEW.

---

### HE-4 — Sequential check execution multiplies tail latency (NEW)

**Severity: Optional**

`lib.rs:208-223`: checks run in registration order, one at a time. If `N` checks each take `T` ms, `/ready` takes `N × T` ms. For a typical 3-check setup at 100ms each, that is 300ms — within bounds. But a slow DB probe at 500ms followed by a Redis probe at 100ms adds up.

The existing comment ("Run sequentially rather than concurrently — concurrency would multiply tail latencies and amplify the cost of one slow check across every probe") is backwards: sequential execution **sums** tail latencies, while concurrent execution exposes only the **max**. The comment was likely written with "concurrent means all checks run simultaneously so a fast check waits for a slow one" in mind, but that is exactly what sequential execution does.

**Fix:** Run all checks (including the DB probe) concurrently with `tokio::try_join_all` or `futures::future::join_all`. Each check should already be wrapped in a per-check timeout per HE-2. The total `/ready` latency becomes `max(check_latencies)` rather than `sum(check_latencies)`.

**Gap:** NEW.

---

## Plugin-contract

- **Facade-only imports:** PARTIAL. `lib.rs:57-59` imports `umbral::db::DbPool`, `umbral::plugin::Plugin`, and `umbral::routes::RouteSpec` — all through the facade. However `lib.rs:16-17` imports `axum::...` directly (allowed — `axum` is a listed direct dep). The raw `sqlx::query` in `probe_database` is the notable exception (HE-1).
- **Migrations:** None. No persisted schema. Correct.
- **`Plugin` impl:** Complete. `name()`, `routes()`, and `route_paths()` all present. No `on_ready()` needed (state is built at `routes()` time from the registered checks).

---

## Tests

| Test | File | Covers |
|---|---|---|
| `liveness_always_returns_200` | `tests/integration.rs:29-45` | `/healthz` status + JSON body |
| `readiness_returns_200_when_db_is_up_and_no_checks_registered` | `tests/integration.rs:47-65` | `/ready` with live in-memory SQLite, no custom checks |
| `readiness_surfaces_registered_check_results` | `tests/integration.rs:89-117` | Mixed pass/fail checks → 503, correct per-check JSON |
| `route_paths_announces_both_endpoints` | `tests/integration.rs:119-125` | `route_paths()` contract |

**Gaps:**
- No test for a hanging/slow check (HE-2 has no coverage; the timeout path, once added, needs a test with a mock that sleeps past the deadline).
- No test for the Postgres pool path in `probe_database` (only SQLite is exercised in the test suite; the Postgres `sqlx::query` branch is untested).
- No test for `/ready` when the DB probe fails (the test uses an in-memory SQLite that is always up; a test that kills/closes the pool after boot would cover the `Err` path in `probe_database`).
- No test that concurrent check execution (once HE-4 is fixed) preserves the correct pass/fail aggregation.

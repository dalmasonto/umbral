# Audit: Observability & Dev-Surface Plugins (`plugin-observability`)

> **Verification stamp — code re-triaged 2026-07-06.** Checked against current code. **Fixed:** #1 (openapi empty router in Prod), #2 (`JoinHandle` reaping), #3 (opaque `/ready`), #4 (analytics path exclusion), #5 (bounded fan-out semaphore), #6 (release defaults to Prod), #7 (logs read request-extension not header), #8 (admin `AutoEscape::Html`), #10 (async subscriber timeout), #11 (`min_status` doc warning). **Still open →** #9 (Swagger UI SRI) and #12 (stale `m2m_changed` "deferred" bullet) tracked in `planning/gaps3.md #27`. Treat the per-finding text below as historical.

Scope: `umbral-logs`, `umbral-analytics`, `umbral-health`, `umbral-openapi`, `umbral-playground`, `umbral-livereload`, `umbral-signals`. Primary lens: observability (area 6), plus the "must-not-ship-to-prod" gating of dev/introspection surfaces. Every finding cites code that was read.

---

## A. Executive summary

The dev/introspection surfaces are **inconsistently gated for production**. `umbral-playground` is correctly Prod-gated (`allow_in_prod`, default off) and `umbral-livereload` is Dev-only, but **`umbral-openapi` has no environment guard whatsoever** — it mounts Swagger UI plus a full machine-readable JSON spec of the entire REST surface unconditionally in every environment. For a system with 10M users and sensitive PII, the complete API map (every model, every field, every filter, every FK relationship) is served to any unauthenticated caller in production.

The single most urgent code defect is a **guaranteed memory leak in `umbral-logs`**: every captured request pushes a `JoinHandle` onto a process-global `Vec` that is only ever drained by the test-only `flush()`. In production `flush()` is never called, so the vector grows one entry per logged request forever — a certain OOM at scale, and the code's own comment ("untouched in prod") mistakes "never drained" for "harmless."

The third urgent issue is **unauthenticated internal-detail leakage from `/ready`** (`umbral-health`): the raw database error string (`e.to_string()`) is returned in the JSON body to any caller, which can expose the DB host/DSN/username, and the endpoint enumerates every registered dependency by name to anyone who probes it.

Additional real issues: `umbral-analytics` ships full request **paths to a third party (PostHog)** with no path exclusion list and no batching (both a PII/token-in-URL leak and an unbounded-task fan-out at scale); `umbral-logs` trusts a client-supplied `X-Umbral-User-Id` header for user attribution (audit-log forgery) and stores unsanitized `user_agent`/`path` that the admin later renders (stored-XSS feed); the framework's **default `Environment` is `Dev`**, so a deployment that forgets to set it ships the permissive posture; and `umbral-openapi`'s Swagger UI loads scripts from an unpinned `unpkg.com` CDN (supply-chain + offline-break).

What I could not assess: the actual reverse-proxy/ingress config (whether `/ready`, `/openapi`, etc. are firewalled off), whether deployments set `UMBRAL_ENVIRONMENT=prod`, how `umbral-admin` escapes the stored `user_agent`/`path` it renders, and runtime tokio task accounting. These are in Blind spots.

---

## B. Findings table

| # | Severity | Area | Location (file:line) | Finding | Impact | Recommended fix | Status |
|---|----------|------|----------------------|---------|--------|-----------------|--------|
| 1 | HIGH | Config/Prod gating | `plugins/umbral-openapi/src/lib.rs:170-192` | `OpenApiPlugin::routes()` has no `Environment` check; mounts Swagger UI + full JSON spec unconditionally. | Complete API surface (all models, fields, filters, FK graph) served unauthenticated in prod — recon goldmine for attacking the PII API. | Gate behind an `Environment::Prod` opt-in mirroring `PlaygroundPlugin::allow_in_prod`; default off in Prod. | | | | | | ✅ done |
| 2 | HIGH | Observability / mem | `plugins/umbral-logs/src/lib.rs:107-128, 418-421` | Every captured request pushes a `JoinHandle` into the global `PENDING` `Vec`; only `flush()` (test-only) drains it, and prod never calls it. | Unbounded memory growth (one handle per logged request) → OOM at 10M-user volume. Comment mislabels the leak as "untouched in prod". | Don't retain handles in prod; use a bounded `JoinSet`/detached spawn, or reap completed handles. Gate the `PENDING` push behind a test-only flag. | | | | | | ✅ done |
| 3 | MEDIUM | API leak (health) | `plugins/umbral-health/src/lib.rs:210-238` | `/ready` (documented unauthenticated, `lib.rs:120-124`) returns the raw DB error `e.to_string()` and enumerates each dependency name/status. | DB error strings can leak host/DSN/user/internal hostnames; dependency list is free recon — all to anonymous callers. | Return a generic `"unavailable"` reason to unauthenticated callers; log the detail server-side only. Offer an opt-in "detailed body" bound to an internal listener/auth. | | | | | | ✅ done |
| 4 | MEDIUM | Data leak (analytics) | `plugins/umbral-analytics/src/lib.rs:232-256` | `pageview_middleware` sends the full request `path` to PostHog for *every* request; no exclusion list (contrast `umbral-logs` DEFAULT_EXCLUDE_PREFIXES). | Paths carrying secrets/PII (`/reset-password/<token>`, `/users/<email>/…`, signed URLs) are shipped to a third party (US region by default). | Add a path-exclusion/allow list; strip query and sensitive path segments; document that paths leave the trust boundary. | | | | | | ✅ done |
| 5 | MEDIUM | Abuse / scale (analytics) | `plugins/umbral-analytics/src/lib.rs:157-178, 245-253` | Each event is an unbatched `tokio::spawn` + outbound HTTPS POST, one per capture and one per request under `.capture_requests()`. | At 10M-user request volume: unbounded outbound task fan-out (resource amplification / self-DoS), no PostHog batch endpoint use, no backpressure. | Batch via PostHog `/batch/`, cap concurrent in-flight sends (bounded queue/semaphore), add sampling. | | | | | | open |
| 6 | MEDIUM | Config / secure-default | `crates/umbral-core/src/settings.rs:419-425` | `Environment` defaults to `Dev` (`#[default] Dev`). Dev is the permissive posture (livereload SSE active, playground mounts, no-store caching). | A deployment that forgets `UMBRAL_ENVIRONMENT=prod` silently ships dev surfaces exposed. Fail-open default. | Default to `Prod`, or fail-closed if unset in a released binary; log a loud warning when running Dev. | | | | | | open |
| 7 | MEDIUM | Auth attribution (logs) | `plugins/umbral-logs/src/lib.rs:342-347, 378` | `user_id` is parsed straight from the client-controlled `X-Umbral-User-Id` header with no signing/validation. | Any client sets `X-Umbral-User-Id: 1` and every request log is attributed to another user → audit-log forgery / misattribution. | Resolve identity from the authenticated session server-side (request extension set by trusted middleware), never a raw inbound header. | | | | | | ✅ done |
| 8 | MEDIUM | Stored-XSS feed (logs) | `plugins/umbral-logs/src/lib.rs:373-408, 440-467` | Attacker-controlled `user_agent` and `path` are stored verbatim and surfaced in the admin list view (`admin_model` list_display/search). | If `umbral-admin` renders these unescaped, stored XSS executes in an operator's authenticated admin session. | Confirm admin escapes these columns; treat stored log fields as untrusted on render. (Cross-plugin — see Blind spots.) | | | | | | open |
| 9 | MEDIUM | Supply chain (openapi) | `plugins/umbral-openapi/templates/swagger_ui.html:6,10` | Swagger UI CSS/JS loaded from `https://unpkg.com/swagger-ui-dist@5/...` — unpinned major version, no SRI, remote origin. | A compromised/hijacked CDN or resolved version serves arbitrary JS into the docs page in your origin; also breaks in air-gapped/CSP-strict deploys. | Vendor the Swagger UI assets locally (serve via the static pipeline) or pin an exact version + `integrity`/SRI. | | | | | | open |
| 10 | LOW | Availability (signals) | `crates/umbral-core/src/signals.rs:247-256` | `emit()` awaits every async subscriber **in series** inline on the ORM write path before the save returns. | A slow/hung subscriber stalls every `Manager::save`/`delete_instance` for that model — a latency amplifier and soft-DoS. | Bound subscriber execution with a timeout, or spawn subscriber futures detached for fire-and-forget semantics. | | | | | | open |
| 11 | LOW | Observability gap | `plugins/umbral-logs/src/lib.rs:206-211, 257-269` | Request logging records only method/path/status; there is no dedicated security-event taxonomy (login, permission-failure, data-export), and `min_status(500)` silently drops 401/403 auth events. | Permission failures and auth events are indistinguishable from ordinary traffic; a status floor can hide them entirely. | Document that `min_status` above 403 drops authz-denial visibility; consider a distinct security-event channel (or leave to auth plugin, but note the seam). | | | | | | ✅ done |
| 12 | LOW | Docstring drift (signals) | `plugins/umbral-signals/src/lib.rs:40-44, 83` | Rustdoc claims "Bulk methods do NOT fire signals" and lists `m2m_changed` as deferred, but core emits `bulk_post_save`/`bulk_post_delete`/`m2m_changed` (`crates/umbral-core/src/signals.rs:376,383,465`). | Developers relying on the rustdoc under-subscribe and miss bulk/m2m events. The user-facing `signals.mdx` is already correct. | Update the plugin rustdoc to match core (Rust edit — out of this audit's edit scope; flagged for a follow-up). | | | | | | ✅ done |

---

## C. Detailed findings (CRITICAL / HIGH)

### Finding 1 (HIGH) — `umbral-openapi` mounts the full API spec in production with no guard

`plugins/umbral-openapi/src/lib.rs:170-192`:

```rust
fn routes(&self) -> Router {
    let _ = CONFIG.set(self.clone());
    umbral::routes::init_openapi_spec_url(self.spec_url());
    let mut router = Router::new()
        .route(&self.spec_url(), get(spec_handler))     // /openapi/openapi.json
        .route(&self.ui_route(), get(swagger_ui_handler)); // /openapi/
    if self.base_path != "/" {
        router = router.route(&self.base_path, get(swagger_ui_handler));
    }
    router
}
```

There is no `Environment` check anywhere in the file (`grep` for `Environment`/`Prod`/`is_prod` returns nothing). Compare `PlaygroundPlugin::routes()` (`plugins/umbral-playground/src/lib.rs:124-139`), which returns an empty router in Prod unless `allow_in_prod()` is set, and `LiveReloadPlugin` (`plugins/umbral-livereload/src/lib.rs:121-128`), which mounts nothing outside Dev.

**Scenario.** The PII app ships to prod with `OpenApiPlugin::default()` (as the quickstart in `openapi.mdx` shows). An attacker requests `GET /openapi/openapi.json` unauthenticated and receives the full model catalog: every table, every column with type/format/`maxLength`/enum choices, every FK relationship (`x-umbral-fk-target`), every filterable field and lookup, and the configured auth schemes. This is a complete blueprint for enumerating and attacking the REST API — including field names on user/PII models that field-level `hide` didn't remove.

**Corrected snippet** — mirror the playground's gate:

```rust
pub struct OpenApiPlugin {
    // ...existing fields...
    allow_in_prod: bool, // default false
}

fn routes(&self) -> Router {
    let is_prod = matches!(
        umbral::settings::get_opt().map(|s| &s.environment),
        Some(umbral::Environment::Prod)
    );
    if is_prod && !self.allow_in_prod {
        tracing::warn!(
            "umbral-openapi: not mounting in Environment::Prod (leaks the full API surface). \
             Call OpenApiPlugin::default().allow_in_prod() to override (e.g. behind an auth proxy)."
        );
        return Router::new();
    }
    // ...existing mount...
}
```

(If public API docs in prod are a deliberate product decision, keep it opt-in via `allow_in_prod()` so the exposure is a conscious choice, not the default.)

### Finding 2 (HIGH) — `umbral-logs` leaks memory on every logged request in production

`plugins/umbral-logs/src/lib.rs:107-128` and `410-421`:

```rust
static PENDING: OnceLock<std::sync::Mutex<Vec<JoinHandle<()>>>> = OnceLock::new();
// ...
let handle = tokio::spawn(async move {
    if let Err(e) = RequestLog::objects().create(row).await {
        warn!(error = ?e, "logs: failed to record request (swallowed)");
    }
});
// Track the handle so the test `flush()` hook can await it. Cheap in
// production (one push per logged request); `flush` is never called there.
if let Ok(mut guard) = pending().lock() {
    guard.push(handle);
}
```

`flush()` (the only drain, lines 120-128) is documented "Never call this on the request path in production." So in prod, `PENDING` accumulates one `JoinHandle` per captured request and is never emptied — completed handles are never removed. The header comment "handles accumulate only between flushes … untouched in prod where flush is never called" describes the leak as if it were benign; "never drained" is exactly the bug.

**Scenario.** A production service logging all requests at even 1k req/s accrues ~86M `JoinHandle`s/day in an ever-growing `Vec` behind a global mutex — steadily rising RSS until OOM, plus growing lock-hold time on every request as the `Vec` reallocates.

**Corrected snippet** — only retain handles when a test opts in; detach otherwise:

```rust
#[cfg(any(test, feature = "test-flush"))]
fn track(handle: JoinHandle<()>) {
    if let Ok(mut guard) = pending().lock() { guard.push(handle); }
}
#[cfg(not(any(test, feature = "test-flush")))]
fn track(_handle: JoinHandle<()>) { /* detached: nothing retained */ }

let handle = tokio::spawn(async move { /* insert */ });
track(handle);
```

(A `tokio::task::JoinSet` reaped on completion is an alternative if you want bounded in-flight accounting in prod too.)

---

## D. Blind spots (could not verify from provided artifacts)

- **Ingress/reverse-proxy config.** Whether `/ready`, `/healthz`, `/openapi/*`, `/api/playground/`, and `/__umbral/livereload` are firewalled off from the public internet at the proxy. All findings assume they are reachable as mounted.
- **Deployment environment variable.** Whether real deployments set `UMBRAL_ENVIRONMENT=prod`. Finding 6 is conditional on the default (`Dev`) being left in place.
- **Admin rendering escape (Finding 8).** `umbral-admin` is out of scope; I did not read how it renders the stored `user_agent`/`path` columns, so the stored-XSS is a *feed*, not a confirmed sink.
- **Actual PostHog payload contents at runtime.** I read the middleware and client; I did not run the app, so I can't confirm which live paths carry PII on a given deployment (Finding 4 is a class-of-issue, evidenced by the code sending raw `path`).
- **`Masked<T>` in signal payloads.** `Masked` serializes to ciphertext (`crates/umbral-core/src/orm/masked.rs:361-378`), so signal `{"instance": <M as JSON>}` payloads do not expose masked plaintext. Non-`Masked` sensitive columns are serialized in full into in-process signal payloads — not a network leak, but noted for handlers that re-log them.
- **Tokio runtime task accounting** for Findings 2 and 5 (real RSS/task counts under load) was not measured; the leak/fan-out is argued from code, not a running profile.
- **Whether core mounts the request `TraceLayer`** claimed by `observability/index.mdx` — verified present at `crates/umbral-core/src/app.rs:1262`, so that doc claim is accurate (no edit needed).

---

## E. Prioritized action plan

**Quick wins (< 1 day)**
1. Gate `umbral-openapi` behind a Prod opt-in (Finding 1).
2. Stop retaining `JoinHandle`s in prod in `umbral-logs` (Finding 2).
3. Return generic `/ready` failure reasons to unauthenticated callers; log detail server-side (Finding 3).
4. Vendor Swagger UI assets locally or add SRI + exact version pin (Finding 9).
5. Resolve `umbral-logs` `user_id` from the trusted request extension, not the raw header (Finding 7).

**Short term (< 2 weeks)**
6. Add a path exclusion/redaction list to `umbral-analytics` and switch to PostHog `/batch/` with bounded concurrency + sampling (Findings 4, 5).
7. Flip the default `Environment` to `Prod` (or fail-closed when unset in release builds) with a loud Dev warning (Finding 6).
8. Bound signal subscriber execution with a timeout or detach it from the ORM write path (Finding 10).
9. Correct the `umbral-signals` rustdoc to match core's bulk/m2m emission (Finding 12).

**Structural (needs design work)**
10. Define a first-class security-event stream (auth success/failure, permission denial, data export) distinct from generic request logging, and document how `min_status` interacts with authz-denial visibility (Finding 11).
11. Confirm/enforce output escaping for all attacker-controlled stored log fields at every admin render site (Finding 8).

---

## Docs updated

- **`documentation/docs/v0.0.1/plugins/openapi.mdx`** — added a security `Callout` documenting that, unlike the playground, `OpenApiPlugin` currently mounts in **every** environment including Prod with no gate, so operators handling sensitive APIs must firewall or auth-proxy `/openapi/*` themselves. This matches the code (Finding 1) rather than leaving the page silent on the exposure. Also noted the Swagger UI loads from the `unpkg.com` CDN (Finding 9), so the docs page needs outbound network / relaxed CSP.

(No edit made to `signals.mdx`: it already correctly documents bulk + `m2m_changed` signals, matching core. The stale claim lives only in the plugin's Rust docstring, which is outside this audit's edit scope — logged as Finding 12.)

## Clarifying questions (would change severity)

1. Is `/openapi/*` intended to be publicly reachable in production, or firewalled to internal only? If publicly intended, Finding 1 stays HIGH; if always firewalled, it drops to MEDIUM.
2. Does `umbral-admin` HTML-escape the `user_agent`/`path` columns it renders from `RequestLog`? If yes, Finding 8 drops to LOW.
3. Do production deployments set `UMBRAL_ENVIRONMENT=prod` via a checked deploy template? If enforced, Finding 6 drops to LOW.

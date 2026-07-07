# Plugin authorization audit — `umbral-permissions`, `umbral-rls`, `umbral-security`

> **Verification stamp — code re-triaged 2026-07-06.** The findings below were checked against current code (file:line refs had drifted). **All CRITICAL/HIGH items are FIXED in code** (this file just wasn't re-annotated): R1 FORCE RLS, R2 GUC plumbing (`db.rs` `before_acquire`), R3 SQLite hard-fail, P3 `is_active` recheck, P4 PK-agnostic layer, S2 scaffold mounts SecurityPlugin. **Shipped this pass:** S1 → `SecurityConfig::production_hardened()` (commit `725ee6c3`). **Still open →** S3/P5/P6 tracked in `planning/gaps3.md #27`; P1/P2/R4/R5/R6 in `#28` (big-design / live-PG). Treat the per-finding text below as historical.

Slug: `plugin-authz`
Scope: `plugins/umbral-permissions/`, `plugins/umbral-rls/`, `plugins/umbral-security/` (every `.rs` read in full).
Auditor stance: skeptical production-readiness review for a ~10M-user deployment. Authorization is the #1 concern.

---

## A. Executive summary

The three plugins are competently written at the unit level (constant-time CSRF comparison, HMAC-signed double-submit, identifier quoting in RLS DDL, sensible header defaults), but the **row-level security plugin is fundamentally non-enforcing as shipped** and the **permission layer is easy to leave off a route**. The single most urgent issue is that `umbral-rls` runs only `ENABLE ROW LEVEL SECURITY`, never `FORCE`, and umbral's one-`DATABASE_URL`/one-pool model means the app connects as the table *owner* — Postgres exempts the owner from non-forced RLS, so **every tenant-isolation policy is silently bypassed** while `pg_policies` still shows the policy (finding R1). Compounding it, the plugin ships **zero runtime plumbing to set/scope/reset the `app.user_id` GUC** the policies depend on (finding R2), and the documented middleware pattern is unsound under connection pooling (leaks the GUC across tenants or loses it entirely). On SQLite the plugin **silently skips all policies** (finding R3), so a SQLite-backed dev/test environment — the framework's stated test backend — has no isolation and any isolation test that runs there passes while proving nothing. The integration test only asserts policies *exist*, never that they *enforce* (evidence for R1–R3).

On the permissions side: authorization is **default-allow** — a route has no authz unless the developer remembers to attach `permission_required(...)` or call `has_perm` in the handler (finding P1); there is **no object/row-level scoping** anywhere, so `permission_required("blog.change_post")` on `/posts/{id}/edit` lets any user with the model-level perm edit *any* row — IDOR by design (finding P2). The tower perm layer also skips the `is_active` check for non-superusers (deactivated users keep access until session expiry, P3) and is hard-wired to `i64` user PKs despite the plugin marketing PK-agnosticism (P4).

The security plugin is the strongest of the three but ships **no CSP and no HSTS by default** (opt-in, prod-warned only) (finding S1), and, like all umbral plugins, must be explicitly mounted — an app that forgets `SecurityPlugin::new()` has no CSRF protection and no hardening headers at all (finding S2).

**Could not fully assess:** whether `umbral::settings` is populated in the OnceLock *before* `wrap_router` runs (affects whether prod silently degrades to unsigned CSRF), umbral-auth's session expiry/invalidation behavior (affects P3 severity), and live Postgres RLS enforcement (the enforcement test is `#[ignore]`'d and needs a real PG). These are in Blind spots.

---

## B. Findings table

| # | Severity | Area | Location (file:line) | Finding | Impact | Recommended fix |
|---|----------|------|----------------------|---------|--------|-----------------|
| R1 | CRITICAL | rls / authz | `plugins/umbral-rls/src/lib.rs:293-298, 303-311` | Plugin emits only `ALTER TABLE ... ENABLE ROW LEVEL SECURITY`, never `FORCE`. In the default one-`DATABASE_URL` setup the runtime role owns the tables (migrations created them), and Postgres exempts the owner from non-forced RLS. | All RLS policies silently bypassed; app reads/writes every tenant's rows despite policies showing in `pg_policies`. False sense of isolation. | Emit `FORCE ROW LEVEL SECURITY` and/or document + require a dedicated non-owner runtime role; add an enforcement test with two tenants. |
| R2 | CRITICAL | rls / authz | `plugins/umbral-rls/src/lib.rs` (whole file — no `set_config`/GUC code anywhere; confirmed by repo-wide grep) | Policies reference `current_setting('app.user_id')` but nothing sets, scopes, or resets that GUC per request/connection. No `after_connect` hook, no per-request pinning. | If RLS were enforced, either every query errors (unset GUC, `::int` cast) or — with session-scope `set_config` on a pooled conn — the GUC leaks to the next request's user = cross-tenant exposure. | Provide a framework middleware that pins one connection/transaction per request and runs `set_config('app.user_id', $1, true)` on it before every ORM query; reset on release. |
| R3 | HIGH | rls / authz | `plugins/umbral-rls/src/lib.rs:240-249` | On SQLite the plugin logs a `warn` and returns `Ok(())`, skipping all RLS. SQLite is the framework's stated test backend. | Divergent behavior: dev/test/SQLite deployments have zero row isolation; isolation tests run on SQLite pass while enforcing nothing. Silent authorization bypass. | Make SQLite a hard boot error when policies are declared (opt-in downgrade only), or emit an app-level `WHERE`-clause fallback. At minimum, fail loudly. |
| R4 | MEDIUM | rls | `plugins/umbral-rls/tests/integration.rs:91-147` | The only PG integration test asserts policies exist in `pg_policies`; it never sets `app.user_id`, inserts rows for two users, and asserts isolation. It is also `#[ignore]`'d. | The plugin's core promise (enforcement) is untested; R1–R3 shipped unnoticed. | Add a non-ignored two-tenant enforcement test in CI against a real/containerized PG. |
| R5 | MEDIUM | rls | `plugins/umbral-rls/src/lib.rs` | Policies are append-only across boots — removing a policy from the builder leaves the DB policy in place. Postgres policies are PERMISSIVE (OR-combined). | A stale permissive policy a developer *thinks* they removed keeps granting access. | Track declared policies and DROP ones no longer declared (migration-style diff), or document the manual `DROP POLICY` requirement more prominently. | **FIXED 2026-07-07.** `apply_policies` now runs a `drop_undeclared_policies` reconcile: for each RLS-managed table it queries `pg_policies` and DROPs every policy not in the builder's declared set for that table, before (re)creating the declared ones. `RlsPlugin` owns the full policy set on its tables, so deleting a `.policy(...)` line revokes it on the next boot. New pub `RlsPlugin::apply_to(pool)` drives it; `#[ignore]`d PG test `integration.rs::undeclared_policy_is_dropped_on_reapply` (apply {A,B} → apply {A} → assert B dropped). |
| R6 | LOW | rls / injection | `plugins/umbral-rls/src/lib.rs:82-90, 283-287, 339-341` | `using` / `with_check` are interpolated verbatim into DDL (no binding possible in DDL). Documented as developer-only SQL. | SQL/DDL injection *iff* an app sources any part of the predicate from user input. | Keep the documented warning; consider a debug-time lint that rejects predicates containing obvious untrusted markers. Accepted risk otherwise. |
| P1 | HIGH | permissions / authz | `plugins/umbral-permissions/src/middleware.rs` (opt-in layer); `plugins/umbral-permissions/src/lib.rs:130-132` (`routes()` empty) | Authorization is default-allow: a route is protected only if the developer attaches `permission_required(...)` or calls `has_perm` by hand. No global default-deny, no compile-time enforcement that every route is gated. | One forgotten layer = a fully open endpoint. At 10M users this is the most likely real-world authz hole. | Offer a default-deny router wrapper / "gated by construction" builder, or a boot-time audit that lists ungated mutating routes. |
| P2 | MEDIUM | permissions / IDOR | `plugins/umbral-permissions/src/perm.rs:101-151`; `middleware.rs:218-255` | All checks are model-level (`blog.change_post`). No object/row-level scoping anywhere; the layer runs before the row is loaded. | Any user holding a model-level perm can act on *any* row → IDOR. `permission_required("blog.change_post")` on `/posts/{id}` does not scope to ownership. | Document loudly (done); ship an object-permission primitive or an in-handler ownership-check convention. Ensure app authors know the layer is not row-aware. |
| P3 | MEDIUM | permissions / authz | `plugins/umbral-permissions/src/middleware.rs:236-247, 263-272` | `is_active` is checked only in the superuser bypass branch. For a non-superuser the layer trusts the session and calls `has_perm` without re-checking `is_active`. | A deactivated (but not logged-out) user keeps every granted permission until their session expires. Contrast REST `HasPermission` which gates `is_active` (rest.rs:176-183). | Re-check `is_active` for all users in the layer, or guarantee deactivation invalidates sessions in umbral-auth. Make the two paths consistent. |
| P4 | MEDIUM | permissions | `plugins/umbral-permissions/src/middleware.rs:224, 263`; `plugins/umbral-auth/src/login_required.rs:354` (`-> Option<i64>`) | The tower perm layer resolves the user via `current_session_user_id` → `Option<i64>` and `is_superuser_safe(user_id: i64)`. Layer is i64-PK-only despite the plugin's PK-agnostic (`user_id: String`) data model and docs. | Apps with `Uuid`/`String` user PKs cannot use `permission_required` at all; silent functional gap that pushes authz into ad-hoc handler code (feeds P1). | Make the layer resolve a stringified user id; or document the limitation (done) and provide a PK-generic variant. |
| P5 | LOW | permissions | `plugins/umbral-permissions/src/middleware.rs:246` | `has_perm(...).await.unwrap_or(false)` — DB error → deny. | Correct fail-closed behavior; noted as a positive. A DB outage denies all perm-gated routes (availability, not security). | None required; consider logging the discarded error for observability. |
| P6 | LOW | permissions / DoS | `plugins/umbral-permissions/src/perm.rs:184-214`; `membership.rs` fetches | `user_perms` / `groups_for_user` fetch all rows with no LIMIT. A user in pathologically many groups/perms loads all into memory each check. | Minor; bounded by realistic group counts. | Cap or paginate if group/perm counts can be attacker-influenced. |
| S1 | MEDIUM | security / headers | `plugins/umbral-security/src/lib.rs:174-176, 219-223, 380-386` | CSP and HSTS are `None`/`false` by default. Prod only logs a `warn`; responses still ship without them. | Default deployment has no CSP (no XSS backstop) and no HSTS (SSL-stripping exposure) unless the operator opts in. | Keep dev-safe defaults but consider a prod-hard-fail or an explicit `.production_hardened()` preset; ensure the warn is not the only signal. |
| S2 | MEDIUM | security / authz | `plugins/umbral-security/src/lib.rs:289-357` (plugin must be mounted); scaffold at `crates/umbral-cli/src/scaffold.rs` | Like every umbral plugin, `SecurityPlugin` is opt-in. An app that omits it has no CSRF protection and no security headers at all. | Forgotten mount = site-wide CSRF exposure + missing hardening. | Ensure the CLI scaffold includes `SecurityPlugin::new()` by default and the getting-started docs mount it; consider warning when it's absent in prod. |
| S3 | LOW | security / CSRF | `plugins/umbral-security/src/lib.rs:424-430, 470-472, 542-546` | Signed CSRF silently degrades to plain double-submit when no `secret_key` is resolvable at `wrap_router` time. `secret` is captured once in `CsrfState::from_config` (build time), while `check_secret_key` runs later in `on_ready`. | If settings aren't in the OnceLock before `wrap_router`, prod could run degraded (plain double-submit) even though `on_ready` confirms a secret exists. Unverified — see Blind spots. | Resolve the secret at request time (or assert settings populated before `wrap_router`); add a test that a prod build never runs unsigned when `secret_key` is set. |
| S4 | LOW | security / CSRF | `plugins/umbral-security/src/lib.rs:149-153, 580-584` | `csrf_exempt_paths` bypasses CSRF by path prefix. If a session-cookie-authenticated endpoint is mounted under an exempted prefix (e.g. `/api`), it becomes CSRF-vulnerable. | Misconfiguration risk; documented tradeoff for token-auth APIs. | Keep the segment-boundary matching (already correct); document that exempted paths must be cookie-session-free. |
| S5 | INFO | security | `plugins/umbral-security/src/lib.rs:189-192, 233, 336-339` | `Server: umbral` is advertised by default (`overriding`). | Minor framework-identity disclosure (no version). Documented tradeoff. | Accepted; operators can set `server_header: None`. |

Positives worth recording (not findings): constant-time token compare via `subtle` (lib.rs:718-721); HMAC-signed double-submit with optional session binding (lib.rs:496-512); CSRF exempt-prefix boundary is segment-correct, not naive `starts_with` (lib.rs:441-446, tests 888-921); RLS identifier quoting doubles embedded `"` (lib.rs:339-341); REST `HasPermission` correctly gates inactive users *before* the superuser bypass (rest.rs:176-198); perm-layer DB errors fail closed (P5). Dependency surface is small and reputable (getrandom, hmac, sha2, subtle, tower-http) — no supply-chain red flags in these three manifests.

---

## C. Detailed findings (CRITICAL / HIGH)

### R1 (CRITICAL) — RLS enabled but never FORCEd; app runs as table owner → total bypass

**Vulnerable code** (`plugins/umbral-rls/src/lib.rs:293-311`):

```rust
pub fn render_enable_sql(&self, table: &str) -> String {
    format!(
        "ALTER TABLE \"{}\" ENABLE ROW LEVEL SECURITY",   // <-- ENABLE only, never FORCE
        escape_ident(table)
    )
}
// ...
for table in &self.tables {
    let sql = self.render_enable_sql(table);
    sqlx::query(&sql).execute(pool).await?;
}
```

**Why it fails.** Postgres non-forced RLS is *not applied to the table's owning role* (and never to `BYPASSRLS`/superuser roles). umbral uses a single `DATABASE_URL` and a single pool (`crates/umbral-core/src/db.rs:348, 436-447` — one `PgPoolOptions`, no separate migration/runtime role). `cargo run -- migrate` creates the tables, so the connecting role *owns* them; the runtime app uses that same role. Result: policies are created and appear in `pg_policies` (the integration test at `tests/integration.rs:92-105` even asserts this), but they are **never evaluated** for the app's queries. Every "tenant can only see own rows" policy is inert.

**Attack scenario.** A SaaS wires `RlsPlugin::new().policy("invoice", "own", Action::All, "org_id = current_setting('app.org_id')::int")`, sees the policy in `pg_policies`, ships. Tenant A calls `GET /invoices`; the ORM issues `SELECT * FROM invoice` as the owner role; RLS is skipped; tenant A receives **every organization's invoices**. No exploit tooling needed — normal use leaks all tenants.

**Corrected shape** (force RLS so the owner is subject to it, and/or run as a non-owner role):

```rust
pub fn render_enable_sql(&self, table: &str) -> String {
    // ENABLE turns policies on; FORCE makes them apply to the table OWNER too,
    // which the default single-role umbral deployment always is.
    format!(
        "ALTER TABLE \"{t}\" ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE \"{t}\" FORCE ROW LEVEL SECURITY",
        t = escape_ident(table)
    )
}
```

Plus documentation/tooling to run the runtime pool under a dedicated role that has DML but not ownership and not `BYPASSRLS`. Ship a two-tenant enforcement test (R4) so this can never regress silently.

---

### R2 (CRITICAL) — No mechanism sets/scopes/resets the `app.user_id` GUC

**Evidence.** Repo-wide grep for `set_config` / `SET app` / `current_setting` finds the GUC referenced only inside RLS policy *strings* (`plugins/umbral-rls/src/lib.rs:21-23, 386-405`) and in tests; the only `set_config`/`SET` execution anywhere is migration `search_path` handling (`crates/umbral-core/src/migrate.rs:1699-1816`). The pool builder (`crates/umbral-core/src/db.rs:436-447`) installs **no `after_connect` hook** and no connection-reset callback. So the value `current_setting('app.user_id')` reads is never populated by the framework.

**Why it fails.** Two failure modes depending on how (if) an app author bolts on a GUC-setter:

1. If RLS were actually enforcing (fix R1) and the GUC is never set, `current_setting('app.user_id')::int` raises `unset parameter "app.user_id"` (the policy uses no `missing_ok` form) — **every query errors**.
2. If an author sets it session-scoped (`set_config(..., false)`) on a pooled connection, the value **persists on that connection after the request returns to the pool** and applies to the *next* request — which belongs to a *different user*. That is a cross-tenant read/write with the wrong `user_id`.
3. If set transaction-scoped (`set_config(..., true)`) but on a *different* pooled connection than the one the ORM later checks out (umbral's ambient pool does not pin a connection per request), the GUC is simply absent for the real query — back to mode 1.

The plugin ships the policy DDL and nothing else, delegating a subtle, security-critical, pooling-aware concern entirely to app code, and the doc example (`documentation/docs/v0.0.1/plugins/rls.mdx`, now corrected) demonstrated exactly the unsound cross-connection pattern.

**Attack scenario.** With session-scoped `set_config`: user 1002's request sets `app.user_id=1002` on pooled connection #4 and finishes. User 77's next request is routed to connection #4, whose GUC still says `1002`; user 77's `SELECT`/`UPDATE` now runs as though they were user 1002. Silent, non-deterministic (depends on pool scheduling), and devastating.

**Corrected shape** — the framework should own a request-scoped, connection-pinned setter that resets on release:

```rust
// Framework-provided middleware: pin ONE connection for the whole request,
// set the GUC transaction-locally, run the handler on that same connection,
// and let the transaction (and thus the GUC) end with the request.
async fn rls_scope(State(pool): State<PgPool>, user_id: i64, req, next) -> Response {
    let mut conn = pool.acquire().await?;            // pin
    let mut tx = conn.begin().await?;
    sqlx::query("SELECT set_config('app.user_id', $1, true)")  // is_local = true
        .bind(user_id.to_string())
        .execute(&mut *tx).await?;
    // CRITICAL: the ORM must run every query for this request on `tx`,
    // not via the ambient pool (which would check out a different conn).
    let resp = run_handler_on(&mut tx, req, next).await;
    tx.commit().await?;                              // GUC dies here; conn resets on drop
    resp
}
```

This requires per-request connection pinning that umbral's ambient-pool ORM does not currently expose — so R2 is partly a core gap, not only a plugin gap. Until it exists, RLS should not be advertised as production-ready.

---

### R3 (HIGH) — SQLite silently skips all RLS (divergent behavior)

**Vulnerable code** (`plugins/umbral-rls/src/lib.rs:240-249`):

```rust
umbral::db::DbPool::Sqlite(_) => {
    tracing::warn!(plugin = "umbral-rls",
        "Row-Level Security is Postgres-only; skipping {} table(s) and {} policy/policies", ...);
    Ok(())   // <-- boots fine, enforces nothing
}
```

**Why it fails.** The framework's own guidance is "Postgres-first, SQLite for tests." A team that develops and runs its test suite on SQLite gets **no row isolation at all**, and any test that tries to assert isolation passes vacuously (there is nothing enforcing it). The gap only surfaces in production on Postgres — and there it's masked by R1 anyway. A single `warn` line in boot logs is not a safe signal for a control this important.

**Attack/failure scenario.** CI runs integration tests on SQLite (fast, no container). A test "user B cannot read user A's records" passes because the query returns everything and the test's fixtures happen not to overlap, or because the assertion is written against the (absent) policy. The team gains false confidence that isolation works, then deploys to Postgres where R1 also silently disables it.

**Corrected shape** — fail loud when isolation was requested but can't be provided:

```rust
umbral::db::DbPool::Sqlite(_) => {
    if !self.policies.is_empty() || !self.tables.is_empty() {
        return Err("umbral-rls: RLS policies were declared but the active backend is \
                    SQLite, which cannot enforce them. Use Postgres, or explicitly opt \
                    into the no-op with RlsPlugin::allow_sqlite_noop()."
                   .into());
    }
    Ok(())
}
```

---

### P1 (HIGH) — Authorization is default-allow; a route can be added with no check

**Evidence.** The perm layer (`plugins/umbral-permissions/src/middleware.rs`) is applied per-router via `.layer(permission_required(...))` and is entirely opt-in; the plugin contributes no routes of its own (`lib.rs:130-132`, `routes()` returns `Router::new()`). There is no global default-deny wrapper, no boot-time check that mutating routes are gated, and no type-level "gated by construction" requirement. `has_perm` in a handler is equally optional.

**Why it matters at scale.** With hundreds of routes and 10M users, the probability that *some* mutating endpoint ships without its `permission_required` layer (or with the layer on the wrong router subtree) approaches 1. The framework provides no backstop; the failure is invisible (the route just works, for everyone).

**Attack scenario.** A developer adds `POST /admin/users/{id}/promote` to the admin router but attaches `permission_required("auth.change_user")` to a *sibling* subtree, or forgets it during a refactor. Any authenticated (or, if `login_required` is also missing, anonymous) user can promote themselves to staff. Nothing at build or boot flags the ungated route.

**Corrected direction** — provide a default-deny primitive and/or a boot audit:

```rust
// Gated-by-construction: routes only mountable through a gate.
let admin = GatedRouter::deny_by_default()          // 403 unless a rule matches
    .route("/admin/users/{id}/promote", post(promote), requires("auth.change_user"))
    .into_router();

// Or, minimally, a boot-time warning listing mutating routes with no perm/login layer.
```

Until such a primitive exists, treat "every mutating route must be manually gated" as a documented, load-bearing operational rule and audit it in review.

---

## D. Blind spots (could not verify from the provided artifacts)

1. **Settings-before-`wrap_router` ordering (S3).** Whether `umbral::settings` is in its OnceLock when `SecurityPlugin::wrap_router` runs (`CsrfState::from_config` reads the secret there). If not, a prod app could silently run *plain* double-submit despite `on_ready` later confirming a secret. Requires reading `App::build` order in `umbral-core` (out of scope). Assumed OK per the plugin's own docstrings, but unverified.
2. **umbral-auth session lifecycle (P3).** Whether deactivating a user invalidates their existing sessions, and session TTL/rotation. Determines how long a deactivated non-superuser keeps perm-gated access. `current_session_user_id` internals not audited beyond its signature.
3. **Live Postgres RLS enforcement (R1–R4).** The only PG test is `#[ignore]`'d and asserts existence, not enforcement. I could not run a real PG to confirm the owner-bypass empirically; the finding is based on documented Postgres semantics + the confirmed single-role/single-pool model.
4. **Per-request connection pinning in the ORM (R2).** Whether the ambient-pool ORM can pin one connection/transaction for a whole request (required for any correct GUC-based RLS). Lives in `umbral-core`, not audited here.
5. **Whether the CLI scaffold mounts `SecurityPlugin` by default (S2).** `crates/umbral-cli/src/scaffold.rs` references it, but I did not confirm it is wired into the generated `main.rs` by default vs. only mentioned.
6. **`rest/permissions.mdx` ownership.** That page documents `umbral-rest`'s permission classes (AllowAny/ReadOnly/IsStaff), not these three plugins. Left untouched — belongs to the REST auditor. Flagging for coordination.
7. **Deployment/role configuration.** Whether operators actually run a separate migration vs. runtime DB role in production is infra I cannot see; R1 severity assumes the documented single-`DATABASE_URL` default.

---

## E. Prioritized action plan

**Quick wins (< 1 day)**
- R3: turn the SQLite RLS skip into a hard boot error when policies are declared (opt-in no-op).
- S2: confirm/ensure the CLI scaffold mounts `SecurityPlugin::new()` by default.
- P3: re-check `is_active` for all users in the perm layer (mirror REST `HasPermission`).
- Docs (done this pass): RLS owner-bypass + connection-pinning warnings; permissions superuser `is_active` + i64-layer limitation.

**Short term (< 2 weeks)**
- R1: emit `FORCE ROW LEVEL SECURITY`; document the dedicated non-owner runtime role.
- R4: add a non-ignored two-tenant RLS enforcement test in CI against a containerized Postgres.
- R5: DROP policies no longer declared (or prominently document manual cleanup).
- S1: add a `.production_hardened()` preset (CSP + HSTS on) and/or prod hard-fail option.
- P4: make `permission_required` resolve a stringified user id (PK-agnostic layer).
- S3: resolve the CSRF secret at request time (or assert settings populated before `wrap_router`) + regression test.

**Structural (needs design work)**
- R2: framework-owned, request-scoped, connection-pinned GUC setter with guaranteed reset — depends on per-request connection pinning in the ambient-pool ORM. RLS should not be marketed production-ready until this lands.
- P1: a default-deny / gated-by-construction router primitive, or a boot-time audit of ungated mutating routes.
- P2: an object/row-level permission primitive (per-row grants) so authorization can be instance-aware, not only model-level.

---

## Docs updated

- `documentation/docs/v0.0.1/plugins/rls.mdx`
  - Added a `type="danger"` Callout under "The policy SQL" documenting that the plugin emits `ENABLE` only, never `FORCE`, so in the default single-role setup the app runs as the table owner and **all policies are silently bypassed** (finding R1). Lists the two conditions required for real enforcement (non-owner role + `FORCE`) and a two-tenant verification step. Reason: the doc previously implied RLS "just works" once policies are applied; this describes the plugin's actual (non-forcing) behavior.
  - Added a `type="danger"` Callout under "Setting user context per request" explaining that connection pinning is not automatic, the previously-shown middleware runs `set_config` on a *different* pooled connection than the ORM query, and session-scoped `set_config` leaks the GUC across tenants (finding R2). Tells readers not to ship the naive snippet. Reason: the original example was unsound under pooling; the correction matches the code, which provides no pinning.
- `documentation/docs/v0.0.1/plugins/permissions.mdx`
  - Corrected the superuser-bypass description to state the layer requires `is_superuser = 1` **AND** `is_active = 1` (code: `middleware.rs:270` `u.is_superuser && u.is_active`); the doc previously said only `is_superuser = 1`.
  - Added a `type="warning"` Callout that the `is_active` check applies **only** to the superuser bypass — non-superusers are not re-checked, so a deactivated user with a live session keeps perm-gated access until session invalidation (finding P3).
  - Added a `type="warning"` Callout that `permission_required` / `permission_required_html` are **i64-user-PK-only** (they resolve via `current_session_user_id -> Option<i64>`), despite the plugin's PK-agnostic data model; UUID/String-PK apps must gate in-handler with `has_perm` (finding P4). Reason: the doc oversold the layer as PK-agnostic.
- Not edited: `documentation/docs/v0.0.1/rest/permissions.mdx` — documents `umbral-rest`'s permission classes, not these three plugins; left for the REST auditor (noted in Blind spots #6).

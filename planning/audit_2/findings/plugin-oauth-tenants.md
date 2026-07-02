# Audit — `umbral-oauth` + `umbral-tenants`

Slug: `plugin-oauth-tenants`
Scope: `plugins/umbral-oauth/` and `plugins/umbral-tenants/` only. Read-only audit of source + tests. Postgres + SQLite, axum, ~10M-user multi-tenant target.

---

## A. Executive summary

The OAuth plugin is, on the whole, carefully built: PKCE is always S256 with a CSPRNG verifier (`pkce.rs`), the CSRF `state` is a 122-bit UUIDv4 bound to the session and consumed on the callback (`routes.rs`), provider tokens are stored in `Masked` columns, token-response parse errors are scrubbed so response bodies never leak into logs, and the `?next=` open-redirect defense is a real scheme+host+port+path allowlist. The account-linking policy correctly gates email-based linking behind provider-verified email. The residual OAuth issues are secondary: one endpoint echoes an internal error string to the client, the create-user/create-social-account pair isn't transactional, and the whole account-linking trust model rests on each provider reporting `email_verified` honestly (fine for Google/GitHub, a takeover vector for a sloppy custom provider).

The tenants plugin is where the serious risk lives, and it is an isolation risk — which the plugin itself calls "the whole game." Two findings stand out. **(1)** The tenant is selected entirely from client-controlled input: the `X-Tenant` request header is enabled by default, it *overrides* the subdomain, and there is no API to turn it off. Because auth/sessions are shared (`public`) by default, there is no binding between the authenticated principal and the tenant — any logged-in user can read and write any other tenant's data by sending one header. **(2)** In database-per-tenant mode the router *fails open*: a tenant whose pool isn't registered is silently routed to the **default** database instead of being rejected, so un-onboarded tenants commingle in the shared DB. A third, conditional issue: the schema-mode default of `FallThroughToPublic` means an unknown/invalid tenant is served under the `public` context rather than failing closed.

What I could NOT assess: the core `DatabaseRouter` / `route_context_scope` / `schema_qualified_table` implementation (lives in `umbral-core`, out of scope) — I take its correctness from the PG isolation tests, which pass but are `#[ignore]`d and require a live Postgres; whether `umbral-security` (CSRF) is mounted in any given deployment; and the `Masked`/keyring crypto (out of scope). See Blind spots.

Three most urgent: (1) tenant is client-selectable with no principal binding [TEN-1, HIGH]; (2) db-per-tenant fail-open to default pool [TEN-2, HIGH]; (3) callback CONFLICT response leaks internal DB error text [OAU-1, MEDIUM].

---

## B. Findings table

| # | Severity | Area | Location (file:line) | Finding | Impact | Recommended fix |
|---|----------|------|----------------------|---------|--------|-----------------|
| TEN-1 | HIGH | Security / tenant isolation | `plugins/umbral-tenants/src/lib.rs:243`, `:588-598`, `:701-735` | Tenant is resolved purely from client input: `X-Tenant` header is on by default, **overrides** the subdomain, and cannot be disabled (only re-named). No binding between the authenticated user/session and the resolved tenant; auth+sessions are shared (`public`). | Any authenticated user reads/writes any other tenant's data by adding `X-Tenant: <victim>`. Full cross-tenant breach in the default config. | Bind principal→tenant (membership check), or gate `X-Tenant` behind an explicit opt-in + trusted-network guard; add a way to disable header resolution; document that subdomain-only deployments must not enable the header. |
| TEN-2 | HIGH | Security / isolation | `plugins/umbral-tenants/src/lib.rs:566-574` | Database-per-tenant router *fails open*: a tenant ctx whose pool alias isn't registered falls back to the **default** pool instead of rejecting. | An un-onboarded / de-registered tenant's reads+writes silently hit the shared default DB; multiple such tenants commingle with no isolation. | Fail closed: return an error / unroutable sentinel when a tenant's pool is missing, so the query aborts rather than touching the default DB. |
| OAU-1 | MEDIUM | Security / info leak | `plugins/umbral-oauth/src/routes.rs:270` | Callback returns `(CONFLICT, e.to_string())` for a `resolve_user` error; `OAuthError::Database` renders the raw DB error string into the response body. | Internal DB error text (table/column names, constraint details) leaked to any client that can trigger a link conflict. | Return a fixed public message; log the detail server-side only (as `server_error` already does). |
| OAU-2 | MEDIUM | Security / account takeover (design) | `plugins/umbral-oauth/src/policy.rs:72-84` | Email-based auto-linking trusts `identity.email_verified` from the provider with no allowlist of which providers may assert verification, and links to *any* existing `AuthUser` with that email (incl. local password accounts). | A custom/compromised provider that reports `email_verified=true` for an address it doesn't own takes over the matching umbral account. (Google/GitHub are safe; the risk is third-party providers.) | Keep the verified gate; additionally require the target account's own email to be verified before linking, and/or restrict verified-email auto-link to an operator-allowlisted set of providers. |
| OAU-3 | MEDIUM (contingent) | Security / CSRF | `plugins/umbral-oauth/src/routes.rs:325-344` | `POST /oauth/{provider}/disconnect` has no in-plugin CSRF check; protection depends entirely on `umbral-security` being mounted. | If security middleware is absent, a cross-site POST with the victim's cookie force-disconnects a provider; for an OAuth-only account (password hash `"!"`), disconnecting the last link = account lockout / DoS. | Fine as-is *iff* CSRF middleware is mandatory; otherwise enforce a state-changing guard. Docs already tell users to include `{{ csrf_input }}` — verify `umbral-security` is required in prod. |
| TEN-3 | MEDIUM | Security / fail-closed | `plugins/umbral-tenants/src/lib.rs:159-164`, `:519-534`, `:737-748` | Default `MissingTenant::FallThroughToPublic` + `schema_for_table` returning `None` (public) for a tenant-owned table with no/invalid tenant ctx: an unknown/invalid tenant is served under the `public` context instead of being rejected. | A request with a bogus tenant key silently runs against `public` rather than failing closed; undefined behavior if tenant-owned routes are reachable without a tenant. | For any app that is genuinely multi-tenant, default to (or strongly steer toward) `MissingTenant::NotFound`; document `FallThroughToPublic` as marketing-site-only. |
| OAU-4 | LOW | Data integrity | `plugins/umbral-oauth/src/policy.rs:86-89` | `create_auth_user` then `create_social_account` are two separate writes, not one transaction. | A failure between them leaves an orphan `AuthUser` (unusable password, occupying the verified email) that can block a later legitimate link and can't log in. | Wrap the create-user + create-social-account pair in a transaction so a partial failure rolls back. |
| OAU-5 | LOW | Security / replay | `plugins/umbral-oauth/src/routes.rs:227-244` | State validation (read at :227) and consumption (:244) aren't atomic; two concurrent callbacks can both read the flow before either nulls it. | Marginal — the provider's own single-use `code` rejects the second exchange; no auth bypass. Docs claim "single-use." | Optional: consume-and-compare in one step (atomic read-delete of the flow key). |
| TEN-4 | LOW / info | Security / defense-in-depth | `plugins/umbral-tenants/src/lib.rs:531` | If a tenant ctx key isn't a valid PG identifier, `Schema::new` returns `None` → `schema_for_table` returns `None` → the tenant-owned query silently runs against `public`. | Relies solely on create-time `Schema::new` validation; a bad key that ever reaches ctx degrades to public rather than erroring. | Log + reject (return an unroutable schema / error) when a present tenant key fails validation, instead of silently falling to public. |

No SQL-injection, hardcoded-secret, or token-plaintext-at-rest issues were found in the provided artifacts. Tenant lookups use parameterized ORM `.eq()`; schema names are validated by `Schema::new` and never string-interpolated from request input; `client_secret` is only read from env and sent in the token POST body; `TokenSet`'s `Debug` masks both tokens (`provider.rs:25-34`).

---

## C. Detailed findings (CRITICAL / HIGH)

### TEN-1 — Tenant is client-selectable, with no principal↔tenant binding (HIGH)

`TenantsPlugin::new()` enables the header, and the resolver lets it win over the host:

```rust
// lib.rs:243
tenant_header: Some("X-Tenant".to_string()),

// lib.rs:588-598  (resolve_tenant_key)
// 1. Explicit header wins.
if let Some(name) = tenant_header {
    if let Some(val) = headers.get(name) {
        if let Ok(s) = val.to_str() {
            let s = s.trim();
            if !s.is_empty() { return Some(s.to_string()); }   // <- header beats subdomain
        }
    }
}
```

The middleware then scopes the whole request to that tenant with no check against the caller's identity (`lib.rs:701-735`). Because `DEFAULT_SHARED_APPS` (`lib.rs:169`) keeps `auth` and `sessions` in `public`, a session is global across all tenants — there is no "user X belongs to tenant Y" concept anywhere in the plugin.

There is also **no public method to disable the header**: `tenant_header(impl Into<String>)` only ever sets `Some(..)`, and `new()` seeds `Some("X-Tenant")`. A subdomain-only deployment therefore *still* honors `X-Tenant`, and the header overrides the subdomain the proxy pinned.

**Attack.** SaaS at `*.example.com`, subdomain isolation, shared auth (all defaults). Mallory is a legitimate member of `acme` only. She authenticates at `acme.example.com` (session cookie, stored in shared `public`). She then sends:

```
GET /api/invoices HTTP/1.1
Host: acme.example.com
Cookie: umbral_session=<her valid session>
X-Tenant: globex
```

`resolve_tenant_key` returns `globex` (header wins), the middleware scopes the request to schema `globex`, her shared session is valid, and every tenant-owned query now reads/writes **globex's** schema. Cross-tenant read and write with a single header.

**Fix (illustrative).** Bind the principal to the tenant and reject a mismatch; do not trust a client header for tenant selection unless it comes from a trusted hop. Minimum viable guard in the resolution middleware:

```rust
// after resolving `t` (the Tenant) and before route_context_scope:
if let Some(user_id) = current_session_user_id(req.headers()).await {
    // App-supplied membership check; framework should expose a hook for it.
    if !tenant_membership::is_member(user_id, t.id).await? {
        return (StatusCode::FORBIDDEN, "not a member of this tenant").into_response();
    }
}
let ctx = RouteContext::new().with_tenant(TenantKey::new(t.schema_name));
umbral::db::route_context_scope(ctx, next.run(req)).await
```

and make header-based resolution opt-in / disableable:

```rust
pub fn no_tenant_header(mut self) -> Self { self.tenant_header = None; self }
// and/or: only read X-Tenant when the request arrived from a trusted proxy CIDR.
```

At minimum the framework must ship a first-class principal→tenant binding; today isolation reduces to "trust the client to name its own tenant," which is no isolation against a malicious authenticated user.

---

### TEN-2 — Database-per-tenant router fails open to the default pool (HIGH)

```rust
// lib.rs:566-574  (db_for, TenantStrategy::Database)
match ctx.tenant() {
    Some(key) if umbral::db::pool_alias_registered(key.as_str()) => {
        Alias::new(key.as_str())
    }
    _ => default(),   // <- un-onboarded tenant → DEFAULT database
}
```

A tenant that is active in the registry (so the middleware happily scopes a ctx for it) but whose pool was never registered via `register_tenant_database` — a provisioning gap, a crashed/restarted process that lost its runtime pool registry, a de-registered tenant — routes **both reads and writes to the default pool**. The comment frames this as "avoids a panic," but the safety trade is backwards: it converts a loud failure into a silent isolation break. Two un-onboarded tenants both land in `default`, commingling their rows with no schema/database separation, in the strategy whose entire selling point is *stronger* isolation.

**Attack / failure.** Operator onboards tenant `globex`'s registry row and restarts the app before `register_tenant_database` re-runs (the pool registry is runtime-only, per the `first-write-wins` comment at `:434`). Requests for `globex` now write invoices into the **default** database. If the default DB is also where another tenant fell back, their data is now interleaved and readable across tenants.

**Fix.** Fail closed:

```rust
match ctx.tenant() {
    Some(key) if umbral::db::pool_alias_registered(key.as_str()) => Alias::new(key.as_str()),
    Some(_) => Alias::unroutable(), // or surface an error the terminal turns into 5xx; never `default()`
    None => default(),
}
```

A request for a tenant whose database isn't wired must error, not quietly borrow the shared DB.

---

## D. Blind spots (could NOT verify from the two in-scope dirs)

- **Core routing internals.** `DatabaseRouter`, `RouteContext`, `route_context_scope`, and the SQL builder's `schema_qualified_table` seam live in `umbral-core` (out of scope). I assumed they behave as the PG isolation tests assert. Those tests (`isolation_postgres.rs`, `m2m_*_postgres.rs`, `db_per_tenant_postgres.rs`) are `#[ignore]`d and need a live Postgres, so I did not observe them pass — I read them for intended behavior only.
- **Whether `umbral-security` (CSRF) is mounted** in real deployments. OAU-3's severity hinges on it. The plugin adds no CSRF layer itself.
- **`Masked<T>` crypto + keyring** (out of scope). I confirmed tokens go into `Masked` columns and round-trip in tests, but not the cipher, key management, or rotation.
- **Session cookie flags** (HttpOnly/Secure/SameSite) — owned by `umbral-sessions`, out of scope. Relevant to whether the OAuth cookie session and the `X-Tenant` attack surface are exploitable cross-site.
- **Rate limiting / brute-force** on the OAuth callback and `/oauth/providers` — no limiter is present in these dirs; whether the framework applies one globally is out of scope.
- **`current_session_user_id` semantics** (in `umbral-auth`) used by `oauth_connect`/`oauth_disconnect`. I assumed it authenticates from the session correctly.
- **Reverse-proxy `Host`/`X-Forwarded-Host` handling.** `host_header` reads literal `Host` and defers forwarded-host to "the host-guard layer" (`lib.rs:618-624`); I could not verify that guard exists or is mounted.

---

## E. Prioritized action plan

**Quick wins (< 1 day)**
- OAU-1: replace `(CONFLICT, e.to_string())` with a fixed message; log detail server-side.
- TEN-2: change the db-mode fallback from `default()` to a fail-closed error/unroutable alias.
- TEN-4: log + reject an invalid-but-present tenant key instead of silently routing to public.
- Docs (done): corrected the `?next=` "ignored" claim (it's a 400) in `auth/oauth.mdx`.

**Short term (< 2 weeks)**
- TEN-1 mitigations: add `no_tenant_header()` / trusted-proxy gate for `X-Tenant`; document that subdomain-isolated deployments must disable the header; add loud warnings when both header + subdomain are active.
- TEN-3: flip the recommended/default missing-tenant policy toward `NotFound` for tenant apps.
- OAU-4: wrap create-user + create-social-account in a transaction.
- OAU-2/OAU-3: add a per-provider "may assert verified email" allowlist; confirm `umbral-security` is mandatory in prod and document it as such.

**Structural (needs design)**
- TEN-1 root cause: introduce a first-class principal↔tenant membership model + a `Plugin` hook the resolution middleware calls, so cross-tenant access is denied by the framework rather than left to each app. This is the load-bearing fix; everything else is mitigation.

---

## Docs updated

- `documentation/docs/v0.0.1/auth/oauth.mdx` (SPA-token `<Callout>`, ~line 184): the page claimed that with no `allow_return` configured, `?next=` "is ignored entirely." The code (`routes.rs::validate_next` → `is_allowed_return` over an empty allowlist → `Err(400)`) instead **rejects** any present `?next=` with 400; only a plain login with no `?next=` keeps the session flow. Reworded to match the code (still safe-by-default, but a present `?next=` without an allowlist 400s rather than silently falling back).

No `umbral-tenants` user-facing MDX page exists under `documentation/docs/` (only build artifacts), so there was nothing owned to correct there — the tenant findings above are un-mirrored in docs. `plugins/oauth.mdx` needed no change (overview only). Ambiguity note: the OAuth in-code doc-comments (`lib.rs:135`, `routes.rs:276`) carry the same "ignored"/"single-use" wording as the MDX; those are Rust source and out of edit scope, but they should be reconciled with OAU-5 / the `?next=` behavior in a code PR.

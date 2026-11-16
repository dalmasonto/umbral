# review_4 · umbral-tenants

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 2853 LOC (`plugins/umbral-tenants`) |
| **Reviewer** | review_4 slice agent (static-only) |
| **Verdict** | minor work |
| **Findings** | 0 critical · 0 high · 1 medium · 1 low · 1 info — 3 new, 1 known |

## Summary

- `TenantsPlugin` implements multi-tenancy as a management layer over the core `DatabaseRouter`: schema-per-tenant (default) and database-per-tenant, request resolution by `Host` subdomain or opt-in header, a `Tenant` registry in the shared schema, and a `migrate_schemas` command.
- Isolation posture is genuinely careful: header resolution is OFF by default, a *present* tenant key that doesn't resolve always 404s (regardless of `on_missing`), database-mode routing fails closed to an unrouted sentinel instead of the default DB, the lookup error path 500s without falling through to shared, and membership denial reuses the same 404 so it is not a tenant-enumeration oracle.
- The sharpest issue is operational: `register_tenant_database` documents itself as re-runnable ("first-write-wins… re-onboarding the same alias keeps the original pool") but its first step inserts the unique `Tenant` row, so a second call errors before the pool is registered — and because runtime tenant pools do not survive a process boundary, a restart leaves every existing tenant failing closed with no documented recovery call (TENANTS-1).
- `schema_for_table` collapses an unparseable `schema_name` to `None` (= shared) instead of failing closed the way `db_for` does; only the provisioning helpers validate the name, while the model doc claims validation "at create time" (TENANTS-2).
- Per-request tenant resolution is one indexed SELECT on a unique column with no cache — fine for v1, noted for scale (TENANTS-3).

## Findings

### TENANTS-1 · MEDIUM · correctness/ops · self-verified
**`register_tenant_database` cannot be re-run: step 1 inserts the unique `Tenant` row before step 2 registers the pool, so re-onboarding errors — and after a process restart there is no documented way to restore the runtime pools, leaving every tenant failing closed.**

- **Where**: `plugins/umbral-tenants/src/lib.rs:512-550` (`register_tenant_database`), docstring `507-511`; `451-454` (`create_tenant`'s honest docstring); `crates/umbral-core/src/db.rs:260-279` (`register_tenant_pool`) and `773-782` (`close()` never touches the dynamic registry); `documentation/docs/v0.0.1/plugins/tenancy.mdx:316-325` (no restart guidance).
- **Evidence**:
  ```
  /// … First-write-wins
  /// on the pool registry: re-onboarding the same alias keeps the original
  /// pool.
  ```
  ```rust
  let saved = Tenant::objects().create(row).await?;

  // 2. Register the runtime pool under the alias.
  umbral::db::register_tenant_pool(alias.clone(), pool);
  ```
  `schema_name` is `#[umbral(unique)]` (`lib.rs:89-92`) — and `create_tenant`'s own docstring already admits the same insert "is NOT idempotent … a second call with the same values surfaces a uniqueness error from the ORM" (`452-454`). The docstring here describes only the registry's first-write-wins behaviour, not the path: step 1 throws before step 2 runs.
- **Impact** (traced): the operator follows tenancy.mdx (`plugin.register_tenant_database("Starknet Mainnet", "mainnet", "mainnet.localhost", pool).await?`) in a boot path — correct on first boot; on the second boot (or any restart) step 1 raises a UNIQUE violation, so the pool is never registered. Runtime pools are process-lifetime (`Box::leak`ed registry; a fresh process starts empty), so every request for `mainnet.localhost` routes to `UNROUTED_TENANT_PREFIX:mainnet` (`lib.rs:646-656`) → query error → 500 for every tenant until someone discovers the undocumented one-liner `umbral::db::register_tenant_pool(alias, pool)`. Fail-closed (no isolation break, no data lost), but tenants are down and the documented onboarding API is the thing that fails. MEDIUM: reachable on every restart, loud, workaround exists but is undocumented.
- **Fix**: make the provisioning path idempotent — get-or-create the `Tenant` row and always (re-)register the pool — or split out an `ensure_tenant_database` boot-time call the docs show. Update the docstring and add the restart story to tenancy.mdx either way. (Core-side registry design — never closed, no eviction/replace API — is the core reviewer's finding; the tenant-visible consequence is this one.)

### TENANTS-2 · LOW · security/isolation (static trace)
**An invalid stored `schema_name` collapses to `None` (= shared) in `schema_for_table`, unlike `db_for`'s fail-closed sentinel; only the two provisioning helpers validate the name, while the model doc claims validation at create time.**
- **Where**: `plugins/umbral-tenants/src/lib.rs:596-611` (`schema_for_table`), `85-103` (model doc, `#[umbral(unique)] schema_name`), `462-464` / `520-524` (helper validation), `857` (middleware scopes with the stored value verbatim); sibling `646-656` (`db_for` sentinel).
- **Evidence**:
  ```rust
  match ctx.tenant() {
      Some(key) => Schema::new(key.as_str()),
      None => None,
  }
  ```
  vs the model doc:
  ```
  /// The Postgres schema this tenant's tables live in. Unique, and validated
  /// as a safe PG identifier ([`Schema::new`]) at create time.
  ```
  A `Schema::new` failure is indistinguishable from "no tenant" here, and `Tenant` is an ordinary registered model (admin auto-registers every registered model), so any write path that bypasses the two helpers can persist an arbitrary string — the model itself carries no validation.
- **Impact** (traced): a `Tenant` row created outside the helpers with `schema_name = "Acme Inc"` resolves normally (domain match, active), the middleware scopes the request with the raw stored value, then `Schema::new` fails → `None` → all ORM reads/writes on that request run **unqualified against the shared schema**. If a same-named table exists there the tenant silently reads/writes shared data; otherwise the result is loud 500s ("relation does not exist"). LOW: needs a write path bypassing both helpers, and silence additionally needs a shadowing shared table — but this is a fail-open fallback in the one router that otherwise fails closed, so it is a real defect, not polish. Static trace; no runtime repro.
- **Fix**: fail closed like `db_for` — sentinel/500 on an unparseable stored key (and/or validate + 404 in the middleware before scoping). Also either put validation on the model-write path (a guard the admin honours) or correct the model doc to say validation lives in the helpers.

### TENANTS-3 · INFO · perf
- **Where**: `plugins/umbral-tenants/src/lib.rs:838-841`.
- One indexed `SELECT` on `tenant.domain` (unique) before any work on every resolved request, no cache — standard and fine at v1 scale; note for high-QPS routing that it is one extra query per tenant request.

## Strengths

- Fail-closed resolution on every branch: present-but-unknown key → 404 regardless of `on_missing` (`865`, TEN-3); lookup error → 500 + log, never fall-through (`868-876`); membership denial reuses the same 404 so it is not a tenant-enumeration oracle (`852-856`, `885-900`, audit_2 C3).
- Database mode's sentinel (`646-656`, `UNROUTED_TENANT_PREFIX` at 176) is the right shape: an un-onboarded tenant's queries abort loudly instead of commingling in the shared DB (TEN-2 held).
- Safe-by-default config: `tenant_header` off (`288`, TEN-1 held) with the trusted-proxy warning spelled out (`278-282`); schema mode refuses a non-Postgres default pool at boot (`762-772`).
- Operational surfaces that exist are honest: `migrate_schemas` skips invalid schema names with a domain-naming warn rather than aborting the loop (`981-990`); inverse `tenant_apps` mode logs the full tenant-owned list at boot so a forgotten app is auditable (`781-793`, gaps4 #11).
- ORM-only: `grep sqlx::query plugins/umbral-tenants/src/` is empty; router matrix + resolution + bad-key paths are unit-tested, with five PG-gated suites (`isolation_postgres.rs`, `db_per_tenant_postgres.rs`, `m2m_*`, `data_migration_boundary_postgres.rs`) as the behavioural backstop for the isolation claims.

## Clean areas checked

- **Tenant isolation (schema & database modes)**: routing, sentinel, and shared-app handling read sound — the fail-closed direction holds on every branch traced (unknown key, invalid key, lookup error, membership denial).
- **Session-var reset under pooling** (routed lead): the `before_acquire` hook is core's, applied to all `umbral::db::connect` pools including tenant pools, with a scoped GUC reset — verified held; not a tenants-crate concern.
- **Header-based resolution**: opt-in and documented as trusted-proxy-only (TEN-1 held); CSRF/host posture is not this crate's domain.
- **`unsafe`/panics**: zero `unsafe`; no `unwrap`/`expect` on request or provisioning paths read.

## Known issues already tracked elsewhere (not counted in the totals)

- `audit_2 core-app-config #14` — tenant pools leaked for process lifetime; no eviction/replace API (credential rotation needs a restart) `[open]`
- `audit_2 TEN-1` (header off by default), `TEN-2` (database mode fail-open) `[fixed/verified held]` — sentinel shipped; `TEN-3` (unknown key) `[fixed/verified held]`; `TEN-4` `[fixed/verified held]`
- `audit_2 C3` — membership guard `[fixed/verified held]`
- `review_3/codex 09` — `tenant_apps` inverse mode can share forgotten tables `[fixed/verified held]` — shipped mitigation is the boot-time warn (`781-793`, gaps4 #11); the isolation property remains the operator's declaration.
- `gaps2 #69` — `DatabaseRouter` foundation + both tenant strategies `[fixed/verified held]`

## Out-of-scope notes

- `crates/umbral-core/src/db.rs` — `register_tenant_pool`/`DYNAMIC_POOLS` are never closed by `close()` (`db.rs:773-782`) and have no eviction/replace path; tenant-visible consequence folded into TENANTS-1. Owned by `review_4/umbral-core.md`.
- `plugins/umbral-rls` — the features.md:310 claim that the tenants/RLS pair gives row-level isolation is not delivered as shipped; cross-ref RLS-3 in `review_4/umbral-rls.md`.
- `plugins/umbral-admin` — auto-registers every registered model (incl. `Tenant`) with no model-level validation hooks, which is the reachable write path for TENANTS-2.

# Design — RLS enforcement + tenant isolation (audit_2 C2 & C3)

**Status:** PROPOSAL — awaiting approval before implementation.
**Closes:** C2 (`plugin-authz` R1/R2/R3), C3 (`plugin-oauth-tenants` TEN-1/2/3).
**Author:** loop pass 2, 2026-07-03.

## Problem recap

- **C2 / R1** — `umbral-rls` emits `ENABLE ROW LEVEL SECURITY` but never `FORCE`. The one-`DATABASE_URL` app connects as the table **owner**, whom Postgres exempts from non-forced RLS → every policy is silently bypassed.
- **C2 / R2** — nothing sets the `app.user_id` (or any) GUC the policies reference. Even if enforced, queries would error (unset var) or, with naive session-level `set_config` on a pooled connection, the value would **leak to the next request's user**.
- **C2 / R3** — SQLite silently skips RLS; isolation tests on the stated test backend pass vacuously.
- **C3 / TEN-1** — the tenant is chosen from the client `X-Tenant` header (on by default, overrides subdomain, no user↔tenant binding) → any authenticated user reads/writes any tenant with one header.
- **C3 / TEN-2/3** — unknown tenant **fails open** to the default/`public` DB.

## What already exists (build on, don't reinvent)

- `crates/umbral-core/src/db/route_context.rs` — `RouteContext`, a `tokio::task_local!` set per request via `route_context::scope(ctx, fut)`, already carrying a `TenantKey`. The DB router reads it for read/write pool + schema routing.
- `pool_dispatched()` / per-alias pools; sqlx `PgPoolOptions` supports `after_connect` / `after_release` hooks.

## Proposed design

### Part A — make RLS actually enforce (C2)

**A1. Emit `FORCE ROW LEVEL SECURITY`.** In the RLS DDL, add `ALTER TABLE "<t>" FORCE ROW LEVEL SECURITY` alongside `ENABLE`. FORCE subjects the table **owner** to policies, so the default single-role deployment is protected without requiring a separate role. (A dedicated non-owner runtime role stays a documented defence-in-depth option, not a hard requirement.)

**A2. Set the policy GUC per request, safely, via pool hooks + `RouteContext`.** Extend `RouteContext` with an optional set of session variables (`Vec<(String, String)>`, e.g. `[("app.user_id","42")]`) populated by a framework middleware from the authenticated identity. Then, on the **Postgres pool**:

- `after_acquire(conn)`: read `route_context::current()`; for each `(var, value)` run `SELECT set_config($1, $2, false)` (parameterised — no SQL injection via the value). Session-scoped (`false`) so it covers the ORM's multiple separate queries on that checkout.
- `after_release(conn)`: `RESET ALL` (or reset exactly the vars we set) so the value **cannot leak** to the next checkout, whose request may have a different or empty `RouteContext`. This is the fix for the R2 leak.

This scopes the GUC to a connection checkout that runs inside one request's task (where the task-local `RouteContext` is visible), without forcing every request into a single pinned transaction. The overhead is two tiny round-trips per checkout on Postgres only.

**A3. RLS config carries the var extractor.** `RlsPlugin::new().session_var("app.user_id", |identity| identity.user_id_string())` — the plugin registers which GUC name(s) to set and how to derive each value from the request identity. The middleware writes them into `RouteContext` before handlers run.

**A4. SQLite fails closed.** SQLite has no RLS. If `RlsPlugin` is registered and the active backend is SQLite **outside `#[cfg(test)]`**, the boot system-check errors (not warns): "RLS is Postgres-only; a SQLite backend provides no row isolation." Tests may still run (they assert Postgres behaviour under `#[ignore]`/containerised PG). This removes the silent-divergence footgun (R3).

**A5. Enforcement test.** Add a non-vacuous two-tenant test (containerised/real PG, un-`#[ignore]`d in CI): user A and user B insert rows; with `app.user_id` set to A, a `SELECT *` returns only A's rows; a cross-tenant `UPDATE` affects zero rows. Asserts *enforcement*, not policy existence (the R4 gap).

### Part B — bind the tenant server-side (C3)

**B1. Resolve tenant from a trusted source, not a raw header.** Tenant resolution order becomes: (1) the authenticated session/user's tenant membership (authoritative); (2) validated subdomain **iff** it matches the user's allowed tenant(s); never a bare client header by default.

**B2. `X-Tenant` off by default.** The header is honored only when explicitly enabled *and* the caller is a trusted internal principal (e.g. a service token), and even then validated against membership. Default deployments ignore it.

**B3. Fail closed.** An unknown/unresolvable tenant, or a mismatch between claimed and authorised tenant, is a hard error (403/404) — never a fall-through to the `public`/default DB (fixes TEN-2/3).

**B4. Consistency with A.** The resolved tenant flows into the same `RouteContext` that (A2) reads, so RLS `app.tenant_id` and DB-per-tenant routing share one authoritative value.

## Alternatives considered (and why not)

- **Per-request pinned transaction** (wrap every request in one tx on one connection, `set_config(...,true)` LOCAL): correct but forces the ORM's ambient dispatch onto a task-local transaction for *all* queries — a large, invasive change to the execution model and to every terminal. The pool-hook approach (A2) achieves per-request scoping with far less blast radius.
- **`after_connect` session GUC** (set once when a connection is first opened): wrong — connections are reused across requests/users, so the value leaks. Must be per-*acquire* + reset on release.
- **Non-owner role as the only fix:** shifts the burden to every operator's DB provisioning and doesn't help the default single-`DATABASE_URL` deployment; `FORCE` fixes the common case in-framework.

## Rollout / safety

- All of Part A is **Postgres-only** and gated behind `RlsPlugin` being registered — zero effect on apps that don't use RLS.
- `FORCE` + GUC land together (A1 without A2 would make RLS-enabled queries error). Ship as one change.
- SQLite fail-closed (A4) is a new boot error; documented as a breaking change for anyone who (incorrectly) ran RLS on SQLite.
- Part B changes tenant-resolution defaults — a breaking change for apps relying on the `X-Tenant` header; documented with a migration note and an explicit opt-in for the old behaviour behind trusted-caller validation.

## Open decisions for you

1. **GUC naming** — standardise on `app.user_id` + `app.tenant_id`, or leave the var name fully author-defined via `.session_var(...)`? (Proposal: author-defined, with those two as documented conventions.)
2. **SQLite + RLS** — hard boot error (proposed) vs. loud warn-and-continue? A hard error is safer but breaks any existing SQLite-multitenant dev setup.
3. **`X-Tenant` migration** — is anyone currently relying on the header in a real deployment? If yes, we need the trusted-caller opt-in in the first cut; if no, we can drop header-trust entirely and simplify.

Once you approve (and answer 1–3), I'll implement Part A and Part B behind tests, per the same TDD + status-marking loop.

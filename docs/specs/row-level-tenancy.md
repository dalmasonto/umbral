# Row-level tenant scoping (kikosi #3 / gaps3 #38 item 3)

Status: **NOT BUILT — deliberately. Phase 1 landed; Phases 2–3 abandoned.** Author: 2026-07-12.

## Why this was not built (read this first)

The consumer that motivated it (Kikosi / web3clubs_fc) does **not** have a tenancy problem. Its real flow: *"I'm in web3clubs FC, and later I join another club — same account, and I can see my clubs."* One user, many clubs, joined like groups.

That is not multi-tenancy. Tenancy means **isolated customers who must never see each other's data**. Both tenancy tools actively *break* the flow above:

- **Schema-per-tenant** (already shipped, `umbral-tenants`) would put each club's rows in a separate Postgres schema. A single account would have to exist in each, and "show me my clubs" becomes a cross-schema union.
- **Row-level auto-scoping** (this spec) pins every query to *one* tenant — which is precisely what makes "I belong to two clubs" impossible.

The correct shape is ordinary modelling: a `Club` model, a `Membership` model (`user` FK + `club` FK), plain foreign keys. **No framework feature required.**

The risk in that design is not a forgotten `WHERE club_id` — it is **authorization**: an endpoint that fails to check "is the caller a member of *this* club". That is what the framework should make hard to get wrong, and `umbral-rest`'s `ResourceConfig::scope` is the right hook for it — except it could not express the membership case. **That** became the actual work; see `docs/specs/membership-scoping.md`.

Build row-level scoping only if a real driver appears: **thousands** of tenants (Postgres schema catalogs get unhappy past ~1k), or a need for cross-tenant analytics in one query. Neither is true today. Everything below is preserved as the record of what it would take.

---

## Original design (superseded)

## What this is, and what it is *not*

`plugins/umbral-tenants/` already ships multi-tenancy — but as **schema-per-tenant** and **database-per-tenant** (`TenantStrategy::{Schema, Database}`). Isolation there happens at the *table reference*: `TenantRouter::schema_for_table` qualifies tenant-owned tables into the tenant's Postgres schema, and **every** ORM builder already routes through `db::router::schema_qualified_table` (`crates/umbral-core/src/db/router.rs:200`). That is why it needed no ORM surgery.

This spec is the **other** variant, recorded as unbuilt at `planning/features.md:286`: **row-level scoping** — a shared schema, a `tenant_id` column, and an *automatically injected* `WHERE tenant_id = <current>`. It is what a single-club → multi-club SaaS wants (you do not want a Postgres schema per club), and it is the live consumer ask in `planning/gaps3.md:116`: *"tenant middleware + **automatic** ORM query scoping so a forgotten `WHERE tenant_id` can't leak."*

The feature's entire reason to exist is that **forgetting the filter must be impossible**, not merely discouraged. Every design call below follows from that.

## Verified ground truth

Facts confirmed by reading the code (not assumed):

1. **Per-request ambient state already exists.** `crates/umbral-core/src/db/route_context.rs:71` — `tokio::task_local! { static ROUTE_CONTEXT: Arc<RouteContext> }`, with `current()` (:79), `scope()` (:87), a `tenant: Option<TenantKey>` field and a typed `extensions` store (:24-33). The ORM already reads this. **A tenant value can reach the QuerySet with zero threading.**
2. **A model-flag → implicit-`WHERE` precedent already exists.** `#[umbral(soft_delete)]` → `Model::SOFT_DELETE` → snapshotted onto the QuerySet in `Manager::queryset()` (`orm/queryset/mod.rs:3445`) → injected in `build_query_for` (:537). Row-level tenancy is structurally identical.
3. **`ModelMeta` already carries `soft_delete`** (read as `self.meta.soft_delete`, `orm/dynamic.rs:222`). So the runtime/dyn path can see model flags — the tenancy flag goes in the same place.
4. **`DynQuerySet` has ONE clause-composition seam, not twenty.** 14 sites call `effective_where_clauses()` (`dynamic.rs:220`) or `live_where_clauses()` (:233), which is where the implicit soft-delete predicate is composed. Exactly **one** site clones `self.where_clauses` raw — the restore path (:786) — and it is deliberate. The *application* loop (`for cond in … { stmt.cond_where(…) }`) is duplicated, but the *predicate list* is centralized. This is much safer than it first looks.
5. **The typed path has four builders, not one.** `build_query_for` (:526, SELECT), `build_update_for` (:3312), `build_delete_for` (:3202), `soft_delete_update` (:3218). `build_update_for` **does** apply the soft-delete filter (:3343); `build_delete_for` intentionally does not (a hard delete may target trashed rows). **A tenant predicate must go into all four** — a hard delete must still be tenant-scoped.
6. **Terminals return `Result<_, sqlx::Error>`** (`fetch` :1589, `first` :1783, `count` :1953, `delete` :2907). `build_query_for` has **26 call sites** (21 in `mod.rs`, 5 in `tx.rs`).

## Decisions

### D1 — Missing tenant context fails CLOSED (approved)

Querying a `tenant_scoped` model with no tenant in the `RouteContext` returns `Err`. Not "all rows" (a forgotten scope becomes a leak, in exactly the place nobody looks), and not "no rows" (silent — a cron job that should process rows quietly does nothing, and the bug presents as "the queue is empty").

Task-locals **do not cross `tokio::spawn`** — pinned by a test at `route_context.rs:113`. So background jobs, CLI commands and cron get *no* tenant by construction. Fail-closed turns each of those into a loud, one-line fix (enter a tenant, or opt out on purpose) instead of a silent leak or a silent no-op.

Escape hatch: an explicit `.across_tenants()`, mirroring the existing `.with_deleted()` shape.

### D2 — Enforcement is compiler-enforced, not convention

`build_query_for` changes to return `Result<SelectStatement, sqlx::Error>`. This is the point of the design: with 26 call sites, a *convention* ("remember to check the tenant") would be forgotten. Returning `Result` means **the compiler refuses to let a terminal skip the guard**. Each call site is already inside a `Result`-returning fn, so the change is a mechanical `?`.

### D3 — Extend `TenantsPlugin`, don't build a second plugin

Add `TenantStrategy::Row { column }` alongside `Schema`/`Database`. This reuses machinery that has already been security-reviewed: the resolution middleware, the `TenantMembership` server-side guard (fails closed 404, no enumeration oracle — audit_2 C3), `X-Tenant` off by default (TEN-1), and present-but-unknown-key fails closed (TEN-3). Re-deriving those in a new plugin would re-open settled security ground.

### D4 — The flag lives on `ModelMeta`, read by both paths

`#[umbral(tenant_scoped)]` (defaulting the column to `tenant_id`) or `#[umbral(tenant_scoped = "org_id")]`, threaded `UmbralStructAttr → Model::TENANT_FIELD → ModelMeta.tenant_field`. One source of truth for the typed path *and* the dyn path. Follow the `privileged` threading (`migrate.rs:833`), which is the established shape for a non-schema, security-only flag.

## The surfaces that must be scoped

A leak anywhere is a leak. In descending order of "will actually bite":

| Surface | Seam | Note |
|---|---|---|
| Typed SELECT | `build_query_for:526` | |
| Typed UPDATE | `build_update_for:3312` | |
| Typed DELETE | `build_delete_for:3202` | hard delete must still be scoped |
| Typed soft-delete | `soft_delete_update:3218` | |
| **Dyn (admin + REST)** | `effective_where_clauses:220`, `live_where_clauses:233`, restore `:786` | **admin and REST run entirely on this** — typed-only scoping is a silent, complete bypass |
| INSERT | typed `create`/`bulk_create`; dyn `build_insert_form_query:1053`, `build_insert_plan:3374` | **stamp** the tenant id; reject an explicit mismatching value |
| `select_related` / `prefetch_related` | `orm/queryset/hydration.rs` | issues its own `Query::select()` — bypasses everything |
| M2M / junction | `orm/m2m.rs` | ditto |
| **Uniqueness validation** | `orm/validation.rs` | a cross-tenant unique check is an **existence oracle** — "that username is taken" leaks another tenant's data |

## Phases

- **Phase 1 (this change).** Extract the shared implicit-predicate seams so a write path cannot silently diverge from the read path: a `soft_delete_predicates()` helper on the typed QuerySet used by both builders that need it, and route the dyn restore path through the shared clause builder. **No behaviour change**; the whole suite must stay green. This is the prerequisite that makes Phase 2 a small, auditable diff instead of a sprawling one.
- **Phase 2.** The feature: D4 flag threading, tenant value in `RouteContext`, `TenantStrategy::Row`, injection at both seams, INSERT stamping, D2 `Result` plumbing, `.across_tenants()`.
- **Phase 3.** The satellite builders (hydration / m2m / **validation**), RLS as defense-in-depth wired from the same context (`AuthPlugin::with_db_session_var` + `RlsPlugin` already exist), and the background-job story.

## The test that decides whether this works

Not unit tests of the predicate. **Two real tenants, rows in both, and every surface asserted blind to the other**: typed fetch/first/count/update/delete, dyn (admin list/detail/update/delete), REST list/retrieve, `select_related`, `prefetch_related`, M2M traversal, and the uniqueness check. Plus: a `tenant_scoped` model queried with no context **errors**, and `.across_tenants()` sees both. Anything less and the feature is decorative.

## Known trap

`soft_delete` had to be **duplicated** into `DynQuerySet` rather than shared. That is the warning, not the template. If the tenancy flag ends up implemented twice, the two copies will drift, and the drift will be a data leak.

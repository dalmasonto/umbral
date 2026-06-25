# umbral framework review — security & feature audit

Date: 2026-06-10. Method: four parallel read-only audits (ORM/SQL, auth/session/authz, HTTP-facing plugins, broken/missing features) across `crates/` and `plugins/`, cross-checked against the existing `bugs/gaps.md`, `bugs/gaps2.md`, `bugs/REAL-GAPS.md`, `bugs/features.md` so only **new** findings are reported here. Every high/critical claim was independently re-verified against the source before being written down (see the `Verified` line on those entries).

This is a point-in-time review, not a tracker. Each finding has `file:line`, evidence, an attack/impact path, and a suggested fix. Triage and convert the ones you accept into `gaps2.md` entries.

## Files in this folder

| File | What it covers |
|---|---|
| [`security-web-surface.md`](security-web-surface.md) | REST/admin/playground/media/openapi — the internet-facing attack surface. Holds the only **critical**. |
| [`security-auth-session.md`](security-auth-session.md) | auth, sessions, permissions, security headers, RLS. |
| [`security-orm-sql.md`](security-orm-sql.md) | SQL-injection sweep of the ORM, migrations, inspectdb, backup. **Clean on injection** + one LIKE-escaping bug. |
| [`performance.md`](performance.md) | ORM runtime performance: unbounded REST default, missing FK auto-index, `bulk_create` validation N+1, pool config, per-row materialization. |
| [`query-api-sufficiency.md`](query-api-sufficiency.md) | Does the query builder cover what apps need without raw SQL? Coverage table + prioritized gaps (`select_for_update`, reverse-relation filtering, EXISTS, `Case`/`When`). |
| [`broken-features.md`](broken-features.md) | Code that can't work as written: task double-claim/loss, signal-mutex poisoning, error-swallowing, panics on reachable paths. |
| [`missing-features.md`](missing-features.md) | Django-parity gaps grounded in an in-tree need (e.g. `select_for_update`). |

## Severity summary

### Critical
- **WEB-1** — `RestPlugin::default()` exposes anonymous full CRUD (create/update/delete) on every non-blocklisted model. The documented happy path ships an open write API.

### High
- **WEB-2** — Mass assignment: REST/dynamic writes strip only `noform`, not `noedit` (and REST `hide()` is response-only). `PATCH {"is_superuser": true}` lands. Chains with WEB-1 into unauthenticated privilege escalation.
- **WEB-3** — Reflected XSS in the admin filter dialog: `tojson` emits `from_safe_string` and a `</script>` in a `filter_<field>=` query param breaks out of an inline `<script>`.
- **WEB-4** — Stored XSS via umbral-media: no extension/MIME allow-list, user-uploaded `.html`/`.svg` served inline on the app origin.
- **AUTH-1** — Flagship `examples/shop` never registers `SecurityPlugin`, so it runs with **no CSRF middleware and no security headers**. Security is opt-in and the reference app forgets it.
- **AUTH-2** — Admin write handlers don't self-enforce CSRF (only login does); compounds AUTH-1.
- **AUTH-3** — `umbral-rls` ships row-level-security policies but nothing ever sets `app.user_id`. RLS is either broken-at-runtime or silently non-isolating.
- **BROKEN-1** — Tasks: two Postgres workers can claim and run the same task (no `FOR UPDATE SKIP LOCKED`; UPDATE filters on id only). Code comment claims the opposite.
- **BROKEN-2** — Tasks: worker crash mid-task strands the row in `running` forever (no visibility-timeout/reclaim). At-most-once, not the advertised with-retries durability.
- **BROKEN-3** — Core signals: one panicking sync handler poisons the registry mutex and turns **every** subsequent ORM write into a 500.
- **PERF-1** — `RestPlugin::default()` applies no LIMIT (default `NoPagination` → `limit: u64::MAX`); `GET /api/<table>/` loads the whole table into RAM. DoS surface; compounds WEB-1.
- **PERF-2** — FK columns are never auto-indexed (only explicit `#[umbral(index)]` emits `CREATE INDEX`). Every join/`?include=`/`WHERE fk_id=?` seq-scans. Django auto-indexes every FK; umbral doesn't.
- **PERF-3** — `bulk_create` runs one FK-existence `COUNT` per FK per row before the single INSERT — `bulk_create(1000)` with 2 FKs ≈ 2000 round-trips, defeating the bulk path.

### Medium
- **AUTH-4** — `is_staff`/`is_superuser` editable through the generic admin form (no superuser-only field guard) → privilege escalation for a "user manager" role.
- **WEB-5** — Raw sqlx error text returned in REST 500 bodies in all environments (schema disclosure).
- **WEB-6** — API playground mounted unauthenticated, runs requests with the visitor's ambient cookies.
- **WEB-7** — Admin bulk-actions and FK-autocomplete skip `permcheck` (only `require_staff`).
- **BROKEN-4** — `run_worker` graceful shutdown calls `std::process::exit(0)` from library code (kills the host process in single-binary deploys); doc says it panics.
- **BROKEN-5** — umbral-signals typed handlers silently never fire when the payload doesn't deserialize (no log).
- **BROKEN-6** — umbral-email `send()` panics on the console backend when settings aren't initialised (defeats its own no-App workaround).
- **BROKEN-7** — `cache_page` body-collection failure fabricates an empty 200 with stale `Content-Length`.
- **BROKEN-8** — `Form<T>` extractor ignores `Content-Type` and turns body-parse failures into bogus "field required" errors instead of 415.
- **BROKEN-9** — `CachePlugin` registered via `.plugin(...)` is inert; only the static `init()` wires the ambient cache (doc claims `App::build()` does).
- **MISS-1** — No `select_for_update()`/`skip_locked()` in the ORM (the gap BROKEN-1/PERF-6 worked around incorrectly). Both review rounds converged on this independently — it's the top ORM gap.
- **PERF-4** — Admin M2M edit form loads the entire target table as `<option>`s (no `.limit()`). **PERF-5** — Postgres pool has no size/timeout config (bare `PgPool::connect`, capped at 10 conns).
- **QUERY-2/3** — No reverse-relation filtering (`filter(comments__author=…)`) and no EXISTS/correlated subqueries; both force hand-rolled SQL for common shapes.

### Low
- **ORM-1** — LIKE wildcards (`%`, `_`) unescaped in `contains`/`startswith`/`search`; `?title__contains=%25` matches everything (data over-disclosure / mild DoS, **not** injection).
- **AUTH-5** — CSRF cookie missing `Secure`. **AUTH-6** — admin login CSRF compare not constant-time. **AUTH-7** — CSRF is double-submit-cookie, not session-bound. **AUTH-8** — bearer tokens never expire; password change doesn't invalidate sessions/tokens.
- **BROKEN-10** — `MemoryBackend` never evicts expired entries except on read (unbounded growth). **BROKEN-11** — embedded static service answers every HTTP method with the body (no 405, no ETag). **BROKEN-12** — `CacheBackend` doc promises logging on swallowed errors; nothing logs. **BROKEN-13** — stale `orm/write.rs` comment references a removed REST sqlite-only path. **BROKEN-14** — `#[derive(Form)]` rejection message omits half the accepted attributes.
- **PERF-7** — `exists()` materializes a full row instead of `SELECT 1`. **PERF-8** — permissions helpers fetch full rows to read one FK column.
- **QUERY low cluster** — `Case`/`When`+`Coalesce`, `HAVING`, per-row annotation, F-expression breadth (`gt_f`/`lt_f`), `order_by` related field + `NULLS FIRST/LAST` absent; workarounds exist but several force raw SQL. Full coverage table in [`query-api-sufficiency.md`](query-api-sufficiency.md).

## What's done well (so the review is fair)
- **No SQL injection** found anywhere — sea-query `Alias::new` for identifiers, bound parameters for values, validated/dropped unknown columns, escaped DDL, hostile-DB-safe inspectdb.
- **Password hashing** is correct (argon2 0.5, per-password OS-CSPRNG salt, constant-time verify, no user enumeration).
- **Session fixation defended** (id rotated on login), session IDs/bearer tokens hashed at rest, strong entropy throughout, correct session-cookie flags.
- **Template autoescaping on by default**; the new `img` filter HTML-escapes every attribute.
- **Permissions are default-closed** (REST and admin both fail-closed on error) — note the per-route gaps in WEB-7 are the exception, not the rule.
- **Email really sends** over STARTTLS with cert verification (no TLS bypass). **CLI** has no stubbed commands.
- The **tasks plugin uses the ORM end-to-end** (zero raw sqlx) — exemplary against the plugin contract.
- **ORM batching is genuinely good** — `select_related`/`prefetch_related`/M2M hydrate in `1 + len(relations)` queries (no N+1), `count()` does COUNT(*) pushdown, `bulk_create`/`bulk_update` are single statements, the pool is created once and shared. The perf issues are *defaults and missing indexes*, not the query engine.
- **The query API is more complete than CLAUDE.md's "80%" list claims** — Q-objects, F-expressions, date-part lookups, group-by aggregation, `IN (subquery)`, union/intersect, `get_or_create`/`upsert`/`bulk_update`, soft-delete all already present. The gaps are a focused set, not a broad hole.

## The one structural theme
Three of the worst findings (WEB-1, AUTH-1, AUTH-3) share a root cause: **security is opt-in and the safe wiring is easy to forget.** The framework's own "make the easy path the safe path" principle (CLAUDE.md) is violated at exactly the points where a consumer copies the happy path. The highest-leverage fix is not 20 patches — it's making `App::builder()` secure-by-default (auto-mount security headers + CSRF, default REST to read-only/authenticated, boot-warn when RLS policies exist without a context hook).

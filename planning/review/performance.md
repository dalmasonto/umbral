# ORM runtime performance

> **Sweep status — 2026-06-14**
> - **Fixed (correctness portion):** PERF-6 — `claim_one` now uses a conditional `UPDATE ... WHERE status='pending'` so two Postgres workers can't both claim a row (`98ef6e9`). The row-level-lock *optimization* (MISS-1) is still open.
> - **Also fixed:** PERF-1 (REST list queries now clamp to a hard 1000-row ceiling even under NoPagination, `06f19df`), PERF-5 (Postgres pool uses PgPoolOptions with a bounded acquire_timeout + configurable max_connections via settings, `65c39d0`).
> - **Closed by design:** PERF-2 (umbra indexes explicitly — add `#[umbra(index)]`), PERF-7/PERF-8 (documented minor; projection via `only()`/`values()` exists).
> - **Deferred (genuine larger work):** PERF-3 (`bulk_create` per-row FK COUNT — batch into one query), PERF-4 (admin M2M form loads the whole target table).

Scope: `crates/umbra-core/src/orm/` (queryset, hydration, dynamic, m2m, aggregate, write, validation), the migrate index generator, the DB pool, and the call sites in `umbra-rest`/`umbra-admin`/`umbra-permissions`/`umbra-tasks`. Cross-checked against `bugs/gaps.md`, `gaps2.md`, `REAL-GAPS.md`, `features.md` — all findings below are new.

**Headline:** the ORM is genuinely good on the classic N+1 axes — `select_related`/`prefetch_related`/M2M hydration are batched (`1 + len(relations)` queries regardless of row count), `count()` does COUNT(*) pushdown, `bulk_create`/`bulk_update` are single statements, the pool is created once and shared ambiently, and REST permissions load once per request. The real problems are in **default configuration (unbounded loads)**, **missing FK indexes**, and a **hidden per-row N+1 inside `bulk_create` validation**.

---

## PERF-1 — REST list endpoint defaults to an unbounded full-table load
> **✅ FIXED** (`06f19df`) — list queries clamp to a 1000-row ceiling even under NoPagination.
**Severity: high** · **Verified** (`PageRequest::all()` sets `limit: u64::MAX` at `pagination.rs:58`; `lib.rs:1463` applies LIMIT only `&& req.limit != u64::MAX`)

- **File:** `plugins/umbra-rest/src/lib.rs:242` (default `NoPagination`), `plugins/umbra-rest/src/pagination.rs:56-59` (`PageRequest::all()` → `limit: u64::MAX`), `plugins/umbra-rest/src/lib.rs:1462-1466` (limit skipped when `u64::MAX`)
- **Evidence:** `RestPlugin::default()` sets `pagination: Arc::new(NoPagination)`. `NoPagination::extract_request` returns `PageRequest::all()` whose `limit == u64::MAX`. In `fetch_rows`: `if let Some(req) = page && req.limit != u64::MAX { qs = qs.limit(...) }` — so for the default config **no LIMIT is ever applied**.
- **Impact:** Out of the box, `GET /api/<table>/` issues `SELECT * FROM table` with no bound and buffers the entire table into a `Vec<Map>` then JSON. A 1M-row table = 1M rows into RAM per request. This is a DoS surface on the **default** config, and it compounds [WEB-1](security-web-surface.md) (anonymous access to that same endpoint).
- **Fix:** Default `RestPlugin` to a bounded paginator (`PageNumberPagination::new(50)` or `LimitOffsetPagination::default()`), or make `NoPagination` still apply a hard safety ceiling. At minimum, warn at boot when a resource is mounted with `NoPagination`.

## PERF-2 — Foreign-key columns are never auto-indexed
> **🚫 CLOSED — by design.** umbra indexes explicitly, not magically: add `#[umbra(index)]` to any FK (or column) you want indexed — the macro already supports it (`umbra-macros/src/lib.rs:390`) and the migration engine emits the `CREATE INDEX`. Auto-indexing *every* FK is a footgun (write-amplification + dead indexes nobody asked for); Django does it, we deliberately don't. Documented behaviour, not a gap.
**Severity: high** · **Verified** (`migrate.rs:2743` emits `CREATE INDEX` only `if col.index && !col.primary_key && !col.unique`; the derive macro never sets `index=true` for FK fields)

- **File:** `crates/umbra-macros/src/lib.rs:1162` (`index` true only with explicit `#[umbra(index)]`), `crates/umbra-core/src/migrate.rs:2742-2745, 2808-2810` (the only `CREATE INDEX` trigger)
- **Evidence:** The migrate engine emits a `CREATE INDEX` solely for `col.index`. The derive macro has no FK-aware branch that sets `index = true`. So an FK column is unindexed unless the author hand-annotates it.
- **Impact:** Every FK column is unindexed by default. The `IN(...)` batches that power `select_related`/`prefetch_related`/reverse-FK (`hydration.rs:397/414/680`), every `?include=` expansion, every admin M2M filter, and every `WHERE fk_id = ?` hit unindexed columns → sequential scans that grow with table size. **Django auto-indexes every ForeignKey; umbra silently doesn't.** This is the single highest-leverage fix for read latency at scale — and it quietly undermines the batched-hydration work the ORM otherwise does well.
- **Fix:** Default `FieldSpec.index = true` for FK fields in the derive macro (with an opt-out), and emit `CREATE INDEX` for FK columns in `CreateTable`/`AddColumn`. Note this is a migration-engine change — existing tables need a generated migration to add the indexes (do not hand-edit; run `makemigrations`).

## PERF-3 — `bulk_create` runs N×F per-row FK-existence COUNT queries before the single INSERT
> **⏳ DEFERRED** — batch the per-row FK COUNT into one query (moderate).
**Severity: high** · **Verified** (`queryset/mod.rs:2869` loops `validate_on_typed_create` per row; that path runs a `.count()` per FK)

- **File:** `crates/umbra-core/src/orm/queryset/mod.rs:2869-2872` (per-instance validation loop) → `orm/validation.rs:80-87` → `validate_fk_references` (`validation.rs:411-435`) → `check_fk_row_exists` (`validation.rs:487-503`, one `DynQuerySet::count()`)
- **Evidence:** `for map in &maps { let errs = validate_on_typed_create(&meta, map).await; }`. `validate_on_typed_create` → `validate_fk_references` → `check_fk_row_exists` (one COUNT) for **each non-null FK on each row**. M2M validation does the same per array element. `check_fk_row_exists` also re-walks the registry via `model_meta_by_table` (`validation.rs:473-482`, uncached nested loop).
- **Impact:** `bulk_create(1000 rows)` on a model with 2 FKs = **~2000 COUNT round-trips** plus one multi-row INSERT. The entire point of `bulk_create` (one statement) is defeated by the validation fan-out.
- **Fix:** Batch FK validation — collect distinct `(target_table, ids)` across all rows, issue one `SELECT pk FROM target WHERE pk IN (...)` per FK target, diff. Or lean on the DB FK constraint (already on for SQLite per `db.rs:295`) and classify the error. Also switch `check_fk_row_exists` from `count()` to `exists()` (see PERF-7).

## PERF-4 — Admin M2M edit form loads the entire target table as candidates
> **⏳ DEFERRED** — admin M2M form candidate loading (UI-side).
**Severity: medium**

- **File:** `plugins/umbra-admin/src/view.rs:322-329` (`form_m2m_fields_for`); comment acknowledging it at `:256-258`
- **Evidence:** `DynQuerySet::for_meta(&target).select_cols(&select_cols).fetch_as_strings().await` — no `.limit()`. The comment says "v1 loads every row."
- **Impact:** Opening an admin create/edit form for a model with an M2M field renders one `<option>`/checkbox per row in the *entire* target table. A `tags`/`products` M2M on a large catalog loads and renders the whole table on every form view. (Column projection mitigates width, not row count.)
- **Fix:** Cap candidates and switch to the HTMX chip-picker — the `fk_picker` handler already does paged search correctly; reuse it for M2M.

## PERF-5 — Postgres pool has no size/timeout configuration
> **✅ FIXED** (`65c39d0`) — PgPoolOptions with acquire_timeout + configurable max_connections.
**Severity: medium**

- **File:** `crates/umbra-core/src/db.rs:258` — `PgPool::connect(url)`
- **Evidence:** The Postgres branch uses bare `PgPool::connect` (sqlx defaults: `max_connections = 10`, no `acquire_timeout`, `min_connections`, or `max_lifetime`). The SQLite branch uses `SqlitePoolOptions` but also never sets `max_connections`. No user-facing knob to tune pool size.
- **Impact:** A "Postgres-first" framework caps concurrency at 10 connections with no way to raise it and no acquire timeout (a saturated pool blocks request tasks indefinitely instead of failing fast). The pool is correctly created once at `App::build()` and shared — no per-request creation (good).
- **Fix:** Use `PgPoolOptions` with configurable `max_connections`/`acquire_timeout`/`max_lifetime`, surfaced through a settings knob.

## PERF-6 — Task `claim_one` has no row-level lock (perf + correctness)
> **✅ FIXED** (`98ef6e9`) — conditional claim guard (correctness); FOR UPDATE SKIP LOCKED still a future optimization.
**Severity: medium** — same root issue as [BROKEN-1](broken-features.md)/[MISS-1](query-api-sufficiency.md), noted here for the throughput angle.

- **File:** `plugins/umbra-tasks/src/lib.rs:377-417`
- **Evidence:** `SELECT ... LIMIT 1` then `UPDATE ... SET status=running` in a transaction, with **no `FOR UPDATE SKIP LOCKED`**. Beyond the double-claim correctness bug, multiple workers contend on the same hot row.
- **Fix:** Add `FOR UPDATE SKIP LOCKED` to the candidate SELECT on Postgres — which requires the missing ORM `.select_for_update()` terminal (see [MISS-1](query-api-sufficiency.md)).

## Lower-severity
- **PERF-7 (low)** 🚫 CLOSED — by design (documented M1 simplification; negligible, projection via only()/values() exists) — `exists()` materializes a full row instead of `SELECT 1` (`crates/umbra-core/src/orm/queryset/mod.rs:1352-1360`): `self.limit(1).fetch()` hydrates `T` via FromRow just to discard it. On wide tables (BLOB/text) this pays full column materialization for a boolean. Called on every `has_perm_scoped` direct-grant check (`permissions/perm.rs:133`). Reshape to `SELECT 1 ... LIMIT 1` with a scalar row type, mirroring `count()`. (Documented as a known M1 simplification.)
- **PERF-8 (low)** 🚫 CLOSED — by design (query count already optimal; use only()/values() if it ever matters) — `user_perms`/membership helpers fetch full rows to read one FK column (`plugins/umbra-permissions/src/perm.rs:189-207`, `membership.rs:202-205`): `...fetch().await?.into_iter().map(|up| up.permission_id.id())` pulls every column to extract one id. Query count is already optimal (no N+1); narrow tables make this minor. Project with `values(&["permission_id"])`/`only()`.

## Done well (no action)
- **Batched select_related / prefetch / reverse-FK / M2M** — `hydration.rs`/`dynamic.rs` issue exactly `1 + len(relations)` queries regardless of parent row count; loops embed pre-fetched buckets, never query inside the row loop. No N+1.
- **`count()`** reshapes to `COUNT(*)` with LIMIT/OFFSET dropped (proper pushdown). **`get()`** uses `LIMIT 2` to distinguish one-vs-many.
- **`bulk_create`** = one multi-row INSERT; **`bulk_update`** = one CASE-based UPDATE; **`set_user_groups`** = 1 DELETE + 1 bulk_create. No commit-per-row loops.
- **REST list** = 1 paged SELECT + 1 COUNT (COUNT skipped for `NoPagination` via `needs_total()`), includes batched; page size capped via `max_page_size`/`max_limit` for the non-default paginators. **Admin changelist** uses column projection (`select_cols(display_cols)`, not SELECT *), page_size clamped to [1,200], FK columns render raw ids (no per-row label N+1); `fk_picker` autocomplete is fully paged + search-pushed.
- **REST permissions** loaded once per request into `identity.extras`; `HasPermission::check` is a pure in-memory lookup.
- **Pool** created once at `App::build()`, shared ambiently via `OnceLock`. SQLite PRAGMAs (WAL/NORMAL/busy_timeout/FK-on) well tuned; per-statement logging disabled.
- **Projection surface exists** (`only()`, `values()`, `select_cols()`) — the gap is that a couple of built-in call sites (PERF-4, PERF-8) don't use it, not that the tool is missing.

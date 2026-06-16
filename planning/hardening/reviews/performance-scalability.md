# Performance & Scalability Review — umbra

Read-only review of the ORM, REST, admin, permissions, and OpenAPI plugins. Each finding cites `file:line`, quantifies the cost, and proposes a fix. Severity: **Critical** (O(table) memory / unbounded query reachable from a public endpoint), **Important** (N+1 on a common path), **Optional**, **FYI**.

---

## N+1 queries

**Important:** `apply_overrides` rebuilds the whole model registry per row (`plugins/umbra-rest/src/lib.rs:779`, called per-row at `:1758-1761`) — `apply_overrides_depth` calls `umbra::migrate::registered_models()` for every row in the list response, and `registered_models()` **deep-clones every `ModelMeta` (each with its full `Vec<Column>`) on every call** (`crates/umbra-core/src/migrate.rs:85-92`). Cost: O(rows × registry_clone) — a 1000-row page against a 200-model project does 1000 full-registry deep clones per request, all to find one `ModelMeta` and check for FK/file columns. → Fix: resolve the table's `ModelMeta` **once** before the row loop and pass it in, or use the existing cached `model_meta_by_table` lookup (the same cache already added for the decode hot path, `migrate.rs:104-118`). gaps ref: NEW.

**Important:** `AdminPerms::load` + `require` issue 5–~14 sequential permission queries per admin page (`plugins/umbra-admin/src/permcheck.rs:118-125` + `:91-102`) — `load` runs four `check()` calls (View/Add/Change/Delete) **sequentially**, and the changelist also calls `require(...View)` first (`handlers/list.rs:424,429`). Each `check` for a non-superuser runs `has_perm` = up to 2–3 queries (direct grant `exists`, `group_ids_for_user`, group-junction `contains_any`; `plugins/umbra-permissions/src/perm.rs:128-150`). So a non-superuser changelist render fires ~12–14 sequential DB round-trips just for permissions. Superusers short-circuit to 1 (`perm.rs:171`). → Fix: load the user's permission set **once** via the already-present `user_perms(user_id)` (`perm.rs:184`, one query for the whole set) and test the four codenames in memory; or at minimum `futures::join_all` the four `check`s. gaps ref: NEW.

**FYI (already optimized):** REST `?include=` FK expansion is properly batched — `hydrate_select_related_into` runs `1 + len(hops)` queries per chain via `SELECT ... WHERE id IN (...)` regardless of parent-row count (`crates/umbra-core/src/orm/dynamic.rs:2141-2199`). No N+1.

**FYI (already optimized):** The admin changelist does **not** resolve FK labels per-row — `fetch_rows_paged` renders raw FK ids (`plugins/umbra-admin/src/rows.rs:94-117`); `resolve_fk_label` (`handlers/list.rs:29`) is only called once per *active filter chip* in `build_active_filter_list` (bounded by filter count, not row count). The dashboard model-count fan-out is `join_all`'d, not serial (`handlers/list.rs:324-335`). Dashboard feed widgets cap at `limit(5)` (`handlers/dashboard.rs:109`).

---

## Unbounded fetching / missing LIMIT

**Critical:** M2M form candidate list fetches the **entire target table** with no LIMIT (`plugins/umbra-admin/src/view.rs:511-518`) — `form_m2m_fields_for` runs `DynQuerySet::for_meta(&target).select_cols(...).fetch_as_strings()` with no `.limit()`, building one checkbox candidate per row. Called on every add/edit form render (`handlers/crud.rs:229,310,362,436,528,607`). A M2M to a growing table (tags, users, products) loads every row into RAM and into the HTML form. The FK picker by contrast is paginated (`handlers/fk_picker.rs:132`, `.limit(page_size)`) — the M2M widget should use the same searchable-picker pattern. → Fix: cap the candidate fetch and switch the M2M editor to the paginated/searchable picker for large targets. gaps ref: NEW.

**Critical:** REST CSV export bypasses the `MAX_LIST_ROWS` ceiling (`plugins/umbra-rest/src/lib.rs:1747-1753`) — `?format=csv` calls `fetch_rows(&model, None, None, &filter, &include)` with `page = None`, so the `if let Some(req) = page` clamp branch (`:2236-2246`, where `MAX_LIST_ROWS = 1000` is applied, `:84`) is skipped entirely. The endpoint then streams `SELECT * FROM table` (filters optional) and buffers every matching row into memory via `fetch_as_json`. Reachable from the same anonymously-readable list route as the JSON path (the PERF-1 ceiling note at `:2237` exists precisely to stop this on the JSON path). → Fix: apply a hard cap (or true streaming) on the CSV path too, or require filters/auth for unbounded CSV. gaps ref: NEW.

**Optional:** Admin detail fallback fetches up to 200 rows when no PK is supplied (`plugins/umbra-admin/src/rows.rs:178-181`) — `fetch_rows_filtered` with `where_pk = None` does `.limit(200)`. Bounded, but 200 full rows is a large default for what callers treat as a single-row helper. → Fix: confirm every caller passes a PK; tighten the no-PK fallback. gaps ref: NEW.

**FYI (already optimized):** REST JSON list is capped — `fetch_rows` clamps `req.limit.min(MAX_LIST_ROWS=1000)` even for `NoPagination` (which requests `u64::MAX`), and skips the COUNT round-trip when `needs_total()` is false (`lib.rs:2244`, `:1762-1769`). `PageNumberPagination` enforces `max_page_size` (`pagination.rs:169-192`). Admin filter facets cap distinct values at 100 (`handlers/list.rs:641-643`).

---

## Hot-path cost

**Optional:** `fetch_as_json` does a linear field scan per column per row (`crates/umbra-core/src/orm/dynamic.rs:942-944`) — `self.meta.fields.iter().find(|c| &c.name == col_name)` inside the row × column loop. O(rows × cols × fields), CPU-only (no clone/IO; uses `self.meta`). Negligible for narrow tables, noticeable for wide ones at high row counts. → Fix: build a `&str → &Column` HashMap once before the loop. gaps ref: NEW.

**Optional:** Admin changelist reads the same per-user pref row 3× + writes 2× per page load (`plugins/umbra-admin/src/handlers/list.rs:442,465,483` reads; `:496,516` writes) — each `get_table_pref` / `set_table_pref` / `set_last_path` calls `fetch_or_default(user_id)` (a DB read, `models.rs:145`), so one changelist render does ~5 round-trips against a single pref row. Constant per request (not row-scaling), but trivially collapsible. → Fix: fetch the pref row once, mutate in memory, single upsert. gaps ref: NEW.

**Optional:** OpenAPI spec is rebuilt from the full registry on every `/openapi.json` request (`plugins/umbra-openapi/src/lib.rs:192-194` → `build_spec`, `:217`) — walks every plugin × every model × every field, emitting six operations + a schema per model, with no memoization, despite the spec being static after `App::build()`. The playground fetches this on every load. For a 200-model project this is a heavy per-request rebuild. → Fix: build once into a `OnceLock<Value>` (or `Arc<str>`) at first request and serve the cached bytes. gaps ref: NEW.

**FYI:** Typed `select_related` hydration calls `registered_models()` once per hydration (outside the row loop — `hydration.rs:180,431,596`) and uses the cached `model_meta_by_table` for the per-row PK decode (`:96`). This is the correct pattern the REST `apply_overrides` finding above should adopt.

---

## Indexing

**Important:** The migration engine does **not auto-index foreign-key columns** (`crates/umbra-core/src/migrate.rs:2993-2994,3058-3059,3172-3173,3195-3196`) — `create_index_stmt` is emitted only for columns with explicit `#[umbra(index)]`, plus `unique` and the GIN full-text index. FK columns get no index unless the author remembers to add one. Every `select_related` `WHERE fk IN (...)`, every reverse/M2M junction join, and every admin FK filter therefore hits a sequential scan on the child/junction side at scale. This is the single biggest item behind gaps2 #63's "200M-row" concern: joins and FK filters fall over first. → Fix: auto-emit `CREATE INDEX` for every `SqlType::ForeignKey` column (Django does this by default), or for junction-table `parent_id`/`child_id` at minimum. gaps ref: #63 (related) / NEW.

**Important:** Soft-delete adds an auto `WHERE deleted_at IS NULL` to every query but emits no index on `deleted_at` (`crates/umbra-core/src/orm/queryset/mod.rs:188-195,242-245` inject the predicate; no matching index in `migrate.rs`) — on a soft-delete model, *every* read filters on an unindexed column, so the planner scans the full table (including tombstoned rows) on each query. → Fix: auto-index `deleted_at` (ideally a partial index `WHERE deleted_at IS NULL` on Postgres) whenever `#[umbra(soft_delete)]` is set. gaps ref: NEW.

**FYI (works):** Explicit single-column `#[umbra(index)]`, multi-column indexes (BUG-7, `migrate.rs:2997,3173`), `unique` constraints, and Postgres GIN full-text indexes are all emitted correctly, and column-level index changes are diffed by the autodetector (`migrate.rs:2682,2831`).

---

## Async / blocking

**FYI (clean):** No blocking syscalls, `std::fs`, or sync locks held across `.await` were found on the request hot paths in `plugins/*/src` or the ORM read path. The `std::fs` references in `umbra-email` are doc-comment examples; the realtime sender uses a bounded channel that drops rather than blocks (`plugins/umbra-realtime/src/lib.rs:49,388`). Permission `check` is async and routes through the pool. No finding.

---

## Summary

**Counts:** 2 Critical, 5 Important, 5 Optional, 0 blocking-async findings. Several already-optimized paths confirmed (REST `?include=` batching, REST JSON list cap, dashboard COUNT fan-out, cached decode lookup).

**Top 3 scalability cliffs:**

1. **Unbounded full-table fetches reachable from endpoints** — admin M2M form candidate list (`view.rs:511`, loads the entire target table into every add/edit form) and REST CSV export (`lib.rs:1748`, bypasses the 1000-row cap). Both buffer O(table) into RAM; both are the first things that fall over as a referenced table grows.

2. **No FK / `deleted_at` indexes** — the migration engine indexes only explicit `#[umbra(index)]` / unique columns, so FK joins (`select_related`, M2M, reverse) and the per-query soft-delete `deleted_at IS NULL` filter all run sequential scans. This is what makes gaps2 #63's 200M-row target unviable until auto-FK-indexing lands.

3. **Per-row registry deep-clone in REST + serial permission storms in admin** — `apply_overrides` calls `registered_models()` (full deep clone) once per response row; `AdminPerms::load` fires ~12 sequential permission queries per non-superuser changelist render. Both are O(rows)/O(checks) waste with a one-line "resolve once" fix and an existing batched primitive (`model_meta_by_table`, `user_perms`).

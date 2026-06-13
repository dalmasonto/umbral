## Features worthy having

1. [ ] **A logs plugin like django-logs** 🔴 High
    > Why: This is both a feature and an architectural proof. If you cannot write a third-party plugin that intercepts every ORM write and logs it, the plugin contract is incomplete. It also answers gap #43 ("can a plugin be extended?"). A logs plugin that auto-registers its model, auto-wires into the admin, and auto-tracks mutations without touching core is the definitive demo that the plugin system works end-to-end.
    >
    > How: `LogsPlugin` implements `Plugin`, contributes a single `LogEntry` model (`id, table, action, pk, actor_id, timestamp, changes_json`). Hook into the ORM via the signal system (gap #38) — `post_save`, `post_delete`, `m2m_changed` — or via a middleware layer that wraps `QuerySet` terminals. Admin: auto-discover the model (already works) and add a "Recent activity" widget to the dashboard (gap #7). No core changes needed.

2. [ ] **An extended notifications plugin — SSE + bell** 🟡 Medium-High
    > Why: The full vision (chatbot SDK, frontend SDK, Phoenix-level realtime) is v1.5 territory. But the narrow version — admin notification bell for model changes, powered by SSE — is a medium-high win because it tests the signal-to-SSE bridge and the admin's ability to host non-CRUD UI.
    >
    > How: Split into two phases. Phase 1 (now): `NotificationsPlugin` subscribes to `post_save` signals (gap #38), pushes SSE events to an `/admin/events` stream, and renders a bell icon with a dropdown in `wrapper.html`. Phase 2 (later): per-model notification rules, email delivery via `umbra-email` (gap #39), and the full chatbot abstraction. The current gap description is too ambitious for one commit; scope the first deliverable narrowly.

3. [x] Should extend the rest plugin to have its own advanced UI like the admin using tailwidn css for a complete api playground. Should use/extend swagger API.
       — Shipped as `umbra-playground` plugin. React 19 + Vite + shadcn (Luma palette) + Inter, mounted by registering `PlaygroundPlugin::new()` alongside `RestPlugin` and `OpenApiPlugin`. Reads the OpenAPI spec from `/openapi/openapi.json` and gives a full request/response surface: methods, query params (with declared filter parameters when `ResourceConfig::enable_filters()` is on), JSON body editor (Monaco), form/multipart body with file uploads, custom headers, Bearer auth. Right pane shows the response with a real headers Table (filter + per-row copy), a History tab (Dexie/IndexedDB-backed, persistent across reloads), a cURL tab, and a Schema tab that renders the request body schema + every response schema with required / nullable / readOnly / maxLength / default / enum / FK target / multichoice surfaced from the umbra-openapi vendor extensions. See `docs/decisions/2026-06-03-playground-introspection-and-dexie.md`.

4. [x] **Extended admin field widgets via macro attributes** — SHIPPED 2026-06-10 (editors live; only code-block syntax highlighting remains, gaps2 #36).
    > **Attribute + render**: `#[umbra(widget = "markdown" | "rte" | "textarea")]` parses into `FieldSpec::widget: Option<&'static str>` (presentation-only, no DB effect, excluded from the migration diff like `help`/`example`); `migrate::Column` round-trips it. The admin's `input_kind` matches the widget to a form-editor branch (unknown widget names soft-fall-back to the SqlType kind, so plugin-defined widgets never break the form). `#[umbra(help = "...")]` now renders as a hint line under every admin input — the docstring's long-standing claim is finally true. Render-side filters: `{{ value | markdown }}` (pulldown-cmark + ammonia sanitize) for markdown fields, `{{ value | sanitize }}` (ammonia clean) for the HTML the `rte` widget stores. Both safe by default — reusable in admin and end-user templates alike.
    > **Editors** (the JS, also shipped): `admin.js` lazy-loads EasyMDE for `markdown` (toolbar + live side-by-side preview, textarea-native) and Quill for `rte` (snow theme; HTML synced back to a hidden textarea), only when a matching `data-widget` textarea is on the page. Mounted on all three form-render paths (DOMContentLoaded, htmx swap, sheet innerHTML), idempotent, and degrades to a plain textarea on JS-off / CDN failure. `wrapper.html` re-skins both with the admin design tokens so they track dark/light. Demonstrated on `umbra_website` markdown fields (`Plugin.description`, `ContentPage.body`, `BlogPost.body`, `PluginComment.body`, `Review.body`). Docs: `orm/column-types.mdx` "Rendering markdown safely" + "The `rte` widget". Tests: `markdown_filter.rs`, `widget_attr.rs`, admin `view::widget_and_help_reach_the_form_field`, `phase4_dashboard::admin_js_mounts_widget_editors` / `wrapper_themes_the_editors`.
    >
    > Original directive preserved below:
    > Why: A blog `body` field rendered as a `<textarea>` is hostile to content editors. But this is a rendering-layer change, not a data-model change — the database still stores `TEXT`. The right shape is a generic widget registration system, not hardcoded RTE/Markdown special cases.
    >
    > How: Extend `umbra-macros` to accept `#[umbra(widget = "rte")]` or `#[umbra(widget = "markdown")]` on any field. The macro sets `FieldSpec::widget: Option<String>` (default `None`). The admin's `input_kind` function matches the widget name to a template branch: `"rte"` loads TinyMCE/Quill.js, `"markdown"` loads a split-pane editor (Markdown-it + preview). Third-party plugins register new widget names by contributing a JS module + a template override. This keeps the admin core agnostic of specific editors.

5. [x] The `umbra startproject` should add all the umbra inbuild crates to cargo.toml by default but most of them commented out. Activate like auth, session, and admin plugins by default.
       — Shipped in `crates/umbra-cli/src/scaffold.rs`. Generated `Cargo.toml` now organises deps into four sections with header comments: framework core (`umbra` + `umbra-cli` — always required), active by default (`umbra-auth`, `umbra-sessions`, `umbra-admin`, `umbra-rest`, `umbra-openapi` — what the generated `main.rs` wires in), available built-ins (`umbra-playground`, `umbra-tasks`, `umbra-permissions`, `umbra-rls`, `umbra-cache`, `umbra-email`, `umbra-media`, `umbra-signals`, `umbra-static`, `umbra-security` — listed as commented `# umbra-…` lines with a one-line description per crate), and third-party runtime deps. Each commented line gives a one-sentence purpose so a user scanning the manifest knows what would happen if they uncommented it. New regression test (`scaffold_project_cargo_toml_lists_every_builtin_plugin_at_least_commented`) walks all 10 non-default plugins and asserts they're present, plus spot-checks three are present as `# umbra-<name>` commented lines.

6. [x] We shall create our own plugin using tailwindcss with something like htmx for Swagger UI integration. This will use swagger api endpoints but our own frontend. This will help us create a highly customized api testing experience just within the framework which is just a **plug away**. Also, the current openapi implementation does not take into account rest api endpoints ie permissions, authentication classes etc. Swagger with django auto-shows it requires bearer token authentication or some other auth method or auth headers automagically. So our UI should be so extensive and nice, think of mini postman with headers, body, and response previews. Body that allows inputs like numbers, forms, json, etc. This might benefit from being an extension of rest plugin ie how DRF has its own api testing tools. But for our case, it should be more better UI ie with the ability to save entries, history, and reuse them later. headers saved in local storage. Can we use something like React here to make the UI more interactive and dynamic? That will be a big win for the user experience. With react, we can even use dexiejs for our local storage database needs. So this means, we expand the rest plugin routes with the interact page. The design system we shall use shall be like that in admin plugin or better /home/dalmas/E/projects/umbra/docs/admin-backend/DESIGN.md. You can copy some styles from the UI folder (/home/dalmas/E/projects/umbra/docs/admin-backend/ui)
       — Shipped as `umbra-playground` (same plugin as #3). React + Vite + shadcn (Luma palette), not htmx — but the "highly customized api testing experience" landed: per-method UI, params/body/headers/auth tabs, Schema introspection panel, Dexie-backed history, cURL export, filter chips on list endpoints. Headers + settings persist via localStorage; history persists via IndexedDB (Dexie). What's still open from this feature description is auth-aware UX (the *playground* doesn't auto-detect which auth method an endpoint needs because the *OpenAPI spec* doesn't publish `securitySchemes` yet); that's logged as item #4 in `bugs/playground-openapi-gaps.md`. The bigger "save entries, reuse" piece is partially there (history per endpoint, click-to-replay is on the next pass list). Mini-Postman feel is there.

7. [ ] **Admin dashboard widgets** 🟡 Medium
    > Why: This turns the admin from "a collection of tables" into "a control panel." It's a genuine differentiator from Django's table-heavy admin. But the full vision (drag-and-drop, per-user layouts, widget DSL) is large. The minimal version proves the concept without committing to the full framework.
    >
    > How: Ship a minimal v1 first — hardcoded "Recent activity" (last 10 `LogEntry` rows) and "Count by status" (a mini bar chart from a `GROUP BY` query) cards on the admin index page. A `Widget` trait with `title()`, `queryset()`, and `template()` methods. No drag-and-drop, no user-scoped layouts, no reordering (gap #8 depends on this). Once the trait exists, gap #8 (reordering) and gap #1 (logs plugin feeding the recent-activity widget) become natural follow-ups.
    > THIS IS SLIGHTLY DONE AND MAYBE DONE - There are dashboard widgets already ie model cards, recent users. There was an extension of the same, it could be its not documented anywhere. So we need to make and have this one. 
    > Important: Widgets should be intelligent ie a widget can show filters ie date ranges, choices, etc. It should be capable of rendering charts, graphs, and other visualizations.

8. [ ] **Widget reordering (per-user)** 🟢 Low
    > Why: Only matters once gap #7 ships a real widget registry. Without widgets, there is nothing to reorder.
    >
    > How: Defer indefinitely if #7 ships as hardcoded cards. If #7 ships with a full `Widget` trait and registry, this becomes medium priority: add `AdminUserPref` columns for `widget_order_json`, parse into a `Vec<WidgetId>` on dashboard render, and let the frontend send a reorder POST. Depends on the widget ID system from #7.

9. [ ] **GraphQL plugin** 🟡 Medium
    > Why: GraphQL is a "check the box" feature for modern frameworks. But a native GraphQL engine (schema introspection, resolver generation, N+1 batching via DataLoader, mutation validation, subscriptions) is months of work.
    >
    > How: The pragmatic path is auto-generating a GraphQL schema from the OpenAPI spec (which already exists) via a converter, rather than building a native engine. That gives `graphql-codegen` compatibility and Apollo Client support for ~20% of the effort. Native resolvers and DataLoader come in v1.5. Ship as `umbra-graphql` plugin, opt-in, mounted alongside `RestPlugin`.

10. [ ] **WebSocket playground** 🟢 Low
    > Why: A standalone WebSocket playground is niche — most API testing is HTTP. Only valuable once the framework has actual WebSocket endpoints to test (gap #45).
    >
    > How: Defer until `umbra-realtime` (gap #45) ships WebSocket/SSE endpoints. Then extend the playground with a "Realtime" sidebar section that lists WebSocket routes, shows connection status, and renders incoming messages as a scrollback. Until then, there is nothing to test.

11. [ ] **Frontend hydration for Jinja templates** 🟡 Medium
    > Why: This is vague as stated. If it means "interactivity without full page reloads" (HTMX + Alpine.js on Jinja templates), that's already working — the admin uses HTMX. If it means "server-side render + client-side hydrate like Next.js / Remix," that's a fundamental architecture change (Vite build pipeline, server/client component boundaries, hydration markers).
    >
    > How: Close the "HTMX + Alpine.js" interpretation as already done. If the user wants true SSR+hydration, open a separate feature for "SSR with client-side hydration" and scope it as a v1.5 research project. The current gap description should be rewritten to clarify which interpretation is intended.

12. [~] **Playground tabs (Dexie persistence)** 🟡 Medium
    > Why: The current playground loses your in-progress request when you click another endpoint. This is a pure UX pain point with clear completion criteria and a small scope.
    >
    > How: Add a `tabs` slice to the existing Zustand store. Each tab holds `endpoint, method, params, body, headers, auth`. The sidebar click opens a new tab if the endpoint is not already open; clicking an existing tab switches to it. Save the full `tabs` array to Dexie on every change (debounced). On reload, restore tabs from Dexie and pre-populate the UI. This is a contained frontend task with immediate payoff.
    > Also with this, we can add data export from the playground as is, ie I can export from my browser and share it with a colleague who can import and get the same snapshot data

---

## ORM Completeness — What is still missing to call the ORM "production-grade"

These are the QuerySet features and model-level capabilities that Django developers reach for every day. Without them, complex reporting, analytics, and relationship-heavy apps are painful or impossible.

13. [x] **`annotate()` + aggregation functions** 🔴 High
       — Shipped. `Aggregate` enum in `crates/umbra-core/src/orm/aggregate.rs` covers `Count`, `Sum`, `Avg`, `Max`, `Min` with named constructors. `QuerySet::aggregate(&[(name, Aggregate)])` returns a single `serde_json::Value::Object` (with COUNT as int, AVG as float, SUM/MAX/MIN inheriting source column type). `QuerySet::annotate(group_cols, &[(name, Aggregate)])` returns `Vec<Value::Object>` with the group columns and named aggregates per row. Both compose with `filter` / `exclude` so WHERE applies before aggregation. Unknown columns fail loudly with `sqlx::Error::Protocol` before any SQL runs. Tests in `crates/umbra-core/tests/aggregates.rs` (7 tests). **Deferred**: `StdDev` / `Variance`, window-function aggregates.

14. [x] **`Q` objects for complex boolean logic** 🔴 High
       — Already shipped in `crates/umbra-core/src/orm/expr.rs:276-311`. `Q::and(a, b)`, `Q::or(a, b)`, `Q::not(p)` compose predicates explicitly; the existing `&` / `|` operator overloads on `Predicate` keep working alongside them. Both styles dispatch through the same per-backend `cond_for(backend)` path so SQLite-specific overrides survive composition. Re-exported from `umbra::orm::Q`. Test coverage in `crates/umbra-core/tests/q_objects.rs` (8 tests pinning render shape, AND/OR/NOT semantics, nested composition, and live SQLite execution).

15. [x] **`exclude()` — negated filtering** 🟡 Medium
       — Shipped on both `QuerySet<T>` and `Manager<T>` (`crates/umbra-core/src/orm/queryset.rs`). Implemented as sugar over `filter(Q::not(p))` so the predicate chain still ANDs naturally — `.filter(A).exclude(B).filter(C)` renders as `WHERE A AND NOT B AND C`. No new SQL-generation surface; Q::not handles backend-specific override negation. Tests in `crates/umbra-core/tests/exclude.rs`.

16. [x] **`values()` and `values_list()` — column projection** 🟡 Medium
       — `values(&["id", "title"])` shipped on both `QuerySet<T>` and `Manager<T>`. Returns `Vec<serde_json::Value::Object>` instead of typed `T` rows; skips both the unused-column transfer cost and the FromRow hydration overhead. Reuses `decode_to_json` / `decode_pg_to_json` from `orm::dynamic` so every column type round-trips correctly (int / string / bool / date / Json). Composes with `filter` / `exclude` / `order_by` / `limit` / `offset`. Unknown column names fail loudly before any SQL runs. Tests in `crates/umbra-core/tests/values_projection.rs`. **Deferred**: `values_list()` (typed-tuple return) — needs a different generic-arity story; ship when a consumer surfaces the need.

17. [x] **`distinct()` — duplicate elimination** 🟢 Low
       — `QuerySet::distinct()` emits `SELECT DISTINCT ...`. Most useful paired with `.values(&["col"])` to dedupe a column-projected list. Tests in `crates/umbra-core/tests/earliest_latest_distinct.rs`. **Deferred**: Postgres-specific `DISTINCT ON (cols)` until a concrete consumer surfaces the need.

18. [x] **`select_related()` — FK prefetch via JOIN** 🔴 High
       — Already shipped. `QuerySet::select_related(field)` and `.select_related_many(&[...])` accumulate FK names; the `fetch` / `first` terminals run one batched `SELECT ... WHERE id IN (...)` per FK after the main query and call `HydrateRelated::hydrate_fk` to populate `ForeignKey<U>.resolved` on every row. Lives in `crates/umbra-core/src/orm/queryset.rs::hydrate_select_related`. Tests in `crates/umbra-core/tests/select_related.rs` cover single FK, multi-FK, serde JSON projection (`post["author"]` renders as the full object after select_related and stays an integer without it), and template-context access. **Deferred**: nested traversal (`"author__manager"`) — current implementation supports one-hop FKs only; chains require successive `.select_related` on the resolved row.

19. [~] **`prefetch_related()` — M2M and reverse-FK batch loading** 🟡 Medium
       — M2M batching shipped. `QuerySet::prefetch_related("tags")` / `prefetch_related_many(&[...])` issue one batched JOIN through the junction table for every parent, group results by `parent_id`, and populate each parent's `M2M.resolved` slot via the new `HydrateRelated::set_m2m_resolved_json` hook. Macro override emits the per-M2M-field arms; new `HydrateRelated::pk_i64` hook (also macro-emitted, only for i64-PK models) feeds the parent-id collection. v1 constraints: M2M only, i64 parent PK only — same as the rest of the M2M plumbing. Tests in `crates/umbra-core/tests/prefetch_related.rs` (3 tests). **Still open**: reverse-FK collection prefetch (`prefetch_related("comment_set")`) — needs a Vec-on-parent slot that doesn't exist yet.

20. [x] **`bulk_update()` — mass updates without N round-trips** 🟡 Medium
       — `Manager::bulk_update(instances)` shipped. Builds `UPDATE table SET col = CASE id WHEN 1 THEN <val1> WHEN 2 THEN <val2> END WHERE id IN (1, 2)` — one CASE per non-PK column. Default-PK instances skipped. Empty input is a no-op. Same SQL on both backends. Tests in `crates/umbra-core/tests/bulk_update_raw.rs`.

21. [x] **`update_or_create()` — upsert with defaults** 🟡 Medium
       — `Manager::update_or_create(predicate, defaults) → (T, bool)` shipped. On hit: update the matched row's non-PK columns with the defaults' values, re-fetch, return `(row, false)`. On miss: insert `defaults`, return `(row, true)`. PK in defaults is ignored on the update path. Tests in `crates/umbra-core/tests/update_or_create.rs`.

22. [x] **`raw()` / `raw_sql()` — escape hatch** 🟡 Medium
       — `Manager::raw(sql)` shipped. Delegates to `sqlx::query_as::<DB, T>` against the ambient pool; dispatches on backend so user code stays portable. Returns typed `Vec<T>` decoded by `FromRow`. Skips `select_related` / `prefetch_related` chains (those only apply to the typed builder path); no parameter binding (sanitise input before calling). Tests in `crates/umbra-core/tests/bulk_update_raw.rs`.

23. [—] **`defer()` / `only()` — lazy column loading** 🟢 Low — **won't ship as spec'd**
       — Recommendation: don't ship as a distinct API. The lazy-fetch-on-access part is what makes `defer` interesting in Django; Rust doesn't have property accessors to intercept (`post.body` is a field access, not a method call). The non-lazy variant is `values()` (#16, shipped) with the column set complemented or restricted. Best move: pin this entry as "intentionally not shipped" so it stops getting re-evaluated; rename `values()` to `project()` if naming clarity matters later. **Revisit only if** a user request specifies the lazy-fetch behaviour and accepts the complexity (either macro-generated partial types per model, or a FromRow extension that tolerates missing columns).

24. [x] **Database functions — `Lower`, `Upper`, `Length`, `Now`, `Coalesce`, `Concat`, `Trim`** 🟡 Medium
       — `StrColExt` ships `.lower()`, `.upper()`, `.length()`, `.trim()`, `.coalesce(default)`, `.concat(suffix)` on `StrCol` + `NullableStrCol`; each returns `ColExpr<T>` → chain `.eq/.ne/.lt/.le/.gt/.ge(val)` for a `Predicate<T>`. `trim` is `TRIM(col)`; `coalesce`/`concat` build **native** sea-query exprs (`Func::coalesce`, `BinOper::Custom("||")`) — NOT `cust_with_values`, which embeds its own params and swaps bind-order against the outer `.eq` value (caught by a de-masking test: distinct fallback vs compared value). All render identically on SQLite + Postgres. 6 live round-trip tests in `crates/umbra-core/tests/db_functions.rs` (real rows, public filter path, read-back). Doc: `orm/querying.mdx`. **Deferred — `now`**: a DB-side `NOW()`/`CURRENT_TIMESTAMP` column comparison is silently wrong on SQLite (its text format ≠ sqlx-chrono's stored ISO format); bind `chrono::Utc::now()` as a value instead (`col.lt(Utc::now())`), which is correct on every backend and already works. Order-by via DB function still deferred (needs `OrderExpr` to accept a `SimpleExpr`).

25. [ ] **Conditional expressions — `Case`, `When`, `Default`** 🟢 Low — **wait for demand**
    > Why: `CASE WHEN ... THEN ... ELSE ... END` is powerful for tiered badges and computed status fields, but it has workarounds (compute in Rust after fetching, or use raw SQL). The SQL generation is straightforward; the ergonomics in Rust are the challenge.
    >
    > How: A builder API: `Case::new().when(view_count.gt(1000), 2).when(view_count.gt(100), 1).default(0)`. Each `when` takes a `Predicate` and a `Value`. Render to `CASE WHEN ... THEN ... ELSE ... END`.
    >
    > **Design call**: ship as a peer of `Aggregate`, **not** as a variant. Different semantics — `Case` is per-row, aggregates collapse rows. Introduce `Annotation` as a thin enum `{ Aggregate(...), Case(...) }`, take that in `annotate()`. The Case builder is ~30 lines + tests. **Triggering condition**: a user actually doing tiered-badge SQL in `raw()` — that's the demand signal. `annotate()` shipped in Wave B; today no consumer.

26. [~] **Subqueries — `Subquery` and `Exists`** 🟡 Medium
       — Partial. `Subquery` type ships in `crates/umbra-core/src/orm/mod.rs`; built via `QuerySet::into_subquery(col_name)` / `Manager::into_subquery(col_name)`. `IntCol::in_subquery(sub)` and `ForeignKeyCol::in_subquery(sub)` produce `Predicate<T>` rendering as `<col> IN (SELECT col FROM ...)`. Most "is there a row that references me" queries collapse to in_subquery without correlated EXISTS. Tests in `crates/umbra-core/tests/subquery.rs`. **Still open**: correlated `EXISTS(...)` with `OuterRef` references back to the outer query's columns.

27. [ ] **Window functions — `RowNumber`, `Rank`, `DenseRank`, `Lead`, `Lag`, `NthValue`** 🟢 Low — **defer hard**
    > Why: Needed for leaderboards and "top N per category," but Postgres-only practically (SQLite needs window-function support compiled in). The user base for this is smaller than the core QuerySet gaps.
    >
    > How: Add a `Window` struct and an `Over` clause.
    >
    > **Design call**: when this does ship, do the minimum — `RowNumber` / `Rank` / `DenseRank` with `PARTITION BY` + `ORDER BY` only. Skip frame specs (`ROWS BETWEEN ...`) entirely until a real bug forces them. That's 60% of the code for 95% of the value. **Until then**: users with this need have `raw()` as the escape hatch and it's tolerable. No demand signal today; revisit when one surfaces.

28. [x] **`union()`, `intersection()`, `difference()` — set operations** 🟢 Low
       — Shipped. `QuerySet::union(other)`, `intersect(other)`, `except(other)` combine two `QuerySet<T>` values via sea-query's `UnionType::{Distinct, Intersect, Except}`. The shared `T` type-param enforces column-shape compatibility at compile time — no runtime check needed. Default is the de-duplicating UNION (UNION ALL would be a future variant). Both sides apply their accumulated WHERE before the combine; further `.filter()` on the returned QuerySet applies to the OUTER combined query. Tests in `crates/umbra-core/tests/set_ops.rs`.

29. [~] **`iterator()` — memory-efficient streaming** 🟡 Medium — **ship in two phases**
    > Why: For tables with millions of rows, `fetch()` collects into a `Vec` and would OOM. `iterator()` yields rows one at a time — the only viable path for exports, migrations, and bulk transforms.
    >
    > **Design call (two phases)**:
    >
    > **Phase 1 — `try_for_each(|row| -> Result<(), E>)` (callback shape)**: ~40 lines, no new workspace dep, idiomatic Rust callback. Ships the same memory bound as a Stream (one row at a time). Critically: do NOT name this `iterator()` — that's a lie about what it returns. Two names, two semantics.
    >
    > **Phase 2 — `iterator()` returning `Stream`**: ships once `futures-util` is in the workspace for some other reason (probably SSE / WebSockets — gap #45). At that point `iterator()` is the BoxStream'd variant and `try_for_each` stays for callers who prefer the callback shape.
    >
    > **Next-session action**: Phase 1 is a one-commit feature whenever it surfaces — write it, ship it, move on. Phase 2 is gated on futures-util landing for another reason; don't pull it in just for iterator().
       — Phase 1 shipped. `QuerySet::try_for_each(chunk_size, |row| -> Result<(), E>)` runs the SELECT in pages of `chunk_size` rows and invokes the callback per row. Memory bound = `chunk_size * sizeof::<T>` instead of the full result set, so a million-row export doesn't OOM the way `fetch()` would. New `TryForEachError<E>` enum distinguishes Sqlx vs Callback failures; first error stops the walk. Deliberately NOT named `iterator()` per the design note — the callback shape requires no new deps. select_related / prefetch_related hooks are not applied (raw column data, one row at a time). 4 tests pin: cross-chunk traversal, oversized chunk = single fetch, short-circuit on callback error, empty filter = no-op. Phase 2 (BoxStream-returning `iterator()`) stays gated on `futures-util` landing for SSE / WebSockets.

30. [x] **Reverse relation accessors — `post.comment_set`, `category.post_set`** 🔴 High
       — Shipped via `#[derive(Model)]`. For every `ForeignKey<Parent>` field on a derived Child, the macro emits `impl Parent { pub fn <child_snake>_set(&self) -> QuerySet<Child> }` returning a QuerySet pre-filtered by the FK column = parent's primary key. Multiple FKs from one Child to the same Parent are disambiguated with `<child>_via_<field>_set`. `ForeignKeyCol::eq` / `ne` generalised from `i64` to `impl Into<sea_query::Value>` so the accessor body works for any PK type. Tests in `crates/umbra-core/tests/reverse_fk.rs`. **Limitations**: parent type must be local (Rust orphan rule); parent PK must implement `Into<sea_query::Value>` (every built-in PK type does).

31. [x] **JSONField / JSONB query operations** 🟡 Medium
       — Shipped on `JsonCol` / `NullableJsonCol` with full backend dispatch. `meta.has_key("name")` renders as Postgres `meta ? 'name'` or SQLite `json_extract(meta, '$.name') IS NOT NULL`. `meta.path_text(&["a", "b"])` returns a chainable that supports `.eq/.ne/.is_null/.is_not_null`; rendering is `meta -> 'a' ->> 'b'` on Postgres or `json_extract(meta, '$.a.b')` on SQLite. Tests: Postgres render shape in `crates/umbra-core/tests/json_ops.rs`; live SQLite end-to-end in `crates/umbra-core/tests/json_sqlite_live.rs`. **Deferred**: REST filter-parser hooks for `?meta__has_key=name` (lives with REST plugin work).

32. [x] **ArrayField operations** 🟢 Low
       — Substantially shipped, Postgres-only with boot-time gating. `Vec<T>` on a model classifies as `SqlType::Array(ArrayElement::*)`; DDL renders `<type>[]`. `ArrayCol<T>` / `NullableArrayCol<T>` column types ship the relational operators: `.contains(val)` (`@>`), `.contains_all(&[vals])`, `.contained_by(&[vals])` (`<@`), `.overlaps(&[vals])` (`&&`). System check rejects Array-having models on SQLite with a clear backend-mismatch diagnostic. Tests: `crates/umbra-core/tests/array_field.rs` (4 unit + 3 ignored live-PG) and `array_ops.rs` (9 unit + 1 ignored live-PG). **Deferred**: SQLite JSONB-storage fallback (the spec described this as a "v1 nice-to-have"; the boot-time rejection is the cleaner default).

33. [~] **Full-text search integration** 🟡 Medium
       — Postgres surface shipped, with limitations. `TsVector` newtype field type classifies as `SqlType::FullText`; DDL renders `tsvector`. System check rejects on SQLite. `FullTextCol<T>` / `NullableFullTextCol<T>` ship `.matches("query")` (`@@ to_tsquery`) and `.matches_websearch("query")` (`@@ websearch_to_tsquery`). Tests: `crates/umbra-core/tests/fulltext_field.rs` (7 unit + 2 ignored live-PG). **Still open**: auto-GIN-index creation on the tsvector column (currently the caller manages indexes manually); the `to_tsvector('english', body) @@ plainto_tsquery('...')` form for text-column-into-tsvector-at-query-time (today the column must already be `tsvector`-typed); SQLite FTS5 fallback (deliberately deferred — different model, virtual tables vs typed column).

34. [x] **`in_bulk()` — fetch many rows by PK into a HashMap** 🟢 Low
       — `QuerySet::in_bulk(pks)` shipped. Builds `SELECT * WHERE pk IN (...)`, groups by the existing `HydrateRelated::pk_i64` hook, returns `HashMap<i64, T>`. Missing ids silently absent; empty input short-circuits. v1 limitation: i64-PK models only. Tests in `crates/umbra-core/tests/in_bulk.rs`.

35. [x] **`explain()` — query plan inspection** 🟡 Medium
       — `QuerySet::explain()` returns the execution plan as a plain-text `String`. SQLite: prepends `EXPLAIN QUERY PLAN` and joins the `detail` column; Postgres: prepends `EXPLAIN` and joins the `QUERY PLAN` column. Tests in `crates/umbra-core/tests/earliest_latest_distinct.rs`. **Deferred**: Postgres `EXPLAIN (FORMAT JSON)` for machine-readable output — use raw sqlx when needed.

36. [x] **Date/time extract functions — `year`, `month`, `day`, `week_day`** 🟡 Medium
       — Shipped. `DateTimeColExt` trait covers `.year()`, `.month()`, `.day()`, `.hour()`, `.minute()`, `.second()`, `.week_day()` on both `DateTimeCol` and `NullableDateTimeCol`. Backend-aware rendering hidden in `ColExpr<T>`: Postgres uses `CAST(EXTRACT(<part> FROM col) AS INTEGER)`, SQLite uses `CAST(strftime('<fmt>', col) AS INTEGER)`. `week_day()` returns 0=Sunday..6=Saturday on both backends (PG `EXTRACT(DOW ...)` and SQLite `strftime('%w', ...)` happen to agree). Compose with `.eq/.ne/.lt/.le/.gt/.ge(int)`. 12 tests in `crates/umbra-core/tests/db_functions.rs` (7 string/year/month/day + 5 new for the time-of-day + weekday extracts).

37. [x] **`earliest()` / `latest()` — convenience wrappers** 🟢 Low
       — Shipped. `QuerySet::earliest("col_name")` = `order_by(col.asc()).first()`; `latest("col_name")` = `order_by(col.desc()).first()`. Takes a `&'static str` column name to match Django's call shape. Tests in `crates/umbra-core/tests/earliest_latest_distinct.rs`.

38. [x] **Signals — `pre_save`, `post_save`, `pre_delete`, `post_delete`, `m2m_changed`** 🔴 High
       — Fully wired. Lives in `crates/umbra-core/src/signals.rs`. Surface: `subscribe`/`subscribe_async`/`emit`/`clear_for_tests` + ORM emitters `emit_pre_save`/`emit_post_save`/`emit_pre_delete`/`emit_post_delete`/`emit_bulk_post_save`/`emit_bulk_post_delete`/`emit_m2m_changed`. The first four fire from `Manager::save` and `Manager::delete_instance` for per-row hooks. Bulk terminals (`bulk_create`, `update_values`, `update_expr`, `QuerySet::delete`) fire one `bulk_post_save:<table>` / `bulk_post_delete:<table>` per call with the affected PKs (captured via `RETURNING <pk>`). M2M mutations (`M2M::add`/`remove`/`set`/`clear`) fire `m2m_changed:<junction>` with `{ action, parent_id, added, removed }`. Actor field: a tokio task-local `ACTOR: serde_json::Value` set via `with_actor(value, fut).await`; every signal payload (ORM and user-level) automatically inherits an `"actor"` key (Null when no scope is active). Tests: `signals_registry.rs`, `signal_actor.rs`, `bulk_signals.rs`, `m2m_signals.rs`.

38.1 [x] **Atomic transactions at the ORM level — opt-in via builder** 🔴 High
       — Shipped. `.atomic()` / `.non_atomic()` available on both `Manager<T>` and `QuerySet<T>`; the Manager flag propagates into QuerySets it constructs. `App::builder().atomic_transactions(true)` flips a global default stored in `OnceLock<bool>` inside `umbra::db`. Resolution order at terminal time: per-call override > builder default > false. Wired terminals: `Manager::create`, `Manager::bulk_create`, `QuerySet::update_values`, `QuerySet::delete`. Each wraps its single SQL statement in BEGIN/COMMIT (rolled back on Err) when atomic is true; otherwise the existing ambient-pool path stays unchanged. `.atomic()` and `.on_tx()` are documented as mutually exclusive — `.on_tx()` wins because that path doesn't read the atomic flag. Tests in `crates/umbra-core/tests/atomic_terminals.rs`.

    > Follow-ups still open under this number: REST-layer `ResourceConfig::new("order").atomic_writes(true)` per-resource opt-in (tracked alongside feature #58 since nested writes are its main use case).

---

## General Framework — What is still missing to call Umbra "feature-complete"

These are the cross-cutting capabilities that turn a framework from a neat ORM demo into a platform you can ship a SaaS on.

39. [ ] **Email sending — SMTP and API backends** 🔴 High
    > Why: Password resets, notifications, and transactional emails are table stakes. Without this, every app re-implements SMTP or pulls in `lettre` directly.
    >
    > How: `umbra-email` plugin with `EmailMessage::builder().to("...").subject("...").body("...").send().await`. Backends: SMTP (lettre), SendGrid, Mailgun, AWS SES. Integrate with the task queue (gap #43) for async sending. The plugin should be small — mostly a typed wrapper around `lettre` plus a backend trait.

40. [x] **File uploads and multipart handling** 🔴 High
    > — SHIPPED (branch `feat/file-image-fields`, 2026-06-12). `umbra::web` gained `is_multipart` / `parse_multipart` / `parse_and_store_multipart` (multer-based, binary-safe, stores file parts via the ambient `Storage`). `FileField` / `ImageField` ORM types store a storage key in a TEXT column (macro-classified, default `widget="file"/"image"`). The admin renders file/image upload widgets (image thumbnail preview), switches to `multipart/form-data` when a file field is present, and writes the stored key on submit (empty file part preserves the existing key). A boot system-check fails the build if a model declares a file field with no `Storage` backend registered. Archived under gaps2 #37.

41. [/] **Media storage — local, S3, R2, GCS** 🟡 Medium
    > — Partially shipped (2026-06-12). The `Storage` trait landed in `umbra-core` (`store(filename, content_type, bytes) -> StoredFile{key,url}`, `retrieve`, `delete`, `url`), with `FsStorage` (local filesystem) in `umbra-media` registered ambiently, and `MediaPlugin::with_storage(...)` to swap backends. The admin renders `ImageField` values as `<img>` thumbnails via `storage.url(key)`. STILL DEFERRED: `S3Storage` (aws-sdk-s3 / rust-s3, any S3-compatible endpoint) and the image library (thumbnails/EXIF) — both slot into `with_storage` / a cargo feature without touching `FileField` or the admin.

42. [ ] **Social auth / OAuth2 / OIDC** 🟡 Medium
    > Why: "Sign in with GitHub/Google" is table stakes for modern SaaS. Without it, every app re-implements the same 200 lines of OAuth dance.
    >
    > How: Extend `umbra-auth` with `OAuth2Backend` trait and built-in providers (GitHub, Google, Discord). Flow: redirect to provider, callback handler, create-or-link user, issue session. Use `oauth2` crate for the protocol. Keep it behind a cargo feature so OAuth-free apps don't pull the dependency.

43. [ ] **Background task queue (`umbra-tasks`)** 🔴 High
    > Why: Celery equivalent — `@task fn send_email(...)` that serializes to a DB table and is consumed by `cargo run -- worker`. Blocks email (gap #39), image processing, report generation, and webhook delivery.
    >
    > How: The `#[task]` macro already exists (gap #40 in gaps.md). What's missing is the consumer: a `TaskRunner` that polls the tasks table, executes handlers, and manages retries with exponential backoff. Add scheduled tasks (`eta: DateTime<Utc>`) and priority queues. This is a medium-to-large plugin but the macro work is already done.

44. [ ] **Caching layer — Redis and in-memory backends** 🟡 Medium
    > Why: Redis-backed cache for expensive queries, view fragments, and session stores. The cache plugin exists but needs deeper integration.
    >
    > How: `Cache::redis(url)` already exists. What's missing: cache key invalidation on model saves (via signals, gap #38), cache-aware QuerySet (`Post::objects().cache(300).fetch()`), and distributed cache invalidation across multiple app instances. Start with per-view `cache_page` (already shipped) and expand to low-level cache API.

45. [x] **WebSockets / SSE — real-time push** 🟡 Medium
       — Shipped end-to-end as `umbra-realtime`. User- AND group/room-targeted delivery (`Realtime::to_user(uid)` / `to_group("chat:123")` / `broadcast()`, `Target::send(event, data)`) over a connection registry (by_user/by_group indexes, O(1) targeting, leak-free deregister) and a `Broker` seam. Two transports: **SSE** (`GET /realtime/sse`, push-only) and **WebSocket** (`GET /realtime/ws`, bidirectional with a `MessageHandler` for inbound frames). Auth-aware `GroupPolicy` gates joins at handshake (default-deny non-`public:` groups → 403). Signals bridge: `RealtimePlugin::on_model::<T>()` / `on_table()` fans `post_save`/`post_delete` (#38) to a real-time send — zero-poll live dashboards. **Multi-instance: `RedisBroker`** (P6 phase 5, behind the `redis` feature) — `RealtimePlugin::redis(url)` relays every targeted send through a shared Redis pub/sub channel so it reaches the socket on whichever instance holds it; one background pump per instance publishes + subscribes + dispatches locally, reconnect-with-backoff; `Envelope`/`TargetKind` are the JSON wire format. Live website demo shipped (e156ad5 — SSE note feed on plugin pages). 11 tests (registry/broker/policy unit + SSE stream + WS round-trip via a bound tungstenite client + signals fan-out + Envelope round-trip + live-Redis cross-instance relay, env-gated). Docs: `realtime/sse.mdx` + `realtime/scaling.mdx`; design spec. **Only the playground "Realtime" tab remains — tracked under the playground feature #12, not here.**

46. [ ] **Rate limiting and throttling** 🟡 Medium
    > Why: Per-IP, per-user, and per-endpoint limits are essential for public APIs and login brute-force protection.
    >
    > How: Middleware that checks a Redis-backed counter per key (`ip:192.168.1.1`, `user:123`). Return `429 Too Many Requests` with `Retry-After`. Configurable via `App::builder().rate_limit(...)` or per-route decorators. Use `redis::expire` for TTL-based windows.

47. [x] **Health checks and readiness probes** 🟡 Medium
       — Shipped as `umbra-health` plugin. `GET /healthz` is unconditional 200 (liveness — the binary answered the syscall). `GET /ready` runs the DB probe (`SELECT 1` against the default pool) + every developer-registered `HealthCheck` and returns 200 + JSON on success or 503 + JSON when any check fails, with per-dependency status in the body so on-call can see which dependency is degraded without log-grepping. `HealthCheck` trait carries `name() -> &'static str` + `async fn check() -> Result<(), HealthError>`; register via `HealthPlugin::default().check(MyCheck)`. Checks run sequentially in `/ready` to avoid amplifying tail latency across the probe response. Routes are unconditionally mounted when the plugin is installed and never carry authentication (k8s + load balancers must reach them without credentials). 4 integration tests pin liveness + readiness behavior under each scenario.

48. [ ] **Structured logging / OpenTelemetry** 🟡 Medium
    > Why: JSON-structured logs with `trace_id`, `span_id`, `request_id` are required for debugging in distributed systems.
    >
    > How: Integration with the `tracing` crate. Add a `tracing_subscriber::layer` that emits JSON. Propagate `trace_id` across async boundaries via a tokio task-local. OpenTelemetry traces for HTTP requests, DB queries, and task queue operations. This shares infrastructure with gap #38 (signals actor field) — the same task-local can carry both the actor and the trace context.

49. [ ] **Metrics and monitoring — Prometheus-compatible** 🟡 Medium
    > Why: `http_requests_total`, `db_query_duration_seconds`, and `task_queue_depth` are needed for alerting, SLO tracking, and capacity planning.
    >
    > How: Use `metrics` crate with a Prometheus exporter. Expose on `/metrics` for scraping. Counters: requests, responses by status, DB queries, cache hits/misses. Histograms: request duration, DB query duration. Gauges: active DB connections, queue depth.

50. [ ] **i18n / localization** 🟢 Low
    > Why: `gettext`-style translation files are needed for non-English users, but the framework is currently English-only. This is a large surface (`.po`/`.mo` files, `LocaleMiddleware`, `{% trans %}` tags, locale-aware formatting).
    >
    > How: Defer until a concrete app needs it. When needed, use `fluent` (Mozilla's localization system) rather than gettext — it's modern, designed for software, and has a Rust crate. Add `LocaleMiddleware` that sets language from `Accept-Language` or a cookie.

51. [ ] **Form validation framework** 🟡 Medium
    > Why: Declarative validators (`#[validate(min_length = 5)]`) that produce per-field error maps. Currently validation is ad-hoc; a unified framework would replace scattered `if` checks in handlers.
    >
    > How: Use `validator` crate (already popular in the Rust ecosystem). Integrate with the admin (render errors inline) and REST (return `400` with `{"field": ["error"]}`). The derive macro can read `#[validate(...)]` attributes alongside `#[umbra(...)]` and emit validation logic.

52. [ ] **Testing utilities — fixtures, factories, and test client** 🟡 Medium
    > Why: A `TestClient` that boots the app in-memory and makes requests is what makes TDD fast. Currently every test is an integration test against a real server or requires manual setup.
    >
    > How: `TestClient::new(app)` that binds to a random port, provides `.get("/")`, `.post("/", body)`, and asserts on status + JSON. `Fixture` trait for loading seed data per test. `Factory` macros using `fake` crate. Transaction rollback per test via `BEGIN` / `ROLLBACK` in `setup` / `teardown`.

53. [ ] **Admin bulk actions** 🟡 Medium
    > Why: Checkbox-select rows, then "Delete selected", "Mark as published", "Export to CSV." Django's admin is powerful because of bulk actions. Currently Umbra admin only handles one row at a time.
    >
    > How: Add checkboxes to the list view (`templates/list.html`), a dropdown for action selection, and POST handlers for each built-in action. Custom actions via `AdminModel::actions()` returning `Vec<AdminAction>`. The existing `AdminConfig::actions()` already supports this at the API level; wire it to the frontend.

54. [ ] **Admin inlines — tabular and stacked** 🟡 Medium
    > Why: Edit related objects on the parent form. `PostAdmin` shows an inline `Comment` form set so an editor can moderate comments without leaving the post page. One of the most-used Django admin features.
    >
    > How: Add `AdminModel::inlines(&["comment_set"])`. In the form template, render a sub-table or sub-form for each related row. POST handling saves the parent and all inlines in one transaction. Depends on gap #30 (reverse relation accessors) to get `post.comment_set`.

55. [~] **Admin filters and date hierarchy** 🟡 Medium
       — Multi-filter rendering shipped. The toolbar above the table now carries the search input + a single `Filter` button (with an active-count badge); clicking it opens the dialog that displays every declared `list_filter` facet plus a search field. Selections from different facets combine with `AND` against the backend. Backend reshape: `pagination::ListParams` second slot moved from `Option<(field, value)>` to `Vec<(field, value)>`; `parse_list_params` collects every `?filter_<field>=<value>` (sorted by field for stable URL + chip ordering) with the legacy `?filter=field=value` shape kept as a single-entry fallback for old bookmarks. `rows::count_rows_filtered` and `fetch_rows_paged` now AND-loop the slice. List template renders one chip per active filter with a per-chip remove link that preserves every other selection. Dialog JS replaces `_pendingFilter: {field, value}` with `_pending: { <field>: <value> }` seeded from the server-rendered active map; Apply walks the map and emits `filter_<f>=<v>` URL params. Hidden inputs on the toolbar mirror every active filter so HTMX `hx-include` carries them through sort / page-size / pagination swaps. New `urlencode` Jinja filter (backed by the existing `urlencoding_simple` helper) escapes the per-chip URLs. 5 unit tests pin `parse_active_filters` against the multi-filter param shape, empty values, the legacy fallback, and named-wins-over-legacy precedence. **Deferred**: date hierarchy drill-down (`2026 > June > 04`) — pure template + handler work, separate iteration; the dialog's date inputs are still single-shot.

56. [x] **Admin dashboard widgets (built-in)** 🟡 Medium
       — Shipped. `plugins/umbra-admin/src/widgets.rs` ships a `Widget` trait (`key`/`title`/`kind`/`default_span`/`permission`/`data`) plus `WidgetSection` grouping, registered via `AdminPlugin::register_widget()` / `dashboard_section()` / `dashboard_section_at()`. Widget kinds: `Kpi`, `Card`, `Line`, `Bar`, `Donut`, `Radial`, `Heatmap`, `Progress`, `Table`, `Feed`. The "complex" additions all landed: `CardPayload`/`KpiPayload` carry comma-formatted + currency-labelled values (`humanize_number` / `format_thousands`), a delta / percent-change, and a sparkline; `LinePayload` is multi-series (overlay N years); donuts/radials/tables/heatmaps present. `builtin_total_models_widget` (bar) + `builtin_recent_users_widget` (feed) ship by default; the `umbra_website` dashboard (`src/widgets/`) drives the full set. Tests in `widgets.rs` pin serialization + payload builders + number formatting. **Deferred**: per-user widget reordering (#8) + drag-and-drop layout.

57. [x] **Admin autocomplete fields** 🟢 Low
       — Shipped. `plugins/umbra-admin/src/handlers/fk_picker.rs` serves `GET /admin/api/{table}/{field}/options?search=&page=&page_size=` — paginated search-as-you-type via the ORM `.search()` over the resource's search fields (or the label column) — plus `.../options/resolve?ids=` to hydrate already-selected values. `assets/admin.js` `initFkPickers()` binds an `input` listener (fires per keystroke) and an HTMX paginated dropdown; M2M pickers carry `data-m2m-search` too, so FK / M2M / O2O all search rather than load-all-rows.

58. [x] **REST plugin — nested serializers and writable nested objects** 🔴 High
       — Shipped (with one caveat). `ResourceConfig::for_::<Order>().nested("items", "order_item")` declares a writable nested resource. `POST /api/order/ { customer, items: [{product, qty}] }` creates the parent, then each child with its FK to the parent set **automatically** (the FK column is discovered from the child model's `fk_target` — the column referencing the parent table), and returns the full nested object (children echoed under `items`). The flat path is untouched/zero-overhead when no nested resources are declared. Helpers: `create_nested`, `meta_for_table`, `child_fk_to`, `compensate` in `plugins/umbra-rest/src/lib.rs`. Test: `tests/nested_writes.rs` (parent + 2 children, FK auto-set, embedded response; + a bad child rolls the whole thing back). Doc: `rest/nested.mdx`. **Caveat — atomicity is *compensating*, not a true DB transaction**: the dynamic write path (`insert_json`) has no tx variant, so a failed child deletes the parent + siblings (covers the common validation-failure case) but a process crash mid-write could orphan a parent. The true fix (`insert_json_in_tx`) is logged as `planning/orm_fixes.md` #2.

59. [x] **REST plugin — authentication integration** 🔴 High
       — Shipped. Every REST handler gates before executing: `cfg.gate(&table, &Action::{List,Retrieve,Create,Update,Delete}, identity)` (list / retrieve / create / update / delete + the custom `@action` dispatch) in `plugins/umbra-rest/src/lib.rs`; `ResourceConfig::permission(...)` attaches the permission class. OpenAPI publishes `components.securitySchemes` + a global `security` array from `umbra_rest::registered_security_schemes()` (`plugins/umbra-openapi/src/lib.rs`), so the playground can auto-detect which endpoints need a Bearer token. Tests: `plugins/umbra-rest/tests/auth_permission.rs`.

60. [x] **REST plugin — action endpoints with custom serializers** 🟡 Medium
       — Shipped. `ActionDef` carries optional `input_schema` / `output_schema` (JSON Schema as `Value`); declared via `ResourceConfig::action_input_schema(name, schema)` / `action_output_schema(name, schema)`. The action dispatch validates the request body against `input_schema` **before** the handler runs (a mismatch is a `400` with field errors); a focused validator covers the common action-guard subset — top-level `type`, `required`, `properties` (recursive), and per-property `type` + `enum` — and ignores unsupported keywords (the full schema still ships to OpenAPI verbatim, so no dep churn from a JSON-Schema crate). `umbra_rest::registered_action_schemas()` (mirrors `registered_security_schemes`) exposes the schemas; `umbra-openapi` emits a path item per schema'd action (`/api/<table>/{id}/<name>/` for detail, request `requestBody`/response `200` schemas inlined, `{id}` path param) so the playground knows the shape. Test: `plugins/umbra-rest/tests/action_schemas.rs` (valid→200; missing-required / wrong-type / bad-enum →400; schemas exposed). Doc: `rest/actions.mdx`. **Deferred**: full JSON-Schema validation (`$ref`, `oneOf`, numeric bounds) — wire the `jsonschema` crate if a consumer needs it.

61. [~] **Data import / export — CSV and Excel** 🟡 Medium
       — CSV **export** shipped via the REST list endpoint: `GET /api/<table>/?format=csv` downloads the full filtered set as CSV (same auth gate, `?filter`/`?search`/`?include`/`?fields` as the JSON list — just a different serialization). Columns follow the model's field order (after hide/sparse-field scrubbing); object/array cells render as compact JSON; the `csv` crate handles quoting/escaping; `Content-Disposition: attachment; filename="<table>.csv"`. `format` is a reserved query key (`filtering.rs`). Helpers `csv_response`/`rows_to_csv`/`csv_cell` in `plugins/umbra-rest/src/lib.rs`; test `tests/csv_export.rs`; doc `rest/csv-export.mdx`. **Still open**: CSV **import** (a `loadcsv`/`importcsv` command validating each row via `insert_json` + `bulk_create`), an admin "Export selected → CSV" bulk action (depends on #53 bulk-action UI), and Excel (`.xlsx` via `calamine`/`rust_xlsxwriter`).

62. [ ] **Feature flags** 🟢 Low
    > Why: `is_enabled("dark_mode")` checks for A/B testing and safe deploys. Useful but not urgent — most apps can get by with env vars at v1.
    >
    > How: `umbra-features` plugin. `FeatureFlag` model (`name, enabled, rollout_percent, segments_json`). `is_enabled("flag", user_id)` checks DB + Redis cache. Defer until a real app needs percentage rollouts.

63. [ ] **API versioning** 🟢 Low
    > Why: `/v1/`, `/v2/` route prefixes for long-lived public APIs. Only needed when mobile clients lag behind server releases.
    >
    > How: `ResourceConfig::new("product").version("v2")` mounts at `/api/v2/product/`. Versioned serializers: `v1` returns `price: String`, `v2` returns `price: Money`. The framework can version at the resource level; per-field versioning is harder and usually not worth it. Defer until a public API is in production.

64. [ ] **Multi-tenancy** 🟢 Low
    > Why: Schema-based or row-level isolation for B2B SaaS. Only needed when one app serves multiple isolated customers.
    >
    > How: `TenantMiddleware` that sets the active tenant from a subdomain or header. Row-level: add `tenant_id` to every model and auto-inject `WHERE tenant_id = ?` into every QuerySet. Schema-based: `SET search_path TO tenant_123` per request. This is a large feature; defer until a concrete multi-tenant app is being built.

65. [x] **Blue-green deployments and zero-downtime migrations** 🟢 Low
       — Command shipped; full ops-guide rollout deferred. `umbra checkmigrations` walks every **pending** operation and classifies it `SAFE` / `WARNING` / `UNSAFE` via `migrate::classify_operation` (pure): creates + nullable/defaulted adds are safe; renames, alters, and NOT-NULL-no-default adds warn (not atomic with a rolling deploy / lock / backfill hazard); drops (table/column/M2M) are unsafe (data loss). Each non-safe op carries an expand-contract note. Exits non-zero on any `UNSAFE` (or any `WARNING` under `--strict`) → drops into a CI gate before `migrate`. `check_pending_safety[_in]` exposes the classified list programmatically. Read-only; applies nothing. Tests: `crates/umbra-core/tests/migration_safety.rs` (pure, every tier) + `tests/migrate.rs::check_pending_safety_classifies_a_pending_migration_off_disk` (end-to-end off disk). Doc: `migrations/checkmigrations.mdx` (incl. the expand-contract pattern). **Deferred**: the full expand-contract *ops guide* (dual-write tooling, automated backfill helpers) until the framework has users doing multiple deploys/day.

66. [x] **Static files handling and compression** 🟡 Medium
       — Shipped. All three pieces are in: (1) **`collectstatic`** — `collect_static` walks every plugin's `static_dirs()` into one output tree (`crates/umbra-core/src/static_files.rs`), wired as a CLI command. (2) The **`{% static "path" %}`** template tag — `register_static_function` adds a `static` minijinja function resolving against `settings.static_url` (`templates.rs`). (3) **gzip/brotli compression** — `AppBuilder::compression()` wraps the router in a `tower-http` `CompressionLayer` (gzip + brotli features); opt-in (off by default since a reverse proxy usually owns it), chooses the algorithm from `Accept-Encoding`, skips non-compressible/already-encoded bodies. Test: `crates/umbra-core/tests/compression.rs` (compresses when gzip accepted; passes through otherwise). **Deferred**: per-file pre-compressed `.gz`/`.br` static artifacts (the live layer covers the common case).

67. [x] **Custom template tags and filters** 🟢 Low
       — Hook + two of the three example built-ins shipped; `url` reverse-routing deferred. `Plugin::template_registrars() -> Vec<TemplateRegistrar>` returns owned `'static` closures (`Box<dyn Fn(&mut Environment) + Send + Sync>`) that mutate the minijinja `Environment` at engine-build time; `App::build` flattens them across plugins in topological order and passes them to `templates::init_with`, which stashes them in a `REGISTRARS` OnceLock so `build_env` re-applies them on every dev-mode hot-reload. Applied *after* the framework built-ins, so a plugin can override one by name. Built-in examples: `now()` (RFC3339 or chrono strftime) and `currency` (symbol + thousands grouping, sign before symbol, ISO-code fallback). Re-exported via the facade (`umbra::templates::{TemplateRegistrar, Environment}`). Tests: `crates/umbra-core/tests/template_tags.rs` (built-ins + a plugin registrar through `init_with`). Docs: `templates/custom-tags.mdx` + the `now`/`currency` rows in `templates/helpers.mdx` (and its stale "plugin filters deferred" note corrected). **Deferred**: the `{% url "name" id=... %}` reverse-route tag — needs a named-route registry + reverse resolver, a feature in its own right; logged here for when routing grows names.

68. [x] **Request/response middleware pipeline** 🟡 Medium
       — Trait + stack + wiring shipped; converting the existing tower layers deferred. `middleware::Middleware` (async, dyn-compatible via `async_trait`) has `before_request(Request) -> Result<Request, Response>` (the `Err` arm short-circuits) and `after_response(Response) -> Response`, both defaulted to pass-through. `MiddlewareStack` collects them and installs one `from_fn_with_state` layer; `before` runs in registration order, `after` in reverse (onion), short-circuit skips later middleware + the handler and only unwinds the middleware that already ran. Registered via `AppBuilder::middleware(impl Middleware)` (app-level, first) + `Plugin::middleware() -> Vec<Arc<dyn Middleware>>` (topological order). Installed after the 404 fallback (so misses are seen) and inside the host/CORS/compression layers (so security stays outermost). Facade: `umbra::middleware::{Middleware, MiddlewareStack}`, `Middleware` in the prelude, `umbra::async_trait` re-exported. Test: `crates/umbra-core/tests/middleware_pipeline.rs` (order + onion + short-circuit through a real `App::build`). Doc: `web/middleware.mdx`. **Deferred**: re-expressing the built-in CORS (gap #80) / rate-limit (gap #46) / cache-page (gap #15) layers on this trait — they already work as tower layers; the unification is cosmetic and can follow when one of them next needs a change.

69. [ ] **Database routers for multi-DB (And DB backups)** 🟢 Low
    > Why: Read-replica scaling and geo-distributed writes. Only needed at scale.
    >
    > How: `DbRouter` trait with `read_db_for::<Product>() -> "replica"` and `write_db_for::<Order>() -> "primary"`. The `QuerySet` and `Manager` already support `on(&pool)`; a router would auto-select the pool based on the operation type. Defer until read-replica scaling is a real bottleneck.

70. [x] **Compression and streaming response bodies** 🟢 Low
       — Both halves shipped. Compression landed with #66 (`AppBuilder::compression()` gzip/brotli). Streaming now: `web::StreamingResponse` (impl `IntoResponse`) builds an axum `Body::from_stream` body — `from_chunks(stream)` for an infallible `Stream<Item: Into<Bytes>>` (the row-generator case), `new(stream)` for a fallible `Result<impl Into<Bytes>, impl Into<BoxError>>` (a DB/file read that can fail mid-flight → truncated body, headers already sent). Builder: `content_type`, `attachment(name)` / `inline(name)` (Content-Disposition, filename stripped of CR/LF/`"` against header injection), `status`. Composes with the compression layer (streamed bodies compress on the fly). Facade: `umbra::web::StreamingResponse` + in the prelude. Test: `crates/umbra-core/tests/streaming_response.rs` (concatenation + headers, mid-stream error truncation, filename sanitization, defaults). Doc: `web/streaming.mdx`.

71. [x] **Management command extensions** 🟡 Medium
       — Already shipped at the trait + CLI layer. `Plugin::commands(&self) -> Vec<Box<dyn PluginCommand>>` is on the `Plugin` trait (default empty); `PluginCommand` lives in `crates/umbra-core/src/cli.rs` with a `clap::Command` builder + an async `run` handler. `umbra_core::cli::dispatch(plugins, argv)` walks every plugin's commands and routes argv to the matching handler. `umbra_cli::dispatch(app)` (the user-binary entry point in `crates/umbra-cli/src/lib.rs`) calls into it. `umbra-auth`'s `createsuperuser` and `umbra-tasks`'s `worker` are real consumers — the pattern is generalized and the surface is stable.

72. [x] **Soft deletes** 🟡 Medium
       — Shipped. New `#[umbra(soft_delete)]` struct-level attr emits `Model::SOFT_DELETE = true`. The user declares `pub deleted_at: Option<DateTime<Utc>>` on the struct themselves (derive macros can't add fields). `QuerySet::build_query_for` auto-injects `WHERE deleted_at IS NULL` on every terminal for soft-delete models; `.with_deleted()` skips the filter, `.only_deleted()` inverts it (admin trash view), `.hard_delete()` bypasses the soft path on the next `.delete()` call (GDPR purge / test cleanup). `QuerySet::delete()` rewrites to `UPDATE ... SET deleted_at = NOW() WHERE ... AND deleted_at IS NULL` (idempotency guard so re-soft-deleting doesn't bump the timestamp); `Manager::delete_instance(&row)` does the same for the typed per-row path. `bulk_post_delete` signal still fires with the affected PKs so subscribers see the same event shape regardless of the underlying SQL. Hard-delete and the with/only/hard_delete builders are also exposed on `Manager<T>` so `Post::objects().only_deleted().fetch()` works without dropping into a queryset. 4 tests in `crates/umbra-core/tests/soft_delete.rs` pin: const is set from macro, delete rewrites to UPDATE, with/only_deleted visibility flips, hard_delete after with_deleted truly purges. Non-soft models stay byte-identical (SOFT_DELETE defaults to false).

73. [ ] **Database views (materialized and regular)** 🟢 Low
    > Why: Complex reports that are too slow to compute per-request. Only needed when a real query is prohibitively expensive.
    >
    > How: `#[derive(Model)]` struct with `#[umbra(view = "...")]` that maps to `CREATE VIEW` instead of a table. The migration engine emits the view DDL. Materialized views: `#[umbra(materialized_view = "...", refresh = "1h")]`. Defer until a real app needs it.

74. [x] **Data seeding / fixture system** 🟡 Medium
       — Shipped as `umbra::fixtures::{load_fixture, dump_fixture}` plus Manager method shims. Per-model JSON-array files: hand-editable, diff-friendly, plain `[{...}, {...}]` shape with no envelope (the `backup` module already covers whole-DB dumps; fixtures are for the test-and-dev case). `Post::objects().load_fixture("tests/fixtures/posts.json").await` bulk-inserts through the same `DynQuerySet::insert_json` path the REST plugin uses, so auto_now / slug_from / validators / FK existence checks / soft-delete WHERE auto-filter all apply transparently. `dump_fixture("path.json")` writes pretty-printed JSON for round-trip. New `FixtureError` enum splits Io / Json / NotAnArray / Write / Read so callers can branch on the failure kind. Tests: 3 in `crates/umbra-core/tests/fixtures.rs` (round-trip via tempfile, non-array rejection, Manager shim). **Deferred**: `cargo run -- seed --fixture <path>` CLI subcommand (needs string-to-model resolution which the typed shape doesn't expose); `Factory` + `fake` crate for generated data; transaction-scoped per-test lifecycle (lands with `TestClient` from feature #52).

75. [x] **Admin permissions per model** 🟡 Medium
       — Shipped. New `permcheck.rs` module in `umbra-admin` bridges `umbra-permissions::has_perm_for_superuser` into the admin's handler + template surface. Codename convention follows the permissions plugin's auto-creation: `<plugin>.view_<table>` / `add_<table>` / `change_<table>` / `delete_<table>`. The plugin name comes from the admin model registry (`find_model(table) -> (plugin_name, ModelMeta)`), so a plugin's own models gate against the plugin's own permission rows. Superusers short-circuit through the upstream `has_perm_for_superuser`. **Graceful no-op when permissions aren't installed**: `permcheck::check` short-circuits to `true` when `umbra::migrate::registered_plugins()` doesn't list `"permissions"`, preserving pre-#75 staff-only behaviour for apps that haven't wired the permissions plugin. Failures from the underlying perm query log a warning and deny by default — never silently allow. Handler wiring: list / rows_fragment / detail / new_form / create / edit_form / update / delete / htmx_delete / preview_sheet / edit_sheet_handler / new_sheet / confirm_delete_dialog / sheet_create / change_password_handler / cell_edit_get / cell_edit_post all call `permcheck::require` after `require_staff`, returning 403 on missing perm so direct URL access is blocked. Template surface: `AdminPerms { can_view, can_add, can_change, can_delete }` is loaded once per render and injected into `changelist.html`, `rows_fragment.html`, `sheet_preview.html`, `detail.html`, and `form.html`. The Add button (top toolbar + empty-state CTA), per-row Edit/Delete buttons (both the macro and the rows fragment), the detail-page Edit/Delete pair, and the form's Save button are wrapped in `{% if perms is undefined or perms.can_X %}` so a missing perms ctx falls back to "show everything" (defensive for any handler that doesn't yet pass perms). The inline-cell dblclick edit also drops its hx-trigger when `can_change` is false so the affordance disappears. 2 unit tests pin the Django-style `<plugin>.<verb>_<table>` codename shape (including underscored plugin/table names). All 16 admin integration test groups still pass.

76. [ ] **Admin custom views** 🟡 Medium
    > Why: Register arbitrary handlers as admin pages: `/admin/reports/sales/`. Needed for dashboards, analytics, and one-off admin tools that don't map to a model.
    >
    > How: `AdminView` trait with `path()`, `template()`, `context()`, and `permission()`. `AdminPlugin::default().view(SalesReportView)` registers the route under `/admin/reports/sales/`. The existing route registration system already supports this; just expose it through the admin builder.

77. [ ] **Admin alerts — unified routing across SSE bell, email, webhooks** 🔴 High
    > Why: A framework that ships SSE notifications (#2), email (#39), and a task queue (#43) but no glue between them forces every app to re-build the same observability spine. "Email me when a Stripe webhook fails three times in an hour" is the canonical SaaS need; it touches every one of those plugins. Without a unified alerts surface, the developer wires `panic::catch_unwind` to `lettre::send` to a `tokio::spawn` retry loop, by hand, in every project. That's the gap.
    >
    > The same surface answers: error reporting (every handler 500 fires a `handler_5xx` alert), background-task failures (an apalis job that exceeds its retry budget fires `task_failed`), business-rule breaches (`umbra::alerts::warn("inventory_low", details)` from a save signal), and capacity thresholds (a metrics-driven `disk_full` alert from the health-check plugin). Different sources, one routing fabric.
    >
    > How — five layers:
    >
    > 1. **`Alert` value type**. A canonical struct in `umbra-alerts`:
    >    ```rust
    >    pub struct Alert {
    >        pub key:      String,                   // "stripe_webhook_failed"
    >        pub severity: Severity,                 // Info / Warning / Error / Critical
    >        pub title:    String,
    >        pub details:  serde_json::Value,        // freeform context
    >        pub source:   Option<String>,           // plugin / module that emitted it
    >        pub fired_at: DateTime<Utc>,
    >    }
    >    ```
    >    `umbra::alerts::fire(Alert)` is the single emission entry point. Auto-sources (handler 500s, task failures) wire through the same call.
    >
    > 2. **Channels**. Each channel implements:
    >    ```rust
    >    #[async_trait]
    >    pub trait AlertChannel: Send + Sync {
    >        fn name(&self) -> &'static str;
    >        async fn deliver(&self, alert: &Alert) -> Result<(), AlertError>;
    >    }
    >    ```
    >    Built-ins: `SseChannel` (admin bell, depends on #2), `EmailChannel` (depends on #39, takes `to: Vec<String>` from settings), `WebhookChannel` (POST JSON to an arbitrary URL — fits PagerDuty / Slack), `LogChannel` (always-on, writes `tracing::error!`). Third parties bring `SmsChannel`, `PagerDutyChannel`, etc.
    >
    > 3. **Routing rules**. A declarative table (`AlertRoute { match_key: GlobOrRegex, min_severity: Severity, channels: Vec<&str>, throttle: Option<Throttle> }`) registered at builder time:
    >    ```rust
    >    AlertsPlugin::default()
    >        .channel(SseChannel::default())
    >        .channel(EmailChannel::to(&["ops@example.com"]))
    >        .route(AlertRoute::all().min_severity(Warning).to("sse"))
    >        .route(AlertRoute::matching("stripe_*").min_severity(Error).to(&["sse", "email"]))
    >        .route(AlertRoute::matching("payment_failed").min_severity(Critical).to(&["email", "webhook:pagerduty"]))
    >    ```
    >    Settings-driven overrides (`UMBRA_ALERTS__STRIPE=email,sse`) trump the builder rules so ops can re-route without redeploys.
    >
    > 4. **Delivery via apalis** (depends on #43). `fire()` doesn't `await` the channel — it persists the alert to an `alert_log` table and enqueues a `DeliverAlert { alert_id, channel }` job per matched channel. Apalis workers pull jobs, call `channel.deliver(&alert)`, retry on failure with exponential backoff. The hot path (handler / signal / task) never blocks on SMTP / webhook latency. **This is the part that makes apalis a hard prerequisite** — sync delivery in the request handler would couple every email outage to user-visible 5xx.
    >
    > 5. **Admin UI**. `/admin/alerts/` lists every `alert_log` row with severity filters + a per-row "delivery history" expansion (which channels succeeded / failed, with retry counts). The dashboard's SSE bell (from #2) is just another consumer of the same `alert_log` — when `SseChannel::deliver` runs it pushes to the connected admin sessions AND inserts into a per-user `unread` table that the bell's badge counts. Closing the bell dropdown marks them read.
    >
    > **What this NOT** — a metrics system. Alerts are discrete events ("X failed, here are the details"); metrics are continuous series ("error rate is 3.2% over the last 5 minutes"). Prometheus / OpenTelemetry (#48, #49) is the right tool for thresholds and SLOs; an alert is what the metrics layer FIRES at when a threshold trips. The framework should make it easy to bridge the two (`MetricsAlertSource` adapter), but the alerts plugin doesn't own the time-series itself.
    >
    > **Settings shape worth pinning early**:
    > ```toml
    > # umbra.toml
    > [alerts]
    > # Hard ceiling — no alert above Critical ever fires more than 1×/min
    > # regardless of route config (prevents pager storms during cascading failure).
    > global_throttle_per_min = 60
    >
    > [alerts.routes.payment_failed]
    > severity = "error"
    > channels = ["email", "webhook:pagerduty"]
    > throttle = { window = "5m", max = 3 }
    >
    > [alerts.channels.email]
    > to = ["ops@example.com", "founders@example.com"]
    > # Reuses [email] block from #39
    >
    > [alerts.channels.webhook.pagerduty]
    > url = "https://events.pagerduty.com/..."
    > headers = { Authorization = "Token ..." }
    > ```
    >
    > **Triggering signals already in flight**: this entry depends on #2 (SSE), #39 (email), #43 (apalis-backed tasks). It pulls them into one coherent feature instead of three orphan pieces. When #43 ships, the alerts plugin is ~600 lines of glue + 1 model (`AlertLog`) + 1 admin view. Ship #43 first, then this becomes the demo that proves apalis is wired correctly end-to-end.
    >
    > **Stretch (post-v1)**: per-user alert preferences (`AdminUserPref::alert_subscriptions`) so each operator opts into the keys they care about; rule-based grouping (10 `stripe_webhook_failed` alerts in 5 minutes collapse to one "Stripe webhook degraded" digest); incident grouping (an alert with `incident_id: Some(...)` joins an open incident thread in the admin).

78. [ ] **Visitor analytics plugin — first-party header-driven, zero external services** 🟡 Medium
    > Why: Every web app eventually wants the same questions answered — "what browsers are my users on, where are they coming from, what % is mobile, how is traffic trending?" The Plausible / Fathom / Umami market exists because the easy answer (Google Analytics) is a privacy + compliance burden, but rolling your own means a tracking-pixel SPA, a separate ingest endpoint, and a separate dashboard. Umbra already has the request middleware surface, an ORM, an admin with widgets — the data the server SEES on every request is enough for a 90% solution. A plugin that captures headers + emits admin widgets is the lightest-weight "we have analytics" story a framework can ship.
    >
    > Critically: NO browser-side script, NO tracking pixel, NO third-party endpoint. The server already receives `user-agent`, `referer`, `accept-language`, `sec-ch-ua-platform`, `sec-ch-ua-mobile`, the request path + method + status code + duration. That's the entire feature surface. Privacy-respecting by construction — no fingerprinting, no cross-site tracking, no consent banner needed for the default config.
    >
    > How — three layers:
    >
    > 1. **Capture middleware** (`AnalyticsPlugin::default()` → `Plugin::wrap_router`). Records a `Visit` row per request:
    >    ```rust
    >    pub struct Visit {
    >        pub id:           i64,
    >        pub timestamp:    DateTime<Utc>,
    >        pub path:         String,
    >        pub method:       String,           // "GET" / "POST" / ...
    >        pub status:       i32,              // response status code
    >        pub duration_ms:  i32,
    >        pub user_agent:   Option<String>,   // raw header — parsed below
    >        pub browser:      Option<String>,   // "Chrome 149"
    >        pub os:           Option<String>,   // "Linux" / "macOS 15" / "iOS 18"
    >        pub device:       Option<String>,   // "desktop" / "mobile" / "tablet"
    >        pub referer:      Option<String>,   // raw
    >        pub referer_host: Option<String>,   // "google.com" — for aggregation
    >        pub language:     Option<String>,   // accept-language primary tag
    >        pub country:      Option<String>,   // when GeoIP feature is on
    >        pub session_id:   Option<String>,   // umbra-sessions cookie when present
    >    }
    >    ```
    >    User-agent parsing via the `woothee` crate (one dep, no regex compilation tax). `sec-ch-ua-platform` + `sec-ch-ua-mobile` headers (already structured, no parsing) win over UA parsing when both are present.
    >
    > 2. **Async write path** — DO NOT block the request thread on the INSERT. Push the row to the apalis task queue (feature #43); a worker drains it in batches. The hot path budget for the middleware is one HashMap lookup (for the session id) and one apalis enqueue. This is what makes "log every request" not a 2x latency tax.
    >
    > 3. **Admin widgets** — opt-in via `AdminPlugin::dashboard_section(visitor_widgets::all())`. Reuses the widget kinds from `documentation/docs/v0.0.1/admin/widgets.mdx`:
    >    - **Daily visits** (Line, multi-series: total / unique sessions / mobile share).
    >    - **Browser distribution** (Donut — Chrome / Safari / Firefox / Edge / Other).
    >    - **OS distribution** (Donut / Bar).
    >    - **Top referers** (Table — referer_host + count, with `?period=` chips).
    >    - **Top paths** (Table — path + visits + avg duration).
    >    - **Geographic spread** (Donut grouped by country when GeoIP is on; hidden otherwise).
    >    - **Status-code mix** (Donut — 2xx / 3xx / 4xx / 5xx share; a 5xx spike is the on-call signal).
    >    - **Live counter** (KPI — visits in the last 5 minutes; SSE-pushed via #2/#45 when those land).
    >
    > **Config knobs to pin early**:
    > ```toml
    > [analytics]
    > # Default ON for path / method / status / duration — zero-PII.
    > # OPT-IN for user_agent / referer / language — some jurisdictions
    > # consider these PII. The plugin defaults to off; the operator
    > # opts in explicitly per field so adding the plugin doesn't
    > # silently log information they didn't intend to collect.
    > capture_user_agent = false
    > capture_referer    = false
    > capture_language   = false
    > capture_ip         = false      # always off by default; GDPR-sensitive
    > capture_geoip      = false      # requires capture_ip + a GeoIP backend
    >
    > # Exclude admin / static / health paths from the visit log — no
    > # point in cluttering the dashboard with operator traffic.
    > exclude_path_prefixes = ["/admin/", "/api/", "/static/", "/healthz", "/ready"]
    >
    > # Retention — auto-delete visit rows older than N days. Defaults
    > # to 90 days because most analytics windows are quarterly or
    > # less; bounds the table size without manual housekeeping.
    > retention_days = 90
    > ```
    >
    > **What this is NOT** — a session-replay / heatmap / funnel-analysis tool. Those need browser-side instrumentation (every click, every scroll, every form field focus event) and a separate event pipeline. Posthog / FullStory / Mixpanel own that market and the plugin doesn't try to compete. The line is "what the server already sees" — that's a clean scope.
    >
    > **Dependencies**:
    >   - **#43 (apalis-backed tasks)** — the async write path. Without it the middleware would either block on every INSERT or fire-and-forget with no retry semantics (lost data on a DB blip). HARD prerequisite.
    >   - **#77 (alerts)** — natural pairing: an analytics-driven alert ("traffic spike", "5xx rate >5%") routes through the alerts plugin's channels. Optional dep; analytics ships value without it.
    >   - **`woothee` crate** — UA parsing. One dep, ~50KB, zero compile-time tax.
    >   - **Optional GeoIP** — separate `umbra-geoip` plugin that ingests a MaxMind DB once and exposes `country_for(ip)`. Feature-gated; not pulled by default.
    >
    > **Triggering signal**: a real app dropping a Google Analytics snippet because they "just need to know what browsers people use." That's the canonical wedge — the plugin gives them an answer with zero JavaScript and no Google ToS.
    >
    > **Stretch (post-v1)**: UTM-parameter capture (`?utm_source=...`) so campaign tracking works; A/B-test bucketing tied to the session cookie; export-to-CSV from the admin views (`Daily visits → Download CSV`); per-path conversion funnels (path-A then path-B within session = "conversion").

79. [ ] We need factory testing library. Related to #74. Aim: Make it easy to write tests that simulate realistic plugin submissions and model updates.

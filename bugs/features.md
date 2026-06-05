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

4. [ ] **Extended admin field widgets via macro attributes** 🟢 Low (nice-to-have, v0.2)
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

13. [ ] **`annotate()` + aggregation functions** 🔴 High
    > Why: `COUNT`, `SUM`, `AVG`, `MAX`, `MIN` are required for every dashboard, report, and analytics page. Currently the only way to get aggregates is raw SQL. This is the single biggest blocker for business-intelligence use cases.
    >
    > How: Add an `annotate` method to `QuerySet` that accepts `Vec<(String, AggregateExpr)>` and appends `SELECT ... COUNT(*) AS count, AVG(price) AS avg_price ... GROUP BY <annotated_cols>` to the generated SQL. Return a `Vec<serde_json::Value>` (or a user-supplied struct) rather than full model instances. The `AggregateExpr` enum covers `Count`, `Sum`, `Avg`, `Max`, `Min`, `StdDev`, `Variance`. Each variant knows its SQL fragment and return type. This composes with `filter()` and `order_by()` naturally.

14. [ ] **`Q` objects for complex boolean logic** 🔴 High
    > Why: `Predicate` already exists but cannot compose with `and` / `or` / `not`. Every non-trivial filter chain (`(A AND B) OR (C AND NOT D)`) currently requires raw SQL or multiple round-trips.
    >
    > How: A `Q` struct that wraps a `Predicate` plus a `QOp` enum (`And`, `Or`, `Not`). `QuerySet::filter` already accepts `Predicate`; overload it to accept `Q` and recursively flatten the tree into a sea-query `Condition` tree. The existing `&` and `|` operators on `Predicate` can be deprecated in favor of `Q::and(a, b)` and `Q::or(a, b)` for explicit composition.

15. [ ] **`exclude()` — negated filtering** 🟡 Medium
    > Why: The complement of `filter()`. Critical for soft-delete patterns (`exclude(is_deleted.eq(true))`) and for "everything except" queries. Small scope, high everyday utility.
    >
    > How: `QuerySet::exclude(predicate)` wraps the accumulated predicate in ` sea_query::Condition::not()` before emitting `WHERE NOT (...)`. One method, one test, immediate payoff.

16. [ ] **`values()` and `values_list()` — column projection** 🟡 Medium
    > Why: The primary tool for reducing memory pressure on large lists. `Post::objects().values("id", "title")` skips the 50KB `body` blob. Without this, every list view pays the cost of every column.
    >
    > How: `QuerySet::values(&[&str])` stores a `projection: Vec<String>` field. In the SQL builder, replace `SELECT *` with `SELECT "id", "title"`. In the hydration path, skip `FromRow` and instead build a `serde_json::Value::Object` directly from the row. `values_list` is the same but returns tuples (requires a generic `Vec<(T1, T2, ...)>` path, which is harder — start with `values` and defer `values_list`).

17. [ ] **`distinct()` — duplicate elimination** 🟢 Low
    > Why: Used for tag clouds and deduplication, but `GROUP BY` (via `annotate`) often covers the same use case. The Postgres-specific `DISTINCT ON` is more valuable than plain `DISTINCT`.
    >
    > How: Add `distinct()` (no args → `SELECT DISTINCT`) and `distinct_on(&[&str])` (Postgres only → `SELECT DISTINCT ON (...)`). Gate `distinct_on` behind a runtime backend check that errors on SQLite. Small change; low priority because it has workarounds.

18. [ ] **`select_related()` — FK prefetch via JOIN** 🔴 High
    > Why: Currently every list view that shows an FK's resolved value is an N+1. The `resolved` slot exists on `ForeignKey<T>` but nothing populates it in the QuerySet fetch path. This is the most visible ORM performance gap.
    >
    > How: `QuerySet::select_related("author")` adds the target table to the JOIN clause (`INNER JOIN "auth_user" ON "post"."author" = "auth_user"."id"`). In `HydrateRelated`, after the main `fetch`, walk the joined rows and populate `resolved` on every `ForeignKey` field that was requested. Requires teaching the macro's `FromRow` impl to read the joined columns (prefixed with the target table name). This is a medium-sized change but well-scoped.

19. [ ] **`prefetch_related()` — M2M and reverse-FK batch loading** 🟡 Medium
    > Why: `select_related` handles FKs via JOIN; `prefetch_related` handles M2M and reverse-FKs via two queries (parents, then children) stitched in Rust. Since M2M was just shipped, the junction-table query is possible but the QuerySet terminal doesn't know how to batch-resolve related collections yet.
    >
    > How: `QuerySet::prefetch_related("tags")` or `"comment_set"`. After the main query, issue a second query `SELECT * FROM "comment" WHERE "post" IN (1, 2, 3, ...)`, group by the FK column in a `HashMap<i64, Vec<Comment>>`, and attach to each parent instance. For M2M, the second query hits the junction table joined to the target. Requires a new `Prefetch` struct and changes to the M2M struct to accept the pre-fetched data.

20. [ ] **`bulk_update()` — mass updates without N round-trips** 🟡 Medium
    > Why: `update_values` works for a single set of values across all filtered rows, but `bulk_update` takes a `Vec<Instance>` and updates each row individually in one statement. Needed for import workflows and sync jobs.
    >
    > How: `Manager::bulk_update(instances)` builds `UPDATE "table" SET ... WHERE "id" IN (...)` with a `CASE "id" WHEN 1 THEN 'new_title_1' WHEN 2 THEN 'new_title_2' END` pattern. Postgres handles this natively; SQLite needs the same pattern or a temp-table approach. Start with the `CASE` syntax since both backends accept it.

21. [ ] **`update_or_create()` — upsert with defaults** 🟡 Medium
    > Why: The everyday pattern for idempotent importers and webhook handlers. Django returns `(instance, created)` so the caller can branch. Without this, every upsert is a manual `get()` → `match` → `create()` or `update()` block.
    >
    > How: `QuerySet::update_or_create(predicate, defaults)` runs `get(predicate)`. On `GetError::NotFound`, insert with `defaults` merged with the predicate values. On success, update the found row with `defaults`. Return `(T, bool)`. Reuse the existing `get()` and `create()` primitives.

22. [ ] **`raw()` / `raw_sql()` — escape hatch** 🟡 Medium
    > Why: Every ORM eventually needs this for complex CTEs, window functions, or vendor-specific SQL. The framework already allows raw SQL in migrations; models need the same escape hatch.
    >
    > How: `QuerySet::raw("SELECT * FROM post WHERE ...")` that returns `Vec<T>` by delegating to `sqlx::query_as` but still respects the model's `HydrateRelated` path. A thin wrapper that gives the user SQL control without losing type safety.

23. [ ] **`defer()` / `only()` — lazy column loading** 🟢 Low
    > Why: `values()` (gap #16) covers 80% of the use case. `defer`/`only` are Django sugar for "skip this column in the initial SELECT, fetch it on first access." The complexity (lazy loading via a second query on property access) is high for marginal gain.
    >
    > How: Defer until `values()` is shipped. If users still ask for it, add a `projection` field to `QuerySet` and implement `defer` as "all columns except these" and `only` as "only these columns." The lazy-fetch-on-access part is the hard bit; start without it and just treat `defer` as a `values()` alias with the full struct return type.

24. [ ] **Database functions — `Lower`, `Upper`, `Length`, `Now`, `Coalesce`, `Concat`, `Trim`** 🟡 Medium
    > Why: Case-insensitive search (`LOWER(title) = 'hello'`) and computed ordering (`LENGTH(title) ASC`) require raw SQL today. These are SQL function wrappers that compose with `filter()`, `annotate()`, and `order_by()`.
    >
    > How: A `DbFunc` enum with variants that know their SQL expression. `Lower("title")` renders to `LOWER("title")`. Allow `DbFunc` inside `Predicate` and `OrderBy` so `filter(title_lower().eq("hello"))` and `order_by(title_length().asc())` both work. Small, well-scoped change with immediate utility.

25. [ ] **Conditional expressions — `Case`, `When`, `Default`** 🟢 Low
    > Why: `CASE WHEN ... THEN ... ELSE ... END` is powerful for tiered badges and computed status fields, but it has workarounds (compute in Rust after fetching, or use raw SQL). The SQL generation is straightforward; the ergonomics in Rust are the challenge.
    >
    > How: A builder API: `Case::new().when(view_count.gt(1000), 2).when(view_count.gt(100), 1).default(0)`. Each `when` takes a `Predicate` and a `Value`. Render to `CASE WHEN ... THEN ... ELSE ... END`. Defer until `annotate()` (gap #13) is shipped, since conditional expressions are primarily useful as annotated columns.

26. [ ] **Subqueries — `Subquery` and `Exists`** 🟡 Medium
    > Why: The only clean way to do "posts that have at least one approved comment" without a JOIN. Correlated subqueries are essential for complex filtering that can't be expressed with simple column comparisons.
    >
    > How: `Subquery::new(Comment::objects().filter(post.eq(OuterRef("id"))).values("id"))` wraps an inner QuerySet and renders it as `(SELECT ...)` in the outer query. `Exists` is the same but wraps in `EXISTS (...)`. Requires `OuterRef` to reference the outer query's columns. This is a medium-sized change but well-scoped.

27. [ ] **Window functions — `RowNumber`, `Rank`, `DenseRank`, `Lead`, `Lag`, `NthValue`** 🟢 Low
    > Why: Needed for leaderboards and "top N per category," but Postgres-only (SQLite needs window-function support compiled in). The user base for this is smaller than the core QuerySet gaps.
    >
    > How: Add a `Window` struct and an `Over` clause. Gate the entire feature behind a runtime backend check that returns a clear error on SQLite. Start with `RowNumber` and `Rank` since they cover the 80% use case. Defer until `annotate()` (gap #13) is shipped, since window functions are a form of annotation.

28. [ ] **`union()`, `intersection()`, `difference()` — set operations** 🟢 Low
    > Why: Useful for merging search results from multiple models, but `OR` filtering within a single model (gap #14, `Q` objects) covers most of the same ground. Set ops across different models are rare in practice.
    >
    > How: `q1.union(q2)` emits `SELECT ... UNION SELECT ...`. Requires both QuerySets to return the same column shape, which is hard to enforce at the type level in Rust. Start with a runtime check that errors if column counts differ. Low priority.

29. [ ] **`iterator()` — memory-efficient streaming** 🟡 Medium
    > Why: For tables with millions of rows, `fetch()` collects into a `Vec` and would OOM. `iterator()` yields rows one at a time — the only viable path for exports, migrations, and bulk transforms.
    >
    > How: `QuerySet::iterator()` returns a struct implementing `Stream` (or an async `Iterator` if `Stream` is too heavy). Under the hood, use `sqlx::query_as().fetch()` which yields rows as they arrive. The challenge is lifetime management — the `Stream` must hold the `sqlx::Pool` reference and the query state. This is medium-sized but critical for large datasets.

30. [ ] **Reverse relation accessors — `post.comment_set`, `category.post_set`** 🔴 High
    > Why: Django auto-generates `related_name` managers on the "one" side of a FK. Umbra has `ForeignKey<T>` on the child but no `QuerySet<T>` on the parent. A `Category` has no ergonomic way to get `Vec<Post>` without writing `Post::objects().filter(category.eq(id)).fetch()`. This is the biggest ergonomics gap in the ORM.
    >
    > How: The macro already emits `ModelMeta` with `fk_target` information. Add a `related_managers()` method to `ModelMeta` that emits `ReverseFK` descriptors. At runtime, `category.post_set()` returns a `QuerySet<Post>` pre-filtered to `post::CATEGORY.eq(category.id)`. This is a medium-sized macro + runtime change but transforms the ORM's ergonomics.

31. [ ] **JSONField / JSONB query operations** 🟡 Medium
    > Why: `serde_json::Value` stores as JSONB but cannot be queried by path or checked for containment. This blocks any schema-less or semi-structured data model.
    >
    > How: For Postgres: `metadata__has_key("name")` → `metadata ? 'name'`, `metadata__path("a", "b")` → `metadata #> '{a,b}'`. For SQLite: fall back to `json_extract` (available in modern SQLite). Add JSON-specific lookup operators to the REST filter parser (gap #29) so `?metadata__has_key=name` works out of the box.

32. [ ] **ArrayField operations** 🟢 Low
    > Why: Postgres arrays are powerful, but most use cases (tags, permissions) are better served by a junction table (M2M) or a JSONB column. Only reach for this if a real app needs `tags__contains` containment checks on a native array.
    >
    > How: Defer until a concrete app demands it. If needed, add `ArrayField<T>` with Postgres-specific DDL (`TEXT[]`, `INTEGER[]`) and operators (`@>`, `&&`, `array_length`). Gate behind backend check; SQLite falls back to JSONB storage with runtime emulation.

33. [ ] **Full-text search integration** 🟡 Medium
    > Why: A content-heavy app (blogs, documentation) cannot ship with only exact `LIKE` search. Postgres `to_tsvector` / `ts_rank` and SQLite FTS5 are the standard backends.
    >
    > How: Add `SearchField` (Postgres-only at v1) that creates a `tsvector` column via GIN index. `Post::objects().filter(body__search("rust async"))` emits `to_tsvector('english', body) @@ plainto_tsquery('rust async')`. For SQLite, ship an FTS5 virtual table as a fallback. This is a medium-sized plugin-level feature, not a core ORM change.

34. [ ] **`in_bulk()` — fetch many rows by PK into a HashMap** 🟢 Low
    > Why: Convenience method for when you have a list of IDs from a cache or external system. `fetch()` + manual HashMap construction is the workaround today.
    >
    > How: `Post::objects().in_bulk([1, 2, 3])` builds `SELECT * FROM post WHERE id IN (1, 2, 3)` and collects into `HashMap<i64, Post>`. One method, one test. Small scope; defer until someone asks for it.

35. [ ] **`explain()` — query plan inspection** 🟡 Medium
    > Why: Essential for debugging slow queries and verifying index usage. Django's `queryset.explain()` is the first tool a developer reaches for when a page is slow.
    >
    > How: `Post::objects().filter(...).explain()` returns the database's execution plan as a `String` (SQLite `EXPLAIN QUERY PLAN`) or `serde_json::Value` (Postgres `EXPLAIN (FORMAT JSON)`). Add an `explain: bool` flag to `QuerySet` that prepends the explain prefix before executing. Simple change, high debugging value.

36. [ ] **Date/time extract functions — `year`, `month`, `day`, `week_day`** 🟡 Medium
    > Why: `Post::objects().filter(created_at__year.eq(2026))` is needed for archive pages, monthly reports, and calendar views. Currently requires raw SQL or filtering in Rust after fetching all rows.
    >
    > How: Add `year`, `month`, `day`, `hour`, `minute`, `week_day` as `DateTimeCol` extension methods that return `DbFunc` expressions. `created_at.year()` renders to `EXTRACT(YEAR FROM created_at)`. Postgres and SQLite both support `strftime` / `EXTRACT`. Well-scoped.

37. [ ] **`earliest()` / `latest()` — convenience wrappers** 🟢 Low
    > Why: Small sugar but used constantly in activity feeds and audit trails. `first()` + `order_by()` already covers this; these are just shorter names.
    >
    > How: `Post::objects().earliest(created_at)` = `order_by(created_at.asc()).first()`. `latest(created_at)` = `order_by(created_at.desc()).first()`. Two one-line methods. Ship as a quick win if someone wants a small task.

38. [ ] **Signals — `pre_save`, `post_save`, `pre_delete`, `post_delete`, `m2m_changed`** 🔴 High
    > Why: Hooks that fire around ORM operations so plugins and user code can react: auto-generate a slug on `pre_save`, clear a cache on `post_delete`, send an email on `post_save`. The permissions plugin currently auto-creates standard permissions on boot; signals would let it do that reactively when a new model is registered. This is a foundational extensibility mechanism.
    >
    > How: A `Signal` type in `umbra-core/src/signals.rs` (already exists but not wired). Define `ModelEvent { kind, table, pk, before, after, actor }`. Fire from `QuerySet::create`, `update_values`, `delete`, and `bulk_create` terminals. The `actor` field comes from a tokio task-local set by an axum middleware (gap #48, structured logging, can share the same task-local infrastructure). This unlocks gaps #1 (logs plugin), #2 (notifications), and #77 (ORM audit trail).

38.1 [ ] **Atomic transactions at the ORM level — opt-in via builder** 🔴 High
    > Why: Manual `begin` / `commit` / `rollback` via `umbra::db::transaction()` works today, but every multi-write endpoint (nested REST creates, admin inlines, bulk imports) has to hand-roll the same transaction wrapping. Django's `with transaction.atomic():` context manager makes this invisible. Umbra needs an equivalent so that `POST /api/order/` with nested `items` (feature #58) can create the parent and all children in one transaction without the REST handler knowing about `sqlx::Transaction`. Without this, a failure mid-way leaves half-written rows in the database.
    >
    > How: Two layers — an **ORM-level** convenience and a **framework-level** default.
    >
    > **ORM layer**: `QuerySet::atomic()` wraps the terminal call in a transaction. `Post::objects().atomic().create(post).await` starts a transaction, runs the insert, commits on `Ok`, rolls back on `Err`. `Manager::bulk_create_atomic(instances)` does the same for the multi-row path. This uses the ambient `DbPool` to `BEGIN` against the correct backend, so the caller never types `pool` or `Transaction`.
    >
    > **Builder layer (opt-in)**: `App::builder().atomic_transactions(true)` sets a global default that makes *every* ORM terminal (`create`, `update_values`, `delete`, `bulk_create`) run inside a transaction unless explicitly opted out with `.non_atomic()`. This is the safe-by-default posture: a framework that claims "secure by default" should also be "transaction-safe by default." The opt-out exists for high-throughput paths where the caller manages batching themselves (e.g. a seed script that already wraps 1000 inserts in one outer transaction).
    >
    > **REST layer**: `ResourceConfig::new("order").atomic_writes(true)` opts a single resource into the transaction wrapper. The REST handler's `create` path calls `Manager::create_atomic()` instead of `create()` when the flag is on. Nested writes (feature #58) inherit the outer transaction automatically — the junction/child inserts share the same `sqlx::Transaction` because the ORM's `atomic()` path stashes the active transaction in a tokio task-local or a `QuerySet` field.
    >
    > **Why opt-in at the builder?** Because `DbPool` is resolved ambiently, and a global default would silently change the behaviour of existing code that already does its own transaction management. `App::builder().atomic_transactions(true)` is an explicit contract: the developer says "I want every write protected." Without the flag, the framework stays exactly as it is today — manual control, no surprises.

---

## General Framework — What is still missing to call Umbra "feature-complete"

These are the cross-cutting capabilities that turn a framework from a neat ORM demo into a platform you can ship a SaaS on.

39. [ ] **Email sending — SMTP and API backends** 🔴 High
    > Why: Password resets, notifications, and transactional emails are table stakes. Without this, every app re-implements SMTP or pulls in `lettre` directly.
    >
    > How: `umbra-email` plugin with `EmailMessage::builder().to("...").subject("...").body("...").send().await`. Backends: SMTP (lettre), SendGrid, Mailgun, AWS SES. Integrate with the task queue (gap #43) for async sending. The plugin should be small — mostly a typed wrapper around `lettre` plus a backend trait.

40. [ ] **File uploads and multipart handling** 🔴 High
    > Why: Avatars, attachments, CSV imports, and image uploads are universal. The REST plugin currently doesn't handle `multipart/form-data`; there is no `FileField` type for models.
    >
    > How: Add `Multipart` extractor to `umbra::web`. Stream uploads to disk or memory. Add `FileField` to the ORM that stores a path/URL string. The admin already has file upload UI from gap #51; wire it to the new field type.

41. [ ] **Media storage — local, S3, R2, GCS** 🟡 Medium
    > Why: User-generated content needs a storage backend abstraction. `FileField` (gap #40) stores a path; this feature decides whether that path is local or remote.
    >
    > How: A `Storage` trait with `store(path, bytes) -> Url` and `url(path) -> Url`. Implementations: `LocalStorage`, `S3Storage` (via `aws-sdk-s3` or `rust-s3`). Admin renders `ImageField` values as `<img>` thumbnails by calling `storage.url(path)`. Depends on gap #40.

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

45. [ ] **WebSockets / SSE — real-time push** 🟡 Medium
    > Why: Notifications, chat, live dashboards, and collaborative editing need real-time channels. This pairs with gap #2 (notifications plugin) for the full Phoenix-like experience.
    >
    > How: `umbra-realtime` plugin with `WebSocketHandler` and `EventStream` traits. Room-based broadcasting (`room("chat:123").send(msg)`). Built on `tokio-tungstenite` for WebSockets and SSE via axum's built-in support. Depends on gap #38 (signals) to broadcast model changes to connected clients.

46. [ ] **Rate limiting and throttling** 🟡 Medium
    > Why: Per-IP, per-user, and per-endpoint limits are essential for public APIs and login brute-force protection.
    >
    > How: Middleware that checks a Redis-backed counter per key (`ip:192.168.1.1`, `user:123`). Return `429 Too Many Requests` with `Retry-After`. Configurable via `App::builder().rate_limit(...)` or per-route decorators. Use `redis::expire` for TTL-based windows.

47. [ ] **Health checks and readiness probes** 🟡 Medium
    > Why: Kubernetes and load balancers require `GET /healthz` (liveness) and `GET /ready` (readiness). Without these, the framework is invisible to infrastructure.
    >
    > How: Built-in routes at `/healthz` (always returns 200 if the process is running) and `/ready` (checks DB connectivity, migration status, and any plugin-specific health checks). Returns JSON with `status: "ok"` and per-dependency details. Add `Plugin::health_check()` optional hook.

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

55. [ ] **Admin filters and date hierarchy** 🟡 Medium
    > Why: Sidebar filters for choices, FKs, booleans, and date ranges. Date hierarchy drill-down (`2026 > June > 04`). Search spanning multiple fields. These make the admin usable with thousands of rows.
    >
    > How: The `list_filter` API already exists in `AdminConfig`. What's missing is the frontend rendering in `templates/list.html`: filter facets in the sidebar, date hierarchy as collapsible links, and search as a top-bar input. Mostly a template + HTMX change.

56. [ ] **Admin dashboard widgets (built-in)** 🟡 Medium
    > Why: See feature #7. This is the general-framework framing of the same capability — "Recent orders", "Pending comments", "New users today" as default dashboard cards.
    >
    > How: Same as #7. Hardcoded widgets first, then a `Widget` trait for custom ones. Render on the admin index page as a grid.

57. [ ] **Admin autocomplete fields** 🟢 Low
    > Why: For FKs with thousands of options, the current async combobox loads all rows. An autocomplete that queries `/api/product/?search=...` as the user types is needed for production datasets.
    >
    > How: Replace the current "load all rows" combobox with a search-as-you-type input that hits the REST API's `?search=` endpoint. The REST plugin already supports search (gap #29). The admin just needs to render a text input with HTMX `hx-get` to the search endpoint and render dropdown results. Small frontend change.

58. [ ] **REST plugin — nested serializers and writable nested objects** 🔴 High
    > Why: `POST /api/order/` with nested `items: [{product: 1, quantity: 2}]` creates the `Order` and its `OrderItem` children in one transaction. The most common DRF feature request. Currently the REST plugin is flat: one table per endpoint.
    >
    > How: `ResourceConfig::nested("items", ResourceConfig::new("order_item"))` declares a nested resource. The create handler reads the nested array from the JSON body, starts a transaction, inserts the parent, then inserts each child with the parent's PK. Return the full nested object in the response. This is a medium-sized change but high-impact.

59. [ ] **REST plugin — authentication integration** 🔴 High
    > Why: REST endpoints currently have no auth gates. `RestPlugin::resource(...).permission(IsAuthenticated)` should protect endpoints. The OpenAPI spec should publish `securitySchemes` so the playground knows which endpoints need a Bearer token.
    >
    > How: The `Authentication` and `Permission` traits already exist in `umbra-rest`. What's missing is wiring: the `list`/`retrieve`/`create`/`update`/`delete` handlers should call `permission.check(&identity)` before executing. Also add `securitySchemes` to the OpenAPI spec output so the playground can auto-detect auth requirements.

60. [ ] **REST plugin — action endpoints with custom serializers** 🟡 Medium
    > Why: `@action` endpoints (e.g. `POST /api/order/1/ship/`) need custom input/output shapes, not just the model's fields.
    >
    > How: Extend `Action` to carry an optional `input_schema: JsonSchema` and `output_schema: JsonSchema`. The macro or builder validates the input body against the schema before calling the handler. The OpenAPI generator includes the custom schema in the spec. This makes custom actions first-class in the playground.

61. [ ] **Data import / export — CSV and Excel** 🟡 Medium
    > Why: Admin action "Export selected rows to CSV" and management command `cargo run -- importcsv` are essential for content migration, bulk updates, and reporting.
    >
    > How: Use `csv` crate for CSV and `calamine` for Excel. Add `AdminModel::export_formats(&["csv", "xlsx"])`. The export handler streams rows to a tempfile and returns a download response. The import command reads a CSV, validates each row against the model's fields, and inserts via `bulk_create`.

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

65. [ ] **Blue-green deployments and zero-downtime migrations** 🟢 Low
    > Why: Expand-contract migration pattern for teams that deploy multiple times per day. Only relevant at significant scale.
    >
    > How: A management command that validates a migration is safe (no `DROP COLUMN` on non-nullable without default, no `RENAME TABLE` without rename detection). Document the expand-contract pattern in the ops guide. Defer until the framework has production users doing multiple deploys per day.

66. [ ] **Static files handling and compression** 🟡 Medium
    > Why: `STATIC_URL` + `STATIC_ROOT` equivalent, `gzip`/`brotli` compression, and `{% static "logo.png" %}` template tag. The `umbra-static` plugin exists but needs a `collectstatic` command and compression.
    >
    > How: `cargo run -- collectstatic` that walks every plugin's `static/` directory and copies files to a single output directory. Add `gzip` and `brotli` middleware (using `tower-http::compression`) for responses. The template tag is a small addition to the minijinja environment.

67. [ ] **Custom template tags and filters** 🟢 Low
    > Why: `{% now "Y-m-d" %}`, `{% url "product_detail" id=product.id %}`, `{{ price|currency:"USD" }}`. Django has hundreds of built-in tags; Umbra has zero custom ones.
    >
    > How: Add a `Plugin::register_tags(&mut Environment)` hook that runs at template init time. Plugins contribute tags and filters by mutating the minijinja `Environment`. Start with `now`, `url`, and `currency` as built-in examples. Defer until a real app needs custom template logic.

68. [ ] **Request/response middleware pipeline** 🟡 Medium
    > Why: A typed `Middleware` trait that wraps handlers with `before_request` and `after_response` hooks. Currently middleware is ad-hoc (axum `Layer`); a framework-level contract makes composition predictable.
    >
    > How: Define `Middleware` trait with `before_request(Request) -> Request` and `after_response(Response) -> Response`. A `MiddlewareStack` that runs all registered middleware in order. Convert existing CORS (gap #80), rate limiting (gap #46), and cache page (gap #15) to implement this trait. This unifies the middleware surface.

69. [ ] **Database routers for multi-DB** 🟢 Low
    > Why: Read-replica scaling and geo-distributed writes. Only needed at scale.
    >
    > How: `DbRouter` trait with `read_db_for::<Product>() -> "replica"` and `write_db_for::<Order>() -> "primary"`. The `QuerySet` and `Manager` already support `on(&pool)`; a router would auto-select the pool based on the operation type. Defer until read-replica scaling is a real bottleneck.

70. [ ] **Compression and streaming response bodies** 🟢 Low
    > Why: Streaming bodies for large CSV exports or file downloads without loading everything into memory. Currently responses are fully buffered strings.
    >
    > How: `Response` builder with `.gzip(true)` or `.brotli(true)` using `tower-http::compression`. For streaming, use axum's `Stream` body type instead of `String`. Defer until a real app generates multi-megabyte responses.

71. [ ] **Management command extensions** 🟡 Medium
    > Why: A `Command` trait so plugins can register CLI subcommands. `umbra-auth` already does this with `createsuperuser`; the pattern should be generalized.
    >
    > How: Define `Plugin::commands() -> Vec<Box<dyn Command>>`. The CLI dispatcher walks all plugins and matches argv against contributed commands. This makes `createsuperuser`, `tasks-worker`, and future plugin commands follow the same pattern.

72. [ ] **Soft deletes** 🟡 Medium
    > Why: `#[umbra(soft_delete)]` adds `deleted_at: Option<DateTime<Utc>>`. `QuerySet::filter(...)` auto-excludes soft-deleted rows unless `.with_deleted()` is called. Needed for audit trails and accidental-deletion recovery.
    >
    > How: The macro detects `#[umbra(soft_delete)]` and adds the `deleted_at` column. `QuerySet::filter` auto-injects `deleted_at IS NULL` unless `.with_deleted()` is called. `instance.delete()` sets `deleted_at` instead of issuing `DELETE`. Small, well-scoped change.

73. [ ] **Database views (materialized and regular)** 🟢 Low
    > Why: Complex reports that are too slow to compute per-request. Only needed when a real query is prohibitively expensive.
    >
    > How: `#[derive(Model)]` struct with `#[umbra(view = "...")]` that maps to `CREATE VIEW` instead of a table. The migration engine emits the view DDL. Materialized views: `#[umbra(materialized_view = "...", refresh = "1h")]`. Defer until a real app needs it.

74. [ ] **Data seeding / fixture system** 🟡 Medium
    > Why: `cargo run -- seed --fixture users.json` loads fixture files. `seed_blogs()` in `examples/shop/src/main.rs` is ad-hoc; a formal system makes it reusable.
    >
    > How: `Fixture` trait with `load(path)` and `dump(path)` methods. Use `fake` crate for generating realistic data. A `Factory` macro that derives `Fixture` and provides `fake_user()`, `fake_post()`. Integration with the test system (gap #52) so fixtures auto-load per test.

75. [ ] **Admin permissions per model** 🟡 Medium
    > Why: Not just global `is_staff`, but `can_view_product`, `can_change_order` enforced in the admin UI. The permissions plugin already creates these rules; the admin doesn't yet check them before rendering actions.
    >
    > How: In the admin handlers, call `has_perm(user_id, "app.change_model")` before rendering edit buttons or processing POSTs. Hide the "Add" button if `add_` permission is missing. Return 403 on direct URL access without the permission. Mostly wiring — the permission system already exists.

76. [ ] **Admin custom views** 🟡 Medium
    > Why: Register arbitrary handlers as admin pages: `/admin/reports/sales/`. Needed for dashboards, analytics, and one-off admin tools that don't map to a model.
    >
    > How: `AdminView` trait with `path()`, `template()`, `context()`, and `permission()`. `AdminPlugin::default().view(SalesReportView)` registers the route under `/admin/reports/sales/`. The existing route registration system already supports this; just expose it through the admin builder.

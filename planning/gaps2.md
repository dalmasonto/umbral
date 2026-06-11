# Seen/Known gaps - Continued from @gaps.md

> `[x]` write-ups are archived verbatim (same numbers) in `archive/gaps2-done.md`. Only open `[ ]` and partial `[~]` entries keep full text here.

1. [x] Save-feedback toast in the admin sheet — SHIPPED in commit `d2916d5` as gaps2 #13. — archived

2. [ ] Can we have a posthog wiring maybe as a plugin, or a way of linking such logging systems into umbra

3. [x] Change-password dialog extracted to an HTML `<template>` — SHIPPED in commit `5b22cc5`. — archived

4. [~] **wrapper.html growing too large — JS extraction shipped, CSS already external, widget-specific splits deferred.** Commit `e7747fa` extracted ~1080 lines of inline `<script>` IIFE blocks (lines 500-1178, 1229-1491, 1493-1634 pre-fix) to a single external `plugins/umbra-admin/src/assets/admin.js`, served via the existing `Plugin::static_files()` hook at `/admin/static/admin.js`. wrapper.html shrunk 1636 → 563 lines (66% smaller). One `umbraAdminBase` bootstrap inlined to carry the Jinja-substituted `{{ admin_base }}` into the external file; the 6 prior `{{ admin_base }}` JS call sites became `umbraAdminBase + '/...'` concat. Live-verified: `/admin/static/admin.js` HTTP 200, 43,420 bytes, `application/javascript`. `plugins/umbra-admin/tests/phase4_dashboard.rs::admin_js_served_as_external_asset_not_inline` pins all four (asset served, bootstrap in wrapper, external script tag in wrapper, old IIFE comments absent).

    What stays inline (correctly): pre-paint theme bootstrap (must run before paint to avoid theme flash), the `window.umbra` stub (must run before child-template inline scripts), the `<script id="tailwind-config">` block (read by the Tailwind CDN runtime), the third-party CDN tags (htmx, lucide, apexcharts).

    CSS side is also addressed — the admin's compiled stylesheet has lived at `/admin/static/admin.css` since the `StaticFile` mechanism landed (see `plugins/umbra-admin/build.rs`). There's no remaining `<style>` block to extract — only the inline `:root { color-scheme: light; }` + `body { font-family: Inter, ... }` early-paint rules in wrapper.html.

    What's deferred to separate follow-up commits:
    - Splitting admin.js into per-feature bundles (sheets, palette, charts, ...) — would benefit from measurement first (perf optimization, not the original gap symptom).
    - Self-hosting the third-party CDN deps (htmx, lucide, apexcharts) — separate decision; the gap mentions them but they're a different category of cleanup.
    - Per-widget `<script>` bundles — only relevant once users register many custom widgets; today the framework's widget catalog is small enough that one file is the right shape.

    Original directive preserved below:

5. [ ] Ability to register custom widgets, ie with full html, js, and css. Its like self contained widgets or widgets that extend on top of the current setup ie tailwind widgets with apex charts.

6. [ ] Ability to create more dynamic widgets right from the admin. This is inline with the ability to create dynamic admin pages ie /admin/page/<reports> which holds specific data like different report widgets etc. This is captured in `../features.md #4, #56, #76` and

7. [x] Wire `AuthPlugin::with_user_in_templates()` — archived

8. [ ] **Bootstrap project layout convention** — `umbra startproject <name>` should scaffold the app with the per-concern layout we landed on in `examples/shop` (commits `32cd1c1` extracted seed + widgets; `2d3693b` extracted views). The shop went from a 1320-line `main.rs` to 243 by following one repeated shape across three sibling modules:

    ```
    src/
      main.rs         # App builder + route table + boot helpers (~200-300 lines)
      auth.rs         # Custom authenticators (if any)
      views/
        mod.rs        # re-exports + shared `internal_error` helper
        public.rs     # public/unauth handlers
        account.rs    # auth-gated handlers
        # ...one file per resource grouping
      seed/
        mod.rs        # `all()` orchestrator that pins dependency order
        credentials.rs
        products.rs
        demo_data.rs
        # ...one file per concern; each step idempotent
      widgets/
        mod.rs        # per-kind re-exports
        aggregates.rs # helpers used by widgets (counts/sums per window)
        cards.rs
        charts.rs
        tables.rs
        # ...one file per widget kind
      plugins/
        <name>/       # local app plugins (existing convention from shop)
    ```

    Three properties this gives for free:
      - **`main.rs` reads like a table of contents.** Every call site is `<bucket>::<name>`; "which file holds it" is a lookup, not memorisation.
      - **`mod.rs` files are the discoverability layer.** Open one and you see the whole subsystem's surface in 20-30 lines. The `seed::all()` orchestrator pattern doubles as documentation of dependency order (e.g. "demo_data must run before blogs because the comments reference user ids 2 + 3 that demo_data creates").
      - **Per-concern submodules stay small.** No file we extracted exceeds ~320 lines (`demo_data.rs` is the heaviest). The Django/Rails convention of "one app, one file per concern" carried over cleanly.

    **What the scaffolder should ship**:
      - Empty `views/{mod.rs, public.rs}` (no `account.rs` by default — added when auth lands).
      - Empty `seed/{mod.rs, credentials.rs}` with `mod.rs::all()` calling just `credentials::test_credentials()`.
      - Empty `widgets/{mod.rs, cards.rs}` with one builtin re-export so a fresh dashboard isn't empty.
      - `mod.rs` files commented to explain "this is the orchestrator / re-export layer" so the developer knows where to slot new submodules.

    **What it should NOT do**: forbid other layouts. The framework reads `main.rs` directly — nothing in the runtime cares whether handlers live in `views/`, `handlers/`, `endpoints/`, or just inline in `main.rs`. The scaffold is a recommended convention so the user opens day-one to a project that already scales past the 1000-line mark; they remain free to flatten or restructure if they have different preferences.

    See `examples/shop/src/` for the canonical reference.

9. [x] `render_500` swallows secondary template errors silently — archived

10. [ ] **Middleware contract — proper plugin + app-level middleware system, not ad-hoc `wrap_router` closures.** Today (commit `bd48bf8`) `AuthPlugin::with_user_in_templates()` mounts `user_context_layer` via `Plugin::wrap_router(router) -> Router`. That works for one middleware but the shape doesn't scale:

    - **Order is invisible.** Each plugin's `wrap_router` runs in topological plugin order and the order they wrap matters (auth must be outside-of CSRF outside-of rate-limit outside-of session, etc.). Today nobody can see the resulting stack without reading every plugin's closure.
    - **No user-side middleware surface.** The user has no `App::builder().middleware(MyMiddleware)` to add their own rate-limit / request-id / cors / auth-shield. The escape hatch is `Routes::layered(method, path, handler.layer(L))` per-route — fine for one route, untenable for "rate-limit every API endpoint."
    - **No conflict / duplication detection.** Two plugins independently calling `router.layer(CorsLayer::permissive())` silently stack two CORS layers; the request goes through both and the response gets two `Access-Control-Allow-Origin` headers. The framework should detect "this kind of middleware is already mounted" and warn.
    - **No semantic ordering.** A real middleware system has slots: `PreAuth` (request-id, tracing), `Auth` (the user-context layer), `PostAuth` (rate-limit by user, CSRF), `Outer` (CORS, compression). Plugins declare which slot their middleware belongs in; the framework assembles the stack in a documented order. `wrap_router` collapses all of that into one un-orderable closure.
    - **No introspection.** The dev-mode 404 page lists routes via `Plugin::route_paths()`; there's no equivalent for "what middleware is mounted on this request path?" That's the kind of thing you grep CI logs for when an unexpected 401 lands.

    **Proposed shape**:

    ```rust
    pub enum MiddlewareSlot {
        // Outermost — runs first on request, last on response
        Outer,         // CORS, compression
        Logging,       // tracing, request-id stamping
        Auth,          // session lookup, identity hydration
        PostAuth,      // CSRF check, rate-limit by user
        // Innermost — closest to the handler
    }

    pub trait Middleware: Send + Sync + 'static {
        fn name(&self) -> &'static str;            // for introspection
        fn slot(&self) -> MiddlewareSlot;
        fn layer(&self) -> tower::util::BoxLayer<Body, Body>;
        fn route_filter(&self) -> RouteFilter { RouteFilter::All }  // restrict to a subset
    }

    // On Plugin:
    fn middleware(&self) -> Vec<Box<dyn Middleware>> { Vec::new() }

    // On App::builder():
    pub fn middleware(self, m: impl Middleware) -> Self { ... }
    ```

    Build phase: collect every plugin's `middleware()` + every app-level `.middleware(...)` call, group by slot, sort within slot by registration order, wrap the router slot-by-slot in `Outer → ... → PostAuth` order. Duplicate detection by `name()` (warns or errors based on a builder flag). Introspection at `/admin/_debug/middleware` (dev-only) shows the resolved stack for any path.

    **What this unblocks**:
      - `umbra-ratelimit` plugin (gap from features.md #46) plugs in cleanly: `RateLimitMiddleware::per_user(60_per_min)` with slot=`PostAuth`.
      - User adds their own: `App::builder().middleware(RequestIdMiddleware::default())` — no Plugin trait needed.
      - The current `Plugin::wrap_router` becomes deprecated in favour of `Plugin::middleware()` — auth's `user_context_layer` migrates to `UserContextMiddleware { slot: Auth }`. `wrap_router` stays as the escape hatch for the rare "I need to wrap the whole router non-linearly" case.

    **Reference**: tower's existing `Layer` trait + `ServiceBuilder` already do the composition; the gap is the umbra-side trait + Plugin + App::builder surface around it. Maybe ~300 lines across umbra-core (trait + builder + slot enum + introspection endpoint) + per-plugin migrations of the existing `wrap_router` users (just AuthPlugin today).

11. [x] Persist all admin UI state into `AdminUserPref` — filters, sort orders, page sizes, search, per-table preferences. — archived

12. [~] **Admin form errors — DynError enum landed; per-field template rendering still to do.**

    **Part 1 (shipped):** `DynError` in `crates/umbra-core/src/orm/dynamic.rs` lifted from `pub type DynError = sqlx::Error;` alias to a real enum `pub enum DynError { Write(WriteError), Sqlx(sqlx::Error) }` with `From<sqlx::Error>` + `From<WriteError>` + `Display` + `Error` impls. Form-coercion failures in `insert_form` / `update_form` / `update_one` now emit `DynError::Write(WriteError::Validator { field, message })` carrying the offending column name, replacing the pre-fix `sqlx::Error::Protocol("umbra::orm::write: <message>")` string-flatten that lost the per-field hint.

    `AdminError` (in `plugins/umbra-admin/src/error.rs`) gained a `Write(WriteError)` variant + `From<WriteError>` + `From<umbra::orm::DynError>` impls so admin handlers' `?` chains route Write → Write and Sqlx → Sqlx. `sanitise_form_error` (`util.rs`) gained the matching `Write` arm — renders the validator message directly with capitalisation matching the legacy sqlx::Error::Protocol path. The REST plugin's `ApiError` gained the parallel `From<DynError>` impl so its `?` chains stay clean.

    Every admin call site that constructed `AdminError::Sqlx(e)` against a now-`DynError` value was lifted to `AdminError::from(e)` so the enum dispatch is preserved (3 sites: `crud.rs::delete`, `crud.rs::bulk_delete`, `sheet.rs::change_password`, plus `inline_edit.rs` which already used `AdminError::Sqlx(e)` for a sanitise call now goes through the From impl too).

    Verification: 3 new tests in `crates/umbra-core/tests/dyn_error_enum.rs` pin the contract (`form_coercion_failure_surfaces_as_dyn_error_write_with_field_name`, `update_form_coercion_failure_also_surfaces_as_dyn_error_write`, `dyn_error_lifts_via_from_for_both_arms`). Full workspace `cargo test`: 1214 passed, 0 failed.

    **Part 2 (deferred to its own PR):** the form template (`form.html` + `_macros/sheet.html` + `_macros/field_editor.html`) doesn't yet consume the `WriteError::field_errors()` map — `sanitise_form_error` still flattens to a single string at the top of the form. The plumbing is now in place; threading `field_errors: HashMap<String, Vec<String>>` into the template context is a focused admin-template change without further ORM work. This is what unblocks gaps2 #19 (`Form<T>` extractor) too — same template surface, same context key.

12. ~~ (the originally-open description below kept for archive trail)

    Today's commit `5b163ab` made the admin surface the message text from `WriteError` (e.g. "A record with this `user` already exists.") instead of the blanket "database error" — but the message is delivered as a single string at the top of the form. The REST plugin returns the same write-error as a structured per-field map:

    ```jsonc
    // POST /api/customer/ with a duplicate FK → 400
    {
      "user": ["A row with this value already exists."],
      "phone": ["This field is required."]
    }
    ```

    The admin form template should render the same shape — each `<input>` knows its column name; render any messages for that key directly beneath the field as a small red span, the way DRF + Django admin both do. A "Customer" form with three FK fields and a UNIQUE violation is currently ambiguous ("which FK collided?"); per-field rendering removes the guess.

    **Root cause**: `DynQuerySet::insert_form` / `update_form` return `Result<_, DynError = sqlx::Error>`. The validator's structured `WriteError` (already a real enum with `field_errors()` + `non_field_errors()` accessors — see `crates/umbra-core/src/orm/write.rs:113-241`) gets flattened to `sqlx::Error::Protocol("umbra::orm::write: <message>")` at the boundary, losing the per-field map. The admin form handler then has nothing to render per-input — only the joined string.

    **Fix shape**:
      - Promote `DynError` from a `pub type DynError = sqlx::Error;` alias (`dynamic.rs:45`) to a real enum: `pub enum DynError { Write(WriteError), Sqlx(sqlx::Error) }` with `From<sqlx::Error>` for backwards compat. `insert_form` / `update_form` / `insert_json` / `update_json` return the structured error directly.
      - Extend `AdminError` with a `Write(WriteError)` variant. The admin form-submit handlers branch on it: render `field_errors()` per-input + `non_field_errors()` at the top.
      - `form.html` + `_macros/sheet.html` + `_macros/field_editor.html` accept a `field_errors: HashMap<String, Vec<String>>` context map and render messages under each input that has a key.

    Same architectural footprint as REST's existing 400 response shape — REST already builds this map (`umbra-rest/src/handlers.rs::write_error_to_drf_body`); the admin should reuse the same `WriteError::field_errors()` accessor.

    **Demo case to fix**: a Customer form with three FK fields (`user`, `default_shipping`, `default_billing`) and a UNIQUE-on-user violation. Today: "A record with this `user` already exists." at the top — fine. But if the user ALSO submitted an unrelated FK that points at a deleted row, the second error is invisible until they fix the first. Structured per-field rendering shows both at once.

    **Related**: ties into gaps2.md #10 (middleware contract) — a unified error-rendering middleware could format both REST + admin from the same `WriteError` source. Also ties into the "two paths for same operation" footnote on commit `5b163ab` (form path vs. JSON path divergence) — collapsing them simplifies the error-rendering refactor.

13. [x] Admin form success: no toast + no table refresh after sheet-create / sheet-edit. — archived

14. [x] Template-side reverse-O2O / forward-FK traversal on `user` — Shipped. — archived

15. [x] REST `?include=fk1,fk2` query-param plumbing → DynQuerySet.select_related(). — archived

16. [x] M2M echo on `DynQuerySet::fetch_as_json` is N+1. — archived

17. [x] Playground multi-select pickers for `?include=` and `?fields=` — SHIPPED in commit `3ff8d22`. — archived

18. [x] Nested `?include=` (dotted / `__` chain) — ORM half shipped. — archived

19. [x] `Form<T>` extractor + `#[derive(Form)]` validation — Shipped. — archived

20. [x] Shop example ships render-blocking CDN Tailwind + Google Fonts — replace with compiled CSS + self-hosted Inter. — archived

21. [~] **Template-side image optimization — Option A (img filter) SHIPPED, Options B (post-render rewriter) + C (resize handler) deferred.** Commit `03f8725` ships the `img` MiniJinja filter with the perf-hat-trick attributes the gap called out as the primary ask: `loading="lazy"`, `decoding="async"`, explicit `width`/`height` when provided (no CLS), empty-`alt=""` default for decorative images, attribute-value escape against quote-breakout. Call shape: `{{ url | img(alt="…", width=N, height=N, class="…") }}`. Wrapper output marked `from_safe_string` so autoescape doesn't double-escape angle brackets; attribute values themselves go through a local `html_escape_into` for security.

    3 regression pins in `crates/umbra-core/tests/template_discovery.rs`: minimal call shape, full-kwargs flow-through, hostile-alt escape (the test forms a payload `" onerror="alert(1)` and asserts exactly one `>` lands in the output, confirming the tag stays well-formed). 4 shop templates retrofitted: `home.html` (2 sites), `product_list.html`, `product_detail.html`. The `{% else %}` placeholder branches are unaffected.

    Options C (on-the-fly resize) + B (lol_html post-render rewriter) deferred — `srcset` and `<picture>` fallbacks need C's resize endpoint to know real asset dimensions, and B's streaming-parse-every-response CPU cost isn't worth paying for a hypothetical regression yet. The filter's design is forward-compatible with C: once the endpoint exists, `img` rewrites src to `{handler}?w=...&format=...`. Original rationale + decision matrix preserved below:

    Scope: HTML templates only. REST responses ship raw URLs as before — the API consumer picks how to handle images.

    **Three options on the table:**

      - **A. Custom MiniJinja `img` filter** — `{{ product.thumbnail | img }}` expands to a fully-formed `<img>` with `loading="lazy"`, `decoding="async"`, `srcset` for 1x/2x/3x, format negotiation. Registered globally on the env once (see `crates/umbra-core/src/templates.rs`).
        - **Pro**: zero ambiguity at the call site; the template author can't accidentally ship a heavy image. Composable with any URL — model field, hardcoded path, computed value.
        - **Con**: opt-in per call site. Old templates with raw `<img>` tags don't benefit until rewritten.

      - **B. `lol_html` post-render middleware** — parses the rendered HTML in a streaming rewriter, injects `loading="lazy"` + `decoding="async"` on every `<img>` that's missing them.
        - **Pro**: covers EVERY template + every plugin's template + every error page automatically. No call-site changes.
        - **Con**: streaming-but-still-parses every byte of HTML on every response (CPU overhead for high-traffic apps). Can't easily inject `srcset` because that requires knowing the asset's real dimensions on disk.

      - **C. On-the-fly resize handler** — `/static/images/...?w=400&format=webp` routes through a Rust handler that reads the original, resizes via `image` / `fast_image_resize`, transcodes to webp/avif, caches the result on disk (or in `umbra-cache`). Original asset never reaches a browser.
        - **Pro**: solves the "raw 5MB JPEG" problem at its source, not just the markup. Works regardless of which markup option above is used.
        - **Con**: meaningful infra surface — needs a cache layer + cache invalidation + storage growth strategy.

    **Recommendation: ship A + C as a pair.**

    - A is the ergonomic surface developers actually touch — one filter, one mental model. The filter emits URLs pointing at C's handler with the right `?w=...&format=...` params, so the markup AND the bytes get optimized together.
    - C does the actual heavy lifting (resize + transcode + cache); the filter is just curated URL generation.
    - B becomes the safety net only IF a real call site stays raw — defer until that surfaces. Skipping B keeps the per-response CPU budget free for actual work.

    Skip B unless a real consumer needs it; the filter-then-handler pattern is enough for the 90% case and is cheaper at runtime.

    **Dependencies + crates**:
      - `image` (or `fast_image_resize` for hot-path scaling) — resize + transcode.
      - `umbra-cache` (feature #44) — result caching keyed by `(asset, width, format)`. Fine to start with on-disk caching in `target/image-cache/` and switch to Redis later.
      - A tiny `umbra-media` adjacent (or just folded into core templates) plugin that registers the `img` filter + mounts the resize route.

    **Triggering case**: same Lighthouse run as gap #20 — the shop's product images hit hundreds of KB each on the home + product-list pages. Post-fix should drop to ~20-40 KB per image (webp at the right pixel size) AND defer offscreen images entirely via `loading="lazy"`.

22. [~] **Multi-DB router — per-plugin routing already exists (plugins ARE Django apps); the residual gap is a Django-style `allow_migrate` / `allow_relation` integrity guard, not the routing itself.** Verified: `Plugin::database()` sets a plugin-level default for *every* model the plugin contributes (`app.rs:480-490` — the per-app model, since umbra plugins = Django apps that own their own migrations), and `#[umbra(database = "alias")]` / `Model::DATABASE` is a per-model override walked after the plugin pass (`app.rs:493-498`); `resolve_pool::<T>()` (`orm/queryset/mod.rs:712`) routes with precedence explicit `.on(&pool)` → per-model alias → plugin default → `"default"`, named pools live in `db.rs` (`POOLS` keyed by alias, configured via the `databases` settings map / `UMBRA_DATABASES__<ALIAS>=…`), and the migrate engine splits each op to the right DB by `table_alias()` with per-DB tracking tables — so routing-per-plugin already aligns with migrations-per-plugin. What's missing, in two halves of one mechanism: (a) a plugin/model-level guard that fails loudly when an FK spans two databases — a cross-DB FK can't be a real constraint, Django's `allow_relation`/`allow_migrate` exists for exactly this — so today a model overridden onto an analytics/archive DB while its FK target stays on `default` would silently generate an invalid constraint; and (b) the field-level escape hatch that makes a cross-DB relation legal — a `#[umbra(db_constraint = false)]` micro (name TBD, mirrors Django's `ForeignKey(db_constraint=False)`) that keeps the FK as a logical relation (joins, `select_related`, app-level `check_fk_row_exists` pre-validation all stay working via `fk_target`) but emits no physical `FOREIGN KEY` DDL. The guard should forbid cross-DB FKs *unless* the field opts out via that attribute. Implementation touches three places like every field attribute: the macro (`umbra-macros` — parse it alongside `on_delete`/`noform`), `FieldSpec` (one more `bool`, like `noform`), and the migration emitter (`migrate.rs` — skip the constraint line when set). It's the field-level subtract-a-default pattern `noform`/`noedit` already use.

23. [ ] **DB router read/write split for replicas — routing is static one-model-one-DB; no `db_for_read` vs `db_for_write`, so a model can't send reads to a replica and writes to the primary.** Extend `resolve_pool` to distinguish read terminals (`first`/`fetch`/`count`/`exists`) from write terminals (`create`/`update_values`/`delete`) and add a router abstraction over the flat alias map (Django's `db_for_read`/`db_for_write`/`allow_relation`/`allow_migrate`); the genuinely hard piece is read-after-write consistency — a read inside a write transaction must hit the primary, not a lagging replica.

24. [ ] **Docs missing for multi-DB / database routing — no `documentation/docs/v0.0.1/orm/` (or `migrations/`) page covers named pools, `Model::DATABASE` routing, `.on(&pool)` override, or per-DB migrations.** Ship the minimal user-facing page (purpose + one example + link to spec) once #22/#23 land, per the "ship a feature, ship its doc page" rule in CLAUDE.md.

25. [ ] **`umbra startproject` should auto-mount `SecurityPlugin` (CSRF + hardening headers) in the scaffolded app builder.** Per the round-one audit (`bugs/review/security-auth-session.md` AUTH-1/AUTH-2), the flagship `examples/shop` runs with no CSRF + no security headers because `SecurityPlugin` is opt-in and easy to forget. The scaffold (gap #8) should wire `.plugin(SecurityPlugin::new())` by default (and `.with_hsts(true)` behind a prod profile); consider a boot-time `check.rs` warning when auth/sessions are mounted without security. Necessary but not sufficient for "secure by default" — see #26 and the XSS fixes (`bugs/review/security-web-surface.md` WEB-3/WEB-4) which `SecurityPlugin` does not cover.

26. [x] Signed/session-bound CSRF (`SecurityConfig::signed_csrf`) is now the default — archived

27. [ ] **Cache plugin should expose axum/tower-http caching + compression layers as opt-in config.** Mirror the new `umbra-security` config-struct shape: surface `tower_http::compression::CompressionLayer` (gzip/br negotiation) and HTTP cache-control header management (e.g. `SetResponseHeaderLayer` for `Cache-Control`/`ETag`/`Vary`) through `umbra-cache` so an app gets response compression + cache headers declaratively, instead of the page-cache (`cache_page`) being the only knob. Note: tower-http has no full response-cache layer; this is header + compression management, distinct from the server-side `cache_page` store. Pairs with the cache gaps already noted in `bugs/review/broken-features.md` (BROKEN-10/12).

28. [x] `allowed_hosts` request-time enforcement — SHIPPED. — archived

29. [x] CORS path scoping — SHIPPED. — archived

30. [ ] **Two flaky test groups under full-workspace parallel runs.** (a) `plugins/umbra-auth/tests/integration.rs::createsuperuser_noinput_errors_without_password_env` — a sibling test sets `UMBRA_SUPERUSER_PASSWORD` while this one runs; the in-test `remove_var` can't guard across parallel threads. Failed twice in unrelated verifies on 2026-06-10 (templates-only change in one case); passes alone and on re-run. Fix shape: a process-wide env-mutex shared by every test that touches superuser env vars, or move env-mutating cases into a serial `#[serial]` group. (b) `plugins/umbra-admin/tests/cross_crate_o2o.rs` — all three tests failed once in a full `cargo test` sweep, pass in isolation and on re-run; likely shared-DB/registry contention. (c) `plugins/umbra-admin/tests/phase2_sheet.rs::test_preview_sheet_htmx_returns_fragment` — failed once in a full sweep, 3/3 green when its file runs alone. The victims differ per sweep, which points at cross-binary contention (shared test DB or ambient registry) rather than per-test bugs. Diagnose the shared resource before papering over with retries.

31. [x] Can you reference deep nested html templates ie in a view you call `.render("")` with a path like `"/foo/bar.html"` and automatically find such a template? (Renumbered from a duplicate #30 — that number was already taken by the flaky-tests entry, committed and cited in `01503da`.)

32. [ ] **Boot check: a `choices` field's `#[umbra(default = "...")]` must be a member of the enum's `VALUES`.** The default string lands verbatim in DDL (`migrate.rs` `def.default(col.default.clone())`), so `default = "PostStatus::Draft"` (the Rust path instead of the DB literal `"draft"`) ships a broken schema: Postgres rejects the value at insert time via the CHECK constraint, but SQLite happily stores undecodable text that errors on the next SELECT. Surfaced 2026-06-10 by `umbra_website/plugins/plugin_directory/src/models.rs`, which used path-shaped defaults on four choices fields (fixed to DB literals; docs updated in `orm/column-types.mdx` "Defaults: two mechanisms"). Per the "backend mismatches caught at boot" principle, `check.rs` should walk every registered model's FieldSpecs and fail when `!choices.is_empty() && !default.is_empty() && !choices.contains(&default)` — with a did-you-mean hint when the default contains `::` (resolve the variant name against `ChoiceField::VALUES`). The same walk could sanity-check boolean defaults (`true`/`false`/`1`/`0`) and numeric defaults that don't parse for the column's SqlType.

33. [ ] **Admin index `/admin/` should not auto-redirect to `last_path` (a.k.a. gaps2 #11 round 2).** The "where I left off" affordance shipped in gaps2 #11 round 2 persists the URL of the last changelist the user visited into `admin_user_pref.preferences.last_path`; the index handler reads it and 302-redirects on every `/admin/` hit. The escape hatch was `?dashboard=1`, but no UI in the admin emits that param, so once `last_path` is set to a changelist, the user has no way to get back to the dashboard short of a direct SQL `UPDATE admin_user_pref SET preferences = '{}'`. Surfaced 2026-06-10 on `umbra_website` (the scaffold's admin kept 302-ing to `features_feature_status_event/?page_size=25` after the first changelist click). **Current mitigation**: the *reader* in the index handler (`plugins/umbra-admin/src/handlers/list.rs` ~lines 245-263) was removed — `/admin/` always renders the dashboard. The *writer* on the changelist handler was left in place (it's a cheap upsert, no behavioural cost). **Proper fix**: this whole feature should live behind an `AdminPlugin` config flag — `restore_last_path: bool` (default `true` for power users who want the behaviour, opt-out for projects that don't) — and the index handler reads that flag instead of hardcoding the redirect. Bonus: the writer on the changelist handler should also gate on the same flag, so users who opt out don't accumulate dead `last_path` data. Templates also need updating: the sidebar's "Home" / brand link should emit `?dashboard=1` automatically when the flag is on, so the opt-out is one click away rather than URL-tape.

34. [ ] **Soft delete: decide whether `update_values` should respect the `deleted_at IS NULL` auto-filter.** Reads and `delete()` are auto-scoped on a `#[umbra(soft_delete)]` model, but `build_update_for` (`orm/queryset/mod.rs` ~2403) walks only the explicit predicates — so a bulk `Post::objects().update_values(...)` also mutates trashed rows, and `.only_deleted()` / `.with_deleted()` are silent no-ops on the update path. Surfaced 2026-06-10 while writing `orm/soft-delete.mdx` (the restore example originally used `.only_deleted()` believing it scoped the UPDATE). Two defensible semantics: (a) consistent-with-reads — inject the filter, honour `with_deleted`/`only_deleted`, which makes restore `only_deleted().update_values(...)`; (b) writes-are-explicit — keep today's behaviour but make `.with_deleted()`/`.only_deleted()` on an update a loud error instead of a no-op. Either way the current silent no-op is the wrong third option. Update the docs example to match whichever wins.

35. [ ] **Soft delete is invisible to the dynamic path — admin and REST hard-delete and show trashed rows on `#[umbra(soft_delete)]` models.** `Model::SOFT_DELETE` lives only on the typed trait; `DynQuerySet` / `ModelMeta` (`orm/dynamic.rs`, zero soft_delete references) never see it. Consequences on a tagged model: admin changelists list soft-deleted rows alongside live ones, the admin delete button (and REST `DELETE /api/...`) destroys the row permanently, and REST list endpoints return trashed rows. Surfaced 2026-06-10 when `umbra_website` tagged its 23 `deleted_at` models. Fix shape: carry a `soft_delete: bool` on `ModelMeta` (wired from the registry the same way `noform`/`noedit` flow), inject the `deleted_at IS NULL` filter in `DynQuerySet` terminals, rewrite its `delete()` the way the typed path does, and give the admin a trash affordance (changelist filter for `only_deleted` + restore action + hard-delete behind a confirm) — the Django-admin-parity shape. Until then, treat soft delete as an app-code feature only.


36. [~] **Rich field-editor follow-ups — code-block syntax highlighting; CDN self-hosting.** SHIPPED (features.md #4, 2026-06-10): `admin.js` lazy-mounts EasyMDE (`markdown`), Quill (`rte`), and CodeMirror (`code`, JSON syntax) on every form-render path, themed to the admin tokens; editor previews are sandboxed through DOMPurify (EasyMDE preview + Quill initial load). What remains: (a) **syntax-highlighted fenced code** in `render_markdown` *display* output (distinct from the `code` *editor* widget) — ammonia strips the `language-*` class pulldown-cmark emits, so highlight.js/Prism have nothing to hook; either widen the ammonia allowlist for `class` on `code`/`pre` (scoped to `language-*`) or pre-highlight server-side (syntect) before sanitizing. (b) **Self-hosting the editor CDNs** (EasyMDE, Quill, CodeMirror, DOMPurify, and EasyMDE's transitive FontAwesome) via `Plugin::static_files`, same call as the htmx/lucide/apexcharts self-hosting already noted in gaps2 #4 — today they load from unpkg/jsdelivr, consistent with the rest of the admin but an offline/air-gapped gap. (c) **EasyMDE image-upload** hook into a future media endpoint. Surfaced 2026-06-10 alongside the widget/markdown landing.

37. [ ] Image submission - We have not yet matured the FileField, together with ImageField. Since ImageField is more of a wrapper around FileField except for the image preview. And I think, it should only be a FileField then we annotate with widget="image". Then, we can link this in form submission so that once a form is submitted, the file is processed. Going forward, we need to make sure the MediaPlugin is active since it will help us avoid this chaos at once.

38. [ ] `.filter(sc::ContactMessage::CREATED_AT.gte(week_ago))` - This does not work, should atleast work well as `.filter(contact_message::CREATED_AT.gte(week_ago))` (This works well)

39. [ ] **`annotate_count` follow-ups: child-side filters + child soft-delete; Form derive should auto-skip `ReverseSet`.** (a) `QuerySet::annotate_count("comment_set")` (shipped 2026-06-11 — Django's `annotate(n=Count("comments"))`, one correlated-subquery query) counts child rows unconditionally: no child-side predicate (Django's `Count(..., filter=Q(moderation="visible"))`) and no child soft-delete awareness (the spec can't see the child model's SOFT_DELETE — same root as gaps2 #35's ModelMeta gap), so the umbra.dev homepage counts ALL discussion notes rather than visible-only. Fix shape: an `annotate_count_where(field, Predicate<Child>)` overload once predicates can be carried generically, and fold `deleted_at IS NULL` into the subquery when the child registry meta says soft_delete. (b) `#[derive(Form)]` rejects `ReverseSet` fields with the unsupported-type error and requires a manual `#[umbra(noform)]`; the Model derive already auto-skips ReverseSet from FIELDS — the Form derive should do the same (it is never user-submittable by construction). Surfaced wiring Plugin.comment_set on umbra.dev.

40. [ ] `/home/dalmas/E/projects/umbra/umbra_website/plugins/plugin_directory/src/models.rs ln 343` - Enable "Foreign keys" to work well with `Form` derive

41. [ ] The body Markdown, code, and RTE don't work yet. I filled in markdown field and got back an error `body is required, null`. So the markdown field did not fill the underlying body input field. When the error came back, the textarea and the mardown field were both shows (./images/Screenshot from 2026-06-11 03-55-50.png)
42. [x] FK save binds text not bigint — archived
43. [ ] Another issue is that in django admin, errors are returned 1 by 1 not all at once. This is a bug and usability issue. They should be returned at once and each field should render the error below it not at the top of the form.
44. [ ] Admin tables don't refresh on entry craetion or update, the page, the table should refresh. This was done but maybe the url is not receiving the refresh signal correctly.
45. [ ] We need to fix the fks relationships. In django you do `Post.objects.get(id=1).comments_set.all()` or even better `Post.objects.annotate(comment_count=Count('comments'))` - We need a seemless way of doing this. The orm should automatically handle reverse keys for all the fks
// Aggregate counts using a LEFT JOIN so related rows are scanned once and plugins without comments are still returned with a count of 0.
// Use LEFT JOIN + COUNT() for efficient comment aggregation while preserving plugins with no comments.
Above are somethings that can be done and then produce a concise query than /home/dalmas/E/projects/umbra/umbra_website/plugins/public/src/test.sql. This is tested on /home/dalmas/E/projects/umbra/umbra_website/plugins/public/src/lib.rs ln 81. The `.to_sql()` call. We need to close this issue before we proceed, same with the `derive Form` for fks and choice fields.
46. [ ] Session plugin is broken - I open a new browser. Load the url and it randomly loads 3 entries into the db. Tha that is wrong by all directions. Should only log a single entry. The same entry is the one to be updated once a user logs in for example. After login it didn't increase the no of sessions. The first part is the wrong one!

47. [ ] **M2M junction write (`set_junction_dynamic`) does one INSERT per child id — batch into one multi-row INSERT.** `crates/umbra-core/src/orm/m2m.rs` (~555-618, both backend arms) runs one DELETE then a loop of one `INSERT INTO <junction> (parent_id, child_id) VALUES (?, ?)` per child id inside the transaction. Not a result-set N+1 (it's bounded by selection count in a single tx), and not introduced by the relations/forms/joins work — surfaced 2026-06-11 auditing the Plan A M2M form-write path, which correctly *reuses* this shared helper rather than hand-rolling raw SQL. Fix: accumulate all `(parent_id, child_id)` tuples into one `Query::insert()` with multiple `values_panic` rows (or sea-query's multi-row form) before `build_sqlx`, turning M INSERTs into 1. Touches the admin M2M write path too (same helper), so pin it with a behavioral test that asserts the junction rows land and a duplicate select stays `ON CONFLICT DO NOTHING`. Low priority — correctness is fine today; this is throughput only.

48. [ ] **`validate_multi_fk_exists` masks a DB error as a validation failure.** `crates/umbra-core/src/orm/forms_runtime.rs` (~190) does `DynQuerySet::for_meta(&meta).select_cols(...).filter_in_strings(...).fetch_as_json().await.unwrap_or_default()` — on a real DB error the row set silently comes back empty, so EVERY submitted M2M id is then flagged "id has no matching record." It fails *closed* (the write is rejected — the safe direction), but the user sees a bogus per-field "no matching record" error instead of the real database error, and the underlying error is swallowed (`.unwrap_or_default()` discards it). Surfaced 2026-06-11 in the Plan A holistic review. Fix per "fix, don't patch / don't swallow secondary errors": propagate the `fetch_as_json` error out of `validate_multi_fk_exists` (it already runs in an async `validate`) so a DB failure surfaces as a 500 / logged error, distinct from a genuine "id not found" validation miss. Same family as the `.ok()`-swallow pattern CLAUDE.md calls out.

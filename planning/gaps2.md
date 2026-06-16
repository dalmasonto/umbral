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

22. [x] Cross-database FK integrity guard + `#[umbra(db_constraint = false)]` opt-out — archived (`BuildError::CrossDatabaseForeignKey` boot guard + field-level constraint opt-out; the broader `DatabaseRouter` trait stays open as #23/#28).

23. [ ] **DB router read/write split for replicas — routing is static one-model-one-DB; no `db_for_read` vs `db_for_write`, so a model can't send reads to a replica and writes to the primary.** Extend `resolve_pool` to distinguish read terminals (`first`/`fetch`/`count`/`exists`) from write terminals (`create`/`update_values`/`delete`) and add a router abstraction over the flat alias map (Django's `db_for_read`/`db_for_write`/`allow_relation`/`allow_migrate`); the genuinely hard piece is read-after-write consistency — a read inside a write transaction must hit the primary, not a lagging replica.

24. [~] **Multi-DB / database routing docs — current-state page SHIPPED; expand when #22/#23 land.** `documentation/docs/v0.0.1/orm/database-routing.mdx` now documents the existing surface: registering named pools (`.database(alias, pool)`, `"default"` required, connect-then-register, the `UMBRA_DATABASES__<ALIAS>` env map), per-model routing (`#[umbra(database = "...")]` / `Model::DATABASE`), per-plugin routing (`Plugin::database()`), the `resolve_pool` precedence (`.on()` → per-model → per-plugin → `"default"`), the `.on(&SqlitePool)` per-query override, per-database migrations (one `umbra_migrations` table per DB, split by `table_alias`), and boot-time alias/backend validation. Shipped early (before #22/#23) at the user's request to make the gap visible — the page's **"Not yet supported"** section flags: no read/write replica split (#23), no dynamic per-request routing (the multitenancy gap — routing is per-model at build time, not per-request), `.on()` is SQLite-typed, and the `databases` settings map isn't auto-connected. **#22 shipped** → the page gained a "Cross-database foreign keys" section documenting the boot guard (`BuildError::CrossDatabaseForeignKey`) + the `#[umbra(db_constraint = false)]` opt-out. Expand further as #23 lands.

25. [ ] **`umbra startproject` should auto-mount `SecurityPlugin` (CSRF + hardening headers) in the scaffolded app builder.** Per the round-one audit (`bugs/review/security-auth-session.md` AUTH-1/AUTH-2), the flagship `examples/shop` runs with no CSRF + no security headers because `SecurityPlugin` is opt-in and easy to forget. The scaffold (gap #8) should wire `.plugin(SecurityPlugin::new())` by default (and `.with_hsts(true)` behind a prod profile); consider a boot-time `check.rs` warning when auth/sessions are mounted without security. Necessary but not sufficient for "secure by default" — see #26 and the XSS fixes (`bugs/review/security-web-surface.md` WEB-3/WEB-4) which `SecurityPlugin` does not cover.

26. [x] Signed/session-bound CSRF (`SecurityConfig::signed_csrf`) is now the default — archived

27. [ ] **Cache plugin should expose axum/tower-http caching + compression layers as opt-in config.** Mirror the new `umbra-security` config-struct shape: surface `tower_http::compression::CompressionLayer` (gzip/br negotiation) and HTTP cache-control header management (e.g. `SetResponseHeaderLayer` for `Cache-Control`/`ETag`/`Vary`) through `umbra-cache` so an app gets response compression + cache headers declaratively, instead of the page-cache (`cache_page`) being the only knob. Note: tower-http has no full response-cache layer; this is header + compression management, distinct from the server-side `cache_page` store. Pairs with the cache gaps already noted in `bugs/review/broken-features.md` (BROKEN-10/12).

28. [x] `allowed_hosts` request-time enforcement — SHIPPED. — archived

29. [x] CORS path scoping — SHIPPED. — archived

30. [ ] **Two flaky test groups under full-workspace parallel runs.** (a) `plugins/umbra-auth/tests/integration.rs::createsuperuser_noinput_errors_without_password_env` — a sibling test sets `UMBRA_SUPERUSER_PASSWORD` while this one runs; the in-test `remove_var` can't guard across parallel threads. Failed twice in unrelated verifies on 2026-06-10 (templates-only change in one case); passes alone and on re-run. Fix shape: a process-wide env-mutex shared by every test that touches superuser env vars, or move env-mutating cases into a serial `#[serial]` group. (b) `plugins/umbra-admin/tests/cross_crate_o2o.rs` — all three tests failed once in a full `cargo test` sweep, pass in isolation and on re-run; likely shared-DB/registry contention. (c) `plugins/umbra-admin/tests/phase2_sheet.rs::test_preview_sheet_htmx_returns_fragment` — failed once in a full sweep, 3/3 green when its file runs alone. The victims differ per sweep, which points at cross-binary contention (shared test DB or ambient registry) rather than per-test bugs. Diagnose the shared resource before papering over with retries.

31. [x] Can you reference deep nested html templates ie in a view you call `.render("")` with a path like `"/foo/bar.html"` and automatically find such a template? (Renumbered from a duplicate #30 — that number was already taken by the flaky-tests entry, committed and cited in `01503da`.)

32. [x] **Boot check `field.choices_default` rejects a choices default that isn't a member of its choices — SHIPPED.** `check.rs` walks every registered model and fails the build with a `Severity::Error` finding when `!choices.is_empty() && !default.is_empty() && !choices.contains(&default)`, with a did-you-mean for `::`-shaped defaults (lowers the tail, suggests the DB literal). Two-binary test (bad path-shaped default fails build; valid default builds). The optional boolean/numeric default sanity-check was deliberately deferred (false-positive risk on `CURRENT_TIMESTAMP`/expression defaults). — archived

33. [ ] **Admin index `/admin/` should not auto-redirect to `last_path` (a.k.a. gaps2 #11 round 2).** The "where I left off" affordance shipped in gaps2 #11 round 2 persists the URL of the last changelist the user visited into `admin_user_pref.preferences.last_path`; the index handler reads it and 302-redirects on every `/admin/` hit. The escape hatch was `?dashboard=1`, but no UI in the admin emits that param, so once `last_path` is set to a changelist, the user has no way to get back to the dashboard short of a direct SQL `UPDATE admin_user_pref SET preferences = '{}'`. Surfaced 2026-06-10 on `umbra_website` (the scaffold's admin kept 302-ing to `features_feature_status_event/?page_size=25` after the first changelist click). **Current mitigation**: the *reader* in the index handler (`plugins/umbra-admin/src/handlers/list.rs` ~lines 245-263) was removed — `/admin/` always renders the dashboard. The *writer* on the changelist handler was left in place (it's a cheap upsert, no behavioural cost). **Proper fix**: this whole feature should live behind an `AdminPlugin` config flag — `restore_last_path: bool` (default `true` for power users who want the behaviour, opt-out for projects that don't) — and the index handler reads that flag instead of hardcoding the redirect. Bonus: the writer on the changelist handler should also gate on the same flag, so users who opt out don't accumulate dead `last_path` data. Templates also need updating: the sidebar's "Home" / brand link should emit `?dashboard=1` automatically when the flag is on, so the opt-out is one click away rather than URL-tape.

34. [ ] **Soft delete: decide whether `update_values` should respect the `deleted_at IS NULL` auto-filter.** Reads and `delete()` are auto-scoped on a `#[umbra(soft_delete)]` model, but `build_update_for` (`orm/queryset/mod.rs` ~2403) walks only the explicit predicates — so a bulk `Post::objects().update_values(...)` also mutates trashed rows, and `.only_deleted()` / `.with_deleted()` are silent no-ops on the update path. Surfaced 2026-06-10 while writing `orm/soft-delete.mdx` (the restore example originally used `.only_deleted()` believing it scoped the UPDATE). Two defensible semantics: (a) consistent-with-reads — inject the filter, honour `with_deleted`/`only_deleted`, which makes restore `only_deleted().update_values(...)`; (b) writes-are-explicit — keep today's behaviour but make `.with_deleted()`/`.only_deleted()` on an update a loud error instead of a no-op. Either way the current silent no-op is the wrong third option. Update the docs example to match whichever wins.

35. [ ] **Soft delete is invisible to the dynamic path — admin and REST hard-delete and show trashed rows on `#[umbra(soft_delete)]` models.** `Model::SOFT_DELETE` lives only on the typed trait; `DynQuerySet` / `ModelMeta` (`orm/dynamic.rs`, zero soft_delete references) never see it. Consequences on a tagged model: admin changelists list soft-deleted rows alongside live ones, the admin delete button (and REST `DELETE /api/...`) destroys the row permanently, and REST list endpoints return trashed rows. Surfaced 2026-06-10 when `umbra_website` tagged its 23 `deleted_at` models. Fix shape: carry a `soft_delete: bool` on `ModelMeta` (wired from the registry the same way `noform`/`noedit` flow), inject the `deleted_at IS NULL` filter in `DynQuerySet` terminals, rewrite its `delete()` the way the typed path does, and give the admin a trash affordance (changelist filter for `only_deleted` + restore action + hard-delete behind a confirm) — the Django-admin-parity shape. Until then, treat soft delete as an app-code feature only.


36. [~] **Rich field-editor follow-ups — code-block syntax highlighting; CDN self-hosting.** SHIPPED (features.md #4, 2026-06-10): `admin.js` lazy-mounts EasyMDE (`markdown`), Quill (`rte`), and CodeMirror (`code`, JSON syntax) on every form-render path, themed to the admin tokens; editor previews are sandboxed through DOMPurify (EasyMDE preview + Quill initial load). What remains: (a) **syntax-highlighted fenced code** in `render_markdown` *display* output (distinct from the `code` *editor* widget) — ammonia strips the `language-*` class pulldown-cmark emits, so highlight.js/Prism have nothing to hook; either widen the ammonia allowlist for `class` on `code`/`pre` (scoped to `language-*`) or pre-highlight server-side (syntect) before sanitizing. (b) **Self-hosting the editor CDNs** (EasyMDE, Quill, CodeMirror, DOMPurify, and EasyMDE's transitive FontAwesome) via `Plugin::static_files`, same call as the htmx/lucide/apexcharts self-hosting already noted in gaps2 #4 — today they load from unpkg/jsdelivr, consistent with the rest of the admin but an offline/air-gapped gap. (c) **EasyMDE image-upload** hook into a future media endpoint. Surfaced 2026-06-10 alongside the widget/markdown landing.

37. [x] FileField + ImageField (ImageField = FileField + widget="image") wired through multipart form submission to the ambient Storage backend; MediaPlugin provides it, enforced by a boot system-check — archived

38. [x] Column predicate consts reachable as `Model::COL` (associated const) alongside `module::COL` — `#[derive(Model)]` now emits `impl Struct { pub const COL: ColType<Self> = module::COL; }` per column (an alias of the module const, one source of truth), so `.filter(ContactMessage::CREATED_AT.gte(..))` works without importing the column module — archived

39. [x] annotate_count child-side filters + child soft-delete + Form derive auto-skips ReverseSet — archived

40. [x] Foreign keys work with Form derive (ModelChoice) — archived

41. [ ] The body Markdown, code, and RTE don't work yet. I filled in markdown field and got back an error `body is required, null`. So the markdown field did not fill the underlying body input field. When the error came back, the textarea and the mardown field were both shows (./images/Screenshot from 2026-06-11 03-55-50.png)
42. [x] FK save binds text not bigint — archived
43. [x] Admin full-page create/edit forms now validate every field up front (`validate_form` in `view.rs`) and surface ALL failures at once — required / number / date / time / datetime-local / choice / max_length — each rendered below its own input (`FormField.error` + `form.html`), instead of one DB error at a time at the top. — archived
44. [x] The post-save `refreshTable` handler now re-fetches the changelist from the server-authoritative `data-rows-url` stamped on `#table-body` (real admin base + table), instead of string-synthesizing `window.location.pathname + '/rows'` — the fragile link that broke the refresh on a custom base path / trailing slash. Sheet create + full-page update both already emit `refreshTable` (gaps2 #13); the URL derivation was the weak point. — archived (needs in-browser confirmation of the original symptom)
45. [x] Seamless reverse-FK relations — annotate auto-discovery + instance `reverse::<Child>()` accessor (zero-declaration) — archived
46. [x] Session plugin created a DB row per cookie-less request (3 on fresh load) — fixed via lazy session creation — archived

47. [x] M2M junction write batched into one multi-row INSERT — archived

48. [x] validate_multi_fk_exists surfaces DB errors instead of masking them as "id not found" — archived

49. [x] Facade re-exports with_actor / current_actor from umbra::signals — archived

50. [ ] Admin plugin inline editing of children. This will remain defered since its a large refactoring effort with no direct impact on the core Umbra API.

51. [x] Inline CSS in the umbra-site is the legitimate "custom" case (Tailwind utilities can't express the dropdown component's `::before` hover bridge, `[aria-expanded]`/`:hover` state selectors, `:root` design tokens, or dynamic per-element values like `--i: {{ loop.index }}` / `width: {{ pct }}%`) — explained, not a Tailwind-avoidance to fix — archived

52. [x] Flaky `createsuperuser_noinput_errors_without_password_env` env-var race fixed — the two tests that mutate `UMBRA_SUPERUSER_PASSWORD` now serialise on a shared `static SUPERUSER_ENV_LOCK: tokio::sync::Mutex<()>` held across each test's env-dependent body, so they can't race under parallel runs — archived

53. [x] Playground shell resolves its asset prefix from the configured `static_url` (snapshotted into `PlaygroundState` at router-build time) instead of a hardcoded `/static/playground/assets` — archived

54. [x] CLI lists every built-in + plugin-contributed command (name + description, column-aligned) on `umbra help` / `--help` and on an unknown command (with `error: unknown command` + the listing) — `62660a1` — archived

55. [ ] Django's collectstatic can autocollect static to the configured aws bucket by the staticstorage backend. We need the same I guess.

56. [x] Grouped aggregate in the ORM (`QuerySet::annotate(group_cols, aggs)` = Django's `.values("status").annotate(count=Count("id"))`) was already shipped + documented; the shop donut + activity widgets refactored off fetch-all-then-count, and the widgets doc updated — archived

57. [ ] The media plugin can be improved to allow background file uploads and processing. This can be done through a function that just returns the perceived file path or URL, and the actual processing is done asynchronously. Also, the media plugin should be directly swappable for different storage backends (e.g. local filesystem, cloud storage) or just extended to maintain the same interface.

58. [x] Same struct = model, form & serializer — surfaced on `/features` (editorial callout + DB catalog entry + self-healing seed) and the `/docs` landing. — archived

59. [ ] We have RBAC already using the permissions plugin. The issue for now is, if a user registers the plugin, do they automatically get permissions gates through their users? Like the rest plugin, will it go through the permissions too? The idea is, how can we make it like a callable ie `.enable_permissions()` if the permissions plugin exists, it enables permissions for users and a user can configure the permissions right from the auth plugin and it towers everything as a middleware or another way, is it immediately enables a middleware that does role check for is_staff, is_superuser, the user is in group x or the specific permissions like `blog.can_publish_post`

60. [ ] From #59 above, I have noticed our middleware is not strong for now. Usually, once a middlware is set, it cuts across every other app without touching any app/plugin code ie in django, the csrf middleware touches nothing, it sought of returns true or false and the next thing is called to continue processing the request.

61. [x] Batch resource/model registration — `RestPlugin::resources(iter)` + `AdminPlugin::register_many(iter)` / `register_for_many(name, iter)`, the per-app "export a Vec, register once" pattern (DRF-style) — archived

62. [x] Browser live-reload — new opt-in `umbra-livereload` plugin (SSE push + `notify` file watcher + auto-injected client; CSS hot-swap + full reload; `.rs` handled by the rebuild→restart→reconnect→reload path). Dev-only, framework-level, dogfooded in umbra_website. — archived
63. [ ] We need to generate alot of data upto about 5GB or even 10GB or 20GB ie a table with about 200 Million rows to just excerise and test the ORM ie in querying, aggregation, and other operations to ensure proper speed. (https://lemire.me/blog/2012/03/27/publicly-available-large-data-sets-for-database-research/)
64. [ ] Can we use wasm for our js requirements for admin ie rendering widget charts etc - Defer, this is a complete non-requirement but worthy trying out.
65. [ ] We need a pagination plugin, not for rest but rather for the system itself, for content rendered through the jinja templates. If the rest plugin is reusable, we can pull that and reuse it else, we need a different plugin for the same.
66. [ ] https://docs.rs/minijinja/latest/minijinja/filters/index.html - We need to document how to use different filters in templates from minijinja and how to register custom template tags.
67. [ ] The `umbra startapp` command should automatically register the added plugin into `cargo.toml` and let the developer know that the plugin has been added successfully.
68. [ ] Plugin issues are not tracked anywhere - They are recorded as comments which is wild but still the comment does not appear! Maybe because they are issues or not public yet. We can leave them as comments but we update the system to track them properly ie update the comments table to track `is_issue`, `is_public` and `is_resolved`. This way, we can safely track and resolve plugin issues. Also, when its not resolved, it remain redish once solved it switches to green. Same comment flow and users can still comment on it without us changing alot of things. ON the issues tab, we only show comments that are issues and their status. With this flow, a user can safely comment on the plugin with the issue url on github or gitlab and a user can click on the link to track the issue now on github
69. [ ] **Pluggable / standalone database router — the keystone for multitenancy, read/write split, and new backends.** _Original (user):_ the current flow embeds postgres and sqlite routers in the ORM. That's fine, but adding mysql/mongodb means updating the ORM, and ORM-level changes like Postgres multitenancy are hard. Move routing out of the ORM — or at least shape it so it works as a standalone, swappable router — so a developer implementing multitenancy can drop in their own.

    **Why this is NOT a duplicate of #22–#24 (keep it separate):** #22 (cross-DB FK guard), #23 (read/write replica split), and #24 (docs) all sit on top of one missing abstraction. `resolve_pool` today is a hardcoded function keyed only on the *model*, resolved at *build time* (`.on()` → `Model::DATABASE` → `Plugin::database()` → `"default"`) — no user-swappable seam, and no notion of the *current request*. Extracting it into a `DatabaseRouter` trait (Django's router surface: `db_for_read` / `db_for_write` / `allow_relation` / `allow_migrate`, **plus** a per-request resolver) is the single change that unblocks all three.

    **Multitenancy — the three strategies and what each needs from the router:**
      - **Schema-per-tenant (the Django `django-tenants` model — the user's target).** ONE Postgres database, a **schema per tenant**; the request's tenant selects the active schema via `SET search_path` per pooled connection. No manually-declared databases. umbra has **no** notion of Postgres schemas or per-request `search_path` today. Needs: (a) a per-request "current tenant → schema" resolver on the router, (b) `search_path` switching on connection checkout, (c) migrations that create + migrate each tenant schema (a `migrate_schemas` equivalent), (d) a shared/public schema for tenant-agnostic tables.
      - **Database-per-tenant.** A pool per tenant, chosen per request — the `.on(&tenant_pool)` primitive exists but only manually; needs the same per-request resolver so routing is ambient, not threaded by hand on every query.
      - **Row-level (shared schema).** A `tenant_id` column + an ambient filter injected on every query — needs a request-scoped predicate, not a pool/schema switch.

    **The common missing primitive:** a **request-scoped routing context** (tenant → schema/pool/filter) populated by middleware (ties into #10 / #60 — the middleware contract) and read by the router. Static per-model routing (#22–#24) is necessary plumbing but routes by *model*, never by *tenant / request* — that's the gap `database-routing.mdx`'s "no dynamic per-request routing" bullet points here for.

    **Shape:** `trait DatabaseRouter { fn db_for_read/db_for_write(&self, model, ctx) -> alias; fn schema_for(&self, ctx) -> Option<&str>; fn allow_relation/allow_migrate(...) -> bool; }`, with a default impl reproducing today's static behaviour, swappable via `App::builder().router(MyTenantRouter)`. This folds in #23 (read/write = `db_for_read`/`db_for_write`), absorbs #22 (cross-DB FK guard = `allow_relation`), enables multitenancy (schema/DB per request), and decouples backend specifics for mysql/mongodb. **Recommendation:** land #22 first (cheap, concrete, makes cross-DB safe today), then design this router trait as its own brainstorm → spec — it's the strategic piece and the real basis of multitenancy.

70. [ ] **Missing Postgres-only field types — PostGIS (geometry/geography), derive-reachable `Cidr`, range/hstore/interval/money, etc.** umbra already ships most PG-only column types — `Array`, `Inet`, `MacAddr`, `FullText` (tsvector), and `Decimal` (`NUMERIC(19,4)`) — each with the full stack (`SqlType` variant + macro `FieldKind` classification + `*Col` predicate type + migration DDL + boot-check backend gating + `inspectdb` mapping). What's missing:

    - **PostGIS `geometry` / `geography`** (the headline ask) — no `SqlType`, no Rust binding, no DDL, no GiST index support. Needs a `geo-types` (or `postgis`) binding, a `Geometry`/`Geography` column type with the spatial predicate surface (`ST_DWithin`, `ST_Contains`, `&&` bbox), `CREATE EXTENSION postgis` awareness, and a GiST index option. Real demand for geospatial apps — prioritise.
    - **`Cidr` via the derive** — `SqlType::Cidr` + `CidrCol` already exist, but `ipnetwork::IpNetwork` classifies as `Inet` by default and the `#[umbra(cidr)]` opt-in attribute is **deferred** (`crates/umbra-macros/src/lib.rs` §~2461), so a CIDR column can only be produced by hand-writing a `FieldSpec` today. Cheap win: wire the `#[umbra(cidr)]` attribute (parse it in the field-attr loop, switch `Inet → Cidr`) so the existing column type becomes derive-reachable.
    - **Nullable `Decimal`** — `rust_decimal::Decimal` works (non-nullable, `_pg` terminals only since rust_decimal is PG-only in sqlx), but `Option<Decimal>` has no `NullableDecimal` classification, so a nullable NUMERIC column fails the derive with "M3 doesn't support this field type". Add `FieldKind::NullableDecimal` + `NullableDecimalCol`, the same shape as the other `Nullable*` types. (Surfaced 2026-06-16 while adding `decimal_field.rs`.)
    - **Other PG-only types with no umbra surface:** range types (`int4range` / `numrange` / `tstzrange` / `daterange`), `hstore`, `interval`, `money`, `bit` / `varbit`, geometric primitives (`point` / `line` / `polygon` — distinct from PostGIS), `ltree`, `xml`, and composite/enum types (enum is approximated today via `#[umbra(choices)]` + TEXT). Demand-driven — add per the standard six-touchpoint recipe (`SqlType` → macro `FieldKind` → `*Col` → migration DDL → boot-check gating → `inspectdb`), the way `Inet` / `Array` / `FullText` landed.

    Each PG-only type fails the boot-time system check on SQLite (by design); tests follow the `network_field.rs` shape — derive classification + SQLite boot-gating on by default, live PG round-trip behind `#[ignore]`. Surfaced 2026-06-16 while correcting the admin docs' stale "Postgres-only field types not in v1" framing (`documentation/docs/v0.0.1/plugins/admin.mdx`) and adding `Decimal` to `orm/column-types.mdx`.

---

> **#71–#78 — surfaced 2026-06-16 by the hardening review** (`planning/hardening/`). Full prioritized detail in `planning/hardening/backlog.md`; per-finding cites in `planning/hardening/reviews/*.md`.

71. [ ] **Concurrency / data-divergence hardening.** Three unguarded write paths diverge data under concurrent same-resource requests: session `set_data` read-modify-writes the whole JSON blob and loses keys (`plugins/umbra-sessions/src/lib.rs:400-456`; also swallows corrupt-data → empty map at `:389,447`); `set_user_groups` non-transactional DELETE+INSERT opens an empty-membership window = transient privilege loss (`plugins/umbra-permissions/src/membership.rs:77-97`, while every sibling junction write is already transactional); `update_or_create`/`get_or_create` SELECT-then-write with no tx → duplicate rows or spurious `UniqueViolation` (`crates/umbra-core/src/orm/queryset/mod.rs:3886-3996`), and `add_user_to_group`/`grant_user_permission` don't catch the UNIQUE backstop (error instead of idempotent). Fix: transaction (`SELECT … FOR UPDATE` / atomic merge) + UNIQUE constraint with caught violation; log session decode failures. Audited-and-SAFE (don't re-audit): `OnceLock` boot init, signals mutex, M2M `ON CONFLICT DO NOTHING`, `update_expr` server-side `col = col + 1`. → `reviews/race-conditions.md`.

72. [ ] **Endpoint scalability — unbounded fetches, per-row clones, missing indexes.** Two endpoint-reachable O(table)-memory holes: the admin M2M form loads the entire target table (no LIMIT) on every add/edit render (`plugins/umbra-admin/src/view.rs:511`; the FK picker beside it is paginated), and REST `?format=csv` bypasses the 1000-row cap and buffers `SELECT *` (`plugins/umbra-rest/src/lib.rs:1748`, `page=None` skips the clamp). Plus `apply_overrides` deep-clones the whole model registry per row (`umbra-rest/src/lib.rs:779 → migrate.rs:85`), `AdminPerms::load` fires ~12-14 serial permission queries per changelist (one-query `user_perms()` fix exists), and the migration engine emits no index for FK columns or `deleted_at` (the #63 cliff + soft-delete scan). Fix: paginate/clamp/stream, clone once, batch the load, auto-emit FK + soft-delete indexes. → `reviews/performance-scalability.md`.

73. [ ] **Silent wrong-writes + per-request panic.** Paths that report success while storing wrong/no data: non-i64 M2M child ids dropped from form junction writes (`crates/umbra-core/src/orm/forms_runtime.rs:226`); `f32`/`f64` bypass `min`/`max` validation (`orm/dynamic.rs:1348, 2656`); `inline_edit` writes `""` on a parse failure (`plugins/umbra-admin/src/inline_edit.rs:163`, vs `actions.rs:44` which 400s); `Masked` malformed key → silent `None` keyring (`orm/masked.rs:204`); REST CSV writer errors dropped. Plus `storage.rs:186` `.expect` panics every request if `MediaPlugin` wasn't registered (→ boot system-check). Fix: surface each error (400 / boot-fail / log); validate floats; persist non-i64 M2M ids. → `reviews/correctness-domain.md` + `reviews/static-analysis.md`.

74. [ ] **OAuth: no PKCE + replayable `state`.** `plugins/umbra-oauth/src/routes.rs:143-218` runs the flow without PKCE and with a non-single-use `state` → auth-code interception + callback replay, worst for the supported SPA token mode. Fix: PKCE (S256) + single-use expiring `state` bound to the session. → `reviews/security.md`.

75. [ ] **Secret / auth hardening.** Empty `SECRET_KEY` silently signs CSRF with an empty HMAC key → forgeable, no warning (`plugins/umbra-security/src/lib.rs:392-394` — fail-closed / boot-warn); `password_hash` is serde-serialized and guarded only by the block-list, so one `.expose(["auth_user"])` without `.hide()` leaks argon2 hashes (`plugins/umbra-auth/src/lib.rs:233-234` — never-serialize / auto-hide); an inactive superuser keeps perms on an already-issued session (`plugins/umbra-permissions/src/rest.rs:97-105` — re-check `is_active`). (No SQL injection found anywhere — the whole raw-SQL surface was verified parameterized.) → `reviews/security.md`.

76. [ ] **Plugin-contract violation — `umbra-auth` depends on `umbra-rest`.** The `Authentication`/`Identity` traits live in `umbra-rest`, so every app with auth pulls in `umbra-rest` even when REST-free — contradicts the CLAUDE.md contract "a REST-free app has to compile and run with zero serializer code." Fix: lift those traits into `umbra-core`/facade; both auth + rest depend inward. → `reviews/architecture-modularity.md`.

77. [ ] **Dedup `to_snake_case` (×3) / `pascal_case` (×2).** `to_snake_case` is reimplemented in `umbra-macros`, `crates/umbra-core/src/inspect.rs`, and `orm/queryset/mod.rs`; `pascal_case` in `umbra-openapi` + `umbra-cli`. Consolidate into one shared helper so casing can't drift. → `reviews/architecture-modularity.md`.

78. [ ] **Module splits for the 5 files >2,800 LOC.** `orm/queryset/mod.rs` (4846), `migrate.rs` (4660), `umbra-macros/src/lib.rs` (4521), `orm/dynamic.rs` (3009), `orm/column.rs` (2845) each mix several responsibilities. Split each into a cohesive *module* (directory of focused files grouped by responsibility, not arbitrary line cuts); proposed trees + the fns/impls that move are in the report. Notably collapse `dynamic.rs`'s 4 parallel decode fns (`decode_to_string`/`_pg`/`_to_json`/`_pg_to_json`). Pure refactor, do incrementally. → `reviews/architecture-modularity.md`.

> Existing entries the review sharpened: **#34** (stale line-ref `~2403`; also misses `update_expr`), **#35** (+ a 3rd soft-delete leak: relation hydration `orm/queryset/hydration.rs:654` returns trashed children), **#63** (FK + `deleted_at` index emission), **#68** (`on_delete` is DDL-only — no ORM cascade collector / `post_delete`), **#79** (the unsafe `nullable→NOT NULL` / `unique false→true` ALTERs lack a NULL/dup pre-check). The ~8 Critical + long-tail **doc drifts** (FColExt-not-in-prelude, non-existent `#[umbra(m2m)]`, realtime MDX artifacts, `checkmigrations` binary, admin CSS path / `on_ready` claim, etc.) ship as a single docs PR — see `planning/hardening/docs-audit/*.md`.

---

## Wave C — per-plugin review (all 19 plugins; `planning/hardening/plugins-review/<plugin>.md`)

> Holistic 5-axis + **completeness** pass over every built-in plugin (one report each, the detailed single source). Verdicts: **Solid/complete** — auth, sessions, permissions, email, realtime, livereload, health, playground, signals (with the async-panic fix), rest (strongest in tree). **Real but incomplete** — rls (DDL real, enforcement absent), oauth (refresh missing), tasks (lean v1). **Has gaps** — admin (one advertised feature stubbed). Net-new findings consolidated below; each cites the per-plugin report.

79. [ ] **Plugin completeness — advertised-but-non-functional surfaces.** Config knobs that compile but do nothing: admin `InlineModel`/`TabularInline` is stored by `AdminModel::inlines()` and **never rendered** (the biggest Django-parity hole + a fix-don't-patch violation), and `Action::permission(codename)` is stored but never enforced; `umbra-rls` emits `CREATE POLICY` DDL but **nothing sets `app.user_id` per request**, so every documented policy `current_setting('app.user_id')` errors-or-denies-all → the plugin is non-functional as shipped (ties #69 routing-context), and policies are append-only across boots (removing one from code never drops it — an access-control footgun); `umbra-oauth` stores+rotates refresh tokens in `Masked` columns but **never exchanges them** (`grant_type=refresh_token` unimplemented, `expires_at` never read) → "API on their behalf" dies at ~1h; `umbra-rest` `?ordering=` is in `RESERVED_KEYS` and doc'd as consumed but the `list` handler never reads it (`filtering.rs:65`) → silent unsorted results; `umbra-openapi` CRUD paths hardcode `/api/{table}/` (`lib.rs:282,293`) ignoring `RestPlugin::base_path()` (Swagger "Try it" 404s under `.at()`) and always emit `page`/`page_size` even under `NoPagination`/`LimitOffset` (a unit test enshrines the bug). Fix: render/enforce or remove each surface. → `plugins-review/{umbra-admin,umbra-rls,umbra-oauth,umbra-rest,umbra-openapi}.md`.

80. [ ] **Plugin reliability & correctness (net-new).** `umbra-signals`: async handler panics are **not isolated** (`signals.rs:239` bare `fut.await` vs the sync `catch_unwind` at `:222`) → a panicking async subscriber unwinds through `emit()` → the triggering ORM write → kills the whole request; apply the existing sync pattern to async. `umbra-sessions`: no rolling/sliding expiry (`lib.rs:224` `expires_at` fixed at login) → active sessions hard-expire mid-use; expired rows accumulate with no `clearsessions`. `umbra-tasks`: non-retriable detection via `err_msg.starts_with("handler not found")` (`lib.rs:487`) instead of the existing `TaskError::HandlerNotFound` variant (fragile — a message change silently makes missing-handler tasks retriable and burn `max_attempts`); no `FOR UPDATE SKIP LOCKED` / orphan-task reclaim (at-most-once, not at-least-once). `umbra-permissions`: `table_app_label` (`lib.rs:294-300`) splits the table name at the first `_` → distinct models collide into one `Permission`/ContentType row (bare `post` and plugin `app_post` both → `app.add_post`); thread the plugin name from `ModelMeta`. `umbra-auth`: `Identity` carries `is_staff` but never `is_superuser` across all four auth paths (`session_user.rs:167`, `bearer_auth.rs:106`, `extractors.rs:120,137`) → superuser bit lost at the REST boundary. → `plugins-review/{umbra-signals,umbra-sessions,umbra-tasks,umbra-permissions,umbra-auth}.md`.

81. [ ] **Plugin security (net-new).** `umbra-security`: `csrf_exempt_paths` matches on prefix without a segment boundary → `/api` also exempts `/api-internal` (the security lens folded only the empty-key item into #75; this is distinct). `umbra-email`: no CRLF/header-injection guard or test on `subject` (a templated `"Re: {user_input}"` is the classic Bcc-injection vector — lettre likely guards via RFC 2047 but the plugin neither states nor tests the contract); console backend prints full bodies incl. reset tokens to stderr and only *warns* outside Dev (consider hard-fail in Prod); SMTP send has no timeout. `umbra-playground`: the `SHELL_HTML.replace("__PLACEHOLDER__", value)` chain (`routes.rs:87-92`) is naive — an `app_name` containing a later token expands into that slot (template-injection breakout); use single-pass substitution. `umbra-cache`: `cache_page` keys ignore Host/Cookie → cache poisoning (Required). `umbra-static`: embedded assets miss ETag/304 + a symlink-loop path (Required). `umbra-oauth`: `reqwest::Client::new()` per call with **no timeout** (handler-stall DoS) + `unique_username` TOCTOU with a non-retrying UNIQUE collision (sibling of #71 which didn't cover oauth). → `plugins-review/{umbra-security,umbra-email,umbra-playground,umbra-cache,umbra-static,umbra-oauth}.md`.

82. [ ] **Plugin completeness — missing breadth vs framework peers (deferred features, lower urgency).** Honest deferrals to schedule, not bugs: `umbra-media` — no file-lifecycle cleanup (orphaned blobs on row delete/replace) + a **fully-buffered `Storage` trait** (no streaming, whole file in memory) + thin `FileField`/`ImageField` ORM integration (Required-grade for prod uploads). `umbra-static` — no `collectstatic`/manifest-hash cache-busting. `umbra-cache` — single backend; no Redis/alt-backend, cache middleware, or template-fragment cache. `umbra-auth` — no password-strength validation (registers accept `"a"`) + no login/register throttle (credential-stuffing by default). `umbra-sessions` — no `SessionStore` trait (DB backend hardcoded into every helper; contradicts the swap-any-built-in ethos). `umbra-tasks` vs Celery — no exponential backoff, periodic/cron beat, result backend, task-status API, per-task timeout, priority queues, admin visibility. `umbra-rest` vs DRF — no throttling/versioning/bulk endpoints. `umbra-realtime` — no `Last-Event-ID` reconnect resume, no aggregate connection cap. → `plugins-review/{umbra-media,umbra-static,umbra-cache,umbra-auth,umbra-sessions,umbra-tasks,umbra-rest,umbra-realtime}.md`.

83. [ ] **admin — custom-base-path bugs + authz disclosure (net-new).** Mounting admin anywhere but `/admin` silently breaks two fragments: inline cell-edit (`inline_edit.rs:85`) and FK-picker pagination (`fk_picker.rs:201`) hardcode `/admin/...` in handler-emitted HTML instead of `branding::current().base_path` → 404 under `.at("/backoffice")` (every other URL honors the base path). The sidebar `apps(_viewer)` (`registry.rs:111`) ignores the `view_<model>` permission → shows every model to every staff user (model-existence disclosure; the changelist itself is gated). Bulk actions silently drop non-i64 selected ids (`actions.rs:49,167`) yet still toast "Deleted N" (PK-lift family). Zero permcheck-authz / `.at()` / non-i64-PK / soft-delete-on-admin tests. → `plugins-review/umbra-admin.md`.

84. [ ] **Plugin-contract & shared framework primitives (net-new).** `umbra-realtime` declares a **hard, non-optional** `umbra-auth` dep (`Cargo.toml:16`) → even anonymous push-only feeds must compile auth (same family as #76); make `umbra-auth` optional behind a feature + define an `IdentityResolver` seam in the facade. `umbra-health` `probe_database` (`lib.rs:243,248`) uses raw `sqlx::query("SELECT 1")` — a CLAUDE.md ORM bypass; add `umbra::db::ping()` to the ORM surface and route through it (also wrap each `HealthCheck` in `tokio::time::timeout` — one blocked check hangs `/ready`). Divergent sync→async `on_ready` bridges: `umbra-rls` uses a bare `Handle::current().block_on` that panics under `#[tokio::test]` (its own docstring admits it) while `umbra-permissions` uses the runtime-tolerant form → add a shared `umbra::plugin::block_on_ready` helper. → `plugins-review/{umbra-realtime,umbra-health,umbra-rls,umbra-permissions}.md`.

85. [ ] **Plugin test-coverage holes (security/correctness-critical).** Security-critical HTTP paths with zero coverage: oauth `callback` state-CSRF defense has no e2e test; empty-key CSRF degradation untested; email header-injection untested; tasks panic-recovery (`tokio::spawn` catch) + concurrent-worker double-claim guard (the BROKEN-1 fix) untested; signals async-panic-isolation (would fail today), bulk `bulk_post_save`/`bulk_post_delete` firing, `m2m_changed`, and the actor envelope all untested; admin permcheck-authz + `.at()` base-path + non-i64-PK untested. Add focused tests alongside the fixes above. → `plugins-review/*.md`.

86. [ ] **Plugin doc drifts (fold into the docs PR).** `plugins/signals.mdx:115-118` tells users `bulk_create`/`update_values`/`QuerySet::delete` are "signal-free" — they all fire `bulk_post_save`/`bulk_post_delete` (the ORM doc is right; the plugin doc is stale); `m2m_changed` still listed there as "not shipped" (it ships). `umbra-openapi`'s `//!` module-doc claims "no securitySchemes / pagination deferred" — both false now. Two **stale backlog/docs-audit assumptions corrected by this wave**: `umbra-oauth` ships a **GitHub** provider alongside Google (not "Google only"), and `umbra-email` **is a real, building workspace member** with a doc page (not "crate doesn't exist" — supersedes the P1 docs note at backlog line 75). `umbra-tasks` doc: `TasksPlugin` vs `TasksPlugin::default()` + a stale Postgres-locking note post-BROKEN-1. → `plugins-review/*.md` + `docs-audit/*.md`.

# Seen/Known gaps - Continued from @gaps.md

> `[x]` write-ups are archived verbatim (same numbers) in `archive/gaps2-done.md`. Only open `[ ]` and partial `[~]` entries keep full text here.

1. [x] Save-feedback toast in the admin sheet — SHIPPED in commit `d2916d5` as gaps2 #13. — archived

2. [ ] Can we have a posthog wiring maybe as a plugin, or a way of linking such logging systems into umbra

3. [x] Change-password dialog extracted to an HTML `<template>` — SHIPPED in commit `5b22cc5`. — archived

4. [x] wrapper.html growing too large — SYMPTOM RESOLVED (2026-06-20): inline JS extracted to external admin.js (e7747fa, 1636->563 lines), CSS already external. Per-feature bundles (perf) + CDN self-host (offline, see #36b) deferred as separate enhancements. — archived

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

12. [x] Admin form errors — per-field rendering SHIPPED (2026-06-20): apply_write_error_to_fields merges WriteError::field_errors() into the per-field FormField.error slot; non_field_errors stay in the top banner (d1bdccb). — archived

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

23. [x] **DB router read/write split — DONE via the #69 `DatabaseRouter` foundation (2026-06-16).** `resolve_pool` now routes read terminals through `db_for_read` and write terminals through `db_for_write` on the swappable router; read-after-write is handled (`get_or_create`/`update_or_create` probe the write pool — read-your-writes; `.on(&pool)` pins). Tests `router_read_write_split` / `router_upsert_readwrite`; example `examples/read-replica`. Full design in the #69 spec — archive the verbatim write-up under #23 when convenient.

24. [x] Multi-DB / database routing docs — SHIPPED (2026-06-20): database-routing.mdx expanded with read/write split (#23) + cross-DB FK (#22); Phase-2 multitenancy items flagged under #69 (78863e8). — archived

25. [ ] **`umbra startproject` should auto-mount `SecurityPlugin` (CSRF + hardening headers) in the scaffolded app builder.** Per the round-one audit (`bugs/review/security-auth-session.md` AUTH-1/AUTH-2), the flagship `examples/shop` runs with no CSRF + no security headers because `SecurityPlugin` is opt-in and easy to forget. The scaffold (gap #8) should wire `.plugin(SecurityPlugin::new())` by default (and `.with_hsts(true)` behind a prod profile); consider a boot-time `check.rs` warning when auth/sessions are mounted without security. Necessary but not sufficient for "secure by default" — see #26 and the XSS fixes (`bugs/review/security-web-surface.md` WEB-3/WEB-4) which `SecurityPlugin` does not cover.

26. [x] Signed/session-bound CSRF (`SecurityConfig::signed_csrf`) is now the default — archived

27. [ ] **Cache plugin should expose axum/tower-http caching + compression layers as opt-in config.** Mirror the new `umbra-security` config-struct shape: surface `tower_http::compression::CompressionLayer` (gzip/br negotiation) and HTTP cache-control header management (e.g. `SetResponseHeaderLayer` for `Cache-Control`/`ETag`/`Vary`) through `umbra-cache` so an app gets response compression + cache headers declaratively, instead of the page-cache (`cache_page`) being the only knob. Note: tower-http has no full response-cache layer; this is header + compression management, distinct from the server-side `cache_page` store. Pairs with the cache gaps already noted in `bugs/review/broken-features.md` (BROKEN-10/12).

28. [x] `allowed_hosts` request-time enforcement — SHIPPED. — archived

29. [x] CORS path scoping — SHIPPED. — archived

30. [ ] **Two flaky test groups under full-workspace parallel runs.** (a) `plugins/umbra-auth/tests/integration.rs::createsuperuser_noinput_errors_without_password_env` — a sibling test sets `UMBRA_SUPERUSER_PASSWORD` while this one runs; the in-test `remove_var` can't guard across parallel threads. Failed twice in unrelated verifies on 2026-06-10 (templates-only change in one case); passes alone and on re-run. Fix shape: a process-wide env-mutex shared by every test that touches superuser env vars, or move env-mutating cases into a serial `#[serial]` group. (b) `plugins/umbra-admin/tests/cross_crate_o2o.rs` — all three tests failed once in a full `cargo test` sweep, pass in isolation and on re-run; likely shared-DB/registry contention. (c) `plugins/umbra-admin/tests/phase2_sheet.rs::test_preview_sheet_htmx_returns_fragment` — failed once in a full sweep, 3/3 green when its file runs alone. The victims differ per sweep, which points at cross-binary contention (shared test DB or ambient registry) rather than per-test bugs. Diagnose the shared resource before papering over with retries. (d) NEW 2026-06-20: `plugins/umbra-rest/tests/auth_permission.rs` SIGSEGV'd ONCE under a full-workspace parallel `cargo test` (signal 11), aborting the run; passes 12/12 in isolation (single- AND multi-threaded, 3/3 runs). Same shared-resource-contention family — now manifesting as a sqlx/sqlite memory crash under high binary-parallelism (this session added ~10 test binaries). Reinforces: diagnose the shared test DB/registry; consider a serial group or bounded binary-parallelism for the workspace gate.

31. [x] Can you reference deep nested html templates ie in a view you call `.render("")` with a path like `"/foo/bar.html"` and automatically find such a template? (Renumbered from a duplicate #30 — that number was already taken by the flaky-tests entry, committed and cited in `01503da`.)

32. [x] **Boot check `field.choices_default` rejects a choices default that isn't a member of its choices — SHIPPED.** `check.rs` walks every registered model and fails the build with a `Severity::Error` finding when `!choices.is_empty() && !default.is_empty() && !choices.contains(&default)`, with a did-you-mean for `::`-shaped defaults (lowers the tail, suggests the DB literal). Two-binary test (bad path-shaped default fails build; valid default builds). The optional boolean/numeric default sanity-check was deliberately deferred (false-positive risk on `CURRENT_TIMESTAMP`/expression defaults). — archived

33. [ ] **Admin index `/admin/` should not auto-redirect to `last_path` (a.k.a. gaps2 #11 round 2).** The "where I left off" affordance shipped in gaps2 #11 round 2 persists the URL of the last changelist the user visited into `admin_user_pref.preferences.last_path`; the index handler reads it and 302-redirects on every `/admin/` hit. The escape hatch was `?dashboard=1`, but no UI in the admin emits that param, so once `last_path` is set to a changelist, the user has no way to get back to the dashboard short of a direct SQL `UPDATE admin_user_pref SET preferences = '{}'`. Surfaced 2026-06-10 on `umbra_website` (the scaffold's admin kept 302-ing to `features_feature_status_event/?page_size=25` after the first changelist click). **Current mitigation**: the *reader* in the index handler (`plugins/umbra-admin/src/handlers/list.rs` ~lines 245-263) was removed — `/admin/` always renders the dashboard. The *writer* on the changelist handler was left in place (it's a cheap upsert, no behavioural cost). **Proper fix**: this whole feature should live behind an `AdminPlugin` config flag — `restore_last_path: bool` (default `true` for power users who want the behaviour, opt-out for projects that don't) — and the index handler reads that flag instead of hardcoding the redirect. Bonus: the writer on the changelist handler should also gate on the same flag, so users who opt out don't accumulate dead `last_path` data. Templates also need updating: the sidebar's "Home" / brand link should emit `?dashboard=1` automatically when the flag is on, so the opt-out is one click away rather than URL-tape.

34. [ ] **Soft delete: decide whether `update_values` should respect the `deleted_at IS NULL` auto-filter.** Reads and `delete()` are auto-scoped on a `#[umbra(soft_delete)]` model, but `build_update_for` (`orm/queryset/mod.rs` ~2403) walks only the explicit predicates — so a bulk `Post::objects().update_values(...)` also mutates trashed rows, and `.only_deleted()` / `.with_deleted()` are silent no-ops on the update path. Surfaced 2026-06-10 while writing `orm/soft-delete.mdx` (the restore example originally used `.only_deleted()` believing it scoped the UPDATE). Two defensible semantics: (a) consistent-with-reads — inject the filter, honour `with_deleted`/`only_deleted`, which makes restore `only_deleted().update_values(...)`; (b) writes-are-explicit — keep today's behaviour but make `.with_deleted()`/`.only_deleted()` on an update a loud error instead of a no-op. Either way the current silent no-op is the wrong third option. Update the docs example to match whichever wins.

35. [ ] **Soft delete is invisible to the dynamic path — admin and REST hard-delete and show trashed rows on `#[umbra(soft_delete)]` models.** `Model::SOFT_DELETE` lives only on the typed trait; `DynQuerySet` / `ModelMeta` (`orm/dynamic.rs`, zero soft_delete references) never see it. Consequences on a tagged model: admin changelists list soft-deleted rows alongside live ones, the admin delete button (and REST `DELETE /api/...`) destroys the row permanently, and REST list endpoints return trashed rows. Surfaced 2026-06-10 when `umbra_website` tagged its 23 `deleted_at` models. Fix shape: carry a `soft_delete: bool` on `ModelMeta` (wired from the registry the same way `noform`/`noedit` flow), inject the `deleted_at IS NULL` filter in `DynQuerySet` terminals, rewrite its `delete()` the way the typed path does, and give the admin a trash affordance (changelist filter for `only_deleted` + restore action + hard-delete behind a confirm) — the Django-admin-parity shape. Until then, treat soft delete as an app-code feature only.


36. [~] **Rich field-editor follow-ups.** _PARTIAL: (a) syntax-highlighted fenced code DONE — server-side syntect (7a4ded8) + a real XSS in the fence info-string fixed via fence_lang_is_safe (5188725). REMAINING (deferred, low-urgency): (b) self-host editor CDNs (EasyMDE/Quill/CodeMirror/DOMPurify) via Plugin::static_files [same as #4 CDN self-host], (c) EasyMDE image-upload into a media endpoint._ Original below:

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
66. [x] Document minijinja filters + custom filter/function registration — SHIPPED (2026-06-20): web/templates-filters.mdx (built-ins + 8 umbra filters + Plugin::template_registrars()) (6a9a0bf). — archived
67. [ ] The `umbra startapp` command should automatically register the added plugin into `cargo.toml` and let the developer know that the plugin has been added successfully.
68. [ ] Plugin issues are not tracked anywhere - They are recorded as comments which is wild but still the comment does not appear! Maybe because they are issues or not public yet. We can leave them as comments but we update the system to track them properly ie update the comments table to track `is_issue`, `is_public` and `is_resolved`. This way, we can safely track and resolve plugin issues. Also, when its not resolved, it remain redish once solved it switches to green. Same comment flow and users can still comment on it without us changing alot of things. ON the issues tab, we only show comments that are issues and their status. With this flow, a user can safely comment on the plugin with the issue url on github or gitlab and a user can click on the link to track the issue now on github
69. [~] **Pluggable / standalone database router — the keystone for multitenancy, read/write split, and new backends.** **FOUNDATION SHIPPED (2026-06-16):** the swappable `DatabaseRouter` trait (`db_for_read`/`db_for_write`/`allow_relation`/`allow_migrate`/`schema_for`) + request-scoped `RouteContext` (task-local, spawn-safe) + `RouteContextLayer` middleware + read/write split (folds in #23) + zero-round-trip schema-qualified SQL (option C, schema-per-tenant), with a default router reproducing today's behaviour byte-for-byte; absorbs #22 (cross-DB FK guard → `allow_relation`). Spec `docs/superpowers/specs/2026-06-16-database-router-foundation-design.md`, plan `docs/superpowers/plans/2026-06-16-database-router-foundation.md`, example `examples/read-replica`, behavioral tests incl. SQLite-ATTACH schema isolation + spawn-safety. **Phase 2 (open):** the schema-per-tenant *management* layer — a `Tenant` model, `migrate_schemas`, the `SHARED_APPS`/`public` split — plus database-per-tenant ambient routing, row-level tenancy, and M2M-junction pool routing (#88b). Foundation follow-ups logged in the spec. _Original (user):_ the current flow embeds postgres and sqlite routers in the ORM. That's fine, but adding mysql/mongodb means updating the ORM, and ORM-level changes like Postgres multitenancy are hard. Move routing out of the ORM — or at least shape it so it works as a standalone, swappable router — so a developer implementing multitenancy can drop in their own.

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

71. [~] **Concurrency / data-divergence hardening.** _PARTIAL (2026-06-20): SHIPPED — `set_user_groups` transactional + rollback test (`a4cdbd8`); `update_or_create`/`get_or_create` converge on UNIQUE race (`18b6a93`); `add_user_to_group`/`grant_user_permission` idempotent caught-UNIQUE (`c818cab`). REMAINING (sole sub-part): session `set_data` read-modify-write loses concurrent keys + swallows corrupt data (`plugins/umbra-sessions/src/lib.rs:400-456`) — DEFERRED to Phase 2a (`perf/sessionstore-2a` rewrites the session core)._

72. [x] Endpoint scalability — SHIPPED (2026-06-20): CSV 1000-cap (58d8c2e), FK/deleted_at index emission (2d2864f), M2M form cap=200 (9de4b4d), apply_overrides clone-once-per-request (cca87e1), AdminPerms one-query (7af921a). — archived

73. [x] Silent wrong-writes + per-request panic — SHIPPED (2026-06-20): float min/max, inline_edit 400, Masked BadKey, storage de-panicked, non-i64 M2M (UUID-BLOB bind), REST CSV errors→500. — archived

74. [x] OAuth: PKCE (S256) on every flow + single-use expiring `state` — verifier persisted in the session `FlowState`, only its S256 hash sent on the authorize redirect, replayed on the token exchange; `state` consumed before exchange. `plugins/umbra-oauth/src/pkce.rs` + provider/route wiring; end-to-end proof in `tests/pkce_flow.rs`. — archived

75. [x] Secret / auth hardening — SHIPPED (2026-06-20): empty `SECRET_KEY` fail-closed in prod / warn in dev (`71c75a0`); `password_hash` hard-denied in REST, un-overridable by `.expose()` (`e7e70ab`, after reverting a macro-based attempt `92be470`); inactive superuser (incl.) denied at the REST perm check (`e2dd1ae`). — archived

76. [ ] **Plugin-contract violation — `umbra-auth` depends on `umbra-rest`.** The `Authentication`/`Identity` traits live in `umbra-rest`, so every app with auth pulls in `umbra-rest` even when REST-free — contradicts the CLAUDE.md contract "a REST-free app has to compile and run with zero serializer code." Fix: lift those traits into `umbra-core`/facade; both auth + rest depend inward. → `reviews/architecture-modularity.md`.

77. [x] Dedup `to_snake_case` (×3) / `pascal_case` (×2) — SHIPPED (2026-06-20): new no-dep `umbra-casing` crate (`to_snake_case` + `pascal_case_from_table` + `pascal_case_from_ident`); all 5 sites consolidated, per-site output preserved (`4b92067`). — archived

78. [ ] **Module splits for the 5 files >2,800 LOC.** `orm/queryset/mod.rs` (4846), `migrate.rs` (4660), `umbra-macros/src/lib.rs` (4521), `orm/dynamic.rs` (3009), `orm/column.rs` (2845) each mix several responsibilities. Split each into a cohesive *module* (directory of focused files grouped by responsibility, not arbitrary line cuts); proposed trees + the fns/impls that move are in the report. Notably collapse `dynamic.rs`'s 4 parallel decode fns (`decode_to_string`/`_pg`/`_to_json`/`_pg_to_json`). Pure refactor, do incrementally. → `reviews/architecture-modularity.md`.

> Existing entries the review sharpened: **#34** (stale line-ref `~2403`; also misses `update_expr`), **#35** (+ a 3rd soft-delete leak: relation hydration `orm/queryset/hydration.rs:654` returns trashed children), **#63** (FK + `deleted_at` index emission), **#68** (`on_delete` is DDL-only — no ORM cascade collector / `post_delete`), **#79** (the unsafe `nullable→NOT NULL` / `unique false→true` ALTERs lack a NULL/dup pre-check). The ~8 Critical + long-tail **doc drifts** (FColExt-not-in-prelude, non-existent `#[umbra(m2m)]`, realtime MDX artifacts, `checkmigrations` binary, admin CSS path / `on_ready` claim, etc.) ship as a single docs PR — see `planning/hardening/docs-audit/*.md`.

---

## Wave C — per-plugin review (all 19 plugins; `planning/hardening/plugins-review/<plugin>.md`)

> Holistic 5-axis + **completeness** pass over every built-in plugin (one report each, the detailed single source). Verdicts: **Solid/complete** — auth, sessions, permissions, email, realtime, livereload, health, playground, signals (with the async-panic fix), rest (strongest in tree). **Real but incomplete** — rls (DDL real, enforcement absent), oauth (refresh missing), tasks (lean v1). **Has gaps** — admin (one advertised feature stubbed). Net-new findings consolidated below; each cites the per-plugin report.

79. [~] **Plugin completeness — advertised-but-non-functional surfaces.** _PARTIAL (2026-06-20): SHIPPED — REST `?ordering=` applied DRF-style (`ee9d5bf`); openapi honors `base_path` + per-class pagination params (`4233fe7`); admin `Action::permission` enforced (`878d73d`). REMAINING (genuine big features): admin `InlineModel`/`TabularInline` rendering (biggest Django-parity hole), `umbra-rls` per-request `app.user_id` (ties #69 routing-context), `umbra-oauth` refresh-token exchange._

80. [~] **Plugin reliability & correctness (net-new).** _PARTIAL (2026-06-20): SHIPPED — signals async-panic isolation (`c186e71`), tasks typed `HandlerNotFound` (`f9e19bd`), auth `is_superuser` (`710fe5b`), sessions opt-in sliding expiry + `clearsessions` (`8989b89`), tasks orphan-reclaim visibility-timeout (`db76467`). REMAINING (sole sub-part, queued #80g): permissions `table_app_label` collision — needs `const APP_LABEL` on the `Model` trait + macro capture of `#[umbra(plugin)]` + `ModelMeta.app_label`._

81. [x] Plugin security (net-new) — SHIPPED (2026-06-20): csrf path-segment boundary; email CRLF guard + console fail-closed in prod + SMTP timeout; playground single-pass; cache Host/Cookie key; static ETag/304 + symlink guard; oauth timeout + unique_username TOCTOU retry. — archived

82. [ ] **Plugin completeness — missing breadth vs framework peers (deferred features, lower urgency).** Honest deferrals to schedule, not bugs: `umbra-media` — no file-lifecycle cleanup (orphaned blobs on row delete/replace) + a **fully-buffered `Storage` trait** (no streaming, whole file in memory) + thin `FileField`/`ImageField` ORM integration (Required-grade for prod uploads). `umbra-static` — no `collectstatic`/manifest-hash cache-busting. `umbra-cache` — single backend; no Redis/alt-backend, cache middleware, or template-fragment cache. `umbra-auth` — no password-strength validation (registers accept `"a"`) + no login/register throttle (credential-stuffing by default). `umbra-sessions` — no `SessionStore` trait (DB backend hardcoded into every helper; contradicts the swap-any-built-in ethos). `umbra-tasks` vs Celery — no exponential backoff, periodic/cron beat, result backend, task-status API, per-task timeout, priority queues, admin visibility. `umbra-rest` vs DRF — no throttling/versioning/bulk endpoints. `umbra-realtime` — no `Last-Event-ID` reconnect resume, no aggregate connection cap. → `plugins-review/{umbra-media,umbra-static,umbra-cache,umbra-auth,umbra-sessions,umbra-tasks,umbra-rest,umbra-realtime}.md`.

83. [x] admin custom-base-path + authz disclosure — SHIPPED (2026-06-20): base_path fragments (006033a), sidebar view_<model> gate (c365c47), bulk non-i64 PK + real count (c2fcdd0); authz/.at()/non-i64 tests added. — archived

84. [x] Plugin-contract & shared framework primitives — SHIPPED (2026-06-20): health umbra::db::ping()+timeout (14c30c4), shared block_on_ready bridge (8a48b5f), realtime auth-optional + IdentityResolver seam (7f96fd5). — archived

85. [ ] **Plugin test-coverage holes (security/correctness-critical).** Security-critical HTTP paths with zero coverage: oauth `callback` state-CSRF defense has no e2e test; empty-key CSRF degradation untested; email header-injection untested; tasks panic-recovery (`tokio::spawn` catch) + concurrent-worker double-claim guard (the BROKEN-1 fix) untested; signals async-panic-isolation (would fail today), bulk `bulk_post_save`/`bulk_post_delete` firing, `m2m_changed`, and the actor envelope all untested; admin permcheck-authz + `.at()` base-path + non-i64-PK untested. Add focused tests alongside the fixes above. → `plugins-review/*.md`.

86. [x] Plugin doc drifts — SHIPPED (2026-06-20): signals.mdx (bulk methods DO fire `bulk_post_save`/`bulk_post_delete`; `m2m_changed` ships), umbra-openapi `//!` (has securitySchemes + pagination), tasks.mdx (Postgres-locking note corrected post-BROKEN-1) (`5d5f745`). — archived

87. [ ] **REST per-request session cost — ANSWERED (no INSERT bottleneck); one micro-opt open.** `session_layer` (`plugins/umbra-sessions/src/lib.rs:844-898`) is lazy (gaps2 #46): an anonymous request (no cookie, no session write) gets only an in-memory token — **zero session-row INSERT** ("anonymous-read requests that never write the session leave zero rows behind", `:862-865`). Authenticated requests do one indexed `read_session` SELECT on entry; *fresh* (no-/stale-cookie) requests do one indexed `read_session` SELECT on exit (`:888-890`) to detect a materialised row. So REST traffic creates **no per-request session rows** — the bottleneck this note worried about doesn't exist by design. **Open micro-opt (low priority, security-adjacent so handle carefully):** a request-scoped "session-dirty" flag set by `upsert_session_data_key` would let `session_layer` skip the exit SELECT (and the Set-Cookie probe) for requests that never wrote the session — saving one round-trip per fresh anonymous request on a session-enabled app.
88. [x] M2M junction parent-id + routing — SHIPPED (2026-06-20): (a) i64 false alarm resolved (pk_uuid_m2m); (b) junction free-fns route through DatabaseRouter via parent alias (9dbe17a). — archived
89. [ ] **Profiling: flamegraph (CPU) + dhat (heap), framework + per-plugin.** Throughput baseline taken with ApacheBench on `examples/read-replica` (release): static `/` ~43k req/s, ORM read `/notes` ~34k req/s, ORM write `/notes/add` ~10k req/s (SQLite single-writer) — router overhead negligible (vtable + 2 HashMap lookups per terminal). Next: (a) `cargo flamegraph` (perf-based) on the serve binary under load to find the dominant request-path cost (`resolve_pool`, hydration, sea-query build, row decode); (b) `dhat` heap profiling for per-request allocations (the `registered_models()` clone / per-row allocs flagged in the perf-hardening pass — gaps2 #72); (c) per-crate `criterion` micro-benches on the ORM (`QuerySet` build, decode) + the migration diff engine. All doable today: flamegraph the example binary, criterion benches per crate, dhat behind a feature on a bench harness.

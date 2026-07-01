# Closed gaps - Continued from @gaps3.md

Shipped write-ups for entries opened in `planning/gaps3.md`. Same numbers; the active file keeps a one-line `[x] ... — archived` stub in place.

---

1. [x] REST: `views([...])` means read-only *everywhere* (routes, OPTIONS, OpenAPI, 405)

The `.views([Action::List, Action::Retrieve])` scope already gated request-time access (it 404'd a scoped-out action), but the scope leaked in three places: the `OPTIONS` `Allow` header was hardcoded, the OpenAPI spec always emitted `post`/`put`/`patch`/`delete`, and a blocked write returned `404` (implying the URI doesn't exist) instead of `405` (the URI exists, this method doesn't). Three surgical changes, one design decision.

**Design decision — OPTIONS reflects what's *mounted*, never the permission class.** `Allow` is defined by HTTP as the methods the *target resource* supports — a property of the route, not of who's asking. Folding the permission class into it would hand two callers different `Allow` headers for the same resource (breaking caching/codegen) and conflate two orthogonal mechanisms. So `view_scope` (+ the `.bulk()` opt-in for collection PATCH/DELETE) is the *only* input to `Allow`. When `views()` isn't set, every verb stays advertised (backward-compatible). A resource that wants OPTIONS to advertise only `GET` says so with `.views([List, Retrieve])`, not with a `ReadOnly` permission.

**What changed:**

- `plugins/umbral-rest/src/lib.rs`
  - New `EndpointKind { Collection, Detail }` and `RestPlugin::exposed_methods(table, kind) -> Vec<&'static str>` — the single source of truth for the verb list, honoring `view_scope` and `.bulk()`. OPTIONS is omitted (always present); callers prepend it.
  - `gate(table, action, kind, identity)` gained the `kind` arg. When an action is scoped out it now distinguishes: endpoint still serves some verb → `ApiError::MethodNotAllowed { allow }` (405 + `Allow`); endpoint serves nothing → `404` (the URI genuinely isn't served). All 8 call sites pass the right `EndpointKind` (custom-action dispatch derives it from `ActionScope`; custom actions never hit the 405 branch since `view_exposed` is always true for them).
  - New `ApiError::MethodNotAllowed { allow: String }` variant + early-return in `IntoResponse` that sets the `Allow` header (mirrors the `Throttled` `Retry-After` pattern).
  - `collection_options` / `detail_options` rewritten to build `Allow` from `options_allow(table, kind)` → `exposed_methods`. `detail_options` gained `Path((table, _id))` so it can consult per-table scope.
  - New public `action_exposed(table, &Action) -> bool` (reads `view_exposed` off the ambient `CONFIG`; defaults to `true` when CONFIG is unset, matching `is_exposed`) — the seam OpenAPI consumes.
- `plugins/umbral-openapi/src/lib.rs`
  - `collection_paths` / `item_paths` build their operation maps conditionally on `umbral_rest::action_exposed(...)`, so a scoped-out action emits no operation.
  - New `has_operations(path_item)` helper; the caller skips inserting a path that ends up with no HTTP operations (e.g. `views([List])` leaves the detail URI with only an `id` parameter).

**Tests:**

- `plugins/umbral-rest/tests/options.rs` — added a `views([List, Retrieve])` `Doc` resource; new tests assert the collection and detail `OPTIONS` advertise only `OPTIONS, GET`.
- `plugins/umbral-rest/tests/auth_permission.rs` — the three `opt_in_views_*_returns_404` tests became `*_returns_405_with_allow` (assert 405 + `Allow` lists served verbs, excludes scoped-out ones). Added a `catalog` resource with `views([List])` and two tests: collection POST → 405, detail GET → 404 (serves nothing, no `Allow` header).
- `plugins/umbral-openapi/tests/integration.rs` — added an `oa_readonly` resource scoped to `views([List, Retrieve])`; new test asserts the spec keeps `get` on both paths but omits `post`/`put`/`patch`/`delete`.

**Docs:** new `documentation/docs/v0.0.1/rest/views.mdx` — purpose, one example, the 405-vs-404 split, and the views-vs-permissions distinction.

Behavior change to note: a write to a view-scoped resource now returns `405` (with `Allow`), not `404`, whenever the endpoint still serves another verb. The previous "always 404" was a weaker, less HTTP-correct signal.

---

4. [x] Flash messages no-op without a pre-existing session — resolved (works with SessionsPlugin; was a test-harness misconfig + doc error)

The original framing (logged during the Task 14 review of the auth form-action surface) claimed flash feedback was silently dropped for an anonymous first-visit form failure because `Messages::add` requires a session token and umbral sets cookies explicitly. That was wrong. `session_layer` (mounted by `SessionsPlugin::wrap_router`, default-on) injects a candidate `SessionToken` into every request extension including cookieless ones (the `fresh = true` path). `Messages::from_request_parts` prefers this extension over the raw cookie, so on a brand-new anonymous visitor's first submit: `session_layer` provides the token → `Messages::add` materialises the session row (lazy side-channel write) → `session_layer` emits `Set-Cookie` on the response. Flash feedback for anonymous first-visit failures works end-to-end **when `SessionsPlugin` is mounted** (which any flash-using app has).

The only configuration where it breaks is `AuthPlugin` booted ALONE without `SessionsPlugin` — a degenerate test-harness config, not a real app config. The `form_surface.rs` test used exactly that boot; the fix (commits 60082a7/4ba53f8 on feat/auth-full-surface) mounts `SessionsPlugin` in the test and asserts the session cookie is set on a failed login, and repoints the `form-endpoints.mdx` Callout from CSRF to SessionsPlugin as the session-establishing layer.

---

6. [x] Admin dashboard widget catalog now filters by `widget.permission`

Surfaced by the custom-views (features #76) final review. `GET /admin/api/dashboard/catalog` (`handlers::dashboard::dashboard_catalog`) built its entry list from `state.widget_catalog` unconditionally, so a user without a widget's codename saw it in the "add widget" picker, added it, then got a 403 on the data fetch (the data endpoint IS gated). A UX gap, not a security hole. Fix (commit `0718300`): capture the user from `require_staff` and skip any widget whose `permission` codename the user lacks (`permcheck::has_codename`), mirroring the check `dashboard_widget_data` already enforces. Graceful no-op preserved (absent `PermissionsPlugin` → all shown). Test `test_catalog_filters_by_widget_permission` in `tests/custom_views.rs` (gated widget absent for `cv_staff`, present for `cv_priv`).

---

7. [x] Custom-view paths are validated at build; a bad path no longer panics the router

Surfaced by the custom-views final review. `AdminPlugin::routes()` mounted `GET {base}/{view.path}` per registered view with no validation, so a view whose path was empty, whose first segment shadowed a built-in admin route (`login`/`logout`/`upload-image`/`api`), or that duplicated another view made axum's router `panic!` on a route conflict at boot. Fix (commit `a8f518d`): new `AdminPlugin::resolved_custom_views()` drops such views up front with a clear `tracing::error!`, and `routes()` (widget flatten, gate map, `AdminState.custom_views`, mount loop) plus `route_paths()` all read the resolved list — so a rejected view is absent from the router AND the sidebar, and the rest of the admin keeps serving. Multi-segment paths (`reports/sales`) coexist with the `{table}/` changelist route via axum static-over-param precedence, so only built-in *static* first segments are reserved. Unit test `resolved_custom_views_drops_reserved_and_duplicate_paths` in `lib.rs` (asserts the dropped set + that `routes()` no longer panics). Not fixed: a single-segment view path that shadows a real model table's changelist — that's a soft footgun (static wins, no panic), left as a documented caveat.

---

8. [x] Per-widget permission checks batched (concurrent, deduped)

`view::accessible_widget_sections_json` (the render filter for the dashboard + custom views) resolved each widget's codename with a sequential `has_codename` await, so a page with N permissioned widgets paid N sequential DB round-trips. Fix (commit `aaaa7ef`): collect the DISTINCT codenames across all widgets, resolve them ONCE and CONCURRENTLY via `futures_util::future::join_all` (already a dep, used by the parallel dashboard COUNTs), then filter widgets against the resulting `codename → bool` map. The render is now a single round of concurrent lookups regardless of widget count. Behavior is unchanged — the existing `test_widget_permission_filters_dashboard_render` regression test still passes.

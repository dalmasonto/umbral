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
